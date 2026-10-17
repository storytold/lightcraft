//! Templates: a layout document without photos, plus a name. User templates are saved as JSON;
//! the built-ins are our own designs.

use serde::{Deserialize, Serialize};

use crate::grid::Grid;
use crate::{Align, Cell, CellKind, CreationKind, Document, Fit, GraphicCell, Insets, LayoutError, Page, PageLayout, Rect, Result, Size, TextStyle};

/// A named layout to start from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    /// Built-in templates can't be deleted or overwritten.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
    pub document: Document,
}

impl Template {
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| LayoutError::Json(e.to_string()))
    }
    /// Parses and validates a saved template.
    pub fn from_json(s: &str) -> Result<Template> {
        let t: Template = serde_json::from_str(s).map_err(|e| LayoutError::Json(e.to_string()))?;
        if t.name.trim().is_empty() || t.name.len() > 256 {
            return Err(LayoutError::Invalid("a template needs a name (up to 256 bytes)".into()));
        }
        if t.document.pages.is_empty() {
            return Err(LayoutError::Invalid("a template has at least one page".into()));
        }
        t.document.validate()?;
        Ok(t)
    }

    /// A document with `photos` placed into the template's photo cells in order. The template's
    /// pages repeat until every photo is placed (a 4×5 contact sheet of 45 photos makes three
    /// pages); with no photos, one copy of the template's pages. With `repeat_one`, every photo
    /// cell of a page gets the same photo, one page per photo (picture packages).
    pub fn instantiate(&self, photos: &[String], repeat_one: bool) -> Result<Document> {
        let mut doc = Document { pages: Vec::new(), ..self.document.clone() };
        let slots: usize = self.document.pages.iter().map(|p| p.photo_cells().count()).sum();
        if self.document.pages.is_empty() {
            return Err(LayoutError::Invalid("the template has no pages".into()));
        }
        if photos.is_empty() || slots == 0 {
            doc.pages = self.document.pages.clone();
            return Ok(doc);
        }
        let mut next = photos.iter();
        if repeat_one {
            for photo in photos {
                for page in &self.document.pages {
                    let mut page = page.clone();
                    page.cells.iter_mut().filter_map(Cell::as_photo_mut).for_each(|c| c.photo = Some(photo.clone()));
                    push_page(&mut doc, page)?;
                }
            }
            return Ok(doc);
        }
        'outer: loop {
            for page in &self.document.pages {
                let mut page = page.clone();
                let mut placed = false;
                for c in page.cells.iter_mut().filter_map(Cell::as_photo_mut) {
                    c.photo = next.next().cloned();
                    placed |= c.photo.is_some();
                }
                if !placed {
                    break 'outer;
                }
                push_page(&mut doc, page)?;
            }
        }
        Ok(doc)
    }
}

fn push_page(doc: &mut Document, page: PageLayout) -> Result<()> {
    if doc.pages.len() >= crate::MAX_PAGES {
        return Err(LayoutError::Invalid(format!("more than {} pages", crate::MAX_PAGES)));
    }
    doc.pages.push(page);
    Ok(())
}

fn builtin(name: &str, kind: CreationKind, page: Page, pages: Vec<PageLayout>) -> Template {
    Template { name: name.into(), builtin: true, document: Document { pages, ..Document::new(kind, page) } }
}

fn grid(page: &Page, rows: u32, cols: u32, gutter: f32, fit: Fit, caption: Option<&str>) -> PageLayout {
    let g = Grid {
        caption_height: if caption.is_some() { 14.0 } else { 0.0 },
        caption: caption.map(str::to_string),
        ..Grid::new(rows, cols).with_gutter(gutter)
    };
    g.layout(page, fit).unwrap_or_default()
}

fn caption(rect: Rect, text: &str, size: f32, align: Align) -> Cell {
    Cell::text(rect, text, TextStyle { size, align, ..TextStyle::default() })
}

/// The built-in templates (own designs). Page sizes: US Letter, A4, a 10×8 in landscape book,
/// 16:9 slides and a web page.
pub fn builtin_templates() -> Vec<Template> {
    let letter = Page::new(Size::inches(8.5, 11.0)).with_margins(Insets::all(36.0));
    let a4 = Page::new(Size::mm(210.0, 297.0)).with_margins(Insets::all(15.0 * crate::PT_PER_MM));
    let letter_land = Page::new(Size::inches(8.5, 11.0).landscape()).with_margins(Insets::all(36.0));
    let book = Page { bleed: 9.0, ..Page::new(Size::inches(10.0, 8.0)).with_margins(Insets::all(36.0)) };
    let slide = Page { background: [24, 24, 24, 255], ..Page::new(Size::new(1920.0, 1080.0)) };
    let web = Page { background: [32, 32, 32, 255], ..Page::new(Size::new(1200.0, 900.0)).with_margins(Insets::all(40.0)) };

    let mut fine_art = grid(&letter, 1, 1, 0.0, Fit::Fit, None);
    // mat: 1.25 in border with the title below
    if let Some(c) = fine_art.cells.first_mut() {
        c.rect = letter.trim().inset(&Insets { top: 90.0, right: 90.0, bottom: 150.0, left: 90.0 });
    }
    let fa = letter.size;
    fine_art.cells.push(caption(Rect::new(90.0, fa.h - 130.0, fa.w - 180.0, 20.0), "{Title}", 12.0, Align::Center));
    fine_art.cells.push(caption(Rect::new(90.0, fa.h - 105.0, fa.w - 180.0, 14.0), "{Exposure} · {Date}", 8.0, Align::Center));

    let triptych = grid(&letter_land, 1, 3, 12.0, Fit::Fill, None);

    let full_bleed = PageLayout {
        cells: vec![{
            let mut c = Cell::photo(book.bleed_box());
            if let Some(p) = c.as_photo_mut() {
                p.fit = Fit::Fill;
            }
            c
        }],
        guides: Vec::new(),
    };
    let mut book_two = grid(&book, 1, 2, 18.0, Fit::Fill, None);
    book_two.cells.push(caption(Rect::new(36.0, book.size.h - 30.0, book.size.w - 72.0, 14.0), "{Caption}", 9.0, Align::Center));

    let mut slide_title = PageLayout { cells: vec![Cell::photo(Rect::new(160.0, 60.0, 1600.0, 860.0))], guides: Vec::new() };
    slide_title.cells.push(Cell {
        rect: Rect::new(160.0, 950.0, 1600.0, 60.0),
        kind: CellKind::Text(crate::TextCell {
            text: "{Title}".into(),
            style: TextStyle { size: 40.0, color: [235, 235, 235, 255], align: Align::Center, ..TextStyle::default() },
            source: None,
        }),
    });

    let mut web_grid = grid(&web, 3, 4, 16.0, Fit::Fill, None);
    web_grid.cells.insert(0, Cell::graphic(web.trim(), GraphicCell { fill: Some([32, 32, 32, 255]), ..GraphicCell::default() }));

    vec![
        builtin("1 Large, Letter", CreationKind::Print, letter.clone(), vec![grid(&letter, 1, 1, 0.0, Fit::Fit, None)]),
        builtin("Fine Art Mat", CreationKind::Print, letter.clone(), vec![fine_art]),
        builtin("2×2 Cells, A4", CreationKind::Print, a4.clone(), vec![grid(&a4, 2, 2, 18.0, Fit::Fill, None)]),
        builtin("4×5 Contact Sheet", CreationKind::Print, letter.clone(), vec![grid(&letter, 5, 4, 9.0, Fit::Fit, Some("{Filename}"))]),
        builtin("Triptych", CreationKind::Print, letter_land, vec![triptych]),
        builtin("Full Bleed Spread", CreationKind::Book, book.clone(), vec![full_bleed]),
        builtin("Two Up with Caption", CreationKind::Book, book, vec![book_two]),
        builtin("Title Slide", CreationKind::Slideshow, slide, vec![slide_title]),
        builtin("Grid Gallery", CreationKind::Web, web, vec![web_grid]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_valid_and_round_trip() {
        let all = builtin_templates();
        assert!(all.len() >= 8);
        for t in &all {
            t.document.validate().unwrap();
            assert!(t.document.pages.iter().all(|p| !p.cells.is_empty()), "{}", t.name);
            let back = Template::from_json(&t.to_json().unwrap()).unwrap();
            assert_eq!(&back, t);
        }
    }

    #[test]
    fn instantiate_repeats_pages() {
        let sheet = builtin_templates().into_iter().find(|t| t.name == "4×5 Contact Sheet").unwrap();
        let photos: Vec<String> = (0..45).map(|i| i.to_string()).collect();
        let doc = sheet.instantiate(&photos, false).unwrap();
        assert_eq!(doc.pages.len(), 3);
        assert_eq!(doc.photos().len(), 45);
        assert_eq!(doc.photos()[44], "44");
        let one = sheet.instantiate(&photos[..2], true).unwrap();
        assert_eq!(one.pages.len(), 2);
        assert!(one.pages[1].photo_cells().all(|c| c.photo.as_deref() == Some("1")));
        assert_eq!(sheet.instantiate(&[], false).unwrap().pages.len(), 1);
    }

    #[test]
    fn hostile_template_json_is_an_error() {
        assert!(Template::from_json("{").is_err());
        assert!(Template::from_json(r#"{"name":"x","document":{"kind":"print","page":{"size":{"w":1e30,"h":10}},"pages":[{}]}}"#).is_err());
        assert!(Template::from_json(r#"{"name":"","document":{"kind":"print","page":{"size":{"w":100,"h":100}},"pages":[{}]}}"#).is_err());
        assert!(
            Template::from_json(r#"{"name":"x","document":{"version":99,"kind":"print","page":{"size":{"w":100,"h":100}},"pages":[{}]}}"#).is_err()
        );
        let ok = r#"{"name":"x","document":{"kind":"print","page":{"size":{"w":100,"h":100}},"pages":[{"cells":[{"rect":{"x":0,"y":0,"w":10,"h":10},"type":"photo","zoom":1}]}]}}"#;
        assert_eq!(Template::from_json(ok).unwrap().document.pages[0].cells.len(), 1);
        let bad_cell = ok.replace("\"zoom\":1", "\"rotate\":9");
        assert!(Template::from_json(&bad_cell).is_err());
    }
}
