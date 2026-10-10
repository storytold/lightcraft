use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Treatment, WbMode};
use lightcraft_geom::Point;
use lightcraft_raster::Rgb32f;

use crate::{RenderRequest, SourceInfo, StageCache, render, render_cached};

#[test]
fn section_bypass_matches_neutral_render_and_restores_cached_edits() {
    let src =
        Arc::new(Rgb32f::from_fn(160, 80, |x, y| [0.02 + x as f32 / 200.0, 0.02 + y as f32 / 110.0, if (x + y) % 3 == 0 { 0.5 } else { 0.05 }]));
    let info = SourceInfo::default();
    let req = RenderRequest::fit(160, 80);
    let base = render(&src, &info, &DevelopSettings::default(), &req).image;
    for section in ["light", "color", "detail", "curve", "mixer", "grading", "grain", "vignette"] {
        let mut s = DevelopSettings::default();
        match section {
            "light" => {
                s.light.exposure = 1.0;
                s.light.shadows = 40.0;
                s.curve.master = vec![Point::new(0.0, 0.05), Point::new(1.0, 0.9)].into();
            }
            "color" => {
                s.wb.mode = WbMode::Custom;
                s.wb.temp = 8000.0;
                s.color.saturation = -30.0;
                s.mixer.red.lum = 30.0;
                s.grading.shadows.sat = 40.0;
            }
            "detail" => {
                s.detail.nr_color = 100.0;
                s.detail.nr_luminance = 60.0;
                s.detail.sharpen_amount = 100.0;
            }
            "curve" => s.curve.master = vec![Point::new(0.0, 0.1), Point::new(1.0, 0.8)].into(),
            "mixer" => s.mixer.red.lum = 60.0,
            "grading" => s.grading.shadows.sat = 80.0,
            "grain" => s.grain.amount = 100.0,
            "vignette" => s.vignette.amount = -100.0,
            _ => panic!("unknown test section {section}"),
        }
        let original = s.clone();
        let cache = StageCache::default();
        let edited = render_cached(&src, &info, &s, &req, &cache).image;
        assert_ne!(edited.data, base.data, "fixture must exercise {section}");
        s.set_section_enabled(section, false);
        let disabled = s.clone();
        assert_eq!(render_cached(&src, &info, &s, &req, &cache).image.data, base.data, "bypass {section}");
        assert_eq!(s, disabled, "render must not erase the saved edits");
        s.set_section_enabled(section, true);
        assert_eq!(s, original);
        assert_eq!(render_cached(&src, &info, &s, &req, &cache).image.data, edited.data, "restore {section}");
    }
}

#[test]
fn color_bypass_keeps_bw_treatment_and_unrelated_light_edit() {
    let src = Rgb32f::from_fn(32, 16, |x, y| [0.1 + x as f32 / 60.0, y as f32 / 30.0, 0.02]);
    let mut expected = DevelopSettings { treatment: Treatment::Bw, ..Default::default() };
    expected.light.exposure = 1.0;
    let mut s = expected.clone();
    s.bw_mix.red = 70.0;
    s.grading.highlights.sat = 50.0;
    s.set_section_enabled("color", false);
    let req = RenderRequest::fit(32, 16);
    let info = SourceInfo::default();
    assert_eq!(render(&src, &info, &s, &req).image.data, render(&src, &info, &expected, &req).image.data);
}

#[test]
fn light_bypass_color_range_sample_matches_rendered_exposure() {
    let src = Rgb32f::filled(16, 16, [0.08, 0.15, 0.3]);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(16, 16);
    let center = Point::new(0.5, 0.5);
    let base = DevelopSettings::default();
    let expected = crate::color_range_sample(&src, &info, &base, &req, center);
    let mut s = base.clone();
    s.light.exposure = 2.0;
    assert_ne!(crate::color_range_sample(&src, &info, &s, &req, center), expected);
    s.set_section_enabled("light", false);
    assert_eq!(crate::color_range_sample(&src, &info, &s, &req, center), expected);
}

#[test]
fn disabled_sections_stay_bypassed_after_creative_profile_deltas() {
    let src = Rgb32f::from_fn(96, 48, |x, y| [0.02 + x as f32 / 120.0, 0.02 + y as f32 / 80.0, 0.08]);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(96, 48);
    for id in ["lc.film.warm-print", "lc.film.faded-slide", "lc.bw.sepia", lightcraft_develop::IMPORTED_LOOK_ID] {
        let mut s = DevelopSettings::default();
        s.profile.id = id.into();
        s.profile.amount = 150.0;
        if id == lightcraft_develop::IMPORTED_LOOK_ID {
            s.profile.imported = Some(lightcraft_develop::ImportedLook { convert_to_grayscale: true, contrast: 50.0, ..Default::default() });
        }
        s.light.exposure = 1.0;
        s.color.saturation = 30.0;
        s.curve.master = vec![Point::new(0.0, 0.1), Point::new(1.0, 0.85)].into();
        s.set_section_enabled("light", false);
        s.set_section_enabled("color", false);
        let saved = s.clone();
        let expected = DevelopSettings { treatment: if s.is_bw() { Treatment::Bw } else { Treatment::Color }, ..Default::default() };
        assert_eq!(render(&src, &info, &s, &req).image.data, render(&src, &info, &expected, &req).image.data, "{id}");
        assert_eq!(s, saved, "bypass must preserve all saved profile/slider/curve values");
    }
}
