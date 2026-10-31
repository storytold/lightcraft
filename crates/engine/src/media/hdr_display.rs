//! HDR display (LR-VIEW-HDR-DISPLAY): an HDR edit's loupe job on an HDR window also renders the
//! HDR rendition ([`OutputDepth::F32Hdr`] in sRGB primaries: display-linear, SDR white = 1) into
//! [`Rendered::deep`](lightcraft_pipeline::Rendered::deep), next to the usual 8-bit SDR rendition
//! the histogram, overlays and previews use. The window paints it (`lightcraft-hdr-display`).

use lightcraft_pipeline::{DeepImage, DeepSamples, OutputDepth, Quality, RenderRequest};
use lightcraft_raster::Rgb32f;

use super::render::{RenderJob, develop};
use super::{Arc, DevelopSettings, SourceInfo, StageCache};

impl RenderJob {
    /// For an HDR display: an HDR edit's job also renders its HDR rendition into
    /// [`Rendered::deep`](lightcraft_pipeline::Rendered::deep). Gets its own result key and never
    /// comes from the rendered-thumbnail cache (which has no HDR). Left alone for SDR edits,
    /// overlays, proofs, windows and high-bit-depth requests (those draw the 8-bit image); a
    /// [draft](Self::draft) renders no HDR rendition (a drag shows SDR until it ends).
    pub fn with_hdr_display(mut self, on: bool) -> Self {
        let wanted = on
            && self.settings.hdr.enabled
            && self.request.overlay == lightcraft_pipeline::Overlay::None
            && self.request.proof.is_none()
            && self.request.window.is_none()
            && self.request.depth == OutputDepth::U8;
        if wanted && !self.hdr_display {
            self.hdr_display = true;
            self.key ^= 0x5851_f42d_4c95_7f2d;
            self.cache = None;
        }
        self
    }
}

/// The HDR rendition for [`RenderJob::with_hdr_display`] (CPU) of a job's `settings`, `request`,
/// `stages` and [display fit](RenderJob::display_fit). Not for a drag's drafts: on the CPU it would
/// make each one 3–5× slower than the GPU's SDR draft, so a drag shows the SDR rendition and the
/// full render brings HDR back. With a display fit the rendition is shrunk the same way as the
/// 8-bit image.
pub(super) fn rendition(
    settings: &DevelopSettings,
    request: &RenderRequest,
    stages: Option<&StageCache>,
    display_fit: Option<(usize, usize)>,
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
) -> Option<DeepImage> {
    if request.quality == Quality::Draft {
        return None;
    }
    let hreq = RenderRequest {
        depth: OutputDepth::F32Hdr,
        space: lightcraft_pipeline::OutputSpace::Srgb,
        overlay: lightcraft_pipeline::Overlay::None,
        proof: None,
        display: None,
        ..*request
    };
    let deep = develop(src, info, settings, &hreq, stages, false).deep?;
    Some(match display_fit {
        Some((fw, fh)) if deep.width > fw.max(1) || deep.height > fh.max(1) => fit_deep(deep, fw, fh),
        _ => deep,
    })
}

/// `deep` (float RGB) shrunk to fit `max_w × max_h` with the Lanczos filter the 8-bit image gets
/// ([`super::fit_for_display`]); negative ringing is clamped away. Anything that is not
/// consistent float RGB is returned as it is.
fn fit_deep(deep: DeepImage, max_w: usize, max_h: usize) -> DeepImage {
    use lightcraft_raster::resample::{Filter, fit};
    let DeepSamples::F32(rgb) = &deep.samples else { return deep };
    let Some(n) = deep.width.checked_mul(deep.height) else { return deep };
    let (px, _) = rgb.as_chunks::<3>();
    let Some(px) = px.get(..n).filter(|_| n > 0) else { return deep };
    let img = Rgb32f { width: deep.width, height: deep.height, data: px.to_vec() };
    let small = fit(&img, max_w.max(1), max_h.max(1), Filter::Lanczos3);
    let samples = DeepSamples::F32(small.data.iter().flat_map(|p| p.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 })).collect());
    DeepImage { width: small.width, height: small.height, space: deep.space, samples }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_pipeline::OutputSpace;

    #[test]
    fn a_display_fit_shrinks_the_hdr_rendition_like_the_image() {
        let deep = DeepImage { width: 8, height: 4, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![2.5; 8 * 4 * 3]) };
        let small = fit_deep(deep, 4, 4);
        assert_eq!((small.width, small.height), (4, 2));
        let DeepSamples::F32(v) = &small.samples else { panic!("float samples") };
        assert_eq!(v.len(), 4 * 2 * 3);
        assert!(v.iter().all(|x| (x - 2.5).abs() < 1e-3), "{v:?}");
    }

    #[test]
    fn inconsistent_renditions_are_left_alone() {
        let short = DeepImage { width: 8, height: 4, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![1.0; 5]) };
        let kept = fit_deep(short, 4, 4);
        assert_eq!((kept.width, kept.height), (8, 4));
        let empty = DeepImage { width: 0, height: 4, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![]) };
        assert_eq!(fit_deep(empty, 4, 4).width, 0);
    }
}
