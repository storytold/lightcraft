//! Canon CRW: the CIFF heap container of the PowerShot and EOS bodies before the CR2 era, with Canon's block
//! Huffman compression of the mosaic.
//!
//! Sources: Canon's published "CIFF specification" 1.0r4 (heap layout, record types and storage classes, prose),
//! the ExifTool CanonRaw tag tables (prose: `SensorInfo`, `DecoderTable`, `ImageInfo`, `ShotInfo`, `ColorBalance`),
//! dcraw's `parse_ciff` and `canon_load_raw` read as a reference for the coding (the three fixed Huffman table pairs
//! are format constants, listed below in BITS/HUFFVAL form; the code is our own), ITU-T T.81 Annex C/F for the
//! canonical Huffman codes and the "magnitude category + additional bits" convention, and our own black-box
//! analysis of 16 CC0 samples from raw.pixls.us. Every layout fact below was checked on them.
//!
//! - Header: byte order `II`, a u32 header length (26), `HEAPCCDR`, a version. The heap runs from the header to
//!   the end of the file. A heap ends with the u32 offset (relative to the heap) of its record table: a u16 count
//!   and 10-byte records (u16 type, u32 length, u32 offset). Records whose type has storage bits `0x4000` hold
//!   their 8 data bytes in place of length and offset. Types with class `0x2800` / `0x3000` are sub-heaps.
//! - `0x2005` is the raw data, `0x2007` the camera JPEG, `0x080a` make and model, `0x1810` the image size and
//!   rotation, `0x1031` the sensor size and the picture rectangle (left, top, right, bottom, inclusive),
//!   `0x1835` the Huffman table set (0..=2) and the length of the coded data, `0x180e` the capture time (seconds
//!   since 1970, camera local time), `0x102a` ISO / aperture / shutter (APEX-like, as ExifTool lists them).
//! - The raw record starts with the 2-bit low parts of 12-bit samples when the body stores them (four samples a
//!   byte, the first in the lowest bits; one sensor row is `width / 4` bytes). The Huffman stream starts 514 bytes
//!   after them (or after the record start). The coded length in `0x1835` doesn't tell the two apart (the G3 and
//!   Pro1 count the low parts in it, the 300D doesn't); the stream never has an `FF` not followed by `00`, the low
//!   parts soon do. Bodies without low parts give 10-bit samples.
//! - The stream (MSB-first bits, a `00` after each `FF` dropped) codes the mosaic, row-major and continuous across
//!   rows, in bands of eight sensor rows cut into blocks of 64 samples. A block holds a DC difference (first table
//!   of the set: the symbol is the bit count, then that many bits) and up to 63 AC differences (second table:
//!   high nibble = zeros to skip, low nibble = bit count; symbol 0 ends the block, `FF` skips one place). The DC
//!   of each block is added to the previous block's. Samples are rebuilt per sensor row from two running sums
//!   (even and odd positions in the block) that restart at 512 at the start of each row.
//! - The colour filter is RGGB from the sensor's first pixel on every Bayer body checked; the G1 / Pro90 IS and
//!   older bodies use a CMYG filter and are refused.
//! - Black is the mean of the masked columns left of the picture rectangle; white the saturation plateau.
//! - White balance: the as-shot RGGB levels where the file states them in a form we understand (`0x10a9`, D60 /
//!   10D / 300D; `0x102c`, G2 / S30 / S40; `0x0032` as reciprocals on the D30), else measured from the camera
//!   JPEG's neutral pixels ([`crate::wb_estimate`]).
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lcc_geom::Orientation;
use lcc_meta::{DateTime, Metadata};

/// Bit counts per code length 1..=16, then the symbols, of the DC tables (first) and AC tables (second) of the
/// three table sets.
const DC: [([u8; 16], &[u8]); 3] = [
    ([0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[4, 3, 5, 6, 2, 7, 1, 8, 9, 0, 10, 11, 0xff]),
    ([0, 2, 2, 3, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0], &[3, 2, 4, 1, 5, 0, 6, 7, 9, 8, 10, 11, 0xff]),
    ([0, 0, 6, 3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[6, 5, 7, 4, 8, 3, 9, 2, 0, 10, 1, 11, 0xff]),
];
const AC: [([u8; 16], &[u8]); 3] = [
    (
        [0, 2, 2, 2, 1, 4, 2, 1, 2, 5, 1, 1, 0, 0, 0, 139],
        &[
            0x03, 0x04, 0x02, 0x05, 0x01, 0x06, 0x07, 0x08, 0x12, 0x13, 0x11, 0x14, 0x09, 0x15, 0x22, 0x00, 0x21, 0x16, 0x0a, 0xf0, 0x23, 0x17, 0x24,
            0x31, 0x32, 0x18, 0x19, 0x33, 0x25, 0x41, 0x34, 0x42, 0x35, 0x51, 0x36, 0x37, 0x38, 0x29, 0x79, 0x26, 0x1a, 0x39, 0x56, 0x57, 0x28, 0x27,
            0x52, 0x55, 0x58, 0x43, 0x76, 0x59, 0x77, 0x54, 0x61, 0xf9, 0x71, 0x78, 0x75, 0x96, 0x97, 0x49, 0xb7, 0x53, 0xd7, 0x74, 0xb6, 0x98, 0x47,
            0x48, 0x95, 0x69, 0x99, 0x91, 0xfa, 0xb8, 0x68, 0xb5, 0xb9, 0xd6, 0xf7, 0xd8, 0x67, 0x46, 0x45, 0x94, 0x89, 0xf8, 0x81, 0xd5, 0xf6, 0xb4,
            0x88, 0xb1, 0x2a, 0x44, 0x72, 0xd9, 0x87, 0x66, 0xd4, 0xf5, 0x3a, 0xa7, 0x73, 0xa9, 0xa8, 0x86, 0x62, 0xc7, 0x65, 0xc8, 0xc9, 0xa1, 0xf4,
            0xd1, 0xe9, 0x5a, 0x92, 0x85, 0xa6, 0xe7, 0x93, 0xe8, 0xc1, 0xc6, 0x7a, 0x64, 0xe1, 0x4a, 0x6a, 0xe6, 0xb3, 0xf1, 0xd3, 0xa5, 0x8a, 0xb2,
            0x9a, 0xba, 0x84, 0xa4, 0x63, 0xe5, 0xc5, 0xf3, 0xd2, 0xc4, 0x82, 0xaa, 0xda, 0xe4, 0xf2, 0xca, 0x83, 0xa3, 0xa2, 0xc3, 0xea, 0xc2, 0xe2,
            0xe3, 0xff, 0xff,
        ],
    ),
    (
        [0, 2, 2, 1, 4, 1, 4, 1, 3, 3, 1, 0, 0, 0, 0, 140],
        &[
            0x02, 0x03, 0x01, 0x04, 0x05, 0x12, 0x11, 0x06, 0x13, 0x07, 0x08, 0x14, 0x22, 0x09, 0x21, 0x00, 0x23, 0x15, 0x31, 0x32, 0x0a, 0x16, 0xf0,
            0x24, 0x33, 0x41, 0x42, 0x19, 0x17, 0x25, 0x18, 0x51, 0x34, 0x43, 0x52, 0x29, 0x35, 0x61, 0x39, 0x71, 0x62, 0x36, 0x53, 0x26, 0x38, 0x1a,
            0x37, 0x81, 0x27, 0x91, 0x79, 0x55, 0x45, 0x28, 0x72, 0x59, 0xa1, 0xb1, 0x44, 0x69, 0x54, 0x58, 0xd1, 0xfa, 0x57, 0xe1, 0xf1, 0xb9, 0x49,
            0x47, 0x63, 0x6a, 0xf9, 0x56, 0x46, 0xa8, 0x2a, 0x4a, 0x78, 0x99, 0x3a, 0x75, 0x74, 0x86, 0x65, 0xc1, 0x76, 0xb6, 0x96, 0xd6, 0x89, 0x85,
            0xc9, 0xf5, 0x95, 0xb4, 0xc7, 0xf7, 0x8a, 0x97, 0xb8, 0x73, 0xb7, 0xd8, 0xd9, 0x87, 0xa7, 0x7a, 0x48, 0x82, 0x84, 0xea, 0xf4, 0xa6, 0xc5,
            0x5a, 0x94, 0xa4, 0xc6, 0x92, 0xc3, 0x68, 0xb5, 0xc8, 0xe4, 0xe5, 0xe6, 0xe9, 0xa2, 0xa3, 0xe3, 0xc2, 0x66, 0x67, 0x93, 0xaa, 0xd4, 0xd5,
            0xe7, 0xf8, 0x88, 0x9a, 0xd7, 0x77, 0xc4, 0x64, 0xe2, 0x98, 0xa5, 0xca, 0xda, 0xe8, 0xf3, 0xf6, 0xa9, 0xb2, 0xb3, 0xf2, 0xd2, 0x83, 0xba,
            0xd3, 0xff, 0xff,
        ],
    ),
    (
        [0, 0, 6, 2, 1, 3, 3, 2, 5, 1, 2, 2, 8, 10, 0, 117],
        &[
            0x04, 0x05, 0x03, 0x06, 0x02, 0x07, 0x01, 0x08, 0x09, 0x12, 0x13, 0x14, 0x11, 0x15, 0x0a, 0x16, 0x17, 0xf0, 0x00, 0x22, 0x21, 0x18, 0x23,
            0x19, 0x24, 0x32, 0x31, 0x25, 0x33, 0x38, 0x37, 0x34, 0x35, 0x36, 0x39, 0x79, 0x57, 0x58, 0x59, 0x28, 0x56, 0x78, 0x27, 0x41, 0x29, 0x77,
            0x26, 0x42, 0x76, 0x99, 0x1a, 0x55, 0x98, 0x97, 0xf9, 0x48, 0x54, 0x96, 0x89, 0x47, 0xb7, 0x49, 0xfa, 0x75, 0x68, 0xb6, 0x67, 0x69, 0xb9,
            0xb8, 0xd8, 0x52, 0xd7, 0x88, 0xb5, 0x74, 0x51, 0x46, 0xd9, 0xf8, 0x3a, 0xd6, 0x87, 0x45, 0x7a, 0x95, 0xd5, 0xf6, 0x86, 0xb4, 0xa9, 0x94,
            0x53, 0x2a, 0xa8, 0x43, 0xf5, 0xf7, 0xd4, 0x66, 0xa7, 0x5a, 0x44, 0x8a, 0xc9, 0xe8, 0xc8, 0xe7, 0x9a, 0x6a, 0x73, 0x4a, 0x61, 0xc7, 0xf4,
            0xc6, 0x65, 0xe9, 0x72, 0xe6, 0x71, 0x91, 0x93, 0xa6, 0xda, 0x92, 0x85, 0x62, 0xf3, 0xc5, 0xb2, 0xa4, 0x84, 0xba, 0x64, 0xa5, 0xb3, 0xd2,
            0x81, 0xe5, 0xd3, 0xaa, 0xc4, 0xca, 0xf2, 0xb1, 0xe4, 0xd1, 0x83, 0x63, 0xea, 0xc3, 0xe2, 0x82, 0xf1, 0xa3, 0xc2, 0xa1, 0xc1, 0xe3, 0xa2,
            0xe1, 0xff, 0xff,
        ],
    ),
];

/// Nesting limit for sub-heaps, and the record count a table may claim.
const MAX_DEPTH: usize = 8;
const MAX_RECORDS: usize = 1024;
/// Models with a CMYG colour filter (four colours), which the pipeline doesn't develop.
const CMYG: &[&str] = &["PowerShot G1", "PowerShot Pro90 IS", "PowerShot Pro70", "PowerShot A5", "PowerShot A50", "PowerShot 600"];

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at.checked_add(1)?)?]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// What the decoder needs from the heap.
#[derive(Debug, Default)]
pub(crate) struct Ciff {
    pub make: Option<String>,
    pub model: Option<String>,
    /// `0x1810`: rotation in degrees.
    pub rotation: Option<i32>,
    /// `0x1031`: sensor width, height, and the picture's left, top, right, bottom (inclusive).
    pub sensor: Option<[usize; 6]>,
    /// `0x1835`: Huffman table set and coded length.
    pub table: Option<(u32, usize)>,
    /// `0x2005`: offset and length of the raw record.
    pub raw: Option<(usize, usize)>,
    pub time: Option<u32>,
    /// `0x102a` ShotInfo as u16 words.
    pub shot: Vec<u16>,
    pub focal_mm: Option<f64>,
    /// As-shot levels R, G, G, B (or their reciprocals for the D30) where the file states them readably.
    pub levels: Option<[f32; 4]>,
}

fn ascii(b: &[u8]) -> Option<String> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let s = String::from_utf8_lossy(b.get(..end)?).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Walk one heap (`start..end` of the file) and its sub-heaps.
fn walk(b: &[u8], start: usize, end: usize, depth: usize, out: &mut Ciff, wbi: &mut usize) -> Result<()> {
    let corrupt = |m: &str| RawError::Corrupt(format!("CRW: {m}"));
    if depth > MAX_DEPTH || end < start.saturating_add(4) || end > b.len() {
        return Err(corrupt("bad heap"));
    }
    let table = start.checked_add(le32(b, end - 4).ok_or_else(|| corrupt("truncated heap"))? as usize).ok_or_else(|| corrupt("bad table offset"))?;
    let count = usize::from(le16(b, table).ok_or_else(|| corrupt("record table past the heap"))?);
    if count > MAX_RECORDS {
        return Err(corrupt("too many records"));
    }
    for i in 0..count {
        let rec = table + 2 + i * 10;
        let (Some(kind), Some(len), Some(off)) = (le16(b, rec), le32(b, rec + 2), le32(b, rec + 6)) else {
            return Err(corrupt("truncated record table"));
        };
        let (len, off) = (len as usize, off as usize);
        let inline = kind & 0xc000 == 0x4000;
        let kind = kind & 0x3fff;
        let (pos, size) = if inline { (rec + 2, 8) } else { (start.saturating_add(off), len) };
        let Some(data) = pos.checked_add(size).filter(|&e| e <= end || inline).and_then(|e| b.get(pos..e)) else {
            continue;
        };
        let words = |at: usize, n: usize| -> Vec<u16> { (0..n).map_while(|k| le16(data, at + 2 * k)).collect() };
        match kind {
            0x2800..=0x37ff if !inline => walk(b, pos, pos + size, depth + 1, out, wbi)?,
            0x080a => {
                let make_end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
                out.make = ascii(data);
                out.model = data.get(make_end + 1..).and_then(ascii);
            }
            0x1810 => out.rotation = le32(data, 12).map(|r| r as i32),
            0x1031 => {
                if let [_, w, h, _, _, l, t, r, btm, ..] = words(0, 9)[..] {
                    out.sensor = Some([w, h, l, t, r, btm].map(usize::from));
                }
            }
            0x1835 => out.table = le32(data, 0).zip(le32(data, 12).map(|v| v as usize)),
            0x2005 => out.raw = Some((pos, size)),
            0x180e => out.time = le32(data, 0),
            0x102a => {
                out.shot = words(0, size / 2);
                *wbi = out.shot.get(7).map_or(0, |&v| usize::from(v));
            }
            0x1029 if inline => {
                // FocalLength: [type, focal]; type 2 counts 1/32 mm, others whole millimetres
                let scale = if le16(data, 0) == Some(2) { 32.0 } else { 1.0 };
                out.focal_mm = le16(data, 2).filter(|&f| f > 0).map(|f| f64::from(f) / scale);
            }
            0x10a9 => {
                // D60, 10D, 300D: RGGB level sets, the as-shot one picked by ShotInfo's white-balance index
                let idx = if size > 66 { [0, 1, 3, 4, 5, 6, 7, 0, 2, 8].get(*wbi).copied().unwrap_or(0) } else { *wbi };
                if let [r, g1, g2, bl] = words(2 + idx * 8, 4)[..] {
                    out.levels = Some([r, g1, g2, bl].map(f32::from));
                }
            }
            0x102c => {
                // G2, S30, S40: G R B G levels 100 bytes in (a first word above 512 marks the CMYG G1 / Pro90 IS)
                if le16(data, 0).is_some_and(|v| v <= 512)
                    && let [g1, r, bl, g2] = words(100, 4)[..]
                {
                    out.levels = Some([r, g1, g2, bl].map(f32::from));
                }
            }
            0x0032 if size == 768 => {
                // EOS D30: reciprocal RGGB levels 72 bytes in, replacing what its `0x102c` (another layout) gave;
                // white balance 0 (auto) has none, so the preview decides
                out.levels = match words(72, 4)[..] {
                    [r, g1, g2, bl] if *wbi != 0 && [r, g1, g2, bl].iter().all(|&v| v > 0) => Some([r, g1, g2, bl].map(|v| 1024.0 / f32::from(v))),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parse the CIFF heap.
pub(crate) fn parse(b: &[u8]) -> Result<Ciff> {
    if !(b.get(0..2) == Some(b"II") && b.get(6..14) == Some(b"HEAPCCDR")) {
        return Err(RawError::NotRaw);
    }
    let hlen = le32(b, 2).ok_or(RawError::NotRaw)? as usize;
    let mut out = Ciff::default();
    let mut wbi = 0;
    walk(b, hlen, b.len(), 0, &mut out, &mut wbi)?;
    Ok(out)
}

/// A canonical Huffman table as a lookup on the next `max` bits: (code length, symbol).
struct Table {
    max: u32,
    lut: Vec<(u8, u8)>,
}

impl Table {
    /// Codes are handed out in order of length; those that don't fit in `max` bits are dropped (the AC tables
    /// claim more 16-bit codes than exist).
    fn new(counts: &[u8; 16], symbols: &[u8]) -> Table {
        let max = counts.iter().rposition(|&c| c > 0).map_or(1, |i| i as u32 + 1);
        let size = 1usize << max;
        let mut lut = Vec::with_capacity(size);
        let mut sym = symbols.iter();
        for (i, &n) in counts.iter().enumerate() {
            let len = i as u32 + 1;
            for _ in 0..n {
                let Some(&s) = sym.next() else { break };
                for _ in 0..1usize << (max - len) {
                    if lut.len() < size {
                        lut.push((len as u8, s));
                    }
                }
            }
        }
        // codes the table leaves unassigned decode as an invalid length
        lut.resize(size, (0, 0));
        Table { max, lut }
    }
}

/// MSB-first bits; a byte after `FF` is dropped when it is `00`, and ends the data (a marker) otherwise.
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
    end: bool,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Bits { src, pos: 0, buf: 0, n: 0, end: false }
    }
    fn fill(&mut self) {
        while self.n <= 56 {
            let mut byte = 0;
            if !self.end {
                match self.src.get(self.pos) {
                    Some(&0xff) => match self.src.get(self.pos + 1) {
                        Some(0) => {
                            byte = 0xff;
                            self.pos += 2;
                        }
                        _ => self.end = true,
                    },
                    Some(&c) => {
                        byte = c;
                        self.pos += 1;
                    }
                    None => self.end = true,
                }
            }
            self.buf |= u64::from(byte) << (56 - self.n);
            self.n += 8;
        }
    }
    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.buf >> (64 - k)) as u32
    }
    fn skip(&mut self, k: u32) {
        self.buf <<= k;
        self.n -= k;
    }
    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.skip(k);
        v
    }
    fn huff(&mut self, t: &Table) -> Result<u8> {
        let (len, sym) = t.lut.get(self.peek(t.max) as usize).copied().unwrap_or((0, 0));
        if len == 0 {
            return Err(RawError::Corrupt("CRW: invalid Huffman code".into()));
        }
        self.skip(u32::from(len));
        Ok(sym)
    }
}

/// Decode the 10-bit Huffman-coded mosaic (`w` x `h`) from `src`.
fn decode_stream(src: &[u8], w: usize, h: usize, set: usize, out: &mut [u16]) -> Result<()> {
    let (dc, ac) = (
        DC.get(set).ok_or(RawError::Unsupported(format!("CRW Huffman table set {set}")))?,
        AC.get(set).ok_or(RawError::Corrupt("CRW table".into()))?,
    );
    let (dc, ac) = (Table::new(&dc.0, dc.1), Table::new(&ac.0, ac.1));
    let mut bits = Bits::new(src);
    let mut carry = 0i32;
    let mut base = [512i32; 2];
    let mut p = 0usize;
    for band in (0..h).step_by(8) {
        let blocks = (8.min(h - band) * w) / 64;
        for _ in 0..blocks {
            let mut diff = [0i32; 64];
            let mut i = 0usize;
            while i < 64 {
                let leaf = bits.huff(if i == 0 { &dc } else { &ac })?;
                if leaf == 0 && i > 0 {
                    break;
                }
                if leaf != 0xff {
                    i += usize::from(leaf >> 4);
                    let len = u32::from(leaf & 15);
                    if len > 0 {
                        let mut d = bits.bits(len) as i32;
                        if d & (1 << (len - 1)) == 0 {
                            d -= (1 << len) - 1;
                        }
                        if let Some(s) = diff.get_mut(i) {
                            *s = d;
                        }
                    }
                }
                i += 1;
            }
            diff[0] += carry;
            carry = diff[0];
            for (k, d) in diff.iter().enumerate() {
                if p.is_multiple_of(w) {
                    base = [512, 512];
                }
                base[k & 1] += d;
                let v = base[k & 1];
                if !(0..1024).contains(&v) {
                    return Err(RawError::Corrupt("CRW: sample out of range".into()));
                }
                *out.get_mut(p).ok_or(RawError::Corrupt("CRW: too many samples".into()))? = v as u16;
                p += 1;
            }
        }
    }
    Ok(())
}

/// Whether the raw record starts with the 2-bit low parts (`low_len` bytes) rather than the Huffman stream at
/// byte 514: in the stream every `FF` is followed by `00`, so an `FF` followed by anything else within the first
/// 16 KiB after byte 514 means low bits (so does a run without any `FF`, too short to tell for a stream).
fn has_low_bits(record: &[u8], low_len: usize) -> bool {
    if record.len() < low_len.saturating_add(514) {
        return false;
    }
    let probe = record.get(514..record.len().min(514 + 0x4000)).unwrap_or_default();
    let mut ffs = probe.windows(2).filter(|p| p[0] == 0xff).peekable();
    ffs.peek().is_none() || ffs.any(|p| p[1] != 0)
}

/// Black from the masked columns at the left edge. `SensorInfo`'s picture rectangle starts well inside the
/// sensor and on the PowerShots the columns between the masked strip and it hold picture, so the strip is the run
/// of columns from column 1 whose mean stays within 10% of the first ones'. 0 when there is none.
fn masked_black(d: &[u16], w: usize, left: usize, rows: std::ops::Range<usize>) -> f32 {
    let col = |x: usize| {
        let (mut s, mut n) = (0f64, 0f64);
        for y in rows.clone() {
            if let Some(&v) = d.get(y * w + x) {
                s += f64::from(v);
                n += 1.0;
            }
        }
        if n > 0.0 { s / n } else { 0.0 }
    };
    if left < 4 {
        return 0.0;
    }
    let base = (1..4).map(col).sum::<f64>() / 3.0;
    let mut sum = 0.0;
    let mut n = 0.0;
    for x in 1..left {
        let m = col(x);
        if m > base * 1.1 + 4.0 {
            break;
        }
        sum += m;
        n += 1.0;
    }
    if n > 0.0 { (sum / n) as f32 } else { 0.0 }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's public-domain `civil_from_days`).
fn civil(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn metadata(c: &Ciff) -> Metadata {
    let mut m = Metadata { make: c.make.clone(), model: c.model.clone(), focal_length: c.focal_mm, ..Default::default() };
    if let Some(t) = c.time.filter(|&t| t > 0) {
        let t = i64::from(t);
        let (y, mo, d) = civil(t.div_euclid(86_400));
        let s = t.rem_euclid(86_400);
        m.capture_time = u16::try_from(y).ok().map(|year| DateTime {
            year,
            month: mo,
            day: d,
            hour: (s / 3600) as u8,
            minute: (s / 60 % 60) as u8,
            second: (s % 60) as u8,
            millis: 0,
            offset_minutes: None,
        });
    }
    let w = |i: usize| c.shot.get(i).copied();
    if let Some(iso) = w(2).filter(|&v| v > 0) {
        let iso = (2f64.powf(f64::from(iso) / 32.0 - 4.0) * 50.0).round();
        m.iso = (iso.is_finite() && iso > 0.0 && iso < 1e6).then_some(iso as u32);
    }
    if let Some(av) = w(4).map(|v| v as i16).filter(|&v| v > 0) {
        m.f_number = Some((2f64.powf(f64::from(av) / 64.0) * 10.0).round() / 10.0);
    }
    if let Some(tv) = w(5).map(|v| v as i16).filter(|&v| v != 0) {
        let t = 2f64.powf(-f64::from(tv) / 32.0);
        m.exposure_time = (t.is_finite() && t > 0.0 && t < 1e5).then_some(t);
    }
    m
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let c = parse(bytes)?;
    let model = c.model.clone().unwrap_or_default();
    if CMYG.iter().any(|m| model.ends_with(m)) {
        return Err(RawError::Unsupported(format!("CRW from the {model}: CMYG colour filter")));
    }
    let [w, h, left, top, right, bottom] = c.sensor.ok_or_else(|| RawError::Unsupported("CRW without SensorInfo".into()))?;
    let (set, _coded) = c.table.ok_or_else(|| RawError::Unsupported("CRW without a decoder table".into()))?;
    let (raw_at, raw_len) = c.raw.ok_or_else(|| RawError::Corrupt("CRW: no raw data record".into()))?;
    if w == 0 || h == 0 || !w.is_multiple_of(4) || right >= w || bottom >= h || left > right || top > bottom {
        return Err(RawError::Corrupt("CRW: bad sensor geometry".into()));
    }
    let n = w.checked_mul(h).filter(|n| *n <= crate::MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let low_len = n / 4;
    let record = bytes.get(raw_at..raw_at.saturating_add(raw_len)).ok_or_else(|| RawError::Corrupt("CRW: raw record truncated".into()))?;
    let lowbits = has_low_bits(record, low_len);
    let start = raw_at.checked_add(514).and_then(|s| s.checked_add(if lowbits { low_len } else { 0 }));
    let src =
        start.and_then(|s| bytes.get(s..raw_at.saturating_add(raw_len))).ok_or_else(|| RawError::Corrupt("CRW: raw record truncated".into()))?;
    let bits = if lowbits { 12 } else { 10 };
    let stored = c.levels.filter(|l| l.iter().all(|&v| v > 0.0)).map(|[r, g1, g2, b]| {
        let g = (g1 + g2) / 2.0;
        [r / g, 1.0, b / g]
    });
    // a header probe needs the samples only to measure the white balance
    let mut d = if mode == Mode::Full || stored.is_none() { vec![0u16; n] } else { Vec::new() };
    if !d.is_empty() {
        decode_stream(src, w, h, set as usize, &mut d)?;
    }
    if lowbits && !d.is_empty() {
        let low = bytes.get(raw_at..raw_at + low_len).ok_or_else(|| RawError::Corrupt("CRW: low bits truncated".into()))?;
        for (px, &byte) in d.chunks_mut(4).zip(low) {
            for (r, v) in px.iter_mut().enumerate() {
                *v = (*v << 2) | u16::from((byte >> (2 * r)) & 3);
            }
        }
    }
    let active = Rect::new(left, top, right - left + 1, bottom - top + 1);
    let cfa = Cfa::bayer_static("RGGB");
    let black = BlackLevel::uniform(masked_black(&d, w, left, top..bottom + 1));
    let white = super::white_from_data(&d, bits);
    let crop = Rect::new(0, 0, active.width, active.height);
    let wb = stored.or_else(|| {
        let frame = crate::wb_estimate::Frame { data: &d, width: w, crop: active, cfa: &cfa, black: &black, white };
        crate::wb_estimate::as_shot(bytes, &frame)
    });
    let orientation = match c.rotation.unwrap_or(0).rem_euclid(360) {
        90 => 6,
        180 => 3,
        270 => 8,
        _ => 1,
    };
    let mut metadata = metadata(&c);
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let img = RawImage {
        format: RawFormat::Crw,
        width: w,
        height: h,
        cpp: 1,
        data: if mode == Mode::Full { RawData::U16(d) } else { RawData::U16(Vec::new()) },
        cfa: Some(cfa),
        bits,
        black,
        white: vec![white],
        active_area: active,
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

    /// Canonical codes (code, length) per symbol of a BITS/HUFFVAL table, as `Table::new` assigns them.
    fn codes(counts: &[u8; 16], symbols: &[u8]) -> Vec<(u8, u32, u32)> {
        let (mut out, mut code, mut sym) = (Vec::new(), 0u32, symbols.iter());
        for (i, &n) in counts.iter().enumerate() {
            for _ in 0..n {
                if let Some(&s) = sym.next()
                    && !out.iter().any(|&(o, _, _)| o == s)
                {
                    out.push((s, code, i as u32 + 1));
                }
                code += 1;
            }
            code <<= 1;
        }
        out
    }

    /// MSB-first bit writer with `FF 00` stuffing.
    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Writer {
        fn put(&mut self, v: u32, len: u32) {
            for k in (0..len).rev() {
                self.acc = (self.acc << 1) | u64::from((v >> k) & 1);
                self.n += 1;
                if self.n == 8 {
                    let b = self.acc as u8;
                    self.bytes.push(b);
                    if b == 0xff {
                        self.bytes.push(0);
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn sym(&mut self, table: &[(u8, u32, u32)], s: u8) {
            let &(_, code, len) = table.iter().find(|&&(o, _, _)| o == s).expect("symbol in table");
            self.put(code, len);
        }
        fn value(&mut self, d: i32) -> u32 {
            let len = 32 - d.unsigned_abs().leading_zeros();
            if len > 0 {
                self.put(if d > 0 { d as u32 } else { (d + (1 << len) - 1) as u32 }, len);
            }
            len
        }
    }

    /// Encode blocks of 64 differences with table set `set`: DC, then the non-zero AC values (runs of zeros
    /// before each up to 15), then end of block.
    fn encode(set: usize, blocks: &[[i32; 64]]) -> Vec<u8> {
        let (dc, ac) = (codes(&DC[set].0, DC[set].1), codes(&AC[set].0, AC[set].1));
        let mut w = Writer::default();
        for b in blocks {
            let len = 32 - b[0].unsigned_abs().leading_zeros();
            w.sym(&dc, len as u8);
            w.value(b[0]);
            let mut run = 0u8;
            for &d in &b[1..] {
                if d == 0 {
                    run += 1;
                    continue;
                }
                while run > 15 {
                    w.sym(&ac, 0xf0);
                    run -= 16;
                }
                let len = 32 - d.unsigned_abs().leading_zeros();
                w.sym(&ac, (run << 4) | len as u8);
                w.value(d);
                run = 0;
            }
            if run > 0 {
                w.sym(&ac, 0);
            }
        }
        w.put(0xff, 8 - w.n % 8);
        w.bytes
    }

    /// The samples the blocks describe: DC chained over blocks, two running sums restarting at 512 per row.
    fn expected(w: usize, blocks: &[[i32; 64]]) -> Vec<u16> {
        let (mut out, mut carry, mut base) = (Vec::new(), 0, [512i32; 2]);
        for b in blocks {
            carry += b[0];
            for (k, &d) in b.iter().enumerate() {
                if out.len() % w == 0 {
                    base = [512, 512];
                }
                base[k & 1] += if k == 0 { carry } else { d };
                out.push(base[k & 1] as u16);
            }
        }
        out
    }

    fn record(kind: u16, data: &[u8], heap: &mut Vec<u8>, table: &mut Vec<u8>) {
        table.extend_from_slice(&kind.to_le_bytes());
        table.extend_from_slice(&(data.len() as u32).to_le_bytes());
        table.extend_from_slice(&(heap.len() as u32).to_le_bytes());
        heap.extend_from_slice(data);
    }

    fn heap(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let (mut h, mut t) = (Vec::new(), (records.len() as u16).to_le_bytes().to_vec());
        for (k, d) in records {
            record(*k, d, &mut h, &mut t);
        }
        let at = h.len() as u32;
        h.extend_from_slice(&t);
        h.extend_from_slice(&at.to_le_bytes());
        h
    }

    fn u16s(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// A CRW of `w` x `h` samples: the raw record (optional low bits, 514 bytes, the stream), make and model in a
    /// sub-heap, SensorInfo with a 4-column masked strip, the decoder table and D60-style RGGB levels.
    fn synth(w: u16, h: u16, set: u32, low: Option<&[u8]>, stream: &[u8]) -> Vec<u8> {
        let mut raw = low.map(<[u8]>::to_vec).unwrap_or_default();
        raw.extend(std::iter::repeat_n(0u8, 514));
        raw.extend_from_slice(stream);
        let camera = heap(&[(0x080a, b"Canon\0Canon EOS D60\0".to_vec())]);
        let sensor = u16s(&[34, w, h, 1, 1, 4, 0, w - 1, h - 1]);
        let mut table = set.to_le_bytes().to_vec();
        table.extend_from_slice(&[0; 8]);
        table.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        let mut levels = vec![0u8; 2];
        levels.extend(u16s(&[2000, 1000, 1000, 1500]));
        let root = heap(&[(0x2005, raw), (0x300a, camera), (0x1031, sensor), (0x1835, table), (0x10a9, levels)]);
        let mut f = b"II\x1a\0\0\0HEAPCCDR\x02\0\x01\0\0\0\0\0\0\0\0\0".to_vec();
        f.extend_from_slice(&root);
        f
    }

    fn blocks(n: usize) -> Vec<[i32; 64]> {
        (0..n)
            .map(|b| {
                let mut d = [0i32; 64];
                d[0] = [5, -3, 0, -2][b % 4];
                d[1] = 2;
                d[2] = -1;
                d[9] = 7;
                d[40] = -20;
                d
            })
            .collect()
    }

    #[test]
    fn decodes_every_table_set_with_and_without_low_bits() {
        let (w, h) = (128usize, 36usize);
        let b = blocks(w * h / 64);
        for set in 0..3 {
            let stream = encode(set, &b);
            let want = expected(w, &b);
            let f = synth(w as u16, h as u16, set as u32, None, &stream);
            assert_eq!(crate::probe(&f), Some(RawFormat::Crw));
            let img = crate::decode(&f).unwrap();
            assert_eq!((img.width, img.height, img.bits), (128, 36, 10));
            assert_eq!(img.data, RawData::U16(want.clone()), "set {set}");
            assert_eq!(img.active_area, Rect::new(4, 0, 124, 36));
            assert_eq!(img.wb_multipliers, Some([2.0, 1.0, 1.5]));
            assert_eq!(img.metadata.model.as_deref(), Some("Canon EOS D60"));
            assert_eq!(crate::probe_info(&f).unwrap(), img.info());
            // the low bits: four samples a byte, the first in the lowest bits (an FF 01 after byte 514 marks them)
            let mut low: Vec<u8> = (0..w * h / 4).map(|i| (i * 37) as u8).collect();
            low[600] = 0xff;
            low[601] = 1;
            let f = synth(w as u16, h as u16, set as u32, Some(&low), &stream);
            let img = crate::decode(&f).unwrap();
            assert_eq!(img.bits, 12);
            let want12: Vec<u16> = want.iter().enumerate().map(|(i, &v)| (v << 2) | u16::from((low[i / 4] >> (2 * (i % 4))) & 3)).collect();
            assert_eq!(img.data, RawData::U16(want12), "set {set} with low bits");
        }
    }

    #[test]
    fn cmyg_and_incomplete_files_are_refused() {
        let b = blocks(2);
        let stream = encode(0, &b);
        let f = synth(16, 8, 0, None, &stream);
        let mut g1 = f.clone();
        let at = f.windows(13).position(|w| w == b"Canon EOS D60").unwrap();
        g1[at..at + 13].copy_from_slice(b"PowerShot G1\0");
        assert!(matches!(crate::decode(&g1), Err(RawError::Unsupported(w)) if w.contains("CMYG")));
        // a table set past the three
        let f = synth(16, 8, 7, None, &stream);
        assert!(matches!(crate::decode(&f), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn hostile_files_are_errors() {
        let b = blocks(2);
        let stream = encode(1, &b);
        let good = synth(16, 8, 1, None, &stream);
        assert!(crate::decode(&good).is_ok());
        // every truncation, every byte set to 00 / FF / 80
        for cut in 0..good.len() {
            let _ = crate::decode(&good[..cut]);
            let _ = crate::probe_info(&good[..cut]);
        }
        for i in 0..good.len() {
            for v in [0u8, 0xff, 0x80] {
                let mut f = good.clone();
                f[i] = v;
                let _ = crate::decode(&f);
            }
        }
        // a huge sensor, a sub-heap that contains itself, a record count past the cap
        assert!(crate::decode(&synth(65532, 65532, 1, None, &stream)).is_err());
        let mut f = b"II\x1a\0\0\0HEAPCCDR\x02\0\x01\0\0\0\0\0\0\0\0\0".to_vec();
        let mut table = 1u16.to_le_bytes().to_vec();
        table.extend_from_slice(&0x300au16.to_le_bytes());
        table.extend_from_slice(&16u32.to_le_bytes());
        table.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&table);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&0u32.to_le_bytes());
        assert!(crate::decode(&f).is_err());
        let mut f = b"II\x1a\0\0\0HEAPCCDR\x02\0\x01\0\0\0\0\0\0\0\0\0".to_vec();
        f.extend_from_slice(&u16::MAX.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        assert!(crate::decode(&f).is_err());
        // a stream of nothing but ones, and one that ends early
        let ones = synth(16, 8, 1, None, &[0xff, 0x00].repeat(64));
        assert!(crate::decode(&ones).is_err());
        let short = synth(16, 8, 1, None, &stream[..stream.len() / 2]);
        let _ = crate::decode(&short);
    }
}
