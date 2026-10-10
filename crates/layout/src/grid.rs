//! Grids: rows × columns of equal photo cells inside the page margins (contact sheets, single
//! image, book grids).

use serde::{Deserialize, Serialize};

use crate::{Cell, LayoutError, Page, PageLayout, Rect, Result};

/// A grid of photo cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub rows: u32,
    pub cols: u32,
    /// Horizontal gap between columns (points).
    #[serde(default)]
    pub gutter_x: f32,
    /// Vertical gap between rows (points).
    #[serde(default)]
    pub gutter_y: f32,
    /// Square cells (the smaller side wins), the grid centred in the content area.
    #[serde(default)]
    pub keep_square: bool,
    /// Room below each cell for a caption (points); a text cell is placed there when
    /// [`Grid::caption`] is set.
    #[serde(default)]
    pub caption_height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Grid {
    pub fn new(rows: u32, cols: u32) -> Grid {
        Grid { rows, cols, gutter_x: 0.0, gutter_y: 0.0, keep_square: false, caption_height: 0.0, caption: None }
    }
    pub fn with_gutter(mut self, g: f32) -> Grid {
        self.gutter_x = g;
        self.gutter_y = g;
        self
    }

    /// The cell rectangles, row by row, inside `area`.
    pub fn rects(&self, area: Rect) -> Result<Vec<Rect>> {
        if self.rows == 0 || self.cols == 0 || self.rows > 100 || self.cols > 100 {
            return Err(LayoutError::Invalid("a grid has 1 to 100 rows and columns".into()));
        }
        let ok = |v: f32| v.is_finite() && v >= 0.0;
        if !ok(self.gutter_x) || !ok(self.gutter_y) || !ok(self.caption_height) {
            return Err(LayoutError::Invalid("grid gutters must be non-negative".into()));
        }
        let (rows, cols) = (self.rows as f32, self.cols as f32);
        let mut cw = (area.w - self.gutter_x * (cols - 1.0)) / cols;
        let mut ch = (area.h - self.gutter_y * (rows - 1.0)) / rows - self.caption_height;
        if cw < 1.0 || ch < 1.0 {
            return Err(LayoutError::Invalid("the grid's cells don't fit inside the margins".into()));
        }
        if self.keep_square {
            let s = cw.min(ch);
            cw = s;
            ch = s;
        }
        let total_w = cw * cols + self.gutter_x * (cols - 1.0);
        let total_h = (ch + self.caption_height) * rows + self.gutter_y * (rows - 1.0);
        let x0 = area.x + (area.w - total_w) / 2.0;
        let y0 = area.y + (area.h - total_h) / 2.0;
        let mut out = Vec::with_capacity((self.rows * self.cols) as usize);
        for r in 0..self.rows {
            for c in 0..self.cols {
                let x = x0 + c as f32 * (cw + self.gutter_x);
                let y = y0 + r as f32 * (ch + self.caption_height + self.gutter_y);
                out.push(Rect::new(x, y, cw, ch));
            }
        }
        Ok(out)
    }

    /// A page layout: one photo cell per grid position (and its caption cell).
    pub fn layout(&self, page: &Page, fill: crate::Fit) -> Result<PageLayout> {
        let mut cells = Vec::new();
        for r in self.rects(page.content())? {
            let mut cell = Cell::photo(r);
            if let Some(p) = cell.as_photo_mut() {
                p.fit = fill;
            }
            cells.push(cell);
            if let Some(text) = &self.caption
                && self.caption_height > 0.0
            {
                let style =
                    crate::TextStyle { size: (self.caption_height * 0.6).clamp(4.0, 24.0), align: crate::Align::Center, ..Default::default() };
                let mut t = Cell::text(Rect::new(r.x, r.bottom(), r.w, self.caption_height), text.clone(), style);
                if let crate::CellKind::Text(tc) = &mut t.kind {
                    tc.source = Some(cells.len() - 1);
                }
                cells.push(t);
            }
        }
        Ok(PageLayout { cells, guides: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fit, Insets, Size};

    #[test]
    fn grid_fills_content_area() {
        let page = Page::new(Size::new(600.0, 800.0)).with_margins(Insets::all(50.0));
        let r = Grid::new(2, 3).with_gutter(10.0).rects(page.content()).unwrap();
        assert_eq!(r.len(), 6);
        assert_eq!(r[0].x, 50.0);
        assert!((r[2].right() - 550.0).abs() < 1e-3);
        assert!((r[5].bottom() - 750.0).abs() < 1e-3);
        assert!((r[1].x - r[0].right() - 10.0).abs() < 1e-3);
    }

    #[test]
    fn square_cells_are_centred() {
        let page = Page::new(Size::new(600.0, 800.0));
        let r = Grid { keep_square: true, ..Grid::new(1, 1) }.rects(page.content()).unwrap();
        assert_eq!((r[0].w, r[0].h), (600.0, 600.0));
        assert_eq!(r[0].y, 100.0);
    }

    #[test]
    fn hostile_grids_are_errors() {
        let area = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(Grid::new(0, 3).rects(area).is_err());
        assert!(Grid::new(1000, 3).rects(area).is_err());
        assert!(Grid::new(50, 50).with_gutter(10.0).rects(area).is_err());
        assert!(Grid::new(2, 2).with_gutter(f32::NAN).rects(area).is_err());
    }

    #[test]
    fn captions_follow_their_photo() {
        let page = Page::new(Size::new(600.0, 800.0));
        let g = Grid { caption_height: 20.0, caption: Some("{Filename}".into()), ..Grid::new(2, 2) };
        let l = g.layout(&page, Fit::Fill).unwrap();
        assert_eq!(l.cells.len(), 8);
        match &l.cells[3].kind {
            crate::CellKind::Text(t) => assert_eq!(t.source, Some(2)),
            k => panic!("{k:?}"),
        }
    }
}
