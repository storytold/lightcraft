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

/// The web build omits the tour page that names the desktop-only launch flow (`--control`,
/// `lightcraft-cli mcp --connect`): a browser has no control port for an agent to attach to.
/// The host is a compile-time fact (wasm32), so this harness — native, a desktop host — tests
/// the pure decision ([`crate::dialogs::welcome_pages`]) for both answers.
#[test]
fn the_web_tour_omits_the_desktop_only_mcp_page() {
    let all = crate::dialogs::WELCOME_PAGES.as_slice();
    assert_eq!(crate::dialogs::welcome_pages(false), all, "the desktop shows every page");
    let web = crate::dialogs::welcome_pages(true);
    assert_eq!(web, &all[..all.len() - 1], "the web tour drops the last page");
    // the dropped page is the desktop-only one: its opening names the launch flow
    let (_, _, opening) = all.last().unwrap();
    assert!(opening.contains("--control 7980") && opening.contains("lightcraft-cli"), "{opening}");
}

/// The first frame decides the launch dialog, but records nothing until the dialog is presented:
/// a window host with no stored version gets the tour; a headless host (the harness, snapshots,
/// scripted runs) gets nothing; a version already seen gets nothing; a version that changed gets
/// What's New.
#[test]
fn the_first_frame_opens_the_tour_for_a_window_host_only() {
    let ctx = egui::Context::default();
    let logic = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        out.textures_delta.clear();
    };
    let fresh = || LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });

    let mut window = fresh();
    logic(&mut window);
    assert!(matches!(window.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", window.ui.dialog);
    assert_eq!(window.ui.seen_version, None, "chosen, not yet presented");

    let mut headless = fresh();
    headless.headless_host = true;
    logic(&mut headless);
    assert!(headless.ui.dialog.is_none(), "{:?}", headless.ui.dialog);
    assert_eq!(headless.ui.seen_version, None);

    let mut seen = fresh();
    seen.ui.seen_version = Some(env!("CARGO_PKG_VERSION").to_string());
    logic(&mut seen);
    assert!(seen.ui.dialog.is_none(), "{:?}", seen.ui.dialog);

    let mut updated = fresh();
    updated.ui.seen_version = Some("0.0.1".to_string());
    logic(&mut updated);
    assert!(matches!(updated.ui.dialog, Some(Dialog::WhatsNew)), "{:?}", updated.ui.dialog);
}

/// The version is recorded when the dialog is presented, not when the logic tick picks it: a
/// hidden host that runs `logic` and quits before a visible frame must not burn the tour.
#[test]
fn the_startup_dialog_is_recorded_when_presented_not_when_chosen() {
    let ctx = egui::Context::default();
    let frame = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.logic(ui.ctx());
            // the presentation pass (`ui` waits for the fonts, so frame one only runs `logic`)
            app.ui(ui);
        });
        out.textures_delta.clear();
    };
    let mut held = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });

    frame(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "not presented yet, not recorded");

    frame(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    // shown once: closing it does not bring it back
    held.ui.dialog = None;
    frame(&mut held);
    assert!(held.ui.dialog.is_none(), "{:?}", held.ui.dialog);
}

/// Another dialog open on the first frame keeps its place: the tour is not chosen that frame and
/// nothing is recorded. Once that dialog closes, the tour opens and is recorded then — shown, not
/// merely due.
#[test]
fn the_tour_waits_for_a_dialog_that_is_already_open() {
    let ctx = egui::Context::default();
    let logic = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        out.textures_delta.clear();
    };
    let present = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    };
    let mut held = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    held.ui.dialog = Some(Dialog::ViewOptions);

    logic(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::ViewOptions)), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "not shown, not recorded");

    held.ui.dialog = None;
    logic(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "chosen behind the other dialog's turn, not yet presented");
    present(&mut held);
    assert_eq!(held.ui.seen_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    // shown once: closing it does not bring it back
    held.ui.dialog = None;
    logic(&mut held);
    assert!(held.ui.dialog.is_none(), "{:?}", held.ui.dialog);
}

/// The launch chooser or a library error is the blocking library window, and it is not in
/// `ui.dialog`: it must still hold the startup dialog back, or the chooser's New Library name
/// prompt (a `ui.dialog` of its own) would take the tour's place with the version already
/// recorded. Once the window closes or is dismissed, the tour opens and is recorded then.
#[test]
fn the_tour_waits_for_the_launch_window_too() {
    let ctx = egui::Context::default();
    let logic = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        out.textures_delta.clear();
    };
    let present = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    };
    let mut held = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    held.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("", "the last library did not open"));

    logic(&mut held);
    assert!(held.ui.dialog.is_none(), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "not shown behind the library window, not recorded");

    held.library_problem = None;
    logic(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "chosen, not yet presented");
    present(&mut held);
    assert_eq!(held.ui.seen_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    // shown once: closing it does not bring it back
    held.ui.dialog = None;
    logic(&mut held);
    assert!(held.ui.dialog.is_none(), "{:?}", held.ui.dialog);
}

/// A dismissed library window (Continue Without Saving) shows the banner instead of the window:
/// it no longer blocks, so the startup dialog opens over it as usual.
#[test]
fn the_tour_opens_once_the_library_window_is_dismissed() {
    let ctx = egui::Context::default();
    let frame = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    };
    let mut held = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut problem = crate::panels::library_problem::LibraryProblem::new("", "the last library did not open");
    problem.dismissed = true;
    held.library_problem = Some(problem);

    frame(&mut held);
    assert!(matches!(held.ui.dialog, Some(Dialog::Welcome { page: 0 })), "{:?}", held.ui.dialog);
    assert_eq!(held.ui.seen_version, None, "chosen this frame, presented once the fonts are in");
    frame(&mut held);
    assert_eq!(held.ui.seen_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
}

/// The browser build's File menu disables Print (`menus::ui_enabled` turns `dialog.print` off on
/// wasm), so the Share page's opening line names Export alone instead of a menu item the host
/// greys out. The desktop keeps both menu paths and the live Print binding.
#[test]
fn the_web_tour_names_export_alone_not_the_desktop_print_menu() {
    use crate::panels::dialogs::welcome_opening;
    assert_eq!(welcome_opening(4, "File › Export… · File › Print…", Some("Ctrl+P"), true), "File › Export…");
    assert_eq!(welcome_opening(4, "File › Export… · File › Print…", Some("Ctrl+P"), false), "File › Export… · File › Print… (Ctrl+P).");
    assert_eq!(welcome_opening(4, "File › Export… · File › Print…", None, false), "File › Export… · File › Print…");
}
