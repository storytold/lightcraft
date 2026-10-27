//! File ▸ Import Keywords… / Export Keywords…: keyword list files (Lightroom Classic's format,
//! which Capture One and Photo Supreme read and write too).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-keyword-files-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    h.settle(SETTLE);
    r
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default()
}

/// The File menu offers Import Keywords… and Export Keywords….
#[test]
fn the_file_menu_offers_keyword_files() {
    let mut h = app();
    let r = h.request("ui.menu.list", json!({}), T);
    let items = r["result"].to_string();
    for id in ["file.importKeywords", "file.exportKeywords"] {
        assert!(items.contains(id), "{id}");
    }
}

/// Import Keywords… reads a list and says how many keywords it added.
#[test]
fn importing_keywords_says_what_it_added() {
    let dir = temp_dir("import");
    let mut h = app();
    let path = dir.join("vocabulary.utf8");
    std::fs::write(&path, "[Places]\r\n\tPortugal\r\n\t\tLisbon\r\n").unwrap();
    let r = run(&mut h, "file.importKeywords", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.session.catalog.has_keyword("Places|Portugal|Lisbon"));
    assert!(toast(&h).contains("3"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Export Keywords… writes the list, and warns about the keywords Capture One won't import.
#[test]
fn exporting_keywords_warns_about_capture_one() {
    let dir = temp_dir("export");
    let mut h = app();
    let id = h.app.session.visible_cloned()[0].0;
    run(&mut h, "photo.setMeta", json!({"ids": [id], "addKeywords": ["fish, chips"]}));
    let path = dir.join("keywords.txt");
    let r = run(&mut h, "file.exportKeywords", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(std::fs::read_to_string(&path).unwrap().contains("fish, chips"));
    assert!(toast(&h).contains("Capture One"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A keyword the list format can't hold is left out of the file, and the toast names it.
#[test]
fn exporting_keywords_names_what_it_left_out() {
    let dir = temp_dir("export-left-out");
    let mut h = app();
    let id = h.app.session.visible_cloned()[0].0;
    run(&mut h, "photo.setMeta", json!({"ids": [id], "addKeywords": ["Travel|[draft]"]}));
    let path = dir.join("keywords.txt");
    let r = run(&mut h, "file.exportKeywords", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(toast(&h).contains("Travel|[draft]"), "{}", toast(&h));
    // a name with a line break in it shows on one line
    run(&mut h, "photo.setMeta", json!({"ids": [id], "addKeywords": ["Line\nbreak"]}));
    run(&mut h, "file.exportKeywords", json!({"path": path.to_string_lossy()}));
    assert!(toast(&h).contains("Line\\nbreak") && !toast(&h).contains('\n'), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A list that can't be written (a folder that isn't there) says why, as importing one does.
#[test]
fn exporting_keywords_says_why_it_failed() {
    let mut h = app();
    let path = std::env::temp_dir().join("lc-no-such-folder-for-keywords").join("keywords.txt");
    let r = run(&mut h, "file.exportKeywords", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(toast(&h).contains("keywords.txt"), "{}", toast(&h));
}
