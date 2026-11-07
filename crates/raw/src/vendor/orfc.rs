//! Olympus compressed ORF entropy and predictive decoding.
//!
//! Clean-room implementation derived strictly from patent specifications and black-box CC0 samples:
//! - Olympus Patent US7333035B2 (Takashi Ishikawa): directional edge-directed prediction across Bayer channels
//!   and run-length adaptive Golomb-Rice entropy coding with quotient escape codes.
//! - Olympus Patent US7715639B2 (Takashi Ishikawa): direct low-order bitfield splitting and residual difference
//!   correction filters.
//! - ImageProcessing maker-note directory (`0x2040`) parameters (`0x0640`–`0x0653`) and valid sensor bit depth (`0x102c`).
//! - Verification against CC0 sample files from raw.pixls.us (OM-D E-M1, OM-D E-M10 Mark III).
//!
//! No LibRaw, dcraw, darktable, RawTherapee, or other GPL/LGPL/AGPL code was consulted or used.
//! See `docs/orf-compression.md` for format specification and verification records.

use crate::{RawError, Result};
use lightcraft_tiff::Ifd;
use lightcraft_tiff::makernote::MakerNote;

/// Valid bits tag in Olympus MakerNote.
const MN_VALID_BITS: u16 = 0x102c;

/// Bitstream reader for big-endian, MSB-first compressed ORF bitstreams.
struct BitStream<'a> {
    data: &'a [u8],
    cursor: usize,
    buffer: u32,
    bits_available: u32,
    bits_loaded: u64,
}

impl<'a> BitStream<'a> {
    /// Create a new bitstream reader over the supplied byte slice.
    fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0, buffer: 0, bits_available: 0, bits_loaded: 0 }
    }

    /// Refill the bit buffer with a 32-bit big-endian word.
    fn refill_word(&mut self) {
        self.bits_loaded = self.bits_loaded.saturating_add(32);
        if let Some(chunk) = self.data.get(self.cursor..self.cursor.saturating_add(4)) {
            self.buffer = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            self.cursor = self.cursor.saturating_add(4);
            self.bits_available = 32;
        } else {
            let mut chunk = [0u8; 4];
            let remaining = self.data.len().saturating_sub(self.cursor);
            if remaining > 0 {
                if let Some(src) = self.data.get(self.cursor..self.cursor.saturating_add(remaining)) {
                    chunk[..remaining].copy_from_slice(src);
                }
                self.cursor = self.data.len();
            }
            self.buffer = u32::from_be_bytes(chunk);
            self.bits_available = 32;
        }
    }

    /// Append up to 16 bits to the lower part of the active bit buffer.
    fn refill_halfword(&mut self) {
        self.bits_loaded = self.bits_loaded.saturating_add(16);
        let next = if let Some(chunk) = self.data.get(self.cursor..self.cursor.saturating_add(2)) {
            self.cursor = self.cursor.saturating_add(2);
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else if let Some(&single) = self.data.get(self.cursor) {
            self.cursor = self.cursor.saturating_add(1);
            (single as u16) << 8
        } else {
            0
        };
        self.buffer = (self.buffer << 16) | u32::from(next);
        self.bits_available = self.bits_available.saturating_add(16);
    }

    /// Read up to 16 bits from the bitstream.
    fn read_bits(&mut self, width: u32) -> u32 {
        if width == 0 {
            return 0;
        }
        while self.bits_available < width {
            self.refill_halfword();
        }
        let shift = self.bits_available & 0x1f;
        let value = (self.buffer.wrapping_shl((32 - shift) & 0x1f)) >> (32 - width);
        self.bits_available = self.bits_available.saturating_sub(width);
        value
    }

    /// Advance by one bit, refilling the 32-bit buffer if necessary.
    fn advance_single_bit(&mut self, state: &mut u32) {
        if *state == 0 {
            self.refill_word();
            self.bits_available = self.bits_available.saturating_sub(1);
            *state = self.bits_available;
        } else {
            *state = state.saturating_sub(1);
            self.bits_available = *state;
        }
    }
}

/// Parameters extracted from ImageProcessing SubIFD (`0x2040`) controlling compressed ORF decoding.
pub(crate) struct CompressionParameters {
    /// Low-order direct bits extracted without prediction (tag `0x0640`).
    pub(crate) direct_bits_low0: u32,
    /// Gradient difference exponent threshold for edge classification (tag `0x0641`).
    pub(crate) gradient_thresh_exp_641: u32,
    /// Default spatial predictor boundary value (tag `0x0642`).
    pub(crate) boundary_predictor_642: i32,
    /// Direct residual sub-bits width (tag `0x0643`).
    pub(crate) direct_bits_sub3: u32,
    /// Vertical feedback shift exponent (tag `0x0644`).
    pub(crate) feedback_shift_644: u32,
    /// Flatness threshold exponent (tag `0x0645`).
    pub(crate) flatness_thresh_exp_645: u32,
    /// Flatness run threshold (tag `0x0646`).
    pub(crate) run_thresh_646: i32,
    /// Adaptive Rice parameter table offset A (tag `0x0647`).
    pub(crate) table_647: i32,
    /// Adaptive Rice parameter table limit A (tag `0x0648`).
    pub(crate) table_648: i32,
    /// Adaptive Rice parameter table offset B (tag `0x0649`).
    pub(crate) table_649: i32,
    /// Adaptive Rice parameter table limit B (tag `0x0650`).
    pub(crate) table_650: i32,
    /// Maximum Rice parameter allowed (tag `0x0651`).
    pub(crate) max_rice_k: i32,
    /// Maximum unary quotient run length before escape coding (tag `0x0652`).
    pub(crate) max_unary_run: u32,
    /// Prefix length of the compressed stream header in bytes (tag `0x0653`).
    pub(crate) header_prefix_len: usize,
    /// Valid sensor bit depth from MakerNote `0x102c`.
    pub(crate) valid_bits: u32,
}

impl CompressionParameters {
    /// Parse and validate compression parameters from the ImageProcessing IFD and MakerNote.
    pub(crate) fn from_ifd(ip: &Ifd, mn: Option<&MakerNote>) -> Option<Self> {
        let header_prefix_len = usize::try_from(ip.u64(0x0653)?).ok()?;
        if header_prefix_len == 0 || header_prefix_len > 8 {
            return None;
        }
        if ip.u64(0x0640).is_some_and(|v| v > 16) || ip.u64(0x0643).is_some_and(|v| v > 16) {
            return None;
        }
        if ip.u64(0x0644).is_some_and(|v| v == 0 || v > 15) {
            return None;
        }
        let direct_bits_low0 = ip.u64(0x0640).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let gradient_thresh_exp_641 = ip.u64(0x0641).and_then(|v| u32::try_from(v).ok()).unwrap_or(5);
        let boundary_predictor_642 = ip.i64(0x0642).and_then(|v| i32::try_from(v).ok()).filter(|&v| (-65536..=65536).contains(&v)).unwrap_or(0);
        let direct_bits_sub3 = ip.u64(0x0643).and_then(|v| u32::try_from(v).ok()).unwrap_or(2);
        let feedback_shift_644 = ip.u64(0x0644).and_then(|v| u32::try_from(v).ok()).unwrap_or(5);
        let flatness_thresh_exp_645 = ip.u64(0x0645).and_then(|v| u32::try_from(v).ok()).unwrap_or(4);
        let run_thresh_646 = ip.u64(0x0646).and_then(|v| i32::try_from(v).ok()).unwrap_or(3);
        let table_647 = ip.u64(0x0647).and_then(|v| i32::try_from(v).ok()).unwrap_or(2);
        let table_648 = ip.u64(0x0648).and_then(|v| i32::try_from(v).ok()).unwrap_or(3);
        let table_649 = ip.u64(0x0649).and_then(|v| i32::try_from(v).ok()).unwrap_or(4);
        let table_650 = ip.u64(0x0650).and_then(|v| i32::try_from(v).ok()).unwrap_or(7);
        let max_rice_k = ip.u64(0x0651).and_then(|v| i32::try_from(v).ok()).unwrap_or(15);
        let max_unary_run = ip.u64(0x0652).and_then(|v| u32::try_from(v).ok()).unwrap_or(12).clamp(1, 32);
        let valid_bits = mn.and_then(|m| m.ifd.u64(MN_VALID_BITS)).and_then(|v| u32::try_from(v).ok()).filter(|b| (8..=16).contains(b)).unwrap_or(12);

        Some(Self {
            direct_bits_low0,
            gradient_thresh_exp_641,
            boundary_predictor_642,
            direct_bits_sub3,
            feedback_shift_644,
            flatness_thresh_exp_645,
            run_thresh_646,
            table_647,
            table_648,
            table_649,
            table_650,
            max_rice_k,
            max_unary_run,
            header_prefix_len,
            valid_bits,
        })
    }
}

/// Check the stream header prefix: $N$ big-endian bytes equal to 1, followed by two zero bytes.
pub(crate) fn validate_stream_header(src: &[u8], prefix_bytes: usize) -> bool {
    if prefix_bytes == 0 || prefix_bytes > 8 || src.len() < prefix_bytes.saturating_add(2) {
        return false;
    }
    let mut prefix_int = 0u64;
    for &b in src.get(..prefix_bytes).unwrap_or(&[]) {
        prefix_int = (prefix_int << 8) | u64::from(b);
    }
    if prefix_int != 1 {
        return false;
    }
    let Some(delimiter) = src.get(prefix_bytes..prefix_bytes.saturating_add(2)) else {
        return false;
    };
    delimiter == [0x00, 0x00]
}

/// Decoded entropy-coded symbol components for one photosite.
struct DecodedSymbol {
    signed_residual: i32,
    raw_magnitude: u32,
    direct_bits_low0: u32,
    direct_bits_sub3: u32,
}

/// Decode the next Golomb-Rice entropy residual symbol according to the patent specification.
fn decode_entropy_symbol(reader: &mut BitStream<'_>, rice_k: u32, params: &CompressionParameters) -> DecodedSymbol {
    if reader.bits_available == 0 {
        reader.refill_word();
    }
    reader.bits_available = reader.bits_available.saturating_sub(1);
    let sign_bit = (reader.buffer >> (reader.bits_available & 0x1f)) & 1;

    let direct_bits_sub3 = if params.direct_bits_sub3 > 0 { reader.read_bits(params.direct_bits_sub3) } else { 0 };
    let direct_bits_low0 = if params.direct_bits_low0 > 0 { reader.read_bits(params.direct_bits_low0) } else { 0 };

    let mut state = reader.bits_available;
    let mut quotient = 0u32;

    loop {
        reader.advance_single_bit(&mut state);
        if ((reader.buffer >> (state & 0x1f)) & 1) != 0 {
            break;
        }
        quotient = quotient.saturating_add(1);
        if quotient == params.max_unary_run {
            quotient = 0;
            if rice_k != 15 {
                let bits_needed = 15u32.saturating_sub(rice_k);
                while state < bits_needed {
                    reader.refill_halfword();
                    state = reader.bits_available;
                }
                let shift = state & 0x1f;
                quotient = (reader.buffer.wrapping_shl((32 - shift) & 0x1f)) >> (rice_k + 17);
                state = state.saturating_sub(bits_needed);
                reader.bits_available = state;
            }
            reader.advance_single_bit(&mut state);
            break;
        }
    }

    let remainder = if rice_k == 0 {
        0
    } else {
        while state < rice_k {
            reader.refill_halfword();
            state = reader.bits_available;
        }
        let shift = state & 0x1f;
        let rem = (reader.buffer.wrapping_shl((32 - shift) & 0x1f)) >> (32 - (rice_k & 0x1f));
        reader.bits_available = state.saturating_sub(rice_k);
        rem
    };

    let raw_magnitude = remainder | (quotient << rice_k);
    let sign_mask = if sign_bit != 0 { 0xffff_ffff } else { 0 };
    let signed_residual = (raw_magnitude ^ sign_mask) as i32;

    DecodedSymbol { signed_residual, raw_magnitude, direct_bits_low0, direct_bits_sub3 }
}

/// Decompress an entire Olympus compressed entropy bitstream into mosaic sensor samples.
pub(crate) fn decompress_stream(stream: &[u8], width: usize, height: usize, params: &CompressionParameters) -> Result<Vec<u16>> {
    let total_pixels = width.checked_mul(height).ok_or(RawError::Limit("ORF image dimensions overflow"))?;
    if total_pixels > 0 && stream.is_empty() {
        return Err(RawError::Corrupt("ORF compressed stream is empty".into()));
    }
    // Reject streams too short to encode even 1 bit per pixel (prevents large allocation
    // from a truncated file before we start decoding).
    if total_pixels > 0 && (stream.len() as u64).saturating_mul(8) < total_pixels as u64 {
        return Err(RawError::Corrupt("ORF compressed stream too short for frame".into()));
    }
    let mut pixels = vec![0u16; total_pixels];
    let mut reader = BitStream::new(stream);

    let gradient_threshold = 1u32.checked_shl(params.gradient_thresh_exp_641.min(30)).unwrap_or(1 << 5);
    let flatness_threshold = 1i32.checked_shl(params.flatness_thresh_exp_645.min(30)).unwrap_or(1 << 4);

    let mut context = [0i32; 4];
    let mut last_predicted_sum = 0i32;
    let mut pred1 = 0i32;
    let mut raw_magnitude = 0u32;
    let mut prev_magnitude_signed = 0i32;

    for row in 0..height {
        let row_offset = row.checked_mul(width).ok_or(RawError::Limit("ORF row overflow"))?;
        for col in 0..width {
            let p = prev_magnitude_signed;
            prev_magnitude_signed = raw_magnitude as i32;

            let psel = if col > 1 && flatness_threshold >= p { context[3].saturating_add(1) } else { 0 };
            context[3] = context[2];
            context[2] = psel;

            let mut rice_k = if col > 1 {
                let p16 = (p as u32 & 0xffff) as u16;
                let high_bit_count = if p16 == 0 { 0i32 } else { (16 - p16.leading_zeros()) as i32 };
                let t1 = if psel >= params.run_thresh_646 { params.table_648 } else { params.table_650 };
                let t2 = if psel >= params.run_thresh_646 { params.table_647 } else { params.table_649 };
                let tt = (high_bit_count - t1).max(-1);
                t2.saturating_add(tt).saturating_add(1)
            } else {
                params.table_649
            };

            if rice_k > params.max_rice_k {
                rice_k = params.max_rice_k;
            }
            if rice_k >= 16 {
                return Err(RawError::Corrupt("ORF compressed Rice parameter out of range".into()));
            }

            let symbol = decode_entropy_symbol(&mut reader, rice_k.max(0) as u32, params);
            let residual = symbol.signed_residual;
            raw_magnitude = symbol.raw_magnitude;
            let direct_bits_low0 = symbol.direct_bits_low0;
            let direct_bits_sub3 = symbol.direct_bits_sub3;

            let (adjusted_residual, feedback_bits) = if params.direct_bits_sub3 > 0 {
                let v = if col < 2 { 0 } else { pred1 };
                let v_plus_g = v.checked_add(residual).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                if params.direct_bits_sub3 == 1 {
                    let g_mult = v_plus_g.checked_mul(2).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    let g = (direct_bits_sub3 as i32).checked_add(g_mult).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    let v_res = v.checked_add(g >> 1).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    (g, v_res)
                } else {
                    let g_mult = v_plus_g.checked_mul(4).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    let g = (direct_bits_sub3 as i32).checked_add(g_mult).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    let half_g = ((g >> 1) as u32 & 0xffff_fffe) as i32;
                    let v_res = half_g
                        .checked_add(v)
                        .and_then(|x| x.checked_add(g >> 2))
                        .ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
                    (g, v_res)
                }
            } else {
                (residual, 0)
            };

            pred1 = context[0];
            context[0] = feedback_bits >> params.feedback_shift_644.min(30);

            let west = if col < 2 { params.boundary_predictor_642 } else { context[1] };
            let mut north = if row < 2 {
                params.boundary_predictor_642
            } else {
                pixels.get((row - 2) * width + col).map_or(params.boundary_predictor_642, |&v| (v as i32) >> params.direct_bits_low0.min(15))
            };
            let north_west = if row < 2 || col < 2 {
                params.boundary_predictor_642
            } else {
                pixels.get((row - 2) * width + col - 2).map_or(params.boundary_predictor_642, |&v| (v as i32) >> params.direct_bits_low0.min(15))
            };

            context[1] = last_predicted_sum;

            // Directional edge-directed spatial prediction across Bayer channels (2-step distance)
            if west < north || north_west < west {
                if north_west <= north && west >= north {
                    north = west;
                } else if north_west < north || west >= north {
                    if north_west > west || west >= north {
                        let diff_n_nw = north.abs_diff(north_west);
                        let diff_w_nw = west.abs_diff(north_west);
                        if diff_n_nw > gradient_threshold {
                            north = north.saturating_add(west.saturating_sub(north_west));
                        } else if diff_w_nw <= gradient_threshold {
                            north = (north.saturating_add(west)) >> 1;
                        } else {
                            north = north.saturating_add(west.saturating_sub(north_west));
                        }
                    }
                } else {
                    north = west;
                }
            }

            let sum = adjusted_residual.checked_add(north).ok_or_else(|| RawError::Corrupt("ORF predictor overflow".into()))?;
            last_predicted_sum = sum;
            let final_sample = i64::from(direct_bits_low0).saturating_add(i64::from(sum) << params.direct_bits_low0.min(15));
            if let Some(px) = pixels.get_mut(row_offset + col) {
                *px = final_sample.clamp(0, i64::from(u16::MAX)) as u16;
            }
        }
    }

    let consumed_bits = reader.bits_loaded.saturating_sub(u64::from(reader.bits_available));
    if consumed_bits > (stream.len() as u64).saturating_mul(8) {
        return Err(RawError::Corrupt("ORF compressed bitstream terminates prematurely".into()));
    }

    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_header_validation() {
        assert!(validate_stream_header(&[0, 0, 0, 1, 0, 0], 4));
        assert!(validate_stream_header(&[0, 0, 0, 0, 0, 0, 0, 1, 0, 0], 8));
        assert!(!validate_stream_header(&[0, 0, 0, 2, 0, 0], 4)); // prefix != 1
        assert!(!validate_stream_header(&[0, 0, 0, 1, 0, 1], 4)); // trailer != [0, 0]
        assert!(!validate_stream_header(&[0, 0, 0, 0], 5)); // truncated
        assert!(!validate_stream_header(&[], 0)); // zero length
        assert!(!validate_stream_header(&[0; 20], 9)); // > 8 bytes
    }

    #[test]
    fn bitstream_reader_operations() {
        let data = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        let mut stream = BitStream::new(&data);
        assert_eq!(stream.read_bits(4), 1);
        assert_eq!(stream.read_bits(4), 2);
        assert_eq!(stream.read_bits(8), 0x34);
        assert_eq!(stream.read_bits(16), 0x5678);
        assert_eq!(stream.read_bits(8), 0x9a);
        assert_eq!(stream.read_bits(8), 0xbc);
        // past EOF reads padded zeros without panicking
        assert_eq!(stream.read_bits(8), 0);
    }

    #[test]
    fn decompress_bounds_and_safety() {
        let params = CompressionParameters {
            direct_bits_low0: 0,
            gradient_thresh_exp_641: 5,
            boundary_predictor_642: 0,
            direct_bits_sub3: 2,
            feedback_shift_644: 5,
            flatness_thresh_exp_645: 4,
            run_thresh_646: 3,
            table_647: 2,
            table_648: 3,
            table_649: 4,
            table_650: 7,
            max_rice_k: 15,
            max_unary_run: 12,
            header_prefix_len: 5,
            valid_bits: 12,
        };
        // empty stream returns error for nonempty image, succeeds for 0x0
        assert!(decompress_stream(&[], 10, 10, &params).is_err());
        assert_eq!(decompress_stream(&[], 0, 0, &params).unwrap(), Vec::<u16>::new());
        // truncated stream (too short for requested dimensions) returns error
        assert!(decompress_stream(&[0xff; 2], 10, 10, &params).is_err());
        assert!(decompress_stream(&[0x00; 2], 10, 10, &params).is_err());

        // invalid Rice parameter returns error
        let mut bad_params = params;
        bad_params.table_649 = 20;
        bad_params.max_rice_k = 20;
        assert!(decompress_stream(&[0xff; 32], 4, 4, &bad_params).is_err());
    }

    #[test]
    fn predictor_extreme_difference_never_panics() {
        assert_eq!(i32::MIN.abs_diff(0), 2_147_483_648u32);
        assert_eq!(i32::MIN.abs_diff(i32::MAX), 4_294_967_295u32);

        let params = CompressionParameters {
            direct_bits_low0: 0,
            gradient_thresh_exp_641: 5,
            boundary_predictor_642: 0,
            direct_bits_sub3: 0,
            feedback_shift_644: 5,
            flatness_thresh_exp_645: 4,
            run_thresh_646: 3,
            table_647: 2,
            table_648: 3,
            table_649: 4,
            table_650: 7,
            max_rice_k: 15,
            max_unary_run: 12,
            header_prefix_len: 5,
            valid_bits: 12,
        };
        for fill in [0x00, 0x55, 0xaa, 0xff] {
            let stream = vec![fill; 64];
            let _ = decompress_stream(&stream, 4, 4, &params);
        }
    }
}
