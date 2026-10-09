//! Embedded preview extraction: the largest baseline/progressive JPEG stored in a TIFF-based raw (IFD strips
//! with JPEG compression, `JPEGInterchangeFormat` pointers in any IFD, Nikon/others' maker-note preview IFDs),
//! or a DNG 1.7 JPEG XL preview IFD stored as a single tile/strip (a standalone `.jxl` file, as the spec
//! recommends for previews).

use dac_tiff::image::chunk_bytes;
use dac_tiff::tags as t;
use dac_tiff::{Ifd, Tiff, makernote};

/// Colour space of an embedded JPEG when the enclosing raw supplies it instead of the JPEG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewColorSpace {
    Srgb,
    AdobeRgb,
}

/// Nikon maker-note ColorSpace (0x001e: 1 = sRGB, 2 = Adobe RGB), per Nikon tag documentation.
/// This is a fallback only: a JPEG's own ICC/EXIF colour declaration takes precedence.
pub fn embedded_preview_color_space(bytes: &[u8]) -> Option<PreviewColorSpace> {
    let tiff = Tiff::parse(bytes).ok()?;
    let make = tiff.ifds.first()?.string(t::MAKE)?;
    if !make.to_ascii_uppercase().starts_with("NIKON") {
        return None;
    }
    let e = tiff.exif()?.get(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make)?;
    match mn.ifd.u16(0x001e)? {
        1 => Some(PreviewColorSpace::Srgb),
        2 => Some(PreviewColorSpace::AdobeRgb),
        _ => None,
    }
}

/// Whether `b` looks like a displayable (DCT) JPEG: SOI, and the first SOF marker is not lossless.
fn is_dct_jpeg(b: &[u8]) -> bool {
    if b.len() < 4 || b[0] != 0xff || b[1] != 0xd8 {
        return false;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xff {
            return false;
        }
        let m = b[i + 1];
        if m == 0xff {
            i += 1;
            continue;
        }
        match m {
            0xc0..=0xc2 => return true,
            0xc3 | 0xc5..=0xc7 | 0xcb | 0xcd..=0xcf => return false,
            0xda | 0xd9 => return false,
            _ => {}
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        i += 2 + len;
    }
    false
}

/// Whether `b` is a JPEG XL file: a bare codestream or the ISO-BMFF container's signature box.
fn is_jxl(b: &[u8]) -> bool {
    b.starts_with(&[0xff, 0x0a]) || b.starts_with(&[0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a])
}

fn candidates<'a>(data: &'a [u8], ifd: &Ifd, base: u64, out: &mut Vec<&'a [u8]>) {
    // whole JPEG files stored as an undefined-type tag value (e.g. Panasonic `JpgFromRaw` 0x002e)
    for e in &ifd.entries {
        if matches!(e.value, dac_tiff::Value::Undefined(_))
            && e.count() > 1024
            && let Some(s) = data.get(e.offset as usize..(e.offset.saturating_add(e.count() as u64) as usize).min(data.len()))
            && s.starts_with(&[0xff, 0xd8])
        {
            out.push(s);
        }
    }
    if let (Some(off), Some(len)) = (ifd.u64(t::JPEG_INTERCHANGE_FORMAT), ifd.u64(t::JPEG_INTERCHANGE_FORMAT_LENGTH)) {
        let off = off.saturating_add(base);
        if let Some(s) = data.get(off as usize..(off.saturating_add(len) as usize).min(data.len())) {
            out.push(s);
        }
    }
    if matches!(ifd.u16(t::COMPRESSION), Some(6) | Some(7) | Some(34892))
        && let Ok(info) = ifd.image()
    {
        let chunks = info.chunks(data.len() as u64);
        if chunks.len() == 1
            && let Some(s) = chunk_bytes(data, &chunks[0])
        {
            out.push(s);
        }
    }
    // a rendered (RGB or grey, never CFA/LinearRaw/mask) JPEG XL preview in one chunk
    if ifd.u16(t::COMPRESSION) == Some(t::compression::JPEG_XL)
        && matches!(ifd.u16(t::PHOTOMETRIC), Some(t::photometric::BLACK_IS_ZERO | t::photometric::RGB))
        && let Ok(info) = ifd.image()
        && let [chunk] = info.chunks(data.len() as u64).as_slice()
        && let Some(s) = chunk_bytes(data, chunk)
        && is_jxl(s)
    {
        out.push(s);
    }
}

/// The largest embedded preview, if any: a JPEG, or (DNG 1.7) a JPEG XL file — both decode with
/// `dac_codecs::decode`.
pub fn embedded_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        let j = crate::vendor::raf::header(bytes).ok()?.jpeg?;
        return is_dct_jpeg(j).then(|| trim_eoi(j).to_vec());
    }
    let format = crate::probe(bytes);
    if format == Some(crate::RawFormat::Cr3) {
        return cr3_preview(bytes).map(|j| trim_eoi(j).to_vec());
    }
    if matches!(format, Some(crate::RawFormat::Crw | crate::RawFormat::Mrw | crate::RawFormat::X3f)) {
        return scan_for_jpeg(bytes);
    }
    let tiff = Tiff::parse(bytes).ok()?;
    let mut found: Vec<&[u8]> = Vec::new();
    for ifd in tiff.all_ifds() {
        candidates(bytes, ifd, 0, &mut found);
    }
    // maker-note preview IFDs (e.g. Nikon PreviewIFD 0x0011 holds JPEGInterchangeFormat relative to the note base)
    if let Some(exif) = tiff.exif()
        && let Some(e) = exif.get(t::MAKER_NOTE)
    {
        let make = tiff.find(t::MAKE).and_then(|e| e.value.as_str()).unwrap_or_default();
        if let Some(mn) = makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make) {
            candidates(bytes, &mn.ifd, mn.base, &mut found);
            if let Some(off) = mn.ifd.u64(0x0011)
                && let Ok((pifd, _)) = dac_tiff::parse_ifd_at(bytes, mn.base + off, mn.order, mn.base, false, &Default::default())
            {
                candidates(bytes, &pifd, mn.base, &mut found);
            }
        }
    }
    // Olympus: CameraSettings preview
    if let Some(p) = crate::vendor::orf::preview(bytes) {
        found.push(p);
    }
    let best = found
        .into_iter()
        .filter(|s| is_dct_jpeg(s) || is_jxl(s))
        .max_by_key(|s| s.len())
        .map(|s| if is_jxl(s) { s.to_vec() } else { trim_eoi(s).to_vec() });
    // a TIFF raw we can't decode whose preview no tag points to (Leaf MOS, Epson ERF)
    best.or_else(|| format.filter(|f| !f.is_supported()).and_then(|_| scan_for_jpeg(bytes)))
}

/// The end of the DCT JPEG whose SOI is at `soi` (exclusive index after its EOI) and its component count,
/// walking the marker segments and the entropy-coded data. `None` when it is not a complete, displayable
/// (baseline or progressive) JPEG, so a stray `D8 FF` in other data is rejected. `*reach` is left at
/// the furthest byte looked at (what the walk cost; see [`scan_for_jpeg`]).
fn jpeg_extent(b: &[u8], soi: usize, reach: &mut usize) -> Option<(usize, u8)> {
    let mut i = soi + 2;
    let r = walk_jpeg(b, &mut i);
    *reach = i;
    r
}

fn walk_jpeg(b: &[u8], i: &mut usize) -> Option<(usize, u8)> {
    let mut components = None;
    for _ in 0..4096 {
        if *b.get(*i)? != 0xff {
            return None;
        }
        while *b.get(*i)? == 0xff {
            *i += 1;
        }
        let m = *b.get(*i)?;
        *i += 1;
        match m {
            0xd9 => return components.map(|c| (*i, c)),
            0x01 | 0xd0..=0xd8 => continue,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*b.get(*i)?, *b.get(*i + 1)?]));
        if len < 2 {
            return None;
        }
        match m {
            0xc0..=0xc2 => {
                // precision, height, width, component count
                let h = u16::from_be_bytes([*b.get(*i + 3)?, *b.get(*i + 4)?]);
                let w = u16::from_be_bytes([*b.get(*i + 5)?, *b.get(*i + 6)?]);
                if h == 0 || w == 0 || components.is_some() {
                    return None;
                }
                components = Some(*b.get(*i + 7)?);
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None, // lossless, arithmetic, hierarchical
            _ => {}
        }
        *i += len;
        if m == 0xda {
            // entropy-coded data up to the next marker that is not a stuffed 0xff or a restart
            loop {
                *i += b.get(*i..)?.iter().position(|&x| x == 0xff)?;
                match *b.get(*i + 1)? {
                    0x00 | 0xd0..=0xd7 => *i += 2,
                    0xff => *i += 1,
                    _ => break,
                }
            }
        }
    }
    None
}

/// Last resort for the containers whose structure is not walked (Canon CRW, Minolta MRW, Sigma X3F, TIFF raws
/// with private blocks such as Leaf MOS or Epson ERF): the largest complete colour DCT JPEG stored as a plain
/// byte run in the file. A one-component JPEG is skipped (a Canon PowerShot CRW stores its sensor mosaic as one).
///
/// Evidence: in the Minolta and Epson files the first byte of the stored preview is not `ff` (it is `00`, `02` or
/// `ee`) while the rest is a regular JPEG, so a run is found by its `d8 ff` and starts one byte earlier,
/// with that byte restored to `ff`.
///
/// A crafted file can hold millions of JPEG-like starts whose walks each run to its end: the walks
/// share a budget of eight times the file's size, so the scan stays linear (a real file's candidates
/// fail within a few bytes, or are the preview itself).
fn scan_for_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    let mut best: Option<(usize, usize)> = None;
    let mut at = 1;
    let mut budget = data.len().saturating_mul(8).max(1 << 20);
    while let Some(off) = data.get(at..).and_then(|s| s.iter().position(|&b| b == 0xd8)) {
        let d8 = at + off;
        at = d8 + 1;
        if data.get(d8 + 1) != Some(&0xff) || !matches!(data.get(d8 + 2), Some(0xc0..=0xc4 | 0xdb | 0xe0..=0xef | 0xfe)) {
            continue;
        }
        let start = d8 - 1; // `at` starts at 1, so d8 >= 1
        let mut reach = start;
        let found = jpeg_extent(data, start, &mut reach);
        budget = budget.saturating_sub(reach.saturating_sub(start));
        if budget == 0 {
            break;
        }
        if let Some((end, 3)) = found {
            if best.is_none_or(|(s, e)| end - start > e - s) {
                best = Some((start, end));
            }
            at = end;
        }
    }
    let (start, end) = best?;
    let mut jpeg = data.get(start..end)?.to_vec();
    if let Some(first) = jpeg.first_mut() {
        *first = 0xff;
    }
    Some(jpeg)
}

/// Canon CR3: the full-size JPEG track (see [`dac_meta::cr3`]), else the `PRVW` / `THMB` boxes.
fn cr3_preview(bytes: &[u8]) -> Option<&[u8]> {
    let full =
        dac_meta::cr3::parse_cr3(bytes).and_then(|c| c.tracks.iter().find(|t| t.kind == dac_meta::cr3::Cr3TrackKind::Jpeg).and_then(|t| t.data));
    if let Some(j) = full.and_then(|(at, len)| bytes.get(at..at.checked_add(len)?)).filter(|j| is_dct_jpeg(j)) {
        return Some(j);
    }
    cr3_preview_boxes(bytes)
}

/// Canon CR3 (ISO base media file): the `PRVW` box (Laurent Clévy's CR3 notes; layout confirmed on a CC0
/// sample) is `u32 size, "PRVW", u32 0, u16 ?, u16 width, u16 height, u16 ?, u32 jpeg length, JPEG`; the smaller
/// `THMB` box has the same shape. Returns the larger valid one.
fn cr3_preview_boxes(bytes: &[u8]) -> Option<&[u8]> {
    let mut best: Option<&[u8]> = None;
    for tag in [b"PRVW", b"THMB"] {
        let mut from = 0;
        while let Some(i) = bytes.get(from..).and_then(|s| s.windows(4).position(|w| w == tag)).map(|p| p + from) {
            from = i + 4;
            let Some(start) = i.checked_sub(4) else { continue };
            let be32 = |at: usize| bytes.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
            let (Some(size), Some(len)) = (be32(start), be32(start + 20)) else { continue };
            let j = start + 24;
            if len < 4 || j + len > start + size.max(24) || j + len > bytes.len() {
                continue;
            }
            let jpeg = &bytes[j..j + len];
            if is_dct_jpeg(jpeg) {
                if best.is_none_or(|b| b.len() < jpeg.len()) {
                    best = Some(jpeg);
                }
                break;
            }
        }
    }
    best
}

/// Trim trailing garbage after the last EOI when a stored length over-reports.
fn trim_eoi(s: &[u8]) -> &[u8] {
    let end = s.windows(2).rposition(|w| w == [0xff, 0xd9]).map(|p| p + 2).unwrap_or(s.len());
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use dac_tiff::{IfdBuilder, ImageData, TiffWriter, Value};

    fn fake_jpeg(n: usize) -> Vec<u8> {
        let mut j = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 8, 0, 1, 0, 1, 1, 1, 0x11, 0];
        j.extend(std::iter::repeat_n(0x55u8, n));
        j.extend_from_slice(&[0xff, 0xd9]);
        j
    }

    #[test]
    fn picks_largest_dct_jpeg() {
        let small = fake_jpeg(10);
        let big = fake_jpeg(500);
        let lossless = crate::ljpeg::encode(&[1u16; 64], 8, 8, 1, 12, 1, 0);
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::COMPRESSION, Value::Short(vec![6]));
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![1]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![1]));
        ifd0.set_image(ImageData::Strips { rows_per_strip: 1, strips: vec![small.clone()] });
        let mut raw = IfdBuilder::new();
        raw.set(t::COMPRESSION, Value::Short(vec![7]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![8]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![8]));
        raw.set_image(ImageData::Strips { rows_per_strip: 8, strips: vec![lossless] });
        ifd0.add_sub_ifd(raw);
        let mut sub = IfdBuilder::new();
        sub.set(t::COMPRESSION, Value::Short(vec![7]));
        sub.set(t::IMAGE_WIDTH, Value::Long(vec![2]));
        sub.set(t::IMAGE_LENGTH, Value::Long(vec![2]));
        sub.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        sub.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![big.clone()] });
        ifd0.add_sub_ifd(sub);
        let bytes = TiffWriter::default().write(&[ifd0]).unwrap();
        assert_eq!(embedded_preview(&bytes).unwrap(), big);
        assert!(embedded_preview(b"nope").is_none());
        // CR3: a PRVW box after the ftyp
        let mut cr3 = b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom".to_vec();
        let j = fake_jpeg(300);
        cr3.extend_from_slice(&((24 + j.len()) as u32).to_be_bytes());
        cr3.extend_from_slice(b"PRVW\0\0\0\0\0\x01\x06\x54\x04\x38\0\x01");
        cr3.extend_from_slice(&(j.len() as u32).to_be_bytes());
        cr3.extend_from_slice(&j);
        assert_eq!(embedded_preview(&cr3).unwrap(), j);
        for n in 0..cr3.len() {
            let _ = embedded_preview(&cr3[..n]);
        }
    }

    /// CR3 with a full-size JPEG track: that JPEG wins over the smaller `PRVW` box.
    #[test]
    fn cr3_prefers_the_full_size_jpeg_track() {
        let bx = |kind: &[u8; 4], body: &[u8]| -> Vec<u8> { [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat() };
        let full = |kind: &[u8; 4], body: &[u8]| bx(kind, &[&[0u8; 4][..], body].concat());
        let (small, big) = (fake_jpeg(300), fake_jpeg(3000));
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        let mut prvw = b"\0\0\0\0\0\x01\x06\x54\x04\x38\0\x01".to_vec();
        prvw.extend_from_slice(&(small.len() as u32).to_be_bytes());
        prvw.extend_from_slice(&small);
        file.extend(bx(b"PRVW", &prvw));
        // CRAW sample entry: 82 bytes, then a JPEG child box; one sample at `at`
        let mut craw = vec![0u8; 82];
        craw.extend(bx(b"JPEG", &[0; 4]));
        let at = 2048u64;
        let stsd = full(b"stsd", &[&1u32.to_be_bytes()[..], &bx(b"CRAW", &craw)].concat());
        let stsz = full(b"stsz", &[0u32.to_be_bytes(), 1u32.to_be_bytes(), (big.len() as u32).to_be_bytes()].concat());
        let co64 = full(b"co64", &[&1u32.to_be_bytes()[..], &at.to_be_bytes()].concat());
        let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &[stsd, stsz, co64].concat()))));
        file.extend(bx(b"moov", &trak));
        file.resize(at as usize, 0);
        file.extend_from_slice(&big);
        assert_eq!(embedded_preview(&file).unwrap(), big);
        // a track pointing past the end falls back to PRVW
        file.truncate(at as usize + 100);
        assert_eq!(embedded_preview(&file).unwrap(), small);
    }

    /// A structurally complete DCT JPEG (no real image data): `nc` components, `n` entropy bytes.
    fn scan_jpeg(nc: u8, n: usize) -> Vec<u8> {
        let mut j = vec![0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00];
        j.extend(std::iter::repeat_n(8u8, 64));
        j.extend_from_slice(&[0xff, 0xc0, 0x00, 8 + 3 * nc, 8, 0, 16, 0, 24, nc]);
        for c in 0..nc {
            j.extend_from_slice(&[c + 1, 0x11, 0]);
        }
        j.extend_from_slice(&[0xff, 0xda, 0x00, 6 + 2 * nc, nc]);
        for c in 0..nc {
            j.extend_from_slice(&[c + 1, 0]);
        }
        j.extend_from_slice(&[0, 63, 0]);
        j.extend(std::iter::repeat_n(0x55u8, n));
        j.extend_from_slice(&[0xff, 0x00, 0x12, 0xff, 0xd0, 0x34, 0xff, 0xd9]);
        j
    }

    /// A crafted container full of JPEG-like starts (each walk running on through the ones after it)
    /// is scanned in linear time: the walks share a budget (without it this 40 MB file takes about a minute).
    #[test]
    fn a_scan_through_many_fake_jpegs_stays_linear() {
        let mut unit = vec![0u8, 0xd8, 0xff, 0xdb, 0x00, 0x02, 0xff, 0xda, 0x00, 0x02];
        unit.extend(std::iter::repeat_n(0x55u8, 190));
        let mut f = b"\0MRM\0\x01\0\0".to_vec();
        for _ in 0..200_000 {
            f.extend_from_slice(&unit);
        }
        let t = std::time::Instant::now();
        assert_eq!(scan_for_jpeg(&f), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    /// Containers whose structure we do not walk give up their largest colour JPEG, found by scanning.
    #[test]
    fn scanned_containers_give_the_largest_colour_jpeg() {
        let (small, big, mosaic) = (scan_jpeg(3, 100), scan_jpeg(3, 900), scan_jpeg(1, 5000));
        let heads: [&[u8]; 3] = [b"II\x1a\0\0\0HEAPCCDR", b"\0MRM\0\x01\0\0", b"FOVb\x02\0\x02\0"];
        for head in heads {
            let mut f = head.to_vec();
            for part in [&small, &mosaic, &big] {
                f.extend_from_slice(&[0xff, 0xd8, 0xff, 0x00, 0x00]); // a stray SOI-like run: not a JPEG
                f.extend_from_slice(part);
                f.extend_from_slice(&[0u8; 7]);
            }
            assert!(crate::probe(&f).is_some_and(|p| !p.is_supported()), "{head:?}");
            assert_eq!(embedded_preview(&f).as_deref(), Some(&big[..]), "{head:?}");
            for n in (0..f.len()).step_by(11) {
                let _ = embedded_preview(&f[..n]);
            }
        }
        // a preview whose first byte was overwritten (Minolta and Epson files store 00, 02 or ee there) is restored
        let mut f = b"\0MRM\0\x01\0\0".to_vec();
        f.push(0x02);
        f.extend_from_slice(&big[1..]);
        assert_eq!(embedded_preview(&f).as_deref(), Some(&big[..]));
        // only a one-component JPEG, or none: nothing
        let mut f = b"FOVb\x02\0\x02\0".to_vec();
        f.extend_from_slice(&mosaic);
        assert_eq!(embedded_preview(&f), None);
    }
}
