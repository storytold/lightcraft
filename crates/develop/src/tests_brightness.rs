//! Brightness (`light.brightness`): the setting, its control, its JSON, copy/paste and presets.

use crate::presets::{Preset, SettingsGroup, SettingsPick, apply_partial, extract_groups};
use crate::{DevelopSettings, Section, controls};
use serde_json::json;

fn with(b: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.brightness = b;
    s
}

#[test]
fn brightness_is_off_by_default_and_left_out_of_the_json_while_zero() {
    let d = DevelopSettings::default();
    assert_eq!(d.light.brightness, 0.0);
    assert!(d.to_json()["light"].get("brightness").is_none(), "{}", d.to_json()["light"]);
    // settings that never had it keep their JSON and hash, also with an explicit zero or a negative zero
    assert_eq!(d.hash64(), with(0.0).hash64());
    assert_eq!(d.hash64(), with(-0.0).hash64());
    assert_eq!(d.to_json(), with(-0.0).to_json());
    // a value is stored and changes the hash
    let b = with(25.0);
    assert_eq!(b.to_json()["light"]["brightness"], json!(25.0));
    assert_ne!(b.hash64(), d.hash64());
}

#[test]
fn old_settings_without_the_field_load_as_zero_and_a_value_round_trips() {
    // a sidecar / catalog entry written before the slider existed
    let old = json!({"light": {"exposure": 0.5, "contrast": 10.0, "highlights": -20.0, "shadows": 0.0, "whites": 0.0, "blacks": 0.0}, "color": {"vibrance": 5.0}});
    let s = DevelopSettings::from_json(&old).unwrap();
    assert_eq!((s.light.exposure, s.light.contrast, s.light.brightness), (0.5, 10.0, 0.0));
    assert_eq!(DevelopSettings::from_untrusted_json(&old).unwrap().light.brightness, 0.0);
    assert_eq!(DevelopSettings::from_json(&json!({})).unwrap().light.brightness, 0.0);
    for b in [-100.0, -37.0, 1.0, 100.0] {
        let s = with(b);
        assert_eq!(DevelopSettings::from_json(&s.to_json()).unwrap().light.brightness, b);
        assert_eq!(DevelopSettings::from_json(&s.to_json()).unwrap(), s);
    }
}

#[test]
fn the_control_is_a_light_slider_like_its_neighbours_right_after_contrast() {
    let spec = controls::find("light.brightness").unwrap();
    assert_eq!((spec.min, spec.max, spec.default, spec.step, spec.decimals), (-100.0, 100.0, 0.0, 1.0, 0));
    let contrast = controls::find("light.contrast").unwrap();
    assert_eq!((spec.min, spec.max, spec.step, spec.decimals), (contrast.min, contrast.max, contrast.step, contrast.decimals));
    assert_eq!(spec.section, Section::Light);
    let ids: Vec<&str> = controls::in_section(Section::Light).map(|c| c.id).collect();
    let at = |id: &str| ids.iter().position(|i| *i == id).unwrap();
    assert_eq!(at("light.brightness"), at("light.contrast") + 1, "{ids:?}");

    let mut s = DevelopSettings::default();
    assert!(controls::set(&mut s, "light.brightness", 40.0));
    assert_eq!((s.light.brightness, controls::get(&s, "light.brightness")), (40.0, Some(40.0)));
    // out of range is brought back, a number that is none falls back to the default
    controls::set(&mut s, "light.brightness", 250.0);
    assert_eq!(s.light.brightness, 100.0);
    controls::set(&mut s, "light.brightness", -250.0);
    assert_eq!(s.light.brightness, -100.0);
    controls::set(&mut s, "light.brightness", f64::NAN);
    assert_eq!(s.light.brightness, 0.0);
    // merged JSON (presets, agents, imports) is kept in range too
    let m = DevelopSettings::default().merged(&json!({"light": {"brightness": 1.0e9}})).unwrap();
    assert_eq!(m.light.brightness, 100.0);
}

#[test]
fn resetting_the_light_section_resets_brightness() {
    let mut s = with(60.0);
    s.light.exposure = 1.0;
    s.reset_section(Section::Light);
    assert_eq!((s.light.brightness, s.light.exposure), (0.0, 0.0));
}

#[test]
fn copying_writes_brightness_out_even_at_zero_so_pasting_an_unedited_photo_resets_it() {
    let unedited = DevelopSettings::default();
    let copied = extract_groups(&unedited, &[SettingsGroup::Light]);
    assert_eq!(copied["light"]["brightness"], json!(0.0), "{copied}");
    let target = with(25.0);
    assert_eq!(apply_partial(&target, &copied, 1.0).light.brightness, 0.0);

    // a photo with Brightness copies it
    let edited = with(-30.0);
    let copied = extract_groups(&edited, &[SettingsGroup::Light]);
    assert_eq!(copied["light"]["brightness"], json!(-30.0));
    assert_eq!(apply_partial(&DevelopSettings::default(), &copied, 1.0).light.brightness, -30.0);

    // the other groups do not carry it
    let other = extract_groups(&edited, &[SettingsGroup::Color, SettingsGroup::ToneCurve]);
    assert!(other.get("light").is_none(), "{other}");
}

#[test]
fn it_is_a_setting_of_its_own_inside_light() {
    let item = SettingsGroup::Light.items().iter().find(|i| i.id == "light.brightness").unwrap();
    assert_eq!(item.paths, &["light.brightness"]);
    // copying it alone (also at 0) names it and nothing else of Light
    let pick = SettingsPick::from_ids(&["light.brightness"]);
    for b in [0.0, 45.0] {
        let part = pick.extract(&with(b));
        assert_eq!(part, json!({"light": {"brightness": b}}), "{part}");
    }
    // pasting it leaves the photo's other Light settings alone, and Exposure alone leaves Brightness alone
    let mut target = with(25.0);
    target.light.exposure = 0.7;
    let pasted = apply_partial(&target, &pick.extract(&with(-10.0)), 1.0);
    assert_eq!((pasted.light.brightness, pasted.light.exposure), (-10.0, 0.7));
    let mut src = with(80.0);
    src.light.exposure = -1.0;
    let pasted = apply_partial(&target, &SettingsPick::from_ids(&["light.exposure"]).extract(&src), 1.0);
    assert_eq!((pasted.light.brightness, pasted.light.exposure), (25.0, -1.0));
    // the pick of a partial that has it knows it
    assert!(SettingsPick::of_partial(&json!({"light": {"brightness": 0.0}})).has("light.brightness"));
}

#[test]
fn presets_carry_brightness_and_blend_it_with_the_amount_like_the_other_light_settings() {
    let preset = Preset::from_settings("p", "P", "G", &with(40.0), &[SettingsGroup::Light]);
    assert_eq!(preset.settings["light"]["brightness"], json!(40.0));
    for (amount, want) in [(0.0, 0.0), (0.25, 10.0), (0.5, 20.0), (1.0, 40.0), (1.5, 60.0)] {
        let out = apply_partial(&DevelopSettings::default(), &preset.settings, amount);
        assert!((out.light.brightness - want).abs() < 1e-9, "amount {amount}: {}", out.light.brightness);
    }
    // a preset made on an unedited photo (Brightness 0 written) brings a photo's Brightness back towards 0
    let flat = Preset::from_settings("q", "Q", "G", &DevelopSettings::default(), &[SettingsGroup::Light]);
    assert_eq!(flat.settings["light"]["brightness"], json!(0.0));
    let half = apply_partial(&with(40.0), &flat.settings, 0.5);
    assert!((half.light.brightness - 20.0).abs() < 1e-9, "{}", half.light.brightness);
    assert_eq!(apply_partial(&with(40.0), &flat.settings, 1.0).light.brightness, 0.0);
    // a preset that never named it (older presets) leaves it alone
    let old = Preset {
        id: "o".into(),
        name: "O".into(),
        group: "G".into(),
        settings: json!({"light": {"contrast": 12.0}}),
        favorite: false,
        builtin: false,
    };
    assert_eq!(apply_partial(&with(40.0), &old.settings, 1.0).light.brightness, 40.0);
    assert_eq!(apply_partial(&with(40.0), &old.settings, 0.5).light.brightness, 40.0);
}
