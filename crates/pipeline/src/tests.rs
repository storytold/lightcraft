use dac_develop::{DevelopSettings, controls};
use dac_raster::Rgb32f;

use crate::{RenderRequest, SourceInfo, render};

fn mean_luma(img: &dac_raster::Rgba8) -> f32 {
    img.data.iter().map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).sum::<f32>() / img.len() as f32
}

fn scene() -> Rgb32f {
    dac_scenes::demo_library()[0].render(240, 160)
}

#[test]
fn default_render_is_sane() {
    let r = render(&scene(), &SourceInfo::default(), &DevelopSettings::default(), &RenderRequest::fit(240, 240));
    assert_eq!((r.image.width, r.image.height), (240, 160));
    let m = mean_luma(&r.image);
    assert!((40.0..220.0).contains(&m), "{m}");
    assert!(r.histogram.total > 0);
}

#[test]
fn grey_ramp_stays_neutral() {
    let src = Rgb32f::from_fn(64, 8, |x, _| [0.002 * 1.12f32.powi(x as i32); 3]);
    let r = render(&src, &SourceInfo::default(), &DevelopSettings::default(), &RenderRequest::fit(64, 8));
    for p in &r.image.data {
        assert!((p[0] as i32 - p[1] as i32).abs() <= 1 && (p[1] as i32 - p[2] as i32).abs() <= 1, "{p:?}");
    }
}

#[test]
fn every_slider_changes_or_keeps_output_without_panicking() {
    let src = scene();
    let base = render(&src, &SourceInfo::default(), &DevelopSettings::default(), &RenderRequest::fit(96, 96)).image;
    for c in controls::CONTROLS {
        for v in [c.min, c.max] {
            let mut s = DevelopSettings::default();
            controls::set(&mut s, c.id, v);
            let r = render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(96, 96));
            assert!(r.image.width > 0, "{}", c.id);
            let _ = &base;
        }
    }
}

#[test]
fn exposure_is_monotone() {
    let src = scene();
    let mut prev = -1.0;
    for ev in [-3.0, -1.5, 0.0, 1.0, 2.5] {
        let mut s = DevelopSettings::default();
        s.light.exposure = ev;
        let m = mean_luma(&render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(96, 96)).image);
        assert!(m > prev, "{ev}: {m} <= {prev}");
        prev = m;
    }
}

#[test]
fn directional_sliders() {
    let src = scene();
    let at = |id: &str, v: f64| {
        let mut s = DevelopSettings::default();
        controls::set(&mut s, id, v);
        mean_luma(&render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(96, 96)).image)
    };
    let base = at("light.exposure", 0.0);
    assert!(at("light.shadows", 100.0) > base);
    assert!(at("light.highlights", -100.0) < base);
    assert!(at("light.whites", 100.0) > base);
    assert!(at("light.blacks", -100.0) < base);
    assert!(at("vignette.amount", -100.0) < base);
    assert!(at("vignette.amount", 100.0) > base);
    assert!(at("effects.dehaze", -100.0) != base);
}

#[test]
fn resolution_independence_of_local_contrast() {
    // Clarity at two preview sizes should give similar results after downscaling.
    let src = dac_scenes::demo_library()[3].render(480, 320);
    let mut s = DevelopSettings::default();
    s.effects.clarity = 80.0;
    s.light.shadows = 60.0;
    let small = render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(120, 120)).image;
    let big = render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(480, 480)).image;
    let bl = big.to_linear();
    let down = dac_raster::resample::resize(&bl, small.width, small.height, dac_raster::resample::Filter::Box).to_srgb8();
    let err: f32 = small.data.iter().zip(&down.data).map(|(a, b)| (a[1] as f32 - b[1] as f32).abs()).sum::<f32>() / small.len() as f32;
    assert!(err < 9.0, "mean abs error {err}");
}

#[test]
fn crop_and_straighten_output_size() {
    let mut s = DevelopSettings::default();
    s.crop.geometry = dac_geom::crop_fit_angle(240.0, 160.0, 8.0, Some(1.0));
    let r = render(&scene(), &SourceInfo::default(), &s, &RenderRequest::fit(200, 200));
    assert_eq!((r.image.width, r.image.height), (200, 200));
}

#[test]
fn mask_brightens_only_inside() {
    use dac_develop::{Mask, MaskComponent, MaskOp, MaskShape};
    use dac_geom::Point;
    let src = Rgb32f::filled(100, 100, [0.1, 0.1, 0.1]);
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.2, angle: 0.0, feather: 10.0, invert: false },
        }],
        adjust: dac_develop::LocalAdjustments { exposure: 2.0, ..Default::default() },
        ..Default::default()
    });
    let r = render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(100, 100)).image;
    assert!(r.get(50, 50)[0] > r.get(5, 5)[0] + 40);
}

#[test]
#[ignore]
fn perf_report() {
    let src = dac_scenes::demo_library()[0].render(3000, 2000);
    let mut s = DevelopSettings::default();
    s.light.shadows = 40.0;
    s.effects.clarity = 20.0;
    s.effects.dehaze = 10.0;
    s.color.vibrance = 20.0;
    let t = std::time::Instant::now();
    let n = 5;
    for _ in 0..n {
        let _ = render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(2560, 1440));
    }
    eprintln!("render 2160x1440 from 6 MP: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
}

#[test]
fn unedited_rendered_source_is_passthrough() {
    // An sRGB ramp decoded to linear Rec.2020 must render back to (almost) the same 8-bit values.
    let src8 = dac_raster::Rgba8::from_fn(256, 4, |x, y| [x as u8, (x as u8).wrapping_add(y as u8 * 40), 255 - x as u8, 255]);
    let m = dac_color::SRGB.to_space(&dac_color::REC2020);
    let src = src8.to_linear().map(|c| m.apply_f32(c));
    let r = render(&src, &SourceInfo::default(), &DevelopSettings::default(), &RenderRequest::fit(256, 4)).image;
    let mut worst = 0i32;
    for (a, b) in src8.data.iter().zip(&r.data) {
        for c in 0..3 {
            worst = worst.max((a[c] as i32 - b[c] as i32).abs());
        }
    }
    assert!(worst <= 3, "max error {worst}");
}

fn typical_edits() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.light.highlights = -40.0;
    s.light.shadows = 30.0;
    s.effects.clarity = 15.0;
    s.effects.texture = 10.0;
    s.effects.dehaze = 12.0;
    s.detail.nr_luminance = 30.0;
    s.detail.nr_color = 25.0;
    s.detail.sharpen_amount = 40.0;
    s
}

fn max_diff(a: &dac_raster::Rgba8, b: &dac_raster::Rgba8) -> i32 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| (p[c] as i32 - q[c] as i32).abs()).max().unwrap_or(0)).max().unwrap_or(0)
}

#[test]
fn stage_cache_matches_uncached_render_through_a_slider_session() {
    use crate::{Quality, StageCache, render_cached};
    use std::sync::Arc;
    let src = Arc::new(scene());
    let info = SourceInfo { raw: true, ..Default::default() };
    let cache = StageCache::default();
    let full = RenderRequest::fit(200, 200);
    let draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(120, 120) };
    let mut s = typical_edits();
    // every kind of edit: tone, exposure, spatial amounts, NR, WB, crop, colour — at two sizes
    let steps: Vec<Box<dyn Fn(&mut DevelopSettings)>> = vec![
        Box::new(|s| s.light.contrast = 20.0),
        Box::new(|s| s.light.exposure = -0.7),
        Box::new(|s| s.light.highlights = -80.0),
        Box::new(|s| s.effects.clarity = -30.0),
        Box::new(|s| s.effects.dehaze = 0.0),
        Box::new(|s| s.effects.dehaze = 25.0),
        Box::new(|s| s.detail.nr_luminance = 60.0),
        Box::new(|s| {
            s.wb.mode = dac_develop::WbMode::Custom;
            s.wb.temp = 4000.0;
        }),
        Box::new(|s| s.crop.geometry.rect = dac_geom::Rect::new(0.1, 0.1, 0.9, 0.8)),
        Box::new(|s| s.color.saturation = 30.0),
        // optics and geometry (merged after the stage cache): defringe lives in the cached linear stage
        Box::new(|s| s.optics.defringe_purple_amount = 10.0),
        Box::new(|s| s.optics.distortion = 30.0),
        Box::new(|s| s.optics.remove_ca = true),
        Box::new(|s| s.geometry.vertical = 20.0),
    ];
    for (i, step) in steps.iter().enumerate() {
        step(&mut s);
        for req in [&full, &draft] {
            let a = render_cached(&src, &info, &s, req, &cache).image;
            let b = render(&src, &info, &s, req).image;
            assert_eq!(a, b, "step {i} {:?}", req.quality);
        }
    }
    assert_eq!(cache.len(), 2);
    // both sizes' sampled + linear images at least (12 bytes per pixel each), nothing after clear()
    let b = cache.bytes();
    assert!(b >= 2 * 12 * (200 * 133 + 120 * 80), "{b}");
    cache.clear();
    assert_eq!(cache.bytes(), 0);
    // a different source buffer never hits another source's entries
    let other = Arc::new(Rgb32f::filled(src.width, src.height, [0.3, 0.3, 0.3]));
    assert_eq!(render_cached(&other, &info, &s, &full, &cache).image, render(&other, &info, &s, &full).image);
}

#[test]
fn exposure_after_spatial_filters_equals_exposing_the_source() {
    // The pipeline filters the image before exposure and applies exposure per pixel; filters on
    // log luminance are shift-equivariant, so this must equal scaling the source.
    let src = scene();
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = typical_edits();
    s.light.exposure = 1.3;
    let a = render(&src, &info, &s, &RenderRequest::fit(200, 200)).image;
    let g = 2f32.powf(1.3);
    let scaled = src.map(|c| c.map(|v| v * g));
    s.light.exposure = 0.0;
    let b = render(&scaled, &info, &s, &RenderRequest::fit(200, 200)).image;
    let d = max_diff(&a, &b);
    assert!(d <= 2, "max difference {d}");
}

/// A section whose eye is off renders as if it were at its defaults (issue #316): Light, Color
/// and Detail, which the pipeline doesn't switch off where it applies them.
#[test]
fn sections_switched_off_render_as_defaults() {
    let src = Rgb32f::from_fn(32, 24, |x, y| [0.05 + x as f32 / 40.0, 0.1 + y as f32 / 30.0, 0.3]);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(32, 24);
    let plain = render(&src, &info, &DevelopSettings::default(), &req).image;
    let mut s = DevelopSettings::default();
    s.light.exposure = 1.5;
    s.light.contrast = 40.0;
    s.color.saturation = -60.0;
    s.color.vibrance = 30.0;
    s.detail.sharpen_amount = 120.0;
    let edited = render(&src, &info, &s, &req).image;
    assert_ne!(edited.data, plain.data);
    for section in ["light", "color", "detail"] {
        s.set_section_enabled(section, false);
    }
    assert_eq!(render(&src, &info, &s, &req).image.data, plain.data, "every edited section off = the unedited photo");
    // one section back on brings back only its own adjustments
    s.set_section_enabled("light", true);
    let mut light_only = DevelopSettings::default();
    light_only.light.exposure = 1.5;
    light_only.light.contrast = 40.0;
    assert_eq!(render(&src, &info, &s, &req).image.data, render(&src, &info, &light_only, &req).image.data);
    assert!(matches!(DevelopSettings::default().effective(), std::borrow::Cow::Borrowed(_)), "nothing is copied when every section is on");
}

#[test]
fn calibration_shifts_colours_but_keeps_greys() {
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest::fit(64, 8);
    // a grey ramp is untouched by the primaries
    let grey = Rgb32f::from_fn(64, 8, |x, _| [0.002 * 1.12f32.powi(x as i32); 3]);
    let mut s = DevelopSettings::default();
    s.calibration.red_hue = 80.0;
    s.calibration.green_sat = -60.0;
    let a = render(&grey, &info, &DevelopSettings::default(), &req).image;
    let b = render(&grey, &info, &s, &req).image;
    assert!(max_diff(&a, &b) <= 1);
    // a red patch rotates towards yellow (green rises relative to blue)
    let red = Rgb32f::filled(16, 16, [0.3, 0.04, 0.03]);
    let r0 = render(&red, &info, &DevelopSettings::default(), &RenderRequest::fit(16, 16)).image.data[0];
    let r1 = render(&red, &info, &s, &RenderRequest::fit(16, 16)).image.data[0];
    assert!(r1[1] as i32 - r1[2] as i32 > r0[1] as i32 - r0[2] as i32 + 5, "{r0:?} -> {r1:?}");
    // shadows tint: magenta (+) lowers green in a dark grey
    let dark = Rgb32f::filled(16, 16, [0.01, 0.01, 0.01]);
    s = DevelopSettings::default();
    s.calibration.shadows_tint = 100.0;
    let d = render(&dark, &info, &s, &RenderRequest::fit(16, 16)).image.data[0];
    assert!(d[1] + 3 < d[0], "{d:?}");
    // the section toggle disables it
    s.set_section_enabled("calibration", false);
    let off = render(&dark, &info, &s, &RenderRequest::fit(16, 16)).image.data[0];
    assert!(off[1].abs_diff(off[0]) <= 1, "{off:?}");
}

#[test]
fn refine_saturation_tames_a_contrast_curve() {
    let info = SourceInfo { raw: true, ..Default::default() };
    let src = Rgb32f::filled(16, 16, [0.12, 0.05, 0.03]);
    let req = RenderRequest::fit(16, 16);
    let sat = |p: [u8; 4]| p[0] as i32 - p[2] as i32;
    let mut s = DevelopSettings::default();
    let flat = render(&src, &info, &s, &req).image.data[0];
    s.curve.master =
        vec![dac_geom::Point::new(0.0, 0.0), dac_geom::Point::new(0.3, 0.15), dac_geom::Point::new(0.7, 0.85), dac_geom::Point::new(1.0, 1.0)];
    let full = render(&src, &info, &s, &req).image.data[0];
    s.curve.refine_saturation = 0.0;
    let refined = render(&src, &info, &s, &req).image.data[0];
    assert!(sat(full) > sat(refined), "{flat:?} {full:?} {refined:?}");
    // the curve's tone change stays
    let y = |p: [u8; 4]| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
    assert!((y(full) - y(refined)).abs() < 2.0);
}

#[test]
fn soft_proof_maps_into_the_proof_gamut_and_flags_what_does_not_fit() {
    use crate::{OutputSpace, Proof};
    let src = scene();
    let mut s = DevelopSettings::default();
    s.color.saturation = 100.0;
    s.color.vibrance = 100.0;
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = |space, proof| RenderRequest { space, proof, ..RenderRequest::fit(120, 120) };
    let red = |img: &dac_raster::Rgba8| img.data.iter().filter(|p| p[..3] == [255, 0, 0]).count();
    let blue = |img: &dac_raster::Rgba8| img.data.iter().filter(|p| p[2] == 255 && p[0] == 0 && p[1] < 80).count();

    // proofing sRGB on an sRGB render changes nothing but the warning
    let plain = render(&src, &info, &s, &req(OutputSpace::Srgb, None)).image;
    let same = render(&src, &info, &s, &req(OutputSpace::Srgb, Some(Proof { space: OutputSpace::Srgb, ..Default::default() }))).image;
    assert_eq!(plain, same);
    let warned =
        render(&src, &info, &s, &req(OutputSpace::Srgb, Some(Proof { space: OutputSpace::Srgb, dest_warning: true, display_warning: false }))).image;
    let n = red(&warned);
    assert!(n > 0 && n < warned.len(), "{n}");
    // …and nothing is flagged on an unsaturated edit of a neutral ramp
    let grey = Rgb32f::from_fn(64, 8, |x, _| [0.002 * 1.12f32.powi(x as i32); 3]);
    let g = render(
        &grey,
        &info,
        &DevelopSettings::default(),
        &RenderRequest { proof: Some(Proof { space: OutputSpace::Srgb, dest_warning: true, display_warning: true }), ..RenderRequest::fit(64, 8) },
    );
    assert_eq!(red(&g.image) + blue(&g.image), 0);

    // a wide-gamut render proofed for sRGB loses the colours sRGB can't hold
    let wide = render(&src, &info, &s, &req(OutputSpace::DisplayP3, None)).image;
    let proofed = render(&src, &info, &s, &req(OutputSpace::DisplayP3, Some(Proof { space: OutputSpace::Srgb, ..Default::default() }))).image;
    assert_ne!(wide, proofed);
    // proofing a wider space than the display: what the display can't show is flagged blue
    let pro =
        render(&src, &info, &s, &req(OutputSpace::Srgb, Some(Proof { space: OutputSpace::ProPhoto, dest_warning: true, display_warning: true })))
            .image;
    assert!(blue(&pro) > 0);
    assert!(red(&pro) < n, "ProPhoto holds more than sRGB");
}

/// A preview for a monitor with a display profile renders into the display's primaries: for a
/// display that *is* a standard space, exactly what an 8-bit render into that space gives.
#[test]
fn display_space_renders_like_the_matching_output_space() {
    use crate::{DisplaySpace, OutputSpace, Proof};
    let src = scene();
    let mut s = DevelopSettings::default();
    s.color.saturation = 100.0;
    s.color.vibrance = 100.0;
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = |space, display, proof| RenderRequest { space, display, proof, ..RenderRequest::fit(120, 120) };
    for space in [OutputSpace::Srgb, OutputSpace::DisplayP3] {
        let d = DisplaySpace::of(space, 7).unwrap();
        let want = render(&src, &info, &s, &req(space, None, None)).image;
        let got = render(&src, &info, &s, &req(OutputSpace::Srgb, Some(d), None)).image;
        assert!(max_diff(&got, &want) <= 1, "{space:?}: {}", max_diff(&got, &want));
    }
    // a wide display shows the saturated colours sRGB clips
    let srgb = render(&src, &info, &s, &req(OutputSpace::Srgb, None, None)).image;
    let p3 = render(&src, &info, &s, &req(OutputSpace::Srgb, Some(DisplaySpace::of(OutputSpace::DisplayP3, 1).unwrap()), None)).image;
    assert!(max_diff(&srgb, &p3) > 4);

    // soft proofing on a display: the display gamut warning is about *this* display
    let blue = |img: &dac_raster::Rgba8| img.data.iter().filter(|p| p[2] == 255 && p[0] == 0 && p[1] < 80).count();
    let pro = Some(Proof { space: OutputSpace::ProPhoto, dest_warning: false, display_warning: true });
    let on_srgb = blue(&render(&src, &info, &s, &req(OutputSpace::Srgb, Some(DisplaySpace::of(OutputSpace::Srgb, 2).unwrap()), pro)).image);
    let on_wide = blue(&render(&src, &info, &s, &req(OutputSpace::Srgb, Some(DisplaySpace::of(OutputSpace::Rec2020, 3).unwrap()), pro)).image);
    assert!(on_srgb > 0 && on_wide < on_srgb, "{on_srgb} {on_wide}");
    let plain = blue(&render(&src, &info, &s, &req(OutputSpace::Srgb, None, pro)).image);
    assert_eq!(plain, on_srgb, "an sRGB display warns like no display profile");
}

#[test]
fn display_space_rejects_degenerate_matrices() {
    use crate::DisplaySpace;
    assert!(DisplaySpace::new(dac_color::Mat3([[1.0, 1.0, 0.0], [1.0, 1.0, 0.0], [0.0, 0.0, 1.0]]), 1).is_none());
    assert!(DisplaySpace::new(dac_color::Mat3([[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]), 1).is_none());
    assert!(DisplaySpace::new(dac_color::Mat3::IDENTITY, 1).is_some());
}

#[test]
fn tint_negative_is_green_and_positive_is_magenta() {
    use dac_develop::WbMode;
    let src = Rgb32f::filled(16, 16, [0.18; 3]);
    // Rendered images, uncalibrated RAW and calibrated RAW with a nonzero As Shot tint.
    for info in [
        SourceInfo::default(),
        SourceInfo { raw: true, relative_wb: true, ..Default::default() },
        SourceInfo { raw: true, as_shot_temp: 4200.0, as_shot_tint: 15.0, ..Default::default() },
        // camera-space white balance: the same sign through the camera's own colour model
        SourceInfo { raw: true, as_shot_temp: 4200.0, as_shot_tint: 15.0, camera_color: Some(camera_color(4200.0, 15.0)), ..Default::default() },
    ] {
        let mut s = DevelopSettings::default();
        let neutral = render(&src, &info, &s, &RenderRequest::fit(16, 16)).image.get(8, 8);
        assert!((neutral[0] as i16 - neutral[1] as i16).abs() <= 1);
        assert!((neutral[2] as i16 - neutral[1] as i16).abs() <= 1);
        s.wb.mode = WbMode::Custom;
        s.wb.temp = info.as_shot_temp;
        for delta in [-50.0, 50.0] {
            s.wb.tint = info.as_shot_tint + delta;
            let p = render(&src, &info, &s, &RenderRequest::fit(16, 16)).image.get(8, 8);
            let magenta = (p[0] as f64 + p[2] as f64) / 2.0 - p[1] as f64;
            assert!(magenta * delta > 100.0, "delta {delta} must follow the green/magenta track, got {p:?}");
        }
    }
}

/// A dual-illuminant (A / D65) camera colour model, its pixels developed for `temp` / `tint`.
fn camera_color(temp: f64, tint: f64) -> std::sync::Arc<crate::CameraColor> {
    use dac_color::Mat3;
    let tags = dac_raw::ColorData {
        illuminant: [17, 21],
        color_matrix: [
            Some(Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
            Some(Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
        ],
        ..Default::default()
    };
    std::sync::Arc::new(crate::CameraColor { tags, developed_for: dac_color::cct::temp_tint_to_xy(temp, tint) })
}

/// Camera-space white balance: a raw source with its colour model is re-developed for the chosen
/// white, so a grey card lit by that white renders neutral. Without the model, or with a relative
/// white balance, the developed pixels are adapted (Bradford, neutral luminance kept).
#[test]
fn custom_white_balance_redevelops_in_camera_space() {
    use crate::local::wb_matrix_for;
    use dac_color::cct::temp_tint_to_xy;
    use dac_develop::WbMode;
    let cc = camera_color(5500.0, 0.0);
    let info = SourceInfo { raw: true, as_shot_temp: 5500.0, as_shot_tint: 0.0, camera_color: Some(cc.clone()), ..Default::default() };
    let mut s = DevelopSettings::default();
    (s.wb.mode, s.wb.temp, s.wb.tint) = (WbMode::Custom, 3200.0, 10.0);
    // a grey card lit by 3200 K / +10, as the camera records it, developed for the as-shot white
    let a = dac_raw::color::camera_transform_of(&cc.tags, cc.developed_for);
    let n = dac_raw::color::camera_neutral(&cc.tags, temp_tint_to_xy(3200.0, 10.0));
    let card = a.matrix.apply(std::array::from_fn(|i| n[i] * a.wb[i] as f64)).map(|v| v as f32);
    let apply = |m: [[f32; 3]; 3], c: [f32; 3]| -> [f32; 3] { std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]) };
    let m = wb_matrix_for(&info, &s).unwrap();
    let out = apply(m, card);
    assert!(out.iter().all(|v| (v / out[1] - 1.0).abs() < 1e-4), "{card:?} -> {out:?}");
    // the adaptation path for sources without a colour model, and for relative white balance
    let luma = |c: [f32; 3]| dac_color::luminance_2020(c);
    for other in [SourceInfo { camera_color: None, ..info.clone() }, SourceInfo { relative_wb: true, ..info.clone() }] {
        let b = wb_matrix_for(&other, &s).unwrap();
        assert_ne!(b, m);
        assert!((luma(apply(b, [1.0; 3])) - 1.0).abs() < 1e-4);
    }
    // the white-balance picker / auto white balance invert the same model: the card names its white
    let picked = crate::auto::auto_wb(&Rgb32f::filled(4, 4, card), &info);
    assert_eq!(picked, (3200.0, 10.0));
    // the as-shot white needs nothing
    s.wb.mode = WbMode::AsShot;
    assert!(wb_matrix_for(&info, &s).is_none());
}

/// Every stored process number renders with a process this build knows: settings saved before
/// process versions existed exactly as V1, and a number from a newer version of the app exactly as the
/// latest process here (`docs/process-versions.md`). Raw (base tone curve), camera-tone and
/// rendered sources, plain and edited, 8-bit and 16-bit.
#[test]
fn stored_process_numbers_render_with_a_known_process() {
    use dac_develop::{Process, ProcessVersion};
    let src = scene();
    let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.18f32.powi(i as i32);
        [x, 1.0 - (-2.0 * x).exp()]
    }))
    .unwrap();
    let infos = [
        SourceInfo::default(),
        SourceInfo { raw: true, as_shot_temp: 5200.0, as_shot_tint: 4.0, ..Default::default() },
        SourceInfo { raw: true, relative_wb: true, camera_tone: Some(curve), ..Default::default() },
    ];
    let mut edited = DevelopSettings::for_raw(5200.0, 4.0);
    for (id, v) in [("light.exposure", 0.4), ("light.contrast", 30.0), ("light.highlights", -60.0), ("light.whites", 15.0), ("light.blacks", -20.0)] {
        controls::set(&mut edited, id, v);
    }
    let legacy = |s: &DevelopSettings| {
        let mut v = s.to_json();
        v.as_object_mut().unwrap().remove("process");
        DevelopSettings::from_json(&v).unwrap()
    };
    let with = |s: &DevelopSettings, p: ProcessVersion| DevelopSettings { process: p, ..s.clone() };
    let deep = RenderRequest { depth: crate::OutputDepth::U16, ..RenderRequest::fit(96, 96) };
    for info in &infos {
        for s in [DevelopSettings::default(), edited.clone()] {
            for req in [RenderRequest::fit(96, 96), deep] {
                let out = |s: &DevelopSettings| {
                    let r = render(&src, info, s, &req);
                    (r.image.data, r.deep)
                };
                let v1 = out(&with(&s, ProcessVersion::V1));
                assert_eq!(legacy(&s).process, ProcessVersion::V1);
                assert!(out(&legacy(&s)) == v1, "saved before process versions: rendered as V1");
                let latest = out(&with(&s, Process::LATEST.version()));
                assert!(out(&with(&s, ProcessVersion(Process::LATEST.version().0 + 7))) == latest, "newer than this build: as the latest");
            }
        }
    }
}
