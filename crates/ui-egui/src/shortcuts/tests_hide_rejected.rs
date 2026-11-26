//! Cmd/Ctrl+Alt+X (View > Hide Rejected Photos, issue #3513) hides the rejected photos and shows
//! them again from the keyboard and the menu, and leaves the key to a focused text field.
use egui::{Key, Modifiers};
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;

/// Four photos: 1 picked and 2 rejected with four stars, 3 unflagged and 4 rejected without; a
/// four-star filter is on and photo 1 is selected.
fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for (id, rating, flag) in [(1, 4, Flag::Pick), (2, 4, Flag::Reject), (3, 0, Flag::None), (4, 0, Flag::Reject)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-hide-rejected-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-11-02",
        );
        photo.rating = rating;
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1]}))?;
    s.execute("library.filter", &json!({"rating": 4}))?;
    Ok(LightcraftApp::new(s, crate::Services { png: None, ..Default::default() }))
}

/// One frame with `key` pressed with `modifiers`, and a focused text field when `text` is given.
fn frame(app: &mut LightcraftApp, ctx: &egui::Context, key: Option<(Key, Modifiers)>, text: Option<&mut String>) {
    let events =
        key.map(|(key, modifiers)| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }).into_iter().collect();
    let mut text = text;
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("hide-rejected-key-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        },
    );
    out.textures_delta.clear();
}

/// The chord as egui reports it on macOS (⌥⌘X) and on Windows and Linux (Ctrl+Alt+X, where Ctrl
/// is also `command`).
const CHORDS: [(&str, Modifiers); 2] = [
    ("macOS", Modifiers { alt: true, ctrl: false, shift: false, mac_cmd: true, command: true }),
    ("Windows/Linux", Modifiers { alt: true, ctrl: true, shift: false, mac_cmd: false, command: true }),
];

/// The photos shown, by id.
fn shown(app: &mut LightcraftApp) -> Vec<u64> {
    app.session.visible().iter().map(|p| p.0).collect()
}

/// Each photo's flag, by id.
fn flags(app: &LightcraftApp) -> Vec<(u64, Flag)> {
    let mut v: Vec<(u64, Flag)> = app.session.catalog.photos().map(|p| (p.id.0, p.flag)).collect();
    v.sort_by_key(|(id, _)| *id);
    v
}

#[test]
fn the_key_is_cmd_alt_x() {
    assert_eq!(super::shortcut_of(&Default::default(), "library.hideRejected"), Some("Cmd+Alt+X"));
    assert_eq!(crate::menubar::shortcut_text("Cmd+Alt+X", true), "⌥⌘X");
    assert_eq!(crate::menubar::shortcut_text("Cmd+Alt+X", false), "Ctrl+Alt+X");
}

#[test]
fn the_key_hides_the_rejects_and_shows_them_again_in_the_grids_and_the_loupe() {
    for (os, chord) in CHORDS {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
            let mut app = app().unwrap();
            app.ui.view = view;
            let ctx = egui::Context::default();
            assert_eq!(shown(&mut app), vec![1, 2], "{os} {view:?}: four stars, rejects included");
            frame(&mut app, &ctx, Some((Key::X, chord)), None);
            assert_eq!(shown(&mut app), vec![1], "{os} {view:?}: four stars and not rejected");
            assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(true), "{os} {view:?}");
            frame(&mut app, &ctx, Some((Key::X, chord)), None);
            assert_eq!(shown(&mut app), vec![1, 2], "{os} {view:?}: the rejects are back, the stars still apply");
            assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(false), "{os} {view:?}");
            assert_eq!(flags(&app), vec![(1, Flag::Pick), (2, Flag::Reject), (3, Flag::None), (4, Flag::Reject)], "{os} {view:?}: no flag changed");
        }
    }
}

#[test]
fn the_menu_item_does_the_same_and_its_check_follows_the_filter_bar() {
    let mut app = app().unwrap();
    let entry = crate::menus::menu_entries(&app).into_iter().find(|e| e.id == "library.hideRejected").expect("in the menus");
    assert_eq!((entry.menu, entry.shortcut.as_deref()), (vec!["View".to_string()], Some("Cmd+Alt+X")));
    crate::menubar::run_item(&mut app, "library.hideRejected", serde_json::Value::Null).unwrap();
    assert_eq!(shown(&mut app), vec![1]);
    // the filter bar's Picked + Unflagged toggles are the same filter, so the row shows it either way
    app.run("library.filter", json!({"flags": []})).unwrap();
    assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(false));
    app.run("library.filter", json!({"flags": ["pick", "none"]})).unwrap();
    assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(true));
    // Picked alone hides the rejects too, but also the unflagged photos: not this row's state
    app.run("library.filter", json!({"flags": ["pick"]})).unwrap();
    assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(false));
}

#[test]
fn plain_x_still_rejects_and_the_chord_yields_to_a_focused_text_field() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    // Ctrl+Alt is AltGr on Windows: in a text box it types a character, so the chord must not act
    for (os, chord) in CHORDS {
        let ctx = egui::Context::default();
        let mut text = String::new();
        frame(&mut app, &ctx, None, Some(&mut text));
        frame(&mut app, &ctx, Some((Key::X, chord)), Some(&mut text));
        assert!(ctx.egui_wants_keyboard_input(), "{os}");
        assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(false), "{os}: the text field kept the key");
    }
    // without a text field, X alone is still Flag as Rejected on the selected photo
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Some((Key::X, Modifiers::NONE)), None);
    assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Reject);
    assert_eq!(crate::menubar::checked(&app, "library.hideRejected"), Some(false), "X alone doesn't hide them");
}
