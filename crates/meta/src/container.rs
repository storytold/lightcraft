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
pub(crate) fn png_webp_embeds_exif_gps_location(bytes: &[u8], budget: &mut usize) -> Option<bool> {
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
            if kind == b"eXIf" && crate::exif::exif_gps_tag_presence_with_budget(data, 0, budget) != Some(false) {
                return Some(true);
            }
            if kind == b"tEXt" || kind == b"zTXt" || kind == b"iTXt" {
                let Some(keyword_end) = data.iter().position(|byte| *byte == 0) else { return Some(true) };
                let Some(keyword) = data.get(..keyword_end) else { return Some(true) };
                if keyword.starts_with(b"Raw profile type") {
                    // PNG raw-profile text can carry embedded EXIF data; this scanner does not decode it.
                    return Some(true);
                }
                if keyword == b"XML:com.adobe.xmp" {
                    if kind != b"iTXt" {
                        // tEXt is not the standard XMP encoding and zTXt requires decompression.
                        return Some(true);
                    }
                    match png_xmp_packet(data) {
                        Some(Some(packet)) => {
                            if !consume_gps_scan_work(budget, packet.len()) || xmp_packet_has_location(packet) != Some(false) {
                                return Some(true);
                            }
                        }
                        Some(None) => {}
                        None => return Some(true),
                    }
                }
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
            if kind == b"EXIF" && crate::exif::exif_gps_tag_presence_with_budget(data, 0, budget) != Some(false) {
                return Some(true);
            }
            if kind == b"XMP " && (!consume_gps_scan_work(budget, data.len()) || xmp_packet_has_location(data) != Some(false)) {
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

/// A bounded marker walk used by privacy checks. Entropy-coded bytes are skipped while honoring
/// byte stuffing and restart markers, so APP segments between progressive scans remain visible.
fn walk_jpeg_segments<'a>(b: &'a [u8], budget: &mut usize, allow_padding: bool, mut visit: impl FnMut(u8, &'a [u8]) -> bool) -> bool {
    fn charge(budget: &mut usize, amount: usize) -> bool {
        let Some(remaining) = budget.checked_sub(amount) else {
            *budget = 0;
            return false;
        };
        *budget = remaining;
        true
    }

    if !b.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut at = 2usize;
    let mut in_scan = false;
    loop {
        let marker_start = at;
        let marker_at = if in_scan {
            loop {
                let Some(&byte) = b.get(at) else { return false };
                if byte != 0xff {
                    at = at.saturating_add(1);
                    if !charge(budget, 1) {
                        return false;
                    }
                    continue;
                }
                let run_start = at;
                while b.get(at) == Some(&0xff) {
                    at = at.saturating_add(1);
                }
                let Some(&code) = b.get(at) else { return false };
                if code == 0x00 || (0xd0..=0xd7).contains(&code) {
                    at = at.saturating_add(1);
                    if !charge(budget, at.saturating_sub(run_start)) {
                        return false;
                    }
                    continue;
                }
                in_scan = false;
                break at;
            }
        } else {
            if b.get(at) != Some(&0xff) {
                return false;
            }
            while b.get(at) == Some(&0xff) {
                at = at.saturating_add(1);
            }
            at
        };
        let Some(&marker) = b.get(marker_at) else { return false };
        let Some(after_marker) = marker_at.checked_add(1) else { return false };
        at = after_marker;
        if marker == 0xd9 {
            let Some(padding) = b.get(at..) else { return false };
            if !charge(budget, at.saturating_sub(marker_start)) {
                return false;
            }
            if padding.is_empty() {
                return true;
            }
            if !allow_padding || !charge(budget, padding.len()) {
                return false;
            }
            return padding.iter().all(|byte| matches!(byte, 0x00 | 0xff));
        }
        if marker == 0x00 {
            return false;
        }
        if marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            if !charge(budget, at.saturating_sub(marker_start)) {
                return false;
            }
            continue;
        }
        let Some(length_end) = at.checked_add(2) else { return false };
        let Some(length) = b.get(at..length_end) else { return false };
        let segment_length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        if segment_length < 2 {
            return false;
        }
        let Some(segment_end) = at.checked_add(segment_length) else { return false };
        if segment_end > b.len() {
            return false;
        }
        let payload_at = length_end;
        let Some(payload) = b.get(payload_at..segment_end) else { return false };
        if !charge(budget, segment_end.saturating_sub(marker_start)) || !visit(marker, payload) {
            return false;
        }
        at = segment_end;
        if marker == 0xda {
            in_scan = true;
        }
    }
}

/// Charge work against a shared scan allowance, returning false when it would be exceeded.
pub(crate) fn consume_gps_scan_work(budget: &mut usize, amount: usize) -> bool {
    let Some(remaining) = budget.checked_sub(amount) else {
        *budget = 0;
        return false;
    };
    *budget = remaining;
    true
}

/// Initial work allowance for a complete source scan and its nested previews.
pub(crate) fn gps_scan_budget(input_len: usize) -> usize {
    input_len.saturating_mul(4).saturating_add(1 << 20)
}

/// Check JPEG EXIF and XMP packets, including those between progressive scans. Truncated or
/// unparseable packets, MPF secondary images, and an exhausted nested-work budget fail closed.
/// Scan a nested JPEG while sharing the source's byte-work budget and recursion limit.
pub(crate) fn jpeg_embeds_location_at_depth(bytes: &[u8], depth: usize, budget: &mut usize) -> bool {
    jpeg_embeds_location_at_depth_with_padding(bytes, depth, budget, false)
}

/// Scan a JPEG stored inside another container, allowing only zero or 0xff bytes after its EOI.
pub(crate) fn jpeg_embeds_location_in_preview(bytes: &[u8], depth: usize, budget: &mut usize) -> bool {
    jpeg_embeds_location_at_depth_with_padding(bytes, depth, budget, true)
}

fn jpeg_embeds_location_at_depth_with_padding(bytes: &[u8], depth: usize, budget: &mut usize, allow_padding: bool) -> bool {
    if depth >= crate::exif::MAX_GPS_NESTING {
        return true;
    }
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let (mut has_location, mut exif_packets, mut xmp_packets, mut extended) =
        (false, Vec::<&[u8]>::new(), Vec::<&[u8]>::new(), Vec::<(u32, u32, [u8; 32], &[u8])>::new());
    let complete = walk_jpeg_segments(bytes, budget, allow_padding, |marker, payload| {
        if marker == 0xe2 && payload.starts_with(b"MPF\0") {
            has_location = true;
            return false;
        }
        if marker == 0xed && payload.starts_with(b"Photoshop 3.0\0") {
            let Some(resources) = payload.get(14..) else {
                has_location = true;
                return false;
            };
            if photoshop_iptc_embeds_location(resources) != Some(false) {
                has_location = true;
                return false;
            }
        }
        if marker == 0xe1 {
            if payload.starts_with(b"Exif\0") {
                exif_packets.push(payload);
            } else if payload.starts_with(XMP_SIG) {
                xmp_packets.push(payload);
            } else if let Some(packet) = payload.strip_prefix(XMP_EXT_SIG) {
                let (Some(guid), Some(total), Some(offset), Some(chunk)) = (
                    packet.get(..32),
                    packet.get(32..36).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]])),
                    packet.get(36..40).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]])),
                    packet.get(40..),
                ) else {
                    has_location = true;
                    return false;
                };
                let mut id = [0u8; 32];
                id.copy_from_slice(guid);
                let Ok(chunk_len) = u32::try_from(chunk.len()) else {
                    has_location = true;
                    return false;
                };
                if total == 0 || offset.checked_add(chunk_len).is_none_or(|end| end > total) {
                    has_location = true;
                    return false;
                }
                extended.push((total, offset, id, chunk));
            }
        }
        true
    });
    if !complete || has_location {
        return true;
    }
    for payload in exif_packets {
        let Some(exif) = payload.get(6..) else { return true };
        if crate::exif::exif_gps_tag_presence_with_budget(exif, depth.saturating_add(1), budget) != Some(false) {
            return true;
        }
    }
    for payload in xmp_packets {
        let Some(packet) = payload.strip_prefix(XMP_SIG) else { return true };
        if !consume_gps_scan_work(budget, packet.len()) {
            return true;
        }
        if xmp_packet_has_location(packet) != Some(false) {
            return true;
        }
    }
    if extended.is_empty() {
        return false;
    }
    extended.sort_by_key(|part| part.1);
    let Some((total, _, id, _)) = extended.first() else { return true };
    let Ok(total) = usize::try_from(*total) else { return true };
    if total > bytes.len() || extended.iter().any(|(len, _, guid, _)| len != &(total as u32) || guid != id) {
        return true;
    }
    if !consume_gps_scan_work(budget, total) {
        return true;
    }
    let mut packet = Vec::with_capacity(total);
    for (declared, offset, _, chunk) in extended {
        if usize::try_from(offset).ok() != Some(packet.len()) || packet.len().checked_add(chunk.len()).is_none_or(|end| end > total) {
            return true;
        }
        packet.extend_from_slice(chunk);
        if usize::try_from(declared).ok() != Some(total) {
            return true;
        }
    }
    packet.len() != total || xmp_packet_has_location(&packet) != Some(false)
}

/// Whether an XMP packet contains a GPS property or place name; malformed packets are unverifiable.
pub(crate) fn xmp_packet_has_location(packet: &[u8]) -> Option<bool> {
    let packet = std::str::from_utf8(packet).ok()?;
    let (xmp, properties_truncated, structured_location) = crate::xmp::parse_xmp_for_privacy(packet).ok()?;
    if properties_truncated {
        return Some(true);
    }
    let metadata = xmp.metadata;
    let raw_gps = xmp
        .properties
        .iter()
        .any(|(key, values)| (key.starts_with("exif:GPS") || key.starts_with("exifEX:GPS")) && values.iter().any(|value| !value.trim().is_empty()));
    let country_code = xmp.properties.get("Iptc4xmpCore:CountryCode").is_some_and(|values| values.iter().any(|value| !value.trim().is_empty()));
    let parsed_structured_location =
        ["Iptc4xmpExt:LocationCreated", "Iptc4xmpExt:LocationShown"].iter().filter_map(|key| xmp.values.get(*key)).any(xmp_value_has_content);
    Some(
        metadata.gps.is_some()
            || raw_gps
            || country_code
            || structured_location
            || parsed_structured_location
            || [metadata.sublocation, metadata.city, metadata.state, metadata.country].into_iter().flatten().any(|value| !value.trim().is_empty()),
    )
}

fn xmp_value_has_content(value: &crate::XmpValue) -> bool {
    match value {
        crate::XmpValue::Text(text) => !text.trim().is_empty(),
        crate::XmpValue::Array(items) => items.iter().any(xmp_value_has_content),
        crate::XmpValue::Struct(fields) => fields.values().any(xmp_value_has_content),
    }
}

/// Return an XMP packet from an uncompressed PNG iTXt record. `None` means the record is malformed
/// or compressed; callers that are checking privacy must treat that as unverifiable.
fn png_xmp_packet(data: &[u8]) -> Option<Option<&[u8]>> {
    let keyword_end = data.iter().position(|byte| *byte == 0)?;
    if data.get(..keyword_end)? != b"XML:com.adobe.xmp" {
        return Some(None);
    }
    let fields = data.get(keyword_end.checked_add(1)?..)?;
    let compression_flag = *fields.first()?;
    let compression_method = *fields.get(1)?;
    if compression_flag != 0 || compression_method != 0 {
        return None;
    }
    let text_fields = fields.get(2..)?;
    let language_end = text_fields.iter().position(|byte| *byte == 0)?;
    let translated = text_fields.get(language_end.checked_add(1)?..)?;
    let translated_end = translated.iter().position(|byte| *byte == 0)?;
    Some(Some(translated.get(translated_end.checked_add(1)?..)?))
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

/// Check all IPTC image resources in a Photoshop APP13 payload. `None` means the resource stream
/// or an IPTC-IIM block is malformed, so a location-removal guard must fail closed.
fn photoshop_iptc_embeds_location(mut b: &[u8]) -> Option<bool> {
    while !b.is_empty() {
        if b.get(..4) != Some(b"8BIM") {
            return None;
        }
        let id = u16::from_be_bytes([*b.get(4)?, *b.get(5)?]);
        let name_len = usize::from(*b.get(6)?);
        let name_size = 1usize.checked_add(name_len)?.checked_add(1)? & !1;
        let size_at = 6usize.checked_add(name_size)?;
        let size_end = size_at.checked_add(4)?;
        let size = usize::try_from(u32::from_be_bytes(b.get(size_at..size_end)?.try_into().ok()?)).ok()?;
        let data_at = size_end;
        let data_end = data_at.checked_add(size)?;
        let data = b.get(data_at..data_end)?;
        if id == 0x0404 && crate::iptc::contains_place_name(data)? {
            return Some(true);
        }
        let padded_size = size.checked_add(1)? & !1;
        b = b.get(data_at.checked_add(padded_size)?..)?;
    }
    Some(false)
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

    /// A clean first APP1 does not change the first valid GPS position's precedence in JPEG or RAF.
    #[test]
    fn jpeg_and_raf_keep_the_first_valid_exif_position() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let exif = |latitude: Option<u32>| {
            let mut ifd0 = IfdBuilder::new();
            if let Some(latitude) = latitude {
                let mut gps = IfdBuilder::new();
                gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
                gps.set(t::GPS_LATITUDE, Value::Rational(vec![(latitude, 1), (30, 1), (0, 1)]));
                gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
                gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(10, 1), (30, 1), (0, 1)]));
                ifd0.set_child(t::GPS_IFD, gps);
            }
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend(TiffWriter::default().write(&[ifd0]).unwrap());
            payload
        };
        let mut jpeg = vec![0xff, 0xd8];
        for payload in [exif(None), exif(Some(45)), exif(Some(12))] {
            jpeg.extend(seg(0xe1, &payload));
        }
        jpeg.extend([0xff, 0xd9]);

        let expected = Some(crate::Gps { latitude: 45.5, longitude: 10.5, altitude: None });
        assert_eq!(source_jpeg_exif_gps(&jpeg), expected);
        assert_eq!(crate::try_extract(&jpeg).unwrap().gps, expected);

        let offset = 96usize;
        let mut raf = vec![0u8; offset];
        let magic = b"FUJIFILMCCD-RAW";
        raf.get_mut(..magic.len()).unwrap().copy_from_slice(magic);
        raf.get_mut(84..88).unwrap().copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        raf.get_mut(88..92).unwrap().copy_from_slice(&u32::try_from(jpeg.len()).unwrap().to_be_bytes());
        raf.extend_from_slice(&jpeg);
        assert_eq!(source_jpeg_exif_gps(&raf), expected);
        assert_eq!(crate::try_extract(&raf).unwrap().gps, expected);
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

        let truncated_png = b"\x89PNG\r\n\x1a\n\0\0\0\x08eXIf".to_vec();
        assert!(crate::source_embeds_exif_gps_location(&truncated_png), "truncated PNG chunk fails closed");
        let mut missing_iend = png(&[exif(false)]);
        missing_iend.truncate(missing_iend.len().saturating_sub(12));
        assert!(crate::source_embeds_exif_gps_location(&missing_iend), "missing PNG IEND fails closed");
        let mut invalid_iend = png(&[exif(false)]);
        let iend_length = invalid_iend.len().saturating_sub(12);
        invalid_iend[iend_length..iend_length + 4].copy_from_slice(&1u32.to_be_bytes());
        assert!(crate::source_embeds_exif_gps_location(&invalid_iend), "nonempty PNG IEND fails closed");
        let mut trailing_png = png(&[exif(false)]);
        trailing_png.push(0);
        assert!(crate::source_embeds_exif_gps_location(&trailing_png), "data after PNG IEND fails closed");

        let mut riff_mismatch = webp(&[exif(false)]);
        let declared_riff_size = u32::from_le_bytes(riff_mismatch[4..8].try_into().unwrap_or([0; 4]));
        riff_mismatch[4..8].copy_from_slice(&declared_riff_size.saturating_add(1).to_le_bytes());
        assert!(crate::source_embeds_exif_gps_location(&riff_mismatch), "WebP RIFF size mismatch fails closed");
        let mut odd_chunk_without_padding = b"RIFF\0\0\0\0WEBPEXIF\x01\0\0\0x".to_vec();
        let riff_size = u32::try_from(odd_chunk_without_padding.len().saturating_sub(8)).unwrap_or(u32::MAX);
        odd_chunk_without_padding[4..8].copy_from_slice(&riff_size.to_le_bytes());
        assert!(crate::source_embeds_exif_gps_location(&odd_chunk_without_padding), "missing odd-size WebP padding fails closed");
    }

    /// Malformed JPEG marker streams fail closed; a complete scan and EOI without GPS are safe.
    #[test]
    fn jpeg_gps_guard_fails_closed_on_malformed_segments() {
        let malformed = [
            ("truncated segment", vec![0xff, 0xd8, 0xff, 0xe1, 0, 8]),
            ("invalid segment length", vec![0xff, 0xd8, 0xff, 0xe1, 0, 1]),
            ("missing marker", vec![0xff, 0xd8, 0]),
            ("data after EOI", vec![0xff, 0xd8, 0xff, 0xd9, 0]),
        ];
        for (name, bytes) in malformed {
            assert!(crate::source_embeds_exif_gps_location(&bytes), "{name} must fail closed");
        }
        let bytes = [0xff, 0xd8, 0xff, 0xda, 0, 2];
        assert!(crate::source_embeds_exif_gps_location(&bytes), "SOS without EOI must fail closed");
        for (name, bytes) in
            [("scan with EOI", vec![0xff, 0xd8, 0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9]), ("standalone EOI", vec![0xff, 0xd8, 0xff, 0xd9])]
        {
            assert!(!crate::source_embeds_exif_gps_location(&bytes), "{name} without GPS ends the scan");
        }
    }

    /// Every ordinary XMP packet in a JPEG is inspected, even when an earlier APP1 packet is clean.
    #[test]
    fn jpeg_gps_guard_checks_each_xmp_packet() {
        let packet = |metadata: crate::Metadata| {
            let xml = crate::write_xmp(&metadata, None);
            [XMP_SIG, xml.as_bytes()].concat()
        };
        let clean = packet(crate::Metadata { title: Some("No location".into()), ..Default::default() });
        let located = packet(crate::Metadata { city: Some("Cusco".into()), ..Default::default() });
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend(seg(0xe1, &clean));
        jpeg.extend(seg(0xe1, &located));
        jpeg.extend([0xff, 0xd9]);
        assert!(crate::source_embeds_exif_gps_location(&jpeg), "a later XMP APP1 with a place name must block the copy");

        let mut clean_jpeg = vec![0xff, 0xd8];
        clean_jpeg.extend(seg(0xe1, &clean));
        clean_jpeg.extend(seg(0xe1, &clean));
        clean_jpeg.extend([0xff, 0xd9]);
        assert!(!crate::source_embeds_exif_gps_location(&clean_jpeg));
    }

    /// IPTC place-name datasets in APP13 must block an unchanged Original copy.
    #[test]
    fn jpeg_gps_guard_checks_iptc_place_name_datasets() {
        let iim = |dataset: u8, value: &[u8]| {
            let mut bytes = vec![0x1c, 2, dataset];
            bytes.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(value);
            bytes
        };
        let jpeg = |iptc: &[u8]| {
            let mut photoshop = b"Photoshop 3.0\0".to_vec();
            photoshop.extend_from_slice(b"8BIM\x04\x04\0\0");
            photoshop.extend_from_slice(&u32::try_from(iptc.len()).unwrap().to_be_bytes());
            photoshop.extend_from_slice(iptc);
            if !iptc.len().is_multiple_of(2) {
                photoshop.push(0);
            }
            let mut bytes = vec![0xff, 0xd8];
            bytes.extend_from_slice(&seg(0xed, &photoshop));
            bytes.extend_from_slice(&[0xff, 0xd9]);
            bytes
        };

        for dataset in [90, 92, 95, 100, 101] {
            assert!(crate::source_embeds_exif_gps_location(&jpeg(&iim(dataset, b"Paris"))), "IPTC dataset 2:{dataset} is location-bearing");
        }
        assert!(!crate::source_embeds_exif_gps_location(&jpeg(&iim(90, b"  \0"))), "empty City does not block the copy");
        assert!(!crate::source_embeds_exif_gps_location(&jpeg(&iim(5, b"Title"))), "unrelated IPTC datasets do not block the copy");
        assert!(crate::source_embeds_exif_gps_location(&jpeg(&[0x1c, 2, 90, 0, 5, b'x'])), "truncated IPTC cannot authorize the copy");
    }

    /// A JPEG may resume with metadata markers between entropy-coded scans.
    #[test]
    fn jpeg_gps_guard_checks_metadata_between_scans() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap());

        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2]);
        jpeg.extend_from_slice(&[0x21, 0xff, 0, 0x37, 0xff, 0xd3, 0x41]);
        jpeg.extend_from_slice(&seg(0xe1, &exif));
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2, 0x42, 0xff, 0xd9]);

        assert!(crate::source_embeds_exif_gps_location(&jpeg), "GPS in an APP1 segment between scans must block the copy");
    }

    /// Nested JPEG scans consume one shared work budget and fail closed when it runs out.
    #[test]
    fn jpeg_nested_scans_share_the_work_budget() {
        let jpeg = [0xff, 0xd8, 0xff, 0xd9];
        let mut budget = 3;
        assert!(!jpeg_embeds_location_at_depth(&jpeg, 0, &mut budget));
        assert!(jpeg_embeds_location_at_depth(&jpeg, 0, &mut budget), "a second nested scan cannot reset the budget");
    }

    /// Embedded preview extents may have safe zero/0xff padding after EOI; standalone JPEGs stay strict.
    #[test]
    fn embedded_jpeg_preview_allows_only_safe_eoi_padding() {
        let padded = [0xff, 0xd8, 0xff, 0xd9, 0, 0xff];
        let mut budget = 64;
        assert!(!jpeg_embeds_location_in_preview(&padded, 1, &mut budget));

        let mut budget = 64;
        assert!(jpeg_embeds_location_at_depth(&padded, 0, &mut budget), "standalone JPEGs reject trailing data");

        let unsafe_padding = [0xff, 0xd8, 0xff, 0xd9, b'x'];
        let mut budget = 64;
        assert!(jpeg_embeds_location_in_preview(&unsafe_padding, 1, &mut budget));
        let mut budget = 3;
        assert!(jpeg_embeds_location_in_preview(&padded, 1, &mut budget), "padding bytes count against the shared work budget");
    }

    /// Every supported PNG/WebP XMP chunk participates in the Original-copy location check.
    #[test]
    fn png_webp_gps_guard_checks_each_xmp_chunk() {
        let packet = |metadata: crate::Metadata| crate::write_xmp(&metadata, None).into_bytes();
        let itxt = |packet: &[u8], compression_flag: u8| {
            let mut data = b"XML:com.adobe.xmp\0".to_vec();
            data.extend_from_slice(&[compression_flag, 0, 0, 0]);
            data.extend_from_slice(packet);
            data
        };
        let png = |packets: &[Vec<u8>]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            for packet in packets {
                bytes.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_be_bytes());
                bytes.extend_from_slice(b"iTXt");
                bytes.extend_from_slice(packet);
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes.extend_from_slice(b"IEND");
            bytes.extend_from_slice(&[0; 4]);
            bytes
        };
        let webp = |packets: &[Vec<u8>]| {
            let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
            for packet in packets {
                bytes.extend_from_slice(b"XMP ");
                bytes.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_le_bytes());
                bytes.extend_from_slice(packet);
                if packet.len() % 2 == 1 {
                    bytes.push(0);
                }
            }
            let riff_size = u32::try_from(bytes.len().saturating_sub(8)).unwrap();
            bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
            bytes
        };
        let clean = packet(crate::Metadata { title: Some("No location".into()), ..Default::default() });
        let located = packet(crate::Metadata { city: Some("Cusco".into()), ..Default::default() });
        let clean_png = png(&[itxt(&clean, 0)]);
        let located_png = png(&[itxt(&clean, 0), itxt(&located, 0)]);
        let clean_webp = webp(std::slice::from_ref(&clean));
        let located_webp = webp(&[clean.clone(), located.clone()]);
        assert!(!crate::source_embeds_exif_gps_location(&clean_png));
        assert!(!crate::source_embeds_exif_gps_location(&clean_webp));
        assert!(crate::source_embeds_exif_gps_location(&located_png), "a later PNG XMP packet with a place name blocks the copy");
        assert!(crate::source_embeds_exif_gps_location(&located_webp), "a later WebP XMP packet with a place name blocks the copy");

        assert!(crate::source_embeds_exif_gps_location(&png(&[itxt(&located, 1)])), "compressed PNG XMP cannot be verified");
        assert!(crate::source_embeds_exif_gps_location(&webp(&[vec![0xff, 0xfe]])), "malformed WebP XMP cannot be verified");
    }

    #[test]
    fn png_gps_guard_fails_closed_on_uninspected_text_profiles() {
        let png = |kind: &[u8; 4], data: &[u8]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            let chunks: [(&[u8], &[u8]); 2] = [(kind.as_slice(), data), (&b"IEND"[..], &b""[..])];
            for (chunk_kind, chunk_data) in chunks {
                bytes.extend_from_slice(&u32::try_from(chunk_data.len()).unwrap_or(u32::MAX).to_be_bytes());
                bytes.extend_from_slice(chunk_kind);
                bytes.extend_from_slice(chunk_data);
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes
        };

        let gps_xmp = crate::write_xmp(&crate::Metadata { city: Some("Cusco".into()), ..Default::default() }, None);
        let xmp_text = [&b"XML:com.adobe.xmp\0"[..], gps_xmp.as_bytes()].concat();
        let raw_profile = b"Raw profile type exif\0compressed profile data";
        let app1_profile = b"Raw profile type APP1\0compressed profile data";
        let compressed_profile = b"Raw profile type exif\0\0\0compressed profile data";
        let clean_text = b"Comment\0ordinary text";

        for (kind, data, name) in [
            (b"tEXt", raw_profile.as_slice(), "tEXt raw profile"),
            (b"tEXt", app1_profile.as_slice(), "tEXt APP1 profile"),
            (b"zTXt", compressed_profile.as_slice(), "zTXt raw profile"),
            (b"iTXt", raw_profile.as_slice(), "iTXt raw profile"),
            (b"tEXt", xmp_text.as_slice(), "tEXt XMP"),
            (b"zTXt", xmp_text.as_slice(), "zTXt XMP"),
        ] {
            assert!(crate::source_embeds_exif_gps_location(&png(kind, data)), "{name} cannot authorize an unchanged copy");
        }
        assert!(!crate::source_embeds_exif_gps_location(&png(b"tEXt", clean_text)), "unrelated text remains safe");
    }

    /// A standalone or destination GPS XMP property is location-bearing without a full coordinate pair.
    #[test]
    fn xmp_location_check_catches_standalone_gps_properties() {
        for property in ["GPSLatitude", "GPSDestLatitude", "GPSDestLongitude", "GPSAltitude"] {
            let packet = format!(
                "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\"><rdf:RDF><rdf:Description exif:{property}=\"45, 0, 0\"/></rdf:RDF></x:xmpmeta>"
            );
            assert_eq!(xmp_packet_has_location(packet.as_bytes()), Some(true), "{property}");
        }
    }

    #[test]
    fn xmp_location_check_catches_country_codes_and_structured_places() {
        let country_code = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:iptc="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"><rdf:RDF><rdf:Description iptc:CountryCode="FR"/></rdf:RDF></x:xmpmeta>"#;
        assert_eq!(xmp_packet_has_location(country_code.as_bytes()), Some(true));

        for field in ["LocationCreated", "LocationShown"] {
            let packet = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ext="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"><rdf:RDF><rdf:Description><ext:{field}><rdf:Seq><rdf:li rdf:parseType="Resource"><ext:City>Paris</ext:City><ext:CountryCode>FR</ext:CountryCode></rdf:li></rdf:Seq></ext:{field}></rdf:Description></rdf:RDF></x:xmpmeta>"#
            );
            let parsed = crate::parse_xmp(&packet).expect("valid structured XMP");
            assert!(parsed.values.contains_key(&format!("Iptc4xmpExt:{field}")), "the extension namespace is canonicalized");
            assert_eq!(xmp_packet_has_location(packet.as_bytes()), Some(true), "{field} array entries are location-bearing");
        }

        let empty = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ext="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"><rdf:RDF><rdf:Description><ext:LocationCreated><rdf:Seq><rdf:li rdf:parseType="Resource"/></rdf:Seq></ext:LocationCreated></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        assert_eq!(xmp_packet_has_location(empty.as_bytes()), Some(false), "an empty location structure has no identifying data");
    }

    #[test]
    fn xmp_location_check_keeps_earlier_structured_locations_and_rejects_truncated_keys() {
        let duplicate = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ext="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"><rdf:RDF><rdf:Description><ext:LocationShown><rdf:Seq><rdf:li rdf:parseType="Resource"><ext:City>Paris</ext:City></rdf:li></rdf:Seq></ext:LocationShown><ext:LocationShown><rdf:Seq><rdf:li rdf:parseType="Resource"/></rdf:Seq></ext:LocationShown></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        assert_eq!(xmp_packet_has_location(duplicate.as_bytes()), Some(true), "a later empty occurrence must not hide an earlier place");

        let mut packet = String::from(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:lc="http://ns.lightcraft.app/lc/1.0/" xmlns:exif="http://ns.adobe.com/exif/1.0/"><rdf:RDF><rdf:Description>"#,
        );
        let suffix = "x".repeat(500);
        for index in 0..2_200 {
            let name = format!("lc:p{index:04}{suffix}");
            packet.push_str(&format!("<{name}>x</{name}>"));
        }
        let final_name = format!("lc:{}", "y".repeat(50));
        packet.push_str(&format!("<{final_name}>x</{final_name}>"));
        packet.push_str("<exif:GPSDestLatitude>45, 0, 0</exif:GPSDestLatitude></rdf:Description></rdf:RDF></x:xmpmeta>");
        let parsed = crate::parse_xmp(&packet).expect("the bounded property collection keeps parsing");
        assert!(!parsed.properties.contains_key("exif:GPSDestLatitude"), "the fixture exhausts the flattened-key budget before GPS");
        assert_eq!(xmp_packet_has_location(packet.as_bytes()), Some(true), "an omitted property cannot establish privacy-safe absence");
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
        let t = sample_exif(ByteOrder::Big);
        assert_eq!(crate::extract(&t).model.as_deref(), Some("Model X"));
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

    /// RAF preview padding is accepted only after a complete JPEG EOI and only for safe pad bytes.
    #[test]
    fn raf_preview_padding_does_not_block_a_gps_free_original_copy() {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xda, 0, 2, 0x11, 0xff, 0xd9];
        jpeg.extend_from_slice(&[0, 0xff]);
        let mut raf = b"FUJIFILMCCD-RAW 0201FF000000TEST".to_vec();
        raf.resize(120, 0);
        raf[84..88].copy_from_slice(&120u32.to_be_bytes());
        raf[88..92].copy_from_slice(&u32::try_from(jpeg.len()).unwrap().to_be_bytes());
        raf.extend_from_slice(&jpeg);

        assert!(!crate::source_embeds_exif_gps_location(&raf));
        assert!(crate::source_embeds_exif_gps_location(&jpeg), "standalone JPEG trailing bytes remain fail-closed");
    }

    #[test]
    fn truncated_containers_do_not_panic() {
        let j = sample_jpeg();
        for n in 0..j.len() {
            let _ = crate::extract(&j[..n]);
        }
    }
}
