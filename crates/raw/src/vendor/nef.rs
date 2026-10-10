//! Nikon NEF / NRW — uncompressed and Huffman-compressed (lossless, lossy) variants.
//!
//! Sources: TIFF 6.0 (the raw image is a standard CFA SubIFD), Laurent Clévy's NEF structure notes (prose: IFD
//! layout, SubIFDs, maker note header) and the ExifTool Nikon tag-name documentation (`0x000c` WB_RBLevels,
//! `0x003d` BlackLevel, `0x0096` NEFLinearizationTable). The Huffman-compressed data (compression 34713) is decoded
//! by [`super::nefc`], which documents its clean-room sources; "lossy after split" files are decoded when their strip follows the
//! rule documented there; files it can't decode are reported as [`RawError::Unsupported`] and their embedded previews still work. So are High Efficiency NEFs
//! (HE / HE★, Z 8, Z 9, Z 6III, Z f): they keep compression 34713 but carry a wavelet codestream that starts with
//! the JPEG XS markers SOC + CAP (ISO/IEC 21122-1, `FF10 FF50`) and have no `0x0096` table (issue #193).
//! Strips labelled 34713 without a `0x0096`
//! linearization table are packed uncompressed data in several measured layouts ([`PackedLayout`]) or, when the strip
//! is exactly three bytes per pixel, Nikon small raw ([`small_raw`]); the D1X's genuinely Huffman-coded strip stays
//! refused.

use super::{nefc, white_from_data};
use crate::tiffraw::{Packing, read_image};
use crate::{BlackLevel, Cfa, ColorData, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_geom::Orientation;
use lightcraft_tiff::image::ImageInfo;
use lightcraft_tiff::tags::{self as t, photometric};
use lightcraft_tiff::{ByteOrder, Ifd, Tiff, makernote};

const WB_RB_LEVELS: u16 = 0x000c;
const BLACK_LEVEL: u16 = 0x003d;
const LINEARIZATION_TABLE: u16 = 0x0096;
const CROP_AREA: u16 = 0x0045;
/// JPEG XS start of codestream (SOC, `FF10`) followed by its mandatory capabilities marker (CAP, `FF50`).
const JPEG_XS_START: [u8; 4] = [0xff, 0x10, 0xff, 0x50];
/// Why a High Efficiency NEF shows its embedded preview: the UI shows the part before " (".
pub(crate) const HIGH_EFFICIENCY: &str =
    "Nikon High Efficiency NEF (HE / HE★, a JPEG XS-based codec with no public specification; shoot RAW Lossless compressed to edit the raw data)";
/// Maker note `0x0014`, the "NRW" data block of Coolpix raws: `"NRW "` + a 4-character version, then fields. In
/// version `0104` the `u32` at byte `0x20` is the black level in sample units (200 in the four 12-bit files here
/// that have 200 as their darkest samples, 0 in the one whose samples reach 0).
const NRW_DATA: u16 = 0x0014;
const NRW_BLACK_AT: usize = 0x20;

/// The NRW data block, when the maker note has one.
fn nrw_data(mn: Option<&makernote::MakerNote>) -> Option<(&[u8], ByteOrder)> {
    let m = mn?;
    m.ifd.bytes(NRW_DATA).filter(|b| b.starts_with(b"NRW ")).map(|b| (b, m.order))
}

/// Black level from the NRW data block, bounded to a quarter of the sample range so an unknown version's field
/// can't turn into a large offset.
fn nrw_black(mn: Option<&makernote::MakerNote>, bits: u32) -> Option<f32> {
    let (b, order) = nrw_data(mn)?;
    let v = order.u32(b.get(NRW_BLACK_AT..NRW_BLACK_AT + 4)?.try_into().ok()?);
    (u64::from(v) < (1u64 << bits.min(16)) / 4).then_some(v as f32)
}

/// Nikon CropArea is `[left, top, width, height]` in sensor pixels (maker-note tag documentation).
/// Keep the active area/CFA origin unchanged: the default crop is applied after demosaicing.
fn default_crop(mn: Option<&makernote::MakerNote>, w: usize, h: usize) -> Rect {
    let fallback = Rect::new(0, 0, w, h);
    let Some(values) = mn.and_then(|m| m.ifd.u64s(CROP_AREA)) else { return fallback };
    let [x, y, width, height] = values.as_slice() else { return fallback };
    if *width == 0 || *height == 0 || x.checked_add(*width).is_none_or(|v| v > w as u64) || y.checked_add(*height).is_none_or(|v| v > h as u64) {
        return fallback;
    }
    Rect::new(*x as usize, *y as usize, *width as usize, *height as usize)
}

fn raw_ifd(tiff: &Tiff) -> Option<&Ifd> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u16(t::PHOTOMETRIC) == Some(photometric::CFA))
        .max_by_key(|i| i.u64(t::IMAGE_WIDTH).unwrap_or(0).saturating_mul(i.u64(t::IMAGE_LENGTH).unwrap_or(0)))
}

/// Width without the optically masked columns some bodies append on the right: trailing columns (at most 64)
/// whose mean is below 1% of the white level while the image interior is brighter. Kept even for CFA phase.
pub(crate) fn trailing_masked_columns(d: &[u16], w: usize, h: usize, white: f32) -> usize {
    if w < 128 || h == 0 {
        return w;
    }
    let step = (h / 256).max(1);
    let col_mean = |x: usize| (0..h).step_by(step).map(|y| d[y * w + x] as f64).sum::<f64>() / h.div_ceil(step) as f64;
    let interior = (w / 4..w * 3 / 4).step_by(w / 64).map(col_mean).sum::<f64>() / 32.0;
    let dark = (white as f64 * 0.01).min(interior * 0.1);
    let mut aw = w;
    while aw > w - 64 && col_mean(aw - 1) < dark {
        aw -= 1;
    }
    aw & !1
}

pub(crate) fn decode(bytes: &[u8]) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = &tiff.ifds[0];
    let raw = raw_ifd(&tiff).ok_or_else(|| RawError::Unsupported("NEF without a CFA image IFD".into()))?;
    let info = raw.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let bits = info.bits() as u32;
    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let mn =
        tiff.exif().and_then(|e| e.get(t::MAKER_NOTE)).and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make));
    if let Some(strip) = small_raw_strip(&info, bytes.len(), mn.as_ref()) {
        return small_raw(bytes, &tiff, &info, strip);
    }
    let data = if info.compression == t::compression::NIKON {
        compressed(bytes, &info, mn.as_ref())?
    } else {
        uncompressed(bytes, &info, tiff.order, mn.as_ref())?
    };
    let RawData::U16(ref samples) = data else { return Err(RawError::Unsupported("float NEF".into())) };

    // Maker note 0x003d is in 14-bit units whatever the sample depth: 12-bit files from the D750, D780, D850,
    // D7500 and Z 50 store 600 / 1008 / 400 / 400 / 1008 while their darkest samples are 150 / ~252 / 99 / 99 / ~250
    // (our measurements on CC0 raw.pixls.us samples; the same bodies' 14-bit files match the tag as is).
    let black_scale = if bits == 12 { 0.25 } else { 1.0 };
    let black = match mn.as_ref().and_then(|m| m.ifd.f64s(BLACK_LEVEL)).as_deref() {
        Some([a, b, c, d]) if [a, b, c, d].iter().all(|v| **v < 16384.0) => BlackLevel {
            repeat_rows: 2,
            repeat_cols: 2,
            values: [a, b, c, d].iter().map(|v| (**v * black_scale) as f32).collect(),
            ..Default::default()
        },
        _ => BlackLevel::uniform(nrw_black(mn.as_ref(), bits).unwrap_or(0.0)),
    };
    let wb = mn
        .as_ref()
        .and_then(|m| m.ifd.f64s(WB_RB_LEVELS))
        .filter(|v| v.len() >= 2 && v[0] > 0.1 && v[1] > 0.1 && v[0] < 10.0 && v[1] < 10.0)
        .map(|v| [v[0] as f32, 1.0, v[1] as f32]);
    let cfa = match (raw.u64s(t::CFA_REPEAT_PATTERN_DIM).as_deref(), raw.bytes(t::CFA_PATTERN_EP)) {
        (Some([2, 2]), Some(p)) if p.len() == 4 && p.iter().all(|&c| c <= 2) => Cfa { width: 2, height: 2, pattern: p.to_vec() },
        _ => Cfa::bayer_static("RGGB"),
    };
    let white = white_from_data(samples, bits);
    let active_w = trailing_masked_columns(samples, w, h, white);
    let crop = default_crop(mn.as_ref(), active_w, h);
    let mut metadata = lightcraft_meta::from_tiff(&tiff);
    metadata.width = Some(crop.width as u32);
    metadata.height = Some(crop.height as u32);
    let img = RawImage {
        format: RawFormat::Nef,
        width: w,
        height: h,
        cpp: 1,
        data,
        cfa: Some(cfa),
        bits,
        black,
        white: vec![white],
        active_area: Rect::new(0, 0, active_w, h),
        crop,
        orientation: Orientation::from_exif(ifd0.u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData::default(),
        wb_multipliers: wb,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
    };
    img.validate()?;
    Ok(img)
}

/// Uncompressed strips: 16-bit words or 12-bit packed, told apart by the strip size. Packed 12-bit data is an MSB-first
/// byte stream, except in files with an NRW data block (Coolpix), where it is MSB-first inside little-endian 32-bit
/// words (the rows are whole words).
fn uncompressed(bytes: &[u8], info: &ImageInfo, order: ByteOrder, mn: Option<&makernote::MakerNote>) -> Result<RawData> {
    let (w, h) = (info.width as usize, info.height as usize);
    let bits = info.bits() as u32;
    let row_samples = w * info.samples_per_pixel as usize;
    let chunks = info.chunks(bytes.len() as u64);
    let rows_in_first = chunks.first().map(|c| c.height as usize).unwrap_or(h).max(1);
    let bytes_per_row = chunks.first().map(|c| c.len as usize / rows_in_first).unwrap_or(0);
    let packing = if bytes_per_row >= row_samples * 2 {
        Packing::Word16
    } else if bits == 12 && bytes_per_row * 8 >= row_samples * 12 && bytes_per_row * 8 < row_samples * 13 {
        if nrw_data(mn).is_some() && bytes_per_row.is_multiple_of(4) { Packing::Words32Msb } else { Packing::Msb }
    } else if info.compression == 1 && bits != 8 && bits != 16 {
        return Err(RawError::Unsupported(format!("NEF uncompressed packing ({bytes_per_row} bytes per {row_samples}-sample row)")));
    } else {
        Packing::Msb
    };
    read_image(bytes, info, order, packing)
}

/// Packed layouts of strips that are labelled compression 34713 but have no linearization table (maker note
/// `0x0096`): they hold plain packed samples, not Huffman codes. The layouts were measured from CC0 raw.pixls.us
/// files by a clean-room black-box analysis (strip byte-count identity, same-colour smoothness and correlation with
/// the camera JPEG against at least eight alternative packings); the tool and log are under
/// `data/testing/flash-nef-packed` (not committed). Every layout is a single strip with
/// `stride = StripByteCounts / height`, selected only when `stride * height` equals the strip size exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackedLayout {
    /// LSB-first bit stream of `bits` per sample (12: 3 bytes per 2 samples, 14: 7 bytes per 4), rows byte aligned.
    /// 12-bit rows are `3w/2` bytes (older bodies) or padded to 16 bytes (Z 6, D6); 14-bit rows are `7w/4` bytes
    /// padded to 16 bytes.
    Lsb { stride: usize },
    /// D100: 16-byte chunks of fifteen data bytes holding ten MSB-first 12-bit samples, plus one zero pad byte. The
    /// rows carry more samples than the IFD width (3040 against 3034); the extra columns are not image.
    Chunked16 { stride: usize },
}

fn packed_layout(w: usize, h: usize, bits: u32, strip: u64) -> Option<PackedLayout> {
    if w == 0 || h == 0 || !matches!(bits, 12 | 14) {
        return None;
    }
    let strip = usize::try_from(strip).ok()?;
    if !strip.is_multiple_of(h) {
        return None;
    }
    let stride = strip / h;
    let tight = (w * bits as usize).div_ceil(8);
    let tight = if bits == 12 && w.is_multiple_of(2) || bits == 14 && w.is_multiple_of(4) { tight } else { return None };
    let padded = tight.next_multiple_of(16);
    let lsb_ok = match bits {
        12 => stride == tight || stride == padded,
        _ => stride == padded,
    };
    if lsb_ok {
        return Some(PackedLayout::Lsb { stride });
    }
    None
}

fn d100_layout(w: usize, h: usize, bits: u32, strip: u64) -> Option<PackedLayout> {
    let strip = usize::try_from(strip).ok()?;
    if bits != 12 || h == 0 || !strip.is_multiple_of(h) {
        return None;
    }
    let stride = strip / h;
    let per_row = stride / 16 * 10;
    (stride.is_multiple_of(16) && per_row >= w && per_row < w + 10).then_some(PackedLayout::Chunked16 { stride })
}

/// Unpack a [`PackedLayout`] strip into `w * h` samples. Short input leaves the missing samples zero.
fn unpack_packed(src: &[u8], w: usize, h: usize, bits: u32, layout: PackedLayout) -> Vec<u16> {
    use rayon::prelude::*;
    let mut out = vec![0u16; w * h];
    let stride = match layout {
        PackedLayout::Lsb { stride } | PackedLayout::Chunked16 { stride } => stride,
    };
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let Some(line) = src.get(y * stride..) else { return };
        let line = &line[..stride.min(line.len())];
        match layout {
            PackedLayout::Lsb { .. } => crate::unpack::unpack_lsb(line, bits, row),
            PackedLayout::Chunked16 { .. } => {
                let mut samples = vec![0u16; line.len() / 16 * 10];
                for (i, dst) in samples.chunks_mut(10).enumerate() {
                    if let Some(chunk) = line.get(i * 16..i * 16 + 15) {
                        crate::unpack::unpack_msb(chunk, 12, dst);
                    }
                }
                let n = row.len().min(samples.len());
                row[..n].copy_from_slice(&samples[..n]);
            }
        }
    });
    out
}

/// Nikon Huffman-compressed strip (see [`super::nefc`]); the decode table is maker note `0x0096`. Without a table the
/// strip may be packed uncompressed data (see [`PackedLayout`]).
fn compressed(bytes: &[u8], info: &ImageInfo, mn: Option<&makernote::MakerNote>) -> Result<RawData> {
    let starts_with = |magic: &[u8]| {
        let start = info.chunks(bytes.len() as u64).first().and_then(|c| usize::try_from(c.offset).ok());
        start.and_then(|o| bytes.get(o..o.checked_add(magic.len())?)) == Some(magic)
    };
    if starts_with(&JPEG_XS_START) {
        return Err(RawError::Unsupported(HIGH_EFFICIENCY.into()));
    }
    let Some(table) = mn.and_then(|m| Some((m.ifd.bytes(LINEARIZATION_TABLE)?, m.order))) else {
        return packed(bytes, info);
    };
    if info.samples_per_pixel != 1 {
        return Err(RawError::Unsupported(format!("Nikon compressed NEF with {} samples per pixel", info.samples_per_pixel)));
    }
    let bits = info.bits() as u32;
    let table = nefc::parse_table(table.0, table.1, bits)?;
    // one strip in every sample; several would be contiguous parts of the same bit stream
    let chunks = info.chunks(bytes.len() as u64);
    let (Some(first), Some(last)) = (chunks.first(), chunks.last()) else { return Err(RawError::Corrupt("NEF: no image data".into())) };
    let end = last.offset.saturating_add(last.len).min(bytes.len() as u64);
    let src = usize::try_from(first.offset).ok().zip(usize::try_from(end).ok()).and_then(|(a, b)| bytes.get(a..b));
    let src = src.ok_or_else(|| RawError::Corrupt("NEF: image data outside the file".into()))?;
    Ok(RawData::U16(nefc::decode(src, info.width as usize, info.height as usize, bits, &table)?))
}

/// A table-less "compressed" strip: packed samples when the strip size matches a measured layout exactly.
fn packed(bytes: &[u8], info: &ImageInfo) -> Result<RawData> {
    let (w, h) = (info.width as usize, info.height as usize);
    let bits = info.bits() as u32;
    let chunks = info.chunks(bytes.len() as u64);
    let layout = match chunks.as_slice() {
        [c] if info.samples_per_pixel == 1 => packed_layout(w, h, bits, c.len).or_else(|| d100_layout(w, h, bits, c.len)),
        _ => None,
    };
    let (Some(layout), [c]) = (layout, chunks.as_slice()) else {
        return Err(RawError::Unsupported("Nikon compressed NEF without a linearization table (maker note 0x96)".into()));
    };
    let end = c.offset.saturating_add(c.len).min(bytes.len() as u64);
    let src = usize::try_from(c.offset).ok().zip(usize::try_from(end).ok()).and_then(|(a, b)| bytes.get(a..b));
    let src = src.ok_or_else(|| RawError::Corrupt("NEF: image data outside the file".into()))?;
    if (w as u64).saturating_mul(h as u64) > crate::MAX_SAMPLES as u64 {
        return Err(RawError::Limit("image too large"));
    }
    Ok(RawData::U16(unpack_packed(src, w, h, bits, layout)))
}

/// Nikon small raw (sRAW), measured clean-room from the CC0 raw.pixls.us D810 (2036) and D4S (2227) samples (analysis
/// log and tools under `data/testing/flash-nef-sraw`, not committed). The raw SubIFD is labelled compression 34713,
/// 12 bits, one CFA sample per pixel, but it carries no linearization table (maker note `0x0096`) and its single strip
/// is exactly `3 * width * height` bytes. It holds a YCbCr 4:2:2 image of the IFD size, not a CFA mosaic (the CFA
/// tags are dummies):
///
/// - the strip is an LSB-first 12-bit stream (`s0 = b0 | (b1 & 0x0f) << 8`, `s1 = b1 >> 4 | b2 << 4`), a row is `3 * width`
///   bytes with no padding, i.e. `2 * width` samples;
/// - the samples come in groups of four, `[Y_left, Y_right, Cb, Cr]`, for the pixel pair `(2k, 2k + 1)`: chroma is
///   halved horizontally and not vertically;
/// - luma is linear and already black-subtracted (the darkest samples are near 0) and never uses the top two bits
///   (maxima 873 and 1004 of 4095), so the white level is 1023; chroma is centred on 2048 (fitted neutral 2045-2049).
///
/// The chroma is a camera colour-difference scale, not JFIF Cb/Cr. In linear terms the stored values behave as
/// `R - Y = R_GAIN * (Cr - 2048)` and `B - Y = B_GAIN * (Cb - 2048)` on the luma scale: regressing the camera JPEG's
/// linear `(R - Y) / Y` and `(B - Y) / Y` on `(Cr - 2048) / Y` and `(Cb - 2048) / Y` over mid-tone pixels gave slopes
/// 4.92 / 5.09 (D810, correlation 0.94 / 0.88) and 3.52 / 3.73 (D4S, 0.98 / 0.97); the gains below are the rounded
/// means. They are approximate (the two bodies differ by about 30 %), the luma is not tone-mapped, and the data is
/// treated as already white balanced (neutral D4S areas have `Cb`, `Cr` within 2 of 2048 and equal JPEG R, G, B). G
/// follows from the luma equation with the BT.601 weights. The chroma is upsampled horizontally with linear
/// interpolation, centred between the two pixels of a pair. Output is linear RGB scaled by 16 (to a 14-bit range),
/// like Sony's downsized ARWs.
const SMALL_RAW_WHITE: f32 = 1023.0;
const SMALL_RAW_SCALE: f32 = 16.0;
const SMALL_RAW_CHROMA_ZERO: f32 = 2048.0;
const SMALL_RAW_R_GAIN: f32 = 4.2;
const SMALL_RAW_B_GAIN: f32 = 4.4;

/// The strip of a small raw: compression 34713, 12 bits, no maker note `0x0096`, one strip of exactly
/// `3 * width * height` bytes (even width).
fn small_raw_strip(info: &ImageInfo, file_len: usize, mn: Option<&makernote::MakerNote>) -> Option<lightcraft_tiff::image::Chunk> {
    if info.compression != t::compression::NIKON || info.bits() != 12 || info.samples_per_pixel != 1 {
        return None;
    }
    if mn.is_some_and(|m| m.ifd.bytes(LINEARIZATION_TABLE).is_some()) {
        return None;
    }
    let (w, h) = (info.width as u64, info.height as u64);
    let chunks = info.chunks(file_len as u64);
    match chunks.as_slice() {
        [c] if w.is_multiple_of(2) && w > 0 && h > 0 && c.len == 3 * w * h => Some(*c),
        _ => None,
    }
}

/// Decode one row of a small raw (`3 * width` bytes) into `3 * width` linear RGB values on the 14-bit scale.
/// Short input reads as zero bytes.
fn small_raw_row(src: &[u8], w: usize, out: &mut [u16]) {
    let mut s = vec![0u16; 2 * w];
    crate::unpack::unpack_lsb(&src[..src.len().min(3 * w)], 12, &mut s);
    let pairs = w / 2;
    let chroma = |k: usize| {
        let g = &s[4 * k + 2..4 * k + 4];
        (f32::from(g[0]) - SMALL_RAW_CHROMA_ZERO, f32::from(g[1]) - SMALL_RAW_CHROMA_ZERO)
    };
    for k in 0..pairs {
        let (cb, cr) = chroma(k);
        for side in 0..2 {
            let (nb, nr) = if side == 0 { chroma(k.saturating_sub(1)) } else { chroma((k + 1).min(pairs - 1)) };
            let (u, v) = (0.75 * cb + 0.25 * nb, 0.75 * cr + 0.25 * nr);
            let y = f32::from(s[4 * k + side]);
            let r = y + SMALL_RAW_R_GAIN * v;
            let b = y + SMALL_RAW_B_GAIN * u;
            let g = (y - 0.299 * r - 0.114 * b) / 0.587;
            let dst = (2 * k + side) * 3;
            for (i, value) in [r, g, b].into_iter().enumerate() {
                out[dst + i] = (value * SMALL_RAW_SCALE).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

fn small_raw(bytes: &[u8], tiff: &Tiff, info: &ImageInfo, strip: lightcraft_tiff::image::Chunk) -> Result<RawImage> {
    use rayon::prelude::*;
    let (w, h) = (info.width as usize, info.height as usize);
    if (w as u64).saturating_mul(h as u64) > crate::MAX_SAMPLES as u64 {
        return Err(RawError::Limit("image too large"));
    }
    let end = strip.offset.saturating_add(strip.len).min(bytes.len() as u64);
    let src = usize::try_from(strip.offset).ok().zip(usize::try_from(end).ok()).and_then(|(a, b)| bytes.get(a..b));
    let src = src.ok_or_else(|| RawError::Corrupt("NEF: image data outside the file".into()))?;
    let mut data = vec![0u16; 3 * w * h];
    data.par_chunks_mut(3 * w).enumerate().for_each(|(y, out)| {
        small_raw_row(src.get(y * 3 * w..).unwrap_or(&[]), w, out);
    });
    let mut metadata = lightcraft_meta::from_tiff(tiff);
    metadata.width = Some(w as u32);
    metadata.height = Some(h as u32);
    let img = RawImage {
        format: RawFormat::Nef,
        width: w,
        height: h,
        cpp: 3,
        data: RawData::U16(data),
        cfa: None,
        bits: 14,
        black: BlackLevel::uniform(0.0),
        white: vec![SMALL_RAW_WHITE * SMALL_RAW_SCALE],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::from_exif(tiff.ifds[0].u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData::default(),
        // the stored colour is already white balanced (see above)
        wb_multipliers: Some([1.0; 3]),
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
    };
    img.validate()?;
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    fn nef(compression: u16, bits: u16, strips: Vec<Vec<u8>>, w: u32, h: u32, rps: u32) -> Vec<u8> {
        nef_with_note(compression, bits, strips, w, h, rps, None)
    }

    /// [`nef`] with an optional maker note (`Nikon\0` v2 header + embedded big-endian TIFF holding `note`).
    fn nef_with_note(compression: u16, bits: u16, strips: Vec<Vec<u8>>, w: u32, h: u32, rps: u32, note: Option<IfdBuilder>) -> Vec<u8> {
        nef_in(ByteOrder::Big, compression, bits, strips, w, h, rps, note)
    }

    /// [`nef_with_note`] in either byte order (file and maker note).
    #[allow(clippy::too_many_arguments)]
    fn nef_in(order: ByteOrder, compression: u16, bits: u16, strips: Vec<Vec<u8>>, w: u32, h: u32, rps: u32, note: Option<IfdBuilder>) -> Vec<u8> {
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits]));
        raw.set(t::COMPRESSION, Value::Short(vec![compression]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![2, 1, 1, 0]));
        raw.set_image(ImageData::Strips { rows_per_strip: rps, strips });
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("NIKON CORPORATION".into()));
        ifd0.set(t::MODEL, Value::Ascii("NIKON TEST".into()));
        if let Some(mn) = note {
            let mut bytes = b"Nikon\0\x02\x10\0\0".to_vec();
            bytes.extend(TiffWriter::new(order, false).write(&[mn]).unwrap());
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(bytes));
            ifd0.set_child(t::EXIF_IFD, exif);
        }
        ifd0.add_sub_ifd(raw);
        TiffWriter::new(order, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn preview_color_space_is_read_from_the_nikon_note() {
        for (value, expected) in [(1, Some(crate::PreviewColorSpace::Srgb)), (2, Some(crate::PreviewColorSpace::AdobeRgb)), (3, None)] {
            let mut mn = IfdBuilder::new();
            mn.set(0x001e, Value::Short(vec![value]));
            let bytes = nef_with_note(1, 14, vec![vec![0; 128]], 8, 8, 8, Some(mn));
            assert_eq!(crate::embedded_preview_color_space(&bytes), expected);
        }
        assert_eq!(crate::embedded_preview_color_space(&nef(1, 14, vec![vec![0; 128]], 8, 8, 8)), None);
        assert_eq!(crate::embedded_preview_color_space(b"not a TIFF"), None);
    }

    #[test]
    fn nikon_crop_area_preserves_sensor_origin_and_rejects_invalid_rectangles() {
        let (w, h) = (32, 24);
        let words: Vec<u8> = (0..w * h).flat_map(|i| (100 + i as u16).to_be_bytes()).collect();
        for (values, expected) in [
            (vec![2, 4, 28, 16], Rect::new(2, 4, 28, 16)),
            (vec![31, 4, 28, 16], Rect::new(0, 0, 32, 24)),
            (vec![2, 4, 0, 16], Rect::new(0, 0, 32, 24)),
            (vec![u32::MAX, 4, 28, 16], Rect::new(0, 0, 32, 24)),
            (vec![2, 4, 28], Rect::new(0, 0, 32, 24)),
        ] {
            let mut mn = IfdBuilder::new();
            mn.set(CROP_AREA, Value::Long(values));
            let bytes = nef_with_note(1, 14, vec![words.clone()], w, h, h, Some(mn));
            let raw = crate::decode(&bytes).unwrap();
            assert_eq!(raw.crop, expected);
            assert_eq!(raw.active_area, Rect::new(0, 0, 32, 24));
            assert_eq!(raw.metadata.width, Some(expected.width as u32));
            assert_eq!(raw.metadata.height, Some(expected.height as u32));
            let developed = raw.develop(crate::Method::Bilinear).unwrap();
            assert_eq!((developed.width, developed.height), (expected.width, expected.height));
        }
    }

    #[test]
    fn black_level_tag_is_in_14_bit_units() {
        let (w, h) = (16usize, 4usize);
        let words: Vec<u8> = (0..w * h).flat_map(|i| (100 + i as u16).to_be_bytes()).collect();
        let note = || {
            let mut mn = IfdBuilder::new();
            mn.set(BLACK_LEVEL, Value::Short(vec![400, 404, 408, 412]));
            mn
        };
        let black = |bits| crate::decode(&nef_with_note(1, bits, vec![words.clone()], w as u32, h as u32, h as u32, Some(note()))).unwrap().black;
        assert_eq!(black(12).values, vec![100.0, 101.0, 102.0, 103.0]);
        assert_eq!(black(14).values, vec![400.0, 404.0, 408.0, 412.0]);
    }

    #[test]
    fn uncompressed_word16_and_packed12() {
        let (w, h) = (16usize, 6usize);
        let px: Vec<u16> = (0..w * h).map(|i| (i * 131 % 4096) as u16).collect();
        let words: Vec<u8> = px.iter().flat_map(|v| v.to_be_bytes()).collect();
        let bytes = nef(1, 12, words.chunks(w * 2 * 3).map(|c| c.to_vec()).collect(), w as u32, h as u32, 3);
        assert_eq!(crate::probe(&bytes), Some(RawFormat::Nef));
        let r = crate::decode(&bytes).unwrap();
        assert_eq!(r.data, RawData::U16(px.clone()));
        assert_eq!(r.cfa.as_ref().unwrap().name(), "BGGR");
        // 12-bit MSB packed
        let mut packed = Vec::new();
        for pair in px.chunks(2) {
            let (a, b) = (pair[0] as u32, pair[1] as u32);
            let v = (a << 12) | b;
            packed.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        let bytes = nef(1, 12, vec![packed], w as u32, h as u32, h as u32);
        assert_eq!(crate::decode(&bytes).unwrap().data, RawData::U16(px));
    }

    /// An NRW data block (`"NRW 0104"`, fields from byte 8) with `black` at byte `0x20`, as a maker note.
    fn nrw_note(black: u32) -> IfdBuilder {
        let mut block = b"NRW 0104".to_vec();
        block.resize(0x20, 0);
        block.extend_from_slice(&black.to_le_bytes());
        block.resize(0x40, 0);
        let mut mn = IfdBuilder::new();
        mn.set(NRW_DATA, Value::Undefined(block));
        mn
    }

    /// 12-bit samples as an MSB-first bit stream inside 32-bit words in `order` (rows are whole words).
    fn words32(px: &[u16], w: usize, order: ByteOrder) -> Vec<u8> {
        let mut out = Vec::new();
        for row in px.chunks(w) {
            let stream = row.iter().fold(Vec::new(), |mut bits: Vec<bool>, v| {
                bits.extend((0..12).rev().map(|b| v >> b & 1 == 1));
                bits
            });
            for word in stream.chunks(32) {
                let v = word.iter().enumerate().fold(0u32, |a, (i, b)| a | (*b as u32) << (31 - i));
                out.extend_from_slice(&match order {
                    ByteOrder::Little => v.to_le_bytes(),
                    ByteOrder::Big => v.to_be_bytes(),
                });
            }
        }
        out
    }

    #[test]
    fn nrw_block_selects_32_bit_word_packing_and_supplies_the_black_level() {
        let (w, h) = (16usize, 6usize);
        let px: Vec<u16> = (0..w * h).map(|i| (200 + i * 131 % 3800) as u16).collect();
        let packed = words32(&px, w, ByteOrder::Little);
        assert_eq!(packed.len(), w * h * 3 / 2);
        let decode = |note: Option<IfdBuilder>| {
            crate::decode(&nef_in(ByteOrder::Little, 1, 12, vec![packed.clone()], w as u32, h as u32, h as u32, note)).unwrap()
        };
        let r = decode(Some(nrw_note(200)));
        assert_eq!(r.data, RawData::U16(px.clone()));
        assert_eq!(r.black.values, vec![200.0]);
        assert_eq!(decode(Some(nrw_note(0))).black.values, vec![0.0]);
        // an implausible field is not a black level
        assert_eq!(decode(Some(nrw_note(60000))).black.values, vec![0.0]);
        // without the block the same bytes are an MSB-first byte stream
        let plain = decode(None);
        assert_ne!(plain.data, RawData::U16(px));
        assert_eq!(plain.black.values, vec![0.0]);
        // the maker-note black level (14-bit units) wins when present
        let mut both = nrw_note(200);
        both.set(BLACK_LEVEL, Value::Short(vec![400, 404, 408, 412]));
        assert_eq!(decode(Some(both)).black.values, vec![100.0, 101.0, 102.0, 103.0]);
    }

    #[test]
    fn compressed_without_table_is_unsupported() {
        let bytes = nef(34713, 14, vec![vec![0; 64]], 8, 8, 8);
        let Err(RawError::Unsupported(why)) = crate::decode(&bytes) else { panic!("expected unsupported") };
        assert!(why.contains("linearization table"), "{why}");
    }

    /// Issue #193: a High Efficiency NEF (Z f, Z 8: compression 34713, no `0x0096` table, a JPEG XS codestream) is
    /// named as such instead of being blamed on the missing table, and the header probe reports the same reason.
    #[test]
    fn high_efficiency_nef_is_named() {
        let mut strip = vec![0xff, 0x10, 0xff, 0x50, 0x00, 0x22];
        strip.resize(64, 0);
        let bytes = nef(34713, 14, vec![strip], 8, 8, 8);
        for result in [crate::decode(&bytes).map(|_| ()), crate::probe_info(&bytes).map(|_| ())] {
            let Err(RawError::Unsupported(why)) = result else { panic!("expected unsupported") };
            assert_eq!(why, HIGH_EFFICIENCY);
            assert!(why.starts_with("Nikon High Efficiency NEF ("), "{why}");
        }
        // a strip too short to hold the markers is still the missing-table case, not a panic
        let short = nef(34713, 14, vec![vec![0xff, 0x10]], 1, 1, 1);
        assert!(matches!(crate::decode(&short), Err(RawError::Unsupported(w)) if w.contains("linearization table")));
    }

    fn samples(n: usize, bits: u32) -> Vec<u16> {
        (0..n).map(|i| (((i * 2654435761usize) >> 7) as u32 % (1 << bits)) as u16).collect()
    }

    fn pack_lsb(row: &[u16], bits: u32) -> Vec<u8> {
        let (mut out, mut acc, mut n) = (Vec::new(), 0u64, 0u32);
        for &v in row {
            acc |= (v as u64) << n;
            n += bits;
            while n >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            out.push(acc as u8);
        }
        out
    }

    fn pack_msb12(row: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for pair in row.chunks(2) {
            let (a, b) = (pair[0], pair.get(1).copied().unwrap_or(0));
            out.extend([(a >> 4) as u8, ((a & 15) << 4) as u8 | (b >> 8) as u8, b as u8]);
        }
        out
    }

    /// Rows of `samples` packed by `row`, each padded with `pad` bytes of 0xA5 (never decoded).
    fn strip(data: &[u16], w: usize, pad: usize, row: impl Fn(&[u16]) -> Vec<u8>) -> Vec<u8> {
        data.chunks(w).flat_map(|r| row(r).into_iter().chain(std::iter::repeat_n(0xA5, pad))).collect()
    }

    fn decoded(strip: Vec<u8>, bits: u16, w: u32, h: u32) -> Result<Vec<u16>> {
        let bytes = nef(34713, bits, vec![strip], w, h, h);
        match crate::decode(&bytes)?.data {
            RawData::U16(d) => Ok(d),
            _ => Err(RawError::Unsupported("float".into())),
        }
    }

    #[test]
    fn packed_34713_layouts_round_trip() {
        let (w, h) = (40usize, 6usize);
        let d12 = samples(w * h, 12);
        let d14 = samples(w * h, 14);
        // group A: 12-bit, 3w/2 bytes per row
        assert_eq!(decoded(strip(&d12, w, 0, |r| pack_lsb(r, 12)), 12, 40, 6).unwrap(), d12);
        // group B: 12-bit, rows padded to 16 bytes (60 -> 64)
        assert_eq!(decoded(strip(&d12, w, 4, |r| pack_lsb(r, 12)), 12, 40, 6).unwrap(), d12);
        // group C: 14-bit, 70 bytes -> 80
        assert_eq!(decoded(strip(&d14, w, 10, |r| pack_lsb(r, 14)), 14, 40, 6).unwrap(), d14);
        // group E: 16-byte chunks, 15 data bytes (ten samples) + a zero byte; 16 chunks = 160 samples (at 40 wide the
        // 64-byte stride would be group B's)
        let (w, h) = (160usize, 3usize);
        let d12 = samples(w * h, 12);
        let e = strip(&d12, w, 0, |r| r.chunks(10).flat_map(|c| pack_msb12(c).into_iter().chain([0u8])).collect());
        assert_eq!(e.len(), 256 * h);
        assert_eq!(decoded(e, 12, 160, 3).unwrap(), d12);
    }

    #[test]
    fn packed_d100_rows_carry_extra_columns() {
        // IFD width 154, rows hold 160 samples (16 chunks); the last 6 are dropped
        let (w, wide, h) = (154usize, 160usize, 3usize);
        let wide_data = samples(wide * h, 12);
        let e = strip(&wide_data, wide, 0, |r| r.chunks(10).flat_map(|c| pack_msb12(c).into_iter().chain([0u8])).collect());
        let got = decoded(e, 12, w as u32, h as u32).unwrap();
        let want: Vec<u16> = wide_data.chunks(wide).flat_map(|r| r[..w].iter().copied()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn packed_34713_wrong_sizes_stay_refused() {
        let d12 = samples(40 * 6, 12);
        let good = strip(&d12, 40, 0, |r| pack_lsb(r, 12));
        // one byte more or fewer, a stride that is neither tight nor 16-aligned, a 13-bit file
        for len in [good.len() - 1, good.len() + 1, good.len() + 6] {
            let mut s = good.clone();
            s.resize(len, 0);
            assert!(matches!(decoded(s, 12, 40, 6), Err(RawError::Unsupported(_))), "len {len}");
        }
        assert!(matches!(decoded(good.clone(), 13, 40, 6), Err(RawError::Unsupported(_))));
        // 14-bit rows must be padded to 16 bytes
        let d14 = samples(40 * 6, 14);
        assert!(matches!(decoded(strip(&d14, 40, 0, |r| pack_lsb(r, 14)), 14, 40, 6), Err(RawError::Unsupported(_))));
    }

    /// A small raw strip of `[Y0 Y1 Cb Cr]` groups, rows of `3 * w` bytes, LSB-first 12-bit.
    fn small_strip(groups: &[[u16; 4]], w: usize) -> Vec<u8> {
        let flat: Vec<u16> = groups.iter().flatten().copied().collect();
        assert_eq!(flat.len() % (2 * w), 0);
        flat.chunks(2 * w).flat_map(|r| pack_lsb(r, 12)).collect()
    }

    fn small(strip: Vec<u8>, bits: u16, w: u32, h: u32) -> Result<RawImage> {
        crate::decode(&nef(34713, bits, vec![strip], w, h, h))
    }

    #[test]
    fn small_raw_422_round_trips() {
        let (w, h) = (8usize, 3usize);
        // flat chroma per row so the interpolation is exact; distinct luma everywhere
        let mut groups = Vec::new();
        for y in 0..h {
            for k in 0..w / 2 {
                let (cb, cr) = (2048 + 10 * y as u16, 2048 - 5 * y as u16);
                groups.push([100 + (y * w + 2 * k) as u16, 100 + (y * w + 2 * k + 1) as u16, cb, cr]);
            }
        }
        let raw = small(small_strip(&groups, w), 12, w as u32, h as u32).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp, raw.cfa.is_none()), (w, h, 3, true));
        assert_eq!(raw.wb_multipliers, Some([1.0; 3]));
        let RawData::U16(d) = &raw.data else { panic!() };
        for y in 0..h {
            for x in 0..w {
                let luma = (100 + y * w + x) as f32;
                let (u, v) = (10.0 * y as f32, -5.0 * y as f32);
                let (r, b) = (luma + SMALL_RAW_R_GAIN * v, luma + SMALL_RAW_B_GAIN * u);
                let g = (luma - 0.299 * r - 0.114 * b) / 0.587;
                for (i, want) in [r, g, b].into_iter().enumerate() {
                    let got = f32::from(d[(y * w + x) * 3 + i]) / SMALL_RAW_SCALE;
                    assert!((got - want).abs() < 0.1, "({x},{y}) channel {i}: {got} vs {want}");
                }
            }
        }
    }

    #[test]
    fn small_raw_neutral_chroma_is_grey_and_chroma_is_interpolated() {
        let w = 8usize;
        let groups: Vec<[u16; 4]> = (0..w / 2).map(|k| [200 + 40 * k as u16, 200 + 40 * k as u16, 2048, 2048]).collect();
        let raw = small(small_strip(&groups, w), 12, w as u32, 1).unwrap();
        let RawData::U16(d) = &raw.data else { panic!() };
        for x in 0..w {
            let px = &d[x * 3..x * 3 + 3];
            assert!(px.iter().all(|&v| v == px[0]), "{px:?}");
            assert_eq!(f32::from(px[0]), (200 + 40 * (x / 2)) as f32 * SMALL_RAW_SCALE);
        }
        // a step in Cb between the first two pairs is shared 3:1 by the pixels next to the boundary
        let mut groups = groups;
        groups[1][2] = 2048 + 100;
        let raw = small(small_strip(&groups, w), 12, w as u32, 1).unwrap();
        let RawData::U16(d) = &raw.data else { panic!() };
        let blue_minus_luma = |x: usize, luma: f32| f32::from(d[x * 3 + 2]) / SMALL_RAW_SCALE - luma;
        assert!((blue_minus_luma(1, 200.0) - SMALL_RAW_B_GAIN * 25.0).abs() < 0.1);
        assert!((blue_minus_luma(2, 240.0) - SMALL_RAW_B_GAIN * 75.0).abs() < 0.1);
    }

    #[test]
    fn small_raw_wrong_sizes_and_depths_are_refused() {
        let (w, h) = (8usize, 3usize);
        let good = small_strip(&vec![[100, 100, 2048, 2048]; w / 2 * h], w);
        assert!(small(good.clone(), 12, w as u32, h as u32).is_ok());
        for len in [good.len() - 1, good.len() + 1] {
            let mut s = good.clone();
            s.resize(len, 0);
            assert!(matches!(small(s, 12, w as u32, h as u32), Err(RawError::Unsupported(_))), "len {len}");
        }
        assert!(matches!(small(good.clone(), 14, w as u32, h as u32), Err(RawError::Unsupported(_))));
        // odd width: 3 bytes per pixel does not split into pixel pairs
        assert!(matches!(small(vec![0; 3 * 7 * 3], 12, 7, 3), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn small_raw_short_input_does_not_panic() {
        for len in [0, 1, 2, 3, 5, 11, 12, 13] {
            let mut out = vec![0u16; 12];
            small_raw_row(&vec![0xFF; len], 4, &mut out);
        }
        let mut bytes = nef(34713, 12, vec![vec![0x55; 3 * 8 * 3]], 8, 3, 3);
        bytes.truncate(bytes.len() - 40);
        let _ = crate::decode(&bytes);
    }

    #[test]
    fn packed_34713_short_input_does_not_panic() {
        let (w, h) = (40usize, 6usize);
        for layout in [PackedLayout::Lsb { stride: 64 }, PackedLayout::Lsb { stride: 80 }, PackedLayout::Chunked16 { stride: 64 }] {
            for len in [0, 1, 15, 16, 17, 63, 64, 65, 200] {
                let bits = if matches!(layout, PackedLayout::Lsb { stride: 80 }) { 14 } else { 12 };
                assert_eq!(unpack_packed(&vec![0xFF; len], w, h, bits, layout).len(), w * h);
            }
        }
        // a strip that claims more bytes than the file has
        let mut bytes = nef(34713, 12, vec![vec![0x55; 60 * 6]], 40, 6, 6);
        bytes.truncate(bytes.len() - 100);
        let _ = crate::decode(&bytes);
    }
}
