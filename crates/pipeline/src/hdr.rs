//! HDR editing ([`Light::hdr`](lightcraft_develop::Light)): the tone map lets tones rise above SDR
//! white (display-linear 1.0) up to [`HDR_PEAK`] instead of rolling off at it, and the per-pixel
//! stage keeps that headroom through colour, gamut mapping and encoding.
//!
//! The HDR values are what an HDR display or export shows. Until those exist, renders (previews
//! and exports alike, so they keep matching) show the HDR result on SDR through [`to_sdr`]; the
//! histogram ([`lightcraft_raster::Histogram::of_hdr`]) shows the whole HDR range.

/// Stops of headroom above SDR white an HDR edit can use (Lightroom's HDR range: +4 stops).
pub const HDR_STOPS: f32 = 4.0;
/// Display-linear peak of an HDR edit (SDR white = 1): `2^HDR_STOPS`.
pub const HDR_PEAK: f32 = 16.0;

/// Display-linear peak of a render under `s`: in HDR mode `2^hdr_max` (the HDR headroom limit,
/// 1..[`HDR_STOPS`] stops; up to [`HDR_PEAK`]), else 1 (SDR white).
pub fn peak(s: &lightcraft_develop::DevelopSettings) -> f32 {
    if !s.light.hdr {
        return 1.0;
    }
    let stops = s.light.hdr_max as f32;
    let stops = if stops.is_finite() { stops.clamp(1.0, HDR_STOPS) } else { HDR_STOPS };
    stops.exp2()
}

/// Visualize HDR range colours, one per stop above SDR white (sRGB 8-bit): +1 cyan, +2 blue,
/// +3 violet, +4 purple. The histogram's colour bar uses the same ones.
pub const RANGE_COLORS: [[u8; 3]; 4] = [[135, 226, 240], [68, 85, 205], [76, 29, 195], [155, 45, 200]];

/// The [`RANGE_COLORS`] band of a tone `stops` above SDR white (0..[`HDR_STOPS`]; outside: the
/// nearest band), sRGB-encoded 0..1.
pub fn range_color(stops: f32) -> [f32; 3] {
    let band = if stops.is_finite() { stops.ceil().clamp(1.0, RANGE_COLORS.len() as f32) as usize - 1 } else { 0 };
    RANGE_COLORS.get(band).copied().unwrap_or(RANGE_COLORS[0]).map(|c| c as f32 / 255.0)
}

/// Visualize HDR range of one display-linear colour (1 = SDR white): a tone above SDR white
/// gets its [`range_color`] (sRGB-encoded 0..1); an SDR tone `None` (it keeps its own colour).
pub fn visualize(c: [f32; 3]) -> Option<[f32; 3]> {
    let m = c.iter().copied().filter(|v| v.is_finite()).fold(0.0f32, f32::max);
    (m > 1.0).then(|| range_color(m.log2()))
}

/// Fraction of `peak` below which [`soft_peak`] is the identity.
const KNEE: f32 = 0.75;

/// Identity up to `KNEE · peak`, then a smooth shoulder (slope 1 at the knee) approaching `peak`.
#[inline]
pub fn soft_peak(o: f32, peak: f32) -> f32 {
    let k = KNEE * peak;
    if o.is_nan() {
        return 0.0;
    }
    if o <= k {
        return o;
    }
    let room = peak - k;
    k + room * (1.0 - (-(o - k) / room).exp())
}

/// Where [`to_sdr`] starts compressing (display-linear, of the largest channel).
const SDR_KNEE: f32 = 0.8;

/// The SDR view of an HDR display-linear colour (1 = SDR white): colours whose largest channel is
/// below `SDR_KNEE` are kept; brighter ones are scaled (hue and saturation kept) so the largest
/// channel rolls off smoothly towards SDR white (`HDR_PEAK` lands just below it).
#[inline]
pub fn to_sdr(c: [f32; 3]) -> [f32; 3] {
    let c = c.map(|v| {
        if v.is_finite() {
            v.max(0.0)
        } else if v > 0.0 {
            HDR_PEAK
        } else {
            0.0
        }
    });
    let m = c[0].max(c[1]).max(c[2]);
    if m <= SDR_KNEE {
        return c;
    }
    let x = (m - SDR_KNEE) / (1.0 - SDR_KNEE);
    let mapped = SDR_KNEE + (1.0 - SDR_KNEE) * x / (1.0 + x);
    c.map(|v| v * mapped / m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_peak_is_identity_below_the_knee_and_bounded() {
        assert_eq!(soft_peak(0.5, 1.0), 0.5);
        assert_eq!(soft_peak(10.0, HDR_PEAK), 10.0);
        let mut prev = 0.0;
        for i in 0..4000 {
            let o = i as f32 * 0.02;
            let v = soft_peak(o, HDR_PEAK);
            assert!(v >= prev && v <= HDR_PEAK, "{o}: {v}");
            prev = v;
        }
        assert_eq!(soft_peak(f32::NAN, HDR_PEAK), 0.0);
        assert_eq!(soft_peak(f32::INFINITY, HDR_PEAK), HDR_PEAK);
    }

    #[test]
    fn peak_follows_the_headroom_limit() {
        let mut s = lightcraft_develop::DevelopSettings::default();
        assert_eq!(peak(&s), 1.0, "SDR edits");
        s.light.hdr = true;
        assert_eq!(peak(&s), HDR_PEAK, "the default limit is the whole range");
        s.light.hdr_max = 2.0;
        assert_eq!(peak(&s), 4.0);
        for (v, want) in [(0.0, 2.0), (9.0, HDR_PEAK), (f64::NAN, HDR_PEAK), (f64::NEG_INFINITY, HDR_PEAK)] {
            s.light.hdr_max = v;
            assert_eq!(peak(&s), want, "{v}");
        }
    }

    #[test]
    fn visualize_colours_hdr_tones_by_stops_and_keeps_sdr_ones() {
        assert_eq!(visualize([0.5, 0.2, 0.9]), None, "SDR tones keep their colour");
        assert_eq!(visualize([1.0; 3]), None, "SDR white is SDR");
        let band = |i: usize| Some(RANGE_COLORS[i].map(|c| c as f32 / 255.0));
        assert_eq!(visualize([1.2; 3]), band(0), "within a stop of white: the first band");
        assert_eq!(visualize([3.0; 3]), band(1));
        assert_eq!(visualize([7.0; 3]), band(2));
        assert_eq!(visualize([15.0; 3]), band(3), "near the peak: the last band");
        assert_eq!(visualize([f32::NAN, 2.0, 0.0]), Some(range_color(1.0)));
        assert_eq!(visualize([f32::NAN; 3]), None);
        assert_eq!(range_color(f32::NAN), range_color(0.0));
        assert_eq!(range_color(99.0), range_color(HDR_STOPS));
    }

    #[test]
    fn to_sdr_keeps_sdr_tones_and_fits_hdr_ones() {
        assert_eq!(to_sdr([0.2, 0.5, 0.7]), [0.2, 0.5, 0.7]);
        let mut prev = 0.0;
        for i in 0..=1600 {
            let v = i as f32 / 100.0;
            let o = to_sdr([v; 3])[0];
            assert!(o >= prev && o <= 1.0, "{v}: {o}");
            prev = o;
        }
        assert!(to_sdr([HDR_PEAK; 3])[0] > 0.98);
        // hue kept: channel ratios survive
        let c = to_sdr([8.0, 4.0, 2.0]);
        assert!((c[0] / c[1] - 2.0).abs() < 1e-5 && (c[1] / c[2] - 2.0).abs() < 1e-5);
        assert_eq!(to_sdr([f32::NAN, f32::NEG_INFINITY, -1.0]), [0.0; 3]);
        assert!(to_sdr([f32::INFINITY, 0.0, 0.0])[0] <= 1.0);
    }
}
