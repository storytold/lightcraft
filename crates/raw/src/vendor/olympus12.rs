//! The independently measured E01/V02 compressed 12-bit ORF profile.
//!
//! Specification, failed hypotheses, specimen identities and arithmetic review:
//! `docs/raw/orf12-compressed-measured.md` and `docs/raw/orf12-review.md`.
//! This fixed profile is not a generalized metadata-driven Olympus codec.

use crate::{RawError, Result};
use lightcraft_tiff::{Ifd, Value};

const PREFIX: [u8; 7] = [0, 0, 0, 0, 1, 0, 0];
const MAX_SAMPLES: usize = 96_000_000;
pub(super) const PROFILE: [(u16, u16); 14] = [
    (0x0640, 0),
    (0x0641, 5),
    (0x0642, 0),
    (0x0643, 2),
    (0x0644, 5),
    (0x0645, 4),
    (0x0646, 3),
    (0x0647, 2),
    (0x0648, 3),
    (0x0649, 4),
    (0x0650, 7),
    (0x0651, 15),
    (0x0652, 12),
    (0x0653, 5),
];

/// An observed raw-field fingerprint, without assigning semantics to individual fields.
pub(super) fn matches_profile(ip: &Ifd) -> bool {
    matches!(ip.value(0x0611), Some(Value::Short(v)) if v.as_slice() == [12, 0])
        && PROFILE.iter().all(|&(tag, value)| matches!(ip.value(tag), Some(Value::Short(v)) if v.as_slice() == [value]))
        && (0x064a..=0x064f).all(|tag| ip.value(tag).is_none())
}

/// Missing/zero fields also occur in packed originals; unfamiliar coding fields must not
/// accidentally select a density-based packed or word reader.
pub(super) fn has_coding_fields(ip: &Ifd) -> bool {
    (0x0640..=0x0653).any(|tag| match ip.value(tag) {
        None => false,
        Some(Value::Short(v)) => v.as_slice() != [0],
        Some(_) => true,
    })
}

/// Validate cheap strip/dimension bounds, also used by header probes without decoding pixels.
pub(super) fn validate(strip: &[u8], width: usize, height: usize) -> Result<usize> {
    let n = width.checked_mul(height).filter(|&n| n > 0 && n <= MAX_SAMPLES).ok_or(RawError::Limit("compressed ORF sample budget"))?;
    if width > 16_384 || height > 20_000 {
        return Err(RawError::Limit("compressed ORF dimension budget"));
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(RawError::Unsupported("compressed ORF with unverified odd dimensions".into()));
    }
    let prefix = strip.get(..PREFIX.len()).ok_or_else(|| RawError::Corrupt("short compressed ORF prefix".into()))?;
    if prefix != PREFIX {
        return Err(RawError::Unsupported("unverified compressed ORF prefix".into()));
    }
    // At least six bits per token; the first three tokens of each parity need two more.
    let warm_bits = width.min(6).checked_mul(height).and_then(|v| v.checked_mul(2));
    let minimum = n.checked_mul(6).and_then(|v| v.checked_add(warm_bits?)).and_then(|v| v.checked_add(56));
    let available = strip.len().checked_mul(8);
    if !matches!((minimum, available), (Some(need), Some(have)) if have >= need) {
        return Err(RawError::Corrupt("compressed ORF strip shorter than minimum coding length".into()));
    }
    Ok(n)
}

/// A strict MSB-first reservoir. A missing required bit is an error, never zero-filled.
struct Bits<'a> {
    bytes: &'a [u8],
    next: usize,
    reservoir: u32,
    available: u32,
}

impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, next: 0, reservoir: 0, available: 0 }
    }

    fn take(&mut self, count: u32) -> Result<u32> {
        if count > 16 {
            return Err(RawError::Corrupt("compressed ORF field width exceeds reader bound".into()));
        }
        while self.available < count {
            let byte = self.bytes.get(self.next).ok_or_else(|| RawError::Corrupt("truncated compressed ORF codeword".into()))?;
            self.next = self.next.checked_add(1).ok_or(RawError::Limit("compressed ORF bit offset"))?;
            // Before refill available < count <= 16, so at most 23 bits are live.
            self.reservoir = (self.reservoir << 8) | u32::from(*byte);
            self.available += 8;
        }
        self.available -= count;
        Ok((self.reservoir >> self.available) & ((1u32 << count) - 1))
    }

    fn remaining(&self) -> Result<usize> {
        self.bytes
            .len()
            .checked_sub(self.next)
            .and_then(|v| v.checked_mul(8))
            .and_then(|v| v.checked_add(self.available as usize))
            .ok_or(RawError::Limit("compressed ORF remaining bit count"))
    }
}

#[derive(Default)]
struct Context {
    previous: u32,
    small: u8,
    bias: i32,
}

impl Context {
    fn width(&self) -> Result<u32> {
        let length = 32 - self.previous.leading_zeros();
        let k = if self.small < 3 { length.saturating_sub(2).max(4) } else { length.max(2) };
        if k > 9 {
            return Err(RawError::Corrupt("compressed ORF context outside 12-bit bounds".into()));
        }
        Ok(k)
    }

    fn sample(&mut self, bits: &mut Bits<'_>, prediction: i32) -> Result<u16> {
        let k = self.width()?;
        let flags = bits.take(3)?;
        let mut zeros = 0;
        while zeros < 12 && bits.take(1)? == 0 {
            zeros += 1;
        }
        let quotient = if zeros == 12 {
            let u = bits.take(15 - k)?;
            bits.take(1)?; // Measured extra escape bit; its value is not interpreted.
            u
        } else {
            zeros
        };
        let q = (quotient << k) + bits.take(k)?;
        // k <= 9 and the bounded escape ensure q <= 32767. With validated prior samples,
        // bias stays in -100..99; all intermediate signed arithmetic therefore fits i32.
        let high = if flags & 4 != 0 { !(q as i32) } else { q as i32 };
        let difference = high + self.bias;
        let sample = prediction + 4 * difference + (flags & 3) as i32;
        if !(0..=4095).contains(&sample) {
            return Err(RawError::Corrupt("compressed ORF sample outside 12-bit range".into()));
        }
        self.bias = (3 * difference + self.bias).div_euclid(32);
        self.previous = q;
        self.small = if q <= 16 { self.small.saturating_add(1).min(3) } else { 0 };
        Ok(sample as u16)
    }
}

fn neighbor(row: &[u16], x: usize) -> Result<i32> {
    row.get(x).copied().map(i32::from).ok_or_else(|| RawError::Corrupt("compressed ORF predictor outside row".into()))
}

fn prediction(row: &[u16], above: &[u16], x: usize, y: usize) -> Result<i32> {
    match (x < 2, y < 2) {
        (true, true) => Ok(0),
        (false, true) => neighbor(row, x - 2),
        (true, false) => neighbor(above, x),
        (false, false) => {
            let (left, up, diagonal) = (neighbor(row, x - 2)?, neighbor(above, x)?, neighbor(above, x - 2)?);
            let (dl, du) = (left - diagonal, up - diagonal);
            Ok(if dl * du < 0 && dl.abs().max(du.abs()) <= 32 { (left + up) / 2 } else { (left + up - diagonal).clamp(left.min(up), left.max(up)) })
        }
    }
}

pub(super) fn decode(strip: &[u8], width: usize, height: usize) -> Result<Vec<u16>> {
    let n = validate(strip, width, height)?;
    let body = strip.get(PREFIX.len()..).ok_or_else(|| RawError::Corrupt("short compressed ORF prefix".into()))?;
    let mut bits = Bits::new(body);
    let mut samples = Vec::new();
    samples.try_reserve_exact(n).map_err(|_| RawError::Limit("compressed ORF sensor allocation"))?;
    samples.resize(n, 0);
    for y in 0..height {
        let start = y.checked_mul(width).ok_or(RawError::Limit("compressed ORF row offset"))?;
        let (prior, tail) = samples.split_at_mut_checked(start).ok_or_else(|| RawError::Corrupt("compressed ORF row outside raster".into()))?;
        let row = tail.get_mut(..width).ok_or_else(|| RawError::Corrupt("compressed ORF row outside raster".into()))?;
        let above = if y < 2 {
            &[][..]
        } else {
            let a = (y - 2).checked_mul(width).ok_or(RawError::Limit("compressed ORF predictor row offset"))?;
            let b = a.checked_add(width).ok_or(RawError::Limit("compressed ORF predictor row end"))?;
            prior.get(a..b).ok_or_else(|| RawError::Corrupt("compressed ORF predictor row outside raster".into()))?
        };
        let (mut even, mut odd) = (Context::default(), Context::default());
        for x in 0..width {
            let p = prediction(row, above, x, y)?;
            let context = if x.is_multiple_of(2) { &mut even } else { &mut odd };
            let value = context.sample(&mut bits, p)?;
            *row.get_mut(x).ok_or_else(|| RawError::Corrupt("compressed ORF sample outside raster".into()))? = value;
        }
    }
    if bits.remaining()? >= 8 {
        return Err(RawError::Unsupported("compressed ORF with unverified trailing bytes".into()));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn strip(body: &[u8]) -> Vec<u8> {
        [PREFIX.as_slice(), body].concat()
    }

    // Fixed codewords, independently calculated from the measured grammar, not an encoder.
    fn small_strip() -> Vec<u8> {
        strip(&[0x18, 0x10, 0x18, 0x10, 0x10, 0x10, 0x10, 0x10, 0x96, 0x10, 0x11, 0x10, 0x10, 0x10, 0x10, 0x10])
    }

    fn zero_strip() -> Vec<u8> {
        let row = ["00010000".repeat(6), "000100".repeat(10)].concat();
        row.repeat(8).as_bytes().chunks(8).fold(PREFIX.to_vec(), |mut bytes, chunk| {
            bytes.push(chunk.iter().fold(0, |v, &bit| (v << 1) | (bit - b'0')));
            bytes
        })
    }

    #[test]
    fn fixed_codewords_cover_bias_floor_and_predictor_boundaries() {
        let original = small_strip();
        assert_eq!(decode(&original, 4, 4).unwrap(), [32, 0, 64, 0, 0, 0, 0, 0, 4, 0, 34, 0, 0, 0, 0, 0]);
        let mut threshold = original.clone();
        threshold[9] = 0x19;
        assert_eq!(decode(&threshold, 4, 4).unwrap()[10], 40, "gradient 36 must select median");
        let mut equal_diagonal = original;
        equal_diagonal[15] = 0x10;
        equal_diagonal[17] = 0x10;
        assert_eq!(decode(&equal_diagonal, 4, 4).unwrap()[10], 64, "diagonal equality must select median");
    }

    #[test]
    fn contexts_reset_at_unaligned_row_boundaries() {
        let bytes = zero_strip();
        assert_eq!(bytes.len(), 115);
        assert_eq!(decode(&bytes, 16, 8).unwrap(), vec![0; 128]);
    }

    #[test]
    fn small_count_includes_sixteen_and_saturates() {
        let mut context = Context { small: 2, ..Default::default() };
        let mut bits = Bits::new(&[0x08, 0x7f]); // 000 01 0000: q=16, then ignored tail bits.
        assert_eq!(context.sample(&mut bits, 0).unwrap(), 64);
        assert_eq!((context.small, context.width().unwrap()), (3, 5));
        let mut context = Context::default();
        for _ in 0..8 {
            context.sample(&mut Bits::new(&[0x10]), 0).unwrap();
        }
        assert_eq!(context.small, 3);
    }

    fn fixed_bits(bits: &str) -> Vec<u8> {
        bits.as_bytes().chunks(8).map(|c| c.iter().fold(0u8, |v, &b| (v << 1) | (b - b'0')) << (8 - c.len())).collect()
    }

    #[test]
    fn escape_consumes_extra_bit_and_uses_signed_complement() {
        // Two initial k=4 escapes: q=219 and q=406, independently calculated.
        let first = "0000000000000000000000110101011";
        let second = "0000000000000000000001100100110";
        assert_eq!((first.len(), second.len()), (31, 31));
        let bytes = fixed_bits(&[first, second].concat());
        let mut bits = Bits::new(&bytes);
        assert_eq!(Context::default().sample(&mut bits, 0).unwrap(), 876);
        assert_eq!(Context::default().sample(&mut bits, 0).unwrap(), 1624);
        assert_eq!(bits.remaining().unwrap(), 2);
        let mut changed = bytes.clone();
        changed[3] ^= 0x20; // First escape's bit 26: consumed but uninterpreted.
        assert_eq!(Context::default().sample(&mut Bits::new(&changed), 0).unwrap(), 876);
        let negative = fixed_bits("1000000000000000000000110101011");
        assert_eq!(Context::default().sample(&mut Bits::new(&negative), 1000).unwrap(), 120);
    }

    #[test]
    fn unused_final_byte_bits_have_no_required_value() {
        let bytes = strip(&fixed_bits("000010000000100000001000000010000"));
        assert_eq!(decode(&bytes, 2, 2).unwrap(), [64, 0, 0, 0]);
        for tail in 0..128 {
            let mut changed = bytes.clone();
            *changed.last_mut().unwrap() = tail;
            assert_eq!(decode(&changed, 2, 2).unwrap(), [64, 0, 0, 0]);
        }
    }

    #[test]
    fn reservoir_matches_bit_oracle_and_never_supplies_missing_bits() {
        for value in 0u32..=u16::MAX as u32 {
            let bytes = (value as u16).to_be_bytes();
            let mut bits = Bits::new(&bytes);
            assert_eq!(bits.take(3).unwrap(), value >> 13);
            assert_eq!(bits.take(9).unwrap(), (value >> 4) & 511);
            assert_eq!(bits.take(4).unwrap(), value & 15);
            assert_eq!(bits.remaining().unwrap(), 0);
            assert!(matches!(bits.take(1), Err(RawError::Corrupt(_))));
        }
        assert!(Bits::new(&[0; 3]).take(17).is_err());
        assert_eq!(Bits::new(&[]).take(0).unwrap(), 0);
    }

    #[test]
    fn truncation_bad_samples_and_trailers_are_errors() {
        let bytes = small_strip();
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut], 4, 4).is_err());
        }
        let mut negative = bytes.clone();
        negative[7] = 0x90; // Negative high part from a zero spatial seed.
        assert!(matches!(decode(&negative, 4, 4), Err(RawError::Corrupt(_))));
        let mut trailer = bytes;
        trailer.push(0xff);
        assert!(matches!(decode(&trailer, 4, 4), Err(RawError::Unsupported(_))));
        assert!(matches!(decode(&trailer, usize::MAX, 8), Err(RawError::Limit(_))));
        assert!(matches!(decode(&trailer, 16_386, 2), Err(RawError::Limit(_))));
        assert!(matches!(decode(&trailer, 2, 20_002), Err(RawError::Limit(_))));
        assert!(matches!(decode(&trailer, 16_000, 8_000), Err(RawError::Limit(_))));
        assert!(matches!(decode(&trailer, 3, 4), Err(RawError::Unsupported(_))));
    }

    proptest! {
        #[test]
        fn hostile_payloads_and_dimensions_do_not_panic(body in prop::collection::vec(any::<u8>(), 0..1024), w in any::<usize>(), h in any::<usize>()) {
            let bytes = strip(&body);
            let _ = decode(&bytes, w, h);
            let _ = decode(&bytes, 4, 4);
        }
    }
}
