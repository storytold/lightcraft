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

// Given thumbnails over their share of the budget, or more remembered failures than are kept, the
// loupe trims them while stepping through photos, as the grid does while scrolling.
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

/// A library whose files can't be read (a drive that sleeps): `n` photos, each failing to decode.
fn unreadable_library(n: u64) -> (crate::LightcraftApp, egui::Context) {
    let mut s = Session::new();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("simulated decode failure".into())));
    // (`with_fs` sets the file system's thumbnail loader, which wins over `file_loader`)
    s.media.reduced_loader = None;
    for id in 1..=n {
        let name = format!("IMG_{id:04}.jpg");
        let photo = Photo::new(PhotoId(id), Source::File { path: format!("/photos/offline/{name}") }, &name, "JPEG", 6000, 4000, "2026-01-01");
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
    assert!(last.contains(&format!("{held} more thumbnail failures")), "{last}");
    assert_eq!(app.renderer.thumb_warnings.held_back, 0);
}
