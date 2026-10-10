//! Never-crash (P6.2): the PDF writer takes images (JPEG files, raw samples, ICC profiles) and
//! fonts (user-installed, any file) it did not make. Damaged ones are errors or skipped glyphs,
//! never a panic, and every PDF it writes is complete.

use std::sync::Arc;

use dac_pdf::{ColorSpace, Document, Image, Page, Paint, Path, Rect, jpeg_info};
use dac_text::{TextBlock, TextEngine};

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");

fn jpeg(w: u16, h: u16, n: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xEE, 0x00, 0x0E];
    v.extend(b"Adobe");
    v.extend([0, 100, 0, 0, 0, 0, 2]);
    v.extend([0xFF, 0xE0, 0x00, 0x10]);
    v.extend(b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
    v.extend([0xFF, 0xC0, 0x00, 8 + 3 * n, 8]);
    v.extend(h.to_be_bytes());
    v.extend(w.to_be_bytes());
    v.push(n);
    for c in 0..n {
        v.extend([c + 1, 0x11, 0]);
    }
    v.extend([0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x12, 0x34, 0xFF, 0xD9]);
    v
}

fn write_with_image(img: Image) {
    let mut doc = Document::new();
    if let Ok(id) = doc.add_image(img) {
        let mut page = Page::new(200.0, 100.0);
        page.image(id, Rect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 });
        if doc.push_page(page).is_ok()
            && let Ok(bytes) = doc.to_bytes()
        {
            assert!(bytes.starts_with(b"%PDF-") && bytes.trim_ascii_end().ends_with(b"%%EOF"));
        }
    }
}

#[test]
fn hostile_jpegs_never_panic() {
    let seeds = [jpeg(64, 48, 3), jpeg(1, 1, 1), jpeg(9, 9, 4)];
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    dac_fuzzkit::run("pdf.jpeg", &refs, 4000, |b| {
        let info = jpeg_info(b);
        let components = info.as_ref().map_or(3, |i| i.components as u8);
        for color in [ColorSpace::Rgb, ColorSpace::Gray, ColorSpace::Cmyk, ColorSpace::Icc { profile: Arc::new(b.to_vec()), components }] {
            write_with_image(Image::jpeg(b.to_vec(), color));
        }
    });
}

#[test]
fn mismatched_samples_and_alpha_never_panic() {
    let mut rng = dac_fuzzkit::Rng::new(11);
    for _ in 0..2000 {
        let (w, h) = (rng.below(5) as u32 * [1, 1000, 1 << 20][rng.below(3)], rng.below(5) as u32);
        let len = rng.below(64);
        let color = [
            ColorSpace::Gray,
            ColorSpace::Rgb,
            ColorSpace::Cmyk,
            ColorSpace::Icc { profile: Arc::new(vec![0; rng.below(200)]), components: rng.byte() },
        ][rng.below(4)]
        .clone();
        let mut img = if rng.chance(2) { Image::samples8(w, h, color, vec![7; len]) } else { Image::samples16(w, h, color, &vec![7; len]) };
        if rng.chance(2) {
            img.alpha = Some(vec![255; rng.below(64)]);
        }
        write_with_image(img);
    }
}

#[test]
fn hostile_fonts_never_panic() {
    // a damaged font file in the user's fonts: registered (or refused), laid out, embedded
    let text = "Hamburgefonstiv ÄÖÜ ﬁ → 123 漢字";
    dac_fuzzkit::run("pdf.font", &[INTER], 120, |b| {
        let mut e = TextEngine::new();
        let names = e.fonts.register_font_data(b.to_vec());
        let family = names.first().cloned().unwrap_or_else(|| "Inter".into());
        let layout = e.layout(&TextBlock::plain(text, &family, 24.0), 72.0);
        let mut page = Page::new(300.0, 100.0);
        page.text(&layout, text, (10.0, 50.0), 1.0);
        page.fill(Path::rect(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }), Paint::Gray(0.5));
        let mut doc = Document::new();
        if doc.push_page(page).is_ok()
            && let Ok(bytes) = doc.to_bytes()
        {
            assert!(bytes.trim_ascii_end().ends_with(b"%%EOF"));
        }
    });
}
