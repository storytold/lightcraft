//! What the conversion needs to know about one negative, and how to measure it from pixels
//! without an allocator (a fixed-size histogram per channel).

use crate::{RANGE_STOPS, color::Encoding, film};

/// Film base and levels, per channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Analysis {
    /// The film base (unexposed film, the border) in linear light: scene black.
    pub base: [f32; 3],
    /// Relative exposure ([`film::exposure`]) that maps to black.
    pub black: [f32; 3],
    /// Relative exposure that maps to white.
    pub white: [f32; 3],
}

impl Analysis {
    /// Levels from the base alone: the base is black and a scene [`RANGE_STOPS`] stops brighter
    /// is white, on every channel. Needs nothing from the image, so it is identical on every band.
    pub fn manual(base_linear: [f32; 3]) -> Self {
        Self::from_base(base_linear, [crate::NEGATIVE_GAMMA; 3])
    }

    /// [`Analysis::manual`] for a film whose layers have the given density per log-exposure: a
    /// steeper layer reaches the same scene brightness at a higher density, so its white point
    /// sits further from the base. Relative exposures stay in the [`film::exposure`] convention.
    pub fn from_base(base_linear: [f32; 3], gamma: [f32; 3]) -> Self {
        let mut white = [0.0; 3];
        for (w, g) in white.iter_mut().zip(gamma) {
            let g = if g.is_finite() && g > 0.05 { g } else { crate::NEGATIVE_GAMMA };
            *w = libm::exp2f(g / crate::NEGATIVE_GAMMA * RANGE_STOPS);
        }
        Self { base: base_linear, black: [1.0; 3], white }
    }

    /// Levels from three scan values per channel in the host encoding: the film base, the
    /// brightest value that still counts as exposed (black) and the darkest (white). `None`
    /// when a channel has no range between black and white.
    pub fn from_scan_values(base: [f32; 3], black: [f32; 3], white: [f32; 3], enc: Encoding) -> Option<Self> {
        let mut a = Self { base: [0.0; 3], black: [0.0; 3], white: [0.0; 3] };
        for ch in 0..3 {
            a.base[ch] = enc.decode(base[ch]);
            a.black[ch] = film::exposure(enc.decode(black[ch]), a.base[ch]);
            a.white[ch] = film::exposure(enc.decode(white[ch]), a.base[ch]);
            if !a.white[ch].is_finite() || !a.black[ch].is_finite() || a.white[ch] <= a.black[ch] * 1.0001 {
                return None;
            }
        }
        Some(a)
    }
}

/// Bins per channel.
pub const BINS: usize = 1024;

/// Per-channel histogram of scan values in the host encoding, 0..1. One pass over a band gives
/// the film base (a high quantile: the unexposed border is the brightest the scan gets) and the
/// black and white points (the quantiles the black and white clips leave).
#[derive(Clone, Copy, Debug)]
pub struct ValueHistogram {
    bins: [[u32; BINS]; 3],
    count: u32,
}

impl Default for ValueHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl ValueHistogram {
    pub const fn new() -> Self {
        Self { bins: [[0; BINS]; 3], count: 0 }
    }

    /// Account for one sample (host encoding, 0..1 per channel).
    pub fn push(&mut self, rgb: [f32; 3]) {
        for (bins, v) in self.bins.iter_mut().zip(rgb) {
            if let Some(b) = bins.get_mut(bin(v)) {
                *b = b.saturating_add(1);
            }
        }
        self.count = self.count.saturating_add(1);
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    /// The value below which a fraction `q` (0..1) of channel `ch` lies.
    pub fn quantile(&self, ch: usize, q: f32) -> Option<f32> {
        self.bins.get(ch).and_then(|bins| quantile_bin(bins, self.count, q)).map(|b| b as f32 / (BINS - 1) as f32)
    }

    /// The analysis of the pixels seen: the base at the `1 − base_clip` quantile, black at
    /// `1 − black_clip`, white at `white_clip` (all fractions, 0..1), for samples in `enc`.
    pub fn analysis(&self, base_clip: f32, black_clip: f32, white_clip: f32, enc: Encoding) -> Option<Analysis> {
        let (mut base, mut black, mut white) = ([0.0; 3], [0.0; 3], [0.0; 3]);
        for ch in 0..3 {
            base[ch] = self.quantile(ch, 1.0 - base_clip.clamp(0.0, 0.5))?;
            black[ch] = self.quantile(ch, 1.0 - black_clip.clamp(0.0, 0.5))?;
            white[ch] = self.quantile(ch, white_clip.clamp(0.0, 0.5))?;
        }
        Analysis::from_scan_values(base, black, white, enc)
    }
}

/// The bin of a value in 0..1.
fn bin(v: f32) -> usize {
    if v.is_finite() { (v.clamp(0.0, 1.0) * (BINS - 1) as f32) as usize } else { 0 }
}

/// The first bin at which the cumulative count reaches `q` of `count` (`None` when empty).
fn quantile_bin(bins: &[u32; BINS], count: u32, q: f32) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let target = libm::ceil(f64::from(q.clamp(0.0, 1.0)) * f64::from(count)).max(1.0) as u64;
    let mut seen = 0u64;
    for (i, n) in bins.iter().enumerate() {
        seen += u64::from(*n);
        if seen >= target {
            return Some(i);
        }
    }
    Some(BINS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color;

    const BASE: [f32; 3] = [0.6, 0.35, 0.15];

    #[test]
    fn histogram_finds_base_and_levels_of_a_synthetic_negative() {
        let mut h = ValueHistogram::new();
        assert_eq!(h.analysis(0.001, 0.0, 0.0, Encoding::Srgb), None);
        // Scene exposures from 1 (black, at the base) to 2^5 (white), evenly on a log scale,
        // plus a hot pixel brighter than the base.
        for i in 0..=1000 {
            let e = libm::exp2f(5.0 * i as f32 / 1000.0);
            let mut rgb = [0.0; 3];
            for (v, b) in rgb.iter_mut().zip(BASE) {
                *v = color::srgb_encode(film::transmittance(e, b));
            }
            h.push(rgb);
        }
        h.push([1.0; 3]);
        assert_eq!(h.count(), 1002);
        let a = h.analysis(0.005, 0.0, 0.0, Encoding::Srgb).unwrap();
        for (ch, b) in BASE.iter().enumerate() {
            // The clip takes the top 0.5 % of a 5-stop log ramp: 0.025 stops under the base.
            assert!((a.base[ch] - b).abs() < 0.01, "{a:?}");
            assert!((a.black[ch] - 1.0).abs() < 0.03, "{a:?}");
            assert!((a.white[ch] - 32.0).abs() / 32.0 < 0.05, "{a:?}");
        }
        let clipped = h.analysis(0.005, 0.1, 0.1, Encoding::Srgb).unwrap();
        assert!(clipped.black[0] > a.black[0] * 1.2 && clipped.white[0] < a.white[0] * 0.8, "{clipped:?}");
        // A flat band (all one value) has no range.
        let mut flat = ValueHistogram::new();
        flat.push([0.3; 3]);
        assert_eq!(flat.analysis(0.0, 0.0, 0.0, Encoding::Srgb), None);
    }

    #[test]
    fn a_steeper_layer_puts_white_further_from_the_base() {
        let a = Analysis::from_base(BASE, [0.6, 0.6, 0.72]);
        assert_eq!(a.white[0], a.white[1]);
        assert!(a.white[2] > a.white[0] * 1.5, "{a:?}");
        assert_eq!(Analysis::from_base(BASE, [f32::NAN, 0.0, 0.6]).white, Analysis::manual(BASE).white);
    }

    #[test]
    fn scan_values_give_levels_in_linear_too() {
        let a = Analysis::from_scan_values(BASE, BASE, [0.1, 0.1, 0.1], Encoding::Linear).unwrap();
        assert_eq!(a.base, BASE);
        assert_eq!(a.black, [1.0; 3]);
        assert!(a.white[0] > a.white[2], "the same scan value is further below a brighter base: {a:?}");
        assert_eq!(Analysis::from_scan_values(BASE, [0.1; 3], [0.1; 3], Encoding::Linear), None);
    }

    #[test]
    fn quantile_edges() {
        let mut bins = [0u32; BINS];
        bins[10] = 5;
        bins[500] = 5;
        assert_eq!(quantile_bin(&bins, 10, 0.0), Some(10));
        assert_eq!(quantile_bin(&bins, 10, 0.5), Some(10));
        assert_eq!(quantile_bin(&bins, 10, 0.51), Some(500));
        assert_eq!(quantile_bin(&bins, 10, 1.0), Some(500));
        assert_eq!(quantile_bin(&bins, 0, 0.5), None);
        assert_eq!(bin(f32::NAN), 0);
        assert_eq!(bin(1.0), BINS - 1);
        let mut h = ValueHistogram::new();
        h.push([f32::NAN, 2.0, -1.0]);
        assert_eq!(h.quantile(0, 0.5), Some(0.0));
        assert_eq!(h.quantile(1, 0.5), Some(1.0));
        assert_eq!(h.quantile(3, 0.5), None);
    }
}
