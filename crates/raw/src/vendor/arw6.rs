//! Sony ARW6 ("Compressed RAW 2" / HQ, TIFF Compression 32766): container parsing.
//!
//! Clean-room. The layout comes from SMPTE RDD 34 (Picture_info in word 0) and from Phase 0 black-box
//! inspection of real ILCE-7RM6 files (tile table, group size words, stream headers and index entries), each rule
//! checked by re-serialising parsed tiles byte-for-byte; see `docs/superpowers/specs/2026-10-09-sony-arw6-decoder-design.md`
//! (*Container (Sony)*). No other raw decoder's source was consulted.

// Consumed by the geometry and `decode` tasks that follow.
#![allow(dead_code)]

use crate::{RawError, Result};

fn corrupt(why: &str) -> RawError {
    RawError::Corrupt(format!("ARW6: {why}"))
}

fn unsupported(why: &str) -> RawError {
    RawError::Unsupported(format!("ARW6 {why}"))
}

fn u16_be(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_be_bytes([*s.first()?, *s.get(1)?])))
}

fn u24_be(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(3)?)?;
    Some(s.iter().fold(0usize, |a, &v| (a << 8) | usize::from(v)))
}

fn u32_le(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(s.try_into().ok()?) as usize)
}

/// One tile of the strip: its bytes run from `offset` to the next tile's offset (the last: the strip end).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TileEntry {
    pub offset: usize,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Most tiles a strip may hold (real files have 1 or 4).
const MAX_TILES: usize = 16;

/// Parse the tile table at the start of the strip (file structure, spec *Container*): `u32` count, `u32` 0, then
/// 24-byte entries `u64` offset (relative to the strip), `u32` x, y, width, height, all little-endian.
pub(crate) fn parse_tile_table(strip: &[u8], width: usize, height: usize) -> Result<Vec<TileEntry>> {
    let count = u32_le(strip, 0).ok_or_else(|| corrupt("truncated tile table"))?;
    if count == 0 || count > MAX_TILES {
        return Err(unsupported("tile count outside 1..=16"));
    }
    let mut tiles: Vec<TileEntry> = Vec::with_capacity(count);
    let mut area = 0usize;
    for i in 0..count {
        let at = 8 + 24 * i;
        let e = strip.get(at..at + 24).ok_or_else(|| corrupt("truncated tile table"))?;
        let offset = e.get(..8).and_then(|b| <[u8; 8]>::try_from(b).ok()).map(u64::from_le_bytes).and_then(|o| usize::try_from(o).ok());
        let offset = offset.filter(|&o| o < strip.len()).ok_or_else(|| corrupt("tile offset outside the strip"))?;
        if tiles.last().is_some_and(|p| offset <= p.offset) {
            return Err(corrupt("tile offsets do not increase"));
        }
        let (x, y, w, h) = (u32_le(e, 8), u32_le(e, 12), u32_le(e, 16), u32_le(e, 20));
        let (Some(x), Some(y), Some(w), Some(h)) = (x, y, w, h) else { return Err(corrupt("truncated tile table")) };
        // Even geometry; the level-3 band width w / 16 must be whole (w/2 a multiple of 8).
        if w == 0 || h == 0 || x % 2 != 0 || y % 2 != 0 || w % 2 != 0 || h % 2 != 0 || (w / 2) % 8 != 0 {
            return Err(unsupported("tile geometry"));
        }
        let fits = x.checked_add(w).is_some_and(|r| r <= width) && y.checked_add(h).is_some_and(|b| b <= height);
        if !fits {
            return Err(corrupt("tile outside the image"));
        }
        let a = w.checked_mul(h).filter(|&a| a <= crate::MAX_SAMPLES).ok_or(RawError::Limit("ARW6 tile too large"))?;
        area = area.checked_add(a).ok_or(RawError::Limit("ARW6 tiles too large"))?;
        tiles.push(TileEntry { offset, x, y, width: w, height: h });
    }
    for (i, a) in tiles.iter().enumerate() {
        for b in tiles.iter().skip(i + 1) {
            if a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height {
                return Err(corrupt("tiles overlap"));
            }
        }
    }
    // In-image, disjoint tiles cover the image exactly when their areas sum to it.
    if width.checked_mul(height) != Some(area) {
        return Err(corrupt("tiles do not cover the image"));
    }
    Ok(tiles)
}

/// Tile `i`'s bytes: from its offset to the next tile's offset (the last: the strip end).
pub(crate) fn tile_bytes<'a>(strip: &'a [u8], tiles: &[TileEntry], i: usize) -> Option<&'a [u8]> {
    let start = tiles.get(i)?.offset;
    let end = match tiles.get(i.checked_add(1)?) {
        Some(next) => next.offset,
        None => strip.len(),
    };
    strip.get(start..end)
}

/// One of a tile's 13 streams (positions in 16-byte words from the tile start).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamRef {
    pub group: u8,
    pub comp: u8,
    pub word: usize,
    pub size_words: usize,
    pub index_words: usize,
    pub data_words: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TileHeader {
    pub hs: usize,
    pub vs: usize,
    pub ntu: usize,
    /// File order: g0 c0..2, g1 c0..2, g2 c0..2, g3 c0..2, g4 c0.
    pub streams: Vec<StreamRef>,
}

/// `n` bits at bit position `pos` (MSB-first) of a tile's word 0.
fn field(w: &[u8], pos: usize, n: usize) -> Option<u32> {
    let mut v = 0u32;
    for i in pos..pos.checked_add(n)? {
        let byte = *w.get(i / 8)?;
        v = (v << 1) | u32::from((byte >> (7 - i % 8)) & 1);
    }
    Some(v)
}

/// Streams per group: g0..g3 one per colour component, g4 the green residual.
const GROUP_STREAMS: [usize; 5] = [3, 3, 3, 3, 1];

/// Parse word 0 (RDD 34 `Picture_info`; every real file has HS = tile width, VS = tile height / 2, BBD 16, NC 3, NW 3,
/// NP 1000000000b), the group size words 3..=7 and each stream's header word (spec *Container*, Phase 0).
pub(crate) fn parse_tile_header(tile: &[u8], tw: usize, th: usize) -> Result<TileHeader> {
    let w0 = tile.get(..16).ok_or_else(|| corrupt("tile shorter than its picture header"))?;
    let f = |pos, n| field(w0, pos, n).map(|v| v as usize);
    let (hs, vs, bbd, nc, nw, np) = (f(64, 16), f(80, 16), f(102, 6), f(112, 3), f(115, 3), f(118, 10));
    let (Some(hs), Some(vs), Some(bbd), Some(nc), Some(nw), Some(np)) = (hs, vs, bbd, nc, nw, np) else {
        return Err(corrupt("picture header"));
    };
    if hs != tw || vs * 2 != th || bbd != 16 || nc != 3 || nw != 3 || np != 0b10_0000_0000 {
        return Err(unsupported(&format!("picture header variant (HS {hs}, VS {vs}, BBD {bbd}, NC {nc}, NW {nw}, NP {np:#b})")));
    }
    let mut streams = Vec::with_capacity(13);
    let mut word = 8usize;
    for (group, &n) in GROUP_STREAMS.iter().enumerate() {
        let table = tile.get((3 + group) * 16..(4 + group) * 16).ok_or_else(|| corrupt("truncated group table"))?;
        if usize::from(table.first().copied().unwrap_or(0)) != n {
            return Err(unsupported("group stream count"));
        }
        for comp in 0..n {
            let size_words = u24_be(table, 1 + 3 * comp).ok_or_else(|| corrupt("group table"))?;
            let end = word.checked_add(size_words).filter(|e| e.checked_mul(16).is_some_and(|b| b <= tile.len()));
            let end = end.ok_or_else(|| corrupt("stream outside the tile"))?;
            let hdr = tile.get(word * 16..word * 16 + 16).ok_or_else(|| corrupt("stream outside the tile"))?;
            let (index_words, data_words) =
                (u16_be(hdr, 0).ok_or_else(|| corrupt("stream header"))?, u24_be(hdr, 2).ok_or_else(|| corrupt("stream header"))?);
            if index_words.checked_add(data_words).and_then(|s| s.checked_add(1)).is_none_or(|s| s > size_words) {
                return Err(corrupt("stream header sizes exceed the stream"));
            }
            streams.push(StreamRef { group: group as u8, comp: comp as u8, word, size_words, index_words, data_words });
            word = end;
        }
    }
    Ok(TileHeader { hs, vs, ntu: vs.div_ceil(16), streams })
}

/// One half-TU of a stream: `len` bytes at byte `start` of the tile, quantiser index per band
/// (g0/g4: the single nibble repeated).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Half {
    pub start: usize,
    pub len: usize,
    pub qi: [u32; 3],
}

fn nibbles(b: &[u8], from: usize, n: usize) -> Option<u32> {
    (from..from + n).try_fold(0u32, |a, i| Some((a << 4) | u32::from((*b.get(i / 2)? >> (4 * (1 - i % 2))) & 15)))
}

/// The `2 * ntu` halves of a stream. Index entry per TU (file structure): `A:16 X B:16 X` with A / B the byte
/// lengths of half 0 / 1 and X the QI nibble (g0, g4: 1 nibble) or three nibbles HL, LH, HH (g1..g3); the data area
/// holds each TU's A bytes then B bytes, consecutive TUs contiguous.
pub(crate) fn stream_halves(tile: &[u8], s: &StreamRef, ntu: usize) -> Result<Vec<Half>> {
    let nx = if s.group == 0 || s.group == 4 { 1 } else { 3 };
    let esz = 4 + nx; // bytes: (16 + 4nx + 16 + 4nx) / 8
    let idx_start = s.word.checked_add(1).and_then(|w| w.checked_mul(16)).ok_or_else(|| corrupt("stream position"))?;
    let idx_len = s.index_words.checked_mul(16).ok_or_else(|| corrupt("stream position"))?;
    if ntu.checked_mul(esz).is_none_or(|n| n > idx_len) {
        return Err(corrupt("index table too small for the tile's TUs"));
    }
    let data_start = idx_start.checked_add(idx_len).ok_or_else(|| corrupt("stream position"))?;
    let index = tile.get(idx_start..data_start).ok_or_else(|| corrupt("index table outside the tile"))?;
    let data_end = s.data_words.checked_mul(16).and_then(|d| d.checked_add(data_start)).ok_or_else(|| corrupt("stream position"))?;
    if data_end > tile.len() {
        return Err(corrupt("stream data outside the tile"));
    }
    let mut halves = Vec::with_capacity(2 * ntu);
    let mut pos = data_start;
    for k in 0..ntu {
        let e = index.get(k * esz..(k + 1) * esz).ok_or_else(|| corrupt("index table"))?;
        let bad = || corrupt("index entry");
        let (a, x1) = (nibbles(e, 0, 4).ok_or_else(bad)? as usize, nibbles(e, 4, nx).ok_or_else(bad)?);
        let (b, x2) = (nibbles(e, 4 + nx, 4).ok_or_else(bad)? as usize, nibbles(e, 8 + nx, nx).ok_or_else(bad)?);
        for (len, x) in [(a, x1), (b, x2)] {
            let end = pos.checked_add(len).filter(|&e| e <= data_end).ok_or_else(|| corrupt("half outside its stream's data"))?;
            let qi = if nx == 1 { [x; 3] } else { [(x >> 8) & 15, (x >> 4) & 15, x & 15] };
            halves.push(Half { start: pos, len, qi });
            pos = end;
        }
    }
    Ok(halves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llvc::Plane;
    use crate::vendor::arw6_testenc::*;
    use crate::vendor::pef::Bits;
    use lightcraft_tiff::Tiff;
    use lightcraft_tiff::image::chunk_bytes;
    use lightcraft_tiff::tags as t;

    fn planes(w2: usize, h2: usize, seed: u64) -> (Plane, Plane, Plane, Plane) {
        let mut x = seed.max(1);
        let mut next = |span: i32| -> i32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % (2 * span as u64 + 1)) as i32 - span
        };
        let mut mk = |centre: i32, span: i32| Plane { width: w2, height: h2, data: (0..w2 * h2).map(|_| centre + next(span)).collect() };
        (mk(2048, 200), mk(0, 60), mk(0, 60), mk(0, 24))
    }

    fn one_tile(w: usize, h: usize, s: u8, qi: Qis) -> (Vec<u8>, TileSpec) {
        let (m, c1, c2, res) = planes(w / 2, h / 2, 7);
        let t = TileSpec { s, qi, m, c1, c2, res };
        (encode_tile(&t), t)
    }

    /// The raw strip bytes of the compression-32766 IFD, as `arw.rs` locates them.
    fn strip_of(file: &[u8]) -> &[u8] {
        let tiff = Tiff::parse(file).unwrap();
        let raw = tiff.all_ifds().into_iter().find(|i| i.u16(t::COMPRESSION) == Some(32766)).unwrap();
        let info = raw.image().unwrap();
        let chunks = info.chunks(file.len() as u64);
        chunk_bytes(file, &chunks[0]).unwrap()
    }

    #[test]
    fn tile_table_round_trips() {
        let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
        let file = arw6_file(&[(0, 0, 64, 48, tile.clone())], 64, 48);
        let strip = strip_of(&file);
        let t = parse_tile_table(strip, 64, 48).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].x, t[0].y, t[0].width, t[0].height, t[0].offset), (0, 0, 64, 48, 512));
    }

    #[test]
    fn tile_table_rejects_bad_layouts() {
        let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
        let strip = strip_of(&arw6_file(&[(0, 0, 64, 48, tile.clone())], 64, 48)).to_vec();
        let mut zero = strip.clone();
        zero[0] = 0;
        assert!(parse_tile_table(&zero, 64, 48).is_err());
        let mut far = strip.clone();
        far[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_tile_table(&far, 64, 48).is_err());
        let mut odd = strip.clone();
        odd[20..24].copy_from_slice(&63u32.to_le_bytes()); // the first tile's y: odd, and no longer covering
        assert!(parse_tile_table(&odd, 64, 48).is_err());
        assert!(parse_tile_table(&strip, 64, 64).is_err()); // does not cover the image
        let two = arw6_file(&[(0, 0, 64, 48, tile.clone()), (0, 0, 64, 48, tile.clone())], 128, 48);
        assert!(parse_tile_table(strip_of(&two), 128, 48).is_err()); // overlapping
        assert!(parse_tile_table(&strip[..7], 64, 48).is_err()); // truncated table
    }

    #[test]
    fn tile_header_and_streams() {
        let (tile, _) = one_tile(64, 48, 3, Qis::REAL);
        let h = parse_tile_header(&tile, 64, 48).unwrap();
        assert_eq!((h.hs, h.vs, h.ntu, h.streams.len()), (64, 24, 2, 13));
        assert_eq!(
            h.streams.iter().map(|s| (s.group, s.comp)).collect::<Vec<_>>(),
            [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (2, 0), (2, 1), (2, 2), (3, 0), (3, 1), (3, 2), (4, 0)]
        );
        let g3 = stream_halves(&tile, &h.streams[9], h.ntu).unwrap();
        assert_eq!(g3.len(), 4);
        assert_eq!(g3[0].qi, [1, 1, 2]);
        let g4 = stream_halves(&tile, &h.streams[12], h.ntu).unwrap();
        assert_eq!(g4[1].qi, [2, 2, 2]);
        assert!(g4[3].start + g4[3].len <= tile.len());
        let mut bad = tile.clone();
        bad[13] = 0xff; // corrupts word 0's BBD bits: any picture-header deviation is Unsupported
        assert!(matches!(parse_tile_header(&bad, 64, 48), Err(RawError::Unsupported(_))));
        assert!(parse_tile_header(&tile, 64, 50).is_err()); // VS != th / 2
    }

    fn put32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn tile_table_validation_cases() {
        let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
        let many = |n: usize| {
            let tiles: Vec<_> = (0..n).map(|i| (64 * i, 0, 64, 48, tile.clone())).collect();
            strip_of(&arw6_file(&tiles, 64 * n, 48)).to_vec()
        };
        let sixteen = many(16);
        assert_eq!(parse_tile_table(&sixteen, 1024, 48).unwrap().len(), 16);
        let mut seventeen = sixteen.clone();
        seventeen[0] = 17;
        assert!(parse_tile_table(&seventeen, 1088, 48).is_err());

        let two = strip_of(&arw6_file(&[(0, 0, 64, 48, tile.clone()), (64, 0, 64, 48, tile.clone())], 128, 48)).to_vec();
        assert_eq!(parse_tile_table(&two, 128, 48).unwrap().len(), 2);
        let mut swapped = two.clone();
        let (a, b) = (two[8..16].to_vec(), two[32..40].to_vec());
        swapped[8..16].copy_from_slice(&b);
        swapped[32..40].copy_from_slice(&a);
        assert!(parse_tile_table(&swapped, 128, 48).is_err());
        let mut at_end = two.clone();
        at_end[32..40].copy_from_slice(&(two.len() as u64).to_le_bytes());
        assert!(parse_tile_table(&at_end, 128, 48).is_err());

        let one = many(1);
        let mut wide = one.clone();
        put32(&mut wide, 24, 72); // w / 2 = 36 is not a multiple of 8
        assert!(parse_tile_table(&wide, 72, 48).is_err());
        let mut huge = one.clone();
        put32(&mut huge, 24, 1 << 20);
        put32(&mut huge, 28, 1 << 20);
        assert!(matches!(parse_tile_table(&huge, 1 << 20, 1 << 20), Err(RawError::Limit(_))));
    }

    #[test]
    fn tile_bytes_span_to_the_next_offset() {
        let (tile, _) = one_tile(64, 48, 0, Qis::ZERO);
        let file = arw6_file(&[(0, 0, 64, 48, tile.clone()), (64, 0, 64, 48, tile.clone())], 128, 48);
        let strip = strip_of(&file);
        let tiles = parse_tile_table(strip, 128, 48).unwrap();
        let (t0, t1) = (tile_bytes(strip, &tiles, 0).unwrap(), tile_bytes(strip, &tiles, 1).unwrap());
        assert_eq!(t0.len(), tiles[1].offset - tiles[0].offset);
        assert_eq!(t0, &tile[..]);
        assert_eq!(t1.as_ptr() as usize + t1.len(), strip.as_ptr() as usize + strip.len());
        assert!(tile_bytes(strip, &tiles, 2).is_none());
    }

    #[test]
    fn tile_header_and_stream_validation_cases() {
        let (tile, _) = one_tile(64, 48, 0, Qis::REAL);
        let h = parse_tile_header(&tile, 64, 48).unwrap();
        let mut two_streams = tile.clone();
        two_streams[3 * 16] = 2;
        assert!(parse_tile_header(&two_streams, 64, 48).is_err());
        let mut big = tile.clone();
        big[3 * 16 + 1..3 * 16 + 4].copy_from_slice(&[0xff; 3]); // stream 0 larger than the tile
        assert!(parse_tile_header(&big, 64, 48).is_err());
        let mut idx = tile.clone();
        idx[8 * 16..8 * 16 + 2].copy_from_slice(&[0xff, 0xff]); // index words beyond the stream
        assert!(parse_tile_header(&idx, 64, 48).is_err());

        let s = &h.streams[9];
        let mut bad_half = tile.clone();
        let idx_start = (s.word + 1) * 16;
        bad_half[idx_start..idx_start + 2].copy_from_slice(&[0xff, 0xff]); // A past the data area
        assert!(stream_halves(&bad_half, s, h.ntu).is_err());
        let outside = StreamRef { index_words: 0xffff, ..s.clone() };
        assert!(stream_halves(&tile, &outside, h.ntu).is_err());
    }

    #[test]
    fn every_stream_of_an_encoded_tile_parses() {
        for s in [0u8, 3] {
            let (tile, spec) = one_tile(64, 48, s, Qis::REAL);
            assert_eq!(tile.len() % 4096, 0);
            let h = parse_tile_header(&tile, 64, 48).unwrap();
            for st in &h.streams {
                assert_eq!(stream_halves(&tile, st, h.ntu).unwrap().len(), 2 * h.ntu);
            }
            // the residual stream's halves decode to the clipped, quantised rows 8n-6+s ..= 8n+1+s
            let g4 = stream_halves(&tile, &h.streams[12], h.ntu).unwrap();
            for (n, half) in g4.iter().enumerate() {
                let bytes = &tile[half.start..half.start + half.len];
                let mut bits = Bits::new(bytes);
                for r in (8 * n as i32 - 6 + s as i32..8 * n as i32 + 2 + s as i32).filter(|&r| (0..24).contains(&r)) {
                    let line = crate::llvc::vld_decode_line(&mut bits, bytes.len() * 8, 32).unwrap();
                    let want: Vec<i32> = spec.res.row(r as usize).unwrap().iter().map(|&v| quantize(v, 2)).collect();
                    assert_eq!(line, want, "s {s} half {n} row {r}");
                }
            }
            // a stream that claims more halves than its index holds is rejected
            assert!(stream_halves(&tile, &h.streams[0], 1000).is_err());
        }
    }

    #[test]
    fn encoder_decodes_its_own_lines() {
        let vals: Vec<i32> = (0..100).map(|i| (i * 37) % 23 - 11).collect();
        let mut w = BitWriter::default();
        vld_encode_line(&vals, 100, &mut w);
        let bytes = w.finish();
        let mut b = Bits::new(&bytes);
        assert_eq!(crate::llvc::vld_decode_line(&mut b, bytes.len() * 8, 100).unwrap(), vals);
    }
}
