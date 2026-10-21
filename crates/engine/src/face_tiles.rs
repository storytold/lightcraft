//! Face tiles cut from a raw's embedded preview (issue #1440).
//!
//! A People card's face picture ([`crate::Session::face_job`]) is a small square around the face.
//! Developing the raw for it reads and demosaics most of a 24–50 MB file for a ~170 px picture; the
//! camera's own JPEG inside the file (read through the planned preview path,
//! [`crate::files::preview_window`], which reads the header and the preview and not the sensor
//! data) usually holds that square at the tile's size already. A [`PreviewCut`] says which preview
//! to read and how large it must be; [`crate::media::RenderJob::run`] tries it before the job's own
//! source and falls back to that source when the preview can't serve.
//!
//! Only a raw that still looks the way it was imported gets one: that is the photo whose grid
//! thumbnail is the same embedded JPEG ([`crate::Session::embedded_thumb_is_final`]). An edited raw
//! needs its edits rendered, which the camera's display-referred JPEG can't take the way the raw
//! does, so it keeps the full develop render.

use lightcraft_geom::Rect;
use lightcraft_raster::Rgba8;
use lightcraft_raster::resample::{Filter, resize};

use crate::media::PreviewLoader;

/// How much smaller than asked an embedded preview may be and still serve a face tile: at least
/// three quarters of the pixels the tile asks for across the face. The square is then enlarged by
/// at most 4⁄3 on its way to the tile, which at a 56–180 pt tile is not visible (the camera's JPEG
/// is sharpened and denoised, so its pixels hold more detail than a binned raw's), and it is the
/// same bound the raw path already accepts for an X-Trans sensor binned 3× for a preview
/// ([`crate::files::bin_factor`]). Below it the tile would look soft: the job's own source renders it.
pub const SOFT_LIMIT: f64 = 0.75;

/// Largest relative difference between the preview's and the photo's aspect ratios: a preview of
/// another shape (letterboxed, 16:9 from a 3:2 sensor, a 4:3 sensor's 3:2 JPEG) does not show the
/// same frame, and the face box would land beside the face. Real previews of the same frame differ
/// by rounding only (Sony A7S III: 1616 × 1080 against 4240 × 2832, 0.06 %).
const ASPECT_TOLERANCE: f64 = 0.02;

/// A face tile to cut from the photo's embedded preview.
#[derive(Clone)]
pub struct PreviewCut {
    /// The raw file.
    pub path: String,
    /// Reads its largest embedded preview, display-ready and upright, no larger than the edge given.
    pub loader: PreviewLoader,
    /// The preview's long edge that gives the tile its own size across the face.
    pub want: usize,
    /// The smallest long edge accepted ([`SOFT_LIMIT`] of `want`).
    pub min: usize,
    /// The square around the face, normalized on the upright photo.
    pub rect: Rect,
    /// The photo's width / height, upright.
    pub aspect: f64,
    /// The tile's edge, pixels.
    pub edge: usize,
}

impl PreviewCut {
    /// The tile, or `None` when the preview can't give it: none could be read (no preview, an
    /// offline or damaged file), it is smaller than [`Self::min`], or it shows another frame.
    pub fn cut(&self) -> Option<Rgba8> {
        let preview = (self.loader)(&self.path, self.want)?;
        self.cut_from(&preview)
    }

    /// [`Self::cut`] from a preview already read.
    pub fn cut_from(&self, img: &Rgba8) -> Option<Rgba8> {
        let (w, h) = (img.width, img.height);
        if w == 0 || h == 0 || w.max(h) < self.min || img.data.len() != w.checked_mul(h)? {
            return None;
        }
        let aspect = w as f64 / h as f64;
        if !(self.aspect.is_finite() && self.aspect > 0.0) || ((aspect - self.aspect) / self.aspect).abs() > ASPECT_TOLERANCE {
            return None;
        }
        let r = self.rect;
        if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) {
            return None;
        }
        // pixel bounds inside the preview (the clamps keep every value in 0..=w / 0..=h)
        let px = |v: f64, n: usize| (v.clamp(0.0, 1.0) * n as f64).round() as usize;
        let (x0, x1, y0, y1) = (px(r.x0, w), px(r.x1, w), px(r.y0, h), px(r.y1, h));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let edge = self.edge.clamp(1, crate::media::SourceLevel::Thumb.max_edge());
        let square = img.crop(x0, y0, x1 - x0, y1 - y0).to_linear();
        Some(resize(&square, edge, edge, Filter::Mitchell).to_srgb8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cut(rect: Rect, aspect: f64, min: usize) -> PreviewCut {
        PreviewCut { path: String::new(), loader: Arc::new(|_, _| None), want: 300, min, rect, aspect, edge: 32 }
    }

    fn picture(w: usize, h: usize) -> Rgba8 {
        let mut img = Rgba8::new(w, h);
        for (i, p) in img.data.iter_mut().enumerate() {
            *p = [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 90, 255];
        }
        img
    }

    /// The square comes out the tile's size, from the place the box names.
    #[test]
    fn cuts_the_square_at_the_tiles_size() {
        let img = picture(300, 200);
        let out = cut(Rect { x0: 0.5, y0: 0.5, x1: 0.6, y1: 0.65 }, 1.5, 200).cut_from(&img).unwrap();
        assert_eq!((out.width, out.height), (32, 32));
        // red follows x: the box starts at half the width
        let red = out.data[16 * 32 + 16][0];
        assert!((125..=160).contains(&red), "{red}");
    }

    /// A preview too small, of another shape, empty, or a box that is no box: no tile, no panic.
    #[test]
    fn declines_what_it_cannot_cut() {
        let img = picture(300, 200);
        let ok = Rect { x0: 0.4, y0: 0.4, x1: 0.5, y1: 0.55 };
        assert!(cut(ok, 1.5, 301).cut_from(&img).is_none(), "below the soft limit");
        assert!(cut(ok, 16.0 / 9.0, 200).cut_from(&img).is_none(), "another frame");
        assert!(cut(ok, f64::NAN, 200).cut_from(&img).is_none());
        assert!(cut(ok, 0.0, 200).cut_from(&img).is_none());
        assert!(cut(ok, 1.5, 0).cut_from(&Rgba8::new(0, 0)).is_none(), "empty preview");
        let short = Rgba8 { width: 300, height: 200, data: vec![[0; 4]; 10] };
        assert!(cut(ok, 1.5, 0).cut_from(&short).is_none(), "a buffer shorter than its size");
        for bad in [
            Rect { x0: f64::NAN, y0: 0.4, x1: 0.5, y1: 0.5 },
            Rect { x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5 },
            Rect { x0: 0.6, y0: 0.6, x1: 0.4, y1: 0.4 },
            Rect { x0: 2.0, y0: 2.0, x1: 3.0, y1: 3.0 },
        ] {
            assert!(cut(bad, 1.5, 0).cut_from(&img).is_none(), "{bad:?}");
        }
        // a box past the edges is clamped to the picture
        let edge = cut(Rect { x0: -1.0, y0: -1.0, x1: 0.1, y1: 0.15 }, 1.5, 0).cut_from(&img).unwrap();
        assert_eq!((edge.width, edge.height), (32, 32));
        // and a loader with nothing to give leaves the job to its source
        assert!(cut(ok, 1.5, 0).cut().is_none());
    }
}
