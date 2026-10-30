//! The Choose… and Browse buttons that used to open a native file dialog inside the frame (#191,
//! #3140): the window stopped redrawing until the dialog closed. The host now shows the dialog
//! elsewhere; the button returns at once and the answer lands where it belongs when it closes.
//!
//! Each test gives the host a picker that answers later, and synchronous services that record
//! being called (a call is the old, blocking behavior).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::pick::{Field, PickKind, PickRequest};
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

type Asked = Arc<Mutex<Vec<(PickRequest, Sender<Vec<String>>)>>>;

/// The demo app, its picker (each request recorded with the sender that answers it) and a flag
/// that says whether a synchronous dialog service was called.
fn app() -> (Headless, Asked, Arc<AtomicBool>) {
    let blocked = Arc::new(AtomicBool::new(false));
    let hit = |b: &Arc<AtomicBool>| {
        let b = b.clone();
        move || b.store(true, Ordering::SeqCst)
    };
    let (f, g, d) = (hit(&blocked), hit(&blocked), hit(&blocked));
    let services = Services {
        pick_folder: Some(Box::new(move || {
            f();
            None
        })),
        pick_files: Some(Box::new(move || {
            g();
            Vec::new()
        })),
        pick_denoise_model: Some(Box::new(move || {
            d();
            Vec::new()
        })),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let asked: Asked = Default::default();
    let a = asked.clone();
    app.services.picker = Some(Box::new(move |req| {
        let (tx, rx) = channel();
        a.lock().unwrap().push((req, tx));
        Ok(rx)
    }));
    let mut h = Headless::new(app, [1400.0, 3000.0], 1.0);
    h.settle(T);
    (h, asked, blocked)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}

fn answer(h: &mut Headless, asked: &Asked, i: usize, path: &str) {
    asked.lock().unwrap()[i].1.send(vec![path.to_string()]).unwrap();
    h.step();
    h.step();
}

/// A button was clicked: exactly one dialog is up, of `kind`; the frame came back without the
/// synchronous service; a second click opens no second dialog.
fn assert_up(h: &mut Headless, asked: &Asked, blocked: &AtomicBool, id: &str, kind: PickKind) {
    click(h, id);
    assert!(!blocked.load(Ordering::SeqCst), "{id} called the synchronous dialog on the UI thread");
    assert_eq!(asked.lock().unwrap().len(), 1, "{id} shows one dialog");
    assert_eq!(asked.lock().unwrap()[0].0.kind, kind);
    click(h, id);
    assert_eq!(asked.lock().unwrap().len(), 1, "{id}: a second click opens no second dialog");
}

fn export_dir(h: &Headless) -> Option<String> {
    match &h.app.ui.dialog {
        Some(Dialog::Export { dir, .. }) => Some(dir.clone()),
        _ => None,
    }
}

fn open_export(h: &mut Headless) {
    h.app.run("dialog.export", json!({})).unwrap();
    h.settle(T);
    assert!(export_dir(h).is_some(), "the export dialog is open");
}

#[test]
fn the_export_folder_dialog_does_not_block_and_its_answer_fills_the_folder() {
    let (mut h, asked, blocked) = app();
    open_export(&mut h);
    assert_up(&mut h, &asked, &blocked, "button:exportChooseFolder", PickKind::Folder);
    assert_ne!(export_dir(&h), Some("/chosen/export".into()), "nothing is filled in before the dialog closes");
    answer(&mut h, &asked, 0, "/chosen/export");
    assert_eq!(export_dir(&h), Some("/chosen/export".into()));
    assert!(h.app.pending_picks.is_empty());
}

#[test]
fn the_watermark_image_dialog_does_not_block_and_its_answer_sets_the_image() {
    let (mut h, asked, blocked) = app();
    open_export(&mut h);
    if let Some(Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
        opts.watermark = Some(lightcraft_engine::export::Watermark { image: "old.png".into(), ..Default::default() });
    }
    h.settle(T);
    assert_up(&mut h, &asked, &blocked, "button:exportWmChoose", PickKind::File);
    answer(&mut h, &asked, 0, "/logos/new.png");
    match &h.app.ui.dialog {
        Some(Dialog::Export { opts, .. }) => assert_eq!(opts.watermark.as_ref().map(|w| w.image.as_str()), Some("/logos/new.png")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_answer_for_a_dialog_closed_meanwhile_is_dropped() {
    let (mut h, asked, _) = app();
    open_export(&mut h);
    click(&mut h, "button:exportChooseFolder");
    assert_eq!(asked.lock().unwrap().len(), 1);
    // the export dialog is closed, then another one opened, before the file dialog answers
    h.app.ui.dialog = None;
    h.step();
    assert!(h.app.pending_picks.is_empty(), "the entry of the closed dialog is forgotten");
    open_export(&mut h);
    let before = export_dir(&h);
    // (the entry is gone, so the dialog's channel is closed: the late answer reaches nobody)
    assert!(asked.lock().unwrap()[0].1.send(vec!["/stale/answer".to_string()]).is_err());
    h.step();
    h.step();
    assert_eq!(export_dir(&h), before, "the new dialog got no answer meant for the old one");
}

#[test]
fn a_closed_dialogs_pick_is_forgotten_in_the_full_screen_preview_too() {
    let (mut h, asked, _) = app();
    open_export(&mut h);
    click(&mut h, "button:exportChooseFolder");
    assert_eq!(asked.lock().unwrap().len(), 1);
    // the full-screen frame returns early: it must still poll, or a reopened export dialog would
    // take the old dialog's answer
    h.app.ui.fullscreen = true;
    h.app.ui.dialog = None;
    h.step();
    assert!(h.app.pending_picks.is_empty(), "the entry of the closed dialog is forgotten in the full-screen frame");
}

#[test]
fn the_import_destination_dialog_does_not_block_and_its_answer_sets_the_destination() {
    let dir = std::env::temp_dir().join(format!("lc-bgdlg-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    let photo = dir.join("one.png");
    std::fs::write(&photo, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let (mut h, asked, blocked) = app();
    h.app.run("file.addPhotos", json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the import review opens");
    if let Some(Dialog::Import { opts }) = &mut h.app.ui.dialog {
        opts.copy = true;
    }
    h.settle(T);
    assert_up(&mut h, &asked, &blocked, "button:importDest", PickKind::Folder);
    answer(&mut h, &asked, 0, "/chosen/originals");
    match &h.app.ui.dialog {
        Some(Dialog::Import { opts }) => assert_eq!(opts.destination, "/chosen/originals"),
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browse_folder_does_not_block_and_the_answer_browses_it() {
    let dir = std::env::temp_dir().join(format!("lc-bgdlg-browse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut h, asked, blocked) = app();
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.left_panel = true;
    h.settle(T);
    assert_up(&mut h, &asked, &blocked, "source:local:browse", PickKind::Folder);
    answer(&mut h, &asked, 0, &dir.to_string_lossy());
    assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::Folder, "the chosen folder is browsed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_watched_folder_dialog_does_not_block_and_its_answer_is_watched() {
    let dir = std::env::temp_dir().join(format!("lc-bgdlg-auto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut h, asked, blocked) = app();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "import".into() });
    h.settle(T);
    assert_up(&mut h, &asked, &blocked, "button:settingsAutoFolder", PickKind::Folder);
    answer(&mut h, &asked, 0, &dir.to_string_lossy());
    // (JSON escapes a Windows path's backslashes: compare with the folder as JSON text)
    let prefs = h.app.session.execute("library.preferences", &json!({})).unwrap().to_string();
    let folder = json!(dir.to_string_lossy()).to_string();
    assert!(prefs.contains(folder.trim_matches('"')), "the folder is watched: {prefs}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Press the button of `field` as its panel does, without the panel's scrolling in the way of a
/// click: one dialog of `kind` is up, nothing blocked, and a second press shows no second dialog.
fn press(h: &mut Headless, asked: &Asked, blocked: &AtomicBool, field: Field, kind: PickKind) {
    let ctx = egui::Context::default();
    crate::pick::choose_and_apply(&mut h.app, &ctx, field);
    crate::pick::choose_and_apply(&mut h.app, &ctx, field);
    assert!(!blocked.load(Ordering::SeqCst), "{field:?} called the synchronous dialog");
    assert_eq!(asked.lock().unwrap().len(), 1, "{field:?} shows one dialog");
    assert_eq!(asked.lock().unwrap()[0].0.kind, kind);
}

#[test]
fn the_slideshow_plate_image_dialog_does_not_block_and_its_answer_sets_the_image() {
    let (mut h, asked, blocked) = app();
    h.app.run("slideshow.overlays", json!({"identityPlate": {"enabled": true, "image": "old.png"}})).unwrap();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "slideshow".into() });
    h.settle(T);
    press(&mut h, &asked, &blocked, Field::SlidePlateImage, PickKind::File);
    answer(&mut h, &asked, 0, "/logos/plate.png");
    assert_eq!(h.app.session.slide_overlays.identity_plate.image, "/logos/plate.png");
}

#[test]
fn the_model_file_dialog_does_not_block_and_its_answer_is_checked() {
    let (mut h, asked, blocked) = app();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "denoise".into() });
    h.settle(T);
    press(&mut h, &asked, &blocked, Field::DenoiseModel, PickKind::File);
    assert!(h.app.caches.denoise.rejected.is_none());
    answer(&mut h, &asked, 0, "/no/such/model.onnx");
    assert!(h.app.caches.denoise.rejected.is_some(), "the chosen file was inspected (and refused: it does not exist)");
}

#[test]
fn the_smart_previews_folder_dialog_does_not_block_and_its_answer_is_applied() {
    let (mut h, asked, blocked) = app();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "performance".into() });
    h.settle(T);
    press(&mut h, &asked, &blocked, Field::SmartPreviewsFolder, PickKind::Folder);
    h.app.ui.toast = None;
    answer(&mut h, &asked, 0, "/no/such/folder");
    assert!(h.app.pending_picks.is_empty());
    assert!(h.app.ui.toast.is_some(), "the answer was applied: the engine's refusal is shown");
}

#[test]
fn the_face_model_command_waits_for_its_dialog() {
    let (mut h, asked, blocked) = app();
    assert_eq!(h.app.run("dialog.faceModel", json!({})).unwrap(), serde_json::Value::Null);
    assert!(!blocked.load(Ordering::SeqCst));
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert_eq!(asked.lock().unwrap()[0].0.kind, PickKind::File);
    assert_eq!(h.app.pending_picks.len(), 1);
}

#[test]
fn a_field_without_a_background_dialog_still_answers_on_the_synchronous_service() {
    // hosts with no picker (the web, tests) keep the synchronous dialogs
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.services.pick_folder = Some(Box::new(|| Some("/sync/folder".into())));
    assert_eq!(crate::pick::choose(&mut app, Field::ExportFolder), Some("/sync/folder".into()));
    assert!(app.pending_picks.is_empty());
}
