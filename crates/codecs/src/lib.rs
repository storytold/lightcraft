//! Standard image codecs (layer L1).
//!
//! - [`sniff`] detects the container format (and flags camera raws for `lcc-raw`).
//! - [`decode`] decodes JPEG, PNG, TIFF, WebP, GIF, BMP, PSD (merged composite) and JPEG XL into
//!   **linear-light** [`Rgb32f`] in the source's own primaries, described by [`SourceSpace`]; ICC
//!   profiles (matrix/TRC exactly, LUT/CMYK via the `moxcms` CMS), 16-bit and float precision are
//!   preserved. EXIF/XMP/ICC blobs are passed through untouched; orientation is reported, never applied.
//! - [`to_working`] converts a decode result to linear Rec.2020 D65 (the pipeline working space).
//! - [`read_header`] reads a file's stored dimensions and orientation from its headers, without
//!   decoding pixels (import probes).
//! - [`decode_thumbnail`] is the fast path for grid thumbnails (EXIF thumbnail or DCT-scaled decode).
//! - [`encode`] writes JPEG, PNG, TIFF, lossless WebP and, on native targets, AVIF (feature `avif`) and
//!   JPEG XL ([`encode_jxl`], feature `jxl`), embedding
//!   ICC/EXIF/XMP. [`icc::write_matrix_trc`] builds profiles for export.
//! - [`video`] encodes AV1 video (native, feature `avif`) and writes it as MP4 or WebM.
//!
//! No decoder panics on malformed input (property-tested).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
mod avif;
mod convert;
pub mod display;
pub mod encode;
pub mod exif;
pub mod gainmap;
mod heif;
pub mod icc;
mod jpeg;
pub mod jpeg_par;
#[cfg(feature = "jxl")]
mod jxl;
mod jxl_encode;
mod other;
mod png_codec;
pub mod print;
pub mod printer;
pub mod proof;
mod psd;
mod sniff;
pub mod space;
mod tiff_codec;
pub mod video;
mod webp;

pub use encode::{
    ChromaSubsampling, EncodeImage, EncodeMeta, HDR_REFERENCE_WHITE_NITS, HLG_REFERENCE_PEAK_NITS, HdrTransfer, Samples, TiffCompression,
    encode_avif, encode_avif_hdr, encode_avif_pq, encode_jpeg, encode_png, encode_png_pq, encode_psd, encode_tiff, encode_tiff_associated,
    encode_webp_lossless, hlg_signal,
};
pub use jxl_encode::{JxlColour, JxlEffort, JxlOptions, encode_jxl};
pub use sniff::{Format, sniff};
pub use space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};

use lcc_raster::{Plane, Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};

/// Codec errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unrecognised image format")]
    UnknownFormat,
    #[error("{0:?} decoding is not supported: {1}")]
    Unsupported(Format, &'static str),
    #[error("malformed {0:?} data: {1}")]
    Malformed(Format, String),
    #[error("image too large: {0}×{1}")]
    TooLarge(u64, u64),
    #[error("encode error: {0}")]
    Encode(String),
}

/// `w × h × c` samples, or [`Error::TooLarge`] when that overflows `usize` (a 32-bit `usize` on wasm32 wraps on
/// ordinary 16-bit dimensions).
pub(crate) fn checked_samples(w: usize, h: usize, c: usize) -> Result<usize> {
    w.checked_mul(h).and_then(|n| n.checked_mul(c)).ok_or(Error::TooLarge(w as u64, h as u64))
}

pub type Result<T> = std::result::Result<T, Error>;

/// Decode options.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecodeOptions {
    /// Fit the result within this box (never upscales). JPEG uses DCT-domain scaling (1/2, 1/4,
    /// 1/8) before a final linear-light resample; other formats decode fully then resample.
    pub max_size: Option<(u32, u32)>,
    /// Refuse images with more pixels than this (default [`DEFAULT_MAX_PIXELS`], 512 MP).
    pub max_pixels: u64,
    /// Refuse images whose samples, in the file's own layout (before conversion to float), need more
    /// bytes than this (default [`DEFAULT_MAX_BYTES`], 4 GiB): the buffer a format decoder allocates
    /// from the header alone, which a few-KB compressed file can make huge (issues #2116, #2125).
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}

/// Default [`DecodeOptions::max_pixels`]: 512 MP, the largest image Lightroom Classic opens (65,000 px
/// a side and 512 megapixels), so every file it reads is in range.
pub const DEFAULT_MAX_PIXELS: u64 = 512_000_000;

/// Default [`DecodeOptions::max_bytes`]: 4 GiB, a 512 MP image at 8 bytes a pixel (RGBA, 16 bits).
pub const DEFAULT_MAX_BYTES: u64 = 4 << 30;

fn default_max_bytes() -> u64 {
    DEFAULT_MAX_BYTES
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { max_size: None, max_pixels: DEFAULT_MAX_PIXELS, max_bytes: DEFAULT_MAX_BYTES }
    }
}

impl DecodeOptions {
    pub fn fit(w: u32, h: u32) -> Self {
        DecodeOptions { max_size: Some((w, h)), ..Default::default() }
    }
}

/// A decoded image.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub format: Format,
    /// Linear light, in the primaries described by [`Decoded::space`].
    pub image: Rgb32f,
    /// Straight (unassociated) alpha 0..1, when the source has transparency.
    pub alpha: Option<Plane>,
    /// A film scanner's infrared pass (a VueScan or SilverFast "RGBI" TIFF: four 16-bit samples
    /// a pixel, the fourth lit by the scanner's infrared lamp), 0..1 at `image`'s size: bright
    /// where the film is clear, dark under dust, hairs and scratches, which block infrared while
    /// the dyes pass it. Not transparency: such a file decodes as an opaque RGB image.
    pub infrared: Option<Plane>,
    pub space: SourceSpace,
    /// Bits per sample in the file (8, 16, 32, …).
    pub bit_depth: u8,
    /// Sample format in the file was floating point.
    pub float: bool,
    pub has_alpha: bool,
    /// The source is grayscale (expanded to RGB in `image`).
    pub grayscale: bool,
    /// The source was CMYK (converted).
    pub cmyk: bool,
    /// EXIF orientation value 1..=8 (1 when absent). **Not applied** to `image`.
    pub orientation: u16,
    /// TIFF-structured EXIF bytes (starting at the `II`/`MM` byte-order mark), if embedded.
    /// For TIFF files the file itself is the TIFF structure and this is `None`.
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<String>,
    pub icc: Option<Vec<u8>>,
    /// Dimensions of `image`.
    pub width: u32,
    pub height: u32,
    /// Full-resolution dimensions of the source (differs from `width`/`height` when scaled).
    pub source_width: u32,
    pub source_height: u32,
}

impl Decoded {
    /// Pixels converted to linear Rec.2020 D65.
    pub fn to_working(&self) -> Rgb32f {
        to_working(self)
    }

    /// Display-encoded 8-bit sRGB (gamut clipped), with alpha.
    pub fn to_srgb8(&self) -> Rgba8 {
        let m = self.space.to_space(&lcc_color::SRGB);
        let ident = m == lcc_color::Mat3::IDENTITY;
        let m = m.to_f32();
        let enc = lcc_color::transfer::encode_srgb8;
        let mut out = Rgba8::new(self.image.width, self.image.height);
        let alpha = self.alpha.as_ref();
        lcc_raster::par_rows(&mut out.data, self.image.width, |y, row| {
            let src = self.image.row(y);
            let a = alpha.map(|a| a.row(y));
            for (x, (o, p)) in row.iter_mut().zip(src).enumerate() {
                let q = if ident { *p } else { mat_apply(&m, *p) };
                let al = a.map(|a| (a[x].clamp(0.0, 1.0) * 255.0 + 0.5) as u8).unwrap_or(255);
                *o = [enc(q[0]), enc(q[1]), enc(q[2]), al];
            }
        });
        out
    }
}

#[inline]
pub(crate) fn mat_apply(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

/// Decode any supported format (see [`Format::can_decode`]).
///
/// Malformed input yields an error, never a panic: on unwinding targets a panic inside a third-party
/// decoder is caught and reported as [`Error::Malformed`].
pub fn decode(bytes: &[u8], opts: DecodeOptions) -> Result<Decoded> {
    let format = sniff(bytes).ok_or(Error::UnknownFormat)?;
    std::panic::catch_unwind(|| decode_unguarded(bytes, format, &opts)).unwrap_or_else(|_| Err(Error::Malformed(format, "decoder panicked".into())))
}

/// What a file's headers say about its image: enough to catalogue it without decoding pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub format: Format,
    /// Full-resolution stored dimensions, orientation **not** applied ([`Decoded::source_width`]).
    pub width: u32,
    pub height: u32,
    /// EXIF/TIFF orientation 1..=8, 1 when absent ([`Decoded::orientation`]).
    pub orientation: u16,
}

/// Read the stored dimensions and orientation of any format [`decode`] supports, agreeing with
/// what a decode reports, without decoding pixels: JPEG, PNG, TIFF, WebP, PSD, HEIF and AVIF are read
/// from their headers (markers, chunks, IFD, resources, boxes); GIF and BMP from theirs (a small one
/// also has its pixel data checked); JPEG XL from its codestream, with its frames parsed but never
/// rendered (upstream #2182).
///
/// The header readers also refuse what a decode would refuse before reaching the pixels (an
/// unsupported layout, compression or frame type) and files whose image data is cut short
/// (truncated: the data runs past the end of the file, or a JPEG's markers don't reach its
/// end-of-image marker). Damage *inside* compressed data that is all present is only found by a
/// decode. Malformed input yields an error, never a panic (as for [`decode`], a panic inside a
/// third-party parser is caught on unwinding targets).
pub fn read_header(bytes: &[u8]) -> Result<Header> {
    let format = sniff(bytes).ok_or(Error::UnknownFormat)?;
    std::panic::catch_unwind(|| read_header_unguarded(bytes, format))
        .unwrap_or_else(|_| Err(Error::Malformed(format, "header reader panicked".into())))
}

/// [`read_header`] without the panic guard (for fuzzing our own code paths).
#[doc(hidden)]
pub fn read_header_unguarded(bytes: &[u8], format: Format) -> Result<Header> {
    let (width, height, orientation) = match format {
        Format::Jpeg => jpeg::header(bytes)?,
        Format::Png => png_codec::header(bytes)?,
        Format::Tiff => tiff_codec::header(bytes)?,
        Format::WebP => webp::header(bytes)?,
        Format::Psd => psd::header(bytes)?,
        Format::Heif => heif::header(bytes)?,
        #[cfg(not(target_arch = "wasm32"))]
        Format::Avif => avif::header(bytes)?,
        Format::Gif | Format::Bmp => other::header(bytes, format)?,
        #[cfg(feature = "jxl")]
        Format::Jxl => jxl::header(bytes)?,
        // what a decode refuses outright (not decodable in this build)
        _ => {
            let d = decode_unguarded(bytes, format, &DecodeOptions::fit(64, 64))?;
            (d.source_width, d.source_height, d.orientation)
        }
    };
    Ok(Header { format, width, height, orientation })
}

/// GIF and BMP images up to this many pixels have their pixel data read by [`read_header`] (a 16 MB
/// scratch buffer at most).
pub const HEADER_CHECK_PIXELS: u64 = 1 << 22;

/// Decode a JPEG preview with its enclosing container's colour space as a fallback.
/// A JPEG carrying ICC or EXIF metadata keeps its own interpretation. The fallback is applied
/// before transfer decoding and linear-light resizing, never to already-resampled pixels.
pub fn decode_jpeg_with_fallback(bytes: &[u8], opts: DecodeOptions, fallback: NamedSpace) -> Result<Decoded> {
    std::panic::catch_unwind(|| jpeg::decode_with_fallback(bytes, &opts, Some(fallback)))
        .unwrap_or_else(|_| Err(Error::Malformed(Format::Jpeg, "decoder panicked".into())))
}

/// [`decode`] without the panic guard (for fuzzing our own code paths).
#[doc(hidden)]
pub fn decode_unguarded(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    let opts = *opts;
    // The reason a format can't be decoded lives in one place, `Format::not_decodable`.
    let unsupported = || Err(Error::Unsupported(format, format.not_decodable().unwrap_or("not decodable in this build")));
    let d = match format {
        Format::Jpeg => jpeg::decode(bytes, &opts),
        Format::Png => png_codec::decode(bytes, &opts),
        Format::Tiff => tiff_codec::decode(bytes, &opts),
        Format::WebP => webp::decode(bytes, &opts),
        Format::Gif | Format::Bmp => other::decode(bytes, format, &opts),
        Format::Psd => psd::decode(bytes, &opts),
        #[cfg(feature = "jxl")]
        Format::Jxl => jxl::decode(bytes, &opts),
        #[cfg(not(feature = "jxl"))]
        Format::Jxl => unsupported(),
        Format::Heif => heif::decode(bytes, &opts),
        #[cfg(not(target_arch = "wasm32"))]
        Format::Avif => avif::decode(bytes, &opts),
        #[cfg(target_arch = "wasm32")]
        Format::Avif => unsupported(),
        Format::RawTiffLike | Format::RawOther => unsupported(),
    }?;
    check_has_pixels(format, &d.image)?;
    Ok(d)
}

/// An image with no pixels (a malformed header claiming zero width or height, a `w × h` that
/// overflows, or a buffer shorter than its size) is refused, so nothing downstream sees an empty
/// image.
fn check_has_pixels(format: Format, image: &Rgb32f) -> Result<()> {
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|n| image.data.len() < n) {
        return Err(Error::Malformed(format, "image has no pixels".into()));
    }
    Ok(())
}

#[cfg(test)]
mod has_pixels_tests {
    use super::*;

    fn image(width: usize, height: usize, len: usize) -> Rgb32f {
        Rgb32f { width, height, data: vec![[0.0; 3]; len] }
    }

    #[test]
    fn decoded_image_without_pixels_is_refused() {
        for (w, h, len) in [(0, 4, 0), (4, 0, 0), (usize::MAX, 2, 1), (4, 4, 15)] {
            let r = check_has_pixels(Format::Png, &image(w, h, len));
            assert!(matches!(r, Err(Error::Malformed(Format::Png, _))), "{w}x{h} with {len} pixels");
        }
        // decoded image with all pixels is accepted
        {
            assert!(check_has_pixels(Format::Png, &image(4, 4, 16)).is_ok());
            assert!(check_has_pixels(Format::Png, &image(1, 1, 1)).is_ok());
        }
    }
}

/// Convert a decode result to linear Rec.2020 D65 (Bradford-adapted), the develop working space.
///
/// The working space has no alpha: transparency is flattened onto white, in linear light like the rest
/// of the pipeline (issue #1212).
pub fn to_working(d: &Decoded) -> Rgb32f {
    let mut img = if d.space.is_working() {
        d.image.clone()
    } else {
        let m = d.space.to_working().to_f32();
        d.image.map(|p| mat_apply(&m, p))
    };
    // white is (1, 1, 1) in the working space too: the conversion is white-preserving
    if let Some(a) = d.alpha.as_ref().filter(|a| a.width == img.width && a.height == img.height) {
        for (p, &av) in img.data.iter_mut().zip(&a.data) {
            let av = av.clamp(0.0, 1.0);
            *p = p.map(|c| c * av + (1.0 - av));
        }
    }
    img
}

/// Undo [`to_working`]'s flatten onto white for an output that keeps transparency (issue #2976):
/// the straight colour `c` from the flattened `f = c·a + (1 − a)` and the alpha `a` it was flattened
/// with. Exact in linear light, and after any linear resample of both (fitting, orienting), where it
/// gives the alpha-weighted colour. Pixels with almost no alpha keep the white: their colour is not
/// recoverable, and white is what edits around them saw before. `None` when `alpha` does not match
/// `img`.
pub fn unflatten(img: &Rgb32f, alpha: &Plane) -> Option<Rgb32f> {
    if (alpha.width, alpha.height) != (img.width, img.height) || alpha.data.len() != img.data.len() {
        return None;
    }
    const VISIBLE: f32 = 1.0 / 1024.0;
    let mut out = img.clone();
    for (p, &av) in out.data.iter_mut().zip(&alpha.data) {
        if av.is_finite() && av >= VISIBLE {
            let av = av.min(1.0);
            // (clamped at 0: a prefilter's negative lobe next to an edge, divided by a small alpha, undershoots)
            *p = p.map(|f| ((f - (1.0 - av)) / av).max(0.0));
        }
    }
    Some(out)
}

/// Where a thumbnail came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThumbnailSource {
    /// The JPEG thumbnail embedded in EXIF (IFD1).
    ExifThumbnail,
    /// An MPF (CIPA DC-007) preview image embedded in a JPEG.
    MpfPreview,
    /// A (DCT-)scaled decode of the main image.
    Scaled,
}

/// A small display-ready image.
#[derive(Clone, Debug)]
pub struct Thumbnail {
    /// 8-bit sRGB with alpha, fitted within `max_edge` (orientation **not** applied).
    pub image: Rgba8,
    pub orientation: u16,
    pub source: ThumbnailSource,
    pub source_width: u32,
    pub source_height: u32,
}

/// Thumbnail request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailOptions {
    /// Fit the result within `max_edge × max_edge`.
    pub max_edge: u32,
    /// Accept an embedded preview whose long edge is at least this (it is then returned at its own,
    /// smaller size). Defaults to `max_edge`, i.e. only previews that need no upscaling. Lower it
    /// (e.g. 160 for typical EXIF thumbnails) for an instant placeholder.
    pub min_embedded_edge: u32,
    /// Maximum source pixels for fallback decoding (default 64 MP). Non-JPEG formats
    /// decode at source resolution before resizing, so output size does not bound memory.
    #[serde(default = "thumbnail_max_pixels")]
    pub max_pixels: u64,
}

fn thumbnail_max_pixels() -> u64 {
    64_000_000
}

impl ThumbnailOptions {
    pub fn new(max_edge: u32) -> Self {
        ThumbnailOptions { max_edge, min_embedded_edge: max_edge, max_pixels: thumbnail_max_pixels() }
    }
}

/// Fast thumbnail: an embedded JPEG preview (EXIF IFD1 thumbnail or MPF preview) at least `max_edge`
/// on its long side if one exists, else a scaled decode. The result fits within `max_edge × max_edge`.
pub fn decode_thumbnail(bytes: &[u8], max_edge: u32) -> Result<Thumbnail> {
    decode_thumbnail_with(bytes, &ThumbnailOptions::new(max_edge))
}

/// [`decode_thumbnail`] with options.
pub fn decode_thumbnail_with(bytes: &[u8], opts: &ThumbnailOptions) -> Result<Thumbnail> {
    let max_edge = opts.max_edge.max(1);
    let min_edge = opts.min_embedded_edge.clamp(1, max_edge);
    if sniff(bytes) == Some(Format::Jpeg)
        && let Ok(Some(t)) = std::panic::catch_unwind(|| jpeg::embedded_thumbnail(bytes, max_edge, min_edge))
    {
        return Ok(t);
    }
    let d = decode(bytes, DecodeOptions { max_size: Some((max_edge, max_edge)), max_pixels: opts.max_pixels, ..Default::default() })?;
    Ok(Thumbnail {
        image: d.to_srgb8(),
        orientation: d.orientation,
        source: ThumbnailSource::Scaled,
        source_width: d.source_width,
        source_height: d.source_height,
    })
}

#[cfg(test)]
mod checked_samples_tests {
    /// Sizes whose sample count overflows `usize` (on wasm32, any 65535×65535×3 picture) are an error, not a wrap.
    #[test]
    fn overflowing_sample_counts_are_errors() {
        assert_eq!(super::checked_samples(4, 3, 3).ok(), Some(36));
        assert!(super::checked_samples(usize::MAX, 2, 1).is_err());
        assert!(super::checked_samples(usize::MAX / 2, 1, 3).is_err());
    }
}
