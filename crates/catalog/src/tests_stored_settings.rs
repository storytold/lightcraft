//! The stored form of develop settings ([`crate::stored`]), on its own: settings are stored as
//! their difference from the defaults and read back exactly. Nothing writes this form into a
//! library yet (catalog format 10 does); the tests name the `serialize_with` functions the way
//! the ops and photos will.

use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tests_stored::{base, edited, rich};
use crate::{HistoryStep, Version, stored};

/// What a record holds of develop settings, stored the way the format 10 records will.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Record {
    #[serde(serialize_with = "stored::settings", deserialize_with = "stored::read_settings")]
    develop: Arc<DevelopSettings>,
    #[serde(serialize_with = "stored::step")]
    step: HistoryStep,
    #[serde(serialize_with = "stored::history")]
    history: Vec<HistoryStep>,
    #[serde(serialize_with = "stored::versions")]
    versions: Vec<Version>,
}

fn record_of(develop: Arc<DevelopSettings>) -> Record {
    let history = vec![HistoryStep { label: "x".into(), settings: develop.clone() }];
    let versions = vec![Version { name: "V".into(), created: "2026-10-10".into(), settings: develop.clone(), auto: false }];
    let step = history[0].clone();
    Record { develop, step, history, versions }
}

fn stored_json(r: &Record) -> Value {
    serde_json::to_value(r).unwrap()
}

/// The record saved and read again, plainly and over the defaults a library states (the path a
/// library written with other defaults takes). The settings must read back with the same public
/// JSON, which tells apart what `==` doesn't (-0.0 and 0.0), and the same hash.
fn reopened(r: &Record) -> Record {
    let text = serde_json::to_string(r).unwrap();
    let back: Record = serde_json::from_str(&text).unwrap();
    {
        let _over = stored::reading_over(Some(Arc::new(stored::base().1.clone())));
        let over: Record = serde_json::from_str(&text).unwrap();
        assert_eq!(over, back, "read over the library's defaults");
    }
    for (a, b) in [
        (&r.develop, &back.develop),
        (&r.step.settings, &back.step.settings),
        (&r.history[0].settings, &back.history[0].settings),
        (&r.versions[0].settings, &back.versions[0].settings),
    ] {
        assert_eq!(serde_json::to_string(a.as_ref()).unwrap(), serde_json::to_string(b.as_ref()).unwrap());
        assert_eq!(a.hash64(), b.hash64());
    }
    back
}

/// Given settings that differ from the defaults in one value, when they are stored, then only
/// that value is stored, in the photo's, its history steps' and its versions' settings alike.
#[test]
fn develop_settings_are_stored_as_their_difference_from_the_defaults() {
    let r = record_of(edited(|s| s.light.exposure = 0.3));
    let exposure = json!({"light": {"exposure": 0.3}});
    let v = stored_json(&r);
    assert_eq!(v["develop"], exposure);
    assert_eq!(v["step"]["settings"], exposure);
    assert_eq!(v["history"][0]["settings"], exposure);
    assert_eq!(v["versions"][0]["settings"], exposure);
    assert_eq!(reopened(&r), r);
    // and untouched settings are `{}`
    assert_eq!(stored_json(&record_of(Arc::new(base())))["develop"], json!({}));
}

/// The rendering process is stored whenever it is not the first: a record without one is on the
/// first process in every build, which is not what the default settings are on.
#[test]
fn the_rendering_process_survives() {
    let on = |json: &str| -> Arc<DevelopSettings> { Arc::new(serde_json::from_str(json).unwrap()) };
    for json in ["{}", r#"{"process":1}"#, r#"{"process":7}"#, r#"{"process":7,"light":{"exposure":1.0}}"#] {
        let r = record_of(on(json));
        assert_eq!(reopened(&r), r, "{json}");
    }
    // new settings: the default ones on the default process, stating only what that needs
    let r = record_of(Arc::new(DevelopSettings::default()));
    let v = stored_json(&r);
    assert!(v["develop"].as_object().unwrap().keys().all(|k| k == "process"), "{}", v["develop"]);
    assert_eq!(reopened(&r), r);
}

/// A value that is read all or nothing is stored whole once any of it differs: a crop rectangle
/// with one edge moved keeps its other edges and its angle (left out one by one, the settings
/// would not read at all).
#[test]
fn a_value_read_all_or_nothing_is_stored_whole() {
    let mut v = serde_json::to_value(base()).unwrap();
    v["crop"]["geometry"]["rect"]["x0"] = json!(0.25);
    let r = record_of(Arc::new(serde_json::from_value(v).unwrap()));
    assert_eq!(stored_json(&r)["develop"], json!({"crop": {"geometry": {"angle": 0.0, "rect": {"x0": 0.25, "x1": 1.0, "y0": 0.0, "y1": 1.0}}}}));
    assert_eq!(reopened(&r), r);
}

/// Changing any one number, switch or text of the develop settings is stored and read back.
#[test]
fn every_develop_setting_survives_on_its_own() {
    fn leaves(v: &Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    path.push(k.clone());
                    leaves(x, path, out);
                    path.pop();
                }
            }
            _ => out.push(path.clone()),
        }
    }
    fn at<'a>(v: &'a mut Value, path: &[String]) -> &'a mut Value {
        path.iter().fold(v, |v, k| &mut v[k.as_str()])
    }
    let default = serde_json::to_value(DevelopSettings::default()).unwrap();
    let mut paths = Vec::new();
    leaves(&default, &mut Vec::new(), &mut paths);
    assert!(paths.len() > 100, "{}", paths.len());
    let mut tried = 0;
    for path in &paths {
        let mut changed = Vec::new();
        match at(&mut default.clone(), path).clone() {
            Value::Number(n) => {
                let x = n.as_f64().unwrap();
                changed.extend([json!(x + 1.0), json!(x - 1.0), json!(0), json!(0.25), json!(-0.0)]);
            }
            Value::Bool(b) => changed.push(Value::Bool(!b)),
            Value::String(s) => changed.push(Value::String(format!("{s}x"))),
            // lists and optional values: see `rich_develop_settings_survive`
            Value::Array(_) | Value::Null | Value::Object(_) => {}
        }
        for value in changed {
            let mut v = default.clone();
            *at(&mut v, path) = value;
            // values the settings don't accept there are not settings
            let Ok(s) = serde_json::from_value::<DevelopSettings>(v) else { continue };
            tried += 1;
            let r = record_of(Arc::new(s));
            assert_eq!(reopened(&r), r, "{}", path.join("."));
        }
    }
    assert!(tried > 150, "{tried}");
}

/// Settings with lists, choices and optional values (masks, curve points, guides, the Upright
/// transform, a locked crop aspect, spots, red eye) read back exactly.
#[test]
fn rich_develop_settings_survive() {
    let rich = Arc::new(rich());
    let v = serde_json::to_value(rich.as_ref()).unwrap();
    for list in ["masks", "spots", "red_eye", "point_colors", "disabled_sections"] {
        assert!(v[list].as_array().is_some_and(|a| !a.is_empty()), "`{list}`");
    }
    assert!(!v["geometry"]["upright_transform"].is_null() && !v["crop"]["aspect"].is_null());
    let r = record_of(rich);
    assert_eq!(reopened(&r), r);
}

/// Pen pressure is an `f32`: a stored brush stroke writes it with the digits of an `f32` (0.3),
/// not the 17 of the `f64` it widens to (0.30000001192092896), which would be larger than
/// the record was before.
#[test]
fn a_pen_pressure_is_stored_with_the_digits_it_has() {
    use lightcraft_develop::{BrushStroke, Mask, MaskComponent, MaskOp, MaskShape};
    let stroke = BrushStroke { points: vec![lightcraft_geom::Point { x: 0.5, y: 0.5 }], pressure: vec![0.3], ..Default::default() };
    let shape = MaskShape::Brush { strokes: vec![stroke] };
    let component = MaskComponent { name: None, op: MaskOp::default(), invert: false, shape };
    let r = record_of(edited(|s| s.masks = vec![Mask { id: 1, name: "Brush".into(), components: vec![component], ..Default::default() }]));
    let text = serde_json::to_string(&r).unwrap();
    assert!(text.contains(r#""pressure":[0.3]"#), "{text}");
    assert_eq!(reopened(&r), r);
}

/// A negative zero is a different number in the saved JSON (a camera's as-shot tint of -0.3
/// rounds to it), and a photo's fingerprint and preview-cache key are made from that JSON: it is
/// stored, also where everything else is the default, and settings that equal it but for the
/// sign of a zero are not taken for it.
#[test]
fn a_negative_zero_survives() {
    let r = record_of(edited(|s| s.wb.tint = -0.0));
    assert_ne!(r.develop.hash64(), base().hash64());
    assert_eq!(stored_json(&r)["develop"].to_string(), r#"{"wb":{"tint":-0.0}}"#);
    assert_eq!(reopened(&r), r);
    let positive = record_of(edited(|s| s.wb.tint = 0.0));
    assert_eq!(stored_json(&positive)["develop"], json!({}));
    assert_ne!(reopened(&r).develop.hash64(), reopened(&positive).develop.hash64());
}

/// While a library's header doesn't state every setting this build has, settings are stored
/// whole (as formats up to 9 did), and only on the thread that asked.
#[test]
fn settings_are_stored_whole_while_asked_to() {
    let r = record_of(edited(|s| s.light.exposure = 0.3));
    let whole = {
        let _full = stored::writing_full();
        // another thread is not affected
        assert_eq!(
            std::thread::spawn({
                let r = r.clone();
                move || stored_json(&r)["develop"].clone()
            })
            .join()
            .unwrap(),
            json!({"light": {"exposure": 0.3}})
        );
        stored_json(&r)
    };
    assert_eq!(whole["develop"], serde_json::to_value(r.develop.as_ref()).unwrap());
    assert_eq!(whole["history"][0]["settings"], whole["develop"]);
    assert_eq!(reopened(&r), r);
    // and compact again once the guard is gone
    assert_eq!(stored_json(&r)["develop"], json!({"light": {"exposure": 0.3}}));
}

/// A library states every setting this build has when its stated defaults have every one of
/// this build's; one that lacks a setting does not.
#[test]
fn a_library_states_every_setting_or_does_not() {
    let ours = stored::base().1.clone();
    assert!(stored::states_every_setting(&ours));
    let mut fewer = ours.clone();
    fewer["light"].as_object_mut().unwrap().remove("exposure");
    assert!(!stored::states_every_setting(&fewer));
    let mut more = ours;
    more["a_setting_a_later_build_has"] = json!(1);
    assert!(stored::states_every_setting(&more));
}
