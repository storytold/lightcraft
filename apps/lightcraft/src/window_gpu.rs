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
pub fn choose(files: &Files, compose_window: bool) -> Chosen {
    let forced =
        backend::forced_window_renderer(std::env::var("LIGHTCRAFT_GPU_BACKEND").ok().as_deref(), std::env::var("WGPU_BACKEND").ok().as_deref());
    let failed = files.take_failed();
    let remembered = if cfg!(windows) { Files::read(files.remembered()) } else { None };
    let compiled = wgpu::Instance::enabled_backend_features();
    let os = std::env::consts::OS;
    let order = backend::window_renderer_order(forced, remembered, failed, os, compiled, true);
    let mut notice = failed.map(|f| format!("The window could not start with its {} renderer last time.", f.name()));
    let mut problems = Vec::new();
    let chosen = if cfg!(windows) {
        pick_probed(&order, &mut problems, compose_window)
    } else if forced == Some(WindowRenderer::Glow) || (forced.is_none() && failed.is_some_and(|f| f != WindowRenderer::Glow)) {
        // elsewhere eframe's own wgpu set-up stays the default; glow after a failed start
        Some((WindowRenderer::Glow, None))
    } else {
        Some((WindowRenderer::Wgpu(backend::window_backends()), None))
    };
    let (which, existing) = chosen.unwrap_or((WindowRenderer::Glow, None));
    if !problems.is_empty() {
        notice = Some(format!("{} {}", notice.unwrap_or_default(), problems.join(" ")).trim().to_string());
    }
    let notice = notice.map(|n| {
        format!(
            "{n} {app} now draws its window with {}. To choose another, start {app} with {var}=dx12, vulkan, gl or glow \
             (see docs/gpu-pipeline.md → Troubleshooting).",
            which.name(),
            app = "LightCraft",
            var = "LIGHTCRAFT_GPU_BACKEND",
        )
    });
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

/// eframe's defaults with the app's backends and DX12 shader compiler (issues #136, #471).
pub fn base_options() -> WgpuConfiguration {
    let mut c = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(n) = &mut c.wgpu_setup {
        n.instance_descriptor.backends = backend::window_backends();
        n.instance_descriptor.backend_options = backend::backend_options();
        // no indirect-call validation pipeline: Intel Metal fails to compile it and the window never opens (#250)
        n.instance_descriptor.flags = backend::instance_flags_from_env(n.instance_descriptor.flags);
    }
    c
}

/// The first renderer of `order` that probes cleanly (glow is taken without a probe).
fn pick_probed(order: &[WindowRenderer], problems: &mut Vec<String>, compose_window: bool) -> Option<(WindowRenderer, Option<WgpuSetupExisting>)> {
    for &r in order {
        match r {
            WindowRenderer::Glow => return Some((r, None)),
            WindowRenderer::Wgpu(b) => {
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
    let base = if adapter.get_info().backend == wgpu::Backend::Gl { wgpu::Limits::downlevel_webgl2_defaults() } else { wgpu::Limits::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("egui wgpu device"),
        required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..base },
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
}
