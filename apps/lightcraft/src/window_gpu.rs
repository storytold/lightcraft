//! The window's renderer and its fallback chain (crash report: on some Windows machines FXC fails
//! to compile egui's own shaders on DX12 — "The parameter is incorrect. (0x80070057)" in
//! `egui_pipeline` — and wgpu's default error handler panicked, killing the app at start-up).
//!
//! - **Windows:** each renderer of [`backend::window_renderer_order`] (DX12, Vulkan, GL, then
//!   eframe's glow) is *probed* before the window opens: a device is created with an error handler
//!   that logs instead of panicking, and egui's pipeline is built inside error scopes. The first
//!   that builds cleanly is handed to eframe as an existing device (so its error handler stays).
//! - **Everywhere:** a marker file names the renderer being started and is removed once the
//!   window's renderer is up. Left behind — the start died or panicked — the next launch tries that
//!   renderer last; [`relaunch_after_failure`] starts that next launch right away.
//! - The renderer that worked is remembered (`<config>/window-renderer`, Windows only) and tried
//!   first next time. `LIGHTCRAFT_GPU_BACKEND=dx12|vulkan|gl|glow` (or `WGPU_BACKEND`) forces one.

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupExisting};
use eframe::wgpu;
use lightcraft_engine::gpu::backend::{self, Backends, WindowRenderer};
use std::path::PathBuf;

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
}

impl Files {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
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
        if let Err(e) = std::fs::write(&p, format!("{}\n", r.name())) {
            log::warn!("window: could not write {}: {e}", p.display());
        }
    }
    /// The renderer the last start died in (marker left behind), removing the marker.
    pub fn take_failed(&self) -> Option<WindowRenderer> {
        let m = self.marker()?;
        let r = Self::read(Some(m.clone()));
        let _ = std::fs::remove_file(m);
        r
    }
    /// Before the window opens: record what is being started.
    pub fn arm(&self, r: WindowRenderer) {
        Self::write(self.marker(), r);
    }
    /// The window's renderer is up: clear the marker and (Windows) remember it.
    pub fn started(&self, r: WindowRenderer) {
        if let Some(m) = self.marker() {
            let _ = std::fs::remove_file(m);
        }
        if cfg!(windows) {
            Self::write(self.remembered(), r);
        }
    }
    /// The marker is still there: the window never came up.
    pub fn armed(&self) -> bool {
        self.marker().is_some_and(|m| m.exists())
    }
}

/// The renderer for this start: see the module docs.
pub fn choose(files: &Files) -> Chosen {
    let forced =
        backend::forced_window_renderer(std::env::var("LIGHTCRAFT_GPU_BACKEND").ok().as_deref(), std::env::var("WGPU_BACKEND").ok().as_deref());
    let failed = files.take_failed();
    let remembered = if cfg!(windows) { Files::read(files.remembered()) } else { None };
    let compiled = wgpu::Instance::enabled_backend_features();
    let os = std::env::consts::OS;
    let order = backend::window_renderer_order(forced, remembered, failed, os, compiled, true);
    let mut problems = Vec::new();
    let chosen = if cfg!(windows) {
        pick_probed(&order, &mut problems)
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
pub fn base_options() -> WgpuConfiguration {
    let mut c = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(n) = &mut c.wgpu_setup {
        n.instance_descriptor.backends = backend::window_backends();
        n.instance_descriptor.backend_options = backend::backend_options();
        // no indirect-call validation pipeline: Intel Metal fails to compile it and the window never opens (#250)
        n.instance_descriptor.flags = backend::instance_flags_from_env(n.instance_descriptor.flags);
        n.device_descriptor = std::sync::Arc::new(device_descriptor);
    }
    c
}

/// Whether renders may share the window's device and reach it as textures (issue #1228): opt-in,
/// `LIGHTCRAFT_GPU_TEXTURES=1`.
fn gpu_textures_on() -> bool {
    std::env::var("LIGHTCRAFT_GPU_TEXTURES").is_ok_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"))
}

/// The window's device: egui's own, unless renders may share it ([`gpu_textures_on`]); then the
/// adapter's limits (never above them), so renders can run there and hand the window textures.
fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    if !gpu_textures_on() {
        return (eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle().device_descriptor)(adapter);
    }
    wgpu::DeviceDescriptor {
        label: Some("egui wgpu device"),
        required_limits: lightcraft_engine::gpu::window_device_limits(adapter),
        ..Default::default()
    }
}

/// Renders that stay on the GPU (issue #1228), drawn through the window's egui-wgpu renderer: they
/// are made on its device ([`lend_device`]) and registered as its textures.
pub struct WindowTextures(eframe::egui_wgpu::RenderState);

impl lightcraft_ui_egui::render::NativeTextures for WindowTextures {
    fn register(&self, texture: &lightcraft_engine::gpu::GpuTexture) -> Option<egui::TextureId> {
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        // linear filtering, clamped edges: what egui's own render textures use (`TextureOptions::LINEAR`)
        Some(self.0.renderer.write().register_native_texture(&self.0.device, &view, wgpu::FilterMode::Linear))
    }

    fn free(&self, id: egui::TextureId) {
        self.0.renderer.write().free_texture(&id);
    }
}

/// Lend the window's device to the GPU renderer, before anything renders: renders then come back
/// as textures on it, which [`WindowTextures`] (returned) draws. `None` with the glow renderer,
/// or when the window's adapter can't run the kernels — renders then keep a device of their own.
pub fn lend_device(render_state: Option<&eframe::egui_wgpu::RenderState>) -> Option<WindowTextures> {
    // Opt-in (`LIGHTCRAFT_GPU_TEXTURES=1`): a window update is fresher, but one device has one
    // queue, and frames wait behind renders there — a 1:1 slider drag on a 61 MP raw dropped 0–4 %
    // of its frames, where renders on a device of their own dropped none (issue #1228).
    if !gpu_textures_on() {
        return None;
    }
    let rs = render_state?;
    let info = rs.adapter.get_info();
    // (a software adapter: the CPU pipeline is faster; the render device refuses it too)
    if info.device_type == wgpu::DeviceType::Cpu {
        return None;
    }
    lightcraft_engine::gpu::share_window_device(rs.device.clone(), rs.queue.clone(), info);
    Some(WindowTextures(rs.clone()))
}

/// The answer to the lost-device dialog: `None` until it was shown, `Some(None)` while it is
/// open, then `Some(Some(restart))`.
static LOST_ANSWER: std::sync::Mutex<Option<Option<bool>>> = std::sync::Mutex::new(None);

/// The window's device was lost (`lightcraft_engine::gpu::window_device_lost`; issue #1228): the
/// window can't draw on it again. Asks once, in a native dialog on its own thread (no GPU needed),
/// whether to restart or quit; `Some(restart)` once answered. Without a dialog to click away
/// (`LIGHTCRAFT_NO_PREFS`: tests, scripts) the answer is quit.
pub fn device_lost_answer(ctx: &egui::Context) -> Option<bool> {
    let mut answer = LOST_ANSWER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = *answer {
        return a;
    }
    log::error!("window: the graphics device was lost; LightCraft can't draw its window any more");
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        *answer = Some(Some(false));
        return Some(false);
    }
    *answer = Some(None);
    let ctx = ctx.clone();
    let asked = ask_device_lost(move |(title, text, restart, quit)| {
        let r = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(text)
            .set_buttons(rfd::MessageButtons::OkCancelCustom(restart.clone(), quit))
            .show();
        let again = matches!(&r, rfd::MessageDialogResult::Custom(b) if *b == restart) || r == rfd::MessageDialogResult::Ok;
        *LOST_ANSWER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Some(again));
        ctx.request_repaint();
    });
    if asked.is_err() {
        // no thread for the dialog: start again rather than leave a window that can't draw
        *answer = Some(Some(true));
        return Some(true);
    }
    None
}

/// Shows the lost-device dialog (`ask`) on a thread of its own. The message is put together
/// here, on the calling (UI) thread: the UI language is per thread, and a new thread's is English.
fn ask_device_lost(ask: impl FnOnce((String, String, String, String)) + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    let message = lightcraft_ui_egui::i18n::device_lost_message();
    std::thread::Builder::new().name("device-lost".into()).spawn(move || ask(message))
}

/// The answer to the dialog shown when saving failed after the device was lost: `None` when no
/// dialog is up, `Some(None)` while it is open, then `Some(Some(quit_anyway))` until read.
static UNSAVED_ANSWER: std::sync::Mutex<Option<Option<bool>>> = std::sync::Mutex::new(None);

/// Saving failed after the window's device was lost: `changes` couldn't be written to disk, for
/// `reason`. Asks in a native dialog on its own thread whether to try again or quit without them;
/// `Some(true)` is Quit Anyway, `Some(false)` Try Again (each answer is read once, so a failed
/// retry asks again). Without a dialog (`LIGHTCRAFT_NO_PREFS`, or no thread for one) nothing is
/// answered: the changes are never dropped without someone choosing to.
pub fn unsaved_answer(ctx: &egui::Context, changes: usize, reason: &str) -> Option<bool> {
    let mut answer = UNSAVED_ANSWER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match *answer {
        Some(Some(quit)) => {
            *answer = None;
            return Some(quit);
        }
        Some(None) => return None,
        None => {}
    }
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return None;
    }
    log::error!("window: {changes} change(s) couldn't be saved after the graphics device was lost: {reason}");
    let ctx = ctx.clone();
    let asked = ask_unsaved(changes, reason, move |(title, text, try_again, quit)| {
        let r = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(text)
            .set_buttons(rfd::MessageButtons::OkCancelCustom(try_again, quit.clone()))
            .show();
        // (as for the lost-device dialog: some platforms answer Ok / Cancel for the two buttons)
        let quit = matches!(&r, rfd::MessageDialogResult::Custom(b) if *b == quit) || r == rfd::MessageDialogResult::Cancel;
        *UNSAVED_ANSWER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Some(quit));
        ctx.request_repaint();
    });
    match asked {
        Ok(_) => *answer = Some(None),
        Err(e) => log::error!("window: could not ask about the unsaved changes: {e}"),
    }
    None
}

/// Shows the saving-failed dialog (`ask`) on a thread of its own, its message put together on the
/// calling (UI) thread, as [`ask_device_lost`] does.
fn ask_unsaved(
    changes: usize,
    reason: &str,
    ask: impl FnOnce((String, String, String, String)) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let message = lightcraft_ui_egui::i18n::device_lost_unsaved_message(changes, reason);
    std::thread::Builder::new().name("device-lost".into()).spawn(move || ask(message))
}

/// Start LightCraft again after the window's device was lost, with the arguments it was started
/// with and renders on a device of their own (`LIGHTCRAFT_GPU_TEXTURES` cleared, so the
/// default): a driver that loses shared devices can't take the new window down with a render.
pub fn relaunch_with_own_render_device() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    log::warn!("window: starting again, renders on a device of their own");
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env_remove("LIGHTCRAFT_GPU_TEXTURES")
        .spawn()
        .map_err(|e| log::error!("window: could not start again: {e}"))
        .is_ok()
}

/// The first renderer of `order` that probes cleanly (glow is taken without a probe).
fn pick_probed(order: &[WindowRenderer], problems: &mut Vec<String>) -> Option<(WindowRenderer, Option<WgpuSetupExisting>)> {
    for &r in order {
        match r {
            WindowRenderer::Glow => return Some((r, None)),
            WindowRenderer::Wgpu(b) => {
                let probed = std::panic::catch_unwind(|| probe(b)).unwrap_or_else(|_| Err("panicked".into()));
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
fn probe(backends: Backends) -> Result<WgpuSetupExisting, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: backend::instance_flags_from_env(wgpu::InstanceFlags::from_build_config().with_env()),
        backend_options: backend::backend_options(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
        ..Default::default()
    }))
    .map_err(|e| format!("no adapter: {e}"))?;
    // (as `base_options` asks)
    let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor(&adapter)))
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The lost-device dialog runs on a thread of its own, whose UI language would be English:
    /// its text must be in the language the UI thread has.
    #[test]
    fn the_lost_device_dialog_speaks_the_ui_language() {
        use lightcraft_ui_egui::i18n;
        let other = *i18n::Locale::ALL.iter().find(|&&l| l != i18n::Locale::En).unwrap();
        i18n::set_language(other);
        let expected = i18n::device_lost_message();
        let (tx, rx) = std::sync::mpsc::channel();
        ask_device_lost(move |message| tx.send(message).unwrap()).unwrap().join().unwrap();
        i18n::set_language(i18n::Locale::En);
        assert_eq!(rx.recv().unwrap(), expected, "dialog shown in {other:?}");
    }

    /// Held by every test here that opens a wgpu instance, for its whole run. wgpu's GL backend
    /// shares one EGL display between instances and terminates it when one is dropped, so a GL
    /// instance opened and dropped by one test segfaults another drawing on Mesa GL meanwhile
    /// (the FreeBSD CI runner, which has no Vulkan driver).
    static GPU_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_gpu_test_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        GPU_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("app-window-gpu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The marker names the renderer being started; left behind, the next start reads it as the
    /// failed one (and removes it); a good start removes it and remembers the renderer (Windows).
    #[test]
    fn marker_records_the_failed_renderer() {
        let d = dir("marker");
        let f = Files::new(Some(d.clone()));
        let dx = WindowRenderer::Wgpu(Backends::DX12);
        f.arm(dx);
        assert!(f.armed());
        assert_eq!(f.take_failed(), Some(dx));
        assert!(!f.armed());
        assert_eq!(f.take_failed(), None);
        f.arm(WindowRenderer::Glow);
        f.started(WindowRenderer::Glow);
        assert!(!f.armed());
        assert_eq!(Files::read(f.remembered()), cfg!(windows).then_some(WindowRenderer::Glow));
        // nothing written without a settings folder
        let none = Files::new(None);
        none.arm(dx);
        assert!(!none.armed() && !relaunch_after_failure(&none));
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

    /// The crash report: a pipeline that fails validation must come back as an error, never as
    /// wgpu's default panic. Needs an adapter; skipped where there is none (CI without a GPU).
    #[test]
    fn pipeline_errors_are_captured_not_panics() {
        let _gpu = one_gpu_test_at_a_time();
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

    /// Issue #1228: renders share the window's device, so a lost device (driver reset, GPU hang)
    /// is lost to the window too. egui-wgpu panics drawing a frame with anything in it there (its
    /// staging buffers can't be made), so the app draws empty frames once the device is lost and
    /// offers a restart (`main.rs`). An empty frame — a render's native texture registered on the
    /// lost device, textures uploaded, the frame recorded and submitted, textures freed — must
    /// not panic.
    #[test]
    fn a_lost_window_device_draws_empty_frames_without_a_panic() {
        use eframe::egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
        let _gpu = one_gpu_test_at_a_time();
        // Software adapters and wgpu's GL backend (Mesa GL on the FreeBSD CI runner, which has no
        // Vulkan driver) segfault when a destroyed device is used, where Metal, DX12 and hardware
        // Vulkan report the loss: nothing to learn there. GL isn't even opened (see `GPU_TESTS`).
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: backend::window_backends() - Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter but GL's");
            return;
        };
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu || info.backend == wgpu::Backend::Gl {
            lightcraft_testkit::skip!(Gpu, "{} adapter {} crashes on a destroyed device", info.backend, info.name);
            return;
        }
        let desc = wgpu::DeviceDescriptor { required_limits: lightcraft_engine::gpu::window_device_limits(&adapter), ..Default::default() };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&desc)) else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        install_error_handler(&device);
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let mut renderer = Renderer::new(&device, format, RendererOptions::default());
        let render = |size: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let shown = renderer.register_native_texture(&device, &render(64).create_view(&Default::default()), wgpu::FilterMode::Linear);
        device.destroy();
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        // the next render arrives after the loss: registered on the lost device
        let next = renderer.register_native_texture(&device, &render(64).create_view(&Default::default()), wgpu::FilterMode::Linear);
        let managed = egui::TextureId::Managed(7);
        let delta = egui::epaint::ImageDelta::full(egui::ColorImage::new([8, 8], vec![egui::Color32::GRAY; 64]), egui::TextureOptions::LINEAR);
        renderer.update_texture(&device, &queue, managed, &delta);
        let mut mesh = egui::Mesh::with_texture(next);
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(64.0, 64.0));
        mesh.add_rect_with_uv(r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        let shapes = [egui::epaint::ClippedPrimitive { clip_rect: r, primitive: egui::epaint::Primitive::Mesh(mesh) }];
        let screen = ScreenDescriptor { size_in_pixels: [64, 64], pixels_per_point: 1.0 };
        // what the app must not draw on a lost device. This pins an egui-wgpu bug: the day an
        // egui-wgpu update stops panicking here, this assertion fails. Then delete it and this
        // half of the test, and draw frames on a lost device as usual (the empty frames in
        // `main.rs` can go); the empty frame below must still not panic.
        let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut enc = device.create_command_encoder(&Default::default());
            renderer.update_buffers(&device, &queue, &mut enc, &shapes, &screen)
        }));
        assert!(drawn.is_err(), "egui-wgpu draws on a lost device now");
        let jobs: [egui::epaint::ClippedPrimitive; 0] = [];
        let mut enc = device.create_command_encoder(&Default::default());
        let user = renderer.update_buffers(&device, &queue, &mut enc, &jobs, &screen);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        {
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        queue.submit(user.into_iter().chain([enc.finish()]));
        renderer.free_texture(&shown);
        renderer.free_texture(&next);
        renderer.free_texture(&managed);
    }
}
