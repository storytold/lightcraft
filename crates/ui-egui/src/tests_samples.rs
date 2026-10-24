//! Help ▸ Add Sample Photos in the app: the albums come in one folder named in the interface's
//! language, and a toast says how many photos came and where to look.

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

/// A new library on disk (an in-memory session isn't a library).
fn library(tag: &str) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-samples-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

fn headless(session: lightcraft_engine::Session) -> Headless {
    let mut a = LightcraftApp::new(session, Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h
}

#[test]
fn adding_the_samples_says_what_came_and_where() {
    let mut h = headless(library("said"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    let n = h.app.session.catalog.sample_photos();
    let toast = h.app.ui.toast.as_ref().map(|(text, _, _)| text.clone()).unwrap_or_default();
    assert!(toast.contains(&format!("Added {n} sample photos")) && toast.contains("“Sample Photos”"), "{toast}");
    // long enough to read, as a toast saying where to look is
    let (_, until, _) = h.app.ui.toast.clone().unwrap();
    assert!(until - h.app.last_time >= 3.0, "{until} at {}", h.app.last_time);
    // their albums are together, in the folder the toast names
    let top: Vec<String> = h.app.session.catalog.albums().filter(|a| a.parent.is_none()).map(|a| a.name.clone()).collect();
    assert_eq!(top, ["Sample Photos"]);
}

#[test]
fn a_caller_can_name_the_folder_and_the_toast_follows() {
    let mut h = headless(library("named"));
    h.app.run("library.addSamples", json!({"folder": "Try these"})).unwrap();
    h.step();
    let toast = h.app.ui.toast.as_ref().map(|(text, _, _)| text.clone()).unwrap_or_default();
    assert!(toast.contains("“Try these”"), "{toast}");
}
