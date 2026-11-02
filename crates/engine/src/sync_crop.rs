//! A crop synced to a photo of another frame shape (Auto Sync, Sync Settings): test notes
//! 2026-10-25 note 15. The crop is stored in normalized coordinates of its photo's frame, so the
//! same numbers on a photo turned the other way (a vertical among landscapes, a photo rotated
//! or of another camera's shape) are another shape: a 3:2 crop of a landscape became a 9:4 sliver
//! of a vertical, or a vertical was given a landscape-shaped crop. Here the crop keeps what it
//! means instead: its ratio lock, flips and angle, its shape turned to the photo's own orientation,
//! how tight it is, and where it sits.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::Crop;
use lightcraft_geom::{CropGeometry, MIN_CROP_FRACTION, Point, Rect, crop_fit_angle};

use crate::Session;

/// The crop a photo with frame `odims` (oriented width, height) gets when the source photo (frame
/// `adims`) has crop `new`: the same aspect lock, flips and angle; on a frame of the same shape the
/// same rectangle. On another shape the crop keeps its tightness and position: its shape is turned
/// to the photo's own orientation (a landscape never gets a portrait-shaped crop, nor a vertical a
/// landscape one), its size is the same fraction of the largest crop of that shape at that angle as
/// on the source, and its centre keeps its normalized place in the frame, moved towards the middle
/// only as far as it must to stay inside the rotated photo. An uncropped photo gives each photo
/// its own whole frame at that angle.
pub(crate) fn synced_crop(new: &Crop, adims: (f64, f64), odims: (f64, f64)) -> Crop {
    let (aw, ah) = (adims.0.max(1.0), adims.1.max(1.0));
    let (ow, oh) = (odims.0.max(1.0), odims.1.max(1.0));
    let (ar, or) = (aw / ah, ow / oh);
    if (ar - or).abs() <= 0.005 * ar {
        return *new;
    }
    let wide = or > 1.0;
    // the shape turned to the photo's own orientation (a square crop stays a square; on a square
    // photo, which has no orientation, the crop keeps its own)
    let square = (or - 1.0).abs() <= 0.005;
    let orient = |s: f64| if !square && s != 1.0 && (s > 1.0) != wide { 1.0 / s } else { s };
    let aspect = turned_aspect(new.aspect, odims);
    let angle = new.geometry.angle;
    let full = crop_fit_angle(aw, ah, angle, None).rect;
    let r = new.geometry.rect;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
    let uncropped = new.aspect.is_none() && close(r.x0, full.x0) && close(r.y0, full.y0) && close(r.x1, full.x1) && close(r.y1, full.y1);
    let px = new.geometry.rect_px(aw, ah);
    let own = new.aspect.map(|(x, y)| f64::from(x) / f64::from(y.max(1))).or(Some(px.width() / px.height()));
    let own = own.filter(|s| s.is_finite() && *s > 0.0);
    let (Some(own), false) = (own, uncropped) else {
        return Crop { geometry: crop_fit_angle(ow, oh, angle, None), aspect, flip_h: new.flip_h, flip_v: new.flip_v };
    };
    // tightness: the crop's size against the largest crop of its own shape on the source photo
    let largest = crop_fit_angle(aw, ah, angle, Some(own)).rect_px(aw, ah);
    let frac = (px.width() / largest.width()).clamp(MIN_CROP_FRACTION, 1.0);
    let frac = if frac.is_finite() { frac } else { 1.0 };
    let fit = crop_fit_angle(ow, oh, angle, Some(orient(own)));
    let f = fit.rect_px(ow, oh);
    let (cw, ch) = (f.width() * frac, f.height() * frac);
    let at = |c: Point| CropGeometry { rect: Rect::from_center(c, cw, ch).scale(1.0 / ow, 1.0 / oh), angle };
    let mid = f.center();
    let want = px.center();
    let want = Point::new(want.x / aw * ow, want.y / ah * oh);
    let want = if want.x.is_finite() && want.y.is_finite() { want } else { mid };
    // the centres where a crop of this size fits form a convex set holding the middle: the first
    // point on the way from `want` to the middle that fits
    let geometry = if at(want).is_within_image(ow, oh) {
        at(want)
    } else {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let t = (lo + hi) / 2.0;
            if at(want.lerp(mid, t)).is_within_image(ow, oh) {
                hi = t;
            } else {
                lo = t;
            }
        }
        at(want.lerp(mid, hi))
    };
    Crop { geometry, aspect, flip_h: new.flip_h, flip_v: new.flip_v }
}

/// A ratio lock `aspect` turned to the orientation of a photo with frame `odims` (oriented width,
/// height): 3:2 on a landscape, 2:3 on a vertical. A square lock, and any lock on a square photo
/// (which has no orientation), stay as they are.
pub(crate) fn turned_aspect(aspect: Option<(u32, u32)>, odims: (f64, f64)) -> Option<(u32, u32)> {
    let or = odims.0.max(1.0) / odims.1.max(1.0);
    let (wide, square) = (or > 1.0, (or - 1.0).abs() <= 0.005);
    aspect.map(|(x, y)| if !square && x != y && (x > y) != wide { (y, x) } else { (x, y) })
}

impl Session {
    /// Photo `id`'s frame as a crop on it sees it with `orientation`: width and height, swapped
    /// when it is turned a quarter.
    pub(crate) fn crop_frame(&self, id: PhotoId, orientation: lightcraft_geom::Orientation) -> (f64, f64) {
        let (w, h) = self.catalog.photo(id).map(|p| (p.width.max(1) as f64, p.height.max(1) as f64)).unwrap_or((3.0, 2.0));
        if orientation.swaps_axes() { (h, w) } else { (w, h) }
    }

    /// `synced` (photo `x`'s settings after a sync from photo `id` with settings `src`) with the
    /// source's crop fitted to `x`'s own frame ([`synced_crop`]).
    pub(crate) fn fit_synced_crop(
        &self,
        id: PhotoId,
        src: &lightcraft_develop::DevelopSettings,
        x: PhotoId,
        synced: &mut lightcraft_develop::DevelopSettings,
    ) {
        synced.crop = synced_crop(&src.crop, self.crop_frame(id, src.orientation), self.crop_frame(x, synced.orientation));
    }
}
