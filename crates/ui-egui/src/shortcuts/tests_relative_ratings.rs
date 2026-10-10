//! Real app command dispatch using egui input, without rendering an image or linking a facade.
use dac_catalog::{Flag, Op, Photo, PhotoId, Source};
use egui::{Key, Modifiers};
use serde_json::json;

use crate::DacApp;
use crate::state::{Dialog, RightPanel, ViewMode};

fn app() -> dac_engine::Result<DacApp> {
    let mut s = dac_engine::Session::new();
    for (id, rating) in [(1, 0), (2, 2), (3, 5)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-relative-rating-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-09",
        );
        photo.rating = rating;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2}))?;
    Ok(DacApp::new(s, crate::Services { png: None, ..Default::default() }))
}

fn ratings(app: &DacApp) -> dac_engine::Result<Vec<u8>> {
    (1..=3)
        .map(|id| app.session.catalog.photo(PhotoId(id)).map(|p| p.rating).ok_or_else(|| dac_catalog::CatalogError::NoPhoto(PhotoId(id)).into()))
        .collect()
}

fn frame(app: &mut DacApp, ctx: &egui::Context, key: Option<Key>, text: Option<&mut String>) {
    let events = key
        .map(|key| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE })
        .into_iter()
        .collect();
    let mut text = text;
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::new("relative-rating-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        },
    );
    out.textures_delta.clear();
}

#[test]
fn grid_brackets_rate_mixed_selection_without_resizing_brushes() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut app = app().unwrap();
        app.ui.view = view;
        let ctx = egui::Context::default();
        let brush = app.ui.brush_size;
        let remove = app.ui.remove_size;
        frame(&mut app, &ctx, Some(Key::CloseBracket), None);
        assert_eq!(ratings(&app).unwrap(), [1, 3, 5]);
        assert_eq!(app.session.undo.len(), 1);
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
        app.session.execute("edit.redo", &json!({})).unwrap();
        frame(&mut app, &ctx, Some(Key::OpenBracket), None);
        assert_eq!(ratings(&app).unwrap(), [0, 2, 4]);
        assert_eq!(app.ui.brush_size, brush);
        assert_eq!(app.ui.remove_size, remove);
        assert_eq!(app.session.active(), Some(PhotoId(2)));
    }
}

#[test]
fn detail_masking_and_remove_keep_bracket_brush_sizing() {
    for panel in [RightPanel::Edit, RightPanel::Masking, RightPanel::Remove] {
        let mut app = app().unwrap();
        app.ui.view = ViewMode::Detail;
        app.ui.right = panel;
        let ctx = egui::Context::default();
        app.ui.brush_size = 0.05;
        app.ui.remove_size = 0.05;
        frame(&mut app, &ctx, Some(Key::CloseBracket), None);
        let size = if panel == RightPanel::Remove { app.ui.remove_size } else { app.ui.brush_size };
        assert!((size - 0.06).abs() < 1e-6);
        frame(&mut app, &ctx, Some(Key::OpenBracket), None);
        let size = if panel == RightPanel::Remove { app.ui.remove_size } else { app.ui.brush_size };
        assert!((size - 0.05).abs() < 1e-6);
        assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
        assert!(app.session.undo.is_empty());
    }
}

#[test]
fn saved_keymap_remapping_unbinding_and_reassignment_win() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    super::assign(&mut app.ui.settings.keymap, "brush.larger", Some("F2")).unwrap();
    super::assign(&mut app.ui.settings.keymap, "brush.smaller", None).unwrap();
    let brush = app.ui.brush_size;
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    frame(&mut app, &ctx, Some(Key::OpenBracket), None);
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
    assert_eq!(app.ui.brush_size, brush);
    frame(&mut app, &ctx, Some(Key::F2), None);
    assert!(app.ui.brush_size > brush, "remapped brush command keeps its meaning");
    super::reset(&mut app.ui.settings.keymap, "brush.smaller").unwrap();
    super::assign(&mut app.ui.settings.keymap, "photo.pick", Some("[")).unwrap();
    frame(&mut app, &ctx, Some(Key::OpenBracket), None);
    assert_eq!(app.session.catalog.photo(PhotoId(2)).unwrap().flag, Flag::Pick);
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
    // Also honor an existing saved override that did not explicitly disable the default.
    super::reset(&mut app.ui.settings.keymap, "brush.larger").unwrap();
    app.ui.settings.keymap.insert("photo.reject".into(), "]".into());
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    assert_eq!(app.session.catalog.photo(PhotoId(2)).unwrap().flag, Flag::Reject);
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
}

#[test]
fn unknown_saved_bracket_assignment_does_not_shadow_grid_ratings() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    app.ui.settings.keymap.insert("retired.command".into(), "]".into());
    assert!(super::find_bindable("retired.command").is_none());
    let brush = app.ui.brush_size;
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    assert_eq!(ratings(&app).unwrap(), [1, 3, 5]);
    assert_eq!(app.session.undo.len(), 1);
    assert_eq!(app.ui.brush_size, brush);
    app.session.execute("edit.undo", &json!({})).unwrap();
    app.ui.settings.keymap.remove("retired.command");
    app.ui.settings.keymap.insert("photo.pick".into(), "]".into());
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    assert_eq!(app.session.catalog.photo(PhotoId(2)).unwrap().flag, Flag::Pick);
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5], "a known saved override still wins");
    assert_eq!(app.ui.brush_size, brush);
}

#[test]
fn relative_rating_keys_use_auto_advance_and_compare_active_target() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
    app.ui.auto_advance = true;
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    assert_eq!(ratings(&app).unwrap(), [1, 2, 5]);
    assert_eq!(app.session.active(), Some(PhotoId(2)));
    app.ui.auto_advance = false;
    app.ui.view = ViewMode::Compare;
    app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    super::assign(&mut app.ui.settings.keymap, "photo.increaseRating", Some("F3")).unwrap();
    frame(&mut app, &ctx, Some(Key::F3), None);
    assert_eq!(ratings(&app).unwrap(), [1, 3, 5]);
    assert_eq!(app.session.active(), Some(PhotoId(2)));
}

#[test]
fn brackets_yield_to_text_focus_and_shortcut_recording() {
    let mut app = app().unwrap();
    app.ui.view = ViewMode::PhotoGrid;
    let ctx = egui::Context::default();
    let mut text = String::new();
    frame(&mut app, &ctx, None, Some(&mut text));
    frame(&mut app, &ctx, Some(Key::CloseBracket), Some(&mut text));
    assert!(ctx.egui_wants_keyboard_input());
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
    assert!(app.session.undo.is_empty());
    let ctx = egui::Context::default();
    app.ui.dialog = Some(Dialog::Shortcuts);
    app.recording_shortcut = Some("brush.larger".into());
    frame(&mut app, &ctx, Some(Key::CloseBracket), None);
    assert_eq!(ratings(&app).unwrap(), [0, 2, 5]);
    assert!(app.session.undo.is_empty());
}
