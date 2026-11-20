use super::*;

fn package() -> Layout {
    Layout { style: Style::PicturePackage, ..Default::default() }
}

fn assert_clean(l: &Layout) {
    let area = l.printable().unwrap();
    for c in &l.cells {
        assert!(area.contains(&c.rect), "{c:?} outside {area:?}");
        assert!(c.page < l.pages);
    }
    assert!(!l.has_overlaps(), "{:#?}", l.cells);
}

#[test]
fn papers_and_orientation() {
    let mut p = PageSetup::default();
    assert_eq!(p.size().unwrap(), (8.5, 11.0));
    p.orientation = Orientation::Landscape;
    assert_eq!(p.size().unwrap(), (11.0, 8.5));
    assert_eq!(p.size_points().unwrap(), (792.0, 612.0));
    p.paper = "A4".into();
    let (w, h) = p.size().unwrap();
    assert!((w - 11.693).abs() < 1e-3 && (h - 8.268).abs() < 1e-3);
    for n in paper_names() {
        p.paper = n.into();
        assert!(p.printable().is_ok(), "{n}");
    }
    p.paper = "custom".into();
    p.custom_width = 20.0;
    p.custom_height = 30.0;
    assert_eq!(p.size().unwrap(), (30.0, 20.0));
    p.paper = "poster".into();
    assert!(p.size().is_err());
}

/// Japanese photo and postcard papers: exact metric sizes to 0.1 pt, the customary names as
/// aliases, and the ISO/US papers around them unchanged.
#[test]
fn japanese_photo_and_postcard_papers() {
    let pt = |name: &str| {
        let p = PageSetup { paper: name.into(), ..Default::default() };
        p.size_points().unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    for (name, w_mm, h_mm) in [
        ("L判", 89.0, 127.0),
        ("2L判", 127.0, 178.0),
        ("KG判", 102.0, 152.0),
        ("はがき", 100.0, 148.0),
        ("往復はがき", 148.0, 200.0),
        ("六切", 203.0, 254.0),
        ("四切", 254.0, 305.0),
        ("名刺", 55.0, 91.0),
    ] {
        let (w, h) = pt(name);
        let want = (w_mm / 25.4 * 72.0, h_mm / 25.4 * 72.0);
        assert!((w - want.0).abs() < 0.1 && (h - want.1).abs() < 0.1, "{name}: {w} x {h} pt, wanted {want:?}");
        assert!((w - h).abs() > 1e-6, "{name} is portrait");
    }
    // The customary and English spellings resolve to the same paper, including spaces and case.
    for (a, b) in [
        ("L判", "l"),
        ("L判", " L "),
        ("L判", "L-size"),
        ("2L判", "2l"),
        ("KG判", "kg"),
        ("はがき", "postcard"),
        ("はがき", "ハガキ"),
        ("往復はがき", "double-postcard"),
        ("六切", "6cut"),
        ("六切", "6切"),
        ("四切", "4cut"),
        ("名刺", "business-card"),
        ("A4", "a 4"),
        ("4 × 6", "4x6"),
    ] {
        assert_eq!(paper_size(a), paper_size(b), "{a} vs {b}");
    }
    // Punctuation is ignored, including the periods and Japanese marks a pasted name may carry.
    for (name, canonical) in [("L判。", "l"), ("L判、", "l"), ("（L判）", "l"), ("l-size.", "l"), ("はがき。", "postcard"), ("6切。", "6cut")]
    {
        assert_eq!(paper_name(name), Some(canonical), "{name}");
    }
    assert_eq!(paper_name("custom"), None);
    assert!(paper_size("poster").is_none());
    // The papers that were already there keep their sizes.
    assert_eq!(pt("letter"), (612.0, 792.0));
    assert_eq!(pt("4x6"), (288.0, 432.0));
    assert_eq!(pt("5x7"), (360.0, 504.0));
    assert_eq!(pt("8x10"), (576.0, 720.0));
    assert_eq!(pt("13x19"), (936.0, 1368.0));
    let (w, h) = pt("A4");
    assert!((w - 595.28).abs() < 0.01 && (h - 841.89).abs() < 0.01, "A4 {w} x {h}");
}

#[test]
fn hostile_numbers_are_rejected() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e9] {
        let p = PageSetup { paper: "custom".into(), custom_width: bad, ..Default::default() };
        assert!(p.size().is_err(), "{bad}");
        let p = PageSetup { margins: Margins { left: bad, ..Margins::default() }, ..Default::default() };
        assert!(p.printable().is_err(), "{bad}");
        let mut l = package();
        assert!(l.add_cell(bad, 2.0).is_err(), "{bad}");
        assert!(l.add_cell(2.0, bad).is_err());
        let id = l.add_cell(2.0, 2.0).unwrap();
        assert!(bad.is_finite() || l.move_cell(id, bad, 0.0).is_err());
        assert!(l.resize_cell(id, Rect::new(0.0, 0.0, bad, 1.0)).is_err());
        assert!(bad.is_finite() || l.snap_move(id, Rect::new(bad, 0.0, 1.0, 1.0), Snap::Cells, 0.25, 0.1).is_err());
        let mut g = Layout { spacing: bad, ..package() };
        assert!(g.validate().is_err());
        assert!(pack(&[(1.0, 1.0)], l.printable().unwrap(), bad, true).is_err());
        assert!(pack(&[(bad, 1.0)], l.printable().unwrap(), 0.0, true).is_err());
    }
    // Margins that leave nothing.
    let p = PageSetup { margins: Margins::uniform(4.5), ..Default::default() };
    assert!(p.printable().is_err());
    let mut l = Layout { grid: GridSpec { columns: 0, ..Default::default() }, ..Default::default() };
    assert!(l.validate().is_err());
    l.grid.columns = 21;
    assert!(l.validate().is_err());
}

#[test]
fn units_convert() {
    for u in [Unit::Inches, Unit::Centimeters, Unit::Millimeters, Unit::Points] {
        assert!((u.to_inches(u.from_inches(3.5)) - 3.5).abs() < 1e-12);
    }
    assert!((Unit::Millimeters.from_inches(1.0) - 25.4).abs() < 1e-12);
    assert_eq!(Unit::Points.from_inches(2.0), 144.0);
    assert_eq!(serde_json::to_string(&Unit::Centimeters).unwrap(), "\"cm\"");
}

#[test]
fn contact_grid_fills_the_printable_area() {
    let l = Layout {
        style: Style::ContactSheet,
        grid: GridSpec { columns: 3, rows: 4, spacing_x: 0.2, spacing_y: 0.1, keep_square: false },
        ..Default::default()
    };
    let area = l.printable().unwrap();
    let cells = l.grid_rects().unwrap();
    assert_eq!(cells.len(), 12);
    let first = cells.first().unwrap();
    let last = cells.last().unwrap();
    assert!((first.x - area.x).abs() < 1e-9 && (first.y - area.y).abs() < 1e-9);
    assert!((last.right() - area.right()).abs() < 1e-9 && (last.bottom() - area.bottom()).abs() < 1e-9);
    for (i, a) in cells.iter().enumerate() {
        assert!((a.w - first.w).abs() < 1e-12 && (a.h - first.h).abs() < 1e-12);
        for b in cells.iter().skip(i + 1) {
            assert!(!a.overlaps(b));
        }
    }
    // Square cells are centred.
    let sq = Layout { grid: GridSpec { keep_square: true, ..l.grid }, ..l.clone() };
    let cells = sq.grid_rects().unwrap();
    assert!(cells.iter().all(|c| (c.w - c.h).abs() < 1e-12));
    let (lo, hi) = (cells.iter().map(|c| c.x).fold(f64::MAX, f64::min), cells.iter().map(|c| c.right()).fold(f64::MIN, f64::max));
    assert!(((lo - area.x) - (area.right() - hi)).abs() < 1e-9);
    // Single image ignores the grid.
    let one = Layout { style: Style::SingleImage, ..l };
    assert_eq!(one.grid_rects().unwrap(), vec![area]);
}

#[test]
fn page_counts() {
    let mut l = Layout { style: Style::ContactSheet, grid: GridSpec { columns: 2, rows: 2, ..Default::default() }, ..Default::default() };
    assert_eq!(l.page_count(0), 1);
    assert_eq!(l.page_count(4), 1);
    assert_eq!(l.page_count(5), 2);
    l.style = Style::SingleImage;
    assert_eq!(l.page_count(3), 3);
    l.style = Style::PicturePackage;
    l.pages = 4;
    assert_eq!(l.page_count(99), 4);
}

#[test]
fn packer_is_deterministic_and_clean() {
    let area = Layout::default().printable().unwrap();
    let sizes: Vec<(f64, f64)> =
        [(4.0, 6.0), (5.0, 7.0), (2.5, 3.5), (2.5, 3.5), (3.5, 5.0), (8.0, 10.0), (4.0, 6.0), (2.0, 2.0), (1.0, 1.5), (5.0, 7.0), (4.0, 6.0)]
            .to_vec();
    let a = pack(&sizes, area, 0.1, true).unwrap();
    assert_eq!(a, pack(&sizes, area, 0.1, true).unwrap());
    assert_eq!(a.len(), sizes.len());
    for (i, (p, r, turned)) in a.iter().enumerate() {
        assert!(area.contains(r), "{r:?}");
        let (w, h) = sizes[i];
        let expect = if *turned { (h, w) } else { (w, h) };
        assert!((r.w - expect.0).abs() < 1e-9 && (r.h - expect.1).abs() < 1e-9);
        for (j, (q, s, _)) in a.iter().enumerate().skip(i + 1) {
            if p == q {
                // The spacing is kept between neighbours.
                let grown = Rect::new(r.x, r.y, r.w + 0.1 - 2e-6, r.h + 0.1 - 2e-6);
                assert!(!grown.overlaps(s), "{i} and {j}: {r:?} {s:?}");
            }
        }
    }
    // The input order does not hide a block that needs a page of its own.
    assert!(a.iter().map(|p| p.0).max().unwrap() >= 1);
}

#[test]
fn packer_rotates_only_when_needed_and_refuses_oversize() {
    let area = Rect::new(0.25, 0.25, 8.0, 10.5);
    // 10 x 5 only fits a portrait page turned.
    let r = pack(&[(10.0, 5.0)], area, 0.0, true).unwrap();
    assert!(r[0].2 && (r[0].1.w - 5.0).abs() < 1e-9 && (r[0].1.h - 10.0).abs() < 1e-9);
    assert!(pack(&[(10.0, 5.0)], area, 0.0, false).is_err());
    let r = pack(&[(4.0, 6.0)], area, 0.0, true).unwrap();
    assert!(!r[0].2, "a block that fits as given is not turned");
    assert!(pack(&[(9.0, 11.0)], area, 0.0, true).is_err());
    // A block that exactly fills the area fits despite the spacing.
    assert!(pack(&[(8.0, 10.5)], area, 0.2, true).is_ok());
    assert!(pack(&vec![(1.0, 1.0); MAX_CELLS + 1], area, 0.0, true).is_err());
    // More pages than allowed.
    assert!(pack(&vec![(8.0, 10.5); 51], area, 0.0, true).is_err());
    assert_eq!(pack(&vec![(8.0, 10.5); 50], area, 0.0, true).unwrap().iter().map(|p| p.0).max(), Some(49));
}

#[test]
fn packing_is_dense() {
    // Four 4x5.25 cells tile a letter page's 8 x 10.5 area exactly.
    let area = Rect::new(0.25, 0.25, 8.0, 10.5);
    let r = pack(&[(4.0, 5.25); 4], area, 0.0, false).unwrap();
    assert!(r.iter().all(|p| p.0 == 0));
    // Eight 4x6 photos fit two pages (four per page with the rotation trick is impossible: 4x6 x2 per column).
    let r = pack(&[(4.0, 6.0); 4], area, 0.0, true).unwrap();
    assert_eq!(r.iter().map(|p| p.0).max(), Some(1));
}

#[test]
fn add_cell_fills_gaps_then_opens_pages() {
    let mut l = package();
    let a = l.add_cell(3.9, 6.0).unwrap();
    let b = l.add_cell(3.9, 6.0).unwrap();
    assert_eq!(l.cell(a).unwrap().rect.x, 0.25);
    assert_ne!(l.cell(a).unwrap().rect, l.cell(b).unwrap().rect);
    assert_clean(&l);
    // Keep adding until a second page appears.
    for _ in 0..6 {
        l.add_cell(3.9, 6.0).unwrap();
    }
    assert!(l.pages >= 2);
    assert_clean(&l);
    assert_eq!(l.cells.len(), 8);
    assert!(l.add_cell(9.0, 9.0).is_err(), "wider than the printable area");
    let mut grid = Layout::default();
    assert!(grid.add_cell(1.0, 1.0).is_err());
}

#[test]
fn cell_cap() {
    let mut l = package();
    for _ in 0..MAX_CELLS {
        if l.add_cell(0.5, 0.5).is_err() {
            break;
        }
    }
    assert!(l.cells.len() <= MAX_CELLS);
    let n = l.cells.len();
    if n == MAX_CELLS {
        assert!(l.add_cell(0.5, 0.5).is_err());
    }
}

#[test]
fn move_and_resize_stay_inside() {
    let mut l = package();
    let id = l.add_cell(2.0, 3.0).unwrap();
    let r = l.move_cell(id, 100.0, -5.0).unwrap();
    let area = l.printable().unwrap();
    assert!((r.right() - area.right()).abs() < 1e-9 && (r.y - area.y).abs() < 1e-9);
    let r = l.resize_cell(id, Rect::new(7.0, 9.0, 50.0, 50.0)).unwrap();
    assert!(area.contains(&r));
    assert!(l.resize_cell(id, Rect::new(0.0, 0.0, 0.0, 1.0)).is_err());
    assert!(l.move_cell(77, 0.0, 0.0).is_err());
    l.delete_cell(id).unwrap();
    assert!(l.delete_cell(id).is_err());
}

#[test]
fn stacking_order() {
    let mut l = Layout { style: Style::CustomPackage, ..Default::default() };
    let ids: Vec<u32> = (0..4).map(|_| l.add_cell(1.0, 1.0).unwrap()).collect();
    let order = |l: &Layout| l.cells.iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(order(&l), ids);
    l.restack(ids[0], Stack::ToFront).unwrap();
    assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
    l.restack(ids[0], Stack::Backward).unwrap();
    assert_eq!(order(&l), vec![ids[1], ids[2], ids[0], ids[3]]);
    l.restack(ids[0], Stack::Forward).unwrap();
    assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
    l.restack(ids[0], Stack::Forward).unwrap();
    assert_eq!(order(&l), vec![ids[1], ids[2], ids[3], ids[0]]);
    l.restack(ids[0], Stack::ToBack).unwrap();
    assert_eq!(order(&l), ids);
    l.restack(ids[0], Stack::Backward).unwrap();
    assert_eq!(order(&l), ids);
    assert!(l.restack(99, Stack::ToBack).is_err());
}

#[test]
fn stacking_ignores_other_pages() {
    let mut l = Layout { style: Style::CustomPackage, pages: 2, ..Default::default() };
    let a = l.add_cell(1.0, 1.0).unwrap();
    let b = l.add_cell(1.0, 1.0).unwrap();
    let c = l.add_cell(1.0, 1.0).unwrap();
    l.set_cell_page(b, 1).unwrap();
    l.restack(a, Stack::ToFront).unwrap();
    // `a` goes past `c` (same page) and not necessarily past `b`.
    let pos = |id| l.cells.iter().position(|x| x.id == id).unwrap();
    assert!(pos(a) > pos(c));
}

#[test]
fn overlap_is_allowed_by_move_and_reported() {
    let mut l = Layout { style: Style::CustomPackage, ..Default::default() };
    let a = l.add_cell(3.0, 3.0).unwrap();
    let b = l.add_cell(3.0, 3.0).unwrap();
    assert!(!l.has_overlaps());
    let r = l.cell(a).unwrap().rect;
    l.move_cell(b, r.x + 1.0, r.y + 1.0).unwrap();
    assert!(l.has_overlaps());
    // Touching edges are not an overlap.
    l.move_cell(b, r.right(), r.y).unwrap();
    assert!(!l.has_overlaps());
}

#[test]
fn auto_layout_repacks_cleanly_and_keeps_photos() {
    let mut l = package();
    for (w, h) in [(4.0, 6.0), (5.0, 7.0), (2.5, 3.5), (8.0, 10.0), (4.0, 6.0), (3.5, 5.0)] {
        l.add_cell(w, h).unwrap();
    }
    let first = l.cells[0].id;
    l.set_cell_photo(first, Some(42)).unwrap();
    // Scramble positions to overlap, then auto layout.
    for c in &mut l.cells {
        c.rect.x = 0.25;
        c.rect.y = 0.25;
        c.page = 0;
    }
    l.pages = 1;
    assert!(l.has_overlaps());
    l.auto_layout().unwrap();
    assert_clean(&l);
    assert_eq!(l.cell(first).unwrap().photo, Some(42));
    assert_eq!(l.cells.len(), 6);
    let again = l.clone();
    let mut l2 = again.clone();
    l2.auto_layout().unwrap();
    l.auto_layout().unwrap();
    assert_eq!(l, l2);
}

#[test]
fn auto_layout_failure_changes_nothing() {
    let mut l = package();
    l.add_cell(2.0, 2.0).unwrap();
    l.cells.push(Cell { id: 50, page: 0, rect: Rect::new(0.25, 0.25, 40.0, 40.0), photo: None });
    let before = l.clone();
    assert!(l.auto_layout().is_err());
    assert_eq!(l, before);
}

#[test]
fn clear_and_new_page() {
    let mut l = package();
    l.add_cell(2.0, 2.0).unwrap();
    assert_eq!(l.new_page().unwrap(), 1);
    assert_eq!(l.pages, 2);
    l.clear();
    assert_eq!((l.cells.len(), l.pages), (0, 1));
    for _ in 1..MAX_PAGES {
        l.new_page().unwrap();
    }
    assert!(l.new_page().is_err());
    let mut g = Layout::default();
    assert!(g.new_page().is_err());
}

#[test]
fn validate_catches_bad_cells() {
    let mut l = package();
    l.add_cell(2.0, 2.0).unwrap();
    let ok = l.clone();
    let mut bad = ok.clone();
    bad.cells[0].rect.w = f64::NAN;
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.cells[0].page = 3;
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.cells[0].rect.x = 50.0;
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.cells.push(bad.cells[0].clone());
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.pages = 0;
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.next_id = 0;
    bad.validate().unwrap();
    assert!(bad.next_id > bad.cells[0].id);
    assert!(serde_json::from_str::<Layout>(r#"{"surprise":1}"#).is_err());
    let mut round: Layout = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
    round.validate().unwrap();
    assert_eq!(round, ok);
}

#[test]
fn snapping_to_cells_grid_and_margins() {
    let mut l = Layout { style: Style::CustomPackage, spacing: 0.0, ..Default::default() };
    let a = l.add_cell(3.0, 3.0).unwrap();
    let b = l.add_cell(2.0, 2.0).unwrap();
    let ra = l.cell(a).unwrap().rect;
    // Left edge of b near the right edge of a.
    let m = l.snap_move(b, Rect::new(ra.right() + 0.04, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
    assert!((m.x - ra.right()).abs() < 1e-9);
    assert_eq!(m.y, 5.0);
    // Too far: untouched.
    let m = l.snap_move(b, Rect::new(ra.right() + 0.5, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
    assert!((m.x - (ra.right() + 0.5)).abs() < 1e-9);
    // Right edge snaps to the printable margin.
    let area = l.printable().unwrap();
    let m = l.snap_move(b, Rect::new(area.right() - 2.0 - 0.03, 5.0, 2.0, 2.0), Snap::Cells, 0.25, 0.1).unwrap();
    assert!((m.right() - area.right()).abs() < 1e-9);
    // Off leaves everything.
    let m = l.snap_move(b, Rect::new(ra.right() + 0.04, 5.0, 2.0, 2.0), Snap::Off, 0.25, 0.1).unwrap();
    assert!((m.x - (ra.right() + 0.04)).abs() < 1e-9);
    // Grid snapping lands on multiples from the printable corner.
    let m = l.snap_move(b, Rect::new(1.37, 2.62, 2.0, 2.0), Snap::Grid, 0.5, 0.2).unwrap();
    assert!(((m.x - area.x) / 0.5).fract().abs() < 1e-9 || (((m.x - area.x) / 0.5).fract() - 1.0).abs() < 1e-9);
    // Resize snaps the dragged corner only.
    let r = l.snap_resize(b, Rect::new(5.0, 5.0, 2.1, 1.9), Snap::Grid, 0.5, 0.2).unwrap();
    assert_eq!((r.x, r.y), (5.0, 5.0));
    assert!(((r.right() - area.x) / 0.5 - ((r.right() - area.x) / 0.5).round()).abs() < 1e-9);
    // Never below the minimum size.
    let r = l.snap_resize(b, Rect::new(5.0, 5.0, 0.0, 0.0), Snap::Grid, 0.5, 0.2).unwrap();
    assert!(r.w >= MIN_CELL_IN && r.h >= MIN_CELL_IN);
    assert!(l.snap_move(99, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Cells, 0.25, 0.1).is_err());
    assert!(l.snap_move(b, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Grid, 0.0, 0.1).is_err());
    assert!(l.snap_move(b, Rect::new(0.0, 0.0, 1.0, 1.0), Snap::Grid, f64::NAN, 0.1).is_err());
}

#[test]
fn fit_cells_follows_the_margins() {
    let mut l = package();
    let id = l.add_cell(7.0, 10.0).unwrap();
    l.page.margins = Margins::uniform(1.0);
    assert!(l.validate().is_err(), "the cell no longer fits");
    l.fit_cells().unwrap();
    l.validate().unwrap();
    let r = l.cell(id).unwrap().rect;
    assert!(l.printable().unwrap().contains(&r));
    assert!((r.w - 6.5).abs() < 1e-9 && (r.h - 9.0).abs() < 1e-9);
    l.page.paper = "poster".into();
    assert!(l.fit_cells().is_err());
}

#[test]
fn cell_aspect_survives_auto_layout() {
    let mut l = package();
    let id = l.add_cell(2.5, 3.5).unwrap();
    l.auto_layout().unwrap();
    let r = l.cell(id).unwrap().rect;
    assert!((r.w / r.h - 2.5 / 3.5).abs() < 1e-9 || (r.h / r.w - 2.5 / 3.5).abs() < 1e-9);
}

#[test]
fn guides_validate() {
    assert!(Guides::default().validate().is_ok());
    for bad in [f64::NAN, 0.0, -1.0, 1e9] {
        assert!(Guides { grid_size: bad, ..Default::default() }.validate().is_err());
    }
}

#[test]
fn rect_helpers() {
    let a = Rect::new(0.0, 0.0, 2.0, 2.0);
    assert!(a.overlaps(&Rect::new(1.0, 1.0, 2.0, 2.0)));
    assert!(!a.overlaps(&Rect::new(2.0, 0.0, 1.0, 1.0)));
    assert!(a.contains(&Rect::new(0.5, 0.5, 1.0, 1.0)));
    assert!(!a.contains(&Rect::new(1.5, 0.5, 1.0, 1.0)));
    assert_eq!(a.area(), 4.0);
}
