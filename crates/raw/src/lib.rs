//! RAW decoding for LightCraft.
//!
//! - [`probe`] recognises raw containers; [`decode`] turns a file into a [`RawImage`] (sensor data + everything
//!   needed to render it: CFA, black/white levels, active area, default crop, orientation, DNG colour tags,
//!   opcode lists, [`Metadata`]); [`embedded_preview`] returns the largest embedded JPEG (or DNG 1.7 JPEG XL) preview;
//!   [`semantic_masks`] reads a DNG's semantic masks (segmentation mattes, e.g. iPhone ProRAW's sky matte).
//! - [`RawImage::normalized`] subtracts black, scales white to 1.0 and crops to the active area (applying DNG
//!   `OpcodeList1`/`OpcodeList2`); [`demosaic`] turns CFA data into camera-RGB [`Rgb32f`];
//!   [`RawImage::develop`] does all of it plus `OpcodeList3` and the default crop; [`RawImage::develop_binned`]
//!   produces the same at 1/k of the size straight from the mosaic (previews, thumbnails;
//!   [`RawImage::develop_binned_masked`] with the mask of blocks holding clipped samples, for [`highlight`]).
//! - [`color`] implements the DNG colour model (dual-illuminant interpolation, forward matrices, white balance)
//!   and produces camera → linear Rec.2020 D65 matrices; [`profile`] reads and applies a DNG's own profile
//!   look tables and tone curve, and [`gaintable`] its gain table map (Apple ProRAW's local tone mapping).
//!
//! Formats: DNG (uncompressed, lossless JPEG, lossy JPEG (Smart Previews), Deflate incl. floating point, JPEG XL (DNG 1.7,
//! `jxl` feature, on by default), tiled/stripped, CFA and LinearRaw),
//! Canon CR2 / CR3 (lossless CRX Bayer and version 0x100/0x200 C-RAW), Nikon NEF/NRW (uncompressed, Huffman lossless / lossy compressed), Sony ARW (uncompressed, ARW2, lossless), Fujifilm RAF (uncompressed Bayer
//! and X-Trans, lossless and lossy compressed), Panasonic RW2 / Leica RWL / Panasonic RAW (every raw format: compressed 4 and 6, the prefix-coded strips of 8,
//! packed 2/5/7, the 16-bit words of the oldest bodies), Pentax PEF (uncompressed, Huffman), Olympus ORF (uncompressed: 16-bit words, 12-bit XZ-2 words, 12-bit 16-byte blocks of the E-300/E-330/E-500).
//! [`embedded_preview`] covers these containers' JPEG previews. Variants we can't decode yet (compressed ORF, CR3 unverified marker families / C-RAW configurations) return [`RawError::Unsupported`]; each vendor module documents its sources
//! (public specifications, tag-name documentation, black-box analysis of CC0 samples) and gaps. Non-DNG files carry no
//! colour matrix: [`spectral`] has matrices fitted to measured spectral sensitivities for 52 models, and
//! otherwise [`color`] falls back to a documented neutral model. The decoders never panic on malformed input.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod binned;
pub mod color;
pub mod demosaic;
mod dng;
pub mod dngwrite;
pub mod gaintable;
pub mod highlight;
#[cfg(feature = "jxl")]
mod jxl;
pub mod kelvin_builtin;
pub mod kelvin_presets;
pub mod lens_metadata;
pub mod ljpeg;
pub(crate) mod llvc;
pub mod opcodes;
mod preview;
pub mod profile;
mod saturation;
pub mod semantic;
pub mod sparse;
pub mod spectral;
mod tiffraw;
mod unpack;
mod vendor;
mod wb_estimate;

pub use demosaic::{Method, demosaic};
pub use dngwrite::{DngCompression, DngWriteOptions, write_dng, write_rendered_dng};
pub use lens_metadata::LensMetadata;
pub use lightcraft_color::Mat3;
pub use lightcraft_geom::Orientation;
pub use lightcraft_meta::Metadata;
pub use lightcraft_raster::Rgb32f;
pub use opcodes::{Opcode, OpcodeLists};
pub use preview::{
    PREVIEW_PREFIX, PreviewColorSpace, PreviewRange, bounded_preview, embedded_preview, embedded_preview_color_space, embedded_preview_covering,
    embedded_preview_dynamic_range_optimized, embedded_preview_from_range,
};
pub use semantic::{SemanticMask, semantic_masks};

use lightcraft_color::Xy;
use lightcraft_tiff::{Tiff, TiffError};
use serde::{Deserialize, Serialize};

/// Upper bound on decoded samples (guards allocations driven by header values).
pub const MAX_SAMPLES: usize = 1 << 30;

/// Checked product of input-derived dimensions, in u64 so it cannot wrap
/// on either 32-bit (wasm32) or 64-bit targets. The arguments are u64 too, so callers can feed
/// unchecked products of header values (row bytes, sample counts) without truncating on 32-bit.
///
/// Returns `None` on overflow, zero, or when the product exceeds [`MAX_SAMPLES`].
pub(crate) fn checked_pixel_count(width: u64, height: u64) -> Option<u64> {
    width.checked_mul(height).filter(|&n| n > 0 && n <= MAX_SAMPLES as u64)
}

/// Errors from raw decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RawError {
    #[error("not a recognised raw file")]
    NotRaw,
    #[error("unsupported raw variant: {0}")]
    Unsupported(String),
    #[error("corrupt raw data: {0}")]
    Corrupt(String),
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    #[error(transparent)]
    Tiff(#[from] TiffError),
}

pub type Result<T> = std::result::Result<T, RawError>;

/// Raw container formats recognised by [`probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawFormat {
    Dng,
    Cr2,
    Cr3,
    Nef,
    Nrw,
    Arw,
    Raf,
    Orf,
    Rw2,
    Pef,
    Srw,
    /// Canon CRW (CIFF heap), the bodies before the CR2 era.
    Crw,
    /// Minolta MRW (`\0MRM` block container, `vendor::mrw`).
    Mrw,
    /// Sigma / Foveon X3F (`FOVb`).
    X3f,
    /// A TIFF-based raw that has no DNG version tag but describes itself the way DNG does (a
    /// full-resolution CFA image with black/white levels, a default crop and `AsShotNeutral`):
    /// Hasselblad 3FR and FFF. Read by `vendor::hasselblad` (the DNG reader plus what Hasselblad adds).
    CfaTiff,
    /// Kodak DCS520C TIFF: a two-component lossless JPEG CFA sub-IFD with a response curve (`vendor::kodak`).
    KodakDcs,
    /// Epson ERF: a CFA sub-IFD of 12-bit samples in 16-byte blocks (`vendor::erf`).
    Erf,
    /// Mamiya MEF: an uncompressed 12-bit packed CFA sub-IFD (`vendor::mef`).
    Mef,
    /// Another TIFF-based raw (3FR, IIQ, ERF, KDC, DCR, MOS, …).
    OtherTiff,
}

impl RawFormat {
    /// Whether [`decode`] supports this container (possibly not every compression inside it).
    pub fn is_supported(self) -> bool {
        matches!(
            self,
            RawFormat::Dng
                | RawFormat::Cr2
                | RawFormat::Cr3
                | RawFormat::Nef
                | RawFormat::Nrw
                | RawFormat::Arw
                | RawFormat::Raf
                | RawFormat::Rw2
                | RawFormat::Pef
                | RawFormat::CfaTiff
                | RawFormat::Srw
                | RawFormat::KodakDcs
                | RawFormat::Mrw
                | RawFormat::Erf
                | RawFormat::Mef
        )
    }
}

/// Recognise a raw file from its first bytes / TIFF structure.
pub fn probe(bytes: &[u8]) -> Option<RawFormat> {
    if bytes.len() >= 12 && &bytes[4..12] == b"ftypcrx " {
        return Some(RawFormat::Cr3);
    }
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        return Some(RawFormat::Raf);
    }
    if bytes.starts_with(b"IIRO") || bytes.starts_with(b"IIRS") || bytes.starts_with(b"MMOR") {
        return Some(RawFormat::Orf);
    }
    if bytes.starts_with(b"IIU\0") {
        return Some(RawFormat::Rw2);
    }
    if bytes.starts_with(b"\0MRM") {
        return Some(RawFormat::Mrw);
    }
    if bytes.starts_with(b"FOVb") {
        return Some(RawFormat::X3f);
    }
    if bytes.len() >= 14 && bytes.starts_with(b"II") && &bytes[6..14] == b"HEAPCCDR" {
        return Some(RawFormat::Crw);
    }
    if bytes.len() >= 10 && bytes.starts_with(b"II*\0") && &bytes[8..10] == b"CR" {
        return Some(RawFormat::Cr2);
    }
    let (_, _) = Tiff::sniff(bytes)?;
    let opts = lightcraft_tiff::ParseOptions { max_ifds: 256, ..Default::default() };
    let t = Tiff::parse_with(bytes, &opts).ok()?;
    let ifd0 = t.ifds.first()?;
    if ifd0.contains(lightcraft_tiff::tags::DNG_VERSION) {
        return Some(RawFormat::Dng);
    }
    let make = t.find(lightcraft_tiff::tags::MAKE).and_then(|e| e.value.as_str()).unwrap_or_default().to_ascii_uppercase();
    let has_cfa = has_raw_ifd(&t);
    // Pentax's reader takes the layout from the Exif `CFAPattern` when no IFD has a CFA photometric (uncompressed
    // and PackBits PEFs), so for the Pentax family that tag counts as raw evidence too
    let pentax_raw = has_cfa || t.exif().is_some_and(|e| e.contains(vendor::EXIF_CFA_PATTERN));
    // Samsung's raw IFD uses private compressions 32769..=32773 (`vendor::srw`), 32773 being PackBits elsewhere
    let samsung_raw = has_cfa || t.all_ifds().iter().any(|i| i.u16(lightcraft_tiff::tags::COMPRESSION).is_some_and(|c| (32769..=32773).contains(&c)));
    // Sony raws without a CFA IFD (#702): SRF (DSC-F828), whose IFD0 looks like RGB but holds 14-bit samples (12 or
    // 14 count: raw sensor depths; the raw is in Sony's SRF blocks), and the DSLR-A100, whose IFD0 is an old-style
    // JPEG thumbnail (compression 6, which today's TIFF writers don't produce) and whose `SubIFDs` tag addresses its
    // raw data. A camera-authored or exported TIFF has 8- or 16-bit samples, and its SubIFDs hold ordinary images
    let model = t.find(lightcraft_tiff::tags::MODEL).and_then(|e| e.value.as_str()).unwrap_or_default().trim().to_ascii_uppercase();
    let sony_raw = has_cfa
        || ifd0.u64s(lightcraft_tiff::tags::BITS_PER_SAMPLE).is_some_and(|b| b.iter().any(|v| matches!(v, 12 | 14)))
        || (model == "DSLR-A100" && ifd0.u16(lightcraft_tiff::tags::COMPRESSION) == Some(6) && ifd0.contains(lightcraft_tiff::tags::SUB_IFDS));
    if make.starts_with("CANON") && t.ifds.len() >= 4 && t.ifds[3].u16(lightcraft_tiff::tags::COMPRESSION) == Some(6) {
        return Some(RawFormat::Cr2);
    }
    if make.starts_with("NIKON") && has_cfa {
        return Some(if t.all_ifds().len() > 1 { RawFormat::Nef } else { RawFormat::Nrw });
    }
    // Hasselblad's Sony-built bodies (the Lunar is a NEX-7 inside) write a Sony ARW under their own Make. Sony's
    // raw coding gives them away: compression 32767 or the raw IFD's tone-curve tag 0x7010, which no 3FR / FFF has
    let sony_coded = || t.all_ifds().iter().any(|i| i.u16(lightcraft_tiff::tags::COMPRESSION) == Some(32767) || i.contains(0x7010));
    if (make.starts_with("SONY") && sony_raw) || (make.starts_with("HASSELBLAD") && sony_coded()) {
        return Some(RawFormat::Arw);
    }
    if (make.starts_with("PENTAX") || make.starts_with("RICOH")) && pentax_raw {
        return Some(RawFormat::Pef);
    }
    // a Samsung-branded body built on a Pentax design writes a Pentax-style maker note (the note's own magic)
    if make.starts_with("SAMSUNG") && pentax_raw && has_pentax_maker_note(&t, bytes) {
        return Some(RawFormat::Pef);
    }
    if make.starts_with("SAMSUNG") && samsung_raw {
        return Some(RawFormat::Srw);
    }
    if vendor::erf::raw_ifd(&t, bytes.len()).is_some() {
        return Some(RawFormat::Erf);
    }
    if vendor::mef::raw_ifd(&t, bytes.len()).is_some() {
        return Some(RawFormat::Mef);
    }
    if vendor::kodak::raw_ifd(&t, bytes).is_some() {
        return Some(RawFormat::KodakDcs);
    }
    if dng::is_plain_cfa_tiff(&t, bytes) {
        return Some(RawFormat::CfaTiff);
    }
    if has_cfa || thumbnail_shell(&t, bytes.len()).is_some() || is_preview_container(ifd0) || is_leaf_mosaic(ifd0) {
        return Some(RawFormat::OtherTiff);
    }
    None
}

/// Leaf's private data block in IFD0, tag `0x8606` (`LeafData` in the ExifTool EXIF tag-name documentation, which
/// says the Leaf tags "are found in .MOS images from Leaf digital camera backs").
const LEAF_DATA: u16 = 0x8606;

/// An uncompressed Leaf MOS: IFD0 is the 16-bit mosaic itself (compression 1, BlackIsZero, one sample per pixel),
/// with no Make tag and no CFA photometric, so only Leaf's data block marks it as a raw (issue #1275). A rendered
/// RGB image that kept the block stays an ordinary image.
fn is_leaf_mosaic(ifd0: &lightcraft_tiff::Ifd) -> bool {
    ifd0.contains(LEAF_DATA) && ifd0.u16(lightcraft_tiff::tags::SAMPLES_PER_PIXEL).unwrap_or(1) == 1
}

/// Whether the Exif maker note starts with the signature of the Pentax layouts (`AOC\0` or `PENTAX \0`).
fn has_pentax_maker_note(t: &Tiff, bytes: &[u8]) -> bool {
    let Some(e) = t.exif().and_then(|e| e.get(lightcraft_tiff::tags::MAKER_NOTE)) else { return false };
    bytes.get(e.offset as usize..).is_some_and(|n| n.starts_with(b"AOC\0") || n.starts_with(b"PENTAX \0"))
}

/// Whether some IFD is marked as raw: CFA photometric, or a raw-only compression value (99 is not a
/// registered TIFF compression; Leaf MOS files use it for their tiled 16-bit lossless-JPEG raw),
/// or vendor-specific raw tags (Sony tone curve, CFA pattern, or Pentax compression 65535).
fn has_raw_ifd(t: &Tiff) -> bool {
    t.all_ifds().iter().any(|i| {
        i.u16(lightcraft_tiff::tags::PHOTOMETRIC) == Some(lightcraft_tiff::tags::photometric::CFA)
            || i.u16(lightcraft_tiff::tags::COMPRESSION).is_some_and(|c| matches!(c, 34713 | 32767 | 32769 | 32770 | 99 | 65535))
            || i.contains(0x7010)
            || i.contains(lightcraft_tiff::tags::CFA_PATTERN_EP)
            || i.contains(lightcraft_tiff::tags::CFA_REPEAT_PATTERN_DIM)
    })
}

/// IFD0 that is not an image at all: no `ImageWidth` / `ImageLength`, but a JPEG preview pointer or
/// `SubIFDs` (the layout of Kodak KDC files, whose pictures sit in private tags). A plain TIFF reader
/// rejects such a file as malformed even though it carries a preview.
fn is_preview_container(ifd0: &lightcraft_tiff::Ifd) -> bool {
    use lightcraft_tiff::tags::{IMAGE_LENGTH, IMAGE_WIDTH, JPEG_INTERCHANGE_FORMAT, SUB_IFDS};
    !ifd0.contains(IMAGE_WIDTH) && !ifd0.contains(IMAGE_LENGTH) && (ifd0.contains(JPEG_INTERCHANGE_FORMAT) || ifd0.contains(SUB_IFDS))
}

/// A TIFF whose first image is only a reduced copy of a picture stored somewhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThumbnailShell {
    /// Size of the first image (IFD0).
    pub thumb: (u32, u32),
    /// Size of the picture it stands in for.
    pub full: (u32, u32),
}

/// Some raw files are a TIFF "shell" around a private block: IFD0 holds a small RGB preview and nothing
/// in the TIFF structure marks the real image as raw (no CFA photometric, no raw compression). The
/// structure still gives them away, with a generic rule that names no maker or model: the file
/// declares a picture at least 4x larger than IFD0 in both directions, and either
/// - a `SubIFDs` child of IFD0 is that bigger image (an 8-bit grey mosaic in one such file), or
/// - the Exif `PixelXDimension` / `PixelYDimension` say so *and* the file holds enough bytes outside
///   every IFD's strips and tiles to carry that many pixels at one bit each (a downsized copy that kept
///   its old Exif size has no such block).
///
/// A multi-page TIFF is not caught (its pages are the IFD chain, not `SubIFDs`), nor a pyramid whose
/// IFD0 is the full image.
fn thumbnail_shell(t: &Tiff, len: usize) -> Option<ThumbnailShell> {
    use lightcraft_tiff::tags::{IMAGE_LENGTH, IMAGE_WIDTH, PIXEL_X_DIMENSION, PIXEL_Y_DIMENSION};
    let ifd0 = t.ifds.first()?;
    let (w0, h0) = (ifd0.u32(IMAGE_WIDTH)?, ifd0.u32(IMAGE_LENGTH)?);
    if w0 == 0 || h0 == 0 {
        return None;
    }
    let much_bigger = |(w, h): (u32, u32)| u64::from(w) >= 4 * u64::from(w0) && u64::from(h) >= 4 * u64::from(h0);
    let shell = |full| Some(ThumbnailShell { thumb: (w0, h0), full });
    let mut best: Option<(u32, u32)> = None;
    for s in &ifd0.sub_ifds {
        if s.image().is_ok()
            && let (Some(w), Some(h)) = (s.u32(IMAGE_WIDTH), s.u32(IMAGE_LENGTH))
            && much_bigger((w, h))
            && best.is_none_or(|(bw, bh)| u64::from(w) * u64::from(h) > u64::from(bw) * u64::from(bh))
        {
            best = Some((w, h));
        }
    }
    if let Some(full) = best {
        return shell(full);
    }
    let exif = t.exif()?;
    let full = (exif.u32(PIXEL_X_DIMENSION)?, exif.u32(PIXEL_Y_DIMENSION)?);
    if !much_bigger(full) {
        return None;
    }
    // bytes covered by image data (strips and tiles shared between IFDs, e.g. IFD0 and its thumbnail, count once)
    let mut seen = std::collections::BTreeMap::new();
    for ifd in t.all_ifds() {
        if let Ok(info) = ifd.image() {
            for c in info.chunks(len as u64) {
                seen.insert(c.offset, c.len);
            }
        }
    }
    let covered = seen.values().fold(0u64, |a, &l| a.saturating_add(l));
    let spare = (len as u64).saturating_sub(covered);
    (spare.saturating_mul(8) >= u64::from(full.0) * u64::from(full.1)).then_some(())?;
    shell(full)
}

/// Why [`decode`] gives up on a file [`probe`] called [`RawFormat::OtherTiff`].
fn other_tiff_reason(bytes: &[u8]) -> String {
    let t = Tiff::parse_with(bytes, &lightcraft_tiff::ParseOptions { max_ifds: 256, ..Default::default() }).ok();
    // a DNG-style CFA IFD whose lossless JPEG the lossless decoder rejects: say why
    if let Some(t) = &t
        && let Some(info) = dng::raw_ifd(t).and_then(|i| i.image().ok())
        && info.compression == lightcraft_tiff::tags::compression::JPEG
        && let Some(src) = info.chunks(bytes.len() as u64).first().and_then(|c| lightcraft_tiff::image::chunk_bytes(bytes, c))
        && let Err(e) = ljpeg::frame_info(src)
    {
        return format!("raw image coded as lossless JPEG that is not decoded yet ({e})");
    }
    match t.as_ref().filter(|t| !has_raw_ifd(t)).and_then(|t| thumbnail_shell(t, bytes.len())) {
        Some(s) => format!(
            "{}x{} raw image in a private block, not decoded yet (the file's first image is a {}x{} reduced copy)",
            s.full.0, s.full.1, s.thumb.0, s.thumb.1
        ),
        None => "OtherTiff files are not decoded yet".to_string(),
    }
}

/// Decode a raw file.
pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    decode_with(bytes, Mode::Full)
}

/// [`decode`] at a smaller size, for a picture whose default crop needs a long edge of only `min_long` px: a DNG
/// that keeps reduced-resolution copies of its LinearRaw image (Lightroom's HDR and panorama merges keep 256 to 2048
/// px ones) decodes the smallest that covers it. The image has the full one's levels, colour and metadata; its
/// active area and crop are scaled to its size. `Ok(None)` when the file keeps no such copy: decode it whole.
pub fn decode_reduced(bytes: &[u8], min_long: usize) -> Result<Option<RawImage>> {
    if probe(bytes) != Some(RawFormat::Dng) {
        return Ok(None);
    }
    Ok(dng::decode_reduced(bytes, min_long)?.map(lift_clipped))
}

/// Everything [`decode`] learns about a raw file except its samples: geometry, orientation, CFA,
/// colour data, as-shot white balance, opcode lists (embedded lens corrections), metadata — read
/// from the headers without decompressing the pixel data, for imports. Equal to
/// [`RawImage::info`] of the decoded image. Black and white levels are not included (some formats
/// measure them from the samples).
///
/// A few vendor formats derive part of this from the samples themselves (Nikon NEF: optically
/// masked trailing columns; Olympus ORF: the bit depth of 16-bit files, and the CFA phase of files
/// without an Exif `CFAPattern`; Pentax PEF without crop tags: dark borders). Those read the
/// samples they need and drop them: ORF and PEF unpack their uncompressed data; NEF reads only the
/// first 128 rows (`vendor::nef::MASK_ROWS`), which both the probe and the full decode judge the masked
/// columns from, so a compressed NEF probes in a few milliseconds instead of decoding its whole
/// Huffman stream (issue #708).
pub fn probe_info(bytes: &[u8]) -> Result<RawInfo> {
    decode_with(bytes, Mode::Header).map(RawImage::into_info)
}

/// How much of a file a decoder reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Headers and samples.
    Full,
    /// Headers only: the returned image has no samples, and black/white levels measured from
    /// the samples are placeholders. Only [`RawImage::into_info`] may look at it.
    Header,
}

fn decode_with(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    match probe(bytes).ok_or(RawError::NotRaw)? {
        RawFormat::Dng => dng::decode(bytes, mode).map(lift_clipped),
        RawFormat::Cr2 => vendor::cr2::decode(bytes, mode),
        RawFormat::Cr3 => vendor::cr3::decode(bytes, mode),
        RawFormat::Nef | RawFormat::Nrw => vendor::nef::decode(bytes, mode),
        RawFormat::Arw => vendor::arw::decode(bytes, mode),
        RawFormat::Raf => vendor::raf::decode(bytes, mode),
        RawFormat::Rw2 => vendor::rw2::decode(bytes, mode),
        RawFormat::Pef => vendor::pef::decode(bytes, mode),
        RawFormat::Orf => vendor::orf::decode(bytes, mode),
        RawFormat::CfaTiff => vendor::hasselblad::decode(bytes, mode).map(lift_clipped),
        RawFormat::Srw => vendor::srw::decode(bytes, mode),
        RawFormat::KodakDcs => vendor::kodak::decode(bytes, mode),
        RawFormat::Mrw => vendor::mrw::decode(bytes, mode),
        RawFormat::Erf => vendor::erf::decode(bytes, mode),
        RawFormat::Mef => vendor::mef::decode(bytes, mode),
        RawFormat::OtherTiff => Err(RawError::Unsupported(other_tiff_reason(bytes))),
        other => Err(RawError::Unsupported(format!("{other:?} files are not decoded yet"))),
    }
}

/// DNG writers state a white level the sensor may never reach; samples stuck at the real saturation point are
/// raised to the white level so they count as clipped (see [`RawImage::lift_clipped_samples`]).
fn lift_clipped(mut r: RawImage) -> RawImage {
    r.lift_clipped_samples();
    r
}

/// A raw file's description without its samples (see [`probe_info`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawInfo {
    pub format: RawFormat,
    /// Full sensor data dimensions (including masked borders).
    pub width: usize,
    pub height: usize,
    pub cpp: usize,
    pub cfa: Option<Cfa>,
    pub bits: u32,
    pub active_area: Rect,
    /// Default crop relative to the active area.
    pub crop: Rect,
    pub orientation: Orientation,
    pub color: ColorData,
    pub wb_multipliers: Option<[f32; 3]>,
    pub opcodes: OpcodeLists,
    pub metadata: Metadata,
    /// Lens-correction tables the file records, decoded but not necessarily applied (see [`crate::lens_metadata`]).
    #[serde(default)]
    pub lens_metadata: LensMetadata,
    /// See [`RawImage::pixel_aspect`].
    #[serde(default = "square_pixels")]
    pub pixel_aspect: f32,
}

fn square_pixels() -> f32 {
    1.0
}

/// Rows of a picture `height` rows tall once its pixels (taller than wide by `aspect`) are made square.
pub fn stretched_height(height: usize, aspect: f32) -> usize {
    if (aspect - 1.0).abs() < 0.01 { height } else { ((height as f32 * aspect).round() as usize).max(1) }
}

impl RawInfo {
    /// Size of the developed image before orientation: the default crop, else the active area, with the rows
    /// stretched for non-square pixels.
    pub fn developed_size(&self) -> (usize, usize) {
        let c = self.crop.clipped(self.active_area.width, self.active_area.height);
        let (w, h) = if c.width > 1 && c.height > 1 { (c.width, c.height) } else { (self.active_area.width, self.active_area.height) };
        (w, stretched_height(h, self.pixel_aspect))
    }
}

/// Sensor samples.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RawData {
    U16(Vec<u16>),
    /// Floating-point DNGs (already linear, typically white = 1.0).
    F32(Vec<f32>),
}

impl RawData {
    pub fn len(&self) -> usize {
        match self {
            RawData::U16(v) => v.len(),
            RawData::F32(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[inline]
    pub fn get(&self, i: usize) -> f32 {
        match self {
            RawData::U16(v) => v[i] as f32,
            RawData::F32(v) => v[i],
        }
    }
}

/// Colour filter array pattern. Colour indices: 0 = red, 1 = green, 2 = blue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cfa {
    pub width: usize,
    pub height: usize,
    /// `width × height` colour indices, row-major, anchored at image pixel (0, 0).
    pub pattern: Vec<u8>,
}

impl Cfa {
    /// A 2×2 Bayer pattern from a string such as `"RGGB"`.
    pub fn bayer(s: &str) -> Option<Cfa> {
        let p: Vec<u8> = s
            .chars()
            .map(|c| {
                Some(match c {
                    'R' => 0,
                    'G' => 1,
                    'B' => 2,
                    _ => return None,
                })
            })
            .collect::<Option<_>>()?;
        (p.len() == 4).then_some(Cfa { width: 2, height: 2, pattern: p })
    }
    /// A 2×2 Bayer layout named in code (`"RGGB"`, `"BGGR"`, `"GRBG"`, `"GBRG"`). Only for literal
    /// names (tested below); anything else falls back to RGGB instead of failing.
    pub(crate) fn bayer_static(s: &'static str) -> Cfa {
        Cfa::bayer(s).unwrap_or(Cfa { width: 2, height: 2, pattern: vec![0, 1, 1, 2] })
    }
    /// The Fujifilm X-Trans 6×6 layout (as commonly documented), anchored at (0, 0).
    pub fn xtrans() -> Cfa {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let pattern = rows
            .iter()
            .flat_map(|r| {
                r.chars().map(|c| match c {
                    'R' => 0,
                    'G' => 1,
                    _ => 2,
                })
            })
            .collect();
        Cfa { width: 6, height: 6, pattern }
    }
    #[inline]
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.pattern[(y % self.height) * self.width + (x % self.width)]
    }
    /// The same pattern anchored at `(dx, dy)` of the current anchor (e.g. after cropping to an active area).
    pub fn shifted(&self, dx: usize, dy: usize) -> Cfa {
        let pattern = (0..self.height).flat_map(|y| (0..self.width).map(move |x| (x, y))).map(|(x, y)| self.color_at(x + dx, y + dy)).collect();
        Cfa { width: self.width, height: self.height, pattern }
    }
    pub fn is_bayer(&self) -> bool {
        if self.width != 2 || self.height != 2 {
            return false;
        }
        let mut c = [0; 3];
        for &p in &self.pattern {
            if p > 2 {
                return false;
            }
            c[p as usize] += 1;
        }
        c == [1, 2, 1]
    }
    /// Pattern name like `"RGGB"` for 2×2 patterns.
    pub fn name(&self) -> String {
        self.pattern.iter().map(|&c| ['R', 'G', 'B', '?'][c.min(3) as usize]).collect()
    }
    fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.width <= 16 && self.height <= 16 && self.pattern.len() == self.width * self.height
    }
}

/// Integer rectangle (pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub fn new(x: usize, y: usize, width: usize, height: usize) -> Rect {
        Rect { x, y, width, height }
    }
    /// Clip to a `w × h` area.
    pub fn clipped(self, w: usize, h: usize) -> Rect {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Rect { x, y, width: self.width.min(w - x), height: self.height.min(h - y) }
    }
}

/// Black level model (DNG `BlackLevelRepeatDim`, `BlackLevel`, `BlackLevelDeltaH/V`). The repeat pattern and
/// the deltas are anchored at the active area's top-left corner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlackLevel {
    pub repeat_rows: usize,
    pub repeat_cols: usize,
    /// `repeat_rows × repeat_cols × cpp` values.
    pub values: Vec<f32>,
    /// Per active-area column.
    pub delta_h: Vec<f32>,
    /// Per active-area row.
    pub delta_v: Vec<f32>,
}

impl Default for BlackLevel {
    fn default() -> Self {
        BlackLevel { repeat_rows: 1, repeat_cols: 1, values: vec![0.0], delta_h: vec![], delta_v: vec![] }
    }
}

impl BlackLevel {
    pub fn uniform(v: f32) -> BlackLevel {
        BlackLevel { values: vec![v], ..Default::default() }
    }
    /// Black level at active-area coordinates (x, y) for sample `s` of `cpp`.
    #[inline]
    pub fn at(&self, x: usize, y: usize, s: usize, cpp: usize) -> f32 {
        let (r, c) = (self.repeat_rows.max(1), self.repeat_cols.max(1));
        let idx = ((y % r) * c + (x % c)) * cpp + s;
        let base = self.values.get(idx).or_else(|| self.values.first()).copied().unwrap_or(0.0);
        base + self.delta_h.get(x).copied().unwrap_or(0.0) + self.delta_v.get(y).copied().unwrap_or(0.0)
    }
    /// Mean of the repeat-pattern values (for display / heuristics).
    pub fn mean(&self) -> f32 {
        if self.values.is_empty() { 0.0 } else { self.values.iter().sum::<f32>() / self.values.len() as f32 }
    }
}

/// DNG colour tags (DNG 1.7 chapter 6). Matrices are 3×3 (three-colour cameras).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorData {
    /// `CalibrationIlluminant1/2` (Exif LightSource codes; 0 = unknown).
    pub illuminant: [u16; 2],
    /// `ColorMatrix1/2`: XYZ → reference camera.
    pub color_matrix: [Option<Mat3>; 2],
    /// `ForwardMatrix1/2`: white-balanced camera → XYZ D50.
    pub forward_matrix: [Option<Mat3>; 2],
    /// `CameraCalibration1/2`.
    pub camera_calibration: [Option<Mat3>; 2],
    pub analog_balance: Option<[f64; 3]>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub as_shot_white_xy: Option<Xy>,
    /// EV to add for a "normal" rendering (`BaselineExposure` + `BaselineExposureOffset`).
    pub baseline_exposure: f64,
    /// `BaselineSharpness`: sharpening relative to a reference camera (`None`: the DNG default,
    /// 1). Read and written back by DNG export; rendering doesn't use it yet.
    #[serde(default)]
    pub baseline_sharpness: Option<f64>,
    /// The file's own camera-profile look (`ProfileHueSatMap*`, `ProfileLookTable*`,
    /// `ProfileToneCurve`), applied by [`color`]'s users at render time, and its
    /// `ProfileGainTableMap*`, kept (a DNG export writes it back) and rendered only when a photo's
    /// "Camera local tone mapping" option asks for it (`lightcraft_pipeline::local_tone`).
    #[serde(default)]
    pub profile: profile::ProfileLook,
    /// The camera's white-balance presets at stated temperatures (Sony ARW, Canon CR2), which give a raw without colour
    /// matrices a Kelvin scale ([`kelvin_presets`]). Not a DNG tag: nothing else reads it.
    #[serde(default)]
    pub kelvin_presets: Option<kelvin_presets::KelvinPresets>,
}

/// A decoded raw image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawImage {
    pub format: RawFormat,
    /// Full sensor data dimensions (including masked borders).
    pub width: usize,
    pub height: usize,
    /// Samples per pixel: 1 = CFA or monochrome, 3 = linear RGB (LinearRaw / demosaiced DNG).
    pub cpp: usize,
    pub data: RawData,
    /// CFA pattern anchored at pixel (0, 0) of the full data; `None` for linear / monochrome data.
    pub cfa: Option<Cfa>,
    /// Significant bits of the stored samples (informational).
    pub bits: u32,
    pub black: BlackLevel,
    /// White (clip) level per sample (after linearization).
    pub white: Vec<f32>,
    /// Area of valid image data within the full data.
    pub active_area: Rect,
    /// Default crop relative to the active area.
    pub crop: Rect,
    pub orientation: Orientation,
    pub color: ColorData,
    /// As-shot white-balance multipliers from vendor maker notes (green = 1), when known (non-DNG).
    pub wb_multipliers: Option<[f32; 3]>,
    /// Whether a `LinearizationTable` was applied to `data`.
    pub linearized: bool,
    /// Height of a sensor pixel over its width (1.0 for square pixels). The D1X and D1H write 2.0: their frame is
    /// 4028 x 1324 and the picture is 3:2 only once the rows are doubled. [`Self::develop`] stretches the rows by
    /// this factor.
    pub pixel_aspect: f32,
    pub opcodes: OpcodeLists,
    pub metadata: Metadata,
    /// Lens-correction tables the file records, decoded but not necessarily applied (see [`crate::lens_metadata`]); only
    /// validated components are in `opcodes`.
    #[serde(default)]
    pub lens_metadata: LensMetadata,
}

/// Black-subtracted, white-normalised data covering the active area.
#[derive(Clone, Debug, PartialEq)]
pub struct Normalized {
    pub width: usize,
    pub height: usize,
    pub cpp: usize,
    /// Nominal range [0, 1] (values may exceed 1 above white or be negative from noise).
    pub data: Vec<f32>,
    /// CFA anchored at (0, 0) of this buffer.
    pub cfa: Option<Cfa>,
}

impl RawImage {
    /// The description without the samples (what [`probe_info`] returns for the same file).
    pub fn info(&self) -> RawInfo {
        RawInfo {
            format: self.format,
            width: self.width,
            height: self.height,
            cpp: self.cpp,
            cfa: self.cfa.clone(),
            bits: self.bits,
            active_area: self.active_area,
            crop: self.crop,
            orientation: self.orientation,
            color: self.color.clone(),
            wb_multipliers: self.wb_multipliers,
            opcodes: self.opcodes.clone(),
            metadata: self.metadata.clone(),
            lens_metadata: self.lens_metadata.clone(),
            pixel_aspect: self.pixel_aspect,
        }
    }

    /// [`Self::info`], dropping the samples.
    pub fn into_info(self) -> RawInfo {
        RawInfo {
            format: self.format,
            width: self.width,
            height: self.height,
            cpp: self.cpp,
            cfa: self.cfa,
            bits: self.bits,
            active_area: self.active_area,
            crop: self.crop,
            orientation: self.orientation,
            color: self.color,
            wb_multipliers: self.wb_multipliers,
            opcodes: self.opcodes,
            metadata: self.metadata,
            lens_metadata: self.lens_metadata,
            pixel_aspect: self.pixel_aspect,
        }
    }

    /// Validate what a decoder read in `mode` ([`Mode::Header`]: everything but the samples).
    pub(crate) fn validate_for(&self, mode: Mode) -> Result<()> {
        self.check(mode == Mode::Full)
    }

    /// Validate internal consistency (dimensions vs data length, CFA shape, rectangles).
    pub fn validate(&self) -> Result<()> {
        self.check(true)
    }

    fn check(&self, samples: bool) -> Result<()> {
        if self.width == 0 || self.height == 0 || !(1..=4).contains(&self.cpp) {
            return Err(RawError::Corrupt("bad dimensions".into()));
        }
        // two samples a pixel are neither one colour nor RGB: nothing downstream can develop them (issue #1511)
        if self.cpp == 2 {
            return Err(RawError::Unsupported("2 samples per pixel".into()));
        }
        if samples {
            let expected = crate::checked_pixel_count(self.width as u64, self.height as u64)
                .and_then(|n| n.checked_mul(self.cpp as u64))
                .ok_or_else(|| RawError::Corrupt("data length mismatch".into()))?;
            let actual = u64::try_from(self.data.len()).map_err(|_| RawError::Corrupt("data length mismatch".into()))?;
            if actual != expected {
                return Err(RawError::Corrupt("data length mismatch".into()));
            }
        }
        if let Some(c) = &self.cfa
            && (!c.valid() || c.pattern.iter().any(|&p| p > 2))
        {
            return Err(RawError::Unsupported(format!("CFA pattern {}", c.name())));
        }
        let a = self.active_area;
        // checked: a crop origin from a maker note can be `u64::MAX`, and `x + width` would wrap
        // back inside the image here and again in `normalized`'s index arithmetic.
        let fits = |x: usize, len: usize, limit: usize| x.checked_add(len).is_some_and(|end| end <= limit);
        if a.width == 0 || a.height == 0 || !fits(a.x, a.width, self.width) || !fits(a.y, a.height, self.height) {
            return Err(RawError::Corrupt("active area outside image".into()));
        }
        Ok(())
    }

    /// White level for sample `s`.
    pub fn white_at(&self, s: usize) -> f32 {
        self.white.get(s).or_else(|| self.white.first()).copied().unwrap_or(65535.0)
    }

    /// Black-subtract, white-scale and crop to the active area, applying `OpcodeList1` (on raw values) first and
    /// `OpcodeList2` (on normalised values) after. 1.0 = white level.
    pub fn normalized(&self) -> Result<Normalized> {
        self.validate()?;
        let (w, cpp) = (self.width, self.cpp);
        let a = self.active_area;
        // stage 1 (full image) opcodes need a mutable float copy only when present
        let stage1: Option<Vec<f32>> = if self.opcodes.list1.is_empty() {
            None
        } else {
            let mut v: Vec<f32> = (0..self.data.len()).map(|i| self.data.get(i)).collect();
            opcodes::apply_list(&self.opcodes.list1, &mut v, self.width, self.height, cpp, self.cfa.as_ref(), 65535.0);
            Some(v)
        };
        let src = |i: usize| match &stage1 {
            Some(v) => v[i],
            None => self.data.get(i),
        };
        let scale: Vec<f32> = (0..cpp)
            .map(|s| {
                let range = self.white_at(s) - self.black.mean();
                if range > 0.0 { 1.0 / range } else { 1.0 }
            })
            .collect();
        let mut out = vec![0f32; a.width * a.height * cpp];
        let row_len = a.width * cpp;
        use rayon::prelude::*;
        out.par_chunks_mut(row_len).enumerate().for_each(|(y, row)| {
            let base = ((a.y + y) * w + a.x) * cpp;
            for x in 0..a.width {
                for s in 0..cpp {
                    let v = src(base + x * cpp + s);
                    let b = self.black.at(x, y, s, cpp);
                    // a NaN or infinite sample (a float DNG's, or a black level a file got wrong)
                    // reads as black, so the demosaic and every stage after it see numbers
                    let n = (v - b) * scale[s];
                    row[x * cpp + s] = if n.is_finite() { n } else { 0.0 };
                }
            }
        });
        let cfa = self.cfa.as_ref().map(|c| c.shifted(a.x, a.y));
        opcodes::apply_list(&self.opcodes.list2, &mut out, a.width, a.height, cpp, cfa.as_ref(), 1.0);
        Ok(Normalized { width: a.width, height: a.height, cpp, data: out, cfa })
    }

    /// Full "raw → camera RGB" path: normalise, demosaic with `method`, apply `OpcodeList3`, then the default crop.
    /// The result is camera RGB (not white balanced), white level = 1.0, not oriented.
    pub fn develop(&self, method: Method) -> Result<Rgb32f> {
        let n = self.normalized()?;
        let rgb = demosaic(&n, method);
        drop(n);
        Ok(self.finish_demosaiced(rgb))
    }

    /// The last steps of [`Self::develop`] for camera RGB made some other way (a denoiser that demosaics too) from
    /// [`Self::normalized`]'s mosaic: apply `OpcodeList3`, then the default crop.
    pub fn finish_demosaiced(&self, mut rgb: Rgb32f) -> Rgb32f {
        opcodes::apply_list3(&self.opcodes.list3, &mut rgb);
        let c = self.develop_crop(rgb.width, rgb.height);
        let rgb =
            if c.x == 0 && c.y == 0 && c.width == rgb.width && c.height == rgb.height { rgb } else { rgb.into_crop(c.x, c.y, c.width, c.height) };
        let h = stretched_height(rgb.height, self.pixel_aspect);
        if h == rgb.height {
            return rgb;
        }
        lightcraft_raster::resample::resize(&rgb, rgb.width, h, lightcraft_raster::resample::Filter::Mitchell)
    }

    /// The part of a `width × height` demosaiced picture (the active area) that [`Self::develop`] keeps: the default
    /// crop, or all of it when there is none.
    pub fn develop_crop(&self, width: usize, height: usize) -> Rect {
        let c = self.crop.clipped(width, height);
        if c.width == 0 || c.height == 0 { Rect::new(0, 0, width, height) } else { c }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod checked_arithmetic_tests {
    use super::*;

    #[test]
    fn checked_pixel_count_wrap_inputs_rejected() {
        // 65536 × 65536 = 2^32 wraps to 0 on wasm32 usize; u64 math keeps it exact.
        assert_eq!(checked_pixel_count(65536, 65536), None);
        assert_eq!(checked_pixel_count(65536, 65537), None);
        // u32::MAX × 2 wraps on 32-bit usize.
        assert_eq!(checked_pixel_count(u64::from(u32::MAX), 2), None);
        // inputs wider than a 32-bit usize reach the gate untruncated (2^40 × 2^40 overflows u64).
        assert_eq!(checked_pixel_count(1 << 40, 1 << 40), None);
    }

    #[test]
    fn checked_pixel_count_over_cap_rejected() {
        // MAX_SAMPLES = 2^30; one over is rejected.
        assert_eq!(checked_pixel_count((1 << 30) + 1, 1), None);
        assert_eq!(checked_pixel_count(1, (1 << 30) + 1), None);
    }

    #[test]
    fn checked_pixel_count_at_cap_accepted() {
        assert_eq!(checked_pixel_count(1 << 30, 1), Some(1 << 30));
        assert_eq!(checked_pixel_count(1 << 15, 1 << 15), Some(1 << 30));
    }

    #[test]
    fn checked_pixel_count_small_accepted() {
        assert_eq!(checked_pixel_count(100, 100), Some(10_000));
        assert_eq!(checked_pixel_count(1, 1), Some(1));
        assert_eq!(checked_pixel_count(0, 100), None);
        assert_eq!(checked_pixel_count(100, 0), None);
    }
}
