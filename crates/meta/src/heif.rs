//! HEIF / HEIC / AVIF metadata: the `Exif` and XMP items of the ISO base media file container
//! (box structure from ISO/IEC 14496-12, item model from ISO/IEC 23008-12).
//!
//! HEIC is what iPhones and Macs write. Its pixels need an HEVC decoder (the optional
//! `lightcraft-heif` crate), its metadata does not, so this reader is in every build: a HEIC
//! imports with its capture time, camera and lens whether or not the build can show the picture.
//!
//! The `meta` box holds an item information box (`iinf`, one `infe` per item) naming each item's
//! type, and an item location box (`iloc`) saying where its bytes are: in the file, or in the
//! `idat` box next to it. The `Exif` item is a four-byte offset followed by a TIFF stream; the XMP
//! packet is a `mime` item whose content type is `application/rdf+xml`. The ICC profile is an item
//! *property*, not an item, and is read by the decoder instead.

use crate::container::Embedded;
use crate::cr3::{MAX_BOXES, be16, be32, be64, boxes};

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
pub fn metadata(bytes: &[u8]) -> Embedded {
    let mut out = Embedded::default();
    let mut budget = MAX_BOXES;
    let Some(meta) = boxes(bytes, 0, bytes.len(), &mut budget).into_iter().find(|b| &b.kind == b"meta") else {
        return out;
    };
    // `meta` is a FullBox: a version and flags before its children
    let (mut items, mut locs, mut idat) = (Vec::new(), Vec::new(), None);
    for b in boxes(bytes, meta.body.saturating_add(4), meta.end, &mut budget) {
        match &b.kind {
            b"iinf" => items = item_infos(bytes, &b, &mut budget),
            b"iloc" => locs = item_locations(bytes.get(b.body..b.end).unwrap_or_default()),
            b"idat" => idat = bytes.get(b.body..b.end),
            _ => {}
        }
    }
    for (id, kind, content_type) in items {
        let want_exif = &kind == b"Exif" && out.exif.is_none();
        let want_xmp = &kind == b"mime" && content_type == "application/rdf+xml" && out.xmp.is_none();
        if !want_exif && !want_xmp {
            continue;
        }
        let Some(loc) = locs.iter().find(|l| l.id == id) else { continue };
        let Some(block) = item_bytes(bytes, idat, loc) else { continue };
        if want_exif {
            // ISO/IEC 23008-12 A.2.1: a four-byte offset from the end of itself to the TIFF header
            let skip = be32(&block, 0).and_then(|v| usize::try_from(v).ok()).and_then(|v| v.checked_add(4));
            out.exif = skip.and_then(|at| block.get(at..)).filter(|b| !b.is_empty()).map(<[u8]>::to_vec);
        } else {
            out.xmp = Some(String::from_utf8_lossy(&block).trim_end_matches('\0').to_string());
        }
    }
    out
}

/// `iinf` → one `(item_ID, item_type, content_type)` per `infe` entry. Entries of version 0 and 1
/// name no item type and are skipped; HEIF files use version 2 or 3.
fn item_infos(bytes: &[u8], iinf: &crate::cr3::BoxRef, budget: &mut usize) -> Vec<(u32, [u8; 4], String)> {
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
