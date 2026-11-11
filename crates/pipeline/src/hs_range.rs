//! Highlights relative to the photo's own tonal range (process V9, `docs/hs-range.md`).
//!
//! Up to V8 Highlights moves the base layer's log luminance by a fixed weight of its absolute level. Lightroom's acts
//! relative to the frame: on a photo with a wide tonal range it reaches further down and works harder. From V9
//! Highlights takes the frame's range from the base layer's percentiles ([`FrameRange`]). Shadows keeps V8's rule.

use lightcraft_develop::Process;

/// The base layer's tonal range in log₂ luminance (exposure included): its 2nd and 99th percentile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRange {
    pub lo: f32,
    pub hi: f32,
}

/// Long edge (px) of the whole-frame render the range is measured on, for every output size.
pub const PROXY_EDGE: usize = 384;
/// Percentiles of the base layer that span the frame's range.
const P_LO: f32 = 0.02;
const P_HI: f32 = 0.99;
/// Highlights: strength `H_AMP · (span / H_SPAN)^H_POW` (clamped to `H_AMP_RANGE`), reaching down to
/// `−1 − H_REACH · (span − H_SPAN)` stops.
pub const H_AMP: f32 = 0.7;
pub const H_SPAN: f32 = 5.0;
pub const H_POW: f32 = 2.4;
pub const H_REACH: f32 = 0.8;
pub const H_AMP_RANGE: (f32, f32) = (0.2, 8.0);

/// Whether `s` uses the frame's range: process V9 with Highlights, globally or in a visible mask.
pub fn wanted(s: &lightcraft_develop::DevelopSettings) -> bool {
    let v8 = match s.process.process() {
        Process::V1 | Process::V2 | Process::V3 | Process::V4 | Process::V5 | Process::V6 | Process::V7 | Process::V8 => false,
        Process::V9 => true,
    };
    v8 && (s.light.highlights != 0.0 || s.masks.iter().any(|m| m.visible && !m.components.is_empty() && m.adjust.highlights != 0.0))
}

impl FrameRange {
    /// The range after exposure `ev` (stops).
    pub fn shifted(self, ev: f32) -> FrameRange {
        FrameRange { lo: self.lo + ev, hi: self.hi + ev }
    }

    /// The range of base-layer values `base` (log₂ luminance before exposure `ev`), from every value (a proxy of
    /// [`PROXY_EDGE`] px holds about 100 000); `None` without a finite value.
    pub fn of(base: &[f32], ev: f32) -> Option<FrameRange> {
        let mut sample: Vec<f32> = base.iter().copied().filter(|v| v.is_finite()).collect();
        if sample.is_empty() {
            return None;
        }
        sample.sort_by(f32::total_cmp);
        let at = |q: f32| sample[((q * (sample.len() - 1) as f32) as usize).min(sample.len() - 1)] + ev;
        Some(FrameRange { lo: at(P_LO), hi: at(P_HI) })
    }

    /// The change in log₂ luminance that Highlights (−1..1) makes at base-layer value `base`.
    pub fn highlights(&self, highlights: f32, base: f32) -> f32 {
        if highlights == 0.0 {
            return 0.0;
        }
        let span = (self.hi - self.lo).max(0.1);
        let amp = (H_AMP * (span / H_SPAN).powf(H_POW)).clamp(H_AMP_RANGE.0, H_AMP_RANGE.1);
        let low = -1.0 - H_REACH * (span - H_SPAN).max(0.0);
        highlights * amp * smooth(low, 2.8, base)
    }
}

/// Hermite smooth step from `e0` to `e1`.
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::DevelopSettings;

    /// Settings on process `p` with Highlights at `h` (−100..100).
    fn on(p: Process, h: f64) -> DevelopSettings {
        let mut s = DevelopSettings { process: p.version(), ..Default::default() };
        s.light.highlights = h;
        s
    }

    #[test]
    fn only_v9_with_highlights_uses_the_range() {
        assert!(wanted(&on(Process::V9, -40.0)));
        assert!(!wanted(&on(Process::V9, 0.0)));
        assert!(!wanted(&on(Process::V8, -40.0)));
        assert!(!wanted(&on(Process::V7, -40.0)));
    }

    #[test]
    fn the_range_is_the_2nd_to_99th_percentile_and_exposure_shifts_it() {
        let base: Vec<f32> = (0..7000).map(|i| (i % 1000) as f32 / 100.0 - 5.0).collect();
        let r = FrameRange::of(&base, 0.0).unwrap();
        assert!((r.lo - -4.8).abs() < 0.05 && (r.hi - 4.9).abs() < 0.05, "{r:?}");
        assert_eq!(FrameRange::of(&base, 1.0).unwrap(), r.shifted(1.0));
        assert!(FrameRange::of(&[f32::NAN], 0.0).is_none());
    }

    #[test]
    fn a_thin_bright_stripe_does_not_flip_the_range_as_it_moves() {
        // a 350 px wide frame with a 3 px bright stripe (under 1 % of it), one column apart
        let frame = |x0: usize| -> Vec<f32> {
            (0..350 * 200).map(|i| if (x0..x0 + 3).contains(&(i % 350)) { 3.0 } else { (i % 350) as f32 / 350.0 * 4.0 - 4.0 }).collect()
        };
        let (a, b) = (FrameRange::of(&frame(140), 0.0).unwrap(), FrameRange::of(&frame(141), 0.0).unwrap());
        assert!((a.hi - b.hi).abs() < 1e-3 && (a.lo - b.lo).abs() < 1e-3, "{a:?} {b:?}");
    }

    #[test]
    fn a_wider_range_makes_highlights_stronger_and_reach_further_down() {
        let narrow = FrameRange { lo: -3.0, hi: 2.0 };
        let wide = FrameRange { lo: -6.0, hi: 2.0 };
        assert_eq!(narrow.highlights(0.0, 1.0), 0.0);
        // in the highlights
        assert!(wide.highlights(-1.0, 2.0) < narrow.highlights(-1.0, 2.0) && narrow.highlights(-1.0, 2.0) < 0.0);
        // below the narrow frame's reach (−1 stop), the wide frame's still acts
        assert_eq!(narrow.highlights(-1.0, -1.5), 0.0);
        assert!(wide.highlights(-1.0, -1.5) < 0.0);
        // a 5-stop frame at full Highlights is about V8's strength at the top (0.7 against 1.7: a gentler start)
        assert!((narrow.highlights(-1.0, 2.8) - -0.7).abs() < 1e-4);
    }
}
