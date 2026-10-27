//! LightCraft mobile shell (spike): the desktop egui UI and engine, unmodified, in an iOS or
//! Android app. What works and what doesn't is in `README.md`; the porting assessment is in
//! `docs/porting-report.md`.
//!
//! The library lives in the app's sandbox: `Documents/LightCraft Library` on iOS (visible in the
//! Files app), `<internal data>/LightCraft Library` on Android. A new library gets the demo photos.
//! Photos come in through the system photo picker. The commands that open other files or a folder
//! (Import from Folder…, Import Presets…, a GPS track log …) show the system's document picker,
//! and those that save a file (Export Keywords…, a contact sheet …) save it in the exports folder
//! and offer it in the share sheet (see [`pick`]).
#![cfg(any(target_os = "ios", target_os = "android"))]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// `unsafe` is confined to the platform modules (Objective-C / UIKit on iOS, JNI on Android: the
// `ios_*` and `android_jni` modules, each with `#![allow(unsafe_code)]`); every block says why
// it is sound
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(target_os = "android")]
mod android_jni;
#[cfg(target_os = "ios")]
mod ios_files;
#[cfg(target_os = "ios")]
mod ios_keys;
#[cfg(target_os = "ios")]
mod ios_pencil;
#[cfg(target_os = "ios")]
mod ios_picker;
#[cfg(target_os = "ios")]
mod ios_scene;
#[cfg(target_os = "ios")]
mod ios_share;
mod memory;
// the desktop's loopback control channel, for driving the app from tests (see `control_port`)
#[path = "../../lightcraft/src/control_server.rs"]
#[allow(dead_code)] // the desktop's constants for its own tests
mod control_server;

use lightcraft_engine::Session;
use lightcraft_ui_egui::{LightcraftApp, Services};

struct App {
    app: LightcraftApp,
    /// The UI state and app settings (language, shortcuts …) in the sandbox's `ui.json`.
    prefs: lightcraft_ui_egui::prefs::UiPrefs,
    /// The touch screen has been announced to egui (see `raw_input_hook`).
    touch_announced: bool,
    /// iOS: the safe area as egui-winit last reported it, before the keyboard is added.
    /// Android: the insets last reported to egui.
    #[cfg(any(target_os = "ios", target_os = "android"))]
    insets: egui::SafeAreaInsets,
    /// The keyboard height last folded into the safe area.
    #[cfg(target_os = "ios")]
    keyboard: f32,
    /// When the window insets were last read (egui time, s).
    #[cfg(target_os = "android")]
    insets_polled: f64,
    /// The text field the soft keyboard types into (see `android_jni::start_text_input`), and
    /// what the keyboard was last told about it.
    #[cfg(target_os = "android")]
    text_input: Option<egui::Id>,
    #[cfg(target_os = "android")]
    field: android_jni::Field,
    /// The widget that had focus when a drag value or slider last reported itself (a click on
    /// one focuses its text field before that field exists), with the value it reported.
    #[cfg(target_os = "android")]
    numeric_focus: Option<(egui::Id, Option<f64>)>,
    /// Until when (egui time, s) the insets are read often after the keyboard went away: it
    /// slides out after the last frame, and nothing else would draw one.
    #[cfg(target_os = "android")]
    insets_settle: f64,
    /// The activity-stack row of picked files being copied in, its ✕'s flag, and whether the
    /// activity was told to stop (see [`App::copy_row`]).
    #[cfg(target_os = "android")]
    copy_row: Option<(lightcraft_engine::activity::TaskGuard, std::sync::Arc<std::sync::atomic::AtomicBool>, bool)>,
}

impl App {
    fn new(app: LightcraftApp, prefs: lightcraft_ui_egui::prefs::UiPrefs) -> Self {
        App {
            app,
            prefs,
            touch_announced: false,
            #[cfg(any(target_os = "ios", target_os = "android"))]
            insets: egui::SafeAreaInsets::default(),
            #[cfg(target_os = "ios")]
            keyboard: 0.0,
            #[cfg(target_os = "android")]
            insets_polled: f64::NEG_INFINITY,
            #[cfg(target_os = "android")]
            text_input: None,
            #[cfg(target_os = "android")]
            field: android_jni::Field::default(),
            #[cfg(target_os = "android")]
            numeric_focus: None,
            #[cfg(target_os = "android")]
            insets_settle: f64::NEG_INFINITY,
            #[cfg(target_os = "android")]
            copy_row: None,
        }
    }

    /// iOS: the soft keyboard joins the bottom of the safe area, so the layout (bottom bar, edit
    /// sheet, dialogs) sits above it rather than under it.
    #[cfg(target_os = "ios")]
    fn keyboard_inset(&mut self, raw: &mut egui::RawInput) {
        if let Some(insets) = raw.safe_area_insets {
            self.insets = insets;
        }
        let keyboard = ios_scene::keyboard_height();
        if raw.safe_area_insets.is_some() || keyboard != self.keyboard {
            self.keyboard = keyboard;
            let mut insets = self.insets;
            insets.0.bottom = insets.0.bottom.max(keyboard);
            raw.safe_area_insets = Some(insets);
        }
    }

    /// Android: egui-winit reports no safe area here, so the shell does: the window edges that
    /// system bars, the display cutout and the soft keyboard cover (see `android_jni`). With
    /// the keyboard in the bottom inset, the layout (bottom bar, edit sheet, dialogs) sits above
    /// it. Polled a few times a second, more often while a text field has focus, since nothing
    /// signals a change.
    /// Picked files being copied into the app (the photo picker's, a folder's): a row in the
    /// activity stack with the count and the file, whose ✕ stops the copy after the current file
    /// (what was copied is imported).
    #[cfg(target_os = "android")]
    fn copy_row(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let picking = PICKING.load(Ordering::Relaxed) || DOC_PICKING.load(Ordering::Relaxed);
        let progress = if picking { ANDROID_APP.get().and_then(android_jni::copy_progress) } else { None };
        let Some((done, total, name)) = progress else {
            self.copy_row = None;
            return;
        };
        if self.copy_row.is_none() {
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let row = self.app.session.activity.start("import", "Copying picked files", lightcraft_engine::activity::Cancel::Flag(flag.clone()));
            self.copy_row = Some((row, flag, false));
        }
        if let Some((row, flag, stopped)) = self.copy_row.as_mut() {
            if total > 0 {
                row.progress(done, total);
            }
            row.detail(&name);
            if flag.load(Ordering::Relaxed) && !*stopped {
                *stopped = true;
                if let Some(app) = ANDROID_APP.get() {
                    android_jni::cancel_copy(app);
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    #[cfg(target_os = "android")]
    fn window_insets(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let now = raw.time.unwrap_or_default();
        let typing = ctx.egui_wants_keyboard_input() || now < self.insets_settle;
        if now < self.insets_polled + if typing { 0.08 } else { 0.5 } {
            return;
        }
        self.insets_polled = now;
        let Some(android) = ANDROID_APP.get() else { return };
        let Some(px) = android_jni::window_insets(android) else { return };
        let ppp = raw.viewports.get(&raw.viewport_id).and_then(|v| v.native_pixels_per_point).unwrap_or(1.0);
        let [left, top, right, bottom] = px.map(|p| p as f32 / ppp);
        let insets = egui::SafeAreaInsets(egui::epaint::MarginF32 { left, top, right, bottom });
        if insets != self.insets {
            self.insets = insets;
            raw.safe_area_insets = Some(insets);
        }
    }
}

/// Android: the soft keyboard follows egui's text fields. While one has focus (it asks for IME
/// input), the keyboard types into the activity's own text input, which supports composing
/// (CJK, Korean, Indic scripts), autocorrect and swipe typing; its edits arrive as egui IME events
/// (see `android_jni`). NativeActivity's own view only gets key events.
#[cfg(target_os = "android")]
impl App {
    /// After a frame: start, move or stop the text input with the focused text field.
    fn follow_text_field(&mut self, ctx: &egui::Context) {
        // the window insets are polled before frames (see `window_insets`): keep frames coming
        // while the keyboard can move (requested here, in the frame, so it isn't used up by it)
        if ctx.egui_wants_keyboard_input() || ctx.input(|i| i.time) < self.insets_settle {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(android) = ANDROID_APP.get() else { return };
        let ime = ctx.output(|o| o.ime.as_ref().map(|i| i.purpose));
        let wanted = ime.and_then(|_| ctx.memory(|m| m.focused()));
        let purpose = ime.unwrap_or_default();
        // the field's text, from what it reported this frame (focus, a change, a selection), and
        // its selection, from its state
        let reported = ctx.output(|o| {
            o.events.iter().rev().find_map(|e| {
                let info = e.widget_info();
                (info.role == egui::Role::TextInput).then(|| info.current_text_value.clone()).flatten()
            })
        });
        let field = |id: egui::Id, text: String| {
            let n = text.chars().count();
            let selection = egui::text_edit::TextEditState::load(ctx, id)
                .and_then(|s| s.cursor.char_range())
                .map(|r| r.as_sorted_char_range())
                .map_or((n, n), |r| (r.start.0.min(n), r.end.0.min(n)));
            android_jni::Field { text, selection, purpose, numeric: false }
        };
        // a drag value (or slider) reports itself when clicked, which focuses the text field it
        // turns into from the next frame on: remember which focus came with it
        let numeric = ctx.output(|o| {
            o.events.iter().find_map(|e| {
                let info = e.widget_info();
                matches!(info.role, egui::Role::SpinButton | egui::Role::Slider).then_some(info.value)
            })
        });
        if let Some(value) = numeric {
            self.numeric_focus = ctx.memory(|m| m.focused()).map(|id| (id, value));
        }
        // the same field, now for another purpose (e.g. a password shown or hidden): start afresh
        if wanted == self.text_input && purpose != self.field.purpose {
            self.text_input = None;
        }
        if wanted == self.text_input {
            if let Some(id) = wanted {
                let f = field(id, reported.unwrap_or_else(|| self.field.text.clone()));
                let f = android_jni::Field { numeric: self.field.numeric, ..f };
                if f != self.field {
                    android_jni::sync_text_input(android, &f);
                    self.field = f;
                }
            }
            return;
        }
        match wanted {
            Some(id) => {
                // a drag value (or slider) being typed into: the keyboard is the number pad
                let number = self.numeric_focus.filter(|(f, _)| *f == id).map(|(_, v)| v);
                // its text field reports no text when it starts: rebuild it from the reported
                // value (a field that reported nothing else starts empty; its first report syncs)
                let text = reported.or_else(|| number.flatten().and_then(|v| drag_value_text(ctx, id, v))).unwrap_or_default();
                self.field = field(id, text);
                self.field.numeric = number.is_some();
                android_jni::start_text_input(android, &self.field);
            }
            None => {
                android_jni::stop_text_input(android);
                self.insets_settle = ctx.input(|i| i.time) + 1.0;
            }
        }
        self.text_input = wanted;
    }

    /// Before a frame: the keyboard's edits since the last one.
    fn text_input_events(&mut self, raw: &mut egui::RawInput) {
        if self.text_input.is_none() {
            return;
        }
        if let Some(android) = ANDROID_APP.get() {
            raw.events.extend(android_jni::take_text_input(android).into_iter().map(egui::Event::Ime));
        }
    }
}

/// The text egui's drag value `id` shows for `value` as it starts being typed into. egui keeps that
/// text private, but selects all of it then, so its length is known: it's the value in egui's
/// number format with the one count of decimals that gives that length. `None` when none does
/// (e.g. a custom formatter).
#[cfg(target_os = "android")]
fn drag_value_text(ctx: &egui::Context, id: egui::Id, value: f64) -> Option<String> {
    let len = egui::text_edit::TextEditState::load(ctx, id)?.cursor.char_range()?.as_sorted_char_range().end.0;
    let style = ctx.global_style();
    let format = &style.number_formatter;
    (0..=15).map(|d| format.format(value, d..=d)).find(|t| t.chars().count() == len)
}

/// A photo pick is under way (Android: its result is polled, see `App::logic`).
#[cfg(target_os = "android")]
static PICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A document pick (a command's file or folder dialog) is under way: its files are copied in.
#[cfg(target_os = "android")]
static DOC_PICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The activity, for its window insets and the photo picker (set once by `android_main`).
#[cfg(target_os = "android")]
static ANDROID_APP: std::sync::OnceLock<winit::platform::android::activity::AndroidApp> = std::sync::OnceLock::new();

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // photos picked (asynchronously) and copied in: the usual Import review
        // while a pick is under way, look for its photos a few times a second (they're copied in
        // the background; nothing else wakes the app when they're in)
        #[cfg(target_os = "android")]
        let picked: Vec<Vec<String>> = if PICKING.load(std::sync::atomic::Ordering::Relaxed) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            let got = ANDROID_APP.get().and_then(android_jni::take_picked);
            if got.is_some() {
                PICKING.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            got.into_iter().filter(|p| !p.is_empty()).collect()
        } else {
            Vec::new()
        };
        #[cfg(target_os = "ios")]
        let picked = ios_picker::take_ready();
        #[cfg(any(target_os = "ios", target_os = "android"))]
        for paths in picked {
            if let Err(e) = self.app.run("file.addPhotos", serde_json::json!({ "paths": paths })) {
                log::warn!("import: {e}");
            }
        }
        #[cfg(target_os = "android")]
        self.copy_row(ctx);
        memory::poll(&mut self.app.session);
        self.app.logic(ctx);
        // the settings, a few seconds after a change (a phone may stop the app without asking)
        let now = ctx.input(|i| i.time);
        if let Some(e) = self.prefs.tick(&self.app.ui, now, self.app.library_problem.is_some()) {
            log::warn!("{e}");
            self.app.notices.push(e);
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // egui turns on touch drag-to-scroll once it has seen a touch, but hit-tests against the
        // previous frame, so the first swipe after launch would never scroll: announce the touch
        // screen up front with a cancelled touch
        if !std::mem::replace(&mut self.touch_announced, true) {
            raw.events.push(egui::Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(0),
                phase: egui::TouchPhase::Cancel,
                pos: egui::Pos2::ZERO,
                force: None,
            });
        }
        #[cfg(target_os = "ios")]
        {
            self.keyboard_inset(raw);
            // a hardware keyboard's keys (winit 0.30 passes only typed text on)
            ios_keys::input(raw, ctx.text_edit_focused());
            // a hovering Pencil or trackpad pointer, and the Pencil's double tap
            ios_pencil::input(raw);
            if ios_pencil::taken_tap() {
                self.app.pencil_tap(ctx);
            }
        }
        #[cfg(target_os = "android")]
        {
            self.window_insets(ctx, raw);
            self.text_input_events(raw);
        }
        self.app.raw_input_hook(ctx, raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        #[cfg(target_os = "ios")]
        ios_keys::after_frame(ui.ctx());
        #[cfg(target_os = "android")]
        self.follow_text_field(ui.ctx());
    }
    fn on_exit(&mut self) {
        if let Some(e) = self.prefs.save(&self.app.ui, self.app.library_problem.is_some()) {
            log::error!("{e}");
        }
        if let Err(e) = self.app.session.close_library() {
            log::error!("saving the library failed: {e}");
        }
    }
}

/// `export_dir`: where exports go by default (next to the library, inside the sandbox; on iOS
/// `Documents/`, so they show in the Files app too).
fn services(export_dir: String) -> Services {
    let saves = std::path::PathBuf::from(&export_dir);
    Services {
        export_dir: Some(export_dir),
        // a finished export opens the share sheet (AirDrop, Messages, Save to Photos, other apps)
        #[cfg(target_os = "ios")]
        share: Some(Box::new(ios_share::share)),
        #[cfg(target_os = "android")]
        share: Some(Box::new(|paths: &[String]| {
            if let Some(app) = ANDROID_APP.get() {
                android_jni::share(app, paths);
            }
        })),
        // atomic (temp file + sync + rename), inside the sandbox
        write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)),
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        // File ▸ Add Photos… shows the system picker; the photos arrive later (see `App::logic`)
        #[cfg(target_os = "ios")]
        pick_files: Some(Box::new(|| {
            ios_picker::present();
            Vec::new()
        })),
        #[cfg(target_os = "android")]
        pick_files: Some(Box::new(|| {
            if let Some(app) = ANDROID_APP.get() {
                let dir = app.internal_data_path().unwrap_or_default().join("LightCraft Library").join("Picked");
                PICKING.store(true, std::sync::atomic::Ordering::Relaxed);
                android_jni::pick_photos(app, &dir.to_string_lossy());
            }
            Vec::new()
        })),
        // the commands' file dialogs: the system's document picker, answered later
        picker: Some(Box::new(move |req| pick(req, &saves))),
        // iOS: a picked folder outside the sandbox is readable only for a while, so the library
        // copies it; Android's picker copies a folder into the library already
        import_copies: cfg!(target_os = "ios"),
        #[cfg(target_os = "ios")]
        open_url: Some(Box::new(|url: &str| https(url).and_then(ios_files::open_url))),
        #[cfg(target_os = "android")]
        open_url: Some(Box::new(|url: &str| {
            let app = ANDROID_APP.get().ok_or("no activity")?;
            https(url).and_then(|url| android_jni::open_url(app, url))
        })),
        ..Default::default()
    }
}

/// Only web links are opened (as on the desktop).
fn https(url: &str) -> Result<&str, String> {
    if url.starts_with("https://") { Ok(url) } else { Err("only https links are opened".into()) }
}

/// Where picked photos are kept: `<library>/Picked`, with the photo picker's (set once at start).
static PICKED_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The commands' file dialogs (`lightcraft_ui_egui::pick`). A save is answered at once with a free
/// name in `saves` (the exports folder), and the UI offers the saved file in the share sheet
/// (Save to Files, AirDrop, other apps); there's no save dialog to show first. File ▸ Import
/// Photos… keeps the photo library's picker (the synchronous `pick_files`). The rest shows the
/// system's document picker.
fn pick(req: lightcraft_ui_egui::pick::PickRequest, saves: &std::path::Path) -> Result<std::sync::mpsc::Receiver<Vec<String>>, String> {
    use lightcraft_ui_egui::pick::PickKind;
    let (tx, rx) = std::sync::mpsc::channel();
    match req.kind {
        PickKind::Save => {
            let name = req.file_name.as_deref().map(file_name).unwrap_or_else(|| "Untitled".into());
            std::fs::create_dir_all(saves).map_err(|e| format!("can't create {}: {e}", saves.display()))?;
            let _ = tx.send(vec![free_path(saves, &name).to_string_lossy().to_string()]);
            Ok(rx)
        }
        PickKind::Files if req.filter.as_ref().is_some_and(|f| f.1 == lightcraft_engine::import::EXTENSIONS) => {
            Err("photos come from the photo library".into())
        }
        #[cfg(target_os = "ios")]
        _ => ios_files::pick(&req),
        #[cfg(target_os = "android")]
        _ => {
            let app = ANDROID_APP.get().ok_or("no activity")?.clone();
            let kind = match req.kind {
                PickKind::Files => 0,
                PickKind::File => 1,
                _ => 2,
            };
            // files in the app's cache (photos among them move into the library); a folder's
            // photos and sidecars straight into the library, where they're added in place
            let dir = if kind == 2 {
                PICKED_DIR.get().cloned().unwrap_or_default()
            } else {
                // the activity's cacheDir, beside its filesDir
                let files = app.internal_data_path().unwrap_or_default();
                files.parent().unwrap_or(&files).join("cache").join("picked")
            };
            let mut exts: Vec<&str> = lightcraft_engine::import::EXTENSIONS.to_vec();
            exts.push("xmp");
            android_jni::pick_documents(&app, kind, &dir.to_string_lossy(), &exts)?;
            DOC_PICKING.store(true, std::sync::atomic::Ordering::Relaxed);
            // nothing calls back into Rust with the answer: poll for it off the UI thread
            std::thread::Builder::new()
                .name("document-picker".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        if let Some(paths) = android_jni::take_documents(&app) {
                            DOC_PICKING.store(false, std::sync::atomic::Ordering::Relaxed);
                            let _ = tx.send(settle_picked(paths));
                            if let Some(ctx) = android_jni::woken() {
                                ctx.request_repaint();
                            }
                            return;
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(rx)
        }
    }
}

/// `name` without directories (a suggested file name, e.g. a preset group's).
fn file_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '_' } else { c }).collect();
    if n.trim().is_empty() || n == "." || n == ".." { "Untitled".into() } else { n }
}

/// `dir/name`, numbered ("name 2.ext") when taken.
fn free_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = std::path::Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..10_000).map(|n| dir.join(format!("{stem} {n}{ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// Picked files are copies in the app's temporary storage, which the system may clear: photos
/// among them (a missing photo located, a watermark graphic) move into `<library>/Picked`, like
/// the photo picker's. The rest (presets, keyword lists, track logs) is read once.
pub(crate) fn settle_picked(paths: Vec<String>) -> Vec<String> {
    let Some(dir) = PICKED_DIR.get() else { return paths };
    paths
        .into_iter()
        .map(|p| {
            let path = std::path::Path::new(&p);
            let photo = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .is_some_and(|e| lightcraft_engine::import::EXTENSIONS.contains(&e.as_str()));
            if !photo || !path.is_file() || path.starts_with(dir) {
                return p;
            }
            let Some(name) = path.file_name() else { return p };
            let to = free_path(dir, &name.to_string_lossy());
            let moved =
                std::fs::create_dir_all(dir).and_then(|()| std::fs::rename(path, &to).or_else(|_| std::fs::copy(path, &to).map(|_| ()))).is_ok();
            if moved { to.to_string_lossy().to_string() } else { p }
        })
        .collect()
}

/// Logs to stderr on iOS (Xcode / `simctl launch --console-pty`), to logcat on Android (tag
/// `LightCraft`; Android discards stderr).
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            #[cfg(target_os = "android")]
            android_jni::logcat(r.level(), &format!("[{}] {}", r.target(), r.args()));
            #[cfg(not(target_os = "android"))]
            eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: StderrLog = StderrLog;

/// Asks for the adapter's own limits: eframe's defaults are above what the iOS Simulator's Metal
/// adapter reports (`max_inter_stage_shader_variables` 15 < 16), and device creation fails.
fn adapter_limits(options: &mut eframe::NativeOptions) {
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = &mut options.wgpu_options.wgpu_setup {
        n.device_descriptor = std::sync::Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("egui wgpu device"),
            required_limits: adapter.limits(),
            ..Default::default()
        });
    }
}

/// Runs the app with its library in `library_dir` and face and denoise models in `data_dir`
/// (`models/`, `denoise-models/`, as in the desktop's settings folder); returns when the event
/// loop ends.
pub fn run(mut options: eframe::NativeOptions, library_dir: std::path::PathBuf, data_dir: std::path::PathBuf) -> eframe::Result {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    adapter_limits(&mut options);
    #[cfg(target_os = "ios")]
    ios_scene::register();
    log::info!("library at {}", library_dir.display());
    eframe::run_native(
        "LightCraft",
        options,
        Box::new(move |_cc| {
            #[cfg(target_os = "ios")]
            if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(_cc) {
                ios_scene::set_window(h.as_raw());
                ios_keys::install(h.as_raw(), &_cc.egui_ctx);
                ios_pencil::install(h.as_raw(), &_cc.egui_ctx);
            }
            #[cfg(target_os = "android")]
            {
                if let Some(android) = ANDROID_APP.get() {
                    android_jni::register_wake(android, &_cc.egui_ctx);
                }
                // CJK typed with the keyboard (and Korean, which no bundled font has) shows in the
                // system's CJK font rather than as boxes
                if let Some((bytes, index)) = android_jni::system_cjk_font() {
                    let font = egui::FontData { index, ..egui::FontData::from_static(bytes) };
                    lightcraft_ui_egui::theme::add_fallback_font("system CJK", font);
                }
            }
            #[cfg(target_os = "ios")]
            {
                ios_scene::set_context(&_cc.egui_ctx);
                ios_files::init(_cc.egui_ctx.clone());
                ios_picker::init(library_dir.join("Picked"), _cc.egui_ctx.clone());
            }
            let export_dir = library_dir.with_file_name("LightCraft Exports").to_string_lossy().to_string();
            let _ = PICKED_DIR.set(library_dir.join("Picked"));
            // a library that can't be opened (unreadable, no room to create it) is reported in the
            // desktop's recovery window before anything can be edited, never a silent session
            let fresh = || Session::new().with_fs().with_system_clock().with_models_in(&data_dir);
            let (mut s, problem) = lightcraft_ui_egui::panels::library_problem::open_or_problem(fresh, &library_dir, true);
            memory::configure(&mut s, &_cc.egui_ctx);
            let mut app = LightcraftApp::new(s, services(export_dir));
            app.library_problem = problem;
            // the settings saved last time (language, shortcuts, panels …)
            let (prefs, saved, warning) = lightcraft_ui_egui::prefs::UiPrefs::load(data_dir.join("ui.json"));
            if let Some(ui) = saved {
                app.ui = ui;
            }
            lightcraft_ui_egui::i18n::set_language(app.ui.language);
            app.notices.extend(warning);
            if let Some(port) = control_port() {
                app = app.with_control(control_server::start(port, _cc.egui_ctx.clone()));
            }
            Ok(Box::new(App::new(app, prefs)))
        }),
    )
}

/// The control channel's port (`docs/control-protocol.md`), for driving the app from a test
/// script: `LIGHTCRAFT_CONTROL_PORT` on iOS (the Simulator passes `SIMCTL_CHILD_…` variables on;
/// it shares the Mac's loopback), the contents of `/data/local/tmp/lc_control` on Android (then
/// `adb forward tcp:PORT tcp:PORT`). Off unless asked for: nothing a user launches sets either.
fn control_port() -> Option<u16> {
    #[cfg(target_os = "android")]
    let v = std::fs::read_to_string("/data/local/tmp/lc_control").ok();
    #[cfg(not(target_os = "android"))]
    let v = std::env::var("LIGHTCRAFT_CONTROL_PORT").ok();
    v.and_then(|v| v.trim().parse().ok())
}

/// iOS: `$HOME` is the app container; `Documents/` shows up in the Files app, so models go in
/// `Library/Application Support/LightCraft` (as on the Mac), which doesn't.
#[cfg(target_os = "ios")]
pub fn ios_main() {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let data = home.join("Library").join("Application Support").join("LightCraft");
    if let Err(e) = run(eframe::NativeOptions::default(), home.join("Documents").join("LightCraft Library"), data) {
        log::error!("{e}");
    }
}

/// Android entry point, called by NativeActivity (via android-activity) on its own thread.
#[cfg(target_os = "android")]
#[allow(unsafe_code)] // the symbol name is the contract with android-activity
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let data = app.internal_data_path().unwrap_or_default();
    let dir = data.join("LightCraft Library");
    let _ = ANDROID_APP.set(app.clone());
    // the activity's onTrimMemory, passed on within a second whether or not frames are drawn
    let trims = app.clone();
    let _ = std::thread::Builder::new().name("trim-memory".into()).spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if android_jni::take_trim_level(&trims) > 0 {
                memory::warn();
            }
        }
    });
    let mut options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    // GL window unless /data/local/tmp/lc_vulkan exists: the emulator's Vulkan surface fails to
    // configure ("Invalid surface"); GPU compute (lightcraft-gpu) still picks Vulkan itself
    if !std::path::Path::new("/data/local/tmp/lc_vulkan").exists()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = &mut options.wgpu_options.wgpu_setup
    {
        n.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    if let Err(e) = run(options, dir, data) {
        log::error!("{e}");
    }
}
