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
