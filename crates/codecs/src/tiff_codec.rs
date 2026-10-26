//! TIFF decode via the `tiff` crate: 1/2/4/8/16/32/64-bit integer, 16/32/64-bit float, gray/RGB/CMYK,
//! alpha (associated or not), ICC (34675), XMP (700), orientation (274). First image only. Palette
//! (indexed-colour) images are refused by the `tiff` crate itself, on every read path.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

const F: Format = Format::Tiff;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut d = Decoder::new(std::io::Cursor::new(bytes)).map_err(err)?.with_limits(limits(opts));
    let (w, h) = d.dimensions().map_err(err)?;
    check_size(F, w as u64, h as u64, opts)?;
    let ct = d.colortype().map_err(err)?;

    let icc = tag_bytes(&mut d, Tag::IccProfile);
    let xmp = tag_bytes(&mut d, Tag::from_u16_exhaustive(700)).map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string());
    let orientation = d.get_tag_u32(Tag::Orientation).ok().map(|v| v as u16);
    let planar = d.get_tag_u32(Tag::PlanarConfiguration).ok() == Some(2);
    let extra = d.get_tag_u16_vec(Tag::ExtraSamples).ok().unwrap_or_default();
    let colormap = d.get_tag_u16_vec(Tag::ColorMap).ok();
    let compression = d.get_tag_u32(Tag::Compression).ok();

    let (model, n_color, n_samples, bits) = layout(ct, compression, planar)?;
    let has_alpha = n_samples > n_color && !matches!(ct, ColorType::Palette(_));
    let premultiplied = has_alpha && extra.first() == Some(&1);

    let res = d.read_image().map_err(err)?;
    let (wu, hu) = (w as usize, h as usize);
    let n = wu * hu;
    let mut buf = match res {
        DecodingResult::U8(v) if bits < 8 => Buf::U8(unpack(&v, wu, hu, n_samples, bits)?),
        DecodingResult::U8(v) => Buf::U8(v),
        DecodingResult::U16(v) => Buf::U16(v),
        // Rare integer widths: keep 16 significant bits (plenty for display-referred data).
        DecodingResult::U32(v) => Buf::U16(v.into_iter().map(|x| (x >> 16) as u16).collect()),
        DecodingResult::U64(v) => Buf::U16(v.into_iter().map(|x| (x >> 48) as u16).collect()),
        DecodingResult::I8(v) => Buf::U8(v.into_iter().map(|x| (x.max(0) as u8) << 1).collect()),
        DecodingResult::I16(v) => Buf::U16(v.into_iter().map(|x| (x.max(0) as u16) << 1).collect()),
        DecodingResult::I32(v) => Buf::U16(v.into_iter().map(|x| (x.max(0) >> 15) as u16).collect()),
        DecodingResult::I64(v) => Buf::U16(v.into_iter().map(|x| (x.max(0) >> 47) as u16).collect()),
        DecodingResult::F16(v) => Buf::F32(v.into_iter().map(|x| x.to_f32()).collect()),
        DecodingResult::F32(v) => Buf::F32(v),
        DecodingResult::F64(v) => Buf::F32(v.into_iter().map(|x| x as f32).collect()),
    };
    let bit_depth = bits;

    if planar && n_samples > 1 {
        buf = interleave(buf, n, n_samples);
    }

    // Palette → 16-bit RGB.
    if let ColorType::Palette(b) = ct {
        let map = colormap.ok_or_else(|| err("palette image without ColorMap"))?;
        let entries = 1usize << b.min(16);
        if map.len() < 3 * entries {
            return Err(err("short ColorMap"));
        }
        let idx: Vec<usize> = match &buf {
            Buf::U8(v) => v.iter().map(|&i| i as usize).collect(),
            Buf::U16(v) => v.iter().map(|&i| i as usize).collect(),
            Buf::F32(_) => return Err(err("float palette")),
        };
        let mut out = Vec::with_capacity(n * 3);
        for &i in idx.iter().take(n) {
            let i = i.min(entries - 1);
            out.extend_from_slice(&[map[i], map[entries + i], map[2 * entries + i]]);
        }
        buf = Buf::U16(out);
        let raw = Raw { width: wu, height: hu, model: Model::Rgb, alpha: false, premultiplied: false, buf, bit_depth };
        let meta = Meta { icc, xmp, orientation, ..Default::default() };
        return finish(F, raw, meta, (w, h), opts);
    }

    // WhiteIsZero needs nothing here: the `tiff` crate inverts those samples as it reads them (and
    // refuses WhiteIsZero layouts it can't invert), so inverting again would show a negative.

    // Drop extra samples beyond colour + one alpha.
    let keep = n_color + has_alpha as usize;
    if n_samples != keep {
        buf = select_channels(buf, n, n_samples, keep);
    }
    let raw = Raw { width: wu, height: hu, model, alpha: has_alpha, premultiplied, buf, bit_depth };
    let meta = Meta { icc, xmp, orientation, ..Default::default() };
    finish(F, raw, meta, (w, h), opts)
}

/// The largest strip or tile [`decode_reduced`] reads (decoded bytes).
pub(crate) const MAX_REDUCED_CHUNK_BYTES: u64 = 64 << 20;
/// The most memory [`decode_reduced`]'s sums of the reduced image take.
pub(crate) const MAX_REDUCED_SUM_BYTES: usize = 1 << 30;

/// A TIFF decoded straight to (about) the size `opts` asks for, from a reader: strip by strip (or tile by tile), each
/// averaged into blocks of `k` × `k` pixels as it is read, so neither the file nor the full-size image is ever held
/// (a 2.3 GB layered Photoshop TIFF of 8640 × 5760 × 16 bits becomes a 540 × 360 picture in a few megabytes).
///
/// `Ok(None)`: not worth it or not covered here (no target size, an image less than twice the target, palette,
/// planar or sub-byte samples, an unusual sample type): decode it whole instead.
pub(crate) fn decode_reduced<R: std::io::Read + std::io::Seek>(reader: R, opts: &DecodeOptions) -> Result<Option<Decoded>> {
    let Some((mw, mh)) = opts.max_size.filter(|(w, h)| *w > 0 && *h > 0) else { return Ok(None) };
    let mut d = Decoder::new(reader).map_err(err)?.with_limits(limits(opts));
    let (w, h) = d.dimensions().map_err(err)?;
    check_size(F, w as u64, h as u64, opts)?;
    let ct = d.colortype().map_err(err)?;
    let planar = d.get_tag_u32(Tag::PlanarConfiguration).ok() == Some(2);
    let compression = d.get_tag_u32(Tag::Compression).ok();
    let (model, n_color, n_samples, bits) = layout(ct, compression, planar)?;
    if planar || bits < 8 || matches!(ct, ColorType::Palette(_)) {
        return Ok(None);
    }
    // whole blocks: the result still covers the box asked for
    let k = (w / mw).min(h / mh);
    if k < 2 {
        return Ok(None);
    }
    let icc = tag_bytes(&mut d, Tag::IccProfile);
    let xmp = tag_bytes(&mut d, Tag::from_u16_exhaustive(700)).map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string());
    let orientation = d.get_tag_u32(Tag::Orientation).ok().map(|v| v as u16);
    let extra = d.get_tag_u16_vec(Tag::ExtraSamples).ok().unwrap_or_default();
    let has_alpha = n_samples > n_color;
    let premultiplied = has_alpha && extra.first() == Some(&1);

    let (wu, hu, ku, s) = (w as usize, h as usize, k as usize, n_samples);
    let (ow, oh) = (wu.div_ceil(ku), hu.div_ceil(ku));
    // the sums of the reduced image: bounded whatever the sample count says (beyond it, the whole decode, with its
    // own limits, takes over)
    let cells = ow.checked_mul(oh).and_then(|n| n.checked_mul(s)).ok_or_else(|| err("image too large"))?;
    if cells > MAX_REDUCED_SUM_BYTES / std::mem::size_of::<f64>() {
        return Ok(None);
    }
    let (cw, ch) = match d.get_chunk_type() {
        tiff::decoder::ChunkType::Strip => {
            let rows = d.get_tag_u32(Tag::RowsPerStrip).unwrap_or(h).clamp(1, h);
            (w, rows)
        }
        tiff::decoder::ChunkType::Tile => {
            let tw = d.get_tag_u32(Tag::TileWidth).map_err(err)?;
            let th = d.get_tag_u32(Tag::TileLength).map_err(err)?;
            (tw, th)
        }
    };
    if cw == 0 || ch == 0 {
        return Err(err("empty strips or tiles"));
    }
    // One strip or tile is decoded at a time; one that is most of the image (no RowsPerStrip, or one strip for the
    // whole image, or huge tiles) would be the full-size decode this path exists to avoid: decode it whole instead.
    let chunk_bytes = u64::from(cw).saturating_mul(u64::from(ch)).saturating_mul(s as u64).saturating_mul(u64::from(bits.div_ceil(8)));
    if chunk_bytes > MAX_REDUCED_CHUNK_BYTES {
        return Ok(None);
    }
    let mut sum = vec![0f64; cells];
    let mut count = vec![0u32; ow * oh];
    let (across, down) = (w.div_ceil(cw) as usize, h.div_ceil(ch) as usize);
    let chunks = across.checked_mul(down).ok_or_else(|| err("too many tiles"))?;
    // the kind of samples, which every chunk must keep
    let mut kind: Option<u8> = None;
    for i in 0..chunks {
        let (x0, y0) = ((i % across) * cw as usize, (i / across) * ch as usize);
        // what is left of the image at this chunk (the last row and column of chunks are cut short)
        let (dw, dh) = ((cw as usize).min(wu - x0), (ch as usize).min(hu - y0));
        let index = u32::try_from(i).map_err(|_| err("too many tiles"))?;
        let chunk = d.read_chunk(index).map_err(err)?;
        let (this, len) = match &chunk {
            DecodingResult::U8(v) => (8u8, v.len()),
            DecodingResult::U16(v) => (16, v.len()),
            DecodingResult::F32(v) => (32, v.len()),
            _ => return Ok(None),
        };
        if *kind.get_or_insert(this) != this {
            return Err(err("sample type changes between strips"));
        }
        // a chunk's rows may be padded to the tile width
        let stride = len.checked_div(dh).unwrap_or(0);
        if stride < dw * s {
            return Err(err("short strip or tile"));
        }
        let block = Block { x0, y0, dw, dh, stride, s, k: ku, ow };
        match &chunk {
            DecodingResult::U8(v) => block.add(v, &mut sum, &mut count),
            DecodingResult::U16(v) => block.add(v, &mut sum, &mut count),
            DecodingResult::F32(v) => block.add(v, &mut sum, &mut count),
            _ => {}
        }
    }
    let mean = |i: usize| sum.get(i).copied().unwrap_or(0.0) / f64::from(count.get(i / s).copied().unwrap_or(1).max(1));
    let n = ow * oh;
    let mut buf = match kind {
        Some(8) => Buf::U8((0..n * s).map(|i| mean(i).round().clamp(0.0, 255.0) as u8).collect()),
        Some(16) => Buf::U16((0..n * s).map(|i| mean(i).round().clamp(0.0, 65535.0) as u16).collect()),
        Some(_) => Buf::F32((0..n * s).map(|i| mean(i) as f32).collect()),
        None => return Err(err("no image data")),
    };
    drop(sum);
    let keep = n_color + has_alpha as usize;
    if n_samples != keep {
        buf = select_channels(buf, n, n_samples, keep);
    }
    let raw = Raw { width: ow, height: oh, model, alpha: has_alpha, premultiplied, buf, bit_depth: bits };
    let meta = Meta { icc, xmp, orientation, ..Default::default() };
    finish(F, raw, meta, (w, h), opts).map(Some)
}

/// Where one strip or tile of [`decode_reduced`] lies, and how its samples are laid out.
struct Block {
    x0: usize,
    y0: usize,
    dw: usize,
    dh: usize,
    stride: usize,
    s: usize,
    k: usize,
    ow: usize,
}

impl Block {
    /// Add the chunk's samples to the sums of the output pixels they fall in.
    fn add<T: Copy + Into<f64>>(&self, v: &[T], sum: &mut [f64], count: &mut [u32]) {
        let s = self.s;
        for y in 0..self.dh {
            let oy = (self.y0 + y) / self.k;
            let Some(row) = v.get(y * self.stride..y * self.stride + self.dw * s) else { return };
            for (x, px) in row.chunks_exact(s).enumerate() {
                let o = oy * self.ow + (self.x0 + x) / self.k;
                if let (Some(c), Some(acc)) = (count.get_mut(o), sum.get_mut(o * s..o * s + s)) {
                    *c += 1;
                    for (a, x) in acc.iter_mut().zip(px) {
                        *a += (*x).into();
                    }
                }
            }
        }
    }
}

fn limits(opts: &DecodeOptions) -> Limits {
    let mut limits = Limits::default();
    limits.decoding_buffer_size = opts.max_pixels.saturating_mul(4 * 8).min(usize::MAX as u64) as usize;
    // A strip or tile whose declared byte count exceeds this is refused: the default 128 MiB refuses a
    // single-strip uncompressed 16-bit export of ~22 MP (#1983). Raised to the decoded image's cap
    // (32 bytes per allowed pixel), never lowered: a strip or tile can outgrow the pixels it shows
    // (JPEG headers, padding to the tile size).
    limits.intermediate_buffer_size = limits.intermediate_buffer_size.max(limits.decoding_buffer_size);
    limits.ifd_value_size = 32 << 20;
    limits
}

/// The sample layout of a colour type we can decode: (model, colour samples, samples per pixel,
/// bits per sample).
fn layout(ct: ColorType, compression: Option<u32>, planar: bool) -> Result<(Model, usize, usize, u8)> {
    let l = match ct {
        ColorType::Gray(b) => (Model::Gray, 1, 1, b),
        ColorType::GrayA(b) => (Model::Gray, 1, 2, b),
        ColorType::RGB(b) => (Model::Rgb, 3, 3, b),
        ColorType::RGBA(b) => (Model::Rgb, 3, 4, b),
        ColorType::CMYK(b) => (Model::Cmyk, 4, 4, b),
        ColorType::CMYKA(b) => (Model::Cmyk, 4, 5, b),
        ColorType::Palette(b) => (Model::Rgb, 1, 1, b),
        // JPEG-compressed YCbCr is converted to RGB by the JPEG decoder.
        ColorType::YCbCr(b) if compression == Some(7) => (Model::Rgb, 3, 3, b),
        ColorType::Multiband { bit_depth, num_samples } if num_samples >= 3 => (Model::Rgb, 3, num_samples as usize, bit_depth),
        ColorType::Multiband { bit_depth, num_samples } if num_samples >= 1 => (Model::Gray, 1, num_samples as usize, bit_depth),
        other => return Err(Error::Unsupported(F, unsupported_name(other))),
    };
    if l.3 < 8 && planar && l.2 > 1 {
        return Err(Error::Unsupported(F, "planar TIFF with samples narrower than a byte"));
    }
    Ok(l)
}

/// Stored dimensions and orientation from the first IFD, without reading the image data. Refused,
/// as a decode would refuse it: a layout or compression we can't decode, or strips/tiles that
/// start past the end of the file (truncated). The compressed data itself is not checked.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let mut d = Decoder::new(std::io::Cursor::new(bytes)).map_err(err)?.with_limits(limits(&DecodeOptions::default()));
    let (w, h) = d.dimensions().map_err(err)?;
    check_size(F, w as u64, h as u64, &DecodeOptions::default())?;
    let ct = d.colortype().map_err(err)?;
    let compression = d.get_tag_u32(Tag::Compression).ok();
    let planar = d.get_tag_u32(Tag::PlanarConfiguration).ok() == Some(2);
    layout(ct, compression, planar)?;
    // what this build of the `tiff` crate decompresses: none, CCITT G4, LZW, JPEG, Deflate, PackBits
    if !matches!(compression.unwrap_or(1), 1 | 4 | 5 | 7 | 8 | 32773 | 32946) {
        return Err(Error::Unsupported(F, "compression method"));
    }
    let chunks = |offsets: Tag, counts: Tag, d: &mut Decoder<std::io::Cursor<&[u8]>>| {
        Some((d.get_tag_u64_vec(offsets).ok()?, d.get_tag_u64_vec(counts).ok()?))
    };
    let (offsets, counts) = chunks(Tag::StripOffsets, Tag::StripByteCounts, &mut d)
        .or_else(|| chunks(Tag::TileOffsets, Tag::TileByteCounts, &mut d))
        .ok_or_else(|| err("no image data"))?;
    // Only where each chunk starts: some writers overstate StripByteCounts, and the `tiff` crate
    // decodes such files (it stops once it has the pixels), so a chunk's end proves nothing.
    let len = bytes.len() as u64;
    if offsets.is_empty() || offsets.iter().zip(&counts).any(|(&o, &c)| c > 0 && o >= len) {
        return Err(err("truncated: image data past the end of the file"));
    }
    let orientation = d.get_tag_u32(Tag::Orientation).ok().map(|v| v as u16).filter(|o| (1..=8).contains(o)).unwrap_or(1);
    Ok((w, h, orientation))
}

/// A BYTE/UNDEFINED tag as bytes (the decoder may surface unknown byte tags as unsigned values).
fn tag_bytes<R: std::io::Read + std::io::Seek>(d: &mut Decoder<R>, tag: Tag) -> Option<Vec<u8>> {
    use tiff::decoder::ifd::Value;
    fn byte(v: &Value) -> Option<u8> {
        match v {
            Value::Byte(b) => Some(*b),
            Value::Unsigned(u) => u8::try_from(*u).ok(),
            Value::Short(u) => u8::try_from(*u).ok(),
            Value::SignedByte(b) => Some(*b as u8),
            _ => None,
        }
    }
    match d.find_tag(tag).ok()?? {
        Value::List(v) => v.iter().map(byte).collect(),
        Value::Ascii(s) => Some(s.into_bytes()),
        v => byte(&v).map(|b| vec![b]),
    }
}

fn unsupported_name(ct: ColorType) -> &'static str {
    match ct {
        ColorType::Lab(_) => "CIE L*a*b* TIFF",
        ColorType::YCbCr(_) => "uncompressed YCbCr TIFF",
        _ => "colour type",
    }
}

/// Samples narrower than a byte (1-bit bilevel and fax scans, 2- and 4-bit gray) arrive packed, most
/// significant bits first, each row padded to a whole byte. Spread them out one per byte, scaled to
/// 0–255 like any 8-bit sample.
fn unpack(packed: &[u8], width: usize, height: usize, samples: usize, bits: u8) -> Result<Vec<u8>> {
    if !matches!(bits, 1 | 2 | 4) {
        return Err(Error::Unsupported(F, "TIFF samples of 3, 5, 6 or 7 bits"));
    }
    let bits = bits as usize;
    let per_row = width.checked_mul(samples).ok_or_else(|| err("row too wide"))?;
    let row_bytes = per_row.checked_mul(bits).ok_or_else(|| err("row too wide"))?.div_ceil(8);
    if row_bytes == 0 || packed.len() / row_bytes < height {
        return Err(err("sample buffer too short"));
    }
    let max = (1u8 << bits) - 1;
    let scale = 255 / max; // 255, 85, 17: exact
    let mut out = Vec::with_capacity(per_row.saturating_mul(height));
    for row in packed.chunks_exact(row_bytes).take(height) {
        let values = row.iter().flat_map(|&b| (1..=8 / bits).map(move |i| (b >> (8 - bits * i)) & max));
        out.extend(values.take(per_row).map(|v| v * scale));
    }
    Ok(out)
}

fn interleave(buf: Buf, n: usize, s: usize) -> Buf {
    fn go<T: Copy + Default>(v: Vec<T>, n: usize, s: usize) -> Vec<T> {
        if v.len() < n * s {
            return v;
        }
        let mut out = vec![T::default(); n * s];
        for c in 0..s {
            for i in 0..n {
                out[i * s + c] = v[c * n + i];
            }
        }
        out
    }
    match buf {
        Buf::U8(v) => Buf::U8(go(v, n, s)),
        Buf::U16(v) => Buf::U16(go(v, n, s)),
        Buf::F32(v) => Buf::F32(go(v, n, s)),
    }
}

fn select_channels(buf: Buf, n: usize, s: usize, keep: usize) -> Buf {
    fn go<T: Copy>(v: Vec<T>, n: usize, s: usize, keep: usize) -> Vec<T> {
        if v.len() < n * s {
            return v;
        }
        let mut out = Vec::with_capacity(n * keep);
        for px in v.chunks_exact(s).take(n) {
            out.extend_from_slice(&px[..keep]);
        }
        out
    }
    match buf {
        Buf::U8(v) => Buf::U8(go(v, n, s, keep)),
        Buf::U16(v) => Buf::U16(go(v, n, s, keep)),
        Buf::F32(v) => Buf::F32(go(v, n, s, keep)),
    }
}
