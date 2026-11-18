//! Levels through copy, paste, presets, controls and JSON: neutral channels (not written) still reset, the
//! signed midtone blends linearly, and no path stores black and white closer than the minimum distance.

use super::*;
use crate::presets::{Preset, SettingsGroup, SettingsPick, apply_partial};
use serde_json::json;

fn edited() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.levels.master.black = 20.0;
    s.levels.master.midtone = 0.4;
    s.levels.red.white = 240.0;
    s
}

fn pick() -> SettingsPick {
    SettingsPick::of_groups(vec![SettingsGroup::Levels])
}

#[test]
fn pasting_neutral_levels_resets_the_destination() {
    // the source has no levels field at all (every channel neutral); the Levels group is ticked
    let src = DevelopSettings::default();
    assert!(src.to_json().get("levels").is_none());
    let mut dst = DevelopSettings { levels: edited().levels, ..Default::default() };
    dst.levels.blue.out_black = 40.0;
    dst.light.exposure = 0.3;
    let partial = pick().extract(&src);
    assert!(partial.get("levels").is_some(), "the copy names Levels, so the paste resets them: {partial}");
    let out = apply_partial(&dst, &partial, 1.0);
    assert!(out.levels.is_default(), "{:?}", out.levels);
    assert_eq!(out.light.exposure, 0.3, "the rest stays");
    // the result writes as a photo without Levels: same JSON, same hash
    assert!(out.to_json().get("levels").is_none());
    let mut plain = DevelopSettings::default();
    plain.light.exposure = 0.3;
    assert_eq!(out.hash64(), plain.hash64());
    // a source with only some channels edited resets the destination's others, too
    let mut some = DevelopSettings::default();
    some.levels.green.white = 200.0;
    let out = apply_partial(&dst, &pick().extract(&some), 1.0);
    assert_eq!(out.levels, some.levels);
    // and a group that is not ticked leaves them alone
    let other = SettingsPick::of_groups(vec![SettingsGroup::ToneCurve]).extract(&src);
    assert_eq!(apply_partial(&dst, &other, 1.0).levels, dst.levels);
}

#[test]
fn a_preset_of_neutral_levels_resets_and_a_preset_of_levels_applies() {
    let neutral = Preset::from_settings("p", "Neutral", "g", &DevelopSettings::default(), &[SettingsGroup::Levels]);
    assert!(neutral.settings.get("levels").is_some(), "{}", neutral.settings);
    assert!(SettingsPick::of_partial(&neutral.settings).groups.contains(&SettingsGroup::Levels), "it shows as a Levels preset");
    let photo = DevelopSettings { levels: edited().levels, ..Default::default() };
    assert!(apply_partial(&photo, &neutral.settings, 1.0).levels.is_default());
    // a preset of edited Levels applies on a photo without them
    let made = Preset::from_settings("q", "Strong", "g", &edited(), &[SettingsGroup::Levels]);
    assert_eq!(apply_partial(&DevelopSettings::default(), &made.settings, 1.0).levels, edited().levels);
    // the neutral preset at half strength moves halfway back: black and midtone both linear
    let half = apply_partial(&photo, &neutral.settings, 0.5);
    assert!((half.levels.master.black - 10.0).abs() < 1e-9, "{:?}", half.levels.master);
    assert!((half.levels.master.midtone - 0.2).abs() < 1e-9, "{:?}", half.levels.master);
}

#[test]
fn preset_amounts_blend_the_signed_midtone_linearly_from_neutral() {
    let mut strong = DevelopSettings::default();
    strong.levels.master.midtone = 0.4;
    strong.levels.master.black = 40.0;
    let made = Preset::from_settings("p", "Strong", "g", &strong, &[SettingsGroup::Levels]);
    for (amount, midtone, black) in [(0.25, 0.1, 10.0), (0.5, 0.2, 20.0), (1.0, 0.4, 40.0), (1.5, 0.6, 60.0)] {
        let m = apply_partial(&DevelopSettings::default(), &made.settings, amount).levels.master;
        assert!((m.midtone - midtone).abs() < 1e-9, "amount {amount}: midtone {} want {midtone}", m.midtone);
        assert!((m.black - black).abs() < 1e-9, "amount {amount}: black {}", m.black);
    }
    // from the photo's own midtone, not from neutral
    let mut photo = DevelopSettings::default();
    photo.levels.master.midtone = -0.2;
    let out = apply_partial(&photo, &made.settings, 0.5);
    assert!((out.levels.master.midtone - 0.1).abs() < 1e-9, "{:?}", out.levels.master);
    // and a blend that would pass the end of the range stops there
    let out = apply_partial(&DevelopSettings::default(), &made.settings, 2.0).levels.master;
    assert!((out.midtone - 0.8).abs() < 1e-9 && out.black == 80.0, "{out:?}");
}

/// Every way settings reach a photo keeps black and white apart by the minimum distance, in whichever direction (black above white inverts).
#[test]
fn no_public_path_stores_black_and_white_closer_than_the_minimum() {
    fn ok(s: &DevelopSettings, how: &str) {
        for ch in Levels::CHANNELS {
            let c = s.levels.channel(ch).unwrap();
            assert!((c.white - c.black).abs() >= LEVELS_MIN_GAP - 1e-9, "{how}: {ch} {c:?}");
            assert!((-1.0..=1.0).contains(&c.midtone), "{how}: {ch} {c:?}");
        }
    }
    let values = [-5.0, 0.0, 1.0, 2.0, 100.0, 253.0, 254.0, 255.0, 300.0, f64::NAN];
    // controls (the CLI's --set, MCP, the UI's sliders and boxes): any pair, either order
    for ch in Levels::CHANNELS {
        for &b in &values {
            for &w in &values {
                for order in 0..2 {
                    let mut s = DevelopSettings::default();
                    let (bi, wi) = (format!("levels.{ch}.black"), format!("levels.{ch}.white"));
                    if order == 0 {
                        controls::set(&mut s, &bi, b);
                        controls::set(&mut s, &wi, w);
                    } else {
                        controls::set(&mut s, &wi, w);
                        controls::set(&mut s, &bi, b);
                    }
                    ok(&s, &format!("controls {ch} {b} {w}"));
                }
            }
        }
    }
    // JSON from anywhere: the trusted reader, the untrusted one, a merge, a paste and a preset
    for &b in &values[..9] {
        for &w in &values[..9] {
            let v = json!({"levels": {"master": {"black": b, "white": w}, "red": {"black": w, "white": b, "midtone": b}}});
            ok(&DevelopSettings::from_json(&v).unwrap(), "from_json");
            ok(&DevelopSettings::from_untrusted_json(&v).unwrap(), "from_untrusted_json");
            ok(&DevelopSettings::default().merged(&v).unwrap(), "merged");
            ok(&apply_partial(&edited(), &v, 1.0), "apply_partial");
            ok(&apply_partial(&edited(), &v, 0.5), "apply_partial half");
        }
    }
    // the editing helpers
    for &b in &values[..9] {
        let mut c = ChannelLevels { white: 100.0, ..Default::default() };
        c.black = b;
        c.after_black_set();
        assert!((c.white - c.black).abs() >= LEVELS_MIN_GAP - 1e-9, "{c:?}");
        let mut c = ChannelLevels { black: 100.0, ..Default::default() };
        c.white = b;
        c.after_white_set();
        assert!((c.white - c.black).abs() >= LEVELS_MIN_GAP - 1e-9, "{c:?}");
    }
}

/// Settings that clamp to the same values are the same settings: one JSON and one `hash64`.
#[test]
fn equal_settings_have_equal_hashes() {
    let read = |v: serde_json::Value| DevelopSettings::from_json(&json!({"levels": {"master": v}})).unwrap();
    let a = read(json!({"black": 300.0, "white": 255.0, "midtone": 1000.0}));
    let b = read(json!({"black": 255.0, "white": 253.0, "midtone": 1.0}));
    assert_eq!(a, b);
    assert_eq!(a.hash64(), b.hash64());
    // a negative zero and a missing field are the default: nothing is written
    let z = read(json!({"black": -0.0, "out_black": -0.0}));
    assert!(z.levels.is_default() && z.to_json().get("levels").is_none());
    assert_eq!(z.hash64(), DevelopSettings::from_json(&json!({})).unwrap().hash64());
    // set by id or through JSON: the same
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "levels.master.black", 400.0);
    controls::set(&mut s, "levels.master.white", 0.0);
    let j = DevelopSettings::default().merged(&json!({"levels": {"master": {"black": 255.0, "white": 0.0}}})).unwrap();
    assert_eq!(s.levels.master, ChannelLevels { black: 255.0, white: 0.0, ..Default::default() }, "an inverted range is kept");
    assert_eq!(s.hash64(), j.hash64());
}

/// An inverted range (black above white) is the same settings however it arrives: one JSON, one hash.
#[test]
fn an_inverted_range_round_trips_and_hashes_like_any_other() {
    let mut s = DevelopSettings::default();
    s.levels.master.black = 215.0;
    s.levels.master.white = 40.0;
    s.levels.master.midtone = 0.7;
    let back = DevelopSettings::from_json(&s.to_json()).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.hash64(), s.hash64());
    let mut by_id = DevelopSettings::default();
    controls::set(&mut by_id, "levels.master.black", 215.0);
    controls::set(&mut by_id, "levels.master.white", 40.0);
    controls::set(&mut by_id, "levels.master.midtone", 0.7);
    assert_eq!(by_id, s);
    assert_eq!(by_id.hash64(), s.hash64());
    // a preset or paste carries it, and the inverse range differs from the plain one
    let made = Preset::from_settings("p", "Inverted", "g", &s, &[SettingsGroup::Levels]);
    assert_eq!(apply_partial(&DevelopSettings::default(), &made.settings, 1.0).levels, s.levels);
    let mut plain = DevelopSettings::default();
    plain.levels.master.black = 40.0;
    plain.levels.master.white = 215.0;
    plain.levels.master.midtone = 0.7;
    assert_ne!(plain.hash64(), s.hash64());
}
