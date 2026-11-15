//! WebP decode via `craft-image-codecs` (lossy + lossless, alpha, ICCP/EXIF/XMP chunks; first frame of
//! animations).

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::WebP;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let ic_opts = craft_image_codecs::DecodeOptions {
        limits: craft_image_codecs::Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: opts.max_pixels,
            max_alloc: opts.max_pixels.saturating_mul(8),
        },
        keep_orientation: true,
    };
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::WebP, bytes, &ic_opts).map_err(|e| match e {
        craft_image_codecs::CodecError::LimitExceeded(_) => Error::TooLarge(0, 0),
        other => err(other),
    })?;
    let (w, h) = (img.width(), img.height());
    check_size(F, w as u64, h as u64, opts)?;
    let alpha = img.layout().has_alpha();
    let model = if img.layout().is_gray() { Model::Gray } else { Model::Rgb };
    let buf = Buf::U8(img.as_u8().ok_or_else(|| err("expected 8-bit WebP samples"))?.to_vec());
    let icc = img.icc;
    let exif = img.meta.exif.map(|e| if e.starts_with(b"Exif\0\0") { e[6..].to_vec() } else { e });
    let xmp = img.meta.xmp.map(|x| x.trim_end_matches('\0').to_string());
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf, bit_depth: 8 };
    finish(F, raw, Meta { icc, exif, xmp, ..Default::default() }, (w, h), opts)
}

/// Stored dimensions and EXIF orientation from the RIFF chunks and the bitstream header, without
/// decoding the image. Refused, as a decode would refuse it: no image chunk (`VP8 `, `VP8L` or an
/// animation frame), or one that runs past the end of the file (truncated). The compressed data
/// itself is not checked.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let (w, h, orientation) = craft_image_codecs::webp_probe::header(bytes).map_err(err)?;
    check_size(F, w as u64, h as u64, &DecodeOptions::default())?;
    Ok((w, h, orientation))
}
