//! HDR display of the loupe (LR-VIEW-HDR-DISPLAY): on a window whose surface is HDR (scRGB:
//! 1.0 = SDR white, brighter above), an HDR edit's loupe render also carries its HDR values
//! ([`HdrPixels`]), and the host paints them through an [`HdrPresenter`] instead of drawing the
//! 8-bit SDR view. The presenter belongs to the host's renderer (the desktop app's egui-wgpu
//! paint callback); this crate stays renderer-agnostic, and without a presenter nothing changes.

use std::sync::Arc;

/// An HDR render's pixels: display-linear RGB in sRGB primaries, 1.0 = SDR white (interleaved,
/// row-major), as [`lightcraft_pipeline::OutputDepth::F32Hdr`] renders them.
#[derive(Debug, PartialEq)]
pub struct HdrPixels {
    /// The render's key: a presenter re-uploads only when it changes.
    pub key: u64,
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<f32>,
}

impl HdrPixels {
    /// The HDR values of `deep`, if it is an HDR render with consistent dimensions.
    pub fn of(key: u64, deep: &lightcraft_pipeline::DeepImage) -> Option<HdrPixels> {
        let lightcraft_pipeline::DeepSamples::F32(v) = &deep.samples else { return None };
        let n = deep.width.checked_mul(deep.height)?.checked_mul(3)?;
        (deep.hdr && deep.width > 0 && deep.height > 0 && v.len() >= n).then(|| HdrPixels {
            key,
            width: deep.width,
            height: deep.height,
            rgb: v.clone(),
        })
    }
}

/// Paints HDR pixels into an HDR window (implemented by the host's renderer).
pub trait HdrPresenter {
    /// Draw `pixels` stretched over `rect` (points), clipped to the painter's clip rectangle.
    fn paint(&self, painter: &egui::Painter, rect: egui::Rect, pixels: &Arc<HdrPixels>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_pipeline::{DeepImage, DeepSamples, OutputSpace};

    #[test]
    fn only_consistent_hdr_renders_become_hdr_pixels() {
        let deep = |hdr, w, h, n| DeepImage { width: w, height: h, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![2.0; n]), hdr };
        let p = HdrPixels::of(7, &deep(true, 2, 2, 12)).unwrap();
        assert_eq!((p.key, p.width, p.height, p.rgb.len()), (7, 2, 2, 12));
        assert!(HdrPixels::of(7, &deep(false, 2, 2, 12)).is_none(), "SDR float renders");
        assert!(HdrPixels::of(7, &deep(true, 2, 2, 5)).is_none(), "short buffers");
        assert!(HdrPixels::of(7, &deep(true, 0, 2, 0)).is_none(), "empty");
        assert!(HdrPixels::of(7, &deep(true, usize::MAX, 3, 12)).is_none(), "overflowing sizes");
        let u16s = DeepImage { width: 1, height: 1, space: OutputSpace::Srgb, samples: DeepSamples::U16(vec![0; 3]), hdr: true };
        assert!(HdrPixels::of(7, &u16s).is_none());
    }
}
