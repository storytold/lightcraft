//! Two-field Nikon Coolpix raws: E5700 (CYGM four-colour filter), E5400, E8400 and E8800 (RGB Bayer).
//!
//! The layout was measured clean-room by black-box analysis of the CC0 raw.pixls.us files (E5700 2576 x 1924,
//! E8400 3280 x 2454), not taken from any other raw converter:
//!
//! * one strip of `w * h * 1.5` bytes: 12-bit samples packed MSB-first, two per three bytes, no row padding (the
//!   ordinary uncompressed NEF reader unpacks it);
//! * the strip holds two interlaced fields of `h / 2` full-width rows, field A first and field B second. Interleaving
//!   them line by line gives the sensor image. The E5700 puts B on the even lines (`out[2k] = B[k]`, `out[2k+1] = A[k]`),
//!   the E8400 puts A on the even lines. The order is a property of the model, not something the files state: the
//!   `0x828e` colour-filter tag is anchored at line 0 of the interleaved image, so the wrong parity swaps the two rows
//!   of every filter cell (a green cast on the CYGM file). We pick it from the model string;
//! * black level 0, white level 4095, nothing masked;
//! * E8400 and E8800: `0x828e = [2, 1, 1, 0]`, an ordinary BGGR Bayer. E5700: `0x828e = [5, 3, 1, 4]`, i.e. Y C / G M.
//!
//! The Nikon reader restricts the data test ([`fields_stacked`]) to the measured legacy maker-note dialect
//! and tight 12-bit layout, then requires independent camera-preview geometry agreement. Scene statistics
//! alone cannot establish a storage format. Missing or ambiguous previews leave stored rows intact.
//! Only field order is per model.
//! The CYGM conversion is taken for the `[5, 3, 1, 4]` colour-filter tag.
//!
//! CYGM to RGB is deliberately simple. Each of the four colour planes is bilinearly interpolated to full resolution,
//! scaled by per-plane gains that make the scene average neutral (gray-world: mean Y = mean C = mean M = 2 x mean G;
//! the files' own white balance is not read), then combined with the ideal complementary-colour relations
//! `Y = R + G, C = G + B, M = R + B`: `R = (Y + M - C) / 2`, `B = (C + M - Y) / 2` and `G` as the mean of the green
//! plane and `(Y + C - M) / 2`. A fitted 3 x 4 matrix was rejected: it was ill-conditioned on a nearly neutral scene.
//! Limits: no per-camera colour matrix (colours are approximate), gray-world white balance, bilinear (soft, zippered
//! at edges) interpolation, negative results clamp to zero.

/// Whether the first field of the strip (rows `0..h/2`) holds the even lines of the sensor image. The order is a
/// property of the model, not something the files state: the E5700 stores the odd lines first, the other bodies the
/// even lines first. (The `0x828e` colour-filter tag is anchored at line 0 of the interleaved image, so the wrong
/// parity swaps the two rows of every filter cell.)
pub(crate) fn first_field_even(model: &str) -> bool {
    model.trim() != "E5700"
}

/// Whether `d` (`w x h`, rows in file order) holds the two interlaced fields one after the other, found from the
/// data and not from the model: in that layout neighbouring stored rows are the same colour two image rows apart, so
/// they differ far less than rows two apart; in a normal Bayer frame it is the other way round (one row over is
/// another colour). The seam at the half is checked as well. For a Bayer `pattern`, matching
/// green sites in the proposed fields must agree, to reject ordinary scenes with a horizon.
pub(crate) fn fields_stacked(d: &[u16], w: usize, h: usize, pattern: &[u8], first_even: bool) -> bool {
    let Some(n) = w.checked_mul(h).filter(|n| *n <= crate::MAX_SAMPLES) else { return false };
    if h < 64 || !h.is_multiple_of(2) || w < 64 || d.len() < n {
        return false;
    }
    let (g1, g2) = (row_gap(d, w, h, 1), row_gap(d, w, h, 2));
    if g1 >= g2 * 0.9 {
        return false;
    }
    // stored rows h/2-1 and h/2 are image rows h-2 and 1 (or h-1 and 0): far apart, so a real seam separates them
    let m = h / 2;
    let seam = (0..w).step_by(5).map(|x| (d[(m - 1) * w + x] as f64 - d[m * w + x] as f64).abs()).sum::<f64>() / w.div_ceil(5) as f64;
    seam > g1 * 4.0 && green_fields_agree(d, w, h, pattern, first_even)
}

/// Confirm the proposed geometry independently against the file's own camera preview.
/// Work is bounded: at most 32 MiB of input, a 2048-pixel JPEG edge and 4 MP decoded,
/// then a fixed 64 x 48 grid. Missing, oversized or ambiguous previews leave rows intact.
pub(crate) fn preview_confirms_fields(bytes: &[u8], d: &[u16], w: usize, h: usize, first_even: bool) -> bool {
    if bytes.len() > 32 * 1024 * 1024 {
        return false;
    }
    let Some(jpeg) = crate::preview::embedded_preview_covering(bytes, 64) else { return false };
    if lcc_meta::extract(&jpeg).orientation.is_some_and(|o| o != lcc_geom::Orientation::Normal) {
        return false;
    }
    let Ok((rgb, pw, ph, _)) = crate::tiffraw::lossy_jpeg_limited(&jpeg, 3, 2048, Some(4 * 1024 * 1024 * 3)) else { return false };
    preview_agrees(d, w, h, first_even, &rgb, pw, ph)
}

fn preview_agrees(d: &[u16], w: usize, h: usize, first_even: bool, rgb: &[u8], pw: usize, ph: usize) -> bool {
    let Some(n) = w.checked_mul(h).filter(|n| *n <= crate::MAX_SAMPLES && *n == d.len()) else { return false };
    let Some(pn) = pw.checked_mul(ph).and_then(|n| n.checked_mul(3)) else { return false };
    if n == 0 || w < 64 || h < 64 || !h.is_multiple_of(2) || pw < 64 || ph < 48 || pn != rgb.len() || pn > 4 * 1024 * 1024 * 3 {
        return false;
    }
    // No registration search: only the measured full-frame aspect, within one percent.
    let (a, b) = (w as u64 * ph as u64, h as u64 * pw as u64);
    if a.abs_diff(b) * 100 > a {
        return false;
    }
    let mut regions = [[Moments::default(); 2]; 4];
    for gy in 3..45 {
        for gx in 3..61 {
            let (x, y) = (((2 * gx + 1) * w / 128) & !1, ((2 * gy + 1) * h / 96) & !1);
            let (mut contiguous, mut interleaved, mut count) = (0.0f64, 0.0f64, 0u32);
            for yy in y.saturating_sub(4)..(y + 4).min(h) {
                let field = if yy % 2 == 0 { !first_even } else { first_even };
                let stored_y = yy / 2 + if field { h / 2 } else { 0 };
                for xx in x.saturating_sub(4)..(x + 4).min(w) {
                    let (Some(&original), Some(&proposed)) = (d.get(yy * w + xx), d.get(stored_y * w + xx)) else { return false };
                    contiguous += f64::from(original);
                    interleaved += f64::from(proposed);
                    count += 1;
                }
            }
            let (mut light, mut pixels) = (0.0f64, 0u32);
            for yy in gy * ph / 48..((gy + 1) * ph / 48).max(gy * ph / 48 + 1) {
                for xx in gx * pw / 64..((gx + 1) * pw / 64).max(gx * pw / 64 + 1) {
                    let at = 3 * (yy * pw + xx);
                    let Some([r, g, b]) = rgb.get(at..at + 3) else { return false };
                    light += (f64::from(*r) + f64::from(*g) + f64::from(*b)) / 3.0;
                    pixels += 1;
                }
            }
            let q = (light / f64::from(pixels)).ln_1p();
            let region = usize::from(gy >= 24) * 2 + usize::from(gx >= 32);
            regions[region][0].add((contiguous / f64::from(count)).ln_1p(), q);
            regions[region][1].add((interleaved / f64::from(count)).ln_1p(), q);
        }
    }
    // All four quadrants must agree, preventing one edge or a repeated half from deciding.
    // Measured real-file quadrant correlations exceed 0.86; the ordinary mild horizon
    // correlates better in its original order. Keep a substantial absolute and relative margin.
    regions.iter().all(|pair| match (pair[0].correlation(), pair[1].correlation()) {
        (Some(original), Some(proposed)) => proposed >= 0.8 && proposed >= original + 0.2,
        _ => false,
    })
}

#[derive(Clone, Copy, Default)]
struct Moments {
    n: f64,
    x: f64,
    y: f64,
    xx: f64,
    yy: f64,
    xy: f64,
}
impl Moments {
    fn add(&mut self, x: f64, y: f64) {
        self.n += 1.0;
        self.x += x;
        self.y += y;
        self.xx += x * x;
        self.yy += y * y;
        self.xy += x * y;
    }
    fn correlation(self) -> Option<f64> {
        let (vx, vy) = (self.n * self.xx - self.x * self.x, self.n * self.yy - self.y * self.y);
        // Flat or nearly flat patches cannot establish geometry.
        (vx > self.n * self.n * 1e-6 && vy > self.n * self.n * 1e-6).then(|| (self.n * self.xy - self.x * self.y) / (vx * vy).sqrt())
    }
}

/// Both Bayer fields see the same green light on neighbouring sensor rows. A normal neutral
/// scene with a strong horizon can pass the gap/seam tests, but its upper and lower halves
/// do not agree on that light. Compare the green sites, independent of the red/blue balance.
/// CYGM has no green site in one field and keeps the original detector.
fn green_fields_agree(d: &[u16], w: usize, h: usize, pattern: &[u8], first_even: bool) -> bool {
    if pattern.len() != 4 || pattern.iter().any(|c| *c > 2) {
        return true;
    }
    let (Some(g0), Some(g1)) = (pattern[..2].iter().position(|c| *c == 1), pattern[2..].iter().position(|c| *c == 1)) else { return true };
    let (a, b) = if first_even { (g0, g1) } else { (g1, g0) };
    let half = h / 2;
    let (mut difference, mut light) = (0.0, 0.0);
    // Called only after the checked dimensions, minimum size and complete raster validation.
    for y in (0..half).step_by(7) {
        for x in (0..w - 1).step_by(8) {
            let (ga, gb) = (f64::from(d[y * w + x + a]), f64::from(d[(half + y) * w + x + b]));
            difference += (ga - gb).abs();
            light += (ga + gb) * 0.5;
        }
    }
    light > 0.0 && difference <= light * 0.25
}

/// Mean absolute difference between rows `r` and `r + step`, over a column and row subsample.
fn row_gap(d: &[u16], w: usize, h: usize, step: usize) -> f64 {
    let (mut sum, mut n) = (0f64, 0u64);
    for y in (0..h - step).step_by(3) {
        for x in (0..w).step_by(5) {
            sum += (d[y * w + x] as f64 - d[(y + step) * w + x] as f64).abs();
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

/// Interleave the two fields of `samples` (`h / 2` rows each, field A first) into a `w x h` image.
pub(crate) fn deinterlace(samples: &[u16], w: usize, h: usize, first_field_even: bool) -> Option<Vec<u16>> {
    let n = w.checked_mul(h).filter(|n| *n <= crate::MAX_SAMPLES)?;
    if w == 0 || h < 2 || !h.is_multiple_of(2) || samples.len() != n {
        return None;
    }
    let half = h / 2;
    let (a, b) = samples.split_at(half * w);
    let (even, odd) = if first_field_even { (a, b) } else { (b, a) };
    let mut out = vec![0u16; n];
    for k in 0..half {
        out[2 * k * w..(2 * k + 1) * w].copy_from_slice(&even[k * w..(k + 1) * w]);
        out[(2 * k + 1) * w..(2 * k + 2) * w].copy_from_slice(&odd[k * w..(k + 1) * w]);
    }
    Some(out)
}

/// The colour-filter tag of the four-colour (CYGM) sensor: Y C / G M.
pub(crate) const CYGM_PATTERN: [u8; 4] = [5, 3, 1, 4];

const CODE_G: u8 = 1;
const CODE_C: u8 = 3;
const CODE_M: u8 = 4;
const CODE_Y: u8 = 5;

/// Linear-light RGB (interleaved, `u16`) from a CYGM mosaic, and the white level of the result.
///
/// `pattern` is the `0x828e` tag, row-major over the 2 x 2 cell at (0, 0); it must hold each of Y, C, G, M once.
pub(crate) fn cygm_to_rgb(mosaic: &[u16], w: usize, h: usize, pattern: [u8; 4]) -> Option<(Vec<u16>, f32)> {
    use rayon::prelude::*;
    let pixels = w.checked_mul(h)?;
    let output_samples = pixels.checked_mul(3).filter(|n| *n <= crate::MAX_SAMPLES)?;
    if w < 4 || h < 4 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || mosaic.len() != pixels {
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

    let mut out = vec![0u16; output_samples];
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
    fn malformed_dimensions_are_refused_before_work_or_allocation() {
        for (w, h) in [(usize::MAX, 2), (usize::MAX - 1, 64), (crate::MAX_SAMPLES, 4)] {
            assert!(!fields_stacked(&[], w, h, &[2, 1, 1, 0], true));
            assert!(deinterlace(&[], w, h, true).is_none());
            assert!(cygm_to_rgb(&[], w, h, CYGM_PATTERN).is_none());
        }
    }

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
    fn field_order_is_per_model() {
        assert!(!first_field_even("E5700"));
        assert!(!first_field_even("E5700 "));
        assert!(first_field_even("E8400") && first_field_even("E8800") && first_field_even("E5400"));
    }

    #[test]
    fn stacked_fields_are_detected_from_the_data() {
        let (w, h) = (96usize, 70usize);
        let full: Vec<u16> = (0..w * h).map(|i| 1000 + ((i / w) * 8 + (i % w) * 3) as u16).collect();
        for first_even in [true, false] {
            let stacked = {
                let mut v = Vec::new();
                let order = if first_even { [0, 1] } else { [1, 0] };
                for start in order {
                    for y in (start..h).step_by(2) {
                        v.extend_from_slice(&full[y * w..(y + 1) * w]);
                    }
                }
                v
            };
            assert!(fields_stacked(&stacked, w, h, &[2, 1, 1, 0], first_even));
            assert_eq!(deinterlace(&stacked, w, h, first_even).unwrap(), full);
        }
        assert!(!fields_stacked(&full, w, h, &[2, 1, 1, 0], true));
        // too small or odd-height frames are never stacked
        assert!(!fields_stacked(&full[..w * 8], w, 8, &[2, 1, 1, 0], true));
        assert!(!fields_stacked(&full[..w * 69], w, 69, &[2, 1, 1, 0], true));
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
    #[test]
    fn preview_evidence_rejects_ambiguous_or_mismatched_geometry() {
        let (w, h) = (160usize, 256usize);
        let d = vec![1000; w * h];
        let rgb = vec![128; w * h * 3];
        assert!(!preview_agrees(&d, w, h, true, &rgb, w, h));
        assert!(!preview_agrees(&d, w, h, true, &rgb, h, w));
        assert!(!preview_agrees(&d, w, h, true, &rgb[..rgb.len() - 1], w, h));
        assert!(!preview_agrees(&[], usize::MAX, h, true, &[], w, h));
        assert!(!preview_confirms_fields(b"invalid JPEG and TIFF", &d, w, h, true));
    }
    #[test]
    fn repeated_vertical_structure_cannot_establish_field_storage() {
        let (w, h) = (160usize, 256usize);
        let d: Vec<u16> = (0..w * h).map(|i| 1000 + (i % w) as u16 * 10).collect();
        let rgb: Vec<u8> = d.iter().flat_map(|v| [(f32::from(*v) / 4095.0).sqrt().mul_add(255.0, 0.0) as u8; 3]).collect();
        assert!(!preview_agrees(&d, w, h, true, &rgb, w, h));
    }
}
