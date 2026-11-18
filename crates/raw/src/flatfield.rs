//! Flat-field correction: a reference raw shot through a diffuser (same lens, focal length and aperture as the
//! photos to correct) records the lens's vignetting and colour cast on its own. [`fit`] measures it into one smooth
//! DNG `GainMap` opcode per CFA colour site (DNG 1.3, `OpcodeList2`: applied to black-subtracted, white-scaled
//! data over the active area), normalised so the centre of the frame keeps its level; [`apply`] adds them to a
//! target raw of the same sensor layout, which `write_dng` then stores.
//!
//! The fit, per CFA phase (or plane): a lattice of points that includes the frame's edges, about one every
//! [`POINT_SPACING_PX`] pixels. Each point's level comes from the phase's samples within half a spacing of it
//! (clipped, non-finite and non-positive samples left out): a least-squares plane through their log levels, around
//! the point, fitted again without the [`OUTLIER_SHARE`] of samples farthest from the first fit (dust is dark, hot
//! pixels bright), and evaluated at the point itself. A window the frame cuts (at an edge or a corner) has its
//! samples on one side of the point; the plane carries the slope out to it, where a mean would stop short of the
//! fall-off. Points without usable samples are filled from their neighbours; then a 3 × 3 median and a Gaussian of
//! [`SMOOTHING`] points, both padded by odd reflection (`2·edge − inner`, which continues a slope, so the corners
//! keep theirs). Each gain is `centre / level`, clamped to [`MIN_GAIN`, `MAX_GAIN`]. The lattice lies on the
//! frame's edges (origin 0, spacing `1/(n − 1)`), so the decoder's bilinear interpolation never extrapolates. The
//! opcode layout follows the DNG specification (1.3+, chapter 6); filmcraft's `crates/dng/src/opcode.rs` was not
//! needed. Sources: own design (the window plane fit, odd-reflection padding; the cell statistics and smoothing as
//! the Develop LCC map's in `flat-field-render`).

use rayon::prelude::*;

use crate::opcodes::{Area, Opcode};
use crate::{Cfa, RawImage};

/// About this many pixels between the lattice points along the long side: on a 45 MP sensor about 97 points, each
/// from about 1800 samples of one Bayer site, fine enough to follow a fall-off that steepens toward the corners.
pub const POINT_SPACING_PX: f64 = 86.0;
/// Fewest and most lattice points along the long side; the short side gets as many as keep the spacing square (3 at
/// least).
pub const MIN_POINTS: usize = 17;
pub const MAX_POINTS: usize = 97;
/// The smallest reference measured, in pixels per side.
const MIN_SIDE: usize = 68;
/// The share of a window's samples left out of the second plane fit (those farthest from the first).
pub const OUTLIER_SHARE: f64 = 0.2;
/// Gaussian smoothing of the lattice, σ in points.
pub const SMOOTHING: f64 = 1.0;
/// Gains are clamped to this range: a corner darker than 1/16 of the centre (4 EV) is not a lens to correct.
pub const MIN_GAIN: f32 = 0.25;
pub const MAX_GAIN: f32 = 16.0;
/// Samples at or above this normalised level count as clipped.
const CLIP: f32 = 0.98;

/// Why a flat field can't be fitted or applied.
#[derive(Debug, Clone, PartialEq)]
pub enum FlatFieldError {
    /// The reference is too small, too dark, clipped, or not a mosaic/linear layout the map can describe.
    Reference(String),
    /// The target's sensor layout differs from the reference's.
    Mismatch(String),
}

impl std::fmt::Display for FlatFieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reference(m) | Self::Mismatch(m) => f.write_str(m),
        }
    }
}

/// The sensor layout a flat field is tied to: active-area size, samples per pixel and CFA (anchored at the active
/// area's corner).
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub cpp: usize,
    pub cfa: Option<Cfa>,
}

impl Layout {
    pub fn of(raw: &RawImage) -> Layout {
        let a = raw.active_area;
        Layout { width: a.width, height: a.height, cpp: raw.cpp, cfa: raw.cfa.as_ref().map(|c| c.shifted(a.x, a.y)) }
    }
}

/// A fitted flat field: the layout it was measured on and its `GainMap` opcodes.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatField {
    pub layout: Layout,
    pub ops: Vec<Opcode>,
}

/// Measure `reference` (a raw of an evenly lit diffuser) into a [`FlatField`].
pub fn fit(reference: &RawImage) -> Result<FlatField, FlatFieldError> {
    let layout = Layout::of(reference);
    let n = reference.normalized().map_err(|e| FlatFieldError::Reference(format!("the flat-field reference doesn't decode: {e}")))?;
    let (w, h, cpp) = (n.width, n.height, n.cpp);
    // (phase x, phase y, pitch x, pitch y, plane)
    let phases: Vec<(usize, usize, usize, usize, usize)> = match (&n.cfa, cpp) {
        (Some(c), 1) if c.width <= 2 && c.height <= 2 && c.width > 0 && c.height > 0 => {
            (0..c.height).flat_map(|y| (0..c.width).map(move |x| (x, y, c.width, c.height, 0))).collect()
        }
        (Some(_), _) => return Err(FlatFieldError::Reference("flat-field correction supports Bayer (2×2) mosaics, not this CFA layout".into())),
        (None, 1 | 3) => (0..cpp).map(|p| (0, 0, 1, 1, p)).collect(),
        (None, _) => return Err(FlatFieldError::Reference(format!("flat-field correction doesn't support {cpp} samples per pixel"))),
    };
    if w < MIN_SIDE || h < MIN_SIDE {
        return Err(FlatFieldError::Reference(format!("the flat-field reference is too small ({w}×{h})")));
    }
    let [ph, pv] = lattice(w, h);
    let mut ops = Vec::with_capacity(phases.len());
    for &(px, py, sx, sy, plane) in &phases {
        let site = Site { data: &n.data, w, h, cpp, px, py, sx, sy, plane };
        let mut levels = site.levels([ph, pv]);
        fill_holes(&mut levels, ph, pv)
            .ok_or_else(|| FlatFieldError::Reference("the flat-field reference has too few usable samples (black or clipped)".into()))?;
        let smooth = gaussian(&median3(&levels, ph, pv), ph, pv, SMOOTHING);
        // the frame centre's level (between lattice points when their count is even)
        let centre = bilinear(&smooth, ph, pv, 0.5, 0.5);
        if !(centre.is_finite() && centre > 1e-4f64.log2()) {
            return Err(FlatFieldError::Reference("the flat-field reference's centre is black".into()));
        }
        let gains = smooth.iter().map(|&l| if l.is_finite() { ((centre - l).exp2() as f32).clamp(MIN_GAIN, MAX_GAIN) } else { 1.0 }).collect();
        let as_u32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        ops.push(Opcode::GainMap {
            area: Area {
                top: as_u32(py),
                left: as_u32(px),
                bottom: as_u32(h),
                right: as_u32(w),
                plane: as_u32(plane),
                planes: 1,
                row_pitch: as_u32(sy),
                col_pitch: as_u32(sx),
            },
            points_v: as_u32(pv),
            points_h: as_u32(ph),
            spacing: [1.0 / (pv - 1) as f64, 1.0 / (ph - 1) as f64],
            origin: [0.0, 0.0],
            map_planes: 1,
            gains,
        });
    }
    Ok(FlatField { layout, ops })
}

/// Lattice points across and down for a `w × h` frame: about one every [`POINT_SPACING_PX`] along the long side
/// ([`MIN_POINTS`]..=[`MAX_POINTS`]), as many on the short side as keep the spacing square (at least 3).
fn lattice(w: usize, h: usize) -> [usize; 2] {
    let long = w.max(h) as f64;
    let n_long = ((long / POINT_SPACING_PX).round() as usize + 1).clamp(MIN_POINTS, MAX_POINTS);
    let n_short = ((w.min(h) as f64 / long * (n_long - 1) as f64).round() as usize + 1).max(3);
    if w >= h { [n_long, n_short] } else { [n_short, n_long] }
}

/// One CFA phase's (or plane's) samples of the normalised reference.
struct Site<'a> {
    data: &'a [f32],
    w: usize,
    h: usize,
    cpp: usize,
    px: usize,
    py: usize,
    sx: usize,
    sy: usize,
    plane: usize,
}

impl Site<'_> {
    /// Log2 level at every lattice point (`ph` across, `pv` down, row-major; NaN where no sample is usable), each from
    /// the samples in its window ([`fit_point`]). Lattice point `i` sits at `i / (n − 1)` of the frame, as the decoder
    /// places a GainMap's points (position `x / w`). An inner point's window reaches half a spacing each way; a point
    /// on the frame's edge has no samples outside, so its window reaches a whole spacing inward instead (as many samples
    /// as an inner point's, all on one side: the plane carries the slope out to the edge).
    fn levels(&self, [ph, pv]: [usize; 2]) -> Vec<f64> {
        let (w, h) = (self.w as f64, self.h as f64);
        let (gx, gy) = ((ph - 1) as f64, (pv - 1) as f64);
        (0..pv)
            .into_par_iter()
            .flat_map_iter(|j| {
                let mut windows: Vec<Vec<(f64, f64, f64, bool)>> = vec![Vec::new(); ph];
                let (py_px, spacing_y, spacing_x) = (j as f64 / gy * h, h / gy, w / gx);
                let rows = (0..self.h).filter(|y| *y >= self.py && (*y - self.py).is_multiple_of(self.sy));
                for (y, y_reach) in rows.filter_map(|y| windows_of(y as f64 / h * gy, pv).find(|(k, _)| *k == j).map(|(_, reach)| (y, reach))) {
                    let mut x = self.px;
                    while x < self.w {
                        if let Some(&v) = self.data.get((y * self.w + x) * self.cpp + self.plane)
                            && v.is_finite()
                            && v > 0.0
                            && v < CLIP
                        {
                            let dy = (y as f64 - py_px) / spacing_y;
                            for (i, x_reach) in windows_of(x as f64 / w * gx, ph) {
                                if let Some(win) = windows.get_mut(i) {
                                    win.push(((x as f64 - i as f64 / gx * w) / spacing_x, dy, f64::from(v).log2(), x_reach || y_reach));
                                }
                            }
                        }
                        x += self.sx;
                    }
                }
                windows.into_iter().map(|win| fit_point(&in_window(&win)).unwrap_or(f64::NAN)).collect::<Vec<_>>()
            })
            .collect()
    }
}

/// Fewest samples a window has before an edge point reaches farther in for more ([`in_window`]).
const MIN_WINDOW_SAMPLES: usize = 12;

/// The lattice points (along one axis of `n`) that may use a sample at lattice position `f` (`0..=n − 1`), each with
/// whether the sample is only in its reach: the nearest point (`false`), and the edge point too (`true`) while `f` is
/// within a spacing of the edge.
fn windows_of(f: f64, n: usize) -> impl Iterator<Item = (usize, bool)> {
    let last = n.saturating_sub(1);
    let nearest = (f.round().max(0.0) as usize).min(last);
    let first_edge = (nearest == 1 && f < 1.0).then_some((0, true));
    let last_edge = (last >= 1 && nearest + 1 == last && f > (last - 1) as f64).then_some((last, true));
    std::iter::once((nearest, false)).chain(first_edge).chain(last_edge)
}

/// A point's window: the samples within half a spacing of it, or, when that holds fewer than
/// [`MIN_WINDOW_SAMPLES`] (a point on the edge of a small frame), those within a spacing inward too.
fn in_window(win: &[(f64, f64, f64, bool)]) -> Vec<(f64, f64, f64)> {
    let near = win.iter().filter(|s| !s.3).count();
    win.iter().filter(|s| near < MIN_WINDOW_SAMPLES || !s.3).map(|s| (s.0, s.1, s.2)).collect()
}

/// The level at a lattice point from the `(dx, dy, log2 level)` samples around it (offsets in lattice spacings): a
/// least-squares plane, fitted again without the [`OUTLIER_SHARE`] of samples farthest from it, at (0, 0). With too
/// few samples for a plane, or a degenerate one (all on a line), their trimmed mean. `None` without samples.
fn fit_point(s: &[(f64, f64, f64)]) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let Some(first) = plane(s.iter()) else { return Some(trimmed_mean(s)) };
    let mut res: Vec<(f64, usize)> = s.iter().enumerate().map(|(k, &(x, y, z))| ((z - first.at(x, y)).abs(), k)).collect();
    let keep = s.len() - ((s.len() as f64 * OUTLIER_SHARE) as usize).min(s.len().saturating_sub(3));
    if keep < res.len() {
        res.select_nth_unstable_by(keep, |a, b| a.0.total_cmp(&b.0));
    }
    let kept = res.iter().take(keep).filter_map(|&(_, k)| s.get(k));
    Some(plane(kept).unwrap_or(first).at(0.0, 0.0))
}

/// `z = a + b·x + c·y`.
#[derive(Clone, Copy)]
struct Plane {
    a: f64,
    b: f64,
    c: f64,
}

impl Plane {
    fn at(&self, x: f64, y: f64) -> f64 {
        self.a + self.b * x + self.c * y
    }
}

/// Least-squares plane through `(x, y, z)` points (normal equations, Cramer's rule); `None` with fewer than 6 points
/// or when they don't span a plane.
fn plane<'a>(pts: impl Iterator<Item = &'a (f64, f64, f64)>) -> Option<Plane> {
    let (mut n, mut sx, mut sy, mut sz, mut sxx, mut syy, mut sxy, mut sxz, mut syz) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y, z) in pts {
        n += 1.0;
        sx += x;
        sy += y;
        sz += z;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
        sxz += x * z;
        syz += y * z;
    }
    if n < 6.0 {
        return None;
    }
    let det3 = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let m = [[n, sx, sy], [sx, sxx, sxy], [sy, sxy, syy]];
    let d = det3(m);
    // (relative to the spread of the offsets: samples along one line don't fix a plane)
    if !(d.is_finite() && d.abs() > 1e-9 * n * n * n) {
        return None;
    }
    let r = [sz, sxz, syz];
    let col = |k: usize| {
        let mut c = m;
        for (row, v) in c.iter_mut().zip(r) {
            row[k] = v;
        }
        det3(c) / d
    };
    let p = Plane { a: col(0), b: col(1), c: col(2) };
    (p.a.is_finite() && p.b.is_finite() && p.c.is_finite()).then_some(p)
}

/// Mean of the `z`s without the lowest and highest [`OUTLIER_SHARE`] / 2 each.
fn trimmed_mean(s: &[(f64, f64, f64)]) -> f64 {
    let mut z: Vec<f64> = s.iter().map(|p| p.2).collect();
    z.sort_unstable_by(f64::total_cmp);
    let k = ((z.len() as f64 * OUTLIER_SHARE / 2.0) as usize).min(z.len().saturating_sub(1) / 2);
    let kept = z.get(k..z.len() - k).unwrap_or(&z);
    kept.iter().sum::<f64>() / kept.len().max(1) as f64
}

/// Fill the NaN points of a `cols × rows` lattice from their valid neighbours, a ring at a time; `None` when no point
/// is valid.
fn fill_holes(v: &mut [f64], cols: usize, rows: usize) -> Option<()> {
    if !v.iter().any(|x| x.is_finite()) {
        return None;
    }
    while v.iter().any(|x| !x.is_finite()) {
        let before = v.to_vec();
        for r in 0..rows {
            for c in 0..cols {
                if before.get(r * cols + c).is_some_and(|x| x.is_finite()) {
                    continue;
                }
                let (mut sum, mut k) = (0.0, 0);
                for (dc, dr) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                    let (cc, rr) = (c as isize + dc, r as isize + dr);
                    if cc >= 0
                        && rr >= 0
                        && (cc as usize) < cols
                        && (rr as usize) < rows
                        && let Some(&x) = before.get(rr as usize * cols + cc as usize)
                        && x.is_finite()
                    {
                        sum += x;
                        k += 1;
                    }
                }
                if k > 0
                    && let Some(slot) = v.get_mut(r * cols + c)
                {
                    *slot = sum / k as f64;
                }
            }
        }
    }
    Some(())
}

/// Value of a `cols × rows` lattice at `(c, r)`, extended past its edges by odd reflection (`2·edge − mirror`), which
/// continues a linear ramp exactly. Each axis is reflected in turn, so corners continue a plane too.
fn padded(v: &[f64], cols: usize, rows: usize, c: isize, r: isize) -> f64 {
    let (ci, ri) = (cols as isize, rows as isize);
    // (edge index, mirrored index) for an index outside 0..n; a mirrored index still outside (a kernel wider than the
    // lattice) is reflected again by the recursion, each bounce n − 1 ≥ 1 nearer, so any depth stays linear
    let mirror = |i: isize, n: isize| -> (isize, isize) { if i < 0 { (0, -i) } else { (n - 1, 2 * (n - 1) - i) } };
    // (an axis of one point has no slope to continue: it stays as it is)
    if (c < 0 || c >= ci) && cols < 2 {
        return padded(v, cols, rows, 0, r);
    }
    if (r < 0 || r >= ri) && rows < 2 {
        return padded(v, cols, rows, c, 0);
    }
    if c < 0 || c >= ci {
        let (e, m) = mirror(c, ci);
        return 2.0 * padded(v, cols, rows, e, r) - padded(v, cols, rows, m, r);
    }
    if r < 0 || r >= ri {
        let (e, m) = mirror(r, ri);
        return 2.0 * padded(v, cols, rows, c, e) - padded(v, cols, rows, c, m);
    }
    v.get(r as usize * cols + c as usize).copied().unwrap_or(0.0)
}

/// 3 × 3 median, odd-reflection padded.
fn median3(v: &[f64], cols: usize, rows: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(v.len());
    for r in 0..rows as isize {
        for c in 0..cols as isize {
            let mut k = [0.0f64; 9];
            let mut i = 0;
            for dr in -1..=1 {
                for dc in -1..=1 {
                    if let Some(slot) = k.get_mut(i) {
                        *slot = padded(v, cols, rows, c + dc, r + dr);
                    }
                    i += 1;
                }
            }
            k.sort_unstable_by(f64::total_cmp);
            let [_, _, _, _, median, ..] = k;
            out.push(median);
        }
    }
    out
}

/// Separable Gaussian of σ `sigma` points, odd-reflection padded.
fn gaussian(v: &[f64], cols: usize, rows: usize, sigma: f64) -> Vec<f64> {
    if sigma.is_nan() || sigma <= 0.0 {
        return v.to_vec();
    }
    let radius = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f64> = (-radius..=radius).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f64 = kernel.iter().sum();
    let kernel: Vec<f64> = kernel.iter().map(|k| k / sum).collect();
    let pass = |src: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = Vec::with_capacity(src.len());
        for r in 0..rows as isize {
            for c in 0..cols as isize {
                let mut acc = 0.0;
                for (k, i) in kernel.iter().zip(-radius..=radius) {
                    let (cc, rr) = if horizontal { (c + i, r) } else { (c, r + i) };
                    acc += k * padded(src, cols, rows, cc, rr);
                }
                out.push(acc);
            }
        }
        out
    };
    let horizontal = pass(v, true);
    pass(&horizontal, false)
}

/// A `cols × rows` lattice (points at `i / (n − 1)`) at relative position `(u, v)`, bilinear.
fn bilinear(g: &[f64], cols: usize, rows: usize, u: f64, v: f64) -> f64 {
    let fx = u.clamp(0.0, 1.0) * (cols - 1) as f64;
    let fy = v.clamp(0.0, 1.0) * (rows - 1) as f64;
    let (x0, y0) = ((fx.floor() as usize).min(cols.saturating_sub(2)), (fy.floor() as usize).min(rows.saturating_sub(2)));
    let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
    let at = |x: usize, y: usize| g.get(y * cols + x).copied().unwrap_or(f64::NAN);
    let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
    let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
    top + (bottom - top) * ty
}

/// Add `ff`'s gain maps to `target`'s `OpcodeList2`, after checking it has the reference's sensor layout.
pub fn apply(ff: &FlatField, target: &mut RawImage) -> Result<(), FlatFieldError> {
    let t = Layout::of(target);
    let r = &ff.layout;
    if (t.width, t.height) != (r.width, r.height) {
        return Err(FlatFieldError::Mismatch(format!(
            "its sensor area is {}×{}, the flat-field reference's is {}×{}",
            t.width, t.height, r.width, r.height
        )));
    }
    if t.cpp != r.cpp || t.cfa != r.cfa {
        return Err(FlatFieldError::Mismatch("its colour filter layout differs from the flat-field reference's".into()));
    }
    target.opcodes.list2.extend(ff.ops.iter().cloned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlackLevel, OpcodeLists, RawData, RawFormat, Rect};

    /// A synthetic RGGB mosaic: `level(x, y, colour)` in [0, 1] over a 12-bit range with black 256.
    fn mosaic(w: usize, h: usize, level: impl Fn(usize, usize, u8) -> f64) -> RawImage {
        let cfa = Cfa::bayer("RGGB").unwrap();
        let data = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| (256.0 + level(x, y, cfa.color_at(x, y)) * 3839.0).round() as u16)
            .collect();
        let full = Rect::new(0, 0, w, h);
        RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16(data),
            cfa: Some(cfa.clone()),
            bits: 12,
            black: BlackLevel::uniform(256.0),
            white: vec![4095.0],
            active_area: full,
            crop: full,
            orientation: lcc_geom::Orientation::Normal,
            color: crate::ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
        }
    }

    /// Vignetting falling off to ~45 % in the corners, stronger in blue (a colour cast), with a little noise.
    fn vignette(w: usize, h: usize, x: usize, y: usize, c: u8) -> f64 {
        let (dx, dy) = (x as f64 / w as f64 - 0.5, y as f64 / h as f64 - 0.5);
        let r2 = (dx * dx + dy * dy) / 0.5;
        let k = [0.5, 0.55, 0.65][c as usize];
        1.0 - k * r2 + if (x * 7 + y * 13).is_multiple_of(5) { 0.004 } else { 0.0 }
    }

    #[test]
    fn written_gain_map_flattens_a_vignetted_frame_after_decode() {
        let (w, h) = (256, 192);
        let reference = mosaic(w, h, |x, y, c| 0.6 * vignette(w, h, x, y, c));
        let ff = fit(&reference).unwrap();
        assert_eq!(ff.ops.len(), 4, "one map per Bayer site");
        // a target of an even scene at a different exposure
        let mut target = mosaic(w, h, |x, y, c| 0.3 * vignette(w, h, x, y, c));
        let before = target.normalized().unwrap();
        apply(&ff, &mut target).unwrap();
        let dng = crate::write_dng(&target, &Default::default()).unwrap();
        let back = crate::decode(&dng).unwrap();
        let n = back.normalized().unwrap();
        let cfa = n.cfa.clone().unwrap();
        let spread = |d: &[f32]| {
            let mut worst = 0f32;
            for c in 0..3u8 {
                let v: Vec<f32> =
                    (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| cfa.color_at(x, y) == c).map(|(x, y)| d[y * w + x]).collect();
                let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
                worst = worst.max((hi - lo) / hi);
            }
            worst
        };
        assert!(spread(&before.data) > 0.3, "the frame was vignetted");
        assert!(spread(&n.data) < 0.06, "corrected to flat within tolerance: {}", spread(&n.data));
        // the centre keeps its level
        let c = n.data[(h / 2) * w + w / 2];
        assert!((c - before.data[(h / 2) * w + w / 2]).abs() < 0.01);
    }

    /// A wide lens wide open: about 3 EV darker in the corners than in the middle, steepening toward them (r², r⁴, r⁶
    /// terms, the shape of a Nikon Z 24 mm f/1.8 reference), blue 4 % stronger than green, red as green.
    fn steep_ev(w: usize, h: usize, x: usize, y: usize, c: u8) -> f64 {
        let (dx, dy) = ((x as f64 + 0.5) / w as f64 - 0.5, (y as f64 + 0.5) / h as f64 - 0.5);
        // 0 in the middle, 1 in the corners
        let d2 = (dx * dx + dy * dy) / 0.5f64.hypot(0.5).powi(2);
        let ev = 0.55 * d2 + 1.2 * d2 * d2 + 1.25 * d2 * d2 * d2;
        ev * if c == 2 { 1.04 } else { 1.0 }
    }

    /// Mean level of colour `c` in the `s × s` box at (x0, y0).
    fn box_mean(n: &crate::Normalized, c: u8, x0: usize, y0: usize, s: usize) -> f64 {
        let cfa = n.cfa.clone().unwrap();
        let v: Vec<f64> = (y0..y0 + s)
            .flat_map(|y| (x0..x0 + s).map(move |x| (x, y)))
            .filter(|&(x, y)| cfa.color_at(x, y) == c)
            .map(|(x, y)| f64::from(n.data[y * n.width + x]))
            .collect();
        v.iter().sum::<f64>() / v.len() as f64
    }

    /// A steep fall-off is followed to the corners and edges: corrected, every colour is within 0.03 EV of the middle
    /// there. The frame is large enough for a lattice as dense, relative to the fall-off, as a real raw's (43 points
    /// along the long side here, 97 on a 45 MP raw). A degree-4 surface through 17 × 17 points missed by more.
    #[test]
    fn a_steep_fall_off_is_followed_to_the_corners() {
        let (w, h) = (3600, 2400);
        assert_eq!(lattice(w, h), [43, 29]);
        let reference = mosaic(w, h, |x, y, c| 0.6 * (-steep_ev(w, h, x, y, c)).exp2());
        let ff = fit(&reference).unwrap();
        let mut target = mosaic(w, h, |x, y, c| 0.5 * (-steep_ev(w, h, x, y, c)).exp2());
        apply(&ff, &mut target).unwrap();
        let n = crate::decode(&crate::write_dng(&target, &Default::default()).unwrap()).unwrap().normalized().unwrap();
        let s = 8;
        let spots = [
            ("top-left", 0, 0),
            ("top-right", w - s, 0),
            ("bottom-left", 0, h - s),
            ("bottom-right", w - s, h - s),
            ("left edge", 0, h / 2 - s / 2),
            ("right edge", w - s, h / 2 - s / 2),
            ("top edge", w / 2 - s / 2, 0),
            ("bottom edge", w / 2 - s / 2, h - s),
        ];
        let mut worst = (0.0f64, "", 0u8);
        for c in 0..3u8 {
            let middle = box_mean(&n, c, w / 2 - s / 2, h / 2 - s / 2, s);
            for (name, x, y) in spots {
                let left = (middle / box_mean(&n, c, x, y, s)).log2();
                if left.abs() > worst.0.abs() {
                    worst = (left, name, c);
                }
            }
        }
        assert!(worst.0.abs() < 0.03, "{:+.3} EV left at the {} (colour {})", worst.0, worst.1, worst.2);
    }

    /// The 3 × 3 median and the Gaussian pad by odd reflection, which continues a slope: a linear ramp (a plane, in
    /// log levels) comes through both unchanged, corners and edges included. Padding by repeating the edge would flatten
    /// the corners.
    #[test]
    fn smoothing_keeps_a_linear_ramp() {
        let (cols, rows) = (13, 9);
        let ramp: Vec<f64> = (0..rows).flat_map(|r| (0..cols).map(move |c| 1.0 + 0.3 * c as f64 - 0.2 * r as f64)).collect();
        let smooth = gaussian(&median3(&ramp, cols, rows), cols, rows, SMOOTHING);
        for (a, b) in ramp.iter().zip(&smooth) {
            assert!((a - b).abs() < 1e-9, "{a} became {b}");
        }
        // (a wider kernel than the lattice still continues it)
        let small: Vec<f64> = (0..3).flat_map(|r| (0..3).map(move |c| 1.0 + 0.3 * c as f64 - 0.2 * r as f64)).collect();
        let wide = gaussian(&small, 3, 3, 4.0);
        assert!(wide.iter().zip(&small).all(|(a, b)| (a - b).abs() < 1e-9), "{wide:?}");
    }

    #[test]
    fn mismatched_layouts_are_refused() {
        let reference = mosaic(128, 96, |_, _, _| 0.5);
        let ff = fit(&reference).unwrap();
        let mut smaller = mosaic(64, 96, |_, _, _| 0.5);
        assert!(matches!(apply(&ff, &mut smaller), Err(FlatFieldError::Mismatch(_))));
        let mut other_cfa = mosaic(128, 96, |_, _, _| 0.5);
        other_cfa.cfa = Cfa::bayer("BGGR");
        assert!(matches!(apply(&ff, &mut other_cfa), Err(FlatFieldError::Mismatch(_))));
        assert!(other_cfa.opcodes.list2.is_empty(), "nothing added on refusal");
    }

    #[test]
    fn hostile_references_are_errors_not_panics() {
        assert!(fit(&mosaic(128, 96, |_, _, _| 0.0)).is_err(), "black");
        assert!(fit(&mosaic(128, 96, |_, _, _| 1.0)).is_err(), "clipped");
        assert!(fit(&mosaic(8, 8, |_, _, _| 0.5)).is_err(), "tiny");
        let mut xt = mosaic(128, 96, |_, _, _| 0.5);
        xt.cfa = Some(Cfa::xtrans());
        assert!(fit(&xt).is_err(), "X-Trans");
        // holes (a clipped half) are filled, gains stay finite and clamped
        let half = fit(&mosaic(128, 96, |x, _, _| if x < 64 { 1.0 } else { 0.5 })).unwrap();
        for op in &half.ops {
            let Opcode::GainMap { gains, .. } = op else { panic!() };
            assert!(gains.iter().all(|g| g.is_finite() && (MIN_GAIN..=MAX_GAIN).contains(g)));
        }
    }
}
