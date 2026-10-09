//! Pentax PEF — uncompressed and Pentax Huffman-compressed (compression 65535).
//!
//! Sources: TIFF 6.0 (container), ITU-T T.81 (JPEG) Annex H / F.1.2.1 for the lossless "difference category +
//! additional bits" coding, the ExifTool Pentax tag-name documentation (maker-note `0x0200` BlackPoint, `0x0201`
//! WhitePoint i.e. WB levels, `0x0220` HuffmanTable) and our own black-box analysis of CC0 samples from
//! raw.pixls.us (K10D, K-5 II s, K-3):
//!
//! - The Huffman table is stored in the file (maker note `0x0220`, in the maker note's byte order): a `u16` `d`,
//!   12 further bytes, then `n = d + 12` `u16` codes left-aligned in 12 bits, then `n` code lengths (bytes).
//!   Symbol `i` is the difference category `i` (T.81 Table H.2: the number of additional bits).
//! - The bit stream (MSB-first, no byte stuffing, continuous across rows) codes one difference per pixel; the
//!   additional bits follow T.81 F.1.2.1 (a leading 0 means a negative value).
//! - Prediction: each pixel is predicted from the previous same-colour pixel in its row (two to the left); the
//!   first two pixels of a row are predicted from the first two pixels of the previous row of the same parity
//!   (starting at 0). Found by trying candidate predictors and checking the decoded rows are continuous.
//! - Maker note `0x0038`/`0x0039`: the image area's left/top and width/height (verified against the data).

use super::{black_from_columns, cfa_from_exif, white_from_data};

use crate::tiffraw::{Packing, read_image_in};
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use dac_geom::Orientation;
use dac_tiff::image::{ImageInfo, chunk_bytes};
use dac_tiff::{ByteOrder, Tiff, makernote, tags as t};

const CROP_ORIGIN: u16 = 0x0038;
const CROP_SIZE: u16 = 0x0039;
const BLACK_POINT: u16 = 0x0200;
const WB_LEVELS: u16 = 0x0201;
const HUFFMAN: u16 = 0x0220;

/// A canonical-by-table Huffman decoder with a 12-bit lookup.
pub(crate) struct Huffman {
    /// Indexed by the next 12 bits: (symbol, length); length 0 = invalid.
    lut: Vec<(u8, u8)>,
}

impl Huffman {
    /// From `(left-aligned 12-bit code, length)` per symbol.
    pub fn new(codes: &[(u16, u8)]) -> Result<Huffman> {
        let mut lut = vec![(0u8, 0u8); 4096];
        for (sym, &(code, len)) in codes.iter().enumerate() {
            if len == 0 {
                continue;
            }
            if len > 12 || sym > 16 {
                return Err(RawError::Unsupported(format!("PEF Huffman code of length {len}")));
            }
            let first = (code >> (12 - len) << (12 - len)) as usize;
            for e in &mut lut[first..first + (1 << (12 - len))] {
                if e.1 == 0 {
                    *e = (sym as u8, len);
                }
            }
        }
        Ok(Huffman { lut })
    }
}

/// MSB-first bit reader that yields zeros past the end.
pub(crate) struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Bits { src, pos: 0, acc: 0, n: 0 }
    }
    #[inline]
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = self.src.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc |= (b as u64) << (56 - self.n);
            self.n += 8;
        }
    }
    #[inline]
    pub fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.acc >> (64 - k)) as u32
    }
    #[inline]
    pub fn skip(&mut self, k: u32) {
        self.acc <<= k;
        self.n -= k;
    }
    #[inline]
    pub fn get(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.skip(k);
        v
    }
    /// Bits handed out so far (may exceed the source length: zeros are read past its end).
    pub fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.n as usize
    }
    pub fn overrun(&self) -> bool {
        self.pos > self.src.len() + 8
    }
}

/// Decode one difference: Huffman category then T.81 additional bits.
#[inline]
pub(crate) fn diff(bits: &mut Bits, h: &Huffman) -> Option<i32> {
    let (sym, len) = h.lut[bits.peek(12) as usize];
    if len == 0 {
        return None;
    }
    bits.skip(len as u32);
    let s = sym as u32;
    if s == 0 {
        return Some(0);
    }
    if s > 16 {
        return None;
    }
    let v = bits.get(s) as i32;
    Some(if v < 1 << (s - 1) { v - (1 << s) + 1 } else { v })
}

/// Parse the maker-note Huffman table.
pub(crate) fn table(b: &[u8], order: ByteOrder) -> Result<Huffman> {
    let u16_at = |i: usize| b.get(i..i + 2).map(|s| order.u16([s[0], s[1]]));
    let d = u16_at(0).ok_or_else(|| RawError::Corrupt("PEF Huffman table too short".into()))? as usize;
    let n = d + 12;
    if n > 17 || b.len() < 14 + 3 * n {
        return Err(RawError::Corrupt(format!("PEF Huffman table: {n} symbols in {} bytes", b.len())));
    }
    let codes: Vec<(u16, u8)> = (0..n).map(|i| (u16_at(14 + 2 * i).unwrap_or(0) & 0x0fff, b[14 + 2 * n + i])).collect();
    Huffman::new(&codes)
}

/// Decode a `w × h` Huffman-coded image.
pub(crate) fn decode_huffman(src: &[u8], h: &Huffman, w: usize, hgt: usize, bits_per: u32) -> Result<Vec<u16>> {
    let mut out = vec![0u16; w * hgt];
    let mut bits = Bits::new(src);
    let mut vpred = [[0i32; 2]; 2];
    let max = (1i32 << bits_per.min(16)) - 1;
    for y in 0..hgt {
        let mut hpred = [0i32; 2];
        let row = &mut out[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let d = diff(&mut bits, h).ok_or_else(|| RawError::Corrupt(format!("PEF: invalid Huffman code at row {y}")))?;
            let p = if x < 2 {
                vpred[y & 1][x] += d;
                hpred[x] = vpred[y & 1][x];
                hpred[x]
            } else {
                hpred[x & 1] += d;
                hpred[x & 1]
            };
            *o = p.clamp(0, max) as u16;
        }
        if bits.overrun() {
            return Err(RawError::Corrupt(format!("PEF: data ends at row {y} of {hgt}")));
        }
    }
    Ok(out)
}

/// The image area without all-dark leading/trailing columns (masked or empty), kept at even offsets.
fn dark_trimmed(d: &[u16], w: usize, h: usize) -> Rect {
    if w < 256 || h == 0 {
        return Rect::new(0, 0, w, h);
    }
    let step = (h / 256).max(1);
    let col_mean = |x: usize| (0..h).step_by(step).map(|y| d[y * w + x] as f64).sum::<f64>() / h.div_ceil(step) as f64;
    let interior = (w / 4..w * 3 / 4).step_by(w / 64).map(col_mean).sum::<f64>() / 32.0;
    let dark = interior * 0.05;
    let mut left = 0;
    while left < 64 && col_mean(left) < dark {
        left += 1;
    }
    let mut right = w;
    while right > w - 64 && col_mean(right - 1) < dark {
        right -= 1;
    }
    let left = left.next_multiple_of(2);
    Rect::new(left, 0, (right - left) & !1, h)
}

/// Whether every strip or tile of a one-sample-per-pixel image holds exactly the bytes that its rows need when
/// the samples are packed MSB-first at the declared bit depth, rows starting on byte boundaries (TIFF 6.0
/// section 7). A real PackBits stream of such an image has a different length (almost always shorter), so the
/// exact match is what tells plain samples under a compression tag that says otherwise.
fn is_exactly_packed(info: &ImageInfo, file_len: u64) -> bool {
    let bits = u64::from(info.bits());
    if info.samples_per_pixel != 1 || info.planar != 1 || !(1..=16).contains(&bits) || info.byte_counts.len() != info.offsets.len() {
        return false;
    }
    let chunks = info.chunks(file_len);
    !chunks.is_empty() && chunks.iter().all(|c| c.len == (u64::from(c.width) * bits).div_ceil(8) * u64::from(c.height))
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = &tiff.ifds[0];
    let info = ifd0.image()?;
    let (w, hgt) = (info.width as usize, info.height as usize);
    let bits = info.bits() as u32;
    if w == 0 || hgt == 0 || w.saturating_mul(hgt) > crate::MAX_SAMPLES {
        return Err(RawError::Limit("image too large"));
    }
    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let mn =
        tiff.exif().and_then(|e| e.get(t::MAKER_NOTE)).and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make));
    let pair = |tag: u16| mn.as_ref().and_then(|m| m.ifd.u64s(tag)).filter(|v| v.len() == 2).map(|v| (v[0] as usize, v[1] as usize));
    let tagged = match (pair(CROP_ORIGIN), pair(CROP_SIZE)) {
        (Some((x, y)), Some((cw, ch))) if cw > 0 && ch > 0 && x + cw <= w && y + ch <= hgt => Some(Rect::new(x, y, cw, ch)),
        _ => None,
    };
    // without crop tags the image area is found from the samples (dark borders)
    let read = if tagged.is_some() { mode } else { Mode::Full };
    let data = match info.compression {
        65535 => {
            let mn = mn.as_ref().ok_or_else(|| RawError::Corrupt("compressed PEF without maker note".into()))?;
            let tb = mn.ifd.bytes(HUFFMAN).ok_or_else(|| RawError::Unsupported("compressed PEF without a Huffman table".into()))?;
            let huff = table(tb, mn.order)?;
            let chunks = info.chunks(bytes.len() as u64);
            let first = chunks.first().ok_or_else(|| RawError::Corrupt("PEF without image data".into()))?;
            let mut c = *first;
            c.len = chunks.iter().map(|c| c.len).sum::<u64>().max(first.len);
            let src = chunk_bytes(bytes, &c)
                .or_else(|| bytes.get(first.offset as usize..))
                .ok_or_else(|| RawError::Corrupt("PEF data outside file".into()))?;
            if read == Mode::Full { RawData::U16(decode_huffman(src, &huff, w, hgt, bits)?) } else { RawData::U16(Vec::new()) }
        }
        1 => {
            let strip: u64 = info.chunks(bytes.len() as u64).iter().map(|c| c.len).sum();
            let packing = if strip >= (w * hgt * 2) as u64 { Packing::Word16 } else { Packing::Msb };
            read_image_in(read, bytes, &info, tiff.order, packing)?
        }
        // Some bodies tag plain packed samples as PackBits (32773). Only a strip that holds exactly the packed
        // size of its rows is read, as the uncompressed MSB-first samples it is; any other size stays unsupported.
        32773 if is_exactly_packed(&info, bytes.len() as u64) => {
            let packed = ImageInfo { compression: 1, ..info.clone() };
            read_image_in(read, bytes, &packed, tiff.order, Packing::Msb)?
        }
        c => return Err(RawError::Unsupported(format!("PEF compression {c}"))),
    };
    let RawData::U16(ref samples) = data else { return Err(RawError::Unsupported("float PEF".into())) };

    let active = match tagged {
        Some(r) => r,
        None => dark_trimmed(samples, w, hgt),
    };
    let cfa = match (ifd0.u64s(t::CFA_REPEAT_PATTERN_DIM).as_deref(), ifd0.bytes(t::CFA_PATTERN_EP)) {
        (Some([2, 2]), Some(p)) if p.len() == 4 && p.iter().all(|&c| c <= 2) => Cfa { width: 2, height: 2, pattern: p.to_vec() },
        // Every body states its layout in the Exif `CFAPattern`, read from the first sample of the stored array, not
        // of the image area: the *ist D (origin 19, 11) and the 645D (80, 59) only fit unshifted. The 12-bit 10 MP
        // bodies and everything from the K-3 on say RGGB; the K-7 / K-5 / 645D generation says BGGR. Files without the
        // tag keep BGGR.
        _ => cfa_from_exif(&tiff).unwrap_or_else(|| Cfa::bayer_static("BGGR")),
    };
    let black = match mn.as_ref().and_then(|m| m.ifd.f64s(BLACK_POINT)).as_deref() {
        Some(v @ [_, _, _, _]) => {
            // RGGB-ordered levels → per CFA position at the active area origin
            let a = cfa.shifted(active.x, active.y);
            let values = a.pattern.iter().map(|&c| [v[0], (v[1] + v[2]) / 2.0, v[3]][c as usize] as f32).collect();
            BlackLevel { repeat_rows: 2, repeat_cols: 2, values, ..Default::default() }
        }
        _ if active.x >= 4 => black_from_columns(samples, w, 0..active.x.saturating_sub(2), 0..hgt, active),
        _ => BlackLevel::uniform(0.0),
    };
    let wb = mn.as_ref().and_then(|m| m.ifd.f64s(WB_LEVELS)).filter(|v| v.len() == 4 && v.iter().all(|x| *x > 0.0)).map(|v| {
        let g = (v[1] + v[2]) / 2.0;
        [(v[0] / g) as f32, 1.0, (v[3] / g) as f32]
    });
    let white = white_from_data(samples, bits);
    let mut metadata = dac_meta::from_tiff(&tiff);
    metadata.width = Some(active.width as u32);
    metadata.height = Some(active.height as u32);
    let img = RawImage {
        format: RawFormat::Pef,
        width: w,
        height: hgt,
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
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our own prefix code for categories 0..=14: lengths 2,2,3,3,4,5,..., codes assigned canonically.
    fn test_table() -> (Vec<u8>, Vec<(u16, u8)>) {
        let lens: [u8; 15] = [3, 2, 2, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12];
        let mut order: Vec<usize> = (0..15).collect();
        order.sort_by_key(|&i| (lens[i], i));
        let mut codes = vec![(0u16, 0u8); 15];
        let (mut code, mut prev) = (0u32, lens[order[0]]);
        for (k, &i) in order.iter().enumerate() {
            if k > 0 {
                code = (code + 1) << (lens[i] - prev);
            }
            prev = lens[i];
            codes[i] = ((code << (12 - lens[i])) as u16, lens[i]);
        }
        let mut b = vec![0u8, 3];
        b.extend_from_slice(&[0; 12]);
        for c in &codes {
            b.extend_from_slice(&c.0.to_be_bytes());
        }
        b.extend(codes.iter().map(|c| c.1));
        (b, codes)
    }

    fn encode(px: &[u16], w: usize, codes: &[(u16, u8)]) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let put = |v: u32, n: u8, bits: &mut Vec<bool>| (0..n).rev().for_each(|i| bits.push(v >> i & 1 == 1));
        let mut vpred = [[0i32; 2]; 2];
        for (y, row) in px.chunks(w).enumerate() {
            let mut hpred = [0i32; 2];
            for (x, &v) in row.iter().enumerate() {
                let pred = if x < 2 { vpred[y & 1][x] } else { hpred[x & 1] };
                let d = v as i32 - pred;
                if x < 2 {
                    vpred[y & 1][x] = v as i32;
                }
                hpred[x & 1] = v as i32;
                let s = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() } as u8;
                let (c, l) = codes[s as usize];
                put((c >> (12 - l)) as u32, l, &mut bits);
                if s > 0 {
                    let extra = if d < 0 { (d - 1) as u32 & ((1 << s) - 1) } else { d as u32 };
                    put(extra, s, &mut bits);
                }
            }
        }
        bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (b as u8) << (7 - i))).collect()
    }

    #[test]
    fn huffman_round_trip() {
        let (tb, codes) = test_table();
        let h = table(&tb, ByteOrder::Big).unwrap();
        let (w, hgt) = (37usize, 9usize);
        let px: Vec<u16> = (0..w * hgt).map(|i| ((i * 7919) % 16384) as u16).collect();
        let src = encode(&px, w, &codes);
        assert_eq!(decode_huffman(&src, &h, w, hgt, 14).unwrap(), px);
        // truncated data is reported, not a panic
        assert!(decode_huffman(&src[..src.len() / 3], &h, w, hgt, 14).is_err());
        assert!(table(&tb[..20], ByteOrder::Big).is_err());
    }

    /// A big-endian one-strip PEF-style TIFF of `w × h` 12-bit samples with the given compression tag and strip
    /// byte count, around `payload`.
    fn tiny_pef(w: u16, h: u16, compression: u16, strip_len: u32, payload: &[u8]) -> Vec<u8> {
        let entries: [(u16, u16, u32); 9] = [
            (0x0100, 3, w as u32 * 0x1_0000),           // ImageWidth
            (0x0101, 3, h as u32 * 0x1_0000),           // ImageLength
            (0x0102, 3, 12 * 0x1_0000),                 // BitsPerSample
            (0x0103, 3, compression as u32 * 0x1_0000), // Compression
            (0x0106, 3, 32803 * 0x1_0000),              // Photometric: CFA
            (0x0111, 4, 8 + 2 + 9 * 12 + 4),            // StripOffsets
            (0x0115, 3, 0x1_0000),                      // SamplesPerPixel
            (0x0116, 3, h as u32 * 0x1_0000),           // RowsPerStrip
            (0x0117, 4, strip_len),                     // StripByteCounts
        ];
        let mut f = b"MM\0*\0\0\0\x08".to_vec();
        f.extend_from_slice(&9u16.to_be_bytes());
        for (tag, ty, val) in entries {
            f.extend_from_slice(&tag.to_be_bytes());
            f.extend_from_slice(&ty.to_be_bytes());
            f.extend_from_slice(&1u32.to_be_bytes());
            f.extend_from_slice(&val.to_be_bytes());
        }
        f.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
        f.extend_from_slice(payload);
        f
    }

    /// Eight 12-bit samples, two rows of four, packed MSB-first: 0x123 0x456 0x789 0xabc / 0xdef 0x012 0x345 0x678.
    const PACKED: [u8; 12] = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78];

    #[test]
    fn packed_samples_under_the_packbits_tag_are_read_when_the_strip_has_exactly_the_packed_size() {
        let file = tiny_pef(4, 2, 32773, 12, &PACKED);
        let img = decode(&file, Mode::Full).unwrap();
        let RawData::U16(v) = &img.data else { panic!("integer samples") };
        assert_eq!(v, &[0x123, 0x456, 0x789, 0xabc, 0xdef, 0x012, 0x345, 0x678]);
        assert_eq!((img.width, img.height, img.bits), (4, 2, 12));
        // the same bytes tagged as plain uncompressed give the same samples
        let plain = decode(&tiny_pef(4, 2, 1, 12, &PACKED), Mode::Full).unwrap();
        assert_eq!(plain.data, img.data);
        // header-only mode accepts it too
        assert!(decode(&file, Mode::Header).is_ok());
    }

    #[test]
    fn a_packbits_tagged_strip_of_any_other_size_stays_unsupported() {
        for len in [11u32, 13, 6, 24] {
            let mut payload = PACKED.to_vec();
            payload.resize(24, 0x55);
            let file = tiny_pef(4, 2, 32773, len, &payload);
            let err = decode(&file, Mode::Full).unwrap_err();
            assert!(matches!(&err, RawError::Unsupported(m) if m.contains("32773")), "strip of {len} bytes: {err:?}");
        }
    }

    /// A 16 x 8 big-endian 12-bit PEF-style file with a 12 x 6 image area at the odd origin (3, 1), the Exif
    /// `CFAPattern` `pattern` and an AOC maker note with the area tags (and, for 65535, a Huffman table). The samples
    /// are plain 16-bit words (1), packed 12-bit (32773) or Huffman coded (65535).
    fn pef_with_pattern(compression: u16, pattern: [u8; 4]) -> Vec<u8> {
        use dac_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
        let (w, h) = (16usize, 8usize);
        let px: Vec<u16> = (0..w * h).map(|i| ((i * 37) % 900 + 100) as u16).collect();
        let (tb, codes) = test_table();
        let strip: Vec<u8> = match compression {
            1 => px.iter().flat_map(|v| v.to_be_bytes()).collect(),
            32773 => px.chunks(2).flat_map(|p| [(p[0] >> 4) as u8, ((p[0] & 15) << 4 | p[1] >> 8) as u8, p[1] as u8]).collect(),
            _ => encode(&px, w, &codes),
        };
        let build = |table_at: u32| {
            let mut ifd = IfdBuilder::new();
            ifd.set(0x0100, Value::Long(vec![w as u32]));
            ifd.set(0x0101, Value::Long(vec![h as u32]));
            ifd.set(0x0102, Value::Short(vec![12]));
            ifd.set(0x0103, Value::Short(vec![compression]));
            ifd.set(0x0106, Value::Short(vec![32803]));
            ifd.set(0x010f, Value::Ascii("PENTAX Corporation".into()));
            ifd.set_image(ImageData::Strips { rows_per_strip: h as u32, strips: vec![strip.clone()] });
            // AOC + "MM", then the IFD (area origin, area size and, for Huffman data, the table), next-IFD 0, the table
            let mut note = b"AOC MM".to_vec();
            let mut entries = vec![(0x0038u16, 3u16, 2u32, [0u8, 3, 0, 1]), (0x0039, 3, 2, [0, 12, 0, 6])];
            if compression == 65535 {
                entries.push((0x0220, 7, tb.len() as u32, table_at.to_be_bytes()));
            }
            note.extend_from_slice(&(entries.len() as u16).to_be_bytes());
            for (tag, ty, cnt, val) in entries {
                note.extend_from_slice(&tag.to_be_bytes());
                note.extend_from_slice(&ty.to_be_bytes());
                note.extend_from_slice(&cnt.to_be_bytes());
                note.extend_from_slice(&val);
            }
            note.extend_from_slice(&0u32.to_be_bytes());
            if compression == 65535 {
                note.extend_from_slice(&tb);
            }
            let mut exif = IfdBuilder::new();
            exif.set(0xa302, Value::Undefined([&[0, 2, 0, 2][..], &pattern[..]].concat()));
            exif.set(0x927c, Value::Undefined(note));
            ifd.set_child(0x8769, exif);
            TiffWriter::new(ByteOrder::Big, false).write(&[ifd]).unwrap()
        };
        let first = build(0);
        if compression != 65535 {
            return first;
        }
        let at = Tiff::parse(&first).unwrap().exif().unwrap().get(0x927c).unwrap().offset;
        build((at + 6 + 2 + 3 * 12 + 4) as u32)
    }

    #[test]
    fn the_pattern_is_the_exif_cfapattern_from_the_array_origin_for_every_compression() {
        for compression in [1u16, 32773, 65535] {
            for (p, name) in [([0u8, 1, 1, 2], "RGGB"), ([2, 1, 1, 0], "BGGR"), ([1, 0, 2, 1], "GRBG"), ([1, 2, 0, 1], "GBRG")] {
                let img = decode(&pef_with_pattern(compression, p), Mode::Full).unwrap();
                assert_eq!(img.active_area, Rect::new(3, 1, 12, 6), "compression {compression}");
                assert_eq!(img.cfa, Some(Cfa::bayer_static(name)), "compression {compression}, Exif {name}");
            }
        }
        // no tag: the assumed layout stays
        let img = decode(&tiny_pef(4, 2, 1, 16, &[0; 16]), Mode::Full).unwrap();
        assert_eq!(img.cfa, Some(Cfa::bayer_static("BGGR")));
    }
}
