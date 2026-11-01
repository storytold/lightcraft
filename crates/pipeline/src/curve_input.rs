//! Diagnostic samples at the selected Tone Curve's input, in the preview's encoded domain.
//!
//! The normal renderer is unchanged. A small, separate render shares its spatial stages and
//! per-pixel code, stopping before the selected curve and everything that follows it.

use lightcraft_develop::{DevelopSettings, ToneCurve};
use lightcraft_raster::{Histogram, Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::finish::{FinishParams, FinishParamsInput};
use crate::{RenderRequest, Rendered, SourceInfo, StageCache};

/// Maximum long edge of the diagnostic render. The UI starts from a thumbnail-level source.
pub const EDGE: usize = 256;

/// The curve whose input is sampled. RGB curves are independent and share the same predecessor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurveInput {
    Parametric,
    Master,
    Red,
    Green,
    Blue,
}

impl CurveInput {
    /// Only curves before this one, in the renderer's order: parametric, master, RGB.
    /// Call after profile expansion when evaluating pixels: a profile can contribute to curves.
    pub fn predecessors(self, curve: &ToneCurve) -> ToneCurve {
        if self == Self::Parametric {
            return ToneCurve::default();
        }
        let mut c = curve.clone();
        if self == Self::Master {
            c.master.clear();
        }
        c.red.clear();
        c.green.clear();
        c.blue.clear();
        c.refine_saturation = 100.0;
        c
    }

    /// Settings identity for this diagnostic. The selected curve, its successors, Refine
    /// Saturation and grain cannot affect its input. Retain the profile: its upstream look can.
    pub fn settings_key(self, settings: &DevelopSettings) -> u64 {
        let mut s = settings.clone();
        s.curve = self.predecessors(&s.curve);
        s.grain = Default::default();
        s.hash64()
    }
}

/// Sample the whole cropped frame, at most [`EDGE`] pixels per edge. A zoom window is deliberately
/// ignored: panning does not change the photo's tonal distribution. Uses the request's rendition
/// and output primaries (including display/proof transforms), with no diagnostic overlays.
///
/// The image is a diagnostic view of the encoded input; its histogram is binned from floats,
/// before quantization. For Master/Parametric, `luma` is an encoded luminance summary (the actual
/// curves act on each RGB component). Values outside 0..1 and nonfinite values are not plotted.
/// An 8-bit preview request of an HDR edit uses its SDR rendition, like the main preview.
pub fn render(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    settings: &DevelopSettings,
    request: &RenderRequest,
    cache: Option<&StageCache>,
    input: CurveInput,
) -> Rendered {
    let request = RenderRequest {
        max_w: request.max_w.clamp(1, EDGE),
        max_h: request.max_h.clamp(1, EDGE),
        window: None,
        overlay: crate::Overlay::None,
        ..*request
    };
    crate::render_impl(crate::Src::Shared(src), info, settings, &request, cache, Some(input))
}

pub(crate) fn finish(
    p: &crate::Prepared,
    settings: &DevelopSettings,
    frame: &crate::geometry::Frame,
    info: &SourceInfo,
    request: &RenderRequest,
    input: CurveInput,
) -> Rendered {
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(FinishParamsInput {
        settings,
        frame,
        info,
        image_size: (w, h),
        px_per_long: p.px_per_long,
        air_pre: p.air,
        space: request.space,
    });
    fp.proof = request.proof.map(|proof| proof.params(request.space));
    if let Some(display) = &request.display {
        fp.for_display(display, request.proof);
    }
    fp.curves = crate::finish::curve_luts(&input.predecessors(&settings.curve));
    fp.refine_sat = 1.0;
    let samples = crate::finish::finish_with(p, &fp, false, true, |e| e);
    let histogram = of_encoded(&samples, fp.out_luma);
    let encode = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let image = Rgba8 { width: w, height: h, data: samples.iter().map(|c| [encode(c[0]), encode(c[1]), encode(c[2]), 255]).collect() };
    Rendered { image, histogram, deep: None }
}

fn of_encoded(samples: &[[f32; 3]], weights: [f32; 3]) -> Histogram {
    let mut h = Histogram { r: vec![0; 256], g: vec![0; 256], b: vec![0; 256], luma: vec![0; 256], total: 0, hdr: None };
    for &[r, g, b] in samples.iter().take(EDGE * EDGE) {
        let y = r * weights[0] + g * weights[1] + b * weights[2];
        for (bins, value) in [(&mut h.r, r), (&mut h.g, g), (&mut h.b, b), (&mut h.luma, y)] {
            if value.is_finite()
                && (0.0..=1.0).contains(&value)
                && let Some(count) = bins.get_mut((value * 255.0 + 0.5) as usize)
            {
                *count += 1;
            }
        }
        h.total += 1;
    }
    h
}

#[cfg(test)]
#[path = "curve_input_tests.rs"]
mod tests;
