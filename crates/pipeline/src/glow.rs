//! Original scene-linear light scattering, based on public descriptions of Glow controls.
//! Highlight extraction has a soft knee; Range 100 includes midtones, Range 0 only bright
//! sources. Scattering is evaluated on an area-averaged, bounded grid in image-relative units.
//! CPU and GPU share this operator, before tone mapping, including HDR and deep exports.
use lightcraft_develop::{DevelopSettings, GlowStyle};
use lightcraft_raster::Rgb32f;

const GRID_EDGE: usize = 768;

pub fn active(s: &DevelopSettings) -> bool {
    s.section_enabled("effects") && s.effects.glow.amount.is_finite() && s.effects.glow.amount > 0.0
}

/// Includes exposure: it determines which source values cross the extraction knee.
pub fn key(s: &DevelopSettings) -> String {
    if active(s) { format!("{:?}:{}", s.effects.glow, s.light.exposure) } else { String::new() }
}

fn bounded(v: f64, lo: f64, hi: f64, default: f64) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) as f32 } else { default as f32 }
}

pub fn apply(img: &mut Rgb32f, s: &DevelopSettings, px_per_long: f64) {
    if !active(s) || img.width == 0 || img.height == 0 || !px_per_long.is_finite() || px_per_long <= 0.0 {
        return;
    }
    let g = &s.effects.glow;
    let amount = bounded(g.amount, 0.0, 100.0, 0.0) / 100.0;
    let range = bounded(g.range, 0.0, 100.0, 50.0) / 100.0;
    let spread = bounded(g.spread, 0.0, 100.0, 50.0) / 100.0;
    let warmth = bounded(g.warmth, -100.0, 100.0, 0.0) / 100.0;
    let softness = bounded(g.softness, 0.0, 100.0, 50.0) / 100.0;
    let angle = bounded(g.direction, -180.0, 180.0, 0.0).to_radians();
    let exposure = bounded(s.light.exposure, -5.0, 5.0, 0.0).exp2();
    let scale = (GRID_EDGE as f64 / img.width.max(img.height) as f64).min(1.0);
    let w = ((img.width as f64 * scale).ceil() as usize).max(1);
    let h = ((img.height as f64 * scale).ceil() as usize).max(1);
    let mut seed = extract_seed(img, w, h, 0.88 - 0.78 * range, exposure, g.style == GlowStyle::Diffusion);
    if g.style == GlowStyle::Halation {
        // Film-like halation is warm even around blue lamps, rather than a channelwise tint.
        for p in &mut seed.data {
            let l = p[0] * 0.2627 + p[1] * 0.6780 + p[2] * 0.0593;
            *p = [l; 3];
        }
    }
    let radius = ((0.001 + 0.039 * spread * spread) * (px_per_long * scale) as f32).clamp(0.0, GRID_EDGE as f32);
    let directional = matches!(g.style, GlowStyle::Motion | GlowStyle::Streak);
    let mut light = seed.clone();
    // Three separable box convolutions approximate a Gaussian in linear time.
    let blur_radius = if directional { radius * (0.02 + 0.28 * softness) } else { radius };
    for _ in 0..3 {
        light = smooth_blur(&light, blur_radius * 0.58);
    }
    if directional {
        light = trail(&light, radius * 4.0, angle, g.style == GlowStyle::Streak);
    }
    let tint = if g.style == GlowStyle::Halation {
        [1.6 + 0.4 * warmth, 0.35 + 0.15 * warmth, 0.08]
    } else {
        [1.0 + 0.35 * warmth, 1.0, 1.0 - 0.35 * warmth]
    };
    add_light(img, &seed, &light, amount, tint, g.style == GlowStyle::Halation);
}

fn extract_seed(img: &Rgb32f, w: usize, h: usize, threshold: f32, exposure: f32, diffusion: bool) -> Rgb32f {
    let mut seed = Rgb32f::new(w, h);
    // Area-average the extracted radiance, rather than thresholding an already reduced image:
    // tiny bright lamps must contribute even when a grid cell mostly contains dark pixels.
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x * img.width / w, (x + 1) * img.width / w);
            let (y0, y1) = (y * img.height / h, (y + 1) * img.height / h);
            let mut sum = [0.0; 3];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = img.get(xx, yy).map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
                    // Rec.2020 luminance, the pipeline's scene-linear working space.
                    let l = p[0] * 0.2627 + p[1] * 0.6780 + p[2] * 0.0593;
                    // A luminance-only cutoff rejects saturated blue/red lamps that look bright.
                    // A modest peak-channel contribution preserves those emitters, while the
                    // perceptual knee leaves dark background pixels out at ordinary Range values.
                    let peak = p[0].max(p[1]).max(p[2]);
                    let brightness = ((0.65 * l + 0.35 * peak) * exposure).sqrt();
                    let t = ((brightness - (threshold - 0.08)) / 0.16).clamp(0.0, 1.0);
                    let mask = t * t * (3.0 - 2.0 * t);
                    let weight = if diffusion { 0.25 + 0.75 * mask } else { mask };
                    for c in 0..3 {
                        sum[c] += p[c] * weight;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)).max(1) as f32;
            seed.set(x, y, sum.map(|v| v / n));
        }
    }
    seed
}

fn add_light(img: &mut Rgb32f, seed: &Rgb32f, light: &Rgb32f, amount: f32, tint: [f32; 3], halation: bool) {
    let (w, h) = (seed.width, seed.height);
    for y in 0..img.height {
        for x in 0..img.width {
            let gx = (x as f32 + 0.5) * w as f32 / img.width as f32 - 0.5;
            let gy = (y as f32 + 0.5) * h as f32 / img.height as f32 - 0.5;
            let glow = sample(light, gx, gy);
            let source = sample(seed, gx, gy);
            let mut p = img.get(x, y);
            for c in 0..3 {
                let halo = if halation { (glow[c] - source[c]).max(0.0) } else { glow[c] };
                // Keep scene headroom: output conversion/tone mapping happens downstream.
                let v = p[c] + amount * halo * tint[c];
                if v.is_finite() {
                    p[c] = v;
                }
            }
            img.set(x, y, p);
        }
    }
}

fn box_blur(src: &Rgb32f, radius: usize) -> Rgb32f {
    let r = radius.min(GRID_EDGE) as isize;
    if r == 0 {
        return src.clone();
    }
    let mut tmp = Rgb32f::new(src.width, src.height);
    let mut out = tmp.clone();
    let div = (2 * r + 1) as f32;
    for y in 0..src.height {
        let mut sum = [0.0; 3];
        for k in -r..=r {
            let p = pixel_zero(src, k, y as isize);
            for c in 0..3 {
                sum[c] += p[c];
            }
        }
        for x in 0..src.width {
            tmp.set(x, y, sum.map(|v| v / div));
            let a = pixel_zero(src, x as isize - r, y as isize);
            let b = pixel_zero(src, x as isize + r + 1, y as isize);
            for c in 0..3 {
                sum[c] += b[c] - a[c];
            }
        }
    }
    for x in 0..src.width {
        let mut sum = [0.0; 3];
        for k in -r..=r {
            let p = pixel_zero(&tmp, x as isize, k);
            for c in 0..3 {
                sum[c] += p[c];
            }
        }
        for y in 0..src.height {
            out.set(x, y, sum.map(|v| (v / div).max(0.0)));
            let a = pixel_zero(&tmp, x as isize, y as isize - r);
            let b = pixel_zero(&tmp, x as isize, y as isize + r + 1);
            for c in 0..3 {
                sum[c] += b[c] - a[c];
            }
        }
    }
    out
}

/// Interpolate neighbouring box radii so small Spread/Softness changes never disappear
/// through rounding (especially in reduced previews).
fn smooth_blur(src: &Rgb32f, radius: f32) -> Rgb32f {
    let radius = radius.clamp(0.0, GRID_EDGE as f32);
    let lo = radius.floor() as usize;
    let fraction = radius - lo as f32;
    let mut out = box_blur(src, lo);
    if fraction > 0.0 {
        let upper = box_blur(src, lo.saturating_add(1));
        for (p, q) in out.data.iter_mut().zip(&upper.data) {
            for c in 0..3 {
                p[c] += fraction * (q[c] - p[c]);
            }
        }
    }
    out
}

fn sample(src: &Rgb32f, x: f32, y: f32) -> [f32; 3] {
    let (x, y) = (x.clamp(0.0, src.width.saturating_sub(1) as f32), y.clamp(0.0, src.height.saturating_sub(1) as f32));
    let (ix, iy) = (x.floor() as isize, y.floor() as isize);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let (a, b, c, d) = (src.get_clamped(ix, iy), src.get_clamped(ix + 1, iy), src.get_clamped(ix, iy + 1), src.get_clamped(ix + 1, iy + 1));
    std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
}

fn pixel_zero(src: &Rgb32f, x: isize, y: isize) -> [f32; 3] {
    if x < 0 || y < 0 || x >= src.width as isize || y >= src.height as isize { [0.0; 3] } else { src.get(x as usize, y as usize) }
}

/// Sample with zero radiance outside the sensor, rather than repeating an edge lamp.
fn sample_zero(src: &Rgb32f, x: f32, y: f32) -> [f32; 3] {
    let (ix, iy) = (x.floor() as isize, y.floor() as isize);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let (a, b, c, d) = (pixel_zero(src, ix, iy), pixel_zero(src, ix + 1, iy), pixel_zero(src, ix, iy + 1), pixel_zero(src, ix + 1, iy + 1));
    std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
}

/// A causal exponential light trail, evaluated in image order along its major axis.
/// Every pixel is visited once; a long trail cannot turn into 64 repeated point stamps.
/// Length is the distance over which about 95% of the scattered energy is distributed.
fn trail_pass(src: &Rgb32f, length: f32, direction: [f32; 2]) -> Rgb32f {
    let [dx, dy] = direction;
    let horizontal = dx.abs() >= dy.abs();
    let major = dx.abs().max(dy.abs()).max(1e-6);
    let [sx, sy] = [dx / major, dy / major];
    let decay = (-3.0 / (length * major).max(1e-3)).exp();
    let (extent, cross) = if horizontal { (src.width, src.height) } else { (src.height, src.width) };
    let forward = if horizontal { dx >= 0.0 } else { dy >= 0.0 };
    let mut out = Rgb32f::new(src.width, src.height);
    for step in 0..extent {
        let along = if forward { step } else { extent - 1 - step };
        for offset in 0..cross {
            let (x, y) = if horizontal { (along, offset) } else { (offset, along) };
            let previous = sample_zero(&out, x as f32 - sx, y as f32 - sy);
            let p = src.get(x, y);
            out.set(x, y, std::array::from_fn(|c| (1.0 - decay) * p[c] + decay * previous[c]));
        }
    }
    out
}

fn trail(src: &Rgb32f, length: f32, angle: f32, symmetric: bool) -> Rgb32f {
    if src.width == 0 || src.height == 0 || !length.is_finite() || length <= 0.0 || !angle.is_finite() {
        return src.clone();
    }
    let (dy, dx) = angle.sin_cos();
    let mut out = trail_pass(src, length, [dx, dy]);
    if symmetric {
        let other = trail_pass(src, length, [-dx, -dy]);
        for (a, b) in out.data.iter_mut().zip(&other.data) {
            for c in 0..3 {
                a[c] = (a[c] + b[c]) * 0.5;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(style: GlowStyle) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.effects.glow.style = style;
        s.effects.glow.amount = 100.0;
        s.effects.glow.spread = 100.0;
        s
    }
    fn lamp() -> Rgb32f {
        Rgb32f::from_fn(129, 129, |x, y| if (60..69).contains(&x) && (60..69).contains(&y) { [4.0; 3] } else { [0.0; 3] })
    }
    /// The reporter's Motion settings must include saturated coloured light sources.
    #[test]
    fn motion_at_reported_settings_spreads_blue_lights() {
        let mut s = settings(GlowStyle::Motion);
        s.effects.glow.amount = 23.0;
        s.effects.glow.range = 81.0;
        s.effects.glow.spread = 47.0;
        s.effects.glow.warmth = 46.0;
        s.effects.glow.softness = 50.0;
        let src = Rgb32f::from_fn(129, 65, |x, y| if (61..68).contains(&x) && (29..36).contains(&y) { [0.0, 0.0, 1.0] } else { [0.0; 3] });
        let mut out = src.clone();
        apply(&mut out, &s, 129.0);
        assert!(out.get(72, 32)[2] > 0.001, "a blue light must have a continuous visible trail at Amount 23, Range 81");
        assert!(out.data.iter().all(|p| p[0] == 0.0 && p[1] == 0.0), "Motion preserves the light's colour");
    }
    #[test]
    fn directional_scattering_does_not_replicate_lights_on_the_frame_edge() {
        for symmetric in [false, true] {
            let src = Rgb32f::from_fn(257, 33, |x, y| if x == 0 && y == 16 { [1.0; 3] } else { [0.0; 3] });
            let out = trail(&src, 80.0, 0.0, symmetric);
            let energy: f32 = out.data.iter().map(|p| p[0]).sum();
            assert!(energy <= 1.001, "edge extension must not manufacture light: {energy}, symmetric={symmetric}");
        }
    }
    #[test]
    fn bloom_at_the_border_does_not_manufacture_light() {
        let mut src = Rgb32f::new(129, 65);
        src.set(0, 32, [1.0; 3]);
        let mut out = src.clone();
        apply(&mut out, &settings(GlowStyle::Bloom), 129.0);
        let total: f32 = out.data.iter().map(|p| p[0]).sum();
        assert!(total <= 2.001, "scattering plus the preserved source must not exceed twice the input energy: {total}");
    }

    #[test]
    fn a_long_sharp_motion_trail_has_no_repeated_point_gaps() {
        let src = Rgb32f::from_fn(384, 33, |x, y| if x == 32 && y == 16 { [1.0; 3] } else { [0.0; 3] });
        let out = trail(&src, 180.0, 0.0, false);
        for x in 33..180 {
            assert!(out.get(x, 16)[0] > 0.0, "the point trail contains a gap at {x}");
        }
    }

    #[test]
    fn small_softness_changes_are_not_rounded_away() {
        let mut s = settings(GlowStyle::Motion);
        s.effects.glow.spread = 47.0;
        s.effects.glow.softness = 0.0;
        let src = lamp();
        let mut a = src.clone();
        apply(&mut a, &s, 129.0);
        s.effects.glow.softness = 10.0;
        let mut b = src.clone();
        apply(&mut b, &s, 129.0);
        assert_ne!(a, b, "the first Softness steps must affect the trail even on a preview");
    }

    #[test]
    fn zero_disabled_and_dark_are_identity() {
        let src = lamp();
        let mut out = src.clone();
        apply(&mut out, &DevelopSettings::default(), 129.0);
        assert_eq!(out, src);
        let mut s = settings(GlowStyle::Bloom);
        s.set_section_enabled("effects", false);
        apply(&mut out, &s, 129.0);
        assert_eq!(out, src);
        for style in [GlowStyle::Bloom, GlowStyle::Diffusion, GlowStyle::Halation, GlowStyle::Motion, GlowStyle::Streak] {
            let mut black = Rgb32f::new(129, 129);
            apply(&mut black, &settings(style), 129.0);
            assert!(black.data.iter().all(|p| *p == [0.0; 3]));
        }
    }
    #[test]
    fn styles_scatter_light_with_distinct_shapes_and_color() {
        let mut bloom = lamp();
        apply(&mut bloom, &settings(GlowStyle::Bloom), 129.0);
        assert!(bloom.get(75, 64)[0] > 0.0);
        assert!((bloom.get(75, 64)[0] - bloom.get(64, 75)[0]).abs() < 1e-5);
        let mut halation = lamp();
        apply(&mut halation, &settings(GlowStyle::Halation), 129.0);
        let p = halation.get(75, 64);
        assert!(p[0] > p[1] && p[1] > p[2]);
        let mut motion = lamp();
        apply(&mut motion, &settings(GlowStyle::Motion), 129.0);
        assert!(motion.get(80, 64)[0] > motion.get(48, 64)[0]);
        let mut streak = lamp();
        apply(&mut streak, &settings(GlowStyle::Streak), 129.0);
        assert!((streak.get(80, 64)[0] - streak.get(48, 64)[0]).abs() < 1e-4);
        assert!(streak.get(80, 64)[0] > streak.get(64, 80)[0]);
    }
    #[test]
    fn radii_scale_with_resolution_and_strength_is_linear() {
        let mut s = settings(GlowStyle::Bloom);
        let source = |edge: usize| {
            Rgb32f::from_fn(edge, edge, |x, y| {
                if (edge * 7 / 16..edge * 9 / 16).contains(&x) && (edge * 7 / 16..edge * 9 / 16).contains(&y) { [2.0; 3] } else { [0.0; 3] }
            })
        };
        let mut small = source(256);
        let mut big = source(512);
        apply(&mut small, &s, 256.0);
        apply(&mut big, &s, 512.0);
        let energy =
            |out: &Rgb32f, base: &Rgb32f| out.data.iter().zip(&base.data).map(|(a, b)| (a[0] - b[0]) as f64).sum::<f64>() / out.data.len() as f64;
        assert!((energy(&small, &source(256)) - energy(&big, &source(512))).abs() < 0.001);
        let mut half = source(256);
        s.effects.glow.amount = 50.0;
        apply(&mut half, &s, 256.0);
        assert!((half.get(160, 128)[0] * 2.0 - small.get(160, 128)[0]).abs() < 1e-5);
        s.effects.glow.spread = 0.0;
        let mut tight = source(256);
        apply(&mut tight, &s, 256.0);
        assert!(small.get(160, 128)[0] > tight.get(160, 128)[0]);
    }

    #[test]
    fn range_warmth_direction_and_diffusion_work() {
        let mut s = settings(GlowStyle::Bloom);
        s.effects.glow.range = 0.0;
        let src = Rgb32f::filled(32, 32, [0.4; 3]);
        let mut a = src.clone();
        apply(&mut a, &s, 32.0);
        assert_eq!(a, src);
        s.effects.glow.range = 100.0;
        s.effects.glow.warmth = 100.0;
        apply(&mut a, &s, 32.0);
        assert!(a.get(16, 16)[0] > a.get(16, 16)[2]);
        let mut diffuse = Rgb32f::filled(32, 32, [0.02; 3]);
        apply(&mut diffuse, &settings(GlowStyle::Diffusion), 32.0);
        assert!(diffuse.get(16, 16)[0] > 0.02);
        s = settings(GlowStyle::Motion);
        s.effects.glow.direction = 90.0;
        let mut out = lamp();
        apply(&mut out, &s, 129.0);
        assert!(out.get(64, 80)[0] > out.get(80, 64)[0]);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{DeepSamples, OutputDepth, PixelWindow, RenderRequest, SourceInfo, StageCache, render, render_cached};
    use std::sync::Arc;
    #[test]
    fn cached_deep_and_windowed_renders_use_glow() {
        let src = Arc::new(Rgb32f::from_fn(96, 64, |x, y| if x > 42 && x < 54 && y > 25 && y < 39 { [3.0; 3] } else { [0.01; 3] }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        let req = RenderRequest::fit(96, 64);
        let cache = StageCache::default();
        let before = render_cached(&src, &info, &s, &req, &cache);
        s.effects.glow.amount = 80.0;
        s.effects.glow.spread = 100.0;
        let after = render_cached(&src, &info, &s, &req, &cache);
        assert_ne!(before.image, after.image);
        assert_eq!(after.image, render(&src, &info, &s, &req).image);
        let win = PixelWindow { x: 32, y: 16, w: 32, h: 32 };
        let mut wr = req;
        wr.window = Some(win);
        assert_eq!(render(&src, &info, &s, &wr).image, after.image.crop(32, 16, 32, 32));
        let mut deep = req;
        deep.depth = OutputDepth::U16;
        let Some(d) = render(&src, &info, &s, &deep).deep else { panic!("missing deep render") };
        assert!(matches!(d.samples, DeepSamples::U16(_)));
        s.effects.glow.amount = 0.0;
        assert_eq!(before.image, render_cached(&src, &info, &s, &req, &cache).image);
    }
}
