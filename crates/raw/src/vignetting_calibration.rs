//! Flat-field maps (Capture One's LCC "Uniform Light"): a lens's light fall-off measured from a reference raw shot
//! through a diffuser, as a small grid of luminance gains in sensor coordinates.
//!
//! - [`build`] measures a reference: the green samples of each grid cell of [`RawImage::normalized`]'s data
//!   (trimmed mean, so dust and hot pixels don't count), smoothed in log space, normalised so the brightest region is
//!   1.0, stored as the gain that brings each cell up to it ([`VignettingMeasurement`]).
//! - [`VignettingMeasurement::gain_at`] samples it (bilinear in log space) at an [`amount`](effective_gain): the gain to the
//!   power `amount`, so 0.5 removes exactly half the fall-off in EV everywhere.
//! - [`SensorFrame`] maps the coordinates the pipeline's geometry stage works in (the user-oriented, EXIF-oriented,
//!   default-cropped source at any size) to sensor coordinates and back.
//! - [`check`] applies a map to the cells of a second reference of the same setup and reports what fall-off is left:
//!   the test of a map.
//! - [`fit_centre`] finds the optical centre of a fall-off.
//!
//! The same gain multiplies every colour channel: this corrects light fall-off, not colour cast. The map is
//! measured on linear data and applied on linear data, so amount 1.0 ([`DEFAULT_VIGNETTING_CALIBRATION_AMOUNT`]) is the
//! physically correct full correction.
//!
//! **Sensor coordinates** are pixels of the active area, as [`RawImage::normalized`] returns it: unrotated, origin at
//! its top-left corner, pixel centres at `x + 0.5`. A map is valid only for an identical active area (its
//! [`VignettingMeasurement::sensor`]); a photo whose active area differs must not use it.
//!
//! Sources: own design (cell statistics, odd-reflection padding, log-space interpolation); the flat-field principle
//! as Capture One describes LCC in its user documentation. No other application's code was read.

use lcc_geom::{Orientation, Point};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::gaintable::SourcePlacement;
use crate::{Normalized, RawImage, Rect, SensorWindow, stretched_height};

/// Version of the [`VignettingMeasurement`] record.
pub const FORMAT_VERSION: u32 = 1;
/// The amount a flat field applies by default: the full correction. The map is measured on linear data and applied on
/// linear data, so 1.0 is the physically correct correction (not a matter of taste).
pub const DEFAULT_VIGNETTING_CALIBRATION_AMOUNT: f64 = 1.0;
/// The largest amount (Capture One's slider goes to 120); larger amounts are clamped to it.
pub const MAX_VIGNETTING_CALIBRATION_AMOUNT: f64 = 1.2;
/// Fewest and most cells along one side of the grid.
pub const MIN_GRID: usize = 2;
pub const MAX_GRID: usize = 256;
/// Largest gain a map may hold (6 EV).
pub const MAX_GAIN: f32 = 64.0;
/// Fewest sensor pixels along one side of a cell.
const MIN_CELL_PX: usize = 4;

/// Where a reference came from: the shot's camera, lens and exposure, and the file's SHA-1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VignettingCalibrationSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// Millimetres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_length: Option<f64>,
    /// F-number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aperture: Option<f64>,
    /// SHA-1 of the reference file (lower-case hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
}

impl VignettingCalibrationSource {
    /// From a shot's metadata; the lens is its model name, else its specification (`24-70mm f/4`).
    pub fn from_metadata(m: &lcc_meta::Metadata, sha1: Option<String>) -> VignettingCalibrationSource {
        let text = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let lens = text(&m.lens_model).or_else(|| m.lens_spec.map(spec_name));
        let positive = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
        VignettingCalibrationSource {
            make: text(&m.make),
            model: text(&m.model),
            serial: text(&m.serial_number),
            lens,
            focal_length: positive(m.focal_length),
            aperture: positive(m.f_number),
            sha1,
        }
    }
}

fn spec_name([f0, f1, a0, a1]: [f64; 4]) -> String {
    let focal = if f0 == f1 { format!("{f0}mm") } else { format!("{f0}-{f1}mm") };
    let aperture = if a0 == a1 { format!("f/{a0}") } else { format!("f/{a0}-{a1}") };
    format!("{focal} {aperture}")
}

/// A measured flat field: `grid[0] × grid[1]` cells over the sensor's active area, row-major, each the gain (≥ 1)
/// that brings the cell up to the brightest one.
///
/// The map stores the full measured correction and is never pre-scaled: the amount is applied when it is sampled
/// ([`Self::gain_at`]). It is valid only for a photo whose active area is exactly [`Self::sensor`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VignettingMeasurement {
    pub version: u32,
    pub source: VignettingCalibrationSource,
    /// Width and height of the active area the grid spans, in sensor pixels.
    pub sensor: [usize; 2],
    /// Cells across and down.
    pub grid: [usize; 2],
    /// `grid[0] · grid[1]` gains, row-major.
    pub gains: Vec<f32>,
    /// Where the reference's active area sits on the camera's full sensor, when the file says
    /// ([`crate::sensor_window`]). It lets the map serve a smaller image area of the same sensor ([`Self::placement`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<SensorWindow>,
}

/// How a reference is measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VignettingCalibrationOptions {
    /// Cells along the sensor's long side (the short side gets as many as keep cells about square). The default, 96,
    /// resolves the sharp peak of a fall-off at the optical centre: on a Z 7 with an 85 mm f/1.8, a reference corrected
    /// with its own map leaves about 0.006 EV between the centre and the corners at 96 cells, 0.02 EV at 48.
    pub long_cells: usize,
    /// Gaussian smoothing of the grid, σ in cells (0 = none; a 3 × 3 median always runs).
    pub smoothing: f64,
    /// Fraction of each cell's samples dropped at each end before averaging (dust is dark, hot pixels are bright).
    pub trim: f64,
    /// Normalised level at or above which a sample counts as clipped.
    pub clip: f32,
    /// Most clipped samples a cell may have (fraction); a reference with more anywhere is refused.
    pub max_clipped: f64,
}

impl Default for VignettingCalibrationOptions {
    fn default() -> Self {
        VignettingCalibrationOptions { long_cells: 96, smoothing: 1.0, trim: 0.2, clip: 0.98, max_clipped: 0.01 }
    }
}

impl VignettingCalibrationOptions {
    fn validate(&self) -> Result<(), VignettingCalibrationError> {
        let bad = |why: &str| Err(VignettingCalibrationError::BadOptions(why.to_string()));
        if !(MIN_GRID..=MAX_GRID).contains(&self.long_cells) {
            return bad("the grid needs 2 to 256 cells along the long side");
        }
        if !(self.smoothing.is_finite() && (0.0..=16.0).contains(&self.smoothing)) {
            return bad("smoothing must be 0 to 16 cells");
        }
        if !(self.trim.is_finite() && (0.0..0.5).contains(&self.trim)) {
            return bad("trim must be at least 0 and below 0.5");
        }
        if !(self.clip.is_finite() && self.clip > 0.0) {
            return bad("the clip level must be above 0");
        }
        if !(self.max_clipped.is_finite() && (0.0..=1.0).contains(&self.max_clipped)) {
            return bad("the clipped fraction must be 0 to 1");
        }
        Ok(())
    }
}

/// Why a reference can't be measured or a map can't be used.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum VignettingCalibrationError {
    #[error("the reference has no green samples to measure")]
    NotCfa,
    #[error("the reference ({width} × {height} px) is too small for a {cols} × {rows} grid")]
    TooSmall { width: usize, height: usize, cols: usize, rows: usize },
    #[error("the reference is too dark (level {level:.4} of white at cell ({col}, {row}))")]
    TooDark { col: usize, row: usize, level: f64 },
    #[error("the reference is clipped: cell ({col}, {row}) has {percent:.1} % of its green samples at or above the clip level")]
    Clipped { col: usize, row: usize, percent: f64 },
    #[error("invalid options: {0}")]
    BadOptions(String),
    #[error("invalid flat-field map: {0}")]
    Invalid(String),
    /// A valid map and a frame it can't be applied to or compared with (another sensor, an unknown image area, too
    /// little overlap).
    #[error("{0}")]
    Mismatch(String),
}

/// The grid for a `width × height` active area: `long_cells` along the long side, the short side in proportion.
pub fn grid_for(width: usize, height: usize, long_cells: usize) -> [usize; 2] {
    let long_cells = long_cells.clamp(MIN_GRID, MAX_GRID);
    let (long, short) = if width >= height { (width, height) } else { (height, width) };
    let ratio = if long == 0 { 1.0 } else { short as f64 / long as f64 };
    let short_cells = ((long_cells as f64 * ratio).round() as usize).clamp(MIN_GRID, MAX_GRID);
    if width >= height { [long_cells, short_cells] } else { [short_cells, long_cells] }
}

/// Per-cell trimmed means of a reference's green samples, over a grid ([`cell_means`]).
#[derive(Clone, Debug, PartialEq)]
pub struct CellGrid {
    pub sensor: [usize; 2],
    pub grid: [usize; 2],
    /// Normalised levels (1.0 = white), row-major.
    pub means: Vec<f64>,
}

/// Bounds of cell `i` of `n` over `len` pixels: `[i·len/n, (i+1)·len/n)`.
fn cell_span(i: usize, n: usize, len: usize) -> (usize, usize) {
    let at = |k: usize| ((k as u128 * len as u128) / n.max(1) as u128) as usize;
    (at(i), at(i.saturating_add(1)))
}

/// The centre of cell `(col, row)` in sensor pixels.
pub fn cell_centre(sensor: [usize; 2], grid: [usize; 2], col: usize, row: usize) -> Point {
    let [w, h] = sensor;
    let [cols, rows] = grid;
    Point::new((col as f64 + 0.5) * w as f64 / cols.max(1) as f64, (row as f64 + 0.5) * h as f64 / rows.max(1) as f64)
}

/// Which samples of a [`Normalized`] buffer are measured.
enum Plane<'a> {
    /// CFA data: the green sites.
    Green(&'a crate::Cfa),
    /// Linear RGB (cpp 3): the green channel.
    Rgb,
    /// Monochrome: every sample.
    Mono,
}

/// Measure every cell of `grid` over `n`: the trimmed mean of its green samples.
///
/// `n` is [`RawImage::normalized`]'s data: black subtracted, white = 1.0, the active area, and with the file's
/// `OpcodeList2` already applied. A DNG `GainMap` there (a camera's or converter's own flat field) is therefore
/// already removed, and a map built from it measures the fall-off that remains. That is consistent as long as the
/// photos it is applied to also go through [`RawImage::normalized`].
///
/// A cell with more than `opts.max_clipped` of its green samples at or above `opts.clip` refuses the whole reference
/// ([`VignettingCalibrationError::Clipped`], the first such cell in reading order): a flat-field reference must be unclipped, and
/// a clipped cell is never filled in from its neighbours.
pub fn cell_means(n: &Normalized, grid: [usize; 2], opts: &VignettingCalibrationOptions) -> Result<CellGrid, VignettingCalibrationError> {
    opts.validate()?;
    let (w, h) = (n.width, n.height);
    let [cols, rows] = grid;
    if !(MIN_GRID..=MAX_GRID).contains(&cols) || !(MIN_GRID..=MAX_GRID).contains(&rows) {
        return Err(VignettingCalibrationError::BadOptions("the grid needs 2 to 256 cells per side".into()));
    }
    if w < cols.saturating_mul(MIN_CELL_PX) || h < rows.saturating_mul(MIN_CELL_PX) {
        return Err(VignettingCalibrationError::TooSmall { width: w, height: h, cols, rows });
    }
    let expected = w.checked_mul(h).and_then(|p| p.checked_mul(n.cpp));
    if expected != Some(n.data.len()) {
        return Err(VignettingCalibrationError::Invalid("the sample buffer doesn't match its size".into()));
    }
    let plane = match (n.cpp, n.cfa.as_ref()) {
        (1, Some(cfa)) if cfa.pattern.contains(&1) => Plane::Green(cfa),
        (1, None) => Plane::Mono,
        (3, _) => Plane::Rgb,
        _ => return Err(VignettingCalibrationError::NotCfa),
    };
    let measured: Vec<Result<Vec<f64>, VignettingCalibrationError>> = (0..rows)
        .into_par_iter()
        .map(|row| {
            let (y0, y1) = cell_span(row, rows, h);
            let mut buf: Vec<f32> = Vec::new();
            let mut out = Vec::with_capacity(cols);
            for col in 0..cols {
                let (x0, x1) = cell_span(col, cols, w);
                buf.clear();
                let (mut total, mut clipped) = (0usize, 0usize);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (take, s) = match plane {
                            Plane::Green(cfa) => (cfa.color_at(x, y) == 1, 0),
                            Plane::Rgb => (true, 1),
                            Plane::Mono => (true, 0),
                        };
                        if !take {
                            continue;
                        }
                        let Some(&v) = n.data.get((y * w + x) * n.cpp + s) else { continue };
                        total += 1;
                        if !v.is_finite() {
                            continue;
                        }
                        if v >= opts.clip {
                            clipped += 1;
                        }
                        buf.push(v);
                    }
                }
                if total == 0 || buf.is_empty() {
                    return Err(VignettingCalibrationError::NotCfa);
                }
                let fraction = clipped as f64 / total as f64;
                if fraction > opts.max_clipped {
                    return Err(VignettingCalibrationError::Clipped { col, row, percent: fraction * 100.0 });
                }
                out.push(trimmed_mean(&mut buf, opts.trim));
            }
            Ok(out)
        })
        .collect();
    let mut means = Vec::with_capacity(cols * rows);
    // (rows in order, so the first clipped cell in reading order is the one reported)
    for row in measured {
        means.extend(row?);
    }
    Ok(CellGrid { sensor: [w, h], grid, means })
}

/// The mean of `v` without its lowest and highest `trim` fraction.
fn trimmed_mean(v: &mut [f32], trim: f64) -> f64 {
    v.sort_unstable_by(f32::total_cmp);
    let n = v.len();
    let k = ((n as f64 * trim).floor() as usize).min(n.saturating_sub(1) / 2);
    let kept = v.get(k..n - k).unwrap_or(v);
    if kept.is_empty() {
        return 0.0;
    }
    kept.iter().map(|&x| f64::from(x)).sum::<f64>() / kept.len() as f64
}

/// Value of a `cols × rows` grid at `(c, r)`, extended past its edges by odd reflection (`2·edge − mirror`), which
/// continues a linear ramp exactly. Each axis is reflected in turn, so corners continue a plane too.
fn padded(v: &[f64], cols: usize, rows: usize, c: isize, r: isize) -> f64 {
    let (ci, ri) = (cols as isize, rows as isize);
    let mirror = |i: isize, n: isize| -> (isize, isize) {
        // (edge index, mirrored index) for an index outside 0..n; a mirrored index still outside (a kernel wider than
        // the grid) is reflected again by the recursion, each bounce n − 1 ≥ 1 nearer, so any depth stays linear
        if i < 0 { (0, -i) } else { (n - 1, 2 * (n - 1) - i) }
    };
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

/// Separable Gaussian of σ `sigma` cells, odd-reflection padded.
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
    let h = pass(v, true);
    pass(&h, false)
}

/// A grid of cell levels in log2 (EV), 3 × 3 median and Gaussian smoothed as [`build`] does. Higher = brighter.
pub fn smooth_ev(cells: &CellGrid, opts: &VignettingCalibrationOptions) -> Result<Vec<f64>, VignettingCalibrationError> {
    let [cols, rows] = cells.grid;
    let mut ev = Vec::with_capacity(cells.means.len());
    for (i, &m) in cells.means.iter().enumerate() {
        if !(m.is_finite() && m > 1e-6) {
            return Err(VignettingCalibrationError::TooDark { col: i % cols.max(1), row: i / cols.max(1), level: m });
        }
        ev.push(m.log2());
    }
    let ev = median3(&ev, cols, rows);
    Ok(gaussian(&ev, cols, rows, opts.smoothing))
}

/// Measure a flat-field reference: `n` is the reference's [`RawImage::normalized`] data, with no vignetting or lens
/// correction applied (`OpcodeList3` isn't, it comes after demosaic; see [`cell_means`] for `OpcodeList2`).
///
/// Cells: trimmed means of the green samples; then log2, a 3 × 3 median (a cell spoiled by a large dust spot) and a
/// Gaussian; normalised so the brightest cell is 1.0, stored as `gain = 1 / level`.
pub fn build(
    n: &Normalized,
    source: VignettingCalibrationSource,
    opts: &VignettingCalibrationOptions,
) -> Result<VignettingMeasurement, VignettingCalibrationError> {
    opts.validate()?;
    let grid = grid_for(n.width, n.height, opts.long_cells);
    let cells = cell_means(n, grid, opts)?;
    let [cols, rows] = grid;
    let (cc, cr) = (cols / 2, rows / 2);
    let centre = cells.means.get(cr * cols + cc).copied().unwrap_or(0.0);
    if centre.is_nan() || centre < 0.02 {
        return Err(VignettingCalibrationError::TooDark { col: cc, row: cr, level: centre });
    }
    let ev = smooth_ev(&cells, opts)?;
    let top = ev.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let gains = ev.iter().map(|e| ((top - e).exp2() as f32).clamp(1.0, MAX_GAIN)).collect();
    let map = VignettingMeasurement { version: FORMAT_VERSION, source, sensor: [n.width, n.height], grid, gains, window: None };
    map.validate()?;
    Ok(map)
}

/// The amount in its range: 0 to [`MAX_VIGNETTING_CALIBRATION_AMOUNT`], not-a-number = 0 (no correction).
pub fn clamp_amount(amount: f64) -> f64 {
    if amount.is_nan() { 0.0 } else { amount.clamp(0.0, MAX_VIGNETTING_CALIBRATION_AMOUNT) }
}

/// The gain a flat-field gain `g` applies at `amount`: `g^amount`, so the amount scales the correction in EV
/// (0.5 = half the fall-off removed, in stops, at every point; 1.0 = all of it).
pub fn effective_gain(g: f64, amount: f64) -> f64 {
    g.powf(clamp_amount(amount))
}

/// A grid of EV values over a sensor area, sampled like a [`VignettingMeasurement`] ([`EvGrid::at`]).
#[derive(Clone, Debug, PartialEq)]
pub struct EvGrid {
    pub sensor: [usize; 2],
    pub grid: [usize; 2],
    /// Row-major.
    pub ev: Vec<f64>,
}

impl EvGrid {
    /// The value at sensor position `(x, y)` ([`sample_grid`]).
    pub fn at(&self, x: f64, y: f64) -> f64 {
        let cols = self.grid[0];
        sample_grid(self.sensor, self.grid, &|c, r| self.ev.get(r * cols + c).copied().unwrap_or(0.0), x, y)
    }
}

/// Sample a grid of per-cell values (`cell(col, row)`) over a `sensor` area at sensor position `(x, y)`: bilinear
/// between cell centres. Between the outermost centres and the frame's edge, the last cell-to-cell segment continues
/// linearly (the half cell there is not flattened, which would understate a steep fall-off at the corners); past the
/// edge, the edge's value. The continuation is bounded by the nearest centre's value ± that segment's step, a safety
/// bound a genuine half-cell continuation never reaches.
pub fn sample_grid(sensor: [usize; 2], grid: [usize; 2], cell: &dyn Fn(usize, usize) -> f64, x: f64, y: f64) -> f64 {
    let [w, h] = sensor;
    let [cols, rows] = grid;
    if cols < MIN_GRID || rows < MIN_GRID || w == 0 || h == 0 {
        return 0.0;
    }
    let (x, y) = (if x.is_finite() { x } else { 0.0 }, if y.is_finite() { y } else { 0.0 });
    // position in cell units, cell centres at whole numbers; clamped to the frame's edges
    let u = (x / w as f64 * cols as f64 - 0.5).clamp(-0.5, cols as f64 - 0.5);
    let v = (y / h as f64 * rows as f64 - 0.5).clamp(-0.5, rows as f64 - 0.5);
    let c0 = (u.floor().max(0.0) as usize).min(cols - 2);
    let r0 = (v.floor().max(0.0) as usize).min(rows - 2);
    let (tx, ty) = (u - c0 as f64, v - r0 as f64);
    let (a, b, c, d) = (cell(c0, r0), cell(c0 + 1, r0), cell(c0, r0 + 1), cell(c0 + 1, r0 + 1));
    let bilinear = |tx: f64, ty: f64| {
        let top = a + (b - a) * tx;
        let bottom = c + (d - c) * tx;
        top + (bottom - top) * ty
    };
    let value = bilinear(tx, ty);
    let (out_x, out_y) = (!(0.0..=1.0).contains(&tx), !(0.0..=1.0).contains(&ty));
    if !out_x && !out_y {
        return value;
    }
    let (ix, iy) = (tx.clamp(0.0, 1.0), ty.clamp(0.0, 1.0));
    let near = bilinear(ix, iy);
    let mut step = 0.0;
    if out_x {
        step += (bilinear(1.0, iy) - bilinear(0.0, iy)).abs();
    }
    if out_y {
        step += (bilinear(ix, 1.0) - bilinear(ix, 0.0)).abs();
    }
    // (not `f64::clamp`, which panics on a NaN bound: a grid may mark cells it has no value for with NaN)
    let (lo, hi) = (near - step, near + step);
    if lo.is_nan() || hi.is_nan() || lo > hi {
        return value;
    }
    value.max(lo).min(hi)
}

impl VignettingMeasurement {
    /// Check a map read from a file: its version, grid and gains. (It says nothing about the photo it is applied to:
    /// that one's active area must equal [`Self::sensor`].)
    pub fn validate(&self) -> Result<(), VignettingCalibrationError> {
        let bad = |why: String| Err(VignettingCalibrationError::Invalid(why));
        if self.version != FORMAT_VERSION {
            return bad(format!("format {} (this build reads {FORMAT_VERSION})", self.version));
        }
        let [cols, rows] = self.grid;
        if !(MIN_GRID..=MAX_GRID).contains(&cols) || !(MIN_GRID..=MAX_GRID).contains(&rows) {
            return bad(format!("a {cols} × {rows} grid (2 to 256 per side)"));
        }
        let [w, h] = self.sensor;
        if w < cols || h < rows {
            return bad(format!("a {cols} × {rows} grid over {w} × {h} px"));
        }
        if cols.checked_mul(rows) != Some(self.gains.len()) {
            return bad(format!("{} gains for a {cols} × {rows} grid", self.gains.len()));
        }
        if let Some(g) = self.gains.iter().find(|g| !(g.is_finite() && (1.0..=MAX_GAIN).contains(*g))) {
            return bad(format!("a gain of {g} (1 to {MAX_GAIN})"));
        }
        if let Some(win) = &self.window
            && !(win.is_valid() && win.size == self.sensor)
        {
            return bad("its sensor window doesn't match its active area".into());
        }
        Ok(())
    }

    /// Where a photo whose active area is `size` (at `window` on its sensor, when known) sits on this map: the offset
    /// from the photo's sensor coordinates to the map's. The same active area takes the map as it is; a smaller
    /// image area of the same sensor (both windows known, the photo's inside the map's) takes the part under it.
    /// Anything else is refused: another sensor, an unknown position, or a photo the map doesn't cover.
    pub fn placement(&self, size: [usize; 2], window: Option<&SensorWindow>) -> Result<[f64; 2], VignettingCalibrationError> {
        let offset = self.relative_offset(size, window)?;
        match (&self.window, window) {
            (Some(m), Some(t)) if !m.contains(t) => Err(VignettingCalibrationError::Mismatch("the map doesn't cover the photo's image area".into())),
            _ => Ok(offset),
        }
    }

    /// The offset from a frame's sensor coordinates to the map's, whether or not the map covers all of the frame: the
    /// same active area (`[0, 0]`), or two known windows on one sensor (the difference of their origins). For
    /// comparing over the area two image areas share ([`check`]); applying a map needs [`Self::placement`].
    pub fn relative_offset(&self, size: [usize; 2], window: Option<&SensorWindow>) -> Result<[f64; 2], VignettingCalibrationError> {
        match (&self.window, window) {
            (Some(m), Some(t)) if m.sensor != t.sensor => Err(VignettingCalibrationError::Mismatch(format!(
                "another sensor: {} × {} px, the map's {} × {} px",
                t.sensor[0], t.sensor[1], m.sensor[0], m.sensor[1]
            ))),
            (Some(_), Some(t)) if t.size != size => {
                Err(VignettingCalibrationError::Mismatch("the photo's window doesn't match its active area".into()))
            }
            (Some(m), Some(t)) => Ok([t.origin[0] as f64 - m.origin[0] as f64, t.origin[1] as f64 - m.origin[1] as f64]),
            _ if size == self.sensor => Ok([0.0, 0.0]),
            _ => Err(VignettingCalibrationError::Mismatch(format!(
                "the active area is {} × {} px, the map's {} × {} px, and where it sits on the sensor is unknown",
                size[0], size[1], self.sensor[0], self.sensor[1]
            ))),
        }
    }

    /// The map's fall-off in EV per cell (log2 of the gains; positive = darker than the brightest cell).
    pub fn ev_grid(&self) -> EvGrid {
        EvGrid { sensor: self.sensor, grid: self.grid, ev: self.gains.iter().map(|g| f64::from(*g).max(1.0).log2()).collect() }
    }

    /// The fall-off at sensor position `(x, y)` in EV (positive = darker): the full correction in stops.
    pub fn ev_at(&self, x: f64, y: f64) -> f64 {
        let cols = self.grid[0];
        let cell = |c: usize, r: usize| self.gains.get(r * cols + c).map_or(0.0, |g| f64::from(*g).max(1.0).log2());
        sample_grid(self.sensor, self.grid, &cell, x, y)
    }

    /// The gain to apply at sensor position `(x, y)` at `amount` ([`effective_gain`]), interpolated in log space.
    pub fn gain_at(&self, x: f64, y: f64, amount: f64) -> f32 {
        (clamp_amount(amount) * self.ev_at(x, y)).exp2() as f32
    }

    /// This map's sensor position under normalized coordinates `(u, v)` of a developed source placed at `p` (the
    /// EXIF-oriented, default-cropped picture; the caller undoes the user's orientation, as for gain tables).
    /// Relative active-area position (`p.active`) → the photo's sensor pixels → this map's, by
    /// [`Self::placement`]. `None` when the map doesn't serve the photo: its active size unknown, another sensor,
    /// or an image area the map doesn't cover.
    pub fn sensor_at(&self, p: &SourcePlacement, u: f64, v: f64) -> Option<(f64, f64)> {
        let size = p.active_size?;
        let [dx, dy] = self.placement(size, p.window.as_ref()).ok()?;
        let (ax, ay) = p.active(u, v);
        Some((ax * size[0] as f64 + dx, ay * size[1] as f64 + dy))
    }

    /// The fall-off in EV under `(u, v)` of a source placed at `p` ([`Self::sensor_at`], [`Self::ev_at`]).
    pub fn ev_on(&self, p: &SourcePlacement, u: f64, v: f64) -> Option<f64> {
        self.sensor_at(p, u, v).map(|(x, y)| self.ev_at(x, y))
    }
}

/// Where a raw's sensor sits in the pictures the app develops from it: the default crop, rows stretched for
/// non-square pixels, the EXIF orientation, and the user's orientation on top. Maps between those pictures and
/// sensor coordinates (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorFrame {
    /// Active-area width and height.
    pub active: [usize; 2],
    /// The default crop within the active area (the whole area when there is none), as [`RawImage::develop_crop`].
    pub crop: Rect,
    /// The developed (default-cropped, row-stretched, unrotated) picture's width and height.
    pub developed: [usize; 2],
    /// The file's orientation.
    pub exif: Orientation,
}

impl SensorFrame {
    pub fn new(active: [usize; 2], crop: Rect, pixel_aspect: f32, exif: Orientation) -> SensorFrame {
        let [aw, ah] = active;
        let c = crop.clipped(aw, ah);
        let crop = if c.width == 0 || c.height == 0 { Rect::new(0, 0, aw, ah) } else { c };
        let developed = [crop.width, stretched_height(crop.height, pixel_aspect)];
        SensorFrame { active, crop, developed, exif }
    }

    pub fn of(raw: &RawImage) -> SensorFrame {
        SensorFrame::new([raw.active_area.width, raw.active_area.height], raw.crop, raw.pixel_aspect, raw.orientation)
    }

    /// The picture's size after the EXIF orientation.
    fn exif_size(&self) -> (f64, f64) {
        let [w, h] = self.developed;
        if self.exif.swaps_axes() { (h as f64, w as f64) } else { (w as f64, h as f64) }
    }

    /// The full-size picture's width and height after the EXIF and then the `user` orientation: the frame
    /// `Warp::gain` positions are in at full size.
    pub fn oriented_size(&self, user: Orientation) -> (f64, f64) {
        let (w, h) = self.exif_size();
        if user.swaps_axes() { (h, w) } else { (w, h) }
    }

    /// The developed picture's place in the active area, as the pipeline knows it ([`SourcePlacement`], the same for
    /// gain tables and flat-field maps): the crop relative to the active area, the EXIF orientation, the active size,
    /// and `window` (where the active area sits on the sensor, when the file says).
    pub fn placement(&self, window: Option<SensorWindow>) -> SourcePlacement {
        let (aw, ah) = (self.active[0].max(1) as f64, self.active[1].max(1) as f64);
        let c = self.crop;
        let rect = [c.x as f64 / aw, c.y as f64 / ah, c.width as f64 / aw, c.height as f64 / ah];
        SourcePlacement { active_size: Some(self.active), window, ..SourcePlacement::new(rect, self.exif) }
    }

    /// The unoriented developed picture's position `p` (px) → sensor coordinates.
    pub fn cropped_to_sensor(&self, p: Point) -> Point {
        let dh = self.developed[1].max(1) as f64;
        Point::new(self.crop.x as f64 + p.x, self.crop.y as f64 + p.y * self.crop.height as f64 / dh)
    }

    /// Sensor coordinates → the unoriented developed picture.
    pub fn sensor_to_cropped(&self, p: Point) -> Point {
        let ch = self.crop.height.max(1) as f64;
        Point::new(p.x - self.crop.x as f64, (p.y - self.crop.y as f64) * self.developed[1] as f64 / ch)
    }

    /// A position `p` in the source the geometry stage samples (`Warp::gain`'s frame: the developed picture after
    /// the EXIF and `user` orientations, at a size of `src_w × src_h`, which may be a fitted or binned copy) →
    /// sensor coordinates.
    /// Through [`Self::placement`], as the pipeline maps positions.
    pub fn source_to_sensor(&self, p: Point, src_w: f64, src_h: f64, user: Orientation) -> Point {
        let (ow, oh) = self.oriented_size(user);
        let (w, h) = (if src_w > 0.0 { src_w } else { ow.max(1.0) }, if src_h > 0.0 { src_h } else { oh.max(1.0) });
        let (u, v) = user.inverse().map(p.x / w, p.y / h, 1.0, 1.0);
        let (ax, ay) = self.placement(None).active(u, v);
        Point::new(ax * self.active[0] as f64, ay * self.active[1] as f64)
    }

    /// [`Self::source_to_sensor`] the other way.
    pub fn sensor_to_source(&self, p: Point, src_w: f64, src_h: f64, user: Orientation) -> Point {
        let (aw, ah) = (self.active[0].max(1) as f64, self.active[1].max(1) as f64);
        let (u, v) = self.placement(None).oriented(p.x / aw, p.y / ah);
        let (u, v) = user.map(u, v, 1.0, 1.0);
        let (ow, oh) = self.oriented_size(user);
        let (w, h) = (if src_w > 0.0 { src_w } else { ow }, if src_h > 0.0 { src_h } else { oh });
        Point::new(u * w, v * h)
    }
}

/// The optical centre of a fall-off ([`fit_centre`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CentreFit {
    /// Offset of the centre from the frame's centre, in % of the width and height (positive = right, down).
    pub dx_pct: f64,
    pub dy_pct: f64,
    /// RMS of the fit's residuals in EV: small for a lens's radial fall-off, larger for a gradient across the frame
    /// (a sky) or a lopsided light.
    pub rms_ev: f64,
}

impl CentreFit {
    /// Whether the fall-off is too uneven for a lens's own: the fit's RMS above 2 % of the mean corner fall-off
    /// (`corner_ev`) or above 0.03 EV, whichever is larger. A strong fall-off is allowed a proportionally larger
    /// misfit of the radial model.
    pub fn uneven(&self, corner_ev: f64) -> bool {
        let limit = (0.02 * corner_ev.abs()).max(0.03);
        self.rms_ev.is_nan() || self.rms_ev > limit
    }
}

/// Fit `EV(x, y) = a + b·d² + c·d⁴` to a grid of EV values (`d` = distance from a centre `(cx, cy)` in half-diagonal
/// units) and return the centre that fits best. The centre is searched coarse to fine within ±25 % of the frame; a,
/// b, c are linear least squares at each candidate. A flat-topped fall-off has no single brightest cell, which is why
/// the centre comes from the whole shape rather than from a peak.
pub fn fit_centre(g: &EvGrid) -> Option<CentreFit> {
    let [w, h] = g.sensor;
    let [cols, rows] = g.grid;
    if w == 0 || h == 0 || g.ev.len() != cols.checked_mul(rows)? || g.ev.len() < 4 {
        return None;
    }
    let (wf, hf) = (w as f64, h as f64);
    let hd2 = (wf * wf + hf * hf) / 4.0;
    let pts: Vec<(f64, f64, f64)> = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (c, r)))
        .filter_map(|(c, r)| {
            let p = cell_centre(g.sensor, g.grid, c, r);
            Some((p.x, p.y, *g.ev.get(r * cols + c)?))
        })
        .filter(|(_, _, e)| e.is_finite())
        .collect();
    let rms_at = |fx: f64, fy: f64| -> Option<f64> {
        let (cx, cy) = (wf * (0.5 + fx), hf * (0.5 + fy));
        let d2 = |x: f64, y: f64| ((x - cx).powi(2) + (y - cy).powi(2)) / hd2;
        // normal equations of the basis (1, d², d⁴, d⁶): moments s_k = Σ d^(2k), t_k = Σ e·d^(2k)
        let (mut s, mut t) = ([0.0f64; 7], [0.0f64; 4]);
        for &(x, y, e) in &pts {
            let q = d2(x, y);
            let mut p = 1.0;
            for (k, sk) in s.iter_mut().enumerate() {
                *sk += p;
                if let Some(tk) = t.get_mut(k) {
                    *tk += e * p;
                }
                p *= q;
            }
        }
        let [s0, s1, s2, s3, s4, s5, s6] = s;
        let m = [[s0, s1, s2, s3], [s1, s2, s3, s4], [s2, s3, s4, s5], [s3, s4, s5, s6]];
        let [a, b, c, e6] = solve4(m, t)?;
        let sum: f64 = pts
            .iter()
            .map(|&(x, y, e)| {
                let q = d2(x, y);
                (a + q * (b + q * (c + q * e6)) - e).powi(2)
            })
            .sum();
        let rms = (sum / pts.len() as f64).sqrt();
        rms.is_finite().then_some(rms)
    };
    let (mut best, mut best_rms) = ((0.0, 0.0), rms_at(0.0, 0.0)?);
    for (half, step) in [(0.25f64, 0.05f64), (0.05, 0.01), (0.01, 0.002), (0.002, 0.0004)] {
        let (bx, by) = best;
        let n = (half / step).round() as i32;
        for iy in -n..=n {
            for ix in -n..=n {
                let (fx, fy) = ((bx + ix as f64 * step).clamp(-0.25, 0.25), (by + iy as f64 * step).clamp(-0.25, 0.25));
                if let Some(rms) = rms_at(fx, fy)
                    && rms < best_rms
                {
                    (best, best_rms) = ((fx, fy), rms);
                }
            }
        }
    }
    Some(CentreFit { dx_pct: best.0 * 100.0, dy_pct: best.1 * 100.0, rms_ev: best_rms })
}

/// Solve a 4 × 4 system by Gaussian elimination with partial pivoting; `None` when it is (nearly) singular: a pivot
/// below `1e-12` of the matrix's largest entry, or a result that isn't finite.
fn solve4(m: [[f64; 4]; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    // augmented rows [m | b]
    let mut a: [[f64; 5]; 4] = [[0.0; 5]; 4];
    for ((row, mr), br) in a.iter_mut().zip(m.iter()).zip(b.iter()) {
        row[..4].copy_from_slice(mr);
        row[4] = *br;
    }
    let scale = m.iter().flatten().fold(0.0f64, |s, v| s.max(v.abs()));
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    for col in 0..4 {
        // the row with the largest entry in this column, from the diagonal down
        let pivot = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        let p = a[pivot][col].abs();
        if p.is_nan() || p <= 1e-12 * scale {
            return None;
        }
        a.swap(col, pivot);
        let head = a[col];
        for row in a.iter_mut().skip(col + 1) {
            let f = row[col] / head[col];
            for (r, h) in row.iter_mut().zip(head.iter()).skip(col) {
                *r -= f * h;
            }
        }
    }
    let mut x = [0.0f64; 4];
    for col in (0..4).rev() {
        let row = a[col];
        let known: f64 = row.iter().zip(x.iter()).skip(col + 1).take(3 - col).map(|(r, v)| r * v).sum();
        x[col] = (row[4] - known) / row[col];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// What a map leaves of a second reference's fall-off ([`check`]).
#[derive(Clone, Debug, PartialEq)]
pub struct CheckReport {
    /// The second reference's own fall-off (EV, positive = darker) relative to its brightest smoothed cell.
    pub before: EvGrid,
    /// Each corrected cell's deviation from the median of all corrected cells (EV, positive = darker).
    pub after: EvGrid,
    /// Largest |residual| in EV.
    pub max_abs: f64,
    /// Brightest minus darkest corrected cell, EV.
    pub peak_to_peak: f64,
    /// Cells of the second reference the map covers (the residuals are over these; the others are NaN in `after`).
    pub cells_used: usize,
}

impl CheckReport {
    /// Max |residual| below 0.05 EV and peak-to-peak below 0.1 EV.
    pub fn passes(&self) -> bool {
        self.max_abs < 0.05 && self.peak_to_peak < 0.1
    }
}

/// Fewest cells a check must cover.
const MIN_CHECK_CELLS: usize = 4;

/// The cells of `cells` (in their order) that lie wholly on `map` when moved by `offset` (sensor px of the cells'
/// frame → the map's), widened on every side by `margin` px.
fn covered(map: &VignettingMeasurement, cells: &CellGrid, offset: [f64; 2], margin: f64) -> Vec<usize> {
    let [cols, rows] = cells.grid;
    let [w, h] = cells.sensor;
    let (mw, mh) = (map.sensor[0] as f64, map.sensor[1] as f64);
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (c, r)))
        .filter(|&(c, r)| {
            let (x0, x1) = cell_span(c, cols, w);
            let (y0, y1) = cell_span(r, rows, h);
            let (ax, bx) = (x0 as f64 + offset[0] - margin, x1 as f64 + offset[0] + margin);
            let (ay, by) = (y0 as f64 + offset[1] - margin, y1 as f64 + offset[1] + margin);
            ax >= 0.0 && ay >= 0.0 && bx <= mw && by <= mh
        })
        .map(|(c, r)| r * cols + c)
        .collect()
}

/// Each listed cell's level after the map's full correction, log2.
fn corrected(map: &VignettingMeasurement, cells: &CellGrid, offset: [f64; 2], which: &[usize]) -> Result<Vec<f64>, VignettingCalibrationError> {
    let cols = cells.grid[0].max(1);
    which
        .iter()
        .map(|&i| {
            let m = cells.means.get(i).copied().unwrap_or(f64::NAN);
            if !(m.is_finite() && m > 1e-6) {
                return Err(VignettingCalibrationError::TooDark { col: i % cols, row: i / cols, level: m });
            }
            let p = cell_centre(cells.sensor, cells.grid, i % cols, i / cols);
            Ok(m.log2() + f64::from(map.gain_at(p.x + offset[0], p.y + offset[1], 1.0)).log2())
        })
        .collect()
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_unstable_by(f64::total_cmp);
    let mid = s.len() / 2;
    if s.len().is_multiple_of(2) {
        (s.get(mid.saturating_sub(1)).copied().unwrap_or(0.0) + s.get(mid).copied().unwrap_or(0.0)) / 2.0
    } else {
        s.get(mid).copied().unwrap_or(0.0)
    }
}

/// Apply `map` at full strength to `cells`, a second reference of the same setup measured on its own grid
/// ([`cell_means`]), placed on the map at `offset` ([`VignettingMeasurement::placement`]), and report what is left over the
/// cells the map covers. Levels are relative to the median corrected cell, not to the centre or the brightest cell, so
/// an off-centre fall-off or a lopsided light shows as a residual instead of moving the reference level.
pub fn check(
    map: &VignettingMeasurement,
    cells: &CellGrid,
    offset: [f64; 2],
    opts: &VignettingCalibrationOptions,
) -> Result<CheckReport, VignettingCalibrationError> {
    map.validate()?;
    let [cols, rows] = cells.grid;
    if cols.checked_mul(rows) != Some(cells.means.len()) {
        return Err(VignettingCalibrationError::Invalid("the cells don't match their grid".into()));
    }
    let smoothed = smooth_ev(cells, opts)?;
    let top = smoothed.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let before = EvGrid { sensor: cells.sensor, grid: cells.grid, ev: smoothed.iter().map(|e| top - e).collect() };
    let used = covered(map, cells, offset, 0.0);
    if used.len() < MIN_CHECK_CELLS {
        return Err(VignettingCalibrationError::Mismatch("the map covers too little of the second reference".into()));
    }
    let levels = corrected(map, cells, offset, &used)?;
    let mid = median(&levels);
    let mut residual = vec![f64::NAN; cells.means.len()];
    for (&i, l) in used.iter().zip(&levels) {
        if let Some(r) = residual.get_mut(i) {
            *r = mid - l;
        }
    }
    let max_abs = levels.iter().fold(0.0f64, |a, l| a.max((mid - l).abs()));
    let (lo, hi) = levels.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), l| (lo.min(*l), hi.max(*l)));
    Ok(CheckReport {
        before,
        after: EvGrid { sensor: cells.sensor, grid: cells.grid, ev: residual },
        max_abs,
        peak_to_peak: hi - lo,
        cells_used: used.len(),
    })
}

/// The offset search of [`find_offset`]: where a second reference fits the map best, against where it is said to be.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OffsetFit {
    /// The offset the file's sensor window gives (sensor px of the second reference → the map's).
    pub expected: [f64; 2],
    /// The offset with the smallest residual spread, within ±64 px of `expected`.
    pub found: [f64; 2],
    /// Residual spread (standard deviation of the corrected cells, EV) at the found and the expected offset, and the
    /// largest of the four at 16 px from the found one (how sharply the search could tell).
    pub rms_found: f64,
    pub rms_expected: f64,
    pub rms_16px: f64,
    /// The spread with no offset at all, when that placement covers the same cells (what ignoring the window costs).
    pub rms_zero: Option<f64>,
    /// Cells compared at every offset.
    pub cells: usize,
}

/// Search the offset that fits `cells` (a second reference) to `map` best: ±64 px around `expected` in steps of 4,
/// then ±4 px in steps of 1. The fit is the standard deviation of the corrected cells over a fixed set (those covered
/// at every offset searched), so it doesn't depend on a reference level. A smooth fall-off moves little over a few
/// pixels, so the minimum is shallow: [`OffsetFit::rms_16px`] says how sharply it was found.
pub fn find_offset(map: &VignettingMeasurement, cells: &CellGrid, expected: [f64; 2]) -> Result<OffsetFit, VignettingCalibrationError> {
    map.validate()?;
    let set = covered(map, cells, expected, 72.0);
    if set.len() < MIN_CHECK_CELLS {
        return Err(VignettingCalibrationError::Mismatch("the map covers too little of the second reference to search".into()));
    }
    let spread = |o: [f64; 2]| -> Result<f64, VignettingCalibrationError> {
        let v = corrected(map, cells, o, &set)?;
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        Ok((v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt())
    };
    let mut best = (expected, spread(expected)?);
    for (half, step) in [(64.0f64, 4.0f64), (4.0, 1.0)] {
        let centre = best.0;
        let n = (half / step).round() as i32;
        for iy in -n..=n {
            for ix in -n..=n {
                let o = [centre[0] + ix as f64 * step, centre[1] + iy as f64 * step];
                if (o[0] - expected[0]).abs() > 64.0 || (o[1] - expected[1]).abs() > 64.0 {
                    continue;
                }
                let s = spread(o)?;
                if s < best.1 {
                    best = (o, s);
                }
            }
        }
    }
    let (found, rms_found) = best;
    let rms_16px = [[16.0, 0.0], [-16.0, 0.0], [0.0, 16.0], [0.0, -16.0]]
        .iter()
        .map(|d| spread([found[0] + d[0], found[1] + d[1]]))
        .collect::<Result<Vec<f64>, _>>()?
        .into_iter()
        .fold(0.0f64, f64::max);
    let zero_covers = covered(map, cells, [0.0, 0.0], 0.0);
    let rms_zero = if set.iter().all(|i| zero_covers.contains(i)) { Some(spread([0.0, 0.0])?) } else { None };
    Ok(OffsetFit { expected, found, rms_found, rms_expected: spread(expected)?, rms_16px, rms_zero, cells: set.len() })
}

#[cfg(test)]
#[path = "vignetting_calibration_tests.rs"]
mod tests;
