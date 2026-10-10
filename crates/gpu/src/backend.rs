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

/// The marker's text: what started, and when.
fn marker_text(what: &str, secs: u64) -> String {
    format!("GPU {what} started (unix time {secs})\n")
}

/// Enter a driver-work scope on `marker`: the first one writes the file.
fn arm(marker: &std::path::Path, active: &Mutex<u32>, what: &str) {
    let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
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
struct Armed(Option<PathBuf>);

impl Drop for Armed {
    fn drop(&mut self) {
        if let Some(m) = &self.0 {
            disarm(m, &ACTIVE);
        }
    }
}

/// Run driver work `f` (`what`: "device creation", "kernel compilation"…) between writing and
/// removing the init marker (if one is set).
pub(crate) fn with_marker<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let marker = MARKER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(m) = &marker {
        arm(m, &ACTIVE, what);
    }
    let _armed = Armed(marker);
    f()
}

/// Run device creation (and the kernel build that follows it) `f` under the init marker.
pub(crate) fn with_init_marker<T>(backends: Backends, f: impl FnOnce() -> T) -> T {
    with_marker(&format!("device and kernel creation (backends {backends:?})"), f)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The tests that set the process-wide marker take turns.
    static GLOBAL: Mutex<()> = Mutex::new(());

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
        let _turn = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lc-gpu-marker-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        set_init_marker(Some(m.clone()));
        let seen = with_init_marker(Backends::DX12, || m.exists());
        set_init_marker(None);
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
        let _turn = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lc-gpu-panic-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        set_init_marker(Some(m.clone()));
        let r = std::panic::catch_unwind(|| with_marker("kernel compilation", || panic!("compile")));
        set_init_marker(None);
        assert!(r.is_err());
        assert!(!m.exists(), "the process survived, so nothing is left to report");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
