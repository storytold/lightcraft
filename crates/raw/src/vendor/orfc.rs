//! Olympus / OM System compressed ORF image data (12-bit; found in every interchangeable-lens body sampled from
//! the E-410 to the OM-1, and in the TG-6).
//!
//! **Provenance: this is not clean-room in the sense of `nefc.rs`.** No public prose description of this bit stream
//! exists. The module was written by an AI model (Claude), which did not derive the format independently: the
//! first coder it tried already reproduced the reference image exactly, so the structure was recalled from
//! training data — most likely the open-source decoders of the dcraw family — and only confirmed by measurement.
//! No decoder source was opened, searched for or fetched while writing it, so nothing was transcribed from a file
//! at hand; but the author cannot rule out that the code resembles the implementations it learned the format from.
//! Whether that is acceptable under the project's clean-room rule is for the maintainers to decide. What was done:
//!
//! - Reference pixels: LibRaw's `unprocessed_raw` binary (0.21.4) run as a black box on CC0 samples from
//!   raw.pixls.us. Its output was used for comparison only and is not in the repository.
//! - Container facts, measured on 12 files from 11 bodies (E-410, E-620, E-P1, E-M5, E-M1, E-M1 II incl. a 50 MP
//!   High Res Shot, E-M1X, E-M10 III, PEN-F, TG-6, OM-1): TIFF compression 1, 16 bits per sample declared, one
//!   strip of 6.0–8.3 bits per pixel that starts with the seven bytes `00 00 00 00 01 00 00`; the samples are
//!   12-bit, and the stream ends within the last byte of the strip (0–7 unused bits).
//! - Each element of the coder was checked by replacing it with an alternative and decoding the first 24 rows of
//!   two files (E-M1, E-M10 III; 111 360 pixels each): the format as described gives no mismatch against the
//!   reference, while starting the stream at byte 8, keeping the state across rows, a fixed number of verbatim
//!   bits, dropping the running bias, or predicting from the left pixel or from the left/up mean alone each give
//!   more than 98 000.
//! - The decoder reproduces the reference bit for bit on all 12 files, wherever the reference has pixels: on four
//!   of them (E-620, E-P1, TG-6, the High Res Shot) the reference leaves 4–26 margin columns at the right edge
//!   blank, which can't be compared. `corpus_orf_*` in `tests/corpus.rs` keep what can be checked without it.
//!
//! Format, as established:
//! - Seven header bytes, then one MSB-first bit stream, no byte stuffing, continuous across rows.
//! - The two colours of a row (even and odd columns) are coded independently. Each keeps three numbers, zero at
//!   the start of every row: the previous magnitude `m`, a running bias `b` and the count `s` of consecutive
//!   magnitudes of at most 16.
//! - Per pixel: `e` is 2 while `s < 3`, else 0; the number of verbatim bits `k` is the smallest value of at least
//!   `2 + e` for which `m >> (k + e)` is zero. The stream then holds a sign bit; the two low bits of the sample
//!   difference; a unary number `q` (`q` zero bits and a one, `q ≤ 11`) or, after twelve zero bits, `15 − k` bits
//!   of `q` and one unused bit; and `k` bits `r`. The new magnitude is `m = q·2^k + r`, the signed value `m` or
//!   `−m − 1`, the difference `d` that value plus `b`, and then `b = (3d + b) >> 5`.
//! - The sample is `prediction + 4d + low bits`. The prediction uses the same-colour neighbours two pixels to the
//!   left (`W`), above (`N`) and above-left (`NW`): in the first two rows `W`, in the first two columns `N` (0 where
//!   neither exists); elsewhere, if `NW` lies strictly between `W` and `N`, `W + N − NW` when either differs from
//!   `NW` by more than 32 and their mean otherwise; else `W` if `|W − NW| > |N − NW|`, else `N`.
//!
//! Not supported (returned as [`RawError::Unsupported`] by `orf.rs`, the embedded preview is used instead):
//! - The 14-bit High Res Shot files of the OM-1 Mark II, whose strip starts with `00 00 00 00 00 01 00 00`.
//!   Not analysed.

use super::pef::Bits;
use crate::{RawError, Result};

/// The bytes every compressed strip starts with.
const HEADER: [u8; 7] = [0, 0, 0, 0, 1, 0, 0];
/// Sample range.
const MAX: i32 = 4095;
/// Shortest code of a pixel: sign, two low bits, a one-bit quotient and two verbatim bits.
const MIN_BITS_PER_PIXEL: usize = 6;

/// Whether `src` is a strip this module decodes.
pub(crate) fn is_compressed(src: &[u8]) -> bool {
    src.starts_with(&HEADER)
}

/// Coder state of one colour of a row.
#[derive(Clone, Copy, Default)]
struct Phase {
    /// Previous magnitude.
    magnitude: u32,
    bias: i32,
    /// Consecutive magnitudes of at most 16.
    small: u32,
    /// Same-colour sample to the left and the one above it.
    left: i32,
    up_left: i32,
}

/// Decode the `w × h` compressed strip `src`.
pub(crate) fn decode(src: &[u8], w: usize, h: usize) -> Result<Vec<u16>> {
    let n = w.checked_mul(h).filter(|&n| n > 0 && n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("ORF image size"))?;
    let body = src.strip_prefix(&HEADER).ok_or_else(|| RawError::Unsupported("Olympus compressed ORF with an unknown header".into()))?;
    // a strip that can't hold the image is truncated (this also bounds the allocation by the file size)
    if n / 8 * MIN_BITS_PER_PIXEL > body.len() {
        return Err(RawError::Corrupt(format!("ORF: {} bytes of compressed data for {w}x{h} pixels", body.len())));
    }
    let mut out = vec![0u16; n];
    let mut stream = Bits::new(body);
    for y in 0..h {
        let (above, rest) = out.split_at_mut(y * w);
        let row = rest.get_mut(..w).ok_or(RawError::Limit("ORF row"))?;
        // the same-colour row above, two rows up
        let up = y.checked_sub(2).and_then(|u| above.get(u * w..(u + 1) * w));
        let mut phases = [Phase::default(); 2];
        for (x, o) in row.iter_mut().enumerate() {
            let p = &mut phases[x & 1];
            let extra = if p.small < 3 { 2 } else { 0 };
            let k = (2 + extra).max((32 - p.magnitude.leading_zeros()).saturating_sub(extra));
            let head = stream.get(3);
            let (negative, low) = (head & 4 != 0, (head & 3) as i32);
            let zeros = (stream.peek(12) << 20).leading_zeros();
            let quotient = if zeros < 12 {
                stream.skip(zeros + 1);
                zeros
            } else {
                stream.skip(12);
                stream.get(16 - k) >> 1
            };
            let magnitude = (quotient << k) | stream.get(k);
            // a 12-bit difference never needs more; it also keeps `k` (≤ 12) and the arithmetic below in range
            if magnitude > MAX as u32 {
                return Err(RawError::Corrupt(format!("ORF: invalid code at row {y}")));
            }
            let diff = if negative { -(magnitude as i32) - 1 } else { magnitude as i32 } + p.bias;
            p.bias = (diff * 3 + p.bias) >> 5;
            p.small = if magnitude > 16 { 0 } else { p.small.saturating_add(1) };
            p.magnitude = magnitude;
            let north = up.and_then(|u| u.get(x)).map(|&v| v as i32);
            let prediction = match north {
                None if x < 2 => 0,
                None => p.left,
                Some(n) if x < 2 => n,
                Some(n) => {
                    let (w, nw) = (p.left, p.up_left);
                    if (w < nw && nw < n) || (n < nw && nw < w) {
                        if (w - nw).abs() > 32 || (n - nw).abs() > 32 { w + n - nw } else { (w + n) >> 1 }
                    } else if (w - nw).abs() > (n - nw).abs() {
                        w
                    } else {
                        n
                    }
                }
            };
            let v = prediction + ((diff << 2) | low);
            if !(0..=MAX).contains(&v) {
                return Err(RawError::Corrupt(format!("ORF: decoded value out of range at row {y}")));
            }
            p.left = v;
            p.up_left = north.unwrap_or(0);
            *o = v as u16;
        }
        if stream.consumed_bits() > body.len() * 8 {
            return Err(RawError::Corrupt(format!("ORF: compressed data ends at row {y}")));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags as t};

    /// MSB-first bit writer for the test encoder.
    #[derive(Default)]
    struct Writer {
        out: Vec<u8>,
        acc: u8,
        n: u32,
        escapes: usize,
    }

    impl Writer {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1) as u8;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc);
                    (self.acc, self.n) = (0, 0);
                }
            }
        }
        /// No padding beyond the last byte, like the cameras.
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.out.push(self.acc << (8 - self.n));
            }
            self.out
        }
    }

    fn prediction(v: &[i32], w: usize, x: usize, y: usize) -> i32 {
        let at = |dx: usize, dy: usize| v[(y - dy) * w + x - dx];
        match (y < 2, x < 2) {
            (true, true) => 0,
            (true, false) => at(2, 0),
            (false, true) => at(0, 2),
            (false, false) => {
                let (w, n, nw) = (at(2, 0), at(0, 2), at(2, 2));
                if (w < nw && nw < n) || (n < nw && nw < w) {
                    if (w - nw).abs() > 32 || (n - nw).abs() > 32 { w + n - nw } else { (w + n) >> 1 }
                } else if (w - nw).abs() > (n - nw).abs() {
                    w
                } else {
                    n
                }
            }
        }
    }

    /// Encode `values` (12-bit) following the module docs; also returns how often the long quotient form was used.
    fn encode(values: &[i32], w: usize) -> (Vec<u8>, usize) {
        let mut wr = Writer { out: HEADER.to_vec(), ..Default::default() };
        for (y, row) in values.chunks(w).enumerate() {
            let mut state = [[0i32; 3]; 2]; // magnitude, bias, run of small magnitudes
            for (x, &v) in row.iter().enumerate() {
                let s = &mut state[x & 1];
                let extra = if s[2] < 3 { 2 } else { 0 };
                let mut k = 2 + extra;
                while s[0] >> (k + extra) != 0 {
                    k += 1;
                }
                let delta = v - prediction(values, w, x, y);
                let (d, low) = (delta >> 2, delta & 3);
                let signed = d - s[1];
                let (negative, m) = if signed < 0 { (1, -signed - 1) } else { (0, signed) };
                wr.put((negative << 2 | low) as u32, 3);
                let q = (m >> k) as u32;
                if q < 12 {
                    wr.put(1, q + 1);
                } else {
                    wr.put(0, 12);
                    wr.put(q << 1, 16 - k as u32);
                    wr.escapes += 1;
                }
                wr.put(m as u32 & ((1 << k) - 1), k as u32);
                *s = [m, (d * 3 + s[1]) >> 5, if m > 16 { 0 } else { s[2] + 1 }];
            }
        }
        let escapes = wr.escapes;
        (wr.finish(), escapes)
    }

    /// Deterministic test image: smooth gradient + texture + hard edges, within `0..=4095`.
    fn image(w: usize, h: usize, seed: u32) -> Vec<i32> {
        let mut s = seed;
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (s >> 24) as i32 % 9 - 4;
                let base = (x * 37 + y * 11) % 2047 + if (x / 7 + y / 5) % 3 == 0 { 1365 } else { 0 };
                (base + noise).clamp(0, MAX)
            })
            .collect()
    }

    fn as_u16(v: &[i32]) -> Vec<u16> {
        v.iter().map(|&v| v as u16).collect()
    }

    #[test]
    fn round_trip_including_long_quotients() {
        for (w, h) in [(64, 9), (37, 6), (2, 5), (1, 3), (5, 1)] {
            let mut img = image(w, h, (w * h) as u32);
            if w > 9 {
                // extreme jumps need the long quotient form
                img[5] = 0;
                img[7] = MAX;
                img[w + 9] = MAX;
                img[3 * w + 4] = 0;
            }
            let (src, escapes) = encode(&img, w);
            assert!(escapes > 0 || w <= 9, "{w}x{h}: the long quotient form was never used");
            assert_eq!(decode(&src, w, h).unwrap(), as_u16(&img), "{w}x{h}");
        }
        // flat extremes
        for v in [0, MAX] {
            let img = vec![v; 48 * 4];
            assert_eq!(decode(&encode(&img, 48).0, 48, 4).unwrap(), as_u16(&img));
        }
    }

    #[test]
    fn truncated_and_corrupted_streams_error_never_panic() {
        let (w, h) = (64, 16);
        let img = image(w, h, 3);
        let (src, _) = encode(&img, w);
        assert!(is_compressed(&src) && !is_compressed(&src[1..]) && !is_compressed(&[]));
        // truncation: too short for the image, or ends early
        for cut in [0, 1, 6, 7, 10, src.len() / 2, src.len() - 2] {
            assert!(decode(&src[..cut], w, h).is_err(), "cut {cut}");
        }
        // another header is a variant we don't know
        let mut other = src.clone();
        other[4..6].copy_from_slice(&[0, 1]);
        assert!(matches!(decode(&other, w, h), Err(RawError::Unsupported(_))));
        // garbage and bit flips: an error or (rarely) some image, never a panic
        let mut s = 12345u32;
        for i in 0..300 {
            let mut bad = src.clone();
            for _ in 0..1 + i % 8 {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let at = HEADER.len() + (s >> 8) as usize % (bad.len() - HEADER.len());
                bad[at] ^= 1 << (s % 8);
            }
            let _ = decode(&bad, w, h);
            let mut noise = HEADER.to_vec();
            noise.extend((0..src.len()).map(|k| ((k as u32).wrapping_mul(2_654_435_761) >> (i % 24)) as u8));
            let _ = decode(&noise, w, h);
        }
        // all zeros and all ones: long quotients that leave the 12-bit range
        for fill in [0u8, 0xff] {
            let mut flat = HEADER.to_vec();
            flat.extend([fill; 512]);
            assert!(decode(&flat, 16, 16).is_err(), "{fill:#x}");
        }
        // absurd dimensions are limited before allocating
        assert!(decode(&src, usize::MAX, 2).is_err());
        assert!(decode(&src, 1 << 20, 1 << 20).is_err());
        assert!(decode(&src, 0, 2).is_err());
        assert!(decode(&src, 2, 0).is_err());
    }

    /// A minimal compressed ORF: `IIRO` magic, 16 bits declared, compression 1, one strip in IFD0; optionally an
    /// Exif `CFAPattern`.
    fn orf_file(strip: Vec<u8>, w: usize, h: usize, cfa_pattern: Option<&[u8]>) -> Vec<u8> {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w as u32]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h as u32]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![1]));
        ifd.set(t::MAKE, Value::Ascii("OLYMPUS CORPORATION".into()));
        ifd.set_image(ImageData::Strips { rows_per_strip: h as u32, strips: vec![strip] });
        if let Some(p) = cfa_pattern {
            let mut exif = IfdBuilder::new();
            exif.set(0xa302, Value::Undefined(p.to_vec()));
            ifd.set_child(t::EXIF_IFD, exif);
        }
        let mut b = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
        b[2] = b'R';
        b[3] = b'O';
        b
    }

    /// A mosaic of one colour with some texture: `site` gives the level of each position of the 2×2 cell.
    fn mosaic(w: usize, h: usize, site: [i32; 4]) -> Vec<i32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                site[(y & 1) * 2 + (x & 1)] + ((x / 2 * 7 + y / 2 * 13) % 23) as i32
            })
            .collect()
    }

    #[test]
    fn decodes_through_the_orf_container() {
        let (w, h) = (160, 144);
        let img = mosaic(w, h, [600, 1000, 1000, 300]);
        let (strip, _) = encode(&img, w);
        // the layout the file states wins, in either byte order of the repeat counts
        for (tag, name) in [(&[2u8, 0, 2, 0, 2, 1, 1, 0], "BGGR"), (&[0, 2, 0, 2, 0, 1, 1, 2], "RGGB"), (&[2, 0, 2, 0, 1, 0, 2, 1], "GRBG")] {
            let bytes = orf_file(strip.clone(), w, h, Some(tag));
            assert_eq!(crate::probe(&bytes), Some(crate::RawFormat::Orf));
            let r = crate::decode(&bytes).unwrap();
            assert_eq!((r.data.clone(), r.bits), (crate::RawData::U16(as_u16(&img)), 12));
            assert_eq!(r.cfa.as_ref().unwrap().name(), name);
            assert_eq!(crate::probe_info(&bytes).unwrap(), r.info(), "{name}");
        }
        // without the tag, or with one that is no Bayer cell: the green diagonal of the samples
        for (site, name) in [([1000, 600, 300, 1000], "GRBG"), ([600, 1000, 1000, 300], "RGGB")] {
            let img = mosaic(w, h, site);
            for tag in [None, Some(&[2u8, 0, 2, 0, 1, 1, 1, 1][..]), Some(&[3, 0, 3, 0, 0, 1, 1, 2]), Some(&[2, 0, 2, 0, 1, 0]), Some(&[])] {
                let bytes = orf_file(encode(&img, w).0, w, h, tag);
                let r = crate::decode(&bytes).unwrap();
                assert_eq!(r.cfa.as_ref().unwrap().name(), name, "{tag:?}");
                assert_eq!(crate::probe_info(&bytes).unwrap(), r.info(), "{name} {tag:?}");
            }
        }
        // a strip cut short: an error from the decode
        let mut cut = strip.clone();
        cut.truncate(cut.len() - 100);
        assert!(matches!(crate::decode(&orf_file(cut, w, h, None)), Err(RawError::Corrupt(_))));
        // an unknown strip header stays preview-only
        let mut other = vec![0, 0, 0, 0, 0, 1, 0, 0];
        other.resize(4000, 0x5a);
        let e = crate::decode(&orf_file(other, w, h, None));
        assert!(matches!(&e, Err(RawError::Unsupported(why)) if why.contains("header 00 00 00 00 00 01 00 00")), "{e:?}");
    }
}
