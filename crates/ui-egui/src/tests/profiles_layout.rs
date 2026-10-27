//! The profile browser in a narrow right panel: names stay inside their cells (#1634).

use egui::{Rect, pos2, vec2};
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
                crate::panels::profiles::show(&mut app, ui, photo);
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
fn profile_names_stay_inside_their_cells_in_a_narrow_panel() {
    let width = 250.0;
    let (texts, widgets) = panel(width);
    let cell = widgets.iter().find(|(id, _)| id == &format!("profileCell:{LONG_ID}")).unwrap().1;
    // the cell's name sits below its thumbnail; the header's copy is above every cell
    let name = texts.iter().find(|t| t.galley.job.text == LONG_NAME && t.pos.y >= cell.top()).unwrap();
    assert!(name.galley.elided, "a name wider than its cell is cut short");
    let bounds = name.visual_bounding_rect();
    assert!(bounds.left() >= cell.left() && bounds.right() <= cell.right(), "{bounds:?} spills out of {cell:?}");
    let applied = texts.iter().find(|t| t.galley.job.text == LONG_NAME && t.pos.y < cell.top()).unwrap();
    assert!(applied.visual_bounding_rect().right() <= width, "the applied profile's name leaves the panel");
    // built-in names too: each one stays out of its neighbour's cell
    for p in lightcraft_engine::presets::PROFILES {
        let cell = widgets.iter().find(|(id, _)| id == &format!("profileCell:{}", p.id)).unwrap().1;
        let name = texts.iter().find(|t| t.galley.job.text == p.name && cell.contains(pos2(cell.center().x, t.pos.y))).unwrap();
        let bounds = name.visual_bounding_rect();
        assert!(bounds.left() >= cell.left() && bounds.right() <= cell.right(), "{}: {bounds:?} spills out of {cell:?}", p.name);
    }
}
