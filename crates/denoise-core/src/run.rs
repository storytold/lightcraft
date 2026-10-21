//! Denoising a whole Bayer mosaic with a tile model: cut it into overlapping tiles, run the model on each, bring
//! each tile's scale back to the input's, keep clipped highlights as they were, and blend the tiles together.
//!
//! The model itself is a [`TileRunner`], so everything around it (packing, tiling, scale matching, highlight
//! protection, blending, cancelling, progress) is tested without one.

use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_raster::Rgb32f;
use rayon::prelude::*;

use crate::bayer::{Layout, pack_tile};
use crate::manifest::Gain;
use crate::tiles::{starts, weight};

/// Most pixels one picture may have (guards the allocations that follow from its size).
pub const MAX_PIXELS: usize = 110_000_000;
/// A tile whose mean signal is below this (white = 1) is too dark to measure its scale from.
const MIN_SIGNAL: f32 = 0.003;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Input(String),
    #[error("the model gave {0}")]
    Model(String),
    #[error("the model could not run: {0}")]
    Runtime(String),
    #[error("the model could not be loaded: {0}")]
    Load(String),
    #[error("cancelled")]
    Cancelled,
}

fn allocate<T: Clone>(n: usize, value: T) -> Result<Vec<T>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| Error::Input("not enough memory for denoise".into()))?;
    v.resize(n, value);
    Ok(v)
}

/// Something that runs the model on one tile.
pub trait TileRunner: Sync {
    /// `input` is the tile as four planes `[R, G1, G2, B]` of `tile × tile` cells (NCHW, batch 1); the answer is
    /// camera RGB, three planes of `2·tile × 2·tile` pixels (CHW), in the model's own scale.
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error>;
}

pub struct Params {
    /// Side of the model's square, in cells.
    pub tile: usize,
    /// Cells neighbouring tiles share.
    pub overlap: usize,
    pub gain: Gain,
    /// White is 1; samples at or above this (minus a little) are clipped and keep their value.
    pub clip: Option<f32>,
    /// Tiles run at once (each model call may itself be single-threaded).
    pub parallel: usize,
}

#[derive(Default)]
pub struct Control<'a> {
    pub cancel: Option<&'a AtomicBool>,
    /// `(tiles done, tiles in all)`.
    pub progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

/// The tile grid of a picture: `(starts across, starts down)`.
pub fn grid(width: usize, height: usize, layout: Layout, tile: usize, overlap: usize) -> (Vec<usize>, Vec<usize>) {
    let (nx, ny) = layout.cells(width, height);
    (starts(nx, tile, overlap), starts(ny, tile, overlap))
}

/// How many model calls [`denoise_bayer`] makes for a picture.
pub fn tile_count(width: usize, height: usize, layout: Layout, tile: usize, overlap: usize) -> usize {
    let (sx, sy) = grid(width, height, layout, tile, overlap);
    sx.len().saturating_mul(sy.len())
}

/// The tile grid of a picture for linear RGB: `(starts across, starts down)`.
pub fn grid_linear_rgb(width: usize, height: usize, tile: usize, overlap: usize) -> (Vec<usize>, Vec<usize>) {
    (starts(width, tile, overlap), starts(height, tile, overlap))
}

/// How many model calls [`denoise_linear_rgb`] makes for a picture.
pub fn tile_count_linear_rgb(width: usize, height: usize, tile: usize, overlap: usize) -> usize {
    let (sx, sy) = grid_linear_rgb(width, height, tile, overlap);
    sx.len().saturating_mul(sy.len())
}

/// Denoise (and demosaic) the normalised mosaic `mosaic` of `width × height` samples, laid out as `layout`.
/// Gives camera RGB at the mosaic's resolution.
pub fn denoise_bayer(
    mosaic: &[f32],
    width: usize,
    height: usize,
    layout: Layout,
    runner: &dyn TileRunner,
    p: &Params,
    ctl: &Control<'_>,
) -> Result<Rgb32f, Error> {
    let pixels = width
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= MAX_PIXELS)
        .ok_or_else(|| Error::Input("the picture has no size, or is too large".into()))?;
    if mosaic.len() != pixels {
        return Err(Error::Input("the mosaic does not have width × height samples".into()));
    }
    if mosaic.iter().any(|v| !v.is_finite()) {
        return Err(Error::Input("the mosaic has samples that are not finite".into()));
    }
    if !(16..=2048).contains(&p.tile) || p.overlap >= p.tile.div_ceil(2) || layout.dx > 1 || layout.dy > 1 {
        return Err(Error::Input("the tile size and overlap do not fit".into()));
    }
    if let Gain::MatchMean { nominal, max_deviation } = p.gain
        && !(nominal.is_finite() && (1e-6..=1e12).contains(&nominal) && max_deviation.is_finite() && (0.0..=0.5).contains(&max_deviation))
    {
        return Err(Error::Input("the model gain is not finite or within its supported range".into()));
    }
    if p.clip.is_some_and(|v| !v.is_finite() || v <= 0.0) {
        return Err(Error::Input("the highlight clip must be finite and positive".into()));
    }
    let (nx, ny) = layout.cells(width, height);
    let (sx, sy) = (starts(nx, p.tile, p.overlap), starts(ny, p.tile, p.overlap));
    let (cols, rows) = (sx.len(), sy.len());
    if cols == 0 || rows == 0 {
        return Err(Error::Input("the picture needs too many tiles".into()));
    }
    let tiles: Vec<(usize, usize, bool, bool, bool, bool)> = sy
        .iter()
        .enumerate()
        .flat_map(|(j, &y0)| sx.iter().enumerate().map(move |(i, &x0)| (x0, y0, i > 0, i + 1 < cols, j > 0, j + 1 < rows)))
        .collect();
    let total = tiles.len();

    let mut acc = allocate(pixels, [0f32; 3])?;
    let mut wsum = allocate(nx * ny, 0f32)?;
    let group = p.parallel.clamp(1, 4);
    let mut done = 0;
    // where the time goes, printed under `LIGHTCRAFT_PROFILE`
    let (mut t_wait, mut t_total) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let began = web_time::Instant::now();

    // One group of tiles at a time: pack, run the model, check and bring each to the input's scale, in parallel.
    // Blending (sequential: the tiles add into one picture, in tile order so the result does not depend on timing) of
    // the previous group runs while the next group is on the model, so neither the card nor the cores wait for it.
    type Tile = (usize, usize, bool, bool, bool, bool);
    let compute = |chunk: &[Tile]| -> Vec<Result<Vec<f32>, Error>> {
        chunk
            .par_iter()
            .map(|&(x0, y0, bx, ax, by, ay)| {
                let mut input = allocate(4 * p.tile * p.tile, 0f32)?;
                if !pack_tile(mosaic, width, height, layout, x0 as isize, y0 as isize, p.tile, &mut input) {
                    return Err(Error::Input("a tile could not be packed".into()));
                }
                let mut out = runner.run(&input)?;
                let side = 2 * p.tile;
                if out.len() != 3 * side * side {
                    return Err(Error::Model(format!("{} numbers for a tile, not the {} expected", out.len(), 3 * side * side)));
                }
                if out.iter().any(|v| !v.is_finite()) {
                    return Err(Error::Model("numbers that are not finite".into()));
                }
                finish_tile(&input, &mut out, p, (bx, ax, by, ay));
                if out.iter().any(|v| !v.is_finite()) {
                    return Err(Error::Model("numbers that are not finite after scale matching or highlight protection".into()));
                }
                Ok(out)
            })
            .collect()
    };
    let mut blend = |chunk: &[Tile], outs: Vec<Result<Vec<f32>, Error>>| -> Result<(), Error> {
        for (&(x0, y0, bx, ax, by, ay), r) in chunk.iter().zip(outs) {
            blend_tile(&r?, &mut acc, &mut wsum, (width, height), (nx, ny), layout, (x0, y0), p, (bx, ax, by, ay));
            done += 1;
            if let Some(f) = ctl.progress {
                f(done, total);
            }
        }
        Ok(())
    };
    let mut previous: Option<(&[Tile], Vec<Result<Vec<f32>, Error>>)> = None;
    for chunk in tiles.chunks(group) {
        if ctl.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let (outs, blended) = rayon::join(
            || compute(chunk),
            || match previous.take() {
                Some((c, o)) => {
                    let started = web_time::Instant::now();
                    let r = blend(c, o);
                    (r, started.elapsed())
                }
                None => (Ok(()), std::time::Duration::ZERO),
            },
        );
        blended.0?;
        t_wait += blended.1;
        previous = Some((chunk, outs));
    }
    if let Some((c, o)) = previous.take() {
        let started = web_time::Instant::now();
        blend(c, o)?;
        t_wait += started.elapsed();
    }
    t_total += began.elapsed();
    let started = web_time::Instant::now();
    // normalise by the weights
    let (ox, oy) = (layout.position(0, 0).0, layout.position(0, 0).1);
    acc.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let cy = ((y as isize - oy) / 2) as usize;
        for (x, px) in row.iter_mut().enumerate() {
            let cx = ((x as isize - ox) / 2) as usize;
            let w = wsum.get(cy * nx + cx).copied().unwrap_or(0.0);
            if w > 0.0 {
                *px = px.map(|v| v / w);
            }
        }
    });
    if acc.iter().flatten().any(|v| !v.is_finite()) {
        return Err(Error::Model("numbers that are not finite after tile blending".into()));
    }
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!(
            "[denoise] {total} tiles, {} at once: tiles {:.0} ms (blending, {:.0} ms of it, overlaps the model), normalise {:.0} ms",
            group,
            t_total.as_secs_f64() * 1e3,
            t_wait.as_secs_f64() * 1e3,
            started.elapsed().as_secs_f64() * 1e3
        );
    }
    Ok(Rgb32f { width, height, data: acc })
}

/// Denoise a linear camera RGB image `image` of `width × height` pixels using a tile model.
/// Gives camera RGB at the image's original resolution. Sensor-agnostic (works for X-Trans, Bayer, HDR merges, etc.).
pub fn denoise_linear_rgb(image: &Rgb32f, runner: &dyn TileRunner, p: &Params, ctl: &Control<'_>) -> Result<Rgb32f, Error> {
    let (width, height) = (image.width, image.height);
    let pixels = width
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= MAX_PIXELS)
        .ok_or_else(|| Error::Input("the picture has no size, or is too large".into()))?;
    if image.data.len() != pixels {
        return Err(Error::Input("the image does not have width × height samples".into()));
    }
    if image.data.iter().flatten().any(|v| !v.is_finite()) {
        return Err(Error::Input("the image has samples that are not finite".into()));
    }
    if !(16..=2048).contains(&p.tile) || p.overlap >= p.tile.div_ceil(2) {
        return Err(Error::Input("the tile size and overlap do not fit".into()));
    }
    if let Gain::MatchMean { nominal, max_deviation } = p.gain
        && !(nominal.is_finite() && (1e-6..=1e12).contains(&nominal.abs()) && max_deviation.is_finite() && (0.0..=0.5).contains(&max_deviation))
    {
        return Err(Error::Input("the model gain is not finite or within its supported range".into()));
    }
    if p.clip.is_some_and(|v| !v.is_finite() || v <= 0.0) {
        return Err(Error::Input("the highlight clip must be finite and positive".into()));
    }
    let (sx, sy) = (starts(width, p.tile, p.overlap), starts(height, p.tile, p.overlap));
    let (cols, rows) = (sx.len(), sy.len());
    if cols == 0 || rows == 0 {
        return Err(Error::Input("the picture needs too many tiles".into()));
    }
    let tiles: Vec<(usize, usize, bool, bool, bool, bool)> = sy
        .iter()
        .enumerate()
        .flat_map(|(j, &y0)| sx.iter().enumerate().map(move |(i, &x0)| (x0, y0, i > 0, i + 1 < cols, j > 0, j + 1 < rows)))
        .collect();
    let total = tiles.len();

    let mut acc = allocate(pixels, [0f32; 3])?;
    let mut wsum = allocate(pixels, 0f32)?;
    let group = p.parallel.clamp(1, 4);
    let mut done = 0;
    let (mut t_wait, mut t_total) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let began = web_time::Instant::now();

    type Tile = (usize, usize, bool, bool, bool, bool);
    let compute = |chunk: &[Tile]| -> Vec<Result<Vec<f32>, Error>> {
        chunk
            .par_iter()
            .map(|&(x0, y0, bx, ax, by, ay)| {
                let mut input = allocate(3 * p.tile * p.tile, 0f32)?;
                if !pack_tile_linear_rgb(image, x0 as isize, y0 as isize, p.tile, &mut input) {
                    return Err(Error::Input("a tile could not be packed".into()));
                }
                let mut out = runner.run(&input)?;
                let side = p.tile;
                if out.len() != 3 * side * side {
                    return Err(Error::Model(format!("{} numbers for a tile, not the {} expected", out.len(), 3 * side * side)));
                }
                if out.iter().any(|v| !v.is_finite()) {
                    return Err(Error::Model("numbers that are not finite".into()));
                }
                finish_tile_linear_rgb(&input, &mut out, p, (bx, ax, by, ay));
                if out.iter().any(|v| !v.is_finite()) {
                    return Err(Error::Model("numbers that are not finite after scale matching or highlight protection".into()));
                }
                Ok(out)
            })
            .collect()
    };
    let mut blend = |chunk: &[Tile], outs: Vec<Result<Vec<f32>, Error>>| -> Result<(), Error> {
        for (&(x0, y0, bx, ax, by, ay), r) in chunk.iter().zip(outs) {
            blend_tile_linear_rgb(&r?, &mut acc, &mut wsum, (width, height), (x0, y0), p, (bx, ax, by, ay));
            done += 1;
            if let Some(f) = ctl.progress {
                f(done, total);
            }
        }
        Ok(())
    };
    let mut previous: Option<(&[Tile], Vec<Result<Vec<f32>, Error>>)> = None;
    for chunk in tiles.chunks(group) {
        if ctl.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let (outs, blended) = rayon::join(
            || compute(chunk),
            || match previous.take() {
                Some((c, o)) => {
                    let started = web_time::Instant::now();
                    let r = blend(c, o);
                    (r, started.elapsed())
                }
                None => (Ok(()), std::time::Duration::ZERO),
            },
        );
        blended.0?;
        t_wait += blended.1;
        previous = Some((chunk, outs));
    }
    if let Some((c, o)) = previous.take() {
        let started = web_time::Instant::now();
        blend(c, o)?;
        t_wait += started.elapsed();
    }
    t_total += began.elapsed();
    let started = web_time::Instant::now();
    acc.par_chunks_mut(width).zip(wsum.par_chunks(width)).for_each(|(row, wrow)| {
        for (px, &w) in row.iter_mut().zip(wrow) {
            if w > 0.0 {
                *px = px.map(|v| v / w);
            }
        }
    });
    if acc.iter().flatten().any(|v| !v.is_finite()) {
        return Err(Error::Model("numbers that are not finite after tile blending".into()));
    }
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!(
            "[denoise-linear-rgb] {total} tiles, {} at once: tiles {:.0} ms (blending, {:.0} ms of it, overlaps the model), normalise {:.0} ms",
            group,
            t_total.as_secs_f64() * 1e3,
            t_wait.as_secs_f64() * 1e3,
            started.elapsed().as_secs_f64() * 1e3
        );
    }
    Ok(Rgb32f { width, height, data: acc })
}

/// Bring a tile's output to the input's scale and keep clipped highlights as they were (in place).
fn finish_tile(input: &[f32], out: &mut [f32], p: &Params, edges: (bool, bool, bool, bool)) {
    let t = p.tile;
    let side = 2 * t;
    if let Gain::MatchMean { nominal, max_deviation } = p.gain {
        // the part of the tile whose scale is measured: away from the edges that blend into a neighbour
        let m = p.overlap / 2;
        let (i0, i1) = (if edges.0 { m } else { 0 }, t - if edges.1 { m } else { 0 });
        let (j0, j1) = (if edges.2 { m } else { 0 }, t - if edges.3 { m } else { 0 });
        let cells = ((i1 - i0) * (j1 - j0)) as f64;
        let plane_sum = |pl: usize| -> f64 {
            (j0..j1)
                .map(|j| input.get(pl * t * t + j * t + i0..pl * t * t + j * t + i1).map_or(0.0, |r| r.iter().map(|&v| f64::from(v)).sum::<f64>()))
                .sum()
        };
        let in_mean = [plane_sum(0) / cells, (plane_sum(1) + plane_sum(2)) / (2.0 * cells), plane_sum(3) / cells];
        for (c, &mean_in) in in_mean.iter().enumerate() {
            let sum_out: f64 = (2 * j0..2 * j1)
                .map(|y| {
                    out.get(c * side * side + y * side + 2 * i0..c * side * side + y * side + 2 * i1)
                        .map_or(0.0, |r| r.iter().map(|&v| f64::from(v)).sum::<f64>())
                })
                .sum();
            let mean_out = sum_out / (4.0 * cells);
            let measured = if mean_in as f32 >= MIN_SIGNAL && mean_out > 0.0 { (mean_out / mean_in) as f32 } else { nominal };
            let g = measured.clamp(nominal * (1.0 - max_deviation), nominal * (1.0 + max_deviation));
            if g.is_finite() && g > 0.0 {
                let inv = 1.0 / g;
                if let Some(plane) = out.get_mut(c * side * side..(c + 1) * side * side) {
                    plane.iter_mut().for_each(|v| *v *= inv);
                }
            }
        }
    }
    if let Some(clip) = p.clip {
        protect_clipped(input, out, t, clip);
    }
}

/// Where the mosaic is at (or within a cell of) white, the output becomes the plain cell values again, so a clipped
/// area stays exactly as clipped as the camera recorded it (highlight recovery later depends on that).
fn protect_clipped(input: &[f32], out: &mut [f32], t: usize, clip: f32) {
    let side = 2 * t;
    let plane = t * t;
    let (Some(r), Some(g1), Some(g2), Some(b)) =
        (input.get(0..plane), input.get(plane..2 * plane), input.get(2 * plane..3 * plane), input.get(3 * plane..4 * plane))
    else {
        return;
    };
    let mut m: Vec<f32> = (0..plane).map(|i| r[i].max(g1[i]).max(g2[i]).max(b[i])).collect();
    if !m.iter().any(|&v| v >= clip) {
        return;
    }
    // 3×3 maximum over cells (separable)
    let dilate = |src: &[f32], horizontal: bool| -> Vec<f32> {
        (0..plane)
            .map(|i| {
                let (x, y) = (i % t, i / t);
                let mut best = src[i];
                for d in [-1isize, 1] {
                    let (nx, ny) = if horizontal { (x as isize + d, y as isize) } else { (x as isize, y as isize + d) };
                    if nx >= 0 && ny >= 0 && (nx as usize) < t && (ny as usize) < t {
                        best = best.max(src[ny as usize * t + nx as usize]);
                    }
                }
                best
            })
            .collect()
    };
    m = dilate(&dilate(&m, true), false);
    let lo = clip - 0.03;
    for j in 0..t {
        for i in 0..t {
            let k = j * t + i;
            let s = ((m[k] - lo) / (clip - lo)).clamp(0.0, 1.0);
            if s <= 0.0 {
                continue;
            }
            let s = s * s * (3.0 - 2.0 * s);
            let plain = [r[k], 0.5 * (g1[k] + g2[k]), b[k]];
            for (c, &pv) in plain.iter().enumerate() {
                for (sy, sx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    if let Some(v) = out.get_mut(c * side * side + (2 * j + sy) * side + 2 * i + sx) {
                        *v += (pv - *v) * s;
                    }
                }
            }
        }
    }
}

/// Add a finished tile to the picture with its blend weights.
#[allow(clippy::too_many_arguments)]
fn blend_tile(
    out: &[f32],
    acc: &mut [[f32; 3]],
    wsum: &mut [f32],
    size: (usize, usize),
    cells: (usize, usize),
    layout: Layout,
    at: (usize, usize),
    p: &Params,
    edges: (bool, bool, bool, bool),
) {
    let (width, height) = size;
    let (nx, ny) = cells;
    let (t, side) = (p.tile, 2 * p.tile);
    let fade = p.overlap;
    let wx: Vec<f32> = (0..t).map(|i| weight(i, t, fade, edges.0, edges.1)).collect();
    let wy: Vec<f32> = (0..t).map(|j| weight(j, t, fade, edges.2, edges.3)).collect();
    for j in 0..t {
        let cy = at.1 + j;
        for i in 0..t {
            let cx = at.0 + i;
            // cells past the picture's edge (a picture smaller than one tile is padded) belong to no pixel
            if cx >= nx || cy >= ny {
                continue;
            }
            let (Some(&a), Some(&b)) = (wx.get(i), wy.get(j)) else { continue };
            let w = a * b;
            if let Some(s) = wsum.get_mut(cy * nx + cx) {
                *s += w;
            }
            let (px0, py0) = layout.position(cx as isize, cy as isize);
            for (sy, sx) in [(0isize, 0isize), (0, 1), (1, 0), (1, 1)] {
                let (x, y) = (px0 + sx, py0 + sy);
                if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
                    continue;
                }
                let o = (2 * j + sy as usize) * side + 2 * i + sx as usize;
                let (Some(&r), Some(&g), Some(&bl)) = (out.get(o), out.get(side * side + o), out.get(2 * side * side + o)) else { continue };
                if let Some(px) = acc.get_mut(y as usize * width + x as usize) {
                    px[0] += w * r;
                    px[1] += w * g;
                    px[2] += w * bl;
                }
            }
        }
    }
}

/// Pack the `tile × tile` window whose top-left pixel is `(x0, y0)` into `out` as three planes `[R, G, B]`
/// (`3 · tile²` values, NCHW). Pixels outside the picture are mirrored ping-pong without repeating edges.
/// Returns `false` (and writes nothing) when the sizes do not fit.
pub fn pack_tile_linear_rgb(image: &Rgb32f, x0: isize, y0: isize, tile: usize, out: &mut [f32]) -> bool {
    let Some(plane) = tile.checked_mul(tile) else { return false };
    let Some(values) = plane.checked_mul(3) else { return false };
    if image.width == 0 || image.height == 0 || tile == 0 || image.width.checked_mul(image.height) != Some(image.data.len()) || out.len() != values {
        return false;
    }
    let Ok(last) = isize::try_from(tile.saturating_sub(1)) else { return false };
    if x0.checked_add(last).is_none() || y0.checked_add(last).is_none() {
        return false;
    }
    let (r_plane, rest) = out.split_at_mut(plane);
    let (g_plane, b_plane) = rest.split_at_mut(plane);
    for j in 0..tile {
        let y = crate::bayer::reflect(y0.saturating_add(j as isize), image.height);
        let Some(row) = image.data.get(y * image.width..(y + 1) * image.width) else { return false };
        let Some(r_row) = r_plane.get_mut(j * tile..(j + 1) * tile) else { return false };
        let Some(g_row) = g_plane.get_mut(j * tile..(j + 1) * tile) else { return false };
        let Some(b_row) = b_plane.get_mut(j * tile..(j + 1) * tile) else { return false };
        for i in 0..tile {
            let x = crate::bayer::reflect(x0.saturating_add(i as isize), image.width);
            let Some(px) = row.get(x) else { return false };
            if let (Some(r), Some(g), Some(b)) = (r_row.get_mut(i), g_row.get_mut(i), b_row.get_mut(i)) {
                *r = px[0];
                *g = px[1];
                *b = px[2];
            }
        }
    }
    true
}

/// Bring a linear RGB tile's output to the input's scale and keep clipped highlights as they were (in place).
///
/// In linear RGB, gain matching computes a single scalar across all three colour planes combined,
/// preserving strict RGB chromaticity ratios (hue and saturation) across tile borders.
fn finish_tile_linear_rgb(input: &[f32], out: &mut [f32], p: &Params, edges: (bool, bool, bool, bool)) {
    let t = p.tile;
    if let Gain::MatchMean { nominal, max_deviation } = p.gain {
        let m = p.overlap / 2;
        let (i0, i1) = (if edges.0 { m } else { 0 }, t - if edges.1 { m } else { 0 });
        let (j0, j1) = (if edges.2 { m } else { 0 }, t - if edges.3 { m } else { 0 });
        if i1 > i0 && j1 > j0 {
            let cells = ((i1 - i0) * (j1 - j0)) as f64;
            let mut in_sum = 0.0f64;
            let mut out_sum = 0.0f64;
            for c in 0..3 {
                let plane_offset = c * t * t;
                in_sum += (j0..j1)
                    .map(|j| {
                        input.get(plane_offset + j * t + i0..plane_offset + j * t + i1).map_or(0.0, |r| r.iter().map(|&v| f64::from(v)).sum::<f64>())
                    })
                    .sum::<f64>();
                out_sum += (j0..j1)
                    .map(|j| {
                        out.get(plane_offset + j * t + i0..plane_offset + j * t + i1).map_or(0.0, |r| r.iter().map(|&v| f64::from(v)).sum::<f64>())
                    })
                    .sum::<f64>();
            }
            let total_cells = cells * 3.0;
            let mean_in = in_sum / total_cells;
            let mean_out = out_sum / total_cells;

            let measured = if mean_in as f32 >= MIN_SIGNAL && mean_out.abs() > 0.0 { (mean_out / mean_in) as f32 } else { nominal };
            let (min_g, max_g) = if nominal >= 0.0 {
                (nominal * (1.0 - max_deviation), nominal * (1.0 + max_deviation))
            } else {
                (nominal * (1.0 + max_deviation), nominal * (1.0 - max_deviation))
            };
            let g = measured.clamp(min_g, max_g);
            if g.is_finite() && g != 0.0 {
                let inv = 1.0 / g;
                out.iter_mut().for_each(|v| *v *= inv);
            }
        }
    }
    if let Some(clip) = p.clip {
        protect_clipped_linear_rgb(input, out, t, clip);
    }
}

/// Where the linear RGB picture is at (or within a pixel of) white, the output becomes the plain pixel values again, so a clipped
/// area stays exactly as clipped as the camera recorded it (highlight recovery later depends on that).
fn protect_clipped_linear_rgb(input: &[f32], out: &mut [f32], t: usize, clip: f32) {
    let plane = t * t;
    let (Some(r), Some(g), Some(b)) = (input.get(0..plane), input.get(plane..2 * plane), input.get(2 * plane..3 * plane)) else {
        return;
    };
    let mut m: Vec<f32> = (0..plane)
        .map(|i| {
            let rv = r.get(i).copied().unwrap_or(0.0);
            let gv = g.get(i).copied().unwrap_or(0.0);
            let bv = b.get(i).copied().unwrap_or(0.0);
            rv.max(gv).max(bv)
        })
        .collect();
    if !m.iter().any(|&v| v >= clip) {
        return;
    }
    // 3×3 maximum over pixels (separable)
    let dilate = |src: &[f32], horizontal: bool| -> Vec<f32> {
        (0..plane)
            .map(|i| {
                let (x, y) = (i % t, i / t);
                let mut best = src.get(i).copied().unwrap_or(0.0);
                for d in [-1isize, 1] {
                    let (nx, ny) = if horizontal { (x as isize + d, y as isize) } else { (x as isize, y as isize + d) };
                    if nx >= 0
                        && ny >= 0
                        && (nx as usize) < t
                        && (ny as usize) < t
                        && let Some(&v) = src.get(ny as usize * t + nx as usize)
                    {
                        best = best.max(v);
                    }
                }
                best
            })
            .collect()
    };
    m = dilate(&dilate(&m, true), false);
    let lo = clip - 0.03;
    for j in 0..t {
        for i in 0..t {
            let k = j * t + i;
            let mk = m.get(k).copied().unwrap_or(0.0);
            let s = ((mk - lo) / (clip - lo)).clamp(0.0, 1.0);
            if s <= 0.0 {
                continue;
            }
            let s = s * s * (3.0 - 2.0 * s);
            let plain = [r.get(k).copied().unwrap_or(0.0), g.get(k).copied().unwrap_or(0.0), b.get(k).copied().unwrap_or(0.0)];
            for (c, &pv) in plain.iter().enumerate() {
                if let Some(v) = out.get_mut(c * plane + k) {
                    *v += (pv - *v) * s;
                }
            }
        }
    }
}

/// Add a finished linear RGB tile to the picture with its blend weights.
fn blend_tile_linear_rgb(
    out: &[f32],
    acc: &mut [[f32; 3]],
    wsum: &mut [f32],
    size: (usize, usize),
    at: (usize, usize),
    p: &Params,
    edges: (bool, bool, bool, bool),
) {
    let (width, height) = size;
    let t = p.tile;
    let plane = t * t;
    let fade = p.overlap;
    let wx: Vec<f32> = (0..t).map(|i| weight(i, t, fade, edges.0, edges.1)).collect();
    let wy: Vec<f32> = (0..t).map(|j| weight(j, t, fade, edges.2, edges.3)).collect();
    for j in 0..t {
        let y = at.1 + j;
        if y >= height {
            continue;
        }
        let Some(&b) = wy.get(j) else { continue };
        for i in 0..t {
            let x = at.0 + i;
            if x >= width {
                continue;
            }
            let Some(&a) = wx.get(i) else { continue };
            let w = a * b;
            let pixel_idx = y * width + x;
            if let Some(s) = wsum.get_mut(pixel_idx) {
                *s += w;
            }
            let tile_idx = j * t + i;
            let (Some(&r), Some(&g), Some(&bl)) = (out.get(tile_idx), out.get(plane + tile_idx), out.get(2 * plane + tile_idx)) else {
                continue;
            };
            if let Some(px) = acc.get_mut(pixel_idx) {
                px[0] += w * r;
                px[1] += w * g;
                px[2] += w * bl;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in model: the cell's samples as an RGB picture at twice the resolution (a plain 2 × 2 "demosaic"),
    /// times `scale`, so the output has a scale of its own like the real model's.
    struct Mock {
        tile: usize,
        scale: f32,
    }
    impl TileRunner for Mock {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
            let (t, side) = (self.tile, 2 * self.tile);
            let plane = t * t;
            let mut out = vec![0.0; 3 * side * side];
            for j in 0..t {
                for i in 0..t {
                    let k = j * t + i;
                    let rgb = [input[k], 0.5 * (input[plane + k] + input[2 * plane + k]), input[3 * plane + k]];
                    for (c, v) in rgb.iter().enumerate() {
                        for (sy, sx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                            out[c * side * side + (2 * j + sy) * side + 2 * i + sx] = v * self.scale;
                        }
                    }
                }
            }
            Ok(out)
        }
    }

    fn params(tile: usize, overlap: usize, gain: Gain) -> Params {
        Params { tile, overlap, gain, clip: None, parallel: 2 }
    }

    #[test]
    fn nonfinite_mosaics_are_rejected_before_inference() {
        struct Counting(std::sync::atomic::AtomicUsize);
        impl TileRunner for Counting {
            fn run(&self, _: &[f32]) -> Result<Vec<f32>, Error> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(vec![0.0; 3 * 32 * 32])
            }
        }
        let model = Counting(std::sync::atomic::AtomicUsize::new(0));
        let layout = Layout { dx: 0, dy: 0 };
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(denoise_bayer(&[sample], 1, 1, layout, &model, &params(16, 0, Gain::None), &Control::default()), Err(Error::Input(_))));
        }
        assert_eq!(model.0.load(Ordering::Relaxed), 0);
        // Finite scene-linear samples can be negative or exceed SDR white.
        for sample in [-0.25, 2.0] {
            assert!(denoise_bayer(&[sample], 1, 1, layout, &model, &params(16, 0, Gain::None), &Control::default()).is_ok());
        }
    }

    #[test]
    fn finite_model_output_that_overflows_during_gain_matching_is_rejected() {
        struct Extreme;
        impl TileRunner for Extreme {
            fn run(&self, _: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(vec![f32::MAX; 3 * 32 * 32])
            }
        }
        let p = params(16, 0, Gain::MatchMean { nominal: 1e-6, max_deviation: 0.0 });
        assert!(matches!(denoise_bayer(&[0.5], 1, 1, Layout { dx: 0, dy: 0 }, &Extreme, &p, &Control::default()), Err(Error::Model(_))));
    }

    #[test]
    fn overlapping_finite_tiles_that_overflow_the_accumulator_are_rejected() {
        struct Extreme;
        impl TileRunner for Extreme {
            fn run(&self, _: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(vec![f32::MAX; 3 * 32 * 32])
            }
        }
        assert!(matches!(
            denoise_bayer(&vec![0.5; 48 * 48], 48, 48, Layout { dx: 0, dy: 0 }, &Extreme, &params(16, 4, Gain::None), &Control::default()),
            Err(Error::Model(_))
        ));
    }

    #[test]
    fn overflowing_tiles_and_nan_gain_are_errors_before_the_model_runs() {
        let layout = Layout::from_cell([0, 1, 1, 2]).unwrap();
        let model = Mock { tile: 16, scale: 1.0 };
        for (tile, overlap, gain) in [
            (usize::MAX, 0, Gain::None),
            (16, usize::MAX, Gain::None),
            (16, 0, Gain::MatchMean { nominal: 1.0, max_deviation: f32::NAN }),
            (16, 0, Gain::MatchMean { nominal: f32::INFINITY, max_deviation: 0.1 }),
        ] {
            assert!(matches!(denoise_bayer(&[0.5], 1, 1, layout, &model, &params(tile, overlap, gain), &Control::default()), Err(Error::Input(_))));
        }
    }

    /// A smooth picture whose colour at a sample is `base[colour] · (1 + shading)`.
    fn mosaic(w: usize, h: usize, cell: [u8; 4]) -> Vec<f32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let c = cell[(y % 2) * 2 + x % 2] as usize;
                [0.5, 0.3, 0.2][c] * (0.6 + 0.4 * ((x as f32 / 23.0).sin() * (y as f32 / 31.0).cos()))
            })
            .collect()
    }

    fn reference(m: &[f32], w: usize, h: usize, layout: Layout) -> Rgb32f {
        // what the mock gives for the whole picture in one tile
        let (nx, ny) = layout.cells(w, h);
        let t = nx.max(ny).div_ceil(16) * 16;
        denoise_bayer(m, w, h, layout, &Mock { tile: t, scale: 1.0 }, &params(t, 16, Gain::None), &Control::default()).unwrap()
    }

    #[test]
    fn tiles_blend_to_the_same_picture_as_one_tile_for_every_layout() {
        for cell in [[0u8, 1, 1, 2], [1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]] {
            let layout = Layout::from_cell(cell).unwrap();
            let (w, h) = (150usize, 110usize);
            let m = mosaic(w, h, cell);
            let want = reference(&m, w, h, layout);
            let got = denoise_bayer(&m, w, h, layout, &Mock { tile: 32, scale: 1.0 }, &params(32, 8, Gain::None), &Control::default()).unwrap();
            assert_eq!((got.width, got.height), (w, h));
            for (a, b) in got.data.iter().zip(&want.data) {
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-5, "layout {cell:?}: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_picture_does_not_depend_on_how_many_tiles_run_at_once_or_how_long_each_takes() {
        // tiles that finish in a different order every time: blending still adds them in tile order
        struct Jittery(Mock);
        impl TileRunner for Jittery {
            fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
                let wait = (input.iter().take(64).sum::<f32>() * 1000.0) as u64 % 7;
                std::thread::sleep(std::time::Duration::from_millis(wait));
                self.0.run(input)
            }
        }
        let layout = Layout { dx: 0, dy: 1 };
        let (w, h) = (150usize, 110usize);
        let m = mosaic(w, h, [1, 0, 2, 1]);
        let run = |parallel: usize| {
            let mut p = params(32, 8, Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 });
            p.parallel = parallel;
            denoise_bayer(&m, w, h, layout, &Jittery(Mock { tile: 32, scale: 1.01e6 }), &p, &Control::default()).unwrap()
        };
        let one = run(1);
        for parallel in [2, 3, 8] {
            assert_eq!(run(parallel).data, one.data, "{parallel} at once");
        }
    }

    #[test]
    fn a_failing_tile_or_a_cancel_ends_the_picture_without_a_panic() {
        struct Fails(Mock, std::sync::atomic::AtomicUsize);
        impl TileRunner for Fails {
            fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
                if self.1.fetch_add(1, Ordering::Relaxed) == 4 {
                    return Err(Error::Runtime("boom".into()));
                }
                self.0.run(input)
            }
        }
        let layout = Layout { dx: 0, dy: 0 };
        let (w, h) = (150usize, 110usize);
        let m = mosaic(w, h, [0, 1, 1, 2]);
        let r = denoise_bayer(
            &m,
            w,
            h,
            layout,
            &Fails(Mock { tile: 32, scale: 1.0 }, Default::default()),
            &params(32, 8, Gain::None),
            &Control::default(),
        );
        assert!(matches!(r, Err(Error::Runtime(_))), "{r:?}");
        let stop = AtomicBool::new(true);
        let ctl = Control { cancel: Some(&stop), progress: None };
        let r = denoise_bayer(&m, w, h, layout, &Mock { tile: 32, scale: 1.0 }, &params(32, 8, Gain::None), &ctl);
        assert!(matches!(r, Err(Error::Cancelled)));
    }

    #[test]
    fn a_scale_of_its_own_is_taken_out_tile_by_tile() {
        let layout = Layout { dx: 0, dy: 0 };
        let (w, h) = (150usize, 110usize);
        let m = mosaic(w, h, [0, 1, 1, 2]);
        let want = reference(&m, w, h, layout);
        let gain = Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 };
        // the scale differs a little from nominal; each tile's own scale is measured and removed
        let got = denoise_bayer(&m, w, h, layout, &Mock { tile: 32, scale: 1.02e6 }, &params(32, 8, gain), &Control::default()).unwrap();
        let mean = |img: &Rgb32f, c: usize| img.data.iter().map(|p| f64::from(p[c])).sum::<f64>() / img.data.len() as f64;
        for c in 0..3 {
            let (a, b) = (mean(&got, c), mean(&want, c));
            assert!((a - b).abs() / b < 0.01, "channel {c}: {a} vs {b}");
        }
        // a scale far from nominal is clamped rather than trusted
        let wild = denoise_bayer(&m, w, h, layout, &Mock { tile: 32, scale: 3.0e6 }, &params(32, 8, gain), &Control::default()).unwrap();
        assert!(mean(&wild, 1) > 2.0 * mean(&want, 1));
    }

    #[test]
    fn a_picture_smaller_than_a_tile_is_padded_by_mirroring() {
        let layout = Layout { dx: 1, dy: 1 };
        let (w, h) = (20usize, 14usize);
        let m = mosaic(w, h, [2, 1, 1, 0]);
        let got = denoise_bayer(&m, w, h, layout, &Mock { tile: 64, scale: 1.0 }, &params(64, 16, Gain::None), &Control::default()).unwrap();
        assert_eq!((got.width, got.height), (w, h));
        assert!(got.data.iter().all(|p| p.iter().all(|v| v.is_finite() && *v > 0.0)));
    }

    #[test]
    fn clipped_highlights_stay_clipped() {
        // a model that darkens everything by 10 % would pull a clipped plateau below white
        struct Dim(Mock);
        impl TileRunner for Dim {
            fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(self.0.run(input)?.into_iter().map(|v| v * 0.9).collect())
            }
        }
        let layout = Layout { dx: 0, dy: 0 };
        let (w, h) = (96usize, 96usize);
        let m: Vec<f32> = (0..w * h).map(|i| if i % w > 40 && i % w < 70 { 1.0 } else { 0.4 }).collect();
        let mut p = params(32, 8, Gain::None);
        p.clip = Some(0.99);
        let got = denoise_bayer(&m, w, h, layout, &Dim(Mock { tile: 32, scale: 1.0 }), &p, &Control::default()).unwrap();
        let mid = got.data[48 * w + 55];
        assert!(mid.iter().all(|&v| v > 0.995), "plateau {mid:?}");
        let dark = got.data[48 * w + 10];
        assert!(dark.iter().all(|&v| (v - 0.36).abs() < 0.01), "unclipped area still goes through the model: {dark:?}");
    }

    #[test]
    fn cancelling_and_progress_work() {
        let layout = Layout { dx: 0, dy: 0 };
        let (w, h) = (150usize, 110usize);
        let m = mosaic(w, h, [0, 1, 1, 2]);
        let cancel = AtomicBool::new(true);
        let r = denoise_bayer(
            &m,
            w,
            h,
            layout,
            &Mock { tile: 32, scale: 1.0 },
            &params(32, 8, Gain::None),
            &Control { cancel: Some(&cancel), progress: None },
        );
        assert!(matches!(r, Err(Error::Cancelled)));
        let seen = std::sync::Mutex::new(Vec::new());
        let f = |d: usize, n: usize| seen.lock().unwrap().push((d, n));
        denoise_bayer(&m, w, h, layout, &Mock { tile: 32, scale: 1.0 }, &params(32, 8, Gain::None), &Control { cancel: None, progress: Some(&f) })
            .unwrap();
        let seen = seen.into_inner().unwrap();
        let total = tile_count(w, h, layout, 32, 8);
        assert!(total > 1);
        assert_eq!(seen.last(), Some(&(total, total)));
        assert!(seen.windows(2).all(|p| p[1].0 == p[0].0 + 1));
    }

    #[test]
    fn hostile_inputs_are_errors() {
        let layout = Layout { dx: 0, dy: 0 };
        let mock = Mock { tile: 32, scale: 1.0 };
        let p = params(32, 8, Gain::None);
        assert!(matches!(denoise_bayer(&[], 0, 0, layout, &mock, &p, &Control::default()), Err(Error::Input(_))));
        assert!(matches!(denoise_bayer(&[0.0; 10], 4, 4, layout, &mock, &p, &Control::default()), Err(Error::Input(_))));
        assert!(matches!(denoise_bayer(&[0.0; 16], 4, 4, layout, &mock, &params(32, 16, Gain::None), &Control::default()), Err(Error::Input(_))));
        struct Bad(Vec<f32>);
        impl TileRunner for Bad {
            fn run(&self, _: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(self.0.clone())
            }
        }
        assert!(matches!(denoise_bayer(&[0.5; 16], 4, 4, layout, &Bad(vec![0.0; 5]), &p, &Control::default()), Err(Error::Model(_))));
        assert!(matches!(denoise_bayer(&[0.5; 16], 4, 4, layout, &Bad(vec![f32::NAN; 3 * 64 * 64]), &p, &Control::default()), Err(Error::Model(_))));
    }

    #[test]
    fn accepts_102_mp_inputs_and_rejects_inputs_above_the_limit() {
        let layout = Layout { dx: 0, dy: 0 };
        let model = Mock { tile: 32, scale: 1.0 };
        let p = params(32, 8, Gain::None);
        assert!(matches!(
            denoise_bayer(&[], 11_648, 8_736, layout, &model, &p, &Control::default()),
            Err(Error::Input(message)) if message == "the mosaic does not have width × height samples"
        ));
        assert!(matches!(
            denoise_bayer(&[], MAX_PIXELS + 1, 1, layout, &model, &p, &Control::default()),
            Err(Error::Input(message)) if message == "the picture has no size, or is too large"
        ));
    }

    struct MockLinearRgb {
        scale: f32,
    }
    impl TileRunner for MockLinearRgb {
        /// Linear RGB mock runner multiplying all input samples by a constant scale factor.
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
            Ok(input.iter().map(|&v| v * self.scale).collect())
        }
    }

    /// Procedurally generated synthetic linear RGB test image with non-trivial spatial color variation.
    fn rgb_test_image(w: usize, h: usize) -> Rgb32f {
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                [
                    0.5 * (0.6 + 0.4 * ((x as f32 / 17.0).sin() * (y as f32 / 29.0).cos())),
                    0.3 * (0.6 + 0.4 * ((x as f32 / 23.0).cos() * (y as f32 / 19.0).sin())),
                    0.2 * (0.6 + 0.4 * ((x as f32 / 31.0).sin() * (y as f32 / 13.0).sin())),
                ]
            })
            .collect();
        Rgb32f { width: w, height: h, data }
    }

    /// Verifies that an identity model reconstructs exact linear RGB input images across various dimensions.
    #[test]
    fn linear_rgb_identity_runner_reconstructs_input_for_various_dimensions() {
        let model = MockLinearRgb { scale: 1.0 };
        for (w, h, tile, overlap) in [
            (150, 110, 32, 8),
            (64, 64, 32, 8),
            (20, 14, 64, 16), // smaller than tile: padded by reflection
            (97, 103, 32, 8), // prime dimensions
        ] {
            let img = rgb_test_image(w, h);
            let got = denoise_linear_rgb(&img, &model, &params(tile, overlap, Gain::None), &Control::default()).unwrap();
            assert_eq!((got.width, got.height), (w, h));
            for (i, (a, b)) in got.data.iter().zip(&img.data).enumerate() {
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-5, "dimension ({w}, {h}) at pixel {i} channel {c}: got {} vs want {}", a[c], b[c]);
                }
            }
        }
    }

    /// Verifies that model scale deviations are normalized tile-by-tile via gain matching.
    #[test]
    fn linear_rgb_scale_of_its_own_is_taken_out_tile_by_tile() {
        let (w, h) = (150usize, 110usize);
        let img = rgb_test_image(w, h);
        let gain = Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 };
        let got = denoise_linear_rgb(&img, &MockLinearRgb { scale: 1.02e6 }, &params(32, 8, gain), &Control::default()).unwrap();
        let mean = |im: &Rgb32f, c: usize| im.data.iter().map(|p| f64::from(p[c])).sum::<f64>() / im.data.len() as f64;
        for c in 0..3 {
            let (a, b) = (mean(&got, c), mean(&img, c));
            assert!((a - b).abs() / b < 0.01, "channel {c}: {a} vs {b}");
        }
        // a scale far from nominal is clamped rather than trusted
        let wild = denoise_linear_rgb(&img, &MockLinearRgb { scale: 3.0e6 }, &params(32, 8, gain), &Control::default()).unwrap();
        assert!(mean(&wild, 1) > 2.0 * mean(&img, 1));
    }

    /// Verifies that clipped highlights in linear RGB remain clipped as recorded.
    #[test]
    fn linear_rgb_clipped_highlights_stay_clipped() {
        struct DimLinear(MockLinearRgb);
        impl TileRunner for DimLinear {
            /// Dimming runner implementation for highlight clipping preservation tests.
            fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(self.0.run(input)?.into_iter().map(|v| v * 0.9).collect())
            }
        }
        let (w, h) = (96usize, 96usize);
        let data: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let x = i % w;
                let val = if x > 40 && x < 70 { 1.0 } else { 0.4 };
                [val, val, val]
            })
            .collect();
        let img = Rgb32f { width: w, height: h, data };
        let mut p = params(32, 8, Gain::None);
        p.clip = Some(0.99);
        let got = denoise_linear_rgb(&img, &DimLinear(MockLinearRgb { scale: 1.0 }), &p, &Control::default()).unwrap();
        let mid = got.data[48 * w + 55];
        assert!(mid.iter().all(|&v| v > 0.995), "clipped plateau preserved: {mid:?}");
        let dark = got.data[48 * w + 10];
        assert!(dark.iter().all(|&v| (v - 0.36).abs() < 0.01), "unclipped area dimmed: {dark:?}");
    }

    /// Verifies cancellation and tile progress reporting for linear RGB denoising.
    #[test]
    fn linear_rgb_cancellation_and_progress_work() {
        let (w, h) = (150usize, 110usize);
        let img = rgb_test_image(w, h);
        let cancel = AtomicBool::new(true);
        let r =
            denoise_linear_rgb(&img, &MockLinearRgb { scale: 1.0 }, &params(32, 8, Gain::None), &Control { cancel: Some(&cancel), progress: None });
        assert!(matches!(r, Err(Error::Cancelled)));

        let seen = std::sync::Mutex::new(Vec::new());
        let f = |d: usize, n: usize| seen.lock().unwrap().push((d, n));
        denoise_linear_rgb(&img, &MockLinearRgb { scale: 1.0 }, &params(32, 8, Gain::None), &Control { cancel: None, progress: Some(&f) }).unwrap();
        let seen = seen.into_inner().unwrap();
        let total = tile_count_linear_rgb(w, h, 32, 8);
        assert!(total > 1);
        assert_eq!(seen.last(), Some(&(total, total)));
        assert!(seen.windows(2).all(|p| p[1].0 == p[0].0 + 1));
    }

    /// Tests validation of hostile and non-finite inputs to denoise_linear_rgb.
    #[test]
    fn linear_rgb_hostile_and_nonfinite_inputs_are_errors() {
        let mock = MockLinearRgb { scale: 1.0 };
        let p = params(32, 8, Gain::None);
        let empty = Rgb32f { width: 0, height: 0, data: vec![] };
        assert!(matches!(denoise_linear_rgb(&empty, &mock, &p, &Control::default()), Err(Error::Input(_))));
        let bad_size = Rgb32f { width: 4, height: 4, data: vec![[0.0; 3]; 10] };
        assert!(matches!(denoise_linear_rgb(&bad_size, &mock, &p, &Control::default()), Err(Error::Input(_))));

        // Non-finite pixel rejected
        let nan_img = Rgb32f { width: 4, height: 4, data: vec![[f32::NAN, 0.0, 0.0]; 16] };
        assert!(matches!(denoise_linear_rgb(&nan_img, &mock, &p, &Control::default()), Err(Error::Input(_))));

        // Model returning wrong tile size or non-finite output
        struct Bad(Vec<f32>);
        impl TileRunner for Bad {
            /// Runner returning predetermined faulty responses for testing validation logic.
            fn run(&self, _: &[f32]) -> Result<Vec<f32>, Error> {
                Ok(self.0.clone())
            }
        }
        let good_img = Rgb32f { width: 4, height: 4, data: vec![[0.5; 3]; 16] };
        assert!(matches!(denoise_linear_rgb(&good_img, &Bad(vec![0.0; 5]), &p, &Control::default()), Err(Error::Model(_))));
        assert!(matches!(denoise_linear_rgb(&good_img, &Bad(vec![f32::NAN; 3 * 32 * 32]), &p, &Control::default()), Err(Error::Model(_))));
    }

    /// Tests linear RGB grid computation and tile count calculation.
    #[test]
    fn linear_rgb_grid_and_tile_counts() {
        let (sx, sy) = grid_linear_rgb(100, 80, 32, 8);
        assert_eq!(sx[0], 0);
        assert_eq!(*sx.last().unwrap(), 100 - 32);
        assert_eq!(sy[0], 0);
        assert_eq!(*sy.last().unwrap(), 80 - 32);
        assert_eq!(tile_count_linear_rgb(100, 80, 32, 8), sx.len() * sy.len());
    }

    /// Verifies that global scalar gain matching in linear RGB preserves inter-channel color ratios.
    #[test]
    fn linear_rgb_gain_matching_preserves_color_ratios() {
        let t = 16;
        let plane = t * t;
        let mut input = vec![0.0f32; 3 * plane];
        let mut out = vec![0.0f32; 3 * plane];
        for i in 0..plane {
            input[i] = 0.2;
            input[plane + i] = 0.4;
            input[2 * plane + i] = 0.8;
            // Model produced output scaled by 2.0
            out[i] = 0.4;
            out[plane + i] = 0.8;
            out[2 * plane + i] = 1.6;
        }
        let p = Params { tile: t, overlap: 4, gain: Gain::MatchMean { nominal: 2.0, max_deviation: 0.5 }, clip: None, parallel: 1 };
        finish_tile_linear_rgb(&input, &mut out, &p, (true, true, true, true));
        for i in 0..plane {
            let (r, g, b) = (out[i], out[plane + i], out[2 * plane + i]);
            assert!((r - 0.2).abs() < 1e-5, "r was {r}, expected ~0.2");
            assert!((g - 0.4).abs() < 1e-5, "g was {g}, expected ~0.4");
            assert!((b - 0.8).abs() < 1e-5, "b was {b}, expected ~0.8");
            assert!((g / r - 2.0).abs() < 1e-5, "G/R ratio must be 2.0");
            assert!((b / r - 4.0).abs() < 1e-5, "B/R ratio must be 4.0");
        }
    }
}
