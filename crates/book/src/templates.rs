//! Built-in page templates, our own designs, grouped by photo count (Lightroom-style
//! "1 Photo", "2 Photos", … "4+ Photos", "Text", "Blank", "Cover").

use crate::{BookCell, BookPage, HAlign, NRect, TypeStyle, VAlign};

/// A template group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Cover,
    One,
    Two,
    Three,
    Four,
    More,
    TextOnly,
    Blank,
}

impl Group {
    pub const ALL: [Group; 8] = [Group::One, Group::Two, Group::Three, Group::Four, Group::More, Group::TextOnly, Group::Blank, Group::Cover];
    pub fn label(self) -> &'static str {
        match self {
            Group::Cover => "Cover",
            Group::One => "1 Photo",
            Group::Two => "2 Photos",
            Group::Three => "3 Photos",
            Group::Four => "4 Photos",
            Group::More => "5+ Photos",
            Group::TextOnly => "Text",
            Group::Blank => "Blank",
        }
    }
}

/// A page template.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub group: Group,
    /// Has text cells.
    pub text: bool,
    pub cells: Vec<BookCell>,
}

impl Template {
    pub fn photos(&self) -> usize {
        self.cells.iter().filter(|c| c.is_photo()).count()
    }
    /// A fresh page from this template.
    pub fn page(&self) -> BookPage {
        BookPage { template: self.id.into(), cells: self.cells.clone(), ..BookPage::default() }
    }
}

const M: f32 = 0.08; // outer margin (fraction of the page)
const G: f32 = 0.025; // gutter

fn p(x: f32, y: f32, w: f32, h: f32) -> BookCell {
    BookCell::photo(NRect::new(x, y, w, h))
}

fn t(x: f32, y: f32, w: f32, h: f32, size: f32, align: HAlign) -> BookCell {
    BookCell::text(NRect::new(x, y, w, h), TypeStyle { align, ..TypeStyle::sized(size) })
}

fn grid(cols: usize, rows: usize, area: NRect) -> Vec<BookCell> {
    let cw = (area.w - G * (cols as f32 - 1.0)) / cols as f32;
    let rh = (area.h - G * (rows as f32 - 1.0)) / rows as f32;
    let mut v = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            v.push(p(area.x + c as f32 * (cw + G), area.y + r as f32 * (rh + G), cw, rh));
        }
    }
    v
}

/// Every built-in template.
pub fn builtin() -> Vec<Template> {
    let inner = NRect::new(M, M, 1.0 - 2.0 * M, 1.0 - 2.0 * M);
    let half = (1.0 - 2.0 * M - G) / 2.0;
    let body = 1.0 - 2.0 * M;
    let tpl = |id, name, group, text, cells: Vec<BookCell>| Template { id, name, group, text, cells };
    vec![
        // covers
        tpl("cover-photo-title", "Photo with Title", Group::Cover, true, vec![p(0.0, 0.0, 1.0, 0.78), t(M, 0.82, body, 0.1, 28.0, HAlign::Center)]),
        tpl("cover-full", "Full Bleed Photo", Group::Cover, false, vec![p(0.0, 0.0, 1.0, 1.0)]),
        tpl("cover-title", "Title Only", Group::Cover, true, vec![t(M, 0.4, body, 0.2, 36.0, HAlign::Center)]),
        // one photo
        tpl("1-full", "Full Bleed", Group::One, false, vec![p(0.0, 0.0, 1.0, 1.0)]),
        tpl("1-margin", "With Margins", Group::One, false, vec![p(inner.x, inner.y, inner.w, inner.h)]),
        tpl("1-small", "Small Centred", Group::One, false, vec![p(0.2, 0.2, 0.6, 0.6)]),
        tpl("1-caption", "Photo with Caption", Group::One, true, vec![p(M, M, body, 0.68), t(M, 0.8, body, 0.12, 11.0, HAlign::Center)]),
        tpl("1-text-side", "Photo with Side Text", Group::One, true, vec![p(M, M, 0.52, body), t(0.64, M, 0.28, body, 11.0, HAlign::Left)]),
        // two
        tpl("2-side", "Side by Side", Group::Two, false, vec![p(M, 0.2, half, 0.6), p(M + half + G, 0.2, half, 0.6)]),
        tpl("2-stack", "Stacked", Group::Two, false, vec![p(M, M, body, half), p(M, M + half + G, body, half)]),
        tpl("2-large-small", "Large and Small", Group::Two, false, vec![p(M, M, 0.56, body), p(M + 0.56 + G, 0.5, body - 0.56 - G, 1.0 - M - 0.5)]),
        tpl(
            "2-caption",
            "Two with Caption",
            Group::Two,
            true,
            vec![p(M, M, half, 0.62), p(M + half + G, M, half, 0.62), t(M, 0.76, body, 0.14, 11.0, HAlign::Left)],
        ),
        // three
        tpl("3-row", "Row of Three", Group::Three, false, grid(3, 1, NRect::new(M, 0.3, body, 0.4))),
        tpl("3-feature", "One Large, Two Small", Group::Three, false, {
            let big = 0.58;
            let sw = body - big - G;
            vec![p(M, M, big, body), p(M + big + G, M, sw, half), p(M + big + G, M + half + G, sw, half)]
        }),
        tpl("3-caption", "Three with Caption", Group::Three, true, {
            let mut v = grid(3, 1, NRect::new(M, M, body, 0.5));
            v.push(t(M, M + 0.5 + G * 2.0, body, 0.2, 11.0, HAlign::Left));
            v
        }),
        tpl("3-column", "Column of Three", Group::Three, false, grid(1, 3, NRect::new(0.25, M, 0.5, body))),
        // four
        tpl("4-grid", "Grid of Four", Group::Four, false, grid(2, 2, inner)),
        tpl("4-row", "Row of Four", Group::Four, false, grid(4, 1, NRect::new(M, 0.35, body, 0.3))),
        tpl("4-caption", "Four with Caption", Group::Four, true, {
            let mut v = grid(2, 2, NRect::new(M, M, body, 0.66));
            v.push(t(M, M + 0.66 + G * 2.0, body, 0.14, 11.0, HAlign::Left));
            v
        }),
        // more
        tpl("6-grid", "Grid of Six", Group::More, false, grid(3, 2, inner)),
        tpl("9-grid", "Grid of Nine", Group::More, false, grid(3, 3, inner)),
        tpl("5-mosaic", "Mosaic of Five", Group::More, false, {
            let top = 0.5;
            let mut v = vec![p(M, M, half, top), p(M + half + G, M, half, top)];
            v.extend(grid(3, 1, NRect::new(M, M + top + G, body, body - top - G)));
            v
        }),
        // text and blank
        tpl("text-page", "Text Page", Group::TextOnly, true, vec![t(M, M, body, body, 12.0, HAlign::Left)]),
        tpl(
            "text-title",
            "Title Page",
            Group::TextOnly,
            true,
            vec![{
                let mut c = t(M, 0.35, body, 0.3, 30.0, HAlign::Center);
                if let crate::CellContent::Text { style, .. } = &mut c.content {
                    style.valign = VAlign::Middle;
                }
                c
            }],
        ),
        tpl(
            "text-columns",
            "Two Columns",
            Group::TextOnly,
            true,
            vec![{
                let mut c = t(M, M, body, body, 11.0, HAlign::Justify);
                if let crate::CellContent::Text { style, .. } = &mut c.content {
                    style.columns = 2;
                }
                c
            }],
        ),
        tpl("blank", "Blank", Group::Blank, false, Vec::new()),
    ]
    .into_iter()
    .map(|mut t| {
        t.cells.iter_mut().for_each(|c| c.rect = round(c.rect));
        t
    })
    .collect()
}

fn round(r: NRect) -> NRect {
    let q = |v: f32| (v * 10_000.0).round() / 10_000.0;
    NRect::new(q(r.x), q(r.y), q(r.w), q(r.h))
}

/// The built-in template with this id.
pub fn find(id: &str) -> Option<Template> {
    builtin().into_iter().find(|t| t.id == id)
}

/// A fresh page from the template with this id.
pub fn page(id: &str) -> Option<crate::BookPage> {
    find(id).map(|t| t.page())
}

/// The default template for `photos` photos (`text` = one with a text cell when there is one).
pub fn for_count(photos: usize, text: bool) -> Template {
    let all = builtin();
    let group = match photos {
        0 => Group::Blank,
        1 => Group::One,
        2 => Group::Two,
        3 => Group::Three,
        4 => Group::Four,
        _ => Group::More,
    };
    all.iter()
        .find(|t| t.group == group && t.text == text && (group != Group::More || t.photos() >= photos))
        .or_else(|| all.iter().find(|t| t.group == group))
        .cloned()
        .unwrap_or(Template { id: "blank", name: "Blank", group: Group::Blank, text: false, cells: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_valid_and_unique() {
        let all = builtin();
        let mut ids: Vec<_> = all.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for t in &all {
            for c in &t.cells {
                let r = c.rect;
                assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 1.0001 && r.y + r.h <= 1.0001, "{} {:?}", t.id, r);
            }
            let has_text = t.cells.iter().any(|c| !c.is_photo());
            assert_eq!(has_text, t.text, "{}", t.id);
        }
        assert_eq!(for_count(1, false).photos(), 1);
        assert_eq!(for_count(3, true).photos(), 3);
        assert!(for_count(7, false).photos() >= 7);
        assert_eq!(for_count(0, false).id, "blank");
    }
}
