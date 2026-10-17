//! AVIF (AV1 Image File Format), read-only.
//!
//! The container is HEIF (ISO/IEC 23008-12) over ISO-BMFF; we read the boxes ourselves (`meta`,
//! `pitm`, `iloc`, `iinf`, `iref`, `iprp`/`ipco`/`ipma`, `idat`), and the AV1 still picture is
//! decoded by `filmcraft-av1`, a clean-room pure-Rust AV1 decoder (no `unsafe`). Supported: the
//! primary item as one `av01` image or a `grid` of them; 8-, 10- and 12-bit; 4:2:0, 4:2:2, 4:4:4
//! and monochrome; full or limited range; the `nclx` colour description (or the AV1 sequence
//! header's) and an ICC profile (`prof` / `rICC`); `clap`, `irot` and `imir` are applied (so the
//! reported orientation is 1, as for HEIC); EXIF and XMP items. PQ and HLG files decode to linear
//! light with SDR white (203 cd/m²) at 1.0, highlights above it kept. Alpha (an auxiliary image)
//! is ignored: the colour image is shown as if over black, as Lightroom shows it. EXIF and XMP come
//! from `lcc_meta` (`crates/meta/src/heif.rs`), the reader import uses. Native only: on wasm32 a
//! panic aborts instead of unwinding, so the web build refuses AVIF rather than risk the decoder.
//!
//! Sources: ISO/IEC 23008-12 (HEIF) and the AV1 Image File Format specification v1.1.0 (box
//! layouts), ITU-T H.273 (colour code points), ITU-R BT.2100 (PQ / HLG); own design.

use crate::convert::{Buf, Meta, Model, Raw, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Avif;
/// Deepest box nesting we follow (meta → iprp → ipco → property is 4).
const MAX_DEPTH: u32 = 8;
/// Most grid tiles / items / properties we accept: far above real files, bounds hostile ones.
const MAX_ITEMS: usize = 1 << 16;

fn bad(m: impl Into<String>) -> Error {
    Error::Malformed(F, m.into())
}

/// A big-endian reader over a byte slice that never panics.
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| bad("box overflows"))?;
        let s = self.b.get(self.p..end).ok_or_else(|| bad("box truncated"))?;
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// An unsigned big-endian number of `n` (0, 1, 2, 4 or 8) bytes.
    fn uint(&mut self, n: usize) -> Result<u64> {
        if n > 8 {
            return Err(bad("field too wide"));
        }
        Ok(self.take(n)?.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
    }
    fn cstr(&mut self) -> Result<&'a [u8]> {
        let rest = self.b.get(self.p..).unwrap_or(&[]);
        let n = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        let s = self.take(n)?;
        if self.p < self.b.len() {
            self.p += 1;
        }
        Ok(s)
    }
    fn rest(&self) -> &'a [u8] {
        self.b.get(self.p..).unwrap_or(&[])
    }
}

/// The child boxes of `b`: (type, payload).
fn boxes(b: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        let mut r = Rd::new(b.get(p..).unwrap_or(&[]));
        if r.b.len() < 8 {
            break; // trailing padding
        }
        let size32 = r.u32()?;
        let t = r.take(4)?;
        let ty = [t[0], t[1], t[2], t[3]];
        let (hdr, size) = match size32 {
            0 => (8usize, r.b.len() as u64),
            1 => (16, r.uint(8)?),
            s => (8, u64::from(s)),
        };
        let size = usize::try_from(size).map_err(|_| bad("box too large"))?;
        if size < hdr || size > r.b.len() {
            return Err(bad(format!("box {} has a bad size", String::from_utf8_lossy(&ty))));
        }
        out.push((ty, r.b.get(hdr..size).unwrap_or(&[])));
        if out.len() > MAX_ITEMS {
            return Err(bad("too many boxes"));
        }
        p = p.checked_add(size).ok_or_else(|| bad("box overflows"))?;
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
struct Extent {
    offset: u64,
    length: u64,
}

#[derive(Clone, Debug, Default)]
struct Loc {
    method: u8,
    base: u64,
    extents: Vec<Extent>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Nclx {
    primaries: u16,
    transfer: u16,
    matrix: u16,
    full_range: bool,
}

#[derive(Clone, Copy, Debug)]
enum Transform {
    /// Clean aperture: width, height, horizontal and vertical offset of the centre (rationals as f64).
    Clap { w: f64, h: f64, dx: f64, dy: f64 },
    /// Anticlockwise rotation by `n` × 90°.
    Rot(u8),
    /// Mirror: 0 = about a vertical axis (left ↔ right), 1 = about a horizontal axis (top ↔ bottom).
    Mir(u8),
}

#[derive(Clone, Debug)]
enum Prop {
    Av1c(Vec<u8>),
    Nclx(Nclx),
    Icc(Vec<u8>),
    Ispe(u32, u32),
    T(Transform),
    Other,
}

#[derive(Default)]
struct Container<'a> {
    file: &'a [u8],
    primary: u32,
    types: Vec<(u32, [u8; 4], Option<String>)>,
    locs: Vec<(u32, Loc)>,
    idat: &'a [u8],
    props: Vec<Prop>,
    /// item → 1-based property indices, in order.
    assoc: Vec<(u32, Vec<usize>)>,
    /// (type, from, to…)
    refs: Vec<([u8; 4], u32, Vec<u32>)>,
}

fn parse(file: &[u8]) -> Result<Container<'_>> {
    let top = boxes(file)?;
    let meta = top.iter().find(|(t, _)| t == b"meta").map(|(_, b)| *b).ok_or_else(|| {
        if top.iter().any(|(t, _)| t == b"moov") { Error::Unsupported(F, "an AVIF image sequence (animated AVIF)") } else { bad("no meta box") }
    })?;
    let mut c = Container { file, ..Default::default() };
    let meta = meta.get(4..).ok_or_else(|| bad("meta box truncated"))?;
    for (t, b) in boxes(meta)? {
        let mut r = Rd::new(b);
        match &t {
            b"pitm" => {
                let v = r.u8()?;
                r.take(3)?;
                c.primary = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
            }
            b"iloc" => parse_iloc(&mut r, &mut c)?,
            b"iinf" => {
                let v = r.u8()?;
                r.take(3)?;
                let _count = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
                for (t, b) in boxes(r.rest())? {
                    if &t != b"infe" {
                        continue;
                    }
                    let mut r = Rd::new(b);
                    let v = r.u8()?;
                    r.take(3)?;
                    if v < 2 {
                        continue;
                    }
                    let id = if v == 2 { u32::from(r.u16()?) } else { r.u32()? };
                    r.u16()?; // protection index
                    let ty = r.take(4)?;
                    let ty = [ty[0], ty[1], ty[2], ty[3]];
                    let _name = r.cstr()?;
                    let mime = (&ty == b"mime").then(|| r.cstr().map(|s| String::from_utf8_lossy(s).into_owned()).ok()).flatten();
                    c.types.push((id, ty, mime));
                    if c.types.len() > MAX_ITEMS {
                        return Err(bad("too many items"));
                    }
                }
            }
            b"iref" => {
                let v = r.u8()?;
                r.take(3)?;
                for (t, b) in boxes(r.rest())? {
                    let mut r = Rd::new(b);
                    let id = |r: &mut Rd| if v == 0 { r.u16().map(u32::from) } else { r.u32() };
                    let from = id(&mut r)?;
                    let n = r.u16()?;
                    let mut to = Vec::with_capacity(usize::from(n));
                    for _ in 0..n {
                        to.push(id(&mut r)?);
                    }
                    c.refs.push((t, from, to));
                }
            }
            b"idat" => c.idat = b,
            b"iprp" => parse_iprp(b, &mut c, 0)?,
            _ => {}
        }
    }
    if c.primary == 0 {
        return Err(bad("no primary item"));
    }
    Ok(c)
}

fn parse_iloc(r: &mut Rd, c: &mut Container) -> Result<()> {
    let v = r.u8()?;
    r.take(3)?;
    if v > 2 {
        return Err(Error::Unsupported(F, "iloc version"));
    }
    let a = r.u8()?;
    let b = r.u8()?;
    let (off_size, len_size, base_size) = (usize::from(a >> 4), usize::from(a & 15), usize::from(b >> 4));
    let index_size = if v >= 1 { usize::from(b & 15) } else { 0 };
    let count = if v < 2 { u32::from(r.u16()?) } else { r.u32()? };
    for _ in 0..count {
        let id = if v < 2 { u32::from(r.u16()?) } else { r.u32()? };
        let method = if v >= 1 { (r.u16()? & 15) as u8 } else { 0 };
        r.u16()?; // data reference index
        let base = r.uint(base_size)?;
        let n = r.u16()?;
        let mut extents = Vec::with_capacity(usize::from(n).min(64));
        for _ in 0..n {
            r.uint(index_size)?;
            let offset = r.uint(off_size)?;
            let length = r.uint(len_size)?;
            extents.push(Extent { offset, length });
        }
        c.locs.push((id, Loc { method, base, extents }));
        if c.locs.len() > MAX_ITEMS {
            return Err(bad("too many items"));
        }
    }
    Ok(())
}

fn parse_iprp(b: &[u8], c: &mut Container, depth: u32) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(bad("boxes nested too deep"));
    }
    for (t, b) in boxes(b)? {
        match &t {
            b"ipco" => {
                for (t, b) in boxes(b)? {
                    c.props.push(parse_prop(&t, b)?);
                    if c.props.len() > MAX_ITEMS {
                        return Err(bad("too many properties"));
                    }
                }
            }
            b"ipma" => {
                let mut r = Rd::new(b);
                let v = r.u8()?;
                let flags = r.take(3)?;
                let wide = flags.get(2).is_some_and(|f| f & 1 != 0);
                let n = r.u32()?;
                for _ in 0..n {
                    let id = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
                    let k = r.u8()?;
                    let mut list = Vec::with_capacity(usize::from(k));
                    for _ in 0..k {
                        let idx = if wide { usize::from(r.u16()? & 0x7fff) } else { usize::from(r.u8()? & 0x7f) };
                        list.push(idx);
                    }
                    c.assoc.push((id, list));
                    if c.assoc.len() > MAX_ITEMS {
                        return Err(bad("too many associations"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_prop(t: &[u8; 4], b: &[u8]) -> Result<Prop> {
    let mut r = Rd::new(b);
    Ok(match t {
        b"av1C" => Prop::Av1c(b.get(4..).unwrap_or(&[]).to_vec()),
        b"colr" => {
            let kind = r.take(4)?;
            match kind {
                b"nclx" => Prop::Nclx(Nclx { primaries: r.u16()?, transfer: r.u16()?, matrix: r.u16()?, full_range: r.u8()? & 0x80 != 0 }),
                b"prof" | b"rICC" => Prop::Icc(r.rest().to_vec()),
                _ => Prop::Other,
            }
        }
        b"ispe" => {
            r.take(4)?;
            Prop::Ispe(r.u32()?, r.u32()?)
        }
        b"irot" => Prop::T(Transform::Rot(r.u8()? & 3)),
        b"imir" => Prop::T(Transform::Mir(r.u8()? & 1)),
        b"clap" => {
            let mut q = || -> Result<f64> {
                let n = r.u32()? as i32;
                let d = r.u32()? as i32;
                if d == 0 {
                    return Err(bad("clap with a zero denominator"));
                }
                Ok(f64::from(n) / f64::from(d))
            };
            let (w, h, dx, dy) = (q()?, q()?, q()?, q()?);
            Prop::T(Transform::Clap { w, h, dx, dy })
        }
        _ => Prop::Other,
    })
}

impl Container<'_> {
    fn item_type(&self, id: u32) -> Option<[u8; 4]> {
        self.types.iter().find(|t| t.0 == id).map(|t| t.1)
    }
    fn props_of(&self, id: u32) -> impl Iterator<Item = &Prop> {
        self.assoc.iter().filter(move |a| a.0 == id).flat_map(|a| a.1.iter()).filter_map(|&i| i.checked_sub(1).and_then(|i| self.props.get(i)))
    }
    fn data(&self, id: u32) -> Result<Vec<u8>> {
        let loc = self.locs.iter().find(|l| l.0 == id).map(|l| &l.1).ok_or_else(|| bad(format!("item {id} has no location")))?;
        let src = match loc.method {
            0 => self.file,
            1 => self.idat,
            _ => return Err(Error::Unsupported(F, "item data by reference to another item")),
        };
        let mut out = Vec::new();
        for e in &loc.extents {
            let start = loc.base.checked_add(e.offset).ok_or_else(|| bad("item offset overflows"))?;
            let start = usize::try_from(start).map_err(|_| bad("item offset out of range"))?;
            let end = if e.length == 0 {
                src.len()
            } else {
                usize::try_from(e.length).ok().and_then(|l| start.checked_add(l)).ok_or_else(|| bad("item length overflows"))?
            };
            out.extend_from_slice(src.get(start..end).ok_or_else(|| bad("item data runs past the end of the file"))?);
        }
        Ok(out)
    }
    fn refs_from(&self, kind: &[u8; 4], from: u32) -> Vec<u32> {
        self.refs.iter().filter(|r| &r.0 == kind && r.1 == from).flat_map(|r| r.2.iter().copied()).collect()
    }
    fn ispe(&self, id: u32) -> Option<(u32, u32)> {
        self.props_of(id).find_map(|p| if let Prop::Ispe(w, h) = p { Some((*w, *h)) } else { None })
    }
    fn transforms(&self, id: u32) -> Vec<Transform> {
        self.props_of(id).filter_map(|p| if let Prop::T(t) = p { Some(*t) } else { None }).collect()
    }
}

/// The size after `clap` / `irot`, from a stored size.
fn displayed(mut w: u32, mut h: u32, ts: &[Transform]) -> (u32, u32) {
    for t in ts {
        match *t {
            Transform::Clap { w: cw, h: ch, .. } => {
                if cw >= 1.0 && ch >= 1.0 {
                    w = (cw.round() as u32).min(w).max(1);
                    h = (ch.round() as u32).min(h).max(1);
                }
            }
            Transform::Rot(n) if n % 2 == 1 => std::mem::swap(&mut w, &mut h),
            _ => {}
        }
    }
    (w, h)
}

fn primary_size(c: &Container) -> Result<(u32, u32)> {
    let id = c.primary;
    match c.item_type(id) {
        Some(t) if &t == b"av01" || &t == b"grid" => {}
        Some(_) => return Err(Error::Unsupported(F, "the primary item is not an AV1 image or a grid")),
        None => return Err(bad("the primary item is not described")),
    }
    let (w, h) = match c.ispe(id) {
        Some(s) => s,
        None if c.item_type(id) == Some(*b"grid") => grid_header(&c.data(id)?)?.2,
        None => return Err(bad("the primary item has no size (ispe)")),
    };
    if w == 0 || h == 0 {
        return Err(bad("zero dimension"));
    }
    Ok((w, h))
}

/// The displayed size (container transforms applied, so orientation 1), from the boxes alone.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let c = parse(bytes)?;
    let (w, h) = primary_size(&c)?;
    let (w, h) = displayed(w, h, &c.transforms(c.primary));
    Ok((w, h, 1))
}

/// (rows, columns, output size) of a `grid` item.
fn grid_header(d: &[u8]) -> Result<(u32, u32, (u32, u32))> {
    let mut r = Rd::new(d);
    r.u8()?; // version
    let flags = r.u8()?;
    let rows = u32::from(r.u8()?) + 1;
    let cols = u32::from(r.u8()?) + 1;
    let n = if flags & 1 != 0 { 4 } else { 2 };
    let w = r.uint(n)? as u32;
    let h = r.uint(n)? as u32;
    Ok((rows, cols, (w, h)))
}

/// One decoded AV1 picture with the colour facts we need.
struct Pic {
    w: usize,
    h: usize,
    bits: u8,
    ssx: usize,
    ssy: usize,
    mono: bool,
    planes: [Vec<u16>; 3],
    nclx: Nclx,
}

fn decode_av1(c: &Container, id: u32) -> Result<Pic> {
    let data = c.data(id)?;
    let mut dec = filmcraft_av1::Decoder::new();
    let e = |e: filmcraft_av1::Error| match e {
        filmcraft_av1::Error::Unsupported(w) => Error::Unsupported(F, w),
        other => bad(format!("AV1: {other}")),
    };
    // The sequence header normally travels in the item too; the av1C copy covers files where it doesn't.
    let config = c.props_of(id).find_map(|p| if let Prop::Av1c(o) = p { Some(o.clone()) } else { None });
    let mut pics = Vec::new();
    if let Some(cfg) = config.filter(|o| !o.is_empty()) {
        pics.extend(dec.decode(&cfg).map_err(e)?);
    }
    pics.extend(dec.decode(&data).map_err(e)?);
    pics.extend(dec.flush_result().map_err(e)?);
    let p = pics.into_iter().next().ok_or_else(|| bad("the AV1 data holds no picture"))?;
    let (w, h) = (p.width as usize, p.height as usize);
    let (ssx, ssy) = (usize::from(p.subsampling_x.min(1)), usize::from(p.subsampling_y.min(1)));
    let need = w.checked_mul(h).ok_or_else(|| bad("picture too large"))?;
    let cneed = ((w + ssx) >> ssx) * ((h + ssy) >> ssy);
    if need == 0 || p.planes[0].len() < need || (!p.mono_chrome && (p.planes[1].len() < cneed || p.planes[2].len() < cneed)) {
        return Err(bad("AV1 picture planes are short"));
    }
    let nclx = Nclx {
        primaries: u16::from(p.color_primaries),
        transfer: u16::from(p.transfer_characteristics),
        matrix: u16::from(p.matrix_coefficients),
        full_range: p.full_range,
    };
    Ok(Pic { w, h, bits: p.bit_depth.clamp(8, 16), ssx, ssy, mono: p.mono_chrome, planes: p.planes, nclx })
}

/// Kr, Kb of an H.273 matrix code point; `None` = identity (GBR).
fn coefficients(matrix: u16) -> Option<(f32, f32)> {
    match matrix {
        0 => None,
        1 => Some((0.2126, 0.0722)),
        4 => Some((0.30, 0.11)),
        7 => Some((0.212, 0.087)),
        9 | 10 => Some((0.2627, 0.0593)),
        // 5, 6 (BT.601), 2 (unspecified: AVIF readers assume BT.601) and the rest
        _ => Some((0.299, 0.114)),
    }
}

/// A chroma sample at luma position (x, y), bilinear between chroma sites centred on their luma pairs.
#[inline]
fn chroma_at(plane: &[u16], cw: usize, ch: usize, ssx: usize, ssy: usize, x: usize, y: usize) -> f32 {
    let pos = |v: usize, ss: usize, n: usize| -> (usize, usize, f32) {
        if ss == 0 {
            return (v.min(n - 1), v.min(n - 1), 0.0);
        }
        let f = (v as f32 + 0.5) / 2.0 - 0.5;
        let f = f.max(0.0);
        let i = f as usize;
        let i0 = i.min(n - 1);
        (i0, (i + 1).min(n - 1), f - i as f32)
    };
    let (x0, x1, fx) = pos(x, ssx, cw);
    let (y0, y1, fy) = pos(y, ssy, ch);
    let s = |xx: usize, yy: usize| f32::from(plane.get(yy * cw + xx).copied().unwrap_or(0));
    let top = s(x0, y0) * (1.0 - fx) + s(x1, y0) * fx;
    let bot = s(x0, y1) * (1.0 - fx) + s(x1, y1) * fx;
    top * (1.0 - fy) + bot * fy
}

/// YCbCr → non-linear R'G'B' 0..1 (may slightly exceed it), interleaved.
fn to_rgb(p: &Pic, nclx: Nclx) -> Vec<f32> {
    let max = ((1u32 << p.bits) - 1) as f32;
    let scale = f32::from(1u16 << (p.bits - 8).min(8));
    let (yoff, yrange, crange) = if nclx.full_range { (0.0, max, max) } else { (16.0 * scale, 219.0 * scale, 224.0 * scale) };
    let mid = f32::from(1u16 << (p.bits - 1).min(15));
    let (cw, ch) = ((p.w + p.ssx) >> p.ssx, (p.h + p.ssy) >> p.ssy);
    let k = coefficients(nclx.matrix);
    let mut out = vec![0f32; p.w * p.h * 3];
    for y in 0..p.h {
        for x in 0..p.w {
            let i = y * p.w + x;
            let luma = (f32::from(p.planes[0].get(i).copied().unwrap_or(0)) - yoff) / yrange;
            let (cb, cr) = if p.mono {
                (0.0, 0.0)
            } else {
                (
                    (chroma_at(&p.planes[1], cw, ch, p.ssx, p.ssy, x, y) - mid) / crange,
                    (chroma_at(&p.planes[2], cw, ch, p.ssx, p.ssy, x, y) - mid) / crange,
                )
            };
            let rgb = match k {
                // identity: Y = G, Cb = B, Cr = R (no offsets on the "chroma" planes)
                None => {
                    let g = luma;
                    let b = cb + mid / crange - yoff / yrange;
                    let r = cr + mid / crange - yoff / yrange;
                    [r, g, b]
                }
                Some((kr, kb)) => {
                    let kg = 1.0 - kr - kb;
                    let r = luma + 2.0 * (1.0 - kr) * cr;
                    let b = luma + 2.0 * (1.0 - kb) * cb;
                    let g = (luma - kr * r - kb * b) / kg;
                    [r, g, b]
                }
            };
            out[i * 3..i * 3 + 3].copy_from_slice(&rgb);
        }
    }
    out
}

/// Apply the container's transforms in order to an interleaved 3-channel image.
fn apply_transforms(mut v: Vec<f32>, mut w: usize, mut h: usize, ts: &[Transform]) -> (Vec<f32>, usize, usize) {
    for t in ts {
        match *t {
            Transform::Clap { w: cw, h: ch, dx, dy } => {
                if !(cw >= 1.0 && ch >= 1.0 && cw.is_finite() && ch.is_finite() && dx.is_finite() && dy.is_finite()) {
                    continue;
                }
                let nw = (cw.round() as usize).clamp(1, w);
                let nh = (ch.round() as usize).clamp(1, h);
                // centre of the aperture = image centre + offset
                let x0 = ((w as f64 - 1.0) / 2.0 + dx - (nw as f64 - 1.0) / 2.0).round().clamp(0.0, (w - nw) as f64) as usize;
                let y0 = ((h as f64 - 1.0) / 2.0 + dy - (nh as f64 - 1.0) / 2.0).round().clamp(0.0, (h - nh) as f64) as usize;
                let mut o = Vec::with_capacity(nw * nh * 3);
                for y in y0..y0 + nh {
                    o.extend_from_slice(&v[(y * w + x0) * 3..(y * w + x0 + nw) * 3]);
                }
                (v, w, h) = (o, nw, nh);
            }
            Transform::Rot(n) => {
                for _ in 0..n {
                    // 90° anticlockwise: (x, y) → (y, w-1-x), new size h × w
                    let mut o = vec![0f32; v.len()];
                    for y in 0..h {
                        for x in 0..w {
                            let (nx, ny) = (y, w - 1 - x);
                            let d = (ny * h + nx) * 3;
                            let s = (y * w + x) * 3;
                            o[d..d + 3].copy_from_slice(&v[s..s + 3]);
                        }
                    }
                    (v, w, h) = (o, h, w);
                }
            }
            Transform::Mir(axis) => {
                for y in 0..h {
                    for x in 0..w {
                        let (sx, sy) = if axis == 0 { (w - 1 - x, y) } else { (x, h - 1 - y) };
                        if (sy * w + sx) > (y * w + x) {
                            for c in 0..3 {
                                v.swap((y * w + x) * 3 + c, (sy * w + sx) * 3 + c);
                            }
                        }
                    }
                }
            }
        }
    }
    (v, w, h)
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let c = parse(bytes)?;
    let (sw, sh) = primary_size(&c)?;
    if u64::from(sw).saturating_mul(u64::from(sh)) > opts.max_pixels {
        return Err(Error::TooLarge(u64::from(sw), u64::from(sh)));
    }
    let id = c.primary;
    let file_nclx = c.props_of(id).find_map(|p| if let Prop::Nclx(n) = p { Some(*n) } else { None });
    let icc = c.props_of(id).find_map(|p| if let Prop::Icc(b) = p { Some(b.clone()) } else { None });
    let check = |p: &Pic| -> Result<()> {
        if (p.w as u64).saturating_mul(p.h as u64) > opts.max_pixels {
            return Err(Error::TooLarge(p.w as u64, p.h as u64));
        }
        Ok(())
    };

    let (rgb, w, h, bits, nclx) = if c.item_type(id) == Some(*b"grid") {
        let (rows, cols, (gw, gh)) = grid_header(&c.data(id)?)?;
        let tiles = c.refs_from(b"dimg", id);
        if tiles.len() as u64 != u64::from(rows) * u64::from(cols) || tiles.is_empty() {
            return Err(bad("grid tile count doesn't match its rows and columns"));
        }
        let (gw, gh) = (gw as usize, gh as usize);
        if gw == 0 || gh == 0 || (gw as u64).saturating_mul(gh as u64) > opts.max_pixels {
            return Err(Error::TooLarge(gw as u64, gh as u64));
        }
        let mut out = vec![0f32; gw * gh * 3];
        let (mut tw, mut th, mut bits, mut nclx) = (0usize, 0usize, 8u8, None);
        for (i, &t) in tiles.iter().enumerate() {
            let p = decode_av1(&c, t)?;
            check(&p)?;
            if i == 0 {
                (tw, th, bits) = (p.w, p.h, p.bits);
                nclx = Some(file_nclx.unwrap_or(p.nclx));
            } else if (p.w, p.h) != (tw, th) {
                return Err(bad("grid tiles differ in size"));
            }
            let Some(n) = nclx else { return Err(bad("grid without tiles")) };
            let tile = to_rgb(&p, n);
            let (r, col) = (i / cols as usize, i % cols as usize);
            for y in 0..th {
                let oy = r * th + y;
                if oy >= gh {
                    break;
                }
                for x in 0..tw {
                    let ox = col * tw + x;
                    if ox >= gw {
                        break;
                    }
                    let (d, s) = ((oy * gw + ox) * 3, (y * tw + x) * 3);
                    out[d..d + 3].copy_from_slice(&tile[s..s + 3]);
                }
            }
        }
        let n = nclx.ok_or_else(|| bad("grid without tiles"))?;
        (out, gw, gh, bits, n)
    } else {
        let p = decode_av1(&c, id)?;
        check(&p)?;
        let n = file_nclx.unwrap_or(p.nclx);
        (to_rgb(&p, n), p.w, p.h, p.bits, n)
    };
    let (rgb, w, h) = apply_transforms(rgb, w, h, &c.transforms(id));

    // EXIF and XMP: the same reader the catalogue uses (`lcc_meta`'s HEIF items, iPhone prefixes and
    // grid tiles included), so a decode and an import agree.
    let lcc_meta::Embedded { exif, xmp, .. } = lcc_meta::embedded(bytes);

    let hdr = icc.is_none() && matches!(nclx.transfer, 16 | 18);
    let (buf, meta) = if hdr {
        // PQ / HLG: linear light with SDR white at 1.0, kept above it.
        let k = coefficients(9).unwrap_or((0.2627, 0.0593));
        let lin: Vec<f32> = if nclx.transfer == 16 {
            rgb.iter().map(|&e| crate::encode::pq_decode(e) / crate::encode::HDR_REFERENCE_WHITE_NITS).collect()
        } else {
            let mut v = Vec::with_capacity(rgb.len());
            for c3 in rgb.as_chunks::<3>().0 {
                // inverse OETF, then the OOTF (system gamma 1.2 on luminance), display light / SDR white
                let s = [c3[0], c3[1], c3[2]].map(|e| lcc_color::transfer::hlg_decode(f64::from(e.clamp(0.0, 1.0))) as f32);
                let ys = k.0 * s[0] + (1.0 - k.0 - k.1) * s[1] + k.1 * s[2];
                let g = if ys > 0.0 { ys.powf(0.2) } else { 0.0 };
                let f = crate::encode::HLG_REFERENCE_PEAK_NITS / crate::encode::HDR_REFERENCE_WHITE_NITS;
                v.extend(s.map(|e| e * g * f));
            }
            v
        };
        let space = crate::space::SourceSpace::from_cicp(nclx.primaries, 8)
            .or_else(|| crate::space::SourceSpace::from_cicp(9, 8))
            .ok_or_else(|| bad("no linear colour space"))?;
        (Buf::F32(lin), Meta { exif, xmp, orientation: Some(1), already_linear: Some(space), ..Default::default() })
    } else {
        let v: Vec<u16> = rgb.iter().map(|&e| (e.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect();
        let hint = crate::space::SourceSpace::from_cicp(nclx.primaries, nclx.transfer);
        (Buf::U16(v), Meta { icc, exif, xmp, orientation: Some(1), hint, ..Default::default() })
    };
    let raw = Raw { width: w, height: h, model: Model::Rgb, alpha: false, premultiplied: false, buf, bit_depth: bits };
    let (dw, dh) = displayed(sw, sh, &c.transforms(id));
    finish(F, raw, meta, (dw, dh), opts)
}

#[cfg(all(test, feature = "avif", not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{EncodeImage, EncodeMeta, HdrTransfer, Samples, encode_avif, encode_avif_hdr};

    /// A smooth sRGB test card with a saturated band, 8-bit RGB.
    fn card(w: usize, h: usize) -> Vec<u8> {
        (0..w * h).flat_map(|i| [(i % w * 255 / (w - 1)) as u8, (i / w * 255 / (h - 1)) as u8, if i % w < w / 4 { 230 } else { 40 }]).collect()
    }

    fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
        a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs()).sum::<f64>() / a.len() as f64
    }

    fn rgb8(d: &Decoded) -> Vec<u8> {
        d.to_srgb8().data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
    }

    #[test]
    fn eight_bit_round_trip_with_exif() {
        let (w, h) = (48usize, 32usize);
        let px = card(w, h);
        let exif: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0";
        let meta = EncodeMeta { exif: Some(exif), ..Default::default() };
        let file = encode_avif(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&px)), 95, 8, &meta).unwrap();
        assert_eq!(crate::read_header(&file).unwrap().width, w as u32);
        let d = crate::decode(&file, crate::DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height, d.orientation, d.format), (w as u32, h as u32, 1, Format::Avif));
        assert_eq!(d.exif.as_deref(), Some(exif));
        let diff = mean_abs_diff(&px, &rgb8(&d));
        assert!(diff < 4.0, "mean difference {diff}");
    }

    #[test]
    fn ten_bit_round_trip() {
        let (w, h) = (40usize, 24usize);
        let px: Vec<u16> = card(w, h).iter().map(|&v| u16::from(v) * 257).collect();
        let file = encode_avif(&EncodeImage::new(w as u32, h as u32, 3, Samples::U16(&px)), 95, 8, &EncodeMeta::default()).unwrap();
        let d = crate::decode(&file, crate::DecodeOptions::default()).unwrap();
        assert_eq!(d.bit_depth, 10);
        let diff = mean_abs_diff(&card(w, h), &rgb8(&d));
        assert!(diff < 4.0, "mean difference {diff}");
    }

    #[test]
    fn hdr_round_trips_keep_highlights_above_white() {
        let (w, h) = (32usize, 16usize);
        // linear Rec. 2020: left half grey 0.18, right half 4 × SDR white
        let rgb: Vec<f32> = (0..w * h).flat_map(|i| if i % w < w / 2 { [0.18f32; 3] } else { [4.0; 3] }).collect();
        for t in [HdrTransfer::Pq, HdrTransfer::Hlg] {
            let file = encode_avif_hdr(w as u32, h as u32, &rgb, 98, 8, &EncodeMeta::default(), t).unwrap();
            let d = crate::decode(&file, crate::DecodeOptions::default()).unwrap();
            let lin = d.to_working();
            let at = |x: usize| lin.data[8 * w + x][1];
            assert!((at(4) / 0.18 - 1.0).abs() < 0.08, "{t:?}: grey {}", at(4));
            assert!((at(w - 4) / 4.0 - 1.0).abs() < 0.08, "{t:?}: highlight {}", at(w - 4));
        }
    }

    #[test]
    fn downscaled_and_size_limited_decodes() {
        let px = card(64, 64);
        let file = encode_avif(&EncodeImage::new(64, 64, 3, Samples::U8(&px)), 80, 10, &EncodeMeta::default()).unwrap();
        let d = crate::decode(&file, crate::DecodeOptions::fit(16, 16)).unwrap();
        assert_eq!((d.width, d.height, d.source_width), (16, 16, 64));
        let small = crate::DecodeOptions { max_pixels: 100, ..Default::default() };
        assert!(matches!(crate::decode(&file, small), Err(Error::TooLarge(64, 64))));
    }

    #[test]
    fn transforms_rotate_mirror_and_crop() {
        // 3 × 2 image, pixel value = index
        let v: Vec<f32> = (0..6).flat_map(|i| [i as f32; 3]).collect();
        let g = |v: &[f32]| v.chunks(3).map(|c| c[0] as u8).collect::<Vec<_>>();
        let (r, w, h) = apply_transforms(v.clone(), 3, 2, &[Transform::Rot(1)]);
        // anticlockwise: the right column becomes the top row
        assert_eq!((w, h, g(&r)), (2, 3, vec![2, 5, 1, 4, 0, 3]));
        let (m, ..) = apply_transforms(v.clone(), 3, 2, &[Transform::Mir(0)]);
        assert_eq!(g(&m), vec![2, 1, 0, 5, 4, 3]);
        let (m, ..) = apply_transforms(v.clone(), 3, 2, &[Transform::Mir(1)]);
        assert_eq!(g(&m), vec![3, 4, 5, 0, 1, 2]);
        let (c, w, h) = apply_transforms(v, 3, 2, &[Transform::Clap { w: 1.0, h: 1.0, dx: 1.0, dy: 0.0 }]);
        assert_eq!((w, h), (1, 1));
        assert!(g(&c)[0] == 2 || g(&c)[0] == 5);
        assert_eq!(displayed(3, 2, &[Transform::Rot(3)]), (2, 3));
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let px = card(16, 16);
        let file = encode_avif(&EncodeImage::new(16, 16, 3, Samples::U8(&px)), 60, 10, &EncodeMeta::default()).unwrap();
        // every truncation: the box parser never panics, and a decode is an error or an image
        for n in 0..file.len() {
            let _ = header(&file[..n]);
            let _ = crate::decode_unguarded(&file[..n], Format::Avif, &crate::DecodeOptions::default());
        }
        // byte flips through the container and the AV1 data
        for i in (0..file.len()).step_by(3) {
            for x in [0x01u8, 0x80, 0xff] {
                let mut m = file.clone();
                m[i] ^= x;
                let _ = header(&m);
                let _ = crate::decode(&m, crate::DecodeOptions::default());
            }
        }
        // box sizes that point far outside the file
        let mut b = b"\0\0\0\x14ftypavif\0\0\0\0avif".to_vec();
        b.extend_from_slice(b"\xff\xff\xff\xffmeta\0\0\0\0");
        assert!(crate::decode(&b, crate::DecodeOptions::default()).is_err());
        let mut b = b"\0\0\0\x14ftypavif\0\0\0\0avif".to_vec();
        b.extend_from_slice(b"\0\0\0\x01meta\xff\xff\xff\xff\xff\xff\xff\xff");
        assert!(header(&b).is_err());
    }

    #[test]
    fn an_animated_avif_is_unsupported() {
        let b = b"\0\0\0\x14ftypavis\0\0\0\0avis\0\0\0\x08moov";
        assert!(matches!(header(b), Err(Error::Unsupported(Format::Avif, _))));
    }
}
