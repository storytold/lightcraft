//! Lateral chromatic aberration corrected on a Bayer mosaic, before demosaic.
//!
//! A lens with lateral CA images red and blue at slightly different magnifications than green: where green's image
//! of a point sits at sensor radius ρ about the optical centre, red's sits at ρ·(1 + red(ρ)) and blue's at
//! ρ·(1 + blue(ρ)). [`LateralCa`] holds that map (a knot table per colour). It is corrected where it lives, on the raw
//! mosaic: every red (blue) site is resampled from red (blue) sites only, at the position the map gives, so that red
//! and blue line up with green before any demosaic. Corrected after demosaic, it overshoots: the colour-difference
//! demosaics (AHD, PPG) borrow green's edges for red and blue, so the picture shows only part of the mosaic's CA.
//!
//! - **Where it runs:** [`crate::RawImage::normalized`], right after `OpcodeList2`, so every demosaic and the AI
//!   denoiser see the corrected mosaic. Binned developments ([`crate::RawImage::develop_binned`]) bin the mosaic
//!   without it and resample the binned image's red and blue instead (same kernel; the clip mask stands in for the
//!   clip level), so previews match full-size developments and no path corrects twice. The map rides in
//!   [`crate::OpcodeLists::lateral_ca`] next to the DNG opcode lists, but it is not a DNG opcode and is never
//!   written to a DNG.
//! - **Kernel:** Catmull-Rom (cubic convolution, a = −0.5), 4 × 4 taps on the colour's half-resolution plane, the
//!   result clamped to the [min, max] of the central 2 × 2 taps (no ringing, no new extremes). It interpolates, so
//!   where the map is 0 a site keeps its value. The radial scale comes from a 4097-entry table over ρ²: no square
//!   root or division per site.
//! - **Highlights:** a site that is clipped (at or above [`highlight::CLIP`]) or non-finite, or whose 4 × 4 support
//!   holds such a sample, keeps its own value, so a clipped sample never turns into a below-clip value that
//!   highlight reconstruction would take for real colour. Such sites stay uncorrected. The support covers the
//!   site's own cell only for shifts under two plane cells (4 sensor px); the check on the site itself covers
//!   larger ones.
//! - **Never crash:** a map that fails validation, a non-Bayer or multi-plane mosaic, one under 4 × 4 or with a data
//!   length that does not match its size is left alone; so is a binned image with k == 0 or a mask of the wrong
//!   length.
//! - **Memory:** one colour at a time: its plane is copied (¼ of the mosaic, or the binned image's size) and dropped
//!   before the next.

use crate::{Normalized, Rect, Rgb32f, highlight};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Largest |relative radial scale| a map may hold: 15 px at a 7500 px radius. (The ILCE-7RM6's tables reach
/// 1724 / 3.36·10⁶ ≈ 5.1·10⁻⁴ over the 54 measured photos.)
pub const MAX_SCALE: f64 = 2e-3;
/// Most knots per colour.
const MAX_KNOTS: usize = 64;
/// Intervals of the radial table over ρ².
const LUT: usize = 4096;

/// Lateral chromatic aberration of a Bayer mosaic: where green's image of a point sits at sensor radius ρ about
/// `center`, red's sits at ρ·(1 + red(ρ)) and blue's at ρ·(1 + blue(ρ)). Applied to the mosaic before demosaic
/// ([`crate::RawImage::normalized`], after `OpcodeList2`) and to binned images; not a DNG opcode, never written
/// to a DNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LateralCa {
    /// Active-area pixel-index coordinates (the convention of `WarpRectilinear`'s centre × (size − 1)).
    pub center: [f64; 2],
    /// Knot i sits at radius i · spacing px; linear in between, the last segment extended.
    pub spacing: f64,
    /// Relative radial scale of red / blue at each knot (Sony: table value / 3.36·10⁶).
    pub red: Vec<f64>,
    pub blue: Vec<f64>,
}

impl LateralCa {
    /// `None` unless the centre is finite, `spacing` finite and ≥ 1, each colour has 2–64 knots all within
    /// ±MAX_SCALE (NaN fails), and not every knot is zero.
    pub fn new(center: [f64; 2], spacing: f64, red: Vec<f64>, blue: Vec<f64>) -> Option<LateralCa> {
        let ca = LateralCa { center, spacing, red, blue };
        ca.valid().then_some(ca)
    }

    /// The rules of [`Self::new`].
    fn valid(&self) -> bool {
        // a NaN fails the comparison
        let knots = |k: &[f64]| (2..=MAX_KNOTS).contains(&k.len()) && k.iter().all(|v| v.abs() <= MAX_SCALE);
        self.center.iter().all(|c| c.is_finite())
            && self.spacing.is_finite()
            && self.spacing >= 1.0
            && knots(&self.red)
            && knots(&self.blue)
            && self.red.iter().chain(&self.blue).any(|&v| v != 0.0)
    }

    /// [`Self::valid`], and the centre lies within one extent of the rectangle [x0, x1] × [y0, y1] (active-area px).
    fn applies_to(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
        let ([cx, cy], w, h) = (self.center, x1 - x0, y1 - y0);
        self.valid() && (x0 - w..=x1 + w).contains(&cx) && (y0 - h..=y1 + h).contains(&cy)
    }

    /// Resample every red and blue site of a Bayer mosaic (cpp 1) from its own colour's sites; green untouched.
    /// No-op unless the map is valid (checked again: a deserialised map never went through `new`) and its centre
    /// lies within one image size of the mosaic (x in [−w, 2w], y in [−h, 2h]).
    pub(crate) fn correct_mosaic(&self, n: &mut Normalized) {
        let (w, h) = (n.width, n.height);
        let Some(cfa) = n.cfa.as_ref().filter(|c| c.is_bayer()) else { return };
        if n.cpp != 1 || w < 4 || h < 4 || n.data.len() != w.saturating_mul(h) || !self.applies_to(0.0, 0.0, w as f64, h as f64) {
            return;
        }
        let [cx, cy] = self.center;
        let (xm, ym) = ((cx.abs().max((w as f64 - 1.0 - cx).abs())), (cy.abs().max((h as f64 - 1.0 - cy).abs())));
        let rho2_max = xm * xm + ym * ym;
        for (colour, knots) in [(0, &self.red), (2, &self.blue)] {
            let Some(i) = cfa.pattern.iter().position(|&c| c == colour) else { continue };
            let (ox, oy) = (i % 2, i / 2);
            let plane = Plane::from_mosaic(&n.data, w, h, ox, oy);
            let radial = Radial::new(knots, self.spacing, rho2_max);
            // dx² of the colour's columns once; per row the scales first (a tight loop), then the samples
            let dx2: Vec<f64> = (ox..w).step_by(2).map(|x| (x as f64 - cx) * (x as f64 - cx)).collect();
            n.data.par_chunks_mut(w).enumerate().filter(|(y, _)| y % 2 == oy).for_each_init(
                || vec![0.0; dx2.len()],
                |scales, (y, row)| {
                    let dy = y as f64 - cy;
                    for (s, d2) in scales.iter_mut().zip(&dx2) {
                        *s = radial.at(d2 + dy * dy);
                    }
                    // the site's plane row: (y − oy) / 2 == y / 2
                    let v = (y / 2) as f64;
                    for ((x, site), s) in row.iter_mut().enumerate().skip(ox).step_by(2).zip(scales.iter()) {
                        // a clipped site stays (its own cell leaves the support beyond two plane cells of shift)
                        if !(site.is_finite() && *site < highlight::CLIP) {
                            continue;
                        }
                        let dx = x as f64 - cx;
                        if let Some(c) = plane.sample((x / 2) as f64 + dx * s / 2.0, v + dy * s / 2.0) {
                            *site = c;
                        }
                    }
                },
            );
        }
    }

    /// The map on a binned image ([`crate::RawImage::develop_binned`]): pixel (bx, by) sits at active-area position
    /// (crop.x + bx·k + (k − 1)/2, crop.y + by·k + (k − 1)/2); its red and blue channels are resampled with the
    /// mosaic's kernel at (bx, by) + d / k (the colours' sub-block offsets are ignored, as binning does). A pixel that
    /// has that colour's bit in `mask` ([`crate::highlight::CLIPPED_R`] / `_B`) or is not finite, or whose support
    /// holds such a pixel, keeps its value (the first rule covers shifts that take the pixel's own cell out of its
    /// support). No-op for k == 0, an empty image, a mask or data length that doesn't match the image, or a map that
    /// doesn't apply.
    pub(crate) fn correct_binned(&self, img: &mut Rgb32f, mask: &[u8], k: usize, crop: Rect) {
        let (w, h, kf) = (img.width, img.height, k as f64);
        let (x0, y0) = (crop.x as f64, crop.y as f64);
        let (x1, y1) = (x0 + w as f64 * kf, y0 + h as f64 * kf);
        if k == 0 || w == 0 || h == 0 || mask.len() != img.data.len() || img.data.len() != w.saturating_mul(h) || !self.applies_to(x0, y0, x1, y1) {
            return;
        }
        let [cx, cy] = self.center;
        let (xm, ym) = ((x0 - cx).abs().max((x1 - cx).abs()), (y0 - cy).abs().max((y1 - cy).abs()));
        let (half, inv_k) = ((kf - 1.0) / 2.0, 1.0 / kf);
        let dx: Vec<f64> = (0..w).map(|bx| x0 + bx as f64 * kf + half - cx).collect();
        for (ch, bit, knots) in [(0, highlight::CLIPPED_R, &self.red), (2, highlight::CLIPPED_B, &self.blue)] {
            let v = img.data.par_iter().zip(mask).map(|(p, &m)| if m & bit == 0 && p[ch].is_finite() { p[ch] } else { f32::INFINITY }).collect();
            let plane = Plane { w, h, v };
            let radial = Radial::new(knots, self.spacing, xm * xm + ym * ym);
            img.data.par_chunks_mut(w).enumerate().for_each_init(
                || vec![0.0; w],
                |scales, (by, row)| {
                    let dy = y0 + by as f64 * kf + half - cy;
                    for (s, d) in scales.iter_mut().zip(&dx) {
                        *s = radial.at(d * d + dy * dy);
                    }
                    for (bx, ((p, d), s)) in row.iter_mut().zip(&dx).zip(scales.iter()).enumerate() {
                        // a clipped pixel stays (its own cell leaves the support beyond two binned px of shift)
                        if !plane.v.get(by * w + bx).is_some_and(|o| o.is_finite()) {
                            continue;
                        }
                        if let Some(c) = plane.sample(bx as f64 + d * s * inv_k, by as f64 + dy * s * inv_k) {
                            p[ch] = c;
                        }
                    }
                },
            );
        }
    }
}

/// The knot table at radius `r` px: knot `i` at `i · spacing`, linear in between, the last segment extended beyond
/// the last knot. NaN with fewer than two knots.
pub(crate) fn interpolate(knots: &[f64], spacing: f64, r: f64) -> f64 {
    let t = r / spacing;
    // `as` saturates, so a huge `t` lands on the last segment; NaN fails `t > 0`
    let i = if t > 0.0 { (t as usize).min(knots.len().saturating_sub(2)) } else { 0 };
    match (knots.get(i), knots.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * (t - i as f64),
        _ => f64::NAN,
    }
}

/// A colour's relative radial scale as a table over ρ² in [0, `rho2_max`].
struct Radial {
    /// Table entries per unit of ρ².
    per_rho2: f64,
    lut: Vec<f64>,
}

impl Radial {
    /// [`LUT`] + 1 entries of [`interpolate`] at ρ = √(i / LUT · `rho2_max`), each clamped to ±[`MAX_SCALE`]: the
    /// extended last segment of a valid but steep table can't run away at the corners.
    fn new(knots: &[f64], spacing: f64, rho2_max: f64) -> Radial {
        let lut = (0..=LUT).map(|i| interpolate(knots, spacing, (i as f64 / LUT as f64 * rho2_max).sqrt()).clamp(-MAX_SCALE, MAX_SCALE)).collect();
        Radial { per_rho2: if rho2_max > 0.0 { LUT as f64 / rho2_max } else { 0.0 }, lut }
    }

    /// The scale at squared radius `rho2`: linear in ρ² between entries, the last entry beyond the table.
    fn at(&self, rho2: f64) -> f64 {
        let t = (rho2 * self.per_rho2).clamp(0.0, LUT as f64);
        let i = (t as usize).min(LUT - 1);
        match (self.lut.get(i), self.lut.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (t - i as f64),
            _ => f64::NAN,
        }
    }
}

/// One colour's samples of a mosaic as a dense grid; clipped (≥ [`highlight::CLIP`]) and non-finite samples are
/// stored as +∞, so one finiteness test of a sample implements the clip rule.
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    /// The samples at `(ox + 2i, oy + 2j)` of a `w × h` mosaic `data` (`data.len() == w · h`).
    fn from_mosaic(data: &[f32], w: usize, h: usize, ox: usize, oy: usize) -> Plane {
        let (pw, ph) = (w.saturating_sub(ox).div_ceil(2), h.saturating_sub(oy).div_ceil(2));
        let mut v = vec![f32::INFINITY; pw * ph];
        v.par_chunks_mut(pw.max(1)).enumerate().for_each(|(j, row)| {
            for (i, p) in row.iter_mut().enumerate() {
                if let Some(&s) = data.get((oy + 2 * j) * w + ox + 2 * i)
                    && s.is_finite()
                    && s < highlight::CLIP
                {
                    *p = s;
                }
            }
        });
        Plane { w: pw, h: ph, v }
    }

    /// Catmull-Rom (a = −0.5) at plane position `(u, v)` on the taps `floor − 1 ..= floor + 2` per axis (indices
    /// clamped into the grid), clamped to the [min, max] of the central 2 × 2 taps. `None` when `(u, v)` is not
    /// finite or a tap is not (clipped).
    fn sample(&self, u: f64, v: f64) -> Option<f32> {
        if !u.is_finite() || !v.is_finite() || self.w == 0 || self.h == 0 {
            return None;
        }
        let (fu, fv) = (floor(u), floor(v));
        if fu >= 1.0 && fv >= 1.0 && fu + 2.0 < self.w as f64 && fv + 2.0 < self.h as f64 {
            // inside the grid: four taps per row with one `get`, in f32
            let ([w0, w1, w2, w3], wv) = (catmull_rom(u - fu).map(|w| w as f32), catmull_rom(v - fv).map(|w| w as f32));
            let start = (fv as usize - 1) * self.w + fu as usize - 1;
            let (mut acc, mut lo, mut hi) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
            for (j, wj) in wv.iter().enumerate() {
                let s = start + j * self.w;
                let [a, b, c, d]: [f32; 4] = self.v.get(s..s + 4)?.try_into().ok()?;
                acc += wj * (w0 * a + w1 * b + w2 * c + w3 * d);
                if j == 1 || j == 2 {
                    lo = lo.min(b).min(c);
                    hi = hi.max(b).max(c);
                }
            }
            // as below; a finite sum overflowing f32 (taps near 10³⁸) keeps the site too
            return acc.is_finite().then(|| acc.max(lo).min(hi));
        }
        let (wu, wv) = (catmull_rom(u - fu), catmull_rom(v - fv));
        // tap indices floor − 1 ..= floor + 2, clamped in f64 (no integer overflow for far positions)
        let taps = |f: f64, n: usize| [-1.0, 0.0, 1.0, 2.0].map(|k| (f + k).clamp(0.0, n.saturating_sub(1) as f64) as usize);
        let (cols, rows) = (taps(fu, self.w), taps(fv, self.h));
        let (mut acc, mut lo, mut hi) = (0.0f64, f32::INFINITY, f32::NEG_INFINITY);
        for (j, (&r, wj)) in rows.iter().zip(wv).enumerate() {
            let row = self.v.get(r * self.w..)?;
            let mut sum = 0.0;
            for (i, (&c, wi)) in cols.iter().zip(wu).enumerate() {
                let t = *row.get(c)?;
                sum += wi * t as f64;
                if (1..=2).contains(&i) && (1..=2).contains(&j) {
                    lo = lo.min(t);
                    hi = hi.max(t);
                }
            }
            acc += wj * sum;
        }
        // one test for all 16 taps: a +∞ (clipped) tap makes the sum ±∞, or NaN (∞ · 0, ∞ − ∞)
        acc.is_finite().then(|| (acc as f32).max(lo).min(hi))
    }
}

/// `x.floor()` for |x| < 2⁶³ (beyond, a finite value whose weights then make the sample `None`), without the libm
/// call `f64::floor` compiles to on baseline x86-64.
fn floor(x: f64) -> f64 {
    let t = x as i64 as f64;
    if t > x { t - 1.0 } else { t }
}

/// Catmull-Rom weights of the taps at −1, 0, 1, 2 for a position `t` in [0, 1) past tap 0.
fn catmull_rom(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [(-t3 + 2.0 * t2 - t) / 2.0, (3.0 * t3 - 5.0 * t2 + 2.0) / 2.0, (-3.0 * t3 + 4.0 * t2 + t) / 2.0, (t3 - t2) / 2.0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binned::tests::raw_from;
    use crate::demosaic::mosaic_from_rgb;
    use crate::opcodes::Area;
    use crate::{Cfa, Method, Opcode, RawImage, Rect, Rgb32f, highlight};

    /// A smooth field (periods ≥ 140 px).
    fn field(x: f64, y: f64) -> f64 {
        0.4 + 0.25 * (x / 31.0).sin() * (y / 23.0).cos() + 0.1 * ((x + y) / 41.0).sin()
    }

    /// `field` with red magnified by `ar` and blue by `ab` about `c` (red's image of a point at radius ρ sits at ρ·(1 + ar)).
    fn smooth(w: usize, h: usize, c: [f64; 2], ar: f64, ab: f64) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (x, y) = (x as f64, y as f64);
            let at = |a: f64| field(c[0] + (x - c[0]) / (1.0 + a), c[1] + (y - c[1]) / (1.0 + a)) as f32;
            [at(ar), at(0.0), at(ab)]
        })
    }

    /// Grey discs centred on `c` (48 px cells, radius 14 px, 8 px soft edges: point-symmetric about `c`), red
    /// magnified by `ar` and blue by `ab` about `c`.
    fn discs(w: usize, h: usize, c: [f64; 2], ar: f64, ab: f64) -> Rgb32f {
        let val = |x: f64, y: f64| {
            let f = |q: f64| (q / 48.0 + 0.5).rem_euclid(1.0) - 0.5;
            let d = f(x - c[0]).hypot(f(y - c[1])) * 48.0 - 14.0;
            (0.15 + 0.5 * (0.5 + (d / 8.0).clamp(-0.5, 0.5))) as f32
        };
        Rgb32f::from_fn(w, h, |x, y| {
            let (x, y) = (x as f64, y as f64);
            let at = |a: f64| val(c[0] + (x - c[0]) / (1.0 + a), c[1] + (y - c[1]) / (1.0 + a));
            [at(ar), at(0.0), at(ab)]
        })
    }

    /// Least-squares radial shift (px, outward positive) of the `colour` sites of `n` against `f` (green's
    /// geometry), over sites beyond 0.6 of the half diagonal and 8 px inside the edges: v ≈ f(p − δ·u).
    fn site_shift(n: &Normalized, colour: u8, c: [f64; 2], f: &dyn Fn(f64, f64) -> f64) -> f64 {
        let cfa = n.cfa.as_ref().unwrap();
        let hd = (n.width as f64).hypot(n.height as f64) / 2.0;
        let (mut num, mut den) = (0.0, 0.0);
        for y in 8..n.height - 8 {
            for x in 8..n.width - 8 {
                let (px, py) = (x as f64, y as f64);
                let (dx, dy) = (px - c[0], py - c[1]);
                let r = dx.hypot(dy);
                if cfa.color_at(x, y) != colour || r < 0.6 * hd {
                    continue;
                }
                let gr = ((f(px + 0.5, py) - f(px - 0.5, py)) * dx + (f(px, py + 0.5) - f(px, py - 0.5)) * dy) / r;
                num += (n.data[y * n.width + x] as f64 - f(px, py)) * gr;
                den += gr * gr;
            }
        }
        -num / den
    }

    /// The same against green inside a developed picture: C ≈ G(p − δ·u), `border` px from the edges excluded.
    fn radial_shift(img: &Rgb32f, ch: usize, c: [f64; 2], border: usize) -> f64 {
        let hd = (img.width as f64).hypot(img.height as f64) / 2.0;
        let g = |x: usize, y: usize| img.get(x, y)[1] as f64;
        let (mut num, mut den) = (0.0, 0.0);
        for y in border..img.height - border {
            for x in border..img.width - border {
                let (dx, dy) = (x as f64 - c[0], y as f64 - c[1]);
                let r = dx.hypot(dy);
                if r < 0.6 * hd {
                    continue;
                }
                let gr = ((g(x + 1, y) - g(x - 1, y)) * dx + (g(x, y + 1) - g(x, y - 1)) * dy) / (2.0 * r);
                num += (img.get(x, y)[ch] as f64 - g(x, y)) * gr;
                den += gr * gr;
            }
        }
        -num / den
    }

    #[test]
    fn magnified_red_and_blue_line_up_with_green() {
        let (w, h, c) = (2000, 1400, [999.5, 699.5]);
        let (ar, ab) = (MAX_SCALE, -1.5e-3);
        let mut n = mosaic_from_rgb(&smooth(w, h, c, ar, ab), &Cfa::bayer("GRBG").unwrap());
        // before: red sits outward by about ar · r (≈ 1.8 px at the measured radii), blue inward
        let (r0, b0) = (site_shift(&n, 0, c, &field), site_shift(&n, 2, c, &field));
        assert!(r0 > 1.0 && b0 < -0.8, "uncorrected: red {r0}, blue {b0} px");
        LateralCa::new(c, 400.0, vec![ar; 2], vec![ab; 2]).unwrap().correct_mosaic(&mut n);
        for colour in [0, 2] {
            let s = site_shift(&n, colour, c, &field);
            assert!(s.abs() < 0.05, "colour {colour}: {s} px after (red {r0}, blue {b0} before)");
        }
        // every interior red and blue site reads the field at its own position (green's geometry)
        let cfa = n.cfa.clone().unwrap();
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                if cfa.color_at(x, y) != 1 {
                    let v = n.data[y * w + x] as f64;
                    assert!((v - field(x as f64, y as f64)).abs() < 1e-3, "({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn green_sites_and_constant_colours_are_untouched() {
        let ca = LateralCa::new([47.5, 31.5], 8.0, vec![MAX_SCALE, -MAX_SCALE], vec![-MAX_SCALE, MAX_SCALE]).unwrap();
        let cfa = Cfa::bayer("BGGR").unwrap();
        // varying R/B: they move, green stays bit-identical
        let before = mosaic_from_rgb(&smooth(96, 64, [47.5, 31.5], 0.0, 0.0), &cfa);
        let mut n = before.clone();
        ca.correct_mosaic(&mut n);
        assert_ne!(n, before);
        for (i, (a, b)) in n.data.iter().zip(&before.data).enumerate() {
            if cfa.color_at(i % 96, i / 96) == 1 {
                assert_eq!(a.to_bits(), b.to_bits(), "green at {i}");
            }
        }
        // constant R and B (varying G): nothing changes at all (the clamp to the 2 × 2 core keeps them exact)
        let flat = Rgb32f::from_fn(96, 64, |x, y| [0.3, 0.2 + 0.01 * ((x * 7 + y * 3) % 11) as f32, 0.6]);
        let before = mosaic_from_rgb(&flat, &cfa);
        let mut n = before.clone();
        ca.correct_mosaic(&mut n);
        assert_eq!(n, before);
    }

    #[test]
    fn tiny_displacement_is_identity() {
        let before = mosaic_from_rgb(&smooth(96, 64, [47.5, 31.5], 0.0, 0.0), &Cfa::bayer("RGGB").unwrap());
        let mut n = before.clone();
        LateralCa::new([47.5, 31.5], 400.0, vec![1e-12; 2], vec![-1e-12; 2]).unwrap().correct_mosaic(&mut n);
        // bit-identical: the kernel interpolates, and offsets under 1e-10 px are below f32 rounding
        assert_eq!(n, before);
    }

    #[test]
    fn clipped_support_keeps_the_site() {
        // a field at most 0.5 with a blown area (1.2): no clipped sample may become a below-clip value
        let field = |x: usize| 0.3 + 0.2 * (x as f32 / 9.0).sin();
        let disc = Rgb32f::from_fn(256, 192, |x, y| [if (x as f64 - 200.0).hypot(y as f64 - 150.0) < 12.0 { 1.2 } else { field(x) }; 3]);
        // shifts up to 0.32 px; then about 5 px at a 3 px stripe 2500 px from the centre (more than two plane cells:
        // the site's own cell is outside its support)
        let stripe = Rgb32f::from_fn(3000, 64, |x, _| [if (2499..2502).contains(&x) { 1.2 } else { field(x) }; 3]);
        for (scene, c, a) in [(disc, [127.5, 95.5], [MAX_SCALE, -MAX_SCALE]), (stripe, [0.0, 31.5], [MAX_SCALE, MAX_SCALE])] {
            let before = mosaic_from_rgb(&scene, &Cfa::bayer("RGGB").unwrap());
            let mut n = before.clone();
            LateralCa::new(c, 50.0, vec![a[0]; 2], vec![a[1]; 2]).unwrap().correct_mosaic(&mut n);
            let clipped = |n: &Normalized| n.data.iter().map(|&v| v >= highlight::CLIP).collect::<Vec<_>>();
            assert_eq!(clipped(&n), clipped(&before), "{}: no clipped sample appears or disappears", n.width);
            assert!(n.data.iter().all(|&v| v <= 0.5 + 1e-6 || v >= highlight::CLIP), "{}: a clipped sample leaked below clip", n.width);
            assert_ne!(n.data, before.data, "{}: elsewhere red and blue moved", n.width);
        }
    }

    #[test]
    fn hostile_tables_are_refused() {
        let (ok, c) = (vec![1e-4, 2e-4], [10.0, 10.0]);
        let cases: Vec<(&str, [f64; 2], f64, Vec<f64>, Vec<f64>)> = vec![
            ("NaN knot", c, 400.0, vec![f64::NAN, 1e-4], ok.clone()),
            ("inf knot", c, 400.0, ok.clone(), vec![f64::INFINITY, 1e-4]),
            ("-inf knot", c, 400.0, vec![f64::NEG_INFINITY, 1e-4], ok.clone()),
            ("1e300", c, 400.0, vec![1e300, 1e-4], ok.clone()),
            ("beyond the bound", c, 400.0, vec![MAX_SCALE * 1.001, 0.0], ok.clone()),
            ("no knots", c, 400.0, vec![], ok.clone()),
            ("one knot", c, 400.0, vec![1e-4], ok.clone()),
            ("65 knots", c, 400.0, vec![1e-4; 65], ok.clone()),
            ("10 000 knots", c, 400.0, ok.clone(), vec![1e-4; 10_000]),
            ("spacing 0", c, 0.0, ok.clone(), ok.clone()),
            ("spacing 0.5", c, 0.5, ok.clone(), ok.clone()),
            ("negative spacing", c, -400.0, ok.clone(), ok.clone()),
            ("NaN spacing", c, f64::NAN, ok.clone(), ok.clone()),
            ("inf spacing", c, f64::INFINITY, ok.clone(), ok.clone()),
            ("NaN centre", [f64::NAN, 0.0], 400.0, ok.clone(), ok.clone()),
            ("inf centre", [0.0, f64::INFINITY], 400.0, ok.clone(), ok.clone()),
            ("all zero", c, 400.0, vec![0.0; 2], vec![0.0; 2]),
        ];
        for (what, c, s, r, b) in cases {
            assert!(LateralCa::new(c, s, r, b).is_none(), "{what}");
        }
        assert!(LateralCa::new(c, 1.0, vec![MAX_SCALE, -MAX_SCALE], vec![0.0, 0.0]).is_some(), "at the bound");
        // a map that bypassed `new` (deserialised) is checked again where it applies: no change, no panic
        let valid = LateralCa::new([47.5, 31.5], 8.0, vec![MAX_SCALE; 2], vec![-MAX_SCALE; 2]).unwrap();
        let before = mosaic_from_rgb(&smooth(96, 64, [47.5, 31.5], 0.0, 0.0), &Cfa::bayer("RGGB").unwrap());
        for bad in [
            LateralCa { red: vec![f64::NAN; 2], ..valid.clone() },
            LateralCa { red: vec![], ..valid.clone() },
            LateralCa { spacing: 0.0, ..valid.clone() },
            LateralCa { center: [1e9, 0.0], ..valid.clone() },
            LateralCa { center: [47.5, -200.0], ..valid.clone() },
        ] {
            let mut n = before.clone();
            bad.correct_mosaic(&mut n);
            assert_eq!(n, before, "{bad:?}");
        }
    }

    #[test]
    fn tiny_and_odd_mosaics_are_left_alone() {
        let ca = LateralCa::new([2.0, 2.0], 1.0, vec![MAX_SCALE; 2], vec![MAX_SCALE; 2]).unwrap();
        let ramp = |w, h| Rgb32f::from_fn(w, h, |x, y| [(x + y) as f32 / 20.0; 3]);
        let bayer = Cfa::bayer("RGGB").unwrap();
        for (w, h, cfa) in [(1, 1, &bayer), (2, 2, &bayer), (3, 5, &bayer), (12, 12, &Cfa::xtrans())] {
            let before = mosaic_from_rgb(&ramp(w, h), cfa);
            let mut n = before.clone();
            ca.correct_mosaic(&mut n);
            assert_eq!(n, before, "{w}×{h} {}", cfa.name());
        }
        for before in [
            Normalized { width: 8, height: 8, cpp: 3, data: vec![0.5; 192], cfa: Some(bayer.clone()) },
            Normalized { width: 8, height: 8, cpp: 1, data: vec![0.5; 64], cfa: None },
            Normalized { width: 8, height: 8, cpp: 1, data: vec![0.5; 63], cfa: Some(bayer.clone()) },
        ] {
            let mut n = before.clone();
            ca.correct_mosaic(&mut n);
            assert_eq!(n, before);
        }
        // odd sizes above the minimum run (no panic), green untouched
        let before = mosaic_from_rgb(&ramp(5, 7), &bayer);
        let mut n = before.clone();
        ca.correct_mosaic(&mut n);
        for (i, (a, b)) in n.data.iter().zip(&before.data).enumerate() {
            if bayer.color_at(i % 5, i / 5) == 1 {
                assert_eq!(a.to_bits(), b.to_bits(), "green at {i}");
            }
        }
    }

    #[test]
    fn lut_matches_direct_evaluation() {
        // the ILCE-7RM6 16–35 mm CA table (red half) on the full-frame knot spacing, out to the padded lossless corner
        let red: Vec<f64> =
            [242., 234., 224., 212., 196., 176., 154., 132., 108., 86., 66., 46., 30., 16., 8., 4.].iter().map(|v| v / 3.36e6).collect();
        let spacing = 9984f64.hypot(6656.0) / 30.0;
        let rho_max = 5120f64.hypot(3584.0);
        let lut = Radial::new(&red, spacing, rho_max * rho_max);
        for i in 0..=10_000 {
            let rho = rho_max * i as f64 / 10_000.0;
            let err = (lut.at(rho * rho) - interpolate(&red, spacing, rho)) * rho;
            assert!(err.abs() < 1e-4, "{rho}: {err} px");
        }
        // a steep valid table's extended last segment stays within the bound
        assert_eq!(Radial::new(&[MAX_SCALE, -MAX_SCALE], 1.0, 1e8).at(1e8), -MAX_SCALE);
    }

    #[test]
    fn normalized_applies_the_map_after_list2() {
        let (w, h) = (96, 64);
        let mut raw = raw_from(&mosaic_from_rgb(&smooth(w, h, [47.5, 31.5], 0.0, 0.0), &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let area = Area { top: 0, left: 0, bottom: h as u32, right: w as u32, plane: 0, planes: 1, row_pitch: 1, col_pitch: 1 };
        // row scales that don't commute with a 2-D resample: the order is visible
        raw.opcodes.list2 = vec![Opcode::ScalePerRow { area, scales: (0..h).map(|y| 1.0 + 0.05 * (y % 3) as f32).collect() }];
        let plain = raw.normalized().unwrap();
        let ca = LateralCa::new([47.5, 31.5], 8.0, vec![MAX_SCALE; 2], vec![-MAX_SCALE; 2]).unwrap();
        raw.opcodes.lateral_ca = Some(ca.clone());
        let mut want = plain.clone();
        ca.correct_mosaic(&mut want);
        let got = raw.normalized().unwrap();
        assert_eq!(got, want);
        assert_ne!(got, plain);
    }

    /// Spec criterion 4: after the correction and AHD, red and blue line up with green at the corners.
    #[test]
    fn lateral_ca_is_corrected_before_demosaic() {
        let (w, h, c) = (1200, 800, [599.5, 399.5]);
        let (ar, ab) = (1.5e-3, -1.2e-3);
        let mut raw = raw_from(&mosaic_from_rgb(&discs(w, h, c, ar, ab), &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let before = raw.develop(Method::Ahd).unwrap();
        let (r0, b0) = (radial_shift(&before, 0, c, 16), radial_shift(&before, 2, c, 16));
        // it shows through AHD (measured 0.75 / −0.60 px; the mosaic's own sites: 0.79 / −0.64 px at these radii)
        assert!(r0 > 0.1 && b0 < -0.08, "uncorrected: red {r0}, blue {b0} px");
        raw.opcodes.lateral_ca = LateralCa::new(c, 400.0, vec![ar; 2], vec![ab; 2]);
        let after = raw.develop(Method::Ahd).unwrap();
        let (r1, b1) = (radial_shift(&after, 0, c, 16), radial_shift(&after, 2, c, 16));
        assert!(r1.abs() < 0.05 && b1.abs() < 0.05, "corrected: red {r1}, blue {b1} px (uncorrected {r0}, {b0})");
    }

    /// A table that varies with the radius (5 knots spanning ±MAX_SCALE) on another CFA phase: every interior red and
    /// blue site reads the field at its own position (checks the ρ² argument and the table's domain).
    #[test]
    fn radius_dependent_table_lines_up() {
        let (w, h, c, spacing) = (2000usize, 1400usize, [999.5, 699.5], 300.0);
        let red = vec![MAX_SCALE, 1e-3, 0.0, -1e-3, -MAX_SCALE];
        let blue = vec![-MAX_SCALE, -5e-4, 1e-3, MAX_SCALE, MAX_SCALE];
        // red's image of a point at green radius ρ sits at ρ·(1 + s(ρ)): invert ρ_x = ρ_P·(1 + s(ρ_P))
        let image = |k: &[f64], x: f64, y: f64| {
            let (dx, dy) = (x - c[0], y - c[1]);
            let rx = dx.hypot(dy);
            let mut rp = rx;
            // a contraction by about |ρ·s'| ≤ 10⁻²: ten steps converge far below f32
            for _ in 0..10 {
                rp = rx / (1.0 + interpolate(k, spacing, rp));
            }
            let f = if rx > 0.0 { rp / rx } else { 1.0 };
            field(c[0] + dx * f, c[1] + dy * f) as f32
        };
        let scene = Rgb32f::from_fn(w, h, |x, y| {
            let (x, y) = (x as f64, y as f64);
            [image(&red, x, y), field(x, y) as f32, image(&blue, x, y)]
        });
        let mut n = mosaic_from_rgb(&scene, &Cfa::bayer("GBRG").unwrap());
        LateralCa::new(c, spacing, red, blue).unwrap().correct_mosaic(&mut n);
        let cfa = n.cfa.clone().unwrap();
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                if cfa.color_at(x, y) != 1 {
                    assert!((n.data[y * w + x] as f64 - field(x as f64, y as f64)).abs() < 1e-3, "({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn binned_matches_corrected_then_binned() {
        let (w, h, c) = (1200, 800, [599.5, 399.5]);
        let mosaic = mosaic_from_rgb(&smooth(w, h, c, MAX_SCALE, -1.5e-3), &Cfa::bayer("RGGB").unwrap());
        // a constant map, and one that varies with the radius (the binned table's domain must match the mosaic's)
        let constant = LateralCa::new(c, 400.0, vec![MAX_SCALE; 2], vec![-1.5e-3; 2]).unwrap();
        let radial = LateralCa::new(c, 300.0, vec![MAX_SCALE, 1e-3, 0.0, -1e-3, -MAX_SCALE], vec![-MAX_SCALE, -5e-4, 1e-3, MAX_SCALE, MAX_SCALE]);
        for ca in [constant, radial.unwrap()] {
            let mut corrected = mosaic.clone();
            ca.correct_mosaic(&mut corrected);
            for k in [2, 4] {
                let (want, _) = raw_from(&corrected, 0.0, 4095.0).develop_binned_masked(k, 0.99).unwrap().unwrap();
                let (mut got, mask) = raw_from(&mosaic, 0.0, 4095.0).develop_binned_masked(k, 0.99).unwrap().unwrap();
                let plain = got.clone();
                ca.correct_binned(&mut got, &mask, k, Rect::new(0, 0, w, h));
                let (mut worst, mut moved) = (0f32, 0f32);
                for y in 3..got.height - 3 {
                    for x in 3..got.width - 3 {
                        for ch in 0..3 {
                            worst = worst.max((got.get(x, y)[ch] - want.get(x, y)[ch]).abs());
                            moved = moved.max((plain.get(x, y)[ch] - want.get(x, y)[ch]).abs());
                        }
                    }
                }
                assert!(worst < 2e-3 && moved > 5.0 * worst, "{} knots, k {k}: {worst} after, {moved} before", ca.red.len());
            }
        }
    }

    /// A default crop that doesn't start at the active area's corner (16:9 and DNG default crops): the binned image's
    /// origin is the crop's, the map's centre stays in active-area coordinates.
    #[test]
    fn binned_crop_origin_matches_corrected_then_binned() {
        let (w, h, crop) = (1200, 800, Rect::new(240, 160, 800, 520));
        let c = [crop.x as f64 + (crop.width as f64 - 1.0) / 2.0, crop.y as f64 + (crop.height as f64 - 1.0) / 2.0];
        let ca =
            LateralCa::new(c, 300.0, vec![MAX_SCALE, 1e-3, 0.0, -1e-3, -MAX_SCALE], vec![-MAX_SCALE, -5e-4, 1e-3, MAX_SCALE, MAX_SCALE]).unwrap();
        let mosaic = mosaic_from_rgb(&smooth(w, h, c, 0.0, 0.0), &Cfa::bayer("RGGB").unwrap());
        let mut corrected = mosaic.clone();
        ca.correct_mosaic(&mut corrected);
        for k in [2, 4] {
            let mut want = raw_from(&corrected, 0.0, 4095.0);
            want.crop = crop;
            let (want, _) = want.develop_binned_masked(k, 0.99).unwrap().unwrap();
            let mut raw = raw_from(&mosaic, 0.0, 4095.0);
            raw.crop = crop;
            let (plain, _) = raw.develop_binned_masked(k, 0.99).unwrap().unwrap();
            raw.opcodes.lateral_ca = Some(ca.clone());
            let (got, _) = raw.develop_binned_masked(k, 0.99).unwrap().unwrap();
            let (mut worst, mut moved) = (0f32, 0f32);
            for y in 3..got.height - 3 {
                for x in 3..got.width - 3 {
                    for ch in 0..3 {
                        worst = worst.max((got.get(x, y)[ch] - want.get(x, y)[ch]).abs());
                        moved = moved.max((plain.get(x, y)[ch] - want.get(x, y)[ch]).abs());
                    }
                }
            }
            assert!(worst < 2e-3 && moved > 5.0 * worst, "k {k}: {worst} after, {moved} before");
        }
    }

    #[test]
    fn clipped_binned_pixels_keep_their_value() {
        let mut img = Rgb32f::from_fn(40, 30, |x, y| [0.2 + 0.01 * x as f32, 0.3, 0.1 + 0.01 * y as f32]);
        let mut mask = vec![0u8; 40 * 30];
        mask[15 * 40 + 30] = highlight::CLIPPED_R;
        let before = img.clone();
        let ca = LateralCa::new([0.0, 0.0], 4.0, vec![MAX_SCALE; 2], vec![MAX_SCALE; 2]).unwrap();
        ca.correct_binned(&mut img, &mask, 2, Rect::new(0, 0, 80, 60));
        for (x, y) in [(30, 15), (31, 15), (30, 16), (29, 14)] {
            assert_eq!(img.get(x, y)[0], before.get(x, y)[0], "red at ({x}, {y}): its support holds the clipped pixel");
        }
        assert_ne!(img.get(10, 10)[0], before.get(10, 10)[0], "elsewhere red moved");
        assert_ne!(img.get(30, 15)[2], before.get(30, 15)[2], "blue isn't clipped there");
        assert!(img.data.iter().zip(&before.data).all(|(a, b)| a[1].to_bits() == b[1].to_bits()), "green untouched");
        // about 2.5 binned px of shift at x = 1250 (5 sensor px at a 2500 px radius): its own cell is outside its support
        let mut far = Rgb32f::from_fn(1500, 4, |x, _| [0.2 + 1e-4 * x as f32; 3]);
        let mut mask = vec![0u8; 1500 * 4];
        mask[1500 + 1250] = highlight::CLIPPED_R;
        let before = far.clone();
        ca.correct_binned(&mut far, &mask, 2, Rect::new(0, 0, 3000, 8));
        assert_eq!(far.get(1250, 1)[0], before.get(1250, 1)[0], "a far clipped pixel keeps its red");
        assert_ne!(far.get(1250, 1)[2], before.get(1250, 1)[2], "and its blue moves");
    }

    #[test]
    fn binned_no_op_cases() {
        let ca = LateralCa::new([20.0, 15.0], 4.0, vec![MAX_SCALE; 2], vec![MAX_SCALE; 2]).unwrap();
        let before = Rgb32f::from_fn(40, 30, |x, y| [0.01 * (x + y) as f32; 3]);
        for (what, mask, k) in [("k 0", vec![0u8; 1200], 0), ("short mask", vec![0u8; 1199], 2)] {
            let mut img = before.clone();
            ca.correct_binned(&mut img, &mask, k, Rect::new(0, 0, 80, 60));
            assert_eq!(img, before, "{what}");
        }
        let mut img = before.clone();
        LateralCa { blue: vec![f64::NAN; 2], ..ca.clone() }.correct_binned(&mut img, &[0u8; 1200], 2, Rect::new(0, 0, 80, 60));
        assert_eq!(img, before, "invalid map");
        let mut one = Rgb32f::new(1, 1);
        ca.correct_binned(&mut one, &[0], 2, Rect::new(0, 0, 2, 2)); // no panic
        let mut empty = Rgb32f::new(0, 3);
        ca.correct_binned(&mut empty, &[], 2, Rect::new(0, 0, 0, 6)); // no panic
        // X-Trans: the full-size development leaves it alone (`correct_mosaic`), so its binned previews do too
        let mut x = raw_from(&mosaic_from_rgb(&smooth(96, 72, [47.5, 35.5], 0.0, 0.0), &Cfa::xtrans()), 0.0, 4095.0);
        let plain = x.develop_binned_masked(3, 0.99).unwrap().unwrap();
        x.opcodes.lateral_ca = Some(ca);
        assert_eq!(x.develop_binned_masked(3, 0.99).unwrap().unwrap(), plain, "X-Trans");
    }

    /// No path corrects twice: with an `OpcodeList2` the binning reads the normalised mosaic (without the map) and
    /// corrects the binned image, exactly as without one.
    #[test]
    fn binning_corrects_once() {
        let (w, h, c) = (96, 64, [47.5, 31.5]);
        let mut raw = raw_from(&mosaic_from_rgb(&smooth(w, h, c, 0.0, 0.0), &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let (plain, _) = raw.develop_binned_masked(2, 0.99).unwrap().unwrap();
        raw.opcodes.lateral_ca = LateralCa::new(c, 8.0, vec![MAX_SCALE; 2], vec![-MAX_SCALE; 2]);
        let (a, _) = raw.develop_binned_masked(2, 0.99).unwrap().unwrap();
        assert_ne!(a, plain, "the map applies to binned images");
        let area = Area { top: 0, left: 0, bottom: h as u32, right: w as u32, plane: 0, planes: 1, row_pitch: 1, col_pitch: 1 };
        raw.opcodes.list2 = vec![Opcode::ScalePerRow { area, scales: vec![1.0; h] }];
        let (b, _) = raw.develop_binned_masked(2, 0.99).unwrap().unwrap();
        assert!(a.data.iter().zip(&b.data).all(|(p, q)| (0..3).all(|i| (p[i] - q[i]).abs() < 1e-6)));
    }

    /// Spec criterion 2 in miniature: the binned preview shows the full lens CA (binning borrows nothing) and is
    /// corrected like the full-size development.
    #[test]
    fn binned_previews_are_corrected_too() {
        let (w, h, c) = (1200, 800, [599.5, 399.5]);
        let (ar, ab) = (1.5e-3, -1.2e-3);
        let mut raw = raw_from(&mosaic_from_rgb(&discs(w, h, c, ar, ab), &Cfa::bayer("RGGB").unwrap()), 0.0, 4095.0);
        let k = 2;
        // binned pixel b sits at sensor position b·k + (k − 1)/2: the centre in binned px, shifts back in sensor px
        let cb = [(c[0] - 0.5) / 2.0, (c[1] - 0.5) / 2.0];
        let shift = |raw: &RawImage, ch| radial_shift(&raw.develop_binned_masked(k, 0.99).unwrap().unwrap().0, ch, cb, 8) * k as f64;
        let (r0, b0) = (shift(&raw, 0), shift(&raw, 2));
        assert!(r0 > 0.5 && b0 < -0.4, "uncorrected: red {r0}, blue {b0} px");
        raw.opcodes.lateral_ca = LateralCa::new(c, 400.0, vec![ar; 2], vec![ab; 2]);
        let (r1, b1) = (shift(&raw, 0), shift(&raw, 2));
        assert!(r1.abs() < 0.05 && b1.abs() < 0.05, "corrected: red {r1}, blue {b1} px");
        let full = raw.develop(Method::Ahd).unwrap();
        let (rf, bf) = (radial_shift(&full, 0, c, 16), radial_shift(&full, 2, c, 16));
        assert!((r1 - rf).abs() < 0.05 && (b1 - bf).abs() < 0.05, "binned {r1}, {b1} vs full {rf}, {bf}");
    }

    /// `cargo test --release -p lightcraft-raw --lib lateral_ca::tests::timing_24mp -- --ignored --nocapture`
    #[test]
    #[ignore = "timing, run by hand"]
    fn timing_24mp() {
        let (w, h) = (6000, 4000);
        let data = (0..w * h).map(|i| (i % 977) as f32 / 1000.0).collect();
        let mosaic = Normalized { width: w, height: h, cpp: 1, data, cfa: Some(Cfa::bayer("RGGB").unwrap()) };
        let ca = LateralCa::new([2999.5, 1999.5], 400.0, vec![5e-4, 3e-4, 1e-4], vec![-4e-4, -2e-4, 1e-4]).unwrap();
        let runs: Vec<_> = (0..5)
            .map(|_| {
                let mut n = mosaic.clone();
                let t = std::time::Instant::now();
                ca.correct_mosaic(&mut n);
                t.elapsed()
            })
            .collect();
        eprintln!("correct_mosaic on 24 MP: min {:?} of {runs:?}", runs.iter().min().unwrap());
        // the Preview path: 4× binning without and with the map, and the map alone on 24 MP and the ILCE-7RM6 binned 4×
        let mut raw = raw_from(&mosaic, 0.0, 4095.0);
        let bin = |raw: &RawImage| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    raw.develop_binned_masked(4, 0.99).unwrap().unwrap();
                    t.elapsed()
                })
                .min()
                .unwrap()
        };
        let plain = bin(&raw);
        raw.opcodes.lateral_ca = Some(ca.clone());
        eprintln!("develop_binned_masked(4) on 24 MP: min {plain:?} without the map, {:?} with", bin(&raw));
        for (w, h) in [(1500, 1000), (2496, 1664)] {
            let img = Rgb32f::from_fn(w, h, |x, y| [((x * 7 + y * 3) % 977) as f32 / 1000.0; 3]);
            let mask = vec![0u8; w * h];
            let ca = LateralCa { center: [(4 * w) as f64 / 2.0 - 0.5, (4 * h) as f64 / 2.0 - 0.5], ..ca.clone() };
            let runs: Vec<_> = (0..5)
                .map(|_| {
                    let mut img = img.clone();
                    let t = std::time::Instant::now();
                    ca.correct_binned(&mut img, &mask, 4, Rect::new(0, 0, 4 * w, 4 * h));
                    t.elapsed()
                })
                .collect();
            eprintln!("correct_binned on {w} × {h} (k 4): min {:?} of {runs:?}", runs.iter().min().unwrap());
        }
    }
}
