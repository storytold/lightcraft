//! Text is set in seven sizes, one per job (`theme::TextSize`), so that two sizes on a screen are
//! always told apart and the same job is the same size on every panel.
//!
//! Before: 17 sizes from 8.5 to 20 pt, 102 of 237 uses on half points. A slider's name (12.5) stood
//! by a checkbox's (13), a caption was 11 in Settings and 11.5 in Info, and panel titles came in
//! 13.5, 14, 15 and 17.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::settings::TABS;
use crate::theme::{TextSize, ThemeKind, Tokens};
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
    h.settle(T);
    h.step();
    h.step();
}

fn set(h: &mut Headless, what: Value) {
    let r = h.request("ui.set", what, T);
    assert_eq!(r["ok"], true, "ui.set: {r}");
    h.settle(T);
    h.step();
    h.step();
}

/// The sizes of the text the last frame drew that are not on the scale, each with a piece of
/// text drawn in it (hundredths of a point, so that they sort).
fn off_scale(h: &Headless) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for shape in h.view.painted_shapes() {
        let egui::Shape::Text(text) = shape else { continue };
        for section in &text.galley.job.sections {
            let size = section.format.font_id.size;
            if !TextSize::ALL.iter().any(|role| (role.points() - size).abs() < 0.01) {
                out.entry((size * 100.0).round() as u32).or_insert_with(|| text.galley.job.text.chars().take(40).collect());
            }
        }
    }
    out
}

fn assert_on_scale(h: &Headless, screen: &str) {
    let off = off_scale(h);
    assert!(off.is_empty(), "{screen}: text drawn off the scale (size × 100, a sample): {off:?}");
}

#[test]
fn the_library_draws_its_text_on_the_scale() {
    // Given the demo library in the grid, with the sidebar and each right panel in turn
    let mut h = demo();
    run(&mut h, "view.photoGrid", json!({}));
    run(&mut h, "library.select", json!({"ids": [1]}));
    for right in ["info", "keywords", "edit", "versions", "activity"] {
        // When the panel is shown
        set(&mut h, json!({"leftPanel": true, "right": right, "thumbSize": 200}));
        // Then every piece of text on screen is one of the seven sizes
        assert_on_scale(&h, &format!("grid with {right}"));
    }
}

#[test]
fn a_photo_being_edited_draws_its_text_on_the_scale() {
    // Given a photo opened by itself with the Edit panel's sections open and the filmstrip under it
    let mut h = demo();
    run(&mut h, "library.select", json!({"ids": [1]}));
    run(&mut h, "view.detail", json!({}));
    for right in ["edit", "info", "masking", "crop", "remove", "profiles"] {
        // When each tool panel is shown
        set(&mut h, json!({"leftPanel": false, "right": right, "openSections": ["light", "color", "effects", "detail"]}));
        // Then every piece of text on screen is one of the seven sizes
        assert_on_scale(&h, &format!("detail with {right}"));
    }
}

#[test]
fn settings_and_the_dialogs_draw_their_text_on_the_scale() {
    let mut h = demo();
    // Given each tab of Settings
    for (tab, _) in TABS {
        // When it is shown
        run(&mut h, "app.settings", json!({"tab": tab}));
        // Then its title, tabs, headings, rows and explanations are on the scale
        assert_on_scale(&h, &format!("Settings ▸ {tab}"));
    }
    h.request("ui.dialog.cancel", json!({}), T);
    // And given the dialogs a person meets most
    for command in ["dialog.export", "app.shortcuts", "app.about"] {
        // When one is open
        run(&mut h, command, json!({}));
        // Then so is everything in it
        assert_on_scale(&h, command);
        h.request("ui.dialog.cancel", json!({}), T);
        h.step();
    }
}

#[test]
fn a_size_asked_for_by_number_is_given_on_the_scale() {
    let t = Tokens::for_kind(ThemeKind::Charcoal);
    let on_scale = |points: f32| TextSize::ALL.iter().any(|size| size.points() == points);
    // Given any number of points, also none at all
    for asked in (0..=120).map(|quarter| quarter as f32 * 0.25).chain([f32::NAN, f32::INFINITY, -4.0]) {
        // When either face is asked for at it
        let (regular, strong) = (t.font(asked), t.semibold(asked));
        // Then the font is one of the seven sizes
        assert!(on_scale(regular.size) && on_scale(strong.size), "{asked} gave {} and {}", regular.size, strong.size);
    }
    // And a size between two goes to the one nearer a control's, one below the smallest to the smallest
    for (asked, given) in [(8.5, 10.0), (10.5, 11.0), (11.5, 12.0), (12.5, 13.0), (13.5, 13.0), (16.0, 15.0), (17.0, 18.0), (20.0, 18.0)] {
        assert_eq!(TextSize::nearest(asked).points(), given, "{asked}");
    }
    // And what is past either end is the end, however far
    for (asked, given) in [(1e9, 18.0), (f32::MAX, 18.0), (f32::INFINITY, 18.0), (-1e9, 10.0), (f32::NEG_INFINITY, 10.0), (0.0, 10.0)] {
        assert_eq!(TextSize::nearest(asked).points(), given, "{asked}");
    }
    assert_eq!(TextSize::nearest(f32::NAN), TextSize::Body);
    // And a size of the scale is itself
    for size in TextSize::ALL {
        assert_eq!(TextSize::nearest(size.points()), size);
    }
}

#[test]
fn the_line_over_a_square_cell_ends_before_the_format() {
    // Given the square grid with small cells, and each thing a cell can say over its photo
    // (one language: the language is the whole process's, and other tests read English)
    for language in ["English"] {
        let mut h = demo();
        run(&mut h, "view.photoGrid", json!({}));
        run(&mut h, "view.squareGrid", json!({}));
        for info in ["exposure", "date", "filename"] {
            run(&mut h, "view.gridInfo", json!({"info": info}));
            set(&mut h, json!({"thumbSize": 110}));
            // When the cells are drawn
            let texts: Vec<(String, egui::Rect)> = h
                .view
                .painted_shapes()
                .iter()
                .filter_map(|shape| match shape {
                    egui::Shape::Text(text) => Some((text.galley.job.text.clone(), text.visual_bounding_rect())),
                    _ => None,
                })
                .collect();
            let formats: Vec<&(String, egui::Rect)> = texts.iter().filter(|(said, _)| said == "JPEG").collect();
            assert!(formats.len() >= 4, "{language} {info}: cells with their format are on screen ({})", formats.len());
            // Then no line runs under a format: it is cut short before it
            for (_, format) in formats {
                let room = format.expand2(egui::vec2(3.0, 0.0));
                let under: Vec<&String> = texts.iter().filter(|(said, at)| said != "JPEG" && at.intersects(room)).map(|(said, _)| said).collect();
                assert!(under.is_empty(), "{language} {info}: {under:?} runs under the format at {format:?}");
            }
        }
    }
}
