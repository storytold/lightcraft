//! Synthetic regressions for the Edit panel's eye toggles (issue #316).

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskShape, Treatment, WbMode};
use lightcraft_geom::{Point, Rect};
use lightcraft_raster::Rgb32f;
use serde_json::json;

use crate::{OutputDepth, Quality, RenderRequest, SourceInfo, StageCache, plan, render, render_cached};

fn source() -> Arc<Rgb32f> {
    Arc::new(Rgb32f::from_fn(48, 32, |x, y| {
        let noise = if (x + y) % 2 == 0 { 0.02 } else { -0.02 };
        [0.05 + x as f32 * 0.009 + noise, 0.04 + y as f32 * 0.01 - noise, 0.03 + ((x * 7 + y * 3) % 32) as f32 * 0.008 + noise]
    }))
}

#[test]
fn panel_toggles_bypass_nested_edits_and_restore_cached_renders() {
    let src = source();
    let info = SourceInfo { raw: true, ..Default::default() };
    let base = DevelopSettings::default();
    let cases = [
        ("light", json!({"light": {"exposure": 1.5, "shadows": 40}, "curve": {"master": [{"x": 0, "y": 0.1}, {"x": 1, "y": 0.9}]}})),
        (
            "color",
            json!({
                "wb": {"mode": "custom", "temp": 3200, "tint": 25}, "color": {"saturation": -40},
                "mixer": {"red": {"hue": 60}}, "point_colors": [{"lum_shift": 60, "range": 100}],
                "grading": {"highlights": {"hue": 40, "sat": 50}}
            }),
        ),
        ("effects", json!({"effects": {"texture": 60, "clarity": 70, "dehaze": 30}, "vignette": {"amount": -70}, "grain": {"amount": 60}})),
        ("detail", json!({"detail": {"sharpen_amount": 120, "nr_luminance": 60, "nr_color": 60}})),
        ("optics", json!({"optics": {"distortion": 35, "vignetting": -60, "defringe_purple_amount": 50}})),
        ("geometry", json!({"geometry": {"vertical": 30, "rotate": 10}})),
        ("calibration", json!({"calibration": {"red_hue": 80, "green_sat": -60, "shadows_tint": 50}})),
    ];
    for (section, patch) in cases {
        for quality in [Quality::Draft, Quality::Full] {
            let req = RenderRequest { quality, ..RenderRequest::fit(48, 32) };
            let cache = StageCache::default();
            let baseline = render(&src, &info, &base, &req).image;
            let original = base.merged(&patch).unwrap();
            let mut s = original.clone();
            let on = render_cached(&src, &info, &s, &req, &cache).image;
            assert_ne!(on, baseline, "{section} {quality:?}: synthetic edits must be visible");
            s.set_section_enabled(section, false);
            let off = render_cached(&src, &info, &s, &req, &cache).image;
            assert_eq!(off, baseline, "{section} {quality:?}: the whole panel group is bypassed");
            assert_eq!(off, render(&src, &info, &s, &req).image, "{section}: cached and uncached agree");
            s.set_section_enabled(section, true);
            assert_eq!(s, original, "{section}: toggling preserves the stored values");
            assert_eq!(render_cached(&src, &info, &s, &req, &cache).image, on, "{section}: a warm cache restores the edit");
        }
    }
}

#[test]
fn color_toggle_uses_source_as_shot_wb_and_preserves_bw_treatment() {
    let src = source();
    let info = SourceInfo { raw: true, as_shot_temp: 2850.0, as_shot_tint: 12.0, ..Default::default() };
    let req = RenderRequest::fit(48, 32);
    let base = DevelopSettings { treatment: Treatment::Bw, ..DevelopSettings::for_raw(2850.0, 12.0) };
    let mut s = base.clone();
    s.wb.mode = WbMode::Custom;
    s.wb.temp = 9000.0;
    s.wb.tint = -50.0;
    s.bw_mix.red = 80.0;
    let on = render(&src, &info, &s, &req).image;
    s.set_section_enabled("color", false);
    let p = plan(&src, &info, &s, &req);
    assert_eq!(p.settings.wb.mode, WbMode::AsShot);
    assert_eq!(p.settings.treatment, Treatment::Bw);
    assert_eq!(p.lin_key, plan(&src, &info, &base, &req).lin_key, "WB stage key must use the source's as-shot reference");
    let off = render(&src, &info, &s, &req).image;
    assert_eq!(off, render(&src, &info, &base, &req).image);
    assert_ne!(on, off);
    s.set_section_enabled("color", true);
    assert_eq!(render(&src, &info, &s, &req).image, on);
}

#[test]
fn shared_plan_bypasses_user_edits_and_preserves_profile_deltas() {
    let src = source();
    let info = SourceInfo::default();
    let req = RenderRequest::fit(48, 32);
    let mut base = DevelopSettings::default();
    base.profile.id = "lc.film.warm-print".into();
    let mut s = base.clone();
    s.light.exposure = 2.0;
    s.detail.nr_color = 80.0;
    s.curve.red = vec![Point::new(0.0, 0.1), Point::new(1.0, 0.9)];
    s.set_section_enabled("light", false);
    s.set_section_enabled("detail", false);
    let saved = s.clone();
    let p = plan(&src, &info, &s, &req);
    let expected = plan(&src, &info, &base, &req);
    assert_eq!(p.settings.light, expected.settings.light);
    assert_eq!(p.settings.curve, expected.settings.curve);
    assert_eq!(p.settings.detail, expected.settings.detail);
    assert_eq!(p.lin_key, expected.lin_key);
    assert_eq!(p.settings.profile, s.profile);
    assert_ne!(p.settings.light, DevelopSettings::default().light, "the profile contrast survives Light bypass");
    assert_eq!(p.settings.grading, expected.settings.grading);
    assert_eq!(s, saved, "planning cannot modify the catalog's settings");
}

#[test]
fn panel_toggles_preserve_profiles_in_direct_cached_and_deep_renders() {
    let src = source();
    let info = SourceInfo { raw: true, ..Default::default() };
    // Matte Soft covers the entire look being lost; Landscape and Portrait cover the old
    // finish-stage Effects gate suppressing profile clarity and texture after planning.
    for id in ["lc.muted.matte-soft", "lc.film.warm-print", "lc.landscape", "lc.portrait"] {
        for depth in [OutputDepth::U8, OutputDepth::U16] {
            let req = RenderRequest { depth, ..RenderRequest::fit(48, 32) };
            let cache = StageCache::default();
            let mut base = DevelopSettings::default();
            base.profile.id = id.into();
            base.profile.amount = 135.0;
            let expected = render(&src, &info, &base, &req);
            assert_ne!(expected.image, render(&src, &info, &DevelopSettings::default(), &req).image, "{id}: visible profile fixture");
            let original = base
                .merged(&json!({
                    "light": {"exposure": 1.5}, "curve": {"red": [{"x": 0, "y": 0.1}, {"x": 1, "y": 0.9}]},
                    "color": {"saturation": -40}, "mixer": {"red": {"hue": 60}},
                    "grading": {"shadows": {"hue": 200, "sat": 30}},
                    "effects": {"texture": 60, "clarity": 70}, "vignette": {"amount": -70}, "grain": {"amount": 60},
                    "detail": {"nr_color": 60}
                }))
                .unwrap();
            let on = render_cached(&src, &info, &original, &req, &cache);
            assert_ne!(on.image, expected.image, "{id}: visible user edits");
            let mut s = original.clone();
            for section in ["light", "color", "effects", "detail"] {
                s.set_section_enabled(section, false);
            }
            let saved = s.clone();
            let direct = render(&src, &info, &s, &req);
            let cached = render_cached(&src, &info, &s, &req, &cache);
            assert_eq!(direct.image, expected.image, "{id} {depth:?}: profile survives bypass");
            assert_eq!(direct.deep, expected.deep, "{id}: deep export keeps the profile");
            assert_eq!(cached.image, direct.image, "{id}: warm cache matches direct render");
            assert_eq!(cached.deep, direct.deep);
            assert_eq!(s, saved, "rendering preserves stored settings");
            for section in ["light", "color", "effects", "detail"] {
                s.set_section_enabled(section, true);
            }
            assert_eq!(s, original);
            let restored = render_cached(&src, &info, &s, &req, &cache);
            let restored_direct = render(&src, &info, &s, &req);
            assert_eq!(restored.image, on.image, "{id}: re-enable restores cached user edits");
            assert_eq!(restored.deep, on.deep);
            assert_eq!(restored_direct.image, on.image);
            assert_eq!(restored_direct.deep, on.deep);
        }
    }
}

#[test]
fn color_range_picker_and_mask_use_effective_exposure() {
    let src = Rgb32f::filled(16, 16, [0.05; 3]);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(16, 16);
    let center = Point::new(0.5, 0.5);
    let expected = crate::color_range_sample(&src, &info, &DevelopSettings::default(), &req, center).unwrap();
    for exposure in [-3.0, 3.0] {
        let mut s = DevelopSettings::default();
        s.light.exposure = exposure;
        let on = crate::color_range_sample(&src, &info, &s, &req, center).unwrap();
        assert_ne!(on, expected, "exposure must change the synthetic color sample");
        s.set_section_enabled("light", false);
        let sample = crate::color_range_sample(&src, &info, &s, &req, center).unwrap();
        assert_eq!(sample, expected, "the picker must use effective exposure zero");
        s.masks.push(Mask {
            id: 9,
            components: vec![MaskComponent {
                name: None,
                op: lightcraft_develop::MaskOp::Add,
                invert: false,
                shape: MaskShape::ColorRange { samples: vec![sample], refine: 0.0 },
            }],
            ..Default::default()
        });
        let overlay = crate::Overlay::Mask { id: 9, view: crate::visualize::MaskView::WhiteOnBlack, color: [255, 0, 0], opacity: 100 };
        let selected = render(&src, &info, &s, &RenderRequest { overlay, ..req });
        assert_eq!(selected.image.get(8, 8), [255; 4], "a tight mask must select the pixel sampled with Light disabled");
        s.set_section_enabled("light", true);
        assert_eq!(s.light.exposure, exposure);
        assert_eq!(crate::color_range_sample(&src, &info, &s, &req, center).unwrap(), on);
    }
}

#[test]
fn geometry_toggle_keeps_native_sizing_in_sync_with_crop_and_optics() {
    let src = Rgb32f::from_fn(100, 100, |x, y| [0.05 + x as f32 * 0.003, 0.05 + y as f32 * 0.003, 0.15]);
    let info = SourceInfo::default();
    for crop in [Rect::new(0.0, 0.0, 1.0, 1.0), Rect::new(0.025, 0.025, 0.975, 0.975)] {
        let mut s = DevelopSettings::default();
        s.crop.geometry.rect = crop;
        s.optics.distortion = -100.0;
        s.geometry.constrain_crop = true;
        let constrained = crate::native_output_size(100, 100, &s);
        let mut base = s.clone();
        base.geometry.constrain_crop = false;
        let expected = crate::native_output_size(100, 100, &base);
        assert!(constrained.0 < expected.0 && constrained.1 < expected.1, "active optics must constrain this crop fixture");
        s.set_section_enabled("geometry", false);
        let saved = s.clone();
        assert_eq!(crate::native_output_size(100, 100, &s), expected);
        let req = RenderRequest::fit(expected.0.round() as usize, expected.1.round() as usize);
        let p = plan(&src, &info, &s, &req);
        assert_eq!(p.frame.native_size(), expected);
        assert_eq!(crate::frame_for(&src, &info, &s, true).native_size(), expected, "stored-settings frame callers match the effective plan");
        assert!(p.frame.warp.is_some(), "independent optics must remain active");
        assert_eq!(p.settings.crop, base.crop);
        assert_eq!(render(&src, &info, &s, &req).image, render(&src, &info, &base, &req).image);
        assert_eq!(s, saved);
        s.set_section_enabled("geometry", true);
        assert_eq!(crate::native_output_size(100, 100, &s), constrained);
        assert_eq!(plan(&src, &info, &s, &req).frame.native_size(), constrained);
    }
}

#[test]
fn disabled_panels_preserve_crop_and_local_edits_in_deep_exports() {
    let src = source();
    let info = SourceInfo::default();
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(48, 32) };
    let mut base = DevelopSettings::default();
    base.crop.geometry.rect = Rect::new(0.1, 0.1, 0.9, 0.9);
    base.masks.push(Mask {
        components: vec![MaskComponent {
            name: None,
            op: lightcraft_develop::MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.1, 0.1), end: Point::new(0.9, 0.9) },
        }],
        adjust: lightcraft_develop::LocalAdjustments { exposure: 0.8, clarity: 40.0, texture: 30.0, ..Default::default() },
        ..Default::default()
    });
    let expected = render(&src, &info, &base, &req);
    let mut s = base.clone();
    s.light.exposure = 2.0;
    s.curve.master = vec![Point::new(0.0, 0.2), Point::new(1.0, 1.0)];
    s.color.saturation = -70.0;
    s.vignette.amount = -80.0;
    s.grain.amount = 90.0;
    for section in ["light", "color", "effects"] {
        s.set_section_enabled(section, false);
    }
    let off = render(&src, &info, &s, &req);
    assert!(off.deep.is_some());
    assert_eq!(off.image, expected.image);
    assert_eq!(off.deep, expected.deep, "deep exports bypass the same groups as previews");
    assert_eq!(s.crop, base.crop);
    assert_eq!(s.masks, base.masks);
}
