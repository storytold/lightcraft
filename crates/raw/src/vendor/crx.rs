//! Canon CRX sensor sample decompression.
//!
//! Clean-room sources: Laurent Clévy's public prose CR3 format description
//! <https://github.com/lclevy/canon_cr3/blob/master/readme.md> (CMP1 and marker layouts),
//! Canon patent US20160323602A1 (MED prediction, signed mapping, adaptive Rice coding),
//! Canon patent JP2017192077A (zero-run flag and MELCODE of run length minus one), and
//! ITU-T T.87 (MELCODE run index table). No third-party decoder source was consulted.
//! CRX-specific boundaries and adaptive parameters were established by comparing
//! CC0 raw.pixls.us files against an external decoder used only as a black-box oracle.
//!
//! Current support: version 0x100 lossless Bayer RAW and horizontal-tile C-RAW,
//! plus version 0x200 single-tile 14-bit C-RAW with adaptive QP quantization.
//! Both ff01/ff02/ff03 and ff11/ff12/ff13 marker families are supported.
//! Six full M50/R100/R8 sensor fixtures match an external decoder sample for sample.
//! Unverified coding variants return explicit unsupported errors.

use super::crx_wavelet::{self, Band as WaveletBand, TileEdges};
use crate::{MAX_SAMPLES, RawError, Result};
use lightcraft_meta::cr3::Cr3Compression;
use rayon::prelude::*;
use std::ops::Range;

const MAX_TILES: usize = 1024;
const MAX_HEADER_BYTES: usize = 1 << 20;
const MAX_CRX_SAMPLES: usize = 200_000_000;
const MAX_CRX_EDGE: u32 = 65_536;
const RUN_EXPONENT: [u8; 32] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

fn corrupt(reason: &str) -> RawError {
    RawError::Corrupt(format!("Canon CRX: {reason}"))
}

#[derive(Debug)]
struct Band {
    data: Range<usize>,
    padding: usize,
    quant: Quantization,
    partial: bool,
}

#[derive(Debug)]
enum Quantization {
    Uniform(u8),
    Adaptive { base: u32, gain: u32 },
}

struct QpMap {
    width: usize,
    height: usize,
    data: Vec<i32>,
}

#[derive(Debug)]
struct Plane {
    partial: bool,
    rounded_bits: u8,
    bands: Vec<Band>,
}

#[derive(Debug)]
struct Tile {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    planes: Vec<Plane>,
    qp: Option<Range<usize>>,
}

/// Decode to full sensor-order CFA samples, including masked borders.
pub(crate) fn decode(sample: &[u8], config: &Cr3Compression) -> Result<Vec<u16>> {
    supports(config)?;
    let width = usize::try_from(config.width).map_err(|_| RawError::Limit("CRX width"))?;
    let count = image_size(config)?;
    let tiles = layout(sample, config)?;
    validate_planes(&tiles, config)?;
    // Allocated once the first plane decodes: a tiny crafted file can claim a huge image.
    let mut output = Vec::new();
    let midpoint = 1i32 << (config.bit_depth - 1);
    let maximum = (1i32 << config.bit_depth) - 1;
    for tile in tiles {
        let qp = tile_qp(sample, &tile)?;
        // Each plane is its own entropy stream: decode a tile's planes in parallel.
        let planes: Vec<Result<Vec<i32>>> = tile
            .planes
            .par_iter()
            .enumerate()
            .map(|(p, plane)| {
                if config.levels == 0 {
                    let band = plane.bands.first().ok_or_else(|| corrupt("missing lossless band"))?;
                    lossless_plane(band_bytes(sample, band)?, tile.width / 2, tile.height / 2)
                } else {
                    let edges = TileEdges { left: tile.x > 0, right: tile.x + tile.width < width, top: false, bottom: false };
                    wavelet_plane(sample, plane, tile.width / 2, tile.height / 2, edges, qp.as_ref())
                }
                .map_err(|error| RawError::Corrupt(format!("Canon CRX tile ({}, {}), plane {p}: {error}", tile.x, tile.y)))
            })
            .collect();
        for (p, decoded) in planes.into_iter().enumerate() {
            let decoded = decoded?;
            if output.is_empty() {
                output.try_reserve_exact(count).map_err(|_| RawError::Limit("CRX sample allocation"))?;
                output.resize(count, 0);
            }
            for (i, &value) in decoded.iter().enumerate() {
                let sensor_value =
                    value.checked_add(midpoint).filter(|&v| v >= 0 && v <= maximum).ok_or_else(|| corrupt("decoded sample outside its bit depth"))?;
                let (x, y) = (tile.x + 2 * (i % (tile.width / 2)) + p % 2, tile.y + 2 * (i / (tile.width / 2)) + p / 2);
                let slot = y
                    .checked_mul(width)
                    .and_then(|n| n.checked_add(x))
                    .and_then(|n| output.get_mut(n))
                    .ok_or_else(|| corrupt("tile writes outside image"))?;
                *slot = sensor_value as u16;
            }
        }
    }
    Ok(output)
}

/// Header-only probing must reject the same unsupported coding variants as full decoding.
pub(crate) fn validate(sample: &[u8], config: &Cr3Compression) -> Result<()> {
    supports(config)?;
    image_size(config)?;
    let tiles = layout(sample, config)?;
    validate_planes(&tiles, config)?;
    for tile in &tiles {
        tile_qp(sample, tile)?;
    }
    Ok(())
}

fn image_size(config: &Cr3Compression) -> Result<usize> {
    if config.width > MAX_CRX_EDGE || config.height > MAX_CRX_EDGE {
        return Err(RawError::Limit("CRX image edge"));
    }
    (config.width as usize)
        .checked_mul(config.height as usize)
        .filter(|&n| n > 0 && n <= MAX_SAMPLES.min(MAX_CRX_SAMPLES))
        .ok_or(RawError::Limit("CRX image size"))
}

fn supports(config: &Cr3Compression) -> Result<()> {
    if ![0x100, 0x200].contains(&config.version)
        || config.encoding != 0
        || config.planes != 4
        || config.cfa_pattern > 3
        || ![0, 3].contains(&config.levels)
    {
        return Err(RawError::Unsupported(format!(
            "Canon CRX version {:#x}, encoding {}, {} planes, {} wavelet levels",
            config.version, config.encoding, config.planes, config.levels
        )));
    }
    if !(8..=16).contains(&config.bit_depth) {
        return Err(RawError::Unsupported(format!("Canon CRX {}-bit samples", config.bit_depth)));
    }
    if config.levels == 3 && config.tile_height != config.height {
        return Err(RawError::Unsupported("Canon CRX C-RAW with vertical tiles".into()));
    }
    if config.version == 0x200 && (config.levels != 3 || config.bit_depth != 14 || config.tile_width != config.width) {
        return Err(RawError::Unsupported("Canon CRX version 0x200 requires a single-tile 14-bit Bayer C-RAW image".into()));
    }
    if config.version == 0x200 && !config.height.div_ceil(4).is_multiple_of(2) {
        return Err(RawError::Unsupported("Canon CRX adaptive QP map with an odd height".into()));
    }
    if config.median_bit_depth.is_some() {
        return Err(RawError::Unsupported("Canon CRX extended median-bit-depth header".into()));
    }
    Ok(())
}

fn validate_planes(tiles: &[Tile], config: &Cr3Compression) -> Result<()> {
    for tile in tiles {
        for plane in &tile.planes {
            if !plane.partial || plane.rounded_bits != 0 || plane.bands.len() != 1 + usize::from(config.levels) * 3 {
                return Err(RawError::Unsupported("Canon CRX plane coding flags".into()));
            }
            for (index, band) in plane.bands.iter().enumerate() {
                if band.partial {
                    return Err(RawError::Unsupported("Canon CRX partial subband coding".into()));
                }
                if config.levels != 0 {
                    match band.quant {
                        Quantization::Uniform(value) => {
                            quant_step(value)?;
                        }
                        Quantization::Adaptive { gain, .. } => {
                            if index < 4 && gain != 0 {
                                return Err(RawError::Unsupported("Canon CRX adaptive quantization subband parameters".into()));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn layout(sample: &[u8], config: &Cr3Compression) -> Result<Vec<Tile>> {
    let (width, height, tw, th) = (config.width as usize, config.height as usize, config.tile_width as usize, config.tile_height as usize);
    if width == 0
        || height == 0
        || tw == 0
        || th == 0
        || tw > width
        || th > height
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || !tw.is_multiple_of(2)
        || !th.is_multiple_of(2)
    {
        return Err(corrupt("invalid Bayer tile dimensions"));
    }
    let columns = width.div_ceil(tw);
    let rows = height.div_ceil(th);
    let tile_count = columns.checked_mul(rows).filter(|&n| n <= MAX_TILES).ok_or(RawError::Limit("CRX tile count"))?;
    let header_size = config.header_size as usize;
    if header_size > MAX_HEADER_BYTES {
        return Err(RawError::Limit("CRX header size"));
    }
    let header = sample.get(..header_size).ok_or_else(|| corrupt("truncated tile headers"))?;
    let mut reader = Header { src: header, at: 0 };
    let mut data_at = header_size;
    let mut tiles = Vec::with_capacity(tile_count);
    for index in 0..tile_count {
        let modern = config.version == 0x200;
        let body = reader.marker(if modern { 0xff11 } else { 0xff01 }, if modern { 16 } else { 8 })?;
        let tile_size = usize::try_from(be32(body, 0)?).map_err(|_| RawError::Limit("CRX tile size"))?;
        if usize::from(be16(body, 4)?) != index {
            return Err(corrupt("tile index out of sequence"));
        }
        let tile_end = data_at.checked_add(tile_size).filter(|&end| end <= sample.len()).ok_or_else(|| corrupt("tile data outside sample"))?;
        let qp = if modern {
            let length = be32(body, 8)? as usize;
            let padding = usize::from(be16(body, 12)?);
            if be16(body, 6)? != 0x4000 || be16(body, 14)? != 0 || length == 0 || padding > 7 {
                return Err(RawError::Unsupported("Canon CRX adaptive quantization tile header".into()));
            }
            let end = data_at.checked_add(length).filter(|&end| end <= tile_end).ok_or_else(|| corrupt("QP data outside tile"))?;
            let data = data_at..end;
            data_at = end.checked_add(padding).filter(|&end| end <= tile_end).ok_or_else(|| corrupt("QP padding outside tile"))?;
            Some(data)
        } else {
            None
        };
        let mut planes = Vec::with_capacity(usize::from(config.planes));
        for p in 0..config.planes {
            let body = reader.marker(if modern { 0xff12 } else { 0xff02 }, 8)?;
            let size = be32(body, 0)? as usize;
            let flags = be32(body, 4)?;
            if flags >> 28 != u32::from(p) {
                return Err(corrupt("plane index out of sequence"));
            }
            let plane_end = data_at.checked_add(size).filter(|&end| end <= tile_end).ok_or_else(|| corrupt("plane size exceeds tile"))?;
            let mut bands = Vec::with_capacity(usize::from(config.levels) * 3 + 1);
            for b in 0..u32::from(config.levels) * 3 + 1 {
                let body = reader.marker(if modern { 0xff13 } else { 0xff03 }, if modern { 16 } else { 8 })?;
                let band_size = be32(body, 0)? as usize;
                let band_flags = be32(body, 4)?;
                if band_flags >> 28 != b {
                    return Err(corrupt("band index out of sequence"));
                }
                let band_end = data_at.checked_add(band_size).filter(|&end| end <= plane_end).ok_or_else(|| corrupt("band size exceeds plane"))?;
                let padding = if modern { usize::from(be16(body, 12)?) } else { (band_flags & 0x7ffff) as usize };
                if padding > band_size || (modern && padding > 7) {
                    return Err(corrupt("invalid band padding"));
                }
                bands.push(Band {
                    data: data_at..band_end,
                    padding,
                    quant: if modern {
                        if be16(body, 14)? != 0 {
                            return Err(RawError::Unsupported("Canon CRX adaptive subband reserved field".into()));
                        }
                        Quantization::Adaptive { base: be32(body, 8)?, gain: band_flags & 0x07ff_ffff }
                    } else {
                        Quantization::Uniform(((band_flags >> 19) & 0xff) as u8)
                    },
                    partial: band_flags & 0x0800_0000 != 0,
                });
                data_at = band_end;
            }
            if data_at != plane_end {
                return Err(corrupt("band sizes do not sum to plane size"));
            }
            planes.push(Plane { partial: flags & 0x0800_0000 != 0, rounded_bits: ((flags >> 25) & 3) as u8, bands });
        }
        if data_at != tile_end {
            return Err(corrupt("plane sizes do not sum to tile size"));
        }
        let (x, y) = ((index % columns) * tw, (index / columns) * th);
        tiles.push(Tile { x, y, width: tw.min(width - x), height: th.min(height - y), planes, qp });
    }
    if header.get(reader.at..).is_none_or(|rest| rest.iter().any(|&byte| byte != 0)) {
        return Err(corrupt("unexpected bytes after tile headers"));
    }
    if data_at != sample.len() {
        return Err(corrupt("tile sizes do not cover the sample"));
    }
    Ok(tiles)
}

fn band_bytes<'a>(sample: &'a [u8], band: &Band) -> Result<&'a [u8]> {
    let end =
        band.data.end.checked_sub(band.padding).filter(|&end| end >= band.data.start).ok_or_else(|| corrupt("band padding exceeds its size"))?;
    sample.get(band.data.start..end).ok_or_else(|| corrupt("band data outside sample"))
}

fn tile_qp(sample: &[u8], tile: &Tile) -> Result<Option<QpMap>> {
    tile.qp
        .as_ref()
        .map(|range| {
            let bytes = sample.get(range.clone()).ok_or_else(|| corrupt("QP map outside sample"))?;
            qp_map(bytes, tile.width.div_ceil(16), tile.height.div_ceil(4))
        })
        .transpose()
}

fn quant_step(value: u8) -> Result<i32> {
    // Measured independently with an external decoder after modifying only the
    // quantValue field: all forty values 4..43 follow this integer sequence.
    if !(4..=43).contains(&value) {
        return Err(RawError::Unsupported(format!("Canon CRX quantization value {value}")));
    }
    let scale = [40i32, 45, 51, 57, 64, 72].get(usize::from(value % 6)).copied().ok_or_else(|| corrupt("quantization table index"))?;
    Ok((scale << (value / 6)) / 64)
}

fn split_dimension(length: usize, before: bool, after: bool) -> (usize, usize) {
    (length.div_ceil(2) + usize::from(after && length.is_multiple_of(2)), length / 2 + usize::from(before) + usize::from(after))
}

fn wavelet_plane(sample: &[u8], plane: &Plane, width: usize, height: usize, edges: TileEdges, qp: Option<&QpMap>) -> Result<Vec<i32>> {
    let mut dimensions = vec![(width, height)];
    let mut high_dimensions = Vec::new();
    for _ in 0..3 {
        let &(w, h) = dimensions.last().ok_or_else(|| corrupt("missing wavelet dimensions"))?;
        let (lw, hw) = split_dimension(w, edges.left, edges.right);
        let (lh, hh) = split_dimension(h, edges.top, edges.bottom);
        dimensions.push((lw, lh));
        high_dimensions.push([(hw, lh), (lw, hh), (hw, hh)]);
    }
    let mut shapes = vec![*dimensions.last().ok_or_else(|| corrupt("missing LL dimensions"))?];
    for high in high_dimensions.iter().rev() {
        shapes.extend_from_slice(high);
    }
    let mut bands = Vec::with_capacity(10);
    for (i, (&(w, h), band)) in shapes.iter().zip(&plane.bands).enumerate() {
        let bytes = band_bytes(sample, band)?;
        let mut data = if i == 0 {
            lossless_plane(bytes, w, h)?
        } else {
            // Some CRX high-frequency bands retain a redundant right column
            // beyond synthesis support (LH2 in the M50/R100 fixtures). Infer
            // that small extension only from a complete, correctly aligned
            // entropy stream, then crop each row to the synthesis support.
            let mut decoded = None;
            for extra in 0..=2 {
                let coded_width = w.checked_add(extra).ok_or(RawError::Limit("CRX band width"))?;
                if let Ok(values) = high_frequency_band(bytes, coded_width, h) {
                    if extra == 0 {
                        decoded = Some(values);
                    } else {
                        let mut cropped = Vec::new();
                        cropped.try_reserve_exact(w * h).map_err(|_| RawError::Limit("CRX cropped band allocation"))?;
                        for row in values.chunks_exact(coded_width) {
                            cropped.extend_from_slice(row.get(..w).ok_or_else(|| corrupt("band crop outside row"))?);
                        }
                        decoded = Some(cropped);
                    }
                    break;
                }
            }
            decoded.ok_or_else(|| RawError::Corrupt(format!("Canon CRX band {i}: unsupported geometry or corrupt entropy")))?
        };
        for (position, value) in data.iter_mut().enumerate() {
            let step = match band.quant {
                Quantization::Uniform(value) => quant_step(value)?,
                Quantization::Adaptive { base, gain } => {
                    adaptive_step(qp.ok_or_else(|| corrupt("missing QP map"))?, i, position % w, position / w, base, gain)?
                }
            };
            *value = value.checked_mul(step).ok_or_else(|| corrupt("dequantized coefficient overflow"))?;
        }
        bands.push(WaveletBand { width: w, height: h, data });
    }
    crx_wavelet::reconstruct(&bands, width, height, 3, edges)
}

fn adaptive_step(map: &QpMap, band: usize, x: usize, y: usize, base: u32, gain: u32) -> Result<i32> {
    let step = if gain == 0 {
        u64::from(base)
    } else {
        let (row, column) = if (4..=6).contains(&band) {
            (y * 2, x / 2)
        } else if (7..=9).contains(&band) {
            (y, x / 4)
        } else {
            return Err(RawError::Unsupported("Canon CRX adaptive low-frequency quantization".into()));
        };
        if row >= map.height || column >= map.width {
            return Err(corrupt("QP coordinate outside map"));
        }
        let index = row.checked_mul(map.width).and_then(|n| n.checked_add(column)).ok_or(RawError::Limit("CRX QP map index"))?;
        let first = *map.data.get(index).ok_or_else(|| corrupt("QP coordinate outside map"))?;
        let value = if band < 7 {
            let second_row = row + 1;
            let second = *map.data.get(second_row * map.width + column).ok_or_else(|| corrupt("QP average outside map"))?;
            (first + second) / 2
        } else {
            first
        };
        // QP rows are averaged before looking up the integer step. R8 field
        // mutations distinguish this order from averaging dequantization steps.
        let quant = u8::try_from(value - 124).map_err(|_| corrupt("QP value outside quantization table"))?;
        u64::from(base) + u64::from(gain) * quant_step(quant)? as u64 / 8
    };
    i32::try_from(step.max(1)).map_err(|_| corrupt("adaptive quantization step outside precision"))
}

struct Header<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> Header<'a> {
    fn marker(&mut self, wanted: u16, wanted_size: usize) -> Result<&'a [u8]> {
        let marker = be16(self.src, self.at)?;
        if marker == wanted + 0x10 {
            return Err(RawError::Unsupported("Canon CRX ff11/ff12/ff13 marker family".into()));
        }
        if marker != wanted {
            return Err(corrupt("unexpected tile/plane/band marker"));
        }
        let size = usize::from(be16(self.src, self.at + 2)?);
        if size != wanted_size {
            return Err(RawError::Unsupported(format!("Canon CRX marker {marker:#x} header length {size}")));
        }
        let start = self.at.checked_add(4).ok_or(RawError::Limit("CRX header offset"))?;
        let end = start.checked_add(size).ok_or(RawError::Limit("CRX header offset"))?;
        let body = self.src.get(start..end).ok_or_else(|| corrupt("truncated marker header"))?;
        self.at = end;
        Ok(body)
    }
}

fn be16(src: &[u8], at: usize) -> Result<u16> {
    let end = at.checked_add(2).ok_or(RawError::Limit("CRX header offset"))?;
    let bytes = src.get(at..end).and_then(|s| <[u8; 2]>::try_from(s).ok()).ok_or_else(|| corrupt("truncated header field"))?;
    Ok(u16::from_be_bytes(bytes))
}

fn be32(src: &[u8], at: usize) -> Result<u32> {
    let end = at.checked_add(4).ok_or(RawError::Limit("CRX header offset"))?;
    let bytes = src.get(at..end).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or_else(|| corrupt("truncated header field"))?;
    Ok(u32::from_be_bytes(bytes))
}

struct Bits<'a> {
    src: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    /// The next 32 bits, MSB first, when at least that many remain before the end of the stream.
    fn peek32(&self) -> Option<u32> {
        let bytes = self.src.get(self.at / 8..self.at / 8 + 5)?;
        let word = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
        Some((word >> (8 - self.at % 8)) as u32)
    }

    fn take(&mut self, count: u32) -> Result<u32> {
        if count > 24 {
            return Err(corrupt("invalid Rice parameter"));
        }
        if count == 0 {
            return Ok(0);
        }
        if let Some(window) = self.peek32() {
            self.at += count as usize;
            return Ok(window >> (32 - count));
        }
        // Near the end of the stream: bit by bit, so truncation is reported exactly.
        let mut value = 0;
        for _ in 0..count {
            let byte = self.src.get(self.at / 8).ok_or_else(|| corrupt("truncated entropy stream"))?;
            value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }

    fn rice(&mut self, k: u32) -> Result<u32> {
        // Common case: the unary prefix and its terminating 1 lie within the next 32 bits.
        if let Some(window) = self.peek32().filter(|&w| w != 0) {
            let zeros = window.leading_zeros();
            self.at += zeros as usize + 1;
            return if k <= 20 { Ok((zeros << k) | self.take(k)?) } else { Err(corrupt("Rice parameter exceeds sample precision")) };
        }
        let mut zeros = 0;
        while self.take(1)? == 0 {
            zeros += 1;
            if zeros > 41 {
                return Err(corrupt("Rice escape prefix exceeds 41 bits"));
            }
        }
        if zeros == 41 {
            self.take(21)
        } else if k <= 20 {
            Ok((zeros << k) | self.take(k)?)
        } else {
            Err(corrupt("Rice parameter exceeds sample precision"))
        }
    }

    fn qp_rice(&mut self, k: u32) -> Result<u32> {
        let mut zeros = 0;
        while zeros < 16 && self.take(1)? == 0 {
            zeros += 1;
        }
        if zeros == 16 {
            // QP escape has no unary terminating 1 before the 16-bit literal.
            self.take(16)
        } else if k <= 20 {
            Ok((zeros << k) | self.take(k)?)
        } else {
            Err(corrupt("QP Rice parameter exceeds precision"))
        }
    }

    fn finish(&self) -> Result<()> {
        let bits = self.src.len().checked_mul(8).ok_or(RawError::Limit("CRX entropy length"))?;
        if bits.saturating_sub(self.at) > 7 {
            return Err(corrupt("unconsumed entropy data"));
        }
        if let Some(&last) = self.src.last() {
            let remaining = (bits - self.at) as u32;
            if u32::from(last) & ((1 << remaining) - 1) != 0 {
                return Err(corrupt("nonzero entropy alignment bits"));
            }
        }
        Ok(())
    }
}

fn signed(symbol: u32) -> i32 {
    (symbol >> 1) as i32 ^ -((symbol & 1) as i32)
}

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

fn median(left: i32, above: i32, corner: i32) -> i32 {
    if corner >= left.max(above) {
        left.min(above)
    } else if corner <= left.min(above) {
        left.max(above)
    } else {
        left + above - corner
    }
}

fn run(bits: &mut Bits<'_>, index: &mut usize, remaining: usize) -> Result<usize> {
    let mut length = 1usize;
    while length < remaining {
        let exponent = u32::from(*RUN_EXPONENT.get(*index).ok_or_else(|| corrupt("run index outside table"))?);
        if bits.take(1)? == 0 {
            length += bits.take(exponent)? as usize;
            *index = index.saturating_sub(1);
            if length > remaining {
                return Err(corrupt("run exceeds row"));
            }
            break;
        }
        length += 1usize << exponent;
        if length <= remaining {
            *index = (*index + 1).min(RUN_EXPONENT.len() - 1);
        }
    }
    Ok(length.min(remaining))
}

fn lossless_plane(src: &[u8], width: usize, height: usize) -> Result<Vec<i32>> {
    let count = width
        .checked_mul(height)
        .filter(|&n| width > 0 && n > 0 && n <= MAX_SAMPLES.min(MAX_CRX_SAMPLES) / 4)
        .ok_or(RawError::Limit("CRX plane size"))?;
    // Grows per decoded row: a truncated stream fails before the whole plane is reserved.
    let mut output = Vec::new();
    let mut previous = Vec::new();
    previous.try_reserve_exact(width).map_err(|_| RawError::Limit("CRX row allocation"))?;
    previous.resize(width, 0i32);
    let mut current = Vec::new();
    current.try_reserve_exact(width).map_err(|_| RawError::Limit("CRX row allocation"))?;
    current.resize(width, 0i32);
    let (mut k, mut run_index) = (0u32, 0usize);
    let mut bits = Bits { src, at: 0 };
    for y in 0..height {
        let mut x = 0;
        let mut left = previous.first().copied().ok_or_else(|| corrupt("empty plane row"))?;
        while x < width {
            let mut interrupted = false;
            let above = *previous.get(x).ok_or_else(|| corrupt("above sample outside row"))?;
            let right = previous.get(x + 1).copied().unwrap_or(above);
            if x + 1 < width && left == above && left == right && bits.take(1)? != 0 {
                let length = run(&mut bits, &mut run_index, width - x)?;
                let end = x.checked_add(length).ok_or(RawError::Limit("CRX run length"))?;
                current.get_mut(x..end).ok_or_else(|| corrupt("run outside row"))?.fill(left);
                x = end;
                if x == width {
                    continue;
                }
                // A run interruption always uses regular prediction; no second run flag.
                interrupted = true;
            }
            let above = *previous.get(x).ok_or_else(|| corrupt("above sample outside row"))?;
            let corner = if x == 0 { above } else { *previous.get(x - 1).ok_or_else(|| corrupt("corner sample outside row"))? };
            let right = previous.get(x + 1).copied().unwrap_or(above);
            let symbol = bits.rice(k)?;
            let prediction = if interrupted { above } else { median(left, above, corner) };
            let value = prediction
                .checked_add(signed(symbol))
                .filter(|v| (-1_048_576..=1_048_575).contains(v))
                .ok_or_else(|| RawError::Corrupt(format!("Canon CRX prediction outside precision at row {y}, column {x}, k {k}, symbol {symbol}")))?;
            *current.get_mut(x).ok_or_else(|| corrupt("decoded sample outside row"))? = value;
            left = value;
            let estimate = if y > 0 && x + 1 < width {
                symbol.checked_add(2 * right.abs_diff(above)).ok_or_else(|| corrupt("Rice estimate overflow"))? / 2
            } else {
                symbol
            };
            k = update_rice(k, estimate);
            x += 1;
        }
        output.try_reserve(width).map_err(|_| RawError::Limit("CRX plane allocation"))?;
        output.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
    }
    bits.finish()?;
    debug_assert_eq!(output.len(), count);
    Ok(output)
}

fn high_frequency_band(src: &[u8], width: usize, height: usize) -> Result<Vec<i32>> {
    let count = width
        .checked_mul(height)
        .filter(|&n| width > 0 && n > 0 && n <= MAX_SAMPLES.min(MAX_CRX_SAMPLES) / 4)
        .ok_or(RawError::Limit("CRX high-frequency band size"))?;
    // Grows per decoded row, as in `lossless_plane`.
    let mut output = Vec::new();
    let row = || -> Result<Vec<i32>> {
        let mut values = Vec::new();
        values.try_reserve_exact(width).map_err(|_| RawError::Limit("CRX row allocation"))?;
        values.resize(width, 0);
        Ok(values)
    };
    let (mut previous, mut current) = (row()?, row()?);
    let (mut previous_k, mut current_k) = (row()?, row()?);
    let (mut k, mut run_index) = (0u32, 0usize);
    let mut bits = Bits { src, at: 0 };
    for y in 0..height {
        let (mut x, mut left) = (0, 0);
        while x < width {
            let above = *previous.get(x).ok_or_else(|| corrupt("above coefficient outside row"))?;
            let right = previous.get(x + 1).copied().unwrap_or(above);
            let mut nonzero = false;
            if x + 1 < width && left == 0 && above == 0 && right == 0 {
                if bits.take(1)? != 0 {
                    let length = run(&mut bits, &mut run_index, width - x)?;
                    let end = x.checked_add(length).ok_or(RawError::Limit("CRX run length"))?;
                    current.get_mut(x..end).ok_or_else(|| corrupt("coefficient run outside row"))?.fill(0);
                    // The upper-row context for a zero run is zero, although the
                    // current Rice parameter remains unchanged across the run.
                    current_k.get_mut(x..end).ok_or_else(|| corrupt("Rice context run outside row"))?.fill(0);
                    left = 0;
                    x = end;
                    if x == width {
                        continue;
                    }
                }
                nonzero = true;
            }
            let coded = bits.rice(k)?;
            // A zero-run flag 0 or its interruption proves that the coefficient
            // is nonzero. That context omits zero from the signed mapping.
            let symbol = coded.checked_add(u32::from(nonzero)).ok_or_else(|| corrupt("coefficient symbol overflow"))?;
            let value = signed(symbol);
            if !(-1_048_576..=1_048_575).contains(&value) {
                return Err(corrupt("high-frequency coefficient outside precision"));
            }
            *current.get_mut(x).ok_or_else(|| corrupt("coefficient outside row"))? = value;
            left = value;
            k = update_rice(k, coded);
            if y > 0 && x + 1 < width {
                let upper_right_k = *previous_k.get(x + 1).ok_or_else(|| corrupt("Rice context outside row"))? as u32;
                if upper_right_k > k + 1 {
                    k += 1;
                }
            }
            *current_k.get_mut(x).ok_or_else(|| corrupt("Rice context outside row"))? = k as i32;
            x += 1;
        }
        output.try_reserve(width).map_err(|_| RawError::Limit("CRX coefficient allocation"))?;
        output.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
        std::mem::swap(&mut current_k, &mut previous_k);
    }
    bits.finish()?;
    debug_assert_eq!(output.len(), count);
    Ok(output)
}

fn qp_map(src: &[u8], width: usize, height: usize) -> Result<QpMap> {
    if !height.is_multiple_of(2) {
        return Err(RawError::Unsupported("Canon CRX adaptive QP map with an odd height".into()));
    }
    let count = width.checked_mul(height).filter(|&n| width > 0 && n > 0 && n <= MAX_CRX_SAMPLES / 32).ok_or(RawError::Limit("CRX QP map size"))?;
    let mut data = Vec::new();
    data.try_reserve_exact(count).map_err(|_| RawError::Limit("CRX QP allocation"))?;
    let row = || -> Result<Vec<i32>> {
        let mut values = Vec::new();
        values.try_reserve_exact(width).map_err(|_| RawError::Limit("CRX QP row allocation"))?;
        values.resize(width, 0);
        Ok(values)
    };
    let (mut previous, mut current) = (row()?, row()?);
    let mut bits = Bits { src, at: 0 };
    let mut k = 0;
    for y in 0..height {
        let mut left = *previous.first().ok_or_else(|| corrupt("empty QP row"))?;
        for x in 0..width {
            let above = *previous.get(x).ok_or_else(|| corrupt("above QP outside row"))?;
            let corner = if x == 0 { above } else { *previous.get(x - 1).ok_or_else(|| corrupt("corner QP outside row"))? };
            let right = previous.get(x + 1).copied().unwrap_or(above);
            let symbol = bits.qp_rice(k)?;
            let value = median(left, above, corner).checked_add(signed(symbol)).ok_or_else(|| corrupt("QP prediction overflow"))?;
            if !(131..=167).contains(&value) {
                return Err(RawError::Unsupported(format!("Canon CRX adaptive QP value {value}")));
            }
            *current.get_mut(x).ok_or_else(|| corrupt("QP outside row"))? = value;
            left = value;
            let estimate = if y > 0 && x + 1 < width {
                symbol.checked_add(2 * right.abs_diff(above)).ok_or_else(|| corrupt("QP Rice estimate overflow"))? / 2
            } else {
                symbol
            };
            k = update_rice(k, estimate);
        }
        data.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
    }
    bits.finish()?;
    Ok(QpMap { width, height, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coding() -> Cr3Compression {
        Cr3Compression {
            version: 0x100,
            width: 4,
            height: 4,
            tile_width: 4,
            tile_height: 4,
            bit_depth: 14,
            planes: 4,
            cfa_pattern: 0,
            encoding: 0,
            levels: 0,
            tile_flags: 0,
            header_size: 108,
            median_bit_depth: None,
        }
    }

    fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
        out.extend(code.to_be_bytes());
        out.extend(8u16.to_be_bytes());
        out.extend(size.to_be_bytes());
        out.extend(flags.to_be_bytes());
    }

    fn flat_sample() -> Vec<u8> {
        let mut out = Vec::new();
        marker(&mut out, 0xff01, 4, 0);
        for p in 0..4 {
            marker(&mut out, 0xff02, 1, (p << 28) | 0x0800_0000);
            marker(&mut out, 0xff03, 1, 0x0020_0000);
        }
        // Four 2×2 planes at coefficient 1. Row 0: zero-run flag 0, Rice(2,k0)=001,
        // Rice(0,k0)=1. Row 1: nonzero-run flag 1, MELCODE(RL−1=1)=1. Zero alignment.
        out.extend([0x1e; 4]);
        out
    }

    #[test]
    fn sensor_mosaic_from_four_lossless_planes() {
        let result = decode(&flat_sample(), &coding()).unwrap();
        assert_eq!(result, vec![8193; 16]);
    }

    #[test]
    fn med_signed_mapping_and_adaptation() {
        assert_eq!([signed(0), signed(1), signed(2), signed(3)], [0, -1, 1, -2]);
        assert_eq!(median(-7, -8, -9), -7);
        assert_eq!(median(-7, -8, -6), -8);
        assert_eq!(median(10, 20, 14), 16);
        assert_eq!(update_rice(0, 12_289), 2);
        assert_eq!(update_rice(2, 58), 4);
        assert_eq!(update_rice(4, 7), 3);
        assert_eq!(update_rice(4, 8), 4);
    }

    #[test]
    fn interrupted_run_predicts_from_above() {
        // First row [5, 5, 8, 9]. The next row runs three 5s, so interruption at
        // column 3 must predict the above sample 9, then add −2 to obtain 7.
        assert_eq!(lossless_plane(&[0x00, 0x18, 0x2d, 0xdc], 4, 2).unwrap(), [5, 5, 8, 9, 5, 5, 5, 7]);
    }

    #[test]
    fn partial_final_run_segment_keeps_its_index() {
        let mut bits = Bits { src: &[0xc0], at: 0 };
        let mut index = 11;
        assert_eq!(run(&mut bits, &mut index, 6).unwrap(), 6);
        assert_eq!(index, 12);
        assert_eq!(bits.at, 2);
    }

    #[test]
    fn every_truncated_header_is_an_error() {
        let sample = flat_sample();
        for n in 0..sample.len() {
            assert!(decode(&sample[..n], &coding()).is_err(), "prefix {n}");
        }
    }

    #[test]
    fn inconsistent_parent_sizes_and_indices_are_errors() {
        let original = flat_sample();
        for (at, mask) in [(7, 1), (9, 1), (19, 1), (20, 0x10), (31, 1), (32, 0x10)] {
            let mut broken = original.clone();
            broken[at] ^= mask;
            assert!(decode(&broken, &coding()).is_err(), "offset {at}");
        }
    }

    #[test]
    fn entropy_truncation_and_invalid_alignment_are_errors() {
        assert!(lossless_plane(&[], 2, 2).is_err());
        assert!(lossless_plane(&[0x1f], 2, 2).is_err());
        assert!(lossless_plane(&[0x1e, 0], 2, 2).is_err());
        assert!(lossless_plane(&[0; 8], 2, 2).is_err());
    }

    #[test]
    fn high_frequency_zero_runs_and_nonzero_mapping() {
        assert_eq!(high_frequency_band(&[0xf0], 2, 2).unwrap(), [0, 0, 0, 0]);
        assert_eq!(high_frequency_band(&[0x34, 0x80], 2, 2).unwrap(), [1, 0, -1, 1]);
        assert!(high_frequency_band(&[0x34], 2, 2).is_err());
        assert!(high_frequency_band(&[0xf1], 2, 2).is_err());
    }

    #[test]
    fn unsupported_wavelet_variant_is_explicit() {
        let mut config = coding();
        config.levels = 2;
        assert!(matches!(decode(&flat_sample(), &config), Err(RawError::Unsupported(_))));
        assert!(matches!(validate(&flat_sample(), &config), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn three_level_craw_reconstructs_sensor_samples() {
        let mut config = coding();
        config.width = 16;
        config.height = 16;
        config.tile_width = 16;
        config.tile_height = 16;
        config.header_size = 540;
        config.levels = 3;
        let mut sample = Vec::new();
        marker(&mut sample, 0xff01, 52, 0);
        for plane in 0..4 {
            marker(&mut sample, 0xff02, 13, (plane << 28) | 0x0800_0000);
            for band in 0..10 {
                marker(&mut sample, 0xff03, if band >= 7 { 2 } else { 1 }, (band << 28) | 0x0020_0000);
            }
        }
        for _ in 0..4 {
            sample.extend([0x80; 4]); // LL3 and the three 1×1 HF3 bands.
            sample.extend([0xf0; 3]); // Three 2×2 all-zero HF2 bands.
            for _ in 0..3 {
                sample.extend([0xff, 0xf8]); // Three 4×4 all-zero HF1 bands.
            }
        }
        validate(&sample, &config).unwrap();
        assert_eq!(decode(&sample, &config).unwrap(), vec![8192; 256]);
        for end in 0..sample.len() {
            assert!(decode(&sample[..end], &config).is_err());
        }
    }

    #[test]
    fn measured_quantization_and_allocation_limits() {
        for (quant, expected) in [(4, 1), (9, 1), (10, 2), (16, 4), (22, 8), (26, 12), (32, 25), (36, 40), (41, 72), (42, 80), (43, 90)] {
            assert_eq!(quant_step(quant).unwrap(), expected);
        }
        assert!(matches!(quant_step(3), Err(RawError::Unsupported(_))));
        assert!(matches!(quant_step(44), Err(RawError::Unsupported(_))));
        for (width, height) in [(20_000, 20_000), (65_538, 2), (2, 65_538)] {
            let mut config = coding();
            config.width = width;
            config.height = height;
            assert!(matches!(decode(&[], &config), Err(RawError::Limit(_))));
            assert!(matches!(validate(&[], &config), Err(RawError::Limit(_))));
        }
    }

    #[test]
    fn qp_escape_and_average_precede_the_quantization_table() {
        let mut bits = Bits { src: &[0, 0, 1, 0x2c], at: 0 };
        assert_eq!(bits.qp_rice(0).unwrap(), 300);
        assert_eq!(bits.at, 32);
        let map = qp_map(&[0, 0, 1, 0x2c, 0x94], 1, 4).unwrap();
        assert_eq!(map.data, [150; 4]);
        assert!(qp_map(&[0, 0, 1, 0x2c, 0x95], 1, 4).is_err());
        let map = QpMap { width: 1, height: 2, data: vec![150, 153] };
        // floor((150+153)/2)=151; integer step 14, gain4/8 gives7. Averaging
        // the nonlinear steps, or rounding QP upward, incorrectly gives8.
        assert_eq!(adaptive_step(&map, 4, 0, 0, 0, 4).unwrap(), 7);
        assert_eq!(adaptive_step(&map, 7, 0, 0, 1, 4).unwrap(), 7);
        assert_eq!(adaptive_step(&map, 7, 0, 1, 0, 16).unwrap(), 36);
        let low = QpMap { width: 1, height: 2, data: vec![131; 2] };
        assert_eq!(adaptive_step(&low, 4, 0, 0, 1, 1).unwrap(), 1);
        assert_eq!(adaptive_step(&low, 4, 0, 0, 0, 1).unwrap(), 1);
        assert_eq!(adaptive_step(&low, 0, 0, 0, 0, 0).unwrap(), 1);
        assert!(matches!(qp_map(&[0, 0, 1, 4, 0x94], 1, 4), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn adaptive_v200_craw_reconstructs_sensor_samples() {
        let mut config = coding();
        config.version = 0x200;
        config.width = 16;
        config.height = 16;
        config.tile_width = 16;
        config.tile_height = 16;
        config.header_size = 872;
        config.levels = 3;
        let mut sample = Vec::new();
        sample.extend(0xff11u16.to_be_bytes());
        sample.extend(16u16.to_be_bytes());
        sample.extend(60u32.to_be_bytes());
        sample.extend(0x4000u32.to_be_bytes());
        sample.extend(5u32.to_be_bytes());
        sample.extend(3u16.to_be_bytes());
        sample.extend(0u16.to_be_bytes());
        let gains = [0, 0, 0, 0, 4, 4, 8, 8, 8, 16];
        for plane in 0..4 {
            marker(&mut sample, 0xff12, 13, (plane << 28) | 0x0800_0000);
            for (band, &gain) in gains.iter().enumerate() {
                sample.extend(0xff13u16.to_be_bytes());
                sample.extend(16u16.to_be_bytes());
                sample.extend(if band >= 7 { 2u32 } else { 1 }.to_be_bytes());
                sample.extend(((band as u32) << 28 | gain).to_be_bytes());
                sample.extend(u32::from(band < 4).to_be_bytes());
                sample.extend([0u8; 4]);
            }
        }
        sample.extend([0u8; 4]);
        sample.extend([0, 0, 1, 0x2c, 0x94, 0, 0, 0]);
        for _ in 0..4 {
            sample.extend([0x80; 4]);
            sample.extend([0xf0; 3]);
            for _ in 0..3 {
                sample.extend([0xff, 0xf8]);
            }
        }
        validate(&sample, &config).unwrap();
        assert_eq!(decode(&sample, &config).unwrap(), vec![8192; 256]);
        for end in 0..sample.len() {
            assert!(decode(&sample[..end], &config).is_err());
        }
    }

    #[test]
    fn optional_cc0_samples_match_external_oracle() {
        let Some(directory) = std::env::var_os("LIGHTCRAFT_CR3_CORPUS") else { return };
        for (name, expected) in [
            ("cr3-canon-r100-raw.cr3", 0xcbe5299ab9c52630u64),
            ("cr3-canon-m50-raw.cr3", 0x62261f0ba81cfcd2u64),
            ("cr3-canon-r100-craw.cr3", 0x341c706b37c38bbfu64),
            ("cr3-canon-m50-craw.cr3", 0x9aacbfe66f505e1bu64),
            ("cr3-canon-r8-raw.cr3", 0x63c1c6d8d1eb2312u64),
            ("cr3-canon-r8-craw.cr3", 0xf836a663a795a375u64),
        ] {
            let path = std::path::Path::new(&directory).join(name);
            let bytes = std::fs::read(path).unwrap();
            let container = lightcraft_meta::cr3::parse_cr3(&bytes).unwrap();
            let mut candidates = container
                .tracks
                .iter()
                .filter_map(|track| {
                    let lightcraft_meta::cr3::Cr3TrackKind::Raw { cmp1: Some((at, length)), .. } = track.kind else { return None };
                    let coding = Cr3Compression::parse(bytes.get(at..at + length)?)?;
                    Some((u64::from(coding.width) * u64::from(coding.height), coding, track.data?))
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|entry| entry.0);
            let (_, coding, (at, length)) = candidates.last().unwrap();
            let sensor = decode(&bytes[*at..at + length], coding).unwrap();
            let hash = sensor
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
            assert_eq!(hash, expected, "{name}");
        }
    }
}
