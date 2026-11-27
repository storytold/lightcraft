//! Backslash (\) shows / hides the Filter Bar in the library grids, as in Lightroom Classic's Library
//! module; in Detail it keeps showing the original (Develop's Before / After key).
use egui::{Key, Modifiers};
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::{BeforeAfter, ViewMode};

fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=2 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-filter-bar-key/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-15",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1]}))?;
    Ok(LightcraftApp::new(s, crate::Services { png: None, ..Default::default() }))
}

fn frame(app: &mut LightcraftApp, ctx: &egui::Context, key: Option<Key>, text: Option<&mut String>) {
    let events = key
        .map(|key| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE })
        .into_iter()
        .collect();
    let mut text = text;
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("filter-bar-key-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        },
    );
    out.textures_delta.clear();
}

#[test]
fn backslash_toggles_the_filter_bar_in_the_grids() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut app = app().unwrap();
        app.ui.view = view;
        let ctx = egui::Context::default();
        assert!(!app.ui.filter_bar);
        frame(&mut app, &ctx, Some(Key::Backslash), None);
        assert!(app.ui.filter_bar, "{view:?}: \\ shows the Filter Bar");
        // the grid's \ is not Show Original: nothing is left switched on for Detail
        assert_eq!(app.ui.before_after, BeforeAfter::Off, "{view:?}");
        frame(&mut app, &ctx, Some(Key::Backslash), None);
        assert!(!app.ui.filter_bar, "{view:?}: \\ hides it again");
        assert_eq!(app.ui.before_after, BeforeAfter::Off, "{view:?}");
        assert_eq!(app.ui.view, view);
    }
}

#[test]
fn backslash_in_detail_still_shows_the_original() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::Detail;
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Some(Key::Backslash), None);
    assert_eq!(app.ui.before_after, BeforeAfter::Original);
    assert!(!app.ui.filter_bar);
    assert_eq!(app.ui.view, ViewMode::Detail);
}

/// F covers the grid with the photo alone: \ there is Show Original (latched), not the hidden Filter Bar.
#[test]
fn backslash_in_the_full_screen_preview_shows_the_original() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut app = app().unwrap();
        app.ui.view = view;
        app.run("view.fullScreenPreview", json!({})).unwrap();
        assert!(app.ui.fullscreen);
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Some(Key::Backslash), None);
        assert_eq!(app.ui.before_after, BeforeAfter::Original, "{view:?}: \\ latches the original");
        assert!(!app.ui.filter_bar, "{view:?}: the hidden Filter Bar is left alone");
        assert_eq!(app.ui.view, view, "the grid is the view behind the preview");
    }
}

#[test]
fn saved_keymap_wins_over_the_grid_backslash() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    // Show Original moved to F2: F2 keeps that meaning in the grid, \ does nothing
    super::assign(&mut app.ui.settings.keymap, "view.showOriginal", Some("F2")).unwrap();
    frame(&mut app, &ctx, Some(Key::F2), None);
    assert!(!app.ui.filter_bar, "a remapped Show Original key is not the Filter Bar key");
    assert_eq!(app.ui.before_after, BeforeAfter::Original);
    frame(&mut app, &ctx, Some(Key::Backslash), None);
    assert!(!app.ui.filter_bar);
    // \ given to another command runs that command
    super::reset(&mut app.ui.settings.keymap, "view.showOriginal").unwrap();
    super::assign(&mut app.ui.settings.keymap, "photo.pick", Some("\\")).unwrap();
    frame(&mut app, &ctx, Some(Key::Backslash), None);
    assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Pick);
    assert!(!app.ui.filter_bar);
}

#[test]
fn backslash_yields_to_a_focused_text_field() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    let mut text = String::new();
    frame(&mut app, &ctx, None, Some(&mut text));
    frame(&mut app, &ctx, Some(Key::Backslash), Some(&mut text));
    assert!(ctx.egui_wants_keyboard_input());
    assert!(!app.ui.filter_bar);
}
