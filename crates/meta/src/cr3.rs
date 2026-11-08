//! Canon CR3: the ISO base media file container (ISO/IEC 14496-12 box structure; CR3 layout from Laurent
//! Clévy's public CR3 format notes, confirmed black-box on EOS R6 Mark III files).
//!
//! - `ftyp` with major brand `crx `.
//! - `moov` holds a Canon `uuid` box (85c0b687-820f-11e0-8111-f4ce462b6a48) whose children are `CMT1` (a TIFF
//!   stream: IFD0), `CMT2` (TIFF: the Exif IFD), `CMT3` (TIFF: the Canon maker note), `CMT4` (TIFF: GPS) and the
//!   `THMB` thumbnail, then one `trak` per stream: a full-size JPEG, a reduced raw, the full raw (`CRAW` sample
//!   entries; raws carry a `CMP1` coding header) and timed metadata (`CTMD`).
//! - A top-level `uuid` box with the XMP uuid (be7acfcb-97a9-42e8-9c71-999491e3afac, XMP spec part 3) holds the
//!   XMP packet.
//!
//! Every size and offset is checked against the buffer; nesting and box counts are bounded.

/// A track's sample-entry kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cr3TrackKind {
    /// `CRAW` with a `JPEG` child: the full-size JPEG.
    Jpeg,
    /// `CRAW` raw image: dimensions and the `CMP1` coding header (byte range in the file).
    Raw {
        width: u16,
        height: u16,
        cmp1: Option<(usize, usize)>,
        /// `CDI1/IAD1` image-area descriptor (byte range in the file).
        iad1: Option<(usize, usize)>,
    },
    /// Anything else (`CTMD` timed metadata, unknown entries).
    Other([u8; 4]),
}

/// One track's single sample: where it is in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cr3Track {
    pub kind: Cr3TrackKind,
    /// Byte offset and length of the sample (the first one) in the file, when the tables give them.
    pub data: Option<(usize, usize)>,
}

/// Canon's `CMP1` image coding descriptor, separate from the encoded track sample.
///
/// Parsed from the payload (after the eight-byte box header). Geometry is the full sensor mosaic,
/// including optically masked borders. Unsupported encoding modes are retained for the decoder to
/// report explicitly, rather than confused with the full-size embedded JPEG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cr3Compression {
    pub version: u16,
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub bit_depth: u8,
    pub planes: u8,
    /// 0 = RGGB, 1 = GRBG, 2 = GBRG, 3 = BGGR.
    pub cfa_pattern: u8,
    pub encoding: u8,
    pub levels: u8,
    pub tile_flags: u8,
    /// Number of bytes at the start of the sample before its entropy-coded data.
    pub header_size: u32,
    pub median_bit_depth: Option<u8>,
}

impl Cr3Compression {
    /// Read the public `CMP1` field layout. Truncated or impossible descriptors return `None`.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        // The first word is reserved in the public notes; real files use FF00, although older
        // descriptions guessed FFFF. It is not a reliable format signature.
        let header_len = usize::from(be16(payload, 2)?).checked_add(4)?;
        if header_len < 52 || header_len > payload.len() {
            return None;
        }
        let packed = *payload.get(25)?;
        let coding = *payload.get(26)?;
        let mut out = Self {
            version: be16(payload, 4)?,
            width: be32(payload, 8)?,
            height: be32(payload, 12)?,
            tile_width: be32(payload, 16)?,
            tile_height: be32(payload, 20)?,
            bit_depth: *payload.get(24)?,
            planes: packed >> 4,
            cfa_pattern: packed & 15,
            encoding: coding >> 4,
            levels: coding & 15,
            tile_flags: *payload.get(27)?,
            header_size: be32(payload, 28)?,
            median_bit_depth: None,
        };
        if out.width == 0
            || out.height == 0
            || out.tile_width == 0
            || out.tile_height == 0
            || out.tile_width > out.width
            || out.tile_height > out.height
            || !(1..=16).contains(&out.bit_depth)
            || !(1..=4).contains(&out.planes)
            || (out.planes > 1 && out.cfa_pattern > 3)
            || out.header_size == 0
        {
            return None;
        }
        if payload.get(32).is_some_and(|flags| flags & 0x80 != 0) {
            // Extended CMP1: a four-byte length, flags, reserved bytes and six words. The optional
            // median precision is used by type-3 burst encodings, not ordinary Bayer raw/craw.
            let extension_len = usize::try_from(be32(payload, 52)?).ok()?;
            let extension_end = 52usize.checked_add(extension_len)?;
            if extension_len < 32 || extension_end > payload.len() {
                return None;
            }
            if payload.get(56).is_some_and(|flags| flags & 0x40 != 0) {
                let precision = *payload.get(84).filter(|_| extension_end > 84)?;
                if !(1..=16).contains(&precision) {
                    return None;
                }
                out.median_bit_depth = Some(precision);
            }
        }
        Some(out)
    }
}

/// `IAD1` geometry. Coordinates are inclusive sensor offsets: `[left, top, right, bottom]`.
/// A full raw descriptor distinguishes valid sensor pixels from the recommended final crop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cr3ImageArea {
    pub width: u16,
    pub height: u16,
    pub crop: [u16; 4],
    pub active: Option<[u16; 4]>,
    pub masked_left: [u16; 4],
    pub masked_top: Option<[u16; 4]>,
}

impl Cr3ImageArea {
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let bounds = |at: usize| Some([be16(payload, at)?, be16(payload, at + 2)?, be16(payload, at + 4)?, be16(payload, at + 6)?]);
        let width = be16(payload, 4)?;
        let height = be16(payload, 6)?;
        if width == 0 || height == 0 {
            return None;
        }
        let full = be16(payload, 10)? == 2;
        Some(Self {
            width,
            height,
            crop: bounds(16)?,
            masked_left: bounds(24)?,
            masked_top: if full { Some(bounds(32)?) } else { None },
            active: if full { Some(bounds(40)?) } else { None },
        })
    }
}

impl Cr3Track {
    /// Decode this track's image coding descriptor without reading its pixel data.
    pub fn compression(&self, bytes: &[u8]) -> Option<Cr3Compression> {
        let Cr3TrackKind::Raw { cmp1: Some((at, len)), .. } = self.kind else { return None };
        Cr3Compression::parse(bytes.get(at..at.checked_add(len)?)?)
    }

    pub fn image_area(&self, bytes: &[u8]) -> Option<Cr3ImageArea> {
        let Cr3TrackKind::Raw { iad1: Some((at, len)), .. } = self.kind else { return None };
        Cr3ImageArea::parse(bytes.get(at..at.checked_add(len)?)?)
    }
}

/// The parts of a CR3 file LightCraft reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cr3<'a> {
    /// `CMT1`…`CMT4` (index 0…3).
    pub cmt: [Option<&'a [u8]>; 4],
    pub thumbnail: Option<&'a [u8]>,
    pub xmp: Option<&'a [u8]>,
    pub tracks: Vec<Cr3Track>,
}

/// One independent TIFF block from Canon timed metadata. Its offsets remain relative to this block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cr3TimedExif<'a> {
    /// CTMD record type (7, 8 or 9).
    pub record_type: u16,
    /// Exif IFD (`0x8769`) or Canon maker note (`0x927c`).
    pub tag: u32,
    pub data: &'a [u8],
}

impl Cr3<'_> {
    /// Read TIFF blocks in the first samples of CTMD tracks. As-shot white balance and sensor
    /// levels often live here rather than in the static `CMT3` maker note. Both record layers use
    /// little-endian byte lengths; TIFF blocks retain their own byte order and offset origin.
    pub fn timed_exif<'a>(&self, bytes: &'a [u8]) -> Vec<Cr3TimedExif<'a>> {
        let mut out = Vec::new();
        let mut budget = MAX_BOXES;
        for track in &self.tracks {
            if track.kind != Cr3TrackKind::Other(*b"CTMD") {
                continue;
            }
            let Some(sample) = track.data.and_then(|(at, len)| at.checked_add(len).and_then(|end| bytes.get(at..end))) else { continue };
            let mut at = 0usize;
            while budget > 0 {
                budget -= 1;
                let Some(len) = le32(sample, at).and_then(|value| usize::try_from(value).ok()) else { break };
                let Some(end) = at.checked_add(len).filter(|&end| len >= 12 && end <= sample.len()) else { break };
                let Some(record) = sample.get(at..end) else { break };
                let Some(kind) = le16(record, 4) else { break };
                if matches!(kind, 7..=9) {
                    let mut item_at = 12usize;
                    while budget > 0 {
                        budget -= 1;
                        let Some(item_len) = le32(record, item_at).and_then(|value| usize::try_from(value).ok()) else { break };
                        let Some(item_end) = item_at.checked_add(item_len).filter(|&end| item_len >= 8 && end <= record.len()) else { break };
                        let Some(item) = record.get(item_at..item_end) else { break };
                        if let Some(tag @ (0x8769 | 0x927c)) = le32(item, 4)
                            && let Some(data) = item.get(8..)
                        {
                            out.push(Cr3TimedExif { record_type: kind, tag, data });
                        }
                        item_at = item_end;
                    }
                }
                at = end;
            }
        }
        out
    }

    /// Read timed EXIF records only when every CTMD sample and record is structurally complete.
    pub(crate) fn timed_exif_with_status<'a>(&self, bytes: &'a [u8]) -> Option<Vec<Cr3TimedExif<'a>>> {
        let mut out = Vec::new();
        let mut budget = MAX_BOXES;
        for track in &self.tracks {
            if track.kind != Cr3TrackKind::Other(*b"CTMD") {
                continue;
            }
            let (at, len) = track.data?;
            let sample = bytes.get(at..at.checked_add(len)?)?;
            if sample.is_empty() {
                return None;
            }
            let mut at = 0usize;
            while at < sample.len() {
                if budget == 0 {
                    return None;
                }
                budget -= 1;
                let len = le32(sample, at).and_then(|value| usize::try_from(value).ok())?;
                let end = at.checked_add(len).filter(|&end| len >= 12 && end <= sample.len())?;
                let record = sample.get(at..end)?;
                let kind = le16(record, 4)?;
                if !matches!(kind, 7..=9) {
                    at = end;
                    continue;
                }
                let mut item_at = 12usize;
                while item_at < record.len() {
                    if budget == 0 {
                        return None;
                    }
                    budget -= 1;
                    let item_len = le32(record, item_at).and_then(|value| usize::try_from(value).ok())?;
                    let item_end = item_at.checked_add(item_len).filter(|&end| item_len >= 8 && end <= record.len())?;
                    let item = record.get(item_at..item_end)?;
                    if let Some(tag @ (0x8769 | 0x927c)) = le32(item, 4)
                        && let Some(data) = item.get(8..)
                    {
                        out.push(Cr3TimedExif { record_type: kind, tag, data });
                    }
                    item_at = item_end;
                }
                at = end;
            }
        }
        Some(out)
    }
}

const CANON_UUID: [u8; 16] = [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
/// The `uuid` of the top-level box holding the XMP packet.
pub const XMP_UUID: [u8; 16] = [0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac];
const MAX_DEPTH: usize = 12;
pub(crate) const MAX_BOXES: usize = 4096;
/// A `VisualSampleEntry` (78 bytes after the box header) plus 4 bytes Canon adds before the child boxes.
const CRAW_CHILDREN_AT: usize = 82;

/// Does this look like a CR3 file (`ftyp` box with major brand `crx `)?
pub fn is_cr3(bytes: &[u8]) -> bool {
    bytes.get(4..12) == Some(b"ftypcrx ".as_slice())
}

/// Parse a CR3 file's boxes; `None` when it is not a CR3 file. Of the top-level boxes only `moov`
/// and the XMP `uuid` are read (`lightcraft_raw::sparse` relies on it to skip the others).
pub fn parse_cr3(bytes: &[u8]) -> Option<Cr3<'_>> {
    if !is_cr3(bytes) {
        return None;
    }
    let mut out = Cr3::default();
    let mut budget = MAX_BOXES;
    for b in boxes(bytes, 0, bytes.len(), &mut budget) {
        match &b.kind {
            b"moov" => moov(bytes, &b, &mut out, &mut budget),
            b"uuid" if b.uuid == Some(XMP_UUID) => out.xmp = bytes.get(b.body..b.end),
            _ => {}
        }
    }
    Some(out)
}

pub(crate) struct BoxRef {
    pub(crate) kind: [u8; 4],
    /// Start of the payload (after the header and, for `uuid`, the 16-byte uuid).
    pub(crate) body: usize,
    pub(crate) end: usize,
    pub(crate) uuid: Option<[u8; 16]>,
}

/// The boxes directly inside `[from, to)`; stops at the first malformed header.
pub(crate) fn boxes(bytes: &[u8], from: usize, to: usize, budget: &mut usize) -> Vec<BoxRef> {
    boxes_with_status(bytes, from, to, budget).0
}

pub(crate) fn boxes_with_status(bytes: &[u8], from: usize, to: usize, budget: &mut usize) -> (Vec<BoxRef>, bool) {
    let mut out = Vec::new();
    let mut at = from;
    let to = to.min(bytes.len());
    if from > to {
        return (out, false);
    }
    let mut complete = true;
    while at < to {
        if at.saturating_add(8) > to || *budget == 0 {
            complete = false;
            break;
        }
        *budget -= 1;
        let (Some(size), Some(kind)) = (be32(bytes, at), bytes.get(at + 4..at + 8)) else {
            complete = false;
            break;
        };
        let mut kind4 = [0u8; 4];
        kind4.copy_from_slice(kind);
        let (len, mut hdr) = match size {
            0 => (to - at, 8),
            1 => match be64(bytes, at + 8).and_then(|v| usize::try_from(v).ok()) {
                Some(l) => (l, 16),
                None => {
                    complete = false;
                    break;
                }
            },
            n => (n as usize, 8),
        };
        let Some(end) = at.checked_add(len).filter(|&e| e <= to && len >= hdr) else {
            complete = false;
            break;
        };
        let mut uuid = None;
        if &kind4 == b"uuid" {
            let Some(uuid_at) = at.checked_add(hdr) else {
                complete = false;
                break;
            };
            let Some(uuid_end) = uuid_at.checked_add(16) else {
                complete = false;
                break;
            };
            let Some(u) = bytes.get(uuid_at..uuid_end).filter(|_| uuid_end <= end) else {
                complete = false;
                break;
            };
            let mut a = [0u8; 16];
            a.copy_from_slice(u);
            uuid = Some(a);
            hdr += 16;
        }
        out.push(BoxRef { kind: kind4, body: at + hdr, end, uuid });
        at = end;
    }
    (out, complete && at == to)
}

/// All raw CMT4 blocks in a CR3, including duplicates discarded by the convenience `Cr3::cmt` view.
pub(crate) fn cmt4_blocks(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    collect_canon_blocks(bytes, b"CMT4")
}

/// All raw CMT1 blocks in a CR3, including duplicates discarded by the convenience `Cr3::cmt` view.
pub(crate) fn cmt1_blocks(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    collect_canon_blocks(bytes, b"CMT1")
}

/// All raw CMT2 EXIF blocks in a CR3, including duplicates discarded by the convenience view.
pub(crate) fn cmt2_blocks(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    collect_canon_blocks(bytes, b"CMT2")
}

/// All raw THMB JPEG blocks in a CR3, including duplicates discarded by the convenience view.
pub(crate) fn thumbnail_blocks(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    collect_canon_blocks(bytes, b"THMB")
}

/// The JPEG bytes of a THMB block, whether stored directly or behind Canon's 16-byte preview header.
pub(crate) fn thumbnail_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.starts_with(&[0xff, 0xd8]) {
        return Some(bytes);
    }
    let length = usize::try_from(be32(bytes, 12)?).ok()?;
    if length < 4 {
        return None;
    }
    let end = 16usize.checked_add(length)?;
    let jpeg = bytes.get(16..end)?;
    let trailing = bytes.get(end..)?;
    (jpeg.starts_with(&[0xff, 0xd8]) && trailing.iter().all(|byte| *byte == 0)).then_some(jpeg)
}

/// All top-level CR3 XMP UUID payloads, including packets overwritten by `Cr3::xmp`.
pub(crate) fn xmp_packets(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    if !is_cr3(bytes) {
        return None;
    }
    let mut budget = MAX_BOXES;
    let (top, complete) = boxes_with_status(bytes, 0, bytes.len(), &mut budget);
    if !complete {
        return None;
    }
    top.into_iter().filter(|b| &b.kind == b"uuid" && b.uuid == Some(XMP_UUID)).map(|b| bytes.get(b.body..b.end)).collect()
}

fn collect_canon_blocks<'a>(bytes: &'a [u8], wanted: &[u8; 4]) -> Option<Vec<&'a [u8]>> {
    if !is_cr3(bytes) {
        return None;
    }
    let mut budget = MAX_BOXES;
    let mut out = Vec::new();
    let (top_boxes, complete) = boxes_with_status(bytes, 0, bytes.len(), &mut budget);
    if !complete {
        return None;
    }
    for top in top_boxes {
        if &top.kind != b"moov" {
            continue;
        }
        let (children, complete) = boxes_with_status(bytes, top.body, top.end, &mut budget);
        if !complete {
            return None;
        }
        for child in children {
            if &child.kind != b"uuid" || child.uuid != Some(CANON_UUID) {
                continue;
            }
            let (metadata_boxes, complete) = boxes_with_status(bytes, child.body, child.end, &mut budget);
            if !complete {
                return None;
            }
            for metadata in metadata_boxes {
                if &metadata.kind == wanted {
                    out.push(bytes.get(metadata.body..metadata.end)?);
                }
            }
        }
    }
    (budget > 0).then_some(out)
}

/// Whether CR3 box lists used by the Original-copy guard were fully traversable.
pub(crate) fn source_boxes_are_complete(bytes: &[u8], parsed_track_count: usize) -> bool {
    if !is_cr3(bytes) {
        return false;
    }
    let mut budget = MAX_BOXES;
    let (top, complete) = boxes_with_status(bytes, 0, bytes.len(), &mut budget);
    if !complete {
        return false;
    }
    let mut found_moov = false;
    let mut track_count = 0usize;
    for box_ref in top {
        if &box_ref.kind != b"moov" {
            continue;
        }
        found_moov = true;
        let (children, complete) = boxes_with_status(bytes, box_ref.body, box_ref.end, &mut budget);
        if !complete {
            return false;
        }
        for child in children {
            if &child.kind == b"uuid" && child.uuid == Some(CANON_UUID) {
                let (_, complete) = boxes_with_status(bytes, child.body, child.end, &mut budget);
                if !complete {
                    return false;
                }
            } else if &child.kind == b"trak" {
                track_count = track_count.saturating_add(1);
                if !track_boxes_are_complete(bytes, &child, 0, &mut budget) {
                    return false;
                }
            }
        }
    }
    found_moov && track_count == parsed_track_count && budget > 0
}

/// Validate a track's nested sample tables and reject tables whose additional samples cannot be scanned.
fn track_boxes_are_complete(bytes: &[u8], parent: &BoxRef, depth: usize, budget: &mut usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let (children, complete) = boxes_with_status(bytes, parent.body, parent.end, budget);
    if !complete {
        return false;
    }
    let is_sample_table = &parent.kind == b"stbl";
    let (mut stsd_count, mut stsz_count, mut stsc_count, mut offset_table_count) = (0usize, 0usize, 0usize, 0usize);
    for child in children {
        match &child.kind {
            b"mdia" | b"minf" | b"stbl" | b"edts" => {
                if !track_boxes_are_complete(bytes, &child, depth + 1, budget) {
                    return false;
                }
            }
            b"stsd" => {
                stsd_count = stsd_count.saturating_add(1);
                let Some(declared_start) = child.body.checked_add(4) else { return false };
                let Some(declared_end) = declared_start.checked_add(4) else { return false };
                let Some(declared) = bytes.get(declared_start..declared_end).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes) else {
                    return false;
                };
                if declared != 1 {
                    return false;
                }
                let Some(entries_at) = child.body.checked_add(8) else { return false };
                let (entries, complete) = boxes_with_status(bytes, entries_at, child.end, budget);
                if !complete || u32::try_from(entries.len()).ok() != Some(declared) {
                    return false;
                }
                for entry in entries {
                    if &entry.kind == b"CRAW" {
                        let Some(children_at) = entry.body.checked_add(CRAW_CHILDREN_AT) else { return false };
                        let (sample_children, complete) = boxes_with_status(bytes, children_at, entry.end, budget);
                        if !complete {
                            return false;
                        }
                        for sample_child in sample_children {
                            if &sample_child.kind == b"CDI1" {
                                let Some(areas_at) = sample_child.body.checked_add(4) else { return false };
                                let (_, complete) = boxes_with_status(bytes, areas_at, sample_child.end, budget);
                                if !complete {
                                    return false;
                                }
                            }
                        }
                    }
                }
            }
            b"stsz" => {
                stsz_count = stsz_count.saturating_add(1);
                let Some(body) = bytes.get(child.body..child.end) else { return false };
                let (Some(sample_size), Some(sample_count)) = (be32(body, 4), be32(body, 8)) else { return false };
                if sample_count != 1 || body.len() != if sample_size == 0 { 16 } else { 12 } {
                    return false;
                }
            }
            b"stsc" => {
                stsc_count = stsc_count.saturating_add(1);
                let Some(body) = bytes.get(child.body..child.end) else { return false };
                if body.len() != 20 || be32(body, 4) != Some(1) || be32(body, 8) != Some(1) || be32(body, 12) != Some(1) || be32(body, 16) != Some(1)
                {
                    return false;
                }
            }
            b"stco" | b"co64" => {
                offset_table_count = offset_table_count.saturating_add(1);
                let Some(body) = bytes.get(child.body..child.end) else { return false };
                let offset_size = if &child.kind == b"co64" { 8 } else { 4 };
                if be32(body, 4) != Some(1) || body.len() != 8 + offset_size {
                    return false;
                }
            }
            _ => {}
        }
    }
    !is_sample_table || (stsd_count == 1 && stsz_count == 1 && stsc_count == 1 && offset_table_count == 1)
}

fn moov<'a>(bytes: &'a [u8], m: &BoxRef, out: &mut Cr3<'a>, budget: &mut usize) {
    for b in boxes(bytes, m.body, m.end, budget) {
        match &b.kind {
            b"uuid" if b.uuid == Some(CANON_UUID) => {
                for c in boxes(bytes, b.body, b.end, budget) {
                    let slot = match &c.kind {
                        b"CMT1" => 0,
                        b"CMT2" => 1,
                        b"CMT3" => 2,
                        b"CMT4" => 3,
                        b"THMB" => {
                            out.thumbnail = bytes.get(c.body..c.end);
                            continue;
                        }
                        _ => continue,
                    };
                    out.cmt[slot] = bytes.get(c.body..c.end);
                }
            }
            b"trak" => {
                if let Some(t) = trak(bytes, &b, budget) {
                    out.tracks.push(t);
                }
            }
            _ => {}
        }
    }
}

/// Descend `trak` → `mdia` → `minf` → `stbl` and read its sample entry and first sample's location.
fn trak(bytes: &[u8], t: &BoxRef, budget: &mut usize) -> Option<Cr3Track> {
    let mut stbl = None;
    let mut stack = vec![(t.body, t.end, 0usize)];
    while let Some((from, to, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        for b in boxes(bytes, from, to, budget) {
            match &b.kind {
                b"mdia" | b"minf" => stack.push((b.body, b.end, depth + 1)),
                b"stbl" => stbl = Some(b),
                _ => {}
            }
        }
    }
    let stbl = stbl?;
    let (mut kind, mut size, mut offset) = (None, None, None);
    for b in boxes(bytes, stbl.body, stbl.end, budget) {
        let body = bytes.get(b.body..b.end)?;
        match &b.kind {
            // full box: version/flags, entry count, entries
            b"stsd" if be32(body, 4)? > 0 => kind = boxes(bytes, b.body + 8, b.end, budget).first().map(|e| sample_entry(bytes, e, budget)),
            // version/flags, sample_size (0 = per-sample table), count, sizes
            b"stsz" => {
                size = match be32(body, 4)? {
                    0 if be32(body, 8)? > 0 => be32(body, 12),
                    0 => None,
                    n if be32(body, 8)? > 0 => Some(n),
                    _ => None,
                }
            }
            b"co64" if be32(body, 4)? > 0 => offset = be64(body, 8).and_then(|v| usize::try_from(v).ok()),
            b"stco" if be32(body, 4)? > 0 => offset = be32(body, 8).map(|v| v as usize),
            _ => {}
        }
    }
    let data = match (offset, size) {
        (Some(o), Some(s)) if o.checked_add(s as usize).is_some_and(|e| e <= bytes.len()) => Some((o, s as usize)),
        _ => None,
    };
    Some(Cr3Track { kind: kind?, data })
}

fn sample_entry(bytes: &[u8], e: &BoxRef, budget: &mut usize) -> Cr3TrackKind {
    if &e.kind != b"CRAW" {
        return Cr3TrackKind::Other(e.kind);
    }
    let Some(body) = bytes.get(e.body..e.end) else { return Cr3TrackKind::Other(e.kind) };
    let (Some(width), Some(height)) = (be16(body, 24), be16(body, 26)) else {
        return Cr3TrackKind::Other(e.kind);
    };
    let (mut cmp1, mut iad1, mut rendered) = (None, None, None);
    for c in boxes(bytes, e.body.saturating_add(CRAW_CHILDREN_AT), e.end, budget) {
        match &c.kind {
            b"JPEG" => rendered = Some(Cr3TrackKind::Jpeg),
            // HDR-capable cameras also put an HEVC preview in a CRAW entry,
            // often with the same displayed dimensions as the main raw track.
            b"HEVC" => rendered = Some(Cr3TrackKind::Other(c.kind)),
            b"CMP1" => cmp1 = Some((c.body, c.end - c.body)),
            b"CDI1" => {
                for area in boxes(bytes, c.body.saturating_add(4), c.end, budget) {
                    if &area.kind == b"IAD1" {
                        iad1 = Some((area.body, area.end - area.body));
                    }
                }
            }
            _ => {}
        }
    }
    // A present, malformed CMP1 must remain a raw descriptor so its corruption
    // is reported. Only an explicitly identified rendered codec can establish
    // that a CRAW entry without CMP1 is a preview rather than a broken raw.
    if cmp1.is_none()
        && let Some(kind) = rendered
    {
        return kind;
    }
    Cr3TrackKind::Raw { width, height, cmp1, iad1 }
}

/// A big-endian `u16` at `at`, or `None` when the buffer is shorter than that.
pub(crate) fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
/// A big-endian `u32` at `at`, or `None` when the buffer is shorter than that.
pub(crate) fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
/// A big-endian `u64` at `at`, or `None` when the buffer is shorter than that.
pub(crate) fn be64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_be_bytes(a))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Merge `CMT1` (IFD0), `CMT2` (Exif) and `CMT4` (GPS) into one TIFF stream, so the Exif readers see a CR3
/// like any TIFF-based raw. The maker note (`CMT3`) is left out: its offsets are relative to its own block.
pub fn merged_exif(c: &Cr3<'_>) -> Option<Vec<u8>> {
    use lightcraft_tiff::{IfdBuilder, Tiff, TiffWriter, tags as t};
    let ifd0 = Tiff::parse(c.cmt[0]?).ok()?;
    let order = ifd0.order;
    let first = |i: usize| c.cmt[i].and_then(|b| Tiff::parse(b).ok()).and_then(|t| t.ifds.into_iter().next());
    // pointers and offsets that would dangle once the IFDs move
    const SKIP: [u16; 9] = [t::EXIF_IFD, t::GPS_IFD, t::INTEROP_IFD, t::SUB_IFDS, t::MAKER_NOTE, 273, 279, 513, 514];
    let build = |ifd: &lightcraft_tiff::Ifd| {
        let mut b = IfdBuilder::new();
        for e in ifd.entries.iter().filter(|e| !SKIP.contains(&e.tag)) {
            b.set(e.tag, e.value.clone());
        }
        b
    };
    let mut root = build(ifd0.ifds.first()?);
    if let Some(exif) = first(1) {
        root.set_child(t::EXIF_IFD, build(&exif));
    }
    if let Some(gps) = first(3) {
        root.set_child(t::GPS_IFD, build(&gps));
    }
    TiffWriter::new(order, false).write(&[root]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    fn uuid(u: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let mut b = u.to_vec();
        b.extend_from_slice(body);
        bx(b"uuid", &b)
    }
    fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(body);
        bx(kind, &b)
    }
    fn craw(w: u16, h: u16, children: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; CRAW_CHILDREN_AT];
        b[24..26].copy_from_slice(&w.to_be_bytes());
        b[26..28].copy_from_slice(&h.to_be_bytes());
        b.extend_from_slice(children);
        bx(b"CRAW", &b)
    }
    fn trak(entry: Vec<u8>, offset: u64, size: u32) -> Vec<u8> {
        trak_with_entries_and_samples(&[entry], offset, &[size])
    }
    fn trak_with_entries_and_samples(entries: &[Vec<u8>], offset: u64, sizes: &[u32]) -> Vec<u8> {
        let mut stsd = u32::try_from(entries.len()).unwrap().to_be_bytes().to_vec();
        stsd.extend(entries.concat());
        let mut stsz = 0u32.to_be_bytes().to_vec();
        stsz.extend(u32::try_from(sizes.len()).unwrap().to_be_bytes());
        for size in sizes {
            stsz.extend(size.to_be_bytes());
        }
        let mut stsc = 1u32.to_be_bytes().to_vec();
        stsc.extend(1u32.to_be_bytes());
        stsc.extend(u32::try_from(sizes.len()).unwrap().to_be_bytes());
        stsc.extend(1u32.to_be_bytes());
        let mut co64 = 1u32.to_be_bytes().to_vec();
        co64.extend(offset.to_be_bytes());
        let stbl = [full(b"stsd", &stsd), full(b"stsz", &stsz), full(b"stsc", &stsc), full(b"co64", &co64)].concat();
        bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))))
    }
    fn tiff_with(tag: u16, v: lightcraft_tiff::Value) -> Vec<u8> {
        let b = lightcraft_tiff::IfdBuilder::new().with(tag, v);
        lightcraft_tiff::TiffWriter::new(lightcraft_tiff::ByteOrder::Little, false).write(&[b]).unwrap()
    }

    /// A small synthetic CR3: Canon uuid with CMT1/CMT2/CMT4, a JPEG track, a raw track, XMP.
    pub(crate) fn sample() -> Vec<u8> {
        use lightcraft_tiff::{Value, tags as t};
        let cmt1 = tiff_with(t::MODEL, Value::Ascii("Canon EOS Test".into()));
        let cmt2 = tiff_with(t::ISO_SPEED, Value::Short(vec![400]));
        let cmt4 = tiff_with(1, Value::Ascii("N".into()));
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"CMT2", &cmt2), bx(b"CMT4", &cmt4), bx(b"THMB", b"thumb")].concat());
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        let payload_at = 4096u64;
        let jpeg_t = trak(craw(64, 48, &bx(b"JPEG", &[0; 4])), payload_at, 16);
        let raw_t = trak(craw(60, 40, &bx(b"CMP1", &[0xff, 0x10, 0, 0x30])), payload_at + 16, 32);
        file.extend(bx(b"moov", &[canon, jpeg_t, raw_t].concat()));
        file.extend(uuid(&XMP_UUID, b"<x:xmpmeta/>"));
        file.resize(payload_at as usize + 64, 0);
        file
    }

    #[test]
    fn parses_canon_boxes_tracks_and_xmp() {
        let f = sample();
        assert!(is_cr3(&f));
        let c = parse_cr3(&f).unwrap();
        assert!(c.cmt[0].is_some() && c.cmt[1].is_some() && c.cmt[2].is_none() && c.cmt[3].is_some());
        assert_eq!(c.thumbnail, Some(b"thumb".as_slice()));
        assert_eq!(c.xmp, Some(b"<x:xmpmeta/>".as_slice()));
        assert_eq!(c.tracks.len(), 2);
        assert_eq!(c.tracks[0], Cr3Track { kind: Cr3TrackKind::Jpeg, data: Some((4096, 16)) });
        let Cr3TrackKind::Raw { width, height, cmp1: Some((at, len)), .. } = c.tracks[1].kind else { panic!("{:?}", c.tracks[1]) };
        assert_eq!((width, height, len), (60, 40, 4));
        assert_eq!(&f[at..at + 2], &[0xff, 0x10]);
        assert_eq!(c.tracks[1].data, Some((4112, 32)));
    }

    #[test]
    fn hevc_preview_is_distinct_from_an_equal_size_raw_track() {
        let preview = trak(craw(6000, 4000, &bx(b"HEVC", &[0; 4])), 4096, 16);
        let reduced = trak(craw(1624, 1080, &bx(b"CMP1", &compression_payload(0))), 4112, 16);
        let sensor = trak(craw(6000, 4000, &bx(b"CMP1", &compression_payload(0))), 4128, 16);
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        file.extend(bx(b"moov", &[preview, reduced, sensor].concat()));
        file.resize(4144, 0);
        let container = parse_cr3(&file).unwrap();
        assert_eq!(container.tracks[0].kind, Cr3TrackKind::Other(*b"HEVC"));
        assert!(container.tracks[0].compression(&file).is_none());
        assert!(matches!(container.tracks[2].kind, Cr3TrackKind::Raw { width: 6000, height: 4000, cmp1: Some(_), .. }));
        assert_eq!(container.tracks[2].compression(&file).unwrap().width, 6888);
    }

    #[test]
    fn rendered_child_does_not_hide_a_malformed_raw_descriptor() {
        for codec in [b"JPEG", b"HEVC"] {
            for first in [true, false] {
                let rendered = bx(codec, &[0; 4]);
                let corrupt = bx(b"CMP1", &[0xff, 0]);
                let children = if first { [rendered, corrupt] } else { [corrupt, rendered] }.concat();
                let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
                file.extend(bx(b"moov", &trak(craw(6000, 4000, &children), 4096, 16)));
                file.resize(4112, 0);
                let container = parse_cr3(&file).unwrap();
                assert!(matches!(container.tracks[0].kind, Cr3TrackKind::Raw { cmp1: Some(_), .. }));
                assert!(container.tracks[0].compression(&file).is_none());
            }
        }
        // Absence alone is not evidence of a preview: retain a broken raw so
        // main-track selection cannot quietly fall back to a reduced image.
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        file.extend(bx(b"moov", &trak(craw(6000, 4000, &bx(b"free", &[])), 4096, 16)));
        file.resize(4112, 0);
        assert!(matches!(parse_cr3(&file).unwrap().tracks[0].kind, Cr3TrackKind::Raw { cmp1: None, .. }));
    }

    #[test]
    fn merged_exif_reads_like_a_tiff_raw() {
        let f = sample();
        let m = crate::read_exif(&merged_exif(&parse_cr3(&f).unwrap()).unwrap());
        assert_eq!(m.model.as_deref(), Some("Canon EOS Test"));
        assert_eq!(m.iso, Some(400));
    }

    /// The Original-copy guard checks raw CMT4 even when merging or parsing it as EXIF fails.
    #[test]
    fn source_gps_guard_checks_raw_cmt4() {
        use lightcraft_tiff::{Value, tags as t};

        let cr3_with = |cmt1: &[u8], cmt4: &[u8]| {
            let canon = uuid(&CANON_UUID, &[bx(b"CMT1", cmt1), bx(b"CMT4", cmt4)].concat());
            [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat()
        };
        let valid_cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));

        let malformed_cmt4 = cr3_with(&valid_cmt1, b"not a TIFF");
        let parsed = parse_cr3(&malformed_cmt4).unwrap();
        let merged = merged_exif(&parsed).unwrap();
        assert_eq!(crate::exif::exif_gps_tag_presence(&merged), Some(false));
        assert!(crate::source_embeds_exif_gps_location(&malformed_cmt4));

        let gps_cmt4 = tiff_with(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let valid_cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let gps_cmt4_file = cr3_with(&valid_cmt1, &gps_cmt4);
        let parsed = parse_cr3(&gps_cmt4_file).unwrap();
        let merged = merged_exif(&parsed).unwrap();
        assert_eq!(crate::exif::exif_gps_tag_presence(&merged), Some(true));
        assert_eq!(crate::exif::exif_gps_tag_presence(&gps_cmt4), Some(true), "CMT4 stores location tags in its root IFD");
        assert!(crate::source_embeds_exif_gps_location(&gps_cmt4_file));
    }

    /// A partially traversed CR3 box tree cannot establish that location-bearing data is absent.
    #[test]
    fn source_gps_guard_fails_closed_on_truncated_box_lists() {
        let valid = sample();
        let parsed = parse_cr3(&valid).unwrap();
        assert!(source_boxes_are_complete(&valid, parsed.tracks.len()));
        let canon = uuid(&CANON_UUID, b"\0\0");
        let malformed = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();
        assert!(!source_boxes_are_complete(&malformed, 0));
        assert!(crate::source_embeds_exif_gps_location(&malformed));
    }

    /// Each destination-coordinate tag in raw CMT4 is location-bearing by itself.
    #[test]
    fn source_gps_guard_checks_destination_tags_in_cmt4() {
        use lightcraft_tiff::{Value, tags as t};

        for tag in [t::GPS_DEST_LATITUDE, t::GPS_DEST_LONGITUDE] {
            let cmt4 = tiff_with(tag, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
            let cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
            let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"CMT4", &cmt4)].concat());
            let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();
            assert!(crate::source_embeds_exif_gps_location(&file), "CMT4 destination tag {tag}");
        }
    }

    /// Every CMT4 packet must be inspected; the parsed convenience view keeps only the last one.
    #[test]
    fn source_gps_guard_checks_duplicate_cmt4_packets() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let gps_cmt4 = TiffWriter::default().write(&[gps]).unwrap();
        let empty_cmt4 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"CMT4", &gps_cmt4), bx(b"CMT4", &empty_cmt4)].concat());
        let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();

        let parsed = parse_cr3(&file).unwrap();
        assert_eq!(parsed.cmt[3], Some(empty_cmt4.as_slice()));
        let merged = merged_exif(&parsed).unwrap();
        assert!(crate::read_exif(&merged).gps.is_none(), "the convenience merge keeps only the later clean block");

        assert!(crate::source_embeds_exif_gps_location(&file));
    }

    /// GPS references in raw CMT1 cannot be lost when merged EXIF drops dangling source pointers.
    #[test]
    fn source_gps_guard_checks_cmt1_before_merging() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Canon".into()));
        ifd0.set_child(t::GPS_IFD, gps);
        let cmt1 = TiffWriter::default().write(&[ifd0]).unwrap();
        let canon = uuid(&CANON_UUID, &bx(b"CMT1", &cmt1));
        let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();

        let parsed = parse_cr3(&file).unwrap();
        let merged = merged_exif(&parsed).unwrap();
        assert_eq!(crate::exif::exif_gps_tag_presence(&cmt1), Some(true));
        assert_eq!(crate::exif::exif_gps_tag_presence(&merged), Some(false), "merging drops the dangling GPS pointer");
        assert!(crate::source_embeds_exif_gps_location(&file));
    }

    /// A later GPS-free CMT1 must not hide an earlier CMT1 with a GPS pointer.
    #[test]
    fn source_gps_guard_checks_duplicate_cmt1_packets() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut located = IfdBuilder::new();
        located.set_child(t::GPS_IFD, gps);
        let gps_cmt1 = TiffWriter::default().write(&[located]).unwrap();
        let clean_cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &gps_cmt1), bx(b"CMT1", &clean_cmt1)].concat());
        let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();

        let parsed = parse_cr3(&file).unwrap();
        assert_eq!(parsed.cmt[0], Some(clean_cmt1.as_slice()), "the convenience view keeps only the last CMT1");
        assert!(crate::source_embeds_exif_gps_location(&file));
    }

    /// CMT4 location tags in any IFD in its TIFF chain must block an Original copy.
    #[test]
    fn source_gps_guard_checks_every_cmt4_root_ifd() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let clean = IfdBuilder::new().with(t::MAKE, Value::Ascii("Canon".into()));
        let located = IfdBuilder::new().with(t::GPS_DEST_LATITUDE, Value::Rational(vec![(45, 1), (0, 1), (0, 1)]));
        let cmt4 = TiffWriter::default().write(&[clean, located]).unwrap();
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"CMT4", &cmt4)].concat());
        let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();

        assert!(crate::source_embeds_exif_gps_location(&file));
    }

    /// CMT2 blocks and every XMP UUID are retained by Original export and must be inspected.
    #[test]
    fn source_gps_guard_checks_cmt2_and_every_xmp_packet() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let clean_cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut cmt2_root = IfdBuilder::new();
        cmt2_root.set_child(t::GPS_IFD, gps);
        let cmt2 = TiffWriter::default().write(&[cmt2_root]).unwrap();
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &clean_cmt1), bx(b"CMT2", &cmt2)].concat());
        let cmt2_file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();
        assert!(crate::source_embeds_exif_gps_location(&cmt2_file), "CMT2 GPS pointers must be checked before merging");

        let located = crate::write_xmp(&crate::Metadata { city: Some("Cusco".into()), ..Default::default() }, None);
        let clean = crate::write_xmp(&crate::Metadata { title: Some("No location".into()), ..Default::default() }, None);
        let mut file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &bx(b"uuid", &CANON_UUID))].concat();
        file.extend(uuid(&XMP_UUID, located.as_bytes()));
        file.extend(uuid(&XMP_UUID, clean.as_bytes()));
        assert!(crate::source_embeds_exif_gps_location(&file), "a later clean packet cannot hide an earlier location packet");
    }

    fn jpeg_with_gps() -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set_child(t::GPS_IFD, gps);
        let exif = TiffWriter::default().write(&[ifd0]).unwrap();
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend(exif);
        let segment_length = u16::try_from(payload.len() + 2).unwrap();
        [vec![0xff, 0xd8, 0xff, 0xe1], segment_length.to_be_bytes().to_vec(), payload, vec![0xff, 0xd9]].concat()
    }

    /// CR3's thumbnail and JPEG tracks are part of the bytes retained by an Original copy.
    #[test]
    fn source_gps_guard_checks_thumbnail_and_jpeg_tracks() {
        let cmt1 = tiff_with(0x010f, lightcraft_tiff::Value::Ascii("Canon".into()));
        let jpeg = jpeg_with_gps();
        let thumbnail_uuid = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"THMB", &jpeg)].concat());
        let thumbnail_file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &thumbnail_uuid)].concat();
        assert!(crate::source_embeds_exif_gps_location(&thumbnail_file), "GPS in THMB must be found");

        let payload_at = 4096u64;
        let jpeg_track = trak(craw(16, 16, &bx(b"JPEG", &[0; 4])), payload_at, u32::try_from(jpeg.len()).unwrap());
        let moov = bx(b"moov", &jpeg_track);
        let mut track_file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), moov].concat();
        track_file.resize(usize::try_from(payload_at).unwrap().saturating_add(jpeg.len()), 0);
        let at = usize::try_from(payload_at).unwrap();
        track_file[at..at + jpeg.len()].copy_from_slice(&jpeg);
        assert!(crate::source_embeds_exif_gps_location(&track_file), "GPS in the JPEG track must be found");
    }

    /// THMB may contain a length-prefixed preview header before its JPEG bytes.
    #[test]
    fn source_gps_guard_parses_wrapped_thmb_before_scanning() {
        let cmt1 = tiff_with(0x010f, lightcraft_tiff::Value::Ascii("Canon".into()));
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let mut thmb = vec![0; 12];
        thmb.extend_from_slice(&u32::try_from(jpeg.len()).unwrap().to_be_bytes());
        thmb.extend_from_slice(&jpeg);
        let canon = uuid(&CANON_UUID, &[bx(b"CMT1", &cmt1), bx(b"THMB", &thmb)].concat());
        let file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &canon)].concat();

        assert!(!crate::source_embeds_exif_gps_location(&file), "a valid wrapped clean preview should not block Original export");
    }

    /// The location guard cannot infer absence from an invalid or uninspected JPEG track sample.
    #[test]
    fn source_gps_guard_rejects_unframed_or_additional_jpeg_samples() {
        let cmt1 = tiff_with(0x010f, lightcraft_tiff::Value::Ascii("Canon".into()));
        let canon = uuid(&CANON_UUID, &bx(b"CMT1", &cmt1));
        let offset = 4096u64;

        let invalid_jpeg_track = trak(craw(16, 16, &bx(b"JPEG", &[0; 4])), offset, 4);
        let mut unframed = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon.clone(), invalid_jpeg_track].concat())].concat();
        unframed.resize(usize::try_from(offset).unwrap().saturating_add(4), 0);
        assert!(crate::source_embeds_exif_gps_location(&unframed), "a JPEG track without SOI is unverified");

        let clean_jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let gps_jpeg = jpeg_with_gps();
        let sizes = [u32::try_from(clean_jpeg.len()).unwrap(), u32::try_from(gps_jpeg.len()).unwrap()];
        let raw_description = craw(16, 16, &bx(b"CMP1", &[0; 4]));
        let jpeg_description = craw(16, 16, &bx(b"JPEG", &[0; 4]));
        let multiple_descriptions = trak_with_entries_and_samples(&[raw_description.clone(), jpeg_description], offset, &[sizes[0]]);
        let mut multiple_descriptions =
            [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon.clone(), multiple_descriptions].concat())].concat();
        multiple_descriptions.resize(usize::try_from(offset).unwrap().saturating_add(clean_jpeg.len()), 0);
        multiple_descriptions[usize::try_from(offset).unwrap()..].copy_from_slice(&clean_jpeg);
        assert!(crate::source_embeds_exif_gps_location(&multiple_descriptions), "multiple descriptions are not all inspected");

        let jpeg_description = craw(16, 16, &bx(b"JPEG", &[0; 4]));
        let multiple_samples = trak_with_entries_and_samples(&[jpeg_description], offset, &sizes);
        let mut multiple_samples = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon, multiple_samples].concat())].concat();
        multiple_samples.resize(usize::try_from(offset).unwrap().saturating_add(clean_jpeg.len()).saturating_add(gps_jpeg.len()), 0);
        let second_start = usize::try_from(offset).unwrap().saturating_add(clean_jpeg.len());
        multiple_samples[usize::try_from(offset).unwrap()..second_start].copy_from_slice(&clean_jpeg);
        multiple_samples[second_start..].copy_from_slice(&gps_jpeg);
        assert!(crate::source_embeds_exif_gps_location(&multiple_samples), "a later JPEG sample must not be skipped");
    }

    /// GPS pointers in CTMD Exif records are retained even though static CMT blocks are clean.
    #[test]
    fn source_gps_guard_checks_ctmd_exif_records_and_fails_closed() {
        use lightcraft_tiff::{IfdBuilder, TiffWriter, Value, tags as t};

        let cmt1 = tiff_with(t::MAKE, Value::Ascii("Canon".into()));
        let mut gps = IfdBuilder::new();
        gps.set(t::GPS_LATITUDE, Value::Rational(vec![(45, 1), (30, 1), (0, 1)]));
        let mut exif = IfdBuilder::new();
        exif.set_child(t::GPS_IFD, gps);
        let exif = TiffWriter::default().write(&[exif]).unwrap();
        let mut item = u32::try_from(exif.len().saturating_add(8)).unwrap().to_le_bytes().to_vec();
        item.extend_from_slice(&0x8769u32.to_le_bytes());
        item.extend_from_slice(&exif);
        let mut sample = vec![0u8; 12];
        let record_len = u32::try_from(sample.len().saturating_add(item.len())).unwrap();
        sample[0..4].copy_from_slice(&record_len.to_le_bytes());
        sample[4..6].copy_from_slice(&8u16.to_le_bytes());
        sample.extend_from_slice(&item);
        let offset = 4096u64;
        let track = trak(bx(b"CTMD", &[]), offset, u32::try_from(sample.len()).unwrap());
        let canon = uuid(&CANON_UUID, &bx(b"CMT1", &cmt1));
        let mut file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon.clone(), track.clone()].concat())].concat();
        let start = usize::try_from(offset).unwrap();
        file.resize(start.saturating_add(sample.len()), 0);
        file[start..].copy_from_slice(&sample);
        assert!(crate::source_embeds_exif_gps_location(&file), "CTMD Exif GPS cannot be dropped from the Original");

        let mut malformed = sample;
        malformed[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon, track].concat())].concat();
        file.resize(start.saturating_add(malformed.len()), 0);
        file[start..].copy_from_slice(&malformed);
        assert!(crate::source_embeds_exif_gps_location(&file), "malformed CTMD records cannot authorize an Original copy");
    }

    #[test]
    fn source_gps_guard_skips_valid_non_exif_ctmd_records() {
        use lightcraft_tiff::{Value, tags as t};

        let record = |kind: u16| {
            let mut record = vec![0u8; 12];
            record[0..4].copy_from_slice(&12u32.to_le_bytes());
            record[4..6].copy_from_slice(&kind.to_le_bytes());
            record
        };
        let sample = [record(1), record(8)].concat();
        let offset = 4096u64;
        let track = trak(bx(b"CTMD", &[]), offset, u32::try_from(sample.len()).unwrap_or(u32::MAX));
        let canon = uuid(&CANON_UUID, &bx(b"CMT1", &tiff_with(t::MAKE, Value::Ascii("Canon".into()))));
        let mut file = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &[canon, track].concat())].concat();
        let start = usize::try_from(offset).unwrap_or(0);
        file.resize(start.saturating_add(sample.len()), 0);
        file[start..].copy_from_slice(&sample);

        assert!(!crate::source_embeds_exif_gps_location(&file), "valid non-EXIF CTMD records do not make location uncheckable");
    }

    fn compression_payload(levels: u8) -> Vec<u8> {
        let mut b = vec![0u8; 52];
        b[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
        b[2..4].copy_from_slice(&48u16.to_be_bytes());
        b[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        for (at, value) in [(8, 6888u32), (12, 4546), (16, 3444), (20, 4546), (28, 216)] {
            b[at..at + 4].copy_from_slice(&value.to_be_bytes());
        }
        b[24] = 14;
        b[25] = 0x40;
        b[26] = levels;
        b
    }

    #[test]
    fn compression_descriptor_retains_sensor_geometry_and_cfa() {
        let b = compression_payload(0);
        let h = Cr3Compression::parse(&b).unwrap();
        assert_eq!((h.width, h.height, h.tile_width, h.tile_height), (6888, 4546, 3444, 4546));
        assert_eq!((h.version, h.bit_depth, h.planes, h.cfa_pattern, h.encoding, h.levels, h.header_size), (0x100, 14, 4, 0, 0, 0, 216));
        let mut craw = compression_payload(3);
        craw[25] = 0x41;
        craw[27] = 0x80;
        let h = Cr3Compression::parse(&craw).unwrap();
        assert_eq!((h.cfa_pattern, h.levels, h.tile_flags), (1, 3, 0x80));
        for len in 0..b.len() {
            assert!(Cr3Compression::parse(&b[..len]).is_none());
        }
        for (at, value) in [(24, 0), (24, 17), (25, 0), (25, 0x44)] {
            let mut bad = b.clone();
            bad[at] = value;
            assert!(Cr3Compression::parse(&bad).is_none());
        }
    }

    #[test]
    fn extended_descriptor_checks_optional_median_precision() {
        let mut b = compression_payload(0x30);
        b[32] = 0x80;
        b.extend_from_slice(&33u32.to_be_bytes());
        b.resize(85, 0);
        b[56] = 0xc0;
        b[84] = 14;
        assert_eq!(Cr3Compression::parse(&b).unwrap().median_bit_depth, Some(14));
        assert!(Cr3Compression::parse(&b[..84]).is_none());
        b[84] = 0;
        assert!(Cr3Compression::parse(&b).is_none());
    }

    #[test]
    fn raw_track_reads_distinct_active_area_and_default_crop() {
        let mut area = vec![0u8; 48];
        for (at, value) in [
            (4, 6888u16),
            (6, 4546),
            (10, 2),
            (16, 156),
            (18, 158),
            (20, 6875),
            (22, 4537),
            (24, 0),
            (26, 0),
            (28, 143),
            (30, 4545),
            (32, 144),
            (34, 0),
            (36, 6887),
            (38, 45),
            (40, 144),
            (42, 46),
            (44, 6887),
            (46, 4545),
        ] {
            area[at..at + 2].copy_from_slice(&value.to_be_bytes());
        }
        let cmp1 = bx(b"CMP1", &compression_payload(0));
        let iad1 = full(b"CDI1", &bx(b"IAD1", &area));
        let track = trak(craw(6888, 4546, &[cmp1, iad1].concat()), 4096, 32);
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        file.extend(bx(b"moov", &track));
        file.resize(4128, 0);
        let c = parse_cr3(&file).unwrap();
        assert_eq!(c.tracks[0].compression(&file).unwrap().width, 6888);
        let geometry = c.tracks[0].image_area(&file).unwrap();
        assert_eq!(geometry.crop, [156, 158, 6875, 4537]);
        assert_eq!(geometry.active, Some([144, 46, 6887, 4545]));
        assert_eq!(geometry.masked_left, [0, 0, 143, 4545]);
        for len in 0..area.len() {
            assert!(Cr3ImageArea::parse(&area[..len]).is_none());
        }
    }

    #[test]
    fn truncated_sample_table_does_not_read_the_next_box() {
        let stbl = [
            full(b"stsd", &[1u32.to_be_bytes().to_vec(), craw(16, 16, &bx(b"CMP1", &compression_payload(0)))].concat()),
            bx(b"stsz", &[]),
            full(b"co64", &[1u32.to_be_bytes().to_vec(), 256u64.to_be_bytes().to_vec()].concat()),
        ]
        .concat();
        let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        file.extend(bx(b"moov", &bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))))));
        file.resize(512, 0);
        assert!(parse_cr3(&file).unwrap().tracks.is_empty());
    }

    #[test]
    fn timed_metadata_exposes_the_dynamic_maker_note() {
        use lightcraft_tiff::{Value, tags as t};
        let exif = tiff_with(t::ISO_SPEED, Value::Short(vec![800]));
        let maker = tiff_with(0x4001, Value::Short(vec![48, 100, 200]));
        let item = |tag: u32, data: &[u8]| {
            let mut b = ((data.len() + 8) as u32).to_le_bytes().to_vec();
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(data);
            b
        };
        let items = [item(0x8769, &exif), item(0x927c, &maker)].concat();
        let mut sample = vec![0u8; 12];
        sample[0..4].copy_from_slice(&((12 + items.len()) as u32).to_le_bytes());
        sample[4..6].copy_from_slice(&8u16.to_le_bytes());
        sample.extend(items);
        let container = Cr3 { tracks: vec![Cr3Track { kind: Cr3TrackKind::Other(*b"CTMD"), data: Some((0, sample.len())) }], ..Default::default() };
        let parts = container.timed_exif(&sample);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].record_type, parts[0].tag, parts[0].data), (8, 0x8769, exif.as_slice()));
        assert_eq!((parts[1].record_type, parts[1].tag, parts[1].data), (8, 0x927c, maker.as_slice()));
        for len in 0..sample.len() {
            assert!(container.timed_exif(&sample[..len]).is_empty());
        }
        for at in [0, 12] {
            let mut corrupt = sample.clone();
            corrupt[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(container.timed_exif(&corrupt).is_empty());
            corrupt[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
            assert!(container.timed_exif(&corrupt).is_empty());
        }
    }

    #[test]
    fn hostile_input_never_panics() {
        let f = sample();
        // every truncation and single-byte corruption parses to something or nothing
        for n in 0..f.len().min(800) {
            let _ = parse_cr3(&f[..n]).map(|c| merged_exif(&c));
        }
        for i in 0..f.len().min(800) {
            for v in [0u8, 1, 0x7f, 0xff] {
                let mut g = f.clone();
                g[i] = v;
                let _ = parse_cr3(&g).map(|c| merged_exif(&c));
            }
        }
        // a sample offset past the end is not reported
        let mut g = f.clone();
        g.truncate(4100);
        assert!(parse_cr3(&g).unwrap().tracks.iter().all(|t| t.data.is_none()));
    }
}
