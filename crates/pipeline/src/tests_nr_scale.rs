//! Noise reduction is resolution independent (issue #960): a preview looks like the full-size
//! render scaled down, whatever the preview's size and whichever source level it is drawn from.

use crate::local::{denoise, log_lum, nr_params};
use crate::{RenderRequest, SourceInfo, plan, render};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::resample::{Filter, resize};
use lightcraft_raster::{Rgb32f, Rgba8};

const W: usize = 1536;
const H: usize = 1024;

/// Deterministic standard normal samples (xorshift + Box-Muller).
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

/// A `W × H` linear scene: a gradient, an edge and texture from 3 to 750 px wavelength, with white
/// luminance noise of `noise` stops and colour noise.
fn noisy_scene(noise: f32) -> Rgb32f {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let n: Vec<[f32; 4]> = (0..W * H).map(|_| [rng.normal(), rng.normal(), rng.normal(), rng.normal()]).collect();
    Rgb32f::from_fn(W, H, |x, y| {
        let r = n[y * W + x];
        let (xf, yf) = (x as f32, y as f32);
        let tex = 0.25 * (xf / 120.0).sin() * (yf / 90.0).cos()
            + 0.12 * ((xf + yf) / 16.0).sin()
            + 0.08 * (xf / 6.0).sin() * (yf / 7.0).sin()
            + 0.05 * ((xf - 2.0 * yf) / 3.0).sin();
        let edge = if x > 900 { 1.2 } else { 0.0 };
        let y0 = 0.18 * (-1.5 + 1.5 * yf / H as f32 + tex + edge + noise * r[0]).exp2();
        let c = |v: f32| y0 * (1.0 + 0.3 * noise * v).max(0.05);
        [c(r[1]), c(r[2]), c(r[3])]
    })
}

fn scaled(img: &Rgb32f, s: f32) -> Rgb32f {
    resize(img, (W as f32 * s).round() as usize, (H as f32 * s).round() as usize, Filter::Mitchell)
}

/// RMS difference of log2 luminance (stops).
fn rms_log(a: &Rgb32f, b: &Rgb32f) -> f32 {
    let n = a.data.len() as f32;
    (a.data.iter().zip(&b.data).map(|(p, q)| (log_lum(*p) - log_lum(*q)).powi(2)).sum::<f32>() / n).sqrt()
}

/// RMS difference of chromaticity (r and b over r + g + b).
fn rms_chroma(a: &Rgb32f, b: &Rgb32f) -> f32 {
    let ch = |p: [f32; 3]| {
        let s = (p[0] + p[1] + p[2]).max(1e-6);
        [p[0] / s, p[2] / s]
    };
    let d = |p: [f32; 3], q: [f32; 3]| (ch(p)[0] - ch(q)[0]).powi(2) + (ch(p)[1] - ch(q)[1]).powi(2);
    (a.data.iter().zip(&b.data).map(|(p, q)| d(*p, *q)).sum::<f32>() / a.data.len() as f32).sqrt()
}

/// For each preview scale `k`: (k, how far the preview is from the full-size render scaled down,
/// how far it would be with noise reduction left off).
fn preview_error(s: &DevelopSettings, scales: &[f32], metric: fn(&Rgb32f, &Rgb32f) -> f32) -> Vec<(f32, f32, f32)> {
    let src = noisy_scene(0.15);
    let mut full = src.clone();
    denoise(&mut full, s, 1.0);
    scales
        .iter()
        .map(|&k| {
            let reference = scaled(&full, k);
            let plain = scaled(&src, k);
            let mut preview = plain.clone();
            denoise(&mut preview, s, k);
            (k, metric(&preview, &reference), metric(&plain, &reference))
        })
        .collect()
}

const SCALES: [f32; 6] = [0.5, 0.33, 0.25, 0.167, 0.1, 0.05];

#[test]
fn luminance_nr_preview_matches_full_size_scaled_down() {
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 50.0;
    s.detail.nr_detail = 50.0;
    s.detail.nr_color = 0.0;
    for (k, err, off) in preview_error(&s, &SCALES, rms_log) {
        let ratio = err / off;
        eprintln!("luminance NR, preview at {k}: {err:.4} stops from the full-size render scaled down ({ratio:.2} of NR off)");
        // Before #960 the radius stopped shrinking at 0.4×: 2.3× (at 0.167) to 6× (at 0.05) further
        // off than no noise reduction at all, and 0.53 / 0.70 of it at 0.5 / 0.33.
        assert!(ratio <= 1.0, "preview at {k} is further off than no noise reduction: {err} vs {off}");
        assert!(err <= 0.01, "preview at {k} is {err} stops off");
        if k >= 0.33 {
            assert!(ratio <= 0.4, "preview at {k}: {ratio}");
        }
    }
}

#[test]
fn colour_nr_preview_matches_full_size_scaled_down() {
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 0.0;
    s.detail.nr_color = 50.0;
    s.detail.nr_color_detail = 50.0;
    s.detail.nr_color_smoothness = 50.0;
    for (k, err, off) in preview_error(&s, &SCALES, rms_chroma) {
        let ratio = err / off;
        eprintln!("colour NR, preview at {k}: {err:.6} from the full-size render scaled down ({ratio:.2} of NR off)");
        // before #960: 0.00020 (0.31 of NR off) at 0.167, 0.00022 (0.75) at 0.1
        assert!(err <= 1.2e-4, "preview at {k} is {err} off");
        if k >= 0.1 {
            assert!(ratio <= 0.4, "preview at {k}: {ratio}");
        }
    }
}

#[test]
fn full_size_parameters_unchanged() {
    // full-size renders and exports keep their look: these are the parameters from before #960
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 50.0;
    s.detail.nr_detail = 50.0;
    s.detail.nr_color = 25.0;
    s.detail.nr_color_detail = 50.0;
    s.detail.nr_color_smoothness = 50.0;
    let (l, c) = nr_params(&s, 1.0);
    let (l, c) = (l.unwrap(), c.unwrap());
    assert_eq!(l.sigma, 1.0 + 2.5 * 0.5);
    assert_eq!(l.eps, 0.002 + 0.5 * 0.5 * 0.25 * (1.0 - 0.7 * 0.5));
    assert_eq!(l.k, 0.5f32.sqrt());
    assert_eq!(c.sigma, (1.5 + 6.0 * 0.25) * 1.5);
    assert_eq!(c.t, 0.25 * (1.0 - 0.25));
}

#[test]
fn tiny_previews_skip_noise_reduction_and_hostile_scales_are_safe() {
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 100.0;
    s.detail.nr_color = 100.0;
    // a 128-px thumbnail of a 6000-px photo: the luminance radius is a few hundredths of a pixel,
    // the colour radius (wider) a quarter of one, so that fades to a tenth
    let (l, c) = nr_params(&s, 128.0 / 6000.0);
    assert_eq!(l, None);
    let (c, full) = (c.unwrap(), nr_params(&s, 1.0).1.unwrap());
    assert!(c.t < full.t * 0.1 && c.t > 0.0, "{c:?}");
    assert_eq!(nr_params(&s, 0.0), (None, None));
    for k in [f32::NAN, f32::INFINITY, -1.0, 7.0] {
        let (l, c) = nr_params(&s, k);
        let vals = [l.map(|l| [l.sigma, l.eps, l.k]), c.map(|c| [c.sigma, c.t, 1.0])];
        for v in vals.into_iter().flatten().flatten() {
            assert!(v.is_finite() && v >= 0.0, "{k}: {v}");
        }
    }
}

fn mean_abs_diff(a: &Rgba8, b: &Rgba8) -> f32 {
    let px = |p: &[u8; 4], q: &[u8; 4]| (0..3).map(|i| (f32::from(p[i]) - f32::from(q[i])).abs()).sum::<f32>() / 3.0;
    a.data.iter().zip(&b.data).map(|(p, q)| px(p, q)).sum::<f32>() / a.data.len() as f32
}

#[test]
fn preview_from_a_smaller_source_level_matches_one_from_the_original() {
    // The app draws a grid cell from the 512-px thumbnail level and the loupe from the 2560-px
    // preview level. Told the original's size, a render from the smaller copy denoises like one from
    // the original at the same output size; before #960 its radii were in the copy's pixels.
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 60.0;
    s.detail.nr_color = 40.0;
    let original = noisy_scene(0.15);
    let thumb_level = scaled(&original, 512.0 / W as f32);
    let req = RenderRequest::fit(256, 256);
    let raw = SourceInfo { raw: true, ..Default::default() };
    let told = SourceInfo { native_long: Some(W), ..raw.clone() };
    assert!((plan(&thumb_level, &told, &s, &req).detail_scale - 256.0 / W as f32).abs() < 1e-4);
    assert!((plan(&original, &raw, &s, &req).detail_scale - 256.0 / W as f32).abs() < 1e-4);
    assert!((plan(&thumb_level, &raw, &s, &req).detail_scale - 0.5).abs() < 1e-4, "original unknown: the source's pixels");

    let mut plain = s.clone();
    plain.detail.nr_luminance = 0.0;
    plain.detail.nr_color = 0.0;
    let from_original = render(&original, &raw, &s, &req).image;
    let told_diff = mean_abs_diff(&render(&thumb_level, &told, &s, &req).image, &from_original);
    let untold_diff = mean_abs_diff(&render(&thumb_level, &raw, &s, &req).image, &from_original);
    // (the two sources differ a little anyway: the thumbnail level is resampled twice)
    let resampling = mean_abs_diff(&render(&thumb_level, &told, &plain, &req).image, &render(&original, &raw, &plain, &req).image);
    eprintln!("mean |Δ| (8-bit) from the original's render: told {told_diff:.3}, not told {untold_diff:.3}, NR off {resampling:.3}");
    assert!(told_diff <= resampling + 0.1, "{told_diff} vs {resampling}");
    assert!(untold_diff > told_diff + 0.3, "the copy's own pixels change the result: {untold_diff} vs {told_diff}");
}

#[test]
fn crop_does_not_change_the_noise_reduction_scale() {
    // a 1:1 render of a crop is at the original's pixels, like a 1:1 render of the whole photo
    let s = DevelopSettings::default();
    let src = Rgb32f::filled(400, 200, [0.2; 3]);
    let info = SourceInfo::default();
    assert!((plan(&src, &info, &s, &RenderRequest::fit(400, 200)).detail_scale - 1.0).abs() < 1e-6);
    let mut cropped = s.clone();
    cropped.crop.geometry.rect = lightcraft_geom::Rect::new(0.25, 0.25, 0.75, 0.75);
    let p = plan(&src, &info, &cropped, &RenderRequest::fit(200, 100));
    assert_eq!((p.w, p.h), (200, 100));
    assert!((p.detail_scale - 1.0).abs() < 1e-6, "{}", p.detail_scale);
}

#[test]
fn nr_settings_are_bounded_at_the_render_boundary() {
    let setters: [fn(&mut DevelopSettings, f64); 5] = [
        |s, v| s.detail.nr_luminance = v,
        |s, v| s.detail.nr_color = v,
        |s, v| s.detail.nr_detail = v,
        |s, v| s.detail.nr_color_detail = v,
        |s, v| s.detail.nr_color_smoothness = v,
    ];
    for set in setters {
        for value in [-1e300, -1.0, 0.0, 50.0, 100.0, 101.0, 1e300, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut s = DevelopSettings::default();
            s.detail.nr_luminance = 50.0;
            s.detail.nr_color = 50.0;
            set(&mut s, value);
            let mut bounded = s.clone();
            let expected = if value.is_finite() { value.clamp(0.0, 100.0) } else { 0.0 };
            set(&mut bounded, expected);
            for scale in [0.0, 0.1, 0.5, 1.0] {
                let params = nr_params(&s, scale);
                assert_eq!(params, nr_params(&bounded, scale), "value={value}, scale={scale}");
                if let Some(l) = params.0 {
                    assert!(l.sigma.is_finite() && (1.0..=3.5).contains(&l.sigma));
                    assert!(l.eps.is_finite() && l.eps > 0.0);
                    assert!((0.0..=1.0).contains(&l.k));
                }
                if let Some(c) = params.1 {
                    assert!(c.sigma.is_finite() && (1.0..=15.0).contains(&c.sigma));
                    assert!((0.0..=1.0).contains(&c.t));
                }
            }
        }
    }
}

#[test]
fn excessive_nr_matches_the_slider_endpoint_on_texture_and_colour_edges() {
    // Fine luminance texture, a luminance edge and an isoluminant colour edge. A saved
    // out-of-range strength must never invert texture/colour by extrapolating past the blur.
    let src = Rgb32f::from_fn(96, 64, |x, y| {
        let texture = if (x + y) % 2 == 0 { 0.015 } else { -0.015 };
        let base = if x < 48 { 0.18 } else { 0.36 };
        let colour = if y < 32 { 0.03 } else { -0.03 };
        [base + texture + colour, base + texture, base + texture - colour]
    });
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 100.0;
    s.detail.nr_color = 100.0;
    let mut endpoint = src.clone();
    denoise(&mut endpoint, &s, 1.0);
    s.detail.nr_luminance = 400.0;
    s.detail.nr_color = 400.0;
    let mut excessive = src;
    denoise(&mut excessive, &s, 1.0);
    assert_eq!(excessive.data, endpoint.data);
    assert!(excessive.data.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
}

#[test]
fn excessive_nr_quality_comparison() {
    use crate::local::{NrColor, NrLum, apply_nr};
    let clean = Rgb32f::from_fn(192, 128, |x, y| {
        let base = if x < 96 { 0.18 } else { 0.36 };
        let fine = 0.015 * (x as f32 * 1.2).sin() * (y as f32 * 0.7).cos();
        let colour = if y < 64 { 0.06 } else { -0.06 };
        [base + fine + colour, base + fine, base + fine - colour]
    });
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut noisy = clean.clone();
    for p in &mut noisy.data {
        *p = p.map(|v| v + 0.005 * rng.normal());
    }
    let mut before = noisy.clone();
    // Original nr_params at strength 400, Detail/Smoothness 50, scale 1.
    apply_nr(&mut before, Some(NrLum { sigma: 11.0, eps: 0.002 + 16.0 * 0.25 * 0.65, k: 2.0 }), Some(NrColor { sigma: 25.5 * 1.5, t: 3.0 }));
    let mut after = noisy;
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 400.0;
    s.detail.nr_color = 400.0;
    denoise(&mut after, &s, 1.0);
    let before_l = rms_log(&before, &clean);
    let after_l = rms_log(&after, &clean);
    let before_c = rms_chroma(&before, &clean);
    let after_c = rms_chroma(&after, &clean);
    eprintln!("out-of-range NR quality: log-luminance RMS {before_l:.6} -> {after_l:.6} EV; chromaticity RMS {before_c:.6} -> {after_c:.6}");
    assert!(after_l < before_l * 0.5);
    assert!(after_c < before_c * 0.5);
}

#[test]
fn valid_nr_parameters_keep_the_legacy_mapping() {
    for value in [0.0, 0.1, 1.0, 25.0, 50.0, 99.9, 100.0] {
        for detail in [0.0, 50.0, 100.0] {
            let mut s = DevelopSettings::default();
            s.detail.nr_luminance = value;
            s.detail.nr_color = value;
            s.detail.nr_detail = detail;
            s.detail.nr_color_detail = detail;
            s.detail.nr_color_smoothness = detail;
            let (l, c) = nr_params(&s, 1.0);
            if value == 0.0 {
                assert_eq!((l, c), (None, None));
                continue;
            }
            let amount = (value / 100.0) as f32;
            let detail = (detail / 100.0) as f32;
            assert_eq!(
                l,
                Some(crate::local::NrLum { sigma: 1.0 + 2.5 * amount, eps: 0.002 + amount * amount * 0.25 * (1.0 - 0.7 * detail), k: amount.sqrt() })
            );
            assert_eq!(c, Some(crate::local::NrColor { sigma: (1.5 + 6.0 * amount) * (1.0 + detail), t: amount * (1.0 - detail * 0.5) }));
        }
    }
}

#[test]
fn colour_nr_keeps_flat_deep_shadows_and_black() {
    for level in [0.0, 1e-12, 1e-9, 1e-7, 1e-6, 0.18] {
        for colour in [[1.0; 3], [1.6, 0.8, 0.4]] {
            let pixel = colour.map(|v| v * level);
            let src = Rgb32f::filled(48, 32, pixel);
            for amount in [0.1, 25.0, 100.0] {
                let mut s = DevelopSettings::default();
                s.detail.nr_color = amount;
                let mut filtered = src.clone();
                denoise(&mut filtered, &s, 1.0);
                for actual in filtered.data.iter().flatten() {
                    assert!(actual.is_finite());
                }
                for actual in &filtered.data {
                    for (a, b) in actual.iter().zip(pixel) {
                        assert!((a - b).abs() <= b.abs() * 2e-5, "level={level}, amount={amount}: {actual:?} != {pixel:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn colour_nr_preserves_luminance_of_noisy_deep_shadows() {
    use lightcraft_color::luminance_2020;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut src = Rgb32f::filled(96, 64, [1e-8; 3]);
    for p in &mut src.data {
        *p = p.map(|v| v * (1.0 + 0.2 * rng.normal()).max(0.1));
    }
    let mut filtered = src.clone();
    let mut s = DevelopSettings::default();
    s.detail.nr_color = 100.0;
    denoise(&mut filtered, &s, 1.0);
    for (before, after) in src.data.iter().zip(&filtered.data) {
        let y = luminance_2020(*before);
        assert!((luminance_2020(*after) - y).abs() <= y * 2e-5);
    }
    // Optional, reproducible visual report; generated images stay outside the repository.
    if let Some(dir) = std::env::var_os("LIGHTCRAFT_NR_REPORT") {
        let old_chroma = src.map(|p| p.map(|v| v / lightcraft_color::luminance_2020(p).max(1e-6)));
        let blurred = lightcraft_raster::blur::gaussian(&old_chroma, nr_params(&s, 1.0).1.unwrap().sigma);
        let before = Rgb32f::from_fn(src.width, src.height, |x, y| {
            let p = src.get(x, y);
            let a = old_chroma.get(x, y);
            let b = blurred.get(x, y);
            [0, 1, 2].map(|k| (a[k] + (b[k] - a[k]) * 0.75) * luminance_2020(p))
        });
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut display = DevelopSettings::default();
        display.detail.nr_color = 0.0;
        display.light.exposure = 20.0;
        for (name, img) in [("before", &before), ("after", &filtered)] {
            let rendered = render(img, &SourceInfo::default(), &display, &RenderRequest::fit(96, 64)).image;
            let bytes: Vec<u8> = rendered.data.iter().flatten().copied().collect();
            image::save_buffer(dir.join(format!("{name}.png")), &bytes, 96, 64, image::ColorType::Rgba8).unwrap();
        }
    }
    let chroma_error = |img: &Rgb32f| rms_chroma(&img.map(|p| p.map(|v| v * 1e8)), &Rgb32f::filled(96, 64, [1.0; 3]));
    let noisy_error = chroma_error(&src);
    let filtered_error = chroma_error(&filtered);
    eprintln!("deep-shadow chromaticity RMS: {noisy_error:.6} -> {filtered_error:.6}; luminance relative error <= 0.00002");
    assert!(filtered_error < noisy_error * 0.5);
}
