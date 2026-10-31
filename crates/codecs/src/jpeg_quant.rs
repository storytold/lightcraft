//! Quantization tables for [`crate::jpeg_par`], ported from jpegli (libjxl's JPEG encoder,
//! <https://github.com/google/jpegli>, `lib/jpegli/quant.cc` at 031a0077), Copyright (c) the JPEG XL Project Authors,
//! BSD-3-Clause (see NOTICE). Modified by the LightCraft contributors (2026): translated from C++ to Rust, YCbCr
//! tables only, and LightCraft's own mapping from the 1–100 quality to the distance ([`quality_to_distance`]).
//!
//! jpegli's tables differ per channel (one each for Y, Cb and Cr) and are scaled by a butteraugli-like *distance*
//! rather than libjpeg's linear quality factor: past distance 1.5 the low frequencies coarsen more slowly than the
//! high ones ([`EXPONENT`]). At 4:2:0 the chroma tables are finer, since each chroma block covers four luma blocks.
//! Rounding uses a slightly wider dead zone for the AC coefficients ([`AC_DEAD_ZONE`]), as jpegli does. The file
//! stays a plain baseline JPEG with 8-bit tables.

/// jpegli `kBaseQuantMatrixYCbCr`: base tables for Y, Cb and Cr (natural order), scaled by [`GLOBAL_SCALE`] and the
/// distance.
#[allow(clippy::excessive_precision)]
#[rustfmt::skip]
const BASE: [[f32; 64]; 3] = [
    [
        1.2397409345866273, 1.7227115097630963, 2.9212167156636855, 2.812737435286529, 3.339819711906184, 3.463603762596166, 3.840915217993518, 3.86956,
        1.7227115097630963, 2.0928894413636874, 2.8456760904429297, 2.704506820909662, 3.4407673520905337, 3.166232352090534, 4.025208741558432, 4.035324490952577,
        2.9212167156636855, 2.8456760904429297, 2.9587403520905338, 3.3862948970669273, 3.619523781336757, 3.9046279999999998, 3.757835838431854, 4.237447515714274,
        2.812737435286529, 2.704506820909662, 3.3862948970669273, 3.380058821812233, 4.1679867415584315, 4.805510627261856, 4.784259, 4.605934,
        3.339819711906184, 3.4407673520905337, 3.619523781336757, 4.1679867415584315, 4.579851258441568, 4.923237, 5.574107, 5.48533336146308,
        3.463603762596166, 3.166232352090534, 3.9046279999999998, 4.805510627261856, 4.923237, 5.43936, 5.093895741558431, 6.0872254423617225,
        3.840915217993518, 4.025208741558432, 3.757835838431854, 4.784259, 5.574107, 5.093895741558431, 5.438461, 5.4037359493250845,
        3.86956, 4.035324490952577, 4.237447515714274, 4.605934, 5.48533336146308, 6.0872254423617225, 5.4037359493250845, 4.37787101190424,
    ],
    [
        2.8236197786377537, 6.495639358561486, 9.310489207538302, 10.64747864717083, 11.07419143098738, 17.146390223910462, 18.463982229408998, 29.087001644203088,
        6.495639358561486, 8.890103846667353, 8.976895794294748, 13.666270550318826, 16.547071905624193, 16.63871382827686, 26.778396930893695, 21.33034294694781,
        9.310489207538302, 8.976895794294748, 11.08737706005991, 18.20548239870446, 19.752481654011646, 23.985660533114896, 102.6457378402362, 24.450989,
        10.64747864717083, 13.666270550318826, 18.20548239870446, 18.628012327860365, 16.042509519487183, 25.04918273242625, 25.017140189353015, 35.79788782635831,
        11.07419143098738, 16.547071905624193, 19.752481654011646, 16.042509519487183, 19.373482748612577, 14.677529999999999, 19.94695960400931, 51.094112,
        17.146390223910462, 16.63871382827686, 23.985660533114896, 25.04918273242625, 14.677529999999999, 31.320412426835304, 46.357234000000005, 67.48111451705412,
        18.463982229408998, 26.778396930893695, 102.6457378402362, 25.017140189353015, 19.94695960400931, 46.357234000000005, 61.315764694388044, 88.34665293823721,
        29.087001644203088, 21.33034294694781, 24.450989, 35.79788782635831, 51.094112, 67.48111451705412, 88.34665293823721, 112.16099098350989,
    ],
    [
        2.9217254961255255, 4.497681013199305, 7.356344520940414, 6.583891506504051, 8.535608740100237, 8.799434353234647, 9.188341534163023, 9.482700481227672,
        4.497681013199305, 6.309548851989123, 7.024608962670982, 7.156445324163424, 8.049059218663244, 7.0124290657218555, 6.711923184393611, 8.380307846134853,
        7.356344520940414, 7.024608962670982, 6.892101177327445, 6.882819916277163, 8.782226090078568, 6.8774750000000004, 7.8858175969577955, 8.67909,
        6.583891506504051, 7.156445324163424, 6.882819916277163, 7.003072944847055, 7.7223464701024875, 7.955425720217421, 7.4734110000000005, 8.362933242943903,
        8.535608740100237, 8.049059218663244, 8.782226090078568, 7.7223464701024875, 6.778005927001542, 9.484922741558432, 9.043702663686046, 8.053178199770173,
        8.799434353234647, 7.0124290657218555, 6.8774750000000004, 7.955425720217421, 9.484922741558432, 8.607606527385098, 9.922697394370815, 64.25135180237939,
        9.188341534163023, 6.711923184393611, 7.8858175969577955, 7.4734110000000005, 9.043702663686046, 9.922697394370815, 63.184936549738225, 83.35294340273799,
        9.482700481227672, 8.380307846134853, 8.67909, 8.362933242943903, 8.053178199770173, 64.25135180237939, 83.35294340273799, 114.89202448569779,
    ],
];

/// jpegli `k420Rescale`: the chroma tables' extra factor at 4:2:0.
#[rustfmt::skip]
const RESCALE_420: [f32; 64] = [
    0.4093, 0.3209, 0.3477, 0.3333, 0.3144, 0.2823, 0.3214, 0.3354,
    0.3209, 0.3111, 0.3489, 0.2801, 0.3059, 0.3119, 0.4135, 0.3445,
    0.3477, 0.3489, 0.3586, 0.3257, 0.2727, 0.3754, 0.3369, 0.3484,
    0.3333, 0.2801, 0.3257, 0.3020, 0.3515, 0.3410, 0.3971, 0.3839,
    0.3144, 0.3059, 0.2727, 0.3515, 0.3105, 0.3397, 0.2716, 0.3836,
    0.2823, 0.3119, 0.3754, 0.3410, 0.3397, 0.3212, 0.3203, 0.0726,
    0.3214, 0.4135, 0.3369, 0.3971, 0.2716, 0.3203, 0.0798, 0.0553,
    0.3354, 0.3445, 0.3484, 0.3839, 0.3836, 0.0726, 0.0553, 0.3368,
];

/// jpegli `kExponent`: how fast each coefficient's step grows with the distance past [`DIST0`].
#[rustfmt::skip]
const EXPONENT: [f32; 64] = [
    1.00, 0.51, 0.67, 0.74, 1.00, 1.00, 1.00, 1.00,
    0.51, 0.66, 0.69, 0.87, 1.00, 1.00, 1.00, 1.00,
    0.67, 0.69, 0.84, 0.83, 0.96, 1.00, 1.00, 1.00,
    0.74, 0.87, 0.83, 1.00, 1.00, 0.91, 0.91, 1.00,
    1.00, 1.00, 0.96, 1.00, 1.00, 1.00, 1.00, 1.00,
    1.00, 1.00, 1.00, 0.91, 1.00, 1.00, 1.00, 1.00,
    1.00, 1.00, 1.00, 0.91, 1.00, 1.00, 1.00, 1.00,
    1.00, 1.00, 1.00, 1.00, 1.00, 1.00, 1.00, 1.00,
];

/// jpegli `kGlobalScaleYCbCr`.
const GLOBAL_SCALE: f32 = 1.739_660_1;
/// jpegli `k420GlobalScale`: every table's extra factor at 4:2:0.
const GLOBAL_SCALE_420: f32 = 1.22;
/// jpegli `kDist0`: the distance past which [`EXPONENT`] applies.
const DIST0: f32 = 1.5;

/// jpegli `kZeroBiasOffsetYCbCrAC`: an AC coefficient whose quantized magnitude is below this rounds to zero (Y, Cb,
/// Cr); DC rounds to nearest.
pub(crate) const AC_DEAD_ZONE: [f32; 3] = [0.590_82, 0.581_46, 0.579_88];

/// jpegli `DistanceToScale`: the factor on coefficient `k`'s base step at `distance`.
fn distance_to_scale(distance: f32, k: usize) -> f32 {
    if distance < DIST0 {
        return distance;
    }
    let exp = EXPONENT.get(k).copied().unwrap_or(1.0);
    let mul = f64::from(DIST0).powf(1.0 - f64::from(exp)) as f32;
    (0.5 * distance).max(mul * distance.powf(exp))
}

/// The Y, Cb and Cr quantization tables (natural order, 1–255) at `distance`, for 4:2:0 chroma when `s420`.
pub(crate) fn tables(distance: f32, s420: bool) -> [[u16; 64]; 3] {
    let global = if s420 { GLOBAL_SCALE * GLOBAL_SCALE_420 } else { GLOBAL_SCALE };
    let distance = if distance.is_finite() { distance.max(0.0) } else { 0.0 };
    std::array::from_fn(|c| {
        std::array::from_fn(|k| {
            let mut scale = global * distance_to_scale(distance, k);
            if s420 && c > 0 {
                scale *= RESCALE_420[k];
            }
            // (`as` saturates, and NaN goes to 0: clamped to 1 either way)
            ((scale * BASE[c][k]).round() as i32).clamp(1, 255) as u16
        })
    })
}

/// (quality, distance) anchors of [`quality_to_distance`]: at each, the distance at which these tables score the same
/// SSIMULACRA2 as the T.81 Annex K tables with libjpeg's quality scaling did at that quality (LightCraft's encoder
/// before it used jpegli's tables; the mean over 12 developed raws at 2048 px, 4:4:4, measured with
/// `crates/engine/examples/jpeg_quality.rs`). So a quality looks as it did and libjpeg's quality scale keeps its
/// meaning, with a smaller file. Below 15, where the old tables score under 17, the anchors follow the power law that
/// fits the rest (distance ≈ 5.03·s^1.08, `s` libjpeg's scale factor over 100), so the lowest qualities still give the
/// smallest files.
const QUALITY_DISTANCE: [(u8, f32); 23] = [
    (1, 340.0),
    (5, 60.0),
    (10, 28.0),
    (15, 19.08),
    (20, 13.9),
    (25, 10.85),
    (30, 9.09),
    (35, 7.52),
    (40, 6.62),
    (45, 5.76),
    (50, 5.07),
    (55, 4.54),
    (60, 4.08),
    (65, 3.49),
    (70, 2.86),
    (75, 2.2),
    (80, 1.73),
    (85, 1.25),
    (90, 0.82),
    (95, 0.44),
    (98, 0.18),
    (99, 0.05),
    (100, 0.0),
];

/// The distance for a 1–100 quality (0 and above 100 clamp): linear between the [`QUALITY_DISTANCE`] anchors.
pub(crate) fn quality_to_distance(quality: u8) -> f32 {
    let q = quality.clamp(1, 100);
    let mut prev = QUALITY_DISTANCE[0];
    for &(aq, ad) in &QUALITY_DISTANCE {
        if q <= aq {
            let span = f32::from(aq.saturating_sub(prev.0)).max(1.0);
            return prev.1 + (ad - prev.1) * f32::from(q.saturating_sub(prev.0)) / span;
        }
        prev = (aq, ad);
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables match the ones `cjpegli -p 0 --noadaptive_quantization` (jpegli 031a0077) writes at the same
    /// distance (its quality 30 is distance 6.4, quality 90 distance 1.0), at 4:4:4 and 4:2:0.
    #[test]
    fn tables_match_cjpegli() {
        #[rustfmt::skip]
        let want: [[u16; 64]; 3] = [
            [14, 10, 20, 21, 37, 39, 43, 43, 10, 14, 20, 25, 38, 35, 45, 45, 20, 20, 26, 29, 38, 43, 42, 47, 21, 25, 29, 38, 46, 47, 47, 51, 37, 38, 38, 46, 51, 55, 62, 61, 39, 35, 43, 47, 55, 61, 57, 68, 43, 45, 42, 47, 62, 57, 61, 60, 43, 45, 47, 51, 61, 68, 60, 49],
            [31, 36, 64, 81, 123, 191, 206, 255, 36, 60, 64, 126, 184, 185, 255, 237, 64, 64, 98, 158, 208, 255, 255, 255, 81, 126, 158, 207, 179, 245, 244, 255, 123, 184, 208, 179, 216, 163, 222, 255, 191, 185, 255, 245, 163, 255, 255, 255, 206, 255, 255, 244, 222, 255, 255, 255, 255, 237, 255, 255, 255, 255, 255, 255],
            [33, 25, 51, 50, 95, 98, 102, 106, 25, 43, 50, 66, 90, 78, 75, 93, 51, 50, 61, 60, 92, 77, 88, 97, 50, 66, 60, 78, 86, 78, 73, 93, 95, 90, 92, 86, 75, 106, 101, 90, 98, 78, 77, 78, 106, 96, 110, 255, 102, 75, 88, 73, 101, 110, 255, 255, 106, 93, 97, 93, 90, 255, 255, 255],
        ];
        assert_eq!(tables(6.4, false), want, "444 q30");
        #[rustfmt::skip]
        let want: [[u16; 64]; 3] = [
            [2, 3, 5, 5, 6, 6, 7, 7, 3, 4, 5, 5, 6, 6, 7, 7, 5, 5, 5, 6, 6, 7, 7, 7, 5, 5, 6, 6, 7, 8, 8, 8, 6, 6, 6, 7, 8, 9, 10, 10, 6, 6, 7, 8, 9, 9, 9, 11, 7, 7, 7, 8, 10, 9, 9, 9, 7, 7, 7, 8, 10, 11, 9, 8],
            [5, 11, 16, 19, 19, 30, 32, 51, 11, 15, 16, 24, 29, 29, 47, 37, 16, 16, 19, 32, 34, 42, 179, 43, 19, 24, 32, 32, 28, 44, 44, 62, 19, 29, 34, 28, 34, 26, 35, 89, 30, 29, 42, 44, 26, 54, 81, 117, 32, 47, 179, 44, 35, 81, 107, 154, 51, 37, 43, 62, 89, 117, 154, 195],
            [5, 8, 13, 11, 15, 15, 16, 16, 8, 11, 12, 12, 14, 12, 12, 15, 13, 12, 12, 12, 15, 12, 14, 15, 11, 12, 12, 12, 13, 14, 13, 15, 15, 14, 15, 13, 12, 17, 16, 14, 15, 12, 12, 14, 17, 15, 17, 112, 16, 12, 14, 13, 16, 17, 110, 145, 16, 15, 15, 15, 14, 112, 145, 200],
        ];
        assert_eq!(tables(1.0, false), want, "444 q90");
        #[rustfmt::skip]
        let want: [[u16; 64]; 3] = [
            [17, 12, 25, 26, 45, 47, 52, 53, 12, 17, 25, 30, 47, 43, 55, 55, 25, 25, 32, 36, 46, 53, 51, 58, 26, 30, 36, 46, 57, 57, 57, 63, 45, 47, 46, 57, 62, 67, 76, 75, 47, 43, 53, 57, 67, 74, 69, 83, 52, 55, 51, 57, 76, 69, 74, 73, 53, 55, 58, 63, 75, 83, 73, 59],
            [16, 14, 27, 33, 47, 66, 81, 133, 14, 23, 27, 43, 69, 70, 150, 100, 27, 27, 43, 63, 69, 122, 255, 116, 33, 43, 63, 76, 77, 102, 118, 187, 47, 69, 69, 77, 82, 68, 74, 255, 66, 70, 122, 102, 68, 137, 202, 67, 81, 150, 255, 118, 74, 202, 66, 66, 133, 100, 116, 187, 255, 67, 66, 255],
            [16, 10, 22, 20, 36, 34, 40, 43, 10, 16, 21, 23, 33, 30, 38, 39, 22, 21, 27, 24, 31, 35, 36, 41, 20, 23, 24, 29, 37, 32, 35, 44, 36, 33, 31, 37, 29, 44, 33, 42, 34, 30, 35, 32, 44, 38, 43, 63, 40, 38, 36, 35, 33, 43, 68, 63, 43, 39, 41, 44, 42, 63, 63, 255],
        ];
        assert_eq!(tables(6.4, true), want, "420 q30");
        #[rustfmt::skip]
        let want: [[u16; 64]; 3] = [
            [3, 4, 6, 6, 7, 7, 8, 8, 4, 4, 6, 6, 7, 7, 9, 9, 6, 6, 6, 7, 8, 8, 8, 9, 6, 6, 7, 7, 9, 10, 10, 10, 7, 7, 8, 9, 10, 10, 12, 12, 7, 7, 8, 10, 10, 12, 11, 13, 8, 9, 8, 10, 12, 11, 12, 11, 8, 9, 9, 10, 12, 13, 11, 9],
            [2, 4, 7, 8, 7, 10, 13, 21, 4, 6, 7, 8, 11, 11, 24, 16, 7, 7, 8, 13, 11, 19, 73, 18, 8, 8, 13, 12, 12, 18, 21, 29, 7, 11, 11, 12, 13, 11, 11, 42, 10, 11, 19, 18, 11, 21, 32, 10, 13, 24, 73, 21, 11, 32, 10, 10, 21, 16, 18, 29, 42, 10, 10, 80],
            [3, 3, 5, 5, 6, 5, 6, 7, 3, 4, 5, 4, 5, 5, 6, 6, 5, 5, 5, 5, 5, 5, 6, 6, 5, 4, 5, 4, 6, 6, 6, 7, 6, 5, 5, 6, 4, 7, 5, 7, 5, 5, 5, 6, 7, 6, 7, 10, 6, 6, 6, 6, 5, 7, 11, 10, 7, 6, 6, 7, 7, 10, 10, 82],
        ];
        assert_eq!(tables(1.0, true), want, "420 q90");
    }

    /// The distance falls as the quality rises, through every anchor; quality 100 is all ones (as libjpeg's is).
    #[test]
    fn quality_maps_to_a_falling_distance() {
        let d: Vec<f32> = (0..=255u8).map(quality_to_distance).collect();
        assert!(d.windows(2).skip(1).take(99).all(|w| w[1] < w[0]), "{d:?}");
        assert!(d[100..].iter().all(|&x| x == 0.0));
        assert_eq!(d[0], d[1]);
        for (q, want) in QUALITY_DISTANCE {
            assert!((quality_to_distance(q) - want).abs() < 1e-6, "q{q}");
        }
        assert_eq!(tables(quality_to_distance(100), false), [[1; 64]; 3]);
        assert_eq!(tables(quality_to_distance(100), true), [[1; 64]; 3]);
    }

    /// Hostile distances still give valid baseline tables.
    #[test]
    fn tables_stay_in_range() {
        for d in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0, 1e-9, 25.0, 1e30] {
            for s420 in [false, true] {
                assert!(tables(d, s420).iter().flatten().all(|&q| (1..=255).contains(&q)), "{d} {s420}");
            }
        }
    }
}
