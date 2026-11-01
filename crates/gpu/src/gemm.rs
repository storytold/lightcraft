//! Matrix products on the GPU for the AI mask models (`lightcraft-segment`): `C = A · W`, where `W`
//! is a model weight matrix that stays on the GPU after its first use and `A` is the activations.
//!
//! The segmenter does all its other work on the CPU and hands its large matrix products here
//! (through `lightcraft_segment::accel`, which the engine wires up); a product that is small, does
//! not fit the device, or fails is done on the CPU by the caller, so this is only ever a speed-up.
//! It has a device of its own (like denoise), respects the same switches (`LIGHTCRAFT_GPU`,
//! `LIGHTCRAFT_GPU_BACKEND`, the GPU preference, `LIGHTCRAFT_GPU_ADAPTER`) and the crash sentinel
//! around device creation, and a device that errs or is lost stops being used.
//!
//! The device is on the second graphics card when there is one ([`crate::ai::preferred`]): the
//! models' weights (up to ~900 MB) are uploaded to one card only, and the card for editing stays
//! free for the photo being edited. When the helper's device fails, the editing card's is used.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use bytemuck::{Pod, Zeroable};

pub use crate::Role;
use crate::ai::{Broken, PerCard};

/// Longest wait for one product before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Most weight bytes kept on the GPU (the cache is emptied when it would grow past this).
const WEIGHT_CACHE: u64 = 3 << 30;

/// The tile one workgroup computes (`TILE × TILE` outputs) and the depth of its inner loop.
const TILE: u32 = 64;

const SHADER: &str = r#"
struct Dims { m: u32, n: u32, k: u32, pad: u32 };
@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@group(0) @binding(3) var<uniform> d: Dims;

const TS: u32 = 64u;
const TK: u32 = 16u;
var<workgroup> sa: array<f32, 1024>;
var<workgroup> sb: array<f32, 1024>;

@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let row0 = wg.y * TS;
    let col0 = wg.x * TS;
    let tid = l.y * 16u + l.x;
    var acc: array<f32, 16>;
    for (var i = 0u; i < 16u; i++) { acc[i] = 0.0; }
    var t = 0u;
    loop {
        if (t >= d.k) { break; }
        for (var e = 0u; e < 4u; e++) {
            let idx = tid * 4u + e;
            let r = row0 + idx / TK;
            let cc = t + idx % TK;
            var v = 0.0;
            if (r < d.m && cc < d.k) { v = a[r * d.k + cc]; }
            sa[idx] = v;
            let r2 = t + idx / TS;
            let c2 = col0 + idx % TS;
            var w = 0.0;
            if (r2 < d.k && c2 < d.n) { w = b[r2 * d.n + c2]; }
            sb[idx] = w;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < TK; kk++) {
            var av: array<f32, 4>;
            var bv: array<f32, 4>;
            for (var i = 0u; i < 4u; i++) {
                av[i] = sa[(l.y * 4u + i) * TK + kk];
                bv[i] = sb[kk * TS + l.x * 4u + i];
            }
            for (var i = 0u; i < 4u; i++) {
                for (var j = 0u; j < 4u; j++) {
                    acc[i * 4u + j] = acc[i * 4u + j] + av[i] * bv[j];
                }
            }
        }
        workgroupBarrier();
        t = t + TK;
    }
    for (var i = 0u; i < 4u; i++) {
        let r = row0 + l.y * 4u + i;
        for (var j = 0u; j < 4u; j++) {
            let cc = col0 + l.x * 4u + j;
            if (r < d.m && cc < d.n) { c[r * d.n + cc] = acc[i * 4u + j]; }
        }
    }
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Dims {
    m: u32,
    n: u32,
    k: u32,
    pad: u32,
}

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    max_binding: u64,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    broken: &'static Broken,
    /// The weights on this device.
    cache: Mutex<Cache>,
}

/// The matrix device on each card ([`crate::ai`]).
static DEVS: PerCard<Dev> = PerCard::new();

/// Why matrix products stopped using the GPU on the editing card (a device error), if they did.
pub fn broken_reason() -> Option<String> {
    DEVS.broken_reason(Role::Primary)
}

/// Why matrix products stopped using `card`, if they did.
pub fn broken_reason_on(card: Role) -> Option<String> {
    DEVS.broken_reason(card)
}

fn dev(card: Role) -> Result<&'static Dev, String> {
    DEVS.get(card, "AI masks", |adapter, broken, _| make_device(adapter, card, broken))
}

/// The device this thread's products go to ([`crate::ai::wanted`]: the helper first): the helper's
/// when it works, else the editing card's.
fn dev_for_work() -> Result<&'static Dev, String> {
    match crate::ai::wanted() {
        Role::Helper => dev(Role::Helper).or_else(|e| dev(Role::Primary).map_err(|_| e)),
        Role::Primary => dev(Role::Primary),
    }
}

fn make_device(adapter: wgpu::Adapter, card: Role, broken: &'static Broken) -> Result<Dev, String> {
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let limits = adapter.limits();
    if limits.max_compute_invocations_per_workgroup < 256 || limits.max_compute_workgroup_storage_size < 8 * 1024 {
        return Err(format!("{} is too limited for the matrix kernel", info.name));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(match card {
            Role::Primary => "lightcraft-ai-masks",
            Role::Helper => "lightcraft-ai-masks-helper",
        }),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    let which = card.id();
    device.on_uncaptured_error(std::sync::Arc::new(move |e| {
        log::error!("gpu (AI masks, {which} card): {e}");
        broken.mark(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(move |reason, msg| {
        log::error!("gpu (AI masks, {which} card): device lost ({reason:?}): {msg}");
        broken.mark(&format!("device lost ({reason:?}): {msg}"));
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("gemm"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
    let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gemm"),
        entries: &[
            entry(0, wgpu::BufferBindingType::Storage { read_only: true }),
            entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
            entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            entry(3, wgpu::BufferBindingType::Uniform),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gemm"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gemm"),
        layout: Some(&pl),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let max_binding = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
    log::info!("gpu (AI masks, {which} card): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, max_binding, pipeline, layout, broken, cache: Mutex::new(Cache::default()) })
}

/// The adapter AI masks would use (the helper first), or why they can't use one. Creates the
/// device on first use.
pub fn adapter() -> Result<String, String> {
    dev_for_work().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

/// [`adapter`] on `card`.
pub fn adapter_on(card: Role) -> Result<String, String> {
    dev(card).map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

/// Weights on the GPU, by the caller's key.
#[derive(Default)]
struct Cache {
    weights: HashMap<u64, (wgpu::Buffer, usize, usize)>,
    bytes: u64,
}

/// `a` (`rows × k`, row-major) times the weight matrix `key` (`k × n`, row-major; `weights` gives
/// it the first time) on the GPU. `None` when the GPU can't do it (unavailable, too large, a
/// device error): the caller computes it on the CPU.
pub fn matmul(a: &[f32], rows: usize, k: usize, n: usize, key: u64, weights: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>> {
    // wgpu panics in `Device::poll` once the device is lost (see `ctx::Gpu::poll`): the GPU is
    // then given up on and the CPU computes this product
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_matmul(a, rows, k, n, key, weights))).unwrap_or_else(|_| {
        // (the card this thread's products went to)
        if let Ok(d) = dev_for_work() {
            d.broken.mark("a matrix product on the GPU panicked");
        }
        Err("the GPU gave up".into())
    });
    match r {
        Ok(c) => Some(c),
        Err(e) => {
            log::debug!("gpu (AI masks): matrix product on the CPU: {e}");
            None
        }
    }
}

/// The out-of-memory, validation and internal error scopes around one product, from before its first
/// buffer: an allocation that fails is then this product's error (on out of memory the CPU does this
/// one) instead of an uncaptured device error that stops the GPU for the session. wgpu wants the scopes
/// popped in reverse order, on every return too: [`Scopes::pop`] and `Drop` do that.
struct Scopes(Option<[wgpu::ErrorScopeGuard; 3]>);

impl Scopes {
    fn push(device: &wgpu::Device) -> Self {
        Scopes(Some([
            device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            device.push_error_scope(wgpu::ErrorFilter::Validation),
            device.push_error_scope(wgpu::ErrorFilter::Internal),
        ]))
    }

    /// `(out of memory, validation or internal)` errors caught.
    fn pop(mut self) -> (Option<wgpu::Error>, Option<wgpu::Error>) {
        let Some([oom, validation, internal]) = self.0.take() else { return (None, None) };
        let internal = pollster::block_on(internal.pop());
        let validation = pollster::block_on(validation.pop());
        let oom = pollster::block_on(oom.pop());
        (oom, internal.or(validation))
    }
}

impl Drop for Scopes {
    fn drop(&mut self) {
        if let Some([oom, validation, internal]) = self.0.take() {
            drop(internal);
            drop(validation);
            drop(oom);
        }
    }
}

fn try_matmul(a: &[f32], rows: usize, k: usize, n: usize, key: u64, weights: &dyn Fn() -> Option<Vec<f32>>) -> Result<Vec<f32>, String> {
    let d = dev_for_work()?;
    let _work = crate::exit::enter().ok_or("LightCraft is closing")?;
    if rows == 0 || k == 0 || n == 0 || Some(a.len()) != rows.checked_mul(k) {
        return Err("bad shape".into());
    }
    let bytes = |e: usize| e.checked_mul(4).map(|b| b as u64).ok_or("too large");
    let (a_bytes, w_bytes, c_bytes) =
        (bytes(a.len())?, bytes(k.checked_mul(n).ok_or("too large")?)?, bytes(rows.checked_mul(n).ok_or("too large")?)?);
    if a_bytes.max(w_bytes).max(c_bytes) > d.max_binding {
        return Err("larger than one GPU buffer".into());
    }
    let groups_x = u32::try_from((n as u64).div_ceil(u64::from(TILE))).map_err(|_| "too wide")?;
    let groups_y = u32::try_from((rows as u64).div_ceil(u64::from(TILE))).map_err(|_| "too tall")?;
    if groups_x > 65_535 || groups_y > 65_535 {
        return Err("too many workgroups".into());
    }
    let dims = Dims {
        m: u32::try_from(rows).map_err(|_| "too tall")?,
        n: u32::try_from(n).map_err(|_| "too wide")?,
        k: u32::try_from(k).map_err(|_| "too deep")?,
        pad: 0,
    };
    let scopes = Scopes::push(&d.device);
    let mut cache = d.cache.lock().unwrap_or_else(|e| e.into_inner());
    if !cache.weights.get(&key).is_some_and(|(_, wk, wn)| (*wk, *wn) == (k, n)) {
        let w = weights().ok_or("the weights are not readable")?;
        if w.len() != k * n {
            return Err("the weights don't match their shape".into());
        }
        if cache.bytes.saturating_add(w_bytes) > WEIGHT_CACHE {
            cache.weights.clear();
            cache.bytes = 0;
        }
        use wgpu::util::DeviceExt;
        let buf = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gemm weights"),
            contents: bytemuck::cast_slice(&w),
            usage: wgpu::BufferUsages::STORAGE,
        });
        if let Some((old, ..)) = cache.weights.insert(key, (buf, k, n)) {
            cache.bytes = cache.bytes.saturating_sub(old.size());
        }
        cache.bytes = cache.bytes.saturating_add(w_bytes);
    }
    let (wbuf, ..) = cache.weights.get(&key).ok_or("weights missing")?;
    use wgpu::util::DeviceExt;
    let abuf = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gemm a"),
        contents: bytemuck::cast_slice(a),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let ubuf = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gemm dims"),
        contents: bytemuck::bytes_of(&dims),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let cbuf = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gemm c"),
        size: c_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gemm readback"),
        size: c_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gemm"),
        layout: &d.layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: abuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wbuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: cbuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: ubuf.as_entire_binding() },
        ],
    });
    let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gemm") });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("gemm"), timestamp_writes: None });
        pass.set_pipeline(&d.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    enc.copy_buffer_to_buffer(&cbuf, 0, &staging, 0, c_bytes);
    d.queue.submit([enc.finish()]);
    drop(cache);
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let waited = d.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
    let (oom, error) = scopes.pop();
    if oom.is_some() || error.is_some() {
        // the weights may be what failed: never keep them for the next product
        let mut cache = d.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((old, ..)) = cache.weights.remove(&key) {
            cache.bytes = cache.bytes.saturating_sub(old.size());
        }
    }
    if let Some(e) = oom {
        return Err(format!("the GPU ran out of memory: {e}"));
    }
    if let Some(e) = error {
        d.broken.mark(&format!("device error: {e}"));
        return Err(format!("the GPU reported an error: {e}"));
    }
    if let Err(e) = waited {
        d.broken.mark(&format!("the GPU did not finish a matrix product: {e}"));
        return Err(format!("the GPU did not finish: {e}"));
    }
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("reading the result back failed: {e}")),
        Err(e) => return Err(format!("reading the result back failed: {e}")),
    }
    let out = {
        let data = slice.get_mapped_range().map_err(|e| format!("reading the result back failed: {e:?}"))?;
        data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
    };
    staging.unmap();
    if out.len() != rows * n {
        return Err("the GPU returned an incomplete result".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(a: &[f32], w: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
        let mut c = vec![0f32; m * n];
        for i in 0..m {
            for p in 0..k {
                for j in 0..n {
                    c[i * n + j] += a[i * k + p] * w[p * n + j];
                }
            }
        }
        c
    }

    /// The kernel equals a plain product on odd sizes (partial tiles on every edge), on each card's
    /// device; the weights are read once per key and card. A card without a device is skipped.
    #[test]
    fn matmul_matches_the_cpu() {
        // (creating the device writes the process-wide init marker the backend tests check)
        let _serial = crate::DEVICE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for card in crate::ai::BACKGROUND_ORDER {
            if let Err(e) = adapter_on(card) {
                lightcraft_testkit::skip!(Gpu, "no matrix device on the {} card ({e})", card.id());
                continue;
            }
            crate::ai::on_card(card, || {
                for (m, k, n) in [(1, 1, 1), (67, 33, 129), (130, 257, 65), (512, 96, 384)] {
                    let a: Vec<f32> = (0..m * k).map(|i| ((i * 37 % 101) as f32 - 50.0) / 50.0).collect();
                    let w: Vec<f32> = (0..k * n).map(|i| ((i * 53 % 97) as f32 - 48.0) / 48.0).collect();
                    let key = (m * 1_000_003 + k * 1009 + n) as u64;
                    let reads = std::cell::Cell::new(0);
                    let get = || {
                        reads.set(reads.get() + 1);
                        Some(w.clone())
                    };
                    let g = matmul(&a, m, k, n, key, &get).unwrap();
                    let want = cpu(&a, &w, m, k, n);
                    let err = g.iter().zip(&want).fold(0f32, |e, (x, y)| e.max((x - y).abs()));
                    assert!(err < 1e-3, "{} card, {m}×{k}×{n}: {err}", card.id());
                    matmul(&a, m, k, n, key, &get).unwrap();
                    assert_eq!(reads.get(), 1, "weights uploaded once");
                }
                // shapes that don't add up are refused (the caller uses the CPU)
                assert!(matmul(&[1.0; 5], 2, 3, 1, 9, &|| Some(vec![1.0; 3])).is_none());
            });
        }
    }

    /// Throughput on a Swin-L-sized product (`cargo test -p lightcraft-gpu --release -- --ignored gemm_speed --nocapture`).
    #[test]
    #[ignore]
    fn gemm_speed() {
        let Ok(name) = adapter() else { return };
        let (m, k, n) = (16_384, 768, 3072);
        let a = vec![0.5f32; m * k];
        let w = vec![0.25f32; k * n];
        let t = std::time::Instant::now();
        matmul(&a, m, k, n, 77, &|| Some(w.clone())).unwrap();
        eprintln!("{name}: first (upload weights) {:?}", t.elapsed());
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let c = matmul(&a, m, k, n, 77, &|| Some(w.clone())).unwrap();
            let s = t.elapsed().as_secs_f64();
            eprintln!("{m}×{k}×{n}: {:.1} ms, {:.0} GFLOP/s incl. transfers (c0 {})", s * 1e3, 2.0 * (m * k * n) as f64 / s / 1e9, c[0]);
        }
    }
}
