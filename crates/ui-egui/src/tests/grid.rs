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

#[test]
fn grid_stack_badge_toggles_a_stack_that_is_not_selected() {
    let mut h = grid(ViewMode::PhotoGrid);
    // a stack on screen, and a loose photo as the active one
    let top = PhotoId(2001);
    select(&mut h, top);
    let vis = h.app.session.visible().to_vec();
    let i = vis.iter().position(|p| *p == top).unwrap();
    let loose = vis[i.saturating_sub(1)..].iter().copied().find(|p| h.app.session.catalog.stack_of(*p).is_none()).unwrap();
    select(&mut h, loose);
    assert!(!h.app.session.selection.contains(top));
    assert!(widget(&h, &format!("stack:{}", top.0)).is_some(), "badge drawn");
    let r = h.request("ui.clickWidget", json!({"id": format!("stack:{}", top.0)}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.catalog.stack_of(top).unwrap().collapsed, "a click on an unselected stack's badge toggles it");
    assert_eq!((h.app.session.selection.ids.clone(), h.app.session.selection.active), (vec![loose], Some(loose)), "and leaves the selection alone");
}

/// What the grid shows (its date runs, and each cell's shape) is what a grid made afresh for the
/// same list shows.
fn assert_as_made_afresh(h: &mut Headless, what: &str) {
    let vis = h.app.session.visible().to_vec();
    if vis.is_empty() {
        // (no photo matches: the grid says so and draws none)
        return;
    }
    let (runs, shapes) = h.app.caches.grid.runs_and_shapes().expect("the grid was drawn");
    let cat = &h.app.session.catalog;
    let by = crate::panels::grid::resolve_group(h.app.session.sort.group, h.app.ui.thumb_size);
    assert_eq!(*runs, cat.date_runs(&vis, h.app.session.sort.key, by), "date runs {what}");
    // (the square grid's cells are all alike: it reads no shape)
    let fresh: Vec<f32> =
        if h.app.ui.view == ViewMode::SquareGrid { Vec::new() } else { vis.iter().map(|id| crate::panels::grid::shape_of(cat, *id)).collect() };
    assert_eq!(shapes, fresh, "cell shapes {what}");
}

fn photo(h: &mut Headless, n: u64, captured: &str) -> Op {
    let id = h.app.session.catalog.alloc_photo_id();
    let (w, h_) = if n.is_multiple_of(3) { (4000, 6000) } else { (6000, 4000) };
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/lightcraft-grid-test/NEW_{n:05}.jpg") },
        &format!("NEW_{n:05}.jpg"),
        "JPEG",
        w,
        h_,
        "2026-01-02T00:00:00",
    );
    p.captured = Some(captured.to_string());
    Op::AddPhoto { photo: Box::new(p) }
}

/// An import lands its photos a batch at a time: the grid reads the batch's photos, not the
/// whole list again (at 180,000 photos that was most of a 100 ms frame per batch). An edit to a
/// photo's date or shape reads that photo.
#[test]
fn a_batch_of_new_photos_or_an_edit_to_one_is_read_photo_by_photo() {
    let mut h = grid(ViewMode::PhotoGrid);
    let read = h.app.caches.grid_stats.photos_read;
    assert_eq!(read, N, "the first frame reads every photo");
    // sixteen photos, on days already shown, a new day, and with no date at all
    let ops: Vec<Op> = (0..16)
        .map(|k| {
            let day = ["2026-12-28T09:30:00", "2026-11-03T10:00:00", "2027-01-05T12:00:00", "nope"][k as usize % 4];
            photo(&mut h, k, day)
        })
        .collect();
    h.app.session.commit("Import", Op::Batch { ops }).unwrap();
    h.step();
    assert_eq!(h.app.caches.grid_stats.photos_read - read, 16, "only the batch's photos are read");
    assert!(widget(&h, "group:2027-01-05").is_some(), "the new day heads the grid");
    assert_as_made_afresh(&mut h, "after the batch");
    // a photo taken on another day: it moves there, and only it is read
    let read = h.app.caches.grid_stats.photos_read;
    h.app.session.commit("Capture time", Op::SetCaptured { id: PhotoId(500), captured: Some("2027-02-01T08:00:00".into()) }).unwrap();
    h.step();
    assert_eq!(h.app.caches.grid_stats.photos_read - read, 1, "only the photo edited is read");
    assert!(widget(&h, "group:2027-02-01").is_some(), "its new day heads the grid");
    assert_as_made_afresh(&mut h, "after a capture time");
}

/// Whatever the edit, the grid shows what a grid made afresh would: followed photo by photo when
/// the list changed by the photos the edit touched, else read whole.
#[test]
fn a_grid_followed_through_edits_shows_what_a_grid_made_afresh_shows() {
    for seed in [11u64, 12, 13, 14, 15] {
        let mut h = grid(ViewMode::PhotoGrid);
        let mut dice = seed;
        let mut roll = |sides: u64| {
            dice = lightcraft_catalog::mix64(dice);
            dice % sides
        };
        for step in 0..120 {
            let vis = h.app.session.visible().to_vec();
            if vis.is_empty() {
                // (no picks shown: back to every photo)
                h.app.session.filter.flag = None;
                continue;
            }
            let id = vis[roll(vis.len() as u64) as usize].0;
            let n = roll(1000);
            match roll(12) {
                0 => {
                    let ops: Vec<Op> = (0..1 + roll(20))
                        .map(|k| photo(&mut h, n * 100 + k, &format!("2026-{:02}-{:02}T10:00:00", 1 + n % 12, 1 + k % 28)))
                        .collect();
                    h.app.session.commit("Import", Op::Batch { ops }).unwrap();
                }
                1 => {
                    let _ = h.app.session.commit("Delete", Op::SetDeleted { id: PhotoId(id), deleted: true });
                }
                2 => {
                    let _ = h
                        .app
                        .session
                        .commit("Capture time", Op::SetCaptured { id: PhotoId(id), captured: Some(format!("2026-{:02}-03T10:00:00", 1 + n % 12)) });
                }
                3 => {
                    h.app.session.execute("library.select", &json!({"ids": [id]})).unwrap();
                    let r = h.app.session.execute("photo.rotateRight", &json!({}));
                    assert!(r.is_ok(), "{r:?}");
                }
                4 => {
                    let mut d = (*h.app.session.catalog.photo(PhotoId(id)).unwrap().develop).clone();
                    d.crop.geometry.rect = lightcraft_geom::Rect::new(0.1, 0.0, 0.9 - 0.1 * (n % 4) as f64, 1.0);
                    h.app
                        .session
                        .commit("Crop", Op::SetDevelop { id: PhotoId(id), settings: std::sync::Arc::new(d), label: "Crop".into(), edited: None })
                        .unwrap();
                }
                5 => h.app.session.commit("Rating", Op::SetRating { id: PhotoId(id), rating: (n % 6) as u8 }).unwrap(),
                6 => {
                    let key = ["captureDate", "fileName", "importDate"][n as usize % 3];
                    let r = h.app.session.execute("library.sort", &json!({"key": key, "ascending": n.is_multiple_of(2)}));
                    assert!(r.is_ok(), "{r:?}");
                }
                7 => {
                    // a stack: an edit to more than its photos, read whole
                    let other = vis[n as usize % vis.len()].0;
                    let _ = h.app.session.execute("stack.group", &json!({"ids": [id, other], "top": id, "collapsed": n.is_multiple_of(2)}));
                }
                8 => {
                    let _ = h.app.session.execute("edit.undo", &json!({}));
                }
                9 => {
                    // picks, for the filter below to show some
                    for k in 0..1 + n % 40 {
                        let p = vis[(n as usize * 7 + k as usize * 13) % vis.len()];
                        h.app.session.commit("Flag", Op::SetFlag { id: p, flag: lightcraft_catalog::Flag::Pick }).unwrap();
                    }
                }
                10 => h.app.ui.view = if h.app.ui.view == ViewMode::PhotoGrid { ViewMode::SquareGrid } else { ViewMode::PhotoGrid },
                _ => h.app.session.filter.flag = n.is_multiple_of(2).then_some(lightcraft_catalog::Flag::Pick),
            }
            h.step();
            assert_as_made_afresh(&mut h, &format!("seed {seed}, after step {step}"));
        }
    }
}
