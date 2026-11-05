//! JPEG: marker parsing (EXIF/XMP/ICC/Adobe/MPF), full decode via `zune-jpeg`, DCT-scaled and
//! CMYK/YCCK decode via `jpeg-decoder`, embedded-preview thumbnails.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result, Thumbnail, ThumbnailSource, exif};

const F: Format = Format::Jpeg;

/// What we learn from walking the marker segments before the first scan.
#[derive(Default, Debug)]
pub(crate) struct Markers {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub precision: u8,
    /// SOFn marker byte (0xC0 baseline, 0xC1 extended, 0xC2 progressive, …).
    pub sof: u8,
    /// Number of components in the first scan (< `components` for non-interleaved sequential files).
    pub first_scan_components: u8,
    /// Number of APP1 segments: capped before the allocating decoders walk them again.
    pub app1: u32,
    pub exif: Option<Vec<u8>>,
    /// Offset of the EXIF TIFF header within the file.
    pub exif_offset: usize,
    pub xmp: Option<String>,
    pub icc: Option<Vec<u8>>,
    /// An ICC_PROFILE APP2 segment was present but its sequence was invalid.
    pub icc_invalid: bool,
    pub adobe_transform: Option<u8>,
    /// Offset of the MPF TIFF header within the file and its bytes.
    pub mpf: Option<(usize, Vec<u8>)>,
    /// The ISO 21496-1 APP2 payload after its URN: 4 bytes (versions only) on a primary image
    /// that has a gain map, the full metadata on the gain map image itself.
    pub iso_gainmap: Option<Vec<u8>>,
}

impl Markers {
    /// This image is a gain map (ISO 21496-1 metadata or Adobe `hdrgm` gain parameters), not a
    /// picture: never a preview, never the image to show.
    pub fn is_gain_map(&self) -> bool {
        self.iso_gainmap.as_ref().is_some_and(|p| p.len() > 4) || self.xmp.as_deref().is_some_and(|x| x.contains("hdrgm:GainMapMax"))
    }
}

pub(crate) fn parse_markers(b: &[u8]) -> Option<Markers> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut m = Markers::default();
    let mut icc_chunks: [Option<&[u8]>; 255] = [None; 255];
    let mut icc_total = None;
    let mut p = 2;
    while p + 4 <= b.len() {
        if b[p] != 0xFF {
            // Tolerate garbage between segments: resync on the next 0xFF.
            p += 1;
            continue;
        }
        let marker = b[p + 1];
        if marker == 0xFF {
            p += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            p += 2;
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        if marker == 0xDA {
            m.first_scan_components = b.get(p + 4).copied().unwrap_or(0);
            break;
        }
        let len = u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let start = p.checked_add(4)?;
        let declared_end = p.checked_add(2)?.checked_add(len)?;
        let complete = declared_end <= b.len();
        let end = declared_end.min(b.len());
        let seg = b.get(start..end)?;
        match marker {
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC && seg.len() >= 6 && m.components == 0 => {
                m.precision = seg[0];
                m.sof = marker;
                m.height = u16::from_be_bytes([seg[1], seg[2]]) as u32;
                m.width = u16::from_be_bytes([seg[3], seg[4]]) as u32;
                m.components = seg[5];
            }
            0xE1 => {
                m.app1 = m.app1.saturating_add(1);
                if seg.starts_with(b"Exif\0\0") && m.exif.is_none() {
                    m.exif = Some(seg[6..].to_vec());
                    m.exif_offset = start + 6;
                } else if let Some(x) = seg.strip_prefix(b"http://ns.adobe.com/xap/1.0/\0")
                    && m.xmp.is_none()
                {
                    m.xmp = Some(String::from_utf8_lossy(x).trim_end_matches('\0').to_string());
                }
            }
            0xE2 => {
                if seg.starts_with(b"ICC_PROFILE\0") {
                    if !complete || seg.len() < 14 {
                        m.icc_invalid = true;
                    } else if let (Some(&sequence), Some(&total)) = (seg.get(12), seg.get(13)) {
                        if sequence == 0 || total == 0 || sequence > total || icc_total.is_some_and(|expected| expected != total) {
                            m.icc_invalid = true;
                        } else {
                            icc_total = Some(total);
                            let Some(slot) = icc_chunks.get_mut(usize::from(sequence - 1)) else {
                                m.icc_invalid = true;
                                p = declared_end;
                                continue;
                            };
                            if slot.is_some() {
                                m.icc_invalid = true;
                            } else if let Some(data) = seg.get(14..) {
                                *slot = Some(data);
                            } else {
                                m.icc_invalid = true;
                            }
                        }
                    } else {
                        m.icc_invalid = true;
                    }
                } else if seg.starts_with(b"MPF\0") && m.mpf.is_none() {
                    m.mpf = Some((start + 4, seg[4..].to_vec()));
                } else if let Some(p) = seg.strip_prefix(crate::gainmap::ISO_URN)
                    && m.iso_gainmap.is_none()
                {
                    m.iso_gainmap = Some(p.to_vec());
                }
            }
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => {
                m.adobe_transform = Some(seg[11]);
            }
            _ => {}
        }
        p = declared_end;
    }
    if let Some(total) = icc_total {
        let mut profile_len = 0usize;
        for chunk in icc_chunks.iter().take(usize::from(total)) {
            let Some(bytes) = chunk else {
                m.icc_invalid = true;
                break;
            };
            let Some(next_len) = profile_len.checked_add(bytes.len()) else {
                m.icc_invalid = true;
                break;
            };
            profile_len = next_len;
        }
        let maximum_len = usize::from(total).checked_mul(65_519);
        if maximum_len.is_none_or(|maximum| profile_len > maximum) {
            m.icc_invalid = true;
        }
        if !m.icc_invalid {
            let mut profile = Vec::with_capacity(profile_len);
            for bytes in icc_chunks.iter().take(usize::from(total)).flatten() {
                profile.extend_from_slice(bytes);
            }
            m.icc = Some(profile);
        }
    }
    Some(m)
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    decode_with_fallback(bytes, opts, None)
}

const MAX_APP1: u32 = 128;

/// Stored dimensions and EXIF orientation from the markers, without decoding the scan data. The
/// file is refused, as a decode would refuse it, when it has no frame header, an unsupported frame
/// type or component count, or when its markers don't run to an end-of-image marker after the
/// image data (a truncated file). Damage *inside* the entropy-coded data can't be seen this way.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let bytes = &*without_sef_trailer(bytes);
    let m = parse_markers(bytes).ok_or_else(|| Error::Malformed(F, "missing SOI".into()))?;
    if m.components == 0 {
        return Err(Error::Malformed(F, "no frame header".into()));
    }
    check_size(F, m.width as u64, m.height as u64, &DecodeOptions::default())?;
    if !matches!(m.sof, 0xC0..=0xC3) {
        return Err(Error::Unsupported(F, "arithmetic-coded or hierarchical JPEG"));
    }
    if !matches!(m.components, 1 | 3 | 4) {
        return Err(Error::Malformed(F, format!("{} colour components", m.components)));
    }
    if m.first_scan_components == 0 || !reaches_eoi(bytes) {
        return Err(Error::Malformed(F, "truncated: the image data doesn't end with an end-of-image marker".into()));
    }
    let orientation = m.exif.as_deref().map(exif::summarize).unwrap_or_default().orientation.unwrap_or(1);
    Ok((m.width, m.height, orientation))
}

/// Whether the markers after SOI, followed through every segment and scan, reach an EOI marker
/// (the first one: the primary image's, ahead of any MPF images appended to the file). Segments
/// are skipped by their length; in scan data `FF 00` (stuffing), `FF FF` (fill) and RSTn are not
/// markers that end it.
fn reaches_eoi(b: &[u8]) -> bool {
    let mut p = 2usize;
    loop {
        // entropy-coded data (or garbage between segments, as `parse_markers` tolerates it)
        let Some(skip) = b.get(p..).and_then(find_ff) else { return false };
        p = p.saturating_add(skip);
        let Some(&marker) = b.get(p.saturating_add(1)) else { return false };
        p = match marker {
            0xD9 => return true,
            0xFF => p.saturating_add(1),
            0x00 | 0x01 | 0xD0..=0xD8 => p.saturating_add(2),
            _ => {
                let Some(len) = b.get(p.saturating_add(2)..p.saturating_add(4)).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) else {
                    return false;
                };
                if len < 2 {
                    return false;
                }
                p.saturating_add(2).saturating_add(len)
            }
        };
    }
}

/// A Samsung panorama whose scan runs straight into its SEF trailer, with no EOI marker (issue
/// #3350), as the file it stands for: the bytes before the trailer's earliest data block plus an
/// EOI. Every other file, a genuinely truncated one included, is borrowed unchanged.
fn without_sef_trailer(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;
    // The trailer is checked first: it reads only the file's last bytes, not the scan data.
    let Some(image) = sef_image_end(bytes).and_then(|end| bytes.get(..end)) else { return Cow::Borrowed(bytes) };
    // the image before the trailer: an FF D9 inside the SEF data doesn't end the scan
    if reaches_eoi(image) {
        return Cow::Borrowed(bytes);
    }
    let mut fixed = Vec::with_capacity(image.len().saturating_add(2));
    fixed.extend_from_slice(image);
    fixed.extend_from_slice(&[0xFF, 0xD9]);
    // The cut must leave the scan data, not a segment or anything before the first scan, open.
    let in_scan = parse_markers(&fixed).is_some_and(|m| m.first_scan_components > 0) && reaches_eoi(&fixed);
    if in_scan { Cow::Owned(fixed) } else { Cow::Borrowed(bytes) }
}

/// Where the image ends in a file carrying a Samsung SEF trailer: the start of its earliest data
/// block. The trailer is the file's last bytes: `SEFH`, a u32 LE version and entry count, 12-byte
/// entries (u16 padding, u16 type, u32 LE offset of the block back from `SEFH`, u32 LE length),
/// then the u32 LE length of that directory and `SEFT`. Each block starts with the same padding
/// and type as its entry. `None` unless every entry's block is nonempty, starts with its entry's
/// type and lies inside the file, before `SEFH`.
fn sef_image_end(b: &[u8]) -> Option<usize> {
    let le32 = |at: usize| b.get(at..at.checked_add(4)?)?.try_into().ok().map(|v| u32::from_le_bytes(v) as usize);
    let tail = b.len().checked_sub(8)?;
    if b.get(tail.checked_add(4)?..)? != b"SEFT" {
        return None;
    }
    let dir = tail.checked_sub(le32(tail)?)?;
    if b.get(dir..dir.checked_add(4)?)? != b"SEFH" {
        return None;
    }
    let count = le32(dir.checked_add(8)?)?;
    let entries = dir.checked_add(12)?;
    if count == 0 || count > tail.checked_sub(entries)? / 12 {
        return None;
    }
    let mut start = dir;
    for i in 0..count {
        let entry = entries.checked_add(i.checked_mul(12)?)?;
        let block = dir.checked_sub(le32(entry.checked_add(4)?)?)?;
        let len = le32(entry.checked_add(8)?)?;
        let kind = |at: usize| b.get(at.checked_add(2)?..at.checked_add(4)?);
        if len < 4 || block.checked_add(len)? > dir || kind(block)? != kind(entry)? {
            return None;
        }
        start = start.min(block);
    }
    Some(start)
}

pub(crate) fn decode_with_fallback(bytes: &[u8], opts: &DecodeOptions, fallback: Option<crate::NamedSpace>) -> Result<Decoded> {
    let bytes = &*without_sef_trailer(bytes);
    let m = parse_markers(bytes).ok_or_else(|| Error::Malformed(F, "missing SOI".into()))?;
    if m.components == 0 {
        return Err(Error::Malformed(F, "no frame header".into()));
    }
    check_size(F, m.width as u64, m.height as u64, opts)?;
    // The allocating decoders below parse the markers again, all of them: the count is capped so
    // their quadratic/unbounded handling of APP1 metadata stays linear in file size.
    if m.app1 > MAX_APP1 {
        return Err(Error::Malformed(F, "too many APP1 markers".into()));
    }
    let scale_to = opts.max_size.filter(|&(mw, mh)| mw > 0 && mh > 0 && (m.width >= mw.saturating_mul(2) || m.height >= mh.saturating_mul(2)));
    // zune-jpeg handles the common cases fastest; jpeg-decoder covers DCT scaling, CMYK/YCCK,
    // 12-bit, non-interleaved sequential scans (which zune-jpeg 0.5 mis-decodes with subsampling)
    // and every lossless (SOF3) file, which zune-jpeg refuses outright.
    let lossless = m.sof == 0xC3;
    let non_interleaved = matches!(m.sof, 0xC0 | 0xC1) && m.first_scan_components < m.components;
    let raw = if m.components == 4 || scale_to.is_some() || m.precision > 8 || non_interleaved || lossless {
        decode_jpeg_decoder(bytes, &m, scale_to, opts)?
    } else {
        decode_zune(bytes, &m, opts)?
    };
    // A raw container may carry the colour declaration of an otherwise untagged preview.
    // Apply it before linearization/resizing; existing JPEG metadata always wins.
    let hint = fallback
        .filter(|_| m.icc.is_none() && !m.icc_invalid && m.exif.is_none())
        .map(|space| crate::SourceSpace::named(space, crate::SpaceOrigin::Container));
    let meta = Meta { icc: m.icc, icc_invalid: m.icc_invalid, exif: m.exif, xmp: m.xmp, hint, ..Default::default() };
    finish(F, raw, meta, (m.width, m.height), opts)
}

fn decode_zune(bytes: &[u8], m: &Markers, budget: &DecodeOptions) -> Result<Raw> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let gray = m.components == 1;
    let opts = DecoderOptions::default().set_max_width(1 << 16).set_max_height(1 << 16).set_strict_mode(false).jpeg_set_out_colorspace(if gray {
        ColorSpace::Luma
    } else {
        ColorSpace::RGB
    });
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    // zune's `decode` allocates straight from the frame header it parses, with its own caps
    // set in us: check that header against the same limits the other decoders honour first.
    d.decode_headers().map_err(|e| Error::Malformed(F, e.to_string()))?;
    let (zw, zh) = d.info().map(|i| (i.width as u64, i.height as u64)).ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    check_size(F, zw, zh, budget)?;
    let px = d.decode().map_err(|e| Error::Malformed(F, e.to_string()))?;
    let info = d.info().ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let model = if gray { Model::Gray } else { Model::Rgb };
    if px.len() < w * h * model.channels() {
        return Err(Error::Malformed(F, "short output".into()));
    }
    Ok(Raw { width: w, height: h, model, alpha: false, premultiplied: false, buf: Buf::U8(px), bit_depth: 8 })
}

fn decode_jpeg_decoder(bytes: &[u8], m: &Markers, scale_to: Option<(u32, u32)>, budget: &DecodeOptions) -> Result<Raw> {
    use jpeg_decoder::PixelFormat;
    let mut d = jpeg_decoder::Decoder::new(bytes);
    d.read_info().map_err(|e| Error::Malformed(F, e.to_string()))?;
    // Its frame header is what a decode allocates from (progressive coefficient buffers follow the
    // unscaled MCU grid): check it against the same limits the other decoders honour, before any
    // DCT scale is requested.
    let info = d.info().ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    check_size(F, info.width as u64, info.height as u64, budget)?;
    // `scale` is the DCT size, not a lossless resample: on a lossless frame it would cut the
    // output to the top-left of each plane. Those decode at full size; `finish` scales after.
    if let Some((mw, mh)) = scale_to.filter(|_| m.sof != 0xC3) {
        // Ask for the smallest DCT scale still covering the target box.
        let (sw, sh) = scaled_request(m.width, m.height, mw, mh);
        d.scale(sw, sh).map_err(|e| Error::Malformed(F, e.to_string()))?;
    }
    let px = d.decode().map_err(|e| Error::Malformed(F, e.to_string()))?;
    let info = d.info().ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let (model, buf, depth) = match info.pixel_format {
        PixelFormat::L8 => (Model::Gray, Buf::U8(px), 8),
        PixelFormat::L16 => (Model::Gray, Buf::U16(lossless_samples(&px, m.precision)), 16),
        // A lossless frame over 8 bits per sample still reports `RGB24` (or `CMYK32`), but its
        // buffer holds two native-endian bytes per sample like L16.
        PixelFormat::RGB24 if m.precision > 8 => (Model::Rgb, Buf::U16(lossless_samples(&px, m.precision)), 16),
        PixelFormat::RGB24 => (Model::Rgb, Buf::U8(px), 8),
        PixelFormat::CMYK32 if m.precision > 8 => (Model::Cmyk, Buf::U16(lossless_samples(&px, m.precision)), 16),
        PixelFormat::CMYK32 => (Model::Cmyk, Buf::U8(px), 8),
    };
    Ok(Raw { width: w, height: h, model, alpha: false, premultiplied: false, buf, bit_depth: depth })
}

/// jpeg-decoder's lossless samples: one native-endian `u16` per sample holding the raw
/// `precision`-bit value. Left-justify it into the 16-bit range the rest of the pipeline reads,
/// as every 16-bit decoder feeding [`Buf::U16`] does.
fn lossless_samples(px: &[u8], precision: u8) -> Vec<u16> {
    let shift = 16u8.saturating_sub(precision);
    px.as_chunks::<2>().0.iter().map(|c| u16::from_ne_bytes([c[0], c[1]]) << shift).collect()
}

/// The first `0xFF` byte, eight bytes at a time (scan data has one every few hundred bytes: this
/// is most of [`header`]'s time).
fn find_ff(b: &[u8]) -> Option<usize> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    let (words, tail) = b.as_chunks::<8>();
    for (i, w) in words.iter().enumerate() {
        // a zero byte in !w is an 0xFF byte in w
        let x = !u64::from_ne_bytes(*w);
        if x.wrapping_sub(ONES) & !x & (ONES << 7) != 0 {
            return w.iter().position(|&v| v == 0xFF).map(|j| i * 8 + j);
        }
    }
    tail.iter().position(|&v| v == 0xFF).map(|j| words.len() * 8 + j)
}

/// The box to request from `jpeg-decoder::scale` so the decoded image still covers `mw × mh` once
/// fitted with the source aspect ratio.
fn scaled_request(w: u32, h: u32, mw: u32, mh: u32) -> (u16, u16) {
    let s = (mw as f64 / w as f64).min(mh as f64 / h as f64).min(1.0);
    let tw = ((w as f64 * s).ceil() as u32).clamp(1, u16::MAX as u32);
    let th = ((h as f64 * s).ceil() as u32).clamp(1, u16::MAX as u32);
    (tw as u16, th as u16)
}

/// Fast path: an embedded JPEG preview (EXIF IFD1 thumbnail, or the largest MPF preview) whose long
/// edge is ≥ `max_edge`, decoded with DCT scaling and fitted.
pub(crate) fn embedded_thumbnail(bytes: &[u8], max_edge: u32, min_edge: u32) -> Option<Thumbnail> {
    let m = parse_markers(bytes)?;
    let summary = m.exif.as_deref().map(exif::summarize).unwrap_or_default();
    let orientation = summary.orientation.unwrap_or(1);
    let mut candidates: Vec<(&[u8], ThumbnailSource)> = Vec::new();
    if let (Some(ex), Some((o, l))) = (m.exif.as_deref(), summary.thumbnail) {
        candidates.push((&ex[o..o + l], ThumbnailSource::ExifThumbnail));
    }
    for e in mpf_images(&m) {
        if e.kind == MPF_GAIN_MAP {
            continue;
        }
        if let Some(s) = bytes.get(e.offset..e.offset.saturating_add(e.len)) {
            candidates.push((s, ThumbnailSource::MpfPreview));
        }
    }
    // Smallest adequate preview wins.
    let mut best: Option<(u32, &[u8], ThumbnailSource)> = None;
    for (data, src) in candidates {
        let Some(pm) = parse_markers(data) else { continue };
        // a gain map (also when its MPF entry doesn't say so) is greyscale gain data, not a preview
        if pm.is_gain_map() {
            continue;
        }
        let long = pm.width.max(pm.height);
        // Previews must have the main image's aspect ratio (within 2%), else they are letterboxed/cropped.
        let aspect_ok = m.width > 0
            && m.height > 0
            && pm.height > 0
            && ((pm.width as f64 / pm.height as f64) / (m.width as f64 / m.height as f64) - 1.0).abs() < 0.02;
        // Prefer the smallest preview covering `max_edge`; otherwise the largest one ≥ `min_edge`.
        let better = match &best {
            None => true,
            Some(b) if b.0 >= max_edge => long >= max_edge && long < b.0,
            Some(b) => long > b.0,
        };
        if long >= min_edge && aspect_ok && better {
            best = Some((long, data, src));
        }
    }
    let (_, data, source) = best?;
    let d = decode(data, &DecodeOptions::fit(max_edge, max_edge)).ok()?;
    Some(Thumbnail { image: d.to_srgb8(), orientation, source, source_width: m.width, source_height: m.height })
}

/// MP type code of a gain map image (CIPA DC-007 entry attribute, low 24 bits).
pub(crate) const MPF_GAIN_MAP: u32 = 0x05_0000;

/// An MPF-listed image other than the primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MpfEntry {
    /// File offset and length of the image.
    pub offset: usize,
    pub len: usize,
    /// MP type code (attribute low 24 bits).
    pub kind: u32,
}

/// The MPF-listed images other than the primary.
pub(crate) fn mpf_images(m: &Markers) -> Vec<MpfEntry> {
    let Some((base, data)) = &m.mpf else { return vec![] };
    let Some(t) = exif::Tiff::new(data) else { return vec![] };
    let Some(ifd) = t.first_ifd() else { return vec![] };
    let Some((entries, _)) = t.ifd(ifd) else { return vec![] };
    let Some(e) = entries.iter().find(|e| e.tag == 0xB002) else { return vec![] };
    let Some(list) = t.bytes(e) else { return vec![] };
    let mut out = Vec::new();
    for (i, rec) in list.as_chunks::<16>().0.iter().enumerate().take(16) {
        if i == 0 {
            continue;
        }
        let rd = |o: usize| {
            let a = [rec[o], rec[o + 1], rec[o + 2], rec[o + 3]];
            if data.starts_with(b"II") { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
        };
        let (attr, size, off) = (rd(0), rd(4) as usize, rd(8) as usize);
        if size > 0 && off > 0 {
            out.push(MpfEntry { offset: base.saturating_add(off), len: size, kind: attr & 0x00FF_FFFF });
        }
    }
    out
}

/// Decode to the file's 8-bit samples as stored (no colour management): 1 channel for greyscale
/// files, else 3 (RGB). For data images such as gain maps, whose values are codes, not colours.
pub(crate) fn decode_codes8(bytes: &[u8]) -> Result<(usize, usize, usize, Vec<u8>)> {
    let m = parse_markers(bytes).ok_or_else(|| Error::Malformed(F, "missing SOI".into()))?;
    if m.components == 0 {
        return Err(Error::Malformed(F, "no frame header".into()));
    }
    check_size(F, m.width as u64, m.height as u64, &DecodeOptions::default())?;
    if m.app1 > MAX_APP1 {
        return Err(Error::Malformed(F, "too many APP1 markers".into()));
    }
    if !matches!(m.components, 1 | 3) || m.precision != 8 {
        return Err(Error::Malformed(F, "expected an 8-bit greyscale or RGB image".into()));
    }
    let non_interleaved = matches!(m.sof, 0xC0 | 0xC1) && m.first_scan_components < m.components;
    let raw = if non_interleaved {
        decode_jpeg_decoder(bytes, &m, None, &DecodeOptions::default())?
    } else {
        decode_zune(bytes, &m, &DecodeOptions::default())?
    };
    let ch = raw.model.channels();
    match raw.buf {
        Buf::U8(v) if v.len() >= raw.width * raw.height * ch => Ok((raw.width, raw.height, ch, v)),
        _ => Err(Error::Malformed(F, "unexpected sample format".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_fallback_matches_tagged_jpeg_before_resizing_and_keeps_own_metadata() {
        use crate::{ChromaSubsampling, EncodeImage, EncodeMeta, NamedSpace, Samples, SpaceOrigin, encode_jpeg};
        let pixels: Vec<u8> = (0..16 * 16).flat_map(|i| if i % 2 == 0 { [120, 180, 90] } else { [30, 100, 200] }).collect();
        let encode = |icc, exif| {
            encode_jpeg(
                &EncodeImage::new(16, 16, 3, Samples::U8(&pixels)),
                100,
                ChromaSubsampling::S444,
                &EncodeMeta { icc, exif, ..Default::default() },
            )
            .unwrap()
        };
        let untagged = encode(None, None);
        let adobe = crate::icc::write_named(NamedSpace::AdobeRgb);
        let tagged = encode(Some(&adobe), None);
        for opts in [DecodeOptions::default(), DecodeOptions::fit(3, 3)] {
            let with = crate::decode_jpeg_with_fallback(&untagged, opts, NamedSpace::AdobeRgb).unwrap();
            let expected = crate::decode(&tagged, opts).unwrap();
            assert_eq!(with.space.origin, SpaceOrigin::Container);
            let (a, b) = (with.to_working(), expected.to_working());
            for (p, q) in a.data.iter().zip(&b.data) {
                assert!(p.iter().zip(q).all(|(a, b)| (a - b).abs() < 0.0001), "{p:?} != {q:?}");
            }
        }
        let srgb = crate::icc::write_named(NamedSpace::Srgb);
        let exif = crate::exif::minimal_exif(6);
        for bytes in [encode(Some(&srgb), None), encode(None, Some(&exif)), encode(Some(b"bad ICC"), None)] {
            let opts = DecodeOptions::fit(5, 5);
            let with = crate::decode_jpeg_with_fallback(&bytes, opts, NamedSpace::AdobeRgb).unwrap();
            let expected = crate::decode(&bytes, opts).unwrap();
            assert_eq!(with.space, expected.space);
            assert_eq!(with.image, expected.image);
            assert_eq!(with.orientation, expected.orientation);
        }
        assert!(crate::decode_jpeg_with_fallback(b"bad JPEG", DecodeOptions::default(), NamedSpace::AdobeRgb).is_err());
    }

    fn app2_icc(sequence: u8, total: u8, data: &[u8]) -> Vec<u8> {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.extend_from_slice(&[sequence, total]);
        payload.extend_from_slice(data);
        let mut segment = vec![0xff, 0xe2];
        segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        segment.extend_from_slice(&payload);
        segment
    }

    fn jpeg_with_icc_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
        use crate::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let pixels = [180, 60, 40, 180, 60, 40, 180, 60, 40, 180, 60, 40];
        let base = encode_jpeg(&EncodeImage::new(2, 2, 3, Samples::U8(&pixels)), 100, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let mut jpeg = base.get(..2).unwrap_or_default().to_vec();
        for chunk in chunks {
            jpeg.extend_from_slice(chunk);
        }
        jpeg.extend_from_slice(base.get(2..).unwrap_or_default());
        jpeg
    }

    #[test]
    fn jpeg_icc_accepts_reordered_chunks_and_preserves_pixels() {
        use crate::{NamedSpace, SpaceOrigin, icc};
        let profile = icc::write_named(NamedSpace::DisplayP3);
        let (first, second) = profile.split_at(profile.len() / 2);
        let single = jpeg_with_icc_chunks(&[app2_icc(1, 1, &profile)]);
        let reordered = jpeg_with_icc_chunks(&[app2_icc(2, 2, second), app2_icc(1, 2, first)]);

        let one = crate::decode(&single, DecodeOptions::default()).unwrap();
        let two = crate::decode(&reordered, DecodeOptions::default()).unwrap();
        assert_eq!(one.icc.as_deref(), Some(profile.as_slice()));
        assert_eq!(two.icc.as_deref(), Some(profile.as_slice()));
        assert!(!parse_markers(&reordered).unwrap().icc_invalid);
        assert_eq!(one.space, two.space);
        assert_eq!(two.space.origin, SpaceOrigin::IccMatrixTrc);
        let (a, b) = (one.to_working(), two.to_working());
        assert_eq!(a.data.len(), b.data.len());
        for (left, right) in a.data.iter().zip(&b.data) {
            assert!(left.iter().zip(right).all(|(x, y)| (x - y).abs() < 0.00001), "{left:?} != {right:?}");
        }
    }

    #[test]
    fn jpeg_icc_rejects_incoherent_chunks_and_uses_unsupported_fallback() {
        use crate::{NamedSpace, SpaceOrigin, icc};
        let profile = icc::write_named(NamedSpace::DisplayP3);
        let (first, second) = profile.split_at(profile.len() / 2);
        let base = jpeg_with_icc_chunks(&[]);
        let reference = crate::decode(&base, DecodeOptions::default()).unwrap();
        let cases = [
            ("missing", vec![app2_icc(1, 2, &profile)]),
            ("duplicate", vec![app2_icc(1, 2, first), app2_icc(1, 2, second)]),
            ("zero sequence", vec![app2_icc(0, 1, &profile)]),
            ("zero total", vec![app2_icc(1, 0, &profile)]),
            ("out of range", vec![app2_icc(2, 1, &profile)]),
            ("gap", vec![app2_icc(1, 3, first), app2_icc(3, 3, second)]),
            ("inconsistent totals", vec![app2_icc(1, 2, first), app2_icc(2, 3, second)]),
            ("short header", {
                let mut segment = vec![0xff, 0xe2];
                segment.extend_from_slice(&15u16.to_be_bytes());
                segment.extend_from_slice(b"ICC_PROFILE\0\x01");
                vec![segment]
            }),
        ];
        for (name, chunks) in cases {
            let jpeg = jpeg_with_icc_chunks(&chunks);
            let markers = parse_markers(&jpeg).unwrap();
            let decoded = crate::decode(&jpeg, DecodeOptions::default()).unwrap();
            assert_eq!(decoded.space.origin, SpaceOrigin::IccUnsupported, "{name}: invalid profile must not be applied");
            assert!(markers.icc.is_none(), "{name}: invalid ICC must not be assembled");
            assert!(markers.icc_invalid, "{name}: invalid ICC presence must be retained");
            assert_eq!(decoded.icc, None, "{name}: invalid ICC bytes must not escape decode");
            assert_eq!(decoded.space.origin, SpaceOrigin::IccUnsupported, "{name}");
            assert_eq!(decoded.space.named, Some(NamedSpace::Srgb), "{name}");
            assert_eq!(decoded.to_working(), reference.to_working(), "{name}: fallback pixels");
        }
    }

    #[test]
    fn jpeg_icc_invalid_sequence_prevents_raw_colour_hint() {
        use crate::{NamedSpace, SpaceOrigin, icc};
        let profile = icc::write_named(NamedSpace::DisplayP3);
        let jpeg = jpeg_with_icc_chunks(&[app2_icc(1, 2, &profile)]);
        let decoded = crate::decode_jpeg_with_fallback(&jpeg, DecodeOptions::default(), NamedSpace::AdobeRgb).unwrap();
        assert_eq!(decoded.space.origin, SpaceOrigin::IccUnsupported);
        assert_eq!(decoded.space.named, Some(NamedSpace::Srgb));
    }

    #[test]
    fn jpeg_icc_rejects_truncated_app2_segment() {
        use crate::{NamedSpace, icc};
        let profile = icc::write_named(NamedSpace::DisplayP3);
        let mut segment = app2_icc(1, 1, &profile);
        let declared = u16::from_be_bytes([segment[2], segment[3]]).saturating_add(1);
        segment[2..4].copy_from_slice(&declared.to_be_bytes());
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend_from_slice(&segment);
        let markers = parse_markers(&jpeg).unwrap();
        assert!(markers.icc.is_none());
        assert!(markers.icc_invalid);
    }

    #[test]
    fn scaled_request_covers() {
        assert_eq!(scaled_request(6000, 4000, 256, 256), (256, 171));
        assert_eq!(scaled_request(100, 100, 256, 256), (100, 100));
    }

    #[test]
    fn find_ff_matches_a_plain_search() {
        let mut b: Vec<u8> = (0..300u32).map(|i| (i * 7 % 255) as u8).collect();
        for at in [None, Some(0), Some(7), Some(8), Some(15), Some(203), Some(296), Some(299)] {
            if let Some(i) = at {
                b[i] = 0xFF;
            }
            for start in [0, 1, 5, 9, 290, 299, 300] {
                let s = &b[start..];
                assert_eq!(find_ff(s), s.iter().position(|&v| v == 0xFF), "{at:?} from {start}");
            }
        }
    }

    #[test]
    fn unbounded_fit_box_decodes_at_full_size() {
        // a full-size load asks for the largest box (`usize::MAX` saturated to `u32::MAX`): no DCT scaling,
        // and no overflow working out whether to scale
        use crate::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let pixels = vec![128u8; 32 * 24 * 3];
        let jpeg = encode_jpeg(&EncodeImage::new(32, 24, 3, Samples::U8(&pixels)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let d = crate::decode(&jpeg, DecodeOptions::fit(u32::MAX, u32::MAX)).unwrap();
        assert_eq!((d.width, d.height), (32, 24));
    }

    #[test]
    fn markers_on_garbage() {
        assert!(parse_markers(b"").is_none());
        assert!(parse_markers(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 1]).is_none());
        let m = parse_markers(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 200, 1, 2]).unwrap();
        assert!(m.exif.is_none());
    }

    // --- lossless (SOF3) synthetic files: one DHT table with a single 1-bit category-0 code, so
    // every pixel is a zero difference and decodes to the predictor's initial sample. No such
    // encoder exists here; the bytes are the smallest a decoder accepts.

    fn lossless_header(out: &mut Vec<u8>, components: u8, w: u16, precision: u8) {
        // DHT: one DC table (id 0) with a single 1-bit code for the category-0 symbol.
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&(2u16 + 1 + 16 + 1).to_be_bytes());
        out.push(0x00);
        let mut counts = [0u8; 16];
        counts[0] = 1;
        out.extend_from_slice(&counts);
        out.push(0x00);
        // SOF3
        out.extend_from_slice(&[0xFF, 0xC3]);
        out.extend_from_slice(&(8u16 + 3 * components as u16).to_be_bytes());
        out.push(precision);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.push(components);
        for c in 1..=components {
            out.extend_from_slice(&[c, 0x11, 0]);
        }
        // SOS: predictor Ra, Se = 0, Ah/Al = 0
        out.extend_from_slice(&[0xFF, 0xDA]);
        out.extend_from_slice(&(6u16 + 2 * components as u16).to_be_bytes());
        out.push(components);
        for c in 1..=components {
            out.extend_from_slice(&[c, 0]);
        }
        out.extend_from_slice(&[1, 0, 0]);
    }

    /// A minimal lossless JPEG, `w` x 1, `components` samples per pixel, every sample the initial
    /// prediction `1 << (precision - 1)`.
    fn lossless_jpeg(components: u8, w: u16, precision: u8) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        lossless_header(&mut out, components, w, precision);
        // entropy: one zero bit per sample (a category-0 code), padded to whole bytes
        out.extend_from_slice(&vec![0u8; (components as usize * w as usize).div_ceil(8)]);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    /// jpeg-decoder emits lossless samples native-endian (its `to_ne_bytes`) holding the raw
    /// `precision`-bit value, so the old `from_be_bytes` swapped every sample (2048 -> 8) and the
    /// raw value was never left-justified into the 16-bit buffer.
    #[test]
    fn lossless_grayscale_samples_are_native_endian_and_left_justified() {
        let bytes = lossless_jpeg(1, 8, 12);
        let m = parse_markers(&bytes).unwrap();
        let raw = decode_jpeg_decoder(&bytes, &m, None, &DecodeOptions::default()).unwrap();
        let Buf::U16(v) = &raw.buf else { panic!("expected 16-bit samples") };
        assert_eq!(v, &vec![2048u16 << 4; 8], "0x0800 native-endian, left-justified");
        assert_eq!((raw.model, raw.bit_depth), (Model::Gray, 16));
    }

    /// A lossless frame over 8 bits per sample reports `RGB24`, but its buffer is two bytes per
    /// sample: it must become `Buf::U16`, not `Buf::U8` over double-length rows.
    #[test]
    fn lossless_high_precision_rgb_is_16_bit() {
        let bytes = lossless_jpeg(3, 2, 12);
        let m = parse_markers(&bytes).unwrap();
        let raw = decode_jpeg_decoder(&bytes, &m, None, &DecodeOptions::default()).unwrap();
        let Buf::U16(v) = &raw.buf else { panic!("expected 16-bit samples") };
        assert_eq!(v, &vec![2048u16 << 4; 6]);
        assert_eq!(raw.model, Model::Rgb);
    }

    /// The same 16-bit wrapping for four components: `CMYK32` over 8 bits is two bytes a sample.
    #[test]
    fn lossless_high_precision_cmyk_is_16_bit() {
        let bytes = lossless_jpeg(4, 2, 12);
        let m = parse_markers(&bytes).unwrap();
        let raw = decode_jpeg_decoder(&bytes, &m, None, &DecodeOptions::default()).unwrap();
        let Buf::U16(v) = &raw.buf else { panic!("expected 16-bit samples") };
        assert_eq!(v, &vec![2048u16 << 4; 8]);
        assert_eq!((raw.model, raw.bit_depth), (Model::Cmyk, 16));
    }

    /// zune-jpeg refuses SOF3 ("Lossless Huffman"), so an 8-bit lossless file must route to
    /// jpeg-decoder; `header` accepts it, so `decode` used to fail a file we listed as readable.
    #[test]
    fn eight_bit_lossless_jpeg_decodes() {
        let bytes = lossless_jpeg(1, 8, 8);
        let d = crate::decode(&bytes, DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (8, 1));
        assert_eq!(d.bit_depth, 8);
    }

    /// Rewrites an encoded file's frame-header dimensions: same tables and scan, a hostile
    /// annotated size.
    fn with_dims(bytes: &[u8], w: u16, h: u16) -> Vec<u8> {
        let mut out = bytes.to_vec();
        let mut p = 2usize;
        while p + 1 < out.len() {
            if out[p] != 0xFF {
                p += 1;
                continue;
            }
            match out[p + 1] {
                0xC0..=0xCF if out[p + 1] != 0xC4 && out[p + 1] != 0xC8 && out[p + 1] != 0xCC => {
                    out[p + 5..p + 7].copy_from_slice(&h.to_be_bytes());
                    out[p + 7..p + 9].copy_from_slice(&w.to_be_bytes());
                    return out;
                }
                0x01 | 0xD0..=0xD9 => p += 2,
                0xFF => p += 1,
                _ => {
                    let Some(l) = out.get(p + 2..p + 4) else { return out };
                    p += 2 + usize::from(u16::from_be_bytes([l[0], l[1]]));
                }
            }
        }
        out
    }

    /// A file claiming 65535×65535 must be refused for size before zune-jpeg allocates from
    /// its frame header (4 GB+ of samples from roughly five hundred bytes of tables).
    #[test]
    fn zune_refuses_its_own_frame_header_over_the_budget() {
        use crate::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let pixels = vec![128u8; 32 * 24];
        let jpeg = encode_jpeg(&EncodeImage::new(32, 24, 1, Samples::U8(&pixels)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let hostile = with_dims(&jpeg, 65535, 65535);
        assert_ne!(hostile, jpeg);
        match crate::decode(&hostile, DecodeOptions::default()) {
            Err(Error::TooLarge(w, h)) => assert_eq!((w, h), (65535, 65535)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// The jpeg-decoder branch (a 4-component file) is checked the same way: its own
    /// `read_info` result is what a full-size decode allocates from.
    #[test]
    fn jpeg_decoder_refuses_its_own_frame_header_over_the_budget() {
        use crate::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let pixels = vec![128u8; 32 * 24 * 4];
        let jpeg = encode_jpeg(&EncodeImage::new(32, 24, 4, Samples::U8(&pixels)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let hostile = with_dims(&jpeg, 65535, 65535);
        match crate::decode(&hostile, DecodeOptions::default()) {
            Err(Error::TooLarge(w, h)) => assert_eq!((w, h), (65535, 65535)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Extended XMP is reassembled from every APP1 fragment: an unbounded count is quadratic
    /// memory in a marker walk, so the count is capped.
    #[test]
    fn an_app1_flood_is_refused_before_the_decoders_rewalk_it() {
        let mut b = vec![0xFF, 0xD8];
        // a 1-component SOF0: 8-bit, 2×2, component (1, 0x11, Q0)
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 8, 0, 2, 0, 2, 1, 0x01, 0x11, 0x00]);
        for _ in 0..200 {
            b.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x02]);
        }
        b.extend_from_slice(&[0xFF, 0xD9]);
        match crate::decode(&b, DecodeOptions::default()) {
            Err(Error::Malformed(F, m)) => assert!(m.contains("APP1"), "{m}"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
