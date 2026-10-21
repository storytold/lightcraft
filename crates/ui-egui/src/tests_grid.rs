//! Headless tests of the photo grid on a larger synthetic library (issue #35): unchanged frames
//! reuse the view's date runs, layout and stack index and visit only the rows near the screen,
//! while date headers, scrolling to the active photo, keyboard navigation, stacks and
//! crop/orientation changes keep working.

use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source, Stack, StackId};
use lightcraft_engine::Session;
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(120);
const N: u64 = 3000;

/// `N` photos (missing files: thumbnails fail fast), 40 per capture day, landscape except every
/// fifth; a three-photo stack every 100 photos.
fn library() -> Session {
    let mut s = Session::new();
    for i in 0..N {
        let (w, h) = if i % 5 == 4 { (4000, 6000) } else { (6000, 4000) };
        let path = format!("/lightcraft-grid-test/IMG_{i:05}.jpg");
        let mut p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:05}.jpg"), "JPEG", w, h, "2026-01-01T00:00:00");
        let day = i / 40;
        p.captured = Some(format!("2026-{:02}-{:02}T{:02}:00:00", 12 - day / 28, 28 - day % 28, 8 + i % 40 / 4));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for k in 0..N / 100 {
        let b = k * 100 + 1;
        let stack = Stack { id: StackId(k + 1), photos: vec![PhotoId(b), PhotoId(b + 1), PhotoId(b + 2)], collapsed: false };
        s.catalog.apply(Op::AddStack { stack }).unwrap();
    }
    s
}

fn grid(view: ViewMode) -> Headless {
    let app = LightcraftApp::new(library(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = view;
    h.app.ui.thumb_size = 160.0;
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn select(h: &mut Headless, id: PhotoId) {
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// The photo's cell is drawn inside the grid's canvas.
fn on_screen(h: &Headless, id: PhotoId) -> bool {
    let canvas = h.app.canvas_rect.expect("grid drawn");
    widget(h, &format!("thumb:{}", id.0)).is_some_and(|r| canvas.contains_rect(r))
}

/// Unchanged frames build nothing and look only at the cells near the screen.
fn assert_cheap_frames(h: &mut Headless) {
    let before = h.app.caches.grid_stats.clone();
    let frames = 10;
    for _ in 0..frames {
        h.step();
    }
    let s = &h.app.caches.grid_stats;
    assert_eq!(s.frames - before.frames, frames);
    assert_eq!(s.view_builds, before.view_builds, "date runs rebuilt on an unchanged frame");
    assert_eq!(s.layout_builds, before.layout_builds, "layout rebuilt on an unchanged frame");
    assert_eq!(s.stack_index_builds, before.stack_index_builds, "stack index rebuilt on an unchanged frame");
    let per_frame = (s.cells_visited - before.cells_visited) / frames;
    assert!(per_frame > 0 && per_frame < 300, "visited {per_frame} of {N} cells per frame");
}

#[test]
fn unchanged_grid_frames_reuse_the_view_and_visit_only_nearby_rows() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut h = grid(view);
        assert_eq!(h.app.caches.grid_stats.view_builds, 1, "{view:?}");
        assert_eq!(h.app.caches.grid_stats.stack_index_builds, 1, "{view:?}");
        assert_cheap_frames(&mut h);
        // far down the grid too
        let far = h.app.session.visible()[2500];
        select(&mut h, far);
        assert_cheap_frames(&mut h);
    }
}

#[test]
fn grid_headers_selection_scrolling_stacks_and_shape_changes() {
    let mut h = grid(ViewMode::PhotoGrid);
    let vis = h.app.session.visible().to_vec();
    // the newest day's header heads the grid
    assert!(widget(&h, "group:2026-12-28").is_some(), "{:?}", h.app.widgets.iter().filter(|(w, _)| w.starts_with("group:")).collect::<Vec<_>>());
    assert_eq!(h.app.grid_scroll, Some(0.0));
    // selecting a photo far down scrolls the grid to it; its day's header is drawn (pinned or not)
    let target = PhotoId(2001); // the top of a stack, captured on day 50
    let i = vis.iter().position(|x| *x == target).unwrap();
    select(&mut h, target);
    assert!(h.app.grid_scroll.unwrap() > 1000.0, "{:?}", h.app.grid_scroll);
    assert!(on_screen(&h, target), "the active photo is scrolled into view");
    // stacks: the badge on the stack's top and members
    assert!(widget(&h, &format!("stack:{}", target.0)).is_some(), "stack badge drawn");
    assert!(widget(&h, "stack:2002").is_some(), "stack member badge drawn");
    // keyboard: the next photo becomes active and stays in view
    h.request("ui.key", json!({"key": "right"}), T);
    h.settle(SETTLE);
    let next = vis[i + 1];
    assert_eq!(h.app.session.selection.active, Some(next));
    assert!(on_screen(&h, next));
    // the user's scroll position is kept on unchanged frames
    let y = h.app.grid_scroll;
    assert_cheap_frames(&mut h);
    assert_eq!(h.app.grid_scroll, y);
    // rotating a landscape photo re-lays the grid with its new (portrait) shape
    let landscape = vis[i + 2];
    let before = widget(&h, &format!("thumb:{}", landscape.0)).unwrap();
    assert!(before.width() > before.height(), "{before:?}");
    let layouts = h.app.caches.grid_stats.layout_builds;
    let r = h.request("engine.execute", json!({"command": "photo.rotateRight", "params": {"ids": [landscape.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.caches.grid_stats.layout_builds > layouts, "a shape change re-lays the grid");
    let after = widget(&h, &format!("thumb:{}", landscape.0)).unwrap();
    assert!(after.height() > after.width(), "{after:?}");
    assert_cheap_frames(&mut h);
}

/// Selected photos whose thumbnails have not loaded (or can't) are still marked selected in the
/// justified grid (issue #298): the active one white, the others grey.
#[test]
fn unloaded_thumbnails_show_their_selection() {
    let mut h = grid(ViewMode::PhotoGrid);
    h.request("engine.execute", json!({"command": "library.selectAll"}), T);
    let img = h.snapshot(SETTLE);
    assert_eq!(h.view.ctx.pixels_per_point(), 1.0);
    let active = h.app.session.selection.active.expect("active");
    let canvas = h.app.canvas_rect.expect("grid drawn");
    let mut checked = 0;
    for (w, r) in h.app.widgets.iter().filter(|(w, _)| w.starts_with("thumb:")) {
        if !canvas.shrink(4.0).contains_rect(*r) || r.min.x < 6.0 {
            continue;
        }
        let id = w.trim_start_matches("thumb:").parse::<u64>().unwrap();
        let want = if id == active.0 { egui::Color32::WHITE } else { egui::Color32::from_gray(170) };
        let px = img[((r.min.x - 1.0) as usize, r.center().y as usize)];
        assert_eq!(px, want, "photo {id}");
        checked += 1;
    }
    assert!(checked >= 3, "checked {checked}");
}

/// A folder of unedited raws on a slow share: the grid must not decode every raw to draw a 512 px
/// thumbnail the embedded camera JPEG already covers. Measured on a 56 MB Fuji RAF over a 1.2 MB/s
/// share, that decode is 45 s a photo, and 149 of them queued in front of the render the user asked
/// for. The embedded JPEG stays a *stand-in* — replaced by a real render — whenever it is not the
/// picture the photo should show.
#[test]
fn an_unedited_raws_thumbnail_skips_the_sensor_decode() {
    use crate::panels::grid::skips_the_real_render;
    // unedited raw, embedded camera JPEG on screen: nothing behind it to render
    assert!(skips_the_real_render(true, true));
    // a cached render of the file was the stand-in, even for an unedited raw: the real one follows
    assert!(!skips_the_real_render(true, false));
    // edited photo, or a raw with no embedded JPEG: render it
    assert!(!skips_the_real_render(false, true));
    assert!(!skips_the_real_render(false, false));
}

/// Press and release the primary button at `at`, one frame apart from the next click (as a real
/// double-click comes in: one click per frame).
fn click_at(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerMoved(at));
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
}

/// Double-clicking a stack badge expands or collapses the stack, as in Lightroom (issue #2233): its
/// second click used to undo the first, and the stack stayed as it was. A single click still toggles it.
#[test]
fn double_clicking_a_stack_badge_toggles_the_stack_once() {
    let mut h = grid(ViewMode::PhotoGrid);
    let top = PhotoId(2001);
    let sid = StackId(21);
    let collapsed = |h: &Headless| h.app.session.catalog.stack(sid).unwrap().collapsed;
    select(&mut h, top);
    let r = h.request("engine.execute", json!({"command": "stack.toggle", "params": {"ids": [top.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(collapsed(&h));
    for want in [false, true] {
        let badge = widget(&h, &format!("stack:{}", top.0)).expect("stack badge drawn").center();
        click_at(&mut h, badge);
        // the first click re-lays the grid; the second must still land on this badge, or the test
        // would pass without the fix (a missed second click toggles nothing)
        let moved = widget(&h, &format!("stack:{}", top.0)).expect("stack badge drawn");
        assert!(moved.contains(badge), "the badge moved from under the pointer: {moved:?}");
        click_at(&mut h, badge);
        h.settle(SETTLE);
        assert_eq!(collapsed(&h), want, "a double-click toggles the stack once");
        assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "a double-click on the badge doesn't open the photo");
        // the next double-click is apart from this one
        for _ in 0..60 {
            h.step();
        }
    }
    let badge = widget(&h, &format!("stack:{}", top.0)).unwrap().center();
    click_at(&mut h, badge);
    h.settle(SETTLE);
    assert!(!collapsed(&h), "a single click toggles it");
}
