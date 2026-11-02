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
/// Maximum number of boxes to inspect.
const MAX_BOXES: usize = 1024;
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

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at + 8)?;
    Some(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// Convert Apple 1904-epoch seconds to ISO 8601 UTC string (`YYYY-MM-DDTHH:MM:SS`).
fn mac_time_to_iso(secs: u64) -> Option<String> {
    if secs <= MAC_EPOCH_OFFSET_SECONDS {
        return None;
    }
    let unix_secs = i64::try_from(secs.checked_sub(MAC_EPOCH_OFFSET_SECONDS)?).ok()?;
    let dt = chrono::DateTime::from_timestamp(unix_secs, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
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

    // Threshold around 1.0 (0x10000 = 65536) allowing minor fixed-point rounding
    let is_one = |v: i32| (v - 0x10000).abs() < 0x200;
    let is_neg_one = |v: i32| (v + 0x10000).abs() < 0x200;
    let is_zero = |v: i32| v.abs() < 0x200;

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

fn parse_trak(body: &[u8], budget: &mut usize, depth: usize) -> Option<TrackInfo> {
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
            b"mdia" => {
                // mdia contains hdlr and minf
                for child in read_boxes(b.body, budget) {
                    if &child.kind == b"hdlr" {
                        // handler type at body + 8..12
                        if child.body.get(8..12) == Some(b"vide") {
                            is_video = true;
                        }
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
    // Classic QuickTime atom: skip 2 bytes length and 2 bytes language
    if body.len() >= 4 {
        let str_bytes = body.get(4..)?;
        return String::from_utf8(str_bytes.to_vec()).ok().or_else(|| Some(String::from_utf8_lossy(str_bytes).into_owned()));
    }
    None
}

/// Parse metadata inside `udta` container box.
fn parse_udta(body: &[u8], meta: &mut Metadata, xmp: &mut Option<String>, budget: &mut usize, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for b in read_boxes(body, budget) {
        match &b.kind {
            b"XMP_" => {
                if let Ok(s) = std::str::from_utf8(b.body) {
                    *xmp = Some(s.to_string());
                }
            }
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
            b"meta" => {
                // QuickTime `meta` atom inside `udta` has 4 bytes version/flags before child boxes
                let child_bytes = b.body.get(4..).unwrap_or(b.body);
                parse_udta(child_bytes, meta, xmp, budget, depth + 1);
            }
            _ => {}
        }
    }
}

/// Extract any embedded XMP packet found in an ISO-BMFF / QuickTime container.
pub fn extract_video_xmp(bytes: &[u8]) -> Option<String> {
    let mut budget = MAX_BOXES;
    for b in read_boxes(bytes, &mut budget) {
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
            }
        }
    }
    None
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
                            let dur = be32(child.body, 16).map(u64::from);
                            (ts, dur, creat)
                        } else {
                            let creat = be64(child.body, 4);
                            let ts = be32(child.body, 20);
                            let dur = be64(child.body, 24);
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
                        if let Some(t) = parse_trak(child.body, &mut budget, 0)
                            && t.is_video
                            && (primary_track.is_none() || (t.width > 0 && t.height > 0))
                        {
                            primary_track = Some(t);
                        }
                    }
                    // A fragmented movie (`empty_moov`: the file is a series of fragments) writes
                    // 0 in `mvhd`; its whole duration is the fragment duration in `mvex` → `mehd`.
                    b"mvex" => {
                        for grand in read_boxes(child.body, &mut budget) {
                            if &grand.kind == b"mehd" {
                                let ver = grand.body.first().copied().unwrap_or(0);
                                let d = if ver == 0 { be32(grand.body, 4).map(u64::from) } else { be64(grand.body, 4) };
                                if let Some(d) = d.filter(|d| *d > 0) {
                                    fragment_duration = Some(d);
                                }
                            }
                        }
                    }
                    b"udta" => {
                        parse_udta(child.body, &mut meta, &mut xmp, &mut budget, 0);
                    }
                    b"uuid" if child.uuid == Some(XMP_UUID) => {
                        if let Ok(s) = std::str::from_utf8(child.body) {
                            xmp = Some(s.to_string());
                        }
                    }
                    _ => {}
                }
            }
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

    // If capture time from `mvhd` was found and `meta.capture_time` was not set, fill it
    if meta.capture_time.is_none() {
        if let Some(c) = captured.as_deref().and_then(DateTime::parse_iso) {
            meta.capture_time = Some(c);
        }
    } else if captured.is_none() {
        captured = meta.capture_time.map(|c| c.to_iso());
    }

    let (width, height, orientation) = match primary_track {
        Some(t) => (t.width, t.height, t.orientation),
        None => (0, 0, None),
    };

    meta.width = (width > 0).then_some(width);
    meta.height = (height > 0).then_some(height);
    meta.orientation = orientation;

    // A valid video probe must have at least a duration or a video track
    if duration.is_some() || width > 0 || height > 0 {
        Some(VideoMeta { duration, width, height, orientation, captured, metadata: meta })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        let size = (payload.len() + 8) as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
    }

    #[test]
    fn synthetic_mp4_probes_duration_dimensions_and_date() {
        // Mac timestamp for 2026-08-14T10:00:00 UTC:
        // 2026-08-14 is 56 years + 225 days after 1970-01-01.
        // Unix secs for 2026-08-14T10:00:00Z: 1786692000
        let unix_secs = 1_786_692_000u64;
        let mac_secs = (unix_secs + MAC_EPOCH_OFFSET_SECONDS) as u32;

        // 600 ticks at 60 fps = 10.0 seconds; 1920x1080 resolution
        let bytes = synthetic_mp4(60, 600, 1920, 1080, mac_secs);
        let meta = probe_video(&bytes).expect("valid video");

        assert_eq!(meta.duration, Some(10.0));
        assert_eq!(meta.width, 1920);
        assert_eq!(meta.height, 1080);
        assert_eq!(meta.orientation, Some(Orientation::Normal));
        assert_eq!(meta.captured, Some("2026-08-14T07:20:00".to_string()));
        assert_eq!(meta.metadata.width, Some(1920));
        assert_eq!(meta.metadata.height, Some(1080));
    }

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
    }

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
