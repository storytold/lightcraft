//! Process V8's heal (Heal spots, and Remove spots with their automatic source): the source keeps its
//! detail, and a smooth correction pins the patch to the photo on the spot's edge.
//!
//! The patch is `g · e^P` in log space, `g` the source: `P` solves Laplace's equation inside the spot,
//! with the log ratio of photo to source on the pixels just outside it as Dirichlet values. This is
//! the healing of T. Georgiev, *Photoshop Healing Brush: a Tool for Seamless Cloning* (ECCV 2004
//! workshop): the difference of photo and source, filled from its values on the boundary, added back
//! to the source; that is the membrane interpolant of P. Pérez, M. Gangnet, A. Blake, *Poisson Image
//! Editing* (SIGGRAPH 2003). Taking the ratio rather than the difference carries the source's texture
//! at its contrast relative to the new light instead of at its absolute amplitude (the relighting-
//! invariant form of T. Georgiev, *Image Reconstruction Invariant to Relighting*, Eurographics 2005).
//!
//! Two adjustments for photographs:
//! * the source's finest detail, mostly sensor noise (relatively stronger in shadows, so a ratio
//!   alone carries too little of it into a darker spot), is scaled to the level around the spot,
//!   measured with medians so that edges don't count;
//! * the edge values are a light blur of the outside, so that the noise of one ring of pixels doesn't
//!   ripple through the fill.
//!
//! The fill is `lightcraft_inpaint::membrane_fill` (cascadic SOR). See `docs/process-versions.md` (V8).

use lightcraft_color::luminance_2020;
use lightcraft_raster::Plane;
use lightcraft_raster::blur::gaussian;

/// Added before taking logs (scene-linear, diffuse white near 1): keeps black finite and makes the
/// heal additive rather than multiplicative far below it.
const LOG_FLOOR: f32 = 1.0 / 4096.0;
/// Fine detail is what a Gaussian blur of this many pixels removes; the edge values are blurred by
/// as much.
const DETAIL_SIGMA: f32 = 1.5;
/// The most the source's detail is weakened or strengthened.
const GAIN_RANGE: (f32, f32) = (0.5, 2.0);
/// Fewest pixels a detail level is measured on; with fewer, the source's detail is kept as it is.
const MIN_SAMPLES: usize = 32;
/// Where the detail around the spot is measured: this many radii from the stroke.
const AROUND: (f32, f32) = (1.05, 2.0);
/// Where the source's detail is measured: within this many radii of the stroke.
const WITHIN: f32 = 0.9;

/// A spot's working box, row-major `w × h`: the source read for each pixel, the photo, and each
/// pixel's distance to the stroke (output pixels); `r` is the spot's radius.
pub(super) struct Patch<'a> {
    pub w: usize,
    pub h: usize,
    pub src: &'a [[f32; 3]],
    pub tgt: &'a [[f32; 3]],
    pub dist: &'a [f32],
    pub r: f32,
}

/// A linear value into the log space the heal works in (negative values count as black).
fn enc(v: f32) -> f32 {
    (v.max(0.0) + LOG_FLOOR).ln()
}

/// A log-space value back to linear, never below black: the inverse of [`enc`].
fn dec(v: f32) -> f32 {
    (v.exp() - LOG_FLOOR).max(0.0)
}

/// The most pixels a spot's box may hold for the pinned heal. At its peak the heal keeps about 16
/// floats per pixel of the box alive (the source and edge ratio in log space, the solver's planes, the
/// output) on top of what [`super::apply`] holds: about 2 GB at this size, 512 MB on the web. A
/// bigger box, a stroke across most of a large frame at export size, gets the averaged heal instead
/// of an allocation that could fail.
pub(super) const MAX_PIXELS: usize = super::MAX_SPOT_SPAN * super::MAX_SPOT_SPAN / 2;

/// The healed box: inside the spot (distance below `r`) the source with its detail matched, pinned to
/// the photo at the spot's edge; outside it the photo. Inputs of mismatched sizes give the photo back;
/// `None` when the box holds more than [`MAX_PIXELS`] pixels (nothing is allocated then).
pub(super) fn heal(p: &Patch<'_>) -> Option<Vec<[f32; 3]>> {
    let n = p.w.checked_mul(p.h).filter(|n| *n <= MAX_PIXELS)?;
    if p.src.len() != n || p.tgt.len() != n || p.dist.len() != n {
        return Some(p.tgt.to_vec());
    }
    let cover: Vec<bool> = p.dist.iter().map(|d| *d < p.r).collect();
    let Some(sub) = SubBox::around(&cover, p.w, p.h) else { return Some(p.tgt.to_vec()) };
    let source = source_log(p, detail_gain(p, &cover));
    // (the edge ratio is freed before the output is allocated)
    let fill = solve(p, &cover, &edge_ratio(p, &cover, &source), &sub);
    let mut out = p.tgt.to_vec();
    for yy in 0..sub.h {
        for xx in 0..sub.w {
            let i = (sub.y + yy) * p.w + sub.x + xx;
            if !cover[i] {
                continue;
            }
            let j = (yy * sub.w + xx) * 3;
            let v: [f32; 3] = std::array::from_fn(|c| dec(source[c][i] + fill.get(j + c).copied().unwrap_or(0.0)));
            if v.iter().all(|c| c.is_finite()) {
                out[i] = v;
            }
        }
    }
    Some(out)
}

/// How much the source's fine detail is scaled so that it matches the photo's around the spot: the
/// median absolute fine detail of log luminance there over the same inside the source, within
/// [`GAIN_RANGE`]; 1 when either has too few pixels or no detail at all.
fn detail_gain(p: &Patch<'_>, cover: &[bool]) -> f32 {
    let fine = |px: &[[f32; 3]]| {
        let l = Plane { width: p.w, height: p.h, data: px.iter().map(|c| enc(luminance_2020(*c))).collect() };
        let base = gaussian(&l, DETAIL_SIGMA);
        l.data.iter().zip(&base.data).map(|(v, b)| (v - b).abs()).collect::<Vec<f32>>()
    };
    let (ft, fs) = (fine(p.tgt), fine(p.src));
    let (lo, hi) = (AROUND.0 * p.r, AROUND.1 * p.r);
    let around: Vec<f32> = ft.iter().zip(p.dist).zip(cover).filter(|((_, d), c)| !**c && **d >= lo && **d < hi).map(|((v, _), _)| *v).collect();
    let within: Vec<f32> = fs.iter().zip(p.dist).filter(|(_, d)| **d < WITHIN * p.r).map(|(v, _)| *v).collect();
    match (median(around), median(within)) {
        (Some(t), Some(s)) if t > 0.0 && s > 0.0 => (t / s).clamp(GAIN_RANGE.0, GAIN_RANGE.1),
        _ => 1.0,
    }
}

/// The median of `v`; `None` with fewer than [`MIN_SAMPLES`] values or a non-finite median.
fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.len() < MIN_SAMPLES {
        return None;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    Some(*m).filter(|m| m.is_finite())
}

/// The source in log space, per channel, its fine detail scaled by `gain`.
fn source_log(p: &Patch<'_>, gain: f32) -> [Vec<f32>; 3] {
    std::array::from_fn(|c| {
        let l = Plane { width: p.w, height: p.h, data: p.src.iter().map(|v| enc(v[c])).collect() };
        if (gain - 1.0).abs() < 1e-3 {
            return l.data;
        }
        let base = gaussian(&l, DETAIL_SIGMA);
        l.data.iter().zip(&base.data).map(|(v, b)| b + gain * (v - b)).collect()
    })
}

/// The log ratio of photo to source outside the spot, per channel, blurred over the outside only
/// (normalized convolution), so that the ring the fill is pinned to carries no single pixel's noise.
fn edge_ratio(p: &Patch<'_>, cover: &[bool], source: &[Vec<f32>; 3]) -> [Vec<f32>; 3] {
    let outside = Plane { width: p.w, height: p.h, data: cover.iter().map(|c| if *c { 0.0 } else { 1.0 }).collect() };
    let weight = gaussian(&outside, DETAIL_SIGMA);
    std::array::from_fn(|c| {
        let ratio = Plane {
            width: p.w,
            height: p.h,
            data: p.tgt.iter().zip(&source[c]).zip(cover).map(|((t, s), inside)| if *inside { 0.0 } else { enc(t[c]) - s }).collect(),
        };
        let blurred = gaussian(&ratio, DETAIL_SIGMA);
        blurred.data.iter().zip(&weight.data).map(|(v, m)| v / m.max(1e-4)).collect()
    })
}

/// The membrane fill of the edge ratio over `sub` (interleaved, 3 channels): the pixels next to the
/// spot (4-neighbours of it) are the only known values, so nothing farther out leaks in through the
/// solver's coarse levels.
fn solve(p: &Patch<'_>, cover: &[bool], edge: &[Vec<f32>; 3], sub: &SubBox) -> Vec<f32> {
    let mut field = vec![0.0f32; sub.w * sub.h * 3];
    let mut unknown = vec![true; sub.w * sub.h];
    for yy in 0..sub.h {
        for xx in 0..sub.w {
            let (x, y) = (sub.x + xx, sub.y + yy);
            if cover[y * p.w + x] || !touches(cover, p.w, p.h, x, y) {
                continue;
            }
            let j = yy * sub.w + xx;
            unknown[j] = false;
            for (c, e) in edge.iter().enumerate() {
                field[j * 3 + c] = e[y * p.w + x];
            }
        }
    }
    lightcraft_inpaint::membrane_fill(sub.w, sub.h, 3, &field, &unknown)
}

/// Whether pixel (x, y) has a 4-neighbour inside the spot.
fn touches(cover: &[bool], w: usize, h: usize, x: usize, y: usize) -> bool {
    let i = y * w + x;
    (x > 0 && cover[i - 1]) || (x + 1 < w && cover[i + 1]) || (y > 0 && cover[i - w]) || (y + 1 < h && cover[i + w])
}

/// The part of the box the fill works in: the spot's bounding box and the ring around it.
struct SubBox {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl SubBox {
    /// The bounding box of the `true` pixels of `cover` (`w × h`), grown by one pixel and kept inside
    /// the box; `None` when no pixel is covered.
    fn around(cover: &[bool], w: usize, h: usize) -> Option<SubBox> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, _) in cover.iter().enumerate().filter(|(_, c)| **c) {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if x0 == usize::MAX {
            return None;
        }
        let (x, y) = (x0.saturating_sub(1), y0.saturating_sub(1));
        Some(SubBox { x, y, w: (x1 + 2).min(w) - x, h: (y1 + 2).min(h) - y })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HealMethod, MAX_SPOT_SPAN, apply};
    use super::{MAX_PIXELS, Patch, heal};
    use crate::geometry::Frame;
    use lightcraft_develop::{DevelopSettings, Process, Spot, SpotMode};
    use lightcraft_geom::Point;
    use lightcraft_raster::Rgb32f;

    const W: usize = 256;

    /// A grey pixel of value `v`.
    fn grey(v: f32) -> [f32; 3] {
        [v; 3]
    }

    /// Uniform noise in -1..1 from a pixel position (no RNG state: the same image every run).
    fn noise(x: usize, y: usize, seed: u32) -> f32 {
        let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263) ^ seed.wrapping_mul(2_246_822_519);
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        h ^= h >> 16;
        (f64::from(h) / f64::from(u32::MAX)) as f32 * 2.0 - 1.0
    }

    /// Distance from the centre of pixel (x, y) to the point `c`.
    fn dist(x: usize, y: usize, c: (f32, f32)) -> f32 {
        (x as f32 + 0.5 - c.0).hypot(y as f32 + 0.5 - c.1)
    }

    /// A Heal spot of radius `r` px at `c` reading from `off` px away, hard-edged, on a square `W` image.
    fn heal_spot(c: (f32, f32), r: f32, off: (f32, f32)) -> Spot {
        let n = W as f64;
        Spot {
            mode: SpotMode::Heal,
            points: vec![Point::new(f64::from(c.0) / n, f64::from(c.1) / n)],
            size: f64::from(r) / n,
            feather: 0.0,
            opacity: 100.0,
            source_offset: Some(Point::new(f64::from(off.0) / n, f64::from(off.1) / n)),
        }
    }

    /// `img` with `spot` applied by `method`.
    fn healed(img: &Rgb32f, spot: Spot, method: HealMethod) -> Rgb32f {
        let mut out = img.clone();
        let frame = Frame::new(img.width, img.height, &DevelopSettings::default(), true);
        apply(&mut out, &[spot], &frame, frame.px_per_long(img.width), method);
        out
    }

    /// Green values of `img` at distances `lo..hi` from `c`.
    fn ring(img: &Rgb32f, c: (f32, f32), lo: f32, hi: f32) -> Vec<f32> {
        (0..img.height)
            .flat_map(|y| (0..img.width).map(move |x| (x, y)))
            .filter(|&(x, y)| (lo..hi).contains(&dist(x, y, c)))
            .map(|(x, y)| img.get(x, y)[1])
            .collect()
    }

    /// Mean and standard deviation of `v`.
    fn mean_std(v: &[f32]) -> (f32, f32) {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
    }

    /// The largest difference between `a` and `b` at distances `lo..hi` from `c`.
    fn max_err(a: &Rgb32f, b: &Rgb32f, c: (f32, f32), lo: f32, hi: f32) -> f32 {
        ring(a, c, lo, hi).iter().zip(ring(b, c, lo, hi)).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max)
    }

    /// `img` with a disc of `v` (radius `r` px) at `c`.
    fn blot(mut img: Rgb32f, c: (f32, f32), r: f32, v: f32) -> Rgb32f {
        for y in 0..img.height {
            for x in 0..img.width {
                if dist(x, y, c) < r {
                    img.set(x, y, grey(v));
                }
            }
        }
        img
    }

    /// V7 and older photos keep the averaged heal; V8 and new photos get the pinned one.
    #[test]
    fn the_process_picks_the_heal() {
        let on = |p: Process| DevelopSettings { process: p.version(), ..Default::default() };
        assert_eq!(HealMethod::of(&on(Process::V7)), HealMethod::Averaged);
        assert_eq!(HealMethod::of(&on(Process::V8)), HealMethod::Pinned);
        assert_eq!(HealMethod::of(&DevelopSettings::default()), HealMethod::Pinned, "new photos get the latest process");
    }

    /// Given a spot on a brightness ramp and a source on a flat area, the healed patch meets the
    /// ramp at the spot's edge (the averaged correction left a step of about 10 % there)
    #[test]
    fn a_heal_on_a_gradient_meets_it_at_the_edge() {
        let truth = Rgb32f::from_fn(W, W, |x, y| grey(if y < 100 { 0.3 } else { 0.3 + 0.004 * (x as f32 - 128.0) }));
        let (c, r) = ((128.0, 180.0), 20.0);
        let img = blot(truth.clone(), c, 0.7 * r, 0.02);
        let spot = heal_spot(c, r, (0.0, -130.0));
        let pinned = healed(&img, spot.clone(), HealMethod::Pinned);
        let averaged = healed(&img, spot, HealMethod::Averaged);
        let (edge_p, edge_a) = (max_err(&pinned, &truth, c, 0.8 * r, r), max_err(&averaged, &truth, c, 0.8 * r, r));
        assert!(edge_p < 0.006, "pinned edge error {edge_p}");
        assert!(edge_a > 0.02, "the averaged correction misses the edge ({edge_a}): the test would not catch a revert");
        assert!(max_err(&pinned, &truth, c, 0.0, r) < 0.01, "inside");
    }

    /// Given texture moved from a bright area into one 4 stops darker, the patch keeps the texture's
    /// contrast relative to its new light (added in linear light, it was 7x too strong, a third clipped)
    #[test]
    fn a_heal_into_a_shadow_keeps_the_texture_contrast() {
        let truth = Rgb32f::from_fn(W, W, |x, y| grey(if x < 128 { 0.8 } else { 0.05 } * (1.0 + 0.25 * noise(x, y, 7))));
        let (c, r) = ((192.0, 128.0), 20.0);
        let img = blot(truth.clone(), c, 0.7 * r, 0.3);
        let spot = heal_spot(c, r, (-128.0, 0.0));
        let relative = |img: &Rgb32f| {
            let (m, s) = mean_std(&ring(img, c, 0.0, 0.8 * r));
            s / m
        };
        let want = relative(&truth);
        let (p, a) = (relative(&healed(&img, spot.clone(), HealMethod::Pinned)), relative(&healed(&img, spot, HealMethod::Averaged)));
        assert!((p / want - 1.0).abs() < 0.25, "pinned texture contrast {p}, the photo's {want}");
        assert!(a > 3.0 * want, "the averaged heal's contrast {a}: the test would not catch a revert");
    }

    /// Given a dark line just outside the spot that its edge doesn't touch, the patch stays as light
    /// as its surroundings (the averaged correction pulled the line's darkness in)
    #[test]
    fn a_heal_ignores_what_does_not_touch_the_spot() {
        let (c, r) = ((128.0, 150.0), 20.0);
        let bar_x = (c.0 + 1.4 * r) as usize;
        let img = Rgb32f::from_fn(W, W, |x, y| {
            let bar = (bar_x..bar_x + 4).contains(&x) && (y as f32 - c.1).abs() < 1.5 * r;
            grey(if bar { 0.02 } else { 0.3 })
        });
        let spot = heal_spot(c, r, (0.0, -100.0));
        let darkest = |img: &Rgb32f| ring(img, c, 0.0, r).into_iter().fold(f32::MAX, f32::min);
        let (p, a) = (darkest(&healed(&img, spot.clone(), HealMethod::Pinned)), darkest(&healed(&img, spot, HealMethod::Averaged)));
        assert!(p > 0.297, "pinned: darkest {p}, the surroundings 0.3");
        assert!(a < 0.28, "the averaged heal's darkest {a}: the test would not catch a revert");
    }

    /// Given a source half a pixel off the grid, Heal and Clone keep the grain (bilinear reads
    /// between pixels halved it)
    #[test]
    fn sources_are_read_at_whole_pixels() {
        let img = Rgb32f::from_fn(W, W, |x, y| grey(0.3 * (1.0 + 0.2 * noise(x, y, 3))));
        let (c, r) = ((128.0, 128.0), 20.0);
        let (_, grain) = mean_std(&ring(&img, c, 0.0, 0.8 * r));
        for mode in [SpotMode::Heal, SpotMode::Clone] {
            let spot = Spot { mode, ..heal_spot(c, r, (40.5, 0.5)) };
            let kept = |method| mean_std(&ring(&healed(&img, spot.clone(), method), c, 0.0, 0.8 * r)).1 / grain;
            let (p, a) = (kept(HealMethod::Pinned), kept(HealMethod::Averaged));
            assert!(p > 0.9, "{mode:?}: whole-pixel source keeps {p} of the grain");
            assert!(a < 0.6, "{mode:?}: between pixels {a}: the test would not catch a revert");
        }
    }

    /// Given a source less grainy than the area around the spot, the patch takes the grain of its
    /// surroundings, not the source's
    #[test]
    fn a_heal_matches_the_grain_around_the_spot() {
        let img = Rgb32f::from_fn(W, W, |x, y| grey(0.3 * (1.0 + if x < 128 { 0.06 } else { 0.11 } * noise(x, y, 5))));
        let (c, r) = ((192.0, 128.0), 20.0);
        let spot = heal_spot(c, r, (-128.0, 0.0));
        let (_, around) = mean_std(&ring(&img, c, 1.2 * r, 2.0 * r));
        let grain = |method| mean_std(&ring(&healed(&img, spot.clone(), method), c, 0.0, 0.8 * r)).1 / around;
        let (p, a) = (grain(HealMethod::Pinned), grain(HealMethod::Averaged));
        assert!((0.85..1.2).contains(&p), "pinned: {p} of the surroundings' grain");
        assert!(a < 0.65, "the averaged heal keeps the source's grain ({a}): the test would not catch a revert");
    }

    /// Given the same scene rendered at two sizes, the half-size heal matches the full-size one,
    /// downsampled (settings are resolution independent)
    #[test]
    fn a_heal_matches_between_preview_and_export_size() {
        let scene = |n: usize| {
            let k = n as f32 / W as f32;
            let img = Rgb32f::from_fn(n, n, |x, y| grey(0.2 + 0.0015 * x as f32 / k + 0.001 * y as f32 / k));
            blot(img, (128.0 * k, 150.0 * k), 12.0 * k, 0.05)
        };
        let spot = heal_spot((128.0, 150.0), 18.0, (-60.0, -40.0));
        let frame = |n: usize| Frame::new(n, n, &DevelopSettings::default(), true);
        let (mut big, mut small) = (scene(2 * W), scene(W));
        let (fb, fs) = (frame(2 * W), frame(W));
        apply(&mut big, std::slice::from_ref(&spot), &fb, fb.px_per_long(2 * W), HealMethod::Pinned);
        apply(&mut small, &[spot], &fs, fs.px_per_long(W), HealMethod::Pinned);
        let mut worst = 0.0f32;
        for y in 130..170 {
            for x in 108..148 {
                let b = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|(dx, dy)| big.get(2 * x + dx, 2 * y + dy)[1]).sum::<f32>() / 4.0;
                worst = worst.max((b - small.get(x, y)[1]).abs());
            }
        }
        assert!(worst < 0.01, "largest difference {worst}");
    }

    /// The same photo and spot heal to the same pixels every time.
    #[test]
    fn a_heal_is_deterministic() {
        let img = Rgb32f::from_fn(W, W, |x, y| grey(0.3 * (1.0 + 0.2 * noise(x, y, 9))));
        let spot = Spot { mode: SpotMode::Remove, source_offset: None, ..heal_spot((100.0, 120.0), 15.0, (0.0, 0.0)) };
        assert_eq!(healed(&img, spot.clone(), HealMethod::Pinned).data, healed(&img, spot, HealMethod::Pinned).data);
    }

    /// Spots with NaN, infinite or far-off positions, sizes and sources render finite, non-negative pixels.
    #[test]
    fn hostile_heal_spots_render_finite_pixels() {
        let img = Rgb32f::from_fn(40, 30, |x, y| if x < 5 { [0.0; 3] } else { [x as f32 / 40.0, y as f32 / 30.0, 0.0] });
        let frame = Frame::new(40, 30, &DevelopSettings::default(), true);
        let hostile = [
            ((0.0, 0.0), 2.0, Some((0.1, 0.0))),
            ((-5.0, 9.0), 0.1, None),
            ((0.5, 0.5), 0.0, Some((f64::NAN, 0.0))),
            ((f64::NAN, 0.5), 0.1, None),
            ((0.5, 0.5), f64::INFINITY, Some((1e30, -1e30))),
            ((0.05, 0.5), 0.2, Some((0.5, 0.0))),
        ];
        for ((p, size, off), mode) in hostile.into_iter().flat_map(|h| [SpotMode::Heal, SpotMode::Remove, SpotMode::Clone].map(|m| (h, m))) {
            let mut out = img.clone();
            let spot =
                Spot { mode, points: vec![Point::new(p.0, p.1)], size, source_offset: off.map(|o| Point::new(o.0, o.1)), ..Default::default() };
            apply(&mut out, &[spot], &frame, frame.px_per_long(40), HealMethod::Pinned);
            assert!(out.data.iter().flatten().all(|v| v.is_finite() && *v >= 0.0), "{mode:?} at {p:?} size {size}");
        }
    }

    /// A box bigger than [`MAX_PIXELS`] (or one whose area overflows) is left to the averaged heal
    /// before anything is allocated; one at the limit is not.
    #[test]
    fn a_box_too_big_for_the_working_memory_is_left_to_the_averaged_heal() {
        let patch = |w, h| Patch { w, h, src: &[], tgt: &[], dist: &[], r: 1.0 };
        assert!(heal(&patch(MAX_PIXELS + 1, 1)).is_none());
        assert!(heal(&patch(MAX_SPOT_SPAN, MAX_SPOT_SPAN)).is_none());
        assert!(heal(&patch(usize::MAX, 2)).is_none());
        // (at the limit, the empty slices are a size mismatch: the photo comes back as it is)
        assert_eq!(heal(&patch(MAX_PIXELS, 1)), Some(Vec::new()));
    }
}
