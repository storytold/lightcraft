//! Cmd+L (Library > Enable Filters) switches the filter off and on from the keyboard and the menu.
use egui::{Key, Modifiers};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;

fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=3 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-enable-filters-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-23",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.select", &json!({"ids": [1]}))?;
    s.execute("photo.rate", &json!({"ids": [1], "rating": 4}))?;
    s.execute("library.filter", &json!({"rating": 4, "text": "1.jpg"}))?;
    let mut app = LightcraftApp::new(s, crate::Services { png: None, ..Default::default() });
    app.ui.search = "1.jpg".into();
    Ok(app)
}

fn press(app: &mut LightcraftApp, ctx: &egui::Context) {
    let events = vec![egui::Event::Key { key: Key::L, physical_key: Some(Key::L), pressed: true, repeat: false, modifiers: Modifiers::COMMAND }];
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| super::handle(app, ui.ctx()),
    );
    out.textures_delta.clear();
}

#[test]
fn the_key_is_cmd_l() {
    assert_eq!(super::shortcut_of(&Default::default(), "library.toggleFilters"), Some("Cmd+L"));
}

#[test]
fn cmd_l_switches_the_filter_off_and_on_in_the_grids_and_the_loupe() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
        let mut app = app().unwrap();
        app.ui.view = view;
        let ctx = egui::Context::default();
        press(&mut app, &ctx);
        assert_eq!(app.session.visible().len(), 3, "{view:?}: off shows every photo");
        assert_eq!(app.ui.search, "", "{view:?}: the search field follows");
        assert_eq!(crate::menubar::checked(&app, "library.toggleFilters"), Some(false));
        press(&mut app, &ctx);
        assert_eq!(app.session.visible().len(), 1, "{view:?}: on brings the filter back");
        assert_eq!(app.ui.search, "1.jpg");
        assert_eq!(crate::menubar::checked(&app, "library.toggleFilters"), Some(true));
    }
}

#[test]
fn the_menu_item_does_the_same() {
    let mut app = app().unwrap();
    crate::menubar::run_item(&mut app, "library.toggleFilters", serde_json::Value::Null).unwrap();
    assert_eq!(app.session.visible().len(), 3);
    assert_eq!(app.ui.search, "");
    assert_eq!(crate::menubar::shortcut_text("Cmd+L", true), "⌘L");
}
