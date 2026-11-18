//! Levels settings: neutral serialisation, the signed midtone and its marker position, and the
//! degenerate cases (black and white close together or inverted, a midtone out of range, inverted output levels).

use super::*;
use crate::levels_curve;
use crate::presets::{SettingsGroup, apply_partial, extract_groups};
use serde_json::json;

fn edited() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.levels.master.black = 20.0;
    s.levels.master.midtone = 0.4;
    s.levels.red.white = 240.0;
    s
}

#[test]
fn default_levels_leave_the_json_and_hash_unchanged() {
    let s = DevelopSettings::default();
    assert!(s.to_json().get("levels").is_none(), "no levels key while neutral");
    assert!(Levels::default().is_default());
    // a levels object spelled out at its defaults reads back as the same settings, same JSON, same hash
    let s = DevelopSettings::from_json(&json!({})).unwrap();
    let spelled = DevelopSettings::from_json(
        &json!({"levels": {"master": {"black": 0.0, "midtone": 0.0, "white": 255.0, "out_black": 0.0, "out_white": 255.0}}}),
    )
    .unwrap();
    assert_eq!(spelled, s);
    assert_eq!(spelled.hash64(), s.hash64());
    assert_eq!(serde_json::to_string(&spelled).unwrap(), serde_json::to_string(&s).unwrap());
    // settings from before Levels existed (no key at all) are untouched
    let old = DevelopSettings::from_json(&json!({"light": {"exposure": 0.5}})).unwrap();
    assert!(old.levels.is_default());
    assert!(old.to_json().get("levels").is_none());
}

#[test]
fn only_edited_channels_are_written_and_round_trip() {
    let s = edited();
    let v = s.to_json();
    let l = v["levels"].as_object().unwrap();
    assert!(l.contains_key("master") && l.contains_key("red"));
    assert!(!l.contains_key("green") && !l.contains_key("blue"), "default channels are skipped: {l:?}");
    let back = DevelopSettings::from_json(&v).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.hash64(), s.hash64());
    assert_ne!(s.hash64(), DevelopSettings::default().hash64(), "cached renders are told apart");
}

#[test]
fn the_midtone_marker_round_trips() {
    let mut c = ChannelLevels::default();
    // neutral sits halfway; the box shows it as 128, and typing 128 gives exactly 0
    assert!((c.midtone_position() - 127.5).abs() < 1e-9);
    c.set_midtone_position(128.0);
    assert_eq!(c.midtone, 0.0);
    assert!(c.is_default());
    // the marker reaches about a quarter to seven tenths of the range (of the 0..255 scale: 65 to 183);
    // every whole level in that reach comes back as itself
    let (lo, hi) = (levels_curve::marker(1.0), levels_curve::marker(-1.0));
    assert!((0.24..0.27).contains(&lo) && (0.70..0.74).contains(&hi), "{lo} {hi}");
    for typed in 66..=182 {
        let mut c = ChannelLevels::default();
        c.set_midtone_position(f64::from(typed));
        assert_eq!(c.midtone_position().round(), f64::from(typed), "typed {typed}, midtone {}", c.midtone);
        assert!((-1.0..=1.0).contains(&c.midtone));
    }
    // inside a narrower range, relative to its black and white points
    let mut c = ChannelLevels { black: 40.0, white: 200.0, ..Default::default() };
    assert!((c.midtone_position() - 120.0).abs() < 1e-9);
    for typed in 82..=154 {
        c.set_midtone_position(f64::from(typed));
        assert_eq!(c.midtone_position().round(), f64::from(typed), "typed {typed}");
    }
    // left of the centre (toward the shadows) brightens: a positive midtone; right of it darkens
    c.set_midtone_position(90.0);
    assert!(c.midtone > 0.0);
    c.set_midtone_position(150.0);
    assert!(c.midtone < 0.0);
    // the other way round: a value to a position and back (to a thousandth, away from the flat ends)
    for m in [-0.8, -0.5, -0.2, 0.1, 0.4, 0.7] {
        let c = ChannelLevels { midtone: m, ..Default::default() };
        let mut back = ChannelLevels::default();
        back.set_midtone_position(c.midtone_position());
        assert!((back.midtone - m).abs() < 2e-3, "{m} -> {}", back.midtone);
    }
}

#[test]
fn midtone_values_are_limited_and_unreachable_positions_clamp() {
    let mut c = ChannelLevels::default();
    c.set_midtone_position(0.0);
    assert_eq!(c.midtone, 1.0);
    c.set_midtone_position(255.0);
    assert_eq!(c.midtone, -1.0);
    c.set_midtone_position(-50.0);
    assert_eq!(c.midtone, 1.0);
    c.set_midtone_position(9999.0);
    assert_eq!(c.midtone, -1.0);
    let before = c;
    c.set_midtone_position(f64::NAN);
    assert_eq!(c, before, "not a number: unchanged");
    for (given, kept) in [(3.0, 1.0), (-3.0, -1.0), (1e9, 1.0), (f64::NAN, 0.0), (f64::INFINITY, 0.0)] {
        let s = ChannelLevels { midtone: given, ..Default::default() }.sanitized();
        assert_eq!(s.midtone, kept, "{given}");
        assert!(ChannelLevels { midtone: given, ..Default::default() }.midtone_position().is_finite());
    }
}

#[test]
fn black_and_white_keep_a_minimum_distance_either_way_round() {
    for (black, white) in
        [(100.0, 100.0), (200.0, 50.0), (255.0, 255.0), (255.0, 0.0), (0.0, 0.0), (254.0, 255.0), (255.0, 254.0), (f64::NAN, f64::NAN), (-5.0, 300.0)]
    {
        let s = ChannelLevels { black, white, ..Default::default() }.sanitized();
        assert!((0.0..=255.0).contains(&s.black) && (0.0..=255.0).contains(&s.white), "({black}, {white}) -> ({}, {})", s.black, s.white);
        assert!((s.white - s.black).abs() >= LEVELS_MIN_GAP - 1e-9, "({black}, {white}) -> ({}, {})", s.black, s.white);
    }
    // an inverted range (black above white) is valid and kept as it is
    for (black, white) in [(215.0, 40.0), (255.0, 0.0), (200.0, 198.0), (3.0, 1.0)] {
        let c = ChannelLevels { black, white, ..Default::default() };
        assert_eq!(c.sanitized(), c, "({black}, {white}) is inverted and valid");
    }
    // too close: the white point gives way, on the side it was on (a tie goes up, or down at the top of the scale)
    let at = |black: f64, white: f64| {
        let s = ChannelLevels { black, white, ..Default::default() }.sanitized();
        (s.black, s.white)
    };
    assert_eq!(at(100.0, 101.0), (100.0, 102.0));
    assert_eq!(at(100.0, 99.0), (100.0, 98.0), "an inverted pair stays inverted");
    assert_eq!(at(100.0, 100.0), (100.0, 102.0));
    assert_eq!(at(255.0, 255.0), (255.0, 253.0));
    assert_eq!(at(1.0, 0.0), (1.0, 3.0), "it goes the other way when its own side has no room");
    // the editing helpers: the point just set wins and the other gives way, on the side it was on
    let mut c = ChannelLevels { white: 100.0, ..Default::default() };
    c.black = 99.0;
    c.after_black_set();
    assert_eq!((c.black, c.white), (99.0, 101.0));
    c.white = 98.0;
    c.after_white_set();
    assert_eq!((c.white, c.black), (98.0, 100.0), "the black point goes to the side it was on (above)");
    let mut c = ChannelLevels { black: 255.0, white: 255.0, ..Default::default() };
    c.after_black_set();
    assert_eq!((c.black, c.white), (255.0, 253.0));
    let mut c = ChannelLevels { white: 0.0, ..Default::default() };
    c.after_white_set();
    assert_eq!((c.black, c.white), (2.0, 0.0), "a tie goes up");
    // settings arriving from outside (a preset, a sidecar) are brought in range the same way
    let m = DevelopSettings::default().merged(&json!({"levels": {"green": {"black": 180.0, "white": 90.0, "midtone": 50.0}}})).unwrap();
    let g = m.levels.green;
    assert_eq!((g.black, g.white), (180.0, 90.0), "an inverted range survives a merge");
    assert_eq!(g.midtone, 1.0, "a midtone out of range is limited");
}

#[test]
fn the_midtone_marker_works_on_an_inverted_range() {
    // black 215, white 40: the range runs from right to left; halfway is 127.5
    let mut c = ChannelLevels { black: 215.0, white: 40.0, ..Default::default() };
    assert!((c.midtone_position() - 127.5).abs() < 1e-9);
    c.set_midtone_position(128.0);
    assert!(c.midtone == 0.0, "the rounded centre is the neutral midtone");
    // a marker nearer the black point (here the right one, 215) brightens the midtones as on a normal range: positive
    c.set_midtone_position(170.0);
    assert!(c.midtone > 0.0, "{}", c.midtone);
    c.set_midtone_position(100.0);
    assert!(c.midtone < 0.0, "{}", c.midtone);
    // every whole level in the marker's reach (about 90 to 170 here) comes back as itself
    for typed in 92..=168 {
        c.set_midtone_position(f64::from(typed));
        assert_eq!(c.midtone_position().round(), f64::from(typed), "typed {typed}, midtone {}", c.midtone);
    }
    // the same midtone puts the marker at the mirrored place on the mirrored range
    let (a, b) = (
        ChannelLevels { black: 40.0, white: 215.0, midtone: 0.5, ..Default::default() },
        ChannelLevels { black: 215.0, white: 40.0, midtone: 0.5, ..Default::default() },
    );
    assert!((a.midtone_position() - 40.0 - (215.0 - b.midtone_position())).abs() < 1e-9);
}

#[test]
fn inverted_output_levels_are_kept_and_valid() {
    let c = ChannelLevels { out_black: 255.0, out_white: 0.0, ..Default::default() };
    assert_eq!(c.sanitized(), c, "an inverted output range is allowed (it reverses the channel)");
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "levels.master.outBlack", 200.0);
    controls::set(&mut s, "levels.master.outWhite", 30.0);
    assert_eq!((s.levels.master.out_black, s.levels.master.out_white), (200.0, 30.0));
    let back = DevelopSettings::from_json(&s.to_json()).unwrap();
    assert_eq!(back.levels, s.levels);
    // equal output points (a flat channel) are valid too
    let flat = ChannelLevels { out_black: 90.0, out_white: 90.0, ..Default::default() };
    assert_eq!(flat.sanitized(), flat);
}

#[test]
fn every_levels_control_addresses_its_field() {
    let ids: Vec<&str> = controls::in_section(Section::Levels).map(|c| c.id).collect();
    assert_eq!(ids.len(), 20);
    let mut s = DevelopSettings::default();
    for ch in Levels::CHANNELS {
        for (field, v) in [("black", 12.0), ("midtone", 0.7), ("white", 230.0), ("outBlack", 8.0), ("outWhite", 240.0)] {
            let id = format!("levels.{ch}.{field}");
            assert!(ids.contains(&id.as_str()), "{id}");
            assert!(controls::set(&mut s, &id, v), "{id}");
            assert_eq!(controls::get(&s, &id), Some(v), "{id}");
        }
    }
    // reset puts everything back, and the section's eye switches it off
    s.reset_section(Section::Levels);
    assert!(s.levels.is_default());
    let mut e = edited();
    e.set_section_enabled("levels", false);
    assert!(e.effective().levels.is_default());
    assert!(!e.levels.is_default(), "the stored values stay");
}

#[test]
fn levels_copy_paste_and_presets_follow_the_other_tone_groups() {
    let g = SettingsGroup::Levels;
    assert_eq!(g.keys(), &["levels"]);
    assert_eq!(g.id(), "levels");
    assert!(SettingsGroup::ALL.contains(&g));
    // in the same copy and preset sets as the Tone Curve and the Primaries
    for other in [SettingsGroup::ToneCurve, SettingsGroup::Primaries] {
        assert_eq!(SettingsGroup::default_copy().contains(&g), SettingsGroup::default_copy().contains(&other));
        assert_eq!(SettingsGroup::default_preset().contains(&g), SettingsGroup::default_preset().contains(&other));
    }
    // pasting the group carries the channels; the photo's other settings stay
    let src = edited();
    let mut dst = DevelopSettings::default();
    dst.light.exposure = 0.3;
    let partial = extract_groups(&src, &[g]);
    let out = apply_partial(&dst, &partial, 1.0);
    assert_eq!(out.levels, src.levels);
    assert_eq!(out.light.exposure, 0.3);
    // a preset made from it applies at half strength: the midtone and the points blend from neutral
    let half = apply_partial(&DevelopSettings::default(), &partial, 0.5);
    assert!(half.levels.master.black > 0.0 && half.levels.master.black < src.levels.master.black, "{:?}", half.levels.master);
}
