//! RDD 34:2015 LLVC codec core as Sony's ARW6 uses it; see docs/arw6-compression.md.
//!
//! Clean-room: written from SMPTE RDD 34:2015 §6.3 (Figures 6.5-6.8) and checked against its worked
//! example (Figure 6.9) and Phase 0 oracle vectors. No other raw decoder's source was consulted.

// Consumed by the dequantiser/wavelet/ARW6 container tasks that follow.
#![allow(dead_code)]

use crate::vendor::pef::Bits;
use crate::{RawError, Result};

/// Largest coefficient bit depth (DPT) a stream may select; keeps `Bits::get` in range.
pub(crate) const MAX_DPT: u32 = 24;

fn corrupt(why: &str) -> RawError {
    RawError::Corrupt(format!("ARW6 VLD: {why}"))
}

/// One bit, failing when the reader has run past the `source_bits` of its half.
fn bit(bits: &mut Bits<'_>, source_bits: usize) -> Result<u32> {
    let b = bits.get(1);
    if bits.consumed_bits() > source_bits {
        return Err(corrupt("past the end of its half"));
    }
    Ok(b)
}

/// Count zero bits up to and including the terminating 1 (the `0^n 1` codes), bounded by `MAX_DPT`.
fn zeros_then_one(bits: &mut Bits<'_>, source_bits: usize) -> Result<u32> {
    let mut n = 0;
    while bit(bits, source_bits)? == 0 {
        n += 1;
        if n > MAX_DPT {
            return Err(corrupt("DPT code exceeds MAX_DPT"));
        }
    }
    Ok(n)
}

/// Decode one coefficient line of `width.div_ceil(4)` sets (RDD 34 §6.3.6) and return exactly `width` values.
pub(crate) fn vld_decode_line(bits: &mut Bits<'_>, source_bits: usize, width: usize) -> Result<Vec<i32>> {
    if width == 0 {
        return Ok(Vec::new()); // no sets: the state machine below would never terminate
    }
    let sets = width.div_ceil(4);
    let (mut abs, mut dpt, mut cnt, mut dpts) = ([0u32; 4], 0u32, sets, 0u8);
    let (mut abs_v, mut abs_e, mut dpt_v, mut dpt_e, mut zr) = (false, false, true, false, 0u64);
    let mut out: Vec<i32> = Vec::new();
    loop {
        // DPT value decoding (Figure 6.6).
        if cnt == 0 {
            dpt_v = false;
        }
        if cnt == 1 {
            dpt_e = true;
        }
        cnt = cnt.saturating_sub(1);
        zr = zr.saturating_sub(1);
        if dpt_v {
            match dpts {
                0 => {
                    if bit(bits, source_bits)? == 0 {
                        // Sony deviation 1, oracle-verified in Phase 0: a zero code at DPT 0 is a zero set entering run mode.
                        if dpt == 0 {
                            dpts = 1;
                        }
                    } else if bit(bits, source_bits)? == 0 {
                        // "1 0 0^n 1": DPT up by n+1.
                        dpt = dpt.saturating_add(zeros_then_one(bits, source_bits)? + 1);
                    } else {
                        // "1 1 0^n 1" (DPT down by n+1) or "1 1 0^(DPT-1)" (DPT to 0).
                        let mut n = 0;
                        while n + 1 < dpt && bit(bits, source_bits)? == 0 {
                            n += 1;
                        }
                        if n + 1 >= dpt {
                            dpt = 0;
                            dpts = 1;
                        } else {
                            dpt -= n + 1;
                        }
                    }
                }
                1 => {
                    if bit(bits, source_bits)? == 1 {
                        // "1 0^n 1": leave run mode at DPT n+1.
                        dpt = zeros_then_one(bits, source_bits)? + 1;
                        dpts = 0;
                    } else {
                        // Run code of ceil(log2(Cnt+2)) bits (Figure 6.6).
                        let lim = (cnt as u64 + 2).next_power_of_two().trailing_zeros();
                        if lim > 32 {
                            return Err(corrupt("line too wide"));
                        }
                        let mut m = 1;
                        while m < lim && bit(bits, source_bits)? == 0 {
                            m += 1;
                        }
                        if m == lim {
                            // "0^lim": the rest of the line is zero.
                            zr = cnt as u64 + 2;
                        } else {
                            zr = 1;
                            for _ in 0..m {
                                zr = (zr << 1) | bit(bits, source_bits)? as u64;
                            }
                        }
                        dpts = 2;
                    }
                }
                _ => {
                    // Sony deviation 2, oracle-verified in Phase 0: the run ends at ZR == 1 (RDD 34 says 0).
                    if zr == 1 {
                        dpt = zeros_then_one(bits, source_bits)? + 1;
                        dpts = 0;
                    }
                }
            }
            if dpt > MAX_DPT {
                return Err(corrupt("DPT exceeds MAX_DPT"));
            }
        }
        // SGN value decoding (Figure 6.7): signs of the previous set.
        if abs_v {
            for v in abs {
                let v = v as i32;
                out.push(if v != 0 && bit(bits, source_bits)? == 1 { -v } else { v });
            }
        }
        if abs_e {
            out.truncate(width);
            return Ok(out);
        }
        abs_v = dpt_v;
        abs_e = dpt_e;
        // ABS value decoding (Figure 6.8).
        if dpt_v {
            for a in abs.iter_mut() {
                *a = bits.get(dpt);
            }
            if bits.consumed_bits() > source_bits {
                return Err(corrupt("past the end of its half"));
            }
        }
    }
}

/// Parity dequantiser, Phase 0 (oracle-verified): 0 -> 0, qi = 0 -> q, else
/// `sign(q) * (((2|q| + 1) << (qi - 1)) - (|q| & 1))`. Saturates instead of overflowing on hostile input.
pub(crate) fn dequant(q: i32, qi: u32) -> i32 {
    if q == 0 || qi == 0 {
        return q;
    }
    let a = i64::from(q).abs();
    let m = (((2 * a + 1) << (qi - 1).min(30)) - (a & 1)).min(i64::from(i32::MAX));
    (if q < 0 { -m } else { m }) as i32
}

/// A row-major plane of integer samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<i32>,
}

impl Plane {
    pub fn zeros(width: usize, height: usize) -> Plane {
        Plane { width, height, data: vec![0; width.saturating_mul(height)] }
    }
    pub fn row(&self, y: usize) -> Option<&[i32]> {
        let start = y.checked_mul(self.width)?;
        self.data.get(start..start.checked_add(self.width)?)
    }
    fn ok(&self) -> bool {
        self.width.checked_mul(self.height) == Some(self.data.len())
    }
}

/// `(low, high)` sample counts of an `n`-sample signal whose low-pass samples sit at indices = `phase` (mod 2).
pub(crate) fn band_rows(n: usize, phase: u8) -> (usize, usize) {
    let low = (n + 1).saturating_sub(usize::from(phase)) / 2;
    (low, n - low)
}

/// `v[i]` with `i` clamped into range (the whole-sample symmetric extension of the band); 0 for an empty band.
fn at(v: &[i32], i: isize) -> i32 {
    let last = v.len() as isize - 1;
    if last < 0 {
        return 0;
    }
    v.get(i.clamp(0, last) as usize).copied().unwrap_or(0)
}

/// 1-D inverse 5/3 lifting, RDD 34 §6.5, with X[-1] := X[1], X[n] := X[n-2] (see `band_rows` for the sizes).
fn inverse_1d(l: &[i32], h: &[i32], phase: u8) -> Result<Vec<i32>> {
    let (n, p) = (l.len() + h.len(), isize::from(phase));
    if band_rows(n, phase) != (l.len(), h.len()) || phase > 1 {
        return Err(RawError::Corrupt("ARW6 wavelet: band sizes do not match".into()));
    }
    let even: Vec<i32> = l.iter().enumerate().map(|(j, &lv)| lv - ((at(h, j as isize - 1 + p) + at(h, j as isize + p) + 2) >> 2)).collect();
    let mut x = vec![0; n];
    let (lo, hi) = (usize::from(phase), 1 - usize::from(phase));
    for (xv, &e) in x.iter_mut().skip(lo).step_by(2).zip(&even) {
        *xv = e;
    }
    for (k, (xv, &hv)) in x.iter_mut().skip(hi).step_by(2).zip(h).enumerate() {
        *xv = hv + ((at(&even, k as isize - p) + at(&even, k as isize + 1 - p)) >> 1);
    }
    Ok(x)
}

/// Gathers the lines of `a` along one axis (columns when `vertical`), applies `f`, and scatters back.
fn lines(low: &Plane, high: &Plane, phase: u8, vertical: bool) -> Result<Plane> {
    if !low.ok() || !high.ok() {
        return Err(RawError::Corrupt("ARW6 wavelet: plane size mismatch".into()));
    }
    let (across_l, across_h) = if vertical { (low.width, high.width) } else { (low.height, high.height) };
    if across_l != across_h {
        return Err(RawError::Corrupt("ARW6 wavelet: band sizes do not match".into()));
    }
    let (nl, nh) = if vertical { (low.height, high.height) } else { (low.width, high.width) };
    let n = nl + nh;
    let (w, h) = if vertical { (across_l, n) } else { (n, across_l) };
    let mut out = Plane::zeros(w, h);
    let get = |p: &Plane, i: usize| -> Vec<i32> {
        if vertical { p.data.iter().skip(i).step_by(p.width.max(1)).copied().collect() } else { p.row(i).map(<[i32]>::to_vec).unwrap_or_default() }
    };
    for i in 0..across_l {
        let x = inverse_1d(&get(low, i), &get(high, i), phase)?;
        if vertical {
            for (o, v) in out.data.iter_mut().skip(i).step_by(w.max(1)).zip(x) {
                *o = v;
            }
        } else if let Some(r) = out.data.get_mut(i * w..(i + 1) * w) {
            r.copy_from_slice(&x);
        }
    }
    Ok(out)
}

/// Vertical inverse 5/3 (along rows of samples): `low`/`high` are stacked bands of equal width.
pub(crate) fn inverse_53_1d_rows(low: &Plane, high: &Plane, phase: u8) -> Result<Plane> {
    lines(low, high, phase, true)
}

/// Horizontal inverse 5/3: `low`/`high` are side-by-side bands of equal height.
pub(crate) fn inverse_53_1d_cols(low: &Plane, high: &Plane, phase: u8) -> Result<Plane> {
    lines(low, high, phase, false)
}

/// 2-D inverse, RDD 34 §6.5 order: vertical IWT (phase `vphase`) on (ll, lh) and (hl, hh), then horizontal (phase 0).
pub(crate) fn inverse_53_2d(ll: &Plane, lh: &Plane, hl: &Plane, hh: &Plane, vphase: u8) -> Result<Plane> {
    let lo = inverse_53_1d_rows(ll, lh, vphase)?;
    let hi = inverse_53_1d_rows(hl, hh, vphase)?;
    inverse_53_1d_cols(&lo, &hi, 0)
}

/// The ten bands of a 3-level decomposition (LL only at the deepest level).
pub(crate) struct Bands3 {
    pub ll3: Plane,
    pub hl3: Plane,
    pub lh3: Plane,
    pub hh3: Plane,
    pub hl2: Plane,
    pub lh2: Plane,
    pub hh2: Plane,
    pub hl1: Plane,
    pub lh1: Plane,
    pub hh1: Plane,
}

/// Full inverse: level 3 uses `phases[2]`, level 2 `phases[1]`, level 1 `phases[0]`.
pub(crate) fn reconstruct3(b: &Bands3, phases: [u8; 3]) -> Result<Plane> {
    let ll2 = inverse_53_2d(&b.ll3, &b.lh3, &b.hl3, &b.hh3, phases[2])?;
    let ll1 = inverse_53_2d(&ll2, &b.lh2, &b.hl2, &b.hh2, phases[1])?;
    inverse_53_2d(&ll1, &b.lh1, &b.hl1, &b.hh1, phases[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::pef::Bits;

    fn bits_of(s: &str) -> Vec<u8> {
        let mut out = vec![0u8; s.len().div_ceil(8)];
        for (i, c) in s.bytes().enumerate() {
            if c == b'1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }
    fn line(s: &str, width: usize) -> (Vec<i32>, usize) {
        let src = bits_of(s);
        let mut b = Bits::new(&src);
        let v = vld_decode_line(&mut b, src.len() * 8, width).unwrap();
        (v, b.consumed_bits())
    }

    #[test]
    fn rdd34_figure_6_9_worked_example() {
        // RDD 34:2015 §6.3.6, Figure 6.9: 8 sets, 91 bits
        let s = "1000010110000010000100110000101000010100110100000000000011000010010001001010011100110000000";
        let (v, used) = line(s, 32);
        assert_eq!(v, vec![6, 0, -8, 4, 0, 0, 0, 0, -9, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(used, 91);
    }
    #[test]
    fn sony_zero_at_dpt0_enters_run_state() {
        // Phase 0 vector "dev1": a "0" at DPT 0 is a zero set that moves to DPTs = 1; the next set uses the "1 0^n 1" code
        let (v, used) = line("01001101000011001010", 8);
        assert_eq!(v, vec![0, 0, 0, 0, 5, 0, -3, 1]);
        assert_eq!(used, 20);
    }
    #[test]
    fn sony_run_value_means_minus_one_zero_sets() {
        // Phase 0 vector "dev2": run code value 3 = two more zero sets, then "0^n 1" post-run DPT
        let (v, used) = line("001101100000000", 16);
        assert_eq!(v, vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(used, 15);
    }
    #[test]
    fn end_of_line_zero_code() {
        let (v, used) = line("1001110100101100100", 12);
        assert_eq!(v, vec![3, -1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(used, 19);
    }
    #[test]
    fn dpt_moves_up_down_and_to_zero() {
        let (v, used) = line("1011000100101111111110011101001011111100000", 20);
        assert_eq!(v, vec![1, 0, 0, 0, 7, 7, -7, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(used, 43);
    }
    #[test]
    fn all_zero_line_is_two_bits_and_width_truncates() {
        let (v, used) = line("00", 6); // L = 2 sets, width 6 keeps 6 of 8 values
        assert_eq!(v, vec![0; 6]);
        assert_eq!(used, 2);
    }
    #[test]
    fn width_zero_returns_empty_line() {
        let (v, used) = line("1111", 0);
        assert!(v.is_empty());
        assert_eq!(used, 0);
    }
    #[test]
    fn hostile_input_errors_instead_of_panicking() {
        let mut climb = vec![0u8; 64];
        climb[0] = 0x80; // "1 0 0^n 1" with n > MAX_DPT
        let mut b = Bits::new(&climb);
        assert!(matches!(vld_decode_line(&mut b, 512, 16), Err(RawError::Corrupt(_))));
        let short = bits_of("10"); // needs more bits than it has
        let mut b = Bits::new(&short);
        assert!(matches!(vld_decode_line(&mut b, 2, 16), Err(RawError::Corrupt(_))));
    }
    proptest::proptest! {
        #[test]
        fn random_bits_never_panic(src in proptest::collection::vec(proptest::num::u8::ANY, 0..256), width in 0usize..300) {
            let mut b = Bits::new(&src);
            let _ = vld_decode_line(&mut b, src.len() * 8, width);
        }
    }

    fn col(v: &[i32]) -> Plane {
        Plane { width: 1, height: v.len(), data: v.to_vec() }
    }
    #[test]
    fn dequant_parity_rule() {
        for (qi, table) in
            [(0u32, [-3, -2, -1, 0, 1, 2, 3]), (1, [-6, -5, -2, 0, 2, 5, 6]), (2, [-13, -10, -5, 0, 5, 10, 13]), (3, [-27, -20, -11, 0, 11, 20, 27])]
        {
            for (q, v) in (-3..=3).zip(table) {
                assert_eq!(dequant(q, qi), v, "q={q} qi={qi}");
            }
        }
    }
    #[test]
    fn quantize_inverts_dequant() {
        for qi in 0..6 {
            for q in -50..=50 {
                assert_eq!(crate::vendor::arw6_testenc::quantize(dequant(q, qi), qi), q, "q={q} qi={qi}");
            }
        }
    }
    #[test]
    fn dequant_does_not_overflow() {
        assert_eq!(dequant(i32::MAX, 31), i32::MAX);
        assert_eq!(dequant(i32::MIN, 40), -i32::MAX);
    }
    #[test]
    fn band_rows_counts() {
        assert_eq!(band_rows(1093, 0), (547, 546));
        assert_eq!(band_rows(1093, 1), (546, 547));
        assert_eq!(band_rows(1668, 0), (834, 834));
        assert_eq!(band_rows(5, 1), (2, 3));
        assert_eq!(band_rows(1, 1), (0, 1));
    }
    #[test]
    fn inverse_53_hand_vectors() {
        // computed by hand from RDD 34 §6.5 with X[-1] := X[1], X[n] := X[n-2]; checked against the Phase 0 model
        assert_eq!(inverse_53_1d_rows(&col(&[10, 20]), &col(&[3, -4]), 0).unwrap().data, vec![8, 17, 20, 16]);
        assert_eq!(inverse_53_1d_rows(&col(&[10, 20]), &col(&[3, -4]), 1).unwrap().data, vec![13, 10, 12, 22]);
        assert_eq!(inverse_53_1d_rows(&col(&[10, 20, 30]), &col(&[3, -4]), 0).unwrap().data, vec![8, 17, 20, 22, 32]);
        assert_eq!(inverse_53_1d_rows(&col(&[3, -4]), &col(&[10, 20, 30]), 1).unwrap().data, vec![5, -5, 9, -17, 13]);
    }
    #[test]
    fn inverse_53_rejects_mismatched_bands() {
        assert!(inverse_53_1d_rows(&col(&[1, 2, 3]), &col(&[1]), 0).is_err());
        assert!(inverse_53_1d_rows(&col(&[1, 2]), &Plane::zeros(2, 2), 0).is_err());
        assert!(inverse_53_1d_cols(&col(&[1, 2]), &col(&[1, 2, 3]), 0).is_err());
    }
    proptest::proptest! {
        #[test]
        fn forward_then_inverse_is_identity(w in 1usize..24, h in 1usize..40, p1 in 0u8..2, p2 in 0u8..2, p3 in 0u8..2, seed in proptest::num::u64::ANY) {
            use crate::vendor::arw6_testenc::{forward3, forward_53_2d};
            use proptest::prop_assert_eq;
            let mut s = seed | 1;
            let mut next = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s % 8191) as i32 - 4095 };
            let x = Plane { width: w, height: h, data: (0..w * h).map(|_| next()).collect() };
            let (ll, lh, hl, hh) = forward_53_2d(&x, p1);
            prop_assert_eq!(inverse_53_2d(&ll, &lh, &hl, &hh, p1).unwrap(), x.clone());
            if w >= 8 && h >= 8 {
                prop_assert_eq!(reconstruct3(&forward3(&x, [p1, p2, p3]), [p1, p2, p3]).unwrap(), x);
            }
        }
    }
}
