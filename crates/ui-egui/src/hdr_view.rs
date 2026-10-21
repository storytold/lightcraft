//! HDR display of the loupe (LR-VIEW-HDR-DISPLAY): on a window whose surface is HDR (scRGB:
//! 1.0 = SDR white, brighter above), an HDR edit's loupe render also carries its HDR rendition
//! ([`HdrPixels`]), and the host paints it through an [`HdrPresenter`] instead of the 8-bit SDR
//! rendition. The presenter belongs to the host's renderer (`lightcraft-hdr-display` in the desktop
//! app); this crate stays renderer-agnostic, and without a presenter nothing changes.

use std::sync::Arc;

/// An HDR rendition's pixels: display-linear RGB in sRGB primaries, 1.0 = SDR white (interleaved,
/// row-major), as an [`OutputDepth::F32Hdr`](lightcraft_pipeline::OutputDepth::F32Hdr) render in
/// sRGB gives them.
#[derive(Debug, PartialEq)]
pub struct HdrPixels {
    /// The render's key: a presenter re-uploads only when it changes.
    pub key: u64,
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<f32>,
}

impl HdrPixels {
    /// The pixels of `deep` if it holds float samples with consistent dimensions (an HDR display
    /// job's HDR rendition; see `RenderJob::with_hdr_display`). Takes the samples over: a 4K
    /// rendition is ~100 MB, not worth copying on the UI thread.
    pub fn of(key: u64, deep: lightcraft_pipeline::DeepImage) -> Option<HdrPixels> {
        let lightcraft_pipeline::DeepSamples::F32(rgb) = deep.samples else { return None };
        let n = deep.width.checked_mul(deep.height)?.checked_mul(3)?;
        (deep.width > 0 && deep.height > 0 && rgb.len() >= n).then_some(HdrPixels { key, width: deep.width, height: deep.height, rgb })
    }

    /// The bytes the pixels hold.
    pub fn bytes(&self) -> usize {
        self.rgb.len().saturating_mul(std::mem::size_of::<f32>())
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
    fn only_consistent_float_renders_become_hdr_pixels() {
        let deep = |w, h, n| DeepImage { width: w, height: h, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![2.0; n]) };
        let p = HdrPixels::of(7, deep(2, 2, 12)).unwrap();
        assert_eq!((p.key, p.width, p.height, p.rgb.len(), p.bytes()), (7, 2, 2, 12, 48));
        assert!(HdrPixels::of(7, deep(2, 2, 5)).is_none(), "short buffers");
        assert!(HdrPixels::of(7, deep(0, 2, 0)).is_none(), "empty");
        assert!(HdrPixels::of(7, deep(usize::MAX, 3, 12)).is_none(), "overflowing sizes");
        let u16s = DeepImage { width: 1, height: 1, space: OutputSpace::Srgb, samples: DeepSamples::U16(vec![0; 3]) };
        assert!(HdrPixels::of(7, u16s).is_none(), "16-bit renders are not HDR");
    }
}
