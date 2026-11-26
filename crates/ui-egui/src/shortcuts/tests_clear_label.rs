//! Clear Color Label (`photo.clearLabel`) is a command of its own, so the keyboard shortcuts editor can give it a key (#2980).
use egui::{Key, Modifiers};
use lightcraft_catalog::{ColorLabel, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::menubar::MenuNode;
use crate::state::ViewMode;

const KEY: &str = "Alt+Shift+L";

fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=3 {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-clear-label-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-25",
        );
        photo.label = Some(ColorLabel::Green);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [2]}))?;
    let mut app = LightcraftApp::new(s, crate::Services { png: None, ..Default::default() });
    super::assign(&mut app.ui.settings.keymap, "photo.clearLabel", Some(KEY)).unwrap();
    Ok(app)
}

fn labels(app: &LightcraftApp) -> Vec<Option<ColorLabel>> {
    (1..=3).map(|id| app.session.catalog.photo(PhotoId(id)).unwrap().label).collect()
}

fn frame(app: &mut LightcraftApp, ctx: &egui::Context, key: Option<(Key, Modifiers)>, text: Option<&mut String>) {
    let events =
        key.map(|(key, modifiers)| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }).into_iter().collect();
    let mut text = text;
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("clear-label-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        },
    );
    out.textures_delta.clear();
}

fn press(app: &mut LightcraftApp, ctx: &egui::Context) {
    frame(app, ctx, Some((Key::L, Modifiers::ALT | Modifiers::SHIFT)), None);
}

#[test]
fn clearing_a_label_has_no_key_until_one_is_given() {
    assert_eq!(super::shortcut_of(&Default::default(), "photo.clearLabel"), None);
    assert!(super::find_bindable("photo.clearLabel").is_some(), "the shortcuts editor lists it");
    let mut app = app().unwrap();
    assert_eq!(super::shortcut_of(&app.ui.settings.keymap, "photo.clearLabel"), Some(KEY));
    // the number keys keep Lightroom's meaning: 6-9 set a label, nothing is taken from them
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Some((Key::Num6, Modifiers::NONE)), None);
    assert_eq!(labels(&app)[1], Some(ColorLabel::Red));
}

#[test]
fn the_key_clears_the_selected_photos_label_in_grid_and_loupe() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
        let mut app = app().unwrap();
        app.ui.view = view;
        let ctx = egui::Context::default();
        press(&mut app, &ctx);
        assert_eq!(labels(&app), [Some(ColorLabel::Green), None, Some(ColorLabel::Green)], "{view:?}");
        assert_eq!(app.session.undo.len(), 1);
        assert_eq!(app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Color label cleared"));
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(labels(&app)[1], Some(ColorLabel::Green));
    }
}

#[test]
fn the_key_does_nothing_while_a_text_field_has_focus() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    let mut text = String::new();
    frame(&mut app, &ctx, None, Some(&mut text));
    frame(&mut app, &ctx, Some((Key::L, Modifiers::ALT | Modifiers::SHIFT)), Some(&mut text));
    assert!(ctx.egui_wants_keyboard_input());
    assert_eq!(labels(&app), [Some(ColorLabel::Green); 3]);
    assert!(app.session.undo.is_empty());
}

#[test]
fn compare_clears_only_the_active_photo_and_auto_advance_moves_on() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::Compare;
    let ctx = egui::Context::default();
    app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    press(&mut app, &ctx);
    assert_eq!(labels(&app), [Some(ColorLabel::Green), None, Some(ColorLabel::Green)]);
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.auto_advance = true;
    app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
    press(&mut app, &ctx);
    assert_eq!(labels(&app), [None, None, Some(ColorLabel::Green)]);
    assert_eq!(app.session.active(), Some(PhotoId(2)));
}

#[test]
fn a_label_given_to_clear_color_label_is_ignored_by_the_toast_too() {
    let mut app = app().unwrap();
    app.run("photo.clearLabel", json!({"label": "red"})).unwrap();
    assert_eq!(labels(&app)[1], None, "the photo's label is cleared, whatever label was passed");
    let toast = app.ui.toast.as_ref().expect("a toast");
    assert_eq!((toast.0.as_str(), toast.2), ("Color label cleared", None), "not \"Red Label\"");
}

fn find<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
    nodes.iter().find_map(|n| match n {
        MenuNode::Item { id: i, .. } if i == id => Some(n),
        MenuNode::Submenu { children, .. } => find(children, id),
        _ => None,
    })
}

#[test]
fn the_menus_none_row_is_the_command_and_shows_its_key() {
    let mut app = app().unwrap();
    let bar = crate::menubar::menu_bar(&app);
    let all: Vec<MenuNode> = bar.into_iter().flat_map(|(_, v)| v).collect();
    let Some(MenuNode::Item { shortcut, label, .. }) = find(&all, "photo.clearLabel") else {
        panic!("Photo ▸ Set Color Label ▸ None is Clear Color Label")
    };
    assert_eq!(label, "None");
    assert_eq!(shortcut.as_deref(), Some(KEY));
    // and the row behaves like the key: Compare clears only the active photo
    app.ui.view = ViewMode::Compare;
    app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    crate::menubar::run_item(&mut app, "photo.clearLabel", serde_json::Value::Null).unwrap();
    assert_eq!(labels(&app), [Some(ColorLabel::Green), None, Some(ColorLabel::Green)]);
}
