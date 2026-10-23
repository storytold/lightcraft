//! The composed print pages as the raster documents a printer takes.
//!
//! [`SheetConverter`] is a craft-print [`DocumentConverter`] that carries the pages of one print: asked to turn the
//! print's PDF into a format the printer lists, it ignores the PDF's bytes (they are the same pages, already
//! composed) and renders PWG raster, or a JPEG for a one-page print, from the pages at the resolution the registry
//! chose. It checks that the document it is handed is the print's own (its length), so it can never send the
//! pixels of one print for the document of another.
//!
//! The pixels come from a [`PageSource`]: asked for a page in a [`RasterType`], it makes the samples at the page's own
//! size, and the converter resamples them to the resolution the printer was given. The registry picks the type: the
//! first of the converter's own preference ([`raster_preference`], best first) that the printer lists in
//! `pwg-raster-document-type-supported`, so a printer that takes 16-bit Adobe RGB gets that and one that lists only
//! the baseline gets 8-bit sRGB. A [`Sheet`] is the simple source: one page already made as 8-bit sRGB.

use std::fmt;
use std::sync::Arc;

use craft_print::pwg;
pub use craft_print::pwg::RasterType;
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
        resample(&self.rgb, (self.width, self.height), (w, h), RasterType::Srgb8)
    }
}

/// A page's pixel size and the paper it covers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeometry {
    pub width: u32,
    pub height: u32,
    /// The paper's size in inches; `width / width_in` is the page's resolution.
    pub width_in: f64,
    pub height_in: f64,
}

impl PageGeometry {
    /// The geometry, or `None` when a size is empty or the paper size is not a positive number.
    pub fn new(width: u32, height: u32, width_in: f64, height_in: f64) -> Option<Self> {
        let size_ok = width_in.is_finite() && width_in > 0.0 && height_in.is_finite() && height_in > 0.0;
        (width > 0 && height > 0 && size_ok).then_some(Self { width, height, width_in, height_in })
    }

    /// Pixels per inch along the width.
    pub fn ppi(&self) -> f64 {
        f64::from(self.width) / self.width_in
    }
}

/// Makes page `index` as samples in a [`RasterType`], at the page's own pixel size, rows tightly packed: one byte a
/// sample for the 8-bit types, two (most significant first) for the 16-bit ones.
pub type PageSource = Arc<dyn Fn(usize, RasterType) -> Result<Vec<u8>, String> + Send + Sync>;

/// The raster types a print offers, best first. `wide_gamut`: the print may be described in Adobe RGB (the job asks
/// for a printer profile or for Adobe RGB), else only in sRGB; `sixteen_bit`: 16 bits a sample are wanted (the job's
/// setting), else 8. A colour type the printer lists earlier here wins, then the grey ones, which a monochrome job
/// takes (the registry picks colour or grey by the job).
///
/// Adobe RGB is offered **only at 16 bits**: 8 bits spread over its wider gamut leave larger steps between colours and
/// show as banding in smooth gradients (CUPS made the same choice for the same reason, on the printing-architecture
/// list), so a printer that lists only `adobe-rgb_8` gets 8-bit sRGB.
pub fn raster_preference(wide_gamut: bool, sixteen_bit: bool) -> Vec<RasterType> {
    use RasterType::*;
    let mut kinds = Vec::new();
    if wide_gamut && sixteen_bit {
        kinds.push(AdobeRgb16);
    }
    if sixteen_bit {
        kinds.push(Srgb16);
    }
    kinds.push(Srgb8);
    if sixteen_bit {
        kinds.push(Sgray16);
    }
    kinds.push(Sgray8);
    kinds
}

/// `data` (a page of `from` pixels in `kind`) as `to` pixels: each target pixel averages the source pixels it covers
/// (so a reduction is a box filter and an enlargement repeats pixels). `None` when the target is empty or beyond
/// [`MAX_PAGE_PIXELS`], or the data is not the size of the source.
pub fn resample(data: &[u8], from: (u32, u32), to: (u32, u32), kind: RasterType) -> Option<Vec<u8>> {
    let ((sw, sh), (w, h)) = (from, to);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PAGE_PIXELS {
        return None;
    }
    let (channels, per_sample) = (kind.channels(), kind.bits() as usize / 8);
    if data.len() != (sw as usize).checked_mul(sh as usize)?.checked_mul(channels * per_sample)? {
        return None;
    }
    if from == to {
        return Some(data.to_vec());
    }
    let (sw, sh) = (sw as usize, sh as usize);
    // The source span a target position covers: at least one source pixel, never past the edge.
    let span = |t: u32, tn: u32, sn: usize| {
        let lo = ((u64::from(t) * sn as u64 / u64::from(tn)) as usize).min(sn.saturating_sub(1));
        let hi = ((u64::from(t) + 1) * sn as u64).div_ceil(u64::from(tn)) as usize;
        (lo, hi.clamp(lo + 1, sn.max(lo + 1)))
    };
    let cols: Vec<(usize, usize)> = (0..w).map(|x| span(x, w, sw)).collect();
    let mut out = Vec::with_capacity(w as usize * h as usize * channels * per_sample);
    let sample = |at: usize| -> u64 {
        if per_sample == 2 {
            data.get(at..at + 2).map_or(0, |b| u64::from(u16::from_be_bytes([b[0], b[1]])))
        } else {
            data.get(at).map_or(0, |&b| u64::from(b))
        }
    };
    for y in 0..h {
        let (y0, y1) = span(y, h, sh);
        for &(x0, x1) in &cols {
            let mut sum = [0u64; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let at = (sy * sw + sx) * channels * per_sample;
                    for (c, s) in sum.iter_mut().take(channels).enumerate() {
                        *s += sample(at + c * per_sample);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as u64;
            for s in sum.iter().take(channels) {
                let mean = (s + n / 2) / n;
                if per_sample == 2 {
                    out.extend_from_slice(&(mean as u16).to_be_bytes());
                } else {
                    out.push(mean as u8);
                }
            }
        }
    }
    Some(out)
}

/// sRGB to the luma a grey page carries (Rec. 601 weights, on the encoded values).
fn luma(px: &[u8; 3]) -> u8 {
    let [r, g, b] = *px;
    ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000) as u8
}

fn failed(why: impl Into<String>) -> PrintError {
    PrintError::new(ErrorKind::Failed, why)
}

/// The converter for one print: its pages and the length of the PDF they were composed with.
#[derive(Clone)]
pub struct SheetConverter {
    pages: Arc<Vec<PageGeometry>>,
    source: PageSource,
    preference: Vec<RasterType>,
    pdf_len: usize,
}

impl fmt::Debug for SheetConverter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SheetConverter")
            .field("pages", &self.pages.len())
            .field("preference", &self.preference)
            .field("pdf_len", &self.pdf_len)
            .finish()
    }
}

impl SheetConverter {
    /// For the `sheets` of a print whose PDF is `pdf_len` bytes long: 8-bit sRGB (and grey) only.
    pub fn new(sheets: Arc<Vec<Sheet>>, pdf_len: usize) -> Self {
        let pages: Vec<PageGeometry> = sheets.iter().filter_map(|s| PageGeometry::new(s.width, s.height, s.width_in, s.height_in)).collect();
        let source: PageSource = Arc::new(move |index, kind| {
            let sheet = sheets.get(index).ok_or("no such page")?;
            match kind {
                RasterType::Srgb8 => Ok(sheet.rgb.clone()),
                RasterType::Sgray8 => Ok(sheet.rgb.as_chunks::<3>().0.iter().map(luma).collect()),
                other => Err(format!("these sheets are 8-bit sRGB, not {}", other.keyword())),
            }
        });
        Self { pages: Arc::new(pages), source, preference: RasterType::BASELINE.to_vec(), pdf_len }
    }

    /// For the pages of a print (`pages`, their geometry) whose pixels `source` makes in whichever of `preference` (best
    /// first, see [`raster_preference`]) the printer lists; `pdf_len` is the length of the PDF they were composed with.
    pub fn from_source(pages: Vec<PageGeometry>, source: PageSource, preference: Vec<RasterType>, pdf_len: usize) -> Self {
        Self { pages: Arc::new(pages), source, preference, pdf_len }
    }

    /// The samples of page `index` in `kind` at its own size, checked against the page.
    fn samples(&self, index: usize, page: &PageGeometry, kind: RasterType) -> Result<Vec<u8>, PrintError> {
        let data = (self.source)(index, kind).map_err(|e| failed(format!("page {} could not be made: {e}", index + 1)))?;
        let want = (page.width as usize).checked_mul(page.height as usize).and_then(|n| n.checked_mul(kind.bytes_per_pixel()));
        if Some(data.len()) != want {
            return Err(failed(format!("page {} came out the wrong size for {}", index + 1, kind.keyword())));
        }
        Ok(data)
    }

    fn pwg(&self, context: &ConvertContext) -> Result<OwnedDocument, PrintError> {
        let (dx, dy) = (context.resolution.x_dpi.max(1), context.resolution.y_dpi.max(1));
        let kind = context.raster;
        let target = |inches: f64, dpi: u32| ((inches * f64::from(dpi)).round() as u32).max(1);
        let mut pages = Vec::with_capacity(self.pages.len());
        for (index, page) in self.pages.iter().enumerate() {
            if context.cancel.is_cancelled() {
                return Err(PrintError::new(ErrorKind::Cancelled, "the print job was cancelled while the pages were prepared"));
            }
            let (w, h) = (target(page.width_in, dx), target(page.height_in, dy));
            let native = self.samples(index, page, kind)?;
            let data = resample(&native, (page.width, page.height), (w, h), kind)
                .ok_or_else(|| failed(format!("page {} is too large at {dx} x {dy} dpi: lower the resolution", index + 1)))?;
            pages.push(pwg::TypedPage { width: w, height: h, kind, data });
        }
        let duplex = match context.duplex {
            None | Some(Duplex::Off) => pwg::Duplex::Off,
            Some(Duplex::LongEdge) => pwg::Duplex::LongEdge,
            Some(Duplex::ShortEdge) => pwg::Duplex::ShortEdge,
        };
        let bytes = pwg::encode_typed(&pages, &pwg::PwgOptions { dpi: (dx, dy), duplex }).map_err(|e| failed(e.to_string()))?;
        Ok(OwnedDocument::new(DocumentFormat::PwgRaster, bytes))
    }

    fn jpeg(&self, context: &ConvertContext) -> Result<OwnedDocument, PrintError> {
        let [page] = self.pages.as_slice() else {
            return Err(PrintError::new(ErrorKind::UnsupportedFormat, "a JPEG holds one page; this print has several"));
        };
        let (Ok(w), Ok(h)) = (u16::try_from(page.width), u16::try_from(page.height)) else {
            return Err(failed("the page is too large for a JPEG"));
        };
        let kind = if context.colour { RasterType::Srgb8 } else { RasterType::Sgray8 };
        let samples = self.samples(0, page, kind)?;
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 92);
        let ppi = page.ppi().round().clamp(1.0, f64::from(u16::MAX)) as u16;
        encoder.set_density(jpeg_encoder::PixelDensity { density: (ppi, ppi), unit: jpeg_encoder::PixelDensityUnit::Inches });
        let colour_type = if context.colour { jpeg_encoder::ColorType::Rgb } else { jpeg_encoder::ColorType::Luma };
        encoder.encode(&samples, w, h, colour_type).map_err(|e| failed(e.to_string()))?;
        Ok(OwnedDocument::new(DocumentFormat::Jpeg, out))
    }
}

impl DocumentConverter for SheetConverter {
    fn targets(&self, from: DocumentFormat) -> Vec<DocumentFormat> {
        match from {
            DocumentFormat::Pdf if self.pages.len() == 1 => vec![DocumentFormat::PwgRaster, DocumentFormat::Jpeg],
            DocumentFormat::Pdf if !self.pages.is_empty() => vec![DocumentFormat::PwgRaster],
            _ => Vec::new(),
        }
    }

    fn raster_types(&self) -> Vec<RasterType> {
        self.preference.clone()
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
    fn sixteen_bit_resampling_averages_with_both_bytes() {
        // two 16-bit grey pixels, 0 and 0xFFFF, averaged into one: 0x8000 (rounded), not an 8-bit mean of the bytes
        let data = [0x00, 0x00, 0xFF, 0xFF];
        assert_eq!(resample(&data, (2, 1), (1, 1), RasterType::Sgray16), Some(vec![0x80, 0x00]));
        // flat 16-bit colour survives any resize exactly
        let flat: Vec<u8> = (0..9).flat_map(|_| [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]).collect();
        let out = resample(&flat, (3, 3), (5, 2), RasterType::AdobeRgb16).expect("resampled");
        assert!(out.as_chunks::<6>().0.iter().all(|px| *px == [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]));
        // a buffer that is not the source's size is refused
        assert!(resample(&flat[..5], (3, 3), (5, 2), RasterType::AdobeRgb16).is_none());
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

    #[test]
    fn the_preference_follows_the_jobs_gamut_and_depth() {
        use RasterType::*;
        assert_eq!(raster_preference(true, true), [AdobeRgb16, Srgb16, Srgb8, Sgray16, Sgray8]);
        // Adobe RGB only at 16 bits: at 8 it bands, so a wide-gamut 8-bit job is sRGB
        assert_eq!(raster_preference(true, false), [Srgb8, Sgray8]);
        assert_eq!(raster_preference(false, true), [Srgb16, Srgb8, Sgray16, Sgray8]);
        assert_eq!(raster_preference(false, false), [Srgb8, Sgray8]);
        assert!(
            ![raster_preference(true, false), raster_preference(false, true), raster_preference(false, false)]
                .iter()
                .flatten()
                .any(|k| *k == AdobeRgb8)
        );
        // the 8-bit sheets only ever offer the baseline
        assert_eq!(SheetConverter::new(Arc::new(vec![sheet(2, 2, [0; 3])]), 1).raster_types(), RasterType::BASELINE.to_vec());
    }

    /// A page source that makes `value` in every sample of every type.
    fn flat_source(value: u8) -> PageSource {
        Arc::new(move |_, kind: RasterType| Ok(vec![value; 4 * 4 * kind.bytes_per_pixel()]))
    }

    #[test]
    fn the_converter_writes_the_type_the_registry_chose() {
        let pages = vec![PageGeometry::new(4, 4, 1.0, 1.0).expect("geometry")];
        let converter = SheetConverter::from_source(pages, flat_source(0x7F), raster_preference(true, true), 1);
        for kind in RasterType::ALL {
            let mut context = colour(8);
            context.raster = kind;
            let out = converter.convert(Document::pdf(b"x"), DocumentFormat::PwgRaster, &context).expect("pwg");
            let header = &out.bytes[4..4 + 1796];
            let at = |o: usize| u32::from_be_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
            assert_eq!((at(372), at(376)), (8, 8), "1 in at 8 dpi");
            assert_eq!((at(400), at(384), at(420)), (kind.colour_space(), kind.bits(), kind.channels() as u32), "{kind:?}");
        }
    }

    #[test]
    fn a_source_that_fails_or_returns_the_wrong_size_is_a_failure_before_anything_is_sent() {
        let pages = vec![PageGeometry::new(4, 4, 1.0, 1.0).expect("geometry")];
        let wrong: PageSource = Arc::new(|_, _| Ok(vec![0; 5]));
        let err = SheetConverter::from_source(pages.clone(), wrong, raster_preference(true, true), 1)
            .convert(Document::pdf(b"x"), DocumentFormat::PwgRaster, &colour(8))
            .expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::Failed);
        let broken: PageSource = Arc::new(|_, _| Err("no pixels".into()));
        let err = SheetConverter::from_source(pages, broken, raster_preference(true, true), 1)
            .convert(Document::pdf(b"x"), DocumentFormat::PwgRaster, &colour(8))
            .expect_err("refused");
        assert!(err.to_string().contains("no pixels"), "{err}");
    }
}
