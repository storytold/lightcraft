//! HEIF, HEIC and AVIF: the metadata items of the image file format built on the ISO base media
//! file format (ISO/IEC 23008-12 on ISO/IEC 14496-12), read from the container alone. No picture
//! is decoded and no decoder is needed, so this works in builds without the codecs' optional
//! `heif` feature.
//!
//! - `ftyp` names a HEIF or AVIF brand ([`is_heif`]).
//! - The top-level `meta` box lists the items (`iinf`, one `infe` each), says where each item's
//!   bytes are (`iloc`: extents in the file, construction method 0, or in the `meta` box's own
//!   `idat`, method 1), which item is the photo (`pitm`) and how items relate (`iref`).
//! - EXIF is an item of type `Exif`: a 4-byte big-endian offset to the TIFF header, then the
//!   payload (ISO/IEC 23008-12 annex A). iPhones write offset 6 and an `Exif\0\0` prefix; libheif
//!   and avif-serialize write offset 0 and the TIFF stream.
//! - XMP is an item of type `mime` with content type `application/rdf+xml`.
//! - A `cdsc` ("content describes") reference ties a metadata item to the image it describes. The
//!   photo's items are those tied to the primary image or to an image it is made from (its `dimg`
//!   inputs: a grid's tiles, the picture an `iden` turns, an `iovl` overlay's or a `tmap` tone
//!   map's inputs), else those tied to no image (they describe the file). An item tied only to
//!   other images (a thumbnail, a depth map, another photo of a collection) is not this photo's.
//! - [`embeds_location`] looks at every EXIF and XMP item instead, for export's "remove location".
//!
//! The ICC profile is left to the decoder, which reads it for rendering; nothing here needs it.
//! Every size and offset is checked against the buffer; the boxes read, the items considered and
//! the bytes gathered are bounded, and `iloc` is walked once.
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, HashSet};

use crate::container::Embedded;
use crate::cr3::{BoxRef, be16, be32, be64, boxes};

/// The `ftyp` brands of HEIF still images and image sequences, and of AVIF: those
/// `lightcraft_codecs::sniff` takes as HEIF or AVIF.
const BRANDS: [&[u8; 4]; 12] = [b"mif1", b"msf1", b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"avif", b"avis"];
/// Brands of other formats in the same container (Canon CR3, JPEG XL), which win as they do there.
const OTHERS: [&[u8; 4]; 2] = [b"crx ", b"jxl "];
/// How far into the `ftyp` box brands are looked for.
const FTYP_MAX: usize = 4096;
/// Boxes read in one file. An iPhone photo has about a hundred items, a large grid a few hundred.
const MAX_BOXES: usize = 1 << 16;
/// Metadata items considered for the photo's own EXIF and XMP (a real file has one or two).
const MAX_CANDIDATES: usize = 64;
/// Metadata items [`embeds_location`] reads; a file with more counts as having a location.
const MAX_SCANNED: usize = 1024;
/// The most bytes one metadata item may gather (EXIF and XMP are kilobytes).
const MAX_ITEM: usize = 16 << 20;
/// The most bytes [`embeds_location`] gathers over all items.
const MAX_SCANNED_BYTES: usize = 64 << 20;

/// Does the `ftyp` box name a HEIF or AVIF brand (and not CR3 or JPEG XL)?
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.get(4..8) != Some(b"ftyp".as_slice()) {
        return false;
    }
    // major brand at 8, minor version at 12, compatible brands from 16 to the end of the box
    let size = be32(bytes, 0).and_then(|s| usize::try_from(s).ok()).unwrap_or(0);
    let end = size.clamp(12, FTYP_MAX).min(bytes.len());
    let compatible = (16..end).step_by(4).filter_map(|at| bytes.get(at..at.checked_add(4).filter(|&e| e <= end)?));
    let mut heif = false;
    for brand in bytes.get(8..12).into_iter().chain(compatible) {
        if OTHERS.iter().any(|b| b.as_slice() == brand) {
            return false;
        }
        heif |= BRANDS.iter().any(|b| b.as_slice() == brand);
    }
    heif
}

/// The Exif and XMP of a HEIF file, or empty metadata when no readable items exist.
pub fn metadata(bytes: &[u8]) -> Embedded {
    embedded(bytes).unwrap_or_default()
}

/// The EXIF and XMP of a HEIF file's primary image; `None` when `bytes` is not HEIF.
pub(crate) fn embedded(bytes: &[u8]) -> Option<Embedded> {
    let c = Container::read(bytes, MAX_CANDIDATES)?;
    // the photo's own items first, then those tied to no image; the first that reads well wins
    let read = |kind: Kind| {
        let mut order: Vec<(u8, usize)> =
            c.items.iter().enumerate().filter(|(_, it)| it.kind == kind).filter_map(|(i, it)| Some((it.rank(c.primary.is_some())?, i))).collect();
        order.sort_by_key(|&(rank, _)| rank);
        order.into_iter().filter_map(|(_, i)| match c.gather(i, MAX_ITEM) {
            Gathered::Bytes(b) => Some(b),
            _ => None,
        })
    };
    let exif = read(Kind::Exif).find_map(|item| exif_tiff(&item).map(<[u8]>::to_vec));
    let xmp = read(Kind::Xmp).next().map(|item| String::from_utf8_lossy(&item).into_owned());
    Some(Embedded { exif, xmp, ..Default::default() })
}

/// Does a HEIF file hold GPS or a place name in any EXIF or XMP item, whichever image it describes?
/// `None` when `bytes` is not HEIF. An item that is there but can't be read (compressed XMP, cut
/// off by the end of the file, too large, built from other items or in another file, not a
/// TIFF stream or an XMP packet, or without a reliable placement) counts as one, and so do more
/// items than [`MAX_SCANNED`] or an incomplete item catalog.
pub(crate) fn embeds_location(bytes: &[u8]) -> Option<bool> {
    let c = Container::read(bytes, MAX_SCANNED)?;
    if c.more || !c.catalog_complete {
        return Some(true);
    }
    let mut budget = MAX_SCANNED_BYTES;
    for (i, it) in c.items.iter().enumerate() {
        let item = match c.gather(i, MAX_ITEM.min(budget)) {
            Gathered::Bytes(b) => b,
            Gathered::NotPlaced => continue,
            Gathered::Unreadable | Gathered::TooLarge => return Some(true),
        };
        budget = budget.saturating_sub(item.len());
        let located = match it.kind {
            Kind::Exif => match exif_tiff(&item) {
                Some(tiff) => crate::try_read_exif(tiff).map_or(true, |m| crate::has_location(&m)),
                None => !item.is_empty(),
            },
            Kind::Xmp => crate::parse_xmp(&String::from_utf8_lossy(&item)).map_or(true, |x| crate::has_location(&x.metadata)),
            Kind::EncodedXmp => true,
        };
        if located {
            return Some(true);
        }
    }
    Some(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Exif,
    Xmp,
    /// XMP with a content encoding (compressed): not read, but [`embeds_location`] counts it.
    EncodedXmp,
}

/// A metadata item and the images it describes.
#[derive(Debug)]
struct Candidate {
    id: u32,
    kind: Kind,
    /// Tied to some image by a `cdsc` reference.
    describes: bool,
    /// Tied to the primary image or an image it is made from.
    describes_primary: bool,
}

impl Candidate {
    /// 0 for the photo's own, 1 for one tied to no image (or any, when the file names no primary
    /// image that exists); `None` for one that describes other images only.
    fn rank(&self, has_primary: bool) -> Option<u8> {
        if self.describes_primary {
            Some(0)
        } else if !self.describes || !has_primary {
            Some(1)
        } else {
            None
        }
    }
}

/// What the `meta` box says about the metadata items, and where their bytes are.
struct Container<'a> {
    file: &'a [u8],
    idat: Option<&'a [u8]>,
    iloc: Option<Iloc<'a>>,
    /// The primary image, when `pitm` names an item `iinf` lists.
    primary: Option<u32>,
    /// The EXIF and XMP items, in `iinf` order (the first `cap` of them).
    items: Vec<Candidate>,
    /// More metadata items than `cap`.
    more: bool,
    /// Discovery read the whole item catalog, including each entry's type.
    catalog_complete: bool,
    /// Where each of `items` is (from one walk of `iloc`).
    places: Vec<Option<Place>>,
    /// A complete `iloc` walk established that any unplaced item has no entry.
    iloc_complete: bool,
}

impl<'a> Container<'a> {
    /// `None` when `bytes` is not HEIF; no items when there is no `meta` box.
    fn read(bytes: &'a [u8], cap: usize) -> Option<Container<'a>> {
        if !is_heif(bytes) {
            return None;
        }
        let mut c = Container {
            file: bytes,
            idat: None,
            iloc: None,
            primary: None,
            items: Vec::new(),
            more: false,
            catalog_complete: false,
            places: Vec::new(),
            iloc_complete: false,
        };
        let mut budget = MAX_BOXES;
        let top = boxes(bytes, 0, bytes.len(), &mut budget);
        c.catalog_complete = top.last().is_some_and(|b| b.end == bytes.len());
        // A complete file without an item `meta` has no item catalog (e.g. a sequence).
        let Some(meta) = top.iter().find(|b| &b.kind == b"meta") else { return Some(c) };
        // `meta` is a full box: version and flags, then the boxes inside it.
        let first = meta.body.saturating_add(4);
        let inside = boxes(bytes, first, meta.end, &mut budget);
        c.catalog_complete &= version(bytes, meta) == Some(0)
            && first <= meta.end
            && inside.last().map_or(first, |b| b.end) == meta.end
            && top.iter().filter(|b| &b.kind == b"meta").count() == 1
            && inside.iter().filter(|b| &b.kind == b"iinf").count() == 1;
        let child = |kind: &[u8; 4]| inside.iter().find(|b| &b.kind == kind);
        let payload = |kind: &[u8; 4]| child(kind).and_then(|b| bytes.get(b.body..b.end));
        let mut ids = Vec::new();
        if let Some(iinf) = child(b"iinf") {
            let catalog = item_infos(bytes, iinf, cap, &mut budget);
            ids = catalog.ids;
            c.items = catalog.items;
            c.more = catalog.more;
            c.catalog_complete &= catalog.complete;
        }
        c.primary = payload(b"pitm").and_then(primary_item).filter(|p| ids.binary_search(p).is_ok());
        if let Some(iref) = child(b"iref") {
            link(bytes, iref, c.primary, &mut c.items, &mut budget);
        }
        c.idat = payload(b"idat");
        c.iloc = payload(b"iloc").and_then(Iloc::parse);
        (c.places, c.iloc_complete) = match &c.iloc {
            Some(iloc) => iloc.locate(&c.items),
            None => (Vec::new(), false),
        };
        Some(c)
    }

    /// The bytes of `items[i]`, at most `limit` of them.
    fn gather(&self, i: usize, limit: usize) -> Gathered {
        match (&self.iloc, self.places.get(i)) {
            (Some(iloc), Some(Some(place))) => iloc.gather(self.file, self.idat, place, limit),
            _ if self.iloc_complete => Gathered::NotPlaced,
            _ => Gathered::Unreadable,
        }
    }
}

/// An item's bytes, or why there are none.
#[derive(Debug)]
enum Gathered {
    Bytes(Vec<u8>),
    /// A complete `iloc` walk found no entry for the item.
    NotPlaced,
    /// Placement unresolved, outside the data, in another file, or built from other items (method 2).
    Unreadable,
    /// More than the limit.
    TooLarge,
}

/// A full box's version: the first byte of its own payload (`None` for an empty box).
fn version(bytes: &[u8], b: &BoxRef) -> Option<u8> {
    bytes.get(b.body..b.end)?.first().copied()
}

/// `pitm`: the primary item's id (16 bits in version 0, else 32).
fn primary_item(pitm: &[u8]) -> Option<u32> {
    match pitm.first()? {
        0 => be16(pitm, 4).map(u32::from),
        _ => be32(pitm, 4),
    }
}

/// Best-effort discovery is kept even when the catalog cannot establish that all items were read.
#[derive(Default)]
struct ItemCatalog {
    ids: Vec<u32>,
    items: Vec<Candidate>,
    more: bool,
    complete: bool,
}

fn item_infos(bytes: &[u8], iinf: &BoxRef, cap: usize, budget: &mut usize) -> ItemCatalog {
    let mut catalog = ItemCatalog::default();
    let Some(data) = bytes.get(iinf.body..iinf.end) else { return catalog };
    // Only the supported versions establish the count and where the entries start.
    let (count, header) = match version(bytes, iinf) {
        Some(0) => (be16(data, 4).map(u32::from), 6),
        Some(1) => (be32(data, 4), 8),
        _ => return catalog,
    };
    let Some(count) = count else { return catalog };
    let first = iinf.body.saturating_add(header);
    let entries = boxes(bytes, first, iinf.end, budget);
    catalog.complete = entries.last().map_or(first, |b| b.end) == iinf.end && u32::try_from(entries.len()).ok() == Some(count);
    for b in &entries {
        let info = bytes.get(b.body..b.end).filter(|_| &b.kind == b"infe").and_then(item_info);
        let Some((id, kind)) = info else {
            catalog.complete = false;
            continue;
        };
        catalog.ids.push(id);
        match kind {
            Some(_) if catalog.items.len() >= cap => catalog.more = true,
            Some(kind) => catalog.items.push(Candidate { id, kind, describes: false, describes_primary: false }),
            None => {}
        }
    }
    catalog.ids.sort_unstable();
    catalog.ids.dedup();
    catalog
}

/// An `infe` entry's item id, and its kind when it is an unprotected `Exif` or XMP item (`mime`,
/// `application/rdf+xml`).
fn item_info(infe: &[u8]) -> Option<(u32, Option<Kind>)> {
    let version = *infe.first()?;
    // version and flags, the item id (32 bits in version 3), the protection index
    let (id, at) = match version {
        0..=2 => (u32::from(be16(infe, 4)?), 6usize),
        3 => (be32(infe, 4)?, 8usize),
        _ => return None,
    };
    if be16(infe, at)? != 0 {
        return Some((id, None)); // protected (encrypted)
    }
    let at = at.checked_add(2)?;
    // versions 0 and 1 have no item type and describe MIME items; 2 and 3 name the type
    let (kind, at) = if version >= 2 { (infe.get(at..at.checked_add(4)?)?, at.checked_add(4)?) } else { (b"mime".as_slice(), at) };
    let (_name, at) = string(infe, at)?;
    let kind = match kind {
        b"Exif" => Some(Kind::Exif),
        b"mime" => {
            let (content_type, at) = string(infe, at)?;
            // Content encoding may be absent, but a present string must terminate.
            let encoding = if at == infe.len() { b"".as_slice() } else { string(infe, at)?.0 };
            match (content_type.trim_ascii().eq_ignore_ascii_case(b"application/rdf+xml"), encoding.trim_ascii().is_empty()) {
                (true, true) => Some(Kind::Xmp),
                (true, false) => Some(Kind::EncodedXmp),
                (false, _) => None,
            }
        }
        b"uri " => {
            string(infe, at)?;
            None
        }
        _ => None,
    };
    Some((id, kind))
}

/// The NUL-terminated string at `at` and where the next field starts.
fn string(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let rest = b.get(at..)?;
    let n = rest.iter().position(|&c| c == 0)?;
    Some((rest.get(..n)?, at.checked_add(n)?.checked_add(1)?))
}

/// Mark the candidates that `cdsc` references tie to an image, and those tied to the primary image
/// or one it is made from (`dimg`, followed to any depth; each image once, so loops end).
fn link(bytes: &[u8], iref: &BoxRef, primary: Option<u32>, items: &mut [Candidate], budget: &mut usize) {
    // full box; item ids are 16 bits in version 0, 32 in version 1
    let width = match version(bytes, iref) {
        Some(0) => 2usize,
        Some(1) => 4,
        _ => return,
    };
    let index: HashMap<u32, usize> = items.iter().enumerate().rev().map(|(i, c)| (c.id, i)).collect();
    let mut inputs: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut described: Vec<(usize, Vec<u32>)> = Vec::new();
    for r in boxes(bytes, iref.body.saturating_add(4), iref.end, budget) {
        let kind = match &r.kind {
            b"cdsc" => b"cdsc",
            b"dimg" => b"dimg",
            _ => continue,
        };
        let Some(body) = bytes.get(r.body..r.end) else { continue };
        let Some(from) = id_at(body, 0, width) else { continue };
        let item = index.get(&from).copied();
        if kind == b"cdsc" && item.is_none() {
            continue;
        }
        let count = be16(body, width).unwrap_or(0);
        let mut to = Vec::new();
        let mut at = width.saturating_add(2);
        for _ in 0..count {
            let Some(id) = id_at(body, at, width) else { break };
            to.push(id);
            at = at.saturating_add(width);
        }
        match item {
            Some(i) if kind == b"cdsc" => described.push((i, to)),
            _ => inputs.entry(from).or_default().extend(to),
        }
    }
    // the primary image and every image it is made from
    let mut photo = HashSet::new();
    let mut todo: Vec<u32> = primary.into_iter().collect();
    while let Some(id) = todo.pop() {
        if photo.insert(id) {
            todo.extend(inputs.get(&id).into_iter().flatten().filter(|i| !photo.contains(*i)));
        }
    }
    for (i, to) in described {
        if let Some(item) = items.get_mut(i)
            && !to.is_empty()
        {
            item.describes = true;
            item.describes_primary |= to.iter().any(|t| photo.contains(t));
        }
    }
}

fn id_at(b: &[u8], at: usize, width: usize) -> Option<u32> {
    if width == 2 { be16(b, at).map(u32::from) } else { be32(b, at) }
}

/// `iloc`: its field sizes, read once.
struct Iloc<'a> {
    data: &'a [u8],
    version: u8,
    offset_size: usize,
    length_size: usize,
    base_size: usize,
    index_size: usize,
}

/// Where an item is: how it is built, and its extents in `iloc`.
#[derive(Clone, Copy, Debug)]
struct Place {
    method: u16,
    data_reference: u16,
    base: u64,
    /// Where its first extent starts in `iloc`.
    extents_at: usize,
    extents: usize,
}

impl<'a> Iloc<'a> {
    fn parse(data: &'a [u8]) -> Option<Iloc<'a>> {
        let version = *data.first()?;
        if version > 2 {
            return None;
        }
        // version and flags, then four 4-bit field sizes: offset, length, base offset, extent index
        let (sizes, more) = (*data.get(4)?, *data.get(5)?);
        let (offset_size, length_size) = (usize::from(sizes / 16), usize::from(sizes % 16));
        let base_size = usize::from(more / 16);
        let index_size = if version == 0 { 0 } else { usize::from(more % 16) }; // reserved in version 0
        if [offset_size, length_size, base_size, index_size].into_iter().any(|n| !matches!(n, 0 | 4 | 8)) {
            return None;
        }
        Some(Iloc { data, version, offset_size, length_size, base_size, index_size })
    }

    /// One walk over the entries: where each of `items` is (the first entry for an id wins). It
    /// stops once all are placed, or where the entries stop making sense. The boolean is true
    /// only if the walk exhausted the entries, so an absent placement is known to be absent.
    fn locate(&self, items: &[Candidate]) -> (Vec<Option<Place>>, bool) {
        let mut places = vec![None; items.len()];
        if items.is_empty() {
            return (places, false);
        }
        let wanted: HashMap<u32, usize> = items.iter().enumerate().rev().map(|(i, c)| (c.id, i)).collect();
        let mut left = wanted.len();
        let complete = self.walk(|id, place| {
            if let Some(&i) = wanted.get(&id)
                && let Some(slot @ None) = places.get_mut(i)
            {
                *slot = Some(place);
                left = left.saturating_sub(1);
            }
            left > 0
        });
        (places, complete.unwrap_or(false))
    }

    /// Call `f` with each entry's item id and place. `Some(true)` means every entry was read,
    /// `Some(false)` means `f` stopped the walk, and `None` means an entry ran past the box.
    fn walk(&self, mut f: impl FnMut(u32, Place) -> bool) -> Option<bool> {
        let (version, data) = (self.version, self.data);
        let id_size = if version == 2 { 4 } else { 2 };
        let (count, mut at) = if version == 2 { (be32(data, 6)?, 10usize) } else { (u32::from(be16(data, 6)?), 8usize) };
        let extent_size = self.index_size.checked_add(self.offset_size)?.checked_add(self.length_size)?;
        // (every entry takes at least six bytes, so a count larger than the box ends at its end)
        for _ in 0..count {
            iloc_entry_visited();
            let id = uint(data, at, id_size)?;
            at = at.checked_add(id_size)?;
            let method = if version == 0 {
                0
            } else {
                let m = be16(data, at)? & 15;
                at = at.checked_add(2)?;
                m
            };
            let data_reference = be16(data, at)?;
            let base = uint(data, at.checked_add(2)?, self.base_size)?;
            at = at.checked_add(2)?.checked_add(self.base_size)?;
            let extents = usize::from(be16(data, at)?);
            at = at.checked_add(2)?;
            let place = Place { method, data_reference, base, extents_at: at, extents };
            at = at.checked_add(extents.checked_mul(extent_size)?)?;
            if at > data.len() {
                return None;
            }
            if let Ok(id) = u32::try_from(id)
                && !f(id, place)
            {
                return Some(false);
            }
        }
        Some(true)
    }

    /// An item's bytes, its extents in order from the file (construction method 0) or from the
    /// `meta` box's `idat` (method 1), at most `limit` of them.
    fn gather(&self, file: &[u8], idat: Option<&[u8]>, place: &Place, limit: usize) -> Gathered {
        // data reference 0 is this file
        let source = match (place.data_reference, place.method, idat) {
            (0, 0, _) => file,
            (0, 1, Some(idat)) => idat,
            _ => return Gathered::Unreadable,
        };
        let mut out = Vec::new();
        let mut at = place.extents_at;
        for _ in 0..place.extents {
            let Some((piece, next)) = self.extent(source, place.base, at) else { return Gathered::Unreadable };
            at = next;
            if out.len().saturating_add(piece.len()) > limit {
                return Gathered::TooLarge;
            }
            out.extend_from_slice(piece);
        }
        Gathered::Bytes(out)
    }

    /// The extent at `at`: its bytes in `source` (a length of 0 runs to the end) and where the
    /// next extent starts.
    fn extent<'s>(&self, source: &'s [u8], base: u64, at: usize) -> Option<(&'s [u8], usize)> {
        let at = at.checked_add(self.index_size)?;
        let offset = uint(self.data, at, self.offset_size)?;
        let at = at.checked_add(self.offset_size)?;
        let length = uint(self.data, at, self.length_size)?;
        let at = at.checked_add(self.length_size)?;
        let rest = source.get(usize::try_from(base.checked_add(offset)?).ok()?..)?;
        let piece = if length == 0 { rest } else { rest.get(..usize::try_from(length).ok()?)? };
        Some((piece, at))
    }
}

#[cfg(test)]
thread_local! {
    /// `iloc` entries walked on this thread, for the tests that bound the work.
    static ILOC_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn iloc_entry_visited() {
    #[cfg(test)]
    ILOC_ENTRIES.with(|n| n.set(n.get().saturating_add(1)));
}

/// A big-endian unsigned field of `size` bytes (0 reads as 0).
fn uint(b: &[u8], at: usize, size: usize) -> Option<u64> {
    match size {
        0 => Some(0),
        2 => be16(b, at).map(u64::from),
        4 => be32(b, at).map(u64::from),
        8 => be64(b, at),
        _ => None,
    }
}

/// The TIFF stream of an `Exif` item: past its 4-byte offset field and the bytes that offset
/// skips. An item whose offset is wrong, or that has no offset field, is read too when an
/// `Exif\0\0` prefix or the TIFF header itself shows where the stream starts.
fn exif_tiff(item: &[u8]) -> Option<&[u8]> {
    let tiff = |b: &&[u8]| b.starts_with(b"II*\0") || b.starts_with(b"MM\0*");
    let stated = be32(item, 0).and_then(|n| usize::try_from(n).ok()?.checked_add(4)).and_then(|at| item.get(at..));
    let after_field = item.get(4..).map(crate::strip_exif_header);
    [stated, after_field, Some(crate::strip_exif_header(item))].into_iter().flatten().find(tiff)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};

    /// One item's bytes, as `embedded` gathers them: its place from `iloc`, then its extents.
    fn item_bytes(file: &[u8], iloc: &[u8], idat: Option<&[u8]>, id: u32) -> Option<Vec<u8>> {
        let iloc = Iloc::parse(iloc)?;
        let mut place = None;
        iloc.walk(|i, p| {
            if i == id {
                place = Some(p);
            }
            place.is_none()
        })?;
        match iloc.gather(file, idat, &place?, MAX_ITEM) {
            Gathered::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The 100-byte EXIF of the issue sweep's `synthetic-metadata.heic` (#693): Make "Synthetic",
    /// Model "Issue sweep camera", DateTime 2026:10:12 12:34:56, big-endian.
    const SWEEP_TIFF: [u8; 100] = [
        0x4d, 0x4d, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x01, 0x0f, 0x00, 0x02, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, //
        0x00, 0x32, 0x01, 0x10, 0x00, 0x02, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, 0x3c, 0x01, 0x32, 0x00, 0x02, 0x00, 0x00, //
        0x00, 0x14, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x53, 0x79, 0x6e, 0x74, 0x68, 0x65, 0x74, 0x69, 0x63, 0x00, //
        0x49, 0x73, 0x73, 0x75, 0x65, 0x20, 0x73, 0x77, 0x65, 0x65, 0x70, 0x20, 0x63, 0x61, 0x6d, 0x65, 0x72, 0x61, 0x00, 0x00, //
        0x32, 0x30, 0x32, 0x36, 0x3a, 0x31, 0x30, 0x3a, 0x31, 0x32, 0x20, 0x31, 0x32, 0x3a, 0x33, 0x34, 0x3a, 0x35, 0x36, 0x00,
    ];

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn full(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
        bx(kind, &[&[version, 0, 0, 0][..], payload].concat())
    }

    fn put(v: &mut Vec<u8>, x: u64, size: u8) {
        match size {
            0 => assert_eq!(x, 0, "a value needs a field"),
            2 => v.extend_from_slice(&(x as u16).to_be_bytes()),
            4 => v.extend_from_slice(&(x as u32).to_be_bytes()),
            8 => v.extend_from_slice(&x.to_be_bytes()),
            _ => panic!("no {size}-byte fields"),
        }
    }

    /// An EXIF item as HEIF stores it: the 4-byte offset, `skip` (which the offset jumps), the TIFF.
    fn exif_item(skip: &[u8], tiff: &[u8]) -> Vec<u8> {
        [&(skip.len() as u32).to_be_bytes()[..], skip, tiff].concat()
    }

    fn tiff_with(model: &str) -> Vec<u8> {
        crate::write_exif(&crate::Metadata { model: Some(model.into()), ..Default::default() })
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Store {
        File,
        Idat,
    }

    struct Item {
        id: u32,
        kind: &'static [u8; 4],
        /// `mime` items: content type and content encoding.
        mime: (&'static str, &'static str),
        data: Vec<u8>,
        store: Store,
        /// Extents the data is split into.
        extents: usize,
    }

    /// A HEIF file for the tests: `ftyp`, `meta` (`hdlr`, `pitm`, `iinf`, `iref`, `iloc`, `idat`)
    /// and `mdat`.
    struct Heif {
        ftyp: Vec<u8>,
        primary: u32,
        items: Vec<Item>,
        /// `iref` references: type, from, to.
        refs: Vec<(&'static [u8; 4], u32, Vec<u32>)>,
        /// `iloc` version and its offset, length, base-offset and index field sizes.
        iloc: (u8, [u8; 4]),
        /// 32-bit item ids: `infe` version 3, `pitm` and `iref` version 1 (`iloc` must be version 2).
        wide: bool,
    }

    impl Heif {
        /// A primary HEVC picture, item 1 (its bytes don't matter here).
        fn new() -> Heif {
            let picture = Item { id: 1, kind: b"hvc1", mime: ("", ""), data: b"coded picture".to_vec(), store: Store::File, extents: 1 };
            Heif { ftyp: b"heic\0\0\0\0mif1heic".to_vec(), primary: 1, items: vec![picture], refs: Vec::new(), iloc: (1, [4, 4, 0, 0]), wide: false }
        }

        fn item(mut self, id: u32, kind: &'static [u8; 4], data: Vec<u8>, store: Store, extents: usize) -> Heif {
            self.items.push(Item { id, kind, mime: ("", ""), data, store, extents });
            self
        }

        fn xmp(mut self, id: u32, mime: (&'static str, &'static str), data: Vec<u8>) -> Heif {
            self.items.push(Item { id, kind: b"mime", mime, data, store: Store::File, extents: 1 });
            self
        }

        fn cdsc(self, from: u32, to: &[u32]) -> Heif {
            self.iref(b"cdsc", from, to)
        }

        fn iref(mut self, kind: &'static [u8; 4], from: u32, to: &[u32]) -> Heif {
            self.refs.push((kind, from, to.to_vec()));
            self
        }

        fn meta(&self, mdat_at: usize) -> Vec<u8> {
            let id = |v: &mut Vec<u8>, id: u32| put(v, u64::from(id), if self.wide { 4 } else { 2 });
            let mut hdlr = vec![0u8; 4];
            hdlr.extend_from_slice(b"pict");
            hdlr.extend_from_slice(&[0; 13]);
            let mut pitm = Vec::new();
            id(&mut pitm, self.primary);
            let mut iinf = Vec::new();
            put(&mut iinf, self.items.len() as u64, if self.wide { 4 } else { 2 });
            for it in &self.items {
                let mut e = Vec::new();
                id(&mut e, it.id);
                e.extend_from_slice(&[0, 0]); // not protected
                e.extend_from_slice(it.kind);
                e.push(0); // no name
                if it.kind == b"mime" {
                    e.extend_from_slice(it.mime.0.as_bytes());
                    e.push(0);
                    e.extend_from_slice(it.mime.1.as_bytes());
                    e.push(0);
                }
                iinf.extend(full(b"infe", if self.wide { 3 } else { 2 }, &e));
            }
            let mut iref = Vec::new();
            for (kind, from, to) in &self.refs {
                let mut r = Vec::new();
                id(&mut r, *from);
                put(&mut r, to.len() as u64, 2);
                to.iter().for_each(|t| id(&mut r, *t));
                iref.extend(bx(kind, &r));
            }
            let (version, [o, l, b, i]) = self.iloc;
            let mut iloc = vec![o << 4 | l, b << 4 | i];
            put(&mut iloc, self.items.len() as u64, if version == 2 { 4 } else { 2 });
            let (mut in_file, mut in_idat) = (mdat_at as u64, 0u64);
            let mut idat = Vec::new();
            for it in &self.items {
                put(&mut iloc, u64::from(it.id), if version == 2 { 4 } else { 2 });
                let at = match it.store {
                    Store::File => &mut in_file,
                    Store::Idat => &mut in_idat,
                };
                if version > 0 {
                    put(&mut iloc, if it.store == Store::Idat { 1 } else { 0 }, 2);
                }
                put(&mut iloc, 0, 2); // this file
                let base = if b > 0 { *at } else { 0 };
                put(&mut iloc, base, b);
                let chunk = it.data.len().div_ceil(it.extents.max(1)).max(1);
                let pieces: Vec<&[u8]> = it.data.chunks(chunk).collect();
                put(&mut iloc, pieces.len() as u64, 2);
                for piece in pieces {
                    if version > 0 {
                        put(&mut iloc, 0, i);
                    }
                    put(&mut iloc, *at - base, o);
                    put(&mut iloc, piece.len() as u64, l);
                    *at += piece.len() as u64;
                }
                if it.store == Store::Idat {
                    idat.extend_from_slice(&it.data);
                }
            }
            let ref_version = if self.wide { 1 } else { 0 };
            let boxes = [
                full(b"hdlr", 0, &hdlr),
                full(b"pitm", ref_version, &pitm),
                full(b"iinf", ref_version, &iinf),
                full(b"iref", ref_version, &iref),
                full(b"iloc", version, &iloc),
                bx(b"idat", &idat),
            ];
            full(b"meta", 0, &boxes.concat())
        }

        fn build(&self) -> Vec<u8> {
            let ftyp = bx(b"ftyp", &self.ftyp);
            let mdat_at = ftyp.len() + self.meta(0).len() + 8;
            let mdat: Vec<u8> = self.items.iter().filter(|i| i.store == Store::File).flat_map(|i| i.data.iter().copied()).collect();
            [ftyp, self.meta(mdat_at), bx(b"mdat", &mdat)].concat()
        }
    }

    /// The issue sweep's case (#693), laid out as the repo's HEIF test builder writes it: `iloc`
    /// version 1, the EXIF item (offset 0, then the TIFF) tied to the primary image by `cdsc`.
    #[test]
    fn the_issue_sweep_heic_gives_its_camera_and_capture_time() {
        let f = Heif::new().item(8, b"Exif", exif_item(&[], &SWEEP_TIFF), Store::File, 1).cdsc(8, &[1]).build();
        assert_eq!(crate::embedded(&f).exif.as_deref(), Some(SWEEP_TIFF.as_slice()));
        let m = crate::extract(&f);
        assert_eq!((m.make.as_deref(), m.model.as_deref()), (Some("Synthetic"), Some("Issue sweep camera")));
        assert_eq!(m.capture_time.map(|d| d.to_iso()).as_deref(), Some("2026-10-12T12:34:56"));
        // All Metadata's rows come from the same block
        let rows = crate::file_tag_rows(&f);
        assert!(rows.iter().any(|r| r.name == "Model" && r.value == "Issue sweep camera"), "{rows:?}");
    }

    /// What an iPhone writes: the civil time it was taken at, plus the UTC offset it was taken in.
    fn iphone_exif(date: &str, offset: &str) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii(date.into()));
        exif.set(t::OFFSET_TIME_ORIGINAL, Value::Ascii(offset.into()));
        exif.set(t::ISO_SPEED, Value::Short(vec![64]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
        ifd0.set(t::MODEL, Value::Ascii("iPhone 15 Pro".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    /// Issue #2284: keep the camera's civil time and offset, including an evening shot whose
    /// UTC date falls on the next day. The file contains no decodable picture.
    #[test]
    fn an_iphone_heic_keeps_the_time_the_camera_wrote() {
        for (date, hour, iso) in [("2026:10:18 13:00:00", 13, "2026-10-18T13:00:00-05:00"), ("2026:10:18 20:15:11", 20, "2026-10-18T20:15:11-05:00")]
        {
            let tiff = iphone_exif(date, "-05:00");
            let file = Heif::new().item(2, b"Exif", exif_item(b"Exif\0\0", &tiff), Store::File, 1).cdsc(2, &[1]).build();
            let m = crate::extract(&file);
            let captured = m.capture_time.expect("a capture time");
            assert_eq!(captured.hour, hour);
            assert_eq!(captured.offset_minutes, Some(-5 * 60));
            assert_eq!(captured.to_iso(), iso);
            assert_eq!(m.model.as_deref(), Some("iPhone 15 Pro"));
            assert_eq!(m.iso, Some(64));
        }
    }

    /// The repository's HEIF writer supplies a second container layout for the same EXIF.
    #[test]
    fn a_synthetic_heif_file_carries_its_exif() {
        let tiff = iphone_exif("2026:10:18 13:00:00", "-05:00");
        let mut spec = lightcraft_heif::testdata::Spec::new(64, 64, &|x, _| [x as u16 * 4, 0, 0]);
        spec.exif = Some(tiff.clone());
        let file = lightcraft_heif::testdata::build(&spec);
        assert!(is_heif(&file));
        assert_eq!(crate::embedded(&file).exif.as_deref(), Some(tiff.as_slice()));
        assert_eq!(crate::extract(&file).capture_time.map(|d| d.to_iso()).as_deref(), Some("2026-10-18T13:00:00-05:00"));
    }

    /// GPS comes with the EXIF, so a HEIC's location is seen (export's "remove location" refuses to
    /// copy such an original out instead of letting the location through unnoticed).
    #[test]
    fn a_heic_location_is_seen() {
        let gps = crate::Gps { latitude: 38.7, longitude: -9.1, altitude: None };
        let tiff = crate::write_exif(&crate::Metadata { gps: Some(gps), ..Default::default() });
        let f = Heif::new().item(2, b"Exif", exif_item(b"Exif\0\0", &tiff), Store::File, 1).cdsc(2, &[1]).build();
        let back = crate::extract(&f).gps.expect("the location");
        assert!((back.latitude - 38.7).abs() < 1e-6 && (back.longitude + 9.1).abs() < 1e-6, "{back:?}");
    }

    /// The 4-byte offset in front of the TIFF stream: iPhones write 6 and `Exif\0\0`, libheif and
    /// avif-serialize 0. Writers that left the field out, or got it wrong, are read when the stream
    /// is plain to see; anything else is no EXIF.
    #[test]
    fn exif_items_with_and_without_their_offset() {
        let t = &SWEEP_TIFF[..];
        let tiff = Some(t);
        assert_eq!(exif_tiff(&exif_item(b"Exif\0\0", t)), tiff, "iPhone");
        assert_eq!(exif_tiff(&exif_item(&[], t)), tiff, "libheif, avif-serialize");
        assert_eq!(exif_tiff(&exif_item(b"\0\0padding", t)), tiff, "any bytes the offset skips");
        assert_eq!(exif_tiff(&[b"\0\0\0\0Exif\0\0".as_slice(), t].concat()), tiff, "offset 0 though a prefix follows");
        assert_eq!(exif_tiff(&[&1000u32.to_be_bytes()[..], t].concat()), tiff, "offset past the end");
        assert_eq!(exif_tiff(&[b"Exif\0\0".as_slice(), t].concat()), tiff, "no offset field, JPEG style");
        assert_eq!(exif_tiff(t), tiff, "no offset field at all");
        assert_eq!(exif_tiff(&exif_item(&[], b"not a TIFF stream")), None);
        assert_eq!(exif_tiff(&[0, 0]), None);
        assert_eq!(exif_tiff(&[]), None);
        // and through a whole file, the iPhone way
        let f = Heif::new().item(2, b"Exif", exif_item(b"Exif\0\0", t), Store::File, 1).cdsc(2, &[1]).build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Issue sweep camera"));
    }

    /// Items in several extents, in the file or the `meta` box's `idat`, with every `iloc` version
    /// and field size.
    #[test]
    fn item_extents_and_construction_methods() {
        let xmp = crate::write_xmp(&crate::Metadata { title: Some("Harbour at dusk".into()), rating: Some(3), ..Default::default() }, None);
        let rdf = ("application/rdf+xml", "");
        // EXIF in `idat` (method 1), XMP in the file in three extents, a 32-bit base offset
        let mut h = Heif::new().item(2, b"Exif", exif_item(&[], &SWEEP_TIFF), Store::Idat, 1).cdsc(2, &[1]).xmp(3, rdf, xmp.clone().into_bytes());
        h.items[2].extents = 3;
        h.iloc = (1, [8, 4, 4, 0]);
        let f = h.cdsc(3, &[1]).build();
        let e = crate::embedded(&f);
        assert_eq!(e.exif.as_deref(), Some(SWEEP_TIFF.as_slice()));
        assert_eq!(e.xmp.as_deref(), Some(xmp.as_str()));
        let m = crate::extract(&f);
        assert_eq!((m.model.as_deref(), m.title.as_deref(), m.rating), (Some("Issue sweep camera"), Some("Harbour at dusk"), Some(3)));
        // version 2: 32-bit item ids, an extent index, 64-bit lengths; EXIF in two extents
        let mut h = Heif::new().item(70_000, b"Exif", exif_item(b"Exif\0\0", &SWEEP_TIFF), Store::File, 2).cdsc(70_000, &[70_001]);
        (h.primary, h.items[0].id, h.wide, h.iloc) = (70_001, 70_001, true, (2, [4, 8, 0, 4]));
        assert_eq!(crate::embedded(&h.build()).exif.as_deref(), Some(SWEEP_TIFF.as_slice()));
        // version 0: no construction method, file offsets only
        let mut h = Heif::new().item(2, b"Exif", exif_item(&[], &SWEEP_TIFF), Store::File, 1).cdsc(2, &[1]);
        h.iloc = (0, [4, 4, 0, 0]);
        assert_eq!(crate::embedded(&h.build()).exif.as_deref(), Some(SWEEP_TIFF.as_slice()));
        // an XMP item in `idat`
        let mut h = Heif::new().xmp(5, rdf, xmp.clone().into_bytes());
        h.items[1].store = Store::Idat;
        assert_eq!(crate::embedded(&h.build()).xmp.as_deref(), Some(xmp.as_str()));
    }

    /// `iloc` entries the builder doesn't write: a length of 0 (to the end of the data), an item
    /// in another file, an item built from other items, unknown versions and field sizes.
    #[test]
    fn iloc_entries_that_are_read_or_refused() {
        let file = b"0123456789".as_slice();
        let idat = b"abcdefgh".as_slice();
        // version 1, offset and length 4 bytes, no base or index; one item with one extent
        let iloc = |version: u8, sizes: [u8; 2], method: u16, data_ref: u16, offset: u32, length: u32| {
            let mut l = vec![version, 0, 0, 0];
            l.extend_from_slice(&sizes);
            l.extend_from_slice(&1u16.to_be_bytes());
            l.extend_from_slice(&7u16.to_be_bytes());
            if version > 0 {
                l.extend_from_slice(&method.to_be_bytes());
            }
            l.extend_from_slice(&data_ref.to_be_bytes());
            l.extend_from_slice(&1u16.to_be_bytes());
            l.extend_from_slice(&offset.to_be_bytes());
            l.extend_from_slice(&length.to_be_bytes());
            l
        };
        let read = |l: &[u8]| item_bytes(file, l, Some(idat), 7);
        assert_eq!(read(&iloc(1, [0x44, 0], 0, 0, 2, 3)).as_deref(), Some(b"234".as_slice()));
        assert_eq!(read(&iloc(1, [0x44, 0], 1, 0, 2, 3)).as_deref(), Some(b"cde".as_slice()), "from idat");
        assert_eq!(read(&iloc(1, [0x44, 0], 0, 0, 6, 0)).as_deref(), Some(b"6789".as_slice()), "length 0: to the end of the file");
        assert_eq!(read(&iloc(1, [0x44, 0], 1, 0, 5, 0)).as_deref(), Some(b"fgh".as_slice()), "length 0: to the end of idat");
        assert_eq!(item_bytes(file, &iloc(1, [0x44, 0], 1, 0, 0, 1), None, 7), None, "method 1 without idat");
        assert_eq!(read(&iloc(1, [0x44, 0], 2, 0, 0, 1)), None, "method 2 (from other items)");
        assert_eq!(read(&iloc(1, [0x44, 0], 0, 1, 0, 1)), None, "in another file");
        assert_eq!(read(&iloc(1, [0x44, 0], 0, 0, 8, 3)), None, "past the end");
        assert_eq!(read(&iloc(1, [0x44, 0], 0, 0, 11, 0)), None, "starts past the end");
        assert_eq!(read(&iloc(1, [0x22, 0], 0, 0, 0, 1)), None, "2-byte fields are not in the standard");
        assert_eq!(read(&iloc(3, [0x44, 0], 0, 0, 0, 1)), None, "unknown version");
        assert_eq!(item_bytes(file, &iloc(1, [0x44, 0], 0, 0, 0, 1), None, 8), None, "no such item");
        // a base offset near u64::MAX plus an extent offset overflows: refused, not wrapped
        let mut l = vec![1, 0, 0, 0, 0x44, 0x80];
        l.extend_from_slice(&1u16.to_be_bytes());
        l.extend_from_slice(&7u16.to_be_bytes());
        l.extend_from_slice(&[0, 0, 0, 0]); // method 0, data reference 0
        l.extend_from_slice(&u64::MAX.to_be_bytes());
        l.extend_from_slice(&1u16.to_be_bytes());
        l.extend_from_slice(&1u32.to_be_bytes());
        l.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(read(&l), None);
    }

    /// The primary image's metadata is read; metadata tied only to another image is not.
    #[test]
    fn the_primary_images_metadata_is_read() {
        let thumb = |h: Heif| h.item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1);
        // a thumbnail's EXIF comes first in the file; the primary image's is the one read
        let f = thumb(Heif::new())
            .item(10, b"Exif", exif_item(&[], &tiff_with("Thumbnail")), Store::File, 1)
            .item(11, b"Exif", exif_item(&[], &tiff_with("Primary")), Store::File, 1)
            .cdsc(10, &[2])
            .cdsc(11, &[1])
            .build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Primary"));
        // EXIF tied only to another image is not this photo's
        let f = thumb(Heif::new()).item(10, b"Exif", exif_item(&[], &tiff_with("Thumbnail")), Store::File, 1).cdsc(10, &[2]).build();
        assert_eq!(crate::embedded(&f).exif, None);
        // tied to no image: it describes the file; tied to both: read
        let f = Heif::new().item(10, b"Exif", exif_item(&[], &tiff_with("Whole file")), Store::File, 1).build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Whole file"));
        let f = thumb(Heif::new()).item(10, b"Exif", exif_item(&[], &tiff_with("Both")), Store::File, 1).cdsc(10, &[2, 1]).build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Both"));
        // a primary image's EXIF that can't be read gives way to one tied to no image
        let f = Heif::new()
            .item(10, b"Exif", exif_item(&[], b"damaged"), Store::File, 1)
            .item(11, b"Exif", exif_item(&[], &tiff_with("Fallback")), Store::File, 1)
            .cdsc(10, &[1])
            .build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Fallback"));
        // XMP: only uncompressed `application/rdf+xml` items, the primary image's first
        let xmp = |title: &str| crate::write_xmp(&crate::Metadata { title: Some(title.into()), ..Default::default() }, None).into_bytes();
        let f = thumb(Heif::new())
            .xmp(20, ("image/jpeg", ""), b"\xff\xd8 not XMP".to_vec())
            .xmp(21, ("application/rdf+xml", "deflate"), b"compressed".to_vec())
            .xmp(22, ("application/rdf+xml", ""), xmp("Thumbnail"))
            .xmp(23, (" Application/RDF+XML ", ""), xmp("Primary"))
            .cdsc(22, &[2])
            .cdsc(23, &[1])
            .build();
        assert_eq!(crate::extract(&f).title.as_deref(), Some("Primary"));
        // 32-bit ids in `pitm`, `iinf` and `iref`
        let mut h = Heif::new().item(70_000, b"Exif", exif_item(&[], &tiff_with("Wide")), Store::File, 1).cdsc(70_000, &[70_001]);
        (h.primary, h.items[0].id, h.wide, h.iloc.0) = (70_001, 70_001, true, 2);
        assert_eq!(crate::extract(&h.build()).model.as_deref(), Some("Wide"));
    }

    /// HEIF, HEIC and AVIF brands are read; other files built of the same boxes are not HEIF.
    #[test]
    fn heif_and_avif_brands() {
        let with = |ftyp: &[u8]| {
            let mut h = Heif::new().item(2, b"Exif", exif_item(&[], &SWEEP_TIFF), Store::File, 1).cdsc(2, &[1]);
            h.ftyp = ftyp.to_vec();
            h.build()
        };
        for ftyp in [&b"heic\0\0\0\0mif1heic"[..], b"mif1\0\0\0\0mif1", b"heix\0\0\0\0mif1heix", b"avif\0\0\0\0avifmif1miaf", b"msf1\0\0\0\0msf1hevc"]
        {
            assert!(is_heif(&with(ftyp)), "{}", String::from_utf8_lossy(ftyp));
            assert_eq!(crate::extract(&with(ftyp)).model.as_deref(), Some("Issue sweep camera"), "{}", String::from_utf8_lossy(ftyp));
        }
        // MP4 and QuickTime video, JPEG XL, Canon CR3
        for ftyp in [&b"isom\0\0\x02\0isomiso2mp41"[..], b"qt  \0\0\0\0qt  ", b"jxl \0\0\0\0jxl ", b"crx \0\0\0\x01crx isom", b"mif1\0\0\0\0crx "] {
            assert!(!is_heif(&with(ftyp)), "{}", String::from_utf8_lossy(ftyp));
            assert_eq!(crate::extract(&with(ftyp)).model, None);
        }
        assert!(!is_heif(b"\0\0\0\x0cftyp"), "no brand");
        assert!(!is_heif(&[0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1]), "JPEG");
        // a HEIF file without a `meta` box has no metadata
        assert_eq!(crate::embedded(&bx(b"ftyp", b"heic\0\0\0\0mif1heic")), Embedded::default());
    }

    /// Damaged structure: an out-of-range box, an `iinf` or `iref` that claims more than it holds,
    /// extents that overlap to gather more than [`MAX_ITEM`], an item count far past the box.
    #[test]
    fn malformed_bounds_are_refused() {
        let good = Heif::new().item(2, b"Exif", exif_item(&[], &SWEEP_TIFF), Store::File, 1).cdsc(2, &[1]).build();
        // the `meta` box says it is longer than the file: nothing is read
        let mut f = good.clone();
        let at = u32::from_be_bytes(f[0..4].try_into().unwrap()) as usize; // `meta` follows `ftyp`
        assert_eq!(&f[at + 4..at + 8], b"meta");
        f[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(crate::embedded(&f), Embedded::default());
        // an extent past the end of the file
        let mut f = good.clone();
        f.truncate(f.len() - 1);
        assert_eq!(crate::embedded(&f).exif, None);
        // 65,535 extents over the same 64 KiB: refused at 16 MiB, quickly
        let file = vec![0u8; 64 << 10];
        let mut l = vec![1, 0, 0, 0, 0x44, 0];
        l.extend_from_slice(&1u16.to_be_bytes());
        l.extend_from_slice(&7u16.to_be_bytes());
        l.extend_from_slice(&[0, 0, 0, 0]);
        l.extend_from_slice(&u16::MAX.to_be_bytes());
        for _ in 0..u16::MAX {
            l.extend_from_slice(&0u32.to_be_bytes());
            l.extend_from_slice(&(file.len() as u32).to_be_bytes());
        }
        assert_eq!(item_bytes(&file, &l, None, 7), None);
        // an item count of 2^32 - 1 in a box holding one entry
        let mut l = vec![2, 0, 0, 0, 0x44, 0];
        l.extend_from_slice(&u32::MAX.to_be_bytes());
        l.extend_from_slice(&1u32.to_be_bytes());
        l.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        assert_eq!(item_bytes(&file, &l, None, 7), None);
        // a `cdsc` claiming 65,535 targets with none there, an `infe` cut short
        let mut iref = vec![0, 0, 0, 0];
        iref.extend(bx(b"cdsc", &[0, 2, 0xff, 0xff]));
        let iref = bx(b"iref", &iref);
        let mut items = vec![Candidate { id: 2, kind: Kind::Exif, describes: false, describes_primary: false }];
        let mut budget = MAX_BOXES;
        let b = boxes(&iref, 0, iref.len(), &mut budget);
        link(&iref, &b[0], Some(1), &mut items, &mut budget);
        assert!(!items[0].describes);
        assert_eq!(item_info(&[2, 0, 0, 0, 0, 2, 0, 0, b'E', b'x']), None, "cut short: the item type is unknown");
        assert_eq!(item_info(&[4, 0, 0, 0, 0, 2, 0, 0, b'E', b'x', b'i', b'f', 0]), None, "unknown infe version");
        assert_eq!(item_info(&[2, 0, 0, 0, 0, 2, 0, 1, b'E', b'x', b'i', b'f', 0]), Some((2, None)), "protected");
        // a full box's version is its own first byte, never the next box's
        let two = [bx(b"iref", &[]), bx(b"free", &[1, 2, 3])].concat();
        let mut budget = MAX_BOXES;
        let b = boxes(&two, 0, two.len(), &mut budget);
        assert_eq!((version(&two, &b[0]), version(&two, &b[1])), (None, Some(1)));
    }

    /// Every cut-short or damaged file is read without a panic.
    #[test]
    fn truncated_and_damaged_files_never_panic() {
        let xmp = crate::write_xmp(&crate::Metadata { title: Some("t".into()), ..Default::default() }, None).into_bytes();
        let mut h = Heif::new()
            .item(2, b"Exif", exif_item(b"Exif\0\0", &SWEEP_TIFF), Store::Idat, 2)
            .xmp(3, ("application/rdf+xml", ""), xmp)
            .cdsc(2, &[1])
            .cdsc(3, &[1]);
        h.items[2].extents = 3;
        h.iloc = (1, [4, 8, 4, 4]);
        let f = h.build();
        assert!(crate::embedded(&f).exif.is_some() && crate::embedded(&f).xmp.is_some());
        for n in 0..f.len() {
            let _ = crate::extract(&f[..n]);
        }
        for i in 0..f.len() {
            for v in [0x00, 0xff, 0x7f, 0x01] {
                let mut g = f.clone();
                g[i] = v;
                let _ = crate::extract(&g);
                let _ = crate::file_tag_rows(&g);
            }
        }
    }

    fn tiff_at(latitude: f64) -> Vec<u8> {
        crate::write_exif(&crate::Metadata { gps: Some(crate::Gps { latitude, longitude: 2.0, altitude: None }), ..Default::default() })
    }

    /// Export's "remove location" check sees GPS anywhere in a HEIF's metadata, not only in the
    /// photo's own first EXIF (#693 review): a thumbnail's EXIF, the primary image's second EXIF
    /// item, an XMP item. What it can't read counts as a location, so the original isn't copied
    /// out on a guess.
    #[test]
    fn a_location_anywhere_in_the_metadata_is_seen() {
        let plain = tiff_with("Primary");
        let thumb = |h: Heif| h.item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1);
        // only the thumbnail's EXIF has GPS
        let f = thumb(Heif::new())
            .item(10, b"Exif", exif_item(&[], &plain), Store::File, 1)
            .item(11, b"Exif", exif_item(&[], &tiff_at(1.0)), Store::File, 1)
            .cdsc(10, &[1])
            .cdsc(11, &[2])
            .build();
        assert_eq!(crate::extract(&f).gps, None, "the photo's own EXIF has none");
        assert!(crate::embeds_location(&f), "thumbnail");
        // the primary image's second EXIF item has it
        let f = Heif::new()
            .item(10, b"Exif", exif_item(&[], &plain), Store::File, 1)
            .item(11, b"Exif", exif_item(&[], &tiff_at(1.0)), Store::File, 1)
            .cdsc(10, &[1])
            .cdsc(11, &[1])
            .build();
        assert_eq!(crate::extract(&f).gps, None);
        assert!(crate::embeds_location(&f), "second EXIF item");
        // a thumbnail's XMP has it
        let rdf = ("application/rdf+xml", "");
        let xmp = crate::write_xmp(
            &crate::Metadata { gps: Some(crate::Gps { latitude: 1.0, longitude: 2.0, altitude: None }), ..Default::default() },
            None,
        );
        let f = thumb(Heif::new())
            .item(10, b"Exif", exif_item(&[], &plain), Store::File, 1)
            .cdsc(10, &[1])
            .xmp(20, rdf, xmp.into_bytes())
            .cdsc(20, &[2])
            .build();
        assert!(crate::embeds_location(&f), "thumbnail XMP");
        // no location anywhere
        let f = thumb(Heif::new())
            .item(10, b"Exif", exif_item(&[], &plain), Store::File, 1)
            .item(11, b"Exif", exif_item(&[], &tiff_with("Thumbnail")), Store::File, 1)
            .cdsc(10, &[1])
            .cdsc(11, &[2])
            .build();
        assert!(!crate::embeds_location(&f));
        // unsure counts as yes: a compressed XMP item, an EXIF item cut off by the end of the file,
        // one larger than the reader takes, more items than it looks at
        let f = Heif::new().xmp(20, ("application/rdf+xml", "deflate"), b"compressed".to_vec()).build();
        assert!(crate::embeds_location(&f), "compressed XMP");
        let mut f = Heif::new().item(10, b"Exif", exif_item(&[], &plain), Store::File, 1).build();
        f.truncate(f.len() - 1);
        assert!(crate::embeds_location(&f), "cut off");
        let f = Heif::new().item(10, b"Exif", exif_item(&[], &vec![0; MAX_ITEM]), Store::File, 1).build();
        assert!(crate::embeds_location(&f), "too large");
        let mut h = Heif::new();
        for id in 0..2000 {
            h = h.item(10 + id, b"Exif", exif_item(&[], &plain), Store::File, 1);
        }
        assert!(crate::embeds_location(&h.build()), "too many items");
        // other files: what `extract` reads
        assert!(crate::embeds_location(&tiff_at(1.0)));
        assert!(!crate::embeds_location(&plain));
    }

    /// Replace only the catalog; the real metadata remains in `idat` with unchanged `iloc`.
    fn with_iinf(h: &Heif, iinf: Option<&[u8]>) -> Vec<u8> {
        assert!(h.items.iter().filter(|it| it.kind == b"Exif" || it.kind == b"mime").all(|it| it.store == Store::Idat));
        let meta = h.meta(0);
        let mut budget = MAX_BOXES;
        let children: Vec<u8> = boxes(&meta, 12, meta.len(), &mut budget)
            .iter()
            .flat_map(|b| if &b.kind == b"iinf" { iinf.map(|data| bx(b"iinf", data)).unwrap_or_default() } else { bx(&b.kind, &meta[b.body..b.end]) })
            .collect();
        [bx(b"ftyp", &h.ftyp), full(b"meta", 0, &children)].concat()
    }

    fn catalog(version: u8, count: u32, entries: &[u8]) -> Vec<u8> {
        let mut data = vec![version, 0, 0, 0];
        put(&mut data, u64::from(count), if version == 0 { 2 } else { 4 });
        data.extend_from_slice(entries);
        data
    }

    fn catalog_entry(version: u8, id: u32, kind: &[u8; 4], fields: &[u8]) -> Vec<u8> {
        let mut data = vec![version, 0, 0, 0];
        put(&mut data, u64::from(id), if version == 3 { 4 } else { 2 });
        data.extend_from_slice(&[0, 0]);
        if version >= 2 {
            data.extend_from_slice(kind);
        }
        data.extend_from_slice(fields);
        data
    }

    #[test]
    fn item_catalog_truncated_type_does_not_hide_exif_gps() {
        let tiff = tiff_at(1.0);
        assert!(crate::try_read_exif(&tiff).unwrap().gps.is_some());
        let h = Heif::new().item(2, b"Exif", exif_item(&[], &tiff), Store::Idat, 1);
        for version in [2, 3] {
            let entry = catalog_entry(version, 2, b"Exif", b"\0");
            let intact = with_iinf(&h, Some(&catalog(0, 1, &bx(b"infe", &entry))));
            assert!(crate::extract(&intact).gps.is_some(), "real EXIF, infe v{version}");
            // Keep the id, protection index and first two type bytes, plus the full GPS payload.
            let cut = &entry[..entry.len() - 3];
            let damaged = with_iinf(&h, Some(&catalog(0, 1, &bx(b"infe", cut))));
            assert_eq!(embeds_location(&damaged), Some(true), "truncated type, infe v{version}");
            for end in 0..entry.len() {
                let damaged = with_iinf(&h, Some(&catalog(0, 1, &bx(b"infe", &entry[..end]))));
                assert_eq!(embeds_location(&damaged), Some(true), "infe v{version}, cut at {end}");
            }
        }
    }

    #[test]
    fn item_catalog_truncated_fields_and_strings_do_not_hide_xmp_gps() {
        let gps = crate::Metadata { gps: Some(crate::Gps { latitude: 1.0, longitude: 2.0, altitude: None }), ..Default::default() };
        let xmp = crate::write_xmp(&gps, None);
        assert!(crate::parse_xmp(&xmp).unwrap().metadata.gps.is_some());
        let mut h = Heif::new().xmp(2, ("application/rdf+xml", ""), xmp.into_bytes());
        h.items[1].store = Store::Idat;
        for version in 0..=3 {
            // The optional content encoding is absent; name and content type are terminated.
            let entry = catalog_entry(version, 2, b"mime", b"name\0application/rdf+xml\0");
            for table_version in [0, 1] {
                let intact = with_iinf(&h, Some(&catalog(table_version, 1, &bx(b"infe", &entry))));
                assert!(crate::extract(&intact).gps.is_some(), "real XMP, infe v{version}, iinf v{table_version}");
                for end in 0..entry.len() {
                    let damaged = with_iinf(&h, Some(&catalog(table_version, 1, &bx(b"infe", &entry[..end]))));
                    assert_eq!(embeds_location(&damaged), Some(true), "infe v{version}, iinf v{table_version}, cut at {end}");
                }
            }
        }
    }

    #[test]
    fn item_catalog_incomplete_walks_counts_and_versions_are_uncertain() {
        let h = Heif::new().item(2, b"Exif", exif_item(&[], &tiff_at(1.0)), Store::Idat, 1);
        let entry = catalog_entry(2, 2, b"Exif", b"\0");
        let intact = catalog(0, 1, &bx(b"infe", &entry));
        assert!(crate::extract(&with_iinf(&h, Some(&intact))).gps.is_some());
        let mut cases = vec![("missing catalog".to_string(), None)];
        for end in 0..intact.len() {
            cases.push((format!("catalog cut at {end}"), Some(intact[..end].to_vec())));
        }
        cases.push(("unsupported table version".into(), Some(catalog(2, 1, &bx(b"infe", &entry)))));
        let mut unsupported = entry.clone();
        unsupported[0] = 4;
        cases.push(("unsupported entry version".into(), Some(catalog(0, 1, &bx(b"infe", &unsupported)))));
        cases.push(("unknown entry box".into(), Some(catalog(0, 1, &bx(b"free", &entry)))));
        // A valid image entry cannot explain a declared but absent second entry.
        let image = bx(b"infe", &catalog_entry(2, 1, b"hvc1", b"\0"));
        for version in [0, 1] {
            for count in [0, 2] {
                cases.push((format!("iinf v{version}, count {count}"), Some(catalog(version, count, &image))));
            }
        }
        for (kind, fields) in [(b"hvc1", b"unterminated".as_slice()), (b"uri ", b"name\0unterminated"), (b"mime", b"name\0image/jpeg\0gzip")] {
            cases.push((format!("unterminated {kind:?} field"), Some(catalog(0, 1, &bx(b"infe", &catalog_entry(2, 2, kind, fields))))));
        }
        for (label, data) in cases {
            assert_eq!(embeds_location(&with_iinf(&h, data.as_deref())), Some(true), "{label}");
        }
    }

    #[test]
    fn item_catalog_uncertainty_keeps_readable_exif() {
        let gps = crate::Metadata { gps: Some(crate::Gps { latitude: 1.0, longitude: 2.0, altitude: None }), ..Default::default() };
        let mut h = Heif::new().item(2, b"Exif", exif_item(&[], &tiff_with("Readable")), Store::Idat, 1).xmp(
            3,
            ("application/rdf+xml", ""),
            crate::write_xmp(&gps, None).into_bytes(),
        );
        h.items[2].store = Store::Idat;
        assert!(crate::extract(&h.build()).gps.is_some());
        let exif = bx(b"infe", &catalog_entry(2, 2, b"Exif", b"\0"));
        let broken = bx(b"infe", &catalog_entry(2, 3, b"mime", b"name\0application/rdf+"));
        for entries in [[exif.clone(), broken.clone()], [broken, exif]] {
            let f = with_iinf(&h, Some(&catalog(0, 2, &entries.concat())));
            assert_eq!(crate::extract(&f).model.as_deref(), Some("Readable"));
            assert_eq!(embeds_location(&f), Some(true), "unreadable XMP after or before readable EXIF");
        }
    }

    #[test]
    fn item_catalog_benign_entries_and_empty_tables_still_allow_copying() {
        let h = Heif::new();
        assert_eq!(embeds_location(&h.build()), Some(false), "image only");
        for table_version in [0, 1] {
            assert_eq!(embeds_location(&with_iinf(&h, Some(&catalog(table_version, 0, &[])))), Some(false), "empty catalog");
            for version in 0..=3 {
                for fields in [b"\0image/jpeg\0".as_slice(), b"name\0image/jpeg\0\0", b"name\0image/jpeg\0gzip\0"] {
                    let entry = catalog_entry(version, 1, b"mime", fields);
                    assert_eq!(embeds_location(&with_iinf(&h, Some(&catalog(table_version, 1, &bx(b"infe", &entry))))), Some(false));
                }
            }
            for kind in [b"hvc1", b"av01", b"zzzz", b"uri "] {
                let fields = if kind == b"uri " { b"name\0https://example.invalid/item\0".as_slice() } else { b"name\0" };
                let entry = catalog_entry(2, 1, kind, fields);
                assert_eq!(embeds_location(&with_iinf(&h, Some(&catalog(table_version, 1, &bx(b"infe", &entry))))), Some(false));
            }
        }
        // Preserve the existing protected-item policy.
        let mut entry = catalog_entry(2, 2, b"Exif", b"\0");
        entry[7] = 1;
        let h = h.item(2, b"Exif", exif_item(&[], &tiff_at(1.0)), Store::Idat, 1);
        assert_eq!(embeds_location(&with_iinf(&h, Some(&catalog(0, 1, &bx(b"infe", &entry))))), Some(false));
        // Complete top-level files without an item meta box have no item catalog to inspect.
        assert_eq!(embeds_location(&bx(b"ftyp", b"msf1\0\0\0\0msf1")), Some(false));
    }

    #[test]
    fn item_catalog_box_budget_and_enclosing_walks_cannot_clear_uncertainty() {
        let h = Heif::new().item(2, b"Exif", exif_item(&[], &tiff_at(1.0)), Store::Idat, 1);
        let f = h.build();
        let ftyp = bx(b"ftyp", &h.ftyp);
        let mut top_cut = f.clone();
        top_cut[ftyp.len()..ftyp.len() + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(embeds_location(&top_cut), Some(true), "meta box cut short");
        let hidden = [ftyp.clone(), bx(b"free", &[]).repeat(MAX_BOXES), h.meta(0)].concat();
        assert_eq!(embeds_location(&hidden), Some(true), "top-level budget hides meta");
        let hidden = [ftyp.clone(), full(b"meta", 0, &[bx(b"free", &[]).repeat(MAX_BOXES), h.meta(0)[12..].to_vec()].concat())].concat();
        assert_eq!(embeds_location(&hidden), Some(true), "meta budget hides catalog");
        let image = bx(b"infe", &catalog_entry(2, 1, b"hvc1", b"\0"));
        let data = catalog(1, MAX_BOXES as u32, &image.repeat(MAX_BOXES));
        assert_eq!(embeds_location(&with_iinf(&h, Some(&data))), Some(true), "catalog budget");
        let hidden = [ftyp, full(b"meta", 0, &[vec![0; 3], h.meta(0)[12..].to_vec()].concat())].concat();
        assert_eq!(embeds_location(&hidden), Some(true), "incomplete meta child walk");
    }

    #[test]
    fn iloc_truncation_after_readable_exif_does_not_hide_xmp_gps() {
        let h = Heif::new().item(2, b"Exif", exif_item(&[], &tiff_with("First")), Store::Idat, 1);
        assert!(!crate::embeds_location(&h.build()), "readable EXIF without GPS is safe to copy");
        let gps = crate::Metadata { gps: Some(crate::Gps { latitude: 1.0, longitude: 2.0, altitude: None }), ..Default::default() };
        let mut h = h.xmp(3, ("application/rdf+xml", ""), crate::write_xmp(&gps, None).into_bytes());
        h.items[2].store = Store::Idat;
        let f = h.build();
        assert!(crate::embeds_location(&f), "the intact XMP has GPS");
        let c = Container::read(&f, MAX_SCANNED).unwrap();
        let iloc = c.iloc.as_ref().unwrap().data;
        let cut = with_iloc(&h, Some(&iloc[..iloc.len() - 1]));
        assert_eq!(crate::extract(&cut).model.as_deref(), Some("First"), "the first metadata item still reads");
        assert!(crate::embeds_location(&cut), "the unresolved XMP must not be skipped");
    }

    /// Replace `iloc` in a builder fixture. Metadata stays in `idat`, so changing the box sizes
    /// cannot change its offsets. The coded picture is not used by these metadata tests.
    fn with_iloc(h: &Heif, iloc: Option<&[u8]>) -> Vec<u8> {
        assert!(h.items.iter().filter(|it| it.kind == b"Exif" || it.kind == b"mime").all(|it| it.store == Store::Idat));
        let meta = h.meta(0);
        let mut budget = MAX_BOXES;
        let children: Vec<u8> = boxes(&meta, 12, meta.len(), &mut budget)
            .iter()
            .flat_map(|b| if &b.kind == b"iloc" { iloc.map(|data| bx(b"iloc", data)).unwrap_or_default() } else { bx(&b.kind, &meta[b.body..b.end]) })
            .collect();
        [bx(b"ftyp", &h.ftyp), full(b"meta", 0, &children)].concat()
    }

    #[test]
    fn iloc_unresolved_metadata_counts_as_location() {
        for kind in [b"Exif", b"mime"] {
            let mut h = Heif::new().item(2, kind, b"unread metadata".to_vec(), Store::Idat, 1);
            h.items[1].mime = ("application/rdf+xml", "");
            let f = h.build();
            let c = Container::read(&f, MAX_SCANNED).unwrap();
            let iloc = c.iloc.as_ref().unwrap().data;
            assert_eq!(embeds_location(&with_iloc(&h, None)), Some(true), "missing iloc: {kind:?}");
            // Every cut leaves the metadata entry unresolved, including cuts in its last extent.
            for end in 0..iloc.len() {
                assert_eq!(embeds_location(&with_iloc(&h, Some(&iloc[..end]))), Some(true), "{kind:?}: iloc cut at {end}");
            }
            // Unsupported version or any one of the four field sizes.
            for (at, value) in [(0, 3), (4, 0x24), (4, 0x42), (5, 0x20), (5, 0x02)] {
                let mut bad = iloc.to_vec();
                bad[at] = value;
                assert_eq!(embeds_location(&with_iloc(&h, Some(&bad))), Some(true), "{kind:?}: field {at} = {value}");
            }
        }
    }

    #[test]
    fn iloc_partial_walk_does_not_clear_unresolved_metadata() {
        let h = Heif::new().item(2, b"Exif", Vec::new(), Store::Idat, 1).item(3, b"Exif", b"unread metadata".to_vec(), Store::Idat, 1);
        let f = h.build();
        let c = Container::read(&f, MAX_SCANNED).unwrap();
        let iloc = c.iloc.as_ref().unwrap().data;
        // Picture: 16 bytes; empty EXIF: 8 bytes; final EXIF: 16 bytes. Keep the first EXIF
        // intact while cutting the second entry at every byte, including its extent fields.
        for end in 32..iloc.len() {
            let cut = with_iloc(&h, Some(&iloc[..end]));
            let partial = Container::read(&cut, MAX_SCANNED).unwrap();
            assert!(matches!(partial.gather(0, MAX_ITEM), Gathered::Bytes(b) if b.is_empty()));
            assert_eq!(embeds_location(&cut), Some(true), "iloc cut at {end}");
        }
    }

    #[test]
    fn iloc_complete_walk_allows_unplaced_and_empty_items() {
        let h = Heif::new().item(2, b"Exif", Vec::new(), Store::Idat, 1);
        assert_eq!(embeds_location(&h.build()), Some(false), "placed EXIF with no extents");
        // A well-formed empty table, or a complete table placing only a different item.
        for version in 0..=2 {
            let mut iloc = vec![version, 0, 0, 0, 0, 0];
            put(&mut iloc, 0, if version == 2 { 4 } else { 2 });
            assert_eq!(embeds_location(&with_iloc(&h, Some(&iloc))), Some(false), "empty iloc v{version}");
            let count_at = iloc.len() - 1;
            iloc[count_at] = 1;
            put(&mut iloc, 9, if version == 2 { 4 } else { 2 });
            if version > 0 {
                put(&mut iloc, 0, 2); // construction method
            }
            iloc.extend_from_slice(&[0, 0, 0, 0]); // this file, no extents
            assert_eq!(embeds_location(&with_iloc(&h, Some(&iloc))), Some(false), "absent item in complete iloc v{version}");
        }
        // No metadata candidates: there is nothing to resolve, even without a usable iloc.
        assert_eq!(embeds_location(&Heif::new().build()), Some(false));
        for iloc in [None, Some([3, 0, 0, 0, 0, 0].as_slice())] {
            ILOC_ENTRIES.with(|n| n.set(0));
            assert_eq!(embeds_location(&with_iloc(&Heif::new(), iloc)), Some(false));
            assert_eq!(ILOC_ENTRIES.with(std::cell::Cell::get), 0);
        }
    }

    #[test]
    fn iloc_stops_after_all_metadata_is_placed() {
        let h = Heif::new().item(2, b"Exif", Vec::new(), Store::Idat, 1).item(3, b"hvc1", vec![1], Store::File, 1);
        let f = h.build();
        let c = Container::read(&f, MAX_SCANNED).unwrap();
        let iloc = c.iloc.as_ref().unwrap().data;
        // The last picture entry is truncated. The only metadata entry is already complete.
        let cut = with_iloc(&h, Some(&iloc[..32]));
        ILOC_ENTRIES.with(|n| n.set(0));
        assert_eq!(embeds_location(&cut), Some(false));
        assert_eq!(ILOC_ENTRIES.with(std::cell::Cell::get), 2, "stop after the picture and the EXIF entry");
    }

    #[test]
    fn iloc_resolved_extents_still_obey_bounds_and_budget() {
        let h = Heif::new().item(2, b"Exif", b"abcd".to_vec(), Store::Idat, 1);
        let f = h.build();
        let c = Container::read(&f, MAX_SCANNED).unwrap();
        assert!(matches!(c.gather(0, 4), Gathered::Bytes(b) if b == b"abcd"));
        assert!(matches!(c.gather(0, 3), Gathered::TooLarge));
        let iloc = c.iloc.as_ref().unwrap().data;
        for (offset, length) in [(0u32, 5u32), (5, 0), (u32::MAX, 1)] {
            let mut bad = iloc.to_vec();
            let end = bad.len();
            bad[end - 8..end - 4].copy_from_slice(&offset.to_be_bytes());
            bad[end - 4..].copy_from_slice(&length.to_be_bytes());
            let f = with_iloc(&h, Some(&bad));
            let c = Container::read(&f, MAX_SCANNED).unwrap();
            assert!(matches!(c.gather(0, MAX_ITEM), Gathered::Unreadable));
            assert_eq!(embeds_location(&f), Some(true));
        }
    }

    /// `iloc` is walked once, however many metadata items there are to place (#693 review: one
    /// walk per item took over a second on a 48 MB `iloc` with 64 items).
    #[test]
    fn iloc_is_walked_once() {
        // 64 metadata items, none placed by `iloc`, and 20,000 entries for other items
        let mut iinf = vec![0u8, 0, 0, 0];
        iinf.extend_from_slice(&64u16.to_be_bytes());
        for id in 0..64u16 {
            let mut e = vec![2u8, 0, 0, 0];
            e.extend_from_slice(&(1000 + id).to_be_bytes());
            e.extend_from_slice(&[0, 0]);
            e.extend_from_slice(if id % 2 == 0 { b"Exif\0" } else { b"mime\0application/rdf+xml\0" });
            iinf.extend(bx(b"infe", &e));
        }
        let entries = 20_000usize;
        let mut iloc = vec![2u8, 0, 0, 0, 0, 0];
        iloc.extend_from_slice(&(entries as u32).to_be_bytes());
        for _ in 0..entries {
            iloc.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]); // item 1, method 0, this file, no extents
        }
        let meta = full(b"meta", 0, &[bx(b"iinf", &iinf), bx(b"iloc", &iloc)].concat());
        let f = [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), meta].concat();
        let walked = |read: &dyn Fn(&[u8])| {
            ILOC_ENTRIES.with(|n| n.set(0));
            read(&f);
            ILOC_ENTRIES.with(std::cell::Cell::get)
        };
        let n = walked(&|f| assert_eq!(crate::embedded(f), Embedded::default()));
        assert!(n <= entries, "embedded walked {n} entries of {entries}");
        let n = walked(&|f| {
            let _ = crate::embeds_location(f);
        });
        assert!(n <= entries, "embeds_location walked {n} entries of {entries}");
    }

    /// EXIF tied to an image the primary is derived from (`dimg`: an `iden` turn, an `iovl` overlay,
    /// a `tmap` tone map, a grid) is the primary image's, through any depth and despite loops. So is
    /// EXIF in a file whose `pitm` names no item. A thumbnail's EXIF still isn't.
    #[test]
    fn metadata_of_the_images_a_primary_is_made_from_is_read() {
        let base = |h: Heif| {
            let mut h = h
                .item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1)
                .item(3, b"hvc1", b"coded picture".to_vec(), Store::File, 1)
                .item(10, b"Exif", exif_item(&[], &tiff_with("Thumbnail")), Store::File, 1)
                .item(11, b"Exif", exif_item(&[], &tiff_with("Input")), Store::File, 1)
                .cdsc(10, &[2])
                .cdsc(11, &[3]);
            h.items[0].kind = b"iden";
            h
        };
        // the primary (1) turns item 3
        let f = base(Heif::new()).iref(b"dimg", 1, &[3]).build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Input"));
        // two levels (a tone map of a grid of 3), and a loop back to the primary
        let f = base(Heif::new())
            .item(4, b"grid", Vec::new(), Store::File, 1)
            .iref(b"dimg", 1, &[4])
            .iref(b"dimg", 4, &[3])
            .iref(b"dimg", 3, &[1])
            .build();
        assert_eq!(crate::extract(&f).model.as_deref(), Some("Input"));
        // `pitm` names an item that doesn't exist: there is no primary image, so tied items count
        let mut h = Heif::new().item(11, b"Exif", exif_item(&[], &tiff_with("Orphan")), Store::File, 1).cdsc(11, &[1]);
        h.primary = 99;
        assert_eq!(crate::extract(&h.build()).model.as_deref(), Some("Orphan"));
        // the thumbnail is not an input of the primary: without the `dimg`, nothing is the photo's
        assert_eq!(crate::extract(&base(Heif::new()).build()).model, None);
    }

    /// Original export checks place names in every metadata item, like GPS coordinates.
    #[test]
    fn place_names_in_a_thumbnail_are_seen_for_original_export() {
        for (metadata, located) in [
            (crate::Metadata { sublocation: Some("Test place".into()), ..Default::default() }, true),
            (crate::Metadata { city: Some("Test city".into()), ..Default::default() }, true),
            (crate::Metadata { state: Some("Test state".into()), ..Default::default() }, true),
            (crate::Metadata { country: Some("Test country".into()), ..Default::default() }, true),
            (crate::Metadata { title: Some("A title without location".into()), ..Default::default() }, false),
        ] {
            let xmp = crate::write_xmp(&metadata, None);
            assert!(crate::parse_xmp(&xmp).is_ok());
            let f = Heif::new()
                .item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1)
                .xmp(3, ("application/rdf+xml", ""), xmp.into_bytes())
                .cdsc(3, &[2])
                .build();
            assert!(crate::embedded(&f).xmp.is_none(), "the thumbnail's packet is not the primary image's");
            assert_eq!(crate::embeds_location(&f), located, "{metadata:?}");
        }
    }

    proptest::proptest! {
        /// Arbitrary bytes behind a HEIF `ftyp` never panic.
        #[test]
        fn arbitrary_boxes_never_panic(body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let f = [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), body.clone()].concat();
            let _ = crate::embedded(&f);
            let meta = [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), full(b"meta", 0, &body)].concat();
            let _ = crate::extract(&meta);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod upstream_tests {
    use super::*;
    // the whole routing, not just this module: a HEIC must reach the HEIF branch of `embedded`
    use crate::embedded;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};

    /// One box: its own size, its four-letter kind, then its payload.
    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    /// A FullBox: a box whose payload begins with a version byte and three flag bytes.
    fn full_box(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
        bx(kind, &[&[version, 0, 0, 0][..], payload].concat())
    }

    struct Item<'a> {
        kind: &'a [u8; 4],
        content_type: &'a str,
        data: Vec<u8>,
    }

    /// A HEIC-shaped file holding `items`: `ftyp`, a `meta` box with one `infe` per item, and the
    /// item bytes in `idat` (`in_idat`, construction method 1) or in `mdat` (method 0).
    fn heic(items: &[Item<'_>], iloc_version: u8, in_idat: bool) -> Vec<u8> {
        let mut iinf = (items.len() as u16).to_be_bytes().to_vec();
        for (i, it) in items.iter().enumerate() {
            let mut e = vec![2u8, 0, 0, 0]; // version 2, flags
            e.extend_from_slice(&(i as u16 + 1).to_be_bytes());
            e.extend_from_slice(&[0, 0]); // protection index
            e.extend_from_slice(it.kind);
            e.extend_from_slice(b"an item\0");
            if it.kind == b"mime" {
                e.extend_from_slice(it.content_type.as_bytes());
                e.push(0);
            }
            iinf.extend_from_slice(&bx(b"infe", &e));
        }
        let iinf = full_box(b"iinf", 0, &iinf);
        let payload: Vec<u8> = items.iter().flat_map(|i| i.data.iter().copied()).collect();
        let idat = if in_idat { bx(b"idat", &payload) } else { Vec::new() };
        let iloc = |base: u32| {
            // four-byte offsets and lengths, no base offset and no extent index
            let mut l = vec![0x44, 0x00];
            match iloc_version {
                2 => l.extend_from_slice(&(items.len() as u32).to_be_bytes()),
                _ => l.extend_from_slice(&(items.len() as u16).to_be_bytes()),
            }
            let mut at = base;
            for (i, it) in items.iter().enumerate() {
                match iloc_version {
                    2 => l.extend_from_slice(&(i as u32 + 1).to_be_bytes()),
                    _ => l.extend_from_slice(&(i as u16 + 1).to_be_bytes()),
                }
                if iloc_version > 0 {
                    l.extend_from_slice(&[0, u8::from(in_idat)]); // construction method
                }
                l.extend_from_slice(&[0, 0]); // data reference index: this file
                l.extend_from_slice(&[0, 1]); // one extent
                l.extend_from_slice(&at.to_be_bytes());
                l.extend_from_slice(&(it.data.len() as u32).to_be_bytes());
                at += it.data.len() as u32;
            }
            full_box(b"iloc", iloc_version, &l)
        };
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        let meta = |base| full_box(b"meta", 0, &[iloc(base), iinf.clone(), idat.clone()].concat());
        if in_idat {
            // the offsets are into `idat`, so they don't depend on where the box lands
            return [ftyp, meta(0)].concat();
        }
        let base = (ftyp.len() + meta(0).len() + 8) as u32;
        [ftyp, meta(base), bx(b"mdat", &payload)].concat()
    }

    /// What an iPhone writes: the civil time it was taken at, plus the UTC offset it was taken in.
    fn iphone_exif(date: &str, offset: &str) -> Vec<u8> {
        let mut exif = IfdBuilder::new();
        exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii(date.into()));
        exif.set(t::OFFSET_TIME_ORIGINAL, Value::Ascii(offset.into()));
        exif.set(t::ISO_SPEED, Value::Short(vec![64]));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
        ifd0.set(t::MODEL, Value::Ascii("iPhone 15 Pro".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap_or_default()
    }

    /// An `Exif` item's payload: the offset from the end of this field to the TIFF header, then
    /// `skip` bytes of padding, then the TIFF stream.
    fn exif_item(tiff: &[u8], skip: usize) -> Vec<u8> {
        [&(skip as u32).to_be_bytes()[..], &vec![0u8; skip], tiff].concat()
    }

    /// One `Exif` item holding `tiff`, the only item in the file.
    fn exif_only(tiff: &[u8]) -> Vec<Item<'static>> {
        vec![Item { kind: b"Exif", content_type: "", data: exif_item(tiff, 0) }]
    }

    /// Issue #2284: a photo taken at 13:00 in UTC-5 must stay 13:00, not become 18:00. Before the
    /// HEIF items were read a HEIC carried no capture time at all, and the import fell back to the
    /// file's modification time, which LightCraft writes as UTC.
    #[test]
    fn an_iphone_heic_keeps_the_time_the_camera_wrote() {
        let file = heic(&exif_only(&iphone_exif("2026:10:18 13:00:00", "-05:00")), 1, false);
        let m = crate::extract(&file);
        let captured = m.capture_time.expect("a capture time");
        assert_eq!(captured.hour, 13);
        assert_eq!(captured.offset_minutes, Some(-5 * 60));
        assert_eq!(captured.to_iso(), "2026-10-18T13:00:00-05:00");
        assert_eq!(m.model.as_deref(), Some("iPhone 15 Pro"));
        assert_eq!(m.iso, Some(64));
    }

    /// The XMP packet is the `mime` item that declares `application/rdf+xml`, not any other one.
    #[test]
    fn the_xmp_packet_is_the_rdf_mime_item() {
        let xmp = crate::write_xmp(&crate::Metadata { title: Some("Cusco".into()), ..Default::default() }, None);
        let items = vec![
            Item { kind: b"mime", content_type: "text/plain", data: b"not the packet".to_vec() },
            Item { kind: b"mime", content_type: "application/rdf+xml", data: xmp.clone().into_bytes() },
            Item { kind: b"Exif", content_type: "", data: exif_item(&iphone_exif("2026:10:18 13:00:00", "-05:00"), 0) },
        ];
        let e = embedded(&heic(&items, 1, false));
        assert_eq!(e.xmp.as_deref(), Some(xmp.as_str()));
        assert!(e.exif.is_some());
        assert_eq!(crate::extract(&heic(&items, 1, false)).title.as_deref(), Some("Cusco"));
    }

    /// Every item location box the format allows: counts and ids are 16-bit before version 2 and
    /// 32-bit in it, and the bytes live in `mdat` or in `idat` next to the location box itself.
    #[test]
    fn items_are_located_in_every_iloc_version() {
        let tiff = iphone_exif("2026:10:18 13:00:00", "-05:00");
        for (version, in_idat) in [(0, false), (1, false), (1, true), (2, false), (2, true)] {
            let file = heic(&exif_only(&tiff), version, in_idat);
            let found = embedded(&file).exif;
            assert_eq!(found.as_deref(), Some(tiff.as_slice()), "iloc v{version}, idat {in_idat}");
        }
    }

    /// The `Exif` item's leading offset may point past padding, not only straight at the TIFF
    /// header, and the reader has to follow it.
    #[test]
    fn the_exif_items_tiff_header_offset_is_honoured() {
        let tiff = iphone_exif("2026:10:18 13:00:00", "-05:00");
        let items = vec![Item { kind: b"Exif", content_type: "", data: exif_item(&tiff, 6) }];
        assert_eq!(embedded(&heic(&items, 1, false)).exif.as_deref(), Some(tiff.as_slice()));
    }

    /// Which `ftyp` brands count as a still image in this container, major or compatible.
    #[test]
    fn brands() {
        for brand in ["heic", "heix", "mif1", "avif", "hevc", "msf1"] {
            let file = bx(b"ftyp", format!("{brand}\0\0\0\0mif1").as_bytes());
            assert!(is_heif(&file), "{brand}");
        }
        // a compatible brand counts too, a CR3, a JPEG XL and a non-ISO file do not
        assert!(is_heif(&bx(b"ftyp", b"mp41\0\0\0\0isomheic")));
        assert!(!is_heif(&bx(b"ftyp", b"crx \0\0\0\0crx isom")));
        assert!(!is_heif(&bx(b"ftyp", b"jxl \0\0\0\0jxl ")));
        assert!(!is_heif(b"\xff\xd8\xff\xe1 a jpeg"));
        assert!(!is_heif(b""));
    }

    /// A truncated or scrambled file must come back empty, never panic (the never-crash rule).
    #[test]
    fn broken_files_yield_nothing_without_panicking() {
        let good = heic(
            &[
                Item { kind: b"Exif", content_type: "", data: exif_item(&iphone_exif("2026:10:18 13:00:00", "-05:00"), 0) },
                Item { kind: b"mime", content_type: "application/rdf+xml", data: b"<x:xmpmeta/>".to_vec() },
            ],
            1,
            false,
        );
        for cut in 0..good.len() {
            let _ = embedded(&good[..cut]);
        }
        for at in 0..good.len() {
            let mut bad = good.clone();
            bad[at] ^= 0xff;
            let _ = embedded(&bad);
        }
        // an item whose extent is outside the file, and a `meta` box that claims more than it has
        let mut lying = good.clone();
        let item_count = u32::MAX.to_be_bytes();
        if let Some(at) = lying.windows(4).position(|w| w == b"iloc") {
            lying.splice(at + 8..at + 12, item_count);
        }
        assert!(embedded(&lying).exif.is_none());
    }

    /// The real container writer of `lightcraft-heif`, so the reader is proved against the layout
    /// a HEIF encoder produces and not only against this module's own test builder.
    #[test]
    fn a_synthetic_heif_file_carries_its_exif() {
        let tiff = iphone_exif("2026:10:18 13:00:00", "-05:00");
        let mut spec = lightcraft_heif::testdata::Spec::new(64, 64, &|x, _| [x as u16 * 4, 0, 0]);
        spec.exif = Some(tiff.clone());
        let file = lightcraft_heif::testdata::build(&spec);
        assert!(is_heif(&file));
        assert_eq!(embedded(&file).exif.as_deref(), Some(tiff.as_slice()));
        assert_eq!(crate::extract(&file).capture_time.map(|d| d.to_iso()).as_deref(), Some("2026-10-18T13:00:00-05:00"));
    }
}
