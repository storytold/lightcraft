//! Golden PDF test for book export (Phase 3 gate): the written PDF is parsed back with `lopdf`
//! (valid cross-reference table, page count, page/trim boxes, embedded image resolution, subset
//! font with every drawn character mapped) and rasterised with `hayro` (pure Rust,
//! MIT/Apache-2.0, dev-only) against expectations kept in code — no committed images.

use std::sync::Arc;

use lcc_book::{Book, BookSize, CellContent, CoverType};
use lcc_layout::render::PhotoSource;
use lcc_raster::Rgba8;
use lcc_text::TextEngine;
use lopdf::{Dictionary, Object};

/// Every photo: left half red, right half blue.
struct Halves;
impl PhotoSource for Halves {
    fn image(&self, _: &str, _: usize) -> Result<Rgba8, String> {
        Ok(Rgba8::from_fn(64, 48, |x, _| if x < 32 { [220, 20, 20, 255] } else { [20, 20, 220, 255] }))
    }
}

fn deref<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Object {
    match o {
        Object::Reference(id) => doc.get_object(*id).unwrap(),
        o => o,
    }
}

fn dict<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Dictionary {
    match deref(doc, o) {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        o => panic!("not a dictionary: {o:?}"),
    }
}

fn nums(doc: &lopdf::Document, o: &Object) -> Vec<f64> {
    match deref(doc, o) {
        Object::Array(a) => a.iter().map(|v| v.as_float().map(f64::from).or_else(|_| v.as_i64().map(|i| i as f64)).unwrap()).collect(),
        o => panic!("not an array: {o:?}"),
    }
}

fn book() -> Book {
    let mut b = Book { name: "Golden book".into(), ..Book::default() };
    b.settings.resolution = 72;
    b.settings.size = BookSize::SmallSquare;
    b.settings.cover = CoverType::Hardcover;
    lcc_book::auto::auto_layout(&mut b, &["a".into(), "b".into(), "c".into()], &lcc_book::auto::builtin_presets()[2]).unwrap();
    if let CellContent::Text { text, .. } = &mut b.front.cells[1].content {
        *text = "Summer Höhe".into();
    }
    b
}

#[test]
fn book_pdf_is_structurally_valid_with_embedded_fonts_and_images() {
    let b = book();
    let mut e = TextEngine::new();
    let bytes = lcc_book::export::export_pdf(&b, &Halves, &mut e, &mut |_, _| true).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).expect("valid xref and trailer");
    // xref: every in-use object listed resolves, and /Size covers the highest id.
    let size = doc.trailer.get(b"Size").unwrap().as_i64().unwrap() as u32;
    assert!(doc.objects.keys().all(|(id, _)| *id < size), "trailer /Size {size}");
    assert!(doc.reference_table.entries.len() >= doc.objects.len().saturating_sub(1), "xref covers the objects");

    let pages = doc.get_pages();
    assert_eq!(pages.len(), b.pages.len() + 2, "front and back cover plus interior pages");
    let trim = b.trim();
    let bleed = f64::from(b.settings.bleed);
    let (pw, ph) = (f64::from(trim.w) + 2.0 * bleed, f64::from(trim.h) + 2.0 * bleed);
    assert!((f64::from(trim.w) - 504.0).abs() < 0.01, "7 in square");
    let mut saw_font = false;
    for (_, id) in pages {
        let pd = doc.get_dictionary(id).unwrap();
        let mb = nums(&doc, pd.get(b"MediaBox").unwrap());
        assert!((mb[2] - pw).abs() < 0.01 && (mb[3] - ph).abs() < 0.01, "{mb:?}");
        let tb = nums(&doc, pd.get(b"TrimBox").unwrap());
        assert!((tb[0] - bleed).abs() < 0.01 && (tb[2] - (pw - bleed)).abs() < 0.01, "{tb:?}");
        // One full-bleed JPEG at the book's resolution (72 ppi → 1 px per pt).
        let res = dict(&doc, pd.get(b"Resources").unwrap());
        let xo = dict(&doc, res.get(b"XObject").unwrap());
        assert_eq!(xo.len(), 1);
        let (_, im) = xo.iter().next().unwrap();
        let im = dict(&doc, im);
        assert_eq!(im.get(b"Filter").unwrap().as_name().unwrap(), b"DCTDecode");
        let w = im.get(b"Width").unwrap().as_i64().unwrap() as f64;
        let h = im.get(b"Height").unwrap().as_i64().unwrap() as f64;
        assert!((w - pw).abs() <= 1.0 && (h - ph).abs() <= 1.0, "image {w}×{h} for a {pw}×{ph} pt page at 72 ppi");
        if let Ok(fonts) = res.get(b"Font") {
            for (_, f) in dict(&doc, fonts).iter() {
                let f = dict(&doc, f);
                let base = f.get(b"BaseFont").unwrap().as_name().unwrap();
                assert_eq!(base.get(6), Some(&b'+'), "subset tag on {}", String::from_utf8_lossy(base));
                let cmap = match deref(&doc, f.get(b"ToUnicode").unwrap()) {
                    Object::Stream(s) => String::from_utf8_lossy(&s.decompressed_content().unwrap()).into_owned(),
                    o => panic!("{o:?}"),
                };
                // Every character drawn on the cover has a glyph mapped back to it.
                for ch in "SumerHöh".chars() {
                    let hex = format!("<{:04X}>", ch as u32);
                    assert!(cmap.to_uppercase().contains(&hex), "{ch} missing from ToUnicode");
                }
                let desc = match deref(&doc, f.get(b"DescendantFonts").unwrap()) {
                    Object::Array(a) => dict(&doc, &a[0]).clone(),
                    o => panic!("{o:?}"),
                };
                let fd = dict(&doc, desc.get(b"FontDescriptor").unwrap());
                assert!(fd.get(b"FontFile2").is_ok() || fd.get(b"FontFile3").is_ok(), "font program embedded");
                saw_font = true;
            }
        }
    }
    assert!(saw_font, "cover title embedded as a font");

    // Raster: an interior page shows the photos' red and blue halves in about equal parts.
    let pdf = hayro::hayro_syntax::Pdf::new(Arc::new(bytes)).unwrap();
    let rendered = hayro::render_pdf(&pdf, 0.5, hayro::hayro_interpret::InterpreterSettings::default(), Some(1..=1)).unwrap();
    let p = &rendered[0];
    let px = p.data_as_u8_slice();
    let (mut red, mut blue) = (0usize, 0usize);
    for c in px.as_chunks::<4>().0 {
        if c[0] > 160 && c[2] < 80 {
            red += 1;
        } else if c[2] > 160 && c[0] < 80 {
            blue += 1;
        }
    }
    let total = usize::from(p.width()) * usize::from(p.height());
    assert!(red > total / 20 && blue > total / 20, "red {red}, blue {blue} of {total}");
    assert!(red.abs_diff(blue) < (red + blue) / 4, "halves balanced: red {red}, blue {blue}");
}
