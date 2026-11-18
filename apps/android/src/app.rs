//! Start-up and the frame loop of the Android app.

use std::path::PathBuf;
use std::sync::Arc;

use lcc_android_host::{AndroidApp, Host, ImeEvent, PickKind, PickRequest};
use lcc_engine::Session;
use lcc_ui_egui::panels::library_problem::LibraryProblem;
use lcc_ui_egui::soft_keyboard::{SoftInput, SoftKeyboard};
use lcc_ui_egui::{LccApp, Services};

use crate::prefs;

/// How long closing waits for renders that are running (as on the desktop).
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

pub fn main(activity: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag(lcc_brand::BINARY));
    let internal = activity.internal_data_path();
    let external = activity.external_data_path();
    let Some(config_root) = internal.clone().or_else(|| external.clone()) else {
        log::error!("the activity has no data folder; nothing can be saved");
        return;
    };
    // settings in the app-private folder; the library where the Files app can reach it
    lcc_brand::set_platform_dirs(lcc_brand::PlatformDirs {
        config_root: config_root.clone(),
        pictures: external.unwrap_or_else(|| config_root.clone()),
    });
    lcc_engine::guard::install_hook(Some(config_root.join(format!("{}-panics.log", lcc_brand::BINARY))));
    // freed decode buffers go back to the system at once, not at the allocator's next purge (a phone has no memory to
    // spare): after each decode, preview build and import, and on entering Develop
    lcc_engine::memory::set_release_hook(|| {
        let _ = lcc_sysmem::release_free_memory();
    });

    let host = match Host::new(&activity) {
        Ok(h) => Some(Arc::new(h)),
        Err(e) => {
            log::error!("no pickers or links: {e}");
            None
        }
    };
    let prefs_path = prefs::path();
    let loaded = prefs::load(prefs_path.as_deref());
    if let Some(ui) = &loaded.ui {
        lcc_ui_egui::i18n::set_language(ui.language);
    }
    let library = lcc_engine::library::default_dir();
    // before the UI makes its fonts
    crate::system_fonts::install();
    let keyboard_host = host.clone();
    let staging_dir = activity.external_data_path().map(|d| d.join(crate::staging::DIR_NAME));

    let options = eframe::NativeOptions { android_app: Some(activity), renderer: eframe::Renderer::Wgpu, ..Default::default() };
    let result = eframe::run_native(
        lcc_brand::DISPLAY_NAME,
        options,
        Box::new(move |cc| {
            let (session, problem) = open_session(library);
            if let Some(h) = &host {
                h.set_copy_progress(Arc::new(crate::copies::CopyRows::new(session.activity.clone())));
            }
            clean_staging(&session, staging_dir.clone());
            let mut app = LccApp::new(session.with_default_sam3_model(), services(cc, host.clone()));
            if let Some(ui) = loaded.ui {
                app.ui = ui;
            }
            lcc_ui_egui::i18n::set_language(app.ui.language);
            app.notices.extend(loaded.warning);
            if let Some(p) = problem {
                app.library_problem = Some(p);
            } else {
                lcc_ui_egui::panels::empty_library::open_on_empty_library(&mut app);
            }
            let writer = prefs::Writer::new(prefs_path, &app, loaded.keep_file);
            Ok(Box::new(App {
                app,
                writer,
                keyboard: Keyboard { host: keyboard_host, ..Default::default() },
                host,
                first_frame_told: false,
                last_frame: None,
            }))
        }),
    );
    if let Err(e) = result {
        log::error!("the app stopped: {e}");
    }
}

/// The library session in the default folder (`<external data>/<library name>`).
fn open_session(dir: Option<PathBuf>) -> (Session, Option<LibraryProblem>) {
    let fresh = || {
        let mut s = Session::new().with_fs().with_default_denoise_models().with_default_genfill().with_default_connections().with_system_clock();
        s.face_models_dir = lcc_engine::config::default_face_models_dir();
        s.ml_models_dir = lcc_engine::config::default_ml_models_dir();
        s.lens_db = lcc_engine::lens_download::LensSettings::load(lcc_engine::config::config_dir());
        s.slide_templates_dir = lcc_engine::config::config_dir().map(|d| d.join(lcc_engine::slide_template::DIR_NAME));
        s
    };
    let Some(dir) = dir else {
        return (fresh(), Some(LibraryProblem::new("", "There is no folder to keep the catalog in.")));
    };
    let mut s = fresh();
    match s.open_library(&dir, false) {
        Ok(r) => {
            let replayed = r.replayed;
            log::info!("library {}: {} photos, {replayed} log records replayed", dir.display(), s.catalog.len());
            (s, None)
        }
        Err(e) => {
            log::error!("can't open catalog {}: {e}", dir.display());
            (fresh(), Some(LibraryProblem::new(dir.to_string_lossy(), e.to_string())))
        }
    }
}

/// Deletes the staged copies of picked documents no library photo refers to, off the UI thread
/// (only inside the staging folder; see `staging`).
fn clean_staging(session: &Session, dir: Option<PathBuf>) {
    let Some(dir) = dir else { return };
    let keep: std::collections::HashSet<PathBuf> = session
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            lcc_catalog::Source::File { path } => Some(PathBuf::from(path)),
            _ => None,
        })
        .filter(|p| p.starts_with(&dir))
        .collect();
    // a library kept inside the staging folder would hold files the catalog doesn't list
    if session.library.as_ref().is_some_and(|l| l.dir.starts_with(&dir) || dir.starts_with(&l.dir)) {
        log::warn!("staging: the library shares {}; not cleaned", dir.display());
        return;
    }
    let spawned = std::thread::Builder::new().name("staging-clean".into()).spawn(move || {
        let done = crate::staging::clean(&dir, &keep, crate::staging::MIN_AGE, std::time::SystemTime::now());
        if done.files > 0 {
            log::info!("staging: deleted {} unused copies ({} bytes)", done.files, done.bytes);
        }
    });
    if let Err(e) = spawned {
        log::warn!("staging: no cleanup thread: {e}");
    }
}

/// What the activity does for the shared UI.
fn services(cc: &eframe::CreationContext<'_>, host: Option<Arc<Host>>) -> Services {
    if let Some(h) = &host {
        let ctx = cc.egui_ctx.clone();
        h.set_waker(Box::new(move || ctx.request_repaint()));
    }
    let picker_host = host.clone();
    let storage_host = host.clone();
    let roll_host = host.clone();
    let gallery_host = host.clone();
    let url_host = host;
    Services {
        // the system picker (Storage Access Framework); the activity copies what is picked into
        // the app's folders and answers with paths
        picker: picker_host.map(|h| -> lcc_ui_egui::pick::Picker {
            Box::new(move |req: lcc_ui_egui::pick::PickRequest| {
                use lcc_ui_egui::pick::PickKind as Ui;
                let kind = match req.kind {
                    Ui::Files => PickKind::Files,
                    Ui::File => PickKind::File,
                    Ui::Folder => PickKind::Folder,
                    Ui::Save => PickKind::Save,
                };
                let mime_types = req.filter.as_ref().map(|(_, exts)| PickRequest::mime_types_for(exts)).unwrap_or_default();
                let paths = h.pick(&PickRequest { kind, mime_types, file_name: req.file_name.clone() }).map_err(|e| e.to_string())?;
                // the UI's channel also carries dialog failures; the activity answers with paths only
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name("pick-answer".into())
                    .spawn(move || {
                        if let Ok(p) = paths.recv() {
                            let _ = tx.send(Ok(p));
                        }
                    })
                    .map_err(|e| format!("could not wait for the picker: {e}"))?;
                Ok(rx)
            })
        }),
        // "All files access", so folders on the device are read in place (Settings ▸ Folders)
        // (only in a build whose manifest declares it: the Play build doesn't, and copies picked folders)
        storage_access: storage_host.filter(|h| lcc_android_host::offers_all_files_access(h.all_files_access_declared())).map(|h| {
            let ask = h.clone();
            lcc_ui_egui::StorageAccess {
                granted: Box::new(move || h.has_all_files_access().unwrap_or(false)),
                ask: Box::new(move || ask.ask_all_files_access().map_err(|e| e.to_string())),
            }
        }),
        // the camera roll through MediaStore (asks for the photo permission; `file.addCameraRoll`)
        camera_roll: roll_host.map(|h| -> lcc_ui_egui::CameraRollFn { Box::new(move || h.camera_roll().map_err(|e| e.to_string())) }),
        // exports: copied into Pictures/<app> (MediaStore), then offered to the share sheet
        save_to_gallery: gallery_host.map(|h| -> lcc_ui_egui::SaveToGalleryFn {
            Box::new(move |paths: &[String], share: bool| h.save_to_gallery(paths, lcc_brand::DISPLAY_NAME, share).map_err(|e| e.to_string()))
        }),
        open_url: url_host.map(|h| -> lcc_ui_egui::OpenUrlFn { Box::new(move |url: &str| h.open_url(url).map_err(|e| e.to_string())) }),
        // atomic (temp file + sync + rename): a failed write never leaves a truncated file
        write_shared: Some(Arc::new(lcc_engine::export::write_file)),
        write: Some(Box::new(lcc_engine::export::write_file)),
        png: Some(Box::new(|img: &lcc_raster::Rgba8| {
            lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        ..Default::default()
    }
}

struct App {
    app: LccApp,
    writer: prefs::Writer,
    host: Option<Arc<Host>>,
    keyboard: Keyboard,
    /// Whether the activity was told the first frame is on screen (see `Host::first_frame_shown`).
    first_frame_told: bool,
    /// When the previous frame ran (egui time), to log frames resuming after a long gap.
    last_frame: Option<f64>,
}

/// The on-screen keyboard: what it typed goes into the frame's input; it shows while a text field
/// has focus.
#[derive(Default)]
struct Keyboard {
    host: Option<Arc<Host>>,
    state: SoftKeyboard,
    shown: bool,
}

impl Keyboard {
    fn input(&mut self, raw: &mut egui::RawInput) {
        for event in lcc_android_host::take_ime() {
            let input = match event {
                ImeEvent::Commit(s) => SoftInput::Commit(s),
                ImeEvent::Preedit(s) => SoftInput::Preedit(s),
                ImeEvent::FinishComposing => SoftInput::FinishComposing,
                ImeEvent::DeleteSurrounding { before, after } => SoftInput::DeleteSurrounding { before, after },
                ImeEvent::Backspace => SoftInput::Backspace,
                ImeEvent::Delete => SoftInput::Delete,
                ImeEvent::Enter => SoftInput::Enter,
            };
            self.state.events(input, &mut raw.events);
        }
    }

    /// Shows or hides the keyboard when whether a text field wants it changed (after the frame).
    fn follow(&mut self, ctx: &egui::Context) {
        let wanted = SoftKeyboard::wanted(ctx);
        if wanted == self.shown {
            return;
        }
        self.shown = wanted;
        if !wanted {
            self.state.reset();
        }
        if let Some(h) = &self.host
            && let Err(e) = h.set_keyboard(wanted)
        {
            log::warn!("keyboard: {e}");
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        // a picker's answer that came after the system restarted the app (see `Unclaimed`)
        for u in self.host.as_ref().map(|h| h.take_unclaimed()).unwrap_or_default() {
            use lcc_ui_egui::pick::PickKind as Ui;
            let kind = match u.kind {
                PickKind::Files => Ui::Files,
                PickKind::File => Ui::File,
                PickKind::Folder => Ui::Folder,
                PickKind::Save => Ui::Save,
            };
            lcc_ui_egui::pick::answer_after_restart(&mut self.app, ctx, kind, u.paths);
        }
        let now = ctx.input(|i| i.time);
        // frames stop while the activity is in the background; log when they come back so a
        // freeze after resuming shows in logcat as "no frame after resume"
        if let Some(prev) = self.last_frame
            && now - prev > 2.0
        {
            log::info!("lifecycle: frames resumed after {:.1} s", now - prev);
        }
        self.last_frame = Some(now);
        self.writer.tick(&mut self.app, now);
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.keyboard.input(raw);
        // the camera cutout and the system bars: the UI keeps clear of them
        if let Some(px) = lcc_android_host::insets() {
            self.app.ui.modern.insets = lcc_ui_egui::modern::Insets::from_px(px, ctx.pixels_per_point());
        }
        // Back (the predictive-back callback): as the Back key, which the UI turns into touch.back
        for _ in 0..lcc_android_host::take_back() {
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key: egui::Key::BrowserBack,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        // photos opened, shared or dropped from other apps take the desktop's drop path: presets
        // import as presets, photos open the import review
        if let Some(h) = &self.host {
            raw.dropped_files.extend(h.take_opened().into_iter().map(|p| -> egui::DroppedFileHandle { Arc::new(SharedFile(p.into())) }));
        }
        self.app.raw_input_hook(raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.perf.host_frame_ms = frame.info().cpu_usage.map(|s| f64::from(s) * 1000.0);
        self.app.ui(ui);
        self.keyboard.follow(ui.ctx());
        if !self.first_frame_told {
            self.first_frame_told = true;
            if let Some(h) = &self.host
                && let Err(e) = h.first_frame_shown()
            {
                log::warn!("first frame: {e}");
            }
        }
        // Back with nothing left to close: the system's Back (the app goes to the background)
        if lcc_ui_egui::modern::take_system_back(&mut self.app)
            && let Some(h) = &self.host
            && let Err(e) = h.go_background()
        {
            log::warn!("back: {e}");
        }
    }
    fn on_exit(&mut self) {
        if let Err(e) = self.writer.save(&self.app) {
            log::error!("{e}");
        }
        if let Err(e) = self.app.session.close_library() {
            log::error!("saving the catalog failed: {e}");
        }
        if !self.app.shutdown(EXIT_WAIT) {
            log::warn!("close: background work was still running after {EXIT_WAIT:?}");
        }
    }
}

/// A photo another app opened, shared or dropped in, as the activity copied it.
#[derive(Debug)]
struct SharedFile(PathBuf);

impl egui::DroppedFile for SharedFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| format!("{}: {e}", self.0.display()))
    }
}
