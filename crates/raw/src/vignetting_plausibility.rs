//! Whether a raw looks like a flat-field reference before Vignetting Calibration makes a map of it: an even surface
//! (a screen, a diffuser) seen through the lens shows nothing but a smooth fall-off from one bright area, in one
//! colour. A landscape, a portrait, a sky with a bird or a starfield shows detail, colours or a lopsided light.
//!
//! On a grid of 96 cells along the long side ([`super::vignetting_calibration::grid_for`]), from the per-cell trimmed
//! means of each colour:
//! - **detail:** log2 of a cell's green level minus the smoothed grid (3 × 3 median, then a Gaussian of σ 1 cell, the
//!   edges padded by odd reflection so a ramp stays a ramp). A flat field leaves noise: its largest cell is a few
//!   hundredths of an EV at most; anything in front of the lens (a branch, a bird, a star, a dust spot on a screen)
//!   stands out by a tenth or more. The 99th percentile catches detail spread over the frame.
//! - **colour:** the same for log2(R/G) and log2(B/G): a lens's colour shading is smooth, objects are not.
//! - **shape:** the smoothed grid's brightest cell near the frame's centre, the fall-off from it never rising again
//!   ring by ring, a fit to a radial fall-off ([`super::vignetting_calibration::fit_centre`]) that leaves little, and
//!   some fall-off at all.
//! - **exposure:** the level at the brightest cell and the share of clipped samples.
//!
//! Tuned (design notes §10, 2026-11-12) on 22 references and 2 astro flats against 553 ordinary raws (318
//! at random, 235 of fog, snow, dawn and dusk skies, the moon, deep sky, clouds and walls): every reference passes,
//! every photo fails, nearly all on detail. Between the flat fields' range and the limits a photo clearly isn't one
//! of, a reference is Borderline: Create asks first.

use rayon::prelude::*;
use serde::Serialize;

use crate::Normalized;
use crate::vignetting_calibration::{EvGrid, fit_centre, grid_for};

/// Cells along the long side.
const LONG_CELLS: usize = 96;
/// A sample at or above this (of white) is clipped.
const CLIP: f32 = 0.98;
/// Levels below this count as black (log of the cell means).
const FLOOR: f64 = 1e-5;
/// Rings around the brightest cell, for the fall-off's shape.
const RINGS: usize = 16;
/// A ring brighter than the one inside it by more than this (EV) rises.
const RISE_EV: f64 = 0.02;

/// A limit: above `borderline` Create asks first, above `fail` it refuses.
#[derive(Clone, Copy, Debug)]
pub struct Limit {
    pub borderline: f64,
    pub fail: f64,
}

/// The largest single-cell deviation from the smoothed field (EV). Flat fields: 0.003 to 0.035; photos: 0.16 and up.
pub const DETAIL_MAX: Limit = Limit { borderline: 0.05, fail: 0.12 };
/// The 99th percentile of that deviation (EV). Flat fields: up to 0.005.
pub const DETAIL_P99: Limit = Limit { borderline: 0.008, fail: 0.015 };
/// Colour structure: the robust RMS of log2(R/G), log2(B/G) less their smoothed grids (the larger). Flat fields: up to
/// 0.0025.
pub const COLOUR: Limit = Limit { borderline: 0.004, fail: 0.010 };
/// The brightest area's distance from the frame's centre, in half-diagonals. Flat fields: up to 0.06.
pub const PEAK_OFFSET: Limit = Limit { borderline: 0.10, fail: 0.25 };
/// Share of rings brighter than the ring inside them. Flat fields: none.
pub const RISING_RINGS: Limit = Limit { borderline: 0.0, fail: 0.10 };
/// RMS of the radial fit (EV). Flat fields: up to 0.05 (a lopsided but clean light passes with a note above 0.03).
pub const FIT_RMS: Limit = Limit { borderline: 0.08, fail: 0.15 };
/// Share of clipped samples. Flat fields: none.
pub const CLIPPED: Limit = Limit { borderline: 0.001, fail: 0.01 };
/// Above this RMS of the radial fit a passing reference gets the "unevenly lit" note.
pub const UNEVEN_NOTE_RMS: f64 = 0.03;
/// Below this level at the brightest cell (of white) it is too dark to tell.
pub const MIN_LEVEL: f64 = 0.02;
/// Less fall-off than this (EV, brightest cell to the corners) isn't a lens's.
pub const MIN_FALLOFF: f64 = 0.05;

/// The measurements ([`check`]); EV are log2 units.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub detail_max: f64,
    pub detail_p99: f64,
    /// Robust RMS of the detail (information: noise on a flat field, 0 on a black frame).
    pub detail_rms: f64,
    /// Median over cells of log2(p90 / p10) of the green samples (information: noise-dependent).
    pub cell_contrast: f64,
    pub colour: f64,
    pub peak_offset: f64,
    pub falloff: f64,
    pub rising_rings: f64,
    pub fit_rms: f64,
    pub level: f64,
    pub clipped: f64,
}

/// Why a raw doesn't look like a flat-field reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// Something in the frame: edges, texture, objects, stars.
    Detail,
    /// Colours that aren't even.
    Colour,
    /// The light doesn't fall off from one bright area in the middle.
    Shape,
    /// No fall-off toward the edges at all.
    NoFalloff,
    /// Too dark to measure.
    Dark,
    /// Clipped samples.
    Clipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    Pass,
    /// A clean field lit unevenly (a sky, a lopsided light): usable, with a note.
    PassWithNote,
    /// Outside the flat fields' range but not clearly a photo: Create asks first.
    Borderline,
    /// Clearly not a flat field: Create refuses.
    Fail,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plausibility {
    pub verdict: Verdict,
    /// For Borderline and Fail: why, most telling first.
    pub reasons: Vec<Reason>,
    pub metrics: Metrics,
}

impl Plausibility {
    /// Judge `m` by the limits above.
    pub fn judge(m: Metrics) -> Plausibility {
        let mut fail = Vec::new();
        let mut border = Vec::new();
        let mut add = |v: f64, l: Limit, r: Reason| {
            if !v.is_finite() || v > l.fail {
                fail.push(r);
            } else if v > l.borderline {
                border.push(r);
            }
        };
        add(m.detail_max, DETAIL_MAX, Reason::Detail);
        add(m.detail_p99, DETAIL_P99, Reason::Detail);
        add(m.colour, COLOUR, Reason::Colour);
        add(m.peak_offset, PEAK_OFFSET, Reason::Shape);
        add(m.rising_rings, RISING_RINGS, Reason::Shape);
        add(m.fit_rms, FIT_RMS, Reason::Shape);
        add(m.clipped, CLIPPED, Reason::Clipped);
        if m.level.is_nan() || m.level < MIN_LEVEL {
            fail.insert(0, Reason::Dark);
        }
        if m.falloff.is_nan() || m.falloff < MIN_FALLOFF {
            fail.push(Reason::NoFalloff);
        }
        let dedup = |v: Vec<Reason>| {
            v.into_iter().fold(Vec::new(), |mut out, r| {
                if !out.contains(&r) {
                    out.push(r);
                }
                out
            })
        };
        let (verdict, reasons) = if !fail.is_empty() {
            (Verdict::Fail, dedup(fail))
        } else if !border.is_empty() {
            (Verdict::Borderline, dedup(border))
        } else if m.fit_rms > UNEVEN_NOTE_RMS {
            (Verdict::PassWithNote, Vec::new())
        } else {
            (Verdict::Pass, Vec::new())
        };
        Plausibility { verdict, reasons, metrics: m }
    }
}

/// Measure and judge a decoded raw (a CFA mosaic, RGB, or one channel). `None` for an empty image.
pub fn check(n: &Normalized) -> Option<Plausibility> {
    metrics(n).map(Plausibility::judge)
}

/// The value of a grid at `(c, r)`, padded outside by odd reflection (a ramp continues as a ramp).
fn padded(v: &[f64], cols: usize, rows: usize, c: isize, r: isize) -> f64 {
    let reflect = |i: isize, n: isize| -> Option<(isize, isize)> {
        if i < 0 {
            Some((0, (-i).min(n - 1)))
        } else if i >= n {
            Some((n - 1, (2 * (n - 1) - i).max(0)))
        } else {
            None
        }
    };
    let (ci, ri) = (cols as isize, rows as isize);
    if let Some((e, m)) = reflect(c, ci) {
        return if cols < 2 { padded(v, cols, rows, 0, r) } else { 2.0 * padded(v, cols, rows, e, r) - padded(v, cols, rows, m, r) };
    }
    if let Some((e, m)) = reflect(r, ri) {
        return if rows < 2 { padded(v, cols, rows, c, 0) } else { 2.0 * padded(v, cols, rows, c, e) - padded(v, cols, rows, c, m) };
    }
    v.get(r as usize * cols + c as usize).copied().unwrap_or(0.0)
}

/// 3 × 3 median, then a separable Gaussian of σ 1 cell (7 taps), edges padded by odd reflection.
fn smooth(v: &[f64], cols: usize, rows: usize) -> Vec<f64> {
    let mut med = Vec::with_capacity(v.len());
    for r in 0..rows as isize {
        for c in 0..cols as isize {
            let mut k = [0.0f64; 9];
            let mut i = 0;
            for dr in -1..=1 {
                for dc in -1..=1 {
                    k[i] = padded(v, cols, rows, c + dc, r + dr);
                    i += 1;
                }
            }
            k.sort_by(f64::total_cmp);
            med.push(k[4]);
        }
    }
    let kernel: Vec<f64> = (-3i32..=3).map(|i| (-f64::from(i * i) / 2.0).exp()).collect();
    let sum: f64 = kernel.iter().sum();
    let pass = |src: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = Vec::with_capacity(src.len());
        for r in 0..rows as isize {
            for c in 0..cols as isize {
                let mut acc = 0.0;
                for (k, i) in kernel.iter().zip(-3isize..=3) {
                    let (cc, rr) = if horizontal { (c + i, r) } else { (c, r + i) };
                    acc += k / sum * padded(src, cols, rows, cc, rr);
                }
                out.push(acc);
            }
        }
        out
    };
    pass(&pass(&med, true), false)
}

/// The `q` quantile (nearest rank) of `v`, sorting it.
fn quantile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// 1.4826 × the median absolute deviation: the RMS of normal noise, deaf to a few outliers.
fn robust_rms(r: &[f64]) -> f64 {
    let mut a = r.to_vec();
    let med = quantile(&mut a, 0.5);
    let mut d: Vec<f64> = r.iter().map(|x| (x - med).abs()).collect();
    1.4826 * quantile(&mut d, 0.5)
}

/// Mean of the middle 60 % of `v` (sorting it).
fn trimmed_mean(v: &mut [f32]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f32::total_cmp);
    let k = v.len() / 5;
    let s = &v[k..v.len() - k];
    s.iter().map(|&x| f64::from(x)).sum::<f64>() / s.len().max(1) as f64
}

/// One cell: R, G, B trimmed means (NaN where the image has no such samples), the green samples' p10 and p90,
/// clipped and total samples.
struct Cell {
    mean: [f64; 3],
    p10: f64,
    p90: f64,
    clipped: usize,
    total: usize,
}

fn metrics(n: &Normalized) -> Option<Metrics> {
    let (w, h) = (n.width, n.height);
    if w == 0 || h == 0 || n.cpp == 0 || n.data.len() < w * h * n.cpp {
        return None;
    }
    let [gx, gy] = grid_for(w, h, LONG_CELLS);
    let span = |i: usize, k: usize, len: usize| (i * len / k, (i + 1) * len / k);
    let cells: Vec<Cell> = (0..gx * gy)
        .into_par_iter()
        .map(|k| {
            let (x0, x1) = span(k % gx, gx, w);
            let (y0, y1) = span(k / gx, gy, h);
            let mut ch: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            let (mut clipped, mut total) = (0, 0);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * n.cpp;
                    let samples: &[f32] = n.data.get(i..i + n.cpp).unwrap_or(&[]);
                    for (c, &v) in samples.iter().enumerate() {
                        total += 1;
                        clipped += usize::from(v >= CLIP);
                        let colour = match (n.cpp, n.cfa.as_ref()) {
                            (1, Some(cfa)) => match cfa.color_at(x, y) {
                                0 => 0,
                                2 => 2,
                                _ => 1,
                            },
                            (1, None) => 1,
                            _ => c.min(2),
                        };
                        ch[colour].push(v);
                    }
                }
            }
            let mut g: Vec<f64> = ch[1].iter().map(|&v| f64::from(v)).collect();
            let p10 = quantile(&mut g, 0.1);
            let p90 = quantile(&mut g, 0.9);
            let [r, gr, b] = &mut ch;
            Cell { mean: [trimmed_mean(r), trimmed_mean(gr), trimmed_mean(b)], p10, p90, clipped, total }
        })
        .collect();
    let lg: Vec<f64> = cells.iter().map(|c| c.mean[1].max(FLOOR).log2()).collect();
    let sg = smooth(&lg, gx, gy);
    let residual: Vec<f64> = lg.iter().zip(&sg).map(|(a, b)| a - b).collect();
    let detail_rms = robust_rms(&residual);
    let mut abs: Vec<f64> = residual.iter().map(|r| r.abs()).collect();
    let detail_p99 = quantile(&mut abs, 0.99);
    let detail_max = abs.last().copied().unwrap_or(f64::NAN);
    let mut contrast: Vec<f64> = cells.iter().map(|c| (c.p90.max(FLOOR) / c.p10.max(FLOOR)).log2()).collect();
    let cell_contrast = quantile(&mut contrast, 0.5);
    let mut colour = 0.0f64;
    for c in [0usize, 2] {
        if cells.iter().any(|x| !x.mean[c].is_finite()) {
            continue;
        }
        let ratio: Vec<f64> = cells.iter().map(|x| (x.mean[c].max(FLOOR) / x.mean[1].max(FLOOR)).log2()).collect();
        let s = smooth(&ratio, gx, gy);
        let d: Vec<f64> = ratio.iter().zip(&s).map(|(a, b)| a - b).collect();
        colour = colour.max(robust_rms(&d));
    }
    // the shape, on the smoothed green grid
    let (pk, peak) = sg.iter().copied().enumerate().fold((0, f64::MIN), |a, (i, v)| if v > a.1 { (i, v) } else { a });
    let centre_of = |i: usize, j: usize| (((i as f64 + 0.5) / gx as f64 - 0.5) * w as f64, ((j as f64 + 0.5) / gy as f64 - 0.5) * h as f64);
    let half_diag = 0.5 * (w as f64).hypot(h as f64);
    let (px, py) = centre_of(pk % gx, pk / gx);
    let peak_offset = px.hypot(py) / half_diag;
    let corners = [0, gx - 1, (gy - 1) * gx, gy * gx - 1];
    let falloff = peak - corners.iter().map(|&i| sg[i]).sum::<f64>() / 4.0;
    let mut rings = [(0.0f64, 0usize); RINGS];
    for j in 0..gy {
        for i in 0..gx {
            let (x, y) = centre_of(i, j);
            let k = (((x - px).hypot(y - py) / (2.0 * half_diag) * RINGS as f64) as usize).min(RINGS - 1);
            rings[k].0 += sg[j * gx + i];
            rings[k].1 += 1;
        }
    }
    let ring: Vec<f64> = rings.iter().filter(|r| r.1 > 0).map(|r| r.0 / r.1 as f64).collect();
    let rises = ring.windows(2).filter(|p| p[1] > p[0] + RISE_EV).count();
    let rising_rings = rises as f64 / ring.len().saturating_sub(1).max(1) as f64;
    let ev = EvGrid { sensor: [w, h], grid: [gx, gy], ev: sg.iter().map(|v| peak - v).collect() };
    let fit_rms = fit_centre(&ev).map_or(f64::NAN, |f| f.rms_ev);
    let level = cells[pk].mean[1];
    let (clipped, total) = cells.iter().fold((0usize, 0usize), |a, c| (a.0 + c.clipped, a.1 + c.total));
    Some(Metrics {
        detail_max,
        detail_p99,
        detail_rms,
        cell_contrast,
        colour,
        peak_offset,
        falloff,
        rising_rings,
        fit_rms,
        level,
        clipped: clipped as f64 / total.max(1) as f64,
    })
}

#[cfg(test)]
#[path = "vignetting_plausibility_tests.rs"]
mod tests;
