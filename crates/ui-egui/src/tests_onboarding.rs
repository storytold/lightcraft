//! Getting Started (fork-owned, `onboarding`): it follows a first welcome once the library is open,
//! steps forward and back, lands its choices in the existing settings, downloads only what was ticked,
//! and fits a desktop window and a phone.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::onboarding::{AiRow, STEP_AI, STEP_FINISH, STEP_INTERFACE, STEP_SETTINGS, STEP_TOUR, preticked};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

fn temp(tag: &str) -> lcc_testkit::TempDir {
    let d = lcc_testkit::TempDir::new(&format!("lc-ui-onboarding-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn step(app: &LccApp) -> Option<usize> {
    app.onboarding.wizard.as_ref().map(|w| w.step)
}

/// A feature row whose "download" is a harmless command, so a test sees which ones Finish sends.
fn fake_row(key: &'static str, on: bool, installed: bool) -> AiRow {
    AiRow {
        key,
        size: Some(1_000_000),
        licence: "MIT".into(),
        licence_url: "https://opensource.org/license/mit".into(),
        installed,
        on,
        commands: vec![("view.reduceMotionProbe".to_string(), json!({"key": key}))],
    }
}

fn ran(r: &Value) -> Vec<String> {
    r["ran"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

#[test]
fn defaults_follow_the_computer_and_never_tick_faces_or_search() {
    for key in ["denoise", "superres", "objects", "subject", "scene", "faces", "search", "lens"] {
        assert!(!preticked(key, None, true), "{key}: unknown memory ticks nothing");
        assert!(!preticked(key, Some(4.0), true), "{key}: 4 GB ticks nothing");
        assert!(!preticked(key, Some(f64::NAN), true), "{key}: a nonsense size ticks nothing");
        assert!(!preticked(key, Some(64.0), true) || !["faces", "search"].contains(&key), "{key}: never faces or search");
    }
    for key in ["denoise", "objects", "lens"] {
        assert!(preticked(key, Some(7.8), false), "{key}: an 8 GB computer");
    }
    for key in ["subject", "scene"] {
        assert!(!preticked(key, Some(8.0), true), "{key}: not at 8 GB");
        assert!(!preticked(key, Some(32.0), false), "{key}: not without a real graphics card");
        assert!(preticked(key, Some(15.9), true), "{key}: 16 GB with a graphics card");
    }
}

// Scenario: the first start's welcome creates a library; the guide then opens at its AI page, once
#[test]
fn the_guide_follows_the_first_welcome_once() {
    let root = temp("follow");
    let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    app.run("catalog.welcome", json!({})).unwrap();
    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    h.settle(SETTLE);
    assert_eq!(step(&h.app), None, "not over the welcome dialog");
    h.app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "First", "demo": false})).unwrap();
    h.settle(SETTLE);
    assert_eq!(step(&h.app), Some(STEP_AI), "the AI page follows the new library");
    // Esc skips: nothing downloads, the library stays open, and it counts as seen
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    assert_eq!(step(&h.app), None);
    assert!(h.app.ui.settings.onboarding_done);
    assert!(h.app.session.library.is_some(), "Esc never loses the library");
    // another welcome (File ▸ Welcome…) doesn't bring it back once seen
    h.app.run("catalog.welcome", json!({})).unwrap();
    h.app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "Second", "demo": false})).unwrap();
    h.settle(SETTLE);
    assert_eq!(step(&h.app), None, "first run only");
    // Help ▸ Getting Started… opens it again
    h.app.run("app.gettingStarted", json!({})).unwrap();
    assert_eq!(step(&h.app), Some(STEP_AI));
    h.app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn back_and_next_keep_choices_and_skip_downloads_nothing() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    app.run("app.gettingStarted", json!({})).unwrap();
    {
        let w = app.onboarding.wizard.as_mut().unwrap();
        w.rows = vec![fake_row("denoise", true, false), fake_row("faces", false, false)];
    }
    let r = app.run("app.gettingStarted", json!({"tick": {"faces": true, "denoise": false}})).unwrap();
    assert_eq!(r["rows"][1]["on"], true, "{r}");
    for _ in 0..3 {
        app.run("app.gettingStarted", json!({"action": "next"})).unwrap();
    }
    assert_eq!(step(&app), Some(STEP_TOUR));
    app.run("app.gettingStarted", json!({"action": "back"})).unwrap();
    assert_eq!(step(&app), Some(STEP_SETTINGS));
    for _ in 0..2 {
        app.run("app.gettingStarted", json!({"action": "back"})).unwrap();
    }
    assert_eq!(step(&app), Some(STEP_AI));
    app.run("app.gettingStarted", json!({"action": "back"})).unwrap();
    assert_eq!(step(&app), Some(STEP_AI), "Back stops at the first page");
    let w = app.onboarding.wizard.as_ref().unwrap();
    assert!(!w.rows[0].on && w.rows[1].on, "Back and Next keep the ticks");
    let r = app.run("app.gettingStarted", json!({"action": "skip"})).unwrap();
    assert!(ran(&r).is_empty(), "Skip downloads nothing: {r}");
    assert!(app.ui.settings.onboarding_done);
    assert_eq!(step(&app), None);
}

#[test]
fn finish_sends_only_the_ticked_downloads() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    app.run("app.gettingStarted", json!({"step": STEP_FINISH})).unwrap();
    app.onboarding.wizard.as_mut().unwrap().rows =
        vec![fake_row("denoise", true, false), fake_row("faces", false, false), fake_row("lens", true, true)];
    let r = app.run("app.gettingStarted", json!({"action": "finish"})).unwrap();
    // one command for the ticked row; the unticked one and the installed one send nothing
    assert_eq!(ran(&r).len(), 1, "{r}");
    assert!(app.ui.settings.onboarding_done);
}

// The real feature rows of this build: unticked, Finish sends none of their download commands
#[test]
fn unticked_real_features_never_download() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    app.run("app.gettingStarted", json!({})).unwrap();
    let keys: Vec<&str> = app.onboarding.wizard.as_ref().unwrap().rows.iter().map(|r| r.key).collect();
    let ticks: serde_json::Map<String, Value> = keys.iter().map(|k| (k.to_string(), json!(false))).collect();
    app.run("app.gettingStarted", json!({"tick": ticks})).unwrap();
    let r = app.run("app.gettingStarted", json!({"action": "finish"})).unwrap();
    assert!(ran(&r).is_empty(), "{r}");
    assert!(app.session.activity.list().iter().all(|t| t.kind != "download"), "no download row");
}

// The interface page writes the existing settings, at once
#[test]
fn interface_and_settings_choices_land_in_the_settings() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    app.run("app.gettingStarted", json!({"step": STEP_INTERFACE})).unwrap();
    let mut h = Headless::new(app, [1280.0, 900.0], 1.0);
    h.request("ui.widgets", json!({}), T);
    h.settle(SETTLE);
    for id in ["radio:wizardLayout.compact", "radio:wizardAppearance.light", "radio:wizardFont.large", "radio:wizardGrading.eboda"] {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.settle(SETTLE);
    }
    let s = &h.app.ui.settings;
    assert!(s.compact_layout);
    assert_eq!(s.appearance_mode, crate::state::AppearanceMode::Light);
    assert_eq!(s.ui_font_size, crate::theme::UiFontSize::Large);
    assert_eq!(s.grading_mode, crate::state::GradingMode::Eboda);
    // Undo these changes puts the look back
    let r = h.request("ui.clickWidget", json!({"id": "button:wizardUndoLook"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let d = crate::state::AppSettings::default();
    assert!(!h.app.ui.settings.compact_layout);
    assert_eq!(h.app.ui.settings.ui_font_size, d.ui_font_size);
    // the key settings page
    h.app.run("app.gettingStarted", json!({"action": "next"})).unwrap();
    h.settle(SETTLE);
    assert_eq!(step(&h.app), Some(STEP_SETTINGS));
    for id in ["check:wizardReduceMotion", "check:wizardAskAtLaunch"] {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.settle(SETTLE);
    }
    assert!(h.app.ui.settings.reduce_motion);
    assert_eq!(h.app.ui.settings.at_launch, crate::state::LaunchLibrary::Ask);
}

/// With `<PREFIX>_TEST_SHOTS=<dir>`, a picture of the screen as `<dir>/<name>.png` (to look at).
fn shot(h: &mut Headless, name: &str) {
    let Some(dir) = lcc_brand::env_os("TEST_SHOTS") else { return };
    h.app.services.png = Some(Box::new(|img: &lcc_raster::Rgba8| {
        lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
    }));
    h.app.services.write = Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string())));
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
}

// Every page, on a desktop window and on a phone: the buttons are on screen and inside the window
#[test]
fn every_page_fits_a_desktop_window_and_a_phone() {
    for (name, size) in [("desktop", [1440.0, 900.0]), ("small", [400.0, 700.0])] {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
        app.run("app.gettingStarted", json!({})).unwrap();
        let mut h = Headless::new(app, size, 1.0);
        h.request("ui.widgets", json!({}), T);
        h.settle(SETTLE);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        for page in 0..=STEP_FINISH {
            assert_eq!(step(&h.app), Some(page));
            h.request("ui.widgets", json!({}), T);
            h.settle(SETTLE);
            shot(&mut h, &format!("{name}-{page:02}"));
            let forward = if page == STEP_FINISH { "button:wizardFinish" } else { "button:wizardNext" };
            let r = h.app.widgets.iter().find(|(w, _)| w == forward).map(|(_, r)| *r);
            let r = r.unwrap_or_else(|| panic!("{name} page {page}: no {forward}"));
            assert!(screen.contains_rect(r), "{name} page {page}: {forward} at {r:?} is off screen");
            if page == STEP_FINISH {
                break;
            }
            let c = h.request("ui.clickWidget", json!({"id": forward}), T);
            assert_eq!(c["ok"], true, "{c}");
            h.settle(SETTLE);
        }
        let c = h.request("ui.clickWidget", json!({"id": "button:wizardFinish"}), T);
        assert_eq!(c["ok"], true, "{c}");
        h.settle(SETTLE);
        assert_eq!(step(&h.app), None);
    }
}
