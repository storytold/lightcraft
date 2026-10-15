//! Mamiya MEF (ZD): a big-endian TIFF whose first `SubIFDs` child is an uncompressed 12-bit packed CFA image.
//!
//! Sources: TIFF 6.0 (container, MSB-first bit packing, `CFAPattern`) and our own black-box analysis of the one CC0
//! MEF sample on raw.pixls.us (Mamiya ZD); every number below was measured on it.
//!
//! - IFD0 is a 144 x 192 RGB thumbnail with three `SubIFDs`: the raw (4016 x 5344, 12 bits, `Compression` 1,
//!   photometric CFA, one strip of exactly 1.5 bytes per pixel, two samples in three bytes most significant bits
//!   first, as TIFF packs them), a 992 x 1328 and a 240 x 320 RGB preview, all uncompressed and 8 bits. No JPEG.
//! - `CFAPattern` `00 01 01 02`: RGGB; the two green sites agree (means 254 and 256 against 235 and 165).
//! - No masked border and no stored black level: the darkest 0.01% of each site's samples sit at 3..6 and the
//!   minimum is 0, so black is 0. White: the saturation plateau, else 4095.
//! - The 992 x 1328 preview shows a 3968 x 5312 window (4 sensor pixels per preview pixel) that starts at column 24,
//!   row 12: that offset gives the best correlation of the preview's luminance with the box-filtered mosaic (0.965,
//!   against 0.83 for the full frame). It is the default crop.
//! - Orientation is the raw IFD's own `Orientation` (6 on the sample, a portrait shot).
//! - No white-balance tag was found (the maker note holds an ICC-like profile), so the as-shot gains are measured
//!   from that preview's neutral pixels ([`crate::wb_estimate`]).
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_geom::Orientation;
use lightcraft_tiff::image::chunk_bytes;
use lightcraft_tiff::{Ifd, Tiff, tags as t};

/// Where the preview's window starts in the sensor frame, and how many sensor pixels one preview pixel covers.
const CROP_ORIGIN: (usize, usize) = (24, 12);
const PREVIEW_SCALE: usize = 4;

fn is_mamiya(tiff: &Tiff) -> bool {
    tiff.find(t::MAKE).and_then(|e| e.value.as_str()).is_some_and(|m| m.to_ascii_uppercase().starts_with("MAMIYA"))
}

/// The raw IFD of a Mamiya MEF: CFA, 12 bits, uncompressed, one strip of 1.5 bytes per pixel.
pub(crate) fn raw_ifd(tiff: &Tiff, len: usize) -> Option<&Ifd> {
    if !is_mamiya(tiff) {
        return None;
    }
    tiff.all_ifds().into_iter().find(|i| {
        let Ok(info) = i.image() else { return false };
        let (w, h) = (info.width as u64, info.height as u64);
        i.u16(t::PHOTOMETRIC) == Some(t::photometric::CFA)
            && i.u16(t::COMPRESSION).unwrap_or(1) == 1
            && i.u16(t::BITS_PER_SAMPLE) == Some(12)
            && w > 0
            && w % 2 == 0
            && matches!(info.chunks(len as u64).as_slice(), [c] if Some(c.len) == (w * 3 / 2).checked_mul(h))
    })
}

/// The largest uncompressed 8-bit RGB sub-IFD (the preview): its pixels, width and height.
fn rgb_preview<'a>(tiff: &Tiff, bytes: &'a [u8]) -> Option<(&'a [u8], usize, usize)> {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| {
            let info = i.image().ok()?;
            let ok = i.u16(t::PHOTOMETRIC) == Some(2)
                && i.u16(t::COMPRESSION).unwrap_or(1) == 1
                && i.u16(t::SAMPLES_PER_PIXEL) == Some(3)
                && i.u64s(t::BITS_PER_SAMPLE).is_some_and(|b| b.iter().all(|&v| v == 8));
            let (w, h) = (info.width as usize, info.height as usize);
            let chunks = info.chunks(bytes.len() as u64);
            let [c] = chunks.as_slice() else { return None };
            let px = chunk_bytes(bytes, c)?;
            (ok && px.len() >= w.checked_mul(h)?.checked_mul(3)?).then_some((px, w, h))
        })
        .max_by_key(|(_, w, h)| w * h)
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let raw = raw_ifd(&tiff, bytes.len()).ok_or_else(|| RawError::Unsupported("Mef raw image not found".into()))?;
    let info = raw.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let n = w.checked_mul(h).filter(|n| *n > 0 && *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let cfa = match (raw.u64s(t::CFA_REPEAT_PATTERN_DIM).as_deref(), raw.bytes(t::CFA_PATTERN_EP)) {
        (Some([2, 2]), Some(p)) if p.len() == 4 && p.iter().all(|&c| c <= 2) => Cfa { width: 2, height: 2, pattern: p.to_vec() },
        _ => return Err(RawError::Unsupported("Mef raw without a 2x2 CFAPattern".into())),
    };
    let preview = rgb_preview(&tiff, bytes);
    let crop = match preview {
        Some((_, pw, ph)) => {
            let (cw, ch) = (pw.saturating_mul(PREVIEW_SCALE), ph.saturating_mul(PREVIEW_SCALE));
            if cw > 0 && ch > 0 && CROP_ORIGIN.0 + cw <= w && CROP_ORIGIN.1 + ch <= h {
                Rect::new(CROP_ORIGIN.0, CROP_ORIGIN.1, cw, ch)
            } else {
                Rect::new(0, 0, w, h)
            }
        }
        None => Rect::new(0, 0, w, h),
    };
    // the white balance is measured from the samples, so the header probe reads them too
    let chunks = info.chunks(bytes.len() as u64);
    let src = chunks.first().and_then(|c| chunk_bytes(bytes, c)).ok_or_else(|| RawError::Corrupt("Mef strip outside file".into()))?;
    let mut d = vec![0u16; n];
    crate::unpack::unpack_msb(src, 12, &mut d);
    let white = super::white_from_data(&d, 12);
    let black = BlackLevel::uniform(0.0);
    let frame = crate::wb_estimate::Frame { data: &d, width: w, crop, cfa: &cfa, black: &black, white };
    let wb = match preview {
        Some((px, pw, ph)) => crate::wb_estimate::as_shot_rgb(px, pw, ph, &frame),
        None => crate::wb_estimate::as_shot(bytes, &frame),
    };
    let data = if mode == Mode::Full { RawData::U16(d) } else { RawData::U16(Vec::new()) };
    let mut metadata = lightcraft_meta::from_tiff(&tiff);
    metadata.width = Some(crop.width as u32);
    metadata.height = Some(crop.height as u32);
    let orientation = raw.u16(t::ORIENTATION).or_else(|| tiff.ifds.first().and_then(|i| i.u16(t::ORIENTATION))).unwrap_or(1);
    let img = RawImage {
        format: RawFormat::Mef,
        width: w,
        height: h,
        cpp: 1,
        data,
        cfa: Some(cfa),
        bits: 12,
        black,
        white: vec![white],
        active_area: Rect::new(0, 0, w, h),
        crop,
        orientation: Orientation::from_exif(orientation),
        color: ColorData::default(),
        wb_multipliers: wb,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
        pixel_aspect: 1.0,
    };
    img.validate_for(mode)?;
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    fn pack(px: &[u16]) -> Vec<u8> {
        px.chunks(2).flat_map(|p| [(p[0] >> 4) as u8, ((p[0] & 15) << 4) as u8 | (p[1] >> 8) as u8, p[1] as u8]).collect()
    }

    fn sample(w: u32, h: u32, px: &[u16]) -> Vec<u8> {
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
        raw.set(t::ORIENTATION, Value::Short(vec![6]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![pack(px)] });
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Mamiya-OP Co.,Ltd.".into()));
        ifd0.set(t::MODEL, Value::Ascii("MAMIYA ZD".into()));
        ifd0.add_sub_ifd(raw);
        TiffWriter::new(ByteOrder::Big, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn decodes_packed_12_bit() {
        let px: Vec<u16> = (0..16 * 6).map(|i| (i * 41 % 4096) as u16).collect();
        let f = sample(16, 6, &px);
        assert_eq!(crate::probe(&f), Some(RawFormat::Mef));
        let r = crate::decode(&f).unwrap();
        assert_eq!(r.data, RawData::U16(px));
        assert_eq!(r.orientation, Orientation::from_exif(6));
        assert_eq!(r.crop, Rect::new(0, 0, 16, 6));
        assert_eq!(crate::probe_info(&f).unwrap(), r.info());
    }

    #[test]
    fn malformed_files_never_panic() {
        let px: Vec<u16> = (0..16 * 6).map(|i| (i * 7 % 4096) as u16).collect();
        let f = sample(16, 6, &px);
        for n in 0..f.len() {
            let _ = crate::decode(&f[..n]);
        }
        for i in 0..f.len() {
            for v in [0u8, 0xff] {
                let mut g = f.clone();
                g[i] = v;
                let _ = crate::decode(&g);
                let _ = crate::probe_info(&g);
            }
        }
    }
}
