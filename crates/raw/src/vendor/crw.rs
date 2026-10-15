//! Canon CRW (CIFF heap) raw decoding: the bodies before the CR2 era (EOS D30/D60/10D/300D,
//! PowerShot G-series).
//!
//! Clean-room. The container rules come from Canon's published CIFF specification and ExifTool's
//! prose tag documentation; the compression was derived from CC0 and maintainer-owned sample files
//! by black-box analysis against LibRaw's command-line output, frozen in
//! `clean-room/crw/06-crw-format.md` and verified sample-exact on 28 of 29 corpus files
//! (`clean-room/crw/07-verification.md`). The code tables are frozen data
//! (`crw_tables`, generated from `clean-room/crw/logs/table_sel{0,1,2}.json`). No other raw
//! decoder's source was consulted.
//!
//! Known gaps (documented in the freeze): the PowerShot G5's entropy coding diverges from this
//! model and is refused (its embedded JPEG is shown instead); PowerShot bodies without CC0
//! samples (G2/G3/Pro1/S-series) decode through the same model unverified.

use crate::vendor::{crw_tables, white_from_data};
use crate::{BlackLevel, Cfa, ColorData, Metadata, Mode, OpcodeLists, Orientation, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_meta::DateTime;
use std::collections::BTreeMap;

fn corrupt(why: &str) -> RawError {
    RawError::Corrupt(format!("CRW: {why}"))
}

/// Heap recursion bound: the deepest directory observed in real files is 4.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Le,
    Be,
}

fn u16_at(b: &[u8], at: usize, o: Order) -> Option<u16> {
    let s: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(match o {
        Order::Le => u16::from_le_bytes(s),
        Order::Be => u16::from_be_bytes(s),
    })
}

fn u32_at(b: &[u8], at: usize, o: Order) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(match o {
        Order::Le => u32::from_le_bytes(s),
        Order::Be => u32::from_be_bytes(s),
    })
}

/// Walk a heap block (`start..start + len` of `bytes`) depth-first in record-table order,
/// recording each in-heap record's tag (typeCode & 0x3fff), absolute offset and length — first
/// occurrence wins. In-record values and reserved storage classes are skipped; dataType 5/6
/// records are sub-heaps.
fn walk(bytes: &[u8], start: usize, len: usize, o: Order, depth: usize, out: &mut BTreeMap<u16, (usize, usize)>) {
    if depth > MAX_DEPTH || len < 6 {
        return;
    }
    let Some(tbl) = start.checked_add(len).and_then(|e| e.checked_sub(4)).and_then(|at| u32_at(bytes, at, o)) else { return };
    let tbl = tbl as usize;
    // the offset table must fit between the block start and its own trailing pointer
    if tbl.checked_add(6).is_none_or(|e| e > len) {
        return;
    }
    let Some(abs_tbl) = start.checked_add(tbl) else { return };
    let Some(n) = u16_at(bytes, abs_tbl, o) else { return };
    let Some(records_end) = (n as usize).checked_mul(10).and_then(|r| tbl.checked_add(2)?.checked_add(r)) else { return };
    if records_end > len {
        return;
    }
    for k in 0..n as usize {
        let at = abs_tbl + 2 + k * 10;
        let (Some(tc), Some(rl), Some(ro)) = (u16_at(bytes, at, o), u32_at(bytes, at + 2, o), u32_at(bytes, at + 6, o)) else { return };
        if tc & 0xc000 != 0 {
            continue; // in-record value or reserved storage: nothing we need
        }
        let (rl, ro) = (rl as usize, ro as usize);
        if ro.checked_add(rl).is_none_or(|e| e > len) {
            continue;
        }
        let Some(abs) = start.checked_add(ro) else { continue };
        if matches!((tc >> 11) & 7, 5 | 6) {
            walk(bytes, abs, rl, o, depth + 1, out);
        } else {
            out.entry(tc & 0x3fff).or_insert((abs, rl));
        }
    }
}

/// CIFF records the decoder consumes.
const RAW_DATA: u16 = 0x2005;
const SENSOR_INFO: u16 = 0x1031;
const DECODER_TABLE: u16 = 0x1835;
const MAKE_MODEL: u16 = 0x080a;
const IMAGE_INFO: u16 = 0x1810;
const TIME_STAMP: u16 = 0x180e;
const COLOR_BALANCE: u16 = 0x10a9;
const SHOT_INFO: u16 = 0x102a;

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let o = match bytes.get(0..2) {
        Some(b"II") => Order::Le,
        Some(b"MM") => Order::Be,
        _ => return Err(RawError::NotRaw),
    };
    if bytes.get(6..14) != Some(b"HEAPCCDR") {
        return Err(RawError::NotRaw);
    }
    let hl = u32_at(bytes, 2, o).ok_or_else(|| corrupt("truncated header"))? as usize;
    let heap_len = bytes.len().checked_sub(hl).filter(|&l| l >= 6).ok_or_else(|| corrupt("header length beyond the file"))?;
    let mut rec = BTreeMap::new();
    walk(bytes, hl, heap_len, o, 0, &mut rec);

    let &(so, sl) = rec.get(&SENSOR_INFO).ok_or_else(|| corrupt("no SensorInfo record"))?;
    let sensor: Vec<u16> = (0..sl / 2).map_while(|i| u16_at(bytes, so + i * 2, o)).collect();
    let (w, h) = match (sensor.get(1), sensor.get(2)) {
        (Some(&w), Some(&h)) => (w as usize, h as usize),
        _ => return Err(corrupt("SensorInfo too short")),
    };
    let n = w.checked_mul(h).filter(|&n| n > 0 && n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("CRW image too large"))?;

    let &(dto, dtl) = rec.get(&DECODER_TABLE).ok_or_else(|| corrupt("no DecoderTable record"))?;
    if dtl < 16 {
        return Err(corrupt("DecoderTable too short"));
    }
    let selector = u32_at(bytes, dto, o).ok_or_else(|| corrupt("truncated DecoderTable"))?;
    let ent_off = u32_at(bytes, dto + 8, o).ok_or_else(|| corrupt("truncated DecoderTable"))? as usize;
    let tables = crw_tables::tables(selector).ok_or_else(|| RawError::Unsupported(format!("CRW decoder table {selector}")))?;
    if ent_off > bytes.len() {
        return Err(corrupt("entropy offset beyond the file"));
    }

    // "Canon\0<model>\0"
    let (make, model) = rec
        .get(&MAKE_MODEL)
        .and_then(|&(mo, ml)| bytes.get(mo..mo + ml))
        .map(|v| {
            let mut parts = v.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).trim().to_string());
            (parts.next().unwrap_or_default(), parts.next().unwrap_or_default())
        })
        .unwrap_or_default();
    if model.contains("PowerShot G5") {
        return Err(RawError::Unsupported(
            "Canon PowerShot G5 CRW entropy coding (a documented anomaly in our derivation); the embedded JPEG is used instead".into(),
        ));
    }

    // active area from the SensorInfo crop (inclusive left, top, right, bottom)
    let active = match (sensor.get(5), sensor.get(6), sensor.get(7), sensor.get(8)) {
        (Some(&l), Some(&t), Some(&r), Some(&b))
            if (l as usize) < (r as usize) && (r as usize) < w && (t as usize) < (b as usize) && (b as usize) < h =>
        {
            Rect::new(l as usize, t as usize, r as usize - l as usize + 1, b as usize - t as usize + 1)
        }
        _ => Rect::new(0, 0, w, h),
    };

    let &(ro, rl) = rec.get(&RAW_DATA).ok_or_else(|| corrupt("no raw data record"))?;
    let payload =
        bytes.get(ro..ro.checked_add(rl).ok_or_else(|| corrupt("raw record size overflow"))?).ok_or_else(|| corrupt("raw data outside the file"))?;

    // Low-bits plane discriminator, two signs of a plane: (a) its raw bytes may hold an 0xff not
    // followed by 0x00, which the stuffed entropy stream never does; (b) the zero header of
    // `ent_off` bytes sits right after it, where a file without a plane is deep in entropy data.
    // (b) catches the D60 files whose lowest bit is almost always 0, so no plane byte is 0xff.
    let q = n.div_ceil(4);
    let span = q.saturating_add(ent_off);
    let head = payload.get(..span.min(payload.len())).unwrap_or(payload);
    let header_after_plane = ent_off >= 64 && payload.len() > span && payload.get(q..span).is_some_and(|z| z.iter().all(|&b| b == 0));
    let has_low = header_after_plane || head.windows(2).any(|p| p[0] == 0xff && p[1] != 0x00);
    let bits = if has_low { 12 } else { 10 };
    let low_len = if has_low { q } else { 0 };

    let data = if mode == Mode::Full {
        let entropy = payload.get(low_len.saturating_add(ent_off)..).ok_or_else(|| corrupt("payload shorter than its header"))?;
        let low = has_low.then(|| payload.get(..q)).flatten();
        if has_low && low.is_none() {
            return Err(corrupt("payload shorter than its low-bits plane"));
        }
        let hi = decode_entropy(entropy, w, h, &tables)?;
        let mut samples = vec![0u16; n];
        for (i, (dst, &hi)) in samples.iter_mut().zip(hi.iter()).enumerate() {
            *dst = match low {
                Some(low) => {
                    let lb = low.get(i >> 2).map_or(0, |&b| (b >> ((i & 3) * 2)) & 3);
                    ((((hi as u32) << 2) | lb as u32) & 0xffff) as u16
                }
                None => hi,
            };
        }
        samples
    } else {
        Vec::new()
    };

    // CFA phase at sensor (0, 0): RGGB on every verified body except the PowerShot G1 (GBRG),
    // established by chroma correlation against each body's embedded JPEG (derivation log).
    let cfa = Cfa::bayer_static(if model.contains("PowerShot G1") { "GBRG" } else { "RGGB" });

    let black = if mode == Mode::Full { black_from_borders(&data, w, h, active) } else { BlackLevel::uniform(0.0) };
    let white = if mode == Mode::Full { white_level(&data, bits) } else { ((1u32 << bits) - 1) as f32 };

    // ImageInfo rotation: degrees; 270 observed on portrait files whose scene is upright after a
    // 90° counter-clockwise turn (Exif 8), so 90 is its mirror (Exif 6)
    let orientation = match rec.get(&IMAGE_INFO).and_then(|&(io, il)| (il >= 16).then(|| u32_at(bytes, io + 12, o)).flatten()) {
        Some(90) => Orientation::from_exif(6),
        Some(180) => Orientation::from_exif(3),
        Some(270) => Orientation::from_exif(8),
        _ => Orientation::from_exif(1),
    };

    let metadata = Metadata {
        make: (!make.is_empty()).then_some(make),
        model: (!model.is_empty()).then(|| model.clone()),
        capture_time: rec
            .get(&TIME_STAMP)
            .and_then(|&(to, tl)| (tl >= 4).then(|| u32_at(bytes, to, o)).flatten())
            .and_then(|t| datetime_from_unix(t as i64)),
        width: Some(active.width as u32),
        height: Some(active.height as u32),
        orientation: Some(orientation),
        ..Metadata::default()
    };

    let img = RawImage {
        format: RawFormat::Crw,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits,
        black,
        white: vec![white],
        active_area: active,
        crop: Rect::new(0, 0, active.width, active.height),
        orientation,
        color: ColorData::default(),
        wb_multipliers: wb_from_color_balance(bytes, &rec, o),
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
    };
    img.validate_for(mode)?;
    Ok(img)
}

/// As-shot white-balance multipliers from the ColorBalance record (0x10a9, D60/10D/300D): a
/// byte-length word, then (R, G, G, B) level quadruples per white-balance setting, the last
/// quadruple being black levels (ExifTool's prose Canon/CanonRaw tag documentation names the
/// slots; the 10D/300D order is Auto, Daylight, Shade, Cloudy, Tungsten, Fluorescent, Flash,
/// Custom, Kelvin, and the D60's seven slots lack Shade and Kelvin). The slot is chosen by the
/// ShotInfo (0x102a) WhiteBalance value; multipliers are `level / green`, confirmed against the
/// cameras' own JPEG renderings (derivation log). Bodies without the record (D30, PowerShots)
/// return `None` and render with the neutral fallback.
fn wb_from_color_balance(bytes: &[u8], rec: &BTreeMap<u16, (usize, usize)>, o: Order) -> Option<[f32; 3]> {
    let &(bo, bl) = rec.get(&COLOR_BALANCE)?;
    let vals: Vec<u16> = (0..bl / 2).map_while(|i| u16_at(bytes, bo + i * 2, o)).collect();
    // quadruples after the length word; the last one is black levels
    let slots = vals.len().checked_sub(1)? / 4;
    let usable = slots.checked_sub(1)?;
    if usable == 0 {
        return None;
    }
    let setting = rec.get(&SHOT_INFO).and_then(|&(so, sl)| (sl / 2 > 7).then(|| u16_at(bytes, so + 14, o)).flatten()).unwrap_or(0);
    // Canon WhiteBalance values: 0 Auto, 1 Daylight, 2 Cloudy, 3 Tungsten, 4 Fluorescent,
    // 5 Flash, 6 Custom, 8 Shade, 9 Kelvin
    let slot = if usable >= 9 {
        match setting {
            1 => 1,
            8 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            9 => 8,
            _ => 0,
        }
    } else {
        match setting {
            s @ 1..=6 if (s as usize) < usable => s as usize,
            _ => 0,
        }
    };
    let q = vals.get(1 + slot.min(usable - 1) * 4..5 + slot.min(usable - 1) * 4)?;
    let g = (q.get(1).copied()? as f32 + q.get(2).copied()? as f32) / 2.0;
    let (r, b) = (q.first().copied()? as f32, q.get(3).copied()? as f32);
    if g <= 0.0 || r <= 0.0 || b <= 0.0 {
        return None;
    }
    let (mr, mb) = (r / g, b / g);
    ((0.2..=8.0).contains(&mr) && (0.2..=8.0).contains(&mb)).then_some([mr, 1.0, mb])
}

/// Fewest samples at the top of the data that count as the sensor's clip level (hot pixels are
/// fewer).
const CLIP_SAMPLES: usize = 64;

/// White level: the saturation plateau when the shared estimate finds one, else the top of the
/// data when it sits just under the full range.
///
/// These bodies clip below the full range at low ISO: on 1,409 D60 files at ISO 100–400 no
/// sample exceeds 4009 (most stop at 4000–4008), while ISO 800–1000 files reach 4095; the 10D
/// and 300D samples stop at 4003–4008 too. A photo with only a small blown area has no plateau
/// for [`white_from_data`], which then answered 4095 (373 of 925 ISO 100 files, 283 of 287 at
/// ISO 400): the blown pixels sat at 0.977 of white, under the clip threshold, and kept a
/// colour after white balance. So when at least [`CLIP_SAMPLES`] samples reach within 3 % of the
/// full range, the level they reach is the white level, lowered by the same 1/256 band the
/// plateau estimate uses.
fn white_level(data: &[u16], bits: u32) -> f32 {
    let full = (1u32 << bits.clamp(1, 16)) - 1;
    let plateau = white_from_data(data, bits);
    if plateau < full as f32 {
        return plateau;
    }
    let mut hist = vec![0usize; full as usize + 1];
    for &v in data {
        if let Some(h) = hist.get_mut(v as usize) {
            *h += 1;
        }
    }
    let mut above = 0usize;
    let top = hist.iter().enumerate().rev().find_map(|(v, &n)| {
        above += n;
        (above >= CLIP_SAMPLES).then_some(v as u32)
    });
    match top {
        Some(top) if top < full && top as f32 >= 0.97 * full as f32 => (top - (top / 256).max(1)) as f32,
        _ => full as f32,
    }
}

/// Black level from the masked sensor borders: the darkest flat strip among 8-column groups of
/// the left border and 4-row groups of the top border, with the strips within 8 DN of it averaged
/// in. A strip whose mean is ≤ 1 is clamped padding (observed on the D60's first rows), not a
/// black reference, and bright transition strips next to the active area only raise means, so the
/// minimum stays on true dark pixels.
fn black_from_borders(data: &[u16], w: usize, h: usize, active: Rect) -> BlackLevel {
    let mean_of = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| -> Option<f64> {
        let (mut sum, mut cnt) = (0f64, 0u64);
        for y in ys {
            let row = data.get(y.checked_mul(w)?..y.checked_mul(w)?.checked_add(w)?)?;
            for &v in row.get(xs.clone())? {
                sum += v as f64;
                cnt += 1;
            }
        }
        (cnt > 0).then(|| sum / cnt as f64)
    };
    let mut strips: Vec<(f64, std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    let rows = active.y..active.y.saturating_add(active.height).min(h);
    let mut x = 2usize;
    while x + 8 <= active.x {
        if let Some(m) = mean_of(x..x + 8, rows.clone()) {
            strips.push((m, x..x + 8, rows.clone()));
        }
        x += 8;
    }
    let cols = active.x..active.x.saturating_add(active.width).min(w);
    let mut y = 2usize;
    while y + 4 <= active.y {
        if let Some(m) = mean_of(cols.clone(), y..y + 4) {
            strips.push((m, cols.clone(), y..y + 4));
        }
        y += 4;
    }
    let Some(darkest) = strips.iter().map(|s| s.0).filter(|&m| m > 1.0).reduce(f64::min) else {
        return BlackLevel::uniform(0.0);
    };
    // per-CFA-site means over every strip near the darkest
    let (mut sum, mut cnt) = ([0f64; 4], [0u64; 4]);
    for (m, xs, ys) in &strips {
        // clamped padding (mean ≤ 1) is no black reference: counting it dragged the D60's level
        // from 128 down to 117 and lifted every shadow
        if *m <= 1.0 || *m > darkest + 8.0 {
            continue;
        }
        for y in ys.clone() {
            let py = (y as isize - active.y as isize).rem_euclid(2) as usize;
            for x in xs.clone() {
                let px = (x as isize - active.x as isize).rem_euclid(2) as usize;
                if let Some(&v) = data.get(y * w + x) {
                    sum[py * 2 + px] += v as f64;
                    cnt[py * 2 + px] += 1;
                }
            }
        }
    }
    if cnt.contains(&0) {
        return BlackLevel::uniform(darkest as f32);
    }
    BlackLevel { repeat_rows: 2, repeat_cols: 2, values: (0..4).map(|i| (sum[i] / cnt[i] as f64) as f32).collect(), delta_h: vec![], delta_v: vec![] }
}

/// A code-table lookup built for 16-bit MSB-first peeks: entry = `len << 16 | run << 8 | size`,
/// zero where no code matches.
fn build_lut(codes: &[(u16, u8, u8, u8)]) -> Vec<u32> {
    let mut lut = vec![0u32; 1 << 16];
    for &(code, len, r, s) in codes {
        if len == 0 || len > 16 {
            continue;
        }
        let span = 1usize << (16 - len);
        let base = (code as usize) << (16 - len);
        for e in lut.iter_mut().skip(base).take(span) {
            *e = (len as u32) << 16 | (r as u32) << 8 | s as u32;
        }
    }
    lut
}

/// Decode the ff00-stuffed entropy stream into the `w × h` high-plane samples, per the frozen
/// model (`clean-room/crw/06-crw-format.md` §2): 64-sample blocks, first/main code tables,
/// (run, size) leaves with JPEG-rule magnitudes, EOB, block-DC carry, and two parity DPCM chains
/// initialised to 512 and reset at every row start. A stream that ends early leaves the remaining
/// samples at their chain values (the frozen stream-end rule).
fn decode_entropy(entropy: &[u8], w: usize, h: usize, tables: &crw_tables::SelTables) -> Result<Vec<u16>> {
    // unstuff: an 0x00 after an 0xff is a stuffing byte
    let mut d = Vec::with_capacity(entropy.len());
    let mut i = 0;
    while let Some(&b) = entropy.get(i) {
        d.push(b);
        i += if b == 0xff && entropy.get(i + 1) == Some(&0x00) { 2 } else { 1 };
    }
    let nbits = d.len().saturating_mul(8);
    let peek16 = |pos: usize| -> u16 {
        let (byte, sh) = (pos >> 3, pos & 7);
        let b = |k: usize| d.get(byte + k).copied().unwrap_or(0) as u32;
        let w24 = b(0) << 16 | b(1) << 8 | b(2);
        ((w24 >> (8 - sh)) & 0xffff) as u16
    };

    let luts = [build_lut(tables.first), build_lut(tables.main)];
    let n = w.checked_mul(h).ok_or(RawError::Limit("CRW image too large"))?;
    let mut dec = vec![0u16; n];
    let (mut ce, mut co): (i32, i32) = (512, 512);
    let mut carry: i32 = 0;
    let mut pos = 0usize;
    let mut qi = 0usize;
    // emit one diff: row-start chain reset, parity chain update, 16-bit masked store
    macro_rules! emit {
        ($d:expr) => {{
            if qi % w == 0 {
                ce = 512;
                co = 512;
            }
            let c = if qi % 2 == 0 { &mut ce } else { &mut co };
            *c = c.wrapping_add($d);
            if let Some(p) = dec.get_mut(qi) {
                *p = (*c & 0xffff) as u16;
            }
            qi += 1;
        }};
    }
    'frame: while qi < n {
        let block_end = n.min((qi / 64 + 1) * 64);
        let mut tok = 0u32;
        while qi < block_end {
            if pos >= nbits.saturating_sub(1) {
                // stream exhausted: every remaining sample keeps its chain value
                while qi < n {
                    emit!(0);
                }
                break 'frame;
            }
            let e = luts[usize::from(tok > 0)].get(peek16(pos) as usize).copied().unwrap_or(0);
            if e == 0 {
                return Err(corrupt("undecodable entropy code"));
            }
            let (len, rr, s) = ((e >> 16) as usize, ((e >> 8) & 0xff) as usize, e & 0xff);
            if pos.saturating_add(len) > nbits {
                while qi < n {
                    emit!(0);
                }
                break 'frame;
            }
            pos += len;
            let mut rr = rr;
            if s == 0 && rr == 0 && tok > 0 {
                // end of block
                while qi < block_end {
                    emit!(0);
                }
                tok += 1;
                continue;
            }
            let diff: i32 = if s > 0 {
                if pos.saturating_add(s as usize) > nbits {
                    while qi < n {
                        emit!(0);
                    }
                    break 'frame;
                }
                let m = (peek16(pos) >> (16 - s)) as i32;
                pos += s as usize;
                // JPEG magnitude rule
                if m >= 1 << (s - 1) { m } else { m - (1 << s) + 1 }
            } else {
                0
            };
            let mut first = qi.is_multiple_of(64);
            if first && rr > 0 {
                // the block's first sample covered by the skip: its diff is exactly the carry
                emit!(carry);
                rr -= 1;
                first = false;
            }
            for _ in 0..rr {
                if qi >= block_end {
                    break;
                }
                emit!(0);
            }
            if qi >= block_end {
                tok += 1;
                continue;
            }
            let dd = if first {
                carry = carry.wrapping_add(diff);
                carry
            } else {
                diff
            };
            emit!(dd);
            tok += 1;
        }
    }
    Ok(dec)
}

/// Civil date-time from Unix seconds (the camera's local clock; inverse of
/// [`DateTime::unix_seconds`], Howard Hinnant's public-domain `civil_from_days`).
fn datetime_from_unix(t: i64) -> Option<DateTime> {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let dt = DateTime {
        year: u16::try_from(y).ok()?,
        month: u8::try_from(m).ok()?,
        day: u8::try_from(d).ok()?,
        hour: u8::try_from(secs / 3600).ok()?,
        minute: u8::try_from(secs / 60 % 60).ok()?,
        second: u8::try_from(secs % 60).ok()?,
        millis: 0,
        offset_minutes: None,
    };
    ((1970..=2200).contains(&dt.year)).then_some(dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put16(v: u16, o: Order) -> [u8; 2] {
        match o {
            Order::Le => v.to_le_bytes(),
            Order::Be => v.to_be_bytes(),
        }
    }
    fn put32(v: u32, o: Order) -> [u8; 4] {
        match o {
            Order::Le => v.to_le_bytes(),
            Order::Be => v.to_be_bytes(),
        }
    }

    /// A heap block: values, record table, trailing offset-table pointer.
    fn heap(records: &[(u16, Vec<u8>)], o: Order) -> Vec<u8> {
        let mut b = Vec::new();
        let mut entries = Vec::new();
        for (tc, v) in records {
            let off = b.len();
            b.extend_from_slice(v);
            if b.len() % 2 == 1 {
                b.push(0);
            }
            entries.push((*tc, v.len() as u32, off as u32));
        }
        let tbl = b.len() as u32;
        b.extend_from_slice(&put16(entries.len() as u16, o));
        for (tc, l, off) in entries {
            b.extend_from_slice(&put16(tc, o));
            b.extend_from_slice(&put32(l, o));
            b.extend_from_slice(&put32(off, o));
        }
        b.extend_from_slice(&put32(tbl, o));
        b
    }

    /// A complete CIFF file: 26-byte header + the root heap.
    fn ciff(records: &[(u16, Vec<u8>)], o: Order) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(match o {
            Order::Le => b"II",
            Order::Be => b"MM",
        });
        f.extend_from_slice(&put32(26, o));
        f.extend_from_slice(b"HEAPCCDR");
        f.extend_from_slice(&put32(0x0001_0002, o));
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&heap(records, o));
        f
    }

    fn sensor_info(w: u16, h: u16, crop: Option<(u16, u16, u16, u16)>, o: Order) -> Vec<u8> {
        let (l, t, r, b) = crop.unwrap_or((0, 0, 0, 0));
        [34, w, h, 0, 0, l, t, r, b, 0, 0, 0, 0, 0, 0, 0, 0].iter().flat_map(|&v| put16(v, o)).collect()
    }

    fn decoder_table(sel: u32, o: Order) -> Vec<u8> {
        [sel, 0, 514, 0].iter().flat_map(|&v| put32(v, o)).collect()
    }

    /// Entropy-encode a 10-bit high plane with the model's inverse: per-block first/main tables,
    /// (0, s) tokens only (the caller keeps mid-block diffs nonzero), JPEG magnitudes, carry,
    /// parity chains reset at row starts; MSB-first packing, zero padding, ff00 stuffing.
    fn encode_hi(hi: &[u16], w: usize, t: &crw_tables::SelTables) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let push_code = |bits: &mut Vec<bool>, tab: &[(u16, u8, u8, u8)], s: u32| {
            let &(c, l, ..) = tab.iter().find(|e| e.2 == 0 && e.3 as u32 == s).expect("(0, s) code exists");
            bits.extend((0..l).rev().map(|i| (c >> i) & 1 == 1));
        };
        let size_of = |d: i32| if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
        let (mut ce, mut co) = (512i32, 512i32);
        let mut carry = 0i32;
        for (qi, &v) in hi.iter().enumerate() {
            if qi % w == 0 {
                ce = 512;
                co = 512;
            }
            let c = if qi % 2 == 0 { &mut ce } else { &mut co };
            let dd = v as i32 - *c;
            *c = v as i32;
            let (tab, d) = if qi % 64 == 0 {
                let d = dd - carry;
                carry = dd;
                (t.first, d)
            } else {
                assert_ne!(dd, 0, "test images avoid zero diffs mid-block");
                (t.main, dd)
            };
            let s = size_of(d);
            push_code(&mut bits, tab, s);
            if s > 0 {
                let m = if d > 0 { d } else { d + (1 << s) - 1 } as u32;
                bits.extend((0..s).rev().map(|i| (m >> i) & 1 == 1));
            }
        }
        let mut bytes = Vec::new();
        for chunk in bits.chunks(8) {
            let mut b = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                b |= (bit as u8) << (7 - i);
            }
            bytes.push(b);
        }
        let mut stuffed = Vec::new();
        for b in bytes {
            stuffed.push(b);
            if b == 0xff {
                stuffed.push(0x00);
            }
        }
        stuffed
    }

    /// A high plane whose diffs are all nonzero, bounded around 512: values follow
    /// `(3 qi) mod 5 + 1`, so same-parity neighbours differ by 1 or −4 (never 0) and
    /// row starts sit 1–5 above the reset value.
    fn test_hi(n: usize) -> Vec<u16> {
        (0..n).map(|qi| 512 + (qi as u16 * 3) % 5 + 1).collect()
    }

    fn payload(low: Option<&[u8]>, entropy: &[u8]) -> Vec<u8> {
        let mut p = low.map(|l| l.to_vec()).unwrap_or_default();
        p.extend_from_slice(&[0; 514]);
        p.extend_from_slice(entropy);
        p.extend_from_slice(&[0, 0]);
        p
    }

    fn make_model(model: &str) -> Vec<u8> {
        let mut v = b"Canon\0".to_vec();
        v.extend_from_slice(model.as_bytes());
        v.push(0);
        v
    }

    fn roundtrip_file(w: u16, h: u16, model: &str, low: Option<&[u8]>, hi: &[u16], o: Order) -> Vec<u8> {
        let tables = crw_tables::tables(0).expect("selector 0");
        let entropy = encode_hi(hi, w as usize, &tables);
        // the realistic shape: raw data in the root, properties in a nested heap
        let props = heap(&[(SENSOR_INFO, sensor_info(w, h, None, o)), (DECODER_TABLE, decoder_table(0, o)), (MAKE_MODEL, make_model(model))], o);
        ciff(&[(RAW_DATA, payload(low, &entropy)), (0x300a, props)], o)
    }

    #[test]
    fn roundtrips_a_10_bit_frame() {
        let (w, h) = (16u16, 8u16);
        let hi = test_hi(128);
        let f = roundtrip_file(w, h, "Canon EOS D60", None, &hi, Order::Le);
        let img = decode(&f, Mode::Full).expect("decodes");
        assert_eq!((img.width, img.height, img.bits), (16, 8, 10));
        assert_eq!(img.data, RawData::U16(hi));
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some("RGGB"));
        assert_eq!(img.metadata.model.as_deref(), Some("Canon EOS D60"));
    }

    #[test]
    fn roundtrips_a_12_bit_frame_with_a_low_bits_plane() {
        let (w, h) = (16u16, 8u16);
        let hi = test_hi(128);
        // low bits all 3: the plane is all 0xff bytes, which trips the presence rule
        let low = vec![0xffu8; 32];
        let f = roundtrip_file(w, h, "Canon EOS D60", Some(&low), &hi, Order::Le);
        let img = decode(&f, Mode::Full).expect("decodes");
        assert_eq!(img.bits, 12);
        let expected: Vec<u16> = hi.iter().map(|&v| (v << 2) | 3).collect();
        assert_eq!(img.data, RawData::U16(expected));
    }

    /// A low-bits plane without a single 0xff byte (real D60 files whose lowest bit is nearly
    /// always 0) used to be taken for "no plane": the plane was read as entropy data and the
    /// file came out as 10-bit garbage. The zero header after the plane gives it away.
    #[test]
    fn a_low_bits_plane_without_ff_bytes_is_still_a_plane() {
        let hi = test_hi(128);
        for byte in [0x22u8, 0x00] {
            let low = vec![byte; 32];
            let f = roundtrip_file(16, 8, "Canon EOS D60", Some(&low), &hi, Order::Le);
            let img = decode(&f, Mode::Full).expect("decodes");
            assert_eq!(img.bits, 12);
            let expected: Vec<u16> = hi.iter().enumerate().map(|(i, &v)| (v << 2) | u16::from((byte >> ((i & 3) * 2)) & 3)).collect();
            assert_eq!(img.data, RawData::U16(expected));
        }
    }

    #[test]
    fn big_endian_ciff_decodes_too() {
        let hi = test_hi(128);
        let f = roundtrip_file(16, 8, "Canon EOS D60", None, &hi, Order::Be);
        let img = decode(&f, Mode::Full).expect("decodes");
        assert_eq!(img.data, RawData::U16(hi));
    }

    #[test]
    fn header_mode_reads_geometry_without_samples() {
        let hi = test_hi(128);
        let f = roundtrip_file(16, 8, "Canon EOS D60", None, &hi, Order::Le);
        let img = decode(&f, Mode::Header).expect("header decodes");
        assert_eq!((img.width, img.height), (16, 8));
        assert_eq!(img.data.len(), 0);
    }

    #[test]
    fn g1_is_gbrg() {
        let hi = test_hi(128);
        let f = roundtrip_file(16, 8, "Canon PowerShot G1", None, &hi, Order::Le);
        let img = decode(&f, Mode::Full).expect("decodes");
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some("GBRG"));
    }

    #[test]
    fn g5_is_refused_with_its_anomaly_note() {
        let hi = test_hi(128);
        let f = roundtrip_file(16, 8, "Canon PowerShot G5", None, &hi, Order::Le);
        let Err(RawError::Unsupported(why)) = decode(&f, Mode::Full) else { panic!("expected Unsupported") };
        assert!(why.contains("G5"), "{why}");
    }

    /// A D60 at ISO 100 clips near 4000, and a photo with a small blown area has no plateau:
    /// the white level must still come down to where the data stops, not stay at 4095. A few
    /// hot pixels, or a photo that never gets near the top, leave the full range.
    #[test]
    fn white_level_follows_a_small_clipped_area() {
        let mut data: Vec<u16> = (0..200_000).map(|i| 300 + (i % 3000) as u16).collect();
        assert_eq!(white_level(&data, 12), 4095.0, "nothing near the top");
        for v in data.iter_mut().take(100) {
            *v = 4002;
        }
        assert_eq!(white_level(&data, 12), (4002 - 15) as f32, "100 samples at the clip level");
        let mut hot: Vec<u16> = (0..200_000).map(|i| 300 + (i % 3000) as u16).collect();
        for v in hot.iter_mut().take(10) {
            *v = 4002;
        }
        assert_eq!(white_level(&hot, 12), 4095.0, "ten hot pixels are not a clip level");
    }

    /// The D60 layout: sensor rows clamped to 0 above the image, dark columns left of it. The
    /// clamped rows must not count towards the black level.
    #[test]
    fn black_level_ignores_clamped_rows() {
        let (w, h) = (64usize, 40usize);
        let active = Rect::new(24, 16, 40, 24);
        let mut data = vec![900u16; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = if y < 7 {
                    0
                } else if x < 24 {
                    128
                } else {
                    900
                };
            }
        }
        let black = black_from_borders(&data, w, h, active);
        assert!(black.values.iter().all(|v| (v - 128.0).abs() < 0.5), "{:?}", black.values);
    }

    /// The frozen stream-end rule: a truncated entropy stream leaves the remaining samples at
    /// their chain values instead of failing the file.
    #[test]
    fn truncated_stream_keeps_chain_values() {
        let (w, h) = (16u16, 8u16);
        let hi = test_hi(128);
        let tables = crw_tables::tables(0).expect("selector 0");
        let entropy = encode_hi(&hi, 16, &tables);
        let cut = entropy.len() / 2;
        let props = heap(
            &[
                (SENSOR_INFO, sensor_info(w, h, None, Order::Le)),
                (DECODER_TABLE, decoder_table(0, Order::Le)),
                (MAKE_MODEL, make_model("Canon EOS D60")),
            ],
            Order::Le,
        );
        let f = ciff(&[(RAW_DATA, payload(None, &entropy[..cut])), (0x300a, props)], Order::Le);
        let img = decode(&f, Mode::Full).expect("truncated stream still decodes");
        let RawData::U16(got) = &img.data else { panic!("not U16") };
        assert_eq!(got.len(), 128);
        assert_eq!(&got[..32], &hi[..32], "the leading samples decode exactly");
    }

    /// Every record the decoder needs missing, bogus selectors, dimensions and offsets: errors,
    /// never panics; and every truncation of a valid file survives.
    #[test]
    fn hostile_inputs_error_cleanly() {
        let o = Order::Le;
        let hi = test_hi(128);
        let good = roundtrip_file(16, 8, "Canon EOS D60", None, &hi, o);
        for n in 0..good.len() {
            let _ = decode(&good[..n], Mode::Full);
        }
        // single-byte mutations over the whole file
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0xa5;
            let _ = decode(&bad, Mode::Full);
        }
        let props_no_sensor = heap(&[(DECODER_TABLE, decoder_table(0, o))], o);
        let f = ciff(&[(RAW_DATA, vec![0; 600]), (0x300a, props_no_sensor)], o);
        assert!(matches!(decode(&f, Mode::Full), Err(RawError::Corrupt(_))));
        let props_bad_sel = heap(&[(SENSOR_INFO, sensor_info(16, 8, None, o)), (DECODER_TABLE, decoder_table(7, o))], o);
        let f = ciff(&[(RAW_DATA, vec![0; 600]), (0x300a, props_bad_sel)], o);
        assert!(matches!(decode(&f, Mode::Full), Err(RawError::Unsupported(_))));
        let props_zero = heap(&[(SENSOR_INFO, sensor_info(0, 0, None, o)), (DECODER_TABLE, decoder_table(0, o))], o);
        let f = ciff(&[(RAW_DATA, vec![0; 600]), (0x300a, props_zero)], o);
        assert!(decode(&f, Mode::Full).is_err());
        // a heap that nests into itself must hit the depth bound, not recurse forever
        let mut self_heap = heap(&[(0x300a, vec![0; 10])], o);
        let len = self_heap.len() as u32;
        // point the nested record at the whole block
        let tbl = u32::from_le_bytes(self_heap[len as usize - 4..].try_into().unwrap()) as usize;
        self_heap[tbl + 4..tbl + 8].copy_from_slice(&len.to_le_bytes());
        self_heap[tbl + 8..tbl + 12].copy_from_slice(&0u32.to_le_bytes());
        let mut f = ciff(&[], o);
        f.truncate(26);
        f.extend_from_slice(&self_heap);
        assert!(decode(&f, Mode::Full).is_err());
    }

    /// The frozen tables are prefix-free within each table, so the 16-bit lookup is unambiguous.
    #[test]
    fn frozen_tables_are_prefix_free() {
        for sel in 0..3 {
            let t = crw_tables::tables(sel).expect("selector");
            for (name, tab) in [("first", t.first), ("main", t.main)] {
                let mut lut = vec![false; 1 << 16];
                for &(code, len, ..) in tab {
                    assert!((1..=16).contains(&len), "sel {sel} {name}: code length {len}");
                    let base = (code as usize) << (16 - len);
                    for cell in lut.iter_mut().skip(base).take(1 << (16 - len)) {
                        assert!(!*cell, "sel {sel} {name}: overlapping codes");
                        *cell = true;
                    }
                }
            }
        }
    }

    /// Every second table codes all 162 symbols: (r, s) for r 0..=15 and s 1..=10, end of block
    /// and (15, 0). Selectors 0 and 1 lacked (7..=14, 10) and (14, 10), learned from too few
    /// files, and 223 of 1,516 real D60 files failed with "undecodable entropy code".
    #[test]
    fn second_tables_code_every_symbol() {
        for sel in 0..3 {
            let t = crw_tables::tables(sel).expect("selector");
            let have: std::collections::BTreeSet<(u8, u8)> = t.main.iter().map(|e| (e.2, e.3)).collect();
            let mut want: std::collections::BTreeSet<(u8, u8)> = (0..16).flat_map(|r| (1..=10).map(move |s| (r, s))).collect();
            want.extend([(0, 0), (15, 0)]);
            assert_eq!(have, want, "selector {sel}");
            assert_eq!(t.main.len(), 162, "selector {sel}: one code per symbol");
        }
    }

    #[test]
    fn white_balance_comes_from_the_shot_info_slot() {
        let o = Order::Le;
        let hi = test_hi(128);
        // D60-shaped table: 7 slots + black levels; Daylight slot (1) = 1800, 800, 800, 960
        let mut cb: Vec<u16> = vec![66];
        cb.extend([2000u16, 760, 760, 830]);
        cb.extend([1800u16, 800, 800, 960]);
        for _ in 0..5 {
            cb.extend([1500u16, 900, 900, 1500]);
        }
        cb.extend([126u16, 127, 126, 126]);
        let cb: Vec<u8> = cb.iter().flat_map(|&v| put16(v, o)).collect();
        let mut shot: Vec<u16> = vec![54; 27];
        shot[7] = 1; // Daylight
        let shot: Vec<u8> = shot.iter().flat_map(|&v| put16(v, o)).collect();
        let tables = crw_tables::tables(0).expect("selector 0");
        let props = heap(
            &[
                (SENSOR_INFO, sensor_info(16, 8, None, o)),
                (DECODER_TABLE, decoder_table(0, o)),
                (MAKE_MODEL, make_model("Canon EOS D60")),
                (COLOR_BALANCE, cb),
                (SHOT_INFO, shot),
            ],
            o,
        );
        let f = ciff(&[(RAW_DATA, payload(None, &encode_hi(&hi, 16, &tables))), (0x300a, props)], o);
        let img = decode(&f, Mode::Full).expect("decodes");
        let wb = img.wb_multipliers.expect("wb");
        assert!((wb[0] - 2.25).abs() < 1e-3 && wb[1] == 1.0 && (wb[2] - 1.2).abs() < 1e-3, "{wb:?}");
    }

    #[test]
    fn rotation_maps_to_exif_orientation() {
        let o = Order::Le;
        let hi = test_hi(128);
        let image_info: Vec<u8> = [3072u32, 2048, 0, 270, 12, 12, 0].iter().flat_map(|&v| put32(v, o)).collect();
        let props = heap(
            &[
                (SENSOR_INFO, sensor_info(16, 8, None, o)),
                (DECODER_TABLE, decoder_table(0, o)),
                (MAKE_MODEL, make_model("Canon EOS 300D DIGITAL")),
                (IMAGE_INFO, image_info),
                (TIME_STAMP, [1083410638u32, 0, 0].iter().flat_map(|&v| put32(v, o)).collect()),
            ],
            o,
        );
        let tables = crw_tables::tables(0).expect("selector 0");
        let f = ciff(&[(RAW_DATA, payload(None, &encode_hi(&hi, 16, &tables))), (0x300a, props)], o);
        let img = decode(&f, Mode::Full).expect("decodes");
        assert_eq!(img.orientation, Orientation::from_exif(8));
        let t = img.metadata.capture_time.expect("capture time");
        assert_eq!(t.unix_seconds(), 1083410638);
    }

    #[test]
    fn unix_datetime_inverse_matches_unix_seconds() {
        for t in [0i64, 1, 86399, 86400, 951867295, 1083410638, 1600000000, 4102444799] {
            let dt = datetime_from_unix(t).expect("in range");
            assert_eq!(dt.unix_seconds(), t, "{t}");
        }
        assert!(datetime_from_unix(-1).is_none() || datetime_from_unix(-1).map(|d| d.year).unwrap_or(0) >= 1969);
    }
}
