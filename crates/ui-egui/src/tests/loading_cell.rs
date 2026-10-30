//! A grid cell whose thumbnail is still on its way says so, quietly, and differs from one whose file
//! can't be read (which says "!"): until the failure is known the two were the same plain skeleton.
//! The cue is still (nothing to stop for a Reduce motion setting) and drawn from the theme's own colors.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;

use crate::headless::{Headless, RenderGate};
use crate::render::Slot;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(60);

/// Twelve photos whose files are not there, with every thumbnail render held until the returned gate
/// opens, and then failed (as an unreadable file's would): the cells are loading, then unreadable, at
/// the test's say.
fn app_with_held_thumbnails(view: ViewMode) -> (Headless, Rc<Cell<bool>>) {
    let mut s = Session::new();
    for i in 0..12u64 {
        let path = format!("/lightcraft-loading-cell-test/IMG_{i:03}.jpg");
        let p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:03}.jpg"), "JPEG", 6000, 4000, "2026-01-01T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1000.0, 700.0], 1.0);
    h.app.ui.thumb_size = 160.0;
    h.app.ui.view = view;
    let open = Rc::new(Cell::new(false));
    h.app.renderer.set_offload(Box::new(RenderGate::failing(open.clone(), |s| matches!(s, Slot::Thumb(_) | Slot::ThumbQuick(_)))));
    (h, open)
}

fn ids(h: &Headless, prefix: &str) -> Vec<String> {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).map(|(w, _)| w.clone()).collect()
}

/// Frames until the grid has its cells.
fn first_grid_frame(h: &mut Headless) {
    for _ in 0..20 {
        h.step();
        if !ids(h, "thumb:").is_empty() {
            return;
        }
    }
    panic!("the grid was never drawn");
}

/// Given a grid whose thumbnails have not arrived, when a frame is drawn, then each cell carries the loading cue; and once the
/// files turn out unreadable the cue is gone and the cells say so instead.
#[test]
fn a_cell_still_loading_differs_from_one_that_failed() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let (mut h, open) = app_with_held_thumbnails(view);
        first_grid_frame(&mut h);
        let loading = ids(&h, "badge:loading:");
        assert!(loading.len() >= 6, "{view:?}: the cells on screen are loading: {loading:?}");
        assert!(!h.painted_text().iter().any(|t| t == "!"), "{view:?}: none of them has failed yet");

        open.set(true);
        assert!(h.settle(SETTLE));
        assert!(ids(&h, "badge:loading:").is_empty(), "{view:?}: the cue goes once the failure is known: {:?}", ids(&h, "badge:loading:"));
        assert!(h.painted_text().iter().any(|t| t == "!"), "{view:?}: an unreadable cell says so");
    }
}

/// The cue sits inside its cell's placeholder, small and centered.
#[test]
fn the_loading_cue_is_a_small_mark_in_the_middle_of_the_cell() {
    let (mut h, _open) = app_with_held_thumbnails(ViewMode::SquareGrid);
    first_grid_frame(&mut h);
    let cell = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap();
    let id = ids(&h, "badge:loading:").remove(0);
    let photo = id.trim_start_matches("badge:loading:");
    let cue = cell(&id);
    let thumb = cell(&format!("thumb:{photo}"));
    assert!(thumb.contains_rect(cue), "{cue:?} in {thumb:?}");
    assert!(cue.width() < thumb.width() / 3.0 && cue.height() < thumb.height() / 3.0, "quiet: {cue:?} in {thumb:?}");
    assert!((cue.center() - thumb.center()).length() < 14.0, "centered: {cue:?} in {thumb:?}");
}

/// The filmstrip's tiles tell loading from unreadable the same way.
#[test]
fn the_filmstrip_tells_loading_from_unreadable_too() {
    let (mut h, open) = app_with_held_thumbnails(ViewMode::PhotoGrid);
    let r = h.request("ui.set", serde_json::json!({"view": "detail"}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!ids(&h, "film:badge:loading:").is_empty(), "the tiles are loading: {:?}", ids(&h, "film:"));
    open.set(true);
    assert!(h.settle(SETTLE));
    assert!(ids(&h, "film:badge:loading:").is_empty(), "gone once the failure is known");
}

/// A failure left by an earlier request is not this tile's verdict: the filmstrip asks for its
/// thumbnail before it reads the failure, so its first frame says loading, not "!".
#[test]
fn a_filmstrip_tile_asks_for_its_thumbnail_before_it_reads_an_old_failure() {
    let (mut h, _open) = app_with_held_thumbnails(ViewMode::Detail);
    for i in 1..=12u64 {
        h.app.renderer.record_failure_for_test(Slot::ThumbQuick(PhotoId(i)), PhotoId(i));
    }
    // the first frame that draws the strip: nothing was asked for yet
    let mut drawn = false;
    for _ in 0..20 {
        h.step();
        if !ids(&h, "film:").is_empty() {
            drawn = true;
            break;
        }
    }
    assert!(drawn, "the strip is drawn");
    assert!(!h.painted_text().iter().any(|t| t == "!"), "no tile says it can't be opened on an old failure");
}

/// The smallest square cells still hold the cue: its size is judged against the cell, and the cue
/// itself must fit the placeholder left inside it.
#[test]
fn the_smallest_square_cells_still_show_the_loading_cue() {
    let (mut h, _open) = app_with_held_thumbnails(ViewMode::SquareGrid);
    h.app.ui.thumb_size = crate::state::THUMB_SIZE_MIN;
    first_grid_frame(&mut h);
    let cues = ids(&h, "badge:loading:");
    assert!(!cues.is_empty(), "the cells on screen are loading and say so: {:?}", ids(&h, "thumb:"));
    for id in cues {
        let photo = id.trim_start_matches("badge:loading:");
        let cue = h.app.widgets.iter().find(|(w, _)| *w == id).map(|(_, r)| *r).unwrap();
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{photo}")).map(|(_, r)| *r).unwrap();
        assert!(cell.contains_rect(cue), "{cue:?} in {cell:?}");
    }
}
