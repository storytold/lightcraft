//! `lightcraft-heif`: the optional HEIF / HEIC (iPhone and Mac photo) decoder.
//!
//! `heic-rs`, a pure-Rust HEIF container and HEVC still-picture decoder, reads the container and
//! reconstructs the coded pictures (bit-exact against libde265 on iPhone photos); this crate turns
//! them into RGB itself ([`ycc`]) so the colours match libheif: it honours the HEVC VUI's range
//! and matrix ([`vui`]) where heic-rs assumes BT.709 limited range, and upsamples chroma like
//! libheif. Single pictures and grid-tiled photos, 8- to 16-bit, monochrome, alpha auxiliary
//! images, ICC, EXIF and XMP. The API is plain data ([`Info`], [`Decoded`], [`Error`]) so the
//! crate knows nothing about the rest of LightCraft; `lightcraft-codecs` adapts it behind its
//! `heif` feature. Shares heic-rs and its regression fixtures with PhotoCraft's `photocraft-heif`.
//!
//! HEIF records orientation in the container (`irot`/`imir`, plus a `clap` crop), not in EXIF.
//! [`Options::apply_transforms`] applies them; with it off, the pixels come back as coded.
//! Auxiliary images other than alpha (HDR gain maps, depth maps) are ignored.
//!
//! Never panics: heic-rs is young, so every call into it runs under `catch_unwind` and a panic
//! inside it becomes [`Error::Malformed`].

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod orient;
/// Lossless synthetic HEIF files for tests, here and in the crates above (feature `testdata`).
#[cfg(any(test, feature = "testdata"))]
#[doc(hidden)]
pub mod testdata;
mod vui;
mod ycc;

use std::borrow::Cow;

use heic_rs::context::Context;
use heic_rs::hevc::Frame;
use heic_rs::props::ItemProps;
use heic_rs::props::colr::Range;

/// What a HEIF file declares, read from the container alone (no pixel is decoded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// Size after the container transforms (rotation, mirror, crop): what a viewer shows.
    pub width: u32,
    pub height: u32,
    /// Size as coded, before the transforms.
    pub coded_width: u32,
    pub coded_height: u32,
    /// The primary image has an alpha auxiliary image.
    pub has_alpha: bool,
    /// Bits per sample as coded (8 for most photos, 10 for HDR-capable ones).
    pub bit_depth: u8,
}

/// Decode settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Refuse images with more pixels than this (checked before decoding).
    pub max_pixels: u64,
    /// Apply the container's rotation, mirror and crop (`irot`/`imir`/`clap`).
    pub apply_transforms: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_pixels: 1 << 28, apply_transforms: true }
    }
}

/// How the file describes its colour: the code points of ITU-T H.273, from the `colr` `nclx`
/// box, else the HEVC VUI, else the standard defaults (2 = unspecified, limited range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

/// A decoded image: interleaved RGB or RGBA, 16 bits per sample whatever the coded depth,
/// row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// Four channels (RGBA) instead of three (RGB).
    pub has_alpha: bool,
    /// Bits per sample as coded.
    pub bit_depth: u8,
    pub data: Vec<u16>,
    pub colour: Colour,
    /// The primary image's ICC profile.
    pub icc: Option<Vec<u8>>,
    /// EXIF as a TIFF structure (without HEIF's offset header). Its Orientation tag only mirrors
    /// what the container says; it is returned verbatim.
    pub exif: Option<Vec<u8>>,
    /// The XMP packet.
    pub xmp: Option<String>,
}

/// Why a file could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A valid file using something this decoder does not handle (image sequences, overlays…).
    Unsupported(&'static str),
    /// The image is larger than [`Options::max_pixels`] or holds an oversized box.
    Limit(String),
    /// Broken or truncated data (or a decoder bug, reported the same way).
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(m) => write!(f, "unsupported HEIF: {m}"),
            Error::Limit(m) => write!(f, "limit exceeded: {m}"),
            Error::Malformed(m) => write!(f, "malformed HEIF: {m}"),
        }
    }
}

impl std::error::Error for Error {}

const SEQUENCE: &str = "this file holds an image sequence; only HEIF still images can be opened";

/// heic-rs 0.1.1 reconstructs 4:2:0 and monochrome pictures exactly but fails on the 4:2:2 and
/// 4:4:4 ones some cameras write (Canon and Sony "HIF"); those failures are reported as this.
const CHROMA_422_444: &str = "4:2:2 and 4:4:4 HEIF images (as some Canon and Sony cameras write) can't be decoded yet";

pub(crate) fn err(e: heic_rs::Error) -> Error {
    match e {
        heic_rs::Error::Unsupported(what) => Error::Unsupported(what),
        // The primary item is neither an HEVC picture nor a grid of them.
        heic_rs::Error::MissingBox("hvcC") => Error::Unsupported("the image is an overlay, an identity derivation or not HEVC-coded"),
        heic_rs::Error::PixelLimit { .. } | heic_rs::Error::BoxTooLarge { .. } => Error::Limit(e.to_string()),
        e => Error::Malformed(e.to_string()),
    }
}

/// Runs `f`, turning a heic-rs panic into an error. Fuzzing in the sibling app found two
/// out-of-range slices in heic-rs 0.1.1 on malformed files: in its box parser (`boxes.rs:130`)
/// and during reconstruction (`hevc/decode/recon.rs:70`).
fn guarded<T>(f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(Error::Malformed("the HEIF decoder failed on this file".into())))
}

/// Reads the container's declared size, depth and alpha without decoding pixels, so callers can
/// check their limits first. Also refuses a file cut short: every coded picture of the primary
/// image must lie within `bytes`.
pub fn probe(bytes: &[u8]) -> Result<Info, Error> {
    guarded(|| probe_unguarded(bytes))
}

fn probe_unguarded(bytes: &[u8]) -> Result<Info, Error> {
    let info = heic_rs::probe(bytes).map_err(|e| match e {
        // No `meta` box: no still image, only a `moov` image sequence (a video track).
        heic_rs::Error::MissingBox("meta") => Error::Unsupported(SEQUENCE),
        e => err(e),
    })?;
    let ctx = Context::open(bytes).map_err(err)?;
    let id = ctx.meta.primary;
    let coded = match ctx.grid(id).map_err(err)? {
        Some((_, tiles)) => tiles,
        None => vec![id],
    };
    for item in coded {
        // Borrowed from `bytes` unless split across extents: cheap.
        ctx.item_data(item).map_err(err)?;
    }
    Ok(Info {
        width: info.width,
        height: info.height,
        coded_width: info.coded_width,
        coded_height: info.coded_height,
        has_alpha: info.has_alpha,
        bit_depth: info.bit_depth,
    })
}

/// Decodes the primary image, with its metadata.
pub fn decode(bytes: &[u8], options: &Options) -> Result<Decoded, Error> {
    guarded(|| decode_unguarded(bytes, options))
}

/// Decodes the smallest thumbnail image the file carries whose long edge is at least
/// `min_edge`, if any (iPhones store one of about 320 × 240). Transforms are applied as for the
/// primary image; the metadata (and, when the thumbnail has no profile of its own, the ICC
/// profile) are the primary image's.
pub fn decode_thumbnail(bytes: &[u8], min_edge: u32, options: &Options) -> Result<Option<Decoded>, Error> {
    guarded(|| {
        let ctx = Context::open(bytes).map_err(err)?;
        let primary = ctx.meta.primary;
        let mut best: Option<(u32, u32)> = None;
        for r in ctx.meta.refs.iter().filter(|r| r.kind == heic_rs::meta::iref::RefKind::Thmb && r.to.contains(&primary)) {
            let Ok(p) = ctx.props(r.from) else { continue };
            let Ok((w, h)) = ctx.coded_size(r.from, &p) else { continue };
            let edge = w.max(h);
            if edge >= min_edge && best.is_none_or(|(_, e)| edge < e) {
                best = Some((r.from, edge));
            }
        }
        let Some((id, _)) = best else { return Ok(None) };
        let mut decoded = decode_item(&ctx, id, options, false)?;
        if decoded.icc.is_none() {
            decoded.icc = item_icc(&ctx, primary);
        }
        (decoded.exif, decoded.xmp) = metadata(&ctx);
        Ok(Some(decoded))
    })
}

/// An item's ICC profile; a grid whose own properties carry none takes its first tile's (as on
/// some iPhones).
fn item_icc(ctx: &Context<'_>, id: u32) -> Option<Vec<u8>> {
    let p = ctx.props(id).ok()?;
    let icc = p.icc.or_else(|| {
        let (_, tiles) = ctx.grid(id).ok()??;
        ctx.props(*tiles.first()?).ok()?.icc
    });
    icc.map(<[u8]>::to_vec)
}

fn decode_unguarded(bytes: &[u8], options: &Options) -> Result<Decoded, Error> {
    // Refuses sequences with a clear message before anything else is read.
    probe_unguarded(bytes)?;
    let ctx = Context::open(bytes).map_err(err)?;
    let mut decoded = decode_item(&ctx, ctx.meta.primary, options, true)?;
    let (exif, xmp) = metadata(&ctx);
    decoded.exif = exif;
    decoded.xmp = xmp;
    Ok(decoded)
}

/// Decodes one image item (a coded picture or a grid of them) to RGB(A), alpha included when
/// `with_alpha` and the item has an alpha auxiliary image.
fn decode_item(ctx: &Context<'_>, id: u32, options: &Options, with_alpha: bool) -> Result<Decoded, Error> {
    let p = ctx.props(id).map_err(err)?;
    let limit = options.max_pixels;
    // The declared sizes are refused before anything is decoded or allocated.
    let (cw, ch) = ctx.coded_size(id, &p).map_err(err)?;
    heic_rs::image::check_pixels(cw, ch, limit).map_err(err)?;
    let (tw, th) = heic_rs::transform::transformed_size(cw, ch, &p.transforms).map_err(err)?;
    heic_rs::image::check_pixels(tw, th, limit).map_err(err)?;

    let coded = Coded::decode(ctx, id, &p, limit)?;
    let planes = coded.planes()?;
    let colour = colour(&p, &coded.first_props);
    let signal = ycc::Signal { matrix: colour.matrix, full_range: colour.full_range };
    let alpha_id = if with_alpha { ctx.alpha_item(id).map_err(err)? } else { None };
    let (w, h) = planes.size();
    let mode = if options.apply_transforms && orient::odd_crop(w, h, &p.transforms)? { ycc::Upsampling::Bilinear } else { ycc::Upsampling::Nearest };
    let mut data = ycc::to_rgb16(&planes, signal, alpha_id.is_some(), mode)
        .ok_or_else(|| Error::Malformed("the decoded pictures don't cover the image".into()))?;
    if let Some(aux) = alpha_id {
        let ap = ctx.props(aux).map_err(err)?;
        let alpha = Coded::decode(ctx, aux, &ap, limit)?;
        let alpha_planes = alpha.planes()?;
        if alpha_planes.size() != (w, h) {
            return Err(Error::Unsupported("an alpha image of a different size than the picture"));
        }
        ycc::put_alpha(&alpha_planes, &mut data).ok_or_else(|| Error::Malformed("the alpha image is incomplete".into()))?;
    }
    let bit_depth = planes.bit_depth();
    let has_alpha = alpha_id.is_some();
    let mut img = orient::Pixels { width: w, height: h, ch: if has_alpha { 4 } else { 3 }, data };
    if options.apply_transforms {
        img = orient::apply(img, &p.transforms)?;
    }
    Ok(Decoded {
        width: u32::try_from(img.width).map_err(|_| Error::Limit("image too wide".into()))?,
        height: u32::try_from(img.height).map_err(|_| Error::Limit("image too tall".into()))?,
        has_alpha,
        bit_depth,
        data: img.data,
        colour,
        // A grid whose own properties carry no profile takes its tiles' (as on some iPhones).
        icc: p.icc.or(coded.first_props.icc).map(<[u8]>::to_vec),
        exif: None,
        xmp: None,
    })
}

/// The decoded pictures of an item: one, or a grid's tiles in `dimg` order.
struct Coded<'a> {
    frames: Vec<Frame>,
    grid: Option<heic_rs::grid::Grid>,
    /// The properties of the first coded picture (the item itself, or the first tile): where the
    /// HEVC configuration and its VUI live.
    first_props: ItemProps<'a>,
    /// A failed 4:2:2 / 4:4:4 picture, for the error message.
    chroma_format: u8,
}

impl<'a> Coded<'a> {
    fn decode(ctx: &Context<'a>, id: u32, p: &ItemProps<'a>, limit: u64) -> Result<Coded<'a>, Error> {
        let (grid, ids) = match ctx.grid(id).map_err(err)? {
            Some((g, tiles)) => (Some(g), tiles),
            None => (None, vec![id]),
        };
        let first = *ids.first().ok_or_else(|| Error::Malformed("grid lists no tiles".into()))?;
        let first_props = if first == id { p.clone() } else { ctx.props(first).map_err(err)? };
        let chroma_format = first_props.hvcc.as_ref().map_or(1, |c| c.chroma_format);
        let decode_one = |t: &u32| -> Result<Frame, Error> {
            let tp = ctx.props(*t).map_err(err)?;
            let hvcc = tp.hvcc.as_ref().ok_or(heic_rs::Error::MissingBox("hvcC")).map_err(err)?;
            let data = ctx.item_data(*t).map_err(err)?;
            let nals = hvcc.split_nals(&data).map_err(err)?;
            let params = hvcc.parameter_sets();
            if params.is_empty() {
                return Err(Error::Malformed("hvcC carries no parameter sets".into()));
            }
            let frame = heic_rs::hevc::decode_still(&params, &nals).map_err(err)?;
            frame.validate().map_err(err)?;
            heic_rs::image::check_pixels(frame.width, frame.height, limit).map_err(err)?;
            Ok(frame)
        };
        #[cfg(not(target_arch = "wasm32"))]
        let frames: Result<Vec<Frame>, Error> = {
            use rayon::prelude::*;
            ids.par_iter().map(decode_one).collect()
        };
        #[cfg(target_arch = "wasm32")]
        let frames: Result<Vec<Frame>, Error> = ids.iter().map(decode_one).collect();
        let frames = frames.map_err(|e| match e {
            Error::Malformed(_) if chroma_format >= 2 => Error::Unsupported(CHROMA_422_444),
            e => e,
        })?;
        Ok(Coded { frames, grid, first_props, chroma_format })
    }

    fn planes(&self) -> Result<ycc::Planes<'_>, Error> {
        match (&self.grid, self.frames.as_slice()) {
            (Some(g), tiles) => heic_rs::grid::Mosaic::new(g, tiles).map(ycc::Planes::Grid).map_err(err),
            (None, [frame]) => Ok(ycc::Planes::Picture(frame)),
            (None, _) => Err(Error::Malformed(format!("expected one coded picture (chroma format {})", self.chroma_format))),
        }
    }
}

/// The colour description in force: `nclx` on the item (or, for a grid without one, on its
/// first tile), else the VUI of its (first) coded picture, else the H.265 defaults.
fn colour(p: &ItemProps<'_>, coded: &ItemProps<'_>) -> Colour {
    if let Some(n) = p.nclx.or(coded.nclx) {
        return Colour { primaries: n.primaries, transfer: n.transfer, matrix: n.matrix_code, full_range: n.range == Range::Full };
    }
    let s = coded.hvcc.as_ref().and_then(|c| vui::signal(&c.parameter_sets())).unwrap_or_default();
    Colour { primaries: s.primaries.into(), transfer: s.transfer.into(), matrix: s.matrix.into(), full_range: s.full_range }
}

/// The primary image's EXIF (TIFF structure) and XMP packet. Metadata is a courtesy: a broken
/// metadata item never fails a decode whose pixels came out fine.
fn metadata(ctx: &Context<'_>) -> (Option<Vec<u8>>, Option<String>) {
    let id = ctx.meta.primary;
    let exif = ctx.exif(id).ok().flatten().map(<[u8]>::to_vec);
    let xmp = ctx.xmp_item(id).and_then(|x| ctx.item_data(x).ok()).and_then(|d| match d {
        Cow::Borrowed(b) => String::from_utf8(b.to_vec()).ok(),
        Cow::Owned(b) => String::from_utf8(b).ok(),
    });
    (exif, xmp.map(|x| x.trim_end_matches('\0').to_string()))
}

#[cfg(test)]
mod tests_synthetic;

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by PhotoCraft's `decode_heif` fuzz target: a malformed box makes heic-rs 0.1.1 slice
    /// out of range (`boxes.rs:130`). It must be an error, not a crash.
    const HEIC_RS_BOX_PANIC: [u8; 72] = [
        0x00, 0x00, 0x00, 0x24, 0x66, 0x74, 0x79, 0x70, 0x68, 0x65, 0x69, 0x63, 0x00, 0x00, 0x00, 0x00, 0x6d, 0x69, 0x66, 0x31, 0x4d, 0x69, 0x50,
        0x72, 0x6d, 0x69, 0x61, 0x66, 0x4d, 0x69, 0x48, 0x42, 0x72, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6d, 0x65, 0x74, 0x61, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x75, 0x75, 0x69, 0x64, 0x00, 0x00, 0x00, 0x00, 0x03, 0x08, 0x08, 0x08, 0x00, 0x03, 0x01, 0x03, 0x70,
        0x00, 0xa8, 0x00,
    ];

    #[test]
    fn a_heic_rs_panic_is_a_malformed_error() {
        for apply_transforms in [true, false] {
            let r = decode(&HEIC_RS_BOX_PANIC, &Options { apply_transforms, ..Default::default() });
            assert!(matches!(r, Err(Error::Malformed(_))), "{r:?}");
        }
    }

    #[test]
    fn image_sequence_is_unsupported_with_a_clear_message() {
        // An ftyp and nothing else: a HEIF image sequence would have a moov here instead of a meta.
        let bytes = b"\0\0\0\x18ftypmsf1\0\0\0\0msf1hevc";
        for r in [probe(bytes).map(|_| ()), decode(bytes, &Options::default()).map(|_| ())] {
            assert!(matches!(&r, Err(Error::Unsupported(m)) if m.contains("sequence")), "{r:?}");
        }
    }

    #[test]
    fn garbage_is_an_error() {
        for bytes in [&b""[..], b"\0\0\0\x18ftypheic\0\0\0\0mif1heic", &[0xFF; 64]] {
            assert!(probe(bytes).is_err());
            assert!(decode(bytes, &Options::default()).is_err());
            assert!(decode_thumbnail(bytes, 1, &Options::default()).is_err());
        }
    }

    #[test]
    fn errors_display_their_kind() {
        assert!(Error::Unsupported("x").to_string().contains("unsupported"));
        assert!(Error::Limit("x".into()).to_string().contains("limit"));
        assert!(Error::Malformed("x".into()).to_string().contains("malformed"));
    }
}
