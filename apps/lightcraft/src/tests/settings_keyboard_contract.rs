//! Test-only disk qualification using the real Settings UI and desktop PrefsWriter.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lightcraft_ui_egui::headless::Headless;
use serde_json::{Value, json};

use super::super::{LightcraftApp, PrefsWriter, Services, Session, load_prefs_at};

const T: Duration = Duration::from_secs(20);

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("lc-settings-keyboard-contract-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).unwrap(); // Exclusive: never adopt/remove a pre-existing folder.
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    steps(h);
}

fn open(library: &Path) -> Headless {
    let mut session = Session::new().with_fs();
    session.open_library(library, false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 1100.0], 1.0);
    assert!(h.settle(T));
    steps(&mut h);
    h
}

fn show(h: &mut Headless) {
    ask(h, "engine.execute", json!({"command": "app.settings", "params": {"tab": "import"}}));
    assert!(h.settle(T));
    steps(h);
}

fn replace(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:settings.copyright"}));
    ask(h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(h, "ui.text", json!({"text": text}));
    assert!(h.painted_text().iter().any(|s| s.contains(text)), "the draft really reached the field");
}

fn reload(mut h: Headless, library: &Path, ui_file: &Path) -> Headless {
    assert!(h.app.ui.dialog.is_none(), "Close must run before the save/reload boundary");
    let mut writer = PrefsWriter { path: Some(ui_file.to_path_buf()), ..Default::default() };
    writer.save_ui(&h.app.ui, false).unwrap();
    assert!(ui_file.is_file(), "actual desktop UI state reached disk");
    h.app.session.close_library().unwrap();
    drop(h);
    let (saved, warning, keep_file) = load_prefs_at(ui_file, false);
    assert_eq!(warning, None);
    assert!(!keep_file);
    let mut h = open(library);
    h.app.ui = saved.expect("desktop loader reads the file written by PrefsWriter");
    steps(&mut h);
    h
}

#[test]
fn real_settings_cancelled_draft_and_applied_changes_round_trip_both_disk_stores() {
    let folder = Folder::new();
    let library = folder.0.join("library");
    let ui_file = folder.0.join("config/ui.json");
    let mut h = open(&library);
    let original_text = h.app.session.import_defaults.copyright.clone();
    let original_library = h.app.session.import_defaults.per_camera;
    let original_app = h.app.ui.settings.import_dropped_directly;
    show(&mut h);
    replace(&mut h, "Cancelled Disk Contract");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_some(), "Escape cancelled the field, not Settings");
    assert_eq!(h.app.session.import_defaults.copyright, original_text);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    h = reload(h, &library, &ui_file);
    assert_eq!(h.app.session.import_defaults.copyright, original_text, "cancelled draft was not persisted");
    assert_eq!(h.app.session.import_defaults.per_camera, original_library);
    assert_eq!(h.app.ui.settings.import_dropped_directly, original_app);

    show(&mut h);
    replace(&mut h, "Committed Disk Contract");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(h.app.ui.dialog.is_some(), "Enter applies a text field, not the whole dialog");
    assert_eq!(h.app.session.import_defaults.copyright, "Committed Disk Contract");
    ask(&mut h, "ui.clickWidget", json!({"id": "check:settings.perCamera"}));
    assert_eq!(h.app.ui.settings.import_dropped_directly, original_app, "library checkbox does not change app settings");
    ask(&mut h, "ui.clickWidget", json!({"id": "check:settings.importDropped"}));
    assert_eq!(h.app.session.import_defaults.per_camera, !original_library, "app checkbox does not change the library value");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    h = reload(h, &library, &ui_file);
    assert_eq!(h.app.session.import_defaults.copyright, "Committed Disk Contract");
    assert_eq!(h.app.session.import_defaults.per_camera, !original_library);
    assert_eq!(h.app.ui.settings.import_dropped_directly, !original_app);
    show(&mut h);
    assert!(h.painted_text().iter().any(|s| s.contains("Committed Disk Contract")), "reopening the real dialog shows the disk value");
    h.app.session.close_library().unwrap();
    drop(h); // Release all handles before the private fixture folder is removed.
}
