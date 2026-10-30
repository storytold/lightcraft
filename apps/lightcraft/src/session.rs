#[cfg(any(target_os = "freebsd", target_os = "linux"))]
use super::platform::DIALOG_PARENT;
use super::platform::{explorer_select_command, file_dialog, windows_open_url_command};
use super::prefs::display_profile_dir;
use super::*;

/// The platform services; `cc` is the window the file dialogs belong to (its `egui_ctx` repaints
/// when a dialog closes), and `log_file` is for Help ▸ Open Log Folder.
pub(crate) fn services(cc: &eframe::CreationContext<'_>, log_file: Option<&std::path::Path>) -> Services {
    let ctx = cc.egui_ctx.clone();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let _ = DIALOG_PARENT.set(rfd::FileDialog::new().set_parent(cc));
    Services {
        local_places: None,
        // Commands' file dialogs run on their own thread (#191): a dialog on the UI thread
        // stopped the window answering the compositor, which then reported the app as hung.
        picker: Some(Box::new(move |req: lightcraft_ui_egui::pick::PickRequest| {
            use lightcraft_ui_egui::pick::PickKind;
            let (tx, rx) = std::sync::mpsc::channel();
            let repaint = ctx.clone();
            std::thread::Builder::new()
                .name("file-dialog".into())
                .spawn(move || {
                    let mut d = file_dialog();
                    if let Some(t) = &req.title {
                        d = d.set_title(t);
                    }
                    if let Some((name, exts)) = &req.filter {
                        d = d.add_filter_nocase(name.clone(), exts);
                    }
                    if let Some(n) = &req.file_name {
                        d = d.set_file_name(n);
                    }
                    let s = |p: std::path::PathBuf| p.to_string_lossy().to_string();
                    let paths: Vec<String> = match req.kind {
                        PickKind::Files => d.pick_files().unwrap_or_default().into_iter().map(s).collect(),
                        PickKind::File => d.pick_file().map(s).into_iter().collect(),
                        PickKind::Folder => d.pick_folder().map(s).into_iter().collect(),
                        PickKind::Save => d.save_file().map(s).into_iter().collect(),
                    };
                    // the app may have quit meanwhile: nobody to tell
                    let _ = tx.send(paths);
                    repaint.request_repaint();
                })
                .map_err(|e| format!("could not start the file dialog: {e}"))?;
            Ok(rx)
        })),
        log_file: log_file.map(|p| p.display().to_string()),
        pick_lightroom_catalog: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Lightroom Catalog"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Lightroom Classic Catalog"), &["lrcat"])
                .pick_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_folder: Some(Box::new(|| {
            file_dialog().set_title(lightcraft_ui_egui::i18n::tr("Open Library")).pick_folder().map(|p| p.to_string_lossy().to_string())
        })),
        #[cfg(target_os = "linux")]
        open_model_folder: Some(std::sync::Arc::new(|path: &str| {
            std::process::Command::new("xdg-open")
                .arg(path)
                .status()
                .map_err(|e| e.to_string())
                .and_then(|s| if s.success() { Ok(()) } else { Err(format!("opening folder failed: {s}")) })
        })),
        open_with: Some(Box::new(editors::open_with)),
        open_all_with: Some(Box::new(editors::open_all_with)),
        check_editor: Some(Box::new(editors::check)),
        open_url: Some(Box::new(|url: &str| {
            if !url.starts_with("https://") {
                return Err("only https links are opened".into());
            }
            let status = if cfg!(target_os = "macos") {
                std::process::Command::new("open").arg(url).status()
            } else if cfg!(target_os = "windows") {
                windows_open_url_command(url).status()
            } else {
                std::process::Command::new("xdg-open").arg(url).status()
            };
            status.map(|_| ()).map_err(|e| e.to_string())
        })),
        reveal: Some(Box::new(|path: &str| {
            let status = if cfg!(target_os = "macos") {
                std::process::Command::new("open").args(["-R", path]).status()
            } else if cfg!(target_os = "windows") {
                explorer_select_command(path).status()
            } else {
                let dir = std::path::Path::new(path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
                std::process::Command::new("xdg-open").arg(dir).status()
            };
            status
                .map_err(|e| e.to_string())
                .and_then(|s| if s.success() || cfg!(target_os = "windows") { Ok(()) } else { Err(format!("reveal failed: {s}")) })
        })),
        pick_files: Some(Box::new(|| {
            file_dialog()
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Photos"), lightcraft_engine::import::EXTENSIONS)
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })),
        pick_denoise_model: Some(Box::new(|| {
            file_dialog()
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("ONNX model"), &["onnx"])
                .pick_file()
                .map(|p| vec![p.to_string_lossy().into_owned()])
                .unwrap_or_default()
        })),
        pick_preset_files: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Presets"))
                .add_filter_nocase(
                    lightcraft_ui_egui::i18n::tr("Presets & Profiles"),
                    &["lcpreset", "xmp", "lrtemplate", "zip", "dng", "lmp", "mplumpack", "cube"],
                )
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })),
        pick_model_file: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Add a Face Recognition Model"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Face models (ONNX)"), &["onnx"])
                .pick_file()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default()
        })),
        pick_tracklog: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Auto-Tag from Tracklog"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("GPS Track Log"), &["gpx"])
                .pick_file()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default()
        })),
        // macOS doesn't colour-manage the window: previews follow the screen's profile (#787)
        #[cfg(target_os = "macos")]
        screen_profile: Some(Box::new(screen_profile::icc)),
        // Linux: the profile the desktop assigns in colord, read off the UI thread
        #[cfg(target_os = "linux")]
        screen_profile: Some(colord::service()),
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        screen_profile: None,
        pick_display_profile: Some(Box::new(|| {
            let mut d = file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Choose Monitor Profile"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("ICC Profiles"), &["icc", "icm"]);
            if let Some(dir) = display_profile_dir() {
                d = d.set_directory(dir);
            }
            d.pick_file().map(|p| vec![p.to_string_lossy().to_string()]).unwrap_or_default()
        })),
        save_preset_file: Some(Box::new(|name: &str| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Export Presets"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("LightCraft Preset"), &["lcpreset"])
                .set_file_name(name)
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_keyword_list: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Keywords"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Keyword Lists"), &["txt", "utf8"])
                .pick_file()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default()
        })),
        save_keyword_list: Some(Box::new(|name: &str| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Export Keywords"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Keyword Lists"), &["txt"])
                .set_file_name(name)
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_curve_preset_files: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Point Curve Presets"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Point Curve Presets"), &["lccurve", "json"])
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })),
        save_curve_preset_file: Some(Box::new(|name: &str| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Export Point Curve Presets"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Point Curve Presets"), &["lccurve"])
                .set_file_name(name)
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_slideshow_templates: Some(Box::new(|| {
            rfd::FileDialog::new()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Slideshow Templates"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Slideshow Templates"), &["json"])
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })),
        save_slideshow_template: Some(Box::new(|name: &str| {
            rfd::FileDialog::new()
                .set_title(lightcraft_ui_egui::i18n::tr("Export Slideshow Template"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Slideshow Templates"), &["json"])
                .set_file_name(name)
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_print_templates: Some(Box::new(|| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Import Print Templates"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Print Templates"), &[lightcraft_engine::print::template::FILE_EXT])
                .pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })),
        save_print_template: Some(Box::new(|name: &str| {
            file_dialog()
                .set_title(lightcraft_ui_egui::i18n::tr("Export Print Template"))
                .add_filter_nocase(lightcraft_ui_egui::i18n::tr("Print Templates"), &[lightcraft_engine::print::template::FILE_EXT])
                .set_file_name(name)
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        // atomic (temp file + sync + rename): a failed write never leaves a truncated file
        write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)),
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        // the library is a folder on disk: backed up with the user's other files
        backup_library: None,
        restore_library: None,
        system_theme: appearance::service(),
    }
}

/// The library session with the folders face models (`<config>/models`) and slideshow templates
/// (`<config>/slideshow-templates`) are kept in. With [`Launch::Ask`] no library is opened yet:
/// the window lists the recent ones.
pub(crate) fn open_session(in_memory: bool, launch: &Launch, seed_demo: bool) -> (Session, Option<LibraryProblem>) {
    let (mut s, problem) = match launch {
        Launch::Ask => (Session::new().with_fs().with_default_denoise_models().with_system_clock(), None),
        Launch::Open { dir, .. } => open_library_session(in_memory, dir.clone(), seed_demo),
    };
    s.face_models_dir = lightcraft_engine::config::default_face_models_dir();
    s.slide_templates_dir = lightcraft_engine::config::config_dir().map(|d| d.join(lightcraft_engine::slide_template::DIR_NAME));
    // Control requests are answered on the UI thread: Add to Apple Photos must not wait there
    s.apple_photos_background = true;
    (s, problem)
}

/// The persistent library session, or with `--memory` an in-memory one (demo photos).
///
/// If the library can't be opened the session is empty and in memory — never seeded with the
/// demo photos, never written anywhere — and the problem is returned: the window then says so
/// and offers Try Again / Choose Another Library… / Continue Without Saving / Quit (issue #100).
fn open_library_session(in_memory: bool, dir: Option<std::path::PathBuf>, seed_demo: bool) -> (Session, Option<LibraryProblem>) {
    if in_memory {
        return (if seed_demo { Session::with_demo() } else { Session::new() }.with_fs().with_default_denoise_models().with_system_clock(), None);
    }
    let unopened = || Session::new().with_fs().with_default_denoise_models().with_system_clock();
    let Some(dir) = dir else {
        log::warn!("no library location (set --library or LIGHTCRAFT_LIBRARY)");
        return (unopened(), Some(LibraryProblem::new("", "There is no home folder to keep the library in. Choose a folder for it.")));
    };
    let t0 = std::time::Instant::now();
    let mut s = Session::new().with_fs().with_default_denoise_models().with_system_clock();
    match s.open_library(&dir, seed_demo) {
        Ok(r) => {
            let (replayed, torn) = (r.replayed, r.torn_bytes);
            log::info!(
                "library {}: {} photos, {replayed} log records replayed{} ({:.1} ms)",
                dir.display(),
                s.catalog.len(),
                if torn > 0 { ", torn tail repaired" } else { "" },
                t0.elapsed().as_secs_f64() * 1000.0
            );
            (s, None)
        }
        Err(e) => {
            log::error!("can't open library {}: {e}", dir.display());
            (unopened(), Some(LibraryProblem::new(dir.to_string_lossy(), e.to_string())))
        }
    }
}
