//! Olympus ORF — words, packed samples and the measured compressed 12-bit profile.
//!
//! Sources: TIFF 6.0 (the `IIRO`/`MMOR` container is a TIFF with a different magic number), the ExifTool
//! Olympus tag-name documentation (maker-note sub-directories `0x2020` CameraSettings — `0x0101/0x0102`
//! PreviewImageStart/Length — and `0x2040` ImageProcessing — `0x0100` WB_RBLevels, `0x0600` BlackLevel2,
//! `0x0612–0x0615` CropLeft/Top/Width/Height) and our own black-box analysis of CC0 samples from raw.pixls.us
//! (E-1, E-400, XZ-2):
//!
//! - 16 bits per sample: little-endian words; some bodies (E-1, E-400) store 12-bit values in the top bits (the low
//!   four bits zero in more than 99% of samples), which we shift down.
//! - 12-bit packed (XZ-2): each row is a sequence of little-endian 32-bit words read MSB-first (found by testing
//!   candidate bit orders for the smoothest image).
//! - The measured E01/V02 compressed 12-bit profile is decoded after checking the observed metadata
//!   fingerprint and strip bounds. Other profiles remain unsupported; embedded previews still work.
//!   See `docs/raw/orf12-compressed-measured.md` and `orf12-review.md` for the separate specification review.
//! - Exif `CFAPattern` (`0xa302`, Exif 2.32) gives the per-file Bayer layout. Without a valid tag, block means
//!   estimate only the green diagonal; the red/blue assignment remains the historical GRBG/RGGB fallback.
//! - E-M5 II High Res Shot: ten 12-bit samples in 16 bytes, five little-endian 3-byte pairs and a zero pad byte.
//!   Independently established from strip size, padding and competing nibble layouts, then checked against
//!   64,328,960 reference sensor samples. See `docs/raw/orf12-format.md` and `orf12-provenance.md`.

use super::{olympus12, white_from_data};
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

/// The colour-filter layout from the Exif `CFAPattern` tag (`0xa302`: two 16-bit repeat counts, found in either
/// byte order, then one byte per site: 0 = red, 1 = green, 2 = blue), when it describes a 2×2 Bayer cell.
fn cfa_from_exif(tiff: &Tiff) -> Option<Cfa> {
    let &[c0, c1, r0, r1, s0, s1, s2, s3] = tiff.exif()?.bytes(EXIF_CFA_PATTERN)? else { return None };
    let two = |a: u8, b: u8| matches!((a, b), (2, 0) | (0, 2));
    let name = match [s0, s1, s2, s3] {
        [0, 1, 1, 2] => "RGGB",
        [2, 1, 1, 0] => "BGGR",
        [1, 0, 2, 1] => "GRBG",
        [1, 2, 0, 1] => "GBRG",
        _ => return None,
    };
    (two(c0, c1) && two(r0, r1)).then(|| Cfa::bayer_static(name))
}

/// The layout found from the samples, for files without the Exif tag: GRBG when the greens sit on the main
/// diagonal of the 2×2 cell at the sensor origin, else RGGB. Over 32×32-pixel blocks of `a` (every fourth in both
/// directions) it compares the block totals of the two sites on each diagonal: the two greens of a block see the
/// same light, red and blue rarely do. Totals rather than single pixels, so that fine texture, which makes
/// neighbouring greens differ, doesn't outweigh a small difference between red and blue.
pub(crate) fn cfa_from_data(d: &[u16], w: usize, a: Rect) -> Cfa {
    const BLOCK: usize = 32;
    let (x0, y0) = (a.x.saturating_add(1) & !1, a.y.saturating_add(1) & !1);
    let across = a.x.saturating_add(a.width).min(w).saturating_sub(x0) / BLOCK;
    let down = a.y.saturating_add(a.height).saturating_sub(y0) / BLOCK;
    let (mut main, mut anti) = (0u64, 0u64);
    for by in (0..down).step_by(4) {
        for bx in (0..across).step_by(4) {
            let mut sums = [0i64; 4];
            for y in 0..BLOCK {
                let row = (y0 + by * BLOCK + y).checked_mul(w).and_then(|r| r.checked_add(x0 + bx * BLOCK));
                // rows past the end of the samples: nothing more to read
                let Some(row) = row.and_then(|start| d.get(start..start.checked_add(BLOCK)?)) else { break };
                for (x, &v) in row.iter().enumerate() {
                    sums[(y & 1) * 2 + (x & 1)] += v as i64;
                }
            }
            let [s0, s1, s2, s3] = sums;
            main += (s0 - s3).unsigned_abs();
            anti += (s1 - s2).unsigned_abs();
        }
    }
    Cfa::bayer_static(if main < anti { "GRBG" } else { "RGGB" })
}

/// Five little-endian 12-bit pairs followed by a zero padding byte (ten samples in 16 bytes).
fn unpack_padded_pairs(src: &[u8], row: &mut [u16]) -> Result<()> {
    if !row.len().is_multiple_of(10) || src.len() != row.len() / 10 * 16 {
        return Err(RawError::Corrupt("ORF padded row length mismatch".into()));
    }
    for (group, out) in src.as_chunks::<16>().0.iter().zip(row.as_chunks_mut::<10>().0.iter_mut()) {
        let payload = group.get(..15).ok_or_else(|| RawError::Corrupt("short ORF packed group".into()))?;
        if group.get(15) != Some(&0) {
            return Err(RawError::Unsupported("ORF padded packing with nonzero padding".into()));
        }
        for (pair, target) in payload.as_chunks::<3>().0.iter().zip(out.as_chunks_mut::<2>().0.iter_mut()) {
            let [a, b, c] = pair;
            let [first, second] = target;
            *first = u16::from(*a) | (u16::from(*b & 15) << 8);
            *second = u16::from(*b >> 4) | (u16::from(*c) << 4);
        }
    }
    Ok(())
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    #[cfg(not(target_arch = "wasm32"))]
    let profile_start = std::env::var_os("LIGHTCRAFT_PROFILE").is_some().then(std::time::Instant::now);
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = tiff.ifds.first().ok_or_else(|| RawError::Corrupt("ORF without IFD0".into()))?;
    let info = ifd0.image()?;
    if info.samples_per_pixel != 1 || info.sample_format != 1 || ifd0.u64(t::SAMPLES_PER_PIXEL).is_some_and(|v| v != 1) {
        return Err(RawError::Unsupported("ORF without a single unsigned sensor sample per pixel".into()));
    }
    let (w, h) = (info.width as usize, info.height as usize);
    let n = w.checked_mul(h).filter(|n| *n > 0 && *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let chunks = info.chunks(bytes.len() as u64);
    let total = chunks.iter().try_fold(0u64, |total, c| {
        let end = c.offset.checked_add(c.len).filter(|end| *end <= bytes.len() as u64);
        if end.is_none() || c.len == 0 {
            return Err(RawError::Corrupt("ORF strip outside file".into()));
        }
        total.checked_add(c.len).ok_or_else(|| RawError::Corrupt("ORF strip lengths overflow".into()))
    })?;
    let stored_bits = total.checked_mul(8).ok_or_else(|| RawError::Corrupt("ORF stored bit count overflow".into()))?;
    let tagged_cfa = cfa_from_exif(&tiff);
    let mn = maker_note(bytes, &tiff);
    let ip = mn.as_ref().and_then(|m| sub_ifd(bytes, m, IMAGE_PROCESSING));
    let compressed_profile = ip.as_ref().is_some_and(olympus12::matches_profile);
    if !compressed_profile && ip.as_ref().is_some_and(olympus12::has_coding_fields) {
        return Err(RawError::Unsupported("unverified Olympus coding profile".into()));
    }
    let packed_header = mode == Mode::Header && tagged_cfa.is_some();
    let (mut data, bits) = if info.compression != 1 {
        return Err(RawError::Unsupported(format!("ORF compression {}", info.compression)));
    } else if compressed_profile {
        if tiff.order != lightcraft_tiff::ByteOrder::Little
            || info.bits_per_sample.as_slice() != [16]
            || info.planar != 1
            || info.predictor != 1
            || tagged_cfa.is_none()
            || info.offsets.len() != 1
            || info.byte_counts.len() != 1
            || info.byte_counts.first().is_none_or(|&count| count == 0)
            || chunks.len() != 1
            || !matches!(info.layout, lightcraft_tiff::image::Layout::Strips { rows_per_strip } if rows_per_strip as usize == h)
        {
            return Err(RawError::Unsupported("unverified compressed ORF container layout or Bayer metadata".into()));
        }
        let chunk = chunks.first().ok_or_else(|| RawError::Corrupt("ORF without strip".into()))?;
        let src = chunk_bytes(bytes, chunk).ok_or_else(|| RawError::Corrupt("ORF strip outside file".into()))?;
        let d = if mode == Mode::Header {
            olympus12::validate(src, w, h)?;
            Vec::new()
        } else {
            olympus12::decode(src, w, h)?
        };
        (RawData::U16(d), 12)
    } else if total >= (n as u64) * 2 {
        let d = read_image(bytes, &info, tiff.order, Packing::Word16)?;
        (d, 16)
    } else if stored_bits >= (n as u64) * 12 && stored_bits < (n as u64) * 13 && chunks.len() == 1 {
        let chunk = chunks.first().ok_or_else(|| RawError::Corrupt("ORF without strip".into()))?;
        let src = chunk_bytes(bytes, chunk).ok_or_else(|| RawError::Corrupt("ORF strip outside file".into()))?;
        if !src.len().is_multiple_of(h) {
            return Err(RawError::Corrupt("ORF rows do not divide strip length".into()));
        }
        let stride = src.len() / h;
        let padded_pairs = w.is_multiple_of(10) && stride == w / 10 * 16;
        if padded_pairs
            && (tiff.order != lightcraft_tiff::ByteOrder::Little
                || !matches!(info.layout, lightcraft_tiff::image::Layout::Strips { rows_per_strip } if rows_per_strip as usize == h))
        {
            return Err(RawError::Unsupported("ORF padded packing not verified for this byte order or strip layout".into()));
        }
        if padded_pairs && src.as_chunks::<16>().0.iter().any(|g| g.get(15) != Some(&0)) {
            return Err(RawError::Unsupported("ORF padded packing with nonzero padding".into()));
        }
        let mut d = if packed_header { Vec::new() } else { vec![0u16; n] };
        d.par_chunks_mut(w).enumerate().try_for_each(|(y, row)| -> Result<()> {
            let start = y.checked_mul(stride).ok_or_else(|| RawError::Corrupt("ORF row offset overflow".into()))?;
            let end = start.checked_add(stride).ok_or_else(|| RawError::Corrupt("ORF row end overflow".into()))?;
            let src = src.get(start..end).ok_or_else(|| RawError::Corrupt("short ORF row".into()))?;
            if padded_pairs {
                unpack_padded_pairs(src, row)?
            } else {
                unpack_row_le32_msb(src, 12, row)
            }
            Ok(())
        })?;
        (RawData::U16(d), 12)
    } else {
        return Err(RawError::Unsupported("Olympus compressed ORF".into()));
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

    let active = match ip.as_ref().map(|i| CROP.map(|tag| i.u64(tag).and_then(|v| usize::try_from(v).ok()))) {
        Some([Some(x), Some(y), Some(cw), Some(ch)])
            if cw > 0 && ch > 0 && x.checked_add(cw).is_some_and(|r| r <= w) && y.checked_add(ch).is_some_and(|b| b <= h) =>
        {
            Rect::new(x, y, cw, ch)
        }
        _ => Rect::new(0, 0, w, h),
    };
    let cfa = tagged_cfa.unwrap_or_else(|| cfa_from_data(samples, w, active));
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
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(start) = profile_start {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "[profile] ORF sensor {w}x{h} {mode:?}: {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
    }
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

    fn orf_with_exif(w: u32, h: u32, bits: u16, strip: Vec<u8>, order: ByteOrder, exif: Option<IfdBuilder>) -> Vec<u8> {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![1]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        ifd.set(t::SAMPLE_FORMAT, Value::Short(vec![1]));
        ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        ifd.set(t::PREDICTOR, Value::Short(vec![1]));
        ifd.set(t::MAKE, Value::Ascii("OLYMPUS IMAGING CORP.".into()));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
        if let Some(exif) = exif {
            ifd.set_child(t::EXIF_IFD, exif);
        }
        let mut b = TiffWriter::new(order, false).write(&[ifd]).unwrap();
        b[..4].copy_from_slice(if order == ByteOrder::Little { b"IIRO" } else { b"MMOR" });
        b
    }

    fn orf(w: u32, h: u32, bits: u16, strip: Vec<u8>) -> Vec<u8> {
        orf_with_exif(w, h, bits, strip, ByteOrder::Little, None)
    }

    fn exif_cfa(order: ByteOrder, pattern: &[u8]) -> IfdBuilder {
        let mut v = Vec::new();
        order.put_u16(&mut v, 2);
        order.put_u16(&mut v, 2);
        v.extend_from_slice(pattern);
        IfdBuilder::new().with(EXIF_CFA_PATTERN, Value::Undefined(v))
    }

    fn orf_with(w: u32, h: u32, bits: u16, strip: Vec<u8>, pattern: Option<&[u8]>) -> Vec<u8> {
        let exif = pattern.map(|p| IfdBuilder::new().with(EXIF_CFA_PATTERN, Value::Undefined(p.to_vec())));
        orf_with_exif(w, h, bits, strip, ByteOrder::Little, exif)
    }

    /// A 12-bit mosaic stored as 16-bit words: `site(cx, cy)` gives the four values of the 2×2 cell at (cx, cy).
    fn mosaic(w: usize, h: usize, site: impl Fn(usize, usize) -> [u16; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|i| site(i % w / 2, i / w / 2)[((i / w) & 1) * 2 + ((i % w) & 1)].to_le_bytes()).collect()
    }

    /// Smooth scene texture shared by the four sites of a cell.
    fn shade(cx: usize, cy: usize) -> u16 {
        ((cx * 7 + cy * 13) % 23) as u16
    }

    const BGGR: &[u8] = &[2, 0, 2, 0, 2, 1, 1, 0];
    const GRBG: &[u8] = &[2, 0, 2, 0, 1, 0, 2, 1];
    // repeat counts in the other byte order
    const RGGB: &[u8] = &[0, 2, 0, 2, 0, 1, 1, 2];
    const GBRG: &[u8] = &[0, 2, 0, 2, 1, 2, 0, 1];

    fn layout(bytes: &[u8]) -> String {
        let r = crate::decode(bytes).unwrap();
        assert_eq!(crate::probe_info(bytes).unwrap(), r.info());
        r.cfa.unwrap().name()
    }

    #[test]
    fn exif_cfa_pattern_states_the_layout() {
        let (w, h) = (160usize, 144usize);
        // samples whose green diagonal alone would say RGGB
        let px = mosaic(w, h, |cx, cy| [600, 1000, 1000, 300].map(|v| v + shade(cx, cy)));
        for (tag, name) in [(BGGR, "BGGR"), (GRBG, "GRBG"), (RGGB, "RGGB"), (GBRG, "GBRG")] {
            assert_eq!(layout(&orf_with(w as u32, h as u32, 16, px.clone(), Some(tag))), name);
        }
        // not a 2×2 Bayer cell (four greens, greens side by side, a fourth colour), other repeat counts, wrong
        // lengths: the samples decide
        let unusable: [&[u8]; 8] = [
            &[2, 0, 2, 0, 1, 1, 1, 1],
            &[2, 0, 2, 0, 1, 1, 0, 2],
            &[2, 0, 2, 0, 0, 1, 1, 3],
            &[3, 0, 3, 0, 0, 1, 1, 2],
            &[2, 2, 2, 0, 0, 1, 1, 2],
            &[2, 0, 2, 0, 1, 0],
            &[2, 0, 2, 0, 2, 1, 1, 0, 0],
            &[],
        ];
        for (site, name) in [([600, 1000, 1000, 300], "RGGB"), ([1000, 600, 300, 1000], "GRBG")] {
            let px = mosaic(w, h, |cx, cy| site.map(|v| v + shade(cx, cy)));
            assert_eq!(layout(&orf(w as u32, h as u32, 16, px.clone())), name);
            for tag in unusable {
                assert_eq!(layout(&orf_with(w as u32, h as u32, 16, px.clone(), Some(tag))), name, "{tag:?}");
            }
        }
    }

    #[test]
    fn packed_12_bit_probe_agrees_with_decode() {
        let (w, h) = (16usize, 4usize);
        let strip: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i * 37 % 251) as u8).collect();
        for tag in [Some(BGGR), None] {
            let bytes = orf_with(w as u32, h as u32, 12, strip.clone(), tag);
            let r = crate::decode(&bytes).unwrap();
            assert_eq!((r.bits, r.data.len()), (12, w * h));
            assert_eq!(r.cfa.as_ref().unwrap().name(), if tag.is_some() { "BGGR" } else { "RGGB" });
            assert_eq!(crate::probe_info(&bytes).unwrap(), r.info());
        }
    }

    #[test]
    fn data_fallback_looks_past_fine_texture() {
        let (w, h) = (160usize, 144usize);
        // greens on the main diagonal, red and blue close; a checkerboard of detail makes the two greens of a cell
        // differ by far more than red differs from blue
        let detail = |cx: usize, cy: usize| if (cx + cy) & 1 == 0 { 150 } else { -150i32 };
        let site = |cx: usize, cy: usize| {
            let (g, d) = (1000 + shade(cx, cy) as i32, detail(cx, cy));
            [(g + d) as u16, 500, 520, (g - d) as u16]
        };
        assert_eq!(layout(&orf(w as u32, h as u32, 16, mosaic(w, h, site))), "GRBG");
        // the same scene with the greens on the other diagonal
        let px = mosaic(w, h, |cx, cy| {
            let [g0, r, b, g1] = site(cx, cy);
            [r, g0, g1, b]
        });
        assert_eq!(layout(&orf(w as u32, h as u32, 16, px)), "RGGB");
    }

    #[test]
    fn data_fallback_survives_any_area() {
        let d = vec![100u16; 64 * 64];
        let big = usize::MAX;
        for a in [
            Rect::new(0, 0, 64, 64),
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 31, 31),
            Rect::new(63, 63, 1, 1),
            Rect::new(0, 0, 4096, 4096),
            Rect::new(big, big, big, big),
            Rect::new(0, big - 40, 64, 40),
        ] {
            for w in [64, 0, 1, big] {
                assert_eq!(cfa_from_data(&d, w, a).name(), "RGGB", "{a:?} width {w}");
                assert_eq!(cfa_from_data(&[], w, a).name(), "RGGB", "{a:?} width {w}");
            }
        }
    }

    fn padded_samples(pixels: &[u16]) -> Vec<u8> {
        let mut strip = Vec::new();
        for group in pixels.as_chunks::<10>().0 {
            for pair in group.as_chunks::<2>().0 {
                let (a, b) = (pair[0], pair[1]);
                strip.extend_from_slice(&[a as u8, ((a >> 8) | (b << 4)) as u8, (b >> 4) as u8]);
            }
            strip.push(0);
        }
        strip
    }

    #[test]
    fn high_res_padded12_roundtrip_and_header_without_samples() {
        let (w, h) = (20usize, 8usize);
        let mut pixels: Vec<u16> = (0..w * h).map(|i| (i * 103 % 4096) as u16).collect();
        pixels[..6].copy_from_slice(&[0, 4095, 1, 4094, 256, 2048]);
        let bytes =
            orf_with_exif(w as u32, h as u32, 16, padded_samples(&pixels), ByteOrder::Little, Some(exif_cfa(ByteOrder::Little, &[1, 0, 2, 1])));
        let raw = crate::decode(&bytes).unwrap();
        assert_eq!(raw.data, RawData::U16(pixels));
        assert_eq!(raw.bits, 12);
        assert_eq!(raw.cfa.unwrap().name(), "GRBG");
        let header = decode(&bytes, Mode::Header).unwrap();
        assert_eq!(header.data.len(), 0, "tagged packed ORF must not allocate sensor samples to probe");
        assert_eq!(header.info(), crate::decode(&bytes).unwrap().info());
        let parsed = Tiff::parse(&bytes).unwrap();
        let offset = parsed.ifds[0].u64(t::STRIP_OFFSETS).unwrap() as usize;
        let mut short = bytes.clone();
        short.truncate(offset + 15);
        assert!(matches!(crate::decode(&short), Err(RawError::Corrupt(_) | RawError::Tiff(_))));
        assert!(matches!(crate::probe_info(&short), Err(RawError::Corrupt(_) | RawError::Tiff(_))));
        let count_at = parsed.ifds[0].get(t::STRIP_BYTE_COUNTS).unwrap().offset as usize;
        let mut outside = bytes.clone();
        outside[count_at..count_at + 4].copy_from_slice(&(bytes.len() as u32 + 1).to_le_bytes());
        assert!(matches!(crate::decode(&outside), Err(RawError::Corrupt(_))));
        let mut unknown = bytes.clone();
        unknown[offset + 15] = 1;
        assert!(matches!(crate::decode(&unknown), Err(RawError::Unsupported(_))));
        // Every truncation and mutations must produce a result, never a panic.
        for length in 0..bytes.len() {
            let _ = crate::decode(&bytes[..length]);
            let _ = crate::probe_info(&bytes[..length]);
        }
        for at in (0..bytes.len()).step_by(7) {
            let mut mutated = bytes.clone();
            mutated[at] ^= 0xff;
            let _ = crate::decode(&mutated);
            let _ = crate::probe_info(&mutated);
        }
    }

    #[test]
    fn compressed_orf_without_verified_profile_stays_unsupported() {
        let bytes = orf_with_exif(40, 24, 16, vec![0; 40], ByteOrder::Little, Some(exif_cfa(ByteOrder::Little, &[0, 1, 1, 2])));
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
        assert!(matches!(crate::probe_info(&bytes), Err(RawError::Unsupported(_))));
    }

    fn profile_fields() -> Vec<(u16, u16, u32, [u8; 4])> {
        let mut fields = vec![(0x0611, 3, 2, [12, 0, 0, 0])];
        fields.extend(olympus12::PROFILE.iter().map(|&(tag, value)| (tag, 3, 1, [value as u8, (value >> 8) as u8, 0, 0])));
        fields
    }

    fn compressed_exif(fields: &[(u16, u16, u32, [u8; 4])]) -> IfdBuilder {
        // Synthetic note-relative OlympusNew directories; every fixture field is inline.
        let mut note = b"OLYMPUS\0II\x03\0".to_vec();
        note.extend_from_slice(&1u16.to_le_bytes());
        note.extend_from_slice(&IMAGE_PROCESSING.to_le_bytes());
        note.extend_from_slice(&4u16.to_le_bytes());
        note.extend_from_slice(&1u32.to_le_bytes());
        note.extend_from_slice(&30u32.to_le_bytes());
        note.extend_from_slice(&0u32.to_le_bytes());
        note.extend_from_slice(&(fields.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in fields {
            note.extend_from_slice(&tag.to_le_bytes());
            note.extend_from_slice(&kind.to_le_bytes());
            note.extend_from_slice(&count.to_le_bytes());
            note.extend_from_slice(value);
        }
        note.extend_from_slice(&0u32.to_le_bytes());
        exif_cfa(ByteOrder::Little, &[0, 1, 1, 2]).with(t::MAKER_NOTE, Value::Undefined(note))
    }

    fn compressed_fixture(exif: IfdBuilder, bits: u16, strip: Vec<u8>) -> Vec<u8> {
        orf_with_exif(4, 4, bits, strip, ByteOrder::Little, Some(exif))
    }

    fn compressed_strip() -> Vec<u8> {
        vec![0, 0, 0, 0, 1, 0, 0, 0x18, 0x10, 0x18, 0x10, 0x10, 0x10, 0x10, 0x10, 0x96, 0x10, 0x11, 0x10, 0x10, 0x10, 0x10, 0x10]
    }

    #[test]
    fn measured_profile_decodes_and_probe_skips_sensor_interpretation() {
        let bytes = compressed_fixture(compressed_exif(&profile_fields()), 16, compressed_strip());
        let raw = crate::decode(&bytes).unwrap();
        assert_eq!(raw.data, RawData::U16(vec![32, 0, 64, 0, 0, 0, 0, 0, 4, 0, 34, 0, 0, 0, 0, 0]));
        assert_eq!(raw.bits, 12);
        assert_eq!(raw.cfa.as_ref().unwrap().name(), "RGGB");
        let header = decode(&bytes, Mode::Header).unwrap();
        assert_eq!(header.data.len(), 0);
        assert_eq!(header.info(), raw.info());
        let mut invalid_body = compressed_strip();
        invalid_body[7..].fill(0);
        let bytes = compressed_fixture(compressed_exif(&profile_fields()), 16, invalid_body);
        assert_eq!(decode(&bytes, Mode::Header).unwrap().data.len(), 0);
        assert!(matches!(crate::decode(&bytes), Err(RawError::Corrupt(_))));
    }

    #[test]
    fn profile_takes_precedence_over_word_density_heuristic() {
        // Two initial escapes (q=219/406), then two reset-row q=0 ordinary words.
        // Literal bits, not an encoder. Seventeen strip bytes exceed 2*4 samples.
        let bits = "000000000000000000000011010101100000000000000000000011001001100001000000010000";
        let mut strip = vec![0, 0, 0, 0, 1, 0, 0];
        strip.extend(bits.as_bytes().chunks(8).map(|c| c.iter().fold(0u8, |v, &b| (v << 1) | (b - b'0')) << (8 - c.len())));
        assert_eq!(strip.len(), 17);
        let bytes = orf_with_exif(2, 2, 16, strip, ByteOrder::Little, Some(compressed_exif(&profile_fields())));
        assert_eq!(crate::decode(&bytes).unwrap().data, RawData::U16(vec![876, 1624, 0, 0]));
        assert_eq!(crate::probe_info(&bytes).unwrap(), crate::decode(&bytes).unwrap().info());
    }

    #[test]
    fn unknown_profile_depth_and_missing_cfa_are_unsupported() {
        let original = profile_fields();
        let mut variants = Vec::new();
        for i in 0..original.len() {
            let mut value = original.clone();
            value[i].3[0] ^= 1;
            variants.push(value);
            let mut kind = original.clone();
            kind[i].1 = 4;
            variants.push(kind);
            let mut count = original.clone();
            count[i].2 = 0;
            variants.push(count);
            let mut missing = original.clone();
            missing.remove(i);
            variants.push(missing);
        }
        let mut future = original.clone();
        future.push((0x064a, 3, 1, [0; 4]));
        variants.push(future);
        let mut fourteen = original.clone();
        fourteen[0].3[0] = 14;
        variants.push(fourteen);
        for fields in variants {
            let bytes = compressed_fixture(compressed_exif(&fields), 16, compressed_strip());
            assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
            assert!(matches!(crate::probe_info(&bytes), Err(RawError::Unsupported(_))));
        }
        let bytes = compressed_fixture(compressed_exif(&original), 14, compressed_strip());
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
        let mut exif = compressed_exif(&original);
        exif.set(EXIF_CFA_PATTERN, Value::Undefined(vec![]));
        let bytes = compressed_fixture(exif, 16, compressed_strip());
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
        let bytes = orf_with_exif(4, 4, 16, compressed_strip(), ByteOrder::Big, Some(compressed_exif(&original)));
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn measured_orf_truncations_and_container_mutations_do_not_panic() {
        let bytes = compressed_fixture(compressed_exif(&profile_fields()), 16, compressed_strip());
        for cut in 0..bytes.len() {
            let _ = crate::decode(&bytes[..cut]);
            let _ = crate::probe_info(&bytes[..cut]);
        }
        for at in 0..bytes.len() {
            let mut changed = bytes.clone();
            changed[at] ^= 0xff;
            let _ = crate::decode(&changed);
            let _ = crate::probe_info(&changed);
        }
    }

    #[test]
    fn compressed_container_layout_must_be_verified() {
        let bytes = compressed_fixture(compressed_exif(&profile_fields()), 16, compressed_strip());
        let parsed = Tiff::parse(&bytes).unwrap();
        for (tag, value) in [
            (t::SAMPLES_PER_PIXEL, 0u16),
            (t::SAMPLES_PER_PIXEL, 2),
            (t::SAMPLE_FORMAT, 2),
            (t::SAMPLE_FORMAT, 3),
            (t::PLANAR_CONFIGURATION, 2),
            (t::PREDICTOR, 2),
            (t::COMPRESSION, 7),
            (t::ROWS_PER_STRIP, 2),
            (t::STRIP_BYTE_COUNTS, 0),
        ] {
            let at = parsed.ifds[0].get(tag).unwrap().offset as usize;
            let mut changed = bytes.clone();
            changed[at..at + 2].copy_from_slice(&value.to_le_bytes());
            assert!(matches!(crate::decode(&changed), Err(RawError::Unsupported(_))), "tag {tag:04x}, value {value}");
            assert!(matches!(crate::probe_info(&changed), Err(RawError::Unsupported(_))), "tag {tag:04x}, value {value}");
        }
    }

    #[test]
    fn unverified_big_endian_padded_packing_is_unsupported() {
        let bytes = orf_with_exif(20, 8, 16, padded_samples(&[1025; 160]), ByteOrder::Big, Some(exif_cfa(ByteOrder::Big, &[0, 1, 1, 2])));
        assert!(matches!(crate::decode(&bytes), Err(RawError::Unsupported(_))));
        assert!(matches!(crate::probe_info(&bytes), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn overflowing_maker_note_crop_does_not_panic() {
        // A new-style Olympus note with one ImageProcessing IFD, wholly synthetic.
        let mut note = b"OLYMPUS\0II\x03\0".to_vec();
        note.extend_from_slice(&1u16.to_le_bytes());
        note.extend_from_slice(&IMAGE_PROCESSING.to_le_bytes());
        note.extend_from_slice(&4u16.to_le_bytes());
        note.extend_from_slice(&1u32.to_le_bytes());
        note.extend_from_slice(&30u32.to_le_bytes());
        note.extend_from_slice(&0u32.to_le_bytes());
        note.extend_from_slice(&4u16.to_le_bytes());
        for (i, tag) in CROP.iter().enumerate() {
            note.extend_from_slice(&tag.to_le_bytes());
            note.extend_from_slice(&16u16.to_le_bytes()); // LONG8: hostile but parsed by the TIFF reader
            note.extend_from_slice(&1u32.to_le_bytes());
            note.extend_from_slice(&(84u32 + i as u32 * 8).to_le_bytes());
        }
        note.extend_from_slice(&0u32.to_le_bytes());
        for value in [u64::MAX, 0, 2, 1] {
            note.extend_from_slice(&value.to_le_bytes());
        }
        let exif = IfdBuilder::new().with(t::MAKER_NOTE, Value::Undefined(note));
        let bytes = orf_with_exif(16, 16, 16, vec![1; 512], ByteOrder::Little, Some(exif));
        let tiff = Tiff::parse(&bytes).unwrap();
        let mn = maker_note(&bytes, &tiff).unwrap();
        assert_eq!(sub_ifd(&bytes, &mn, IMAGE_PROCESSING).unwrap().u64(CROP[0]), Some(u64::MAX));
        assert_eq!(crate::decode(&bytes).unwrap().active_area, Rect::new(0, 0, 16, 16));
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
