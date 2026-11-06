//! What the grid's thumbnails cost in memory, and what their failures cost the log, when a library
//! is far bigger than the screen: the textures follow the memory budget, the failures the grid
//! remembers stay bounded, and a folder whose files can't be read doesn't write a warning per cell.

use std::collections::HashSet;
use std::sync::Arc;

use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::Session;
use lightcraft_engine::memory;

use super::*;
use crate::test_budget::BudgetGuard;

const MB: usize = 1 << 20;

/// A texture the renderer counts as `size` (the picture itself is one pixel).
fn tex(ctx: &egui::Context, id: u64, size: [usize; 2]) -> Tex {
    Tex {
        look: None,
        key: id,
        photo: PhotoId(id),
        tex: ctx.load_texture(format!("thumb {id}"), egui::ColorImage::new([1, 1], vec![egui::Color32::BLACK]), Default::default()),
        native: None,
        size,
        histogram: None,
        ms: 0.0,
        quick: None,
        pixels: None,
    }
}

/// A renderer holding a grid thumbnail of `size` for each of `ids`.
fn thumbs(ctx: &egui::Context, ids: std::ops::RangeInclusive<u64>, size: [usize; 2]) -> Renderer {
    let mut r = Renderer::default();
    for id in ids {
        r.textures.insert(Slot::Thumb(PhotoId(id)), tex(ctx, id, size));
    }
    r
}

/// The photos the cells on screen show.
fn on_screen(ids: impl IntoIterator<Item = u64>) -> HashSet<PhotoId> {
    ids.into_iter().map(PhotoId).collect()
}

/// A failure as the renderer remembers it: `slot` failed to render `photo`.
fn fail(r: &mut Renderer, slot: Slot, photo: u64) {
    r.failed.insert(slot, (1, "simulated decode failure".into(), 0, PhotoId(photo)));
}

/// The thumbnail failures the renderer remembers, by slot.
fn remembered(r: &Renderer) -> HashSet<Slot> {
    r.failed.keys().copied().filter(|s| matches!(s, Slot::Thumb(_) | Slot::ThumbQuick(_))).collect()
}

// Given thumbnails no cell on screen shows, they stay while they fit a quarter of the memory budget
// and go once they don't: a smaller budget keeps fewer, a larger one more (the cap was 600
// pictures, whatever the budget).
#[test]
fn the_thumbnail_cap_follows_the_memory_budget() {
    let ctx = egui::Context::default();
    // 100 thumbnails of 256 x 256 px: 256 KiB each, 25 MiB together
    let left_after = |budget: usize| {
        let mut r = thumbs(&ctx, 1..=100, [256, 256]);
        let _turn = BudgetGuard::hold();
        memory::set_budget(budget);
        r.evict_thumbs(&on_screen([7]));
        r.thumb_textures()
    };
    assert_eq!(left_after(512 * MB), 100, "25 MiB fit the 128 MiB that a quarter of 512 MiB is");
    assert_eq!(left_after(64 * MB), 1, "25 MiB don't fit the 16 MiB that a quarter of 64 MiB is: the one on screen is left");
}

// Given thumbnails of different sizes, the cap counts the bytes they hold, not the pictures: ten
// 1024 px thumbnails are 40 MiB, far fewer than 600 pictures and still over a 32 MiB share, and the
// views' textures are no thumbnails.
#[test]
fn the_thumbnail_cap_counts_bytes_not_pictures() {
    let ctx = egui::Context::default();
    let _turn = BudgetGuard::hold();
    memory::set_budget(128 * MB);

    let mut big = thumbs(&ctx, 1..=10, [1024, 1024]);
    assert_eq!(big.thumb_budget(), 32 * MB, "a quarter of the budget");
    big.textures.insert(Slot::ThumbQuick(PhotoId(11)), tex(&ctx, 11, [1024, 1024]));
    big.textures.insert(Slot::Main, tex(&ctx, 99, [6000, 4000]));
    assert_eq!(big.thumb_bytes(), 11 * 4 * MB, "the thumbnails and their stand-ins, not the loupe's picture");
    big.evict_thumbs(&on_screen([]));
    assert_eq!(big.thumb_textures(), 0, "11 pictures, 44 MiB");
    assert!(big.textures.contains_key(&Slot::Main), "a view is not a thumbnail");

    let mut small = thumbs(&ctx, 1..=100, [128, 128]);
    small.evict_thumbs(&on_screen([]));
    assert_eq!(small.thumb_textures(), 100, "100 pictures, 6 MiB");
}

// Given the failures of every thumbnail a long scroll passed (each remembered so that the grid
// doesn't send an unreadable file back to the decoder every frame), the ones beyond the screen are
// dropped once they outnumber what is worth remembering: a pass over a library of files that
// can't be read doesn't keep a record, with its error text, for each of them.
#[test]
fn the_failures_remembered_for_the_grid_are_bounded() {
    let mut r = Renderer::default();
    for id in 1..=5 {
        fail(&mut r, Slot::Thumb(PhotoId(id)), id);
    }
    fail(&mut r, Slot::ThumbQuick(PhotoId(5)), 5);
    fail(&mut r, Slot::Main, 1);
    r.evict_thumbs(&on_screen([1]));
    assert_eq!(r.failed_thumbs(), 6, "a few are remembered: scrolling back doesn't ask the decoder again");

    for id in 6..=2 * MAX_FAILED_THUMBS as u64 {
        fail(&mut r, Slot::Thumb(PhotoId(id)), id);
    }
    r.evict_thumbs(&on_screen([1, 2]));
    assert_eq!(remembered(&r), HashSet::from([Slot::Thumb(PhotoId(1)), Slot::Thumb(PhotoId(2))]), "the cells on screen keep theirs");
    assert!(r.failed.contains_key(&Slot::Main), "a view's failure is not the grid's to drop");
}

// Given the failure of a stand-in (an unedited raw whose camera preview can't be read) forgotten
// with the others of a long scroll, the photo is asked for again when it comes back on screen: the
// note that the stand-in was tried went with its failure. Left behind, it kept the grid from asking,
// and the cell stayed empty, with no failure left to say why. A cell on screen keeps its failure
// and its note, and so does a newer try that the failure forgotten isn't about.
#[test]
fn a_stand_in_is_asked_for_again_once_its_failure_is_forgotten() {
    let (mut app, ctx) = unreadable_raws(3);
    let ask = |app: &mut crate::LightcraftApp, id: u64| crate::panels::grid::request_thumb(app, PhotoId(id), 384, 10);
    for id in [1, 2] {
        ask(&mut app, id);
    }
    assert_eq!(app.renderer.pool.run_inline(2), 2);
    app.renderer.poll(&ctx, &mut app.session);
    for id in [1, 2] {
        assert!(app.renderer.failed.contains_key(&Slot::ThumbQuick(PhotoId(id))), "the stand-in of photo {id} failed");
        ask(&mut app, id);
    }
    assert_eq!(app.renderer.pool.queued(), 0, "while the failures are remembered, the decoder isn't asked again");
    // (photo 3 was edited since its failure: its note is the newer try's)
    app.renderer.quick_tried.insert(Slot::ThumbQuick(PhotoId(3)), 99);
    fail(&mut app.renderer, Slot::ThumbQuick(PhotoId(3)), 3);

    // a long scroll away from photo 1: more failures than are kept, nothing asks for it, photo 2 is on screen
    for id in 4..=MAX_FAILED_THUMBS as u64 + 4 {
        fail(&mut app.renderer, Slot::Thumb(PhotoId(id)), id);
    }
    app.renderer.frame_drawn();
    app.renderer.frame_drawn();
    app.renderer.evict_thumbs(&on_screen([2]));
    assert!(!app.renderer.failed.contains_key(&Slot::ThumbQuick(PhotoId(1))), "forgotten with the others");
    assert!(app.renderer.failed.contains_key(&Slot::ThumbQuick(PhotoId(2))), "the cell on screen keeps its failure");
    ask(&mut app, 2);
    assert_eq!(app.renderer.pool.queued(), 0, "and the decoder isn't asked again for it");
    assert_eq!(app.renderer.quick_tried.get(&Slot::ThumbQuick(PhotoId(3))), Some(&99), "a newer try keeps its note");
    ask(&mut app, 1);
    assert_eq!(app.renderer.pool.queued(), 1, "back on screen, photo 1 is asked for again");
}

// Given a view that draws after the grid (the Print dialog's cells) and shows photos far from the
// grid's window, the grid's trim keeps what a view asked a thumbnail of in this frame or the last:
// a cell that finds no picture asks the decoder again, every frame. Two frames on, nobody asks,
// and they go like the rest.
#[test]
fn the_trim_keeps_the_thumbnails_another_view_asked_for_in_this_frame_or_the_last() {
    let ctx = egui::Context::default();
    let mut r = thumbs(&ctx, 1..=20, [128, 128]);
    let held = |r: &Renderer| (1..=20).filter(|&id| r.textures.contains_key(&Slot::Thumb(PhotoId(id)))).collect::<Vec<u64>>();

    r.thumb_asked(PhotoId(5));
    r.evict_thumbs_within(&on_screen([7]), 0);
    assert_eq!(held(&r), [5, 7], "asked in this frame, and the one on screen");
    r.frame_drawn();
    r.evict_thumbs_within(&on_screen([7]), 0);
    assert_eq!(held(&r), [5, 7], "asked in the frame before");
    r.frame_drawn();
    r.evict_thumbs_within(&on_screen([7]), 0);
    assert_eq!(held(&r), [7], "not asked for two frames");

    for id in 1..=2 * MAX_FAILED_THUMBS as u64 {
        fail(&mut r, Slot::Thumb(PhotoId(id)), id);
    }
    r.thumb_asked(PhotoId(3));
    r.evict_thumbs(&on_screen([1]));
    assert_eq!(remembered(&r), HashSet::from([Slot::Thumb(PhotoId(1)), Slot::Thumb(PhotoId(3))]), "a failure a view asked about stays");
}

// Given a view that asks for a thumbnail through the grid's request, the renderer notes the photo
// for the trim (the Print dialog, the filmstrip and the loupe's look-ahead all ask this way).
#[test]
fn a_thumbnail_request_notes_the_photo_for_the_trim() {
    let (mut app, _ctx) = unreadable_library(1);
    crate::panels::grid::request_thumb(&mut app, PhotoId(1), 384, 10);
    assert!(app.renderer.thumbs_asked.contains(&PhotoId(1)));
}

// Given a Print cell that stops asking once its picture has arrived, and the grid trimming behind
// the dialog every frame with its own window elsewhere, the cell keeps its picture: it notes the
// photo every frame it is drawn, or the picture would go two frames after the last ask, be asked
// for again, and flicker.
#[test]
fn a_print_cell_keeps_its_thumbnail_through_the_grids_trims() {
    let (mut app, ctx) = unreadable_library(3);
    app.renderer.textures.insert(Slot::Thumb(PhotoId(2)), tex(&ctx, 2, [256, 256]));
    app.renderer.thumb_asked(PhotoId(2)); // (the frame before, when the cell still asked for it)
    for frame in 0..6 {
        app.renderer.evict_thumbs_within(&on_screen([1]), 0); // (the grid draws first)
        let shown = crate::dialogs::print_view::cell_thumbnail(&mut app, PhotoId(2), 256);
        assert!(shown.is_some(), "frame {frame}: the cell shows its picture");
        assert_eq!(app.renderer.thumb_jobs_built, 0, "frame {frame}: and doesn't ask for it again");
        app.renderer.frame_drawn();
    }
}

// Given a Print cell whose thumbnail is still queued, and the grid trimming before it every frame
// with its own window elsewhere, the cell's job stays queued, the same one each frame: dropped by
// each trim, the cell would build and ask for a new job every frame (a new request each time).
// Two frames after the last ask it goes like the rest.
#[test]
fn the_trim_keeps_the_queued_thumbnail_of_a_photo_a_view_asked_for() {
    let (mut app, _ctx) = unreadable_library(3);
    let slot = Slot::Thumb(PhotoId(2));
    let mut placed = None;
    for frame in 0..4 {
        app.renderer.evict_thumbs_within(&on_screen([1]), 0); // (the grid draws first)
        assert!(crate::dialogs::print_view::cell_thumbnail(&mut app, PhotoId(2), 256).is_none(), "frame {frame}: no picture yet");
        assert!(app.renderer.is_queued(slot), "frame {frame}: its job is queued");
        let request = app.renderer.requests[&slot].id;
        assert_eq!(*placed.get_or_insert(request), request, "frame {frame}: the one asked for in the first frame, not a new one");
        assert_eq!(app.renderer.thumb_jobs_built, 1, "frame {frame}: and no job is built again");
        app.renderer.frame_drawn();
    }
    app.renderer.frame_drawn();
    app.renderer.evict_thumbs_within(&on_screen([1]), 0);
    assert!(!app.renderer.is_queued(slot), "not asked for two frames: its job goes");
}

// Given the browser build, where jobs wait in the renderer's own queue for a free worker, the grid's
// trim drops the queued thumbnail jobs of photos that scrolled away and keeps those a view asked
// for (as for the pool's jobs above): a Print cell whose job went would build and ask for a new one
// every frame.
#[test]
fn the_trim_keeps_the_offload_queues_thumbnail_jobs_of_photos_a_view_asked_for() {
    let (mut app, _ctx) = unreadable_library(3);
    app.renderer.set_offload(Box::new(super::thumbnail_tests::Blocked));
    for id in 1..=3 {
        crate::panels::grid::request_thumb(&mut app, PhotoId(id), 256, 10);
    }
    let queued = |app: &crate::LightcraftApp| {
        let mut ids: Vec<u64> = app.renderer.queue.iter().filter_map(|q| if let Slot::Thumb(id) = q.slot { Some(id.0) } else { None }).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(queued(&app), [1, 2, 3], "no worker is free: the three jobs wait");
    app.renderer.frame_drawn();
    app.renderer.frame_drawn(); // (nothing has asked for two frames)
    app.renderer.thumb_asked(PhotoId(2));
    app.renderer.evict_thumbs_within(&on_screen([1]), 0);
    assert_eq!(queued(&app), [1, 2], "the one on screen and the one a view asked for stay; the third scrolled away");
    assert!(app.renderer.is_pending(Slot::Thumb(PhotoId(2))), "and the one kept is still waited for");
    assert!(!app.renderer.is_pending(Slot::Thumb(PhotoId(3))), "the one dropped is not");
    app.renderer.frame_drawn();
    app.renderer.frame_drawn();
    app.renderer.evict_thumbs_within(&on_screen([1]), 0);
    assert_eq!(queued(&app), [1], "not asked for two frames: the job goes like the rest");
}

// Given photos a view asked a thumbnail of, the asks age with the frames the window draws: the
// logic ticks a hidden window gets leave them as they were (nothing drew, nothing could ask again),
// so the first frame drawn after it comes back still finds them. A drawn frame makes them the
// frame before's, and a second lets them go.
#[test]
fn the_asks_age_with_the_frames_drawn_not_with_the_ticks_of_a_hidden_window() {
    let (app, _ctx) = unreadable_library(3);
    let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    for _ in 0..3 {
        h.step(); // (the fonts are made, then frames are laid out)
    }
    h.app.renderer.thumb_asked(PhotoId(99)); // (a photo no cell shows)
    let ctx = h.view.ctx.clone();
    for _ in 0..4 {
        h.app.logic(&ctx);
    }
    assert!(h.app.renderer.thumbs_asked.contains(&PhotoId(99)), "ticks without a frame leave the asks as they were");
    h.step();
    assert!(h.app.renderer.thumbs_asked_last.contains(&PhotoId(99)), "a drawn frame makes them the frame before's");
    assert!(!h.app.renderer.thumbs_asked.contains(&PhotoId(99)));
    h.step();
    assert!(!h.app.renderer.thumbs_asked_last.contains(&PhotoId(99)), "and a second lets them go");
}

// Given the second window on the active photo, it notes the photo for the trim: until the render
// made for the window arrives, its picture is the photo's thumbnail, which the grid's trim takes
// when the photo is off screen and the thumbnails are over their share.
#[test]
fn the_second_window_notes_the_photo_whose_thumbnail_it_falls_back_on() {
    let (mut app, ctx) = unreadable_library(3);
    app.ui.second_window = true;
    app.session.execute("library.select", &serde_json::json!({"ids": [2]})).unwrap();
    assert_eq!(app.session.active(), Some(PhotoId(2)));
    let mut pass = ctx.run_ui(egui::RawInput::default(), |ui| crate::panels::second::show(&mut app, ui.ctx()));
    pass.textures_delta.clear();
    assert!(app.renderer.thumbs_asked.contains(&PhotoId(2)));
}

// Given thumbnails over their share of the budget, or more remembered failures than are kept, the
// loupe trims them, as the grid does while scrolling.
#[test]
fn thumbnails_are_over_their_limit_by_bytes_or_by_remembered_failures() {
    let ctx = egui::Context::default();
    let _turn = BudgetGuard::hold();
    memory::set_budget(64 * MB);
    let mut r = thumbs(&ctx, 1..=10, [512, 512]);
    assert!(!r.thumbs_over_limit(), "10 MiB fit the 16 MiB share");
    for id in 11..=30 {
        r.textures.insert(Slot::Thumb(PhotoId(id)), tex(&ctx, id, [512, 512]));
    }
    assert!(r.thumbs_over_limit(), "30 MiB don't");
    r.evict_thumbs(&on_screen([]));
    assert!(!r.thumbs_over_limit());
    for id in 1..=MAX_FAILED_THUMBS as u64 + 1 {
        fail(&mut r, Slot::Thumb(PhotoId(id)), id);
    }
    assert!(r.thumbs_over_limit(), "more remembered failures than are kept");
}

/// The loupe's look-ahead for the open photo `id`, asking for nothing ahead: only its trim runs.
fn rest_on(app: &mut crate::LightcraftApp, id: u64) {
    crate::panels::detail::look_ahead(app, PhotoId(id), 64, true, crate::panels::detail::Prepare::Nothing);
}

/// `n` photos in file-name order, each with a grid thumbnail of `size`.
fn library_with_thumbnails(n: u64, size: [usize; 2]) -> crate::LightcraftApp {
    let (mut app, ctx) = unreadable_library(n);
    app.session.sort.key = lightcraft_catalog::SortKey::FileName;
    app.session.sort.ascending = true;
    for id in 1..=n {
        app.renderer.textures.insert(Slot::Thumb(PhotoId(id)), tex(&ctx, id, size));
    }
    app
}

// Given a loupe resting on one photo, a smaller memory budget (Settings ▸ Performance) trims the
// thumbnails at once: the trim doesn't wait for a step to another photo.
#[test]
fn the_loupe_trims_the_thumbnails_when_the_budget_shrinks_while_it_rests() {
    let mut app = library_with_thumbnails(300, [256, 256]);
    let _turn = BudgetGuard::hold();
    memory::set_budget(1024 * MB);
    rest_on(&mut app, 150);
    assert_eq!(app.renderer.thumb_textures(), 300, "75 MiB fit the 256 MiB that a quarter of 1 GiB is");
    memory::set_budget(256 * MB);
    rest_on(&mut app, 150);
    assert!(!app.renderer.thumbs_over_limit(), "75 MiB don't fit the 64 MiB that a quarter of 256 MiB is");
    assert_eq!(app.renderer.thumb_textures(), 129, "the window of the photos around it, 64 either side, fits and stays whole");
    assert!(app.renderer.thumb(PhotoId(150)).is_some(), "the open photo's own stays");
}

// Given thumbnails around the open photo that don't all fit their share of the memory budget, the
// loupe keeps the nearest ones that do, not its whole window of 129: a small budget is held to even
// when the grid left large thumbnails behind. The open photo's own stays.
#[test]
fn the_loupes_window_of_thumbnails_holds_to_their_share_of_the_budget() {
    let mut app = library_with_thumbnails(300, [512, 512]);
    let _turn = BudgetGuard::hold();
    memory::set_budget(64 * MB);
    rest_on(&mut app, 150);
    let kept: Vec<u64> = (1..=300).filter(|&id| app.renderer.thumb(PhotoId(id)).is_some()).collect();
    assert!(!app.renderer.thumbs_over_limit(), "{} MiB held against the 16 MiB share: {kept:?}", kept.len());
    assert!(kept.contains(&150) && kept.contains(&149) && kept.contains(&151), "the open photo and its neighbors stay: {kept:?}");
    assert!(kept.iter().all(|id| id.abs_diff(150) <= 8), "the nearest ones are the ones kept: {kept:?}");
}

/// The photos whose grid thumbnail the renderer holds, in order.
fn held_thumbnails(app: &crate::LightcraftApp, n: u64) -> Vec<u64> {
    (1..=n).filter(|&id| app.renderer.thumb(PhotoId(id)).is_some()).collect()
}

// Given an open photo whose own thumbnail is over the whole share (a 3000 px one against 16 MiB),
// the loupe still keeps it, and only it: the window around it is the nearest that fit, and none do.
#[test]
fn the_loupe_keeps_the_open_photos_thumbnail_though_it_alone_is_over_the_share() {
    let mut app = library_with_thumbnails(300, [128, 128]);
    let ctx = egui::Context::default();
    let _turn = BudgetGuard::hold();
    memory::set_budget(64 * MB);
    app.renderer.textures.insert(Slot::Thumb(PhotoId(150)), tex(&ctx, 150, [3000, 3000]));
    rest_on(&mut app, 150);
    assert_eq!(held_thumbnails(&app, 300), [150]);
}

// Given the open photo at either end of the library, the loupe's window is the 64 photos that
// exist on the other side, not nothing and not a shorter one: nothing is kept past the ends.
#[test]
fn the_loupes_window_of_thumbnails_reaches_from_either_end_of_the_library() {
    let _turn = BudgetGuard::hold();
    memory::set_budget(64 * MB); // (300 thumbnails of 64 KiB are 18.75 MiB: over the 16 MiB share)
    let mut first = library_with_thumbnails(300, [128, 128]);
    rest_on(&mut first, 1);
    assert_eq!(held_thumbnails(&first, 300), (1..=65).collect::<Vec<u64>>(), "the first photo and the 64 after it");
    let mut last = library_with_thumbnails(300, [128, 128]);
    rest_on(&mut last, 300);
    assert_eq!(held_thumbnails(&last, 300), (236..=300).collect::<Vec<u64>>(), "the last photo and the 64 before it");
}

// `ui.inspect` reports the thumbnails beside the stage caches: how many, how much, the limit they
// are held to, and the failures remembered.
#[test]
fn the_memory_report_gives_the_thumbnails_and_their_limit() {
    let ctx = egui::Context::default();
    let mut app = crate::LightcraftApp::new(Session::new(), crate::Services::default());
    app.renderer.textures.insert(Slot::Thumb(PhotoId(1)), tex(&ctx, 1, [256, 256]));
    app.renderer.textures.insert(Slot::ThumbQuick(PhotoId(2)), tex(&ctx, 2, [128, 128]));
    app.renderer.textures.insert(Slot::Main, tex(&ctx, 3, [1000, 1000]));
    fail(&mut app.renderer, Slot::Thumb(PhotoId(4)), 4);
    let _turn = BudgetGuard::hold();
    let m = crate::control::memory(&app);
    let t = &m["thumbnails"];
    assert_eq!(t["count"], 2, "{m}");
    assert_eq!(t["bytes"], (256 * 256 + 128 * 128) * 4, "{m}");
    assert_eq!(t["budgetBytes"], memory::budget() / THUMB_BUDGET_SHARE, "{m}");
    assert_eq!(t["failed"], 1, "{m}");
    assert!(m["stageCaches"].is_object() && m["textures"]["count"] == 3, "beside the blocks that were there: {m}");
}

// The warning limit lets a burst through, then one each interval, and counts what it held back;
// a long quiet earns another burst, no more.
#[test]
fn the_warning_limit_lets_a_burst_through_and_then_one_each_interval() {
    let burst = FAILURE_WARN_BURST as usize;
    let interval = FAILURE_WARN_INTERVAL_MS;
    let mut w = WarnLimit::default();
    for i in 0..burst {
        assert_eq!(w.admit(0.0), Admit::Warn { held_back: 0 }, "failure {i} is in the burst");
    }
    for _ in 0..3 {
        assert_eq!(w.admit(0.0), Admit::Held);
    }
    assert_eq!(w.admit(interval / 2.0), Admit::Held, "half an interval is not enough");
    assert_eq!(w.admit(interval), Admit::Warn { held_back: 4 }, "an interval after the burst one more goes out, saying what it follows");
    assert_eq!(w.admit(interval * 1.5), Admit::Held);
    assert_eq!(w.admit(interval * 2.0), Admit::Warn { held_back: 1 });

    let later = interval * 10_000.0;
    let admitted = (0..2 * burst).filter(|_| matches!(w.admit(later), Admit::Warn { .. })).count();
    assert_eq!(admitted, burst, "a long quiet earns a burst again, not more");
}

// A time that doesn't go forward doesn't earn warnings or break the count.
#[test]
fn the_warning_limit_tolerates_a_clock_that_stands_still_or_goes_back() {
    let mut w = WarnLimit::default();
    for _ in 0..FAILURE_WARN_BURST as usize {
        assert!(matches!(w.admit(5_000.0), Admit::Warn { .. }));
    }
    assert_eq!(w.admit(1_000.0), Admit::Held, "earlier than the last failure");
    assert_eq!(w.admit(f64::NAN), Admit::Held, "not a time");
    assert_eq!(w.admit(5_000.0), Admit::Held);
}

// Given failures the log held back, and then none for a whole interval, the count is due once: the
// next warning would have said it, and when the failures have stopped none comes. Failures that go
// on, a time that isn't one, and nothing held back have nothing due.
#[test]
fn the_failures_held_back_are_counted_once_they_stop() {
    let interval = FAILURE_WARN_INTERVAL_MS;
    let mut w = WarnLimit::default();
    assert_eq!(w.settle(10.0 * interval), None, "nothing failed");
    for _ in 0..FAILURE_WARN_BURST as usize {
        assert!(matches!(w.admit(100.0), Admit::Warn { held_back: 0 }));
    }
    assert_eq!(w.settle(100.0 + 2.0 * interval), None, "the burst was all logged: nothing held back");
    for _ in 0..3 {
        assert_eq!(w.admit(100.0), Admit::Held);
    }
    assert_eq!(w.settle(100.0 + interval / 2.0), None, "the failures may go on");
    assert_eq!(w.admit(100.0 + 0.9 * interval), Admit::Held, "and one more comes");
    assert_eq!(w.settle(100.0 + 1.5 * interval), None, "an interval has not passed since the last one");
    assert_eq!(w.settle(f64::NAN), None, "not a time");
    assert_eq!(w.settle(100.0 + 2.0 * interval), Some(4), "an interval with none: the four held back");
    assert_eq!(w.settle(100.0 + 3.0 * interval), None, "said once");
    assert_eq!(w.admit(100.0 + 3.0 * interval), Admit::Warn { held_back: 0 }, "and not again with the next warning");
}

/// A library whose files can't be read (a drive that sleeps): `n` photos, each failing to decode.
fn unreadable_library(n: u64) -> (crate::LightcraftApp, egui::Context) {
    unreadable_files(n, false)
}

/// [`unreadable_library`] of unedited raws whose camera previews can't be read either: the grid asks
/// for each one's stand-in (`Slot::ThumbQuick`), and the render behind it fails too.
fn unreadable_raws(n: u64) -> (crate::LightcraftApp, egui::Context) {
    unreadable_files(n, true)
}

fn unreadable_files(n: u64, raw: bool) -> (crate::LightcraftApp, egui::Context) {
    let mut s = Session::new();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
    if raw {
        s.media.preview_loader = Some(Arc::new(|_: &str, _| None));
    }
    // (`with_fs` sets the file system's thumbnail loader, which wins over `file_loader`)
    s.media.reduced_loader = None;
    for id in 1..=n {
        let (extension, format) = if raw { ("cr3", "CR3") } else { ("jpg", "JPEG") };
        let name = format!("IMG_{id:04}.{extension}");
        let mut photo = Photo::new(PhotoId(id), Source::File { path: format!("/photos/offline/{name}") }, &name, format, 6000, 4000, "2026-01-01");
        if raw {
            photo.kind = lightcraft_catalog::MediaKind::Raw;
            photo.develop = Arc::new(photo.camera_defaults());
        }
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut app = crate::LightcraftApp::new(s, crate::Services::default());
    app.renderer.pool = JobPool::new(0); // nothing starts until run_inline
    (app, egui::Context::default())
}

// Given a folder whose files can't be read, a thumbnail fails for every cell the grid asks for:
// the log gets a burst of warnings and a trickle after it, the rest go to the debug trace, every
// failure is still remembered, and a view the person is looking at still warns.
#[test]
fn a_flood_of_thumbnail_failures_does_not_flood_the_log() {
    const FLOOD: usize = 100;
    let (mut app, ctx) = unreadable_library(FLOOD as u64);
    for id in 1..=FLOOD as u64 {
        crate::panels::grid::request_thumb(&mut app, PhotoId(id), 384, 10);
    }
    assert_eq!(app.renderer.pool.run_inline(FLOOD), FLOOD);
    app.renderer.poll(&ctx, &mut app.session);

    let warned = app.renderer.failure_messages.len();
    assert!(warned >= FAILURE_WARN_BURST as usize, "the first failures are warnings: {warned}");
    assert!(warned < FLOOD / 2, "not a warning per cell: {warned} of {FLOOD}");
    let traced = app.renderer.decisions.iter().filter(|l| l.contains("not a warning")).count();
    assert_eq!(warned + traced, FLOOD, "each failure is a warning or a debug line");
    assert_eq!(app.renderer.failed_thumbs(), FLOOD, "every failure is remembered: the grid doesn't ask again");

    let job = app.session.render_job(PhotoId(1), 200, 200, false, true).unwrap();
    app.renderer.request(Slot::Main, job, 100);
    assert_eq!(app.renderer.pool.run_inline(1), 1);
    app.renderer.poll(&ctx, &mut app.session);
    let last = app.renderer.failure_messages.last().unwrap();
    assert!(last.starts_with("render Main"), "the loupe's failure warns however many thumbnails failed before it: {last}");

    // the first thumbnail warning after the quiet says how many it follows
    let held = app.renderer.thumb_warnings.held_back;
    assert_eq!(held as usize, traced);
    app.renderer.thumb_warnings.tokens = 1.0; // (an interval has passed)
    app.renderer.warn_failure(Slot::Thumb(PhotoId(1)), "render Thumb(PhotoId(1)): again".into());
    let last = app.renderer.failure_messages.last().unwrap();
    assert!(last.starts_with("render Thumb(PhotoId(1)): again"), "{last}");
    assert!(last.contains(&format!("not logged since the last warning: {held};")), "{last}");
    assert_eq!(app.renderer.thumb_warnings.held_back, 0);
}

// Given a flood of thumbnail failures the log held back some of, and then no more, the renderer says
// how many in a warning of its own once an interval has passed: before, the count waited for a
// failure that never came. Said once.
#[test]
fn a_flood_that_stops_is_counted_in_the_log() {
    const FLOOD: usize = 100;
    let (mut app, ctx) = unreadable_library(FLOOD as u64);
    for id in 1..=FLOOD as u64 {
        crate::panels::grid::request_thumb(&mut app, PhotoId(id), 384, 10);
    }
    assert_eq!(app.renderer.pool.run_inline(FLOOD), FLOOD);
    app.renderer.poll(&ctx, &mut app.session);
    let held = app.renderer.thumb_warnings.held_back;
    let warned = app.renderer.failure_messages.len();
    assert!(held > 0, "some were held back");
    app.renderer.poll(&ctx, &mut app.session);
    assert_eq!(app.renderer.failure_messages.len(), warned, "the failures may be going on: nothing is said yet");

    app.renderer.thumb_warnings.at_ms = Some(crate::monotonic_ms() - 2.0 * FAILURE_WARN_INTERVAL_MS); // (an interval with no failure)
    app.renderer.poll(&ctx, &mut app.session);
    assert_eq!(app.renderer.failure_messages.len(), warned + 1, "one warning for the count");
    let last = app.renderer.failure_messages.last().unwrap();
    assert!(last.contains(&format!("not logged since the last warning: {held};")), "{last}");
    assert_eq!(app.renderer.thumb_warnings.held_back, 0);
    app.renderer.poll(&ctx, &mut app.session);
    assert_eq!(app.renderer.failure_messages.len(), warned + 1, "said once");
}
