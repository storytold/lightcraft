//! HEIF, HEIC and AVIF metadata through the shared decoder-independent item reader.
use crate::Embedded;
pub use craft_heif::items::is_heif;

/// EXIF and XMP of the primary image, or empty metadata when no readable items exist.
pub fn metadata(bytes: &[u8]) -> Embedded {
    let (exif, xmp) = craft_heif::items::read(bytes).unwrap_or_default();
    Embedded { exif, xmp, ..Default::default() }
}

/// Whether any HEIF metadata item contains location; uncertain items count as location.
pub(crate) fn embeds_location(bytes: &[u8]) -> Option<bool> {
    use craft_heif::items::{Kind, exif_tiff, scan};
    let scan = scan(bytes).ok()?;
    if !scan.complete() {
        return Some(true);
    }
    for item in scan {
        let bytes = match item.data {
            Ok(bytes) => bytes,
            Err(_) => return Some(true),
        };
        let located = match item.kind {
            Kind::Exif => match exif_tiff(&bytes) {
                Some(tiff) => exif_has_location(tiff),
                None => !bytes.is_empty(),
            },
            Kind::Xmp => xmp_has_location(&String::from_utf8_lossy(&bytes)),
            _ => true,
        };
        if located {
            return Some(true);
        }
    }
    Some(false)
}

/// Any GPS tag counts, not only a complete coordinate pair: the projected [`crate::Metadata`] keeps
/// GPS only when latitude and longitude both read, yet a lone latitude is still a location. A GPS
/// IFD pointer whose directory can't be read counts too, and so does any directory the reader
/// couldn't read in full: an entry past its entry limit can be the GPS pointer.
fn exif_has_location(tiff: &[u8]) -> bool {
    use craft_tiff::{ParseOptions, Tiff, tags as t};
    let Ok((parsed, complete)) = Tiff::parse_with_status(tiff, &ParseOptions::default()) else { return true };
    if !complete {
        return true;
    }
    let gps = parsed.all_ifds().into_iter().any(|ifd| match &ifd.gps {
        Some(gps) => gps.entries.iter().any(|e| e.tag != t::GPS_VERSION_ID),
        None => ifd.contains(t::GPS_IFD),
    });
    gps || crate::try_read_exif(tiff).map_or(true, |m| crate::has_location(&m))
}

/// The packet's whole location inventory (IPTC `LocationShown` / `LocationCreated`, GPS
/// properties), not only the fields projected into [`crate::Metadata`]. A packet whose property
/// list was cut short can't prove it has none.
fn xmp_has_location(packet: &str) -> bool {
    let Ok(xmp) = crate::xmp::parse_xmp_with(packet, &[]) else { return true };
    xmp.properties_truncated || xmp.has_location_metadata() || crate::parse_xmp(packet).map_or(true, |x| crate::has_location(&x.metadata))
}
#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod upstream_tests {
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
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing, dead_code)]
mod location_tests {
    use craft_heif::items::MAX_ITEM;
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
    /// A GPS IFD with a latitude but no longitude reads as no `Gps`, but it is still a location
    /// (#4315 review).
    #[test]
    fn a_lone_gps_latitude_in_a_thumbnail_is_seen() {
        use craft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};
        let tiff = |gps: Option<IfdBuilder>| {
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MODEL, Value::Ascii("Thumbnail".into()));
            if let Some(gps) = gps {
                ifd0.set_child(t::GPS_IFD, gps);
            }
            TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap_or_default()
        };
        let mut latitude = IfdBuilder::new();
        latitude.set(t::GPS_VERSION_ID, Value::Byte(vec![2, 3, 0, 0]));
        latitude.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
        latitude.set(t::GPS_LATITUDE, Value::Rational(vec![(48, 1), (51, 1), (0, 1)]));
        let mut version_only = IfdBuilder::new();
        version_only.set(t::GPS_VERSION_ID, Value::Byte(vec![2, 3, 0, 0]));
        for (gps, located) in [(Some(latitude), true), (Some(version_only), false), (None, false)] {
            let tiff = tiff(gps);
            assert_eq!(crate::read_exif(&tiff).gps, None, "no coordinate pair");
            let f = Heif::new()
                .item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1)
                .item(3, b"Exif", exif_item(&[], &tiff), Store::File, 1)
                .cdsc(3, &[2])
                .build();
            assert_eq!(crate::embeds_location(&f), located);
        }
    }

    /// A GPS pointer the reader never reaches, after its 8,192-entry limit, still counts (#4315
    /// review): an incompletely read EXIF item is a possible location.
    #[test]
    fn a_gps_pointer_past_the_entry_limit_counts_as_a_location() {
        let entries = 8193u32;
        let gps_at = 8 + 2 + 12 * entries + 4;
        let mut tiff = b"II\x2a\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&(entries as u16).to_le_bytes());
        for tag in 1..entries as u16 {
            // SHORT, count 1, value 0
            tiff.extend_from_slice(&[&tag.to_le_bytes()[..], &[3, 0, 1, 0, 0, 0, 0, 0, 0, 0]].concat());
        }
        tiff.extend_from_slice(&[&34853u16.to_le_bytes()[..], &[4, 0, 1, 0, 0, 0], &gps_at.to_le_bytes()].concat());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        // GPS IFD: GPSLatitudeRef "N", then no next IFD
        tiff.extend_from_slice(&[1, 0, 1, 0, 2, 0, 2, 0, 0, 0, b'N', 0, 0, 0, 0, 0, 0, 0]);
        let f = Heif::new()
            .item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1)
            .item(3, b"Exif", exif_item(&[], &tiff), Store::File, 1)
            .cdsc(3, &[2])
            .build();
        assert!(crate::embeds_location(&f));
    }

    /// An IPTC `LocationShown` place is a location even though it isn't projected into
    /// `Metadata`'s city or country (#4315 review).
    #[test]
    fn an_iptc_location_shown_in_a_thumbnail_is_seen() {
        let packet = |inner: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:Iptc4xmpExt="http://iptc.org/std/Iptc4xmpExt/2008-02-29/" xmlns:dc="http://purl.org/dc/elements/1.1/">{inner}</rdf:Description></rdf:RDF></x:xmpmeta>"#
            )
        };
        let shown = packet(
            r#"<Iptc4xmpExt:LocationShown><rdf:Bag><rdf:li rdf:parseType="Resource"><Iptc4xmpExt:City>Paris</Iptc4xmpExt:City></rdf:li></rdf:Bag></Iptc4xmpExt:LocationShown>"#,
        );
        let plain = packet(r#"<dc:format>image/heic</dc:format>"#);
        for (xmp, located) in [(shown, true), (plain, false)] {
            assert!(crate::parse_xmp(&xmp).is_ok_and(|x| !crate::has_location(&x.metadata)), "not projected into Metadata");
            let f = Heif::new()
                .item(2, b"hvc1", b"thumbnail".to_vec(), Store::File, 1)
                .xmp(3, ("application/rdf+xml", ""), xmp.into_bytes())
                .cdsc(3, &[2])
                .build();
            assert_eq!(crate::embeds_location(&f), located);
        }
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
}
