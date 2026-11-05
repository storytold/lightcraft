//! Auto tone (a Lightroom-like recipe, see [`auto_tone`]) and auto white balance (grey-world
//! statistics on a proxy).

use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_color::{REC2020, Xy, bradford, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::Rgb32f;
use serde::Serialize;

use crate::SourceInfo;
use crate::local::effective_wb;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct AutoTone {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub vibrance: f64,
    pub saturation: f64,
}

/// Exposure in EV that Auto adds per EV of the scene's log-average luminance (the slope is negative:
/// darker scenes get more). The slope is Lightroom's: regressing the Exposure its Auto chose on 24
/// raw photos against our own [`scene_log_mean`] of each gives -0.41 EV/EV (r = -0.92).
const EXPOSURE_SLOPE: f64 = -0.41;
/// Exposure for a scene whose log-average luminance is middle grey: fitted, with [`CONTRAST`] and
/// [`VIBRANCE`], so our renders match Lightroom's Auto results on those photos (see [`auto_tone`]).
const EXPOSURE_AT_GREY: f64 = -0.58;
/// Highlights and Shadows on a typical photo are Lightroom's own: the average its Auto chose on the
/// 24 photos (our sliders aim at the same as its). Each photo scales them by how much of its frame is
/// bright or dark against [`BRIGHT_SHARE_TYPICAL`] / [`DARK_SHARE_TYPICAL`] (see [`auto_tone`]).
const HIGHLIGHTS: f64 = -66.0;
const SHADOWS: f64 = 52.0;
/// Contrast and Vibrance on a typical photo are fitted: our slider scales differ from Lightroom's,
/// and our base tone is flatter, so Contrast is far above its +6. Contrast then follows the photo's
/// luminance spread around [`SPREAD_TYPICAL`]; Lightroom leaves Saturation at about 0.
const CONTRAST: f64 = 68.0;
const VIBRANCE: f64 = 30.0;
/// Share of the frame (after Auto's exposure) above [`BRIGHT_Y`] / below [`DARK_Y`] scene-linear
/// that gets the typical Highlights / Shadows. Own design: a sky-and-land frame has about this much.
const BRIGHT_Y: f32 = 0.2;
const DARK_Y: f32 = 0.045;
const BRIGHT_SHARE_TYPICAL: f64 = 0.15;
const DARK_SHARE_TYPICAL: f64 = 0.35;
/// Luminance spread (EV between the 5th and 95th percentile, after exposure) of a typical photo, and
/// the Contrast added per EV the photo is flatter than that.
const SPREAD_TYPICAL: f64 = 4.5;
const CONTRAST_PER_EV: f64 = 8.0;
/// Where the base tone puts white and black (scene-linear luminance, roughly): Whites and Blacks aim
/// the photo's 99.5th / 0.5th percentile (after exposure) just short of them, a few points per EV.
const WHITE_Y: f64 = 0.45;
const BLACK_Y: f64 = 0.004;
const WHITES_PER_EV: f64 = 12.0;
const BLACKS_PER_EV: f64 = 8.0;

/// The valid scene-linear luminances of `src` under the current white balance, from a 512 px proxy
/// (before any tone curve). Pixels that aren't finite (a damaged decode) are left out: they get no
/// weight when the proxy is made, and proxy pixels made only of them are skipped. Empty when no
/// pixel is valid.
fn scene_luminances(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Vec<f32> {
    let (mut clean, mut weight) = (src.clone(), Rgb32f::new(src.width, src.height));
    let mut any = false;
    for (p, w) in clean.data.iter_mut().zip(weight.data.iter_mut()) {
        if p.iter().all(|v| v.is_finite()) {
            *w = [1.0; 3];
            any = true;
        } else {
            *p = [0.0; 3];
        }
    }
    if !any {
        return Vec::new();
    }
    let fit = |img: &Rgb32f| lightcraft_raster::resample::fit(img, 512, 512, lightcraft_raster::resample::Filter::Box);
    let (img, weight) = (fit(&clean), fit(&weight));
    // the mean of the valid pixels under each proxy pixel
    let kept: Vec<[f32; 3]> = img.data.iter().zip(&weight.data).filter(|(_, w)| w[0] > 1e-6).map(|(p, w)| p.map(|v| v / w[0])).collect();
    let mut kept = Rgb32f { width: kept.len(), height: 1, data: kept };
    let base = DevelopSettings { wb: s.wb, process: s.process, ..DevelopSettings::default() };
    // (white balance is per pixel: the valid pixels can be balanced in a row of their own)
    crate::local::scene_linear_pre(&mut kept, info, &base);
    kept.data.iter().map(|c| luminance_2020(*c).max(1e-6)).filter(|y| y.is_finite()).collect()
}

fn log_mean(ys: &[f32]) -> Option<f64> {
    let sum: f64 = ys.iter().map(|y| (*y as f64 / 0.18).log2()).sum();
    (!ys.is_empty()).then(|| sum / ys.len() as f64)
}

/// The scene's log-average luminance under the current white balance, in EV relative to middle grey
/// (the mean of log2(Y / 0.18) over a 512 px proxy, scene-linear, before any tone curve). Pixels that
/// aren't finite are left out. `None` when no pixel is valid.
pub fn scene_log_mean(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<f64> {
    log_mean(&scene_luminances(src, info, s))
}

/// Auto settings for `src` under the current white balance (ignores current tone values), aiming at
/// what Lightroom's Auto does: the Exposure a scene needs follows its log-average luminance
/// ([`scene_log_mean`], scene-linear, so the rule doesn't depend on the base tone curve). The other
/// sliders follow the photo's luminance distribution after that exposure:
/// - Highlights and Shadows scale with the share of the frame that is bright / dark (the square root
///   of its ratio to a typical photo's share), from about a fifth to one and a half of the typical
///   values;
/// - Contrast rises for flat photos and falls for contrasty ones (the 5th–95th percentile spread);
/// - Whites and Blacks bring the 99.5th / 0.5th percentile just short of white / black: a photo whose
///   brightest tones sit well below white gets positive Whites, one with much clipped gets negative;
/// - Vibrance stays at the calibrated value, a little less for photos that need a lot of Shadows
///   (lifting shadows already adds colour); Saturation 0.
///
/// On a typical photo these give the recipe calibrated black-box against Lightroom's observed
/// behaviour (the values its Auto chose and its exported results) on 24 Sony raw photos: Exposure
/// -0.58 - 0.41·log-mean, Contrast 68, Highlights -66, Shadows 52, Vibrance 30. Held out one photo at
/// a time, that recipe came out at a median dE76 of 6.4 from Lightroom's Auto results on the 16 photos
/// whose starting look Sony's DRO doesn't brighten. Rendered sources (JPEGs) are already toned, so
/// their log-average reads brighter and Auto changes their exposure less. A source without a single
/// valid pixel gets Exposure 0 and the typical values.
pub fn auto_tone(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> AutoTone {
    let mut ys = scene_luminances(src, info, s);
    let exposure = log_mean(&ys).map_or(0.0, |m| (EXPOSURE_AT_GREY + EXPOSURE_SLOPE * m).clamp(-2.0, 3.0));
    let exposure = (exposure * 100.0).round() / 100.0;
    let typical = AutoTone {
        exposure,
        contrast: CONTRAST,
        highlights: HIGHLIGHTS,
        shadows: SHADOWS,
        whites: 0.0,
        blacks: 0.0,
        vibrance: VIBRANCE,
        saturation: 0.0,
    };
    if ys.is_empty() {
        return typical;
    }
    let gain = 2f32.powf(exposure as f32);
    for y in &mut ys {
        *y *= gain;
    }
    ys.sort_by(|a, b| a.total_cmp(b));
    let n = ys.len();
    let pct = |q: f64| ys.get(((n - 1) as f64 * q).round() as usize).copied().unwrap_or(0.18).max(1e-6) as f64;
    let share = |f: &dyn Fn(f32) -> bool| ys.iter().filter(|y| f(**y)).count() as f64 / n as f64;
    let (bright, dark) = (share(&|y| y > BRIGHT_Y), share(&|y| y < DARK_Y));
    let scale = |s: f64, typical: f64| (s / typical).sqrt().clamp(0.2, 1.5);
    let highlights = (HIGHLIGHTS * scale(bright, BRIGHT_SHARE_TYPICAL)).clamp(-100.0, 0.0);
    let shadows = (SHADOWS * scale(dark, DARK_SHARE_TYPICAL)).clamp(0.0, 100.0);
    let spread = (pct(0.95) / pct(0.05)).log2();
    let contrast = (CONTRAST + CONTRAST_PER_EV * (SPREAD_TYPICAL - spread)).clamp(0.0, 100.0);
    let whites = (WHITES_PER_EV * (WHITE_Y / pct(0.995)).log2()).clamp(-60.0, 60.0);
    let blacks = (-BLACKS_PER_EV * (pct(0.005) / BLACK_Y).log2()).clamp(-60.0, 40.0);
    let vibrance = (VIBRANCE - 0.1 * (shadows - SHADOWS).max(0.0)).clamp(0.0, 100.0);
    AutoTone {
        contrast: contrast.round(),
        highlights: highlights.round(),
        shadows: shadows.round(),
        whites: whites.round(),
        blacks: blacks.round(),
        vibrance: vibrance.round(),
        ..typical
    }
}

/// An automatic black & white mix (slider values, red … magenta) for `src` under `s`'s white
/// balance and tone: each hue band's colourful pixels are pushed away from the image's mean
/// lightness — bands brighter than average get brighter, darker ones darker — so areas that
/// differ only in colour stay apart in grey. Bands with almost no colourful pixels stay at 0.
pub fn auto_bw_mix(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> [f64; 8] {
    use lightcraft_color::perceptual::{lab_to_lch, oklab_from_2020};
    let mut img = lightcraft_raster::resample::fit(src, 512, 512, lightcraft_raster::resample::Filter::Box);
    let base = DevelopSettings { wb: s.wb, light: s.light, process: s.process, ..DevelopSettings::default() };
    crate::local::scene_linear_pre(&mut img, info, &base);
    let gain = 2f32.powf(base.light.exposure as f32);
    let (mut mass, mut sum_l) = ([0f64; 8], [0f64; 8]);
    let (mut all_l, mut n) = (0f64, 0usize);
    for p in &img.data {
        let lch = lab_to_lch(oklab_from_2020(p.map(|v| (v * gain).max(0.0))));
        all_l += lch[0] as f64;
        n += 1;
        let k = (lch[1] / 0.2).min(1.0) as f64; // as in the B&W conversion
        if k < 0.05 {
            continue;
        }
        let w = crate::colorops::band_weights(lch[2]);
        for i in 0..8 {
            mass[i] += w[i] as f64 * k;
            sum_l[i] += w[i] as f64 * k * lch[0] as f64;
        }
    }
    if n == 0 {
        return [0.0; 8];
    }
    let mean = all_l / n as f64;
    let total: f64 = mass.iter().sum();
    std::array::from_fn(|i| {
        if total <= 0.0 || mass[i] / total < 0.01 {
            return 0.0;
        }
        let sep = sum_l[i] / mass[i] - mean;
        (sep * 400.0).clamp(-60.0, 60.0).round()
    })
}

/// Grey-world white balance weighted towards mid-tone, low-chroma pixels. Returns (temp, tint).
pub fn auto_wb(src: &Rgb32f, info: &SourceInfo) -> (f64, f64) {
    let img = lightcraft_raster::resample::fit(src, 256, 256, lightcraft_raster::resample::Filter::Box);
    let (mut acc, mut wsum) = ([0.0f64; 3], 0.0f64);
    for c in &img.data {
        let y = luminance_2020(*c);
        if !(0.01..=2.0).contains(&y) {
            continue;
        }
        let mx = c[0].max(c[1]).max(c[2]);
        let mn = c[0].min(c[1]).min(c[2]);
        let chroma = (mx - mn) / (mx + 1e-6);
        let w = (1.0 - chroma).powi(2) as f64 * (1.0 - ((y.log2() + 2.5) / 4.0).abs().min(1.0)) as f64;
        for i in 0..3 {
            acc[i] += c[i] as f64 * w;
        }
        wsum += w;
    }
    if wsum <= 0.0 {
        return (info.as_shot_temp, info.as_shot_tint);
    }
    let avg = acc.map(|v| v / wsum);
    // the white that makes `avg` neutral, through the same model the white balance renders with
    // (`local::wb_matrix_for`): the camera's own colour model, else a Bradford adaptation
    let camera = info.camera_color.as_deref().filter(|_| info.raw && !info.relative_wb);
    let seen = match camera.and_then(|cc| lightcraft_raw::color::neutral_white(&cc.tags, cc.developed_for, avg)) {
        Some(white) => white,
        None => {
            let shot = lightcraft_color::cct::temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint);
            Xy::from_xyz(bradford(REC2020.white, shot).apply(REC2020.to_xyz().apply(avg)))
        }
    };
    let (t, tint) = xy_to_temp_tint(seen);
    (t.clamp(2000.0, 50000.0).round(), tint.clamp(-150.0, 150.0).round())
}

/// Temperature/tint currently in effect (for UI display).
pub fn current_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    effective_wb(info, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_ranges(a: &AutoTone) -> bool {
        let fields = [a.exposure, a.contrast, a.highlights, a.shadows, a.whites, a.blacks, a.vibrance, a.saturation];
        fields.iter().all(|v| v.is_finite())
            && (-2.0..=3.0).contains(&a.exposure)
            && (0.0..=100.0).contains(&a.contrast)
            && (-100.0..=0.0).contains(&a.highlights)
            && (0.0..=100.0).contains(&a.shadows)
            && (-100.0..=100.0).contains(&a.whites)
            && (-100.0..=100.0).contains(&a.blacks)
    }

    #[test]
    fn exposure_follows_the_log_average_luminance() {
        let flat = |y: f32| Rgb32f::filled(64, 64, [y; 3]);
        let auto = |img: &Rgb32f| auto_tone(img, &SourceInfo::default(), &DevelopSettings::default());
        let log_mean = |img: &Rgb32f| scene_log_mean(img, &SourceInfo::default(), &DevelopSettings::default()).unwrap();
        // a scene at middle grey gets the exposure at grey; each EV darker adds 0.41 EV
        assert!(log_mean(&flat(0.18)).abs() < 1e-3);
        assert_eq!(auto(&flat(0.18)).exposure, -0.58);
        assert!((auto(&flat(0.18 / 8.0)).exposure - (-0.58 + 3.0 * 0.41)).abs() < 1e-9);
        // darker scenes get more, brighter ones less, within -2..3 EV
        let ramp = |lo: f32, step: f32| Rgb32f::from_fn(64, 64, |x, _| [lo + x as f32 * step; 3]);
        let (dark, grey, bright) = (auto(&ramp(0.01, 0.0003)), auto(&ramp(0.15, 0.001)), auto(&ramp(0.8, 0.01)));
        assert!(dark.exposure > 0.5 && bright.exposure < -1.0, "{dark:?} {bright:?}");
        assert!(dark.exposure > grey.exposure && grey.exposure > bright.exposure, "{dark:?} {grey:?} {bright:?}");
        assert_eq!(auto(&flat(1e-9)).exposure, 3.0);
        assert_eq!(auto(&flat(1e6)).exposure, -2.0);
        // every setting stays within its slider range
        for a in [dark, grey, bright, auto(&flat(1e-9)), auto(&flat(1e6))] {
            assert!(in_ranges(&a), "{a:?}");
        }
    }

    #[test]
    fn pixels_that_are_not_finite_are_left_out() {
        let auto = |img: &Rgb32f| auto_tone(img, &SourceInfo::default(), &DevelopSettings::default());
        // half middle grey, half NaN (or infinite): the grey half alone decides, as if the rest weren't there
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let half = Rgb32f::from_fn(64, 64, |x, _| if x < 32 { [0.18; 3] } else { [bad; 3] });
            assert_eq!(auto(&half).exposure, -0.58, "{bad}");
            // also through the resampling of a source larger than the proxy (proxy pixels on the border mix both halves)
            let big = Rgb32f::from_fn(1030, 700, |x, _| if x < 515 { [0.045; 3] } else { [bad; 3] });
            assert!((auto(&big).exposure - (-0.58 + 2.0 * 0.41)).abs() < 1e-9, "{bad}");
            // a few broken pixels scattered over a grey image change nothing
            let speckled = Rgb32f::from_fn(1030, 700, |x, y| if (x * 7 + y * 13) % 97 == 0 { [bad; 3] } else { [0.18; 3] });
            assert_eq!(auto(&speckled).exposure, -0.58, "{bad}");
            assert!(in_ranges(&auto(&speckled)));
        }
        // with no valid pixel at all (or none at all) there is nothing to measure: Exposure 0, the typical values
        for img in [Rgb32f::filled(8, 8, [f32::NAN; 3]), Rgb32f::filled(600, 600, [f32::INFINITY; 3]), Rgb32f::new(0, 0)] {
            let a = auto(&img);
            assert_eq!((a.exposure, a.contrast, a.vibrance), (0.0, 68.0, 30.0));
        }
        // pure black still gives usable values
        assert!(in_ranges(&auto(&Rgb32f::filled(32, 32, [0.0; 3]))));
    }

    #[test]
    fn neutral_image_keeps_as_shot_wb() {
        let img = Rgb32f::from_fn(32, 32, |x, y| [0.05 + (x + y) as f32 * 0.004; 3]);
        let (t, tint) = auto_wb(&img, &SourceInfo::default());
        assert!((t - 6500.0).abs() < 150.0, "{t}");
        assert!(tint.abs() < 6.0, "{tint}");
    }

    #[test]
    fn blue_cast_is_corrected_by_higher_temp() {
        // A bluish cast should be neutralised by telling the pipeline the light was bluer (higher K).
        let img = Rgb32f::from_fn(32, 32, |_, _| [0.16, 0.18, 0.24]);
        let (t, _) = auto_wb(&img, &SourceInfo::default());
        assert!(t > 7000.0, "{t}");
        let mut s = DevelopSettings::default();
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = t;
        let (t2, tint2) = auto_wb(&img, &SourceInfo::default());
        let _ = (t2, tint2);
        let mut out = img.clone();
        let (tt, ti) = auto_wb(&img, &SourceInfo::default());
        s.wb.temp = tt;
        s.wb.tint = ti;
        crate::local::scene_linear_pre(&mut out, &SourceInfo::default(), &s);
        let c = out.get(0, 0);
        assert!((c[0] - c[2]).abs() < 0.02, "{c:?}");
    }
}

#[cfg(test)]
mod bw_tests {
    use super::*;

    #[test]
    fn auto_bw_mix_pushes_light_and_dark_hues_apart() {
        // left: a light yellow, right: a dark blue (scene-linear Rec. 2020)
        let src = Rgb32f::from_fn(64, 32, |x, _| if x < 32 { [0.55, 0.5, 0.05] } else { [0.01, 0.02, 0.12] });
        let m = auto_bw_mix(&src, &SourceInfo::default(), &DevelopSettings::default());
        let (yellow, blue) = (m[2], m[5]);
        assert!(yellow > 0.0 && blue < 0.0, "{m:?}");
        // an image without colour leaves the mix alone
        let grey = Rgb32f::from_fn(16, 16, |x, _| [x as f32 / 16.0; 3]);
        assert_eq!(auto_bw_mix(&grey, &SourceInfo::default(), &DevelopSettings::default()), [0.0; 8]);
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    #[test]
    fn auto_wb_and_picker_correct_green_with_positive_tint() {
        // The picker delegates a sampled patch to this same auto_wb implementation.
        for (rgb, sign) in [([0.18, 0.24, 0.18], 1.0), ([0.24, 0.18, 0.24], -1.0)] {
            let img = Rgb32f::filled(16, 16, rgb);
            let info = SourceInfo { raw: true, relative_wb: true, ..Default::default() };
            let (temp, tint) = auto_wb(&img, &info);
            assert!(tint * sign > 0.0, "{rgb:?}: temp {temp}, tint {tint}");
            let mut s = DevelopSettings::default();
            s.wb.mode = lightcraft_develop::WbMode::Custom;
            s.wb.temp = temp;
            s.wb.tint = tint;
            let mut corrected = img;
            crate::local::white_balance(&mut corrected, &info, &s);
            let p = corrected.get(0, 0);
            let spread = p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
            assert!(spread < 0.003, "{rgb:?} -> {p:?}");
        }
    }
}

#[cfg(test)]
mod per_photo_tests {
    use super::*;

    fn auto_for(name: &str) -> AutoTone {
        let scene = lightcraft_scenes::demo_library().into_iter().find(|s| s.name == name).unwrap();
        auto_tone(&scene.render_fit(384), &SourceInfo::default(), &DevelopSettings::default())
    }

    #[test]
    fn very_different_photos_get_different_tone_settings() {
        // a dark night sky against sunlit canyon rock: the night has more dark frame to open up
        let (night, canyon) = (auto_for("Aurora over the ridge"), auto_for("Canyon walls"));
        assert!(night.shadows > canyon.shadows, "{night:?} {canyon:?}");
        assert_ne!((night.contrast, night.whites, night.blacks), (canyon.contrast, canyon.whites, canyon.blacks));
    }

    #[test]
    fn bright_and_dark_shares_drive_highlights_and_shadows() {
        // mostly shadow with a little bright sky, and the other way round
        let mostly_dark = Rgb32f::from_fn(64, 64, |_, y| if y < 8 { [0.9; 3] } else { [0.01; 3] });
        let mostly_bright = Rgb32f::from_fn(64, 64, |_, y| if y < 8 { [0.01; 3] } else { [0.9; 3] });
        let auto = |img: &Rgb32f| auto_tone(img, &SourceInfo::default(), &DevelopSettings::default());
        let (d, b) = (auto(&mostly_dark), auto(&mostly_bright));
        assert!(b.highlights < d.highlights && d.shadows > b.shadows, "{d:?} {b:?}");
        // a flat grey frame gets more Contrast than a wide-range one
        let flat = auto(&Rgb32f::from_fn(64, 64, |x, _| [0.15 + x as f32 * 0.001; 3]));
        let wide = auto(&Rgb32f::from_fn(64, 64, |x, _| [0.002 * 1.12f32.powi(x as i32); 3]));
        assert!(flat.contrast > wide.contrast, "{flat:?} {wide:?}");
        // a dim frame with nothing near white gets more Whites than one with much clipped
        let dim = auto(&Rgb32f::from_fn(64, 64, |x, _| [0.05 + x as f32 * 0.002; 3]));
        let clipped = auto(&Rgb32f::from_fn(64, 64, |x, _| if x < 24 { [8.0; 3] } else { [0.02 + x as f32 * 0.004; 3] }));
        assert!(dim.whites > clipped.whites, "{dim:?} {clipped:?}");
    }
}
