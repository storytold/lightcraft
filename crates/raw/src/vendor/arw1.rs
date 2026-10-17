//! Sony ARW version 1: the 12-bit Huffman-coded strip of the DSLR-A100 and of the DSLR-A200, A230, A290, A300, A330,
//! A350, A380 and A390 (compression 32767 with a strip that is neither one byte nor 1.5 bytes per pixel).
//!
//! Sources: the format as described by dcraw's `sony_arw_load_raw` (read to understand the layout, per the fork's
//! raw-decoder reference policy; no code copied: this is our own implementation, fork-only), checked on CC0
//! raw.pixls.us samples (DSLR-A200 `DSC00108.ARW`):
//!
//! - One MSB-first bit stream for the whole strip, no byte stuffing.
//! - Samples are coded column by column, from the rightmost column to the leftmost; within a column the even rows
//!   come first (top to bottom), then the odd rows. The coded frame is eight rows taller than the image; those rows
//!   are decoded and dropped.
//! - Each sample is the previous sample (across columns too, starting at 0) plus a difference coded as in lossless
//!   JPEG: a Huffman-coded bit length `s`, then `s` bits, a leading 0 bit meaning a negative difference.
//! - The Huffman code is fixed, see [`CODES`]. Every decoded sample must stay in 0..4096.

use crate::{RawError, Result};

/// The fixed prefix code, in code order: `(code length, difference bit length)`. Codes are consecutive from all zeros,
/// longest first (so the two 15-bit codes are 0 and 1 and the last 2-bit code is `11`); the lengths fill the code
/// space exactly.
const CODES: [(u8, u8); 18] = [
    (15, 17),
    (15, 16),
    (14, 15),
    (13, 14),
    (12, 13),
    (11, 12),
    (10, 11),
    (9, 10),
    (8, 9),
    (7, 8),
    (6, 7),
    (5, 6),
    (4, 5),
    (3, 4),
    (3, 3),
    (3, 0),
    (2, 2),
    (2, 1),
];

const PEEK: u32 = 15;

/// 2^15-entry lookup by the next 15 bits: `(code length, difference length)`.
fn lookup() -> Vec<(u8, u8)> {
    let mut table = Vec::with_capacity(1 << PEEK);
    for &(len, diff) in &CODES {
        table.extend(std::iter::repeat_n((len, diff), 1 << (PEEK - u32::from(len))));
    }
    table
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl Bits<'_> {
    /// Keep at least 32 bits in the accumulator; past the end, zeros (callers check [`Bits::overrun`]).
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = self.src.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc |= u64::from(b) << (56 - self.n);
            self.n += 8;
        }
    }
    fn peek(&mut self, k: u32) -> u32 {
        self.fill();
        (self.acc >> (64 - k)) as u32
    }
    fn skip(&mut self, k: u32) {
        self.acc <<= k;
        self.n -= k;
    }
    /// More bits consumed than the strip holds (with a few bytes of slack for the look-ahead).
    fn overrun(&self) -> bool {
        self.pos.saturating_sub(self.n as usize / 8) > self.src.len()
    }
}

/// Decode a `w` × `h` image from an ARW1 strip (coded as `h + 8` rows).
pub(crate) fn decode(src: &[u8], w: usize, h: usize) -> Result<Vec<u16>> {
    let n = w.checked_mul(h).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let coded_h = h + 8;
    let table = lookup();
    let mut cols = vec![0u16; n];
    let mut bits = Bits { src, pos: 0, acc: 0, n: 0 };
    let mut sum: i32 = 0;
    for x in (0..w).rev() {
        for y in (0..coded_h).step_by(2).chain((1..coded_h).step_by(2)) {
            let (len, s) = table.get(bits.peek(PEEK) as usize).copied().unwrap_or((PEEK as u8, 17));
            bits.skip(u32::from(len));
            let diff = match s {
                0 => 0,
                1..=15 => {
                    let v = bits.peek(u32::from(s)) as i32;
                    bits.skip(u32::from(s));
                    if v < 1 << (s - 1) { v - (1 << s) + 1 } else { v }
                }
                _ => return Err(RawError::Corrupt("ARW1: invalid difference length".into())),
            };
            sum += diff;
            if !(0..4096).contains(&sum) {
                return Err(RawError::Corrupt("ARW1: sample out of the 12-bit range".into()));
            }
            // column-major while decoding (contiguous writes); transposed below
            if y < h
                && let Some(o) = cols.get_mut(x * h + y)
            {
                *o = sum as u16;
            }
        }
        if bits.overrun() {
            return Err(RawError::Corrupt("ARW1: strip ends before the image".into()));
        }
    }
    let mut out = vec![0u16; n];
    const TILE: usize = 64;
    for x0 in (0..w).step_by(TILE) {
        for y0 in (0..h).step_by(TILE) {
            for x in x0..(x0 + TILE).min(w) {
                for y in y0..(y0 + TILE).min(h) {
                    if let (Some(o), Some(&v)) = (out.get_mut(y * w + x), cols.get(x * h + y)) {
                        *o = v;
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode samples in the ARW1 order with [`CODES`] (test-only encoder).
    fn encode(samples: &[u16], w: usize, h: usize) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let put = |v: u32, k: u32, bits: &mut Vec<bool>| (0..k).rev().for_each(|i| bits.push(v >> i & 1 == 1));
        let code_of = |s: u8| {
            let mut code = 0u32;
            for &(len, d) in &CODES {
                if d == s {
                    return (code >> (PEEK - u32::from(len)), u32::from(len));
                }
                code += 1 << (PEEK - u32::from(len));
            }
            panic!("no code for {s}")
        };
        let mut prev = 0i32;
        for x in (0..w).rev() {
            for y in (0..h + 8).step_by(2).chain((1..h + 8).step_by(2)) {
                let v = if y < h { i32::from(samples[y * w + x]) } else { prev };
                let d = v - prev;
                prev = v;
                let s = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() } as u8;
                let (c, l) = code_of(s);
                put(c, l, &mut bits);
                if s > 0 {
                    let m = if d < 0 { (d + (1 << s) - 1) as u32 } else { d as u32 };
                    put(m, u32::from(s), &mut bits);
                }
            }
        }
        bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, b)| a | (u8::from(*b) << (7 - i)))).collect()
    }

    #[test]
    fn code_space_is_full() {
        assert_eq!(CODES.iter().map(|(l, _)| 1u32 << (PEEK - u32::from(*l))).sum::<u32>(), 1 << PEEK);
        assert_eq!(lookup().len(), 1 << PEEK);
    }

    #[test]
    fn round_trips() {
        let (w, h) = (7, 6);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 977 + 13) % 4096) as u16).collect();
        assert_eq!(decode(&encode(&samples, w, h), w, h).unwrap(), samples);
    }

    #[test]
    fn short_or_garbage_strips_are_errors() {
        let (w, h) = (16, 8);
        let samples: Vec<u16> = (0..w * h).map(|i| (i * 31 % 4096) as u16).collect();
        let full = encode(&samples, w, h);
        assert!(decode(&full[..full.len() / 2], w, h).is_err());
        assert!(decode(&[], w, h).is_err());
        let _ = decode(&[0xff; 64], w, h);
        let _ = decode(&[0x00; 64], w, h);
    }
}
