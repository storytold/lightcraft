//! Convert Negative through the command layer: switching it on measures the scan, the preset and
//! choices are validated, the sliders are ordinary develop controls, the picker stores the film
//! base and the gray point, and copy / paste carries the conversion to the rest of a roll.

use serde_json::json;

use crate::Session;

fn negative(s: &Session) -> lightcraft_develop::Negative {
    s.develop_of(s.active().unwrap()).unwrap().negative.clone()
}

#[test]
fn switching_on_measures_and_settings_follow() {
    let mut s = Session::with_demo();
    assert!(!negative(&s).enabled);
    let r = s.execute("develop.negative", &json!({})).unwrap();
    assert_eq!(r["enabled"], true, "{r}");
    assert_eq!(r["presetName"], "Standard");
    let n = negative(&s);
    let m = n.measured.expect("measured when switched on");
    assert!((0..3).all(|ch| m.white[ch] < m.black[ch]), "{m:?}");
    // a preset by any spelling, the advanced choices by name ("" = the preset's)
    s.execute("develop.negative", &json!({"preset": "portra400", "advanced": true, "tone": "cinematic", "look": "", "wb": "warm"})).unwrap();
    let n = negative(&s);
    assert_eq!((n.preset.as_str(), n.advanced, n.tone.as_str(), n.wb.as_str()), ("Portra 400", true, "cinematic", "warm"));
    assert_eq!(n.measured, Some(m), "not measured again unless asked");
    // the sliders are develop controls
    s.execute("develop.set", &json!({"control": "negative.exposure", "value": 9})).unwrap();
    assert_eq!(negative(&s).exposure, 5.0, "clamped by its spec");
    // off keeps everything, so on again is the same conversion
    s.execute("develop.negative", &json!({"on": false})).unwrap();
    assert!(!negative(&s).enabled && negative(&s).measured == Some(m));
    // one undo step per change
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(negative(&s).enabled);
}

#[test]
fn bad_arguments_are_errors() {
    let mut s = Session::with_demo();
    for p in [
        json!({"preset": "Kodachrome 25"}),
        json!({"preset": 3}),
        json!({"tone": "spicy"}),
        json!({"look": 1}),
        json!({"border": 90}),
        json!({"border": "x"}),
    ] {
        assert!(s.execute("develop.negative", &p).is_err(), "{p}");
    }
    for p in [json!({}), json!({"x": 0.5}), json!({"x": 0.5, "y": 0.5, "target": "sky"})] {
        assert!(s.execute("develop.negativePick", &p).is_err(), "{p}");
    }
    assert!(!negative(&s).enabled, "nothing changed");
}

#[test]
fn the_picker_stores_the_base_and_the_gray_point() {
    let mut s = Session::with_demo();
    let r = s.execute("develop.negativePick", &json!({"x": 0.01, "y": 0.01})).unwrap();
    assert_eq!((r["enabled"].as_bool(), r["advanced"].as_bool()), (Some(true), Some(true)), "{r}");
    let n = negative(&s);
    let base = n.base.expect("a base");
    assert!(base.iter().all(|v| (0.0..=1.0).contains(v)), "{base:?}");
    assert!(n.measured.is_some());
    s.execute("develop.negativePick", &json!({"x": 2.0, "y": -1.0, "target": "gray"})).unwrap();
    assert!(negative(&s).gray.is_some(), "points outside the photo clamp to its edge");
    s.execute("develop.negative", &json!({"clearBase": true, "clearGray": true})).unwrap();
    let n = negative(&s);
    assert!(n.base.is_none() && n.gray.is_none());
}

#[test]
fn copy_and_paste_carries_the_conversion_to_another_frame() {
    let mut s = Session::with_demo();
    s.execute("develop.negative", &json!({"preset": "Gold 200"})).unwrap();
    let first = negative(&s);
    s.execute("develop.copy", &json!({"groups": ["negative"]})).unwrap();
    let ids: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    s.execute("library.select", &json!({"ids": [ids[1]], "active": ids[1]})).unwrap();
    assert!(!negative(&s).enabled);
    s.execute("develop.paste", &json!({})).unwrap();
    assert_eq!(negative(&s), first, "the roll converts with the first frame's measurement");
}

#[test]
fn booleans_must_be_booleans_and_atomic_on_error() {
    let mut s = Session::with_demo();
    for p in [json!({"on": "false"}), json!({"advanced": 1}), json!({"measure": "yes"}), json!({"clearBase": 0}), json!({"clearGray": "x"})] {
        assert!(s.execute("develop.negative", &p).is_err(), "{p}");
    }
    // a valid preset next to an invalid tone changes nothing
    assert!(s.execute("develop.negative", &json!({"preset": "Gold 200", "tone": "spicy"})).is_err());
    assert_eq!(negative(&s), lightcraft_develop::Negative::default());
    for p in [json!({"target": 123}), json!({"x": 0.5, "y": 0.5, "target": 7})] {
        assert!(s.execute("develop.negativePick", &p).is_err(), "{p}");
    }
    assert_eq!(negative(&s), lightcraft_develop::Negative::default());
}

#[test]
fn off_changes_the_choices_and_can_still_measure() {
    let mut s = Session::with_demo();
    s.execute("develop.negative", &json!({"on": false, "preset": "Gold 200"})).unwrap();
    let n = negative(&s);
    assert!(!n.enabled && n.measured.is_none());
    assert_eq!(n.preset, "Gold 200");
    let r = s.execute("develop.negative", &json!({"on": false, "measure": true, "border": 20})).unwrap();
    assert!(!negative(&s).enabled && negative(&s).measured.is_some());
    assert_eq!(r["border"], json!(20.0));
}

#[test]
fn the_border_is_validated_and_changes_the_measurement() {
    let mut s = Session::with_demo();
    for b in [json!(-1), json!(46), json!("8")] {
        assert!(s.execute("develop.negative", &json!({"border": b})).is_err(), "{b}");
    }
    s.execute("develop.negative", &json!({"border": 0})).unwrap();
    let none = negative(&s).measured.unwrap();
    s.execute("develop.negative", &json!({"measure": true, "border": 40})).unwrap();
    let wide = negative(&s).measured.unwrap();
    assert_ne!(none, wide, "a different border measures a different area");
    s.execute("develop.negative", &json!({"measure": true, "border": 45})).unwrap();
}

#[test]
fn the_measurement_is_taken_inside_the_crop_and_orientation() {
    let mut s = Session::with_demo();
    s.execute("develop.negative", &json!({"on": false, "measure": true})).unwrap();
    let whole = negative(&s).measured.unwrap();
    s.execute("crop.set", &json!({"rect": [0.5, 0.5, 1.0, 1.0]})).unwrap();
    s.execute("develop.negative", &json!({"on": false, "measure": true})).unwrap();
    let cropped = negative(&s).measured.unwrap();
    assert_ne!(whole, cropped, "the measurement follows the crop");
    assert!(cropped.base.iter().chain(&cropped.black).chain(&cropped.white).all(|v| v.is_finite()));
}

#[test]
fn a_second_pick_keeps_the_measurement_and_each_pick_is_one_undo_step() {
    let mut s = Session::with_demo();
    s.execute("develop.negativePick", &json!({"x": 0.01, "y": 0.01})).unwrap();
    let m = negative(&s).measured;
    s.execute("develop.negativePick", &json!({"x": 0.5, "y": 0.5, "target": "gray"})).unwrap();
    assert_eq!(negative(&s).measured, m);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(negative(&s).gray.is_none() && negative(&s).base.is_some());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!negative(&s).enabled);
}

#[test]
fn without_a_photo_the_commands_fail_and_change_nothing() {
    let mut s = Session::new();
    assert!(s.execute("develop.negative", &json!({})).is_err());
    assert!(s.execute("develop.negativePick", &json!({"x": 0.5, "y": 0.5})).is_err());
}

#[test]
fn the_default_copy_carries_the_conversion_and_its_measurement() {
    let mut s = Session::with_demo();
    s.execute("develop.negativePick", &json!({"x": 0.01, "y": 0.01})).unwrap();
    s.execute("develop.negative", &json!({"preset": "Gold 200"})).unwrap();
    s.execute("develop.copy", &json!({})).unwrap();
    let ids: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    s.execute("library.select", &json!({"ids": [ids[1]], "active": ids[1]})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    let n = negative(&s);
    assert!(n.enabled && n.preset == "Gold 200");
    assert!(n.base.is_some() && n.measured.is_some(), "the roll converts with the first frame's measurement: {n:?}");
}

/// The automatic adjustments read the positive: a converted negative gives other answers than the
/// same pixels left as they are.
#[test]
fn the_auto_commands_read_the_positive() {
    let run = |on: bool| {
        let mut s = Session::with_demo();
        if on {
            s.execute("develop.negative", &json!({})).unwrap();
        }
        let wb = s.execute("develop.wb", &json!({"mode": "auto"})).unwrap();
        let pick = s.execute("develop.wbPick", &json!({"x": 0.5, "y": 0.5})).unwrap();
        s.execute("develop.autoBwMix", &json!({})).unwrap();
        let bw = s.develop_of(s.active().unwrap()).unwrap().bw_mix;
        s.execute("develop.auto", &json!({})).unwrap();
        let d = s.develop_of(s.active().unwrap()).unwrap();
        (wb, pick, format!("{bw:?}"), format!("{:?}", d.light))
    };
    let (raw, positive) = (run(false), run(true));
    assert_ne!(raw.0, positive.0, "auto white balance");
    assert_ne!(raw.1, positive.1, "white balance selector");
    assert_ne!(raw.2, positive.2, "auto B&W mix");
    assert_ne!(raw.3, positive.3, "auto");
}

/// A recipe that resets a Convert Negative slider to its default is a recipe, as for any slider
/// (the conversion is left out of stored settings at its defaults).
#[test]
fn a_recipe_can_reset_a_negative_slider() {
    let mut s = Session::with_demo();
    let r = s.execute("preset.saveRecipe", &json!({"name": "Neg reset", "recipe": [{"id": "negative.exposure", "value": 0}]}));
    let id = r.expect("saved")["id"].as_str().unwrap().to_string();
    let p = s.presets.iter().find(|p| p.id == id).unwrap();
    assert_eq!(p.settings, json!({"negative": {"exposure": 0.0}}));
}
