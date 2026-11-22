//! Spatial context for a window of the full output. The UI's margin is for pan
//! reuse; filter support is computed here, before the CPU and GPU share a plan.

use lightcraft_develop::{DevelopSettings, MaskShape};
use lightcraft_raster::blur::{box_radii, gauss_taps};

use crate::{PixelWindow, Quality, geometry::Frame, local, masks, masks::Mattes};

/// Finite support of the three box passes used by `gaussian`, in each axis.
fn gaussian_radius(sigma: f32) -> usize {
    if sigma <= 0.3 { 0 } else { box_radii(sigma).into_iter().sum() }
}

fn guided_radius(sigma: f32) -> usize {
    let step = local::guided_fast_step(sigma);
    // Two Gaussian convolutions, plus the box downsample and bilinear upsample.
    // full / ceil(full / step) never exceeds step on either axis.
    // GuidedGrid keeps that full-frame scale and phase for partial windows;
    // these extra taps also exclude the clamped outer work boundary.
    let radius = 2 * gaussian_radius(sigma / step as f32) * step;
    if step > 1 { radius + 2 * step } else { radius }
}

/// Support of the plane the highlights/shadows base filters: `detail` px for V2's median; from V8
/// the coarse median of `local::detail_free` too, at `detail` px of a `BASE_COARSE_STEP`× smaller
/// copy, plus its box downsample, bilinear upsample and the clamped outer work boundary.
fn base_detail_radius(s: &DevelopSettings, detail: usize) -> usize {
    if detail > 0 && local::base_keeps_gaps(s) { (detail + 3) * local::BASE_COARSE_STEP } else { detail }
}

/// Context for the local planes, including upstream noise reduction. Parallel
/// planes take the maximum support; sequential filters add their supports.
/// The planner adds Defringe support after resolving spot source reads. `shown`
/// is the mask an overlay draws, hidden or not.
pub(crate) fn margin(
    s: &DevelopSettings,
    (frame, full_w, full_h): (&Frame, usize, usize),
    (detail_scale, native_long): (f32, f64),
    q: Quality,
    mattes: Option<&Mattes>,
    shown: Option<u32>,
) -> usize {
    let ppl = frame.px_per_long(full_w);
    let out_long = full_w.max(full_h);
    let sig = local::context_plane_sigmas(s, ppl, native_long, q);
    let mut radius = sig.base.map_or(0, |sg| guided_radius(sg) + base_detail_radius(s, sig.base_detail));
    radius = radius.max(sig.clarity.map_or(0, guided_radius));
    for sigma in [sig.texture, sig.dark, sig.chroma].into_iter().flatten() {
        radius = radius.max(gaussian_radius(sigma));
    }
    // process V3's pixel-scale sharpening blur is an exact Gaussian of its own taps
    radius = radius.max(sig.sharpen.map_or(0, |sg| gauss_taps(sg).len() / 2));
    // Mask alpha is prepared in parallel with the local planes. Its refinement
    // reads the shape's blurred alpha, so those supports add too.
    for mask in s.masks.iter().filter(|m| (m.visible || shown == Some(m.id)) && !m.components.is_empty()) {
        let shape = mask
            .components
            .iter()
            .map(|c| match &c.shape {
                shape if mattes.is_some_and(|m| m.uses_matte(shape)) => 0,
                MaskShape::Sky => gaussian_radius((0.01 * ppl as f32).max(0.5)),
                MaskShape::Object { seg, detail, edge, .. } | MaskShape::Prompt { seg, detail, edge, .. } if seg.is_some() || !detail.is_empty() => {
                    // The model's grid is guided-filtered, then Edge feathers it.
                    let grid = masks::seg_sigma(seg.as_ref(), detail, frame, full_w).map_or(0, |sg| guided_radius(sg.clamp(0.5, out_long as f32)));
                    let feather =
                        if *edge > 0.0 { gaussian_radius(((*edge / 100.0).clamp(0.0, 1.0) as f32 * 0.02 * ppl as f32).max(0.5)) } else { 0 };
                    grid + feather
                }
                MaskShape::Object { hint, .. } if hint.is_empty() => 0,
                MaskShape::Subject | MaskShape::Background | MaskShape::Object { .. } | MaskShape::People { .. } => {
                    gaussian_radius(0.03 * ppl as f32) + gaussian_radius((0.015 * ppl as f32).max(0.5))
                }
                // Auto Mask's dab reads and refinement are handled by window_for_reads.
                MaskShape::Brush { .. }
                | MaskShape::Linear { .. }
                | MaskShape::Radial { .. }
                | MaskShape::LuminanceRange { .. }
                | MaskShape::ColorRange { .. }
                | MaskShape::DepthRange { .. }
                | MaskShape::Landscape { .. }
                | MaskShape::Prompt { .. } => 0,
            })
            .max()
            .unwrap_or(0);
        let refine =
            if mask.refine > 0.0 { 2 * gaussian_radius((0.04 * out_long as f32 * (mask.refine / 100.0).clamp(0.0, 1.0) as f32).max(1.0)) } else { 0 };
        radius = radius.max(shape + refine);
    }
    let (lum, col) = local::nr_params(s, detail_scale);
    radius += lum.map_or(0, |n| 2 * gaussian_radius(n.sigma));
    radius += col.map_or(0, |n| gaussian_radius(n.sigma));
    radius
}

/// Grow inside the real frame. No arbitrary cap may discard context: the work
/// buffers can exceed the display texture limit; only the requested pixels are
/// returned. The GPU's storage limit still falls back to the CPU as usual.
pub(crate) fn expand(win: PixelWindow, full_w: usize, full_h: usize, margin: usize) -> PixelWindow {
    let win = win.clamped(full_w, full_h);
    let x = win.x.saturating_sub(margin);
    let y = win.y.saturating_sub(margin);
    let end_x = win.x.saturating_add(win.w).saturating_add(margin).min(full_w);
    let end_y = win.y.saturating_add(win.h).saturating_add(margin).min(full_h);
    PixelWindow { x, y, w: end_x - x, h: end_y - y }
}
