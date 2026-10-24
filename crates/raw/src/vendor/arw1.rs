//! Sony ARW version 1 (DSLR-A200 to A390, 2008-2009): Compression 32767 with a strip that is neither one byte per
//! pixel (ARW2) nor 1.5 bytes per pixel (packed 12-bit).
//!
//! The strip is one most-significant-bit-first bit stream of lossless-JPEG-style differences (a Huffman code giving
//! the bit length of the difference, then that many bits, sign-extended the JPEG way). One running 12-bit predictor
//! spans the whole frame, which is coded column by column from the right edge to the left; within a column the even
//! rows come first, top to bottom, then the odd rows. The coded frame is eight rows taller than the image IFD's
//! height; the extra rows are dropped.
//!
//! The code is canonical over 15 bits; [`CODES`] lists (code length, difference bits) in code order, the first entry
//! taking the all-zero code. Verified on the CC0 raw.pixls.us samples of the DSLR-A200, A230, A290, A300, A330, A350,
//! A380 and A390: each decodes to the scene of its embedded preview, every sum stays within 12 bits and the stream ends inside
//! the strip.

use crate::{RawError, Result};

/// Extra coded rows below the image.
pub(crate) const EXTRA_ROWS: usize = 8;

/// (code length, difference bit count), in increasing code order.
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

/// The fewest bits one coded sample can take: the shortest codes plus their difference bits ((2, 1) and (3, 0)).
const MIN_BITS: u64 = 3;

/// 15-bit lookup: (code length, difference bits) for every possible next 15 bits.
fn table() -> Vec<(u8, u8)> {
    let mut t = Vec::with_capacity(1 << PEEK);
    for (len, bits) in CODES {
        t.extend(std::iter::repeat_n((len, bits), 1 << (PEEK - u32::from(len))));
    }
    t
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl Bits<'_> {
    /// Keep at least 32 bits buffered; past the end it shifts in zeros (the caller checks [`Self::overrun`]).
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
    fn overrun(&self) -> bool {
        // bytes consumed beyond the buffered bits
        self.pos.saturating_sub(self.n as usize / 8) > self.src.len()
    }
}

/// Whether a `strip_len`-byte strip can hold a coded `w` × `h` frame: every coded sample, [`EXTRA_ROWS`] included, takes
/// at least [`MIN_BITS`]. Cheap, so the header decode checks it too (a file of another layout fails at import, not at
/// its first render).
pub(crate) fn check_strip(strip_len: u64, w: usize, h: usize) -> Result<()> {
    let coded = (w as u64).saturating_mul((h as u64).saturating_add(EXTRA_ROWS as u64));
    if strip_len.saturating_mul(8) < coded.saturating_mul(MIN_BITS) {
        return Err(RawError::Corrupt("Sony ARW v1: raw strip too short for the frame".into()));
    }
    Ok(())
}

/// Decode a `w` × `h` frame (the image IFD's size; [`EXTRA_ROWS`] more are coded and dropped) into 12-bit samples.
pub(crate) fn decode(src: &[u8], w: usize, h: usize) -> Result<Vec<u16>> {
    let coded_h = h.checked_add(EXTRA_ROWS).ok_or(RawError::Limit("image too large"))?;
    let len = w.checked_mul(h).ok_or(RawError::Limit("image too large"))?;
    // refused before the buffer exists
    check_strip(src.len() as u64, w, h)?;
    let mut out = vec![0u16; len];
    let lut = table();
    let mut bits = Bits { src, pos: 0, acc: 0, n: 0 };
    let mut sum: i32 = 0;
    let rows = (0..coded_h).step_by(2).chain((1..coded_h).step_by(2));
    let rows: Vec<usize> = rows.collect();
    for x in (0..w).rev() {
        for &y in &rows {
            let (code_len, diff_bits) = lut.get(bits.peek(PEEK) as usize).copied().unwrap_or((0, 0));
            if code_len == 0 || diff_bits > 15 {
                return Err(RawError::Corrupt("Sony ARW v1: invalid difference code".into()));
            }
            bits.skip(u32::from(code_len));
            let diff = if diff_bits == 0 {
                0
            } else {
                let k = u32::from(diff_bits);
                let v = bits.peek(k) as i32;
                bits.skip(k);
                if v >> (k - 1) == 0 { v - ((1 << k) - 1) } else { v }
            };
            sum = sum.saturating_add(diff);
            if y < h
                && let Some(px) = out.get_mut(y * w + x)
            {
                *px = sum.clamp(0, 4095) as u16;
            }
        }
        if bits.overrun() {
            return Err(RawError::Corrupt("Sony ARW v1: raw strip ends before the image".into()));
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Bit writer for the test encoder.
    struct Writer {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl Writer {
        fn put(&mut self, v: u32, k: u32) {
            if k == 0 {
                return;
            }
            self.acc = (self.acc << k) | u64::from(v & ((1u32 << k) - 1));
            self.n += k;
            while self.n >= 8 {
                self.out.push((self.acc >> (self.n - 8)) as u8);
                self.n -= 8;
            }
        }
    }

    /// Encode `w` × `h` 12-bit samples (rows past `h` repeat the last sum) in the ARW v1 layout.
    pub(crate) fn encode(samples: &[u16], w: usize, h: usize) -> Vec<u8> {
        // canonical codes in CODES order
        let mut codes = Vec::new();
        let mut code = 0u32;
        for (len, bits) in CODES {
            codes.push((bits, code >> (PEEK - u32::from(len)), u32::from(len)));
            code += 1 << (PEEK - u32::from(len));
        }
        let mut wr = Writer { out: Vec::new(), acc: 0, n: 0 };
        let coded_h = h + EXTRA_ROWS;
        let rows: Vec<usize> = (0..coded_h).step_by(2).chain((1..coded_h).step_by(2)).collect();
        let mut prev = 0i32;
        for x in (0..w).rev() {
            for &y in &rows {
                let v = if y < h { i32::from(samples[y * w + x]) } else { prev };
                let d = v - prev;
                prev = v;
                let k = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
                let &(_, c, l) = codes.iter().find(|c| u32::from(c.0) == k).unwrap();
                wr.put(c, l);
                let bits = if d < 0 { (d + (1 << k) - 1) as u32 } else { d as u32 };
                wr.put(bits, k);
            }
        }
        wr.put(0, 7);
        wr.out.extend([0u8; 8]);
        wr.out
    }

    #[test]
    fn codes_fill_the_15_bit_space() {
        assert_eq!(table().len(), 1 << PEEK);
    }

    #[test]
    fn round_trips_the_column_interleaved_layout() {
        let (w, h) = (6, 5);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 977 + 13) % 4096) as u16).collect();
        let strip = encode(&samples, w, h);
        assert_eq!(decode(&strip, w, h).unwrap(), samples);
    }

    #[test]
    fn hostile_strips_error_instead_of_panicking() {
        // truncated stream
        let samples: Vec<u16> = (0..64).map(|i| (i * 61 % 4096) as u16).collect();
        let strip = encode(&samples, 8, 8);
        assert!(decode(&strip[..strip.len() / 3], 8, 8).is_err());
        // all-zero bits: the all-zero code is a 17-bit difference
        assert!(decode(&[0u8; 64], 8, 8).is_err());
        // all-one bits: 1-bit differences that run the sum out of range are clamped, the strip runs out
        assert!(decode(&[0xffu8; 4], 8, 8).is_err());
        assert!(decode(&[], 0, 0).unwrap().is_empty());
    }
}
