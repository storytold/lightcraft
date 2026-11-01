//! What belongs to the scan rather than the film, measured once on the whole image: the film base
//! and each channel's black and white point, and a mid-tone balance that makes the scene average
//! grey. A host stores the result with the photo's settings, so every render of it (a draft, a
//! zoomed window, a full-size export) converts with the same numbers.

use crate::{Analysis, Converter, Encoding, Params, Tone, ValueHistogram, color};

/// How a scan is measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureOptions {
    /// Fraction of each edge left out (film holder, sprocket holes, the border itself), 0..0.45.
    pub border: f32,
    /// Share of the brightest samples that may lie above the film base (dust, hot pixels).
    pub base_clip: f32,
    /// Shares of samples allowed beyond black and white.
    pub black_clip: f32,
    pub white_clip: f32,
    /// Bring each channel's median to the common one with a per-channel gamma (a grey scene
    /// average). Off: the black and white points are neutral and the scene keeps its own colour.
    pub neutral: bool,
    /// At most this many pixels are sampled (a regular grid over the image).
    pub max_samples: usize,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self { border: 0.08, base_clip: 0.001, black_clip: 0.003, white_clip: 0.003, neutral: true, max_samples: 1_000_000 }
    }
}

/// A measured scan: scan values in linear light, 0..1, per channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    /// The film base: the unexposed film, scene black.
    pub base: [f32; 3],
    /// The brightest value that still counts as exposed (the positive's black).
    pub black: [f32; 3],
    /// The densest value (the positive's white).
    pub white: [f32; 3],
    /// Per-channel mid-tone gammas that neutralise the scene average (1 = none).
    pub balance: [f32; 3],
}

impl Measurement {
    /// The levels of this measurement for linear samples; with a `base` picked from the border,
    /// that base is black and the measured white point still applies. `None` when a channel has
    /// no range left between black and white.
    pub fn analysis(&self, base: Option<[f32; 3]>) -> Option<Analysis> {
        match base {
            Some(b) => Analysis::from_scan_values(b, b, self.white, Encoding::Linear),
            None => Analysis::from_scan_values(self.base, self.black, self.white, Encoding::Linear),
        }
    }
}

/// Measures a `w × h` scan of linear samples (row-major, 0..1 per channel).
pub fn measure(pixels: &[[f32; 3]], w: usize, h: usize, o: &MeasureOptions) -> Result<Measurement, String> {
    if pixels.len() < w.saturating_mul(h) {
        return Err("the image is smaller than its size says".into());
    }
    let border = if o.border.is_finite() { o.border.clamp(0.0, 0.45) } else { 0.0 };
    let (bx, by) = ((w as f32 * border) as usize, (h as f32 * border) as usize);
    let (x0, x1, y0, y1) = (bx, w.saturating_sub(bx), by, h.saturating_sub(by));
    if x1 <= x0 || y1 <= y0 {
        return Err("the border leaves no pixels to measure".into());
    }
    let area = (x1 - x0).saturating_mul(y1 - y0);
    let step = (libm::ceil(libm::sqrt(area as f64 / o.max_samples.max(1) as f64)) as usize).max(1);
    // The histogram is binned on the sRGB-encoded values, which spread the dense end (the
    // highlights, near 0 in linear light) over many more bins than a linear scale would.
    let mut hist = ValueHistogram::new();
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            if let Some(p) = pixels.get(y * w + x) {
                hist.push(Encoding::Srgb.encode3(*p));
            }
        }
    }
    if hist.count() == 0 {
        return Err("no pixels to measure".into());
    }
    let q = |ch: usize, frac: f32| hist.quantile(ch, frac).map(color::srgb_decode).ok_or_else(|| "empty histogram".to_string());
    let mut m = Measurement { base: [0.0; 3], black: [0.0; 3], white: [0.0; 3], balance: [1.0; 3] };
    let mut median = [0.0; 3];
    for (ch, med) in median.iter_mut().enumerate() {
        m.base[ch] = q(ch, 1.0 - o.base_clip.clamp(0.0, 0.5))?;
        m.black[ch] = q(ch, 1.0 - o.black_clip.clamp(0.0, 0.5))?;
        m.white[ch] = q(ch, o.white_clip.clamp(0.0, 0.5))?;
        *med = q(ch, 0.5)?;
        if m.white[ch] >= m.black[ch] {
            return Err("the scan has no tonal range between black and white (a flat image, or the border leaves too little)".into());
        }
    }
    if o.neutral {
        m.balance = neutral_balance(&m, median);
    }
    Ok(m)
}

/// Per-channel gammas that bring each channel's median positive to the geometric mean of the
/// three: the scene averages grey, black and white stay put.
fn neutral_balance(m: &Measurement, median: [f32; 3]) -> [f32; 3] {
    let Some(a) = m.analysis(None) else { return [1.0; 3] };
    let c = Converter::new(&a, &Params { tone: Tone::Linear, ..Params::default() }, Encoding::Linear, None);
    let out = c.convert(median).map(|v| v.clamp(1e-3, 0.999));
    let target = (libm::logf(out[0]) + libm::logf(out[1]) + libm::logf(out[2])) / 3.0;
    out.map(|v| {
        let b = target / libm::logf(v);
        if b.is_finite() { b.clamp(0.4, 2.5) } else { 1.0 }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RANGE_STOPS, film};

    const BASE: [f32; 3] = [0.6, 0.35, 0.15];

    /// A synthetic negative: a log ramp of scene exposures across the width, with a cast in the
    /// blue mid-tones the neutral balance should take out, framed by a border of film base.
    fn scan(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= w / 10 && x < w - w / 10 && y >= h / 10 && y < h - h / 10;
                let s = x as f32 / (w - 1) as f32;
                let e = libm::exp2f(s * RANGE_STOPS);
                px.push(if inside {
                    [film::transmittance(e, BASE[0]), film::transmittance(e, BASE[1]), film::transmittance(e, BASE[2]) * 0.8]
                } else {
                    BASE
                });
            }
        }
        px
    }

    #[test]
    fn measures_base_levels_and_balance_of_a_synthetic_negative() {
        let (w, h) = (300, 200);
        let px = scan(w, h);
        let m = measure(&px, w, h, &MeasureOptions { border: 0.12, ..MeasureOptions::default() }).unwrap();
        for ch in 0..3 {
            assert!(m.white[ch] < m.black[ch] && m.black[ch] <= m.base[ch] * 1.01, "{m:?}");
        }
        assert!(m.balance[2] != 1.0, "the blue cast gets a balance: {m:?}");
        // With the border in the measured area, the base is the border.
        let framed = measure(&px, w, h, &MeasureOptions { border: 0.0, ..MeasureOptions::default() }).unwrap();
        for ch in 0..3 {
            assert!((framed.base[ch] - BASE[ch]).abs() < 0.01, "{framed:?}");
        }
        let a = m.analysis(None).unwrap();
        assert!(a.white.iter().zip(a.black).all(|(w, b)| *w > b));
        // A picked base is black; the measured white point stays.
        let picked = m.analysis(Some(BASE)).unwrap();
        assert_eq!(picked.base, BASE);
        assert_eq!(picked.black, [1.0; 3]);
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(measure(&[], 0, 0, &MeasureOptions::default()).is_err());
        assert!(measure(&[[0.5; 3]; 4], 10, 10, &MeasureOptions::default()).is_err(), "short buffer");
        assert!(measure(&[[0.5; 3]; 100], 10, 10, &MeasureOptions::default()).is_err(), "flat image");
        let px = scan(40, 30);
        assert!(measure(&px, 40, 30, &MeasureOptions { border: f32::NAN, ..MeasureOptions::default() }).is_ok());
        // One sample has no range: an error too.
        assert!(measure(&px, 40, 30, &MeasureOptions { max_samples: 0, ..MeasureOptions::default() }).is_err());
        let nan = vec![[f32::NAN, f32::INFINITY, -1.0]; 100];
        assert!(measure(&nan, 10, 10, &MeasureOptions::default()).is_err());
    }
}
