//! GIF through `craft-gif` and BMP through the `image` crate's pure-Rust decoder.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use image::{DynamicImage, ImageDecoder};

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
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (w, h) = match format {
        Format::Bmp => image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?.dimensions(),
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

/// The decoder's allocation limit: the RGBA buffer of the largest image `opts` allows. It bounds the
/// decoders' own frame buffers only: `DynamicImage::from_decoder` allocates the canvas without
/// consulting it, so the canvas is bounded by `check_size` and [`check_fillable`] beforehand (a
/// mostly blank one by [`SPARSE_MAX_PIXELS`]).
fn limits(opts: &DecodeOptions) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(opts.max_pixels.saturating_mul(4));
    l
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
        _ => match bmp_layout(bytes) {
            BmpLayout::Rle { offset, rle4 } => bmp_rle_pixels(bytes, offset, rle4, w, h),
            BmpLayout::Rows { offset, bits } => {
                let row = u64::from(w).saturating_mul(u64::from(bits)).div_ceil(32).saturating_mul(4);
                let needed = row.saturating_mul(u64::from(h));
                let held = bytes.len().saturating_sub(offset) as u64;
                if held < needed {
                    return Err(Error::Malformed(
                        format,
                        format!("a {w} × {h} canvas is larger than its {held} bytes of pixel data can hold ({needed} needed)"),
                    ));
                }
                canvas
            }
        },
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

/// Where a BMP's pixels start (the offset in its file header, which the decoder seeks to) and how
/// they are stored.
enum BmpLayout {
    /// RLE8 or RLE4 commands.
    Rle { offset: usize, rle4: bool },
    /// Rows of `bits` per pixel, each padded to 4 bytes.
    Rows { offset: usize, bits: u16 },
}

/// The layout the decoder reads, from the file header and the bitmap header: a 12-byte core header
/// (bits per pixel at 24, never compressed) or a 40-byte or larger one (bits at 28, compression at
/// 30). Fields the decoder would have refused are read as 1 bit per pixel at offset 0, the most
/// lenient layout.
fn bmp_layout(bytes: &[u8]) -> BmpLayout {
    let u16_at = |i: usize| bytes.get(i..i.checked_add(2)?).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes);
    let u32_at = |i: usize| bytes.get(i..i.checked_add(4)?).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    let offset = u32_at(10).and_then(|o| usize::try_from(o).ok()).unwrap_or(0);
    let (bits, compression) = match u32_at(14) {
        Some(12) => (u16_at(24), Some(0)),
        _ => (u16_at(28), u32_at(30)),
    };
    match compression {
        Some(1) => BmpLayout::Rle { offset, rle4: false },
        Some(2) => BmpLayout::Rle { offset, rle4: true },
        _ => BmpLayout::Rows { offset, bits: bits.filter(|&b| b > 0).unwrap_or(1) },
    }
}

/// The pixels an RLE8 or RLE4 BMP's commands draw on a `w` × `h` canvas, read the way the decoder
/// paints them: a run is clipped at the end of its row, an end-of-row code moves to the next row, a
/// delta skips pixels (left blank), and an end-of-bitmap code, a truncated command or the last row
/// ends the picture. Every command takes at least 2 bytes, so the walk is linear in the file.
fn bmp_rle_pixels(bytes: &[u8], offset: usize, rle4: bool, w: u32, h: u32) -> u64 {
    let (w, h) = (u64::from(w), u64::from(h));
    let (mut pos, mut x, mut y, mut covered) = (offset, 0u64, 0u64, 0u64);
    while y < h {
        let (Some(&count), Some(&value)) = (bytes.get(pos), bytes.get(pos.saturating_add(1))) else {
            break;
        };
        pos = pos.saturating_add(2);
        match (count, value) {
            // end of row
            (0, 0) => (x, y) = (0, y.saturating_add(1)),
            // end of bitmap
            (0, 1) => break,
            // delta: right and down
            (0, 2) => {
                let (Some(&dx), Some(&dy)) = (bytes.get(pos), bytes.get(pos.saturating_add(1))) else {
                    break;
                };
                pos = pos.saturating_add(2);
                (x, y) = (x.saturating_add(u64::from(dx)), y.saturating_add(u64::from(dy)));
            }
            // absolute: `value` pixels stored one per byte (RLE8) or nibble (RLE4), padded to 2 bytes
            (0, n) => {
                let len = if rle4 { usize::from(n).div_ceil(2) } else { usize::from(n) };
                pos = pos.saturating_add(len + (len & 1));
                if pos > bytes.len() {
                    break;
                }
                covered = covered.saturating_add(u64::from(n).min(w.saturating_sub(x)));
                x = x.saturating_add(u64::from(n));
            }
            // a run of `count` pixels
            (n, _) => {
                covered = covered.saturating_add(u64::from(n).min(w.saturating_sub(x)));
                x = x.saturating_add(u64::from(n));
            }
        }
    }
    covered
}

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    if format == Format::Gif {
        return decode_gif(bytes, opts);
    }
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (img, icc) = match format {
        Format::Bmp => {
            let mut d = image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes, w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    let (w, h) = (img.width(), img.height());
    let alpha = img.color().has_alpha();
    let gray = !img.color().has_color();
    let (model, alpha, buf) = match (gray, alpha) {
        (true, false) => (Model::Gray, false, img.into_luma8().into_raw()),
        (true, true) => (Model::Gray, true, img.into_luma_alpha8().into_raw()),
        (false, false) => (Model::Rgb, false, img.into_rgb8().into_raw()),
        (false, true) => (Model::Rgb, true, img.into_rgba8().into_raw()),
    };
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf: Buf::U8(buf), bit_depth: 8 };
    finish(format, raw, Meta { icc, ..Default::default() }, (w, h), opts)
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
        let frames = [
            ("solid", image::RgbaImage::from_pixel(w, h, image::Rgba([90, 90, 90, 255]))),
            ("gradient", image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x / 40 * 16) as u8, (y / 30 * 16) as u8, 0, 255]))),
            ("noise", image::RgbaImage::from_fn(w, h, |_, _| image::Rgba([noise(), noise(), noise(), 255]))),
        ];
        for (name, frame) in frames {
            let mut gif = Vec::new();
            image::codecs::gif::GifEncoder::new_with_speed(&mut gif, 30).encode_frame(image::Frame::new(frame)).unwrap();
            let first = craft_gif::scan(&gif, 1).unwrap().frames.remove(0);
            assert_eq!((u32::from(first.width), u32::from(first.height)), (w, h), "{name}");
            assert_eq!(craft_gif::first_frame_decoded_pixels(&gif), Some(u64::from(w * h)), "{name}");
        }
    }

    #[test]
    fn gif_icc_profile_is_preserved_by_the_codec_adapter() {
        let mut bytes = Vec::new();
        image::codecs::gif::GifEncoder::new_with_speed(&mut bytes, 30)
            .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba([80, 120, 160, 255]))))
            .unwrap();
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
        let mut bytes = Vec::new();
        let pixels =
            image::RgbaImage::from_fn(16, 12, |x, y| image::Rgba([(x * 13) as u8, (y * 17) as u8, 91, if (x + y) % 3 == 0 { 127 } else { 255 }]));
        image::codecs::gif::GifEncoder::new_with_speed(&mut bytes, 30).encode_frame(image::Frame::new(pixels)).unwrap();

        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).unwrap();
        let legacy = DynamicImage::from_decoder(decoder).unwrap().into_rgba8().into_raw();
        let shared = craft_gif::frames::read_frames(&bytes, &craft_gif::frames::FrameLimits::default()).unwrap();
        assert_eq!(shared.frames[0].rgba, legacy);
    }
}
