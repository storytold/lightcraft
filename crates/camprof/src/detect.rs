// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/detect.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Locating a chart in a photograph.
//!
//! [`find_by_pattern`] is the search the application uses. It recognises the
//! chart by its colours: a grid of sample points is slid over a small copy of
//! the frame, and at each place the 24 cells' colours are correlated with the
//! chart's published ones — their lightness and two colour ratios, each over
//! the whole grid, so neither the exposure nor the white balance matters. Grass,
//! a wall or the other page of a ColorChecker Passport cannot imitate a grey
//! ramp beside eighteen hues in the right order, and the correlation also says
//! which way round the chart lies. The winner is then fitted precisely by making
//! every patch as even as possible.
//!
//! The older searches below it — [`detect`] and [`candidates`] — look only at
//! edges in a grey thumbnail. They are kept for the grids this crate has no
//! reference colours for, but on a textured background they lock onto the
//! texture.

use crate::frame::RawFrame;
use crate::linalg::Vec3;
use crate::reference::{Chart, ChartCorners, chart_cell};

// ---------------------------------------------------------------------------
// Finding a chart by its colours
// ---------------------------------------------------------------------------

/// A chart found in a frame by its colour pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternMatch {
    /// The outer corners of the patch grid, in half-resolution pixels and
    /// screen order.
    pub corners: ChartCorners,
    /// How the chart lies, counted as [`crate::reference::chart_cell`] counts.
    pub quarter_turns: u8,
    /// How closely the cells' colours follow the chart's, as a mean correlation
    /// from -1 to 1. See [`PATTERN_THRESHOLD`].
    pub score: f64,
}

/// The pattern score below which a match is not believed to be the chart.
///
/// On the three test photographs the chart scores 0.95 and 0.96, the best
/// placement anywhere else in the frame 0.45 to 0.55, and the chart itself
/// read the wrong way round 0.35 at most.
pub const PATTERN_THRESHOLD: f64 = 0.75;

/// Width of the working copy the coarse search runs on, in blocks.
const SEARCH_WIDTH: usize = 480;

/// A patch spacing, in blocks, the fine fit works at.
const FINE_PITCH: f64 = 24.0;

/// Block-averaged camera RGB with summed-area tables of the values and of their
/// squares, so the mean and variance of any rectangle cost four look-ups.
struct Summed {
    width: usize,
    height: usize,
    /// Half-resolution pixels per block.
    factor: usize,
    /// Half-resolution position of block (0, 0).
    origin: [usize; 2],
    sum: Vec<[f64; 3]>,
    square: Vec<[f64; 3]>,
}

impl Summed {
    fn new(frame: &RawFrame, factor: usize, window: [usize; 4]) -> Self {
        let (blocks, width, height) = frame.block_means(factor, window);
        let stride = width + 1;
        let mut sum = vec![[0.0f64; 3]; stride * (height + 1)];
        let mut square = vec![[0.0f64; 3]; stride * (height + 1)];
        for y in 0..height {
            let mut run = [0.0f64; 3];
            let mut run_square = [0.0f64; 3];
            for x in 0..width {
                let v = blocks[y * width + x];
                for c in 0..3 {
                    run[c] += v.v[c];
                    run_square[c] += v.v[c] * v.v[c];
                    sum[(y + 1) * stride + x + 1][c] = sum[y * stride + x + 1][c] + run[c];
                    square[(y + 1) * stride + x + 1][c] = square[y * stride + x + 1][c] + run_square[c];
                }
            }
        }
        Self { width, height, factor: factor.max(1), origin: [window[0], window[1]], sum, square }
    }

    /// The summed-area table at a fractional position. The image is constant
    /// over each block, so its integral is bilinear inside each block and this
    /// interpolation is exact.
    fn integral_at(table: &[[f64; 3]], stride: usize, x: f64, y: f64) -> [f64; 3] {
        let (xi, yi) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = (x - xi as f64, y - yi as f64);
        let x1 = (xi + 1).min(stride - 1);
        let rows = table.len() / stride;
        let y1 = (yi + 1).min(rows - 1);
        let mut out = [0.0f64; 3];
        for c in 0..3 {
            let a = table[yi * stride + xi][c];
            let b = table[yi * stride + x1][c];
            let d = table[y1 * stride + xi][c];
            let e = table[y1 * stride + x1][c];
            out[c] = a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + d * (1.0 - fx) * fy + e * fx * fy;
        }
        out
    }

    /// Mean and summed variance over the box of half-size `half` around
    /// `centre`, exactly, edges and all, in blocks; `None` when the box leaves
    /// the copy. Unlike [`Summed::cell`] this changes smoothly as the box
    /// moves by fractions of a block, which a fine fit needs.
    fn cell_exact(&self, centre: [f64; 2], half: f64) -> Option<([f64; 3], f64)> {
        let (x0, y0, x1, y1) = (centre[0] - half, centre[1] - half, centre[0] + half, centre[1] + half);
        if !(x0 >= 0.0 && y0 >= 0.0 && x1 <= self.width as f64 && y1 <= self.height as f64) || half <= 0.0 {
            return None;
        }
        let stride = self.width + 1;
        let area = (x1 - x0) * (y1 - y0);
        let s = |table: &[[f64; 3]]| {
            let a = Self::integral_at(table, stride, x1, y1);
            let b = Self::integral_at(table, stride, x0, y1);
            let c = Self::integral_at(table, stride, x1, y0);
            let d = Self::integral_at(table, stride, x0, y0);
            [a[0] - b[0] - c[0] + d[0], a[1] - b[1] - c[1] + d[1], a[2] - b[2] - c[2] + d[2]]
        };
        let (sum, square) = (s(&self.sum), s(&self.square));
        let mut mean = [0.0f64; 3];
        let mut variance = 0.0;
        for c in 0..3 {
            mean[c] = sum[c] / area;
            variance += (square[c] / area - mean[c] * mean[c]).max(0.0);
        }
        Some((mean, variance))
    }

    /// Mean and summed variance of the box of half-size `half` around
    /// `centre`, in blocks; `None` when the box leaves the copy.
    fn cell(&self, centre: [f64; 2], half: f64) -> Option<([f64; 3], f64)> {
        let x0 = (centre[0] - half).round();
        let y0 = (centre[1] - half).round();
        let x1 = (centre[0] + half).round().max(x0 + 1.0);
        let y1 = (centre[1] + half).round().max(y0 + 1.0);
        if x0 < 0.0 || y0 < 0.0 || x1 > self.width as f64 || y1 > self.height as f64 {
            return None;
        }
        let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
        let stride = self.width + 1;
        let area = ((x1 - x0) * (y1 - y0)) as f64;
        let mut mean = [0.0f64; 3];
        let mut variance = 0.0f64;
        for c in 0..3 {
            let s = self.sum[y1 * stride + x1][c] - self.sum[y0 * stride + x1][c] - self.sum[y1 * stride + x0][c] + self.sum[y0 * stride + x0][c];
            let q = self.square[y1 * stride + x1][c] - self.square[y0 * stride + x1][c] - self.square[y1 * stride + x0][c]
                + self.square[y0 * stride + x0][c];
            mean[c] = s / area;
            variance += (q / area - mean[c] * mean[c]).max(0.0);
        }
        Some((mean, variance))
    }
}

/// A grid of cells: centre, spacing across and down, rotation, and shape, in
/// the blocks of a [`Summed`] copy.
#[derive(Debug, Clone, Copy)]
struct Grid {
    centre: [f64; 2],
    pitch: [f64; 2],
    angle: f64,
    rows: usize,
    columns: usize,
}

impl Grid {
    fn at(&self, u: f64, v: f64) -> [f64; 2] {
        let (s, c) = self.angle.sin_cos();
        [self.centre[0] + u * c - v * s, self.centre[1] + u * s + v * c]
    }

    fn cell(&self, row: usize, column: usize) -> [f64; 2] {
        let u = (column as f64 + 0.5 - self.columns as f64 / 2.0) * self.pitch[0];
        let v = (row as f64 + 0.5 - self.rows as f64 / 2.0) * self.pitch[1];
        self.at(u, v)
    }

    fn corners(&self) -> [[f64; 2]; 4] {
        let (w, h) = (self.columns as f64 * self.pitch[0] / 2.0, self.rows as f64 * self.pitch[1] / 2.0);
        [self.at(-w, -h), self.at(w, -h), self.at(w, h), self.at(-w, h)]
    }
}

/// Lightness and two colour ratios of a colour, in logarithms, so that an
/// exposure or a white balance is a constant added to each.
fn pattern_features(rgb: [f64; 3]) -> [f64; 3] {
    let r = rgb[0].max(1.0e-6);
    let g = rgb[1].max(1.0e-6);
    let b = rgb[2].max(1.0e-6);
    [(r + g + b).ln(), (r / g).ln(), (b / g).ln()]
}

/// Remove the mean of each feature over the cells and scale it to unit length.
fn standardise(features: &mut [[f64; 3]]) -> bool {
    let n = features.len() as f64;
    for k in 0..3 {
        let mean = features.iter().map(|f| f[k]).sum::<f64>() / n;
        let mut norm = 0.0;
        for f in features.iter_mut() {
            f[k] -= mean;
            norm += f[k] * f[k];
        }
        let norm = norm.sqrt();
        if !(norm > 1.0e-9) {
            return false;
        }
        for f in features.iter_mut() {
            f[k] /= norm;
        }
    }
    true
}

/// The chart's features in frame order for each quarter turn: what the cells
/// of a frame grid of that orientation should read, cell by cell.
fn expected_patterns(chart: &Chart) -> Option<[Vec<[f64; 3]>; 4]> {
    let (rows, columns) = (chart.layout.rows, chart.layout.columns);
    if chart.patches.len() != rows * columns || rows < 2 || columns < 2 {
        return None;
    }
    // A wide-gamut RGB stands in for the camera: every chart colour is
    // positive in ProPhoto, and a correlation does not need the camera's own
    // primaries, only colours in the right order.
    let to_rgb = crate::color::prophoto_to_xyz().inverted_or_identity();
    let chart_features: Vec<[f64; 3]> = chart
        .patches
        .iter()
        .map(|p| {
            let xyz = crate::color::lab_to_xyz(Vec3::new(p.lab[0], p.lab[1], p.lab[2]), crate::color::D50);
            let rgb = to_rgb * xyz;
            pattern_features([rgb[0].max(1.0e-3), rgb[1].max(1.0e-3), rgb[2].max(1.0e-3)])
        })
        .collect();
    let mut out: [Vec<[f64; 3]>; 4] = Default::default();
    for (turns, slot) in out.iter_mut().enumerate() {
        let (frame_rows, frame_columns) = if turns % 2 == 0 { (rows, columns) } else { (columns, rows) };
        let mut features = Vec::with_capacity(rows * columns);
        for fr in 0..frame_rows {
            for fc in 0..frame_columns {
                let (r, c) = chart_cell(turns as u8, fr, fc, rows, columns);
                features.push(chart_features[r * columns + c]);
            }
        }
        if !standardise(&mut features) {
            return None;
        }
        *slot = features;
    }
    Some(out)
}

/// The cells' standardised features, or `None` when a cell leaves the copy or
/// the grid is flat.
fn observed_pattern(image: &Summed, grid: &Grid, half: f64, out: &mut Vec<[f64; 3]>) -> bool {
    out.clear();
    for row in 0..grid.rows {
        for column in 0..grid.columns {
            match image.cell(grid.cell(row, column), half) {
                Some((mean, _)) => out.push(pattern_features(mean)),
                None => return false,
            }
        }
    }
    standardise(out)
}

/// Mean correlation of observed and expected features.
fn correlation(observed: &[[f64; 3]], expected: &[[f64; 3]]) -> f64 {
    let mut total = 0.0;
    for k in 0..3 {
        total += observed.iter().zip(expected.iter()).map(|(o, e)| o[k] * e[k]).sum::<f64>();
    }
    total / 3.0
}

/// How uneven the patches are: the variance inside the sampled boxes over the
/// spread between them. A grid that sits on the patches samples flat colour
/// and scores near zero; one that straddles a patch edge picks up the edge. The
/// spread in the denominator stops a blank surface, which is perfectly even,
/// from scoring well. Each box has half-size `half`, in blocks; it is fixed
/// rather than a fraction of the grid's own spacing, because a box that
/// shrinks with the grid takes in less of every edge and so rewards shrinking.
fn unevenness(image: &Summed, grid: &Grid, half: f64) -> Option<f64> {
    let n = (grid.rows * grid.columns) as f64;
    let mut means = Vec::with_capacity(grid.rows * grid.columns);
    let mut within = 0.0;
    for row in 0..grid.rows {
        for column in 0..grid.columns {
            let (mean, variance) = image.cell_exact(grid.cell(row, column), half)?;
            within += variance;
            means.push(mean);
        }
    }
    let mut centre = [0.0f64; 3];
    for m in &means {
        for c in 0..3 {
            centre[c] += m[c] / n;
        }
    }
    let between: f64 = means.iter().map(|m| (0..3).map(|c| (m[c] - centre[c]).powi(2)).sum::<f64>()).sum();
    (between > 0.0).then(|| within / between)
}

/// Move, stretch and turn a grid to make its patches as even as possible with
/// boxes covering `box_fraction` of the starting cell, from steps of `scale`
/// times the default ones.
fn fit_grid(image: &Summed, start: Grid, box_fraction: f64, scale: f64) -> Option<Grid> {
    let half = 0.5 * box_fraction * start.pitch[0].min(start.pitch[1]);
    let mut grid = start;
    let mut cost = unevenness(image, &grid, half)?;
    let pitch0 = grid.pitch[0];
    let mut steps = [0.15 * pitch0, 0.15 * pitch0, 0.04 * pitch0, 0.04 * pitch0, 0.03].map(|s| s * scale);
    for _ in 0..400 {
        let mut improved = false;
        for parameter in 0..5 {
            for sign in [-1.0f64, 1.0] {
                let mut candidate = grid;
                match parameter {
                    0 => candidate.centre[0] += sign * steps[0],
                    1 => candidate.centre[1] += sign * steps[1],
                    2 => candidate.pitch[0] += sign * steps[2],
                    3 => candidate.pitch[1] += sign * steps[3],
                    _ => candidate.angle += sign * steps[4],
                }
                // Square patches, and a camera roughly facing the chart.
                let ratio = candidate.pitch[0] / candidate.pitch[1];
                if !(0.8..=1.25).contains(&ratio) || candidate.angle.abs() > 0.3 || candidate.pitch[0] < 2.0 {
                    continue;
                }
                if let Some(c) = unevenness(image, &candidate, half)
                    && c < cost
                {
                    cost = c;
                    grid = candidate;
                    improved = true;
                }
            }
        }
        if !improved {
            for s in steps.iter_mut() {
                *s *= 0.5;
            }
            if steps[0] < 0.002 * pitch0 {
                break;
            }
        }
    }
    Some(grid)
}

/// Whether [`find_by_pattern`] can look for this chart: it needs every patch's
/// reference colour and a grid of at least two by two.
pub fn find_by_pattern_possible(chart: &Chart) -> bool {
    expected_patterns(chart).is_some()
}

/// Find a chart by its colour pattern. See the module documentation.
///
/// Returns `None` when nothing in the frame follows the chart's pattern well
/// enough ([`PATTERN_THRESHOLD`]), or when the chart has no reference colours
/// to look for.
pub fn find_by_pattern(frame: &RawFrame, chart: &Chart) -> Option<PatternMatch> {
    let expected = expected_patterns(chart)?;
    let (half_width, half_height) = (frame.width, frame.height);
    if half_width < 64 || half_height < 64 {
        return None;
    }
    let (rows, columns) = (chart.layout.rows, chart.layout.columns);

    // Coarse: every place, size and orientation on a small copy. Each row of
    // places is a task of its own, and the tasks are shared out over the
    // processor's cores; a chart of 140 patches is six times the work of one of
    // 24, all of it independent.
    let factor = (half_width as f64 / SEARCH_WIDTH as f64).round().max(1.0) as usize;
    let coarse = Summed::new(frame, factor, [0, 0, half_width, half_height]);
    let mut tasks: Vec<(usize, usize, f64, f64)> = Vec::new();
    let shapes = if rows == columns { vec![(rows, columns)] } else { vec![(rows, columns), (columns, rows)] };
    for (frame_rows, frame_columns) in shapes {
        let largest = (coarse.width as f64 / frame_columns as f64).min(coarse.height as f64 / frame_rows as f64);
        let mut pitch = 4.0f64;
        while pitch <= largest {
            let step = (pitch / 3.0).max(1.0);
            let half_h = frame_rows as f64 * pitch / 2.0;
            let mut cy = half_h;
            while cy <= coarse.height as f64 - half_h {
                tasks.push((frame_rows, frame_columns, pitch, cy));
                cy += step;
            }
            pitch *= 1.06;
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 32).min(tasks.len().max(1));
    // The best score, and the task and column it came from so that equal
    // scores resolve the same way whichever thread found them.
    type Best = (f64, usize, usize, Grid, u8);
    let better = |a: &Best, b: &Best| a.0 > b.0 || (a.0 == b.0 && (a.1, a.2) < (b.1, b.2));
    let best: Option<Best> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut best: Option<Best> = None;
                    let mut observed = Vec::with_capacity(rows * columns);
                    loop {
                        let task = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(frame_rows, frame_columns, pitch, cy)) = tasks.get(task) else {
                            break;
                        };
                        let step = (pitch / 3.0).max(1.0);
                        let half_w = frame_columns as f64 * pitch / 2.0;
                        let half = (pitch * 0.22).max(0.5);
                        let mut cx = half_w;
                        let mut place = 0usize;
                        while cx <= coarse.width as f64 - half_w {
                            let grid = Grid { centre: [cx, cy], pitch: [pitch, pitch], angle: 0.0, rows: frame_rows, columns: frame_columns };
                            if observed_pattern(&coarse, &grid, half, &mut observed) {
                                for t in 0..4u8 {
                                    let upright = if t % 2 == 0 { frame_rows == rows } else { frame_rows == columns };
                                    if !upright {
                                        continue;
                                    }
                                    let candidate = (correlation(&observed, &expected[t as usize]), task, place, grid, t);
                                    if best.as_ref().is_none_or(|b| better(&candidate, b)) {
                                        best = Some(candidate);
                                    }
                                }
                            }
                            cx += step;
                            place += 1;
                        }
                    }
                    best
                })
            })
            .collect();
        workers.into_iter().filter_map(|w| w.join().ok().flatten()).reduce(|a, b| if better(&a, &b) { a } else { b })
    });
    let (coarse_score, _, _, coarse_grid, turns) = best?;
    let mut observed = Vec::with_capacity(rows * columns);
    if coarse_score < PATTERN_THRESHOLD {
        return None;
    }

    // Fine: a sharper copy of the chart's neighbourhood, where the grid is fitted
    // so every sampled box sits inside its patch.
    let pitch_half = coarse_grid.pitch[0] * factor as f64;
    let fine_factor = (pitch_half / FINE_PITCH).round().max(1.0) as usize;
    let centre_half = [coarse_grid.centre[0] * factor as f64, coarse_grid.centre[1] * factor as f64];
    let reach = (coarse_grid.rows.max(coarse_grid.columns) as f64 + 2.0) * pitch_half;
    let window = [
        (centre_half[0] - reach).max(0.0) as usize,
        (centre_half[1] - reach).max(0.0) as usize,
        (centre_half[0] + reach).min(half_width as f64) as usize,
        (centre_half[1] + reach).min(half_height as f64) as usize,
    ];
    let fine = Summed::new(frame, fine_factor, window);
    let to_fine = |p: f64, axis: usize| (p - fine.origin[axis] as f64) / fine.factor as f64;
    let start = Grid { centre: [to_fine(centre_half[0], 0), to_fine(centre_half[1], 1)], pitch: [pitch_half / fine.factor as f64; 2], ..coarse_grid };
    // First with boxes well inside the patches, which converges from a rough
    // start but leaves each box free to slide wherever it stays inside; then
    // with boxes a little larger than a patch, which reach into the gaps and are
    // evenest exactly when centred.
    let grid = fit_grid(&fine, start, 0.6, 1.0)?;
    let grid = fit_grid(&fine, grid, 0.9, 0.3)?;

    // The fitted grid must still read as the chart, the same way round.
    let half = 0.3 * grid.pitch[0].min(grid.pitch[1]);
    if !observed_pattern(&fine, &grid, half, &mut observed) {
        return None;
    }
    let score = correlation(&observed, &expected[turns as usize]);
    if score < PATTERN_THRESHOLD {
        return None;
    }
    let to_half = |p: [f64; 2]| [fine.origin[0] as f64 + p[0] * fine.factor as f64, fine.origin[1] as f64 + p[1] * fine.factor as f64];
    let [tl, tr, br, bl] = grid.corners().map(to_half);
    Some(PatternMatch { corners: ChartCorners { top_left: tl, top_right: tr, bottom_right: br, bottom_left: bl }, quarter_turns: turns, score })
}

/// [`find_by_pattern`] for a chart whose reference may not say which way its
/// grid is read ([`Chart::reading_known`]): the chart is looked for as given and
/// transposed, and the better match wins. A photograph cannot show the mirror
/// image the wrong reading describes, so it scores well below the threshold.
///
/// Returns the match and, when the transposed reading won, that chart, which is
/// the one the match's corners and orientation refer to.
pub fn find_by_pattern_either_way(frame: &RawFrame, chart: &Chart) -> Option<(PatternMatch, Option<Chart>)> {
    let as_given = find_by_pattern(frame, chart);
    if chart.reading_known {
        return as_given.map(|found| (found, None));
    }
    let transposed = chart.transposed();
    let other = find_by_pattern(frame, &transposed);
    match (as_given, other) {
        (Some(a), Some(b)) if b.score > a.score => Some((b, Some(transposed))),
        (Some(a), _) => Some((a, None)),
        (None, Some(b)) => Some((b, Some(transposed))),
        (None, None) => None,
    }
}

/// What a search found.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// The best corners found, in preview coordinates.
    pub corners: ChartCorners,
    /// How strongly the patches differ from each other, per unit of signal.
    ///
    /// A chart in a photograph gives roughly 0.2 and up. A blank surface gives
    /// close to zero. The number is not calibrated, so it is used for comparing
    /// candidate positions and for warning, not as a probability.
    pub score: f64,
    /// How many candidates were considered, for diagnostics.
    pub tried: usize,
    /// Quarter turns clockwise applied to the corners to match the chart's
    /// orientation in the frame, zero to three.
    pub quarter_turns: u8,
}

/// A low resolution grey copy of a preview, which is all the search needs.
struct Thumbnail {
    width: usize,
    height: usize,
    /// Rec. 709 luminance.
    luma: Vec<f64>,
}

impl Thumbnail {
    fn from_preview(pixels: &[Vec3], width: usize, height: usize) -> Self {
        let luma = pixels.iter().map(|p| 0.2126 * p.v[0] + 0.7152 * p.v[1] + 0.0722 * p.v[2]).collect();
        Self { width, height, luma }
    }

    /// Bilinear sample, clamped at the edges.
    fn at(&self, x: f64, y: f64) -> f64 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let x = x.clamp(0.0, (self.width - 1) as f64);
        let y = y.clamp(0.0, (self.height - 1) as f64);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let top = self.luma[y0 * self.width + x0] * (1.0 - fx) + self.luma[y0 * self.width + x1] * fx;
        let bottom = self.luma[y1 * self.width + x0] * (1.0 - fx) + self.luma[y1 * self.width + x1] * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// Score how well a candidate rectangle lines up with a patch grid.
///
/// The measure is the luminance change across each cell boundary. A grid that
/// sits on the patches has a strong transition at every internal edge, because
/// that is where one patch meets the next. A grid that is offset, or that
/// straddles the card's outer edge, has flat interiors and weak internal edges.
///
/// An earlier version scored the spread of the cell means instead. That rewards
/// any dark-against-light boundary, so it locked onto the edge of the dark card
/// against a bright wall and produced a confidently wrong placement. Edge energy
/// between neighbouring cells cannot be satisfied that way.
fn score_cells(thumb: &Thumbnail, corners: &ChartCorners, chart: &Chart) -> f64 {
    let layout = chart.layout;
    let columns = layout.columns.max(1);
    let rows = layout.rows.max(1);
    if layout.patches() == 0 {
        return f64::NEG_INFINITY;
    }
    // The cell size, and a probe that stays inside the cells either side of a
    // boundary so the gap between patches is not sampled.
    let extent = corners.patch_extent(layout, 0.30);

    let mut total = 0.0f64;
    let mut count = 0usize;
    let mut absolute = 0.0f64;

    // Compare neighbouring cells across each internal vertical boundary.
    for row in 0..rows {
        for column in 0..columns.saturating_sub(1) {
            let left = corners.patch_centre(layout, row * columns + column);
            let right = corners.patch_centre(layout, row * columns + column + 1);
            let a = thumb.at(left[0] + extent[0] * 0.7, left[1]);
            let b = thumb.at(right[0] - extent[0] * 0.7, right[1]);
            if !a.is_finite() || !b.is_finite() {
                return f64::NEG_INFINITY;
            }
            total += (a - b).abs();
            absolute += a + b;
            count += 1;
        }
    }
    // And across each internal horizontal boundary.
    for row in 0..rows.saturating_sub(1) {
        for column in 0..columns {
            let top = corners.patch_centre(layout, row * columns + column);
            let bottom = corners.patch_centre(layout, (row + 1) * columns + column);
            let a = thumb.at(top[0], top[1] + extent[1] * 0.7);
            let b = thumb.at(bottom[0], bottom[1] - extent[1] * 0.7);
            if !a.is_finite() || !b.is_finite() {
                return f64::NEG_INFINITY;
            }
            total += (a - b).abs();
            absolute += a + b;
            count += 1;
        }
    }
    if count == 0 || absolute <= 1e-9 {
        return 0.0;
    }
    // Normalise by the overall level so a dim photograph scores like a bright
    // one and the number is comparable between frames.
    total / count as f64 / (absolute / (count as f64 * 2.0)).max(1e-6)
}
/// The chart as it appears in the frame: its own layout when upright, the
/// transposed layout when it lies on its side.
fn oriented(chart: &Chart, on_side: bool) -> Chart {
    let mut shape = chart.clone();
    if on_side {
        shape.layout = crate::reference::Layout { columns: chart.layout.rows, rows: chart.layout.columns };
    }
    shape
}

/// Score an axis-aligned grid of `shape` centred at `centre`, `width` wide.
fn evaluate_at(thumb: &Thumbnail, centre: (f64, f64), width: f64, shape: &Chart) -> Option<(f64, ChartCorners)> {
    let aspect = shape.layout.rows as f64 / shape.layout.columns.max(1) as f64;
    let height = width * aspect;
    let corners = ChartCorners::from_rect(centre.0 - width / 2.0, centre.1 - height / 2.0, width, height);
    if !corners.is_usable() {
        return None;
    }
    let score = score_cells(thumb, &corners, shape);
    score.is_finite().then_some((score, corners))
}

/// Search a preview for a chart of the given shape.
///
/// `preview_width` is the width the pixels are laid out at; the returned corners
/// use the same units and are in screen order. The chart may lie upright or on
/// its side, and both are searched; [`Detection::quarter_turns`] is 0 for an
/// upright grid and 1 for one on its side. Which of the two orientations of
/// that shape is right needs the colours, so [`crate::align::resolve_orientation`]
/// decides it.
pub fn detect(pixels: &[Vec3], preview_width: usize, chart: &Chart, expected_size: Option<f64>) -> Option<Detection> {
    if preview_width == 0 || pixels.is_empty() {
        return None;
    }
    let preview_height = pixels.len() / preview_width;
    if preview_height == 0 {
        return None;
    }

    // Work at a reduced size: the search only needs the patch layout, and this
    // keeps a full sweep cheap.
    let target_width = preview_width.clamp(8, 180);
    let target_height = (preview_height * target_width / preview_width).max(8);
    let mut small = Vec::with_capacity(target_width * target_height);
    for y in 0..target_height {
        let sy = y * preview_height / target_height;
        for x in 0..target_width {
            let sx = x * preview_width / target_width;
            small.push(pixels[(sy * preview_width + sx).min(pixels.len() - 1)]);
        }
    }
    let thumb = Thumbnail::from_preview(&small, target_width, target_height);

    let mut tried = 0usize;
    let mut best: Option<(f64, ChartCorners, bool)> = None;
    let base_width = expected_size.unwrap_or(0.6) * target_width as f64;

    for on_side in [false, true] {
        let shape = oriented(chart, on_side);
        let aspect = shape.layout.rows as f64 / shape.layout.columns.max(1) as f64;
        // The chart is expected to fill a decent part of the frame, so widths
        // around the expected size are swept. A chart on its side is narrower
        // for the same physical size.
        let base = if on_side { base_width * aspect.recip().min(1.0) } else { base_width };
        for width in [base * 0.7, base * 0.85, base, base * 1.15, base * 1.3] {
            if width < 6.0 {
                continue;
            }
            let height = width * aspect;
            if height > target_height as f64 || width > target_width as f64 {
                continue;
            }
            let step = (width / 12.0).max(1.0);
            let mut cy = height / 2.0;
            while cy <= target_height as f64 - height / 2.0 {
                let mut cx = width / 2.0;
                while cx <= target_width as f64 - width / 2.0 {
                    tried += 1;
                    if let Some((score, corners)) = evaluate_at(&thumb, (cx, cy), width, &shape)
                        && best.as_ref().is_none_or(|(b, _, _)| score > *b)
                    {
                        best = Some((score, corners, on_side));
                    }
                    cx += step;
                }
                cy += step;
            }
        }
    }

    let (mut best_score, mut best_corners, on_side) = best?;
    let shape = oriented(chart, on_side);

    // Refine around the winner, halving the step each time.
    let mut step = (base_width / 12.0).max(0.5);
    for _ in 0..10 {
        let (cx, cy) = ((best_corners.top_left[0] + best_corners.top_right[0]) / 2.0, (best_corners.top_left[1] + best_corners.bottom_left[1]) / 2.0);
        let width = best_corners.top_right[0] - best_corners.top_left[0];
        let mut improved = false;
        for dy in [-1.0f64, 0.0, 1.0] {
            for dx in [-1.0f64, 0.0, 1.0] {
                for dw in [-1.0f64, 0.0, 1.0] {
                    let candidate = evaluate_at(&thumb, (cx + dx * step, cy + dy * step), (width + dw * step).max(4.0), &shape);
                    tried += 1;
                    if let Some((score, corners)) = candidate
                        && score > best_score
                    {
                        best_score = score;
                        best_corners = corners;
                        improved = true;
                    }
                }
            }
        }
        step *= 0.5;
        if !improved && step < 0.15 {
            break;
        }
    }

    // Return to preview units.
    let scale = preview_width as f64 / target_width as f64;
    let rescale = |p: [f64; 2]| [p[0] * scale, p[1] * scale];
    Some(Detection {
        corners: ChartCorners {
            top_left: rescale(best_corners.top_left),
            top_right: rescale(best_corners.top_right),
            bottom_right: rescale(best_corners.bottom_right),
            bottom_left: rescale(best_corners.bottom_left),
        },
        score: best_score,
        tried,
        quarter_turns: u8::from(on_side),
    })
}

/// The best few placements, rather than only the best one.
///
/// A single winner is not enough when the frame holds more than one target: the
/// search optimises for how strongly the cells differ, which a chart edge against
/// a bright background can satisfy better than the chart itself. Returning
/// several candidates lets the caller try each and decide with a criterion that
/// knows what a chart is — the evenness of the patch interiors.
///
/// Candidates are spread out: a later one is only kept if it is not centred
/// within `separation` of one already chosen, so the list covers distinct
/// regions instead of many near-copies of the same answer.
pub fn candidates(
    pixels: &[Vec3],
    preview_width: usize,
    chart: &Chart,
    expected_size: Option<f64>,
    wanted: usize,
    separation: f64,
) -> Vec<Detection> {
    if preview_width == 0 || pixels.is_empty() || wanted == 0 {
        return Vec::new();
    }
    let preview_height = pixels.len() / preview_width;
    if preview_height == 0 {
        return Vec::new();
    }
    let target_width = preview_width.clamp(8, 180);
    let target_height = (preview_height * target_width / preview_width).max(8);
    let mut small = Vec::with_capacity(target_width * target_height);
    for y in 0..target_height {
        let sy = y * preview_height / target_height;
        for x in 0..target_width {
            let sx = x * preview_width / target_width;
            small.push(pixels[(sy * preview_width + sx).min(pixels.len() - 1)]);
        }
    }
    let thumb = Thumbnail::from_preview(&small, target_width, target_height);
    let aspect = chart.layout.rows as f64 / chart.layout.columns as f64;

    let mut scored: Vec<(f64, ChartCorners)> = Vec::new();
    let centre_of = |c: &ChartCorners| {
        [
            (c.top_left[0] + c.top_right[0] + c.bottom_left[0] + c.bottom_right[0]) / 4.0,
            (c.top_left[1] + c.bottom_right[1] + c.top_right[1] + c.bottom_left[1]) / 4.0,
        ]
    };
    let width_of = |c: &ChartCorners| (c.top_right[0] - c.top_left[0]).abs();

    let base_width = expected_size.unwrap_or(0.6) * target_width as f64;
    // A wider range than the single-best search uses: the chart may be much
    // smaller than the frame, which is exactly the case that fails today.
    for scale in [0.25, 0.4, 0.55, 0.7, 0.85, 1.0, 1.15] {
        let width = base_width * scale;
        if width < 6.0 {
            continue;
        }
        let height = width * aspect;
        if height > target_height as f64 || width > target_width as f64 {
            continue;
        }
        let step = (width / 10.0).max(1.5);
        let mut cy = height / 2.0;
        while cy <= target_height as f64 - height / 2.0 {
            let mut cx = width / 2.0;
            while cx <= target_width as f64 - width / 2.0 {
                let corners = ChartCorners::from_rect(cx - width / 2.0, cy - height / 2.0, width, height);
                if corners.is_usable() {
                    let score = score_cells(&thumb, &corners, chart);
                    if score.is_finite() {
                        scored.push((score, corners));
                    }
                }
                cx += step;
            }
            cy += step;
        }
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));

    let scale = preview_width as f64 / target_width as f64;
    let mut chosen: Vec<Detection> = Vec::with_capacity(wanted);
    let mut tried = 0usize;
    for (score, corners) in scored {
        tried += 1;
        let centre = centre_of(&corners);
        let width = width_of(&corners);
        let too_close = chosen.iter().any(|existing| {
            let other = centre_of(&existing.corners);
            let distance = ((other[0] - centre[0]).powi(2) + (other[1] - centre[1]).powi(2)).sqrt();
            // Relative to the grid's own size, so a large chart and a small one
            // are separated on their own terms.
            distance < separation * width.max(width_of(&existing.corners))
        });
        if too_close {
            continue;
        }
        let rescale = |p: [f64; 2]| [p[0] * scale, p[1] * scale];
        chosen.push(Detection {
            corners: ChartCorners {
                top_left: rescale(corners.top_left),
                top_right: rescale(corners.top_right),
                bottom_right: rescale(corners.bottom_right),
                bottom_left: rescale(corners.bottom_left),
            },
            score,
            tried,
            quarter_turns: 0,
        });
        if chosen.len() >= wanted {
            break;
        }
    }
    chosen
}

/// Detect a chart directly from a frame, rendering the preview it needs.
pub fn detect_in_frame(frame: &RawFrame, chart: &Chart) -> Option<Detection> {
    let preview_width = 700usize;
    let half_w = frame.width;
    let half_h = frame.height;
    let preview_height = (preview_width * half_h / half_w.max(1)).max(1);
    let pixels = frame.preview(preview_width, preview_height);
    detect(&pixels, preview_width, chart, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::COLORCHECKER_CLASSIC;

    /// A frame holding a ColorChecker in its published colours, as a camera
    /// with a strong colour cast would record it, on a busy textured
    /// background — grass, more or less — with the frame grid's outer corners
    /// at `corners` (half-resolution pixels) and the chart lying `turns`
    /// quarter turns round.
    fn chart_on_grass(half_width: usize, half_height: usize, corners: &ChartCorners, turns: u8) -> RawFrame {
        chart_on_grass_of(&COLORCHECKER_CLASSIC, half_width, half_height, corners, turns)
    }

    /// [`chart_on_grass`] for any chart.
    fn chart_on_grass_of(chart: &Chart, half_width: usize, half_height: usize, corners: &ChartCorners, turns: u8) -> RawFrame {
        let (width, height) = (half_width * 2, half_height * 2);
        let to_rgb = crate::color::prophoto_to_xyz().inverted_or_identity();
        let cast = [0.45, 1.0, 0.7];
        let mut seed = 12345u64;
        let mut noise = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64) / (1u64 << 31) as f64
        };
        // Texture in blobs a few pixels across, so it has edges at every scale
        // the search looks at.
        let mut half = vec![[0.0f64; 3]; half_width * half_height];
        let blob = 3;
        for by in (0..half_height).step_by(blob) {
            for bx in (0..half_width).step_by(blob) {
                let level = 0.05 + 0.3 * noise();
                let tint = [0.8 + 0.4 * noise(), 1.0, 0.6 + 0.3 * noise()];
                for y in by..(by + blob).min(half_height) {
                    for x in bx..(bx + blob).min(half_width) {
                        half[y * half_width + x] = [level * tint[0], level, level * tint[2]];
                    }
                }
            }
        }
        let (chart_rows, chart_columns) = (chart.layout.rows, chart.layout.columns);
        let (rows, columns) = if turns.is_multiple_of(2) { (chart_rows, chart_columns) } else { (chart_columns, chart_rows) };
        let layout = crate::reference::Layout { columns, rows };
        // The card behind the patches is a dark grey; a patch covers 84% of its
        // cell, as on a real ColorChecker.
        let extent = corners.patch_extent(layout, 0.84);
        let card = corners.patch_extent(layout, 0.5);
        let (x0, x1) = (corners.top_left[0] - card[0] * 0.2, corners.top_right[0] + card[0] * 0.2);
        let (y0, y1) = (corners.top_left[1] - card[1] * 0.2, corners.bottom_left[1] + card[1] * 0.2);
        for y in (y0.max(0.0) as usize)..(y1 as usize).min(half_height) {
            for x in (x0.max(0.0) as usize)..(x1 as usize).min(half_width) {
                half[y * half_width + x] = [0.02, 0.02, 0.02];
            }
        }
        for fr in 0..rows {
            for fc in 0..columns {
                let (r, c) = crate::reference::chart_cell(turns, fr, fc, chart_rows, chart_columns);
                let lab = chart.patches[r * chart_columns + c].lab;
                let xyz = crate::color::lab_to_xyz(Vec3::new(lab[0], lab[1], lab[2]), crate::color::D50);
                let rgb = to_rgb * xyz * 0.6;
                let centre = corners.patch_centre(layout, fr * columns + fc);
                for y in (centre[1] - extent[1]).round() as usize..(centre[1] + extent[1]).round() as usize {
                    for x in (centre[0] - extent[0]).round() as usize..(centre[0] + extent[0]).round() as usize {
                        if x < half_width && y < half_height {
                            half[y * half_width + x] = [rgb[0].max(0.001) * cast[0], rgb[1].max(0.001) * cast[1], rgb[2].max(0.001) * cast[2]];
                        }
                    }
                }
            }
        }
        let mut samples = vec![0u16; width * height];
        for y in 0..half_height {
            for x in 0..half_width {
                let [r, g, b] = half[y * half_width + x];
                let s = |v: f64| (v.clamp(0.0, 1.0) * 16383.0) as u16;
                samples[(2 * y) * width + 2 * x] = s(r);
                samples[(2 * y) * width + 2 * x + 1] = s(g);
                samples[(2 * y + 1) * width + 2 * x] = s(g);
                samples[(2 * y + 1) * width + 2 * x + 1] = s(b);
            }
        }
        crate::frame::rggb_test_frame(width, height, &samples, 16383.0)
    }

    #[test]
    fn the_chart_is_found_by_its_colours_in_every_orientation() {
        let (hw, hh) = (900usize, 600usize);
        for turns in 0..4u8 {
            // A small chart off centre, as a ColorChecker on the grass is.
            let (w, h) = if turns % 2 == 0 { (150.0, 100.0) } else { (100.0, 150.0) };
            let truth = ChartCorners::from_rect(520.0, 210.0, w, h);
            let frame = chart_on_grass(hw, hh, &truth, turns);
            let found = find_by_pattern(&frame, &COLORCHECKER_CLASSIC).expect("the chart is in the frame");
            assert_eq!(found.quarter_turns, turns, "orientation");
            assert!(found.score > 0.9, "score {}", found.score);
            for (a, b) in found.corners.points().iter().zip(truth.points().iter()) {
                let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                // Within a twentieth of a patch spacing (25 pixels).
                assert!(d < 0.05 * 25.0 + 1.0, "turns {turns}: corner {a:?} is {d:.2} px from {b:?}");
            }
        }
    }

    #[test]
    fn a_reference_read_the_wrong_way_round_is_turned_back() {
        // A reference file that only names patches by letter and number is
        // read with the letters down the rows; for this chart that is wrong,
        // and the search has to find it the other way.
        let mut misread = COLORCHECKER_CLASSIC.transposed();
        misread.reading_known = false;
        let truth = ChartCorners::from_rect(520.0, 210.0, 100.0, 150.0);
        let frame = chart_on_grass(900, 600, &truth, 1);
        assert!(find_by_pattern(&frame, &misread).is_none(), "the mirror image should not pass for the chart");
        let (found, turned) = find_by_pattern_either_way(&frame, &misread).expect("found the other way");
        let turned = turned.expect("the transposed reading won");
        assert_eq!(turned.patches, COLORCHECKER_CLASSIC.patches);
        assert_eq!(found.quarter_turns, 1);
        assert!(found.score > 0.9, "score {}", found.score);
    }

    #[test]
    fn grass_alone_is_not_a_chart() {
        // The same texture with the chart painted nowhere visible.
        let off = ChartCorners::from_rect(5000.0, 5000.0, 150.0, 100.0);
        let frame = chart_on_grass(900, 600, &off, 0);
        assert!(find_by_pattern(&frame, &COLORCHECKER_CLASSIC).is_none());
    }

    /// Paint a chart into a grey image: each cell gets its own level, on a
    /// background that has none of that structure.
    fn paint_chart(width: usize, height: usize, corners: &ChartCorners, chart: &Chart) -> Vec<Vec3> {
        let mut pixels = vec![Vec3::new(0.5, 0.5, 0.5); width * height];
        let layout = chart.layout;
        for index in 0..layout.patches() {
            let centre = corners.patch_centre(layout, index);
            let extent = corners.patch_extent(layout, 0.8);
            // A different level per patch, cycling so neighbours differ.
            let level = 0.15 + 0.7 * ((index * 7 % 11) as f64 / 10.0);
            let x0 = (centre[0] - extent[0]).max(0.0) as usize;
            let x1 = ((centre[0] + extent[0]) as usize).min(width);
            let y0 = (centre[1] - extent[1]).max(0.0) as usize;
            let y1 = ((centre[1] + extent[1]) as usize).min(height);
            for y in y0..y1 {
                for x in x0..x1 {
                    pixels[y * width + x] = Vec3::new(level, level, level);
                }
            }
        }
        pixels
    }

    #[test]
    fn a_painted_chart_is_found_where_it_was_painted() {
        let width = 180usize;
        let height = 120usize;
        let truth = ChartCorners::from_rect(40.0, 25.0, 100.0, 100.0 * 4.0 / 6.0);
        let pixels = paint_chart(width, height, &truth, &COLORCHECKER_CLASSIC);

        let found = detect(&pixels, width, &COLORCHECKER_CLASSIC, None).expect("a chart was painted");
        assert!(found.score > 0.05, "score {} should be well clear of a blank frame", found.score);
        // The corners should land within a few pixels of the truth.
        for (a, b) in found.corners.points().iter().zip(truth.points().iter()) {
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            // The sweep is coarse by design: the interface's draggable pins are
            // how a user gets the last pixel, so the search only has to land on
            // the right chart. A few percent of a chart width is close enough.
            assert!(d < 10.0, "corner {a:?} is {d:.1} from the truth {b:?}");
        }
    }

    #[test]
    fn a_blank_frame_scores_near_zero() {
        let width = 180usize;
        let height = 120usize;
        let pixels = vec![Vec3::new(0.45, 0.45, 0.45); width * height];
        // Finding nothing at all is also an acceptable answer for a blank
        // frame, and better than a confident wrong one.
        if let Some(found) = detect(&pixels, width, &COLORCHECKER_CLASSIC, None) {
            assert!(found.score < 0.02, "blank frame scored {}", found.score);
        }
        // an empty input is handled
        {
            assert!(detect(&[], 0, &COLORCHECKER_CLASSIC, None).is_none());
        }
    }

    #[test]
    fn the_score_is_scale_invariant() {
        // The same contrast on a dark chart and a bright one should score alike,
        // otherwise a dark photograph would never be detected.
        let width = 180usize;
        let height = 120usize;
        let corners = ChartCorners::from_rect(40.0, 25.0, 100.0, 66.0);
        let bright = paint_chart(width, height, &corners, &COLORCHECKER_CLASSIC);
        let dim: Vec<Vec3> = bright.iter().map(|p| Vec3::new(p.v[0] * 0.3, p.v[1] * 0.3, p.v[2] * 0.3)).collect();
        let a = detect(&bright, width, &COLORCHECKER_CLASSIC, None).expect("bright");
        let b = detect(&dim, width, &COLORCHECKER_CLASSIC, None).expect("dim");
        let ratio = a.score / b.score.max(1e-9);
        assert!((0.8..=1.25).contains(&ratio), "scores {} and {}", a.score, b.score);
    }
}
