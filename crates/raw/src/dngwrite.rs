//! Writing DNG files from a [`RawImage`] (uncompressed or lossless-JPEG tiles), used for DNG export and for
//! bit-exact decoder round-trip tests. The raw image is stored in IFD0 (allowed by DNG 1.7 §"File Structure").

use crate::{RawData, RawError, RawImage, Result};
pub use craft_raw::dngwrite::f32_to_f16;
use craft_tiff::ByteOrder;
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DngCompression {
    Uncompressed,
    /// Lossless JPEG tiles of `tile × tile` pixels (`tile` even). CFA tiles are encoded as two interleaved
    /// components of half the tile width (the common DNG layout), linear data with one component per sample.
    Lj92 {
        tile: u32,
    },
    /// Deflate (zlib) tiles of `tile × tile` pixels. Floating-point data is stored as 16-bit (`half`) or
    /// 32-bit IEEE floats with the floating-point predictor (TIFF predictor 3, Adobe TIFF Technical Note 3);
    /// integer data with horizontal differencing (predictor 2). 16-bit float is what HDR merges usually use.
    Deflate {
        tile: u32,
        half: bool,
    },
    /// Lossy DNG (DNG 1.4 compression 34892): the raw data demosaiced to linear camera RGB (with `OpcodeList3`
    /// and the default crop applied), scaled down to at most `long_edge` pixels on the long side when given,
    /// then stored as 8-bit baseline JPEG tiles of `tile × tile` pixels at JPEG `quality` (1–100). The 8-bit
    /// codes follow a 1/2.2 power curve that a `LinearizationTable` maps back to linear 16-bit values, so
    /// shadows keep their precision. The colour tags (matrices, white balance, profile look) are kept.
    LossyJpeg {
        tile: u32,
        quality: u8,
        long_edge: Option<u32>,
    },
}

/// Gamma of the 8-bit codes in a lossy DNG (own choice: perceptually close to even steps, like sRGB).
const LOSSY_GAMMA: f64 = 2.2;

/// The 256-entry `LinearizationTable` of a lossy DNG: code → linear 16-bit value.
fn lossy_linearization() -> Vec<u16> {
    (0..256u32).map(|i| ((i as f64 / 255.0).powf(LOSSY_GAMMA) * 65535.0).round() as u16).collect()
}

/// The image a lossy DNG stores: `raw` developed to linear camera RGB (white = 1), scaled to fit
/// `long_edge`, as 8-bit codes (in `u16` samples) on the [`LOSSY_GAMMA`] curve, with the levels,
/// geometry and opcode lists that this data needs (black 0, white 65535 after linearization).
fn lossy_source(raw: &RawImage, long_edge: Option<u32>) -> Result<RawImage> {
    let mut rgb = raw.develop(crate::Method::Ahd)?;
    if let Some(edge) = long_edge.map(|e| e.max(1) as usize) {
        let long = rgb.width.max(rgb.height);
        if long > edge {
            let s = edge as f64 / long as f64;
            let w = ((rgb.width as f64 * s).round() as usize).max(1);
            let h = ((rgb.height as f64 * s).round() as usize).max(1);
            rgb = lightcraft_raster::resample::resize(&rgb, w, h, lightcraft_raster::resample::Filter::Lanczos3);
        }
    }
    let (w, h) = (rgb.width, rgb.height);
    if w == 0 || h == 0 {
        return Err(RawError::Corrupt("empty image".into()));
    }
    let inv = 1.0 / LOSSY_GAMMA as f32;
    let data: Vec<u16> = rgb
        .data
        .par_iter()
        .flat_map_iter(|px| px.map(|v| if v.is_finite() { (v.clamp(0.0, 1.0).powf(inv) * 255.0).round() as u16 } else { 0 }))
        .collect();
    let full = crate::Rect::new(0, 0, w, h);
    Ok(RawImage {
        width: w,
        height: h,
        cpp: 3,
        data: RawData::U16(data),
        cfa: None,
        bits: 8,
        black: crate::BlackLevel::default(),
        white: vec![65535.0],
        active_area: full,
        crop: full,
        // list 1 and 2 work on the undemosaiced data and list 3 is applied by `develop`
        opcodes: crate::OpcodeLists::default(),
        linearized: true,
        ..raw.clone()
    })
}

/// Software tag of the DNGs this writer makes. A bare "LightCraft" marks older files, whose CFAPattern is anchored at
/// the sensor origin instead of the ActiveArea (the reader keeps their original reading).
pub const SOFTWARE: &str = "LightCraft DNG 2";

#[derive(Clone, Debug, PartialEq)]
pub struct DngWriteOptions {
    pub compression: DngCompression,
    pub order: ByteOrder,
    /// XMP packet to embed (tag 700); defaults to one generated from the image metadata.
    pub xmp: Option<String>,
}

impl Default for DngWriteOptions {
    fn default() -> Self {
        Self { compression: DngCompression::Lj92 { tile: 256 }, order: ByteOrder::Little, xmp: None }
    }
}

/// Serialise `raw` as a DNG. Development, resize and the lossy transfer curve remain application policy.
pub fn write_dng(raw: &RawImage, opts: &DngWriteOptions) -> Result<Vec<u8>> {
    raw.validate()?;
    let lossy;
    let raw = match opts.compression {
        DngCompression::LossyJpeg { long_edge, .. } => {
            lossy = lossy_source(raw, long_edge)?;
            &lossy
        }
        _ => raw,
    };
    let color = &raw.color;
    let matrices = |pair: &[Option<lightcraft_color::Mat3>; 2]| pair.map(|matrix| matrix.map(|m| m.0));
    let cinema = craft_raw::CinemaTags::default();
    let view = craft_raw::dngwrite::DngImage {
        width: raw.width,
        height: raw.height,
        cpp: raw.cpp,
        data: &raw.data,
        cfa: raw.cfa.as_ref(),
        bits: raw.bits,
        black: &raw.black,
        white: &raw.white,
        active_area: raw.active_area,
        crop: raw.crop,
        orientation: craft_raw::Orientation::from_exif(raw.orientation.to_exif()),
        color: craft_raw::dngwrite::DngColor {
            illuminant: color.illuminant,
            color_matrix: matrices(&color.color_matrix),
            forward_matrix: matrices(&color.forward_matrix),
            camera_calibration: matrices(&color.camera_calibration),
            analog_balance: color.analog_balance,
            as_shot_neutral: color.as_shot_neutral,
            as_shot_white_xy: color.as_shot_white_xy.map(|xy| [xy.x, xy.y]),
            baseline_exposure: color.baseline_exposure,
            baseline_sharpness: color.baseline_sharpness,
            profile: &color.profile,
        },
        wb_multipliers: raw.wb_multipliers,
        opcodes: &raw.opcodes,
        make: raw.metadata.make.as_deref(),
        model: raw.metadata.model.as_deref(),
        unique_model: None,
        cinema: &cinema,
        unknown_camera_model: Some("LightCraft"),
    };
    let compression = match opts.compression {
        DngCompression::Uncompressed | DngCompression::LossyJpeg { .. } => craft_raw::DngCompression::Uncompressed,
        DngCompression::Lj92 { tile } => craft_raw::DngCompression::Lj92 { tile },
        DngCompression::Deflate { tile, half } => craft_raw::DngCompression::Deflate { tile, half },
    };
    let options = craft_raw::DngWriteOptions {
        compression,
        order: opts.order,
        software: SOFTWARE.into(),
        xmp: Some(opts.xmp.clone().unwrap_or_else(|| lightcraft_meta::write_xmp(&raw.metadata, None))),
    };
    if let DngCompression::LossyJpeg { tile, quality, .. } = opts.compression {
        let encode = |pixels: Vec<u8>, tile: u32, quality: u8| {
            let image = craft_image_codecs::Image::from_u8(tile, tile, craft_image_codecs::ChannelLayout::Rgb, pixels)
                .map_err(|e| RawError::Corrupt(format!("lossy JPEG tile: {e}")))?;
            let options = craft_image_codecs::EncodeOptions { jpeg_quality: quality, ..Default::default() };
            craft_image_codecs::encode(&image, craft_image_codecs::Format::Jpeg, &options)
                .map_err(|e| RawError::Corrupt(format!("lossy JPEG tile: {e}")))
        };
        craft_raw::dngwrite::write_lossy_dng(&view, &options, tile, quality, &lossy_linearization(), &encode)
    } else {
        craft_raw::dngwrite::write_dng_view(&view, &options)
    }
}

/// Exif `LightSource` code of a white point the rendered DNG writer knows (D65 = 21, D50 = 23).
fn illuminant_of(white: lightcraft_color::Xy) -> u16 {
    if (white.x - lightcraft_color::D50.x).abs() < 1e-3 && (white.y - lightcraft_color::D50.y).abs() < 1e-3 { 23 } else { 21 }
}

/// Render to DNG: a linear DNG (`LinearRaw`, 16-bit float Deflate tiles) of an already rendered,
/// oriented image. `rgb` is `width × height` interleaved linear RGB in `space` (white = 1; values
/// above 1 are kept up to the half-float maximum, negative and non-finite ones become 0). The colour tags describe `space`
/// exactly — `ColorMatrix1` = XYZ → space, `ForwardMatrix1` = space → XYZ D50, a neutral
/// `AsShotNeutral` — and an identity `ProfileToneCurve` keeps raw converters from adding their
/// default tone curve on top of the baked-in edit.
pub fn write_rendered_dng(
    width: usize,
    height: usize,
    rgb: &[f32],
    space: &lightcraft_color::RgbSpace,
    metadata: &lightcraft_meta::Metadata,
    xmp: Option<String>,
) -> Result<Vec<u8>> {
    let n = width.checked_mul(height).and_then(|p| p.checked_mul(3)).ok_or(RawError::Limit("image too large"))?;
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
        return Err(RawError::Corrupt(format!("cannot write a {width}×{height} DNG")));
    }
    let src = rgb.get(..n).ok_or_else(|| RawError::Corrupt(format!("{} samples for a {width}×{height} RGB image", rgb.len())))?;
    let data: Vec<f32> = src.iter().map(|&v| if v.is_finite() { v.clamp(0.0, 65504.0) } else { 0.0 }).collect();
    let full = crate::Rect::new(0, 0, width, height);
    let forward = lightcraft_color::bradford(space.white, lightcraft_color::D50).mul(&space.to_xyz());
    let color = crate::ColorData {
        illuminant: [illuminant_of(space.white), 0],
        color_matrix: [Some(space.from_xyz()), None],
        forward_matrix: [Some(forward), None],
        as_shot_neutral: Some([1.0; 3]),
        profile: crate::profile::ProfileLook {
            tone_curve: Some(crate::profile::ToneCurve { points: vec![[0.0, 0.0], [1.0, 1.0]] }),
            ..Default::default()
        },
        ..Default::default()
    };
    let raw = RawImage {
        format: crate::RawFormat::Dng,
        width,
        height,
        cpp: 3,
        data: RawData::F32(data),
        cfa: None,
        bits: 16,
        black: crate::BlackLevel::uniform(0.0),
        white: vec![1.0],
        active_area: full,
        crop: full,
        orientation: lightcraft_geom::Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: crate::OpcodeLists::default(),
        metadata: metadata.clone(),
        lens_metadata: Default::default(),
    };
    let order = ByteOrder::Little;
    write_dng(&raw, &DngWriteOptions { compression: DngCompression::Deflate { tile: 256, half: true }, order, xmp })
}

#[cfg(test)]
#[path = "dngwrite_legacy_tests.rs"]
mod legacy;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlackLevel, OpcodeLists, RawData, RawFormat, RawImage, Rect};

    use crate::{Cfa, ColorData, Mat3, Orientation};

    fn mosaic(w: usize, h: usize) -> RawImage {
        let data: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let c = (x % 2) + (y % 2);
                (64.0 + 3000.0 * (x as f32 / w as f32) * (0.5 + c as f32 * 0.25) + 500.0 * (y as f32 / h as f32)) as u16
            })
            .collect();
        let color = ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(Mat3([[0.7, -0.1, -0.05], [-0.4, 1.2, 0.2], [-0.1, 0.2, 0.6]])), None],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        };
        RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16(data),
            cfa: Some(Cfa::bayer("RGGB").unwrap()),
            bits: 12,
            black: BlackLevel::uniform(64.0),
            white: vec![4095.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(2, 2, w - 4, h - 4),
            orientation: Orientation::Rotate90,
            color,
            wb_multipliers: None,
            linearized: false,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
            pixel_aspect: 1.0,
        }
    }

    #[test]
    fn shared_writer_matches_frozen_application_bytes() {
        let raw = mosaic(20, 16);
        for order in [ByteOrder::Little, ByteOrder::Big] {
            for (compression, old_compression) in [
                (DngCompression::Uncompressed, legacy::DngCompression::Uncompressed),
                (DngCompression::Lj92 { tile: 16 }, legacy::DngCompression::Lj92 { tile: 16 }),
                (DngCompression::Deflate { tile: 16, half: false }, legacy::DngCompression::Deflate { tile: 16, half: false }),
                (
                    DngCompression::LossyJpeg { tile: 32, quality: 95, long_edge: None },
                    legacy::DngCompression::LossyJpeg { tile: 32, quality: 95, long_edge: None },
                ),
                (
                    DngCompression::LossyJpeg { tile: 32, quality: 95, long_edge: Some(8) },
                    legacy::DngCompression::LossyJpeg { tile: 32, quality: 95, long_edge: Some(8) },
                ),
            ] {
                let options = DngWriteOptions { compression, order, xmp: None };
                let old_options = legacy::DngWriteOptions { compression: old_compression, order, xmp: None };
                assert_eq!(write_dng(&raw, &options).unwrap(), legacy::write_dng(&raw, &old_options).unwrap());
            }
        }
        let rgb = (0..4 * 4 * 3).map(|i| i as f32 / 48.0).collect::<Vec<_>>();
        let metadata = lightcraft_meta::Metadata::default();
        for space in [lightcraft_color::SRGB, lightcraft_color::PROPHOTO] {
            assert_eq!(
                write_rendered_dng(4, 4, &rgb, &space, &metadata, None).unwrap(),
                legacy::write_rendered_dng(4, 4, &rgb, &space, &metadata, None).unwrap()
            );
        }
    }

    #[test]
    fn shared_writer_keeps_padding_and_declared_white_guards() {
        let mut raw = mosaic(20, 16);
        raw.white = vec![65535.0];
        let options = DngWriteOptions { compression: DngCompression::Lj92 { tile: 16 }, ..Default::default() };
        let bytes = write_dng(&raw, &options).unwrap();
        let tiff = craft_tiff::Tiff::parse(&bytes).unwrap();
        assert_eq!(tiff.all_ifds()[0].u16(craft_tiff::tags::BITS_PER_SAMPLE), Some(16));
        for tile in [2050, u32::MAX] {
            let options = DngWriteOptions { compression: DngCompression::Lj92 { tile }, ..Default::default() };
            assert!(matches!(write_dng(&raw, &options), Err(RawError::Limit("tile too large"))));
        }
    }

    /// Deflate tiles of `tile × tile` pad into a square buffer that grows with the square: a
    /// caller-supplied edge of `u32::MAX` would make the writer pad an 8 × 8 image into billions of
    /// edge-replicate iterations and a multi-gigabyte buffer. The clamp keeps the bounded shape — same
    /// file as requesting the cap exactly, and a DNG either way.
    #[test]
    fn a_hostile_deflate_tile_edge_is_clamped() {
        let (w, h) = (8usize, 8usize);
        let full = Rect::new(0, 0, w, h);
        let raw = RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16((0..64).map(|i| i as u16 * 129).collect()),
            cfa: None,
            bits: 12,
            black: BlackLevel::uniform(0.0),
            white: vec![4095.0],
            active_area: full,
            crop: full,
            orientation: lightcraft_geom::Orientation::Normal,
            color: crate::ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
        };
        let opts = |tile: u32| DngWriteOptions { compression: DngCompression::Deflate { tile, half: false }, ..DngWriteOptions::default() };
        let hostile = write_dng(&raw, &opts(u32::MAX)).expect("the clamped tile writes");
        assert_eq!(hostile, write_dng(&raw, &opts(2048)).expect("the cap itself writes"), "u32::MAX is clamped to the cap");
        assert_ne!(hostile, write_dng(&raw, &opts(256)).expect("a normal tile writes"), "the clamp is not a no-op");
        assert_eq!(crate::probe(&hostile), Some(RawFormat::Dng), "the clamped output is still a DNG");
        // the writer's own padded tile (2048 × 2048 around an 8 × 8 image) reads back
        let back = crate::decode(&hostile).expect("a tile this writer pads to decodes");
        assert_eq!((back.width, back.height), (w, h));
    }
}
