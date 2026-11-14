//! Sensors that clip below the stated white level.
//!
//! A DNG's `WhiteLevel` is what the writer says the sample range tops out at, not always where the
//! sensor really saturates: a camera app can state 4095 for 12-bit data that piles up at 3567 (85 % of
//! the range above black). Highlight handling ([`crate::highlight`]) treats samples at the white level
//! as clipped; samples stuck 15 % below it look like real, bright, unclipped light, so white balance
//! multiplies each channel's plateau by a different factor and the highlights turn magenta or cyan.
//!
//! [`RawImage::lift_clipped_samples`] finds the real saturation point of each colour from the samples
//! (a pile-up of identical values near the top of the range) and raises the samples at or above it to
//! the stated white level. Nothing else changes: unclipped samples keep their values, and a file whose
//! data really reaches its white level, or has no pile-up, is left alone.

use crate::{RawData, RawImage};

/// A saturation plateau must hold at least this share of its colour's samples (and 100 of them)...
const MIN_SHARE: f64 = 1e-4;
/// ...and be at least this many times as populated as the average value just below it.
const MIN_SPIKE: f64 = 16.0;
/// Plateaus within this share of the range from the white level are the white level (no lift needed).
const NEAR_WHITE: f32 = 0.03;
/// ...and have next to nothing above it (a real clip point is the top of the data; a popular value in the
/// middle of sparse or curve-coded data has plenty of samples above): at most this share of the plateau
/// lies more than `ABOVE_MARGIN` of the range over it (a few counts of noise or gain spill past the plateau).
const MAX_ABOVE: f64 = 0.02;
const ABOVE_MARGIN: f32 = 0.01;
/// How far below the plateau the "average value just below it" reaches.
const NEIGHBOURHOOD: usize = 64;

// CR3 gain-normalized saturation can occupy several nearby codes, rather than one exact value.
const CR3_BINS: usize = 256;
const CR3_PLATEAU_BINS: usize = 8;
const CR3_MIN_SAMPLES: u64 = 64;

/// The lower edge of a terminal pile-up, ignoring a few brighter outliers. A continuous
/// highlight distribution has comparable density below it and therefore has no plateau.
fn cr3_plateau(hist: &[[u64; 3]; CR3_BINS], first: &[[u16; 3]; CR3_BINS], colour: usize, from: usize, white: f32) -> Option<f32> {
    let (start, end, count) = (from + 1..=CR3_BINS)
        .map(|end| {
            let start = end.saturating_sub(CR3_PLATEAU_BINS).max(from);
            let count: u64 = hist[start..end].iter().map(|bin| bin[colour]).sum();
            (start, end, count)
        })
        .max_by_key(|&(_, end, count)| (count, end))?;
    let below_start = start.saturating_sub(CR3_PLATEAU_BINS);
    let below: u64 = hist[below_start..start].iter().map(|bin| bin[colour]).sum();
    let above: u64 = hist[end..].iter().map(|bin| bin[colour]).sum();
    let density = count as f64 / (end - start) as f64;
    let below_density = below as f64 / (start - below_start).max(1) as f64;
    if count < CR3_MIN_SAMPLES || (below > 0 && density < MIN_SPIKE * below_density) || above as f64 > MAX_ABOVE * count as f64 {
        return None;
    }
    let threshold = (density / MIN_SPIKE).max(MIN_SPIKE * below_density).max(1.0);
    (start..end).find(|&bin| hist[bin][colour] as f64 >= threshold).map(|bin| first[bin][colour] as f32).filter(|&point| point > white)
}

impl RawImage {
    /// The value each colour's samples saturate at, when that is clearly below the stated white level
    /// (see the module docs). Only single-plane integer CFA data is examined.
    pub fn saturation_points(&self) -> [Option<u16>; 3] {
        let none = [None; 3];
        let (RawData::U16(d), Some(cfa)) = (&self.data, &self.cfa) else { return none };
        let white = self.white_at(0);
        let black = self.black.mean();
        // a hostile file can state any levels: NaN, negative or inverted ones are left alone
        let levels_ok = black.is_finite() && white.is_finite() && black >= 0.0 && white > black && white <= 65535.0;
        if self.cpp != 1 || !crate::cfa_valid(cfa) || self.width.checked_mul(self.height) != Some(d.len()) || !levels_ok {
            return none;
        }
        let a = self.active_area;
        let fits = |start: usize, len: usize, max: usize| start.checked_add(len).is_some_and(|end| end <= max);
        if !fits(a.x, a.width, self.width) || !fits(a.y, a.height, self.height) {
            return none;
        }
        let lo = (black + 0.5 * (white - black)).ceil().max(0.0) as usize;
        let hi = (white as usize).min(65535);
        if lo + NEIGHBOURHOOD >= hi {
            return none;
        }
        let mut hist = vec![[0u32; 3]; 65536];
        let mut total = [0u64; 3];
        for y in 0..a.height {
            let row = &d[(a.y + y) * self.width + a.x..][..a.width];
            for (x, &v) in row.iter().enumerate() {
                let c = cfa.color_at(a.x + x, a.y + y).min(2) as usize;
                total[c] += 1;
                hist[v as usize][c] += 1;
            }
        }
        let mut out = none;
        for c in 0..3 {
            // the most populated value of the upper half of the range
            let Some((v, n)) = (lo..hi).map(|v| (v, hist[v][c])).max_by_key(|&(v, n)| (n, v)) else { continue };
            if (n as f64) < (total[c] as f64 * MIN_SHARE).max(100.0) || v as f32 >= white - NEAR_WHITE * (white - black) {
                continue;
            }
            let below: u64 = (v.saturating_sub(NEIGHBOURHOOD)..v).map(|u| hist[u][c] as u64).sum();
            let from = (v + ((ABOVE_MARGIN * (white - black)) as usize).max(1) + 1).min(hist.len());
            let above: u64 = hist[from..].iter().map(|h| h[c] as u64).sum();
            if n as f64 >= MIN_SPIKE * (below as f64 / NEIGHBOURHOOD as f64).max(1.0) && above as f64 <= MAX_ABOVE * n as f64 {
                out[c] = Some(v as u16);
            }
        }
        out
    }

    /// Raise samples at or above their colour's real saturation point ([`Self::saturation_points`]) to the
    /// stated white level, so highlight reconstruction recognises them as clipped. Returns how many
    /// samples changed (0: the data is untouched).
    pub fn lift_clipped_samples(&mut self) -> usize {
        let points = self.saturation_points();
        if points.iter().all(Option::is_none) {
            return 0;
        }
        let (a, w, white) = (self.active_area, self.width, self.white_at(0).round() as u16);
        let Some(cfa) = self.cfa.clone() else { return 0 };
        let RawData::U16(d) = &mut self.data else { return 0 };
        let mut changed = 0;
        for y in 0..a.height {
            let row = &mut d[(a.y + y) * w + a.x..][..a.width];
            for (x, v) in row.iter_mut().enumerate() {
                let c = cfa.color_at(a.x + x, a.y + y).min(2) as usize;
                if let Some(p) = points[c]
                    && *v >= p
                    && *v < white
                {
                    *v = white;
                    changed += 1;
                }
            }
        }
        changed
    }
    /// Recover real CR3 samples beyond a conservatively stated common white level. Search
    /// coarse bins so gain-spread saturation (e.g. R100) is found too. With enough above-white
    /// data but no clear pile-up, the bit-depth ceiling keeps all that unclipped headroom.
    /// One shared range and its compensating baseline EV preserve lower tones and colour ratios.
    pub(crate) fn extend_cr3_white(&mut self) -> bool {
        if self.format != crate::RawFormat::Cr3 || !self.color.baseline_exposure.is_finite() || !(1..=16).contains(&self.bits) {
            return false;
        }
        let (RawData::U16(data), Some(cfa)) = (&self.data, &self.cfa) else { return false };
        let (old, black) = (self.white_at(0), self.black.mean());
        let ceiling = ((1u32 << self.bits) - 1) as f32;
        if self.cpp != 1
            || !crate::cfa_valid(cfa)
            || self.width.checked_mul(self.height) != Some(data.len())
            || !black.is_finite()
            || !old.is_finite()
            || black < 0.0
            || old <= black
            || old >= ceiling
            || self.white.is_empty()
            || self.white.iter().any(|w| *w != old)
        {
            return false;
        }
        let a = self.active_area;
        let fits = |start: usize, len: usize, max: usize| start.checked_add(len).is_some_and(|end| end <= max);
        if a.width == 0 || a.height == 0 || !fits(a.x, a.width, self.width) || !fits(a.y, a.height, self.height) {
            return false;
        }
        let step = ((ceiling - black) / CR3_BINS as f32).ceil().max(1.0);
        let mut hist = [[0u64; 3]; CR3_BINS];
        let mut first = [[u16::MAX; 3]; CR3_BINS];
        let mut over = [0u64; 3];
        for y in a.y..a.y + a.height {
            for (x, &v) in data[y * self.width + a.x..][..a.width].iter().enumerate() {
                if v as f32 > ceiling {
                    return false;
                }
                let c = cfa.color_at(a.x + x, y).min(2) as usize;
                let bin = (((v as f32 - black).max(0.0) / step) as usize).min(CR3_BINS - 1);
                hist[bin][c] += 1;
                first[bin][c] = first[bin][c].min(v);
                over[c] += u64::from(v as f32 > old);
            }
        }
        let above: u64 = over.iter().sum();
        let pixels = a.width * a.height;
        if above < CR3_MIN_SAMPLES || (above as f64) < pixels as f64 * 1e-5 {
            return false;
        }
        let from = (((old - black) / step) as usize).min(CR3_BINS - 1);
        let new = (0..3)
            .map(|c| if over[c] < CR3_MIN_SAMPLES { ceiling } else { cr3_plateau(&hist, &first, c, from, old).unwrap_or(ceiling) })
            .fold(ceiling, f32::min);
        let ev = ((new - black) as f64 / (old - black) as f64).log2();
        if new <= old || !ev.is_finite() {
            return false;
        }
        self.white.fill(new);
        self.color.baseline_exposure += ev;
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    /// A 64 x 64 RGGB mosaic with a gradient, black 528, whose samples above `clip` (when given) are
    /// stuck at `clip` like a sensor that saturates early.
    fn raw_with(clip: Option<u16>, white: f32) -> RawImage {
        let data = (0..64 * 64)
            .map(|i| {
                let v = 528 + ((i % 64) * 60 + (i / 64) * 7) as u16 % 4000;
                clip.map_or(v, |c| v.min(c))
            })
            .collect();
        RawImage {
            format: RawFormat::Dng,
            width: 64,
            height: 64,
            cpp: 1,
            data: RawData::U16(data),
            cfa: Cfa::bayer("RGGB"),
            bits: 12,
            black: BlackLevel::uniform(528.0),
            white: vec![white],
            active_area: Rect::new(0, 0, 64, 64),
            crop: Rect::new(0, 0, 64, 64),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Metadata::default(),
            lens_metadata: Default::default(),
        }
    }

    /// The gradient of `raw_with`, wrapped so it never gets near the top of the range.
    fn without_top(r: &mut RawImage) {
        if let RawData::U16(d) = &mut r.data {
            for v in d.iter_mut() {
                *v = 528 + (*v - 528) % 3000;
            }
        }
    }

    #[test]
    fn hostile_levels_are_left_alone() {
        // a negative or NaN black level used to let the search start below NEIGHBOURHOOD and underflow
        for black in [-5000.0, f32::NAN] {
            let mut r = raw_with(Some(40), 4095.0);
            r.black = BlackLevel::uniform(black);
            assert_eq!(r.saturation_points(), [None; 3]);
            assert_eq!(r.lift_clipped_samples(), 0);
        }
        // an active area that runs past the image (or overflows when added up) is refused
        let mut r = raw_with(Some(3567), 4095.0);
        r.active_area = Rect::new(usize::MAX, 0, 2, 64);
        assert_eq!(r.saturation_points(), [None; 3]);
    }

    #[test]
    fn a_pile_up_below_the_white_level_is_found_and_lifted() {
        let mut r = raw_with(Some(3567), 4095.0);
        assert!(r.saturation_points().iter().all(|p| *p == Some(3567)), "{:?}", r.saturation_points());
        let before = r.data.clone();
        assert!(r.lift_clipped_samples() > 100);
        let (RawData::U16(b), RawData::U16(a)) = (&before, &r.data) else { return };
        for (x, y) in b.iter().zip(a) {
            // plateau samples go to the white level, everything else is untouched
            assert_eq!(*y, if *x >= 3567 { 4095 } else { *x });
        }
        assert_eq!(r.lift_clipped_samples(), 0, "idempotent");
    }

    #[test]
    fn data_that_reaches_its_white_level_is_left_alone() {
        let mut r = raw_with(Some(4095), 4095.0);
        let before = r.data.clone();
        assert_eq!(r.saturation_points(), [None; 3]);
        assert_eq!(r.lift_clipped_samples(), 0);
        assert_eq!(r.data, before);
    }

    #[test]
    fn data_without_a_pile_up_is_left_alone() {
        let mut r = raw_with(None, 4095.0);
        without_top(&mut r);
        let before = r.data.clone();
        assert_eq!(r.saturation_points(), [None; 3]);
        assert_eq!(r.lift_clipped_samples(), 0);
        assert_eq!(r.data, before);
    }

    /// Sparse or curve-coded data has popular values with plenty of samples above them: not a clip point.
    #[test]
    fn a_popular_value_with_data_above_it_is_not_a_clip_point() {
        let mut r = raw_with(None, 4095.0);
        without_top(&mut r);
        if let RawData::U16(d) = &mut r.data {
            for (i, v) in d.iter_mut().enumerate() {
                // a third of the samples sit on one code in the upper half, a few hundred are brighter
                *v = if i % 3 == 0 {
                    3000
                } else if i % 11 == 0 {
                    3000 + (i % 700) as u16 + 1
                } else {
                    *v
                };
            }
        }
        assert_eq!(r.saturation_points(), [None; 3]);
        assert_eq!(r.lift_clipped_samples(), 0);
    }

    #[test]
    fn a_few_stuck_samples_are_not_a_plateau() {
        let mut r = raw_with(None, 4095.0);
        without_top(&mut r);
        if let RawData::U16(d) = &mut r.data {
            for v in d.iter_mut().take(20) {
                *v = 3800;
            }
        }
        assert_eq!(r.saturation_points(), [None; 3]);
    }

    /// The decoder applies it: a DNG that states 4095 but saturates at 3567 comes back with the plateau at 4095.
    #[test]
    fn decoding_a_dng_lifts_the_plateau() {
        let r = raw_with(Some(3567), 4095.0);
        let opts = DngWriteOptions { compression: DngCompression::Uncompressed, ..Default::default() };
        let bytes = write_dng(&r, &opts).unwrap_or_default();
        let back = decode(&bytes).map(|b| b.data);
        let (Ok(RawData::U16(d)), RawData::U16(orig)) = (back, &r.data) else { panic!("decode failed") };
        assert!(orig.contains(&3567));
        assert!(d.iter().all(|&v| v != 3567));
        assert_eq!(d.iter().filter(|&&v| v == 4095).count(), orig.iter().filter(|&&v| v == 3567).count());
    }

    /// The reported bug end to end: a white wall that clips at 3567 under a stated 4095, with the camera's
    /// white balance (red x2.46, blue x1.59), must come out neutral after highlight reconstruction.
    #[test]
    fn early_clipping_renders_neutral_after_white_balance() {
        let wb = [2.46f32, 1.0, 1.59];
        let mut r = raw_with(None, 4095.0);
        let cfa = r.cfa.clone().unwrap_or_else(Cfa::xtrans);
        // a wall far brighter than any channel's clip point
        let wall = Rgb32f::from_fn(64, 64, |_, _| [4.0, 4.0, 4.0]);
        let n = demosaic::mosaic_from_rgb(&wall, &cfa);
        r.data = RawData::U16(n.data.iter().map(|&v| (528.0 + v * 3039.0).min(3567.0) as u16).collect());
        let render = |r: &RawImage| {
            let mut img = r.develop(Method::Bilinear).unwrap_or_else(|_| Rgb32f::new(1, 1));
            highlight::reconstruct(&mut img, wb, 0.99);
            let p = img.get(32, 32);
            [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]]
        };
        let tint = |p: [f32; 3]| (p[0] / p[1] - 1.0).abs().max((p[2] / p[1] - 1.0).abs());
        let before = render(&r);
        assert!(tint(before) > 0.3, "the plateau is not recognised as clipped: {before:?}");
        assert!(r.lift_clipped_samples() > 1000);
        let after = render(&r);
        assert!(tint(after) < 0.01, "{after:?}");
    }

    /// Canon states a white level below the sensor's real saturation (EOS RP: 14558 stated, 16383
    /// real). Samples pile up at the real saturation above the stated white; the white level must be
    /// extended to the saturation point so highlight reconstruction does not treat real highlight data
    /// between the two as clipped. The plateau above the stated white is currently outside the search
    /// range (which ends at the white level), so it is never found and the white level is never extended.
    #[test]
    fn extend_white_to_saturation_above_stated_white() {
        let mut r = raw_with(None, 14558.0);
        if let RawData::U16(d) = &mut r.data {
            for v in d.iter_mut().take(2048) {
                *v = 16383;
            }
        }
        r.format = RawFormat::Cr3;
        r.bits = 14;
        r.color.baseline_exposure = 0.5;
        let original_gain = 2f64.powf(r.color.baseline_exposure) as f32;
        let before = r.normalized().unwrap();
        let samples = r.data.clone();
        assert!(r.extend_cr3_white());
        let after = r.normalized().unwrap();
        let gain = 2f64.powf(r.color.baseline_exposure) as f32;
        for (a, b) in before.data.iter().zip(&after.data) {
            assert!((a * original_gain - b * gain).abs() < 2e-6, "unclipped exposure changed");
        }
        assert_eq!(r.data, samples);
        let ev = r.color.baseline_exposure;
        assert!(!r.extend_cr3_white(), "idempotent");
        assert_eq!(r.color.baseline_exposure, ev);
        assert_eq!(r.white_at(0), 16383.0);
    }
    #[test]
    fn cr3_headroom_does_not_change_other_formats_or_unproven_plateaus() {
        let mut r = raw_with(None, 14558.0);
        if let RawData::U16(d) = &mut r.data {
            d[..2048].fill(16383);
        }
        assert!(!r.extend_cr3_white());
        assert_eq!(r.white_at(0), 14558.0);
        r.format = RawFormat::Cr3;
        r.white = vec![14558.0, 15000.0, 14558.0];
        assert!(!r.extend_cr3_white());
        assert_eq!(r.color.baseline_exposure, 0.0);
    }
    fn cr3(white: f32) -> RawImage {
        let mut r = raw_with(None, white);
        r.format = RawFormat::Cr3;
        r.bits = 14;
        r
    }

    #[test]
    fn cr3_headroom_recognizes_a_spread_saturation_plateau() {
        let mut r = cr3(10439.0);
        if let RawData::U16(d) = &mut r.data {
            for (i, v) in d.iter_mut().take(3072).enumerate() {
                *v = 13300 + (i * 37 % 401) as u16;
            }
        }
        let samples = r.data.clone();
        assert!(r.extend_cr3_white(), "the R100-style plateau spans hundreds of codes");
        assert!((13300.0..=13364.0).contains(&r.white_at(0)), "{}", r.white_at(0));
        assert_eq!(r.data, samples);
    }

    #[test]
    fn cr3_headroom_keeps_valid_values_when_nothing_is_saturated() {
        let mut r = cr3(14558.0);
        if let RawData::U16(d) = &mut r.data {
            for (i, v) in d.iter_mut().enumerate() {
                *v = 528 + (i * 37 % 15855) as u16;
            }
        }
        let samples = r.data.clone();
        assert!(r.extend_cr3_white(), "unclipped data past the stated white is still real light");
        assert_eq!(r.white_at(0), 16383.0);
        assert_eq!(r.data, samples);
    }

    #[test]
    fn cr3_headroom_is_recovered_when_only_one_colour_reaches_it() {
        let mut r = cr3(14558.0);
        let cfa = r.cfa.clone().unwrap();
        if let RawData::U16(d) = &mut r.data {
            for (i, v) in d.iter_mut().enumerate() {
                if cfa.color_at(i % 64, i / 64) == 0 {
                    *v = 16383;
                }
            }
        }
        assert!(r.extend_cr3_white(), "a coloured highlight need not clip all three channels");
        assert_eq!(r.white_at(0), 16383.0);
    }
}
