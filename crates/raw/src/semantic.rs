//! DNG semantic masks (DNG 1.6+, chapter "Semantic Masks"): single-channel segmentation mattes
//! stored in their own IFDs (NewSubFileType 10004.H, PhotometricInterpretation 52527) with a
//! `SemanticName`, an optional `SemanticInstanceID` and an optional `MaskSubArea`. iPhone ProRAW
//! stores e.g. a sky matte this way.
//!
//! [`semantic_masks`] reads them separately from [`crate::decode`] (only callers that use them
//! pay for them); malformed or unsupported mask IFDs are skipped.

use crate::Rect;
use crate::tiffraw::{Packing, read_image};
use crate::{RawData, Result};
use lightcraft_raster::Image;
use lightcraft_tiff::Tiff;
use lightcraft_tiff::tags::{self as t, photometric};

/// At most this many masks are read from one file.
pub const MAX_MASKS: usize = 16;
/// Masks with more samples than this are skipped (a mask is a low-resolution matte; this is
/// 8192 × 4096).
pub const MAX_MASK_SAMPLES: usize = 1 << 25;

/// Where a cropped mask sits in the uncropped mask (DNG `MaskSubArea`). The uncropped mask
/// (`full_width × full_height`) spans the main image's active area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaskSubArea {
    pub top: u32,
    pub left: u32,
    pub full_width: u32,
    pub full_height: u32,
}

/// One semantic mask: `width × height` values, 0 = not part of the attribute, 65535 = fully part.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticMask {
    /// `SemanticName`, e.g. `urn:com:apple:photo:2020:aux:semanticskymatte`.
    pub name: String,
    pub instance_id: Option<String>,
    pub width: usize,
    pub height: usize,
    /// `None`: the mask covers the whole active area.
    pub sub_area: Option<MaskSubArea>,
    pub data: Vec<u16>,
}

/// The semantic masks of a DNG, in file order (none for other files).
pub fn semantic_masks(bytes: &[u8]) -> Vec<SemanticMask> {
    let Ok(tiff) = Tiff::parse(bytes) else { return Vec::new() };
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u16(t::PHOTOMETRIC) == Some(photometric::MASK) || i.u32(t::NEW_SUBFILE_TYPE) == Some(t::SUBFILE_SEMANTIC_MASK))
        .filter_map(|ifd| read_mask(bytes, &tiff, ifd).ok().flatten())
        .take(MAX_MASKS)
        .collect()
}

fn read_mask(bytes: &[u8], tiff: &Tiff, ifd: &lightcraft_tiff::Ifd) -> Result<Option<SemanticMask>> {
    let Some(name) = ifd.string(t::SEMANTIC_NAME).filter(|n| !n.is_empty()) else { return Ok(None) };
    let info = ifd.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if info.samples_per_pixel != 1 || w.checked_mul(h).is_none_or(|n| n > MAX_MASK_SAMPLES) {
        return Ok(None);
    }
    let bits = info.bits() as u32;
    let data: Vec<u16> = match read_image(bytes, &info, tiff.order, Packing::Msb)? {
        RawData::U16(v) if (1..=16).contains(&bits) => {
            let max = ((1u32 << bits) - 1) as f32;
            if bits == 16 { v } else { v.into_iter().map(|s| (s as f32 / max * 65535.0).round().min(65535.0) as u16).collect() }
        }
        RawData::F32(v) => v.into_iter().map(|s| if s.is_finite() { (s.clamp(0.0, 1.0) * 65535.0).round() as u16 } else { 0 }).collect(),
        RawData::U16(_) => return Ok(None),
    };
    if data.len() != w * h {
        return Ok(None);
    }
    // the tag is ignored when the stored mask does not fit inside the uncropped one
    let sub_area = match ifd.u64s(t::MASK_SUB_AREA).as_deref() {
        Some(&[top, left, fw, fh]) => {
            let fits = fw > 0 && fh > 0 && left.saturating_add(w as u64) <= fw && top.saturating_add(h as u64) <= fh && fw.max(fh) <= u32::MAX as u64;
            fits.then_some(MaskSubArea { top: top as u32, left: left as u32, full_width: fw as u32, full_height: fh as u32 })
        }
        _ => None,
    };
    Ok(Some(SemanticMask { name, instance_id: ifd.string(t::SEMANTIC_INSTANCE_ID).filter(|s| !s.is_empty()), width: w, height: h, sub_area, data }))
}

impl SemanticMask {
    /// The mask value (0..1) at a continuous position of the uncropped mask (pixel centres at
    /// +0.5); 0 outside the stored (cropped) mask.
    pub fn value_at(&self, x: f64, y: f64) -> f32 {
        let (left, top) = self.sub_area.map_or((0.0, 0.0), |a| (a.left as f64, a.top as f64));
        let (x, y) = (x - left, y - top);
        if self.width == 0 || self.height == 0 || !(x >= 0.0 && y >= 0.0 && x <= self.width as f64 && y <= self.height as f64) {
            return 0.0;
        }
        let (fx, fy) = ((x - 0.5).max(0.0), (y - 0.5).max(0.0));
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
        let at = |x: usize, y: usize| {
            let (x, y) = (x.min(self.width - 1), y.min(self.height - 1));
            self.data.get(y * self.width + x).copied().unwrap_or(0) as f32 / 65535.0
        };
        let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
        top + (bottom - top) * ty
    }

    /// The mask over the developed image ([`crate::RawImage::develop`]: the default `crop` of the
    /// `active_area`, before orientation), at the mask's own resolution, as 0..255 alpha. `None`
    /// for an empty crop.
    pub fn developed(&self, active_area: Rect, crop: Rect) -> Option<Image<u8>> {
        let c = crop.clipped(active_area.width, active_area.height);
        if c.width == 0 || c.height == 0 || self.width == 0 || self.height == 0 {
            return None;
        }
        let (fw, fh) = self.sub_area.map_or((self.width, self.height), |a| (a.full_width as usize, a.full_height as usize));
        // uncropped-mask px per active-area px
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
