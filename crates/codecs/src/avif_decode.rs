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
// Match craft-video's AV1 store: 128-sample superblocks plus 160 samples of edge padding,
// three u16 planes, eight references, the current picture, and at most eight workers + two
// in-flight pictures. Keep a hard byte ceiling even when the caller permits many pixels.
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn picture_store_budget(width: u32, height: u32) -> u64 {
    let padded = |n: u32| u64::from(n).div_ceil(128) * 128 + 160;
    padded(width).saturating_mul(padded(height)).saturating_mul(PICTURE_STORE_BYTES_PER_SAMPLE).min(PICTURE_STORE_CAP)
}
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
const PICTURE_STORE_BYTES_PER_SAMPLE: u64 = 3 * 2 * (8 + 1 + 8 + 2);
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
const PICTURE_STORE_CAP: u64 = if cfg!(target_pointer_width = "64") { 4 << 30 } else { 1 << 30 };

/// The picture-store budget for any stored AV1 canvas the pixel limit admits. `probe_size` reports
/// the size after `clap`, but the decoder builds the stored (pre-crop) canvas, which the shared
/// decoder checks against `max_pixels` from its `ispe` before decoding. A canvas of at most
/// `max_pixels` samples with AV1's 65,536-sample edge limit has `w + h <= min(max_pixels + 1, 2^17)`,
/// and each padded edge is under `edge + 288`, so the padded area is below
/// `max_pixels + 288 * (w + h) + 288^2`.
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn stored_picture_store_budget(max_pixels: u64) -> u64 {
    let edge_sum = max_pixels.saturating_add(1).min(2 * 65_536);
    let area = max_pixels.saturating_add(edge_sum.saturating_mul(288)).saturating_add(288 * 288);
    area.saturating_mul(PICTURE_STORE_BYTES_PER_SAMPLE).min(PICTURE_STORE_CAP)
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
            max_alloc: stored_picture_store_budget(opts.max_pixels).max(picture_store_budget(w, h)),
        },
        keep_orientation: false,
    };
    let (img, bit_depth) = craft_image_codecs::decode_avif_with_depth(bytes, &options).map_err(err)?;
    let (w, h) = (img.width(), img.height());
    check_size(F, w as u64, h as u64, opts)?;
    let buf = match img.sample_type() {
        craft_image_codecs::SampleType::U8 => Buf::U8(img.as_u8().ok_or_else(|| Error::Malformed(F, "expected 8-bit samples".into()))?.to_vec()),
        craft_image_codecs::SampleType::U16 => Buf::U16(img.to_u16_samples().ok_or_else(|| Error::Malformed(F, "expected 16-bit samples".into()))?),
        craft_image_codecs::SampleType::F32 => Buf::F32(img.to_f32_samples().ok_or_else(|| Error::Malformed(F, "expected float samples".into()))?),
        _ => return Err(Error::Unsupported(F, "unsupported AVIF sample type")),
    };
    let hint = img.cicp.and_then(|c| SourceSpace::from_cicp(c.primaries, c.transfer));
    let raw = Raw { width: w as usize, height: h as usize, model: Model::Rgb, alpha: img.layout().has_alpha(), premultiplied: false, buf, bit_depth };
    let meta = Meta { icc: img.icc, exif: img.meta.exif, xmp: img.meta.xmp, orientation: Some(1), hint, ..Default::default() };
    let mut decoded = finish(F, raw, meta, (w, h), opts)?;
    // AV1 stores integer samples even when HDR transfer decoding produces linear float pixels.
    decoded.float = false;
    Ok(decoded)
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
        assert_eq!(decoded.bit_depth, 8);
        let exact = DecodeOptions { max_pixels: 33 * 17, ..DecodeOptions::default() };
        let decoded = crate::decode(FIXTURE, exact).unwrap();
        assert_eq!((decoded.source_width, decoded.source_height), (33, 17));
        assert!(!decoded.float);
        let opts = DecodeOptions { max_pixels: 33 * 17 - 1, ..DecodeOptions::default() };
        assert!(matches!(crate::decode(FIXTURE, opts), Err(Error::TooLarge(..))));
    }
    #[test]
    fn picture_store_budget_accounts_for_padding_and_stays_bounded() {
        assert_eq!(picture_store_budget(33, 17), 288 * 288 * 6 * 19);
        let cap = if cfg!(target_pointer_width = "64") { 4 << 30 } else { 1 << 30 };
        assert_eq!(picture_store_budget(u32::MAX, u32::MAX), cap);
        assert_eq!(stored_picture_store_budget(u64::MAX), cap);
        // Every canvas the pixel limit admits fits, whatever its shape.
        for max_pixels in [1u64, 561, 512 * 512, 1 << 20] {
            let budget = stored_picture_store_budget(max_pixels);
            for (w, h) in [(1, max_pixels), (max_pixels, 1), (max_pixels.isqrt(), max_pixels.isqrt())] {
                let (w, h) = (w.min(65_536) as u32, h.min(65_536) as u32);
                assert!(picture_store_budget(w, h) <= budget, "{w}x{h} under {max_pixels}");
            }
        }
    }

    /// `probe_size` reports the `clap`-cropped size; the decoder still builds the stored canvas.
    /// A 512×512 picture cropped to 16×16 must decode with the default options and at a pixel
    /// limit that covers the stored canvas, and fail as too large below it.
    #[test]
    fn cropped_avif_decodes_with_a_budget_for_its_stored_canvas() {
        let pixels = [90u8, 140, 200].repeat(512 * 512);
        let image = crate::encode::EncodeImage::new(512, 512, 3, crate::encode::Samples::U8(&pixels));
        let full = crate::encode::encode_avif(&image, 60, 10, &crate::encode::EncodeMeta::default()).unwrap();
        let cropped = with_clap(&full, 16, 16);
        assert_eq!(crate::read_header(&cropped).map(|h| (h.width, h.height)).unwrap(), (16, 16));
        // The old budget, sized from the cropped 16×16, is below what the stored canvas needs.
        assert!(picture_store_budget(16, 16) < picture_store_budget(512, 512));
        let decoded = crate::decode(&cropped, DecodeOptions::default()).unwrap();
        assert_eq!((decoded.source_width, decoded.source_height), (16, 16));
        let exact = DecodeOptions { max_pixels: 512 * 512, ..DecodeOptions::default() };
        assert_eq!(crate::decode(&cropped, exact).map(|d| (d.source_width, d.source_height)).unwrap(), (16, 16));
        let below = DecodeOptions { max_pixels: 512 * 512 - 1, ..DecodeOptions::default() };
        assert!(crate::decode(&cropped, below).is_err());
    }

    fn be32(b: &[u8], at: usize) -> usize {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize
    }
    fn put32(b: &mut [u8], at: usize, v: usize) {
        b[at..at + 4].copy_from_slice(&(v as u32).to_be_bytes());
    }
    /// Child boxes of `b[start..end]`: (offset, size, type).
    fn boxes(b: &[u8], start: usize, end: usize) -> Vec<(usize, usize, [u8; 4])> {
        let mut out = Vec::new();
        let mut at = start;
        while at + 8 <= end {
            let size = be32(b, at);
            out.push((at, size, b[at + 4..at + 8].try_into().unwrap()));
            at += size;
        }
        out
    }
    fn find(b: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> (usize, usize) {
        boxes(b, start, end).into_iter().find(|x| &x.2 == kind).map(|x| (x.0, x.1)).unwrap()
    }

    /// Adds a centred `clap` of `w × h` to the primary item of a single-item AVIF from the encoder,
    /// fixing the enclosing box sizes and the `iloc` offsets into `mdat`.
    fn with_clap(file: &[u8], w: u32, h: u32) -> Vec<u8> {
        let mut b = file.to_vec();
        let (meta, meta_size) = find(&b, 0, b.len(), b"meta");
        let children = meta + 12;
        let (iprp, _) = find(&b, children, meta + meta_size, b"iprp");
        let (ipco, ipco_size) = find(&b, iprp + 8, iprp + be32(&b, iprp), b"ipco");
        let index = boxes(&b, ipco + 8, ipco + ipco_size).len() + 1;
        let mut clap = Vec::new();
        for v in [w, 1, h, 1, 0, 1, 0, 1] {
            clap.extend_from_slice(&v.to_be_bytes());
        }
        let clap = [&((8 + clap.len()) as u32).to_be_bytes()[..], b"clap", &clap].concat();
        // ipma: version/flags, entry count, then item id, association count, associations.
        let (ipma, ipma_size) = find(&b, iprp + 8, iprp + be32(&b, iprp), b"ipma");
        let (version, flags) = (b[ipma + 8], b[ipma + 11]);
        assert_eq!(be32(&b, ipma + 12), 1, "one item");
        let id_len = if version < 1 { 2 } else { 4 };
        let count_at = ipma + 16 + id_len;
        let wide = flags & 1 == 1;
        let assoc_end = count_at + 1 + b[count_at] as usize * if wide { 2 } else { 1 };
        assert_eq!(assoc_end, ipma + ipma_size);
        let assoc: Vec<u8> = if wide { (0x8000u16 | index as u16).to_be_bytes().to_vec() } else { vec![0x80 | index as u8] };
        let grow = clap.len() + assoc.len();
        b[count_at] += 1;
        b.splice(assoc_end..assoc_end, assoc.iter().copied());
        b.splice(ipco + ipco_size..ipco + ipco_size, clap.iter().copied());
        // ipco precedes ipma inside iprp, so ipma moved by the clap's length.
        for (at, by) in [(meta, grow), (iprp, grow), (ipco, clap.len()), (ipma + clap.len(), assoc.len())] {
            let size = be32(&b, at);
            put32(&mut b, at, size + by);
        }
        // iloc (version 0 or 1): shift every absolute extent offset by the growth of `meta`.
        let (iloc, _) = find(&b, children, meta + be32(&b, meta), b"iloc");
        let v = b[iloc + 8];
        let (offset_size, length_size, base_size) = ((b[iloc + 12] >> 4) as usize, (b[iloc + 12] & 15) as usize, (b[iloc + 13] >> 4) as usize);
        let index_size = if v >= 1 { (b[iloc + 13] & 15) as usize } else { 0 };
        let mut at = iloc + 14;
        let items = if v < 2 {
            let n = u16::from_be_bytes([b[at], b[at + 1]]) as usize;
            at += 2;
            n
        } else {
            let n = be32(&b, at);
            at += 4;
            n
        };
        let read = |b: &[u8], at: usize, n: usize| b[at..at + n].iter().fold(0usize, |acc, &x| acc << 8 | x as usize);
        let write = |b: &mut [u8], at: usize, n: usize, v: usize| {
            for i in 0..n {
                b[at + i] = (v >> (8 * (n - 1 - i))) as u8;
            }
        };
        for _ in 0..items {
            at += if v < 2 { 2 } else { 4 };
            if v >= 1 {
                at += 2;
            }
            at += 2;
            let base = read(&b, at, base_size);
            if base_size > 0 && base > 0 {
                write(&mut b, at, base_size, base + grow);
            }
            at += base_size;
            let extents = u16::from_be_bytes([b[at], b[at + 1]]) as usize;
            at += 2;
            for _ in 0..extents {
                at += index_size;
                let off = read(&b, at, offset_size);
                if base == 0 && offset_size > 0 {
                    write(&mut b, at, offset_size, off + grow);
                }
                at += offset_size + length_size;
            }
        }
        b
    }
    #[test]
    fn hdr_avif_round_trips_highlights_through_public_decoder() {
        let pixels = vec![2.0f32; 16 * 16 * 3];
        for transfer in [crate::encode::HdrTransfer::Pq, crate::encode::HdrTransfer::Hlg] {
            let bytes = crate::encode::encode_avif_hdr(16, 16, &pixels, 95, 10, &crate::encode::EncodeMeta::default(), transfer).unwrap();
            let decoded = crate::decode(&bytes, DecodeOptions::default()).unwrap();
            assert_eq!(decoded.bit_depth, 10);
            assert!(!decoded.float, "AV1 source samples are integer for {transfer:?}");
            assert!(decoded.image.data.iter().any(|pixel| pixel[0] > 1.0), "HDR highlights were lost");
        }
    }
    #[test]
    fn avif_alpha_survives_public_round_trip() {
        let pixels = [200u8, 100, 50, 96].repeat(16 * 16);
        let image = crate::encode::EncodeImage::new(16, 16, 4, crate::encode::Samples::U8(&pixels));
        let bytes = crate::encode::encode_avif(&image, 95, 10, &crate::encode::EncodeMeta::default()).unwrap();
        let decoded = crate::decode(&bytes, DecodeOptions::default()).unwrap();
        let alpha = decoded.alpha.expect("alpha plane");
        assert!((alpha.data[0] - 96.0 / 255.0).abs() < 0.04);
    }
}
