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
    pub exif: Option<Vec<u8>>,
    /// Offset of the EXIF TIFF header within the file.
    pub exif_offset: usize,
    pub xmp: Option<String>,
    pub icc: Option<Vec<u8>>,
    pub adobe_transform: Option<u8>,
    /// Offset of the MPF TIFF header within the file and its bytes.
    pub mpf: Option<(usize, Vec<u8>)>,
}

pub(crate) fn parse_markers(b: &[u8]) -> Option<Markers> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut m = Markers::default();
    let mut icc_chunks: Vec<(u8, &[u8])> = Vec::new();
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
        let start = p + 4;
        let end = (p + 2 + len).min(b.len());
        let seg = &b[start..end];
        match marker {
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                if seg.len() >= 6 && m.components == 0 {
                    m.precision = seg[0];
                    m.sof = marker;
                    m.height = u16::from_be_bytes([seg[1], seg[2]]) as u32;
                    m.width = u16::from_be_bytes([seg[3], seg[4]]) as u32;
                    m.components = seg[5];
                }
            }
            0xE1 => {
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
                if seg.starts_with(b"ICC_PROFILE\0") && seg.len() >= 14 {
                    icc_chunks.push((seg[12], &seg[14..]));
                } else if seg.starts_with(b"MPF\0") && m.mpf.is_none() {
                    m.mpf = Some((start + 4, seg[4..].to_vec()));
                }
            }
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => {
                m.adobe_transform = Some(seg[11]);
            }
            _ => {}
        }
        p += 2 + len;
    }
    if !icc_chunks.is_empty() {
        icc_chunks.sort_by_key(|c| c.0);
        m.icc = Some(icc_chunks.iter().flat_map(|c| c.1.iter().copied()).collect());
    }
    Some(m)
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    decode_with_fallback(bytes, opts, None)
}

/// Stored dimensions and EXIF orientation from the markers, without decoding the scan data. The
/// file is refused, as a decode would refuse it, when it has no frame header, an unsupported frame
/// type or component count, or when its markers don't run to an end-of-image marker after the
/// image data (a truncated file). Damage *inside* the entropy-coded data can't be seen this way.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
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

pub(crate) fn decode_with_fallback(bytes: &[u8], opts: &DecodeOptions, fallback: Option<crate::NamedSpace>) -> Result<Decoded> {
    let m = parse_markers(bytes).ok_or_else(|| Error::Malformed(F, "missing SOI".into()))?;
    if m.components == 0 {
        return Err(Error::Malformed(F, "no frame header".into()));
    }
    check_size(F, m.width as u64, m.height as u64, opts)?;
    let scale_to = opts.max_size.filter(|&(mw, mh)| mw > 0 && mh > 0 && (m.width >= mw.saturating_mul(2) || m.height >= mh.saturating_mul(2)));
    // zune-jpeg handles the common cases fastest; jpeg-decoder covers DCT scaling, CMYK/YCCK,
    // 12-bit and non-interleaved sequential scans (which zune-jpeg 0.5 mis-decodes with subsampling).
    let non_interleaved = matches!(m.sof, 0xC0 | 0xC1) && m.first_scan_components < m.components;
    let raw = if m.components == 4 || scale_to.is_some() || m.precision > 8 || non_interleaved {
        decode_jpeg_decoder(bytes, &m, scale_to)?
    } else {
        decode_zune(bytes, &m)?
    };
    // A raw container may carry the colour declaration of an otherwise untagged preview.
    // Apply it before linearization/resizing; existing JPEG metadata always wins.
    let hint = fallback.filter(|_| m.icc.is_none() && m.exif.is_none()).map(|space| crate::SourceSpace::named(space, crate::SpaceOrigin::Container));
    let meta = Meta { icc: m.icc, exif: m.exif, xmp: m.xmp, hint, ..Default::default() };
    finish(F, raw, meta, (m.width, m.height), opts)
}

fn decode_zune(bytes: &[u8], m: &Markers) -> Result<Raw> {
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
    let px = d.decode().map_err(|e| Error::Malformed(F, e.to_string()))?;
    let info = d.info().ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let model = if gray { Model::Gray } else { Model::Rgb };
    if px.len() < w * h * model.channels() {
        return Err(Error::Malformed(F, "short output".into()));
    }
    Ok(Raw { width: w, height: h, model, alpha: false, premultiplied: false, buf: Buf::U8(px), bit_depth: 8 })
}

fn decode_jpeg_decoder(bytes: &[u8], m: &Markers, scale_to: Option<(u32, u32)>) -> Result<Raw> {
    use jpeg_decoder::PixelFormat;
    let mut d = jpeg_decoder::Decoder::new(bytes);
    d.read_info().map_err(|e| Error::Malformed(F, e.to_string()))?;
    if let Some((mw, mh)) = scale_to {
        // Ask for the smallest DCT scale still covering the target box.
        let (sw, sh) = scaled_request(m.width, m.height, mw, mh);
        d.scale(sw, sh).map_err(|e| Error::Malformed(F, e.to_string()))?;
    }
    let px = d.decode().map_err(|e| Error::Malformed(F, e.to_string()))?;
    let info = d.info().ok_or_else(|| Error::Malformed(F, "no info".into()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let (model, buf, depth) = match info.pixel_format {
        PixelFormat::L8 => (Model::Gray, Buf::U8(px), 8),
        PixelFormat::L16 => {
            let v = px.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            (Model::Gray, Buf::U16(v), 16)
        }
        PixelFormat::RGB24 => (Model::Rgb, Buf::U8(px), 8),
        PixelFormat::CMYK32 => (Model::Cmyk, Buf::U8(px), 8),
    };
    Ok(Raw { width: w, height: h, model, alpha: false, premultiplied: false, buf, bit_depth: depth })
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
    for (o, l) in mpf_images(&m) {
        if let Some(s) = bytes.get(o..o.saturating_add(l)) {
            candidates.push((s, ThumbnailSource::MpfPreview));
        }
    }
    // Smallest adequate preview wins.
    let mut best: Option<(u32, &[u8], ThumbnailSource)> = None;
    for (data, src) in candidates {
        let Some(pm) = parse_markers(data) else { continue };
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

/// (file offset, length) of MPF-listed images other than the primary.
fn mpf_images(m: &Markers) -> Vec<(usize, usize)> {
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
        let (size, off) = (rd(4) as usize, rd(8) as usize);
        if size > 0 && off > 0 {
            out.push((base + off, size));
        }
    }
    out
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
}
