//! Words that are longer in another language keep to their place: a button does not lie over
//! its neighbour, a choice's name stays inside the choice, a row of the filter bar ends inside
//! the bar, and a value begins after its label.
//!
//! Before, in German: Reset ("Zurücksetzen") lay over HDR in the head of the Edit panel, "Mit
//! Hilfslinien" ran out of its third of the Upright choices, and the filter bar's first row ran
//! off the grid at "Bearbeitet oder nicht". (The card of Info already gives a long label its
//! room, "Velocidade do obturador" among them: its scenario keeps it so.)

use std::time::Duration;

use egui::Rect;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(language: &str) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1600.0, 1000.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    run(&mut h, language, json!({}));
    run(&mut h, "library.select", json!({"ids": [1]}));
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
    h.request("ui.move", json!({"x": 2.0, "y": 2.0}), T);
    // (a layout that follows what it measured the frame before has come to rest)
    for _ in 0..5 {
        h.step();
    }
}

fn rect(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn widgets(h: &Headless, prefix: &str) -> Vec<(String, Rect)> {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).cloned().collect()
}

/// Every piece of text drawn, with where it is.
fn texts(h: &Headless) -> Vec<(String, Rect)> {
    h.view
        .painted_shapes()
        .iter()
        .filter_map(|shape| match shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), text.visual_bounding_rect())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_buttons_at_the_head_of_edit_do_not_lie_over_one_another() {
    for language in ["app.language.english", "app.language.german", "app.language.russian", "app.language.french"] {
        // Given a photo being edited in a language
        let mut h = demo(language);
        run(&mut h, "view.detail", json!({}));
        set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light"]}));
        // When the head of the Edit panel is drawn
        let buttons: Vec<(&str, Rect)> = ["auto", "bw", "hdr", "reset"].into_iter().map(|id| (id, rect(&h, &format!("button:{id}")))).collect();
        // Then no button touches another
        for (i, (a, ra)) in buttons.iter().enumerate() {
            for (b, rb) in buttons.iter().skip(i + 1) {
                assert!(!ra.expand(1.0).intersects(*rb), "{language}: {a} {ra:?} lies over {b} {rb:?}");
            }
        }
    }
}

/// Reset used to be drawn in a right-to-left layout of the head's single row, and lay over HDR in
/// German. Now the head wraps: Reset goes on the row of the others where it has room (English),
/// on a row of its own where it has not (German), and at the right of the head either way.
#[test]
fn reset_is_on_the_row_of_the_others_when_it_has_room_and_below_them_when_it_has_not() {
    for (language, own_row) in [("app.language.english", false), ("app.language.german", true)] {
        // Given a photo being edited in a language
        let mut h = demo(language);
        run(&mut h, "view.detail", json!({}));
        set(&mut h, json!({"leftPanel": false, "right": "edit", "openSections": ["light"]}));
        // When the head of the Edit panel is drawn
        let hdr = rect(&h, "button:hdr");
        let reset = rect(&h, "button:reset");
        // Then Reset is below the others only where it has no room beside them
        if own_row {
            assert!(reset.top() >= hdr.bottom(), "{language}: reset {reset:?} is not below hdr {hdr:?}");
        } else {
            assert!((reset.center().y - hdr.center().y).abs() < 4.0, "{language}: reset {reset:?} is not on the row of hdr {hdr:?}");
            assert!(reset.left() >= hdr.right(), "{language}: reset {reset:?} lies over hdr {hdr:?}");
        }
        // And at the right of the head either way
        assert!(reset.right() > hdr.right(), "{language}: reset {reset:?} is not at the right of hdr {hdr:?}");
    }
}

#[test]
fn the_name_of_a_choice_stays_inside_the_choice() {
    for language in ["app.language.english", "app.language.german", "app.language.russian", "app.language.french", "app.language.spanish"] {
        // Given the Crop panel in a language, with its rows of choices
        let mut h = demo(language);
        run(&mut h, "view.detail", json!({}));
        set(&mut h, json!({"leftPanel": false, "right": "crop"}));
        let drawn = texts(&h);
        let mut choices = widgets(&h, "button:cropOverlay-");
        choices.extend(widgets(&h, "button:upright-"));
        assert!(choices.len() >= 10, "{language}: the overlay and upright choices are on screen ({})", choices.len());
        // Then each choice's name lies inside it, with a little room at each side
        for (id, choice) in choices {
            let name = drawn.iter().find(|(_, at)| choice.contains(at.center())).unwrap_or_else(|| panic!("{language}: {id} has no name"));
            assert!(
                choice.shrink2(egui::vec2(2.0, 0.0)).contains_rect(name.1),
                "{language}: {:?} at {:?} runs out of {id} {choice:?}",
                name.0,
                name.1
            );
        }
    }
}

#[test]
fn a_row_of_the_filter_bar_ends_inside_the_bar() {
    for language in ["app.language.english", "app.language.german", "app.language.russian", "app.language.french"] {
        // Given the grid with both side panels and the filter bar, in a language
        let mut h = demo(language);
        run(&mut h, "view.photoGrid", json!({}));
        set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200, "filterBar": true}));
        let bar = rect(&h, "filterbar");
        // When its controls are drawn
        let mut controls = widgets(&h, "dropdown:filter-");
        controls.extend(["button:filterSave", "button:filterClear", "button:filterPresets"].into_iter().map(|id| (id.to_string(), rect(&h, id))));
        assert!(controls.len() >= 8, "{language}: {}", controls.len());
        // Then every one of them lies in the bar
        for (id, control) in controls {
            assert!(bar.contains_rect(control), "{language}: {id} {control:?} runs out of the bar {bar:?}");
        }
    }
}

#[test]
fn a_value_on_the_card_of_info_begins_after_its_label() {
    for language in ["app.language.english", "app.language.german", "app.language.portuguese", "app.language.spanish", "app.language.french"] {
        // Given the Info panel of a photo in a language
        let mut h = demo(language);
        run(&mut h, "view.photoGrid", json!({}));
        set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200}));
        let drawn = texts(&h);
        let meta = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(1)).map(|p| p.meta.clone()).expect("photo 1");
        let mut begins = Vec::new();
        for (label, value) in [
            ("Focal length", meta.focal_mm.and_then(crate::readout::focal)),
            ("Shutter speed", crate::readout::shutter(&meta.shutter)),
            ("Aperture", meta.aperture.and_then(crate::readout::aperture)),
        ] {
            let (label, value) = (crate::i18n::tr(label).to_string(), value.expect("the demo photo has it"));
            let at = |said: &str| {
                drawn.iter().find(|(text, _)| text == said).map(|(_, at)| *at).unwrap_or_else(|| panic!("{language}: {said:?} is not drawn"))
            };
            let (label_at, value_at) = (at(&label), at(&value));
            // Then the value begins at least a gap after its label ends
            assert!(value_at.left() >= label_at.right() + 8.0, "{language}: {label:?} {label_at:?} and {value:?} {value_at:?}");
            begins.push(value_at.left());
        }
        // And the values stand under one another
        assert!(begins.iter().all(|x| (x - begins[0]).abs() < 1.0), "{language}: {begins:?}");
    }
}

#[test]
fn the_filter_bar_comes_to_rest_at_every_width() {
    for language in ["app.language.english", "app.language.german", "app.language.french"] {
        // Given the grid and the filter bar in a language, and what the bar's three parts measure
        let mut h = demo(language);
        run(&mut h, "view.photoGrid", json!({}));
        set(&mut h, json!({"leftPanel": true, "right": "info", "thumbSize": 200, "filterBar": true}));
        let parts: [f32; 3] = h.view.ctx.data(|d| d.get_temp(egui::Id::unique("filterbar:widths"))).expect("the bar has measured its parts");
        assert!(parts.iter().all(|w| *w > 40.0), "{language}: {parts:?}");
        let gap = 4.0;
        // When the window is as wide as leaves the bar a point or two more or less than each of
        // the widths at which what a row holds changes
        for edge in [parts[0] + parts[1] + parts[2] + 2.0 * gap, parts[0] + parts[1] + gap, parts[1] + parts[2] + gap] {
            for tenth in -40..=40 {
                let room = edge + tenth as f32 / 10.0;
                h.size.x += room - (rect(&h, "filterbar").width() - 32.0);
                let mut heights = Vec::new();
                for _ in 0..12 {
                    h.step();
                    heights.push(rect(&h, "filterbar").height());
                }
                assert!(((rect(&h, "filterbar").width() - 32.0) - room).abs() < 0.05, "{language}: the bar has the room asked for");
                // Then within a few frames the bar is one height and stays it: it used to change
                // every frame in a band four points wide, and the grid under it jumped
                let rest: Vec<f32> = heights.iter().skip(6).copied().collect();
                assert!(rest.iter().all(|height| *height == rest[0]), "{language}: at {room} pt of room the bar is {heights:?} high frame by frame");
                // And nothing in it runs out of it
                let bar = rect(&h, "filterbar");
                for (id, control) in widgets(&h, "dropdown:filter-") {
                    assert!(control.right() <= bar.right() + 0.5, "{language}: at {room} pt {id} {control:?} runs out of {bar:?}");
                }
            }
        }
    }
}
