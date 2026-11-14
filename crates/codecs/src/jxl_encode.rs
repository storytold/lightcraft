//! JPEG XL encoder adapter to the shared `craft-jxl` codec.

use crate::encode::{EncodeImage, EncodeMeta, Samples};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Encoder effort exposed by LightCraft's export settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JxlEffort {
    /// Quick search, producing larger files.
    Fast,
    /// Balanced search.
    #[default]
    Normal,
}

impl JxlEffort {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "fast" => Self::Fast,
            "normal" => Self::Normal,
            _ => return None,
        })
    }
}

/// The color encoding declared by the JPEG XL file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JxlColour {
    #[default]
    Srgb,
    /// P3 primaries, D65, and the sRGB curve.
    DisplayP3,
    /// Rec. 2020 primaries and the BT.709 curve.
    Rec2020,
    /// HDR Rec. 2020 using the PQ curve.
    Rec2020Pq,
}

/// JPEG XL export settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JxlOptions {
    /// Lossy quality on a 1..=100 scale; ignored when lossless.
    pub quality: u8,
    /// Use mathematically lossless coding.
    pub lossless: bool,
    /// Encoder search effort.
    pub effort: JxlEffort,
    /// Write a progressive image.
    pub progressive: bool,
    /// Color encoding to declare.
    pub colour: JxlColour,
}

impl Default for JxlOptions {
    fn default() -> Self {
        Self { quality: 90, lossless: false, effort: JxlEffort::Normal, progressive: false, colour: JxlColour::Srgb }
    }
}

/// Encode JPEG XL while preserving LightCraft's public option and metadata types.
pub fn encode_jxl(image: &EncodeImage, options: &JxlOptions, metadata: &EncodeMeta) -> Result<Vec<u8>> {
    image.validate()?;
    let layout = match image.channels {
        1 => craft_jxl::Layout::Gray,
        2 => craft_jxl::Layout::GrayAlpha,
        3 => craft_jxl::Layout::Rgb,
        4 => craft_jxl::Layout::Rgba,
        _ => return Err(Error::Encode(format!("unsupported channel count {}", image.channels))),
    };
    let samples = match image.samples {
        Samples::U8(samples) => craft_jxl::Samples::U8(samples),
        Samples::U16(samples) => craft_jxl::Samples::U16(samples),
        Samples::F32(samples) => craft_jxl::Samples::F32(samples),
    };
    let image = craft_jxl::ImageRef { width: image.width, height: image.height, layout, samples };
    let options = craft_jxl::EncodeOptions {
        quality: options.quality,
        lossless: options.lossless,
        effort: match options.effort {
            JxlEffort::Fast => craft_jxl::Effort::Fast,
            JxlEffort::Normal => craft_jxl::Effort::Normal,
        },
        progressive: options.progressive,
        colour: match options.colour {
            JxlColour::Srgb => craft_jxl::JxlColour::Srgb,
            JxlColour::DisplayP3 => craft_jxl::JxlColour::DisplayP3,
            JxlColour::Rec2020 => craft_jxl::JxlColour::Rec2020,
            JxlColour::Rec2020Pq => craft_jxl::JxlColour::Rec2020Pq,
        },
    };
    let metadata = craft_jxl::Metadata {
        icc: metadata.icc.map(<[u8]>::to_vec),
        exif: metadata.exif.map(<[u8]>::to_vec),
        xmp: metadata.xmp.map(|packet| packet.as_bytes().to_vec()),
    };
    craft_jxl::encode(&image, options, &metadata).map_err(|error| Error::Encode(error.to_string()))
}
