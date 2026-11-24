//! Tone Equalizer: the curve goes through the bands, neutral settings change nothing, the smoothed mask keeps
//! local contrast that a per-pixel curve flattens, Exposure moves which band a pixel is in, a window matches the
//! whole render, and hostile pixels and settings never panic.

use crate::tone_eq::{self, Curve};
use crate::{PixelWindow, RenderRequest, SourceInfo, render};
use lcc_develop::{DevelopSettings, ToneEq};
use lcc_raster::Rgb32f;

fn grey(v: f32) -> [f32; 3] {
    [v; 3]
}

/// Linear luminance of a pixel `ev` stops from middle grey.
fn at_ev(ev: f32) -> f32 {
    0.18 * ev.exp2()
}

fn with(t: ToneEq) -> DevelopSettings {
    DevelopSettings { tone_eq: t, ..Default::default() }
}

#[test]
fn the_curve_passes_close_to_every_band() {
    let t = ToneEq { ev8: 1.0, ev6: -0.5, ev4: 2.0, ev2: -1.0, ev0: 0.5, ..Default::default() };
    let c = Curve::new(&t);
    for (i, want) in t.bands().iter().enumerate() {
        let got = c.at(-8.0 + i as f64);
        assert!((got - want).abs() < 0.05, "band {i}: {got} vs {want}");
    }
    // past the span it stays at the end bands' values
    assert!((c.at(-20.0) - c.at(-8.0)).abs() < 1e-12 && (c.at(5.0) - c.at(0.0)).abs() < 1e-12);
    // every smoothing gives a bounded, finite curve
    for smoothing in [-1.0, 0.0, 1.0] {
        let c = Curve::new(&ToneEq { smoothing, ev3: 2.0, ev4: -2.0, ..Default::default() });
        for k in -100..=20 {
            let v = c.at(k as f64 / 10.0);
            assert!(v.is_finite() && v.abs() <= 2.0, "{smoothing}: {v}");
        }
    }
}

#[test]
fn neutral_settings_change_nothing() {
    let img = Rgb32f::from_fn(32, 24, |x, y| grey(at_ev(x as f32 / 4.0 - 8.0) * (1.0 + y as f32 / 50.0)));
    let mut out = img.clone();
    tone_eq::apply(&mut out, &DevelopSettings::default(), 32.0);
    assert_eq!(out, img);
    assert!(tone_eq::key(&DevelopSettings::default()).is_none());
    assert_eq!(tone_eq::reach(&DevelopSettings::default(), 1000.0), 0.0);
}

#[test]
fn a_band_moves_its_tones_more_than_far_ones() {
    // no smoothing: the per-pixel curve
    let s = with(ToneEq { ev6: 1.0, detail: 0.0, ..Default::default() });
    let mut img = Rgb32f::from_fn(2, 1, |x, _| grey(if x == 0 { at_ev(-6.0) } else { at_ev(0.0) }));
    tone_eq::apply(&mut img, &s, 2.0);
    let dark = (img.data[0][0] / at_ev(-6.0)).log2();
    let bright = (img.data[1][0] / at_ev(0.0)).log2();
    assert!((dark - 1.0).abs() < 0.1, "the -6 EV tone moves about a stop: {dark}");
    assert!(bright.abs() < 0.1, "grey barely moves: {bright}");
}

#[test]
fn exposure_decides_the_band() {
    // a tone at -6 EV, exposed up by 2 EV, is in the -4 EV band
    let s = DevelopSettings {
        light: lcc_develop::Light { exposure: 2.0, ..Default::default() },
        ..with(ToneEq { ev4: -1.0, detail: 0.0, ..Default::default() })
    };
    let mut img = Rgb32f::from_fn(1, 1, |_, _| grey(at_ev(-6.0)));
    tone_eq::apply(&mut img, &s, 1.0);
    let moved = (img.data[0][0] / at_ev(-6.0)).log2();
    assert!((moved + 1.0).abs() < 0.1, "{moved}");
    let mut t = s.clone();
    t.light.exposure = 1.0;
    assert_ne!(tone_eq::key(&s), tone_eq::key(&t), "Exposure is part of the key while it is on");
}

/// Log-ratio contrast of the fine texture in the middle of a compressed image.
fn texture_contrast(detail: f64, edges: f64) -> f32 {
    // a smooth ramp from -8 to 0 EV with a fine +-0.5 EV checker on top
    let (w, h) = (256, 64);
    let img = Rgb32f::from_fn(w, h, |x, y| {
        let base = -8.0 + 8.0 * x as f32 / (w - 1) as f32;
        let tex = if (x / 2 + y / 2) % 2 == 0 { 0.5 } else { -0.5 };
        grey(at_ev(base + tex))
    });
    // compress: shadows up, highlights down
    let t =
        ToneEq { ev8: 2.0, ev7: 1.5, ev6: 1.0, ev5: 0.5, ev4: 0.0, ev3: -0.5, ev2: -1.0, ev1: -1.5, ev0: -2.0, detail, edges, ..Default::default() };
    let mut out = img.clone();
    tone_eq::apply(&mut out, &with(t), w as f64);
    let (x, y) = (w / 2, h / 2);
    let at = |i: &Rgb32f, x: usize| i.data[y * w + x][0].log2();
    // neighbours 2 px apart sit on opposite checker cells
    (at(&out, x) - at(&out, x + 2)).abs() / (at(&img, x) - at(&img, x + 2)).abs()
}

#[test]
fn the_smoothed_mask_keeps_local_contrast_a_per_pixel_curve_flattens() {
    let flat = texture_contrast(0.0, 0.0);
    assert!(flat < 0.7, "a per-pixel compression flattens the texture: {flat}");
    // a soft mask (low Edge Refinement) treats the texture as detail inside the band and keeps it
    let kept = texture_contrast(100.0, 0.0);
    assert!(kept > 0.8 && kept > flat + 0.2, "the guided mask keeps it: {kept} (per pixel {flat})");
    // the default keeps most of it, too
    let default = texture_contrast(100.0, ToneEq::default().edges);
    assert!(default > flat + 0.1, "{default} vs {flat}");
}

#[test]
fn hostile_pixels_and_settings_never_panic() {
    let hostile = ToneEq {
        ev8: f64::NAN,
        ev7: f64::INFINITY,
        ev0: -1e300,
        smoothing: f64::NAN,
        detail: 1e300,
        size: f64::INFINITY,
        edges: f64::NEG_INFINITY,
        ev3: 1.0,
        ..Default::default()
    };
    let pixels = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0, f32::MIN_POSITIVE, 1e30, 0.18];
    for (w, h) in [(0, 0), (1, 1), (3, 1), (1, 5), (17, 9)] {
        for ppl in [0.0, -1.0, f64::NAN, f64::INFINITY, 1.0, 1e12] {
            for t in [hostile, ToneEq { ev3: 1.0, ..Default::default() }] {
                let mut img = Rgb32f::from_fn(w, h, |x, y| grey(pixels[(x + y) % pixels.len()]));
                tone_eq::apply(&mut img, &with(t), ppl);
                for (p, q) in img.data.iter().zip(Rgb32f::from_fn(w, h, |x, y| grey(pixels[(x + y) % pixels.len()])).data) {
                    if q[0].is_finite() && q[0].abs() < 1e29 {
                        assert!(p[0].is_finite(), "{q:?} -> {p:?}");
                    }
                }
                let _ = tone_eq::reach(&with(t), ppl);
            }
        }
    }
}

#[test]
fn a_whole_render_matches_a_windowed_one() {
    let (w, h) = (400, 300);
    let src = Rgb32f::from_fn(w, h, |x, y| {
        let ev = -7.0 + 7.0 * x as f32 / w as f32 + if (x / 3 + y / 3) % 2 == 0 { 0.3 } else { -0.3 };
        grey(at_ev(ev) * (0.8 + 0.4 * y as f32 / h as f32))
    });
    let s = with(ToneEq { ev7: 1.5, ev5: 1.0, ev3: 0.5, ..Default::default() });
    let info = SourceInfo::default();
    let full = render(&src, &info, &s, &RenderRequest::fit(w, h)).image;
    let win = PixelWindow { x: 180, y: 130, w: 48, h: 40 };
    let part = render(&src, &info, &s, &RenderRequest { window: Some(win), ..RenderRequest::fit(w, h) }).image;
    let mut worst = 0u8;
    for y in 0..win.h {
        for x in 0..win.w {
            let a = full.data[(win.y + y) * w + win.x + x];
            let b = part.data[y * win.w + x];
            for k in 0..3 {
                worst = worst.max(a[k].abs_diff(b[k]));
            }
        }
    }
    assert!(worst <= 2, "window differs by {worst}");
    let plain = render(&src, &info, &DevelopSettings::default(), &RenderRequest::fit(w, h)).image;
    let mean = |i: &lcc_raster::Rgba8| i.data.iter().map(|p| p[1] as f64).sum::<f64>() / i.data.len() as f64;
    assert!(mean(&full) > mean(&plain), "raising the shadows brightens the render");
}
