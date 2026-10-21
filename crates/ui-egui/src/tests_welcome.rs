//! The first-run Welcome tour: Back / Next walk its pages, Skip and the last page's Get Started
//! close it. (The auto-open at launch is decided by `startup::open_at_startup`, unit-tested there:
//! a headless host never gains the modal, so the tour is placed by hand here.)

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

/// The app with the tour open on `page` (the last page of [`crate::panels::dialogs::WELCOME_PAGES`]
/// is `WELCOME_PAGES.len() - 1`).
fn tour(page: usize) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.dialog = Some(Dialog::Welcome { page });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // fonts land on the second frame, and the dialog window settles its place a frame or two later;
    // clicks must aim at the settled layout
    h.step();
    h.settle(Duration::from_secs(5));
    h
}

fn page_of(h: &Headless) -> Option<usize> {
    match &h.app.ui.dialog {
        Some(Dialog::Welcome { page }) => Some(*page),
        _ => None,
    }
}

/// Click an automation id and let the injected press/release play out (they can straddle a frame).
fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn welcome_pages_walk_back_next_and_skip_closes() {
    let mut h = tour(0);
    // Back is disabled on the first page: the click lands on nothing and the tour stays.
    click(&mut h, "button:welcomeBack");
    assert_eq!(page_of(&h), Some(0), "{:?}", h.app.ui.dialog);
    click(&mut h, "button:welcomeNext");
    assert_eq!(page_of(&h), Some(1), "{:?}", h.app.ui.dialog);
    click(&mut h, "button:welcomeBack");
    assert_eq!(page_of(&h), Some(0), "{:?}", h.app.ui.dialog);
    click(&mut h, "button:welcomeSkip");
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
}

#[test]
fn welcome_last_page_gets_started_closes() {
    let last = crate::panels::dialogs::WELCOME_PAGES.len() - 1;
    let mut h = tour(last);
    click(&mut h, "button:welcomeNext");
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
}

/// The first frame decides the launch dialog. A window host with no stored version gets the tour
/// and records that it was shown; a headless host (the harness, snapshots, scripted runs) gets
/// nothing and records nothing; a version already seen gets nothing; a version that changed gets
/// What's New.
#[test]
fn the_first_frame_opens_the_tour_for_a_window_host_only() {
    let ctx = egui::Context::default();
    let first_frame = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        out.textures_delta.clear();
    };
    let fresh = || LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });

    let mut window = fresh();
    first_frame(&mut window);
    assert!(matches!(window.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", window.ui.dialog);
    assert_eq!(window.ui.seen_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    let mut headless = fresh();
    headless.headless_host = true;
    first_frame(&mut headless);
    assert!(headless.ui.dialog.is_none(), "{:?}", headless.ui.dialog);
    assert_eq!(headless.ui.seen_version, None);

    let mut seen = fresh();
    seen.ui.seen_version = Some(env!("CARGO_PKG_VERSION").to_string());
    first_frame(&mut seen);
    assert!(seen.ui.dialog.is_none(), "{:?}", seen.ui.dialog);

    let mut updated = fresh();
    updated.ui.seen_version = Some("0.0.1".to_string());
    first_frame(&mut updated);
    assert!(matches!(updated.ui.dialog, Some(Dialog::WhatsNew)), "{:?}", updated.ui.dialog);
}
