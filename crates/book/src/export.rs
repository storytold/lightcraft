//! Book export: one PDF (front cover, pages, back cover; spreads as single pages) or one JPEG
//! per page, at the book's resolution, JPEG quality and output sharpening.
//!
//! PDF pages carry the bleed in the media box and the trim in the trim box. Each page's photos,
//! backgrounds and graphics are one sRGB JPEG with the ICC profile attached; its text is vector
//! text with the fonts embedded as subsets (searchable, sharp at any size). Text opacity is
//! kept in rasters only for now: the PDF writer paints text opaque.

use dac_layout::render::PhotoSource;
use dac_raster::Rgba8;
use dac_text::TextEngine;

use crate::render::{Options, layout_item, render_page, text_items};
use crate::{Book, BookError, PageRef, Result, Sharpening};

/// Called before each page with (done, total); `false` cancels.
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize) -> bool;

/// Unsharp mask tuned per level (radius scales with resolution).
pub fn sharpen(img: &mut Rgba8, level: Sharpening, ppi: u32) {
    let amount = match level {
        Sharpening::Off => return,
        Sharpening::Low => 0.35,
        Sharpening::Standard => 0.7,
        Sharpening::High => 1.1,
    };
    let r = ((ppi as f32 / 240.0).round() as usize).clamp(1, 3);
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || img.data.len() != w.saturating_mul(h) {
        return;
    }
    // separable box blur
    let mut tmp = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
            let mut s = [0f32; 3];
            for xx in x0..=x1 {
                let p = img.data[y * w + xx];
                (0..3).for_each(|k| s[k] += f32::from(p[k]));
            }
            let n = (x1 - x0 + 1) as f32;
            tmp[y * w + x] = s.map(|v| v / n);
        }
    }
    let mut out = img.data.clone();
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            let mut s = [0f32; 3];
            for yy in y0..=y1 {
                let p = tmp[yy * w + x];
                (0..3).for_each(|k| s[k] += p[k]);
            }
            let n = (y1 - y0 + 1) as f32;
            let o = &mut out[y * w + x];
            for k in 0..3 {
                let v = f32::from(o[k]);
                o[k] = (v + (v - s[k] / n) * amount).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    img.data = out;
}

fn jpeg(img: &Rgba8, quality: u8, ppi: u32, icc: &[u8]) -> Result<Vec<u8>> {
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let e = dac_codecs::EncodeImage::new(img.width as u32, img.height as u32, 3, dac_codecs::Samples::U8(&rgb));
    let meta = dac_codecs::EncodeMeta { icc: Some(icc), ppi: u16::try_from(ppi).ok(), ..Default::default() };
    dac_codecs::encode_jpeg(&e, quality.clamp(1, 100), dac_codecs::ChromaSubsampling::S444, &meta).map_err(|e| BookError::Export(e.to_string()))
}

fn srgb_icc() -> Vec<u8> {
    dac_codecs::icc::write_named(dac_codecs::NamedSpace::Srgb)
}

/// The file name suffix of a page (`cover`, `001`, …, `back`).
pub fn page_suffix(r: PageRef) -> String {
    match r {
        PageRef::Front => "cover".into(),
        PageRef::Back => "back".into(),
        PageRef::Page(i) => format!("{:03}", i + 1),
    }
}

fn check(book: &Book) -> Result<Vec<PageRef>> {
    book.validate()?;
    let pages = book.all_pages();
    if book.pages.is_empty() {
        return Err(BookError::Export("the book has no pages".into()));
    }
    Ok(pages)
}

/// One JPEG per page: `(file name suffix, bytes)`.
pub fn export_jpegs(book: &Book, photos: &dyn PhotoSource, engine: &mut TextEngine, progress: Progress) -> Result<Vec<(String, Vec<u8>)>> {
    let pages = check(book)?;
    let s = &book.settings;
    let icc = srgb_icc();
    let mut out = Vec::with_capacity(pages.len());
    for (i, r) in pages.iter().enumerate() {
        if !progress(i, pages.len()) {
            return Err(BookError::Export("cancelled".into()));
        }
        let mut img = render_page(book, *r, photos, engine, &Options { dpi: s.resolution as f32, ..Options::default() })?.image;
        sharpen(&mut img, s.sharpening, s.resolution);
        out.push((page_suffix(*r), jpeg(&img, s.jpeg_quality, s.resolution, &icc)?));
    }
    Ok(out)
}

/// The whole book as one PDF.
pub fn export_pdf(book: &Book, photos: &dyn PhotoSource, engine: &mut TextEngine, progress: Progress) -> Result<Vec<u8>> {
    let pages = check(book)?;
    let s = &book.settings;
    let icc = std::sync::Arc::new(srgb_icc());
    let trim = book.trim();
    let bleed = f64::from(s.bleed);
    let (pw, ph) = (f64::from(trim.w) + 2.0 * bleed, f64::from(trim.h) + 2.0 * bleed);
    let mut doc = dac_pdf::Document::new();
    doc.metadata.title = Some(book.name.clone()).filter(|n| !n.is_empty());
    if !s.paper.is_empty() {
        doc.metadata.subject = Some(format!("Paper: {}", s.paper));
    }
    let perr = |e: dac_pdf::PdfError| BookError::Export(e.to_string());
    for (i, r) in pages.iter().enumerate() {
        if !progress(i, pages.len()) {
            return Err(BookError::Export("cancelled".into()));
        }
        let mut img =
            render_page(book, *r, photos, engine, &Options { dpi: s.resolution as f32, bleed: true, text: false, ..Options::default() })?.image;
        sharpen(&mut img, s.sharpening, s.resolution);
        let bytes = jpeg(&img, s.jpeg_quality, s.resolution, &[])?;
        let id = doc.add_image(dac_pdf::Image::jpeg(bytes, dac_pdf::ColorSpace::Icc { profile: icc.clone(), components: 3 })).map_err(perr)?;
        let mut page = dac_pdf::Page::new(pw, ph);
        page.image(id, dac_pdf::Rect::new(0.0, 0.0, pw, ph));
        if bleed > 0.0 {
            page.bleed = Some(dac_pdf::Rect::new(0.0, 0.0, pw, ph));
            page.trim = Some(dac_pdf::Rect::new(bleed, bleed, f64::from(trim.w), f64::from(trim.h)));
        }
        for item in text_items(book, *r, photos, false) {
            let placed = layout_item(engine, &item, 72.0);
            let rect =
                dac_pdf::Rect::new(f64::from(item.rect.x) + bleed, f64::from(item.rect.y) + bleed, f64::from(item.rect.w), f64::from(item.rect.h));
            crate::text::draw_pdf(&mut page, &placed, rect);
        }
        doc.push_page(page).map_err(perr)?;
    }
    doc.to_bytes().map_err(perr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CellContent;

    struct Solid;
    impl PhotoSource for Solid {
        fn image(&self, _: &str, _: usize) -> std::result::Result<Rgba8, String> {
            Ok(Rgba8::from_fn(64, 48, |x, _| if x < 32 { [200, 30, 30, 255] } else { [30, 30, 200, 255] }))
        }
    }

    fn book() -> Book {
        let mut b = Book::default();
        b.settings.resolution = 72;
        b.settings.size = crate::BookSize::SmallSquare;
        crate::auto::auto_layout(&mut b, &["a".into(), "b".into(), "c".into()], &crate::auto::builtin_presets()[2]).unwrap();
        if let CellContent::Text { text, .. } = &mut b.front.cells[1].content {
            *text = "Summer".into();
        }
        b
    }

    #[test]
    fn pdf_has_cover_pages_and_text() {
        let b = book();
        let mut e = TextEngine::new();
        let mut calls = 0;
        let pdf = export_pdf(&b, &Solid, &mut e, &mut |_, _| {
            calls += 1;
            true
        })
        .unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert_eq!(calls, b.pages.len() + 2);
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains(&format!("/Count {}", b.pages.len() + 2)), "page count");
        assert!(s.contains("/TrimBox") && s.contains("/FontFile"));
        assert!(export_pdf(&b, &Solid, &mut e, &mut |_, _| false).is_err());
        assert!(export_pdf(&Book::default(), &Solid, &mut e, &mut |_, _| true).is_err());
    }

    #[test]
    fn jpegs_one_per_page() {
        let mut b = book();
        b.settings.cover = crate::CoverType::None;
        let mut e = TextEngine::new();
        let out = export_jpegs(&b, &Solid, &mut e, &mut |_, _| true).unwrap();
        assert_eq!(out.len(), b.pages.len());
        assert_eq!(out[0].0, "001");
        assert!(out.iter().all(|(_, j)| j.starts_with(&[0xFF, 0xD8])));
    }

    #[test]
    fn sharpen_is_safe_and_increases_contrast() {
        let mut img = Rgba8::from_fn(9, 9, |x, _| if x < 4 { [100, 100, 100, 255] } else { [150, 150, 150, 255] });
        sharpen(&mut img, Sharpening::High, 240);
        assert!(img.get(3, 4)[0] < 100 && img.get(4, 4)[0] > 150);
        let mut empty = Rgba8::new(0, 0);
        sharpen(&mut empty, Sharpening::High, 600);
    }
}
