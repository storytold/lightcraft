//! #782: dialogs that only show something (System Info, All Metadata, What's New) end with one
//! Close, like About, instead of a Cancel / OK pair that both just close them.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Open the dialog through its menu item; let the new window size itself before reading it.
fn open(h: &mut Headless, id: &str) {
    let r = h.request("ui.menu.invoke", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    assert!(h.app.ui.dialog.is_some(), "{id}: no dialog");
    h.step();
    h.step();
}

#[test]
fn information_dialogs_end_with_one_close() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    for id in ["app.systemInfo", "dialog.allMetadata", "app.whatsNew"] {
        open(&mut h, id);
        assert!(!has(&h, "button:dialogCancel"), "{id}: a Cancel button, though there is nothing to confirm");
        assert!(has(&h, "button:dialogOk"), "{id}: no Close button");
        let close = h.app.widgets.iter().filter(|(w, _)| w == "button:dialogOk").count();
        assert_eq!(close, 1, "{id}: one footer button");
        let text = h.painted_text();
        assert!(text.iter().any(|t| t == "Close"), "{id}: the button reads Close: {text:?}");
        assert!(!text.iter().any(|t| t == "Cancel" || t == "OK"), "{id}: no Cancel / OK: {text:?}");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{id}: Close closes the dialog");
        // Esc still closes it too
        open(&mut h, id);
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{id}: Esc closes the dialog");
    }
}

/// A JPEG on disk with the camera make and ISO in its EXIF; `shade` tells the pixels of files
/// that are otherwise the same apart, so an import doesn't take one for a duplicate of another.
fn tagged_jpeg(path: &std::path::Path, iso: u32, shade: u8) {
    let img = lightcraft_raster::Rgba8 { width: 16, height: 8, data: vec![[90, 120, shade, 255]; 128] };
    let exif = lightcraft_meta::write_exif(&lightcraft_meta::Metadata { make: Some("Maker".into()), iso: Some(iso), ..Default::default() });
    let meta = lightcraft_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    let jpg = lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap();
    std::fs::write(path, jpg).unwrap();
}

/// Open All Metadata… for several photos: their files are read on a worker thread with a row
/// in the activity stack, and the dialog opens when they are read.
fn open_several(h: &mut Headless) {
    let r = h.request("ui.menu.invoke", json!({"id": "dialog.allMetadata"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.ui.dialog.is_some()), "the dialog opens once the files are read");
    assert!(!h.app.tasks.is_running("Reading metadata"), "the read is over");
    h.step();
    h.step();
}

/// Gap 30: with several photos selected and Show Metadata For on Selected Photos, All Metadata
/// shows what the selected photos share and says Mixed where they differ (their files read in
/// the background); on Target Photo it shows the target photo alone, as the Info panel does.
#[test]
fn all_metadata_of_selected_photos_says_mixed() {
    let dir = std::env::temp_dir().join(format!("lc-ui-allmeta-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b, c) = (dir.join("a.jpg"), dir.join("b.jpg"), dir.join("c.jpg"));
    tagged_jpeg(&a, 800, 200);
    tagged_jpeg(&b, 1600, 150);
    tagged_jpeg(&c, 1600, 100);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [a.to_string_lossy(), b.to_string_lossy(), c.to_string_lossy()]})).unwrap();
    session.execute("library.selectAll", &json!({})).unwrap();
    assert_eq!(session.selected().len(), 3);
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(SETTLE);

    h.app.ui.settings.info_selected_photos = true;
    open_several(&mut h);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t == "3 selected photos"), "the title counts the photos: {text:?}");
    assert!(text.iter().any(|t| t == "Maker"), "a shared value is shown: {text:?}");
    assert!(text.iter().any(|t| t == "Mixed"), "the ISO they differ in says Mixed: {text:?}");
    assert!(!text.iter().any(|t| t == "800" || t == "1600"), "no one photo's ISO stands for all: {text:?}");
    h.app.ui.dialog = None;
    h.step();

    // a file that went away is left out of the comparison and named
    std::fs::remove_file(&a).unwrap();
    open_several(&mut h);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t == "2 selected photos"), "{text:?}");
    assert!(text.iter().any(|t| t == "1600"), "the two left share their ISO: {text:?}");
    assert!(text.iter().any(|t| t.contains("could not be read") && t.contains("a.jpg")), "the unreadable file is named: {text:?}");
    h.app.ui.dialog = None;
    h.step();

    // the active photo's file is back (its ISO now like the others')
    tagged_jpeg(&a, 1600, 200);
    h.app.ui.settings.info_selected_photos = false;
    open(&mut h, "dialog.allMetadata");
    let text = h.painted_text();
    assert!(!text.iter().any(|t| t == "Mixed" || t.ends_with("selected photos")), "Target Photo shows one photo: {text:?}");
    assert!(text.iter().any(|t| t == "1600"), "its own ISO: {text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// All Metadata reading several photos holds a row in the activity stack for as long as it
/// reads (kind `metadata`, a count, ✕), and the row's cancel stops it: no dialog opens.
#[test]
fn all_metadata_reading_several_photos_shows_in_the_activity_stack() {
    use std::sync::{Arc, Condvar, Mutex};
    let dir = std::env::temp_dir().join(format!("lc-ui-allmeta-row-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
    tagged_jpeg(&a, 800, 200);
    tagged_jpeg(&b, 1600, 150);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [a.to_string_lossy(), b.to_string_lossy()]})).unwrap();
    session.execute("library.selectAll", &json!({})).unwrap();
    // the files are read only once the test opens the gate
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    session.media.file_bytes = Some(Arc::new(move |path: &str| {
        let (open, cv) = &*g;
        let mut open = open.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        std::fs::read(path).map_err(|e| e.to_string())
    }));
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    h.app.ui.settings.info_selected_photos = true;
    let release = || {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    };

    let r = h.request("ui.menu.invoke", json!({"id": "dialog.allMetadata"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let rows = h.app.session.activity.list();
    let row = rows
        .iter()
        .find(|r| r.kind == "metadata")
        .unwrap_or_else(|| panic!("no metadata row: {:?}", rows.iter().map(|r| r.kind).collect::<Vec<_>>()));
    assert_eq!((row.label.as_str(), row.total, row.cancellable), ("Reading metadata", 2, true));
    assert!(h.app.ui.dialog.is_none(), "the dialog waits for the files");
    // a second request while it reads is refused, not a second read
    let again = h.request("ui.menu.invoke", json!({"id": "dialog.allMetadata"}), T);
    assert_eq!(again["ok"], false, "{again}");

    h.app.session.activity.cancel(row.id).unwrap();
    release();
    assert!(h.step_until(T, |h| !h.app.tasks.is_running("Reading metadata")), "the read stops");
    h.step();
    assert!(h.app.ui.dialog.is_none(), "a cancelled read opens no dialog");
    assert!(!h.app.session.activity.list().iter().any(|r| r.kind == "metadata"), "the row is gone");

    // read to the end: the dialog opens and the row goes
    open_several(&mut h);
    assert!(h.painted_text().iter().any(|t| t == "2 selected photos"));
    assert!(!h.app.session.activity.list().iter().any(|r| r.kind == "metadata"));
    let _ = std::fs::remove_dir_all(&dir);
}
