//! Pixel tests of the local (mask) Noise, Moiré and Defringe sliders.

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const W: usize = 240;
const H: usize = 120;

/// A deterministic hash in −1..1.
fn noise(x: usize, y: usize, k: u32) -> f32 {
    let mut v = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ k.wrapping_mul(0xcb1a_b31f);
    v ^= v >> 13;
    v = v.wrapping_mul(0x5bd1_e995);
    v ^= v >> 15;
    (v & 0xffff) as f32 / 32768.0 - 1.0
}

/// Settings with one mask covering the left half (x < 0.45), with `adjust`.
fn left_mask(adjust: LocalAdjustments) -> DevelopSettings {
    let shape = MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.46, 0.5) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], adjust, ..Default::default() });
    s
}

fn shot(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

/// Standard deviation of `f(pixel)` over columns `x0..x1` (rows 4..H-4).
fn spread(img: &Rgba8, x0: usize, x1: usize, f: impl Fn([u8; 4]) -> f32) -> f32 {
    let v: Vec<f32> = (4..H - 4).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| f(img.data[y * W + x])).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn chroma(p: [u8; 4]) -> f32 {
    p[0] as f32 - p[2] as f32
}

fn bright_camera_info() -> SourceInfo {
    let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.0005 * 1.3f32.powi(i as i32);
        [x, x / (x + 0.01)]
    }))
    .unwrap();
    SourceInfo { raw: true, camera_tone: Some(curve), ..Default::default() }
}

#[test]
fn local_whites_follow_visible_camera_highlights_in_v3() {
    use lightcraft_develop::{Process, Treatment};
    // Both patches lie below the old +0.5 scene-EV gate, but the camera renders the lower
    // patch as bright stone.
    let src = Rgb32f::from_fn(W, H, |_, y| [if y < H / 2 { 0.00003 } else { 0.018 }; 3]);
    let info = bright_camera_info();
    let image = |process: Process, whites: f64| {
        let mut s = left_mask(LocalAdjustments { whites, ..Default::default() });
        s.process = process.version();
        s.treatment = Treatment::Bw;
        render(&src, &info, &s, &RenderRequest::fit(W, H)).image
    };
    let neutral = image(Process::V3, 0.0);
    let (minus, plus) = (image(Process::V3, -100.0), image(Process::V3, 100.0));
    let bright = 90 * W + 50;
    let original = luma(neutral.data[bright]);
    assert!(original > 180.0, "fixture must be visibly bright: {original}");
    assert!(luma(minus.data[bright]) < original - 4.0, "negative Whites must lower bright stone");
    assert!(luma(plus.data[bright]) > original + 4.0, "positive Whites must lift bright stone");
    assert_eq!(minus.data[20 * W + 50], neutral.data[20 * W + 50], "keep deep shadows");
    assert_eq!(plus.data[20 * W + 50], neutral.data[20 * W + 50], "keep deep shadows");
    for y in 0..H {
        for x in 150..W {
            assert_eq!(plus.data[y * W + x], neutral.data[y * W + x], "outside mask");
            assert_eq!(minus.data[y * W + x], neutral.data[y * W + x], "outside mask");
        }
    }
}

#[test]
fn v3_local_whites_do_not_change_other_adjustments_or_disabled_masks() {
    use lightcraft_develop::Process;
    let src = Rgb32f::from_fn(W, H, |x, _| [0.00002 * 1.025f32.powi(x as i32); 3]);
    let info = bright_camera_info();
    for (whites, visible, amount) in [(0.0, true, 100.0), (100.0, false, 100.0), (100.0, true, 0.0)] {
        let mut settings = left_mask(LocalAdjustments { whites, exposure: 0.3, shadows: 35.0, ..Default::default() });
        settings.process = Process::V3.version();
        settings.light.shadows = 50.0;
        settings.light.blacks = -40.0;
        settings.masks[0].visible = visible;
        settings.masks[0].adjust.amount = amount;
        let mut baseline = settings.clone();
        baseline.masks[0].adjust.whites = 0.0;
        let req = RenderRequest::fit(W, H);
        assert_eq!(render(&src, &info, &settings, &req).image, render(&src, &info, &baseline, &req).image);
    }
}

#[test]
fn local_whites_weights_keep_tonal_order_for_steep_camera_curves() {
    let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| [0.0005 * 1.2f32.powi(i as i32), if i < 16 { 0.005 } else { 0.96 }])).unwrap();
    let info = SourceInfo { raw: true, camera_tone: Some(curve), ..Default::default() };
    let weights = crate::finish::local_whites_lut(&info);
    assert!(weights.v.iter().all(|w| w.is_finite() && (0.0..=1.0).contains(w)));
    let step = (crate::tone::LUT_MAX_EV - crate::tone::LUT_MIN_EV) / (crate::tone::LUT_N - 1) as f32;
    for w in weights.v.windows(2) {
        assert!(w[1] >= w[0]);
        assert!(step - 0.8 * (w[1] - w[0]) > 0.0, "Whites -100 must not reverse adjacent scene tones");
    }
    assert_eq!(weights.v[0], 0.0);
    assert_eq!(*weights.v.last().unwrap(), 1.0);
}

#[test]
fn local_noise_smooths_luminance_noise_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| [0.18 * (1.0 + 0.12 * noise(x, y, 1)); 3]);
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, luma);
    let on = shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() }));
    let off = shot(&src, &left_mask(LocalAdjustments { noise: -100.0, ..Default::default() }));
    let (smoothed, boosted) = (spread(&on, 10, 90, luma), spread(&off, 10, 90, luma));
    assert!(smoothed < 0.6 * base, "noise +100: {smoothed} vs {base}");
    assert!(boosted > 1.2 * base, "noise −100: {boosted} vs {base}");
    // outside the mask nothing changes
    assert_eq!(spread(&on, 150, 230, luma), spread(&plain, 150, 230, luma));
    // the mean brightness is kept
    let mean = |img: &Rgba8| (4..H - 4).flat_map(|y| (10..90).map(move |x| luma(img.data[y * W + x]))).sum::<f32>() / (80 * (H - 8)) as f32;
    assert!((mean(&on) - mean(&plain)).abs() < 2.0);
}

#[test]
fn local_moire_removes_colour_speckle_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let n = 0.25 * noise(x, y, 2);
        [0.18 * (1.0 + n), 0.18, 0.18 * (1.0 - n)]
    });
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, chroma);
    let on = shot(&src, &left_mask(LocalAdjustments { moire: 100.0, ..Default::default() }));
    let reduced = spread(&on, 10, 90, chroma);
    assert!(reduced < 0.4 * base, "moiré +100: {reduced} vs {base}");
    assert_eq!(spread(&on, 150, 230, chroma), spread(&plain, 150, 230, chroma));
    // luminance is kept
    assert!((spread(&on, 10, 90, luma) - spread(&plain, 10, 90, luma)).abs() < 1.0);
    // positive Noise also calms colour noise (less than Moiré)
    let n = spread(&shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() })), 10, 90, chroma);
    assert!(n < 0.8 * base && n > reduced, "{n}");
}

#[test]
fn local_defringe_desaturates_purple_edges_only() {
    // dark | bright edge at x = 40 (inside the mask) and x = 200 (outside), each with a purple
    // fringe column; a flat purple patch (no edge) at x 60..80
    let src = Rgb32f::from_fn(W, H, |x, _| {
        let fringe = x == 39 || x == 40 || x == 199 || x == 200;
        if fringe {
            [0.25, 0.04, 0.3]
        } else if (60..80).contains(&x) {
            [0.2, 0.08, 0.22]
        } else if (40..120).contains(&x) || x >= 200 {
            [0.7; 3]
        } else {
            [0.02; 3]
        }
    });
    let s = left_mask(LocalAdjustments { defringe: 100.0, ..Default::default() });
    let (plain, on) = (shot(&src, &DevelopSettings::default()), shot(&src, &s));
    let sat = |p: [u8; 4]| p[0].max(p[2]) as f32 - p[1] as f32;
    let at = |img: &Rgba8, x: usize| img.data[60 * W + x];
    assert!(sat(at(&plain, 40)) > 40.0, "a purple fringe to start with: {:?}", at(&plain, 40));
    assert!(sat(at(&on, 40)) < 0.5 * sat(at(&plain, 40)), "fringe in the mask: {:?} vs {:?}", at(&on, 40), at(&plain, 40));
    assert_eq!(at(&on, 200), at(&plain, 200), "outside the mask");
    let flat = |img: &Rgba8| sat(at(img, 70));
    assert!((flat(&on) - flat(&plain)).abs() <= 2.0, "flat purple kept: {} vs {}", flat(&on), flat(&plain));
}
