//! Check painted text as well as widget rectangles: custom painters can overflow a valid widget.

use egui::{Rect, pos2, vec2};
use serde_json::json;

use crate::{LightcraftApp, Services, headless::HeadlessView, i18n::Locale};

const LONG_NAME: &str = "The person standing beside the very long fence across the background";

fn text_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.clone()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_shapes(s, out)),
        _ => {}
    }
}

fn panel(width: f32, scale: f32, locale: Locale, describe: bool) -> (Vec<egui::epaint::TextShape>, Vec<(String, Rect)>) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.run("mask.add", json!({"kind": "subject", "name": LONG_NAME})).unwrap();
    app.ui.describe = describe.then(|| ("new".into(), LONG_NAME.repeat(4)));
    app.ui.detail_due = Some((100.0, 1));
    let photo = app.session.active().unwrap();
    let ctx = egui::Context::default();
    crate::i18n::set_language(locale);
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    let mut texts = Vec::new();
    let mut widgets = Vec::new();
    for frame in 0..3 {
        let raw = HeadlessView::raw_input(vec2(width, 1600.0), scale, frame as f64 / 60.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                crate::panels::masking::show(&mut app, ui, photo);
            });
        });
        out.textures_delta.clear();
        texts.clear();
        for shape in &out.shapes {
            text_shapes(&shape.shape, &mut texts);
        }
        widgets = crate::widgets::take_registry(&ctx);
    }
    if describe {
        assert_eq!(app.ui.describe.as_ref().unwrap().1, LONG_NAME.repeat(4), "layout must not shorten the actual prompt");
    }
    crate::i18n::set_language(Locale::En);
    (texts, widgets)
}

#[test]
fn masking_tile_labels_fit_without_eliding_at_supported_panel_widths() {
    for locale in [Locale::En, Locale::De, Locale::PtBr] {
        for width in [250.0, 270.0, 330.0, 520.0] {
            for scale in [1.0, 2.0] {
                let (texts, widgets) = panel(width, scale, locale, false);
                for (kind, label) in [("background", "Background"), ("luminanceRange", "Luminance"), ("prompt", "Describe")] {
                    crate::i18n::set_language(locale);
                    let label = crate::i18n::tr(label).to_string();
                    crate::i18n::set_language(Locale::En);
                    let tile = widgets.iter().find(|(id, _)| id == &format!("maskNew:{kind}")).unwrap().1;
                    let text = texts.iter().find(|t| t.galley.job.text == label && t.pos.y < tile.bottom()).unwrap();
                    assert!(!text.galley.elided, "{locale:?} {width} {label} should be readable in full");
                    assert!(
                        tile.shrink2(vec2(2.0, 0.0)).contains_rect(text.visual_bounding_rect()),
                        "{locale:?} {width} {scale}: {label} exceeds {tile:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn masking_long_names_and_describe_controls_stay_inside_the_panel() {
    for locale in [Locale::En, Locale::De, Locale::PtBr] {
        for width in [250.0, 270.0, 520.0] {
            let (texts, widgets) = panel(width, 2.0, locale, true);
            let row = widgets.iter().find(|(id, _)| id == "mask:1").unwrap().1;
            let name = texts.iter().find(|t| t.galley.job.text == LONG_NAME).unwrap();
            assert!(name.galley.elided, "long mask names should be elided");
            assert!(
                Rect::from_min_max(pos2(row.left() + 32.0, row.top()), pos2(row.right() - 28.0, row.bottom()))
                    .contains_rect(name.visual_bounding_rect())
            );
            let field = widgets.iter().find(|(id, _)| id == "maskDescribe").unwrap().1;
            let button = widgets.iter().find(|(id, _)| id == "button:maskDescribeGo").unwrap().1;
            assert!(field.right() <= button.left(), "the prompt must not overlap Select");
            assert!(field.left() >= 24.0 && button.right() <= width - 22.0, "{locale:?} {width}: {field:?} {button:?}");
        }
    }
}
