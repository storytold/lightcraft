//! Picture packages: photo cells of fixed sizes (e.g. one 5×7 and four 2.5×3.5) laid out
//! automatically over as many pages as needed.
//!
//! Sources: "next-fit decreasing height" shelf packing (textbook strip-packing heuristic), own
//! design for the rotation and page-break rules.

use crate::{Cell, LayoutError, Page, PageLayout, Rect, Result, Size};

/// Lays `items` out on pages of `page`'s content area, `gap` points apart. Items are placed
/// tallest first on shelves; an item that doesn't fit as given is turned 90° when that fits.
/// Returns one rectangle list per page, each rectangle tagged with its item index.
pub fn auto_layout(page: &Page, items: &[Size], gap: f32) -> Result<Vec<Vec<(usize, Rect)>>> {
    if items.len() > crate::MAX_CELLS {
        return Err(LayoutError::Invalid(format!("more than {} package cells", crate::MAX_CELLS)));
    }
    if !(gap.is_finite() && gap >= 0.0) {
        return Err(LayoutError::Invalid("package gap must be non-negative".into()));
    }
    let area = page.content();
    let fits = |s: Size| s.w <= area.w + 1e-3 && s.h <= area.h + 1e-3;
    let mut sized = Vec::with_capacity(items.len());
    for (i, s) in items.iter().enumerate() {
        if !(s.w.is_finite() && s.h.is_finite() && s.w > 0.0 && s.h > 0.0) {
            return Err(LayoutError::Invalid(format!("package cell {i} has an invalid size")));
        }
        let s = if fits(*s) {
            *s
        } else if fits(Size::new(s.h, s.w)) {
            Size::new(s.h, s.w)
        } else {
            return Err(LayoutError::DoesNotFit(i));
        };
        sized.push((i, s));
    }
    // Tallest first; ties keep input order (stable sort).
    sized.sort_by(|a, b| b.1.h.total_cmp(&a.1.h));
    let mut pages: Vec<Vec<(usize, Rect)>> = vec![Vec::new()];
    let (mut x, mut y, mut shelf_h) = (area.x, area.y, 0.0f32);
    for (i, s) in sized {
        if x + s.w > area.right() + 1e-3 {
            // next shelf
            x = area.x;
            y += shelf_h + gap;
            shelf_h = 0.0;
        }
        if y + s.h > area.bottom() + 1e-3 {
            pages.push(Vec::new());
            x = area.x;
            y = area.y;
            shelf_h = 0.0;
        }
        if let Some(p) = pages.last_mut() {
            p.push((i, Rect::new(x, y, s.w, s.h)));
        }
        x += s.w + gap;
        shelf_h = shelf_h.max(s.h);
    }
    Ok(pages)
}

/// [`auto_layout`] as page layouts of photo cells (fill, as packages crop to their cell).
pub fn package_pages(page: &Page, items: &[Size], gap: f32) -> Result<Vec<PageLayout>> {
    Ok(auto_layout(page, items, gap)?
        .into_iter()
        .map(|rects| PageLayout {
            cells: rects
                .into_iter()
                .map(|(_, r)| {
                    let mut c = Cell::photo(r);
                    if let Some(p) = c.as_photo_mut() {
                        p.fit = crate::Fit::Fill;
                    }
                    c
                })
                .collect(),
            guides: Vec::new(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Insets;

    fn letter() -> Page {
        Page::new(Size::inches(8.5, 11.0)).with_margins(Insets::all(18.0))
    }

    #[test]
    fn package_fits_on_one_page_without_overlap() {
        let items = [Size::inches(5.0, 7.0), Size::inches(2.5, 3.5), Size::inches(2.5, 3.5), Size::inches(2.5, 3.5), Size::inches(2.5, 3.5)];
        // with no gap a 5×7 and four wallets exactly fill a letter page inside half-inch margins
        let pages = auto_layout(&letter(), &items, 0.0).unwrap();
        assert_eq!(pages.len(), 1);
        let rects: Vec<Rect> = pages[0].iter().map(|r| r.1).collect();
        for (a, ra) in rects.iter().enumerate() {
            assert!(letter().content().x <= ra.x && ra.right() <= letter().content().right() + 1e-3);
            for rb in rects.iter().skip(a + 1) {
                assert!(!ra.intersects(rb), "{ra:?} {rb:?}");
            }
        }
    }

    #[test]
    fn overflow_starts_a_new_page() {
        let items = vec![Size::inches(5.0, 7.0); 3];
        assert_eq!(auto_layout(&letter(), &items, 9.0).unwrap().len(), 3);
    }

    #[test]
    fn oversized_items_rotate_or_fail() {
        let wide = [Size::inches(10.0, 7.0)];
        let p = auto_layout(&letter(), &wide, 0.0).unwrap();
        assert!(p[0][0].1.h > p[0][0].1.w);
        assert_eq!(auto_layout(&letter(), &[Size::inches(20.0, 20.0)], 0.0), Err(LayoutError::DoesNotFit(0)));
        assert!(auto_layout(&letter(), &[Size::new(f32::NAN, 1.0)], 0.0).is_err());
        assert!(auto_layout(&letter(), &[Size::new(10.0, 10.0)], -1.0).is_err());
    }
}
