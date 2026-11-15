//! AVIF container and pixel decoding via shared image codecs.
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Avif;

#[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
pub(crate) fn header(_: &[u8]) -> Result<(u32, u32, u16)> {
    Err(Error::Unsupported(F, "AVIF decoding is not included in this build"))
}
#[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
pub(crate) fn decode(_: &[u8], _: &DecodeOptions) -> Result<Decoded> {
    Err(Error::Unsupported(F, "AVIF decoding is not included in this build"))
}

#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn err(e: craft_image_codecs::CodecError) -> Error {
    match e {
        craft_image_codecs::CodecError::LimitExceeded(_) => Error::TooLarge(0, 0),
        craft_image_codecs::CodecError::Unsupported { .. } => Error::Unsupported(F, "unsupported AVIF"),
        other => Error::Malformed(F, other.to_string()),
    }
}
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let (w, h) = craft_image_codecs::probe_size(bytes).map_err(err)?;
    Ok((w, h, 1))
}
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
    use crate::space::SourceSpace;
    let (w, h, _) = header(bytes)?;
    check_size(F, w as u64, h as u64, opts)?;
    let options = craft_image_codecs::DecodeOptions {
        limits: craft_image_codecs::Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: opts.max_pixels,
            max_alloc: opts.max_pixels.saturating_mul(32),
        },
        keep_orientation: false,
    };
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::Avif, bytes, &options).map_err(err)?;
    let (w, h) = (img.width(), img.height());
    check_size(F, w as u64, h as u64, opts)?;
    let (buf, bit_depth) = match img.sample_type() {
        craft_image_codecs::SampleType::U8 => (Buf::U8(img.as_u8().ok_or_else(|| Error::Malformed(F, "expected 8-bit samples".into()))?.to_vec()), 8),
        craft_image_codecs::SampleType::U16 => {
            (Buf::U16(img.to_u16_samples().ok_or_else(|| Error::Malformed(F, "expected 16-bit samples".into()))?), 16)
        }
        craft_image_codecs::SampleType::F32 => {
            (Buf::F32(img.to_f32_samples().ok_or_else(|| Error::Malformed(F, "expected float samples".into()))?), 32)
        }
        _ => return Err(Error::Unsupported(F, "unsupported AVIF sample type")),
    };
    let hint = img.cicp.and_then(|c| SourceSpace::from_cicp(c.primaries, c.transfer));
    let raw = Raw { width: w as usize, height: h as usize, model: Model::Rgb, alpha: img.layout().has_alpha(), premultiplied: false, buf, bit_depth };
    let meta = Meta { icc: img.icc, exif: img.meta.exif, xmp: img.meta.xmp, orientation: Some(1), hint, ..Default::default() };
    finish(F, raw, meta, (w, h), opts)
}

#[cfg(all(test, feature = "avif", not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/quad-444-gbr-lossless.avif");
    #[test]
    fn production_entrypoints_decode_avif_and_enforce_pixel_budget() {
        assert!(F.can_decode());
        assert_eq!(crate::sniff(FIXTURE), Some(F));
        let header = crate::read_header(FIXTURE).unwrap();
        assert_eq!((header.width, header.height, header.orientation), (33, 17, 1));
        let decoded = crate::decode(FIXTURE, DecodeOptions::default()).unwrap();
        assert_eq!((decoded.source_width, decoded.source_height), (33, 17));
        let opts = DecodeOptions { max_pixels: 100, ..DecodeOptions::default() };
        assert!(matches!(crate::decode(FIXTURE, opts), Err(Error::TooLarge(..))));
    }
    #[test]
    fn hdr_avif_round_trips_highlights_through_public_decoder() {
        let pixels = vec![2.0f32; 16 * 16 * 3];
        for transfer in [crate::encode::HdrTransfer::Pq, crate::encode::HdrTransfer::Hlg] {
            let bytes = crate::encode::encode_avif_hdr(16, 16, &pixels, 95, 10, &crate::encode::EncodeMeta::default(), transfer).unwrap();
            let decoded = crate::decode(&bytes, DecodeOptions::default()).unwrap();
            assert!(decoded.image.data.iter().any(|pixel| pixel[0] > 1.0), "HDR highlights were lost");
        }
    }
}
