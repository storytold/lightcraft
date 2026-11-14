//! Developed semantic mask conversion to `lightcraft_raster::Image<u8>`.

pub use craft_raw::semantic::*;
use lightcraft_raster::Image;

/// Extension trait to render a [`SemanticMask`] onto the developed image.
pub trait SemanticMaskExt {
    /// The mask over the developed image (the default `crop` of the `active_area`, before
    /// orientation), at the mask's own resolution, as 0..255 alpha. `None` for an empty crop.
    fn developed(&self, active_area: craft_raw::Rect, crop: craft_raw::Rect) -> Option<Image<u8>>;
}

impl SemanticMaskExt for SemanticMask {
    fn developed(&self, active_area: craft_raw::Rect, crop: craft_raw::Rect) -> Option<Image<u8>> {
        let c = crop.clipped(active_area.width, active_area.height);
        if c.width == 0 || c.height == 0 || self.width == 0 || self.height == 0 {
            return None;
        }
        let (fw, fh) = self.sub_area.map_or((self.width, self.height), |a| (a.full_width as usize, a.full_height as usize));
        let (sx, sy) = (fw as f64 / active_area.width as f64, fh as f64 / active_area.height as f64);
        let ow = ((c.width as f64 * sx).round() as usize).clamp(1, self.width.max(fw));
        let oh = ((c.height as f64 * sy).round() as usize).clamp(1, self.height.max(fh));
        if ow.checked_mul(oh).is_none_or(|n| n > MAX_MASK_SAMPLES) {
            return None;
        }
        let (kx, ky) = (c.width as f64 / ow as f64, c.height as f64 / oh as f64);
        let mut out = Image::new(ow, oh);
        lightcraft_raster::par_rows(&mut out.data, ow, |y, row| {
            let ay = c.y as f64 + (y as f64 + 0.5) * ky;
            for (x, v) in row.iter_mut().enumerate() {
                let ax = c.x as f64 + (x as f64 + 0.5) * kx;
                *v = (self.value_at(ax * sx, ay * sy) * 255.0).round() as u8;
            }
        });
        Some(out)
    }
}
