//! Film negative conversion through the whole renderer (`docs/negative-conversion.md`): a raw scan and a rendered
//! (JPEG / TIFF) scan of the same negative convert alike, every later slider acts on the positive the right way round,
//! and what reads the photo for Auto, the white balance selector and the camera's own looks sees the positive.

use lightcraft_develop::{BaseSource, DevelopSettings, FilmType, WbMode, controls};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

/// Orange-masked film base (linear transmittance) and each layer's density per decade of scene light.
const BASE: [f32; 3] = [0.70, 0.45, 0.25];
const GAMMA: [f32; 3] = [0.55, 0.62, 0.70];

fn on_film(l: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| BASE[i] * 10f32.powf(-GAMMA[i] * (l[i].max(1e-4) / 0.005).log10().max(0.0)))
}

/// A scanned negative: grey ramp (clearest in the middle) over three colour bands, a rebate along top and bottom.
fn negative() -> Rgb32f {
    let (w, h) = (240, 180);
    let mut img = Rgb32f::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let fy = y as f32 / h as f32;
            img.data[y * w + x] = if !(0.02..0.98).contains(&fy) {
                BASE
            } else {
                let l = 0.02 * 65f32.powf((2.0 * x as f32 / w as f32 - 1.0).abs());
                let t = match (fy * 3.0) as usize {
                    0 => [1.0, 1.0, 1.0],
                    1 => [1.0, 0.6, 0.4],
                    _ => [0.4, 0.8, 1.0],
                };
                on_film([l * t[0], l * t[1], l * t[2]])
            };
        }
    }
    img
}

fn converted() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.negative.film = FilmType::ColorNegative;
    s
}

/// The facts of a raw camera scan: a Kelvin white balance from a tungsten-ish light, the camera's colour model and
/// tone curve, and its own local tone mapping turned on in the profile.
fn raw_info() -> SourceInfo {
    use lightcraft_color::Mat3;
    let tags = lightcraft_raw::ColorData {
        illuminant: [17, 21],
        color_matrix: [
            Some(Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
            Some(Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
        ],
        ..Default::default()
    };
    let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.18f32.powi(i as i32);
        [x, 1.0 - (-2.5 * x).exp()]
    }))
    .unwrap();
    let local_tone = crate::local_tone::LocalTone {
        map: lightcraft_raw::gaintable::GainTableMap {
            points_v: 1,
            points_h: 2,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0, 0.0],
            gamma: 1.0,
            gains: vec![1.0, 4.0],
        },
        placement: lightcraft_raw::gaintable::SourcePlacement { rect: [0.0, 0.0, 1.0, 1.0], orientation: lightcraft_geom::Orientation::Normal },
    };
    SourceInfo {
        raw: true,
        as_shot_temp: 3442.0,
        as_shot_tint: 22.0,
        camera_color: Some(std::sync::Arc::new(crate::CameraColor { tags, developed_for: lightcraft_color::cct::temp_tint_to_xy(3442.0, 22.0) })),
        camera_tone: Some(curve),
        local_tone: Some(std::sync::Arc::new(local_tone)),
        ..Default::default()
    }
}

/// A raw scan's settings: the camera's as-shot white, its local tone mapping on.
fn raw_settings(mut s: DevelopSettings) -> DevelopSettings {
    (s.wb.mode, s.wb.temp, s.wb.tint) = (WbMode::AsShot, 3442.0, 22.0);
    s.profile.camera_local_tone = true;
    s
}

fn shot(img: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Rgba8 {
    render(img, info, s, &RenderRequest::fit(240, 240)).image
}

fn max_diff(a: &Rgba8, b: &Rgba8) -> i32 {
    a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |i| (p[i] as i32 - q[i] as i32).abs())).max().unwrap_or(0)
}

/// Mean (r, g, b) of a render, in 8-bit levels.
fn mean_rgb(img: &Rgba8) -> [f32; 3] {
    let n = img.data.len().max(1) as f32;
    [0, 1, 2].map(|i| img.data.iter().map(|p| p[i] as f32).sum::<f32>() / n)
}

/// The same negative scanned raw (camera tone curve, colour model, tungsten as-shot white, local tone mapping) and
/// rendered converts to the same positive: before the conversion used the scene tone map and relative white
/// balance, the raw scan rendered about 12 L* brighter through the camera's own curve.
#[test]
fn raw_and_rendered_scans_of_one_negative_convert_alike() {
    let img = negative();
    let rendered = shot(&img, &SourceInfo::default(), &converted());
    let raw = shot(&img, &raw_info(), &raw_settings(converted()));
    let d = max_diff(&rendered, &raw);
    assert!(d <= 2, "raw and rendered conversions differ by {d} levels");
    // and the same Custom white balance moves both alike (relative to the positive, not the camera's Kelvin)
    let warm = |mut s: DevelopSettings| {
        (s.wb.mode, s.wb.temp, s.wb.tint) = (WbMode::Custom, 7500.0, 10.0);
        s
    };
    let d = max_diff(&shot(&img, &SourceInfo::default(), &warm(converted())), &shot(&img, &raw_info(), &warm(raw_settings(converted()))));
    assert!(d <= 2, "a custom white balance differs by {d} levels between raw and rendered scans");
}

/// The camera's own local tone mapping (a gain map for the scene it photographed) never applies to a film scan.
#[test]
fn camera_local_tone_mapping_is_off_for_a_converted_scan() {
    let info = raw_info();
    let mut s = raw_settings(DevelopSettings::default());
    assert!(crate::local_tone::enabled(&info, &s));
    s.negative.film = FilmType::ColorNegative;
    assert!(!crate::local_tone::enabled(&info, &s));
}

/// Every slider after the conversion works on the positive: Exposure brightens, Temp warms, Tint turns magenta,
/// Blacks deepens, Saturation adds colour, for a raw and a rendered scan alike.
#[test]
fn later_sliders_act_on_the_positive_the_right_way_round() {
    let img = negative();
    for (info, base) in [(SourceInfo::default(), converted()), (raw_info(), raw_settings(converted()))] {
        let at = |id: &str, v: f64| {
            let mut s = base.clone();
            controls::set(&mut s, id, v);
            if id.starts_with("wb.") {
                s.wb.mode = WbMode::Custom;
            }
            mean_rgb(&shot(&img, &info, &s))
        };
        let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let mid = mean_rgb(&shot(&img, &info, &base));
        assert!(luma(at("light.exposure", 1.0)) > luma(mid) + 10.0, "Exposure +1 must brighten");
        assert!(luma(at("light.exposure", -1.0)) < luma(mid) - 10.0, "Exposure −1 must darken");
        assert!(luma(at("light.blacks", -80.0)) < luma(mid), "Blacks −80 must darken");
        let warm = at("wb.temp", 9000.0);
        assert!(warm[0] - warm[2] > mid[0] - mid[2] + 5.0, "a warmer Temp must warm: {warm:?} vs {mid:?}");
        let magenta = at("wb.tint", 60.0);
        assert!(magenta[1] < mid[1] - 2.0, "a magenta Tint must take green out: {magenta:?} vs {mid:?}");
        let spread = |c: [f32; 3]| c.iter().fold(0.0f32, |m, v| m.max(*v)) - c.iter().fold(255.0f32, |m, v| m.min(*v));
        assert!(spread(at("color.saturation", 80.0)) > spread(mid), "Saturation must add colour");
    }
}

/// Auto, Auto White Balance and the white balance selector read the positive ([`crate::negative::positive_source`]
/// with [`crate::local::wb_source`]): on a converted scan a grey reads neutral (as converted), not as the orange mask.
#[test]
fn auto_white_balance_reads_the_positive() {
    let img = negative();
    let s = raw_settings(converted());
    let info = crate::local::wb_source(&raw_info(), &s).into_owned();
    let positive = crate::negative::positive_source(&img, &s);
    let (t, tint) = crate::auto::auto_wb(&positive, &info);
    assert!((t - 6500.0).abs() < 600.0 && tint.abs() < 12.0, "auto white balance on the positive: {t} K / {tint}");
    // the scan itself, read as a scene, is far from neutral: what the selector used to see
    let (t0, tint0) = crate::auto::auto_wb(&img, &info);
    assert!((t0 - 6500.0).abs() > 1500.0 || tint0.abs() > 20.0, "the negative itself reads {t0} K / {tint0}");
}

/// A raw scan whose white balance gains lift the unexposed film above 1 in one channel is still film: only light at
/// the scan's own white level (each channel's clipping plateau) counts as clipped.
#[test]
fn a_raw_rebate_above_one_is_film_and_clipped_light_is_not() {
    let gain = [2.0f32, 1.0, 1.6];
    let mut img = negative();
    for p in &mut img.data {
        *p = [0, 1, 2].map(|i| p[i] * gain[i]);
    }
    // a clipped holder gap at the left: every channel at its white level (its gain)
    let w = img.width;
    for y in 0..img.height {
        for x in 0..4 {
            img.data[y * w + x] = gain;
        }
    }
    let s = converted();
    let a = crate::negative::analyze(&img, &s.negative, s.negative_region(), None).unwrap();
    assert_eq!(a.base_source, BaseSource::Detected, "{a:?}");
    let want = [0, 1, 2].map(|i| f64::from(BASE[i] * gain[i]));
    assert!(a.base.iter().zip(&want).all(|(b, w)| (b - w).abs() < 1e-3), "base {:?}, want {want:?}", a.base);
}

/// An analysis keeps the film type it was measured for: switching a converted colour negative to Slide (a preset, a
/// paste) measures again instead of reading the negative's densities as a slide's, which turned the picture black.
#[test]
fn switching_the_film_type_measures_again() {
    let img = negative();
    let mut s = converted();
    let a = crate::negative::analyze(&img, &s.negative, s.negative_region(), None).unwrap();
    assert_eq!(a.film, Some(FilmType::ColorNegative));
    s.negative.analysis = Some(a);
    s.negative.locked = true;
    s.negative.film = FilmType::Slide;
    assert!(!a.fits(FilmType::Slide) && a.fits(FilmType::ColorNegative));
    let stale = crate::negative::film_for(&s.negative, &a);
    let fresh = crate::negative::resolve(&img, &s).unwrap();
    assert_ne!(fresh, stale, "the slide is measured as a slide");
    let luma = |p: [f32; 3]| 0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2];
    let mid = img.data[img.width * img.height / 2 + img.width / 4];
    assert!(luma(fresh.convert(mid)) > 1e-3, "not black: {:?}", fresh.convert(mid));
    // an analysis from before the type was recorded still fits
    assert!(lightcraft_develop::FilmAnalysis { film: None, ..a }.fits(FilmType::Slide));
}

/// Film has grain, clipped light is flat: a raw rebate that is itself the brightest thing in the scan (lifted above 1
/// by white balance gains, no holder gap) is film, not clipping.
#[test]
fn an_unclipped_raw_rebate_that_is_the_brightest_thing_is_still_film() {
    let gain = [2.0f32, 1.0, 1.6];
    let mut img = negative();
    // a little grain everywhere, as real film has
    for (i, p) in img.data.iter_mut().enumerate() {
        let g = 1.0 + 0.01 * (((i * 2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
        *p = [0, 1, 2].map(|k| p[k] * gain[k] * g);
    }
    let s = converted();
    let a = crate::negative::analyze(&img, &s.negative, s.negative_region(), None).unwrap();
    assert_eq!(a.base_source, BaseSource::Detected, "{a:?}");
    let want = [0, 1, 2].map(|i| f64::from(BASE[i] * gain[i]));
    assert!(a.base.iter().zip(&want).all(|(b, w)| (b - w).abs() < 0.02 * w), "base {:?}, want {want:?}", a.base);
}

/// The densest film there is converts to the brightest white, never to black: the conversion's exponent is capped
/// before `exp2` overflows.
#[test]
fn the_densest_film_clips_to_white_not_black() {
    let f = crate::negative::resolve(&negative(), &converted()).unwrap();
    let luma = |p: [f32; 3]| 0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2];
    let dense = f.convert([1e-30; 3]);
    assert!(dense.iter().all(|c| c.is_finite()) && luma(dense) > luma(f.convert(on_film([1.3; 3]))), "{dense:?}");
}

/// A conversion that can't be measured (one flat colour) renders the scan exactly as with the conversion off: no
/// positive-only white balance or tone on an image that wasn't inverted.
#[test]
fn an_unmeasurable_conversion_renders_as_the_scan() {
    let flat = lightcraft_raster::Rgb32f::filled(120, 80, [0.4, 0.3, 0.2]);
    let on = shot(&flat, &raw_info(), &raw_settings(converted()));
    let off = shot(&flat, &raw_info(), &raw_settings(DevelopSettings::default()));
    assert_eq!(max_diff(&on, &off), 0);
}

/// A small crop of a large scan is sampled on its own grid: with the whole scan's stride (one sample every 4 px on
/// a 2048 px scan) a 31 × 20 px crop gave 40 samples, too few to measure.
#[test]
fn a_small_crop_is_measured_on_its_own_grid() {
    let small = negative();
    let (w, h) = (small.width * 8, small.height * 8);
    let mut img = lightcraft_raster::Rgb32f::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.data[y * w + x] = small.data[(y / 8) * small.width + x / 8];
        }
    }
    let s = converted();
    // (across the boundary of two colour bands, so it has tones to measure)
    let tiny = lightcraft_geom::Rect::new(0.2, 0.325, 0.2 + 31.0 / w as f64, 0.325 + 20.0 / h as f64);
    let r = crate::negative::analyze(&img, &s.negative, tiny, None);
    assert!(r.is_ok(), "{r:?}");
}
