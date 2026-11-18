//! PNG decode via the `png` crate (8/16-bit, palette, tRNS, iCCP, eXIf, iTXt XMP, sRGB/cICP/cHRM/gAMA).

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish, try_filled};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use lcc_color::{Mat3, RgbSpace, Xy};

const F: Format = Format::Png;

/// Expansion cap for a compressed text chunk's payload: real XMP documents are a few hundred
/// kilobytes, so anything past this is hostile input, not metadata.
const MAX_TEXT_CHUNK: usize = 4 << 20;

/// Deflate's largest expansion: a 258-byte match coded in two bits, 1032 bytes per input byte.
const MAX_DEFLATE_RATIO: u64 = 1032;

/// The bytes the first run of IDAT chunks holds in this file, as much of a chunk as the file has. Later runs are not
/// the image: the decoder refuses an IDAT after other chunks. Adapted from upstream #3912.
fn idat_bytes(b: &[u8]) -> u64 {
    let (mut p, mut total, mut in_run) = (8usize, 0u64, false);
    while let (Some(len), Some(kind)) = (b.get(p..p.saturating_add(4)), b.get(p.saturating_add(4)..p.saturating_add(8))) {
        let len = len.try_into().map(u32::from_be_bytes).map_or(0, |n| n as usize);
        let data = p.saturating_add(8);
        if kind == b"IDAT" {
            in_run = true;
            total = total.saturating_add(len.min(b.len().saturating_sub(data)) as u64);
        } else if in_run {
            break;
        }
        p = data.saturating_add(len).saturating_add(4);
    }
    total
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    // the byte budget, not a multiple of the pixel cap (which allowed 8 GiB, issue #2125)
    dec.set_limits(png::Limits { bytes: opts.max_bytes.min(isize::MAX as u64) as usize });
    let mut reader = dec.read_info().map_err(err)?;
    let (w, h, bits) = {
        let i = reader.info();
        let samples: u64 = match i.color_type {
            png::ColorType::Grayscale | png::ColorType::Indexed => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
        };
        (i.width, i.height, samples * i.bit_depth as u64)
    };
    check_size(F, w as u64, h as u64, opts)?;
    // The pixel bytes inflate from the file's IDAT data and Deflate expands at most 1032:1: an IHDR claiming more pixel
    // bytes than its IDAT data can inflate to is refused before the output buffer is made (upstream #3912).
    let pixel_bytes = (w as u64).saturating_mul(h as u64).saturating_mul(bits) / 8;
    let idat = idat_bytes(bytes);
    if pixel_bytes > idat.saturating_mul(MAX_DEFLATE_RATIO) {
        return Err(err(format!("{w}×{h} image data cannot come from {idat} bytes of IDAT")));
    }
    let size = reader.output_buffer_size().ok_or_else(|| err("buffer size overflow"))?;
    if size as u64 > opts.max_bytes {
        return Err(Error::TooLarge(w.into(), h.into()));
    }
    let mut buf = try_filled(size, 0u8, (w as usize, h as usize))?;
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

    // png's `get_text()` inflates a compressed chunk with no bound of its own: a tiny
    // iTXt can claim an XMP document of arbitrary size. Expand through the bounded path
    // instead, and drop any XMP bigger than the cap.
    let xmp = info
        .utf8_text
        .iter()
        .find(|t| t.keyword == "XML:com.adobe.xmp")
        .and_then(|t| {
            let mut t = t.clone();
            t.decompress_text_with_limit(MAX_TEXT_CHUNK).ok()?;
            t.get_text().ok()
        })
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

#[cfg(test)]
mod tests {
    use crate::DecodeOptions;

    fn crc(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let c = crc(&out[start..]);
        out.extend(c.to_be_bytes());
    }

    /// A compressed text chunk is expanded by `png` through our own cap: an XMP document
    /// bigger than it is dropped, not inflated. Compressing an 8 MiB string is a few
    /// kilobytes — the reverse of what the unbounded `get_text()` path allowed.
    #[test]
    fn compressed_xmp_is_read_under_the_cap_and_dropped_over_it() {
        let out = png_with_compressed_xmp(&"x".repeat(8 << 20));
        let d = crate::decode(&out, DecodeOptions::default()).unwrap();
        assert!(d.xmp.is_none());
        assert_eq!((d.width, d.height), (1, 1));
        // compressed xmp under the cap is read
        // XMP within the cap is still read, compressed or not.
        {
            let out = png_with_compressed_xmp(&"<?xpacket?><xmp/>".repeat(100));
            let d = crate::decode(&out, DecodeOptions::default()).unwrap();
            assert!(d.xmp.as_deref().is_some_and(|x| x.contains("xmp/")));
        }
    }

    /// #2562: an IHDR claiming 20000×20000 RGBA16 (3.2 GB of pixel bytes) in a file of under 100 bytes is refused
    /// as malformed before the output buffer is made, even under a cap that allows its pixel count.
    #[test]
    fn ihdr_larger_than_its_data_can_inflate_is_refused() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend(20000u32.to_be_bytes());
        ihdr.extend(20000u32.to_be_bytes());
        ihdr.extend([16, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        // a zlib stream with one empty stored block
        chunk(&mut png, b"IDAT", &[0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01]);
        chunk(&mut png, b"IEND", &[]);
        assert!(png.len() < 100);
        let opts = DecodeOptions { max_pixels: 1 << 30, max_bytes: 1 << 40, ..DecodeOptions::default() };
        // refused by the inflate bound itself, not later by the empty IDAT once the 3.2 GB buffer is made
        assert!(matches!(crate::decode(&png, opts), Err(crate::Error::Malformed(_, m)) if m.contains("cannot come from")));
    }

    /// The pixels come from the IDAT chunks only: bytes elsewhere in the file don't lift the inflate bound (upstream #3912).
    #[test]
    fn ancillary_bytes_do_not_lift_the_inflate_bound() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend(20000u32.to_be_bytes());
        ihdr.extend(20000u32.to_be_bytes());
        ihdr.extend([1, 3, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"PLTE", &[0, 0, 0, 255, 255, 255]);
        chunk(&mut png, b"tRNS", &[0, 255]);
        chunk(&mut png, b"teXt", &[b'x'; 60_000]);
        chunk(&mut png, b"IDAT", &[0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01]);
        chunk(&mut png, b"IEND", &[]);
        let opts = DecodeOptions { max_pixels: 1 << 30, max_bytes: 1 << 40, ..DecodeOptions::default() };
        assert!(matches!(crate::decode(&png, opts), Err(crate::Error::Malformed(_, m)) if m.contains("cannot come from")));
    }

    /// An image split over several IDAT chunks counts them all; a truncated or oversized chunk length never panics.
    #[test]
    fn inflate_bound_counts_every_idat_chunk_and_survives_hostile_lengths() {
        let (w, h) = (2000u32, 2000u32);
        let mut whole = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut whole, w, h);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_compression(png::Compression::High);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&vec![0u8; (w * h) as usize]).unwrap();
        }
        let (mut p, mut idat) = (8, Vec::new());
        while p + 8 <= whole.len() {
            let len = u32::from_be_bytes([whole[p], whole[p + 1], whole[p + 2], whole[p + 3]]) as usize;
            if &whole[p + 4..p + 8] == b"IDAT" {
                idat.extend_from_slice(&whole[p + 8..p + 8 + len]);
            }
            p += len + 12;
        }
        const PIECE: usize = 64;
        assert!(PIECE as u64 * super::MAX_DEFLATE_RATIO < u64::from(w * h), "one chunk must fall short of the bound");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 0, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        for piece in idat.chunks(PIECE) {
            chunk(&mut png, b"IDAT", piece);
        }
        chunk(&mut png, b"IEND", &[]);
        let d = crate::decode(&png, DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (w, h));
        // hostile lengths: every truncation of the file, and a chunk claiming u32::MAX bytes
        for cut in 0..png.len().min(400) {
            let _ = super::idat_bytes(&png[..cut]);
        }
        let mut huge = b"\x89PNG\r\n\x1a\n".to_vec();
        huge.extend(u32::MAX.to_be_bytes());
        huge.extend(b"IDAT");
        assert_eq!(super::idat_bytes(&huge), 0);
    }

    /// A little-endian PNG with one compressed iTXt chunk carrying `payload` as XMP.
    fn png_with_compressed_xmp(payload: &str) -> Vec<u8> {
        use png::text_metadata::{EncodableTextChunk, ITXtChunk};
        let mut base = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut base, 1, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0x7F]).unwrap();
            w.finish().unwrap();
        }
        // `ITXtChunk::encode` writes the complete chunk (length, type, payload, CRC), with
        // the text compressed.
        let mut chunk = Vec::new();
        let mut xmp = ITXtChunk::new("XML:com.adobe.xmp", payload);
        xmp.compressed = true;
        xmp.compress_text().unwrap();
        xmp.encode(&mut chunk).unwrap();
        // Text chunks after IDAT land in `info` once the stream is finished: insert ours
        // before the trailing IEND (a zero-length chunk is exactly 12 bytes).
        let split = base.len() - 12;
        let mut out = base[..split].to_vec();
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&base[split..]);
        out
    }
}
