//! The Edit panel at its narrowest with a long profile applied: the profile row fits (#1759).

use egui::{Rect, vec2};
use serde_json::json;

use crate::{LightcraftApp, Services, headless::HeadlessView};

const LONG_ID: &str = "lut:kodak-portra-400-plus-one-stop-overexposed";
const LONG_NAME: &str = "Kodak Portra 400 Plus One Stop Overexposed";

fn text_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.clone()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_shapes(s, out)),
        _ => {}
    }
}

fn panel(width: f32) -> (Vec<egui::epaint::TextShape>, Vec<(String, Rect)>) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.session.lut_profiles.push(lightcraft_engine::cmd::lut_profiles::LutProfile {
        id: LONG_ID.into(),
        name: LONG_NAME.into(),
        group: "Film Stocks".into(),
        file: String::new(),
    });
    app.run("develop.profile", json!({"id": LONG_ID})).unwrap();
    let photo = app.session.active().unwrap();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    let mut texts = Vec::new();
    let mut widgets = Vec::new();
    for frame in 0..3 {
        let raw = HeadlessView::raw_input(vec2(width, 4000.0), 1.0, frame as f64 / 60.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                crate::panels::edit::show(&mut app, ui, photo);
            });
        });
        out.textures_delta.clear();
        texts.clear();
        for shape in &out.shapes {
            text_shapes(&shape.shape, &mut texts);
        }
        widgets = crate::widgets::take_registry(&ctx);
    }
    (texts, widgets)
}

#[test]
fn a_long_profile_name_leaves_the_edit_panel_its_width() {
    let width = crate::state::RIGHT_WIDTH.min;
    let (texts, widgets) = panel(width);
    let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not drawn")).1;
    let dropdown = rect("dropdown:profile");
    let browse = rect("icon:profileBrowser");
    assert!(dropdown.right() <= browse.left(), "the profile menu {dropdown:?} runs into Browse Profiles {browse:?}");
    let name = texts.iter().find(|t| t.galley.job.text == LONG_NAME).unwrap();
    assert!(name.galley.elided, "a name wider than the row is cut short");
    assert!(name.visual_bounding_rect().right() <= dropdown.right(), "the name leaves its menu");
    // every slider drawn, from the profile's Amount down
    rect("sliderValue:profile.amount");
    rect("sliderValue:light.exposure");
    for (id, r) in widgets.iter().filter(|(id, _)| id.starts_with("slider")) {
        assert!(r.right() <= width, "{id} {r:?} is cut off by the {width} pt panel");
    }
}
