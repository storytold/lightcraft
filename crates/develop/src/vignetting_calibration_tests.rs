use super::*;
use crate::DevelopSettings;
use crate::presets::{SettingsPick, apply_partial};
use serde_json::json;

fn map(ev: f64, window: Option<[usize; 6]>) -> EmbeddedVignettingMap {
    let src = VignettingMapOrigin {
        model: Some("NIKON Z 7".into()),
        focal_length: Some(85.0),
        aperture: Some(1.8),
        sha1: Some("ab".repeat(20)),
        file_name: Some("REF_0001.NEF".into()),
        captured: Some("2021-03-27T10:00:00".into()),
        ..Default::default()
    };
    let evs: Vec<f64> = (0..12).map(|i| ev * f64::from(i) / 11.0).collect();
    EmbeddedVignettingMap::from_ev([6000, 4000], [4, 3], window, src, &evs).unwrap()
}

fn id(ev: f64) -> VignettingMapId {
    VignettingMapId::of(&map(ev, None))
}

fn with_map(m: Option<VignettingMapId>, amount: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.optics.vignetting_calibration = m;
    s.optics.vignetting_calibration_amount = amount;
    s
}

#[test]
fn a_map_round_trips_and_quantises_to_a_4096th_ev() {
    let m = map(1.3, Some([8288, 5520, 688, 0, 6000, 4000]));
    let v = serde_json::to_value(&m).unwrap();
    assert!(v["ev"].is_string(), "{v}");
    let back = map_from_json(&v).unwrap();
    assert_eq!(back, m);
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
    for i in 0..12 {
        assert!((m.ev_of(i) - 1.3 * i as f64 / 11.0).abs() <= 0.5 / VIGNETTING_CALIBRATION_EV_STEP, "{i}");
    }
    assert_eq!(m.ev_of(99), 0.0);
    // out-of-range gains are held to what a code can hold, never wrap
    let e = EmbeddedVignettingMap::from_ev([4, 4], [2, 2], None, Default::default(), &[-1.0, f64::NAN, 99.0, f64::INFINITY]).unwrap();
    assert_eq!(e.ev.0, vec![0, 0, u16::MAX, 0]);
    assert!(EmbeddedVignettingMap::from_ev([4, 4], [2, 2], None, Default::default(), &[0.0; 3]).is_none());
}

#[test]
fn the_id_is_the_maps_own_hash() {
    let a = map(1.0, None);
    let i = VignettingMapId::of(&a);
    assert_eq!(i.as_str().len(), 64);
    assert_eq!(VignettingMapId::of(&map(1.0, None)), i, "the same map, the same id");
    assert_ne!(VignettingMapId::of(&map(1.1, None)), i, "another map, another id");
    let mut other_source = a.clone();
    other_source.source.file_name = Some("REF_0002.NEF".into());
    assert_ne!(VignettingMapId::of(&other_source), i, "the reference shot's facts are part of the map");
    assert_eq!(VignettingMapId::parse(i.as_str()), Some(i.clone()));
    for bad in ["", "abc", &"g".repeat(64), &"A".repeat(64), &"0".repeat(63), &"0".repeat(65)] {
        assert!(VignettingMapId::parse(bad).is_none(), "{bad}");
    }
    assert_eq!(i.short().len(), 8);
}

#[test]
fn settings_hold_only_the_id_and_keep_their_json_and_hash_without_one() {
    let d = DevelopSettings::default();
    let optics = d.to_json()["optics"].clone();
    assert!(optics.get("vignetting_calibration").is_none() && optics.get("vignetting_calibration_amount").is_none(), "{optics}");
    assert_eq!(DevelopSettings::from_json(&d.to_json()).unwrap().hash64(), d.hash64());
    let s = with_map(Some(id(1.0)), 80.0);
    let v = s.to_json();
    assert_eq!(v["optics"]["vignetting_calibration"], json!(id(1.0).as_str()));
    assert_eq!(v["optics"]["vignetting_calibration_amount"], json!(80.0));
    assert!(serde_json::to_string(&v).unwrap().len() < d.to_json().to_string().len() + 150, "about 130 bytes more");
    assert_eq!(DevelopSettings::from_json(&v).unwrap(), s);
    assert_ne!(s.hash64(), with_map(Some(id(1.1)), 80.0).hash64(), "another map is another settings hash");
}

#[test]
fn anything_but_an_id_reads_as_no_map_and_the_rest_is_kept() {
    for bad in [json!("not an id"), json!(17), json!({"grid": [4, 3]}), json!("A".repeat(64)), serde_json::to_value(map(1.0, None)).unwrap()] {
        let raw =
            json!({"optics": {"vignetting_calibration": bad, "vignetting_calibration_amount": 70.0, "vignetting": 12.0}, "light": {"exposure": 0.5}});
        let s = DevelopSettings::from_json(&raw).unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert!(s.optics.vignetting_calibration.is_none(), "{bad}");
        assert_eq!((s.optics.vignetting_calibration_amount, s.optics.vignetting, s.light.exposure), (70.0, 12.0, 0.5), "{bad}");
    }
}

#[test]
fn a_damaged_map_is_no_map() {
    let good = serde_json::to_value(map(1.0, None)).unwrap();
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut m = good.clone();
        f(&mut m);
        m
    };
    let cases = [
        bad(&|m| m["ev"] = json!("not base64 !")),
        bad(&|m| m["ev"] = json!("AAAA")),                        // 3 bytes: an odd byte count
        bad(&|m| m["ev"] = json!("AAAAAA==")),                    // 2 codes for 12 cells
        bad(&|m| m["ev"] = json!("A".repeat(MAX_CODES_B64 + 4))), // refused before decoding
        bad(&|m| m["ev"] = json!(17)),
        bad(&|m| m["grid"] = json!([300, 3])),
        bad(&|m| m["grid"] = json!([1, 12])),
        bad(&|m| m["grid"] = json!([-4, 3])),
        bad(&|m| m["sensor"] = json!([3, 4000])),
        bad(&|m| m["sensor"] = json!([6000, usize::MAX])),
        bad(&|m| m["version"] = json!(2)),
        bad(&|m| m["window"] = json!([8288, 5520, 4000, 0, 6000, 4000])), // past the sensor's edge
        bad(&|m| m["window"] = json!([8288, 5520, 0, 0, 5000, 4000])),    // not this map's size
        bad(&|m| m["window"] = json!([8288, 5520, usize::MAX, 0, 6000, 4000])),
        bad(&|m| m["window"] = json!([8288, 5520, 0])),
        bad(&|m| m["source"]["focal_length"] = json!(-85.0)),
        bad(&|m| m["source"]["file_name"] = json!("x".repeat(MAX_TEXT + 1))),
        json!("a map"),
        json!([1, 2, 3]),
    ];
    for (i, m) in cases.iter().enumerate() {
        assert!(map_from_json(m).is_none(), "case {i}");
        // in a portable partial, a damaged map is dropped and the id left as it was
        let mut v = json!({"optics": {"vignetting_calibration": id(2.0).as_str(), PORTABLE_MAP_KEY: m}});
        assert!(absorb_portable(&mut v).is_none(), "case {i}");
        assert_eq!(v, json!({"optics": {"vignetting_calibration": id(2.0).as_str()}}), "case {i}");
    }
}

#[test]
fn the_portable_form_carries_the_map_and_reading_it_takes_it_out() {
    let m = map(1.0, Some([8288, 5520, 688, 0, 6000, 4000]));
    let i = VignettingMapId::of(&m);
    let s = with_map(Some(i.clone()), 90.0);
    let mut v = s.to_json();
    embed_portable(&mut v, |want| (*want == i).then(|| m.clone()));
    assert_eq!(map_from_json(&v["optics"][PORTABLE_MAP_KEY]).as_ref(), Some(&m));
    // an older reader ignores the extra key
    assert_eq!(DevelopSettings::from_json(&v).unwrap(), s);
    let (got, back) = absorb_portable(&mut v).unwrap();
    assert_eq!((got, back), (i.clone(), m.clone()));
    assert_eq!(v, s.to_json(), "the reference stays, the map is out");
    // nothing to embed without a map, or when the catalog doesn't know it
    let mut plain = DevelopSettings::default().to_json();
    embed_portable(&mut plain, |_| Some(m.clone()));
    assert_eq!(plain, DevelopSettings::default().to_json());
    let mut unknown = s.to_json();
    embed_portable(&mut unknown, |_| None);
    assert_eq!(unknown, s.to_json());
    // an id that disagrees with its map (edited by hand) gets the map's
    let mut tampered = json!({"optics": {"vignetting_calibration": id(3.0).as_str(), PORTABLE_MAP_KEY: serde_json::to_value(&m).unwrap()}});
    assert_eq!(absorb_portable(&mut tampered).map(|(i, _)| i), Some(i.clone()));
    assert_eq!(tampered["optics"]["vignetting_calibration"], json!(i.as_str()));
    // a map with no id at all gets its own
    let mut bare = json!({"optics": {PORTABLE_MAP_KEY: serde_json::to_value(&m).unwrap()}});
    assert_eq!(absorb_portable(&mut bare).map(|(i, _)| i), Some(i.clone()));
    assert_eq!(bare["optics"]["vignetting_calibration"], json!(i.as_str()));
    // merging a portable partial references the map by its id
    let merged = DevelopSettings::default().merged(&json!({"optics": {PORTABLE_MAP_KEY: serde_json::to_value(&m).unwrap()}})).unwrap();
    assert_eq!((merged.optics.vignetting_calibration, merged.optics.vignetting_calibration_amount), (Some(i), 100.0));
}

#[test]
fn the_amount_is_kept_in_range() {
    let s = DevelopSettings::default();
    for (given, kept) in [(500.0, 120.0), (-5.0, 0.0), (1e308, 120.0), (60.0, 60.0)] {
        let m = s.merged(&json!({"optics": {"vignetting_calibration_amount": given}})).unwrap();
        assert_eq!(m.optics.vignetting_calibration_amount, kept, "{given}");
    }
    assert_eq!(crate::controls::get(&with_map(None, 42.0), "optics.vignettingCalibrationAmount"), Some(42.0));
}

/// Design notes §10 *Storage*: named without an amount, a map comes at 100; `null` removes it and keeps the amount;
/// the amount alone sets only the amount.
#[test]
fn partial_settings_follow_the_vignetting_calibration_rule_in_both_merge_paths() {
    let (old, new) = (id(2.0), id(0.5));
    let photo = with_map(Some(old.clone()), 40.0);
    type Merge = fn(&DevelopSettings, &Value) -> DevelopSettings;
    let paths: [(&str, Merge); 2] = [("merged", |s, p| s.merged(p).unwrap()), ("apply_partial", |s, p| apply_partial(s, p, 1.0))];
    for (name, merge) in paths {
        let m = merge(&photo, &json!({"optics": {"vignetting_calibration": new.as_str()}}));
        assert_eq!((m.optics.vignetting_calibration.as_ref(), m.optics.vignetting_calibration_amount), (Some(&new), 100.0), "{name}");
        let m = merge(&photo, &json!({"optics": {"vignetting_calibration": new.as_str(), "vignetting_calibration_amount": 70}}));
        assert_eq!((m.optics.vignetting_calibration.as_ref(), m.optics.vignetting_calibration_amount), (Some(&new), 70.0), "{name}");
        let m = merge(&photo, &json!({"optics": {"vignetting_calibration": null}}));
        assert_eq!((m.optics.vignetting_calibration.as_ref(), m.optics.vignetting_calibration_amount), (None, 40.0), "{name}");
        let m = merge(&photo, &json!({"optics": {"vignetting_calibration_amount": 90}}));
        assert_eq!((m.optics.vignetting_calibration.as_ref(), m.optics.vignetting_calibration_amount), (Some(&old), 90.0), "{name}");
        let m = merge(&photo, &json!({"optics": {"vignetting": 10}}));
        assert_eq!((m.optics.vignetting_calibration.as_ref(), m.optics.vignetting_calibration_amount), (Some(&old), 40.0), "{name}");
    }
}

#[test]
fn preset_strength_takes_the_map_whole_and_fades_a_new_one_in() {
    let photo = with_map(Some(id(2.0)), 40.0);
    let p = json!({"optics": {"vignetting_calibration": id(0.5).as_str()}});
    let half = apply_partial(&photo, &p, 0.5);
    assert_eq!(half.optics.vignetting_calibration, Some(id(0.5)));
    assert!((half.optics.vignetting_calibration_amount - 50.0).abs() < 1e-9, "{}", half.optics.vignetting_calibration_amount);
    assert_eq!(apply_partial(&photo, &p, 0.0), photo);
    // the photo's own map again: the amount blends from the photo's
    let same = json!({"optics": {"vignetting_calibration": id(2.0).as_str(), "vignetting_calibration_amount": 100}});
    assert!((apply_partial(&photo, &same, 0.5).optics.vignetting_calibration_amount - 70.0).abs() < 1e-9);
    // a portable preset's map is never blended number by number
    let m = map(0.5, None);
    let portable = json!({"optics": {PORTABLE_MAP_KEY: serde_json::to_value(&m).unwrap()}});
    let half = apply_partial(&photo, &portable, 0.5);
    assert_eq!(half.optics.vignetting_calibration, Some(VignettingMapId::of(&m)));
}

#[test]
fn copy_and_paste_carry_the_reference_and_its_amount() {
    let a = with_map(Some(id(1.0)), 100.0);
    let b = with_map(Some(id(2.0)), 40.0);
    let part = SettingsPick::from_ids(&["optics.vignettingCalibration"]).extract(&a);
    assert_eq!(part, json!({"optics": {"vignetting_calibration": id(1.0).as_str(), "vignetting_calibration_amount": 100.0}}));
    assert_eq!(b.merged(&part).unwrap().optics, a.optics);
    let group = crate::presets::extract_groups(&a, &[crate::SettingsGroup::Optics]);
    assert_eq!(b.merged(&group).unwrap().optics, a.optics);
    // another Optics setting alone leaves the map and the amount
    let vig = SettingsPick::from_ids(&["optics.vignetting"]).extract(&a);
    let m = b.merged(&vig).unwrap();
    assert_eq!((m.optics.vignetting_calibration, m.optics.vignetting_calibration_amount), (b.optics.vignetting_calibration.clone(), 40.0));
}
