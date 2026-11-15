//! PNG decode via `craft-image-codecs` (8/16-bit, palette, tRNS, iCCP, eXIf, iTXt XMP, sRGB/cICP/cHRM/gAMA).

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use lightcraft_color::{Mat3, RgbSpace, Xy};

const F: Format = Format::Png;

/// Expansion cap for a compressed text chunk's payload: real XMP documents are a few hundred
/// kilobytes, so anything past this is hostile input, not metadata.
const MAX_TEXT_CHUNK: usize = 4 << 20;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    if let Some((w, h)) = craft_image_codecs::png_probe::declared_dimensions(bytes) {
        check_size(F, w as u64, h as u64, opts)?;
    }
    let ic_opts = craft_image_codecs::DecodeOptions {
        limits: craft_image_codecs::Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: opts.max_pixels,
            max_alloc: opts.max_pixels.saturating_mul(8),
        },
        keep_orientation: true,
    };
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::Png, bytes, &ic_opts).map_err(|e| match e {
        craft_image_codecs::CodecError::LimitExceeded(_) => Error::TooLarge(0, 0),
        other => err(other),
    })?;
    let (w, h) = (img.width(), img.height());
    check_size(F, w as u64, h as u64, opts)?;
    let model = if img.layout().is_gray() { Model::Gray } else { Model::Rgb };
    let alpha = img.layout().has_alpha();
    let (samples, depth) = match img.sample_type() {
        craft_image_codecs::SampleType::U16 => (Buf::U16(img.to_u16_samples().ok_or_else(|| err("expected 16-bit samples"))?), 16),
        _ => (Buf::U8(img.as_u8().ok_or_else(|| err("expected 8-bit samples"))?.to_vec()), 8),
    };
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf: samples, bit_depth: depth };

    let cicp_hint = img.cicp.as_ref().and_then(|c| SourceSpace::from_cicp(c.primaries, c.transfer));
    let hint = cicp_hint.or_else(|| container_hint_from_chunks(bytes));
    let xmp = img.meta.xmp.filter(|x| x.len() <= MAX_TEXT_CHUNK);
    let exif = img.meta.exif.map(|e| if e.starts_with(b"Exif\0\0") { e[6..].to_vec() } else { e });
    let meta = Meta { icc: img.icc, exif, xmp, hint, ..Default::default() };
    finish(F, raw, meta, (w, h), opts)
}

/// Stored dimensions and EXIF orientation without inflating pixels.
pub(crate) fn header(b: &[u8]) -> Result<(u32, u32, u16)> {
    let (w, h, orientation) =
        craft_image_codecs::png_probe::header_with_max_pixels(b, DecodeOptions::default().max_pixels).map_err(|error| match error {
            craft_image_codecs::CodecError::LimitExceeded(_) => {
                let (w, h) = craft_image_codecs::png_probe::declared_dimensions(b).unwrap_or((0, 0));
                Error::TooLarge(w as u64, h as u64)
            }
            other => err(other),
        })?;
    Ok((w, h, orientation))
}

fn container_hint_from_chunks(b: &[u8]) -> Option<SourceSpace> {
    let chunks = craft_image_codecs::png_probe::color_chunks(b)?;
    let has_srgb = chunks.srgb;
    let gamma = chunks.gamma;
    let chrm = chunks.chromaticities;
    if has_srgb {
        return Some(SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::Container));
    }
    if gamma.is_none() && chrm.is_none() {
        return None;
    }
    let trc = match gamma {
        Some(g) if (g - 1.0).abs() < 1e-3 => Trc::Linear,
        Some(g) => Trc::Gamma(1.0 / g),
        None => Trc::Srgb,
    };
    let mut s = SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::Container);
    if let Some(c) = chrm {
        let space = RgbSpace { name: "PNG cHRM", white: Xy::new(c[0], c[1]), r: Xy::new(c[2], c[3]), g: Xy::new(c[4], c[5]), b: Xy::new(c[6], c[7]) };
        let valid = [space.r, space.g, space.b, space.white].iter().all(|p| p.y > 1e-4 && p.x >= 0.0 && p.x + p.y <= 1.0 + 1e-6);
        let m = if valid { rgb_to_xyz_d50_checked(&space) } else { None };
        if let Some(m) = m {
            s.to_xyz_d50 = m;
            s.named = NamedSpace::recognize(&m);
        }
    }
    s.trc = Some([trc.clone(), trc.clone(), trc]);
    Some(s)
}

fn rgb_to_xyz_d50_checked(space: &RgbSpace) -> Option<Mat3> {
    let p = [space.r.to_xyz(), space.g.to_xyz(), space.b.to_xyz()];
    let m = Mat3([[p[0][0], p[1][0], p[2][0]], [p[0][1], p[1][1], p[2][1]], [p[0][2], p[1][2], p[2][2]]]);
    if m.determinant().abs() < 1e-9 {
        return None;
    }
    Some(crate::space::rgb_to_xyz_d50(space))
}

#[cfg(test)]
mod tests {
    use crate::DecodeOptions;

    /// A compressed text chunk is expanded by `png` through our own cap: an XMP document
    /// bigger than it is dropped, not inflated. Compressing an 8 MiB string is a few
    /// kilobytes — the reverse of what the unbounded `get_text()` path allowed.
    #[test]
    fn exif_orientation_mutation_parity() {
        let image = craft_image_codecs::Image::from_u8(1, 1, craft_image_codecs::ChannelLayout::Gray, vec![0]).unwrap();
        let base = craft_image_codecs::encode(&image, craft_image_codecs::Format::Png, &craft_image_codecs::EncodeOptions::default()).unwrap();
        let exif = crate::exif::minimal_exif(6);
        for at in 0..exif.len() {
            for value in [0, 255] {
                let mut data = exif.clone();
                data[at] = value;
                let mut png = base.clone();
                let iend = png.len() - 12;
                png.splice(iend..iend, png_chunk(*b"eXIf", &data));
                assert_eq!(super::header(&png).unwrap().2, crate::exif::summarize(&data).orientation.unwrap_or(1), "at {at}, value {value}");
            }
        }
    }

    #[test]
    fn compressed_xmp_over_the_cap_is_dropped() {
        let out = png_with_compressed_xmp(&"x".repeat(8 << 20));
        let d = crate::decode(&out, DecodeOptions::default()).unwrap();
        assert!(d.xmp.is_none());
        assert_eq!((d.width, d.height), (1, 1));
    }

    /// XMP within the cap is still read, compressed or not.
    #[test]
    fn compressed_xmp_under_the_cap_is_read() {
        let out = png_with_compressed_xmp(&"<?xpacket?><xmp/>".repeat(100));
        let d = crate::decode(&out, DecodeOptions::default()).unwrap();
        assert!(d.xmp.as_deref().is_some_and(|x| x.contains("xmp/")));
    }

    /// A little-endian PNG with one compressed iTXt chunk carrying `payload` as XMP.
    fn png_with_compressed_xmp(payload: &str) -> Vec<u8> {
        let img = craft_image_codecs::Image::from_u8(1, 1, craft_image_codecs::ChannelLayout::Gray, vec![0x7F]).unwrap();
        let base = craft_image_codecs::encode(&img, craft_image_codecs::Format::Png, &craft_image_codecs::EncodeOptions::default()).unwrap();
        // Build a valid zlib stream using uncompressed DEFLATE stored blocks (<= 65535 bytes each).
        let raw = payload.as_bytes();
        let mut z = vec![0x78u8, 0x01];
        let mut offset = 0usize;
        loop {
            let end = (offset + 65535).min(raw.len());
            let block = &raw[offset..end];
            let is_last = end == raw.len();
            let len = block.len() as u16;
            z.push(u8::from(is_last));
            z.extend_from_slice(&len.to_le_bytes());
            z.extend_from_slice(&(!len).to_le_bytes());
            z.extend_from_slice(block);
            offset = end;
            if is_last {
                break;
            }
        }
        let (mut s1, mut s2) = (1u32, 0u32);
        for &b in raw {
            s1 = (s1 + u32::from(b)) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        z.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
        let mut itxt = Vec::new();
        itxt.extend_from_slice(b"XML:com.adobe.xmp\0");
        itxt.push(1); // compression_flag = 1
        itxt.push(0); // compression_method = 0
        itxt.extend_from_slice(b"\0\0"); // empty language_tag + translated_keyword
        itxt.extend_from_slice(&z);
        let chunk = png_chunk(*b"iTXt", &itxt);
        let split = base.len() - 12;
        let mut out = base[..split].to_vec();
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&base[split..]);
        out
    }

    fn png_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in &out[4..] {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
        out
    }
}
