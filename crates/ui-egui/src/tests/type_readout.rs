//! What is read off a photo or a control is written one way: an exposure in the same words
//! wherever it is shown, a negative value with a minus, values in a column of figures.
//! (The parts are tested in `readout.rs`; here, what a person sees.)

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1600.0, 1000.0], 1.0);
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

/// Where the text that is exactly `said` was drawn.
fn drawn(h: &Headless, said: &str) -> egui::Rect {
    h.view
        .painted_shapes()
        .iter()
        .find_map(|shape| match shape {
            egui::Shape::Text(text) if text.galley.job.text == said => Some(text.visual_bounding_rect()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "{said:?} is not drawn; drawn: {:?}",
                h.painted_text().iter().filter(|t| t.chars().any(|c| c.is_ascii_digit())).collect::<Vec<_>>()
            )
        })
}

#[test]
fn a_photos_exposure_reads_the_same_wherever_it_is_shown() {
    // Given a photo of the demo library and what it was taken with
    let mut h = demo();
    run(&mut h, "library.select", json!({"ids": [1]}));
    let meta = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(1)).map(|p| p.meta.clone()).expect("photo 1");
    let parts = crate::readout::exposure(&meta);
    let line = parts.join(" · ");
    assert_eq!(parts.len(), 4, "the demo photo has all four: {line}");
    assert!(line.contains(" sec · f/") && line.contains(" · ISO ") && line.ends_with(" mm"), "{line}");
    let said = |h: &Headless, what: &str| h.painted_text().iter().any(|t| t == what);
    // When it is shown in the Info panel, each part is on its row as the line has it
    run(&mut h, "view.photoGrid", json!({}));
    set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200}));
    for part in parts.iter().filter(|part| !part.starts_with("ISO")) {
        assert!(said(&h, part), "Info says {part:?}: {:?}", h.painted_text());
    }
    // And over a square cell of the grid, the line
    run(&mut h, "view.squareGrid", json!({}));
    run(&mut h, "view.gridInfo", json!({"info": "exposure"}));
    set(&mut h, json!({"thumbSize": 320}));
    let lines: Vec<String> = h.app.session.catalog.photos().filter_map(|p| crate::readout::exposure_line(&p.meta)).collect();
    let over_cells: Vec<String> = h.painted_text().into_iter().filter(|t| t.contains(" · ISO ")).collect();
    assert!(over_cells.len() >= 3, "cells say what their photos were taken with: {over_cells:?}");
    for over in &over_cells {
        assert!(lines.contains(over), "the grid says {over:?}, which is no photo's line");
    }
    run(&mut h, "view.squareGrid", json!({}));
    // And under the histogram of the Edit panel, the line
    run(&mut h, "view.detail", json!({}));
    set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light"]}));
    assert!(said(&h, &line), "the histogram says {line:?}");
    // And in the overlay on the photo, the line
    run(&mut h, "view.infoOverlay", json!({}));
    if !said(&h, &line) {
        run(&mut h, "view.infoOverlay", json!({}));
    }
    assert!(said(&h, &line), "the overlay says {line:?}");
    // Then nowhere on these screens is an aperture spaced out or a focal length run together
    for text in h.painted_text() {
        assert!(!text.contains("f / "), "{text:?}");
        assert!(!text.as_bytes().windows(3).any(|w| w[0].is_ascii_digit() && &w[1..] == b"mm"), "{text:?}");
    }
}

#[test]
fn a_negative_value_has_a_minus_and_is_typed_with_either_sign() {
    // Given a photo being edited
    let mut h = demo();
    run(&mut h, "library.select", json!({"ids": [1]}));
    run(&mut h, "view.detail", json!({}));
    set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light"]}));
    // When its exposure is taken down
    run(&mut h, "develop.set", json!({"values": {"light.exposure": -1.25}}));
    // Then the value is written with a minus, not a hyphen
    let texts = h.painted_text();
    assert!(texts.iter().any(|t| t == "−1.25"), "{texts:?}");
    assert!(!texts.iter().any(|t| t == "-1.25"));
    // And what is typed into the value is read with the minus it was shown with, or a hyphen
    let exposure = lightcraft_develop::controls::find("light.exposure").expect("the exposure control");
    assert_eq!(crate::widgets::typed_value(exposure, "−1.25"), Some(-1.25));
    assert_eq!(crate::widgets::typed_value(exposure, "-1.25"), Some(-1.25));
    assert_eq!(crate::widgets::typed_value(exposure, "−"), None);
}

/// Scenario: a value is typed with the digits an Arabic or Persian keyboard gives.
#[test]
fn a_value_is_typed_in_arabic_or_persian_digits() {
    // Given the exposure, contrast and temperature controls
    let exposure = lightcraft_develop::controls::find("light.exposure").expect("the exposure control");
    let contrast = lightcraft_develop::controls::find("light.contrast").expect("the contrast control");
    let temp = lightcraft_develop::controls::find("wb.temp").expect("the temperature control");
    // When a value is typed in Arabic-Indic digits, with the Arabic decimal sign or a point
    // Then it is the number those digits write
    assert_eq!(crate::widgets::typed_value(exposure, "١٫٢٥"), Some(1.25));
    assert_eq!(crate::widgets::typed_value(exposure, "١.٢٥"), Some(1.25));
    assert_eq!(crate::widgets::typed_value(exposure, "-٠٫٥"), Some(-0.5));
    assert_eq!(crate::widgets::typed_value(exposure, "−٠٫٥"), Some(-0.5), "with the minus a value is shown with");
    // And with the Arabic percent sign, or a unit, after it
    assert_eq!(crate::widgets::typed_value(contrast, "٢٠٪"), Some(20.0));
    assert_eq!(crate::widgets::typed_value(temp, "٥٦٠٠ K"), Some(5600.0));
    // And in the Persian forms of the digits, or the two kinds together
    assert_eq!(crate::widgets::typed_value(exposure, "۱٫۲۵"), Some(1.25));
    assert_eq!(crate::widgets::typed_value(contrast, "۲0"), Some(20.0));
    // And with the direction marks a keyboard or a paste can put around a number
    assert_eq!(crate::widgets::typed_value(contrast, "\u{200f}٢٠\u{200e}"), Some(20.0));
    assert_eq!(crate::widgets::typed_value(contrast, "\u{61c}-٢٠"), Some(-20.0));
    // (a number pasted from an app that wraps it in an isolate or an embedding)
    assert_eq!(crate::widgets::typed_value(contrast, "\u{2068}٢٠\u{2069}"), Some(20.0));
    assert_eq!(crate::widgets::typed_value(contrast, "\u{202a}20\u{202c}"), Some(20.0));
    // And with the Arabic comma for the decimal sign, as a comma is one
    assert_eq!(crate::widgets::typed_value(exposure, "١،٥"), Some(1.5));
    // And an angle with its degree sign, whatever follows the sign
    assert_eq!(crate::widgets::typed_value(contrast, "٥°\u{200e}"), Some(5.0));
    assert_eq!(crate::widgets::typed_value(contrast, "5 °"), Some(5.0));
    // But what is not a number stays one: the slider keeps its value
    for bad in ["٫", "١٫٢٫٣", "٪", "١٬٢٠٠", "٢٠ك"] {
        assert_eq!(crate::widgets::typed_value(contrast, bad), None, "{bad:?}");
    }
}

#[test]
fn the_values_of_sliders_stand_in_one_column() {
    // Given a photo being edited
    let mut h = demo();
    run(&mut h, "library.select", json!({"ids": [1]}));
    run(&mut h, "view.detail", json!({}));
    set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light"]}));
    // When two sliders hold numbers of as many figures, narrow ones and wide ones
    run(&mut h, "develop.set", json!({"values": {"light.contrast": 11, "light.highlights": 44}}));
    // Then the two values begin at the same place, and end within half the room a 1 leaves in
    // its cell (as the font draws them, "+11" is six points narrower than "+44")
    let (narrow, wide) = (drawn(&h, "+11"), drawn(&h, "+44"));
    assert!((narrow.left() - wide.left()).abs() < 1.0, "{narrow:?} {wide:?}");
    assert!((narrow.right() - wide.right()).abs() < 2.5, "{narrow:?} {wide:?}");
}

#[test]
fn the_counts_of_the_sidebar_are_set_in_figures_of_one_width() {
    // Given the library with its sidebar and the counts beside its rows
    let mut h = demo();
    run(&mut h, "view.photoGrid", json!({}));
    set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200}));
    let left = h.app.widgets.iter().find(|(w, _)| w == "thumb:1").map_or(480.0, |(_, r)| r.left());
    // When the count of all photos is drawn (two figures)
    let all = h.app.session.catalog.photos().count().to_string();
    let counts: Vec<egui::epaint::TextShape> = h
        .view
        .painted_shapes()
        .into_iter()
        .filter_map(|shape| match shape {
            egui::Shape::Text(text) if text.galley.job.text == all && text.pos.x < left => Some(text),
            _ => None,
        })
        .collect();
    // Then on the row of All Photos a figure narrower than the widest is given the room it
    // lacks before it (the font's own "2" is narrower than its "4"; rows drawn by other code,
    // such as By Date, are not this change's)
    let room = |count: &egui::epaint::TextShape| count.galley.job.sections.first().map_or(0.0, |s| s.leading_space);
    assert!(all.starts_with('2'), "the demo has twenty-odd photos ({all})");
    assert!(counts.iter().any(|count| room(count) > 0.1), "no count of {all} in the sidebar is set in cells ({} counts)", counts.len());
}

#[test]
fn a_value_opens_for_typing_with_a_hyphen_a_keyboard_has() {
    // Given a negative and a positive value as a slider shows them
    let exposure = lightcraft_develop::controls::find("light.exposure").expect("the exposure control");
    assert_eq!(crate::widgets::shown_value(exposure, -1.25), "−1.25");
    // When the value is opened to be typed over
    // Then the draft has the keyboard's hyphen (what is copied from it pastes anywhere) and no plus
    assert_eq!(crate::widgets::draft_value(exposure, -1.25), "-1.25");
    assert_eq!(crate::widgets::draft_value(exposure, 0.5), "0.50");
    assert_eq!(crate::widgets::typed_value(exposure, &crate::widgets::draft_value(exposure, -1.25)), Some(-1.25));
}
