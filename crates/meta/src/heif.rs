//! HEIF / HEIC / AVIF metadata: the `Exif` and XMP items of the ISO base media file container
//! (box structure from ISO/IEC 14496-12, item model from ISO/IEC 23008-12).
//!
//! HEIC is what iPhones and Macs write. Its pixels need an HEVC decoder (the optional
//! `craft-heif` crate), its metadata does not, so this reader is in every build: a HEIC
//! imports with its capture time, camera and lens whether or not the build can show the picture.
//!
//! The `meta` box holds an item information box (`iinf`, one `infe` per item) naming each item's
//! type, and an item location box (`iloc`) saying where its bytes are: in the file, or in the
//! `idat` box next to it. The `Exif` item is a four-byte offset followed by a TIFF stream; the XMP
//! packet is a `mime` item whose content type is `application/rdf+xml`. The ICC profile is an item
//! *property*, not an item, and is read by the decoder instead.

use crate::Embedded;

const MAX_BOXES: usize = 4096;

#[derive(Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    body: usize,
    end: usize,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at + 8)?;
    Some(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn boxes(bytes: &[u8], mut at: usize, end: usize, budget: &mut usize) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let end = end.min(bytes.len());
    while at + 8 <= end && *budget > 0 {
        *budget -= 1;
        let Some(size32) = be32(bytes, at) else { break };
        let Some(kind_slice) = bytes.get(at + 4..at + 8) else { break };
        let mut kind = [0u8; 4];
        kind.copy_from_slice(kind_slice);
        let (hdr, box_end) = match size32 {
            0 => (8, end),
            1 => {
                let Some(s64) = be64(bytes, at + 8) else { break };
                let Ok(s) = usize::try_from(s64) else { break };
                if s < 16 {
                    break;
                }
                (16, at.saturating_add(s).min(end))
            }
            s if s >= 8 => (8, at.saturating_add(s as usize).min(end)),
            _ => break,
        };
        if at + hdr > box_end {
            break;
        }
        out.push(BoxRef { kind, body: at + hdr, end: box_end });
        if box_end <= at {
            break;
        }
        at = box_end;
    }
    out
}

/// Brands that declare a still image in the ISO base media container.
const IMAGE_BRANDS: [&[u8]; 12] = [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1", b"avif", b"avis"];

/// Longest `ftyp` box this reads brands from.
const MAX_FTYP: usize = 4096;
/// Most items one file's `iinf` / `iloc` describes (an iPhone HEIC has a handful).
const MAX_ITEMS: usize = 1024;
/// Most extents one item's bytes are split into.
const MAX_EXTENTS: u16 = 1024;
/// Largest metadata item this returns; a real photo's Exif and XMP are a few hundred kilobytes.
const MAX_ITEM: usize = 32 << 20;

/// Does this file declare an ISO base media still image (HEIF / HEIC / AVIF)?
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.get(4..8) != Some(b"ftyp".as_slice()) {
        return false;
    }
    let Some(size) = be32(bytes, 0).map(|v| v as usize) else {
        return false;
    };
    let end = size.min(bytes.len()).min(MAX_FTYP);
    let brand = |at: usize| bytes.get(at..at.saturating_add(4)).is_some_and(|b| IMAGE_BRANDS.contains(&b));
    // the major brand, then (after the minor version) the compatible brands
    brand(8) || (16..end).step_by(4).any(brand)
}

/// The Exif and XMP blocks of a HEIF file. Anything malformed is left out rather than reported:
/// a file whose items can't be located simply has no metadata, as before this reader existed.
///
/// A HEIF can hold several XMP items, one per image they describe, each tied to its image by a `cdsc` reference
/// (iPhones write one for the depth map, the mattes and the gain map before the photo's own). The photo's packet is
/// the one that describes the primary item (`pitm`); without such a reference, the first packet that describes no
/// other item, which is every packet of a file without `iref` (issue #4336).
pub fn metadata(bytes: &[u8]) -> Embedded {
    let mut out = Embedded::default();
    let Some(s) = structure(bytes) else { return out };
    let describes = |id: u32| s.refs.iter().filter(move |&&(kind, from, _)| &kind == b"cdsc" && from == id).map(|&(_, _, to)| to);
    // the item of `kind` that describes the photo, by the rule above
    let pick = |want: &dyn Fn(&[u8; 4], &str) -> bool| {
        let candidates = || s.items.iter().filter(|(_, kind, ct)| want(kind, ct)).map(|(id, ..)| *id);
        let of_primary = s.primary.and_then(|p| candidates().find(|&id| describes(id).any(|to| to == p)));
        of_primary.or_else(|| candidates().find(|&id| describes(id).next().is_none()))
    };
    let block = |id: u32| s.locs.iter().find(|l| l.id == id).and_then(|loc| item_bytes(bytes, s.idat, loc));
    if let Some(exif) = pick(&|kind, _| kind == b"Exif").and_then(block) {
        // ISO/IEC 23008-12 A.2.1: a four-byte offset from the end of itself to the TIFF header
        let skip = be32(&exif, 0).and_then(|v| usize::try_from(v).ok()).and_then(|v| v.checked_add(4));
        out.exif = skip.and_then(|at| exif.get(at..)).filter(|b| !b.is_empty()).map(<[u8]>::to_vec);
    }
    if let Some(xmp) = pick(&|kind, ct| kind == b"mime" && ct == "application/rdf+xml").and_then(block) {
        out.xmp = Some(String::from_utf8_lossy(&xmp).trim_end_matches('\0').to_string());
    }
    out
}

/// `auxC` types that mark an auxiliary image as depth: the one iPhones write for Portrait photos (an HEVC
/// auxiliary picture, ISO/IEC 23008-2 SEI `aux_id` 2 = depth), and MPEG's codec-independent one (ISO/IEC 23001-17).
const DEPTH_AUX_TYPES: [&[u8]; 2] = [b"urn:mpeg:hevc:2015:auxid:2", b"urn:mpeg:mpegB:cicp:systems:auxiliary:depth"];
/// Most references, properties and property associations read from one file (an iPhone HEIC has well under 100).
const MAX_ENTRIES: usize = 4096;

/// The parts of a HEIF `meta` box that tie items to each other: what the depth reading needs.
struct Structure<'a> {
    /// The primary item (`pitm`): the photo.
    primary: Option<u32>,
    /// `(reference type, from item, to item)` for the `auxl` and `cdsc` references in `iref`.
    refs: Vec<([u8; 4], u32, u32)>,
    /// 1-based `ipco` indices of the `auxC` properties whose type is depth.
    depth_props: Vec<u16>,
    /// `(item, property index)` from `ipma`.
    assoc: Vec<(u32, u16)>,
    items: Vec<(u32, [u8; 4], String)>,
    locs: Vec<Loc>,
    idat: Option<&'a [u8]>,
}

fn structure(bytes: &[u8]) -> Option<Structure<'_>> {
    let mut budget = MAX_BOXES;
    let meta = boxes(bytes, 0, bytes.len(), &mut budget).into_iter().find(|b| &b.kind == b"meta")?;
    let mut s =
        Structure { primary: None, refs: Vec::new(), depth_props: Vec::new(), assoc: Vec::new(), items: Vec::new(), locs: Vec::new(), idat: None };
    for b in boxes(bytes, meta.body.saturating_add(4), meta.end, &mut budget) {
        match &b.kind {
            b"pitm" => {
                s.primary = match bytes.get(b.body) {
                    Some(0) => be16(bytes, b.body + 4).map(u32::from),
                    Some(1) => be32(bytes, b.body + 4),
                    _ => None,
                }
            }
            b"iref" => s.refs = references(bytes, &b, &mut budget),
            b"iprp" => {
                for p in boxes(bytes, b.body, b.end, &mut budget) {
                    match &p.kind {
                        b"ipco" => s.depth_props = depth_property_indices(bytes, &p, &mut budget),
                        b"ipma" => s.assoc.extend(associations(bytes.get(p.body..p.end).unwrap_or_default())),
                        _ => {}
                    }
                }
            }
            b"iinf" => s.items = item_infos(bytes, &b, &mut budget),
            b"iloc" => s.locs = item_locations(bytes.get(b.body..b.end).unwrap_or_default()),
            b"idat" => s.idat = bytes.get(b.body..b.end),
            _ => {}
        }
    }
    Some(s)
}

/// The primary image's depth map: an item that points at the primary item (`pitm`) through an `auxl` reference and
/// whose associated `auxC` property (`ipma` → `ipco`) names a depth type.
fn depth_item(s: &Structure<'_>) -> Option<u32> {
    let primary = s.primary?;
    s.refs
        .iter()
        .filter(|(kind, _, to)| kind == b"auxl" && *to == primary)
        .map(|&(_, from, _)| from)
        .find(|item| s.assoc.iter().any(|(i, p)| i == item && s.depth_props.contains(p)))
}

/// Does the primary image carry recorded depth ([`depth_item`])? Only boxes are read, no picture is decoded;
/// anything malformed is "no depth".
pub fn has_depth(bytes: &[u8]) -> bool {
    structure(bytes).is_some_and(|s| depth_item(&s).is_some())
}

/// The XMP packet that describes the primary image's depth map: the `application/rdf+xml` item whose `cdsc`
/// reference points at the depth item. iPhones record Portrait settings there, not in the photo's own XMP.
pub fn depth_xmp(bytes: &[u8]) -> Option<String> {
    let s = structure(bytes)?;
    let depth = depth_item(&s)?;
    let describes_depth = |id: u32| s.refs.iter().any(|&(kind, from, to)| &kind == b"cdsc" && from == id && to == depth);
    let (id, ..) = s.items.iter().find(|(id, kind, ct)| kind == b"mime" && ct == "application/rdf+xml" && describes_depth(*id))?;
    let loc = s.locs.iter().find(|l| l.id == *id)?;
    let block = item_bytes(bytes, s.idat, loc)?;
    Some(String::from_utf8_lossy(&block).trim_end_matches('\0').to_string())
}

/// `iref` → `(reference type, from item, to item)` for each `auxl` and `cdsc` reference.
fn references(bytes: &[u8], iref: &BoxRef, budget: &mut usize) -> Vec<([u8; 4], u32, u32)> {
    // FullBox: version 0 has 16-bit item ids, version 1 32-bit
    let wide = bytes.get(iref.body) == Some(&1);
    let id = |at: usize| if wide { be32(bytes, at) } else { be16(bytes, at).map(u32::from) };
    let w = if wide { 4 } else { 2 };
    let mut out = Vec::new();
    for r in boxes(bytes, iref.body.saturating_add(4), iref.end, budget) {
        if !matches!(&r.kind, b"auxl" | b"cdsc") {
            continue;
        }
        let (Some(from), Some(count)) = (id(r.body), be16(bytes, r.body + w)) else { continue };
        let first = r.body + w + 2;
        for i in 0..usize::from(count) {
            let at = first.saturating_add(i.saturating_mul(w));
            if at.saturating_add(w) > r.end || out.len() >= MAX_ENTRIES {
                break;
            }
            if let Some(to) = id(at) {
                out.push((r.kind, from, to));
            }
        }
    }
    out
}

/// `ipco` → the 1-based indices (as `ipma` counts them) of the `auxC` properties whose type is depth.
fn depth_property_indices(bytes: &[u8], ipco: &BoxRef, budget: &mut usize) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, p) in boxes(bytes, ipco.body, ipco.end, budget).iter().enumerate().take(MAX_ENTRIES) {
        if &p.kind != b"auxC" {
            continue;
        }
        // FullBox, then the type as a NUL-terminated string, then codec-specific subtype bytes
        let aux_type = bytes.get(p.body.saturating_add(4)..p.end).unwrap_or_default().split(|&c| c == 0).next().unwrap_or_default();
        if DEPTH_AUX_TYPES.contains(&aux_type) {
            out.extend(u16::try_from(i + 1).ok());
        }
    }
    out
}

/// `ipma` (payload after the box header) → one `(item_ID, property index)` per association, stopping at the first
/// entry that doesn't fit. Index 0 means "no property" and is left out.
fn associations(p: &[u8]) -> Vec<(u32, u16)> {
    let (Some(&version), Some(&flags)) = (p.first(), p.get(3)) else { return Vec::new() };
    let Some(count) = be32(p, 4).and_then(|c| usize::try_from(c).ok()) else { return Vec::new() };
    // version 0 has 16-bit item ids; flag bit 0 makes each association 16 bits (15-bit index) instead of 8 (7-bit)
    let (wide_id, wide_index) = (version >= 1, flags & 1 == 1);
    let mut out = Vec::new();
    let mut at = 8usize;
    for _ in 0..count.min(MAX_ENTRIES) {
        let item = if wide_id { be32(p, at) } else { be16(p, at).map(u32::from) };
        at += if wide_id { 4 } else { 2 };
        let (Some(item), Some(&n)) = (item, p.get(at)) else { break };
        at += 1;
        for _ in 0..n {
            let index = if wide_index { be16(p, at).map(|v| v & 0x7fff) } else { p.get(at).map(|&v| u16::from(v & 0x7f)) };
            at += if wide_index { 2 } else { 1 };
            let Some(index) = index else { return out };
            if index != 0 && out.len() < MAX_ENTRIES {
                out.push((item, index));
            }
        }
    }
    out
}

/// `iinf` → one `(item_ID, item_type, content_type)` per `infe` entry. Entries of version 0 and 1
/// name no item type and are skipped; HEIF files use version 2 or 3.
fn item_infos(bytes: &[u8], iinf: &BoxRef, budget: &mut usize) -> Vec<(u32, [u8; 4], String)> {
    // FullBox version and flags, then a 16- or 32-bit entry count before the `infe` boxes
    let version = *bytes.get(iinf.body).unwrap_or(&0);
    let entries_at = iinf.body.saturating_add(if version == 0 { 6 } else { 8 });
    let mut out = Vec::new();
    for b in boxes(bytes, entries_at, iinf.end, budget) {
        if &b.kind != b"infe" || out.len() >= MAX_ITEMS {
            continue;
        }
        let Some(e) = bytes.get(b.body..b.end) else { continue };
        let (id, kind_at) = match *e.first().unwrap_or(&0) {
            2 => (be16(e, 4).map(u32::from), 8),
            3 => (be32(e, 4), 10),
            _ => continue,
        };
        let (Some(id), Some(kind)) = (id, e.get(kind_at..kind_at + 4)) else { continue };
        let mut kind4 = [0u8; 4];
        kind4.copy_from_slice(kind);
        // the item name, then (for a `mime` item) the content type, both NUL-terminated
        let mut strings = e.get(kind_at + 4..).unwrap_or_default().split(|&c| c == 0).skip(1);
        let content_type = String::from_utf8_lossy(strings.next().unwrap_or_default()).into_owned();
        out.push((id, kind4, content_type));
    }
    out
}

/// Where one item's bytes are.
struct Loc {
    id: u32,
    /// 0 = offsets into the file, 1 = into the `idat` box, 2 = another item (not read).
    construction: u8,
    base: u64,
    /// `(offset, length)` per extent, in order.
    extents: Vec<(u64, u64)>,
}

/// `iloc` (payload after the box header) → one [`Loc`] per entry, stopping at the first
/// unreadable one.
fn item_locations(p: &[u8]) -> Vec<Loc> {
    let version = *p.first().unwrap_or(&0);
    let (Some(sizes), Some(base_index)) = (p.get(4), p.get(5)) else { return Vec::new() };
    let (offset_size, length_size) = ((sizes >> 4) as usize, (sizes & 0xf) as usize);
    let (base_size, index_size) = ((base_index >> 4) as usize, if version > 0 { (base_index & 0xf) as usize } else { 0 });
    let (count, mut at) = match version {
        0 | 1 => (be16(p, 6).map(usize::from), 8),
        2 => (be32(p, 6).and_then(|v| usize::try_from(v).ok()), 10),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for _ in 0..count.unwrap_or(0).min(MAX_ITEMS) {
        let (id, after_id) = match version {
            2 => (be32(p, at), at + 4),
            _ => (be16(p, at).map(u32::from), at + 2),
        };
        // version 1 and 2 put the construction method in the low nibble of a reserved 16-bit field
        let construction = if version == 0 { Some(0) } else { be16(p, after_id).map(|v| (v & 0xf) as u8) };
        let after_method = after_id + if version == 0 { 0 } else { 2 };
        // a data reference index of 0 means this file; an external reference has no bytes here
        let external = be16(p, after_method).is_some_and(|v| v != 0);
        let base = uint(p, after_method + 2, base_size);
        let extent_at = after_method + 2 + base_size;
        let (Some(id), Some(construction), Some(base), Some(extent_count)) = (id, construction, base, be16(p, extent_at)) else {
            break;
        };
        let stride = index_size + offset_size + length_size;
        let mut extents = Vec::new();
        let mut e = extent_at + 2;
        for _ in 0..extent_count.min(MAX_EXTENTS) {
            let (Some(offset), Some(length)) = (uint(p, e + index_size, offset_size), uint(p, e + index_size + offset_size, length_size)) else {
                return out;
            };
            extents.push((offset, length));
            e += stride;
        }
        at = e;
        if !external {
            out.push(Loc { id, construction, base, extents });
        }
    }
    out
}

/// A big-endian unsigned integer of `size` bytes; `size` 0 is the value 0, and the item boxes
/// allow no other width than 4 or 8.
fn uint(b: &[u8], at: usize, size: usize) -> Option<u64> {
    match size {
        0 => Some(0),
        4 => be32(b, at).map(u64::from),
        8 => be64(b, at),
        _ => None,
    }
}

/// An item's bytes, its extents concatenated. `None` when they don't fit the file (or the `idat`
/// box), when the item is built by referencing another one, or when it is implausibly large.
fn item_bytes(bytes: &[u8], idat: Option<&[u8]>, loc: &Loc) -> Option<Vec<u8>> {
    let src = match loc.construction {
        0 => bytes,
        1 => idat?,
        _ => return None,
    };
    let mut out = Vec::new();
    for &(offset, length) in &loc.extents {
        let at = loc.base.checked_add(offset)?;
        let (start, end) = (usize::try_from(at).ok()?, usize::try_from(at.checked_add(length)?).ok()?);
        let extent = src.get(start..end)?;
        if out.len().checked_add(extent.len())? > MAX_ITEM {
            return None;
        }
        out.extend_from_slice(extent);
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    // the whole routing, not just this module: a HEIC must reach the HEIF branch of `embedded`
    use crate::embedded;
    use craft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};

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

    /// The real container writer of `craft-heif`, so the reader is proved against the layout
    /// a HEIF encoder produces and not only against this module's own test builder.
    #[test]
    fn a_synthetic_heif_file_carries_its_exif() {
        let tiff = iphone_exif("2026:10:18 13:00:00", "-05:00");
        let mut spec = craft_heif::testdata::Spec::new(64, 64, &|x, _| [x as u16 * 4, 0, 0]);
        spec.exif = Some(tiff.clone());
        let file = craft_heif::testdata::build(&spec);
        assert!(is_heif(&file));
        assert_eq!(embedded(&file).exif.as_deref(), Some(tiff.as_slice()));
        assert_eq!(crate::extract(&file).capture_time.map(|d| d.to_iso()).as_deref(), Some("2026-10-18T13:00:00-05:00"));
    }

    /// How an iPhone Portrait HEIC ties its depth map to the photo, reduced to the boxes [`has_depth`] reads.
    struct Aux<'a> {
        /// The primary item (`pitm`); `None` leaves the box out.
        primary: Option<u16>,
        /// `(auxiliary item, item it is auxiliary to)` as `auxl` references.
        auxl: &'a [(u16, u16)],
        /// The `auxC` type of each property in `ipco`, in order (`ipma` counts them from 1).
        props: &'a [&'a str],
        /// `(item, property index)` associations in `ipma`.
        assoc: &'a [(u16, u16)],
        /// `ipma` version 1 (32-bit item ids) and flag 1 (16-bit associations), as large files use.
        wide: bool,
    }

    fn aux_heic(a: &Aux<'_>) -> Vec<u8> {
        aux_heic_with_xmp(a, &[])
    }

    /// [`aux_heic`] plus XMP items: `(XMP item, item it describes, packet)`, as `mime` items in `idat` tied to the
    /// item they describe by `cdsc`.
    fn aux_heic_with_xmp(a: &Aux<'_>, xmp: &[(u16, u16, &str)]) -> Vec<u8> {
        let pitm = a.primary.map(|p| full_box(b"pitm", 0, &p.to_be_bytes())).unwrap_or_default();
        let reference = |kind: &[u8; 4], from: u16, to: u16| bx(kind, &[from.to_be_bytes(), 1u16.to_be_bytes(), to.to_be_bytes()].concat());
        let mut refs: Vec<u8> = a.auxl.iter().flat_map(|&(from, to)| reference(b"auxl", from, to)).collect();
        refs.extend(xmp.iter().flat_map(|&(item, of, _)| reference(b"cdsc", item, of)));
        let iref = full_box(b"iref", 0, &refs);
        // `infe` version 2 entries, and `iloc` version 1 extents into `idat` (construction method 1)
        let mut iinf = (xmp.len() as u16).to_be_bytes().to_vec();
        let mut iloc = vec![0x44, 0x00];
        iloc.extend_from_slice(&(xmp.len() as u16).to_be_bytes());
        let mut idat = Vec::new();
        for &(item, _, packet) in xmp {
            let e = [&[2u8, 0, 0, 0][..], &item.to_be_bytes(), &[0, 0], b"mime", b"XMP\0application/rdf+xml\0"].concat();
            iinf.extend_from_slice(&bx(b"infe", &e));
            iloc.extend_from_slice(&item.to_be_bytes());
            iloc.extend_from_slice(&[0, 1, 0, 0, 0, 1]); // construction method 1, this file, one extent
            iloc.extend_from_slice(&(idat.len() as u32).to_be_bytes());
            iloc.extend_from_slice(&(packet.len() as u32).to_be_bytes());
            idat.extend_from_slice(packet.as_bytes());
        }
        let items = [full_box(b"iinf", 0, &iinf), full_box(b"iloc", 1, &iloc), bx(b"idat", &idat)].concat();
        let ipco: Vec<u8> = a.props.iter().flat_map(|t| full_box(b"auxC", 0, &[t.as_bytes(), &[0u8]].concat())).collect();
        let mut ipma = (a.assoc.len() as u32).to_be_bytes().to_vec();
        for &(item, index) in a.assoc {
            if a.wide {
                ipma.extend_from_slice(&u32::from(item).to_be_bytes());
                ipma.push(1);
                ipma.extend_from_slice(&(0x8000 | index).to_be_bytes()); // essential bit set
            } else {
                ipma.extend_from_slice(&item.to_be_bytes());
                ipma.push(1);
                ipma.push(0x80 | index as u8);
            }
        }
        let ipma = bx(b"ipma", &[&[u8::from(a.wide), 0, 0, u8::from(a.wide)][..], &ipma].concat());
        let iprp = bx(b"iprp", &[bx(b"ipco", &ipco), ipma].concat());
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        [ftyp, full_box(b"meta", 0, &[pitm, iref, iprp, items].concat())].concat()
    }

    const DEPTH: &str = "urn:mpeg:hevc:2015:auxid:2";
    const ALPHA: &str = "urn:mpeg:hevc:2015:auxid:1";
    const MATTE: &str = "urn:com:apple:photo:2018:aux:portraiteffectsmatte";

    /// Issue #4334: as in an iPhone Portrait photo, item 38 is a depth map auxiliary to the primary item 36, beside a
    /// matte (item 40). Both `ipma` layouts are read, and [`crate::extract`] reports it.
    #[test]
    fn a_depth_map_auxiliary_to_the_primary_is_recorded_depth() {
        for wide in [false, true] {
            let file = aux_heic(&Aux { primary: Some(36), auxl: &[(38, 36), (40, 36)], props: &[MATTE, DEPTH], assoc: &[(40, 1), (38, 2)], wide });
            assert!(has_depth(&file), "wide ipma: {wide}");
            assert!(crate::extract(&file).has_depth, "wide ipma: {wide}");
            assert!(crate::try_extract(&file).is_ok_and(|m| m.has_depth), "wide ipma: {wide}");
        }
        let mpeg = aux_heic(&Aux {
            primary: Some(1),
            auxl: &[(2, 1)],
            props: &["urn:mpeg:mpegB:cicp:systems:auxiliary:depth"],
            assoc: &[(2, 1)],
            wide: false,
        });
        assert!(has_depth(&mpeg), "MPEG's codec-independent depth type");
    }

    /// Other auxiliary images, a depth map of another item (a thumbnail's), or a depth property no auxiliary item uses
    /// are not the photo's recorded depth; nor is anything without a primary item, or a file that isn't HEIF.
    #[test]
    fn only_a_depth_map_of_the_primary_counts() {
        let cases = [
            ("alpha only", Aux { primary: Some(1), auxl: &[(2, 1)], props: &[ALPHA], assoc: &[(2, 1)], wide: false }),
            ("matte only", Aux { primary: Some(1), auxl: &[(2, 1)], props: &[MATTE], assoc: &[(2, 1)], wide: false }),
            ("depth of a thumbnail", Aux { primary: Some(1), auxl: &[(3, 2)], props: &[DEPTH], assoc: &[(3, 1)], wide: false }),
            (
                "depth property on the primary itself",
                Aux { primary: Some(1), auxl: &[(2, 1)], props: &[DEPTH, ALPHA], assoc: &[(1, 1), (2, 2)], wide: false },
            ),
            ("no primary item", Aux { primary: None, auxl: &[(2, 1)], props: &[DEPTH], assoc: &[(2, 1)], wide: false }),
        ];
        for (why, a) in cases {
            assert!(!has_depth(&aux_heic(&a)), "{why}");
        }
        assert!(!crate::extract(b"\xff\xd8\xff\xd9").has_depth, "a JPEG");
        assert!(!crate::extract(&heic(&exif_only(&iphone_exif("2026:10:18 13:00:00", "-05:00")), 1, false)).has_depth, "a plain HEIC");
    }

    /// Hostile input: every truncation of a Portrait-shaped file, and counts that claim more than is there, read as
    /// "no depth" (or depth, when the cut leaves it whole) without panicking.
    #[test]
    fn a_truncated_or_lying_file_never_panics() {
        let file = aux_heic(&Aux { primary: Some(36), auxl: &[(38, 36)], props: &[DEPTH], assoc: &[(38, 1)], wide: true });
        for end in 0..file.len() {
            let _ = has_depth(&file[..end]);
        }
        assert_eq!(associations(&[1, 0, 0, 1, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 7, 200]), Vec::new(), "count past the end");
        assert_eq!(associations(&[]), Vec::new());
    }

    /// A packet as iPhones write it, with Portrait settings in Apple's `depthBlurEffect` namespace.
    fn packet(body: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:depthBlurEffect="http://ns.apple.com/depthBlurEffect/1.0/">{body}</rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    /// Issue #4333: the XMP that describes the depth map (item 39 → 38) is found by its `cdsc` reference, not by
    /// order: the matte's packet (41 → 40) comes first here, and the photo's own (51 → 36) last.
    #[test]
    fn the_depth_maps_xmp_is_found_by_its_reference() {
        let depth = packet("<depthBlurEffect:SimulatedAperture>2.800000</depthBlurEffect:SimulatedAperture>");
        let (matte, photo) = (packet(""), packet("<depthBlurEffect:SimulatedAperture>9</depthBlurEffect:SimulatedAperture>"));
        let portrait = Aux { primary: Some(36), auxl: &[(38, 36), (40, 36)], props: &[MATTE, DEPTH], assoc: &[(40, 1), (38, 2)], wide: false };
        let file = aux_heic_with_xmp(&portrait, &[(41, 40, &matte), (39, 38, &depth), (51, 36, &photo)]);
        assert!(has_depth(&file));
        assert_eq!(depth_xmp(&file).as_deref(), Some(depth.as_str()));
        assert_eq!(crate::extract(&file).portrait_aperture, Some(2.8));
        // a matte but no depth map, or a depth map without a packet of its own: nothing
        let matte_only = Aux { primary: Some(36), auxl: &[(40, 36)], props: &[MATTE], assoc: &[(40, 1)], wide: false };
        let none = aux_heic_with_xmp(&matte_only, &[(41, 40, &depth)]);
        assert_eq!((depth_xmp(&none), crate::extract(&none).portrait_aperture), (None, None));
        let bare = aux_heic_with_xmp(&portrait, &[(51, 36, &photo)]);
        assert_eq!((depth_xmp(&bare), crate::extract(&bare).portrait_aperture), (None, None));
        for end in 0..file.len() {
            let _ = crate::extract(&file[..end]);
        }
    }

    /// Issue #4336: an iPhone writes the depth map's and mattes' XMP before the photo's own. The photo's packet (51,
    /// describing the primary item 36) is the one read, with what it says: here a title.
    #[test]
    fn the_photos_own_xmp_is_read_not_the_first_one() {
        let photo = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Harbour</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let depth = packet("<depthBlurEffect:SimulatedAperture>2.8</depthBlurEffect:SimulatedAperture>");
        let portrait = Aux { primary: Some(36), auxl: &[(38, 36), (40, 36)], props: &[MATTE, DEPTH], assoc: &[(40, 1), (38, 2)], wide: false };
        let file = aux_heic_with_xmp(&portrait, &[(39, 38, &depth), (41, 40, &packet("")), (51, 36, photo)]);
        assert_eq!(embedded(&file).xmp.as_deref(), Some(photo));
        let m = crate::extract(&file);
        assert_eq!(m.title.as_deref(), Some("Harbour"));
        assert_eq!(m.portrait_aperture, Some(2.8), "the depth map's packet still gives the Portrait aperture");
    }

    /// Without a packet for the photo: one that describes no item is the photo's (as in a file without `iref`), and
    /// one that describes only another image is never taken for it.
    #[test]
    fn a_packet_of_another_image_is_never_the_photos() {
        let portrait = Aux { primary: Some(36), auxl: &[(38, 36)], props: &[DEPTH], assoc: &[(38, 1)], wide: false };
        let (depth, loose) = (packet("<depthBlurEffect:SimulatedAperture>2.8</depthBlurEffect:SimulatedAperture>"), packet(""));
        assert_eq!(embedded(&aux_heic_with_xmp(&portrait, &[(39, 38, &depth)])).xmp, None);
        let unreferenced = aux_heic_with_xmp(&portrait, &[(39, 38, &depth), (60, 0, &loose)]);
        // item 60's `cdsc` points at item 0, which doesn't exist: drop the reference to describe nothing
        let unreferenced = drop_reference_to_item_0(unreferenced);
        assert_eq!(embedded(&unreferenced).xmp.as_deref(), Some(loose.as_str()));
    }

    /// Turns a `cdsc` reference to item 0 into a `free` box, so the XMP item it came from describes nothing.
    fn drop_reference_to_item_0(mut file: Vec<u8>) -> Vec<u8> {
        let needle = [&b"cdsc"[..], &60u16.to_be_bytes(), &1u16.to_be_bytes(), &0u16.to_be_bytes()].concat();
        if let Some(at) = file.windows(needle.len()).position(|w| w == needle.as_slice()) {
            file[at..at + 4].copy_from_slice(b"free");
        }
        file
    }
}
