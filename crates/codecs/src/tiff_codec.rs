//! TIFF decode via `craft-image-codecs` and header/IFD inspection via `craft-tiff`.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Tiff;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let (tiff, complete) = craft_tiff::Tiff::parse_with_status(bytes, &Default::default()).map_err(err)?;
    if !complete {
        return Err(err("truncated TIFF"));
    }
    let ifd = tiff.ifds.first().ok_or_else(|| err("no IFD"))?;
    let info = ifd.image().map_err(err)?;
    check_size(F, info.width as u64, info.height as u64, opts)?;
    let orientation = ifd.u16(craft_tiff::tags::ORIENTATION);
    let bit_depth = info.bits_per_sample.first().copied().unwrap_or(8) as u8;

    let ic_opts = craft_image_codecs::DecodeOptions {
        limits: craft_image_codecs::Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: opts.max_pixels,
            max_alloc: opts.max_pixels.saturating_mul(32),
        },
        keep_orientation: true,
    };
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::Tiff, bytes, &ic_opts).map_err(|e| match e {
        craft_image_codecs::CodecError::LimitExceeded(_) => Error::TooLarge(info.width as u64, info.height as u64),
        craft_image_codecs::CodecError::Unsupported { .. } => Error::Unsupported(F, "unsupported TIFF"),
        other => err(other),
    })?;
    if img.warnings.iter().any(|w| matches!(w, craft_image_codecs::DecodeWarning::Truncated { .. })) {
        return Err(err("truncated: image data past the end of the file"));
    }

    let (w, h) = (img.width(), img.height());
    check_size(F, w as u64, h as u64, opts)?;
    let model = match img.layout() {
        craft_image_codecs::ChannelLayout::Gray | craft_image_codecs::ChannelLayout::GrayA => Model::Gray,
        craft_image_codecs::ChannelLayout::Rgb | craft_image_codecs::ChannelLayout::Rgba => Model::Rgb,
        craft_image_codecs::ChannelLayout::Cmyk | craft_image_codecs::ChannelLayout::CmykA => Model::Cmyk,
        craft_image_codecs::ChannelLayout::Lab | craft_image_codecs::ChannelLayout::LabA => {
            return Err(Error::Unsupported(F, "CIE L*a*b* TIFF"));
        }
    };
    let alpha = img.layout().has_alpha();
    let buf = match img.sample_type() {
        craft_image_codecs::SampleType::U8 => Buf::U8(img.as_u8().ok_or_else(|| err("expected 8-bit samples"))?.to_vec()),
        craft_image_codecs::SampleType::U16 => Buf::U16(img.to_u16_samples().ok_or_else(|| err("expected 16-bit samples"))?),
        craft_image_codecs::SampleType::F16 => {
            let bits = img.to_f16_samples().ok_or_else(|| err("expected 16-bit float samples"))?;
            Buf::F32(bits.into_iter().map(|h| h.to_f32()).collect())
        }
        craft_image_codecs::SampleType::F32 => Buf::F32(img.to_f32_samples().ok_or_else(|| err("expected 32-bit float samples"))?),
    };

    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf, bit_depth };
    let meta = Meta { icc: img.icc, xmp: img.meta.xmp, orientation, ..Default::default() };
    finish(F, raw, meta, (w, h), opts)
}

/// Stored dimensions and orientation from the first IFD, without reading the image data. Refused,
/// as a decode would refuse it: a layout or compression we can't decode, or strips/tiles that
/// start past the end of the file (truncated). The compressed data itself is not checked.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    craft_image_codecs::tiff_probe::header(bytes, DecodeOptions::default().max_pixels).map_err(|error| match error {
        craft_image_codecs::tiff_probe::Error::TooLarge { width, height } => Error::TooLarge(u64::from(width), u64::from(height)),
        craft_image_codecs::tiff_probe::Error::Unsupported(reason) => Error::Unsupported(F, reason),
        craft_image_codecs::tiff_probe::Error::Malformed(message) => err(message),
    })
}
