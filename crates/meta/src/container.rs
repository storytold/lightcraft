//! Locating metadata blocks inside image containers.
//!
//! - JPEG (ITU-T T.81 marker structure): APP1 `Exif\0\0`, APP1 `http://ns.adobe.com/xap/1.0/\0` (XMP),
//!   APP1 `http://ns.adobe.com/xmp/extension/\0` (extended XMP, reassembled), APP2 `ICC_PROFILE\0`
//!   (multi-chunk, reassembled in sequence order), APP13 `Photoshop 3.0\0` 8BIM resource 0x0404 (IPTC-IIM).
//! - PNG (PNG spec 3rd ed.): `eXIf`, `iTXt` with keyword `XML:com.adobe.xmp` (uncompressed).
//! - WebP (RIFF container spec): `EXIF`, `XMP `, `ICCP` chunks.
//! - TIFF-based files (TIFF, DNG and most raws): the whole file is the Exif block; XMP (700), ICC (34675) and
//!   IPTC (33723) from IFD0.
//! - Photoshop PSD/PSB: EXIF (image resource 1058) and XMP (image resource 1060) from the bounded image-resource section.
//! - Minolta MRW (see [`crate::mrw`]): the `TTW` block is the Exif block.
//! - Canon CR3 (ISO base media file, see [`crate::cr3`]): `CMT1`/`CMT2`/`CMT4` merged into one Exif block; the
//!   XMP `uuid` box.
//! - HEIF/HEIC/AVIF (ISO base media file, see [`crate::heif`]): the `Exif` item's TIFF stream and the XMP
//!   `mime` item.

use lightcraft_tiff::{Tiff, tags};

/// Metadata blocks found in a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Embedded {
    /// TIFF-structured Exif block (without the `Exif\0\0` prefix).
    pub exif: Option<Vec<u8>>,
    /// XMP packet (the main one; extended XMP is appended by [`Embedded::xmp_extended`]).
    pub xmp: Option<String>,
    /// Extended XMP (JPEG only), reassembled.
    pub xmp_extended: Option<String>,
    pub icc: Option<Vec<u8>>,
    /// IPTC-IIM stream.
    pub iptc: Option<Vec<u8>>,
}

/// Sniff the container and collect its metadata blocks. Unknown containers yield an empty result.
pub fn embedded(bytes: &[u8]) -> Embedded {
    if bytes.starts_with(b"8BPS") {
        return crate::psd::embedded(bytes);
    }
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        // Fujifilm RAF: metadata lives in the embedded JPEG whose (offset, length) are big-endian u32s at byte 84
        let be = |i: usize| bytes.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
        return match (be(84), be(88)) {
            (Some(o), Some(l)) if o > 0 => bytes.get(o..o.saturating_add(l).min(bytes.len())).map(jpeg_segments).unwrap_or_default(),
            _ => Embedded::default(),
        };
    }
    if let Some(m) = crate::mrw::parse(bytes) {
        // Minolta MRW: the `TTW` block is a complete TIFF with the Exif data
        return m
            .block(b"TTW")
            .filter(|t| Tiff::parse(t).is_ok())
            .map(|t| Embedded { exif: Some(t.to_vec()), ..Default::default() })
            .unwrap_or_default();
    }
    if let Some(c) = crate::cr3::parse_cr3(bytes) {
        return Embedded { exif: crate::cr3::merged_exif(&c), xmp: c.xmp.map(|b| String::from_utf8_lossy(b).into_owned()), ..Default::default() };
    }
    if crate::heif::is_heif(bytes) {
        return crate::heif::metadata(bytes);
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        jpeg_segments(bytes)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png_chunks(bytes)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        webp_chunks(bytes)
    } else if let Ok(t) = Tiff::parse(bytes) {
        let ifd0 = &t.ifds[0];
        Embedded {
            exif: Some(bytes.to_vec()),
            xmp: ifd0.bytes(tags::XMP).map(|b| String::from_utf8_lossy(b).into_owned()),
            xmp_extended: None,
            icc: ifd0.bytes(tags::ICC_PROFILE).map(|b| b.to_vec()),
            iptc: ifd0.value(tags::IPTC_NAA).map(|v| match v {
                lightcraft_tiff::Value::Byte(b) | lightcraft_tiff::Value::Undefined(b) => b.clone(),
                other => lightcraft_tiff::writer::encode(t.order, other),
            }),
        }
    } else {
        Embedded::default()
    }
}

/// The bounded JPEG preview inside a Fujifilm RAF, if its header range is complete.
pub(crate) fn raf_embedded_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let read_be32 = |at: usize| {
        let end = at.checked_add(4)?;
        let value: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
        Some(u32::from_be_bytes(value) as usize)
    };
    let offset = read_be32(84)?;
    let length = read_be32(88)?;
    if offset == 0 || length == 0 {
        return None;
    }
    let end = offset.checked_add(length)?;
    let jpeg = bytes.get(offset..end)?;
    jpeg.starts_with(&[0xff, 0xd8]).then_some(jpeg)
}

/// Check every PNG/WebP EXIF chunk for location tags. Malformed recognized containers or EXIF blocks
/// fail closed because an Original copy cannot establish that location data is absent.
pub(crate) fn png_webp_embeds_exif_gps_location(bytes: &[u8]) -> Option<bool> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut i = 8usize;
        while i < bytes.len() {
            let length_start = i;
            let Some(type_start) = length_start.checked_add(4) else { return Some(true) };
            let Some(data_start) = type_start.checked_add(4) else { return Some(true) };
            let Some(length_end) = bytes.get(length_start..type_start) else { return Some(true) };
            let Ok(length_bytes) = <[u8; 4]>::try_from(length_end) else { return Some(true) };
            let Ok(length) = usize::try_from(u32::from_be_bytes(length_bytes)) else { return Some(true) };
            let Some(data_end) = data_start.checked_add(length) else { return Some(true) };
            let Some(chunk_end) = data_end.checked_add(4) else { return Some(true) };
            let (Some(kind), Some(data)) = (bytes.get(type_start..data_start), bytes.get(data_start..data_end)) else { return Some(true) };
            if chunk_end > bytes.len() {
                return Some(true);
            }
            if kind == b"eXIf" && crate::exif::exif_gps_tag_presence(data) != Some(false) {
                return Some(true);
            }
            if kind == b"IEND" {
                return Some(length != 0 || chunk_end != bytes.len());
            }
            i = chunk_end;
        }
        return Some(true);
    }

    if bytes.len() >= 12 && bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        let Some(riff_size_bytes) = bytes.get(4..8) else { return Some(true) };
        let Ok(riff_size_bytes) = <[u8; 4]>::try_from(riff_size_bytes) else { return Some(true) };
        let Ok(riff_size) = usize::try_from(u32::from_le_bytes(riff_size_bytes)) else { return Some(true) };
        let Some(riff_end) = 8usize.checked_add(riff_size) else { return Some(true) };
        if riff_size < 4 || riff_end != bytes.len() {
            return Some(true);
        }
        let mut i = 12usize;
        while i < bytes.len() {
            let Some(length_start) = i.checked_add(4) else { return Some(true) };
            let Some(data_start) = i.checked_add(8) else { return Some(true) };
            let Some(length_end) = bytes.get(length_start..data_start) else { return Some(true) };
            let Ok(length_bytes) = <[u8; 4]>::try_from(length_end) else { return Some(true) };
            let Ok(length) = usize::try_from(u32::from_le_bytes(length_bytes)) else { return Some(true) };
            let Some(data_end) = data_start.checked_add(length) else { return Some(true) };
            let Some(padded_end) = data_end.checked_add(length & 1) else { return Some(true) };
            let (Some(kind), Some(data)) = (bytes.get(i..length_start), bytes.get(data_start..data_end)) else { return Some(true) };
            if padded_end > bytes.len() {
                return Some(true);
            }
            if kind == b"EXIF" && crate::exif::exif_gps_tag_presence(data) != Some(false) {
                return Some(true);
            }
            i = padded_end;
        }
        return Some(false);
    }

    None
}

/// Locate metadata blocks while preserving PSD/PSB resource-structure errors for callers that
/// must establish whether embedded GPS is absent. Other containers keep their best-effort walk.
pub(crate) fn try_embedded(bytes: &[u8]) -> Result<Embedded, crate::ExtractError> {
    if bytes.starts_with(b"8BPS") { crate::psd::parse(bytes).ok_or(crate::ExtractError::MalformedPsd) } else { Ok(embedded(bytes)) }
}

const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";

/// Walk JPEG markers up to SOS and collect APP1 Exif / XMP, APP2 ICC and APP13 IPTC.
pub fn jpeg_segments(b: &[u8]) -> Embedded {
    jpeg_segments_with_exif(b, |_| {})
}

/// Collect JPEG metadata while visiting every EXIF APP1 block for GPS fallback.
fn jpeg_segments_with_exif(b: &[u8], mut visit_exif: impl FnMut(&[u8])) -> Embedded {
    let mut out = Embedded::default();
    if !b.starts_with(&[0xff, 0xd8]) {
        return out;
    }
    let mut icc_parts: [Option<&[u8]>; 255] = [None; 255];
    let mut icc_total = None;
    let mut icc_invalid = false;
    let mut ext: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut i = 2usize;
    while i + 4 <= b.len() {
        if b[i] != 0xff {
            break;
        }
        let marker = b[i + 1];
        if marker == 0xff {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            break;
        }
        let Some(start) = i.checked_add(4) else { break };
        let Some(declared_end) = i.checked_add(2).and_then(|at| at.checked_add(len)) else { break };
        let complete = declared_end <= b.len();
        let end = declared_end.min(b.len());
        let Some(payload) = b.get(start..end) else { break };
        match marker {
            0xe1 if payload.starts_with(b"Exif\0") => {
                let exif = payload.get(6..).unwrap_or(&[]);
                visit_exif(exif);
                if out.exif.is_none() {
                    out.exif = Some(exif.to_vec());
                }
            }
            0xe1 if payload.starts_with(XMP_SIG) && out.xmp.is_none() => {
                out.xmp = Some(String::from_utf8_lossy(&payload[XMP_SIG.len()..]).into_owned());
            }
            0xe1 if payload.starts_with(XMP_EXT_SIG) => {
                // GUID (32) + full length (4) + offset (4) + data
                let p = &payload[XMP_EXT_SIG.len()..];
                if p.len() >= 40 {
                    let off = u32::from_be_bytes([p[36], p[37], p[38], p[39]]);
                    ext.push((off, p[40..].to_vec()));
                }
            }
            0xe2 if payload.starts_with(b"ICC_PROFILE\0") => {
                if !complete || payload.len() < 14 {
                    icc_invalid = true;
                } else if let Some(header) = payload.get(12..14) {
                    let (Some(&sequence), Some(&total)) = (header.first(), header.get(1)) else {
                        icc_invalid = true;
                        i = end;
                        continue;
                    };
                    if sequence == 0 || total == 0 || sequence > total || icc_total.is_some_and(|expected| expected != total) {
                        icc_invalid = true;
                    } else {
                        icc_total = Some(total);
                        if let Some(slot) = icc_parts.get_mut(usize::from(sequence - 1)) {
                            if slot.is_some() {
                                icc_invalid = true;
                            } else if let Some(data) = payload.get(14..) {
                                *slot = Some(data);
                            } else {
                                icc_invalid = true;
                            }
                        } else {
                            icc_invalid = true;
                        }
                    }
                } else {
                    icc_invalid = true;
                }
            }
            0xed if payload.starts_with(b"Photoshop 3.0\0") && out.iptc.is_none() => out.iptc = photoshop_iptc(&payload[14..]),
            _ => {}
        }
        i = end;
    }
    if let Some(total) = icc_total {
        let mut profile_len = 0usize;
        for chunk in icc_parts.iter().take(usize::from(total)) {
            let Some(bytes) = chunk else {
                icc_invalid = true;
                break;
            };
            let Some(next_len) = profile_len.checked_add(bytes.len()) else {
                icc_invalid = true;
                break;
            };
            profile_len = next_len;
        }
        let maximum_len = usize::from(total).checked_mul(65_519);
        if maximum_len.is_none_or(|maximum| profile_len > maximum) {
            icc_invalid = true;
        }
        if !icc_invalid {
            let mut profile = Vec::with_capacity(profile_len);
            for bytes in icc_parts.iter().take(usize::from(total)).flatten() {
                profile.extend_from_slice(bytes);
            }
            out.icc = Some(profile);
        }
    }
    if !ext.is_empty() {
        ext.sort_by_key(|p| p.0);
        out.xmp_extended = Some(String::from_utf8_lossy(&ext.into_iter().flat_map(|p| p.1).collect::<Vec<u8>>()).into_owned());
    }
    out
}

/// GPS from the first EXIF APP1 block in a JPEG preview that projects a valid position.
pub(crate) fn jpeg_exif_gps(bytes: &[u8]) -> Option<crate::Gps> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut gps = None;
    jpeg_segments_with_exif(bytes, |exif| {
        if gps.is_none() {
            gps = crate::read_exif(exif).gps;
        }
    });
    gps
}

/// GPS from every EXIF APP1 packet in a JPEG source or a RAF's embedded JPEG preview.
pub(crate) fn source_jpeg_exif_gps(bytes: &[u8]) -> Option<crate::Gps> {
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg_exif_gps(bytes);
    }
    raf_embedded_jpeg(bytes).and_then(jpeg_exif_gps)
}

/// Check JPEG Exif APP1 packets for location tags. MPF multi-picture JPEGs and truncated or
/// unparseable Exif packets fail closed because an Original copy cannot establish that location data is absent.
pub(crate) fn jpeg_embeds_exif_gps_location(bytes: &[u8]) -> bool {
    jpeg_embeds_exif_gps_location_at_depth(bytes, 0)
}

/// Nested TIFF thumbnails are untrusted; cap recursive JPEG/EXIF traversal and fail closed.
pub(crate) fn jpeg_embeds_exif_gps_location_at_depth(bytes: &[u8], depth: usize) -> bool {
    if depth >= crate::exif::MAX_GPS_NESTING {
        return true;
    }
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut i = 2usize;
    loop {
        if bytes.get(i) != Some(&0xff) {
            return true;
        }
        let Some(marker_at) = i.checked_add(1) else { return true };
        let Some(&marker) = bytes.get(marker_at) else { return true };
        if marker == 0xff {
            i = marker_at;
            continue;
        }
        if marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            let Some(next) = marker_at.checked_add(1) else { return true };
            i = next;
            continue;
        }
        if marker == 0xd9 || marker == 0xda {
            return false;
        }
        let Some(length_start) = i.checked_add(2) else { return true };
        let Some(length_end) = i.checked_add(4) else { return true };
        let Some(length) = bytes.get(length_start..length_end) else { return true };
        let (Some(&hi), Some(&lo)) = (length.first(), length.get(1)) else { return true };
        let segment_length = usize::from(u16::from_be_bytes([hi, lo]));
        if segment_length < 2 {
            return true;
        }
        let Some(payload_start) = i.checked_add(4) else { return true };
        let Some(segment_end) = i.checked_add(2).and_then(|at| at.checked_add(segment_length)) else { return true };
        if segment_end > bytes.len() {
            return true;
        }
        let Some(payload) = bytes.get(payload_start..segment_end) else { return true };
        if marker == 0xe2 && payload.starts_with(b"MPF\0") {
            // Secondary JPEG images follow the primary image and need their own metadata scans.
            // Until those offsets are parsed, refuse an Original copy rather than miss GPS there.
            return true;
        }
        if marker == 0xe1 && payload.starts_with(b"Exif\0") {
            let Some(exif) = payload.get(6..) else { return true };
            if crate::exif::exif_gps_tag_presence_at_depth(exif, depth.saturating_add(1)) != Some(false) {
                return true;
            }
        }
        i = segment_end;
    }
}

/// IPTC-IIM block (resource 0x0404) from a Photoshop image-resource stream (`8BIM` records).
fn photoshop_iptc(mut b: &[u8]) -> Option<Vec<u8>> {
    while b.len() >= 12 && &b[..4] == b"8BIM" {
        let id = u16::from_be_bytes([b[4], b[5]]);
        let name_len = b[6] as usize;
        let name_total = (1 + name_len).next_multiple_of(2);
        let size_at = 6 + name_total;
        let size = u32::from_be_bytes(b.get(size_at..size_at + 4)?.try_into().ok()?) as usize;
        let data_at = size_at + 4;
        let data = b.get(data_at..data_at.checked_add(size)?)?;
        if id == 0x0404 {
            return Some(data.to_vec());
        }
        b = b.get(data_at + size.next_multiple_of(2)..)?;
    }
    None
}

/// PNG chunks: `eXIf`, uncompressed `iTXt` XMP.
pub fn png_chunks(b: &[u8]) -> Embedded {
    let mut out = Embedded::default();
    let mut i = 8usize;
    while i + 12 <= b.len() {
        let len = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let typ = &b[i + 4..i + 8];
        let Some(data) = b.get(i + 8..(i + 8).saturating_add(len)) else { break };
        match typ {
            b"eXIf" => out.exif = Some(crate::strip_exif_header(data).to_vec()),
            b"iTXt" if data.starts_with(b"XML:com.adobe.xmp\0") => {
                // keyword\0 compression-flag compression-method language\0 translated\0 text
                let rest = &data[18..];
                if rest.len() >= 2 && rest[0] == 0 {
                    let rest = &rest[2..];
                    let mut parts = rest.splitn(3, |&c| c == 0);
                    let (_, _, text) = (parts.next(), parts.next(), parts.next());
                    if let Some(t) = text {
                        out.xmp = Some(String::from_utf8_lossy(t).into_owned());
                    }
                }
            }
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    out
}

/// WebP RIFF chunks `EXIF`, `XMP `, `ICCP`.
pub fn webp_chunks(b: &[u8]) -> Embedded {
    let mut out = Embedded::default();
    let mut i = 12usize;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let Some(data) = b.get(i + 8..(i + 8).saturating_add(len)) else { break };
        match id {
            b"EXIF" => out.exif = Some(crate::strip_exif_header(data).to_vec()),
            b"XMP " => out.xmp = Some(String::from_utf8_lossy(data).into_owned()),
            b"ICCP" => out.icc = Some(data.to_vec()),
            _ => {}
        }
        i += 8 + len + (len & 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif::tests::sample_exif;
    use lightcraft_tiff::ByteOrder;

    fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xff, marker];
        v.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn icc_seg(sequence: u8, total: u8, data: &[u8]) -> Vec<u8> {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.extend_from_slice(&[sequence, total]);
        payload.extend_from_slice(data);
        seg(0xe2, &payload)
    }

    fn jpeg_with_icc(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8];
        let packet = crate::write_xmp(&crate::Metadata { title: Some("retained".into()), ..Default::default() }, None);
        let mut xmp = XMP_SIG.to_vec();
        xmp.extend_from_slice(packet.as_bytes());
        jpeg.extend_from_slice(&seg(0xe1, &xmp));
        for part in parts {
            jpeg.extend_from_slice(part);
        }
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2]);
        jpeg
    }

    pub(crate) fn sample_jpeg() -> Vec<u8> {
        let mut j = vec![0xff, 0xd8];
        j.extend(seg(0xe0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"));
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(sample_exif(ByteOrder::Big));
        j.extend(seg(0xe1, &exif));
        let meta = crate::Metadata { title: Some("Sunset".into()), rating: Some(4), ..Default::default() };
        let mut xmp = XMP_SIG.to_vec();
        xmp.extend(crate::write_xmp(&meta, Some("{\"exposure\":1.5}")).into_bytes());
        j.extend(seg(0xe1, &xmp));
        // ICC in two chunks, out of order
        j.extend(seg(0xe2, b"ICC_PROFILE\0\x02\x02WORLD"));
        j.extend(seg(0xe2, b"ICC_PROFILE\0\x01\x02HELLO"));
        // IPTC
        let iim = crate::iptc::tests::sample_iim();
        let mut ps = b"Photoshop 3.0\0".to_vec();
        ps.extend_from_slice(b"8BIM\x04\x0c\0\0");
        ps.extend_from_slice(&2u32.to_be_bytes());
        ps.extend_from_slice(&[1, 2]);
        ps.extend_from_slice(b"8BIM\x04\x04\0\0");
        ps.extend_from_slice(&(iim.len() as u32).to_be_bytes());
        ps.extend_from_slice(&iim);
        j.extend(seg(0xed, &ps));
        j.extend_from_slice(&[0xff, 0xda, 0, 2, 1, 2, 3, 0xff, 0xd9]);
        j
    }

    #[test]
    fn jpeg_blocks() {
        let j = sample_jpeg();
        let e = jpeg_segments(&j);
        assert_eq!(e.icc.as_deref(), Some(&b"HELLOWORLD"[..]));
        assert!(e.xmp.as_deref().unwrap().contains("Sunset"));
        assert!(e.iptc.is_some());
        let m = crate::extract(&j);
        assert_eq!(m.iso, Some(400));
        assert_eq!(m.title.as_deref(), Some("Sunset"));
        assert_eq!(m.rating, Some(4)); // XMP overrides EXIF rating 3
        assert_eq!(m.keywords, vec!["alpha".to_string(), "beta".to_string()]); // from IPTC
        assert_eq!(m.caption.as_deref(), Some("A caption"));
    }

    /// The Original-export scan checks each Exif APP1 packet, not only the first one.
    #[test]
    fn jpeg_gps_guard_checks_each_exif_app1() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let exif = |destination_tag: Option<u16>| {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("No GPS camera".into()));
            if let Some(tag) = destination_tag {
                let mut gps = IfdBuilder::new();
                gps.set(tag, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
                ifd0.set_child(t::GPS_IFD, gps);
            }
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
        };
        let jpeg = |blocks: &[Vec<u8>]| {
            let mut bytes = vec![0xff, 0xd8];
            for block in blocks {
                let mut payload = b"Exif\0\0".to_vec();
                payload.extend_from_slice(block);
                bytes.extend(seg(0xe1, &payload));
            }
            bytes.extend_from_slice(&[0xff, 0xd9]);
            bytes
        };
        let empty = exif(None);
        let destination = exif(Some(t::GPS_DEST_LATITUDE));
        assert_eq!(crate::exif::exif_gps_tag_presence(&empty), Some(false));
        assert!(!crate::source_embeds_exif_gps_location(&jpeg(std::slice::from_ref(&empty))));
        assert!(crate::source_embeds_exif_gps_location(&jpeg(&[empty, destination])));
    }

    /// MPF JPEGs fail closed because the metadata scan does not inspect secondary images.
    #[test]
    fn jpeg_gps_guard_fails_closed_for_mpf_secondary_images() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        let exif = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

        let mut secondary = vec![0xff, 0xd8];
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&exif);
        secondary.extend(seg(0xe1, &exif_payload));
        secondary.extend_from_slice(&[0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9]);

        let scan = [0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9];
        let mpf_payload = |primary_size: u32, secondary_offset: u32| {
            let mut payload = b"MPF\0II*\0\x08\0\0\0".to_vec();
            payload.extend_from_slice(&3u16.to_le_bytes());
            payload.extend_from_slice(&0xb000u16.to_le_bytes());
            payload.extend_from_slice(&7u16.to_le_bytes());
            payload.extend_from_slice(&4u32.to_le_bytes());
            payload.extend_from_slice(b"0100");
            payload.extend_from_slice(&0xb001u16.to_le_bytes());
            payload.extend_from_slice(&4u16.to_le_bytes());
            payload.extend_from_slice(&1u32.to_le_bytes());
            payload.extend_from_slice(&2u32.to_le_bytes());
            payload.extend_from_slice(&0xb002u16.to_le_bytes());
            payload.extend_from_slice(&7u16.to_le_bytes());
            payload.extend_from_slice(&32u32.to_le_bytes());
            payload.extend_from_slice(&50u32.to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
            for (attributes, size, offset) in
                [(0x2000_0000u32, primary_size, 0u32), (0u32, u32::try_from(secondary.len()).unwrap(), secondary_offset)]
            {
                payload.extend_from_slice(&attributes.to_le_bytes());
                payload.extend_from_slice(&size.to_le_bytes());
                payload.extend_from_slice(&offset.to_le_bytes());
                payload.extend_from_slice(&0u16.to_le_bytes());
                payload.extend_from_slice(&0u16.to_le_bytes());
            }
            payload
        };
        let mpf = seg(0xe2, &mpf_payload(0, 0));
        let primary_size = 2 + mpf.len() + scan.len();
        let secondary_offset = primary_size - 10;
        let mpf = seg(0xe2, &mpf_payload(u32::try_from(primary_size).unwrap(), u32::try_from(secondary_offset).unwrap()));

        let mut mpo = vec![0xff, 0xd8];
        mpo.extend(mpf);
        mpo.extend_from_slice(&scan);
        mpo.extend_from_slice(&secondary);
        assert!(crate::source_embeds_exif_gps_location(&mpo), "GPS in the secondary image must block an Original copy");

        let gps_free_jpeg = [0xff, 0xd8, 0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9];
        assert!(!crate::source_embeds_exif_gps_location(&gps_free_jpeg), "ordinary GPS-free JPEGs remain eligible");
    }

    /// Malformed JPEG headers fail closed, while scan and end markers stop the metadata scan.
    #[test]
    fn jpeg_gps_guard_fails_closed_for_malformed_segments() {
        let cases = [
            ("truncated segment", vec![0xff, 0xd8, 0xff, 0xe1, 0, 6, b'E', b'x']),
            ("invalid segment length", vec![0xff, 0xd8, 0xff, 0xe0, 0, 1]),
            ("missing marker", vec![0xff, 0xd8, 0x01]),
        ];
        for (label, bytes) in cases {
            assert!(crate::source_embeds_exif_gps_location(&bytes), "{label} must fail closed");
        }
        for bytes in [vec![0xff, 0xd8, 0xff, 0xda, 0, 2], vec![0xff, 0xd8, 0xff, 0xd9]] {
            assert!(!crate::source_embeds_exif_gps_location(&bytes), "SOS and EOI terminate a valid metadata scan");
        }
    }

    /// Invalid EXIF GPS coordinates leave XMP fallback available and valid EXIF remains preferred.
    #[test]
    fn jpeg_gps_falls_back_from_invalid_exif_and_keeps_valid_exif() {
        use crate::Gps;
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let exif_with_latitude = |latitude: Vec<(u32, u32)>| {
            let mut gps = IfdBuilder::new();
            gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
            gps.set(t::GPS_LATITUDE, Value::Rational(latitude));
            gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
            gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(10, 1), (30, 1), (0, 1)]));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set_child(t::GPS_IFD, gps);
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
        };
        let jpeg = |exif: &[u8], xmp: Option<&str>| {
            let mut jpeg = vec![0xff, 0xd8];
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend_from_slice(exif);
            jpeg.extend(seg(0xe1, &payload));
            if let Some(xmp) = xmp {
                let mut payload = XMP_SIG.to_vec();
                payload.extend_from_slice(xmp.as_bytes());
                jpeg.extend(seg(0xe1, &payload));
            }
            jpeg.extend_from_slice(&[0xff, 0xd9]);
            jpeg
        };

        let fallback = Gps { latitude: 51.5, longitude: -0.125, altitude: Some(12.0) };
        let xmp = crate::write_xmp(&crate::Metadata { gps: Some(fallback), ..Default::default() }, None);
        let invalid_exif = exif_with_latitude(vec![(45, 1), (60, 1), (0, 1)]);
        assert!(crate::exif_embeds_gps_location(&invalid_exif), "location-bearing EXIF tags are retained even when their values are invalid");
        let bytes = jpeg(&invalid_exif, Some(&xmp));
        assert_eq!(crate::try_extract(&bytes).unwrap().gps, Some(fallback), "invalid EXIF coordinates leave XMP fallback available");
        assert_eq!(embedded(&bytes).exif.as_deref(), Some(invalid_exif.as_slice()), "the source EXIF bytes remain available");

        let valid_exif = exif_with_latitude(vec![(45, 1), (30, 1), (0, 1)]);
        assert!(crate::exif_embeds_gps_location(&valid_exif));
        let bytes = jpeg(&valid_exif, None);
        assert_eq!(crate::try_extract(&bytes).unwrap().gps, Some(Gps { latitude: 45.5, longitude: 10.5, altitude: None }));
    }

    #[test]
    fn jpeg_extended_xmp() {
        let mut j = vec![0xff, 0xd8];
        for (off, part) in [(5u32, &b"world"[..]), (0, b"hello")] {
            let mut p = XMP_EXT_SIG.to_vec();
            p.extend_from_slice(&[b'A'; 32]);
            p.extend_from_slice(&10u32.to_be_bytes());
            p.extend_from_slice(&off.to_be_bytes());
            p.extend_from_slice(part);
            j.extend(seg(0xe1, &p));
        }
        assert_eq!(jpeg_segments(&j).xmp_extended.as_deref(), Some("helloworld"));
    }

    #[test]
    fn jpeg_icc_accepts_reordered_chunks() {
        let parts = [icc_seg(2, 2, b"world"), icc_seg(1, 2, b"hello")];
        let embedded = jpeg_segments(&jpeg_with_icc(&parts));
        assert_eq!(embedded.icc.as_deref(), Some(&b"helloworld"[..]));
    }

    #[test]
    fn jpeg_icc_rejects_incomplete_or_incoherent_chunks() {
        let cases = [
            ("missing", vec![icc_seg(1, 2, b"helloworld")]),
            ("duplicate", vec![icc_seg(1, 2, b"hello"), icc_seg(1, 2, b"world")]),
            ("zero sequence", vec![icc_seg(0, 1, b"helloworld")]),
            ("zero total", vec![icc_seg(1, 0, b"helloworld")]),
            ("out of range", vec![icc_seg(2, 1, b"helloworld")]),
            ("gap", vec![icc_seg(1, 3, b"hello"), icc_seg(3, 3, b"world")]),
            ("inconsistent totals", vec![icc_seg(1, 2, b"hello"), icc_seg(2, 3, b"world")]),
            ("short header", vec![seg(0xe2, b"ICC_PROFILE\0\x01")]),
        ];
        for (name, parts) in cases {
            let embedded = jpeg_segments(&jpeg_with_icc(&parts));
            assert!(embedded.icc.is_none(), "{name} ICC chunks must be rejected");
            assert!(embedded.xmp.as_deref().is_some_and(|x| x.contains("retained")), "{name}: unrelated XMP must remain readable");
        }
    }

    #[test]
    fn jpeg_icc_rejects_truncated_segment_and_keeps_other_metadata() {
        let mut truncated = icc_seg(1, 1, b"profile");
        let declared = u16::from_be_bytes([truncated[2], truncated[3]]).saturating_add(1);
        truncated[2..4].copy_from_slice(&declared.to_be_bytes());
        let mut jpeg = vec![0xff, 0xd8];
        let xmp = crate::write_xmp(&crate::Metadata { title: Some("kept".into()), ..Default::default() }, None);
        let mut packet = XMP_SIG.to_vec();
        packet.extend_from_slice(xmp.as_bytes());
        jpeg.extend_from_slice(&seg(0xe1, &packet));
        jpeg.extend_from_slice(&truncated);

        let embedded = jpeg_segments(&jpeg);
        assert!(embedded.icc.is_none());
        assert!(embedded.xmp.as_deref().is_some_and(|x| x.contains("kept")));
    }

    /// The Original-copy guard checks every PNG/WebP EXIF chunk, not only the last one.
    #[test]
    fn png_and_webp_gps_guard_checks_each_exif_chunk() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let exif = |with_gps: bool| {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Procedural camera".into()));
            if with_gps {
                let mut gps = IfdBuilder::new();
                gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
                ifd0.set_child(t::GPS_IFD, gps);
            }
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
        };
        let chunks = [exif(true), exif(false)];

        let png = |blocks: &[Vec<u8>]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            for data in blocks {
                bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
                bytes.extend_from_slice(b"eXIf");
                bytes.extend_from_slice(data);
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes.extend_from_slice(b"IEND");
            bytes.extend_from_slice(&[0; 4]);
            bytes
        };
        let webp = |blocks: &[Vec<u8>]| {
            let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
            for data in blocks {
                bytes.extend_from_slice(b"EXIF");
                bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
                bytes.extend_from_slice(data);
                if data.len() % 2 == 1 {
                    bytes.push(0);
                }
            }
            let riff_size = u32::try_from(bytes.len().saturating_sub(8)).unwrap_or(u32::MAX);
            bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
            bytes
        };

        assert_eq!(crate::exif::exif_gps_tag_presence(&chunks[0]), Some(true));
        assert_eq!(crate::exif::exif_gps_tag_presence(&chunks[1]), Some(false));
        assert_eq!(embedded(&png(&chunks)).exif.as_deref(), Some(chunks[1].as_slice()));
        let png_has_gps = crate::source_embeds_exif_gps_location(&png(&chunks));
        let webp_has_gps = crate::source_embeds_exif_gps_location(&webp(&chunks));
        assert!(png_has_gps && webp_has_gps, "earlier EXIF chunks contain GPS: PNG={png_has_gps}, WebP={webp_has_gps}");
        let no_gps = [exif(false), exif(false)];
        assert!(!crate::source_embeds_exif_gps_location(&png(&no_gps)));
        assert!(!crate::source_embeds_exif_gps_location(&webp(&no_gps)));
    }

    /// Malformed PNG and WebP structure cannot authorize a privacy-sensitive Original copy.
    #[test]
    fn malformed_png_and_webp_gps_guards_fail_closed() {
        let png = |chunks: &[(&[u8], &[u8])]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            for (kind, data) in chunks {
                bytes.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
                bytes.extend_from_slice(kind);
                bytes.extend_from_slice(data);
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes
        };
        let truncated_chunk = {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(&8u32.to_be_bytes());
            bytes.extend_from_slice(b"eXIf");
            bytes.extend_from_slice(&[1, 2]);
            bytes
        };
        let missing_iend = png(&[(&b"tEXt"[..], &[][..])]);
        let invalid_iend = png(&[(&b"IEND"[..], &[0][..])]);
        let trailing_after_iend = {
            let mut bytes = png(&[(&b"IEND"[..], &[][..])]);
            bytes.push(0);
            bytes
        };
        for (label, bytes) in [
            ("truncated PNG chunk", truncated_chunk),
            ("missing PNG IEND", missing_iend),
            ("invalid PNG IEND", invalid_iend),
            ("bytes after PNG IEND", trailing_after_iend),
        ] {
            assert!(crate::source_embeds_exif_gps_location(&bytes), "{label} must fail closed");
        }
        assert!(!crate::source_embeds_exif_gps_location(&png(&[(&b"IEND"[..], &[][..])])));

        let webp = |chunks: &[(&[u8], &[u8], bool)]| {
            let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
            for (kind, data, pad) in chunks {
                bytes.extend_from_slice(kind);
                bytes.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
                bytes.extend_from_slice(data);
                if *pad && data.len() % 2 == 1 {
                    bytes.push(0);
                }
            }
            let riff_size = u32::try_from(bytes.len().saturating_sub(8)).unwrap_or(u32::MAX);
            bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
            bytes
        };
        let mut wrong_riff_size = webp(&[]);
        wrong_riff_size[4..8].copy_from_slice(&5u32.to_le_bytes());
        assert!(crate::source_embeds_exif_gps_location(&wrong_riff_size), "RIFF size mismatch must fail closed");
        let odd_chunk_without_padding = webp(&[(&b"VP8 "[..], &[1][..], false)]);
        assert!(crate::source_embeds_exif_gps_location(&odd_chunk_without_padding), "missing odd-size padding must fail closed");
        assert!(!crate::source_embeds_exif_gps_location(&webp(&[])));
    }

    #[test]
    fn png_and_webp() {
        let exif = sample_exif(ByteOrder::Little);
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |typ: &[u8], data: &[u8]| {
            p.extend_from_slice(&(data.len() as u32).to_be_bytes());
            p.extend_from_slice(typ);
            p.extend_from_slice(data);
            p.extend_from_slice(&[0; 4]);
        };
        chunk(b"eXIf", &exif);
        let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
        itxt.extend(crate::write_xmp(&crate::Metadata { label: Some("Red".into()), ..Default::default() }, None).into_bytes());
        chunk(b"iTXt", &itxt);
        chunk(b"IEND", &[]);
        let m = crate::extract(&p);
        assert_eq!(m.iso, Some(400));
        assert_eq!(m.label.as_deref(), Some("Red"));

        let mut w = b"RIFF\0\0\0\0WEBP".to_vec();
        let mut body = |id: &[u8], data: &[u8]| {
            w.extend_from_slice(id);
            w.extend_from_slice(&(data.len() as u32).to_le_bytes());
            w.extend_from_slice(data);
            if data.len() % 2 == 1 {
                w.push(0);
            }
        };
        body(b"VP8X", &[0; 10]);
        body(b"ICCP", b"abc");
        let mut ex = b"Exif\0\0".to_vec();
        ex.extend_from_slice(&exif);
        body(b"EXIF", &ex);
        let e = webp_chunks(&w);
        assert_eq!(e.icc.as_deref(), Some(&b"abc"[..]));
        assert_eq!(crate::extract(&w).f_number, Some(2.8));
    }

    #[test]
    fn tiff_file_is_its_own_exif() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let t = sample_exif(ByteOrder::Big);
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("No GPS camera".into()));
        let without_gps = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
        assert_eq!(crate::extract(&t).model.as_deref(), Some("Model X"));
        assert!(crate::source_embeds_exif_gps_location(&t), "whole-file TIFF with GPS must be location-bearing");
        assert!(!crate::source_embeds_exif_gps_location(&without_gps), "whole-file TIFF without GPS is safe");
        assert_eq!(embedded(b"not an image"), Embedded::default());
    }

    #[test]
    fn raf_reads_its_embedded_jpeg() {
        let j = sample_jpeg();
        let mut raf = b"FUJIFILMCCD-RAW 0201FF000000TEST".to_vec();
        raf.resize(120, 0);
        raf[84..88].copy_from_slice(&120u32.to_be_bytes());
        raf[88..92].copy_from_slice(&(j.len() as u32).to_be_bytes());
        raf.extend_from_slice(&j);
        assert_eq!(crate::extract(&raf).iso, Some(400));
        for n in 0..raf.len() {
            let _ = crate::extract(&raf[..n]);
        }
    }

    /// Build a RAF preview with a GPS-free EXIF APP1 followed by a GPS-bearing one.
    fn raf_with_later_gps_exif() -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let exif = |with_gps: bool| {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii("Procedural camera".into()));
            if with_gps {
                let mut gps = IfdBuilder::new();
                gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
                gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
                gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("W".into()));
                gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(10, 1), (30, 1), (0, 1)]));
                ifd0.set_child(t::GPS_IFD, gps);
            }
            TiffWriter::default().write(&[ifd0]).unwrap()
        };
        let mut jpeg = vec![0xff, 0xd8];
        for block in [exif(false), exif(true)] {
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend_from_slice(&block);
            let length = u16::try_from(payload.len().saturating_add(2)).unwrap();
            jpeg.extend_from_slice(&[0xff, 0xe1]);
            jpeg.extend_from_slice(&length.to_be_bytes());
            jpeg.extend_from_slice(&payload);
        }
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9]);

        let mut raf = b"FUJIFILMCCD-RAW 0201FF000000TEST".to_vec();
        raf.resize(120, 0);
        raf[84..88].copy_from_slice(&120u32.to_be_bytes());
        raf[88..92].copy_from_slice(&u32::try_from(jpeg.len()).unwrap().to_be_bytes());
        raf.extend_from_slice(&jpeg);
        raf
    }

    /// The privacy scan and metadata projection include GPS in later RAF preview EXIF packets.
    #[test]
    fn raf_checks_every_embedded_jpeg_exif_app1_for_gps() {
        let raf = raf_with_later_gps_exif();
        assert!(crate::source_embeds_exif_gps_location(&raf));
        let preview = raf_embedded_jpeg(&raf).unwrap();
        assert_eq!(crate::extract(preview).gps, Some(crate::Gps { latitude: 45.5, longitude: -10.5, altitude: None }));
        assert_eq!(crate::try_extract(&raf).unwrap().gps, Some(crate::Gps { latitude: 45.5, longitude: -10.5, altitude: None }));
    }

    #[test]
    fn truncated_containers_do_not_panic() {
        let j = sample_jpeg();
        for n in 0..j.len() {
            let _ = crate::extract(&j[..n]);
        }
    }
}
