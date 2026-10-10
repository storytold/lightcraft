//! RAW decoding.
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
pub mod ljpeg;
pub mod opcodes;
mod preview;
pub mod profile;
mod saturation;
pub mod semantic;
pub mod spectral;
mod tiffraw;
mod unpack;
mod vendor;

pub use dac_color::Mat3;
pub use dac_geom::Orientation;
pub use dac_meta::Metadata;
pub use dac_raster::Rgb32f;
pub use demosaic::{Method, demosaic};
pub use dngwrite::{DngCompression, DngWriteOptions, write_dng};
pub use opcodes::{Opcode, OpcodeLists};
pub use preview::{PreviewColorSpace, embedded_preview, embedded_preview_color_space, embedded_preview_dynamic_range_optimized};
pub use semantic::{SemanticMask, semantic_masks};

use dac_color::Xy;
use dac_tiff::{Tiff, TiffError};
use serde::{Deserialize, Serialize};

/// Upper bound on decoded samples (guards allocations driven by header values).
pub const MAX_SAMPLES: usize = 1 << 30;

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
    /// Minolta MRW (`\0MRM` block container).
    Mrw,
    /// Sigma / Foveon X3F (`FOVb`).
    X3f,
    /// A TIFF-based raw that has no DNG version tag but describes itself the way DNG does (a
    /// full-resolution CFA image with black/white levels, a default crop and `AsShotNeutral`):
    /// Hasselblad 3FR and FFF. Read by the DNG reader.
    CfaTiff,
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
    let opts = dac_tiff::ParseOptions { max_ifds: 256, ..Default::default() };
    let t = Tiff::parse_with(bytes, &opts).ok()?;
    let ifd0 = t.ifds.first()?;
    if ifd0.contains(dac_tiff::tags::DNG_VERSION) {
        return Some(RawFormat::Dng);
    }
    let make = t.find(dac_tiff::tags::MAKE).and_then(|e| e.value.as_str()).unwrap_or_default().to_ascii_uppercase();
    let has_cfa = has_raw_ifd(&t);
    // Pentax's reader takes the layout from the Exif `CFAPattern` when no IFD has a CFA photometric (uncompressed
    // and PackBits PEFs), so for the Pentax family that tag counts as raw evidence too
    let pentax_raw = has_cfa || t.exif().is_some_and(|e| e.contains(vendor::EXIF_CFA_PATTERN));
    // Samsung's raw IFD uses private compressions 32769..=32773 (`vendor::srw`), 32773 being PackBits elsewhere
    let samsung_raw = has_cfa || t.all_ifds().iter().any(|i| i.u16(dac_tiff::tags::COMPRESSION).is_some_and(|c| (32769..=32773).contains(&c)));
    if make.starts_with("CANON") && t.ifds.len() >= 4 && t.ifds[3].u16(dac_tiff::tags::COMPRESSION) == Some(6) {
        return Some(RawFormat::Cr2);
    }
    if make.starts_with("NIKON") && has_cfa {
        return Some(if t.all_ifds().len() > 1 { RawFormat::Nef } else { RawFormat::Nrw });
    }
    if make.starts_with("SONY") && has_cfa {
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
    if dng::is_plain_cfa_tiff(&t, bytes) {
        return Some(RawFormat::CfaTiff);
    }
    if has_cfa || thumbnail_shell(&t, bytes.len()).is_some() || is_preview_container(ifd0) {
        return Some(RawFormat::OtherTiff);
    }
    None
}

/// Whether the Exif maker note starts with the signature of the Pentax layouts (`AOC\0` or `PENTAX \0`).
fn has_pentax_maker_note(t: &Tiff, bytes: &[u8]) -> bool {
    let Some(e) = t.exif().and_then(|e| e.get(dac_tiff::tags::MAKER_NOTE)) else { return false };
    bytes.get(e.offset as usize..).is_some_and(|n| n.starts_with(b"AOC\0") || n.starts_with(b"PENTAX \0"))
}

/// Whether some IFD is marked as raw: CFA photometric, or a raw-only compression value (99 is not a
/// registered TIFF compression; Leaf MOS files use it for their tiled 16-bit lossless-JPEG raw),
/// or vendor-specific raw tags (Sony tone curve, CFA pattern, or Pentax compression 65535).
fn has_raw_ifd(t: &Tiff) -> bool {
    t.all_ifds().iter().any(|i| {
        i.u16(dac_tiff::tags::PHOTOMETRIC) == Some(dac_tiff::tags::photometric::CFA)
            || i.u16(dac_tiff::tags::COMPRESSION).is_some_and(|c| matches!(c, 34713 | 32767 | 32769 | 32770 | 99 | 65535))
            || i.contains(0x7010)
            || i.contains(dac_tiff::tags::CFA_PATTERN_EP)
            || i.contains(dac_tiff::tags::CFA_REPEAT_PATTERN_DIM)
    })
}

/// IFD0 that is not an image at all: no `ImageWidth` / `ImageLength`, but a JPEG preview pointer or
/// `SubIFDs` (the layout of Kodak KDC files, whose pictures sit in private tags). A plain TIFF reader
/// rejects such a file as malformed even though it carries a preview.
fn is_preview_container(ifd0: &dac_tiff::Ifd) -> bool {
    use dac_tiff::tags::{IMAGE_LENGTH, IMAGE_WIDTH, JPEG_INTERCHANGE_FORMAT, SUB_IFDS};
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
    use dac_tiff::tags::{IMAGE_LENGTH, IMAGE_WIDTH, PIXEL_X_DIMENSION, PIXEL_Y_DIMENSION};
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
    let t = Tiff::parse_with(bytes, &dac_tiff::ParseOptions { max_ifds: 256, ..Default::default() }).ok();
    // a DNG-style CFA IFD whose lossless JPEG the lossless decoder rejects: say why
    if let Some(t) = &t
        && let Some(info) = dng::raw_ifd(t).and_then(|i| i.image().ok())
        && info.compression == dac_tiff::tags::compression::JPEG
        && let Some(src) = info.chunks(bytes.len() as u64).first().and_then(|c| dac_tiff::image::chunk_bytes(bytes, c))
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

/// Everything [`decode`] learns about a raw file except its samples: geometry, orientation, CFA,
/// colour data, as-shot white balance, opcode lists (embedded lens corrections), metadata — read
/// from the headers without decompressing the pixel data, for imports. Equal to
/// [`RawImage::info`] of the decoded image. Black and white levels are not included (some formats
/// measure them from the samples).
///
/// A few uncompressed vendor formats derive part of this from the samples themselves (Nikon
/// NEF: optically masked trailing columns; Olympus ORF: the bit depth of 16-bit files, and the CFA
/// phase of files without an Exif `CFAPattern`; Pentax PEF without crop tags: dark borders); for
/// those the samples are read (unpacked, nothing to decompress) and dropped.
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
        RawFormat::Nef | RawFormat::Nrw => vendor::nef::decode(bytes),
        RawFormat::Arw => vendor::arw::decode(bytes, mode),
        RawFormat::Raf => vendor::raf::decode(bytes, mode),
        RawFormat::Rw2 => vendor::rw2::decode(bytes, mode),
        RawFormat::Pef => vendor::pef::decode(bytes, mode),
        RawFormat::Orf => vendor::orf::decode(bytes, mode),
        RawFormat::CfaTiff => dng::decode_as(bytes, mode, RawFormat::CfaTiff).map(lift_clipped),
        RawFormat::Srw => vendor::srw::decode(bytes, mode),
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
}

impl RawInfo {
    /// Size of the developed image before orientation: the default crop, else the active area.
    pub fn developed_size(&self) -> (usize, usize) {
        let c = self.crop.clipped(self.active_area.width, self.active_area.height);
        if c.width > 1 && c.height > 1 { (c.width, c.height) } else { (self.active_area.width, self.active_area.height) }
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
    /// "Camera local tone mapping" option asks for it (`dac_pipeline::local_tone`).
    #[serde(default)]
    pub profile: profile::ProfileLook,
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
    pub opcodes: OpcodeLists,
    pub metadata: Metadata,
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
        if samples && self.data.len() != self.width * self.height * self.cpp {
            return Err(RawError::Corrupt("data length mismatch".into()));
        }
        if let Some(c) = &self.cfa
            && (!c.valid() || c.pattern.iter().any(|&p| p > 2))
        {
            return Err(RawError::Unsupported(format!("CFA pattern {}", c.name())));
        }
        let a = self.active_area;
        if a.width == 0 || a.height == 0 || a.x + a.width > self.width || a.y + a.height > self.height {
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
                    row[x * cpp + s] = (v - b) * scale[s];
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
        if c.x == 0 && c.y == 0 && c.width == rgb.width && c.height == rgb.height {
            return rgb;
        }
        rgb.into_crop(c.x, c.y, c.width, c.height)
    }

    /// The part of a `width × height` demosaiced picture (the active area) that [`Self::develop`] keeps: the default
    /// crop, or all of it when there is none.
    pub fn develop_crop(&self, width: usize, height: usize) -> Rect {
        let c = self.crop.clipped(width, height);
        if c.width == 0 || c.height == 0 { Rect::new(0, 0, width, height) } else { c }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfa_helpers() {
        let c = Cfa::bayer("RGGB").unwrap();
        assert!(c.is_bayer());
        assert_eq!(c.color_at(0, 0), 0);
        assert_eq!(c.color_at(3, 3), 2);
        assert_eq!(c.shifted(1, 0).name(), "GRBG");
        assert_eq!(c.shifted(1, 1).name(), "BGGR");
        assert!(Cfa::bayer("RGGX").is_none());
        for name in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            assert_eq!(Cfa::bayer_static(name).name(), name);
        }
        let x = Cfa::xtrans();
        assert!(!x.is_bayer());
        let greens = x.pattern.iter().filter(|&&p| p == 1).count();
        assert_eq!(greens, 20);
        // every 3×3 block of the X-Trans layout has 5 greens
        for by in 0..2 {
            for bx in 0..2 {
                let g = (0..9).filter(|i| x.color_at(bx * 3 + i % 3, by * 3 + i / 3) == 1).count();
                assert_eq!(g, 5);
            }
        }
    }

    #[test]
    fn a_picture_made_elsewhere_is_finished_like_develop_does() {
        let (w, h) = (12usize, 10usize);
        let raw = RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16((0..w * h).map(|i| (100 + (i * 37) % 900) as u16).collect()),
            cfa: Cfa::bayer("RGGB"),
            bits: 12,
            black: BlackLevel::uniform(64.0),
            white: vec![1023.0],
            active_area: Rect::new(2, 2, 8, 6),
            crop: Rect::new(1, 1, 5, 4),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            opcodes: OpcodeLists::default(),
            metadata: Metadata::default(),
        };
        let direct = raw.develop(Method::Bilinear).unwrap();
        let by_hand = raw.finish_demosaiced(demosaic(&raw.normalized().unwrap(), Method::Bilinear));
        assert_eq!((direct.width, direct.height), (5, 4));
        assert_eq!(direct, by_hand);
        // the crop is what develop keeps; no usable crop keeps everything
        assert_eq!(raw.develop_crop(8, 6), Rect::new(1, 1, 5, 4));
        let mut whole = raw.clone();
        whole.crop = Rect::default();
        assert_eq!(whole.develop_crop(8, 6), Rect::new(0, 0, 8, 6));
        assert_eq!(whole.develop(Method::Bilinear).unwrap().width, 8);
        // a crop that pokes out is clipped to the picture
        whole.crop = Rect::new(6, 4, 10, 10);
        assert_eq!(whole.develop_crop(8, 6), Rect::new(6, 4, 2, 2));
    }

    #[test]
    fn black_levels() {
        let b = BlackLevel { repeat_rows: 2, repeat_cols: 2, values: vec![1.0, 2.0, 3.0, 4.0], delta_h: vec![0.5, 0.0], delta_v: vec![0.0, 10.0] };
        assert_eq!(b.at(0, 0, 0, 1), 1.5);
        assert_eq!(b.at(1, 1, 0, 1), 14.0);
        assert_eq!(b.at(3, 2, 0, 1), 2.0);
        assert_eq!(b.mean(), 2.5);
        assert_eq!(BlackLevel::uniform(7.0).at(9, 9, 0, 3), 7.0);
    }

    // --- TIFF shells around a private raw block (thumbnail-only IFD0) ---

    use dac_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};

    /// An uncompressed 8-bit RGB image IFD.
    fn rgb_ifd(w: u32, h: u32) -> IfdBuilder {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![2]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![vec![90u8; (w * h * 3) as usize]] });
        ifd
    }

    fn exif_size(w: u32, h: u32) -> IfdBuilder {
        IfdBuilder::new().with(t::PIXEL_X_DIMENSION, Value::Long(vec![w])).with(t::PIXEL_Y_DIMENSION, Value::Long(vec![h]))
    }

    fn write(chain: &[IfdBuilder]) -> Vec<u8> {
        TiffWriter::default().write(chain).unwrap()
    }

    const SHELL_REASON: &str = "raw image in a private block";

    /// A bigger image in a `SubIFDs` child of a small IFD0 (the layout of an 8-bit-mosaic TIFF raw).
    #[test]
    fn thumbnail_with_a_bigger_sub_ifd_is_an_undecodable_raw() {
        let mut ifd0 = rgb_ifd(16, 12);
        ifd0.add_sub_ifd(rgb_ifd(64, 48));
        let bytes = write(&[ifd0]);
        assert_eq!(probe(&bytes), Some(RawFormat::OtherTiff));
        let Err(RawError::Unsupported(why)) = decode(&bytes) else { panic!("expected Unsupported") };
        assert!(why.contains("64x48") && why.contains("16x12") && why.contains(SHELL_REASON), "{why}");
        // a sub-IFD that is not much bigger (a pyramid level) does not count
        let mut ifd0 = rgb_ifd(16, 12);
        ifd0.add_sub_ifd(rgb_ifd(32, 24));
        assert_eq!(probe(&write(&[ifd0])), None);
    }

    /// Exif says the picture is far bigger than IFD0 and the file holds a block that could carry it.
    #[test]
    fn thumbnail_with_a_private_block_behind_the_exif_size_is_an_undecodable_raw() {
        let mut ifd0 = rgb_ifd(16, 12);
        ifd0.set_child(t::EXIF_IFD, exif_size(400, 300));
        let mut bytes = write(&[ifd0.clone()]);
        // no private block: a downsized copy that kept its old Exif size is an ordinary image
        assert_eq!(probe(&bytes), None);
        // 400 x 300 pixels at one bit each is 15000 bytes: 14000 is not enough, 16000 is
        bytes.extend(std::iter::repeat_n(7u8, 14_000));
        assert_eq!(probe(&bytes), None);
        bytes.extend(std::iter::repeat_n(7u8, 2_000));
        assert_eq!(probe(&bytes), Some(RawFormat::OtherTiff));
        let Err(RawError::Unsupported(why)) = decode(&bytes) else { panic!("expected Unsupported") };
        assert!(why.contains("400x300") && why.contains("16x12") && why.contains(SHELL_REASON), "{why}");
        // truncating it never panics
        for n in (0..bytes.len()).step_by(37) {
            let _ = probe(&bytes[..n]);
            let _ = decode(&bytes[..n]);
        }
    }

    /// Ordinary TIFFs stay images: the full picture first, equal Exif size, or later pages that are bigger.
    #[test]
    fn ordinary_tiffs_are_not_shells() {
        let padded = |chain: &[IfdBuilder]| {
            let mut b = write(chain);
            b.extend(std::iter::repeat_n(0u8, 100_000));
            b
        };
        let mut full = rgb_ifd(400, 300);
        full.set_child(t::EXIF_IFD, exif_size(400, 300));
        assert_eq!(probe(&padded(&[full])), None);
        // the Exif size is much bigger than IFD0, with spare bytes, but IFD0 is not tiny next to it
        let mut near = rgb_ifd(120, 90);
        near.set_child(t::EXIF_IFD, exif_size(400, 300));
        assert_eq!(probe(&padded(&[near])), None);
        // a multi-page TIFF whose first page is small
        assert_eq!(probe(&padded(&[rgb_ifd(16, 12), rgb_ifd(400, 300)])), None);
        // no dimensions at all
        assert_eq!(probe(&padded(&[IfdBuilder::new().with(t::MAKE, Value::Ascii("X".into()))])), None);
        // raw evidence without a CFA photometric: Pentax's Exif `CFAPattern` (uncompressed / PackBits PEFs) and
        // Samsung's private compressions (here 32773, PackBits elsewhere: NX1, NX500)
        let mut pef = rgb_ifd(400, 300);
        pef.set(t::MAKE, Value::Ascii("PENTAX Corporation".into()));
        let mut exif = exif_size(400, 300);
        exif.set(vendor::EXIF_CFA_PATTERN, Value::Undefined(vec![0, 2, 0, 2, 0, 1, 1, 2]));
        pef.set_child(t::EXIF_IFD, exif);
        assert_eq!(probe(&padded(&[pef])), Some(RawFormat::Pef));
        let mut srw = rgb_ifd(400, 300);
        srw.set(t::MAKE, Value::Ascii("SAMSUNG".into()));
        srw.set(t::COMPRESSION, Value::Short(vec![32773]));
        assert_eq!(probe(&padded(&[srw])), Some(RawFormat::Srw));
        // camera-authored or exported TIFFs preserving camera Make tags remain ordinary images
        for make in ["SONY", "NIKON CORPORATION", "PENTAX", "RICOH", "SAMSUNG"] {
            let mut cam = rgb_ifd(400, 300);
            cam.set(t::MAKE, Value::Ascii(make.into()));
            cam.set_child(t::EXIF_IFD, exif_size(400, 300));
            assert_eq!(probe(&padded(&[cam])), None, "{make} TIFF should not be probed as raw");
        }
    }

    // --- containers that are recognised but not decoded, with a preview ---

    #[test]
    fn private_containers_are_recognised_by_their_magic() {
        assert_eq!(probe(b"II\x1a\0\0\0HEAPCCDR\0\0"), Some(RawFormat::Crw));
        assert_eq!(probe(b"\0MRM\0\x01\0\0"), Some(RawFormat::Mrw));
        assert_eq!(probe(b"FOVb\x02\0\x02\0"), Some(RawFormat::X3f));
        for f in [RawFormat::Crw, RawFormat::Mrw, RawFormat::X3f] {
            assert!(!f.is_supported());
        }
        assert!(matches!(decode(b"\0MRM\0\x01\0\0"), Err(RawError::Unsupported(w)) if w.contains("Mrw")));
        // too short for the CIFF signature
        assert_eq!(probe(b"II\x1a\0\0\0HEAP"), None);
    }

    /// An IFD0 with a JPEG pointer but no `ImageWidth` / `ImageLength` (the Kodak KDC layout) is a raw
    /// container with a preview; a TIFF reader would call it malformed.
    #[test]
    fn ifd0_without_dimensions_but_with_a_jpeg_is_a_preview_container() {
        let jpeg: Vec<u8> = [&[0xffu8, 0xd8, 0xff, 0xc0, 0, 0x0b, 8, 0, 1, 0, 1, 1, 1, 0x11, 0][..], &[0x55; 40], &[0xff, 0xd9]].concat();
        let off = 8 + 2 + 24 + 4;
        let mut f = b"II*\0\x08\0\0\0\x02\0".to_vec();
        for (tag, v) in [(513u16, off as u32), (514, jpeg.len() as u32)] {
            f.extend_from_slice(&tag.to_le_bytes());
            f.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(&jpeg);
        assert_eq!(probe(&f), Some(RawFormat::OtherTiff));
        assert!(matches!(decode(&f), Err(RawError::Unsupported(_))));
        assert_eq!(embedded_preview(&f), Some(jpeg));
        // no pointer and no sub-IFDs: just a broken TIFF
        let g = write(&[IfdBuilder::new().with(t::MAKE, Value::Ascii("KODAK".into()))]);
        assert_eq!(probe(&g), None);
    }

    /// A GoPro GPR (a DNG with VC-5 coded raw data, Compression 9) is refused by the full decode with
    /// a reason that names the format, whatever its tile bytes hold; the header probe still
    /// describes it (so it imports).
    #[test]
    fn gopro_vc5_dng_is_unsupported_with_a_clear_reason() {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![16]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![32803]));
        ifd.set(t::COMPRESSION, Value::Short(vec![9]));
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        ifd.set_image(ImageData::Tiles { tile_width: 32, tile_height: 16, tiles: vec![vec![0x5au8; 400]] });
        let bytes = write(&[ifd]);
        assert_eq!(probe(&bytes), Some(RawFormat::Dng));
        let want = "DNG compression 9 (GoPro VC-5) is not decoded yet";
        let Err(RawError::Unsupported(why)) = decode(&bytes) else { panic!("expected Unsupported from decode") };
        assert_eq!(why, want);
        let info = probe_info(&bytes).expect("header probe describes the file");
        assert_eq!((info.width, info.height), (32, 16));
    }

    /// Compression 99 (not a registered TIFF value; Leaf MOS tiles) marks a raw even without a CFA tag.
    #[test]
    fn private_compression_99_is_a_raw() {
        let mut ifd = rgb_ifd(16, 12);
        ifd.set(t::COMPRESSION, Value::Short(vec![99]));
        assert_eq!(probe(&write(&[ifd])), Some(RawFormat::OtherTiff));
    }

    // --- TIFF raws that describe their raw IFD the way DNG does but carry no DNG version tag ---

    /// 8 x 6 samples, 16 bits, value `1000 + 100 y + x`.
    fn cfa_samples() -> Vec<u16> {
        (0..48u16).map(|i| 1000 + 100 * (i / 8) + i % 8).collect()
    }

    /// A small raw in the layout of the Hasselblad 3FR/FFF family: IFD0 is a reduced RGB image with the
    /// colour tags, a SubIFD holds the full-resolution CFA image (no `CFAPattern`, DNG-style levels and
    /// crop). `strip` is the CFA image's data and `compression` its coding.
    fn plain_cfa_tiff(compression: u16, strip: Vec<u8>, pattern: Option<[u8; 4]>) -> Vec<u8> {
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![8]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![6]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
        raw.set(t::COMPRESSION, Value::Short(vec![compression]));
        raw.set(t::BLACK_LEVEL, Value::Rational(vec![(256, 1)]));
        raw.set(t::WHITE_LEVEL, Value::Long(vec![60000]));
        raw.set(t::DEFAULT_CROP_ORIGIN, Value::Short(vec![2, 2]));
        raw.set(t::DEFAULT_CROP_SIZE, Value::Short(vec![4, 2]));
        if let Some(p) = pattern {
            raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
            raw.set(t::CFA_PATTERN_EP, Value::Byte(p.to_vec()));
        }
        raw.set_image(ImageData::Strips { rows_per_strip: 6, strips: vec![strip] });
        let mut ifd0 = rgb_ifd(4, 3);
        ifd0.set(t::MAKE, Value::Ascii("Hasselblad".into()));
        ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(5, 10), (-1, 10), (0, 10), (-5, 10), (12, 10), (3, 10), (-1, 10), (2, 10), (6, 10)]));
        ifd0.set(t::AS_SHOT_NEUTRAL, Value::Rational(vec![(2, 5), (1, 1), (3, 5)]));
        ifd0.add_sub_ifd(raw);
        write(&[ifd0])
    }

    fn le_words(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// Known answer: samples, black and white level, crop and colour come from the file's own tags; the
    /// layout it does not state is RGGB.
    #[test]
    fn uncompressed_cfa_tiff_without_a_dng_version_decodes() {
        let bytes = plain_cfa_tiff(1, le_words(&cfa_samples()), None);
        assert_eq!(probe(&bytes), Some(RawFormat::CfaTiff));
        let r = decode(&bytes).unwrap();
        assert_eq!((r.format, r.width, r.height, r.bits), (RawFormat::CfaTiff, 8, 6, 16));
        assert_eq!(r.data, RawData::U16(cfa_samples()));
        assert_eq!(r.cfa.as_ref().unwrap().name(), "RGGB");
        assert_eq!((r.black.mean(), r.white.clone()), (256.0, vec![60000.0]));
        assert_eq!(r.crop, Rect::new(2, 2, 4, 2));
        assert!(color::has_matrix(&r.color));
        assert_eq!(r.color.as_shot_neutral, Some([0.4, 1.0, 0.6]));
        // headers only: the same description without samples
        let i = probe_info(&bytes).unwrap();
        assert_eq!((i.format, i.cfa.as_ref().map(Cfa::name), i.crop), (RawFormat::CfaTiff, Some("RGGB".to_string()), Rect::new(2, 2, 4, 2)));
    }

    /// Without DNG's white-balance tag the file is left alone (a Kodak or Sinar TIFF with a CFA IFD, say).
    #[test]
    fn cfa_tiff_without_a_white_balance_tag_is_not_claimed() {
        let mut raw = IfdBuilder::new();
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![8]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![6]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set_image(ImageData::Strips { rows_per_strip: 6, strips: vec![le_words(&cfa_samples())] });
        let mut ifd0 = rgb_ifd(4, 3);
        ifd0.add_sub_ifd(raw);
        assert_eq!(probe(&write(&[ifd0])), Some(RawFormat::OtherTiff));
    }

    /// A stated `CFAPattern` wins over the RGGB default.
    #[test]
    fn cfa_tiff_keeps_a_stated_pattern() {
        let bytes = plain_cfa_tiff(1, le_words(&cfa_samples()), Some([2, 1, 1, 0]));
        assert_eq!(decode(&bytes).unwrap().cfa.unwrap().name(), "BGGR");
    }

    /// The lossless-JPEG coding of the same layout goes through the lossless decoder (one strip for the
    /// whole image, as the 3FR/FFF files have it).
    #[test]
    fn lossless_jpeg_cfa_tiff_decodes() {
        let strip = ljpeg::encode(&cfa_samples(), 8, 6, 1, 16, 1, 0);
        let bytes = plain_cfa_tiff(7, strip, None);
        assert_eq!(probe(&bytes), Some(RawFormat::CfaTiff));
        let r = decode(&bytes).unwrap();
        assert_eq!(r.data, RawData::U16(cfa_samples()));
        assert_eq!(r.cfa.unwrap().name(), "RGGB");
    }

    /// Hasselblad's own lossless-JPEG coding (selection value 8, as in FFF and most compressed 3FR files) of the
    /// same layout decodes: the file is read like the uncompressed one.
    #[test]
    fn selection_value_8_cfa_tiff_decodes() {
        let strip = ljpeg::tests::encode_hasselblad(&cfa_samples(), 8, 6);
        let bytes = plain_cfa_tiff(7, strip, None);
        assert_eq!(probe(&bytes), Some(RawFormat::CfaTiff));
        let r = decode(&bytes).unwrap();
        assert_eq!((r.format, r.width, r.height), (RawFormat::CfaTiff, 8, 6));
        assert_eq!(r.data, RawData::U16(cfa_samples()));
        assert_eq!(r.cfa.unwrap().name(), "RGGB");
        assert_eq!(probe_info(&bytes).unwrap().format, RawFormat::CfaTiff);
    }

    /// A lossless-JPEG scan whose predictor selection value is neither 0 to 7 (T.81) nor 8 (Hasselblad) is not a
    /// coding the lossless decoder knows: the file stays an undecodable raw, with that as the reason, and keeps the
    /// stand-in preview an unsupported container gets.
    #[test]
    fn cfa_tiff_with_an_unknown_predictor_stays_undecodable() {
        let mut strip = ljpeg::encode(&cfa_samples(), 8, 6, 1, 16, 1, 0);
        let sos = strip.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        let ss = sos + 2 + 2 + 1 + 2;
        assert_eq!(strip[ss], 1, "selection value byte");
        strip[ss] = 9;
        assert!(matches!(ljpeg::decode(&strip, 1 << 20), Err(RawError::Unsupported(w)) if w.contains("selection value 9")));
        let bytes = plain_cfa_tiff(7, strip, None);
        assert_eq!(probe(&bytes), Some(RawFormat::OtherTiff));
        assert!(!RawFormat::OtherTiff.is_supported());
        let Err(RawError::Unsupported(why)) = decode(&bytes) else { panic!("expected Unsupported") };
        assert!(why.contains("lossless JPEG") && why.contains("selection value 9"), "{why}");
    }

    /// Other codings of a CFA IFD (here Deflate) are not claimed by the plain reader.
    #[test]
    fn cfa_tiff_with_another_coding_is_not_claimed() {
        let bytes = plain_cfa_tiff(8, vec![0; 96], None);
        assert_eq!(probe(&bytes), Some(RawFormat::OtherTiff));
    }

    /// A Samsung-branded body whose maker note has a Pentax layout is read by the Pentax reader; another
    /// Samsung file stays with the Samsung format.
    #[test]
    fn samsung_make_with_a_pentax_maker_note_is_pef() {
        let with_note = |note: &[u8], cfa: bool| {
            let mut ifd = rgb_ifd(16, 12);
            if cfa {
                ifd.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
            }
            ifd.set(t::MAKE, Value::Ascii("SAMSUNG TECHWIN".into()));
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(note.to_vec()));
            ifd.set_child(t::EXIF_IFD, exif);
            write(&[ifd])
        };
        assert_eq!(probe(&with_note(b"AOC\0MM\0\x01\0\0\0\0\0\0", true)), Some(RawFormat::Pef));
        assert_eq!(probe(&with_note(b"PENTAX \0MM\0\0\0\0\0\0\0", true)), Some(RawFormat::Pef));
        assert_eq!(probe(&with_note(b"STMN100\0\0\0\0\0\0\0\0\0", true)), Some(RawFormat::Srw));
        // without raw evidence (an RGB image from such a body) neither reader claims it (#281)
        assert_eq!(probe(&with_note(b"AOC\0MM\0\x01\0\0\0\0\0\0", false)), None);
        assert_eq!(probe(&with_note(b"STMN100\0\0\0\0\0\0\0\0\0", false)), None);
    }

    #[test]
    fn probe_rejects_junk() {
        assert_eq!(probe(b""), None);
        assert_eq!(probe(b"hello world, not a raw"), None);
        assert_eq!(probe(b"\0\0\0\x18ftypcrx \0\0\0\x01"), Some(RawFormat::Cr3));
        assert_eq!(probe(b"FUJIFILMCCD-RAW 0201"), Some(RawFormat::Raf));
        assert!(decode(b"FUJIFILMCCD-RAW 0201").is_err());
        assert_eq!(decode(b"junk"), Err(RawError::NotRaw));
    }
}
