//! Which wgpu backends the app lets wgpu use — for its compute device and for the desktop
//! window (eframe) — and the crash sentinel around device creation (issue #136).
//!
//! wgpu loads the system driver of *every* backend in the instance's set while it enumerates
//! adapters, even when it then picks another one: with Vulkan in the set, a broken Vulkan driver
//! can crash the process (an access violation inside the driver DLL, which nothing can catch)
//! although DX12 would have been used. So each backend is tried on its own, in order: on Windows
//! Vulkan first and DX12 as the fallback (issue #934: DX12 compiles every kernel with FXC, which on
//! a cold shader cache took 31 s on an RTX 2060; Vulkan builds them in a fraction of that). The
//! window probes Vulkan under its crash marker (`window_renderer_order`): a start that dies in it
//! tries Vulkan last next time, and the compute devices follow the window's backend. macOS uses
//! Metal, Linux Vulkan (plus GL for the window when the build has it).
//!
//! Overrides, read once per process:
//! - `{ENV_PREFIX}_GPU_BACKEND` = `dx12` | `vulkan` | `metal` | `gl` (comma lists allowed), `auto`
//!   (the platform default), or `off` (no GPU compute; the window still needs one backend and
//!   keeps the platform default);
//! - else `WGPU_BACKEND` (wgpu's own variable, same names) for both the window and compute;
//! - `{ENV_PREFIX}_GPU_ADAPTER` = (part of) a graphics card's name, any case (`4070`): the card for
//!   the compute devices — the render device, the denoise network's, the face detector's
//!   ([`pick_adapter`]).
//! - `{ENV_PREFIX}_GPU_SOFTWARE=1`: CI and tests only — render on the software adapter (lavapipe /
//!   llvmpipe on Vulkan, WARP on DX12) instead of a hardware card, which the app otherwise never
//!   does (the CPU pipeline is faster). The nightly software-adapter equivalence job uses it
//!   (`docs/ci.md`); [`software_forced`].
//! - else, for compute only, the saved preference ([`set_compute_preference`]: Settings ▸
//!   Performance ▸ Graphics interface). On Windows, DX12 builds the compute shaders with FXC, which
//!   can take many seconds at every start on some machines; Vulkan builds them much faster. The
//!   window keeps the platform default, so a broken Vulkan driver can't stop it from opening.
//!
//! Backends this build doesn't contain (e.g. `gl`: the GL backend isn't compiled in) are ignored
//! with a warning and the platform default is used.

use std::path::PathBuf;
use std::sync::Mutex;

pub use wgpu::Backends;

/// What the environment asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    /// The platform default.
    Auto,
    /// No GPU compute.
    Off,
    /// These backends.
    Use(Backends),
}

/// Parse a backend list (`"dx12"`, `"vulkan,gl"`, `"auto"`, `"off"`…). `None`: nothing usable in
/// it (empty or unknown names).
pub fn parse_backends(s: &str) -> Option<BackendChoice> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "auto" | "default" => Some(BackendChoice::Auto),
        "off" | "none" | "cpu" | "0" | "false" | "no" => Some(BackendChoice::Off),
        // the window's OpenGL renderer (see `WindowRenderer`): compute keeps its default
        "glow" => Some(BackendChoice::Auto),
        _ => {
            let b = Backends::from_comma_list(&s);
            (!b.is_empty()).then_some(BackendChoice::Use(b))
        }
    }
}

/// The choice from `{ENV_PREFIX}_GPU_BACKEND` (`app`) or else `WGPU_BACKEND` (`wgpu`): the
/// first one that parses wins; an unknown value is skipped (with a warning).
pub fn choose(app: Option<&str>, wgpu: Option<&str>) -> BackendChoice {
    let app_name = lcc_brand::env_var("GPU_BACKEND");
    for (name, v) in [(app_name.as_str(), app), ("WGPU_BACKEND", wgpu)] {
        let Some(v) = v else { continue };
        match parse_backends(v) {
            // WGPU_BACKEND is wgpu's variable: it selects backends, it never turns the GPU off
            Some(BackendChoice::Off) if name == "WGPU_BACKEND" => log::warn!("gpu: WGPU_BACKEND={v} ignored (use {app_name}=off)"),
            Some(c) => return c,
            None => log::warn!("gpu: {name}={v} names no known backend; ignored"),
        }
    }
    BackendChoice::Auto
}

/// The backends compute devices use when nothing is asked for on `os` (`std::env::consts::OS`):
/// the first of [`compute_backend_order`].
pub fn default_compute_backends(os: &str) -> Backends {
    compute_backend_order(os).first().copied().unwrap_or(Backends::PRIMARY)
}

/// The backends tried, one at a time, for the compute devices on `os` when nothing is asked for:
/// on Windows Vulkan, then DX12 (issue #934).
pub fn compute_backend_order(os: &str) -> Vec<Backends> {
    match os {
        "windows" => vec![Backends::VULKAN, Backends::DX12],
        "macos" | "ios" => vec![Backends::METAL],
        _ => vec![Backends::PRIMARY],
    }
}

/// The automatic compute backend: the window's when it runs on one of `order` (it already loaded
/// that driver safely, and the editing device shares it), else the first of `order` in `compiled`
/// that `has_adapter` (a hardware graphics card on it), else the first of `order` in the build.
pub fn pick_compute_backend(order: &[Backends], compiled: Backends, window: Option<Backends>, has_adapter: impl Fn(Backends) -> bool) -> Backends {
    let usable: Vec<Backends> = order.iter().copied().filter(|b| !b.is_empty() && compiled.contains(*b)).collect();
    if let Some(w) = window.filter(|w| usable.contains(w)) {
        return w;
    }
    usable
        .iter()
        .copied()
        .find(|b| has_adapter(*b))
        .or_else(|| usable.first().copied())
        .unwrap_or_else(|| order.first().copied().unwrap_or(Backends::PRIMARY))
}

/// The backend the window's renderer runs on (set by [`crate::offer_window_device`]).
static WINDOW_BACKEND: Mutex<Option<Backends>> = Mutex::new(None);

pub(crate) fn note_window_backend(b: wgpu::Backend) {
    *WINDOW_BACKEND.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Backends::from(b));
}

/// Whether `backends` offers a hardware graphics card (not a software adapter).
fn has_hardware_adapter(backends: Backends) -> bool {
    let instance = crate::ctx::instance(backends);
    let adapters = with_marker(&format!("adapter probe ({backends:?})"), || pollster::block_on(instance.enumerate_adapters(backends)));
    adapters.iter().any(|a| a.get_info().device_type != wgpu::DeviceType::Cpu || software_forced())
}

/// `<PREFIX>_GPU_SOFTWARE` is on: the compute device is the software adapter (CI/tests only).
pub fn software_forced() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| software_forced_from(lcc_brand::env("GPU_SOFTWARE").as_deref()))
}

/// [`software_forced`] from the variable's value: `1`, `true`, `yes` or `on`, any case.
pub fn software_forced_from(env: Option<&str>) -> bool {
    env.map(|s| s.trim().to_ascii_lowercase()).is_some_and(|s| matches!(s.as_str(), "1" | "true" | "yes" | "on"))
}

/// The automatic compute backend for this process ([`pick_compute_backend`]), decided once.
fn auto_compute_backends() -> Backends {
    static AUTO: std::sync::OnceLock<Backends> = std::sync::OnceLock::new();
    *AUTO.get_or_init(|| {
        let os = std::env::consts::OS;
        let window = *WINDOW_BACKEND.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let b = pick_compute_backend(&compute_backend_order(os), wgpu::Instance::enabled_backend_features(), window, has_hardware_adapter);
        log::info!("gpu: compute backend {b:?} (window {window:?})");
        b
    })
}

/// The backends the window uses when nothing is asked for on `os`.
pub fn default_window_backends(os: &str) -> Backends {
    match os {
        "windows" => Backends::VULKAN,
        "macos" | "ios" => Backends::METAL,
        // eframe's default (GL only when the build has it)
        _ => Backends::PRIMARY | Backends::GL,
    }
}

/// Resolve `choice` against the backends this build contains (`compiled`): `None` = GPU off.
/// Asked-for backends that aren't compiled in fall back to `default`.
pub fn resolve(choice: BackendChoice, default: Backends, compiled: Backends) -> Option<Backends> {
    match choice {
        BackendChoice::Off => None,
        BackendChoice::Auto => Some(default),
        BackendChoice::Use(b) if b.intersects(compiled) => Some(b & compiled),
        BackendChoice::Use(b) => {
            log::warn!("gpu: backend {b:?} is not in this build ({compiled:?}); using {default:?}");
            Some(default)
        }
    }
}

static COMPUTE_PREFERENCE: std::sync::OnceLock<BackendChoice> = std::sync::OnceLock::new();

/// The saved compute backend preference (`"auto"`, `"dx12"`, `"vulkan"`…), applied when neither
/// environment variable names one. Call it once at launch, before any compute device is created;
/// later calls are ignored. `off` and unknown names count as `auto`: turning the GPU off is
/// [`crate::set_enabled`]'s job.
pub fn set_compute_preference(name: &str) {
    let choice = match parse_backends(name) {
        Some(BackendChoice::Use(b)) => BackendChoice::Use(b),
        _ => BackendChoice::Auto,
    };
    let _ = COMPUTE_PREFERENCE.set(choice);
}

/// The compute choice: the environment's when it names one, else `preference`.
pub fn compute_choice(env: BackendChoice, env_set: bool, preference: Option<BackendChoice>) -> BackendChoice {
    // (an explicit `auto` in the environment also wins: the platform default, whatever the setting)
    if env_set { env } else { preference.unwrap_or(BackendChoice::Auto) }
}

fn env_is_set() -> bool {
    let usable = |v: Option<String>| v.is_some_and(|v| parse_backends(&v).is_some());
    usable(lcc_brand::env("GPU_BACKEND")) || usable(std::env::var("WGPU_BACKEND").ok())
}

fn env_choice() -> BackendChoice {
    static C: std::sync::OnceLock<BackendChoice> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        let lc = lcc_brand::env("GPU_BACKEND");
        let wg = std::env::var("WGPU_BACKEND").ok();
        choose(lc.as_deref(), wg.as_deref())
    })
}

/// The backends of the compute device (`None`: `{ENV_PREFIX}_GPU_BACKEND=off`).
pub fn compute_backends() -> Option<Backends> {
    let choice = compute_choice(env_choice(), env_is_set(), COMPUTE_PREFERENCE.get().copied());
    let compiled = wgpu::Instance::enabled_backend_features();
    match choice {
        BackendChoice::Auto => Some(auto_compute_backends()),
        c => resolve(c, default_compute_backends(std::env::consts::OS), compiled),
    }
}

/// The backends for the desktop window's renderer (eframe/egui-wgpu). The window always needs
/// one: `off` keeps the platform default.
pub fn window_backends() -> Backends {
    let default = default_window_backends(std::env::consts::OS);
    let choice = match env_choice() {
        BackendChoice::Off => BackendChoice::Auto,
        c => c,
    };
    resolve(choice, default, wgpu::Instance::enabled_backend_features()).unwrap_or(default)
}

/// The DX12 shader compiler: FXC (`d3dcompiler_47.dll`, part of Windows) unless
/// `WGPU_DX12_COMPILER` (`wgpu`, wgpu's own variable: `fxc`, `dxc`, `auto`…) names another.
///
/// Issue #471: wgpu's default (`Auto`) loads whichever `dxcompiler.dll` the DLL search path finds
/// first — the app ships none, so it is some other program's (an SDK's, a folder on `PATH`). A
/// copy without its `dxil.dll` beside it warns that the DXIL is unsigned; wgpu takes the warning for
/// a compile error, its own validation pipelines fail, the device is lost and the window never
/// opens. FXC is always there and compiles everything the app's shaders need.
pub fn dx12_compiler(env: Option<&str>) -> wgpu::Dx12Compiler {
    let Some(v) = env else { return wgpu::Dx12Compiler::Fxc };
    v.parse().unwrap_or_else(|e| {
        log::warn!("gpu: WGPU_DX12_COMPILER={v} ignored: {e}");
        wgpu::Dx12Compiler::Fxc
    })
}

/// wgpu's backend options for every instance the app creates — the window's and the compute
/// devices': wgpu's environment variables, with the DX12 compiler from [`dx12_compiler`].
pub fn backend_options() -> wgpu::BackendOptions {
    let mut o = wgpu::BackendOptions::from_env_or_default();
    o.dx12.shader_compiler = dx12_compiler(std::env::var("WGPU_DX12_COMPILER").ok().as_deref());
    o
}

/// wgpu's instance flags for every instance the app creates, from `flags` (wgpu's defaults with its
/// environment variables applied) without `VALIDATION_INDIRECT_CALL`, unless
/// `WGPU_VALIDATION_INDIRECT_CALL` asks for it (any value but `0`). That validation builds an internal
/// compute pipeline when each device is created; Intel Macs' Metal compiler fails on it ("Compiler
/// encountered an internal error"), the device is lost and the window never opens (#250). It only
/// guards indirect draws and dispatches, which neither the app nor egui issue.
pub fn instance_flags(flags: wgpu::InstanceFlags, env: Option<&str>) -> wgpu::InstanceFlags {
    if env.is_some_and(|v| v != "0") {
        flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    }
}

/// [`instance_flags`] with wgpu's environment variables applied to `flags` (`WGPU_VALIDATION`, `WGPU_DEBUG`,
/// `WGPU_ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER`…, see `wgpu::InstanceFlags::with_env`) and
/// `WGPU_VALIDATION_INDIRECT_CALL` read from the environment. The compute devices pass wgpu's defaults here,
/// so this is where those variables reach them: a Vulkan driver that reports no conformance version, like
/// Mesa's Dozen (the only one on WSL2), is listed only with `WGPU_ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER=1`.
pub fn instance_flags_from_env(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    instance_flags(flags.with_env(), std::env::var("WGPU_VALIDATION_INDIRECT_CALL").ok().as_deref())
}

/// The graphics card `<PREFIX>_GPU_ADAPTER` asks for: (part of) its name, trimmed and in lower
/// case; `None` when it is unset or blank.
pub fn wanted_adapter() -> Option<String> {
    wanted_adapter_from(lcc_brand::env("GPU_ADAPTER").as_deref())
}

/// [`wanted_adapter`] from the variable's value `env`.
pub fn wanted_adapter_from(env: Option<&str>) -> Option<String> {
    env.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty())
}

/// Whether the adapter called `name` is the one `wanted` asks for: `wanted` is part of its name,
/// ignoring case (`4070` matches "NVIDIA GeForce RTX 4070 SUPER").
pub fn adapter_name_matches(wanted: &str, name: &str) -> bool {
    let wanted = wanted.trim().to_lowercase();
    !wanted.is_empty() && name.to_lowercase().contains(&wanted)
}

/// Which of `adapters` (name and kind, in wgpu's order) `wanted` picks: the first hardware one whose
/// name matches. Software adapters (WARP, llvmpipe) are never picked: the CPU pipeline is faster.
pub fn find_named_adapter<'a>(wanted: &str, adapters: impl IntoIterator<Item = (&'a str, wgpu::DeviceType)>) -> Option<usize> {
    adapters.into_iter().position(|(name, kind)| kind != wgpu::DeviceType::Cpu && adapter_name_matches(wanted, name))
}

/// The adapter for a compute device (the render device, the denoise network's, the face detector's)
/// on `instance` with `backends`: the card `<PREFIX>_GPU_ADAPTER` names, else the
/// high-performance one. A name that matches no card is logged and the high-performance one is
/// used; never a panic.
pub fn pick_adapter(instance: &wgpu::Instance, backends: Backends) -> Result<wgpu::Adapter, String> {
    pick_adapter_named(instance, backends, wanted_adapter().as_deref())
}

/// [`pick_adapter`] for an explicit `wanted` name (part of it, any case) instead of the variable's.
pub fn pick_adapter_named(instance: &wgpu::Instance, backends: Backends, wanted: Option<&str>) -> Result<wgpu::Adapter, String> {
    if software_forced() {
        // wgpu's fallback adapter is the software one (WARP on DX12, lavapipe on Vulkan)
        return pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { force_fallback_adapter: true, ..Default::default() }))
            .map_err(|e| format!("{} is set but there is no software adapter among {backends:?} ({e})", lcc_brand::env_var("GPU_SOFTWARE")));
    }
    if let Some(w) = wanted {
        let all = pollster::block_on(instance.enumerate_adapters(backends));
        let infos: Vec<wgpu::AdapterInfo> = all.iter().map(|a| a.get_info()).collect();
        let found = find_named_adapter(w, infos.iter().map(|i| (i.name.as_str(), i.device_type)));
        if let Some(adapter) = found.and_then(|i| all.into_iter().nth(i)) {
            log::info!("gpu: GPU_ADAPTER={w}: using {}", adapter.get_info().name);
            return Ok(adapter);
        }
        let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        log::warn!("gpu: GPU_ADAPTER={w} names none of the graphics cards ({}); using the default choice", names.join(", "));
    }
    pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))
}

/// `{ENV_PREFIX}_GPU_BACKEND=off`.
pub(crate) fn env_off() -> bool {
    env_choice() == BackendChoice::Off
}

static MARKER: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Scopes of driver work (device creation, shader and pipeline compilation) running now, so
/// that overlapping ones (the window-side warm-up and a first denoise) share one marker file.
static ACTIVE: Mutex<u32> = Mutex::new(0);

/// Write this file before the compute device is created or any shader / pipeline is compiled
/// (the kernels, the denoise network's), and remove it once that returned (successfully or not).
/// A file still there at the next launch means the process died inside the driver — e.g. Apple's
/// Metal compiler (`MTLCompilerService`) aborting on a kernel, issue #250 — or ended while a device
/// creation that passed its time limit was still stuck there (issue #3068): see
/// [`take_init_marker`]. `None` (the default) writes nothing.
pub fn set_init_marker(path: Option<PathBuf>) {
    *MARKER.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

/// The marker's text: what started, and when.
fn marker_text(what: &str, secs: u64) -> String {
    format!("GPU {what} started (unix time {secs})\n")
}

/// Enter a driver-work scope on `marker`: the first one writes the file.
#[cfg(test)]
fn arm(marker: &std::path::Path, active: &Mutex<u32>, what: &str) {
    arm_locked(marker, &mut active.lock().unwrap_or_else(|e| e.into_inner()), what);
}

/// [`arm`] with the scope counter `n` already locked.
fn arm_locked(marker: &std::path::Path, n: &mut u32, what: &str) {
    if *n == 0 {
        if let Some(d) = marker.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = std::fs::write(marker, marker_text(what, secs));
    }
    *n += 1;
}

/// Leave a scope: the last one removes the file.
fn disarm(marker: &std::path::Path, active: &Mutex<u32>) {
    let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
    *n = n.saturating_sub(1);
    if *n == 0 {
        let _ = std::fs::remove_file(marker);
    }
}

/// Removes the marker on the way out, a panic included (the process survived).
struct Armed<'a>(Option<PathBuf>, &'a Mutex<u32>);

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        if let Some(m) = &self.0 {
            disarm(m, self.1);
        }
    }
}

/// Run driver work `f` (`what`: "device creation", "kernel compilation"…) between writing and
/// removing the init marker (if one is set).
#[cfg_attr(not(feature = "denoise"), allow(dead_code))] // the denoise network's kernels
pub(crate) fn with_marker<T>(what: &str, f: impl FnOnce() -> T) -> T {
    with_marker_in(&MARKER, &ACTIVE, what, f)
}

/// [`with_marker`] on an explicit marker slot and scope counter (the tests use their own, so that
/// device-creating tests running in parallel cannot join their scopes). The slot is read under the
/// scope lock (scope lock first, then slot lock, everywhere), so a scope starting while
/// [`release_on`] runs either is cleared by it or writes nothing.
fn with_marker_in<T>(slot: &Mutex<Option<PathBuf>>, active: &Mutex<u32>, what: &str, f: impl FnOnce() -> T) -> T {
    let marker = {
        let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
        let marker = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(m) = &marker {
            arm_locked(m, &mut n, what);
        }
        marker
    };
    let _armed = Armed(marker, active);
    f()
}

/// The app is quitting in an orderly way: remove the init marker now, even while device creation
/// (or a kernel build) is still running on another thread, and write none from here on.
///
/// A launch that quits a few seconds in, before the driver returned the device (e.g. while a large
/// library was still opening), used to leave the marker behind, so the next launch said the app had
/// closed unexpectedly while starting the GPU and turned GPU rendering off. An orderly quit proves
/// the process was not killed inside the driver; a real crash or kill during device creation never
/// gets here, so its marker stays for the next launch to report.
pub fn release_init_marker_on_quit() {
    release_on(&MARKER, &ACTIVE);
}

/// [`release_init_marker_on_quit`] on an explicit marker slot and scope counter (for the tests).
fn release_on(slot: &Mutex<Option<PathBuf>>, active: &Mutex<u32>) {
    // the scope lock first, as `with_marker_in` / `disarm` take it: no scope writes the file while it goes
    let _n = active.lock().unwrap_or_else(|e| e.into_inner());
    let marker = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(m) = marker
        && let Err(e) = std::fs::remove_file(&m)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("gpu: could not remove {}: {e}", m.display());
    }
}

/// The init marker (if one is set) and its scope counter, taken on the thread that asks for a device
/// and carried to the thread that creates it (`init.rs`, issue #3068): the marker stays while the
/// driver work really runs, also past the caller's time limit.
pub(crate) struct InitMarker {
    slot: &'static Mutex<Option<PathBuf>>,
    active: &'static Mutex<u32>,
}

impl InitMarker {
    /// The process-wide marker ([`set_init_marker`]).
    pub(crate) fn current() -> InitMarker {
        InitMarker { slot: &MARKER, active: &ACTIVE }
    }

    /// A marker at `path` on its own scope counter (tests: other tests create real devices through
    /// the process-wide one).
    #[cfg(test)]
    pub(crate) fn at(path: PathBuf, active: &'static Mutex<u32>) -> InitMarker {
        // leaked: the slot must live as long as the scope counter, and a test process is short
        InitMarker { slot: Box::leak(Box::new(Mutex::new(Some(path)))), active }
    }

    /// Run device creation (and the kernel build that follows it) `f` on `backends` between writing
    /// and removing the marker.
    pub(crate) fn run<T>(self, backends: Backends, f: impl FnOnce() -> T) -> T {
        with_marker_in(self.slot, self.active, &format!("device and kernel creation (backends {backends:?})"), f)
    }
}

/// If the init marker `path` is present — the last process died while creating the GPU device —
/// remove it and return what it recorded.
pub fn take_init_marker(path: &std::path::Path) -> Option<String> {
    let text = read_init_marker(path)?;
    let _ = std::fs::remove_file(path);
    Some(text)
}

/// What the init marker `path` recorded, if it is present — without removing it. For sessions
/// that must leave the disk as they found it (`--memory`, issues #164 and #169): GPU rendering
/// still starts off, and the marker stays for the next ordinary launch to report and clear.
pub fn read_init_marker(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string())
}

/// What draws the desktop window: egui-wgpu on one wgpu backend, or eframe's OpenGL renderer
/// (glow) as the last resort when no wgpu backend can build the window's pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRenderer {
    /// egui-wgpu on exactly one backend.
    Wgpu(Backends),
    /// eframe's glow (OpenGL) renderer.
    Glow,
}

impl WindowRenderer {
    /// The name used in `{ENV_PREFIX}_GPU_BACKEND`, the remembered-renderer file and the logs.
    pub fn name(self) -> &'static str {
        match self {
            WindowRenderer::Glow => "glow",
            WindowRenderer::Wgpu(b) if b == Backends::DX12 => "dx12",
            WindowRenderer::Wgpu(b) if b == Backends::VULKAN => "vulkan",
            WindowRenderer::Wgpu(b) if b == Backends::METAL => "metal",
            WindowRenderer::Wgpu(b) if b == Backends::GL => "gl",
            WindowRenderer::Wgpu(_) => "wgpu",
        }
    }

    /// One renderer name (`dx12`, `vulkan`, `metal`, `gl`, `glow`); anything else (lists, `auto`,
    /// `off`, unknown names) is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s == "glow" {
            return Some(WindowRenderer::Glow);
        }
        match parse_backends(&s)? {
            BackendChoice::Use(b) if [Backends::DX12, Backends::VULKAN, Backends::METAL, Backends::GL].contains(&b) => Some(WindowRenderer::Wgpu(b)),
            _ => None,
        }
    }
}

/// The renderer the environment forces for the window: `{ENV_PREFIX}_GPU_BACKEND` (`app`), else
/// `WGPU_BACKEND` (`wgpu`), when it names exactly one renderer.
pub fn forced_window_renderer(app: Option<&str>, wgpu: Option<&str>) -> Option<WindowRenderer> {
    app.and_then(WindowRenderer::parse).or_else(|| wgpu.and_then(WindowRenderer::parse))
}

/// The order in which the window's renderers are tried on `os`: the forced one (environment)
/// first, then the one remembered from the last good start, then the platform chain — on Windows
/// Vulkan, DX12, GL, then glow (issue #934: Vulkan builds the kernels much faster than DX12's FXC).
/// A broken Vulkan driver can crash the process uncatchably (issue #136): the start's marker then
/// names it, and the next start tries it last.
/// `failed` (the renderer the last start died in) goes last, as a last resort only. Backends not
/// in `compiled`, and glow without `glow`, are left out; the list is never empty.
pub fn window_renderer_order(
    forced: Option<WindowRenderer>,
    remembered: Option<WindowRenderer>,
    failed: Option<WindowRenderer>,
    os: &str,
    compiled: Backends,
    glow: bool,
) -> Vec<WindowRenderer> {
    let platform = match os {
        "windows" => vec![Backends::VULKAN, Backends::DX12, Backends::GL],
        "macos" | "ios" => vec![Backends::METAL],
        _ => vec![Backends::VULKAN, Backends::GL],
    };
    let chain = platform.into_iter().map(WindowRenderer::Wgpu).chain([WindowRenderer::Glow]);
    let usable = |r: &WindowRenderer| match r {
        WindowRenderer::Glow => glow,
        WindowRenderer::Wgpu(b) => !b.is_empty() && compiled.contains(*b),
    };
    let not_failed = |r: &WindowRenderer| Some(*r) != failed;
    let candidates = forced.into_iter().chain(remembered.filter(not_failed)).chain(chain.filter(not_failed)).chain(failed);
    let mut out: Vec<WindowRenderer> = Vec::new();
    for r in candidates {
        if usable(&r) && !out.contains(&r) {
            out.push(r);
        }
    }
    if out.is_empty() {
        out.push(WindowRenderer::Wgpu(default_window_backends(os) & compiled));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {

    /// The environment names a backend: it wins over the saved preference; otherwise the
    /// preference applies; nothing at all is the platform default.
    #[test]
    fn software_adapter_is_forced_only_by_a_yes() {
        for on in ["1", "true", " YES ", "On"] {
            assert!(software_forced_from(Some(on)), "{on}");
        }
        for off in [None, Some(""), Some("0"), Some("no"), Some("off"), Some("lavapipe")] {
            assert!(!software_forced_from(off), "{off:?}");
        }
    }

    #[test]
    fn compute_preference_applies_only_without_an_environment_choice() {
        let vulkan = BackendChoice::Use(Backends::VULKAN);
        let dx12 = BackendChoice::Use(Backends::DX12);
        assert_eq!(compute_choice(BackendChoice::Auto, false, Some(vulkan)), vulkan);
        assert_eq!(compute_choice(dx12, true, Some(vulkan)), dx12);
        assert_eq!(compute_choice(BackendChoice::Off, true, Some(vulkan)), BackendChoice::Off);
        assert_eq!(compute_choice(BackendChoice::Auto, true, Some(vulkan)), BackendChoice::Auto);
        assert_eq!(compute_choice(BackendChoice::Auto, false, None), BackendChoice::Auto);
    }

    /// #250: the indirect-call validation pipeline is off unless asked for; other flags are kept.
    #[test]
    fn indirect_call_validation_is_off_unless_asked_for() {
        use wgpu::InstanceFlags as F;
        let base = F::VALIDATION_INDIRECT_CALL | F::DEBUG;
        assert_eq!(instance_flags(base, None), F::DEBUG);
        assert_eq!(instance_flags(base, Some("0")), F::DEBUG);
        assert_eq!(instance_flags(F::empty(), Some("1")), F::VALIDATION_INDIRECT_CALL);
        assert_eq!(instance_flags(F::empty(), None), F::empty());
    }
    use super::*;

    #[test]
    fn app_variable_wins_over_wgpu_backend() {
        assert_eq!(choose(None, None), BackendChoice::Auto);
        assert_eq!(choose(None, Some("dx12")), BackendChoice::Use(Backends::DX12));
        assert_eq!(choose(Some("vulkan"), Some("dx12")), BackendChoice::Use(Backends::VULKAN));
        assert_eq!(choose(Some("off"), Some("dx12")), BackendChoice::Off);
        // unknown value of our variable: WGPU_BACKEND still applies
        assert_eq!(choose(Some("bogus"), Some("gl")), BackendChoice::Use(Backends::GL));
        // WGPU_BACKEND never turns the GPU off
        assert_eq!(choose(None, Some("off")), BackendChoice::Auto);
    }

    /// Issue #471: DX12 shaders compile with FXC, never a `dxcompiler.dll` found on the search path,
    /// unless `WGPU_DX12_COMPILER` asks for it.
    #[test]
    fn dx12_compiles_with_fxc_unless_asked() {
        assert!(matches!(dx12_compiler(None), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("bogus")), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("FXC")), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("dxc")), wgpu::Dx12Compiler::DynamicDxc { .. }));
        assert!(matches!(dx12_compiler(Some("auto")), wgpu::Dx12Compiler::Auto));
        if std::env::var_os("WGPU_DX12_COMPILER").is_none() {
            assert!(matches!(backend_options().dx12.shader_compiler, wgpu::Dx12Compiler::Fxc));
        }
    }

    /// `<PREFIX>_GPU_ADAPTER` picks a card by part of its name, any case; software adapters never.
    #[test]
    fn gpu_adapter_variable_matches_part_of_the_card_name() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu};
        assert_eq!(wanted_adapter_from(None), None);
        assert_eq!(wanted_adapter_from(Some("  ")), None);
        assert_eq!(wanted_adapter_from(Some(" RTX 4070 ")).as_deref(), Some("rtx 4070"));
        assert!(adapter_name_matches("4070", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(adapter_name_matches("geforce rtx 4070 super", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(adapter_name_matches(" Super ", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(!adapter_name_matches("4070", "NVIDIA GeForce RTX 5070"));
        assert!(!adapter_name_matches("", "NVIDIA GeForce RTX 5070"), "a blank name matches nothing");
        let two = [("NVIDIA GeForce RTX 5070", DiscreteGpu), ("NVIDIA GeForce RTX 4070 SUPER", DiscreteGpu), ("Microsoft Basic Render Driver", Cpu)];
        assert_eq!(find_named_adapter("4070", two), Some(1));
        assert_eq!(find_named_adapter("5070", two), Some(0));
        assert_eq!(find_named_adapter("070", two), Some(0));
        assert_eq!(find_named_adapter("radeon", two), None, "no match: the caller falls back to the default choice");
        assert_eq!(find_named_adapter("basic render", two), None, "a software adapter is never picked");
        assert_eq!(find_named_adapter("intel", [("Intel(R) UHD Graphics 770", IntegratedGpu)]), Some(0));
        assert_eq!(find_named_adapter("4070", [] as [(&str, wgpu::DeviceType); 0]), None);
    }

    #[test]
    fn resolves_against_the_build() {
        let compiled = Backends::DX12 | Backends::VULKAN;
        let def = Backends::DX12;
        assert_eq!(resolve(BackendChoice::Auto, def, compiled), Some(def));
        assert_eq!(resolve(BackendChoice::Off, def, compiled), None);
        assert_eq!(resolve(BackendChoice::Use(Backends::VULKAN), def, compiled), Some(Backends::VULKAN));
        assert_eq!(resolve(BackendChoice::Use(Backends::VULKAN | Backends::GL), def, compiled), Some(Backends::VULKAN));
        // GL isn't compiled in: the default, not an empty set (no adapter at all)
        assert_eq!(resolve(BackendChoice::Use(Backends::GL), def, compiled), Some(def));
    }

    #[test]
    fn init_marker_is_written_during_creation_and_removed_after() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-marker-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        // exclusive: a directory planted at this predictable name fails the test instead of receiving the marker
        std::fs::create_dir(&dir).unwrap();
        // a local scope counter: other tests create real devices through the process-wide one
        let active = Mutex::new(0);
        let seen = with_marker_in(&Mutex::new(Some(m.clone())), &active, "device creation", || m.exists());
        assert!(seen, "the marker exists while the device is created");
        assert!(!m.exists(), "and is removed afterwards");
        // a marker left behind by a crashed process is reported once; reading it leaves it in place
        std::fs::write(&m, "GPU device and kernel creation started (backends DX12)").unwrap();
        assert!(read_init_marker(&m).unwrap().contains("DX12"));
        assert!(m.exists(), "read_init_marker leaves the marker for the next launch");
        assert!(take_init_marker(&m).unwrap().contains("DX12"));
        assert_eq!(take_init_marker(&m), None);
        assert_eq!(read_init_marker(&m), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_current_init_marker_is_the_process_wide_one() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-global-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        // exclusive: a directory planted at this predictable name fails the test instead of receiving the marker
        std::fs::create_dir(&dir).unwrap();
        set_init_marker(Some(m.clone()));
        let seen = InitMarker::current().run(Backends::DX12, || m.exists());
        set_init_marker(None);
        // not checked for removal: device-creating tests running in parallel may still hold the file
        // (the test above checks removal on its own scope counter)
        assert!(seen, "the marker set with set_init_marker exists while the device is created");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The window's renderer fallback: forced, remembered, the platform chain, the failed one last.
    #[test]
    fn window_renderer_fallback_order() {
        use WindowRenderer::{Glow, Wgpu};
        let all = Backends::DX12 | Backends::VULKAN | Backends::GL | Backends::METAL;
        let (dx, vk, gl) = (Wgpu(Backends::DX12), Wgpu(Backends::VULKAN), Wgpu(Backends::GL));
        assert_eq!(window_renderer_order(None, None, None, "windows", all, true), [vk, dx, gl, Glow]);
        assert_eq!(window_renderer_order(None, None, None, "windows", all, false), [vk, dx, gl]);
        // a build without GL
        assert_eq!(window_renderer_order(None, None, None, "windows", Backends::DX12 | Backends::VULKAN, true), [vk, dx, Glow]);
        // a build without Vulkan: DX12 first
        assert_eq!(window_renderer_order(None, None, None, "windows", Backends::DX12, false), [dx]);
        // remembered from the last good start goes first
        assert_eq!(window_renderer_order(None, Some(gl), None, "windows", all, true), [gl, vk, dx, Glow]);
        // the environment wins over the remembered one
        assert_eq!(window_renderer_order(Some(gl), Some(dx), None, "windows", all, true), [gl, dx, vk, Glow]);
        assert_eq!(window_renderer_order(Some(Glow), None, None, "windows", all, true)[0], Glow);
        // a forced backend that is not in the build is skipped
        assert_eq!(window_renderer_order(Some(Wgpu(Backends::METAL)), None, None, "windows", Backends::DX12, false), [dx]);
        // never empty
        assert!(!window_renderer_order(None, None, None, "windows", Backends::empty(), false).is_empty());
        assert_eq!(window_renderer_order(None, None, None, "macos", all, true), [Wgpu(Backends::METAL), Glow]);
    }

    /// Issue #934: Windows tries Vulkan first and falls back to DX12; a start that died in Vulkan
    /// (a broken driver, issue #136) tries it last, also when it was remembered.
    #[test]
    fn windows_tries_vulkan_first_and_a_vulkan_crash_puts_it_last() {
        use WindowRenderer::{Glow, Wgpu};
        let all = Backends::DX12 | Backends::VULKAN | Backends::GL;
        let (dx, vk, gl) = (Wgpu(Backends::DX12), Wgpu(Backends::VULKAN), Wgpu(Backends::GL));
        assert_eq!(window_renderer_order(None, None, None, "windows", all, true).first(), Some(&vk));
        assert_eq!(window_renderer_order(None, Some(vk), Some(vk), "windows", all, true), [dx, gl, Glow, vk]);
        assert_eq!(window_renderer_order(None, None, Some(vk), "windows", all, true).first(), Some(&dx));
    }

    /// Issue #934: the automatic compute backend on Windows is Vulkan, DX12 when Vulkan has no
    /// graphics card or isn't built in, and the window's backend when it runs on one of them.
    #[test]
    fn compute_backend_prefers_vulkan_then_dx12() {
        let (vk, dx) = (Backends::VULKAN, Backends::DX12);
        let order = compute_backend_order("windows");
        assert_eq!(order, [vk, dx]);
        assert_eq!(default_compute_backends("windows"), vk);
        let all = vk | dx | Backends::GL;
        assert_eq!(pick_compute_backend(&order, all, None, |_| true), vk);
        // no Vulkan card (or no Vulkan driver): DX12
        assert_eq!(pick_compute_backend(&order, all, None, |b| b == dx), dx);
        // Vulkan not in the build: DX12, without probing Vulkan
        assert_eq!(
            pick_compute_backend(&order, dx, None, |b| {
                assert_ne!(b, vk);
                true
            }),
            dx
        );
        // the window fell back to DX12 (Vulkan failed there): compute follows it, never probing
        assert_eq!(pick_compute_backend(&order, all, Some(dx), |_| panic!("probed")), dx);
        // a window on GL is not a compute backend: the order decides
        assert_eq!(pick_compute_backend(&order, all, Some(Backends::GL), |_| true), vk);
        // no card anywhere: still the first in the build (creation then reports why)
        assert_eq!(pick_compute_backend(&order, all, None, |_| false), vk);
        assert_eq!(compute_backend_order("macos"), [Backends::METAL]);
        assert_eq!(compute_backend_order("linux"), [Backends::PRIMARY]);
    }

    /// An orderly quit while the device is still being created removes the marker, so the next
    /// launch reports no crash; the creation finishing afterwards, or a new scope, writes nothing.
    #[test]
    fn an_orderly_quit_during_device_creation_removes_the_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-quit-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        let during = with_marker_in(&slot, &active, "device creation", || {
            assert!(m.exists(), "armed while the device is created");
            release_on(&slot, &active);
            m.exists()
        });
        assert!(!during, "the orderly quit removed the marker while creation was still running");
        assert!(!m.exists());
        assert_eq!(read_init_marker(&m), None, "the next launch finds nothing to report");
        // driver work starting after the quit writes no marker
        assert!(!with_marker_in(&slot, &active, "kernel compilation", || m.exists()));
        assert_eq!(*active.lock().unwrap(), 0);
        // releasing twice, or with no marker, is harmless
        release_on(&slot, &active);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_scopes_share_one_marker_until_the_last_ends() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-scopes-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        // exclusive: a directory planted at this predictable name fails the test instead of receiving the marker
        std::fs::create_dir(&dir).unwrap();
        let active = Mutex::new(0);
        arm(&m, &active, "device creation");
        assert!(read_init_marker(&m).unwrap().contains("device creation"));
        // a second scope (a denoise network's kernels) keeps the first's text and the file
        arm(&m, &active, "denoise kernel compilation");
        assert!(read_init_marker(&m).unwrap().contains("device creation"));
        disarm(&m, &active);
        assert!(m.exists(), "the first scope is still running");
        disarm(&m, &active);
        assert!(!m.exists(), "the last scope removes it");
        // a stray extra disarm does not underflow or break the next scope
        disarm(&m, &active);
        arm(&m, &active, "kernel compilation");
        assert!(m.exists());
        disarm(&m, &active);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_inside_the_scope_still_removes_the_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-panic-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        // exclusive: a directory planted at this predictable name fails the test instead of receiving the marker
        std::fs::create_dir(&dir).unwrap();
        let active = Mutex::new(0);
        let r = std::panic::catch_unwind(|| with_marker_in(&Mutex::new(Some(m.clone())), &active, "kernel compilation", || panic!("compile")));
        assert!(r.is_err());
        assert!(!m.exists(), "the process survived, so nothing is left to report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panicking_scope_keeps_other_active_work_marked() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-overlap-panic-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let active = Mutex::new(0);
        let slot = Mutex::new(Some(m.clone()));
        with_marker_in(&slot, &active, "device creation", || {
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_marker_in(&slot, &active, "kernel compilation", || panic!("compile"))));
            assert!(r.is_err());
            assert!(m.exists(), "other driver work is still active after the inner panic");
            assert_eq!(*active.lock().unwrap(), 1);
        });
        assert!(!m.exists(), "the last surviving scope removes the marker");
        assert_eq!(*active.lock().unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
