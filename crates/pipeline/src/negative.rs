//! Film negative conversion (`docs/negative-conversion.md`): a scanned colour or black-and-white negative, or a
//! slide, becomes a scene-linear positive at the start of the scene-linear stage, before white balance, so every
//! later slider (Exposure, Contrast, Temp/Tint, curves, masks) works on it the right way round.
//!
//! Per channel: the film base is divided out (removing the orange mask), the transmittance becomes a density
//! (`−log10`), the density range found by the analysis is normalised to 0..1, each channel's midtones are matched
//! (auto colour balance), and the result is mapped back to scene-linear light over the tone profile's stops. A small
//! colour model (our own generic matrices) and saturation finish it.
//!
//! The analysis ([`analyze`]) runs on a fixed-size sample of the whole source, so the numbers it measures are the
//! same for a preview and an export of the same source; `negative.analyze` stores them in the settings.

use lightcraft_develop::{BaseSource, DevelopSettings, FILM_TONE_LEVELS, FilmAnalysis, FilmColorModel, FilmType, Negative};
use lightcraft_geom::Rect;
use lightcraft_raster::Rgb32f;

use crate::for_rows;

/// Smallest transmittance / value the logarithms see.
const EPS: f32 = 1e-6;
/// Long edge (in samples) of the grid the analysis reads.
const ANALYSIS_EDGE: usize = 512;
/// Fewest usable samples an analysis needs.
const MIN_SAMPLES: usize = 64;
/// Percentile of each channel's brightest transmittance taken as the film base when it isn't sampled.
const BASE_PCT: f64 = 0.998;
/// Density percentiles that become the positive's black and white points.
const LO_PCT: f64 = 0.001;
const HI_PCT: f64 = 0.997;
/// Rec.2020 luminance weights.
const Y: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// The positive's mid grey (scene-linear) at Brightness 0, where the frame's median density lands (auto exposure).
/// Fitted on a maintainer-supplied 6×6 colour negative, as a Fuji RAF camera scan and as a JPEG scan, against a
/// reference positive the maintainer made with another converter: mean L* of both within 1 of the reference's 57.8.
const MID_GREY: f32 = 0.25;
/// Largest exponent (stops from mid grey) the conversion evaluates: far past white, far short of `exp2`'s overflow.
const MAX_STOPS: f32 = 40.0;
/// Normalised density offset per 100 of Temp / Tint (and toning).
const SHIFT: f32 = 0.08;
/// Part of the density range Black Point / White Point move at ±100.
const POINT_RANGE: f64 = 0.15;

/// A film conversion resolved for one render: everything the per-pixel pass needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Film {
    film: FilmType,
    base: [f32; 3],
    lo: [f32; 3],
    inv_range: [f32; 3],
    gamma: [f32; 3],
    /// Per channel, the auto balance as a curve through (normalised density, balanced density) knots from (0, 0) to
    /// (1, 1); empty: the analysis has no tone levels and `gamma` balances the medians.
    curves: [Vec<(f32, f32)>; 3],
    offset: [f32; 3],
    shadow: [f32; 3],
    highlight: [f32; 3],
    stops: f32,
    mid: f32,
    /// The normalised density that becomes `mid`: the frame's median after balance (auto exposure).
    center: f32,
    floor: f32,
    matrix: [[f32; 3]; 3],
}

/// Linear value → density for `film`: `−log10` of the transmittance over the base for negatives, `log10` of the
/// value for slides. Black-and-white negatives use the luminance (all three channels alike).
fn density(film: FilmType, v: [f32; 3], base: [f32; 3]) -> [f32; 3] {
    let v = v.map(|c| if c.is_finite() { c } else { 0.0 });
    match film {
        FilmType::BwNegative => {
            let t = (Y[0] * v[0] + Y[1] * v[1] + Y[2] * v[2]) / (Y[0] * base[0] + Y[1] * base[1] + Y[2] * base[2]).max(EPS);
            let d = -t.max(EPS).log10();
            [d; 3]
        }
        FilmType::Slide => v.map(|c| c.max(EPS).log10()),
        _ => [0, 1, 2].map(|i| -(v[i] / base[i].max(EPS)).max(EPS).log10()),
    }
}

/// The `p`-quantile (0..1) of `v` (reordered in place). `v` must not be empty.
fn quantile(v: &mut [f32], p: f64) -> f32 {
    let Some(last) = v.len().checked_sub(1) else { return 0.0 };
    let k = ((last as f64) * p.clamp(0.0, 1.0)).round() as usize;
    let (_, x, _) = v.select_nth_unstable_by(k.min(last), f32::total_cmp);
    *x
}

/// Side of the tiles searched for the film's unexposed edge, as a fraction of the source's long edge: smaller than
/// the thinnest rebate or holder margin worth finding (about 1 % of a scan).
const TILE: f64 = 0.005;
/// Most tiles searched along one axis (the search's cost is capped, not the source's size).
const TILE_GRID: usize = 240;
/// Fewest tiles that must agree before a film edge counts as found (one odd tile is noise or a speck).
const MIN_TILES: usize = 6;
/// Tiles within this fraction of each other in every channel are the same film edge: unexposed film is one colour
/// along its whole length, while a clear area of the picture that happens to run along the frame changes colour.
const SAME_EDGE: f32 = 0.05;
/// A film edge reaches across at least this much of the scan…
const EDGE_SPAN: f64 = 0.5;
/// …and is one strip at most this thick…
const EDGE_THIN: f64 = 0.04;
/// …or its tiles cover at most this much of their bounding box (strips around a frame, not a solid patch).
const EDGE_FILL: f64 = 0.35;
/// Smallest part of an uncropped scan the film edge found must outline before the range is measured inside it.
const MIN_FRAME_AREA: f64 = 0.2;
/// A tile is uniform when the interquartile range of its luminance is below this fraction of its median.
const UNIFORM: f32 = 0.08;
/// Unexposed film is the clearest film there is: in every channel at least this fraction of the clearest part of
/// the picture (a little under 1 for noise and falloff), and clearer overall by [`EDGE_CLEARER`]…
const EDGE_MIN: f32 = 0.97;
const EDGE_CLEARER: f32 = 1.08;
/// …and at most this many times as bright: a sanity bound only. A crop of a high-key frame can hold nothing near the
/// base, so the film edge may be several times brighter than the picture's clearest part; bare light through a holder
/// gap or a leak is told apart by clipping ([`CLIPPED`]) in a scan exposed for the film.
const EDGE_MAX: f32 = 8.0;
/// Clipped light is a plateau: a tile is clipped when at least [`PLATEAU`] of its pixels sit within this fraction of a
/// channel's largest value in the scan. Film never does that (it has grain), even when it is itself the brightest
/// thing in the scan, as an unclipped rebate lifted above 1 by a raw's white balance gains is; light at the sensor's
/// (or a rendered file's) white level does, whatever that level is per channel.
const CLIPPED: f32 = 0.9995;
/// The part of a tile's pixels at the plateau that makes it clipped.
const PLATEAU: f32 = 0.5;
/// Smallest channel maximum that can be a white level: below it nothing in the scan clipped, and the clipping test is
/// skipped. A scan whose bare light (a holder gap) stays under it in every channel isn't exposed to clip it, and such a
/// gap can then pass the other tests as film; the film-base eyedropper is for that case.
const WHITE: f32 = 0.98;

fn luma(p: [f32; 3]) -> f32 {
    Y[0] * p[0] + Y[1] * p[1] + Y[2] * p[2]
}

/// Sampled usable pixels of `src` inside `r` (normalised), at most about [`ANALYSIS_EDGE`]² of them.
fn samples(src: &Rgb32f, r: Rect) -> Vec<[f32; 3]> {
    let (w, h) = (src.width, src.height);
    let r = r.intersect(&Rect::UNIT);
    if w == 0 || h == 0 || r.is_empty() {
        return Vec::new();
    }
    let x0 = ((r.x0 * w as f64) as usize).min(w - 1);
    let y0 = ((r.y0 * h as f64) as usize).min(h - 1);
    let x1 = ((r.x1 * w as f64).ceil() as usize).clamp(x0 + 1, w);
    let y1 = ((r.y1 * h as f64).ceil() as usize).clamp(y0 + 1, h);
    // on the region's own grid, so a small crop is sampled as finely as the whole scan
    let step = (x1 - x0).max(y1 - y0).div_ceil(ANALYSIS_EDGE).max(1);
    let mut out = Vec::with_capacity((x1 - x0).div_ceil(step) * (y1 - y0).div_ceil(step));
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            if let Some(p) = src.data.get(y * w + x)
                && p.iter().all(|c| c.is_finite() && *c > 0.0)
            {
                out.push(*p);
            }
        }
    }
    out
}

/// Per-channel median of the pixels of `px` between the 85th and 95th percentiles of luminance: a bright reference
/// that a film edge taking up a few percent of the scan doesn't reach into.
fn bright_band(px: &[[f32; 3]]) -> [f32; 3] {
    let mut ys: Vec<f32> = px.iter().map(|p| luma(*p)).collect();
    let (a, b) = (quantile(&mut ys, 0.85), quantile(&mut ys, 0.95));
    let band: Vec<[f32; 3]> = px.iter().copied().filter(|p| (a..=b).contains(&luma(*p))).collect();
    if band.is_empty() { [0.0; 3] } else { median(&band) }
}

/// Per-channel median of `px` (not empty).
fn median(px: &[[f32; 3]]) -> [f32; 3] {
    let mut ch: Vec<f32> = Vec::with_capacity(px.len());
    [0, 1, 2].map(|i| {
        ch.clear();
        ch.extend(px.iter().map(|p| p[i]));
        quantile(&mut ch, 0.5)
    })
}

/// Find the film base on the unexposed edge of the film (the rebate between frames, or the clear margin a holder
/// leaves) anywhere in the scan outside the picture (`interior`, normalised source coordinates: the crop less its
/// border buffer). On a camera scan the film sits inside a holder, so this edge can be far from the scan's own edges,
/// and a crop drawn inside the frame leaves it well outside the crop. Of the small tiles there that are uniform, clearer than the clearest part of the
/// picture (`clear`, its colour) in every channel, not clipped and not far brighter than film can be, the brightest
/// group wins: unexposed film is the clearest film there is, so it outshines the picture's own clear shadows at its
/// border, while bare light through a gap or a leak is clipped or far brighter. Hues aren't compared: the picture's
/// clearest shadows still hold some dye, so they are more orange than the base. The group must also look like a film
/// edge: lines reaching across at least half the scan, not a solid patch (a clear area of the picture); one thin strip
/// counts only with `one_strip` (the band around the picture, where a gradient's clear line can't run), else it takes
/// strips around a frame. `None` when no [`MIN_TILES`] qualifying tiles agree.
pub fn detect_base(src: &Rgb32f, clear: [f32; 3], interior: Rect, one_strip: bool) -> Option<([f32; 3], Rect)> {
    let clear_y = luma(clear);
    if !(clear_y.is_finite() && clear_y > 0.0) || clear.iter().any(|c| c.is_nan() || *c <= 0.0) {
        return None;
    }
    let (w, h) = (src.width as f64, src.height as f64);
    let long = w.max(h);
    if long < 1.0 {
        return None;
    }
    // tile side and step in normalised units of each axis
    let (tw, th) = ((TILE * long / w).min(1.0), (TILE * long / h).min(1.0));
    let (sx, sy) = ((tw / 2.0).max(1.0 / TILE_GRID as f64), (th / 2.0).max(1.0 / TILE_GRID as f64));
    // each channel's white level, where the scan reaches one (see CLIPPED)
    let peak = samples(src, Rect::UNIT).iter().fold([0.0f32; 3], |m, p| [0, 1, 2].map(|i| m[i].max(p[i])));
    let clipped = |px: &[[f32; 3]]| {
        (0..3).any(|i| {
            let at = px.iter().filter(|p| p[i] >= CLIPPED * peak[i]).count();
            peak[i] >= WHITE && at as f32 >= PLATEAU * px.len() as f32
        })
    };
    let mut found: Vec<([f32; 3], Rect, f32)> = Vec::new();
    let mut ys: Vec<f32> = Vec::with_capacity(64);
    let mut px: Vec<[f32; 3]> = Vec::with_capacity(64);
    let mut y0 = 0.0;
    while y0 + th <= 1.0 + 1e-9 {
        let mut x0 = 0.0;
        while x0 + tw <= 1.0 + 1e-9 {
            let r = Rect::new(x0, y0, x0 + tw, y0 + th);
            x0 += sx;
            if !r.intersect(&interior).is_empty() {
                continue;
            }
            tile_samples(src, r, &mut px);
            if px.len() < 9 {
                continue;
            }
            ys.clear();
            ys.extend(px.iter().map(|p| luma(*p)));
            let (q1, q2, q3) = (quantile(&mut ys, 0.25), quantile(&mut ys, 0.5), quantile(&mut ys, 0.75));
            if !(q2 > 0.0 && (q3 - q1) / q2 < UNIFORM) {
                continue;
            }
            let m = median(&px);
            let y = luma(m);
            if clipped(&px) || m.iter().zip(&clear).any(|(c, k)| *c < EDGE_MIN * k) || y < EDGE_CLEARER * clear_y || y > EDGE_MAX * clear_y {
                continue;
            }
            found.push((m, r, y));
        }
        y0 += sy;
    }
    if found.len() < MIN_TILES {
        return None;
    }
    // the brightest cluster: from the top, each tile starts a cluster of the tiles of its colour (every channel within
    // SAME_EDGE); the first with at least MIN_TILES tiles that looks like a film edge wins. A few stray bright tiles
    // can't form one, a film edge always does. A rejected cluster's core (its tiles within a quarter of SAME_EDGE of its
    // median colour: the patch it is) is spent, neither an anchor again nor part of a later cluster, so a large clear
    // patch costs one rejection, not one per tile, and no fragment of it passes for an edge later; its other tiles stay
    // free, so a rebate a few percent off a brighter patch is still found.
    found.sort_by(|a, b| b.2.total_cmp(&a.2));
    let same = |a: &[f32; 3], b: &[f32; 3], tol: f32| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol * y);
    let mut spent = vec![false; found.len()];
    let mut group: Option<Vec<&([f32; 3], Rect, f32)>> = None;
    for (i, t) in found.iter().enumerate() {
        if spent.get(i).copied().unwrap_or(true) {
            continue;
        }
        let anchor = t.0;
        let members: Vec<usize> = (i..found.len())
            .filter(|k| !spent.get(*k).copied().unwrap_or(true) && found.get(*k).is_some_and(|u| same(&u.0, &anchor, SAME_EDGE)))
            .collect();
        let g: Vec<&([f32; 3], Rect, f32)> = members.iter().filter_map(|k| found.get(*k)).collect();
        if g.len() >= MIN_TILES && looks_like_edge(&g, (sx, sy), one_strip) {
            group = Some(g);
            break;
        }
        let core = median(&g.iter().map(|u| u.0).collect::<Vec<_>>());
        for k in members {
            if found.get(k).is_some_and(|u| same(&u.0, &core, 0.25 * SAME_EDGE))
                && let Some(s) = spent.get_mut(k)
            {
                *s = true;
            }
        }
    }
    let group = group?;
    let colours: Vec<[f32; 3]> = group.iter().map(|t| t.0).collect();
    let rect = group.iter().map(|t| t.1).reduce(|a, b| a.union(&b))?;
    Some((median(&colours), rect))
}

/// Tiles (on a grid of `step`) that lie along a film edge: in one direction they cover at least [`EDGE_SPAN`] of the scan
/// (their distinct positions along it, so a few patches far apart don't count as a line), and their bounding box is
/// either one thin strip (at most [`EDGE_THIN`] across; only when `one_strip`) or strips around a frame (the tiles cover
/// at most [`EDGE_FILL`] of it), not a solid patch.
fn looks_like_edge(tiles: &[&([f32; 3], Rect, f32)], step: (f64, f64), one_strip: bool) -> bool {
    let Some(b) = tiles.iter().map(|t| t.1).reduce(|a, b| a.union(&b)) else { return false };
    let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
    let area = w * h;
    let covered = |pos: &dyn Fn(&Rect) -> f64, s: f64| {
        let mut at: Vec<i64> = tiles.iter().map(|t| (pos(&t.1) / s).round() as i64).collect();
        at.sort_unstable();
        at.dedup();
        at.len() as f64 * s
    };
    let along = covered(&|r| r.x0, step.0).max(covered(&|r| r.y0, step.1));
    let fill = tiles.len() as f64 * step.0 * step.1 / area;
    area > 0.0 && along >= EDGE_SPAN && ((one_strip && w.min(h) <= EDGE_THIN) || fill <= EDGE_FILL)
}

/// Up to 8 × 8 usable pixels of `src` spread over `r` (normalised), into `out`.
fn tile_samples(src: &Rgb32f, r: Rect, out: &mut Vec<[f32; 3]>) {
    out.clear();
    let (w, h) = (src.width, src.height);
    if w == 0 || h == 0 {
        return;
    }
    for j in 0..8 {
        let y = ((r.y0 + (r.y1 - r.y0) * (j as f64 + 0.5) / 8.0) * h as f64) as usize;
        for i in 0..8 {
            let x = ((r.x0 + (r.x1 - r.x0) * (i as f64 + 0.5) / 8.0) * w as f64) as usize;
            if let Some(p) = src.data.get(y.min(h - 1) * w + x.min(w - 1))
                && p.iter().all(|c| c.is_finite() && *c > 0.0)
            {
                out.push(*p);
            }
        }
    }
}

/// Measure the film base and the density range of `src` for `neg`'s film type inside `region` (normalised source
/// coordinates: the crop, [`DevelopSettings::negative_region`]), leaving out `neg.border` % of each of its edges for
/// the range, so the holder, the rebate and light leaks around the picture don't count. The base is, in order:
/// `sampled` (a colour and the region it came from, `negative.sampleBase`), the film's unexposed edge found in the
/// scan ([`detect_base`]), or an estimate from the clearest part of the picture. A measured base sets the black
/// point's colour: the film's own clear base is neutral black. An error when there is nothing to measure.
pub fn analyze(src: &Rgb32f, neg: &Negative, region: Rect, sampled: Option<([f64; 3], Rect)>) -> Result<FilmAnalysis, String> {
    let m = measure_frame(src, neg, region, sampled)?;
    let a = range_of(neg.film, &m.px, m.base, m.base_source, m.base_rect)?;
    Ok(FilmAnalysis { region: Some(m.region), ..a })
}

/// Most picture samples one frame gives a roll analysis, so every frame weighs about the same.
const ROLL_SAMPLES: usize = 40_000;
/// Most picture samples a whole roll pools (about 24 MB): past [`ROLL_SAMPLES`] × 50 frames, each frame gives fewer, so
/// a large selection doesn't grow the memory a sync holds without bound.
const ROLL_POOL: usize = 2_000_000;

/// The samples each frame of a roll of `frames` gives its analysis: [`ROLL_SAMPLES`], fewer for a roll past
/// [`ROLL_POOL`] (at least one).
pub fn roll_samples_per_frame(frames: usize) -> usize {
    (ROLL_POOL / frames.max(1)).clamp(1, ROLL_SAMPLES)
}

/// One analysis for a whole roll (`frames`, each measured by [`measure_frame`]): the same film base and density range
/// for every frame, so frames shot alike convert alike, as a lab scans a roll. The base is a sampled one if any frame
/// has it, else the per-channel median of the bases found on film edges, else the clearest estimate (the roll's
/// least dense film); the range and balance are measured on every frame's pixels together. Its region is `None`: the
/// caller stores each frame's own. An error when no frame was measured.
pub fn analyze_roll(film: FilmType, frames: &[FrameMeasure]) -> Result<FilmAnalysis, String> {
    let first = frames.first().ok_or("no frames to analyse")?;
    let pick = |source: BaseSource| frames.iter().filter(move |f| f.base_source == source);
    let (base, base_source, base_rect) = if let Some(f) = pick(BaseSource::Sampled).next() {
        (f.base, BaseSource::Sampled, f.base_rect)
    } else if pick(BaseSource::Detected).next().is_some() {
        let bases: Vec<[f32; 3]> = pick(BaseSource::Detected).map(|f| f.base).collect();
        (median(&bases), BaseSource::Detected, pick(BaseSource::Detected).next().and_then(|f| f.base_rect))
    } else {
        let b = frames.iter().fold([0.0f32; 3], |m, f| [0, 1, 2].map(|i| m[i].max(f.base[i])));
        (b, first.base_source, None)
    };
    let per = roll_samples_per_frame(frames.len());
    let mut pooled: Vec<[f32; 3]> = Vec::with_capacity(frames.len().saturating_mul(per));
    for f in frames {
        let step = f.px.len().div_ceil(per).max(1);
        pooled.extend(f.px.iter().step_by(step));
    }
    // (frames already thinned by `thin_for_roll` pool alike: their step is 1)
    range_of(film, &pooled, base, base_source, base_rect)
}

/// How far apart the film bases measured on a roll's film edges are: the largest ratio, over the channels, of the
/// clearest to the densest base among the frames whose base was found or sampled (1 when fewer than two were). One
/// roll scanned in one setup measures about the same base on every frame; a much larger spread means the frames were
/// scanned differently (light, exposure, scanner), and one analysis won't fit them all.
pub fn base_spread(frames: &[FrameMeasure]) -> f32 {
    let bases: Vec<[f32; 3]> = frames.iter().filter(|f| f.base_source.measured()).map(|f| f.base).collect();
    if bases.len() < 2 {
        return 1.0;
    }
    (0..3)
        .map(|i| {
            let (lo, hi) = bases.iter().fold((f32::INFINITY, 0.0f32), |(lo, hi), b| (lo.min(b[i]), hi.max(b[i])));
            if lo > 0.0 { hi / lo } else { f32::INFINITY }
        })
        .fold(1.0, f32::max)
}

/// Keep only the samples [`analyze_roll`] reads from `m`, one frame of a roll of `frames` ([`roll_samples_per_frame`]), so a roll's frames can be held
/// until it is written, and the roll analysed again without some of them, at a bounded cost.
pub fn thin_for_roll(m: &mut FrameMeasure, frames: usize) {
    let step = m.px.len().div_ceil(roll_samples_per_frame(frames)).max(1);
    if step > 1 {
        m.px = m.px.iter().step_by(step).copied().collect();
    }
}

/// One frame's measurements for an analysis ([`measure_frame`]): its picture's pixels and its film base.
#[derive(Clone, Debug)]
pub struct FrameMeasure {
    /// Sampled picture pixels (inside the frame, its border left out), linear.
    pub px: Vec<[f32; 3]>,
    pub base: [f32; 3],
    pub base_source: BaseSource,
    pub base_rect: Option<Rect>,
    /// The part of the source measured (the crop, or the whole scan).
    pub region: Rect,
}

/// [`analyze`]'s first half: the frame's picture pixels and its film base, for one frame or a roll.
pub fn measure_frame(src: &Rgb32f, neg: &Negative, region: Rect, sampled: Option<([f64; 3], Rect)>) -> Result<FrameMeasure, String> {
    if !neg.converts() {
        return Err("no film type chosen".into());
    }
    if src.width == 0 || src.height == 0 || src.data.len() < src.width.saturating_mul(src.height) {
        return Err("the photo has no pixels".into());
    }
    let border = if neg.border.is_finite() { neg.border.clamp(0.0, 25.0) / 100.0 } else { 0.05 };
    let region = if [region.x0, region.y0, region.x1, region.y1].iter().all(|v| v.is_finite()) { region.intersect(&Rect::UNIT) } else { Rect::UNIT };
    let region = if region.is_empty() { Rect::UNIT } else { region };
    let (bx, by) = ((region.x1 - region.x0) * border, (region.y1 - region.y0) * border);
    let interior = Rect::new(region.x0 + bx, region.y0 + by, region.x1 - bx, region.y1 - by);
    let mut px = samples(src, interior);
    if px.len() < MIN_SAMPLES {
        return Err("too few usable pixels to analyse (all black, clipped or not a number)".into());
    }
    let film = neg.film;
    // the clearest part of the picture: its brightest percent, averaged
    let mut ys: Vec<f32> = px.iter().map(|p| luma(*p)).collect();
    let bright = quantile(&mut ys, 0.99);
    let top: Vec<[f32; 3]> = px.iter().copied().filter(|p| luma(*p) >= bright).collect();
    let clear = if top.is_empty() { [0.0; 3] } else { median(&top) };
    let (base, base_source, base_rect) = match sampled {
        Some((b, r)) if b.iter().all(|c| c.is_finite() && *c > 0.0) => (b.map(|c| c as f32), BaseSource::Sampled, Some(r)),
        Some(_) => return Err("the sampled film base is not a usable colour".into()),
        None if film == FilmType::Slide => ([1.0; 3], BaseSource::Estimated, None),
        // outside the picture first; failing that, anywhere in the scan (an uncropped camera scan: the film edge sits
        // inside the area measured, beside the holder)
        // (there the film edge can be the clearest percent itself, so it is compared with the bright band below it)
        None => match detect_base(src, clear, interior, true).or_else(|| detect_base(src, bright_band(&px), Rect::new(0.0, 0.0, 0.0, 0.0), true)) {
            Some((b, r)) => {
                // uncropped, the film edge found outlines the frame: measure the range inside it, without the holder
                let frame = r.intersect(&region);
                if region == Rect::UNIT && (frame.x1 - frame.x0) * (frame.y1 - frame.y0) >= MIN_FRAME_AREA && frame != Rect::UNIT {
                    let (bx, by) = ((frame.x1 - frame.x0) * border, (frame.y1 - frame.y0) * border);
                    let inner = Rect::new(frame.x0 + bx, frame.y0 + by, frame.x1 - bx, frame.y1 - by);
                    let inside = samples(src, inner);
                    if inside.len() >= MIN_SAMPLES {
                        px = inside;
                    }
                }
                (b, BaseSource::Detected, Some(r))
            }
            None => {
                let mut ch: Vec<f32> = Vec::with_capacity(px.len());
                let b = [0, 1, 2].map(|i| {
                    ch.clear();
                    ch.extend(px.iter().map(|p| p[i]));
                    quantile(&mut ch, BASE_PCT)
                });
                (b, BaseSource::Estimated, None)
            }
        },
    };
    if base.iter().any(|b| !(b.is_finite() && *b > 0.0)) {
        return Err("could not find the film base".into());
    }
    Ok(FrameMeasure { px, base, base_source, base_rect, region })
}

/// Each channel's black point, white point, median and tone levels of the picture pixels `px` (densities of `film`
/// over `base`), as a [`FilmAnalysis`] without a region. A measured base gives one black point for every channel.
fn range_of(film: FilmType, px: &[[f32; 3]], base: [f32; 3], base_source: BaseSource, base_rect: Option<Rect>) -> Result<FilmAnalysis, String> {
    let d: Vec<[f32; 3]> = px.iter().map(|p| density(film, *p, base)).collect();
    let mut ch: Vec<f32> = Vec::with_capacity(d.len());
    let mut lo = [0.0f32; 3];
    let mut hi = [0.0f32; 3];
    for i in 0..3 {
        ch.clear();
        ch.extend(d.iter().map(|p| p[i]));
        lo[i] = quantile(&mut ch, LO_PCT);
        hi[i] = quantile(&mut ch, HI_PCT);
    }
    if base_source.measured() && film == FilmType::ColorNegative {
        // the film's clear base is neutral: one black-point density for every channel, so the deepest shadows
        // take the base's colour (neutral once it is divided out) instead of each channel's own darkest pixels
        ch.clear();
        ch.extend(d.iter().map(|p| (p[0] + p[1] + p[2]) / 3.0));
        let l = quantile(&mut ch, LO_PCT).max(0.0);
        lo = [l; 3];
    }
    if lo.iter().zip(&hi).any(|(l, h)| (h - l).is_nan() || h - l <= 1e-3) {
        return Err("the photo has no tonal range to convert (one flat colour)".into());
    }
    let mid = [0, 1, 2].map(|i| {
        ch.clear();
        ch.extend(d.iter().map(|p| ((p[i] - lo[i]) / (hi[i] - lo[i])).clamp(1e-3, 1.0)));
        f64::from(quantile(&mut ch, 0.5))
    });
    let mut tones = [[0.0f64; 3]; FILM_TONE_LEVELS.len()];
    for i in 0..3 {
        ch.clear();
        ch.extend(d.iter().map(|p| p[i]));
        for (k, q) in FILM_TONE_LEVELS.iter().enumerate() {
            tones[k][i] = f64::from(quantile(&mut ch, *q));
        }
    }
    Ok(FilmAnalysis {
        // (no lower than the conversion's floor in f64, so the analysis stored stays valid; density floors it alike)
        base: base.map(|b| f64::from(b).max(FilmAnalysis::MIN_BASE)),
        lo: lo.map(f64::from),
        hi: hi.map(f64::from),
        mid,
        tones: Some(tones),
        base_source,
        base_rect,
        region: None,
        film: Some(film),
    })
}

/// The conversion of `s` for source `src`, or `None` when the photo isn't converted (no film type, the section
/// switched off, or nothing usable to analyse: it then renders as scanned).
pub fn resolve(src: &Rgb32f, s: &DevelopSettings) -> Option<Film> {
    if !s.negative_active() {
        return None;
    }
    let n = &s.negative;
    // (one measured for another film type is measured again: a negative's densities don't fit a slide)
    let a = match n.analysis.filter(|a| a.fits(n.film)) {
        Some(a) => a,
        None => analyze(src, n, s.negative_region(), None).ok()?,
    };
    Some(film_for(n, &a))
}

/// `v`, or `d` when `v` is NaN or infinite (a hand-edited setting).
fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

/// [`Film`] from the settings and a valid analysis.
pub fn film_for(n: &Negative, a: &FilmAnalysis) -> Film {
    let pct = |v: f64| finite_or(v, 0.0).clamp(-100.0, 100.0) / 100.0;
    let (bp, wp) = (pct(n.black_point), pct(n.white_point));
    let mut lo = [0.0f32; 3];
    let mut inv_range = [1.0f32; 3];
    for i in 0..3 {
        let r = a.hi[i] - a.lo[i];
        let l = a.lo[i] + r * POINT_RANGE * bp;
        let h = (a.hi[i] - r * POINT_RANGE * wp).max(l + 1e-4);
        lo[i] = l as f32;
        inv_range[i] = (1.0 / (h - l)) as f32;
    }
    let bw = n.film == FilmType::BwNegative;
    let gamma = if bw {
        [1.0; 3]
    } else {
        let mid = a.mid.map(|m| finite_or(m, 0.5).clamp(0.02, 0.98));
        let target = (mid[0] + mid[1] + mid[2]) / 3.0;
        let amount = finite_or(n.balance, 100.0).clamp(0.0, 100.0) / 100.0;
        mid.map(|m| (1.0 + (target.ln() / m.ln() - 1.0) * amount).clamp(0.25, 4.0) as f32)
    };
    let amount = (finite_or(n.balance, 100.0).clamp(0.0, 100.0) / 100.0) as f32;
    // the tone levels in this render's normalised densities (after Black / White Point)
    let at = |i: usize, d: f64| ((d - f64::from(lo[i])) * f64::from(inv_range[i])) as f32;
    let curves: [Vec<(f32, f32)>; 3] = match a.tones.filter(|_| !bw) {
        Some(t) if t.iter().flatten().all(|v| v.is_finite()) => {
            let target: Vec<f32> = t.iter().map(|lv| (0..3).map(|i| at(i, lv[i])).sum::<f32>() / 3.0).collect();
            [0, 1, 2].map(|i| {
                let mut k = vec![(0.0f32, 0.0f32)];
                for (lv, y) in t.iter().zip(&target) {
                    let x = at(i, lv[i]);
                    let y = x + (y - x) * amount;
                    // strictly rising, inside (0, 1): a knot that isn't is left out
                    if x > 0.0 && x < 1.0 && y > 0.0 && y < 1.0 && k.last().is_some_and(|(px, py)| x > *px + 1e-4 && y > *py + 1e-4) {
                        k.push((x, y));
                    }
                }
                k.push((1.0, 1.0));
                k
            })
        }
        _ => [Vec::new(), Vec::new(), Vec::new()],
    };
    // blue ↔ yellow, green ↔ magenta, as normalised density offsets
    let shift = |temp: f64, tint: f64| -> [f32; 3] {
        if bw {
            return [0.0; 3];
        }
        let (t, m) = (pct(temp) as f32 * SHIFT, pct(tint) as f32 * SHIFT);
        [t + 0.5 * m, -m, -t + 0.5 * m]
    };
    let stops = (n.tone.stops() * (pct(n.contrast) * 1.5).exp2()) as f32;
    // auto exposure: the frame's median density (after balance) is mid grey, whatever the scan's own exposure or
    // tone curve (a linear raw and a gamma-encoded JPEG of the same negative land alike)
    let mids = [0, 1, 2].map(|i| at(i, a.lo[i] + finite_or(a.mid[i], 0.5).clamp(0.02, 0.98) * (a.hi[i] - a.lo[i])));
    let center = ((0..3).map(|i| balance(&curves[i], gamma[i], mids[i])).sum::<f32>() / 3.0).clamp(0.05, 0.95);
    let floor = (-center * stops).exp2();
    let mid = MID_GREY * (pct(n.brightness) as f32 * 2.0).exp2();
    Film {
        film: n.film,
        base: a.base.map(|b| b as f32),
        lo,
        inv_range,
        gamma,
        curves,
        offset: shift(n.temp, n.tint),
        shadow: shift(n.shadow_temp, n.shadow_tint),
        highlight: shift(n.highlight_temp, n.highlight_tint),
        stops,
        mid,
        center,
        floor,
        matrix: if bw { IDENTITY } else { color_matrix(n.color_model, pct(n.saturation)) },
    }
}

/// Auto colour balance of one channel's normalised density `n`: through `curve` (piecewise linear, identity
/// outside 0..1), or, when it is empty, the median-matching `gamma`.
fn balance(curve: &[(f32, f32)], gamma: f32, n: f32) -> f32 {
    if curve.is_empty() {
        return if n > 0.0 { n.powf(gamma) } else { n };
    }
    if !(n > 0.0 && n < 1.0) {
        return n;
    }
    for w in curve.windows(2) {
        if let [(x0, y0), (x1, y1)] = *w
            && n <= x1
        {
            return y0 + (n - x0) * (y1 - y0) / (x1 - x0).max(EPS);
        }
    }
    n
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The colour model's matrix with saturation (`sat` −1..1) folded in: channel gains normalised to keep a neutral's
/// luminance, then a luminance-preserving saturation.
fn color_matrix(model: FilmColorModel, sat: f64) -> [[f32; 3]; 3] {
    let (gain, model_sat) = match model {
        FilmColorModel::None => ([1.0, 1.0, 1.0], 1.0),
        FilmColorModel::Neutral => ([1.0, 1.0, 1.0], 1.10),
        // fitted on the same pair (its mean b* 3.6 against 4.5, a* 1.3 against −0.4): warm, a touch green
        FilmColorModel::Warm => ([1.09, 1.04, 0.89], 1.12),
        FilmColorModel::Cool => ([0.95, 1.0, 1.07], 1.06),
    };
    let y = Y[0] * gain[0] + Y[1] * gain[1] + Y[2] * gain[2];
    let gain = gain.map(|g| g / y);
    let s = (model_sat * (1.0 + sat)).max(0.0) as f32;
    let mut m = [[0.0f32; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            let sat = (1.0 - s) * Y[c] + if r == c { s } else { 0.0 };
            *v = sat * gain[c];
        }
    }
    m
}

impl Film {
    /// One pixel: scanned linear value → scene-linear positive.
    pub fn convert(&self, v: [f32; 3]) -> [f32; 3] {
        let d = density(self.film, v, self.base);
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            let n = (d[i] - self.lo[i]) * self.inv_range[i];
            let n = balance(&self.curves[i], self.gamma[i], n);
            let w = n.clamp(0.0, 1.0);
            let n = n + self.offset[i] + (1.0 - w) * (1.0 - w) * self.shadow[i] + w * w * self.highlight[i];
            // (the exponent capped well below f32's overflow: an extreme highlight clips to white, never to 0)
            let e = ((n - self.center) * self.stops).clamp(-MAX_STOPS, MAX_STOPS);
            let l = self.mid * (e.exp2() - self.floor) / (1.0 - self.floor).max(EPS);
            out[i] = if l.is_finite() { l.max(0.0) } else { 0.0 };
        }
        let m = &self.matrix;
        [0, 1, 2].map(|r| (m[r][0] * out[0] + m[r][1] * out[1] + m[r][2] * out[2]).max(0.0))
    }
}

/// `src` as the photo shows it for analyses of the picture (Auto, Auto White Balance, the white balance selector):
/// the converted positive when `s` converts a film scan, else `src` itself. Read it with
/// [`crate::local::wb_source`]'s source facts.
pub fn positive_source<'a>(src: &'a Rgb32f, s: &DevelopSettings) -> std::borrow::Cow<'a, Rgb32f> {
    match resolve(src, s) {
        Some(film) => {
            let mut img = src.clone();
            apply(&mut img, &film);
            std::borrow::Cow::Owned(img)
        }
        None => std::borrow::Cow::Borrowed(src),
    }
}

/// Convert `img` in place.
pub fn apply(img: &mut Rgb32f, film: &Film) {
    let w = img.width;
    for_rows(&mut img.data, w, |_, row| {
        for p in row.iter_mut() {
            *p = film.convert(*p);
        }
    });
}

/// Mean colour (linear) of `src` inside `rect` (normalised source coordinates, clamped to the image), for sampling
/// the film base from the rebate. `None` when the rectangle is empty, not finite or holds no usable pixel.
pub fn sample_mean(src: &Rgb32f, rect: lightcraft_geom::Rect) -> Option<[f64; 3]> {
    let (w, h) = (src.width as f64, src.height as f64);
    if ![rect.x0, rect.y0, rect.x1, rect.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    let r = rect.intersect(&lightcraft_geom::Rect::UNIT);
    if r.is_empty() || src.width == 0 || src.height == 0 {
        return None;
    }
    let x0 = ((r.x0 * w).floor() as usize).min(src.width - 1);
    let y0 = ((r.y0 * h).floor() as usize).min(src.height - 1);
    let x1 = ((r.x1 * w).ceil() as usize).min(src.width).max(x0 + 1);
    let y1 = ((r.y1 * h).ceil() as usize).min(src.height).max(y0 + 1);
    let step = (x1 - x0).max(y1 - y0).div_ceil(ANALYSIS_EDGE).max(1);
    let (mut sum, mut n) = ([0.0f64; 3], 0usize);
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            if let Some(p) = src.data.get(y * src.width + x)
                && p.iter().all(|c| c.is_finite() && *c > 0.0)
            {
                for i in 0..3 {
                    sum[i] += f64::from(p[i]);
                }
                n += 1;
            }
        }
    }
    (n > 0).then(|| sum.map(|s| s / n as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::DevelopSettings;

    /// Orange-masked base of the synthetic film (linear transmittance per channel).
    const BASE: [f32; 3] = [0.70, 0.45, 0.25];
    /// Density per decade of scene luminance, per layer (unequal, as real film's are).
    const GAMMA: [f32; 3] = [0.55, 0.62, 0.70];

    /// A scene value (linear, 0.005..1.3 times a colour) photographed on the synthetic film and scanned.
    fn on_film(l: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| BASE[i] * 10f32.powf(-GAMMA[i] * (l[i].max(1e-4) / 0.005).log10().max(0.0)))
    }

    /// A synthetic negative of a scene: grey ramp left to right, colour patches below; with a rebate strip (bare
    /// base) `strip` deep at top and bottom.
    fn negative(w: usize, h: usize, strip: f64) -> Rgb32f {
        let mut img = Rgb32f::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let fy = y as f64 / h as f64;
                let v = if fy < strip || fy >= 1.0 - strip {
                    BASE
                } else {
                    // clearest (darkest in the scene) in the middle, densest at the sides
                    let l = 0.02 * 65f32.powf((2.0 * x as f32 / w as f32 - 1.0).abs());
                    let tint = match (fy * 4.0) as usize {
                        1 => [1.0, 0.6, 0.4],
                        2 => [0.4, 0.8, 1.0],
                        _ => [1.0, 1.0, 1.0],
                    };
                    on_film([l * tint[0], l * tint[1], l * tint[2]])
                };
                img.data[y * w + x] = v;
            }
        }
        img
    }

    fn settings(film: FilmType) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.negative.film = film;
        s
    }

    #[test]
    fn detects_the_base_on_the_rebate() {
        let img = negative(400, 300, 0.015);
        let a = analyze(&img, &settings(FilmType::ColorNegative).negative, Rect::UNIT, None).unwrap();
        assert_eq!(a.base_source, BaseSource::Detected);
        for i in 0..3 {
            assert!((a.base[i] - f64::from(BASE[i])).abs() < 1e-3, "base {:?}", a.base);
        }
        let r = a.base_rect.unwrap();
        assert!(r.y0 < 0.03 || r.y1 > 0.97, "rect {r:?}");
    }

    #[test]
    fn estimates_the_base_without_a_rebate() {
        let img = negative(400, 300, 0.0);
        let a = analyze(&img, &settings(FilmType::ColorNegative).negative, Rect::UNIT, None).unwrap();
        assert_eq!(a.base_source, BaseSource::Estimated, "{a:?}");
        assert!(a.is_valid());
    }

    #[test]
    fn bare_light_is_not_the_base() {
        // a holder gap (light through no film, clipped in a scan exposed for the film) at the top, the rebate at the
        // bottom (unclipped bare light can pass for film: the film-base eyedropper is for that)
        let mut img = negative(400, 300, 0.015);
        for y in 0..4 {
            for x in 0..400 {
                img.data[y * 400 + x] = [1.0, 0.995, 0.99];
            }
        }
        let a = analyze(&img, &settings(FilmType::ColorNegative).negative, Rect::UNIT, None).unwrap();
        assert_eq!(a.base_source, BaseSource::Detected);
        assert!((a.base[2] - f64::from(BASE[2])).abs() < 1e-3, "base {:?}", a.base);
    }

    #[test]
    fn greys_convert_neutral_and_in_order() {
        let img = negative(400, 300, 0.015);
        let s = settings(FilmType::ColorNegative);
        let f = resolve(&img, &s).unwrap();
        let mut last = -1.0;
        for k in 1..12 {
            let l = 0.02 * 65f32.powf(k as f32 / 12.0);
            let p = f.convert(on_film([l; 3]));
            let y = luma(p);
            assert!(y > last, "positive must rise with the scene: {p:?}");
            last = y;
            // a neutral grey stays near neutral (density balance, then the auto midtone balance)
            let spread = p.iter().fold(0.0f32, |m, c| m.max((c / y.max(1e-6)).ln().abs()));
            assert!(spread < 0.25, "grey {l} → {p:?}");
        }
    }

    #[test]
    fn black_and_white_is_grey() {
        let img = negative(200, 150, 0.0);
        let f = resolve(&img, &settings(FilmType::BwNegative)).unwrap();
        let p = f.convert(on_film([0.1, 0.05, 0.2]));
        assert!((p[0] - p[1]).abs() < 1e-6 && (p[1] - p[2]).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn off_or_switched_off_converts_nothing() {
        let img = negative(200, 150, 0.0);
        assert!(resolve(&img, &settings(FilmType::Off)).is_none());
        let mut s = settings(FilmType::ColorNegative);
        s.set_section_enabled("negative", false);
        assert!(resolve(&img, &s).is_none());
    }

    #[test]
    fn flat_and_empty_images_are_errors_not_panics() {
        let n = settings(FilmType::ColorNegative).negative;
        let mut flat = Rgb32f::new(64, 64);
        flat.data.fill([0.3, 0.2, 0.1]);
        assert!(analyze(&flat, &n, Rect::UNIT, None).is_err());
        assert!(analyze(&Rgb32f::new(0, 0), &n, Rect::UNIT, None).is_err());
        let mut nan = Rgb32f::new(64, 64);
        nan.data.fill([f32::NAN, f32::INFINITY, -1.0]);
        assert!(analyze(&nan, &n, Rect::UNIT, None).is_err());
        assert!(resolve(&flat, &settings(FilmType::ColorNegative)).is_none());
    }

    #[test]
    fn hostile_settings_and_pixels_give_finite_output() {
        let img = negative(200, 150, 0.015);
        let mut s = settings(FilmType::ColorNegative);
        let n = &mut s.negative;
        (n.contrast, n.brightness, n.black_point, n.white_point) = (f64::NAN, f64::INFINITY, 1e9, -1e9);
        (n.balance, n.temp, n.saturation, n.highlight_tint) = (f64::NAN, -1e9, f64::NEG_INFINITY, 1e300);
        let f = resolve(&img, &s).unwrap();
        for v in [[0.0; 3], [f32::NAN; 3], [f32::INFINITY, -1.0, 1e30], [1e-30; 3], BASE] {
            assert!(f.convert(v).iter().all(|c| c.is_finite() && *c >= 0.0), "{v:?}");
        }
        // a damaged stored analysis renders from a fresh one
        s.negative.analysis = Some(FilmAnalysis { base: [0.0, f64::NAN, 1.0], ..FilmAnalysis::default() });
        assert!(resolve(&img, &s).is_some());
    }

    #[test]
    fn sampled_base_and_rect_clamping() {
        let img = negative(200, 150, 0.02);
        let m = sample_mean(&img, Rect::new(0.2, 0.0, 0.8, 0.01)).unwrap();
        assert!((m[0] - f64::from(BASE[0])).abs() < 1e-4);
        assert!(sample_mean(&img, Rect::new(2.0, 2.0, 3.0, 3.0)).is_none());
        assert!(sample_mean(&img, Rect::new(f64::NAN, 0.0, 1.0, 1.0)).is_none());
        let a = analyze(&img, &settings(FilmType::ColorNegative).negative, Rect::UNIT, Some((m, Rect::new(0.2, 0.0, 0.8, 0.01)))).unwrap();
        assert_eq!(a.base_source, BaseSource::Sampled);
    }

    #[test]
    fn unconverted_settings_keep_their_json() {
        let s = DevelopSettings::default();
        assert!(s.to_json().get("negative").is_none());
        let back = DevelopSettings::from_json(&settings(FilmType::ColorNegative).to_json()).unwrap();
        assert_eq!(back.negative.film, FilmType::ColorNegative);
        // an unknown film or tone from a newer LightCraft loads (and renders unconverted)
        let odd2 = DevelopSettings::from_json(&serde_json::json!({"negative": {"film": "kodachromeX", "tone": "?"}})).unwrap();
        assert_eq!(odd2.negative.film, FilmType::Unknown);
        assert!(!odd2.negative.converts());
    }

    /// A camera scan as from a copy stand: the square frame in the middle of a 3:2 scan, the rest a dark grey holder,
    /// thin rebate strips along the frame's edges (not the scan's), cropped to the frame. Before crop-aware analysis
    /// and the tile search, the holder set the density range (a flat, purple positive) and the base wasn't found.
    fn holder_scan() -> (Rgb32f, DevelopSettings) {
        let (w, h) = (600, 400);
        let mut img = Rgb32f::new(w, h);
        let frame = (90, 510); // x range of film; the film runs the full height
        for y in 0..h {
            for x in 0..w {
                let v = if x < frame.0 || x >= frame.1 {
                    [0.04, 0.045, 0.05] // the holder: opaque, dimly lit
                } else if x < frame.0 + 8 || x >= frame.1 - 8 || y < 6 || y >= h - 6 {
                    BASE // the rebate around the frame
                } else {
                    let l = 0.01 * 130f32.powf((x - frame.0) as f32 / (frame.1 - frame.0) as f32);
                    on_film([l; 3])
                };
                img.data[y * w + x] = v;
            }
        }
        let mut s = settings(FilmType::ColorNegative);
        s.crop.geometry.rect = Rect::new(frame.0 as f64 / w as f64, 0.0, frame.1 as f64 / w as f64, 1.0);
        (img, s)
    }

    #[test]
    fn camera_scan_in_a_holder_finds_the_rebate_and_ignores_the_holder() {
        let (img, s) = holder_scan();
        let a = analyze(&img, &s.negative, s.negative_region(), None).unwrap();
        assert_eq!(a.base_source, BaseSource::Detected, "{a:?}");
        for i in 0..3 {
            assert!((a.base[i] - f64::from(BASE[i])).abs() < 1e-3, "base {:?}", a.base);
        }
        // the range is the film's own (its densest scene value), not stretched by the holder
        let film_max = density(FilmType::ColorNegative, on_film([1.3; 3]), BASE);
        assert!(a.hi.iter().zip(&film_max).all(|(h, d)| (*h - f64::from(*d)).abs() < 0.15), "hi {:?} vs film {film_max:?}", a.hi);
        assert!(a.region.unwrap().x0 > 0.14, "region {:?}", a.region);
        let f = resolve(&img, &s).unwrap();
        // a mid grey of the scene converts near neutral
        let p = f.convert(on_film([0.1; 3]));
        let y = luma(p);
        assert!(p.iter().all(|c| (c / y).ln().abs() < 0.25), "{p:?}");
    }

    #[test]
    fn the_analysis_region_follows_the_crop_through_rotation() {
        let mut s = settings(FilmType::ColorNegative);
        s.crop.geometry.rect = Rect::new(0.1, 0.2, 0.5, 0.9);
        s.orientation = lightcraft_geom::Orientation::Rotate270;
        let r = s.negative_region();
        let back = s.orientation.map_norm_rect(r);
        let c = s.crop.geometry.rect;
        assert!([back.x0 - c.x0, back.y0 - c.y0, back.x1 - c.x1, back.y1 - c.y1].iter().all(|d| d.abs() < 1e-9), "{back:?}");
        s.crop.geometry.rect = Rect::new(f64::NAN, 0.0, 1.0, 1.0);
        assert_eq!(s.negative_region(), Rect::UNIT);
    }

    /// A roll's frames give it fewer samples each past the pool's size, so a large selection holds bounded memory.
    #[test]
    fn a_large_roll_pools_a_bounded_number_of_samples() {
        assert_eq!(roll_samples_per_frame(36), ROLL_SAMPLES);
        assert_eq!(roll_samples_per_frame(0), ROLL_SAMPLES);
        assert_eq!(roll_samples_per_frame(1000) * 1000, ROLL_POOL);
        assert_eq!(roll_samples_per_frame(usize::MAX), 1);
    }

    /// A base channel measured below the conversion's floor is stored at the floor, so the analysis stays valid (and
    /// isn't measured again on every render).
    #[test]
    fn a_base_below_the_floor_is_stored_valid() {
        // (a red layer so dense that its clear base, and the picture, read below the floor)
        let px: Vec<[f32; 3]> = (1..200).map(|k| on_film([k as f32 / 200.0; 3])).map(|p| [p[0] * 1.0e-8, p[1], p[2]]).collect();
        let a = range_of(FilmType::ColorNegative, &px, [0.7e-8, 0.45, 0.25], BaseSource::Sampled, None).unwrap();
        assert_eq!(a.base[0], FilmAnalysis::MIN_BASE);
        assert!(a.is_valid(), "{a:?}");
    }

    /// A large clear patch a little brighter than the rebate, and close to it in colour, is rejected (with the rebate
    /// it makes a solid area, not an edge); the rebate, a strip along the top, is still tried on its own and found.
    #[test]
    fn a_rebate_close_in_colour_to_a_brighter_rejected_patch_is_found() {
        let (w, h) = (600, 400);
        let mut img = Rgb32f::new(w, h);
        let patch = BASE.map(|c| c * 1.02);
        for y in 0..h {
            for x in 0..w {
                img.data[y * w + x] = if y < 8 {
                    BASE
                } else if (30..570).contains(&x) && (60..390).contains(&y) {
                    patch
                } else {
                    BASE.map(|c| c * 0.3)
                };
            }
        }
        let clear = BASE.map(|c| c * 0.5);
        let (base, rect) = detect_base(&img, clear, Rect::new(0.0, 0.0, 0.0, 0.0), true).expect("the rebate is found");
        assert!(base.iter().zip(&BASE).all(|(b, r)| (b - r).abs() < 1e-4), "base {base:?}, not the patch {patch:?}");
        assert!(rect.y1 < 0.1, "the strip along the top: {rect:?}");
    }

    #[test]
    fn a_crop_well_inside_the_frame_still_finds_the_rebate() {
        // the band around this crop holds only picture: the search falls back to the whole scan
        let (img, mut s) = holder_scan();
        s.crop.geometry.rect = Rect::new(0.35, 0.3, 0.65, 0.7);
        let a = analyze(&img, &s.negative, s.negative_region(), None).unwrap();
        assert_eq!(a.base_source, BaseSource::Detected, "{a:?}");
        assert!((a.base[2] - f64::from(BASE[2])).abs() < 1e-3, "base {:?}", a.base);
    }

    #[test]
    fn a_roll_shares_the_base_found_on_one_frame_and_pools_the_range() {
        let n = settings(FilmType::ColorNegative).negative;
        // frame A shows the rebate; frame B is a darker scene with none (its clearest estimate is denser film)
        let a = negative(300, 200, 0.02);
        let mut b = negative(300, 200, 0.0);
        for p in &mut b.data {
            *p = p.map(|c| c * 0.6);
        }
        let ma = measure_frame(&a, &n, Rect::UNIT, None).unwrap();
        let mb = measure_frame(&b, &n, Rect::UNIT, None).unwrap();
        assert_eq!((ma.base_source, mb.base_source), (BaseSource::Detected, BaseSource::Estimated));
        let roll = analyze_roll(FilmType::ColorNegative, &[ma.clone(), mb.clone()]).unwrap();
        assert_eq!(roll.base_source, BaseSource::Detected);
        assert!(roll.base.iter().zip(&BASE).all(|(r, b)| (r - f64::from(*b)).abs() < 1e-3), "{:?}", roll.base);
        // the pooled range reaches as far as either frame's own
        let alone = |m: &FrameMeasure| range_of(FilmType::ColorNegative, &m.px, m.base, BaseSource::Detected, None).unwrap();
        let (ra, rb) = (alone(&ma), analyze(&b, &n, Rect::UNIT, Some((roll.base, Rect::UNIT))).unwrap());
        for i in 0..3 {
            assert!(
                roll.hi[i] >= ra.hi[i].min(rb.hi[i]) - 1e-6 && roll.hi[i] <= ra.hi[i].max(rb.hi[i]) + 1e-6,
                "hi {:?} vs {:?} / {:?}",
                roll.hi,
                ra.hi,
                rb.hi
            );
        }
        assert!(analyze_roll(FilmType::ColorNegative, &[]).is_err());
    }
}
