//! Sensors that clip below, or above, the stated white level.
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
//!
//! The opposite also happens: Canon's CR3 `ColorData` white level sits below where the sensor saturates
//! (EOS RP: 14558 while samples run linearly up to a pile-up at 16383; EOS R100 at ISO 640: 10439 under a
//! pile-up spread over 13300–13700, about 0.4 EV). Samples between the two are real light, but highlight
//! reconstruction took them as clipped and rebuilt them, which drew a hard-edged pale band where a bright
//! sky crosses the stated level. [`RawImage::extend_white_to_saturation`] moves the white level up to the
//! data's saturation point and adds the difference to the baseline exposure (recording it as
//! [`crate::ColorData::white_extension_ev`], so the camera-look fit still leaves out what the file calls clipped):
//! everything below the old white level renders exactly as before and only the highlights gain range.

use crate::vendor::sample_row_step;
use crate::{Cfa, RawData, RawImage};

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

/// Above the stated white level the search runs on coarse bins of this share of the range above black: a plateau
/// made by a digital ISO pull (Canon's −⅓-stop ISOs) is spread over a few hundred values, not one.
const ABOVE_BIN: f32 = 1.0 / 128.0;
/// Samples above the stated white level worth acting on: at least this share of all samples (and 64 of them).
const ABOVE_SHARE: f64 = 1e-5;
/// A saturation plateau above the stated white level is at least this many times as populated as the bins below
/// it (`ABOVE_REFERENCE` of them, skipping the one right under the plateau, which a spread plateau spills into).
const ABOVE_SPIKE: f64 = 3.0;
const ABOVE_REFERENCE: usize = 8;
/// Samples above the top of the data that are hot pixels, not where it ends.
const ABOVE_STRAYS: u64 = 16;

impl RawImage {
    /// The value each colour's samples saturate at, when that is clearly below the stated white level
    /// (see the module docs). Only single-plane integer CFA data is examined.
    pub fn saturation_points(&self) -> [Option<u16>; 3] {
        let none = [None; 3];
        let Some((d, cfa, black, white)) = self.cfa_samples().filter(|&(.., white)| white <= 65535.0) else { return none };
        let a = self.active_area;
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

    /// Where the sensor saturates when its samples run past the stated white level (see the module docs): the
    /// lowest lower edge of a colour's pile-up above that level, or the top of the bit depth when no pile-up is
    /// found but the data reaches that top. `None` when next to nothing lies above the stated white level, when
    /// the data ends under the top without piling up (no measured saturation point), or when the image isn't
    /// single-plane integer CFA data with sane levels.
    pub fn saturation_above_white(&self) -> Option<f32> {
        let full = ((1u32 << self.bits.clamp(1, 16)) - 1) as f32;
        let (d, cfa, black, white) = self.cfa_samples().filter(|&(.., white)| white < full)?;
        let a = self.active_area;
        // coarse bins over the upper half of the range, so the bins under a plateau just above the stated white
        // level can still serve as its reference
        let base = (black + 0.5 * (full - black)).min(white).floor() as usize;
        let bin = ((ABOVE_BIN * (full - black)) as usize).max(1);
        let bins = (full as usize - base) / bin + 1;
        let mut hist = vec![[0u64; 3]; bins];
        // integer samples above `white` are those above its floor (`white` is finite, in 0..full)
        let white_floor = white as usize;
        let full_value = full as usize;
        let mut total = 0u64;
        let mut above = 0u64;
        let mut at_top = 0u64;
        for y in (0..a.height).step_by(sample_row_step(a.height)) {
            let Some(row) = d.get((a.y + y) * self.width + a.x..).and_then(|r| r.get(..a.width)) else { continue };
            for (x, &v) in row.iter().enumerate() {
                total += 1;
                let v = usize::from(v);
                if v > white_floor {
                    above += 1;
                }
                if v >= full_value {
                    at_top += 1;
                }
                if v >= base
                    && let Some(h) = hist.get_mut((v - base) / bin)
                {
                    h[cfa.color_at(a.x + x, a.y + y).min(2) as usize] += 1;
                }
            }
        }
        if (above as f64) < (total as f64 * ABOVE_SHARE).max(64.0) {
            return None;
        }
        // the bin holding the stated white level: a pile-up can start in it (white level just under a spread plateau)
        let first = white_floor.saturating_sub(base) / bin;
        let pile_up = |c: usize| {
            let count = |i: usize| hist.get(i).map_or(0, |h| h[c]);
            // a pile-up is the top of the data (a few stray hot pixels above it aside), not its busiest bin: a bright
            // sky just over the stated level can hold more samples than a small clipped patch
            let mut strays = 0;
            let top = (first..bins).rev().find(|&i| {
                strays += count(i);
                strays > ABOVE_STRAYS
            })?;
            let peak = (top.saturating_sub(2).max(first)..=top).max_by_key(|&i| (count(i), i)).unwrap_or(top);
            let n = count(peak);
            let reference =
                (peak.saturating_sub(1 + ABOVE_REFERENCE)..peak.saturating_sub(1)).map(count).sum::<u64>() as f64 / ABOVE_REFERENCE as f64;
            let spike = ABOVE_SPIKE * reference.max(1.0);
            let beyond: u64 = (peak + 3..bins).map(count).sum();
            if (n as f64) < spike || n < 32 || beyond as f64 > MAX_ABOVE * n as f64 {
                return None;
            }
            // a spread plateau covers the bins under its peak that still stand out from the reference
            let mut edge = peak;
            while edge > first && count(edge - 1) as f64 >= spike && count(edge - 1) * 2 >= n {
                edge -= 1;
            }
            Some(((base + edge * bin) as f32).clamp(white, full))
        };
        // no pile-up found: data that reaches the top of the bit depth piles up there behind a busy patch of light
        // the bins can't tell apart from it; data that ends under the top has no measured saturation point and keeps
        // the stated level. A pile-up at or just over the stated level keeps it too.
        let level = match (0..3).filter_map(pile_up).reduce(f32::min) {
            Some(level) => level,
            None if at_top > ABOVE_STRAYS => full,
            None => return None,
        };
        (level > white + 1.0).then_some(level)
    }

    /// Raise the white level to [`Self::saturation_above_white`] and add the change to the baseline exposure, so
    /// samples below the old white level render as before while the ones above it stay real light instead of
    /// counting as clipped. The change is also kept as [`crate::ColorData::white_extension_ev`], for fits that must
    /// still treat what the file called clipped as clipped. Returns the new white level; the samples are not touched.
    pub fn extend_white_to_saturation(&mut self) -> Option<f32> {
        let level = self.saturation_above_white()?;
        // finite and above 1: `saturation_above_white` checked the levels and returns one above the white level
        let black = self.black.mean();
        let ev = f64::from((level - black) / (self.white_at(0) - black)).log2();
        self.white = vec![level];
        self.color.baseline_exposure += ev;
        self.color.white_extension_ev += ev;
        Some(level)
    }

    /// The samples, CFA, black and stated white level of single-plane integer CFA data whose active area lies in
    /// the image. A hostile file can state any levels: NaN, negative or inverted ones give `None`.
    fn cfa_samples(&self) -> Option<(&[u16], &Cfa, f32, f32)> {
        let (RawData::U16(d), Some(cfa)) = (&self.data, &self.cfa) else { return None };
        let white = self.white_at(0);
        let black = self.black.mean();
        let levels_ok = black.is_finite() && white.is_finite() && black >= 0.0 && white > black;
        let a = self.active_area;
        let fits = |start: usize, len: usize, max: usize| start.checked_add(len).is_some_and(|end| end <= max);
        let ok = self.cpp == 1
            && cfa.valid()
            && self.width.checked_mul(self.height) == Some(d.len())
            && levels_ok
            && fits(a.x, a.width, self.width)
            && fits(a.y, a.height, self.height);
        ok.then_some((d.as_slice(), cfa, black, white))
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

    /// A 14-bit 128 x 128 RGGB mosaic like a Canon CR3: black `black`, stated white `white`, samples spread
    /// evenly from black up to `top`; `pile(i, v)` may replace each sample (a sensor's saturation).
    fn canon(black: f32, white: f32, top: u32, pile: impl Fn(usize, u16) -> u16) -> RawImage {
        let span = top - black as u32;
        let data = (0..128 * 128usize)
            .map(|i| {
                let (x, y) = (i % 128, i / 128);
                let v = (black as u32 + ((x * 131 + y * 37 + (x * y) % 97) as u32 * 7919) % span).min(65535) as u16;
                pile(i, v)
            })
            .collect();
        RawImage {
            width: 128,
            height: 128,
            data: RawData::U16(data),
            bits: 14,
            black: BlackLevel::uniform(black),
            active_area: Rect::new(0, 0, 128, 128),
            crop: Rect::new(0, 0, 128, 128),
            ..raw_with(None, white)
        }
    }

    /// EOS RP: ColorData says 14558, the sensor piles up at 16383 and is linear in between.
    fn eos_rp() -> RawImage {
        canon(512.0, 14558.0, 18000, |_, v| v.min(16383))
    }

    /// EOS R100 at ISO 640 (a digitally pulled ISO): ColorData says 10439, the samples pile up over 13300..13700.
    fn r100(white: f32) -> RawImage {
        canon(2048.0, white, 16000, |i, v| if v >= 13300 { 13300 + ((i * 7919) % 400) as u16 } else { v })
    }

    #[test]
    fn data_past_the_white_level_extends_it_to_the_pile_up() {
        let mut r = eos_rp();
        let before = r.data.clone();
        let level = r.extend_white_to_saturation().expect("raised");
        // the lower edge of the plateau's bin (1/128 of the range)
        assert!((16383.0 - 125.0..=16383.0).contains(&level), "{level}");
        assert_eq!(r.white, vec![level]);
        assert!(r.color.white_extension_ev > 0.0 && r.color.white_extension_ev == r.color.baseline_exposure, "{:?}", r.color);
        assert_eq!(r.data, before, "samples untouched");
        // a sample under the old white level normalises (with the baseline exposure) to what it did before
        let v = 9000.0f32;
        let old = (v - 512.0) / (14558.0 - 512.0);
        let new = (v - 512.0) / (level - 512.0) * 2f32.powf(r.color.baseline_exposure as f32);
        assert!((old - new).abs() < 1e-5, "{old} {new}");
        assert_eq!(r.extend_white_to_saturation(), None, "idempotent");
    }

    #[test]
    fn a_spread_pile_up_above_the_white_level_is_found() {
        let r = r100(10439.0);
        let level = r.saturation_above_white().expect("found");
        assert!((13100.0..=13400.0).contains(&level), "{level}");
    }

    #[test]
    fn a_white_level_just_under_a_pile_up_is_kept() {
        // the stated level sits inside the plateau's bin: the plateau is still a plateau, not unclipped light
        // (taking the whole range would leave the saturated samples unflagged and turn them magenta)
        let r = r100(13320.0);
        // kept, or moved up at most to the plateau's lower bin edge, where its samples still count as clipped
        let level = r.saturation_above_white().unwrap_or(13320.0);
        assert!(level <= 2048.0 + (13300.0 - 2048.0) / 0.99, "{level}");
    }

    #[test]
    fn a_small_pile_up_above_a_bright_sky_is_found() {
        // most samples above the stated level are a sky just over it; the clipped patch at the top, where a pulled
        // ISO saturates (16100, under the 14-bit range), is far smaller. (A sky within `ABOVE_REFERENCE` bins of
        // the pile-up would hide it: the reference is a mean.)
        let r = canon(512.0, 14558.0, 16300, |i, v| match i % 50 {
            0 => 16100,
            1..=25 if v > 12000 => 14600 + (v % 100),
            _ => v.min(16100),
        });
        let level = r.saturation_above_white().expect("found");
        assert!((16100.0 - 125.0..=16100.0).contains(&level), "{level}");
    }

    #[test]
    fn data_past_the_white_level_without_a_pile_up_keeps_the_stated_level() {
        // light above the stated level that ends under the top of the bit depth without piling up: no measured
        // saturation point, so the stated level stays (a 90D at ISO 100 tops out at 15047 over a stated 13036)
        let mut r = canon(2048.0, 14558.0, 15800, |_, v| v);
        assert_eq!(r.extend_white_to_saturation(), None);
        assert_eq!(r.white, vec![14558.0]);
    }

    #[test]
    fn data_reaching_the_top_takes_it_when_its_pile_up_is_hidden() {
        // a third of the samples are a busy patch of light over the 8 bins under the top, which hides the pile-up at
        // 16383 from the bin search; the data still reaches the top of the bit depth, so that is the saturation point
        let r = canon(512.0, 14558.0, 16300, |i, v| match (i % 3, i % 40) {
            (_, 0) => 16383,
            (0, _) => 15400 + v % 980,
            _ => v,
        });
        assert_eq!(r.saturation_above_white(), Some(16383.0));
    }

    #[test]
    fn data_under_the_white_level_is_left_alone() {
        let mut r = canon(512.0, 14558.0, 14000, |_, v| v);
        assert_eq!(r.extend_white_to_saturation(), None);
        assert_eq!(r.white, vec![14558.0]);
        assert_eq!(r.color.baseline_exposure, 0.0);
        // a few hot pixels above it are not light worth a new white level
        let mut r = canon(512.0, 14558.0, 14000, |i, v| if i % 1000 == 0 { 16383 } else { v });
        assert_eq!(r.extend_white_to_saturation(), None);
    }

    #[test]
    fn hostile_levels_keep_the_stated_white_level() {
        for (black, white) in [(f32::NAN, 14558.0), (-5.0, 14558.0), (512.0, f32::NAN), (512.0, 16383.0), (512.0, 70000.0), (15000.0, 14558.0)] {
            let mut r = eos_rp();
            r.black = BlackLevel::uniform(black);
            r.white = vec![white];
            assert_eq!(r.extend_white_to_saturation(), None, "{black} {white}");
        }
        let mut r = eos_rp();
        r.active_area = Rect::new(usize::MAX, 0, 2, 128);
        assert_eq!(r.extend_white_to_saturation(), None);
        r.active_area = Rect::new(0, 0, 0, 0);
        assert_eq!(r.extend_white_to_saturation(), None);
    }
}
