//! Vendor raw formats (TIFF-based) and helpers shared by them.

pub mod arw;
mod arw_lens;
pub mod cr2;
pub mod cr3;
pub mod crx;
pub(crate) mod crx_wavelet;
pub mod nef;
pub mod nefc;
pub mod orf;
pub mod pef;
pub mod raf;
mod rafc;
pub mod rw2;
mod sr2;
pub mod srw;

use crate::{BlackLevel, Cfa, Rect};
use dac_tiff::Tiff;
use std::ops::Range;

/// Exif `CFAPattern`.
pub(crate) const EXIF_CFA_PATTERN: u16 = 0xa302;

/// The colour-filter layout from the Exif `CFAPattern` tag (`0xa302`: two 16-bit repeat counts, found in either
/// byte order, then one byte per site: 0 = red, 1 = green, 2 = blue), when it describes a 2×2 Bayer cell.
pub(crate) fn cfa_from_exif(tiff: &Tiff) -> Option<Cfa> {
    let &[c0, c1, r0, r1, s0, s1, s2, s3] = tiff.exif()?.bytes(EXIF_CFA_PATTERN)? else { return None };
    let two = |a: u8, b: u8| matches!((a, b), (2, 0) | (0, 2));
    let name = match [s0, s1, s2, s3] {
        [0, 1, 1, 2] => "RGGB",
        [2, 1, 1, 0] => "BGGR",
        [1, 0, 2, 1] => "GRBG",
        [1, 2, 0, 1] => "GBRG",
        _ => return None,
    };
    (two(c0, c1) && two(r0, r1)).then(|| Cfa::bayer_static(name))
}

/// Black level per 2×2 CFA position (anchored at the active area origin) from masked sensor columns `cols` over
/// rows `rows`. Falls back to 0 when the region is empty.
pub(crate) fn black_from_columns(data: &[u16], width: usize, cols: Range<usize>, rows: Range<usize>, active: Rect) -> BlackLevel {
    let height = data.len() / width.max(1);
    let (mut sum, mut n) = ([0f64; 4], [0u64; 4]);
    for y in rows.start..rows.end.min(height) {
        let py = (y as isize - active.y as isize).rem_euclid(2) as usize;
        for x in cols.start..cols.end.min(width) {
            let px = (x as isize - active.x as isize).rem_euclid(2) as usize;
            sum[py * 2 + px] += data[y * width + x] as f64;
            n[py * 2 + px] += 1;
        }
    }
    if n.contains(&0) {
        return BlackLevel::uniform(0.0);
    }
    BlackLevel { repeat_rows: 2, repeat_cols: 2, values: (0..4).map(|i| (sum[i] / n[i] as f64) as f32).collect(), delta_h: vec![], delta_v: vec![] }
}

/// White level estimate: the saturation plateau if a noticeable number of samples sit at the maximum value,
/// otherwise the full `bits` range.
pub(crate) fn white_from_data(data: &[u16], bits: u32) -> f32 {
    let full = ((1u32 << bits.clamp(1, 16)) - 1) as f32;
    // samples above the nominal range are padding / invalid (e.g. 0xFFFF fill), not saturation
    let Some(&mx) = data.iter().filter(|&&v| v as f32 <= full).max() else { return full };
    let band = (mx / 256).max(1);
    let near = mx.saturating_sub(band);
    let below = near.saturating_sub(band);
    let step = (data.len() / 2_000_000).max(1);
    let (mut top, mut under) = (0usize, 0usize);
    for &v in data.iter().step_by(step) {
        if v as f32 > full {
            continue;
        }
        if v >= near {
            top += 1;
        } else if v >= below {
            under += 1;
        }
    }
    // a saturation plateau is a spike: many more samples in the top band than in the band just below it
    if mx as f32 > full * 0.5 && top * step >= (data.len() / 20_000).max(16) && top >= 4 * under + 4 {
        // plateau: use its lower edge so everything at saturation maps to ≥ 1.0
        near as f32
    } else {
        full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_columns_by_parity() {
        // 8 wide, masked columns 0..4, active starts at x=4 (even)
        let data: Vec<u16> = (0..8 * 6)
            .map(|i| {
                let (x, y) = (i % 8, i / 8);
                if x < 4 { [100, 101, 102, 103][(y % 2) * 2 + x % 2] } else { 5000 }
            })
            .collect();
        let b = black_from_columns(&data, 8, 0..4, 0..6, Rect::new(4, 0, 4, 6));
        assert_eq!(b.values, vec![100.0, 101.0, 102.0, 103.0]);
        let b = black_from_columns(&data, 8, 0..4, 0..6, Rect::new(3, 1, 4, 5));
        assert_eq!(b.at(0, 0, 0, 1), 103.0);
        assert_eq!(black_from_columns(&data, 8, 0..0, 0..6, Rect::new(0, 0, 8, 6)), BlackLevel::uniform(0.0));
    }

    #[test]
    fn white_plateau() {
        let mut d: Vec<u16> = (0..100_000).map(|i| (i % 10000) as u16).collect();
        assert_eq!(white_from_data(&d, 14), 16383.0);
        d.extend(std::iter::repeat_n(15000u16, 1000));
        let w = white_from_data(&d, 14);
        assert!((14900.0..=15000.0).contains(&w), "{w}");
        assert_eq!(white_from_data(&[], 12), 4095.0);
    }
}
