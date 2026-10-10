//! A book as a shared `dac-layout` document and back, so books are saved creations (catalog
//! collections holding a layout) and the engine's photo preparation and rendering apply.
//!
//! The layout document carries the page size, every page (covers included) with its photo and
//! text cells (padding applied, fit/fill, zoom, pan, basic type) and background photos as
//! full-bleed cells. Book-only settings (export settings, photo and page text, graphics, page
//! numbers, guides, presets) travel in the book JSON; [`from_layout`] rebuilds what the layout
//! holds.

use dac_layout::{Align, Cell, CellKind, CreationKind, Document, Fit, Page, PageLayout, PhotoCell, Rect, TextCell, TextStyle};

use crate::{Book, BookCell, BookPage, BookSize, CellContent, CoverType, HAlign, NRect, Padding, PageRef, TypeStyle};

fn layout_style(s: &TypeStyle) -> TextStyle {
    TextStyle {
        family: (!s.font.is_empty()).then(|| s.font.clone()),
        size: s.size,
        color: [s.color[0], s.color[1], s.color[2], (s.opacity.clamp(0.0, 1.0) * 255.0).round() as u8],
        align: match s.align {
            HAlign::Left | HAlign::Justify => Align::Left,
            HAlign::Center => Align::Center,
            HAlign::Right => Align::Right,
        },
        bold: s.style.bold(),
        italic: s.style.italic(),
    }
}

fn book_style(s: &TextStyle) -> TypeStyle {
    let style = match (s.bold, s.italic) {
        (true, true) => crate::FontStyle::BoldItalic,
        (true, false) => crate::FontStyle::Bold,
        (false, true) => crate::FontStyle::Italic,
        (false, false) => crate::FontStyle::Regular,
    };
    TypeStyle {
        font: s.family.clone().unwrap_or_default(),
        style,
        size: if s.size.is_finite() { s.size.clamp(1.0, 500.0) } else { 11.0 },
        opacity: f32::from(s.color[3]) / 255.0,
        color: [s.color[0], s.color[1], s.color[2]],
        align: match s.align {
            Align::Left => HAlign::Left,
            Align::Center => HAlign::Center,
            Align::Right => HAlign::Right,
        },
        ..TypeStyle::default()
    }
}

/// The book as a layout document (front cover, pages, back cover).
pub fn to_layout(book: &Book) -> Document {
    let trim = book.trim();
    let mut page = Page::new(trim);
    page.bleed = book.settings.bleed;
    page.background = [book.background.color[0], book.background.color[1], book.background.color[2], 255];
    let mut doc = Document::new(CreationKind::Book, page);
    for r in book.all_pages() {
        let Some(p) = book.page(r) else { continue };
        let mut cells = Vec::with_capacity(p.cells.len() + 1);
        if let Some(id) = &book.background_of(r).photo {
            let b = book.settings.bleed;
            let mut c = Cell::photo(Rect::new(-b, -b, trim.w + 2.0 * b, trim.h + 2.0 * b));
            if let Some(pc) = c.as_photo_mut() {
                pc.photo = Some(id.clone());
                pc.fit = Fit::Fill;
            }
            cells.push(c);
        }
        for c in &p.cells {
            let rect = c.rect.to_points(trim).inset(&c.padding.insets());
            cells.push(match &c.content {
                CellContent::Photo { photo, fill, zoom, pan, .. } => Cell {
                    rect,
                    kind: CellKind::Photo(PhotoCell {
                        photo: photo.clone(),
                        fit: if *fill { Fit::Fill } else { Fit::Fit },
                        zoom: *zoom,
                        pan: *pan,
                        ..PhotoCell::default()
                    }),
                },
                CellContent::Text { text, style } => {
                    Cell { rect, kind: CellKind::Text(TextCell { text: text.clone(), style: layout_style(style), source: None }) }
                }
            });
        }
        doc.pages.push(PageLayout { cells, guides: Vec::new() });
    }
    doc
}

/// A book rebuilt from a layout document: size, pages and cells (the first and last page become
/// the covers when there are three or more pages).
pub fn from_layout(doc: &Document, name: &str) -> crate::Result<Book> {
    doc.validate()?;
    let size = doc.page.size;
    if !(size.w > 0.0 && size.h > 0.0) {
        return Err(crate::BookError::Invalid("page size".into()));
    }
    let mut book = Book { name: name.to_string(), ..Book::default() };
    book.settings.size = BookSize::PRESETS
        .into_iter()
        .find(|b| (b.points().w - size.w).abs() < 0.5 && (b.points().h - size.h).abs() < 0.5)
        .unwrap_or(BookSize::Custom { w: size.w / 72.0, h: size.h / 72.0 });
    book.settings.bleed = doc.page.bleed.clamp(0.0, 36.0);
    book.background.color = [doc.page.background[0], doc.page.background[1], doc.page.background[2]];
    let norm = |r: &Rect| NRect::new(r.x / size.w, r.y / size.h, r.w / size.w, r.h / size.h);
    let mut pages: Vec<BookPage> = doc
        .pages
        .iter()
        .map(|pl| {
            let mut page = BookPage { template: "custom".into(), ..BookPage::default() };
            for c in &pl.cells {
                let full = c.rect.x <= 0.0 && c.rect.y <= 0.0 && c.rect.right() >= size.w && c.rect.bottom() >= size.h;
                match &c.kind {
                    CellKind::Photo(p) if full && pl.cells.len() > 1 && page.cells.is_empty() => {
                        let mut bg = book.background.clone();
                        bg.photo = p.photo.clone();
                        page.background = Some(bg);
                    }
                    CellKind::Photo(p) => page.cells.push(BookCell {
                        rect: norm(&c.rect),
                        padding: Padding::default(),
                        content: CellContent::Photo {
                            photo: p.photo.clone(),
                            fill: p.fit == Fit::Fill,
                            zoom: p.zoom.clamp(1.0, 10.0),
                            pan: p.pan,
                            text: None,
                        },
                    }),
                    CellKind::Text(t) => page.cells.push(BookCell {
                        rect: norm(&c.rect),
                        padding: Padding::default(),
                        content: CellContent::Text { text: t.text.clone(), style: book_style(&t.style) },
                    }),
                    CellKind::Graphic(_) => {}
                }
            }
            page.cells.truncate(crate::MAX_CELLS);
            page
        })
        .collect();
    if pages.len() >= 3 {
        book.settings.cover = CoverType::Hardcover;
        book.back = pages.pop().unwrap_or_default();
        book.front = pages.remove(0);
    } else {
        book.settings.cover = CoverType::None;
    }
    book.pages = pages;
    book.validate()?;
    Ok(book)
}

/// Where a layout page index lands in the book.
pub fn page_of(book: &Book, layout_index: usize) -> Option<PageRef> {
    book.all_pages().get(layout_index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_pages_photos_and_text() {
        let mut b = Book::default();
        crate::auto::auto_layout(&mut b, &["1".into(), "2".into(), "3".into()], &crate::auto::builtin_presets()[2]).unwrap();
        b.pages[0].background = Some(crate::Background { photo: Some("2".into()), ..Default::default() });
        if let CellContent::Text { text, .. } = &mut b.front.cells[1].content {
            *text = "Title".into();
        }
        let doc = to_layout(&b);
        doc.validate().unwrap();
        assert_eq!(doc.pages.len(), b.pages.len() + 2);
        assert_eq!(doc.photos().len(), 5); // cover + 3 + background
        let back = from_layout(&doc, "x").unwrap();
        assert_eq!(back.pages.len(), b.pages.len());
        assert_eq!(back.settings.size, b.settings.size);
        assert_eq!(back.photos(), b.photos());
        assert_eq!(back.pages[0].background.as_ref().and_then(|g| g.photo.as_deref()), Some("2"));
        assert!(matches!(&back.front.cells[1].content, CellContent::Text { text, .. } if text == "Title"));
        assert_eq!(page_of(&back, 0), Some(PageRef::Front));
    }
}
