//! Diagnostic views drawn over a finished render ([`Overlay`]), shared by the CPU and GPU renderers
//! (they run on the 8-bit result, after the histogram is taken).
//!
//! - **Point Color range:** the photo is rendered without that sample's own adjustment; pixels
//!   inside the sample's range keep their colour, the rest turn grey (weighted by the range).
//! - **Visualize Spots** (Remove tool): a high-pass of the luminance thresholded to black/white,
//!   so dust spots and specks stand out as white dots. The scale is relative to the image (the
//!   same spots show at any preview size); the threshold slider (0..100) raises sensitivity.
//! - **Mask overlay** (Masking): the evaluated alpha of one mask, drawn as a colour tint, a colour
//!   tint on a black-and-white image, the image on black / white, or the alpha as white on black
//!   ([`MaskView`]). Both renderers hand the same alpha plane (the one the render used) to [`apply`].

use std::borrow::Cow;

use lightcraft_color::perceptual::{lab_to_lch, oklab_from_2020};
use lightcraft_color::transfer::srgb_to_linear;
use lightcraft_color::{REC2020, SRGB};
use lightcraft_develop::DevelopSettings;
use lightcraft_geom::{Orientation, Point};
use lightcraft_raster::{Plane, Rgba8};

use crate::colorops::PointK;
use crate::geometry::{self, SampleMode};
use crate::{Plan, SensorClip, for_rows};

/// A diagnostic view of a render.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Overlay {
    #[default]
    None,
    /// Point Color sample `i`: its range in colour, everything else grey.
    PointColorRange(u8),
    /// Visualize Spots with a threshold 0..100 (higher = more sensitive).
    Spots(u8),
    /// The evaluated alpha of mask `id` (a [`lightcraft_develop::Mask`] id), drawn as `view` in
    /// `color` at `opacity` (0..100; the colour views only).
    Mask { id: u16, view: MaskView, color: [u8; 3], opacity: u8 },
    /// The evaluated alpha of every visible mask that has components, drawn as one `view` in
    /// `color` at `opacity` (0..100; the colour views only) — every mask's region at once.
    AllMasks { view: MaskView, color: [u8; 3], opacity: u8 },
    /// Visualize HDR (photos edited in HDR): the SDR image dimmed to grey, with every pixel the HDR
    /// rendition puts above SDR white painted by how far above it is ([`HDR_BANDS`]).
    HdrRange,
    /// Show Clipping (J): display-clipped pixels painted over the photo — any channel at 255 red,
    /// any at 0 blue ([`CLIP_COLORS`]; highlights win where a pixel clips both ways).
    Clipping,
    /// Show Sensor Clipping (Shift+J, raws only): photosites at the sensor's clip level, per
    /// colour channel, before any adjustment ([`SENSOR_CLIP_COLORS`]) — what can still be
    /// recovered and what is gone, however the edits move the display values.
    SensorClipping,
}

/// [`Overlay::HdrRange`] colours (sRGB) for 0–1, 1–2, 2–3 and ≥ 3 stops above SDR white.
pub const HDR_BANDS: [[u8; 3]; 4] = [[255, 214, 92], [255, 150, 40], [235, 64, 52], [214, 60, 210]];

/// [`Overlay::Clipping`] colours (sRGB): highlight clip, shadow clip (the histogram warning's).
pub const CLIP_COLORS: [[u8; 3]; 2] = [[255, 60, 60], [60, 120, 255]];

/// [`Overlay::SensorClipping`] colours (sRGB), per clipped colour channel (the CFA colours, like
/// darktable's raw overexposed warning); white when several channels of a pixel clipped (linear-raw
/// photosites carry all three).
pub const SENSOR_CLIP_COLORS: [[u8; 3]; 4] = [[255, 60, 60], [60, 220, 60], [60, 120, 255], [255, 255, 255]];

/// How [`Overlay::Mask`] draws the mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaskView {
    /// The photo with the mask tinted in the overlay colour.
    #[default]
    Color,
    /// A black-and-white photo with the mask tinted in the overlay colour.
    ColorOnBw,
    /// The photo where the mask is, black elsewhere.
    ImageOnBlack,
    /// The photo where the mask is, white elsewhere.
    ImageOnWhite,
    /// The mask itself: white on black.
    WhiteOnBlack,
}

impl MaskView {
    pub const ALL: [MaskView; 5] = [MaskView::Color, MaskView::ColorOnBw, MaskView::ImageOnBlack, MaskView::ImageOnWhite, MaskView::WhiteOnBlack];

    /// Stable name (control channel, settings).
    pub fn name(self) -> &'static str {
        match self {
            MaskView::Color => "color",
            MaskView::ColorOnBw => "colorOnBw",
            MaskView::ImageOnBlack => "imageOnBlack",
            MaskView::ImageOnWhite => "imageOnWhite",
            MaskView::WhiteOnBlack => "whiteOnBlack",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MaskView::Color => "Color Overlay",
            MaskView::ColorOnBw => "Color Overlay on B&W",
            MaskView::ImageOnBlack => "Image on Black",
            MaskView::ImageOnWhite => "Image on White",
            MaskView::WhiteOnBlack => "White on Black",
        }
    }

    pub fn parse(s: &str) -> Option<MaskView> {
        MaskView::ALL.into_iter().find(|v| v.name() == s)
    }

    /// The next view (Shift+O cycles them).
    pub fn next(self) -> MaskView {
        let i = MaskView::ALL.iter().position(|v| *v == self).unwrap_or(0);
        MaskView::ALL[(i + 1) % MaskView::ALL.len()]
    }

    fn index(self) -> u64 {
        MaskView::ALL.iter().position(|v| *v == self).unwrap_or(0) as u64
    }
}

/// [`Overlay::Mask`]'s fields in 51 bits (exact in an `f64`): id 16, view 4, colour 24, opacity 7.
fn pack_mask(id: u16, view: MaskView, color: [u8; 3], opacity: u8) -> u64 {
    let rgb = (color[0] as u64) << 16 | (color[1] as u64) << 8 | color[2] as u64;
    id as u64 | view.index() << 16 | rgb << 20 | (opacity.min(100) as u64) << 44
}

fn unpack_mask(v: u64) -> Overlay {
    let rgb = (v >> 20) & 0xff_ffff;
    Overlay::Mask {
        id: (v & 0xffff) as u16,
        view: MaskView::ALL.get(((v >> 16) & 0xf) as usize).copied().unwrap_or_default(),
        color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
        opacity: ((v >> 44) & 0x7f).min(100) as u8,
    }
}

impl Overlay {
    /// A plain (kind, value) pair, e.g. for sending a request to a web worker.
    pub fn to_parts(self) -> (u8, f64) {
        match self {
            Overlay::None => (0, 0.0),
            Overlay::PointColorRange(i) => (1, i as f64),
            Overlay::Spots(t) => (2, t as f64),
            Overlay::Mask { id, view, color, opacity } => (3, pack_mask(id, view, color, opacity) as f64),
            Overlay::AllMasks { view, color, opacity } => (5, pack_mask(0, view, color, opacity) as f64),
            Overlay::HdrRange => (4, 0.0),
            Overlay::Clipping => (6, 0.0),
            Overlay::SensorClipping => (7, 0.0),
        }
    }

    /// Inverse of [`Overlay::to_parts`] (unknown kinds: no overlay).
    pub fn from_parts(kind: u8, v: f64) -> Overlay {
        match kind {
            1 => Overlay::PointColorRange(v as u8),
            2 => Overlay::Spots(v.clamp(0.0, 100.0) as u8),
            3 if v.is_finite() && v >= 0.0 => unpack_mask(v as u64),
            4 => Overlay::HdrRange,
            6 => Overlay::Clipping,
            7 => Overlay::SensorClipping,
            5 if v.is_finite() && v >= 0.0 => {
                let rgb = (v as u64 >> 20) & 0xff_ffff;
                Overlay::AllMasks {
                    view: MaskView::ALL.get(((v as u64 >> 16) & 0xf) as usize).copied().unwrap_or_default(),
                    color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
                    opacity: ((v as u64 >> 44) & 0x7f).min(100) as u8,
                }
            }
            _ => Overlay::None,
        }
    }

    /// A stable value for render-cache keys (0 = no overlay).
    pub fn key(self) -> u64 {
        match self {
            Overlay::None => 0,
            Overlay::PointColorRange(i) => 0x1000 + i as u64,
            Overlay::Spots(t) => 0x2000 + t as u64,
            Overlay::Mask { id, view, color, opacity } => 3 << 60 | pack_mask(id, view, color, opacity),
            Overlay::AllMasks { view, color, opacity } => 5 << 60 | pack_mask(0, view, color, opacity),
            Overlay::HdrRange => 0x4000,
            Overlay::Clipping => 0x5000,
            Overlay::SensorClipping => 0x6000,
        }
    }

    /// The mask an [`Overlay::Mask`] shows (if it exists in `s` and has components).
    pub fn mask(self, s: &DevelopSettings) -> Option<&lightcraft_develop::Mask> {
        match self {
            Overlay::Mask { id, .. } => s.masks.iter().find(|m| m.id == id as u32 && !m.components.is_empty()),
            _ => None,
        }
    }
}

/// Settings changes an overlay needs before rendering (e.g. the visualized sample's adjustment
/// is left out, so the range is shown on the colours it selects).
pub fn adjust_settings(o: Overlay, s: &mut Cow<'_, DevelopSettings>) {
    if let Overlay::PointColorRange(i) = o
        && let Some(p) = s.point_colors.get(i as usize)
        && !p.is_neutral()
    {
        let p = &mut s.to_mut().point_colors[i as usize];
        (p.hue_shift, p.sat_shift, p.lum_shift, p.variance) = (0.0, 0.0, 0.0, 0.0);
    }
}

/// Draw overlay `o` over `img` (rendered with `plan`). `mask` is the evaluated alpha of the mask
/// an [`Overlay::Mask`] shows, at the image's size (see [`Overlay::mask`]); `masks` is the set of
/// alphas an [`Overlay::AllMasks`] shows, one per visible mask with components.
pub fn apply(img: &mut Rgba8, o: Overlay, plan: &Plan<'_>, mask: Option<&Plane>, masks: Option<&[Plane]>) {
    match o {
        // drawn by the renderer from the HDR rendition ([`hdr_range`])
        Overlay::None | Overlay::HdrRange => {}
        Overlay::PointColorRange(i) => {
            if let Some(p) = plan.settings.point_colors.get(i as usize) {
                point_color_range(img, &PointK::new(p));
            }
        }
        Overlay::Spots(t) => spots(img, t, plan.px_per_long),
        Overlay::Mask { view, color, opacity, .. } => {
            if let Some(a) = mask.filter(|a| (a.width, a.height) == (img.width, img.height)) {
                mask_view(img, a, view, color, opacity);
            }
        }
        Overlay::AllMasks { view, color, opacity } => {
            // one view of the union of the masks: painting the planes one after another
            // overwrites (White on Black, Image on Black…) or compounds (Color) the earlier ones
            let mut union: Option<Plane> = None;
            for a in masks.unwrap_or_default().iter().filter(|a| (a.width, a.height) == (img.width, img.height)) {
                match &mut union {
                    None => union = Some(a.clone()),
                    Some(u) => u.data.iter_mut().zip(&a.data).for_each(|(u, a)| *u = u.max(*a)),
                }
            }
            if let Some(u) = &union {
                mask_view(img, u, view, color, opacity);
            }
        }
        Overlay::Clipping => clipping(img),
        Overlay::SensorClipping => sensor_clipping(img, plan),
    }
}

/// Paint display-clipped pixels ([`Overlay::Clipping`]): any channel at 255 red, any at 0 blue
/// (highlights win where a pixel clips both ways), the rest untouched.
fn clipping(img: &mut Rgba8) {
    let [hi, lo] = CLIP_COLORS;
    for px in &mut img.data {
        let [r, g, b, _] = *px;
        if r == 255 || g == 255 || b == 255 {
            *px = [hi[0], hi[1], hi[2], 255];
        } else if r == 0 || g == 0 || b == 0 {
            *px = [lo[0], lo[1], lo[2], 255];
        }
    }
}

/// Paint sensor-clipped photosites ([`Overlay::SensorClipping`]): the raw's photosites at the
/// clip level, per colour channel, before any adjustment — sampled through the same frame
/// transform the picture went through, so the marks sit on the pixels that show them.
fn sensor_clipping(img: &mut Rgba8, plan: &Plan<'_>) {
    let Some(clip) = plan.sensor_clip.as_deref() else { return };
    let flags = sample_flags(clip, &plan.frame, img.width, img.height);
    for (px, f) in img.data.iter_mut().zip(&flags) {
        if *f != 0 {
            let c = sensor_clip_color(*f);
            *px = [c[0], c[1], c[2], 255];
        }
    }
}

/// The colour of a pixel with these clip flags (see [`SENSOR_CLIP_COLORS`]).
fn sensor_clip_color(f: u8) -> [u8; 3] {
    match f {
        0b001 => SENSOR_CLIP_COLORS[0],
        0b010 => SENSOR_CLIP_COLORS[1],
        0b100 => SENSOR_CLIP_COLORS[2],
        _ => SENSOR_CLIP_COLORS[3],
    }
}

/// Sample the sensor clip flags through the same transform [`geometry::Frame::sample`] resamples
/// the picture with — user orientation, Mitchell prefilter (OR instead of average: a flag is a
/// fact about a photosite, not a value to blend), then affine or warp, nearest.
fn sample_flags(clip: &SensorClip, frame: &geometry::Frame, w: usize, h: usize) -> Vec<u8> {
    let owned;
    let oriented: &SensorClip = if frame.orient == Orientation::Normal {
        clip
    } else {
        owned = clip.clone().into_oriented(frame.orient);
        &owned
    };
    let plan = frame.sample_plan(oriented.width, oriented.height, w, h);
    let shrunk;
    let base: &SensorClip = match plan.prefilter {
        Some((nw, nh)) => {
            shrunk = oriented.fit_or(nw, nh);
            &shrunk
        }
        None => oriented,
    };
    let mut out = vec![0u8; w * h];
    match &plan.mode {
        SampleMode::Copy => {
            if base.width == w && base.height == h {
                out.copy_from_slice(&base.data);
            }
        }
        SampleMode::Affine(xf) => {
            for (y, row) in out.chunks_mut(w).enumerate() {
                for (x, f) in row.iter_mut().enumerate() {
                    let p = xf.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                    *f = nearest_flag(base, p.x, p.y);
                }
            }
        }
        // `sample_plan` plans a warp only when there is one; without it the same mapping is affine
        SampleMode::Warp(o2t) => {
            if let Some(wp) = frame.warp.as_ref() {
                for (y, row) in out.chunks_mut(w).enumerate() {
                    for (x, f) in row.iter_mut().enumerate() {
                        let Some((_, g)) = wp.frame(o2t, x, y) else { continue };
                        *f = nearest_flag(base, g.x * plan.sx, g.y * plan.sy);
                    }
                }
            }
        }
    }
    out
}

/// The flags of the pixel a continuous prefiltered-space coordinate falls in (0 outside).
fn nearest_flag(base: &SensorClip, x: f64, y: f64) -> u8 {
    if !x.is_finite() || !y.is_finite() {
        return 0;
    }
    let (bx, by) = (x.floor() as isize, y.floor() as isize);
    if bx < 0 || by < 0 {
        return 0;
    }
    base.at(bx as usize, by as usize)
}

/// Draw mask alpha `a` over `img` as `view`.
pub fn mask_view(img: &mut Rgba8, a: &Plane, view: MaskView, color: [u8; 3], opacity: u8) {
    let op = opacity.min(100) as f32 / 100.0;
    let col = color.map(|c| c as f32);
    let w = img.width;
    for_rows(&mut img.data, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = a.data[y * w + x].clamp(0.0, 1.0);
            let p = [px[0] as f32, px[1] as f32, px[2] as f32];
            let grey = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            let out: [f32; 3] = match view {
                MaskView::Color => std::array::from_fn(|c| p[c] + (col[c] - p[c]) * a * op),
                MaskView::ColorOnBw => std::array::from_fn(|c| grey + (col[c] - grey) * a * op),
                MaskView::ImageOnBlack => p.map(|v| v * a),
                MaskView::ImageOnWhite => p.map(|v| v * a + 255.0 * (1.0 - a)),
                MaskView::WhiteOnBlack => [255.0 * a; 3],
            };
            let o = out.map(|v| v.round().clamp(0.0, 255.0) as u8);
            *px = [o[0], o[1], o[2], 255];
        }
    });
}

/// Blur radius (Gaussian sigma) of the spot view, as a fraction of the long edge.
const SPOT_SIGMA: f64 = 0.006;

/// High-pass threshold (encoded luminance) for slider value `t` (0..100): 100 shows the faintest
/// specks, 0 only strong ones.
pub fn spot_threshold(t: u8) -> f32 {
    let k = 1.0 - t.min(100) as f32 / 100.0;
    0.01 + 0.2 * k * k
}

fn spots(img: &mut Rgba8, t: u8, ppl: f64) {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return;
    }
    let l = lightcraft_raster::Plane::from_fn(w, h, |x, y| {
        let p = img.data[y * w + x];
        (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
    });
    let b = lightcraft_raster::blur::gaussian(&l, (SPOT_SIGMA * ppl).max(0.8) as f32);
    let thr = spot_threshold(t);
    for (i, px) in img.data.iter_mut().enumerate() {
        let v = if (l.data[i] - b.data[i]).abs() > thr { 255 } else { 0 };
        *px = [v, v, v, 255];
    }
}

fn point_color_range(img: &mut Rgba8, k: &PointK) {
    let m = SRGB.to_space(&REC2020).to_f32();
    let lut: Vec<f32> = (0..256).map(|v| srgb_to_linear(v as f32 / 255.0)).collect();
    let w = img.width;
    for_rows(&mut img.data, w, |_, row| {
        for px in row.iter_mut() {
            let lin = [lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize]];
            let c = std::array::from_fn(|r| m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2]);
            let [l, ch, h] = lab_to_lch(oklab_from_2020(c));
            let a = k.weight(l, ch, h);
            let grey = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
            for c in 0..3 {
                px[c] = (grey + (px[c] as f32 - grey) * a).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
}

/// Draw [`Overlay::HdrRange`] on `img` (the SDR render) from `hdr` (the HDR render of the same
/// request, float linear with luminance weights `luma`): grey below SDR white, band colours above.
pub fn hdr_range(img: &mut Rgba8, hdr: &crate::DeepImage, luma: [f32; 3]) {
    let crate::DeepSamples::F32(v) = &hdr.samples else { return };
    if (hdr.width, hdr.height) != (img.width, img.height) {
        return;
    }
    let w = img.width;
    for_rows(&mut img.data, w, |y, row| {
        for (x, p) in row.iter_mut().enumerate() {
            let i = (y * w + x) * 3;
            let Some(c) = v.get(i..i + 3) else { continue };
            let lum = luma[0] * c[0] + luma[1] * c[1] + luma[2] * c[2];
            let grey = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) * 0.55;
            if lum > 1.0 {
                let stops = lum.log2();
                let band = HDR_BANDS[(stops as usize).min(HDR_BANDS.len() - 1)];
                for k in 0..3 {
                    p[k] = (grey * 0.25 + band[k] as f32 * 0.75).round().clamp(0.0, 255.0) as u8;
                }
            } else {
                let g = grey.round().clamp(0.0, 255.0) as u8;
                (p[0], p[1], p[2]) = (g, g, g);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RenderRequest, SourceInfo, plan, render};
    use lightcraft_develop::PointColor;
    use lightcraft_raster::Rgb32f;

    #[test]
    fn spots_view_marks_specks_at_any_size() {
        // a smooth gradient with two small dark specks
        let src = Rgb32f::from_fn(400, 300, |x, y| {
            let d = |cx: f32, cy: f32| ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let base = 0.1 + 0.3 * x as f32 / 400.0;
            if d(100.0, 100.0) < 2.5 || d(300.0, 200.0) < 2.5 { [base * 0.5; 3] } else { [base; 3] }
        });
        let info = SourceInfo::default();
        let s = DevelopSettings::default();
        for size in [400, 200] {
            let req = RenderRequest { overlay: Overlay::Spots(50), ..RenderRequest::fit(size, size) };
            let v = render(&src, &info, &s, &req).image;
            let at = |x: f64, y: f64| v.data[(y * v.height as f64) as usize * v.width + (x * v.width as f64) as usize];
            assert_eq!(at(0.25, 1.0 / 3.0), [255, 255, 255, 255], "speck at size {size}");
            assert_eq!(at(0.75, 2.0 / 3.0), [255, 255, 255, 255]);
            assert_eq!(at(0.5, 0.5), [0, 0, 0, 255], "smooth gradient is black");
            let white = v.data.iter().filter(|p| p[0] == 255).count() as f64 / v.data.len() as f64;
            assert!(white < 0.01, "{white}");
        }
        assert!(spot_threshold(0) > spot_threshold(50) && spot_threshold(50) > spot_threshold(100));
        let (k, v) = Overlay::Spots(37).to_parts();
        assert_eq!(Overlay::from_parts(k, v), Overlay::Spots(37));
    }

    #[test]
    fn point_color_range_keeps_the_selected_colour_only() {
        // left half orange, right half blue
        let src = Rgb32f::from_fn(32, 16, |x, _| if x < 16 { [0.4, 0.15, 0.04] } else { [0.03, 0.06, 0.35] });
        let info = SourceInfo::default();
        let plain = render(&src, &info, &DevelopSettings::default(), &RenderRequest::fit(32, 16)).image;
        // sample the orange as it renders
        let px = plain.data[8 * 32 + 4];
        let lin = [px[0], px[1], px[2]].map(|v| srgb_to_linear(v as f32 / 255.0));
        let c = SRGB.to_space(&REC2020).apply_f32(lin);
        let [l, ch, h] = lab_to_lch(oklab_from_2020(c));
        let mut s = DevelopSettings::default();
        s.point_colors.push(PointColor { lum: l as f64, chroma: ch as f64, hue: (h as f64).to_degrees(), hue_shift: 80.0, ..Default::default() });
        let req = RenderRequest { overlay: Overlay::PointColorRange(0), ..RenderRequest::fit(32, 16) };
        let v = render(&src, &info, &s, &req).image;
        let (o, b) = (v.data[8 * 32 + 4], v.data[8 * 32 + 28]);
        // the orange stays orange (and unshifted: the overlay shows the selection, not the edit)
        assert!(o[0].abs_diff(px[0]) <= 2 && o[2].abs_diff(px[2]) <= 2, "{o:?} vs {px:?}");
        // the blue turns grey
        assert!(b[0].abs_diff(b[2]) <= 1, "{b:?}");
        // without the overlay the edit shows
        let e = render(&src, &info, &s, &RenderRequest::fit(32, 16)).image.data[8 * 32 + 4];
        assert!(e[1].abs_diff(px[1]) > 10, "{e:?} vs {px:?}");
    }

    #[test]
    fn mask_overlay_shows_the_evaluated_alpha() {
        use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
        use lightcraft_geom::Point;
        let src = Rgb32f::from_fn(80, 40, |x, _| if x < 40 { [0.18, 0.18, 0.18] } else { [0.05, 0.1, 0.3] });
        let info = SourceInfo::default();
        let shape = MaskShape::Radial { center: Point::new(0.25, 0.5), rx: 0.15, ry: 0.15, angle: 0.0, feather: 10.0, invert: false };
        let mut s = DevelopSettings::default();
        s.masks.push(Mask { id: 7, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], ..Default::default() });
        let plain = render(&src, &info, &s, &RenderRequest::fit(80, 40)).image;
        let shot = |s: &DevelopSettings, view: MaskView| {
            let o = Overlay::Mask { id: 7, view, color: [255, 0, 0], opacity: 100 };
            render(&src, &info, s, &RenderRequest { overlay: o, ..RenderRequest::fit(80, 40) }).image
        };
        let (inside, outside) = (20 * 80 + 20, 20 * 80 + 70);
        let c = shot(&s, MaskView::Color);
        assert_eq!(c.data[inside], [255, 0, 0, 255], "full tint inside the mask");
        assert_eq!(c.data[outside], plain.data[outside], "untouched outside");
        let bw = shot(&s, MaskView::WhiteOnBlack);
        assert_eq!((bw.data[inside], bw.data[outside]), ([255; 4], [0, 0, 0, 255]));
        let ib = shot(&s, MaskView::ImageOnBlack);
        assert_eq!((ib.data[inside], ib.data[outside]), (plain.data[inside], [0, 0, 0, 255]));
        let iw = shot(&s, MaskView::ImageOnWhite);
        assert_eq!(iw.data[outside], [255; 4]);
        let g = shot(&s, MaskView::ColorOnBw).data[outside];
        assert!(g[0] == g[1] && g[1] == g[2], "grey outside: {g:?}");
        // a hidden mask still shows (it's the one being edited)
        s.masks[0].visible = false;
        assert_eq!(shot(&s, MaskView::WhiteOnBlack).data[inside], [255; 4]);
        // an unknown mask draws nothing
        let o = Overlay::Mask { id: 9, view: MaskView::WhiteOnBlack, color: [0; 3], opacity: 50 };
        assert_eq!(render(&src, &info, &s, &RenderRequest { overlay: o, ..RenderRequest::fit(80, 40) }).image, plain);
        // parts round trip (web worker requests) and distinct cache keys
        let o = Overlay::Mask { id: 513, view: MaskView::ImageOnWhite, color: [12, 200, 255], opacity: 65 };
        let (k, v) = o.to_parts();
        assert_eq!(Overlay::from_parts(k, v), o);
        assert_ne!(o.key(), Overlay::Mask { id: 513, view: MaskView::Color, color: [12, 200, 255], opacity: 65 }.key());
        assert_eq!(MaskView::WhiteOnBlack.next(), MaskView::Color);
        assert_eq!(MaskView::parse("colorOnBw"), Some(MaskView::ColorOnBw));
    }

    #[test]
    fn all_masks_overlay_shows_every_visible_mask_at_once() {
        use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
        use lightcraft_geom::Point;
        let src = Rgb32f::from_fn(80, 40, |_, _| [0.1, 0.1, 0.1]);
        let info = SourceInfo::default();
        let left = MaskShape::Radial { center: Point::new(0.25, 0.5), rx: 0.15, ry: 0.15, angle: 0.0, feather: 10.0, invert: false };
        let right = MaskShape::Radial { center: Point::new(0.75, 0.5), rx: 0.15, ry: 0.15, angle: 0.0, feather: 10.0, invert: false };
        let mut s = DevelopSettings::default();
        s.masks.push(Mask {
            id: 1,
            visible: true,
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: left }],
            ..Default::default()
        });
        s.masks.push(Mask {
            id: 2,
            visible: true,
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: right }],
            ..Default::default()
        });
        let o = Overlay::AllMasks { view: MaskView::WhiteOnBlack, color: [255, 0, 0], opacity: 100 };
        let img = render(&src, &info, &s, &RenderRequest { overlay: o, ..RenderRequest::fit(80, 40) }).image;
        let (in_l, in_r, mid) = (20 * 80 + 12, 20 * 80 + 60, 20 * 80 + 40);
        assert_eq!(img.data[in_l], [255; 4], "left mask region painted");
        assert_eq!(img.data[in_r], [255; 4], "right mask region painted");
        assert_eq!(img.data[mid], [0, 0, 0, 255], "between the masks: black");
        // a hidden mask is skipped
        s.masks[1].visible = false;
        let img2 = render(&src, &info, &s, &RenderRequest { overlay: o, ..RenderRequest::fit(80, 40) }).image;
        assert_eq!(img2.data[in_r], [0, 0, 0, 255], "hidden right mask not painted: black like everything outside");
        assert_eq!(img2.data[in_l], [255; 4], "visible left mask still painted");
        // parts round trip and a stable, distinct cache key
        let (k, v) = o.to_parts();
        assert_eq!(Overlay::from_parts(k, v), o);
        assert_ne!(o.key(), Overlay::Mask { id: 1, view: MaskView::WhiteOnBlack, color: [255, 0, 0], opacity: 100 }.key());
    }

    #[test]
    fn clipping_overlay_paints_clipped_pixels() {
        // left half white, right half black: at +6 EV the white clips high, the black clips low
        let src = Rgb32f::from_fn(80, 40, |x, _| if x < 40 { [1.0; 3] } else { [0.0; 3] });
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.light.exposure = 6.0;
        let req = RenderRequest { overlay: Overlay::Clipping, ..RenderRequest::fit(80, 40) };
        let img = render(&src, &info, &s, &req).image;
        let [hi, lo] = CLIP_COLORS;
        assert_eq!(img.data[20 * 80 + 10], [hi[0], hi[1], hi[2], 255], "clipped highlight painted red");
        assert_eq!(img.data[20 * 80 + 70], [lo[0], lo[1], lo[2], 255], "clipped shadow painted blue");
        let reds = img.data.iter().filter(|p| p[0] == hi[0] && p[1] == hi[1] && p[2] == hi[2]).count();
        let blues = img.data.iter().filter(|p| p[0] == lo[0] && p[1] == lo[1] && p[2] == lo[2]).count();
        assert!(reds > 30 * 40, "{reds} red pixels in the white half");
        assert!(blues > 30 * 40, "{blues} blue pixels in the black half");
        // without the overlay the same render is untouched
        let plain = render(&src, &info, &s, &RenderRequest::fit(80, 40)).image;
        assert_eq!(plain.data[20 * 80 + 10], [255, 255, 255, 255]);
        assert_eq!(plain.data[20 * 80 + 70], [0, 0, 0, 255]);
        // the painter alone: any channel at 255 red (highlights win over shadows), any at 0 blue,
        // everything else untouched
        let mut img = Rgba8 {
            width: 4,
            height: 2,
            data: vec![
                [255, 255, 255, 255],
                [0, 255, 0, 255],
                [0, 0, 0, 255],
                [128, 128, 128, 255],
                [255, 0, 0, 255],
                [1, 1, 1, 255],
                [0, 0, 255, 255],
                [255, 128, 0, 255],
            ],
        };
        let p = plan(&src, &info, &s, &req);
        apply(&mut img, Overlay::Clipping, &p, None, None);
        assert_eq!(img.data[0], [hi[0], hi[1], hi[2], 255]);
        assert_eq!(img.data[1], [hi[0], hi[1], hi[2], 255], "green at 255 is a highlight clip");
        assert_eq!(img.data[2], [lo[0], lo[1], lo[2], 255]);
        assert_eq!(img.data[3], [128, 128, 128, 255], "mid grey untouched");
        assert_eq!(img.data[4], [hi[0], hi[1], hi[2], 255], "red at 255 is a highlight clip");
        assert_eq!(img.data[5], [1, 1, 1, 255], "near black is not a clip");
        assert_eq!(img.data[6], [hi[0], hi[1], hi[2], 255], "blue at 255 wins over red at 0");
        assert_eq!(img.data[7], [hi[0], hi[1], hi[2], 255]);
        // parts round trip (web worker requests) and a distinct cache key
        assert_eq!(Overlay::Clipping.to_parts(), (6, 0.0));
        assert_eq!(Overlay::from_parts(6, 0.0), Overlay::Clipping);
        assert_eq!(Overlay::from_parts(6, 7.0), Overlay::Clipping);
        assert_eq!(Overlay::Clipping.key(), 0x5000);
        assert_ne!(Overlay::Clipping.key(), Overlay::HdrRange.key());
    }

    #[test]
    fn sensor_clip_overlay_paints_clipped_photosites_per_colour() {
        // grey photo; the flags say which photosites sat at the sensor's clip level
        let src = Rgb32f::from_fn(80, 40, |_, _| [0.1; 3]);
        let clip = SensorClip {
            width: 80,
            height: 40,
            data: {
                let mut d = vec![0u8; 80 * 40];
                d[20 * 80 + 10] = 0b001; // red clipped
                d[20 * 80 + 70] = 0b010; // green clipped
                d[10 * 80 + 40] = 0b011; // red and green: white
                d
            },
        };
        let info = SourceInfo { sensor_clip: Some(std::sync::Arc::new(clip)), ..SourceInfo::default() };
        let s = DevelopSettings::default();
        let plain = render(&src, &info, &s, &RenderRequest::fit(80, 40)).image;
        let req = RenderRequest { overlay: Overlay::SensorClipping, ..RenderRequest::fit(80, 40) };
        let img = render(&src, &info, &s, &req).image;
        let [r, g, _, w] = SENSOR_CLIP_COLORS;
        assert_eq!(img.data[20 * 80 + 10], [r[0], r[1], r[2], 255], "clipped red photosite painted red");
        assert_eq!(img.data[20 * 80 + 70], [g[0], g[1], g[2], 255], "clipped green photosite painted green");
        assert_eq!(img.data[10 * 80 + 40], [w[0], w[1], w[2], 255], "several channels clipped: white");
        assert_eq!(img.data[30 * 80 + 40], plain.data[30 * 80 + 40], "unclipped pixel untouched");
        // a source without sensor clip data (not a raw) paints nothing
        let bare = render(&src, &SourceInfo::default(), &s, &req).image;
        assert_eq!(bare, plain);
        // parts round trip (web worker requests) and a distinct cache key
        assert_eq!(Overlay::SensorClipping.to_parts(), (7, 0.0));
        assert_eq!(Overlay::from_parts(7, 0.0), Overlay::SensorClipping);
        assert_eq!(Overlay::SensorClipping.key(), 0x6000);
        assert_ne!(Overlay::SensorClipping.key(), Overlay::Clipping.key());
    }

    #[test]
    fn sensor_clip_flags_follow_the_picture_orientation_and_fit() {
        let src = Rgb32f::from_fn(80, 40, |_, _| [0.1; 3]);
        let clip = SensorClip {
            width: 80,
            height: 40,
            data: {
                let mut d = vec![0u8; 80 * 40];
                d[20 * 80 + 10] = 0b001;
                d
            },
        };
        let info = SourceInfo { sensor_clip: Some(std::sync::Arc::new(clip)), ..SourceInfo::default() };
        // Rotate Right: the output is 40×80 and the photosite at source (10, 20) shows at (19, 10)
        let mut s = DevelopSettings { orientation: lightcraft_geom::Orientation::Rotate90, ..DevelopSettings::default() };
        let req = RenderRequest { overlay: Overlay::SensorClipping, ..RenderRequest::fit(40, 80) };
        let img = render(&src, &info, &s, &req).image;
        let img_plain = render(&src, &info, &s, &RenderRequest::fit(40, 80)).image;
        assert_eq!((img.width, img.height), (40, 80));
        let [r, ..] = SENSOR_CLIP_COLORS;
        assert_eq!(img.data[10 * 40 + 19], [r[0], r[1], r[2], 255], "the mark turned with the photo");
        assert_eq!(img.data[20 * 40 + 10], img_plain.data[20 * 40 + 10], "the unturned spot is clean");
        // half size: the OR footprint of the downscaled pixel keeps the mark
        s.orientation = lightcraft_geom::Orientation::Normal;
        let small = render(&src, &info, &s, &RenderRequest { overlay: Overlay::SensorClipping, ..RenderRequest::fit(40, 20) }).image;
        assert_eq!((small.width, small.height), (40, 20));
        assert_eq!(small.data[10 * 40 + 5], [r[0], r[1], r[2], 255], "a block with a clipped photosite counts as clipped");
    }

    #[test]
    fn sensor_clip_orientation_and_fit_helpers_match_the_picture() {
        use lightcraft_geom::Orientation;
        // 3×2, one marked photosite per corner colour
        let clip = SensorClip { width: 3, height: 2, data: vec![0b001, 0b010, 0b000, 0b000, 0b000, 0b100] };
        // at() is 0 outside
        assert_eq!(clip.at(3, 0), 0);
        assert_eq!(clip.at(0, 2), 0);
        assert_eq!(clip.at(1, 1), 0b000);
        // Rotate90: dst(x, y) = src(y, h−1−x); the mark at src (0, 0) shows at dst (1, 0)
        let rot = clip.clone().into_oriented(Orientation::Rotate90);
        assert_eq!((rot.width, rot.height), (2, 3));
        assert_eq!(rot.at(1, 0), 0b001);
        assert_eq!(rot.at(0, 0), 0b000);
        // Rotate180: dst(x, y) = src(w−1−x, h−1−y); the mark at src (0, 0) shows at dst (2, 1)
        let rot = clip.clone().into_oriented(Orientation::Rotate180);
        assert_eq!((rot.width, rot.height), (3, 2));
        assert_eq!(rot.at(2, 1), 0b001);
        // Rotate270: dst(x, y) = src(h−1−y, x); the mark at src (0, 0) shows at dst (0, 2)
        let rot = clip.clone().into_oriented(Orientation::Rotate270);
        assert_eq!((rot.width, rot.height), (2, 3));
        assert_eq!(rot.at(0, 2), 0b001);
        // FlipH: dst(x, y) = src(w−1−x); the mark at src (0, 0) shows at dst (2, 0)
        let flip = clip.clone().into_oriented(Orientation::FlipH);
        assert_eq!((flip.width, flip.height), (3, 2));
        assert_eq!(flip.at(2, 0), 0b001);
        // fit_or ORs the footprint and picks the same target size as resample::fit
        let fit = clip.fit_or(2, 2);
        assert_eq!((fit.width, fit.height), (2, 1));
        assert_eq!(fit.at(0, 0), 0b001, "left block keeps the red mark");
        assert_eq!(fit.at(1, 0), 0b110, "right block ORs the green and blue marks");
        // fitting to its own size is a no-op
        let same = clip.clone().fit_or(3, 2);
        assert_eq!(same, clip);
    }
}
