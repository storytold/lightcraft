//! JPEG XL adapter to the shared `craft-jxl` codec.

use crate::convert::{Buf, Meta, Model, Raw, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Jxl;

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let limits = craft_jxl::DecodeLimits { max_pixels: opts.max_pixels, ..Default::default() };
    let decoded = craft_jxl::decode_with_color(bytes, limits, craft_jxl::OutputColor::LinearRec2020).map_err(|e| match e {
        craft_jxl::Error::PixelLimitExceeded { width, height, .. } => Error::TooLarge(u64::from(width), u64::from(height)),
        craft_jxl::Error::Malformed(message) | craft_jxl::Error::LimitExceeded(message) | craft_jxl::Error::InvalidImage(message) => {
            Error::Malformed(F, message)
        }
        craft_jxl::Error::Unsupported(message) | craft_jxl::Error::Encode(message) => Error::Malformed(F, message),
    })?;

    let gray = matches!(decoded.layout, craft_jxl::Layout::Gray | craft_jxl::Layout::GrayAlpha);
    let alpha = decoded.layout.has_alpha();
    let pixels_linear_rec2020 = decoded.pixels_linear_rec2020;
    let rendered_icc = if !pixels_linear_rec2020 && decoded.metadata.icc.is_none() && !decoded.rendered_icc.is_empty() {
        Some(decoded.rendered_icc.clone())
    } else {
        None
    };
    let mut meta = Meta {
        orientation: Some(1),
        icc: decoded.metadata.icc.or(rendered_icc),
        exif: decoded.metadata.exif,
        xmp: decoded.metadata.xmp.map(|packet| String::from_utf8_lossy(&packet).trim_end_matches('\0').to_owned()),
        ..Default::default()
    };
    if pixels_linear_rec2020 {
        let mut space = SourceSpace::named(NamedSpace::Rec2020, SpaceOrigin::Container);
        space.trc = Some([Trc::Linear, Trc::Linear, Trc::Linear]);
        meta.already_linear = Some(space);
    } else if meta.icc.is_none() {
        return Err(Error::Malformed(F, "shared decoder did not report the rendered JPEG XL color profile".into()));
    }
    let raw = Raw {
        width: decoded.width as usize,
        height: decoded.height as usize,
        model: if gray { Model::Gray } else { Model::Rgb },
        alpha,
        premultiplied: decoded.pixels_premultiplied,
        buf: Buf::F32(decoded.pixels),
        bit_depth: decoded.bit_depth,
    };
    let (source_width, source_height) =
        if (5..=8).contains(&decoded.source_orientation) { (decoded.height, decoded.width) } else { (decoded.width, decoded.height) };
    let mut result = finish(F, raw, meta, (source_width, source_height), opts)?;
    result.float = decoded.float_samples;
    Ok(result)
}
