//! Headless tests of a mask's Color Mixer flyout (LR-MASK-MIXER): its colours, sliders, Reset and targeted
//! adjustment edit the selected mask's mixer, not the photo's.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view with the Masking panel on a new radial mask (selected) and its Color Mixer flyout open.
fn mask_mixer_open() -> (Headless, u32) {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 2600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "panel.masking", json!({}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    let mask = h.app.session.active_mask.expect("the new mask is selected");
    h.step();
    h.step();
    click_widget(&mut h, "flyout:maskMixer", 1);
    h.step();
    (h, mask)
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn click_widget(h: &mut Headless, id: &str, count: u64) {
    let r = h.request("ui.clickWidget", json!({"id": id, "count": count}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn mask_mixer(h: &Headless, id: u32) -> lightcraft_develop::Mixer {
    develop(h).masks.iter().find(|m| m.id == id).expect("the mask").mixer
}

fn widgets(h: &mut Headless, filter: &str) -> Vec<String> {
    let r = h.request("ui.widgets", json!({"filter": filter}), T);
    r["result"].as_array().map(|a| a.iter().filter_map(|w| w["id"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

#[test]
fn a_masks_color_mixer_edits_that_masks_mixer_only() {
    let (mut h, mask) = mask_mixer_open();
    assert!(h.app.ui.flyout_open("maskMixer"), "the Color Mixer flyout opens");
    let ids = widgets(&mut h, "maskMixer");
    for b in lightcraft_develop::MIXER_BANDS {
        assert!(ids.iter().any(|x| *x == format!("maskMixerBand:{b}")), "{b} on screen: {ids:?}");
    }
    // a colour, then its sliders: a drag is one undo step on the mask's mixer
    click_widget(&mut h, "maskMixerBand:yellow", 1);
    assert_eq!(h.app.ui.mask_mixer_band, "yellow");
    let ids = widgets(&mut h, "slider:maskMixer");
    for k in ["hue", "sat", "lum"] {
        assert!(ids.iter().any(|x| *x == format!("slider:maskMixer.yellow.{k}")), "yellow {k} slider: {ids:?}");
    }
    let undo = h.app.session.undo.len();
    let r = h.request("ui.dragWidget", json!({"id": "slider:maskMixer.yellow.hue", "dx": -40.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let m = mask_mixer(&h, mask);
    assert!(m.yellow.hue < -1.0, "the drag moved the mask's yellow hue: {m:?}");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step per drag");
    assert!(develop(&h).mixer.is_neutral(), "the photo's Color Mixer is untouched");
    assert!(!h.app.ui.mask_overlay, "a mask mixer edit hides the overlay, as a mask slider does");
    // Reset shows while anything is set, and sets every band back
    click_widget(&mut h, "button:maskMixerReset", 1);
    assert!(mask_mixer(&h, mask).is_neutral());
}

#[test]
fn the_masks_targeted_adjustment_moves_the_colour_under_the_pointer() {
    let (mut h, mask) = mask_mixer_open();
    click_widget(&mut h, "button:maskMixerTargeted-sat", 1);
    assert_eq!(h.app.ui.tool, "maskMixer:sat");
    let img = h.app.image_rect.expect("the photo on screen");
    let (x, y) = (img.center().x, img.center().y);
    let undo = h.app.session.undo.len();
    let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y - 60.0, "steps": 10}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let m = mask_mixer(&h, mask);
    assert!(!m.is_neutral(), "the drag moved the mask's mixer: {m:?}");
    assert!(m.bands().iter().all(|b| b.hue == 0.0 && b.lum == 0.0), "Saturation only: {m:?}");
    assert!(m.bands().iter().any(|b| b.sat > 0.0), "raised: {m:?}");
    assert!(develop(&h).mixer.is_neutral(), "the photo's Color Mixer is untouched");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step per drag");
}
