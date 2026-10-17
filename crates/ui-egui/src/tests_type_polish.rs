//! Smaller matters of how the interface writes: emphasis that shows, a value no larger than the
//! header over it, a format in capitals with room between its letters, what an empty view says
//! kept inside the view, and a German date with both its numbers in two figures.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::theme::{TextSize, ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2]) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), size, 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn run(h: &mut Headless, command: &str, params: Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    rest(h);
}

fn set(h: &mut Headless, what: Value) {
    let r = h.request("ui.set", what, T);
    assert_eq!(r["ok"], true, "ui.set: {r}");
    rest(h);
}

fn rest(h: &mut Headless) {
    h.settle(T);
    for _ in 0..3 {
        h.step();
    }
}

/// The text shapes whose text has `part`.
fn drawn(h: &Headless, part: &str) -> Vec<egui::epaint::TextShape> {
    h.view
        .painted_shapes()
        .into_iter()
        .filter_map(|shape| match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(part) => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn emphasis_is_the_colour_of_what_is_written_in_every_theme() {
    for kind in ThemeKind::ALL {
        // Given a theme applied
        let ctx = egui::Context::default();
        crate::theme::apply_kind(&ctx, kind);
        let (t, visuals) = (Tokens::for_kind(kind), ctx.global_style().visuals.clone());
        // Then text marked strong is in the text colour, which stands out from a label's, and
        // text marked weak is the dim text
        assert_eq!(visuals.strong_text_color(), t.text, "{kind:?}");
        assert_ne!(visuals.strong_text_color(), visuals.text_color(), "{kind:?}: strong is told from plain");
        assert_eq!(visuals.weak_text_color(), t.text_dim, "{kind:?}");
        // And a label marked strong is drawn in it (the theme's one colour for all text does not
        // take it back)
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.label(egui::RichText::new("marked").strong());
            ui.label("plain");
        });
        let colour = |said: &str| {
            output.shapes.iter().find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.job.text == said => {
                    let own = text.galley.job.sections.first().map(|s| s.format.color).filter(|c| *c != egui::Color32::PLACEHOLDER);
                    Some(text.override_text_color.or(own).unwrap_or(text.fallback_color))
                }
                _ => None,
            })
        };
        assert_eq!((colour("marked"), colour("plain")), (Some(t.text), Some(t.text_label)), "{kind:?}");
        output.textures_delta.clear();
    }
}

#[test]
fn a_value_in_edit_is_no_larger_than_the_header_of_its_section() {
    // Given a photo being edited, the Light and Color sections open
    let mut h = demo([1600.0, 1000.0]);
    run(&mut h, "library.select", json!({"ids": [1]}));
    run(&mut h, "view.detail", json!({}));
    set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light", "color"]}));
    let size = |part: &str| {
        let shapes = drawn(&h, part);
        let shape = shapes.iter().find(|s| s.galley.job.text == part).unwrap_or_else(|| panic!("{part:?} is not drawn"));
        shape.galley.job.sections.first().map(|s| s.format.font_id.size).unwrap_or(0.0)
    };
    // Then the profile and the white balance are written at the size of a control's value, under
    // headers a step larger
    assert_eq!(size("Light"), TextSize::Title.points());
    assert_eq!(size("As Shot"), TextSize::Body.points());
    // (the profile of a demo photo is Color, which is also the name of a section)
    let mut colors: Vec<f32> = drawn(&h, "Color")
        .iter()
        .filter(|s| s.galley.job.text == "Color")
        .filter_map(|s| s.galley.job.sections.first().map(|s| s.format.font_id.size))
        .collect();
    colors.sort_by(f32::total_cmp);
    assert_eq!(colors, [TextSize::Body.points(), TextSize::Title.points()], "the profile's value and the section's header");
}

#[test]
fn a_format_in_capitals_has_room_between_its_letters() {
    // Given the Info panel of a photo
    let mut h = demo([1600.0, 1000.0]);
    run(&mut h, "library.select", json!({"ids": [1]}));
    run(&mut h, "view.photoGrid", json!({}));
    set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200}));
    // Then the format on its card, written in capitals, is spaced a little
    let chips: Vec<f32> = drawn(&h, "JPEG").iter().filter_map(|s| s.galley.job.sections.first().map(|s| s.format.extra_letter_spacing)).collect();
    assert!(chips.iter().any(|spacing| *spacing >= 0.4), "{chips:?}");
}

#[test]
fn what_an_empty_view_says_stays_inside_the_view() {
    // Given a narrow view with nothing to show and much to say
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply_kind(&ctx, ThemeKind::Charcoal);
    let view = egui::Rect::from_min_size(egui::pos2(120.0, 60.0), egui::vec2(300.0, 400.0));
    let body = "Right-click a photo ▸ Set as Reference Photo, then View → Reference View (Shift+R)";
    // When it says what to do
    let mut output = ctx.run_ui(Default::default(), |ui| crate::panels::empty_message(ui, view, "No reference photo", body));
    let said: Vec<egui::epaint::TextShape> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    output.textures_delta.clear();
    // Then the title and the sentence are inside the view, the sentence on more than one line,
    // and the title above it
    assert_eq!(said.len(), 2, "a title and a sentence");
    for text in &said {
        let at = text.visual_bounding_rect();
        assert!(view.contains_rect(at), "{:?} at {at:?} runs out of {view:?}", text.galley.job.text);
    }
    assert!(said[1].galley.rows.len() >= 2, "the sentence is wrapped");
    assert!(said[0].visual_bounding_rect().bottom() <= said[1].visual_bounding_rect().top());
    // And both are centred in it
    for text in &said {
        assert!((text.visual_bounding_rect().center().x - view.center().x).abs() < 3.0, "{:?}", text.visual_bounding_rect());
    }
}

#[test]
fn a_german_date_has_its_day_in_two_figures_as_its_month_is() {
    crate::i18n::set_language(crate::i18n::Locale::De);
    assert_eq!(crate::i18n::date_group_label("2026-09-04", false), "Freitag, 04.09.2026");
    assert_eq!(crate::i18n::date_group_label("2026-09-20", false), "Sonntag, 20.09.2026");
    crate::i18n::set_language(crate::i18n::Locale::En);
}
