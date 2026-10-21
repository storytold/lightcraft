//! Describing a raw file without reading its pixel data.
//!
//! [`probe_info`](crate::probe_info) and `lightcraft_meta::extract` read only a raw file's
//! headers, but those are a few hundred kilobytes of a file of tens of megabytes: an import from a
//! slow drive spends nearly all its time reading samples nothing looks at. [`header_plan`] says
//! which byte ranges of a file those readers need. It is asked about a buffer of the file's full
//! length in which only the ranges in `loaded` hold the file's bytes (the rest is zeros); the caller
//! loads the ranges it names and asks again. Once it says [`HeaderPlan::Ready`], probing the buffer
//! gives what probing the whole file gives.
//!
//! Only bulk sample data is left out: the strips and tiles of each large TIFF image (keeping the
//! start of the first one, where the coding headers a header probe validates sit), a CR3's image
//! tracks (keeping the raw track's coding headers and quantization maps, and the timed-metadata
//! track where Canon puts the as-shot white balance), a RAF's CFA strip (keeping its compression
//! header) and a Panasonic RW2's raw data. Formats whose header probe reads samples (NEF: masked
//! columns; ORF: bit depth and CFA phase; PEF: dark borders; ARW6, for its header plan only: the
//! start of every tile) and anything else get [`HeaderPlan::Whole`], as does a structure that is
//! incomplete, inconsistent or keeps metadata inside its sample data: the plan never guesses.

use std::collections::HashSet;

use lightcraft_meta::cr3::{Cr3TrackKind, XMP_UUID, is_cr3, parse_cr3};
use lightcraft_tiff::{ByteOrder, FieldType, Tiff, tags as t};

use crate::RawFormat;

/// Bytes kept at the start of a skipped block: the coding headers a header probe validates there
/// (lossless-JPEG frame headers, a RAF's compression header and stripe table) fit with room to spare.
const HEAD: u64 = 64 * 1024;
/// An image whose chunks hold fewer bytes than this is read (thumbnails, small previews).
const BULK: u64 = 256 * 1024;
/// At most this many bytes between two of an image's chunks are alignment padding.
const PAD: u64 = 16;
/// [`reads`]' defaults for a drive where each read costs a round trip: on the raw.pixls.us corpus
/// they take a RAF from 7 reads in 5 rounds to 4 in 2, and a CR3 from 4.1 to 3.5 reads, for
/// 0.2–0.3 % more of the file.
pub const READ_MIN: u64 = 64 * 1024;
pub const READ_GAP: u64 = 16 * 1024;
/// The same bounds as `lightcraft_tiff`'s reader.
const MAX_IFDS: usize = 1024;
const MAX_ENTRIES: u64 = 8192;
const MAX_BOXES: usize = 4096;
/// Child IFDs listed by one `SubIFDs` entry.
const MAX_CHILDREN: u64 = 4096;
/// The RAF raw container's pointer to its raw IFD.
const RAF_RAW_IFD: u16 = 0xf000;

/// Byte ranges `[start, end)` of a file: sorted, disjoint and not touching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `[start, end)` (nothing when it is empty).
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let mut placed = false;
        for &(a, b) in &self.0 {
            if b < start {
                out.push((a, b));
            } else if a > end {
                if !placed {
                    out.push((start, end));
                    placed = true;
                }
                out.push((a, b));
            } else {
                start = start.min(a);
                end = end.max(b);
            }
        }
        if !placed {
            out.push((start, end));
        }
        self.0 = out;
    }

    /// Add every `[start, end)` of `spans` in one sort and merge (empty ones add nothing).
    pub fn extend(&mut self, spans: impl IntoIterator<Item = (u64, u64)>) {
        let mut all: Vec<(u64, u64)> = std::mem::take(&mut self.0);
        all.extend(spans.into_iter().filter(|&(a, b)| a < b));
        all.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(all.len());
        for (a, b) in all {
            match out.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        self.0 = out;
    }

    /// Whether all of `[start, end)` is covered (an empty range always is).
    pub fn contains(&self, start: u64, end: u64) -> bool {
        start >= end || self.0.iter().any(|&(a, b)| a <= start && end <= b)
    }

    /// Whether some of `[start, end)` is covered.
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        start < end && self.0.iter().any(|&(a, b)| a < end && start < b)
    }

    pub fn spans(&self) -> &[(u64, u64)] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Bytes covered.
    pub fn total(&self) -> u64 {
        self.0.iter().fold(0u64, |n, &(a, b)| n.saturating_add(b - a))
    }

    /// The parts of `[start, end)` not covered.
    pub fn gaps(&self, start: u64, end: u64) -> Ranges {
        let mut out = Ranges::new();
        let mut at = start;
        for &(a, b) in &self.0 {
            if b <= at {
                continue;
            }
            if a >= end {
                break;
            }
            if a > at {
                out.insert(at, a);
            }
            at = at.max(b);
        }
        if at < end {
            out.insert(at, end);
        }
        out
    }

    /// These ranges without the ones in `other`.
    pub fn minus(&self, other: &Ranges) -> Ranges {
        let mut out = Ranges::new();
        for &(a, b) in &self.0 {
            for &(x, y) in other.gaps(a, b).spans() {
                out.insert(x, y);
            }
        }
        out
    }

    /// These ranges with the gaps of at most `gap` bytes between them filled in.
    pub fn bridged(&self, gap: u64) -> Ranges {
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(self.0.len());
        for &(a, b) in &self.0 {
            match out.last_mut() {
                Some(last) if a.saturating_sub(last.1) <= gap => last.1 = b,
                _ => out.push((a, b)),
            }
        }
        Ranges(out)
    }
}

/// The reads that load what a plan says it `need`s on a file of `len` bytes with `loaded` read,
/// for a drive where every read costs a round trip (a network share, a hard disk): each range is
/// read at least `min` bytes long, since the plan's next round usually asks for what follows a
/// header; ranges at most `gap` apart are read as one; and the bytes already loaded are left out
/// (unless they sit in such a gap). Everything in `need` is covered.
pub fn reads(need: &Ranges, loaded: &Ranges, len: u64, min: u64, gap: u64) -> Ranges {
    let mut wide = Ranges::new();
    for &(a, b) in need.spans() {
        wide.insert(a, b.max(a.saturating_add(min).min(len)));
    }
    wide.minus(loaded).bridged(gap)
}

/// What [`header_plan`] says about a partly loaded file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderPlan {
    /// Load these ranges (none of them loaded yet) and ask again.
    Need(Ranges),
    /// Everything the header readers look at is loaded: probing the buffer gives the whole file's
    /// result.
    Ready,
    /// The file can't be described from part of it (why): read it whole.
    Whole(&'static str),
}

/// What of the raw file in `bytes` a header probe reads. `bytes` has the file's full length;
/// only the ranges in `loaded` hold its bytes (see the module docs).
pub fn header_plan(bytes: &[u8], loaded: &Ranges) -> HeaderPlan {
    plan(bytes, loaded, false)
}

/// [`header_plan`] for a raw shown from its embedded preview (a coding not decoded yet): also what
/// [`crate::embedded_preview`] reads — a CR3's JPEG track and preview boxes, the strips of a TIFF
/// raw's preview images.
pub fn preview_plan(bytes: &[u8], loaded: &Ranges) -> HeaderPlan {
    plan(bytes, loaded, true)
}

fn plan(bytes: &[u8], loaded: &Ranges, preview: bool) -> HeaderPlan {
    let mut w = Want::new(bytes, loaded);
    if !w.want(0, 16) {
        return w.into_plan().unwrap_or(HeaderPlan::Whole("file too short"));
    }
    if is_cr3(bytes) {
        cr3_plan(bytes, loaded, preview)
    } else if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        // (its preview, the embedded JPEG, is read anyway: its metadata are there)
        raf_plan(bytes, loaded)
    } else if bytes.get(..4).is_some_and(|m| matches!(m, b"IIRO" | b"IIRS" | b"MMOR")) {
        HeaderPlan::Whole("an ORF header probe reads samples")
    } else if Tiff::sniff(bytes).is_some() {
        tiff_plan(bytes, loaded, preview)
    } else {
        HeaderPlan::Whole("not a raw layout read in part")
    }
}

/// Collects the ranges a plan still needs.
struct Want<'a> {
    len: u64,
    loaded: &'a Ranges,
    need: Ranges,
}

impl<'a> Want<'a> {
    fn new(bytes: &[u8], loaded: &'a Ranges) -> Self {
        Self { len: bytes.len() as u64, loaded, need: Ranges::new() }
    }

    /// Whether `[start, end)` (clipped to the file) is loaded; asks for what isn't.
    fn want(&mut self, start: u64, end: u64) -> bool {
        let end = end.min(self.len);
        if self.loaded.contains(start, end) {
            return true;
        }
        for &(a, b) in self.loaded.gaps(start, end).spans() {
            self.need.insert(a, b);
        }
        false
    }

    /// `Need` when something is missing.
    fn into_plan(self) -> Option<HeaderPlan> {
        (!self.need.is_empty()).then_some(HeaderPlan::Need(self.need))
    }
}

/// Everything but `holes` is needed. `meta` (IFD tables and out-of-line values) must not lie in a
/// hole; once all is loaded, `ready` decides.
fn finish(len: u64, loaded: &Ranges, holes: &Ranges, meta: &Ranges, ready: impl FnOnce() -> HeaderPlan) -> HeaderPlan {
    if holes.spans().iter().any(|&(a, b)| meta.overlaps(a, b)) {
        return HeaderPlan::Whole("metadata inside sample data");
    }
    let missing = holes.gaps(0, len).minus(loaded);
    if missing.is_empty() { ready() } else { HeaderPlan::Need(missing) }
}

fn tiff_plan(bytes: &[u8], loaded: &Ranges, preview: bool) -> HeaderPlan {
    let len = bytes.len() as u64;
    let mut w = Want::new(bytes, loaded);
    let meta = match walk_tiff(&mut w, 0, bytes, &[]) {
        Ok(meta) => meta,
        Err(why) => return HeaderPlan::Whole(why),
    };
    if let Some(plan) = w.into_plan() {
        return plan;
    }
    // (the Make is usually in the first kilobytes: a NEF, PEF or ORF is known before the rest is read)
    if matches!(crate::probe(bytes), Some(RawFormat::Nef | RawFormat::Nrw | RawFormat::Orf | RawFormat::Pef | RawFormat::Erf | RawFormat::Mef)) {
        return HeaderPlan::Whole("its header probe reads samples");
    }
    let Ok(tiff) = Tiff::parse(bytes) else { return HeaderPlan::Whole("TIFF structure") };
    let mut holes = Ranges::new();
    let mut spans = Vec::new();
    for ifd in tiff.all_ifds() {
        let chunks: Vec<(u64, u64)> = match ifd.image() {
            // a preview's strips are read when the preview is wanted: only the raw image is skipped
            Ok(info) if preview && !matches!(info.photometric, t::photometric::CFA | t::photometric::LINEAR_RAW) => continue,
            // (Sony Compressed RAW 2: the header probe decodes the start of every tile; a preview reads none)
            Ok(info) if !preview && info.compression == 32766 => return HeaderPlan::Whole("an ARW6 header probe reads its tiles"),
            Ok(info) => info.chunks(len).iter().map(|c| (c.offset, c.len)).collect(),
            // a raw IFD without the tags a TIFF image needs (an older CR2's) still lists its strips
            Err(_) => match (ifd.u64s(t::STRIP_OFFSETS), ifd.u64s(t::STRIP_BYTE_COUNTS)) {
                (Some(offsets), Some(counts)) if offsets.len() == counts.len() => offsets.into_iter().zip(counts).collect(),
                _ => continue,
            },
        };
        let total = chunks.iter().fold(0u64, |n, c| n.saturating_add(c.1));
        if total < BULK {
            continue;
        }
        // (offset, where the skipped part starts, end) of each chunk
        let mut skipped: Vec<(u64, u64, u64)> = chunks
            .iter()
            .enumerate()
            .map(|(i, &(offset, size))| (offset, if i == 0 { offset.saturating_add(HEAD) } else { offset }, offset.saturating_add(size).min(len)))
            .collect();
        spans.extend(skipped.iter().map(|&(_, start, end)| (start, end)));
        // the padding between two skipped chunks (a writer aligns each chunk to a word) is skipped
        // too: a tiled DNG would otherwise be hundreds of one-byte reads
        skipped.sort_unstable();
        for pair in skipped.windows(2) {
            let &[(_, start, end), (next, next_start, next_end)] = pair else { continue };
            if start < end && next_start == next && next < next_end && next > end && next - end <= PAD {
                spans.push((end, next));
            }
        }
    }
    holes.extend(spans);
    // A Panasonic RW2 keeps its raw data from `RawDataOffset` to the end, outside any TIFF image.
    if holes.is_empty()
        && bytes.starts_with(b"IIU\0")
        && let Some(off) = tiff.ifds.first().and_then(|i| i.u64(crate::vendor::rw2::RAW_OFFSET))
    {
        holes.insert(off.saturating_add(HEAD), len);
    }
    finish(len, loaded, &holes, &meta, || match crate::probe(bytes) {
        Some(RawFormat::Nef | RawFormat::Nrw | RawFormat::Orf | RawFormat::Pef | RawFormat::Erf | RawFormat::Mef) => {
            HeaderPlan::Whole("its header probe reads samples")
        }
        Some(f) if f.is_supported() => HeaderPlan::Ready,
        _ => HeaderPlan::Whole("not a supported raw"),
    })
}

/// Walk the IFDs of the TIFF in `data` (which starts at `origin` in the file) as `lightcraft_tiff`'s
/// reader does — the chain, `SubIFDs`, Exif, GPS and Interop, plus the `extra` pointer tags —
/// asking for every IFD table and for the values that locate IFDs and image chunks. Returns the IFD
/// tables and out-of-line values found (absolute ranges); incomplete while `w` needs more.
fn walk_tiff(w: &mut Want, origin: u64, data: &[u8], extra: &[u16]) -> Result<Ranges, &'static str> {
    let len = data.len() as u64;
    let at = |rel: u64| origin.saturating_add(rel);
    let mut meta = Ranges::new();
    if !w.want(origin, at(len.min(16))) {
        return Ok(meta);
    }
    let (order, big) = Tiff::sniff(data).ok_or("not a TIFF")?;
    let first = if big { order.read_u64(data, 8) } else { order.read_u32(data, 4).map(u64::from) }.ok_or("truncated TIFF header")?;
    meta.insert(origin, at(len.min(if big { 16 } else { 8 })));
    let (head, entry, inline) = if big { (8u64, 20u64, 8u64) } else { (2, 12, 4) };
    let offset_at = |pos: u64| -> Option<u64> {
        let pos = usize::try_from(pos).ok()?;
        if big { order.read_u64(data, pos) } else { order.read_u32(data, pos).map(u64::from) }
    };
    let mut stack = vec![first];
    let mut seen = HashSet::new();
    while let Some(off) = stack.pop() {
        if off == 0 || off >= len || seen.len() >= MAX_IFDS || !seen.insert(off) {
            continue;
        }
        if !w.want(at(off), at(off.saturating_add(head))) {
            continue;
        }
        let Ok(pos) = usize::try_from(off) else { continue };
        let n = if big { order.read_u64(data, pos) } else { order.read_u16(data, pos).map(u64::from) }.unwrap_or(0);
        let table = off.saturating_add(head);
        let n_read = n.min(len.saturating_sub(table) / entry).min(MAX_ENTRIES);
        if n_read == 0 {
            continue;
        }
        let table_end = table.saturating_add(n_read * entry);
        // the reader reads the next-IFD pointer only when every entry fits
        let complete = n_read == n;
        let end = if complete { table_end.saturating_add(inline).min(len) } else { table_end };
        if !w.want(at(off), at(end)) {
            continue;
        }
        meta.insert(at(off), at(end));
        for i in 0..n_read {
            let p = table + i * entry;
            let Ok(pu) = usize::try_from(p) else { continue };
            let (Some(tag), Some(typ)) = (order.read_u16(data, pu), order.read_u16(data, pu + 2).and_then(FieldType::from_u16)) else { continue };
            let count = if big { order.read_u64(data, pu + 4) } else { order.read_u32(data, pu + 4).map(u64::from) };
            let Some(count) = count else { continue };
            let field = p + if big { 12 } else { 8 };
            let Some(total) = count.checked_mul(typ.size() as u64) else { continue };
            let start = if total <= inline {
                field
            } else {
                match offset_at(field) {
                    Some(v) => v,
                    None => continue,
                }
            };
            let Some(value_end) = start.checked_add(total).filter(|&e| e <= len) else { continue };
            if total > inline {
                meta.insert(at(start), at(value_end));
            }
            let child = matches!(tag, t::SUB_IFDS | t::EXIF_IFD | t::GPS_IFD | t::INTEROP_IFD) || extra.contains(&tag);
            let locates = child || matches!(tag, t::STRIP_OFFSETS | t::STRIP_BYTE_COUNTS | t::TILE_OFFSETS | t::TILE_BYTE_COUNTS);
            if !locates || !w.want(at(start), at(value_end)) || !child {
                continue;
            }
            // the reader follows every `SubIFDs` value and the first value of the other pointers
            let take = if tag == t::SUB_IFDS { count.min(MAX_CHILDREN) } else { count.min(1) };
            for k in 0..take {
                if let Some(v) = unsigned(data, order, typ, start + k * typ.size() as u64) {
                    stack.push(v);
                }
            }
        }
        if complete && let Some(next) = offset_at(table_end) {
            stack.push(next);
        }
    }
    Ok(meta)
}

/// One unsigned value of type `typ` at `pos`.
fn unsigned(data: &[u8], order: ByteOrder, typ: FieldType, pos: u64) -> Option<u64> {
    let pos = usize::try_from(pos).ok()?;
    match typ.size() {
        1 => data.get(pos).map(|&b| u64::from(b)),
        2 => order.read_u16(data, pos).map(u64::from),
        4 => order.read_u32(data, pos).map(u64::from),
        _ => order.read_u64(data, pos),
    }
}

fn be32(bytes: &[u8], at: u64) -> Option<u64> {
    let at = usize::try_from(at).ok()?;
    bytes.get(at..at.checked_add(4)?).map(|s| u64::from(u32::from_be_bytes([s[0], s[1], s[2], s[3]])))
}

fn be64(bytes: &[u8], at: u64) -> Option<u64> {
    let at = usize::try_from(at).ok()?;
    bytes.get(at..at.checked_add(8)?).map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// A CR3 is read where `lightcraft_meta::cr3::parse_cr3` and the CR3 decoder look: the file type,
/// `moov` and the XMP `uuid` box, the first sample of each timed-metadata (`CTMD`) track, and the
/// main raw track's coding headers and quantization maps. Previews and image tracks are skipped,
/// unless `preview`: then every top-level box but `mdat` and the JPEG tracks are read too.
fn cr3_plan(bytes: &[u8], loaded: &Ranges, preview: bool) -> HeaderPlan {
    let len = bytes.len() as u64;
    let mut w = Want::new(bytes, loaded);
    // top-level boxes, walked as `parse_cr3` walks them (it stops at the first malformed header)
    let mut at = 0u64;
    let mut boxes = 0;
    while at.saturating_add(8) <= len && boxes < MAX_BOXES {
        boxes += 1;
        // size, type, a 64-bit size and a uuid
        if !w.want(at, at.saturating_add(32)) {
            break;
        }
        let Some(size) = be32(bytes, at) else { break };
        let (box_len, hdr) = match size {
            0 => (len - at, 8),
            1 => match be64(bytes, at + 8) {
                Some(l) => (l, 16),
                None => break,
            },
            n => (n, 8),
        };
        let Some(end) = at.checked_add(box_len).filter(|&e| e <= len && box_len >= hdr) else { break };
        let kind = bytes.get(at as usize + 4..at as usize + 8);
        let needed = match kind {
            Some(b"mdat") => false,
            Some(b"moov") => true,
            Some(b"uuid") => preview || bytes.get((at + hdr) as usize..(at + hdr + 16) as usize) == Some(XMP_UUID.as_slice()),
            _ => preview,
        };
        if needed {
            w.want(at, end);
        }
        at = end;
    }
    if let Some(plan) = w.into_plan() {
        return plan;
    }
    let mut w = Want::new(bytes, loaded);
    let Some(cr3) = parse_cr3(bytes) else { return HeaderPlan::Whole("CR3 structure") };
    for track in &cr3.tracks {
        if (track.kind == Cr3TrackKind::Other(*b"CTMD") || preview && track.kind == Cr3TrackKind::Jpeg)
            && let Some((offset, size)) = track.data
        {
            w.want(offset as u64, offset.saturating_add(size) as u64);
        }
    }
    if let Some(track) = crate::vendor::cr3::main_track(&cr3.tracks)
        && let Some((offset, size)) = track.data
        && let Some(coding) = track.compression(bytes)
    {
        let o = offset as u64;
        if w.want(o, o.saturating_add(u64::from(coding.header_size)))
            && let Some(sample) = bytes.get(offset..offset.saturating_add(size))
            && let Ok(ranges) = crate::vendor::crx::header_ranges(sample, &coding)
        {
            for r in ranges {
                w.want(o.saturating_add(r.start as u64), o.saturating_add(r.end as u64));
            }
        }
    }
    w.into_plan().unwrap_or(HeaderPlan::Ready)
}

/// A RAF is read whole but for its CFA strip: the header, the embedded JPEG (its metadata), the
/// record directory and the raw container's IFDs and compression header.
fn raf_plan(bytes: &[u8], loaded: &Ranges) -> HeaderPlan {
    let len = bytes.len() as u64;
    let mut w = Want::new(bytes, loaded);
    if !w.want(0, 160) {
        return w.into_plan().unwrap_or(HeaderPlan::Whole("RAF header"));
    }
    let (Some(start), Some(size)) = (be32(bytes, 100), be32(bytes, 104)) else { return HeaderPlan::Whole("RAF header") };
    let Some(end) = start.checked_add(size).filter(|&e| start > 0 && size > 0 && e <= len) else { return HeaderPlan::Whole("RAF raw container") };
    let Some(container) = bytes.get(start as usize..end as usize) else { return HeaderPlan::Whole("RAF raw container") };
    let meta = match walk_tiff(&mut w, start, container, &[RAF_RAW_IFD]) {
        Ok(meta) => meta,
        Err(why) => return HeaderPlan::Whole(why),
    };
    if let Some(plan) = w.into_plan() {
        return plan;
    }
    let Ok(ifd) = crate::vendor::raf::raw_ifd(container) else { return HeaderPlan::Whole("RAF raw IFD") };
    let (Some(off), Some(strip)) = (ifd.u64(0xf007), ifd.u64(0xf008)) else { return HeaderPlan::Whole("RAF raw strip") };
    let mut holes = Ranges::new();
    if strip >= BULK {
        let s = start.saturating_add(off);
        holes.insert(s.saturating_add(HEAD), s.saturating_add(strip).min(end));
    }
    finish(len, loaded, &holes, &meta, || HeaderPlan::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a plan to its end, loading from `file` what it asks for; the buffer's unloaded bytes are `fill`.
    pub(crate) fn run(file: &[u8], prefix: usize, fill: u8) -> (HeaderPlan, Vec<u8>, Ranges) {
        let mut buf = vec![fill; file.len()];
        let mut loaded = Ranges::new();
        let load = |buf: &mut Vec<u8>, loaded: &mut Ranges, a: u64, b: u64| {
            buf[a as usize..b as usize].copy_from_slice(&file[a as usize..b as usize]);
            loaded.insert(a, b);
        };
        load(&mut buf, &mut loaded, 0, prefix.min(file.len()) as u64);
        for _ in 0..32 {
            match header_plan(&buf, &loaded) {
                HeaderPlan::Need(r) => {
                    for &(a, b) in r.spans() {
                        assert!(!loaded.overlaps(a, b), "asked again for {a}..{b}");
                        load(&mut buf, &mut loaded, a, b);
                    }
                }
                done => return (done, buf, loaded),
            }
        }
        panic!("no answer after 32 rounds");
    }

    #[test]
    fn ranges_merge_and_subtract() {
        let mut r = Ranges::new();
        r.insert(10, 20);
        r.insert(30, 40);
        r.insert(20, 25); // touching: merged
        r.insert(5, 5); // empty
        assert_eq!(r.spans(), &[(10, 25), (30, 40)]);
        r.insert(0, 100);
        assert_eq!(r.spans(), &[(0, 100)]);
        let mut r = Ranges::new();
        r.insert(10, 20);
        r.insert(30, 40);
        assert!(r.contains(12, 18) && r.contains(5, 5) && !r.contains(15, 35));
        assert!(r.overlaps(15, 35) && !r.overlaps(20, 30));
        assert_eq!(r.gaps(0, 50).spans(), &[(0, 10), (20, 30), (40, 50)]);
        assert_eq!(r.gaps(12, 18).spans(), &[] as &[(u64, u64)]);
        let mut cut = Ranges::new();
        cut.insert(15, 32);
        assert_eq!(r.minus(&cut).spans(), &[(10, 15), (32, 40)]);
        assert_eq!(r.total(), 20);
    }

    /// Reads cover what is needed, at least `min` long (not past the file's end), close ones as one,
    /// without the loaded bytes unless they sit in a bridged gap.
    #[test]
    fn reads_cover_the_need_in_fewer_larger_reads() {
        let mut need = Ranges::new();
        need.insert(100, 110);
        need.insert(150, 160);
        need.insert(1000, 1010);
        need.insert(1040, 1045);
        need.insert(1990, 1995);
        let mut loaded = Ranges::new();
        loaded.insert(0, 100);
        loaded.insert(1020, 1030);
        // no widening, no bridging: the need itself
        assert_eq!(reads(&need, &loaded, 2000, 0, 0), need);
        // close ranges as one (over loaded bytes too); the far one stays apart
        assert_eq!(reads(&need, &loaded, 2000, 0, 50).spans(), &[(100, 160), (1000, 1045), (1990, 1995)]);
        // at least 100 bytes, clipped to the file; the loaded 1020..1030 left out
        assert_eq!(reads(&need, &loaded, 2000, 100, 0).spans(), &[(100, 250), (1000, 1020), (1030, 1140), (1990, 2000)]);
        // ... unless it sits in a bridged gap
        assert_eq!(reads(&need, &loaded, 2000, 100, 64).spans(), &[(100, 250), (1000, 1140), (1990, 2000)]);
        for (min, gap) in [(0, 0), (100, 0), (0, 50), (100, 64), (u64::MAX, u64::MAX)] {
            let r = reads(&need, &loaded, 2000, min, gap);
            assert!(need.spans().iter().all(|&(a, b)| r.contains(a, b)), "{min} {gap}");
            assert!(r.spans().iter().all(|&(_, b)| b <= 2000), "{min} {gap}");
        }
    }

    #[test]
    fn hostile_input_never_panics_and_ends() {
        let mut loaded = Ranges::new();
        for len in [0usize, 1, 8, 15, 16, 200] {
            let bytes = vec![0u8; len];
            loaded.insert(0, len as u64);
            assert!(matches!(header_plan(&bytes, &loaded), HeaderPlan::Whole(_)));
        }
        // an IFD that points at itself, an IFD count past the end, a huge SubIFDs count
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&[0x4a, 0x01, 4, 0, 0xff, 0xff, 0xff, 0x0f, 8, 0, 0, 0]); // SubIFDs, 0x0fffffff values at 8
        tiff.extend_from_slice(&[0x69, 0x87, 4, 0, 1, 0, 0, 0, 8, 0, 0, 0]); // Exif → itself
        tiff.extend_from_slice(&8u32.to_le_bytes()); // next → itself
        let (plan, _, _) = run(&tiff, 4, 0);
        assert!(matches!(plan, HeaderPlan::Whole(_)), "{plan:?}");
        // a CR3 whose `moov` claims more than the file holds: walked as the parser walks it (it
        // stops there), so the plan ends with the little there is, and the probe then fails as
        // it does on the whole file
        let mut cr3 = 24u32.to_be_bytes().to_vec();
        cr3.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
        cr3.extend_from_slice(&u32::MAX.to_be_bytes());
        cr3.extend_from_slice(b"moov");
        cr3.resize(4096, 0);
        let (plan, buf, loaded) = run(&cr3, 16, 0);
        assert_eq!(plan, HeaderPlan::Ready);
        assert_eq!(loaded.total(), 24 + 32, "only the two box headers are read");
        assert_eq!(format!("{:?}", crate::probe_info(&buf)), format!("{:?}", crate::probe_info(&cr3)));
        // a RAF whose container lies outside the file
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(160, 0);
        raf[100..104].copy_from_slice(&1000u32.to_be_bytes());
        raf[104..108].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(run(&raf, 160, 0).0, HeaderPlan::Whole("RAF raw container"));
    }

    /// A writer that word-aligns its tiles leaves a pad byte after each odd-length one. The pad is
    /// skipped with the tiles, not asked for one byte at a time: Adobe-converted DNGs on raw.pixls.us
    /// made the plan ask for up to 240 one-byte ranges, each a read of its own (issue #832, SMB).
    #[test]
    fn padding_between_tiles_is_skipped_with_them() {
        use crate::dngwrite::{DngCompression, DngWriteOptions, write_dng};
        use crate::{BlackLevel, Cfa, ColorData, Metadata, OpcodeLists, Orientation, RawData, RawImage, Rect};
        let (w, h) = (512, 512);
        let mut seed = 1u32;
        let data = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                528 + (seed >> 20) as u16 % 3500
            })
            .collect();
        let raw = RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16(data),
            cfa: Cfa::bayer("RGGB"),
            bits: 12,
            black: BlackLevel::uniform(528.0),
            white: vec![4095.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(0, 0, w, h),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Metadata::default(),
            lens_metadata: Default::default(),
        };
        let file = write_dng(&raw, &DngWriteOptions { compression: DngCompression::Lj92 { tile: 64 }, ..Default::default() }).unwrap();
        let tiff = Tiff::parse(&file).unwrap();
        let counts = tiff.all_ifds().into_iter().find_map(|ifd| ifd.u64s(t::TILE_BYTE_COUNTS)).unwrap();
        assert!(counts.iter().filter(|&&n| n % 2 == 1).count() > 8, "the tiles are padded");
        let (plan, buf, loaded) = run(&file, 16, 0);
        assert_eq!(plan, HeaderPlan::Ready);
        assert!(loaded.spans().len() <= 3, "{} ranges read", loaded.spans().len());
        assert!(loaded.total() * 4 < file.len() as u64, "{} of {} bytes", loaded.total(), file.len());
        assert_eq!(format!("{:?}", crate::probe_info(&buf)), format!("{:?}", crate::probe_info(&file)));
    }

    /// An ARW6 header probe reads every tile's headers and the first lines of its data, which lie in the strip a
    /// plan would skip: here the second tile, past a first one of bulk size.
    #[test]
    fn arw6_is_described_from_the_whole_file() {
        use crate::vendor::arw6_testenc::{Qis, arw6_file, one_tile_quantised};
        let (tile, _) = one_tile_quantised(64, 48, 0, Qis::REAL);
        let mut first = tile.clone();
        first.resize(BULK as usize, 0);
        let file = arw6_file(&[(0, 0, 64, 48, first), (64, 0, 64, 48, tile)], 128, 48);
        assert!(crate::probe_info(&file).is_ok());
        assert_eq!(run(&file, 16, 0).0, HeaderPlan::Whole("an ARW6 header probe reads its tiles"));
    }

    /// A TIFF with a million one-byte strips listed in descending order and all byte counts 0: the plan is
    /// built in one sort and merge, not one pass over all strips (or all holes) per strip.
    #[test]
    fn many_strips_without_byte_counts_plan_fast() {
        let n = 1_000_000u32;
        let (offsets_at, counts_at) = (8 + 2 + 5 * 12 + 4, 8 + 2 + 5 * 12 + 4 + 4 * n);
        let data = counts_at + 4 * n;
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&5u16.to_le_bytes());
        let entry = |tiff: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&typ.to_le_bytes());
            tiff.extend_from_slice(&count.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        };
        entry(&mut tiff, 256, 4, 1, 1);
        entry(&mut tiff, 257, 4, 1, n);
        entry(&mut tiff, 273, 4, n, offsets_at);
        entry(&mut tiff, 278, 4, 1, 1);
        entry(&mut tiff, 279, 4, n, counts_at);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        for i in (0..n).rev() {
            tiff.extend_from_slice(&(data + i).to_le_bytes());
        }
        tiff.resize(data as usize + n as usize, 0);
        let len = tiff.len() as u64;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut loaded = Ranges::new();
            loaded.insert(0, len);
            let _ = tx.send(header_plan(&tiff, &loaded));
        });
        let plan = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("header_plan took longer than 20 s");
        assert!(!matches!(plan, HeaderPlan::Need(_)), "{plan:?}");
    }
}
