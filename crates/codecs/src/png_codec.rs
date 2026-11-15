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
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") && bytes.len() >= 24 {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
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

/// Stored dimensions and EXIF orientation (`eXIf`, wherever it is) from the chunk headers, without
/// inflating the image data. Refused, as a decode would refuse it: an invalid IHDR, an indexed
/// image without a palette, no IDAT, or a file that ends before the chunk after the last IDAT
/// begins (truncated image data). Chunk CRCs and the deflate stream are not checked.
pub(crate) fn header(b: &[u8]) -> Result<(u32, u32, u16)> {
    if !b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(err("bad signature"));
    }
    let be32 = |p: usize| b.get(p..p.checked_add(4)?).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]));
    if be32(8) != Some(13) || b.get(12..16) != Some(b"IHDR") {
        return Err(err("missing IHDR"));
    }
    let ihdr = b.get(16..29).ok_or_else(|| err("truncated IHDR"))?;
    let (w, h) = (u32::from_be_bytes([ihdr[0], ihdr[1], ihdr[2], ihdr[3]]), u32::from_be_bytes([ihdr[4], ihdr[5], ihdr[6], ihdr[7]]));
    let (depth, color, compression, filter, interlace) = (ihdr[8], ihdr[9], ihdr[10], ihdr[11], ihdr[12]);
    if w > i32::MAX as u32 || h > i32::MAX as u32 {
        return Err(err("dimensions out of range"));
    }
    check_size(F, w as u64, h as u64, &DecodeOptions::default())?;
    let depth_ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !depth_ok || compression != 0 || filter != 0 || interlace > 1 {
        return Err(err("invalid IHDR"));
    }
    let (mut p, mut palette, mut idat, mut after_idat, mut exif) = (8usize, false, false, false, None);
    while let Some(len) = be32(p) {
        let Some(kind) = p.checked_add(4).and_then(|k| b.get(k..k.checked_add(4)?)) else { break };
        if idat && kind != b"IDAT" {
            // a chunk follows the image data: the IDAT run is complete
            after_idat = true;
        }
        if kind == b"IEND" {
            break;
        }
        let start = p.saturating_add(8);
        let data = (len <= i32::MAX as u32).then(|| start.checked_add(len as usize).and_then(|end| b.get(start..end))).flatten();
        let Some(data) = data else {
            if after_idat {
                break; // a damaged chunk after the image data: a decode ignores it too
            }
            return Err(err("truncated"));
        };
        match kind {
            b"PLTE" => palette = true,
            b"IDAT" if after_idat => {} // a stray IDAT after other chunks: the first run is the image
            b"IDAT" => {
                if color == 3 && !palette {
                    return Err(err("indexed image without a palette"));
                }
                idat = true;
            }
            b"eXIf" if exif.is_none() => exif = Some(data.strip_prefix(b"Exif\0\0").unwrap_or(data)),
            _ => {}
        }
        p = start.saturating_add(data.len()).saturating_add(4);
    }
    if !idat {
        return Err(err("no image data"));
    }
    if !after_idat {
        return Err(err("truncated image data"));
    }
    Ok((w, h, exif.map(crate::exif::summarize).unwrap_or_default().orientation.unwrap_or(1)))
}

fn container_hint_from_chunks(b: &[u8]) -> Option<SourceSpace> {
    if !b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut has_srgb = false;
    let mut gamma: Option<f32> = None;
    let mut chrm: Option<[f64; 8]> = None;
    let mut p = 8usize;
    while let Some(len_bytes) = b.get(p..p + 4) {
        let len = u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        let Some(kind) = b.get(p + 4..p + 8) else { break };
        let Some(payload) = b.get(p + 8..p + 8 + len) else { break };
        if kind == b"sRGB" {
            has_srgb = true;
        } else if kind == b"gAMA" && payload.len() == 4 {
            let v = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as f32 / 100_000.0;
            if v > 0.0 && v.is_finite() {
                gamma = Some(v);
            }
        } else if kind == b"cHRM" && payload.len() == 32 {
            let mut vals = [0.0f64; 8];
            for (i, dst) in vals.iter_mut().enumerate() {
                let raw = i32::from_be_bytes([payload[i * 4], payload[i * 4 + 1], payload[i * 4 + 2], payload[i * 4 + 3]]);
                *dst = raw as f64 / 100_000.0;
            }
            chrm = Some(vals);
        } else if kind == b"IDAT" || kind == b"IEND" {
            break;
        }
        p = p.saturating_add(12).saturating_add(len);
    }
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
