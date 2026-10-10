//! Denoise networks on the GPU: a [`NetRunner`] executes a [`Net`] (the plain description of a U-Net's layers) with
//! WGSL compute kernels, one tile at a time, behind the same [`TileRunner`] interface the CPU runner has.
//!
//! The runner has a device of its own, so a denoise job and the interactive renders are separate contexts that the
//! driver time-slices, and a failure of one never takes the other down. It respects the same switches as GPU rendering
//! (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, the GPU preference) and the same crash sentinel around device creation.
//! `LIGHTCRAFT_GPU_ADAPTER` picks an adapter by (part of) its name, e.g. to try another card.
//!
//! Every failure is an `Err` (never a panic), and a device that errs or is lost stops being used: callers then run the
//! CPU runner. The kernels (`wgsl/nn_conv.wgsl`, `wgsl/nn_pool.wgsl`) are tested against the plain-loop reference
//! interpreter in `lightcraft_denoise::reference` and, with the real model, against the pure-Rust CPU runner.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use lightcraft_denoise::net::{Net, Op};
use lightcraft_denoise::run::{Error, TileRunner};
use wgpu::util::DeviceExt;

/// Longest wait for one tile's GPU work before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(60);
/// The largest tile side (in input cells) the dispatch sizes and 32-bit indexing are checked for.
const MAX_TILE: usize = 1024;
/// The largest single buffer used: indices in the kernels are 32-bit element offsets.
const MAX_BUFFER: u64 = 2 << 30;
/// Tiles that can be on the GPU at once (each has buffers of its own).
const WORKSPACES: usize = 2;

// ---------------------------------------------------------------------------------------------------------------------
// the device

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
}

static DEV: OnceLock<Result<Dev, String>> = OnceLock::new();
static BROKEN: AtomicBool = AtomicBool::new(false);
static BROKEN_WHY: Mutex<Option<String>> = Mutex::new(None);

fn mark_broken(why: &str) {
    let mut w = BROKEN_WHY.lock().unwrap_or_else(|e| e.into_inner());
    if w.is_none() {
        *w = Some(why.to_string());
    }
    BROKEN.store(true, Ordering::Relaxed);
}

/// Why the GPU stopped being used for denoise (a device error), if it did.
pub fn broken_reason() -> Option<String> {
    BROKEN.load(Ordering::Relaxed).then(|| BROKEN_WHY.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| "device error".into()))
}

fn dev() -> Result<&'static Dev, String> {
    if let Some(why) = broken_reason() {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    // creating the device is GPU work too: not once the process is ending (issue #620)
    let _work = match DEV.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or(CLOSING)?),
    };
    DEV.get_or_init(create_device).as_ref().map_err(|e| e.clone())
}

/// Why nothing runs on the GPU once [`crate::begin_shutdown`] was called.
const CLOSING: &str = "LightCraft is closing";

fn create_device() -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    crate::backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| make_device(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

fn make_device(backends: wgpu::Backends) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    // DX12 shaders compile with FXC (issue #471)
    desc.backend_options = crate::backend::backend_options();
    let instance = wgpu::Instance::new(desc);
    let wanted = std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let adapter = match &wanted {
        Some(w) => {
            let all = pollster::block_on(instance.enumerate_adapters(backends));
            let names: Vec<String> = all.iter().map(|a| a.get_info().name).collect();
            all.into_iter()
                .find(|a| {
                    let i = a.get_info();
                    i.device_type != wgpu::DeviceType::Cpu && i.name.to_lowercase().contains(w.as_str())
                })
                .ok_or_else(|| format!("no GPU adapter named like `{w}` (found: {})", names.join(", ")))?
        }
        None => pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?,
    };
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < 6
        || limits.max_compute_invocations_per_workgroup < 256
        || limits.max_compute_workgroup_size_x < 256
    {
        return Err(format!("{} is too limited for the denoise kernels", info.name));
    }
    if limits.max_compute_workgroup_storage_size < 16 * 1024 {
        return Err(format!("{} has too little workgroup memory for the denoise kernels", info.name));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lightcraft-denoise"),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    device.on_uncaptured_error(std::sync::Arc::new(|e| {
        log::error!("gpu (denoise): {e}");
        mark_broken(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(|reason, msg| {
        log::error!("gpu (denoise): device lost ({reason:?}): {msg}");
        mark_broken(&format!("device lost ({reason:?}): {msg}"));
    });
    log::info!("gpu (denoise): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits })
}

/// The adapter denoise would use ("NVIDIA GeForce RTX 4090 Laptop GPU (Dx12)"), or why it cannot use one. Creates the
/// device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

// ---------------------------------------------------------------------------------------------------------------------
// the plan: what to run, in what buffers

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ConvParams {
    h: u32,
    w: u32,
    c0: u32,
    c1: u32,
    n: u32,
    npad: u32,
    k: u32,
    mode: u32,
    cout: u32,
    leaky: u32,
    alpha: f32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PoolParams {
    h: u32,
    w: u32,
    c4: u32,
    total: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kernel {
    Conv { bm: u32, bn: u32, bk: u32 },
    Pool,
}

/// Where a step's tensor lives.
#[derive(Clone, Copy, Debug)]
enum Place {
    Input,
    Slot(usize),
    Output,
}

struct Step {
    kernel: Kernel,
    groups: [u32; 3],
    src: Place,
    src2: Option<Place>,
    dst: Place,
    /// Index into the per-layer GPU data (weights, bias), for the convolutions.
    layer: Option<usize>,
    params: Vec<u8>,
}

struct Layout {
    steps: Vec<Step>,
    /// Bytes of each activation buffer.
    slots: Vec<u64>,
    in_floats: usize,
    out_floats: usize,
}

/// Weights arranged for the kernel: `[k][npad]` rows with `k = tap · cin + ci` (columns are output channels, zero past
/// `n`), and the bias padded the same.
struct Layer {
    weight: Vec<f32>,
    bias: Vec<f32>,
}

fn unsupported<T>(why: impl Into<String>) -> Result<T, String> {
    Err(why.into())
}

/// The kernel shape for `n` output columns: 64 × 64 blocks when `n` is a multiple of 64, else 128 × 32.
fn blocks(n: usize, cin: usize) -> (u32, u32, u32) {
    let bn = if n.is_multiple_of(64) { 64 } else { 32 };
    let bk = if cin.is_multiple_of(16) {
        16
    } else if cin.is_multiple_of(8) {
        8
    } else {
        4
    };
    (4096 / bn, bn, bk)
}

fn round_up(v: usize, to: usize) -> usize {
    v.div_ceil(to) * to
}

/// Work out the steps, the buffers and the arranged weights for `net` on `tile × tile` input cells.
fn plan(net: &Net, tile: usize, max_binding: u64) -> Result<(Layout, Vec<Layer>), String> {
    if !net.fits_tile(tile) {
        return unsupported("the tile does not fit the network");
    }
    if tile > MAX_TILE {
        return unsupported("the tile is larger than the kernels' dispatches reach");
    }
    let side = |t: usize| -> usize {
        let level = net.shape(t).map_or(0, |s| s.level);
        if level >= 0 { tile >> level } else { tile << (-level) }
    };
    let chan = |t: usize| net.shape(t).map_or(0, |s| s.channels);
    if !net.in_channels.is_multiple_of(4) {
        return unsupported("the input channel count is not a multiple of 4");
    }
    let ops = net.ops();
    // the network must end in a 1 × 1 convolution and a depth-to-space (they run as one kernel)
    let n_ops = ops.len();
    let ends_right = matches!(
        (n_ops.checked_sub(2).and_then(|i| ops.get(i)), ops.last()),
        (Some(Op::Conv(c)), Some(Op::DepthToSpace2 { src })) if c.k == 1 && c.src2.is_none() && *src == n_ops - 1 && c.cout == 12 && c.leaky.is_none()
    );
    if !ends_right {
        return unsupported("the network does not end in a 1 × 1 convolution to 12 channels and depth-to-space");
    }
    // the last layer that reads each tensor
    let mut last_use = vec![0usize; n_ops + 1];
    for (i, op) in ops.iter().enumerate() {
        let reads: Vec<usize> = match op {
            Op::Conv(c) => std::iter::once(c.src).chain(c.src2).collect(),
            Op::ConvT2(c) => vec![c.src],
            Op::MaxPool2 { src } | Op::DepthToSpace2 { src } => vec![*src],
        };
        for t in reads {
            if let Some(l) = last_use.get_mut(t) {
                *l = i;
            }
        }
    }

    use Place::{Input, Output, Slot};
    let mut slots: Vec<u64> = Vec::new();
    let mut free: Vec<usize> = Vec::new();
    let mut place: Vec<Option<Place>> = vec![None; n_ops + 1];
    if let Some(p) = place.first_mut() {
        *p = Some(Input);
    }
    let mut steps: Vec<Step> = Vec::new();
    let mut layers: Vec<Layer> = Vec::new();
    /// A buffer of at least `bytes`: the smallest free one that is big enough, else the biggest free one grown, else new.
    fn alloc(bytes: u64, slots: &mut Vec<u64>, free: &mut Vec<usize>) -> usize {
        let size = |s: usize| slots.get(s).copied().unwrap_or(0);
        let fits = free.iter().enumerate().filter(|&(_, &s)| size(s) >= bytes).min_by_key(|&(_, &s)| size(s)).map(|(i, _)| i);
        let pick = fits.or_else(|| free.iter().enumerate().max_by_key(|&(_, &s)| size(s)).map(|(i, _)| i));
        match pick {
            Some(i) => {
                let s = free.swap_remove(i);
                if let Some(b) = slots.get_mut(s) {
                    *b = (*b).max(bytes);
                }
                s
            }
            None => {
                slots.push(bytes);
                slots.len() - 1
            }
        }
    }
    let get = |place: &[Option<Place>], t: usize| place.get(t).copied().flatten().ok_or_else(|| format!("tensor {t} has no buffer"));

    for (i, op) in ops.iter().enumerate() {
        let out_t = i + 1;
        match op {
            Op::Conv(c) => {
                if !c.cin.is_multiple_of(4) || !c.cout.is_multiple_of(4) {
                    return unsupported("a convolution whose channel counts are not multiples of 4");
                }
                let s = side(c.src);
                let last = i + 2 == n_ops;
                let (n, mode) = (c.cout, if last { 2u32 } else { 0u32 });
                let (bm, bn, bk) = blocks(n, c.cin);
                let npad = round_up(n, bn as usize);
                // arranged weights: row (tap · cin + ci), column co
                let taps = c.k * c.k;
                let mut weight = vec![0f32; taps * c.cin * npad];
                for co in 0..c.cout {
                    for ci in 0..c.cin {
                        for ky in 0..c.k {
                            for kx in 0..c.k {
                                let v = c.weight.get(((co * c.cin + ci) * c.k + ky) * c.k + kx).copied().unwrap_or(0.0);
                                if let Some(w) = weight.get_mut(((ky * c.k + kx) * c.cin + ci) * npad + co) {
                                    *w = v;
                                }
                            }
                        }
                    }
                }
                let mut bias = vec![0f32; npad];
                if let Some(b) = bias.get_mut(..c.cout) {
                    b.copy_from_slice(&c.bias);
                }
                let m = s * s;
                let params = ConvParams {
                    h: s as u32,
                    w: s as u32,
                    c0: chan(c.src) as u32,
                    c1: c.src2.map_or(0, chan) as u32,
                    n: n as u32,
                    npad: npad as u32,
                    k: c.k as u32,
                    mode,
                    cout: c.cout as u32,
                    leaky: u32::from(c.leaky.is_some()),
                    alpha: c.leaky.unwrap_or(0.0),
                    pad: 0,
                };
                let src = get(&place, c.src)?;
                let src2 = c.src2.map(|t| get(&place, t)).transpose()?;
                let dst = if last {
                    Output
                } else {
                    let slot = alloc((m * c.cout * 4) as u64, &mut slots, &mut free);
                    Slot(slot)
                };
                if let Some(p) = place.get_mut(out_t) {
                    *p = Some(dst);
                }
                layers.push(Layer { weight, bias });
                steps.push(Step {
                    kernel: Kernel::Conv { bm, bn, bk },
                    groups: [m.div_ceil(bm as usize) as u32, (npad as u32) / bn, 1],
                    src,
                    src2,
                    dst,
                    layer: Some(layers.len() - 1),
                    params: bytemuck::bytes_of(&params).to_vec(),
                });
            }
            Op::ConvT2(c) => {
                if !c.cin.is_multiple_of(4) || !c.cout.is_multiple_of(4) {
                    return unsupported("a transposed convolution whose channel counts are not multiples of 4");
                }
                let s = side(c.src);
                let n = 4 * c.cout;
                let (bm, bn, bk) = blocks(n, c.cin);
                let npad = round_up(n, bn as usize);
                // arranged weights: row ci, column (ky · 2 + kx) · cout + co
                let mut weight = vec![0f32; c.cin * npad];
                for ci in 0..c.cin {
                    for co in 0..c.cout {
                        for ky in 0..2 {
                            for kx in 0..2 {
                                let v = c.weight.get(((ci * c.cout + co) * 2 + ky) * 2 + kx).copied().unwrap_or(0.0);
                                if let Some(w) = weight.get_mut(ci * npad + (ky * 2 + kx) * c.cout + co) {
                                    *w = v;
                                }
                            }
                        }
                    }
                }
                let mut bias = vec![0f32; npad];
                for q in 0..4 {
                    for co in 0..c.cout {
                        if let (Some(b), Some(v)) = (bias.get_mut(q * c.cout + co), c.bias.get(co)) {
                            *b = *v;
                        }
                    }
                }
                let m = s * s;
                let params = ConvParams {
                    h: s as u32,
                    w: s as u32,
                    c0: c.cin as u32,
                    c1: 0,
                    n: n as u32,
                    npad: npad as u32,
                    k: 1,
                    mode: 1,
                    cout: c.cout as u32,
                    leaky: u32::from(c.leaky.is_some()),
                    alpha: c.leaky.unwrap_or(0.0),
                    pad: 0,
                };
                let src = get(&place, c.src)?;
                let slot = alloc((4 * m * c.cout * 4) as u64, &mut slots, &mut free);
                if let Some(p) = place.get_mut(out_t) {
                    *p = Some(Slot(slot));
                }
                layers.push(Layer { weight, bias });
                steps.push(Step {
                    kernel: Kernel::Conv { bm, bn, bk },
                    groups: [m.div_ceil(bm as usize) as u32, (npad as u32) / bn, 1],
                    src,
                    src2: None,
                    dst: Slot(slot),
                    layer: Some(layers.len() - 1),
                    params: bytemuck::bytes_of(&params).to_vec(),
                });
            }
            Op::MaxPool2 { src } => {
                let (s, c) = (side(*src), chan(*src));
                if !c.is_multiple_of(4) {
                    return unsupported("a max-pool over a channel count that is not a multiple of 4");
                }
                let total = (s / 2) * (s / 2) * (c / 4);
                let params = PoolParams { h: s as u32, w: s as u32, c4: (c / 4) as u32, total: total as u32 };
                let from = get(&place, *src)?;
                let slot = alloc((total * 16) as u64, &mut slots, &mut free);
                if let Some(p) = place.get_mut(out_t) {
                    *p = Some(Slot(slot));
                }
                let groups = total.div_ceil(64);
                steps.push(Step {
                    kernel: Kernel::Pool,
                    groups: [groups.min(8192) as u32, groups.div_ceil(8192) as u32, 1],
                    src: from,
                    src2: None,
                    dst: Slot(slot),
                    layer: None,
                    params: bytemuck::bytes_of(&params).to_vec(),
                });
            }
            // run as part of the 1 × 1 convolution before it
            Op::DepthToSpace2 { .. } => {}
        }
        // buffers whose last reader was this layer are free for the next layer's result
        for t in 0..=i {
            if last_use.get(t).copied() == Some(i)
                && let Some(Some(Slot(s))) = place.get(t)
                && !free.contains(s)
            {
                free.push(*s);
            }
        }
    }
    if slots.iter().any(|&b| b > max_binding) {
        return unsupported("an activation buffer is larger than this GPU allows");
    }
    if layers.iter().any(|l| (l.weight.len() as u64).saturating_mul(4) > max_binding) {
        return unsupported("a weight buffer is larger than this GPU allows");
    }
    Ok((Layout { steps, slots, in_floats: net.in_channels * tile * tile, out_floats: 3 * (2 * tile) * (2 * tile) }, layers))
}

// ---------------------------------------------------------------------------------------------------------------------
// the GPU objects

struct Pipelines {
    conv: HashMap<(u32, u32, u32), wgpu::ComputePipeline>,
    conv_layout: wgpu::BindGroupLayout,
    pool: wgpu::ComputePipeline,
    pool_layout: wgpu::BindGroupLayout,
    /// How long each kernel took to build (the driver compiles them), for the log and the tests.
    built: Vec<(String, f64)>,
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn pipeline(d: &Dev, label: &str, src: &str, layout: &wgpu::BindGroupLayout) -> wgpu::ComputePipeline {
    let module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pl = d.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(layout)], immediate_size: 0 });
    d.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        module: &module,
        entry_point: Some("main"),
        // The kernels write all of their workgroup memory before reading any of it, so the zeroing wgpu adds by default
        // is not needed; on DX12 it is written out as one assignment per element, which makes the shader compiler take
        // a minute over a 2k-element array.
        compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
        cache: None,
    })
}

fn conv_source(bm: u32, bn: u32, bk: u32) -> String {
    include_str!("wgsl/nn_conv.wgsl")
        .replace("{{BM}}", &bm.to_string())
        .replace("{{BN}}", &bn.to_string())
        .replace("{{BK}}", &bk.to_string())
        .replace("{{B_VECS}}", &(bk * bn / 4).to_string())
}

fn conv_bgl(d: &Dev) -> wgpu::BindGroupLayout {
    use wgpu::BufferBindingType::{Storage, Uniform};
    d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: true }),
            entry(5, Storage { read_only: false }),
        ],
    })
}

fn pool_bgl(d: &Dev) -> wgpu::BindGroupLayout {
    use wgpu::BufferBindingType::{Storage, Uniform};
    d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[entry(0, Uniform), entry(1, Storage { read_only: true }), entry(2, Storage { read_only: false })],
    })
}

fn build_pipelines(d: &Dev, layout: &Layout) -> Pipelines {
    let (conv_layout, pool_layout) = (conv_bgl(d), pool_bgl(d));
    let mut conv = HashMap::new();
    let mut built = Vec::new();
    for s in &layout.steps {
        if let Kernel::Conv { bm, bn, bk } = s.kernel {
            conv.entry((bm, bn, bk)).or_insert_with(|| {
                let started = std::time::Instant::now();
                let p = pipeline(d, "nn_conv", &conv_source(bm, bn, bk), &conv_layout);
                built.push((format!("conv {bm}x{bn}x{bk}"), started.elapsed().as_secs_f64() * 1000.0));
                p
            });
        }
    }
    let started = std::time::Instant::now();
    let pool = pipeline(d, "nn_pool", include_str!("wgsl/nn_pool.wgsl"), &pool_layout);
    built.push(("pool".into(), started.elapsed().as_secs_f64() * 1000.0));
    Pipelines { conv, conv_layout, pool, pool_layout, built }
}

/// One tile's buffers and the bound steps.
struct Workspace {
    input: wgpu::Buffer,
    output: wgpu::Buffer,
    staging: wgpu::Buffer,
    bound: Vec<(Kernel, [u32; 3], wgpu::BindGroup)>,
    _slots: Vec<wgpu::Buffer>,
}

struct Shared {
    dev: &'static Dev,
    layout: Layout,
    pipes: Pipelines,
    weights: Vec<(wgpu::Buffer, wgpu::Buffer)>,
    uniforms: Vec<wgpu::Buffer>,
    dummy: wgpu::Buffer,
}

fn storage(d: &Dev, label: &str, bytes: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
    d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(16),
        usage: wgpu::BufferUsages::STORAGE | extra,
        mapped_at_creation: false,
    })
}

impl Shared {
    fn workspace(&self) -> Workspace {
        let d = self.dev;
        let slots: Vec<wgpu::Buffer> = self.layout.slots.iter().map(|&b| storage(d, "activations", b, wgpu::BufferUsages::empty())).collect();
        let input = storage(d, "input", (self.layout.in_floats * 4) as u64, wgpu::BufferUsages::COPY_DST);
        let output = storage(d, "output", (self.layout.out_floats * 4) as u64, wgpu::BufferUsages::COPY_SRC);
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (self.layout.out_floats * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let buf = |p: Place| -> &wgpu::Buffer {
            match p {
                Place::Input => &input,
                Place::Output => &output,
                Place::Slot(i) => slots.get(i).unwrap_or(&self.dummy),
            }
        };
        let mut bound = Vec::new();
        for (i, s) in self.layout.steps.iter().enumerate() {
            let uniform = self.uniforms.get(i).unwrap_or(&self.dummy);
            let bg = match s.kernel {
                Kernel::Conv { .. } => {
                    let (w, b) = s.layer.and_then(|l| self.weights.get(l)).map_or((&self.dummy, &self.dummy), |(w, b)| (w, b));
                    d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &self.pipes.conv_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: buf(s.src).as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: s.src2.map_or(&self.dummy, buf).as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: w.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 4, resource: b.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 5, resource: buf(s.dst).as_entire_binding() },
                        ],
                    })
                }
                Kernel::Pool => d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.pipes.pool_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: buf(s.src).as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: buf(s.dst).as_entire_binding() },
                    ],
                }),
            };
            bound.push((s.kernel, s.groups, bg));
        }
        Workspace { input, output, staging, bound, _slots: slots }
    }
}

/// A network on the GPU for one tile size. Cheap to share between threads: each call takes a free workspace.
pub struct NetRunner {
    shared: Shared,
    tile: usize,
    in_channels: usize,
    free: Mutex<Vec<Workspace>>,
    /// Workspaces made so far (they are made when a call finds none free, up to [`WORKSPACES`]).
    made: Mutex<usize>,
    wake: Condvar,
    bytes: u64,
}

/// Put `net` on the GPU for `tile × tile` input cells, or say why it cannot be (no GPU, a layer or a size the kernels
/// do not do, the device refusing the buffers). Creates the device on first use and compiles the kernels.
pub fn runner(net: &Net, tile: usize) -> Result<NetRunner, String> {
    let _work = crate::exit::enter().ok_or(CLOSING)?;
    let d = dev()?;
    let max_binding = d.limits.max_storage_buffer_binding_size.min(d.limits.max_buffer_size).min(MAX_BUFFER);
    let (layout, layers) = plan(net, tile, max_binding)?;
    let scope = [
        d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
        d.device.push_error_scope(wgpu::ErrorFilter::Validation),
        d.device.push_error_scope(wgpu::ErrorFilter::Internal),
    ];
    // the driver compiles the kernels here: a crash in it is reported at the next launch (issue #250)
    let pipes = crate::backend::with_marker("denoise kernel compilation", || build_pipelines(d, &layout));
    let weights: Vec<(wgpu::Buffer, wgpu::Buffer)> = layers
        .iter()
        .map(|l| {
            let make = |v: &[f32]| {
                d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("weights"),
                    contents: bytemuck::cast_slice(v),
                    usage: wgpu::BufferUsages::STORAGE,
                })
            };
            (make(&l.weight), make(&l.bias))
        })
        .collect();
    let uniforms: Vec<wgpu::Buffer> = layout
        .steps
        .iter()
        .map(|s| {
            d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("params"),
                contents: &s.params,
                usage: wgpu::BufferUsages::UNIFORM,
            })
        })
        .collect();
    let dummy = storage(d, "dummy", 16, wgpu::BufferUsages::empty());
    let bytes: u64 = layout.slots.iter().sum::<u64>() + ((layout.in_floats + layout.out_floats) * 8) as u64;
    let shared = Shared { dev: d, layout, pipes, weights, uniforms, dummy };
    let first = shared.workspace();
    let [oom, validation, internal] = scope;
    let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
    if let Some(e) = errors.into_iter().flatten().next() {
        return Err(format!("{}: the denoise kernels could not be set up ({e})", d.info.name));
    }
    Ok(NetRunner { shared, tile, in_channels: net.in_channels, free: Mutex::new(vec![first]), made: Mutex::new(1), wake: Condvar::new(), bytes })
}

impl NetRunner {
    /// The adapter this runner uses.
    pub fn adapter(&self) -> String {
        format!("{} ({:?})", self.shared.dev.info.name, self.shared.dev.info.backend)
    }

    /// How long each kernel took to build, e.g. `conv 64x64x16: 812 ms`.
    pub fn build_report(&self) -> String {
        self.shared.pipes.built.iter().map(|(k, ms)| format!("{k}: {ms:.0} ms")).collect::<Vec<_>>().join(", ")
    }

    /// Device memory one tile in flight uses (bytes), without the weights.
    pub fn bytes_per_tile(&self) -> u64 {
        self.bytes
    }

    /// A free workspace, a new one when fewer than [`WORKSPACES`] exist, else wait for one to come back.
    fn take(&self) -> Result<Workspace, Error> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(w) = free.pop() {
                return Ok(w);
            }
            {
                let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
                if *made < WORKSPACES {
                    *made += 1;
                    drop(made);
                    drop(free);
                    let d = self.shared.dev;
                    let scopes =
                        [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];
                    let w = self.shared.workspace();
                    let [oom, validation] = scopes;
                    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
                    if let Some(e) = oom.or(validation) {
                        self.forget();
                        return Err(Error::Runtime(format!("the GPU could not make room for another tile: {e}")));
                    }
                    return Ok(w);
                }
            }
            free = self.wake.wait(free).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn give(&self, w: Workspace) {
        self.free.lock().unwrap_or_else(|e| e.into_inner()).push(w);
        self.wake.notify_one();
    }

    /// A workspace is gone (dropped after a failure): room for a new one, and a waiting caller may make it.
    fn forget(&self) {
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        *made = made.saturating_sub(1);
        drop(made);
        self.wake.notify_one();
    }

    fn execute(&self, w: &Workspace, input: &[f32]) -> Result<Vec<f32>, Error> {
        let d = self.shared.dev;
        let (t, c) = (self.tile, self.in_channels);
        // planar → channels innermost
        let mut nhwc = vec![0f32; input.len()];
        for (ch, plane) in input.chunks_exact(t * t).enumerate() {
            for (i, v) in plane.iter().enumerate() {
                if let Some(o) = nhwc.get_mut(i * c + ch) {
                    *o = *v;
                }
            }
        }
        let mut calls = Vec::with_capacity(w.bound.len());
        for (kernel, groups, bg) in &w.bound {
            let pipe = match kernel {
                Kernel::Conv { bm, bn, bk } => self.shared.pipes.conv.get(&(*bm, *bn, *bk)),
                Kernel::Pool => Some(&self.shared.pipes.pool),
            };
            let Some(pipe) = pipe else { return Err(Error::Runtime("a kernel is missing".into())) };
            calls.push((pipe, groups, bg));
        }
        let scopes = [
            d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            d.device.push_error_scope(wgpu::ErrorFilter::Validation),
            d.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ];
        d.queue.write_buffer(&w.input, 0, bytemuck::cast_slice(&nhwc));
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("denoise tile"), timestamp_writes: None });
            for (pipe, groups, bg) in calls {
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
            }
        }
        let bytes = (self.shared.layout.out_floats * 4) as u64;
        enc.copy_buffer_to_buffer(&w.output, 0, &w.staging, 0, bytes);
        d.queue.submit([enc.finish()]);
        let slice = w.staging.slice(..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
        let [oom, validation, internal] = scopes;
        let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
        let [internal, validation, oom] = errors;
        if let Some(e) = oom {
            // not a broken device: this tile runs on the CPU, and the next may fit
            return Err(Error::Runtime(format!("the GPU ran out of memory: {e}")));
        }
        if let Some(e) = internal.or(validation) {
            mark_broken(&format!("device error: {e}"));
            return Err(Error::Runtime(format!("the GPU reported an error: {e}")));
        }
        if let Err(e) = waited {
            mark_broken(&format!("the GPU did not finish a tile: {e}"));
            return Err(Error::Runtime(format!("the GPU did not finish: {e}")));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(Error::Runtime(format!("reading the result back failed: {e}"))),
            Err(e) => return Err(Error::Runtime(format!("reading the result back failed: {e}"))),
        }
        let out = {
            let data = slice.get_mapped_range().map_err(|e| Error::Runtime(format!("reading the result back failed: {e:?}")))?;
            data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
        };
        w.staging.unmap();
        Ok(out)
    }
}

impl TileRunner for NetRunner {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        if let Some(why) = broken_reason() {
            return Err(Error::Runtime(format!("the GPU is not used any more: {why}")));
        }
        if input.len() != self.shared.layout.in_floats {
            return Err(Error::Input("the tile is not the size the model wants".into()));
        }
        // one tile is one piece of GPU work: a photo's worth would outlast any wait at exit
        let Some(_work) = crate::exit::enter() else { return Err(Error::Runtime(CLOSING.into())) };
        let w = self.take()?;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.execute(&w, input)));
        match r {
            Ok(Ok(out)) => {
                self.give(w);
                Ok(out)
            }
            Ok(Err(e)) => {
                // its readback buffer may be left half-mapped: a fresh workspace is made when one is next needed
                drop(w);
                self.forget();
                Err(e)
            }
            Err(_) => {
                // a panic inside wgpu: this workspace is not given back, and the GPU is not used again
                mark_broken("a denoise tile panicked");
                drop(w);
                self.forget();
                Err(Error::Runtime("the GPU runner gave up".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_denoise::{onnx, reference, synthetic};

    fn net(tile: u64, ch: u64, depth: usize, seed: u64) -> Net {
        let path = std::env::temp_dir().join(format!("lc-nn-{}-{tile}-{ch}-{depth}-{seed}.onnx", std::process::id()));
        std::fs::write(&path, synthetic::unet_onnx(tile, ch, depth, seed)).unwrap();
        let n = onnx::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        n
    }

    fn picture(len: usize, seed: u64) -> Vec<f32> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s >> 12;
                s ^= s << 25;
                s ^= s >> 27;
                (s.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect()
    }

    /// The runner, or `None` (with a note) on a machine with no usable GPU.
    fn gpu(net: &Net, tile: usize) -> Option<NetRunner> {
        match runner(net, tile) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("skipped: no GPU runner here ({e})");
                None
            }
        }
    }

    fn worst(a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len());
        assert!(a.iter().chain(b).all(|v| v.is_finite()), "non-finite denoise output");
        let scale = b.iter().fold(1e-3f32, |m, v| m.max(v.abs()));
        a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs())) / scale
    }

    #[test]
    fn the_gpu_gives_what_the_reference_gives() {
        // (tile, channels, depth): a tile that is not a multiple of the block size, odd channel counts, a deeper net
        for (tile, ch, depth, seed) in
            [(1u64, 4u64, 0usize, 1u64), (3, 4, 0, 8), (6, 4, 1, 9), (20, 12, 2, 2), (32, 8, 2, 3), (48, 12, 2, 4), (64, 16, 3, 5)]
        {
            let n = net(tile, ch, depth, seed);
            let Some(g) = gpu(&n, tile as usize) else { return };
            let x = picture(4 * (tile * tile) as usize, seed);
            let want = reference::run(&n, tile as usize, &x).unwrap();
            let got = g.run(&x).unwrap();
            let cpu = lightcraft_denoise::cpu::NetRunner::new(&n, tile as usize).unwrap().run(&x).unwrap();
            assert!(worst(&got, &cpu) < 1e-3, "GPU and pure-Rust CPU differ");
            let e = worst(&got, &want);
            eprintln!("{}: tile {tile}, {ch} channels, depth {depth}: off by {e:e} of the output's size", g.adapter());
            assert!(e < 2e-4, "tile {tile}, {ch} channels, depth {depth}: the GPU is off by {e:e} of the output's size");
        }
    }

    // Issue #479: several pixels loaded by different invocations shared a vec4,
    // so lane writes raced on Metal. Exercise a partial block repeatedly with new
    // inputs; stale shared-memory values must never leak into the next dispatch.
    #[test]
    fn partial_tiles_remain_correct_when_reusing_a_runner() {
        let n = net(3, 4, 0, 8);
        let Some(g) = gpu(&n, 3) else { return };
        for seed in 100..112 {
            let x = picture(4 * 3 * 3, seed);
            let want = reference::run(&n, 3, &x).unwrap();
            let e = worst(&g.run(&x).unwrap(), &want);
            assert!(e < 2e-4, "seed {seed}: off by {e:e}");
        }
    }

    #[test]
    fn a_runner_serves_many_tiles_from_many_threads() {
        let n = net(32, 8, 2, 7);
        let Some(g) = gpu(&n, 32) else { return };
        let inputs: Vec<Vec<f32>> = (0..12).map(|i| picture(4 * 32 * 32, 100 + i)).collect();
        let wants: Vec<Vec<f32>> = inputs.iter().map(|x| reference::run(&n, 32, x).unwrap()).collect();
        std::thread::scope(|s| {
            for chunk in inputs.chunks(3).zip(wants.chunks(3)) {
                let g = &g;
                s.spawn(move || {
                    for (x, want) in chunk.0.iter().zip(chunk.1) {
                        let e = worst(&g.run(x).unwrap(), want);
                        assert!(e < 2e-4, "off by {e:e}");
                    }
                });
            }
        });
    }

    /// The real model on the GPU against the pure-Rust CPU runner, with timings (the numbers in docs/denoise.md come from here):
    /// `LC_DENOISE_MODEL=<model_bayer.onnx> cargo test --release -p lightcraft-gpu --lib real_model -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the real model: see the doc comment"]
    fn real_model_on_the_gpu_matches_cpu() {
        use lightcraft_denoise::manifest::{DenoiserManifest, Domain, Gain};
        use lightcraft_denoise::runtime::CpuRunner;
        use std::time::Instant;
        let Some(model) = std::env::var_os("LC_DENOISE_MODEL") else { return };
        let net = onnx::read(std::path::Path::new(&model)).unwrap();
        let tile = 512usize;
        let started = Instant::now();
        let g = runner(&net, tile).unwrap();
        println!("{}: set up in {:.2} s, {:.0} MB per tile in flight", g.adapter(), started.elapsed().as_secs_f64(), g.bytes_per_tile() as f64 / 1e6);
        println!("kernels: {}", g.build_report());
        let manifest = DenoiserManifest {
            id: "rawnind-bayer".into(),
            name: "RawNIND Bayer".into(),
            version: "1".into(),
            licence: Default::default(),
            source: None,
            sha256: None,
            size_bytes: None,
            provenance: String::new(),
            domain: Domain::BayerToRgb,
            tile: 512,
            overlap: 64,
            gain: Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 },
        };
        let cpu = CpuRunner::load(std::path::Path::new(&model), &manifest).unwrap();
        // a gradient with noise, in the model's input range
        let noise = picture(4 * tile * tile, 42);
        let input: Vec<f32> =
            (0..4 * tile * tile).map(|i| (0.2 + 0.3 * ((i % tile) as f32 / tile as f32) + 0.1 * (noise[i] - 0.5)).clamp(0.0, 1.0)).collect();
        let t = Instant::now();
        let want = cpu.run(&input).unwrap();
        println!("CPU: {:.0} ms for one tile (one call runs on one core)", t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let got = g.run(&input).unwrap();
        println!("gpu, first tile: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        let e = worst(&got, &want);
        println!("worst difference {e:e} of the output's size");
        assert!(e < 1e-3, "the GPU does not give the CPU answer ({e:e})");
        let mut times: Vec<f64> = (0..8)
            .map(|_| {
                let t = Instant::now();
                g.run(&input).unwrap();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(|a, b| a.total_cmp(b));
        println!("gpu, one tile at a time: median {:.0} ms (best {:.0}, worst {:.0})", times[times.len() / 2], times[0], times[times.len() - 1]);
        // two threads, as the denoiser feeds it
        let t = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    for _ in 0..8 {
                        g.run(&input).unwrap();
                    }
                });
            }
        });
        println!("gpu, 16 tiles from 2 threads: {:.0} ms per tile", t.elapsed().as_secs_f64() * 1000.0 / 16.0);
    }

    /// How long the driver takes to build one conv kernel variant: `LC_SHADER=<file.wgsl> LC_BLOCKS=128,32,16`.
    #[test]
    #[ignore = "a probe for shader compile times"]
    fn time_a_shader_build() {
        let Some(path) = std::env::var_os("LC_SHADER") else { return };
        let blocks: Vec<u32> = std::env::var("LC_BLOCKS").unwrap_or_else(|_| "128,32,16".into()).split(',').filter_map(|v| v.parse().ok()).collect();
        let (bm, bn, bk) = (blocks[0], blocks[1], blocks[2]);
        let src = std::fs::read_to_string(path)
            .unwrap()
            .replace("{{BM}}", &bm.to_string())
            .replace("{{BN}}", &bn.to_string())
            .replace("{{BK}}", &bk.to_string())
            .replace("{{B_VECS}}", &(bk * bn / 4).to_string());
        let d = dev().unwrap();
        let layout = conv_bgl(d);
        let started = std::time::Instant::now();
        let _ = pipeline(d, "probe", &src, &layout);
        println!("{} {bm}x{bn}x{bk}: built in {:.0} ms", d.info.backend.to_str(), started.elapsed().as_secs_f64() * 1000.0);
    }

    #[test]
    fn a_tile_of_the_wrong_size_is_an_error_not_a_crash() {
        let n = net(16, 4, 1, 9);
        let Some(g) = gpu(&n, 16) else { return };
        assert!(g.run(&[0.0; 10]).is_err());
        assert!(g.run(&[]).is_err());
    }

    #[test]
    fn networks_the_kernels_cannot_run_are_refused() {
        let n = net(16, 4, 1, 11);
        // a tile the network does not fit
        assert!(plan(&n, 15, u64::MAX).is_err());
        // a buffer limit smaller than the activations
        assert!(plan(&n, 16, 64).is_err());
    }
}
