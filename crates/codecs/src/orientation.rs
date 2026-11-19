//! The orientation [`crate::read_header`] gives a file, found by seeking to the bytes that hold it rather than reading
//! the file: the first `Exif` APP1 segment before a JPEG's first scan, a PNG's first `eXIf` chunk, a WebP's first
//! `EXIF` chunk, a TIFF's first directory, a PSD's last Exif image resource. Each is parsed as the header reader parses
//! it. A many-gigabyte TIFF or PSD costs a few reads of its first kilobytes.
//!
//! The file is taken as one that has been read before: what the header reader also checks (the pixel data, its size
//! and layout) is not checked again. Where this can't follow the reader (BigTIFF, an orientation stored in an unusual
//! TIFF type, a structure the reader would reject, a block over [`MAX_BLOCK`]), it says so with `Ok(None)` and the
//! caller reads the file whole.

use std::io::{self, BufReader, Read, Seek};

use crate::Format;

/// The most bytes read for one block (an Exif segment or chunk, a TIFF directory, a PSD's head). A larger one is
/// `Ok(None)`: read the file whole instead.
pub const MAX_BLOCK: u64 = 64 << 20;

/// The most structures (JPEG markers, PNG or WebP chunks) walked before giving up with `Ok(None)`.
const MAX_STEPS: u32 = 1 << 22;

/// The Exif orientation (1–8) [`crate::read_header`] would give the file `reader` holds, `len` bytes long, of
/// `format`; 1 where it states none. `Ok(None)`: not found this way (another format, or see the module notes).
/// `Err`: the file couldn't be read.
pub fn header_orientation<R: Read + Seek>(reader: &mut R, len: u64, format: Format) -> io::Result<Option<u16>> {
    let mut at = At { r: BufReader::new(reader), pos: 0, len };
    at.r.rewind()?;
    match format {
        Format::Jpeg => jpeg(&mut at),
        Format::Png => png(&mut at),
        Format::WebP => webp(&mut at),
        Format::Tiff => tiff(&mut at),
        Format::Psd => psd(&mut at),
        _ => Ok(None),
    }
}

/// Positioned reads within a file of `len` bytes, through a buffer that short forward skips don't empty.
struct At<R> {
    r: BufReader<R>,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> At<R> {
    /// Fills `buf` from `at`: `Ok(false)` when that runs past the end of the file.
    fn fill(&mut self, at: u64, buf: &mut [u8]) -> io::Result<bool> {
        let Some(end) = at.checked_add(buf.len() as u64) else { return Ok(false) };
        if end > self.len {
            return Ok(false);
        }
        let (Ok(to), Ok(from)) = (i64::try_from(at), i64::try_from(self.pos)) else { return Ok(false) };
        self.r.seek_relative(to.saturating_sub(from))?;
        self.r.read_exact(buf)?;
        self.pos = end;
        Ok(true)
    }

    /// The `n` bytes at `at`: `None` past the end of the file or over [`MAX_BLOCK`].
    fn block(&mut self, at: u64, n: u64) -> io::Result<Option<Vec<u8>>> {
        if n > MAX_BLOCK {
            return Ok(None);
        }
        let Ok(n) = usize::try_from(n) else { return Ok(None) };
        let mut v = vec![0; n];
        Ok(self.fill(at, &mut v)?.then_some(v))
    }
}

/// As [`crate::jpeg`]'s marker walk (`parse_markers`): garbage between segments is skipped to the next 0xFF, the walk
/// ends at the first scan or EOI, and the first APP1 that starts `Exif\0\0` counts, cut at the end of the file.
fn jpeg<R: Read + Seek>(f: &mut At<R>) -> io::Result<Option<u16>> {
    let mut soi = [0; 2];
    if !f.fill(0, &mut soi)? || soi != [0xff, 0xd8] {
        return Ok(None);
    }
    let mut p: u64 = 2;
    for _ in 0..MAX_STEPS {
        let mut h = [0; 4];
        if !f.fill(p, &mut h)? {
            return Ok(Some(1));
        }
        let [b0, marker, l0, l1] = h;
        if b0 != 0xff || marker == 0xff {
            p = p.saturating_add(1);
            continue;
        }
        match marker {
            0xd8 | 0xd0..=0xd7 | 0x01 => {
                p = p.saturating_add(2);
                continue;
            }
            0xd9 | 0xda => return Ok(Some(1)),
            _ => {}
        }
        let seg_len = u64::from(u16::from_be_bytes([l0, l1]));
        if seg_len < 2 {
            return Ok(None);
        }
        let start = p.saturating_add(4);
        let declared_end = p.saturating_add(2).saturating_add(seg_len);
        if marker == 0xe1 {
            let end = declared_end.min(f.len);
            let Some(seg) = f.block(start, end.saturating_sub(start))? else { return Ok(None) };
            if let Some(exif) = seg.strip_prefix(b"Exif\0\0") {
                return Ok(Some(crate::exif::summarize(exif).orientation.unwrap_or(1)));
            }
        }
        p = declared_end;
    }
    Ok(None)
}

/// As `craft_image_codecs::png_probe::header`: the first `eXIf` chunk anywhere, the walk ending at IEND or at a damaged
/// chunk after the image data.
fn png<R: Read + Seek>(f: &mut At<R>) -> io::Result<Option<u16>> {
    let mut sig = [0; 16];
    if !f.fill(0, &mut sig)? || &sig[..8] != b"\x89PNG\r\n\x1a\n" || sig[8..12] != 13u32.to_be_bytes() || &sig[12..] != b"IHDR" {
        return Ok(None);
    }
    let (mut p, mut idat, mut after_idat, mut exif) = (8u64, false, false, None);
    for _ in 0..MAX_STEPS {
        let mut h = [0; 8];
        if !f.fill(p, &mut h)? {
            // too short for a chunk header here: the reader's walk ends too
            return finish_png(f, idat, after_idat, exif);
        }
        let [l0, l1, l2, l3, k0, k1, k2, k3] = h;
        let (n, kind) = (u32::from_be_bytes([l0, l1, l2, l3]), [k0, k1, k2, k3]);
        if idat && &kind != b"IDAT" {
            after_idat = true;
        }
        if &kind == b"IEND" {
            return finish_png(f, idat, after_idat, exif);
        }
        let start = p.saturating_add(8);
        let end = start.saturating_add(u64::from(n));
        if n > i32::MAX as u32 || end > f.len {
            if after_idat {
                return finish_png(f, idat, after_idat, exif);
            }
            return Ok(None);
        }
        match &kind {
            b"IDAT" if !after_idat => idat = true,
            b"eXIf" if exif.is_none() => exif = Some((start, u64::from(n))),
            _ => {}
        }
        p = end.saturating_add(4);
    }
    Ok(None)
}

fn finish_png<R: Read + Seek>(f: &mut At<R>, idat: bool, after_idat: bool, exif: Option<(u64, u64)>) -> io::Result<Option<u16>> {
    if !idat || !after_idat {
        return Ok(None);
    }
    let Some((at, n)) = exif else { return Ok(Some(1)) };
    let Some(data) = f.block(at, n)? else { return Ok(None) };
    Ok(Some(craft_metadata::compat_orientation::orientation(data.strip_prefix(b"Exif\0\0").unwrap_or(&data))))
}

/// As [`crate::webp`]'s header: the first `EXIF` chunk that fits in the file.
fn webp<R: Read + Seek>(f: &mut At<R>) -> io::Result<Option<u16>> {
    let (mut p, mut has_image, mut exif) = (12u64, false, None);
    for _ in 0..MAX_STEPS {
        let mut h = [0; 8];
        if !f.fill(p, &mut h)? {
            if !has_image {
                return Ok(None);
            }
            let Some((at, n)) = exif else { return Ok(Some(1)) };
            let Some(e) = f.block(at, n)? else { return Ok(None) };
            return Ok(Some(crate::exif::summarize(e.strip_prefix(b"Exif\0\0").unwrap_or(&e)).orientation.unwrap_or(1)));
        }
        let [i0, i1, i2, i3, s0, s1, s2, s3] = h;
        let (id, size) = ([i0, i1, i2, i3], u64::from(u32::from_le_bytes([s0, s1, s2, s3])));
        let end = p.saturating_add(8).saturating_add(size);
        if matches!(&id, b"VP8 " | b"VP8L" | b"ANMF") {
            if end > f.len {
                return Ok(None);
            }
            has_image = true;
        } else if &id == b"EXIF" && end <= f.len && exif.is_none() {
            exif = Some((p.saturating_add(8), size));
        }
        p = end.saturating_add(size & 1);
    }
    Ok(None)
}

/// As `craft_image_codecs::tiff_probe::header`: the first directory's Orientation tag, 1 when it has none or it
/// isn't 1–8. Classic TIFF only, the tag stored as one BYTE, SHORT or LONG.
fn tiff<R: Read + Seek>(f: &mut At<R>) -> io::Result<Option<u16>> {
    let mut h = [0; 8];
    if !f.fill(0, &mut h)? {
        return Ok(None);
    }
    let le = match &h[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Ok(None),
    };
    let u16_of = |b: [u8; 2]| if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) };
    let u32_of = |b: [u8; 4]| if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
    let [_, _, m0, m1, o0, o1, o2, o3] = h;
    if u16_of([m0, m1]) != 42 {
        return Ok(None);
    }
    let ifd = u64::from(u32_of([o0, o1, o2, o3]));
    let mut count = [0; 2];
    if ifd < 8 || !f.fill(ifd, &mut count)? {
        return Ok(None);
    }
    let Some(entries) = f.block(ifd.saturating_add(2), 12 * u64::from(u16_of(count)))? else { return Ok(None) };
    let mut found = None;
    for e in entries.chunks_exact(12) {
        let [t0, t1, y0, y1, c0, c1, c2, c3, v0, v1, v2, v3] = <[u8; 12]>::try_from(e).unwrap_or_default();
        if u16_of([t0, t1]) != 274 {
            continue;
        }
        if found.is_some() || u32_of([c0, c1, c2, c3]) != 1 {
            return Ok(None);
        }
        found = Some(match u16_of([y0, y1]) {
            1 => u32::from(v0),
            3 => u32::from(u16_of([v0, v1])),
            4 => u32_of([v0, v1, v2, v3]),
            _ => return Ok(None),
        });
    }
    Ok(Some(found.and_then(|v| u16::try_from(v).ok()).filter(|o| (1..=8).contains(o)).unwrap_or(1)))
}

/// As [`crate::psd`]'s header: the head (header, colour mode data, image resources) parsed by `craft_psd`, the last
/// Exif resource (1058) counting.
fn psd<R: Read + Seek>(f: &mut At<R>) -> io::Result<Option<u16>> {
    let mut h = [0; 30];
    if !f.fill(0, &mut h)? {
        return Ok(None);
    }
    let [.., c0, c1, c2, c3] = h;
    let res_len_at = 30u64.saturating_add(u64::from(u32::from_be_bytes([c0, c1, c2, c3])));
    let mut r = [0; 4];
    if !f.fill(res_len_at, &mut r)? {
        return Ok(None);
    }
    let head_len = res_len_at.saturating_add(4).saturating_add(u64::from(u32::from_be_bytes(r)));
    let Some(head) = f.block(0, head_len)? else { return Ok(None) };
    let resources = craft_psd::Head::from_bytes(&head).map(|head| head.resources).unwrap_or_default();
    let exif = resources.iter().rev().find(|r| r.id == 1058).map(|r| r.data.as_slice());
    Ok(Some(exif.map(|d| crate::exif::summarize(d.strip_prefix(b"Exif\0\0").unwrap_or(d))).unwrap_or_default().orientation.unwrap_or(1)))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, SeekFrom};

    use super::header_orientation;
    use crate::{EncodeImage, EncodeMeta, Format, TiffCompression, read_header};

    /// Counts the bytes read from the file, so a test can see how little of it was.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        read: u64,
    }
    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read += n as u64;
            Ok(n)
        }
    }
    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// The seek probe's answer and the bytes it read; `None` when it can't tell.
    fn probe(bytes: &[u8]) -> (Option<u16>, u64) {
        let format = crate::sniff(bytes).unwrap();
        let mut c = Counting { inner: Cursor::new(bytes.to_vec()), read: 0 };
        (header_orientation(&mut c, bytes.len() as u64, format).unwrap(), c.read)
    }

    /// The probe gives what `read_header` gives the whole file, and finds it by seeking.
    #[track_caller]
    fn same(name: &str, bytes: &[u8]) -> u16 {
        let whole = read_header(bytes).unwrap_or_else(|e| panic!("{name}: {e}")).orientation;
        assert_eq!(probe(bytes).0, Some(whole), "{name}");
        whole
    }

    fn encode(format: Format, w: usize, h: usize, exif: Option<&[u8]>, xmp: Option<&str>) -> Vec<u8> {
        let img = lightcraft_raster::Rgba8::from_fn(w, h, |x, y| {
            // noise: a large file stays large in every format
            let n = (x as u32).wrapping_mul(2_654_435_761) ^ (y as u32).wrapping_mul(40_503).rotate_left(13);
            let [a, b, c, _] = n.wrapping_mul(2_246_822_519).to_le_bytes();
            [a, b, c, 255]
        });
        let img = EncodeImage::rgba8(&img);
        let meta = EncodeMeta { exif, xmp, ..Default::default() };
        match format {
            Format::Jpeg => crate::encode_jpeg(&img, 90, Default::default(), &meta),
            Format::Png => crate::encode_png(&img, &meta),
            Format::Tiff => crate::encode_tiff(&img, TiffCompression::None, &meta),
            Format::Psd => crate::encode_psd(&img, &meta),
            Format::WebP => crate::encode_webp_lossless(&img, &meta),
            other => panic!("no encoder here for {other:?}"),
        }
        .unwrap()
    }

    const FORMATS: [Format; 5] = [Format::Jpeg, Format::Png, Format::Tiff, Format::Psd, Format::WebP];

    #[test]
    fn every_orientation_of_every_format_is_the_headers() {
        for format in FORMATS {
            let mut seen = Vec::new();
            for o in 1..=8u16 {
                let exif = crate::exif::minimal_exif(o);
                seen.push(same(&format!("{format:?} {o}"), &encode(format, 16, 12, Some(&exif), None)));
            }
            assert_eq!(same(&format!("{format:?} none"), &encode(format, 16, 12, None, None)), 1);
            // an orientation only XMP states is not the header's
            let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:Orientation="6"/></rdf:RDF></x:xmpmeta>"#;
            assert_eq!(same(&format!("{format:?} xmp"), &encode(format, 16, 12, None, Some(xmp))), 1);
            // the TIFF encoder writes no Orientation tag: `tiff_with_orientation` covers TIFF's eight
            let want: Vec<u16> = if format == Format::Tiff { vec![1; 8] } else { (1..=8).collect() };
            assert_eq!(seen, want, "{format:?}: the fixtures carry their orientation");
        }
    }

    #[test]
    fn a_large_file_is_read_only_where_its_orientation_is() {
        let exif = crate::exif::minimal_exif(6);
        for format in FORMATS {
            let bytes = encode(format, 1024, 1024, Some(&exif), None);
            assert!(bytes.len() > 1 << 20, "{format:?}: {}", bytes.len());
            same(&format!("{format:?} large"), &bytes);
            let (_, read) = probe(&bytes);
            assert!(read <= 64 << 10, "{format:?}: read {read} of {} bytes", bytes.len());
        }
    }

    /// JPEG segments the walk must step over, or stop at, as the header reader does.
    #[test]
    fn jpeg_segments_are_walked_as_the_header_reader_walks_them() {
        let six = [&b"Exif\0\0"[..], &crate::exif::minimal_exif(6)].concat();
        let plain = [&b"Exif\0\0"[..], &crate::exif::minimal_exif(1)].concat();
        let base = encode(Format::Jpeg, 16, 12, None, None);
        // `extra` inserted right after SOI
        let with = |extra: &[u8]| [&base[..2], extra, &base[2..]].concat();
        let seg = |marker: u8, payload: &[u8]| [&[0xff, marker][..], &(payload.len() as u16 + 2).to_be_bytes(), payload].concat();
        let big: Vec<u8> = (0..17).flat_map(|_| seg(0xef, &vec![0; 0xfffd])).collect();
        let xmp = [&b"http://ns.adobe.com/xap/1.0/\0"[..], b"<x:xmpmeta/>"].concat();
        for (name, bytes, want) in [
            ("past the first MiB", with(&[big.clone(), seg(0xe1, &six)].concat()), 6),
            ("garbage and fill bytes", with(&[&seg(0xee, b"x")[..], &[0x00, 0x12, 0xff, 0xff], &seg(0xe1, &six)].concat()), 6),
            ("XMP first", with(&[seg(0xe1, &xmp), seg(0xe1, &six)].concat()), 6),
            ("the first Exif counts", with(&[seg(0xe1, &plain), seg(0xe1, &six)].concat()), 1),
            // the walk ends at the first scan: an Exif segment after the image isn't the header's
            ("after the image", [&base[..], &seg(0xe1, &six)].concat(), 1),
        ] {
            assert_eq!(same(name, &bytes), want, "{name}");
        }
        // nor one after the first scan header (the header reader stops there, before any EOI)
        let after_sos = [&[0xff, 0xd8][..], &seg(0xda, &[1, 1, 0, 0, 0, 0]), &seg(0xe1, &six)].concat();
        assert_eq!(probe(&after_sos).0, Some(1), "after SOS");
        // reading the Exif past the first MiB costs a buffer per segment header, not the segments
        let far = with(&[big, seg(0xe1, &six)].concat());
        let (_, read) = probe(&far);
        assert!(read < far.len() as u64 / 4, "read {read} of {}", far.len());
    }

    /// PNG and WebP keep chunks after the image data; the first Exif chunk counts.
    #[test]
    fn late_and_repeated_exif_chunks_are_the_header_readers() {
        let six = crate::exif::minimal_exif(6);
        let three = crate::exif::minimal_exif(3);
        // a PNG with `chunks` (kind, data) put before IEND; the CRC isn't checked by either reader
        let png = |chunks: &[(&[u8; 4], &[u8])]| {
            let base = encode(Format::Png, 16, 12, None, None);
            let iend = base.len() - 12;
            let mut out = base[..iend].to_vec();
            for (kind, data) in chunks {
                out.extend_from_slice(&(data.len() as u32).to_be_bytes());
                out.extend_from_slice(*kind);
                out.extend_from_slice(data);
                out.extend_from_slice(&[0; 4]);
            }
            out.extend_from_slice(&base[iend..]);
            out
        };
        assert_eq!(same("png late", &png(&[(b"eXIf", &six)])), 6);
        assert_eq!(same("png two", &png(&[(b"eXIf", &three), (b"eXIf", &six)])), 3);
        // after an early eXIf (the encoder's), a late one doesn't count
        let early = encode(Format::Png, 16, 12, Some(&three), None);
        let iend = early.len() - 12;
        let mut both = early[..iend].to_vec();
        both.extend_from_slice(&(six.len() as u32).to_be_bytes());
        both.extend_from_slice(b"eXIf");
        both.extend_from_slice(&six);
        both.extend_from_slice(&[0; 4]);
        both.extend_from_slice(&early[iend..]);
        assert_eq!(same("png early and late", &both), 3);

        // a WebP with a second EXIF chunk after the encoder's
        let webp = encode(Format::WebP, 16, 12, Some(&three), None);
        let chunk = [&b"EXIF"[..], &(six.len() as u32).to_le_bytes(), &six, &vec![0; six.len() % 2]].concat();
        let mut two = [&webp[..], &chunk].concat();
        let riff = (two.len() - 8) as u32;
        two[4..8].copy_from_slice(&riff.to_le_bytes());
        assert_eq!(same("webp two", &two), 3);
    }

    /// Where the probe can't follow the header reader it says so, rather than guessing.
    #[test]
    fn what_the_probe_cant_follow_is_none() {
        let tiff = encode(Format::Tiff, 16, 12, None, None);
        let mut big = tiff.clone();
        big[2] = 43; // BigTIFF magic (little-endian)
        assert_eq!(probe(&big).0, None, "BigTIFF");
        let mut c = Cursor::new(vec![0xff]);
        assert_eq!(header_orientation(&mut c, 1, Format::Jpeg).unwrap(), None, "too short for SOI");
        let mut c = Cursor::new(Vec::new());
        assert_eq!(header_orientation(&mut c, 0, Format::Gif).unwrap(), None, "another format");
        let _ = tiff;
    }

    /// `tiff` (little-endian, as the encoder writes it) with its first directory copied to the end of the file and
    /// `added` (tag, type, count, 4 value bytes) entries put in it, tag order kept.
    fn tiff_with(tiff: &[u8], added: &[(u16, u16, u32, [u8; 4])]) -> Vec<u8> {
        assert_eq!(&tiff[..4], b"II*\0");
        let le16 = |at: usize| u16::from_le_bytes([tiff[at], tiff[at + 1]]);
        let ifd = u32::from_le_bytes(tiff[4..8].try_into().unwrap()) as usize;
        let n = usize::from(le16(ifd));
        let mut entries: Vec<[u8; 12]> = (0..n).map(|i| tiff[ifd + 2 + 12 * i..ifd + 14 + 12 * i].try_into().unwrap()).collect();
        for (tag, typ, count, value) in added {
            let mut e = [0; 12];
            e[..2].copy_from_slice(&tag.to_le_bytes());
            e[2..4].copy_from_slice(&typ.to_le_bytes());
            e[4..8].copy_from_slice(&count.to_le_bytes());
            e[8..].copy_from_slice(value);
            entries.push(e);
        }
        entries.sort_by_key(|e| u16::from_le_bytes([e[0], e[1]]));
        let mut out = tiff.to_vec();
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let at = out.len() as u32;
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        entries.iter().for_each(|e| out.extend_from_slice(e));
        out.extend_from_slice(&[0; 4]);
        out[4..8].copy_from_slice(&at.to_le_bytes());
        out
    }

    #[test]
    fn a_tiffs_orientation_tag_is_the_headers() {
        let base = encode(Format::Tiff, 16, 12, None, None);
        let short = |v: u16| [v.to_le_bytes()[0], v.to_le_bytes()[1], 0, 0];
        for o in 1..=8u16 {
            assert_eq!(same(&format!("SHORT {o}"), &tiff_with(&base, &[(274, 3, 1, short(o))])), o);
            assert_eq!(same(&format!("LONG {o}"), &tiff_with(&base, &[(274, 4, 1, u32::from(o).to_le_bytes())])), o);
            assert_eq!(same(&format!("BYTE {o}"), &tiff_with(&base, &[(274, 1, 1, [o as u8, 0, 0, 0])])), o);
        }
        // out of range: 1, as the header reader takes it
        for v in [0, 9, 0xffff] {
            assert_eq!(same(&format!("SHORT {v}"), &tiff_with(&base, &[(274, 3, 1, short(v))])), 1);
        }
        assert_eq!(same("LONG past u16", &tiff_with(&base, &[(274, 4, 1, 0x1_0006u32.to_le_bytes())])), 1);
        // the large-file test's TIFF, with its orientation tag: still a few reads
        let big = tiff_with(&encode(Format::Tiff, 1024, 1024, None, None), &[(274, 3, 1, short(6))]);
        assert_eq!(same("large", &big), 6);
        assert!(probe(&big).1 <= 64 << 10);
        // what the probe doesn't follow: a signed type, two tags, a count of two
        for (name, added) in [
            ("SSHORT", vec![(274, 8, 1, short(6))]),
            ("two tags", vec![(274, 3, 1, short(6)), (274, 3, 1, short(3))]),
            ("two values", vec![(274, 3, 2, [6, 0, 3, 0])]),
        ] {
            assert_eq!(probe(&tiff_with(&base, &added)).0, None, "{name}");
        }
    }
}
