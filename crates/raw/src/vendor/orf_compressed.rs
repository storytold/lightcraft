//! Clean-room Olympus ORF compressed bitstream decoder.
//!
//! Implementation conforms strictly to the functional specification in `clean-room/orf/orf-spec.md`,
//! derived from US Patent US7333035B2 (Takashi Ishikawa, Olympus Corp.) and US Patent US7715639B2
//! (Takashi Ishikawa, Olympus Corp.), TIFF 6.0, and Exif 2.3 MakerNote structures.
//!
//! Non-test code forbids unsafe code and panics. All input streams are treated as untrusted and hostile.

use crate::{RawError, Result};
use craft_tiff::Ifd;
use craft_tiff::makernote::MakerNote;

/// Olympus MakerNote tag specifying valid sensor bit depth.
const MAKERNOTE_VALID_BITS_TAG: u16 = 0x102c;

/// Operational parameters extracted from the MakerNote `ImageProcessing` SubIFD (`0x2040`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrfCompressionConfig {
    /// Direct uncompressed low-order bit depth ($S_0$, tag `0x0640`).
    pub direct_low_bits: u32,
    /// Edge gradient threshold ($E_{th} = 2^{T_E}$, tag `0x0641`).
    pub gradient_threshold: u32,
    /// Default boundary baseline predictor value ($P_0$, tag `0x0642`).
    pub boundary_baseline: i32,
    /// Direct residual sub-bit depth ($S_R$, tag `0x0643`).
    pub residual_sub_bits: u32,
    /// Context feedback divisor bit-shift ($D_{fb}$, tag `0x0644`).
    pub feedback_shift: u32,
    /// Flatness threshold value ($F_{th} = 2^{T_{flat}}$, tag `0x0645`).
    pub flatness_threshold: i32,
    /// Flatness run-length threshold ($R_{th}$, tag `0x0646`).
    pub run_threshold: i32,
    /// Adaptation table A parameters: `(base, limit)` (tags `0x0647`, `0x0648`).
    pub table_a: (i32, i32),
    /// Adaptation table B parameters: `(base, limit)` (tags `0x0649`, `0x0650`).
    pub table_b: (i32, i32),
    /// Maximum Rice parameter $k_{max}$ (tag `0x0651`).
    pub max_rice_k: i32,
    /// Maximum unary quotient run length $Q_{max}$ (tag `0x0652`).
    pub max_unary_run: u32,
    /// Header prefix byte length $N$ (tag `0x0653`).
    pub header_prefix_len: usize,
    /// Sensor bit depth from MakerNote tag `0x102c`.
    pub sensor_bits: u32,
}

impl OrfCompressionConfig {
    /// Parse and validate configuration parameters from the `ImageProcessing` SubIFD and MakerNote.
    pub(crate) fn from_subifd(ip: &Ifd, mn: Option<&MakerNote>) -> Option<Self> {
        let header_prefix_len = usize::try_from(ip.u64(0x0653)?).ok()?;
        if !(1..=8).contains(&header_prefix_len) {
            return None;
        }

        let direct_low_bits = parse_bounded_u32(ip, 0x0640, 0, 0..=16)?;
        let residual_sub_bits = parse_bounded_u32(ip, 0x0643, 2, 0..=2)?;
        let feedback_shift = parse_bounded_u32(ip, 0x0644, 5, 1..=15)?;
        let grad_exp = parse_bounded_u32(ip, 0x0641, 5, 0..=16)?;
        let gradient_threshold = 1u32.checked_shl(grad_exp)?;

        let boundary_baseline = match ip.i64(0x0642) {
            Some(raw) => {
                let val = i32::try_from(raw).ok()?;
                if (-65536..=65536).contains(&val) {
                    val
                } else {
                    return None;
                }
            }
            None => 0,
        };

        let flat_exp = parse_bounded_u32(ip, 0x0645, 4, 0..=16)?;
        let flatness_threshold = 1i32.checked_shl(flat_exp)?;

        let run_threshold = parse_bounded_i32(ip, 0x0646, 3, 0..=32)?;
        let table_a = (parse_bounded_i32(ip, 0x0647, 2, 0..=15)?, parse_bounded_i32(ip, 0x0648, 3, 0..=15)?);
        let table_b = (parse_bounded_i32(ip, 0x0649, 4, 0..=15)?, parse_bounded_i32(ip, 0x0650, 7, 0..=15)?);

        let max_rice_k = parse_bounded_i32(ip, 0x0651, 15, 0..=15)?;
        let max_unary_run = parse_bounded_u32(ip, 0x0652, 12, 1..=32)?;

        let sensor_bits = match mn.and_then(|m| m.ifd.u64(MAKERNOTE_VALID_BITS_TAG)) {
            Some(raw) => {
                let b = u32::try_from(raw).ok()?;
                if (8..=16).contains(&b) {
                    b
                } else {
                    return None;
                }
            }
            None => 12,
        };

        Some(Self {
            direct_low_bits,
            gradient_threshold,
            boundary_baseline,
            residual_sub_bits,
            feedback_shift,
            flatness_threshold,
            run_threshold,
            table_a,
            table_b,
            max_rice_k,
            max_unary_run,
            header_prefix_len,
            sensor_bits,
        })
    }
}

/// Helper to parse an optional unsigned 32-bit tag with range validation, falling back to default when absent.
fn parse_bounded_u32(ip: &Ifd, tag: u16, default: u32, range: std::ops::RangeInclusive<u32>) -> Option<u32> {
    match ip.u64(tag) {
        Some(raw) => {
            let val = u32::try_from(raw).ok()?;
            if range.contains(&val) { Some(val) } else { None }
        }
        None => Some(default),
    }
}

/// Helper to parse an optional signed 32-bit tag with range validation, falling back to default when absent.
fn parse_bounded_i32(ip: &Ifd, tag: u16, default: i32, range: std::ops::RangeInclusive<i32>) -> Option<i32> {
    match ip.u64(tag) {
        Some(raw) => {
            let val = i32::try_from(raw).ok()?;
            if range.contains(&val) { Some(val) } else { None }
        }
        None => Some(default),
    }
}

/// Verify that the compressed strip begins with $N$ big-endian bytes equal to 1, followed by `[0x00, 0x00]`.
pub(crate) fn verify_stream_header(data: &[u8], prefix_len: usize) -> bool {
    if prefix_len == 0 || prefix_len > 8 || data.len() < prefix_len.saturating_add(2) {
        return false;
    }
    let mut integer_acc = 0u64;
    for &b in data.get(..prefix_len).unwrap_or(&[]) {
        integer_acc = (integer_acc << 8) | u64::from(b);
    }
    if integer_acc != 1 {
        return false;
    }
    data.get(prefix_len..prefix_len.saturating_add(2)) == Some(&[0x00, 0x00])
}

/// MSB-first bitstream reader with a 64-bit internal accumulator.
struct MsbBitReader<'a> {
    stream: &'a [u8],
    cursor: usize,
    accumulator: u64,
    bits_in_acc: u32,
    bits_consumed_total: u64,
}

impl<'a> MsbBitReader<'a> {
    fn new(stream: &'a [u8]) -> Self {
        Self { stream, cursor: 0, accumulator: 0, bits_in_acc: 0, bits_consumed_total: 0 }
    }

    /// Pull bytes from the input slice into the accumulator until at least `needed` bits are present.
    fn refill(&mut self, needed: u32) {
        while self.bits_in_acc < needed && self.cursor < self.stream.len() {
            let next_byte = self.stream.get(self.cursor).copied().unwrap_or(0);
            self.cursor = self.cursor.saturating_add(1);
            self.accumulator = (self.accumulator << 8) | u64::from(next_byte);
            self.bits_in_acc = self.bits_in_acc.saturating_add(8);
        }
    }

    /// Read $n$ bits ($0 \le n \le 32$) from the stream.
    fn read_bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.bits_in_acc < n {
            self.refill(n);
        }
        self.bits_consumed_total = self.bits_consumed_total.saturating_add(u64::from(n));
        if self.bits_in_acc >= n {
            let shift = self.bits_in_acc.saturating_sub(n);
            let val = (self.accumulator >> shift) & ((1u64 << n) - 1);
            self.bits_in_acc = shift;
            val as u32
        } else {
            // Reached EOF unexpectedly: shift whatever bits remain and pad with zeros
            let available = self.bits_in_acc;
            let val = self.accumulator & ((1u64 << available) - 1);
            self.bits_in_acc = 0;
            (val << (n - available)) as u32
        }
    }

    /// Read a single bit from the stream.
    fn read_single_bit(&mut self) -> u32 {
        if self.bits_in_acc == 0 {
            self.refill(1);
        }
        self.bits_consumed_total = self.bits_consumed_total.saturating_add(1);
        if self.bits_in_acc > 0 {
            self.bits_in_acc = self.bits_in_acc.saturating_sub(1);
            ((self.accumulator >> self.bits_in_acc) & 1) as u32
        } else {
            0
        }
    }
}

/// Decoded symbol representation for a single photosite.
struct DecodedResidual {
    signed_delta: i32,
    raw_magnitude: u32,
    sub_bits: u32,
    low_bits: u32,
}

/// Decode the next entropy symbol per US Patent US7333035B2.
fn decode_entropy_symbol(reader: &mut MsbBitReader<'_>, rice_k: u32, cfg: &OrfCompressionConfig) -> DecodedResidual {
    let sign_bit = reader.read_single_bit();
    let sub_bits = if cfg.residual_sub_bits > 0 { reader.read_bits(cfg.residual_sub_bits) } else { 0 };
    let low_bits = if cfg.direct_low_bits > 0 { reader.read_bits(cfg.direct_low_bits) } else { 0 };

    let mut quotient = 0u32;
    loop {
        let bit = reader.read_single_bit();
        if bit != 0 {
            break;
        }
        quotient = quotient.saturating_add(1);
        if quotient == cfg.max_unary_run {
            quotient = 0;
            if rice_k < 15 {
                let escape_bits = 15u32.saturating_sub(rice_k);
                quotient = reader.read_bits(escape_bits);
            }
            // Consume and discard trailing boundary bit
            let _ = reader.read_single_bit();
            break;
        }
    }

    let remainder = if rice_k > 0 { reader.read_bits(rice_k) } else { 0 };
    let raw_magnitude = (quotient << rice_k) | remainder;

    let signed_delta = if sign_bit == 1 {
        // Two's complement negative residual mapping: -(M + 1)
        (raw_magnitude ^ 0xffff_ffff) as i32
    } else {
        raw_magnitude as i32
    };

    DecodedResidual { signed_delta, raw_magnitude, sub_bits, low_bits }
}

/// Directional edge-directed spatial predictor over Bayer channels per US Patent US7333035B2.
fn predict_spatial_baseline(west: i32, north: i32, north_west: i32, gradient_threshold: u32) -> i32 {
    let mut baseline = north;

    if west < north || north_west < west {
        if north_west <= north && west >= north {
            baseline = west;
        } else if north_west < north || west >= north {
            if north_west > west || west >= north {
                let diff_vert = north.abs_diff(north_west);
                let diff_horiz = west.abs_diff(north_west);
                if diff_vert > gradient_threshold {
                    baseline = north.saturating_add(west.saturating_sub(north_west));
                } else if diff_horiz <= gradient_threshold {
                    baseline = (north.saturating_add(west)) >> 1;
                } else {
                    baseline = north.saturating_add(west.saturating_sub(north_west));
                }
            }
        } else {
            baseline = west;
        }
    }

    baseline
}

/// Decompress an entire Olympus compressed entropy bitstream into mosaic sensor samples.
pub(crate) fn decompress_stream(stream: &[u8], width: usize, height: usize, config: &OrfCompressionConfig) -> Result<Vec<u16>> {
    let total_pixels = width.checked_mul(height).ok_or(RawError::Limit("ORF image dimensions overflow"))?;
    if total_pixels == 0 {
        return Ok(Vec::new());
    }
    if stream.is_empty() {
        return Err(RawError::Corrupt("ORF compressed bitstream is empty".into()));
    }

    // Safety guard: require at least 1 bit per pixel available in the input slice
    if (stream.len() as u64).saturating_mul(8) < (total_pixels as u64) {
        return Err(RawError::Corrupt("ORF compressed stream shorter than expected frame".into()));
    }

    let mut pixels = vec![0u16; total_pixels];
    let mut reader = MsbBitReader::new(stream);

    let mut flat_history = [0i32; 4];
    let mut last_reconstructed_sum = 0i32;
    let mut feedback_predictor = 0i32;
    let mut prev_magnitude = 0i32;
    let mut curr_magnitude = 0u32;

    for row in 0..height {
        let row_offset = row.checked_mul(width).ok_or(RawError::Limit("ORF row offset overflow"))?;

        for col in 0..width {
            let p = prev_magnitude;
            prev_magnitude = curr_magnitude as i32;

            // Context adaptation: flatness detection
            let flat_step = if col > 1 && p <= config.flatness_threshold { flat_history[3].saturating_add(1) } else { 0 };
            flat_history[3] = flat_history[2];
            flat_history[2] = flat_step;

            // Derive Rice parameter k for the current photosite
            let mut rice_k = if col > 1 {
                let p16 = (p as u32 & 0xffff) as u16;
                let high_bit_pos = if p16 == 0 { 0i32 } else { (16 - p16.leading_zeros()) as i32 };
                let (table_base, table_limit) =
                    if flat_step >= config.run_threshold { (config.table_a.0, config.table_a.1) } else { (config.table_b.0, config.table_b.1) };
                let offset = (high_bit_pos - table_limit).max(-1);
                table_base.saturating_add(offset).saturating_add(1)
            } else {
                config.table_b.0
            };

            if rice_k > config.max_rice_k {
                rice_k = config.max_rice_k;
            }
            if !(0..=15).contains(&rice_k) {
                return Err(RawError::Corrupt("ORF Rice parameter out of valid range".into()));
            }

            // Decode entropy symbol
            let symbol = decode_entropy_symbol(&mut reader, rice_k as u32, config);
            if reader.bits_consumed_total > (stream.len() as u64).saturating_mul(8) {
                return Err(RawError::Corrupt("ORF compressed stream ended unexpectedly".into()));
            }
            let residual = symbol.signed_delta;
            curr_magnitude = symbol.raw_magnitude;

            // Residual sub-bit adjustment (US7715639B2)
            let (adjusted_residual, feedback_bits) = if config.residual_sub_bits > 0 {
                let v = if col < 2 { 0 } else { feedback_predictor };
                let v_plus_res = v.checked_add(residual).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;

                if config.residual_sub_bits == 1 {
                    let g_mult = v_plus_res.checked_mul(2).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    let g = (symbol.sub_bits as i32).checked_add(g_mult).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    let v_res = v.checked_add(g >> 1).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    (g, v_res)
                } else {
                    let g_mult = v_plus_res.checked_mul(4).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    let g = (symbol.sub_bits as i32).checked_add(g_mult).ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    let half_g = ((g >> 1) as u32 & 0xffff_fffe) as i32;
                    let v_res = half_g
                        .checked_add(v)
                        .and_then(|x| x.checked_add(g >> 2))
                        .ok_or_else(|| RawError::Corrupt("ORF arithmetic overflow".into()))?;
                    (g, v_res)
                }
            } else {
                (residual, 0)
            };

            feedback_predictor = flat_history[0];
            flat_history[0] = feedback_bits >> config.feedback_shift.min(30);

            // Fetch spatial Bayer neighbors (stride 2)
            let west = if col < 2 { config.boundary_baseline } else { flat_history[1] };
            let north = if row < 2 {
                config.boundary_baseline
            } else {
                pixels.get((row - 2) * width + col).map_or(config.boundary_baseline, |&val| (val as i32) >> config.direct_low_bits.min(15))
            };
            let north_west = if row < 2 || col < 2 {
                config.boundary_baseline
            } else {
                pixels.get((row - 2) * width + col - 2).map_or(config.boundary_baseline, |&val| (val as i32) >> config.direct_low_bits.min(15))
            };

            flat_history[1] = last_reconstructed_sum;

            // Directional spatial prediction
            let predicted_baseline = predict_spatial_baseline(west, north, north_west, config.gradient_threshold);

            // Final photosite sample reconstruction
            let reconstructed_sum =
                adjusted_residual.checked_add(predicted_baseline).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
            last_reconstructed_sum = reconstructed_sum;

            let final_sample = i64::from(symbol.low_bits).saturating_add(i64::from(reconstructed_sum) << config.direct_low_bits.min(15));

            if let Some(pixel_slot) = pixels.get_mut(row_offset + col) {
                *pixel_slot = final_sample.clamp(0, i64::from(u16::MAX)) as u16;
            }
        }
    }

    // Safety verification: confirm bitstream consumption remained within stream bounds
    if reader.bits_consumed_total > (stream.len() as u64).saturating_mul(8) {
        return Err(RawError::Corrupt("ORF compressed stream ended unexpectedly".into()));
    }

    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_header_verification() {
        assert!(verify_stream_header(&[0, 0, 0, 1, 0, 0, 0xaa], 4));
        assert!(verify_stream_header(&[0, 0, 0, 0, 1, 0, 0, 0xbb], 5));
        assert!(verify_stream_header(&[0, 0, 0, 0, 0, 0, 0, 1, 0, 0], 8));

        // Invalid cases
        assert!(!verify_stream_header(&[0, 0, 0, 1, 0, 1], 4)); // Non-zero delimiter
        assert!(!verify_stream_header(&[0, 0, 0, 2, 0, 0], 4)); // Prefix not equal to 1
        assert!(!verify_stream_header(&[0, 0, 0, 1], 4)); // Truncated before delimiter
        assert!(!verify_stream_header(&[], 0)); // Empty slice
        assert!(!verify_stream_header(&[0; 16], 9)); // Prefix length > 8
    }

    #[test]
    fn test_bit_reader_msb_ordering() {
        let payload = [0b1011_0001, 0b1100_1010];
        let mut r = MsbBitReader::new(&payload);

        assert_eq!(r.read_single_bit(), 1);
        assert_eq!(r.read_single_bit(), 0);
        assert_eq!(r.read_bits(2), 0b11);
        assert_eq!(r.read_bits(4), 0b0001);
        assert_eq!(r.read_bits(8), 0b1100_1010);

        // Past EOF pads with zeros without panic
        assert_eq!(r.read_bits(4), 0);
        assert_eq!(r.read_single_bit(), 0);
    }

    #[test]
    fn test_predictor_bounds_and_symmetry() {
        assert_eq!(i32::MIN.abs_diff(0), 2_147_483_648u32);
        assert_eq!(i32::MIN.abs_diff(i32::MAX), 4_294_967_295u32);

        let p1 = predict_spatial_baseline(100, 100, 100, 32);
        assert_eq!(p1, 100);

        let p2 = predict_spatial_baseline(100, 200, 100, 32);
        assert!((100..=200).contains(&p2));
    }

    #[test]
    fn test_decompress_hostile_inputs() {
        let config = OrfCompressionConfig {
            direct_low_bits: 0,
            gradient_threshold: 32,
            boundary_baseline: 0,
            residual_sub_bits: 2,
            feedback_shift: 5,
            flatness_threshold: 16,
            run_threshold: 3,
            table_a: (2, 3),
            table_b: (4, 7),
            max_rice_k: 15,
            max_unary_run: 12,
            header_prefix_len: 4,
            sensor_bits: 12,
        };

        // Empty stream on 0x0 succeeds
        assert_eq!(decompress_stream(&[], 0, 0, &config).unwrap(), Vec::<u16>::new());

        // Empty stream on non-zero dimensions fails cleanly
        assert!(decompress_stream(&[], 10, 10, &config).is_err());

        // Truncated stream fails cleanly without panicking
        assert!(decompress_stream(&[0xff; 2], 20, 20, &config).is_err());

        // Stream with random garbage does not panic
        for fill in [0x00, 0x55, 0xaa, 0xff] {
            let buf = vec![fill; 128];
            let _ = decompress_stream(&buf, 8, 8, &config);
        }
    }
}
