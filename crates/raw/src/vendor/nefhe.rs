//! Nikon High Efficiency / HE★, the four-component JPEG XS-derived NEF profile.
//!
//! Original adapter around `oxideav-jpegxs` (MIT, pure Rust). Header layout, band/packet order and
//! reconstruction were checked against camera files and independently decoded mosaics. Nikon uses
//! half-resolution component grids, per-component decomposition in CDT, long packet headers, and
//! horizontal-before-vertical inverse lifting. See `docs/nikon-he.md` for provenance and limits.

use crate::{RawError, Result};
use oxideav_jpegxs::entropy::packet_body::PrecinctState;
use oxideav_jpegxs::entropy::{self, BandGeometry, PacketEntry, PacketLayout, PrecinctGeometry, PrecinctTop};

const MAX_PIXELS: usize = 64 * 1024 * 1024;
const BANDS: usize = 25;
pub(super) const MAGIC: [u8; 4] = [0xff, 0x10, 0xff, 0x50];

fn corrupt(why: impl std::fmt::Display) -> RawError {
    RawError::Corrupt(format!("Nikon High Efficiency NEF: {why}"))
}

fn unsupported(why: &str) -> RawError {
    RawError::Unsupported(format!("Nikon High Efficiency NEF ({why})"))
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (value, rest) = self.rest.split_at_checked(n).ok_or_else(|| corrupt("truncated codestream"))?;
        self.rest = rest;
        Ok(value)
    }

    fn marker(&mut self, marker: [u8; 2]) -> Result<&'a [u8]> {
        if self.take(2)? != marker {
            return Err(corrupt(format!("expected marker {:02x}{:02x}", marker[0], marker[1])));
        }
        let len = self.take(2)?;
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        self.take(len.checked_sub(2).ok_or_else(|| corrupt("invalid marker length"))?)
    }
}

#[derive(Clone, Copy)]
struct Transfer {
    black: u16,
    gain: u32,
    pivot: u16,
}

impl Transfer {
    /// Integer transfer rule documented by RdWing (2026-09-11); round the Q16 gain half up,
    /// then square. The final bias incorporates both rounding stages of the 14-bit output.
    fn apply(self, value: i32) -> u16 {
        let d = (i64::from(value) + 32768).clamp(0, 65535) - i64::from(self.pivot);
        let q = if d < 0 {
            -d * d
        } else {
            let t = (d * i64::from(self.gain) + 32768) >> 16;
            t * t
        };
        (i64::from(self.black) + ((q + 163840) >> 18)).clamp(0, 16383) as u16
    }
}

struct Header {
    slice_height: usize,
    weights: [(u8, u8); BANDS],
    transfer: Transfer,
}

fn header(reader: &mut Reader<'_>, length: usize, width: usize, height: usize) -> Result<Header> {
    if reader.take(2)? != [0xff, 0x10] {
        return Err(corrupt("missing start marker"));
    }
    let cap = reader.marker([0xff, 0x50])?;
    if cap != b"CONTACT_INTOPIX_\xef\xc0\0\0\0\0\0\0\0\0\0\0\0\0\0\0" {
        return Err(unsupported("unrecognized capabilities"));
    }
    let pih: &[u8; 37] = reader.marker([0xff, 0x12])?.try_into().map_err(|_| unsupported("unrecognized picture header"))?;
    let declared = u32::from_be_bytes([pih[0], pih[1], pih[2], pih[3]]) as usize;
    if declared != length {
        return Err(corrupt("codestream length disagrees with the NEF strip"));
    }
    let w = usize::from(u16::from_be_bytes([pih[8], pih[9]]));
    let h = usize::from(u16::from_be_bytes([pih[10], pih[11]]));
    if (w, h) != (width, height) {
        return Err(corrupt("codestream dimensions disagree with the NEF IFD"));
    }
    let slice_height = usize::from(u16::from_be_bytes([pih[14], pih[15]]));
    if slice_height == 0 {
        return Err(corrupt("zero slice height"));
    }
    // Nc=4, Ng=4, Ss=8, Bw=18, Fq=4, Br=4, Ppoc=1, NLx=5, NLy=1,
    // uniform quantization, separate signs. Cw=0 means a single precinct column.
    if pih[4..8] != [0; 4] || pih[12..14] != [0; 2] || pih[16..24] != [4, 4, 8, 18, 0x44, 0x10, 0x51, 0x14] {
        return Err(unsupported("unrecognized coding profile"));
    }
    let tail = &pih[24..];
    // Only the independently verified transfer fields may vary. Other vendor fields encode
    // gains/transform choices whose meanings are not yet established here: fail explicitly.
    if tail[0..3] != [0x50, 0x88, 0x70] || tail[3] & 0xf0 != 0x80 || tail[5..7] != [0x15, 0x23] || tail[7] & 0xfc != 0xd0 || tail[12] != 7 {
        return Err(unsupported("unrecognized vendor transform parameters"));
    }
    let transfer = Transfer {
        black: (u16::from(tail[3] & 15) << 8) | u16::from(tail[4]),
        gain: (u32::from(tail[7] & 3) << 16) | (u32::from(tail[8]) << 8) | u32::from(tail[9]),
        pivot: u16::from_be_bytes([tail[10], tail[11]]),
    };
    if reader.marker([0xff, 0x13])? != [14, 0x11, 0x51, 14, 0x11, 0x51, 14, 0x11, 0, 14, 0x11, 0x51] {
        return Err(unsupported("unrecognized component decomposition"));
    }
    let wgt = reader.marker([0xff, 0x14])?;
    if wgt.len() != 2 * BANDS {
        return Err(corrupt("expected 25 band weights"));
    }
    let mut weights = [(0, 0); BANDS];
    let mut priorities = 0u32;
    for (dst, pair) in weights.iter_mut().zip(wgt.as_chunks::<2>().0) {
        let (gain, priority) = (pair[0], pair[1]);
        if gain > 15 || priority >= BANDS as u8 || priorities & (1 << priority) != 0 {
            return Err(corrupt("invalid band gain or priority"));
        }
        priorities |= 1 << priority;
        *dst = (gain, priority);
    }
    Ok(Header { slice_height, weights, transfer })
}

/// Codestream band index -> (component, horizontal/vertical filter index).
/// The first precinct line carries each component's horizontal bands; the second carries
/// vertical detail. Component 2 bypasses the wavelet and has a sample row on both lines.
fn band_map() -> Vec<(usize, usize)> {
    (0..4)
        .flat_map(|c| (0..if c == 2 { 1 } else { 6 }).map(move |b| (c, b)))
        .chain([0, 1, 3].into_iter().flat_map(|c| (6..8).map(move |b| (c, b))))
        .collect()
}

fn geometry(width: usize, weights: &[(u8, u8); BANDS]) -> (PrecinctGeometry, Vec<PacketLayout>) {
    let widths = [
        width.div_ceil(32),
        width.div_ceil(16) / 2,
        width.div_ceil(8) / 2,
        width.div_ceil(4) / 2,
        width.div_ceil(2) / 2,
        width / 2,
        width.div_ceil(2),
        width / 2,
    ];
    let map = band_map();
    let bands = map
        .iter()
        .zip(weights)
        .map(|(&(c, b), &(gain, priority))| BandGeometry {
            wpb: if c == 2 { width as u32 } else { widths[b] as u32 },
            gain,
            priority,
            l0: 0,
            l1: if c == 2 { 2 } else { 1 },
            exists: true,
        })
        .collect();
    let mut packets = Vec::with_capacity(8);
    for line in 0..2 {
        for c in 0..4 {
            let entries = map
                .iter()
                .enumerate()
                .filter(|(_, (cc, b))| *cc == c && (c == 2 || ((*b < 6) == (line == 0))))
                .map(|(band, _)| PacketEntry { band: band as u16, line: if c == 2 { line } else { 0 } })
                .collect();
            packets.push(PacketLayout { entries });
        }
    }
    // Nikon always writes the seven-byte packet header, even with PIH Lh=0.
    (PrecinctGeometry { bands, ng: 4, ss: 8, br: 4, fs: 1, rm: 0, rl: 0, lh: 1, short_packet_header: false }, packets)
}

/// Decode to the original 14-bit Bayer sample domain; also return its encoded black level.
pub(super) fn decode(bytes: &[u8], width: usize, height: usize, bits: u32) -> Result<(Vec<u16>, u16)> {
    let count = width.checked_mul(height).filter(|&n| n <= MAX_PIXELS).ok_or_else(|| corrupt("image exceeds the pixel limit"))?;
    if bits != 14 || width < 64 || !width.is_multiple_of(2) || height < 4 || !height.is_multiple_of(4) {
        return Err(unsupported("requires a 14-bit image with even width >= 64 and height divisible by four"));
    }
    let mut reader = Reader { rest: bytes };
    let header = header(&mut reader, bytes.len(), width, height)?;
    let (w, h) = (width / 2, height / 2);
    // Each precinct has a 12-byte header and eight seven-byte packet headers, before any
    // entropy payload. Reject implausibly short input before reserving image-sized storage.
    if reader.rest.len() < (h / 2) * (12 + 8 * 7) + (h / 2).div_ceil(header.slice_height) * 6 + 2 {
        return Err(corrupt("insufficient data for the declared dimensions"));
    }
    let (geom, packets) = geometry(w, &header.weights);
    let mut gathered: Vec<Vec<i32>> = geom.bands.iter().map(|b| Vec::with_capacity(b.wpb as usize * b.l1 as usize * (h / 2))).collect();
    let mut top = PrecinctTop::default();
    for precinct in 0..h / 2 {
        if precinct % header.slice_height == 0 {
            let slice = reader.marker([0xff, 0x20])?;
            if slice != ((precinct / header.slice_height) as u16).to_be_bytes() {
                return Err(corrupt("invalid slice index"));
            }
            // Vertical GCLI prediction is signalled even at Nikon slice starts. Its external
            // predecessor is zero; the second row of component 2 still predicts within the precinct.
            top = PrecinctTop::default();
        }
        let ph = entropy::parse_precinct_header(reader.rest, &geom).map_err(corrupt)?;
        reader.take(ph.header_bytes)?;
        let mut body = Reader { rest: reader.take(ph.lprc as usize)? };
        let mut state = PrecinctState::default();
        for layout in &packets {
            let packet = entropy::parse_packet_header(body.rest, &geom).map_err(corrupt)?;
            body.take(packet.header_bytes)?;
            let decoded = entropy::decode_packet_body(body.rest, &geom, &ph, &packet, layout, &mut state, Some(&top)).map_err(corrupt)?;
            body.take(decoded.bytes_consumed)?;
        }
        // Lprc includes encoder padding (not necessarily zero), which is not another packet.
        let dequantized = oxideav_jpegxs::dequant::dequantize_precinct(1, &geom, &entropy::precinct_truncation(&geom, &ph), &state.coefficients, 4);
        for (dst, src) in gathered.iter_mut().zip(dequantized) {
            dst.extend(src);
        }
        top = PrecinctTop::capture(&geom, &ph, &state);
    }
    if reader.take(2)? != [0xff, 0x11] || !reader.rest.is_empty() {
        return Err(corrupt("missing end marker or unexpected trailing data"));
    }
    let mut planes: [Vec<i32>; 4] = std::array::from_fn(|_| Vec::new());
    let map = band_map();
    for (c, plane) in planes.iter_mut().enumerate() {
        let mut bands = vec![Vec::new(); if c == 2 { 1 } else { 8 }];
        for ((cc, b), source) in map.iter().zip(&mut gathered) {
            if *cc == c {
                let dest = bands.get_mut(*b).ok_or_else(|| corrupt("invalid band index"))?;
                *dest = std::mem::take(source);
            }
        }
        *plane = if c == 2 { std::mem::take(&mut bands[0]) } else { inverse_wavelet(&bands, w, h)? };
        // Br=4 bounds dequantized coefficients to <2^19; six 5/3 passes fit i32. Keep the
        // following four-component colour transform's neighbour sums well inside i32 as well.
        if plane.len() != count / 4 || plane.iter().any(|&v| !(-(1 << 24)..=1 << 24).contains(&v)) {
            return Err(corrupt("reconstructed component is out of range"));
        }
        plane.reverse();
    }
    // Nikon's superpixel origin is opposite to the standard transform's origin. Reverse both
    // axes (the contiguous plane), order Ya/Cb/Cr/Delta, then undo the reversal on its outputs.
    planes.swap(2, 3);
    let mut slices: Vec<_> = planes.iter_mut().map(Vec::as_mut_slice).collect();
    oxideav_jpegxs::colour_transform::inverse_star_tetrix(&mut slices, w, h, 0, 0, 0, 0).map_err(corrupt)?;
    for plane in &mut planes {
        plane.reverse();
    }
    let mut out = vec![0; count];
    for (y, row) in out.chunks_exact_mut(width).enumerate() {
        for (x, value) in row.iter_mut().enumerate() {
            let c = 3 - (y % 2 * 2 + x % 2);
            let sample = planes[c].get((y / 2) * w + x / 2).ok_or_else(|| corrupt("incomplete reconstructed plane"))?;
            *value = header.transfer.apply(*sample);
        }
    }
    Ok((out, header.transfer.black))
}

fn band_row(bands: &[Vec<i32>], band: usize, y: usize, width: usize) -> Result<&[i32]> {
    bands.get(band).and_then(|b| b.get(y * width..(y + 1) * width)).ok_or_else(|| corrupt("incomplete wavelet band"))
}

/// Nikon synthesizes all horizontal levels before the vertical level. Swapping these passes
/// changes integer rounding, even though the real-valued separable transform would commute.
fn inverse_wavelet(bands: &[Vec<i32>], w: usize, h: usize) -> Result<Vec<i32>> {
    let mut out = vec![0; w * h];
    for (y, rows) in out.chunks_exact_mut(2 * w).enumerate() {
        let mut low = band_row(bands, 0, y, w.div_ceil(32))?.to_vec();
        for (band, level) in (1..=5).zip((1..=5).rev()) {
            let next_width = w.div_ceil(1 << (level - 1));
            let high = band_row(bands, band, y, next_width / 2)?;
            let mut next = vec![0; next_width];
            oxideav_jpegxs::dwt::inverse_horizontal_1d(&low, high, &mut next).map_err(corrupt)?;
            low = next;
        }
        let (even, odd) = rows.split_at_mut(w);
        even.copy_from_slice(&low);
        oxideav_jpegxs::dwt::inverse_horizontal_1d(band_row(bands, 6, y, w.div_ceil(2))?, band_row(bands, 7, y, w / 2)?, odd).map_err(corrupt)?;
    }
    // Inverse 5/3 vertical lifting, with symmetric extension of the updated even samples.
    for y in (0..h).step_by(2) {
        let above = if y == 0 { 1 } else { y - 1 };
        for x in 0..w {
            let a = *out.get(above * w + x).ok_or_else(|| corrupt("invalid vertical neighbour"))?;
            let b = *out.get((y + 1) * w + x).ok_or_else(|| corrupt("invalid vertical neighbour"))?;
            let value = out.get_mut(y * w + x).ok_or_else(|| corrupt("invalid vertical output"))?;
            *value -= (a + b + 2) >> 2;
        }
    }
    for y in (1..h).step_by(2) {
        let below = if y + 1 == h { y - 1 } else { y + 1 };
        for x in 0..w {
            let a = *out.get((y - 1) * w + x).ok_or_else(|| corrupt("invalid vertical neighbour"))?;
            let b = *out.get(below * w + x).ok_or_else(|| corrupt("invalid vertical neighbour"))?;
            let value = out.get_mut(y * w + x).ok_or_else(|| corrupt("invalid vertical output"))?;
            *value += (a + b) >> 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn bits(out: &mut Vec<bool>, value: u64, count: usize) {
        out.extend((0..count).rev().map(|n| value & (1 << n) != 0));
    }

    fn pack(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8).map(|chunk| chunk.iter().enumerate().fold(0, |v, (i, b)| v | (u8::from(*b) << (7 - i)))).collect()
    }

    fn marker(out: &mut Vec<u8>, code: u8, payload: &[u8]) {
        out.extend([0xff, code]);
        out.extend(((payload.len() + 2) as u16).to_be_bytes());
        out.extend(payload);
    }

    /// Original procedural fixture: constant but distinct Ya/Cr/Delta/Cb, raw GCLI, padded groups,
    /// and independently assembled packet headers. No camera bytes or external decoder code.
    pub(crate) fn fixture(width: usize, height: usize) -> Vec<u8> {
        let mut out = vec![0xff, 0x10];
        marker(&mut out, 0x50, b"CONTACT_INTOPIX_\xef\xc0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let mut pih = vec![0; 16];
        pih[8..10].copy_from_slice(&(width as u16).to_be_bytes());
        pih[10..12].copy_from_slice(&(height as u16).to_be_bytes());
        pih[15] = 1; // One precinct per slice, to exercise repeated slice headers.
        pih.extend([4, 4, 8, 18, 0x44, 0x10, 0x51, 0x14]);
        pih.extend([0x50, 0x88, 0x70, 0x83, 0xf0, 0x15, 0x23, 0xd1, 0x49, 0xcd, 0x3f, 0x7f, 7]);
        marker(&mut out, 0x12, &pih);
        marker(&mut out, 0x13, &[14, 0x11, 0x51, 14, 0x11, 0x51, 14, 0x11, 0, 14, 0x11, 0x51]);
        let weights: [(u8, u8); BANDS] = std::array::from_fn(|i| (0, i as u8));
        marker(&mut out, 0x14, &weights.iter().flat_map(|&(g, p)| [g, p]).collect::<Vec<_>>());
        let (geom, packets) = geometry(width / 2, &weights);
        let map = band_map();
        for slice in 0..height / 4 {
            marker(&mut out, 0x20, &(slice as u16).to_be_bytes());
            let mut body = Vec::new();
            for packet in &packets {
                let (mut gcli, mut data, mut signs) = (Vec::new(), Vec::new(), Vec::new());
                for entry in &packet.entries {
                    let bi = entry.band as usize;
                    let (component, band) = map[bi];
                    let value: i32 = if band == 0 { [-800, 25, -6, -19][component] } else { 0 };
                    let magnitude = value.unsigned_abs();
                    let planes = 32 - magnitude.leading_zeros();
                    let width = geom.bands[bi].wpb as usize;
                    for group in 0..width.div_ceil(4) {
                        bits(&mut gcli, planes as u64, 4);
                        for plane in (0..planes).rev() {
                            for x in 0..4 {
                                data.push(group * 4 + x < width && magnitude & (1 << plane) != 0);
                            }
                        }
                        for x in 0..4 {
                            if magnitude != 0 && group * 4 + x < width {
                                signs.push(value < 0);
                            }
                        }
                    }
                }
                let (gcli, data, signs) = (pack(&gcli), pack(&data), pack(&signs));
                let mut header = Vec::new();
                bits(&mut header, 1, 1); // Raw GCLI: significance and predictor are bypassed.
                bits(&mut header, data.len() as u64, 20);
                bits(&mut header, gcli.len() as u64, 20);
                bits(&mut header, signs.len() as u64, 15);
                body.extend(pack(&header));
                body.extend(gcli);
                body.extend(data);
                body.extend(signs);
            }
            body.extend([0xa5, 0x5a]); // Precinct padding need not be zero.
            let length = (body.len() as u32).to_be_bytes();
            out.extend(&length[1..]);
            out.extend([0, 0]); // Q=R=0: no coefficient truncation.
            out.extend([0xff; 6]); // Predictor modes must not affect raw-GCLI packets.
            out.push(0xc0);
            out.extend(body);
        }
        out.extend([0xff, 0x11]);
        let length = out.len() as u32;
        out[42..46].copy_from_slice(&length.to_be_bytes());
        out
    }

    #[test]
    fn raw_gcli_constant_components_reconstruct_distinct_bayer_sites() {
        // Half-width 35 includes partial groups and uneven wavelet splits; three slices.
        let (w, h) = (70, 12);
        let (samples, black) = decode(&fixture(w, h), w, h, 14).unwrap();
        assert_eq!(black, 1008);
        // Hand calculation: pre-transfer R/G1/G2/B = -12424/-12776/-12872/-13128.
        for y in 0..h {
            for x in 0..w {
                assert_eq!(samples[y * w + x], [1114, 1097, 1092, 1081][2 * (y % 2) + x % 2]);
            }
        }
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = fixture(70, 12);
        for end in 0..bytes.len() {
            let mut truncated = bytes[..end].to_vec();
            // Keep the declared size consistent to reach the inner packet bounds checks too.
            if end >= 46 {
                truncated[42..46].copy_from_slice(&(end as u32).to_be_bytes());
            }
            assert!(decode(&truncated, 70, 12, 14).is_err(), "accepted truncation at {end}");
        }
    }

    #[test]
    fn malformed_headers_dimensions_and_lengths_are_rejected() {
        let bytes = fixture(70, 12);
        for (at, value) in
            [(4, 0xff), (57, 0), (58, 0), (63, 0), (64, 0), (67, 0), (78, 0), (99, 255), (100, 255), (154, 1), (155, 0xff), (158, 32), (159, 25)]
        {
            let mut bad = bytes.clone();
            bad[at] = value;
            assert!(decode(&bad, 70, 12, 14).is_err(), "accepted mutation at {at}");
        }
        assert!(decode(&bytes, 72, 12, 14).is_err());
        assert!(decode(&bytes, 70, 12, 8).is_err());
        assert!(decode(&bytes, usize::MAX, 12, 14).is_err());
        assert!(decode(&bytes, 65534, 65532, 14).is_err());
    }

    #[test]
    fn transfer_rounding_negative_branch_and_clipping() {
        let normal = Transfer { black: 1008, gain: 84429, pivot: 16255 };
        assert_eq!(normal.apply(16255 - 32768), 1008);
        assert_eq!(normal.apply(16000 - 32768), 1008);
        assert_eq!(normal.apply(i32::MIN), 0);
        assert_eq!(normal.apply(i32::MAX), 16383);
        let ties = Transfer { gain: 65905, ..normal };
        assert_eq!(ties.apply(49023 - 32768), 5151); // Half-up, not ties-to-even (5150).
        assert_eq!(ties.apply(i32::MAX), ties.apply(65535 - 32768));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(128))]
        #[test]
        fn mutated_codestreams_do_not_panic(changes in proptest::collection::vec((0usize..2048, proptest::num::u8::ANY), 1..16)) {
            let mut bytes = fixture(70, 12);
            for (offset, value) in changes { let at = offset % bytes.len(); bytes[at] = value; }
            if let Ok((samples, black)) = decode(&bytes, 70, 12, 14) {
                proptest::prop_assert_eq!(samples.len(), 70 * 12);
                proptest::prop_assert!(samples.iter().all(|&v| v <= 16383));
                proptest::prop_assert!(black <= 4095);
            }
        }
    }
}
