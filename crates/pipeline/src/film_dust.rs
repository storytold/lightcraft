//! Film dust and hair detection on a scanned negative or slide, after its conversion: dust and
//! hair on the film block light, so in the positive they show as **sharp, bright** specks and thin
//! bright lines (sensor dust, [`crate::dust`], shows as soft dark round spots instead). Classical:
//! a grey opening (a local minimum, then maximum, over a few pixels) removes every bright feature
//! narrower than that, so the original minus its opening (a white top-hat) holds exactly the
//! small and thin bright features. Those standing well above the local texture are grouped into
//! blobs; a small roundish blob is a speck (one heal spot), a long thin one a hair or scratch (a
//! heal stroke along its centre line).
//!
//! A film scanner's **infrared pass** ([`detect_infrared`]: a VueScan or SilverFast RGBI file)
//! makes this certain: film dyes pass infrared, dust, hairs and scratches block it, so the pass
//! is bright everywhere but under a defect. It is divided by its own local level (a grey closing
//! fills every dip narrower than the window: the lamp's falloff and the little the dyes absorb
//! stay out of it), and the dips are grouped into the same specks, lines and, for anything larger,
//! patches. The picture itself is only read for where to heal a line from.
//!
//! Sources: morphological top-hat transform (J. Serra, "Image Analysis and Mathematical
//! Morphology", 1982); blob shape from second moments; own design otherwise.

use std::sync::{Arc, Mutex, PoisonError};

use lcc_raster::{Plane, Rgb32f, Rgba8};

/// A found defect: a speck (one point) or a hair or scratch (a path of points), in 0..1 of the
/// image's width and height, with its radius (fraction of the long edge) and how much brighter
/// than its surroundings it is (0..1). A line also suggests where to heal it from: `source`, an
/// offset (0..1 of the width and height) to the side, onto the smoother of the two strips that
/// run beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct FilmDefect {
    pub points: Vec<(f64, f64)>,
    pub radius: f64,
    pub strength: f32,
    pub source: Option<(f64, f64)>,
}

impl FilmDefect {
    pub fn is_line(&self) -> bool {
        self.points.len() > 1
    }
}

/// Most defects reported from the picture: the strongest are kept.
const MAX_DEFECTS: usize = 150;
/// Most defects reported from the infrared pass, which is sure of each one: a dusty frame has a
/// few hundred (each is one heal spot, so there is a limit still).
const MAX_DEFECTS_INFRARED: usize = 400;
/// Most points on a hair's path.
const MAX_PATH_POINTS: usize = 48;

/// Separable running minimum (`max == false`) or maximum over a square of side `2r + 1`, edges
/// repeated: van Herk / Gil-Werman, a few comparisons a pixel whatever `r`.
fn rank(src: &Plane, r: usize, max: bool) -> Plane {
    let (w, h) = (src.width, src.height);
    if r == 0 || w == 0 || h == 0 || src.data.len() < w * h {
        return src.clone();
    }
    let mut rows = Plane::new(w, h);
    let mut buf = RankBuf::default();
    for y in 0..h {
        let (a, b) = (y * w, y * w + w);
        if let (Some(line), Some(out)) = (src.data.get(a..b), rows.data.get_mut(a..b)) {
            rank_line(line, out, r, max, &mut buf);
        }
    }
    let mut out = Plane::new(w, h);
    let (mut col, mut res) = (vec![0.0f32; h], vec![0.0f32; h]);
    for x in 0..w {
        for (y, c) in col.iter_mut().enumerate() {
            *c = rows.data.get(y * w + x).copied().unwrap_or(0.0);
        }
        rank_line(&col, &mut res, r, max, &mut buf);
        for (y, v) in res.iter().enumerate() {
            if let Some(o) = out.data.get_mut(y * w + x) {
                *o = *v;
            }
        }
    }
    out
}

/// Scratch space of [`rank_line`], reused from line to line.
#[derive(Default)]
struct RankBuf {
    pad: Vec<f32>,
    g: Vec<f32>,
    hh: Vec<f32>,
}

/// One line of [`rank`]: `out[i]` is the extreme of `line[i - r ..= i + r]`, edges repeated.
fn rank_line(line: &[f32], out: &mut [f32], r: usize, max: bool, b: &mut RankBuf) {
    let n = line.len();
    let (Some(&first), Some(&last)) = (line.first(), line.last()) else { return };
    let pick = |a: f32, c: f32| if max { a.max(c) } else { a.min(c) };
    let k = 2 * r + 1;
    b.pad.clear();
    b.pad.extend(std::iter::repeat_n(first, r));
    b.pad.extend_from_slice(line);
    b.pad.extend(std::iter::repeat_n(last, r));
    let m = b.pad.len();
    b.g.clear();
    b.g.resize(m, 0.0);
    b.hh.clear();
    b.hh.resize(m, 0.0);
    for j in 0..m {
        let v = b.pad[j];
        b.g[j] = if j % k == 0 { v } else { pick(b.g[j - 1], v) };
    }
    for j in (0..m).rev() {
        let v = b.pad[j];
        b.hh[j] = if j + 1 == m || (j + 1) % k == 0 { v } else { pick(b.hh[j + 1], v) };
    }
    for (i, o) in out.iter_mut().enumerate().take(n) {
        *o = pick(b.hh[i], b.g[i + 2 * r]);
    }
}

/// Film dust, hair and scratches in a rendered positive; `sensitivity` 0..100 (higher finds
/// fainter ones).
pub fn detect(img: &Rgba8, sensitivity: f32) -> Vec<FilmDefect> {
    let (w, h) = (img.width, img.height);
    if w < 64 || h < 64 || img.data.len() < w * h {
        return Vec::new();
    }
    let long = w.max(h) as f32;
    let l = Plane::from_fn(w, h, |x, y| {
        let p = img.data.get(y * w + x).copied().unwrap_or_default();
        (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])) / 255.0
    });
    // the widest defect: about 0.15 % of the long edge (4 px at 2400)
    let r = ((0.0015 * long).round() as usize).max(2);
    let opened = rank(&rank(&l, r, false), r, true);
    let tophat: Vec<f32> = l.data.iter().zip(&opened.data).map(|(a, b)| (a - b).max(0.0)).collect();
    // local texture, in small cells: the mean top-hat (grain and foliage make it busy
    // everywhere) or the spread of the opened image (the photo with dust removed: edges,
    // railings, window frames), whichever is larger; then the busiest of the 3 x 3 cells around
    // each pixel, so a bright pixel at a busy area's edge doesn't stand out either. A speck on a
    // smooth area (sky, skin, a wall) stands alone.
    let cell = ((0.008 * long).round() as usize).max(4);
    let (cw, ch) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut sums = vec![(0.0f32, 0.0f32, 0.0f32, 0u32); cw * ch];
    for (i, (t, o)) in tophat.iter().zip(&opened.data).enumerate() {
        if let Some(c) = sums.get_mut((i / w) / cell * cw + (i % w) / cell) {
            c.0 += t;
            c.1 += o;
            c.2 += o * o;
            c.3 += 1;
        }
    }
    let busy: Vec<f32> = sums
        .iter()
        .map(|c| {
            let n = c.3.max(1) as f32;
            let mean = c.1 / n;
            (c.0 / n).max((c.2 / n - mean * mean).max(0.0).sqrt())
        })
        .collect();
    let busiest: Vec<f32> = (0..cw * ch)
        .map(|c| {
            let (cx, cy) = (c % cw, c / cw);
            let mut m = 0.0f32;
            for y in cy.saturating_sub(1)..=(cy + 1).min(ch - 1) {
                for x in cx.saturating_sub(1)..=(cx + 1).min(cw - 1) {
                    m = m.max(busy.get(y * cw + x).copied().unwrap_or(0.0));
                }
            }
            m
        })
        .collect();
    let texture = |i: usize| busiest.get((i / w) / cell * cw + (i % w) / cell).copied().unwrap_or(1.0);
    let s = (sensitivity / 100.0).clamp(0.0, 1.0);
    let min_bright = 0.35 - 0.27 * s;
    let contrast = 4.0 - 1.5 * s;
    // the outermost pixels read as a thin bright line wherever the photo is brighter at its
    // very edge: left out
    let margin = r + 1;
    let candidate = |i: usize| {
        let (x, y) = (i % w, i / w);
        if x < margin || y < margin || x + margin >= w || y + margin >= h {
            return false;
        }
        let t = tophat.get(i).copied().unwrap_or(0.0);
        t > min_bright && t > contrast * texture(i)
    };
    let max_blob = ((0.06 * long) * (0.004 * long)).max(64.0) as usize;
    let mut out = Vec::new();
    for px in blobs(w, h, max_blob, candidate, |i| tophat.get(i).copied().unwrap_or(0.0)) {
        if let Some(d) = classify(&px, &l, long, Certainty::Guessed) {
            out.push(d);
        }
    }
    out.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    out.truncate(MAX_DEFECTS);
    out
}

/// Film dust, hairs and scratches from a scanner's infrared pass `ir` (the picture's size, 0..1:
/// bright where the film is clear) at `sensitivity` 0..100, as [`detect`] reports them; `luma`
/// (the picture, the same size) only says which side of a line to heal from. Dips of the pass
/// below its local level are defects, whatever the picture shows there; a blob too large for a
/// speck or too wide for a line is a patch (one round heal spot over it). Outside the film
/// (the scanner's gate, where the pass is dark everywhere) nothing is reported.
pub fn detect_infrared(ir: &Plane, luma: &Plane, sensitivity: f32) -> Vec<FilmDefect> {
    let (w, h) = (ir.width, ir.height);
    if luma.width != w || luma.height != h || luma.data.len() < w * h {
        return Vec::new();
    }
    // (the marks' own shape: a caller that wants them grown grows the spots it makes of them)
    let Some(map) = infrared_map(ir, sensitivity, 0) else { return Vec::new() };
    let long = w.max(h) as f32;
    let r = closing_radius(long);
    let margin = 2 * r + 1;
    let candidate = |i: usize| {
        let (x, y) = (i % w, i / w);
        x >= margin && y >= margin && x + margin < w && y + margin < h && map.data.get(i).is_some_and(|v| *v > 0.0)
    };
    // a hair may cross the whole frame, a patch be a few percent of the long edge across
    let max_blob = ((0.6 * long) * (0.012 * long)).max(256.0) as usize;
    let mut out = Vec::new();
    for px in blobs(w, h, max_blob, candidate, |i| map.data.get(i).copied().unwrap_or(0.0)) {
        if let Some(d) = classify(&px, luma, long, Certainty::Infrared) {
            out.push(d);
        }
    }
    out.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    out.truncate(MAX_DEFECTS_INFRARED);
    out
}

/// The window the spot search ([`detect_infrared`]) leaves along the frame's edge.
fn closing_radius(long: f32) -> usize {
    ((0.006 * long).round() as usize).max(3)
}

/// The defect map of a scanner's infrared pass `ir` (0..1, bright where the film is clear) at
/// `sensitivity` 0..100: 1 on dust, hairs and scratches, 0 on clean film and outside it, grown
/// by `expand` pixels (shrunk when negative). `None` for a pass too small to read.
///
/// The pass is grainy (a few percent from pixel to pixel), so the clean film's level is the
/// pass's own smoothed level with the dips left out (a normalised convolution), and a pixel
/// counts by how many grain widths it falls below it: a speck's core well past the grain, and
/// the soft edge around it joined to that core (hysteresis). A faint scratch a few pixels wide
/// barely leaves the grain at any one pixel, but along its length it does: the pass is averaged
/// along short strokes in a dozen directions (at half size), and a stroke that falls well below
/// the strips beside it is a line. What [`detect_infrared`] groups into defects, the mask the
/// develop step fills and the overlay shows.
pub fn infrared_map(ir: &Plane, sensitivity: f32, expand: i32) -> Option<Plane> {
    let (w, h) = (ir.width, ir.height);
    if w < 64 || h < 64 || ir.data.len() < w * h {
        return None;
    }
    let s = (sensitivity / 100.0).clamp(0.0, 1.0);
    if !s.is_finite() {
        return None;
    }
    let long = w.max(h) as f32;
    let clean = |v: f32| if v.is_finite() { v.clamp(0.0, 4.0) } else { 0.0 };
    let ir = Plane { width: w, height: h, data: ir.data.iter().take(w * h).copied().map(clean).collect() };
    // the grain, softened a little: a pixel's noise is a third lower and a speck keeps its shape
    let sm = lcc_raster::blur::gaussian(&ir, 1.0);
    // a first guess of the clean level, on a reduced copy: a closing fills every dip narrower than
    // its window; the film is where that level is at least a fifth of its typical value
    let f = ((long / 650.0).round() as usize).clamp(1, 8);
    let small = shrink(&sm, f);
    let r = (((0.004 * long) / f as f32).round() as usize).max(2);
    let lvl_small = closing(&small, r);
    let mut sorted: Vec<f32> = lvl_small.data.iter().copied().filter(|v| v.is_finite()).collect();
    sorted.sort_by(f32::total_cmp);
    let typical = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    if typical <= 1e-4 {
        return Some(Plane::new(w, h));
    }
    let lit = Plane::from_fn(lvl_small.width, lvl_small.height, |x, y| if lvl_small.get(x, y) >= 0.2 * typical { 1.0 } else { 0.0 });
    // (a band the window's width along the film's edge is left out: there the closing reaches the gate)
    let inside_small = rank(&lit, r + 1, false);
    let lvl0 = grow(&lvl_small, f, w, h);
    let inside = grow(&inside_small, f, w, h);
    // the clean level: the smoothed pass where it isn't a dip, spread over the dips
    let keep = Plane::from_fn(w, h, |x, y| {
        let (v, l) = (sm.get(x, y), lvl0.get(x, y));
        if l > 0.0 && v / l > 0.96 { 1.0 } else { 0.0 }
    });
    let kept = Plane { width: w, height: h, data: sm.data.iter().zip(&keep.data).map(|(v, k)| v * k).collect() };
    let sigma = (0.003 * long).max(3.0);
    let (num, den) = (lcc_raster::blur::gaussian(&kept, sigma), lcc_raster::blur::gaussian(&keep, sigma));
    let film = |i: usize| inside.data.get(i).copied().unwrap_or(0.0) >= 0.5;
    let drop = Plane::from_fn(w, h, |x, y| {
        let i = y * w + x;
        let (n, dn, v) = (num.data.get(i).copied().unwrap_or(0.0), den.data.get(i).copied().unwrap_or(0.0), sm.get(x, y));
        let level = if dn > 1e-3 { n / dn } else { lvl0.get(x, y) };
        if !film(i) || level <= 1e-4 { 0.0 } else { 1.0 - v / level }
    });
    // the grain's width in drop: a robust spread over the film (a dusty frame's specks don't widen it)
    let sigma_d = robust_spread(&drop, film).max(0.004);
    // how many grain widths: a speck's core and the edge joined to it, both wider at low sensitivity
    let k_core = 10.0 - 5.0 * s;
    let k_edge = k_core * 0.5;
    let mut marks =
        hysteresis(w, h, |i| drop.data.get(i).is_some_and(|d| *d > k_core * sigma_d), |i| drop.data.get(i).is_some_and(|d| *d > k_edge * sigma_d));
    // faint lines, at half size
    let lines = line_marks(&drop, &marks, &film, s, long);
    for (m, l) in marks.iter_mut().zip(&lines) {
        *m |= *l;
    }
    let mut map = Plane { width: w, height: h, data: marks.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect() };
    // grown (a maximum over the distance) or shrunk (a minimum), at most a twentieth of the long edge
    let reach = expand.clamp(-(long as i32 / 20), long as i32 / 20);
    if reach != 0 {
        map = round_rank(&map, reach.unsigned_abs() as usize, reach > 0);
    }
    Some(map)
}

/// [`rank`] over an octagon about `r` pixels across each way, not a square: a grown speck stays
/// round. A square of half the distance, then a diamond (one 4-neighbour step at a time) of the rest.
fn round_rank(src: &Plane, r: usize, max: bool) -> Plane {
    let square = r * 2 / 5;
    let mut out = rank(src, square, max);
    let (w, h) = (out.width, out.height);
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    for _ in 0..r.saturating_sub(square) {
        let prev = out.clone();
        let at = |x: usize, y: usize| prev.data.get(y * w + x).copied().unwrap_or(0.0);
        for y in 0..h {
            for x in 0..w {
                let mut v = at(x, y);
                if x > 0 {
                    v = pick(v, at(x - 1, y));
                }
                if x + 1 < w {
                    v = pick(v, at(x + 1, y));
                }
                if y > 0 {
                    v = pick(v, at(x, y - 1));
                }
                if y + 1 < h {
                    v = pick(v, at(x, y + 1));
                }
                if let Some(o) = out.data.get_mut(y * w + x) {
                    *o = v;
                }
            }
        }
    }
    out
}

/// A grey closing (a maximum, then a minimum, over a square of side `2r + 1`) of `p`, padded once
/// with its own edge values first: padding each pass on its own would repeat the maximum's border
/// value in the minimum's pass, so a dark strip along the image's edge (a scanner's gate) next to
/// bright film would be filled in.
fn closing(p: &Plane, r: usize) -> Plane {
    let (w, h) = (p.width, p.height);
    if r == 0 || w == 0 || h == 0 {
        return p.clone();
    }
    let (pw, ph) = (w + 2 * r, h + 2 * r);
    let padded = Plane::from_fn(pw, ph, |x, y| p.get(x.saturating_sub(r).min(w - 1), y.saturating_sub(r).min(h - 1)));
    let closed = rank(&rank(&padded, r, true), r, false);
    Plane::from_fn(w, h, |x, y| closed.get(x + r, y + r))
}

/// `p` reduced by a whole factor `f` (the mean of each `f`×`f` cell; a partial cell at the edge too).
fn shrink(p: &Plane, f: usize) -> Plane {
    if f <= 1 {
        return p.clone();
    }
    let (sw, sh) = (p.width.div_ceil(f), p.height.div_ceil(f));
    Plane::from_fn(sw, sh, |x, y| {
        let (mut sum, mut n) = (0.0f32, 0u32);
        for yy in y * f..((y + 1) * f).min(p.height) {
            for xx in x * f..((x + 1) * f).min(p.width) {
                sum += p.get(xx, yy);
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f32 }
    })
}

/// `p` (a [`shrink`] by `f`) back at `w`×`h`, bilinear between cell centres.
fn grow(p: &Plane, f: usize, w: usize, h: usize) -> Plane {
    if f <= 1 && (p.width, p.height) == (w, h) {
        return p.clone();
    }
    let (pw, ph) = (p.width.max(1), p.height.max(1));
    let at = |x: isize, y: isize| p.get(x.clamp(0, pw as isize - 1) as usize, y.clamp(0, ph as isize - 1) as usize);
    Plane::from_fn(w, h, |x, y| {
        let (fx, fy) = ((x as f32 + 0.5) / f as f32 - 0.5, (y as f32 + 0.5) / f as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    })
}

/// 1.4826 × the median absolute deviation of `p` over the pixels `take` keeps (a sample of at most
/// a few hundred thousand): the noise's standard deviation, which a minority of outliers doesn't move.
fn robust_spread(p: &Plane, take: impl Fn(usize) -> bool) -> f32 {
    let step = (p.data.len() / 300_000).max(1);
    let mut v: Vec<f32> =
        (0..p.data.len()).step_by(step).filter(|i| take(*i)).filter_map(|i| p.data.get(i).copied()).filter(|x| x.is_finite()).collect();
    if v.len() < 16 {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    let med = v.get(v.len() / 2).copied().unwrap_or(0.0);
    let mut dev: Vec<f32> = v.iter().map(|x| (x - med).abs()).collect();
    dev.sort_by(f32::total_cmp);
    1.4826 * dev.get(dev.len() / 2).copied().unwrap_or(0.0)
}

/// The pixels of a `w`×`h` image that are `weak` and 8-connected to one that is `strong` (a
/// strong one itself is weak too, as callers make it).
fn hysteresis(w: usize, h: usize, strong: impl Fn(usize) -> bool, weak: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut out = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|i| strong(*i)).collect();
    for &i in &stack {
        if let Some(o) = out.get_mut(i) {
            *o = true;
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if out.get(j).copied() == Some(false) && weak(j) {
                if let Some(o) = out.get_mut(j) {
                    *o = true;
                }
                stack.push(j);
            }
        }
    }
    out
}

/// Directions a line is looked for in.
const LINE_ANGLES: usize = 12;

/// Faint scratches and hairs: on `drop` at half size (specks already in `marks` left out, so they
/// don't feed it), the mean along a stroke about 1.2 % of the long edge in each of
/// [`LINE_ANGLES`] directions, less the mean of the strips a couple of pixels to either side; a
/// stroke far past that response's own spread is a line, extended along what stays well past it,
/// and kept when it is long. Back at full size, a line marks the pixels of it that fall below the
/// level at all, and their neighbours.
fn line_marks(drop: &Plane, marks: &[bool], film: &impl Fn(usize) -> bool, s: f32, long: f32) -> Vec<bool> {
    let (w, h) = (drop.width, drop.height);
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let half = Plane::from_fn(hw, hh, |x, y| {
        let (mut sum, mut n, mut speck) = (0.0f32, 0u32, false);
        for yy in 2 * y..(2 * y + 2).min(h) {
            for xx in 2 * x..(2 * x + 2).min(w) {
                sum += drop.get(xx, yy);
                n += 1;
                speck |= marks.get(yy * w + xx).copied().unwrap_or(false);
            }
        }
        if speck || n == 0 { 0.0 } else { sum / n as f32 }
    });
    let len = ((0.006 * long).round() as i64).clamp(6, 40);
    let mut best = vec![f32::NEG_INFINITY; hw * hh];
    let mut on = vec![0.0f32; hw * hh];
    for k in 0..LINE_ANGLES {
        let a = std::f32::consts::PI * k as f32 / LINE_ANGLES as f32;
        let (dx, dy) = (a.cos(), a.sin());
        let mut taps: Vec<(i64, i64)> = (-len..=len).map(|t| ((t as f32 * dx * 0.5).round() as i64, (t as f32 * dy * 0.5).round() as i64)).collect();
        taps.dedup();
        let nt = taps.len().max(1) as f32;
        {
            use rayon::prelude::*;
            on.par_chunks_mut(hw).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let mut sum = 0.0;
                    for (ox, oy) in &taps {
                        let (sx, sy) = ((x as i64 + ox).clamp(0, hw as i64 - 1) as usize, (y as i64 + oy).clamp(0, hh as i64 - 1) as usize);
                        sum += half.data.get(sy * hw + sx).copied().unwrap_or(0.0);
                    }
                    *o = sum / nt;
                }
            });
        }
        let (nx, ny) = ((-dy * 2.0).round() as i64, (dx * 2.0).round() as i64);
        let at = |x: i64, y: i64| on.get(y.clamp(0, hh as i64 - 1) as usize * hw + x.clamp(0, hw as i64 - 1) as usize).copied().unwrap_or(0.0);
        for y in 0..hh {
            for x in 0..hw {
                let (xi, yi) = (x as i64, y as i64);
                let r = at(xi, yi) - 0.5 * (at(xi + nx, yi + ny) + at(xi - nx, yi - ny));
                if let Some(b) = best.get_mut(y * hw + x)
                    && r > *b
                {
                    *b = r;
                }
            }
        }
    }
    let resp = Plane { width: hw, height: hh, data: best };
    let film_half = |i: usize| {
        let (x, y) = (i % hw, i / hw);
        film((2 * y).min(h - 1) * w + (2 * x).min(w - 1))
    };
    let mut sample: Vec<f32> = (0..resp.data.len())
        .step_by((resp.data.len() / 200_000).max(1))
        .filter(|i| film_half(*i))
        .filter_map(|i| resp.data.get(i).copied())
        .collect();
    sample.sort_by(f32::total_cmp);
    let med = sample.get(sample.len() / 2).copied().unwrap_or(0.0);
    let spread = robust_spread(&resp, film_half).max(1e-4);
    let (k_core, k_edge) = (12.0 - 5.0 * s, 6.0 - 2.5 * s);
    let z = |i: usize| (resp.data.get(i).copied().unwrap_or(0.0) - med) / spread;
    let line = hysteresis(hw, hh, |i| film_half(i) && z(i) > k_core, |i| film_half(i) && z(i) > k_edge);
    // keep the long ones: a component at least a stroke long
    let min_extent = (len as usize).max(6);
    let mut keep = vec![false; hw * hh];
    let mut seen = vec![false; hw * hh];
    let mut stack = Vec::new();
    for start in 0..hw * hh {
        if !line.get(start).copied().unwrap_or(false) || seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut comp = Vec::new();
        stack.clear();
        stack.push(start);
        if let Some(v) = seen.get_mut(start) {
            *v = true;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % hw, i / hw);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            comp.push(i);
            for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= hw as i64 || ny >= hh as i64 {
                    continue;
                }
                let j = ny as usize * hw + nx as usize;
                if line.get(j).copied().unwrap_or(false) && !seen.get(j).copied().unwrap_or(true) {
                    if let Some(v) = seen.get_mut(j) {
                        *v = true;
                    }
                    stack.push(j);
                }
            }
        }
        if (x1 - x0).max(y1 - y0) + 1 >= min_extent {
            for i in comp {
                if let Some(k) = keep.get_mut(i) {
                    *k = true;
                }
            }
        }
    }
    // back at full size: the line's own pixels that dip at all, and one pixel around them
    let core: Vec<bool> = (0..w * h)
        .map(|i| keep.get(((i / w) / 2) * hw + (i % w) / 2).copied().unwrap_or(false) && drop.data.get(i).is_some_and(|d| *d > 0.0))
        .collect();
    let mut out = core.clone();
    for (i, c) in core.iter().enumerate() {
        if !*c {
            continue;
        }
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(1)..(y + 2).min(h) {
            for xx in x.saturating_sub(1)..(x + 2).min(w) {
                if let Some(o) = out.get_mut(yy * w + xx) {
                    *o = true;
                }
            }
        }
    }
    out
}

/// The 8-connected blobs of `candidate` pixels of a `w`×`h` image, each as its pixels with
/// `strength` there; a blob of more than `max_blob` pixels (a bright structure, a whole area) or
/// fewer than 2 (one noisy pixel) is left out.
fn blobs(w: usize, h: usize, max_blob: usize, candidate: impl Fn(usize) -> bool, strength: impl Fn(usize) -> f32) -> Vec<Vec<(usize, usize, f32)>> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if seen.get(start).copied().unwrap_or(true) || !candidate(start) {
            continue;
        }
        let mut px: Vec<(usize, usize, f32)> = Vec::new();
        stack.clear();
        stack.push(start);
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        let mut too_big = false;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            px.push((x, y, strength(i)));
            if px.len() > max_blob {
                too_big = true;
            }
            for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if !seen.get(j).copied().unwrap_or(true) && candidate(j) {
                    if let Some(s) = seen.get_mut(j) {
                        *s = true;
                    }
                    if !too_big {
                        stack.push(j);
                    }
                }
            }
        }
        if !too_big && px.len() >= 2 {
            out.push(px);
        }
    }
    out
}

/// How sure a blob is a defect: guessed from the picture (a bright feature that could be a
/// highlight: strict shape rules, and a line must lie on one background), or read from the
/// scanner's infrared pass (certain: looser sizes, and what fits neither a speck nor a line is a
/// patch).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Certainty {
    Guessed,
    Infrared,
}

/// A blob's shape: a speck when small and roundish, a hair or scratch when long and thin,
/// `None` otherwise (a bright edge or texture).
fn classify(px: &[(usize, usize, f32)], luma: &Plane, long: f32, how: Certainty) -> Option<FilmDefect> {
    let certain = how == Certainty::Infrared;
    let (w, h) = (luma.width, luma.height);
    let n = px.len() as f64;
    let (mx, my) = px.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 as f64, a.1 + p.1 as f64));
    let (mx, my) = (mx / n, my / n);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in px {
        let (dx, dy) = (p.0 as f64 - mx, p.1 as f64 - my);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let (sxx, syy, sxy) = (sxx / n, syy / n, sxy / n);
    // principal axes of the blob
    let tr = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    let (l1, l2) = (tr / 2.0 + disc, (tr / 2.0 - disc).max(0.0));
    let length = 4.0 * l1.sqrt() + 1.0;
    let width = (n / length).max(1.0);
    let strength = px.iter().map(|p| p.2).fold(0.0, f32::max);
    let long = f64::from(long);
    let to_norm = |x: f64, y: f64| ((x + 0.5) / w as f64, (y + 0.5) / h as f64);
    let elongation = (l1 / l2.max(0.05)).sqrt();
    // (the infrared map keeps a mark's soft edge, a few pixels whatever the photo's size)
    let (max_speck, max_width) = if certain { ((0.014 * long).max(10.0), (0.005 * long).max(6.0)) } else { (0.008 * long, 0.003 * long) };
    if elongation < 2.5 && length <= max_speck {
        // a speck: heal a circle a little larger than it
        return Some(FilmDefect { points: vec![to_norm(mx, my)], radius: (length / 2.0 * 1.6 + 1.0) / long, strength, source: None });
    }
    if elongation >= 3.0 && width <= max_width && length >= 0.008 * long {
        // a hair or scratch: its centre line, as the mean position along the main axis in steps
        let (ax, ay) = if sxy.abs() < 1e-12 && sxx >= syy {
            (1.0, 0.0)
        } else if sxy.abs() < 1e-12 {
            (0.0, 1.0)
        } else {
            let (vx, vy) = (l1 - syy, sxy);
            let m = (vx * vx + vy * vy).sqrt();
            (vx / m, vy / m)
        };
        let t: Vec<f64> = px.iter().map(|p| (p.0 as f64 - mx) * ax + (p.1 as f64 - my) * ay).collect();
        let (t0, t1) = t.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, v| (a.0.min(*v), a.1.max(*v)));
        let steps = (((t1 - t0) / (2.0 * width).max(3.0)).ceil() as usize).clamp(2, MAX_PATH_POINTS);
        let mut sums = vec![(0.0f64, 0.0f64, 0usize); steps];
        for (p, tv) in px.iter().zip(&t) {
            let k = (((tv - t0) / (t1 - t0).max(1e-9)) * (steps as f64 - 1.0)).round() as usize;
            if let Some(s) = sums.get_mut(k.min(steps - 1)) {
                s.0 += p.0 as f64;
                s.1 += p.1 as f64;
                s.2 += 1;
            }
        }
        let path: Vec<(f64, f64)> = sums.iter().filter(|s| s.2 > 0).map(|s| (s.0 / s.2 as f64, s.1 / s.2 as f64)).collect();
        if path.len() >= 2 && (certain || same_both_sides(&path, luma, width + 2.0, 0.004 * long, strength)) {
            let points = path.iter().map(|p| to_norm(p.0, p.1)).collect();
            let radius = width * 1.5 + 1.5;
            let source = side_source(&path, luma, (ay, -ax), 2.5 * radius + 2.0).map(|d| (d.0 / w as f64, d.1 / h as f64));
            return Some(FilmDefect { points, radius: radius / long, strength, source });
        }
    }
    if certain {
        // a patch (a fibre's clump, a smudge): one round heal spot covering its extent
        let (x0, x1) = px.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.0 as f64), a.1.max(p.0 as f64)));
        let (y0, y1) = px.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.1 as f64), a.1.max(p.1 as f64)));
        let across = ((x1 - x0 + 1.0).powi(2) + (y1 - y0 + 1.0).powi(2)).sqrt();
        return Some(FilmDefect {
            points: vec![to_norm((x0 + x1) / 2.0, (y0 + y1) / 2.0)],
            radius: (across / 2.0 * 1.25 + 2.0) / long,
            strength,
            source: None,
        });
    }
    None
}

/// The heal source for a line along `path`: `normal * reach` or its opposite (in pixels),
/// whichever lands on the smoother strip of `luma` (the one most like plain background), or
/// `None` when both fall off the image.
fn side_source(path: &[(f64, f64)], luma: &Plane, normal: (f64, f64), reach: f64) -> Option<(f64, f64)> {
    let (w, h) = (luma.width, luma.height);
    let spread = |sign: f64| {
        let vals: Vec<f32> = path
            .iter()
            .filter_map(|p| {
                let (x, y) = ((p.0 + sign * normal.0 * reach).round(), (p.1 + sign * normal.1 * reach).round());
                if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
                    return None;
                }
                luma.data.get(y as usize * w + x as usize).copied()
            })
            .collect();
        if vals.len() < path.len() {
            return None;
        }
        let n = vals.len() as f32;
        let mean = vals.iter().sum::<f32>() / n;
        Some(vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n)
    };
    let sign = match (spread(1.0), spread(-1.0)) {
        (Some(a), Some(b)) => {
            if a <= b {
                1.0
            } else {
                -1.0
            }
        }
        (Some(_), None) => 1.0,
        (None, Some(_)) => -1.0,
        (None, None) => return None,
    };
    Some((sign * normal.0 * reach, sign * normal.1 * reach))
}

/// Whether a bright line lies across one background (a hair or scratch: the photo looks the same
/// on both sides of it) rather than along the border of two areas or as part of a structure (the
/// lit rim of a roof, a railing's top rail, a window frame): the mean `luma` over a strip from
/// `near` to `near + depth` pixels to either side of the path is compared, and the typical
/// difference must be small next to the line's own brightness `strength`.
fn same_both_sides(path: &[(f64, f64)], luma: &Plane, near: f64, depth: f64, strength: f32) -> bool {
    let (w, h) = (luma.width, luma.height);
    let at = |x: f64, y: f64| {
        let (x, y) = (x.round(), y.round());
        if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
            return None;
        }
        luma.data.get(y as usize * w + x as usize).copied()
    };
    let steps = (depth.ceil() as usize).max(2);
    let side = |p: (f64, f64), nx: f64, ny: f64| {
        let mut sum = 0.0;
        for k in 0..steps {
            let d = near + depth * k as f64 / (steps - 1) as f64;
            sum += at(p.0 + nx * d, p.1 + ny * d)?;
        }
        Some(sum / steps as f32)
    };
    let mut diffs: Vec<f32> = (0..path.len())
        .filter_map(|i| {
            let (a, b) = (path.get(i.saturating_sub(1))?, path.get((i + 1).min(path.len() - 1))?);
            let p = *path.get(i)?;
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let m = (dx * dx + dy * dy).sqrt();
            if m < 1e-9 {
                return None;
            }
            let (nx, ny) = (-dy / m, dx / m);
            Some((side(p, nx, ny)? - side(p, -nx, -ny)?).abs())
        })
        .collect();
    if diffs.len() * 2 < path.len() {
        return false; // mostly off the image: can't tell
    }
    diffs.sort_by(f32::total_cmp);
    let typical = diffs.get(diffs.len() * 3 / 4).copied().unwrap_or(f32::INFINITY);
    typical <= (0.25 * strength).max(0.03)
}

/// The long edge `FilmDust::expand` is given at: a mark on a smaller or larger pass is grown by the same fraction.
pub const EXPAND_EDGE: f64 = 2400.0;

/// The defect map of the infrared pass `ir` for `fd` ([`infrared_map`] at `fd`'s sensitivity and
/// expand), kept for the next render: a photo's pass and settings make the same map every time.
pub fn cached_map(ir: &Arc<Plane>, fd: &lcc_develop::FilmDust) -> Option<Arc<Plane>> {
    static MAPS: Mutex<Vec<(u64, Arc<Plane>)>> = Mutex::new(Vec::new());
    let long = ir.width.max(ir.height) as f64;
    let expand = if fd.expand.is_finite() { (fd.expand.clamp(-8.0, 40.0) * long / EXPAND_EDGE).round() as i32 } else { 0 };
    let sensitivity = if fd.sensitivity.is_finite() { fd.sensitivity.clamp(0.0, 100.0) as f32 } else { 50.0 };
    let key = crate::hash_of((Arc::as_ptr(ir) as usize, ir.width, ir.height, ir.data.len(), sensitivity.to_bits(), expand));
    if let Some((_, m)) = MAPS.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key) {
        return Some(m.clone());
    }
    let map = Arc::new(infrared_map(ir, sensitivity, expand)?);
    let mut g = MAPS.lock().unwrap_or_else(PoisonError::into_inner);
    if g.len() >= 4 {
        g.remove(0);
    }
    g.push((key, map.clone()));
    Some(map)
}

/// The marks of the photo's infrared pass as the render `frame` of `w`×`h` sees them (0..1), for
/// `s.film_dust`'s sensitivity and expand; `None` when the photo has no pass.
pub fn mask_for(info: &crate::SourceInfo, s: &lcc_develop::DevelopSettings, frame: &crate::geometry::Frame, w: usize, h: usize) -> Option<Plane> {
    let ir = info.infrared.as_ref()?;
    let map = cached_map(ir, &s.film_dust)?;
    Some(frame.sample_alpha(&map, w, h))
}

/// The Source-stage step: when `s.film_dust` is on and the photo has an infrared pass, the marked
/// pixels of `img` (the source as `frame` samples it, `img.width`×`img.height`) are filled in
/// from their surroundings ([`inpaint`]). True when the image changed.
pub fn stage(img: &mut Rgb32f, info: &crate::SourceInfo, s: &lcc_develop::DevelopSettings, frame: &crate::geometry::Frame) -> bool {
    if !s.film_dust.on() || info.infrared.is_none() {
        return false;
    }
    let Some(mask) = mask_for(info, s, frame, img.width, img.height) else { return false };
    inpaint(img, &mask)
}

/// A pixel whose sampled mark value reaches this is filled in (a speck a pixel wide keeps a
/// footprint when the mask is resampled smaller).
const MASKED: f32 = 0.3;
/// Most rings of filling from the edge inwards: a mark wider than twice this (in render pixels) keeps its middle.
const MAX_RINGS: usize = 96;

/// Fills every pixel of `img` where `mask` ≥ [`MASKED`] from the pixels around it: ring by ring
/// from the mark's edge inwards, each pixel the weighted mean of its already-known neighbours
/// (the 4-neighbours counting twice the diagonals), then a few passes of smoothing over the filled
/// pixels alone, so the fill carries the surroundings' tone and gradient without the seams a
/// single pass leaves. Texture isn't invented: a filled speck is smooth, as a scanner's own
/// infrared cleaning leaves it. False when nothing was marked.
pub fn inpaint(img: &mut Rgb32f, mask: &Plane) -> bool {
    let (w, h) = (img.width, img.height);
    if mask.width != w || mask.height != h || img.data.len() < w * h || mask.data.len() < w * h || w == 0 || h == 0 {
        return false;
    }
    // 0 = known, 1 = to fill, 2 = filled this ring
    let mut state: Vec<u8> = mask.data.iter().take(w * h).map(|m| u8::from(*m >= MASKED)).collect();
    let mut unknown: Vec<usize> = state.iter().enumerate().filter(|(_, s)| **s == 1).map(|(i, _)| i).collect();
    if unknown.is_empty() {
        return false;
    }
    let filled_all: Vec<usize> = unknown.clone();
    let neighbours = |i: usize| -> [(Option<usize>, f32); 8] {
        let (x, y) = (i % w, i / w);
        let at = |dx: i64, dy: i64| {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            (nx >= 0 && ny >= 0 && nx < w as i64 && ny < h as i64).then(|| ny as usize * w + nx as usize)
        };
        [(at(-1, 0), 1.0), (at(1, 0), 1.0), (at(0, -1), 1.0), (at(0, 1), 1.0), (at(-1, -1), 0.5), (at(1, -1), 0.5), (at(-1, 1), 0.5), (at(1, 1), 0.5)]
    };
    for _ in 0..MAX_RINGS {
        if unknown.is_empty() {
            break;
        }
        // this ring: the unknown pixels with a known neighbour, from the image as it stands
        let mut ring: Vec<(usize, [f32; 3])> = Vec::new();
        for &i in &unknown {
            let (mut sum, mut wsum) = ([0.0f32; 3], 0.0f32);
            for (n, wt) in neighbours(i) {
                if let Some(n) = n
                    && state.get(n).copied() == Some(0)
                    && let Some(p) = img.data.get(n)
                {
                    sum = [sum[0] + p[0] * wt, sum[1] + p[1] * wt, sum[2] + p[2] * wt];
                    wsum += wt;
                }
            }
            if wsum > 0.0 {
                ring.push((i, sum.map(|v| v / wsum)));
            }
        }
        if ring.is_empty() {
            break; // nothing known to fill from (a mask over the whole image)
        }
        for (i, v) in &ring {
            if let Some(p) = img.data.get_mut(*i) {
                *p = *v;
            }
            if let Some(s) = state.get_mut(*i) {
                *s = 2;
            }
        }
        for s in state.iter_mut() {
            if *s == 2 {
                *s = 0;
            }
        }
        unknown.retain(|i| state.get(*i).copied() == Some(1));
    }
    // smoothing over the filled pixels: each the mean of its 4-neighbours (known ones as they
    // are), a few Jacobi passes, so the rings' seams go
    let mut next: Vec<(usize, [f32; 3])> = Vec::with_capacity(filled_all.len());
    for _ in 0..4 {
        next.clear();
        for &i in &filled_all {
            let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
            for (j, _) in neighbours(i).iter().take(4) {
                if let Some(p) = j.and_then(|j| img.data.get(j)) {
                    sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                    n += 1.0;
                }
            }
            if n > 0.0 {
                next.push((i, sum.map(|v| v / n)));
            }
        }
        for (i, v) in &next {
            if let Some(p) = img.data.get_mut(*i) {
                *p = *v;
            }
        }
    }
    // the film's grain, which a smooth fill lacks: each filled pixel takes the fine detail (the
    // image less its slight blur) of a clean pixel a short way off, so a filled speck is as grainy
    // as the film around it
    let fine = lcc_raster::blur::gaussian(img, 1.2);
    let offsets: [(i64, i64); 8] = [(11, 5), (-9, 8), (7, -12), (-12, -6), (15, -2), (-3, 14), (4, 16), (-15, 3)];
    let marked = |i: usize| mask.data.get(i).is_some_and(|m| *m >= MASKED);
    let mut grain: Vec<(usize, [f32; 3])> = Vec::with_capacity(filled_all.len());
    for &i in &filled_all {
        let (x, y) = ((i % w) as i64, (i / w) as i64);
        // (a different start per pixel row and column, so the borrowed grain doesn't repeat as a pattern)
        let start = ((x * 7 + y * 13) as usize) % offsets.len();
        for k in 0..offsets.len() {
            let (ox, oy) = offsets[(start + k) % offsets.len()];
            let (sx, sy) = (x + ox, y + oy);
            if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                continue;
            }
            let j = sy as usize * w + sx as usize;
            if marked(j) {
                continue;
            }
            if let (Some(p), Some(b)) = (img.data.get(j), fine.data.get(j)) {
                grain.push((i, [p[0] - b[0], p[1] - b[1], p[2] - b[2]]));
            }
            break;
        }
    }
    for (i, g) in grain {
        if let Some(p) = img.data.get_mut(i) {
            *p = [(p[0] + g[0]).max(0.0), (p[1] + g[1]).max(0.0), (p[2] + g[2]).max(0.0)];
        }
    }
    true
}

/// Whether a render of `s` must take the CPU pipeline for film dust: the develop step is on for a
/// photo with an infrared pass, or `overlay` shows the pass or its marks (the GPU renderer does neither).
pub fn needs_cpu(info: &crate::SourceInfo, s: &lcc_develop::DevelopSettings, overlay: crate::Overlay) -> bool {
    info.infrared.is_some() && (s.film_dust.on() || matches!(overlay, crate::Overlay::FilmDust | crate::Overlay::Infrared))
}

/// The infrared pass as the render `frame` of `w`×`h` sees it, for the Infrared view.
pub fn infrared_for(info: &crate::SourceInfo, frame: &crate::geometry::Frame, w: usize, h: usize) -> Option<Plane> {
    let ir = info.infrared.as_ref()?;
    Some(frame.sample_alpha(ir, w, h))
}

/// The Infrared view: `ir` (the pass at the image's size) in grey, stretched from its darkest to
/// its brightest film, with `marks` (what the develop step fills) tinted red over it.
pub fn paint_infrared(img: &mut Rgba8, ir: &Plane, marks: Option<&Plane>) {
    if (ir.width, ir.height) != (img.width, img.height) {
        return;
    }
    let step = (ir.data.len() / 100_000).max(1);
    let mut v: Vec<f32> = ir.data.iter().step_by(step).copied().filter(|x| x.is_finite() && *x > 0.01).collect();
    v.sort_by(f32::total_cmp);
    let lo = v.get(v.len() / 200).copied().unwrap_or(0.0);
    let hi = v.get(v.len().saturating_sub(1).saturating_sub(v.len() / 200)).copied().unwrap_or(1.0);
    let span = (hi - lo).max(1e-4);
    for (i, p) in img.data.iter_mut().enumerate() {
        let g = ((ir.data.get(i).copied().unwrap_or(0.0) - lo) / span).clamp(0.0, 1.0) * 255.0;
        let m = marks.and_then(|m| m.data.get(i)).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let tint = |c: f32, t: f32| (c + (t - c) * m * 0.8).round().clamp(0.0, 255.0) as u8;
        *p = [tint(g, 255.0), tint(g, 40.0), tint(g, 30.0), p[3]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth grey gradient (a sky) with a busy textured strip along the bottom.
    fn scene(w: usize, h: usize) -> Rgba8 {
        let mut img = Rgba8::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let base = 120.0 + 60.0 * x as f32 / w as f32;
                let v = if y > h * 3 / 4 {
                    // texture: a fine high-contrast pattern, as foliage
                    let n = ((x * 7919 + y * 104_729) % 97) as f32 / 97.0;
                    base - 40.0 + 80.0 * n
                } else {
                    base
                };
                let v = v.clamp(0.0, 255.0) as u8;
                img.data[y * w + x] = [v, v, v, 255];
            }
        }
        img
    }

    fn dab(img: &mut Rgba8, cx: usize, cy: usize, r: i64) {
        let w = img.width;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    let (x, y) = ((cx as i64 + dx) as usize, (cy as i64 + dy) as usize);
                    img.data[y * w + x] = [250, 250, 250, 255];
                }
            }
        }
    }

    #[test]
    fn bright_specks_and_a_hair_on_a_smooth_area_are_found_and_texture_is_left() {
        let (w, h) = (800, 600);
        let mut img = scene(w, h);
        for (x, y) in [(100, 100), (300, 80), (500, 200), (650, 120)] {
            dab(&mut img, x, y, 1);
        }
        // a hair: a thin bright arc across the sky
        for i in 0..1200 {
            let t = i as f32 / 1200.0;
            let (x, y) = (150.0 + 400.0 * t, 300.0 + 60.0 * (t * 3.0).sin());
            img.data[y as usize * w + x as usize] = [245, 245, 245, 255];
            img.data[(y as usize + 1) * w + x as usize] = [245, 245, 245, 255];
        }
        let found = detect(&img, 50.0);
        let specks: Vec<_> = found.iter().filter(|d| !d.is_line()).collect();
        let hairs: Vec<_> = found.iter().filter(|d| d.is_line()).collect();
        for (x, y) in [(100, 100), (300, 80), (500, 200), (650, 120)] {
            let (nx, ny) = (x as f64 / w as f64, y as f64 / h as f64);
            assert!(
                specks.iter().any(|d| (d.points[0].0 - nx).abs() < 0.01 && (d.points[0].1 - ny).abs() < 0.01),
                "speck at {x},{y} missed: {found:?}"
            );
        }
        assert!(!hairs.is_empty(), "the hair was missed: {found:?}");
        // the hair's path runs along it, end to end
        let longest = hairs.iter().map(|d| d.points.len()).max().unwrap_or(0);
        assert!(longest >= 5, "a hair should be a path: {hairs:?}");
        // the textured strip gives (almost) nothing
        let in_texture = found.iter().filter(|d| d.points.iter().any(|p| p.1 > 0.76)).count();
        assert!(in_texture <= 2, "texture taken for dust: {in_texture}");
    }

    #[test]
    fn a_clean_photo_and_a_bright_object_give_nothing() {
        let (w, h) = (640, 480);
        let mut img = scene(w, h);
        // a large bright object (a white wall, the sun): not dust
        for y in 50..200 {
            for x in 300..500 {
                img.data[y * w + x] = [250, 250, 250, 255];
            }
        }
        assert!(detect(&img, 50.0).is_empty());
        assert!(detect(&scene(w, h), 100.0).len() <= 2);
    }

    /// An infrared pass: flat with a gentle falloff, a few dark specks, a thin dark hair and a
    /// large dark patch, over a picture that shows none of them (the specks are read from the
    /// pass alone); the dark gate around the frame gives nothing.
    #[test]
    fn the_infrared_pass_gives_specks_a_hair_a_patch_and_nothing_at_the_gate() {
        let (w, h) = (800, 600);
        let luma = Plane::from_fn(w, h, |x, _| 0.3 + 0.4 * x as f32 / w as f32);
        let mut ir = Plane::from_fn(w, h, |x, y| if x < 30 || y < 30 || x >= w - 30 || y >= h - 30 { 0.01 } else { 0.6 - 0.1 * y as f32 / h as f32 });
        let dip = |ir: &mut Plane, cx: usize, cy: usize, r: i64, v: f32| {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy <= r * r {
                        ir.data[(cy as i64 + dy) as usize * w + (cx as i64 + dx) as usize] = v;
                    }
                }
            }
        };
        for (x, y) in [(100, 100), (300, 80), (500, 200), (650, 120)] {
            dip(&mut ir, x, y, 1, 0.05);
        }
        for i in 0..1200 {
            let t = i as f32 / 1200.0;
            let (x, y) = (150.0 + 400.0 * t, 350.0 + 60.0 * (t * 3.0).sin());
            ir.data[y as usize * w + x as usize] = 0.2;
            ir.data[(y as usize + 1) * w + x as usize] = 0.2;
        }
        dip(&mut ir, 400, 500, 4, 0.1);
        let found = detect_infrared(&ir, &luma, 50.0);
        let specks: Vec<_> = found.iter().filter(|d| !d.is_line()).collect();
        for (x, y) in [(100, 100), (300, 80), (500, 200), (650, 120)] {
            let (nx, ny) = (x as f64 / w as f64, y as f64 / h as f64);
            assert!(
                specks.iter().any(|d| (d.points[0].0 - nx).abs() < 0.01 && (d.points[0].1 - ny).abs() < 0.01),
                "speck at {x},{y} missed: {found:?}"
            );
        }
        assert!(found.iter().any(|d| d.is_line() && d.points.len() >= 5), "the hair was missed: {found:?}");
        let patch = specks.iter().find(|d| (d.points[0].0 - 0.5).abs() < 0.01 && (d.points[0].1 - 500.0 / 600.0).abs() < 0.01);
        assert!(patch.is_some_and(|p| p.radius * w as f64 >= 5.0), "the patch should be one spot covering it: {patch:?}");
        // nothing at the gate's edge, and nothing on a clean pass
        assert!(found.iter().all(|d| d.points.iter().all(|p| p.0 > 0.03 && p.0 < 0.97 && p.1 > 0.03 && p.1 < 0.97)), "{found:?}");
        let clean = Plane::from_fn(w, h, |_, y| 0.6 - 0.1 * y as f32 / h as f32);
        assert!(detect_infrared(&clean, &luma, 100.0).is_empty());
    }

    /// Expand grows every mark of the map by that many pixels (and a negative value shrinks it),
    /// so a heal takes in a mark's soft edge; a map only grows where there is a mark.
    #[test]
    fn expand_grows_the_marks_and_nothing_else() {
        let (w, h) = (400, 300);
        let mut ir = Plane::from_fn(w, h, |_, _| 0.6);
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                ir.data[(150 + dy) as usize * w + (200 + dx) as usize] = 0.05;
            }
        }
        // the marked pixels' count and bounding box (x0, y0, x1, y1)
        let marked = |e: i32| {
            let m = infrared_map(&ir, 50.0, e).unwrap();
            let pts: Vec<(usize, usize)> = (0..w * h).filter(|i| m.data[*i] > 0.0).map(|i| (i % w, i / w)).collect();
            let bbox = pts.iter().fold((usize::MAX, usize::MAX, 0, 0), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)));
            (pts.len(), bbox)
        };
        let ((plain, pb), (grown, gb), (shrunk, _)) = (marked(0), marked(3), marked(-1));
        assert!(plain >= 9 && grown > plain && shrunk < plain, "{plain} {grown} {shrunk}");
        // the speck's own soft edge counts, and growing adds at most the distance on each side
        assert!(pb.0 <= 199 && pb.2 >= 201 && pb.1 <= 149 && pb.3 >= 151, "the speck itself: {pb:?}");
        assert!(gb.0 + 3 >= pb.0 && gb.2 <= pb.2 + 3 && gb.1 + 3 >= pb.1 && gb.3 <= pb.3 + 3, "grown only around the mark: {gb:?} vs {pb:?}");
        assert_eq!(infrared_map(&Plane::from_fn(w, h, |_, _| 0.6), 50.0, 5).unwrap().data.iter().filter(|v| **v > 0.0).count(), 0);
    }

    /// A grainy pass with a dark scanner gate along the left edge (its border a few pixels from
    /// bright film, the layout of a real scan): nothing in the gate is marked, and a speck on the film is.
    #[test]
    fn the_scanner_gate_is_never_marked() {
        let (w, h) = (600, 400);
        let noise = |x: usize, y: usize| (((x * 7919 + y * 104_729) % 97) as f32 / 97.0 - 0.5) * 0.03;
        let mut ir = Plane::from_fn(w, h, |x, y| if x < 24 { 0.02 } else { 0.6 + noise(x, y) });
        for dy in 0..3 {
            for dx in 0..3 {
                ir.data[(200 + dy) * w + 300 + dx] = 0.05;
            }
        }
        let m = infrared_map(&ir, 100.0, 2).unwrap();
        let in_gate = (0..h).flat_map(|y| (0..24).map(move |x| (x, y))).filter(|(x, y)| m.get(*x, *y) > 0.0).count();
        assert_eq!(in_gate, 0, "marks in the gate");
        assert!(m.get(301, 201) > 0.0, "the speck on the film");
    }

    #[test]
    fn hostile_input_never_panics() {
        for (w, h) in [(0, 0), (1, 1), (63, 63), (64, 64), (1000, 64)] {
            let img = Rgba8::new(w, h);
            let _ = detect(&img, 50.0);
        }
        let short = Rgba8 { width: 100, height: 100, data: vec![[0; 4]; 10] };
        assert!(detect(&short, 50.0).is_empty());
        let _ = detect(&scene(200, 200), f32::NAN);
        for (w, h) in [(0, 0), (1, 1), (63, 63), (64, 64)] {
            let _ = detect_infrared(&Plane::new(w, h), &Plane::new(w, h), 50.0);
        }
        let _ = detect_infrared(&Plane::new(100, 100), &Plane::new(90, 100), 50.0);
        let short = Plane { width: 100, height: 100, data: vec![0.0; 10] };
        assert!(detect_infrared(&short, &Plane::new(100, 100), 50.0).is_empty());
        assert!(infrared_map(&short, 50.0, 0).is_none());
        let _ = detect_infrared(&Plane::from_fn(200, 200, |_, _| f32::NAN), &Plane::new(200, 200), f32::NAN);
        let _ = infrared_map(&Plane::from_fn(200, 200, |_, _| 0.5), 50.0, i32::MIN);
    }

    /// Marked pixels take the tone of their surroundings: a dark speck and a dark line on a smooth
    /// gradient vanish, nothing else moves, and an empty or mismatched mask changes nothing.
    #[test]
    fn inpaint_fills_the_marked_pixels_from_their_surroundings() {
        let (w, h) = (120, 80);
        let clean = Rgb32f::from_fn(w, h, |x, y| [0.2 + 0.5 * x as f32 / w as f32, 0.4, 0.1 + 0.3 * y as f32 / h as f32]);
        let mut img = clean.clone();
        let mut mask = Plane::new(w, h);
        for (x, y) in (0..40).map(|i| (40 + i, 30 + i / 4)).chain([(20, 20), (21, 20), (20, 21), (21, 21)]) {
            img.data[y * w + x] = [0.0; 3];
            mask.data[y * w + x] = 1.0;
        }
        assert!(inpaint(&mut img, &mask));
        let worst = img.data.iter().zip(&clean.data).map(|(a, b)| (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
        assert!(worst < 0.03, "the marks should take the gradient's tone: worst {worst}");
        let mut again = clean.clone();
        assert!(!inpaint(&mut again, &Plane::new(w, h)));
        assert_eq!(again.data, clean.data);
        assert!(!inpaint(&mut again, &Plane::new(w + 1, h)));
        // a mask over everything: nothing known to fill from, no panic
        let mut all = clean.clone();
        let _ = inpaint(&mut all, &Plane::from_fn(w, h, |_, _| 1.0));
        let _ = inpaint(&mut Rgb32f::new(0, 0), &Plane::new(0, 0));
    }
}
