//! The welcome reaches everyone: a screen reader hears its words (the title as a heading), and
//! the keyboard starts on Import Photos… in a new library with a ring showing where it is.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use egui::{Rect, pos2, vec2};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// (role, name, value) of every node a frame of `draw` gives screen readers.
fn nodes(draw: impl Fn(&mut egui::Ui)) -> Vec<(String, String, String)> {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut found = Vec::new();
    for _ in 0..3 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| draw(ui));
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            found = update
                .nodes
                .iter()
                .map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string(), n.value().unwrap_or_default().to_string()))
                .collect();
        }
    }
    found
}

#[test]
fn a_screen_reader_hears_the_welcome_not_just_its_buttons() {
    let actions = [crate::panels::EmptyAction { command: "file.addPhotos", label: "Import Photos…", widget: "button:emptyImportPhotos" }];
    let found = nodes(|ui| {
        let r = ui.max_rect();
        let _ = crate::panels::empty_state(
            ui,
            r,
            "Welcome to LightCraft",
            "Start with photos of your own.",
            &actions,
            Some("or drop photos and folders into this window"),
            crate::panels::EmptyMore::default(),
        );
        crate::panels::empty_note(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 100.0)), "Import photos to get started");
    });
    let has = |role: &str, words: &str| found.iter().any(|(r, l, v)| r == role && (l == words || v == words));
    // each text is read once: a heading by its name, other text by its value
    assert!(found.iter().all(|(_, l, v)| l.is_empty() || v.is_empty()), "words read twice: {found:?}");
    assert!(has("Heading", "Welcome to LightCraft"), "the title, as a heading: {found:?}");
    assert!(has("Label", "Start with photos of your own."), "the body: {found:?}");
    assert!(has("Label", "or drop photos and folders into this window"), "the hint: {found:?}");
    assert!(has("Button", "Import Photos…"), "{found:?}");
    assert!(has("Label", "Import photos to get started"), "the right panel's note: {found:?}");
}

fn library(tag: &str) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-access-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

#[test]
fn the_keyboard_starts_on_import_photos_in_a_new_library() {
    // Given a new library's welcome, with a picker that counts being asked
    let (mut h, asked) = welcome_counting_picks("focus");
    // When Enter is pressed, with no click before it
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    // Then Import Photos… asked for the photos
    assert_eq!(asked.get(), 1, "the keyboard was on Import Photos…");
}

/// A new library's welcome, with a picker that counts being asked.
fn welcome_counting_picks(tag: &str) -> (Headless, Rc<Cell<usize>>) {
    let mut a = LightcraftApp::new(library(tag), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    let asked = Rc::new(Cell::new(0));
    let seen = asked.clone();
    a.services.pick_files = Some(Box::new(move || {
        seen.set(seen.get() + 1);
        Vec::new()
    }));
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h.step();
    (h, asked)
}

#[test]
fn a_launch_notice_over_the_welcome_keeps_the_keyboard_off_its_button() {
    // Given a new library's welcome that opens with a launch notice (no graphics adapter, say) over it
    let mut a = LightcraftApp::new(library("notice"), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.notices.push("No graphics processor found".to_string());
    let asked = Rc::new(Cell::new(0));
    let seen = asked.clone();
    a.services.pick_files = Some(Box::new(move || {
        seen.set(seen.get() + 1);
        Vec::new()
    }));
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    for _ in 0..3 {
        h.step();
    }
    assert!(!crate::widgets::plain_button_has_focus(&h.view.ctx), "the keyboard waits while the notice is up");
    // When Enter is pressed (as someone would on a button of the notice)
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    // Then nothing was imported behind it
    assert_eq!(asked.get(), 0, "Enter reached Import Photos… behind the notice");
    // and once the notice is gone the welcome still takes the keyboard, once
    h.app.notices.clear();
    h.step();
    h.step();
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(asked.get(), 1, "the keyboard went to Import Photos… after the notice");
}

#[test]
fn a_notice_that_comes_after_the_keyboard_is_on_the_welcome_keeps_enter_from_pressing_it() {
    // Given a new library's welcome with the keyboard on Import Photos…
    let (mut h, asked) = welcome_counting_picks("late-notice");
    // When a notice comes over it (the graphics check finishes a few frames after the window is up)
    h.app.notices.push("No graphics processor found".to_string());
    h.step();
    h.step();
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    // Then Enter didn't start an import behind it
    assert_eq!(asked.get(), 0, "Enter reached Import Photos… behind the notice");
    // and the notice took the keyboard with it (#2913 clears focus across a prompt): once it is gone
    // nothing is focused, so Enter doesn't press Import Photos… either
    h.app.notices.clear();
    h.step();
    h.step();
    assert!(!crate::widgets::plain_button_has_focus(&h.view.ctx), "the notice cleared the keyboard");
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(asked.get(), 0, "Enter pressed Import Photos… after the notice cleared the focus");
}

#[test]
fn the_keyboard_is_put_on_import_photos_once() {
    // a person who moved it away keeps it away: Esc lets go, and Enter then starts nothing
    let (mut h, asked) = welcome_counting_picks("once");
    assert_eq!(h.request("ui.key", json!({"key": "Escape"}), T)["ok"], true);
    h.step();
    h.step();
    h.step();
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(asked.get(), 0, "focus wasn't put back");
}

#[test]
fn the_list_over_a_new_library_takes_the_keys_from_the_welcome_s_button() {
    // Given a new library's welcome with the keyboard on Import Photos…
    let (mut h, asked) = welcome_counting_picks("alt-list");
    // When Alt held as the window appears opens the launch list, and Return is pressed
    h.app.alt_asks_for_library = true;
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.library_problem.as_ref().is_some_and(|p| p.chooser.is_some()), "the list shows");
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    h.step();
    h.step();
    // Then the button behind the list didn't take it
    assert_eq!(asked.get(), 0, "Return reached Import Photos… behind the list");
}

#[test]
fn each_library_opened_gets_the_keyboard_on_its_welcome() {
    // Given a session whose welcome has already put the keyboard on its button
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.welcome_focused = true;
    // When a library is opened (as the first frame does, and as a switch does)
    app.library_changed(&egui::Context::default());
    // Then its welcome puts the keyboard there again
    assert!(!app.welcome_focused);
}

/// Given the welcome in Detail with the keyboard on Import Photos…,
/// When Esc is pressed,
/// Then it lets go of the button and goes no further: Back (out of Detail) waits for the next Esc.
#[test]
fn esc_on_the_welcomes_button_only_lets_go_of_it() {
    let mut a = LightcraftApp::new(library("esc-back"), Services::default());
    a.ui.view = ViewMode::Detail;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h.step();
    assert!(crate::widgets::plain_button_has_focus(&h.view.ctx), "the keyboard is on the welcome first");
    assert_eq!(h.request("ui.key", json!({"key": "Escape"}), T)["ok"], true);
    h.step();
    h.step();
    assert!(!crate::widgets::plain_button_has_focus(&h.view.ctx), "Esc let go of the button");
    assert_eq!(h.app.ui.view, ViewMode::Detail, "and did not also go Back");
}

#[test]
fn shortcuts_still_work_with_the_keyboard_on_the_welcome() {
    // the welcome's focus keeps Enter and Space; Import Photos…'s own shortcut, and the rest, act
    let (mut h, asked) = welcome_counting_picks("shortcut");
    assert!(crate::widgets::plain_button_has_focus(&h.view.ctx), "the keyboard is on the welcome first");
    assert_eq!(h.request("ui.key", json!({"key": "I", "cmd": true, "shift": true}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(asked.get(), 1, "Cmd/Ctrl+Shift+I opened Import Photos…");
    assert!(!h.app.text_focus, "a focused button isn't a text field");
}

/// Given the keyboard on a plain button (`text_focus` false),
/// When it moves to a text field (Shift+Tab from the welcome's button),
/// Then a frame is asked for at once, so the native menu hears of it in the next `logic` and not at the key's release.
#[test]
fn a_frame_is_asked_for_when_the_keyboard_moves_to_a_text_field() {
    let mut app = LightcraftApp::new(library("text-focus-repaint"), Services::default());
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut text = String::new();
    let mut frame = |app: &mut LightcraftApp, focus: bool| {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let field = ui.text_edit_singleline(&mut text);
            if focus {
                field.request_focus();
            }
            app.note_keyboard_owner(ui.ctx());
        });
        out.textures_delta.clear();
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    };
    for _ in 0..3 {
        frame(&mut app, false);
    }
    assert!(!app.text_focus);
    assert_eq!(frame(&mut app, true), Duration::ZERO, "the change asks for a frame");
    assert!(app.text_focus);
}
