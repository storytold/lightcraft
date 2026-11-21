//! RAW decoding for LightCraft.
//!
//! - [`probe`] recognises raw containers; [`decode`] turns a file into a [`RawImage`] (sensor data + everything
//!   needed to render it: CFA, black/white levels, active area, default crop, orientation, DNG colour tags,
//!   opcode lists, [`Metadata`]); [`embedded_preview`] returns the largest embedded JPEG (or DNG 1.7 JPEG XL) preview;
//!   [`semantic_masks`] reads a DNG's semantic masks (segmentation mattes, e.g. iPhone ProRAW's sky matte).
//! - [`RawImage::normalized`] subtracts black, scales white to 1.0 and crops to the active area (applying DNG
//!   `OpcodeList1`/`OpcodeList2`); [`demosaic`] turns CFA data into camera-RGB [`Rgb32f`];
//!   [`RawImage::develop`] does all of it plus `OpcodeList3` and the default crop ([`RawImage::develop_balanced`]
//!   demosaics the mosaic white balanced, for pictures that will be); [`RawImage::develop_binned`]
//!   produces the same at 1/k of the size straight from the mosaic (previews, thumbnails;
//!   [`RawImage::develop_binned_masked`] with the mask of blocks holding clipped samples, for [`highlight`]).
//! - [`color`] implements the DNG colour model (dual-illuminant interpolation, forward matrices, white balance)
//!   and produces camera → linear Rec.2020 D65 matrices; [`profile`] reads and applies a DNG's own profile
//!   look tables and tone curve, and [`gaintable`] its gain table map (Apple ProRAW's local tone mapping).
//!
//! Formats: DNG (uncompressed, lossless JPEG, lossy JPEG (Smart Previews), Deflate incl. floating point, JPEG XL (DNG 1.7,
//! `jxl` feature, on by default), tiled/stripped, CFA and LinearRaw),
//! Canon CR2 / CR3 (lossless CRX Bayer and version 0x100/0x200 C-RAW) / CRW (the CIFF bodies of 2000–2004; clean-room-derived
//! compression, `vendor::crw`; the PowerShot G5's anomalous coding is refused), Nikon NEF/NRW (uncompressed, Huffman lossless / lossy compressed), Sony ARW (uncompressed, ARW2, lossless), Fujifilm RAF (uncompressed Bayer
//! and X-Trans, lossless and lossy compressed), Panasonic RW2 / Leica RWL / Panasonic RAW (every raw format: compressed 4 and 6, the prefix-coded strips of 8,
//! packed 2/5/7, the 16-bit words of the oldest bodies), Pentax PEF (uncompressed, Huffman), Olympus ORF (uncompressed: 16-bit words, 12-bit XZ-2 words,
//! two-field 12-bit samples and 12-bit 16-byte blocks of the E-300/E-330/E-500).
//! [`embedded_preview`] covers these containers' JPEG previews. Variants we can't decode yet (compressed Olympus ORF, CR3 unverified marker families / C-RAW configurations) return [`RawError::Unsupported`]; each vendor module documents its sources
//! (public specifications, tag-name documentation, black-box analysis of CC0 samples) and gaps. Non-DNG files carry no
//! colour matrix: [`spectral`] has matrices fitted to measured spectral sensitivities for 52 models, and
//! otherwise [`color`] falls back to a documented neutral model. The decoders never panic on malformed input.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod binned;
pub mod color;
mod convert;
pub mod dcp;
pub mod demosaic;
pub mod dngwrite;
pub mod gaintable;
pub mod highlight;
mod hotpixel;
pub mod kelvin_builtin;
pub mod kelvin_presets;
pub use craft_raw::lens_metadata;
pub mod opcodes;
pub mod profile;
mod saturation;
pub mod semantic;
pub use craft_raw::sparse;
pub mod spectral;
mod wb_estimate;

pub use craft_raw::{
    BlackLevel, Cfa, MAX_SAMPLES, PREVIEW_PREFIX, PreviewColorSpace, PreviewRange, RawData, RawError, RawFormat, Rect, Result, SemanticMask,
    bounded_preview, embedded_preview, embedded_preview_color_space, embedded_preview_covering, embedded_preview_dynamic_range_optimized,
    embedded_preview_from_range, ljpeg, probe, semantic_masks, stretched_height,
};
pub use demosaic::{Method, demosaic};
pub use dngwrite::{DngCompression, DngWriteOptions, write_dng, write_rendered_dng};
pub use hotpixel::HOT_PIXEL_MAX_SHARE;
pub use lens_metadata::LensMetadata;
pub use lightcraft_color::Mat3;
pub use lightcraft_geom::Orientation;
pub use lightcraft_meta::Metadata;
pub use lightcraft_raster::Rgb32f;
pub use opcodes::{Opcode, OpcodeLists};

use lightcraft_color::Xy;
use serde::{Deserialize, Serialize};

/// Checked product of input-derived dimensions, in u64 so it cannot wrap
/// on either 32-bit (wasm32) or 64-bit targets. The arguments are u64 too, so callers can feed
/// unchecked products of header values (row bytes, sample counts) without truncating on 32-bit.
///
/// Returns `None` on overflow, zero, or when the product exceeds [`MAX_SAMPLES`].
pub(crate) fn checked_pixel_count(width: u64, height: u64) -> Option<u64> {
    width.checked_mul(height).filter(|&n| n > 0 && n <= MAX_SAMPLES as u64)
}

/// Whether [`decode`] reads this container (possibly not every compression inside it): `craft-raw`'s makers and
/// the ones still decoded here (Canon CR3, Sony ARW, Fujifilm RAF, Panasonic RW2).
pub fn is_supported(format: RawFormat) -> bool {
    format.is_supported()
}

#[cfg(test)]
pub(crate) fn bayer(s: &'static str) -> Cfa {
    Cfa::bayer(s).unwrap_or(Cfa { width: 2, height: 2, origin_x: 0, origin_y: 0, pattern: vec![0, 1, 1, 2] })
}

/// A CFA pattern this crate can address: 1 to 16 sites a side, one colour index per site.
pub(crate) fn cfa_valid(c: &Cfa) -> bool {
    c.width > 0 && c.height > 0 && c.width <= 16 && c.height <= 16 && c.pattern.len() == c.width * c.height
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
    convert::decode_reduced(bytes, min_long)
}

/// Everything [`decode`] learns about a raw file except its samples: geometry, orientation, CFA,
/// colour data, as-shot white balance, opcode lists (embedded lens corrections), metadata — read
/// from the headers without decompressing the pixel data, for imports. Equal to
/// [`RawImage::info`] of the decoded image. Black and white levels are not included (some formats
/// measure them from the samples).
///
/// A few formats derive part of this from the samples themselves (Nikon NEF: optically masked trailing columns;
/// Olympus ORF: the bit depth of 16-bit files, and the CFA phase of files without an Exif `CFAPattern`; Pentax PEF
/// without crop tags: dark borders; Epson ERF and Mamiya MEF: the white balance, estimated from the samples).
/// Those read the samples they need and drop them; a compressed NEF reads only the rows its masked columns are
/// judged from (issue #708).
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
    if probe(bytes).is_none() {
        return Err(RawError::NotRaw);
    }
    convert::decode(bytes, mode)
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

impl RawInfo {
    /// Size of the developed image before orientation: the default crop, else the active area, with the rows
    /// stretched for non-square pixels.
    pub fn developed_size(&self) -> (usize, usize) {
        let c = self.crop.clipped(self.active_area.width, self.active_area.height);
        let (w, h) = if c.width > 1 && c.height > 1 { (c.width, c.height) } else { (self.active_area.width, self.active_area.height) };
        (w, stretched_height(h, self.pixel_aspect))
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
    /// A full CR3 decode also includes compensation for sample-derived white headroom;
    /// a header-only probe cannot determine that adjustment.
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
            && (!cfa_valid(c) || c.pattern.iter().any(|&p| p > 2))
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
    ///
    /// The demosaic sees the mosaic as the sensor recorded it. A picture that will be white balanced should be
    /// developed with [`Self::develop_balanced`].
    pub fn develop(&self, method: Method) -> Result<Rgb32f> {
        let n = self.normalized()?;
        // with no `OpcodeList3` to apply to the whole picture first, AHD demosaics just the default crop (the same
        // pixels, without demosaicing the margin and then copying the crop out of the full picture)
        if self.opcodes.list3.is_empty() && method == Method::Ahd {
            let c = self.develop_crop(n.width, n.height);
            if let Some(rgb) = demosaic::ahd_window(&n, c) {
                drop(n);
                return Ok(self.stretched(rgb));
            }
        }
        let rgb = demosaic(&n, method);
        drop(n);
        Ok(self.finish_demosaiced(rgb))
    }

    /// [`Self::develop`] with the demosaic run on the white-balanced mosaic: each normalised sample is multiplied by
    /// `wb`'s multiplier for its CFA colour (relative to green's) before the demosaic, and each channel of the result
    /// is divided by it after. The result is camera RGB on [`Self::develop`]'s scale (white level = 1.0, not white
    /// balanced), so highlight reconstruction's clip test and the colour model applied after it keep their units.
    ///
    /// Why (issue #1328): AHD, PPG and the X-Trans directional method complete red and blue from neighbouring colour
    /// differences (C − G). A neutral surface has C = G only once it is white balanced; in camera RGB its R − G is
    /// (1 / wb_r − 1) · G, which changes with brightness, so across a steep edge the bright side's difference drives
    /// the dark side's red or blue to zero or below: green, cyan, yellow or magenta outlines along white edges and
    /// beside clipped specular highlights. Balanced, a neutral edge has no colour difference to carry over.
    ///
    /// A constant-colour mosaic still comes back as that colour (to `f32` rounding). Bilinear interpolates each colour
    /// from its own samples alone, so it gives the same either way and skips the scaling; so do monochrome and
    /// already demosaiced data, a `wb` that isn't three positive finite numbers or whose red or blue multiplier differs
    /// from green's by more than a factor of 64, and a mosaic with a non-finite sample or one whose magnitude, times
    /// the largest gain or divided by the smallest, exceeds 1e30: those develop as [`Self::develop`].
    pub fn develop_balanced(&self, method: Method, wb: [f32; 3]) -> Result<Rgb32f> {
        self.develop_balanced_with(method, wb, None).map(|(rgb, _)| rgb)
    }

    /// [`Self::develop_balanced`] and, per developed pixel, what [`highlight::reconstruct_masked`] needs to know
    /// about the clipped samples around it (one entry per pixel). A pixel whose own sample is unclipped gets
    /// [`highlight::FROM_CLIPPED_R`], … for each other colour it has a clipped sample (at least `clip`, normalised)
    /// of within [`CLIP_REACH`] px, and the colour it measured ([`highlight::MEASURED_SHIFT`]): the demosaic made
    /// those channels partly from clipped samples, so below the clip their values are unknown (above or below the
    /// truth) and reconstruction rebuilds them from the measured one. Told by value alone, they kept the colour
    /// difference the clip gave them: magenta or green rims around clipped skies and lights. A pixel whose own sample
    /// clipped gets [`highlight::CLIPPED_R`], … for that colour only, and its other channels are told by value as
    /// before. The pixel's own sample is a measurement whatever its neighbours did: a dark twig against a clipped
    /// sky keeps its level.
    ///
    /// The mask is `None` when no sample is clipped, when there is no single-plane CFA, or when the file has any
    /// `OpcodeList3`, or when non-square pixels stretch the picture (the mosaic's positions then differ from the
    /// picture's): highlight reconstruction tells clipping by value, as before.
    pub fn develop_balanced_masked(&self, method: Method, wb: [f32; 3], clip: f32) -> Result<(Rgb32f, Option<Vec<u8>>)> {
        self.develop_balanced_with(method, wb, Some(clip))
    }

    fn develop_balanced_with(&self, method: Method, wb: [f32; 3], clip: Option<f32>) -> Result<(Rgb32f, Option<Vec<u8>>)> {
        let mut n = self.normalized()?;
        let mask = clip.and_then(|clip| self.clip_mask(&n, clip));
        let rgb = match (n.cfa.clone(), balance_gains(wb)) {
            (Some(cfa), Some(gains)) if n.cpp == 1 && method != Method::Bilinear => {
                if !scale_mosaic(&mut n.data, n.width, &cfa, gains) {
                    // a sample too large for the gains, or not finite: the mosaic, read afresh, develops unbalanced
                    drop(n);
                    let n = self.normalized()?;
                    return Ok((self.finish_demosaiced(demosaic(&n, method)), mask));
                }
                let mut rgb = demosaic(&n, method);
                drop(n);
                lightcraft_raster::par_rows(&mut rgb.data, rgb.width, |_, row| {
                    for p in row {
                        p[0] /= gains[0];
                        p[2] /= gains[2];
                    }
                });
                rgb
            }
            _ => demosaic(&n, method),
        };
        Ok((self.finish_demosaiced(rgb), mask))
    }

    /// [`Self::develop_balanced_masked`]'s mask from the normalised mosaic `n`, cropped as the development is.
    fn clip_mask(&self, n: &Normalized, clip: f32) -> Option<Vec<u8>> {
        let cfa = n.cfa.as_ref()?;
        let (w, h) = (n.width, n.height);
        if !self.opcodes.list3.is_empty() || n.cpp != 1 || w == 0 || h == 0 || n.data.len() != w * h || clip.is_nan() || clip <= 0.0 {
            return None;
        }
        let c = self.develop_crop(w, h);
        if stretched_height(c.height, self.pixel_aspect) != c.height {
            return None;
        }
        use rayon::prelude::*;
        if !n.data.par_iter().any(|&v| v >= clip) {
            return None;
        }
        let bit = |c: u8| [highlight::FROM_CLIPPED_R, highlight::FROM_CLIPPED_G, highlight::FROM_CLIPPED_B].get(c as usize).copied().unwrap_or(0);
        let own = |c: u8| [highlight::CLIPPED_R, highlight::CLIPPED_G, highlight::CLIPPED_B].get(c as usize).copied().unwrap_or(0);
        // per row: the colours of clipped samples within CLIP_REACH columns
        let mut rows = vec![0u8; w * h];
        rows.par_chunks_mut(w).zip(n.data.par_chunks(w)).enumerate().for_each(|(y, (row, samples))| {
            let own: Vec<u8> = samples.iter().enumerate().map(|(x, &v)| if v >= clip { bit(cfa.color_at(x, y)) } else { 0 }).collect();
            for (x, o) in row.iter_mut().enumerate() {
                let (a, b) = (x.saturating_sub(CLIP_REACH), (x + CLIP_REACH).min(w - 1));
                *o = own.get(a..=b).map_or(0, |s| s.iter().fold(0, |m, v| m | v));
            }
        });
        // then within CLIP_REACH rows, for the developed (cropped) pixels only
        let mut mask = vec![0u8; c.width * c.height];
        mask.par_chunks_mut(c.width.max(1)).enumerate().for_each(|(oy, out)| {
            let y = c.y + oy;
            for yy in y.saturating_sub(CLIP_REACH)..=(y + CLIP_REACH).min(h - 1) {
                if let Some(src) = rows.get(yy * w + c.x..yy * w + c.x + c.width) {
                    out.iter_mut().zip(src).for_each(|(o, s)| *o |= s);
                }
            }
            // The pixel's own colour is measured, never interpolated: marked clipped only when its own sample is. A
            // pixel whose own sample clipped keeps its interpolated channels as values (below the clip, they carry
            // the dark structure beside it: a twig a pixel or two wide stays a line instead of breaking into dots).
            for (ox, o) in out.iter_mut().enumerate() {
                let x = c.x + ox;
                let colour = cfa.color_at(x, y);
                if n.data.get(y * w + x).is_some_and(|&v| v >= clip) {
                    *o = own(colour);
                } else if *o & !bit(colour) != 0 {
                    *o = (*o & !bit(colour)) | ((colour.min(2) + 1) << highlight::MEASURED_SHIFT);
                } else {
                    *o = 0;
                }
            }
        });
        Some(mask)
    }

    /// The last steps of [`Self::develop`] for camera RGB made some other way (a denoiser that demosaics too) from
    /// [`Self::normalized`]'s mosaic: apply `OpcodeList3`, then the default crop.
    pub fn finish_demosaiced(&self, mut rgb: Rgb32f) -> Rgb32f {
        opcodes::apply_list3(&self.opcodes.list3, &mut rgb);
        let c = self.develop_crop(rgb.width, rgb.height);
        let rgb =
            if c.x == 0 && c.y == 0 && c.width == rgb.width && c.height == rgb.height { rgb } else { rgb.into_crop(c.x, c.y, c.width, c.height) };
        self.stretched(rgb)
    }

    /// The cropped camera RGB with its rows stretched for non-square pixels.
    fn stretched(&self, rgb: Rgb32f) -> Rgb32f {
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

/// How far (px, either way) a clipped mosaic sample marks its colour as unknown in the pixels around it, for
/// [`RawImage::develop_balanced_masked`]. A heuristic, not the demosaic's full reach: it covers the samples AHD's
/// 5-tap green filter, PPG's and the X-Trans method's 5 × 5 neighbourhoods read directly, but their colour
/// differences use neighbours' estimated greens, which read samples further out still (and AHD's homogeneity choice
/// further again). Wider would mark more measured detail as unknown for little gain.
pub const CLIP_REACH: usize = 2;

/// The largest factor between red's or blue's white-balance multiplier and green's that
/// [`RawImage::develop_balanced`] applies: far beyond any camera's (red and blue relative to green lie between about
/// 0.3 and 6). Outside it the mosaic is demosaiced unbalanced, as [`RawImage::develop`]. On its own it does not make
/// the arithmetic safe: [`MAX_BALANCED`] bounds the samples too.
const MAX_GAIN: f32 = 64.0;

/// The largest sample magnitude (normalised, times the largest gain or divided by the smallest) the balanced demosaic
/// accepts: the demosaics' arithmetic (sums of a few weighted samples, AHD's Lab distances on cube roots, X-Trans
/// weights up to 1000) stays finite far above it. A mosaic with a larger or non-finite sample (a hostile
/// floating-point DNG) is demosaiced unbalanced.
const MAX_BALANCED: f32 = 1e30;

/// [`RawImage::develop_balanced`]'s per-colour gains: `wb` relative to its green multiplier (so green, and the
/// demosaic's luminance-driven choices, stay on the sensor's scale), or `None` when `wb` isn't three positive finite
/// numbers, is neutral already, or asks for a gain outside `1 / MAX_GAIN ..= MAX_GAIN`.
fn balance_gains(wb: [f32; 3]) -> Option<[f32; 3]> {
    if !wb.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let gains = [wb[0] / wb[1], 1.0, wb[2] / wb[1]];
    let safe = |g: f32| g.is_finite() && (1.0 / MAX_GAIN..=MAX_GAIN).contains(&g);
    (gains.iter().all(|&g| safe(g)) && gains != [1.0; 3]).then_some(gains)
}

/// Multiply every sample of a single-plane mosaic `width` samples wide by the gain of its CFA colour. Whether every
/// sample was finite and, times the largest gain or divided by the smallest, within [`MAX_BALANCED`] (checked in the
/// same pass): when not, `data` is partly scaled and only fit to be dropped.
fn scale_mosaic(data: &mut [f32], width: usize, cfa: &Cfa, gains: [f32; 3]) -> bool {
    let (lo, hi) = (gains.iter().copied().fold(f32::MAX, f32::min), gains.iter().copied().fold(0.0f32, f32::max));
    let limit = MAX_BALANCED / hi.max(1.0 / lo.max(f32::MIN_POSITIVE));
    let fits = std::sync::atomic::AtomicBool::new(true);
    lightcraft_raster::par_rows(data, width, |y, row| {
        // one period of the row's colours, repeated along it
        let period: Vec<f32> = (0..cfa.width.max(1)).map(|x| gains.get(cfa.color_at(x, y) as usize).copied().unwrap_or(1.0)).collect();
        let mut ok = true;
        for (v, g) in row.iter_mut().zip(period.iter().cycle()) {
            // (false for a NaN or an infinity)
            ok &= v.abs() <= limit;
            *v *= g;
        }
        if !ok {
            fits.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    });
    fits.into_inner()
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
