//! Buttons moved to the shared `Button`: each is still reachable by its automation id and still
//! does what it did (a state change or a command, not just a widget on screen).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::LibraryProblem;
use crate::state::Dialog;
use crate::{LightcraftApp, QuitPrompt, Services, model_setup};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Where `id` was drawn in the last frame.
fn rect_of(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Click a widget and let the synthetic events (move, press, release) play out. A window that
/// just appeared is placed and sized over its first frames, so wait until the widget holds still.
fn click(h: &mut Headless, id: &str) {
    let mut last = None;
    for _ in 0..30 {
        h.step();
        let now = rect_of(h, id);
        if now.is_some() && now == last {
            break;
        }
        last = now;
    }
    assert!(has(h, id), "{id} is not on screen");
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

fn empty_app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() })
}

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(SETTLE);
    h
}

#[test]
fn library_problem_buttons_still_fire() {
    let mut app = empty_app();
    app.library_problem = Some(LibraryProblem::new("/nonexistent/lightcraft-library", "the folder is locked"));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);

    // Continue Without Saving dismisses the window into the banner, whose Open Library… button
    // brings the window back
    click(&mut h, "button:libraryTemporary");
    assert!(h.app.library_problem.as_ref().is_some_and(|p| p.dismissed));
    assert!(!has(&h, "button:libraryQuit"), "the window is gone");
    click(&mut h, "button:libraryReopen");
    assert!(h.app.library_problem.as_ref().is_some_and(|p| !p.dismissed));
    assert!(has(&h, "button:libraryQuit"), "the window is back");

    // Quit asks the app to close
    assert!(!h.quit_requested() && !h.app.ui.quit);
    click(&mut h, "button:libraryQuit");
    assert!(h.quit_requested() || h.app.ui.quit, "Quit closes the app");
}

#[test]
fn notice_buttons_still_fire() {
    let mut app = empty_app();
    app.notices.push("settings.json is damaged".to_string());
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    click(&mut h, "button:noticeOk");
    assert!(h.app.notices.is_empty(), "{:?}", h.app.notices);
    h.step();
    assert!(!has(&h, "button:noticeOk"));

    // the quit question: Cancel leaves the app running
    h.app.quit_prompt = Some(QuitPrompt::Tasks("1 task is still running.".to_string()));
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none());
    assert!(!h.quit_requested() && !h.app.ui.quit);
}

#[test]
fn settings_library_buttons_still_fire() {
    let mut h = demo();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    h.app.services.pick_folder = Some(Box::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        None
    }));
    h.app.run("app.settings", json!({"tab": "general"})).unwrap();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Settings { .. })));
    click(&mut h, "button:settingsOpenLibrary");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "Open Library… asked for a folder");
    click(&mut h, "button:settingsNewLibrary");
    // New Library… asks for the name first (the folder comes after)
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::TextPrompt { command, .. }) if command == "app.newLibrary"), "{:?}", h.app.ui.dialog.is_some());
}

#[test]
fn keymap_icon_buttons_reset_and_remove_a_shortcut() {
    use crate::shortcuts::shortcut_of;

    let mut h = demo();
    h.app.run("app.setShortcut", json!({"id": "view.compare", "shortcut": "Shift+J"})).unwrap();
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.step();
    h.step();
    // the list is longer than the dialog: narrow it so the row is on screen
    click(&mut h, "field:shortcutsSearch");
    assert_eq!(h.request("ui.text", json!({"text": "compare"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+J"));

    // ↺ restores the declared key
    click(&mut h, "button:shortcutReset-view.compare");
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+C"));

    // × removes it
    click(&mut h, "button:shortcutClear-view.compare");
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "view.compare"), None);

    // Reset All gives every declared key back
    click(&mut h, "button:shortcutsResetAll");
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+C"));
}

/// A library with one raw photo and a models folder, but no model installed.
fn denoise_fixture(name: &str) -> (LightcraftApp, std::path::PathBuf, PhotoId) {
    let dir = std::env::temp_dir().join(format!("lc-button-migration-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("library"), true).unwrap();
    s.set_denoise_models_dir(Some(dir.join("denoise")));
    s.execute("denoise.settings", &json!({"runOn": "cpu"})).unwrap();
    let id = PhotoId(s.catalog.photos().map(|p| p.id.0).max().unwrap_or(0) + 1);
    let mut raw = Photo::new(
        id,
        Source::File { path: dir.join("synthetic.dng").to_string_lossy().into_owned() },
        "synthetic.dng",
        "DNG",
        128,
        128,
        "2026-10-07",
    );
    raw.kind = MediaKind::Raw;
    s.commit("fixture", Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    (LightcraftApp::new(s, Services { png: None, ..Default::default() }), dir, id)
}

/// Settings ▸ AI Denoise offers "Cancel pending action" while AI Denoise waits for a model; the
/// button drops the request (the model runtime is not needed for this).
#[test]
fn denoise_cancel_pending_action_button_drops_the_request() {
    let (mut app, dir, raw) = denoise_fixture("cancel");
    let r = app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    assert_eq!(r["setupRequired"], true, "{r}");
    assert!(model_setup::denoise_requested(&app, raw));
    assert!(matches!(app.ui.dialog, Some(Dialog::Settings { .. })));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    click(&mut h, "modelSetup:cancel:denoise");
    assert!(!model_setup::denoise_requested(&h.app, raw), "the pending request is gone");
    h.step();
    assert!(!has(&h, "modelSetup:cancel:denoise"), "and so is the offer");
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

/// A `Button` with an automation id registers its own response, not a hover probe: the id the
/// registry holds is the AccessKit node a screen reader's Focus or Click action targets, and the
/// prompts' buttons depend on it.
#[test]
fn a_button_registers_its_real_response_for_automation() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
    let mut response = None;
    let mut out = ctx.run_ui(raw, |ui| {
        response = Some(crate::button::Button::new("Quit").widget("button:probe").show(ui));
    });
    out.textures_delta.clear();
    let response = response.expect("the frame ran");
    let (rects, hits) = crate::widgets::take_registries(&ctx);
    assert!(rects.iter().any(|(id, rect)| id == "button:probe" && *rect == response.rect), "registered at its rectangle: {rects:?}");
    let (_, hit) = hits.iter().find(|(id, _)| id == "button:probe").expect("a widget hit is recorded");
    assert_eq!(hit.id, response.id, "the hit is the button's own egui id");
    assert_eq!(hit.layer_id, response.layer_id);
}
