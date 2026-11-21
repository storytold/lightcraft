//! The window's renderer and its fallback chain (crash report: on some Windows machines FXC fails
//! to compile egui's own shaders on DX12 — "The parameter is incorrect. (0x80070057)" in
//! `egui_pipeline` — and wgpu's default error handler panicked, killing the app at start-up).
//!
//! - **Windows:** each renderer of [`backend::window_renderer_order`] (DX12, Vulkan, GL, then
//!   eframe's glow) is *probed* before the window opens: a device is created with an error handler
//!   that logs instead of panicking, and egui's pipeline is built inside error scopes. The first
//!   that builds cleanly is handed to eframe as an existing device (so its error handler stays).
//! - **Everywhere:** a marker file names the renderer being started and is emptied once the
//!   window's renderer is up. Left behind — the start died or panicked — the next launch tries that
//!   renderer last; [`relaunch_after_failure`] starts that next launch right away. The copy that
//!   starts holds an OS lock on the marker, so a second copy starting at the same time sees a live
//!   start, not a dead one, and leaves the marker alone; a crash or kill releases the lock. A copy
//!   whose start hangs without dying still holds it: end it to clear the way.
//! - The renderer that worked is remembered (`<config>/window-renderer`, Windows only) and tried
//!   first next time. `LIGHTCRAFT_GPU_BACKEND=dx12|vulkan|gl|glow` (or `WGPU_BACKEND`) forces one.

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupExisting};
use eframe::wgpu;
use lightcraft_engine::gpu::backend::{self, Backends, WindowRenderer};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Seek, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Environment variable counting automatic relaunches after a failed start (bounds the chain).
const RETRY_VAR: &str = "LIGHTCRAFT_WINDOW_RETRY";
/// Never more relaunches than renderers to try.
const MAX_RETRIES: u32 = 4;

/// The window's renderer for this start.
pub struct Chosen {
    pub renderer: eframe::Renderer,
    pub wgpu_options: WgpuConfiguration,
    pub which: WindowRenderer,
    /// For the user: the last start failed with another renderer, or one failed now.
    pub notice: Option<String>,
}

/// Files under the settings folder (`None` dir: write nothing — `--memory`, `LIGHTCRAFT_NO_PREFS`).
pub struct Files {
    dir: Option<PathBuf>,
    /// The marker this copy armed, with its lock held: the lock says "a start is in flight here".
    held: Mutex<Option<File>>,
}

impl Files {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, held: Mutex::new(None) }
    }
    fn held(&self) -> MutexGuard<'_, Option<File>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn marker(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("window-init.marker"))
    }
    fn remembered(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("window-renderer"))
    }
    fn read(p: Option<PathBuf>) -> Option<WindowRenderer> {
        std::fs::read_to_string(p?).ok().as_deref().and_then(WindowRenderer::parse)
    }
    fn write(p: Option<PathBuf>, r: WindowRenderer) {
        let Some(p) = p else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // on disk before the driver is touched: a driver fault can take the machine down with it
        let written = std::fs::File::create(&p).and_then(|mut f| {
            std::io::Write::write_all(&mut f, format!("{}\n", r.name()).as_bytes())?;
            f.sync_all()
        });
        if let Err(e) = written {
            log::warn!("window: could not write {}: {e}", p.display());
        }
    }
    /// The renderer the last start died in (marker left behind), clearing the marker. A marker
    /// whose lock another running copy holds is that copy's start in flight: not a failure, not
    /// touched. A marker without a lock holder (a crash, an older version's) is a failed start.
    pub fn take_failed(&self) -> Option<WindowRenderer> {
        let m = self.marker()?;
        // a marker that can't be written is still read (and `clear` deletes it if it can't be emptied)
        let mut f = OpenOptions::new().read(true).write(true).open(&m).or_else(|_| File::open(&m)).ok()?;
        match f.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return None,
            // a file system without locks: read it as before
            Err(TryLockError::Error(e)) => log::warn!("window: can't lock {}: {e}", m.display()),
        }
        // through the locked handle: on Windows a lock refuses reads through any other
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut f, &mut text);
        if !text.is_empty() {
            Self::clear(&f, Some(m));
        }
        let _ = f.unlock();
        WindowRenderer::parse(&text)
    }
    /// Empty the marker through its locked handle; only if that fails, delete it.
    fn clear(f: &File, path: Option<PathBuf>) {
        if f.set_len(0).is_err()
            && let Some(p) = path
        {
            let _ = std::fs::remove_file(p);
        }
    }
    /// Before the window opens: record what is being started, holding the marker's lock until
    /// the start finishes. Another copy's start in flight keeps its marker (this one writes none).
    pub fn arm(&self, r: WindowRenderer) {
        let Some(p) = self.marker() else { return };
        let mut held = self.held();
        if held.is_none() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let f = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&p) {
                Ok(f) => f,
                Err(e) => return log::warn!("window: could not write {}: {e}", p.display()),
            };
            match f.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return log::info!("window: another copy is starting its window; no marker written"),
                Err(TryLockError::Error(e)) => log::warn!("window: can't lock {}: {e}", p.display()),
            }
            *held = Some(f);
        }
        let Some(f) = held.as_mut() else { return };
        // on disk before the driver is touched: a driver fault can take the machine down with it
        let written =
            f.set_len(0).and_then(|()| f.rewind()).and_then(|()| f.write_all(format!("{}\n", r.name()).as_bytes())).and_then(|()| f.sync_all());
        if let Err(e) = written {
            log::warn!("window: could not write {}: {e}", p.display());
        }
    }
    /// The window's renderer is up: empty the marker (it is never deleted, so every copy locks the
    /// same file; an empty one names no failed start) and (Windows) remember the renderer.
    pub fn started(&self, r: WindowRenderer) {
        if let Some(f) = self.held().take() {
            Self::clear(&f, self.marker());
            let _ = f.unlock();
        }
        if cfg!(windows) {
            Self::write(self.remembered(), r);
        }
    }
    /// Whether this copy armed the marker and the start has not come up.
    pub fn armed(&self) -> bool {
        self.held().is_some()
    }
    /// Hand the marker on: keep the file, drop the lock, so the copy started next reads it as the
    /// failed start (this copy is still alive, and would otherwise look like a start in flight).
    fn release_for_relaunch(&self) {
        if let Some(f) = self.held().take() {
            let _ = f.unlock();
        }
    }
}

/// The renderer for this start: see the module docs.
pub fn choose(files: &Files, compose_window: bool) -> Chosen {
    let forced =
        backend::forced_window_renderer(std::env::var("LIGHTCRAFT_GPU_BACKEND").ok().as_deref(), std::env::var("WGPU_BACKEND").ok().as_deref());
    let failed = files.take_failed();
    let remembered = if cfg!(windows) { Files::read(files.remembered()) } else { None };
    let compiled = wgpu::Instance::enabled_backend_features();
    let os = std::env::consts::OS;
    let order = backend::window_renderer_order(forced, remembered, failed, os, compiled, true);
    let mut problems = Vec::new();
    let chosen = if cfg!(windows) {
        pick_probed(files, &order, &mut problems, compose_window)
    } else if forced == Some(WindowRenderer::Glow) || (forced.is_none() && failed.is_some_and(|f| f != WindowRenderer::Glow)) {
        // elsewhere eframe's own wgpu set-up stays the default; glow after a failed start
        Some((WindowRenderer::Glow, None))
    } else {
        Some((WindowRenderer::Wgpu(backend::window_backends()), None))
    };
    let (which, existing) = chosen.unwrap_or((WindowRenderer::Glow, None));
    let notice = user_notice(failed, forced, &problems, which);
    log::info!("window: renderer {} (order {:?})", which.name(), order.iter().map(|r| r.name()).collect::<Vec<_>>());
    let mut wgpu_options = base_options();
    let renderer = match (which, existing) {
        (WindowRenderer::Glow, _) => eframe::Renderer::Glow,
        (WindowRenderer::Wgpu(_), Some(e)) => {
            wgpu_options.wgpu_setup = WgpuSetup::Existing(e);
            eframe::Renderer::Wgpu
        }
        (WindowRenderer::Wgpu(b), None) => {
            if let WgpuSetup::CreateNew(n) = &mut wgpu_options.wgpu_setup {
                n.instance_descriptor.backends = b;
            }
            eframe::Renderer::Wgpu
        }
    };
    Chosen { renderer, wgpu_options, which, notice }
}

/// What to tell the user about the window's renderer. A renderer that failed its probe is skipped
/// silently (it is logged, and the one that worked is remembered): the app works, so a pop-up with
/// wgpu's error only alarms (#1624). Shown: the last start died, or the forced renderer failed.
fn user_notice(failed: Option<WindowRenderer>, forced: Option<WindowRenderer>, problems: &[String], which: WindowRenderer) -> Option<String> {
    let mut notice = failed.map(|f| format!("The window could not start with its {} renderer last time.", f.name()));
    if forced.is_some() && !problems.is_empty() {
        notice = Some(format!("{} {}", notice.unwrap_or_default(), problems.join(" ")).trim().to_string());
    }
    notice.map(|n| {
        format!(
            "{n} {app} now draws its window with {}. To choose another, start {app} with {var}=dx12, vulkan, gl or glow \
             (see docs/gpu-pipeline.md → Troubleshooting).",
            which.name(),
            app = "LightCraft",
            var = "LIGHTCRAFT_GPU_BACKEND",
        )
    })
}

/// eframe's defaults with the app's backends and DX12 shader compiler (issues #136, #471).
///
/// Taken from `NativeOptions::default()`, whose surface is egui-wgpu's `LOW_LATENCY` (one frame in
/// flight; two drawables on Metal). `WgpuConfiguration::default()` is `HIGH_THROUGHPUT` instead (two
/// frames queued, three drawables): every drag, slider and crop handle reached the screen a frame
/// later than eframe's default. Live window resizing still gets three drawables from egui-wgpu.
pub fn base_options() -> WgpuConfiguration {
    let mut c = eframe::NativeOptions::default().wgpu_options;
    if let WgpuSetup::CreateNew(n) = &mut c.wgpu_setup {
        n.instance_descriptor.backends = backend::window_backends();
        n.instance_descriptor.backend_options = backend::backend_options();
        // no indirect-call validation pipeline: Intel Metal fails to compile it and the window never opens (#250)
        n.instance_descriptor.flags = backend::instance_flags_from_env(n.instance_descriptor.flags);
        // the GPU renderer may run on this device (`adopt_window_device`): it needs the adapter's
        // buffer limits (a 24 MP photo is a 275 MiB buffer), not wgpu's defaults
        let default = n.device_descriptor.clone();
        n.device_descriptor = std::sync::Arc::new(move |adapter| {
            if adapter.get_info().backend == wgpu::Backend::Gl {
                return default(adapter);
            }
            wgpu::DeviceDescriptor { label: Some("lightcraft window"), required_limits: adapter.limits(), ..Default::default() }
        });
    }
    c
}

/// The first renderer of `order` that probes cleanly (glow is taken without a probe).
fn pick_probed(
    files: &Files,
    order: &[WindowRenderer],
    problems: &mut Vec<String>,
    compose_window: bool,
) -> Option<(WindowRenderer, Option<WgpuSetupExisting>)> {
    for &r in order {
        match r {
            WindowRenderer::Glow => return Some((r, None)),
            WindowRenderer::Wgpu(b) => {
                // name the renderer being probed before the crash-prone device
                // creation and egui pipeline compile: if the driver kills the
                // start, the marker survives so the next launch tries it last
                files.arm(r);
                let probed = std::panic::catch_unwind(|| probe(b, compose_window)).unwrap_or_else(|_| Err("panicked".into()));
                match probed {
                    Ok(e) => return Some((r, Some(e))),
                    Err(e) => {
                        log::error!("window: the {} renderer failed: {e}", r.name());
                        problems.push(format!("The {} renderer failed ({e}).", r.name()));
                    }
                }
            }
        }
    }
    None
}

/// Create a device on `backends` and build egui's pipeline on it; any wgpu error is returned.
fn probe(backends: Backends, compose_window: bool) -> Result<WgpuSetupExisting, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: backend::instance_flags_from_env(wgpu::InstanceFlags::from_build_config().with_env()),
        backend_options: presentation_options(backends, compose_window),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
        ..Default::default()
    }))
    .map_err(|e| format!("no adapter: {e}"))?;
    // (not GL: the adapter's own limits, which the GPU renderer needs when it adopts this device)
    let base = if adapter.get_info().backend == wgpu::Backend::Gl { wgpu::Limits::downlevel_webgl2_defaults() } else { adapter.limits() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("egui wgpu device"),
        // (GL takes the conservative defaults for everything else, whose 2048 is too small for a window)
        required_limits: wgpu::Limits { max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d, ..base },
        ..Default::default()
    }))
    .map_err(|e| format!("{}: no device: {e}", adapter.get_info().name))?;
    install_error_handler(&device);
    check_pipeline(&device, |d| {
        drop(eframe::egui_wgpu::Renderer::new(d, wgpu::TextureFormat::Bgra8Unorm, eframe::egui_wgpu::RendererOptions::default()));
    })?;
    Ok(WgpuSetupExisting { instance, adapter, device, queue })
}

/// wgpu's default handler for errors outside an error scope panics ("Handling wgpu errors as
/// fatal by default"); this one logs.
pub fn install_error_handler(device: &wgpu::Device) {
    device.on_uncaptured_error(std::sync::Arc::new(|e| log::error!("window: wgpu error: {e}")));
}

/// Run `build` inside validation, internal and out-of-memory error scopes: the first error is
/// returned instead of reaching the uncaptured-error handler.
pub fn check_pipeline(device: &wgpu::Device, build: impl FnOnce(&wgpu::Device)) -> Result<(), String> {
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    build(device);
    let errors = [pollster::block_on(validation.pop()), pollster::block_on(internal.pop()), pollster::block_on(oom.pop())];
    match errors.into_iter().flatten().next() {
        Some(e) => Err(e.to_string()),
        None => Ok(()),
    }
}

/// The start failed (error or panic) before the window's renderer came up: start the app again,
/// which then tries the next renderer. `false`: not relaunched (it came up, nothing written to
/// disk, or the retries are used up).
pub fn relaunch_after_failure(files: &Files) -> bool {
    if !files.armed() {
        return false;
    }
    // this start failed: the marker now belongs to the next one (relaunched, or started later while
    // this copy shows its error), and a lock still held here would hide it from that one
    files.release_for_relaunch();
    let tries = std::env::var(RETRY_VAR).ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    if tries >= MAX_RETRIES {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    log::warn!("window: the renderer failed to start; starting again with the next one");
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(RETRY_VAR, (tries + 1).to_string())
        .spawn()
        .map_err(|e| log::error!("window: could not start again: {e}"))
        .is_ok()
}

/// Only DX12 windows use the compositor; fallback renderers retain their normal presentation.
/// Adapted from kmnkv1's PhotoCraft PR: https://github.com/storytold/photocraft/pull/7035.
pub fn composes_window(enabled: bool, renderer: WindowRenderer) -> bool {
    cfg!(target_os = "windows") && enabled && renderer == WindowRenderer::Wgpu(Backends::DX12)
}

/// Select DirectComposition for opted-in Windows DX12 windows, preserving
/// the default presentation options for other renderers.
fn presentation_options(backends: Backends, enabled: bool) -> wgpu::BackendOptions {
    let mut options = backend::backend_options();
    if composes_window(enabled, WindowRenderer::Wgpu(backends)) {
        options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    options
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("app-window-gpu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Bytes in the marker, `None` when the file is gone (metadata, not a read: on Windows a held
    /// lock refuses reads).
    fn marker_len(f: &Files) -> Option<u64> {
        std::fs::metadata(f.marker().unwrap()).ok().map(|m| m.len())
    }

    /// The marker names the renderer being started; left behind by a copy that is gone, the next
    /// start reads it as the failed one (and clears it); a good start clears it and remembers
    /// the renderer (Windows).
    #[test]
    fn marker_records_the_failed_renderer() {
        let d = dir("marker");
        let f = Files::new(Some(d.clone()));
        let dx = WindowRenderer::Wgpu(Backends::DX12);
        f.arm(dx);
        assert!(f.armed() && marker_len(&f) > Some(0));
        // the start died: the OS releases its lock, the marker stays (unlocked by hand: a sibling
        // test spawning a process can hold a copy of our descriptor, and a flock lasts while one is open)
        f.release_for_relaunch();
        drop(f);
        let next = Files::new(Some(d.clone()));
        assert_eq!(next.take_failed(), Some(dx));
        assert_eq!(marker_len(&next), Some(0), "taken: the marker is emptied, not deleted");
        assert_eq!(next.take_failed(), None);
        next.arm(WindowRenderer::Glow);
        next.started(WindowRenderer::Glow);
        assert!(!next.armed() && marker_len(&next) == Some(0));
        assert_eq!(Files::read(next.remembered()), cfg!(windows).then_some(WindowRenderer::Glow));
        // nothing written without a settings folder
        let none = Files::new(None);
        none.arm(dx);
        assert!(!none.armed() && !relaunch_after_failure(&none));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A second copy started on the same settings folder while the first is still starting finds
    /// the first's marker: that is a start in flight, not a failure. It leaves the marker (and
    /// the first's later clean start) alone, and writes none of its own.
    #[test]
    fn a_marker_of_a_live_copy_is_not_a_failure() {
        let d = dir("live-marker");
        let first = Files::new(Some(d.clone()));
        let dx = WindowRenderer::Wgpu(Backends::DX12);
        first.arm(dx);
        let second = Files::new(Some(d.clone()));
        assert_eq!(second.take_failed(), None, "the first copy is still starting");
        assert!(marker_len(&second) > Some(0), "its marker is not cleared");
        second.arm(WindowRenderer::Glow);
        assert!(!second.armed(), "the second copy does not take over the first's marker");
        second.started(WindowRenderer::Glow);
        assert!(marker_len(&second) > Some(0), "nor does its start clear it");
        first.started(dx);
        assert_eq!(marker_len(&first), Some(0));
        assert_eq!(Files::new(Some(d.clone())).take_failed(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The relaunch after a failed start hands the marker on while the failed copy is still alive:
    /// the copy it starts reads the failed renderer from it.
    #[test]
    fn the_relaunched_copy_reads_the_marker_of_its_failed_parent() {
        let d = dir("relaunch-marker");
        let parent = Files::new(Some(d.clone()));
        let dx = WindowRenderer::Wgpu(Backends::DX12);
        parent.arm(dx);
        parent.release_for_relaunch();
        assert!(!parent.armed());
        assert_eq!(Files::new(Some(d.clone())).take_failed(), Some(dx));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A marker that can be read but not written (its permissions changed, a settings folder
    /// copied from another account) still names the failed start: the next start does not retry
    /// the renderer that died.
    #[test]
    fn a_read_only_marker_still_names_the_failed_start() {
        let d = dir("read-only-marker");
        let first = Files::new(Some(d.clone()));
        let metal = WindowRenderer::Wgpu(Backends::METAL);
        first.arm(metal);
        first.release_for_relaunch();
        drop(first);
        let path = d.join("window-init.marker");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();
        assert_eq!(Files::new(Some(d.clone())).take_failed(), Some(metal));
        if let Ok(m) = std::fs::metadata(&path) {
            let mut perms = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&path, perms);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #1624: a renderer skipped by the probe (DX12 → Vulkan) is no pop-up; a died start or a
    /// failed forced renderer still is.
    #[test]
    fn probe_fallback_is_silent() {
        let (dx, vk) = (WindowRenderer::Wgpu(Backends::DX12), WindowRenderer::Wgpu(Backends::VULKAN));
        let problems = vec!["The dx12 renderer failed (Validation Error).".to_string()];
        assert_eq!(user_notice(None, None, &problems, vk), None);
        assert_eq!(user_notice(None, None, &[], vk), None);
        let forced = user_notice(None, Some(dx), &problems, vk).unwrap();
        assert!(forced.starts_with("The dx12 renderer failed") && forced.contains("now draws its window with vulkan"), "{forced}");
        let died = user_notice(Some(dx), None, &problems, vk).unwrap();
        assert!(died.contains("last time") && !died.contains("Validation Error"), "{died}");
    }

    /// The window keeps eframe's low-latency surface: one frame queued (two Metal drawables), not
    /// the high-throughput `WgpuConfiguration::default()`, which held input a frame longer. The
    /// probed (Windows) and forced-backend configurations are built on the same options.
    #[test]
    fn window_surface_keeps_one_frame_in_flight() {
        let surface = base_options().surface;
        assert_eq!(surface, eframe::NativeOptions::default().wgpu_options.surface);
        assert_eq!(surface, eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY);
        assert_eq!(surface.desired_maximum_frame_latency, Some(1));
        assert_eq!(surface.present_mode, wgpu::PresentMode::AutoVsync);
        assert_ne!(surface, WgpuConfiguration::default().surface, "egui-wgpu's own default is the high-throughput one");
        // what `main` hands eframe (on Windows `choose` probes the adapters first; not here)
        if !cfg!(windows) {
            assert_eq!(choose(&Files::new(None), false).wgpu_options.surface, surface);
        }
    }

    /// The crash report: a pipeline that fails validation must come back as an error, never as
    /// wgpu's default panic. Needs an adapter; skipped where there is none (CI without a GPU).
    #[test]
    fn pipeline_errors_are_captured_not_panics() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: backend::window_backends(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let Ok((device, _queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        install_error_handler(&device);
        // a shader that does not validate, then a pipeline using it (like a failed FXC compile)
        let r = check_pipeline(&device, |d| {
            let m = d.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("broken"),
                source: wgpu::ShaderSource::Wgsl("@vertex fn vs_main() -> @builtin(position) vec4<f32> { return undefined_thing; }".into()),
            });
            let _ = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("egui_pipeline"),
                layout: None,
                vertex: wgpu::VertexState { module: &m, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: None,
                multiview_mask: None,
                cache: None,
            });
        });
        assert!(r.is_err(), "{r:?}");
        // egui's own pipeline builds cleanly here
        check_pipeline(&device, |d| {
            drop(eframe::egui_wgpu::Renderer::new(d, wgpu::TextureFormat::Bgra8Unorm, eframe::egui_wgpu::RendererOptions::default()));
        })
        .unwrap();
        // outside a scope: logged by our handler, not a panic
        let _ = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl("not wgsl".into()) });
    }

    /// The probed device keeps the adapter's own `max_texture_dimension_2d`, so a window
    /// ballooned past 8192 px can still configure its surface. Needs an adapter; skipped
    /// where there is none (CI without a GPU).
    #[test]
    fn probed_device_uses_adapter_max_texture_size() {
        let Ok(setup) = probe(backend::window_backends(), false) else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        assert_eq!(
            setup.device.limits().max_texture_dimension_2d,
            setup.adapter.limits().max_texture_dimension_2d,
            "the probed device must use the adapter's own max_texture_dimension_2d, not a fixed 8192 cap",
        );
    }

    /// The workaround is opt-in and changes presentation only for Windows DX12.
    #[test]
    fn composition_only_changes_windows_dx12_presentation() {
        let legacy: lightcraft_ui_egui::state::AppSettings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.compose_window, "existing settings keep the workaround off until opted in");
        assert!(!lightcraft_ui_egui::state::AppSettings::default().compose_window);
        let enabled: lightcraft_ui_egui::state::AppSettings = serde_json::from_str(r#"{"composeWindow":true}"#).unwrap();
        let saved = serde_json::to_value(&enabled).unwrap();
        assert_eq!(saved["composeWindow"], true);
        for backend in [Backends::DX12, Backends::VULKAN, Backends::GL] {
            let wanted = cfg!(target_os = "windows") && backend == Backends::DX12;
            assert_eq!(composes_window(true, WindowRenderer::Wgpu(backend)), wanted);
            assert_eq!(presentation_options(backend, true).dx12.presentation_system == wgpu::Dx12SwapchainKind::DxgiFromVisual, wanted);
            assert!(!composes_window(false, WindowRenderer::Wgpu(backend)));
        }
        assert!(!composes_window(true, WindowRenderer::Glow));
    }

    /// The probe builds egui's pipeline on a live device before the window opens; a driver crash
    /// there must leave the window-init marker on disk so the next launch tries another renderer.
    #[cfg(windows)]
    #[test]
    fn the_window_marker_is_written_while_the_renderer_is_probed() {
        // glow is taken without a probe, so a forced glow arms nothing here
        let forced =
            backend::forced_window_renderer(std::env::var("LIGHTCRAFT_GPU_BACKEND").ok().as_deref(), std::env::var("WGPU_BACKEND").ok().as_deref());
        if forced == Some(WindowRenderer::Glow) {
            return;
        }
        let d = dir("probe-marker");
        let f = Files::new(Some(d.clone()));
        choose(&f, false);
        assert!(f.armed(), "the window-init marker must be written while the renderer is probed");
        let _ = std::fs::remove_dir_all(&d);
    }
}
