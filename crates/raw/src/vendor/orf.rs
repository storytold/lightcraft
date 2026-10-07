//! Olympus ORF — the container, the uncompressed variants and everything around the samples. The compressed
//! variant's bit stream is decoded by [`super::orfc`].
//!
//! Sources: TIFF 6.0 (the `IIRO`/`MMOR` container is a TIFF with a different magic number), Exif 2.3 (`CFAPattern`,
//! tag `0xa302`), the ExifTool Olympus tag-name documentation (maker-note sub-directories `0x2020` CameraSettings —
//! `0x0101/0x0102` PreviewImageStart/Length — and `0x2040` ImageProcessing — `0x0100` WB_RBLevels, `0x0600`
//! BlackLevel2, `0x0612–0x0615` CropLeft/Top/Width/Height) and our own black-box analysis of CC0 samples from
//! raw.pixls.us (16 files, E-1 … OM-1 Mark II):
//!
//! - 16 bits per sample: little-endian words; some bodies (E-1, E-400) store 12-bit values in the top bits (the low
//!   four bits zero in more than 99% of samples), which we shift down.
//! - 12-bit packed (XZ-2): each row is a sequence of little-endian 32-bit words read MSB-first (found by testing
//!   candidate bit orders for the smoothest image).
//! - Compressed (E-410 onwards): 16 bits declared, one strip well under 12 bits per pixel that starts with
//!   `00 00 00 00 01 00 00`; see [`super::orfc`]. Other strips of that size (the OM-1 Mark II's 14-bit High Res Shot)
//!   report [`RawError::Unsupported`]; the embedded preview still works.
//! - The colour-filter layout is the Exif `CFAPattern` of the file, present in all 16 samples: GRBG (E-1, E-400,
//!   TG-6), BGGR (E-620, E-M1) or RGGB (the rest). It agrees with the picture in every one of them: with it, the
//!   red/blue balance of the decoded mosaic follows the embedded JPEG (`corpus_orf_matches_embedded_preview`). The
//!   samples' active areas all start at even offsets, so they can't tell whether the pattern is anchored at the
//!   sensor origin (assumed) or at the active area. Files without the tag fall back to the green diagonal found
//!   from the data, which can't tell red from blue (RGGB or GRBG is assumed).

use super::{orfc, white_from_data};
use crate::tiffraw::{Packing, read_image};
use crate::unpack::unpack_msb;
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_geom::Orientation;
use lightcraft_tiff::image::chunk_bytes;
use lightcraft_tiff::makernote::MakerNote;
use lightcraft_tiff::{Ifd, Tiff, Value, makernote, tags as t};
use rayon::prelude::*;

pub(crate) const CAMERA_SETTINGS: u16 = 0x2020;
const IMAGE_PROCESSING: u16 = 0x2040;
pub(crate) const PREVIEW_START: u16 = 0x0101;
pub(crate) const PREVIEW_LENGTH: u16 = 0x0102;
const WB_RB: u16 = 0x0100;
const BLACK: u16 = 0x0600;
const CROP: [u16; 4] = [0x0612, 0x0613, 0x0614, 0x0615];
/// Exif `CFAPattern`.
const EXIF_CFA_PATTERN: u16 = 0xa302;

/// The Olympus maker note.
pub(crate) fn maker_note(bytes: &[u8], tiff: &Tiff) -> Option<MakerNote> {
    let make = tiff.find(t::MAKE).and_then(|e| e.value.as_str()).unwrap_or_default().to_string();
    let e = tiff.exif().and_then(|e| e.get(t::MAKER_NOTE))?;
    makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make)
}

/// A maker-note sub-directory: an IFD pointer (offset relative to the note's base) in new-style notes, or the
/// IFD stored inline as an undefined blob (offsets relative to the base) in old-style ones.
pub(crate) fn sub_ifd(bytes: &[u8], mn: &MakerNote, tag: u16) -> Option<Ifd> {
    let e = mn.ifd.get(tag)?;
    let at = match &e.value {
        Value::Undefined(_) | Value::Byte(_) => e.offset,
        _ => mn.base.checked_add(mn.ifd.u64(tag)?)?,
    };
    let opts = lightcraft_tiff::ParseOptions { max_ifds: 4, max_depth: 1, follow_children: false, ..Default::default() };
    lightcraft_tiff::parse_ifd_at(bytes, at, mn.order, mn.base, false, &opts).ok().map(|(i, _)| i)
}

/// Unpack one row of 12-bit samples stored as little-endian 32-bit words read MSB-first.
pub(crate) fn unpack_row_le32_msb(src: &[u8], bits: u32, out: &mut [u16]) {
    let swapped: Vec<u8> = src
        .chunks(4)
        .flat_map(|c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            [w[3], w[2], w[1], w[0]]
        })
        .collect();
    unpack_msb(&swapped, bits, out);
}

/// The colour-filter layout from the Exif `CFAPattern` tag (`0xa302`: two 16-bit repeat counts in either byte
/// order, then one byte per site, 0 = red, 1 = green, 2 = blue), when it describes a 2×2 Bayer cell.
fn cfa_from_exif(tiff: &Tiff) -> Option<Cfa> {
    let [c0, c1, r0, r1, sites @ ..] = tiff.exif()?.bytes(EXIF_CFA_PATTERN)? else { return None };
    let two = |a: &u8, b: &u8| matches!((*a, *b), (2, 0) | (0, 2));
    let count = |colour: u8| sites.iter().filter(|&&s| s == colour).count();
    (two(c0, c1) && two(r0, r1) && sites.len() == 4 && (count(0), count(1), count(2)) == (1, 2, 1) && (sites[0] == 1) == (sites[3] == 1))
        .then(|| Cfa { width: 2, height: 2, pattern: sites.to_vec() })
}

/// The layout found from the samples, for files without the Exif tag: GRBG when the greens sit on the main
/// diagonal of the 2×2 cell at the sensor origin, else RGGB. Over 32×32-pixel blocks of `a` (every fourth in both
/// directions) it compares the block totals of the two sites on each diagonal: the two greens of a block see the
/// same light, red and blue rarely do. (Differences of single pixels are not enough: on the E-M10 Mark III sample,
/// whose red and blue levels are close, texture outweighs them.)
pub(crate) fn cfa_from_data(d: &[u16], w: usize, a: Rect) -> Cfa {
    const BLOCK: usize = 32;
    let (x0, y0) = ((a.x + 1) & !1, (a.y + 1) & !1);
    let across = (a.x + a.width).saturating_sub(x0) / BLOCK;
    let down = (a.y + a.height).saturating_sub(y0) / BLOCK;
    let (mut main, mut anti) = (0u64, 0u64);
    for by in (0..down).step_by(4) {
        for bx in (0..across).step_by(4) {
            let mut sums = [0i64; 4];
            for y in 0..BLOCK {
                let start = (y0 + by * BLOCK + y) * w + x0 + bx * BLOCK;
                let Some(row) = d.get(start..start + BLOCK) else { continue };
                for (x, &v) in row.iter().enumerate() {
                    sums[(y & 1) * 2 + (x & 1)] += v as i64;
                }
            }
            main += (sums[0] - sums[3]).unsigned_abs();
            anti += (sums[1] - sums[2]).unsigned_abs();
        }
    }
    Cfa::bayer_static(if main < anti { "GRBG" } else { "RGGB" })
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = &tiff.ifds[0];
    let info = ifd0.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let n = w.checked_mul(h).filter(|n| *n > 0 && *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let mn = maker_note(bytes, &tiff);
    let ip = mn.as_ref().and_then(|m| sub_ifd(bytes, m, IMAGE_PROCESSING));
    let active = match ip.as_ref().map(|i| CROP.map(|tag| i.u64(tag).map(|v| v as usize))) {
        Some([Some(x), Some(y), Some(cw), Some(ch)]) if cw > 0 && ch > 0 && x + cw <= w && y + ch <= h => Rect::new(x, y, cw, ch),
        _ => Rect::new(0, 0, w, h),
    };
    let chunks = info.chunks(bytes.len() as u64);
    let total: u64 = chunks.iter().map(|c| c.len).sum();
    let stated = cfa_from_exif(&tiff);
    let (mut data, bits) = if info.compression != 1 {
        return Err(RawError::Unsupported(format!("ORF compression {}", info.compression)));
    } else if total >= (n as u64) * 2 {
        let d = read_image(bytes, &info, tiff.order, Packing::Word16)?;
        (d, 16)
    } else if let [chunk] = chunks.as_slice()
        && let Some(src) = chunk_bytes(bytes, chunk)
        && orfc::is_compressed(src)
    {
        // a header-only probe needs the samples only when the file doesn't state its colour-filter layout
        let d = if mode == Mode::Full || stated.is_none() { orfc::decode(src, w, h)? } else { Vec::new() };
        (RawData::U16(d), 12)
    } else if total * 8 >= (n as u64) * 12 && total * 8 < (n as u64) * 13 && chunks.len() == 1 {
        let src = chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("ORF strip outside file".into()))?;
        let stride = src.len() / h;
        let mut d = vec![0u16; n];
        d.par_chunks_mut(w).enumerate().for_each(|(y, row)| unpack_row_le32_msb(&src[y * stride..(y + 1) * stride], 12, row));
        (RawData::U16(d), 12)
    } else {
        let head = chunks.first().and_then(|c| chunk_bytes(bytes, c)).and_then(|s| s.get(..8)).unwrap_or_default();
        let head: Vec<String> = head.iter().map(|b| format!("{b:02x}")).collect();
        return Err(RawError::Unsupported(format!("this Olympus compressed ORF variant is not decoded yet (strip header {})", head.join(" "))));
    };
    let RawData::U16(ref mut samples) = data else { return Err(RawError::Unsupported("float ORF".into())) };
    let bits = if bits == 16 && samples.iter().step_by(7).filter(|v| *v & 15 != 0).count() * 700 <= n {
        // 12-bit values stored in the top bits
        samples.par_iter_mut().for_each(|v| *v >>= 4);
        12
    } else if bits == 16 {
        let mx = samples.iter().step_by(31).max().copied().unwrap_or(0);
        if mx < 4096 {
            12
        } else if mx < 16384 {
            14
        } else {
            16
        }
    } else {
        bits
    };

    let cfa = stated.unwrap_or_else(|| cfa_from_data(samples, w, active));
    let black = match ip.as_ref().and_then(|i| i.f64s(BLACK)).as_deref() {
        Some(v @ [_, _, _, _]) => {
            let a = cfa.shifted(active.x, active.y);
            let values = a.pattern.iter().map(|&c| [v[0], (v[1] + v[2]) / 2.0, v[3]][c as usize] as f32).collect();
            BlackLevel { repeat_rows: 2, repeat_cols: 2, values, ..Default::default() }
        }
        _ => BlackLevel::uniform(0.0),
    };
    let wb = ip
        .as_ref()
        .and_then(|i| i.f64s(WB_RB))
        .filter(|v| v.len() >= 2 && v[0] > 0.0 && v[1] > 0.0)
        .map(|v| [(v[0] / 256.0) as f32, 1.0, (v[1] / 256.0) as f32]);
    let white = white_from_data(samples, bits);
    let mut metadata = lightcraft_meta::from_tiff(&tiff);
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let img = RawImage {
        format: RawFormat::Orf,
        width: w,
        height: h,
        cpp: 1,
        data,
        cfa: Some(cfa),
        bits,
        black,
        white: vec![white],
        active_area: active,
        crop: Rect::new(0, 0, active.width, active.height),
        orientation: Orientation::from_exif(ifd0.u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData::default(),
        wb_multipliers: wb,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
    };
    img.validate_for(mode)?;
    Ok(img)
}

/// The large preview JPEG referenced by CameraSettings `PreviewImageStart/Length` (relative to the note base).
pub(crate) fn preview(bytes: &[u8]) -> Option<&[u8]> {
    let tiff = Tiff::parse(bytes).ok()?;
    let mn = maker_note(bytes, &tiff)?;
    let cs = sub_ifd(bytes, &mn, CAMERA_SETTINGS)?;
    let start = mn.base.checked_add(cs.u64(PREVIEW_START)?)? as usize;
    let len = cs.u64(PREVIEW_LENGTH)? as usize;
    bytes.get(start..start.checked_add(len)?.min(bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter};

    fn orf(w: u32, h: u32, bits: u16, strip: Vec<u8>) -> Vec<u8> {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![1]));
        ifd.set(t::MAKE, Value::Ascii("OLYMPUS IMAGING CORP.".into()));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
        let mut b = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
        b[2] = b'R';
        b[3] = b'O';
        b
    }

    #[test]
    fn word16_shifted_and_packed12() {
        let (w, h) = (16usize, 4usize);
        let px: Vec<u16> = (0..w * h).map(|i| ((i * 211) % 4096) as u16).collect();
        let words: Vec<u8> = px.iter().flat_map(|v| (v << 4).to_le_bytes()).collect();
        let bytes = orf(w as u32, h as u32, 16, words);
        assert_eq!(crate::probe(&bytes), Some(RawFormat::Orf));
        let r = crate::decode(&bytes).unwrap();
        assert_eq!((r.data.clone(), r.bits), (RawData::U16(px.clone()), 12));
        // 12-bit: MSB-first stream, every 32-bit word byte-swapped
        let mut be = Vec::new();
        for p in px.chunks(2) {
            let v = (p[0] as u32) << 12 | p[1] as u32;
            be.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        let le: Vec<u8> = be.chunks(4).flat_map(|c| [c[3], c[2], c[1], c[0]]).collect();
        let bytes = orf(w as u32, h as u32, 12, le);
        assert_eq!(crate::decode(&bytes).unwrap().data, RawData::U16(px));
        let bytes = orf(w as u32, h as u32, 16, vec![0; 40]);
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
    }
}
