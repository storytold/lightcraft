//! Sony SRF (DSC-F828, DSC-V3): an encrypted 14-bit sensor dump behind an RGB-looking IFD0.
//!
//! Sources: translated from dcraw 9.28 (`sony_load_raw`, `sony_decrypt`, and the DSC-F828 / DSC-V3 rows of
//! `identify()`), outside its RESTRICTED block (see `NOTICE`), and checked on the CC0 DSC-F828 sample on
//! raw.pixls.us. The keystream generator is the one `sr2.rs` already derives (`seed_from_key`).
//!
//! - The file key is the big-endian `u32` at `200896 + 4 × byte[200896]`. It decrypts the 40 bytes at 164600; their
//!   bytes 22..26, read little-endian, are the data key.
//! - The sensor data is IFD0's `ImageWidth × ImageLength` big-endian 16-bit words, one keystream from its first word
//!   to the last, every sample below 2^14. IFD0 has no `StripOffsets`: the data ends the file.
//! - DSC-F828: 3288 columns from column 5, a four-colour R E / G B mosaic (E = emerald) counted from that column; the
//!   five columns on the left are masked (the black level). DSC-V3: 3109 columns from column 59, RGGB from there.
//!
//! Our pipeline is three-colour, so the F828's emerald sites are read as green after scaling them to the green sites'
//! mean: tones and detail are right, colour is approximate (no four-colour matrix).

use super::sr2::{apply_keystream_with, seed_from_key};
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lcc_geom::Orientation;
use lcc_tiff::Tiff;
use lcc_tiff::tags as t;

const KEY_TABLE: usize = 200_896;
const HEAD: usize = 164_600;
const WHITE: f32 = 16368.0;

/// Per body: (image width, left margin, four-colour R E / G B).
fn body(model: &str) -> Option<(usize, usize, bool)> {
    match model.trim() {
        "DSC-F828" => Some((3288, 5, true)),
        "DSC-V3" => Some((3109, 59, false)),
        _ => None,
    }
}

/// Whether this is an SRF body we know the layout of.
pub(crate) fn applies(model: &str) -> bool {
    body(model).is_some()
}

fn data_key(bytes: &[u8]) -> Option<u32> {
    let at = KEY_TABLE.checked_add(usize::from(*bytes.get(KEY_TABLE)?) * 4)?;
    let key = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    let mut head: [u8; 40] = bytes.get(HEAD..HEAD + 40)?.try_into().ok()?;
    apply_keystream_with(&mut head, seed_from_key(key));
    Some(u32::from_le_bytes(head.get(22..26)?.try_into().ok()?))
}

pub(crate) fn decode(bytes: &[u8], tiff: &Tiff, mode: Mode) -> Result<RawImage> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| RawError::Corrupt("SRF without IFD0".into()))?;
    let model = ifd0.string(t::MODEL).unwrap_or_default();
    let (iw, left, four) = body(&model).ok_or_else(|| RawError::Unsupported(format!("SRF from {model}")))?;
    let w = ifd0.u64(t::IMAGE_WIDTH).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    let h = ifd0.u64(t::IMAGE_LENGTH).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    if w < left.saturating_add(iw) || h < 8 {
        return Err(RawError::Corrupt(format!("SRF: sensor {w}x{h} narrower than the image")));
    }
    let n = crate::checked_samples(&[w, h])? as usize;
    let len = n.checked_mul(2).ok_or(RawError::Limit("image too large"))?;
    let start = bytes.len().checked_sub(len).ok_or_else(|| RawError::Corrupt("SRF: file shorter than its sensor data".into()))?;
    let key = data_key(bytes).ok_or_else(|| RawError::Corrupt("SRF: key block outside the file".into()))?;
    let (data, black) = match mode {
        Mode::Header => (Vec::new(), 0.0),
        Mode::Full => {
            let mut src = bytes.get(start..).unwrap_or_default().to_vec();
            apply_keystream_with(&mut src, seed_from_key(key));
            let mut data: Vec<u16> = src.as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
            if data.iter().any(|v| v >> 14 != 0) {
                return Err(RawError::Corrupt("SRF: sample above 14 bits (wrong key or layout)".into()));
            }
            let black = masked_black(&data, w, left);
            if four {
                equalize_emerald(&mut data, w, h, left, iw, black);
            }
            (data, black)
        }
    };
    let crop = Rect::new(left, 0, iw, h);
    let mut metadata = lcc_meta::from_tiff(tiff);
    metadata.width = Some(crop.width as u32);
    metadata.height = Some(crop.height as u32);
    let img = RawImage {
        format: RawFormat::Arw,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        // dcraw's layouts count columns from the crop; the CFA here is anchored at sensor column 0
        cfa: Some(Cfa::bayer_static(if left % 2 == 1 { "GRBG" } else { "RGGB" })),
        bits: 14,
        black: BlackLevel::uniform(black),
        white: vec![WHITE],
        active_area: Rect::new(0, 0, w, h),
        crop,
        orientation: Orientation::from_exif(ifd0.u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
        pixel_aspect: 1.0,
    };
    img.validate_for(mode)?;
    Ok(img)
}

/// Median of the masked columns left of the image (0 when there are none).
fn masked_black(data: &[u16], w: usize, left: usize) -> f32 {
    let mut v: Vec<u16> = data.chunks_exact(w).flat_map(|row| row.iter().take(left).copied()).collect();
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable(mid);
    f32::from(*m)
}

/// Scale the emerald sites (row 0, odd columns of the 2 × 2 cell) to the green sites' mean above black.
fn equalize_emerald(data: &mut [u16], w: usize, h: usize, left: usize, iw: usize, black: f32) {
    let mean = |data: &[u16], row0: usize, col0: usize| {
        let (mut s, mut c) = (0f64, 0f64);
        for y in (row0..h).step_by(2) {
            for x in (left + col0..left + iw).step_by(2) {
                if let Some(v) = data.get(y * w + x) {
                    s += f64::from(*v) - f64::from(black);
                    c += 1.0;
                }
            }
        }
        s / c.max(1.0)
    };
    // the phase counts from the crop's left edge: R E on even rows, G B on odd rows
    let (e, g) = (mean(data, 0, 1), mean(data, 1, 0));
    if !(e > 1.0 && g > 1.0) {
        return;
    }
    let gain = (g / e).clamp(0.25, 4.0) as f32;
    for y in (0..h).step_by(2) {
        for x in ((left + 1) % 2..w).step_by(2) {
            if let Some(v) = data.get_mut(y * w + x) {
                *v = ((f32::from(*v) - black) * gain + black).clamp(0.0, WHITE) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bodies_only() {
        assert!(applies("DSC-F828") && applies(" DSC-V3 ") && !applies("DSC-R1") && !applies(""));
    }

    /// Truncated, empty and garbage files are errors, never panics.
    #[test]
    fn hostile_files_are_errors() {
        assert!(data_key(&[]).is_none());
        assert!(data_key(&vec![0xffu8; KEY_TABLE + 1]).is_none());
        let mut tiff = b"II*\x00\x08\x00\x00\x00".to_vec();
        tiff.extend_from_slice(&[3, 0]);
        for (tag, value) in [(0x100u16, 3360u32), (0x101, 2460)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&4u16.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0x110u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&9u32.to_le_bytes());
        tiff.extend_from_slice(&50u32.to_le_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        tiff.extend_from_slice(b"DSC-F828\0");
        for len in [tiff.len(), 200_000, 300_000] {
            let mut b = tiff.clone();
            b.resize(len, 0x5a);
            if let Ok(parsed) = Tiff::parse(&b) {
                for mode in [Mode::Header, Mode::Full] {
                    assert!(decode(&b, &parsed, mode).is_err(), "{len} {mode:?}");
                }
            }
        }
    }

    #[test]
    fn emerald_is_scaled_to_green() {
        let (w, h, left, iw) = (8, 4, 0, 8);
        let mut d = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                d[y * w + x] = match (y % 2, x % 2) {
                    (0, 1) => 300,
                    (1, 0) => 600,
                    _ => 400,
                };
            }
        }
        equalize_emerald(&mut d, w, h, left, iw, 100.0);
        assert_eq!(d[1], 600);
        assert_eq!(masked_black(&d, w, 0), 0.0);
    }
}
