//! The wgpu device, the compiled kernels, buffers, dispatch and readback.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A kernel module: WGSL source, its storage bindings (name, writable, element type) after the
/// parameter block `P` (binding 0), and its entry points.
struct Module {
    src: &'static str,
    bindings: &'static [(&'static str, bool, &'static str)],
    entries: &'static [&'static str],
}

const MODULES: &[Module] = &[
    Module {
        src: include_str!("wgsl/finish.wgsl"),
        bindings: &[
            ("img", false, "f32"),
            ("log_l", false, "f32"),
            ("base_p", false, "f32"),
            ("clar", false, "f32"),
            ("tex", false, "f32"),
            ("dark", false, "f32"),
            ("masks", false, "f32"),
            ("aux", false, "f32"),
            ("out", true, "u32"),
        ],
        entries: &["main"],
    },
    Module {
        src: include_str!("wgsl/blur.wgsl"),
        bindings: &[("src", false, "f32"), ("dst", true, "f32")],
        entries: &["box_h", "box_v", "minmax", "median_h", "median_v"],
    },
    Module {
        src: include_str!("wgsl/resize.wgsl"),
        bindings: &[("src", false, "f32"), ("table", false, "u32"), ("dst", true, "f32")],
        entries: &["resize_h", "resize_v"],
    },
    Module {
        src: include_str!("wgsl/map.wgsl"),
        bindings: &[("a", false, "f32"), ("b", false, "f32"), ("c", false, "f32"), ("dst", true, "f32")],
        entries: &[
            "log_lum_k",
            "dark_k",
            "guided_pre",
            "guided_ab",
            "guided_apply",
            "wb_k",
            "nr_lum",
            "chroma_k",
            "nr_col",
            "subsample",
            "redeye_k",
            "xguided_pre",
            "xguided_ab",
            "xguided_apply",
            "fringe_lab",
            "fringe_apply",
        ],
    },
    Module {
        src: include_str!("wgsl/spot.wgsl"),
        bindings: &[("img", true, "f32"), ("pts", false, "f32"), ("al", true, "f32"), ("sp", true, "f32"), ("df", true, "f32"), ("wt", true, "f32")],
        entries: &["spot_prep", "spot_apply", "spot_score"],
    },
    Module { src: include_str!("wgsl/hist.wgsl"), bindings: &[("px", false, "u32"), ("bins", true, "atomic<u32>")], entries: &["histogram"] },
    Module {
        src: include_str!("wgsl/mask.wgsl"),
        bindings: &[("img", false, "f32"), ("log_l", false, "f32"), ("aux", false, "f32"), ("c", true, "f32"), ("alpha", true, "f32")],
        entries: &["shape", "combine", "finalize"],
    },
    Module {
        src: include_str!("wgsl/present.wgsl"),
        bindings: &[("px", false, "u32"), ("dst", true, "u32"), ("count", true, "atomic<u32>")],
        entries: &["unwritten", "pad_rows"],
    },
    Module {
        src: include_str!("wgsl/geom.wgsl"),
        bindings: &[("src", false, "f32"), ("dst", true, "f32"), ("coverage", false, "u32")],
        entries: &["orient", "sample_affine", "sample_warp"],
    },
];

struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    nbuf: usize,
}

/// A module's shader and layouts, shared by the kernels of its entry points.
struct Prepared {
    module: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    nbuf: usize,
}

/// Validation and internal error scopes. They belong to the thread that pushed them.
fn push_scopes(device: &wgpu::Device) -> [wgpu::ErrorScopeGuard; 2] {
    [device.push_error_scope(wgpu::ErrorFilter::Validation), device.push_error_scope(wgpu::ErrorFilter::Internal)]
}

/// The error the scopes caught, if any (popped innermost first).
fn pop_scopes(scopes: [wgpu::ErrorScopeGuard; 2]) -> Option<String> {
    let [validation, internal] = scopes;
    let (internal, validation) = (pollster::block_on(internal.pop()), pollster::block_on(validation.pop()));
    internal.or(validation).map(|e| e.to_string())
}

/// Build every kernel. A kernel the driver's compiler refuses is a validation or an internal error,
/// and then there is no GPU, not a half-built one.
///
/// Issue #2936: built one after another, the 27 pipelines took ~2.2 s on an Intel iGPU under DX12
/// (device start-up 1.7–4.4 s in all, 7 s on the first run after an install), where FXC (issue #471)
/// compiles each one, single-threaded, inside `create_compute_pipeline`. The
/// device takes pipeline creation from any thread, so the entry points are shared out to worker
/// threads. This thread works too: every kernel is built even when no thread can be started.
fn build_kernels(device: &wgpu::Device) -> Result<HashMap<&'static str, Kernel>, String> {
    let scopes = push_scopes(device);
    let consts = constants();
    let prepared: Vec<Prepared> = MODULES
        .iter()
        .map(|m| {
            let src = module_source(m, &consts);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(src.into()) });
            let mut entries = vec![storage_entry(0, true)];
            entries.extend(m.bindings.iter().enumerate().map(|(i, (_, w, _))| storage_entry(i as u32 + 1, !w)));
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &entries });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            Prepared { module, layout, pipeline_layout, nbuf: m.bindings.len() }
        })
        .collect();
    if let Some(e) = pop_scopes(scopes) {
        return Err(e);
    }
    let jobs: Vec<(&Prepared, &'static str)> = MODULES.iter().zip(&prepared).flat_map(|(m, p)| m.entries.iter().map(move |e| (p, *e))).collect();
    let next = AtomicUsize::new(0);
    let built = std::sync::Mutex::new(HashMap::with_capacity(jobs.len()));
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len()).saturating_sub(1);
    let errors: Vec<String> = std::thread::scope(|s| {
        let work = || build_jobs(device, &jobs, &next, &built);
        let threads: Vec<_> =
            (0..workers).filter_map(|_| std::thread::Builder::new().name("lc-gpu-kernels".into()).spawn_scoped(s, work).ok()).collect();
        let mut errors: Vec<String> = work().into_iter().collect();
        for t in threads {
            match t.join() {
                Ok(e) => errors.extend(e),
                Err(_) => errors.push("a kernel build thread panicked".into()),
            }
        }
        errors
    });
    if let Some(e) = errors.into_iter().next() {
        return Err(e);
    }
    Ok(built.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// One worker of [`build_kernels`]: build kernels until none is left; the first error its scopes caught.
fn build_jobs(
    device: &wgpu::Device,
    jobs: &[(&Prepared, &'static str)],
    next: &AtomicUsize,
    built: &std::sync::Mutex<HashMap<&'static str, Kernel>>,
) -> Option<String> {
    let scopes = push_scopes(device);
    while let Some(&(p, entry)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&p.pipeline_layout),
            module: &p.module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        let kernel = Kernel { pipeline, layout: p.layout.clone(), nbuf: p.nbuf };
        built.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(entry, kernel);
    }
    pop_scopes(scopes)
}

/// A device buffer of `len` 32-bit values.
///
/// Dropped buffers are recycled: they first wait in a per-thread list (commands recorded on this
/// thread may still use them) and become reusable once this thread submits ([`Gpu::submitted`]),
/// as the queue then runs every earlier use before any later one. Reuse matters: allocating (and
/// zero-filling) a fresh 100–300 MB buffer per pass costs as much as the pass itself.
pub struct Buf {
    buf: Option<Tracked>,
    pub len: usize,
}

impl Buf {
    /// The generation of the device the buffer was made on ([`Gpu::generation`]).
    pub fn generation(&self) -> u64 {
        self.buf.as_ref().map_or(0, |b| b.1)
    }

    pub fn raw(&self) -> &wgpu::Buffer {
        // `buf` is `Some` from construction (`Gpu::buffer`/`upload`) until `Drop` takes it back
        // for the pool, so a live `Buf` always has it.
        #[allow(clippy::expect_used)]
        &self.buf.as_ref().expect("live buffer").0
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        let Some(b) = self.buf.take() else { return };
        // Outside a render this thread has no commands recorded (only renders record), so the
        // buffer is reusable at once; without this, buffers dropped on threads that rarely
        // submit (the UI thread clearing a view's stages) would wait indefinitely.
        let Ok(in_render) = IN_RENDER.try_with(|c| c.get() > 0) else {
            // thread teardown: wgpu's thread-locals may be gone too, and dropping (or trimming
            // the pool, which drops) could abort — leak it (the device owns its memory)
            std::mem::forget(b);
            return;
        };
        if !in_render && let Some(g) = crate::existing_device() {
            g.pool([b]);
            return;
        }
        // during thread teardown the list may already be gone: then the buffer leaks
        let size = b.0.size();
        let mut b = Some(b);
        let _ = RETIRED.try_with(|r| r.0.borrow_mut().extend(b.take()));
        match b {
            // Leaking is deliberate during thread teardown — dropping a wgpu buffer whose device
            // thread-locals have already gone can abort the process. The device owns it.
            Some(b) => std::mem::forget(b), // NOSONAR
            None => {
                RETIRED_BYTES.fetch_add(size, Ordering::Relaxed);
            }
        }
    }
}

/// Buffers dropped on a thread since its last submit.
struct Retired(std::cell::RefCell<Vec<Tracked>>);

impl Drop for Retired {
    /// At thread exit wgpu's own thread-locals may already be destroyed, and dropping a buffer
    /// then aborts the process: leak the few buffers left instead (the device owns their memory).
    fn drop(&mut self) {
        for b in self.0.get_mut().drain(..) {
            RETIRED_BYTES.fetch_sub(b.0.size(), Ordering::Relaxed);
            std::mem::forget(b);
        }
    }
}

/// This thread's retired buffers (empty during thread teardown).
fn take_retired() -> Vec<Tracked> {
    let retired = RETIRED.try_with(|r| std::mem::take(&mut *r.0.borrow_mut())).unwrap_or_default();
    RETIRED_BYTES.fetch_sub(retired.iter().map(|b| b.0.size()).sum(), Ordering::Relaxed);
    retired
}

thread_local! {
    /// Renders running on this thread (they may hold recorded, unsubmitted commands).
    static IN_RENDER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Marks a render on this thread; when it ends, the buffers it released join the pool (everything
/// it recorded has been submitted by then).
pub(crate) struct RenderScope<'a>(&'a Gpu);

impl<'a> RenderScope<'a> {
    pub fn new(g: &'a Gpu) -> RenderScope<'a> {
        IN_RENDER.with(|c| c.set(c.get() + 1));
        RenderScope(g)
    }
}

impl Drop for RenderScope<'_> {
    fn drop(&mut self) {
        let outer = IN_RENDER.with(|c| {
            c.set(c.get().saturating_sub(1));
            c.get() == 0
        });
        if outer {
            self.0.pool(take_retired());
        }
    }
}

/// A device buffer, counted in [`ALLOCATED`] while it exists, and the generation of the device it
/// was made on: a buffer of a device that was replaced ([`crate::device`] recovers from a lost one)
/// is never pooled for, or bound on, the new one.
pub(crate) struct Tracked(wgpu::Buffer, u64);

impl Tracked {
    fn new(b: wgpu::Buffer, generation: u64) -> Tracked {
        let size = b.size();
        let now = ALLOCATED.fetch_add(size, Ordering::Relaxed).saturating_add(size);
        PEAK.fetch_max(now, Ordering::Relaxed);
        Tracked(b, generation)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        ALLOCATED.fetch_sub(self.0.size(), Ordering::Relaxed);
    }
}

#[cfg(test)]
thread_local! {
    /// Polls on this thread panic as wgpu's do once a Vulkan or DX12 device is lost (#1858).
    static SIMULATED_LOST_POLL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    static RETIRED: Retired = const { Retired(std::cell::RefCell::new(Vec::new())) };
}

/// Bytes of device buffers that exist (in use, retired or pooled).
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
/// The most [`ALLOCATED`] has been since [`reset_peak`].
static PEAK: AtomicU64 = AtomicU64::new(0);
/// Errors that reached a device's uncaptured-error handler (diagnostics).
pub(crate) static UNCAPTURED: AtomicU64 = AtomicU64::new(0);
/// The generation the next device gets ([`Gpu::generation`]).
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Measure [`GpuMemory::peak`] again from what exists now.
pub(crate) fn reset_peak() {
    PEAK.store(ALLOCATED.load(Ordering::Relaxed), Ordering::Relaxed);
}
/// Bytes waiting in per-thread retired lists.
static RETIRED_BYTES: AtomicU64 = AtomicU64::new(0);
/// The generations of devices that are lost ([`Gpu::generation`]): wgpu's `poll` panics on one
/// from then on, so [`Gpu::poll`] stops calling it. Other failures that retire a device leave it
/// usable for releasing its buffers, and the device that replaces a lost one is polled as usual.
static LOST_DEVICES: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// Device `generation` is lost (see [`LOST_DEVICES`]).
pub(crate) fn mark_lost(generation: u64) {
    let mut lost = LOST_DEVICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !lost.contains(&generation) {
        // (the latest ones only: a device that is gone isn't asked about again, and the list
        // stays short however many losses there are: review of PR #3506)
        if lost.len() >= MAX_LOST_KEPT {
            lost.remove(0);
        }
        lost.push(generation);
    }
}

/// Lost devices remembered ([`mark_lost`]).
const MAX_LOST_KEPT: usize = 64;

/// Whether device `generation` is lost (0, an unknown device: never).
pub(crate) fn is_lost(generation: u64) -> bool {
    generation != 0 && LOST_DEVICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&generation)
}
/// Bytes in the free pool.
static POOLED: AtomicU64 = AtomicU64::new(0);
/// Most bytes kept in the free pool (see [`crate::set_pool_limit`]).
pub(crate) static POOL_LIMIT: AtomicU64 = AtomicU64::new(DEFAULT_POOL_BYTES);

/// Default for the most bytes kept in the free pool (apps derive it from their memory budget).
pub(crate) const DEFAULT_POOL_BYTES: u64 = 256 << 20;

/// Device buffers held by the renderer (bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemory {
    /// Every buffer that exists (in use + retired + pooled).
    pub allocated: u64,
    /// Of which recycled buffers waiting in the free pool.
    pub pooled: u64,
    /// Of which buffers dropped since their thread's last submit (pooled at its next submit).
    pub retired: u64,
    /// The most that existed at once since [`crate::reset_memory_peak`].
    pub peak: u64,
}

pub(crate) fn memory() -> GpuMemory {
    GpuMemory {
        allocated: ALLOCATED.load(Ordering::Relaxed),
        pooled: POOLED.load(Ordering::Relaxed),
        retired: RETIRED_BYTES.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed),
    }
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    kernels: HashMap<&'static str, Kernel>,
    dummy: wgpu::Buffer,
    /// Recycled buffers (see [`Buf`]).
    free: std::sync::Mutex<Vec<Tracked>>,
    /// Largest storage buffer the device accepts (bytes).
    pub max_buffer: u64,
    /// The device is the window's (eframe's): renders can hand it textures to draw directly.
    pub window: bool,
    /// Which device this is: each one created in the process gets the next number, so buffers and
    /// cached stages of a device that was replaced are told apart.
    pub generation: u64,
    /// The adapter's own memory (bytes), when the system tells ([`crate::vram`]).
    pub vram: Option<u64>,
}

/// WGSL constants shared by every module (generated from the CPU pipeline's values).
fn constants() -> String {
    use lightcraft_pipeline::finish::{GRAIN_HASH, MASK_SUMS, MASK_TERMS, SRGB_LUT_N};
    use lightcraft_pipeline::tone::{CHROMA_N, LUT_MAX_EV, LUT_MIN_EV, LUT_N};
    let mut s = String::new();
    let [to_lms, from_lms, to_lab, from_lab] = lightcraft_color::perceptual::oklab_matrices();
    for (name, m) in [("OK_TO_LMS", to_lms), ("OK_FROM_LMS", from_lms), ("OK_TO_LAB", to_lab), ("OK_FROM_LAB", from_lab)] {
        let rows: Vec<String> = m.iter().map(|r| format!("vec3<f32>({:?}, {:?}, {:?})", r[0], r[1], r[2])).collect();
        s += &format!("const {name} = array<vec3<f32>, 3>({});\n", rows.join(", "));
    }
    s += &format!(
        "const TONE_MIN_EV: f32 = {LUT_MIN_EV:?};\nconst TONE_MAX_EV: f32 = {LUT_MAX_EV:?};\nconst TONE_N: u32 = {LUT_N}u;\nconst CHROMA_N: u32 = {CHROMA_N}u;\n"
    );
    s += &format!("const TONE_MIN_GAIN: f32 = {:?};\n", 2f32.powf(LUT_MIN_EV));
    let b = lightcraft_pipeline::geometry::BLANK;
    s += &format!("const BLANK_R: f32 = {:?};\nconst BLANK_G: f32 = {:?};\nconst BLANK_B: f32 = {:?};\n", b[0], b[1], b[2]);
    s += &format!(
        "const SRGB_N: u32 = {SRGB_LUT_N}u;\nconst CURVE_N: u32 = {}u;\nconst MASK_TERMS: u32 = {MASK_TERMS}u;\nconst MASK_SUMS: u32 = {MASK_SUMS}u;\n",
        crate::params::CURVE_N
    );
    s += &format!("const POINT_WORDS: u32 = {}u;\n", lightcraft_pipeline::colorops::POINT_WORDS);
    s += &format!("const EYE_WORDS: u32 = {}u;\n", lightcraft_pipeline::redeye::EYE_WORDS);
    use lightcraft_pipeline::masks::{AUTO_TOL_CHROMA, AUTO_TOL_EV};
    s += &format!("const AUTO_TOL_EV: f32 = {AUTO_TOL_EV:?};\nconst AUTO_TOL_CHROMA: f32 = {AUTO_TOL_CHROMA:?};\n");
    s += &format!("const SHADOW_TINT_K: f32 = {:?};\n", lightcraft_pipeline::colorops::SHADOW_TINT);
    s += &format!("const GAMUT_WARN: f32 = {:?};\n", lightcraft_pipeline::finish::GAMUT_WARN);
    s += &format!("const NCAND: u32 = {}u;\n", lightcraft_pipeline::spots::candidates(1.0).len());
    use lightcraft_pipeline::output::{PROOF_DEST_WARNING, PROOF_DISPLAY_WARNING};
    for (name, c) in [("PROOF_DEST", PROOF_DEST_WARNING), ("PROOF_DISP", PROOF_DISPLAY_WARNING)] {
        s += &format!("const {name}_R: f32 = {:?};\nconst {name}_G: f32 = {:?};\nconst {name}_B: f32 = {:?};\n", c[0], c[1], c[2]);
    }
    for (i, h) in GRAIN_HASH.iter().enumerate() {
        s += &format!("const GRAIN_H{i}: u32 = {h}u;\n");
    }
    for (name, idx) in crate::params::finish_fields() {
        s += &format!("const F_{name}: u32 = {idx}u;\n");
    }
    s
}

fn module_source(m: &Module, consts: &str) -> String {
    let mut s = String::from(consts);
    s += "@group(0) @binding(0) var<storage, read> P: array<u32>;\n";
    for (i, (name, write, ty)) in m.bindings.iter().enumerate() {
        let access = if *write { "read_write" } else { "read" };
        s += &format!("@group(0) @binding({}) var<storage, {access}> {name}: array<{ty}>;\n", i + 1);
    }
    s += include_str!("wgsl/common.wgsl");
    s += m.src;
    s
}

impl Gpu {
    /// Create a device on the best available adapter (no software fallback), or why there is none.
    pub fn new(backends: wgpu::Backends) -> Result<Gpu, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        // only these drivers are loaded (`crate::backend`: never Vulkan on Windows unless asked)
        desc.backends = backends;
        // DX12 shaders compile with FXC (issue #471)
        desc.backend_options = crate::backend::backend_options();
        // no indirect-call validation pipeline: Intel Metal fails to compile it (#250)
        desc.flags = crate::backend::instance_flags_from_env(desc.flags);
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            log::info!("gpu: only a software adapter ({}), using the CPU pipeline", info.name);
            return Err(format!("software adapter ({}) skipped: the CPU pipeline is faster", info.name));
        }
        let mut limits = adapter.limits();
        if limits.max_storage_buffers_per_shader_stage < 10 {
            log::info!("gpu: {} has too few storage buffers per stage", info.name);
            return Err(format!("{} has too few storage buffers per shader stage ({} < 10)", info.name, limits.max_storage_buffers_per_shader_stage));
        }
        limits = capped(limits, limits_override());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("lightcraft"),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
        Gpu::with_device(device, queue, info, limits, false)
    }

    /// The renderer on the window's device ([`crate::adopt_window_device`]), within its limits
    /// lowered by `LIGHTCRAFT_GPU_LIMITS` as a device of its own would be.
    pub fn adopt(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo) -> Result<Gpu, String> {
        Gpu::adopt_capped(device, queue, info, limits_override())
    }

    /// [`Gpu::adopt`] with the limits override given (`LIGHTCRAFT_GPU_LIMITS`, parsed).
    fn adopt_capped(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo, cap: Option<(u64, u64)>) -> Result<Gpu, String> {
        let limits = capped(device.limits(), cap);
        Gpu::with_device(device, queue, info, limits, true)
    }

    /// The renderer on an existing device: the window's (`window`, see [`crate::adopt_window_device`])
    /// or one [`Gpu::new`] created with `limits`. Builds the kernels (~0.4–2 s: not on the UI thread).
    pub fn with_device(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo, limits: wgpu::Limits, window: bool) -> Result<Gpu, String> {
        if limits.max_storage_buffers_per_shader_stage < 10 {
            return Err(format!("{} has too few storage buffers per shader stage ({} < 10)", info.name, limits.max_storage_buffers_per_shader_stage));
        }
        let started = std::time::Instant::now();
        let kernels = match build_kernels(&device) {
            Ok(kernels) => kernels,
            Err(e) => {
                log::error!("gpu: kernels failed to build on {}: {e}", info.name);
                return Err(format!("{}: kernels failed to build ({e})", info.name));
            }
        };
        let build_ms = started.elapsed().as_millis();
        // Only a device the renderer keeps gets these: a window's device declined above keeps the
        // window's own handler, so a later error of the window's can't stop the renderer's device
        // (review of PR #1065).
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        // Errors outside a render's error scopes (see `ErrorScopes`; scopes are per thread, so a
        // render's own errors never land here). On the window's device they are the window's (a
        // surface reconfigured while the GPU was busy: "Failed to wait for GPU to come idle"):
        // logged, and the renders, which check their own errors, go on. On a device of our own an
        // internal error means the device is in trouble: it is replaced.
        device.on_uncaptured_error(std::sync::Arc::new(move |e| {
            log::error!("gpu: {e}");
            UNCAPTURED.fetch_add(1, Ordering::Relaxed);
            if !window && matches!(e, wgpu::Error::Internal { .. }) {
                crate::mark_broken(generation, &format!("device error: {e}"));
            }
        }));
        // wgpu reports a lost device (driver reset, GPU hang, watchdog) only here — never as an
        // error — so without this callback a lost device went unnoticed. (A device we dropped
        // after replacing it reports `Destroyed`: not a failure.)
        device.set_device_lost_callback(move |reason, msg| {
            if reason == wgpu::DeviceLostReason::Destroyed {
                return;
            }
            mark_lost(generation);
            log::error!("gpu: device lost ({reason:?}): {msg}");
            crate::mark_broken(generation, &format!("device lost ({reason:?}): {msg}"));
        });
        let dummy = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let max_buffer = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        log::info!(
            "gpu: {} ({:?}), {} kernels in {build_ms} ms, buffers ≤ {} MiB{}",
            info.name,
            info.backend,
            kernels.len(),
            max_buffer >> 20,
            if window { ", the window's device" } else { "" }
        );
        let vram = crate::vram::adapter_memory(&info);
        Ok(Gpu { device, queue, info, kernels, dummy, free: Default::default(), max_buffer, window, generation, vram })
    }

    /// A buffer of `len` 32-bit values with undefined contents (a recycled one when possible).
    ///
    /// A buffer over the device limit fails the render ([`FailKind::Limit`]) instead of raising a
    /// validation error: the caller gets a placeholder and the render is redone on the CPU.
    pub fn buffer(&self, len: usize) -> Buf {
        let size = (len.max(1) * 4) as u64;
        if size > self.limit() {
            self.over_limit(size);
            return Buf { buf: Some(Tracked::new(self.placeholder(), self.generation)), len };
        }
        let recycled = {
            let mut f = self.free.lock().unwrap_or_else(|e| e.into_inner());
            // the smallest pooled buffer that holds `size` with at most 25 % to spare: photos of
            // slightly different sizes reuse each other's buffers instead of piling up new ones
            let b = f
                .iter()
                .enumerate()
                .filter(|(_, b)| b.0.size() >= size && b.0.size() <= size + size / 4)
                .min_by_key(|(_, b)| b.0.size())
                .map(|(i, _)| i)
                .map(|i| f.swap_remove(i));
            if let Some(b) = &b {
                POOLED.fetch_sub(b.0.size(), Ordering::Relaxed);
            }
            b
        };
        let buf = recycled.unwrap_or_else(|| {
            Tracked::new(
                self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                self.generation,
            )
        });
        Buf { buf: Some(buf), len }
    }

    /// Submit command buffers, then make the buffers this thread dropped before reusable. Returns
    /// the submission, to wait for it alone ([`Gpu::finish_and_read`]).
    pub fn submit(&self, cmds: impl IntoIterator<Item = wgpu::CommandBuffer>) -> wgpu::SubmissionIndex {
        let index = self.queue.submit(cmds);
        let retired = take_retired();
        self.pool(retired);
        index
    }

    /// Put reusable buffers into the free pool (trimmed to its limit). Buffers of another device
    /// (one this replaced) are freed instead.
    fn pool(&self, bufs: impl IntoIterator<Item = Tracked>) {
        let mut added = false;
        {
            let mut f = self.free.lock().unwrap_or_else(|e| e.into_inner());
            for b in bufs.into_iter().filter(|b| b.1 == self.generation) {
                POOLED.fetch_add(b.0.size(), Ordering::Relaxed);
                f.push(b);
                added = true;
            }
        }
        if added {
            self.trim(POOL_LIMIT.load(Ordering::Relaxed));
        }
    }

    /// Free pooled buffers (oldest first) until at most `keep` bytes stay pooled.
    pub fn trim(&self, keep: u64) {
        let mut f = self.free.lock().unwrap_or_else(|e| e.into_inner());
        let mut total: u64 = f.iter().map(|b| b.0.size()).sum();
        let mut dropped = Vec::new();
        while total > keep && !f.is_empty() {
            let b = f.remove(0);
            total -= b.0.size();
            POOLED.fetch_sub(b.0.size(), Ordering::Relaxed);
            dropped.push(b);
        }
        drop(f);
        if !dropped.is_empty() {
            drop(dropped);
            // let wgpu release the memory now rather than at its next maintenance
            let _ = self.poll(wgpu::PollType::Poll);
        }
    }

    /// Process finished GPU work: `device.poll`, with its error as text. Never panics (#1858): once a
    /// device is lost, wgpu 30 panics in `poll` rather than returning an error (a device error is
    /// not a `PollError`), and the buffer pool polls from destructors ([`Gpu::trim`]), where a
    /// second panic while a failed render unwinds aborts the process. A lost device is not polled
    /// again; a poll that panics counts as one and stops the GPU.
    pub(crate) fn poll(&self, t: wgpu::PollType) -> Result<wgpu::PollStatus, String> {
        poll_device(&self.device, self.generation, t)
    }

    /// Upload 32-bit values.
    pub fn upload<T: bytemuck::Pod>(&self, data: &[T]) -> Buf {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        let len = bytes.len() / 4;
        if bytes.is_empty() {
            return self.buffer(1);
        }
        if bytes.len() as u64 > self.limit() {
            self.over_limit(bytes.len() as u64);
            return Buf { buf: Some(Tracked::new(self.placeholder(), self.generation)), len };
        }
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        if let Some(size) = wgpu::BufferSize::new(bytes.len() as u64).filter(|s| s.get() % wgpu::COPY_BUFFER_ALIGNMENT == 0) {
            // through the queue: the bytes are copied once, into a staging buffer the next submission copies from, in
            // parallel parts (`create_buffer_init` zeroes a staging buffer first and copies over that, two passes on
            // one thread over a full-size photo's ~290 MB)
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: size.get(), usage, mapped_at_creation: false });
            if let Some(mut view) = self.queue.write_buffer_with(&buf, 0, size) {
                const PART: usize = 1 << 20;
                {
                    let (whole, mut tail) = view.slice(..).into_chunks::<PART>();
                    let (src, src_tail) = bytes.as_chunks::<PART>();
                    let mut parts: Vec<_> = whole.into_iter().zip(src).map(Some).collect();
                    lightcraft_raster::par_rows(&mut parts, 1, |_, part| {
                        for (d, s) in part.iter_mut().filter_map(Option::take) {
                            wgpu::WriteOnly::<[u8]>::from(d).copy_from_slice(s);
                        }
                    });
                    tail.copy_from_slice(src_tail);
                }
                // (dropping the view schedules the copy)
                drop(view);
            } else {
                // recorded now, not only when the error scope pops: the zeroed buffer must not be
                // shared before then (`render::shared_source`)
                fail(FailKind::OutOfMemory, "out of device memory: the upload's staging buffer couldn't be allocated".into());
            }
            // no view: the buffer couldn't be allocated (out of device memory). The error scope has recorded why and
            // the render fails over to the CPU; allocating it again would only fail again (issue #1764)
            return Buf { buf: Some(Tracked::new(buf, self.generation)), len };
        }
        let buf = self.init_buffer(bytes, usage);
        Buf { buf: Some(Tracked::new(buf, self.generation)), len }
    }

    /// A buffer holding `contents` (zero-padded to the copy alignment), written through the queue. Not
    /// `create_buffer_init`: it maps the new buffer, and mapping one that couldn't be allocated (out of device
    /// memory) panics inside wgpu instead of raising an error the render's scopes catch (issue #1764).
    fn init_buffer(&self, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
        let size = contents.len().max(align).next_multiple_of(align);
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: size as u64,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if size == contents.len() {
            self.queue.write_buffer(&buf, 0, contents);
        } else {
            let mut padded = contents.to_vec();
            padded.resize(size, 0);
            self.queue.write_buffer(&buf, 0, &padded);
        }
        buf
    }

    fn over_limit(&self, size: u64) {
        fail(
            FailKind::Limit,
            format!("a {} MiB buffer exceeds the device's {} MiB storage-buffer limit", size.div_ceil(1 << 20), self.limit() >> 20),
        );
    }

    /// Stands in for a buffer that could not be created (the render has failed already).
    fn placeholder(&self) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("placeholder"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Whether a buffer of `len` 32-bit values fits the device limits.
    pub fn fits(&self, len: usize) -> bool {
        (len as u64) * 4 <= self.limit()
    }

    /// Largest buffer a render may use (bytes): the device limit, or a lower one set for the
    /// render on this thread ([`LimitOverride`]).
    pub fn limit(&self) -> u64 {
        LIMIT.try_with(|l| l.get()).ok().flatten().map_or(self.max_buffer, |l| l.min(self.max_buffer))
    }

    pub fn encoder(&self) -> wgpu::CommandEncoder {
        self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None })
    }

    /// Record kernel `name` with parameters `p` over buffers `bufs` (None = unused binding).
    pub fn run(&self, enc: &mut wgpu::CommandEncoder, name: &str, p: &[u32], bufs: &[Option<&Buf>], groups: [u32; 3]) {
        if failed() {
            return; // the render is redone on the CPU: don't spend the GPU on it
        }
        let Some(k) = self.kernels.get(name) else {
            // a kernel name typo: fail this render over to the CPU instead of crashing
            fail(FailKind::Fatal, format!("unknown kernel {name}"));
            return;
        };
        assert_eq!(bufs.len(), k.nbuf, "{name}: binding count");
        let params = self.init_buffer(bytemuck::cast_slice(if p.is_empty() { &[0u32] } else { p }), wgpu::BufferUsages::STORAGE);
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }];
        for (i, b) in bufs.iter().enumerate() {
            let buf = b.map(|b| b.raw()).unwrap_or(&self.dummy);
            entries.push(wgpu::BindGroupEntry { binding: i as u32 + 1, resource: buf.as_entire_binding() });
        }
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &k.layout, entries: &entries });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(name), timestamp_writes: None });
        pass.set_pipeline(&k.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
    }

    /// Submit `enc`, then read back the first `len` 32-bit values of `src`.
    ///
    /// A failed readback (device lost, map error, timeout) fails the render ([`FailKind::Fatal`])
    /// and returns zeros, which the caller discards; after an earlier failure in the same render
    /// nothing more is submitted.
    pub fn finish_and_read<T: bytemuck::Pod>(&self, mut enc: wgpu::CommandEncoder, src: &Buf, len: usize) -> Vec<T> {
        assert!(len <= src.len, "readback of {len} values from a buffer of {}", src.len);
        let zeros = || vec![<T as bytemuck::Zeroable>::zeroed(); len];
        if failed() {
            return zeros();
        }
        let size = (len * 4) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: size.max(4),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(src.raw(), 0, &staging, 0, size);
        let index = self.submit([enc.finish()]);
        let slice = staging.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let failed = |why: String| {
            fail(FailKind::Fatal, format!("readback failed: {why}"));
            zeros()
        };
        // A lost device makes the wait fail ([`Gpu::poll`] turns wgpu's panic into the error) and
        // the map callback never run: checking both keeps this from blocking forever (or handing
        // back an unwritten buffer as the image). The wait is for this render's own submission:
        // waiting for the whole queue (every other render's work too) timed out when several
        // exports shared the GPU, and turned the GPU off.
        match poll_device_raw(&self.device, self.generation, wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(READBACK_TIMEOUT) })
        {
            Ok(Ok(_)) => {}
            // the GPU is busy, not broken: this render goes to the CPU, the GPU stays in use
            Ok(Err(wgpu::PollError::Timeout)) => {
                fail(FailKind::Busy, format!("the GPU did not finish within {} s (busy)", READBACK_TIMEOUT.as_secs()));
                return zeros();
            }
            Ok(Err(e)) => return failed(format!("{e}")),
            Err(e) => return failed(e),
        }
        // another thread's poll may be the one that runs the callback: allow it a moment
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return failed(format!("{e}")),
            Err(e) => return failed(format!("map callback: {e}")),
        }
        let data = match slice.get_mapped_range() {
            Ok(d) => d,
            Err(e) => return failed(format!("{e:?}")),
        };
        // copied out in parallel parts: one thread reads mapped memory slowly (~2 GB/s, so ~45 ms for a 24 MP
        // export's pixels), and the new vector's pages are first touched there too
        let src: &[u8] = &data;
        // (`len` counts 32-bit words: as many `T` as they fill)
        let mut out = vec![<T as bytemuck::Zeroable>::zeroed(); src.len() / std::mem::size_of::<T>().max(1)];
        const PART: usize = 1 << 20;
        lightcraft_raster::par_rows(bytemuck::cast_slice_mut::<T, u8>(&mut out), PART, |k, part| {
            if let Some(s) = src.get(k * PART..k * PART + part.len()) {
                part.copy_from_slice(s);
            }
        });
        drop(data);
        staging.unmap();
        out
    }
}

/// Longest wait for a render's GPU work: beyond it the render fails over to the CPU (drivers
/// normally reset a hung GPU much sooner).
const READBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// `LIGHTCRAFT_GPU_LIMITS`: lower the device limits, e.g. to reproduce a smaller adapter.
/// `webgpu` / `downlevel` = 128 MiB storage bindings and 256 MiB buffers (the WebGPU defaults),
/// `<n>` = n MiB storage bindings and 2n MiB buffers. Returns (binding bytes, buffer bytes).
fn limits_override() -> Option<(u64, u64)> {
    parse_limits(&std::env::var("LIGHTCRAFT_GPU_LIMITS").ok()?)
}

/// `limits` lowered to `cap` ([`limits_override`]: storage binding bytes, buffer bytes).
fn capped(mut limits: wgpu::Limits, cap: Option<(u64, u64)>) -> wgpu::Limits {
    if let Some((binding, buffer)) = cap {
        limits.max_storage_buffer_binding_size = limits.max_storage_buffer_binding_size.min(binding);
        limits.max_buffer_size = limits.max_buffer_size.min(buffer);
        log::info!("gpu: LIGHTCRAFT_GPU_LIMITS: storage bindings ≤ {} MiB, buffers ≤ {} MiB", binding >> 20, buffer >> 20);
    }
    limits
}

/// `device.poll` that never panics: see [`Gpu::poll`]. [`read_texture`] uses it too: its device
/// (the window's, when adopted) shares the device-lost record. `generation`: the device's
/// ([`Gpu::generation`]; 0 when unknown).
fn poll_device(device: &wgpu::Device, generation: u64, t: wgpu::PollType) -> Result<wgpu::PollStatus, String> {
    poll_device_raw(device, generation, t)?.map_err(|e| e.to_string())
}

/// [`poll_device`] with wgpu's own error (a timeout is not a failure of the device): `Err` when the
/// device is lost.
fn poll_device_raw(device: &wgpu::Device, generation: u64, t: wgpu::PollType) -> Result<Result<wgpu::PollStatus, wgpu::PollError>, String> {
    if is_lost(generation) {
        return Err("the device is lost".into());
    }
    let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        if SIMULATED_LOST_POLL.with(std::cell::Cell::get) {
            panic!(
                "Error in Device::poll: Validation Error

Caused by:
  Parent device is lost (simulated)"
            );
        }
        device.poll(t)
    }));
    match polled {
        Ok(r) => Ok(r),
        Err(_) => {
            mark_lost(generation);
            crate::mark_broken(generation, "device lost: Device::poll failed");
            Err("the device is lost (Device::poll failed)".into())
        }
    }
}

/// See [`crate::read_texture`].
pub(crate) fn read_texture(
    device: &wgpu::Device,
    generation: u64,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: usize,
    height: usize,
) -> Result<lightcraft_raster::Rgba8, String> {
    if is_lost(generation) {
        return Err("the device is lost".into());
    }
    if width == 0 || height == 0 {
        return Err("an empty texture".into());
    }
    let (Ok(w32), Ok(h32)) = (u32::try_from(width), u32::try_from(height)) else { return Err("texture too large".into()) };
    if w32 > texture.width() || h32 > texture.height() || texture.format() != wgpu::TextureFormat::Rgba8Unorm {
        return Err("not an 8-bit view texture of that size".into());
    }
    // texture copies take rows of a multiple of 256 bytes
    let stride = width.checked_mul(4).ok_or("texture too large")?.next_multiple_of(256);
    let size = stride.checked_mul(height).ok_or("texture too large")?;
    let (Ok(stride32), Ok(size64)) = (u32::try_from(stride), u64::try_from(size)) else { return Err("texture too large".into()) };
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("view readback"),
        size: size64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("view readback") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride32), rows_per_image: Some(h32) },
        },
        wgpu::Extent3d { width: w32, height: h32, depth_or_array_layers: 1 },
    );
    let copied = queue.submit([enc.finish()]);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("texture copy failed: {e}"));
    }
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let wait = std::time::Duration::from_secs(10);
    // (the copy's own submission, not whatever else the device has queued: review of PR #3530)
    poll_device(device, generation, wgpu::PollType::Wait { submission_index: Some(copied), timeout: Some(wait) })?;
    match rx.recv_timeout(wait) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("{e}")),
        Err(e) => return Err(format!("map callback: {e}")),
    }
    let bytes = slice.get_mapped_range().map_err(|e| format!("{e:?}"))?;
    let mut data = Vec::with_capacity(width * height);
    for row in bytes.chunks(stride).take(height) {
        let row = row.get(..width * 4).ok_or("short readback row")?;
        data.extend(row.as_chunks::<4>().0.iter().copied());
    }
    drop(bytes);
    staging.unmap();
    if data.len() != width * height {
        return Err("incomplete readback".into());
    }
    Ok(lightcraft_raster::Rgba8 { width, height, data })
}

fn parse_limits(v: &str) -> Option<(u64, u64)> {
    let v = v.trim().to_ascii_lowercase();
    match v.as_str() {
        "" => None,
        "webgpu" | "downlevel" | "default" => {
            let d = wgpu::Limits::default();
            Some((d.max_storage_buffer_binding_size, d.max_buffer_size))
        }
        n => {
            let mb: u64 = n.trim_end_matches("mib").trim_end_matches("mb").trim().parse().ok()?;
            let b = mb.max(1) << 20;
            Some((b, 2 * b))
        }
    }
}

/// How a GPU render failed (it is then redone on the CPU).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailKind {
    /// A buffer over the device limits: this render only.
    Limit,
    /// The device ran out of memory: this render only (the buffer pool is emptied).
    OutOfMemory,
    /// The image came back entirely black: redone on the CPU in case the GPU dropped work.
    Blank,
    /// The GPU took too long (busy with other work): this render only.
    Busy,
    /// Device error or loss, readback failure, incomplete image: the device is replaced
    /// ([`crate::device`] recovers), the render redone on the CPU.
    Fatal,
}

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub kind: FailKind,
    pub reason: String,
}

thread_local! {
    /// A lower buffer limit for the render on this thread (fault injection in tests).
    static LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Lowers [`Gpu::limit`] on this thread while it lives.
pub(crate) struct LimitOverride;

impl LimitOverride {
    pub fn new(bytes: u64) -> LimitOverride {
        LIMIT.with(|l| l.set(Some(bytes)));
        LimitOverride
    }
}

impl Drop for LimitOverride {
    fn drop(&mut self) {
        let _ = LIMIT.try_with(|l| l.set(None));
    }
}

thread_local! {
    /// The first failure of the render running on this thread.
    static FAILURE: std::cell::RefCell<Option<Failure>> = const { std::cell::RefCell::new(None) };
}

/// Record a failure of the render running on this thread (the first one is kept).
pub(crate) fn fail(kind: FailKind, reason: String) {
    let _ = FAILURE.try_with(|f| {
        let mut f = f.borrow_mut();
        if f.is_none() {
            *f = Some(Failure { kind, reason });
        }
    });
}

/// Has the render on this thread failed?
pub(crate) fn failed() -> bool {
    FAILURE.try_with(|f| f.borrow().is_some()).unwrap_or(false)
}

/// The render's failure, clearing it.
pub(crate) fn take_failure() -> Option<Failure> {
    FAILURE.try_with(|f| f.borrow_mut().take()).ok().flatten()
}

/// wgpu error scopes around a render: out-of-memory, validation and internal errors raised by
/// this thread's calls are captured (instead of reaching the uncaptured-error handler) and fail
/// the render.
pub(crate) struct ErrorScopes([wgpu::ErrorScopeGuard; 3]);

impl ErrorScopes {
    pub fn push(g: &Gpu) -> ErrorScopes {
        ErrorScopes([
            g.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            g.device.push_error_scope(wgpu::ErrorFilter::Validation),
            g.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ])
    }

    /// Pop the scopes: the most telling error captured (out of memory first — an allocation
    /// failure makes later commands invalid too — then internal, then validation).
    pub fn pop(self) -> Option<Failure> {
        let [oom, validation, internal] = self.0;
        // scopes pop in reverse order of their creation
        let internal = pollster::block_on(internal.pop());
        let validation = pollster::block_on(validation.pop());
        let oom = pollster::block_on(oom.pop());
        if let Some(e) = oom {
            return Some(Failure { kind: FailKind::OutOfMemory, reason: format!("out of device memory: {e}") });
        }
        internal.or(validation).map(|e| Failure { kind: FailKind::Fatal, reason: format!("device error: {e}") })
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

/// Workgroups covering a `w × h` grid with `wg`-sized groups.
pub fn groups2(w: usize, h: usize, wg: [usize; 2]) -> [u32; 3] {
    [w.div_ceil(wg[0]).max(1) as u32, h.div_ceil(wg[1]).max(1) as u32, 1]
}

/// Workgroups for a 1-D kernel of `n` threads (256 per group) on a 2-D grid (see `lin_index`).
pub fn groups1(n: usize) -> [u32; 3] {
    let g = n.div_ceil(256).max(1);
    let x = g.min(32768);
    [x as u32, g.div_ceil(x) as u32, 1]
}

#[cfg(test)]
mod tests {
    /// Every kernel module, as it is handed to the device (the generated constants, the bindings, `common.wgsl`, the
    /// module), parses and validates without a GPU.
    #[test]
    fn every_kernel_module_validates_without_a_gpu() {
        let consts = super::constants();
        for m in super::MODULES {
            let name = m.entries.first().copied().unwrap_or("?");
            assert_eq!(crate::validate_wgsl(name, &super::module_source(m, &consts)), Ok(()));
        }
    }

    #[test]
    fn limits_override_parses() {
        assert_eq!(super::parse_limits("webgpu"), Some((128 << 20, 256 << 20)));
        assert_eq!(super::parse_limits(" Downlevel "), Some((128 << 20, 256 << 20)));
        assert_eq!(super::parse_limits("64"), Some((64 << 20, 128 << 20)));
        assert_eq!(super::parse_limits("512MiB"), Some((512 << 20, 1024 << 20)));
        assert_eq!(super::parse_limits(""), None);
        assert_eq!(super::parse_limits("lots"), None);
    }

    /// `LIGHTCRAFT_GPU_LIMITS` caps the window's device as it caps a device of its own (review of
    /// PR #1065: adopting the window's device skipped it, so a 1024 × 1024 RGB float buffer — 12 MiB —
    /// was taken under an 8 MiB cap).
    #[test]
    fn the_limits_override_caps_an_adopted_device() {
        let big = wgpu::Limits { max_storage_buffer_binding_size: 1 << 30, max_buffer_size: 1 << 31, ..wgpu::Limits::default() };
        let c = super::capped(big.clone(), super::parse_limits("8"));
        assert_eq!((c.max_storage_buffer_binding_size, c.max_buffer_size), (8 << 20, 16 << 20));
        let c = super::capped(big.clone(), None);
        assert_eq!((c.max_storage_buffer_binding_size, c.max_buffer_size), (1 << 30, 1 << 31));

        let _lock = crate::DEVICE_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((device, queue, info)) = test_device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let full = device.limits().max_storage_buffer_binding_size.min(device.limits().max_buffer_size);
        if full <= 12 << 20 {
            lightcraft_testkit::skip!(Gpu, "the adapter's buffers are already ≤ 12 MiB");
            return;
        }
        let g = super::Gpu::adopt_capped(device, queue, info, super::parse_limits("8")).expect("kernels build");
        assert!(g.window);
        assert_eq!(g.limit(), 8 << 20, "the cap applies to the adopted device");
        assert!(1024 * 1024 * 3 * 4 > g.limit(), "a 12 MiB buffer is over it");
    }

    /// A device as eframe makes the window's (the adapter's own limits).
    fn test_device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
        let backends = crate::backend::compute_backends()?;
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        desc.backend_options = crate::backend::backend_options();
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("test window"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .ok()?;
        Some((device, queue, info))
    }

    /// Issue #2936: the kernels are built on several threads; every entry point of every module has one.
    #[test]
    fn every_entry_point_has_a_kernel() {
        let Some(g) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let entries: Vec<&str> = super::MODULES.iter().flat_map(|m| m.entries.iter().copied()).collect();
        assert_eq!(g.kernels.len(), entries.len());
        for e in entries {
            assert!(g.kernels.contains_key(e), "no kernel for {e}");
        }
    }

    /// Run `child` (an ignored test in this module) in its own process: it loses the device, which
    /// no other test running beside it may see. Returns its stdout, or `None` without a GPU.
    fn run_child(child: &str) -> Option<String> {
        let exe = std::env::current_exe().unwrap();
        // output goes to files: a pipe nobody reads while we wait could fill and stall the child
        let log = |stream: &str| std::env::temp_dir().join(format!("lc-gpu-{child}-{}.{stream}", std::process::id()));
        let (out_path, err_path) = (log("out"), log("err"));
        let mut proc = std::process::Command::new(exe)
            .args(["--exact", &format!("ctx::tests::{child}"), "--ignored", "--nocapture", "--test-threads=1"])
            .stdout(std::fs::File::create(&out_path).unwrap())
            .stderr(std::fs::File::create(&err_path).unwrap())
            .spawn()
            .unwrap();
        // a driver that stalls must fail the test, not hang CI
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let status = loop {
            if let Some(status) = proc.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = proc.kill();
                let _ = proc.wait();
                panic!("{child}: still running after 120 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        let _ = (std::fs::remove_file(&out_path), std::fs::remove_file(&err_path));
        if stdout.contains("CHILD: no GPU") {
            return None;
        }
        // an abort (SIGABRT, "panic in a destructor during cleanup") has no exit code
        assert!(status.success(), "{child}: {status:?}\n{stdout}\n{stderr}");
        assert!(stdout.contains("CHILD: survived"), "{child}: {stdout}");
        Some(stdout)
    }

    /// An upload reaches the device whole: it is copied into staging memory in parallel parts, the last one
    /// shorter than the others (and a small upload is that last part alone).
    #[test]
    fn uploads_read_back_exactly() {
        let Some(g) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        for len in [1usize, 1000, (3 << 20) / 4 + 777] {
            let data: Vec<u32> = (0..len as u32).map(|i| i.wrapping_mul(2_654_435_761)).collect();
            let b = g.upload(&data);
            let back: Vec<u32> = g.finish_and_read(g.encoder(), &b, len);
            assert!(back == data, "{len} values");
        }
        // read back as wider values (`len` counts 32-bit words), as `read_rgb` does
        let data: Vec<u32> = (0..3 << 20).collect();
        let b = g.upload(&data);
        let back: Vec<[u32; 3]> = g.finish_and_read(g.encoder(), &b, data.len());
        assert!(back.len() == 1 << 20 && back.as_flattened() == data.as_slice());
    }
    /// Issue #1858: once a Vulkan or DX12 device is lost, wgpu 30 panics in `Device::poll` instead of
    /// returning an error. A render's readback panicked; while it unwound, a buffer's way back to
    /// the pool polls again from a destructor, and that second panic aborted the app.
    #[test]
    fn a_poll_on_a_lost_device_neither_panics_nor_aborts() {
        if run_child("lost_poll_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
        }
    }

    /// Review of PR #1065: a view readback (headless screenshot) whose poll finds the device lost
    /// returns an error instead of panicking, stops the GPU, and doesn't poll the device again.
    #[test]
    fn a_view_readback_on_a_lost_device_fails_without_a_panic() {
        if run_child("lost_readback_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
        }
    }

    #[test]
    #[ignore = "run in its own process by a_view_readback_on_a_lost_device_fails_without_a_panic"]
    fn lost_readback_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        let texture = g.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("view"),
            size: wgpu::Extent3d { width: 8, height: 8, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        assert!(super::read_texture(&g.device, g.generation, &g.queue, &texture, 8, 8).is_ok(), "a live device reads back");
        super::SIMULATED_LOST_POLL.with(|c| c.set(true));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::read_texture(&g.device, g.generation, &g.queue, &texture, 8, 8)));
        assert!(matches!(r, Ok(Err(_))), "the lost device fails the readback without a panic");
        assert!(!crate::available(), "the GPU is not used again");
        super::SIMULATED_LOST_POLL.with(|c| c.set(false));
        assert!(super::read_texture(&g.device, g.generation, &g.queue, &texture, 8, 8).is_err(), "a lost device is not polled again");
        println!("CHILD: survived");
    }

    /// Review of PR #3499: a batch render waiting for its previous submission on a lost device
    /// fails the wait without a panic, records the loss, and doesn't poll the device again.
    #[test]
    fn a_batch_wait_on_a_lost_device_fails_without_a_panic() {
        if run_child("lost_throttle_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
        }
    }

    #[test]
    #[ignore = "run in its own process by a_batch_wait_on_a_lost_device_fails_without_a_panic"]
    fn lost_throttle_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        let submitted = || Some(g.queue.submit([g.device.create_command_encoder(&Default::default()).finish()]));
        crate::gate::as_batch(|| {
            assert!(crate::gate::throttle(&g, submitted()), "a live device's wait succeeds");
            super::SIMULATED_LOST_POLL.with(|c| c.set(true));
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::gate::throttle(&g, submitted())));
            super::SIMULATED_LOST_POLL.with(|c| c.set(false));
            assert!(matches!(r, Ok(false)), "the lost device fails the wait without a panic");
            assert!(super::is_lost(g.generation), "the loss is recorded");
            assert!(!crate::gate::throttle(&g, submitted()), "a lost device is not polled again");
        });
        println!("CHILD: survived");
    }

    /// Review of PR #3565: a device lost before it is put in place (its loss callback ran while
    /// it was being made) isn't used: the loss is recorded and renders go to the CPU.
    #[test]
    fn a_device_lost_while_it_is_made_is_not_used() {
        if run_child("lost_while_made_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
        }
    }

    #[test]
    #[ignore = "run in its own process by a_device_lost_while_it_is_made_is_not_used"]
    fn lost_while_made_child() {
        static NO_GPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        fn made_lost() -> Result<super::Gpu, String> {
            let g = crate::new_device().inspect_err(|_| NO_GPU.store(true, std::sync::atomic::Ordering::Relaxed))?;
            super::mark_lost(g.generation);
            Ok(g)
        }
        let got = crate::slot::get(made_lost);
        if NO_GPU.load(std::sync::atomic::Ordering::Relaxed) {
            println!("CHILD: no GPU");
            return;
        }
        assert!(got.is_none(), "the lost device is not handed out");
        assert!(crate::slot::lost_reason().is_some(), "its loss is recorded");
        // a replacement lost while it was made doesn't say the card is back (review of PR #3532)
        crate::slot::replace(made_lost());
        assert!(crate::slot::lost_reason().is_some(), "the replacement's loss is recorded");
        let kinds: Vec<_> = crate::slot::events_after(0).into_iter().map(|e| e.kind).collect();
        assert!(!kinds.contains(&crate::slot::EventKind::Recovered), "no Recovered notice: {kinds:?}");
        println!("CHILD: survived");
    }

    /// A Metal device really lost (its command buffers exhausted) is never polled again.
    #[test]
    fn a_really_lost_device_is_not_polled_again() {
        if run_child("really_lost_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device, or one that can't be lost on purpose here");
        }
    }

    /// A GPU retired by another failure (a validation error) keeps a live device: its buffers are
    /// still released, which takes a poll.
    #[test]
    fn a_retired_but_live_device_is_still_polled() {
        if run_child("retired_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
        }
    }

    #[test]
    #[ignore = "run in its own process by a_retired_but_live_device_is_still_polled"]
    fn retired_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        crate::mark_broken(g.generation, "device error: simulated validation error");
        assert!(!crate::available());
        drop(g.upload(&[1u32; 1024]));
        g.poll(wgpu::PollType::Poll).unwrap();
        println!("CHILD: survived");
    }

    /// A render that runs out of device memory falls back to the CPU and leaves the GPU in use: the upload
    /// whose buffer couldn't be allocated used to try again through `create_buffer_init`, which panicked
    /// mapping the second failed buffer, and the panic stopped GPU use for the session (issue #1764).
    #[test]
    fn running_out_of_device_memory_falls_back_without_stopping_the_gpu() {
        if run_child("out_of_memory_child").is_none() {
            lightcraft_testkit::skip!(Gpu, "no GPU device, or its memory never ran out");
        }
    }

    #[test]
    #[ignore = "run in its own process by running_out_of_device_memory_falls_back_without_stopping_the_gpu"]
    fn out_of_memory_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        // hold device memory until an allocation fails: 1 GiB at a time, then smaller, so little is
        // left; no larger than the device allows in one buffer, which would be a validation error
        let max = g.device.limits().max_buffer_size & !(wgpu::COPY_BUFFER_ALIGNMENT - 1);
        let mut sizes: Vec<u64> = [1u64 << 30, 1 << 28, 1 << 26].into_iter().map(|s| s.min(max)).collect();
        sizes.dedup();
        let mut held = Vec::new();
        let mut full = false;
        for size in sizes {
            for _ in 0..128 {
                let validation = g.device.push_error_scope(wgpu::ErrorFilter::Validation);
                let oom = g.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
                let usage = wgpu::BufferUsages::STORAGE;
                let buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
                let ran_out = pollster::block_on(oom.pop()).is_some();
                if let Some(e) = pollster::block_on(validation.pop()) {
                    panic!("holding {size} bytes of device memory: {e}");
                }
                if ran_out {
                    full = true;
                    break;
                }
                held.push(buf);
            }
        }
        if !full {
            println!("CHILD: no GPU (its memory never ran out)");
            return;
        }
        // a render that needs more than is left: a larger one when the driver still finds room for the first
        let s = lightcraft_develop::DevelopSettings::default();
        let mut ran_out = false;
        for (w, h) in [(4000, 3000), (8000, 6000)] {
            let src = std::sync::Arc::new(lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.18, 0.18, 0.18]; w * h] });
            let req = lightcraft_pipeline::RenderRequest::fit(w, h);
            match crate::try_render(&src, &lightcraft_pipeline::SourceInfo::default(), &s, &req, None) {
                Ok(_) => continue,
                Err(crate::Fallback::Failed(reason)) => assert!(reason.contains("out of device memory"), "{reason}"),
                Err(crate::Fallback::NotTried) => panic!("the GPU wasn't tried"),
            }
            ran_out = true;
            break;
        }
        assert_eq!(crate::unavailable_reason(), None, "the GPU stays in use");
        drop(held);
        if !ran_out {
            println!("CHILD: no GPU (the renders found room anyway)");
            return;
        }
        println!("CHILD: survived");
    }

    /// Empties the buffer pool when dropped, as the UI does (`trim_pool(0)`) and as returning a
    /// buffer to a full pool does.
    struct TrimOnDrop(std::sync::Arc<super::Gpu>);

    impl Drop for TrimOnDrop {
        fn drop(&mut self) {
            self.0.trim(0);
        }
    }

    #[test]
    #[ignore = "run in its own process by a_poll_on_a_lost_device_neither_panics_nor_aborts"]
    fn lost_poll_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        let src = g.upload(&[1u32; 1024]);
        drop(g.upload(&[2u32; 1024])); // pooled: trimming it polls
        super::SIMULATED_LOST_POLL.with(|c| c.set(true));
        // a render (caught as `try_render` catches it) whose readback finds the device lost
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _trim = TrimOnDrop(g.clone());
            let v: Vec<u32> = g.finish_and_read(g.encoder(), &src, 1024);
            assert!(v.iter().all(|x| *x == 0), "a failed readback hands back zeros");
        }));
        assert!(r.is_ok(), "the lost device fails the render without a panic");
        assert!(super::failed(), "the render failed, so it is redone on the CPU");
        assert!(!crate::available(), "the GPU is not used again");
        // a render that panics for another reason still unwinds through a destructor that polls
        drop(g.upload(&[3u32; 1024]));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _trim = TrimOnDrop(g.clone());
            panic!("a render panics (expected in this test)");
        }));
        assert!(r.is_err());
        println!("CHILD: survived");
    }

    #[test]
    #[ignore = "run in its own process by a_really_lost_device_is_not_polled_again"]
    fn really_lost_child() {
        let Some(g) = crate::device().filter(|g| g.info.backend == wgpu::Backend::Metal) else {
            println!("CHILD: no GPU");
            return;
        };
        // wgpu-hal's Metal backend treats a command buffer past its 4096 unsubmitted as a lost device
        let b = g.upload(&[1u32; 16]);
        let held: Vec<wgpu::CommandBuffer> = (0..4200)
            .map(|_| {
                let mut e = g.encoder();
                e.clear_buffer(b.raw(), 0, None);
                e.finish()
            })
            .collect();
        assert!(crate::unavailable_reason().is_some_and(|r| r.contains("device lost")), "{:?}", crate::unavailable_reason());
        let e = g.poll(wgpu::PollType::Poll).unwrap_err();
        assert!(e.contains("lost"), "{e}");
        drop(held);
        println!("CHILD: survived");
    }

    #[test]
    fn dispatches_stay_within_the_workgroup_limit() {
        // 1-D kernels over a 100 MP image and 2-D ones over 16k × 16k stay under 65535 per axis
        assert!(super::groups1(100_000_000 * 3).iter().all(|g| *g <= 65535));
        assert!(super::groups2(16384, 16384, [16, 16]).iter().all(|g| *g <= 65535));
    }
}
