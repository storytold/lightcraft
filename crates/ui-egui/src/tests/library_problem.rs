//! Issue #100: a library that can't be opened at launch is never replaced by a silent in-memory
//! demo session. The window says why and offers Try Again / Choose Another Library… / Continue
//! Without Saving / Quit; a temporary session shows a banner and never writes to the library.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::LibraryProblem;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// A settings notice owns the frame: the launch list / library-error window is another
/// `Foreground` area and must not be drawn above the prompt or consume its keys.
#[test]
fn a_notice_prompt_hides_the_library_problem_window_and_takes_its_keys() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    h.step();
    h.step();
    assert!(has(&h, "button:libraryRetry"), "the library problem window is shown without a prompt");
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    h.step();
    assert!(has(&h, "button:noticeOk"), "the settings notice is laid out");
    assert!(!has(&h, "button:libraryRetry"), "the library problem window is not drawn above the prompt");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.notices.is_empty(), "Escape safely dismissed the notice");
    assert!(!h.app.ui.quit, "the prompt's Escape did not reach the library window");
    assert!(has(&h, "button:libraryRetry"), "the library problem window returns after the prompt");
}

/// The launch list's own arrow/Return/Escape handling must stay still while a settings notice is
/// up. Use a real chooser: with the old error window this assertion could never reach
/// `chooser_keys` at all.
#[test]
fn a_notice_prompt_over_the_launch_list_takes_escape_without_quitting_it() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.recent_libraries = vec!["/no/such/library-one".into(), "/no/such/library-two".into()];
    app.ui.settings.library_path = "/no/such/library-one".into();
    app.library_problem = Some(LibraryProblem::choose(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "button:libraryOpen"), "the launch list is shown without a prompt");
    let selected = h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).map(|c| c.selected);
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    h.step();
    assert!(has(&h, "button:noticeOk"), "the settings notice is laid out");
    assert!(!has(&h, "button:libraryOpen"), "the launch list is not drawn above the prompt");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.notices.is_empty(), "Escape safely dismissed the notice");
    assert!(!h.quit_requested(), "the prompt's Escape did not reach the launch list");
    assert!(h.app.library_problem.is_some(), "the launch list stays");
    assert_eq!(
        h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).map(|c| c.selected),
        selected,
        "the prompt's Escape did not move the launch-list selection"
    );
    assert!(has(&h, "button:libraryOpen"), "the launch list returns after the prompt");
}

#[test]
fn unopenable_library_asks_instead_of_running_a_demo() {
    let dir = std::env::temp_dir().join(format!("lc-ui-libproblem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();

    // another program has the library open: the launch fails like the desktop host's would
    let held = lightcraft_catalog::LibraryLock::acquire(&lib, "another LightCraft").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    let err = session.open_library(&lib, true).unwrap_err().to_string();
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let mut app = LightcraftApp::new(
        session,
        Services {
            png: Some(Box::new(png)),
            write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
            ..Default::default()
        },
    );
    app.library_problem =
        Some(LibraryProblem { pending_import: vec![photo.to_string_lossy().to_string()], ..LibraryProblem::new(lib.to_string_lossy(), err) });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(h.app.session.catalog.is_empty(), "no demo photos");
    for b in ["button:libraryRetry", "button:libraryChoose", "button:libraryTemporary", "button:libraryQuit"] {
        assert!(has(&h, b), "{b}");
    }
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("dialog.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let inspect = h.request("ui.inspect", json!({}), T);
    let p = &inspect["result"]["libraryProblem"];
    assert!(p["error"].as_str().unwrap().contains("already open in another LightCraft"), "{p}");
    assert_eq!(p["temporarySession"], false);

    // still locked: Try Again keeps the window, with the error
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    h.step();
    assert!(h.app.session.library.is_none());
    assert!(has(&h, "button:libraryRetry"));

    // Continue Without Saving: a banner, the command-line photo in the temporary session, and
    // nothing written into the library folder
    let before: Vec<_> = std::fs::read_dir(&lib).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryTemporary"}), T)["ok"], true);
    h.step();
    h.step();
    assert!(!has(&h, "button:libraryRetry"));
    assert!(has(&h, "indicator:temporarySession"));
    assert!(h.app.session.library.is_none());
    // imported on the import worker (issue #374: never synchronously on the UI thread)
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the command-line photo is imported into the temporary session");
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("banner.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 3}}), T);
    let after: Vec<_> = std::fs::read_dir(&lib).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(before, after, "the temporary session never writes to the library");

    // the other program quits; Open Library… → Try Again opens it, the banner goes away
    drop(held);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryReopen"}), T)["ok"], true);
    h.step();
    assert!(has(&h, "button:libraryRetry"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    h.step();
    h.step();
    assert!(h.app.session.library.is_some());
    assert!(h.app.library_problem.is_none());
    assert!(!has(&h, "indicator:temporarySession") && !has(&h, "button:libraryRetry"));
    assert!(h.app.session.catalog.is_empty(), "an opened (new) library is never seeded with demo photos here");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pressing the dim raises its egui area. The blocking window must be moved back above it, or its
/// own buttons are covered: the click then reports a no-op success and the state never changes.
#[test]
fn a_backdrop_press_keeps_the_library_window_above_its_dim() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1000.0, 760.0], 1.0);
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    assert!(h.settle(Duration::from_secs(20)), "the library window settles");
    assert!(has(&h, "button:libraryTemporary"), "the window is laid out");
    // Outside the centred window: on the full-screen dim, which egui raises when pressed.
    let at = egui::pos2(10.0, 10.0);
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
    h.step();
    h.step();
    assert!(h.app.library_problem.as_ref().is_some_and(|p| !p.dismissed), "the backdrop itself chose nothing");
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryTemporary"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(
        h.app.library_problem.as_ref().is_some_and(|p| p.dismissed),
        "the window's own button made the actual state change after the backdrop press"
    );
    assert!(!has(&h, "button:libraryRetry"), "Continue Without Saving removed the blocking window");
}

/// The library window is another foreground modal over Settings: its raw Escape and outside-click
/// handlers must stay still while it is on screen, then the dialog keeps its ordinary behaviour.
#[test]
fn the_library_window_blocks_settings_escape_and_the_backdrop() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 900.0],
        1.0,
    );
    h.step();
    assert!(h.app.run("app.settings", json!({})).is_ok(), "Settings opens");
    assert!(h.settle(Duration::from_secs(20)), "Settings settles");
    h.app.ui.settings.close_dialogs_outside = true;
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    assert!(h.settle(Duration::from_secs(20)), "the library window settles");
    assert!(has(&h, "button:libraryRetry"), "the library window is laid out");
    // Escape belongs to the window, not to Settings behind it (the window has no Escape action
    // for this error; the launch list handles its own keys separately).
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Escape closed Settings behind the library window");
    assert!(h.app.library_problem.is_some(), "the library window stayed");
    // The outside-click preference reads raw pointer events without asking the layer, so it must
    // be gated too. The dim swallows the click itself.
    let at = egui::pos2(10.0, 890.0);
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "the backdrop click closed Settings behind the library window");
    // Positive control: with the library window gone, Settings' own Escape works again.
    h.app.library_problem = None;
    h.step();
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Escape closes Settings without the library window");
}

/// A dialog opened before the launch list (Settings) is not the list's own New Library prompt:
/// the list stays visible above it and keeps owning Escape, instead of vanishing and letting the
/// unrelated dialog's raw handlers run.
#[test]
fn an_unrelated_open_dialog_does_not_own_the_launch_list_or_its_barrier() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 900.0],
        1.0,
    );
    h.settle(T);
    assert!(h.app.run("app.settings", json!({})).is_ok(), "Settings opens first");
    h.step();
    let mut problem = LibraryProblem::choose(&settings);
    if let Some(c) = problem.chooser.as_mut() {
        c.over = Some(h.app.session.library_generation());
    }
    h.app.library_problem = Some(problem);
    h.step();
    h.step();
    assert!(has(&h, "button:libraryOpen"), "the launch list stays above the unrelated Settings dialog");
    assert!(crate::panels::library_problem::blocks_dialog_input(&h.app), "the launch list owns input over an unrelated dialog");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.library_problem.is_none(), "the launch list's own Escape kept the open library");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Escape did not close Settings behind the launch list");
}

/// The launch list's own Escape is not a Settings raw handler: with no notice prompt blocking it,
/// it still quits from an empty list (the library chooser's long-standing behaviour).
#[test]
fn the_launch_list_still_quits_on_escape_without_a_notice() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.recent_libraries = vec!["/no/such/library-one".into(), "/no/such/library-two".into()];
    app.ui.settings.library_path = "/no/such/library-one".into();
    app.library_problem = Some(LibraryProblem::choose(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "button:libraryOpen"), "the launch list is shown without a prompt");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.quit_requested(), "the launch list's own Escape still quits when no notice blocks it");
}

/// A blocking library window created after `raw_input_hook` must not let the background text
/// field swallow the same frame's host text before the new modal area has settled. The same
/// sequence without the window is the positive control.
fn creation_frame_drops_background_text(problem: LibraryProblem, button: &str) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 760.0], 1.0);
    h.settle(T);
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), T);
    assert_eq!(r["ok"], true, "the search field takes focus: {r}");
    let r = h.request("ui.text", json!({"text": "base"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.search, "base");

    h.events.push(egui::Event::Text("leak".into()));
    let mut problem = Some(problem);
    h.set_after_raw_input_hook(move |app| app.library_problem = problem.take());
    h.step();
    assert_eq!(h.app.ui.search, "base", "the new library window's creation frame dropped the background text");
    assert!(!h.view.ctx.text_edit_focused(), "the background text field's focus was surrendered");
    h.step();
    assert!(has(&h, button), "the blocking window is laid out after its shield pass");

    // Positive control: without the window, the same focused field receives text in one frame.
    h.app.library_problem = None;
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.events.push(egui::Event::Text("after".into()));
    h.step();
    assert_eq!(h.app.ui.search, "baseafter", "ordinary background text input still works");
}

#[test]
fn library_error_and_chooser_creation_frames_drop_background_text() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    creation_frame_drops_background_text(LibraryProblem::new("/no/such/library", "a test error"), "button:libraryRetry");
    creation_frame_drops_background_text(LibraryProblem::choose(&settings), "button:libraryOpen");
}

/// The library barrier removes events that `raw_input_hook` already injected. Like the prompt
/// barrier, it must fail the owning gesture; otherwise `ui.text`, `ui.hoverWidget` and synthetic
/// paste can report `ok: true` for input no panel ever saw. The window is created after the hook,
/// exactly as `alt_at_launch` or a command drawn late in the frame can do.
fn creation_after_raw_input_cancels_injected_text(problem: LibraryProblem) {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let (tx, rx) = std::sync::mpsc::channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.text".into(), json!({"text": "leak"}), tx).expect("the text gesture is admitted");
    h.step(); // resolve: one Text event is queued
    assert_eq!(h.app.synthetic.len(), 1, "the text gesture is ready to inject");
    let mut problem = Some(problem);
    h.set_after_raw_input_hook(move |app| app.library_problem = problem.take());
    h.step(); // raw hook injects; the hook creates the window; the barrier drops the batch
    let reply = rx.try_recv().expect("the dropped gesture answers");
    assert_eq!(reply["ok"], false, "the library barrier must not report a false success: {reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("blocking library window"), "{reply}");
    assert!(h.app.synthetic.is_empty(), "the dropped text batch left nothing queued");
    assert!(h.app.pending_gestures.is_empty(), "the dropped gesture was canceled, not kept");
}

#[test]
fn library_error_and_chooser_creation_after_raw_input_cancel_an_injected_gesture() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    creation_after_raw_input_cancels_injected_text(LibraryProblem::new("/no/such/library", "a test error"));
    creation_after_raw_input_cancels_injected_text(LibraryProblem::choose(&settings));
}

/// `alt_at_launch` creates the chooser from `logic`, after `raw_input_hook` injected this frame's
/// synthetic batch. That later creation path must cancel the dropped gesture too.
#[test]
fn alt_launch_creation_after_raw_input_cancels_an_injected_gesture() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let (tx, rx) = std::sync::mpsc::channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.text".into(), json!({"text": "leak"}), tx).expect("the text gesture is admitted");
    h.step();
    assert_eq!(h.app.synthetic.len(), 1);
    h.app.alt_asks_for_library = true;
    h.time = 0.1; // stay inside the Alt-at-launch watch window regardless of settle time
    h.events.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    h.step(); // raw hook injects Text, then alt_at_launch creates the chooser and its barrier drops it
    assert!(h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some(), "Alt at launch opened the chooser");
    let reply = rx.try_recv().expect("the dropped gesture answers");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("blocking library window"), "{reply}");
    assert!(h.app.synthetic.is_empty() && h.app.pending_gestures.is_empty());
}

/// A held host press over a background control in the same frame as a library window created
/// after `raw_input_hook` is released by the window's first pass; its later release must not
/// become a background click.
#[test]
fn a_held_background_press_is_released_when_a_library_window_appears_after_raw_input() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 900.0],
        1.0,
    );
    h.step();
    assert!(h.app.run("app.settings", json!({})).is_ok(), "Settings opens");
    assert!(h.settle(T), "Settings settles");
    let id = "check:settings.confirmDelete";
    let at = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| rect.center()).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), press(true)]);
    h.set_after_raw_input_hook(|app| app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error")));
    h.step();
    assert!(h.app.library_problem.is_some(), "the library window is shown");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the creation pass released the held press");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the press did not reach Settings");
    h.events.push(press(false));
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the release did not become a background click");
    assert!(h.app.library_problem.is_some(), "the release did not activate the background control");
}

/// A library window first seen after `raw_input_hook` drops wheel, key and background pointer
/// input already accepted for the pass. Observed on egui's actual input state, not on the event
/// list the barrier edits.
#[test]
fn a_library_window_first_seen_after_begin_pass_drops_background_input() {
    let mut h =
        Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }), [800.0, 600.0], 1.0);
    h.settle(T);
    let _ = pass_log(&mut h);
    let at = egui::pos2(30.0, 40.0);
    h.events.extend([
        egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 2.0),
            phase: egui::TouchPhase::Start,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
    ]);
    let mut problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    h.set_after_raw_input_hook(move |app| app.library_problem = problem.take());
    h.step();
    assert!(h.app.library_problem.is_some(), "the window was created after this pass's raw input");
    let (down, keys, scroll) = h.view.ctx.input(|i| (i.pointer.any_down(), i.keys_down.clone(), i.smooth_scroll_delta));
    assert!(!down, "the held background pointer press was released");
    assert!(keys.is_empty(), "the background key press was dropped: {keys:?}");
    assert_eq!(scroll, egui::Vec2::ZERO, "wheel scrolling was cleared");
    assert!(!h.view.ctx.input(|i| i.key_pressed(egui::Key::A)), "the dropped key press is not reported to panels");
    // The creation frame is a dedicated shield pass that egui discards, then the background pass.
    assert_eq!(
        pass_log(&mut h),
        vec![crate::panels::library_problem::LibraryPass::Shield, crate::panels::library_problem::LibraryPass::Background],
        "the creation frame draws only the window and is discarded before the background pass"
    );
}

/// A Touch Start in one pass and its End/Cancel in a pass where a library window appears after
/// `raw_input_hook` must leave egui's own touch state empty: `begin_pass` processes the release
/// before the barrier filters the events. Asserting that the End/Cancel events merely survive the
/// filter would not prove the private touch state this protects.
fn touch_release_clears_egui_touch_state_when_the_library_window_appears_after_begin_pass(phase: egui::TouchPhase) {
    let mut h =
        Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }), [800.0, 600.0], 1.0);
    h.settle(T);
    let at = egui::pos2(30.0, 40.0);
    let touch = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: at, force: None };
    h.events.push(touch(egui::TouchPhase::Start));
    h.step();
    assert!(h.view.ctx.input(|i| i.any_touches()), "the touch is down before the window appears");
    let mut problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    h.set_after_raw_input_hook(move |app| app.library_problem = problem.take());
    h.events.push(touch(phase));
    h.step();
    assert!(h.app.library_problem.is_some(), "the window was created after this pass's raw input");
    assert!(!h.view.ctx.input(|i| i.any_touches()), "{phase:?} must clear egui's own touch state, not only stay in the filtered event batch");
}

#[test]
fn touch_end_and_cancel_clear_egui_touch_state_when_the_library_window_appears_after_begin_pass() {
    touch_release_clears_egui_touch_state_when_the_library_window_appears_after_begin_pass(egui::TouchPhase::End);
    touch_release_clears_egui_touch_state_when_the_library_window_appears_after_begin_pass(egui::TouchPhase::Cancel);
}

fn select_first_photo(h: &mut Headless) -> lightcraft_catalog::PhotoId {
    let photo = h.app.session.visible_cloned()[0];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [photo.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    photo
}

fn press_rating(h: &mut Headless, key: egui::Key) {
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    }
}

fn rating(h: &Headless, photo: lightcraft_catalog::PhotoId) -> u8 {
    h.app.session.catalog.photo(photo).expect("the selected photo").rating
}

/// Take the recorded library-barrier passes (clearing them). A shielded return frame must be
/// visible here even while the global shortcut guard would also keep the rating shortcut from
/// acting: the sequence itself is what these regressions protect.
fn pass_log(h: &mut Headless) -> Vec<crate::panels::library_problem::LibraryPass> {
    std::mem::take(&mut h.app.library_passes)
}

/// The frame that just ran returned from a hidden/unsettled blocking window: it must first be a
/// dedicated shield pass that is discarded, then a background pass. The next frame must be
/// ordinary (no repeated transition). Removing the shield pass, its discard or its settlement
/// changes this sequence; the rating shortcut alone would still stay blocked.
fn assert_shielded_return(h: &mut Headless, what: &str) {
    use crate::panels::library_problem::LibraryPass;
    assert_eq!(
        pass_log(h),
        vec![LibraryPass::Shield, LibraryPass::Background],
        "{what}: the return frame must shield (and discard) the window's first pass, then draw the background"
    );
    h.step();
    assert_eq!(pass_log(h), vec![LibraryPass::Background], "{what}: once settled, later frames draw normally without re-shielding");
}

/// A notice dismissed after the error window was already settled must shield the first frame that
/// draws the window again. A background rating shortcut injected on that frame would otherwise act
/// through the still-focused/background state while egui's hit test uses the previous pass.
#[test]
fn a_notice_dismissal_shields_the_library_windows_first_return_frame() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let photo = select_first_photo(&mut h);
    let before = rating(&h, photo);
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    h.step();
    h.step();
    assert!(has(&h, "button:libraryRetry"), "the error window is settled");
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    h.step();
    assert!(has(&h, "button:noticeOk") && !has(&h, "button:libraryRetry"), "the notice hides the settling window");
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.app.notices.is_empty(), "Escape dismissed the notice");
    // The dismissal happened during the draw; this clears that frame's background-only record.
    let _ = pass_log(&mut h);
    // This is the first frame in which the window is drawn again. The shortcut must be dropped.
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    assert_eq!(rating(&h, photo), before, "the first return frame did not let a shortcut act behind the library window");
    assert_shielded_return(&mut h, "notice dismissal");
    assert!(has(&h, "button:libraryRetry"), "the window is back");
    // A settled blocking window keeps owning the keyboard: shortcuts stay blocked on later frames.
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    h.step();
    assert_eq!(rating(&h, photo), before, "a settled library window did not block a background rating shortcut");
    // Positive control: once the blocking window is dismissed, the same shortcut works.
    h.app.library_problem = None;
    h.step();
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    h.step();
    assert_eq!(rating(&h, photo), 5, "ordinary shortcuts work once the blocking window is gone");
}

/// A library window created while a notice is up has never drawn its own shielded pass. Dismissing
/// the notice must not settle it from a frame where it was hidden by the prompt.
#[test]
fn a_library_window_created_under_a_notice_shields_the_first_frame_after_dismissal() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let photo = select_first_photo(&mut h);
    let before = rating(&h, photo);
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    h.step();
    assert!(has(&h, "button:noticeOk"));
    let mut problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    h.set_after_raw_input_hook(move |app| app.library_problem = problem.take());
    h.step();
    assert!(has(&h, "button:noticeOk") && !has(&h, "button:libraryRetry"), "the prompt still owns the frame");
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.app.notices.is_empty());
    let _ = pass_log(&mut h);
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    assert_eq!(rating(&h, photo), before, "the first frame after dismissal was shielded");
    assert_shielded_return(&mut h, "window created under a notice");
    assert!(has(&h, "button:libraryRetry"), "the window is drawn after its shield pass");
}

/// Closing New Library's own name dialog returns the launch list. The list was hidden for the
/// dialog, so the first pass that draws it again must be shielded rather than settling from the
/// dialog-closing pass.
#[test]
fn closing_the_new_library_dialog_shields_the_launch_lists_first_return_frame() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    let mut h = Headless::new(
        LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            Services { png: None, pick_folder: Some(Box::new(|| -> Option<String> { None })), ..Default::default() },
        ),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let photo = select_first_photo(&mut h);
    let before = rating(&h, photo);
    h.app.library_problem = Some(LibraryProblem::choose(&settings));
    h.settle(T);
    assert!(has(&h, "button:libraryOpen"), "the launch list is settled");
    assert!(h.app.run("app.newLibrary", json!({})).is_ok(), "New Library opens its name prompt");
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })), "the name dialog is open");
    assert!(!has(&h, "button:libraryOpen"), "the launch list yields to the dialog it opened");
    let cancel = h
        .app
        .widgets
        .iter()
        .rev()
        .find(|(id, _)| id == "button:dialogCancel")
        .map(|(_, rect)| rect.center())
        .expect("the dialog's Cancel is on screen");
    let press = |pressed| egui::Event::PointerButton { pos: cancel, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(cancel), press(true), press(false)]);
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Cancel closed the New Library name dialog");
    // The dialog-closing frame drew the list but must not settle it.
    let _ = pass_log(&mut h);
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    assert_eq!(rating(&h, photo), before, "the launch list's first return frame is shielded");
    assert_shielded_return(&mut h, "New Library dialog cancel");
    assert!(has(&h, "button:libraryOpen"), "the launch list is back");
    // Positive control: dismissing the blocking window restores ordinary shortcuts.
    h.app.library_problem = None;
    h.step();
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    h.step();
    assert_eq!(rating(&h, photo), 5, "ordinary shortcuts work once the launch list is gone");
}

/// The banner's Open Library… reopens the window from inside the draw pass, after the background
/// panels already consumed this frame's input. The next frame must be the shielded first frame.
#[test]
fn the_banner_reopen_shields_the_first_input_frame() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    let photo = select_first_photo(&mut h);
    let before = rating(&h, photo);
    h.app.library_problem = Some(LibraryProblem { dismissed: true, ..LibraryProblem::new("/no/such/library", "a test error") });
    h.step();
    h.step();
    assert!(has(&h, "indicator:temporarySession"), "the temporary-session banner is shown");
    let reopen = h
        .app
        .widgets
        .iter()
        .rev()
        .find(|(id, _)| id == "button:libraryReopen")
        .map(|(_, rect)| rect.center())
        .expect("the banner button is on screen");
    let press = |pressed| egui::Event::PointerButton { pos: reopen, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(reopen), press(true), press(false)]);
    h.step();
    assert!(h.app.library_problem.as_ref().is_some_and(|p| !p.dismissed), "the banner reopened the window");
    assert!(has(&h, "button:libraryRetry"));
    // The reopen happened late in that draw, after its own background pass began.
    let _ = pass_log(&mut h);
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    assert_eq!(rating(&h, photo), before, "input on the frame after a banner reopen was shielded");
    assert_shielded_return(&mut h, "banner reopen");
    assert!(has(&h, "button:libraryRetry"), "the window is back after its shield pass");
    // Positive control: dismissing the blocking window restores ordinary shortcuts.
    h.app.library_problem = None;
    h.step();
    press_rating(&mut h, egui::Key::Num5);
    h.step();
    h.step();
    assert_eq!(rating(&h, photo), 5, "ordinary shortcuts work once the window is gone");
}

/// The chooser's own New Library text prompt is not a background barrier: `visible_mode` is None
/// while the dialog is open, so clipboard paste must reach that dialog's focused field.
#[test]
fn clipboard_pastes_into_the_launch_lists_own_text_prompt() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    let mut h = Headless::new(
        LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            Services { png: None, pick_folder: Some(Box::new(|| -> Option<String> { None })), ..Default::default() },
        ),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    h.app.library_problem = Some(LibraryProblem::choose(&settings));
    h.settle(T);
    assert!(h.app.run("app.newLibrary", json!({})).is_ok(), "New Library opens its name prompt");
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })), "the name dialog is open");
    assert!(!crate::panels::library_problem::blocks_dialog_input(&h.app), "a dialog opened by the launch list is not a background barrier");
    h.view.clipboard = "pasted-name".into();
    let r = h.request("ui.clipboard", json!({"action": "paste"}), T);
    assert_eq!(r["ok"], true, "the chooser's own prompt accepts a paste: {r}");
    h.step();
    let Some(crate::state::Dialog::TextPrompt { value, .. }) = &h.app.ui.dialog else {
        panic!("the name dialog closed unexpectedly");
    };
    assert!(value.contains("pasted-name"), "the paste reached the chooser's own text field: {value:?}");
}

/// The blocking library window owns input while it is on screen, including over an unrelated
/// dialog it did not open. Disabling only the dialog widgets that handle pointer input is not
/// enough: `dialog_field` must not take the focus back, or egui's text edit keeps consuming host
/// text into the draft behind the window. Several settled barrier frames must stay inert, and the
/// same field must accept typing again once the window is gone.
#[test]
fn a_settled_library_window_keeps_a_background_dialogs_text_draft_unchanged() {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    h.app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: "Trip".into(), folder: false, parent: None, include_selected: false });
    for _ in 0..4 {
        h.step();
    }
    assert!(h.view.ctx.text_edit_focused(), "the dialog's field takes the focus while it is the top modal");
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));
    assert!(h.settle(T), "the library window settles over the dialog");
    assert!(has(&h, "button:libraryRetry"), "the library window is laid out");
    assert!(crate::panels::library_problem::blocks_dialog_input(&h.app), "the library window owns input");
    assert!(!h.view.ctx.text_edit_focused(), "the background dialog's field must not regain the focus behind the library window");
    for text in ["leak", "ed", "-draft"] {
        h.events.push(egui::Event::Text(text.into()));
        h.step();
        let draft = match &h.app.ui.dialog {
            Some(crate::state::Dialog::NewAlbum { name, .. }) => name.as_str(),
            other => panic!("the background New Album dialog is still open: {other:?}"),
        };
        assert_eq!(draft, "Trip", "host text on a settled library-barrier frame must not mutate the dialog draft");
    }
    assert!(!h.view.ctx.text_edit_focused(), "several settled barrier frames still leave the field unfocused");
    // Positive control: with the library window gone, the same field takes focus and typing again.
    h.app.library_problem = None;
    assert!(h.settle(T), "the dialog settles once the library window is gone");
    assert!(!crate::panels::library_problem::blocks_dialog_input(&h.app), "there is no barrier now");
    assert!(h.view.ctx.text_edit_focused(), "the dialog's field is usable again");
    h.events.push(egui::Event::Text("Holiday".into()));
    h.step();
    let draft = match &h.app.ui.dialog {
        Some(crate::state::Dialog::NewAlbum { name, .. }) => name.as_str(),
        other => panic!("the New Album dialog is still open: {other:?}"),
    };
    assert_eq!(draft, "Holiday", "typing reaches the dialog once the library barrier is gone");
}

/// The launch list's own New Library name prompt is not a background dialog: `visible_mode` is
/// None while it is open, so the dialog must stay enabled and `dialog_field` must keep giving it
/// the focus and accept host text.
#[test]
fn typing_reaches_the_launch_lists_own_new_library_prompt() {
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    let mut h = Headless::new(
        LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            Services { png: None, pick_folder: Some(Box::new(|| -> Option<String> { None })), ..Default::default() },
        ),
        [1200.0, 760.0],
        1.0,
    );
    h.settle(T);
    h.app.library_problem = Some(LibraryProblem::choose(&settings));
    h.settle(T);
    assert!(h.app.run("app.newLibrary", json!({})).is_ok(), "New Library opens its name prompt");
    h.step();
    h.step();
    assert!(!crate::panels::library_problem::blocks_dialog_input(&h.app), "the launch list's own prompt is not a background barrier");
    assert!(h.view.ctx.text_edit_focused(), "the prompt's name field has the focus");
    h.events.push(egui::Event::Text("Fresh".into()));
    h.step();
    let Some(crate::state::Dialog::TextPrompt { value, command, .. }) = &h.app.ui.dialog else {
        panic!("the New Library name dialog closed unexpectedly");
    };
    assert_eq!(command, "app.newLibrary");
    assert_eq!(value, "Fresh", "typing reaches the launch list's own text prompt");
}
