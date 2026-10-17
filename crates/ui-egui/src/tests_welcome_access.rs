//! The welcome reaches everyone: a screen reader hears its words (the title as a heading), the
//! keyboard starts on Import Photos… in a new library with a ring showing where it is, and dim
//! text reads at WCAG AA (4.5:1) in every theme.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use egui::{Rect, pos2, vec2};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::theme::{ThemeKind, Tokens};
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
            0.0,
            false,
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
    let mut a = LightcraftApp::new(library("focus"), Services::default());
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

/// WCAG 2 contrast of two colours.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let lum = |c: egui::Color32| {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn dim_text_reads_at_aa_on_every_surface_of_every_theme() {
    // the welcome's body and hint, the right panel's note, the sidebar's counts: 12–13 pt
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        for (surface, colour) in [("chrome", t.chrome), ("canvas", t.canvas), ("grid", t.grid_bg)] {
            let c = contrast(t.text_dim, colour);
            assert!(c >= 4.5, "{kind:?} text_dim on {surface}: {c:.2}:1");
        }
        // and it stays dimmer than the labels it sits under
        assert!(contrast(t.text_label, t.grid_bg) > contrast(t.text_dim, t.grid_bg), "{kind:?}");
    }
}
