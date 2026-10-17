//! Colour readout under the histogram: the colour under the pointer in the loupe, as RGB %,
//! HSL or Lab (`view.colorReadout`), and the histogram's context menu that picks the format.

use std::time::Duration;

use serde_json::json;

use crate::color_readout::ColorReadout;
use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn hover_photo(h: &mut Headless) {
    let r = h.request("ui.hoverWidget", json!({"id": "canvas:image"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn readout(h: &Headless) -> Option<String> {
    h.painted_text().into_iter().find(|t| t.starts_with("R ") || t.starts_with("H ") || t.starts_with("L "))
}

#[test]
fn hovering_the_photo_shows_its_colour_in_the_chosen_format() {
    let mut h = demo();
    assert!(readout(&h).is_none(), "no readout before the pointer is on the photo");
    hover_photo(&mut h);
    let rgb = readout(&h).expect("RGB readout while hovering");
    assert!(rgb.ends_with('%') && rgb.contains(" G ") && rgb.contains(" B "), "{rgb}");

    for (id, want) in [("view.colorReadoutLab", ColorReadout::Lab), ("view.colorReadoutHsl", ColorReadout::Hsl)] {
        h.app.run(id, json!({})).unwrap();
        assert_eq!(h.app.ui.settings.color_readout, want);
        hover_photo(&mut h);
        let text = readout(&h).expect("readout");
        match want {
            ColorReadout::Lab => assert!(text.starts_with("L ") && text.contains(" a ") && text.contains(" b "), "{text}"),
            _ => assert!(text.starts_with("H ") && text.contains('°'), "{text}"),
        }
    }
    // the pointer off the photo: the EXIF strip is back
    h.request("ui.move", json!({"x": 2.0, "y": 2.0}), T);
    h.step();
    h.step();
    assert!(readout(&h).is_none(), "{:?}", h.painted_text());
}

#[test]
fn the_readout_command_cycles_and_rejects_unknown_formats() {
    let mut h = demo();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let r = h.app.run("view.colorReadout", json!({})).unwrap();
        seen.push(r["format"].as_str().unwrap().to_string());
    }
    assert_eq!(seen, ["hsl", "lab", "rgb"]);
    assert_eq!(h.app.run("view.colorReadout", json!({"format": "LAB"})).unwrap()["format"], "lab");
    for bad in [json!({"format": "cmyk"}), json!({"format": 3})] {
        assert!(h.app.run("view.colorReadout", bad).is_err());
    }
    assert_eq!(h.app.ui.settings.color_readout, ColorReadout::Lab, "a rejected call changes nothing");
    // persisted with the settings
    let saved = serde_json::to_value(&h.app.ui.settings).unwrap();
    assert_eq!(saved["colorReadout"], "lab");
}
