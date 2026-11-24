//! The ORF container around a compressed sensor stream: which files carry one, and everything but the samples.
//!
//! `craft-raw` reads Olympus ORF files and refuses compressed sensor data (`Unsupported("Olympus compressed ORF")`).
//! This module recognises a compressed file, decodes its stream with [`super::decompress_stream`] and builds
//! the same [`RawImage`] `craft-raw` builds for an uncompressed file that has an `ImageProcessing` maker-note
//! sub-directory, which every compressed file has (the stream parameters live there). Crop, black level and as-shot
//! white balance come from `ImageProcessing`, the colour-filter layout from the Exif `CFAPattern` (else from the
//! samples), and the white level from the samples' saturation plateau.
//!
//! Sources: TIFF 6.0 (the `IIRO`/`MMOR` container), Exif 2.3 (`CFAPattern`, tag `0xa302`), and the maker-note
//! layout `craft-raw`'s ORF reader documents (`0x2040` ImageProcessing; `0x0600` black, `0x0100`/`0x011f` white
//! balance, `0x0612`–`0x0615` crop; main-note `0x102c` ValidBits).

use craft_raw::{
    BlackLevel, Camera, Cfa, ColorData, LevelSource, LevelSources, OpcodeLists, Orientation, RawData, RawFormat, RawImage, RawNotices, Rect,
};
use craft_tiff::image::chunk_bytes;
use craft_tiff::makernote::{self, MakerNote};
use craft_tiff::{Ifd, Tiff, Value, tags as t};

use super::{OrfCompressionConfig, decompress_stream, verify_stream_header};
use crate::{MAX_SAMPLES, Mode, RawError, Result};

/// Maker-note sub-directory holding the image-processing parameters, the compressed stream's among them.
const IMAGE_PROCESSING: u16 = 0x2040;
/// ImageProcessing: red and blue white-balance gains.
const WB_RB: u16 = 0x0100;
/// ImageProcessing: the green gain the red and blue ones are relative to (256 when absent).
const WB_G: u16 = 0x011f;
/// ImageProcessing: black level, one value per 2×2 CFA site (R, G, G, B).
const BLACK: u16 = 0x0600;
/// ImageProcessing: crop left, top, width, height.
const CROP: [u16; 4] = [0x0612, 0x0613, 0x0614, 0x0615];
/// Main maker note: the sensor's valid bits.
const MN_VALID_BITS: u16 = 0x102c;
/// Exif `CFAPattern`.
const EXIF_CFA_PATTERN: u16 = 0xa302;
/// ImageProcessing: byte length of the compressed stream's header prefix; its presence marks a compressed file.
const STREAM_HEADER_PREFIX: u16 = 0x0653;

/// A compressed ORF decoded, or `Ok(None)` when `bytes` is not one: no readable TIFF or maker note, no
/// `ImageProcessing` stream header length (tag `0x0653`) or one of 0, or a TIFF compression other than 1. Those files
/// go to `craft-raw`, which reports their errors. Uncompressed files may state every stream parameter as 0 (the CC0
/// E-M5 Mark II sample does), so only a non-zero header length marks a compressed file. A file that marks one but
/// whose other parameters are out of range, or whose stream is damaged, is an error here, so it is never misread as
/// uncompressed 16-bit words.
pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<Option<RawImage>> {
    let Ok(tiff) = Tiff::parse(bytes) else { return Ok(None) };
    let Some(mn) = maker_note(bytes, &tiff) else { return Ok(None) };
    let Some(ip) = sub_ifd(bytes, &mn, IMAGE_PROCESSING) else { return Ok(None) };
    if ip.u64(STREAM_HEADER_PREFIX).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let config =
        OrfCompressionConfig::from_subifd(&ip, Some(&mn)).ok_or_else(|| RawError::Corrupt("ORF compressed stream parameters out of range".into()))?;
    let Some(ifd0) = tiff.ifds.first() else { return Ok(None) };
    let Ok(info) = ifd0.image() else { return Ok(None) };
    if info.compression != 1 {
        return Ok(None);
    }
    let (w, h) = (info.width as usize, info.height as usize);
    w.checked_mul(h).filter(|n| *n > 0 && *n <= MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;

    let chunks = info.chunks(bytes.len() as u64);
    let src = chunks.first().and_then(|c| chunk_bytes(bytes, c)).ok_or_else(|| RawError::Corrupt("ORF strip outside file".into()))?;
    if !verify_stream_header(src, config.header_prefix_len) {
        return Err(RawError::Corrupt("corrupt ORF compressed stream header".into()));
    }
    let stream = src.get(config.header_prefix_len.saturating_add(2)..).ok_or_else(|| RawError::Corrupt("ORF compressed stream truncated".into()))?;

    let stated = cfa_from_exif(&tiff);
    let has_valid_bits = mn.ifd.u64(MN_VALID_BITS).is_some();
    // a header probe needs the samples only for what the file doesn't state: the layout and the bit depth
    let samples = if mode == Mode::Full || stated.is_none() || !has_valid_bits { decompress_stream(stream, w, h, &config)? } else { Vec::new() };
    let bits = if has_valid_bits {
        config.sensor_bits
    } else {
        // every sample: a few values past a depth's range are enough to need the next one
        match samples.iter().max() {
            Some(&mx) if mx < 4096 => 12,
            Some(&mx) if mx < 16384 => 14,
            Some(_) => 16,
            None => config.sensor_bits,
        }
    };

    let active = match CROP.map(|tag| ip.u64(tag).and_then(|v| usize::try_from(v).ok())) {
        // checked: the tags are file data; `x + cw` on a hostile value would wrap past the bounds
        [Some(x), Some(y), Some(cw), Some(ch)]
            if cw > 0 && ch > 0 && x.checked_add(cw).is_some_and(|end| end <= w) && y.checked_add(ch).is_some_and(|end| end <= h) =>
        {
            Rect::new(x, y, cw, ch)
        }
        _ => Rect::new(0, 0, w, h),
    };
    let assumed_cfa = stated.is_none();
    let cfa = stated.unwrap_or_else(|| cfa_from_data(&samples, w, active));
    let (black, black_source) = match ip.f64s(BLACK).as_deref() {
        Some(&[r, g1, g2, b]) => {
            let a = cfa.shifted(active.x, active.y);
            // the greens averaged, as craft-raw's ORF reader does with this tag: which green site each value belongs
            // to is not established, and a wrong guess would offset one green site against the other
            let per_colour = [r, (g1 + g2) / 2.0, b];
            let values = a.pattern.iter().map(|&c| per_colour.get(usize::from(c)).copied().unwrap_or(0.0) as f32).collect();
            (BlackLevel { repeat_rows: 2, repeat_cols: 2, values, ..Default::default() }, LevelSource::Metadata)
        }
        _ => (BlackLevel::uniform(0.0), LevelSource::Fallback),
    };
    let wb = ip.f64s(WB_RB).map(|v| (v, ip.f64(WB_G).filter(|g| *g > 0.0 && g.is_finite()).unwrap_or(256.0))).and_then(|(v, g)| match v.as_slice() {
        &[r, b, ..] if r > 0.0 && b > 0.0 => Some([(r / g) as f32, 1.0, (b / g) as f32]),
        _ => None,
    });
    let (white, white_source) = white_from_data(&samples, bits);

    let img = RawImage {
        format: RawFormat::Orf,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(samples),
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
        linearization_table: None,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        camera: Camera::from_ifd0(ifd0),
        cinema: Default::default(),
        lens_metadata: Default::default(),
        notices: RawNotices { assumed_cfa, ..Default::default() },
        level_sources: LevelSources { black: black_source, white: white_source },
    };
    // a header probe may hold no samples, which `validate` would count as a length mismatch
    if mode == Mode::Full {
        img.validate()?;
    }
    Ok(Some(img))
}

/// The Olympus maker note.
fn maker_note(bytes: &[u8], tiff: &Tiff) -> Option<MakerNote> {
    let make = tiff.find(t::MAKE).and_then(|e| e.value.as_str()).unwrap_or_default().to_string();
    let e = tiff.exif().and_then(|e| e.get(t::MAKER_NOTE))?;
    makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make)
}

/// A maker-note sub-directory: an IFD pointer (offset relative to the note's base) in new-style notes, or the
/// IFD stored inline as an undefined blob (offsets relative to the base) in old-style ones.
fn sub_ifd(bytes: &[u8], mn: &MakerNote, tag: u16) -> Option<Ifd> {
    let e = mn.ifd.get(tag)?;
    let at = match &e.value {
        Value::Undefined(_) | Value::Byte(_) => e.offset,
        _ => mn.base.checked_add(mn.ifd.u64(tag)?)?,
    };
    let opts = craft_tiff::ParseOptions { max_ifds: 4, max_depth: 1, follow_children: false, ..Default::default() };
    craft_tiff::parse_ifd_at(bytes, at, mn.order, mn.base, false, &opts).ok().map(|(i, _)| i)
}

/// A 2×2 Bayer cell anchored at the sensor origin; colour indices 0 = R, 1 = G, 2 = B.
fn bayer(pattern: [u8; 4]) -> Cfa {
    Cfa { width: 2, height: 2, origin_x: 0, origin_y: 0, pattern: pattern.to_vec() }
}

/// The layout the Exif `CFAPattern` states, when it is a 2×2 Bayer cell.
fn cfa_from_exif(tiff: &Tiff) -> Option<Cfa> {
    let &[c0, c1, r0, r1, s0, s1, s2, s3] = tiff.exif()?.bytes(EXIF_CFA_PATTERN)? else { return None };
    let two = |a: u8, b: u8| matches!((a, b), (2, 0) | (0, 2));
    let pattern = match [s0, s1, s2, s3] {
        p @ ([0, 1, 1, 2] | [2, 1, 1, 0] | [1, 0, 2, 1] | [1, 2, 0, 1]) => p,
        _ => return None,
    };
    (two(c0, c1) && two(r0, r1)).then(|| bayer(pattern))
}

/// The layout found from the samples, for files without the Exif tag: GRBG when the greens sit on the main
/// diagonal of the 2×2 cell at the sensor origin, else RGGB. Over 32×32-pixel blocks of `a` (every fourth in both
/// directions) it compares the block totals of the two sites on each diagonal: the two greens of a block see the
/// same light, red and blue rarely do.
fn cfa_from_data(d: &[u16], w: usize, a: Rect) -> Cfa {
    const BLOCK: usize = 32;
    let (x0, y0) = (a.x.saturating_add(1) & !1, a.y.saturating_add(1) & !1);
    let across = a.x.saturating_add(a.width).min(w).saturating_sub(x0) / BLOCK;
    let down = a.y.saturating_add(a.height).saturating_sub(y0) / BLOCK;
    let (mut main, mut anti) = (0u64, 0u64);
    for by in (0..down).step_by(4) {
        for bx in (0..across).step_by(4) {
            let mut sums = [0i64; 4];
            for y in 0..BLOCK {
                let start = y0
                    .checked_add(by.saturating_mul(BLOCK))
                    .and_then(|r| r.checked_add(y))
                    .and_then(|r| r.checked_mul(w))
                    .and_then(|r| r.checked_add(x0.checked_add(bx.checked_mul(BLOCK)?)?));
                // rows past the end of the samples: nothing more to read
                let Some(row) = start.and_then(|s| d.get(s..s.checked_add(BLOCK)?)) else { break };
                for (x, &v) in row.iter().enumerate() {
                    if let Some(s) = sums.get_mut((y & 1) * 2 + (x & 1)) {
                        *s += i64::from(v);
                    }
                }
            }
            let [s0, s1, s2, s3] = sums;
            main = main.saturating_add((s0 - s3).unsigned_abs());
            anti = anti.saturating_add((s1 - s2).unsigned_abs());
        }
    }
    bayer(if main < anti { [1, 0, 2, 1] } else { [0, 1, 1, 2] })
}

/// The white level: the lower edge of the samples' saturation plateau ([`LevelSource::Inferred`]) when there is
/// one, else the top of the `bits` range ([`LevelSource::Fallback`]).
fn white_from_data(data: &[u16], bits: u32) -> (f32, LevelSource) {
    let full_level = (1u32 << bits.clamp(1, 16)) - 1;
    let full = full_level as f32;
    // an odd step: with an even one, an even-width mosaic is read on even columns only, missing two CFA sites
    let step = (data.len() / 2_000_000).max(1) | 1;
    // samples above the nominal range are padding / invalid (e.g. 0xFFFF fill), not saturation
    let mut hist = vec![0usize; full_level as usize + 1];
    for &v in data.iter().step_by(step) {
        if let Some(h) = hist.get_mut(usize::from(v)) {
            *h += 1;
        }
    }
    let min_plateau = (data.len() / 20_000).max(16);
    // the top of the data, skipping up to half a plateau's worth of strays so a real plateau can't be skipped
    let strays = min_plateau / 2;
    let mut above = 0usize;
    let Some(mx) = hist.iter().enumerate().rev().find_map(|(v, &n)| {
        above = above.saturating_add(n.saturating_mul(step));
        (above > strays).then_some(v as u32)
    }) else {
        return (full, LevelSource::Fallback);
    };
    let band = (mx / 256).max(1);
    let near = mx.saturating_sub(band);
    let below = near.saturating_sub(band);
    let count = |from: u32, to: u32| hist.get(from as usize..to as usize).map_or(0, |h| h.iter().sum::<usize>());
    // the strays count as saturated too
    let top = count(near, full_level + 1);
    let under = count(below, near);
    // a saturation plateau is a spike: many more samples in the top band than in the band just below it
    if mx as f32 > full * 0.5 && top.saturating_mul(step) >= min_plateau && top >= under.saturating_mul(4).saturating_add(4) {
        (near as f32, LevelSource::Inferred)
    } else {
        (full, LevelSource::Fallback)
    }
}

#[cfg(test)]
mod tests;
