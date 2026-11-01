//! Video container metadata extraction for LightCraft (MP4, QuickTime MOV, M4V).
//!
//! Parses ISO base media file format (ISO/IEC 14496-12) and QuickTime containers without
//! decompressing video frames:
//! - Movie header (`mvhd`): duration, timescale, creation time.
//! - Video track header (`tkhd`): width, height, display transform matrix / rotation.
//! - Metadata: XMP (`uuid` with XMP UUID or `udta` `XMP_`), QuickTime tags (`©mak`, `©mod`, `©day`, `©xyz`).

use lightcraft_geom::Orientation;

use crate::datetime::DateTime;
use crate::{Gps, Metadata};

/// Maximum box nesting depth to avoid recursion attacks.
const MAX_DEPTH: usize = 8;
/// Maximum number of boxes to inspect: the top-level budget native import and the streaming
/// Original copy allow (`MAX_VIDEO_TOP_LEVEL_BOXES` in the engine), so a fragmented clip accepted
/// by import can still be probed and verified before it is published.
const MAX_BOXES: usize = 1_000_000;
/// XMP UUID in ISO-BMFF `uuid` boxes: be7acfcb-97a9-42e8-9c71-999491e3afac (XMP Specification Part 3).
const XMP_UUID: [u8; 16] = [0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac];
/// Difference in seconds between the Apple/Mac epoch (1904-01-01 00:00:00 UTC) and the Unix epoch
/// (1970-01-01 00:00:00 UTC): 66 years (including 17 leap days) = 24,107 days * 86,400 seconds.
const MAC_EPOCH_OFFSET_SECONDS: u64 = 2_082_844_800;

/// Extracted video container facts and metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoMeta {
    /// Video duration in seconds (e.g. 14.52).
    pub duration: Option<f64>,
    /// Native pixel width of the primary video track.
    pub width: u32,
    /// Native pixel height of the primary video track.
    pub height: u32,
    /// Orientation from the video track's display transform matrix (e.g. 90° for smartphone videos).
    pub orientation: Option<Orientation>,
    /// Capture time as ISO 8601 (`YYYY-MM-DDTHH:MM:SS`), from container header or metadata.
    pub captured: Option<String>,
    /// Any photo/camera metadata extracted from container tags or embedded XMP.
    pub metadata: Metadata,
}

#[derive(Clone, Copy, Debug)]
struct BoxRef<'a> {
    kind: [u8; 4],
    body: &'a [u8],
    uuid: Option<[u8; 16]>,
}

/// Read an unsigned 32-bit big-endian integer at offset `at`, returning None if out of bounds.
fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Read an unsigned 64-bit big-endian integer at offset `at`, returning None if out of bounds.
fn be64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at + 8)?;
    Some(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// Convert Apple 1904-epoch seconds to ISO 8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
fn mac_time_to_iso(secs: u64) -> Option<String> {
    if secs <= MAC_EPOCH_OFFSET_SECONDS {
        return None;
    }
    let unix_secs = i64::try_from(secs.checked_sub(MAC_EPOCH_OFFSET_SECONDS)?).ok()?;
    let dt = chrono::DateTime::from_timestamp(unix_secs, 0)?;
    // `mvhd` times are UTC (ISO/IEC 14496-12); the suffix says so, so a later reader does not take
    // them for the local wall-clock time a camera records in Exif/XMP.
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Parse ISO 6709 geolocation string (e.g. `+37.7749-122.4194/` or `+35.6586+139.7454+035.100/`).
fn parse_iso6709(s: &str) -> Option<Gps> {
    let s = s.trim().trim_end_matches('/');
    let mut sign_indices = Vec::new();
    for (i, c) in s.char_indices() {
        if c == '+' || c == '-' {
            sign_indices.push(i);
        }
    }
    if sign_indices.len() < 2 {
        return None;
    }
    let lat_idx = *sign_indices.first()?;
    let lon_idx = *sign_indices.get(1)?;
    let alt_idx = sign_indices.get(2).copied();

    let lat_str = s.get(lat_idx..lon_idx)?;
    let lon_str = match alt_idx {
        Some(a_idx) => s.get(lon_idx..a_idx)?,
        None => s.get(lon_idx..)?,
    };
    let alt_str = alt_idx.and_then(|a_idx| s.get(a_idx..));

    let latitude = lat_str.parse::<f64>().ok().filter(|v| v.is_finite() && (-90.0..=90.0).contains(v))?;
    let longitude = lon_str.parse::<f64>().ok().filter(|v| v.is_finite() && (-180.0..=180.0).contains(v))?;
    let altitude = alt_str.and_then(|a| a.parse::<f64>().ok()).filter(|v| v.is_finite());
    Some(Gps { latitude, longitude, altitude })
}

/// Parse boxes sequentially within `bytes`, decrementing `budget` for each box inspected.
fn read_boxes<'a>(bytes: &'a [u8], budget: &mut usize) -> Vec<BoxRef<'a>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at.saturating_add(8) <= bytes.len() && *budget > 0 {
        *budget -= 1;
        let (Some(size), Some(kind_bytes)) = (be32(bytes, at), bytes.get(at + 4..at + 8)) else { break };
        let mut kind = [0u8; 4];
        kind.copy_from_slice(kind_bytes);

        let (total_len, hdr_len) = match size {
            0 => (bytes.len().saturating_sub(at), 8usize),
            1 => match be64(bytes, at.saturating_add(8)).and_then(|v| usize::try_from(v).ok()) {
                Some(l) => (l, 16usize),
                None => break,
            },
            n => (n as usize, 8usize),
        };

        if total_len < hdr_len {
            break;
        }

        let Some(end) = at.checked_add(total_len).filter(|&e| e <= bytes.len()) else { break };

        let mut body_start = at.saturating_add(hdr_len);
        let mut uuid = None;
        if &kind == b"uuid" {
            if let Some(u) = bytes.get(body_start..body_start + 16).filter(|_| body_start + 16 <= end) {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(u);
                uuid = Some(arr);
                body_start = body_start.saturating_add(16);
            } else {
                break;
            }
        }

        let Some(body) = bytes.get(body_start..end) else { break };
        out.push(BoxRef { kind, body, uuid });
        at = end;
    }
    out
}

/// Like [`read_boxes`], but a privacy check must not mistake a malformed or over-budget suffix for
/// metadata that is absent. The existing parser is intentionally best-effort for display metadata;
/// here verify that its parsed boxes account for the complete byte sequence.
fn read_boxes_strict<'a>(bytes: &'a [u8], budget: &mut usize) -> Result<Vec<BoxRef<'a>>, String> {
    let boxes = read_boxes(bytes, budget);
    let mut at = 0usize;
    for b in &boxes {
        let header_len = if be32(bytes, at) == Some(1) { 16usize } else { 8usize };
        let uuid_len = if b.kind == *b"uuid" { 16usize } else { 0usize };
        at = at
            .checked_add(header_len)
            .and_then(|n| n.checked_add(uuid_len))
            .and_then(|n| n.checked_add(b.body.len()))
            .ok_or_else(|| "can't verify video location: metadata box length overflow".to_string())?;
    }
    if at != bytes.len() {
        return Err("can't verify video location: malformed or excessive metadata boxes".into());
    }
    Ok(boxes)
}

/// Parse orientation from QuickTime/ISOBMFF 3x3 transform matrix (9 x 32-bit fixed point: 16.16).
/// `[a, b, u, c, d, v, x, y, w]`.
fn orientation_from_matrix(matrix: &[u8]) -> Option<Orientation> {
    if matrix.len() < 20 {
        return None;
    }
    let a = i32::from_be_bytes([*matrix.first()?, *matrix.get(1)?, *matrix.get(2)?, *matrix.get(3)?]);
    let b = i32::from_be_bytes([*matrix.get(4)?, *matrix.get(5)?, *matrix.get(6)?, *matrix.get(7)?]);
    let c = i32::from_be_bytes([*matrix.get(12)?, *matrix.get(13)?, *matrix.get(14)?, *matrix.get(15)?]);
    let d = i32::from_be_bytes([*matrix.get(16)?, *matrix.get(17)?, *matrix.get(18)?, *matrix.get(19)?]);

    // Threshold around 1.0 (0x10000 = 65536) allowing minor fixed-point rounding. The entries
    // come from the file: checked arithmetic, so an `i32::MIN`/`i32::MAX` entry is simply not a
    // rotation instead of an overflow (never-crash rule).
    let is_one = |v: i32| v.checked_sub(0x10000).is_some_and(|d| d.unsigned_abs() < 0x200);
    let is_neg_one = |v: i32| v.checked_add(0x10000).is_some_and(|d| d.unsigned_abs() < 0x200);
    let is_zero = |v: i32| v.unsigned_abs() < 0x200;

    if is_one(a) && is_one(d) && is_zero(b) && is_zero(c) {
        Some(Orientation::Normal)
    } else if is_zero(a) && is_zero(d) && is_one(b) && is_neg_one(c) {
        Some(Orientation::Rotate90)
    } else if is_neg_one(a) && is_neg_one(d) && is_zero(b) && is_zero(c) {
        Some(Orientation::Rotate180)
    } else if is_zero(a) && is_zero(d) && is_neg_one(b) && is_one(c) {
        Some(Orientation::Rotate270)
    } else {
        None
    }
}

/// Helper struct for track inspection.
struct TrackInfo {
    is_video: bool,
    width: u32,
    height: u32,
    orientation: Option<Orientation>,
}

/// Parse a `trak` box to extract video dimensions and matrix orientation from its `tkhd`, and the
/// metadata a camera or recorder keeps per track (`trak/udta`, or `trak/mdia/udta`).
fn parse_trak(body: &[u8], budget: &mut usize, depth: usize, meta: &mut Metadata, xmp: &mut Option<String>) -> Option<TrackInfo> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut is_video = false;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut orientation = None;

    for b in read_boxes(body, budget) {
        match &b.kind {
            b"tkhd" => {
                let Some(&ver) = b.body.first() else { continue };
                let (matrix_off, w_off, h_off) = if ver == 0 { (40usize, 76usize, 80usize) } else { (52usize, 88usize, 92usize) };
                if let Some(m) = b.body.get(matrix_off..matrix_off + 36) {
                    orientation = orientation_from_matrix(m);
                }
                if let Some(w) = be32(b.body, w_off) {
                    width = w >> 16;
                }
                if let Some(h) = be32(b.body, h_off) {
                    height = h >> 16;
                }
            }
            b"udta" => parse_udta(b.body, meta, xmp, budget, depth + 1),
            b"meta" => parse_meta(b.body, meta, xmp, budget, depth + 1),
            b"XMP_" => take_xmp(b.body, xmp),
            b"uuid" if b.uuid == Some(XMP_UUID) => take_xmp(b.body, xmp),
            b"mdia" => {
                // mdia contains hdlr and minf, and some recorders keep their udta here
                for child in read_boxes(b.body, budget) {
                    match &child.kind {
                        b"hdlr" => {
                            // handler type at body + 8..12
                            if child.body.get(8..12) == Some(b"vide") {
                                is_video = true;
                            }
                        }
                        b"udta" => parse_udta(child.body, meta, xmp, budget, depth + 1),
                        b"meta" => parse_meta(child.body, meta, xmp, budget, depth + 1),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Some(TrackInfo { is_video, width, height, orientation })
}

/// Read text payload from a QuickTime user data atom.
/// Some tags store directly `[len: u16, lang: u16, text: [u8]]`, others wrap in a `data` box.
fn read_udta_text(body: &[u8]) -> Option<String> {
    // Check if it wraps a `data` child box (iTunes/MP4 style)
    if body.len() >= 16 && body.get(4..8) == Some(b"data") {
        let size = be32(body, 0)? as usize;
        let data_payload = body.get(16..size.min(body.len()))?;
        return String::from_utf8(data_payload.to_vec()).ok();
    }
    // Classic QuickTime atom: a 16-bit text length, a 16-bit language, then the text (the length
    // says where the text ends, so trailing bytes of the box are not part of the value)
    if body.len() >= 4 {
        let declared = body.get(..2).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])));
        let end = declared.and_then(|n| 4usize.checked_add(n)).filter(|e| *e <= body.len()).unwrap_or(body.len());
        let str_bytes = body.get(4..end)?;
        return String::from_utf8(str_bytes.to_vec()).ok().or_else(|| Some(String::from_utf8_lossy(str_bytes).into_owned()));
    }
    None
}

/// Keep the first XMP packet found in an `XMP_` or XMP `uuid` box.
fn take_xmp(body: &[u8], xmp: &mut Option<String>) {
    if let Ok(s) = std::str::from_utf8(body) {
        *xmp = Some(s.to_string());
    }
}

/// `true` for a `loci` atom that carries nothing: only its version/flags header, or zeroed fields.
fn loci_is_empty(body: &[u8]) -> bool {
    body.get(4..).is_some_and(|rest| rest.iter().all(|b| *b == 0))
}

/// Parse a QuickTime/3GP `loci` (location information) atom: a version/flags header, a 16-bit
/// language, a null-terminated place name, a role byte, the coordinates as signed 16.16 fixed
/// point (longitude, latitude, altitude in metres) and two trailing text fields (astronomical body
/// and notes). A cleared atom — all zeroes — means "no location".
fn parse_loci(body: &[u8]) -> Option<Gps> {
    if loci_is_empty(body) {
        return None;
    }
    // header (4) + language (2), then the name up to its terminator, then the role byte
    let name = body.get(6..)?;
    let nul = name.iter().position(|b| *b == 0)?;
    let at = 6usize.checked_add(nul)?.checked_add(1)?.checked_add(1)?;
    let fixed = |i: usize| -> Option<f64> {
        let s = body.get(i..i.checked_add(4)?)?;
        Some(f64::from(i32::from_be_bytes([s[0], s[1], s[2], s[3]])) / 65_536.0)
    };
    let longitude = fixed(at)?;
    let latitude = fixed(at.checked_add(4)?)?;
    let altitude = fixed(at.checked_add(8)?)?;
    if !latitude.is_finite() || !longitude.is_finite() || !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    Some(Gps { latitude, longitude, altitude: altitude.is_finite().then_some(altitude) })
}

/// Parse metadata inside `udta` container box.
fn parse_udta(body: &[u8], meta: &mut Metadata, xmp: &mut Option<String>, budget: &mut usize, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for b in read_boxes(body, budget) {
        match &b.kind {
            b"XMP_" => take_xmp(b.body, xmp),
            b"\xa9mak" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.make = Some(s.trim().to_string());
                }
            }
            b"\xa9mod" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.model = Some(s.trim().to_string());
                }
            }
            b"\xa9swr" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.software = Some(s.trim().to_string());
                }
            }
            b"\xa9art" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.artist = Some(s.trim().to_string());
                }
            }
            b"\xa9nam" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.title = Some(s.trim().to_string());
                }
            }
            b"\xa9cmt" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty()) {
                    meta.caption = Some(s.trim().to_string());
                }
            }
            b"\xa9day" => {
                if let Some(s) = read_udta_text(b.body).filter(|s| !s.trim().is_empty())
                    && let Some(dt) = DateTime::parse_iso(&s).or_else(|| DateTime::parse_exif(&s))
                {
                    meta.capture_time = Some(dt);
                }
            }
            b"\xa9xyz" => {
                if let Some(s) = read_udta_text(b.body)
                    && let Some(gps) = parse_iso6709(&s)
                {
                    meta.gps = Some(gps);
                }
            }
            b"loci" => {
                if let Some(gps) = parse_loci(b.body) {
                    meta.gps = Some(gps);
                }
            }
            b"meta" => {
                parse_meta(b.body, meta, xmp, budget, depth + 1);
            }
            _ => {}
        }
    }
}

/// Apply one QuickTime metadata key (mdta) value to the file's metadata.
fn apply_mdta(key: &str, value: &str, meta: &mut Metadata) {
    if value.is_empty() {
        return;
    }
    match key {
        // ISO 8601 with an offset, e.g. `2026-05-04T21:00:00-0400`: the local capture time.
        "com.apple.quicktime.creationdate" => {
            if let Some(dt) = DateTime::parse_iso(value).or_else(|| DateTime::parse_exif(value)) {
                meta.capture_time = Some(dt);
            }
        }
        "com.apple.quicktime.make" => meta.make = Some(value.to_string()),
        "com.apple.quicktime.model" => meta.model = Some(value.to_string()),
        "com.apple.quicktime.software" => meta.software = Some(value.to_string()),
        "com.apple.quicktime.location.ISO6709" => {
            if let Some(gps) = parse_iso6709(value) {
                meta.gps = Some(gps);
            }
        }
        "com.apple.quicktime.artist" => meta.artist = Some(value.to_string()),
        "com.apple.quicktime.title" => meta.title = Some(value.to_string()),
        "com.apple.quicktime.comment" => meta.caption = Some(value.to_string()),
        _ => {}
    }
}

/// QuickTime starts with an `hdlr` child; ISO FullBox metadata has version/flags before children.
fn meta_children(body: &[u8]) -> Option<&[u8]> {
    if body.get(4..8) == Some(b"hdlr") { Some(body) } else { body.get(4..) }
}

/// Parse a QuickTime/ISO `meta` box: its `keys` name the fields and its `ilst` items, numbered by
/// the key they belong to, hold the values. Apple's MOVs record the capture time, make, model,
/// software and location here (mdta) rather than in `udta`; the time carries its offset.
fn parse_meta(body: &[u8], meta: &mut Metadata, xmp: &mut Option<String>, budget: &mut usize, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(body) = meta_children(body) else { return };
    let mut keys: Vec<Option<String>> = Vec::new();
    for b in read_boxes(body, budget) {
        match &b.kind {
            b"XMP_" => take_xmp(b.body, xmp),
            b"uuid" if b.uuid == Some(XMP_UUID) => take_xmp(b.body, xmp),
            b"keys" => {
                // FullBox header, entry count, then each key: size (namespace included) and name
                let count = be32(b.body, 4).unwrap_or(0).min(4096) as usize;
                let mut at = 8usize;
                for _ in 0..count {
                    let Some(size) = be32(b.body, at).map(|s| s as usize).filter(|s| *s >= 8) else { break };
                    let Some(name) = b.body.get(at.saturating_add(8)..at.saturating_add(size)) else { break };
                    keys.push(std::str::from_utf8(name).ok().map(str::to_owned));
                    at = at.saturating_add(size);
                }
            }
            b"ilst" => {
                for item in read_boxes(b.body, budget) {
                    let index = u32::from_be_bytes(item.kind) as usize;
                    let Some(Some(key)) = keys.get(index.saturating_sub(1)) else { continue };
                    for data in read_boxes(item.body, budget) {
                        if &data.kind != b"data" {
                            continue;
                        }
                        // `data` is a type and a locale before the value (type 1: UTF-8 text)
                        let Some(bytes) = data.body.get(8..) else { continue };
                        if let Ok(value) = std::str::from_utf8(bytes) {
                            apply_mdta(key, value.trim(), meta);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract any embedded XMP packet found in an ISO-BMFF / QuickTime container.
pub fn extract_video_xmp(bytes: &[u8]) -> Option<String> {
    let mut budget = MAX_BOXES;
    for b in read_boxes(bytes, &mut budget) {
        if b.kind == *b"XMP_" {
            return std::str::from_utf8(b.body).ok().map(str::to_string);
        }
        if &b.kind == b"uuid"
            && b.uuid == Some(XMP_UUID)
            && let Ok(s) = std::str::from_utf8(b.body)
        {
            return Some(s.to_string());
        }
        if &b.kind == b"moov" {
            for child in read_boxes(b.body, &mut budget) {
                if &child.kind == b"uuid"
                    && child.uuid == Some(XMP_UUID)
                    && let Ok(s) = std::str::from_utf8(child.body)
                {
                    return Some(s.to_string());
                }
                if &child.kind == b"udta" {
                    let mut meta = Metadata::default();
                    let mut xmp = None;
                    parse_udta(child.body, &mut meta, &mut xmp, &mut budget, 0);
                    if xmp.is_some() {
                        return xmp;
                    }
                }
                if &child.kind == b"meta"
                    && let Some(body) = meta_children(child.body)
                {
                    for inner in read_boxes(body, &mut budget) {
                        if (&inner.kind == b"XMP_" || inner.uuid == Some(XMP_UUID))
                            && let Ok(s) = std::str::from_utf8(inner.body)
                        {
                            return Some(s.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Establish that a supported video has no embedded location before copying it unchanged.
/// Unlike probing for display, incomplete boxes, exhausted budgets and malformed XMP fail closed.
pub fn verify_video_location_absent(bytes: &[u8]) -> Result<(), String> {
    let mut budget = MAX_BOXES;
    verify_location_boxes(bytes, &mut budget, 0)?;
    probe_video(bytes).ok_or("can't verify location: unreadable video")?;
    Ok(())
}

fn verify_xmp_structure(packet: &str) -> Result<(), String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(packet);
    let (mut depth, mut roots) = (0usize, 0usize);
    for _ in 0..200_000 {
        let event = reader.read_event().map_err(|e| format!("can't verify video location: invalid XMP: {e}"))?;
        match event {
            Event::Start(_) | Event::Empty(_) => {
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err("can't verify video location: multiple XMP roots".into());
                    }
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                    if depth > 64 {
                        return Err("can't verify video location: XMP nesting limit exceeded".into());
                    }
                }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or("can't verify video location: unexpected XMP end tag")?;
            }
            Event::DocType(_) => return Err("can't verify video location: unsupported XMP document type".into()),
            Event::Text(text) if depth == 0 && !text.as_ref().iter().all(u8::is_ascii_whitespace) => {
                return Err("can't verify video location: text outside the XMP root".into());
            }
            Event::CData(_) if depth == 0 => return Err("can't verify video location: text outside the XMP root".into()),
            Event::Eof => {
                return if roots == 1 && depth == 0 { Ok(()) } else { Err("can't verify video location: incomplete XMP".into()) };
            }
            _ => {}
        }
    }
    Err("can't verify video location: XMP event limit exceeded".into())
}

fn verify_location_boxes(bytes: &[u8], budget: &mut usize, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("can't verify video location: metadata nesting limit exceeded".into());
    }
    let mut at = 0usize;
    while at < bytes.len() {
        if *budget == 0 {
            return Err("can't verify video location: metadata box limit exceeded".into());
        }
        let size = be32(bytes, at).ok_or("can't verify video location: truncated box header")?;
        let (len, header) = match size {
            0 => (bytes.len().saturating_sub(at), 8),
            1 => (
                be64(bytes, at.saturating_add(8)).and_then(|n| usize::try_from(n).ok()).ok_or("can't verify video location: invalid large box")?,
                16,
            ),
            n => (n as usize, 8),
        };
        let end = at.checked_add(len).filter(|end| len >= header && *end <= bytes.len()).ok_or("can't verify video location: incomplete box")?;
        let one = bytes.get(at..end).ok_or("can't verify video location: invalid box range")?;
        let mut one_budget = 1;
        let b = read_boxes(one, &mut one_budget).pop().ok_or("can't verify video location: malformed box")?;
        *budget -= 1;
        match &b.kind {
            b"\xa9xyz" => {
                // An empty atom names no place: some writers leave the tag behind when the user
                // clears a clip's location. Anything it does hold refuses the unchanged copy.
                if !read_udta_text(b.body).unwrap_or_default().trim().is_empty() {
                    return Err(
                        "can't remove embedded GPS or embedded location metadata from an original video; export without Remove Location only to share the unchanged clip"
                            .into(),
                    );
                }
            }
            b"loci" => {
                if !loci_is_empty(b.body) {
                    return Err(
                        "can't remove embedded GPS from an original video; export without Remove Location only to share the unchanged clip".into()
                    );
                }
            }
            b"moov" | b"udta" | b"trak" | b"mdia" | b"moof" | b"traf" => verify_location_boxes(b.body, budget, depth + 1)?,
            b"meta" => {
                let body = meta_children(b.body).ok_or("can't verify video location: truncated metadata")?;
                // `keys` names the `ilst` items: only a location key that holds a value refuses.
                verify_meta_location(body, budget)?;
                verify_location_boxes(body, budget, depth + 1)?;
            }
            b"XMP_" | b"uuid" if &b.kind == b"XMP_" || b.uuid == Some(XMP_UUID) => {
                let packet = std::str::from_utf8(b.body).map_err(|_| "can't verify video location: invalid XMP encoding")?;
                verify_xmp_structure(packet)?;
                let xmp = crate::parse_xmp(packet).map_err(|e| format!("can't verify video location: invalid XMP: {e}"))?;
                // The flattened map gives up properties once their key text passes a limit: it can
                // no longer prove that a location is absent, so the packet is unverifiable.
                if xmp.properties_truncated {
                    return Err(concat!(
                        "can't verify an original video's embedded location: the XMP property inventory ",
                        "exceeds the size limit; export without Remove Location only to share the unchanged clip"
                    )
                    .into());
                }
                if xmp.metadata.gps.is_some() || xmp.has_location_metadata() {
                    return Err(
                        "can't remove embedded GPS or embedded location metadata from an original video; export without Remove Location only to share the unchanged clip"
                            .into(),
                    );
                }
            }
            _ => {}
        }
        at = end;
    }
    Ok(())
}

const LOCATION_KEY: &[u8] = b"com.apple.quicktime.location.ISO6709";

/// Verify a `meta` box's mdta pairs: `keys` names each `ilst` item by its 1-based index, and a
/// location key that carries a value refuses the copy. A key whose item is missing or blank names
/// no place, like the empty XMP place fields the parser accepts.
fn verify_meta_location(body: &[u8], budget: &mut usize) -> Result<(), String> {
    let mut location_keys: Vec<usize> = Vec::new();
    let mut items: Vec<(usize, &[u8])> = Vec::new();
    let mut key_count = None;
    let mut saw_ilst = false;
    for b in read_boxes_strict(body, budget)? {
        match &b.kind {
            b"keys" => {
                if key_count.is_some() || saw_ilst {
                    return Err("can't verify video location: duplicate or out-of-order metadata keys".into());
                }
                let count = be32(b.body, 4).ok_or("can't verify video location: truncated metadata keys")?;
                if count > 4096 {
                    return Err("can't verify video location: metadata key limit exceeded".into());
                }
                key_count = Some(count as usize);
                let mut offset = 8usize;
                for index in 1..=count as usize {
                    let n = be32(b.body, offset).filter(|n| *n >= 8).ok_or("can't verify video location: invalid metadata key")? as usize;
                    let end = offset.checked_add(n).ok_or("can't verify video location: metadata key overflow")?;
                    let key = b.body.get(offset.saturating_add(8)..end).ok_or("can't verify video location: truncated metadata key")?;
                    if key == LOCATION_KEY {
                        location_keys.push(index);
                    }
                    offset = end;
                }
                if offset != b.body.len() {
                    return Err("can't verify video location: trailing metadata key bytes".into());
                }
            }
            b"ilst" => {
                if saw_ilst {
                    return Err("can't verify video location: duplicate metadata item list".into());
                }
                saw_ilst = true;
                for item in read_boxes_strict(b.body, budget)? {
                    // `ilst` can hold QuickTime atoms by name as well as mdta index items. A
                    // `©xyz` item carries the same place string as `udta`: an empty value names
                    // no place, anything else refuses the copy.
                    if &item.kind == b"\xa9xyz" {
                        for data in read_boxes_strict(item.body, budget)? {
                            if &data.kind != b"data" {
                                return Err("can't verify video location: unreadable location metadata value".into());
                            }
                            let value = data.body.get(8..).ok_or("can't verify video location: truncated metadata value")?;
                            if !value.iter().all(u8::is_ascii_whitespace) {
                                return Err(
                                    "can't remove embedded GPS or embedded location metadata from an original video; export without Remove Location only to share the unchanged clip"
                                        .into(),
                                );
                            }
                        }
                        continue;
                    }
                    items.push((u32::from_be_bytes(item.kind) as usize, item.body));
                }
            }
            _ => {}
        }
    }
    for index in location_keys {
        for (item_index, item) in &items {
            if *item_index == 0 || key_count.is_some_and(|n| *item_index > n) {
                return Err("can't verify video location: metadata item has an invalid key index".into());
            }
            if *item_index == index {
                for data in read_boxes_strict(item, budget)? {
                    if &data.kind != b"data" {
                        return Err("can't verify video location: unreadable location metadata value".into());
                    }
                    // type and locale precede the value; a short or unreadable item could hide one
                    let value = data.body.get(8..).ok_or("can't verify video location: truncated metadata value")?;
                    if !value.iter().all(u8::is_ascii_whitespace) {
                        return Err(
                            "can't remove embedded GPS from an original video; export without Remove Location only to share the unchanged clip"
                                .into(),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Probe a video container file's bytes (MP4, QuickTime MOV, M4V) without decoding pixel streams:
/// returns `Some(VideoMeta)` if valid video tracks and header boxes are found.
pub fn probe_video(bytes: &[u8]) -> Option<VideoMeta> {
    if bytes.len() < 12 {
        return None;
    }

    let mut budget = MAX_BOXES;
    let mut duration = None;
    let mut timescale = None;
    let mut fragment_duration = None;
    let mut captured = None;
    let mut primary_track: Option<TrackInfo> = None;
    let mut meta = Metadata::default();
    let mut xmp = None;

    for b in read_boxes(bytes, &mut budget) {
        if &b.kind == b"uuid"
            && b.uuid == Some(XMP_UUID)
            && let Ok(s) = std::str::from_utf8(b.body)
        {
            xmp = Some(s.to_string());
        }
        if &b.kind == b"moov" {
            for child in read_boxes(b.body, &mut budget) {
                match &child.kind {
                    b"mvhd" => {
                        let Some(&ver) = child.body.first() else { continue };
                        let (ts, dur, creat) = if ver == 0 {
                            let creat = be32(child.body, 4).map(u64::from);
                            let ts = be32(child.body, 12);
                            let dur = be32(child.body, 16).filter(|&d| d != u32::MAX).map(u64::from);
                            (ts, dur, creat)
                        } else {
                            let creat = be64(child.body, 4);
                            let ts = be32(child.body, 20);
                            let dur = be64(child.body, 24).filter(|&d| d != u64::MAX);
                            (ts, dur, creat)
                        };

                        timescale = ts;
                        if let (Some(ts), Some(d)) = (ts, dur)
                            && ts > 0
                            && d > 0
                        {
                            let secs = d as f64 / ts as f64;
                            if secs.is_finite() && secs > 0.0 {
                                duration = Some(secs);
                            }
                        }

                        if let Some(creat_secs) = creat
                            && let Some(iso) = mac_time_to_iso(creat_secs)
                        {
                            captured = Some(iso);
                        }
                    }
                    b"trak" => {
                        if let Some(t) = parse_trak(child.body, &mut budget, 0, &mut meta, &mut xmp)
                            && t.is_video
                        {
                            // The main video is the track showing the largest picture; an
                            // auxiliary track (a thumbnail, a preview) may come later and must
                            // not replace it. Ties keep the first track.
                            let area = |t: &TrackInfo| u64::from(t.width).saturating_mul(u64::from(t.height));
                            let larger = primary_track.as_ref().is_none_or(|p| area(&t) > area(p));
                            if larger {
                                primary_track = Some(t);
                            }
                        }
                    }
                    // A fragmented movie (`empty_moov`: the file is a series of fragments) writes
                    // 0 in `mvhd`; its whole duration is the fragment duration in `mvex` → `mehd`.
                    b"mvex" => {
                        for grand in read_boxes(child.body, &mut budget) {
                            if &grand.kind == b"mehd" {
                                let ver = grand.body.first().copied().unwrap_or(0);
                                let d = if ver == 0 {
                                    be32(grand.body, 4).filter(|&d| d != u32::MAX).map(u64::from)
                                } else {
                                    be64(grand.body, 4).filter(|&d| d != u64::MAX)
                                };
                                if let Some(d) = d.filter(|d| *d > 0) {
                                    fragment_duration = Some(d);
                                }
                            }
                        }
                    }
                    b"udta" => {
                        parse_udta(child.body, &mut meta, &mut xmp, &mut budget, 0);
                    }
                    b"meta" => {
                        parse_meta(child.body, &mut meta, &mut xmp, &mut budget, 0);
                    }
                    b"XMP_" => take_xmp(child.body, &mut xmp),
                    b"uuid" if child.uuid == Some(XMP_UUID) => take_xmp(child.body, &mut xmp),
                    _ => {}
                }
            }
        }
        if &b.kind == b"meta" {
            parse_meta(b.body, &mut meta, &mut xmp, &mut budget, 0);
        }
        if &b.kind == b"XMP_" {
            take_xmp(b.body, &mut xmp);
        }
    }

    // `mvhd`'s duration in its timescale; a fragmented movie takes `mehd`'s instead.
    if duration.is_none()
        && let (Some(ts), Some(d)) = (timescale, fragment_duration)
        && ts > 0
    {
        let secs = d as f64 / ts as f64;
        if secs.is_finite() && secs > 0.0 {
            duration = Some(secs);
        }
    }

    // Merge any XMP found into metadata
    if let Some(xmp_str) = xmp.as_deref().and_then(|x| crate::parse_xmp(x).ok()) {
        meta.overlay_user_fields(&xmp_str.metadata);
        meta.fill_missing(&xmp_str.metadata);
    }

    // If the container tags and the XMP named no capture time, the movie header's creation time
    // stands in; when they did name one, it reports that instead of the time the file was muxed.
    if meta.capture_time.is_none() {
        if let Some(c) = captured.as_deref().and_then(DateTime::parse_iso) {
            meta.capture_time = Some(c);
        }
    } else if let Some(c) = meta.capture_time {
        captured = Some(c.to_iso());
    }

    let has_video_track = primary_track.is_some();
    let (width, height, orientation) = match primary_track {
        Some(t) => (t.width, t.height, t.orientation),
        None => (0, 0, None),
    };

    meta.width = (width > 0).then_some(width);
    meta.height = (height > 0).then_some(height);
    meta.orientation = orientation;

    // A valid video probe needs a video track; a duration alone is not a video.
    if has_video_track && (duration.is_some() || width > 0 || height > 0) {
        Some(VideoMeta { duration, width, height, orientation, captured, metadata: meta })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_verification_checks_every_packet_and_partial_gps() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        assert!(verify_video_location_absent(&clean).is_ok());
        let clean_xmp = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description/></rdf:RDF>"#;
        let mut bytes = clean.clone();
        write_box(&mut bytes, b"XMP_", clean_xmp);
        assert!(verify_video_location_absent(&bytes).is_ok());
        for attribute in [r#"e:GPSLatitude="37,46.494N""#, r#"e:GPSAltitude="100/1""#, r#"e:GPSLongitude="invalid""#] {
            let xmp = format!(
                r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:e="http://ns.adobe.com/exif/1.0/" {attribute}/></rdf:RDF>"#
            );
            let mut located = bytes.clone();
            let mut packet = XMP_UUID.to_vec();
            packet.extend_from_slice(xmp.as_bytes());
            write_box(&mut located, b"uuid", &packet);
            assert!(verify_video_location_absent(&located).unwrap_err().contains("GPS"), "{attribute}");
        }
        write_box(&mut bytes, b"XMP_", b"<broken>");
        assert!(verify_video_location_absent(&bytes).unwrap_err().contains("XMP"));
    }

    #[test]
    fn location_verification_rejects_place_names_without_gps_in_every_xmp_container() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        for (namespace, local, canonical) in [
            ("http://ns.adobe.com/photoshop/1.0/", "City", "photoshop:City"),
            ("http://ns.adobe.com/photoshop/1.0/", "State", "photoshop:State"),
            ("http://ns.adobe.com/photoshop/1.0/", "Country", "photoshop:Country"),
            ("http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/", "Location", "Iptc4xmpCore:Location"),
            (crate::xmp::LC_NS, "location", "lc:location"),
        ] {
            for property in [
                format!(r#"<rdf:Description xmlns:p="{namespace}" p:{local}="Private place"/>"#),
                format!(r#"<rdf:Description xmlns:p="{namespace}"><p:{local}>Private place</p:{local}></rdf:Description>"#),
                format!(
                    r#"<rdf:Description xmlns:p="{namespace}"><p:{local}><rdf:Seq><rdf:li>Private place</rdf:li><rdf:li/></rdf:Seq></p:{local}></rdf:Description>"#
                ),
                format!(
                    r#"<rdf:Description xmlns:p="{namespace}"><p:details rdf:parseType="Resource"><p:{local}>Private place</p:{local}></p:details></rdf:Description>"#
                ),
            ] {
                let packet = format!(r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">{property}</rdf:RDF>"#);
                let parsed = crate::parse_xmp(&packet).unwrap();
                assert!(parsed.metadata.gps.is_none());
                assert!(parsed.properties.keys().any(|key| key.split('/').any(|part| part == canonical)));
                for uuid in [false, true] {
                    let mut bytes = clean.clone();
                    write_box(&mut bytes, b"XMP_", br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#);
                    let mut body = if uuid { XMP_UUID.to_vec() } else { Vec::new() };
                    body.extend_from_slice(packet.as_bytes());
                    write_box(&mut bytes, if uuid { b"uuid" } else { b"XMP_" }, &body);
                    let error = verify_video_location_absent(&bytes).unwrap_err();
                    assert!(error.contains("embedded location metadata"), "{canonical}, uuid={uuid}: {error}");
                }
            }
        }
    }

    #[test]
    fn location_verification_rejects_iptc_extension_location_structs() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        let packets = [
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:place="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"><rdf:Description><place:LocationShown><rdf:Bag><rdf:li rdf:parseType="Resource"><place:City>Rome</place:City></rdf:li></rdf:Bag></place:LocationShown></rdf:Description></rdf:RDF>"#,
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:place="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"><rdf:Description><place:LocationCreated rdf:parseType="Resource"><place:CountryName>Italy</place:CountryName></place:LocationCreated></rdf:Description></rdf:RDF>"#,
        ];
        for packet in packets {
            let parsed = crate::parse_xmp(packet).expect("valid IPTC Extension XMP");
            assert!(parsed.has_location_metadata(), "the IPTC location structure is inventoried");
            for uuid in [false, true] {
                let mut bytes = clean.clone();
                let mut body = if uuid { XMP_UUID.to_vec() } else { Vec::new() };
                body.extend_from_slice(packet.as_bytes());
                write_box(&mut bytes, if uuid { b"uuid" } else { b"XMP_" }, &body);
                let error = verify_video_location_absent(&bytes).unwrap_err();
                assert!(error.contains("GPS") || error.contains("location metadata"), "uuid={uuid}: {error}");
            }
        }

        let unrelated = concat!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:place="urn:unrelated">"#,
            r#"<rdf:Description><place:LocationShown><rdf:Bag><rdf:li rdf:parseType="Resource"><place:City>Rome</place:City></rdf:li></rdf:Bag></place:LocationShown></rdf:Description></rdf:RDF>"#
        );
        assert!(!crate::parse_xmp(unrelated).expect("valid unrelated XMP").has_location_metadata());
    }

    #[test]
    fn location_verification_allows_empty_place_names_and_unrelated_properties() {
        let packet = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description xmlns:p="http://ns.adobe.com/photoshop/1.0/"
            xmlns:i="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" xmlns:l="http://ns.lightcraft.app/lc/1.0/"
            xmlns:photoshop="urn:unrelated" xmlns:dc="http://purl.org/dc/elements/1.1/"
            p:City=" " p:State="" p:Country="" i:Location="" l:location=" " photoshop:City="Unrelated value"
            dc:title="City lights"/>
        </rdf:RDF>"#;
        for uuid in [false, true] {
            let mut bytes = synthetic_mp4(1000, 1000, 640, 360, 0);
            let mut body = if uuid { XMP_UUID.to_vec() } else { Vec::new() };
            body.extend_from_slice(packet);
            write_box(&mut bytes, if uuid { b"uuid" } else { b"XMP_" }, &body);
            verify_video_location_absent(&bytes).unwrap();
        }
    }

    /// A packet can bury its GPS properties past the flattened-key budget: the map then omits them
    /// while parsing still succeeds, so the verifier must refuse an inventory it cannot prove
    /// complete rather than read the omission as absence.
    #[test]
    fn location_verification_refuses_a_packet_that_buries_its_gps_past_the_key_budget() {
        let long = "u".repeat(4000);
        let mut packet = String::from(concat!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description"#,
            r#" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/" "#
        ));
        // Longer keys than the whole budget, repeated under one name.
        for _ in 0..400 {
            packet.push_str(&format!("dc:{long}=\"x\" "));
        }
        // Then short ones until the remainder cannot pay for another four-byte key.
        for _ in 0..1100 {
            packet.push_str("dc:z=\"x\" ");
        }
        packet.push_str(r#"exif:GPSLatitude="37,46.494N" exif:GPSLongitude="122,15.120W"/></rdf:RDF>"#);

        let parsed = crate::parse_xmp(&packet).unwrap();
        assert!(parsed.properties_truncated, "the fixture really exhausts the key budget");
        assert!(parsed.metadata.gps.is_none(), "the budget hides the coordinates from the map");
        assert!(!parsed.properties.keys().any(|key| key.contains("GPS")), "no GPS property survives the budget");

        for uuid in [false, true] {
            let mut bytes = synthetic_mp4(1000, 1000, 640, 360, 0);
            let mut body = if uuid { XMP_UUID.to_vec() } else { Vec::new() };
            body.extend_from_slice(packet.as_bytes());
            write_box(&mut bytes, if uuid { b"uuid" } else { b"XMP_" }, &body);
            let error = verify_video_location_absent(&bytes).unwrap_err();
            assert!(error.contains("exceeds the size limit"), "uuid={uuid}: {error}");
        }
    }

    /// Atoms a writer leaves behind when a location is cleared hold no place: they must not refuse
    /// an otherwise eligible copy. Anything with content still refuses.
    #[test]
    fn location_verification_allows_cleared_quicktime_location_atoms() {
        let text = |s: &str| {
            let mut body = (s.len() as u16).to_be_bytes().to_vec();
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(s.as_bytes());
            body
        };
        let with = |kind: &[u8; 4], body: &[u8]| {
            let mut bytes = synthetic_mp4(1000, 1000, 640, 360, 0);
            let mut atom = Vec::new();
            write_box(&mut atom, kind, body);
            let mut udta = Vec::new();
            write_box(&mut udta, b"udta", &atom);
            let moov_at = be32(&bytes, 0).expect("ftyp size") as usize;
            let mut moov = bytes.get(moov_at + 8..).expect("moov body").to_vec();
            moov.extend_from_slice(&udta);
            bytes.truncate(moov_at);
            write_box(&mut bytes, b"moov", &moov);
            bytes
        };
        let located = "+37.7749-122.4194/";
        for (body, allows) in [(text(""), true), (text(" "), true), (Vec::new(), true), (text(located), false)] {
            let bytes = with(b"\xa9xyz", &body);
            assert_eq!(verify_video_location_absent(&bytes).is_ok(), allows, "©xyz {body:?}");
            assert_eq!(probe_video(&bytes).unwrap().metadata.gps.is_some(), !allows, "©xyz probe {body:?}");
        }

        // a `loci` atom written as all zeroes names no place; spec coordinates do
        let cleared = vec![0u8; 4 + 2 + 1 + 1 + 12];
        let mut located = vec![0u8; 4]; // version/flags
        located.extend_from_slice(&0u16.to_be_bytes()); // language
        located.extend_from_slice(b"Place\0");
        located.push(1); // role: real location
        for value in [-122.4194f64, 37.7749, 0.0] {
            located.extend_from_slice(&((value * 65_536.0) as i32).to_be_bytes());
        }
        located.extend_from_slice(b"Earth\0");
        assert!(verify_video_location_absent(&with(b"loci", &cleared)).is_ok());
        assert!(probe_video(&with(b"loci", &cleared)).unwrap().metadata.gps.is_none());
        let located = with(b"loci", &located);
        assert!(verify_video_location_absent(&located).is_err());
        let gps = probe_video(&located).unwrap().metadata.gps.expect("loci coordinates");
        assert!((gps.latitude - 37.7749).abs() < 1e-4 && (gps.longitude + 122.4194).abs() < 1e-4, "{gps:?}");
    }

    /// Metadata kept per track (`trak/udta` and `trak/mdia/udta`) is read for display and checked
    /// the same way as the movie-level boxes, so the catalog never hides a location the copy refuses.
    #[test]
    fn quicktime_track_metadata_is_read_and_checked() {
        let text = |s: &str| {
            let mut body = (s.len() as u16).to_be_bytes().to_vec();
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(s.as_bytes());
            body
        };
        let udta = || {
            let mut udta = Vec::new();
            write_box(&mut udta, b"\xa9day", &text("2026-05-04T21:00:00-0400"));
            write_box(&mut udta, b"\xa9xyz", &text("+37.7749-122.4194/"));
            udta
        };
        for mdia in [false, true] {
            let file = synthetic_mp4(60, 600, 1920, 1080, 0);
            let mut extra = Vec::new();
            write_box(&mut extra, b"udta", &udta());
            let bytes = if mdia { with_mdia_children(&file, &extra) } else { with_trak_children(&file, &extra) };
            let meta = probe_video(&bytes).expect("valid video");
            assert_eq!(meta.captured.as_deref(), Some("2026-05-04T21:00:00-04:00"), "mdia={mdia}");
            assert!(meta.metadata.gps.is_some(), "mdia={mdia}");
            assert!(verify_video_location_absent(&bytes).is_err(), "mdia={mdia}");
        }
    }

    /// `ilst` items named `©xyz` (rather than the mdta index form) carry a place too.
    #[test]
    fn location_verification_checks_ilst_quicktime_location_items() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        let with_xyz = |value: &str| {
            let mut data = 1u32.to_be_bytes().to_vec();
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(value.as_bytes());
            let mut data_box = Vec::new();
            write_box(&mut data_box, b"data", &data);
            let mut item = Vec::new();
            write_box(&mut item, b"\xa9xyz", &data_box);
            let mut ilst = Vec::new();
            write_box(&mut ilst, b"ilst", &item);
            let mut meta_body = vec![0u8; 4];
            meta_body.extend_from_slice(&ilst);
            let mut meta = Vec::new();
            write_box(&mut meta, b"meta", &meta_body);
            let mut udta = Vec::new();
            write_box(&mut udta, b"udta", &meta);
            with_trak_children(&clean, &udta)
        };
        assert!(verify_video_location_absent(&with_xyz("")).is_ok(), "an empty ilst ©xyz names no place");
        let error = verify_video_location_absent(&with_xyz("+37.7749-122.4194/")).unwrap_err();
        assert!(error.contains("location metadata"), "{error}");
    }

    /// Fragmented movies carry metadata inside `moof`/`traf`; verification must look there too.
    #[test]
    fn location_verification_checks_fragmented_movie_metadata() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        let with_moof = |body: &[u8]| {
            let mut moof_body = Vec::new();
            write_box(&mut moof_body, b"traf", body);
            let mut moof = Vec::new();
            write_box(&mut moof, b"moof", &moof_body);
            let mut bytes = clean.clone();
            bytes.extend_from_slice(&moof);
            bytes
        };
        assert!(verify_video_location_absent(&with_moof(b"")).is_ok());
        let mut text = (19u16).to_be_bytes().to_vec();
        text.extend_from_slice(&0u16.to_be_bytes());
        text.extend_from_slice(b"+37.7749-122.4194/");
        let mut udta = Vec::new();
        write_box(&mut udta, b"udta", &{
            let mut atom = Vec::new();
            write_box(&mut atom, b"\xa9xyz", &text);
            atom
        });
        let error = verify_video_location_absent(&with_moof(&udta)).unwrap_err();
        assert!(error.contains("location metadata"), "{error}");
    }

    /// The main picture is the largest video track; a later auxiliary track does not replace it.
    #[test]
    fn probe_keeps_the_largest_video_track() {
        let file = synthetic_mp4(60, 600, 1920, 1080, 0);
        for (width, height, expected) in [(320, 240, (1920, 1080)), (2560, 1440, (2560, 1440))] {
            let bytes = with_extra_track(&file, width, height);
            let meta = probe_video(&bytes).expect("valid video");
            assert_eq!((meta.width, meta.height), expected, "added {width}x{height}");
        }
    }

    #[test]
    fn location_verification_refuses_incomplete_or_excessive_metadata() {
        let clean = synthetic_mp4(1000, 1000, 640, 360, 0);
        for tail in [&[0, 0, 0][..], &[0, 0, 0, 16, b'm', b'e', b't', b'a'][..]] {
            let mut bytes = clean.clone();
            bytes.extend_from_slice(tail);
            assert!(verify_video_location_absent(&bytes).is_err());
        }
        let mut bytes = clean.clone();
        for _ in 0..MAX_BOXES {
            write_box(&mut bytes, b"free", b"");
        }
        assert!(verify_video_location_absent(&bytes).unwrap_err().contains("box limit"));
        let mut nested = Vec::new();
        write_box(&mut nested, b"loci", b"");
        for _ in 0..=MAX_DEPTH {
            let mut parent = Vec::new();
            write_box(&mut parent, b"udta", &nested);
            nested = parent;
        }
        let mut bytes = clean;
        bytes.extend_from_slice(&nested);
        assert!(verify_video_location_absent(&bytes).unwrap_err().contains("nesting limit"));
    }

    /// Build a minimal synthetic MP4 file with `ftyp`, `moov`, `mvhd`, and a video `trak` (`tkhd` + `mdia`/`hdlr`).
    fn synthetic_mp4(timescale: u32, duration: u32, width: u32, height: u32, creation_time: u32) -> Vec<u8> {
        synthetic_mp4_with_fragment(timescale, duration, width, height, creation_time, None)
    }

    /// [`synthetic_mp4`], plus a `mvex/mehd` fragment duration for a fragmented movie (`None`: none).
    fn synthetic_mp4_with_fragment(
        timescale: u32,
        duration: u32,
        width: u32,
        height: u32,
        creation_time: u32,
        fragment_duration: Option<u32>,
    ) -> Vec<u8> {
        let mut out = Vec::new();

        // ftyp box (24 bytes)
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"mp42"); // major brand
        ftyp.extend_from_slice(&0u32.to_be_bytes()); // minor version
        ftyp.extend_from_slice(b"isommp42"); // compatible brands
        write_box(&mut out, b"ftyp", &ftyp);

        // mvhd box
        let mut mvhd = Vec::new();
        mvhd.push(0); // version 0
        mvhd.extend_from_slice(&[0, 0, 0]); // flags
        mvhd.extend_from_slice(&creation_time.to_be_bytes()); // creation time
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // mod time
        mvhd.extend_from_slice(&timescale.to_be_bytes()); // timescale
        mvhd.extend_from_slice(&duration.to_be_bytes()); // duration
        mvhd.extend_from_slice(&0x00010000u32.to_be_bytes()); // rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        mvhd.extend_from_slice(&[0; 10]); // reserved
        // 36 bytes identity matrix
        mvhd.extend_from_slice(&0x00010000u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0x00010000u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0x40000000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0; 24]); // pre-defined
        mvhd.extend_from_slice(&2u32.to_be_bytes()); // next_track_id

        // tkhd box (84 bytes for v0)
        let mut tkhd = Vec::new();
        tkhd.push(0); // version 0
        tkhd.extend_from_slice(&[0, 0, 1]); // flags: enabled
        tkhd.extend_from_slice(&creation_time.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // track_id 1
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // reserved
        tkhd.extend_from_slice(&duration.to_be_bytes()); // track duration
        tkhd.extend_from_slice(&[0; 8]); // reserved
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // layer
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // volume
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // reserved
        // 36 bytes identity matrix
        tkhd.extend_from_slice(&0x00010000u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0x00010000u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0x40000000u32.to_be_bytes());
        tkhd.extend_from_slice(&(width << 16).to_be_bytes()); // width 16.16
        tkhd.extend_from_slice(&(height << 16).to_be_bytes()); // height 16.16

        // mdia box
        let mut mdia = Vec::new();
        // hdlr box
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0, 0, 0, 0]); // version & flags
        hdlr.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
        hdlr.extend_from_slice(b"vide"); // handler_type
        hdlr.extend_from_slice(&[0; 12]); // reserved
        hdlr.extend_from_slice(b"VideoHandler\0");
        write_box(&mut mdia, b"hdlr", &hdlr);

        // trak box
        let mut trak = Vec::new();
        write_box(&mut trak, b"tkhd", &tkhd);
        write_box(&mut trak, b"mdia", &mdia);

        // moov box
        let mut moov = Vec::new();
        write_box(&mut moov, b"mvhd", &mvhd);
        write_box(&mut moov, b"trak", &trak);
        if let Some(fragment_duration) = fragment_duration {
            // mehd is a full box: version and flags, then the fragment duration (movie timescale)
            let mut mehd = vec![0, 0, 0, 0];
            mehd.extend_from_slice(&fragment_duration.to_be_bytes());
            let mut mvex = Vec::new();
            write_box(&mut mvex, b"mehd", &mehd);
            write_box(&mut moov, b"mvex", &mvex);
        }

        write_box(&mut out, b"moov", &moov);
        out
    }

    /// Helper to write an ISO-BMFF box header and payload into a byte buffer.
    fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        let size = (payload.len() + 8) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
    }

    /// Probing synthetic MP4 bytes correctly extracts duration, dimensions, orientation, and capture date.
    #[test]
    fn synthetic_mp4_probes_duration_dimensions_and_date() {
        // 1_786_692_000 Unix seconds = 2026-08-14T07:20:00Z; in the Mac 1904 epoch that is
        // 1_786_692_000 + MAC_EPOCH_OFFSET_SECONDS.
        let unix_secs = 1_786_692_000u64;
        let mac_secs = (unix_secs + MAC_EPOCH_OFFSET_SECONDS) as u32;

        // 600 ticks at 60 fps = 10.0 seconds; 1920x1080 resolution
        let bytes = synthetic_mp4(60, 600, 1920, 1080, mac_secs);
        let meta = probe_video(&bytes).expect("valid video");

        assert_eq!(meta.duration, Some(10.0));
        assert_eq!(meta.width, 1920);
        assert_eq!(meta.height, 1080);
        assert_eq!(meta.orientation, Some(Orientation::Normal));
        assert_eq!(meta.captured, Some("2026-08-14T07:20:00Z".to_string()));
        assert_eq!(meta.metadata.width, Some(1920));
        assert_eq!(meta.metadata.height, Some(1080));
    }

    /// A fragmented MP4 with empty moov reads its running time from the movie extends header (mehd).
    #[test]
    fn fragmented_movie_takes_its_duration_from_mehd() {
        // empty_moov leaves mvhd's duration 0; the fragment duration lives in mvex/mehd
        let bytes = synthetic_mp4_with_fragment(1000, 0, 640, 360, 0, Some(3_000));
        let meta = probe_video(&bytes).expect("valid video");
        assert_eq!(meta.duration, Some(3.0));
    }

    /// A movie that states no positive duration has none to show: the UI prints no time rather
    /// than "00:00" for a clip that plays.
    #[test]
    fn a_zero_or_missing_duration_is_unknown() {
        let zero = synthetic_mp4(1000, 0, 640, 360, 0);
        let meta = probe_video(&zero).expect("a video track makes a valid video");
        assert_eq!(meta.duration, None);
        assert_eq!(meta.width, 640);

        // ISO/IEC 14496-12 all-ones sentinel duration (0xFFFFFFFF in v0) is unknown
        let ones = synthetic_mp4(1000, u32::MAX, 640, 360, 0);
        let meta_ones = probe_video(&ones).expect("valid video");
        assert_eq!(meta_ones.duration, None);
    }

    /// An audio-only container with mvhd duration but no video track is rejected.
    #[test]
    fn movie_with_duration_but_no_video_track_is_rejected() {
        // An audio-only or empty container has mvhd duration but no video trak
        let mut out = Vec::new();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"mp42\0\0\0\0isommp42");
        write_box(&mut out, b"ftyp", &ftyp);
        let mut mvhd = Vec::new();
        mvhd.push(0);
        mvhd.extend_from_slice(&[0, 0, 0]);
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // creation
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // mod
        mvhd.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd.extend_from_slice(&5000u32.to_be_bytes()); // duration 5s
        mvhd.extend_from_slice(&[0; 80]); // rest of mvhd
        let mut moov = Vec::new();
        write_box(&mut moov, b"mvhd", &mvhd);
        write_box(&mut out, b"moov", &moov);

        assert_eq!(probe_video(&out), None, "a movie with duration but no video track is rejected");
    }

    /// 90-degree matrix rotation in the display matrix is detected and mapped to EXIF orientation.
    /// Append child boxes to the synthetic file's `moov` (the `meta` box, for the mdta test).
    fn with_moov_children(bytes: &[u8], children: &[u8]) -> Vec<u8> {
        let ftyp_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let moov_len = u32::from_be_bytes([bytes[ftyp_len], bytes[ftyp_len + 1], bytes[ftyp_len + 2], bytes[ftyp_len + 3]]) as usize;
        let mut body = bytes[ftyp_len + 8..ftyp_len + moov_len].to_vec();
        body.extend_from_slice(children);
        let mut out = bytes[..ftyp_len].to_vec();
        write_box(&mut out, b"moov", &body);
        out
    }

    /// Grow the (last) `trak` of the synthetic movie, keeping the enclosing `moov` size honest.
    fn with_trak_children(bytes: &[u8], children: &[u8]) -> Vec<u8> {
        let ftyp_len = be32(bytes, 0).expect("ftyp size") as usize;
        let trak_at = bytes.windows(4).position(|w| w == b"trak").expect("trak") - 4;
        let trak_len = be32(bytes, trak_at).expect("trak size") as usize;
        assert_eq!(trak_at + trak_len, bytes.len(), "the fixture's trak ends the file");
        let mut out = bytes.get(..trak_at).expect("trak start").to_vec();
        let mut body = bytes.get(trak_at + 8..trak_at + trak_len).expect("trak body").to_vec();
        body.extend_from_slice(children);
        write_box(&mut out, b"trak", &body);
        let moov_len = u32::try_from(out.len() - ftyp_len).expect("small fixture");
        out.get_mut(ftyp_len..ftyp_len + 4).expect("moov size")[..].copy_from_slice(&moov_len.to_be_bytes());
        out
    }

    /// Append a second video track (a copy of the fixture's `trak`, resized) to the synthetic movie.
    fn with_extra_track(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
        let ftyp_len = be32(bytes, 0).expect("ftyp size") as usize;
        let trak_at = bytes.windows(4).position(|w| w == b"trak").expect("trak") - 4;
        let trak_len = be32(bytes, trak_at).expect("trak size") as usize;
        let mut second = bytes.get(trak_at..trak_at + trak_len).expect("trak body").to_vec();
        // `trak` header (8) + `tkhd` header (8) + the v0 body's width/height offsets (76/80).
        second.get_mut(92..96).expect("track width")[..].copy_from_slice(&(width << 16).to_be_bytes());
        second.get_mut(96..100).expect("track height")[..].copy_from_slice(&(height << 16).to_be_bytes());
        let mut out = bytes.get(..trak_at + trak_len).expect("movie ending at its track").to_vec();
        out.extend_from_slice(&second);
        let moov_len = u32::try_from(out.len() - ftyp_len).expect("small fixture");
        out.get_mut(ftyp_len..ftyp_len + 4).expect("moov size")[..].copy_from_slice(&moov_len.to_be_bytes());
        out
    }

    /// Grow the (last) `mdia` of the synthetic movie, keeping the enclosing `trak` and `moov` sizes honest.
    fn with_mdia_children(bytes: &[u8], children: &[u8]) -> Vec<u8> {
        let ftyp_len = be32(bytes, 0).expect("ftyp size") as usize;
        let trak_at = bytes.windows(4).position(|w| w == b"trak").expect("trak") - 4;
        let mdia_at = bytes.windows(4).position(|w| w == b"mdia").expect("mdia") - 4;
        let mdia_len = be32(bytes, mdia_at).expect("mdia size") as usize;
        assert_eq!(mdia_at + mdia_len, bytes.len(), "the fixture's mdia ends the file");
        let mut out = bytes.get(..mdia_at).expect("mdia start").to_vec();
        let mut body = bytes.get(mdia_at + 8..mdia_at + mdia_len).expect("mdia body").to_vec();
        body.extend_from_slice(children);
        write_box(&mut out, b"mdia", &body);
        let trak_len = u32::try_from(out.len() - trak_at).expect("small fixture");
        out.get_mut(trak_at..trak_at + 4).expect("trak size")[..].copy_from_slice(&trak_len.to_be_bytes());
        let moov_len = u32::try_from(out.len() - ftyp_len).expect("small fixture");
        out.get_mut(ftyp_len..ftyp_len + 4).expect("moov size")[..].copy_from_slice(&moov_len.to_be_bytes());
        out
    }

    /// A QuickTime `meta` box (mdta): `keys` names the fields, `ilst` items hold their values.
    fn mdta_meta_box(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = vec![0u8; 4];
        keys.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        for (key, _) in fields {
            keys.extend_from_slice(&((8 + key.len()) as u32).to_be_bytes());
            keys.extend_from_slice(b"mdta");
            keys.extend_from_slice(key.as_bytes());
        }
        let mut ilst = Vec::new();
        for (i, (_, value)) in fields.iter().enumerate() {
            let mut data = 1u32.to_be_bytes().to_vec();
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(value.as_bytes());
            let mut data_box = Vec::new();
            write_box(&mut data_box, b"data", &data);
            let size = (data_box.len() + 8) as u32;
            ilst.extend_from_slice(&size.to_be_bytes());
            ilst.extend_from_slice(&u32::try_from(i + 1).expect("few fields").to_be_bytes());
            ilst.extend_from_slice(&data_box);
        }
        let mut body = vec![0u8; 4];
        let mut keys_box = Vec::new();
        write_box(&mut keys_box, b"keys", &keys);
        body.extend_from_slice(&keys_box);
        let mut ilst_box = Vec::new();
        write_box(&mut ilst_box, b"ilst", &ilst);
        body.extend_from_slice(&ilst_box);
        let mut out = Vec::new();
        write_box(&mut out, b"meta", &body);
        out
    }

    /// A 64-bit mdat before moov is skipped by the box walk, not treated as metadata.
    #[test]
    fn mdat_before_moov_and_a_64_bit_mdat_size() {
        let file = synthetic_mp4(60, 600, 1920, 1080, 0);
        let ftyp_len = u32::from_be_bytes([file[0], file[1], file[2], file[3]]) as usize;
        let mut out = file[..ftyp_len].to_vec();
        // a 64-bit `mdat` between ftyp and moov: the reader seeks over it, not through it
        let payload = [0u8; 64];
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&((16 + payload.len()) as u64).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&file[ftyp_len..]);
        let meta = probe_video(&out).expect("valid video");
        assert_eq!((meta.duration, meta.width, meta.height), (Some(10.0), 1920, 1080));
    }

    /// Apple mdta creation date, make and model are read from meta/keys/ilst.
    #[test]
    fn quicktime_creation_date_and_camera_come_from_mdta() {
        let file = synthetic_mp4(60, 600, 1920, 1080, 0);
        let meta_box = mdta_meta_box(&[
            ("com.apple.quicktime.creationdate", "2026-05-04T21:00:00-0400"),
            ("com.apple.quicktime.make", "Apple"),
            ("com.apple.quicktime.model", "iPhone 17 Pro"),
        ]);
        let bytes = with_moov_children(&file, &meta_box);
        let meta = probe_video(&bytes).expect("valid video");
        assert_eq!(meta.captured.as_deref(), Some("2026-05-04T21:00:00-04:00"), "the local time and its offset");
        assert_eq!(meta.metadata.make.as_deref(), Some("Apple"));
        assert_eq!(meta.metadata.model.as_deref(), Some("iPhone 17 Pro"));
    }

    #[test]
    fn quicktime_and_fullbox_metadata_share_extraction_and_location_checks() {
        let file = synthetic_mp4(60, 600, 1920, 1080, 0);
        for quicktime in [false, true] {
            for located in [false, true] {
                let mut fields = vec![
                    ("com.apple.quicktime.creationdate", "2026-05-04T21:00:00-0400"),
                    ("com.apple.quicktime.make", "Apple"),
                    ("com.apple.quicktime.model", "iPhone 17 Pro"),
                ];
                if located {
                    fields.push(("com.apple.quicktime.location.ISO6709", "+37.7749-122.4194/"));
                }
                let meta = mdta_meta_box(&fields);
                let children = &meta[12..];
                let mut body = Vec::new();
                if quicktime {
                    let mut handler = vec![0; 8];
                    handler.extend_from_slice(b"mdta");
                    handler.extend_from_slice(&[0; 12]);
                    write_box(&mut body, b"hdlr", &handler);
                } else {
                    body.extend_from_slice(&0x100u32.to_be_bytes());
                }
                body.extend_from_slice(children);
                let mut atom = Vec::new();
                write_box(&mut atom, b"meta", &body);
                let bytes = with_moov_children(&file, &atom);
                let parsed = probe_video(&bytes).unwrap();
                assert_eq!(parsed.captured.as_deref(), Some("2026-05-04T21:00:00-04:00"), "QuickTime layout: {quicktime}");
                let metadata = parsed.metadata;
                assert_eq!(metadata.make.as_deref(), Some("Apple"), "QuickTime layout: {quicktime}");
                assert_eq!(metadata.model.as_deref(), Some("iPhone 17 Pro"), "QuickTime layout: {quicktime}");
                assert_eq!(metadata.gps.is_some(), located);
                let checked = verify_video_location_absent(&bytes);
                if located {
                    assert!(checked.unwrap_err().contains("GPS"), "QuickTime layout: {quicktime}");
                } else {
                    checked.unwrap();
                }
                atom.pop();
                let truncated = with_moov_children(&file, &atom);
                assert!(verify_video_location_absent(&truncated).is_err());
            }
        }
        assert_eq!(meta_children(&[0; 3]), None);
    }

    #[test]
    fn location_verification_rejects_a_malformed_mdta_value_box() {
        let file = synthetic_mp4(60, 600, 1920, 1080, 0);

        let mut keys = vec![0; 4]; // FullBox version and flags
        keys.extend_from_slice(&1u32.to_be_bytes());
        keys.extend_from_slice(&(8u32 + LOCATION_KEY.len() as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(LOCATION_KEY);
        let mut keys_box = Vec::new();
        write_box(&mut keys_box, b"keys", &keys);

        // The item box itself is bounded and well-formed, but its `data` child claims bytes beyond
        // the item. Best-effort metadata parsing skips that child; a privacy check must fail closed.
        let value = b"+37.7749-122.4194/";
        let mut bad_data = Vec::new();
        bad_data.extend_from_slice(&((16 + value.len() + 8) as u32).to_be_bytes());
        bad_data.extend_from_slice(b"data");
        bad_data.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
        bad_data.extend_from_slice(value);
        let mut ilst = Vec::new();
        write_box(&mut ilst, &1u32.to_be_bytes(), &bad_data);
        let mut ilst_box = Vec::new();
        write_box(&mut ilst_box, b"ilst", &ilst);

        let mut meta_body = vec![0; 4]; // ISO FullBox version and flags
        meta_body.extend_from_slice(&keys_box);
        meta_body.extend_from_slice(&ilst_box);
        let mut meta = Vec::new();
        write_box(&mut meta, b"meta", &meta_body);
        let bytes = with_moov_children(&file, &meta);

        let error = verify_video_location_absent(&bytes).unwrap_err();
        assert!(error.contains("malformed") || error.contains("unreadable"), "{error}");
    }

    /// A 90-degree display matrix is read as Rotate90.
    #[test]
    fn rotated_90_degrees_matrix_detected() {
        let mut bytes = synthetic_mp4(1000, 5000, 1920, 1080, 0);
        // Find tkhd and alter matrix to 90 degrees clockwise:
        // a=0, b=1.0, c=-1.0, d=0
        let tkhd_pos = bytes.windows(4).position(|w| w == b"tkhd").expect("has tkhd");
        let matrix_pos = tkhd_pos + 4 + 40; // skip 'tkhd' header + 40 bytes to matrix
        let mut rot90 = Vec::new();
        rot90.extend_from_slice(&0u32.to_be_bytes()); // a = 0
        rot90.extend_from_slice(&0x00010000u32.to_be_bytes()); // b = 1.0
        rot90.extend_from_slice(&0u32.to_be_bytes());
        rot90.extend_from_slice(&(-0x10000i32 as u32).to_be_bytes()); // c = -1.0
        rot90.extend_from_slice(&0u32.to_be_bytes()); // d = 0
        bytes[matrix_pos..matrix_pos + 20].copy_from_slice(&rot90);

        let meta = probe_video(&bytes).expect("valid video");
        assert_eq!(meta.orientation, Some(Orientation::Rotate90));
    }

    /// Truncated or corrupted video buffers never panic during box parsing.
    #[test]
    fn quicktime_text_stops_at_its_declared_length() {
        let mut body = vec![0, 5, 0, 0];
        body.extend_from_slice(b"HelloTRAILING");
        assert_eq!(read_udta_text(&body).as_deref(), Some("Hello"));
        // a length past the box falls back to what the box holds
        let mut bogus = vec![0xff, 0xff, 0, 0];
        bogus.extend_from_slice(b"ok");
        assert_eq!(read_udta_text(&bogus).as_deref(), Some("ok"));
    }

    /// A matrix entry read from a hostile file is never negated in a way that overflows: the
    /// extreme values are "not a rotation", not a panic (debug) or a misread (release).
    #[test]
    fn hostile_matrix_entries_do_not_overflow_or_panic() {
        for value in [i32::MIN, i32::MAX, i32::MIN + 0x10000, i32::MAX - 0x10000] {
            let mut bytes = synthetic_mp4(1000, 5000, 1920, 1080, 0);
            let tkhd_pos = bytes.windows(4).position(|w| w == b"tkhd").expect("has tkhd");
            let matrix_pos = tkhd_pos + 4 + 40;
            for i in 0..9 {
                bytes[matrix_pos + i * 4..matrix_pos + i * 4 + 4].copy_from_slice(&value.to_be_bytes());
            }
            let meta = probe_video(&bytes).expect("still a video");
            assert_eq!(meta.orientation, None, "{value:#x} is not a rotation");
        }
    }

    /// Empty, short and every truncated prefix of a synthetic movie return gracefully.
    #[test]
    fn corrupted_and_truncated_buffers_do_not_panic() {
        assert_eq!(probe_video(b""), None);
        assert_eq!(probe_video(b"short"), None);
        assert_eq!(probe_video(b"\0\0\0\x20ftypmp42\0\0\0\0"), None);

        let bytes = synthetic_mp4(60, 600, 1920, 1080, 0);
        // Truncate at every single byte
        for len in 0..bytes.len() {
            let _ = probe_video(&bytes[..len]);
        }
    }

    /// Valid and malformed ISO 6709 coordinates are parsed correctly or rejected cleanly.
    #[test]
    fn iso6709_parser_handles_valid_and_malformed() {
        let g1 = parse_iso6709("+37.5090-122.2520/").unwrap();
        assert!((g1.latitude - 37.5090).abs() < 1e-5);
        assert!((g1.longitude - (-122.2520)).abs() < 1e-5);
        assert_eq!(g1.altitude, None);

        let g2 = parse_iso6709("+35.6586+139.7454+035.100/").unwrap();
        assert!((g2.latitude - 35.6586).abs() < 1e-5);
        assert!((g2.longitude - 139.7454).abs() < 1e-5);
        assert_eq!(g2.altitude, Some(35.1));

        assert_eq!(parse_iso6709("bad"), None);
        assert_eq!(parse_iso6709("+95.0000+10.0000/"), None); // latitude > 90
        assert_eq!(parse_iso6709("+10.0000+190.0000/"), None); // longitude > 180
    }
}
