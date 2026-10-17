//! The composed print pages as the raster documents a printer takes.
//!
//! A [`Sheet`] is one page of the print as 8-bit sRGB with its paper size. [`SheetConverter`] is a
//! craft-print [`DocumentConverter`] that carries the sheets of one print: asked to turn the print's PDF into a
//! format the printer lists, it ignores the PDF's bytes (they are the same pages, already composed) and
//! renders PWG raster, or a JPEG for a one-page print, from the sheets at the resolution the registry chose.
//! It checks that the document it is handed is the print's own (its length), so it can never send the
//! pixels of one print for the document of another.

use std::sync::Arc;

use craft_print::pwg;
use craft_print::{ConvertContext, Document, DocumentConverter, DocumentFormat, Duplex, ErrorKind, OwnedDocument, PrintError};

/// Most pixels a converted page may have (A3 at 600 dpi is about 35 million).
pub const MAX_PAGE_PIXELS: u64 = 120_000_000;

/// One page of a print: 8-bit sRGB, row-major, three bytes a pixel, with the paper it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    /// The paper's size in inches; `width / width_in` is the page's resolution.
    pub width_in: f64,
    pub height_in: f64,
    pub rgb: Vec<u8>,
}

impl Sheet {
    /// A sheet, or `None` when the pixels do not match the size or the paper size is not a positive number.
    pub fn new(width: u32, height: u32, width_in: f64, height_in: f64, rgb: Vec<u8>) -> Option<Self> {
        let expected = u64::from(width).checked_mul(u64::from(height))?.checked_mul(3)?;
        let size_ok = width_in.is_finite() && width_in > 0.0 && height_in.is_finite() && height_in > 0.0;
        let ok = width > 0 && height > 0 && rgb.len() as u64 == expected && size_ok;
        ok.then_some(Self { width, height, width_in, height_in, rgb })
    }

    /// Pixels per inch along the width.
    pub fn ppi(&self) -> f64 {
        f64::from(self.width) / self.width_in
    }

    /// The sheet as `w` by `h` pixels: each target pixel averages the source pixels it covers (so a
    /// reduction is a box filter and an enlargement repeats pixels). `None` when the size is empty or
    /// beyond [`MAX_PAGE_PIXELS`].
    pub fn resampled(&self, w: u32, h: u32) -> Option<Vec<u8>> {
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PAGE_PIXELS {
            return None;
        }
        if (w, h) == (self.width, self.height) {
            return Some(self.rgb.clone());
        }
        let (sw, sh) = (self.width as usize, self.height as usize);
        // The source span a target position covers: at least one source pixel, never past the edge.
        let span = |t: u32, tn: u32, sn: usize| {
            let lo = ((u64::from(t) * sn as u64 / u64::from(tn)) as usize).min(sn.saturating_sub(1));
            let hi = ((u64::from(t) + 1) * sn as u64).div_ceil(u64::from(tn)) as usize;
            (lo, hi.clamp(lo + 1, sn.max(lo + 1)))
        };
        let cols: Vec<(usize, usize)> = (0..w).map(|x| span(x, w, sw)).collect();
        let mut out = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            let (y0, y1) = span(y, h, sh);
            for &(x0, x1) in &cols {
                let mut sum = [0u32; 3];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let at = (sy * sw + sx) * 3;
                        if let Some(px) = self.rgb.get(at..at + 3) {
                            for (s, v) in sum.iter_mut().zip(px) {
                                *s += u32::from(*v);
                            }
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)).max(1) as u32;
                out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
            }
        }
        Some(out)
    }
}

/// sRGB to the luma a grey page carries (Rec. 601 weights, on the encoded values).
fn luma(px: &[u8; 3]) -> u8 {
    let [r, g, b] = *px;
    ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000) as u8
}

fn failed(why: impl Into<String>) -> PrintError {
    PrintError::new(ErrorKind::Failed, why)
}

/// The converter for one print: its sheets and the length of the PDF they were composed with.
#[derive(Clone, Debug)]
pub struct SheetConverter {
    sheets: Arc<Vec<Sheet>>,
    pdf_len: usize,
}

impl SheetConverter {
    /// For the `sheets` of a print whose PDF is `pdf_len` bytes long.
    pub fn new(sheets: Arc<Vec<Sheet>>, pdf_len: usize) -> Self {
        Self { sheets, pdf_len }
    }

    fn pwg(&self, context: &ConvertContext) -> Result<OwnedDocument, PrintError> {
        let (dx, dy) = (context.resolution.x_dpi.max(1), context.resolution.y_dpi.max(1));
        let gray = !context.colour;
        let target = |inches: f64, dpi: u32| ((inches * f64::from(dpi)).round() as u32).max(1);
        let mut pages = Vec::with_capacity(self.sheets.len());
        for (index, sheet) in self.sheets.iter().enumerate() {
            if context.cancel.is_cancelled() {
                return Err(PrintError::new(ErrorKind::Cancelled, "the print job was cancelled while the pages were prepared"));
            }
            let (w, h) = (target(sheet.width_in, dx), target(sheet.height_in, dy));
            let rgb =
                sheet.resampled(w, h).ok_or_else(|| failed(format!("page {} is too large at {dx} x {dy} dpi: lower the resolution", index + 1)))?;
            let pixels = if gray { rgb.as_chunks::<3>().0.iter().map(luma).collect() } else { rgb };
            pages.push(pwg::RasterPage { width: w, height: h, gray, pixels });
        }
        let duplex = match context.duplex {
            None | Some(Duplex::Off) => pwg::Duplex::Off,
            Some(Duplex::LongEdge) => pwg::Duplex::LongEdge,
            Some(Duplex::ShortEdge) => pwg::Duplex::ShortEdge,
        };
        let bytes = pwg::encode(&pages, &pwg::PwgOptions { dpi: (dx, dy), duplex }).map_err(|e| failed(e.to_string()))?;
        Ok(OwnedDocument::new(DocumentFormat::PwgRaster, bytes))
    }

    fn jpeg(&self, context: &ConvertContext) -> Result<OwnedDocument, PrintError> {
        let [sheet] = self.sheets.as_slice() else {
            return Err(PrintError::new(ErrorKind::UnsupportedFormat, "a JPEG holds one page; this print has several"));
        };
        let (Ok(w), Ok(h)) = (u16::try_from(sheet.width), u16::try_from(sheet.height)) else {
            return Err(failed("the page is too large for a JPEG"));
        };
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 92);
        let ppi = sheet.ppi().round().clamp(1.0, f64::from(u16::MAX)) as u16;
        encoder.set_density(jpeg_encoder::PixelDensity { density: (ppi, ppi), unit: jpeg_encoder::PixelDensityUnit::Inches });
        if context.colour {
            encoder.encode(&sheet.rgb, w, h, jpeg_encoder::ColorType::Rgb).map_err(|e| failed(e.to_string()))?;
        } else {
            let grey: Vec<u8> = sheet.rgb.as_chunks::<3>().0.iter().map(luma).collect();
            encoder.encode(&grey, w, h, jpeg_encoder::ColorType::Luma).map_err(|e| failed(e.to_string()))?;
        }
        Ok(OwnedDocument::new(DocumentFormat::Jpeg, out))
    }
}

impl DocumentConverter for SheetConverter {
    fn targets(&self, from: DocumentFormat) -> Vec<DocumentFormat> {
        match from {
            DocumentFormat::Pdf if self.sheets.len() == 1 => vec![DocumentFormat::PwgRaster, DocumentFormat::Jpeg],
            DocumentFormat::Pdf if !self.sheets.is_empty() => vec![DocumentFormat::PwgRaster],
            _ => Vec::new(),
        }
    }

    fn convert(&self, document: Document<'_>, target: DocumentFormat, context: &ConvertContext) -> Result<OwnedDocument, PrintError> {
        if document.format != DocumentFormat::Pdf || document.bytes.len() != self.pdf_len {
            return Err(PrintError::new(ErrorKind::UnsupportedFormat, "this converter only makes the pages of the print it was made for"));
        }
        match target {
            DocumentFormat::PwgRaster => self.pwg(context),
            DocumentFormat::Jpeg => self.jpeg(context),
            other => Err(PrintError::new(ErrorKind::UnsupportedFormat, format!("this converter makes PWG raster or JPEG, not {}", other.mime()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_print::Resolution;

    fn sheet(w: u32, h: u32, rgb: [u8; 3]) -> Sheet {
        let pixels = (0..w * h).flat_map(|_| rgb).collect();
        Sheet::new(w, h, f64::from(w) / 100.0, f64::from(h) / 100.0, pixels).expect("a valid sheet")
    }

    fn colour(dpi: u32) -> ConvertContext {
        ConvertContext::new(Resolution::new(dpi, dpi), true)
    }

    #[test]
    fn a_sheet_must_match_its_size() {
        assert!(Sheet::new(2, 2, 1.0, 1.0, vec![0; 12]).is_some());
        for bad in [
            Sheet::new(2, 2, 1.0, 1.0, vec![0; 11]),
            Sheet::new(0, 2, 1.0, 1.0, vec![]),
            Sheet::new(2, 2, 0.0, 1.0, vec![0; 12]),
            Sheet::new(2, 2, f64::NAN, 1.0, vec![0; 12]),
        ] {
            assert!(bad.is_none());
        }
    }

    #[test]
    fn resampling_keeps_flat_colour_and_averages_a_reduction() {
        let flat = sheet(10, 10, [10, 120, 250]);
        assert_eq!(flat.resampled(4, 7).map(|v| v[..3].to_vec()), Some(vec![10, 120, 250]));
        assert_eq!(flat.resampled(40, 40).map(|v| v.len()), Some(40 * 40 * 3));
        let two = Sheet::new(2, 1, 1.0, 1.0, vec![0, 0, 0, 255, 255, 255]).expect("sheet");
        assert_eq!(two.resampled(1, 1), Some(vec![128, 128, 128]));
        assert!(flat.resampled(0, 5).is_none());
        assert!(flat.resampled(100_000, 100_000).is_none());
    }

    #[test]
    fn pwg_comes_out_at_the_asked_resolution_and_the_paper_size() {
        let converter = SheetConverter::new(Arc::new(vec![sheet(100, 150, [200, 30, 30])]), 7);
        let out = converter.convert(Document::pdf(b"1234567"), DocumentFormat::PwgRaster, &colour(50)).expect("pwg");
        assert_eq!(out.format, DocumentFormat::PwgRaster);
        assert!(out.bytes.starts_with(b"RaS2"));
        // 1 x 1.5 in at 50 dpi: 50 x 75 pixels (width at 372, height at 376 of the first page header)
        let header = &out.bytes[4..4 + 1796];
        let at = |o: usize| u32::from_be_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
        assert_eq!((at(372), at(376)), (50, 75));
        assert_eq!(at(400), 19, "colour: sRGB");
        let grey = ConvertContext::new(Resolution::new(50, 50), false);
        let out = converter.convert(Document::pdf(b"1234567"), DocumentFormat::PwgRaster, &grey).expect("grey pwg");
        let header = &out.bytes[4..4 + 1796];
        assert_eq!(u32::from_be_bytes([header[400], header[401], header[402], header[403]]), 18, "monochrome: sGray");
    }

    #[test]
    fn jpeg_is_for_one_page() {
        let one = SheetConverter::new(Arc::new(vec![sheet(20, 30, [0, 0, 255])]), 3);
        let out = one.convert(Document::pdf(b"abc"), DocumentFormat::Jpeg, &colour(300)).expect("jpeg");
        assert_eq!(out.format, DocumentFormat::Jpeg);
        assert!(out.bytes.starts_with(&[0xFF, 0xD8]) && out.bytes.ends_with(&[0xFF, 0xD9]));
        assert!(one.targets(DocumentFormat::Jpeg).is_empty(), "only a PDF is converted");
        let two = SheetConverter::new(Arc::new(vec![sheet(4, 4, [0; 3]), sheet(4, 4, [0; 3])]), 3);
        assert_eq!(two.targets(DocumentFormat::Pdf), vec![DocumentFormat::PwgRaster]);
        let err = two.convert(Document::pdf(b"abc"), DocumentFormat::Jpeg, &colour(300)).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::UnsupportedFormat);
    }

    #[test]
    fn another_prints_document_is_refused() {
        let converter = SheetConverter::new(Arc::new(vec![sheet(4, 4, [0; 3])]), 10);
        let err = converter.convert(Document::pdf(b"too short"), DocumentFormat::PwgRaster, &colour(300)).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::UnsupportedFormat);
        let err = converter.convert(Document::pdf(b"0123456789"), DocumentFormat::Png, &colour(300)).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::UnsupportedFormat);
    }
}
