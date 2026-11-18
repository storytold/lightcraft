//! Output framing: user orientation, lens corrections and perspective ([`Warp`]), crop + straighten,
//! flips — and the mapping between output pixels and normalized coordinates (used by masks, spots and
//! on-canvas tools).
//!
//! Normalized coordinates (masks, spots, crop) refer to the *transformed* image: the oriented source after
//! lens correction and perspective, at the source's size. Everything is sampled in one resample.

use lcc_develop::{DevelopSettings, EmbeddedLens};
use lcc_geom::{Affine, CropGeometry, Homography, Orientation, Point, Rect};
use lcc_raster::resample::{Filter, resize};
use lcc_raster::{Coverage, Plane, Rgb32f, par_rows};

use crate::optics::{Warp, reorient_lens};

/// Linear value of areas outside the source after a warp (a light neutral, like an empty canvas).
pub const BLANK: [f32; 3] = [0.55, 0.55, 0.55];

/// The most pixels a render enlarged past its photo's own size can have: 512 megapixels,
/// Lightroom's ceiling too, and the same cap the export path applies (`MAX_ENLARGED_PIXELS` in
/// `crates/engine/src/export/options.rs`; that path was already bounded — this bounds the pipeline
/// itself, so every caller of [`Frame::fit`] is covered). An output at or under the photo's own
/// pixel count is never held to the ceiling, so a full-size render of a photo larger than this is
/// unaffected. [`Frame::fit`] scales an over-ceiling output down to it instead of allocating it: a
/// request box far beyond the photo aborted the process on allocation, which `catch_unwind` cannot
/// catch (issue #3320).
pub const MAX_RENDER_PIXELS: f64 = 512e6;

/// The geometric frame of a render.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Source (EXIF-oriented) dimensions in pixels.
    pub src_w: usize,
    pub src_h: usize,
    /// User orientation (Rotate Left/Right, Flip) on top of the source.
    pub orient: Orientation,
    /// Oriented dimensions.
    pub ow: f64,
    pub oh: f64,
    pub crop: CropGeometry,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Lens corrections / perspective (None = identity).
    pub warp: Option<Warp>,
    /// Set when this frame draws only a window of a larger output (see [`Frame::window`]).
    pub view: Option<ViewWindow>,
    /// Mask coordinates are on the photo before the warp ([`Frame::for_mask`],
    /// `lcc_develop::Mask::on_photo`); otherwise normalized coordinates of the transformed image.
    pub masks_on_photo: bool,
    /// Filter of the area downscale ahead of the bilinear resample ([`Frame::sample`]); see
    /// [`crate::Downscale`].
    pub downscale: Filter,
}

/// Where a windowed [`Frame`]'s pixels sit in the full output: its offset and the full output's
/// size, for effects defined by the whole frame (post-crop vignette).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewWindow {
    pub x: f64,
    pub y: f64,
    pub full_w: f64,
    pub full_h: f64,
}

impl Frame {
    pub fn new(src_w: usize, src_h: usize, s: &DevelopSettings, apply_crop: bool) -> Frame {
        Frame::with_lens(src_w, src_h, s, apply_crop, None)
    }

    /// Like [`Frame::new`], with the source's embedded lens corrections (relative to the EXIF-oriented source).
    pub fn with_lens(src_w: usize, src_h: usize, s: &DevelopSettings, apply_crop: bool, lens: Option<&EmbeddedLens>) -> Frame {
        let orient = s.orientation;
        let (ow, oh) = if orient.swaps_axes() { (src_h as f64, src_w as f64) } else { (src_w as f64, src_h as f64) };
        // bounded: a crop from a sidecar or an agent can reach far past the photo (issue #2555)
        let crop = if apply_crop { s.crop.geometry.bounded(ow, oh) } else { CropGeometry::default() };
        let lens = lens.map(|l| reorient_lens(l, orient, src_w as f64, src_h as f64));
        let mut warp = Warp::from_settings(ow, oh, s, lens.as_ref());
        let persp = perspective(s, ow, oh);
        if persp != Homography::IDENTITY
            && let Some(inv) = persp.inverse()
        {
            warp.persp = persp;
            warp.persp_inv = inv;
        }
        let mut f = Frame {
            src_w,
            src_h,
            orient,
            ow,
            oh,
            crop,
            // a flip is not part of the crop: the crop tool's uncropped view shows it too
            flip_h: s.crop.flip_h,
            flip_v: s.crop.flip_v,
            warp: (!warp.is_identity()).then_some(warp),
            view: None,
            downscale: Filter::Mitchell,
            masks_on_photo: false,
        };
        if apply_crop {
            f.constrain_to_image(s.geometry.constrain_crop);
        }
        f
    }

    /// `crop` as the crop tool draws it on this frame's photo: shrunk about its centre as the
    /// render shrinks it (lens corrections always, perspective with Constrain Crop), so the box the
    /// tool shows is what the render crops.
    pub fn constrained_crop(&self, crop: CropGeometry, with_persp: bool) -> CropGeometry {
        let mut f = Frame { crop, view: None, ..self.clone() };
        f.constrain_to_image(with_persp);
        f.crop
    }

    /// Shrink the crop about its centre until it contains no area outside the warped source. Lens
    /// corrections (profile, embedded camera warp, manual distortion, lateral CA) always do this, as
    /// a corrected lens never shows the blank past the photo's edge (issue #245); perspective only
    /// with "Constrain Crop" (`with_persp`), so a keystone fix can keep its corners.
    pub(crate) fn constrain_to_image(&mut self, with_persp: bool) {
        let Some(full) = self.warp.as_ref().filter(|w| w.moves_pixels()) else { return };
        let optics;
        let wp = if with_persp || full.persp == Homography::IDENTITY {
            full
        } else {
            let mut o = full.clone();
            o.persp = Homography::IDENTITY;
            o.persp_inv = Homography::IDENTITY;
            optics = o;
            &optics
        };
        if !wp.moves_pixels() {
            return;
        }
        let planes: &[usize] = if wp.per_channel() { &[0, 1, 2] } else { &[1] };
        let a = self.crop.output_to_source(self.ow, self.oh, 1.0, 1.0);
        let (w, h) = (self.ow, self.oh);
        let fits = |t: f64| {
            (0..=48).all(|i| {
                let u = i as f64 / 48.0;
                [(u, 0.0), (u, 1.0), (0.0, u), (1.0, u)].iter().all(|&(x, y)| {
                    let p = Point::new(0.5 + t * (x - 0.5), 0.5 + t * (y - 0.5));
                    planes.iter().all(|&ch| {
                        let s = wp.to_source(a.apply(p), ch);
                        s.x >= -1e-6 * w && s.y >= -1e-6 * h && s.x <= w * (1.0 + 1e-6) && s.y <= h * (1.0 + 1e-6)
                    })
                })
            })
        };
        if fits(1.0) {
            return;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..30 {
            let m = (lo + hi) / 2.0;
            if fits(m) { lo = m } else { hi = m }
        }
        let t = lo.max(0.02);
        let r = self.crop.rect;
        self.crop.rect = Rect::from_center(r.center(), r.width() * t, r.height() * t);
    }

    /// Long edge, in pixels, of the whole output this frame draws (its own size unless it is a
    /// window of a larger one): what size-relative effects (noise reduction, edge refinement)
    /// must scale with, so a window matches the same pixels of the whole render.
    pub fn output_long(&self, w: usize, h: usize) -> usize {
        self.view.map_or(w.max(h), |v| v.full_w.max(v.full_h) as usize)
    }

    /// This frame narrowed to `win` of its `full_w × full_h` output: the same mapping, but the
    /// output is only that window (the crop rectangle shrinks to it; flips are as drawn).
    pub fn window(&self, full_w: usize, full_h: usize, win: crate::PixelWindow) -> Frame {
        let (fw, fh) = (full_w.max(1) as f64, full_h.max(1) as f64);
        let (x0, x1) = (win.x as f64 / fw, (win.x + win.w) as f64 / fw);
        let (y0, y1) = (win.y as f64 / fh, (win.y + win.h) as f64 / fh);
        // flips happen on the output before it maps onto the crop: undo them for the range
        let (x0, x1) = if self.flip_h { (1.0 - x1, 1.0 - x0) } else { (x0, x1) };
        let (y0, y1) = if self.flip_v { (1.0 - y1, 1.0 - y0) } else { (y0, y1) };
        let r = self.crop.rect;
        let (rw, rh) = (r.width(), r.height());
        let mut f = self.clone();
        f.crop.rect = Rect { x0: r.x0 + x0 * rw, y0: r.y0 + y0 * rh, x1: r.x0 + x1 * rw, y1: r.y0 + y1 * rh };
        f.view = Some(ViewWindow { x: win.x as f64, y: win.y as f64, full_w: fw, full_h: fh });
        f
    }

    /// Add automatically estimated lateral CA (`[α_R, α_B]`, see [`crate::optics::estimate_lateral_ca`]).
    pub fn add_lateral_ca(&mut self, ca: [f64; 2]) {
        if ca == [0.0; 2] {
            return;
        }
        let w = self.warp.get_or_insert_with(|| Warp::identity(self.ow, self.oh));
        w.ca[0] += ca[0];
        w.ca[2] += ca[1];
    }

    /// Apply a flat-field map's gain ([`crate::vignetting_calibration::field_for`]). It takes the place of the lens profile's (or
    /// the file's) vignetting, which would correct the same fall-off twice; the profile's distortion and lateral CA
    /// and manual vignetting stay. A gain moves no pixels, so the crop is unaffected.
    pub fn add_vignetting_calibration(&mut self, field: Option<std::sync::Arc<crate::vignetting_calibration::FieldGain>>) {
        let Some(field) = field else { return };
        let w = self.warp.get_or_insert_with(|| Warp::identity(self.ow, self.oh));
        w.field = Some(field);
        w.lens_vig = 0.0;
        if w.is_identity() {
            self.warp = None;
        }
    }

    /// Map a point of the transformed image (normalized) to the lens-corrected, pre-perspective image (normalized).
    pub fn transformed_to_corrected(&self, p: Point) -> Point {
        match &self.warp {
            Some(w) => {
                let q = w.to_corrected(Point::new(p.x * self.ow, p.y * self.oh));
                Point::new(q.x / self.ow, q.y / self.oh)
            }
            None => p,
        }
    }

    /// Inverse of [`Frame::transformed_to_corrected`].
    pub fn corrected_to_transformed(&self, p: Point) -> Point {
        match &self.warp {
            Some(w) => {
                let q = w.from_corrected(Point::new(p.x * self.ow, p.y * self.oh));
                Point::new(q.x / self.ow, q.y / self.oh)
            }
            None => p,
        }
    }

    /// Aspect ratio (w/h) of the output. A crop narrower than a source pixel (or of no size, from
    /// an agent or a sidecar) counts each side as one pixel, so the aspect stays finite: 0/0 made
    /// it NaN, and the loupe laid the photo out at a NaN size.
    pub fn aspect(&self) -> f64 {
        let r = self.crop.rect_px(self.ow, self.oh);
        r.width().max(1.0) / r.height().max(1.0)
    }

    /// Size of the (cropped) output at the source's own resolution, in pixels.
    pub fn native_size(&self) -> (f64, f64) {
        let r = self.crop.rect_px(self.ow, self.oh);
        (r.width(), r.height())
    }

    /// The output size that fits in `max_w × max_h` at this frame's aspect: the box itself when it
    /// already matches up to rounding (an export size), otherwise the largest aspect-true size
    /// inside it. A result of more pixels than the ceiling ([`MAX_RENDER_PIXELS`], or this frame's
    /// own pixel count when larger) is scaled down to it, both sides by the same factor, so the
    /// aspect survives: every size the ceiling allows is returned exactly as before the ceiling
    /// existed, and a request box far beyond the photo comes back at the ceiling instead of being
    /// allocated, which aborted the process (issue #3320).
    pub fn fit(&self, max_w: usize, max_h: usize) -> (usize, usize) {
        let a = self.aspect();
        let (mw, mh) = (max_w.max(1) as f64, max_h.max(1) as f64);
        // a box that is already this aspect up to rounding (an export size) is the size: fitting
        // into it again rounds twice and can lose a pixel of the edge that was asked for
        let box_is_size = (mw - 0.5) / a <= mh + 0.5 && (mw + 0.5) / a >= mh - 0.5;
        let (w, h) = if box_is_size {
            (mw, mh)
        } else if mw / mh > a {
            (mh * a, mh)
        } else {
            (mw, mw / a)
        };
        // the sizing is all f64 (a `usize::MAX` box overflows the byte count in usize before the
        // allocation could even fail) and the cast happens only on the clamped result
        let ceiling = self.pixel_ceiling();
        let px = w * h;
        let (w, h) = if px > ceiling {
            let k = (ceiling / px).sqrt();
            // (at least one pixel a side, as the engine's resize does: for an aspect more extreme
            // than the ceiling itself the limit wins and the sides stay whole pixels)
            ((w * k).max(1.0), (h * k).max(1.0))
        } else {
            (w, h)
        };
        ((w.round() as usize).max(1), (h.round() as usize).max(1))
    }

    /// The most pixels this frame's render may have: [`MAX_RENDER_PIXELS`], or this frame's own
    /// (cropped) pixel count when that is larger — a render is never held below the photo's own
    /// size. The exemption is the photo's area at most: the crop rectangle comes from settings (a
    /// sidecar, a preset, `develop.merge`, `render --settings`), and a turned crop is deliberately
    /// left unclipped to 0..1 (`DevelopSettings::keep_in_range`, issue #1946), so it can name more
    /// pixels than the photo has and must not lift the ceiling with them. The comparison is by
    /// area, not per side, because a legitimate straightened crop can be wider than the photo
    /// itself while its corners still lie inside it — clamping a side would shrink a native render
    /// that fits.
    pub(crate) fn pixel_ceiling(&self) -> f64 {
        let (nw, nh) = self.native_size();
        (nw * nh).min(self.ow * self.oh).max(MAX_RENDER_PIXELS)
    }

    /// Output pixels per unit of the oriented image's long edge.
    pub fn px_per_long(&self, out_w: usize) -> f64 {
        let r = self.crop.rect_px(self.ow, self.oh);
        out_w as f64 / r.width() * self.ow.max(self.oh)
    }

    /// Affine from output pixel coordinates to oriented-image pixel coordinates.
    pub fn out_to_oriented(&self, out_w: usize, out_h: usize) -> Affine {
        let (w, h) = (out_w as f64, out_h as f64);
        let mut flip = Affine::IDENTITY;
        if self.flip_h {
            flip = Affine([-1.0, 0.0, 0.0, 1.0, w, 0.0]) * flip;
        }
        if self.flip_v {
            flip = Affine([1.0, 0.0, 0.0, -1.0, 0.0, h]) * flip;
        }
        self.crop.output_to_source(self.ow, self.oh, w, h) * flip
    }

    /// Map an output pixel to normalized oriented-image coordinates (0..1).
    pub fn out_to_norm(&self, out_w: usize, out_h: usize) -> Affine {
        Affine::scale(1.0 / self.ow, 1.0 / self.oh) * self.out_to_oriented(out_w, out_h)
    }

    /// Normalized oriented coords → output pixel (inverse of `out_to_norm`).
    pub fn norm_to_out(&self, out_w: usize, out_h: usize) -> Affine {
        self.out_to_norm(out_w, out_h).inverse().unwrap_or(Affine::IDENTITY)
    }

    /// This frame for evaluating a mask whose coordinates are on the photo before the warp
    /// (`on_photo`, [`lcc_develop::Mask::on_photo`]) or on the corrected frame.
    pub fn for_mask(&self, on_photo: bool) -> std::borrow::Cow<'_, Frame> {
        if self.masks_on_photo == on_photo {
            std::borrow::Cow::Borrowed(self)
        } else {
            std::borrow::Cow::Owned(Frame { masks_on_photo: on_photo, ..self.clone() })
        }
    }

    /// The warp mask coordinates go through: the lens / perspective warp when masks are on the
    /// photo and it moves pixels, else none (mask coordinates are then normalized coordinates).
    pub fn mask_warp(&self) -> Option<&Warp> {
        self.warp.as_ref().filter(|w| self.masks_on_photo && w.moves_pixels())
    }

    /// Output pixel → mask coordinates (see [`Frame::masks_on_photo`]).
    pub fn out_to_mask(&self, out_w: usize, out_h: usize) -> MaskMap {
        MaskMap {
            o2t: self.out_to_oriented(out_w, out_h),
            to_norm: self.out_to_norm(out_w, out_h),
            ow: self.ow,
            oh: self.oh,
            warp: self.mask_warp().cloned(),
        }
    }

    /// Mask coordinates → output pixel (the inverse of [`Frame::out_to_mask`]); `None` for a
    /// point the warp can't invert (far outside a strong one), which has no place in the output.
    pub fn mask_to_out(&self, out_w: usize, out_h: usize) -> impl Fn(Point) -> Option<Point> + use<> {
        let to_out = self.norm_to_out(out_w, out_h);
        let (warp, ow, oh) = (self.mask_warp().cloned(), self.ow, self.oh);
        move |n: Point| match &warp {
            Some(w) => w.from_source(Point::new(n.x * ow, n.y * oh)).map(|t| to_out.apply(Point::new(t.x / ow, t.y / oh))),
            None => Some(to_out.apply(n)),
        }
    }

    /// Normalized coordinates of this frame's transformed image → mask coordinates.
    pub fn norm_to_mask(&self, n: Point) -> Point {
        match self.mask_warp() {
            Some(w) => {
                let s = w.to_source(Point::new(n.x * self.ow, n.y * self.oh), 1);
                Point::new(s.x / self.ow, s.y / self.oh)
            }
            None => n,
        }
    }

    /// Mask coordinates → normalized coordinates of this frame's transformed image.
    pub fn mask_to_norm(&self, n: Point) -> Point {
        mask_to_norm(self.mask_warp(), self.ow, self.oh, n)
    }

    /// Convert normalized oriented coordinates to "long-edge units" (isotropic), used by mask shapes.
    pub fn norm_to_long(&self, p: Point) -> Point {
        let l = self.ow.max(self.oh);
        Point::new(p.x * self.ow / l, p.y * self.oh / l)
    }

    /// How [`Frame::sample`] resamples a (EXIF-oriented) source of `src_w × src_h` into `w × h`.
    pub fn sample_plan(&self, src_w: usize, src_h: usize, w: usize, h: usize) -> SamplePlan {
        let (ow, oh) = if self.orient.swaps_axes() { (src_h, src_w) } else { (src_w, src_h) };
        let crop_px = self.crop.rect_px(self.ow, self.oh);
        let k = crop_px.width() / w as f64;
        let prefilter = (k > 1.25).then(|| (((ow as f64 / k).round() as usize).max(1), ((oh as f64 / k).round() as usize).max(1)));
        let (bw, bh) = prefilter.unwrap_or((ow, oh));
        let (sx, sy) = (bw as f64 / ow as f64, bh as f64 / oh as f64);
        let mode = if self.warp.is_some() {
            SampleMode::Warp(self.out_to_oriented(w, h))
        } else if self.crop.is_identity() && !self.flip_h && !self.flip_v && bw == w && bh == h {
            SampleMode::Copy
        } else {
            SampleMode::Affine(Affine::scale(sx, sy) * self.out_to_oriented(w, h))
        };
        SamplePlan { ow, oh, prefilter, sx, sy, mode }
    }

    /// Which pixels of a `w × h` output of a `src_w × src_h` source [`Self::sample`] fills from the photo,
    /// when some fall past its edge (lens corrections, perspective) and get [`BLANK`]. `None` when every
    /// pixel shows the photo.
    pub fn coverage(&self, src_w: usize, src_h: usize, w: usize, h: usize) -> Option<Coverage> {
        let (SampleMode::Warp(o2t), Some(warp)) = (self.sample_plan(src_w, src_h, w, h).mode, self.warp.as_ref()) else {
            return None;
        };
        Some(warp.coverage(&o2t, w, h)).filter(|c| !c.is_full())
    }

    /// Sample the source into a `w × h` output buffer: orientation, crop, rotation, flips — one
    /// bilinear resample from a pre-filtered (area-downscaled) copy, so minification never aliases.
    pub fn sample(&self, src: &Rgb32f, w: usize, h: usize) -> Rgb32f {
        let oriented = if self.orient == Orientation::Normal { None } else { Some(src.oriented(self.orient)) };
        let o = oriented.as_ref().unwrap_or(src);
        let plan = self.sample_plan(src.width, src.height, w, h);
        let base = match plan.prefilter {
            Some((nw, nh)) => std::borrow::Cow::Owned(resize(o, nw, nh, self.downscale)),
            None => std::borrow::Cow::Borrowed(o),
        };
        self.sample_base(base, plan, w, h)
    }

    /// [`Frame::sample`] of each of `windows` of this frame at its `full_w × full_h` output size: the
    /// same pixels, with the source oriented and prefiltered once for all of them rather than per window.
    pub fn sample_windows(&self, src: &Rgb32f, full_w: usize, full_h: usize, windows: &[crate::PixelWindow]) -> Vec<Rgb32f> {
        let oriented = if self.orient == Orientation::Normal { None } else { Some(src.oriented(self.orient)) };
        let o = oriented.as_ref().unwrap_or(src);
        let plans: Vec<(Frame, SamplePlan)> = windows
            .iter()
            .map(|t| {
                let frame = if (t.w, t.h) == (full_w, full_h) { self.clone() } else { self.window(full_w, full_h, *t) };
                let plan = frame.sample_plan(src.width, src.height, t.w, t.h);
                (frame, plan)
            })
            .collect();
        let mut bases: Vec<((usize, usize), Rgb32f)> = Vec::new();
        for (frame, plan) in &plans {
            if let Some(dims) = plan.prefilter
                && !bases.iter().any(|b| b.0 == dims)
            {
                bases.push((dims, resize(o, dims.0, dims.1, frame.downscale)));
            }
        }
        plans
            .into_iter()
            .zip(windows)
            .map(|((frame, plan), t)| {
                let base = match plan.prefilter.and_then(|dims| bases.iter().find(|b| b.0 == dims)) {
                    Some(b) => std::borrow::Cow::Borrowed(&b.1),
                    None => std::borrow::Cow::Borrowed(o),
                };
                frame.sample_base(base, plan, t.w, t.h)
            })
            .collect()
    }

    /// The sampling of [`Frame::sample`] from the oriented, prefiltered source `base`.
    fn sample_base(&self, base: std::borrow::Cow<'_, Rgb32f>, plan: SamplePlan, w: usize, h: usize) -> Rgb32f {
        let affine = |xf: Affine| {
            let mut out = Rgb32f::new(w, h);
            par_rows(&mut out.data, w, |y, row| {
                for (x, px) in row.iter_mut().enumerate() {
                    let p = xf.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                    *px = base.sample_bilinear(p.x as f32, p.y as f32);
                }
            });
            out
        };
        match (plan.mode, self.warp.as_ref()) {
            (SampleMode::Copy, _) => base.into_owned(),
            (SampleMode::Warp(o2t), Some(warp)) => sample_warped(&base, plan.sx, plan.sy, warp, o2t, w, h),
            // `sample_plan` plans a warp only when there is one; without it the same mapping is affine
            (SampleMode::Warp(o2t), None) => affine(Affine::scale(plan.sx, plan.sy) * o2t),
            (SampleMode::Affine(xf), _) => affine(xf),
        }
    }

    /// [`Self::sample`] for an alpha plane of the same source (`src` the size of its pixels): the
    /// same resample through the same mapping, without the lens's vignette gain or per-channel
    /// shifts (alpha follows green). What the mapping brings in from outside the source is 0.
    pub fn sample_alpha(&self, src: &Plane, w: usize, h: usize) -> Plane {
        let oriented = if self.orient == Orientation::Normal { None } else { Some(src.oriented(self.orient)) };
        let o = oriented.as_ref().unwrap_or(src);
        let plan = self.sample_plan(src.width, src.height, w, h);
        let base = match plan.prefilter {
            Some((nw, nh)) => std::borrow::Cow::Owned(resize(o, nw, nh, Filter::Mitchell)),
            None => std::borrow::Cow::Borrowed(o),
        };
        let mut out = match (plan.mode, self.warp.as_ref()) {
            (SampleMode::Copy, _) => base.into_owned(),
            (SampleMode::Warp(o2t), Some(warp)) => {
                let mut out = Plane::new(w, h);
                par_rows(&mut out.data, w, |y, row| {
                    for (x, px) in row.iter_mut().enumerate() {
                        *px = warp.frame(&o2t, x, y).map_or(0.0, |(_, g)| bilinear_plane(&base, g.x * plan.sx, g.y * plan.sy));
                    }
                });
                out
            }
            (SampleMode::Warp(o2t), None) => sample_alpha_affine(&base, Affine::scale(plan.sx, plan.sy) * o2t, w, h),
            (SampleMode::Affine(xf), _) => sample_alpha_affine(&base, xf, w, h),
        };
        // (the Mitchell prefilter rings a little past 0 and 1)
        for a in &mut out.data {
            *a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        }
        out
    }

    /// The crop rectangle as a quad in normalized oriented coordinates (for overlays).
    pub fn crop_quad_norm(&self) -> [Point; 4] {
        let r = self.crop.rect_px(self.ow, self.oh);
        let back = Affine::rotate_about(-self.crop.angle.to_radians(), Point::new(self.ow / 2.0, self.oh / 2.0));
        r.corners().map(|p| {
            let q = back.apply(p);
            Point::new(q.x / self.ow, q.y / self.oh)
        })
    }
}

/// How a render resamples its source (see [`Frame::sample_plan`]).
#[derive(Clone, Debug)]
pub struct SamplePlan {
    /// Oriented source size.
    pub ow: usize,
    pub oh: usize,
    /// Size of the prefilter ([`Frame::downscale`]: area downscale before the bilinear resample), if any.
    pub prefilter: Option<(usize, usize)>,
    /// Oriented-source px → prefiltered px.
    pub sx: f64,
    pub sy: f64,
    pub mode: SampleMode,
}

#[derive(Clone, Debug)]
pub enum SampleMode {
    /// The (prefiltered) oriented source is the output.
    Copy,
    /// Bilinear through this affine (output px → prefiltered px).
    Affine(Affine),
    /// Through the warp; the affine maps output px → transformed px.
    Warp(Affine),
}

/// The perspective homography of the settings (Upright, then the manual Transform sliders), lens-corrected →
/// transformed, in centred coordinates.
pub fn perspective(s: &DevelopSettings, ow: f64, oh: f64) -> Homography {
    if !s.section_enabled("geometry") {
        return Homography::IDENTITY;
    }
    crate::transform::manual(&s.geometry).mul(&crate::upright::homography(&s.geometry, ow, oh))
}

/// One resample through the warp: `o2t` maps output px → transformed px, the warp maps those to source px
/// (scaled by `sx, sy` into `base`).
fn sample_warped(base: &Rgb32f, sx: f64, sy: f64, wp: &Warp, o2t: Affine, w: usize, h: usize) -> Rgb32f {
    let per_channel = wp.per_channel();
    let gain = wp.has_gain();
    let mut out = Rgb32f::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            let Some((c, g)) = wp.frame(&o2t, x, y) else {
                *px = BLANK;
                continue;
            };
            let mut v = base.sample_bilinear((g.x * sx) as f32, (g.y * sy) as f32);
            if per_channel {
                for ch in [0usize, 2] {
                    let q = wp.corrected_to_source(c, ch);
                    v[ch] = base.sample_bilinear((q.x * sx) as f32, (q.y * sy) as f32)[ch];
                }
            }
            if gain {
                let k = wp.gain(g);
                v = v.map(|c| c * k);
            }
            *px = v;
        }
    });
    out
}

fn sample_alpha_affine(base: &Plane, xf: Affine, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            let p = xf.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            *px = alpha_at(base, p.x, p.y);
        }
    });
    out
}

/// Bilinear alpha at pixel coordinates `(x, y)` of `a`; 0 off the image, which (as for a warp,
/// `Warp::edges`) has a half-pixel margin, where its edge pixels extend.
fn alpha_at(a: &Plane, x: f64, y: f64) -> f32 {
    const MARGIN: f64 = 0.5;
    let (w, h) = (a.width as f64, a.height as f64);
    if !(x >= -MARGIN && y >= -MARGIN && x <= w + MARGIN && y <= h + MARGIN) {
        return 0.0;
    }
    bilinear_plane(a, x, y)
}

/// Bilinear sample of `a` at pixel coordinates `(x, y)`, reading the edge pixel past the edge (as
/// `Rgb32f::sample_bilinear` does); 0 for an empty plane or a coordinate that is not finite.
fn bilinear_plane(a: &Plane, x: f64, y: f64) -> f32 {
    if a.data.is_empty() || !x.is_finite() || !y.is_finite() {
        return 0.0;
    }
    let (w, h) = (a.width as f64, a.height as f64);
    let (fx, fy) = (x.clamp(-1.0, w + 1.0) - 0.5, y.clamp(-1.0, h + 1.0) - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let at = |xi: f64, yi: f64| a.get_clamped(xi as isize, yi as isize);
    let top = at(x0, y0) + (at(x0 + 1.0, y0) - at(x0, y0)) * tx;
    let bot = at(x0, y0 + 1.0) + (at(x0 + 1.0, y0 + 1.0) - at(x0, y0 + 1.0)) * tx;
    top + (bot - top) * ty
}

/// Whole-image rectangle in normalized coordinates.
pub const FULL: Rect = Rect::UNIT;

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #2555: a turned crop rect of 0..1000 (from a sidecar or an agent) made the native size
    /// of a 6000 × 4000 photo ~2.4·10¹³ pixels, which a full-size export allocated.
    #[test]
    fn a_crop_far_past_the_photo_keeps_the_native_size_near_the_source() {
        let mut s = DevelopSettings::default();
        s.crop.geometry = lcc_geom::CropGeometry { rect: lcc_geom::Rect::new(0.0, 0.0, 1000.0, 1000.0), angle: 5.0 };
        let (w, h) = crate::native_output_size(6000, 4000, &s);
        assert!(w <= 12_000.0 && h <= 8_000.0, "{w} x {h}");
        // an untouched photo is its own size
        assert_eq!(crate::native_output_size(6000, 4000, &DevelopSettings::default()), (6000.0, 4000.0));
    }

    #[test]
    fn default_frame_is_identity() {
        let s = DevelopSettings::default();
        let f = Frame::new(300, 200, &s, true);
        assert_eq!(f.fit(3000, 3000), (3000, 2000));
        let m = f.out_to_norm(300, 200);
        let p = m.apply(Point::new(150.0, 100.0));
        assert!((p.x - 0.5).abs() < 1e-9 && (p.y - 0.5).abs() < 1e-9);
        assert!((f.px_per_long(300) - 300.0).abs() < 1e-9);
    }

    #[test]
    fn rotation_swaps_aspect() {
        let s = DevelopSettings { orientation: Orientation::Rotate90, ..Default::default() };
        let f = Frame::new(300, 200, &s, true);
        assert_eq!(f.fit(1000, 1000), (667, 1000));
    }

    #[test]
    fn crop_changes_aspect_and_mapping() {
        let mut s = DevelopSettings::default();
        s.crop.geometry.rect = Rect::new(0.5, 0.0, 1.0, 1.0);
        let f = Frame::new(400, 200, &s, true);
        assert_eq!(f.fit(1000, 1000), (1000, 1000));
        let p = f.out_to_norm(100, 100).apply(Point::new(0.0, 0.0));
        assert!((p.x - 0.5).abs() < 1e-9 && p.y.abs() < 1e-9);
        let back = f.norm_to_out(100, 100).apply(Point::new(0.75, 0.5));
        assert!((back.x - 50.0).abs() < 1e-6 && (back.y - 50.0).abs() < 1e-6);
    }

    /// A crop of no size (or under a pixel wide) gave a NaN or infinite aspect; each side now
    /// counts as at least one source pixel.
    #[test]
    fn degenerate_crops_have_a_finite_aspect() {
        let mut s = DevelopSettings::default();
        for (rect, want) in [
            (Rect::new(0.5, 0.5, 0.5, 0.5), 1.0),
            (Rect::new(0.0, 0.0, 1e-9, 1.0), 1.0 / 200.0),
            (Rect::new(0.0, 0.5, 1.0, 0.5), 400.0),
            (Rect::new(0.25, 0.0, 0.75, 1.0), 1.0),
        ] {
            s.crop.geometry.rect = rect;
            let f = Frame::new(400, 200, &s, true);
            assert!((f.aspect() - want).abs() < 1e-9, "{rect:?}: {}", f.aspect());
            let (w, h) = f.fit(1000, 1000);
            assert!((1..=1000).contains(&w) && (1..=1000).contains(&h), "{rect:?}: {w}×{h}");
        }
    }

    #[test]
    fn flip_mirrors() {
        let mut s = DevelopSettings::default();
        s.crop.flip_h = true;
        let src = Rgb32f::from_fn(4, 1, |x, _| [x as f32, 0.0, 0.0]);
        let f = Frame::new(4, 1, &s, true);
        let out = f.sample(&src, 4, 1);
        assert!((out.get(0, 0)[0] - 3.0).abs() < 1e-5);
        assert!((out.get(3, 0)[0] - 0.0).abs() < 1e-5);
    }

    /// The crop tool shows the crop Constrain Crop applies: the box `constrained_crop` gives on the
    /// uncropped frame is the one the render's frame crops to (no white wedges after Upright).
    #[test]
    fn the_crop_tool_shows_the_constrained_crop() {
        let mut s = DevelopSettings::default();
        s.geometry.vertical = 40.0;
        s.geometry.constrain_crop = true;
        let render = Frame::new(600, 400, &s, true);
        let tool = Frame::new(600, 400, &s, false);
        let shown = tool.constrained_crop(s.crop.geometry, true);
        assert_eq!(shown, render.crop);
        assert!(shown.rect.width() < 1.0 - 1e-3, "the warp pulls the box in: {shown:?}");
        // without a warp, nothing changes
        let plain = DevelopSettings::default();
        assert_eq!(Frame::new(600, 400, &plain, false).constrained_crop(plain.crop.geometry, true), plain.crop.geometry);
    }

    /// The crop tool's uncropped frame keeps the flips: Flip H / Flip V show while cropping, and
    /// screen points map back through the mirror (the crop box is drawn and dragged right).
    #[test]
    fn uncropped_frame_keeps_the_flips() {
        let mut s = DevelopSettings::default();
        s.crop.flip_h = true;
        s.crop.flip_v = true;
        s.crop.geometry.rect = Rect::new(0.1, 0.2, 0.6, 0.9);
        let src = Rgb32f::from_fn(4, 2, |x, y| [x as f32, y as f32, 0.0]);
        let f = Frame::new(4, 2, &s, false);
        let out = f.sample(&src, 4, 2);
        assert!((out.get(0, 0)[0] - 3.0).abs() < 1e-5 && (out.get(0, 0)[1] - 1.0).abs() < 1e-5, "{:?}", out.get(0, 0));
        assert!((out.get(3, 1)[0] - 0.0).abs() < 1e-5 && (out.get(3, 1)[1] - 0.0).abs() < 1e-5, "{:?}", out.get(3, 1));
        // the whole image, mirrored: output top-left is the image's bottom-right
        let p = f.out_to_norm(400, 200).apply(Point::new(0.0, 0.0));
        assert!((p.x - 1.0).abs() < 1e-9 && (p.y - 1.0).abs() < 1e-9, "{p:?}");
    }

    /// A request box far beyond the photo comes back at the render pixel ceiling instead of the
    /// 2e10 px the caller asked for (which the render then allocated, aborting the process;
    /// issue #3320): 200000×100000 lands at exactly 512 megapixels.
    #[test]
    fn an_oversized_box_is_clamped_to_the_pixel_ceiling() {
        let s = DevelopSettings::default();
        let f = Frame::new(8, 4, &s, true); // 2:1, 32 px of its own
        let (w, h) = f.fit(200_000, 100_000);
        assert!((w as i64 - 32_000).abs() <= 1 && (h as i64 - 16_000).abs() <= 1, "{w}×{h}");
        assert!((w * h) as f64 <= MAX_RENDER_PIXELS * 1.001, "{} px", w * h);
        assert!(((w as f64) / (h as f64) - 2.0).abs() < 0.01, "the frame's 2:1 aspect: {w}×{h}");
    }

    /// A `usize::MAX` box used to overflow the byte count in usize before the allocation could
    /// even fail (debug builds panicked at the multiply). The sizing is all f64 now and the
    /// ceiling wins.
    #[test]
    fn a_usize_max_box_clamps_without_overflow() {
        let s = DevelopSettings::default();
        let f = Frame::new(8, 4, &s, true);
        let (w, h) = f.fit(usize::MAX, usize::MAX);
        assert!((1..=100_000).contains(&w) && (1..=100_000).contains(&h), "{w}×{h}");
        assert!((w * h) as f64 <= MAX_RENDER_PIXELS * 1.001, "{} px", w * h);
        assert!(((w as f64) / (h as f64) - 2.0).abs() < 0.01, "the frame's 2:1 aspect: {w}×{h}");
    }

    /// Every size the ceiling allows is returned exactly as before: a large-but-legal enlargement
    /// (2e8 px < 5.12e8) passes through untouched.
    #[test]
    fn a_legal_request_is_returned_unchanged() {
        let s = DevelopSettings::default();
        let f = Frame::new(8, 4, &s, true);
        assert_eq!(f.fit(20_000, 10_000), (20_000, 10_000));
        assert_eq!(f.fit(1000, 500), (1000, 500));
        assert_eq!(f.fit(3000, 3000), (3000, 1500));
        assert_eq!(f.fit(1, 1), (1, 1));
    }

    /// A photo with more than 512 megapixels of its own is never held below its own size (a Frame
    /// is pure math: building one allocates no pixels), and an enlargement past its own size stops
    /// at its own size, not the ceiling.
    #[test]
    fn a_frame_larger_than_the_ceiling_is_not_clamped_below_its_own_size() {
        let s = DevelopSettings::default();
        let f = Frame::new(400_000_000, 200_000_000, &s, true); // 8e16 px of its own
        assert_eq!(f.fit(400_000_000, 200_000_000), (400_000_000, 200_000_000));
        assert_eq!(f.fit(800_000_000, 400_000_000), (400_000_000, 200_000_000));
    }

    /// The crop rectangle comes from settings (a sidecar, a preset, `develop.merge`,
    /// `render --settings`), and a turned crop is deliberately left unclipped to 0..1
    /// (`DevelopSettings::keep_in_range`), so a hostile or typo'd one can name more pixels than the
    /// photo has. It must not lift the ceiling with them: the exemption stops at the photo's area,
    /// so the fit below lands at 512 megapixels, not at the ~2 gigapixels the crop names.
    #[test]
    fn an_out_of_bounds_crop_cannot_lift_the_ceiling_past_the_photo() {
        let mut s = DevelopSettings::default();
        s.crop.geometry.rect = Rect::new(-4_000.0, -4_000.0, 4_001.0, 4_001.0); // ~2e9 px "of its own"
        s.crop.geometry.angle = 5.0;
        let f = Frame::new(8, 4, &s, true);
        let (w, h) = f.fit(200_000, 100_000);
        // `Frame::new` bounds the turned crop to the photo first, so its aspect is the bounded
        // crop's, not the 2:1 of the photo; the fit keeps that aspect and lands on the ceiling
        let got = w as f64 / h as f64;
        assert!((got - f.aspect()).abs() / f.aspect() < 1e-3, "{w}×{h} vs aspect {}", f.aspect());
        assert!((w * h) as f64 >= MAX_RENDER_PIXELS * 0.99, "{} px: held below the ceiling", w * h);
        assert!((w * h) as f64 <= MAX_RENDER_PIXELS * 1.001, "{} px", w * h);
    }

    /// A straightened crop may be wider than the photo itself while its corners still lie inside
    /// it: 54000×13000 at 45° fits a 48000×48000 photo (702 megapixels of its own). The exemption
    /// is the photo's *area*, so this native render is returned as asked, not shrunk by a clamped
    /// side.
    #[test]
    fn a_straightened_crop_wider_than_the_photo_keeps_its_native_size() {
        let mut s = DevelopSettings::default();
        let (rw, rh) = (1.125, 13_000.0 / 48_000.0);
        s.crop.geometry.rect = Rect::new(0.5 - rw / 2.0, 0.5 - rh / 2.0, 0.5 + rw / 2.0, 0.5 + rh / 2.0);
        s.crop.geometry.angle = 45.0;
        let f = Frame::new(48_000, 48_000, &s, true);
        assert_eq!(f.fit(54_000, 13_000), (54_000, 13_000));
    }
}

/// Mask coordinates `n` (on the photo before `warp`, normalized to `ow × oh`) → normalized
/// transformed coordinates. A point the warp can't invert (far outside a strong one) stays put.
fn mask_to_norm(warp: Option<&Warp>, ow: f64, oh: f64, n: Point) -> Point {
    match warp.and_then(|w| w.from_source(Point::new(n.x * ow, n.y * oh))) {
        Some(t) => Point::new(t.x / ow, t.y / oh),
        None => n,
    }
}

/// Output pixel → mask coordinates of a frame ([`Frame::out_to_mask`]).
#[derive(Clone, Debug)]
pub struct MaskMap {
    o2t: Affine,
    to_norm: Affine,
    ow: f64,
    oh: f64,
    warp: Option<Warp>,
}

impl MaskMap {
    /// The mask coordinates at output position `p` (px; pixel centres at `x + 0.5`).
    #[inline]
    pub fn apply(&self, p: Point) -> Point {
        match &self.warp {
            Some(w) => {
                let s = w.to_source(self.o2t.apply(p), 1);
                Point::new(s.x / self.ow, s.y / self.oh)
            }
            None => self.to_norm.apply(p),
        }
    }
}
