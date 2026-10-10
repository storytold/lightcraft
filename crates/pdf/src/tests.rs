//! Golden structural tests: documents are written, parsed back with `lopdf`, and their objects
//! checked; embedded font subsets are parsed with `skrifa` and compared with the source fonts.

use std::sync::Arc;

use lopdf::{Dictionary, Object};
use skrifa::raw::TableProvider;
use skrifa::{GlyphId, MetadataProvider};

use super::*;

fn parse(bytes: &[u8]) -> lopdf::Document {
    lopdf::Document::load_mem(bytes).unwrap()
}

fn deref<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Object {
    match o {
        Object::Reference(r) => doc.get_object(*r).unwrap(),
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

fn stream<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a lopdf::Stream {
    match deref(doc, o) {
        Object::Stream(s) => s,
        o => panic!("not a stream: {o:?}"),
    }
}

fn nums(doc: &lopdf::Document, o: &Object) -> Vec<f64> {
    match deref(doc, o) {
        Object::Array(a) => a
            .iter()
            .map(|v| match v {
                Object::Integer(i) => *i as f64,
                Object::Real(r) => f64::from(*r),
                o => panic!("not a number: {o:?}"),
            })
            .collect(),
        o => panic!("not an array: {o:?}"),
    }
}

fn name(o: &Object) -> &[u8] {
    match o {
        Object::Name(n) => n,
        o => panic!("not a name: {o:?}"),
    }
}

fn text(o: &Object) -> String {
    match o {
        Object::String(s, _) => {
            if s.starts_with(&[0xFE, 0xFF]) {
                let u: Vec<u16> = s[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&u)
            } else {
                String::from_utf8_lossy(s).into_owned()
            }
        }
        o => panic!("not a string: {o:?}"),
    }
}

fn close(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
}

/// A minimal JPEG header (SOI, optional Adobe APP14, SOF0, EOI): enough for passthrough.
fn fake_jpeg(w: u16, h: u16, n: u8, adobe: bool) -> Vec<u8> {
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

/// Not a real colour profile: the writer passes ICC bytes through untouched.
fn fake_icc(tag: &[u8; 4]) -> Arc<Vec<u8>> {
    let mut v = vec![0u8; 132];
    v[12..16].copy_from_slice(tag);
    v[36..40].copy_from_slice(b"acsp");
    Arc::new(v)
}

fn laid_out(e: &mut dac_text::TextEngine, s: &str, size: f32) -> dac_text::TextLayout {
    e.layout(&dac_text::TextBlock::plain(s, "Inter", size), 72.0)
}

fn content(doc: &lopdf::Document, page: lopdf::ObjectId) -> String {
    String::from_utf8_lossy(&doc.get_page_content(page)).into_owned()
}

#[test]
fn multi_page_document_round_trips() {
    let mut d = Document::new();
    d.metadata = Metadata {
        title: Some("Contact sheet — été".into()),
        author: Some("A. Photographer".into()),
        subject: Some("Test".into()),
        keywords: vec!["one".into(), "two & three".into()],
        creator: Some("the app".into()),
        producer: Some("dac-pdf".into()),
        created: Some(DateTime { year: 2026, month: 10, day: 10, hour: 9, minute: 30, second: 0 }),
        modified: None,
    };
    let srgb = fake_icc(b"mntr");
    d.output_intent = Some(OutputIntent {
        subtype: "GTS_PDFX".into(),
        identifier: "FOGRA39".into(),
        info: Some("Coated FOGRA39".into()),
        profile: fake_icc(b"prtr"),
        components: 4,
    });
    let jpeg = d.add_image(Image::jpeg(fake_jpeg(64, 48, 3, false), ColorSpace::Icc { profile: srgb.clone(), components: 3 })).unwrap();
    let cmyk = d.add_image(Image::jpeg(fake_jpeg(8, 8, 4, true), ColorSpace::Cmyk)).unwrap();
    let mut rgba = Image::samples8(2, 2, ColorSpace::Icc { profile: srgb.clone(), components: 3 }, (0..12).collect());
    rgba.alpha = Some(vec![0, 85, 170, 255]);
    let flate8 = d.add_image(rgba).unwrap();
    let flate16 = d.add_image(Image::samples16(3, 1, ColorSpace::Gray, &[0, 0x8000, 0xFFFF])).unwrap();

    let mut e = dac_text::TextEngine::new();
    for i in 0..3 {
        let mut p = Page::new(612.0, 792.0);
        p.bleed = Some(Rect::new(9.0, 9.0, 594.0, 774.0));
        p.trim = Some(Rect::new(18.0, 18.0, 576.0, 756.0));
        p.art = Some(Rect::new(36.0, 36.0, 540.0, 720.0));
        p.image(jpeg, Rect::new(36.0, 36.0, 320.0, 240.0));
        p.image(flate8, Rect::new(400.0, 36.0, 100.0, 100.0));
        if i == 1 {
            p.image(cmyk, Rect::new(36.0, 300.0, 50.0, 50.0));
            p.image(flate16, Rect::new(36.0, 400.0, 90.0, 30.0));
        }
        p.stroke(
            Path::rect(Rect::new(36.0, 36.0, 320.0, 240.0)),
            Stroke { paint: Paint::Rgb(0.5, 0.5, 0.5), width: 0.5, dash: Some((vec![3.0, 2.0], 0.0)) },
        );
        p.crop_marks(Rect::new(18.0, 18.0, 576.0, 756.0), 6.0, 12.0, 0.25);
        p.registration_mark(306.0, 9.0, 12.0, 0.25);
        let s = format!("Page {} — Hello, Wörld", i + 1);
        p.text(&laid_out(&mut e, &s, 24.0), &s, (36.0, 700.0), 1.0);
        d.push_page(p).unwrap();
    }
    let bytes = d.to_bytes().unwrap();
    assert!(bytes.starts_with(b"%PDF-1.7"));
    let doc = parse(&bytes);
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 3);

    // Boxes are flipped to PDF's bottom-left origin.
    let p1 = *pages.get(&1).unwrap();
    let pd = doc.get_dictionary(p1).unwrap();
    assert!(close(&nums(&doc, pd.get(b"MediaBox").unwrap()), &[0.0, 0.0, 612.0, 792.0]));
    assert!(close(&nums(&doc, pd.get(b"BleedBox").unwrap()), &[9.0, 9.0, 603.0, 783.0]));
    assert!(close(&nums(&doc, pd.get(b"TrimBox").unwrap()), &[18.0, 18.0, 594.0, 774.0]));
    assert!(close(&nums(&doc, pd.get(b"ArtBox").unwrap()), &[36.0, 36.0, 576.0, 756.0]));

    // Images: JPEG passthrough with its ICC space, Flate 8-bit with soft mask, 16-bit gray.
    let res = dict(&doc, pd.get(b"Resources").unwrap());
    let xo = dict(&doc, res.get(b"XObject").unwrap());
    let im0 = stream(&doc, xo.get(b"Im0").unwrap());
    assert_eq!(name(im0.dict.get(b"Filter").unwrap()), b"DCTDecode");
    assert_eq!(im0.content, fake_jpeg(64, 48, 3, false), "JPEG bytes pass through unchanged");
    assert_eq!(im0.dict.get(b"Width").unwrap().as_i64().unwrap(), 64);
    let cs = match deref(&doc, im0.dict.get(b"ColorSpace").unwrap()) {
        Object::Array(a) => a.clone(),
        o => panic!("{o:?}"),
    };
    assert_eq!(name(&cs[0]), b"ICCBased");
    let icc = stream(&doc, &cs[1]);
    assert_eq!(icc.dict.get(b"N").unwrap().as_i64().unwrap(), 3);
    assert_eq!(icc.decompressed_content().unwrap(), *srgb);
    let im2 = stream(&doc, xo.get(b"Im2").unwrap());
    assert_eq!(name(im2.dict.get(b"Filter").unwrap()), b"FlateDecode");
    assert_eq!(im2.decompressed_content().unwrap(), (0..12).collect::<Vec<u8>>());
    // Same ICC profile written once.
    let cs2 = match deref(&doc, im2.dict.get(b"ColorSpace").unwrap()) {
        Object::Array(a) => a.clone(),
        o => panic!("{o:?}"),
    };
    assert_eq!(cs2[1], cs[1]);
    let mask = stream(&doc, im2.dict.get(b"SMask").unwrap());
    assert_eq!(mask.decompressed_content().unwrap(), vec![0, 85, 170, 255]);
    assert_eq!(name(mask.dict.get(b"ColorSpace").unwrap()), b"DeviceGray");
    let p2 = doc.get_dictionary(*pages.get(&2).unwrap()).unwrap();
    let xo2 = dict(&doc, dict(&doc, p2.get(b"Resources").unwrap()).get(b"XObject").unwrap());
    let im1 = stream(&doc, xo2.get(b"Im1").unwrap());
    assert!(close(&nums(&doc, im1.dict.get(b"Decode").unwrap()), &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]), "Adobe CMYK JPEGs are inverted");
    let im3 = stream(&doc, xo2.get(b"Im3").unwrap());
    assert_eq!(im3.dict.get(b"BitsPerComponent").unwrap().as_i64().unwrap(), 16);
    assert_eq!(im3.decompressed_content().unwrap(), vec![0, 0, 0x80, 0, 0xFF, 0xFF]);

    // Content: images placed (y flipped), dashed stroke, registration-colour marks, text.
    let c = content(&doc, p1);
    assert!(c.contains("320 0 0 240 36 516 cm"), "{c}");
    assert!(c.contains("/Im0 Do"));
    assert!(c.contains("[3 2] 0 d"));
    assert!(c.contains("1 1 1 1 K"), "registration colour");
    assert!(c.contains("Tf") && c.contains("Tj"));

    // Fonts: one subset Type0 font with an embedded TrueType program and ToUnicode.
    let fonts = dict(&doc, res.get(b"Font").unwrap());
    assert_eq!(fonts.len(), 1);
    let f0 = dict(&doc, fonts.get(b"F0").unwrap());
    assert_eq!(name(f0.get(b"Subtype").unwrap()), b"Type0");
    assert_eq!(name(f0.get(b"Encoding").unwrap()), b"Identity-H");
    let base = name(f0.get(b"BaseFont").unwrap());
    assert!(base.len() > 7 && base[6] == b'+' && base[..6].iter().all(u8::is_ascii_uppercase), "{}", String::from_utf8_lossy(base));
    let cmap = String::from_utf8_lossy(&stream(&doc, f0.get(b"ToUnicode").unwrap()).decompressed_content().unwrap()).into_owned();
    assert!(cmap.contains("beginbfchar") || cmap.contains("beginbfrange"), "{cmap}");
    let desc = match deref(&doc, f0.get(b"DescendantFonts").unwrap()) {
        Object::Array(a) => dict(&doc, &a[0]).clone(),
        o => panic!("{o:?}"),
    };
    assert_eq!(name(desc.get(b"Subtype").unwrap()), b"CIDFontType2");
    let fd = dict(&doc, desc.get(b"FontDescriptor").unwrap());
    let file = stream(&doc, fd.get(b"FontFile2").unwrap()).decompressed_content().unwrap();
    assert!(file.len() < dac_text::fonts::INTER_REGULAR.len() / 4, "subset: {} bytes", file.len());

    // Output intent and metadata.
    let cat = doc.catalog().unwrap();
    let intents = match deref(&doc, cat.get(b"OutputIntents").unwrap()) {
        Object::Array(a) => a.clone(),
        o => panic!("{o:?}"),
    };
    let oi = dict(&doc, &intents[0]);
    assert_eq!(name(oi.get(b"S").unwrap()), b"GTS_PDFX");
    assert_eq!(text(oi.get(b"OutputConditionIdentifier").unwrap()), "FOGRA39");
    let dest = stream(&doc, oi.get(b"DestOutputProfile").unwrap());
    assert_eq!(dest.dict.get(b"N").unwrap().as_i64().unwrap(), 4);
    let xmp = String::from_utf8_lossy(&stream(&doc, cat.get(b"Metadata").unwrap()).content).into_owned();
    assert!(xmp.contains("Contact sheet — été") && xmp.contains("two &amp; three") && xmp.contains("2026-10-10T09:30:00Z"), "{xmp}");
    let info = dict(&doc, doc.trailer.get(b"Info").unwrap());
    assert_eq!(text(info.get(b"Title").unwrap()), "Contact sheet — été");
    assert_eq!(text(info.get(b"Producer").unwrap()), "dac-pdf");
    assert!(text(info.get(b"CreationDate").unwrap()).starts_with("D:20261010093000"));

    if let Some(dir) = std::env::var_os("DAC_PDF_DUMP") {
        std::fs::write(std::path::Path::new(&dir).join("pages.pdf"), &bytes).unwrap();
    }
    // Deterministic output.
    assert_eq!(d.to_bytes().unwrap(), bytes);
}

/// The subset program keeps exactly the glyphs drawn (plus .notdef), each with the outline of
/// the source glyph, and ToUnicode maps them back to the text.
#[test]
fn font_subsetting_round_trip() {
    let mut e = dac_text::TextEngine::new();
    let s = "Subset fidelity: ÀÉÎõü 0123 ffi";
    let l = laid_out(&mut e, s, 30.0);
    let mut d = Document::new();
    let mut p = Page::new(595.0, 842.0);
    p.text(&l, s, (40.0, 100.0), 1.0);
    d.push_page(p).unwrap();
    let doc = parse(&d.to_bytes().unwrap());
    let page = *doc.get_pages().get(&1).unwrap();
    let res = dict(&doc, doc.get_dictionary(page).unwrap().get(b"Resources").unwrap());
    let f0 = dict(&doc, dict(&doc, res.get(b"Font").unwrap()).get(b"F0").unwrap());
    let desc = match deref(&doc, f0.get(b"DescendantFonts").unwrap()) {
        Object::Array(a) => dict(&doc, &a[0]).clone(),
        o => panic!("{o:?}"),
    };
    let fd = dict(&doc, desc.get(b"FontDescriptor").unwrap());
    let program = stream(&doc, fd.get(b"FontFile2").unwrap()).decompressed_content().unwrap();
    let sub = skrifa::FontRef::new(&program).unwrap();
    let orig = skrifa::FontRef::new(dac_text::fonts::INTER_REGULAR).unwrap();
    let used: std::collections::BTreeSet<u32> = l.glyphs.iter().map(|g| g.id).collect();
    // The glyphs drawn, .notdef, and the components of composite (accented) glyphs.
    let n = sub.maxp().map(|m| u32::from(m.num_glyphs())).unwrap();
    let all = orig.maxp().map(|m| u32::from(m.num_glyphs())).unwrap();
    assert!(n > used.len() as u32 && n < 2 * (used.len() as u32 + 1) && n < all / 20, "{n} of {all} glyphs for {} used", used.len());

    // ToUnicode: CID -> char; the subset glyph for that CID has the source glyph's outline.
    let cmap = String::from_utf8_lossy(&stream(&doc, f0.get(b"ToUnicode").unwrap()).decompressed_content().unwrap()).into_owned();
    let mut pairs = Vec::new();
    let mut in_chars = false;
    for line in cmap.lines() {
        if line.contains("beginbfchar") || line.contains("endbfchar") {
            in_chars = line.contains("begin");
            continue;
        }
        let parts: Vec<&str> = line.split(['<', '>']).filter(|t| !t.trim().is_empty()).collect();
        if in_chars
            && let [cid, uni] = parts[..]
            && cid.len() == 4
            && let (Ok(cid), Ok(u)) = (u16::from_str_radix(cid, 16), u32::from_str_radix(uni, 16))
            && let Some(ch) = char::from_u32(u)
        {
            pairs.push((cid, ch));
        }
    }
    assert!(pairs.len() >= 15, "{cmap}");
    for ch in "SubsetÀÉÎõü0123".chars() {
        assert!(pairs.iter().any(|p| p.1 == ch), "{ch} missing from ToUnicode");
    }
    let bounds = |f: &skrifa::FontRef, g: u32| {
        let o = f.outline_glyphs().get(GlyphId::new(g)).unwrap();
        let mut pen = Bounds::default();
        o.draw(skrifa::outline::DrawSettings::unhinted(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default()), &mut pen)
            .unwrap();
        pen.0
    };
    for (cid, ch) in &pairs {
        let src = orig.charmap().map(*ch).unwrap().to_u32();
        assert_eq!(bounds(&sub, u32::from(*cid)), bounds(&orig, src), "{ch}");
    }
    // Widths are written per CID in 1/1000 em.
    let w = match deref(&doc, desc.get(b"W").unwrap()) {
        Object::Array(a) => a.len(),
        _ => 0,
    };
    assert!(w >= 2, "W array");
}

#[derive(Default)]
struct Bounds([i32; 4]);

impl skrifa::outline::OutlinePen for Bounds {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
        self.add(x, y);
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, x: f32, y: f32) {
        self.add(x, y);
    }
    fn close(&mut self) {}
}

impl Bounds {
    fn add(&mut self, x: f32, y: f32) {
        let (x, y) = (x.round() as i32, y.round() as i32);
        self.0 = [self.0[0].min(x), self.0[1].min(y), self.0[2].max(x), self.0[3].max(y)];
    }
}

/// With craft-fonts (`CRAFT_FONTS_DIR`): a multi-page document with images and shaped CJK
/// text, horizontal and vertical, Japanese (TrueType) and Chinese (CFF) faces.
#[test]
fn cjk_multi_page_document_with_craft_fonts() {
    if dac_text::craft_fonts::install_from_env() == 0 {
        eprintln!("skipping: CRAFT_FONTS_DIR is not set to a craft-fonts checkout");
        return;
    }
    let installed = dac_text::craft_fonts::installed();
    let mut e = dac_text::TextEngine::new();
    let mut d = Document::new();
    let img = d.add_image(Image::samples8(4, 4, ColorSpace::Rgb, vec![200; 48])).unwrap();
    let mut families = Vec::new();
    for (i, (fam, sample)) in
        [("BIZ UDPGothic", "日本語の縦書き、カタカナ。"), ("Shippori Mincho", "明朝体のテキスト"), ("Noto Sans CJK SC", "简体中文排版测试")]
            .iter()
            .enumerate()
    {
        if !installed.iter().any(|f| f.family == *fam) {
            continue;
        }
        families.push(*fam);
        let mut p = Page::new(420.0, 595.0);
        p.image(img, Rect::new(20.0, 20.0, 100.0, 100.0));
        let mut b = dac_text::TextBlock::plain(*sample, *fam, 18.0);
        let l = e.layout(&b, 72.0);
        assert!(l.glyphs.iter().all(|g| g.id != 0), "{fam}: no .notdef");
        p.text(&l, sample, (20.0, 200.0), 1.0);
        b.orientation = dac_text::Orientation::Vertical;
        let v = e.layout(&b, 72.0);
        p.text(&v, sample, (380.0, 40.0), 1.0);
        d.push_page(p).unwrap();
        assert_eq!(d.page_count(), families.len(), "page {i}");
    }
    assert!(!families.is_empty());
    let bytes = d.to_bytes().unwrap();
    let doc = parse(&bytes);
    assert_eq!(doc.get_pages().len(), families.len());
    let mut subtypes = Vec::new();
    for (_, page) in doc.get_pages() {
        let res = dict(&doc, doc.get_dictionary(page).unwrap().get(b"Resources").unwrap());
        for (_, f) in dict(&doc, res.get(b"Font").unwrap()).iter() {
            let f = dict(&doc, f);
            let desc = match deref(&doc, f.get(b"DescendantFonts").unwrap()) {
                Object::Array(a) => dict(&doc, &a[0]).clone(),
                o => panic!("{o:?}"),
            };
            let st = name(desc.get(b"Subtype").unwrap()).to_vec();
            let fd = dict(&doc, desc.get(b"FontDescriptor").unwrap());
            let file = fd.get(b"FontFile2").or_else(|_| fd.get(b"FontFile3")).unwrap();
            let program = stream(&doc, file).decompressed_content().unwrap();
            assert!(program.len() < 400_000, "CJK fonts are subset: {} bytes", program.len());
            assert!(skrifa::FontRef::new(&program).is_ok());
            if st == b"CIDFontType0" {
                assert_eq!(name(stream(&doc, file).dict.get(b"Subtype").unwrap()), b"OpenType");
            }
            subtypes.push(st);
        }
    }
    assert!(!subtypes.is_empty());
    if families.contains(&"Noto Sans CJK SC") {
        assert!(subtypes.iter().any(|s| s == b"CIDFontType0"), "CFF font embedded as CIDFontType0");
    }
    // Whole document stays small (multi-megabyte CJK fonts subset to a few glyphs).
    assert!(bytes.len() < 1_000_000, "{} bytes", bytes.len());
    if let Some(dir) = std::env::var_os("DAC_PDF_DUMP") {
        std::fs::write(std::path::Path::new(&dir).join("cjk.pdf"), &bytes).unwrap();
    }
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    let mut d = Document::new();
    for (w, h) in [(0.0, 100.0), (f64::NAN, 100.0), (100.0, f64::INFINITY), (20_000.0, 100.0), (-5.0, -5.0)] {
        assert!(matches!(d.push_page(Page::new(w, h)), Err(PdfError::Page(_))));
    }
    let mut p = Page::new(100.0, 100.0);
    p.trim = Some(Rect::new(0.0, 0.0, f64::NAN, 1.0));
    assert!(d.push_page(p).is_err());
    let mut other = Document::new();
    let id = other.add_image(Image::samples8(1, 1, ColorSpace::Gray, vec![0])).unwrap();
    let mut p = Page::new(100.0, 100.0);
    p.image(id, Rect::new(0.0, 0.0, 10.0, 10.0));
    assert_eq!(d.push_page(p), Err(PdfError::UnknownImage(0)));
    assert!(d.add_image(Image::jpeg(b"not a jpeg".to_vec(), ColorSpace::Rgb)).is_err());
    // Non-finite geometry is dropped or clamped, never written as NaN.
    let mut p = Page::new(100.0, 100.0);
    p.fill(Path::new().move_to(f64::NAN, 1.0).line_to(f64::INFINITY, 2.0).close(), Paint::Rgb(f32::NAN, 2.0, -1.0));
    p.stroke(Path::line((0.0, 0.0), (1e300, 5.0)), Stroke { paint: Paint::BLACK, width: f64::NAN, dash: Some((vec![f64::NAN], f64::INFINITY)) });
    p.restore();
    p.restore();
    let mut e = dac_text::TextEngine::new();
    let l = laid_out(&mut e, "x", 12.0);
    p.text(&l, "x", (f64::NAN, 0.0), 1.0);
    p.text(&l, "x", (0.0, 0.0), -1.0);
    d.push_page(p).unwrap();
    let bytes = d.to_bytes().unwrap();
    let doc = parse(&bytes);
    let c = content(&doc, *doc.get_pages().get(&1).unwrap());
    assert!(!c.to_lowercase().contains("nan") && !c.contains("inf"), "{c}");
    // Empty documents are still valid PDF.
    assert_eq!(parse(&Document::new().to_bytes().unwrap()).get_pages().len(), 0);
}

#[test]
fn faces_that_cannot_be_embedded_are_drawn_as_outlines() {
    // A variable instance (non-default coordinates) is drawn as paths, no font resource.
    let mut e = dac_text::TextEngine::new();
    let mut l = laid_out(&mut e, "Ab", 20.0);
    for f in &mut l.faces {
        f.coords = vec![1];
    }
    let mut d = Document::new();
    let mut p = Page::new(200.0, 100.0);
    p.text(&l, "Ab", (10.0, 50.0), 1.0);
    d.push_page(p).unwrap();
    let doc = parse(&d.to_bytes().unwrap());
    let page = *doc.get_pages().get(&1).unwrap();
    let c = content(&doc, page);
    assert!(!c.contains("Tj") && c.contains(" c\n") && c.contains("f\n"), "{c}");
    let res = dict(&doc, doc.get_dictionary(page).unwrap().get(b"Resources").unwrap());
    assert!(res.get(b"Font").map(|f| dict(&doc, f).is_empty()).unwrap_or(true));
}
