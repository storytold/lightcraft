//! A book page as pixels: background colour, background photo (with opacity), graphic, photo
//! cells (through the shared `dac-layout` renderer, so fit/fill, zoom and pan match Print), then
//! text (photo text, text cells, page text, page number) and, in the editor, guides.
//!
//! Everything is placed in points and scaled by `dpi / 72`, so the editor preview and a 300 ppi
//! export show the same page.

use dac_layout::render::{PhotoSource, RenderOptions};
use dac_layout::tokens::{PhotoInfo, expand};
use dac_layout::{Cell, Fit, Page, PageLayout, PhotoCell, Rect};
use dac_raster::Rgba8;
use dac_text::TextEngine;

use crate::text::{Placed, layout_box};
use crate::{Book, BookError, BookPage, CellContent, Graphic, NumberPos, PageRef, PageTextPos, PhotoTextPos, Result, TEXT_SAFE, TypeStyle, VAlign};

/// Largest rendered page, pixels.
pub const MAX_PIXELS: usize = 400_000_000;

/// Render settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub dpi: f32,
    /// Include the bleed around the trim.
    pub bleed: bool,
    /// Editor overlay: the book's guides, filler text and empty photo slots.
    pub editor: bool,
    /// Draw the text (off for the PDF's raster layer, whose text is vector).
    pub text: bool,
    /// Highlight this cell (editor).
    pub selected: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options { dpi: 72.0, bleed: false, editor: false, text: true, selected: None }
    }
}

/// Filler shown in empty text cells while editing.
pub const FILLER: &str = "Add text here. Type or paste a story, a caption or a quote; the Type panel sets its font, size, colour and columns.";

/// A text box on a page: its rectangle in trim points, the expanded text and its style.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    pub rect: Rect,
    pub text: String,
    pub style: TypeStyle,
    /// Editor filler (not exported).
    pub filler: bool,
}

/// The 1-based number a page shows, if it shows one.
pub fn page_number(book: &Book, r: PageRef) -> Option<usize> {
    match r {
        PageRef::Page(i) if book.numbers.show && !book.pages.get(i).is_some_and(|p| p.hide_number) => Some(i + 1),
        _ => None,
    }
}

fn info(photos: &dyn PhotoSource, id: Option<&str>, page: usize, pages: usize) -> PhotoInfo {
    let mut i = id.map(|id| photos.info(id)).unwrap_or_default();
    i.page = page;
    i.pages = pages;
    i
}

/// Every text box of a page (trim coordinates), tokens expanded.
pub fn text_items(book: &Book, r: PageRef, photos: &dyn PhotoSource, editor: bool) -> Vec<TextItem> {
    let Some(page) = book.page(r) else { return Vec::new() };
    let trim = book.trim();
    let (num, pages) = (
        match r {
            PageRef::Page(i) => i + 1,
            _ => 0,
        },
        book.pages.len(),
    );
    let first_photo = page.photos().next();
    let mut out = Vec::new();
    for c in &page.cells {
        let rect = c.rect.to_points(trim).inset(&c.padding.insets());
        match &c.content {
            CellContent::Text { text, style } => {
                let t = expand(text, &info(photos, first_photo, num, pages));
                if t.trim().is_empty() {
                    if editor && book.guides.show && book.guides.filler_text {
                        out.push(TextItem { rect, text: FILLER.into(), style: TypeStyle { color: [150, 150, 150], ..style.clone() }, filler: true });
                    }
                } else {
                    out.push(TextItem { rect, text: t, style: style.clone(), filler: false });
                }
            }
            CellContent::Photo { photo, text: Some(pt), .. } => {
                let t = expand(&pt.text, &info(photos, photo.as_deref(), num, pages));
                if t.trim().is_empty() {
                    continue;
                }
                let h = (pt.style.leading.unwrap_or(pt.style.size * 1.2) * 2.0 + 2.0).max(1.0);
                let tr = match pt.position {
                    PhotoTextPos::Below => Rect::new(rect.x, rect.bottom() + pt.offset, rect.w, h),
                    PhotoTextPos::Above => Rect::new(rect.x, rect.y - pt.offset - h, rect.w, h),
                    PhotoTextPos::Over => Rect::new(rect.x + 6.0, rect.bottom() - pt.offset - h, (rect.w - 12.0).max(1.0), h),
                };
                let style =
                    if pt.position == PhotoTextPos::Above { TypeStyle { valign: VAlign::Bottom, ..pt.style.clone() } } else { pt.style.clone() };
                out.push(TextItem { rect: tr, text: t, style, filler: false });
            }
            CellContent::Photo { .. } => {}
        }
    }
    if let Some(pt) = &page.text {
        let t = expand(&pt.text, &info(photos, first_photo, num, pages));
        if !t.trim().is_empty() {
            let h = pt.style.leading.unwrap_or(pt.style.size * 1.2) * 3.0;
            let y = match pt.position {
                PageTextPos::Top => pt.offset,
                PageTextPos::Bottom => trim.h - pt.offset - h,
            };
            out.push(TextItem {
                rect: Rect::new(TEXT_SAFE, y, (trim.w - 2.0 * TEXT_SAFE).max(1.0), h),
                text: t,
                style: pt.style.clone(),
                filler: false,
            });
        }
    }
    if let (Some(n), PageRef::Page(i)) = (page_number(book, r), r) {
        let s = &book.numbers;
        let h = s.style.size * 1.4;
        let w = s.style.size * 4.0;
        let right = i % 2 == 0; // page 1 is a right-hand page
        let o = s.offset;
        let (x, y, align) = match s.position {
            NumberPos::TopCorner => (if right { trim.w - o - w } else { o }, o, if right { crate::HAlign::Right } else { crate::HAlign::Left }),
            NumberPos::BottomCorner => {
                (if right { trim.w - o - w } else { o }, trim.h - o - h, if right { crate::HAlign::Right } else { crate::HAlign::Left })
            }
            NumberPos::TopCenter => ((trim.w - w) / 2.0, o, crate::HAlign::Center),
            NumberPos::BottomCenter => ((trim.w - w) / 2.0, trim.h - o - h, crate::HAlign::Center),
            NumberPos::Side => {
                (if right { trim.w - o - w } else { o }, (trim.h - h) / 2.0, if right { crate::HAlign::Right } else { crate::HAlign::Left })
            }
        };
        out.push(TextItem { rect: Rect::new(x, y, w, h), text: n.to_string(), style: TypeStyle { align, ..s.style.clone() }, filler: false });
    }
    out
}

/// Lays a text item out at `dpi`.
pub fn layout_item(engine: &mut TextEngine, item: &TextItem, dpi: f32) -> Vec<Placed> {
    layout_box(engine, &item.text, &item.style, item.rect.w, item.rect.h, dpi)
}

fn rgba(c: [u8; 3]) -> [u8; 4] {
    [c[0], c[1], c[2], 255]
}

/// `src` over `dst` (same size) at `opacity`.
fn composite(dst: &mut Rgba8, src: &Rgba8, opacity: f32) {
    if dst.width != src.width || dst.height != src.height {
        return;
    }
    let o = opacity.clamp(0.0, 1.0);
    for (d, s) in dst.data.iter_mut().zip(&src.data) {
        let a = f32::from(s[3]) / 255.0 * o;
        if a <= 0.0 {
            continue;
        }
        for k in 0..3 {
            d[k] = (f32::from(s[k]) * a + f32::from(d[k]) * (1.0 - a)).round() as u8;
        }
    }
}

fn fill(img: &mut Rgba8, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3], a: f32) {
    let (w, h) = (img.width as f32, img.height as f32);
    let (x0, y0, x1, y1) = (x0.clamp(0.0, w) as usize, y0.clamp(0.0, h) as usize, x1.clamp(0.0, w) as usize, y1.clamp(0.0, h) as usize);
    let a = a.clamp(0.0, 1.0);
    for y in y0..y1 {
        for x in x0..x1 {
            if let Some(d) = img.data.get_mut(y * img.width + x) {
                for k in 0..3 {
                    d[k] = (f32::from(c[k]) * a + f32::from(d[k]) * (1.0 - a)).round() as u8;
                }
            }
        }
    }
}

fn outline(img: &mut Rgba8, r: Rect, t: f32, c: [u8; 3], a: f32) {
    fill(img, r.x, r.y, r.right(), r.y + t, c, a);
    fill(img, r.x, r.bottom() - t, r.right(), r.bottom(), c, a);
    fill(img, r.x, r.y, r.x + t, r.bottom(), c, a);
    fill(img, r.right() - t, r.y, r.right(), r.bottom(), c, a);
}

/// The graphic's shapes in trim points: rectangles to fill.
pub fn graphic_rects(g: Graphic, w: f32, h: f32) -> Vec<Rect> {
    let line = 1.5;
    match g {
        Graphic::None => Vec::new(),
        Graphic::Frame => {
            let r = Rect::new(TEXT_SAFE, TEXT_SAFE, w - 2.0 * TEXT_SAFE, h - 2.0 * TEXT_SAFE);
            vec![
                Rect::new(r.x, r.y, r.w, line),
                Rect::new(r.x, r.bottom() - line, r.w, line),
                Rect::new(r.x, r.y, line, r.h),
                Rect::new(r.right() - line, r.y, line, r.h),
            ]
        }
        Graphic::Corners => {
            let (m, l) = (TEXT_SAFE, w.min(h) * 0.12);
            let mut v = Vec::new();
            for (x, dx) in [(m, 1.0), (w - m, -1.0)] {
                for (y, dy) in [(m, 1.0), (h - m, -1.0)] {
                    let hx = if dx > 0.0 { x } else { x - l };
                    let vy = if dy > 0.0 { y } else { y - l };
                    v.push(Rect::new(hx, if dy > 0.0 { y } else { y - line }, l, line));
                    v.push(Rect::new(if dx > 0.0 { x } else { x - line }, vy, line, l));
                }
            }
            v
        }
        Graphic::Band => vec![Rect::new(0.0, h * 0.62, w, h * 0.18)],
        Graphic::Lines => {
            let step = 18.0;
            let n = ((h / step) as usize).min(400);
            (1..n).map(|i| Rect::new(0.0, i as f32 * step, w, 0.5)).collect()
        }
    }
}

fn photo_layer(page: &Page, cells: Vec<Cell>, photos: &dyn PhotoSource, ro: &RenderOptions, warnings: &mut Vec<String>) -> Result<Option<Rgba8>> {
    if cells.is_empty() {
        return Ok(None);
    }
    let layer = Page { background: [0, 0, 0, 0], ..page.clone() };
    let r = dac_layout::render::render_page(&layer, &PageLayout { cells, guides: Vec::new() }, photos, &dac_layout::render::PlaceholderText, ro)?;
    warnings.extend(r.warnings);
    Ok(Some(r.image))
}

/// A rendered page and its warnings (missing photos are drawn as grey slots).
pub struct Rendered {
    pub image: Rgba8,
    pub warnings: Vec<String>,
}

/// Renders a page.
pub fn render_page(book: &Book, r: PageRef, photos: &dyn PhotoSource, engine: &mut TextEngine, opt: &Options) -> Result<Rendered> {
    let page: &BookPage = book.page(r).ok_or_else(|| BookError::Bad(format!("no {}", r.label())))?;
    if !(opt.dpi.is_finite() && opt.dpi > 0.0 && opt.dpi <= 1200.0) {
        return Err(BookError::Render("dpi must be between 0 and 1200".into()));
    }
    let trim = book.trim();
    let bleed = if opt.bleed { book.settings.bleed } else { 0.0 };
    let k = opt.dpi / 72.0;
    let (w, h) = (((trim.w + 2.0 * bleed) * k).round().max(1.0) as usize, ((trim.h + 2.0 * bleed) * k).round().max(1.0) as usize);
    if w.saturating_mul(h) > MAX_PIXELS {
        return Err(BookError::Render(format!("{w}×{h} pixels is too large")));
    }
    let bg = book.background_of(r);
    let mut img = Rgba8::filled(w, h, rgba(bg.color));
    let lpage = Page { size: trim, margins: Default::default(), bleed, background: rgba(bg.color) };
    let ro = RenderOptions { dpi: opt.dpi, include_bleed: opt.bleed, show_guides: false, page: 1, pages: 1 };
    let mut warnings = Vec::new();
    let px = |v: f32| (v + bleed) * k;
    // background photo
    if let Some(id) = &bg.photo {
        let mut c = Cell::photo(Rect::new(-bleed, -bleed, trim.w + 2.0 * bleed, trim.h + 2.0 * bleed));
        if let Some(p) = c.as_photo_mut() {
            p.photo = Some(id.clone());
            p.fit = Fit::Fill;
        }
        if let Some(layer) = photo_layer(&lpage, vec![c], photos, &ro, &mut warnings)? {
            composite(&mut img, &layer, bg.photo_opacity);
        }
    }
    // graphic
    for g in graphic_rects(bg.graphic, trim.w, trim.h) {
        fill(&mut img, px(g.x), px(g.y), px(g.right()), px(g.bottom()), bg.graphic_color, bg.graphic_opacity);
    }
    // photos
    let cells: Vec<Cell> = page
        .cells
        .iter()
        .filter_map(|c| match &c.content {
            CellContent::Photo { photo, fill, zoom, pan, .. } if photo.is_some() || opt.editor => Some(Cell {
                rect: c.rect.to_points(trim).inset(&c.padding.insets()),
                kind: dac_layout::CellKind::Photo(PhotoCell {
                    photo: photo.clone(),
                    fit: if *fill { Fit::Fill } else { Fit::Fit },
                    zoom: *zoom,
                    pan: *pan,
                    ..PhotoCell::default()
                }),
            }),
            _ => None,
        })
        .collect();
    if let Some(layer) = photo_layer(&lpage, cells, photos, &ro, &mut warnings)? {
        composite(&mut img, &layer, 1.0);
    }
    // text
    if opt.text {
        for item in text_items(book, r, photos, opt.editor) {
            let placed = layout_item(engine, &item, opt.dpi);
            crate::text::draw(&mut img, &placed, px(item.rect.x), px(item.rect.y), item.rect.w * k, item.rect.h * k);
        }
    }
    // guides
    if opt.editor && book.guides.show {
        let t = 1.0f32.max(k * 0.5);
        let g = &book.guides;
        if g.bleed && opt.bleed && bleed > 0.0 {
            outline(&mut img, Rect::new(px(0.0), px(0.0), trim.w * k, trim.h * k), t, [230, 40, 40], 0.9);
        }
        if g.text_safe {
            outline(
                &mut img,
                Rect::new(px(TEXT_SAFE), px(TEXT_SAFE), (trim.w - 2.0 * TEXT_SAFE) * k, (trim.h - 2.0 * TEXT_SAFE) * k),
                t,
                [40, 140, 230],
                0.6,
            );
        }
        if g.photo_cells {
            for c in &page.cells {
                let pr = c.rect.to_points(trim);
                outline(&mut img, Rect::new(px(pr.x), px(pr.y), pr.w * k, pr.h * k), t, [150, 150, 150], 0.7);
            }
        }
    }
    if let Some(c) = opt.selected.and_then(|i| page.cells.get(i)) {
        let pr = c.rect.to_points(trim);
        outline(&mut img, Rect::new(px(pr.x), px(pr.y), pr.w * k, pr.h * k), (2.0 * k).max(2.0), [250, 200, 40], 1.0);
    }
    Ok(Rendered { image: img, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Background, BookCell, NRect};

    pub(crate) struct Solid;
    impl PhotoSource for Solid {
        fn image(&self, photo: &str, _: usize) -> std::result::Result<Rgba8, String> {
            match photo {
                "red" => Ok(Rgba8::filled(300, 200, [220, 20, 20, 255])),
                "blue" => Ok(Rgba8::filled(200, 300, [20, 20, 220, 255])),
                _ => Err("missing".into()),
            }
        }
        fn info(&self, photo: &str) -> PhotoInfo {
            PhotoInfo { title: format!("Title of {photo}"), ..PhotoInfo::default() }
        }
    }

    fn book() -> Book {
        let mut b = Book::default();
        let mut p = crate::templates::page("1-full").unwrap();
        if let CellContent::Photo { photo, .. } = &mut p.cells[0].content {
            *photo = Some("red".into());
        }
        b.pages.push(p);
        b
    }

    #[test]
    fn renders_photos_backgrounds_and_text() {
        let mut e = TextEngine::new();
        let mut b = book();
        let r = render_page(&b, PageRef::Page(0), &Solid, &mut e, &Options::default()).unwrap();
        assert_eq!((r.image.width, r.image.height), (720, 576));
        assert_eq!(r.image.get(360, 288)[0], 220);
        // half-opacity background photo on a white page
        let mut p = crate::templates::page("blank").unwrap();
        p.background = Some(Background { photo: Some("blue".into()), photo_opacity: 0.5, ..Background::default() });
        b.pages.push(p);
        let r = render_page(&b, PageRef::Page(1), &Solid, &mut e, &Options::default()).unwrap();
        let c = r.image.get(100, 100);
        assert!((130..=140).contains(&c[0]) && c[2] > 230, "{c:?}");
        // text cell with tokens, page numbers
        let mut p = crate::templates::page("1-caption").unwrap();
        if let CellContent::Photo { photo, .. } = &mut p.cells[0].content {
            *photo = Some("red".into());
        }
        if let CellContent::Text { text, .. } = &mut p.cells[1].content {
            *text = "{Title}".into();
        }
        b.pages.push(p);
        b.numbers.show = true;
        let items = text_items(&b, PageRef::Page(2), &Solid, false);
        assert_eq!(items[0].text, "Title of red");
        assert_eq!(items.last().unwrap().text, "3");
        let r = render_page(&b, PageRef::Page(2), &Solid, &mut e, &Options { dpi: 36.0, ..Options::default() }).unwrap();
        assert!(r.warnings.is_empty());
        // missing photo and editor overlays
        b.pages[0].cells.push(BookCell::photo(NRect::new(0.1, 0.1, 0.2, 0.2)));
        if let CellContent::Photo { photo, .. } = &mut b.pages[0].cells[1].content {
            *photo = Some("gone".into());
        }
        let r = render_page(&b, PageRef::Page(0), &Solid, &mut e, &Options { editor: true, bleed: true, selected: Some(0), ..Options::default() })
            .unwrap();
        assert_eq!(r.warnings.len(), 1);
        assert!(render_page(&b, PageRef::Page(9), &Solid, &mut e, &Options::default()).is_err());
        assert!(render_page(&b, PageRef::Page(0), &Solid, &mut e, &Options { dpi: f32::NAN, ..Options::default() }).is_err());
    }

    #[test]
    fn graphics_stay_on_the_page() {
        for g in Graphic::ALL {
            for r in graphic_rects(g, 500.0, 400.0) {
                assert!(r.x >= 0.0 && r.y >= 0.0 && r.right() <= 500.0 && r.bottom() <= 400.0, "{g:?} {r:?}");
            }
        }
    }
}
