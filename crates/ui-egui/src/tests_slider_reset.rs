//! Headless tests of resetting a slider (issue #1436): double-clicking its name or its thumb puts it
//! back to its default in one undo step, as in Lightroom, and resting the pointer on either says so.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
/// The sliders the issue names, each set away from its default.
const EDITED: [(&str, f64); 3] = [("light.exposure", 1.25), ("light.contrast", 40.0), ("light.brightness", 40.0)];

/// The demo library in Detail with the Edit panel's Light section open.
fn edit_panel() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T)["ok"], true);
    assert_eq!(h.request("ui.set", json!({"view": "detail", "right": "edit"}), T)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string()];
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn value(h: &Headless, control: &str) -> f64 {
    let id = h.app.session.selection.active.unwrap();
    lcc_develop::controls::get(&h.app.session.develop_of(id).unwrap_or_default(), control).unwrap()
}

fn default(control: &str) -> f64 {
    lcc_develop::controls::find(control).unwrap().default
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn set(h: &mut Headless, control: &str, value: f64) {
    ask(h, "engine.execute", json!({"command": "develop.set", "params": {"control": control, "value": value}}));
}

/// The thumb's place along the track (0..1), plus `dx` points: a hand lands near its centre, not on it.
fn thumb_fx(h: &Headless, control: &str, dx: f32) -> f32 {
    let spec = lcc_develop::controls::find(control).unwrap();
    let track = widget(h, &format!("slider:{control}"));
    spec.position(value(h, control)) as f32 + dx / track.width()
}

/// A double-click on a widget: the control channel plays each press and release in a frame of its
/// own, so the slider sees the first click before the second, as with a hand on a mouse.
fn double_click(h: &mut Headless, id: &str, fx: f32) {
    ask(h, "ui.clickWidget", json!({"id": id, "count": 2, "fx": fx}));
}

#[test]
fn double_clicking_a_sliders_name_or_thumb_resets_it_in_one_step() {
    let mut h = edit_panel();
    for (control, edited) in EDITED {
        set(&mut h, control, edited);
        let undo = h.app.session.undo.len();
        double_click(&mut h, &format!("sliderLabel:{control}"), 0.1);
        assert_eq!(value(&h, control), default(control), "{control} is back at its default");
        assert_eq!(h.app.session.undo.len() - undo, 1, "{control}: one undo step");
        ask(&mut h, "engine.execute", json!({"command": "edit.undo"}));
        assert_eq!(value(&h, control), edited, "{control}: undo brings the edit back");
    }
    // the thumb: the first click of the double-click lands a few points off its centre and leaves the value alone
    // (it used to move it there, a history step of its own before the reset)
    for (control, edited) in EDITED {
        set(&mut h, control, edited);
        let undo = h.app.session.undo.len();
        let fx = thumb_fx(&h, control, 4.0);
        double_click(&mut h, &format!("slider:{control}"), fx);
        assert_eq!(value(&h, control), default(control), "{control} is back at its default");
        assert_eq!(h.app.session.undo.len() - undo, 1, "{control}: one undo step");
        ask(&mut h, "engine.execute", json!({"command": "edit.undo"}));
        assert_eq!(value(&h, control), edited, "{control}: undo brings the edit back");
    }
    // a click on the thumb leaves the value alone; a click elsewhere on the track still moves it there
    set(&mut h, "light.exposure", 1.25);
    let fx = thumb_fx(&h, "light.exposure", -4.0);
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:light.exposure", "fx": fx}));
    assert_eq!(value(&h, "light.exposure"), 1.25);
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:light.exposure", "fx": 0.25}));
    assert!((value(&h, "light.exposure") - -2.5).abs() < 0.02, "{}", value(&h, "light.exposure"));
}

/// Typing a value (KEY-SLIDER-TYPE, #1362) still works next to the reset.
#[test]
fn a_typed_value_still_applies_after_a_reset() {
    let mut h = edit_panel();
    set(&mut h, "light.exposure", 1.25);
    double_click(&mut h, "sliderLabel:light.exposure", 0.1);
    assert_eq!(value(&h, "light.exposure"), 0.0);
    let undo = h.app.session.undo.len();
    ask(&mut h, "ui.clickWidget", json!({"id": "sliderValue:light.exposure"}));
    for (method, params) in [("ui.text", json!({"text": "0.5"})), ("ui.key", json!({"key": "Enter"}))] {
        ask(&mut h, method, params);
    }
    assert_eq!(value(&h, "light.exposure"), 0.5);
    assert_eq!(h.app.session.undo.len() - undo, 1, "one undo step");
}

/// After a click on a slider, Left / Right still move to the next and previous photo (#880), while
/// Up / Down keep nudging the selected slider.
#[test]
fn arrows_still_change_the_photo_after_a_slider_edit() {
    let mut h = edit_panel();
    let first = h.app.session.selection.active.unwrap();
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:light.exposure", "fx": 0.75}));
    assert!(crate::widgets::slider_has_focus(&h.view.ctx), "the click selects the slider");
    let before = value(&h, "light.exposure");
    ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
    assert!(value(&h, "light.exposure") > before, "Up still nudges the selected slider");
    ask(&mut h, "ui.key", json!({"key": "ArrowRight"}));
    let next = h.app.session.selection.active.unwrap();
    assert_ne!(next, first, "Right shows the next photo after a develop edit");
    ask(&mut h, "ui.key", json!({"key": "ArrowLeft"}));
    assert_eq!(h.app.session.selection.active.unwrap(), first, "Left goes back");
}

/// A raw starts with sharpening 40 and colour noise reduction 25 (issue #1244): resetting a slider by its name goes
/// back to the photo's own default, not the spec table's neutral 0, for every control that starts elsewhere.
#[test]
fn a_reset_goes_to_the_photos_own_default_for_every_control() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let id = lcc_catalog::PhotoId(app.session.visible_cloned()[0].0);
    let mut raw = (*app.session.catalog.photo(id).unwrap().clone()).clone();
    let id = lcc_catalog::PhotoId(9_000_001);
    raw.id = id;
    raw.kind = lcc_catalog::MediaKind::Raw;
    raw.preview_only = None;
    app.session.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(raw.clone()) }).unwrap();
    app.session.selection.active = Some(id);
    let defaults = raw.camera_defaults();
    let mut nonzero = Vec::new();
    for spec in lcc_develop::controls::CONTROLS {
        let Some(own) = lcc_develop::controls::get(&defaults, spec.id) else { continue };
        let reset = crate::panels::edit::photo_default_spec(&app, spec).map_or(spec.default, |s| s.default);
        assert!((reset - own.clamp(spec.min, spec.max)).abs() < 1e-9, "{}: resets to {reset}, the photo starts at {own}", spec.id);
        if own != spec.default {
            nonzero.push(spec.id);
        }
    }
    assert!(nonzero.contains(&"detail.sharpenAmount"), "{nonzero:?}");
    // the Light sliders, Brightness among them, start at the neutral 0 on a raw as on any photo: their reset is the spec table's
    for id in ["light.contrast", "light.brightness"] {
        let spec = lcc_develop::controls::CONTROLS.iter().find(|s| s.id == id).expect(id);
        assert_eq!(lcc_develop::controls::get(&defaults, id), Some(0.0), "{id}: the photo's own default");
        assert!(!nonzero.contains(&id), "{id}: {nonzero:?}");
        assert!(crate::panels::edit::photo_default_spec(&app, spec).is_none(), "{id}: no photo-specific default");
    }
}

/// Brightness has its row right after Contrast in the Basic section and moves like its neighbours: Up / Down nudge it by
/// one, Shift + Up / Down by five (a click on the slider selects it first).
#[test]
fn the_brightness_row_follows_contrast_and_steps_like_its_neighbours() {
    let mut h = edit_panel();
    let (contrast, brightness, highlights) =
        (widget(&h, "slider:light.contrast"), widget(&h, "slider:light.brightness"), widget(&h, "slider:light.highlights"));
    assert!(contrast.top() < brightness.top() && brightness.top() < highlights.top(), "{contrast:?} {brightness:?} {highlights:?}");
    assert_eq!(value(&h, "light.brightness"), 0.0);
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:light.brightness", "fx": 0.75}));
    let start = value(&h, "light.brightness");
    assert!(start > 0.0, "the click set it ({start})");
    ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
    assert_eq!(value(&h, "light.brightness"), start + 1.0, "Up: one step");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    assert_eq!(value(&h, "light.brightness"), start, "Down: back");
    ask(&mut h, "ui.key", json!({"key": "ArrowUp", "shift": true}));
    assert_eq!(value(&h, "light.brightness"), start + 5.0, "Shift + Up: five steps");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown", "shift": true}));
    assert_eq!(value(&h, "light.brightness"), start, "Shift + Down: back");
    // the other Light sliders were left alone
    assert_eq!(value(&h, "light.contrast"), 0.0);
}
