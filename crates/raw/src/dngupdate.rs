//! Updating the preview and the XMP of an existing DNG without touching its raw data
//! (Metadata ▸ Update DNG Preview & Metadata).
//!
//! Every byte the file still needs stays where it is (the raw image's strips or tiles, its tables,
//! the other IFDs, the maker note). What is new (a baseline JPEG preview, a preview IFD that points at
//! it, the new XMP packet, the `SubIFDs` list and IFD0 that names them) is written **in place** where
//! the space of what it replaces is big enough: the old IFD0, the old XMP packet and `SubIFDs` list,
//! and a preview a previous update of this app made. What does not fit there is **appended** after the
//! end of the file, and the header (for IFD0) or the entry that names it then points at the new copy;
//! the space it leaves is unreferenced. So a second update with a preview and packet no bigger than
//! the first leaves the file the same length, and a first update grows it by what it adds. A write
//! in place never touches a range anything else in the file references (every directory, strip,
//! tile and out-of-line value is listed and checked before and after); should the result not read
//! back as built, the whole update is redone append-only.
//!
//! The entries of IFD0 are copied as the bytes they are, so every value in them keeps its meaning,
//! including the ones stored out of line (their offsets still point at the unmoved original). Only
//! `XMP` (700) and `SubIFDs` (330) differ. Directories are written with their tags in ascending order
//! as TIFF requires, so a file whose IFD0 is out of order comes out sorted. Classic TIFF and BigTIFF
//! (8-byte offsets, 20-byte entries) are both handled, in either byte order.
//!
//! A file this is not sure about is **refused**, never patched on a guess: a DNG written by Adobe
//! software (Lightroom and the DNG Converter keep their own preview and fast-load data consistent
//! with their own metadata), a file without a `DNGVersion`, directories that cannot be read or that
//! store a tag twice, and a classic TIFF that would outgrow 32-bit offsets.
//! The caller writes the result atomically (temp file, sync, rename); nothing here touches the disk.
//!
//! Sources: Adobe DNG Specification 1.7 (preview IFDs, `PreviewApplicationName`,
//! `PreviewColorSpace`, XMP in IFD0), TIFF 6.0 (IFD layout, `SubIFDs`, JPEG in strips),
//! TIFF Technical Note 2 (JPEG compression 7), the BigTIFF design notes (Aware Systems; header,
//! 20-byte entries, `LONG8`/`IFD8`). Own design.

use lcc_tiff::tags as t;
use lcc_tiff::{ByteOrder, Ifd, Tiff};

use crate::{RawError, Result};

/// `PreviewApplicationName` (DNG 1.2): which program made a preview IFD. A preview carrying this app's
/// name is one a previous update made, and the next update drops it.
pub const PREVIEW_APPLICATION_NAME: u16 = 50966;
/// `PreviewApplicationVersion`.
pub const PREVIEW_APPLICATION_VERSION: u16 = 50967;

/// Largest directory read (real DNGs have well under 200 entries in IFD0).
const MAX_ENTRIES: usize = 1024;
/// Most `SubIFDs` entries read.
const MAX_SUB_IFDS: usize = 64;
/// A preview bigger than this is not a preview (and could not be attached to a 4 GiB-addressed file).
const MAX_PREVIEW: usize = 64 << 20;
/// Largest XMP packet embedded.
const MAX_XMP: usize = 16 << 20;
/// Most the JPEG header is scanned for its frame.
const MAX_JPEG_SEGMENTS: usize = 256;

/// What an update made.
#[derive(Clone, Debug)]
pub struct Updated {
    /// The whole new file.
    pub bytes: Vec<u8>,
    /// Previews this app made earlier that the new one replaces.
    pub replaced_previews: usize,
    /// Width and height of the new preview.
    pub preview_size: (u32, u32),
    /// How much longer the file got: 0 when everything new fit where what it replaces was.
    pub appended_bytes: usize,
}

fn unsupported(why: impl Into<String>) -> RawError {
    RawError::Unsupported(why.into())
}

/// The facts about a baseline JPEG that a preview IFD repeats.
struct JpegFrame {
    width: u32,
    height: u32,
    /// Horizontal and vertical sampling of the first component (the `YCbCrSubSampling`).
    subsampling: (u16, u16),
}

/// Read the frame header of a baseline, 8-bit, three-component JPEG: DNG previews are exactly that.
fn jpeg_frame(jpeg: &[u8]) -> Result<JpegFrame> {
    let bad = |why: &str| RawError::Corrupt(format!("the preview is not a usable JPEG: {why}"));
    if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err(bad("it does not start with an SOI marker"));
    }
    let mut pos = 2usize;
    for _ in 0..MAX_JPEG_SEGMENTS {
        // skip fill bytes up to the next marker
        while jpeg.get(pos) == Some(&0xFF) && jpeg.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        if jpeg.get(pos) != Some(&0xFF) {
            return Err(bad("a marker is missing"));
        }
        let marker = *jpeg.get(pos + 1).ok_or_else(|| bad("it ends inside a marker"))?;
        pos += 2;
        match marker {
            // standalone markers
            0x01 | 0xD0..=0xD7 => continue,
            // image data begins before any frame was seen
            0xD9 | 0xDA => return Err(bad("it has no frame header")),
            _ => {}
        }
        let len =
            usize::from(u16::from_be_bytes([*jpeg.get(pos).ok_or_else(|| bad("truncated"))?, *jpeg.get(pos + 1).ok_or_else(|| bad("truncated"))?]));
        let body = jpeg.get(pos + 2..pos.checked_add(len).ok_or_else(|| bad("truncated"))?).ok_or_else(|| bad("truncated"))?;
        match marker {
            // baseline and extended sequential DCT
            0xC0 | 0xC1 => {
                let (&precision, rest) = body.split_first().ok_or_else(|| bad("short frame header"))?;
                let height = u32::from(u16::from_be_bytes([
                    *rest.first().ok_or_else(|| bad("short frame header"))?,
                    *rest.get(1).ok_or_else(|| bad("short frame header"))?,
                ]));
                let width = u32::from(u16::from_be_bytes([
                    *rest.get(2).ok_or_else(|| bad("short frame header"))?,
                    *rest.get(3).ok_or_else(|| bad("short frame header"))?,
                ]));
                let components = *rest.get(4).ok_or_else(|| bad("short frame header"))?;
                if precision != 8 || components != 3 {
                    return Err(bad("a DNG preview is 8-bit with three components"));
                }
                if width == 0 || height == 0 {
                    return Err(bad("it has no size"));
                }
                let first = *rest.get(6).ok_or_else(|| bad("short frame header"))?;
                let (h, v) = (u16::from(first >> 4), u16::from(first & 15));
                if h == 0 || v == 0 {
                    return Err(bad("a zero sampling factor"));
                }
                return Ok(JpegFrame { width, height, subsampling: (h, v) });
            }
            // progressive, lossless, arithmetic-coded frames: not what a DNG preview may be
            0xC2..=0xC3 | 0xC5..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                return Err(unsupported("a DNG preview must be a baseline JPEG"));
            }
            _ => {}
        }
        pos = pos.checked_add(len).ok_or_else(|| bad("truncated"))?;
    }
    Err(bad("no frame header was found"))
}

/// Classic TIFF or BigTIFF, in one byte order: the sizes every structure is read and written with.
#[derive(Clone, Copy)]
struct Layout {
    order: ByteOrder,
    big: bool,
}

impl Layout {
    /// Bytes per directory entry.
    fn entry_len(self) -> usize {
        if self.big { 20 } else { 12 }
    }
    /// Bytes a value fits in inside its entry.
    fn inline(self) -> usize {
        if self.big { 8 } else { 4 }
    }
    /// Bytes of a directory's entry count.
    fn count_len(self) -> usize {
        if self.big { 8 } else { 2 }
    }
    /// Bytes of an offset (the header's, a next-IFD pointer, an out-of-line value's).
    fn offset_len(self) -> usize {
        if self.big { 8 } else { 4 }
    }
    /// Where the header keeps the offset of IFD0.
    fn header_offset_at(self) -> usize {
        if self.big { 8 } else { 4 }
    }
    /// Where an entry's value field starts.
    fn value_at(self) -> usize {
        if self.big { 12 } else { 8 }
    }
    fn read_offset(self, data: &[u8], at: usize) -> Option<u64> {
        if self.big { self.order.read_u64(data, at) } else { self.order.read_u32(data, at).map(u64::from) }
    }
    fn put_offset(self, out: &mut Vec<u8>, v: u64) -> Result<()> {
        if self.big {
            self.order.put_u64(out, v);
        } else {
            self.order.put_u32(out, u32::try_from(v).map_err(|_| too_large())?);
        }
        Ok(())
    }
    /// The entry count of the directory at `at`, when it is in the file and not absurd.
    fn directory_count(self, file: &[u8], at: usize) -> Option<usize> {
        let n = if self.big { self.order.read_u64(file, at)? } else { u64::from(self.order.read_u16(file, at)?) };
        usize::try_from(n).ok().filter(|n| *n <= MAX_ENTRIES)
    }
    /// Bytes of a directory of `n` entries: count, entries, next-IFD offset.
    fn directory_len(self, n: usize) -> usize {
        self.count_len() + n.saturating_mul(self.entry_len()) + self.offset_len()
    }
}

fn too_large() -> RawError {
    unsupported("the file would be too large for 32-bit offsets")
}

/// One directory entry as the 12 (or, in a BigTIFF, 20) bytes it is.
#[derive(Clone, Copy)]
struct RawEntry {
    tag: u16,
    bytes: [u8; 20],
    len: usize,
}

impl RawEntry {
    fn bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
    fn field_type(&self, l: Layout) -> u16 {
        l.order.read_u16(&self.bytes, 2).unwrap_or(0)
    }
    fn count(&self, l: Layout) -> u64 {
        if l.big { l.order.read_u64(&self.bytes, 4) } else { l.order.read_u32(&self.bytes, 4).map(u64::from) }.unwrap_or(0)
    }
    /// The value field: the value itself when it fits, else its offset.
    fn value(&self, l: Layout) -> &[u8] {
        self.bytes.get(l.value_at()..self.len).unwrap_or(&[])
    }
}

/// One directory entry: tag, field type, count and the value field (`value`, padded; at most the inline size).
fn entry(l: Layout, tag: u16, field_type: u16, count: u64, value: &[u8]) -> Result<RawEntry> {
    if value.len() > l.inline() {
        return Err(unsupported("a value does not fit its directory entry"));
    }
    let mut b = Vec::with_capacity(20);
    l.order.put_u16(&mut b, tag);
    l.order.put_u16(&mut b, field_type);
    if l.big {
        l.order.put_u64(&mut b, count);
    } else {
        l.order.put_u32(&mut b, u32::try_from(count).map_err(|_| unsupported("a value is too long"))?);
    }
    b.extend_from_slice(value);
    b.resize(l.entry_len(), 0);
    let mut bytes = [0u8; 20];
    for (o, i) in bytes.iter_mut().zip(&b) {
        *o = *i;
    }
    Ok(RawEntry { tag, bytes, len: l.entry_len() })
}

fn short(order: ByteOrder, v: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(2);
    order.put_u16(&mut b, v);
    b
}

fn long(order: ByteOrder, v: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(4);
    order.put_u32(&mut b, v);
    b
}

const BYTE: u16 = 1;
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const LONG8: u16 = 16;
const IFD8: u16 = 18;

/// The first IFD as found in the file.
struct Head {
    layout: Layout,
    ifd0: usize,
    /// In the order the file has them (written back sorted).
    entries: Vec<RawEntry>,
    /// The offset after the entries: the next IFD of the chain, copied as it is.
    next: u64,
}

fn read_head(file: &[u8]) -> Result<Head> {
    let cut = |what: &str| RawError::Corrupt(format!("the {what} is cut short"));
    let order = match file.get(..2) {
        Some(b"II") => ByteOrder::Little,
        Some(b"MM") => ByteOrder::Big,
        _ => return Err(unsupported("it is not a TIFF-structured file")),
    };
    let big = match order.read_u16(file, 2) {
        Some(42) => false,
        Some(43) => {
            if order.read_u16(file, 4) != Some(8) || order.read_u16(file, 6) != Some(0) {
                return Err(unsupported("its BigTIFF header is not one this understands"));
            }
            true
        }
        _ => return Err(unsupported("it is not a TIFF-structured file")),
    };
    let l = Layout { order, big };
    let ifd0 = l.read_offset(file, l.header_offset_at()).ok_or_else(|| cut("header"))?;
    let ifd0 = usize::try_from(ifd0).map_err(|_| RawError::Corrupt("the first directory is outside the file".into()))?;
    let n = if big { order.read_u64(file, ifd0) } else { order.read_u16(file, ifd0).map(u64::from) }
        .ok_or_else(|| RawError::Corrupt("the first directory is outside the file".into()))?;
    let n = usize::try_from(n)
        .ok()
        .filter(|n| (1..=MAX_ENTRIES).contains(n))
        .ok_or_else(|| unsupported(format!("the first directory has {n} entries")))?;
    let table = ifd0.checked_add(l.count_len()).ok_or_else(|| unsupported("the first directory is outside the file"))?;
    let end = n.checked_mul(l.entry_len()).and_then(|b| b.checked_add(table)).ok_or_else(|| unsupported("the first directory is too large"))?;
    let next = l.read_offset(file, end).ok_or_else(|| cut("first directory"))?;
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let at = table + i * l.entry_len();
        let src = file.get(at..at + l.entry_len()).ok_or_else(|| cut("first directory"))?;
        let mut bytes = [0u8; 20];
        for (o, i) in bytes.iter_mut().zip(src) {
            *o = *i;
        }
        let tag = order.read_u16(&bytes, 0).ok_or_else(|| cut("first directory"))?;
        entries.push(RawEntry { tag, bytes, len: l.entry_len() });
    }
    // out-of-order tags are put in order when the directory is written; a tag stored twice is ambiguous
    let mut tags: Vec<u16> = entries.iter().map(|e| e.tag).collect();
    tags.sort_unstable();
    if tags.windows(2).any(|w| w.first() == w.get(1)) {
        return Err(unsupported("a tag appears twice in the first directory"));
    }
    Ok(Head { layout: l, ifd0, entries, next })
}

/// Whether a software name is one of Adobe's (Lightroom, Camera Raw, the DNG Converter, Photoshop).
fn adobe_software(s: &str) -> bool {
    s.to_ascii_lowercase().contains("adobe")
}

/// The structure checks shared by [`check`] and [`update`]: the file as parsed, its head.
fn inspect(file: &[u8]) -> Result<(Tiff, Head)> {
    let head = read_head(file)?;
    let tiff = Tiff::parse(file)?;
    let first = tiff.ifds.first().ok_or_else(|| unsupported("it has no image directory"))?;
    if first.offset != head.ifd0 as u64 || tiff.bigtiff != head.layout.big {
        return Err(unsupported("the first directory could not be read the way it is stored"));
    }
    let version = first.bytes(t::DNG_VERSION).ok_or_else(|| unsupported("it has no DNGVersion: it is not a DNG"))?;
    if version.first() != Some(&1) {
        return Err(unsupported("its DNG version is not 1.x"));
    }
    for ifd in tiff.all_ifds() {
        if ifd.string(t::SOFTWARE).is_some_and(|s| adobe_software(&s)) {
            return Err(unsupported("it was written by Adobe software, which keeps its own preview and metadata consistent: update it there"));
        }
    }
    Ok((tiff, head))
}

/// `Ok` when [`update`] would take the file; otherwise why it is refused. Cheap: reads the
/// directories only.
pub fn check(file: &[u8]) -> Result<()> {
    inspect(file).map(|_| ())
}

/// The XMP packet embedded in IFD0, if any (the file is checked as [`check`] does).
pub fn embedded_xmp(file: &[u8]) -> Result<Option<Vec<u8>>> {
    let (tiff, _) = inspect(file)?;
    Ok(tiff.ifds.first().and_then(|i| i.bytes(t::XMP)).map(<[u8]>::to_vec))
}

/// Whether `ifd` is a preview a previous update of this app made.
fn is_ours(ifd: &Ifd) -> bool {
    ifd.u32(t::NEW_SUBFILE_TYPE) == Some(1)
        && ifd.string(PREVIEW_APPLICATION_NAME).is_some_and(|s| s.trim_end_matches('\0').trim() == lcc_brand::DISPLAY_NAME)
}

/// A byte range of the file: start, end (exclusive).
type Span = (usize, usize);

fn overlaps(a: Span, b: Span) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Where an out-of-line value of `ifd` lives (`None` for a value inside its entry).
fn value_slot(ifd: &Ifd, tag: u16, l: Layout) -> Option<Span> {
    let e = ifd.get(tag)?;
    let len = e.field_type().size().checked_mul(e.count())?;
    if len <= l.inline() {
        return None;
    }
    let start = usize::try_from(e.offset).ok()?;
    Some((start, start.checked_add(len)?))
}

/// The bytes of the directory at `offset`.
fn directory_slot(file: &[u8], l: Layout, offset: u64) -> Option<Span> {
    let start = usize::try_from(offset).ok()?;
    let n = l.directory_count(file, start)?;
    Some((start, start.checked_add(l.directory_len(n))?))
}

/// Every byte range the update must leave alone: the header, every directory but IFD0, every out-of-line
/// value but IFD0's `XMP` and `SubIFDs`, every strip, tile and JPEG thumbnail. A preview of ours that
/// is being replaced is not in it. The in-place writes never touch these; the result is checked against
/// them.
fn protected(file: &[u8], tiff: &Tiff, l: Layout) -> Vec<Span> {
    fn walk(file: &[u8], ifd: &Ifd, l: Layout, first: bool, depth: usize, out: &mut Vec<Span>) {
        if depth > 16 {
            return;
        }
        if !first && let Some(s) = directory_slot(file, l, ifd.offset) {
            out.push(s);
        }
        for e in &ifd.entries {
            if first && (e.tag == t::XMP || e.tag == t::SUB_IFDS) {
                continue;
            }
            if let Some(s) = value_slot(ifd, e.tag, l) {
                out.push(s);
            }
        }
        for (offsets, counts) in [
            (t::STRIP_OFFSETS, t::STRIP_BYTE_COUNTS),
            (t::TILE_OFFSETS, t::TILE_BYTE_COUNTS),
            (t::JPEG_INTERCHANGE_FORMAT, t::JPEG_INTERCHANGE_FORMAT_LENGTH),
        ] {
            if let (Some(o), Some(c)) = (ifd.u64s(offsets), ifd.u64s(counts)) {
                for (o, c) in o.into_iter().zip(c) {
                    if let (Ok(s), Ok(n)) = (usize::try_from(o), usize::try_from(c)) {
                        out.push((s, s.saturating_add(n)));
                    }
                }
            }
        }
        for s in &ifd.sub_ifds {
            if !(first && is_ours(s)) {
                walk(file, s, l, false, depth + 1, out);
            }
        }
        for c in [&ifd.exif, &ifd.gps, &ifd.interop].into_iter().flatten() {
            walk(file, c, l, false, depth + 1, out);
        }
    }
    // the header but its IFD0 offset, which the update sets
    let mut out = vec![(0, l.header_offset_at())];
    for (i, ifd) in tiff.ifds.iter().enumerate() {
        walk(file, ifd, l, i == 0, 0, &mut out);
    }
    out
}

/// The builder: the original file, rewritten in place where a freed slot is big enough, appended to
/// (word-aligned) where it is not.
struct Out {
    bytes: Vec<u8>,
    layout: Layout,
    /// Length of the original file: slots lie within it.
    original: usize,
    /// Ranges that must not be written: what the file still references, and what this update already wrote.
    protected: Vec<Span>,
}

impl Out {
    fn align(&mut self) {
        if self.bytes.len() % 2 == 1 {
            self.bytes.push(0);
        }
    }
    /// Append `data` (word-aligned); its offset.
    fn block(&mut self, data: &[u8]) -> Result<u64> {
        self.align();
        let at = self.bytes.len();
        let end = at.checked_add(data.len()).ok_or_else(too_large)?;
        if !self.layout.big && u32::try_from(end).is_err() {
            return Err(too_large());
        }
        self.bytes.extend_from_slice(data);
        Ok(at as u64)
    }
    /// Write `data` into `slot` when it fits there and nothing else the file references is in it (the
    /// rest of the slot is zeroed); otherwise append it. Its offset.
    fn place(&mut self, data: &[u8], slot: Option<Span>) -> Result<u64> {
        if let Some((start, end)) = slot {
            let fits = end >= start
                && data.len() <= end - start
                && end <= self.original
                && start % 2 == 0
                && !self.protected.iter().any(|p| overlaps(*p, (start, end)));
            if fits && let Some(dst) = self.bytes.get_mut(start..end) {
                let (head, tail) = dst.split_at_mut(data.len());
                head.copy_from_slice(data);
                tail.fill(0);
                self.protected.push((start, end));
                return Ok(start as u64);
            }
        }
        self.block(data)
    }
    /// An entry for `data`: inside the entry when it fits, else placed (see [`Out::place`]).
    fn value(&mut self, tag: u16, field_type: u16, count: u64, data: &[u8], slot: Option<Span>) -> Result<RawEntry> {
        let l = self.layout;
        if data.len() <= l.inline() {
            return entry(l, tag, field_type, count, data);
        }
        let at = self.place(data, slot)?;
        let mut offset = Vec::with_capacity(8);
        l.put_offset(&mut offset, at)?;
        entry(l, tag, field_type, count, &offset)
    }
    /// An ASCII (NUL-terminated) value.
    fn ascii(&mut self, tag: u16, text: &str, slot: Option<Span>) -> Result<RawEntry> {
        let mut data: Vec<u8> = text.bytes().filter(|b| *b != 0).collect();
        data.push(0);
        self.value(tag, ASCII, data.len() as u64, &data, slot)
    }
    /// An offset-valued entry: LONG in a classic TIFF, LONG8 in a BigTIFF.
    fn offset_entry(&self, tag: u16, v: u64) -> Result<RawEntry> {
        let l = self.layout;
        if l.big {
            let mut b = Vec::with_capacity(8);
            l.order.put_u64(&mut b, v);
            entry(l, tag, LONG8, 1, &b)
        } else {
            entry(l, tag, LONG, 1, &long(l.order, u32::try_from(v).map_err(|_| too_large())?))
        }
    }
    /// A directory of `entries`, written sorted by tag as TIFF requires, followed by `next`; its offset.
    fn directory(&mut self, mut entries: Vec<RawEntry>, next: u64, slot: Option<Span>) -> Result<u64> {
        entries.sort_by_key(|e| e.tag);
        let l = self.layout;
        let mut data = Vec::with_capacity(l.directory_len(entries.len()));
        if l.big {
            l.order.put_u64(&mut data, entries.len() as u64);
        } else {
            l.order.put_u16(&mut data, u16::try_from(entries.len()).map_err(|_| unsupported("too many directory entries"))?);
        }
        for e in &entries {
            data.extend_from_slice(e.bytes());
        }
        l.put_offset(&mut data, next)?;
        self.place(&data, slot)
    }
}

/// Add (or replace) a baseline-JPEG preview IFD and the XMP packet of the DNG `file`, leaving the raw
/// data and everything else byte for byte as it was (see the module docs). `jpeg` is the preview
/// image (baseline, 8-bit, sRGB), `xmp` the XMP packet to embed.
///
/// A preview an earlier update of this app made is left out of the new `SubIFDs` list (its space is
/// reused when the new one fits); other previews, made by the camera or another program, are kept.
pub fn update(file: &[u8], jpeg: &[u8], xmp: &[u8]) -> Result<Updated> {
    if jpeg.is_empty() || jpeg.len() > MAX_PREVIEW {
        return Err(RawError::Corrupt("the preview image is empty or too large".into()));
    }
    if xmp.is_empty() || xmp.len() > MAX_XMP {
        return Err(RawError::Corrupt("the XMP packet is empty or too large".into()));
    }
    let frame = jpeg_frame(jpeg)?;
    let (tiff, head) = inspect(file)?;
    // in place where the freed space allows; should that not read back as intended, append everything
    match build(file, jpeg, &frame, xmp, &tiff, &head, true) {
        Ok(done) => Ok(done),
        Err(_) => build(file, jpeg, &frame, xmp, &tiff, &head, false),
    }
}

/// One rewrite; `reuse` allows writing into the space of what the update replaces.
fn build(file: &[u8], jpeg: &[u8], frame: &JpegFrame, xmp: &[u8], tiff: &Tiff, head: &Head, reuse: bool) -> Result<Updated> {
    let l = head.layout;
    let order = l.order;
    let first = tiff.ifds.first().ok_or_else(|| unsupported("it has no image directory"))?;

    // the SubIFDs already there: which stay, which are ours from before
    let mut kept: Vec<u64> = Vec::new();
    let mut ours: Vec<&Ifd> = Vec::new();
    let mut list_slot: Option<Span> = None;
    if let Some(e) = head.entries.iter().find(|e| e.tag == t::SUB_IFDS) {
        let elem = match e.field_type(l) {
            4 | 13 => 4usize,
            16 | 18 if l.big => 8,
            _ => return Err(unsupported("its SubIFDs entry is not a list of offsets this understands")),
        };
        let count = usize::try_from(e.count(l)).ok().filter(|c| (1..=MAX_SUB_IFDS).contains(c));
        let count = count.ok_or_else(|| unsupported("its SubIFDs entry is not a list of offsets this understands"))?;
        let len = count * elem;
        let value = e.value(l);
        let (src, base) = if len <= l.inline() {
            (value, 0usize)
        } else {
            let at = l.read_offset(value, 0).and_then(|a| usize::try_from(a).ok());
            let at = at.ok_or_else(|| RawError::Corrupt("the SubIFDs list is outside the file".into()))?;
            list_slot = at.checked_add(len).map(|end| (at, end));
            (file, at)
        };
        let list: Vec<u64> = (0..count)
            .map(|i| {
                let p = base.checked_add(i * elem)?;
                if elem == 4 { order.read_u32(src, p).map(u64::from) } else { order.read_u64(src, p) }
            })
            .collect::<Option<_>>()
            .ok_or_else(|| RawError::Corrupt("the SubIFDs list is outside the file".into()))?;
        if first.sub_ifds.len() != list.len() {
            return Err(unsupported("one of its SubIFDs could not be read"));
        }
        for (offset, sub) in list.into_iter().zip(&first.sub_ifds) {
            if is_ours(sub) {
                ours.push(sub);
            } else {
                kept.push(offset);
            }
        }
    }
    let replaced = ours.len();

    // the space this update frees: IFD0, its XMP packet and SubIFDs list, and a single preview of ours
    let none = (None, None, None, None, None, None);
    let (ifd0_slot, xmp_slot, old_dir, old_strip, old_bps, old_names) = if reuse {
        let old = if ours.len() == 1 { ours.first().copied() } else { None };
        let strip = old.and_then(|p| {
            let (o, c) = (p.u64s(t::STRIP_OFFSETS)?, p.u64s(t::STRIP_BYTE_COUNTS)?);
            let (o, c) = (usize::try_from(*o.first()?).ok()?, usize::try_from(*c.first()?).ok()?);
            Some((o, o.checked_add(c)?))
        });
        (
            l.directory_count(file, head.ifd0).and_then(|n| head.ifd0.checked_add(l.directory_len(n)).map(|end| (head.ifd0, end))),
            value_slot(first, t::XMP, l),
            old.and_then(|p| directory_slot(file, l, p.offset)),
            strip,
            old.and_then(|p| value_slot(p, t::BITS_PER_SAMPLE, l)),
            old.map(|p| (value_slot(p, PREVIEW_APPLICATION_NAME, l), value_slot(p, PREVIEW_APPLICATION_VERSION, l))),
        )
    } else {
        none
    };
    let list_slot = if reuse { list_slot } else { None };
    let (name_slot, version_slot) = old_names.unwrap_or((None, None));

    let guarded = protected(file, tiff, l);
    let mut out =
        Out { bytes: Vec::with_capacity(file.len() + jpeg.len() + xmp.len() + 4096), layout: l, original: file.len(), protected: guarded.clone() };
    out.bytes.extend_from_slice(file);

    // the preview: its pixels, then the directory that describes them
    let jpeg_at = out.place(jpeg, old_strip)?;
    let mut bps = Vec::new();
    for _ in 0..3 {
        order.put_u16(&mut bps, 8);
    }
    let bps_entry = out.value(t::BITS_PER_SAMPLE, SHORT, 3, &bps, old_bps)?;
    let name = out.ascii(PREVIEW_APPLICATION_NAME, lcc_brand::DISPLAY_NAME, name_slot)?;
    let version = out.ascii(PREVIEW_APPLICATION_VERSION, env!("CARGO_PKG_VERSION"), version_slot)?;
    let jpeg_len = u32::try_from(jpeg.len()).map_err(|_| unsupported("the preview is too large"))?;
    let (sub_h, sub_v) = frame.subsampling;
    let mut subsampling = short(order, sub_h);
    subsampling.extend(short(order, sub_v));
    let preview = vec![
        entry(l, t::NEW_SUBFILE_TYPE, LONG, 1, &long(order, 1))?,
        entry(l, t::IMAGE_WIDTH, LONG, 1, &long(order, frame.width))?,
        entry(l, t::IMAGE_LENGTH, LONG, 1, &long(order, frame.height))?,
        bps_entry,
        entry(l, t::COMPRESSION, SHORT, 1, &short(order, t::compression::JPEG))?,
        entry(l, t::PHOTOMETRIC, SHORT, 1, &short(order, t::photometric::YCBCR))?,
        out.offset_entry(t::STRIP_OFFSETS, jpeg_at)?,
        entry(l, t::SAMPLES_PER_PIXEL, SHORT, 1, &short(order, 3))?,
        entry(l, t::ROWS_PER_STRIP, LONG, 1, &long(order, frame.height))?,
        entry(l, t::STRIP_BYTE_COUNTS, LONG, 1, &long(order, jpeg_len))?,
        entry(l, t::PLANAR_CONFIGURATION, SHORT, 1, &short(order, 1))?,
        entry(l, t::YCBCR_SUBSAMPLING, SHORT, 2, &subsampling)?,
        entry(l, 531, SHORT, 1, &short(order, 1))?,
        name,
        version,
        // 2 = sRGB
        entry(l, t::PREVIEW_COLOR_SPACE, LONG, 1, &long(order, 2))?,
    ];
    let preview_at = out.directory(preview, 0, old_dir)?;

    // the SubIFDs list: what stays, then the new preview
    kept.push(preview_at);
    let (list_type, mut list) = if l.big { (IFD8, Vec::with_capacity(kept.len() * 8)) } else { (LONG, Vec::with_capacity(kept.len() * 4)) };
    for o in &kept {
        l.put_offset(&mut list, *o)?;
    }
    let sub_entry = out.value(t::SUB_IFDS, list_type, kept.len() as u64, &list, list_slot)?;

    // the XMP packet
    let xmp_entry = out.value(t::XMP, BYTE, xmp.len() as u64, xmp, xmp_slot)?;

    // IFD0 again, naming both; every other entry as the bytes it was
    let mut entries: Vec<RawEntry> = head.entries.iter().copied().filter(|e| e.tag != t::SUB_IFDS && e.tag != t::XMP).collect();
    entries.push(sub_entry);
    entries.push(xmp_entry);
    let ifd0_at = out.directory(entries, head.next, ifd0_slot)?;
    let mut header_offset = Vec::with_capacity(8);
    l.put_offset(&mut header_offset, ifd0_at)?;
    let at = l.header_offset_at();
    out.bytes.get_mut(at..at + l.offset_len()).ok_or_else(|| RawError::Corrupt("the header is cut short".into()))?.copy_from_slice(&header_offset);

    // the result must read back the way it was built, and nothing the file still references moved
    let untouched = guarded.iter().all(|&(s, e)| {
        let e = e.min(file.len());
        s >= e || out.bytes.get(s..e) == file.get(s..e)
    });
    let check = Tiff::parse(&out.bytes)?;
    let new_first = check.ifds.first().ok_or_else(|| RawError::Corrupt("the rewritten file has no image directory".into()))?;
    let ok = untouched
        && check.bigtiff == l.big
        && new_first.offset == ifd0_at
        && new_first.bytes(t::XMP) == Some(xmp)
        && new_first.sub_ifds.len() == kept.len()
        && new_first.sub_ifds.last().is_some_and(|s| {
            s.offset == preview_at
                && s.u32(t::IMAGE_WIDTH) == Some(frame.width)
                && s.u64(t::STRIP_OFFSETS).and_then(|o| usize::try_from(o).ok()).and_then(|o| out.bytes.get(o..o.checked_add(jpeg.len())?))
                    == Some(jpeg)
        });
    if !ok {
        return Err(RawError::Corrupt("the rewritten file did not read back as intended".into()));
    }
    let appended_bytes = out.bytes.len().saturating_sub(file.len());
    Ok(Updated { bytes: out.bytes, replaced_previews: replaced, preview_size: (frame.width, frame.height), appended_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dngwrite::{DngCompression, DngWriteOptions, write_dng};
    use crate::{BlackLevel, OpcodeLists, RawData, RawFormat, RawImage, Rect};

    /// A baseline 4:2:0 JPEG of `w × h` made with the encoder this crate already uses.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let rgb: Vec<u8> = (0..usize::from(w) * usize::from(h) * 3).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 85).encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb).expect("encodes");
        out
    }

    fn raw() -> RawImage {
        let (w, h) = (16usize, 12usize);
        let full = Rect::new(0, 0, w, h);
        RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16((0..w * h).map(|i| (i * 37 % 4000) as u16).collect()),
            cfa: None,
            bits: 12,
            black: BlackLevel::uniform(0.0),
            white: vec![4095.0],
            active_area: full,
            crop: full,
            orientation: lcc_geom::Orientation::Normal,
            color: crate::ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
        }
    }

    fn dng(order: ByteOrder) -> Vec<u8> {
        write_dng(&raw(), &DngWriteOptions { compression: DngCompression::Uncompressed, order, ..Default::default() }).expect("writes")
    }

    const XMP: &[u8] = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/></x:xmpmeta>";

    fn pixels(file: &[u8]) -> Vec<u16> {
        match crate::decode(file).expect("decodes").data {
            RawData::U16(v) => v,
            other => panic!("unexpected data {other:?}"),
        }
    }

    /// The point of the feature: the raw data and its decode are exactly what they were, the file
    /// gains a preview IFD that holds the JPEG, and the XMP is the new packet, in both byte orders.
    #[test]
    fn the_raw_is_untouched_and_the_preview_and_xmp_are_added() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let original = dng(order);
            let preview = jpeg(32, 24);
            let done = update(&original, &preview, XMP).expect("updates");
            assert_eq!(done.preview_size, (32, 24));
            assert_eq!(done.replaced_previews, 0);
            // the whole original is still there, but for the header's IFD0 offset, IFD0 and the old XMP
            // packet (which the new one may overwrite in place)
            let old = Tiff::parse(&original).expect("parses");
            let l = Layout { order, big: false };
            let mut free = vec![(4usize, 8usize), directory_slot(&original, l, old.ifds[0].offset).expect("ifd0")];
            free.extend(value_slot(&old.ifds[0], t::XMP, l));
            for (i, (a, b)) in original.iter().zip(&done.bytes).enumerate() {
                if a != b {
                    assert!(free.iter().any(|&(s, e)| (s..e).contains(&i)), "byte {i} changed");
                }
            }
            assert_eq!(pixels(&done.bytes), pixels(&original), "the raw decodes the same");
            let tiff = Tiff::parse(&done.bytes).expect("parses");
            let first = &tiff.ifds[0];
            assert_eq!(first.bytes(t::XMP), Some(XMP));
            let sub = first.sub_ifds.last().expect("a preview IFD");
            assert_eq!((sub.u32(t::NEW_SUBFILE_TYPE), sub.u16(t::COMPRESSION)), (Some(1), Some(7)));
            let strip = sub.u32(t::STRIP_OFFSETS).expect("strip") as usize;
            let len = sub.u32(t::STRIP_BYTE_COUNTS).expect("length") as usize;
            assert_eq!(done.bytes.get(strip..strip + len), Some(preview.as_slice()));
            assert_eq!(sub.string(PREVIEW_APPLICATION_NAME).as_deref().map(|s| s.trim_end_matches('\0')), Some(lcc_brand::DISPLAY_NAME));
            // everything else in IFD0 is as it was
            let before = Tiff::parse(&original).expect("parses");
            for e in &before.ifds[0].entries {
                if e.tag != t::XMP && e.tag != t::SUB_IFDS {
                    assert_eq!(first.get(e.tag).map(|x| &x.value), Some(&e.value), "tag {}", e.tag);
                }
            }
            assert_eq!(crate::probe(&done.bytes), Some(RawFormat::Dng));
        }
    }

    /// Updating again replaces our own preview (the list does not grow) and keeps a foreign one.
    #[test]
    fn a_second_update_replaces_our_preview_and_keeps_others() {
        let once = update(&dng(ByteOrder::Little), &jpeg(32, 24), XMP).expect("first").bytes;
        let twice = update(&once, &jpeg(48, 36), XMP).expect("second");
        assert_eq!(twice.replaced_previews, 1);
        let tiff = Tiff::parse(&twice.bytes).expect("parses");
        assert_eq!(tiff.ifds[0].sub_ifds.len(), 1, "one preview, not two");
        assert_eq!(tiff.ifds[0].sub_ifds[0].u32(t::IMAGE_WIDTH), Some(48));
        assert_eq!(pixels(&twice.bytes), pixels(&once));
    }

    /// A second update whose preview and packet fit where the first one's are rewrites in place: the
    /// file keeps its length; a bigger one falls back to appending. The raw decodes the same each time.
    #[test]
    fn an_update_that_fits_is_written_in_place() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let original = dng(order);
            let once = update(&original, &jpeg(32, 24), XMP).expect("first");
            assert!(once.appended_bytes > 0, "a first update adds what was not there");
            let twice = update(&once.bytes, &jpeg(32, 24), XMP).expect("second");
            assert_eq!(twice.appended_bytes, 0, "{order:?}");
            assert_eq!(twice.bytes.len(), once.bytes.len());
            assert_eq!(twice.bytes.get(..original.len()), once.bytes.get(..original.len()), "the original part is untouched");
            let small = &XMP[..XMP.len() - 20];
            let thrice = update(&twice.bytes, &jpeg(16, 12), small).expect("smaller");
            assert_eq!(thrice.appended_bytes, 0);
            let tiff = Tiff::parse(&thrice.bytes).expect("parses");
            assert_eq!(tiff.ifds[0].bytes(t::XMP), Some(small));
            assert_eq!(tiff.ifds[0].sub_ifds[0].u32(t::IMAGE_WIDTH), Some(16));
            let bigger = update(&thrice.bytes, &jpeg(96, 72), XMP).expect("bigger");
            assert!(bigger.appended_bytes > 0);
            for f in [&twice.bytes, &thrice.bytes, &bigger.bytes] {
                assert_eq!(pixels(f), pixels(&original));
                assert_eq!(Tiff::parse(f).expect("parses").ifds[0].sub_ifds.len(), 1);
            }
        }
    }

    /// An XMP entry whose space overlaps the raw strip (a hostile or broken file) is never written in
    /// place: the strip stays byte for byte.
    #[test]
    fn space_something_else_uses_is_never_reused() {
        let once = update(&dng(ByteOrder::Little), &jpeg(32, 24), XMP).expect("first").bytes;
        let tiff = Tiff::parse(&once).expect("parses");
        let raw_ifd = &tiff.ifds[0];
        let strip = raw_ifd.u32(t::STRIP_OFFSETS).or_else(|| raw_ifd.sub_ifds.first().and_then(|s| s.u32(t::STRIP_OFFSETS)));
        let strip = strip.expect("a strip") as usize;
        let ifd0 = u32::from_le_bytes([once[4], once[5], once[6], once[7]]) as usize;
        let n = u16::from_le_bytes([once[ifd0], once[ifd0 + 1]]) as usize;
        let mut bad = once.clone();
        let mut patched = false;
        for i in 0..n {
            let at = ifd0 + 2 + i * 12;
            if u16::from_le_bytes([bad[at], bad[at + 1]]) == t::XMP {
                bad[at + 8..at + 12].copy_from_slice(&(strip as u32).to_le_bytes());
                patched = true;
            }
        }
        assert!(patched);
        let before = pixels(&bad);
        let done = update(&bad, &jpeg(32, 24), XMP).expect("updates");
        assert_eq!(pixels(&done.bytes), before);
        assert_eq!(done.bytes.get(strip..strip + 64), bad.get(strip..strip + 64));
    }

    /// An IFD0 stored out of tag order is accepted and written back in order.
    #[test]
    fn unsorted_tags_are_sorted_on_write() {
        let mut file = dng(ByteOrder::Little);
        let ifd0 = u32::from_le_bytes([file[4], file[5], file[6], file[7]]) as usize;
        let (a, b) = (ifd0 + 2, ifd0 + 2 + 12);
        let first: Vec<u8> = file[a..a + 12].to_vec();
        let second: Vec<u8> = file[b..b + 12].to_vec();
        file[a..a + 12].copy_from_slice(&second);
        file[b..b + 12].copy_from_slice(&first);
        let done = update(&file, &jpeg(32, 24), XMP).expect("updates");
        let at = u32::from_le_bytes([done.bytes[4], done.bytes[5], done.bytes[6], done.bytes[7]]) as usize;
        let n = u16::from_le_bytes([done.bytes[at], done.bytes[at + 1]]) as usize;
        let tags: Vec<u16> = (0..n).map(|i| u16::from_le_bytes([done.bytes[at + 2 + i * 12], done.bytes[at + 3 + i * 12]])).collect();
        assert!(tags.windows(2).all(|w| w[0] < w[1]), "{tags:?}");
        assert_eq!(pixels(&done.bytes), pixels(&dng(ByteOrder::Little)));
    }

    /// A minimal BigTIFF DNG: one uncompressed CFA strip in IFD0, an out-of-line XMP packet.
    fn big_dng(order: ByteOrder) -> (Vec<u8>, Vec<u8>) {
        use lcc_tiff::Value;
        use lcc_tiff::writer::{IfdBuilder, ImageData, TiffWriter};
        let strip: Vec<u8> = (0..16 * 12 * 2).map(|i| (i * 7 % 256) as u8).collect();
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]))
            .set(t::IMAGE_WIDTH, Value::Long(vec![16]))
            .set(t::IMAGE_LENGTH, Value::Long(vec![12]))
            .set(t::BITS_PER_SAMPLE, Value::Short(vec![16]))
            .set(t::COMPRESSION, Value::Short(vec![1]))
            .set(t::PHOTOMETRIC, Value::Short(vec![32803]))
            .set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]))
            .set(t::MAKE, Value::Ascii("Test".into()))
            .set(t::MODEL, Value::Ascii("BigTIFF camera".into()))
            .set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]))
            .set(t::XMP, Value::Byte(b"<x:xmpmeta old packet, long enough to be out of line/>".to_vec()));
        ifd.set_image(ImageData::Strips { rows_per_strip: 12, strips: vec![strip.clone()] });
        (TiffWriter::new(order, true).write(&[ifd]).expect("writes"), strip)
    }

    /// BigTIFF: updated in both byte orders, reads back as BigTIFF, the strip untouched; a second update
    /// that fits is in place. Damaged BigTIFFs never panic.
    #[test]
    fn bigtiff_dngs_are_updated() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let (file, strip) = big_dng(order);
            let strip_at = |f: &[u8]| {
                let tiff = Tiff::parse(f).expect("parses");
                assert!(tiff.bigtiff);
                tiff.ifds[0].u64(t::STRIP_OFFSETS).expect("strip") as usize
            };
            let at = strip_at(&file);
            let once = update(&file, &jpeg(32, 24), XMP).expect("updates");
            assert_eq!(strip_at(&once.bytes), at);
            assert_eq!(once.bytes.get(at..at + strip.len()), Some(strip.as_slice()));
            let tiff = Tiff::parse(&once.bytes).expect("parses");
            assert_eq!(tiff.ifds[0].bytes(t::XMP), Some(XMP));
            assert_eq!(tiff.ifds[0].sub_ifds[0].u32(t::IMAGE_WIDTH), Some(32));
            assert_eq!(tiff.ifds[0].string(t::MODEL).as_deref(), Some("BigTIFF camera"));
            let twice = update(&once.bytes, &jpeg(32, 24), XMP).expect("again");
            assert_eq!(twice.appended_bytes, 0);
            assert_eq!(twice.replaced_previews, 1);
            assert_eq!(twice.bytes.get(at..at + strip.len()), Some(strip.as_slice()));
            let head = once.bytes.clone();
            for cut in (0..head.len()).step_by((head.len() / 300).max(1)) {
                let _ = update(&head[..cut], &jpeg(8, 8), XMP);
            }
            let ifd0 = match order {
                ByteOrder::Little => u64::from_le_bytes(head[8..16].try_into().unwrap()),
                ByteOrder::Big => u64::from_be_bytes(head[8..16].try_into().unwrap()),
            } as usize;
            let mut spots: Vec<usize> = (0..16).collect();
            spots.extend(ifd0..(ifd0 + 8 + 20 * 20).min(head.len()));
            for s in spots {
                let mut bad = head.clone();
                bad[s] ^= 0x41;
                if let Ok(done) = update(&bad, &jpeg(8, 8), XMP) {
                    assert!(Tiff::parse(&done.bytes).is_ok(), "byte {s}");
                }
            }
        }
    }

    /// Refusals, each with a message (no panic, nothing returned to write).
    #[test]
    fn files_that_are_not_safe_to_patch_are_refused() {
        let good = dng(ByteOrder::Little);
        let preview = jpeg(8, 8);
        let refused = |file: &[u8]| update(file, &preview, XMP).expect_err("refused").to_string();
        // not a TIFF at all, empty, a header alone
        for junk in [&b""[..], b"II", b"hello world, not a raw file", b"II*\0\x08\0\0\0"] {
            refused(junk);
        }
        // a BigTIFF header that does not say 8-byte offsets
        let mut big = good.clone();
        big[2..4].copy_from_slice(&43u16.to_le_bytes());
        assert!(refused(&big).contains("BigTIFF"));
        // a tag stored twice in IFD0
        let mut twice = good.clone();
        let ifd0 = u32::from_le_bytes([good[4], good[5], good[6], good[7]]) as usize;
        let tag = [good[ifd0 + 2], good[ifd0 + 3]];
        twice[ifd0 + 14..ifd0 + 16].copy_from_slice(&tag);
        assert!(refused(&twice).contains("twice"), "{}", refused(&twice));
        // Adobe software
        let mut adobe = good.clone();
        let sw = lcc_tiff::Tiff::parse(&good).expect("parses").ifds[0].get(t::SOFTWARE).expect("software").offset as usize;
        let name = crate::dngwrite::software();
        assert_eq!(&good[sw..sw + name.len()], name.as_bytes());
        adobe[sw..sw + 5].copy_from_slice(b"Adobe");
        assert!(refused(&adobe).contains("Adobe"), "{}", refused(&adobe));
        // a bad preview is refused before anything is built
        assert!(update(&good, b"not a jpeg", XMP).is_err());
        assert!(update(&good, &preview, b"").is_err());
        let mut progressive = preview.clone();
        if let Some(i) = progressive.windows(2).position(|w| w == [0xFF, 0xC0]) {
            progressive[i + 1] = 0xC2;
        }
        assert!(update(&good, &progressive, XMP).is_err());
    }

    /// Hostile input: every truncation of a valid DNG and a flip of each header / directory byte gives
    /// an error or a file that still parses, never a panic and never a result that fails its own check.
    #[test]
    fn damaged_files_never_panic() {
        let good = dng(ByteOrder::Little);
        let preview = jpeg(8, 8);
        let step = (good.len() / 400).max(1);
        for cut in (0..good.len()).step_by(step) {
            let _ = update(&good[..cut], &preview, XMP);
            let _ = check(&good[..cut]);
            let _ = embedded_xmp(&good[..cut]);
        }
        let ifd0 = u32::from_le_bytes([good[4], good[5], good[6], good[7]]) as usize;
        let mut spots: Vec<usize> = (0..16).collect();
        spots.extend(ifd0..(ifd0 + 2 + 12 * 40).min(good.len()));
        for at in spots {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut bad = good.clone();
                bad[at] ^= flip;
                if let Ok(done) = update(&bad, &preview, XMP) {
                    assert!(Tiff::parse(&done.bytes).is_ok(), "byte {at} ^ {flip:#x}");
                }
            }
        }
        // the in-place path: a file an earlier update wrote, damaged in IFD0 and in our preview directory
        let once = update(&good, &preview, XMP).expect("first").bytes;
        let raw_pixels = pixels(&once);
        let tiff = Tiff::parse(&once).expect("parses");
        let mut spots: Vec<usize> = (0..16).collect();
        for offset in [tiff.ifds[0].offset, tiff.ifds[0].sub_ifds[0].offset] {
            let at = offset as usize;
            spots.extend(at..(at + 2 + 12 * 40).min(once.len()));
        }
        for at in spots {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut bad = once.clone();
                bad[at] ^= flip;
                if let Ok(done) = update(&bad, &preview, XMP) {
                    assert!(Tiff::parse(&done.bytes).is_ok(), "updated file byte {at} ^ {flip:#x}");
                }
            }
        }
        assert_eq!(pixels(&update(&once, &jpeg(16, 12), XMP).expect("again").bytes), raw_pixels);
        // a SubIFDs loop and an offset list pointing outside
        let mut looped = good.clone();
        looped[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(update(&looped, &preview, XMP).is_err());
    }

    /// The JPEG reader only takes what a DNG preview may be, and never reads out of bounds.
    #[test]
    fn jpeg_frames_are_read_strictly() {
        let good = jpeg(40, 30);
        let f = jpeg_frame(&good).expect("frame");
        assert_eq!((f.width, f.height), (40, 30));
        assert!(f.subsampling.0 >= 1 && f.subsampling.1 >= 1);
        for cut in 0..good.len().min(300) {
            let _ = jpeg_frame(&good[..cut]);
        }
        assert!(jpeg_frame(&[0xFF, 0xD8, 0xFF]).is_err());
        assert!(jpeg_frame(&[0xFF, 0xD8, 0xFF, 0xC0, 0xFF, 0xFF]).is_err());
    }
}
