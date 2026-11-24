//! GIF through `craft-gif` and BMP through `craft-image-codecs`.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

/// Canvases up to this many pixels are still decoded by [`header`] (at most 16 MB), so a truncated
/// small file is refused at import as before.
const HEADER_DECODE_MAX_PIXELS: u64 = 4_000_000;

/// Stored size for the import probe (issue #2553). The probe used to decode these in full, and a
/// few-dozen-byte file declaring a gigapixel canvas allocated gigabytes. The size now comes from the
/// header and must be one the file's bytes can fill; only a canvas small enough to be cheap is
/// decoded (to catch truncation). Orientation is always 1: neither format carries EXIF here.
pub(crate) fn header(bytes: &[u8], format: Format) -> Result<(u32, u32, u16)> {
    if format == Format::Gif {
        let info = craft_gif::scan(bytes, 1).ok_or_else(|| Error::Malformed(format, "invalid GIF header".into()))?;
        let (w, h) = (u32::from(info.width), u32::from(info.height));
        check_size(format, u64::from(w), u64::from(h), &DecodeOptions::default())?;
        check_fillable(format, bytes, w, h)?;
        if craft_gif::palette(bytes).is_none() {
            return Err(Error::Malformed(format, "GIF has no readable colour table".into()));
        }
        if u64::from(w).saturating_mul(u64::from(h)) <= HEADER_DECODE_MAX_PIXELS {
            let decoded = decode(bytes, format, &DecodeOptions::fit(64, 64))?;
            return Ok((decoded.source_width, decoded.source_height, decoded.orientation));
        }
        return Ok((w, h, 1));
    }
    if format == Format::Bmp {
        let (w, h) = craft_image_codecs::bmp_probe::dimensions(bytes).ok_or_else(|| Error::Malformed(format, "invalid BMP header".into()))?;
        if w == 0 || h == 0 {
            return Err(Error::Malformed(format, "zero dimension".into()));
        }
        check_fillable(format, bytes, w, h)?;
    }
    let (w, h) = match format {
        Format::Bmp => craft_image_codecs::probe_size(bytes).map_err(|e| Error::Malformed(format, e.to_string()))?,
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    if w == 0 || h == 0 {
        return Err(Error::Malformed(format, "zero dimension".into()));
    }
    check_fillable(format, bytes, w, h)?;
    if u64::from(w).saturating_mul(u64::from(h)) <= HEADER_DECODE_MAX_PIXELS {
        let d = decode(bytes, format, &DecodeOptions::fit(64, 64))?;
        return Ok((d.source_width, d.source_height, d.orientation));
    }
    Ok((w, h, 1))
}

/// The largest canvas allowed that the file's pixel data leaves mostly blank: a GIF screen its
/// first frame's codes don't cover, or an RLE BMP whose runs don't. Its decode still allocates the
/// whole canvas (16 MB at this size), so it is held well below `max_pixels`.
const SPARSE_MAX_PIXELS: u64 = 4_000_000;

/// Refuses a canvas larger than the file's pixel data can describe, before the decoder allocates it
/// (issue #2553). Coverage is measured from the pixel data itself, never from the file's length, so
/// padding (comments, bytes after an end code, trailing bytes) covers nothing:
///
/// - Uncompressed BMP rows are read in full, padding included, from the offset the file header
///   gives, so the data must hold every row: anything shorter fails to decode once the canvas is
///   allocated, and is refused here instead.
/// - An RLE BMP covers the pixels its runs and absolute blocks draw, as the decoder paints them; an
///   end-of-row, end-of-bitmap or delta code leaves the rest blank.
/// - A GIF covers the pixels its first frame's LZW codes draw up to the end code. The shared GIF
///   reader counts code-string lengths without allocating decoded pixels; the screen around the
///   frame may be larger, and is blank there.
///
/// Canvases the data leaves mostly blank are limited to [`SPARSE_MAX_PIXELS`].
fn check_fillable(format: Format, bytes: &[u8], w: u32, h: u32) -> Result<()> {
    let area = |w: u32, h: u32| u64::from(w).saturating_mul(u64::from(h));
    let canvas = area(w, h);
    // the pixels the file's data can cover
    let covered = match format {
        Format::Gif => {
            // A frame's compressed data must cover its declared raster before it can justify the
            // logical screen. This retains the old "can hold" refusal for truncated/empty frames.
            let info = craft_gif::scan(bytes, 1).ok_or_else(|| Error::Malformed(format, "no readable first frame".into()))?;
            let Some(frame) = info.frames.first() else {
                return Err(Error::Malformed(format, "no readable first frame".into()));
            };
            let frame_width = u32::from(frame.width);
            let frame_height = u32::from(frame.height);
            let compressed_bytes =
                craft_gif::first_frame_compressed_bytes(bytes).ok_or_else(|| Error::Malformed(format, "no readable first frame".into()))?;
            if area(frame_width, frame_height) > (compressed_bytes as u64).saturating_mul(2_731) {
                return Err(Error::Malformed(
                    format,
                    format!("a {frame_width} × {frame_height} frame is far larger than its {compressed_bytes} bytes of pixel data can hold"),
                ));
            }
            craft_gif::first_frame_decoded_pixels(bytes).unwrap_or(0)
        }
        _ => {
            let coverage = craft_image_codecs::bmp_probe::coverage(bytes, w, h);
            if let Some((held, needed)) = coverage.row_bytes
                && held < needed
            {
                return Err(Error::Malformed(
                    format,
                    format!("a {w} × {h} canvas is larger than its {held} bytes of pixel data can hold ({needed} needed)"),
                ));
            }
            coverage.pixels
        }
    };
    if canvas > covered && canvas > SPARSE_MAX_PIXELS {
        return Err(Error::Malformed(
            format,
            format!(
                "a {w} × {h} canvas is mostly blank (its pixel data covers at most {covered} pixels), and such pictures are limited to {SPARSE_MAX_PIXELS} pixels"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    if format == Format::Gif {
        return decode_gif(bytes, opts);
    }
    if format != Format::Bmp {
        return Err(Error::Unsupported(format, "not handled by this decoder"));
    }
    let (w, h) = craft_image_codecs::bmp_probe::dimensions(bytes).ok_or_else(|| Error::Malformed(format, "invalid BMP header".into()))?;
    check_size(format, w as u64, h as u64, opts)?;
    check_fillable(format, bytes, w, h)?;
    let (w, h) = craft_image_codecs::probe_size(bytes).map_err(|e| Error::Malformed(format, e.to_string()))?;
    let ic_opts = craft_image_codecs::DecodeOptions {
        limits: craft_image_codecs::Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: opts.max_pixels,
            max_alloc: opts.max_pixels.saturating_mul(4),
        },
        keep_orientation: true,
    };
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::Bmp, bytes, &ic_opts).map_err(|e| match e {
        craft_image_codecs::CodecError::LimitExceeded(_) => Error::TooLarge(w as u64, h as u64),
        other => Error::Malformed(format, other.to_string()),
    })?;
    let (w, h) = (img.width(), img.height());
    let alpha = img.layout().has_alpha();
    let model = if img.layout().is_gray() { Model::Gray } else { Model::Rgb };
    let buf = img.as_u8().ok_or_else(|| Error::Malformed(format, "expected 8-bit BMP samples".into()))?.to_vec();
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf: Buf::U8(buf), bit_depth: 8 };
    finish(format, raw, Meta { icc: img.icc, ..Default::default() }, (w, h), opts)
}

fn decode_gif(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let format = Format::Gif;
    let Some(info) = craft_gif::scan(bytes, 1) else {
        return Err(Error::Malformed(format, "invalid GIF header".into()));
    };
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_size(format, u64::from(width), u64::from(height), opts)?;
    let Some(frame) = info.frames.first() else {
        return Err(Error::Malformed(format, "no readable first frame".into()));
    };
    check_size(format, u64::from(frame.width), u64::from(frame.height), opts)?;
    check_fillable(format, bytes, width, height)?;
    let max_bytes = opts.max_pixels.saturating_mul(4);
    let mut limits = craft_gif::frames::FrameLimits::default();
    limits.max_frames = 1;
    limits.max_bytes = max_bytes;
    limits.max_canvas_bytes = max_bytes;
    limits.max_decode_bytes = Some(max_bytes);
    let animation = craft_gif::frames::read_frames(bytes, &limits).map_err(|e| Error::Malformed(format, e.to_string()))?;
    let Some(decoded) = animation.frames.into_iter().next() else {
        return Err(Error::Malformed(format, "no decoded frame".into()));
    };
    let raw = Raw {
        width: animation.width as usize,
        height: animation.height as usize,
        model: Model::Rgb,
        alpha: true,
        premultiplied: false,
        buf: Buf::U8(decoded.rgba),
        bit_depth: 8,
    };
    let icc = craft_gif::frames::icc_profile(bytes);
    finish(format, raw, Meta { icc, ..Default::default() }, (animation.width, animation.height), opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(blocks: &[u8]) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        // logical screen: 2×2, no global colour table, background 0, aspect 0
        b.extend_from_slice(&[2, 0, 2, 0, 0x00, 0, 0]);
        b.extend_from_slice(blocks);
        b
    }

    /// A GIF decoder allocates its frame buffer at the image descriptor's declared
    /// size, which the canvas bound ([`crate::decode`]'s `max_pixels` on the logical screen)
    /// does not cover. A 20-byte file used to buy a 4 GB allocation before the gate.
    #[test]
    fn a_first_frame_over_the_bound_is_refused_without_allocating_it() {
        let bytes = gif(&[0x2C, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0x00]);
        match crate::decode(&bytes, DecodeOptions::default()) {
            Err(Error::TooLarge(w, h)) => assert_eq!((w, h), (65535, 65535)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A frame within the bound still reaches the decoder (which reports the truncated data);
    /// the gate may only refuse on size.
    #[test]
    fn a_first_frame_within_the_bound_is_not_refused() {
        let bytes = gif(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x00]);
        let r = crate::decode(&bytes, DecodeOptions::default());
        assert!(matches!(r, Err(Error::Malformed(..)) | Err(Error::TooLarge(..))), "{r:?}");
    }

    /// A GIF whose blocks can't be walked to a first frame (here a comment that runs to the end of the
    /// file) is refused, instead of the whole file being counted as pixel data for its 400 MP screen.
    /// The `image` decoder also stops at such a file when it opens it, but the canvas bound doesn't
    /// rely on the two walks agreeing.
    #[test]
    fn a_gif_with_no_readable_first_frame_is_refused() {
        let mut b = b"GIF89a".to_vec();
        b.extend(20_000u16.to_le_bytes());
        b.extend(20_000u16.to_le_bytes());
        b.extend([0, 0, 0, 0x21, 0xFE]);
        for _ in 0..600 {
            b.push(255);
            b.extend([b'x'; 255]);
        }
        let r = check_fillable(Format::Gif, &b, 20_000, 20_000);
        assert!(matches!(&r, Err(Error::Malformed(_, m)) if m.contains("no readable first frame")), "{r:?}");
    }

    /// The LZW walk counts exactly the pixels an encoder wrote up to its end code: a frame of one
    /// colour (long strings, the smallest codes), a 256-colour gradient and noise (a table that fills
    /// up and is cleared again).
    #[test]
    fn the_lzw_walk_counts_every_pixel_an_encoder_wrote() {
        let (w, h) = (640u32, 480u32);
        let mut seed = 2_553u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        let encode_gif = |rgba: Vec<u8>| -> Vec<u8> {
            let img = craft_image_codecs::Image::from_u8(w, h, craft_image_codecs::ChannelLayout::Rgba, rgba).unwrap();
            craft_image_codecs::encode(&img, craft_image_codecs::Format::Gif, &craft_image_codecs::EncodeOptions::default()).unwrap()
        };
        let solid = [90, 90, 90, 255].repeat((w * h) as usize);
        let mut gradient = Vec::with_capacity((w * h * 4) as usize);
        let mut noise_buf = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                gradient.extend_from_slice(&[((x / 40 * 16) as u8), ((y / 30 * 16) as u8), 0, 255]);
                noise_buf.extend_from_slice(&[noise(), noise(), noise(), 255]);
            }
        }
        for (name, rgba) in [("solid", solid), ("gradient", gradient), ("noise", noise_buf)] {
            let gif = encode_gif(rgba);
            let first = craft_gif::scan(&gif, 1).unwrap().frames.remove(0);
            assert_eq!((u32::from(first.width), u32::from(first.height)), (w, h), "{name}");
            assert_eq!(craft_gif::first_frame_decoded_pixels(&gif), Some(u64::from(w * h)), "{name}");
        }
    }

    #[test]
    fn gif_icc_profile_is_preserved_by_the_codec_adapter() {
        let img = craft_image_codecs::Image::from_u8(2, 2, craft_image_codecs::ChannelLayout::Rgba, [80, 120, 160, 255].repeat(4)).unwrap();
        let mut bytes = craft_image_codecs::encode(&img, craft_image_codecs::Format::Gif, &craft_image_codecs::EncodeOptions::default()).unwrap();
        let frame = craft_gif::scan(&bytes, 1).unwrap().frames[0];
        let profile = [1, 2, 3, 4, 5];
        let mut extension = vec![0x21, 0xff, 0x0b];
        extension.extend_from_slice(b"ICCRGBG1012");
        extension.push(profile.len() as u8);
        extension.extend_from_slice(&profile);
        extension.push(0);
        bytes.splice(frame.offset..frame.offset, extension);

        let decoded = crate::decode(&bytes, DecodeOptions::default()).unwrap();
        assert_eq!(decoded.icc.as_deref(), Some(profile.as_slice()));
    }

    #[test]
    fn gif_first_frame_pixels_match_the_previous_image_decoder() {
        let mut pixels = Vec::with_capacity(16 * 12 * 4);
        for y in 0..12 {
            for x in 0..16 {
                pixels.extend_from_slice(&[(x * 13) as u8, (y * 17) as u8, 91, if (x + y) % 3 == 0 { 127 } else { 255 }]);
            }
        }
        let img = craft_image_codecs::Image::from_u8(16, 12, craft_image_codecs::ChannelLayout::Rgba, pixels).unwrap();
        let bytes = craft_image_codecs::encode(&img, craft_image_codecs::Format::Gif, &craft_image_codecs::EncodeOptions::default()).unwrap();

        let decoded = craft_image_codecs::decode_as(craft_image_codecs::Format::Gif, &bytes).unwrap();
        let shared = craft_gif::frames::read_frames(&bytes, &craft_gif::frames::FrameLimits::default()).unwrap();
        assert_eq!(shared.frames[0].rgba, decoded.as_u8().unwrap());
    }
}
