//! Process versions: settings saved before they existed stay on V1, new settings get the latest,
//! and numbers this build doesn't know load without harm.

use serde_json::{Value, json};

use crate::*;

/// Complete settings as LightCraft wrote them before process versions existed (main at 8f924a9:
/// `serde_json::to_string` of a raw photo with tone, colour, curve, grading, effects and a radial
/// mask), one top-level key per line. There is no `process` field.
const LEGACY: &str = r#"
{
  "version": 1,
  "profile": {"id": "lc.color", "amount": 100.0},
  "treatment": "color",
  "wb": {"mode": "asShot", "temp": 5200.0, "tint": 4.0},
  "light": {"exposure": 0.4, "contrast": 30.0, "highlights": -60.0, "shadows": 40.0, "whites": 15.0, "blacks": -20.0},
  "curve": {"highlights": -10.0, "lights": 0.0, "darks": 10.0, "shadows": 0.0, "split_shadows": 25.0, "split_mid": 50.0, "split_highlights": 75.0, "master": [{"x": 0.0, "y": 0.05}, {"x": 0.5, "y": 0.55}, {"x": 1.0, "y": 0.95}], "red": [], "green": [], "blue": [], "refine_saturation": 100.0},
  "color": {"vibrance": 20.0, "saturation": -5.0},
  "mixer": {"red": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "orange": {"hue": 0.0, "sat": 0.0, "lum": 15.0}, "yellow": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "green": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "aqua": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "blue": {"hue": 0.0, "sat": -30.0, "lum": 0.0}, "purple": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "magenta": {"hue": 0.0, "sat": 0.0, "lum": 0.0}},
  "point_colors": [],
  "bw_mix": {"red": 0.0, "orange": 0.0, "yellow": 0.0, "green": 0.0, "aqua": 0.0, "blue": 0.0, "purple": 0.0, "magenta": 0.0},
  "grading": {"shadows": {"hue": 220.0, "sat": 20.0, "lum": -5.0}, "midtones": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "highlights": {"hue": 40.0, "sat": 15.0, "lum": 5.0}, "global": {"hue": 0.0, "sat": 0.0, "lum": 0.0}, "blending": 50.0, "balance": 0.0},
  "effects": {"texture": 20.0, "clarity": 25.0, "dehaze": 15.0},
  "vignette": {"amount": -25.0, "midpoint": 50.0, "roundness": 0.0, "feather": 50.0, "highlights": 0.0, "style": "highlightPriority"},
  "grain": {"amount": 20.0, "size": 25.0, "roughness": 50.0, "seed": 0},
  "detail": {"sharpen_amount": 40.0, "sharpen_radius": 1.0, "sharpen_detail": 25.0, "sharpen_masking": 0.0, "nr_luminance": 20.0, "nr_detail": 50.0, "nr_contrast": 0.0, "nr_color": 25.0, "nr_color_detail": 50.0, "nr_color_smoothness": 50.0},
  "optics": {"remove_ca": false, "lens_profile": false, "profile_distortion": 100.0, "profile_vignetting": 100.0, "defringe_purple_amount": 0.0, "defringe_purple_hue_lo": 30.0, "defringe_purple_hue_hi": 70.0, "defringe_green_amount": 0.0, "defringe_green_hue_lo": 40.0, "defringe_green_hue_hi": 60.0, "distortion": 0.0, "vignetting": 0.0, "vignetting_midpoint": 50.0, "ca_red": 0.0, "ca_blue": 0.0},
  "geometry": {"upright": "off", "guides": [], "upright_transform": null, "vertical": 0.0, "horizontal": 0.0, "rotate": 0.0, "aspect": 0.0, "scale": 100.0, "offset_x": 0.0, "offset_y": 0.0, "constrain_crop": false},
  "crop": {"geometry": {"rect": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}, "angle": 0.0}, "aspect": null, "flip_h": false, "flip_v": false},
  "orientation": "Normal",
  "masks": [{"id": 1, "name": "Mask 1", "visible": true, "invert": false, "components": [{"op": "add", "invert": false, "shape": {"kind": "radial", "center": {"x": 0.4, "y": 0.5}, "rx": 0.3, "ry": 0.2, "angle": 15.0, "feather": 60.0, "invert": false}}], "adjust": {"temp": 10.0, "tint": 0.0, "exposure": 0.5, "contrast": 0.0, "highlights": 0.0, "shadows": 0.0, "whites": 0.0, "blacks": 0.0, "texture": 0.0, "clarity": 30.0, "dehaze": 0.0, "hue": 0.0, "saturation": 0.0, "sharpness": 0.0, "noise": 0.0, "moire": 0.0, "defringe": 0.0, "color_hue": 0.0, "color_sat": 0.0, "amount": 100.0}}],
  "spots": [],
  "red_eye": [],
  "lens_blur": {"enabled": false, "amount": 0.0, "focal_distance": 0.0, "focal_range": 0.0},
  "enhance": {"denoise": 0.0, "raw_details": false, "super_resolution": false},
  "calibration": {"shadows_tint": 0.0, "red_hue": 0.0, "red_sat": 0.0, "green_hue": 0.0, "green_sat": 0.0, "blue_hue": 0.0, "blue_sat": 0.0},
  "disabled_sections": []
}
"#;

fn legacy() -> Value {
    serde_json::from_str(LEGACY).unwrap()
}

#[test]
fn settings_saved_before_process_versions_load_as_v1() {
    let v = legacy();
    assert!(v.get("process").is_none(), "the fixture predates the field");
    let s = DevelopSettings::from_json(&v).unwrap();
    // V1 as a number, not `LATEST`: once a newer process is the latest, this is what keeps every
    // existing catalog, sidecar and preset on the rendering it was made with
    assert_eq!(s.process, ProcessVersion(1));
    assert_eq!(s.process, ProcessVersion::legacy());
    assert_eq!((s.light.exposure, s.light.highlights, s.masks.len(), s.detail.sharpen_amount), (0.4, -60.0, 1, 40.0));
    // the smallest legacy file too, and settings nested in other records (history, versions)
    assert_eq!(DevelopSettings::from_json(&json!({})).unwrap().process, ProcessVersion(1));
    let nested: Vec<DevelopSettings> = serde_json::from_value(json!([{}, {"light": {"exposure": 1.0}}])).unwrap();
    assert!(nested.iter().all(|s| s.process == ProcessVersion(1)));
    // saved again, they gain no field: files and preview-cache keys (`hash64`) stay as they were
    assert!(s.to_json().get("process").is_none());
}

#[test]
fn new_settings_get_the_latest_process() {
    assert_eq!(DevelopSettings::default().process, ProcessVersion::LATEST);
    assert_eq!(DevelopSettings::for_raw(5200.0, 4.0).process, ProcessVersion::LATEST);
    assert_eq!(ProcessVersion::LATEST, Process::LATEST.version());
    assert_eq!(ProcessVersion::V1, ProcessVersion(1));
    // the processes are listed oldest first, the latest last, each with its own number
    let versions: Vec<u32> = Process::ALL.iter().map(|p| p.version().0).collect();
    assert!(versions.windows(2).all(|w| w[0] < w[1]), "{versions:?}");
    assert_eq!(Process::ALL.last(), Some(&Process::LATEST));
    assert!(Process::ALL.iter().all(|p| p.version().process() == *p && p.version().is_known()));
}

#[test]
fn v1_is_left_out_when_saving_and_other_processes_are_kept() {
    let mut s = DevelopSettings::from_json(&legacy()).unwrap();
    assert!(s.to_json().get("process").is_none());
    let before = s.hash64();
    s.process = ProcessVersion(2);
    let v = s.to_json();
    assert_eq!(v["process"], json!(2));
    assert_eq!(DevelopSettings::from_json(&v).unwrap().process, ProcessVersion(2));
    assert_ne!(s.hash64(), before, "another process is another preview");
}

#[test]
fn unknown_process_numbers_load_and_render_with_the_newest_known() {
    // written by a newer LightCraft: kept as it is, rendered with the latest process this build has
    let s = DevelopSettings::from_json(&json!({"process": 7, "light": {"exposure": 0.5}})).unwrap();
    assert_eq!(s.process, ProcessVersion(7));
    assert_eq!(s.process.process(), Process::LATEST);
    assert!(!s.process.is_known());
    assert!(!s.process.is_outdated(), "never downgraded by Update to Current Process");
    assert_eq!(s.to_json()["process"], json!(7));
    assert_eq!(s.merged(&json!({"light": {"exposure": 1.0}})).unwrap().process, ProcessVersion(7));
    // below V1 (no LightCraft writes it): rendered as V1, and updatable
    let zero = DevelopSettings::from_json(&json!({"process": 0})).unwrap().process;
    assert_eq!((zero.process(), zero.is_known(), zero.is_outdated()), (Process::V1, false, true));
    let max = DevelopSettings::from_json(&json!({"process": u32::MAX})).unwrap().process;
    assert_eq!(max.process(), Process::LATEST);
    // a whole number written as a float is that number
    assert_eq!(DevelopSettings::from_json(&json!({"process": 2.0})).unwrap().process, ProcessVersion(2));
}

#[test]
fn a_damaged_process_value_reads_as_v1_and_keeps_the_edits() {
    let too_big = u64::from(u32::MAX) + 1;
    for bad in [
        json!(-1),
        json!(1.5),
        json!(-0.5),
        json!(1e300),
        json!(too_big),
        json!(u64::MAX),
        json!("2"),
        json!(null),
        json!(true),
        json!([2]),
        json!({"v": 2}),
    ] {
        let mut v = legacy();
        v["process"] = bad.clone();
        let s = DevelopSettings::from_json(&v).unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert_eq!(s.process, ProcessVersion::V1, "{bad}");
        assert_eq!(
            (s.light.exposure, s.masks.len(), s.curve.master.sparse_points().unwrap().len()),
            (0.4, 1, 3),
            "{bad}: the rest of the settings load"
        );
        // nested in other records too (history, versions, the catalog)
        let nested: Vec<DevelopSettings> = serde_json::from_value(json!([v])).unwrap();
        assert_eq!(nested[0].process, ProcessVersion::V1);
    }
}

#[test]
fn the_process_is_not_an_edit_nor_part_of_copied_settings() {
    let mut s = DevelopSettings { process: ProcessVersion(LATEST_PLUS_ONE), ..Default::default() };
    assert!(s.is_unedited(), "a photo on another process is not edited for that");
    s.light.exposure = 1.0;
    assert!(!s.is_unedited());
    // resetting a section or a slider leaves the process alone
    s.reset_section(Section::Light);
    assert_eq!(s.process, ProcessVersion(LATEST_PLUS_ONE));
    // copy / paste / sync / presets: no group carries the process, so the target keeps its own
    assert!(SettingsGroup::ALL.iter().all(|g| !g.keys().contains(&"process")));
    s.light.exposure = 1.0;
    let copied = extract_groups(&s, &SettingsGroup::ALL);
    assert!(copied.get("process").is_none());
    let target = DevelopSettings::from_json(&legacy()).unwrap();
    let pasted = apply_partial(&target, &copied, 1.0);
    assert_eq!((pasted.process, pasted.light.exposure), (ProcessVersion(1), 1.0));
    // (at another amount, without Grain: its integer seed doesn't survive scaling, a separate issue)
    let groups: Vec<SettingsGroup> = SettingsGroup::ALL.into_iter().filter(|g| *g != SettingsGroup::Grain).collect();
    let preset = Preset::from_settings("p", "P", "User", &s, &groups);
    let half = preset.apply(&target, 0.5);
    assert_eq!((half.process, half.light.exposure), (ProcessVersion(1), 0.7));
    // a partial that names a process explicitly (Apply Settings JSON) sets it
    assert_eq!(target.merged(&json!({"process": LATEST_PLUS_ONE})).unwrap().process, ProcessVersion(LATEST_PLUS_ONE));
}

/// A process other than the latest: a newer LightCraft's number (no older one than V1 exists).
const LATEST_PLUS_ONE: u32 = ProcessVersion::LATEST.0 + 1;

#[test]
fn sparse_schema_markers_keep_legacy_wire_and_hash_with_process_and_hdr_omitted() {
    // FNV-1a of the historical LEGACY fixture's compact, original-order wire, changing only
    // its schema marker. These constants do not come from the new PointCurve serializer.
    for (version, expected_hash) in [(0, 0xa9b9_3888_0e9c_e961), (1, 0x2656_2488_7e99_a10e), (2, 0xb5ca_4299_c8d8_70d7)] {
        let mut wire = legacy();
        wire["version"] = json!(version);
        let settings = DevelopSettings::from_json(&wire).unwrap();
        assert_eq!(settings.version, version);
        assert!(settings.curve.master.sparse_points().is_some());
        assert_eq!(settings.to_json(), wire);
        assert!(settings.to_json().get("process").is_none());
        assert!(settings.to_json().get("hdr").is_none());
        assert_eq!(settings.hash64(), expected_hash, "schema {version}");
        let saved = serde_json::to_string(&settings).unwrap();
        let loaded: DevelopSettings = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded, settings);
        assert_eq!(serde_json::to_string(&loaded).unwrap(), saved);
    }
}

#[test]
fn functions_keep_hdr_camera_tone_and_future_process_while_using_schema2() {
    let settings = DevelopSettings::from_json(&json!({
        "version": 2,
        "process": 77,
        "profile": {"camera_local_tone": true},
        "hdr": {"enabled": true, "max_ev": 4.0, "sdr_brightness": 12.0, "sdr_highlights": -30.0},
        "curve": {"master": {
            "kind": "function", "version": 1,
            "base": {"pieces": [{"end": 1.0, "hermite": [0.1, 0.9, 0.8, 0.8]}]},
            "adjustments": []
        }}
    }))
    .unwrap();
    assert_eq!((settings.version, settings.process), (2, ProcessVersion(77)));
    assert!(settings.curve.master.is_function());
    assert!(settings.profile.camera_local_tone);
    assert_eq!((settings.hdr.enabled, settings.hdr.max_ev, settings.hdr.sdr_brightness, settings.hdr.sdr_highlights), (true, 4.0, 12.0, -30.0));
    let saved = serde_json::to_string(&settings).unwrap();
    let loaded: DevelopSettings = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, settings);
    assert_eq!(loaded.hash64(), settings.hash64());
    assert_eq!(loaded.to_json(), settings.to_json());
    let mut another_process = settings.clone();
    another_process.process = ProcessVersion(78);
    assert_ne!(another_process.hash64(), settings.hash64());
    assert!(!another_process.same_values(&settings));
    another_process.process = ProcessVersion::V1;
    assert!(another_process.to_json().get("process").is_none());
    assert_eq!(another_process.to_json()["version"], 2);
    let sdr = settings.sdr_rendition();
    assert_eq!(sdr.curve, settings.curve);
    assert_eq!(sdr.process, settings.process);
}

#[test]
fn curve_and_hdr_presets_keep_integer_fields_and_captured_process_contract() {
    let mut base = DevelopSettings { process: ProcessVersion(7), ..Default::default() };
    base.hdr.enabled = true;
    base.hdr.max_ev = 2.0;
    let mut target = base.clone();
    target.process = ProcessVersion(99);
    target.hdr.max_ev = 4.0;
    target.light.exposure = 1.0;
    target.grain.seed = 12345;
    target.grain.amount = 40.0;
    target.curve.master = PointCurve::try_from_points(vec![
        lightcraft_geom::Point::new(0.0, 0.1),
        lightcraft_geom::Point::new(0.4, 0.7),
        lightcraft_geom::Point::new(1.0, 0.95),
    ])
    .unwrap();
    let preset = Preset::from_settings("p", "P", "User", &target, &SettingsGroup::ALL);
    assert!(preset.settings.get("process").is_none());
    assert!(preset.settings.get("hdr").is_some());
    assert_eq!(preset.try_apply(&base, 0.0).unwrap(), base);
    for (amount, seed) in [(0.3, 0), (0.5, 12345), (1.0, 12345), (1.5, 12345)] {
        let out = preset.try_apply(&base, amount).unwrap();
        assert_eq!(out.process, base.process);
        assert_eq!(out.grain.seed, seed);
        assert!((out.light.exposure - amount).abs() < 1e-12);
        assert!((out.grain.amount - 40.0 * amount).abs() < 1e-12);
        assert!((out.hdr.max_ev - (2.0 + 2.0 * amount)).abs() < 1e-12);
        let expected = 0.4 + amount * (target.curve.master.eval(0.4).unwrap() - 0.4);
        assert!((out.curve.master.eval(0.4).unwrap() - expected).abs() < 1e-11);
    }
    // Preserve the documented upstream exception: explicit JSON, including a hand-written
    // preset, may name process. Capturing groups above is what normally keeps it out.
    let mut explicit = preset;
    explicit.settings["process"] = json!(88);
    assert_eq!(explicit.try_apply(&base, 1.0).unwrap().process, ProcessVersion(88));
    assert_eq!(try_apply_partial(&base, &json!({"process": 89}), 1.0).unwrap().process, ProcessVersion(89));
}

#[test]
fn future_schema_and_duplicate_wire_markers_remain_checked_with_process_enabled() {
    for version in [json!(3), json!(u64::MAX), json!(-1), json!(1.5)] {
        let wire = json!({"version": version, "process": 77, "light": {"exposure": 1.0}});
        assert!(DevelopSettings::from_json(&wire).is_err());
        assert!(serde_json::from_str::<DevelopSettings>(&wire.to_string()).is_err());
        assert!(try_apply_partial(&DevelopSettings::default(), &wire, 1.0).is_err());
    }
    for wire in [r#"{"version":3,"version":1,"process":77}"#, r#"{"version":1,"version":3,"process":77}"#] {
        assert!(serde_json::from_str::<DevelopSettings>(wire).is_err());
    }
    let raw = DevelopSettings::for_raw(5210.0, -4.0);
    assert_eq!(raw.version, 1, "ordinary sparse settings keep the legacy wire marker");
    let loaded = DevelopSettings::from_json(&raw.to_json()).unwrap();
    assert_eq!(loaded, raw);
    assert_eq!(loaded.hash64(), raw.hash64());
}
