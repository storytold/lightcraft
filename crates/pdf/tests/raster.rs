//! Golden raster tests (Phase 3 gate): documents written by `lcc-pdf` are rasterised with
//! `hayro` (pure Rust, MIT/Apache-2.0, dev-only) and compared with expectations kept in code —
//! pixel colours at known spots and ink statistics, never committed image files. This proves a
//! real PDF consumer can draw what we write: vector paint, Flate images (8/16-bit, soft mask) at
//! their placed size and orientation, and text through the subset-embedded font.

use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use lcc_pdf::{ColorSpace, Document, Image, Page, Paint, Path, Rect};

/// RGBA8 pixels of each page at `scale` px per pt.
struct Raster {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Raster {
    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2]]
    }

    /// Dark pixels (luma < 128) inside `[x0, x1) × [y0, y1)`, and their bounding box.
    fn ink(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (usize, Option<(usize, usize, usize, usize)>) {
        let mut n = 0;
        let mut bb: Option<(usize, usize, usize, usize)> = None;
        for y in y0..y1.min(self.h) {
            for x in x0..x1.min(self.w) {
                let [r, g, b] = self.at(x, y);
                if u32::from(r) * 3 + u32::from(g) * 6 + u32::from(b) < 128 * 10 {
                    n += 1;
                    bb = Some(match bb {
                        None => (x, y, x, y),
                        Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                    });
                }
            }
        }
        (n, bb)
    }
}

fn rasterise(bytes: Vec<u8>, scale: f32) -> Vec<Raster> {
    let pdf = Pdf::new(Arc::new(bytes)).expect("hayro parses our PDF");
    hayro::render_pdf(&pdf, scale, InterpreterSettings::default(), None)
        .expect("rendered")
        .into_iter()
        .map(|p| Raster { w: usize::from(p.width()), h: usize::from(p.height()), px: p.data_as_u8_slice().to_vec() })
        .collect()
}

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[test]
fn vector_images_and_text_rasterise_as_laid_out() {
    let mut d = Document::new();
    // 2×2 RGB image: red, green / blue, white — placed 100×100 pt, top-left origin.
    let quad = d.add_image(Image::samples8(2, 2, ColorSpace::Rgb, vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])).unwrap();
    // 16-bit gray ramp black → white, 1 row of 2.
    let gray = d.add_image(Image::samples16(2, 1, ColorSpace::Gray, &[0, 0xFFFF])).unwrap();
    // Fully transparent soft-masked black square: must not paint.
    let mut ghost = Image::samples8(1, 1, ColorSpace::Rgb, vec![0, 0, 0]);
    ghost.alpha = Some(vec![0]);
    let ghost = d.add_image(ghost).unwrap();

    let mut e = lcc_text::TextEngine::new();
    let s = "Golden HIM";
    let layout = e.layout(&lcc_text::TextBlock::plain(s, "Inter", 40.0), 72.0);

    let mut p = Page::new(300.0, 200.0);
    p.fill(Path::rect(Rect::new(10.0, 10.0, 60.0, 40.0)), Paint::Rgb(0.0, 0.0, 1.0));
    p.image(quad, Rect::new(100.0, 10.0, 100.0, 100.0));
    p.image(gray, Rect::new(210.0, 10.0, 80.0, 20.0));
    p.image(ghost, Rect::new(210.0, 60.0, 40.0, 40.0));
    p.text(&layout, s, (10.0, 150.0), 1.0);
    d.push_page(p).unwrap();
    // Second page, different size: page sizes must survive.
    let mut p2 = Page::new(100.0, 50.0);
    p2.fill(Path::rect(Rect::new(0.0, 0.0, 100.0, 50.0)), Paint::Rgb(0.0, 0.0, 0.0));
    d.push_page(p2).unwrap();

    let pages = rasterise(d.to_bytes().unwrap(), 2.0);
    assert_eq!(pages.len(), 2);
    let r = &pages[0];
    assert_eq!((r.w, r.h), (600, 400));
    assert_eq!((pages[1].w, pages[1].h), (200, 100));
    assert!(near(pages[1].at(100, 50), [0, 0, 0], 2));

    // Blue rectangle at (10,10)–(70,50) pt, y measured from the top.
    assert!(near(r.at(80, 60), [0, 0, 255], 2), "{:?}", r.at(80, 60));
    assert!(near(r.at(150, 60), [255, 255, 255], 2), "outside the rectangle");
    // Image quadrants keep their orientation (red top-left, white bottom-right).
    assert!(near(r.at(240, 60), [255, 0, 0], 3), "{:?}", r.at(240, 60));
    assert!(near(r.at(360, 60), [0, 255, 0], 3), "{:?}", r.at(360, 60));
    assert!(near(r.at(240, 180), [0, 0, 255], 3), "{:?}", r.at(240, 180));
    assert!(near(r.at(360, 180), [255, 255, 255], 3), "{:?}", r.at(360, 180));
    // 16-bit gray: black on the left, white on the right.
    assert!(r.at(430, 40)[0] < 40 && r.at(570, 40)[0] > 215, "{:?} {:?}", r.at(430, 40), r.at(570, 40));
    // The transparent image leaves the page white.
    assert!(near(r.at(460, 160), [255, 255, 255], 2), "{:?}", r.at(460, 160));

    // Text: ink is present where the layout put it and spans its advance width.
    let (n, bb) = r.ink(0, 230, 600, 400);
    let (x0, y0, x1, y1) = bb.expect("text was drawn with the embedded font");
    let width_px = f64::from(layout.lines[0].x1 - layout.lines[0].x0) * 2.0;
    assert!((20..30).contains(&x0), "text starts at the origin: {x0}");
    assert!(((x1 - x0) as f64 - width_px).abs() < width_px * 0.1, "ink width {} vs layout {width_px}", x1 - x0);
    // Cap height of Inter at 40 pt ≈ 29 pt → ≈ 58 px tall ink.
    assert!((50..=70).contains(&(y1 - y0)), "ink height {} bb {:?} n {n}", y1 - y0, bb);
    // Ink coverage of ten glyphs at 80 px: a few thousand dark pixels, not a filled box.
    let bbox = (x1 - x0 + 1) * (y1 - y0 + 1);
    assert!(n > bbox / 8 && n < bbox * 2 / 3, "{n} dark pixels in a {bbox} px box");
}

#[test]
fn cjk_text_rasterises_with_craft_fonts() {
    if lcc_text::craft_fonts::install_from_env() == 0 {
        eprintln!("CRAFT_FONTS_DIR unset: skipping CJK raster test");
        return;
    }
    let mut e = lcc_text::TextEngine::new();
    let s = "写真の本";
    let layout = e.layout(&lcc_text::TextBlock::plain(s, "Inter", 40.0), 72.0);
    let mut d = Document::new();
    let mut p = Page::new(300.0, 100.0);
    p.text(&layout, s, (10.0, 20.0), 1.0);
    d.push_page(p).unwrap();
    let pages = rasterise(d.to_bytes().unwrap(), 1.0);
    let (n, bb) = pages[0].ink(0, 0, 300, 100);
    let (x0, _, x1, _) = bb.expect("CJK glyphs drawn");
    assert!((x1 - x0) as f32 > (layout.lines[0].x1 - layout.lines[0].x0) * 0.8, "four ideographs span the line");
    assert!(n > 200, "{n} dark pixels");
}
