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
    Raw { width: u16, height: u16, cmp1: Option<(usize, usize)> },
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

/// The parts of a CR3 file LightCraft reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cr3<'a> {
    /// `CMT1`…`CMT4` (index 0…3).
    pub cmt: [Option<&'a [u8]>; 4],
    pub thumbnail: Option<&'a [u8]>,
    pub xmp: Option<&'a [u8]>,
    pub tracks: Vec<Cr3Track>,
}

const CANON_UUID: [u8; 16] = [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
const XMP_UUID: [u8; 16] = [0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac];
const MAX_DEPTH: usize = 12;
const MAX_BOXES: usize = 4096;
/// A `VisualSampleEntry` (78 bytes after the box header) plus 4 bytes Canon adds before the child boxes.
const CRAW_CHILDREN_AT: usize = 82;

/// Does this look like a CR3 file (`ftyp` box with major brand `crx `)?
pub fn is_cr3(bytes: &[u8]) -> bool {
    bytes.get(4..12) == Some(b"ftypcrx ".as_slice())
}

/// Parse a CR3 file's boxes; `None` when it is not a CR3 file.
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

struct BoxRef {
    kind: [u8; 4],
    /// Start of the payload (after the header and, for `uuid`, the 16-byte uuid).
    body: usize,
    end: usize,
    uuid: Option<[u8; 16]>,
}

/// The boxes directly inside `[from, to)`; stops at the first malformed header.
fn boxes(bytes: &[u8], from: usize, to: usize, budget: &mut usize) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let mut at = from;
    let to = to.min(bytes.len());
    while at.saturating_add(8) <= to && *budget > 0 {
        *budget -= 1;
        let (Some(size), Some(kind)) = (be32(bytes, at), bytes.get(at + 4..at + 8)) else { break };
        let mut kind4 = [0u8; 4];
        kind4.copy_from_slice(kind);
        let (len, mut hdr) = match size {
            0 => (to - at, 8),
            1 => match be64(bytes, at + 8).and_then(|v| usize::try_from(v).ok()) {
                Some(l) => (l, 16),
                None => break,
            },
            n => (n as usize, 8),
        };
        let Some(end) = at.checked_add(len).filter(|&e| e <= to && len >= hdr) else { break };
        let mut uuid = None;
        if &kind4 == b"uuid" {
            let Some(u) = bytes.get(at + hdr..at + hdr + 16).filter(|_| at + hdr + 16 <= end) else { break };
            let mut a = [0u8; 16];
            a.copy_from_slice(u);
            uuid = Some(a);
            hdr += 16;
        }
        out.push(BoxRef { kind: kind4, body: at + hdr, end, uuid });
        at = end;
    }
    out
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
        match &b.kind {
            // full box: version/flags, entry count, entries
            b"stsd" => kind = boxes(bytes, b.body + 8, b.end, budget).first().map(|e| sample_entry(bytes, e, budget)),
            // version/flags, sample_size (0 = per-sample table), count, sizes
            b"stsz" => {
                size = match be32(bytes, b.body + 4)? {
                    0 if be32(bytes, b.body + 8)? > 0 => be32(bytes, b.body + 12),
                    0 => None,
                    n => Some(n),
                }
            }
            b"co64" if be32(bytes, b.body + 4)? > 0 => offset = be64(bytes, b.body + 8).and_then(|v| usize::try_from(v).ok()),
            b"stco" if be32(bytes, b.body + 4)? > 0 => offset = be32(bytes, b.body + 8).map(|v| v as usize),
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
    let (Some(width), Some(height)) = (be16(bytes, e.body + 24), be16(bytes, e.body + 26)) else {
        return Cr3TrackKind::Other(e.kind);
    };
    let mut cmp1 = None;
    for c in boxes(bytes, e.body.saturating_add(CRAW_CHILDREN_AT), e.end, budget) {
        match &c.kind {
            b"JPEG" => return Cr3TrackKind::Jpeg,
            b"CMP1" => cmp1 = Some((c.body, c.end - c.body)),
            _ => {}
        }
    }
    Cr3TrackKind::Raw { width, height, cmp1 }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_be_bytes(a))
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
        let mut stsd = 1u32.to_be_bytes().to_vec();
        stsd.extend(entry);
        let mut stsz = 0u32.to_be_bytes().to_vec();
        stsz.extend(1u32.to_be_bytes());
        stsz.extend(size.to_be_bytes());
        let mut co64 = 1u32.to_be_bytes().to_vec();
        co64.extend(offset.to_be_bytes());
        let stbl = [full(b"stsd", &stsd), full(b"stsz", &stsz), full(b"co64", &co64)].concat();
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
        let Cr3TrackKind::Raw { width, height, cmp1: Some((at, len)) } = c.tracks[1].kind else { panic!("{:?}", c.tracks[1]) };
        assert_eq!((width, height, len), (60, 40, 4));
        assert_eq!(&f[at..at + 2], &[0xff, 0x10]);
        assert_eq!(c.tracks[1].data, Some((4112, 32)));
    }

    #[test]
    fn merged_exif_reads_like_a_tiff_raw() {
        let f = sample();
        let m = crate::read_exif(&merged_exif(&parse_cr3(&f).unwrap()).unwrap());
        assert_eq!(m.model.as_deref(), Some("Canon EOS Test"));
        assert_eq!(m.iso, Some(400));
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
