//! Embedded preview extraction: the largest baseline/progressive JPEG stored in a TIFF-based raw (IFD strips
//! with JPEG compression, `JPEGInterchangeFormat` pointers in any IFD, Nikon/others' maker-note preview IFDs),
//! or a DNG 1.7 JPEG XL preview IFD stored as a single tile/strip (a standalone `.jxl` file, as the spec
//! recommends for previews).

use lightcraft_tiff::image::chunk_bytes;
use lightcraft_tiff::tags as t;
use lightcraft_tiff::{Ifd, Tiff, makernote};

/// Colour space of an embedded JPEG when the enclosing raw supplies it instead of the JPEG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewColorSpace {
    Srgb,
    AdobeRgb,
}

/// The colour space the raw says its embedded JPEG is in, for a JPEG that declares none itself (no ICC profile,
/// no EXIF), first available first: the Nikon maker-note ColorSpace (0x001e: 1 = sRGB, 2 = Adobe RGB, per Nikon
/// tag documentation); for a Canon CR2 or CR3, the Canon maker-note ColorSpace (0x00b4: 1 = sRGB, 2 = Adobe RGB);
/// the raw's own EXIF ColorSpace (0xA001: 1 = sRGB), or Uncalibrated (0xFFFF) with the Interoperability IFD's
/// InteropIndex "R03", the DCF option file for Adobe RGB (as a Sony body shot in Adobe RGB writes, issue #778; per
/// the Exif and DCF specifications), not for a DNG. This is a fallback only: a JPEG's own ICC/EXIF colour
/// declaration takes precedence.
pub fn embedded_preview_color_space(bytes: &[u8]) -> Option<PreviewColorSpace> {
    let Ok(tiff) = Tiff::parse(bytes) else { return cr3_color_space(bytes) };
    let make = tiff.ifds.first()?.string(t::MAKE).unwrap_or_default();
    let exif = tiff.exif()?;
    if make.to_ascii_uppercase().starts_with("NIKON")
        && let Some(space) = nikon_color_space(bytes, &tiff, exif, &make)
    {
        return Some(space);
    }
    // a DNG's EXIF describes the original capture; its previews are rendered by the converter
    if tiff.ifds.first()?.contains(t::DNG_VERSION) {
        return None;
    }
    if make.to_ascii_uppercase().starts_with("CANON")
        && let Some(e) = exif.get(t::MAKER_NOTE)
        && let Some(mn) = makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make)
        && let Some(space) = canon_color_space(&mn.ifd)
    {
        return Some(space);
    }
    match exif.u16(0xA001)? {
        1 => Some(PreviewColorSpace::Srgb),
        0xFFFF if exif.interop.as_deref()?.bytes(0x0001)?.starts_with(b"R03") => Some(PreviewColorSpace::AdobeRgb),
        _ => None,
    }
}

/// A CR3 keeps its EXIF and maker note in `CMT2`/`CMT3` TIFF blocks and its JPEGs carry no EXIF; a body shot in
/// Adobe RGB writes ColorSpace Uncalibrated (0xFFFF) with no Interoperability IFD (issue #2654), so the Canon
/// maker note says which, else an EXIF ColorSpace of sRGB is taken.
fn cr3_color_space(bytes: &[u8]) -> Option<PreviewColorSpace> {
    let cr3 = lightcraft_meta::cr3::parse_cr3(bytes)?;
    let first = |i: usize| cr3.cmt[i].and_then(|b| Tiff::parse(b).ok()).and_then(|t| t.ifds.into_iter().next());
    if let Some(space) = first(2).and_then(|mn| canon_color_space(&mn)) {
        return Some(space);
    }
    (first(1)?.u16(0xA001)? == 1).then_some(PreviewColorSpace::Srgb)
}

/// The Canon maker-note ColorSpace (0x00b4: 1 = sRGB, 2 = Adobe RGB, per ExifTool's published Canon tag names).
/// A Canon body shot in Adobe RGB writes EXIF ColorSpace Uncalibrated (0xFFFF) with InteropIndex "R98" in a CR2
/// and no Interoperability IFD in a CR3, so the EXIF alone does not say Adobe RGB (issue #2654).
fn canon_color_space(note: &Ifd) -> Option<PreviewColorSpace> {
    match note.u16(0x00b4)? {
        1 => Some(PreviewColorSpace::Srgb),
        2 => Some(PreviewColorSpace::AdobeRgb),
        _ => None,
    }
}

fn nikon_color_space(bytes: &[u8], tiff: &Tiff, exif: &Ifd, make: &str) -> Option<PreviewColorSpace> {
    let e = exif.get(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, make)?;
    match mn.ifd.u16(0x001e)? {
        1 => Some(PreviewColorSpace::Srgb),
        2 => Some(PreviewColorSpace::AdobeRgb),
        _ => None,
    }
}

/// Whether the camera optimised its embedded JPEG's dynamic range with local tone mapping that the raw data doesn't
/// carry: Sony's Dynamic Range Optimizer (maker note `0xb025`, see `vendor::arw`). Such a JPEG is brighter in its
/// darker regions than the raw developed with the camera's global tone curve. `Some(false)` when the file says it
/// was off, `None` when it doesn't say (other makers, notes without the tag, undocumented values).
pub fn embedded_preview_dynamic_range_optimized(bytes: &[u8]) -> Option<bool> {
    let tiff = Tiff::parse(bytes).ok()?;
    let make = tiff.ifds.first()?.string(t::MAKE)?;
    if !make.trim().to_ascii_uppercase().starts_with("SONY") {
        return None;
    }
    let e = tiff.exif()?.get(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make)?;
    crate::vendor::arw::dynamic_range_optimizer(&mn)
}

/// The first frame (SOF) marker of a JPEG and its position, from the marker segments before the first scan.
fn first_frame(b: &[u8]) -> Option<(u8, usize)> {
    if b.len() < 4 || !b.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut i = 2usize;
    while i.checked_add(4)? <= b.len() {
        if *b.get(i)? != 0xff {
            return None;
        }
        let m = *b.get(i + 1)?;
        if m == 0xff {
            i += 1;
            continue;
        }
        match m {
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xcb | 0xcd..=0xcf => return Some((m, i)),
            0xda | 0xd9 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i + 2)?, *b.get(i + 3)?]));
        i = i.checked_add(2 + len)?;
    }
    None
}

/// Whether `b` looks like a displayable (DCT) JPEG: SOI, and the first SOF marker is not lossless.
fn is_dct_jpeg(b: &[u8]) -> bool {
    first_frame(b).is_some_and(|(m, _)| matches!(m, 0xc0..=0xc2))
}

/// Width, height and component count of a displayable (DCT) JPEG, from its frame header.
fn jpeg_frame(b: &[u8]) -> Option<(u32, u32, u8)> {
    let (_, i) = first_frame(b).filter(|(m, _)| matches!(m, 0xc0..=0xc2))?;
    let at = |k: usize| b.get(i.checked_add(k)?).copied();
    let be16 = |k: usize| Some(u32::from(u16::from_be_bytes([at(k)?, at(k + 1)?])));
    Some((be16(7)?, be16(5)?, at(9)?))
}
fn is_jxl(b: &[u8]) -> bool {
    b.starts_with(&[0xff, 0x0a]) || b.starts_with(&[0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a])
}

fn candidates<'a>(data: &'a [u8], ifd: &Ifd, base: u64, out: &mut Vec<&'a [u8]>) {
    // whole JPEG files stored as an undefined-type tag value (e.g. Panasonic `JpgFromRaw` 0x002e)
    for e in &ifd.entries {
        if matches!(e.value, lightcraft_tiff::Value::Undefined(_))
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
        && let Some(chunk) = info.single_chunk(data.len() as u64)
        && let Some(s) = chunk_bytes(data, &chunk)
    {
        out.push(s);
    }
    // a rendered (RGB or grey, never CFA/LinearRaw/mask) JPEG XL preview in one chunk
    if ifd.u16(t::COMPRESSION) == Some(t::compression::JPEG_XL)
        && matches!(ifd.u16(t::PHOTOMETRIC), Some(t::photometric::BLACK_IS_ZERO | t::photometric::RGB))
        && let Ok(info) = ifd.image()
        && let Some(chunk) = info.single_chunk(data.len() as u64)
        && let Some(s) = chunk_bytes(data, &chunk)
        && is_jxl(s)
    {
        out.push(s);
    }
}

/// The largest embedded preview, if any: a JPEG, or (DNG 1.7) a JPEG XL file, or for a Hasselblad 3FR / FFF, which
/// has no JPEG, its uncompressed RGB preview wrapped as a baseline TIFF — all decode with `lightcraft_codecs::decode`.
pub fn embedded_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    preview_covering(bytes, None)
}

/// The smallest embedded JPEG whose long edge is at least `min_edge` and that looks like a copy of the largest preview
/// (its aspect ratio within 1 %, so not a letterboxed or cropped thumbnail, and as many colour components, so not a
/// grey mask or depth map), else [`embedded_preview`]. Only TIFF-based raws keep several: an A7R VI's 1616 × 1080
/// preview is 0.3 MB beside its 9984 × 6656 one (5.6 MB), so a stand-in up to 1616 px decodes about a twentieth of the data.
pub fn embedded_preview_covering(bytes: &[u8], min_edge: u32) -> Option<Vec<u8>> {
    preview_covering(bytes, Some(min_edge))
}

/// Of the DCT JPEGs in `found`, the one with the shortest long edge of at least `min_edge` with `largest`'s aspect
/// ratio (within 1 %) and component count.
fn smallest_covering<'a>(found: &[&'a [u8]], largest: &[u8], min_edge: u32) -> Option<&'a [u8]> {
    let (bw, bh, bc) = jpeg_frame(largest)?;
    let like = |(w, h, c): (u32, u32, u8)| {
        let (a, b) = (u64::from(w) * u64::from(bh), u64::from(h) * u64::from(bw));
        w > 0 && h > 0 && c == bc && a.abs_diff(b) * 100 <= a
    };
    found
        .iter()
        .filter_map(|&s| jpeg_frame(s).filter(|&f| like(f) && f.0.max(f.1) >= min_edge).map(|(w, h, _)| (s, w.max(h))))
        .min_by_key(|&(_, long)| long)
        .map(|(s, _)| s)
}

fn preview_covering(bytes: &[u8], min_edge: Option<u32>) -> Option<Vec<u8>> {
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
                && let Ok((pifd, _)) = lightcraft_tiff::parse_ifd_at(bytes, mn.base + off, mn.order, mn.base, false, &Default::default())
            {
                candidates(bytes, &pifd, mn.base, &mut found);
            }
        }
    }
    // Olympus: CameraSettings preview
    if let Some(p) = crate::vendor::orf::preview(bytes) {
        found.push(p);
    }
    let largest = found.iter().copied().filter(|s| is_dct_jpeg(s) || is_jxl(s)).max_by_key(|s| s.len());
    let best = min_edge
        .zip(largest)
        .and_then(|(min, big)| smallest_covering(&found, big, min))
        .or(largest)
        .map(|s| if is_jxl(s) { s.to_vec() } else { trim_eoi(s).to_vec() });
    // a TIFF raw whose preview no tag points to: one we can't decode (Leaf MOS), or Epson ERF (in its maker note)
    best.or_else(|| format.filter(|f| !f.is_supported() || *f == crate::RawFormat::Erf).and_then(|_| scan_for_jpeg(bytes)))
        // Hasselblad 3FR / FFF carry no JPEG, only uncompressed RGB images: one wrapped as a baseline TIFF
        .or_else(|| (format == Some(crate::RawFormat::CfaTiff)).then(|| crate::vendor::hasselblad::rgb_preview(&tiff, bytes)).flatten())
}

/// The end of the DCT JPEG whose SOI is at `soi` (exclusive index after its EOI) and its component count,
/// walking the marker segments and the entropy-coded data. `None` when it is not a complete, displayable
/// (baseline or progressive) JPEG, so a stray `D8 FF` in other data is rejected. `*reach` is left at
/// the furthest byte looked at (what the walk cost; see [`scan_for_jpeg`]).
/// Where a preview lives in a file, so a caller with a seekable file can read just that part of it.
///
/// Why it exists: a 56 MB Fuji RAF keeps its JPEG in the first 5.6 MB (measured: DSCF9492.RAF at
/// 5.6 MB), so a grid thumbnail or a loupe preview has no reason to pull the remaining 50 MB —
/// which on a network share is the difference between a moment and half a minute. A caller that has
/// only bytes (`wasm`, tests) keeps using [`embedded_preview`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewRange {
    /// Byte offset of the preview in the file.
    pub offset: u64,
    /// Bytes to read at [`Self::offset`]: the container's declared preview extent, which may carry
    /// padding past the preview's end-of-image marker.
    pub window_len: u64,
}

/// The preview's range in the file, when the file's header alone locates it.
///
/// `bytes` is the file's first [`PREVIEW_PREFIX`] bytes (more is fine). `None` means "read the file
/// whole and call [`embedded_preview`]": the formats not covered here (a TIFF-based raw's pointer
/// tag can sit in an IFD the prefix cuts short; CR3 scans box by box) and files whose pointer is
/// damaged.
///
/// The caller reads `[range.offset, range.offset + range.window_len)` and passes those bytes to
/// [`embedded_preview_from_range`], which returns exactly what [`embedded_preview`] returns for the
/// whole file.
///
/// Pass a prefix that actually covers the pointer's range (read [`Self::offset`] +
/// [`Self::window_len`] bytes, or start with [`PREVIEW_PREFIX`] and grow when it is not enough):
/// [`crate::vendor::raf::header`] clamps a slice to the buffer it was given, so a prefix that stops
/// inside the declared extent yields a shorter `window_len` than the file really holds.
pub fn bounded_preview(bytes: &[u8]) -> Option<PreviewRange> {
    if !bytes.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let jpeg = crate::vendor::raf::header(bytes).ok()?.jpeg?;
    let offset = (jpeg.as_ptr() as usize).checked_sub(bytes.as_ptr() as usize)?;
    if offset.checked_add(jpeg.len())? > bytes.len() {
        return None;
    }
    Some(PreviewRange { offset: offset as u64, window_len: jpeg.len() as u64 })
}

/// The preview inside `window`, which holds the file's bytes at `[range.offset, range.offset +
/// range.window_len)`: the counterpart of [`bounded_preview`]. Padding past the end-of-image marker
/// is dropped, exactly as [`embedded_preview`] drops it.
pub fn embedded_preview_from_range(window: &[u8], range: PreviewRange) -> Option<Vec<u8>> {
    let preview = window.get(..usize::try_from(range.window_len).ok()?)?;
    if !is_dct_jpeg(preview) {
        return None;
    }
    Some(trim_eoi(preview).to_vec())
}

/// How many bytes of a raw file are enough for [`bounded_preview`]: every covered format keeps its
/// preview pointer in a fixed header, well inside this.
pub const PREVIEW_PREFIX: usize = 4096;

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
                let Some(p) = b.get(*i..)?.iter().position(|&x| x == 0xff) else {
                    // the terminal search examined the rest of the slice: charge it before failing,
                    // or a crafted file can repeat full-file searches while the budget stays positive
                    *i = b.len();
                    return None;
                };
                *i += p;
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
    scan_with_budget(data, scan_budget(data.len())).0
}

/// The walks' shared budget: eight times the file's size, at least 1 MiB (a tiny file still gets a
/// useful allowance), saturating so a hostile length cannot wrap it.
fn scan_budget(len: usize) -> usize {
    len.saturating_mul(8).max(1 << 20)
}

/// [`scan_for_jpeg`] with the walks' shared budget passed in, returning the walks' charged extent:
/// `(preview, bytes charged)`. The counter records the work each walk did whether or not budget
/// remains, so it stays a measure of work rather than of `budget - left`; a last walk may overshoot
/// the remaining budget by its own reach. It is a conservative charged-work measure, not literally
/// the bytes read: marker payloads skipped by `*i += len` are charged without being read, and a
/// failed terminal search is charged to the end of the slice. That over-count is safe for the
/// budget. It is a `u64` with saturating addition because the unbudgeted worst case is quadratic in
/// the file size (32-bit `usize` could otherwise overflow), and it costs one addition per
/// candidate, no allocation.
fn scan_with_budget(data: &[u8], budget: usize) -> (Option<Vec<u8>>, u64) {
    let mut best: Option<(usize, usize)> = None;
    let mut at = 1;
    let mut left = budget;
    let mut walked = 0u64;
    while let Some(off) = data.get(at..).and_then(|s| s.iter().position(|&b| b == 0xd8)) {
        let d8 = at + off;
        at = d8 + 1;
        if data.get(d8 + 1) != Some(&0xff) || !matches!(data.get(d8 + 2), Some(0xc0..=0xc4 | 0xdb | 0xe0..=0xef | 0xfe)) {
            continue;
        }
        let start = d8 - 1; // `at` starts at 1, so d8 >= 1
        let mut reach = start;
        let found = jpeg_extent(data, start, &mut reach);
        let spent = reach.saturating_sub(start);
        left = left.saturating_sub(spent);
        walked = walked.saturating_add(spent as u64);
        if left == 0 {
            break;
        }
        if let Some((end, 3)) = found {
            if best.is_none_or(|(s, e)| end - start > e - s) {
                best = Some((start, end));
            }
            at = end;
        }
    }
    let jpeg = best.and_then(|(start, end)| {
        let mut jpeg = data.get(start..end)?.to_vec();
        if let Some(first) = jpeg.first_mut() {
            *first = 0xff;
        }
        Some(jpeg)
    });
    (jpeg, walked)
}

/// Canon CR3: the full-size JPEG track (see [`lightcraft_meta::cr3`]), else the `PRVW` / `THMB` boxes.
fn cr3_preview(bytes: &[u8]) -> Option<&[u8]> {
    let full = lightcraft_meta::cr3::parse_cr3(bytes)
        .and_then(|c| c.tracks.iter().find(|t| t.kind == lightcraft_meta::cr3::Cr3TrackKind::Jpeg).and_then(|t| t.data));
    if let Some(j) = full.and_then(|(at, len)| bytes.get(at..at.checked_add(len)?)).filter(|j| is_dct_jpeg(j)) {
        return Some(j);
    }
    cr3_preview_boxes(bytes)
}

/// Canon CR3 (ISO base media file): the `PRVW` box (Laurent Clévy's CR3 notes; layout confirmed on a CC0
/// sample) is `u32 size, "PRVW", u32 0, u16 ?, u16 width, u16 height, u16 ?, u32 jpeg length, JPEG`; the smaller
/// `THMB` box has the same shape. Returns the larger valid one.
/// Marker-walk budget of the CR3 fallback: candidates can overlap (each claiming the rest of the file) and
/// [`is_dct_jpeg`] walks each marker by marker, so walking every candidate would be quadratic in file size.
/// The `THMB`/`PRVW` previews this finds are far below the cap together; once it is spent the scan stops.
const CR3_MAX_WALK: usize = 8 << 20;

fn cr3_preview_boxes(bytes: &[u8]) -> Option<&[u8]> {
    let mut best: Option<&[u8]> = None;
    let mut walked = 0usize;
    for tag in [b"PRVW", b"THMB"] {
        let mut from = 0;
        while let Some(i) = bytes.get(from..).and_then(|s| s.windows(4).position(|w| w == tag)).map(|p| p + from) {
            from = i + 4;
            let Some(start) = i.checked_sub(4) else { continue };
            let be32 = |at: usize| bytes.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
            let (Some(size), Some(len)) = (be32(start), be32(start + 20)) else { continue };
            let j = start + 24;
            // 32-bit box lengths can wrap `usize` on wasm32: check the sums, then slice with `get`
            let (Some(end), Some(lim)) = (j.checked_add(len), start.checked_add(size.max(24))) else { continue };
            if len < 4 || end > lim {
                continue;
            }
            let Some(jpeg) = bytes.get(j..end) else { continue };
            // One that doesn't fit the budget left is skipped; a smaller one later may still fit.
            if walked + jpeg.len() > CR3_MAX_WALK {
                continue;
            }
            walked += jpeg.len();
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
mod tests;
