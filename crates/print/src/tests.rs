//! End to end: settings → pages → PDF / JPEG.

use dac_layout::render::PhotoSource;
use dac_layout::tokens::PhotoInfo;
use dac_raster::Rgba8;

use crate::job::builtin_templates;
use crate::output::{PdfOptions, to_jpeg, to_pdf};
use crate::text::ShapedText;
use crate::{LayoutStyle, PrintSettings};

struct Gradient;
impl PhotoSource for Gradient {
    fn image(&self, photo: &str, long: usize) -> Result<Rgba8, String> {
        if photo == "missing" {
            return Err("not found".into());
        }
        let long = long.clamp(8, 600);
        Ok(Rgba8::from_fn(long, long * 2 / 3, |x, y| [(x * 255 / long) as u8, (y * 255 / long) as u8, 128, 255]))
    }
    fn info(&self, photo: &str) -> PhotoInfo {
        PhotoInfo { filename: format!("{photo}.dng"), title: "Ünïcödé 写真".into(), ..PhotoInfo::default() }
    }
}

fn template(name: &str) -> PrintSettings {
    builtin_templates().into_iter().find(|(n, _)| n == name).map(|(_, s)| s).unwrap_or_default()
}

fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("IMG_{i:04}")).collect()
}

fn pdf_of(s: &PrintSettings, photos: &[String]) -> (lopdf::Document, Vec<u8>, Vec<String>) {
    let doc = s.build(photos).unwrap();
    let out = to_pdf(&doc, s, &Gradient, &ShapedText::new(), &PdfOptions::default(), &mut |_, _| true).unwrap();
    let bytes = out.files.into_iter().next().unwrap();
    (lopdf::Document::load_mem(&bytes).unwrap(), bytes, out.warnings)
}

fn has_embedded_font(pdf: &lopdf::Document) -> bool {
    pdf.objects.values().any(|o| o.as_dict().is_ok_and(|d| d.has(b"FontFile2") || d.has(b"FontFile3")))
}

#[test]
fn contact_sheet_2x2_to_pdf_with_fonts_and_marks() {
    let mut s = template("2×2 Cells");
    s.options.photo_info = Some("{Filename}".into());
    s.options.page_numbers = true;
    s.options.page_info = true;
    s.options.crop_marks = true;
    let (pdf, _, warnings) = pdf_of(&s, &ids(6));
    assert!(warnings.is_empty(), "{warnings:?}");
    let pages = pdf.get_pages();
    assert_eq!(pages.len(), 2);
    assert!(has_embedded_font(&pdf));
    let first = pdf.get_object(*pages.get(&1).unwrap()).unwrap().as_dict().unwrap();
    let media = first.get(b"MediaBox").unwrap().as_array().unwrap();
    // crop marks need room outside the trim: the media box is larger than Letter
    assert!(media[2].as_float().unwrap() > 612.0 + 40.0);
    assert!(first.has(b"TrimBox"));
    let text: String = pdf.extract_text(&[1]).unwrap().chars().filter(|c| !c.is_whitespace()).collect();
    assert!(text.contains("IMG_0000.dng"), "{text}");
    assert!(text.contains("Page1of2"), "{text}");
}

#[test]
fn picture_package_and_custom_package_to_pdf() {
    let (pdf, _, _) = pdf_of(&template("(1) 4×6, (6) 2×3"), &ids(2));
    assert!(pdf.get_pages().len() >= 2);
    let mut custom = template("Custom Centered");
    custom.options.identity_plate = Some(crate::job::IdentityPlate { text: "Studio 東京".into(), ..Default::default() });
    let (pdf, bytes, _) = pdf_of(&custom, &ids(3));
    assert_eq!(pdf.get_pages().len(), 1);
    assert!(has_embedded_font(&pdf));
    // one image object per distinct photo and size
    let images = pdf
        .objects
        .values()
        .filter(|o| o.as_stream().is_ok_and(|s| s.dict.get(b"Subtype").is_ok_and(|v| v.as_name().is_ok_and(|n| n == b"Image"))))
        .count();
    assert_eq!(images, 3);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn missing_photos_warn_and_cancel_stops() {
    let s = template("2×2 Cells");
    let doc = s.build(&["missing".to_string(), "IMG_1".to_string()]).unwrap();
    let out = to_pdf(&doc, &s, &Gradient, &ShapedText::new(), &PdfOptions::default(), &mut |_, _| true).unwrap();
    assert_eq!(out.warnings.len(), 1);
    assert_eq!(to_pdf(&doc, &s, &Gradient, &ShapedText::new(), &PdfOptions::default(), &mut |_, _| false).unwrap_err(), crate::PrintError::Cancelled);
}

#[test]
fn jpeg_pages_at_print_resolution() {
    let mut s = template("4×5 Contact Sheet");
    s.job.dpi = 50.0;
    let doc = s.build(&ids(21)).unwrap();
    let out = to_jpeg(&doc, &s, &Gradient, &ShapedText::new(), &mut |_, _| true).unwrap();
    assert_eq!(out.files.len(), 2);
    let info = dac_pdf::jpeg_info(&out.files[0]).unwrap();
    assert_eq!((info.width, info.height), (425, 550)); // 8.5×11 in at 50 ppi
}

#[test]
fn single_image_grid_style_survives_round_trip() {
    let s = PrintSettings { layout: LayoutStyle::SingleImage { grid: dac_layout::grid::Grid::new(3, 2) }, ..PrintSettings::default() };
    let back = PrintSettings::from_json(&s.to_json().unwrap()).unwrap();
    assert_eq!(back.build(&ids(7)).unwrap().pages.len(), 2);
}
