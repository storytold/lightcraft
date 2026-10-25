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

use lightcraft_tiff::{Tiff, tags};

/// Metadata blocks found in a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Embedded {
    /// TIFF-structured Exif block (without the `Exif\0\0` prefix).
    pub exif: Option<Vec<u8>>,
    /// Standard XMP packet.
    pub xmp: Option<String>,
    /// Referenced Extended XMP packet (JPEG only), reassembled after chunk validation.
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
        return raf_jpeg(bytes).map(jpeg_segments).unwrap_or_default();
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

/// Return the JPEG payload embedded in a Fujifilm RAF, if its offsets are readable.
pub(crate) fn raf_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let read_be_u32 = |offset: usize| {
        let end = offset.checked_add(4)?;
        let encoded: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
        Some(u32::from_be_bytes(encoded))
    };
    let offset = usize::try_from(read_be_u32(84)?).ok()?;
    let length = usize::try_from(read_be_u32(88)?).ok()?;
    if offset == 0 {
        return None;
    }
    bytes.get(offset..offset.saturating_add(length).min(bytes.len()))
}

/// Locate metadata blocks while preserving PSD/PSB resource-structure errors for callers that
/// must establish whether embedded GPS is absent. Other containers keep their best-effort walk.
pub(crate) fn try_embedded(bytes: &[u8]) -> Result<Embedded, crate::ExtractError> {
    if bytes.starts_with(b"8BPS") { crate::psd::parse(bytes).ok_or(crate::ExtractError::MalformedPsd) } else { Ok(embedded(bytes)) }
}

const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const MAX_EXTENDED_XMP_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXTENDED_XMP_CHUNKS: usize = 4096;

#[derive(Clone, Debug)]
pub(crate) struct ExtendedXmpChunk {
    guid: String,
    total_length: u32,
    offset: u32,
    data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct JpegXmpPackets {
    /// Whether any Extended XMP APP1 marker was present, used to reject unreferenced extensions.
    pub(crate) saw_extension: bool,
    /// Validation errors for the referenced GUID selected by the caller.
    pub(crate) extension_error: Option<String>,
    pub(crate) chunks: Vec<ExtendedXmpChunk>,
    received_bytes: usize,
}

impl JpegXmpPackets {
    pub(crate) fn assemble(&self, expected_guid: &str) -> Result<String, String> {
        if let Some(error) = &self.extension_error {
            return Err(error.clone());
        }
        let mut chunks: Vec<&ExtendedXmpChunk> = self.chunks.iter().filter(|chunk| chunk.guid == expected_guid).collect();
        if chunks.is_empty() {
            return Err(format!("no Extended XMP chunks match GUID {expected_guid}"));
        }
        if chunks.len() > MAX_EXTENDED_XMP_CHUNKS {
            return Err("Extended XMP has too many chunks".into());
        }
        let Some(total_length) = chunks.first().map(|chunk| chunk.total_length) else {
            return Err("Extended XMP has no chunks".into());
        };
        let total = usize::try_from(total_length).map_err(|_| "Extended XMP length is too large")?;
        if total == 0 || total > MAX_EXTENDED_XMP_BYTES {
            return Err("Extended XMP length exceeds the 16 MiB limit".into());
        }
        if chunks.iter().any(|chunk| chunk.total_length != total_length) {
            return Err("Extended XMP chunks declare different full lengths".into());
        }
        chunks.sort_by_key(|chunk| chunk.offset);

        let mut assembled = Vec::new();
        let mut next_offset = 0u32;
        for chunk in chunks {
            if chunk.offset != next_offset {
                return Err("Extended XMP chunks have a gap, overlap or duplicate offset".into());
            }
            let part_length = u32::try_from(chunk.data.len()).map_err(|_| "Extended XMP chunk is too large")?;
            next_offset = chunk.offset.checked_add(part_length).ok_or("Extended XMP chunk offset overflow")?;
            if next_offset > total_length {
                return Err("Extended XMP chunks exceed their declared full length".into());
            }
            assembled.extend_from_slice(&chunk.data);
        }
        if next_offset != total_length || assembled.len() != total {
            return Err("Extended XMP chunks do not cover the declared full length".into());
        }
        String::from_utf8(assembled).map_err(|_| "Extended XMP packet is not UTF-8".into())
    }
}

/// Walk JPEG markers up to SOS and collect APP1 Exif / XMP, APP2 ICC and APP13 IPTC.
pub fn jpeg_segments(b: &[u8]) -> Embedded {
    crate::jpeg_embedded(b).0
}

/// Collect JPEG metadata and, when supplied, only validate and retain chunks for the referenced GUID.
/// Passing `None` discovers the standard packet and extension presence without accumulating chunks,
/// allowing the caller to apply chunk and byte limits only after resolving the reference.
pub(crate) fn jpeg_segments_with_packets(b: &[u8], expected_guid: Option<&str>) -> (Embedded, JpegXmpPackets) {
    let mut out = Embedded::default();
    if !b.starts_with(&[0xff, 0xd8]) {
        return (out, JpegXmpPackets::default());
    }
    let mut icc_parts: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut packets = JpegXmpPackets::default();
    let mut i = 2usize;
    while let Some(header_end) = i.checked_add(4).filter(|end| *end <= b.len()) {
        let Some(header) = b.get(i..header_end) else { break };
        if header.first() != Some(&0xff) {
            break;
        }
        let Some(marker) = header.get(1).copied() else { break };
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
        let Some(length_bytes) = header.get(2..4) else { break };
        let len = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if len < 2 {
            break;
        }
        let Some(segment_end) = i.checked_add(2).and_then(|start| start.checked_add(len)) else { break };
        let payload_start = header_end;
        let available_end = segment_end.min(b.len());
        let Some(payload) = b.get(payload_start..available_end) else { break };
        let complete = segment_end <= b.len();
        match marker {
            0xe1 if expected_guid.is_none() && payload.starts_with(b"Exif\0") && out.exif.is_none() => {
                out.exif = Some(payload.get(6..).unwrap_or(&[]).to_vec());
            }
            0xe1 if expected_guid.is_none() && payload.starts_with(XMP_SIG) && out.xmp.is_none() => {
                out.xmp = Some(String::from_utf8_lossy(payload.get(XMP_SIG.len()..).unwrap_or(&[])).into_owned());
            }
            0xe1 if payload.starts_with(XMP_EXT_SIG) => {
                packets.saw_extension = true;
                if let Some(expected_guid) = expected_guid.filter(|_| packets.extension_error.is_none()) {
                    match parse_extended_xmp_chunk(payload.get(XMP_EXT_SIG.len()..), complete, expected_guid) {
                        Ok(Some(chunk)) => {
                            let Some(received) = packets.received_bytes.checked_add(chunk.data.len()) else {
                                packets.extension_error = Some("Extended XMP received length overflow".into());
                                i = segment_end;
                                continue;
                            };
                            if usize::try_from(chunk.total_length).map_or(true, |length| length > MAX_EXTENDED_XMP_BYTES) {
                                packets.extension_error = Some("Extended XMP length exceeds the 16 MiB limit".into());
                            } else if packets.chunks.len() >= MAX_EXTENDED_XMP_CHUNKS {
                                packets.extension_error = Some("Extended XMP has too many chunks".into());
                            } else if received > MAX_EXTENDED_XMP_BYTES {
                                packets.extension_error = Some("Extended XMP received data exceeds the 16 MiB limit".into());
                            } else {
                                packets.received_bytes = received;
                                packets.chunks.push(chunk);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => packets.extension_error = Some(error),
                    }
                }
            }
            0xe2 if expected_guid.is_none() && payload.starts_with(b"ICC_PROFILE\0") && payload.len() >= 14 => {
                if let (Some(sequence), Some(data)) = (payload.get(12), payload.get(14..)) {
                    icc_parts.push((*sequence, data.to_vec()));
                }
            }
            0xed if expected_guid.is_none() && payload.starts_with(b"Photoshop 3.0\0") && out.iptc.is_none() => {
                out.iptc = photoshop_iptc(payload.get(14..).unwrap_or(&[]));
            }
            _ => {}
        }
        if !complete {
            break;
        }
        i = segment_end;
    }
    if !icc_parts.is_empty() {
        icc_parts.sort_by_key(|p| p.0);
        out.icc = Some(icc_parts.into_iter().flat_map(|p| p.1).collect());
    }
    (out, packets)
}

fn normalized_guid(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 32 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(bytes.iter().map(|byte| char::from(*byte).to_ascii_uppercase()).collect())
}

fn parse_extended_xmp_chunk(payload: Option<&[u8]>, complete: bool, expected_guid: &str) -> Result<Option<ExtendedXmpChunk>, String> {
    let Some(payload) = payload else { return Ok(None) };
    let Some(guid_bytes) = payload.get(..32) else { return Ok(None) };
    let Some(guid) = normalized_guid(guid_bytes) else { return Ok(None) };
    if guid != expected_guid {
        return Ok(None);
    }
    if !complete {
        return Err("Extended XMP APP1 segment is truncated".into());
    }
    if payload.len() < 40 {
        return Err("Extended XMP APP1 header is truncated".into());
    }
    let length_bytes = payload.get(32..36).and_then(|bytes| <[u8; 4]>::try_from(bytes).ok()).ok_or("Extended XMP full length is truncated")?;
    let offset_bytes = payload.get(36..40).and_then(|bytes| <[u8; 4]>::try_from(bytes).ok()).ok_or("Extended XMP offset is truncated")?;
    let data = payload.get(40..).ok_or("Extended XMP data is truncated")?;
    Ok(Some(ExtendedXmpChunk { guid, total_length: u32::from_be_bytes(length_bytes), offset: u32::from_be_bytes(offset_bytes), data: data.to_vec() }))
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
    fn jpeg_extended_xmp() {
        let guid = "A1234567890123456789012345678901";
        let extension = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" dc:title="Extended title"/></rdf:RDF></x:xmpmeta>"#;
        let standard = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:note="http://ns.adobe.com/xmp/note/" note:HasExtendedXMP="{guid}"/></rdf:RDF></x:xmpmeta>"#
        );
        let mut j = vec![0xff, 0xd8];
        let mut standard_payload = XMP_SIG.to_vec();
        standard_payload.extend_from_slice(standard.as_bytes());
        j.extend(seg(0xe1, &standard_payload));
        let split = extension.len() / 2;
        for (off, part) in [(split as u32, extension.as_bytes().get(split..).unwrap_or(&[])), (0, extension.as_bytes().get(..split).unwrap_or(&[]))] {
            let mut p = XMP_EXT_SIG.to_vec();
            p.extend_from_slice(guid.as_bytes());
            p.extend_from_slice(&(extension.len() as u32).to_be_bytes());
            p.extend_from_slice(&off.to_be_bytes());
            p.extend_from_slice(part);
            j.extend(seg(0xe1, &p));
        }
        assert_eq!(jpeg_segments(&j).xmp_extended.as_deref(), Some(extension));
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

    #[test]
    fn truncated_containers_do_not_panic() {
        let j = sample_jpeg();
        for n in 0..j.len() {
            let _ = crate::extract(&j[..n]);
        }
    }
}
