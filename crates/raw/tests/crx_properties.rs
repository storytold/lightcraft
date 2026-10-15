//! Property-based invariants of the Canon CRX decoder, exercised through the public decode path.
//!
//! `vendor/crx` and the 5/3 synthesis in `vendor/crx_wavelet` are `pub(crate)`, so every property here
//! wraps a synthetic CRX stream in a minimal CR3 container (ftyp + moov + a CRAW track with CMP1/CDI1)
//! and calls [`lightcraft_raw::decode`]. The CRX entropy streams are produced by reference encoders in
//! this file that mirror the documented coders (MED prediction, signed mapping, adaptive Rice, the
//! MELCODE zero run, the QP map predictor — sources cited in `crates/raw/src/vendor/crx.rs`).
//!
//! Wavelet roundtrip: the analysis side is this file's exact algebraic inverse of one inverse-lifting
//! line, so the property tests the crate's own synthesis. It is exact by construction because the
//! tile-edge halo high-pass coefficients are free parameters of the encoding — synthesis absorbs any
//! stored value through the neighbouring low-pass coefficient — so random planes of every reachable
//! parity (odd plane sizes via even tile sizes ≡ 2 mod 4) and multi-tile halo configuration must decode
//! back to exactly the encoded sensor samples.
//!
//! Properties:
//! 1. C-RAW wavelet roundtrip is exact across sizes, parities and tile halos.
//! 2. Quantization: every supported uniform quant value (4–44) and adaptive QP value (131–168)
//!    dequantizes within the CMP1 bit depth — C-RAW clamps the overshoot, lossless rejects it — and
//!    the values just outside those bands (quant 3 and 45, QP 130 and 169) are rejected.
//! 3. Rice/Golomb bounds: adversarial entropy prefixes, in the plane bands and in the QP map, error
//!    out, never panic, and stay deterministic.
//! 4. IAD1 geometry: `active_area ⊆ sensor dims` and `crop ⊆ active_area` hold or the file is rejected.
//! 5. Decoding is deterministic across repeated and parallel invocations (rayon plane decode).
//! 6. Odd QP-map heights decode: the last QP row has no partner row and is used alone.
//!
//! The case budget is proptest's default (256 per property); the `PROPTEST_CASES` environment variable
//! overrides it.

use lightcraft_raw::{RawData, RawError, RawFormat, RawImage, decode, probe, probe_info};
use proptest::prelude::*;

// --- reference entropy coders (mirrors of the decoder's documented bitstream grammar) ---

/// MELCODE run-index table (crx.rs `RUN_EXPONENT`).
const RUN_EXPONENT: [u8; 32] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// MSB-first bit writer; `into_bytes` zero-pads the final byte (the decoder's `finish` allows ≤ 7 zero tail bits).
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    filled: usize,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) {
        for shift in (0..count).rev() {
            if self.filled.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value.checked_shr(shift).unwrap_or(0) & 1 == 1 {
                let byte = self.bytes.last_mut().unwrap();
                *byte |= 0x80 >> (self.filled % 8);
            }
            self.filled += 1;
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Adaptive Rice code: ≤ 40-zero prefix, otherwise the 41-zero escape with a 21-bit literal (crx.rs `Bits::rice`).
fn put_rice(bits: &mut BitWriter, k: u32, symbol: u32) {
    let zeros = symbol >> k;
    if zeros <= 40 {
        bits.put(0, zeros);
        bits.put(1, 1);
        bits.put(symbol & ((1 << k) - 1), k);
    } else {
        assert!(symbol < 1 << 21, "reference encoder literals are bounded by the plane precision");
        bits.put(0, 41);
        bits.put(1, 1);
        bits.put(symbol & 0x001f_ffff, 21);
    }
}

/// QP Rice code (crx.rs `qp_rice`), a limited-length code: a quotient below 23 is a plain Rice code, anything
/// larger is 23 zeros, the terminating 1 and the whole symbol in 8 bits.
fn put_qp_rice(bits: &mut BitWriter, k: u32, symbol: u32) {
    let zeros = symbol >> k;
    if zeros < 23 {
        bits.put(0, zeros);
        bits.put(1, 1);
        bits.put(symbol & ((1 << k) - 1), k);
    } else {
        assert!(symbol < 1 << 8, "reference QP escapes carry an 8-bit symbol");
        bits.put(0, 23);
        bits.put(1, 1);
        bits.put(symbol, 8);
    }
}

/// Emits the MELCODE bits whose decode (`crx.rs run`) yields exactly `target` with the same run-index bookkeeping.
fn put_run(bits: &mut BitWriter, index: &mut usize, remaining: usize, target: usize) {
    assert!(target >= 1 && target <= remaining);
    let mut length = 1usize;
    while length < remaining {
        let exponent = u32::from(RUN_EXPONENT[*index]);
        if target - length >= 1usize << exponent {
            bits.put(1, 1);
            length += 1usize << exponent;
            if length <= remaining {
                *index = (*index + 1).min(RUN_EXPONENT.len() - 1);
            }
        } else {
            bits.put(0, 1);
            bits.put((target - length) as u32, exponent);
            *index = index.saturating_sub(1);
            return;
        }
    }
    assert_eq!(length, target, "a run reaching the row end needs no terminator");
}

/// Inverse of the decoder's signed mapping (crx.rs `signed`).
fn zig(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

/// MED prediction (crx.rs `median`).
fn median(left: i32, above: i32, corner: i32) -> i32 {
    if corner >= left.max(above) {
        left.min(above)
    } else if corner <= left.min(above) {
        left.max(above)
    } else {
        left + above - corner
    }
}

/// Adaptive Rice parameter update (crx.rs `update_rice`).
fn update_rice(mut k: u32, symbol: u32) -> u32 {
    if symbol >= 3 << k {
        k += 1;
        if symbol >= 3 << k {
            k += 1;
        }
    } else if k > 0 && symbol < 1 << (k - 1) {
        k -= 1;
    }
    k.min(20)
}

/// Encodes a plane with the lossless coder: a zero run flag wherever the decoder reads one, then MED + Rice.
fn encode_lossless_plane(values: &[i32], width: usize, height: usize, bits: &mut BitWriter) {
    let mut k = 0u32; // runs are never emitted, so the MELCODE run index stays unused here
    let (mut previous, mut current) = (vec![0i32; width], vec![0i32; width]);
    for y in 0..height {
        let mut left = previous[0];
        for x in 0..width {
            let above = previous[x];
            let right = previous.get(x + 1).copied().unwrap_or(above);
            if x + 1 < width && left == above && left == right {
                bits.put(0, 1); // a run is never required: regular prediction encodes any sample
            }
            let corner = if x == 0 { above } else { previous[x - 1] };
            let symbol = zig(values[y * width + x] - median(left, above, corner));
            put_rice(bits, k, symbol);
            current[x] = values[y * width + x];
            left = values[y * width + x];
            let estimate = if y > 0 && x + 1 < width { (symbol + 2 * right.abs_diff(above)) / 2 } else { symbol };
            k = update_rice(k, estimate);
        }
        std::mem::swap(&mut current, &mut previous);
    }
}

/// Encodes a high-frequency band: zero-run flags in the all-zero context (forced for zero coefficients),
/// the nonzero-proven mapping after an interruption, and the upper-row Rice context (crx.rs `high_frequency_band`).
fn encode_high_frequency_band(values: &[i32], width: usize, height: usize, bits: &mut BitWriter) {
    let (mut k, mut run_index) = (0u32, 0usize);
    let (mut previous, mut current) = (vec![0i32; width], vec![0i32; width]);
    let (mut previous_k, mut current_k) = (vec![0i32; width], vec![0i32; width]);
    for y in 0..height {
        let (mut x, mut left) = (0usize, 0i32);
        while x < width {
            let above = previous[x];
            let right = previous.get(x + 1).copied().unwrap_or(above);
            let mut nonzero = false;
            if x + 1 < width && left == 0 && above == 0 && right == 0 {
                if values[y * width + x] == 0 {
                    bits.put(1, 1); // a zero in this context must take the run path
                    let mut length = 1usize;
                    while x + length < width && values[y * width + x + length] == 0 {
                        length += 1;
                    }
                    put_run(bits, &mut run_index, width - x, length);
                    for slot in &mut current[x..x + length] {
                        *slot = 0;
                    }
                    for slot in &mut current_k[x..x + length] {
                        *slot = 0;
                    }
                    left = 0;
                    x += length;
                    if x == width {
                        continue;
                    }
                    nonzero = true; // the interruption proves the next coefficient nonzero
                } else {
                    bits.put(0, 1);
                    nonzero = true;
                }
            }
            let coded = zig(values[y * width + x]) - u32::from(nonzero);
            put_rice(bits, k, coded);
            current[x] = values[y * width + x];
            left = values[y * width + x];
            k = update_rice(k, coded);
            if y > 0 && x + 1 < width && previous_k[x + 1] as u32 > k + 1 {
                k += 1;
            }
            current_k[x] = k as i32;
            x += 1;
        }
        std::mem::swap(&mut current, &mut previous);
        std::mem::swap(&mut current_k, &mut previous_k);
    }
}

/// Encodes an adaptive QP map: the first symbol as a 32-bit literal, then QP Rice codes over the MED
/// predictor (crx.rs `qp_map`).
fn encode_qp_map(values: &[i32], width: usize, height: usize) -> Vec<u8> {
    let mut k = 0u32;
    let (mut previous, mut current) = (vec![0i32; width], vec![0i32; width]);
    let mut bits = BitWriter::default();
    for y in 0..height {
        let mut left = previous[0];
        for x in 0..width {
            let above = previous[x];
            let corner = if x == 0 { above } else { previous[x - 1] };
            let right = previous.get(x + 1).copied().unwrap_or(above);
            let symbol = zig(values[y * width + x] - median(left, above, corner));
            if x == 0 && y == 0 {
                bits.put(symbol, 32);
            } else {
                put_qp_rice(&mut bits, k, symbol);
            }
            current[x] = values[y * width + x];
            left = values[y * width + x];
            let estimate = if y > 0 && x + 1 < width { (symbol + 2 * right.abs_diff(above)) / 2 } else { symbol };
            k = update_rice(k, estimate);
        }
        std::mem::swap(&mut current, &mut previous);
    }
    bits.into_bytes()
}

// --- exact algebraic inverse of the crate's 5/3 synthesis (the forward transform under test) ---

struct Band {
    width: usize,
    height: usize,
    data: Vec<i32>,
}

/// Produces the (low, high) pair whose synthesis (`crx_wavelet::inverse_line`) reproduces `line` exactly.
///
/// Mirrors the inverse lifting: each odd sample fixes its stored high coefficient, each even sample its
/// stored low coefficient. The `before`/`after` halo coefficients are free parameters (any value works:
/// synthesis absorbs them through the boundary low coefficient), so they are supplied by the caller.
fn forward_lift(line: &[i32], before: bool, after: bool, halo_before: i32, halo_after: i32) -> (Vec<i32>, Vec<i32>) {
    let n = line.len();
    let high_count = n / 2 + usize::from(before) + usize::from(after);
    let low_count = n.div_ceil(2) + usize::from(after && n.is_multiple_of(2));
    if high_count == 0 {
        return (line.to_vec(), Vec::new());
    }
    // The right prediction of the final odd sample of an even, right-halo line (`after`: even(i + 1)).
    let extension = if after && n.is_multiple_of(2) { Some(2 * (line[n - 1] - halo_after) - line[n - 2]) } else { None };
    let mut high = Vec::with_capacity(high_count);
    if before {
        high.push(halo_before);
    }
    for i in 0..n / 2 {
        let right = match line.get(2 * i + 2) {
            Some(&value) => value,
            None => extension.unwrap_or(line[2 * i]), // no right neighbour: synthesis mirrors the last even sample
        };
        high.push(line[2 * i + 1] - (line[2 * i] + right).div_euclid(2));
    }
    if after {
        high.push(halo_after);
    }
    let mut low = Vec::with_capacity(low_count);
    let evens = n.div_ceil(2);
    for i in 0..low_count {
        let even_value = if i < evens { line[2 * i] } else { extension.unwrap_or_default() };
        let left_index = (i + usize::from(before)).saturating_sub(1).min(high.len() - 1);
        let right_index = (i + usize::from(before)).min(high.len() - 1);
        low.push(even_value + (high[left_index] + high[right_index] + 2).div_euclid(4));
    }
    (low, high)
}

/// One inverse level, reversed: vertical lifting first (CRX analysis order), then horizontal, with the
/// same tile-edge flags the decoder passes for every level.
fn forward_level(plane: &[i32], width: usize, height: usize, before: bool, after: bool, halo: i32) -> (Band, Band, Band, Band) {
    let (low_h, high_h) = (height.div_ceil(2), height / 2);
    let mut temporary = vec![0i32; width * height];
    for x in 0..width {
        let column: Vec<i32> = (0..height).map(|y| plane[y * width + x]).collect();
        let (lo, hi) = forward_lift(&column, false, false, halo, halo);
        for (y, value) in lo.into_iter().chain(hi).enumerate() {
            temporary[y * width + x] = value;
        }
    }
    let low_w = width.div_ceil(2) + usize::from(after && width.is_multiple_of(2));
    let high_w = width / 2 + usize::from(before) + usize::from(after);
    let band = |w: usize, h: usize| Band { width: w, height: h, data: vec![0i32; w * h] };
    let (mut ll, mut hl, mut lh, mut hh) = (band(low_w, low_h), band(high_w, low_h), band(low_w, high_h), band(high_w, high_h));
    for y in 0..height {
        let (lo, hi) = forward_lift(&temporary[y * width..(y + 1) * width], before, after, halo, halo);
        let (low_band, high_band, by) = if y < low_h { (&mut ll, &mut hl, y) } else { (&mut lh, &mut hh, y - low_h) };
        low_band.data[by * low_w..(by + 1) * low_w].copy_from_slice(&lo);
        high_band.data[by * high_w..(by + 1) * high_w].copy_from_slice(&hi);
    }
    (ll, hl, lh, hh)
}

/// Subbands in CRX order: LL3, HL3, LH3, HH3, HL2, LH2, HH2, HL1, LH1, HH1.
fn forward_bands(plane: &[i32], width: usize, height: usize, before: bool, after: bool, halo: i32) -> Vec<Band> {
    let (ll1, hl1, lh1, hh1) = forward_level(plane, width, height, before, after, halo);
    let (ll2, hl2, lh2, hh2) = forward_level(&ll1.data, ll1.width, ll1.height, before, after, halo);
    let (ll3, hl3, lh3, hh3) = forward_level(&ll2.data, ll2.width, ll2.height, before, after, halo);
    vec![ll3, hl3, lh3, hh3, hl2, lh2, hh2, hl1, lh1, hh1]
}

/// Subband shapes the decoder derives (crx.rs `wavelet_plane` / `crx_wavelet::sizes`).
fn band_shapes(plane_width: usize, plane_height: usize, before: bool, after: bool) -> [(usize, usize); 10] {
    let mut levels = [(0usize, 0usize, 0usize, 0usize); 3]; // (low_w, high_w, low_h, high_h), finest first
    let (mut w, mut h) = (plane_width, plane_height);
    for level in &mut levels {
        let low_w = w.div_ceil(2) + usize::from(after && w % 2 == 0);
        let high_w = w / 2 + usize::from(before) + usize::from(after);
        *level = (low_w, high_w, h.div_ceil(2), h / 2);
        w = low_w;
        h = level.2;
    }
    let (lw1, hw1, lh1, hh1) = levels[0];
    let (lw2, hw2, lh2, hh2) = levels[1];
    let (lw3, hw3, lh3, hh3) = levels[2];
    [(lw3, lh3), (hw3, lh3), (lw3, hh3), (hw3, hh3), (hw2, lh2), (lw2, hh2), (hw2, hh2), (hw1, lh1), (lw1, hh1), (hw1, hh1)]
}

fn encode_band(band: &Band, lowpass: bool) -> Vec<u8> {
    let mut bits = BitWriter::default();
    if lowpass {
        encode_lossless_plane(&band.data, band.width, band.height, &mut bits);
    } else {
        encode_high_frequency_band(&band.data, band.width, band.height, &mut bits);
    }
    bits.into_bytes()
}

/// The measured uniform quantization sequence (crx.rs `quant_step`).
fn quant_step(value: u8) -> i32 {
    let scale = [40i32, 45, 51, 57, 64, 72][usize::from(value % 6)];
    (scale << (value / 6)) / 64
}

// --- synthetic CR3 container and CRX sample assembly ---

fn box_bytes(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn track_box(entry: &[u8], offset: u64, size: u32) -> Vec<u8> {
    let mut stsd = vec![0u8; 4];
    stsd.extend(1u32.to_be_bytes());
    stsd.extend_from_slice(entry);
    let mut stsz = vec![0u8; 4];
    stsz.extend(size.to_be_bytes());
    stsz.extend(1u32.to_be_bytes());
    let mut co64 = vec![0u8; 4];
    co64.extend(1u32.to_be_bytes());
    co64.extend(offset.to_be_bytes());
    let tables = [box_bytes(b"stsd", &stsd), box_bytes(b"stsz", &stsz), box_bytes(b"co64", &co64)].concat();
    box_bytes(b"trak", &box_bytes(b"mdia", &box_bytes(b"minf", &box_bytes(b"stbl", &tables))))
}

/// Wraps a CRAW sample entry and its sample bytes in a minimal CR3 file (no metadata boxes: geometry
/// and levels come from the container and CMP1 alone).
fn cr3_file(entry: &[u8], sample: &[u8]) -> Vec<u8> {
    let ftyp = box_bytes(b"ftyp", b"crx \0\0\0\x01crx isom");
    // The track length does not depend on the offset value (always a 64-bit field), so it can be
    // measured with a placeholder before the real offset is known.
    let track_len = track_box(entry, 0, sample.len() as u32).len();
    let offset = (ftyp.len() + 8 + track_len + 7) & !7;
    let mut file = ftyp;
    file.extend(box_bytes(b"moov", &track_box(entry, offset as u64, sample.len() as u32)));
    file.resize(offset, 0);
    file.extend_from_slice(sample);
    file
}

fn cmp1(version: u16, width: u32, height: u32, tile_width: u32, tile_height: u32, levels: u8, header_size: u32) -> Vec<u8> {
    let mut coding = vec![0u8; 52];
    coding[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
    coding[2..4].copy_from_slice(&48u16.to_be_bytes());
    coding[4..6].copy_from_slice(&version.to_be_bytes());
    coding[8..12].copy_from_slice(&width.to_be_bytes());
    coding[12..16].copy_from_slice(&height.to_be_bytes());
    coding[16..20].copy_from_slice(&tile_width.to_be_bytes());
    coding[20..24].copy_from_slice(&tile_height.to_be_bytes());
    coding[24] = 14;
    coding[25] = 0x40; // four planes, CFA pattern 0 (RGGB)
    coding[26] = levels; // encoding 0
    coding[28..32].copy_from_slice(&header_size.to_be_bytes());
    coding
}

fn iad1_bytes(width: u16, height: u16, full: u16, crop: [u16; 4], masked_left: [u16; 4], masked_top: [u16; 4], active: [u16; 4]) -> Vec<u8> {
    let mut area = vec![0u8; 48];
    let mut put = |at: usize, value: u16| area[at..at + 2].copy_from_slice(&value.to_be_bytes());
    put(4, width);
    put(6, height);
    put(10, full);
    for (i, value) in crop.iter().enumerate() {
        put(16 + 2 * i, *value);
    }
    for (i, value) in masked_left.iter().enumerate() {
        put(24 + 2 * i, *value);
    }
    if full == 2 {
        for (i, value) in masked_top.iter().enumerate() {
            put(32 + 2 * i, *value);
        }
        for (i, value) in active.iter().enumerate() {
            put(40 + 2 * i, *value);
        }
    }
    area
}

fn craw_entry(coding: &[u8], width: u16, height: u16, iad1: Option<&[u8]>) -> Vec<u8> {
    let mut craw = vec![0u8; 82];
    craw[24..26].copy_from_slice(&width.to_be_bytes());
    craw[26..28].copy_from_slice(&height.to_be_bytes());
    craw.extend(box_bytes(b"CMP1", coding));
    if let Some(area) = iad1 {
        craw.extend(box_bytes(b"CDI1", &[vec![0u8; 4], box_bytes(b"IAD1", area)].concat()));
    }
    box_bytes(b"CRAW", &craw)
}

fn put_marker(out: &mut Vec<u8>, code: u16, a: u32, b: u32) {
    out.extend(code.to_be_bytes());
    out.extend(8u16.to_be_bytes());
    out.extend(a.to_be_bytes());
    out.extend(b.to_be_bytes());
}

fn put_marker16(out: &mut Vec<u8>, code: u16, words: [u32; 4]) {
    out.extend(code.to_be_bytes());
    out.extend(16u16.to_be_bytes());
    for word in words {
        out.extend(word.to_be_bytes());
    }
}

/// Assembles a version 0x100 sample: one ff01 tile header and per plane an ff02 header plus `bands`
/// ff03 headers, then the band bytes. `bands` is 1 (lossless) or 10 (three-level C-RAW).
fn v100_sample(tiles: &[[Vec<Vec<u8>>; 4]], quant: u8) -> (usize, Vec<u8>) {
    let mut header = Vec::new();
    let mut data = Vec::new();
    for (index, planes) in tiles.iter().enumerate() {
        let tile_size: usize = planes.iter().flatten().map(Vec::len).sum();
        put_marker(&mut header, 0xff01, tile_size as u32, u32::from(index as u16) << 16);
        for (p, bands) in planes.iter().enumerate() {
            let plane_size: usize = bands.iter().map(Vec::len).sum();
            put_marker(&mut header, 0xff02, plane_size as u32, (p as u32) << 28 | 0x0800_0000);
            for (b, bytes) in bands.iter().enumerate() {
                put_marker(&mut header, 0xff03, bytes.len() as u32, (b as u32) << 28 | u32::from(quant) << 19);
            }
        }
        for bands in planes {
            for bytes in bands {
                data.extend_from_slice(bytes);
            }
        }
    }
    let header_size = header.len();
    header.extend(data);
    (header_size, header)
}

/// Assembles a version 0x200 single-tile sample: an ff11 header with the adaptive QP stream, ff12/ff13
/// headers with per-band base and gain, then QP data, its padding and the band bytes.
fn v200_sample(tile: &[Vec<Vec<u8>>; 4], gains: [u32; 10], bases: [u32; 10], qp: &[u8], qp_padding: usize) -> (usize, Vec<u8>) {
    let band_bytes: usize = tile.iter().map(|bands| bands.iter().map(Vec::len).sum::<usize>()).sum();
    let tile_size = qp.len() + qp_padding + band_bytes;
    let mut header = Vec::new();
    put_marker16(&mut header, 0xff11, [tile_size as u32, 0x4000, qp.len() as u32, u32::from(qp_padding as u16) << 16]);
    for (p, bands) in tile.iter().enumerate() {
        let plane_size: usize = bands.iter().map(Vec::len).sum();
        put_marker(&mut header, 0xff12, plane_size as u32, (p as u32) << 28 | 0x0800_0000);
        for (b, bytes) in bands.iter().enumerate() {
            put_marker16(&mut header, 0xff13, [bytes.len() as u32, (b as u32) << 28 | gains[b], bases[b], 0]);
        }
    }
    let header_size = header.len();
    let mut data = header;
    data.extend_from_slice(qp);
    data.resize(data.len() + qp_padding, 0);
    for bands in tile {
        for bytes in bands {
            data.extend_from_slice(bytes);
        }
    }
    (header_size, data)
}

// --- fixture builders ---

/// A synthetic CRX stream plus its container, with the expected sensor samples when exact decoding is asserted.
struct Fixture {
    file: Vec<u8>,
    expected: Vec<u16>,
}

fn wrap(coding: &[u8], width: u32, height: u32, sample: &[u8], iad1: Option<&[u8]>) -> Vec<u8> {
    cr3_file(&craw_entry(coding, width as u16, height as u16, iad1), sample)
}

/// Deterministic pseudo-random plane content for fixed regression-style files.
fn lcg_values(seed: u64, count: usize) -> Vec<i32> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) % 16_384) as i32 - 8192
        })
        .collect()
}

/// Tile geometries with three non-empty wavelet levels: (tile width, tile height, tile count).
/// Plane sizes are half of each; odd plane sizes come from tile sizes ≡ 2 (mod 4).
const GEOMETRIES: [(usize, usize, usize); 8] =
    [(16, 16, 1), (18, 16, 1), (16, 18, 1), (18, 18, 2), (24, 16, 2), (32, 18, 2), (24, 24, 1), (48, 16, 3)];
const MAX_PLANE_VALUES: usize = 3 * 4 * 24 * 12;

const LOSSLESS_GEOMETRIES: [(usize, usize); 5] = [(4, 4), (8, 4), (6, 8), (12, 6), (8, 8)];

/// Version 0x200 sensor sizes; the QP map has one cell per 16×4 pixels, so heights 12 and 20 give odd map heights (3 and 5).
const V200_GEOMETRIES: [(usize, usize); 6] = [(16, 16), (32, 16), (16, 24), (16, 32), (16, 12), (32, 20)];

/// Encodes the four planes of one tile into CRX band byte streams.
fn encode_tile_planes(
    planes_values: [Vec<i32>; 4],
    plane_w: usize,
    plane_h: usize,
    before: bool,
    after: bool,
    halo: i32,
    levels: u8,
) -> [Vec<Vec<u8>>; 4] {
    let mut encoded: [Vec<Vec<u8>>; 4] = Default::default();
    for (p, plane_bytes) in encoded.iter_mut().enumerate() {
        let values = &planes_values[p];
        if levels == 0 {
            let band = Band { width: plane_w, height: plane_h, data: values.clone() };
            *plane_bytes = vec![encode_band(&band, true)];
        } else {
            let bands = forward_bands(values, plane_w, plane_h, before, after, halo);
            *plane_bytes = bands.iter().enumerate().map(|(i, band)| encode_band(band, i == 0)).collect();
        }
    }
    encoded
}

/// Builds a C-RAW file whose planes hold the given random values exactly (uniform quant 4/8/9 has step 1).
fn craw_roundtrip_file((tile_w, tile_h, tile_count): (usize, usize, usize), quant: u8, halo: i32, values: &[i32]) -> Fixture {
    let (plane_w, plane_h) = (tile_w / 2, tile_h / 2);
    let (width, height) = (tile_w * tile_count, tile_h);
    let mut cursor = 0usize;
    let mut tiles: Vec<[Vec<Vec<u8>>; 4]> = Vec::new();
    let mut expected = vec![0u16; width * height];
    for tile in 0..tile_count {
        let mut planes_values = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for p in 0..4 {
            planes_values[p] = values[cursor..cursor + plane_w * plane_h].to_vec();
            cursor += plane_w * plane_h;
            for (i, &value) in planes_values[p].iter().enumerate() {
                let x = tile * tile_w + 2 * (i % plane_w) + p % 2;
                let y = 2 * (i / plane_w) + p / 2;
                expected[y * width + x] = (value + 8192) as u16; // in sensor range: no clamping
            }
        }
        tiles.push(encode_tile_planes(planes_values, plane_w, plane_h, tile > 0, tile + 1 < tile_count, halo, 3));
    }
    let (header_size, sample) = v100_sample(&tiles, quant);
    let coding = cmp1(0x100, width as u32, height as u32, tile_w as u32, tile_h as u32, 3, header_size as u32);
    Fixture { file: wrap(&coding, width as u32, height as u32, &sample, None), expected }
}

/// Builds a lossless file (one band per plane) whose planes hold exactly the given values.
fn lossless_file((width, height): (usize, usize), values: &[i32]) -> Fixture {
    let (plane_w, plane_h) = (width / 2, height / 2);
    let mut planes_values = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut expected = vec![0u16; width * height];
    for p in 0..4 {
        planes_values[p] = values[p * plane_w * plane_h..(p + 1) * plane_w * plane_h].to_vec();
        for (i, &value) in planes_values[p].iter().enumerate() {
            let x = 2 * (i % plane_w) + p % 2;
            let y = 2 * (i / plane_w) + p / 2;
            expected[y * width + x] = (value + 8192) as u16;
        }
    }
    let tiles = [encode_tile_planes(planes_values, plane_w, plane_h, false, false, 0, 0)];
    let (header_size, sample) = v100_sample(&tiles, 4);
    let coding = cmp1(0x100, width as u32, height as u32, width as u32, height as u32, 0, header_size as u32);
    Fixture { file: wrap(&coding, width as u32, height as u32, &sample, None), expected }
}

/// Constant-coefficient planes for one tile: LL3 flat at `ll3`, band `mid_band.0` flat at `mid_band.1`,
/// everything else zero. Encoded at the given level count with the given quant/gain/base parameters.
fn constant_band_planes(
    plane_w: usize,
    plane_h: usize,
    levels: u8,
    ll3: i32,
    mid_band: Option<(usize, i32)>,
    quant: u8,
    gains: [u32; 10],
    bases: [u32; 10],
    qp: Option<&[i32]>,
    qp_padding: usize,
    before: bool,
    after: bool,
) -> (usize, Vec<u8>) {
    let planes: [Vec<Vec<u8>>; 4] = if levels == 0 {
        let mut encoded: [Vec<Vec<u8>>; 4] = Default::default();
        for bands in encoded.iter_mut() {
            let band = Band { width: plane_w, height: plane_h, data: vec![ll3; plane_w * plane_h] };
            *bands = vec![encode_band(&band, true)];
        }
        encoded
    } else {
        let shapes = band_shapes(plane_w, plane_h, before, after);
        let mut encoded: [Vec<Vec<u8>>; 4] = Default::default();
        for bands in encoded.iter_mut() {
            let bands_data: Vec<Band> = shapes
                .iter()
                .enumerate()
                .map(|(i, &(w, h))| {
                    let value = if i == 0 { ll3 } else { mid_band.filter(|&(band, _)| band == i).map_or(0, |(_, value)| value) };
                    Band { width: w, height: h, data: vec![value; w * h] }
                })
                .collect();
            *bands = bands_data.iter().enumerate().map(|(i, band)| encode_band(band, i == 0)).collect();
        }
        encoded
    };
    if levels == 0 {
        v100_sample(&[planes], quant)
    } else if let Some(map) = qp {
        let (map_w, map_h) = (plane_w / 8, plane_h / 2); // one QP cell per 16×4 sensor pixels
        v200_sample(&planes, gains, bases, &encode_qp_map(map, map_w, map_h), qp_padding)
    } else {
        v100_sample(&[planes], quant)
    }
}

/// A 16×16 three-level C-RAW file built from constant bands at the given uniform quant value.
fn constant_bands_file(ll3: i32, mid_band: Option<(usize, i32)>, quant: u8) -> Fixture {
    let (header_size, sample) = constant_band_planes(8, 8, 3, ll3, mid_band, quant, [0; 10], [0; 10], None, 0, false, false);
    let coding = cmp1(0x100, 16, 16, 16, 16, 3, header_size as u32);
    Fixture { file: wrap(&coding, 16, 16, &sample, None), expected: Vec::new() }
}

/// A version 0x200 file with the given adaptive QP map (one cell per 16×4 sensor pixels) and per-band gains/bases.
fn v200_mapped_file(
    width: usize,
    height: usize,
    map: &[i32],
    ll3: i32,
    mid_band: Option<(usize, i32)>,
    gains: [u32; 10],
    bases: [u32; 10],
) -> Fixture {
    assert_eq!(map.len(), (width / 16) * (height / 4));
    let (header_size, sample) = constant_band_planes(width / 2, height / 2, 3, ll3, mid_band, 4, gains, bases, Some(map), 3, false, false);
    let coding = cmp1(0x200, width as u32, height as u32, width as u32, height as u32, 3, header_size as u32);
    Fixture { file: wrap(&coding, width as u32, height as u32, &sample, None), expected: Vec::new() }
}

/// A version 0x200 file with a uniform adaptive QP map and the given per-band gains/bases.
fn v200_constant_bands_file(
    width: usize,
    height: usize,
    qp_value: i32,
    ll3: i32,
    mid_band: Option<(usize, i32)>,
    gains: [u32; 10],
    bases: [u32; 10],
) -> Fixture {
    v200_mapped_file(width, height, &vec![qp_value; (width / 16) * (height / 4)], ll3, mid_band, gains, bases)
}

fn sensor_hash(image: &RawImage) -> u64 {
    match &image.data {
        RawData::U16(samples) => samples
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)),
        RawData::F32(_) => panic!("u16 sensor data expected"),
    }
}

fn quad() -> impl proptest::strategy::Strategy<Value = [u16; 4]> {
    (any::<u16>(), any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(a, b, c, d)| [a, b, c, d])
}

proptest! {
    /// Property 1: synthesis after the exact-inverse analysis reproduces random planes exactly, across
    /// odd plane parities and multi-tile halo configurations, with arbitrary halo values.
    #[test]
    fn craw_wavelet_roundtrip_is_exact(
        geometry in 0usize..GEOMETRIES.len(),
        values in proptest::collection::vec(-8192i32..=8191, MAX_PLANE_VALUES),
        halo in -4096i32..=4096,
        quant in proptest::prop_oneof![Just(4u8), Just(8u8), Just(9u8)],
    ) {
        let (tile_w, tile_h, tile_count) = GEOMETRIES[geometry];
        let fixture = craw_roundtrip_file(GEOMETRIES[geometry], quant, halo, &values);
        assert_eq!(probe(&fixture.file), Some(RawFormat::Cr3));
        let image = decode(&fixture.file).unwrap();
        assert_eq!((image.width, image.height, image.bits), (tile_w * tile_count, tile_h, 14));
        let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
        assert_eq!(samples, &fixture.expected, "synthesis after analysis must be exact");
        assert_eq!(probe_info(&fixture.file).unwrap(), image.info());
    }

    /// Property 2 (lossless side): in-range planes decode exactly; a sample outside the CMP1 bit depth
    /// is corrupt data, never clamped.
    #[test]
    fn lossless_planes_are_exact_and_out_of_range_values_are_rejected(
        geometry in 0usize..LOSSLESS_GEOMETRIES.len(),
        values in proptest::collection::vec(-8192i32..=8191, 4 * 6 * 4),
        out_of_range in 0u8..=2,
    ) {
        let (width, height) = LOSSLESS_GEOMETRIES[geometry];
        let mut values = values[..4 * (width / 2) * (height / 2)].to_vec();
        if out_of_range > 0 {
            values[0] = if out_of_range == 1 { 9000 } else { -9000 };
        }
        let fixture = lossless_file((width, height), &values);
        if out_of_range == 0 {
            let image = decode(&fixture.file).unwrap();
            let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
            assert_eq!(samples, &fixture.expected);
        } else {
            assert!(matches!(decode(&fixture.file), Err(RawError::Corrupt(_))));
        }
    }

    /// Property 3: pathological Rice/Golomb prefixes inside well-formed frames are errors or clean
    /// decodes — never a panic, never a hang, always deterministic.
    #[test]
    fn adversarial_entropy_never_panics_and_is_deterministic(
        kind in 0usize..4,
        payload in proptest::collection::vec(any::<u8>(), 0..=600),
        slot in any::<usize>(),
    ) {
        let file = adversarial_frame(kind, &payload, slot);
        let first = decode(&file);
        assert_eq!(first, decode(&file), "repeated decodes of the same bytes must agree");
        if let Ok(image) = &first {
            let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
            assert!(samples.iter().all(|&value| value <= 16383), "C-RAW clamps to the CMP1 bit depth");
        }
        let _ = probe_info(&file);
    }

    /// Property 2 (adaptive side): random QP maps in the supported 131–168 band, including odd map heights,
    /// and random per-band base/gain parameters always decode, with every sample inside the 14-bit range.
    #[test]
    fn adaptive_qp_maps_stay_within_the_bit_depth(
        geometry in 0usize..V200_GEOMETRIES.len(),
        qp_values in proptest::collection::vec(131i32..=168, 16),
        ll3 in -2048i32..=2048,
        mid in -16i32..=16,
        gain in 0u32..=32,
        base in 0u32..=8,
    ) {
        let (width, height) = V200_GEOMETRIES[geometry];
        let (map_w, map_h) = (width / 16, height / 4);
        let mut gains = [0u32; 10];
        let mut bases = [0u32; 10];
        for (i, (g, b)) in gains.iter_mut().zip(bases.iter_mut()).enumerate() {
            if i >= 4 {
                *g = gain;
            }
            *b = if i < 4 { base + 1 } else { base };
        }
        let (header_size, sample) = constant_band_planes(
            width / 2,
            height / 2,
            3,
            ll3,
            Some((4, mid)),
            4,
            gains,
            bases,
            Some(&qp_values[..map_w * map_h]),
            3,
            false,
            false,
        );
        let coding = cmp1(0x200, width as u32, height as u32, width as u32, height as u32, 3, header_size as u32);
        let file = wrap(&coding, width as u32, height as u32, &sample, None);
        let image = decode(&file).unwrap();
        let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
        assert!(samples.iter().all(|&value| value <= 16383), "adaptive dequantization must stay in the bit depth");
        assert_eq!(probe_info(&file).unwrap(), image.info());
    }

    /// Property 4: randomly generated IAD1 fields either decode — with `active_area ⊆ sensor dims` and
    /// `crop ⊆ active_area` — or are rejected (no overflow or out-of-bounds geometry gets through).
    #[test]
    fn random_iad1_geometry_keeps_active_area_and_crop_invariants(
        base in 0usize..2,
        full in proptest::prop_oneof![Just(0u16), Just(2u16)],
        crop in quad(),
        masked_left in quad(),
        masked_top in quad(),
        active in quad(),
        width_off in -3i32..=3,
        height_off in -3i32..=3,
    ) {
        let (width, height, coding, sample) = match base {
            0 => (4usize, 4usize, cmp1(0x100, 4, 4, 4, 4, 0, 108), lossless_sample(100)),
            _ => (16usize, 16usize, cmp1(0x100, 16, 16, 16, 16, 3, 540), constant_bands_sample(0, None, 4)),
        };
        let clamp16 = |off: i32, base_dim: usize| u16::try_from((base_dim as i64 + off as i64).clamp(1, i64::from(u16::MAX))).unwrap();
        let area = iad1_bytes(clamp16(width_off, width), clamp16(height_off, height), full, crop, masked_left, masked_top, active);
        let file = wrap(&coding, width as u32, height as u32, &sample, Some(&area));
        assert_eq!(probe(&file), Some(RawFormat::Cr3));
        if let Ok(image) = decode(&file) {
            image.validate().unwrap();
            let a = image.active_area;
            assert!(a.width > 0 && a.height > 0, "{a:?}");
            assert!(a.x + a.width <= image.width && a.y + a.height <= image.height, "active area outside sensor: {a:?}");
            assert!(
                image.crop.x + image.crop.width <= a.width && image.crop.y + image.crop.height <= a.height,
                "crop outside active area: {:?} in {a:?}",
                image.crop
            );
            assert_eq!(probe_info(&file).unwrap(), image.info());
        }
    }
}

/// Raw sample bytes of the 4×4 lossless fixture (a 108-byte marker header plus four flat planes).
fn lossless_sample(value: i32) -> Vec<u8> {
    let band = Band { width: 2, height: 2, data: vec![value; 4] };
    let tiles = [std::array::from_fn(|_| vec![encode_band(&band, true)])];
    let (_, sample) = v100_sample(&tiles, 4);
    sample
}

/// Raw sample bytes of a constant-band 16×16 C-RAW fixture.
fn constant_bands_sample(ll3: i32, mid_band: Option<(usize, i32)>, quant: u8) -> Vec<u8> {
    let (_, sample) = constant_band_planes(8, 8, 3, ll3, mid_band, quant, [0; 10], [0; 10], None, 0, false, false);
    sample
}

// --- adversarial entropy frames (property 3) ---

/// A well-formed frame of the given kind (0: lossless 8×6, 1: C-RAW 16×16, 2: 0x200 with an adaptive
/// QP map, 3: the same 0x200 frame with the QP map itself replaced) in which one plane's entropy streams
/// (kinds 0 to 2) or the QP map stream (kind 3) are replaced by the adversarial payload. The marker
/// sizes follow the payload, so the layout stays parseable and the pathological bits reach the
/// Rice/Golomb readers; the untouched planes decode fine, reaching the mosaic stage on success.
fn adversarial_frame(kind: usize, payload: &[u8], slot: usize) -> Vec<u8> {
    let shapes = if kind == 0 { vec![(4usize, 3usize)] } else { band_shapes(8, 8, false, false).to_vec() };
    let mut tiles: Vec<[Vec<Vec<u8>>; 4]> = vec![Default::default()];
    for (p, bands) in tiles[0].iter_mut().enumerate() {
        *bands = if kind != 3 && p == slot % 4 {
            shapes.iter().map(|_| payload.to_vec()).collect()
        } else {
            shapes
                .iter()
                .enumerate()
                .map(|(i, &(w, h))| {
                    let band = Band { width: w, height: h, data: vec![0; w * h] };
                    encode_band(&band, i == 0)
                })
                .collect()
        };
    }
    let (header_size, sample) = if kind == 2 {
        let map = vec![150i32; 16];
        v200_sample(&tiles[0], [0; 10], [1; 10], &encode_qp_map(&map, 1, 4), 3)
    } else if kind == 3 {
        v200_sample(&tiles[0], [0; 10], [1; 10], payload, 3)
    } else {
        v100_sample(&tiles, 4)
    };
    let (width, height, levels) = match kind {
        0 => (8u32, 6u32, 0u8),
        1 => (16, 16, 3),
        _ => (16, 16, 3),
    };
    let version = if kind >= 2 { 0x200u16 } else { 0x100 };
    let coding = cmp1(version, width, height, width, height, levels, header_size as u32);
    wrap(&coding, width, height, &sample, None)
}

// --- exhaustive coverage of the verified quantization bands (property 2) ---

#[test]
fn every_uniform_quant_value_dequantizes_within_the_bit_depth() {
    for quant in 4u8..=44 {
        let step = quant_step(quant);
        // Overshoot: the dequantized DC leaves the 14-bit range and C-RAW clamps it.
        let fixture = constant_bands_file(8000, None, quant);
        let expected_value = (8192 + 8000 * step).clamp(0, 16383) as u16;
        let image = decode(&fixture.file).unwrap();
        assert_eq!(image.data, RawData::U16(vec![expected_value; 256]), "quant {quant}");
        assert_eq!(probe_info(&fixture.file).unwrap().bits, 14);
        // In range: the DC lands exactly on 8192 + level * step, so the step itself is observable
        // (the overshoot case above saturates for every step above 1).
        let level = 8000 / step;
        let fixture = constant_bands_file(level, None, quant);
        let image = decode(&fixture.file).unwrap();
        assert_eq!(image.data, RawData::U16(vec![(8192 + level * step) as u16; 256]), "quant {quant} in range");
    }
    for quant in [3u8, 45] {
        let fixture = constant_bands_file(8000, None, quant);
        assert!(matches!(decode(&fixture.file), Err(RawError::Unsupported(_))), "quant {quant}");
        assert!(matches!(probe_info(&fixture.file), Err(RawError::Unsupported(_))));
    }
}

#[test]
fn every_qp_value_in_the_supported_band_dequantizes_within_the_bit_depth() {
    let gains = [0, 0, 0, 0, 8, 8, 8, 8, 8, 8];
    let bases = [8, 8, 8, 8, 2, 2, 2, 2, 2, 2];
    let mut first_hash = 0u64;
    let mut last_hash = 0u64;
    for value in 131i32..=175 {
        let fixture = v200_constant_bands_file(16, 16, value, 400, Some((4, 8)), gains, bases);
        let image = decode(&fixture.file).unwrap();
        let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
        assert!(samples.iter().all(|&sample| sample <= 16383), "QP {value}");
        assert_eq!(probe_info(&fixture.file).unwrap(), image.info());
        if value == 131 {
            first_hash = sensor_hash(&image);
        }
        last_hash = sensor_hash(&image);
    }
    assert_ne!(first_hash, last_hash, "the QP value must reach the dequantization step");
    for value in [130i32, 176] {
        let fixture = v200_constant_bands_file(16, 16, value, 400, None, gains, bases);
        assert!(matches!(decode(&fixture.file), Err(RawError::Unsupported(_))), "QP {value}");
        assert!(matches!(probe_info(&fixture.file), Err(RawError::Unsupported(_))));
    }
}

// --- determinism (property 5) ---

#[test]
fn decoding_is_deterministic_across_repeated_and_parallel_invocations() {
    let craw = craw_roundtrip_file((24, 16, 2), 4, 7, &lcg_values(7, MAX_PLANE_VALUES));
    let lossless = lossless_file((8, 6), &lcg_values(11, 4 * 4 * 3));
    let adaptive = v200_constant_bands_file(16, 16, 150, 400, Some((4, 8)), [0, 0, 0, 0, 8, 8, 8, 8, 8, 8], [8, 8, 8, 8, 2, 2, 2, 2, 2, 2]);
    for fixture in [&craw, &lossless, &adaptive] {
        let first = decode(&fixture.file).unwrap();
        assert_eq!(first, decode(&fixture.file).unwrap(), "repeated decodes must be identical");
        let expected_hash = sensor_hash(&first);
        let hashes: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| sensor_hash(&decode(&fixture.file).unwrap()))).collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect()
        });
        assert!(hashes.iter().all(|&hash| hash == expected_hash), "parallel decodes must agree with the serial hash");
    }
}

// --- QP map shapes: odd heights, large jumps, over-long prefixes (properties 3 and 6) ---

/// An odd QP-map height (16×12 sensor, 1×3 map) decodes, and with step 1 (base 1, no gain) the flat LL3
/// coefficient comes through exactly.
#[test]
fn odd_qp_map_height_0x200_decodes_within_range() {
    let gains = [0; 10];
    let bases = [1; 10];
    let fixture = v200_constant_bands_file(16, 12, 150, 400, None, gains, bases);
    let image = decode(&fixture.file).unwrap();
    let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
    assert_eq!(samples, &vec![8592u16; 16 * 12], "step 1 (base 1) keeps the LL3 coefficient exact");
    assert_eq!(probe_info(&fixture.file).unwrap(), image.info());
}

/// Adaptive steps on an odd-height map: the coarser bands pair QP rows, averaging the QP values before the
/// step lookup, and the last row of an odd map has no partner and is used alone. Here HL2 has two rows: the
/// first pairs QP rows 0 and 1 (150 and 153, average 151), the second is QP row 2 alone (151). The adaptive
/// stream must therefore decode exactly like the uniform stream whose step is the one QP 151 gives,
/// and differ from the uniform streams one step either side.
#[test]
fn odd_height_qp_map_uses_its_last_row_alone() {
    let map = [150, 153, 151];
    let step = |qp: i32| 2 + 8 * quant_step((qp - 124) as u8) as u32 / 8;
    assert_eq!(step(151), 16);
    let adaptive_gains = [0, 0, 0, 0, 8, 8, 8, 8, 8, 8];
    let adaptive_bases = [8, 8, 8, 8, 2, 2, 2, 2, 2, 2];
    let uniform = |hl_step: u32| {
        let fixture = v200_mapped_file(16, 12, &map, 400, Some((4, 8)), [0; 10], [8, 8, 8, 8, hl_step, hl_step, hl_step, hl_step, hl_step, hl_step]);
        decode(&fixture.file).unwrap()
    };
    let fixture = v200_mapped_file(16, 12, &map, 400, Some((4, 8)), adaptive_gains, adaptive_bases);
    let adaptive = decode(&fixture.file).unwrap();
    assert_eq!(adaptive, uniform(step(151)), "QP rows 0+1 average to 151 and the unpaired row 2 is 151 alone");
    assert_ne!(adaptive, uniform(step(151) - 1));
    assert_ne!(adaptive, uniform(step(151) + 1));
    assert_eq!(probe_info(&fixture.file).unwrap(), adaptive.info());
}

/// A map whose neighbouring cells differ by up to 37 forces symbols past the plain Rice range, so the
/// 23-zero escape is exercised deterministically (a run of equal cells drops the Rice parameter to 0 first).
#[test]
fn qp_map_with_large_jumps_uses_the_escape_code() {
    let map = [131, 131, 131, 131, 168, 131, 168, 131];
    let gains = [0, 0, 0, 0, 8, 8, 8, 8, 8, 8];
    let bases = [8, 8, 8, 8, 2, 2, 2, 2, 2, 2];
    let fixture = v200_mapped_file(16, 32, &map, 400, Some((4, 8)), gains, bases);
    let image = decode(&fixture.file).unwrap();
    let RawData::U16(samples) = &image.data else { panic!("u16 sensor data expected") };
    assert!(samples.iter().all(|&sample| sample <= 16383));
    assert_eq!(probe_info(&fixture.file).unwrap(), image.info());
}

/// A QP Rice prefix longer than the 23-zero escape is corrupt data, not an endless read.
#[test]
fn qp_rice_prefix_longer_than_the_escape_is_corrupt() {
    // The 32-bit literal 300 (QP 150), then 24 zero bits with no terminating 1 within 23 zeros.
    let payload = [0x00, 0x00, 0x01, 0x2c, 0x00, 0x00, 0x00, 0x80];
    let file = adversarial_frame(3, &payload, 0);
    let error = decode(&file).unwrap_err();
    assert!(matches!(error, RawError::Corrupt(ref message) if message.contains("QP Rice escape prefix")), "{error:?}");
    assert!(probe_info(&file).is_err());
}
