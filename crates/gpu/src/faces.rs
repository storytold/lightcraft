//! Face networks on the GPU: an [`lightcraft_faces::net::Accelerator`] that runs the interpreter's convolutions (most of
//! YuNet's, SFace's and AuraFace's time) with a WGSL kernel (`wgsl/faces_conv.wgsl`). Everything else in the graph
//! stays on the CPU interpreter, which is the reference: a net attaches this only after it agreed with the CPU on a
//! test input (`Net::attach_checked`), and drops it on the first error.
//!
//! The device is a separate one (like denoise's), so a face scan and the interactive renders never share a queue, and
//! it respects the same switches as GPU rendering (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, the GPU preference)
//! and the crash sentinel around device creation. A model's weights are uploaded once, on first use, and stay on the
//! device while the accelerator (one per loaded model) lives. Every failure is an `Err`, never a panic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use lightcraft_faces::net::program::{Arg, Binary, INPUT, Op, Program, Unary};
use lightcraft_faces::net::{Accelerator, ConvJob};
use wgpu::util::DeviceExt;

/// Longest wait for one convolution before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Below this many multiply-adds a convolution stays on the CPU: the upload, dispatch and read-back cost more.
const MIN_MACS: usize = 1 << 20;
/// Workgroups a convolution should make at least, cutting its K into slices when its tiles are too few.
const TARGET_WORKGROUPS: usize = 256;
/// Weights one model may keep on the device (AuraFace's are about 250 MB).
const MAX_RESIDENT: u64 = 768 << 20;

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
    layout: wgpu::BindGroupLayout,
    tiled: wgpu::ComputePipeline,
    direct: wgpu::ComputePipeline,
    reduce: wgpu::ComputePipeline,
    ops: Ops,
    /// Bound where a kernel has fewer operands than bindings.
    dummy: wgpu::Buffer,
}

/// The kernels of `wgsl/faces_ops.wgsl`.
struct Ops {
    unary: wgpu::ComputePipeline,
    affine: wgpu::ComputePipeline,
    prelu: wgpu::ComputePipeline,
    binary: wgpu::ComputePipeline,
    transpose: wgpu::ComputePipeline,
    maxpool: wgpu::ComputePipeline,
    resize: wgpu::ComputePipeline,
    gavg: wgpu::ComputePipeline,
    gemv: wgpu::ComputePipeline,
}

static DEV: OnceLock<Result<Dev, String>> = OnceLock::new();
static BROKEN: AtomicBool = AtomicBool::new(false);
static BROKEN_WHY: Mutex<Option<String>> = Mutex::new(None);

fn mark_broken(why: &str) {
    let mut w = BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner);
    if w.is_none() {
        *w = Some(why.to_string());
    }
    BROKEN.store(true, Ordering::Relaxed);
}

/// Why the GPU stopped being used for faces (a device error), if it did.
pub fn broken_reason() -> Option<String> {
    BROKEN.load(Ordering::Relaxed).then(|| BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "device error".into()))
}

fn dev() -> Result<&'static Dev, String> {
    if let Some(why) = broken_reason() {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let _work = match DEV.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or("the app is closing")?),
    };
    DEV.get_or_init(create_device).as_ref().map_err(|e| e.clone())
}

fn create_device() -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    crate::backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| make_device(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn make_device(backends: wgpu::Backends) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = wgpu::Instance::new(desc);
    let wanted = std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let adapter = match &wanted {
        Some(w) => pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .find(|a| {
                let i = a.get_info();
                i.device_type != wgpu::DeviceType::Cpu && i.name.to_lowercase().contains(w.as_str())
            })
            .ok_or_else(|| format!("no GPU adapter named like `{w}`"))?,
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
    if limits.max_storage_buffers_per_shader_stage < 4
        || limits.max_compute_invocations_per_workgroup < 256
        || limits.max_compute_workgroup_size_x < 256
        || limits.max_compute_workgroup_storage_size < 8 * 1024
    {
        return Err(format!("{} is too limited for the face kernels", info.name));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lightcraft-faces"),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    device.on_uncaptured_error(Arc::new(|e| {
        log::error!("gpu (faces): {e}");
        mark_broken(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(|reason, msg| {
        log::error!("gpu (faces): device lost ({reason:?}): {msg}");
        mark_broken(&format!("device lost ({reason:?}): {msg}"));
    });
    use wgpu::BufferBindingType::{Storage, Uniform};
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("faces conv"),
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: false }),
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("faces conv"),
        source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/faces_conv.wgsl").into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let ops_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("faces ops"),
        source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/faces_ops.wgsl").into()),
    });
    let pipe_in = |module: &wgpu::ShaderModule, entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            module,
            entry_point: Some(entry),
            // both kernels write all of their workgroup memory before reading it
            compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
            cache: None,
        })
    };
    let pipe = |entry: &str| pipe_in(&module, entry);
    let op = |entry: &str| pipe_in(&ops_module, entry);
    let (tiled, direct, reduce) = (pipe("tiled"), pipe("direct"), pipe("reduce"));
    let ops = Ops {
        unary: op("unary"),
        affine: op("affine"),
        prelu: op("prelu"),
        binary: op("binary"),
        transpose: op("transpose"),
        maxpool: op("maxpool"),
        resize: op("resize"),
        gavg: op("gavg"),
        gemv: op("gemv"),
    };
    let dummy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("faces unused"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    log::info!("gpu (faces): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits, layout, tiled, direct, reduce, ops, dummy })
}

/// The adapter face scans would use ("AMD Radeon RX 7900 XTX (Vulkan)"), or why they cannot use one. Creates the
/// device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    m: u32,
    n: u32,
    k: u32,
    cg: u32,
    h: u32,
    w: u32,
    ow: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    pt: u32,
    pl: u32,
    bias: u32,
    total: u32,
    row: u32,
    splits: u32,
    kchunk: u32,
    _a: u32,
    _b: u32,
}

/// A layer's weights and bias on the device.
struct Resident {
    weights: wgpu::Buffer,
    bias: wgpu::Buffer,
}

/// The GPU accelerator for one loaded face model: its weights stay on the device while it lives.
pub struct FaceGpu {
    dev: &'static Dev,
    layers: Mutex<HashMap<usize, Arc<Resident>>>,
    consts: Mutex<HashMap<usize, wgpu::Buffer>>,
    resident: AtomicU64,
}

/// A GPU accelerator for one face model, or why there is none (GPU off, missing or broken).
pub fn accelerator() -> Result<Arc<FaceGpu>, String> {
    Ok(Arc::new(FaceGpu { dev: dev()?, layers: Mutex::new(HashMap::new()), consts: Mutex::new(HashMap::new()), resident: AtomicU64::new(0) }))
}

fn u32_of(v: usize) -> Result<u32, String> {
    u32::try_from(v).ok().filter(|v| *v < (1 << 31)).ok_or_else(|| "a layer too large for the GPU kernel".to_string())
}

/// How much of the GPU face work may take: at most `concurrent` runs at once, and busy for at most `share` of the time
/// (after a run of `t`, the next one waits `t · (1 − share) / share`). Set by the background scan's pace.
struct Pace {
    concurrent: usize,
    share: f32,
    running: usize,
}

static PACE: Mutex<Pace> = Mutex::new(Pace { concurrent: usize::MAX, share: 1.0, running: 0 });
static PACE_FREED: std::sync::Condvar = std::sync::Condvar::new();

/// Limit the GPU work face models do: `concurrent` runs at once (at least one) and `share` of the time (0.05 to 1).
/// The background face scan sets it from its pace; anything else that runs a face model shares the same limit.
pub fn set_pace(concurrent: usize, share: f32) {
    let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
    p.concurrent = concurrent.max(1);
    p.share = if share.is_finite() { share.clamp(0.05, 1.0) } else { 1.0 };
    PACE_FREED.notify_all();
}

/// A turn on the GPU under the pace; waits for one. Dropping it gives it back.
struct Turn {
    share: f32,
    started: std::time::Instant,
}

impl Turn {
    fn take() -> Turn {
        let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
        while p.running >= p.concurrent {
            p = PACE_FREED.wait_timeout(p, Duration::from_millis(200)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        p.running += 1;
        Turn { share: p.share, started: std::time::Instant::now() }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        // rest while still holding the turn, so the GPU stays idle for the rest of the period
        if self.share < 1.0 {
            let rest = self.started.elapsed().mul_f32((1.0 - self.share) / self.share).min(Duration::from_secs(2));
            std::thread::sleep(rest);
        }
        let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
        p.running = p.running.saturating_sub(1);
        PACE_FREED.notify_one();
    }
}

/// One dispatch to record: a pipeline, its parameters, its four buffers (three read, the last written) and its
/// workgroup counts.
struct Dispatch {
    pipeline: &'static wgpu::ComputePipeline,
    params: Vec<u8>,
    buffers: [wgpu::Buffer; 4],
    groups: [u32; 3],
}

/// Workgroups for `count` items of 256 (or `count` workgroups when `per_group`): x, y and the row length the kernel
/// reads back as `q(1)`.
fn flat(count: usize, limit: usize) -> Option<[u32; 3]> {
    let blocks = count.max(1);
    let row = blocks.clamp(1, limit.min(65_535));
    let rows = blocks.div_ceil(row);
    (rows <= limit).then_some([u32::try_from(row).ok()?, u32::try_from(rows).ok()?, u32::try_from(row).ok()?])
}

fn params24(values: &[u32]) -> Vec<u8> {
    let mut all = [0u32; 24];
    for (to, v) in all.iter_mut().zip(values) {
        *to = *v;
    }
    bytemuck::cast_slice(&all).to_vec()
}

impl FaceGpu {
    /// Bytes of weights this model keeps on the device.
    pub fn resident_bytes(&self) -> u64 {
        self.resident.load(Ordering::Relaxed)
    }

    fn upload(&self, label: &str, data: &[f32]) -> wgpu::Buffer {
        let zero = [0f32; 4];
        self.dev.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(if data.is_empty() { &zero } else { data }),
            usage: wgpu::BufferUsages::STORAGE,
        })
    }

    /// Whether `bytes` more may stay on the device; counts them if so.
    fn keep(&self, bytes: u64) -> bool {
        if self.resident_bytes().saturating_add(bytes) > MAX_RESIDENT {
            return false;
        }
        self.resident.fetch_add(bytes, Ordering::Relaxed);
        true
    }

    /// A convolution's weights and bias: kept on the device while the budget lasts, uploaded for this run past it.
    fn resident(&self, job: &ConvJob) -> Arc<Resident> {
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = layers.get(&job.layer) {
            return r.clone();
        }
        let r = Arc::new(Resident { weights: self.upload("faces weights", job.weights), bias: self.upload("faces bias", job.bias.unwrap_or(&[])) });
        if self.keep((job.weights.len() as u64 + job.cout as u64) * 4) {
            layers.insert(job.layer, r.clone());
        }
        r
    }

    /// A program's constant, kept on the device like the weights.
    fn constant(&self, id: usize, data: &[f32]) -> wgpu::Buffer {
        let mut consts = self.consts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(b) = consts.get(&id) {
            return b.clone();
        }
        let b = self.upload("faces constant", data);
        if self.keep(data.len() as u64 * 4) {
            consts.insert(id, b.clone());
        }
        b
    }

    fn largest_binding(&self) -> u64 {
        self.dev.limits.max_storage_buffer_binding_size.min(self.dev.limits.max_buffer_size)
    }

    /// The dispatches of one convolution reading `input` and writing `output`, or `None` when it exceeds the device.
    fn conv_dispatches(&self, job: &ConvJob, input: &wgpu::Buffer, output: &wgpu::Buffer) -> Result<Option<Vec<Dispatch>>, String> {
        let d = self.dev;
        let groups = job.groups.max(1);
        let (m, cg) = (job.cout / groups, job.cin / groups);
        let n = job.oh.checked_mul(job.ow).ok_or("a layer too large")?;
        let k = cg.checked_mul(job.kh).and_then(|v| v.checked_mul(job.kw)).ok_or("a layer too large")?;
        let total = job.cout.checked_mul(n).ok_or("a layer too large")?;
        let direct = m < 8;
        // Too few 64 x 64 tiles to keep the GPU busy (a 7 x 7 map of 512 channels makes 8): cut K into slices that
        // run side by side, and sum them in a second pass.
        let tiles = n.div_ceil(64) * m.div_ceil(64) * groups;
        let splits = if direct { 1 } else { (TARGET_WORKGROUPS / tiles.max(1)).clamp(1, (k / 128).max(1)) };
        let kchunk = k.div_ceil(splits).div_ceil(16) * 16;
        let splits = k.div_ceil(kchunk.max(1)).max(1);
        let partial = total.checked_mul(splits).ok_or("a layer too large")?;
        let cin_len = job.cin.checked_mul(job.h).and_then(|v| v.checked_mul(job.w)).ok_or("a layer too large")?;
        let largest = [cin_len, job.weights.len(), partial].into_iter().max().unwrap_or(0) as u64 * 4;
        let limit = d.limits.max_compute_workgroups_per_dimension as usize;
        let Some(sum) = flat(total.div_ceil(256), limit) else { return Ok(None) };
        let (wx, wy, wz) = if direct { (sum[0] as usize, sum[1] as usize, 1) } else { (n.div_ceil(64), m.div_ceil(64), groups * splits) };
        if largest > self.largest_binding() || [wx, wy, wz].into_iter().any(|v| v > limit) {
            return Ok(None);
        }
        let res = self.resident(job);
        let params = Params {
            m: u32_of(m)?,
            n: u32_of(n)?,
            k: u32_of(k)?,
            cg: u32_of(cg)?,
            h: u32_of(job.h)?,
            w: u32_of(job.w)?,
            ow: u32_of(job.ow)?,
            kh: u32_of(job.kh)?,
            kw: u32_of(job.kw)?,
            sh: u32_of(job.sh)?,
            sw: u32_of(job.sw)?,
            pt: u32_of(job.pt)?,
            pl: u32_of(job.pl)?,
            bias: u32::from(job.bias.is_some()),
            total: u32_of(total)?,
            row: sum[2],
            splits: u32_of(splits)?,
            kchunk: u32_of(kchunk)?,
            _a: 0,
            _b: 0,
        };
        let params = bytemuck::bytes_of(&params).to_vec();
        let main = |out: &wgpu::Buffer| Dispatch {
            pipeline: if direct { &d.direct } else { &d.tiled },
            params: params.clone(),
            buffers: [input.clone(), res.weights.clone(), res.bias.clone(), out.clone()],
            groups: [wx as u32, wy as u32, wz as u32],
        };
        if splits == 1 {
            return Ok(Some(vec![main(output)]));
        }
        let partials = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces partial sums"),
            size: partial as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Ok(Some(vec![
            main(&partials),
            Dispatch {
                pipeline: &d.reduce,
                params,
                buffers: [partials, res.weights.clone(), res.bias.clone(), output.clone()],
                groups: [sum[0], sum[1], 1],
            },
        ]))
    }

    /// Record `list` into `enc` as one compute pass, with all their parameters in one uniform buffer.
    fn record(&self, enc: &mut wgpu::CommandEncoder, list: &[Dispatch]) -> Result<(), String> {
        let d = self.dev;
        if list.is_empty() {
            return Ok(());
        }
        let align = (d.limits.min_uniform_buffer_offset_alignment as usize).max(256);
        let mut blob = vec![0u8; list.len() * align];
        for (i, x) in list.iter().enumerate() {
            blob.get_mut(i * align..i * align + x.params.len()).ok_or("parameters too large")?.copy_from_slice(&x.params);
        }
        let uniform = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces params"),
            contents: &blob,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let binds = list
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let [a, b, c, out] = &x.buffers;
                d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &d.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &uniform,
                                offset: (i * align) as u64,
                                size: std::num::NonZeroU64::new(x.params.len() as u64),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: a.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: b.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
                    ],
                })
            })
            .collect::<Vec<_>>();
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("faces"), timestamp_writes: None });
        for (x, bind) in list.iter().zip(&binds) {
            pass.set_pipeline(x.pipeline);
            pass.set_bind_group(0, bind, &[]);
            let [gx, gy, gz] = x.groups;
            pass.dispatch_workgroups(gx, gy, gz);
        }
        Ok(())
    }

    /// Submit `enc`, which ends by copying into `staging` (`bytes` long), wait for it and read `staging` back.
    fn finish(&self, enc: wgpu::CommandEncoder, staging: &wgpu::Buffer, scopes: [wgpu::ErrorScopeGuard; 2]) -> Result<Vec<f32>, String> {
        let d = self.dev;
        let index = d.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(TIMEOUT) });
        let [oom, validation] = scopes;
        let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
        if let Some(e) = oom {
            return Err(format!("the GPU ran out of memory: {e}"));
        }
        if let Some(e) = validation {
            mark_broken(&format!("device error: {e}"));
            return Err(format!("the GPU reported an error: {e}"));
        }
        if let Err(e) = waited {
            mark_broken(&format!("the GPU did not finish a face model: {e}"));
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
        Ok(out)
    }

    fn scopes(&self) -> [wgpu::ErrorScopeGuard; 2] {
        [self.dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), self.dev.device.push_error_scope(wgpu::ErrorFilter::Validation)]
    }

    fn staging(&self, bytes: u64) -> wgpu::Buffer {
        self.dev.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces read-back"),
            size: bytes.max(4),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn scratch(&self, bytes: u64) -> wgpu::Buffer {
        self.dev.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces values"),
            size: bytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    }

    /// One convolution, uploaded and read back on its own.
    fn run(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        let total = job.cout.checked_mul(job.oh).and_then(|v| v.checked_mul(job.ow)).ok_or("a layer too large")?;
        let bytes = total as u64 * 4;
        if bytes > self.largest_binding() || job.input.len() as u64 * 4 > self.largest_binding() {
            return Ok(None);
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let _turn = Turn::take();
        let d = self.dev;
        let scopes = self.scopes();
        let input = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces input"),
            contents: bytemuck::cast_slice(job.input),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let output = self.scratch(bytes);
        let Some(list) = self.conv_dispatches(job, &input, &output)? else {
            // leave the scopes balanced
            let [oom, validation] = scopes;
            let _ = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
            return Ok(None);
        };
        let staging = self.staging(bytes);
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.record(&mut enc, &list)?;
        enc.copy_buffer_to_buffer(&output, 0, &staging, 0, bytes);
        self.finish(enc, &staging, scopes).map(Some)
    }

    /// The dispatch of one non-convolution step.
    fn op_dispatch(&self, op: &Op, out_len: usize, args: [wgpu::Buffer; 3], out: &wgpu::Buffer) -> Result<Option<Dispatch>, String> {
        let d = self.dev;
        let limit = d.limits.max_compute_workgroups_per_dimension as usize;
        let u = u32_of;
        let n = u(out_len)?;
        let per_value = |pipeline: &'static wgpu::ComputePipeline, rest: &[u32]| -> Option<Dispatch> {
            let groups = flat(out_len.div_ceil(256), limit)?;
            let mut all = vec![n, groups[2]];
            all.extend_from_slice(rest);
            Some(Dispatch {
                pipeline,
                params: params24(&all),
                buffers: [args[0].clone(), args[1].clone(), args[2].clone(), out.clone()],
                groups: [groups[0], groups[1], 1],
            })
        };
        let per_group = |pipeline: &'static wgpu::ComputePipeline, rest: &[u32]| -> Option<Dispatch> {
            let groups = flat(out_len, limit)?;
            let mut all = vec![n, groups[2]];
            all.extend_from_slice(rest);
            Some(Dispatch {
                pipeline,
                params: params24(&all),
                buffers: [args[0].clone(), args[1].clone(), args[2].clone(), out.clone()],
                groups: [groups[0], groups[1], 1],
            })
        };
        let four = |v: &[usize; 4]| -> Result<[u32; 4], String> { Ok([u(v[0])?, u(v[1])?, u(v[2])?, u(v[3])?]) };
        let k = &d.ops;
        Ok(match op {
            Op::Conv(_) => None,
            Op::Unary(f) => per_value(&k.unary, &[u32::from(*f == Unary::Sigmoid)]),
            Op::ChannelAffine { channels, plane } => per_value(&k.affine, &[u(*plane)?, u(*channels)?]),
            Op::ChannelPrelu { channels, plane } => per_value(&k.prelu, &[u(*plane)?, u(*channels)?]),
            Op::Binary { op, dims, a, b } => {
                let kind = match op {
                    Binary::Add => 0,
                    Binary::Sub => 1,
                    Binary::Mul => 2,
                };
                let mut rest = vec![kind];
                rest.extend(four(dims)?);
                rest.extend(four(a)?);
                rest.extend(four(b)?);
                per_value(&k.binary, &rest)
            }
            Op::Transpose { dims, strides } => {
                let mut rest = vec![0];
                rest.extend(four(dims)?);
                rest.extend(four(strides)?);
                per_value(&k.transpose, &rest)
            }
            Op::MaxPool { h, w, kh, kw, sh, sw, oh, ow, .. } => {
                per_value(&k.maxpool, &[u(*h)?, u(*w)?, u(*kh)?, u(*kw)?, u(*sh)?, u(*sw)?, u(*oh)?, u(*ow)?])
            }
            Op::Resize { h, w, sh, sw, .. } => per_value(&k.resize, &[u(*h)?, u(*w)?, u(*sh)?, u(*sw)?]),
            Op::GlobalAverage { plane, .. } => per_group(&k.gavg, &[u(*plane)?]),
            Op::Gemv { k: kk, n: nn, trans_b, alpha } => per_group(&k.gemv, &[u(*kk)?, u(*nn)?, u32::from(*trans_b), alpha.to_bits()]),
        })
    }

    /// A whole program for each input, every value staying on the device; one submission and one read-back.
    fn program(&self, prog: &Program, inputs: &[&[f32]]) -> Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        let d = self.dev;
        let largest = self.largest_binding();
        if prog.lens.iter().any(|l| *l as u64 * 4 > largest) || inputs.iter().any(|i| Some(&i.len()) != prog.lens.get(INPUT)) {
            return Ok(None);
        }
        let out_bytes: u64 = prog.outputs.iter().map(|(_, v, _)| prog.lens.get(*v).copied().unwrap_or(0) as u64 * 4).sum();
        let total_bytes = out_bytes.checked_mul(inputs.len() as u64).ok_or("too many faces at once")?;
        if total_bytes > d.limits.max_buffer_size {
            return Ok(None);
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let _turn = Turn::take();
        let scopes = self.scopes();
        let staging = self.staging(total_bytes);
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("faces program") });
        // buffers nothing reads any more, reused for later values (best fit)
        let mut pool: Vec<(u64, wgpu::Buffer)> = Vec::new();
        let take = |pool: &mut Vec<(u64, wgpu::Buffer)>, bytes: u64| -> wgpu::Buffer {
            let best = pool
                .iter()
                .enumerate()
                .filter(|(_, (size, _))| *size >= bytes && *size <= bytes.saturating_mul(2))
                .min_by_key(|(_, (size, _))| *size)
                .map(|(i, _)| i);
            match best {
                Some(i) => pool.swap_remove(i).1,
                None => self.scratch(bytes),
            }
        };
        let mut at = 0u64;
        let mut fits = true;
        for input in inputs {
            let mut values: Vec<Option<(u64, wgpu::Buffer)>> = vec![None; prog.lens.len()];
            // a fresh buffer: queued writes land before any of the encoder's work, so a pooled one could still be in use
            let first = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("faces input"),
                contents: bytemuck::cast_slice(input),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
            if let Some(v) = values.get_mut(INPUT) {
                *v = Some((input.len() as u64 * 4, first));
            }
            let mut list = Vec::new();
            for step in &prog.steps {
                let len = prog.lens.get(step.out).copied().ok_or("a step writes a value the program does not have")?;
                let out = take(&mut pool, len as u64 * 4);
                let mut args = Vec::with_capacity(3);
                for a in &step.args {
                    args.push(match a {
                        Arg::Value(v) => {
                            values.get(*v).and_then(|v| v.as_ref()).map(|(_, b)| b.clone()).ok_or("a step reads a value before it is made")?
                        }
                        Arg::Const { id, data } => self.constant(*id, data),
                    });
                }
                let dispatches = match &step.op {
                    Op::Conv(job) => {
                        let input = args.first().ok_or("a convolution without input")?;
                        self.conv_dispatches(job, input, &out)?
                    }
                    op => {
                        let arg = |i: usize| args.get(i).cloned().unwrap_or_else(|| d.dummy.clone());
                        self.op_dispatch(op, len, [arg(0), arg(1), arg(2)], &out)?.map(|x| vec![x])
                    }
                };
                let Some(dispatches) = dispatches else {
                    fits = false;
                    break;
                };
                list.extend(dispatches);
                if let Some(v) = values.get_mut(step.out) {
                    *v = Some((len as u64 * 4, out));
                }
                for f in &step.free {
                    if let Some(b) = values.get_mut(*f).and_then(Option::take) {
                        pool.push(b);
                    }
                }
            }
            if !fits {
                break;
            }
            self.record(&mut enc, &list)?;
            for (_, v, _) in &prog.outputs {
                let (bytes, buffer) = values.get(*v).and_then(|v| v.as_ref()).ok_or("an output was never made")?;
                let len = prog.lens.get(*v).copied().unwrap_or(0) as u64 * 4;
                enc.copy_buffer_to_buffer(buffer, 0, &staging, at, len.min(*bytes));
                at += len;
            }
            pool.extend(values.into_iter().flatten());
        }
        if !fits {
            let [oom, validation] = scopes;
            let _ = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
            return Ok(None);
        }
        let flat_out = self.finish(enc, &staging, scopes)?;
        let mut rest = flat_out.as_slice();
        let mut per_input = Vec::with_capacity(inputs.len());
        for _ in inputs {
            let mut outs = Vec::with_capacity(prog.outputs.len());
            for (_, v, _) in &prog.outputs {
                let len = prog.lens.get(*v).copied().unwrap_or(0);
                let (this, next) = rest.split_at_checked(len).ok_or("the read-back is short")?;
                outs.push(this.to_vec());
                rest = next;
            }
            per_input.push(outs);
        }
        Ok(Some(per_input))
    }
}

/// Run `f`, turning a panic into a broken device.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        mark_broken("a face model on the GPU panicked");
        Err("the GPU runner gave up".into())
    })
}

impl Accelerator for FaceGpu {
    fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    fn conv(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if crate::switched_off().is_some() || job.macs() < MIN_MACS {
            return Ok(None);
        }
        guarded(|| self.run(job))
    }

    fn run_program(&self, program: &Program, inputs: &[&[f32]]) -> Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if crate::switched_off().is_some() || PER_CONV.load(Ordering::Relaxed) {
            return Ok(None);
        }
        guarded(|| self.program(program, inputs))
    }
}

/// Tests and benchmarks only: run convolutions one by one (the first step's way) instead of whole programs.
static PER_CONV: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    /// The face detector's kernels validate without a GPU.
    #[test]
    fn the_face_kernels_validate_without_a_gpu() {
        assert_eq!(crate::validate_wgsl("faces_conv", include_str!("wgsl/faces_conv.wgsl")), Ok(()));
        assert_eq!(crate::validate_wgsl("faces_ops", include_str!("wgsl/faces_ops.wgsl")), Ok(()));
    }

    fn gpu() -> Option<Arc<FaceGpu>> {
        match accelerator() {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("no GPU for the face kernels, skipping: {e}");
                None
            }
        }
    }

    /// The plain definition of a convolution, to check the kernels against.
    fn reference(j: &ConvJob) -> Vec<f32> {
        let (m, cg) = (j.cout / j.groups, j.cin / j.groups);
        let mut out = vec![0f32; j.cout * j.oh * j.ow];
        for co in 0..j.cout {
            let g = co / m;
            for oy in 0..j.oh {
                for ox in 0..j.ow {
                    let mut v = j.bias.map_or(0.0, |b| b[co]);
                    for ci in 0..cg {
                        for ky in 0..j.kh {
                            for kx in 0..j.kw {
                                let (iy, ix) = ((oy * j.sh + ky) as isize - j.pt as isize, (ox * j.sw + kx) as isize - j.pl as isize);
                                if iy >= 0 && ix >= 0 && (iy as usize) < j.h && (ix as usize) < j.w {
                                    v += j.weights[((co * cg + ci) * j.kh + ky) * j.kw + kx]
                                        * j.input[((g * cg + ci) * j.h + iy as usize) * j.w + ix as usize];
                                }
                            }
                        }
                    }
                    out[(co * j.oh + oy) * j.ow + ox] = v;
                }
            }
        }
        out
    }

    #[test]
    fn the_gpu_convolution_gives_what_the_definition_gives() {
        let Some(gpu) = gpu() else { return };
        // (cin, cout, groups, k, stride, pad, h, w): dense, strided, depthwise, grouped, pointwise, odd sizes
        for (layer, (cin, cout, groups, k, s, p, h, w)) in [
            (3, 16, 1, 3, 2, 1, 41, 37),
            (64, 64, 1, 3, 1, 1, 28, 28),
            (32, 32, 32, 3, 1, 1, 30, 31),
            (16, 32, 4, 3, 2, 1, 33, 20),
            (48, 72, 1, 1, 1, 0, 50, 45),
            (8, 130, 1, 5, 1, 2, 23, 19),
        ]
        .into_iter()
        .enumerate()
        {
            let cg = cin / groups;
            let weights: Vec<f32> = (0..cout * cg * k * k).map(|i| ((i * 37 % 23) as f32 - 11.0) * 0.03).collect();
            let bias: Vec<f32> = (0..cout).map(|i| i as f32 * 0.01 - 0.3).collect();
            let input: Vec<f32> = (0..cin * h * w).map(|i| ((i * 13 % 29) as f32 - 14.0) * 0.07).collect();
            let (oh, ow) = ((h + 2 * p - k) / s + 1, (w + 2 * p - k) / s + 1);
            let job = ConvJob {
                layer,
                cin,
                h,
                w,
                cout,
                groups,
                kh: k,
                kw: k,
                sh: s,
                sw: s,
                pt: p,
                pl: p,
                oh,
                ow,
                weights: &weights,
                bias: Some(&bias),
                input: &input,
            };
            let got = gpu.run(&job).unwrap().unwrap();
            let want = reference(&job);
            let worst = got.iter().zip(&want).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-3, "case {layer}: off by {worst}");
        }
        assert!(gpu.resident_bytes() > 0, "the weights stay on the device");
    }

    #[test]
    fn every_operator_on_the_gpu_gives_what_the_cpu_gives() {
        let Some(gpu) = gpu() else { return };
        let net = lightcraft_faces::net::Net::new(lightcraft_faces::synthetic::every_operator_graph()).unwrap();
        let inputs: Vec<lightcraft_faces::net::Tensor> = (0..3)
            .map(|s| {
                lightcraft_faces::net::Tensor::new(vec![1, 4, 8, 8], (0..256).map(|i| (((i * 31 + s * 7) % 37) as f32 - 18.0) * 0.05).collect())
                    .unwrap()
            })
            .collect();
        let cpu: Vec<_> = inputs.iter().map(|x| net.run(x.clone()).unwrap()).collect();
        let program = net.program(&[1, 4, 8, 8]).unwrap().unwrap();
        let slices: Vec<&[f32]> = inputs.iter().map(|t| t.data.as_slice()).collect();
        let got = gpu.program(&program, &slices).unwrap().unwrap();
        for (want, got) in cpu.iter().zip(&got) {
            for ((name, want), got) in want.iter().zip(got) {
                let worst = want.data.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(worst < 1e-4, "{name}: off by {worst}");
            }
        }
    }

    /// The real models on the GPU agree with the CPU interpreter: `LC_FACE_MODELS=<folder>` holding
    /// `face_detection_yunet_2023mar.onnx`, `face_recognition_sface_2021dec.onnx` and (optionally) `glintr100.onnx`.
    #[test]
    fn real_face_models_on_the_gpu_agree_with_the_cpu() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else {
            eprintln!("LC_FACE_MODELS is not set: skipping");
            return;
        };
        if gpu().is_none() {
            return;
        }
        let det = lightcraft_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (900, 700);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) as f32 * 0.25 + (i / 3 / w) as f32 * 0.3 + (i * 7919 % 41) as f32) as u8).collect();
        let opts = lightcraft_faces::yunet::Options { score: 0.0, ..Default::default() };
        let cpu = det.detect(&rgb, w, h, &opts).unwrap();
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let fast = det.detect(&rgb, w, h, &opts).unwrap();
        assert!(det.net().accelerator().is_some());
        assert_eq!(cpu.len(), fast.len());
        for (a, b) in cpu.iter().zip(&fast) {
            assert!((a.score - b.score).abs() < 1e-3, "{} vs {}", a.score, b.score);
        }
        for (file, sha) in [
            ("face_recognition_sface_2021dec.onnx", lightcraft_faces::known::SFACE_SHA256),
            ("glintr100.onnx", lightcraft_faces::known::AURAFACE_SHA256),
        ] {
            let path = dir.join(file);
            if !path.exists() {
                continue;
            }
            let e = lightcraft_faces::runtime::Embedder::load(&path, &lightcraft_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let face: Vec<u8> = (0..ew * eh * 3).map(|i| ((i * 13) % 251) as u8).collect();
            let cpu = e.embed(&face).unwrap();
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let fast = e.embed(&face).unwrap();
            let cosine: f32 = cpu.iter().zip(&fast).map(|(a, b)| a * b).sum();
            assert!(cosine > 0.9999, "{file}: cosine {cosine}");
        }
    }

    /// Timings, CPU against the GPU one convolution at a time and as whole programs (one face, and a batch of 16):
    /// `LC_FACE_MODELS=<folder> cargo test --release -p lightcraft-gpu faces::tests::timings -- --ignored --nocapture`.
    #[test]
    #[ignore = "a benchmark: needs LC_FACE_MODELS and a GPU"]
    fn timings() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else { return };
        let Some(_) = gpu() else { return };
        let time = |runs: usize, f: &mut dyn FnMut()| {
            f();
            let t = std::time::Instant::now();
            for _ in 0..runs {
                f();
            }
            t.elapsed().as_secs_f64() * 1000.0 / runs as f64
        };
        let det = lightcraft_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (1024, 768);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let opts = lightcraft_faces::yunet::Options::default();
        let cpu = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        PER_CONV.store(true, Ordering::Relaxed);
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let per_conv = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        PER_CONV.store(false, Ordering::Relaxed);
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let whole = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        assert!(det.net().accelerator().is_some());
        eprintln!("YuNet 640: CPU {cpu:.1} ms, GPU per convolution {per_conv:.1} ms, GPU whole network {whole:.1} ms");
        for (file, sha) in [
            ("face_recognition_sface_2021dec.onnx", lightcraft_faces::known::SFACE_SHA256),
            ("glintr100.onnx", lightcraft_faces::known::AURAFACE_SHA256),
        ] {
            let e = lightcraft_faces::runtime::Embedder::load(&dir.join(file), &lightcraft_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let faces: Vec<Vec<u8>> = (0..16).map(|s| (0..ew * eh * 3).map(|i| ((i + s * 101) * 13 % 251) as u8).collect()).collect();
            let refs: Vec<&[u8]> = faces.iter().map(Vec::as_slice).collect();
            let cpu = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            PER_CONV.store(true, Ordering::Relaxed);
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let per_conv = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            PER_CONV.store(false, Ordering::Relaxed);
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let whole = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            let batch = time(3, &mut || drop(e.embed_batch(&refs))) / 16.0;
            assert!(e.net().accelerator().is_some());
            eprintln!(
                "{file}: CPU {cpu:.1} ms, GPU per convolution {per_conv:.1} ms, GPU whole network {whole:.1} ms, batch of 16 {batch:.1} ms a face"
            );
        }
    }
}
