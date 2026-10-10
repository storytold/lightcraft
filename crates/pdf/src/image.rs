//! Images for PDF pages: JPEG passthrough (DCTDecode, the file's bytes unchanged) and raw 8- or
//! 16-bit samples compressed with Flate, optional 8-bit alpha as a soft mask.

use std::sync::Arc;

use crate::{ColorSpace, PdfError};

/// Pixel data of an [`Image`].
#[derive(Clone, Debug, PartialEq)]
pub enum ImageData {
    /// A baseline or progressive JPEG file, embedded unchanged. Its size and component count are
    /// read from the file's frame header.
    Jpeg(Arc<Vec<u8>>),
    /// Interleaved samples, row-major, no padding: `bits` is 8 or 16 (16-bit samples big-endian,
    /// as PDF stores them).
    Samples { width: u32, height: u32, bits: u8, data: Vec<u8> },
}

/// An image to place on pages (added once with [`crate::Document::add_image`], drawn any number of
/// times).
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub data: ImageData,
    /// Colour space of the samples. Its component count must match the data.
    pub color: ColorSpace,
    /// Optional 8-bit alpha (`width * height` bytes), written as a soft mask.
    pub alpha: Option<Vec<u8>>,
    /// Ask viewers to smooth the image when it is scaled up.
    pub interpolate: bool,
}

impl Image {
    /// A JPEG file in `color` (use [`ColorSpace::Icc`] with the file's embedded profile to keep it
    /// colour-managed).
    pub fn jpeg(bytes: impl Into<Arc<Vec<u8>>>, color: ColorSpace) -> Self {
        Self { data: ImageData::Jpeg(bytes.into()), color, alpha: None, interpolate: false }
    }

    /// 8-bit samples.
    pub fn samples8(width: u32, height: u32, color: ColorSpace, data: Vec<u8>) -> Self {
        Self { data: ImageData::Samples { width, height, bits: 8, data }, color, alpha: None, interpolate: false }
    }

    /// 16-bit samples (native `u16` values, written big-endian).
    pub fn samples16(width: u32, height: u32, color: ColorSpace, data: &[u16]) -> Self {
        let bytes = data.iter().flat_map(|v| v.to_be_bytes()).collect();
        Self { data: ImageData::Samples { width, height, bits: 16, data: bytes }, color, alpha: None, interpolate: false }
    }

    /// Size in pixels.
    pub fn size(&self) -> Result<(u32, u32), PdfError> {
        match &self.data {
            ImageData::Jpeg(b) => jpeg_info(b).map(|i| (i.width, i.height)),
            ImageData::Samples { width, height, .. } => Ok((*width, *height)),
        }
    }

    /// Checks sizes, depth and component counts; returns what the writer needs.
    pub(crate) fn validate(&self) -> Result<Checked, PdfError> {
        let n = self.color.components();
        // an ICC profile has 1 (gray), 3 (RGB/Lab) or 4 (CMYK) channels: anything else can't be
        // written (and the PDF writer asserts on it)
        if !matches!(n, 1 | 3 | 4) {
            return Err(PdfError::Image(format!("an ICC colour space with {n} components (1, 3 or 4 supported)")));
        }
        let (width, height, bits, adobe_inverted) = match &self.data {
            ImageData::Jpeg(b) => {
                let info = jpeg_info(b)?;
                if info.components != n {
                    return Err(PdfError::Image(format!("JPEG has {} components, colour space has {n}", info.components)));
                }
                (info.width, info.height, 8, info.adobe && n == 4)
            }
            ImageData::Samples { width, height, bits, data } => {
                if *bits != 8 && *bits != 16 {
                    return Err(PdfError::Image(format!("{bits}-bit samples (8 or 16 supported)")));
                }
                let want = (*width as u64).saturating_mul(*height as u64).saturating_mul(n as u64).saturating_mul(u64::from(*bits / 8));
                if data.len() as u64 != want {
                    return Err(PdfError::Image(format!("{width}x{height}x{n} at {bits} bits needs {want} bytes, got {}", data.len())));
                }
                (*width, *height, *bits, false)
            }
        };
        if width == 0 || height == 0 {
            return Err(PdfError::Image("empty image".into()));
        }
        if let Some(a) = &self.alpha
            && a.len() as u64 != (width as u64).saturating_mul(height as u64)
        {
            return Err(PdfError::Image(format!("alpha has {} bytes for {width}x{height}", a.len())));
        }
        Ok(Checked { width, height, bits, adobe_inverted })
    }
}

pub(crate) struct Checked {
    pub width: u32,
    pub height: u32,
    pub bits: u8,
    /// Adobe-style CMYK JPEG (APP14): stored inverted, needs a `/Decode [1 0 …]`.
    pub adobe_inverted: bool,
}

/// What a JPEG frame header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    pub components: usize,
    /// An Adobe APP14 marker is present (CMYK data is then stored inverted).
    pub adobe: bool,
}

/// Reads size and component count from a JPEG's SOFn marker.
pub fn jpeg_info(b: &[u8]) -> Result<JpegInfo, PdfError> {
    let bad = |m: &str| PdfError::Image(format!("not a usable JPEG: {m}"));
    if b.get(0..2) != Some(&[0xFF, 0xD8]) {
        return Err(bad("no SOI marker"));
    }
    let mut i = 2usize;
    let mut adobe = false;
    loop {
        // Skip fill bytes.
        while b.get(i) == Some(&0xFF) && b.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        let (Some(&0xFF), Some(&marker)) = (b.get(i), b.get(i + 1)) else {
            return Err(bad("no frame header"));
        };
        i += 2;
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return Err(bad("no frame header before the scan"));
        }
        let len = match b.get(i..i + 2) {
            Some(&[h, l]) => usize::from(u16::from_be_bytes([h, l])),
            _ => return Err(bad("truncated segment")),
        };
        if len < 2 {
            return Err(bad("bad segment length"));
        }
        let seg = b.get(i + 2..i + len).ok_or_else(|| bad("truncated segment"))?;
        if marker == 0xEE && seg.starts_with(b"Adobe") {
            adobe = true;
        }
        // SOF0..SOF15 except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let (Some(&p), Some(h), Some(w), Some(&n)) = (seg.first(), seg.get(1..3), seg.get(3..5), seg.get(5)) else {
                return Err(bad("short frame header"));
            };
            if p != 8 {
                return Err(bad(&format!("{p}-bit samples")));
            }
            let height = u32::from(u16::from_be_bytes([h[0], h[1]]));
            let width = u32::from(u16::from_be_bytes([w[0], w[1]]));
            if width == 0 || height == 0 || !matches!(n, 1 | 3 | 4) {
                return Err(bad("unsupported frame"));
            }
            return Ok(JpegInfo { width, height, components: usize::from(n), adobe });
        }
        i += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal JPEG header: SOI, APP14 Adobe (optional), SOF0.
    pub(crate) fn fake_jpeg(w: u16, h: u16, n: u8, adobe: bool) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        if adobe {
            v.extend([0xFF, 0xEE, 0x00, 0x0E]);
            v.extend(b"Adobe");
            v.extend([0, 100, 0, 0, 0, 0, 2]);
        }
        v.extend([0xFF, 0xC0, 0x00, 8 + 3 * n, 8]);
        v.extend(h.to_be_bytes());
        v.extend(w.to_be_bytes());
        v.push(n);
        for c in 0..n {
            v.extend([c + 1, 0x11, 0]);
        }
        v.extend([0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_headers() {
        assert_eq!(jpeg_info(&fake_jpeg(640, 480, 3, false)).unwrap(), JpegInfo { width: 640, height: 480, components: 3, adobe: false });
        assert!(jpeg_info(&fake_jpeg(10, 10, 4, true)).unwrap().adobe);
        // Hostile input never panics.
        for bad in [&b""[..], b"\xFF\xD8", b"\xFF\xD8\xFF\xC0\x00", b"\xFF\xD8\xFF\xC0\x00\x01", b"\xFF\xD8\xFF\xDA", b"\x89PNG"] {
            assert!(jpeg_info(bad).is_err());
        }
        let mut zero = fake_jpeg(0, 10, 3, false);
        assert!(jpeg_info(&zero).is_err());
        zero = fake_jpeg(10, 10, 2, false);
        assert!(jpeg_info(&zero).is_err());
    }

    #[test]
    fn sample_sizes_are_checked() {
        assert!(Image::samples8(2, 2, ColorSpace::Rgb, vec![0; 12]).validate().is_ok());
        assert!(Image::samples8(2, 2, ColorSpace::Rgb, vec![0; 11]).validate().is_err());
        assert!(Image::samples16(2, 1, ColorSpace::Gray, &[1, 2]).validate().is_ok());
        assert!(Image::samples8(0, 0, ColorSpace::Gray, vec![]).validate().is_err());
        let mut a = Image::samples8(2, 2, ColorSpace::Gray, vec![0; 4]);
        a.alpha = Some(vec![0; 3]);
        assert!(a.validate().is_err());
        assert!(Image::jpeg(fake_jpeg(4, 4, 3, false), ColorSpace::Gray).validate().is_err());
        assert!(Image::samples8(u32::MAX, u32::MAX, ColorSpace::Cmyk, vec![]).validate().is_err());
    }
}
