//! Tone Equalizer settings: neutral settings keep their JSON and hash, hostile values come back in range,
//! controls address every band, and copy/paste and the section's eye reset it.

use super::*;
use crate::presets::{SettingsGroup, SettingsPick, apply_partial};
use serde_json::json;

#[test]
fn neutral_tone_eq_is_not_written() {
    let s = DevelopSettings::default();
    assert!(s.to_json().get("tone_eq").is_none());
    let mut e = s.clone();
    e.tone_eq.ev4 = 0.5;
    assert!(e.to_json().get("tone_eq").is_some());
    assert_ne!(e.hash64(), s.hash64());
    let back = DevelopSettings::from_json(&e.to_json()).unwrap();
    assert_eq!(back.tone_eq, e.tone_eq);
}

#[test]
fn hostile_tone_eq_values_come_back_in_range() {
    let cases = [
        json!({"tone_eq": {"ev0": 1e300, "ev8": -1e300, "smoothing": 99, "detail": -5, "size": 1e9, "edges": -1e9}}),
        json!({"tone_eq": {"ev3": "loud", "detail": null}}),
        json!({"tone_eq": [1, 2, 3]}),
        json!({"tone_eq": "x"}),
        json!({"tone_eq": {"ev1": -0.0}}),
    ];
    for v in cases {
        let s = DevelopSettings::from_untrusted_json(&v).unwrap_or_default();
        let t = s.tone_eq;
        for b in t.bands() {
            assert!((-2.0..=2.0).contains(&b), "{v}: {t:?}");
        }
        assert!((-1.0..=1.0).contains(&t.smoothing) && (0.0..=100.0).contains(&t.detail), "{v}: {t:?}");
        assert!((0.0..=100.0).contains(&t.size) && (0.0..=100.0).contains(&t.edges), "{v}: {t:?}");
        assert_eq!(t, t.sanitized(), "sanitizing twice changes nothing: {v}");
    }
    let nan = ToneEq { ev2: f64::NAN, smoothing: f64::INFINITY, detail: f64::NEG_INFINITY, ..Default::default() }.sanitized();
    assert!(nan.is_default(), "{nan:?}");
}

#[test]
fn every_band_is_a_control() {
    let mut s = DevelopSettings::default();
    for i in 0..tone_eq::BANDS {
        let id = format!("toneEq.ev{i}");
        assert!(controls::set(&mut s, &id, 9.0), "{id}");
        assert_eq!(controls::get(&s, &id), Some(2.0), "{id} is clamped to its range");
    }
    assert!(s.tone_eq.bands().iter().all(|b| *b == 2.0));
    assert!(controls::in_section(Section::ToneEq).count() > tone_eq::BANDS);
    s.reset_section(Section::ToneEq);
    assert!(s.tone_eq.is_default());
}

#[test]
fn the_eye_switches_it_off() {
    let mut s = DevelopSettings::default();
    s.tone_eq.ev2 = 1.0;
    assert!(s.effective().tone_eq.active());
    s.set_section_enabled("toneEq", false);
    assert!(!s.effective().tone_eq.active());
}

#[test]
fn pasting_a_neutral_tone_eq_resets_the_destination() {
    let pick = SettingsPick::of_groups(vec![SettingsGroup::ToneEq]);
    let partial = pick.extract(&DevelopSettings::default());
    assert!(partial.get("tone_eq").is_some(), "{partial}");
    let mut dst = DevelopSettings::default();
    dst.tone_eq.ev6 = 1.5;
    dst.light.exposure = 0.3;
    let out = apply_partial(&dst, &partial, 1.0);
    assert!(out.tone_eq.is_default());
    assert_eq!(out.light.exposure, 0.3);
    // half a preset amount blends from the photo's own values
    let mut src = DevelopSettings::default();
    src.tone_eq.ev6 = -1.0;
    let half = apply_partial(&dst, &pick.extract(&src), 0.5);
    assert!((half.tone_eq.ev6 - 0.25).abs() < 1e-9, "{:?}", half.tone_eq);
}
