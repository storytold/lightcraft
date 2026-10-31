//! The Color Mixer laid out as in Lightroom Classic (item 13 of the evening retest): "Mixer |
//! Point Color" tabs, Adjust HSL / Color, and in HSL the Hue | Saturation | Luminance | All groups
//! of eight colour sliders, each with its targeted adjustment. HSL is the default and the choice
//! is remembered; the sliders are the same `mixer.*` controls.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const BANDS: [&str; 8] = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];

fn mixer() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1300.0, 2600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    run(&mut h, "view.colorMixer", json!({}));
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn sliders(h: &Headless, part: &str) -> usize {
    BANDS.iter().filter(|b| has(h, &format!("slider:mixer.{b}.{part}"))).count()
}

#[test]
fn hsl_is_the_default_with_eight_sliders_per_group() {
    let mut h = mixer();
    for id in ["button:mixerTab-mixer", "button:mixerTab-point", "button:mixerAdjust-hsl", "button:mixerAdjust-color"] {
        assert!(has(&h, id), "{id}");
    }
    for g in ["hue", "sat", "lum", "all"] {
        assert!(has(&h, &format!("button:mixerGroup-{g}")), "{g}");
    }
    assert_eq!((h.app.ui.mixer_adjust.as_str(), h.app.ui.mixer_group.as_str()), ("hsl", "hue"), "HSL, Hue first");
    assert_eq!((sliders(&h, "hue"), sliders(&h, "sat"), sliders(&h, "lum")), (8, 0, 0), "eight Hue sliders");
    assert!(has(&h, "icon:tatMixer-hue"), "with its targeted adjustment");
    click(&mut h, "button:mixerGroup-sat");
    assert_eq!((sliders(&h, "hue"), sliders(&h, "sat"), sliders(&h, "lum")), (0, 8, 0));
    assert!(has(&h, "icon:tatMixer-sat"));
    click(&mut h, "button:mixerGroup-lum");
    assert_eq!((sliders(&h, "hue"), sliders(&h, "sat"), sliders(&h, "lum")), (0, 0, 8));
    // All: the three groups stacked, each with a heading and its targeted adjustment
    click(&mut h, "button:mixerGroup-all");
    assert_eq!((sliders(&h, "hue"), sliders(&h, "sat"), sliders(&h, "lum")), (8, 8, 8));
    let at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top()).unwrap_or_else(|| panic!("no {id}"));
    for g in ["hue", "sat", "lum"] {
        assert!(has(&h, &format!("mixerHeading:{g}")) && has(&h, &format!("icon:tatMixer-{g}")), "{g}");
    }
    assert!(at(&h, "mixerHeading:hue") < at(&h, "slider:mixer.red.hue"));
    assert!(at(&h, "slider:mixer.magenta.hue") < at(&h, "mixerHeading:sat"));
    assert!(at(&h, "slider:mixer.magenta.sat") < at(&h, "mixerHeading:lum"));
    // a group's targeted adjustment arms its tool
    click(&mut h, "icon:tatMixer-sat");
    assert_eq!(h.app.ui.tool, "tat:sat");
}

#[test]
fn the_sliders_are_the_mixer_controls() {
    let mut h = mixer();
    run(&mut h, "develop.set", json!({"control": "mixer.orange.hue", "value": -30}));
    let id = h.app.session.active().expect("a photo");
    assert_eq!(h.app.session.develop_of(id).expect("settings").mixer.orange.hue, -30.0);
    // the same value in Color mode, Orange
    run(&mut h, "view.colorMixer", json!({"adjust": "color", "band": "orange"}));
    h.settle(SETTLE);
    assert!(has(&h, "mixerBand:orange"), "the colour dots");
    assert_eq!((sliders(&h, "hue"), sliders(&h, "sat"), sliders(&h, "lum")), (1, 1, 1), "one colour's three sliders");
    assert!(has(&h, "slider:mixer.orange.hue"));
}

#[test]
fn the_choice_is_remembered_and_point_color_is_a_tab() {
    let mut h = mixer();
    click(&mut h, "button:mixerAdjust-color");
    assert_eq!(h.app.ui.mixer_adjust, "color");
    assert!(has(&h, "mixerBand:red"));
    // kept with the UI settings (a restart)
    let saved = serde_json::to_value(&h.app.ui).expect("ui state");
    let back: crate::UiState = serde_json::from_value(saved).expect("reads back");
    assert_eq!(back.mixer_adjust, "color");
    // older settings without it start in HSL
    let old: crate::UiState = serde_json::from_value(json!({})).expect("defaults");
    assert_eq!((old.mixer_tab.as_str(), old.mixer_adjust.as_str(), old.mixer_group.as_str()), ("mixer", "hsl", "hue"));
    // Point Color is the second tab
    click(&mut h, "button:mixerTab-point");
    assert!(has(&h, "icon:pointColorPicker"), "Point Color's eyedropper");
    assert_eq!(sliders(&h, "hue") + sliders(&h, "sat") + sliders(&h, "lum"), 0);
    // the command checks what it is given
    let r = h.request("engine.execute", json!({"command": "view.colorMixer", "params": {"group": "chroma"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// A Color Mixer targeted adjustment whose button the layout no longer shows is put down (it would
/// change a hidden group); the command leaves full-screen preview, where the mixer can't be seen.
#[test]
fn a_hidden_groups_targeted_adjustment_is_put_down_and_the_command_shows_the_mixer() {
    let mut h = mixer();
    click(&mut h, "icon:tatMixer-hue");
    assert_eq!(h.app.ui.tool, "tat:hue");
    click(&mut h, "button:mixerGroup-all");
    assert_eq!(h.app.ui.tool, "tat:hue", "All shows the Hue group's button too");
    click(&mut h, "button:mixerGroup-sat");
    assert_eq!(h.app.ui.tool, "", "Saturation hides Hue's");
    click(&mut h, "icon:tatMixer-sat");
    run(&mut h, "view.colorMixer", json!({"adjust": "color"}));
    assert_eq!(h.app.ui.tool, "tat:sat", "Color mode shows all three");
    run(&mut h, "view.colorMixer", json!({"tab": "point"}));
    assert_eq!(h.app.ui.tool, "", "Point Color shows none");
    h.app.ui.fullscreen = true;
    run(&mut h, "view.colorMixer", json!({"tab": "mixer"}));
    assert!(!h.app.ui.fullscreen);
}

/// A B&W photo's mixer shows its B&W mix: Point Color's range view can't stay on the photo.
#[test]
fn point_colors_range_view_is_off_for_a_bw_photo() {
    let mut h = mixer();
    run(&mut h, "view.colorMixer", json!({"tab": "point"}));
    h.app.ui.point_color_visualize = true;
    h.app.ui.point_color = 0;
    let id = h.app.session.active().expect("a photo");
    let mut d = (*h.app.session.develop_of(id).expect("settings")).clone();
    d.point_colors.push(Default::default());
    assert!(matches!(crate::panels::detail::view_overlay(&h.app, &d), lightcraft_pipeline::Overlay::PointColorRange(0)));
    d.treatment = lightcraft_develop::Treatment::Bw;
    assert!(!matches!(crate::panels::detail::view_overlay(&h.app, &d), lightcraft_pipeline::Overlay::PointColorRange(_)));
}

/// A B&W photo's mixer always shows its Luminance targeted adjustment: the command keeps it.
#[test]
fn a_bw_photos_luminance_targeted_adjustment_stays() {
    let mut h = mixer();
    run(&mut h, "develop.treatment", json!({"bw": true}));
    h.app.ui.tool = "tat:lum".into();
    run(&mut h, "view.colorMixer", json!({}));
    assert_eq!(h.app.ui.tool, "tat:lum");
    h.app.ui.tool = "tat:hue".into();
    run(&mut h, "view.colorMixer", json!({}));
    assert_eq!(h.app.ui.tool, "", "no Hue on a B&W photo");
}

/// Point Color's picker is put down when its tab is left, so a click on the photo picks no colour.
#[test]
fn point_colors_picker_is_put_down_when_its_tab_is_left() {
    let mut h = mixer();
    run(&mut h, "view.colorMixer", json!({"tab": "point"}));
    h.app.ui.tool = "pointColor".into();
    run(&mut h, "view.colorMixer", json!({}));
    assert_eq!(h.app.ui.tool, "pointColor", "still on its tab");
    run(&mut h, "view.colorMixer", json!({"tab": "mixer"}));
    assert_eq!(h.app.ui.tool, "", "the Mixer tab puts it down");
}

/// On a B&W photo the command says the layout shown, its B&W mix, not Point Color: the choice of
/// tab is kept for colour photos.
#[test]
fn a_bw_photo_reports_the_mixer_it_shows() {
    let mut h = mixer();
    run(&mut h, "develop.treatment", json!({"bw": true}));
    let r = run(&mut h, "view.colorMixer", json!({"tab": "point"}));
    assert_eq!(r["tab"], "mixer", "{r}");
    assert_eq!(h.app.ui.mixer_tab, "point", "kept for colour photos");
}
