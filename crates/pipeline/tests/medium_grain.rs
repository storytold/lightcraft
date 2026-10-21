//! Public render/export and saved-settings seams for the particle-only grain fork.
use lightcraft_develop::{DevelopSettings, Preset, SettingsGroup};
use lightcraft_geom::Rect;
use lightcraft_pipeline::{OutputDepth, PixelWindow, RenderRequest, SourceInfo, render};
use lightcraft_raster::Rgb32f;
use serde_json::json;

fn settings(process: &str) -> DevelopSettings {
    DevelopSettings::from_json(&json!({"grain": {"process": process, "amount": 40, "size": 25, "roughness": 0, "seed": 2026}})).unwrap()
}

#[test]
fn old_and_experimental_settings_both_render_only_particles() {
    let src = Rgb32f::from_fn(128, 96, |_, _| [0.18; 3]);
    let req = RenderRequest::fit(128, 96);
    let info = SourceInfo::default();
    let old = DevelopSettings::from_json(&json!({"grain": {"amount":40,"size":25,"roughness":0,"seed":2026}})).unwrap();
    let legacy = render(&src, &info, &settings("legacy"), &req).image;
    assert!(legacy.data == render(&src, &info, &old, &req).image.data);
    let medium = render(&src, &info, &settings("mediumParticlesV1"), &req).image;
    assert_eq!(medium.data, legacy.data, "legacy input must migrate to particles");
    let mut native = DevelopSettings::default();
    native.grain.amount = 40.0;
    native.grain.seed = 2026;
    assert_eq!(native.grain.roughness, 0.0);
    assert_eq!(medium.data, render(&src, &info, &native, &req).image.data);
    assert_ne!(medium.data, render(&src, &info, &DevelopSettings::default(), &req).image.data);
}

#[test]
fn saved_settings_normalize_process_and_keep_existing_numeric_values() {
    let old_grain = json!({"amount":40.0,"size":25.0,"roughness":0.0,"seed":2026});
    let old = DevelopSettings::from_json(&json!({"grain": old_grain})).unwrap();
    for key in ["amount", "size", "roughness", "seed"] {
        assert_eq!(old.to_json()["grain"][key], old_grain[key]);
    }
    assert_eq!(old.to_json()["grain"]["process"], "mediumParticlesV1");
    let old_rough = DevelopSettings::from_json(&json!({"grain":{"process":"legacy","amount":23,"size":51,"roughness":50,"seed":17}})).unwrap();
    assert_eq!((old_rough.grain.amount, old_rough.grain.size, old_rough.grain.roughness, old_rough.grain.seed), (23.0, 51.0, 50.0, 17));
    let s = settings("mediumParticlesV1");
    let reopened = DevelopSettings::from_json(&s.to_json()).unwrap();
    assert_eq!(s, reopened);
    assert_eq!(s.hash64(), old.hash64());
    let p = Preset::from_settings("medium", "Medium", "Experimental", &s, &[SettingsGroup::Grain]);
    assert_eq!(p.apply(&DevelopSettings::default(), 1.0).grain, s.grain);
    assert!(DevelopSettings::from_json(&json!({"grain":{"process":"unknown"}})).is_err());
}

#[test]
fn grain_presets_at_every_amount_and_numeric_partials_keep_particle_state() {
    use lightcraft_develop::{GrainProcess, apply_partial};
    for (from, to) in [("legacy", "mediumParticlesV1"), ("mediumParticlesV1", "legacy")] {
        let base = settings(from);
        let mut target = settings(to);
        target.light.exposure = 2.0;
        target.grain.amount = 80.0;
        let preset = Preset::from_settings("p", "P", "User", &target, &[SettingsGroup::Grain, SettingsGroup::Light]);
        for amount in [0.0, 0.25, 0.49, 0.5, 1.0, 2.0] {
            let out = preset.apply(&base, amount);
            assert_eq!(out.grain.process, if amount < 0.5 { base.grain.process } else { target.grain.process });
            assert_eq!(out.light.exposure, 2.0 * amount);
            assert_eq!(out.grain.amount, (40.0 + 40.0 * amount).min(100.0));
            assert_eq!(DevelopSettings::from_json(&out.to_json()).unwrap(), out);
        }
        let mut expected = base.grain;
        expected.amount = 12.0;
        assert_eq!(base.merged(&json!({"grain":{"amount":12}})).unwrap().grain, expected);
        assert_eq!(apply_partial(&base, &json!({"grain":{"amount":12}}), 1.0).grain, expected);
    }
    let medium = settings("mediumParticlesV1");
    let old = settings("legacy").to_json();
    assert_eq!(apply_partial(&medium, &json!({"grain":old["grain"]}), 1.0).grain.process, GrainProcess::MediumParticlesV1);
    // Historical preset payloads may contain either alias, or omit the field entirely.
    for process in [None, Some("legacy"), Some("mediumParticlesV1")] {
        let mut patch = json!({"grain":{"amount":80.0,"size":50.0,"roughness":50.0,"seed":81},"light":{"exposure":2.0}});
        if let Some(p) = process {
            patch["grain"]["process"] = json!(p);
        }
        for amount in [0.0, 0.25, 0.49, 0.5, 1.0, 2.0] {
            let out = apply_partial(&medium, &patch, amount);
            assert_eq!(out.light.exposure, 2.0 * amount);
            assert_eq!(out.grain.amount, (40.0 + 40.0 * amount).min(100.0));
            assert_eq!(out.grain.process, GrainProcess::MediumParticlesV1);
            assert_eq!(out.grain.seed, if amount < 0.5 { 2026 } else { 81 });
        }
    }
}

#[test]
fn medium_is_repeatable_seeded_and_zero_amount_or_disabled_effects_is_identity() {
    let src = Rgb32f::from_fn(128, 96, |_, _| [0.18; 3]);
    let req = RenderRequest::fit(128, 96);
    let info = SourceInfo::default();
    let mut s = settings("mediumParticlesV1");
    let a = render(&src, &info, &s, &req).image;
    assert!(a.data == render(&src, &info, &s, &req).image.data);
    s.grain.seed += 1;
    assert!(a.data != render(&src, &info, &s, &req).image.data);
    let clean = render(&src, &info, &DevelopSettings::default(), &req).image;
    s.grain.amount = 0.0;
    assert!(clean.data == render(&src, &info, &s, &req).image.data);
    s.grain.amount = 40.0;
    s = s.merged(&json!({"disabled_sections":["effects"]})).unwrap();
    assert!(clean.data == render(&src, &info, &s, &req).image.data);
}

#[test]
fn medium_windows_and_pixel_aligned_crops_keep_absolute_material_coordinates() {
    let src = Rgb32f::from_fn(256, 192, |_, _| [0.18; 3]);
    let s = settings("mediumParticlesV1");
    let info = SourceInfo::default();
    let req = RenderRequest::fit(256, 192);
    let full = render(&src, &info, &s, &req).image;
    let at = PixelWindow { x: 64, y: 48, w: 128, h: 96 };
    let win = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req }).image;
    let mut cropped = s.clone();
    cropped.crop.geometry.rect = Rect { x0: 0.25, y0: 0.25, x1: 0.75, y1: 0.75 };
    let crop = render(&src, &info, &cropped, &RenderRequest::fit(128, 96)).image;
    for y in 0..96 {
        for x in 0..128 {
            assert_eq!(win.get(x, y), full.get(x + 64, y + 48));
            assert_eq!(crop.get(x, y), full.get(x + 64, y + 48));
        }
    }
}

#[test]
fn preview_filtering_reduces_variance_and_preview_sources_retain_native_scale() {
    let src = Rgb32f::from_fn(512, 384, |_, _| [0.18; 3]);
    let s = settings("mediumParticlesV1");
    let info = SourceInfo::default();
    let native = render(&src, &info, &s, &RenderRequest::fit(512, 384)).image;
    let small = render(&src, &info, &s, &RenderRequest::fit(128, 96)).image;
    let preview_src = Rgb32f::from_fn(128, 96, |_, _| [0.18; 3]);
    let preview_info = SourceInfo { native_long: Some(512), ..SourceInfo::default() };
    assert!(small.data == render(&preview_src, &preview_info, &s, &RenderRequest::fit(128, 96)).image.data);
    let variance = |pixels: &[[u8; 4]]| {
        let mean = pixels.iter().map(|p| p[0] as f64).sum::<f64>() / pixels.len() as f64;
        pixels.iter().map(|p| (p[0] as f64 - mean).powi(2)).sum::<f64>() / pixels.len() as f64
    };
    let (nv, sv) = (variance(&native.data), variance(&small.data));
    println!("medium flat patch: native SD {:.4} levels, 4x preview SD {:.4}", nv.sqrt(), sv.sqrt());
    assert!(nv > 1.0 && sv < nv * 0.65, "preview should integrate, not alias or renormalize the texture: {nv} / {sv}");
}

#[test]
fn deep_export_uses_medium_too() {
    let src = Rgb32f::from_fn(128, 96, |_, _| [0.18; 3]);
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(128, 96) };
    let info = SourceInfo::default();
    let s = settings("mediumParticlesV1");
    let a = render(&src, &info, &s, &req).deep.unwrap();
    let b = render(&src, &info, &DevelopSettings::default(), &req).deep.unwrap();
    match (a.samples, b.samples) {
        (lightcraft_pipeline::DeepSamples::U16(a), lightcraft_pipeline::DeepSamples::U16(b)) => assert!(a != b),
        _ => panic!("expected 16-bit export"),
    }
    let req = RenderRequest { depth: OutputDepth::F32Linear, ..req };
    let a = render(&src, &info, &s, &req).deep.unwrap();
    let b = render(&src, &info, &DevelopSettings::default(), &req).deep.unwrap();
    match (a.samples, b.samples) {
        (lightcraft_pipeline::DeepSamples::F32(a), lightcraft_pipeline::DeepSamples::F32(b)) => {
            assert!(a.iter().all(|v| v.is_finite()));
            assert!(a != b);
        }
        _ => panic!("expected float export"),
    }
}
