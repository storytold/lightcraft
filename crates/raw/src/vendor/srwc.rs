//! Samsung SRW: the three compressed codings of the raw image.
//!
//! Sources: prose descriptions of the formats (the open decoders' documentation and our reading of how they work,
//! no code copied), checked against CC0 samples from raw.pixls.us (NX300 for compression 32770, NX3000 for 32772,
//! NX1 and NX500 for 32773: their decoded mosaics follow the camera's own JPEG, `corpus_samsung_srw`).
//!
//! All three read bits MSB-first; 32770 and 32773 take them from little-endian 32-bit words, 32772 from bytes.
//!
//! - **32770 with a row table** (NX30, NX300, NX2000, EK-GN120): tag `0xa010` of the raw IFD points at one
//!   little-endian 32-bit offset per row, counted from the strip. Each row is blocks of 16 samples: a direction bit,
//!   four 2-bit length updates (keep, +1, -1, or a new 4-bit length; lengths start at 7 in the first two rows, 4
//!   after), then 8 even- and 8 odd-position signed differences. The prediction is the sample one row (even
//!   positions) or two rows (odd) above, or, going left, the last even / odd sample of the previous block (128 at
//!   the row start). Finally the samples of each 2x2 cell swap their top-right and bottom-left values.
//! - **32772** (NX mini, NX3000, NX3300): one bit stream of canonical-Huffman-coded difference lengths (the
//!   lossless-JPEG difference coding) with a fixed 14-entry table; each sample adds its difference to the previous
//!   same-colour sample of the row, the first two of a row to those two rows of the same parity above.
//! - **32773** (NX1, NX500): a 16-byte header (bit depth, option flags, initial value), then rows starting at
//!   16-byte boundaries, each blocks of 16 samples (8 of one colour column parity, interleaved) with an optional
//!   quantizer step, a prediction mode (left, or the mean of two samples above at one of six offsets) and four
//!   difference lengths updated like 32770's.
use crate::{RawError, Result};

/// MSB-first bits from little-endian 32-bit words, loaded only when needed (so a row's end position is a word
/// boundary after its last used bit).
struct Words<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u64,
    have: u32,
}

impl<'a> Words<'a> {
    fn new(src: &'a [u8], pos: usize) -> Self {
        Self { src, pos, buf: 0, have: 0 }
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if self.have < n {
            let w = self
                .src
                .get(self.pos..self.pos.saturating_add(4))
                .and_then(|b| b.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| RawError::Corrupt("SRW compressed data truncated".into()))?;
            self.pos += 4;
            self.buf = (self.buf << 32) | u64::from(w);
            self.have += 32;
        }
        self.have -= n;
        Ok(((self.buf >> self.have) & ((1u64 << n) - 1)) as u32)
    }
}

/// A signed difference of `n` bits (two's complement).
fn signed(v: u32, n: u32) -> i32 {
    if n == 0 {
        0
    } else if v >> (n - 1) != 0 {
        v as i32 - (1i32 << n)
    } else {
        v as i32
    }
}

fn store(v: i32) -> u16 {
    v.clamp(0, u16::MAX as i32) as u16
}

/// Compression 32770 with a row-offset table at `table` (absolute file offset); `strip` is the strip's offset.
pub(crate) fn decode_v0(bytes: &[u8], strip: usize, table: usize, width: usize, h: usize) -> Result<Vec<u16>> {
    if h < 2 || width < 2 {
        return Err(RawError::Corrupt(format!("Samsung compressed raw {width}x{h}")));
    }
    // rows are whole 16-sample blocks: decode into rows padded to that, keep the stored width
    let w = width.next_multiple_of(16);
    let mut d = vec![0u16; w * h];
    for row in 0..h {
        let off = table
            .checked_add(row.saturating_mul(4))
            .and_then(|o| bytes.get(o..o.checked_add(4)?))
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| RawError::Corrupt("SRW row table truncated".into()))?;
        let mut bits = Words::new(bytes, strip.saturating_add(off as usize));
        let mut len = [if row < 2 { 7u32 } else { 4 }; 4];
        for col in (0..w).step_by(16) {
            let up = bits.bits(1)? == 1;
            let mut op = [0u32; 4];
            for o in &mut op {
                *o = bits.bits(2)?;
            }
            for (l, o) in len.iter_mut().zip(op) {
                *l = match o {
                    3 => bits.bits(4)?,
                    2 => l.saturating_sub(1),
                    1 => (*l + 1).min(16),
                    _ => *l,
                };
            }
            for parity in 0..2 {
                // the left prediction of this parity: the previous block's last sample of it
                let left = if col == 0 { 128 } else { i32::from(d[row * w + col - 2 + parity]) };
                for c in (parity..16).step_by(2) {
                    let n = len[(parity << 1) | (c >> 3)];
                    let diff = signed(bits.bits(n)?, n);
                    let pred = if up {
                        let r = row.checked_sub(1 + parity).ok_or_else(|| RawError::Corrupt("SRW block predicts from above the first row".into()))?;
                        i32::from(d[r * w + col + c])
                    } else {
                        left
                    };
                    d[row * w + col + c] = store(pred + diff);
                }
            }
        }
    }
    // each 2x2 cell is stored with its top-right and bottom-left samples exchanged
    for row in (0..h - 1).step_by(2) {
        for col in (0..w - 1).step_by(2) {
            d.swap(row * w + col + 1, (row + 1) * w + col);
        }
    }
    Ok(d.chunks_exact(w).flat_map(|r| &r[..width]).copied().collect())
}

/// The 32772 code table: (code length, difference length) in code order.
const V1_TABLE: [(u32, u32); 14] =
    [(3, 4), (3, 7), (2, 6), (2, 5), (4, 3), (6, 0), (7, 9), (8, 10), (9, 11), (10, 12), (10, 13), (5, 1), (4, 8), (4, 2)];

/// Compression 32772: the strip at `src`.
pub(crate) fn decode_v1(src: &[u8], w: usize, h: usize, bits_per_sample: u32) -> Result<Vec<u16>> {
    // canonical codes in table order: entry i takes the next 1024 >> len slots of a 10-bit lookup
    let mut lookup = [(0u32, 0u32); 1024];
    let mut n = 0usize;
    for &(cl, dl) in &V1_TABLE {
        for _ in 0..(1024 >> cl) {
            if let Some(s) = lookup.get_mut(n) {
                *s = (cl, dl);
            }
            n += 1;
        }
    }
    let mut d = vec![0u16; w * h];
    let (mut pos, mut buf, mut have) = (0usize, 0u64, 0u32);
    let mut vpred = [[0i32; 2]; 2];
    let limit = 1i64 << bits_per_sample.clamp(8, 16);
    for row in 0..h {
        let mut hpred = [0i32; 2];
        for col in 0..w {
            while have < 32 {
                // past the end: zero bits, caught below if a code needs them
                buf = (buf << 8) | u64::from(src.get(pos).copied().unwrap_or(0));
                pos += 1;
                have += 8;
            }
            let (cl, dl) = lookup[((buf >> (have - 10)) & 1023) as usize];
            have -= cl;
            let diff = if dl == 0 {
                0
            } else {
                let v = ((buf >> (have - dl)) & ((1u64 << dl) - 1)) as i32;
                have -= dl;
                if v & (1 << (dl - 1)) == 0 { v - ((1 << dl) - 1) } else { v }
            };
            if pos > src.len().saturating_add(8) {
                return Err(RawError::Corrupt("SRW compressed data truncated".into()));
            }
            let p = col & 1;
            if col < 2 {
                vpred[row & 1][col] += diff;
                hpred[col] = vpred[row & 1][col];
            } else {
                hpred[p] += diff;
            }
            if !(0..limit).contains(&i64::from(hpred[p])) {
                return Err(RawError::Corrupt(format!("SRW sample out of range at row {row}")));
            }
            d[row * w + col] = hpred[p] as u16;
        }
    }
    Ok(d)
}

/// Compression 32773: the strip at `src`. Returns the samples and the header's bit depth.
pub(crate) fn decode_v2(src: &[u8], w: usize, h: usize) -> Result<(Vec<u16>, u32)> {
    let header = src.get(..16).ok_or_else(|| RawError::Corrupt("SRW compressed header truncated".into()))?;
    let depth = u32::from(header[1] & 15) + 1;
    let opt = u32::from(header[9] & 15);
    let init = i32::from(u16::from_le_bytes([header[12], header[13]]) & 0x3fff);
    if !(8..=16).contains(&depth) {
        return Err(RawError::Unsupported(format!("Samsung compressed raw with {depth}-bit samples")));
    }
    let max = (1i32 << depth) - 1;
    let mut d = vec![0u16; w * h];
    let mut pos = 16usize;
    for row in 0..h {
        pos = pos.next_multiple_of(16);
        let mut bits = Words::new(src, pos);
        let (mut mag, mut pmode) = (0i32, 7u32);
        let mut lent = [[if row < 2 { 7u32 } else { 4 }; 2]; 3];
        let mut len = [0u32; 4];
        let odd = row & 1;
        for tab in (0..w.saturating_sub(15)).step_by(16) {
            if opt & 4 == 0 && tab % 64 == 0 {
                mag = match bits.bits(2)? {
                    0 => mag,
                    1 => mag - 2,
                    2 => mag + 2,
                    _ => bits.bits(12)? as i32,
                };
            }
            if opt & 2 != 0 {
                pmode = 7 - 4 * bits.bits(1)?;
            } else if bits.bits(1)? == 0 {
                pmode = bits.bits(3)?;
            }
            if opt & 1 != 0 || bits.bits(1)? == 0 {
                for l in &mut len {
                    *l = bits.bits(2)?;
                }
                for (c, l) in len.iter_mut().enumerate() {
                    let i = ((odd << 1) | (c & 1)) % 3;
                    *l = match *l {
                        0 => lent[i][0],
                        1 => (lent[i][0] + 1).min(16),
                        2 => lent[i][0].saturating_sub(1),
                        _ => bits.bits(4)?,
                    };
                    lent[i] = [lent[i][1], *l];
                }
            }
            for c in 0..16 {
                let col = tab + ((((c & 7) << 1) ^ (c >> 3)) ^ odd);
                let pred = if pmode == 7 || row < 2 {
                    if tab == 0 { init } else { i32::from(d[row * w + tab - 2 + (col & 1)]) }
                } else {
                    // samples of this colour above: the row above (diagonal neighbours) or two rows above
                    let diagonal = (col & 1) == odd;
                    let (r, shift): (usize, isize) = if diagonal { (row - 1, if odd == 0 { 1 } else { -1 }) } else { (row - 2, 0) };
                    let a = [-4isize, -2, -2, 0, 0, 2, 4][pmode as usize];
                    let b = [-4isize, -2, 0, 0, 2, 2, 4][pmode as usize];
                    let at = |o: isize| -> Result<i32> {
                        let x = (col as isize).checked_add(shift + o).filter(|x| (0..w as isize).contains(x));
                        let x = x.ok_or_else(|| RawError::Corrupt("SRW prediction outside the row".into()))?;
                        Ok(i32::from(d[r * w + x as usize]))
                    };
                    (at(a)? + at(b)? + 1) >> 1
                };
                let n = len[c >> 2];
                let diff = signed(bits.bits(n)?, n);
                let v = pred + diff * (mag * 2 + 1) + mag;
                d[row * w + col] = v.clamp(0, max) as u16;
            }
        }
        pos = bits.pos;
    }
    Ok((d, depth))
}

/// The four Bayer layouts (colour of each 2x2 site, row-major from the stored array's first sample).
const LAYOUTS: [[u8; 4]; 4] = [[0, 1, 1, 2], [1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]];

/// The Bayer layout whose colours follow the camera's own embedded JPEG: the compressed files' `CFAPattern` is not
/// reliable (the NX300 and NX2000 state RGGB and the NX30 GRBG for the same sensor read-out; the NX3000 states GBRG
/// for a GRBG mosaic). Per cell of a coarse grid over `active`, the log red/green and blue/green ratios of the raw
/// sites are correlated with the JPEG's; the layout must win clearly, else `None` (keep the tag).
pub(crate) fn layout_from_preview(jpeg: &[u8], d: &[u16], w: usize, active: crate::Rect, black: f64) -> Option<Vec<u8>> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    const GW: usize = 24;
    const GH: usize = 18;
    let opts = DecoderOptions::default().set_strict_mode(false).jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let (jw, jh) = (info.width as usize, info.height as usize);
    if jw < GW || jh < GH || px.len() < jw * jh * 3 {
        return None;
    }
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let mut jc = vec![([0f64; 3], 0f64); GW * GH];
    for y in 0..jh {
        for x in 0..jw {
            let cell = &mut jc[(y * GH / jh) * GW + x * GW / jw];
            for c in 0..3 {
                cell.0[c] += lin(px[(y * jw + x) * 3 + c]);
            }
            cell.1 += 1.0;
        }
    }
    let (x0, y0) = ((active.x + 1) & !1, (active.y + 1) & !1);
    let (cw, ch) = ((active.x + active.width).saturating_sub(x0) / 2, (active.y + active.height).saturating_sub(y0) / 2);
    if cw < GW || ch < GH {
        return None;
    }
    let mut rc = vec![([0f64; 4], 0f64); GW * GH];
    for cy in 0..ch {
        for cx in 0..cw {
            let cell = &mut rc[(cy * GH / ch) * GW + cx * GW / cw];
            for site in 0..4 {
                let v = d.get((y0 + 2 * cy + site / 2) * w + x0 + 2 * cx + site % 2).copied().unwrap_or(0);
                cell.0[site] += f64::from(v) - black;
            }
            cell.1 += 1.0;
        }
    }
    let score = |layout: &[u8; 4]| {
        let (mut xs, mut ys) = ([vec![], vec![]], [vec![], vec![]]);
        for ((s, sn), (j, jn)) in rc.iter().zip(&jc) {
            let mut rgb = [0f64; 3];
            for (site, &c) in layout.iter().enumerate() {
                rgb[c as usize] += s[site] / sn / if c == 1 { 2.0 } else { 1.0 };
            }
            let j = j.map(|v| v / jn);
            if rgb.iter().all(|v| *v > 1.0) && j.iter().all(|v| (0.002..0.9).contains(v)) {
                for (k, c) in [0, 2].into_iter().enumerate() {
                    xs[k].push((rgb[c] / rgb[1]).ln());
                    ys[k].push((j[c] / j[1]).ln());
                }
            }
        }
        (correlation(&xs[0], &ys[0]) + correlation(&xs[1], &ys[1])) / 2.0
    };
    let mut scored: Vec<(f64, [u8; 4])> = LAYOUTS.iter().map(|l| (score(l), *l)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, runner_up) = (scored.first()?, scored.get(1)?);
    (best.0 > 0.5 && best.0 - runner_up.0 > 0.3).then(|| best.1.to_vec())
}

/// Pearson correlation; 0 for fewer than 16 points or no spread.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 16 {
        return 0.0;
    }
    let (ma, mb) = (a.iter().sum::<f64>() / n as f64, b.iter().sum::<f64>() / n as f64);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    let den = (saa * sbb).sqrt();
    if den > 0.0 { sab / den } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bits packed into little-endian 32-bit words.
    #[derive(Default)]
    struct Writer {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Writer {
        fn put(&mut self, v: u32, bits: u32) {
            if bits == 0 {
                return;
            }
            self.acc = (self.acc << bits) | u64::from(v & ((1u32 << bits) - 1));
            self.n += bits;
            while self.n >= 32 {
                self.n -= 32;
                self.out.extend_from_slice(&((self.acc >> self.n) as u32).to_le_bytes());
            }
        }
        /// Flush to a word boundary.
        fn finish(&mut self) -> Vec<u8> {
            if self.n > 0 {
                let pad = 32 - self.n;
                self.put(0, pad);
            }
            std::mem::take(&mut self.out)
        }
    }

    fn image(w: usize, h: usize) -> Vec<u16> {
        (0..w * h).map(|i| ((i % w) * 7 + (i / w) * 13 + (i % 3) * 50) as u16 % 4000).collect()
    }

    /// 32770: every block predicts left, with an explicit 13-bit length.
    fn encode_v0(img: &[u16], w: usize, h: usize) -> Vec<u8> {
        let wp = w.next_multiple_of(16);
        let mut s = vec![0u16; wp * h];
        for r in 0..h {
            s[r * wp..r * wp + w].copy_from_slice(&img[r * w..r * w + w]);
        }
        for r in (0..h - 1).step_by(2) {
            for c in (0..wp - 1).step_by(2) {
                s.swap(r * wp + c + 1, (r + 1) * wp + c);
            }
        }
        let (mut table, mut data) = (vec![], vec![]);
        for r in 0..h {
            table.extend_from_slice(&(data.len() as u32).to_le_bytes());
            let mut b = Writer::default();
            for col in (0..wp).step_by(16) {
                b.put(0, 1);
                for _ in 0..4 {
                    b.put(3, 2);
                }
                for _ in 0..4 {
                    b.put(13, 4);
                }
                for parity in 0..2 {
                    let left = if col == 0 { 128 } else { i32::from(s[r * wp + col - 2 + parity]) };
                    for c in (parity..16).step_by(2) {
                        b.put((i32::from(s[r * wp + col + c]) - left) as u32, 13);
                    }
                }
            }
            data.extend(b.finish());
        }
        // file: row table at 0, strip after it
        let mut file = table;
        file.extend(data);
        file
    }

    #[test]
    fn row_block_coding_round_trips() {
        let (w, h) = (40, 6); // not a whole number of blocks: rows are padded to 48
        let img = image(w, h);
        let file = encode_v0(&img, w, h);
        assert_eq!(decode_v0(&file, h * 4, 0, w, h).unwrap(), img);
    }

    fn encode_v1(img: &[u16], w: usize, h: usize) -> Vec<u8> {
        let mut starts = [0u32; 17];
        let mut slot = 0u32;
        for &(cl, dl) in &V1_TABLE {
            starts[dl as usize] = (slot >> (10 - cl)) | (cl << 16);
            slot += 1024 >> cl;
        }
        let (mut bits, mut acc, mut n) = (vec![], 0u64, 0u32);
        let mut put = |v: u32, k: u32, bits: &mut Vec<u8>| {
            acc = (acc << k) | u64::from(v);
            n += k;
            while n >= 8 {
                n -= 8;
                bits.push((acc >> n) as u8);
            }
        };
        let mut vpred = [[0i32; 2]; 2];
        for r in 0..h {
            let mut hpred = [0i32; 2];
            for c in 0..w {
                let v = i32::from(img[r * w + c]);
                let p = if c < 2 { vpred[r & 1][c] } else { hpred[c & 1] };
                let diff = v - p;
                let dl = 32 - diff.unsigned_abs().leading_zeros();
                let code = starts[dl as usize];
                put(code & 0xffff, code >> 16, &mut bits);
                let stored = if diff >= 0 { diff } else { diff + (1 << dl) - 1 };
                put(stored as u32, dl, &mut bits);
                if c < 2 {
                    vpred[r & 1][c] = v;
                }
                hpred[c & 1] = v;
            }
        }
        put(0, 7, &mut bits);
        bits
    }

    #[test]
    fn huffman_coding_round_trips() {
        let (w, h) = (34, 5);
        let img = image(w, h);
        assert_eq!(decode_v1(&encode_v1(&img, w, h), w, h, 12).unwrap(), img);
    }

    /// 32773 with options 7 (no quantizer, left prediction, explicit lengths of 12 bits) and initial value 100.
    fn encode_v2(img: &[u16], w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; 16];
        out[1] = 13; // 14-bit samples
        out[9] = 7;
        out[12..14].copy_from_slice(&100u16.to_le_bytes());
        for r in 0..h {
            out.resize(out.len().next_multiple_of(16), 0);
            let mut b = Writer::default();
            for tab in (0..w.saturating_sub(15)).step_by(16) {
                b.put(0, 1); // pmode 7
                for _ in 0..4 {
                    b.put(3, 2);
                }
                for _ in 0..4 {
                    b.put(14, 4);
                }
                for c in 0..16 {
                    let col = tab + ((((c & 7) << 1) ^ (c >> 3)) ^ (r & 1));
                    let pred = if tab == 0 { 100 } else { i32::from(img[r * w + tab - 2 + (col & 1)]) };
                    b.put((i32::from(img[r * w + col]) - pred) as u32, 14);
                }
            }
            out.extend(b.finish());
        }
        out
    }

    #[test]
    fn block_coding_round_trips() {
        let (w, h) = (48, 4);
        let img = image(w, h);
        let (d, depth) = decode_v2(&encode_v2(&img, w, h), w, h).unwrap();
        assert_eq!((d, depth), (img, 14));
    }

    #[test]
    fn hostile_streams_do_not_panic() {
        let (w, h) = (40, 6);
        let img = image(w, h);
        let v0 = encode_v0(&img, w, h);
        let v1 = encode_v1(&img, w, h);
        let v2 = encode_v2(&img, 48, 4);
        let noise: Vec<u8> = (0..4000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        for cut in [0, 1, 7, 16, 23, 64, 100] {
            let _ = decode_v0(v0.get(..cut).unwrap_or(&v0), h * 4, 0, w, h);
            let _ = decode_v1(v1.get(..cut).unwrap_or(&v1), w, h, 12);
            let _ = decode_v2(v2.get(..cut).unwrap_or(&v2), 48, 4);
        }
        for (ww, hh) in [(0, 0), (1, 1), (16, 1), (17, 3), (64, 64)] {
            let _ = decode_v0(&noise, 7, 3, ww, hh);
            let _ = decode_v0(&noise, usize::MAX - 2, usize::MAX - 2, ww, hh);
            let _ = decode_v1(&noise, ww, hh, 12);
            let _ = decode_v2(&noise, ww, hh);
        }
        // a stream that asks for predictions from above in the first rows
        let mut up = vec![0u8; 8];
        up.extend([0xff; 64]);
        assert!(decode_v0(&up, 8, 0, 16, 2).is_err());
        // a bit depth outside 8..=16 is refused, not decoded
        let mut bad = v2.clone();
        bad[1] = 3;
        assert!(matches!(decode_v2(&bad, 48, 4), Err(RawError::Unsupported(_))));
    }
}
