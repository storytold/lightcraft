//! Canon CRW (CIFF "heap" container), the PowerShot and EOS bodies of 2000-2004.
//!
//! # Provenance (clean room)
//!
//! Everything below was derived from the sample files themselves plus public specifications: the CIFF heap
//! layout (a heap of blocks whose last four bytes point at a directory of 10-byte entries) was read from the
//! files, and the pixel coding is ordinary ITU-T T.81 Annex C / F.1.2 Huffman coding (public) used as a bare entropy
//! coder. No other raw decoder was consulted. The numeric facts used here were measured on the samples:
//!
//! - Raw blob (CIFF tag `0x2005`): one of three layouts, told apart by structure.
//!   - *headered* (PowerShot G3 / G5 / G6 / Pro1 / S45 / S50 / S60 / S70): `W*H/4` bytes of low bits, then a
//!     JPEG-style header (SOI, DQT, SOF0, DHT, SOS) that carries the Huffman tables, then the entropy stream.
//!   - *12-bit headerless* (EOS D30 / D60 / 10D / 300D): `W*H/4` bytes of low bits, 514 zero bytes, the entropy
//!     stream. The tables are selected by the first word of tag `0x1835`.
//!   - *10-bit headerless* (PowerShot G1 / G2 / S30 / S40): 514 zero bytes, the entropy stream, no low bits.
//! - Entropy stream: `W*H/64` blocks, each 64 consecutive samples in raster order, coded as a T.81 DC symbol
//!   (category, extra bits) followed by AC symbols (run/size, ZRL, EOB). Stuffed `ff 00` pairs are unstuffed.
//! - Sample model: the 0th value of each block is the difference of the previous block's 0th value (a cumulative
//!   sum over blocks gives the value). Every row is then two interleaved DPCM chains, `p[x] = p[x-2] + e[x]`.
//!   Samples are `p + 512`: 10-bit `v = p + 512`, 12-bit `v = 4 * (p + 512) + low`, the low bits being two bits
//!   per sample, four samples per byte, least significant first, in raster order.
//! - Sensor-info record (`0x1031`, in the `0x300a/0x300b` directory): `[len, W, H, 1, 1, left, top, right, bottom,
//!   mleft, mtop, mright, mbottom]` (u16), the active area (inclusive) and an optically masked area used for the
//!   black level. The Bayer layout is RGGB from the full frame's (0, 0), found by regressing the four site classes
//!   on the embedded preview's linearised colour channels.
//!
//! Gaps: the third Huffman table (selected on later 10D / 300D firmware) is not derived yet, and the PowerShot Pro70
//! (no sensor-info record) is not decoded; both return [`RawError::Unsupported`] and keep the embedded-JPEG preview.

use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lightcraft_geom::Orientation;
use lightcraft_meta::Metadata;
use std::collections::HashMap;

const SENSOR_INFO: u16 = 0x1031;
const TABLE_INDEX: u16 = 0x1835;
const RAW_DATA: u16 = 0x2005;
const IMAGE_SPEC: u16 = 0x1810;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 6;
const ZERO_PAD: usize = 514;

// Huffman tables 0 and 1 (counts per code length, then symbols in code order), read from the headered files.
const T0_DC_COUNTS: [u8; 16] = [0, 1, 4, 2, 3, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const T0_DC_VALUES: [u8; 12] = [4, 3, 5, 6, 2, 7, 1, 8, 9, 0, 10, 11];
const T0_AC_COUNTS: [u8; 16] = [0, 2, 2, 2, 1, 4, 2, 1, 2, 5, 1, 1, 0, 0, 0, 139];
const T0_AC_VALUES: [u8; 162] = [
    3, 4, 2, 5, 1, 6, 7, 8, 18, 19, 17, 20, 9, 21, 34, 0, 33, 22, 10, 240, 35, 23, 36, 49, 50, 24, 25, 51, 37, 65, 52, 66, 53, 81, 54, 55, 56, 41,
    121, 38, 26, 57, 86, 87, 40, 39, 82, 85, 88, 67, 118, 89, 119, 84, 97, 249, 113, 120, 117, 150, 151, 73, 183, 83, 215, 116, 182, 152, 71, 72,
    149, 105, 153, 145, 250, 184, 104, 181, 185, 214, 247, 216, 103, 70, 69, 148, 137, 248, 129, 213, 246, 180, 136, 177, 42, 68, 114, 217, 135, 102,
    212, 245, 58, 167, 115, 169, 168, 134, 98, 199, 101, 200, 201, 161, 244, 209, 233, 90, 146, 133, 166, 231, 147, 232, 193, 198, 122, 100, 225, 74,
    106, 230, 179, 241, 211, 165, 138, 178, 154, 186, 132, 164, 99, 229, 197, 243, 210, 196, 130, 170, 218, 228, 242, 202, 131, 163, 162, 195, 234,
    194, 226, 227,
];
const T1_DC_COUNTS: [u8; 16] = [0, 2, 2, 3, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const T1_DC_VALUES: [u8; 12] = [3, 2, 4, 1, 5, 0, 6, 7, 9, 8, 10, 11];
const T1_AC_COUNTS: [u8; 16] = [0, 2, 2, 1, 4, 1, 4, 1, 3, 3, 1, 0, 0, 0, 0, 140];
const T1_AC_VALUES: [u8; 162] = [
    2, 3, 1, 4, 5, 18, 17, 6, 19, 7, 8, 20, 34, 9, 33, 0, 35, 21, 49, 50, 10, 22, 240, 36, 51, 65, 66, 25, 23, 37, 24, 81, 52, 67, 82, 41, 53, 97,
    57, 113, 98, 54, 83, 38, 56, 26, 55, 129, 39, 145, 121, 85, 69, 40, 114, 89, 161, 177, 68, 105, 84, 88, 209, 250, 87, 225, 241, 185, 73, 71, 99,
    106, 249, 86, 70, 168, 42, 74, 120, 153, 58, 117, 116, 134, 101, 193, 118, 182, 150, 214, 137, 133, 201, 245, 149, 180, 199, 247, 138, 151, 184,
    115, 183, 216, 217, 135, 167, 122, 72, 130, 132, 234, 244, 166, 197, 90, 148, 164, 198, 146, 195, 104, 181, 200, 228, 229, 230, 233, 162, 163,
    227, 194, 102, 103, 147, 170, 212, 213, 231, 248, 136, 154, 215, 119, 196, 100, 226, 152, 165, 202, 218, 232, 243, 246, 169, 178, 179, 242, 210,
    131, 186, 211,
];

/// Directory entries by (hash of the enclosing directory chain, tag): absolute offset and length of the value.
type Entries = HashMap<(u32, u16), (usize, usize)>;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

fn chain(path: u32, tag: u16) -> u32 {
    path.wrapping_mul(0x9e37_79b1).wrapping_add(u32::from(tag) + 1)
}

fn path_of(tags: &[u16]) -> u32 {
    tags.iter().fold(0u32, |p, &t| chain(p, t))
}

/// Walk the heap: a block's last four bytes give its directory offset (from the block start); the directory is a
/// u16 count and 10-byte entries (type code, length, offset). Type-code bits 14-15 = 1 store the value in the entry.
fn walk(b: &[u8], start: usize, end: usize, path: u32, depth: usize, out: &mut Entries) -> Option<()> {
    if depth > MAX_DEPTH || end < start.checked_add(6)? || end > b.len() {
        return None;
    }
    let dir = start.checked_add(u32_at(b, end - 4)? as usize)?;
    let n = u16_at(b, dir)? as usize;
    for i in 0..n {
        if out.len() > MAX_ENTRIES {
            return None;
        }
        let e = dir.checked_add(2 + i * 10)?;
        let tc = u16_at(b, e)?;
        let (len, off) = (u32_at(b, e + 2)? as usize, u32_at(b, e + 6)? as usize);
        let tag = tc & 0x3fff;
        if tc >> 14 == 1 {
            out.insert((path, tag), (e + 2, 8));
            continue;
        }
        let abs = start.checked_add(off)?;
        if abs.checked_add(len)? > end {
            continue;
        }
        if matches!((tc >> 8) & 0x38, 0x28 | 0x30) {
            let _ = walk(b, abs, abs + len, chain(path, tag), depth + 1, out);
        } else {
            out.insert((path, tag), (abs, len));
        }
    }
    Some(())
}

struct Huff {
    /// Indexed by the next 16 bits: `len << 8 | symbol`, 0 where no code matches.
    lut: Vec<u16>,
}

impl Huff {
    fn new(counts: &[u8], values: &[u8]) -> Result<Huff> {
        let mut lut = vec![0u16; 1 << 16];
        let (mut code, mut k) = (0u32, 0usize);
        for l in 1..=16u32 {
            for _ in 0..counts[l as usize - 1] {
                let sym = *values.get(k).ok_or_else(|| RawError::Corrupt("CRW Huffman table short".into()))?;
                k += 1;
                if code >= 1 << l {
                    return Err(RawError::Corrupt("CRW Huffman table overfull".into()));
                }
                let lo = (code << (16 - l)) as usize;
                let hi = ((code + 1) << (16 - l)) as usize;
                for e in &mut lut[lo..hi] {
                    *e = (l as u16) << 8 | u16::from(sym);
                }
                code += 1;
            }
            code <<= 1;
        }
        Ok(Huff { lut })
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    total: usize,
}

impl Bits<'_> {
    fn peek16(&self) -> u32 {
        let o = self.pos >> 3;
        let mut w = 0u32;
        for i in 0..3 {
            w = w << 8 | u32::from(self.d.get(o + i).copied().unwrap_or(0));
        }
        (w >> (8 - (self.pos & 7))) & 0xffff
    }
    fn sym(&mut self, h: &Huff) -> Result<u32> {
        let e = h.lut[self.peek16() as usize];
        let l = (e >> 8) as usize;
        if l == 0 || self.pos + l > self.total {
            return Err(RawError::Corrupt("CRW entropy stream ends or has a bad code".into()));
        }
        self.pos += l;
        Ok(u32::from(e & 0xff))
    }
    fn value(&mut self, size: u32) -> Result<i32> {
        if size == 0 {
            return Ok(0);
        }
        if size > 16 || self.pos + size as usize > self.total {
            return Err(RawError::Corrupt("CRW entropy stream ends".into()));
        }
        let v = (self.peek16() >> (16 - size)) as i32;
        self.pos += size as usize;
        Ok(if v >= 1 << (size - 1) { v } else { v - (1 << size) + 1 })
    }
}

fn unstuff(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        out.push(raw[i]);
        if raw[i] == 0xff && raw.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    out
}

type Table = (Vec<u8>, Vec<u8>);

/// DC and AC table of the headered layout's scan, and the offset just after SOS.
fn parse_header(b: &[u8]) -> Option<(Table, Table, usize)> {
    let mut dc: [Option<Table>; 4] = [None, None, None, None];
    let mut ac: [Option<Table>; 4] = [None, None, None, None];
    let mut p = 2;
    loop {
        if *b.get(p)? != 0xff {
            return None;
        }
        let m = *b.get(p + 1)?;
        let len = u16::from_be_bytes([*b.get(p + 2)?, *b.get(p + 3)?]) as usize;
        let seg = b.get(p + 4..p + 2 + len)?;
        match m {
            0xc4 => {
                let mut q = 0;
                while q + 17 <= seg.len() {
                    let (class, id) = (seg[q] >> 4, (seg[q] & 15) as usize);
                    let counts = seg[q + 1..q + 17].to_vec();
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(q + 17..q + 17 + n)?.to_vec();
                    q += 17 + n;
                    if id < 4 {
                        if class == 0 {
                            dc[id] = Some((counts, vals));
                        } else {
                            ac[id] = Some((counts, vals));
                        }
                    }
                }
            }
            0xda => {
                let sel = *seg.get(2)?;
                let (td, ta) = ((sel >> 4) as usize & 3, (sel & 15) as usize & 3);
                return Some((dc[td].take()?, ac[ta].take()?, p + 2 + len));
            }
            _ => {}
        }
        p += 2 + len;
    }
}

struct Plan<'a> {
    low: Option<&'a [u8]>,
    stream: &'a [u8],
    bits: u32,
    dc: Huff,
    ac: Huff,
}

fn plan(blob: &[u8], w: usize, h: usize, table: u32) -> Result<Plan<'_>> {
    let lb = w * h / 4;
    if blob.get(lb..lb + 2) == Some(&[0xff, 0xd8]) {
        let (dc, ac, after) = parse_header(&blob[lb..]).ok_or_else(|| RawError::Corrupt("CRW header".into()))?;
        let dcn: usize = dc.0.iter().map(|&c| c as usize).sum();
        let acn: usize = ac.0.iter().map(|&c| c as usize).sum();
        if dc.0.len() != 16 || ac.0.len() != 16 || dcn > 17 || acn > 256 {
            return Err(RawError::Corrupt("CRW Huffman table".into()));
        }
        return Ok(Plan {
            low: Some(&blob[..lb]),
            stream: &blob[lb + after..],
            bits: 12,
            dc: Huff::new(&dc.0, &dc.1)?,
            ac: Huff::new(&ac.0, &ac.1)?,
        });
    }
    let zeros = |o: usize| blob.get(o..o + ZERO_PAD).is_some_and(|z| z.iter().all(|&x| x == 0));
    let (low, start, bits) = if zeros(lb) {
        (Some(&blob[..lb]), lb + ZERO_PAD, 12)
    } else if zeros(0) {
        (None, ZERO_PAD, 10)
    } else {
        return Err(RawError::Unsupported("CRW raw layout not recognised".into()));
    };
    let (dc, ac) = match table {
        0 => (Huff::new(&T0_DC_COUNTS, &T0_DC_VALUES)?, Huff::new(&T0_AC_COUNTS, &T0_AC_VALUES)?),
        1 => (Huff::new(&T1_DC_COUNTS, &T1_DC_VALUES)?, Huff::new(&T1_AC_COUNTS, &T1_AC_VALUES)?),
        t => return Err(RawError::Unsupported(format!("CRW Huffman table {t} is not decoded yet (preview only)"))),
    };
    Ok(Plan { low, stream: &blob[start..], bits, dc, ac })
}

/// Entropy-decode and integrate the samples into `w * h` values.
fn samples(p: &Plan, w: usize, h: usize) -> Result<Vec<u16>> {
    let n = w * h;
    let s = unstuff(p.stream);
    let mut br = Bits { d: &s, pos: 0, total: s.len() * 8 };
    let mut e = vec![0i32; n];
    let mut acc = 0i32;
    for blk in e.chunks_exact_mut(64) {
        let cat = br.sym(&p.dc)?;
        if cat > 15 {
            return Err(RawError::Corrupt("CRW DC category".into()));
        }
        acc = acc.wrapping_add(br.value(cat)?);
        blk[0] = acc;
        let mut i = 1usize;
        while i < 64 {
            let rs = br.sym(&p.ac)?;
            let (run, size) = ((rs >> 4) as usize, rs & 15);
            if size == 0 {
                if run == 15 {
                    i += 16;
                    continue;
                }
                break;
            }
            i += run;
            if i > 63 {
                return Err(RawError::Corrupt("CRW coefficient run overruns its block".into()));
            }
            blk[i] = br.value(size)?;
            i += 1;
        }
    }
    let mut out = vec![0u16; n];
    let max = (1i64 << p.bits) - 1;
    for (y, row) in e.chunks_exact_mut(w).enumerate() {
        for x in 2..w {
            row[x] = row[x].wrapping_add(row[x - 2]);
        }
        for (x, v) in row.iter().enumerate() {
            let i = y * w + x;
            let v = i64::from(v.wrapping_add(512));
            let v = match p.low {
                Some(l) => 4 * v + i64::from((l[i >> 2] >> (2 * (i & 3))) & 3),
                None => v,
            };
            out[i] = v.clamp(0, max) as u16;
        }
    }
    Ok(out)
}

fn cstr(b: &[u8]) -> Option<String> {
    let s: Vec<u8> = b.iter().copied().take_while(|&c| c != 0).collect();
    let t = String::from_utf8_lossy(&s).trim().to_string();
    (!t.is_empty()).then_some(t)
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let hl = u32_at(bytes, 2).ok_or(RawError::NotRaw)? as usize;
    let mut ent = Entries::new();
    walk(bytes, hl, bytes.len(), 0, 0, &mut ent).ok_or_else(|| RawError::Corrupt("CRW heap".into()))?;
    let sub = path_of(&[0x300a, 0x300b]);
    let get = |path: u32, tag: u16| ent.get(&(path, tag)).and_then(|&(o, l)| bytes.get(o..o + l));
    let si = get(sub, SENSOR_INFO).ok_or_else(|| RawError::Unsupported("CRW without a sensor-info record (preview only)".into()))?;
    let f = |i: usize| u16_at(si, 2 * i).map(usize::from);
    let (Some(w), Some(h), Some(l), Some(t), Some(r), Some(bt)) = (f(1), f(2), f(5), f(6), f(7), f(8)) else {
        return Err(RawError::Corrupt("CRW sensor info".into()));
    };
    let n = crate::checked_pixel_count(w as u64, h as u64).ok_or(RawError::Limit("image too large"))? as usize;
    if n % 64 != 0 || w % 2 != 0 || r < l || bt < t || r >= w || bt >= h {
        return Err(RawError::Corrupt("CRW geometry".into()));
    }
    let active = Rect::new(l, t, r - l + 1, bt - t + 1);
    let table = get(sub, TABLE_INDEX).and_then(|b| u32_at(b, 0)).unwrap_or(0);
    let blob = get(0, RAW_DATA).ok_or_else(|| RawError::Corrupt("CRW without raw data".into()))?;
    let plan = plan(blob, w, h, table)?;
    let (data, black, white) = if mode == Mode::Full {
        let d = samples(&plan, w, h)?;
        let black = match (f(9), f(10), f(11), f(12)) {
            (Some(ml), Some(mt), Some(mr), Some(mb)) if mr > ml && mb > mt && mr < w && mb < h => {
                super::black_from_columns(&d, w, ml..mr + 1, mt..mb + 1, active)
            }
            _ => BlackLevel::uniform(0.0),
        };
        let white = super::white_from_data(&super::active_rows(&d, w, active), plan.bits);
        (RawData::U16(d), black, white)
    } else {
        (RawData::U16(Vec::new()), BlackLevel::uniform(0.0), ((1u32 << plan.bits) - 1) as f32)
    };
    let mut metadata = Metadata::default();
    if let Some(mm) = get(path_of(&[0x300a, 0x2807]), 0x080a) {
        let mut parts = mm.split(|&c| c == 0);
        metadata.make = parts.next().and_then(cstr);
        metadata.model = parts.next().and_then(cstr);
    }
    // The G1 carries a complementary (CMYG) filter array, which the RGB
    // pipeline cannot represent: keep it on the embedded-preview path.
    if metadata.model.as_deref().map(str::trim) == Some("Canon PowerShot G1") {
        return Err(RawError::Unsupported("CRW CMYG sensor (PowerShot G1) is preview only".into()));
    }
    // Camera white balance, where recorded: [R, G1, G2, B] gains in 1/1024 units.
    let wb = get(path_of(&[0x300a, 0x300b]), 0x10aa).and_then(|b| {
        let v = |i: usize| u16_at(b, 2 * i).map(f32::from);
        let (r, g1, g2, bl) = (v(1)?, v(2)?, v(3)?, v(4)?);
        let g = (g1 + g2) / 2.0;
        (r > 0.0 && bl > 0.0 && g > 0.0).then(|| [g / r, 1.0, g / bl])
    });
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let rotation = get(path_of(&[0x300a]), IMAGE_SPEC).and_then(|b| u32_at(b, 12)).unwrap_or(0);
    let orientation = match rotation {
        90 => 6,
        180 => 3,
        270 => 8,
        _ => 1,
    };
    let img = RawImage {
        format: RawFormat::Crw,
        width: w,
        height: h,
        cpp: 1,
        data,
        cfa: Some(Cfa::bayer_static("RGGB")),
        bits: plan.bits,
        black,
        white: vec![white],
        active_area: active,
        crop: Rect::new(0, 0, active.width, active.height),
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
