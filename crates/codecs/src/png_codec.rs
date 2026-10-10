//! PNG decode via the `png` crate (8/16-bit, palette, tRNS, iCCP, eXIf, iTXt XMP, sRGB/cICP/cHRM/gAMA).

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use lightcraft_color::{Mat3, RgbSpace, Xy};

const F: Format = Format::Png;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    dec.set_limits(png::Limits { bytes: opts.max_pixels.saturating_mul(8).min(usize::MAX as u64) as usize });
    let mut reader = dec.read_info().map_err(err)?;
    let (w, h) = {
        let i = reader.info();
        (i.width, i.height)
    };
    check_size(F, w as u64, h as u64, opts)?;
    let size = reader.output_buffer_size().ok_or_else(|| err("buffer size overflow"))?;
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(err)?;
    // Text chunks after IDAT land in `info` once the stream is finished; ignore trailing errors.
    let _ = reader.finish();
    let info = reader.info();

    let (model, alpha) = match out.color_type {
        png::ColorType::Grayscale => (Model::Gray, false),
        png::ColorType::GrayscaleAlpha => (Model::Gray, true),
        png::ColorType::Rgb => (Model::Rgb, false),
        png::ColorType::Rgba => (Model::Rgb, true),
        png::ColorType::Indexed => return Err(err("palette not expanded")),
    };
    buf.truncate(out.buffer_size());
    let (samples, depth) = match out.bit_depth {
        png::BitDepth::Sixteen => (Buf::U16(buf.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect()), 16),
        _ => (Buf::U8(buf), info.bit_depth as u8),
    };
    let raw = Raw { width: out.width as usize, height: out.height as usize, model, alpha, premultiplied: false, buf: samples, bit_depth: depth };

    let xmp = info
        .utf8_text
        .iter()
        .find(|t| t.keyword == "XML:com.adobe.xmp")
        .and_then(|t| t.get_text().ok())
        .or_else(|| info.uncompressed_latin1_text.iter().find(|t| t.keyword == "XML:com.adobe.xmp").map(|t| t.text.clone()));
    let exif = info.exif_metadata.as_ref().map(|e| {
        let e = e.to_vec();
        // Some writers keep the JPEG "Exif\0\0" prefix.
        if e.starts_with(b"Exif\0\0") { e[6..].to_vec() } else { e }
    });
    let meta = Meta { icc: info.icc_profile.as_ref().map(|c| c.to_vec()), exif, xmp, hint: container_hint(info), ..Default::default() };
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

fn container_hint(info: &png::Info) -> Option<SourceSpace> {
    if let Some(s) =
        info.coding_independent_code_points.as_ref().and_then(|c| SourceSpace::from_cicp(c.color_primaries.into(), c.transfer_function.into()))
    {
        return Some(s);
    }
    if info.srgb.is_some() {
        return Some(SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::Container));
    }
    let gamma = info.gama_chunk.or(info.source_gamma).map(|g| g.into_value()).filter(|g| *g > 0.0 && g.is_finite());
    let chrm = info.chrm_chunk.or(info.source_chromaticities);
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
        let xy = |p: (png::ScaledFloat, png::ScaledFloat)| Xy::new(p.0.into_value() as f64, p.1.into_value() as f64);
        let space = RgbSpace { name: "PNG cHRM", r: xy(c.red), g: xy(c.green), b: xy(c.blue), white: xy(c.white) };
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
