//! Snapping a moved or resized cell to page edges, margins, the centre lines, guides and the
//! other cells' edges.

use crate::{Guide, Page, PageLayout, Rect};

/// Lines a cell can snap to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapTargets {
    /// Vertical lines (x positions).
    pub xs: Vec<f32>,
    /// Horizontal lines (y positions).
    pub ys: Vec<f32>,
}

impl SnapTargets {
    /// Targets for moving cell `moving` on `layout`: trim edges, margins, centre lines, guides and
    /// the edges and centres of every other cell.
    pub fn for_page(page: &Page, layout: &PageLayout, moving: Option<usize>) -> SnapTargets {
        let mut t = SnapTargets::default();
        let mut add = |r: &Rect| {
            let (cx, cy) = r.center();
            t.xs.extend([r.x, cx, r.right()]);
            t.ys.extend([r.y, cy, r.bottom()]);
        };
        add(&page.trim());
        add(&page.content());
        for (i, c) in layout.cells.iter().enumerate() {
            if Some(i) != moving {
                add(&c.rect);
            }
        }
        for g in &layout.guides {
            match *g {
                Guide::Vertical(x) => t.xs.push(x),
                Guide::Horizontal(y) => t.ys.push(y),
            }
        }
        t.xs.retain(|v| v.is_finite());
        t.ys.retain(|v| v.is_finite());
        t
    }
}

/// The smallest shift (|shift| ≤ `tol`) that brings one of `edges` onto one of `lines`.
fn best_shift(edges: [f32; 3], lines: &[f32], tol: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for e in edges {
        for l in lines {
            let d = l - e;
            if d.abs() <= tol && best.is_none_or(|b| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
    }
    best
}

/// `rect` moved so its nearest edge or centre lies on a target within `tol` points, per axis.
/// The second value says which axes snapped (for drawing the snap line).
pub fn snap_move(rect: Rect, targets: &SnapTargets, tol: f32) -> (Rect, [Option<f32>; 2]) {
    let tol = if tol.is_finite() { tol.max(0.0) } else { 0.0 };
    let (cx, cy) = rect.center();
    let dx = best_shift([rect.x, cx, rect.right()], &targets.xs, tol);
    let dy = best_shift([rect.y, cy, rect.bottom()], &targets.ys, tol);
    let out = Rect { x: rect.x + dx.unwrap_or(0.0), y: rect.y + dy.unwrap_or(0.0), ..rect };
    let (ox, oy) = out.center();
    let line =
        |d: Option<f32>, edges: [f32; 3], lines: &[f32]| d.and_then(|_| edges.into_iter().find(|e| lines.iter().any(|l| (l - e).abs() < 1e-3)));
    (out, [line(dx, [out.x, ox, out.right()], &targets.xs), line(dy, [out.y, oy, out.bottom()], &targets.ys)])
}

/// Resizing from the bottom-right corner: the right and bottom edges snap.
pub fn snap_resize(rect: Rect, targets: &SnapTargets, tol: f32) -> Rect {
    let tol = if tol.is_finite() { tol.max(0.0) } else { 0.0 };
    let r = rect.right();
    let b = rect.bottom();
    let dx = best_shift([r, r, r], &targets.xs, tol).unwrap_or(0.0);
    let dy = best_shift([b, b, b], &targets.ys, tol).unwrap_or(0.0);
    Rect { w: (rect.w + dx).max(1.0), h: (rect.h + dy).max(1.0), ..rect }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Insets, Size};

    #[test]
    fn snaps_to_margin_guide_and_other_cell() {
        let page = Page::new(Size::new(600.0, 800.0)).with_margins(Insets::all(36.0));
        let layout = PageLayout {
            cells: vec![Cell::photo(Rect::new(300.0, 300.0, 100.0, 100.0)), Cell::photo(Rect::new(40.0, 500.0, 50.0, 50.0))],
            guides: vec![Guide::Horizontal(520.0)],
        };
        let t = SnapTargets::for_page(&page, &layout, Some(1));
        let (r, lines) = snap_move(layout.cells[1].rect, &t, 6.0);
        assert_eq!(r.x, 36.0); // left margin
        assert_eq!(r.y, 495.0); // its centre (525) onto the guide at 520
        assert_eq!(lines, [Some(36.0), Some(520.0)]);
        // far from everything: unchanged
        let (r2, l2) = snap_move(Rect::new(150.0, 150.0, 10.0, 10.0), &t, 2.0);
        assert_eq!(r2, Rect::new(150.0, 150.0, 10.0, 10.0));
        assert_eq!(l2, [None, None]);
        // the other cell's right edge
        let rs = snap_resize(Rect::new(250.0, 100.0, 148.0, 30.0), &t, 4.0);
        assert_eq!(rs.right(), 400.0);
        // hostile tolerance
        assert_eq!(snap_move(Rect::new(37.0, 37.0, 1.0, 1.0), &t, f32::NAN).0.x, 37.0);
    }
}
