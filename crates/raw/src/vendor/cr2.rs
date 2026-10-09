//! Canon CR2.
//!
//! Source: Laurent Clévy, "Understanding what is stored in a Canon RAW .CR2 file" (prose sections on the file
//! structure, IFD#3, the `0xc640` slice layout and the maker-note `SensorInfo` table), plus the ExifTool Canon
//! tag-name documentation for maker-note tag meanings. Only the structural description was used.
//!
//! - The raw data is IFD#3's single strip: a lossless-JPEG frame (usually 2 or 4 components).
//! - The decoded sample stream fills vertical slices left to right: `cr2_slice = [n, w, last]` means `n` slices of
//!   width `w` then one of width `last`; each slice is filled top to bottom, row by row.
//! - Colour filter layout: IFD#3 tag `0xc5e0` (`CR2CFAPattern` in the ExifTool EXIF tag-name docs): 1 = RGGB,
//!   2 = BGGR, 3 = GBRG, 4 = GRBG, anchored at the top-left of the full decoded sensor (masked borders included).
//!   It differs by model (e.g. 3 on the 50D/60D/7D/550D/5D Mark II, 1 on the 40D/5D Mark III/6D/5DS R), so it is
//!   read per file (no model table); files without the tag fall back to RGGB (issue #85). The parity of the
//!   `SensorInfo` borders does *not* predict it (RGGB files come with both even and odd top borders).
//! - `SensorInfo` (maker note `0x00e0`): sensor width/height and the left/top/right/bottom borders of the image
//!   area; the masked columns left of it give the black level.
//! - `ColorBalance` (maker note `0x4001`): as-shot `RGGB` levels at a model-dependent offset; we probe the known
//!   offsets and accept the first plausible quadruple. The array is 16-bit words; some models store it as UNDEFINED
//!   bytes (ColorData versions -3 and -4), which are paired up in the maker note's byte order first.
//! - sRAW / mRAW (raw IFD tag `0xc6c5` = 4) hold subsampled YCbCr instead of a mosaic: see [`sraw`].

mod sraw;

use super::white_from_data;
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result, ljpeg};
use lightcraft_geom::Orientation;
use lightcraft_tiff::image::chunk_bytes;
use lightcraft_tiff::{ByteOrder, Ifd, Tiff, Value, makernote, tags as t};

const CR2_SLICE: u16 = 0xc640;

/// The border next to the active sensor can contain illuminated pixels (EOS 760D).
/// A median per CFA site keeps those outliers from raising the pedestal and clipping shadows.
fn black_from_columns(data: &[u16], width: usize, cols: std::ops::Range<usize>, rows: std::ops::Range<usize>, active: Rect) -> BlackLevel {
    if width == 0 {
        return BlackLevel::uniform(0.0);
    }
    let mut samples: [Vec<u16>; 4] = std::array::from_fn(|_| Vec::new());
    for y in rows.start..rows.end.min(data.len() / width) {
        for x in cols.start..cols.end.min(width) {
            let site = (((y & 1) ^ (active.y & 1)) * 2) + ((x & 1) ^ (active.x & 1));
            if let Some(sample) = y.checked_mul(width).and_then(|offset| offset.checked_add(x)).and_then(|offset| data.get(offset))
                && let Some(bucket) = samples.get_mut(site)
            {
                bucket.push(*sample);
            }
        }
    }
    if samples.iter().any(Vec::is_empty) {
        return BlackLevel::uniform(0.0);
    }
    let values = samples
        .iter_mut()
        .map(|bucket| {
            bucket.sort_unstable();
            let upper = bucket.get(bucket.len() / 2).copied().unwrap_or(0);
            let lower = bucket.get((bucket.len() - 1) / 2).copied().unwrap_or(upper);
            (f32::from(lower) + f32::from(upper)) * 0.5
        })
        .collect();
    BlackLevel { repeat_rows: 2, repeat_cols: 2, values, delta_h: Vec::new(), delta_v: Vec::new() }
}
const SRAW_TYPE: u16 = 0xc6c5;
/// `SRAW_TYPE` value of the YCbCr frames of sRAW / mRAW (IFD2's preview image carries 3).
const SRAW_YCC: u32 = 4;
const CR2_CFA_PATTERN: u16 = 0xc5e0;
const SENSOR_INFO: u16 = 0x00e0;
const COLOR_BALANCE: u16 = 0x4001;

fn raw_ifd(tiff: &Tiff) -> Option<&Ifd> {
    tiff.ifds.get(3).or_else(|| tiff.ifds.iter().rev().find(|i| i.contains(CR2_SLICE)))
}

/// Bayer layout from the raw IFD's `CR2CFAPattern` (`0xc5e0`) value; `None` for missing/unknown values.
fn cfa_from_tag(v: Option<u64>) -> Option<Cfa> {
    let name = match v? {
        1 => "RGGB",
        2 => "BGGR",
        3 => "GBRG",
        4 => "GRBG",
        _ => return None,
    };
    Some(Cfa::bayer_static(name))
}

/// The 16-bit words of a maker-note array. Canon writes `ColorData` as SHORT in most models, but as UNDEFINED
/// bytes (count = byte length, e.g. 5120) in the PowerShot / EOS M models with ColorData versions -3 and -4;
/// those bytes are the same 16-bit words in the maker note's byte order, so they are paired up, not read one by
/// one. Other types are returned value by value.
fn words(v: &Value, order: ByteOrder) -> Vec<u64> {
    match v {
        Value::Byte(b) | Value::Undefined(b) => b.as_chunks::<2>().0.iter().map(|c| u64::from(order.u16(*c))).collect(),
        _ => v.to_u64_vec(),
    }
}

/// As-shot WB multipliers (R, G, B; G = 1) from the ColorBalance array.
fn wb_from_color_balance(v: &[u64]) -> Option<[f32; 3]> {
    for off in [63usize, 25, 24, 34, 71, 85, 105, 69, 77] {
        let q = v.get(off..off + 4)?;
        let (r, g1, g2, b) = (q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32);
        if g1 < 256.0 || g2 < 256.0 || r < 64.0 || b < 64.0 || g1 > 16384.0 || (g1 - g2).abs() > 0.05 * g1 {
            continue;
        }
        let g = (g1 + g2) / 2.0;
        let (mr, mb) = (r / g, b / g);
        if (0.25..6.0).contains(&mr) && (0.25..6.0).contains(&mb) {
            return Some([mr, 1.0, mb]);
        }
    }
    None
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = &tiff.ifds[0];
    let raw = raw_ifd(&tiff).ok_or_else(|| RawError::Corrupt("CR2 without raw IFD".into()))?;
    match raw.u32(SRAW_TYPE) {
        Some(SRAW_YCC) => return sraw::decode(bytes, &tiff, raw, mode),
        Some(v) if v != 1 => return Err(RawError::Unsupported(format!("Canon raw type {v}"))),
        _ => {}
    }
    let off = raw.u64(t::STRIP_OFFSETS).ok_or(RawError::Tiff(lightcraft_tiff::TiffError::MissingTag(t::STRIP_OFFSETS)))?;
    let len = raw.u64(t::STRIP_BYTE_COUNTS).unwrap_or(bytes.len() as u64 - off.min(bytes.len() as u64));
    let chunk = lightcraft_tiff::image::Chunk { index: 0, x: 0, y: 0, width: 0, height: 0, plane: 0, offset: off, len };
    let src = chunk_bytes(bytes, &chunk).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
    let (fw, fh, nc, prec) = ljpeg::frame_info(src)?;
    let frame = match mode {
        Mode::Full => Some(ljpeg::decode(src, (fw * fh * nc).min(crate::MAX_SAMPLES))?),
        Mode::Header => None,
    };
    let total = match &frame {
        Some(f) => f.data.len(),
        None => {
            let total = fw
                .checked_mul(fh)
                .and_then(|v| v.checked_mul(nc))
                .filter(|t| *t > 0)
                .ok_or_else(|| RawError::Corrupt("bad CR2 frame size".into()))?;
            if total > crate::MAX_SAMPLES {
                return Err(RawError::Limit("lossless JPEG frame larger than expected"));
            }
            total
        }
    };
    let slices = raw.u64s(CR2_SLICE).filter(|s| s.len() == 3 && s[1] > 0 && s[2] > 0);
    let widths: Vec<usize> = match &slices {
        Some(s) => {
            let n = s[0].min(64) as usize;
            let mut v = vec![s[1] as usize; n];
            v.push(s[2] as usize);
            v
        }
        None => vec![fw * nc],
    };
    let width: usize = widths.iter().sum();
    if width == 0 || total % width != 0 {
        return Err(RawError::Corrupt(format!("CR2 slices ({width}) do not divide the frame ({total} samples)")));
    }
    let height = total / width;
    let mut data = Vec::new();
    if let Some(frame) = &frame {
        data = vec![0u16; total];
        let mut i = 0;
        let mut x0 = 0;
        for &sw in &widths {
            for y in 0..height {
                data[y * width + x0..y * width + x0 + sw].copy_from_slice(&frame.data[i..i + sw]);
                i += sw;
            }
            x0 += sw;
        }
    }
    drop(frame);

    // maker note: sensor borders and white balance
    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let mn =
        tiff.exif().and_then(|e| e.get(t::MAKER_NOTE)).and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make));
    let mut active = Rect::new(0, 0, width, height);
    if let Some(si) = mn.as_ref().and_then(|m| m.ifd.u64s(SENSOR_INFO)).filter(|v| v.len() >= 9) {
        let (l, tp, r, b) = (si[5] as usize, si[6] as usize, si[7] as usize, si[8] as usize);
        if r > l && b > tp && r < width && b < height {
            active = Rect::new(l, tp, r - l + 1, b - tp + 1);
        }
    }
    let wb = mn.as_ref().and_then(|m| m.ifd.value(COLOR_BALANCE).map(|v| words(v, m.order))).and_then(|v| wb_from_color_balance(&v));
    let cfa = cfa_from_tag(raw.u64(CR2_CFA_PATTERN)).unwrap_or_else(|| Cfa::bayer_static("RGGB"));
    let black = if active.x >= 8 {
        black_from_columns(&data, width, 2..active.x - 2, active.y..active.y + active.height, active)
    } else {
        BlackLevel::uniform(0.0)
    };
    let white = white_from_data(&data, prec as u32);
    let mut metadata = lightcraft_meta::from_tiff(&tiff);
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let img = RawImage {
        format: RawFormat::Cr2,
        width,
        height,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: prec as u32,
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    /// Build a synthetic CR2: IFD0..IFD2 placeholders + IFD3 holding a sliced 2-component LJ92 frame.
    pub(crate) fn synthetic_cr2(w: usize, h: usize, slices: Option<[u16; 3]>) -> (Vec<u8>, Vec<u16>) {
        synthetic_cr2_note(w, h, slices, None)
    }

    /// Like [`synthetic_cr2`], with `note` as the Exif maker note (bytes as stored).
    fn synthetic_cr2_note(w: usize, h: usize, slices: Option<[u16; 3]>, note: Option<Vec<u8>>) -> (Vec<u8>, Vec<u16>) {
        let img: Vec<u16> = (0..w * h).map(|i| 1024 + ((i * 7919) % 12000) as u16).collect();
        let widths: Vec<usize> = match slices {
            Some([n, sw, last]) => {
                let mut v = vec![sw as usize; n as usize];
                v.push(last as usize);
                v
            }
            None => vec![w],
        };
        let mut stream = Vec::with_capacity(w * h);
        let mut x0 = 0;
        for &sw in &widths {
            for y in 0..h {
                stream.extend_from_slice(&img[y * w + x0..y * w + x0 + sw]);
            }
            x0 += sw;
        }
        let enc = crate::ljpeg::encode(&stream, w / 2, h, 2, 14, 1, 0);
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Canon".into()));
        ifd0.set(t::MODEL, Value::Ascii("Canon EOS Test".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![8]));
        // maker note: SensorInfo + ColorBalance, plain IFD with offsets relative to the TIFF header
        let mut exif = IfdBuilder::new();
        exif.set(t::ISO_SPEED, Value::Short(vec![400]));
        if let Some(n) = note {
            exif.set(t::MAKER_NOTE, Value::Undefined(n));
        }
        ifd0.set_child(t::EXIF_IFD, exif);
        let mut ifd3 = IfdBuilder::new();
        ifd3.set(t::COMPRESSION, Value::Short(vec![6]));
        ifd3.set(CR2_CFA_PATTERN, Value::Long(vec![3]));
        if let Some(s) = slices {
            ifd3.set(CR2_SLICE, Value::Short(s.to_vec()));
        }
        ifd3.set_image(ImageData::Strips { rows_per_strip: h as u32, strips: vec![enc] });
        // (real files also carry a "CR\x02\0" signature at offset 8; probe recognises the structure without it)
        let bytes = TiffWriter::new(ByteOrder::Little, false)
            .write(&[ifd0, IfdBuilder::new().with(1, Value::Short(vec![0])), IfdBuilder::new().with(1, Value::Short(vec![0])), ifd3])
            .unwrap();
        (bytes, img)
    }

    #[test]
    fn synthetic_roundtrip() {
        for slices in [None, Some([2u16, 24, 16]), Some([1, 40, 24])] {
            let (bytes, img) = synthetic_cr2(64, 10, slices);
            assert_eq!(crate::probe(&bytes), Some(RawFormat::Cr2));
            let r = crate::decode(&bytes).unwrap();
            assert_eq!((r.width, r.height), (64, 10));
            assert_eq!(crate::probe_info(&bytes).unwrap(), r.info());
            assert_eq!(r.data, RawData::U16(img));
            assert_eq!(r.orientation, Orientation::Rotate270);
            assert_eq!(r.cfa.as_ref().map(Cfa::name).as_deref(), Some("GBRG"), "CR2CFAPattern 3");
            assert_eq!(r.metadata.iso, Some(400));
            assert!(r.develop(crate::Method::Ahd).is_ok());
        }
    }

    #[test]
    fn cfa_pattern_tag() {
        let name = |v| cfa_from_tag(v).map(|c| c.name());
        assert_eq!(name(Some(1)).as_deref(), Some("RGGB"));
        assert_eq!(name(Some(2)).as_deref(), Some("BGGR"));
        assert_eq!(name(Some(3)).as_deref(), Some("GBRG"));
        assert_eq!(name(Some(4)).as_deref(), Some("GRBG"));
        assert_eq!(name(Some(0)), None);
        assert_eq!(name(Some(7)), None);
        assert_eq!(name(None), None);
    }

    #[test]
    fn illuminated_border_does_not_crush_shadows() {
        let width = 100;
        let active = Rect::new(85, 3, 15, 7);
        let pedestal = [2040u16, 2048, 2052, 2044];
        let mut data = vec![0u16; width * 10];
        for y in 0..10 {
            for x in 0..width {
                let site = (((y & 1) ^ (active.y & 1)) * 2) + ((x & 1) ^ (active.x & 1));
                data[y * width + x] = pedestal[site] + if x >= 60 { 4000 } else { 0 };
            }
        }
        let black = black_from_columns(&data, width, 2..83, 3..10, active);
        assert_eq!(black.values, pedestal.map(f32::from));
        // Shadows just above the true pedestal must stay positive after subtraction.
        for (measured, expected) in black.values.iter().zip(pedestal) {
            assert!(f32::from(expected + 10) - measured > 0.0);
        }
        assert_eq!(black_from_columns(&[], 0, 0..1, 0..1, active).values, vec![0.0]);
        assert_eq!(black_from_columns(&data, width, 0..0, 0..10, active).values, vec![0.0]);
    }

    #[test]
    fn wb_probe() {
        let mut v = vec![0u64; 120];
        v[63..67].copy_from_slice(&[2000, 1024, 1026, 1500]);
        let wb = wb_from_color_balance(&v).unwrap();
        assert!((wb[0] - 2000.0 / 1025.0).abs() < 1e-4 && (wb[2] - 1500.0 / 1025.0).abs() < 1e-4);
        assert!(wb_from_color_balance(&[0; 10]).is_none());
    }

    #[test]
    fn wb_from_undefined_bytes() {
        // hand-coded: ColorData of an S110-like file, UNDEFINED bytes, little-endian words; word 71.. = 1988 745 745 1641
        let mut b = vec![0u8; 2 * 120];
        b[0..2].copy_from_slice(&[0xfd, 0xff]); // version -3
        b[142..150].copy_from_slice(&[0xc4, 0x07, 0xe9, 0x02, 0xe9, 0x02, 0x69, 0x06]);
        let le = words(&Value::Undefined(b.clone()), ByteOrder::Little);
        assert_eq!(&le[71..75], &[1988, 745, 745, 1641]);
        let wb = wb_from_color_balance(&le).unwrap();
        assert!((wb[0] - 1988.0 / 745.0).abs() < 1e-5 && wb[1] == 1.0 && (wb[2] - 1641.0 / 745.0).abs() < 1e-5);
        // read as single bytes (the old behaviour) nothing is plausible
        assert!(wb_from_color_balance(&b.iter().map(|&x| u64::from(x)).collect::<Vec<_>>()).is_none());
        // big-endian note: same words, bytes swapped
        let mut be = b.clone();
        be.as_chunks_mut::<2>().0.iter_mut().for_each(|c| c.swap(0, 1));
        assert_eq!(words(&Value::Undefined(be), ByteOrder::Big), le);
        // SHORT values pass through, an odd trailing byte is ignored
        assert_eq!(words(&Value::Short(vec![1, 2, 3]), ByteOrder::Little), vec![1, 2, 3]);
        assert_eq!(words(&Value::Undefined(vec![1, 0, 2]), ByteOrder::Little), vec![1]);
    }

    /// A maker note holding only `ColorData` (`0x4001`) as `ty` (3 = SHORT, 7 = UNDEFINED) with `n` values, a plain
    /// IFD whose value offset is relative to the TIFF header (patched once the note's position is known).
    fn note_with_color_data(ty: u16, data: &[u8]) -> Vec<u8> {
        let count = if ty == 3 { data.len() / 2 } else { data.len() } as u32;
        let mut note = Vec::new();
        note.extend_from_slice(&1u16.to_le_bytes());
        note.extend_from_slice(&COLOR_BALANCE.to_le_bytes());
        note.extend_from_slice(&ty.to_le_bytes());
        note.extend_from_slice(&count.to_le_bytes());
        note.extend_from_slice(&0u32.to_le_bytes()); // value offset, patched below
        note.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        note.extend_from_slice(data);
        note
    }

    fn decode_with_color_data(ty: u16, data: &[u8]) -> RawImage {
        let (mut bytes, _) = synthetic_cr2_note(64, 10, None, Some(note_with_color_data(ty, data)));
        let tiff = Tiff::parse(&bytes).unwrap();
        let e = tiff.exif().unwrap().get(t::MAKER_NOTE).unwrap();
        let off = e.offset as usize;
        bytes[off + 10..off + 14].copy_from_slice(&(e.offset as u32 + 18).to_le_bytes());
        crate::decode(&bytes).unwrap()
    }

    /// `ColorData` stored as UNDEFINED bytes (versions -3 / -4: PowerShot, EOS M) gives the as-shot WB from word 71,
    /// exactly as the same words stored as SHORT do.
    #[test]
    fn color_data_stored_as_bytes() {
        let mut bytes = vec![0u8; 2 * 120];
        bytes[0..2].copy_from_slice(&[0xfd, 0xff]); // version -3
        bytes[142..150].copy_from_slice(&[0xc4, 0x07, 0xe9, 0x02, 0xe9, 0x02, 0x69, 0x06]); // 1988 745 745 1641
        for ty in [7, 3] {
            let wb = decode_with_color_data(ty, &bytes).wb_multipliers.unwrap_or_else(|| panic!("type {ty}: no WB"));
            assert!((wb[0] - 1988.0 / 745.0).abs() < 1e-5 && wb[1] == 1.0 && (wb[2] - 1641.0 / 745.0).abs() < 1e-5, "type {ty}: {wb:?}");
        }
    }
}
