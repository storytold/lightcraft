//! Convert Negative settings: omitted while default, read leniently, kept in range, and copied
//! without what belongs to one scan.

use serde_json::json;

use crate::presets::{SettingsGroup, extract_groups};
use crate::*;

fn on() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.negative.enabled = true;
    s.negative.preset = "Portra 400".into();
    s.negative.base = Some([0.6, 0.35, 0.15]);
    s.negative.gray = Some([0.4, 0.4, 0.4]);
    s.negative.measured = Some(NegativeMeasurement { base: [0.6, 0.35, 0.15], black: [0.1; 3], white: [0.9; 3], balance: [1.0; 3] });
    s
}

#[test]
fn a_default_negative_is_not_written_and_keeps_the_hash() {
    let s = DevelopSettings::default();
    assert!(s.to_json().get("negative").is_none());
    let mut t = s.clone();
    t.negative.enabled = true;
    assert_ne!(s.hash64(), t.hash64());
}

#[test]
fn a_negative_round_trips() {
    let s = on();
    assert_eq!(DevelopSettings::from_json(&s.to_json()).unwrap(), s);
}

#[test]
fn a_partial_negative_takes_the_defaults_of_the_rest() {
    let s = DevelopSettings::from_json(&json!({"negative": {"enabled": true}})).unwrap();
    assert!(s.negative.enabled);
    assert_eq!((s.negative.range, s.negative.preset.as_str()), (3.0, "Standard"));
}

#[test]
fn untrusted_values_are_kept_in_range() {
    let mut v = on().to_json();
    v["negative"]["range"] = json!(99.0);
    v["negative"]["exposure"] = json!(-40.0);
    v["negative"]["base"] = json!([5.0, -1.0, 0.5]);
    v["negative"]["measured"]["balance"] = json!([100.0, 0.0, 1.0]);
    let n = DevelopSettings::from_untrusted_json(&v).unwrap().negative;
    assert_eq!((n.range, n.exposure), (10.0, -5.0));
    assert_eq!(n.base, Some([1.0, 0.0, 0.5]));
    assert_eq!(n.measured.unwrap().balance, [2.5, 0.4, 1.0]);
}

#[test]
fn not_a_number_becomes_the_default() {
    let n = Negative { range: f64::NAN, contrast: f64::INFINITY, gray: Some([f64::NAN, 0.5, 2.0]), ..Negative::default() }.sanitized();
    assert_eq!((n.range, n.contrast, n.gray), (3.0, 0.0, Some([0.0, 0.5, 1.0])));
}

#[test]
fn the_sanitized_ranges_are_the_controls_ranges() {
    for c in controls::CONTROLS.iter().filter(|c| c.id.starts_with("negative.")) {
        let mut s = DevelopSettings::default();
        for v in [c.min - 1e3, c.max + 1e3, f64::NAN] {
            controls::set(&mut s, c.id, v);
            let back = Negative { ..s.negative.clone() }.sanitized();
            assert_eq!(controls::get(&back.into_settings(), c.id), controls::get(&s, c.id), "{}", c.id);
        }
        assert_eq!(controls::get(&DevelopSettings::default(), c.id), Some(c.default), "{} default", c.id);
    }
}

#[test]
fn copying_the_group_carries_the_measurement_to_the_rest_of_a_roll() {
    let copied = extract_groups(&on(), &[SettingsGroup::Negative]);
    let n = &copied["negative"];
    assert_eq!(n["preset"], json!("Portra 400"));
    for k in ["base", "gray", "measured"] {
        assert!(n.get(k).is_some(), "{k} is copied");
    }
    assert!(SettingsGroup::default_copy().contains(&SettingsGroup::Negative));
    assert_eq!(SettingsGroup::Negative.keys(), &["negative"]);
}

impl Negative {
    fn into_settings(self) -> DevelopSettings {
        DevelopSettings { negative: self, ..DevelopSettings::default() }
    }
}

#[test]
fn resetting_the_section_switches_the_whole_conversion_off() {
    let mut s = on();
    s.negative.exposure = 2.0;
    s.reset_section(controls::Section::Negative);
    assert_eq!(s.negative, Negative::default());
}

#[test]
fn a_copy_states_the_conversion_even_when_it_is_off_or_has_no_picks() {
    // from a photo that is not a negative: the destination's conversion goes off
    let off = extract_groups(&DevelopSettings::default(), &[SettingsGroup::Negative]);
    assert_eq!(on().merged(&off).unwrap().negative, Negative::default());
    // from a conversion without picks: the destination's picks go
    let mut plain = on();
    plain.negative.base = None;
    plain.negative.gray = None;
    let copied = extract_groups(&plain, &[SettingsGroup::Negative]);
    let pasted = on().merged(&copied).unwrap().negative;
    assert_eq!((pasted.base, pasted.gray), (None, None));
    assert_eq!(pasted.measured, plain.negative.measured);
    // stored JSON stays compact
    assert!(DevelopSettings::default().to_json().get("negative").is_none());
}
