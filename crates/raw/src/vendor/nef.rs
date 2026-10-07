//! Nikon NEF / NRW — uncompressed and Huffman-compressed (lossless, lossy) variants.
//!
//! Sources: TIFF 6.0 (the raw image is a standard CFA SubIFD), Laurent Clévy's NEF structure notes (prose: IFD
//! layout, SubIFDs, maker note header) and the ExifTool Nikon tag-name documentation (`0x000c` WB_RBLevels,
//! `0x003d` BlackLevel, `0x0096` NEFLinearizationTable). The Huffman-compressed data (compression 34713) is decoded
//! by [`super::nefc`], which documents its clean-room sources; files it can't decode yet ("lossy after split")
//! are reported as [`RawError::Unsupported`] and their embedded previews still work.

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
    let data =
        if info.compression == t::compression::NIKON { compressed(bytes, &info, mn.as_ref())? } else { uncompressed(bytes, &info, tiff.order)? };
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
        _ => BlackLevel::uniform(0.0),
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
    // Nikon ColorTemperatureAuto (ExifTool tag documentation), not a camera colour profile.
    metadata.camera_temperature = mn
        .as_ref()
        .filter(|m| m.ifd.string(0x0005).is_some_and(|mode| mode.starts_with("AUTO")))
        .and_then(|m| m.ifd.u16(0x004f))
        .filter(|k| (2000..=50000).contains(k));
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

/// Uncompressed strips: 16-bit words or 12-bit MSB-packed, told apart by the strip size.
fn uncompressed(bytes: &[u8], info: &ImageInfo, order: ByteOrder) -> Result<RawData> {
    let (w, h) = (info.width as usize, info.height as usize);
    let bits = info.bits() as u32;
    let row_samples = w * info.samples_per_pixel as usize;
    let chunks = info.chunks(bytes.len() as u64);
    let rows_in_first = chunks.first().map(|c| c.height as usize).unwrap_or(h).max(1);
    let bytes_per_row = chunks.first().map(|c| c.len as usize / rows_in_first).unwrap_or(0);
    let packing = if bytes_per_row >= row_samples * 2 {
        Packing::Word16
    } else if bits == 12 && bytes_per_row * 8 >= row_samples * 12 && bytes_per_row * 8 < row_samples * 13 {
        Packing::Msb
    } else if info.compression == 1 && bits != 8 && bits != 16 {
        return Err(RawError::Unsupported(format!("NEF uncompressed packing ({bytes_per_row} bytes per {row_samples}-sample row)")));
    } else {
        Packing::Msb
    };
    read_image(bytes, info, order, packing)
}

/// Nikon Huffman-compressed strip (see [`super::nefc`]); the decode table is maker note `0x0096`.
fn compressed(bytes: &[u8], info: &ImageInfo, mn: Option<&makernote::MakerNote>) -> Result<RawData> {
    // (some Z bodies label uncompressed, row-padded data 34713 without a table: not handled here yet)
    let table = mn
        .and_then(|m| Some((m.ifd.bytes(LINEARIZATION_TABLE)?, m.order)))
        .ok_or_else(|| RawError::Unsupported("Nikon compressed NEF without a linearization table (maker note 0x96)".into()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    fn nef(compression: u16, bits: u16, strips: Vec<Vec<u8>>, w: u32, h: u32, rps: u32) -> Vec<u8> {
        nef_with_note(compression, bits, strips, w, h, rps, None)
    }

    /// [`nef`] with an optional maker note (`Nikon\0` v2 header + embedded big-endian TIFF holding `note`).
    fn nef_with_note(compression: u16, bits: u16, strips: Vec<Vec<u8>>, w: u32, h: u32, rps: u32, note: Option<IfdBuilder>) -> Vec<u8> {
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
            bytes.extend(TiffWriter::new(ByteOrder::Big, false).write(&[mn]).unwrap());
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(bytes));
            ifd0.set_child(t::EXIF_IFD, exif);
        }
        ifd0.add_sub_ifd(raw);
        TiffWriter::new(ByteOrder::Big, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn camera_temperature_is_validated_and_survives_header_probe() {
        for (value, expected) in [(5550, Some(5550)), (0, None), (65535, None)] {
            let mut mn = IfdBuilder::new();
            mn.set(0x0005, Value::Ascii("AUTO0".into()));
            mn.set(0x004f, Value::Short(vec![value]));
            let bytes = nef_with_note(1, 14, vec![vec![0; 128]], 8, 8, 8, Some(mn));
            assert_eq!(crate::probe_info(&bytes).unwrap().metadata.camera_temperature, expected);
            assert_eq!(crate::decode(&bytes).unwrap().metadata.camera_temperature, expected);
        }
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

    #[test]
    fn compressed_without_table_is_unsupported() {
        let bytes = nef(34713, 14, vec![vec![0; 64]], 8, 8, 8);
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
    }
}
