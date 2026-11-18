//! The Import screen (fork-owned, `crate::import_screen`): its filters show what they say, Add in
//! place hides the copy-only rows, its button imports the checked photos, and Back cancels.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::import::ImportDialog;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn review(dir: &std::path::Path, n: usize) -> Headless {
    std::fs::create_dir_all(dir).unwrap();
    // (a folder picker: Copy and Move are offered)
    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let mut paths = Vec::new();
    for i in 0..n {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[(i * 30) as u8, 90, 9, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("new{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": paths}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "the review is open");
    h
}

fn opts(h: &Headless) -> &ImportDialog {
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review is open") };
    opts
}

fn cells(h: &Headless) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.strip_prefix("import:").is_some_and(|n| n.parse::<usize>().is_ok())).count()
}

#[test]
fn the_filters_show_new_photos_and_duplicates() {
    let dir = lcc_testkit::TempDir::new("lcc-import-screen-filter");
    let mut h = review(&dir, 3);
    // one of them already in the library
    if let Some(Dialog::Import { opts }) = &mut h.app.ui.dialog {
        opts.candidates[1].duplicate = Some("content".into());
        opts.candidates[1].existing = Some(1);
    }
    h.settle(SETTLE);
    assert_eq!(cells(&h), 3, "All");
    click(&mut h, "import:filter:new");
    assert_eq!(cells(&h), 2, "New");
    click(&mut h, "import:filter:dups");
    assert_eq!(cells(&h), 1, "Duplicates");
    click(&mut h, "import:filter:all");
    assert_eq!(cells(&h), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn add_in_place_hides_the_copy_rows_and_copy_shows_them() {
    let dir = lcc_testkit::TempDir::new("lcc-import-screen-how");
    let mut h = review(&dir, 2);
    click(&mut h, "button:importCopy");
    assert!(opts(&h).copy && !opts(&h).move_files);
    assert!(has(&h, "combo:importOrganize") && has(&h, "field:importRename") && has(&h, "check:importDng"), "the copy rows");
    click(&mut h, "button:importMove");
    assert!(opts(&h).move_files);
    assert!(!has(&h, "check:importDng"), "no DNG when moving");
    click(&mut h, "button:importAdd");
    assert!(!opts(&h).copy);
    assert!(!has(&h, "combo:importOrganize") && !has(&h, "field:importRename"), "hidden, not greyed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_button_imports_the_checked_photos_and_back_cancels() {
    let dir = lcc_testkit::TempDir::new("lcc-import-screen-go");
    let mut h = review(&dir, 2);
    click(&mut h, "button:importAdd");
    let before = h.app.session.catalog.photos().count();
    click(&mut h, "import:go");
    assert!(h.app.ui.dialog.is_none(), "the review closes");
    let start = std::time::Instant::now();
    while h.app.session.catalog.photos().count() < before + 2 {
        assert!(start.elapsed() < T, "imported");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    // a second review: Back closes it without importing
    let mut h = review(&dir.join("again"), 1);
    let before = h.app.session.catalog.photos().count();
    click(&mut h, "import:back");
    assert!(h.app.ui.dialog.is_none());
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.session.catalog.photos().count(), before, "nothing imported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_checked_leaves_the_button_doing_nothing() {
    let dir = lcc_testkit::TempDir::new("lcc-import-screen-none");
    let mut h = review(&dir, 2);
    click(&mut h, "button:importNone");
    assert!(opts(&h).selected_paths().is_empty());
    let r = h.request("ui.clickWidget", json!({"id": "import:go"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "still open: nothing to import");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A card the system lists shows in the rail, and picking it scans it into the review; the watched
/// folder shows only once one is set. A card with an empty or odd name still gets a row.
#[test]
fn the_rail_lists_cards_and_the_watched_folder_and_picking_one_scans_it() {
    let tmp = lcc_testkit::TempDir::new("lcc-import-rail");
    let mut h = review(&tmp.join("first"), 1);
    let card = tmp.join("card");
    std::fs::create_dir_all(&card).unwrap();
    for i in 0..2 {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[(i * 90) as u8, 40, 200, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(card.join(format!("card{i}.png")), png).unwrap();
    }
    assert!(!has(&h, "import:source:watched"), "no watched folder set: no row");
    let devices = vec![("EOS_DIGITAL".to_string(), card.to_string_lossy().to_string()), ("".to_string(), "/nowhere/\u{0}bad".to_string())];
    h.view.ctx.data_mut(|d| d.insert_temp(egui::Id::new(crate::import_screen::TEST_DEVICES), devices));
    h.app.session.import_defaults.auto_folder = Some(card.to_string_lossy().to_string());
    h.settle(SETTLE);
    assert!(has(&h, "import:source:card:0") && has(&h, "import:source:card:1"), "a row per card");
    assert!(has(&h, "import:source:watched"), "the watched folder");
    click(&mut h, "import:source:card:0");
    assert_eq!(opts(&h).sources, vec![card.to_string_lossy().to_string()], "the card was scanned into the review");
    assert_eq!(opts(&h).candidates.len(), 2);
    // a card that can't be read is an error, not a crash
    let r = h.request("ui.clickWidget", json!({"id": "import:source:card:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dialog_import_with_path_and_paths_populates_candidates() {
    let tmp = lcc_testkit::TempDir::new("lcc-import-dialog-cmd");
    std::fs::create_dir_all(&tmp).unwrap();

    let write_small_png = |path: &std::path::Path, seed: u8| {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[seed, 50, 100, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(path, png).unwrap();
    };

    let p1 = tmp.join("one.png");
    let p2 = tmp.join("two.png");
    write_small_png(&p1, 11);
    write_small_png(&p2, 22);

    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);

    // 1. Invoke dialog.import with a folder "path"
    let r = h.request("engine.execute", json!({"command": "dialog.import", "params": {"path": tmp.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);

    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "dialog is open after path param");
    assert_eq!(opts(&h).candidates.len(), 2, "both files found via path");
    assert_eq!(cells(&h), 2);

    // Close the dialog
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none());

    // 2. Invoke dialog.import with a list of "paths"
    let paths_val = vec![p1.to_string_lossy().to_string(), p2.to_string_lossy().to_string()];
    let r = h.request("engine.execute", json!({"command": "dialog.import", "params": {"paths": paths_val}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);

    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "dialog is open after paths param");
    assert_eq!(opts(&h).candidates.len(), 2, "both files found via paths");
    assert_eq!(cells(&h), 2);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn import_settings_survive_subfolder_rescan() {
    let tmp = lcc_testkit::TempDir::new("lcc-import-settings-survive");
    let sub = tmp.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let write_small_png = |path: &std::path::Path, seed: u8| {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[seed, 50, 100, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(path, png).unwrap();
    };

    write_small_png(&tmp.join("root.png"), 10);
    write_small_png(&sub.join("nested.png"), 20);

    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);

    // Open import from folder
    let r = h.request("engine.execute", json!({"command": "dialog.import", "params": {"path": tmp.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);

    assert_eq!(opts(&h).candidates.len(), 2);

    // Switch to Copy mode
    click(&mut h, "button:importCopy");
    assert!(opts(&h).copy && !opts(&h).move_files);

    // Set keywords in the open dialog
    if let Some(Dialog::Import { opts }) = &mut h.app.ui.dialog {
        opts.keywords = "vacation, sunny".to_string();
    }

    // Toggle "Include subfolders" to trigger a re-scan
    click(&mut h, "check:includeSubfolders");
    assert!(!opts(&h).subfolders, "subfolders unchecked");

    // After re-scan finishes, candidates update, but copy choice and keywords survive
    assert_eq!(opts(&h).candidates.len(), 1, "only root file after excluding subfolders");
    assert!(opts(&h).copy && !opts(&h).move_files, "copy mode survived re-scan");
    assert_eq!(opts(&h).keywords, "vacation, sunny", "keywords survived re-scan");

    // Switch to Move mode and re-enable subfolders to re-scan again
    click(&mut h, "button:importMove");
    assert!(opts(&h).move_files, "move mode selected");

    click(&mut h, "check:includeSubfolders");
    assert!(opts(&h).subfolders, "subfolders re-enabled");

    // After re-scan finishes, move mode and keywords still survive
    assert_eq!(opts(&h).candidates.len(), 2, "both root and nested restored");
    assert!(opts(&h).copy && opts(&h).move_files, "move mode survived re-scan");
    assert_eq!(opts(&h).keywords, "vacation, sunny", "keywords still preserved");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn import_from_folder_with_subfolders_toggle_updates_candidates_and_imports() {
    let tmp = lcc_testkit::TempDir::new("lcc-import-subfolders");
    let sub = tmp.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let write_small_png = |path: &std::path::Path, seed: u8| {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[seed, 50, 100, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(path, png).unwrap();
    };

    write_small_png(&tmp.join("root1.png"), 10);
    write_small_png(&tmp.join("root2.png"), 20);
    write_small_png(&sub.join("nested.png"), 30);

    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);

    let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": tmp.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);

    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "the import dialog is open");
    assert!(opts(&h).subfolders, "subfolders is true by default");
    assert_eq!(opts(&h).candidates.len(), 3, "initially discovers root1, root2, and nested");
    assert_eq!(cells(&h), 3);

    // Uncheck "Include subfolders"
    click(&mut h, "check:includeSubfolders");
    assert!(!opts(&h).subfolders, "subfolders is now false");
    // After re-scan completes, only root photos are candidates
    assert_eq!(opts(&h).candidates.len(), 2, "nested photo excluded when subfolders unchecked");
    assert_eq!(cells(&h), 2);

    // Click Import (Add in place)
    click(&mut h, "button:importAdd");
    let before = h.app.session.catalog.photos().count();
    click(&mut h, "import:go");
    assert!(h.app.ui.dialog.is_none(), "import dialog closes after starting import");

    let start = std::time::Instant::now();
    while h.app.session.catalog.photos().count() < before + 2 {
        assert!(start.elapsed() < T, "imported 2 root photos");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(h.app.session.catalog.photos().count(), before + 2, "only 2 root photos imported");
    let _ = std::fs::remove_dir_all(&tmp);
}

// Scenario (issue #1083): with a macOS integrated title bar the ‹ Back button (and the title after
// it) starts right of the traffic lights, as Settings' does; in full screen nothing moves
#[test]
fn back_clears_the_traffic_lights() {
    let dir = std::env::temp_dir().join(format!("lcc-import-screen-traffic-{}", std::process::id()));
    let mut h = review(&dir, 1);
    let left = |h: &Headless| h.app.widgets.iter().find(|(w, _)| w == "import:back").map(|(_, r)| r.left()).unwrap();
    assert!((left(&h) - 10.0).abs() < 0.5);
    h.app.integrated_titlebar = true;
    h.step();
    h.step();
    assert!((left(&h) - 78.0).abs() < 0.5);
    h.app.window_is_fullscreen = true;
    h.step();
    h.step();
    assert!((left(&h) - 10.0).abs() < 0.5);
    let _ = std::fs::remove_dir_all(&dir);
}
