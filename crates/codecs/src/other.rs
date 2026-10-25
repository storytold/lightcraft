//! GIF (first frame) and BMP via the `image` crate's pure-Rust decoders.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use image::{DynamicImage, ImageDecoder};

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    if format == Format::Gif {
        gated_first_frame(bytes, opts)?;
    }
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (img, icc) = match format {
        Format::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        Format::Bmp => {
            let mut d = image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
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

/// Bounds a GIF's first image descriptor: the decoder composites frames into the logical screen
/// (bounded by [`check_size`] on the canvas) but allocates its own frame buffers at the frame's
/// declared size, which a small file can inflate far past the canvas. The canvas check cannot
/// see that size, so it is read from the block headers here.
fn gated_first_frame(bytes: &[u8], opts: &DecodeOptions) -> Result<()> {
    // The constants of the header grammar: signature, logical screen descriptor (with its global
    // colour table), extension blocks (0x21, skipped by their sub-block chains) and image
    // descriptors (0x2C).
    const SCREEN: usize = 13;
    if bytes.len() < SCREEN {
        return Ok(()); // too broken for a frame; the decoder reports the damage itself
    }
    let gct_entries = 1usize << (u32::from(bytes[10] & 0x07) + 1); // at most 256
    let gct_bytes = if (bytes[10] & 0x80) != 0 { gct_entries * 3 } else { 0 };
    let mut p = SCREEN.saturating_add(gct_bytes);
    while let Some(&kind) = bytes.get(p) {
        match kind {
            0x2C => {
                // Image descriptor: two LE coordinates then two LE edges, after the separator.
                let end = p.checked_add(10).filter(|&e| e <= bytes.len());
                let Some(d) = end.and_then(|e| bytes.get(p + 1..e)) else {
                    break; // cut short mid-descriptor: the decoder reports that
                };
                let (w, h) = (u16::from_le_bytes([d[4], d[5]]) as u64, u16::from_le_bytes([d[6], d[7]]) as u64);
                return check_size(Format::Gif, w, h, opts);
            }
            0x21 => {
                // Extension: a label byte, then <len,data> sub-blocks ended by a zero length.
                p = p.saturating_add(2);
                loop {
                    let Some(&len) = bytes.get(p) else {
                        return Ok(()); // ran past the data: decode reports the truncation
                    };
                    p = p.saturating_add(1);
                    if len == 0 {
                        break;
                    }
                    p = p.saturating_add(len as usize);
                }
            }
            _ => p = p.saturating_add(1),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{DecodeOptions, Error};

    fn gif(blocks: &[u8]) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        // logical screen: 2×2, no global colour table, background 0, aspect 0
        b.extend_from_slice(&[2, 0, 2, 0, 0x00, 0, 0]);
        b.extend_from_slice(blocks);
        b
    }

    /// `image`'s GIF decoder allocates its frame buffer at the image descriptor's declared
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
}
