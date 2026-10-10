//! Clip-aware highlight handling on demosaiced camera RGB (before white balance, white level = 1.0).
//!
//! - [`clip_neutral`]: white-balance-aware clipping so that sensor-clipped areas render neutral instead of
//!   magenta/cyan (each channel limited to the smallest white-balanced clip level).
//! - [`reconstruct`] / [`reconstruct_with`]: where only some channels are clipped, rebuild them from the
//!   unclipped channels using the chromaticity of reliable unclipped content nearby (bright, away from edges and
//!   from the clipped pixels themselves; diffused into the clipped region coarse to fine) unless both the pixel's
//!   own channels and the clipped surface's colour just below its clip (the rim) contradict it, and neutral where
//!   there is none within reach. Right past the rim, the rim's colour carries on. Fully clipped pixels become
//!   neutral at the brightest plausible level; partly clipped ones fade into it on their way from the first channel
//!   clipping to the last, pale ones from the start.
//!
//! Why the care: the white-balance gains push clipped channels apart (green clips first, red and blue keep
//! rising), so whatever colour the rebuilt pixels carry is hidden at Neutral only by the tone map's roll-off to
//! white. Any darkening (Highlights, negative Exposure or Whites) brings it out. Three things used to leak colour
//! in: the chromaticity of whatever unclipped pixel happened to be nearest (dark foliage, purple and green
//! fringes along branches, pixels demosaiced from clipped neighbours); a colour model that does not render the
//! camera's white-balanced neutral as neutral (a look fitted to the camera's JPEG, a camera profile); and, in a
//! pale partly clipped sky, the colour of another reliable surface (bright foliage) and the sky's own faint
//! colour, which darkening and a camera profile turned teal.

use lightcraft_raster::Rgb32f;
use rayon::prelude::*;

/// Clip every channel of `wb ⊙ img` at `min_c(wb_c) · clip` — i.e. at the lowest channel's clip level after WB —
/// then divide the multipliers back out. `clip` is the sensor clip in normalised units (≈ 1.0).
pub fn clip_neutral(img: &mut Rgb32f, wb: [f32; 3], clip: f32) {
    let limit = wb.iter().cloned().fold(f32::MAX, f32::min) * clip;
    for p in &mut img.data {
        for c in 0..3 {
            p[c] = (p[c] * wb[c]).min(limit) / wb[c];
        }
    }
}

/// Unclipped pixels lend their chromaticity in proportion to how close they come to clipping (their largest
/// channel, relative to the clip level): from nothing below `SOURCE_LO` to fully above `SOURCE_HI`. Dark content
/// (foliage, branches, buildings against a clipped sky) says nothing about the sky's colour.
const SOURCE_LO: f32 = 0.25;
const SOURCE_HI: f32 = 0.6;
/// Sources on a strong edge (a 3 × 3 half-resolution neighbour brighter or darker by this many stops) are
/// left out, fading from `EDGE_LO` to `EDGE_HI`: edge pixels mix two surfaces and carry lens fringes.
const EDGE_LO: f32 = 0.15;
const EDGE_HI: f32 = 0.4;
/// Source cells within this many half-resolution cells of a clipped pixel are left out: their channels were
/// demosaiced partly from clipped samples. (At least 1: the same neighbourhood tells which cells clipped
/// pixels sample.)
const MARGIN: usize = 1;
/// Weight of the coarser estimate against a cell's own sources when the pyramid is pushed back down (in units
/// of source density: a cell half covered by full-weight sources counts 0.5).
const PRIOR: f32 = 0.01;
/// The rebuilt colour fades to neutral where reliable content covers less than `SUPPORT` of the surroundings at
/// every scale up to `REACH` of the long edge (so previews and exports agree), gradually, as the coverage of the
/// widest one falls: a clipped sky between bare branches is better white than the colour of a river far away.
const REACH: f32 = 1.0 / 4.0;
const SUPPORT: f32 = 0.05;
/// An unclipped channel above `FLOOR_LO` of its clip level counts more and more as the clip floor it is about to
/// become (see [`reconstruct_with`]).
const FLOOR_LO: f32 = 0.75;
/// A clipped pixel takes its estimated colour (reliable colour around, or the neutral guess) only as far as it is
/// brighter (or otherwise unlike) the unclipped pixels nearest its clip: the "rim", unclipped pixels above `RIM_LO`
/// of the clip level, weighted towards the clip, the clipped surface's own colour just below it. Up to `NEAR_RIM`
/// stops past the rim (the rim is an average, a little below the clip) it is rebuilt with the rim's colour, and
/// over the next `DEPTH` stops it goes over to the estimate, so a surface runs into clipping without a step
/// whatever its colour and whatever lent the estimate; well past it, or where the rim is another surface (its
/// colour differs; a clipped channel differs by at least as much as it lies above the rim's), the estimate applies.
const RIM_LO: f32 = 0.85;
const NEAR_RIM: f32 = 0.03;
const DEPTH: f32 = 0.2;
/// Reliable colour counts less and less as both the pixel's own channels (see [`contradiction`]) and the rim's
/// colour contradict it, by `AGREE.0` to `AGREE.1` stops: it is another surface's, like bright foliage below a pale
/// sky, whose green would turn the sky teal. (Either alone can be wrong: a pixel's own channels are noisy and
/// can't speak for the channels that clipped, and the rim can be another surface, like a white window frame
/// around a bright view.)
const AGREE: (f32, f32) = (0.15, 0.4);
/// On the way from the first channel clipping to the last, a partly clipped pixel's brightness goes over to the
/// clipped neutral's in the second half, and so does the colour of a strongly coloured one (saturation relative to
/// the white above `PALE.1`). A pale one's colour (below `PALE.0`) goes over from the start: the few per cent of
/// colour left in a pale sky once a channel clipped is as much the estimate's error as the sky's, and darkening
/// shows it.
const PALE: (f32, f32) = (0.2, 0.4);

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One level of a pyramid, `w × h` cells of `N - 1` values and a weight. Pulled: per cell the weighted sums of
/// the values and the weight, as densities (per pixel of the cell). Pushed: the estimated values, and still the
/// weight. The chromaticity pyramid holds red and blue chromaticity, the rim pyramid white-balanced RGB.
struct Level<const N: usize> {
    w: usize,
    h: usize,
    cells: Vec<[f32; N]>,
}

/// Bilinear sample of a level at the centre of pixel `(x, y)` of a grid `scale` times finer.
#[inline]
fn sample<const N: usize>(l: &Level<N>, scale: f32, x: usize, y: usize) -> [f32; N] {
    let (cv, cw, ch) = (&l.cells, l.w, l.h);
    let fx = ((x as f32 + 0.5) / scale - 0.5).clamp(0.0, cw.saturating_sub(1) as f32);
    let fy = ((y as f32 + 0.5) / scale - 0.5).clamp(0.0, ch.saturating_sub(1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(cw.saturating_sub(1)), (y0 + 1).min(ch.saturating_sub(1)));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let g = |xx: usize, yy: usize| cv.get(yy * cw + xx).copied().unwrap_or([0.0; N]);
    let (a, b, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        top + (bot - top) * ty
    })
}

/// The half-resolution source level of `img`: every `2 × 2` block's source-weighted chromaticity sums, with
/// blocks near clipped pixels or on strong edges left out. Also returns, per cell, whether a clipped pixel lies
/// in its row within [`MARGIN`] cells (the vertical half of that test is left to the readers).
fn sources(img: &Rgb32f, wb: [f32; 3], clip: f32) -> (Level<3>, Vec<bool>) {
    let (w, h) = (img.width, img.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    // per cell: [red sum, blue sum, weight] densities, luminance, whether it holds a clipped pixel
    let mut cells = vec![[0f32; 3]; cw * ch];
    let mut lum = vec![0f32; cw * ch];
    let mut near = vec![false; cw * ch];
    cells.par_chunks_mut(cw).zip(lum.par_chunks_mut(cw)).zip(near.par_chunks_mut(cw)).enumerate().for_each(|(y, ((crow, lrow), nrow))| {
        let mut touched = vec![false; cw];
        let row = |r: usize| img.data.get(r * w..(r + 1) * w).unwrap_or(&[]);
        let (r0, r1) = (row(2 * y), row(2 * y + 1));
        let lo = SOURCE_LO * clip;
        for x in 0..cw {
            let (mut a, mut sum, mut n, mut hit) = ([0f32; 3], 0f32, 0usize, false);
            let span = 2 * x..(2 * x + 2).min(w);
            for p in r0.get(span.clone()).unwrap_or(&[]).iter().chain(r1.get(span).unwrap_or(&[])) {
                let q = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                let s = q[0].max(0.0) + q[1].max(0.0) + q[2].max(0.0);
                sum += s;
                n += 1;
                let top = p[0].max(p[1]).max(p[2]);
                if top >= clip {
                    hit = true;
                    continue;
                }
                if top > lo && s > 1e-4 {
                    let b = smoothstep(SOURCE_LO, SOURCE_HI, top / clip) / s;
                    a[0] += b * q[0].max(0.0);
                    a[1] += b * q[2].max(0.0);
                    a[2] += b * s;
                }
            }
            let k = 1.0 / n.max(1) as f32;
            crow[x] = a.map(|v| v * k);
            lrow[x] = (sum * k).max(1e-6);
            touched[x] = hit;
        }
        for (x, nr) in nrow.iter_mut().enumerate() {
            *nr = touched.get(x.saturating_sub(MARGIN)..=(x + MARGIN).min(cw - 1)).is_some_and(|t| t.iter().any(|&t| t));
        }
    });
    // leave out cells near clipped pixels and on strong edges (luminance ratios to the neighbours)
    let (lum, near_ref) = (&lum, &near);
    let (edge_lo, edge_hi) = (EDGE_LO.exp2(), EDGE_HI.exp2());
    cells.par_chunks_mut(cw).enumerate().for_each(|(y, row)| {
        let (ya, yb) = (y.saturating_sub(MARGIN), (y + MARGIN).min(ch - 1));
        let (ea, eb) = (y.saturating_sub(1), (y + 1).min(ch - 1));
        for (x, cell) in row.iter_mut().enumerate() {
            if cell[2] <= 0.0 {
                continue;
            }
            if (ya..=yb).any(|yy| near_ref.get(yy * cw + x).copied().unwrap_or(false)) {
                *cell = [0.0; 3];
                continue;
            }
            let l = lum.get(y * cw + x).copied().unwrap_or(1.0);
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(cw - 1));
            let (mut lo, mut hi) = (l, l);
            for yy in ea..=eb {
                for &v in lum.get(yy * cw + xa..=yy * cw + xb).unwrap_or(&[]) {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let keep = 1.0 - smoothstep(edge_lo, edge_hi, (hi / l).max(l / lo));
            *cell = cell.map(|v| v * keep);
        }
    });
    (Level { w: cw, h: ch, cells }, near)
}

/// The quarter-resolution rim level of `img`: every `4 × 4` block's white-balanced RGB of its unclipped pixels
/// above [`RIM_LO`] of the clip level, weighted (steeply) towards the clip, as densities. Pixels next to clipped
/// ones (`margin`, by half-resolution cell) are left out, like the colour sources: they were demosaiced partly from
/// clipped samples, and lens fringes along clipped content would pass for its colour.
fn rim(img: &Rgb32f, wb: [f32; 3], clip: f32, margin: &(dyn Fn(usize, usize) -> bool + Sync)) -> Level<4> {
    let (w, h) = (img.width, img.height);
    let (cw, ch) = (w.div_ceil(4), h.div_ceil(4));
    let mut cells = vec![[0f32; 4]; cw * ch];
    cells.par_chunks_mut(cw).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.iter_mut().enumerate() {
            let (mut a, mut n) = ([0f32; 4], 0usize);
            for sy in 4 * y..(4 * y + 4).min(h) {
                for (i, p) in img.data.get(sy * w + 4 * x..sy * w + (4 * x + 4).min(w)).unwrap_or(&[]).iter().enumerate() {
                    n += 1;
                    let top = p[0].max(p[1]).max(p[2]) / clip;
                    if !(RIM_LO..1.0).contains(&top) || margin((4 * x + i) / 2, sy / 2) {
                        continue;
                    }
                    let k = ((top - RIM_LO) / (1.0 - RIM_LO)).powi(4);
                    for c in 0..3 {
                        a[c] += k * p[c] * wb[c];
                    }
                    a[3] += k;
                }
            }
            *cell = a.map(|v| v / n.max(1) as f32);
        }
    });
    Level { w: cw, h: ch, cells }
}

/// The next coarser level: each `2 × 2` block's mean densities.
fn pull<const N: usize>(l: &Level<N>) -> Level<N> {
    let (w, h) = (l.w.div_ceil(2), l.h.div_ceil(2));
    let mut cells = vec![[0f32; N]; w * h];
    cells.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.iter_mut().enumerate() {
            let (mut s, mut n) = ([0f32; N], 0usize);
            for (sx, sy) in [(2 * x, 2 * y), (2 * x + 1, 2 * y), (2 * x, 2 * y + 1), (2 * x + 1, 2 * y + 1)] {
                if let Some(c) = (sx < l.w && sy < l.h).then(|| l.cells.get(sy * l.w + sx)).flatten() {
                    for k in 0..N {
                        s[k] += c[k];
                    }
                    n += 1;
                }
            }
            *cell = s.map(|v| v / n.max(1) as f32);
        }
    });
    Level { w, h, cells }
}

/// The estimated values of the cells of `levels[0]` that `needed` marks (all of them when `None`), and of every
/// cell of the coarser levels: each level's own sources blended with the next coarser estimate. The top level is
/// its own sources' weighted mean (normalised: however sparse they are in a large empty picture, the empty part
/// never pulls the estimate anywhere), and `empty` (at zero weight) when it has none at all.
fn push<const N: usize>(levels: &mut [Level<N>], empty: [f32; N], needed: Option<&(dyn Fn(usize, usize) -> bool + Sync)>) {
    let estimate = |cell: [f32; N], prior: [f32; N]| -> [f32; N] {
        let weight = cell[N - 1];
        let d = weight + PRIOR;
        std::array::from_fn(|k| if k + 1 == N { weight } else { (cell[k] + PRIOR * prior[k]) / d })
    };
    let Some(last) = levels.last_mut() else { return };
    last.cells.iter_mut().for_each(|c| {
        let weight = c[N - 1];
        *c = if weight > 0.0 { std::array::from_fn(|k| if k + 1 == N { weight } else { c[k] / weight }) } else { empty };
    });
    for i in (0..levels.len().saturating_sub(1)).rev() {
        let (fine, coarse) = levels.split_at_mut(i + 1);
        let (Some(fine), Some(coarse)) = (fine.last_mut(), coarse.first()) else { continue };
        let w = fine.w;
        let needed = needed.filter(|_| i == 0);
        fine.cells.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, cell) in row.iter_mut().enumerate() {
                if needed.is_some_and(|f| !f(x, y)) {
                    continue;
                }
                *cell = estimate(*cell, sample(coarse, 2.0, x, y));
            }
        });
    }
}

/// How many stops two chromaticities differ by in any channel ratio.
fn unlike(a: [f32; 3], b: [f32; 3]) -> f32 {
    let ratio = |i: usize, j: usize| (a[i].max(1e-6) * b[j].max(1e-6)) / (a[j].max(1e-6) * b[i].max(1e-6));
    [(0, 1), (0, 2), (1, 2)].iter().map(|&(i, j)| ratio(i, j).log2().abs()).fold(0.0f32, f32::max)
}

/// How far channel `c` of a pixel (sensor values `p`, clipped channels `cl`) only bounds its true value from below:
/// fully once clipped, and increasingly from `FLOOR_LO` of the clip on, so that nothing compared with it jumps where
/// it clips.
fn bound(p: [f32; 3], cl: [bool; 3], clip: f32, c: usize) -> f32 {
    if cl[c] { 1.0 } else { smoothstep(FLOOR_LO, 1.0, p[c] / clip) }
}

/// How many stops the channels of a partly clipped pixel (sensor values `p`, white-balanced `q`, clipped channels
/// `cl`) contradict chromaticity `r`: how far the levels they imply spread, where a clipped channel's only bounds the
/// level from below (see [`bound`]). 0 for the pixel's own colour, whatever its level.
fn contradiction(p: [f32; 3], q: [f32; 3], cl: [bool; 3], r: [f32; 3], clip: f32) -> f32 {
    let (mut lo, mut hi) = (f32::INFINITY, 0f32);
    for c in 0..3 {
        let level = q[c] / r[c].max(1e-3);
        hi = hi.max(level);
        let w = bound(p, cl, clip, c);
        if r[c] > 1e-3 && w < 1.0 {
            lo = lo.min(level / (1.0 - w));
        }
    }
    if hi > 0.0 && lo > 0.0 && lo.is_finite() { (hi / lo).log2().max(0.0) } else { 0.0 }
}

/// The white-balanced colour of a partly clipped pixel (sensor values `p`, white-balanced `q`, clipped channels
/// `cl`) whose colour is estimated at chromaticity `r`: clipped channels rebuilt, unclipped ones kept. `None`
/// when no unclipped channel can tell the level.
fn rebuild(p: [f32; 3], q: [f32; 3], cl: [bool; 3], r: [f32; 3], clip: f32, max_level: f32) -> Option<[f32; 3]> {
    // The white-balanced channel sum the unclipped channels imply: their mean, except that a channel about to clip
    // counts more and more as the floor it is about to become (continuous with the neighbours where it has).
    let (mut sum, mut near_clip, mut k) = (0f32, 0f32, 0usize);
    for c in 0..3 {
        if !cl[c] && r[c] > 1e-3 {
            let s = q[c] / r[c];
            sum += s;
            near_clip = near_clip.max(s * smoothstep(FLOOR_LO, 1.0, p[c] / clip));
            k += 1;
        }
    }
    if k == 0 {
        return None;
    }
    // Two clipped channels must not be left at their clip levels: those differ by the white-balance gains (green
    // clips first), and the pair would show as magenta or red. They keep the estimate's ratio instead, at the level
    // that lifts each to its clip (no channel above the clipped neutral), and so does a channel about to clip (the
    // same floor, so nothing jumps where it clips).
    let floor = (0..3).filter(|&c| cl[c]).map(|c| q[c] / r[c].max(1e-3)).fold(0.0f32, f32::max);
    let top = r[0].max(r[1]).max(r[2]).max(1e-3);
    let sum = (sum / k as f32).max(near_clip.max(floor).min(max_level / top));
    Some(std::array::from_fn(|c| if cl[c] { q[c].max(sum * r[c]) } else { q[c] }))
}

/// Reconstruct partially clipped channels, with a neutral that is neutral in camera RGB. See
/// [`reconstruct_with`].
pub fn reconstruct(img: &mut Rgb32f, wb: [f32; 3], clip: f32) -> usize {
    reconstruct_with(img, wb, clip, [1.0; 3])
}

/// Reconstruct partially clipped channels. `clip` is the sensor clip level in normalised units (use slightly
/// below 1.0, e.g. 0.99), `wb` the white-balance multipliers that will be applied afterwards, `white` the
/// white-balanced camera RGB that the colour model applied afterwards renders neutral (`[1, 1, 1]` for a model
/// that maps the white-balanced neutral to neutral), scaled to the brightness that `[1, 1, 1]` renders at.
/// Fully clipped pixels become `white` times the brightest plausible level; partly clipped ones take the
/// chromaticity of reliable unclipped content nearby, and that of `white` where there is none.
/// Returns the number of pixels that had at least one clipped channel; the others are left exactly as they were.
pub fn reconstruct_with(img: &mut Rgb32f, wb: [f32; 3], clip: f32, white: [f32; 3]) -> usize {
    let (w, h) = (img.width, img.height);
    let is_clipped = |p: &[f32; 3]| p[0] >= clip || p[1] >= clip || p[2] >= clip;
    let count = img.data.par_iter().filter(|p| is_clipped(p)).count();
    if count == 0 || w == 0 || h == 0 {
        return 0;
    }
    // a white far from the camera's neutral (or not a number) is a broken colour model: keep the camera's
    let plausible = white.iter().all(|v| v.is_finite() && (1.0 / 16.0..=16.0).contains(v));
    let white = if plausible { white } else { [1.0; 3] };
    let total = white[0] + white[1] + white[2];
    let neutral = [white[0] / total, white[1] / total, white[2] / total];
    // A clipped pixel is never a source, so its chromaticity is always the bilinear sample of the
    // half-resolution estimate: the pyramid starts there, straight from the image.
    let (first, near) = sources(img, wb, clip);
    let mut levels = vec![first];
    while let Some(l) = levels.last().filter(|l| l.w > 1 || l.h > 1) {
        let next = pull(l);
        levels.push(next);
    }
    // a clipped pixel samples the cells within one of its own, which are within one of a clipped cell
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let needed = |x: usize, y: usize| (y.saturating_sub(1)..=(y + 1).min(ch - 1)).any(|yy| near.get(yy * cw + x).copied().unwrap_or(false));
    push(&mut levels, [neutral[0], neutral[2], 0.0], Some(&needed));
    // How much reliable content lies around each needed cell: its density at any scale up to [`REACH`] (level
    // i's cells span 2^i cells of level 0). Near reliable content some fine level is dense with it; far from it
    // only the widest one may still hold a little, and the trust fades with that. Kept in the third entry of the
    // level 0 estimates, which clipped pixels sample with their colour.
    let reach = REACH * w.max(h) as f32 / 2.0;
    let widest = (0..levels.len()).find(|&i| (1u64 << i.min(62)) as f32 >= reach).unwrap_or(levels.len().saturating_sub(1));
    if let Some((first, coarser)) = levels.split_first_mut() {
        let support: Vec<(&Level<3>, f32)> = coarser.iter().take(widest).enumerate().map(|(i, l)| (l, (1u64 << (i + 1).min(62)) as f32)).collect();
        let fw = first.w;
        first.cells.par_chunks_mut(fw).enumerate().for_each(|(y, row)| {
            for (x, cell) in row.iter_mut().enumerate() {
                if !needed(x, y) {
                    continue;
                }
                let mut t = 0f32;
                for &(l, scale) in &support {
                    t = t.max(smoothstep(0.0, SUPPORT, sample(l, scale, x, y)[2]));
                    if t >= 1.0 {
                        break;
                    }
                }
                cell[2] = t;
            }
        });
    }
    let Some(field) = levels.first() else { return count };
    // the rim (level 0 of its pyramid holds the estimate), if there is one anywhere at all
    let rims = {
        let mut levels = vec![rim(img, wb, clip, &needed)];
        while let Some(l) = levels.last().filter(|l| l.w > 1 || l.h > 1) {
            let next = pull(l);
            levels.push(next);
        }
        push(&mut levels, [0.0; 4], None);
        Some(levels).filter(|levels| levels.last().is_some_and(|top| top.cells.iter().any(|c| c[3] > 0.0)))
    };
    let rim_field = rims.as_ref().and_then(|l| l.first());
    let max_level = wb.iter().cloned().fold(0.0f32, f32::max) * clip;
    img.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let p = *px;
            let cl = [p[0] >= clip, p[1] >= clip, p[2] >= clip];
            if !cl.iter().any(|&b| b) {
                continue;
            }
            let q = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
            // rebuilt with the colour of the reliable content around, and with neutral, blended by how much of it
            // there is and how well the pixel agrees with it (blending the results, not the colours, keeps the
            // estimate consistent with the pixel's own unclipped channels all the way)
            let e = sample(field, 2.0, x, y);
            let borrowed = [e[0], 1.0 - e[0] - e[1], e[1]];
            let edge = rim_field.map(|rim| sample(rim, 4.0, x, y));
            let mut trust = e[2].clamp(0.0, 1.0);
            if trust > 0.0 {
                let mut off = contradiction(p, q, cl, borrowed, clip);
                if off > AGREE.0 {
                    off = off.min(edge.map_or(0.0, |edge| {
                        let s = edge[0] + edge[1] + edge[2];
                        if s > 1e-6 { unlike([edge[0] / s, edge[1] / s, edge[2] / s], borrowed) } else { 0.0 }
                    }));
                }
                trust *= 1.0 - smoothstep(AGREE.0, AGREE.1, off);
            }
            // either applies only as far as the pixel lies past the rim nearest its clip (in any channel); nearer, the
            // pixel is rebuilt with the rim's colour
            let at_rim = edge.and_then(|edge| {
                // stops between pixel and rim in each channel; a clipped channel's true value lies at least as far
                // above the rim's as its clip, and no further below (see [`bound`])
                let ratio = (0..3)
                    .map(|c| {
                        let (a, b) = (q[c].max(1e-12), edge[c].max(1e-12));
                        if a >= b {
                            return a / b;
                        }
                        match bound(p, cl, clip, c) {
                            w if w <= 0.0 => b / a,
                            w if w >= 1.0 => 1.0,
                            w => (b / a).powf(1.0 - w),
                        }
                    })
                    .fold(1.0f32, f32::max);
                let past = ratio.log2().min(64.0);
                let k = smoothstep(NEAR_RIM, NEAR_RIM + DEPTH, past);
                let s = edge[0] + edge[1] + edge[2];
                (k < 1.0 && s > 1e-6).then(|| rebuild(p, q, cl, [edge[0] / s, edge[1] / s, edge[2] / s], clip, max_level)).flatten().map(|a| (a, k))
            });
            let past_rim = |b: [f32; 3]| -> [f32; 3] {
                let Some((a, k)) = at_rim else { return b };
                std::array::from_fn(|c| a[c] + (b[c] - a[c]) * k)
            };
            let local = (trust > 0.0).then(|| rebuild(p, q, cl, borrowed, clip, max_level)).flatten().map(past_rim);
            let plain = (trust < 1.0).then(|| rebuild(p, q, cl, neutral, clip, max_level)).flatten().map(past_rim);
            let mut out = match (local, plain) {
                (Some(a), Some(b)) => std::array::from_fn(|c| b[c] + (a[c] - b[c]) * trust),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => {
                    let v = q.iter().cloned().fold(0.0f32, f32::max).max(max_level);
                    white.map(|c| c * v)
                }
            };
            // Fade into the fully clipped neutral on the way from the first channel clipping (no change: continuous
            // with the unclipped neighbours) to the last one clipping (all of it: continuous with the fully clipped
            // ones). The way is measured in stops: how far the rebuilt clipped channels lie above their clip,
            // against how far the lowest unclipped channel still lies below its own.
            let above = (0..3).filter(|&c| cl[c]).map(|c| (out[c] / q[c].max(1e-9)).max(1.0).log2()).fold(0.0f32, f32::max);
            let lowest = (0..3).filter(|&c| !cl[c]).map(|c| p[c]).fold(f32::MAX, f32::min);
            let below = (clip / lowest.max(1e-6)).log2().max(0.0);
            let way = if above > 0.0 && lowest < clip { above / (above + below) } else { 0.0 };
            // The brightness goes over in the second half of the way, and so does the colour of a strongly coloured
            // pixel; a pale one's goes over from the start ([`PALE`]).
            let u = [out[0] / white[0], out[1] / white[1], out[2] / white[2]];
            let (lo, hi) = (u[0].min(u[1]).min(u[2]), u[0].max(u[1]).max(u[2]));
            let saturation = if hi > 0.0 && hi.is_finite() { 1.0 - lo.max(0.0) / hi } else { 1.0 };
            let start = 0.5 * smoothstep(PALE.0, PALE.1, saturation);
            let (t_level, t_colour) = (smoothstep(0.5, 1.0, way), smoothstep(start, start + 0.5, way));
            if t_colour > 0.0 {
                let level = (out[0] + out[1] + out[2]) / total;
                let full = q.iter().cloned().fold(0.0f32, f32::max).max(max_level);
                let v = level + (full - level) * t_level;
                out = std::array::from_fn(|c| out[c] + (white[c] * v - out[c]) * t_colour);
            }
            *px = [out[0] / wb[0], out[1] / wb[1], out[2] / wb[2]];
        }
    });
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Daylight-like multipliers: green clips first, red and blue keep rising.
    const WB: [f32; 3] = [2.5, 1.0, 1.6];

    fn balanced(p: [f32; 3]) -> [f32; 3] {
        [p[0] * WB[0], p[1] * WB[1], p[2] * WB[2]]
    }

    fn sensor(q: [f32; 3]) -> [f32; 3] {
        [q[0] / WB[0], q[1] / WB[1], q[2] / WB[2]].map(|v| v.min(1.0))
    }

    fn chroma(q: [f32; 3]) -> [f32; 3] {
        let s = q[0] + q[1] + q[2];
        q.map(|v| v / s)
    }

    /// A pale blue sky whose green channel clipped, an unclipped strip of the same sky along the top, and a dark
    /// branch with purple fringes on both sides standing in the clipped part (white-balanced sky colour returned).
    fn branch_scene() -> (Rgb32f, [f32; 3]) {
        let sky = [1.10, 1.25, 1.45];
        let img = Rgb32f::from_fn(192, 128, |x, y| {
            sensor(if y < 24 {
                sky.map(|v| v * 0.7)
            } else if (90..100).contains(&x) && y >= 40 {
                [0.06, 0.05, 0.03]
            } else if (x == 89 || x == 100) && y >= 40 {
                [0.75, 0.25, 0.96]
            } else {
                sky
            })
        });
        (img, sky)
    }

    /// How far chromaticity `got` lies off the way from `from` to neutral (largest channel difference to the
    /// nearest point of that way).
    fn off_the_way_to_neutral(got: [f32; 3], from: [f32; 3]) -> f32 {
        let d: [f32; 3] = std::array::from_fn(|c| 1.0 / 3.0 - from[c]);
        let len = d.iter().map(|v| v * v).sum::<f32>();
        let t = if len > 0.0 { ((0..3).map(|c| (got[c] - from[c]) * d[c]).sum::<f32>() / len).clamp(0.0, 1.0) } else { 0.0 };
        (0..3).map(|c| (got[c] - (from[c] + d[c] * t)).abs()).fold(0.0, f32::max)
    }

    /// Issue #523: the clipped sky beside a branch took the colour of the branch's fringes and of the branch
    /// itself (magenta, green), which any darkening of the highlights then showed. Beside the branch it is now the
    /// clipped sky away from it: the sky's colour, on its way to neutral.
    #[test]
    fn fringes_and_dark_branches_do_not_tint_a_clipped_sky() {
        let (mut img, sky) = branch_scene();
        assert!(reconstruct(&mut img, WB, 0.99) > 0);
        let mut worst = 0f32;
        for y in 40..128 {
            let away = chroma(balanced(img.get(30, y)));
            assert!(away == chroma(balanced(img.get(170, y))), "row {y}");
            let off = off_the_way_to_neutral(away, chroma(sky));
            assert!(off < 0.005, "row {y}: the clipped sky {away:?} is not the sky's colour on its way to neutral ({off})");
            for x in [84, 86, 88, 101, 103, 105] {
                let got = chroma(balanced(img.get(x, y)));
                worst = worst.max((0..3).map(|c| (got[c] - away[c]).abs()).fold(0.0, f32::max));
            }
        }
        assert!(worst < 0.01, "clipped sky beside the branch is off the sky away from it by {worst}");
    }

    /// A warm light with red and green clipped: left at their clip levels the pair reads red-magenta (the white
    /// balance multiplies red's clip level by 2.5, green's by 1).
    #[test]
    fn two_clipped_channels_are_not_left_at_their_clip_levels() {
        let light = [3.5, 2.6, 0.8];
        let mut img = Rgb32f::from_fn(64, 64, |x, y| {
            let d = ((x as f32 - 32.0).powi(2) + (y as f32 - 32.0).powi(2)).sqrt();
            if d < 20.0 { sensor(light) } else { [0.02; 3] }
        });
        reconstruct(&mut img, WB, 0.99);
        let q = balanced(img.get(32, 32));
        assert!((q[2] - 0.8).abs() < 0.1, "the unclipped channel is about kept: {q:?}");
        let ratio = q[1] / q[0];
        assert!((0.6..=1.05).contains(&ratio), "green / red {ratio} ({q:?}): not a warm white");
    }

    #[test]
    fn fully_clipped_pixels_follow_the_colour_model_white() {
        let white = [1.03, 0.98, 1.06];
        let mut img = Rgb32f::filled(8, 8, [1.0; 3]);
        img.data[0] = [0.3, 0.6, 0.4];
        reconstruct_with(&mut img, WB, 0.99, white);
        let q = balanced(img.get(4, 4));
        let k = q[1] / white[1];
        assert!((0..3).all(|c| (q[c] - white[c] * k).abs() < 1e-4 * k), "{q:?} is not along {white:?}");
        assert!(k >= 2.5 * 0.99 - 1e-4, "clipped neutral at least as bright as the highest clip level: {k}");
        assert_eq!(img.data[0], [0.3, 0.6, 0.4]);
        // a white that cannot be right falls back to camera neutral, and nothing overflows
        for bad in [[f32::NAN, 1.0, 1.0], [2.5e38, 0.74, 0.74], [1.0, 0.0, 1.0], [40.0, 1.0, 1.0]] {
            let mut img = Rgb32f::filled(4, 4, [1.0; 3]);
            img.data[0] = [1.0, 1.0, 0.5];
            reconstruct_with(&mut img, WB, 0.99, bad);
            let q = balanced(img.get(1, 1));
            assert!((q[0] - q[1]).abs() < 1e-4 && (q[2] - q[1]).abs() < 1e-4, "{bad:?}: {q:?}");
            assert!(img.data.iter().flatten().all(|v| v.is_finite()), "{bad:?}");
        }
    }

    /// Red rises through its clip level across a sky whose green and blue clipped: the rebuilt colour changes
    /// smoothly (no edge where red clips too) and arrives at the clipped neutral.
    #[test]
    fn rebuilt_colour_is_continuous_as_more_channels_clip() {
        let blue = [1.0, 1.3, 1.9];
        let img0 = Rgb32f::from_fn(240, 40, |x, y| {
            if y < 8 {
                return sensor(blue.map(|v| v * 0.5)); // unclipped sky: the source of colour
            }
            let level = 0.6 + 2.2 * x as f32 / 239.0; // red at 0.24 to 1.12 of its clip level
            sensor([level * WB[0] * 0.4, 1.6, 2.0])
        });
        let mut img = img0.clone();
        reconstruct(&mut img, WB, 0.99);
        let row: Vec<[f32; 3]> = (0..240).map(|x| chroma(balanced(img.get(x, 30)))).collect();
        let step = row.windows(2).map(|w| (0..3).map(|c| (w[1][c] - w[0][c]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
        assert!(step < 0.01, "largest chromaticity step between neighbours {step}");
        let end = row[239];
        assert!(end.iter().all(|v| (v - 1.0 / 3.0).abs() < 1e-3), "fully clipped end {end:?}");
    }

    /// A smooth pink ramp (`[0.9k, k, 0.8k]`, k from 0.8 to 1.35) whose green clips first, then red, then blue,
    /// in `rows` of a `h`-row picture that is otherwise near black: the rebuilt middle row, and where green first
    /// clips on it.
    fn rebuilt_ramp(rows: std::ops::Range<usize>, h: usize) -> (Vec<[f32; 3]>, usize) {
        let (w, k0, k1) = (1600usize, 0.8f32, 1.35f32);
        let k = |x: usize| k0 + (k1 - k0) * x as f32 / (w - 1) as f32;
        let img0 = Rgb32f::from_fn(w, h, |x, y| if rows.contains(&y) { [0.9 * k(x), k(x), 0.8 * k(x)].map(|v| v.min(1.0)) } else { [0.01; 3] });
        let mut img = img0.clone();
        reconstruct(&mut img, WB, 0.99);
        let y = (rows.start + rows.end) / 2;
        let row: Vec<[f32; 3]> = (0..w).map(|x| balanced(img.get(x, y))).collect();
        (row, (0..w).find(|&x| img0.get(x, y)[1] >= 0.99).unwrap_or(0))
    }

    /// No channel changes by more than `at_clip` (relative) where green first clips, nor by 3 % anywhere.
    fn assert_continuous(row: &[[f32; 3]], first: usize, at_clip: f32, case: &str) {
        // across the first clip the colour carries on as it was (the sensor's own colour, not a fade)
        let (a, b) = (row[first - 1], row[first]);
        assert!((0..3).all(|c| (b[c] / a[c] - 1.0).abs() < at_clip), "{case}, first clip: {a:?} then {b:?}");
        let worst = row.windows(2).map(|p| (0..3).map(|c| (p[1][c] / p[0][c] - 1.0).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
        // (a smooth ramp moves 0.04 % per pixel here; the rebuilt colour turns faster where it leaves the colour of
        // the unclipped part for neutral, but never by a step)
        assert!(worst < 0.03, "{case}: largest relative step between neighbours {worst}");
    }

    /// The rebuilt colour has no step anywhere along the ramp: not where green first clips (the neutral fade starts
    /// from nothing there), nor where red does, nor where the last channel clips (the fade ends at the fully
    /// clipped neutral).
    #[test]
    fn rebuilt_ramp_is_continuous_from_the_first_clip_to_the_last() {
        let (row, first) = rebuilt_ramp(0..32, 32);
        assert_continuous(&row, first, 0.002, "full ramp");
    }

    /// The same ramp as a thin line on near black has no reliable colour around (every pixel of it is on a strong
    /// edge), so its clipped part falls back to the neutral guess. That guess only applies as far as the line runs
    /// past its own unclipped part: no step where it first clips, however thin the line and however much dark
    /// background surrounds it.
    #[test]
    fn thin_line_without_reliable_colour_is_continuous_where_it_clips() {
        for height in [32usize, 512, 1024] {
            for width in [1usize, 4, 16] {
                let top = (height - width) / 2;
                let (row, first) = rebuilt_ramp(top..top + width, height);
                assert_continuous(&row, first, 0.002, &format!("{width} rows of {height}"));
                // and well past the first clip the neutral guess has taken over (green as high as red and blue imply)
                let far = row[1500];
                assert!(far[1] > 0.95 * far[2], "{width} rows of {height}: {far:?}");
            }
        }
    }

    /// Issue #523 (a river photo darkened by Highlights -75): a pale cyan-blue sky (`[1, 1.07, 1.2]` white-balanced)
    /// brightening from left to right through the point where green clips (and later blue), above bright
    /// yellow-green foliage, with thin dark branches standing in its lower half. Its rebuilt colour used to lean
    /// green to teal: the foliage lent its colour to the sky (green rebuilt from it), with a step where green first
    /// clips, and the sky kept its own pale colour where Lightroom shows white.
    #[test]
    fn pale_sky_by_dark_branches_and_foliage_is_not_tinted() {
        let sky = [1.0, 1.07, 1.2];
        let (w, h) = (320usize, 96usize);
        let k = |x: usize| 0.7 * (1.2 * x as f32 / (w - 1) as f32).exp2();
        let branches = [150usize, 200, 250];
        let on_branch = |x: usize| branches.iter().any(|&b| (b.saturating_sub(1)..=b + 2).contains(&x));
        let img0 = Rgb32f::from_fn(w, h, |x, y| {
            sensor(if y >= 72 {
                [0.5, 0.6, 0.22]
            } else if y >= 40 && branches.iter().any(|&b| x == b || x == b + 1) {
                [0.1, 0.09, 0.04]
            } else {
                sky.map(|v| v * k(x))
            })
        });
        let mut img = img0.clone();
        reconstruct(&mut img, WB, 0.99);
        let at = |x: usize, y: usize| chroma(balanced(img.get(x, y)));
        let first = (0..w).find(|&x| img0.get(x, 20)[1] >= 0.99).unwrap_or(0);
        assert!(first > 0 && first < 150);
        let diff = |a: [f32; 3], b: [f32; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max);
        for y in [20, 50] {
            // no step where green clips
            let step = diff(at(first - 1, y), at(first, y));
            assert!(step < 0.001, "row {y}: step of {step} where green clips");
            for x in first..w {
                // nothing but the sky's colour on its way to neutral (no green from the foliage)...
                let off = off_the_way_to_neutral(at(x, y), chroma(sky));
                assert!(on_branch(x) || off < 0.005, "row {y}, x {x}: {:?} is off the sky's way to neutral by {off}", at(x, y));
                // ... the same beside the branches as above them ...
                let beside = diff(at(x, y), at(x, 20));
                assert!(on_branch(x) || beside < 0.005, "row {y}, x {x}: {:?} beside a branch, {:?} above", at(x, y), at(x, 20));
            }
            // ... and neutral well before the last channel clips (the sky is 0.7 stops past green's clip there)
            let end = at(w - 1, y);
            assert!(end.iter().all(|v| (v - 1.0 / 3.0).abs() < 0.002), "row {y}: {end:?} at the end");
        }
    }

    #[test]
    fn unclipped_pixels_are_left_exactly() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        for (w, h) in [(1, 1), (3, 1), (7, 5), (64, 48), (129, 77)] {
            let noise: Vec<f32> = (0..w * h).map(|_| rnd()).collect();
            let img = Rgb32f::from_fn(w, h, |x, y| {
                let t = ((x as f32 * 0.21).sin() + (y as f32 * 0.17).cos()) * 0.5;
                let base = [0.5 + 0.4 * t, 0.45 - 0.2 * t, 0.3 + 0.1 * t];
                match noise[y * w + x] {
                    r if r < 0.1 => [1.0; 3],
                    r if r < 0.4 => [1.0, base[1] * 1.6, base[2]],
                    r if r > 0.995 => [0.0, 0.0, 0.00001],
                    _ => base,
                }
            });
            let mut out = img.clone();
            let n = reconstruct_with(&mut out, [2.1, 1.0, 1.6], 0.99, [1.02, 0.99, 1.05]);
            let clipped = img.data.iter().filter(|p| p.iter().any(|&v| v >= 0.99)).count();
            assert_eq!(n, clipped);
            for (a, b) in img.data.iter().zip(&out.data) {
                if a.iter().all(|&v| v < 0.99) {
                    assert_eq!(a, b, "{w}×{h}");
                } else {
                    assert!(b.iter().all(|v| v.is_finite() && *v >= 0.0), "{w}×{h}: {b:?}");
                }
            }
        }
    }

    #[test]
    fn clip_neutral_makes_clipped_white_neutral() {
        let wb = [2.0, 1.0, 1.5];
        let mut img = Rgb32f::filled(2, 1, [1.0, 1.0, 1.0]);
        img.data[1] = [0.2, 0.3, 0.4];
        clip_neutral(&mut img, wb, 1.0);
        let p = img.data[0];
        let q = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        assert!((q[0] - q[1]).abs() < 1e-6 && (q[1] - q[2]).abs() < 1e-6);
        assert_eq!(img.data[1], [0.2, 0.3, 0.4]);
    }

    #[test]
    fn reconstruct_restores_clipped_channel() {
        // a warm gradient whose red channel clips at its bright end
        let truth = Rgb32f::from_fn(256, 16, |x, _| {
            let v = 0.3 + 1.0 * x as f32 / 255.0;
            [v * 1.0, v * 0.6, v * 0.35]
        });
        let mut img = truth.map(|p| p.map(|v| v.min(1.0)));
        let first = (0..256).find(|&x| img.get(x, 8)[0] >= 0.999).unwrap();
        // near the unclipped gradient (within the reach of its colour) red is rebuilt from it
        let err = |img: &Rgb32f| -> f32 { (first..first + 12).map(|x| (img.get(x, 8)[0] - truth.get(x, 8)[0]).abs()).sum() };
        let before = err(&img);
        let n = reconstruct(&mut img, [1.0, 1.0, 1.0], 0.999);
        assert!(n > 0);
        let after = err(&img);
        assert!(after < before * 0.2, "before {before} after {after}");
        // unclipped pixels untouched
        assert_eq!(img.get(0, 0), truth.get(0, 0));
        assert_eq!(img.get(first - 1, 8), truth.get(first - 1, 8));
    }

    #[test]
    fn fully_clipped_and_no_clipping() {
        let mut img = Rgb32f::filled(4, 4, [1.0; 3]);
        assert_eq!(reconstruct(&mut img, [2.0, 1.0, 1.5], 0.99), 16);
        let p = img.data[0];
        assert!((p[0] * 2.0 - p[1]).abs() < 1e-5 && (p[2] * 1.5 - p[1]).abs() < 1e-5);
        let mut img = Rgb32f::filled(4, 4, [0.5; 3]);
        assert_eq!(reconstruct(&mut img, [2.0, 1.0, 1.5], 0.99), 0);
        let mut empty = Rgb32f::new(0, 0);
        assert_eq!(reconstruct(&mut empty, [1.0; 3], 0.99), 0);
    }

    /// `cargo test --release -p lightcraft-raw --lib -- --ignored bench_reconstruct --nocapture`
    #[test]
    #[ignore]
    fn bench_reconstruct() {
        let img = Rgb32f::from_fn(6000, 4000, |x, y| {
            let v = 0.4 + 0.8 * ((x as f32 * 0.003).sin() * (y as f32 * 0.002).cos()).abs();
            [v.min(1.0), (v * 0.7).min(1.0), (v * 0.5).min(1.0)]
        });
        let ms = (0..3)
            .map(|_| {
                let mut i = img.clone();
                let t = std::time::Instant::now();
                reconstruct(&mut i, [2.0, 1.0, 1.5], 0.99);
                t.elapsed().as_secs_f64() * 1e3
            })
            .fold(f64::MAX, f64::min);
        eprintln!("reconstruct 24 MP: {ms:.0} ms");
    }
}
