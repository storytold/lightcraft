//! Two-field Nikon Coolpix raws: E5700 (CYGM four-colour filter) and E8400 (RGB Bayer).
//!
//! The layout was measured clean-room by black-box analysis of the CC0 raw.pixls.us files (E5700 2576 x 1924,
//! E8400 3280 x 2454; log under `data/testing/flash-coolpix`, not committed), not taken from any other raw converter:
//!
//! * one strip of `w * h * 1.5` bytes: 12-bit samples packed MSB-first, two per three bytes, no row padding;
//! * the strip holds two interlaced fields of `h / 2` full-width rows, field A first and field B second. Interleaving
//!   them line by line gives the sensor image. The E5700 puts B on the even lines (`out[2k] = B[k]`, `out[2k+1] = A[k]`),
//!   the E8400 puts A on the even lines. The order is a property of the model, not something the files state: the
//!   `0x828e` colour-filter tag is anchored at line 0 of the interleaved image, so the wrong parity swaps the two rows
//!   of every filter cell (a green cast on the CYGM file). We pick it from the model string;
//! * black level 0, white level 4095, nothing masked;
//! * E8400: `0x828e = [2, 1, 1, 0]`, an ordinary BGGR Bayer. E5700: `0x828e = [5, 3, 1, 4]`, i.e. Y C / G M.
//!
//! The decoder is guarded: only those two models with exactly those sizes, a single strip of exactly the packed size
//! and (E5700) exactly that colour-filter tag take this path; every other NEF is untouched.
//!
//! CYGM to RGB is deliberately simple. Each of the four colour planes is bilinearly interpolated to full resolution,
//! scaled by per-plane gains that make the scene average neutral (gray-world: mean Y = mean C = mean M = 2 x mean G;
//! the files' own white balance is not read), then combined with the ideal complementary-colour relations
//! `Y = R + G, C = G + B, M = R + B`: `R = (Y + M - C) / 2`, `B = (C + M - Y) / 2` and `G` as the mean of the green
//! plane and `(Y + C - M) / 2`. A fitted 3 x 4 matrix was rejected: it was ill-conditioned on a nearly neutral scene.
//! Limits: no per-camera colour matrix (colours are approximate), gray-world white balance, bilinear (soft, zippered
//! at edges) interpolation, negative results clamp to zero.

/// How a Coolpix model stores its interlaced fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TwoField {
    /// The first field of the strip (rows `0..h/2`) is the even lines of the sensor image.
    pub first_field_even: bool,
    /// Four-colour CYGM filter (else RGB Bayer taken from the file's tag).
    pub cygm: bool,
}

/// The layout for `model` at exactly the measured size, `None` for any other file.
pub(crate) fn layout(model: &str, w: usize, h: usize) -> Option<TwoField> {
    match (model.trim(), w, h) {
        ("E5700", 2576, 1924) => Some(TwoField { first_field_even: false, cygm: true }),
        ("E8400", 3280, 2454) => Some(TwoField { first_field_even: true, cygm: false }),
        _ => None,
    }
}

/// Whether the strip table is the measured single strip of exactly `w * h * 1.5` bytes.
pub(crate) fn strip_matches(offsets: &[u64], byte_counts: &[u64], w: usize, h: usize) -> bool {
    offsets.len() == 1 && byte_counts == [(w * h * 3 / 2) as u64]
}

/// Interleave the two fields of `samples` (`h / 2` rows each, field A first) into a `w x h` image.
pub(crate) fn deinterlace(samples: &[u16], w: usize, h: usize, first_field_even: bool) -> Option<Vec<u16>> {
    if w == 0 || h < 2 || !h.is_multiple_of(2) || samples.len() != w * h {
        return None;
    }
    let half = h / 2;
    let (a, b) = samples.split_at(half * w);
    let (even, odd) = if first_field_even { (a, b) } else { (b, a) };
    let mut out = vec![0u16; w * h];
    for k in 0..half {
        out[2 * k * w..(2 * k + 1) * w].copy_from_slice(&even[k * w..(k + 1) * w]);
        out[(2 * k + 1) * w..(2 * k + 2) * w].copy_from_slice(&odd[k * w..(k + 1) * w]);
    }
    Some(out)
}

const CODE_G: u8 = 1;
const CODE_C: u8 = 3;
const CODE_M: u8 = 4;
const CODE_Y: u8 = 5;

/// Linear-light RGB (interleaved, `u16`) from a CYGM mosaic, and the white level of the result.
///
/// `pattern` is the `0x828e` tag, row-major over the 2 x 2 cell at (0, 0); it must hold each of Y, C, G, M once.
pub(crate) fn cygm_to_rgb(mosaic: &[u16], w: usize, h: usize, pattern: [u8; 4]) -> Option<(Vec<u16>, f32)> {
    use rayon::prelude::*;
    if w < 4 || h < 4 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || mosaic.len() != w * h {
        return None;
    }
    // phase (py * 2 + px) of each colour: [Y, C, G, M]
    let phase_of = |code: u8| pattern.iter().position(|&c| c == code);
    let ph = [phase_of(CODE_Y)?, phase_of(CODE_C)?, phase_of(CODE_G)?, phase_of(CODE_M)?];
    let mut seen = [false; 4];
    ph.iter().for_each(|&p| seen[p] = true);
    if !seen.iter().all(|&s| s) {
        return None;
    }
    let (pw, prows) = (w / 2, h / 2);
    // the four sub-sampled planes, in colour order Y, C, G, M
    let planes: Vec<Vec<f32>> = ph
        .iter()
        .map(|&p| {
            let (py, px) = (p / 2, p % 2);
            (0..prows).flat_map(|i| (0..pw).map(move |j| (i, j))).map(|(i, j)| mosaic[(2 * i + py) * w + 2 * j + px] as f32).collect()
        })
        .collect();
    let mean = |p: &[f32]| (p.iter().map(|&v| v as f64).sum::<f64>() / p.len() as f64).max(1.0);
    let means: Vec<f64> = planes.iter().map(|p| mean(p)).collect();
    let target = (means[0] + means[1] + means[3]) / 3.0;
    // neutral scene: Y = C = M = 2 G
    let gains = [target / means[0], target / means[1], target / 2.0 / means[2], target / means[3]].map(|g| g as f32);

    // bilinear taps along one axis: (low index, high index, weight of the high one) per output coordinate
    let taps = |n: usize, phase: usize, len: usize| -> Vec<(usize, usize, f32)> {
        (0..len)
            .map(|c| {
                if c < phase {
                    (0, 0, 0.0)
                } else {
                    let i = (c - phase) / 2;
                    let f = if (c - phase) % 2 == 1 { 0.5 } else { 0.0 };
                    (i.min(n - 1), (i + 1).min(n - 1), f)
                }
            })
            .collect()
    };
    let x_taps: Vec<Vec<_>> = (0..2).map(|px| taps(pw, px, w)).collect();
    let y_taps: Vec<Vec<_>> = (0..2).map(|py| taps(prows, py, h)).collect();

    let mut out = vec![0u16; w * h * 3];
    let scale = 4.0f32;
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut v = [0f32; 4];
            for (k, plane) in planes.iter().enumerate() {
                let (py, px) = (ph[k] / 2, ph[k] % 2);
                let (y0, y1, fy) = y_taps[py][y];
                let (x0, x1, fx) = x_taps[px][x];
                let at = |i: usize, j: usize| plane[i * pw + j];
                let top = at(y0, x0) * (1.0 - fx) + at(y0, x1) * fx;
                let bot = at(y1, x0) * (1.0 - fx) + at(y1, x1) * fx;
                v[k] = (top * (1.0 - fy) + bot * fy) * gains[k];
            }
            let [yy, cc, gg, mm] = v;
            let r = (yy + mm - cc) * 0.5;
            let g = (gg + (yy + cc - mm) * 0.5) * 0.5;
            let b = (cc + mm - yy) * 0.5;
            for (o, c) in row[x * 3..x * 3 + 3].iter_mut().zip([r, g, b]) {
                *o = (c * scale).round().clamp(0.0, 65535.0) as u16;
            }
        }
    });
    // a clipped neutral patch reaches 4095 in the weakest of Y, C, M (after gain) and gives half of it in R, G, B
    let weakest = gains[0].min(gains[1]).min(gains[3]);
    Some((out, (4095.0 * weakest * 0.5 * scale).clamp(1.0, 65535.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_field_round_trip_for_both_parities() {
        let (w, h) = (6usize, 8usize);
        // sensor image whose every line is distinct
        let img: Vec<u16> = (0..w * h).map(|i| (i * 7 % 4096) as u16).collect();
        for first_even in [false, true] {
            let line = |y: usize| &img[y * w..(y + 1) * w];
            // fields as stored: A = first h/2 rows, B = the rest
            let evens: Vec<usize> = (0..h).step_by(2).collect();
            let odds: Vec<usize> = (1..h).step_by(2).collect();
            let order = if first_even { [evens, odds] } else { [odds, evens] };
            let stored: Vec<u16> = order.iter().flatten().flat_map(|&y| line(y).iter().copied()).collect();
            assert_eq!(deinterlace(&stored, w, h, first_even).unwrap(), img, "first_even = {first_even}");
        }
        // the two parities differ
        let s: Vec<u16> = (0..w * h).map(|i| i as u16).collect();
        assert_ne!(deinterlace(&s, w, h, true), deinterlace(&s, w, h, false));
    }

    #[test]
    fn deinterlace_refuses_bad_shapes() {
        assert!(deinterlace(&[0; 12], 3, 5, true).is_none());
        assert!(deinterlace(&[0; 11], 3, 4, true).is_none());
        assert!(deinterlace(&[], 0, 0, true).is_none());
    }

    #[test]
    fn layout_is_exact() {
        assert_eq!(layout("E5700", 2576, 1924), Some(TwoField { first_field_even: false, cygm: true }));
        assert_eq!(layout("E8400 ", 3280, 2454), Some(TwoField { first_field_even: true, cygm: false }));
        assert_eq!(layout("E5700", 2576, 1920), None);
        assert_eq!(layout("E8400", 2576, 1924), None);
        assert_eq!(layout("E5000", 2576, 1924), None);
        assert!(strip_matches(&[10], &[(2576 * 1924 * 3 / 2) as u64], 2576, 1924));
        assert!(!strip_matches(&[10, 20], &[1, 2], 2576, 1924));
        assert!(!strip_matches(&[10], &[5], 2576, 1924));
    }

    #[test]
    fn cygm_neutral_patch_stays_neutral() {
        let (w, h) = (16usize, 12usize);
        // ideal complementary filters on a neutral grey of level g: Y = C = M = 2 g, G = g
        for g in [200u16, 1000, 2000] {
            let m: Vec<u16> = (0..w * h).map(|i| if (i / w) % 2 == 1 && i % 2 == 0 { g } else { 2 * g }).collect();
            let (rgb, white) = cygm_to_rgb(&m, w, h, [5, 3, 1, 4]).unwrap();
            assert!(white > 0.0);
            let first = [rgb[0], rgb[1], rgb[2]];
            assert!(first[0] > 0);
            for px in rgb.chunks(3) {
                assert_eq!(px, &first);
            }
        }
    }

    #[test]
    fn cygm_refuses_bad_input_without_panicking() {
        assert!(cygm_to_rgb(&[0; 10], 4, 4, [5, 3, 1, 4]).is_none());
        assert!(cygm_to_rgb(&[], 0, 0, [5, 3, 1, 4]).is_none());
        assert!(cygm_to_rgb(&[0; 16], 4, 4, [5, 3, 1, 1]).is_none());
        assert!(cygm_to_rgb(&[0; 16], 4, 4, [2, 1, 1, 0]).is_none());
        assert!(cygm_to_rgb(&[0; 15], 5, 3, [5, 3, 1, 4]).is_none());
        // all-black input is fine
        assert!(cygm_to_rgb(&[0; 16], 4, 4, [5, 3, 1, 4]).is_some());
    }
}
