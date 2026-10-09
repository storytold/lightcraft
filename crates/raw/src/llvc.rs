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
        fn random_bits_never_panic(src in proptest::collection::vec(proptest::num::u8::ANY, 0..256), width in 1usize..300) {
            let mut b = Bits::new(&src);
            let _ = vld_decode_line(&mut b, src.len() * 8, width);
        }
    }
}
