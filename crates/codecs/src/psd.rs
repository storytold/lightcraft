//! Photoshop PSD/PSB merged-composite reader (clean-room, from the public file-format description):
//! 8/16/32-bit gray, RGB, CMYK, indexed, duotone (as gray); raw or PackBits/RLE image data;
//! ICC (resource 1039), EXIF (1058), XMP (1060); transparency when the layer count is negative.
//! Also a flat-file writer ([`encode`]): the merged image of an 8- or 16-bit gray/RGB document
//! with no layers, so an image editor opens it as one background layer and saves back to it.

use crate::convert::{Buf, Meta, Model, Raw, finish};
use crate::encode::{EncodeImage, EncodeMeta, Samples};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Psd;

fn err(s: &str) -> Error {
    Error::Malformed(F, s.to_string())
}

#[cfg(test)]
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

#[cfg(test)]
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| err("truncated"))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}

/// The merged composite of `bytes`. A file whose image resources section doesn't parse is opened
/// with that section emptied (a copy in `spare`): the picture still decodes, without its profile
/// and metadata.
fn open_merged<'a>(bytes: &'a [u8], opts: &DecodeOptions, spare: &'a mut Vec<u8>) -> Result<craft_psd::merged::Merged<'a>> {
    let error = match open_exact(bytes, opts) {
        Ok(merged) => return Ok(merged),
        // a pixel-limit refusal reads the header, which the copy keeps: no retry
        Err(error @ Error::TooLarge(..)) => return Err(error),
        Err(error) => error,
    };
    let Some(stripped) = without_resources(bytes) else { return Err(error) };
    *spare = stripped;
    let spare: &'a Vec<u8> = spare;
    open_exact(spare, opts).map_err(|_| error)
}

/// `bytes` with an empty image resources section, when the header parses but the resources don't.
fn without_resources(bytes: &[u8]) -> Option<Vec<u8>> {
    if craft_psd::Head::from_bytes(bytes).is_ok() || craft_psd::Header::from_bytes(bytes).is_err() {
        return None;
    }
    let len_at = |at: usize| -> Option<usize> { usize::try_from(u32::from_be_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?)).ok() };
    let res_at = 30usize.checked_add(len_at(26)?)?;
    let tail = bytes.get(res_at.checked_add(4)?.checked_add(len_at(res_at)?)?..)?;
    let mut out = Vec::new();
    // a failed allocation leaves the original parse error, not an abort
    out.try_reserve_exact(bytes.len()).ok()?;
    out.extend_from_slice(bytes.get(..res_at)?);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(tail);
    Some(out)
}

fn open_exact<'a>(bytes: &'a [u8], opts: &DecodeOptions) -> Result<craft_psd::merged::Merged<'a>> {
    craft_psd::merged::Merged::open(bytes, opts.max_pixels).map_err(|error| match error {
        craft_psd::PsdError::LimitExceeded("pixel count") => {
            let (w, h) = craft_psd::merged::declared_dimensions(bytes).unwrap_or((0, 0));
            Error::TooLarge(u64::from(w), u64::from(h))
        }
        craft_psd::PsdError::Invalid(message) => err(&message),
        craft_psd::PsdError::UnexpectedEof { .. } => err("truncated"),
        other => err(&other.to_string()),
    })
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut spare = Vec::new();
    let merged = open_merged(bytes, opts, &mut spare)?;
    let head = &merged.head;
    let h = &head.header;
    let (channels, width, height, depth, mode) = (usize::from(h.channels), h.width as usize, h.height as usize, h.depth, h.color_mode.as_u16());
    let color_mode_data = head.color_mode_data.as_slice();
    let (icc, exif, xmp) = resources(&head.resources);
    let merged_transparency = merged.transparency;

    // Image data.
    let bpc = match depth {
        8 => 1usize,
        16 => 2,
        32 => 4,
        1 => return Err(Error::Unsupported(F, "1-bit bitmap PSD")),
        _ => return Err(err("bad depth")),
    };
    let (model, n_color) = match mode {
        1 | 8 => (Model::Gray, 1),
        2 => (Model::Rgb, 1),
        3 => (Model::Rgb, 3),
        4 => (Model::Cmyk, 4),
        7 => (Model::Gray, 1),
        9 => return Err(Error::Unsupported(F, "Lab PSD")),
        _ => return Err(err("unknown colour mode")),
    };
    if channels < n_color {
        return Err(err("too few channels"));
    }
    let alpha = merged_transparency && channels > n_color && mode != 2;
    // Only the colour (+ alpha) planes are decoded; extra channels (spot colours, masks) are
    // stored after them and never touched.
    let used = n_color + alpha as usize;
    let too_large = || Error::TooLarge(width as u64, height as u64);
    let n = width.checked_mul(height).ok_or_else(too_large)?;
    let planes = merged.read_planes(used).map_err(|error| match error {
        craft_psd::PsdError::LimitExceeded(_) | craft_psd::PsdError::OutOfMemory(_) => too_large(),
        craft_psd::PsdError::Unsupported(_) => Error::Unsupported(F, "ZIP-compressed merged image data"),
        craft_psd::PsdError::Invalid(message) => err(&message),
        craft_psd::PsdError::UnexpectedEof { .. } => err("truncated"),
        other => err(&other.to_string()),
    })?;
    let samples = n.checked_mul(used).ok_or_else(too_large)?;

    // Interleave to device samples.
    let buf = if mode == 2 {
        // Indexed → 8-bit RGB via the 768-byte palette.
        let (Some(pal), true) = (color_mode_data.get(..768), depth == 8) else {
            return Err(err("bad indexed palette"));
        };
        let idx = planes.first().ok_or_else(|| err("missing plane"))?;
        let mut v = zeroed::<u8>(n.checked_mul(3).ok_or_else(too_large)?, too_large)?;
        for (o, &i) in v.as_chunks_mut::<3>().0.iter_mut().zip(idx) {
            let i = i as usize;
            *o = [pal[i], pal[256 + i], pal[512 + i]];
        }
        Buf::U8(v)
    } else {
        let cmyk = model == Model::Cmyk;
        match bpc {
            1 => {
                let mut v = zeroed::<u8>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, &x) in v.iter_mut().skip(c).step_by(used).zip(p) {
                        // PSD CMYK stores 255 = no ink; our CMYK convention is 0 = no ink.
                        *o = if cmyk && c < 4 { 255 - x } else { x };
                    }
                }
                Buf::U8(v)
            }
            2 => {
                let mut v = zeroed::<u16>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, s) in v.iter_mut().skip(c).step_by(used).zip(p.as_chunks::<2>().0) {
                        let x = u16::from_be_bytes(*s);
                        *o = if cmyk && c < 4 { 65535 - x } else { x };
                    }
                }
                Buf::U16(v)
            }
            _ => {
                let mut v = zeroed::<f32>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, s) in v.iter_mut().skip(c).step_by(used).zip(p.as_chunks::<4>().0) {
                        let x = f32::from_be_bytes(*s);
                        *o = if cmyk && c < 4 { 1.0 - x } else { x };
                    }
                }
                Buf::F32(v)
            }
        }
    };
    let raw = Raw { width, height, model, alpha, premultiplied: false, buf, bit_depth: depth as u8 };
    finish(F, raw, Meta { icc, exif, xmp, ..Default::default() }, (width as u32, height as u32), opts)
}

/// The largest side a PSD (version 1) may have; larger documents need PSB.
pub const MAX_SIDE: u32 = 30_000;

/// Write a flat PSD: header, the image resources for `meta` (resolution 1005, ICC 1039, EXIF
/// 1058, XMP 1060), an empty layer section and the merged image. 8-bit planes are PackBits
/// compressed, 16-bit ones stored raw (PackBits saves little on 16-bit photos). Gray or RGB;
/// alpha is dropped (the render is opaque). Float samples are refused.
pub(crate) fn encode(img: &EncodeImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let (w, h) = (img.width, img.height);
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err(Error::Encode(format!("{w} × {h} is larger than a PSD can hold ({MAX_SIDE} px a side): use TIFF")));
    }
    let samples = match img.samples {
        Samples::U8(s) => craft_psd::flat::Samples::U8(s),
        Samples::U16(s) => craft_psd::flat::Samples::U16(s),
        Samples::F32(_) => return Err(Error::Encode("32-bit float PSD is not supported: use TIFF".into())),
    };
    let metadata = craft_psd::flat::Metadata { icc: meta.icc, exif: meta.exif, xmp: meta.xmp, ppi: meta.ppi };
    craft_psd::flat::write(w, h, img.channels, samples, metadata).map_err(|error| Error::Encode(error.to_string()))
}

/// Stored dimensions and EXIF orientation (resource 1058) from the header and image resources,
/// without reading the merged image data. Refused, as a decode would refuse it: an unsupported
/// depth, colour mode or compression, or merged image data that runs past the end of the file
/// (truncated). The run-length data itself is not unpacked.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let mut spare = Vec::new();
    let merged = open_merged(bytes, &DecodeOptions::default(), &mut spare)?;
    let head = &merged.head;
    let h = &head.header;
    let (channels, width, height, depth, mode) = (usize::from(h.channels), h.width as usize, h.height as usize, h.depth, h.color_mode.as_u16());
    let color_mode_data = head.color_mode_data.as_slice();
    let (_, exif, _) = resources(&head.resources);
    match depth {
        8 | 16 | 32 => {}
        1 => return Err(Error::Unsupported(F, "1-bit bitmap PSD")),
        _ => return Err(err("bad depth")),
    }
    let n_color = match mode {
        1 | 8 | 7 | 2 => 1,
        3 => 3,
        4 => 4,
        9 => return Err(Error::Unsupported(F, "Lab PSD")),
        _ => return Err(err("unknown colour mode")),
    };
    if channels < n_color {
        return Err(err("too few channels"));
    }
    if mode == 2 && (color_mode_data.len() < 768 || depth != 8) {
        return Err(err("bad indexed palette"));
    }
    merged.validate_all().map_err(|error| match error {
        craft_psd::PsdError::Unsupported(_) => Error::Unsupported(F, "ZIP-compressed merged image data"),
        craft_psd::PsdError::Invalid(message) => err(&message),
        craft_psd::PsdError::UnexpectedEof { .. } => err("truncated"),
        other => err(&other.to_string()),
    })?;
    let orientation = exif.as_deref().map(crate::exif::summarize).unwrap_or_default().orientation.unwrap_or(1);
    Ok((width as u32, height as u32, orientation))
}

/// A zero-filled buffer whose allocation failure is an error, not an abort.
fn zeroed<T: Clone + Default>(len: usize, too_large: impl Fn() -> Error) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| too_large())?;
    v.resize(len, T::default());
    Ok(v)
}

#[cfg(test)]
fn unpackbits(src: &[u8], row: &mut [u8]) -> Result<()> {
    craft_psd::merged::unpackbits(src, row).map_err(|error| err(&error.to_string()))
}

/// ICC (1039), EXIF (1058, without its optional `Exif\0\0` prefix) and XMP (1060) resources; the last of each id.
fn resources(res: &[craft_psd::ImageResource]) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<String>) {
    let last = |id: u16| res.iter().rev().find(|r| r.id == id).map(|r| r.data.as_slice());
    let icc = last(1039).map(<[u8]>::to_vec);
    let exif = last(1058).map(|d| d.strip_prefix(b"Exif\0\0").unwrap_or(d).to_vec());
    let xmp = last(1060).map(|d| String::from_utf8_lossy(d).trim_end_matches('\0').to_string());
    (icc, exif, xmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits() {
        let mut out = [0u8; 6];
        unpackbits(&[0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A], &mut out).unwrap();
        assert_eq!(out, [0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A]);
        // Short run data is zero-padded.
        let mut out = [0u8; 4];
        unpackbits(&[0x01, 1], &mut out).unwrap();
        assert_eq!(out, [1, 0, 0, 0]);
        // Truncated literal run: copies what is there.
        let mut out = [0u8; 4];
        unpackbits(&[0x03, 7, 8], &mut out).unwrap();
        assert_eq!(out, [7, 8, 0, 0]);
    }

    #[test]
    fn packbits_overrun_is_an_error() {
        // Repeat run of 128 into a 4-byte row.
        let mut out = [0u8; 4];
        assert!(unpackbits(&[0x81, 0xAA], &mut out).is_err());
        // Literal run of 6 into a 4-byte row.
        assert!(unpackbits(&[0x05, 1, 2, 3, 4, 5, 6], &mut out).is_err());
    }

    /// PSD header + empty colour-mode/resources/layer sections + compression id.
    fn header(channels: u16, w: u32, h: u32, depth: u16, mode: u16, compression: u16) -> Vec<u8> {
        let mut b = b"8BPS".to_vec();
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&channels.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&depth.to_be_bytes());
        b.extend_from_slice(&mode.to_be_bytes());
        b.extend_from_slice(&[0; 12]); // colour mode data, resources, layer & mask: all empty
        b.extend_from_slice(&compression.to_be_bytes());
        b
    }

    #[test]
    fn huge_rle_with_truncated_count_table_is_rejected_before_allocating() {
        // 1 × 300 000 px, 56 channels: the row table alone would be 16.8 M entries; the file has 4 bytes.
        let mut b = header(56, 1, 300_000, 32, 3, 1);
        b.extend_from_slice(&[0, 0, 0, 0]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m == "truncated"), "{e:?}");
    }

    #[test]
    fn huge_rle_of_empty_rows_is_rejected_before_allocating() {
        // 300 000 × 1 px RGB at 32 bits: every row count is 0, which used to zero-pad 3 × 1.2 MB.
        let mut b = header(3, 300_000, 1, 32, 3, 1);
        b.extend_from_slice(&[0; 6]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m.contains("too short")), "{e:?}");
        // 0.27 Gpx RGB 8-bit whose (plausible) row counts claim more data than the file holds.
        let mut b = header(3, 1 << 18, 1 << 10, 8, 3, 1);
        b.extend_from_slice(&[0xFF; 3 * 1024 * 2]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m == "truncated"), "{e:?}");
    }

    #[test]
    fn a_damaged_resource_section_is_left_out() {
        // a resource section of 4 bytes that is not an `8BIM` record: the picture decodes without it
        let mut b = header(3, 1, 1, 8, 3, 0);
        b[30..34].copy_from_slice(&4u32.to_be_bytes());
        b.splice(34..34, *b"JUNK");
        b.extend_from_slice(&[1, 2, 3]);
        let d = decode(&b, &DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height, d.icc.is_none()), (1, 1, true));
        assert_eq!(super::header(&b).unwrap(), (1, 1, 1));
    }

    #[test]
    fn a_damaged_resource_section_over_the_pixel_limit_is_too_large() {
        // the size refusal comes before (and instead of) the copy without the resources
        let mut b = header(3, 4, 4, 8, 3, 0);
        b[30..34].copy_from_slice(&4u32.to_be_bytes());
        b.splice(34..34, *b"JUNK");
        let opts = DecodeOptions { max_pixels: 15, ..DecodeOptions::default() };
        let e = decode(&b, &opts).unwrap_err();
        assert!(matches!(e, Error::TooLarge(4, 4)), "{e:?}");
    }

    #[test]
    fn rle_repeat_run_past_row_width_is_an_error() {
        // 4 × 1 gray 8-bit; the single row is a 128-byte repeat run.
        let mut b = header(1, 4, 1, 8, 1, 1);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(&[0x81, 0x55]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m.contains("overruns")), "{e:?}");
    }

    #[test]
    fn rle_gray16_and_extra_channels_decode() {
        // 2 × 2 gray 16-bit with an extra (unused) channel whose data is absent: only the used
        // plane is decoded. Row: repeat run of 4 bytes 0x80 -> samples 0x8080.
        let mut b = header(2, 2, 2, 16, 1, 1);
        for _ in 0..4 {
            b.extend_from_slice(&2u16.to_be_bytes());
        }
        b.extend_from_slice(&[0xFD, 0x80, 0xFD, 0x80]);
        let d = decode(&b, &DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (2, 2));
        let p = d.image.get(1, 1);
        let q = d.image.get(0, 0);
        assert!(p[0] > 0.0 && (p[0] - q[0]).abs() < 1e-6, "{p:?}");
    }
    #[test]
    fn packbits_round_trips() {
        let rows: [&[u8]; 6] = [&[], &[7], &[1, 1], &[1, 2, 3, 3, 3, 4, 5, 5], &[9; 300], &(0..=255u8).cycle().take(700).collect::<Vec<_>>()];
        for row in rows {
            let mut enc = Vec::new();
            craft_psd::compression::packbits::encode(row, &mut enc);
            let mut out = vec![0u8; row.len()];
            unpackbits(&enc, &mut out).unwrap();
            assert_eq!(out, row);
        }
    }

    #[test]
    fn flat_psd_round_trips_8_and_16_bit() {
        let icc = crate::icc::write_named(crate::NamedSpace::AdobeRgb);
        let meta = EncodeMeta { icc: Some(&icc), xmp: Some("<x:xmpmeta/>"), ppi: Some(300), ..Default::default() };
        // 8-bit RGBA (alpha dropped), 5 × 3 with a flat run and a gradient
        let px: Vec<u8> = (0..15u8).flat_map(|i| [i * 10, if i < 8 { 200 } else { i }, 255 - i, 255]).collect();
        let b = encode(&EncodeImage::new(5, 3, 4, Samples::U8(&px)), &meta).unwrap();
        assert_eq!(super::header(&b).unwrap(), (5, 3, 1));
        let d = decode(&b, &DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (5, 3));
        assert_eq!(d.icc.as_deref(), Some(&icc[..]), "the ICC profile is kept");
        assert_eq!(d.xmp.as_deref(), Some("<x:xmpmeta/>"));
        // 16-bit RGB, raw planes: the samples come back exactly
        let px16: Vec<u16> = (0..4 * 2 * 3).map(|i| (i as u16) * 2731).collect();
        let b = encode(&EncodeImage::new(4, 2, 3, Samples::U16(&px16)), &meta).unwrap();
        assert_eq!(&b[22..24], &16u16.to_be_bytes());
        let mut r = Rd { b: &b, p: 26 };
        let cm = r.u32().unwrap() as usize;
        r.take(cm).unwrap();
        let res = r.u32().unwrap() as usize;
        r.take(res).unwrap();
        assert_eq!(r.u32().unwrap(), 0, "no layers");
        assert_eq!(r.u16().unwrap(), 0, "16-bit planes are raw");
        let red: Vec<u16> = (0..8).map(|_| r.u16().unwrap()).collect();
        assert_eq!(red, px16.iter().step_by(3).copied().collect::<Vec<_>>());
        assert!(decode(&b, &DecodeOptions::default()).is_ok());
        // gray 16-bit
        let g = encode(&EncodeImage::new(2, 2, 1, Samples::U16(&[0, 1000, 40000, 65535])), &EncodeMeta::default()).unwrap();
        assert_eq!((&g[12..14], &g[24..26]), (&1u16.to_be_bytes()[..], &1u16.to_be_bytes()[..]), "one channel, grayscale mode");
        assert!(decode(&g, &DecodeOptions::default()).is_ok());
    }

    #[test]
    fn flat_psd_refuses_what_it_cannot_hold() {
        let e = encode(&EncodeImage::new(1, 1, 3, Samples::F32(&[0.5; 3])), &EncodeMeta::default()).unwrap_err();
        assert!(e.to_string().contains("float"), "{e}");
        let e = encode(&EncodeImage::new(MAX_SIDE + 1, 1, 1, Samples::U8(&vec![0; MAX_SIDE as usize + 1])), &EncodeMeta::default()).unwrap_err();
        assert!(e.to_string().contains("TIFF"), "{e}");
    }
}
