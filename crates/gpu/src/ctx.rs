//! The wgpu device, the compiled kernels, buffers, dispatch and readback.

use crate::init::Progress;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
        entries: &["box_h", "box_h_slide", "box_v", "median_h", "median_v", "min_h", "min_v", "minmax"],
    },
    Module {
        src: include_str!("wgsl/gauss.wgsl"),
        bindings: &[("src", false, "f32"), ("taps", false, "f32"), ("dst", true, "f32")],
        entries: &["gauss_h", "gauss_v"],
    },
    Module {
        src: include_str!("wgsl/resize.wgsl"),
        bindings: &[("src", false, "f32"), ("table", false, "u32"), ("dst", true, "f32")],
        entries: &["resize_h", "resize_v"],
    },
    Module { src: include_str!("wgsl/fit.wgsl"), bindings: &[("src", false, "u32"), ("dst", true, "f32")], entries: &["unpack_rgb8"] },
    Module { src: include_str!("wgsl/fit_pack.wgsl"), bindings: &[("src", false, "f32"), ("dst", true, "u32")], entries: &["pack_rgb8"] },
    Module { src: include_str!("wgsl/hdr_texels.wgsl"), bindings: &[("src", false, "u32"), ("dst", true, "u32")], entries: &["hdr_texels"] },
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
            "clamp0_k",
            "chroma_med",
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
        src: include_str!("wgsl/nrcj.wgsl"),
        bindings: &[("a", false, "f32"), ("b", false, "f32"), ("dst", true, "f32")],
        entries: &[
            "nrcj_yuv",
            "nrcj_uv",
            "nrcj_samples",
            "nrcj_speck",
            "nrcj_pilot",
            "nrcj_links1",
            "nrcj_paths",
            "nrcj_links",
            "nrcj_dh",
            "nrcj_dv",
            "nrcj_recon",
        ],
    },
    Module {
        src: include_str!("wgsl/spot.wgsl"),
        bindings: &[("img", true, "f32"), ("pts", false, "f32"), ("al", true, "f32"), ("sp", true, "f32"), ("df", true, "f32"), ("wt", true, "f32")],
        entries: &["spot_prep", "spot_apply", "spot_score"],
    },
    Module {
        src: include_str!("wgsl/mask.wgsl"),
        bindings: &[("img", false, "f32"), ("log_l", false, "f32"), ("aux", false, "f32"), ("c", true, "f32"), ("alpha", true, "f32")],
        entries: &["shape", "combine", "finalize"],
    },
    Module {
        src: include_str!("wgsl/stroke.wgsl"),
        bindings: &[("src", false, "f32"), ("packed", true, "u32"), ("dst", true, "f32")],
        entries: &["crop", "pack16", "combine16"],
    },
    Module {
        src: include_str!("wgsl/hist.wgsl"),
        bindings: &[("img", false, "u32"), ("cov", false, "u32"), ("hist", true, "atomic<u32>")],
        entries: &["hist_k"],
    },
    Module {
        src: include_str!("wgsl/skin.wgsl"),
        bindings: &[
            ("img", true, "f32"),
            ("cov", false, "f32"),
            ("lab", true, "f32"),
            ("fine", false, "f32"),
            ("mid", false, "f32"),
            ("low", false, "f32"),
        ],
        entries: &["skin_lab", "skin_reduce", "skin_combine"],
    },
    Module {
        src: include_str!("wgsl/present.wgsl"),
        bindings: &[("px", false, "u32"), ("dst", true, "u32"), ("count", true, "atomic<u32>")],
        entries: &["unwritten", "pad_rows"],
    },
    Module {
        src: include_str!("wgsl/geom.wgsl"),
        bindings: &[("src", false, "f32"), ("dst", true, "f32"), ("coverage", false, "u32"), ("field", false, "f32")],
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
///
/// Each module prepared and each kernel built is [`Progress`]: a first start compiles every kernel
/// (30 s on an RTX 4070 SUPER under DX12), longer than the start-up limit, and goes on as long as
/// kernels keep coming.
fn build_kernels(device: &wgpu::Device) -> Result<HashMap<&'static str, Kernel>, String> {
    let progress = Progress::current();
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
            progress.tick();
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
        let work = || build_jobs(device, &jobs, &next, &built, &progress);
        let threads: Vec<_> =
            (0..workers).filter_map(|_| std::thread::Builder::new().name("gpu-kernels".into()).spawn_scoped(s, work).ok()).collect();
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
    progress: &Progress,
) -> Option<String> {
    let scopes = push_scopes(device);
    while let Some(&(p, entry)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&p.pipeline_layout),
            module: &p.module,
            entry_point: Some(entry),
            // Every kernel here writes all of its workgroup memory before reading it (`hist_k` zeroes its
            // bins), so the zeroing wgpu adds by default is not needed; on DX12 it is written out as one
            // assignment per element, and FXC took ~8 s over `hist_k`'s 1281 bins at every launch.
            compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
            cache: None,
        });
        let kernel = Kernel { pipeline, layout: p.layout.clone(), nbuf: p.nbuf };
        built.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(entry, kernel);
        progress.tick();
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
    /// A buffer of `len` words on no device (tests of what holds buffers, without a GPU).
    #[cfg(test)]
    pub(crate) fn detached(len: usize) -> Buf {
        Buf { buf: None, len }
    }

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
        let now = ALLOCATED.fetch_add(b.size(), Ordering::Relaxed).saturating_add(b.size());
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

/// Measure [`GpuMemory::peak`] again from what exists now.
pub(crate) fn reset_peak() {
    PEAK.store(ALLOCATED.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Bytes waiting in per-thread retired lists.
static RETIRED_BYTES: AtomicU64 = AtomicU64::new(0);
/// The generation the next device gets ([`Gpu::generation`]).
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Bytes in the free pool.
static POOLED: AtomicU64 = AtomicU64::new(0);
/// Most bytes kept in the free pool (see [`crate::set_pool_limit`]).
pub(crate) static POOL_LIMIT: AtomicU64 = AtomicU64::new(DEFAULT_POOL_BYTES);

/// Default for the most bytes kept in the free pool (apps derive it from their memory budget).
pub(crate) const DEFAULT_POOL_BYTES: u64 = 256 << 20;

/// The share of a card's own memory its pool may hold when that is more than [`POOL_LIMIT`]: 1/n,
/// 0 for none ([`crate::set_pool_vram_share`]).
pub(crate) static POOL_VRAM_SHARE: AtomicU64 = AtomicU64::new(0);

/// The most bytes a device keeps pooled while it works: `limit` ([`POOL_LIMIT`]), or `1/share` of
/// the card's own memory when that is more (`share` 0: `limit`). An app's limit that follows its RAM
/// budget (2 GiB at most) is small for the 1:1 tiles of a 45 MP photo, whose source buffers are
/// some 720 MiB: the pool freed them as fast as renders made new ones, and on NVIDIA's Vulkan
/// driver each free holds a lock for the whole process for 140-300 ms, which the window's frames
/// waited for (2026-11-10). The idle trim still empties the pool a few seconds after the last render.
pub(crate) fn pool_limit(limit: u64, vram: Option<u64>, share: u64) -> u64 {
    match (vram, share) {
        (Some(v), n) if n > 0 => limit.max(v / n),
        _ => limit,
    }
}

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
    /// An unused binding's placeholder, one per binding slot.
    dummy: Vec<wgpu::Buffer>,
    /// Recycled buffers (see [`Buf`]).
    free: std::sync::Mutex<Vec<Tracked>>,
    /// Recycled staging buffers of uploads and readbacks ([`Staging`]).
    staging: Staging,
    /// Largest storage buffer the device accepts (bytes).
    pub max_buffer: u64,
    /// Which device this is: each one created in the process gets the next number, so buffers and
    /// cached stages of a device that was replaced are told apart.
    pub generation: u64,
    /// The adapter's own memory (bytes), when its backend tells ([`lcc_gpumem::adapter_memory`]).
    pub vram: Option<u64>,
    /// This device is lost: wgpu's `poll` panics from then on, so [`Gpu::poll`] stops calling it.
    /// Other failures leave the device usable for releasing its buffers.
    lost: Arc<AtomicBool>,
    /// The device is the window's (eframe's): renders can hand it textures to draw directly.
    pub window: bool,
}

/// WGSL constants shared by every module (generated from the CPU pipeline's values).
fn constants() -> String {
    use lcc_pipeline::finish::{GRAIN_HASH, MASK_SUMS, MASK_TERMS, MASK_TONE, SRGB_LUT_N};
    use lcc_pipeline::tone::{CHROMA_N, LUT_MAX_EV, LUT_MIN_EV, LUT_N, RGB_CONTRAST_N};
    let mut s = String::new();
    let [to_lms, from_lms, to_lab, from_lab] = lcc_color::perceptual::oklab_matrices();
    for (name, m) in [("OK_TO_LMS", to_lms), ("OK_FROM_LMS", from_lms), ("OK_TO_LAB", to_lab), ("OK_FROM_LAB", from_lab)] {
        let rows: Vec<String> = m.iter().map(|r| format!("vec3<f32>({:?}, {:?}, {:?})", r[0], r[1], r[2])).collect();
        s += &format!("const {name} = array<vec3<f32>, 3>({});\n", rows.join(", "));
    }
    s += &format!(
        "const TONE_MIN_EV: f32 = {LUT_MIN_EV:?};\nconst TONE_MAX_EV: f32 = {LUT_MAX_EV:?};\nconst TONE_N: u32 = {LUT_N}u;\nconst CHROMA_N: u32 = {CHROMA_N}u;\nconst RGB_CONTRAST_N: u32 = {RGB_CONTRAST_N}u;\n"
    );
    s += &format!("const TONE_MIN_GAIN: f32 = {:?};\n", 2f32.powf(LUT_MIN_EV));
    let b = lcc_pipeline::geometry::BLANK;
    s += &format!("const BLANK_R: f32 = {:?};\nconst BLANK_G: f32 = {:?};\nconst BLANK_B: f32 = {:?};\n", b[0], b[1], b[2]);
    s += &format!(
        "const SRGB_N: u32 = {SRGB_LUT_N}u;\nconst CURVE_N: u32 = {}u;\nconst MASK_TERMS: u32 = {MASK_TERMS}u;\nconst MASK_SUMS: u32 = {MASK_SUMS}u;\nconst MASK_TONE: u32 = {MASK_TONE}u;\n",
        crate::params::CURVE_N
    );
    s += &format!("const POINT_WORDS: u32 = {}u;\n", lcc_pipeline::colorops::POINT_WORDS);
    s += &format!("const LOCAL_POINT_WORDS: u32 = {}u;\n", crate::params::LOCAL_POINT_WORDS);
    s += &format!("const LOCAL_POINT_SAMPLES: u32 = {}u;\n", crate::params::LOCAL_POINT_SAMPLES);
    s += &format!("const EYE_WORDS: u32 = {}u;\n", lcc_pipeline::redeye::EYE_WORDS);
    use lcc_pipeline::masks::{AUTO_LRC_CHROMA, AUTO_LRC_EV, AUTO_TOL_CHROMA, AUTO_TOL_EV};
    s += &format!("const AUTO_TOL_EV: f32 = {AUTO_TOL_EV:?};\nconst AUTO_TOL_CHROMA: f32 = {AUTO_TOL_CHROMA:?};\n");
    s += &format!("const NCAND: u32 = {}u;\n", lcc_pipeline::spots::candidates(1.0).len());
    s += &format!(
        "const AUTO_LRC_EV0: f32 = {:?};\nconst AUTO_LRC_EV1: f32 = {:?};\nconst AUTO_LRC_CH0: f32 = {:?};\nconst AUTO_LRC_CH1: f32 = {:?};\n",
        AUTO_LRC_EV.0, AUTO_LRC_EV.1, AUTO_LRC_CHROMA.0, AUTO_LRC_CHROMA.1
    );
    s += &format!("const SHADOW_TINT_K: f32 = {:?};\n", lcc_pipeline::colorops::SHADOW_TINT);
    // value noise of processes V1–V6
    for (i, h) in GRAIN_HASH.iter().enumerate() {
        s += &format!("const GRAIN_VH{i}: u32 = {h}u;\n");
    }
    // the particle grain of process V7 (`lcc_pipeline::grain`)
    {
        use lcc_pipeline::grain::{CHANNEL_SALT, MAX_PER_CELL, MAX_REACH, MAX_SUB, P_MAX, P_MIN};
        for (i, h) in lcc_pipeline::grain::GRAIN_HASH.iter().enumerate() {
            s += &format!("const GRAIN_H{i}: u32 = {h}u;\n");
        }
        s += &format!(
            "const GRAIN_P_MIN: f32 = {P_MIN:?};\nconst GRAIN_P_MAX: f32 = {P_MAX:?};\nconst GRAIN_MAX_PER_CELL: u32 = {MAX_PER_CELL}u;\nconst GRAIN_MAX_SUB: u32 = {MAX_SUB}u;\nconst GRAIN_MAX_REACH: i32 = {MAX_REACH};\n"
        );
        let phases: Vec<String> = lcc_pipeline::grain::PHASES.iter().map(|p| format!("vec2<f32>({:?}, {:?})", p[0], p[1])).collect();
        // a switch, not an array: a private array shares naga's strided array type, which Vulkan's validation layer
        // refuses ("explicit layout in the Private storage class"), and a const array may not be indexed at run time
        s += "fn grain_phase(i: u32) -> vec2<f32> {\n    switch i {\n";
        for (i, ph) in phases.iter().enumerate().take(15) {
            s += &format!("        case {i}u: {{ return {ph}; }}\n");
        }
        s += &format!("        default: {{ return {}; }}\n    }}\n}}\n", phases.get(15).map(String::as_str).unwrap_or("vec2<f32>(0.0, 0.0)"));
        for (name, salt) in ["R", "G", "B"].iter().zip(CHANNEL_SALT) {
            s += &format!("const GRAIN_SALT_{name}: u32 = {salt}u;\n");
        }
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

/// A wgpu instance that loads only `backends`' drivers (`crate::backend`: never Vulkan on Windows
/// unless asked), with the DX12 shader compiler and validation flags the renderer needs.
pub(crate) fn instance(backends: wgpu::Backends) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    // DX12 shaders compile with FXC (issue #471)
    desc.backend_options = crate::backend::backend_options();
    // no indirect-call validation pipeline: Intel Metal fails to compile it (#250)
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    crate::new_instance(desc)
}

/// Staging buffers kept between renders. Every render used to create its own: a buffer for each
/// upload plus the staging copy `queue.write_buffer` makes of it, and one for each readback. With
/// a 45 MP photo those are hundreds of MiB each, and NVIDIA's Vulkan driver holds a lock for the
/// whole process while it allocates or frees one (80-250 ms): the window's swapchain view and
/// submit waited behind it, and drags at 100 % stuttered on Vulkan but not on DX12 (2026-11-10).
#[derive(Default)]
struct Staging {
    /// For uploads ([`Gpu::upload`]), each with the submission that last copied out of it.
    up: std::sync::Mutex<Vec<(wgpu::Buffer, wgpu::SubmissionIndex)>>,
    /// For readbacks ([`Gpu::finish_and_read_parts`]), unmapped.
    down: std::sync::Mutex<Vec<wgpu::Buffer>>,
}

/// At most this many bytes of each kind of staging buffer are kept per device (host memory): two
/// uploads of a 45 MP photo's source (537 MB each) at once fit.
const STAGING_KEEP: u64 = 3 << 29;

/// The index of the smallest buffer in `bufs` (by `size`) that holds `want` bytes with at most
/// 25 % to spare.
fn best_fit<T>(bufs: &[T], want: u64, size: impl Fn(&T) -> u64) -> Option<usize> {
    bufs.iter().enumerate().filter(|(_, b)| size(b) >= want && size(b) <= want.saturating_add(want / 4)).min_by_key(|(_, b)| size(b)).map(|(i, _)| i)
}

/// Keep `buf` in `pool` unless that would hold more than [`STAGING_KEEP`] bytes.
fn keep_staging<T>(pool: &std::sync::Mutex<Vec<T>>, buf: T, size: impl Fn(&T) -> u64) {
    let mut p = pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held: u64 = p.iter().map(&size).sum();
    if held.saturating_add(size(&buf)) <= STAGING_KEEP {
        p.push(buf);
    }
}

impl Gpu {
    /// Create a device on the best available adapter (no software fallback), or why there is none.
    pub fn new(backends: wgpu::Backends) -> Result<Gpu, String> {
        let instance = instance(backends);
        // the card `<PREFIX>_GPU_ADAPTER` names, else the high-performance one
        let adapter = crate::backend::pick_adapter(&instance, backends)?;
        Gpu::on_adapter(&adapter)
    }

    /// Create a device on `adapter` (not a software one), or why it can't be.
    pub fn on_adapter(adapter: &wgpu::Adapter) -> Result<Gpu, String> {
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
            label: Some(lcc_brand::BINARY),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
        let vram = lcc_gpumem::adapter_memory(adapter);
        Gpu::with_device(device, queue, info, limits, vram, false)
    }

    /// The renderer on the window's device ([`crate::adopt_window_device`]), within its limits
    /// lowered by `<PREFIX>_GPU_LIMITS` as a device of its own would be.
    pub fn adopt(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo, vram: Option<u64>) -> Result<Gpu, String> {
        Gpu::adopt_capped(device, queue, info, vram, limits_override())
    }

    /// [`Gpu::adopt`] with the limits override given (`<PREFIX>_GPU_LIMITS`, parsed).
    fn adopt_capped(
        device: wgpu::Device,
        queue: wgpu::Queue,
        info: wgpu::AdapterInfo,
        vram: Option<u64>,
        cap: Option<(u64, u64)>,
    ) -> Result<Gpu, String> {
        let limits = capped(device.limits(), cap);
        Gpu::with_device(device, queue, info, limits, vram, true)
    }

    /// The renderer on an existing device: the window's (`window`, see [`crate::adopt_window_device`])
    /// or one [`Gpu::new`] created with `limits`. Builds the kernels (~0.4–2 s: not on the UI thread).
    pub fn with_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        info: wgpu::AdapterInfo,
        limits: wgpu::Limits,
        vram: Option<u64>,
        window: bool,
    ) -> Result<Gpu, String> {
        if limits.max_storage_buffers_per_shader_stage < 10 {
            return Err(format!("{} has too few storage buffers per shader stage ({} < 10)", info.name, limits.max_storage_buffers_per_shader_stage));
        }
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let lost = Arc::new(AtomicBool::new(false));
        // Errors outside a render's error scopes (see `ErrorScopes`): log, and replace the device,
        // unless it is the window's own surface failing on the window's device (adapted from upstream #4032).
        device.on_uncaptured_error(std::sync::Arc::new(move |e| {
            let text = e.to_string();
            if stops_renderer(window, &text) {
                log::error!("gpu: {text}");
                crate::mark_broken(generation, &format!("device error: {text}"));
            } else {
                log::warn!("gpu: the window's surface: {text}");
            }
        }));
        // wgpu reports a lost device (driver reset, GPU hang, watchdog) only here — never as an
        // error — so without this callback a lost device went unnoticed. (A device dropped after
        // it was replaced reports `Destroyed`: not a failure.)
        let flag = lost.clone();
        device.set_device_lost_callback(move |reason, msg| {
            flag.store(true, Ordering::Relaxed);
            if reason == wgpu::DeviceLostReason::Destroyed {
                return;
            }
            log::error!("gpu: device lost ({reason:?}): {msg}");
            crate::mark_broken(generation, &format!("device lost ({reason:?}): {msg}"));
        });
        let started = std::time::Instant::now();
        let kernels = match build_kernels(&device) {
            Ok(kernels) => kernels,
            Err(e) => {
                log::error!("gpu: kernels failed to build on {}: {e}", info.name);
                return Err(format!("{}: kernels failed to build ({e})", info.name));
            }
        };
        let build_ms = started.elapsed().as_millis();
        // one placeholder per binding slot: a kernel may leave a read-only and a read-write
        // binding unused, and one buffer can't be both in a dispatch
        let nbuf = kernels.values().map(|k| k.nbuf).max().unwrap_or(0);
        let dummy = (0..nbuf)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("dummy"),
                    size: 16,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let max_buffer = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        log::info!(
            "gpu: {} ({:?}), {} kernels in {build_ms} ms, buffers ≤ {} MiB{}",
            info.name,
            info.backend,
            kernels.len(),
            max_buffer >> 20,
            if window { ", the window's device" } else { "" }
        );
        Ok(Gpu {
            device,
            queue,
            info,
            kernels,
            dummy,
            free: Default::default(),
            staging: Staging::default(),
            max_buffer,
            generation,
            vram,
            lost,
            window,
        })
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
        if simulated_oom() {
            fail(FailKind::OutOfMemory, "out of device memory (simulated)".into());
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
        let buf = match recycled {
            Some(b) => b,
            None => {
                let created = self.checked(size, || {
                    self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: None,
                        size,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
                });
                Tracked::new(created.unwrap_or_else(|| self.placeholder()), self.generation)
            }
        };
        Buf { buf: Some(buf), len }
    }

    /// Submit command buffers, then make the buffers this thread dropped before reusable.
    /// Returns the submission, to wait for it alone ([`Gpu::finish_and_read`]).
    pub fn submit(&self, cmds: impl IntoIterator<Item = wgpu::CommandBuffer>) -> wgpu::SubmissionIndex {
        let index = self.queue.submit(cmds);
        let retired = take_retired();
        self.pool(retired);
        index
    }

    /// Put reusable buffers into the free pool (trimmed to its limit).
    fn pool(&self, bufs: impl IntoIterator<Item = Tracked>) {
        let mut added = false;
        {
            let mut f = self.free.lock().unwrap_or_else(|e| e.into_inner());
            // (buffers of a device this one replaced are freed instead)
            for b in bufs.into_iter().filter(|b| b.1 == self.generation) {
                POOLED.fetch_add(b.0.size(), Ordering::Relaxed);
                f.push(b);
                added = true;
            }
        }
        if added {
            self.trim(pool_limit(POOL_LIMIT.load(Ordering::Relaxed), self.vram, POOL_VRAM_SHARE.load(Ordering::Relaxed)));
        }
    }

    /// Free pooled buffers (oldest first) until at most `keep` bytes stay pooled; emptying the pool
    /// (`keep` 0: idle, or the device replaced) frees the staging buffers kept for uploads and
    /// readbacks too ([`Staging`]). (Every submission trims to the pool's limit: those keep them.)
    pub fn trim(&self, keep: u64) {
        let staged = keep == 0 && {
            let up = std::mem::take(&mut *self.staging.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            let down = std::mem::take(&mut *self.staging.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            !up.is_empty() || !down.is_empty()
        };
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
        if !dropped.is_empty() || staged {
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
        if self.lost.load(Ordering::Relaxed) {
            return Err("the device is lost".into());
        }
        let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if SIMULATED_LOST_POLL.with(std::cell::Cell::get) {
                panic!("Error in Device::poll: Validation Error\n\nCaused by:\n  Parent device is lost (simulated)");
            }
            self.device.poll(t)
        }));
        match polled {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => {
                self.lost.store(true, Ordering::Relaxed);
                crate::mark_broken(self.generation, "device lost: Device::poll failed");
                Err("the device is lost (Device::poll failed)".into())
            }
        }
    }

    /// Upload 32-bit values.
    pub fn upload<T: bytemuck::Pod>(&self, data: &[T]) -> Buf {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        let len = bytes.len() / 4;
        // A failed render runs no further GPU work (the render is redone on the CPU): staging the
        // bytes would only queue a copy for a submission nobody needs, so the caller gets the
        // placeholder with the requested length instead.
        if failed() {
            return Buf { buf: Some(Tracked::new(self.placeholder(), self.generation)), len };
        }
        if bytes.is_empty() {
            return self.buffer(1);
        }
        if bytes.len() as u64 > self.limit() {
            self.over_limit(bytes.len() as u64);
            return Buf { buf: Some(Tracked::new(self.placeholder(), self.generation)), len };
        }
        // a recycled buffer when there is one (out of memory: the placeholder, failure recorded)
        let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let mut dst = self.buffer((size / 4) as usize);
        dst.len = len;
        if dst.raw().size() < size {
            return dst;
        }
        // the copy is of whole 4-byte words
        let padded;
        let bytes = if bytes.len() as u64 == size {
            bytes
        } else {
            let mut p = bytes.to_vec();
            p.resize(size as usize, 0);
            padded = p;
            &padded
        };
        if !self.write_staged(dst.raw(), bytes) {
            self.queue.write_buffer(dst.raw(), 0, bytes);
        }
        dst
    }

    /// Copy `bytes` (a multiple of 4) into `dst` through a recycled staging buffer ([`Staging`]),
    /// submitted at once: it runs before any later submission that reads `dst`. `false` when no
    /// staging buffer could be had (the caller writes through the queue instead).
    fn write_staged(&self, dst: &wgpu::Buffer, bytes: &[u8]) -> bool {
        let size = bytes.len() as u64;
        let reused = {
            let mut p = self.staging.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            best_fit(&p, size, |b| b.0.size()).map(|i| p.swap_remove(i))
        };
        let staging = match reused {
            Some((b, last)) => {
                // mapped again once the GPU has copied out of it
                if self.poll(wgpu::PollType::Wait { submission_index: Some(last), timeout: Some(READBACK_TIMEOUT) }).is_err() {
                    return false;
                }
                let (tx, rx) = std::sync::mpsc::channel();
                b.slice(..size).map_async(wgpu::MapMode::Write, move |r| {
                    let _ = tx.send(r);
                });
                let started = std::time::Instant::now();
                loop {
                    if self.poll(wgpu::PollType::Poll).is_err() {
                        return false;
                    }
                    match rx.recv_timeout(std::time::Duration::from_millis(1)) {
                        Ok(Ok(())) => break,
                        Ok(Err(_)) => return false,
                        Err(_) if started.elapsed() < MAP_WAIT => {}
                        Err(_) => return false,
                    }
                }
                b
            }
            None => {
                let created = self.checked(size, || {
                    self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("upload"),
                        size,
                        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: true,
                    })
                });
                let Some(b) = created else { return false };
                b
            }
        };
        match staging.slice(..size).get_mapped_range_mut() {
            Ok(mut view) => view.copy_from_slice(bytes),
            Err(_) => return false,
        }
        staging.unmap();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("upload") });
        enc.copy_buffer_to_buffer(&staging, 0, dst, 0, size);
        // straight to the queue, not `Gpu::submit`: that also pools the buffers this thread dropped
        // since its last submission, which an encoder not yet submitted may still use
        let index = self.queue.submit([enc.finish()]);
        keep_staging(&self.staging.up, (staging, index), |b| b.0.size());
        true
    }

    /// Create a buffer, or record [`FailKind::OutOfMemory`] and return `None` when the device has
    /// no memory for it. wgpu hands back an invalid buffer on out of memory; caught here, it never
    /// reaches the free pool or the shared source, where another thread's render would pick it up
    /// and fail with "Buffer is invalid", a validation error that retired the GPU (upstream #1764).
    fn checked(&self, size: u64, create: impl FnOnce() -> wgpu::Buffer) -> Option<wgpu::Buffer> {
        if simulated_oom() {
            fail(FailKind::OutOfMemory, "out of device memory (simulated)".into());
            return None;
        }
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let buf = create();
        match pollster::block_on(scope.pop()) {
            None => Some(buf),
            Some(e) => {
                fail(FailKind::OutOfMemory, format!("out of device memory for a {} MiB buffer: {e}", size.div_ceil(1 << 20)));
                None
            }
        }
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
        if bufs.len() != k.nbuf {
            // a caller/kernel mismatch would bind the wrong buffers: fail this render over to the
            // CPU instead of crashing
            fail(FailKind::Fatal, format!("{name}: binding count {} != {}", bufs.len(), k.nbuf));
            return;
        }
        let params = crate::upload::buffer_init(
            &self.device,
            &self.queue,
            None,
            bytemuck::cast_slice(if p.is_empty() { &[0u32] } else { p }),
            wgpu::BufferUsages::STORAGE,
        );
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }];
        for (i, b) in bufs.iter().enumerate() {
            let Some(buf) = b.map(|b| b.raw()).or_else(|| self.dummy.get(i)) else {
                fail(FailKind::Fatal, format!("{name}: no placeholder for binding {i}"));
                return;
            };
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
    pub fn finish_and_read<T: bytemuck::Pod>(&self, enc: wgpu::CommandEncoder, src: &Buf, len: usize) -> Vec<T> {
        self.finish_and_read_parts(enc, src, &[len]).pop().unwrap_or_default()
    }

    /// [`Self::finish_and_read`] of consecutive parts of `src` (the first `lens[0]` values, the
    /// `lens[1]` after them, …), each into its own vector, with one submission and one mapping.
    pub fn finish_and_read_parts<T: bytemuck::Pod>(&self, mut enc: wgpu::CommandEncoder, src: &Buf, lens: &[usize]) -> Vec<Vec<T>> {
        let len: usize = lens.iter().sum();
        // (lengths are in 32-bit values, as for `finish_and_read`; a part holds as many `T` as fit)
        let count = |l: usize| l * 4 / std::mem::size_of::<T>().max(1);
        let zeros = || lens.iter().map(|&l| vec![<T as bytemuck::Zeroable>::zeroed(); count(l)]).collect::<Vec<_>>();
        if failed() {
            return zeros();
        }
        // a zero-length readback would issue a `copy_buffer_to_buffer` of size 0, which wgpu
        // rejects as an invalid copy: submit the (possibly staged) work untouched and return
        // zeros instead
        if len == 0 {
            self.submit([enc.finish()]);
            return zeros();
        }
        if len > src.len {
            // a caller bug would read past the buffer: fail this render over to the CPU
            fail(FailKind::Fatal, format!("readback of {len} values from a buffer of {}", src.len));
            return zeros();
        }
        let size = (len * 4) as u64;
        // a recycled staging buffer ([`Staging`]); out of memory here is the render's failure, not
        // a broken readback
        let reused = {
            let mut p = self.staging.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            best_fit(&p, size, wgpu::Buffer::size).map(|i| p.swap_remove(i))
        };
        let staging = match reused {
            Some(b) => b,
            None => {
                let Some(b) = self.checked(size, || {
                    self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("readback"),
                        size: size.max(4),
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
                }) else {
                    return zeros();
                };
                b
            }
        };
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
        // back an unwritten buffer as the image).
        // The wait is for this render's own submission: waiting for the whole queue (every other
        // render's work too) timed out when several exports shared the GPU, and turned it off.
        match self.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(READBACK_TIMEOUT) }) {
            Ok(_) => {}
            // the GPU is busy, not broken: this render goes to the CPU, the GPU stays in use
            Err(e) if e == wgpu::PollError::Timeout.to_string() => {
                fail(FailKind::Busy, format!("the GPU did not finish within {} s (busy)", READBACK_TIMEOUT.as_secs()));
                return zeros();
            }
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
        let mut at = 0;
        let out = lens
            .iter()
            .map(|&l| {
                let part = data.get(at..at + 4 * l).and_then(|b| bytemuck::try_cast_slice::<u8, T>(b).ok()).map(<[T]>::to_vec);
                at += 4 * l;
                part.unwrap_or_else(|| vec![<T as bytemuck::Zeroable>::zeroed(); count(l)])
            })
            .collect();
        drop(data);
        staging.unmap();
        keep_staging(&self.staging.down, staging, wgpu::Buffer::size);
        out
    }
}

/// Longest wait for a render's GPU work: beyond it the render fails over to the CPU (drivers
/// normally reset a hung GPU much sooner).
const READBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Longest wait for a recycled upload buffer to be mapped again once the GPU is done with it.
const MAP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// `{ENV_PREFIX}_GPU_LIMITS`: lower the device limits, e.g. to reproduce a smaller adapter.
/// `webgpu` / `downlevel` = 128 MiB storage bindings and 256 MiB buffers (the WebGPU defaults),
/// `<n>` = n MiB storage bindings and 2n MiB buffers. Returns (binding bytes, buffer bytes).
fn limits_override() -> Option<(u64, u64)> {
    parse_limits(&lcc_brand::env("GPU_LIMITS")?)
}

/// `limits` lowered to `cap` ([`limits_override`]: storage binding bytes, buffer bytes).
fn capped(mut limits: wgpu::Limits, cap: Option<(u64, u64)>) -> wgpu::Limits {
    if let Some((binding, buffer)) = cap {
        limits.max_storage_buffer_binding_size = limits.max_storage_buffer_binding_size.min(binding);
        limits.max_buffer_size = limits.max_buffer_size.min(buffer);
        log::info!("gpu: {}: storage bindings ≤ {} MiB, buffers ≤ {} MiB", lcc_brand::env_var("GPU_LIMITS"), binding >> 20, buffer >> 20);
    }
    limits
}

/// See [`crate::read_texture`].
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: usize,
    height: usize,
) -> Result<lcc_raster::Rgba8, String> {
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
    queue.submit([enc.finish()]);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("texture copy failed: {e}"));
    }
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // (the render path's wait: queued work ahead of the copy can take that long too)
    let wait = READBACK_TIMEOUT;
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(wait) }).map_err(|e| format!("{e}"))?;
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
    Ok(lcc_raster::Rgba8 { width, height, data })
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
            let b = mb.max(1).checked_mul(1 << 20)?;
            Some((b, b.checked_mul(2)?))
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
    /// ([`crate::device`] recovers after a pause), the render redone on the CPU.
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
    /// Every allocation of the render on this thread finds the device out of memory (tests).
    static SIMULATED_OOM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn simulated_oom() -> bool {
    SIMULATED_OOM.try_with(|c| c.get()).unwrap_or(false)
}

/// Makes allocations on this thread run out of device memory while it lives ([`crate::Fault::OutOfMemory`]).
pub(crate) struct SimulatedOom;

impl SimulatedOom {
    pub fn new() -> SimulatedOom {
        SIMULATED_OOM.with(|c| c.set(true));
        SimulatedOom
    }
}

impl Drop for SimulatedOom {
    fn drop(&mut self) {
        let _ = SIMULATED_OOM.try_with(|c| c.set(false));
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

/// Whether an uncaptured error on the renderer's device stops the GPU renderer. The renderer's own
/// work runs inside error scopes, so on the window's device ([`Gpu::adopt`]) an uncaptured error
/// naming a `Surface` is the window's swapchain: one failed `Surface::configure` (a resize or desktop
/// switch) leaves the device fine, and eframe configures it again. A device that is really gone is
/// reported by the device-lost callback.
pub(crate) fn stops_renderer(window: bool, error: &str) -> bool {
    !(window && error.contains("Surface::"))
}

#[cfg(test)]
mod tests {
    /// On the window's device, the window's surface failing to configure is logged, not a reason to stop the GPU
    /// renderer; any other uncaptured error still stops it, and on a device of the renderer's own every one does.
    #[test]
    fn only_the_window_surface_failing_leaves_the_renderer_on_the_gpu() {
        let surface =
            "Validation Error\n\nCaused by:\n  In Surface::configure\n    Failed to wait for GPU to come idle before reconfiguring the Surface";
        assert!(!super::stops_renderer(true, surface));
        assert!(super::stops_renderer(false, surface));
        assert!(super::stops_renderer(true, "Validation Error\n  In Queue::submit\n    Buffer is destroyed"));
    }

    #[test]
    fn limits_override_is_in_mebibytes_and_never_overflows() {
        assert_eq!(super::parse_limits("512MiB"), Some((512 << 20, 1024 << 20)));
        assert_eq!(super::parse_limits("0"), Some((1 << 20, 2 << 20)));
        assert_eq!(super::parse_limits(""), None);
        assert_eq!(super::parse_limits("lots"), None);
        assert_eq!(super::parse_limits("9223372036854775807"), None);
        assert_eq!(super::parse_limits("17592186044416"), None);
    }

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

    /// Issue #2936: the kernels are built on several threads; every entry point of every module has one.
    #[test]
    fn every_entry_point_has_a_kernel() {
        let Some(g) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let entries: Vec<&str> = super::MODULES.iter().flat_map(|m| m.entries.iter().copied()).collect();
        assert_eq!(g.kernels.len(), entries.len());
        for e in entries {
            assert!(g.kernels.contains_key(e), "no kernel for {e}");
        }
    }

    /// Caller mistakes the render used to panic on fail the render over to the CPU instead: a
    /// readback past the buffer, a kernel run with the wrong number of bindings, and an upload
    /// after a failure (which returns the placeholder with the requested length). A zero-length
    /// readback is no failure.
    /// Uploads and readbacks through recycled staging buffers ([`super::Staging`]): the data arrives
    /// intact round after round, and the idle trim frees them.
    #[test]
    fn uploads_and_readbacks_reuse_their_staging_buffers() {
        let Some(g) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let words = 777_773; // a size no other test uses: the device's pools are shared
        let bytes = (words * 4) as u64;
        let kept = |g: &super::Gpu| {
            let up = g.staging.up.lock().unwrap().iter().filter(|b| b.0.size() == bytes).count();
            let down = g.staging.down.lock().unwrap().iter().filter(|b| b.size() == bytes).count();
            (up, down)
        };
        for round in 0..3u32 {
            let data: Vec<u32> = (0..words as u32).map(|i| i.wrapping_mul(2_654_435_761).wrapping_add(round)).collect();
            let b = g.upload(&data);
            let back: Vec<u32> = g.finish_and_read(g.encoder(), &b, words);
            assert!(back == data, "round {round}: the data comes back intact");
            assert!(!super::failed());
        }
        // (how many are kept is not checked here: the tests share the device, and another test's
        // upload of a similar size may borrow one; `staging_pools_fit_closely_and_stay_bounded`)
        g.trim(0);
        assert_eq!(kept(&g), (0, 0), "the idle trim frees them");
    }

    /// With a share of the card's memory set, a device keeps up to that share pooled while it
    /// works, never less than the app's limit; without one, or with its memory unknown, the limit.
    #[test]
    fn the_pool_may_hold_a_share_of_the_cards_memory() {
        let gib = 1u64 << 30;
        assert_eq!(super::pool_limit(2 * gib, Some(12 * gib), 3), 4 * gib);
        assert_eq!(super::pool_limit(2 * gib, Some(3 * gib), 3), 2 * gib, "never below the app's limit");
        assert_eq!(super::pool_limit(2 * gib, None, 3), 2 * gib);
        assert_eq!(super::pool_limit(2 * gib, Some(12 * gib), 0), 2 * gib, "no share: the limit");
        assert_eq!(super::pool_limit(0, Some(u64::MAX), 3), u64::MAX / 3, "no overflow");
    }

    /// The staging pools take the smallest buffer that fits with at most 25 % to spare, and hold
    /// at most [`super::STAGING_KEEP`] bytes.
    #[test]
    fn staging_pools_fit_closely_and_stay_bounded() {
        let sizes = [100u64, 130, 124, 1000];
        assert_eq!(super::best_fit(&sizes, 100, |b| *b), Some(0), "an exact fit first");
        assert_eq!(super::best_fit(&sizes, 101, |b| *b), Some(2), "then the smallest within 25 %");
        assert_eq!(super::best_fit(&sizes, 900, |b| *b), Some(3));
        assert_eq!(super::best_fit(&sizes, 700, |b| *b), None, "1000 is too big for 700");
        assert_eq!(super::best_fit(&sizes, u64::MAX, |b| *b), None, "no overflow");
        let pool = std::sync::Mutex::new(Vec::new());
        let half = super::STAGING_KEEP / 2;
        super::keep_staging(&pool, half, |b| *b);
        super::keep_staging(&pool, half, |b| *b);
        super::keep_staging(&pool, 1, |b| *b);
        assert_eq!(pool.lock().unwrap().len(), 2, "the third would pass the limit");
    }

    #[test]
    fn caller_mistakes_fail_the_render_instead_of_panicking() {
        let Some(g) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let b = g.upload(&[1u32; 8]);
        let none: Vec<u32> = g.finish_and_read(g.encoder(), &b, 0);
        assert!(none.is_empty() && !super::failed(), "an empty readback is not a failure");

        let past: Vec<u32> = g.finish_and_read(g.encoder(), &b, 1 << 20);
        assert!(past.iter().all(|v| *v == 0));
        assert_eq!(super::take_failure().map(|f| f.kind), Some(super::FailKind::Fatal), "a readback past the buffer fails the render");

        let mut enc = g.encoder();
        let kernel = super::MODULES.iter().flat_map(|m| m.entries.iter().copied()).next().unwrap_or("?");
        g.run(&mut enc, kernel, &[], &[], [1, 1, 1]);
        assert_eq!(super::take_failure().map(|f| f.kind), Some(super::FailKind::Fatal), "a binding count mismatch fails the render");

        super::fail(super::FailKind::Fatal, "simulated failure before an upload".into());
        let up = g.upload(&[7u32; 1024]);
        assert_eq!(up.len, 1024, "the requested length is preserved");
        assert_eq!(up.raw().size(), 16, "the placeholder, not a staged 4 KiB buffer");
        assert_eq!(super::take_failure().map(|f| f.kind), Some(super::FailKind::Fatal));
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

    /// Issue #1858: once a Vulkan or DX12 device is lost, wgpu 30 panics in `Device::poll` instead of
    /// returning an error. A render's readback panicked; while it unwound, a buffer's way back to
    /// the pool polls again from a destructor, and that second panic aborted the app.
    #[test]
    fn a_poll_on_a_lost_device_neither_panics_nor_aborts() {
        if run_child("lost_poll_child").is_none() {
            lcc_testkit::skip!(Gpu, "no GPU device");
        }
    }

    /// A Metal device really lost (its command buffers exhausted) is never polled again.
    #[test]
    fn a_really_lost_device_is_not_polled_again() {
        if run_child("really_lost_child").is_none() {
            lcc_testkit::skip!(Gpu, "no GPU device, or one that can't be lost on purpose here");
        }
    }

    /// A GPU retired by another failure (a validation error) keeps a live device: its buffers are
    /// still released, which takes a poll.
    #[test]
    fn a_retired_but_live_device_is_still_polled() {
        if run_child("retired_child").is_none() {
            lcc_testkit::skip!(Gpu, "no GPU device");
        }
    }

    #[test]
    #[ignore = "run in its own process by a_retired_but_live_device_is_still_polled"]
    fn retired_child() {
        let Some(g) = crate::device() else {
            println!("CHILD: no GPU");
            return;
        };
        crate::mark_broken(0, "device error: simulated validation error");
        assert!(!crate::available());
        drop(g.upload(&[1u32; 1024]));
        g.poll(wgpu::PollType::Poll).unwrap();
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

    /// ``<PREFIX>_GPU_LIMITS`` caps the window's device as it caps a device of its own (review of
    /// PR #1065: adopting the window's device skipped it, so a 1024 × 1024 RGB float buffer — 12 MiB —
    /// was taken under an 8 MiB cap).
    #[test]
    fn the_limits_override_caps_an_adopted_device() {
        let big = wgpu::Limits { max_storage_buffer_binding_size: 1 << 30, max_buffer_size: 1 << 31, ..wgpu::Limits::default() };
        let c = super::capped(big.clone(), super::parse_limits("8"));
        assert_eq!((c.max_storage_buffer_binding_size, c.max_buffer_size), (8 << 20, 16 << 20));
        let c = super::capped(big.clone(), None);
        assert_eq!((c.max_storage_buffer_binding_size, c.max_buffer_size), (1 << 30, 1 << 31));

        let Some((device, queue, info)) = test_device() else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let full = device.limits().max_storage_buffer_binding_size.min(device.limits().max_buffer_size);
        if full <= 12 << 20 {
            lcc_testkit::skip!(Gpu, "the adapter's buffers are already ≤ 12 MiB");
            return;
        }
        let g = super::Gpu::adopt_capped(device, queue, info, None, super::parse_limits("8")).expect("kernels build");
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
        let instance = crate::new_instance(desc);
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

    #[test]
    fn dispatches_stay_within_the_workgroup_limit() {
        // 1-D kernels over a 100 MP image and 2-D ones over 16k × 16k stay under 65535 per axis
        assert!(super::groups1(100_000_000 * 3).iter().all(|g| *g <= 65535));
        assert!(super::groups2(16384, 16384, [16, 16]).iter().all(|g| *g <= 65535));
    }
}
