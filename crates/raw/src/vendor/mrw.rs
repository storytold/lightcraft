//! Minolta / Konica Minolta MRW: a big-endian block container (`\0MRM`) followed by the uncompressed mosaic.
//!
//! Sources: the publicly documented MRW block layout (Dalibor Jelinek's "MRW format" notes and the ExifTool tag
//! tables, prose only), and our own black-box analysis of three CC0 samples from raw.pixls.us (DiMAGE 7Hi, DiMAGE A2,
//! Dynax 7D). No decoder source was copied.
//!
//! - Header: `\0MRM`, then a big-endian u32 giving the length of the block area; the mosaic starts right after it
//!   (offset 8 + length). Each block is a 4-byte tag and a big-endian u32 length.
//! - `\0PRD` (picture-raw dimensions): an 8-byte version string, sensor rows and columns (u16 each), image rows and
//!   columns, the stored bits per sample (16 or 12), the significant bits (12), the storage method (`0x52`
//!   unpacked big-endian 16-bit words, `0x59` 12-bit MSB-first packing), two reserved bytes and the Bayer layout
//!   (u16: 1 = RGGB, 4 = GBRG). The mosaic holds sensor rows x sensor columns samples; the image is its top-left
//!   corner (the extra columns and rows on the right and bottom are not picture).
//! - `\0WBG`: four scale bytes, then the as-shot white-balance coefficients as u16 in the order of the 2x2 layout
//!   (R G G B in the RGGB files, G B R G in the GBRG DiMAGE A200); the gains are each colour's coefficient over
//!   green. On the DiMAGE 7Hi and A2 they match the neutral clouds of the camera JPEG within 1%.
//! - `\0TTW`: a complete big-endian TIFF (offsets relative to the block) with Exif, orientation and the thumbnail.
//! - Levels: the samples are 12-bit with no pedestal (the darkest samples of each image sit at 0), white 4095.
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lcc_geom::Orientation;
use lcc_tiff::{Tiff, tags as t};

/// The parsed block area of an MRW file.
#[derive(Debug, PartialEq)]
pub(crate) struct Header {
    pub sensor_h: usize,
    pub sensor_w: usize,
    pub image_h: usize,
    pub image_w: usize,
    /// Storage: true for the 12-bit packing, false for 16-bit words.
    pub packed: bool,
    pub cfa: &'static str,
    /// White-balance coefficients R, G, G, B.
    pub wbg: Option<[u16; 4]>,
    /// The `\0TTW` block's bytes.
    pub ttw: Option<(usize, usize)>,
    /// Offset of the mosaic.
    pub data: usize,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

/// Walk the blocks after the `\0MRM` header.
pub(crate) fn header(bytes: &[u8]) -> Result<Header> {
    let corrupt = |m: &str| RawError::Corrupt(format!("MRW: {m}"));
    if !bytes.starts_with(b"\0MRM") {
        return Err(RawError::NotRaw);
    }
    let len = be32(bytes, 4).ok_or_else(|| corrupt("truncated header"))? as usize;
    let data = len.checked_add(8).filter(|&d| d <= bytes.len()).ok_or_else(|| corrupt("block area past the end of the file"))?;
    let mut prd = None;
    let (mut wbg, mut ttw) = (None, None);
    let mut p = 8usize;
    // each block takes at least 8 bytes, so this ends; the count cap is a second guard
    for _ in 0..1024 {
        if p.saturating_add(8) > data {
            break;
        }
        let tag = bytes.get(p..p + 4).ok_or_else(|| corrupt("truncated block"))?;
        let blen = be32(bytes, p + 4).ok_or_else(|| corrupt("truncated block"))? as usize;
        let start = p + 8;
        let end = start.checked_add(blen).filter(|&e| e <= data).ok_or_else(|| corrupt("block past the block area"))?;
        let body = bytes.get(start..end).ok_or_else(|| corrupt("truncated block"))?;
        match tag {
            b"\0PRD" if body.len() >= 24 => prd = Some(body),
            b"\0WBG" if body.len() >= 12 => {
                wbg = Some([be16(body, 4), be16(body, 6), be16(body, 8), be16(body, 10)].map(|v| v.unwrap_or(0)));
            }
            b"\0TTW" => ttw = Some((start, end)),
            _ => {}
        }
        p = end;
    }
    let prd = prd.ok_or_else(|| corrupt("no PRD block"))?;
    let g = |at: usize| be16(prd, at).map(usize::from).unwrap_or(0);
    let (sensor_h, sensor_w, image_h, image_w) = (g(8), g(10), g(12), g(14));
    let packed = match (prd.get(16), prd.get(18)) {
        (Some(12), Some(0x59)) => true,
        (Some(16), Some(0x52)) => false,
        (d, s) => return Err(RawError::Unsupported(format!("MRW storage {d:?}/{s:?} is not decoded"))),
    };
    let cfa = match be16(prd, 22) {
        Some(1) => "RGGB",
        Some(4) => "GBRG",
        other => return Err(RawError::Unsupported(format!("MRW Bayer layout {other:?}"))),
    };
    if sensor_w == 0 || sensor_h == 0 || image_w == 0 || image_h == 0 || image_w > sensor_w || image_h > sensor_h {
        return Err(corrupt("bad dimensions"));
    }
    if packed && sensor_w % 2 != 0 {
        return Err(corrupt("odd row length in 12-bit packing"));
    }
    Ok(Header { sensor_h, sensor_w, image_h, image_w, packed, cfa, wbg, ttw, data })
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let h = header(bytes)?;
    let (w, ht) = (h.sensor_w, h.sensor_h);
    let n = w.checked_mul(ht).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let need = if h.packed { n / 2 * 3 } else { n * 2 };
    let src = bytes.get(h.data..).and_then(|s| s.get(..need)).ok_or_else(|| RawError::Corrupt("MRW mosaic truncated".into()))?;
    let data = if mode == Mode::Full {
        let mut out = vec![0u16; n];
        if h.packed {
            crate::unpack::unpack_msb(src, 12, &mut out);
        } else {
            for (o, c) in out.iter_mut().zip(src.as_chunks::<2>().0) {
                *o = u16::from_be_bytes([c[0], c[1]]) & 0x0fff;
            }
        }
        RawData::U16(out)
    } else {
        RawData::U16(Vec::new())
    };
    let tiff = h.ttw.and_then(|(s, e)| bytes.get(s..e)).and_then(|b| Tiff::parse(b).ok());
    let mut metadata = tiff.as_ref().map(lcc_meta::from_tiff).unwrap_or_default();
    metadata.width = Some(h.image_w as u32);
    metadata.height = Some(h.image_h as u32);
    let orientation = tiff.as_ref().and_then(|t| t.ifds.first()).and_then(|i| i.u16(t::ORIENTATION)).unwrap_or(1);
    let cfa = Cfa::bayer_static(h.cfa);
    // the coefficients follow the 2x2 layout's order (R G G B for RGGB, G B R G for GBRG)
    let wb = h.wbg.filter(|c| c.iter().all(|&v| v > 0)).map(|c| {
        let (mut sum, mut n) = ([0f32; 3], [0f32; 3]);
        for (&v, &col) in c.iter().zip(&cfa.pattern) {
            let k = usize::from(col.min(2));
            sum[k] += f32::from(v);
            n[k] += 1.0;
        }
        let g = sum[1] / n[1].max(1.0);
        [sum[0] / n[0].max(1.0) / g, 1.0, sum[2] / n[2].max(1.0) / g]
    });
    let img = RawImage {
        format: RawFormat::Mrw,
        width: w,
        height: ht,
        cpp: 1,
        data,
        cfa: Some(cfa),
        bits: 12,
        black: BlackLevel::uniform(0.0),
        white: vec![4095.0],
        active_area: Rect::new(0, 0, h.image_w, h.image_h),
        crop: Rect::new(0, 0, h.image_w, h.image_h),
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

    /// A minimal MRW: PRD + WBG blocks, then the mosaic.
    pub(crate) fn synth(sw: u16, sh: u16, iw: u16, ih: u16, packed: bool, bayer: u16, mosaic: &[u16]) -> Vec<u8> {
        let mut prd = b"21810002".to_vec();
        for v in [sh, sw, ih, iw] {
            prd.extend_from_slice(&v.to_be_bytes());
        }
        prd.extend_from_slice(&[if packed { 12 } else { 16 }, 12, if packed { 0x59 } else { 0x52 }, 0, 0, 0]);
        prd.extend_from_slice(&bayer.to_be_bytes());
        let mut blocks = Vec::new();
        blocks.extend_from_slice(b"\0PRD");
        blocks.extend_from_slice(&(prd.len() as u32).to_be_bytes());
        blocks.extend_from_slice(&prd);
        blocks.extend_from_slice(b"\0WBG\0\0\0\x0c\x02\x02\x02\x02");
        for v in [512u16, 256, 256, 384] {
            blocks.extend_from_slice(&v.to_be_bytes());
        }
        let mut f = b"\0MRM".to_vec();
        f.extend_from_slice(&(blocks.len() as u32).to_be_bytes());
        f.extend_from_slice(&blocks);
        if packed {
            for p in mosaic.chunks(2) {
                let (a, b) = (p[0], p.get(1).copied().unwrap_or(0));
                f.extend_from_slice(&[(a >> 4) as u8, (((a & 15) << 4) | (b >> 8)) as u8, b as u8]);
            }
        } else {
            for v in mosaic {
                f.extend_from_slice(&v.to_be_bytes());
            }
        }
        f
    }

    #[test]
    fn decodes_both_storages() {
        let mosaic: Vec<u16> = (0..6 * 4).map(|i| (i * 170) as u16 & 0xfff).collect();
        for packed in [true, false] {
            let f = synth(6, 4, 4, 2, packed, 4, &mosaic);
            assert_eq!(crate::probe(&f), Some(RawFormat::Mrw));
            let img = crate::decode(&f).unwrap();
            assert_eq!((img.width, img.height), (6, 4));
            assert_eq!((img.active_area.width, img.active_area.height), (4, 2));
            assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some("GBRG"));
            assert_eq!(img.data, RawData::U16(mosaic.clone()));
            // GBRG: the coefficients 512, 256, 256, 384 are G, B, R, G
            assert_eq!(img.wb_multipliers, Some([256.0 / 448.0, 1.0, 256.0 / 448.0]));
            assert_eq!(crate::probe_info(&f).unwrap(), img.info());
        }
    }

    #[test]
    fn hostile_files_are_errors() {
        let mosaic = vec![7u16; 24];
        let good = synth(6, 4, 4, 2, true, 1, &mosaic);
        // every truncation
        for cut in 0..good.len() {
            let _ = crate::decode(&good[..cut]);
            let _ = crate::probe_info(&good[..cut]);
        }
        // block area length past the end, block length past the area
        let mut f = good.clone();
        f[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(crate::decode(&f).is_err());
        let mut f = good.clone();
        f[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(crate::decode(&f).is_err());
        // image bigger than the sensor, zero size, huge sensor, unknown storage / layout
        assert!(crate::decode(&synth(6, 4, 8, 2, true, 1, &mosaic)).is_err());
        assert!(crate::decode(&synth(0, 0, 0, 0, true, 1, &[])).is_err());
        assert!(crate::decode(&synth(65535, 65535, 2, 2, true, 1, &mosaic)).is_err());
        assert!(crate::decode(&synth(5, 4, 4, 2, true, 1, &mosaic)).is_err());
        assert!(crate::decode(&synth(6, 4, 4, 2, true, 3, &mosaic)).is_err());
        let mut f = good.clone();
        f[8 + 8 + 18] = 0x7f;
        assert!(matches!(crate::decode(&f), Err(RawError::Unsupported(_))));
        // every single-byte corruption of the header
        for i in 0..good.len().min(80) {
            for v in [0u8, 0xff, 0x80] {
                let mut f = good.clone();
                f[i] = v;
                let _ = crate::decode(&f);
            }
        }
    }
}
