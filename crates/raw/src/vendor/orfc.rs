//! Olympus's lossless ORF stream, independently derived from CC0 raw.pixls.us files.
//!
//! The predictor, signed Rice codes, flatness runs and residual correction are described in Olympus
//! patents US7333035B2 and US7715639B2 (Takashi Ishikawa). The actual ORF constants and packing were
//! established against sensor pixels from the `unprocessed_raw` executable (LibRaw 0.22.2), used only
//! as a black-box oracle: no decoder source or camera matrices were consulted. See docs/orf.md.
//!
//! After a seven-byte header, samples are coded MSB-first without byte stuffing or row alignment.
//! Even/odd columns have separate magnitude, correction and flatness state, reset on every row.
//! A sample has a sign bit, two uncompressed residual bits, a bounded unary quotient (12 zeros escape
//! to a fixed-size quotient), then the adaptive Rice remainder. Spatial neighbours are two pixels
//! away, preserving the Bayer colour. The first two rows predict from the left; subsequent rows
//! start from above, then use an edge predictor with a mean in low-gradient regions.

use super::pef::Bits;
use crate::{MAX_SAMPLES, RawError, Result};

const HEADER: &[u8] = &[0, 0, 0, 0, 1, 0, 0];

pub(super) fn recognizes(src: &[u8]) -> bool {
    src.starts_with(HEADER)
}

#[derive(Clone, Copy, Default)]
struct Context {
    magnitude: u32,
    correction: i32,
    flat_run: u32,
    left: i32,
}

impl Context {
    fn residual(&mut self, bits: &mut Bits) -> Result<i32> {
        let extra = if self.flat_run < 3 { 2 } else { 0 };
        let k = (2 + extra).max((32 - self.magnitude.leading_zeros()).saturating_sub(extra));
        if k > 15 {
            return Err(RawError::Corrupt("ORF residual exceeds the coding range".into()));
        }
        let sign_low = bits.get(3);
        let mut quotient = 0;
        while quotient < 12 && bits.get(1) == 0 {
            quotient += 1;
        }
        if quotient == 12 {
            // The escape has one unused low bit before the ordinary k-bit remainder.
            quotient = bits.get(16 - k) >> 1;
        }
        self.magnitude = (quotient << k) | bits.get(k);
        let signed = if sign_low & 4 == 0 { self.magnitude as i32 } else { -(self.magnitude as i32) - 1 };
        let high = signed + self.correction;
        self.correction = (3 * high + self.correction) >> 5;
        self.flat_run = if self.magnitude <= 16 { self.flat_run.saturating_add(1) } else { 0 };
        Ok(4 * high + (sign_low & 3) as i32)
    }
}

fn predictor(left: i32, up: i32, corner: i32) -> i32 {
    if (left < corner && corner < up) || (up < corner && corner < left) {
        if (left - corner).abs() > 32 || (up - corner).abs() > 32 { left + up - corner } else { (left + up) / 2 }
    } else if (left - corner).abs() > (up - corner).abs() {
        left
    } else {
        up
    }
}

fn payload_bits(byte_len: usize) -> Result<usize> {
    // Bits prefetches eight bytes, including zero padding. A truncated symbol consumes at most
    // 31 bits before we detect it; leave room for both in its byte-to-bit position arithmetic.
    byte_len.checked_add(16).and_then(|n| n.checked_mul(8)).ok_or(RawError::Limit("ORF compressed strip too large"))?;
    Ok(byte_len * 8)
}

/// Validate the stream envelope without allocating the sensor buffer (also used by import probes).
pub(super) fn validate(src: &[u8], w: usize, h: usize) -> Result<&[u8]> {
    let n = w.checked_mul(h).filter(|n| w > 0 && h > 0 && *n <= MAX_SAMPLES).ok_or(RawError::Limit("ORF image too large"))?;
    if !recognizes(src) {
        return Err(RawError::Unsupported("Olympus ORF stream header".into()));
    }
    let payload = src.get(HEADER.len()..).ok_or_else(|| RawError::Corrupt("ORF stream header truncated".into()))?;
    // Every pixel needs at least sign + two low bits + unary terminator + two remainder bits.
    // Check this before allocation, so a tiny file cannot request an enormous sensor buffer.
    if n > payload_bits(payload.len())? / 6 {
        return Err(RawError::Corrupt("ORF compressed strip too short for its dimensions".into()));
    }
    Ok(payload)
}

pub(super) fn decode(src: &[u8], w: usize, h: usize) -> Result<Vec<u16>> {
    let payload = validate(src, w, h)?;
    let mut bits = Bits::new(payload);
    let limit = payload_bits(payload.len())?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(w * h).map_err(|_| RawError::Limit("cannot allocate ORF sensor buffer"))?;
    samples.resize(w * h, 0u16);
    for y in 0..h {
        let mut contexts = [Context::default(); 2];
        let (past, rest) = samples.split_at_mut_checked(y * w).ok_or_else(|| RawError::Corrupt("ORF row outside sensor".into()))?;
        let row = rest.get_mut(..w).ok_or_else(|| RawError::Corrupt("ORF row outside sensor".into()))?;
        for (x, sample) in row.iter_mut().enumerate() {
            let context = contexts.get_mut(x & 1).ok_or_else(|| RawError::Corrupt("ORF invalid column parity".into()))?;
            let residual = context.residual(&mut bits)?;
            if bits.consumed_bits() > limit {
                return Err(RawError::Corrupt(format!("ORF compressed strip truncated at row {y}")));
            }
            let prediction = if y < 2 {
                context.left
            } else {
                let offset = (y - 2) * w + x;
                let up = past.get(offset).copied().ok_or_else(|| RawError::Corrupt("ORF predictor outside sensor".into()))? as i32;
                if x < 2 {
                    up
                } else {
                    let corner = past.get(offset - 2).copied().ok_or_else(|| RawError::Corrupt("ORF predictor outside sensor".into()))? as i32;
                    predictor(context.left, up, corner)
                }
            };
            let value = prediction + residual;
            if !(0..=4095).contains(&value) {
                return Err(RawError::Corrupt(format!("ORF sample outside 12-bit range at ({x}, {y})")));
            }
            *sample = value as u16;
            context.left = value;
        }
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn independent_first_sensor_samples_include_escape_and_negative_residuals() {
        // First bytes and first 16 sensor samples of CC0 E-M1 file 1051, independently read with
        // unprocessed_raw. This pins the escape's unused bit, sign/low-bit order and correction.
        let src = [
            0, 0, 0, 0, 1, 0, 0, 0x20, 0, 4, 0xb8, 0xc0, 0, 0x0d, 0x42, 0xca, 0xca, 0x8f, 0x4f, 0x97, 0x21, 0x2f, 0x47, 5, 0x91, 0xdc, 0x85, 0x75,
            0xb2, 0x7a, 0xff, 0xad,
        ];
        assert_eq!(decode(&src, 16, 1).unwrap(), [1201, 1667, 1226, 1659, 1213, 1626, 1220, 1630, 1203, 1633, 1217, 1601, 1224, 1637, 1203, 1626]);
    }

    #[test]
    fn edge_prediction_and_gradient_threshold() {
        assert_eq!(predictor(20, 80, 40), 60); // high gradient, planar
        assert_eq!(predictor(20, 60, 40), 40); // low gradient, mean
        assert_eq!(predictor(20, 60, 20), 60); // equality selects the farther neighbour
        assert_eq!(predictor(20, 60, 60), 20);
        assert_eq!(predictor(20, 60, 0), 60);
        assert_eq!(predictor(20, 60, 80), 20);
        assert_eq!(predictor(0, 64, 32), 32);
        assert_eq!(predictor(0, 64, 33), 31); // >32, not >=32
    }

    #[test]
    fn hostile_dimensions_and_truncated_codes_are_errors() {
        assert!(payload_bits(usize::MAX).is_err());
        assert!(payload_bits(usize::MAX / 8).is_err());
        assert_eq!(payload_bits(123).unwrap(), 984);
        let src = [HEADER, &[0xff; 32]].concat();
        for (w, h) in [(0, 1), (1, 0), (usize::MAX, 2), (MAX_SAMPLES, 1), (1000, 1000)] {
            assert!(decode(&src, w, h).is_err());
        }
        for end in 0..HEADER.len() {
            assert!(decode(&src[..end], 1, 1).is_err());
        }
        // Enough bits by the minimum-size bound, but escape literals overrun the actual stream.
        let zeros = [HEADER, &[0; 12]].concat();
        assert!(matches!(decode(&zeros, 16, 1), Err(RawError::Corrupt(_))));
        // A positive escape with a 15-bit magnitude cannot represent a 12-bit sample.
        let range = [HEADER, &[0, 1, 0xff, 0xfe]].concat();
        assert!(decode(&range, 1, 1).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_compressed_payload_never_panics(payload in prop::collection::vec(any::<u8>(), 0..512), w in 0usize..40, h in 0usize..12) {
            let src = [HEADER, &payload].concat();
            let _ = decode(&src, w, h);
        }
    }
}
