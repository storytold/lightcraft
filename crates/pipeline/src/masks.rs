//! Mask evaluation: each mask becomes an alpha plane at output resolution.
//!
//! Shapes are defined in normalized oriented-image coordinates; sizes are in "long-edge units".
//! Components combine with Add (max), Subtract (a·(1−c)) and Intersect (a·c).
//!
//! AI shapes (Sky, Subject, Background, People) use the segmentation [`Mattes`] the source carries
//! (DNG semantic masks, e.g. iPhone ProRAW's) when it has a matching one, else a heuristic.

use lcc_color::spline::Lut1;
use lcc_develop::{BrushStroke, LocalAdjustments, Mask, MaskOp, MaskShape, SegMask};
use lcc_geom::{Orientation, Point};
use lcc_raster::{Image, Plane, Rgb32f};

use crate::for_rows;
use crate::geometry::Frame;

/// What an embedded segmentation matte selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatteKind {
    Sky,
    /// A whole person (all people in the photo).
    Person,
    Skin,
    Hair,
    Teeth,
    Glasses,
}

impl MatteKind {
    /// A stable code for serialization (e.g. a smart preview's stored mattes); never renumber.
    pub fn code(self) -> u8 {
        match self {
            MatteKind::Sky => 0,
            MatteKind::Person => 1,
            MatteKind::Skin => 2,
            MatteKind::Hair => 3,
            MatteKind::Teeth => 4,
            MatteKind::Glasses => 5,
        }
    }

    /// [`MatteKind::code`]'s inverse (`None` for a code this build doesn't know).
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(MatteKind::Sky),
            1 => Some(MatteKind::Person),
            2 => Some(MatteKind::Skin),
            3 => Some(MatteKind::Hair),
            4 => Some(MatteKind::Teeth),
            5 => Some(MatteKind::Glasses),
            _ => None,
        }
    }
}

/// Segmentation mattes that came with the source: 0..255 alpha planes, each covering the whole
/// (EXIF-oriented) source image at its own resolution. Several mattes of one kind (e.g. one per
/// person) are combined with max.
#[derive(Clone, Default, PartialEq)]
pub struct Mattes {
    mattes: Vec<(MatteKind, std::sync::Arc<Image<u8>>)>,
    /// Selections computed from the photo itself ([`crate::auto_mask`]), used by shapes no
    /// matte fits.
    pub auto: Option<std::sync::Arc<crate::auto_mask::AutoMasks>>,
}

impl std::fmt::Debug for Mattes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.mattes.iter().map(|(k, m)| (k, m.width, m.height))).finish()
    }
}

impl Mattes {
    /// Add a matte (empty ones are ignored).
    pub fn push(&mut self, kind: MatteKind, matte: Image<u8>) {
        if matte.width > 0 && matte.height > 0 && matte.data.len() == matte.width * matte.height {
            self.mattes.push((kind, std::sync::Arc::new(matte)));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mattes.is_empty() && self.auto.is_none()
    }

    /// Memory held (bytes).
    pub fn bytes(&self) -> usize {
        self.mattes.iter().map(|(_, m)| m.data.len()).sum()
    }

    /// The stored mattes, in insertion order (kind and plane), e.g. for storing them with a
    /// photo's proxy source.
    pub fn iter(&self) -> impl Iterator<Item = (MatteKind, &Image<u8>)> {
        self.mattes.iter().map(|(k, m)| (*k, m.as_ref()))
    }

    /// Whether `a` and `b` hold the very same mattes (each plane and the automatic selections by
    /// identity), so mask planes evaluated with one fit the other. A render's plan wraps the
    /// source's mattes and its cached automatic selections in a new [`Mattes`] every time
    /// (`auto_mask::needed`), so comparing the outer `Arc`s missed on every slider tick of a photo
    /// with an AI mask and evaluated all its masks again (about 55 ms of each 45 MP draft).
    pub fn same(a: &std::sync::Arc<Mattes>, b: &std::sync::Arc<Mattes>) -> bool {
        use std::sync::Arc;
        Arc::ptr_eq(a, b)
            || (a.mattes.len() == b.mattes.len()
                && a.mattes.iter().zip(&b.mattes).all(|((ka, ma), (kb, mb))| ka == kb && Arc::ptr_eq(ma, mb))
                && match (&a.auto, &b.auto) {
                    (None, None) => true,
                    (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                    _ => false,
                })
    }

    fn of(&self, kinds: &[MatteKind]) -> Vec<&Image<u8>> {
        self.mattes.iter().filter(|(k, _)| kinds.contains(k)).map(|(_, m)| &**m).collect()
    }

    /// The mattes a shape uses instead of its heuristic (empty: none fits).
    fn for_shape(&self, shape: &MaskShape) -> Vec<&Image<u8>> {
        use MatteKind::*;
        // the whole person: a person matte, else the union of the parts
        let person = || {
            let p = self.of(&[Person]);
            if p.is_empty() { self.of(&[Skin, Hair, Teeth, Glasses]) } else { p }
        };
        match shape {
            MaskShape::Sky => self.of(&[Sky]),
            MaskShape::Subject => person(),
            MaskShape::People { parts, .. } => {
                let wanted: Vec<MatteKind> = parts
                    .iter()
                    .filter_map(|p| {
                        let p = p.to_ascii_lowercase();
                        [("skin", Skin), ("hair", Hair), ("teeth", Teeth), ("glasses", Glasses)]
                            .into_iter()
                            .find(|(n, _)| p.contains(n))
                            .map(|(_, k)| k)
                    })
                    .collect();
                let whole =
                    parts.is_empty() || parts.iter().any(|p| matches!(p.to_ascii_lowercase().as_str(), "person" | "entire person" | "entireperson"));
                // parts without a matte (lips, clothes, …) keep the heuristic
                if whole { person() } else { self.of(&wanted) }
            }
            _ => Vec::new(),
        }
    }
}

pub struct Evaluated {
    /// The mask's id ([`Mask::id`]).
    pub id: u32,
    /// Shared with the view's stage cache, which keeps it for the next render ([`alpha_key`]).
    pub alpha: std::sync::Arc<Plane>,
    pub adjust: LocalAdjustments,
    /// The mask's local Curve as tables ([`crate::finish::local_curve_luts`]); `None` while it is linear.
    pub curve: Option<[Lut1; 3]>,
}

/// Whether `shape`'s alpha depends on the exposure: range masks select on exposed values, and so
/// does the sky heuristic (not a sky from the source's matte). Shapes not listed as independent
/// count as dependent (a new shape is re-evaluated rather than wrongly reused).
fn reads_exposure(shape: &MaskShape, mattes: Option<&Mattes>) -> bool {
    // a shape drawn from the source's matte (an AI sky or subject) reads only the matte
    if mattes.is_some_and(|m| !m.for_shape(shape).is_empty()) {
        return false;
    }
    !matches!(
        shape,
        MaskShape::Linear { .. }
            | MaskShape::Radial { .. }
            | MaskShape::Brush { .. }
            | MaskShape::Subject
            | MaskShape::Background
            | MaskShape::Object { .. }
            | MaskShape::Prompt { .. }
            | MaskShape::People { .. }
    )
}

/// Key of the alpha planes [`evaluate`] gives for `masks` at `w × h` with exposure `ev`, for one
/// image: the caller also keys that image (its linear stage and frame) and the source's mattes
/// (by identity). What the planes depend on is in it — the visible masks' components, inversion,
/// Refine Edges, Feather and amount, and the exposure where a shape selects on it — and nothing else, so
/// a slider that does not touch the masks reuses them (a drag of a photo with AI masks re-ran
/// them every tick: most of its render time).
pub fn alpha_key(masks: &[Mask], ev: f32, w: usize, h: usize, mattes: Option<&Mattes>) -> u64 {
    use std::fmt::Write;
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    let mut parts = String::new();
    let mut exposure = false;
    let mut automatic = false;
    for m in masks.iter().filter(|m| m.visible && !m.components.is_empty()) {
        let _ = write!(parts, "{}|{}|{}|{}|{}|{}", m.id, m.invert, m.refine.to_bits(), m.feather.to_bits(), m.adjust.amount.to_bits(), m.on_photo);
        for c in &m.components {
            // (the whole component: a field added to it later counts too)
            let _ = write!(parts, "|{c:?}");
            exposure |= reads_exposure(&c.shape, mattes);
            automatic |= crate::auto_mask::wants(&c.shape);
        }
        parts.push(';');
    }
    let ev = if exposure { ev.to_bits() } else { 0 };
    // (a mask on an automatic selection changes when a model is installed or removed)
    let models = if automatic { crate::auto_mask::model_generation() } else { 0 };
    BuildHasherDefault::<DefaultHasher>::default().hash_one((parts, w, h, ev, models))
}

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Most bytes of alpha planes a render keeps at once, and the fewest masks it always keeps: every evaluated mask
/// retains a full-size `f32` plane, so a sidecar listing thousands of masks must not multiply the output size
/// (upstream #3699).
const MAX_ALPHA_BYTES: usize = 8 << 30;
const MIN_MASKS: usize = 16;

/// How many masks of a `w` × `h` render are evaluated (the first ones, in order).
pub fn max_masks(w: usize, h: usize) -> usize {
    (MAX_ALPHA_BYTES / w.saturating_mul(h).saturating_mul(4).max(1)).max(MIN_MASKS)
}

/// The visible masks with components, evaluated in order (`mattes`: the source's, see [`Mattes`]).
#[allow(clippy::too_many_arguments)]
pub fn evaluate(masks: &[Mask], frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Vec<Evaluated> {
    masks
        .iter()
        .filter(|m| m.visible && !m.components.is_empty())
        .take(max_masks(w, h))
        .map(|m| Evaluated {
            id: m.id,
            alpha: std::sync::Arc::new(evaluate_one(m, frame, w, h, img, log_l, ev, mattes)),
            adjust: m.adjust,
            curve: crate::finish::local_curve_luts(&m.curve),
        })
        .collect()
}

/// The alpha plane of mask `m` (whether visible or not): its components combined, inverted and
/// scaled by its amount.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_one(m: &Mask, frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Plane {
    // its coordinates on the photo before the warp, or (an old mask) on the corrected frame
    let frame = &*frame.for_mask(m.on_photo);
    let mut alpha = Plane::new(w, h);
    let mut first = true;
    for comp in &m.components {
        let mut c = shape_alpha(&comp.shape, frame, w, h, img, log_l, ev, mattes);
        if comp.invert {
            c.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        }
        if first {
            alpha = c;
            first = false;
            continue;
        }
        // an inverted mask is inverted at the end, so combine on its visible (inverted) side:
        // [+] adds to what the mask shows and [-] removes from it
        let inv = m.invert;
        for (a, c) in alpha.data.iter_mut().zip(&c.data) {
            let v = if inv { 1.0 - *a } else { *a };
            let v = match comp.op {
                MaskOp::Add => v.max(*c),
                MaskOp::Subtract => v * (1.0 - c),
                MaskOp::Intersect => v * c,
            };
            *a = if inv { 1.0 - v } else { v };
        }
        first = false;
    }
    if m.refine > 0.0 {
        // Refine Edges: the mask's edges follow the photo's (window up to ~4 % of the long edge,
        // the width of a soft brush edge or gradient)
        let k = (m.refine / 100.0).clamp(0.0, 1.0) as f32;
        let sigma = (0.04 * frame.output_long(w, h) as f32 * k).max(1.0);
        let refined = guided_cross(log_l, &alpha, sigma, 0.02);
        for (a, r) in alpha.data.iter_mut().zip(&refined.data) {
            *a += (r - *a) * k.sqrt();
        }
    }
    if m.feather > 0.0 {
        // Feather: soften the combined mask's edges; sigma up to 2 % of the long edge (relative,
        // so a preview and a full-size export match)
        let k = (m.feather / 100.0).clamp(0.0, 1.0) as f32;
        let sigma = 0.02 * frame.output_long(w, h) as f32 * k;
        alpha = lcc_raster::blur::gaussian(&alpha, sigma);
    }
    if m.invert {
        alpha.data.iter_mut().for_each(|v| *v = 1.0 - *v);
    }
    let amt = (m.adjust.amount / 100.0) as f32;
    if (amt - 1.0).abs() > 1e-6 {
        alpha.data.iter_mut().for_each(|v| *v *= amt);
    }
    alpha
}

/// Per-pixel positions in long-edge units for output pixel centres.
fn for_each_pos(frame: &Frame, w: usize, h: usize, out: &mut Plane, f: impl Fn(Point, usize) -> f32 + Sync + Send) {
    let m = frame.out_to_mask(w, h);
    let (ow, oh, l) = (frame.ow, frame.oh, frame.ow.max(frame.oh));
    for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            *v = f(Point::new(n.x * ow / l, n.y * oh / l), y * w + x);
        }
    });
}

/// The alpha plane of one component shape. AI shapes use the source's matching `mattes`, if any.
#[allow(clippy::too_many_arguments)]
pub fn shape_alpha(shape: &MaskShape, frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Plane {
    // Background is the complement of Subject, whichever source (embedded matte or automatic grid) Subject uses
    if matches!(shape, MaskShape::Background) {
        let mut s = shape_alpha(&MaskShape::Subject, frame, w, h, img, log_l, ev, mattes);
        s.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        return s;
    }
    // a People selection the people models computed on this photo (one person, their parts), a
    // remembered object found on it
    if let MaskShape::People { seg: Some(seg), .. } | MaskShape::Concept { seg: Some(seg), .. } = shape {
        let mut out = Plane::new(w, h);
        // (plain enlargement: the edge-following filter is checked on Object and Describe grids only)
        sample_seg(Some(seg), &[], 1.0, frame, w, h, None, &mut out);
        return out;
    }
    let fitting = mattes.map(|m| m.for_shape(shape)).unwrap_or_default();
    if !fitting.is_empty() {
        return matte_alpha(&fitting, frame, w, h);
    }
    if let MaskShape::DepthRange { lo, hi, feather } = shape {
        // the depth model's distance grid (none: nothing selected), sampled like the image
        let Some(grid) = mattes.and_then(|m| m.auto.as_deref()).and_then(|a| a.depth.as_ref()) else { return Plane::new(w, h) };
        let mut out = crate::auto_mask::sample(grid, frame, w, h);
        let (lo, hi, f) = (*lo as f32, *hi as f32, *feather as f32);
        out.map_in_place(|d| crate::depth_model::range_alpha(d, lo, hi, f));
        return out;
    }
    if let Some(auto) = mattes.and_then(|m| m.auto.as_deref()) {
        let grid = auto.for_shape(shape);
        if let Some(grid) = grid.filter(|_| crate::auto_mask::wants(shape)) {
            let coarse = crate::auto_mask::sample(grid, frame, w, h);
            // snap the coarse grid's edges to the photo's (radius: a grid cell)
            let cell = (frame_px(frame, w) / grid.width.max(grid.height).max(1) as f32).max(1.0);
            return guided_cross(log_l, &coarse, 1.5 * cell, 0.01);
        }
    }
    let mut out = Plane::new(w, h);
    let to_long = |p: Point| frame.norm_to_long(p);
    // `img`/`log_l` are before exposure: range masks select on the exposed values.
    let gain = ev.exp2();
    match shape {
        MaskShape::Linear { start, end } => {
            let (a, b) = (to_long(*start), to_long(*end));
            let d = b - a;
            let len2 = d.dot(d).max(1e-12);
            for_each_pos(frame, w, h, &mut out, |p, _| {
                let t = ((p - a).dot(d) / len2) as f32;
                1.0 - smooth(0.0, 1.0, t)
            });
        }
        MaskShape::Radial { center, rx, ry, angle, feather, invert } => {
            let c = to_long(*center);
            let (s, co) = (-angle.to_radians()).sin_cos();
            let f = (*feather / 100.0).clamp(0.0, 1.0) as f32;
            let (rx, ry) = (rx.max(1e-6), ry.max(1e-6));
            let inv = *invert;
            for_each_pos(frame, w, h, &mut out, |p, _| {
                let d = p - c;
                let (x, y) = (d.x * co - d.y * s, d.x * s + d.y * co);
                let r = ((x / rx).powi(2) + (y / ry).powi(2)).sqrt() as f32;
                let a = 1.0 - smooth(1.0 - f.max(0.01), 1.0, r);
                if inv { 1.0 - a } else { a }
            });
        }
        MaskShape::Brush { strokes } => rasterize_brush(strokes, frame, w, h, img, log_l, &mut out),
        MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
            let (lo, hi, lf, hf) = (*lo as f32, *hi as f32, (*lo_feather as f32).max(1e-3), (*hi_feather as f32).max(1e-3));
            for (v, l) in out.data.iter_mut().zip(&log_l.data) {
                // map log luminance to a 0..1 perceptual scale (−8..+4 EV)
                let y = ((l + ev + 8.0) / 12.0).clamp(0.0, 1.0);
                *v = smooth(lo - lf, lo, y) * (1.0 - smooth(hi, hi + hf, y));
            }
        }
        MaskShape::ColorRange { samples, refine, .. } => {
            let tol = color_range_tolerance(*refine);
            let samples: Vec<[f32; 3]> = samples.iter().map(|s| [s[0] as f32, s[1] as f32, s[2] as f32]).collect();
            for (v, c) in out.data.iter_mut().zip(&img.data) {
                let lab = lcc_color::perceptual::oklab_from_2020(tonemap_for_select(c.map(|v| v * gain)));
                let d = samples
                    .iter()
                    .map(|s| ((lab[1] - s[1]).powi(2) + (lab[2] - s[2]).powi(2) + 0.25 * (lab[0] - s[0]).powi(2)).sqrt())
                    .fold(f32::MAX, f32::min);
                *v = 1.0 - smooth(tol * 0.5, tol, d);
            }
        }
        MaskShape::Sky => {
            // Classical sky heuristic until the segmenter lands (M12): bright, smooth, blue-ish or
            // unsaturated, and connected to the top of the frame.
            let m = frame.out_to_norm(w, h);
            for (i, v) in out.data.iter_mut().enumerate() {
                let (x, y) = (i % w, i / w);
                let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                let c = img.data[i].map(|v| v * gain);
                let l = log_l.data[i] + ev;
                let blue = (c[2] - c[0]).max(0.0) / (c[2] + 1e-4);
                let top = 1.0 - smooth(0.25, 0.7, n.y as f32);
                *v = top * smooth(-3.5, -1.0, l) * (0.4 + 0.6 * smooth(0.0, 0.3, blue).max(smooth(0.0, 1.5, l)));
            }
            smooth_plane(&mut out, 0.01 * frame_px(frame, w));
        }
        MaskShape::Object { seg, detail, edge, .. } | MaskShape::Prompt { seg, detail, edge, .. } if seg.is_some() || !detail.is_empty() => {
            // Edge: below 0 a steeper transition (up to 8× the logits), above 0 feathered (up to
            // 2 % of the long edge, so previews and exports match)
            let e = (*edge / 100.0).clamp(-1.0, 1.0) as f32;
            let gain = if e < 0.0 { 1.0 - 7.0 * e } else { 1.0 };
            sample_seg(seg.as_ref(), detail, gain, frame, w, h, Some(log_l), &mut out);
            if e > 0.0 {
                smooth_plane(&mut out, e * 0.02 * frame_px(frame, w));
            }
        }
        // not computed yet (no clicks, or a build without the model): nothing selected
        MaskShape::Object { hint, .. } if hint.is_empty() => {}
        MaskShape::Prompt { .. } => {}
        // a remembered object not found (yet) on this photo: nothing selected
        MaskShape::Concept { .. } => {}
        MaskShape::Subject | MaskShape::Object { .. } | MaskShape::People { .. } => {
            // Saliency heuristic: centre-weighted local contrast (replaced by the segmenter in M12).
            let m = frame.out_to_norm(w, h);
            let blur = lcc_raster::blur::gaussian(log_l, 0.03 * frame_px(frame, w));
            for (i, v) in out.data.iter_mut().enumerate() {
                let (x, y) = (i % w, i / w);
                let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                let d = (((n.x - 0.5) / 0.35).powi(2) + ((n.y - 0.55) / 0.4).powi(2)).sqrt() as f32;
                let contrast = (log_l.data[i] - blur.data[i]).abs();
                *v = (1.0 - smooth(0.6, 1.0, d)) * (0.5 + 0.5 * smooth(0.05, 0.6, contrast));
            }
            smooth_plane(&mut out, 0.015 * frame_px(frame, w));
        }
        MaskShape::Background => {
            let mut s = shape_alpha(&MaskShape::Subject, frame, w, h, img, log_l, ev, mattes);
            s.data.iter_mut().for_each(|v| *v = 1.0 - *v);
            out = s;
        }
        MaskShape::DepthRange { .. } | MaskShape::Landscape { .. } => {}
    }
    out
}

/// A decoded [`SegMask`]: its logits and the normalized rectangle they cover.
pub(crate) struct SegGrid {
    logits: Vec<f32>,
    side: usize,
    r: [f64; 4],
}

impl SegGrid {
    pub(crate) fn new(seg: &SegMask) -> Option<SegGrid> {
        Some(SegGrid { logits: seg.logits()?, side: seg.side as usize, r: seg.bounds()? })
    }

    /// The normalized box of the cells whose logit is above `min` (`None`: no such cell).
    pub(crate) fn bounds_above(&self, min: f32) -> Option<[f64; 4]> {
        let side = self.side;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, l) in self.logits.iter().enumerate() {
            if *l > min {
                let (x, y) = (i % side, i / side);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        if x0 == usize::MAX {
            return None;
        }
        let (rw, rh) = (self.r[2] - self.r[0], self.r[3] - self.r[1]);
        let s = side as f64;
        Some([self.r[0] + x0 as f64 / s * rw, self.r[1] + y0 as f64 / s * rh, self.r[0] + x1 as f64 / s * rw, self.r[1] + y1 as f64 / s * rh])
    }

    pub(crate) fn contains(&self, n: Point) -> bool {
        n.x >= self.r[0] && n.x <= self.r[2] && n.y >= self.r[1] && n.y <= self.r[3]
    }

    /// The bilinearly sampled logit at normalized image point `n` (cell centres at
    /// `(i + 0.5) / side` of the rectangle).
    pub(crate) fn logit(&self, n: Point) -> f32 {
        let side = self.side;
        let at = |x: usize, y: usize| self.logits.get(y.min(side - 1) * side + x.min(side - 1)).copied().unwrap_or(-16.0);
        let u = (n.x - self.r[0]) / (self.r[2] - self.r[0]);
        let v = (n.y - self.r[1]) / (self.r[3] - self.r[1]);
        let (fx, fy) = ((u * side as f64 - 0.5) as f32, (v * side as f64 - 0.5) as f32);
        let (fx, fy) = (fx.clamp(0.0, (side - 1) as f32), fy.clamp(0.0, (side - 1) as f32));
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let bot = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bot * ty
    }

    /// Output pixels per grid cell (the longer side of a cell: the grid is square over a
    /// rectangle).
    fn cell_px(&self, frame: &Frame, w: usize) -> f64 {
        let px = frame.px_per_long(w) / frame.ow.max(frame.oh).max(1e-9);
        ((self.r[2] - self.r[0]) * frame.ow).max((self.r[3] - self.r[1]) * frame.oh) * px / self.side.max(1) as f64
    }
}

/// [`sample_seg`]'s window, in grid cells (Gaussian sigma): the width of the band bilinear
/// enlargement leaves uncertain, about a cell on either side of the contour.
const SEG_WINDOW: f64 = 1.5;
/// [`sample_seg`]'s guided-filter regularization (EV² of log luminance): an edge of ~0.1 EV
/// contrast counts half.
const SEG_EPS: f32 = 0.01;

/// Stored model segmentations at each output pixel: the zoomed-in `detail` passes inside
/// their rectangles (the highest of them where they overlap), the whole-photo `seg`
/// elsewhere; the logit (times `gain`: > 1 is a harder edge) through a sigmoid. Damaged
/// data selects nothing.
///
/// A model's grid is much coarser than an export (a 288² grid has ~20 px cells on 24 MP), and
/// its bilinear contour passes up to a cell from the photo's edge, so the mask spilled onto the
/// background (a bright outline under a raised Exposure). With a `guide` (the photo's log
/// luminance), where the grid is coarser than the output, the logits are guided-filtered by it
/// ([`guided_cross_fast`]; a window of [`SEG_WINDOW`] cells, so previews and exports draw the same
/// edge): near the contour the logit follows the photo's tones and its edge lands on the photo's.
/// Filtering logits, not coverage, makes the confident side win (a strip at 0.3 between 1 and 0
/// goes to 0, not 0.1), and leaves a linear ramp of logits, where the photo has no edge, as it
/// was. Pixels whose window is all inside or all outside (|mean logit| ≥ 7, times `gain`) keep
/// their sample exactly, as do those where the photo has no edge to follow. Refine Edges then
/// works on the combined mask ([`evaluate_one`]) and finds these edges already in place.
#[allow(clippy::too_many_arguments)]
fn sample_seg(seg: Option<&SegMask>, detail: &[SegMask], gain: f32, frame: &Frame, w: usize, h: usize, guide: Option<&Plane>, out: &mut Plane) {
    let coarse = seg.and_then(SegGrid::new);
    // (capped: a hostile document can hold many)
    let patches: Vec<SegGrid> = detail.iter().take(lcc_develop::segmask::MAX_DETAIL).filter_map(SegGrid::new).collect();
    if coarse.is_none() && patches.is_empty() {
        return;
    }
    let m = frame.out_to_mask(w, h);
    for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            let fine = patches.iter().filter(|p| p.contains(n)).map(|p| p.logit(n)).reduce(f32::max);
            *v = fine.or_else(|| coarse.as_ref().map(|c| c.logit(n))).unwrap_or(-16.0) * gain;
        }
    });
    // (the coarsest grid sets the window: it leaves the widest uncertain band)
    let cell = coarse.iter().chain(&patches).map(|g| g.cell_px(frame, w)).fold(0.0, f64::max);
    if let Some(guide) = guide.filter(|_| cell >= 1.0) {
        guided_cross_fast(guide, out, (SEG_WINDOW * cell) as f32, SEG_EPS, |mean| 1.0 - smooth(3.0 * gain, 7.0 * gain, mean.abs()));
    }
    out.map_in_place(|l| 1.0 / (1.0 + (-l).exp()));
}

fn frame_px(frame: &Frame, w: usize) -> f32 {
    frame.px_per_long(w) as f32
}

fn smooth_plane(p: &mut Plane, sigma: f32) {
    *p = lcc_raster::blur::gaussian(p, sigma.max(0.5));
}

/// The union (max) of `mattes` (0..255, over the EXIF-oriented source) at output resolution,
/// sampled like the image: user orientation, lens / perspective warp, crop and flips; area-averaged
/// first when the output is much smaller than the matte, bilinear otherwise.
fn matte_alpha(mattes: &[&Image<u8>], frame: &Frame, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    let o2t = frame.out_to_oriented(w, h);
    let ppl = frame.px_per_long(w).max(1e-9);
    for m in mattes {
        let oriented =
            if frame.orient == Orientation::Normal { std::borrow::Cow::Borrowed(*m) } else { std::borrow::Cow::Owned(m.oriented(frame.orient)) };
        // matte px per output px
        let k = oriented.width as f64 / frame.ow * frame.ow.max(frame.oh) / ppl;
        let base = box_down(&oriented, if k >= 2.0 { (k.floor() as usize).min(64) } else { 1 });
        let (sx, sy) = ((base.width as f64 / frame.ow) as f32, (base.height as f64 / frame.oh) as f32);
        for_rows(&mut out.data, w, |y, row| {
            for (x, v) in row.iter_mut().enumerate() {
                let s = match &frame.warp {
                    Some(wp) => {
                        let Some((_, s)) = wp.frame(&o2t, x, y) else {
                            continue;
                        };
                        s
                    }
                    None => o2t.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5)),
                };
                *v = v.max(bilinear(&base, s.x as f32 * sx, s.y as f32 * sy) / 255.0);
            }
        });
    }
    out
}

/// `m` area-averaged by `f × f` (0..255 values kept); `f == 1` converts only.
fn box_down(m: &Image<u8>, f: usize) -> Plane {
    let f = f.max(1);
    let (bw, bh) = (m.width.div_ceil(f).max(1), m.height.div_ceil(f).max(1));
    let mut out = Plane::new(bw, bh);
    for_rows(&mut out.data, bw, |by, row| {
        let rows = by * f..((by + 1) * f).min(m.height);
        for (bx, v) in row.iter_mut().enumerate() {
            let cols = bx * f..((bx + 1) * f).min(m.width);
            let (mut sum, mut n) = (0u32, 0u32);
            for y in rows.clone() {
                for x in cols.clone() {
                    sum += m.data.get(y * m.width + x).copied().unwrap_or(0) as u32;
                    n += 1;
                }
            }
            *v = sum as f32 / n.max(1) as f32;
        }
    });
    out
}

/// Bilinear sample at continuous pixel coordinates (pixel centres at +0.5), edges extended.
fn bilinear(p: &Plane, x: f32, y: f32) -> f32 {
    // (held a pixel outside, which reads the same edge: an infinite coordinate reads the edge
    // instead of a NaN weight and a neighbour index past isize::MAX)
    let (fx, fy) = (x.clamp(-1.0, p.width as f32 + 1.0) - 0.5, y.clamp(-1.0, p.height as f32 + 1.0) - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as isize, y0 as isize);
    let a = p.get_clamped(x0, y0) + (p.get_clamped(x0 + 1, y0) - p.get_clamped(x0, y0)) * tx;
    let b = p.get_clamped(x0, y0 + 1) + (p.get_clamped(x0 + 1, y0 + 1) - p.get_clamped(x0, y0 + 1)) * tx;
    a + (b - a) * ty
}

/// A rough display mapping for colour picking (so samples taken on screen match).
pub(crate) fn tonemap_for_select(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| v / (1.0 + v))
}

/// Colour range: the OkLab distance (a, b at full weight, L at a quarter) at which a pixel leaves
/// the selection for a Refine amount of 0..100. A pixel is fully selected up to half of it and
/// fades out over the other half (CPU here, `mask.wgsl` kind 3; both take this value).
///
/// Issue #157: `0.04 + 0.16·refine` selected the whole photo. Through [`tonemap_for_select`] a
/// slightly teal mid-grey (sRGB 120,150,155) and a neutral light grey (200,198,195) are only ~0.063
/// apart — L 0.60 vs 0.72, a/b ≤ 0.02 — so at the default Refine 50 (full selection below 0.06)
/// the grey patch was 99 % selected, and at Refine 85 / 100 still most of it. A JND is ~0.01 on
/// this scale, so Refine 50 now holds a surface up to ~2 JND from its sample and drops everything
/// beyond ~4.5 (full to 0.0225, gone at 0.045); Refine 85 is gone at 0.066, past that grey patch;
/// Refine 100 reaches 0.075 and Refine 0 is the sample's own shade (full to 0.0075, gone at 0.015).
pub fn color_range_tolerance(refine: f64) -> f32 {
    0.015 + 0.06 * (refine / 100.0).clamp(0.0, 1.0) as f32
}

/// A brush stroke resolved to output pixels: dab centres, radius (where a dab ends), hard-core
/// radius, flow and density (0..1), and whether it erases. A legacy stroke's dabs lie r/4 apart
/// along the path, each solid inside `hard` and smoothly fading to `r`; a Lightroom-model stroke's
/// ([`BrushDabs::lrc`]) every [`DAB_SPACING`] × r, each `exp(−(d/a)^p)` cut at `r`.
pub struct BrushDabs {
    pub dabs: Vec<Point>,
    pub r: f64,
    pub hard: f64,
    /// Lightroom's brush model ([`lcc_develop::BRUSH_MODEL_LRC`]): the dab profile's `a`
    /// (pixels, at full pressure) and `p`; strokes add up instead of taking the maximum. `None`: the
    /// legacy model.
    pub lrc: Option<(f64, f64)>,
    pub flow: f32,
    pub density: f32,
    pub erase: bool,
    /// Auto Mask: paint only pixels like the one under each dab, then snap to edges.
    pub auto: bool,
    /// How much a Lightroom-model stroke's Auto Mask widens its limits ([`BrushStroke::auto_range_scale`]); 1
    /// for a legacy stroke.
    pub auto_scale: f32,
    /// Pen pressure (0..1) of each dab, parallel to `dabs`; empty when the stroke has no pressure data (full pressure).
    pub pressure: Vec<f32>,
}

impl BrushDabs {
    /// Radius, hard core and flow of dab `i`, pen pressure applied ([`pressure_radius`], [`pressure_flow`]).
    pub fn dab(&self, i: usize) -> (f64, f64, f32) {
        match self.pressure.get(i) {
            Some(&p) => {
                let r = pressure_radius(self.r, p);
                (r, if self.r > 0.0 { self.hard * r / self.r } else { self.hard }, pressure_flow(self.flow, p))
            }
            None => (self.r, self.hard, self.flow),
        }
    }

    /// How much one dab of radius `r` and hard core `hard` (from [`BrushDabs::dab`]) paints at
    /// distance `dd` from its centre, in this stroke's brush model.
    #[inline]
    pub fn alpha(&self, dd: f64, r: f64, hard: f64) -> f32 {
        match self.lrc {
            // pen pressure scales the whole dab
            Some((a, p)) => lrc_dab_alpha(dd, r, if self.r > 0.0 { a * r / self.r } else { a }, p),
            None => dab_alpha(dd, r, hard),
        }
    }

    /// The coverage `o` after this stroke paints `a` there ([`combine_stroke`]).
    #[inline]
    pub fn combine(&self, o: f32, a: f32) -> f32 {
        combine_stroke(o, a, self.erase, self.lrc.is_some(), self.density)
    }
}

/// The coverage `o` after a stroke paints `a` there (`a` already capped by the stroke's density):
/// an erasing stroke takes away, a legacy stroke keeps the larger of the two, a Lightroom-model
/// stroke adds up the way its dabs do, up to its density (note 20: overlapping soft strokes blend
/// into one shape instead of pinching where they meet).
#[inline]
pub fn combine_stroke(o: f32, a: f32, erase: bool, lrc: bool, density: f32) -> f32 {
    if erase {
        o * (1.0 - a)
    } else if lrc {
        o.max((o + a - o * a).min(density))
    } else {
        o.max(a)
    }
}

/// A Lightroom-model dab ([`lcc_develop::brush_model`]) at distance `dd` from its centre:
/// `exp(−(dd/a)^p)` up to the radius `r`, 0 beyond.
#[inline]
pub fn lrc_dab_alpha(dd: f64, r: f64, a: f64, p: f64) -> f32 {
    if dd.is_nan() || dd > r || a <= 0.0 {
        return 0.0;
    }
    (-(dd / a).powf(p)).exp() as f32
}

/// The radius of a dab of a brush of radius `r` (pixels) painted at pen pressure `p` (0..1): from a tenth of the
/// brush at the lightest touch to the full brush at full pressure, never below half a pixel.
#[inline]
pub fn pressure_radius(r: f64, p: f32) -> f64 {
    let p = if p.is_nan() { 1.0 } else { f64::from(p.clamp(0.0, 1.0)) };
    (r * (0.1 + 0.9 * p)).max(0.5).min(r.max(0.5))
}

/// The flow (0..1) of a dab painted at pen pressure `p` (0..1): proportional to the pressure.
#[inline]
pub fn pressure_flow(flow: f32, p: f32) -> f32 {
    let p = if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) };
    flow * p
}

/// Pressures for the `added` dabs [`push_dabs`] just laid from a sample at pressure `from` to one at `to`: evenly
/// interpolated, the last one at `to`, as the dabs themselves are placed.
pub fn push_pressures(pressure: &mut Vec<f32>, added: usize, from: f32, to: f32) {
    for k in 1..=added {
        pressure.push(from + (to - from) * k as f32 / added as f32);
    }
}

/// Output pixels a brush sample must move from the last dab to count as movement: below this the
/// pointer is held still (the same screen position mapped again, up to float rounding).
const STATIONARY_PX: f64 = 1e-6;

/// Most dabs one stroke holds (32 MiB of centres). A segment is capped on its own, but a stroke of
/// many hostile segments (an agent, a crafted XMP or catalog) would otherwise add up without
/// bound; past this the rest of the stroke lays nothing (#1520).
pub const MAX_STROKE_DABS: usize = 2_000_000;

/// The dabs of stroke `s` in a `w × h` output.
pub fn brush_dabs(s: &BrushStroke, frame: &Frame, w: usize, h: usize) -> BrushDabs {
    brush_dabs_within(s, frame, w, h, MAX_STROKE_DABS)
}

/// [`brush_dabs`] stopping at `max` dabs instead of [`MAX_STROKE_DABS`].
///
/// For a caller that discards a stroke past a threshold of its own: it builds to that threshold
/// rather than to the stroke budget and throws the difference away. The GPU path rejects a brush
/// mask past 4096 dabs in total and the Auto Mask spot window past 200,000, both far below the
/// two million a single stroke may otherwise construct first (#1520).
///
/// Ask for one more than the threshold to tell "at it" from "past it": a result shorter than
/// `max` is the whole stroke.
pub fn brush_dabs_within(s: &BrushStroke, frame: &Frame, w: usize, h: usize, max: usize) -> BrushDabs {
    if s.is_lrc() {
        return lrc_brush_dabs(s, frame, w, h, max);
    }
    let to_out = frame.mask_to_out(w, h);
    let ppl = frame.px_per_long(w);
    let r = (s.size * ppl).max(0.5);
    let hard = r * (1.0 - (s.feather / 100.0).clamp(0.0, 1.0));
    // Densify the path so dabs overlap (spacing r/4). Coverage builds up by passes, not by how
    // long the button is held: a sample where the pointer hasn't moved (a stationary hold records
    // one per frame) lays no dab, so waiting in place adds nothing (#518).
    let mut dabs: Vec<Point> = Vec::new();
    let mut pressure: Vec<f32> = Vec::new();
    let with_pressure = !s.pressure.is_empty();
    let mut prev = None;
    let mut prev_p = 1.0;
    let mut emitted = 0usize;
    for (i, p) in s.points.iter().enumerate() {
        if emitted >= max {
            break;
        }
        // (a point the warp can't place lays no dab, rather than one at its unwarped place)
        let Some(q) = to_out(*p) else { continue };
        let before = dabs.len();
        if push_dabs_within(&mut dabs, &mut emitted, prev, q, r, max) {
            let pq = s.pressure_at(i);
            if with_pressure {
                // the first dab of the stroke has nothing to interpolate from
                let from = if prev.is_some() { prev_p } else { pq };
                push_pressures(&mut pressure, dabs.len() - before, from, pq);
            }
            prev = Some(q);
            prev_p = pq;
        }
    }
    BrushDabs {
        dabs,
        r,
        hard,
        lrc: None,
        flow: (s.flow / 100.0).clamp(0.0, 1.0) as f32,
        density: (s.density / 100.0).clamp(0.0, 1.0) as f32,
        erase: s.erase,
        auto: s.auto_mask,
        auto_scale: 1.0,
        pressure,
    }
}

/// The dabs of a Lightroom-model stroke ([`lcc_develop::brush_model`]): `size` is the core
/// radius `R0`, the dabs reach `R0 · outer_ratio(feather)` and lie every [`DAB_SPACING`] of that
/// along the path ([`walk_dabs`]), as Lightroom saves them.
fn lrc_brush_dabs(s: &BrushStroke, frame: &Frame, w: usize, h: usize, max: usize) -> BrushDabs {
    use lcc_develop::brush_model;
    let to_out = frame.mask_to_out(w, h);
    let ppl = frame.px_per_long(w);
    let r0 = (s.size * ppl).max(0.5);
    let r0 = if r0.is_finite() { r0 } else { 0.5 };
    let r = r0 * brush_model::outer_ratio(s.feather);
    let (a, p) = brush_model::profile(s.feather);
    let mut dabs: Vec<Point> = Vec::new();
    let mut pressure: Vec<f32> = Vec::new();
    let with_pressure = !s.pressure.is_empty();
    let mut walk = DabWalk::default();
    let mut emitted = 0usize;
    let mut ts: Vec<f64> = Vec::new();
    let mut prev_p = 1.0;
    for (i, q) in s.points.iter().enumerate() {
        if dabs.len() >= max {
            break;
        }
        let Some(q_out) = to_out(*q) else { continue };
        let pq = s.pressure_at(i);
        let had = walk.prev.is_some();
        ts.clear();
        if walk_dabs(&mut dabs, &mut emitted, &mut walk, q_out, DAB_SPACING * r, &mut ts) {
            if with_pressure {
                let from = if had { prev_p } else { pq };
                pressure.extend(ts.iter().map(|t| from + (pq - from) * *t as f32));
            }
            prev_p = pq;
        }
    }
    dabs.truncate(max);
    pressure.truncate(max);
    BrushDabs {
        dabs,
        r,
        hard: 0.0,
        lrc: Some((a * r0, p)),
        flow: (s.flow / 100.0).clamp(0.0, 1.0) as f32,
        density: (s.density / 100.0).clamp(0.0, 1.0) as f32,
        erase: s.erase,
        auto: s.auto_mask,
        auto_scale: s.auto_range_scale(),
        pressure,
    }
}

pub use lcc_develop::brush_model::DAB_SPACING;

/// Where [`walk_dabs`] is along a stroke: the last sample taken and the path length still to go
/// before the next dab.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DabWalk {
    pub prev: Option<Point>,
    pub left: f64,
}

/// Lay the dabs of a Lightroom-model stroke along the segment from the walk's last sample to `q`:
/// the first sample is a dab, then one every `spacing` of path length (carried over from segment to
/// segment), so the dabs lie evenly however the samples came in: a slow drag (samples a pixel
/// apart) lays no more dabs than a fast one, and dabs saved `spacing` apart (Lightroom's) are laid
/// where they were saved. `ts` receives, per dab laid, where it lies on the segment (0..1; the first
/// sample's dab 1), for interpolating the pen pressure. A sample that didn't move (or isn't finite)
/// is skipped; returns whether `q` was taken. A stroke holding [`MAX_STROKE_DABS`] (counting
/// `emitted`) takes no more dabs.
pub fn walk_dabs(dabs: &mut Vec<Point>, emitted: &mut usize, walk: &mut DabWalk, q: Point, spacing: f64, ts: &mut Vec<f64>) -> bool {
    if !(q.x.is_finite() && q.y.is_finite()) {
        return false;
    }
    let spacing = if spacing.is_finite() { spacing.max(0.5) } else { 0.5 };
    let before = dabs.len();
    match walk.prev {
        None => {
            if *emitted < MAX_STROKE_DABS {
                dabs.push(q);
                ts.push(1.0);
            }
            walk.left = spacing;
        }
        Some(prev) => {
            let seg = prev.dist(q);
            if seg.is_nan() || seg <= STATIONARY_PX {
                return false;
            }
            // a hostile, huge segment lays at most 1 M dabs (and the stroke's budget)
            let room = MAX_STROKE_DABS.saturating_sub(*emitted).min(1_000_000);
            let mut t = walk.left;
            // (a hair of slack: dabs saved exactly `spacing` apart land on the samples)
            let end = seg + 1e-3 * spacing;
            while t <= end && dabs.len() - before < room {
                let k = (t / seg).min(1.0);
                dabs.push(prev.lerp(q, k));
                ts.push(k);
                t += spacing;
            }
            walk.left = if t <= end { spacing } else { (t - seg).max(0.0) };
        }
    }
    *emitted += dabs.len() - before;
    walk.prev = Some(q);
    true
}

/// Distance between consecutive dabs of a brush of radius `r` (pixels): a quarter of the radius,
/// so neighbouring dabs overlap and a stroke reads as continuous however fast it was painted.
#[inline]
pub fn dab_spacing(r: f64) -> f64 {
    (r / 4.0).max(0.5)
}

/// Append the dabs of one path segment, from the previous sample `prev` (already stamped) to `q`,
/// for a brush of radius `r`: evenly spaced at most [`dab_spacing`] apart, ending on `q`. The
/// committed stroke ([`brush_dabs`]) and the live one the UI draws while the button is held place
/// their dabs with this, so they cover the same pixels (issue #517). A sample where the pointer
/// hasn't moved (a stationary hold records one per frame) or that is not a finite point lays
/// nothing, so holding the brush still adds no coverage (#518). Returns whether `q` was stamped;
/// callers move `prev` on only then. A stroke holding [`MAX_STROKE_DABS`] takes no more.
pub fn push_dabs(dabs: &mut Vec<Point>, prev: Option<Point>, q: Point, r: f64) -> bool {
    let mut emitted = dabs.len();
    push_dabs_counted(dabs, &mut emitted, prev, q, r)
}

/// Append one segment using a persistent count of all dabs emitted for this stroke, including
/// dabs no longer held in `dabs`. Incremental callers retain `emitted` when clearing their buffer;
/// it advances only by the number actually appended. Acceptance and spacing match [`push_dabs`].
pub fn push_dabs_counted(dabs: &mut Vec<Point>, emitted: &mut usize, prev: Option<Point>, q: Point, r: f64) -> bool {
    push_dabs_within(dabs, emitted, prev, q, r, MAX_STROKE_DABS)
}

/// [`push_dabs_counted`] against a caller's own budget rather than [`MAX_STROKE_DABS`].
///
/// A caller that will not look at more than `max` dabs should build no more than `max`: a length
/// check after the vector exists bounds nothing, it only throws away what was already paid for
/// (#1520).
pub fn push_dabs_within(dabs: &mut Vec<Point>, emitted: &mut usize, prev: Option<Point>, q: Point, r: f64, max: usize) -> bool {
    let room = max.min(MAX_STROKE_DABS).saturating_sub(*emitted);
    if room == 0 || !(q.x.is_finite() && q.y.is_finite()) {
        return false;
    }
    let before = dabs.len();
    if let Some(prev) = prev {
        let dist = prev.dist(q);
        if dist.is_nan() || dist <= STATIONARY_PX {
            return false;
        }
        let n = (dist / dab_spacing(r)).ceil();
        // a hostile or degenerate segment (huge) gets no in-between dabs rather than a huge
        // allocation: 1 M dabs is far more than any on-screen stroke needs
        let n = if n.is_finite() { n.clamp(0.0, 1_000_000.0) as usize } else { 0 };
        // the in-between dabs and `q` together fit what is left of the stroke's budget
        let n = n.min(room);
        for k in 1..n {
            dabs.push(prev.lerp(q, k as f64 / n as f64));
        }
    }
    dabs.push(q);
    *emitted += dabs.len() - before;
    true
}

/// How much one dab paints at distance `dd` from its centre: 1 inside the hard core `hard`,
/// smoothly falling to 0 at the radius `r`.
#[inline]
pub fn dab_alpha(dd: f64, r: f64, hard: f64) -> f32 {
    if dd > r {
        0.0
    } else if dd <= hard {
        1.0
    } else {
        1.0 - smooth(hard as f32, r as f32, dd as f32)
    }
}

/// A stroke's coverage `v` after another dab paints `a` there: flow accumulates up to density.
#[inline]
pub fn accumulate(v: f32, a: f32, flow: f32, density: f32) -> f32 {
    (v + a * flow * (1.0 - v)).min(density)
}

/// Auto Mask tolerances: a pixel takes a dab's paint fully up to half of these from the colour
/// under the dab centre, none beyond them (log2 luminance, chromaticity `rgb / Y`).
pub const AUTO_TOL_EV: f32 = 0.5;
pub const AUTO_TOL_CHROMA: f32 = 0.25;

/// Auto Mask of a Lightroom-model stroke, measured on Lightroom Classic output (black-box): a pixel
/// within 0.7 EV of the colour under the dab passes fully and one 0.8 EV away is blocked (a sharp
/// cut), and at the same brightness the paint fades out with our chroma distance from 0.17 to 0.42.
pub const AUTO_LRC_EV: (f32, f32) = (0.7, 0.8);
pub const AUTO_LRC_CHROMA: (f32, f32) = (0.17, 0.42);

/// Auto Mask of a Lightroom-model stroke ([`AUTO_LRC_EV`], [`AUTO_LRC_CHROMA`]): how much a pixel (log
/// luminance `l`, chromaticity `ch`) takes the paint of a dab over a reference pixel. The cut is left
/// pixel-sharp (no edge refinement afterwards), as Lightroom's is.
#[inline]
pub fn auto_similarity_lrc(l: f32, ch: [f32; 3], rl: f32, rch: [f32; 3]) -> f32 {
    auto_similarity_lrc_scaled(l, ch, rl, rch, 1.0)
}

/// [`auto_similarity_lrc`] with every limit times `scale` (the stroke's Auto Mask range,
/// [`BrushStroke::auto_range_scale`]): the distances are divided by it, which is the same.
#[inline]
pub fn auto_similarity_lrc_scaled(l: f32, ch: [f32; 3], rl: f32, rch: [f32; 3], scale: f32) -> f32 {
    let k = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    let dl = (l - rl).abs() * k;
    let dc = ((ch[0] - rch[0]).powi(2) + (ch[1] - rch[1]).powi(2) + (ch[2] - rch[2]).powi(2)).sqrt() * k;
    (1.0 - smooth(AUTO_LRC_EV.0, AUTO_LRC_EV.1, dl)) * (1.0 - smooth(AUTO_LRC_CHROMA.0, AUTO_LRC_CHROMA.1, dc))
}

/// Chromaticity `rgb / Y` (as colour noise reduction uses it).
#[inline]
pub fn chromaticity(c: [f32; 3]) -> [f32; 3] {
    let y = lcc_color::luminance_2020(c).max(1e-6);
    [c[0] / y, c[1] / y, c[2] / y]
}

/// Auto Mask: how much a pixel (log luminance `l`, chromaticity `ch`) is like the reference.
#[inline]
pub fn auto_similarity(l: f32, ch: [f32; 3], rl: f32, rch: [f32; 3]) -> f32 {
    let dl = (l - rl) / AUTO_TOL_EV;
    let dc = ((ch[0] - rch[0]).powi(2) + (ch[1] - rch[1]).powi(2) + (ch[2] - rch[2]).powi(2)).sqrt() / AUTO_TOL_CHROMA;
    1.0 - smooth(0.5, 1.0, (dl * dl + dc * dc).sqrt())
}

/// The output pixel a dab at `d` samples its reference colour from. The position is taken at f32
/// precision (like the GPU kernel): a dab on a pixel boundary must not fall on either side of it
/// by an f64 rounding error, which depends on where the window of a zoomed view starts.
#[inline]
pub fn dab_pixel(d: Point, w: usize, h: usize) -> usize {
    let x = ((d.x as f32).floor().max(0.0) as usize).min(w - 1);
    let y = ((d.y as f32).floor().max(0.0) as usize).min(h - 1);
    y * w + x
}

/// Guided-filter refinement of an Auto Mask stroke of radius `r` px: (sigma px, epsilon EV²). The
/// stroke keeps the larger of its own and the refined alpha.
pub fn auto_refine(r: f64) -> (f32, f32) {
    ((0.25 * r).max(1.0) as f32, 0.02)
}

/// The pixels (x0, y0, x1, y1; exclusive ends) of a `w × h` plane an Auto Mask stroke's coverage
/// can reach: its dabs' extent plus the reach of its refine (none for a Lightroom-model stroke,
/// which keeps its pixel-sharp cut), clipped to the plane; `None` when it reaches none. Both
/// renderers refine a stroke over this rectangle only, with the blur's edges clamped to it.
pub fn auto_stroke_reach(bd: &BrushDabs, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    auto_stroke_reach_in(bd, (0.0, 0.0), w, h)
}

/// [`auto_stroke_reach`] in a larger `w × h` frame in which the stroke's plane starts at `origin`
/// (a window of it: its dabs move by `origin`); in the frame's pixels.
pub fn auto_stroke_reach_in(bd: &BrushDabs, origin: (f64, f64), w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    auto_stroke::refine_reach(bd, origin, w, h, auto_stroke_sigma(bd))
}

/// The sigma (pixels) of an Auto Mask stroke's refine: 0 for a Lightroom-model stroke, which keeps
/// its pixel-sharp cut.
pub fn auto_stroke_sigma(bd: &BrushDabs) -> f32 {
    if bd.lrc.is_none() { auto_refine(bd.r).0 } else { 0.0 }
}

/// What an Auto Mask stroke's coverage depends on besides the pixels it reads: its dabs as placed
/// in this `w × h` plane and its brush (the GPU renderer keeps strokes under it).
pub fn auto_stroke_key(bd: &BrushDabs, w: usize, h: usize) -> u64 {
    auto_stroke::stroke_key(bd, w, h, 0)
}

/// A guided filter's linear model at each pixel (`q = a · guide + b`, coefficients averaged over
/// the windows that hold the pixel), the window mean of the filtered plane and, when asked for,
/// how much of an edge the guide has there (`var / (var + eps)`, averaged like the coefficients:
/// 0 where the guide is flat and the model is only a blur).
struct Guided {
    a: Plane,
    b: Plane,
    mean: Plane,
    edge: Option<Plane>,
}

/// The guided filter (He, Sun and Tang) of `p` steered by `guide`, both the same size: Gaussian
/// windows of `sigma` px, `eps` the regularization (in guide units squared).
fn guided_coeffs(guide: &Plane, p: &Plane, sigma: f32, eps: f32, with_edge: bool) -> Guided {
    use lcc_raster::blur::gaussian;
    let mi = gaussian(guide, sigma);
    let mp = gaussian(p, sigma);
    let cip = gaussian(&guide.zip_map(p, |a, b| a * b), sigma);
    let cii = gaussian(&guide.map(|a| a * a), sigma);
    let mut a = Plane::new(p.width, p.height);
    let mut b = Plane::new(p.width, p.height);
    let mut edge = Plane::new(if with_edge { p.width } else { 0 }, if with_edge { p.height } else { 0 });
    for i in 0..p.data.len() {
        let var = (cii.data[i] - mi.data[i] * mi.data[i]).max(0.0);
        let k = (cip.data[i] - mi.data[i] * mp.data[i]) / (var + eps);
        a.data[i] = k;
        b.data[i] = mp.data[i] - k * mi.data[i];
        if let Some(e) = edge.data.get_mut(i) {
            *e = var / (var + eps);
        }
    }
    Guided { a: gaussian(&a, sigma), b: gaussian(&b, sigma), mean: mp, edge: with_edge.then(|| gaussian(&edge, sigma)) }
}

/// Guided filter of `p` steered by `guide` (He et al.), clamped to 0..1: `p`'s edges snap to the
/// guide's.
pub fn guided_cross(guide: &Plane, p: &Plane, sigma: f32, eps: f32) -> Plane {
    let g = guided_coeffs(guide, p, sigma, eps, false);
    let mut q = Plane::new(p.width, p.height);
    for i in 0..q.data.len() {
        q.data[i] = (g.a.data[i] * guide.data[i] + g.b.data[i]).clamp(0.0, 1.0);
    }
    q
}

/// [`guided_cross`] in its subsampled form (He and Sun, "Fast Guided Filter", 2015, as
/// [`crate::local::guided_fast`] is of the self-guided filter, at its step), in place and
/// unclamped, for a `p` that is smooth at the window's scale (a mask enlarged from a model's grid):
/// the linear model is fitted on both planes box-reduced to about 3 px of window, sampled
/// bilinearly and applied to the full-resolution guide, which is where the edge comes from. A
/// pixel moves to the filtered value by `weight` of its window mean of `p` times how much of an
/// edge the guide has there (0 keeps it exactly: where the photo is flat the filter could only
/// blur `p`). Extra memory: the reduced planes only (the coefficients are sampled per pixel, not
/// enlarged into planes).
fn guided_cross_fast(guide: &Plane, p: &mut Plane, sigma: f32, eps: f32, weight: impl Fn(f32) -> f32 + Sync + Send) {
    use lcc_raster::resample::{Filter, resize};
    let (w, h) = (p.width, p.height);
    if guide.width != w || guide.height != h || w == 0 || h == 0 || !sigma.is_finite() {
        return;
    }
    // (a window past the whole frame fits one model to it all)
    let sigma = sigma.clamp(0.5, w.max(h) as f32);
    let s = crate::local::guided_fast_step(sigma);
    let (lw, lh) = (w.div_ceil(s).max(1), h.div_ceil(s).max(1));
    let g = guided_coeffs(&resize(guide, lw, lh, Filter::Box), &resize(p, lw, lh, Filter::Box), sigma / s as f32, eps, true);
    let Some(edge) = &g.edge else {
        return;
    };
    // (the reduced planes span the same frame: their pixel per full pixel)
    let (kx, ky) = (lw as f32 / w as f32, lh as f32 / h as f32);
    for_rows(&mut p.data, w, |y, row| {
        let fy = (y as f32 + 0.5) * ky;
        for (x, (v, i)) in row.iter_mut().zip(guide.row(y)).enumerate() {
            let fx = (x as f32 + 0.5) * kx;
            let k = weight(bilinear(&g.mean, fx, fy));
            // (no edge in the photo: nothing to follow, keep the sample)
            let k = if k > 0.0 { k * smooth(0.05, 1.0, bilinear(edge, fx, fy)) } else { 0.0 };
            if k > 0.0 {
                let q = bilinear(&g.a, fx, fy) * i + bilinear(&g.b, fx, fy);
                *v += (q - *v) * k;
            }
        }
    });
}

/// Rows per band of [`stamp_dabs`]: the bands are stamped in parallel, each with the dabs that
/// reach it.
const BRUSH_BAND: usize = 32;

/// Stamp the dabs of one stroke into `out` (`w × h`, zero or partly painted): flow accumulates in
/// dab order up to the density ([`accumulate`]); Auto Mask dabs paint only pixels like the one
/// under their centre ([`auto_similarity`]).
///
/// The image is cut into bands of [`BRUSH_BAND`] rows, stamped in parallel: each band gets the
/// dabs that reach it, in stroke order, so every pixel sees the same dabs in the same order as
/// stamping them one by one over the whole plane (the same result, bit for bit). Dabs outside the
/// plane (a zoomed view's window holds a small part of a long stroke) cost nothing. A stroke over
/// a dress, 100 % zoom on a 45 MP photo, took seconds dab by dab on one core (TEST-NOTES
/// 2026-10-16 item 28).
pub fn stamp_dabs(bd: &BrushDabs, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, out: &mut Plane) {
    let chroma = if bd.auto { chroma_plane(img) } else { Vec::new() };
    stamp_dabs_with(bd, w, h, log_l, &chroma, out);
}

/// [`chromaticity`] of every pixel of `img` (an Auto Mask stroke compares each pixel with each
/// dab: computed once, not once per dab).
fn chroma_plane(img: &Rgb32f) -> Vec<[f32; 3]> {
    use rayon::prelude::*;
    img.data.par_iter().map(|c| chromaticity(*c)).collect()
}

/// [`stamp_dabs`] with the image's chromaticity given ([`chroma_plane`]; empty for a stroke
/// without Auto Mask).
fn stamp_dabs_with(bd: &BrushDabs, w: usize, h: usize, log_l: &Plane, chroma: &[[f32; 3]], out: &mut Plane) {
    use rayon::prelude::*;
    if w == 0 || h == 0 || out.data.len() < w * h {
        return;
    }
    let (dens, auto) = (bd.density, bd.auto);
    let bands = h.div_ceil(BRUSH_BAND);
    // per dab: radius, hard core, flow and (Auto Mask) the colour under its centre
    let mut info: Vec<(f64, f64, f32, Option<(f32, [f32; 3])>)> = Vec::with_capacity(bd.dabs.len());
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (k, d) in bd.dabs.iter().enumerate() {
        let (r, hard, flow) = bd.dab(k);
        let refc = if auto {
            let j = dab_pixel(*d, w, h);
            log_l.data.get(j).zip(chroma.get(j)).map(|(l, c)| (*l, *c))
        } else {
            None
        };
        info.push((r, hard, flow, refc));
        // (NaN fails these too)
        let reaches = d.x + r > 0.0 && d.x - r < w as f64 && d.y + r > 0.0 && d.y - r < h as f64;
        let Ok(k) = u32::try_from(k) else { break };
        if !reaches || (auto && refc.is_none()) {
            continue;
        }
        let b0 = ((d.y - r).floor().max(0.0) as usize) / BRUSH_BAND;
        let b1 = (((d.y + r).ceil().max(0.0) as usize).min(h).saturating_sub(1) / BRUSH_BAND).min(bands - 1);
        for list in lists.iter_mut().take(b1 + 1).skip(b0) {
            list.push(k);
        }
    }
    out.data[..w * h].par_chunks_mut(BRUSH_BAND * w).zip(lists.par_iter()).enumerate().for_each(|(b, (band, list))| {
        let top = b * BRUSH_BAND;
        let rows = band.len() / w;
        for &k in list {
            let (Some(d), Some(&(r, hard, flow, refc))) = (bd.dabs.get(k as usize), info.get(k as usize)) else { continue };
            let (y0, y1) = (((d.y - r).floor().max(0.0)) as usize, ((d.y + r).ceil().max(0.0) as usize).min(h));
            let (y0, y1) = (y0.max(top), y1.min(top + rows));
            let r2 = r * r;
            for y in y0..y1 {
                let dy = y as f64 + 0.5 - d.y;
                let left = r2 - dy * dy;
                if left < 0.0 {
                    continue;
                }
                // the row's span of the disc (a pixel of slack each side; the exact test is below)
                let half = left.sqrt();
                let x0 = ((d.x - half - 1.5).floor().max(0.0)) as usize;
                let x1 = ((d.x + half + 1.5).ceil().max(0.0) as usize).min(w);
                let row = (y - top) * w;
                for x in x0..x1 {
                    let Some(v) = band.get_mut(row + x) else { break };
                    // painted up to the density already: no further dab changes it
                    if *v >= dens {
                        continue;
                    }
                    let dx = x as f64 + 0.5 - d.x;
                    let d2 = dx * dx + dy * dy;
                    if d2 > r2 {
                        continue;
                    }
                    let mut a = bd.alpha(d2.sqrt(), r, hard);
                    if let Some((rl, rch)) = refc {
                        let i = y * w + x;
                        let (Some(l), Some(c)) = (log_l.data.get(i), chroma.get(i)) else { continue };
                        a *= if bd.lrc.is_some() {
                            auto_similarity_lrc_scaled(*l, *c, rl, rch, bd.auto_scale)
                        } else {
                            auto_similarity(*l, *c, rl, rch)
                        };
                    }
                    // flow accumulates within a stroke up to density
                    *v = accumulate(*v, a, flow, dens);
                }
            }
        }
    });
}

/// Whether the strokes of one brush mask hold more dabs than the mask's shared budget ([`MAX_STROKE_DABS`]), so that
/// rasterizing it would leave later strokes (an erase among them) unapplied. Callers that can fail check this and
/// report an error rather than render a wrong mask.
pub fn brush_over_budget(strokes: &[BrushStroke], frame: &Frame, w: usize, h: usize) -> bool {
    let mut room = MAX_STROKE_DABS;
    for s in strokes {
        if s.points.is_empty() {
            continue;
        }
        if room == 0 {
            return true;
        }
        let n = brush_dabs_within(s, frame, w, h, room).dabs.len();
        if n >= room {
            // the budget is exactly filled, or the stroke was cut off at it: one more dab says which (a stroke
            // already at its own cap [`MAX_STROKE_DABS`] is cut off, whatever it would have held)
            if room >= MAX_STROKE_DABS || brush_dabs_within(s, frame, w, h, room + 1).dabs.len() > room {
                return true;
            }
        }
        room -= n;
    }
    false
}

/// Stamp brush strokes into `out` ([`BrushDabs::combine`]: erase strokes subtract, Lightroom-model
/// strokes add up, legacy ones take the maximum). Auto Mask strokes weight
/// each dab by [`auto_similarity`] to the pixel under its centre and are refined with
/// [`guided_cross`] on log luminance.
fn rasterize_brush(strokes: &[BrushStroke], frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, out: &mut Plane) {
    if w == 0 || h == 0 {
        return;
    }
    // (each pixel's chromaticity once for every Auto Mask stroke)
    let any_auto = strokes.iter().any(|s| s.auto_mask);
    let chroma = if any_auto { chroma_plane(img) } else { Vec::new() };
    let pixels = if any_auto { auto_stroke::pixels_key(img, log_l) } else { 0 };
    let pass = auto_stroke::pass();
    // one dab budget for the whole mask, as the GPU path keeps: a stroke's own cap alone let a thousand far-flung
    // two-point strokes build and stamp a billion dabs (upstream #3681)
    let mut room = MAX_STROKE_DABS;
    for s in strokes {
        if room == 0 {
            break;
        }
        let bd = brush_dabs_within(s, frame, w, h, room);
        room = room.saturating_sub(bd.dabs.len());
        // an Auto Mask stroke: kept from the last render of these pixels, refined within its reach
        // (the budget above is charged for a kept stroke too: its dabs are part of its key)
        if bd.auto {
            auto_stroke::auto_stroke(&bd, w, h, log_l, &chroma, pixels, pass).0.combine_into(out, |o, a| bd.combine(o, a));
            continue;
        }
        let mut stroke_alpha = Plane::new(w, h);
        stamp_dabs_with(&bd, w, h, log_l, &chroma, &mut stroke_alpha);
        for (o, a) in out.data.iter_mut().zip(&stroke_alpha.data) {
            *o = bd.combine(*o, *a);
        }
    }
}

#[path = "auto_stroke.rs"]
mod auto_stroke;

#[cfg(test)]
#[path = "tests_brush_lrc.rs"]
mod tests_brush_lrc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_evaluated_mask_count_is_bounded_by_the_alpha_plane_memory() {
        // a 24 MP render keeps a few dozen planes, a 100 MP one fewer, but never fewer than MIN_MASKS
        assert!(max_masks(6000, 4000) * 6000 * 4000 * 4 <= MAX_ALPHA_BYTES);
        assert_eq!(max_masks(20_000, 20_000), MIN_MASKS);
        assert!(max_masks(1, 1) >= 1000);
        assert!(max_masks(0, 0) >= MIN_MASKS);
    }

    // Given masks, the key of their planes changes with what the planes depend on and only that:
    // the exposure counts for a range mask, not for a gradient; a mask's own sliders never count
    #[test]
    fn the_alpha_key_follows_what_the_planes_depend_on() {
        use lcc_develop::MaskOp;
        let comp = |shape| MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape };
        let linear = MaskShape::Linear { start: Point::new(0.2, 0.5), end: Point::new(0.8, 0.5) };
        let mut masks = vec![Mask { id: 1, components: vec![comp(linear)], ..Default::default() }];
        let k = alpha_key(&masks, 0.0, 100, 50, None);
        assert_eq!(alpha_key(&masks, 1.5, 100, 50, None), k, "a gradient does not read the exposure");
        assert_ne!(alpha_key(&masks, 0.0, 100, 51, None), k, "size");
        masks[0].adjust.exposure = 1.0;
        assert_eq!(alpha_key(&masks, 0.0, 100, 50, None), k, "the mask's own sliders");
        masks[0].adjust.amount = 50.0;
        let k2 = alpha_key(&masks, 0.0, 100, 50, None);
        assert_ne!(k2, k, "the amount scales the plane");
        masks[0].feather = 30.0;
        let k2 = alpha_key(&masks, 0.0, 100, 50, None);
        assert_ne!(k2, k, "feathered");
        masks[0].curve.master = vec![Point::new(0.0, 0.1), Point::new(1.0, 0.9)];
        assert_eq!(alpha_key(&masks, 0.0, 100, 50, None), k2, "the mask's own curve");
        masks[0].invert = true;
        assert_ne!(alpha_key(&masks, 0.0, 100, 50, None), k2, "inverted");
        masks[0].invert = false;
        let before = alpha_key(&masks, 0.0, 100, 50, None);
        masks[0].on_photo = !masks[0].on_photo;
        assert_ne!(alpha_key(&masks, 0.0, 100, 50, None), before, "the frame the coordinates are on (under a warp they differ)");
        masks[0].on_photo = !masks[0].on_photo;
        masks[0].invert = true;
        masks.push(Mask {
            id: 2,
            components: vec![comp(MaskShape::LuminanceRange { lo: 0.2, hi: 0.8, lo_feather: 0.1, hi_feather: 0.1 })],
            ..Default::default()
        });
        let k3 = alpha_key(&masks, 0.0, 100, 50, None);
        assert_ne!(alpha_key(&masks, 0.5, 100, 50, None), k3, "a range mask reads the exposure");
        masks[1].visible = false;
        assert_eq!(alpha_key(&masks, 0.5, 100, 50, None), alpha_key(&masks, 0.0, 100, 50, None), "hidden masks count for nothing");
        // the sky heuristic reads the exposure; a sky from the source's matte does not
        let sky = vec![Mask { id: 3, components: vec![comp(MaskShape::Sky)], ..Default::default() }];
        assert_ne!(alpha_key(&sky, 0.5, 100, 50, None), alpha_key(&sky, 0.0, 100, 50, None));
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Sky, Image { width: 4, height: 2, data: vec![255; 8] });
        assert_eq!(alpha_key(&sky, 0.5, 100, 50, Some(&mattes)), alpha_key(&sky, 0.0, 100, 50, Some(&mattes)));
    }
    use lcc_develop::{DevelopSettings, MaskComponent};

    /// The banded, parallel [`stamp_dabs`] paints exactly what stamping the dabs one by one over
    /// the whole plane does (the way it was done before), for plain and Auto Mask strokes, with
    /// pen pressure, and with dabs off the plane's edges.
    #[test]
    fn banded_dab_stamping_matches_stamping_dab_by_dab() {
        let (w, h) = (300, 171);
        let mut img = Rgb32f::new(w, h);
        for (i, p) in img.data.iter_mut().enumerate() {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            *p = [0.2 + 0.001 * x, 0.1 + 0.002 * y, if x > 150.0 { 0.6 } else { 0.05 }];
        }
        let log_l = Plane { width: w, height: h, data: img.data.iter().map(|c| lcc_color::luminance_2020(*c).max(1e-6).log2()).collect() };
        let naive = |bd: &BrushDabs| {
            let mut out = Plane::new(w, h);
            for (k, d) in bd.dabs.iter().enumerate() {
                let (r, hard, flow) = bd.dab(k);
                let refc = bd.auto.then(|| {
                    let j = dab_pixel(*d, w, h);
                    (log_l.data[j], chromaticity(img.data[j]))
                });
                let (x0, x1) = (((d.x - r).floor().max(0.0)) as usize, ((d.x + r).ceil().max(0.0) as usize).min(w));
                let (y0, y1) = (((d.y - r).floor().max(0.0)) as usize, ((d.y + r).ceil().max(0.0) as usize).min(h));
                for y in y0..y1 {
                    for x in x0..x1 {
                        let dd = Point::new(x as f64 + 0.5, y as f64 + 0.5).dist(*d);
                        if dd > r {
                            continue;
                        }
                        let mut a = dab_alpha(dd, r, hard);
                        let i = y * w + x;
                        if let Some((rl, rch)) = refc {
                            a *= auto_similarity(log_l.data[i], chromaticity(img.data[i]), rl, rch);
                        }
                        out.data[i] = accumulate(out.data[i], a, flow, bd.density);
                    }
                }
            }
            out
        };
        let f = frame(w, h);
        for (auto, pressure) in [(false, false), (true, false), (false, true)] {
            let points: Vec<Point> = (0..40).map(|i| Point::new(-0.1 + 0.03 * i as f64, 0.1 + 0.4 * ((i as f64) * 0.4).sin().abs())).collect();
            let pressure = if pressure { (0..40).map(|i| 0.2 + 0.02 * i as f32).collect() } else { Vec::new() };
            let st = BrushStroke { points, size: 0.06, feather: 60.0, flow: 40.0, density: 90.0, auto_mask: auto, pressure, ..Default::default() };
            let bd = brush_dabs(&st, &f, w, h);
            assert!(bd.dabs.len() > 50);
            let mut banded = Plane::new(w, h);
            stamp_dabs(&bd, w, h, &img, &log_l, &mut banded);
            let want = naive(&bd);
            let worst = banded.data.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-6, "auto {auto}: {worst}");
            assert!(want.data.iter().any(|v| *v > 0.5), "the stroke paints");
        }
    }

    fn frame(w: usize, h: usize) -> Frame {
        Frame::new(w, h, &DevelopSettings::default(), true)
    }

    /// With an embedded Subject matte and an automatic grid beside it, Background is the inverse of the
    /// matte (not of the grid, which Subject does not use).
    #[test]
    fn background_inverts_the_embedded_subject_matte() {
        let (w, h) = (16, 16);
        let f = frame(w, h);
        let img = Rgb32f::new(w, h);
        let log_l = Plane::new(w, h);
        let mut m = Mattes::default();
        m.push(MatteKind::Person, Image::from_fn(w, h, |x, _| if x < 8 { 255u8 } else { 0 }));
        // a grid that would select the opposite half
        let mut auto = crate::auto_mask::analyze(&Rgb32f::new(0, 0));
        auto.subject = Plane::from_fn(8, 8, |x, _| if x < 4 { 0.0 } else { 1.0 });
        m.auto = Some(std::sync::Arc::new(auto));
        let sub = shape_alpha(&MaskShape::Subject, &f, w, h, &img, &log_l, 0.0, Some(&m));
        let bg = shape_alpha(&MaskShape::Background, &f, w, h, &img, &log_l, 0.0, Some(&m));
        for (s, b) in sub.data.iter().zip(&bg.data) {
            assert!((s + b - 1.0).abs() < 1e-4, "subject {s} + background {b}");
        }
        assert!(sub.get(1, 8) > 0.9 && bg.get(14, 8) > 0.9);
    }

    /// A Depth Range selects the band of the depth grid between its bounds, at any output size;
    /// without a depth grid (no model) it selects nothing.
    #[test]
    fn depth_range_selects_its_band_of_the_depth_grid() {
        let near = MaskShape::DepthRange { lo: 0.0, hi: 0.3, feather: 0.05 };
        let mut auto = crate::auto_mask::analyze(&Rgb32f::new(0, 0));
        // nearest at the left edge, farthest at the right
        auto.depth = Some(Plane::from_fn(32, 8, |x, _| x as f32 / 31.0));
        let m = Mattes { auto: Some(std::sync::Arc::new(auto)), ..Default::default() };
        for (w, h) in [(40, 20), (400, 200)] {
            let a = shape_alpha(&near, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, Some(&m));
            assert!(a.get(1, h / 2) > 0.99 && a.get(w - 2, h / 2) < 0.01, "{w}×{h}");
            let none = shape_alpha(&near, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
            assert!(none.data.iter().all(|v| *v == 0.0));
        }
    }

    #[test]
    fn linear_gradient_ramps() {
        let f = frame(100, 50);
        let img = Rgb32f::new(100, 50);
        let l = Plane::new(100, 50);
        let a = shape_alpha(&MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) }, &f, 100, 50, &img, &l, 0.0, None);
        assert!(a.get(1, 25) > 0.99 && a.get(98, 25) < 0.01);
        assert!((a.get(50, 25) - 0.5).abs() < 0.05);
    }

    #[test]
    fn stored_segmentations_render_at_any_size_and_follow_the_crop() {
        // the left half selected, on a 16² logit grid
        let side = 16;
        let logits: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 12.0 } else { -12.0 }).collect();
        let seg = SegMask::from_logits(side, &logits);
        for shape in [
            MaskShape::Object { hint: vec![Point::new(0.25, 0.5)], exclude: vec![], seg: Some(seg.clone()), detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "left".into(), seg: Some(seg.clone()), detail: vec![], edge: 0.0 },
        ] {
            for (w, h) in [(40, 20), (400, 200)] {
                let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
                assert!(a.get(1, h / 2) > 0.99 && a.get(w - 2, h / 2) < 0.01, "{w}×{h}");
                if w >= 400 {
                    let soft = (0..w).filter(|x| (0.05..0.95).contains(&a.get(*x, h / 2))).count();
                    assert!((2..30).contains(&soft), "an anti-aliased edge, not a ramp: {soft} px");
                }
            }
        }
        // cropped to the right half: nothing left selected
        let mut d = DevelopSettings::default();
        d.crop.geometry.rect = lcc_geom::Rect::from_center(Point::new(0.75, 0.5), 0.5, 1.0);
        let f = Frame::new(40, 20, &d, true);
        let shape = MaskShape::Prompt { text: "left".into(), seg: Some(seg), detail: vec![], edge: 0.0 };
        let a = shape_alpha(&shape, &f, 20, 20, &Rgb32f::new(20, 20), &Plane::new(20, 20), 0.0, None);
        assert!(a.data.iter().all(|v| *v < 0.05));
    }

    #[test]
    fn detail_patches_replace_the_coarse_mask_inside_their_rectangle() {
        // coarse: nothing; a patch over the right half selects its own left half (x 0.5..0.75)
        let coarse = SegMask::from_logits(4, &[-12.0; 16]);
        let side = 16;
        let l: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 12.0 } else { -12.0 }).collect();
        let patch = SegMask::from_logits_in(side, &l, [0.5, 0.0, 1.0, 1.0]);
        let shape = MaskShape::Object { hint: vec![Point::new(0.6, 0.5)], exclude: vec![], seg: Some(coarse), detail: vec![patch], edge: 0.0 };
        let (w, h) = (200, 100);
        let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
        assert!(a.get(20, 50) < 0.01, "outside the patch: the coarse mask");
        assert!(a.get(120, 50) > 0.99, "inside the patch, its selected half");
        assert!(a.get(190, 50) < 0.01, "inside the patch, its unselected half");
    }

    #[test]
    fn a_hostile_number_of_detail_patches_is_capped() {
        // built in memory (a deserialized document is capped already): only the first
        // MAX_DETAIL are decoded and sampled, so this needs 4 × 4 MB, not 20 000 × 4 MB
        let side = 1024;
        let l: Vec<f32> = vec![12.0; side * side];
        let patch = SegMask::from_logits_in(side, &l, [0.0, 0.0, 0.5, 1.0]);
        let shape = MaskShape::Object { hint: vec![Point::new(0.2, 0.5)], exclude: vec![], seg: None, detail: vec![patch; 20_000], edge: 0.0 };
        let (w, h) = (64, 32);
        let t = std::time::Instant::now();
        let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
        assert!(a.get(5, 16) > 0.99 && a.get(60, 16) < 0.01);
        assert!(t.elapsed() < std::time::Duration::from_secs(300), "{:?}", t.elapsed());
    }

    #[test]
    fn the_edge_setting_hardens_or_feathers_the_transition() {
        let side = 16;
        let l: Vec<f32> = (0..side * side).map(|i| ((i % side) as f32 - 7.5) * -0.6).collect(); // a gentle ramp
        let seg = SegMask::from_logits(side, &l);
        let (w, h) = (400, 40);
        let soft_px = |edge: f64| {
            let shape = MaskShape::Prompt { text: "x".into(), seg: Some(seg.clone()), detail: vec![], edge };
            let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
            assert!(a.get(2, h / 2) > 0.9 && a.get(w - 3, h / 2) < 0.1, "still the same selection at {edge}");
            (0..w).filter(|x| (0.1..0.9).contains(&a.get(*x, h / 2))).count()
        };
        let (hard, plain, soft) = (soft_px(-100.0), soft_px(0.0), soft_px(100.0));
        assert!(hard < plain && plain < soft, "transition widths {hard} < {plain} < {soft}");
    }

    /// A dark disc (radius 0.3 of the frame) on a light background, `n × n`: the photo and its log
    /// luminance, and a model's view of it on a `side²` grid (each cell's logit from how much of the
    /// cell the disc covers, as a low-resolution segmenter sees it).
    fn disc_scene(n: usize, side: usize) -> (Rgb32f, Plane, SegMask) {
        const R: f64 = 0.3;
        let dark = |x: f64, y: f64| (x - 0.5).hypot(y - 0.5) < R;
        let lum = |x: usize, y: usize| if dark((x as f64 + 0.5) / n as f64, (y as f64 + 0.5) / n as f64) { 0.05f32 } else { 0.6 };
        let img = Rgb32f::from_fn(n, n, |x, y| [lum(x, y); 3]);
        let log_l = img.map(crate::local::log_lum);
        let logits: Vec<f32> = (0..side * side)
            .map(|i| {
                let (cx, cy) = (i % side, i / side);
                let sub = 8;
                let cover = (0..sub * sub)
                    .filter(|k| {
                        dark((cx as f64 + (k % sub) as f64 / sub as f64) / side as f64, (cy as f64 + (k / sub) as f64 / sub as f64) / side as f64)
                    })
                    .count() as f32
                    / (sub * sub) as f32;
                12.0 * (2.0 * cover - 1.0)
            })
            .collect();
        (img, log_l, SegMask::from_logits(side, &logits))
    }

    /// Alpha along the disc's horizontal radius, `d` px outside (> 0) or inside (< 0) its edge.
    fn across_disc_edge(a: &Plane, d: i32) -> f32 {
        let n = a.width;
        let edge = (0.8 * n as f64).round() as i32; // first background pixel on the right
        let x = if d > 0 { edge + d - 1 } else { edge + d };
        a.get(x as usize, n / 2)
    }

    /// A mask from a low-resolution model grid spilled 1–2 px onto the background (a bright
    /// outline once its Exposure was raised). Enlarged along the photo's edges, it is ≈ 0 one and two
    /// pixels outside the subject, ≈ 1 inside, and exactly what it was away from the edge.
    #[test]
    fn ai_masks_follow_the_photos_edge_when_enlarged() {
        let (n, side) = (512, 32); // 16 px cells, as a 288² grid on a ~4600 px export
        let (img, log_l, seg) = disc_scene(n, side);
        let shape = MaskShape::Prompt { text: "disc".into(), seg: Some(seg), detail: vec![], edge: 0.0 };
        let f = frame(n, n);
        let a = shape_alpha(&shape, &f, n, n, &img, &log_l, 0.0, None);
        for d in [1, 2] {
            assert!(across_disc_edge(&a, d) < 0.05, "{d} px outside: {}", across_disc_edge(&a, d));
            assert!(across_disc_edge(&a, -d) > 0.95, "{d} px inside: {}", across_disc_edge(&a, -d));
        }
        // all around the disc, not only along one row
        let r_px = 0.3 * n as f64;
        for k in 0..64 {
            let t = k as f64 / 64.0 * std::f64::consts::TAU;
            let at = |r: f64| a.get((256.0 + r * t.cos()) as usize, (256.0 + r * t.sin()) as usize);
            assert!(at(r_px + 2.5) < 0.05 && at(r_px - 2.5) > 0.95, "at {t:.2} rad: {} / {}", at(r_px + 2.5), at(r_px - 2.5));
        }
        // away from the edge the selection is the plain sample of the grid, to the bit
        let flat = Plane::new(n, n);
        let plain = shape_alpha(&shape, &f, n, n, &img, &flat, 0.0, None);
        for (x, y) in [(256, 256), (200, 300), (10, 10), (500, 256), (256, 505)] {
            assert_eq!(a.get(x, y), plain.get(x, y), "({x}, {y})");
        }
    }

    /// Refine Edges on top of an enlarged AI mask: the second filter finds the edge already in place, so
    /// there is no second halo and the edge doesn't move inwards.
    #[test]
    fn refine_edges_on_an_enlarged_ai_mask_neither_halos_nor_tightens() {
        let (n, side) = (512, 32);
        let (img, log_l, seg) = disc_scene(n, side);
        let f = frame(n, n);
        let mask = |refine: f64| Mask {
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Prompt { text: "disc".into(), seg: Some(seg.clone()), detail: vec![], edge: 0.0 },
            }],
            refine,
            ..Default::default()
        };
        let crossing = |a: &Plane| (0..n).rev().find(|x| a.get(*x, n / 2) > 0.5).unwrap_or(0);
        let plain = evaluate_one(&mask(0.0), &f, n, n, &img, &log_l, 0.0, None);
        for refine in [50.0, 100.0] {
            let a = evaluate_one(&mask(refine), &f, n, n, &img, &log_l, 0.0, None);
            for d in [1, 2] {
                assert!(across_disc_edge(&a, d) < 0.05, "refine {refine}, {d} px outside: {}", across_disc_edge(&a, d));
                assert!(across_disc_edge(&a, -d) > 0.95, "refine {refine}, {d} px inside: {}", across_disc_edge(&a, -d));
            }
            assert!(crossing(&a).abs_diff(crossing(&plain)) <= 1, "refine {refine}: edge at {} vs {}", crossing(&a), crossing(&plain));
        }
    }

    /// The window follows the grid, not the output: a preview and an export put the edge in the same
    /// place of the photo (and on its edge).
    #[test]
    fn enlarged_ai_masks_put_the_edge_in_the_same_place_at_any_size() {
        let side = 32;
        let mut edges = Vec::new();
        for n in [256, 512, 1024] {
            let (img, log_l, seg) = disc_scene(n, side);
            let shape = MaskShape::Prompt { text: "disc".into(), seg: Some(seg), detail: vec![], edge: 0.0 };
            let a = shape_alpha(&shape, &frame(n, n), n, n, &img, &log_l, 0.0, None);
            // the edge's sub-pixel position along the row: the integral of alpha past the centre
            let covered: f32 = (n / 2..n).map(|x| a.get(x, n / 2)).sum();
            edges.push((n / 2) as f64 + covered as f64);
            let x = edges.last().copied().unwrap_or(0.0) / n as f64;
            assert!((x - 0.8).abs() < 1.0 / n as f64, "{n} px: edge at {x:.4} of the width, the disc's at 0.8");
        }
    }

    #[test]
    fn ai_shapes_without_a_segmentation_select_nothing() {
        let f = frame(30, 30);
        let (img, l) = (Rgb32f::new(30, 30), Plane::new(30, 30));
        for shape in [
            MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "sky".into(), seg: None, detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "sky".into(), seg: Some(SegMask { side: 4, data: "damaged!".into(), rect: None }), detail: vec![], edge: 0.0 },
        ] {
            assert!(shape_alpha(&shape, &f, 30, 30, &img, &l, 0.0, None).data.iter().all(|v| *v == 0.0));
        }
    }

    /// Issue #2677: on an inverted mask, [+] adds to and [-] removes from the visible area.
    #[test]
    fn inverted_mask_add_and_subtract_act_on_the_visible_area() {
        let f = frame(100, 100);
        let img = Rgb32f::new(100, 100);
        let l = Plane::new(100, 100);
        let radial = |x: f64| MaskShape::Radial { center: Point::new(x, 0.5), rx: 0.1, ry: 0.1, angle: 0.0, feather: 20.0, invert: false };
        let comp = |op, x| MaskComponent { anchor: None, name: None, op, invert: false, shape: radial(x) };
        let at = |ops: Vec<MaskComponent>, x, y| {
            let m = Mask { components: ops, invert: true, ..Default::default() };
            evaluate(&[m], &f, 100, 100, &img, &l, 0.0, None)[0].alpha.get(x, y)
        };
        let base = || comp(MaskOp::Add, 0.2);
        // the inverted base shows everything but the first spot
        assert!(at(vec![base()], 80, 50) > 0.99 && at(vec![base()], 20, 50) < 0.01);
        // [+] at the first spot makes it visible
        assert!(at(vec![base(), comp(MaskOp::Add, 0.2)], 20, 50) > 0.99);
        // [-] at the other spot hides it
        assert!(at(vec![base(), comp(MaskOp::Subtract, 0.8)], 80, 50) < 0.01);
        // [∩] keeps only the visible part inside the shape
        let both = vec![base(), comp(MaskOp::Intersect, 0.8)];
        assert!(at(both.clone(), 80, 50) > 0.99 && at(both, 20, 50) < 0.01);
    }

    #[test]
    fn radial_inside_and_invert() {
        let f = frame(100, 100);
        let img = Rgb32f::new(100, 100);
        let l = Plane::new(100, 100);
        let shape = MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.2, angle: 0.0, feather: 20.0, invert: false };
        let a = shape_alpha(&shape, &f, 100, 100, &img, &l, 0.0, None);
        assert!(a.get(50, 50) > 0.99 && a.get(5, 5) < 0.01);
        let m = Mask { components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: true, shape }], ..Default::default() };
        let e = evaluate(&[m], &f, 100, 100, &img, &l, 0.0, None);
        assert!(e[0].alpha.get(50, 50) < 0.01);
    }

    /// Past the shared dab budget a brush mask is an error for the caller, not a mask missing its later
    /// (erase) strokes (upstream #3681).
    #[test]
    fn an_over_budget_brush_mask_is_reported() {
        let (w, h) = (64, 64);
        let far = BrushStroke { points: vec![Point::new(-4000.0, 0.5), Point::new(4000.0, 0.5)], size: 0.05, ..Default::default() };
        let near = BrushStroke { points: vec![Point::new(0.1, 0.1), Point::new(0.9, 0.9)], size: 0.05, ..Default::default() };
        assert!(!brush_over_budget(std::slice::from_ref(&near), &frame(w, h), w, h));
        assert!(brush_over_budget(&vec![far; 10], &frame(w, h), w, h));
    }

    /// #1520: a caller that will not look past its own threshold builds to that threshold, not to
    /// the stroke budget. Each segment here is admissible on its own and together they want far
    /// more than any threshold the callers apply, which is the aggregate the issue describes
    /// rather than one hostile segment.
    #[test]
    fn a_stroke_is_built_only_as_far_as_the_caller_will_look() {
        let f = frame(4000, 4000);
        // Twenty long sweeps across the output: tens of thousands of dabs between them.
        let points: Vec<Point> =
            (0..20).map(|i| if i % 2 == 0 { Point::new(0.02, 0.02 + i as f64 * 0.04) } else { Point::new(0.98, 0.02 + i as f64 * 0.04) }).collect();
        let s = BrushStroke { points, size: 0.002, feather: 50.0, flow: 100.0, density: 100.0, ..Default::default() };

        let whole = brush_dabs(&s, &f, 4000, 4000);
        assert!(whole.dabs.len() > 20_000, "the stroke really is large: {}", whole.dabs.len());
        assert!(whole.dabs.len() <= MAX_STROKE_DABS);

        // Each caller gets what it asked for and no more, however large the stroke is. 4097 and
        // 200_001 are what the GPU path and the Auto Mask spot window now ask for.
        for max in [1usize, 2, 4097, 200_001] {
            let got = brush_dabs_within(&s, &f, 4000, 4000, max);
            assert!(got.dabs.len() <= max, "asked for at most {max}, built {}", got.dabs.len());
            assert_eq!(got.r, whole.r, "the brush itself is unchanged");
        }

        // A budget the stroke never reaches changes nothing: it only ever truncates.
        let generous = brush_dabs_within(&s, &f, 4000, 4000, whole.dabs.len() + 1000);
        assert_eq!(generous.dabs, whole.dabs, "an unreached budget leaves the stroke alone");

        // The guarantee the callers rely on: a result short of the budget was never truncated,
        // so it is the stroke itself. Both callers ask for one more than they will accept, so
        // anything they keep is identical to what they drew before this change, and anything
        // truncated is something they were going to reject anyway.
        //
        // A truncated one is not a prefix: running out of room makes a segment lay its remaining
        // dabs further apart rather than stop mid-segment, which is how `push_dabs_counted`
        // already behaved at MAX_STROKE_DABS. That only ever reaches a caller that rejects it.
        let cut = brush_dabs_within(&s, &f, 4000, 4000, 500);
        assert_eq!(cut.dabs.len(), 500, "a stroke past the budget fills it exactly");
        for max in [700usize, 5_000, whole.dabs.len()] {
            let got = brush_dabs_within(&s, &f, 4000, 4000, max);
            if got.dabs.len() < max {
                assert_eq!(got.dabs, whole.dabs, "a result short of its budget is the whole stroke");
            }
        }
    }

    /// Issue #517: dabs along a long segment (a fast stroke: two pointer samples far apart) are at
    /// most [`dab_spacing`] apart, painting dab by dab (the live stroke) or the whole path at once
    /// (the committed one).
    #[test]
    fn a_long_segment_gets_dabs_at_most_the_spacing_apart() {
        let r = 12.0;
        let pts = [Point::new(10.0, 20.0), Point::new(610.0, 260.0), Point::new(611.0, 260.0), Point::new(40.0, 400.0)];
        let mut live = Vec::new();
        let mut prev = None;
        for q in pts {
            if push_dabs(&mut live, prev, q, r) {
                prev = Some(q);
            }
        }
        assert!(live.len() > 3 * 600 / 12, "{} dabs", live.len());
        assert!(live.windows(2).all(|w| w[0].dist(w[1]) <= dab_spacing(r) + 1e-9));
        assert_eq!(live.first(), pts.first());
        assert_eq!(live.last(), pts.last());
        // the committed stroke places the same dabs (in output pixels; this frame maps 1:1 scaled)
        let (w, h) = (1000, 500);
        let f = frame(w, h);
        let stroke = BrushStroke { points: pts.map(|p| Point::new(p.x / w as f64, p.y / h as f64)).to_vec(), ..Default::default() };
        let committed = brush_dabs(&stroke, &f, w, h);
        let mut again = Vec::new();
        let mut prev = None;
        for p in &stroke.points {
            let q = f.norm_to_out(w, h).apply(*p);
            if push_dabs(&mut again, prev, q, committed.r) {
                prev = Some(q);
            }
        }
        assert_eq!(committed.dabs, again);
        assert!(committed.dabs.windows(2).all(|w| w[0].dist(w[1]) <= dab_spacing(committed.r) + 1e-9));
        // many huge segments stop at the stroke budget instead of adding up without bound
        let mut big = Vec::new();
        let mut at = Point::new(0.0, 0.0);
        for i in 0..5 {
            let q = Point::new(if i % 2 == 0 { 1e12 } else { 0.0 }, 0.0);
            if push_dabs(&mut big, Some(at), q, 1.0) {
                at = q;
            }
        }
        assert!(big.len() <= MAX_STROKE_DABS, "{} dabs", big.len());
        assert!(!push_dabs(&mut big, Some(at), Point::new(5.0, 5.0), 1.0), "a full stroke takes no more");
        // a hostile segment places no in-between dabs
        let mut v = Vec::new();
        assert!(!push_dabs(&mut v, Some(Point::new(f64::NAN, 0.0)), Point::new(1.0, 1.0), r));
        assert!(!push_dabs(&mut v, None, Point::new(f64::INFINITY, 1.0), r));
        push_dabs(&mut v, Some(Point::new(0.0, 0.0)), Point::new(1e300, 1.0), 0.0);
        assert!(v.len() <= 1_000_002, "{}", v.len());
    }

    /// A temporary output buffer must spend the same stroke budget as a retained one.
    #[test]
    fn incremental_dab_buffers_share_the_stroke_budget() {
        let points = [Point::new(20.0, 20.0), Point::new(80.0, 20.0), Point::new(200.0, 80.0), Point::new(21.0, 20.0)];
        let mut retained = Vec::new();
        let mut streamed = Vec::new();
        let mut retained_count = MAX_STROKE_DABS - 3;
        let mut streamed_count = retained_count;
        let mut prev = None;
        for q in points {
            let mut chunk = Vec::new();
            let accepted = push_dabs_counted(&mut retained, &mut retained_count, prev, q, 10.0);
            assert_eq!(accepted, push_dabs_counted(&mut chunk, &mut streamed_count, prev, q, 10.0));
            streamed.extend(chunk);
            if accepted {
                prev = Some(q);
            }
        }
        assert_eq!(retained, streamed);
        assert_eq!(retained.len(), 3);
        assert_eq!(retained_count, MAX_STROKE_DABS);
        assert_eq!(streamed_count, retained_count);
        assert_eq!(prev, Some(points[1]));
        assert!(!retained.contains(&points[2]));
        assert!(!retained.contains(&points[3]));
    }

    /// Pen pressure scales each dab's radius and flow; a stroke without pressure data paints as before, and hostile
    /// pressures (NaN, out of range, a short list) neither panic nor paint more than full pressure.
    #[test]
    fn pen_pressure_scales_dab_size_and_flow() {
        let (w, h) = (400, 100);
        let f = frame(w, h);
        let img = Rgb32f::new(w, h);
        let l = Plane::new(w, h);
        let line = vec![Point::new(0.1, 0.5), Point::new(0.9, 0.5)];
        let alpha = |pressure: Vec<f32>| {
            let s = BrushStroke { points: line.clone(), size: 0.05, feather: 0.0, flow: 100.0, pressure, ..Default::default() };
            shape_alpha(&MaskShape::Brush { strokes: vec![s] }, &f, w, h, &img, &l, 0.0, None)
        };
        let full = alpha(vec![]);
        assert_eq!(full.data, alpha(vec![1.0, 1.0]).data, "full pressure is no pressure");
        // light at the start, full at the end: thin and faint on the left, as wide as the brush on the right
        let ramp = alpha(vec![0.1, 1.0]);
        let width = |a: &Plane, x: usize| (0..h).filter(|y| a.get(x, *y) > 0.01).count();
        assert!(width(&ramp, 60) < width(&full, 60) / 2, "{} vs {}", width(&ramp, 60), width(&full, 60));
        assert!(width(&ramp, 350) + 2 >= width(&full, 350));
        assert!(ramp.get(60, 50) < full.get(60, 50) + 1e-6);
        let faint = alpha(vec![0.2]);
        assert!(faint.data.iter().zip(&full.data).all(|(a, b)| *a <= *b + 1e-6), "never more than full pressure");
        assert!(faint.get(200, 50) > 0.0 && faint.get(200, 50) < full.get(200, 50));
        for hostile in [vec![f32::NAN, f32::INFINITY], vec![-3.0], vec![0.5; 10_000]] {
            let a = alpha(hostile);
            assert!(a.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
        assert_eq!(pressure_radius(10.0, f32::NAN), 10.0);
        assert_eq!(pressure_radius(10.0, 0.0), 1.0);
        assert_eq!(pressure_flow(0.8, 2.0), 0.8);
    }

    #[test]
    fn brush_and_subtract() {
        let f = frame(200, 100);
        let img = Rgb32f::new(200, 100);
        let l = Plane::new(200, 100);
        let stroke = BrushStroke { points: vec![Point::new(0.1, 0.5), Point::new(0.9, 0.5)], size: 0.05, ..Default::default() };
        let erase = BrushStroke { points: vec![Point::new(0.5, 0.5)], size: 0.05, erase: true, feather: 0.0, ..Default::default() };
        let m = Mask {
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Brush { strokes: vec![stroke, erase] },
            }],
            ..Default::default()
        };
        let e = evaluate(&[m], &f, 200, 100, &img, &l, 0.0, None);
        let a = &e[0].alpha;
        assert!(a.get(40, 50) > 0.9, "{}", a.get(40, 50));
        assert!(a.get(100, 50) < 0.05, "erased centre {}", a.get(100, 50));
        assert!(a.get(40, 5) < 0.01);
    }

    /// #518: holding the brush still adds no coverage. A press plus any number of stationary
    /// pointer samples paints exactly the single dab of the press, whatever the frame rate.
    #[test]
    fn holding_the_brush_still_adds_no_coverage() {
        let (w, h) = (200, 100);
        let f = frame(w, h);
        let img = Rgb32f::new(w, h);
        let l = Plane::new(w, h);
        let p = Point::new(0.5, 0.5);
        let spot = |samples: usize, flow: f64| {
            let s = BrushStroke { points: vec![p; samples], size: 0.1, feather: 80.0, flow, ..Default::default() };
            shape_alpha(&MaskShape::Brush { strokes: vec![s] }, &f, w, h, &img, &l, 0.0, None)
        };
        for flow in [100.0, 40.0] {
            let press = spot(1, flow);
            // the feathered edge is soft to begin with
            let edge = press.get(100 + 14, 50);
            assert!(edge > 0.05 && edge < 0.6, "flow {flow}: edge {edge}");
            for n in [2, 5, 60, 600] {
                let held = spot(n, flow);
                assert!(held.data == press.data, "flow {flow}: {n} stationary samples hardened the edge {edge} → {}", held.get(100 + 14, 50));
            }
        }
        let s = BrushStroke { points: vec![p; 60], size: 0.1, ..Default::default() };
        assert_eq!(brush_dabs(&s, &f, w, h).dabs.len(), 1, "one dab for the press");
        // a stroke that pauses partway lays the same dabs as one that doesn't
        let (a, b) = (Point::new(0.2, 0.5), Point::new(0.8, 0.5));
        let paused = BrushStroke { points: vec![a, p, p, p, p, b], flow: 30.0, ..Default::default() };
        let smooth = BrushStroke { points: vec![a, p, b], flow: 30.0, ..Default::default() };
        assert_eq!(brush_dabs(&paused, &f, w, h).dabs, brush_dabs(&smooth, &f, w, h).dabs);
    }

    /// Feather: a hard mask edge softens, by the same relative width at any resolution.
    #[test]
    fn feather_softens_edges_resolution_independently() {
        let alpha_at = |w: usize, h: usize, feather: f64, fx: f64| {
            let img = Rgb32f::from_fn(w, h, |_, _| [0.2; 3]);
            let log_l = img.map(crate::local::log_lum);
            let f = Frame::new(w, h, &Default::default(), true);
            let m = Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Linear { start: Point::new(0.501, 0.5), end: Point::new(0.499, 0.5) },
                }],
                feather,
                ..Default::default()
            };
            let a = evaluate_one(&m, &f, w, h, &img, &log_l, 0.0, None);
            a.get((fx * w as f64) as usize, h / 2)
        };
        // hard: just inside the edge is fully selected; feathered it is partly
        let (hard, soft) = (alpha_at(200, 100, 0.0, 0.52), alpha_at(200, 100, 100.0, 0.52));
        assert!(hard > 0.99 && soft < 0.95 && soft > 0.5, "hard {hard}, feathered {soft}");
        let big = alpha_at(400, 200, 100.0, 0.52);
        assert!((big - soft).abs() < 0.05, "same falloff at twice the size: {soft} vs {big}");
    }

    /// Feather softens a radial mask's edge but keeps its shape: the half-selected contour of an
    /// ellipse stays where it was (no rounding), along both axes.
    #[test]
    fn feather_keeps_a_radial_masks_shape() {
        let (w, h) = (400, 200);
        let img = Rgb32f::from_fn(w, h, |_, _| [0.2; 3]);
        let log_l = img.map(crate::local::log_lum);
        let f = Frame::new(w, h, &Default::default(), true);
        let extents = |feather: f64| {
            let m = Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.3, ry: 0.1, angle: 0.0, feather: 50.0, invert: false },
                }],
                feather,
                ..Default::default()
            };
            let a = evaluate_one(&m, &f, w, h, &img, &log_l, 0.0, None);
            let ex = (w / 2..w).take_while(|&x| a.get(x, h / 2) >= 0.5).count() as f64;
            let ey = (h / 2..h).take_while(|&y| a.get(w / 2, y) >= 0.5).count() as f64;
            eprintln!("feather {feather}: ex {ex} ey {ey} centre {}", a.get(w / 2, h / 2));
            (ex, ey)
        };
        let (x0, y0) = extents(0.0);
        let (x1, y1) = extents(100.0);
        assert!((x1 - x0).abs() <= 3.0 && (y1 - y0).abs() <= 3.0, "contour moved: {x0}x{y0} -> {x1}x{y1}");
    }

    /// A mask whose first component is Intersect starts from that component (upstream #3854): it
    /// used to start from nothing, so alone it selected nothing.
    #[test]
    fn a_mask_starting_with_intersect_selects_its_first_shape() {
        let (w, h) = (100, 100);
        let img = Rgb32f::from_fn(w, h, |_, _| [0.2; 3]);
        let log_l = img.map(crate::local::log_lum);
        let f = Frame::new(w, h, &Default::default(), true);
        let shape = MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.2, angle: 0.0, feather: 20.0, invert: false };
        let m =
            Mask { components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Intersect, invert: false, shape }], ..Default::default() };
        let a = evaluate_one(&m, &f, w, h, &img, &log_l, 0.0, None);
        assert!(a.get(50, 50) > 0.99 && a.get(5, 5) < 0.01, "the radial's inside is selected, its outside not: {} {}", a.get(50, 50), a.get(5, 5));
    }

    /// A half-resolution matte selecting the left `frac` of a `w × h` source.
    fn left_matte(w: usize, h: usize, frac: f64) -> Image<u8> {
        Image::from_fn(w / 2, h / 2, |x, _| if (x as f64 + 0.5) < frac * (w / 2) as f64 { 255 } else { 0 })
    }

    /// The Sky mask follows the source's sky matte where it has one (here: the bottom-left of a
    /// uniformly bright blue photo, where the heuristic sees no sky, and not its top right, where
    /// it does), through user orientation, at any output size; without one it keeps the heuristic.
    #[test]
    fn sky_uses_the_sources_matte() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.5, 0.7, 1.4]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Sky, left_matte(w, h, 0.5));
        let f = frame(w, h);
        let heuristic = shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, None);
        let matte = shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, Some(&mattes));
        assert!(heuristic.get(150, 5) > 0.9 && heuristic.get(25, 90) < 0.1, "{} {}", heuristic.get(150, 5), heuristic.get(25, 90));
        assert!(matte.get(150, 5) < 0.01 && matte.get(25, 90) > 0.99, "{} {}", matte.get(150, 5), matte.get(25, 90));
        // mattes of other kinds leave the heuristic alone
        let mut hair = Mattes::default();
        hair.push(MatteKind::Hair, left_matte(w, h, 0.5));
        assert_eq!(shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, Some(&hair)), heuristic);
        // rotated 90° clockwise, the source's left half is the top half; a preview and a larger
        // render agree
        let s = lcc_develop::DevelopSettings { orientation: Orientation::Rotate90, ..Default::default() };
        let f = Frame::new(w, h, &s, true);
        for (ow, oh) in [(100, 200), (25, 50)] {
            let small = Rgb32f::filled(ow, oh, [0.5, 0.7, 1.4]);
            let a = shape_alpha(&MaskShape::Sky, &f, ow, oh, &small, &small.map(crate::local::log_lum), 0.0, Some(&mattes));
            let at = |x: f64, y: f64| a.get((x * ow as f64) as usize, (y * oh as f64) as usize);
            assert!(at(0.5, 0.2) > 0.99 && at(0.5, 0.8) < 0.01, "{ow}×{oh}: {} {}", at(0.5, 0.2), at(0.5, 0.8));
        }
    }

    /// Subject, Background and People use person mattes: the whole person is the union of the
    /// parts; a People part without a matte keeps the heuristic.
    #[test]
    fn people_and_subject_use_person_mattes() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.2; 3]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Skin, left_matte(w, h, 0.25));
        let right: Image<u8> = Image::from_fn(w / 2, h / 2, |x, _| if x >= 75 { 255 } else { 0 });
        mattes.push(MatteKind::Hair, right);
        let f = frame(w, h);
        let eval = |shape: &MaskShape| shape_alpha(shape, &f, w, h, &img, &l, 0.0, Some(&mattes));
        let subject = eval(&MaskShape::Subject);
        assert!(subject.get(10, 50) > 0.99 && subject.get(190, 50) > 0.99 && subject.get(100, 50) < 0.01);
        let background = eval(&MaskShape::Background);
        assert!(background.get(100, 50) > 0.99 && background.get(10, 50) < 0.01);
        let hair = eval(&MaskShape::People { person: 0, parts: vec!["Hair".into()], who: None, seg: None });
        assert!(hair.get(190, 50) > 0.99 && hair.get(10, 50) < 0.01);
        let lips = MaskShape::People { person: 0, parts: vec!["Lips".into()], who: None, seg: None };
        assert_eq!(eval(&lips), shape_alpha(&lips, &f, w, h, &img, &l, 0.0, None));
    }

    #[test]
    fn a_people_selection_the_models_computed_wins_over_mattes_and_the_heuristic() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.2; 3]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Skin, left_matte(w, h, 0.25));
        // the right half of the photo selected
        let side = 64;
        let logits: Vec<f32> = (0..side * side).map(|i| if i % side >= side / 2 { 8.0 } else { -8.0 }).collect();
        let seg = lcc_develop::SegMask::from_logits(side, &logits);
        let shape = MaskShape::People { person: 1, parts: vec!["Face Skin".into()], who: None, seg: Some(seg) };
        let a = shape_alpha(&shape, &frame(w, h), w, h, &img, &l, 0.0, Some(&mattes));
        assert!(a.get(190, 50) > 0.9 && a.get(10, 50) < 0.1, "{} {}", a.get(190, 50), a.get(10, 50));
    }
}
