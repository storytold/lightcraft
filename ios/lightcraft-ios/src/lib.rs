#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
mod monkey;
use monkey::MonkeyTester;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use eframe::egui::{Context, RawInput, Ui};
use lightcraft_engine::Session;
use lightcraft_ui_egui::{LightcraftApp, Services};

unsafe extern "C" {
    fn ios_log(format: *const c_char);
    fn ios_open_url(url: *const c_char) -> bool;
    fn ios_share_file(path: *const c_char) -> bool;
    #[allow(dead_code)]
    fn ios_share_files(paths_json: *const c_char) -> bool;
    fn ios_get_safe_area(top: *mut f32, bottom: *mut f32, left: *mut f32, right: *mut f32);
    fn ios_show_native_picker(
        kind: u32,
        title: *const c_char,
        filter_name: *const c_char,
        filter_exts: *const c_char,
        file_name: *const c_char,
        context: *mut c_void,
        completion: unsafe extern "C" fn(*mut c_void, *const c_char),
    );
    fn ios_toggle_keyboard(show: bool);
    fn ios_get_system_theme() -> i32;
}

struct PickerCallbackPayload {
    tx: Sender<Vec<String>>,
    ctx: Context,
}

unsafe extern "C" fn on_picker_complete(ctx: *mut c_void, json_ptr: *const c_char) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: ctx was created via Box::into_raw from a Box<PickerCallbackPayload>
    let payload = unsafe { Box::from_raw(ctx as *mut PickerCallbackPayload) };
    let mut paths = Vec::new();
    if !json_ptr.is_null() {
        // SAFETY: json_ptr is a valid NUL-terminated C string created by Swift
        let s = unsafe { CStr::from_ptr(json_ptr) };
        if let Ok(s) = s.to_str()
            && let Ok(parsed) = serde_json::from_str::<Vec<String>>(s)
        {
            paths = parsed;
        }
    }
    let _ = payload.tx.send(paths);
    payload.ctx.request_repaint();
}

struct OsLogger;

impl log::Log for OsLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let msg = format!("[LightCraft-Rust] [{}] {}\0", record.level(), record.args());
        // SAFETY: `msg` ends in NUL and lives until the call returns; Swift's `ios_log` only reads it.
        unsafe {
            ios_log(msg.as_ptr() as *const c_char);
        }
    }

    fn flush(&self) {}
}

static OS_LOGGER: OsLogger = OsLogger;
static PENDING_ACTIONS: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub extern "C" fn lightcraft_post_action(action: i32) {
    if let Ok(mut q) = PENDING_ACTIONS.lock() {
        q.push(action);
    }
}

struct IosApp {
    app: LightcraftApp,
    first_frame: bool,
    last_wants_kb: bool,
    monkey: MonkeyTester,
    /// Pencil strokes waiting to be read together (a lifted 4 or 5, or two diagonals making a cross).
    pending_strokes: Vec<(std::time::Instant, Vec<(f32, f32)>)>,
}

/// Apple Pencil strokes from Swift (window points), waiting for the UI thread.
static PENCIL_STROKES: std::sync::Mutex<Vec<Vec<(f32, f32)>>> = std::sync::Mutex::new(Vec::new());

/// Swift reports one finished Pencil stroke: `n` points as parallel `xs` / `ys` arrays.
///
/// # Safety
/// `xs` and `ys` must each point to `n` readable `f32`s that stay valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lightcraft_ios_pencil_stroke(xs: *const f32, ys: *const f32, n: i32) {
    if xs.is_null() || ys.is_null() || !(2..=4096).contains(&n) {
        return;
    }
    // SAFETY: Swift passes two live arrays of `n` floats (checked non-null, n bounded above).
    let (xs, ys) = unsafe { (std::slice::from_raw_parts(xs, n as usize), std::slice::from_raw_parts(ys, n as usize)) };
    let stroke: Vec<(f32, f32)> = xs.iter().copied().zip(ys.iter().copied()).collect();
    let mut q = PENCIL_STROKES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if q.len() < 8 {
        q.push(stroke);
    }
}

/// Swift's segmenter (Apple Vision): `(rgb, w, h, key, clicks [x, y, include]*, n_clicks, out_logits, side)`;
/// returns 1 = mask written, 0 = nothing to select, negative = failure.
type SegmentCb = unsafe extern "C" fn(*const u8, i32, i32, u64, *const f32, i32, *mut f32, i32) -> i32;
static SEGMENT_CB: std::sync::OnceLock<SegmentCb> = std::sync::OnceLock::new();
/// Side of the coverage grid Vision's mask is sampled to.
const VISION_SIDE: usize = 384;

fn vision_segment(
    rgb: &[u8],
    w: usize,
    h: usize,
    key: u64,
    clicks: &[[f32; 3]],
) -> Result<Option<lightcraft_engine::segment::platform::PlatformMask>, String> {
    let cb = SEGMENT_CB.get().ok_or("Apple Vision segmentation is not available")?;
    if rgb.len() < w.saturating_mul(h).saturating_mul(3) || w == 0 || h == 0 {
        return Err("the photo could not be prepared for Apple Vision".into());
    }
    let mut logits = vec![0f32; VISION_SIDE * VISION_SIDE];
    // SAFETY: the buffers outlive the call and their sizes match what is passed (checked above).
    let r =
        unsafe { cb(rgb.as_ptr(), w as i32, h as i32, key, clicks.as_ptr().cast(), clicks.len() as i32, logits.as_mut_ptr(), VISION_SIDE as i32) };
    match r {
        1 => Ok(Some(lightcraft_engine::segment::platform::PlatformMask { side: VISION_SIDE, logits })),
        0 => Ok(None),
        -2 => Err("Apple Vision subject selection needs iPadOS 17 or later".into()),
        e => Err(format!("Apple Vision could not select anything ({e})")),
    }
}

/// Swift registers its Vision segmenter at launch; masks then need no model download.
#[unsafe(no_mangle)]
pub extern "C" fn lightcraft_ios_set_segmenter(cb: Option<SegmentCb>) {
    if let Some(cb) = cb
        && SEGMENT_CB.set(cb).is_ok()
    {
        lightcraft_engine::segment::platform::set_platform_segmenter(vision_segment);
    }
}

impl IosApp {
    /// Pencil marks on the photo: a digit 1-5 rates, a circle (0) clears the rating, a tick picks, a cross rejects.
    /// Strokes wait a moment so a digit drawn with a lift (a 4, a 5) or a two-stroke cross is read as one mark.
    fn apply_pencil_strokes(&mut self, ctx: &Context) {
        use lightcraft_ui_egui::pencil::{Mark, recognize_strokes};
        let now = std::time::Instant::now();
        let fresh = std::mem::take(&mut *PENCIL_STROKES.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for stroke in fresh {
            if self.pending_strokes.last().is_some_and(|(t, _)| now.duration_since(*t) > std::time::Duration::from_millis(1500)) {
                self.pending_strokes.clear();
            }
            self.pending_strokes.push((now, stroke));
        }
        let Some((last, _)) = self.pending_strokes.last() else { return };
        if now.duration_since(*last) < std::time::Duration::from_millis(380) {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
            return;
        }
        let strokes: Vec<Vec<(f32, f32)>> = self.pending_strokes.drain(..).map(|(_, s)| s).collect();
        // Crop, Remove, Masking and Red Eye use Pencil strokes on the photo themselves; the Edit panel does not
        let draws_on_photo = matches!(
            self.app.ui.right,
            lightcraft_ui_egui::state::RightPanel::Crop
                | lightcraft_ui_egui::state::RightPanel::Remove
                | lightcraft_ui_egui::state::RightPanel::Masking
                | lightcraft_ui_egui::state::RightPanel::RedEye
        );
        if self.app.ui.dialog.is_some() || draws_on_photo || !self.app.ui.tool.is_empty() {
            return;
        }
        // the first stroke has to start over the photo / grid, not over a panel's sliders
        if let (Some(canvas), Some(start)) = (self.app.canvas_rect, strokes.first().and_then(|s| s.first()))
            && !canvas.contains(eframe::egui::pos2(start.0, start.1))
        {
            return;
        }
        let Some(mark) = recognize_strokes(&strokes) else { return };
        let (cmd, params, label) = match mark {
            Mark::Rate(0) => ("photo.rate", serde_json::json!({"rating": 0}), "No rating".to_string()),
            Mark::Rate(n) => ("photo.rate", serde_json::json!({"rating": n}), "★".repeat(usize::from(n))),
            Mark::Pick => ("photo.flag", serde_json::json!({"flag": "pick"}), "✓ Picked".to_string()),
            Mark::Reject => ("photo.flag", serde_json::json!({"flag": "reject"}), "✗ Rejected".to_string()),
        };
        match self.app.run(cmd, params) {
            Ok(_) => self.app.toast(ctx, label),
            Err(e) => log::info!("pencil mark {mark:?} not applied: {e}"),
        }
    }

    fn update_keyboard(&mut self, ctx: &Context) {
        let wants_kb = ctx.egui_wants_keyboard_input() || self.app.text_focus;
        if wants_kb != self.last_wants_kb {
            self.last_wants_kb = wants_kb;
            // SAFETY: `ios_toggle_keyboard` is defined by the Swift host linked into this app and takes a plain bool.
            unsafe {
                ios_toggle_keyboard(wants_kb);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(wants_kb));
        }
    }
}

impl eframe::App for IosApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let mut top = 0.0f32;
        let mut bottom = 0.0f32;
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        unsafe {
            ios_get_safe_area(&mut top, &mut bottom, &mut left, &mut right);
        }
        if top < 20.0 {
            top = 24.0;
        }
        lightcraft_ui_egui::theme::set_safe_area_insets(ctx, top, bottom, left, right);
        if self.first_frame {
            log::info!("[LightCraft] First logic frame rendering with safe area top: {top}, bottom: {bottom}!");
        }
        if let Ok(mut q) = PENDING_ACTIONS.lock() {
            for action in q.drain(..) {
                match action {
                    1 => {
                        log::info!("[LightCraft] Executing 2-finger Undo");
                        let _ = self.app.run("edit.undo", serde_json::json!({}));
                        self.app.toast(ctx, lightcraft_ui_egui::i18n::tr("Undo"));
                    }
                    2 => {
                        log::info!("[LightCraft] Executing 3-finger Redo");
                        let _ = self.app.run("edit.redo", serde_json::json!({}));
                        self.app.toast(ctx, lightcraft_ui_egui::i18n::tr("Redo"));
                    }
                    3 => {
                        log::info!("[LightCraft] Executing Apple Pencil double-tap (zoom toggle)");
                        let _ = self.app.run("view.zoomToggle", serde_json::json!({}));
                    }
                    _ => {}
                }
            }
        }
        self.app.logic(ctx);
        self.update_keyboard(ctx);
    }

    fn raw_input_hook(&mut self, ctx: &Context, raw: &mut RawInput) {
        self.monkey.on_raw_input(ctx, raw);
        self.app.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            log::info!("[LightCraft] First UI frame rendering!");
            self.first_frame = false;
        }
        self.app.ui(ui);
        self.apply_pencil_strokes(ui.ctx());
        self.monkey.audit_state(&self.app, ui.ctx());
        self.update_keyboard(ui.ctx());
    }

    fn on_exit(&mut self) {
        log::info!("[LightCraft] on_exit callback invoked!");
        if let Err(error) = self.app.session.close_library() {
            log::error!("saving the iOS library failed: {error}");
        }
        if !self.app.shutdown(std::time::Duration::from_secs(2)) {
            log::warn!("iOS shutdown ended while render work was still active");
        }
    }
}

fn create_services(egui_ctx: Context) -> Services {
    // Atomic file writing services for export batch and presets
    let mut services = Services {
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)),
        ..Services::default()
    };
    services.png = Some(Box::new(|img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    }));

    // Native iOS Share Sheet for exported photos and files
    services.reveal = Some(Box::new(|path: &str| {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        let ok =
            if path.trim_start().starts_with('[') { unsafe { ios_share_files(c_path.as_ptr()) } } else { unsafe { ios_share_file(c_path.as_ptr()) } };
        if ok { Ok(()) } else { Err(format!("failed to open share sheet for {path}")) }
    }));

    // Asynchronous non-blocking native picker for egui's pick::poll (#191)
    let picker_ctx = egui_ctx.clone();
    services.picker = Some(Box::new(move |req: lightcraft_ui_egui::pick::PickRequest| {
        let (tx, rx) = channel();
        let payload = Box::new(PickerCallbackPayload { tx, ctx: picker_ctx.clone() });
        let ctx_ptr = Box::into_raw(payload) as *mut c_void;
        let kind = match req.kind {
            lightcraft_ui_egui::pick::PickKind::Files => 0,
            lightcraft_ui_egui::pick::PickKind::File => 1,
            lightcraft_ui_egui::pick::PickKind::Folder => 2,
            lightcraft_ui_egui::pick::PickKind::Save => 3,
        };

        let title_c = req.title.as_ref().map(|s| CString::new(s.as_str()).unwrap_or_default());
        let title_ptr = title_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

        let filter_name_c = req.filter.as_ref().map(|(n, _)| CString::new(n.as_str()).unwrap_or_default());
        let filter_name_ptr = filter_name_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

        let filter_exts_joined = req.filter.as_ref().map(|(_, exts)| exts.join(","));
        let filter_exts_c = filter_exts_joined.as_ref().map(|s| CString::new(s.as_str()).unwrap_or_default());
        let filter_exts_ptr = filter_exts_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

        let file_name_c = req.file_name.as_ref().map(|s| CString::new(s.as_str()).unwrap_or_default());
        let file_name_ptr = file_name_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

        unsafe {
            ios_show_native_picker(kind, title_ptr, filter_name_ptr, filter_exts_ptr, file_name_ptr, ctx_ptr, on_picker_complete);
        }
        Ok(rx)
    }));

    // Lightroom Classic catalog import picker (.lrcat)
    let lr_ctx = egui_ctx.clone();
    services.pick_lightroom_catalog = Some(Box::new(move || {
        let (tx, rx) = channel();
        let payload = Box::new(PickerCallbackPayload { tx, ctx: lr_ctx.clone() });
        let ctx_ptr = Box::into_raw(payload) as *mut c_void;
        let title = CString::new("Import Lightroom Catalog").unwrap_or_default();
        let filter_name = CString::new("Lightroom Classic Catalog").unwrap_or_default();
        let filter_exts = CString::new("lrcat").unwrap_or_default();
        unsafe {
            ios_show_native_picker(
                1, // kind: File
                title.as_ptr(),
                filter_name.as_ptr(),
                filter_exts.as_ptr(),
                std::ptr::null(),
                ctx_ptr,
                on_picker_complete,
            );
        }
        rx.recv().ok().and_then(|mut paths| paths.pop())
    }));

    // Synchronous fallback pickers
    let repaint_files = egui_ctx.clone();
    services.pick_files = Some(Box::new(move || {
        let (tx, rx) = channel();
        let payload = Box::new(PickerCallbackPayload { tx, ctx: repaint_files.clone() });
        let ctx_ptr = Box::into_raw(payload) as *mut c_void;
        unsafe {
            ios_show_native_picker(
                0, // kind: Files
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                ctx_ptr,
                on_picker_complete,
            );
        }
        rx.recv().ok().unwrap_or_default()
    }));

    // Folder picking on iOS (open or new library folder)
    let folder_ctx = egui_ctx.clone();
    services.pick_folder = Some(Box::new(move || {
        let (tx, rx) = channel();
        let payload = Box::new(PickerCallbackPayload { tx, ctx: folder_ctx.clone() });
        let ctx_ptr = Box::into_raw(payload) as *mut c_void;
        let title = CString::new("Select Library Folder").unwrap_or_default();
        unsafe {
            ios_show_native_picker(
                2, // kind: Folder
                title.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                ctx_ptr,
                on_picker_complete,
            );
        }
        rx.recv().ok().and_then(|mut paths| paths.pop())
    }));

    // Preset export picker (kind: Save)
    let save_preset_ctx = egui_ctx.clone();
    services.save_preset_file = Some(Box::new(move |name: &str| {
        let (tx, rx) = channel();
        let payload = Box::new(PickerCallbackPayload { tx, ctx: save_preset_ctx.clone() });
        let ctx_ptr = Box::into_raw(payload) as *mut c_void;
        let title = CString::new("Export Preset").unwrap_or_default();
        let filter_name = CString::new("LightCraft Preset").unwrap_or_default();
        let filter_exts = CString::new("lcpreset").unwrap_or_default();
        let file_name = CString::new(name).unwrap_or_default();
        unsafe {
            ios_show_native_picker(
                3, // kind: Save
                title.as_ptr(),
                filter_name.as_ptr(),
                filter_exts.as_ptr(),
                file_name.as_ptr(),
                ctx_ptr,
                on_picker_complete,
            );
        }
        rx.recv().ok().and_then(|mut paths| paths.pop())
    }));

    // System web URL opener for documentation / links
    services.system_theme = Some(Box::new(|_ctx| {
        let t = unsafe { ios_get_system_theme() };
        match t {
            1 => Some(egui::Theme::Light),
            2 => Some(egui::Theme::Dark),
            _ => None,
        }
    }));

    services.open_url = Some(Box::new(|url: &str| {
        if !url.starts_with("https://") {
            return Err("only https links are opened".into());
        }
        let url_c = CString::new(url).map_err(|e| e.to_string())?;
        let ok = unsafe { ios_open_url(url_c.as_ptr()) };
        if ok { Ok(()) } else { Err(format!("failed to open URL: {url}")) }
    }));

    services
}

fn launch(library_dir: PathBuf) -> i32 {
    let _ = log::set_logger(&OS_LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    log::info!("LightCraft launch initiated with library_dir: {:?}", library_dir);

    lightcraft_engine::gpu::set_enabled(true);
    // Warm up GPU pipeline on a background thread so UI starts immediately without blocking
    lightcraft_engine::gpu::warm_up();

    let mut session = Session::new().with_fs().with_default_denoise_models().with_system_clock();

    // SAM 3 AI segmentation models directory and mirror setup in Documents/models/sam3
    let documents_dir = library_dir.parent().unwrap_or(&library_dir);
    let models_dir = documents_dir.join("models");
    let sam3_models_dir = models_dir.join("sam3");
    if let Err(e) = std::fs::create_dir_all(&sam3_models_dir) {
        log::warn!("Could not create SAM 3 models directory {:?}: {e}", sam3_models_dir);
    }
    let mirrors_file = models_dir.join("sam3-mirrors.txt");
    if !mirrors_file.exists() {
        let template = "# LightCraft SAM 3 Model Mirrors\n# Add download base URLs below (one per line, e.g. Hugging Face resolve URL):\n# https://huggingface.co/facebook/sam3/resolve/main\n";
        let _ = std::fs::write(&mirrors_file, template);
    }
    session = session.with_default_sam3_model();
    if session.segmenter.dir.is_none() || session.segmenter.dir.as_ref().is_some_and(|d| !d.exists()) {
        session.segmenter.dir = Some(sam3_models_dir.clone());
    }
    if session.segmenter.mirrors_file.is_none() || session.segmenter.mirrors_file.as_ref().is_some_and(|f| !f.exists()) {
        session.segmenter.mirrors_file = Some(mirrors_file);
    }

    // Check if the app bundle includes models/sam3
    if let Ok(exe) = std::env::current_exe()
        && let Some(bundle_dir) = exe.parent().map(|p| p.join("models/sam3"))
        && lightcraft_segment::is_model_dir(&bundle_dir)
    {
        log::info!("Found bundled SAM 3 model in {:?}", bundle_dir);
        session.segmenter.dir = Some(bundle_dir);
    }
    // (not seeded with demo photos: the library is the user's own, and an empty one says how to import)
    let library_error = session.open_library(&library_dir, false).err().map(|error| error.to_string());
    // iOS can hand the app a new container folder after a reinstall: photos added in place then point into the old
    // one and show as offline originals. Find them again in the current container.
    if library_error.is_none()
        && let Some(container) = library_dir.parent().and_then(std::path::Path::parent)
    {
        match session.relink_after_container_change(container) {
            Ok(0) => {}
            Ok(n) => log::info!("relinked {n} photos into the current app container"),
            Err(e) => log::warn!("relinking photos after a container change failed: {e}"),
        }
    }
    if library_error.is_none() {
        match session.remove_demo_photos() {
            Ok(0) => {}
            Ok(n) => log::info!("removed {n} demo photos from the library"),
            Err(e) => log::warn!("removing demo photos failed: {e}"),
        }
        // launch housekeeping is not something the user can undo
        session.undo.clear();
        session.redo.clear();
    }
    // (a library that would not open leaves the configured session as it is: an empty, in-memory one that keeps the
    // services, model folder and clock set up above. The user is told below; no demo photos stand in for their own.)
    session.media.preview_capacity = 2;

    // Default export directory inside iPad Documents/Exports container
    let exports_dir = library_dir.parent().unwrap_or(&library_dir).join("Exports");
    if let Err(e) = std::fs::create_dir_all(&exports_dir) {
        log::warn!("Could not create exports directory {:?}: {e}", exports_dir);
    }
    let exports_str = exports_dir.to_string_lossy().to_string();
    if let Some(last) = session.last_export.as_mut() {
        if let Some(d) = last.get("dir").and_then(|v| v.as_str()) {
            if !std::path::Path::new(d).exists() && std::fs::create_dir_all(d).is_err() {
                log::warn!("Resetting invalid/stale export dir {d} to {exports_str}");
                last["dir"] = serde_json::json!(exports_str);
            }
        } else {
            last["dir"] = serde_json::json!(exports_str);
        }
    } else {
        session.last_export = Some(serde_json::json!({
            "dir": exports_str,
            "format": "jpeg",
            "quality": 90,
        }));
    }

    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(ref mut setup) = wgpu_options.wgpu_setup {
        setup.device_descriptor = std::sync::Arc::new(|adapter| {
            let supported = adapter.limits();
            let mut limits = eframe::wgpu::Limits::downlevel_webgl2_defaults();
            limits.max_texture_dimension_2d = 8192.min(supported.max_texture_dimension_2d);
            limits.max_inter_stage_shader_variables = supported.max_inter_stage_shader_variables;
            eframe::wgpu::DeviceDescriptor { label: Some("egui wgpu device"), required_limits: limits, ..Default::default() }
        });
    }

    let native_options = eframe::NativeOptions { renderer: eframe::Renderer::Wgpu, wgpu_options, ..Default::default() };

    log::info!("Calling eframe::run_native...");
    let result = eframe::run_native(
        "LightCraft iPad (fork)",
        native_options,
        Box::new(move |cc| {
            log::info!("app_creator invoked: touch UI configured");
            lightcraft_ui_egui::theme::set_touch_ui(&cc.egui_ctx, true);
            lightcraft_ui_egui::theme::set_safe_area_insets(&cc.egui_ctx, 24.0, 20.0, 0.0, 0.0);
            let services = create_services(cc.egui_ctx.clone());
            let mut app = LightcraftApp::new(session, services);
            app.ui.settings.dark_theme = lightcraft_ui_egui::state::DarkTheme::Glass;
            // the tool panels' controls need ~320pt; the desktop default (270) clips them
            app.ui.right_width = 340.0;
            app.ui.left_width = 300.0;
            app.ui.dialog = None;
            if let Some(error) = &library_error {
                app.toast_for(&cc.egui_ctx, format!("Your library could not be opened ({error}). Photos you add now are not saved."), 12.0);
            }
            let monkey = MonkeyTester::new();
            Ok(Box::new(IosApp { app, first_frame: true, last_wants_kb: false, monkey, pending_strokes: Vec::new() }))
        }),
    );

    match result {
        Ok(()) => {
            log::info!("eframe::run_native exited cleanly (0)");
            0
        }
        Err(e) => {
            log::error!("eframe::run_native failed: {e}");
            -1
        }
    }
}

/// # Safety
/// `library_dir` must be null or a valid NUL-terminated C string for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lightcraft_start(library_dir: *const c_char) -> i32 {
    let path = if library_dir.is_null() {
        PathBuf::from("LightCraft Library")
    } else {
        let c_str = unsafe { CStr::from_ptr(library_dir) };
        c_str.to_str().map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("LightCraft Library"))
    };
    launch(path)
}
