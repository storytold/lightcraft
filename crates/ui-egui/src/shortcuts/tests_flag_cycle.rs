//! The backtick (`) key cycles the flag of the selection / active photo; Cmd+Up / Cmd+Down raise / lower it.
use egui::{Key, Modifiers};
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;

fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=3 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-flag-cycle-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-13",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [2]}))?;
    Ok(LightcraftApp::new(s, crate::Services { png: None, ..Default::default() }))
}

fn flags(app: &LightcraftApp) -> Vec<Flag> {
    (1..=3).map(|id| app.session.catalog.photo(PhotoId(id)).unwrap().flag).collect()
}

fn frame(app: &mut LightcraftApp, ctx: &egui::Context, key: Option<(Key, Modifiers)>, text: Option<&mut String>) {
    let events =
        key.map(|(key, modifiers)| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }).into_iter().collect();
    let mut text = text;
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::new("flag-cycle-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        },
    );
    out.textures_delta.clear();
}

fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    frame(app, ctx, Some((Key::Backtick, Modifiers::NONE)), None);
}

#[test]
fn backtick_parses_and_formats_as_the_grave_key() {
    assert_eq!(super::parse("`"), Some((Modifiers::NONE, Key::Backtick)));
    assert_eq!(super::format(Modifiers::NONE, Key::Backtick).as_deref(), Some("`"));
    assert_eq!(super::shortcut_of(&Default::default(), "photo.cycleFlag"), Some("`"));
}

#[test]
fn backtick_cycles_the_selected_photo_in_grid_and_loupe() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
        let mut app = app().unwrap();
        app.ui.view = view;
        let ctx = egui::Context::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            tick(&mut app, &ctx);
            seen.push(flags(&app)[1]);
        }
        assert_eq!(seen, [Flag::Pick, Flag::Reject, Flag::None], "{view:?}");
        assert_eq!(flags(&app)[0], Flag::None);
        assert_eq!(flags(&app)[2], Flag::None);
        assert_eq!(app.session.undo.len(), 3);
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(flags(&app)[1], Flag::Reject);
    }
}

#[test]
fn cmd_up_and_down_raise_and_lower_the_flag() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    let cmd = Modifiers::COMMAND;
    frame(&mut app, &ctx, Some((Key::ArrowUp, cmd)), None);
    assert_eq!(flags(&app)[1], Flag::Pick);
    frame(&mut app, &ctx, Some((Key::ArrowDown, cmd)), None);
    frame(&mut app, &ctx, Some((Key::ArrowDown, cmd)), None);
    assert_eq!(flags(&app)[1], Flag::Reject);
    assert_eq!(app.session.undo.len(), 3);
}

#[test]
fn backtick_does_nothing_while_a_text_field_has_focus() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    let mut text = String::new();
    frame(&mut app, &ctx, None, Some(&mut text));
    frame(&mut app, &ctx, Some((Key::Backtick, Modifiers::NONE)), Some(&mut text));
    assert!(ctx.egui_wants_keyboard_input());
    assert_eq!(flags(&app), [Flag::None; 3]);
    assert!(app.session.undo.is_empty());
}

#[test]
fn compare_flags_only_the_active_photo_and_auto_advance_moves_on() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::Compare;
    let ctx = egui::Context::default();
    app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    tick(&mut app, &ctx);
    assert_eq!(flags(&app), [Flag::None, Flag::Pick, Flag::None]);
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.auto_advance = true;
    app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
    tick(&mut app, &ctx);
    assert_eq!(flags(&app), [Flag::Pick, Flag::Pick, Flag::None]);
    assert_eq!(app.session.active(), Some(PhotoId(2)));
}

#[test]
fn a_saved_assignment_of_the_key_wins_over_the_default() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    super::assign(&mut app.ui.settings.keymap, "photo.reject", Some("`")).unwrap();
    tick(&mut app, &ctx);
    assert_eq!(flags(&app)[1], Flag::Reject);
}

#[test]
fn the_menu_item_follows_the_same_culling_rules() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::Compare;
    app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    crate::menubar::run_item(&mut app, "photo.cycleFlag", serde_json::Value::Null).unwrap();
    assert_eq!(flags(&app), [Flag::None, Flag::Pick, Flag::None]);
    assert_eq!(crate::menubar::shortcut_text("Cmd+Up", true), "⌘↑");
}
