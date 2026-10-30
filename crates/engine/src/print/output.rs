//! Writes composed pages as a print-ready PDF: each page is converted to the job's colour target
//! (a printer / paper profile, or sRGB / Adobe RGB when the printer manages colour) with its
//! intent, black point compensation and print adjustment ([`crate::print_color`]), then embedded
//! covering the paper as an ICC-tagged, losslessly compressed 16-bit (or 8-bit) image. A printer
//! profile is also named as the document's output intent ([`crate::print_pdf`]).
use super::{MAX_JOB_PAGES, PageRaster, PrintJob};
use crate::print_color::{ManagedSpace, PrintColorJob, PrintProfile};
use crate::print_pdf::ImageData;
use lightcraft_color::transfer::encode_srgb8;
use lightcraft_color::{REC2020, SRGB};
use lightcraft_pipeline::OutputSpace;
use lightcraft_raster::Rgba8;

/// Largest PDF produced, bytes.
const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;

/// A Rec. 2020 page as 8-bit sRGB (out-of-gamut colours are clipped).
pub fn page_to_srgb8(page: &PageRaster) -> Result<Rgba8, String> {
    let (w, h) = (page.width as usize, page.height as usize);
    let n = w.checked_mul(h).ok_or("page too large")?;
    if n == 0 || page.rgb.len() != n.checked_mul(3).ok_or("page too large")? {
        return Err("a page's pixel data does not match its size".into());
    }
    let trc = OutputSpace::Rec2020.trc();
    // 16-bit Rec. 2020 code → linear light, once per code value.
    let lut: Vec<f32> = (0..=u16::MAX).map(|v| trc.decode(f32::from(v) / 65535.0)).collect();
    let m = REC2020.to_space(&SRGB).to_f32();
    let mut out = Rgba8::new(w, h);
    for (dst, src) in out.data.iter_mut().zip(page.rgb.as_chunks::<3>().0) {
        let l = src.map(|v| lut.get(usize::from(v)).copied().unwrap_or(0.0));
        let row = |r: usize| m.get(r).map_or(0.0, |m| m[0] * l[0] + m[1] * l[1] + m[2] * l[2]);
        *dst = [encode_srgb8(row(0)), encode_srgb8(row(1)), encode_srgb8(row(2)), 255];
    }
    Ok(out)
}

/// The pixel format of the raster sent to a printer that takes raster rather than PDF: what the printer lists in
/// `pwg-raster-document-type-supported`, as far as it can be made here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SheetFormat {
    /// Adobe RGB (a wider gamut than sRGB); else sRGB.
    pub adobe_rgb: bool,
    /// 16 bits a sample (most significant byte first); else 8.
    pub sixteen_bit: bool,
    /// One grey sample a pixel; else three.
    pub gray: bool,
}

impl SheetFormat {
    /// Bytes in one pixel.
    pub fn bytes_per_pixel(self) -> usize {
        (if self.gray { 1 } else { 3 }) * (if self.sixteen_bit { 2 } else { 1 })
    }
}

/// A composed page as interleaved samples in `format`, at the page's own size: the raster sent to a printer that
/// takes raster rather than PDF. It goes through the same conversion as the PDF's page image ([`PrintColorJob`]): the
/// job's rendering intent, black point compensation and print adjustment (brightness and contrast) apply, so the
/// printer's format does not change the print, and the page is taken to the format's own colour space (sRGB or Adobe
/// RGB, from the Rec. 2020 page, at the format's depth). The printer profile does not apply: the printer behind a
/// raster format manages colour itself. A grey format takes the Rec. 709 luma of the converted colours
/// ([`super::gray_u8`]'s weights), which leaves a page that is already grey as it is.
pub fn sheet_samples(page: &PageRaster, job: &PrintJob, format: SheetFormat) -> Result<Vec<u8>, String> {
    let mut settings = job.color.clone();
    settings.profile = PrintProfile::Managed { space: if format.adobe_rgb { ManagedSpace::AdobeRgb } else { ManagedSpace::Srgb } };
    settings.sixteen_bit = format.sixteen_bit;
    let color = PrintColorJob::new(&settings)?;
    let image = color.page_image(page.width, page.height, &page.rgb)?;
    let pixels = (page.width as usize).checked_mul(page.height as usize).ok_or("page too large")?;
    let per_sample = if format.sixteen_bit { 2 } else { 1 };
    let samples = match image.data {
        ImageData::Raw { bits, samples }
            if bits == if format.sixteen_bit { 16 } else { 8 } && Some(samples.len()) == pixels.checked_mul(3 * per_sample) =>
        {
            samples
        }
        _ => return Err("the page could not be made into raster samples".into()),
    };
    if !format.gray {
        return Ok(samples);
    }
    let mut out = Vec::with_capacity(pixels * per_sample);
    for px in samples.chunks_exact(3 * per_sample) {
        let sample = |c: usize| -> u32 {
            if format.sixteen_bit {
                px.get(c * 2..c * 2 + 2).map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])))
            } else {
                px.get(c).map_or(0, |&b| u32::from(b))
            }
        };
        let y = (sample(0) * 54 + sample(1) * 183 + sample(2) * 19) >> 8;
        if format.sixteen_bit {
            out.extend_from_slice(&(y as u16).to_be_bytes());
        } else {
            out.push(y as u8);
        }
    }
    Ok(out)
}

/// The PDF for `pages` (all the same size): one page per raster, its MediaBox the paper size in
/// points (`pixels / ppi * 72`) with the image covering it. Each page is converted and compressed
/// in turn, so memory holds one page's device samples at a time.
pub fn write(pages: &[PageRaster], job: &PrintJob, title: &str) -> Result<Vec<u8>, String> {
    let first = pages.first().ok_or("nothing to print")?;
    if pages.len() > MAX_JOB_PAGES {
        return Err(format!("a print is limited to {MAX_JOB_PAGES} pages"));
    }
    if !(first.ppi.is_finite() && first.ppi > 0.0) {
        return Err("a page has no resolution".into());
    }
    let points = |px: u32| f64::from(px) / f64::from(first.ppi) * 72.0;
    let size = (points(first.width), points(first.height));
    let color = PrintColorJob::new(&job.color)?;
    let mut out = Vec::with_capacity(pages.len());
    let mut bytes = 0usize;
    for page in pages {
        if (page.width, page.height) != (first.width, first.height) {
            return Err("all pages of a PDF must be the same size".into());
        }
        let image = color.page_image(page.width, page.height, &page.rgb)?.deflated()?;
        if let ImageData::Flate { data, .. } = &image.data {
            bytes = bytes.saturating_add(data.len());
        }
        if bytes > MAX_PDF_BYTES {
            return Err("the PDF would exceed 256 MiB; lower the print resolution, use 8 bits or print fewer pages".into());
        }
        out.push((size, image));
    }
    color.pdf(out, title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::compose::rec2020_from_srgb8;

    fn raster(w: u32, h: u32, srgb: [u8; 3]) -> PageRaster {
        let c = rec2020_from_srgb8(srgb);
        PageRaster { width: w, height: h, ppi: 100.0, rgb: (0..w * h).flat_map(|_| c).collect() }
    }

    #[test]
    fn colours_survive_the_round_trip() {
        for c in [[255u8, 255, 255], [0, 0, 0], [128, 128, 128], [200, 30, 30], [10, 200, 90]] {
            let px = page_to_srgb8(&raster(2, 2, c)).unwrap().data[0];
            for k in 0..3 {
                assert!(px[k].abs_diff(c[k]) <= 1, "{c:?} -> {px:?}");
            }
        }
        assert!(page_to_srgb8(&PageRaster { width: 2, height: 2, ppi: 100.0, rgb: vec![0; 5] }).is_err());
        assert!(page_to_srgb8(&PageRaster { width: 0, height: 2, ppi: 100.0, rgb: vec![] }).is_err());
    }

    const SRGB8: SheetFormat = SheetFormat { adobe_rgb: false, sixteen_bit: false, gray: false };

    #[test]
    fn a_sheet_gets_the_print_adjustment_like_the_pdf_does() {
        let page = raster(2, 2, [128, 128, 128]);
        let plain = PrintJob::default();
        let sheet = sheet_samples(&page, &plain, SRGB8).unwrap();
        let shown = page_to_srgb8(&page).unwrap().data[0];
        assert!((0..3).all(|k| sheet[k].abs_diff(shown[k]) <= 1), "with no adjustment the sheet is the page: {sheet:?} vs {shown:?}");
        let mut bright = PrintJob::default();
        bright.color.brightness = 60.0;
        let lit = sheet_samples(&page, &bright, SRGB8).unwrap();
        assert!(lit[0] > sheet[0] + 10, "brightness +60 reaches the sheet: {lit:?} vs {sheet:?}");
        assert_eq!(lit.len(), 2 * 2 * 3);
        assert!(sheet_samples(&PageRaster { width: 2, height: 2, ppi: 100.0, rgb: vec![0; 5] }, &plain, SRGB8).is_err());
    }

    fn sample16(px: &[u8], c: usize) -> u16 {
        u16::from_be_bytes([px[c * 2], px[c * 2 + 1]])
    }

    #[test]
    fn sixteen_bit_samples_are_the_same_colours_with_more_precision() {
        let page = raster(2, 1, [200, 30, 90]);
        let job = PrintJob::default();
        let eight = sheet_samples(&page, &job, SRGB8).unwrap();
        let sixteen = sheet_samples(&page, &job, SheetFormat { sixteen_bit: true, ..SRGB8 }).unwrap();
        assert_eq!(sixteen.len(), 2 * 6, "two pixels, three 16-bit samples each");
        for c in 0..3 {
            let wide = i32::from(sample16(&sixteen, c));
            assert!((wide / 257 - i32::from(eight[c])).abs() <= 1, "channel {c}: {wide} vs {}", eight[c]);
        }
        assert!(sixteen.as_chunks::<2>().0.iter().any(|b| b[1] != b[0]), "the low bytes carry detail");
    }

    #[test]
    fn adobe_rgb_keeps_a_colour_the_page_has_beyond_srgb() {
        // A saturated green of the page: inside Adobe RGB's gamut and outside sRGB's, so sRGB clips it.
        let green = rec2020_from_srgb8([0, 255, 0]);
        let wide = [(green[0] / 2).max(1), green[1], green[2] / 2];
        let page = PageRaster { width: 1, height: 1, ppi: 100.0, rgb: wide.to_vec() };
        let job = PrintJob::default();
        let srgb = sheet_samples(&page, &job, SRGB8).unwrap();
        let adobe = sheet_samples(&page, &job, SheetFormat { adobe_rgb: true, ..SRGB8 }).unwrap();
        assert_ne!(srgb, adobe, "the two colour spaces describe the same page differently");
        assert_eq!(adobe.len(), 3);
    }

    #[test]
    fn a_grey_format_has_one_sample_and_leaves_a_grey_page_alone() {
        let page = raster(3, 1, [90, 90, 90]);
        let job = PrintJob::default();
        let colour = sheet_samples(&page, &job, SRGB8).unwrap();
        let grey = sheet_samples(&page, &job, SheetFormat { gray: true, ..SRGB8 }).unwrap();
        assert_eq!(grey.len(), 3);
        assert!(grey.iter().all(|&g| g.abs_diff(colour[0]) <= 1), "a neutral colour stays itself: {grey:?} vs {colour:?}");
        let grey16 = sheet_samples(&page, &job, SheetFormat { gray: true, sixteen_bit: true, ..SRGB8 }).unwrap();
        assert_eq!(grey16.len(), 6);
        // a colour page is turned grey with the preview's weights
        let red = raster(1, 1, [255, 0, 0]);
        let y = sheet_samples(&red, &job, SheetFormat { gray: true, ..SRGB8 }).unwrap();
        assert!(y[0].abs_diff(53) <= 2, "{y:?}");
    }

    #[test]
    fn pdf_pages_have_the_paper_size() {
        let pages = [raster(425, 550, [255, 255, 255]), raster(425, 550, [0, 0, 255])];
        let bytes = write(&pages, &PrintJob::default(), "t").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF-"));
        assert!(text.contains("/Count 2"));
        let at = text.find("/MediaBox [").unwrap() + "/MediaBox [".len();
        let nums: Vec<f64> = text[at..].split(']').next().unwrap().split_whitespace().map(|v| v.parse().unwrap()).collect();
        assert_eq!((nums[2], nums[3]), (306.0, 396.0), "4.25 x 5.5 in is 306 x 396 pt");
        assert!(text.contains("/Width 425 /Height 550"));
        // managed by the printer: sRGB, 16 bits, tagged, no output intent
        assert!(text.contains("/ICCBased") && text.contains("/BitsPerComponent 16") && !text.contains("/OutputIntents"));
        assert!(write(&[], &PrintJob::default(), "t").is_err());
        let mixed = [raster(10, 10, [0; 3]), raster(10, 12, [0; 3])];
        assert!(write(&mixed, &PrintJob::default(), "t").is_err());
        let bad = PageRaster { ppi: f32::NAN, ..raster(4, 4, [0; 3]) };
        assert!(write(&[bad], &PrintJob::default(), "t").is_err());
        let zero = PageRaster { ppi: 0.0, ..raster(4, 4, [0; 3]) };
        assert!(write(&[zero], &PrintJob::default(), "t").is_err());
    }

    #[test]
    fn a_printer_profile_is_converted_to_and_named_as_the_output_intent() {
        use crate::print_color::{PrintColorSettings, PrintIntent, PrintProfile};
        let dir = std::env::temp_dir().join(format!("lc-print-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let icc = dir.join("Lustre Paper.icc");
        std::fs::write(&icc, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::AdobeRgb)).unwrap();
        let color =
            PrintColorSettings { profile: PrintProfile::Icc { path: icc }, intent: PrintIntent::Relative, sixteen_bit: false, ..Default::default() };
        let job = PrintJob { color, ..Default::default() };
        let bytes = write(&[raster(20, 30, [200, 30, 30])], &job, "Lustre").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/OutputIntents") && text.contains("/DestOutputProfile"), "{}", &text[..text.len().min(500)]);
        assert!(text.contains("/BitsPerComponent 8") && text.contains("/Title (Lustre)"));
        // a profile that isn't one fails the print and says which file
        std::fs::write(dir.join("Junk.icc"), [7u8; 200]).unwrap();
        let junk = PrintJob {
            color: PrintColorSettings { profile: PrintProfile::Icc { path: dir.join("Junk.icc") }, ..Default::default() },
            ..Default::default()
        };
        let err = write(&[raster(2, 2, [0; 3])], &junk, "t").unwrap_err();
        assert!(err.contains("Junk"), "{err}");
    }
}
