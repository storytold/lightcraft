//! ISO 21496-1 gain-map AVIF: the SDR base image as the primary `av01` item, a hidden `av01`
//! item holding the single-channel gain map, and a `tmap` (tone map derived image) item that
//! carries the ISO 21496-1 metadata and is linked to both by a `dimg` reference. Viewers that
//! know gain maps rebuild the HDR rendition from the base and the map; the rest show the base,
//! which is an ordinary AVIF.
//!
//! The file is laid out as libavif 1.2 lays it out (itself following ISO/IEC 23008-12:2024
//! amendment 1, "tone map derived image items", for the `tmap` item and the `tmap` brand, and
//! ISO 21496-1 clause C.2.2 for the metadata):
//!
//! - `ftyp`: the base file's brands plus `tmap` in the compatible brands (ISO/IEC 23008-12:2024
//!   AMD 1: when the brand is present it shall be among the compatible brands).
//! - the base `av01` item, the primary item, with the properties its own file had.
//! - `tmap` item: payload `version` (u8, 0) followed by the ISO 21496-1 `GainMapMetadata`
//!   (versions, flags, rationals; **no** URN: the URN only names the JPEG APP2 payload). Its
//!   properties are `ispe` (the base size) and a `colr` nclx describing the alternate (HDR)
//!   image: the base primaries, a linear transfer, no matrix. It is not hidden. (avifenc writes
//!   PQ for the transfer, the convention of gain map JPEGs; this writer describes the linear
//!   light its own gain map rebuilds.)
//! - the gain map item: a hidden `av01` image, 8-bit monochrome, full range, colour primaries and
//!   transfer "unspecified" (2), identity matrix, with `ispe`, `av1C` (level 31, as the base's),
//!   `colr` nclx and `pixi`.
//! - `iref`: one `dimg` reference from the `tmap` item to `[base, gain map]`, in that order.
//! - `grpl`/`altr`: the `tmap` item and the base item as alternatives (AVIF v1.2.0 §4.2.2:
//!   "should be grouped together by an 'altr' entity group").
//!
//! Sources: read as a reference, own code: libavif v1.2.1 `src/write.c`
//! (`avifWriteGainmapMetadata`, `avifWriteToneMappedImagePayload`, `avifEncoderWrite`, and its
//! `ftyp`, `iinf`, `iref` and `altr` writers) and `src/read.c` (`avifParseToneMappedImageBox`,
//! `avifDecoderDataFindToneMappedImageItem`), BSD-2-Clause; AV1 Image File Format v1.2.0 §4.2.2
//! and §5.1; ISO/IEC 23008-12 box syntax as libavif cites it (only the box layout is
//! reproduced). The AV1 gain map is encoded with rav1e and the base with `ravif`, as every other
//! AVIF here.

use crate::encode::{EncodeImage, EncodeMeta};
use crate::{Error, Result};

/// The name libavif gives the `tmap` and gain map items.
const GAIN_MAP_NAME: &[u8] = b"GMap";

/// `seq_level_idx` value that means "no level constraint" (AV1 sequence header).
const LEVEL_UNCONSTRAINED: u8 = 31;

fn bad(m: impl Into<String>) -> Error {
    Error::Encode(format!("gain map AVIF: {}", m.into()))
}

/// One box, 32-bit size. More than 4 GiB is an error, not a wrapped size.
fn r#box(ty: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>> {
    let len = payload.len().checked_add(8).filter(|n| *n <= u32::MAX as usize).ok_or_else(|| bad("the file would be over 4 GiB"))?;
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(&(len as u32).to_be_bytes());
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    Ok(v)
}

/// A full box: version and flags lead the payload.
fn full_box(ty: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Result<Vec<u8>> {
    let mut p = Vec::with_capacity(payload.len().saturating_add(4));
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    r#box(ty, &p)
}

/// `colr` with an `nclx` colour description, full range.
fn nclx(primaries: u16, transfer: u16, matrix: u16) -> Result<Vec<u8>> {
    r#box(b"colr", &[b"nclx".as_slice(), &primaries.to_be_bytes(), &transfer.to_be_bytes(), &matrix.to_be_bytes(), &[0x80]].concat())
}

/// The `av1C` property of the gain map: profile 0, level 31 ("unconstrained", as the base
/// image's own `av1C` has), 8-bit monochrome (bit 4 of the third byte), identity matrix, full
/// range (avif-serialize's layout).
fn gain_av1c() -> Result<Vec<u8>> {
    r#box(b"av1C", &[0x81, LEVEL_UNCONSTRAINED, 0x10, 0])
}

/// A reader that never panics; every read past the end is an error.
struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b }
    }
    fn rest(&self) -> &'a [u8] {
        self.b
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (a, rest) = self.b.split_at_checked(n).ok_or_else(|| bad("a box is truncated"))?;
        self.b = rest;
        Ok(a)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| bad("u16"))?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| bad("u32"))?))
    }
    /// An unsigned integer of `n` bytes (0 = 0).
    fn uint(&mut self, n: usize) -> Result<u64> {
        if n > 8 {
            return Err(bad("integer too wide"));
        }
        let mut v = 0u64;
        for &x in self.take(n)? {
            v = (v << 8) | u64::from(x);
        }
        Ok(v)
    }
    /// A NUL-terminated string.
    fn cstr(&mut self) -> Result<&'a [u8]> {
        let n = self.b.iter().position(|&c| c == 0).unwrap_or(self.b.len());
        let s = self.take(n)?;
        if !self.b.is_empty() {
            self.b = self.b.get(1..).unwrap_or(&[]);
        }
        Ok(s)
    }
    /// The type and payload length of the next box in a box list.
    fn box_header(&mut self) -> Result<([u8; 4], usize)> {
        let size = self.u32()?;
        let ty: [u8; 4] = self.take(4)?.try_into().map_err(|_| bad("box type"))?;
        let (hdr, size) = match size {
            0 => (8usize, self.rest().len() as u64),
            1 => {
                let hi = self.u32()?;
                let lo = self.u32()?;
                (16usize, (u64::from(hi) << 32) | u64::from(lo))
            }
            s => (8usize, u64::from(s)),
        };
        let size = usize::try_from(size).map_err(|_| bad("box too large"))?;
        let len = size.checked_sub(hdr).ok_or_else(|| bad("box smaller than its header"))?;
        if len > self.rest().len() {
            return Err(bad("a box runs past the end"));
        }
        Ok((ty, len))
    }
}

/// An item of the base file, as it is written again.
#[derive(Clone)]
struct Item {
    id: u16,
    ty: [u8; 4],
    /// Item name; for `mime` items the content type.
    name: Vec<u8>,
    mime: Option<Vec<u8>>,
    hidden: bool,
    /// Properties in association order: essential flag and the raw property box.
    props: Vec<(bool, Vec<u8>)>,
    data: Vec<u8>,
}

struct Base {
    minor_version: u32,
    brands: Vec<[u8; 4]>,
    hdlr: Option<Vec<u8>>,
    primary: u16,
    items: Vec<Item>,
}

/// Read the single-image file a base encode produced (avif-serialize's layout, through `ravif`):
/// version 0 `pitm`, `iloc` (one or more extents in the file itself), version 2 `infe`,
/// `ipco`/`ipma`. Anything else is refused: this reads our own output, not foreign files.
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn read_base(file: &[u8]) -> Result<Base> {
    use crate::avif::boxes;
    let top = boxes(file).map_err(|_| bad("the base image's boxes could not be read"))?;
    let find = |t: &[u8; 4]| top.iter().find(|(k, _)| k == t).map(|(_, p)| *p);
    let ftyp = find(b"ftyp").ok_or_else(|| bad("the base file has no ftyp box"))?;
    let mut r = Reader::new(ftyp);
    r.skip(4).map_err(|_| bad("ftyp"))?;
    let minor_version = r.u32()?;
    let mut brands = Vec::new();
    for b in r.rest().as_chunks::<4>().0 {
        let ty = [b[0], b[1], b[2], b[3]];
        if !brands.contains(&ty) {
            brands.push(ty);
        }
    }
    let meta = find(b"meta").ok_or_else(|| bad("the base file has no meta box"))?;
    let meta = meta.get(4..).ok_or_else(|| bad("the base meta box is truncated"))?;
    let (mut primary, mut hdlr) = (None, None);
    let mut items: Vec<Item> = Vec::new();
    let mut props: Vec<Vec<u8>> = Vec::new();
    let mut assoc: Vec<(u16, Vec<(bool, usize)>)> = Vec::new();
    for (t, p) in boxes(meta).map_err(|_| bad("the base meta box could not be read"))? {
        match &t {
            b"hdlr" if p.len() >= 4 => hdlr = Some(p.to_vec()),
            b"pitm" if p.first() == Some(&0) => primary = Some(Reader::new(p.get(4..).unwrap_or(&[])).u16()?),
            b"iloc" if p.len() >= 8 && p[0] == 0 => {
                let (off_size, len_size, base_size) = (usize::from(p[4] >> 4), usize::from(p[4] & 15), usize::from(p[5] >> 4));
                if p[5] & 15 != 0 || off_size > 8 || len_size > 8 || base_size > 8 {
                    return Err(bad("the base iloc box uses fields this writer does not read"));
                }
                let mut q = Reader::new(p.get(6..).unwrap_or(&[]));
                for _ in 0..q.u16()? {
                    let id = q.u16()?;
                    q.u16()?; // data reference index
                    let base = q.uint(base_size)?;
                    let mut data = Vec::new();
                    for _ in 0..q.u16()? {
                        let start = base.saturating_add(q.uint(off_size)?);
                        let len = q.uint(len_size)?;
                        let (Some(a), Some(b)) = (usize::try_from(start).ok(), usize::try_from(len).ok()) else {
                            return Err(bad("an item runs outside the base file"));
                        };
                        data.extend_from_slice(file.get(a..a.saturating_add(b)).ok_or_else(|| bad("an item runs outside the base file"))?);
                    }
                    items.push(Item { id, ty: [0; 4], name: Vec::new(), mime: None, hidden: false, props: Vec::new(), data });
                }
            }
            b"iinf" if p.len() >= 6 && p[0] == 0 => {
                let mut q = Reader::new(p.get(4..).unwrap_or(&[]));
                for _ in 0..q.u16()? {
                    let (ty, len) = q.box_header()?;
                    let payload = q.take(len)?;
                    if &ty != b"infe" {
                        continue;
                    }
                    let mut e = Reader::new(payload);
                    let version = e.u8()?;
                    let flags = [e.u8()?, e.u8()?, e.u8()?];
                    let id = e.u16()?;
                    e.u16()?; // protection index
                    let ity: [u8; 4] = e.take(4)?.try_into().map_err(|_| bad("item type"))?;
                    if version < 2 {
                        continue; // no item_type before version 2: not an image item
                    }
                    let name = e.cstr()?.to_vec();
                    let mime = (ity == *b"mime").then(|| e.cstr().map(|m| m.to_vec())).transpose()?;
                    match items.iter_mut().find(|i| i.id == id) {
                        Some(i) => (i.ty, i.hidden, i.name, i.mime) = (ity, flags[2] & 1 != 0, name, mime),
                        None => return Err(bad("an item has no location")),
                    }
                }
            }
            b"iprp" => {
                for (k, v) in boxes(p).map_err(|_| bad("the base iprp box could not be read"))? {
                    match &k {
                        b"ipco" => {
                            for (kt, kv) in boxes(v).map_err(|_| bad("the base ipco box could not be read"))? {
                                props.push(r#box(&kt, kv)?);
                            }
                        }
                        b"ipma" if v.first() == Some(&0) => {
                            let wide = v.get(3).is_some_and(|f| f & 1 != 0);
                            let mut q = Reader::new(v.get(4..).unwrap_or(&[]));
                            for _ in 0..q.u32()? {
                                let id = q.u16()?;
                                let n = q.u8()?;
                                let mut list = Vec::with_capacity(usize::from(n).min(64));
                                for _ in 0..n {
                                    let a = if wide { q.u16()? } else { u16::from(q.u8()?) };
                                    let (bit, mask) = if wide { (0x8000, 0x7fff) } else { (0x80, 0x7f) };
                                    list.push(((a & bit) != 0, usize::from(a & mask)));
                                }
                                assoc.push((id, list));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    for i in items.iter_mut() {
        if let Some((_, list)) = assoc.iter().find(|(id, _)| *id == i.id) {
            for (essential, idx) in list {
                // property indices are 1-based
                let p = props.get(idx.checked_sub(1).unwrap_or(usize::MAX)).ok_or_else(|| bad("an item names a property that does not exist"))?;
                i.props.push((*essential, p.clone()));
            }
        }
    }
    let primary =
        primary.filter(|id| items.iter().any(|i| i.id == *id && i.ty == *b"av01")).ok_or_else(|| bad("the base file has no AV1 primary item"))?;
    Ok(Base { minor_version, brands, hdlr, primary, items })
}

/// The `tmap` item payload: `version` then the ISO 21496-1 metadata (the same bytes as the JPEG
/// APP2 payload after its URN).
fn tone_map_payload(m: &crate::gainmap::GainMapMeta) -> Result<Vec<u8>> {
    let iso = m.to_iso();
    let body = iso.get(crate::gainmap::ISO_URN.len()..).ok_or_else(|| bad("metadata"))?;
    let mut p = Vec::with_capacity(body.len().saturating_add(1));
    p.push(0); // version
    p.extend_from_slice(body);
    Ok(p)
}

/// Encode the gain map as an 8-bit monochrome AV1 still picture, full range.
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn encode_gain_av1(gain: &crate::gainmap::GainImage, quality: u8, speed: u8) -> Result<Vec<u8>> {
    use rav1e::prelude::*;
    let (w, h) = (gain.width, gain.height);
    let n = w.checked_mul(h).filter(|_| w > 0 && h > 0).ok_or_else(|| bad("the gain map has no size or is too large"))?;
    if gain.data.len() < n {
        return Err(bad("the gain map buffer is short"));
    }
    let q = quality.clamp(1, 100) as f32 / 100.0;
    // the same quality → quantizer curve as the other AVIF paths
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        1.0 - 0.125 - q * 0.5
    } else {
        1.0 - q
    };
    let quantizer = (x * 255.0).round() as usize;
    let mut ec = EncoderConfig::with_speed_preset(speed.clamp(1, 10));
    ec.width = w;
    ec.height = h;
    ec.time_base = Rational::new(1, 1);
    ec.bit_depth = 8;
    ec.chroma_sampling = ChromaSampling::Cs400;
    ec.pixel_range = PixelRange::Full;
    // primaries and transfer "unspecified", as libavif requires of a gain map image
    ec.color_description = Some(ColorDescription {
        color_primaries: ColorPrimaries::Unspecified,
        transfer_characteristics: TransferCharacteristics::Unspecified,
        matrix_coefficients: MatrixCoefficients::Identity,
    });
    ec.still_picture = true;
    ec.quantizer = quantizer;
    ec.min_quantizer = quantizer.min(255) as u8;
    ec.speed_settings.multiref = false;
    ec.speed_settings.rdo_lookahead_frames = 1;
    ec.speed_settings.scene_detection_mode = SceneDetectionSpeed::None;
    let e = |x: &dyn std::fmt::Display| bad(format!("AV1: {x}"));
    let mut ctx: Context<u8> = Config::new().with_encoder_config(ec).new_context().map_err(|x| e(&x))?;
    let mut frame = ctx.new_frame();
    {
        let Some(plane) = frame.planes.first_mut() else { return Err(bad("AV1: the frame has no plane")) };
        let mut area = plane.mut_slice(Default::default());
        for (y, row) in area.rows_iter_mut().take(h).enumerate() {
            let src = gain.data.get(y * w..y * w + w).unwrap_or(&[]);
            for (dst, &v) in row.iter_mut().take(w).zip(src) {
                *dst = v;
            }
        }
    }
    ctx.send_frame(frame).map_err(|x| e(&x))?;
    ctx.flush();
    let mut obus = Vec::new();
    loop {
        match ctx.receive_packet() {
            Ok(mut p) if p.frame_type == FrameType::KEY => obus.append(&mut p.data),
            Ok(_) => {}
            Err(EncoderStatus::Encoded | EncoderStatus::LimitReached) => break,
            Err(x) => return Err(e(&x)),
        }
    }
    if obus.is_empty() {
        return Err(bad("AV1: no picture was encoded"));
    }
    Ok(obus)
}

/// An item as it leaves the writer.
struct Out {
    item: Item,
    /// Properties in association order: essential flag and 1-based index into `ipco`.
    props: Vec<(bool, usize)>,
}

/// The file without `mdat`: the leading boxes, and the item data in `mdat` order.
fn assemble(base: &Base, gain: &crate::gainmap::GainImage, m: &crate::gainmap::GainMapMeta, gain_av1: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let base_item = base.items.iter().find(|i| i.id == base.primary && !i.hidden).ok_or_else(|| bad("no base image"))?;
    if base_item.props.iter().any(|(_, p)| &p[4..8] == b"auxC") {
        return Err(bad("the base item is an auxiliary image"));
    }
    let (gw, gh) =
        (u32::try_from(gain.width).map_err(|_| bad("gain map too large"))?, u32::try_from(gain.height).map_err(|_| bad("gain map too large"))?);
    let is_new = |id: u16| !base.items.iter().any(|i| i.id == id);
    let tmap_id = (1..=u16::MAX).find(|id| is_new(*id)).ok_or_else(|| bad("too many items"))?;
    let gain_id = (tmap_id.saturating_add(1)..=u16::MAX).find(|id| is_new(*id)).ok_or_else(|| bad("too many items"))?;
    let group_id = (gain_id.saturating_add(1)..=u16::MAX).find(|id| is_new(*id)).ok_or_else(|| bad("too many items"))?;

    // the property container: every property the base file had (first-seen order), then the new ones
    let mut props: Vec<Vec<u8>> = Vec::new();
    fn index(props: &mut Vec<Vec<u8>>, p: &[u8]) -> Result<usize> {
        if let Some(i) = props.iter().position(|q| q == p) {
            return Ok(i + 1);
        }
        props.push(p.to_vec());
        Ok(props.len())
    }
    let mut out: Vec<Out> = Vec::new();
    for i in &base.items {
        let mut list = Vec::with_capacity(i.props.len());
        for (essential, p) in &i.props {
            list.push((*essential, index(&mut props, p)?));
        }
        out.push(Out { item: i.clone(), props: list });
    }
    let base_ispe =
        base_item.props.iter().find(|(_, p)| &p[4..8] == b"ispe").map(|(_, p)| p.as_slice()).ok_or_else(|| bad("the base item has no ispe"))?;
    base_ispe.get(12..20).ok_or_else(|| bad("the base ispe is truncated"))?;
    let ispe_base = index(&mut props, base_ispe)?;
    let ispe_gain = index(&mut props, &full_box(b"ispe", 0, 0, &[gw.to_be_bytes(), gh.to_be_bytes()].concat())?)?;
    let av1c_gain = index(&mut props, &gain_av1c()?)?;
    let colr_gain = index(&mut props, &nclx(2, 2, 0)?)?;
    let pixi_gain = index(&mut props, &full_box(b"pixi", 0, 0, &[1, 8])?)?;
    // The alternate image is the base's linear light with the headroom the map adds, in the base
    // primaries (the gain applies in the base colour space): primaries from the base, transfer
    // "linear" (8), matrix "unspecified" (2). (avifenc writes PQ here, the convention of gain map
    // JPEGs; this writer describes what its own gain map rebuilds.)
    let alt_primaries = base_item
        .props
        .iter()
        .find(|(_, p)| &p[4..8] == b"colr" && &p[8..12] == b"nclx")
        .and_then(|(_, p)| p.get(12..14))
        .and_then(|b| b.try_into().ok())
        .map_or(1, u16::from_be_bytes);
    let colr_alt = index(&mut props, &nclx(alt_primaries, 8, 2)?)?;
    if props.len() >= 128 {
        return Err(bad("too many item properties"));
    }
    let plain = |id: u16, ty: &[u8; 4], hidden: bool, data: Vec<u8>| Item {
        id,
        ty: *ty,
        name: GAIN_MAP_NAME.to_vec(),
        mime: None,
        hidden,
        props: Vec::new(),
        data,
    };
    out.push(Out { item: plain(tmap_id, b"tmap", false, tone_map_payload(m)?), props: vec![(false, ispe_base), (false, colr_alt)] });
    out.push(Out {
        item: plain(gain_id, b"av01", true, gain_av1.to_vec()),
        props: vec![(false, ispe_gain), (true, av1c_gain), (false, colr_gain), (false, pixi_gain)],
    });
    let count = u16::try_from(out.len()).map_err(|_| bad("too many items"))?;

    // ftyp: the base brands and the `tmap` brand
    let mut brands = base.brands.clone();
    if !brands.contains(b"tmap") {
        brands.push(*b"tmap");
    }
    let ftyp = r#box(b"ftyp", &[b"avif".as_slice(), &base.minor_version.to_be_bytes(), &brands.concat()].concat())?;

    // meta children: hdlr, pitm, iinf, iref, iprp, grpl; iloc follows once the size is known
    let mut meta = Vec::new();
    if let Some(h) = &base.hdlr {
        meta.extend(r#box(b"hdlr", h)?); // a full box: its version and flags are in the payload
    }
    meta.extend(full_box(b"pitm", 0, 0, &base.primary.to_be_bytes())?);
    let mut infe = Vec::new();
    for o in &out {
        let i = &o.item;
        let mut tail = i.name.clone();
        tail.push(0);
        if let Some(mime) = &i.mime {
            tail.extend_from_slice(mime);
            tail.push(0);
        }
        infe.extend(full_box(b"infe", 2, u32::from(i.hidden), &[&i.id.to_be_bytes(), &[0, 0][..], &i.ty, tail.as_slice()].concat())?);
    }
    meta.extend(full_box(b"iinf", 0, 0, &[count.to_be_bytes().as_slice(), infe.as_slice()].concat())?);
    meta.extend(full_box(
        b"iref",
        0,
        0,
        &r#box(b"dimg", &[&tmap_id.to_be_bytes(), &[0, 2][..], &base.primary.to_be_bytes(), &gain_id.to_be_bytes()].concat())?,
    )?);
    let mut ipma = Vec::new();
    for o in &out {
        let n = u8::try_from(o.props.len()).map_err(|_| bad("too many properties on an item"))?;
        ipma.extend_from_slice(&o.item.id.to_be_bytes());
        ipma.push(n);
        for (essential, idx) in &o.props {
            ipma.push(if *essential { 0x80 } else { 0 } | u8::try_from(*idx).map_err(|_| bad("too many item properties"))? & 0x7f);
        }
    }
    let ipma = full_box(b"ipma", 0, 0, &[u32::from(count).to_be_bytes().as_slice(), ipma.as_slice()].concat())?;
    meta.extend(r#box(b"iprp", &[r#box(b"ipco", &props.concat())?.as_slice(), ipma.as_slice()].concat())?);
    // grpl/altr: group id, entity count and entity ids are 32-bit
    let altr: Vec<u8> = [
        u32::from(group_id).to_be_bytes().as_slice(),
        2u32.to_be_bytes().as_slice(),
        u32::from(tmap_id).to_be_bytes().as_slice(),
        u32::from(base.primary).to_be_bytes().as_slice(),
    ]
    .concat();
    meta.extend(r#box(b"grpl", &full_box(b"altr", 0, 0, &altr)?)?);

    let data: Vec<Vec<u8>> = out.iter().map(|o| o.item.data.clone()).collect();
    let total = data.iter().try_fold(0u64, |a, d| a.checked_add(d.len() as u64)).ok_or_else(|| bad("the file would be over 4 GiB"))?;
    // iloc: version 0, 4-byte offsets and lengths, no base offset, one extent per item
    let iloc = |offsets: &[u64]| -> Result<Vec<u8>> {
        let mut v = vec![0x44, 0]; // offset_size 4, length_size 4, base_offset_size 0, reserved 0
        v.extend_from_slice(&count.to_be_bytes());
        for (o, (&at, d)) in out.iter().zip(offsets.iter().zip(&data)) {
            let at = u32::try_from(at).map_err(|_| bad("the file would be over 4 GiB"))?;
            let len = u32::try_from(d.len()).map_err(|_| bad("the file would be over 4 GiB"))?;
            v.extend_from_slice(&o.item.id.to_be_bytes());
            v.extend_from_slice(&[0, 0, 0, 1]); // data reference index, extent count
            v.extend_from_slice(&at.to_be_bytes());
            v.extend_from_slice(&len.to_be_bytes());
        }
        full_box(b"iloc", 0, 0, &v)
    };
    let empty = vec![0u64; out.len()];
    // every iloc field has a fixed width, so meta's size is the same whatever the offsets are
    let meta_len = full_box(b"meta", 0, 0, &[meta.as_slice(), iloc(&empty)?.as_slice()].concat())?.len();
    let mut at = ftyp.len() as u64 + meta_len as u64 + 8; // + the mdat header
    let mut offsets = Vec::with_capacity(out.len());
    for d in &data {
        offsets.push(at);
        at = at.saturating_add(d.len() as u64);
    }
    if at > u64::from(u32::MAX) || total > u64::from(u32::MAX) - 8 {
        return Err(bad("the file would be over 4 GiB"));
    }
    let meta = full_box(b"meta", 0, 0, &[meta.as_slice(), iloc(&offsets)?.as_slice()].concat())?;
    Ok(([ftyp.as_slice(), meta.as_slice()].concat(), data))
}

/// Encode an ISO 21496-1 gain-map AVIF: `base` (8-bit, as [`crate::encode::encode_avif`] writes
/// it, EXIF included) plus the single-channel `gain` and its metadata `m` (see the module docs
/// for the layout). `quality` and `speed` are as for `encode_avif`; the gain map is encoded at
/// the same quality. Returns [`Error::Encode`] for a three-channel or short gain map, unusable
/// metadata, wasm32 or a build without the `avif` feature.
pub fn encode_avif_gain_map(
    base: &EncodeImage,
    gain: &crate::gainmap::GainImage,
    m: &crate::gainmap::GainMapMeta,
    quality: u8,
    speed: u8,
    meta: &EncodeMeta,
) -> Result<Vec<u8>> {
    base.validate()?;
    if !m.is_valid() {
        return Err(bad("the gain map metadata is not usable"));
    }
    if gain.channels != 1 || m.multichannel {
        return Err(bad("AVIF gain maps are single-channel: a three-channel map needs multichannel metadata and is not written"));
    }
    if gain.width == 0 || gain.height == 0 || gain.data.len() < gain.width.saturating_mul(gain.height) {
        return Err(bad("the gain map is empty or its buffer is short"));
    }
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    {
        // rav1e asserts internally: a panic there is an export error, not a crash
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let file = crate::encode::encode_avif(base, quality, speed, meta)?;
            let parsed = read_base(&file)?;
            let av1 = encode_gain_av1(gain, quality, speed)?;
            let (head, data) = assemble(&parsed, gain, m, &av1)?;
            let payload = data.iter().map(Vec::len).fold(0usize, |a, n| a.saturating_add(n));
            let mut out = Vec::with_capacity(head.len().saturating_add(payload.saturating_add(8)));
            out.extend_from_slice(&head);
            out.extend_from_slice(&u32::try_from(payload.saturating_add(8)).map_err(|_| bad("the file would be over 4 GiB"))?.to_be_bytes());
            out.extend_from_slice(b"mdat");
            for d in &data {
                out.extend_from_slice(d);
            }
            Ok(out)
        }))
        .unwrap_or_else(|_| Err(bad("the AV1 encoder panicked")))
    }
    #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
    {
        let _ = (gain, m, quality, speed, meta);
        Err(Error::Encode("AVIF encoding is not available in this build".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_map_payload_has_no_urn() {
        let m = crate::gainmap::GainMapMeta::default();
        let p = tone_map_payload(&m).unwrap();
        assert_eq!(p[0], 0);
        let iso = m.to_iso();
        assert_eq!(p[1..], iso[crate::gainmap::ISO_URN.len()..]);
        assert!(!p.windows(crate::gainmap::ISO_URN.len()).any(|w| w == crate::gainmap::ISO_URN));
    }

    #[test]
    fn bad_inputs_are_errors() {
        let img = EncodeImage::new(4, 4, 3, crate::Samples::U8(&[128; 48]));
        let m = crate::gainmap::GainMapMeta::default();
        let em = EncodeMeta::default();
        let g = |width, height, channels, data: Vec<u8>| crate::gainmap::GainImage { width, height, channels, data };
        assert!(encode_avif_gain_map(&img, &g(4, 4, 1, vec![0; 16]), &m, 80, 10, &em).is_ok());
        // three channels
        assert!(encode_avif_gain_map(&img, &g(4, 4, 3, vec![0; 48]), &m, 80, 10, &em).is_err());
        // short buffer, empty map
        for map in [g(4, 4, 1, vec![0; 15]), g(0, 4, 1, vec![0; 16]), g(4, 4, 1, Vec::new())] {
            assert!(encode_avif_gain_map(&img, &map, &m, 80, 10, &em).is_err());
        }
        // three-channel metadata over a single-channel map
        let rgb_meta = crate::gainmap::GainMapMeta { multichannel: true, ..m };
        assert!(encode_avif_gain_map(&img, &g(4, 4, 1, vec![0; 16]), &rgb_meta, 80, 10, &em).is_err());
        // unusable metadata
        let bad_m = crate::gainmap::GainMapMeta { gamma: [0.0; 3], ..m };
        assert!(encode_avif_gain_map(&img, &g(4, 4, 1, vec![0; 16]), &bad_m, 80, 10, &em).is_err());
        // a base whose buffer is short
        let short = EncodeImage::new(4, 4, 3, crate::Samples::U8(&[128; 47]));
        assert!(encode_avif_gain_map(&short, &g(4, 4, 1, vec![0; 16]), &m, 80, 10, &em).is_err());
    }
}

#[cfg(all(test, feature = "avif", not(target_arch = "wasm32")))]
mod gain_map_tests {
    use super::*;
    use crate::gainmap::test_support::fixture;
    use crate::{DecodeOptions, Format, Samples, decode, gainmap};

    fn build(w: usize, h: usize, quality: u8, meta: &EncodeMeta) -> Result<(Vec<u8>, Vec<f32>, Vec<u8>, gainmap::GainImage, gainmap::GainMapMeta)> {
        let (base, hdr, gain, gm) = fixture(w, h)?;
        let img = EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&base));
        let file = encode_avif_gain_map(&img, &gain, &gm, quality, 8, meta)?;
        Ok((file, hdr, base, gain, gm))
    }

    fn mean_abs(a: &[u8], b: &[u8]) -> f64 {
        a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs()).sum::<f64>() / a.len().max(1) as f64
    }

    #[test]
    fn the_gain_map_rebuilds_the_hdr_rendition() {
        let (w, h) = (64usize, 40usize);
        let (file, _, base, gain, gm) = build(w, h, 95, &EncodeMeta::default()).unwrap();
        // an ordinary AVIF to every reader: the base image
        let d = decode(&file, DecodeOptions::default()).unwrap();
        assert_eq!((d.format, d.width as usize, d.height as usize), (Format::Avif, w, h));
        let shown: Vec<u8> = d.to_srgb8().data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        assert!(mean_abs(&shown, &base) < 4.0, "base mean difference {}", mean_abs(&shown, &base));
        // the gain map and its metadata come back
        let back = gainmap::read_avif(&file).expect("the gain map is found");
        assert_eq!((back.gain.width, back.gain.height, back.gain.channels), (gain.width, gain.height, 1));
        assert!((back.meta.min[0] - gm.min[0]).abs() < 1e-5 && (back.meta.max[0] - gm.max[0]).abs() < 1e-5);
        assert!((back.meta.alternate_headroom - gm.alternate_headroom).abs() < 1e-5);
        assert!(mean_abs(&back.gain.data, &gain.data) < 3.0, "gain map mean difference {}", mean_abs(&back.gain.data, &gain.data));
        // applied to the decoded base, the HDR rendition is rebuilt (linear, SDR white = 1)
        let mut lin: Vec<[f32; 3]> = decode(&file, DecodeOptions::default()).unwrap().to_working().data.iter().map(|p| [p[0], p[1], p[2]]).collect();
        gainmap::apply(&mut lin, w, h, &back.gain, &back.meta, 1.0).unwrap();
        let at = |y: usize, x: usize| lin[y * w + x][1];
        assert!((at(h / 8, 2) / 4.0 - 1.0).abs() < 0.12, "highlight {}", at(h / 8, 2));
        let mid = lcc_color::transfer::srgb_to_linear(0.5);
        assert!((at(h * 3 / 4, w / 2) / mid - 1.0).abs() < 0.05, "midtone {}", at(h * 3 / 4, w / 2));
        assert!(lin.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn exif_travels_with_the_base_item() {
        let exif: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0";
        let (file, ..) = build(16, 16, 90, &EncodeMeta { exif: Some(exif), ..Default::default() }).unwrap();
        assert_eq!(decode(&file, DecodeOptions::default()).unwrap().exif.as_deref(), Some(exif));
        assert!(gainmap::read_avif(&file).is_some());
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let (file, ..) = build(16, 16, 60, &EncodeMeta::default()).unwrap();
        let opts = DecodeOptions::default();
        // every truncation of the container and the AV1 data
        for n in 0..file.len() {
            let _ = decode(&file[..n], opts);
            let _ = gainmap::read_avif(&file[..n]);
        }
        // byte flips through the whole file
        for i in 0..file.len() {
            for x in [0x01u8, 0x80, 0xff] {
                let mut m = file.clone();
                m[i] ^= x;
                let _ = decode(&m, opts);
                let _ = gainmap::read_avif(&m);
            }
        }
    }
}
