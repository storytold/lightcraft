//! Which wgpu backends LightCraft lets wgpu use — for its compute device and for the desktop
//! window (eframe) — and the crash sentinel around device creation (issue #136).
//!
//! wgpu loads the system driver of *every* backend in the instance's set while it enumerates
//! adapters, even when it then picks another one: with Vulkan in the set, a broken Vulkan driver
//! can crash the process (an access violation inside the driver DLL, which nothing can catch)
//! although DX12 would have been used. So Windows defaults to DX12 alone. macOS uses Metal, Linux
//! Vulkan (plus GL for the window when the build has it).
//!
//! Overrides, read once per process:
//! - `LIGHTCRAFT_GPU_BACKEND` = `dx12` | `vulkan` | `metal` | `gl` (comma lists allowed), `auto`
//!   (the platform default), or `off` (no GPU compute; the window still needs one backend and
//!   keeps the platform default);
//! - else `WGPU_BACKEND` (wgpu's own variable, same names) for both the window and compute.
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

/// The choice from `LIGHTCRAFT_GPU_BACKEND` (`lightcraft`) or else `WGPU_BACKEND` (`wgpu`): the
/// first one that parses wins; an unknown value is skipped (with a warning).
pub fn choose(lightcraft: Option<&str>, wgpu: Option<&str>) -> BackendChoice {
    for (name, v) in [("LIGHTCRAFT_GPU_BACKEND", lightcraft), ("WGPU_BACKEND", wgpu)] {
        let Some(v) = v else { continue };
        match parse_backends(v) {
            // WGPU_BACKEND is wgpu's variable: it selects backends, it never turns the GPU off
            Some(BackendChoice::Off) if name == "WGPU_BACKEND" => log::warn!("gpu: WGPU_BACKEND={v} ignored (use LIGHTCRAFT_GPU_BACKEND=off)"),
            Some(c) => return c,
            None => log::warn!("gpu: {name}={v} names no known backend; ignored"),
        }
    }
    BackendChoice::Auto
}

/// The backends compute devices use when nothing is asked for on `os` (`std::env::consts::OS`).
pub fn default_compute_backends(os: &str) -> Backends {
    match os {
        // never touch the Vulkan driver unless asked: see the module docs (issue #136)
        "windows" => Backends::DX12,
        "macos" | "ios" => Backends::METAL,
        _ => Backends::PRIMARY,
    }
}

/// The backends the window uses when nothing is asked for on `os`.
pub fn default_window_backends(os: &str) -> Backends {
    match os {
        "windows" => Backends::DX12,
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

fn env_choice() -> BackendChoice {
    static C: std::sync::OnceLock<BackendChoice> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        let lc = std::env::var("LIGHTCRAFT_GPU_BACKEND").ok();
        let wg = std::env::var("WGPU_BACKEND").ok();
        choose(lc.as_deref(), wg.as_deref())
    })
}

/// The backends of the compute device (`None`: `LIGHTCRAFT_GPU_BACKEND=off`).
pub fn compute_backends() -> Option<Backends> {
    resolve(env_choice(), default_compute_backends(std::env::consts::OS), wgpu::Instance::enabled_backend_features())
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
/// first — LightCraft ships none, so it is some other program's (an SDK's, a folder on `PATH`). A
/// copy without its `dxil.dll` beside it warns that the DXIL is unsigned; wgpu takes the warning for
/// a compile error, its own validation pipelines fail, the device is lost and the window never
/// opens. FXC is always there and compiles everything LightCraft's shaders need.
pub fn dx12_compiler(env: Option<&str>) -> wgpu::Dx12Compiler {
    let Some(v) = env else { return wgpu::Dx12Compiler::Fxc };
    v.parse().unwrap_or_else(|e| {
        log::warn!("gpu: WGPU_DX12_COMPILER={v} ignored: {e}");
        wgpu::Dx12Compiler::Fxc
    })
}

/// wgpu's backend options for every instance LightCraft creates — the window's and the compute
/// devices': wgpu's environment variables, with the DX12 compiler from [`dx12_compiler`].
pub fn backend_options() -> wgpu::BackendOptions {
    let mut o = wgpu::BackendOptions::from_env_or_default();
    o.dx12.shader_compiler = dx12_compiler(std::env::var("WGPU_DX12_COMPILER").ok().as_deref());
    o
}

/// wgpu's instance flags for every instance LightCraft creates, from `flags` (wgpu's defaults with its
/// environment variables applied) without `VALIDATION_INDIRECT_CALL`, unless
/// `WGPU_VALIDATION_INDIRECT_CALL` asks for it (any value but `0`). That validation builds an internal
/// compute pipeline when each device is created; Intel Macs' Metal compiler fails on it ("Compiler
/// encountered an internal error"), the device is lost and the window never opens (#250). It only
/// guards indirect draws and dispatches, which neither LightCraft nor egui issue.
pub fn instance_flags(flags: wgpu::InstanceFlags, env: Option<&str>) -> wgpu::InstanceFlags {
    if env.is_some_and(|v| v != "0") {
        flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    }
}

/// [`instance_flags`] with `WGPU_VALIDATION_INDIRECT_CALL` read from the environment.
pub fn instance_flags_from_env(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    instance_flags(flags, std::env::var("WGPU_VALIDATION_INDIRECT_CALL").ok().as_deref())
}

/// `LIGHTCRAFT_GPU_BACKEND=off`.
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
/// Metal compiler (`MTLCompilerService`) aborting on a kernel, issue #250: see
/// [`take_init_marker`]. `None` (the default) writes nothing.
pub fn set_init_marker(path: Option<PathBuf>) {
    *MARKER.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

/// The marker line naming the process that wrote it.
const OWNER_PREFIX: &str = "owner ";

/// This launch's owner token: the process id plus a per-launch random part, so that a second
/// LightCraft sharing the configuration folder (and so the marker file) never mistakes the other
/// instance's marker for its own, even after the operating system reuses a process id.
fn owner_token() -> &'static str {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        // `RandomState` is seeded randomly per process; the clock adds to it
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        h.write_u128(nanos);
        format!("{}-{:016x}", std::process::id(), h.finish())
    })
}

/// The marker's text: what started, when, and which launch wrote it (`owner`).
fn marker_text(what: &str, secs: u64, owner: &str) -> String {
    format!("GPU {what} started (unix time {secs})\n{OWNER_PREFIX}{owner}\n")
}

/// The marker text this launch writes when driver work `what` starts (owner line included), for
/// tests outside this crate that stand in for a device creation still running.
pub fn this_launch_marker_text(what: &str) -> String {
    marker_text(what, 1, owner_token())
}

/// Whether the marker text names `owner` as the launch that wrote it. A marker without an owner
/// line (written by an older LightCraft) belongs to no running launch.
fn marker_owned_by(text: &str, owner: &str) -> bool {
    text.lines().any(|l| l.trim().strip_prefix(OWNER_PREFIX) == Some(owner))
}

/// The lock file beside `marker` (`gpu-init.marker.lock`) that every LightCraft sharing the
/// configuration folder holds while it writes, checks or removes the marker. The scope mutexes
/// only order this process's threads; this orders the processes, so no instance can overwrite
/// the marker between another one's ownership check and its removal.
fn marker_lock_path(marker: &std::path::Path) -> PathBuf {
    let mut name = marker.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(".lock");
    marker.with_file_name(name)
}

/// Hold the cross-process marker lock (see [`marker_lock_path`]) until the returned file is
/// dropped. `None` (logged) when the lock file can't be opened or locked: the caller goes on
/// without it, as before the lock existed, rather than failing the GPU start or the quit.
fn lock_marker(marker: &std::path::Path) -> Option<std::fs::File> {
    let path = marker_lock_path(marker);
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("gpu: could not open {}: {e}", path.display());
            return None;
        }
    };
    match file.lock() {
        Ok(()) => Some(file),
        Err(e) => {
            log::warn!("gpu: could not lock {}: {e}", path.display());
            None
        }
    }
}

/// Remove `marker` if this launch still owns it. Another LightCraft using the same configuration
/// may have overwritten it while starting its own GPU device; that marker is left for it to clear
/// (or, if it dies in the driver, for the next launch to report). The check and the removal run
/// under the cross-process marker lock, which every marker write also takes. I/O errors are logged.
fn remove_if_owned(marker: &std::path::Path) {
    remove_if_owned_then(marker, || {});
}

/// [`remove_if_owned`], running `between` after the ownership check and before the removal (the
/// tests start another instance's write there).
fn remove_if_owned_then(marker: &std::path::Path, between: impl FnOnce()) {
    let _lock = lock_marker(marker);
    let text = match std::fs::read_to_string(marker) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            log::warn!("gpu: could not read {}: {e}", marker.display());
            return;
        }
    };
    if !marker_owned_by(&text, owner_token()) {
        log::info!("gpu: leaving {} in place: another LightCraft wrote it", marker.display());
        return;
    }
    between();
    if let Err(e) = std::fs::remove_file(marker)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("gpu: could not remove {}: {e}", marker.display());
    }
}

/// Write `text` to `marker` under the cross-process marker lock.
fn write_marker(marker: &std::path::Path, text: &str) {
    if let Some(d) = marker.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _lock = lock_marker(marker);
    if let Err(e) = std::fs::write(marker, text) {
        log::warn!("gpu: could not write {}: {e}", marker.display());
    }
}

/// Enter a driver-work scope on `marker`: the first one writes the file.
#[cfg(test)]
fn arm(marker: &std::path::Path, active: &Mutex<u32>, what: &str) {
    arm_locked(marker, &mut active.lock().unwrap_or_else(|e| e.into_inner()), what);
}

/// [`arm`] with the scope counter `n` already locked.
fn arm_locked(marker: &std::path::Path, n: &mut u32, what: &str) {
    if *n == 0 {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        write_marker(marker, &marker_text(what, secs, owner_token()));
    }
    *n += 1;
}

/// Leave a scope: the last one removes the file, if this launch still owns it.
fn disarm(marker: &std::path::Path, active: &Mutex<u32>) {
    let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
    *n = n.saturating_sub(1);
    if *n == 0 {
        remove_if_owned(marker);
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
pub(crate) fn with_marker<T>(what: &str, f: impl FnOnce() -> T) -> T {
    with_marker_in(&MARKER, &ACTIVE, what, f)
}

/// [`with_marker`] on an explicit marker slot and scope counter (the tests use their own, so that
/// device-creating tests running in parallel cannot join their scopes). The slot is read under the scope
/// lock, so a scope starting while [`release_on`] runs either is cleared by it or writes nothing.
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
/// A launch that quits a few seconds in — before the driver returned the device, e.g. while a
/// large library was still opening — used to leave the marker behind, so the next launch said
/// LightCraft had closed unexpectedly while starting the GPU and turned GPU rendering off. An
/// orderly quit proves the process wasn't killed inside the driver; a real crash or kill during
/// device creation never gets here, so its marker stays for the next launch to report.
pub fn release_init_marker_on_quit() {
    release_on(&MARKER, &ACTIVE);
}

/// [`release_init_marker_on_quit`] on an explicit marker slot and scope counter (for the tests).
fn release_on(slot: &Mutex<Option<PathBuf>>, active: &Mutex<u32>) {
    // the scope lock first, as `arm` / `disarm` take it: no scope writes the file while it goes
    let _n = active.lock().unwrap_or_else(|e| e.into_inner());
    let marker = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(m) = marker {
        // only our own: another instance may be inside device creation right now
        remove_if_owned(&m);
    }
}

/// Run device creation (and the kernel build that follows it) `f` under the init marker.
pub(crate) fn with_init_marker<T>(backends: Backends, f: impl FnOnce() -> T) -> T {
    with_marker(&format!("device and kernel creation (backends {backends:?})"), f)
}

/// If the init marker `path` is present — the last process died while creating the GPU device —
/// remove it and return what it recorded.
pub fn take_init_marker(path: &std::path::Path) -> Option<String> {
    let _lock = lock_marker(path);
    let text = read_init_marker(path)?;
    let _ = std::fs::remove_file(path);
    Some(text)
}

/// What the init marker `path` recorded, if it is present — without removing it. For sessions
/// that must leave the disk as they found it (`--memory`, issues #164 and #169): GPU rendering
/// still starts off, and the marker stays for the next ordinary launch to report and clear.
/// The owner line is left out of the description; a marker without one (an older LightCraft's)
/// still counts.
pub fn read_init_marker(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let what: Vec<&str> = text.lines().filter(|l| !l.trim().starts_with(OWNER_PREFIX)).collect();
    Some(what.join("\n").trim().to_string())
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
    /// The name used in `LIGHTCRAFT_GPU_BACKEND`, the remembered-renderer file and the logs.
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

/// The renderer the environment forces for the window: `LIGHTCRAFT_GPU_BACKEND` (`app`), else
/// `WGPU_BACKEND` (`wgpu`), when it names exactly one renderer.
pub fn forced_window_renderer(app: Option<&str>, wgpu: Option<&str>) -> Option<WindowRenderer> {
    app.and_then(WindowRenderer::parse).or_else(|| wgpu.and_then(WindowRenderer::parse))
}

/// The order in which the window's renderers are tried on `os`: the forced one (environment)
/// first, then the one remembered from the last good start, then the platform chain — on Windows
/// DX12, Vulkan, GL, then glow. DX12 stays first: with Vulkan loaded, a broken Vulkan driver can
/// crash the process uncatchably (issue #136), so Vulkan is touched only after DX12 failed.
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
        "windows" => vec![Backends::DX12, Backends::VULKAN, Backends::GL],
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
    fn parses_backend_names() {
        assert_eq!(parse_backends("dx12"), Some(BackendChoice::Use(Backends::DX12)));
        assert_eq!(parse_backends(" D3D12 "), Some(BackendChoice::Use(Backends::DX12)));
        assert_eq!(parse_backends("vulkan"), Some(BackendChoice::Use(Backends::VULKAN)));
        assert_eq!(parse_backends("vk,gl"), Some(BackendChoice::Use(Backends::VULKAN | Backends::GL)));
        assert_eq!(parse_backends("metal"), Some(BackendChoice::Use(Backends::METAL)));
        assert_eq!(parse_backends("auto"), Some(BackendChoice::Auto));
        for off in ["off", "OFF", "none", "cpu", "0"] {
            assert_eq!(parse_backends(off), Some(BackendChoice::Off), "{off}");
        }
        assert_eq!(parse_backends(""), None);
        assert_eq!(parse_backends("directx9"), None);
    }

    #[test]
    fn lightcraft_variable_wins_over_wgpu_backend() {
        assert_eq!(choose(None, None), BackendChoice::Auto);
        assert_eq!(choose(None, Some("dx12")), BackendChoice::Use(Backends::DX12));
        assert_eq!(choose(Some("vulkan"), Some("dx12")), BackendChoice::Use(Backends::VULKAN));
        assert_eq!(choose(Some("off"), Some("dx12")), BackendChoice::Off);
        // unknown LightCraft value: WGPU_BACKEND still applies
        assert_eq!(choose(Some("bogus"), Some("gl")), BackendChoice::Use(Backends::GL));
        // WGPU_BACKEND never turns the GPU off
        assert_eq!(choose(None, Some("off")), BackendChoice::Auto);
    }

    /// Issue #136: Windows must not load the Vulkan driver unless asked to.
    #[test]
    fn windows_defaults_to_dx12_only() {
        assert_eq!(default_compute_backends("windows"), Backends::DX12);
        assert_eq!(default_window_backends("windows"), Backends::DX12);
        assert!(!default_window_backends("windows").contains(Backends::VULKAN));
        assert_eq!(default_compute_backends("macos"), Backends::METAL);
        assert!(default_compute_backends("linux").contains(Backends::VULKAN));
        assert!(default_window_backends("linux").contains(Backends::VULKAN | Backends::GL));
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

    /// An orderly quit while the device is still being created (a launch closed a few seconds in,
    /// before the driver returned) removes the marker, so the next launch reports no crash; the
    /// creation finishing afterwards, or a new scope, writes nothing more. Before the fix the marker
    /// stayed, and the next launch turned GPU rendering off.
    #[test]
    fn an_orderly_quit_during_device_creation_removes_the_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-quit-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        let during = with_marker_in(&slot, &active, "device creation", || {
            assert!(m.exists(), "armed while the device is created");
            // the user quits before the driver returned
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

    /// Two LightCraft instances share one configuration (and so one marker file). Instance A arms,
    /// then instance B starts its own device creation and overwrites the marker; A's orderly quit,
    /// or A's scope ending, must leave B's marker so a crash of B is still reported. Before the
    /// fix A removed B's marker. A alone still removes its own.
    #[test]
    fn an_orderly_quit_leaves_another_instances_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-other-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let b_text = marker_text("device creation", 1, "99999-00000000deadbeef");
        // the orderly quit while B owns the file
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        with_marker_in(&slot, &active, "device creation", || {
            assert!(marker_owned_by(&std::fs::read_to_string(&m).unwrap(), owner_token()), "A wrote its token");
            std::fs::write(&m, &b_text).unwrap(); // instance B arms
            release_on(&slot, &active);
        });
        assert_eq!(std::fs::read_to_string(&m).unwrap(), b_text, "B's marker stays after A's quit and scope end");
        // A's scope ending (no quit) while B owns the file
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        with_marker_in(&slot, &active, "kernel compilation", || std::fs::write(&m, &b_text).unwrap());
        assert_eq!(std::fs::read_to_string(&m).unwrap(), b_text, "B's marker stays after A's scope ends");
        // the description the next launch reports leaves the owner line out
        assert_eq!(read_init_marker(&m).unwrap(), "GPU device creation started (unix time 1)");
        // A alone: its quit removes its own marker
        std::fs::remove_file(&m).unwrap();
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        with_marker_in(&slot, &active, "device creation", || release_on(&slot, &active));
        assert!(!m.exists(), "A's own marker is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The race the ownership check alone left open: instance A reads the marker and finds its own
    /// token, instance B then writes its marker, and A removes the file, B's marker with it. B's
    /// write now waits for the cross-process lock A holds from the check to the removal, so B's
    /// marker is written after A's removal and stays. Before the lock, B's write landed in between
    /// (the thread signals it within the wait below) and A deleted it.
    #[test]
    fn another_instance_writing_between_the_check_and_the_removal_keeps_its_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-race-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let b_text = marker_text("device creation", 1, "99999-00000000deadbeef");
        let active = Mutex::new(0);
        arm(&m, &active, "device creation");
        assert!(marker_owned_by(&std::fs::read_to_string(&m).unwrap(), owner_token()), "A wrote its token");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut b = None;
        remove_if_owned_then(&m, || {
            // instance B arms on its own handle of the lock file (a separate process in real use)
            let (m, b_text) = (m.clone(), b_text.clone());
            b = Some(std::thread::spawn(move || {
                write_marker(&m, &b_text);
                let _ = tx.send(());
            }));
            // without the lock B's write lands now; with it, B waits until A's removal is done
            let _ = rx.recv_timeout(std::time::Duration::from_millis(300));
        });
        b.unwrap().join().unwrap();
        assert_eq!(std::fs::read_to_string(&m).unwrap(), b_text, "B's marker, written after A's check, stays");
        assert!(marker_lock_path(&m).ends_with("gpu-init.marker.lock"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The crash case is unchanged: a process that dies inside device creation never runs the
    /// orderly quit, so the marker stays for the next launch.
    #[test]
    fn a_crash_during_device_creation_keeps_the_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-killed-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let (slot, active) = (Mutex::new(Some(m.clone())), Mutex::new(0));
        // armed, then the process "dies": the scope never ends and no quit runs
        let n = &mut active.lock().unwrap();
        let marker = slot.lock().unwrap().clone().unwrap();
        arm_locked(&marker, n, "device creation");
        assert!(read_init_marker(&m).unwrap().contains("device creation"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_init_marker_writes_the_process_wide_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-global-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        set_init_marker(Some(m.clone()));
        let seen = with_init_marker(Backends::DX12, || m.exists());
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
        assert_eq!(window_renderer_order(None, None, None, "windows", all, true), [dx, vk, gl, Glow]);
        assert_eq!(window_renderer_order(None, None, None, "windows", all, false), [dx, vk, gl]);
        // a build without GL
        assert_eq!(window_renderer_order(None, None, None, "windows", Backends::DX12 | Backends::VULKAN, true), [dx, vk, Glow]);
        // remembered from the last good start goes first
        assert_eq!(window_renderer_order(None, Some(vk), None, "windows", all, true), [vk, dx, gl, Glow]);
        // the environment wins over the remembered one
        assert_eq!(window_renderer_order(Some(gl), Some(vk), None, "windows", all, true), [gl, vk, dx, Glow]);
        assert_eq!(window_renderer_order(Some(Glow), None, None, "windows", all, true)[0], Glow);
        // the last start died in DX12: tried last only (also when it was remembered)
        assert_eq!(window_renderer_order(None, Some(dx), Some(dx), "windows", all, true), [vk, gl, Glow, dx]);
        // a forced backend that is not in the build is skipped
        assert_eq!(window_renderer_order(Some(Wgpu(Backends::METAL)), None, None, "windows", Backends::DX12, false), [dx]);
        // never empty
        assert!(!window_renderer_order(None, None, None, "windows", Backends::empty(), false).is_empty());
        assert_eq!(window_renderer_order(None, None, None, "macos", all, true), [Wgpu(Backends::METAL), Glow]);
    }

    #[test]
    fn window_renderer_names_round_trip() {
        for n in ["dx12", "vulkan", "metal", "gl", "glow"] {
            assert_eq!(WindowRenderer::parse(n).map(WindowRenderer::name), Some(n));
        }
        assert_eq!(WindowRenderer::parse(" DX12 "), Some(WindowRenderer::Wgpu(Backends::DX12)));
        for n in ["auto", "off", "vulkan,gl", "", "bogus"] {
            assert_eq!(WindowRenderer::parse(n), None, "{n}");
        }
        assert_eq!(forced_window_renderer(Some("glow"), Some("dx12")), Some(WindowRenderer::Glow));
        assert_eq!(forced_window_renderer(Some("auto"), Some("vulkan")), Some(WindowRenderer::Wgpu(Backends::VULKAN)));
        assert_eq!(forced_window_renderer(None, None), None);
        // `glow` only concerns the window: compute keeps its default, without a warning
        assert_eq!(parse_backends("glow"), Some(BackendChoice::Auto));
    }

    #[test]
    fn overlapping_scopes_share_one_marker_until_the_last_ends() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-scopes-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
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
        let active = Mutex::new(0);
        let r = std::panic::catch_unwind(|| with_marker_in(&Mutex::new(Some(m.clone())), &active, "kernel compilation", || panic!("compile")));
        assert!(r.is_err());
        assert!(!m.exists(), "the process survived, so nothing is left to report");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
