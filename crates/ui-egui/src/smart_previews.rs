//! Show the storage estimate before a Lossless build. Probe the destination on a worker so
//! an offline volume cannot block the UI. The confirmed build keeps the displayed selection.

use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::Dialog;

const BYTES_PER_PHOTO: u64 = 39_500_000;
pub(crate) type SpaceCheck = std::sync::mpsc::Receiver<Result<u64, String>>;

pub(crate) fn intercept(app: &mut LightcraftApp, command: &str, params: &Value) -> Option<Result<Value, String>> {
    if command != "library.smartPreviews" || params["discard"] == true || cfg!(target_arch = "wasm32") {
        return None;
    }
    let format = match params.get("format") {
        Some(v) => lightcraft_engine::smart::Format::parse(v.as_str()?)?,
        None => app.session.smart_previews_format,
    };
    if format != lightcraft_engine::smart::Format::Lossless
        || app.session.preview_build.as_ref().is_some_and(|b| !b.finished.load(std::sync::atomic::Ordering::Relaxed))
    {
        return None;
    }
    let path = app.session.media.smart_dir.clone()?;
    let ids = if params.get("ids").is_some() || params.get("id").is_some() || !app.session.selection.ids.is_empty() {
        match app.session.targets(params) {
            Ok(ids) => ids,
            Err(error) => return Some(Err(error.to_string())),
        }
    } else {
        app.session.visible_cloned()
    };
    let ids: Vec<_> = ids
        .into_iter()
        .filter(|id| app.session.catalog.photo(*id).is_some_and(|p| matches!(p.source, lightcraft_catalog::Source::File { .. })))
        .map(|id| id.0)
        .collect();
    let count = ids.len();
    let estimated_bytes = (count as u64).saturating_mul(BYTES_PER_PHOTO);
    let mut params = params.clone();
    if !params.is_object() {
        params = json!({});
    }
    params["ids"] = json!(ids);
    params["format"] = json!("lossless");
    let custom = app.session.smart_previews_dir.is_some();
    let (tx, rx) = std::sync::mpsc::channel();
    let folder = path.clone();
    let probe = move || {
        let _ = tx.send(available_space(&folder, custom));
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("lc-smart-space".into()).spawn(probe) {
        return Some(Err(crate::i18n::tr_format!("could not check smart preview storage: {e}", e = e)));
    }
    #[cfg(target_arch = "wasm32")]
    probe();
    app.smart_preview_space = Some(rx);
    app.ui.dialog = Some(Dialog::BuildSmartPreviews { params, path, count, estimated_bytes, checking: true, free_bytes: None, space_error: None });
    Some(Ok(json!({"confirmationRequired": true, "count": count, "estimatedBytes": estimated_bytes})))
}

fn available_space(path: &std::path::Path, custom: bool) -> Result<u64, String> {
    // The default folder may not exist until the first build. Never substitute a parent
    // volume for a missing user-selected folder on a disconnected drive.
    let folder = if !custom && !path.exists() { path.parent().unwrap_or(path) } else { path };
    #[cfg(any(unix, windows))]
    {
        fs4::available_space(folder).map_err(|e| format!("{}: {e}", folder.display()))
    }
    #[cfg(not(any(unix, windows)))]
    Err(crate::i18n::tr_format!("free space is unavailable for {path} on this platform", path = folder.display()))
}

pub(crate) fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(result) = app.session.smart_migration_result.take() {
        ctx.data_mut(|d| d.remove::<(f64, Value)>(egui::Id::unique("smart-location")));
        let message = match result {
            Err(error) => crate::i18n::tr_format!("Smart preview folder change failed: {error}", error = error),
            Ok(result) if result["cancelled"] == true => {
                crate::i18n::tr("Smart preview folder change cancelled. The old folder is still in use.").to_string()
            }
            Ok(result) => {
                let count = result["failed"].as_array().map_or(0, Vec::len);
                match count {
                    0 => crate::i18n::tr("Smart preview folder changed.").to_string(),
                    _ => crate::i18n::tr_format!("Smart preview folder changed. {count} source files could not be removed.", count = count),
                }
            }
        };
        app.toast(ctx, message);
    }

    let Some(Dialog::BuildSmartPreviews { checking, free_bytes, space_error, .. }) = &mut app.ui.dialog else {
        app.smart_preview_space = None;
        return;
    };
    let Some(rx) = &app.smart_preview_space else { return };
    let result = match rx.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(_) => Err(crate::i18n::tr("the free-space check stopped unexpectedly").to_string()),
    };
    *checking = false;
    match result {
        Ok(bytes) => *free_bytes = Some(bytes),
        Err(error) => *space_error = Some(error),
    }
    app.smart_preview_space = None;
}

pub(crate) fn exceeds_free_space(dialog: &Dialog) -> bool {
    matches!(dialog, Dialog::BuildSmartPreviews { estimated_bytes, free_bytes: Some(free), .. } if estimated_bytes > free)
}

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.2} GB", bytes as f64 / 1_000_000_000.0) } else { format!("{:.1} MB", bytes as f64 / 1_000_000.0) }
}

pub(crate) fn body(ui: &mut egui::Ui, dialog: &Dialog) {
    let Dialog::BuildSmartPreviews { path, count, estimated_bytes, checking, free_bytes, space_error, .. } = dialog else { return };
    ui.label(crate::i18n::tr_format!("Photos: {count}", count = *count));
    ui.label(crate::i18n::tr_format!("Estimated total size: {size}", size = size(*estimated_bytes)));
    ui.label(crate::i18n::tr(
        "About 39.5 MB per photo, measured on an A7CR proxy. Actual sizes vary; existing previews may reduce the space needed.",
    ));
    ui.add_space(6.0);
    ui.label(crate::i18n::tr_format!("Destination: {path}", path = path.display()));
    if *checking {
        ui.spinner();
        ui.label(crate::i18n::tr("Checking free space…"));
    } else if let Some(free) = free_bytes {
        ui.label(crate::i18n::tr_format!("Free space: {size}", size = size(*free)));
        if exceeds_free_space(dialog) {
            ui.colored_label(
                crate::theme::Tokens::get(ui.ctx()).reject,
                crate::i18n::tr("The estimated build will not fit in the available space. Free space or choose another folder before building."),
            );
        }
    } else {
        ui.label(crate::i18n::tr("Free space is unavailable. Check the destination drive before building."));
        if let Some(error) = space_error {
            ui.label(error);
        }
    }
    ui.add_space(6.0);
    ui.label(crate::i18n::tr("Previews are rebuilt from available originals. Offline originals keep their existing previews."));
}

pub(crate) fn confirm(app: &mut LightcraftApp, dialog: &Dialog) -> Result<Value, String> {
    let Dialog::BuildSmartPreviews { params, path, checking, .. } = dialog else {
        return Err(crate::i18n::tr("no smart preview build to confirm").to_string());
    };
    if *checking {
        return Err(crate::i18n::tr("wait for the free-space check to finish").to_string());
    }
    if app.session.media.smart_dir.as_ref() != Some(path) {
        return Err(crate::i18n::tr("the smart preview folder changed; start the build again to check its free space").to_string());
    }
    app.session.execute("library.smartPreviews", params).map_err(|e| e.to_string())
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    use lightcraft_engine::smart::Format;

    fn app(tag: &str) -> (LightcraftApp, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("lc-smart-cost-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let mut session = lightcraft_engine::Session::new();
        session.media.smart_dir = Some(dir.join("Smart Previews"));
        session.smart_previews_format = Format::Lossless;
        session.media.file_loader = Some(std::sync::Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(8, 8), Default::default()))));
        for id in 1..=2 {
            let p = Photo::new(PhotoId(id), Source::File { path: format!("fixture-{id}.tif") }, "fixture.tif", "TIFF", 8, 8, "2026-01-01");
            session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        session.selection.ids = vec![PhotoId(1)];
        session.selection.active = Some(PhotoId(1));
        (LightcraftApp::new(session, Default::default()), dir)
    }

    fn finish_check(app: &mut LightcraftApp) {
        let ctx = egui::Context::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.smart_preview_space.is_some() {
            poll(app, &ctx);
            assert!(std::time::Instant::now() < until, "storage check timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn lossless_menu_waits_for_cost_confirmation_and_keeps_the_displayed_targets() {
        let (mut app, dir) = app("menu");
        let result = crate::menubar::run_item(&mut app, "library.smartPreviews", json!({})).unwrap();
        assert_eq!(result["confirmationRequired"], true);
        assert_eq!(result["estimatedBytes"], BYTES_PER_PHOTO);
        assert!(!dir.join("Smart Previews").exists(), "preflight never writes previews");
        let waiting = app.ui.dialog.clone().unwrap();
        assert!(confirm(&mut app, &waiting).is_err(), "must finish the space check first");
        finish_check(&mut app);
        let dialog = app.ui.dialog.clone().unwrap();
        assert!(matches!(dialog, Dialog::BuildSmartPreviews { count: 1, free_bytes: Some(_), checking: false, .. }));
        app.session.selection.ids = vec![PhotoId(2)];
        let built = confirm(&mut app, &dialog).unwrap();
        assert_eq!(built["built"], 1);
        let folder = app.session.media.smart_dir.as_ref().unwrap();
        assert!(lightcraft_engine::smart::file_path(folder, app.session.catalog.photo(PhotoId(1)).unwrap()).exists());
        assert!(!lightcraft_engine::smart::file_path(folder, app.session.catalog.photo(PhotoId(2)).unwrap()).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lossless_override_and_all_photos_share_the_cost_dialog() {
        let (mut app, dir) = app("all");
        app.session.smart_previews_format = Format::Compact;
        let r = app.run("library.smartPreviews", json!({"ids": [1, 2], "format": "lossless", "background": true})).unwrap();
        assert_eq!(r["count"], 2);
        assert_eq!(r["estimatedBytes"], 2 * BYTES_PER_PHOTO);
        finish_check(&mut app);
        let mut dialog = app.ui.dialog.clone().unwrap();
        if let Dialog::BuildSmartPreviews { free_bytes, .. } = &mut dialog {
            *free_bytes = Some(BYTES_PER_PHOTO);
        }
        assert!(exceeds_free_space(&dialog));
        app.ui.dialog = None; // Cancel preserves the files and starts no build.
        poll(&mut app, &egui::Context::default());
        assert!(app.session.preview_build.is_none());
        assert!(!dir.join("Smart Previews").exists());
        assert!(intercept(&mut app, "library.smartPreviews", &json!({})).is_none(), "Compact stays immediate");
        assert!(intercept(&mut app, "library.smartPreviews", &json!({"discard": true, "format": "lossless"})).is_none());
        assert!(available_space(&dir.join("missing/volume"), true).is_err(), "never report the system drive for a missing custom folder");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lossless_confirmation_validates_explicit_targets_before_checking_storage() {
        let (mut app, dir) = app("invalid-targets");
        app.session.selection.ids.clear();
        app.session.selection.active = None;
        for params in [json!({"ids": "bad"}), json!({"ids": [1, "bad"]}), json!({"id": "bad"})] {
            let error = intercept(&mut app, "library.smartPreviews", &params).unwrap().unwrap_err();
            assert!(error.contains("photo ids"), "{error}");
            assert!(app.ui.dialog.is_none());
            assert!(app.smart_preview_space.is_none());
            assert!(app.session.preview_build.is_none());
        }
        let result = intercept(&mut app, "library.smartPreviews", &json!({"id": 2})).unwrap().unwrap();
        assert_eq!(result["count"], 1);
        let Some(Dialog::BuildSmartPreviews { params, .. }) = &app.ui.dialog else { panic!("cost dialog missing") };
        assert_eq!(params["ids"], json!([2]), "an explicit id never expands to every visible photo");
        finish_check(&mut app);
        assert!(!dir.join("Smart Previews").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lossless_storage_dialog_translations_fit_small_and_large_windows() {
        let (mut app, dir) = app("localized-layout");
        app.ui.settings.gpu = false;
        app.run("library.smartPreviews", json!({"ids": [1, 2]})).unwrap();
        finish_check(&mut app);
        let mut h = crate::headless::Headless::new(app, [800.0, 600.0], 1.0);
        // Low free space confirms with Build Anyway; the usual case, enough space, with Build.
        for (free, confirm) in [(1, "Build Anyway"), (1_000_000_000_000, "Build")] {
            if let Some(Dialog::BuildSmartPreviews { free_bytes, .. }) = &mut h.app.ui.dialog {
                *free_bytes = Some(free);
            }
            for locale in crate::i18n::Locale::ALL {
                h.app.ui.language = *locale;
                for (w, height) in [(800.0, 600.0), (1600.0, 1000.0)] {
                    h.size = egui::vec2(w, height);
                    h.settle(std::time::Duration::from_secs(10));
                    let painted = h.painted_text();
                    let text = painted.join("\n");
                    for key in [
                        "Build Lossless Smart Previews",
                        confirm,
                        "Previews are rebuilt from available originals. Offline originals keep their existing previews.",
                    ] {
                        let translated = locale.tr(key);
                        if *locale != crate::i18n::Locale::En {
                            assert_ne!(translated, key, "{locale:?}: missing translation");
                        }
                        assert!(text.contains(translated), "{locale:?}: missing {translated:?} in {text}");
                    }
                    assert!(painted.iter().any(|line| line == locale.tr(confirm)), "{locale:?}: no {confirm:?} button in {text}");
                    fn check_title(shape: &egui::epaint::Shape, title: &str) {
                        match shape {
                            egui::epaint::Shape::Text(text) if text.galley.job.text == title => {
                                assert!(!text.galley.elided, "dialog title was cut short: {title}");
                            }
                            egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| check_title(shape, title)),
                            _ => {}
                        }
                    }
                    for shape in &h.view.shapes {
                        check_title(&shape.shape, locale.tr("Build Lossless Smart Previews"));
                    }
                    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
                    for id in ["dialog:window", "button:dialogOk", "button:dialogCancel"] {
                        let rect = h.app.widgets.iter().find(|(key, _)| key == id).unwrap().1;
                        assert!(screen.contains_rect(rect), "{locale:?}: {id} outside {screen:?}: {rect:?}");
                    }
                    if let Some(output) = std::env::var_os("LC_SMART_SCREENSHOTS") {
                        let pixels = h.view.paint(&Default::default());
                        let image = lightcraft_raster::Rgba8 {
                            width: pixels.size[0],
                            height: pixels.size[1],
                            data: pixels.pixels.iter().map(|p| p.to_array()).collect(),
                        };
                        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
                        std::fs::write(std::path::PathBuf::from(output).join(format!("storage-{}-{free}-{w}.png", locale.code())), bytes).unwrap();
                    }
                }
            }
        }
        crate::i18n::set_language(crate::i18n::Locale::En);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn smart_preview_labels_are_translated_in_every_language() {
        let mut labels: Vec<&str> =
            lightcraft_engine::command_specs().iter().filter(|spec| spec.id.contains("smartPreview")).map(|spec| spec.label).collect();
        assert!(labels.contains(&"Smart Preview Format"), "{labels:?}");
        labels.extend(["Build", "Build Anyway", "Build for All Photos", "Build Lossless Smart Previews"]);
        for locale in crate::i18n::Locale::ALL.iter().filter(|locale| **locale != crate::i18n::Locale::En) {
            for label in &labels {
                assert_ne!(locale.tr(label), *label, "{locale:?}: {label:?} is not translated");
            }
        }
    }

    #[test]
    fn lossless_storage_dialog_shows_cost_and_space_in_small_and_large_windows() {
        let (mut app, dir) = app("layout");
        app.ui.settings.gpu = false;
        app.run("library.smartPreviews", json!({"ids": [1, 2]})).unwrap();
        finish_check(&mut app);
        let mut h = crate::headless::Headless::new(app, [800.0, 600.0], 1.0);
        for (name, free, error) in [
            ("enough", Some(1_000_000_000), None),
            ("low", Some(1_000_000), None),
            ("unknown", None, Some("destination drive is unavailable".to_string())),
        ] {
            if let Some(Dialog::BuildSmartPreviews { free_bytes, space_error, .. }) = &mut h.app.ui.dialog {
                *free_bytes = free;
                *space_error = error;
            }
            for (w, height) in [(800.0, 600.0), (1600.0, 1000.0)] {
                h.size = egui::vec2(w, height);
                h.settle(std::time::Duration::from_secs(10));
                let text = h.painted_text().join("\n");
                assert!(text.contains("Estimated total size: 79.0 MB"), "{text}");
                assert!(text.contains("39.5 MB per photo"));
                match name {
                    "enough" => assert!(text.contains("Free space: 1.00 GB")),
                    "low" => assert!(text.contains("will not fit") && text.contains("Build Anyway")),
                    _ => assert!(text.contains("Free space is unavailable")),
                }
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
                for id in ["dialog:window", "button:dialogOk", "button:dialogCancel"] {
                    let rect = h.app.widgets.iter().find(|(key, _)| key == id).unwrap().1;
                    assert!(screen.contains_rect(rect), "{id} is outside {screen:?}: {rect:?}");
                }
                if let Some(output) = std::env::var_os("LC_SMART_SCREENSHOTS") {
                    let pixels = h.view.paint(&Default::default());
                    let image = lightcraft_raster::Rgba8 {
                        width: pixels.size[0],
                        height: pixels.size[1],
                        data: pixels.pixels.iter().map(|p| p.to_array()).collect(),
                    };
                    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
                    std::fs::write(std::path::PathBuf::from(output).join(format!("storage-{name}-{w}.png")), bytes).unwrap();
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
