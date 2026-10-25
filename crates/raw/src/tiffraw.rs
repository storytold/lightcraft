//! Reading the pixel data of a TIFF IFD (strips or tiles; uncompressed, lossless/lossy JPEG, Deflate, JPEG XL), in parallel.

use crate::unpack::*;
use crate::{MAX_SAMPLES, RawData, RawError, Result, ljpeg};
use lightcraft_tiff::image::{Chunk, ImageInfo, chunk_bytes};
use lightcraft_tiff::{ByteOrder, tags::compression as comp};
use rayon::prelude::*;

/// Bit packing for uncompressed integer data with bit depths other than 8/16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // vendor decoders use the other variants
pub enum Packing {
    /// TIFF standard: MSB-first, rows start on byte boundaries.
    Msb,
    /// LSB-first little-endian bit stream (some vendor raws), rows byte aligned.
    Lsb,
    /// Each sample stored in 16 bits in file byte order, regardless of `bits`.
    Word16,
    /// MSB-first bit stream inside 32-bit words stored in file byte order ([`unpack_words32_msb`]); rows start on a
    /// word boundary.
    Words32Msb,
}

pub(crate) enum ChunkPx {
    U16(Vec<u16>),
    F32(Vec<f32>),
}

/// The checks [`read_image`] makes before decoding anything (sample format, size limits, data
/// present); returns the number of samples.
pub fn check_image(data: &[u8], info: &ImageInfo) -> Result<usize> {
    let (w, h) = (info.width as usize, info.height as usize);
    let cpp = info.samples_per_pixel as usize;
    let total = w.checked_mul(h).and_then(|v| v.checked_mul(cpp)).ok_or(RawError::Limit("image too large"))?;
    if total > MAX_SAMPLES || total == 0 {
        return Err(RawError::Limit("image too large"));
    }
    let bits = info.bits() as u32;
    let float = info.sample_format == 3;
    if float && !matches!(bits, 16 | 24 | 32) {
        return Err(RawError::Unsupported(format!("{bits}-bit floating point samples")));
    }
    if !float && !(1..=16).contains(&bits) {
        return Err(RawError::Unsupported(format!("{bits}-bit integer samples")));
    }
    let chunks = info.chunks(data.len() as u64);
    if chunks.is_empty() {
        return Err(RawError::Corrupt("no image data chunks".into()));
    }
    // every visible sample needs a chunk: a short offsets table would leave zero-filled (black) regions
    let (across, down) = info.grid();
    let planes = if info.planar == 2 { cpp } else { 1 };
    let expected = (across as usize).saturating_mul(down as usize).saturating_mul(planes);
    if chunks.len() < expected {
        return Err(RawError::Corrupt(format!("{} image data chunks, expected {expected}", chunks.len())));
    }
    // Plausibility bound before allocating: no supported coding stores more than ~2000 samples per byte
    // (Deflate of constant data is the extreme), so tiny files cannot trigger huge allocations.
    let available: u64 = chunks.iter().map(|c| chunk_bytes(data, c).map_or(0, |s| s.len() as u64)).sum();
    if available.saturating_mul(2048) < total as u64 {
        return Err(RawError::Corrupt(format!("{available} bytes of image data cannot hold {total} samples")));
    }
    Ok(total)
}

/// Read the samples of `info` in `mode`: [`Mode::Header`] only checks them ([`check_image`]) and
/// returns no samples (floating-point data as an empty `F32`).
pub(crate) fn read_image_in(mode: crate::Mode, data: &[u8], info: &ImageInfo, order: ByteOrder, packing: Packing) -> Result<RawData> {
    match mode {
        crate::Mode::Full => read_image(data, info, order, packing),
        crate::Mode::Header => {
            check_image(data, info)?;
            // a lossless-JPEG layout the decoder rejects (subsampled components, e.g. Sony's
            // lossless M/S sizes) must fail here too, as the full decode will; likewise a JPEG XL
            // chunk whose headers don't match the IFD
            let first = info.chunks(data.len() as u64).first().copied();
            if let Some(c) = first
                && let Some(src) = chunk_bytes(data, &c)
            {
                match info.compression {
                    7 => {
                        ljpeg::frame_info(src)?;
                    }
                    comp::JPEG_XL => {
                        let planar = info.planar == 2 && info.samples_per_pixel > 1;
                        let cpp = if planar { 1 } else { info.samples_per_pixel as usize };
                        let (cw, ch, vw, vh) = chunk_dims(info, &c);
                        jxl_chunk_check(src, cw, ch, vw, vh, cpp, info.bits() as u32, info.sample_format == 3)?;
                    }
                    _ => {}
                }
            }
            Ok(if info.sample_format == 3 { RawData::F32(Vec::new()) } else { RawData::U16(Vec::new()) })
        }
    }
}

/// Decode all chunks of `info` into one buffer of `width × height × cpp` samples.
pub fn read_image(data: &[u8], info: &ImageInfo, order: ByteOrder, packing: Packing) -> Result<RawData> {
    let (w, h) = (info.width as usize, info.height as usize);
    let total = check_image(data, info)?;
    let cpp = info.samples_per_pixel as usize;
    let bits = info.bits() as u32;
    let float = info.sample_format == 3;
    let chunks = info.chunks(data.len() as u64);
    let planar = info.planar == 2 && cpp > 1;
    let ccpp = if planar { 1 } else { cpp };
    // The chunks are decoded and copied into the output in groups whose combined nominal size stays within
    // a few times the output buffer's: everything decoded at once would retain the whole image twice, and
    // a hostile tile table's chosen padding could stretch that much further (issue #1517). One group
    // still decodes in parallel; the grid order groups make, the copies stay disjoint.
    // The tile grid decides how much is decoded, whatever the file's bytes: the nominal sizes of all
    // the chunks stay within a few times the image (edge tiles pad it) before any is decoded.
    let nominal = chunks.iter().fold(0usize, |sum, c| sum.saturating_add(chunk_samples(c, ccpp)));
    if nominal > total.saturating_mul(4).saturating_add(1 << 24) {
        return Err(RawError::Corrupt("the tile grid is much larger than the image".into()));
    }
    let group_samples = (total / 4).max(1 << 20);
    let mut out_u16 = if float { Vec::new() } else { vec![0u16; total] };
    let mut out_f32 = if float { vec![0f32; total] } else { Vec::new() };
    let mut decode_from = 0usize;
    while decode_from < chunks.len() {
        let mut to = decode_from;
        let mut taken = 0usize;
        for (k, c) in chunks[decode_from..].iter().enumerate() {
            let n = chunk_samples(c, ccpp);
            if taken > 0 && taken.saturating_add(n) > group_samples {
                break;
            }
            taken = taken.saturating_add(n);
            to = decode_from + k + 1;
        }
        let decoded: Vec<Result<(Chunk, ChunkPx)>> =
            chunks[decode_from..to].par_iter().map(|c| decode_chunk(data, info, c, order, packing, ccpp, bits, float).map(|p| (*c, p))).collect();
        for r in decoded {
            // a chunk that cannot be decoded is not a partial image: the file stays preview-only
            let (c, px) = r?;
            let (cw, ch) = (c.width as usize, c.height as usize);
            let (x0, y0) = (c.x as usize, c.y as usize);
            if x0 >= w || y0 >= h {
                continue;
            }
            let copy_w = cw.min(w - x0);
            let copy_h = ch.min(h - y0);
            for y in 0..copy_h {
                for x in 0..copy_w {
                    for s in 0..ccpp {
                        let src = (y * cw + x) * ccpp + s;
                        let dst_s = if planar { c.plane as usize } else { s };
                        if dst_s >= cpp {
                            continue;
                        }
                        let dst = ((y0 + y) * w + x0 + x) * cpp + dst_s;
                        match &px {
                            ChunkPx::U16(v) => {
                                if let (Some(o), Some(&s)) = (out_u16.get_mut(dst), v.get(src)) {
                                    *o = s;
                                }
                            }
                            ChunkPx::F32(v) => {
                                if let (Some(o), Some(&s)) = (out_f32.get_mut(dst), v.get(src)) {
                                    *o = s;
                                }
                            }
                        }
                    }
                }
            }
        }
        decode_from = to;
    }
    Ok(if float { RawData::F32(out_f32) } else { RawData::U16(out_u16) })
}

/// Bytes that `n` samples take in a row of an uncompressed or inflated chunk, laid out as
/// [`unpack_chunk`] reads them: whole bytes or 16-bit words, or 32-bit words for `Words32Msb`.
fn packed_row_bytes(n: u64, bits: u32, packing: Packing, float: bool) -> u64 {
    if float {
        return n.saturating_mul(u64::from(bits.div_ceil(8)));
    }
    let bit_len = n.saturating_mul(u64::from(bits));
    match (bits, packing) {
        (8, _) => n,
        (16, _) | (_, Packing::Word16) => n.saturating_mul(2),
        (_, Packing::Msb | Packing::Lsb) => bit_len.div_ceil(8),
        (_, Packing::Words32Msb) => bit_len.div_ceil(32).saturating_mul(4),
    }
}

/// A chunk's nominal decoded size in samples (per plane, as the decoder builds it).
fn chunk_samples(c: &Chunk, ccpp: usize) -> usize {
    (c.width as usize).saturating_mul(c.height as usize).saturating_mul(ccpp)
}

#[allow(clippy::too_many_arguments)]
fn decode_chunk(data: &[u8], info: &ImageInfo, c: &Chunk, order: ByteOrder, packing: Packing, cpp: usize, bits: u32, float: bool) -> Result<ChunkPx> {
    let src = chunk_bytes(data, c).ok_or_else(|| RawError::Corrupt("chunk offset past end of file".into()))?;
    let (cw, ch, vw, vh) = chunk_dims(info, c);
    let n = cw.checked_mul(ch).and_then(|v| v.checked_mul(cpp)).ok_or(RawError::Limit("chunk too large"))?;
    if n > MAX_SAMPLES {
        return Err(RawError::Limit("chunk too large"));
    }
    match info.compression {
        comp::NONE => {
            // uncompressed: missing rows are filled with zeros, but a chunk holding a small fraction of what its
            // samples need is no picture: refused before the nominal chunk is allocated (issue #1512)
            let need = (n as u64).saturating_mul(u64::from(bits)) / 8;
            if (src.len() as u64).saturating_mul(64) < need {
                return Err(RawError::Corrupt(format!("an uncompressed chunk of {n} samples has only {} bytes", src.len())));
            }
            unpack_chunk(src, order, packing, cw, ch, cpp, bits, float, 1)
        }
        comp::JPEG => {
            let f = ljpeg::decode(src, n.saturating_mul(2).max(1 << 16))?;
            if f.data.len() < n {
                // Some writers encode edge tiles smaller than nominal: accept a frame that covers the rows inside the image.
                if f.data.is_empty() || f.data.len() % (cw * cpp) != 0 || f.data.len() / (cw * cpp) < vh {
                    return Err(RawError::Corrupt(format!("lossless JPEG tile has {} samples, expected {n}", f.data.len())));
                }
            }
            let mut v = f.data;
            v.resize(n, 0);
            Ok(ChunkPx::U16(v))
        }
        comp::LOSSY_JPEG => {
            // lossy DNG: each chunk is a baseline (DCT) JPEG of 8-bit samples
            let (px, jw, jh, jc) = lossy_jpeg(src, cpp, MAX_JPEG_SAMPLES)?;
            // the buffer holds the rows the decoded JPEG covers (the caller zero-fills the tile's
            // remainder and reads only rows it fills): a tile-many-larger decoded JPEG does not
            // get the nominal tile's full buffer (issue #1517)
            let rows = ch.min(jh);
            let mut v = vec![0u16; rows.max(1).saturating_mul(cw).saturating_mul(cpp)];
            for y in 0..rows {
                for x in 0..cw.min(jw) {
                    for k in 0..cpp {
                        v[(y * cw + x) * cpp + k] = px[(y * jw + x) * jc + k.min(jc - 1)] as u16;
                    }
                }
            }
            Ok(ChunkPx::U16(v))
        }
        comp::ADOBE_DEFLATE | comp::DEFLATE => {
            // row bytes in u64: a wrapping `cw · cpp · …` in usize could come out small on 32-bit targets and slip past the gate
            let row_bytes = packed_row_bytes(cw as u64 * cpp as u64, bits, packing, float);
            // the gate caps at MAX_SAMPLES (≤ 2^30), so the `usize` cast below is lossless on 32-bit targets too
            let inflate_len =
                crate::checked_pixel_count(row_bytes, ch as u64).ok_or_else(|| RawError::Corrupt("bad TIFF chunk dimensions".into()))?;
            // Deflate compresses at most about 1:1032, so a source this far under half of its
            // nominal length cannot reconstruct the chunk: refused before the nominal buffer is
            // inflated and predicted, which padding-only work would make gigabytes.
            if (src.len() as u64).saturating_mul(2048) < inflate_len {
                return Err(RawError::Corrupt(format!("a deflated chunk of {inflate_len} bytes is packed into only {} bytes", src.len())));
            }
            let raw = inflate(src, inflate_len as usize)?;
            // The stream has to reach the last row inside the image (edge strips may stop there);
            // it is laid out in nominal rows, so shorter is a truncated chunk, not a picture.
            let last = packed_row_bytes(vw as u64 * cpp as u64, bits, packing, float);
            let visible = row_bytes.saturating_mul(vh.saturating_sub(1) as u64).saturating_add(last);
            if (raw.len() as u64) < visible {
                return Err(RawError::Corrupt(format!(
                    "a deflated chunk holds {} bytes, fewer than the {visible} bytes its in-image samples span",
                    raw.len()
                )));
            }
            unpack_chunk(&raw, order, packing, cw, ch, cpp, bits, float, info.predictor)
        }
        comp::JPEG_XL => jxl_chunk(src, cw, ch, vw, vh, cpp, bits, float),
        other => Err(RawError::Unsupported(format!("TIFF compression {other}"))),
    }
}

/// A chunk's nominal size and the part of it inside the image (smaller for edge tiles).
fn chunk_dims(info: &ImageInfo, c: &Chunk) -> (usize, usize, usize, usize) {
    let (cw, ch) = (c.width as usize, c.height as usize);
    let vw = cw.min(info.width.saturating_sub(c.x) as usize);
    let vh = ch.min(info.height.saturating_sub(c.y) as usize);
    (cw, ch, vw, vh)
}

#[cfg(feature = "jxl")]
use crate::jxl::{check as jxl_chunk_check, decode as jxl_chunk};

#[cfg(not(feature = "jxl"))]
#[allow(clippy::too_many_arguments)]
fn jxl_chunk(_: &[u8], _: usize, _: usize, _: usize, _: usize, _: usize, _: u32, _: bool) -> Result<ChunkPx> {
    Err(RawError::Unsupported("JPEG XL DNG (built without the `jxl` feature)".into()))
}

#[cfg(not(feature = "jxl"))]
#[allow(clippy::too_many_arguments)]
fn jxl_chunk_check(src: &[u8], cw: usize, ch: usize, vw: usize, vh: usize, cpp: usize, bits: u32, float: bool) -> Result<()> {
    jxl_chunk(src, cw, ch, vw, vh, cpp, bits, float).map(|_| ())
}

/// The most samples a tiled JPEG chunk or a white-balance preview may decode to: far more than
/// any real lossy tile or preview carries, and small enough that a JPEG header alone cannot size
/// a multi-gigabyte allocation.
pub(crate) const MAX_JPEG_SAMPLES: usize = 1 << 26;

/// Decode one baseline JPEG chunk to 8-bit samples: (samples, width, height, channels).
pub(crate) fn lossy_jpeg(src: &[u8], cpp: usize, max_samples: usize) -> Result<(Vec<u8>, usize, usize, usize)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let cs = if cpp == 1 { ColorSpace::Luma } else { ColorSpace::RGB };
    let opts = DecoderOptions::default().set_max_width(1 << 16).set_max_height(1 << 16).set_strict_mode(false).jpeg_set_out_colorspace(cs);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(src), opts);
    // The header is read first: the JPEG's own dimensions, not the enclosing tile's, size every
    // allocation `decode` makes, and they may claim billions of pixels.
    d.decode_headers().map_err(|e| RawError::Corrupt(format!("lossy JPEG tile: {e}")))?;
    let info = d.info().ok_or_else(|| RawError::Corrupt("lossy JPEG tile without a header".into()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let c = if cpp == 1 { 1 } else { 3 };
    let out = w as u64 * h as u64 * c as u64;
    if w == 0 || h == 0 || out > max_samples as u64 {
        return Err(RawError::Corrupt(format!("a lossy JPEG of {w} × {h} × {c} would exceed the {max_samples} output samples allowed")));
    }
    let px = d.decode().map_err(|e| RawError::Corrupt(format!("lossy JPEG tile: {e}")))?;
    if px.len() < w * h * c {
        return Err(RawError::Corrupt("lossy JPEG tile: short output".into()));
    }
    Ok((px, w, h, c))
}

#[allow(clippy::too_many_arguments)]
fn unpack_chunk(
    src: &[u8],
    order: ByteOrder,
    packing: Packing,
    cw: usize,
    ch: usize,
    cpp: usize,
    bits: u32,
    float: bool,
    predictor: u16,
) -> Result<ChunkPx> {
    let row_n = cw * cpp;
    let (factor, fp) = match predictor {
        1 => (0, false),
        2 => (1, false),
        3 => (1, true),
        34892 => (2, false),
        34893 => (4, false),
        34894 => (2, true),
        34895 => (4, true),
        p => return Err(RawError::Unsupported(format!("predictor {p}"))),
    };
    if float {
        let bp = bits.div_ceil(8) as usize;
        let row_bytes = row_n * bp;
        let mut out = vec![0f32; row_n * ch];
        let mut rowbuf = vec![0u8; row_bytes];
        for y in 0..ch {
            let s = src.get(y * row_bytes..).unwrap_or(&[]);
            let len = s.len().min(row_bytes);
            rowbuf[..len].copy_from_slice(&s[..len]);
            rowbuf[len..].fill(0);
            let big = if fp {
                undo_float_predictor(&mut rowbuf, row_n, bp, cpp * factor);
                true
            } else {
                if factor > 0 {
                    return Err(RawError::Unsupported("integer predictor on float data".into()));
                }
                order == ByteOrder::Big
            };
            for (i, o) in out[y * row_n..(y + 1) * row_n].iter_mut().enumerate() {
                let b = &rowbuf[i * bp..(i + 1) * bp];
                *o = match (bp, big) {
                    (2, true) => f16_to_f32(u16::from_be_bytes([b[0], b[1]])),
                    (2, false) => f16_to_f32(u16::from_le_bytes([b[0], b[1]])),
                    (3, true) => f24_to_f32(u32::from_be_bytes([0, b[0], b[1], b[2]])),
                    (3, false) => f24_to_f32(u32::from_le_bytes([b[0], b[1], b[2], 0])),
                    (4, true) => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                    _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
            }
        }
        return Ok(ChunkPx::F32(out));
    }
    if fp {
        return Err(RawError::Unsupported("floating-point predictor on integer data".into()));
    }
    let mut out = vec![0u16; row_n * ch];
    let (row_bytes, kind) = match (bits, packing) {
        (8, _) => (row_n, 0),
        (16, _) | (_, Packing::Word16) => (row_n * 2, 1),
        (_, Packing::Msb) => ((row_n * bits as usize).div_ceil(8), 2),
        (_, Packing::Lsb) => ((row_n * bits as usize).div_ceil(8), 3),
        (_, Packing::Words32Msb) => ((row_n * bits as usize).div_ceil(32) * 4, 4),
    };
    for y in 0..ch {
        let s = src.get(y * row_bytes..).unwrap_or(&[]);
        let s = &s[..s.len().min(row_bytes)];
        let row = &mut out[y * row_n..(y + 1) * row_n];
        match kind {
            0 => row.iter_mut().zip(s).for_each(|(o, &b)| *o = b as u16),
            1 => read_u16s(s, order, row),
            2 => unpack_msb(s, bits, row),
            4 => unpack_words32_msb(s, order, bits, row),
            _ => unpack_lsb(s, bits, row),
        }
        if factor > 0 {
            if bits == 8 {
                for i in cpp * factor..row.len() {
                    row[i] = (row[i] as u8).wrapping_add(row[i - cpp * factor] as u8) as u16;
                }
            } else {
                undo_diff_u16(row, cpp * factor);
            }
        }
    }
    Ok(ChunkPx::U16(out))
}

#[cfg(test)]
mod lossy_tests {
    use super::*;
    use lightcraft_tiff::image::Layout;

    /// A lossy-DNG-style image: two 16×8 tiles, each a baseline JPEG, the right one cut short
    /// by the image edge.
    #[test]
    fn lossy_jpeg_tiles_decode() {
        let (tw, th) = (16usize, 8usize);
        let tile = |shade: u8| {
            let px: Vec<u8> = (0..tw * th).flat_map(|i| [shade, (i % tw * 15) as u8, 200]).collect();
            let mut out = Vec::new();
            jpeg_encoder::Encoder::new(&mut out, 100).encode(&px, tw as u16, th as u16, jpeg_encoder::ColorType::Rgb).unwrap();
            out
        };
        let (a, b) = (tile(40), tile(220));
        let mut file = vec![0u8; 8];
        let oa = file.len() as u64;
        file.extend_from_slice(&a);
        let ob = file.len() as u64;
        file.extend_from_slice(&b);
        let info = ImageInfo {
            width: 24,
            height: 8,
            bits_per_sample: vec![8, 8, 8],
            samples_per_pixel: 3,
            compression: comp::LOSSY_JPEG,
            photometric: 34892,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Tiles { tile_width: tw as u32, tile_height: th as u32 },
            offsets: vec![oa, ob],
            byte_counts: vec![a.len() as u64, b.len() as u64],
        };
        let RawData::U16(v) = read_image(&file, &info, ByteOrder::Little, Packing::Msb).unwrap() else { panic!("integer samples") };
        assert_eq!(v.len(), 24 * 8 * 3);
        let px = |x: usize, y: usize| &v[(y * 24 + x) * 3..(y * 24 + x) * 3 + 3];
        assert!((px(2, 3)[0] as i32 - 40).abs() <= 3 && (px(2, 3)[2] as i32 - 200).abs() <= 3, "{:?}", px(2, 3));
        assert!((px(20, 3)[0] as i32 - 220).abs() <= 3, "second tile: {:?}", px(20, 3));
        assert!((px(10, 5)[1] as i32 - 150).abs() <= 6, "green ramp: {:?}", px(10, 5));
    }

    fn strip_info(compression: u16, offsets: Vec<u64>, byte_counts: Vec<u64>) -> ImageInfo {
        ImageInfo {
            width: 4,
            height: 4,
            bits_per_sample: vec![8],
            samples_per_pixel: 1,
            compression,
            photometric: 1,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Strips { rows_per_strip: 2 },
            offsets,
            byte_counts,
        }
    }

    /// A missing strip or a strip that fails to decode is an error, not a black band in a "successful" image.
    #[test]
    fn missing_or_failed_chunks_are_errors() {
        let file = vec![9u8; 8 + 8];
        // both strips present
        let ok = strip_info(comp::NONE, vec![0, 8], vec![8, 8]);
        assert!(read_image(&file, &ok, ByteOrder::Little, Packing::Msb).is_ok());
        // the second strip has no offset
        let missing = strip_info(comp::NONE, vec![0], vec![8]);
        assert!(read_image(&file, &missing, ByteOrder::Little, Packing::Msb).is_err());
        // the header-only read rejects it too, as the full decode will
        assert!(read_image_in(crate::Mode::Header, &file, &missing, ByteOrder::Little, Packing::Msb).is_err());
        // the second strip starts past the end of the file
        let past_end = strip_info(comp::NONE, vec![0, 100], vec![8, 8]);
        assert!(read_image(&file, &past_end, ByteOrder::Little, Packing::Msb).is_err());
    }

    /// A small image whose uncompressed tile claims to be enormous (16384 x 16384 nominal, a few bytes of data) is
    /// refused before that tile is allocated (issue #1512); an edge tile stored at its full nominal size decodes.
    #[test]
    fn a_huge_nominal_uncompressed_tile_with_little_data_is_refused() {
        let info = |tile: u32, bytes: u64| ImageInfo {
            width: 8,
            height: 8,
            bits_per_sample: vec![16],
            samples_per_pixel: 1,
            compression: comp::NONE,
            photometric: 32803,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Tiles { tile_width: tile, tile_height: tile },
            offsets: vec![0],
            byte_counts: vec![bytes],
        };
        let small = vec![1u8; 2 * 16 * 16];
        assert!(read_image(&small, &info(16, small.len() as u64), ByteOrder::Little, Packing::Msb).is_ok());
        let data = vec![1u8; 4096];
        assert!(matches!(read_image(&data, &info(16384, data.len() as u64), ByteOrder::Little, Packing::Msb), Err(RawError::Corrupt(_))));
    }

    /// A lossless-JPEG tile may be coded only as tall as the part inside the image, never shorter.
    #[test]
    fn short_lossless_jpeg_tiles_are_errors() {
        let tile = |rows: usize| ljpeg::encode(&vec![300u16; 8 * rows], 8, rows, 1, 12, 1, 0);
        let decode = |height: u32, rows: usize| {
            let t = tile(rows);
            let info = ImageInfo {
                width: 8,
                height,
                bits_per_sample: vec![12],
                samples_per_pixel: 1,
                compression: comp::JPEG,
                photometric: 32803,
                planar: 1,
                predictor: 1,
                sample_format: 1,
                new_subfile_type: 0,
                layout: Layout::Tiles { tile_width: 8, tile_height: 8 },
                offsets: vec![0],
                byte_counts: vec![t.len() as u64],
            };
            read_image(&t, &info, ByteOrder::Little, Packing::Msb)
        };
        // an edge tile coded at its in-image height, and a full tile
        assert_eq!(decode(6, 6), Ok(RawData::U16(vec![300; 8 * 6])));
        assert_eq!(decode(8, 8), Ok(RawData::U16(vec![300; 8 * 8])));
        // half of the visible rows are missing
        assert!(decode(8, 4).is_err());
    }

    /// A lossy JPEG whose header alone claims more samples than any real tile or preview (here
    /// 40000 × 40000 × 3) is refused from the header, before the decoder allocates its output
    /// from the JPEG's own dimensions.
    #[test]
    fn an_oversized_lossy_jpeg_header_is_refused_before_decoding() {
        // A real 8 × 8 baseline JPEG, whose frame header then claims other dimensions: the decoder
        // parses the header, and the size check is what refuses it.
        let px = vec![128u8; 8 * 8 * 3];
        let mut j = Vec::new();
        jpeg_encoder::Encoder::new(&mut j, 90).encode(&px, 8, 8, jpeg_encoder::ColorType::Rgb).unwrap();
        // SOF0: marker, length, precision, then the height and the width
        let sof = j.windows(2).position(|w| w == [0xff, 0xc0]).expect("a baseline frame header");
        let claim = |j: &mut Vec<u8>, size: u16| {
            j[sof + 5..sof + 7].copy_from_slice(&size.to_be_bytes());
            j[sof + 7..sof + 9].copy_from_slice(&size.to_be_bytes());
        };
        claim(&mut j, 40_000);
        let RawError::Corrupt(refused) = lossy_jpeg(&j, 3, MAX_JPEG_SAMPLES).unwrap_err() else { panic!("a corrupt-data error") };
        assert!(refused.contains("would exceed"), "{refused}");
        // a header inside the budget passes the size check (and then fails to decode only for lack of data)
        claim(&mut j, 1024);
        match lossy_jpeg(&j, 3, MAX_JPEG_SAMPLES) {
            Ok(_) => {}
            Err(RawError::Corrupt(m)) => assert!(!m.contains("would exceed"), "the smaller header was refused for its size: {m}"),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    /// A lossy tile whose JPEG decodes far less than the nominal tile is fine: the rows the JPEG
    /// covers hold its pixels, the rest of the output stays zero (and the tile's intermediate buffer
    /// is sized by the JPEG, not by the nominal tile).
    #[test]
    fn a_small_lossy_jpeg_in_a_large_nominal_tile_decodes_with_zeros_below() {
        let px: Vec<u8> = (0..8 * 8).flat_map(|_| [30u8, 90, 150]).collect();
        let mut j = Vec::new();
        jpeg_encoder::Encoder::new(&mut j, 100).encode(&px, 8, 8, jpeg_encoder::ColorType::Rgb).unwrap();
        let info = ImageInfo {
            width: 16,
            height: 16,
            bits_per_sample: vec![8, 8, 8],
            samples_per_pixel: 3,
            compression: comp::LOSSY_JPEG,
            photometric: 34892,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Tiles { tile_width: 16, tile_height: 16 },
            offsets: vec![0],
            byte_counts: vec![j.len() as u64],
        };
        let RawData::U16(v) = read_image(&j, &info, ByteOrder::Little, Packing::Msb).unwrap() else { panic!("integer samples") };
        assert_eq!(v.len(), 16 * 16 * 3, "the output is the full nominal image");
        let px = |x: usize, y: usize| &v[(y * 16 + x) * 3..(y * 16 + x) * 3 + 3];
        assert!((px(3, 3)[0] as i32 - 30).abs() <= 3 && (px(3, 3)[2] as i32 - 150).abs() <= 3, "covered rows: {:?}", px(3, 3));
        assert!(px(3, 12).iter().all(|&s| s == 0), "rows below the JPEG stay zero: {:?}", px(3, 12));
    }

    /// A deflated tile is refused when its packed bytes are too few for deflate to have produced
    /// the nominal chunk, or when the stream stops short of the rows inside the image; a fully
    /// covered flat tile still decodes.
    #[test]
    fn a_short_or_undercovering_deflated_tile_is_refused() {
        let info = |byte_counts: u64| ImageInfo {
            width: 512,
            height: 512,
            bits_per_sample: vec![16],
            samples_per_pixel: 1,
            compression: comp::ADOBE_DEFLATE,
            photometric: 32803,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Tiles { tile_width: 512, tile_height: 512 },
            offsets: vec![0],
            byte_counts: vec![byte_counts],
        };
        // a complete flat tile: 512 KiB of zeros compresses to a few hundred bytes (well within
        // deflate's packing bound) and inflates to every nominal byte the tile claims
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 512 * 512 * 2], 6);
        assert_eq!(read_image(&zlib, &info(zlib.len() as u64), ByteOrder::Little, Packing::Msb), Ok(RawData::U16(vec![0; 512 * 512])));
        // the same 1 KiB packed, but the stream claims a whole tile: a few hundred bytes cannot
        // reconstruct 512 KiB from a decoder that compresses no further than about 1:1032
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 1024], 6);
        assert!(matches!(read_image(&zlib, &info(512 * 512 * 2), ByteOrder::Little, Packing::Msb), Err(RawError::Corrupt(_))), "{zlib:?}");
        // 500 KiB of zeros compress to a stream within the packing bound and inflating to more
        // bytes than the in-image samples span, but 24 KiB short of the whole nominal tile:
        // refused as truncated rather than filled with the predictor's padding
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 500 * 1000], 6);
        assert!(matches!(read_image(&zlib, &info(zlib.len() as u64), ByteOrder::Little, Packing::Msb), Err(RawError::Corrupt(_))));
    }
}
