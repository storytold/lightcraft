//! Automatic Sky, Subject and Landscape (water, vegetation) selections without a model.
//!
//! Each selection is computed once per photo look on a small proxy of the whole, uncropped,
//! oriented and white-balanced photo ([`proxy`], long edge [`EDGE`]) and kept as a grid over the
//! uncropped photo's normalized coordinates (like a stored SAM 3 segmentation): previews,
//! zoomed windows and exports of any size and crop sample the same grid, and an edge-aware
//! refinement at output resolution snaps it to the photo's edges.
//!
//! - **Sky:** a colour model of the sky, seeded from smooth, plausible (blue, bright neutral or
//!   dark night) pixels near the top and refit from the result twice; each column is split at
//!   the row that best separates sky-like pixels above from the rest below (maximum of the
//!   running sum of `2·skyness − 1`), and strongly non-sky pixels above the split are left out
//!   (branches, poles).
//! - **Subject:** boundary-prior saliency: colours unlike those along the frame's border, sharper
//!   than their surroundings and nearer the centre; thresholded (Otsu) and kept to the
//!   connected regions that hold most of the saliency.
//! - **Water:** below the sky line, smooth and coloured like the sky (reflections, a lake under
//!   a blue sky) or blue-green and smooth. **Vegetation:** green, below the sky line.
//!
//! Sources: own design; the column split follows the idea of skyline detection by a per-column
//! boundary (Lie et al., "A robust dynamic programming algorithm to extract skyline in images
//! for navigation", Pattern Recognition Letters 2005, prose); the subject's boundary prior
//! follows Zhu et al., "Saliency Optimization from Robust Background Detection", CVPR 2014
//! (paper); Otsu's threshold (IEEE SMC 1979).

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

use lcc_develop::{DevelopSettings, MaskShape};
use lcc_geom::Point;
use lcc_raster::{Plane, Rgb32f};

use crate::SourceInfo;
use crate::geometry::Frame;

/// Sky continuity: the Oklab step between vertical neighbours at which the sky stops carrying on,
/// and how much of it is kept per row.
const CONT: f32 = 0.012;
const DECAY: f32 = 0.998;
/// Bonus (in rows, as a share of the height) for ending the sky at an edge.
const EDGE_BONUS: f32 = 0.05;

/// Long edge of the analysed proxy (pixels).
pub const EDGE: usize = 256;

/// Selections of one photo over its uncropped, oriented frame (each `0..1`, grid of the proxy).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMasks {
    pub sky: Plane,
    pub subject: Plane,
    pub water: Plane,
    pub vegetation: Plane,
}

impl AutoMasks {
    /// The grid a shape uses, if it has one here.
    pub fn for_shape(&self, shape: &MaskShape) -> Option<&Plane> {
        match shape {
            MaskShape::Sky => Some(&self.sky),
            MaskShape::Subject | MaskShape::People { .. } | MaskShape::Object { .. } => Some(&self.subject),
            MaskShape::Landscape { class } => {
                let c = class.to_ascii_lowercase();
                if c.contains("sky") {
                    Some(&self.sky)
                } else if c.contains("water") {
                    Some(&self.water)
                } else if c.contains("vegetation") || c.contains("plant") || c.contains("tree") {
                    Some(&self.vegetation)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Whether `shape` would use an automatic selection (no stored segmentation of its own).
pub fn wants(shape: &MaskShape) -> bool {
    match shape {
        MaskShape::Sky | MaskShape::Subject | MaskShape::Background | MaskShape::People { .. } | MaskShape::Landscape { .. } => true,
        MaskShape::Object { seg, detail, hint, .. } => seg.is_none() && detail.is_empty() && !hint.is_empty(),
        _ => false,
    }
}

/// Whether any mask (or component) of `s` uses an automatic selection.
pub fn needed(s: &DevelopSettings) -> bool {
    s.masks.iter().any(|m| m.components.iter().any(|c| wants(&c.shape)))
}

/// The selection grid sampled at output pixel `(x, y)` of a `w × h` render of `frame`.
pub fn sample(grid: &Plane, frame: &Frame, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    if grid.width == 0 || grid.height == 0 {
        return out;
    }
    let m = frame.out_to_norm(w, h);
    let (gw, gh) = (grid.width as f32, grid.height as f32);
    crate::for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            *v = bilinear(grid, n.x as f32 * gw - 0.5, n.y as f32 * gh - 0.5);
        }
    });
    out
}

fn bilinear(p: &Plane, fx: f32, fy: f32) -> f32 {
    let fx = fx.clamp(0.0, (p.width - 1) as f32);
    let fy = fy.clamp(0.0, (p.height - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(p.width - 1), (y0 + 1).min(p.height - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |x: usize, y: usize| p.data.get(y * p.width + x).copied().unwrap_or(0.0);
    let a = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
    let b = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
    a + (b - a) * ty
}

/// The whole uncropped, oriented photo at [`EDGE`] px, white-balanced (what the analysis sees).
pub fn proxy(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Rgb32f {
    let frame = crate::frame_for(src, info, s, false);
    let (w, h) = frame.fit(EDGE, EDGE);
    let mut img = frame.sample(src, w.max(1), h.max(1));
    crate::local::white_balance(&mut img, info, s);
    img
}

/// Recently analysed photos (a key per source pixels, framing and white balance).
static CACHE: Mutex<Vec<(u64, Arc<AutoMasks>)>> = Mutex::new(Vec::new());
const CACHE_LEN: usize = 8;

/// [`analyze`] of `src` under `s`, cached: a slider drag or a zoomed window reuses it.
pub fn for_source(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Arc<AutoMasks> {
    let frame = crate::frame_for(src, info, s, false);
    let (t, tint) = crate::local::effective_wb(info, s);
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (src.width, src.height, format!("{frame:?}"), t.to_bits(), tint.to_bits()).hash(&mut hs);
    // the pixels themselves (strided: ~4096 samples)
    let step = (src.data.len() / 4096).max(1);
    for c in src.data.iter().step_by(step) {
        c.map(f32::to_bits).hash(&mut hs);
    }
    let key = hs.finish();
    if let Some((_, a)) = CACHE.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key) {
        return a.clone();
    }
    let mut analysed = analyze(&proxy(src, info, s));
    if let Some(subject) = model_subject(src, info, s) {
        analysed.subject = subject;
    }
    let a = Arc::new(analysed);
    let mut c = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if c.len() >= CACHE_LEN {
        c.remove(0);
    }
    c.push((key, a.clone()));
    a
}

/// A segmentation model for the Subject (and, inverted, Background) selection: given the whole
/// uncropped, oriented photo as 8-bit sRGB (`w × h`, row-major RGB), a `0..1` grid of any size
/// covering the same frame, or `None` when it can't (the photo analysis is used then). The engine
/// installs one when the model is installed ([`set_subject_model`]); this layer only calls it.
pub type SubjectModel = dyn Fn(&[u8], usize, usize) -> Option<Plane> + Send + Sync;

static SUBJECT_MODEL: std::sync::RwLock<Option<Arc<SubjectModel>>> = std::sync::RwLock::new(None);
/// Model selections by photo (pixels and framing; white balance left out, so a WB drag never
/// re-runs the model).
static SUBJECT_CACHE: Mutex<Vec<(u64, Plane)>> = Mutex::new(Vec::new());
/// Held while the model runs: a second render of the same photo waits and takes the cached result.
static SUBJECT_BUSY: Mutex<()> = Mutex::new(());

/// Long edge of the image the subject model is given.
pub const MODEL_EDGE: usize = 1024;

/// Install (or with `None` remove) the subject model; cached model selections are dropped.
pub fn set_subject_model(m: Option<Arc<SubjectModel>>) {
    *SUBJECT_MODEL.write().unwrap_or_else(PoisonError::into_inner) = m;
    SUBJECT_CACHE.lock().unwrap_or_else(PoisonError::into_inner).clear();
    CACHE.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

/// Whether a subject model is installed.
pub fn has_subject_model() -> bool {
    SUBJECT_MODEL.read().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// The photo as the subject model sees it: framed, exposure-normalized to its median, a gentle
/// display curve, sRGB primaries and encoding.
pub fn model_input(img: &Rgb32f) -> Vec<u8> {
    let mut ys: Vec<f32> = img.data.iter().map(|c| 0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2]).collect();
    let k = ys.len() / 2;
    let gain = if ys.is_empty() {
        1.0
    } else {
        ys.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
        0.18 / ys.get(k).copied().unwrap_or(0.18).max(1e-4)
    };
    let mut out = Vec::with_capacity(img.data.len() * 3);
    for c in &img.data {
        // linear Rec.2020 → linear sRGB (ITU-R BT.2087)
        let [r, g, b] = c.map(|v| (v * gain).max(0.0));
        let srgb = [1.6605 * r - 0.5876 * g - 0.0728 * b, -0.1246 * r + 1.1329 * g - 0.0083 * b, -0.0182 * r - 0.1006 * g + 1.1187 * b];
        for v in srgb {
            let v = v.max(0.0);
            out.push(lcc_color::transfer::encode_srgb8((1.5 * v / (1.0 + v)).min(1.0)));
        }
    }
    out
}

/// The installed model's Subject selection of `src` under `s` (cached), if there is a model.
fn model_subject(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<Plane> {
    let model = SUBJECT_MODEL.read().unwrap_or_else(PoisonError::into_inner).clone()?;
    let frame = crate::frame_for(src, info, s, false);
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (src.width, src.height, format!("{frame:?}")).hash(&mut hs);
    let step = (src.data.len() / 4096).max(1);
    for c in src.data.iter().step_by(step) {
        c.map(f32::to_bits).hash(&mut hs);
    }
    let key = hs.finish();
    let cached = || SUBJECT_CACHE.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone());
    if let Some(p) = cached() {
        return Some(p);
    }
    let _busy = SUBJECT_BUSY.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(p) = cached() {
        return Some(p);
    }
    let (w, h) = frame.fit(MODEL_EDGE, MODEL_EDGE);
    let (w, h) = (w.max(1), h.max(1));
    let img = frame.sample(src, w, h);
    let rgb = model_input(&img);
    let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| model(&rgb, w, h))).ok().flatten()?;
    if p.width == 0 || p.height == 0 || p.data.len() != p.width * p.height {
        return None;
    }
    let mut c = SUBJECT_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if c.len() >= CACHE_LEN {
        c.remove(0);
    }
    c.push((key, p.clone()));
    Some(p)
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    // lightness at half weight: a sky's gradient changes it more than its hue
    0.25 * (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// A few k-means centres of `samples` (deterministic: seeded along the sorted lightness).
fn centres(samples: &[[f32; 3]], k: usize) -> Vec<[f32; 3]> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut c: Vec<[f32; 3]> = (0..k).filter_map(|i| sorted.get((2 * i + 1) * sorted.len() / (2 * k)).copied()).collect();
    for _ in 0..6 {
        let mut sum = vec![[0.0f32; 3]; c.len()];
        let mut n = vec![0usize; c.len()];
        for s in samples {
            let j = (0..c.len()).min_by(|&a, &b| d2(*s, c[a]).total_cmp(&d2(*s, c[b]))).unwrap_or(0);
            if let (Some(acc), Some(cnt)) = (sum.get_mut(j), n.get_mut(j)) {
                for (a, v) in acc.iter_mut().zip(s) {
                    *a += v;
                }
                *cnt += 1;
            }
        }
        for ((cj, s), n) in c.iter_mut().zip(&sum).zip(&n) {
            if *n > 0 {
                *cj = s.map(|v| v / *n as f32);
            }
        }
    }
    c
}

fn nearest(c: &[[f32; 3]], p: [f32; 3]) -> f32 {
    c.iter().map(|q| d2(p, *q)).fold(f32::MAX, f32::min).sqrt()
}

/// Analyse an oriented, white-balanced linear Rec.2020 photo (a [`proxy`]).
pub fn analyze(img: &Rgb32f) -> AutoMasks {
    let (w, h) = (img.width, img.height);
    let empty = || Plane::new(w, h);
    if w < 4 || h < 4 {
        return AutoMasks { sky: empty(), subject: empty(), water: empty(), vegetation: empty() };
    }
    // exposure-normalize to the photo's median brightness, then a display-like curve
    let mut ys: Vec<f32> = img.data.iter().map(|c| 0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2]).collect();
    let k = ys.len() / 2;
    ys.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    let gain = 0.18 / ys.get(k).copied().unwrap_or(0.18).max(1e-4);
    let lab: Vec<[f32; 3]> =
        img.data.iter().map(|c| lcc_color::perceptual::oklab_from_2020(crate::masks::tonemap_for_select(c.map(|v| (v * gain).max(0.0))))).collect();
    let l = Plane { width: w, height: h, data: lab.iter().map(|p| p[0]).collect() };
    // texture: deviation from a small blur of lightness
    let blur = lcc_raster::blur::gaussian(&l, 1.5);
    let tex_raw = l.zip_map(&blur, |a, b| (a - b).abs());
    let tex = lcc_raster::blur::gaussian(&tex_raw, 1.0);
    let smoothness: Vec<f32> = tex.data.iter().map(|t| 1.0 - smooth(0.006, 0.03, *t)).collect();

    let sky = sky(&lab, &smoothness, w, h);
    let subject = subject(&lab, &tex, w, h);
    let (water, vegetation) = landscape(&lab, &smoothness, &sky, w, h);
    AutoMasks { sky: sky.alpha, subject, water, vegetation }
}

struct Sky {
    alpha: Plane,
    /// Per column, the row the sky ends at.
    line: Vec<usize>,
    centres: Vec<[f32; 3]>,
}

/// How plausible a colour is for a sky: blue, bright and neutral, or dark (night).
fn sky_colour(p: [f32; 3]) -> f32 {
    let chroma = (p[1] * p[1] + p[2] * p[2]).sqrt();
    let blue = smooth(0.0, 0.05, -p[2]) * (1.0 - smooth(0.04, 0.1, p[1].abs()));
    let neutral_bright = smooth(0.55, 0.8, p[0]) * (1.0 - smooth(0.04, 0.1, chroma));
    let dark = 1.0 - smooth(0.15, 0.3, p[0]);
    // warm sunset skies are plausible near the top only through the colour model
    blue.max(neutral_bright).max(dark).max(0.35)
}

fn sky(lab: &[[f32; 3]], smoothness: &[f32], w: usize, h: usize) -> Sky {
    let at = |x: usize, y: usize| lab.get(y * w + x).copied().unwrap_or([0.0; 3]);
    let sm = |x: usize, y: usize| smoothness.get(y * w + x).copied().unwrap_or(0.0);
    // seed: smooth, plausible pixels of the top band
    let band = (h / 10).max(2);
    let mut seed = Vec::new();
    for y in 0..band {
        for x in 0..w {
            let p = at(x, y);
            if sm(x, y) > 0.6 && sky_colour(p) > 0.5 {
                seed.push(p);
            }
        }
    }
    let mut line = vec![0usize; w];
    let mut centres_now = centres(&seed, 4);
    let sigma = 0.07f32;
    let skyness = |c: &[[f32; 3]], x: usize, y: usize| {
        let p = at(x, y);
        let near = (-(nearest(c, p) / sigma).powi(2)).exp();
        near * (0.3 + 0.7 * sm(x, y)) * sky_colour(p).sqrt()
    };
    if centres_now.is_empty() {
        return Sky { alpha: Plane::new(w, h), line, centres: centres_now };
    }
    for pass in 0..3 {
        for (x, b) in line.iter_mut().enumerate() {
            let (mut run, mut best, mut at_best) = (0.0f32, 0.0f32, 0usize);
            let mut prev = 0.0f32;
            for y in 0..h {
                // a smooth gradient carries the sky on from the pixel above (sunset skies
                // turn warm towards the horizon, away from the colours seen at the top)
                let cont = if y > 0 { (-(d2(at(x, y), at(x, y - 1)).sqrt() / CONT).powi(2)).exp() * sm(x, y) } else { 0.0 };
                let s = skyness(&centres_now, x, y).max(prev * cont * DECAY);
                prev = s;
                run += 2.0 * s - 1.0;
                // the sky ends at an edge more likely than in a smooth stretch
                let edge = if y + 1 < h { (d2(at(x, y + 1), at(x, y)).sqrt() / 0.04).min(1.0) } else { 1.0 };
                let score = run + EDGE_BONUS * h as f32 * edge;
                if score > best {
                    best = score;
                    at_best = y + 1;
                }
            }
            *b = at_best;
        }
        if pass == 2 {
            break;
        }
        // refit the colour model to what is now sky (sampled: at most ~8000 pixels)
        let total: usize = line.iter().sum();
        let step = (total / 8000).max(1);
        let mut samples = Vec::new();
        let mut i = 0usize;
        for (x, b) in line.iter().enumerate() {
            for y in 0..*b {
                if i.is_multiple_of(step) && sm(x, y) > 0.3 {
                    samples.push(at(x, y));
                }
                i += 1;
            }
        }
        if samples.len() > 32 {
            centres_now = centres(&samples, 6);
        }
    }
    let mut alpha = Plane::new(w, h);
    for (x, b) in line.iter().enumerate() {
        for y in 0..*b {
            let s = skyness(&centres_now, x, y);
            if let Some(v) = alpha.data.get_mut(y * w + x) {
                // inside the sky, only clearly foreign pixels (branches, wires) are left out
                *v = smooth(0.02, 0.15, s);
            }
        }
    }
    let alpha = lcc_raster::blur::gaussian(&alpha, 0.7);
    Sky { alpha, line, centres: centres_now }
}

fn subject(lab: &[[f32; 3]], tex: &Plane, w: usize, h: usize) -> Plane {
    let at = |x: usize, y: usize| lab.get(y * w + x).copied().unwrap_or([0.0; 3]);
    // background model: the frame's border
    let bw = (w.min(h) / 25).max(1);
    let mut border = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if x < bw || y < bw || x >= w - bw || y >= h - bw {
                border.push(at(x, y));
            }
        }
    }
    let bg = centres(&border, 8);
    // sharpness, relative to the photo's own
    let mut tsorted: Vec<f32> = tex.data.clone();
    let k = (tsorted.len() * 9 / 10).min(tsorted.len().saturating_sub(1));
    tsorted.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    let t90 = tsorted.get(k).copied().unwrap_or(1.0).max(1e-4);
    let sharp = lcc_raster::blur::gaussian(&tex.map(|t| (t / t90).min(1.5)), (w.max(h) as f32) * 0.02);
    let mut sal = Plane::new(w, h);
    let (cx, cy) = (w as f32 / 2.0, h as f32 * 0.5);
    let r = w.max(h) as f32;
    for y in 0..h {
        for x in 0..w {
            let d = nearest(&bg, at(x, y));
            let contrast = smooth(0.02, 0.15, d);
            let dc = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / r;
            let centre = (-(dc / 0.45).powi(2)).exp();
            let s = sharp.data.get(y * w + x).copied().unwrap_or(0.0);
            if let Some(v) = sal.data.get_mut(y * w + x) {
                *v = contrast * centre * (0.5 + 0.5 * s.min(1.0));
            }
        }
    }
    let sal = lcc_raster::blur::gaussian(&sal, (w.max(h) as f32) * 0.008);
    let t = otsu(&sal.data);
    let mut bin: Vec<bool> = sal.data.iter().map(|v| *v > t).collect();
    keep_main_regions(&mut bin, &sal.data, w, h);
    let mut out = Plane { width: w, height: h, data: bin.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect() };
    // soft edge proportional to the frame
    out = lcc_raster::blur::gaussian(&out, (w.max(h) as f32) * 0.004);
    out
}

/// Otsu's threshold of values in 0..max over 64 bins.
fn otsu(v: &[f32]) -> f32 {
    let max = v.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    let mut hist = [0usize; 64];
    for x in v {
        let b = ((x / max) * 63.0).clamp(0.0, 63.0) as usize;
        if let Some(c) = hist.get_mut(b) {
            *c += 1;
        }
    }
    let total = v.len().max(1) as f64;
    let sum_all: f64 = hist.iter().enumerate().map(|(i, c)| i as f64 * *c as f64).sum();
    let (mut wb, mut sb, mut best, mut best_t) = (0.0f64, 0.0f64, -1.0f64, 0usize);
    for (i, c) in hist.iter().enumerate() {
        wb += *c as f64;
        if wb == 0.0 {
            continue;
        }
        let wf = total - wb;
        if wf <= 0.0 {
            break;
        }
        sb += i as f64 * *c as f64;
        let (mb, mf) = (sb / wb, (sum_all - sb) / wf);
        let between = wb * wf * (mb - mf).powi(2);
        if between > best {
            best = between;
            best_t = i;
        }
    }
    (best_t as f32 + 0.5) / 63.0 * max
}

/// Keep the 4-connected regions of `bin` holding at least a fifth of the saliency of the
/// strongest one.
fn keep_main_regions(bin: &mut [bool], sal: &[f32], w: usize, h: usize) {
    let mut label = vec![0u32; bin.len()];
    let mut mass = vec![0.0f32];
    let mut stack = Vec::new();
    for start in 0..bin.len() {
        if !bin.get(start).copied().unwrap_or(false) || label.get(start).copied().unwrap_or(1) != 0 {
            continue;
        }
        let id = mass.len() as u32;
        let mut m = 0.0;
        stack.push(start);
        if let Some(l) = label.get_mut(start) {
            *l = id;
        }
        while let Some(i) = stack.pop() {
            m += sal.get(i).copied().unwrap_or(0.0);
            let (x, y) = (i % w, i / w);
            let mut push = |j: usize| {
                if bin.get(j).copied().unwrap_or(false)
                    && let Some(l) = label.get_mut(j)
                    && *l == 0
                {
                    *l = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        mass.push(m);
    }
    let top = mass.iter().copied().fold(0.0f32, f32::max);
    for (b, l) in bin.iter_mut().zip(&label) {
        if *b && mass.get(*l as usize).copied().unwrap_or(0.0) < 0.2 * top {
            *b = false;
        }
    }
}

fn landscape(lab: &[[f32; 3]], smoothness: &[f32], sky: &Sky, w: usize, h: usize) -> (Plane, Plane) {
    let mut water = Plane::new(w, h);
    let mut veg = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let below = sky.line.get(x).is_none_or(|b| y >= *b);
            if !below {
                continue;
            }
            let p = lab.get(i).copied().unwrap_or([0.0; 3]);
            let sm = smoothness.get(i).copied().unwrap_or(0.0);
            // reflections carry the sky's hue, darker
            let hue_d = sky.centres.iter().map(|c| ((p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2)).sqrt()).fold(f32::MAX, f32::min);
            let like_sky = (-(hue_d / 0.05).powi(2)).exp();
            let blue_green = smooth(0.0, 0.04, -p[2]) * (1.0 - smooth(0.0, 0.06, p[1]));
            if let Some(v) = water.data.get_mut(i) {
                *v = smooth(0.3, 0.8, sm) * like_sky.max(blue_green);
            }
            let green = smooth(0.0, 0.04, -p[1]) * smooth(-0.02, 0.03, p[2]);
            if let Some(v) = veg.data.get_mut(i) {
                *v = green;
            }
        }
    }
    let r = w.max(h) as f32 * 0.006;
    (lcc_raster::blur::gaussian(&water, r), lcc_raster::blur::gaussian(&veg, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Degenerate and hostile proxies (tiny, flat, NaN / infinite pixels) give empty or finite
    /// selections, never a panic.
    #[test]
    fn hostile_proxies_never_panic() {
        for (w, h) in [(0, 0), (1, 1), (3, 200), (200, 3), (5, 5)] {
            let a = analyze(&Rgb32f::new(w, h));
            assert_eq!(a.sky.data.len(), w * h);
        }
        let mut img = Rgb32f::filled(40, 30, [0.3, 0.4, 0.8]);
        img.data[7] = [f32::NAN, f32::INFINITY, -1.0];
        img.data[100] = [f32::INFINITY; 3];
        let a = analyze(&img);
        assert_eq!(a.subject.data.len(), 40 * 30);
        let grid = Plane::new(0, 0);
        let f = Frame::new(10, 10, &DevelopSettings::default(), true);
        assert!(sample(&grid, &f, 10, 10).data.iter().all(|v| *v == 0.0));
    }
}
