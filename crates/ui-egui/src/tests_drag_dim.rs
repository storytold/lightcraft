//! Photos dragged from the grid step back while the drag lasts: their thumbnails are painted darker,
//! the others stay as they are, and the grid is as before once the drag ends.

use std::time::Duration;

use egui::{Color32, ColorImage, Pos2};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(120);

fn app(view: ViewMode) -> Headless {
    marked_app(view, |_| {})
}

/// The fixture, with `mark` applied to each photo before it is added (a flag, a virtual copy's name...).
fn marked_app(view: ViewMode, mark: impl Fn(&mut Photo)) -> Headless {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=6 {
        let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 1 }, &format!("{id}.jpg"), "JPEG", 640, 480, "2026-10-08");
        mark(&mut p);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true, "group": "none"})).unwrap();
    // two photos selected: the drag carries both
    s.execute("library.select", &json!({"ids": [1, 2]})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.app.ui.view = view;
    h.app.ui.right = crate::state::RightPanel::None;
    h.app.ui.thumb_size = 160.0;
    // the thumbnails' real textures, so a darker pixel is the dimming and not a placeholder
    assert!(h.step_until(SETTLE, |h| h.app.renderer.thumb_textures() == 6), "all fixture thumbnails must be ready");
    assert!(h.settle(SETTLE), "fixture did not settle");
    h
}

fn centre(h: &Headless, id: u64) -> Pos2 {
    h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{id}")).unwrap().1.center()
}

fn pixel(image: &ColorImage, p: Pos2) -> Color32 {
    image.pixels[p.y as usize * image.size[0] + p.x as usize]
}

fn brightness(c: Color32) -> u32 {
    u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b())
}

fn pointer(h: &mut Headless, at: Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

#[test]
fn the_photos_being_dragged_are_dimmed_and_only_those() {
    for view in [ViewMode::SquareGrid, ViewMode::PhotoGrid] {
        let mut h = app(view);
        let before = h.paint();
        // When photo 1 (of the selected 1 and 2) is picked up and carried over the grid
        let (from, to) = (centre(&h, 1), centre(&h, 6));
        pointer(&mut h, from, true);
        for i in 1..=8 {
            h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
            h.step();
        }
        assert_eq!(h.app.ui.dragging_photos, Some(vec![1, 2]), "{view:?}: the drag carries the selection");
        let during = h.paint();
        // Then both are darker, and a photo that isn't in it is untouched
        for dragged in [1, 2] {
            let at = centre(&h, dragged);
            assert!(brightness(pixel(&during, at)) < brightness(pixel(&before, at)), "{view:?}: photo {dragged} is dimmed");
        }
        let other = centre(&h, 3);
        assert_eq!(pixel(&during, other), pixel(&before, other), "{view:?}: a photo that isn't dragged keeps its colours");
        // and a change of the selection in mid-drag moves nothing: what is dimmed is what a drop would carry
        h.app.run("library.select", json!({"ids": [3]})).unwrap();
        h.step();
        let reselected = h.paint();
        assert_eq!(h.app.ui.dragging_photos, Some(vec![1, 2]), "{view:?}: the drag still carries photos 1 and 2");
        for dragged in [1, 2] {
            let at = centre(&h, dragged);
            assert!(brightness(pixel(&reselected, at)) < brightness(pixel(&before, at)), "{view:?}: photo {dragged} is still dimmed");
        }
        assert_eq!(pixel(&reselected, other), pixel(&before, other), "{view:?}: photo 3 is selected but not dragged, and isn't dimmed");
        // and once the button is let go, they are as they were (with the selection they had, which tints a thumbnail itself)
        h.app.run("library.select", json!({"ids": [1, 2]})).unwrap();
        pointer(&mut h, to, false);
        // (the frame that sees the release ends the drag after the grid is painted, so the next one paints it undimmed)
        h.step();
        h.step();
        assert!(h.app.ui.dragging_photos.is_none(), "{view:?}: the drag ended");
        let after = h.paint();
        for id in [1, 2, 3] {
            let at = centre(&h, id);
            assert_eq!(pixel(&after, at), pixel(&before, at), "{view:?}: photo {id} is back to normal");
        }
    }
}

/// Carries the selection (photos 1 and 2) over the grid and paints a frame of it.
fn carry(h: &mut Headless) -> ColorImage {
    let (from, to) = (centre(h, 1), centre(h, 6));
    pointer(h, from, true);
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    assert_eq!(h.app.ui.dragging_photos, Some(vec![1, 2]), "the drag carries the selection");
    h.paint()
}

#[test]
fn a_dragged_rejected_photo_is_dimmed_once() {
    for view in [ViewMode::SquareGrid, ViewMode::PhotoGrid] {
        // Given photo 1 is rejected (its thumbnail is already dimmed) and photo 2 is not
        let mut h = marked_app(view, |p| {
            if p.id == PhotoId(1) {
                p.flag = lightcraft_catalog::Flag::Reject;
            }
        });
        let before = h.paint();
        // When both are carried
        let during = carry(&mut h);
        // Then the rejected one is no darker than it was: the two dimmings are one
        let at = centre(&h, 1);
        assert_eq!(pixel(&during, at), pixel(&before, at), "{view:?}: a dragged rejected photo is dimmed once");
        // (and its marks keep the dimming they have, nothing of it changes when the drag starts)
        let cell = h.app.widgets.iter().find(|(w, _)| *w == "thumb:1").unwrap().1;
        for y in (cell.top() as usize)..(cell.bottom() as usize) {
            for x in (cell.left() as usize)..(cell.right() as usize) {
                let p = Pos2::new(x as f32, y as f32);
                assert_eq!(pixel(&during, p), pixel(&before, p), "{view:?}: a dragged rejected photo looks as it did, at {x},{y}");
            }
        }
        assert!(brightness(pixel(&during, centre(&h, 2))) < brightness(pixel(&before, centre(&h, 2))), "{view:?}: the other dragged photo is dimmed");
    }
}

#[test]
fn a_dragged_photos_marks_are_not_dimmed() {
    for view in [ViewMode::SquareGrid, ViewMode::PhotoGrid] {
        // Given photo 2 is a virtual copy: its tag at the top right is white on a dark pill
        let mut h = marked_app(view, |p| {
            if p.id == PhotoId(2) {
                p.copy_name = Some("V1".into());
            }
        });
        let before = h.paint();
        let during = carry(&mut h);
        // Then the tag's white strokes are as bright as before, while the photo under it is dimmed
        let c = centre(&h, 2);
        let cell = h.app.widgets.iter().find(|(w, _)| *w == "thumb:2").unwrap().1;
        let brightest = |image: &ColorImage| {
            let top = if view == ViewMode::SquareGrid { cell.top() + 24.0 } else { cell.top() } + 4.0;
            let right = if view == ViewMode::SquareGrid { cell.right() - 10.0 } else { cell.right() } - 8.0;
            let mut best = 0;
            for y in (top as usize)..(top as usize + 20) {
                for x in ((right - 40.0) as usize)..(right as usize) {
                    best = best.max(brightness(pixel(image, Pos2::new(x as f32, y as f32))));
                }
            }
            best
        };
        assert!(brightness(pixel(&during, c)) < brightness(pixel(&before, c)), "{view:?}: the photo is dimmed");
        assert_eq!(brightest(&during), brightest(&before), "{view:?}: the virtual-copy tag keeps its brightness");
    }
}

#[test]
fn a_dragged_photos_selection_ring_is_not_dimmed() {
    // Given the square grid, where a thumbnail is letterboxed inside its cell and its ring runs through the dimmed area
    let mut h = app(ViewMode::SquareGrid);
    let before = h.paint();
    let during = carry(&mut h);
    for id in [1, 2] {
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{id}")).unwrap().1;
        let img = egui::Rect::from_min_max(cell.min + egui::vec2(10.0, 24.0), cell.max - egui::vec2(10.0, 10.0));
        let [tw, th] = h.app.renderer.thumb(PhotoId(id)).unwrap().size;
        let scale = (img.width() / tw as f32).min(img.height() / th as f32);
        let top = img.center().y - th as f32 * scale / 2.0;
        assert!(top - img.top() > 5.0, "the fixture's thumbnails are letterboxed");
        // the ring (selected: 2 points, the active photo's: 2 more away) is painted just above the picture
        let ring_brightest = |image: &ColorImage| {
            ((top as usize - 4)..top as usize).map(|y| brightness(pixel(image, Pos2::new(img.center().x, y as f32)))).max().unwrap()
        };
        // Then it is as bright as it was
        assert_eq!(ring_brightest(&during), ring_brightest(&before), "photo {id}: the selection ring keeps its brightness");
    }
}
