//! Lens Blur ([`lightcraft_develop::LensBlur`]): a synthetic defocus driven by a per-pixel depth
//! estimate, so a background can be blurred progressively with distance from a chosen focal
//! plane, the way a wider aperture would have rendered it.
//!
//! The depth estimate here is classical, not a learned model: two monocular cues, blended and
//! smoothed. - detail: areas the original lens already rendered sharp are assumed closer to its
//! focal plane than areas it already rendered soft. - position: areas lower in the frame are
//! assumed closer (most photos are shot close to level, subject low, background rising behind
//! it). It is a heuristic approximation of where a trained depth model would place the focal
//! plane on an ordinary photo, not scene depth: a flat subject photographed from above, or a
//! sharp background behind a soft foreground, reads incorrectly. Swapping in a learned depth
//! model later only means replacing [`depth_map`]; everything downstream (settings, rendering,
//! caching) stays the same.

use lightcraft_raster::blur::gaussian;
use lightcraft_raster::Plane;

/// Detail-band radius (relative to the long edge) the sharpness cue is read at.
const DETAIL_SIGMA: f32 = 0.0035;
/// How much the smoothed depth field is blurred, so the blur transitions don't follow fine
/// texture pixel for pixel (relative to the long edge).
const DEPTH_SMOOTH: f32 = 0.035;
/// Weight of the ground-plane position cue against the sharpness cue, 0..1.
const POSITION_WEIGHT: f32 = 0.25;
/// Width of the ramp from "in focus" to "fully blurred" beyond `range`, in the same 0..1 depth
/// units as `focal`/`range`.
const TRANSITION: f32 = 0.18;

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A 0 (near / already sharp) .. 1 (far / already soft) depth estimate for `log_l` (log2
/// luminance relative to middle grey, as [`crate::Prepared::log_l`] holds it), at its own
/// resolution. See the module docs for the two cues it blends.
pub fn depth_map(log_l: &Plane, px_per_long: f64) -> Plane {
    if log_l.data.is_empty() {
        return Plane { width: log_l.width, height: log_l.height, data: vec![] };
    }
    let (w, h) = (log_l.width.max(1), log_l.height.max(1));
    let ppl = px_per_long as f32;
    let detail_sigma = (DETAIL_SIGMA * ppl).max(0.6);
    let blurred = gaussian(log_l, detail_sigma);
    let mut detail: Vec<f32> = log_l.data.iter().zip(&blurred.data).map(|(a, b)| (a - b).abs()).collect();

    // Normalize by a high percentile so a handful of very sharp pixels (eyes, edges) don't wash
    // out the rest of the subject.
    let mut sorted = detail.clone();
    let k = (((sorted.len() as f32) * 0.98) as usize).min(sorted.len().saturating_sub(1));
    let (_, nth, _) = sorted.select_nth_unstable_by(k, f32::total_cmp);
    let norm = (*nth).max(1e-4);
    for v in &mut detail {
        *v = (*v / norm).clamp(0.0, 1.0);
    }

    let mut data = vec![0f32; w * h];
    for y in 0..h {
        let pos = y as f32 / (h.max(2) - 1) as f32; // 0 = top, 1 = bottom
        // depth contribution of each cue, low = near: a sharp pixel or one low in the frame
        // reads as near, so both go in as (1 - cue).
        let depth_from_pos = 1.0 - pos;
        for x in 0..w {
            let i = y * w + x;
            let depth_from_sharp = 1.0 - detail[i];
            data[i] = (POSITION_WEIGHT * depth_from_pos + (1.0 - POSITION_WEIGHT) * depth_from_sharp).clamp(0.0, 1.0);
        }
    }
    gaussian(&Plane { width: w, height: h, data }, (DEPTH_SMOOTH * ppl).max(1.0))
}

/// Gaussian sigma (px) of the fully-blurred layer at `amount` (0..100).
pub fn blur_sigma(amount: f64, px_per_long: f64) -> f32 {
    0.05 * (amount / 100.0).clamp(0.0, 1.0) as f32 * px_per_long as f32
}

/// How much of the blurred layer to mix in at a pixel of depth `depth` (0..1), given the focal
/// plane `focal` and in-focus half-width `range` (both 0..1): 0 inside the focal band, ramping to
/// 1 over [`TRANSITION`] beyond it.
pub fn blend_weight(depth: f32, focal: f32, range: f32) -> f32 {
    let dist = (depth - focal).abs();
    smoothstep(range, range + TRANSITION, dist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_raster::Plane;

    /// A `w × h` plane: sharp (alternating) texture in its left half, already smooth (a ramp) in
    /// its right half.
    fn half_sharp_half_smooth(w: usize, h: usize) -> Plane {
        let data = (0..w * h)
            .map(|i| {
                let x = i % w;
                if x < w / 2 {
                    if x % 2 == 0 { 0.4 } else { -0.4 }
                } else {
                    (x as f32 / w as f32) * 0.3
                }
            })
            .collect();
        Plane { width: w, height: h, data }
    }

    #[test]
    fn sharp_half_reads_nearer_than_soft_half() {
        let p = half_sharp_half_smooth(64, 64);
        let d = depth_map(&p, 64.0);
        let left: f32 = (0..64).map(|y| d.data[y * 64 + 8]).sum::<f32>() / 64.0;
        let right: f32 = (0..64).map(|y| d.data[y * 64 + 56]).sum::<f32>() / 64.0;
        assert!(left < right, "sharp half should read nearer (lower depth) than the smooth half: {left} vs {right}");
    }

    #[test]
    fn depth_stays_in_unit_range() {
        let p = half_sharp_half_smooth(32, 32);
        let d = depth_map(&p, 32.0);
        assert!(d.data.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn empty_plane_does_not_panic() {
        let p = Plane { width: 0, height: 0, data: vec![] };
        let d = depth_map(&p, 100.0);
        assert_eq!(d.data.len(), 0);
    }

    #[test]
    fn blend_weight_is_zero_inside_the_focal_band() {
        assert_eq!(blend_weight(0.5, 0.5, 0.1), 0.0);
        assert_eq!(blend_weight(0.55, 0.5, 0.1), 0.0);
    }

    #[test]
    fn blend_weight_rises_with_distance_from_focus() {
        let near = blend_weight(0.3, 0.5, 0.05);
        let far = blend_weight(0.95, 0.5, 0.05);
        assert!(near < far);
        assert!(far > 0.9);
    }

    #[test]
    fn blur_sigma_is_zero_at_zero_amount() {
        assert_eq!(blur_sigma(0.0, 1000.0), 0.0);
    }

    #[test]
    fn blur_sigma_grows_with_amount() {
        assert!(blur_sigma(100.0, 1000.0) > blur_sigma(10.0, 1000.0));
    }
}
