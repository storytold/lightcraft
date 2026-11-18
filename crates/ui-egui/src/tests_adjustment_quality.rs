//! Adjustment quality is independent of interaction scheduling and the preview size limit.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use lcc_catalog::{Op, Photo, PhotoId, Source};
use lcc_engine::media::{RenderJob, RenderResult};
use lcc_pipeline::Quality;
use serde_json::json;

use crate::headless::Headless;
use crate::render::{PANE_AFTER, PANE_BEFORE, RenderOffload, Slot};
use crate::state::{BeforeAfter, ViewMode, Zoom};
use crate::{LccApp, Services};

const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Work {
    asked: Vec<(Slot, RenderJob)>,
    running: Vec<(Slot, RenderJob)>,
    /// The run time (ms) every render reports: a slow machine without waiting for one.
    ms: f64,
    /// …and the HDR view renders, when set.
    hdr_ms: Option<f64>,
}

struct Record(Rc<RefCell<Work>>);

impl RenderOffload for Record {
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
        let mut work = self.0.borrow_mut();
        work.asked.push((slot, job.clone()));
        work.running.push((slot, job));
        None
    }

    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        let mut work = self.0.borrow_mut();
        let (ms, hdr_ms) = (work.ms, work.hdr_ms);
        let took = move |slot: Slot| if matches!(slot, Slot::HdrView(_)) { hdr_ms.unwrap_or(ms) } else { ms };
        std::mem::take(&mut work.running).into_iter().map(|(slot, job)| (slot, job.run(), took(slot))).collect()
    }
}

fn detail(native: [u32; 2], ppp: f32) -> (Headless, Rc<RefCell<Work>>) {
    let mut session = lcc_engine::Session::new();
    session.media.file_loader = Some(Arc::new(move |_, _, _| {
        let (w, h) = (native[0] as usize, native[1] as usize);
        let mut image = lcc_raster::Rgb32f::new(w, h);
        // Fine stripes, edges, colour and noise expose resampling and draft NR shortcuts.
        for (i, pixel) in image.data.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let stripe = if (x / 2 + y / 3) % 2 == 0 { 0.12 } else { 0.72 };
            *pixel = [stripe, stripe * 0.7 + (x % 7) as f32 * 0.01, stripe * 0.5 + (y % 11) as f32 * 0.01];
        }
        Ok((image, Default::default()))
    }));
    let photo = Photo::new(
        PhotoId(1),
        Source::File { path: "adjustment-quality.jpg".into() },
        "adjustment-quality.jpg",
        "JPEG",
        native[0],
        native[1],
        "2026-10-22",
    );
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    session.selection = lcc_engine::Selection::single(PhotoId(1));
    session.execute("develop.set", &json!({"values": {"light.highlights": -35.0, "detail.nrColor": 35.0}})).unwrap();
    let mut app = LccApp::new(session, Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::Detail;
    app.ui.right = crate::state::RightPanel::None;
    app.ui.filmstrip = false;
    app.ui.settings.high_quality_adjustment_previews = true;
    let work = Rc::new(RefCell::new(Work::default()));
    app.renderer.set_offload(Box::new(Record(work.clone())));
    let mut h = Headless::new(app, [740.3, 570.5], ppp);
    assert!(h.settle(TIMEOUT));
    (h, work)
}

fn latest(work: &Rc<RefCell<Work>>, slot: Slot) -> RenderJob {
    work.borrow().asked.iter().rev().find(|(s, _)| *s == slot).expect("requested view").1.clone()
}

fn pixels(h: &Headless, slot: Slot) -> Arc<egui::ColorImage> {
    h.app.renderer.textures[&slot].pixels.clone().expect("headless texture pixels")
}

#[test]
fn high_quality_adjustments_match_settled_pixels_at_every_display_scale() {
    let _ = lcc_engine::gpu::available();
    for native in [[1024, 768], [1200, 800]] {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let (mut h, work) = detail(native, ppp);
            // Also exercise an explicitly capped preview and a CPU-only proofed view.
            h.app.ui.settings.preview_limit = 1600;
            h.app.ui.soft_proof = ppp == 1.25;
            assert!(h.settle(TIMEOUT));
            let still = latest(&work, Slot::Main);
            let still_pixels = pixels(&h, Slot::Main);
            h.app.session.begin_interaction("Exposure").unwrap();
            assert!(h.settle(TIMEOUT));
            assert_eq!(pixels(&h, Slot::Main).pixels, still_pixels.pixels, "unchanged drag: {native:?}, {ppp}");
            for value in [0.25, -0.25] {
                h.app.session.execute("develop.set", &json!({"values": {"light.exposure": value}})).unwrap();
                assert!(h.settle(TIMEOUT));
                let live = latest(&work, Slot::Main);
                assert_eq!(live.request.quality, Quality::Full);
                assert_eq!((live.request.max_w, live.request.max_h), (still.request.max_w, still.request.max_h));
                assert_eq!(live.display_fit, still.display_fit);
                let live_pixels = pixels(&h, Slot::Main);
                h.app.session.end_interaction().unwrap();
                assert!(h.settle(TIMEOUT));
                assert_eq!(pixels(&h, Slot::Main).size, live_pixels.size);
                assert_eq!(pixels(&h, Slot::Main).pixels, live_pixels.pixels, "release: {native:?}, {ppp}");
                h.app.session.begin_interaction("Exposure").unwrap();
            }
            h.app.session.end_interaction().unwrap();
        }
    }
}

#[test]
fn high_quality_adjustments_respect_preview_and_texture_limits() {
    let (mut h, work) = detail([2000, 1500], 1.0);
    h.app.ui.settings.preview_limit = 1600;
    assert!(h.settle(TIMEOUT));
    let still = latest(&work, Slot::Main);
    assert_eq!(still.request.max_w, 1600);
    h.app.session.begin_interaction("Exposure").unwrap();
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 0.5}})).unwrap();
    assert!(h.settle(TIMEOUT));
    assert_eq!(latest(&work, Slot::Main).request.max_w, 1600);
    h.max_texture_side = 1024;
    assert!(h.settle(TIMEOUT));
    let limited = latest(&work, Slot::Main);
    assert_eq!(limited.request.max_w, 1024);
    assert_eq!(limited.request.quality, Quality::Full);
}

#[test]
fn high_quality_adjustment_hdr_jobs_keep_full_quality_and_extended_values() {
    struct Hdr;
    impl crate::HdrOutput for Hdr {
        fn headroom(&mut self) -> Option<f32> {
            Some(4.0)
        }
        fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
            Ok(egui::TextureId::User(700 + u64::from(view)))
        }
    }
    let (mut h, work) = detail([640, 480], 1.0);
    h.app.services.hdr_output = Some(Box::new(Hdr));
    h.app.hdr_headroom = Some(4.0);
    h.app.session.execute("develop.hdr", &json!({"enabled": true})).unwrap();
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 3.0}})).unwrap();
    h.app.session.begin_interaction("Exposure").unwrap();
    let job = h.app.session.render_job(PhotoId(1), 320, 240, false, true).unwrap();
    assert!(crate::panels::detail::request_hdr(&mut h.app, crate::render::HDR_LOUPE, job, true, 98).is_some());
    let live = latest(&work, Slot::HdrView(crate::render::HDR_LOUPE));
    assert_eq!(live.request.quality, Quality::Full);
    assert_eq!(live.request.depth, lcc_pipeline::OutputDepth::F32Hdr);
    let deep = live.run().rendered.unwrap().deep.expect("HDR float pixels");
    let lcc_pipeline::DeepSamples::F32(rgb) = deep.samples else { panic!("linear HDR float output") };
    assert!(rgb.iter().copied().any(|v| v > 1.0));
}

#[test]
fn high_quality_adjustments_can_switch_during_a_drag_and_ignore_slow_verdicts() {
    let (mut h, work) = detail([1024, 768], 1.0);
    h.app.drag_scale.remember_slow(PhotoId(1), "Exposure");
    // (the drag's drafts, not the refine of a still photo, are what this test looks at)
    h.app.still_refine.never = true;
    h.app.session.begin_interaction("Exposure").unwrap();
    for high in [false, true, false, true] {
        let reply = h.request("ui.set", json!({"settings": {"highQualityAdjustmentPreviews": high}}), TIMEOUT);
        assert_eq!(reply["ok"], true, "{reply}");
        assert!(h.settle(TIMEOUT));
        // the job behind the picture shown (switching back to full quality may show the full
        // render kept from before, with the same edit, instead of asking for it again)
        let shown = h.app.renderer.textures.get(&Slot::Main).map(|t| t.key).expect("a loupe picture");
        let job = work
            .borrow()
            .asked
            .iter()
            .rev()
            .find(|(s, j)| *s == Slot::Main && j.key == shown)
            .map(|(_, j)| j.clone())
            .expect("the shown render's job");
        assert_eq!(job.request.quality, if high { Quality::Full } else { Quality::Draft });
        assert_eq!(job.display_fit.is_some(), high);
        assert!(h.app.session.interaction.is_some(), "changing quality must not end the drag");
    }
}

#[test]
fn high_quality_adjustments_keep_both_before_after_views_at_settled_quality() {
    let (mut h, work) = detail([1024, 768], 1.25);
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom, BeforeAfter::Split, BeforeAfter::SplitTopBottom] {
        h.app.ui.before_after = mode;
        assert!(h.settle(TIMEOUT));
        let before = pixels(&h, Slot::Before);
        let after = pixels(&h, Slot::Main);
        h.app.session.begin_interaction("Exposure").unwrap();
        assert!(h.settle(TIMEOUT));
        for slot in [Slot::Main, Slot::Before] {
            let job = latest(&work, slot);
            assert_eq!(job.request.quality, Quality::Full);
            assert!(job.display_fit.is_some());
        }
        assert_eq!(pixels(&h, Slot::Before).pixels, before.pixels);
        assert_eq!(pixels(&h, Slot::Main).pixels, after.pixels);
        h.app.session.end_interaction().unwrap();
    }
}

#[test]
fn high_quality_adjustments_use_one_native_window_per_pane_instead_of_tiles() {
    let (mut h, work) = detail([1600, 1200], 1.0);
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.app.ui.before_after = BeforeAfter::SideBySide;
    assert!(h.settle(TIMEOUT));
    work.borrow_mut().asked.clear();
    h.app.session.begin_interaction("Exposure").unwrap();
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 0.5}})).unwrap();
    assert!(h.settle(TIMEOUT));
    assert_eq!(h.app.renderer.tiles_pending(), 0);
    assert!(work.borrow().asked.iter().all(|(s, _)| !matches!(s, Slot::Tile { .. })));
    for pane in [PANE_AFTER, PANE_BEFORE] {
        let job = latest(&work, Slot::Window(pane));
        assert_eq!(job.request.quality, Quality::Full);
        let window = job.request.window.expect("visible window with filter context");
        assert_eq!(job.request.max_w.max(job.request.max_h), 1600);
        assert!(window.w * window.h < 1600 * 1200, "not the entire original");
        assert!(job.stages.is_some(), "interactive window reuses its stage cache");
    }
}

/// The Main jobs asked for since `from` (an index into `asked`).
fn main_jobs_since(work: &Rc<RefCell<Work>>, from: usize) -> Vec<RenderJob> {
    work.borrow().asked.iter().skip(from).filter(|(s, _)| *s == Slot::Main).map(|(_, j)| j.clone()).collect()
}

fn adjust_quality(h: &mut Headless) -> String {
    let reply = h.request("ui.inspect", json!({}), TIMEOUT);
    reply["result"]["perf"]["adjustQuality"].as_str().unwrap_or_default().to_string()
}

// Given high quality adjustment previews on a machine too slow for them (every update takes
// 400 ms), a drag falls back: drafts while the slider moves, the settled render as soon as it
// holds still with the mouse still down, and the next drag of that slider starts drafting
#[test]
fn slow_high_quality_adjustments_fall_back_to_drafts_while_moving() {
    let (mut h, work) = detail([1024, 768], 1.0);
    let still = latest(&work, Slot::Main);
    work.borrow_mut().ms = crate::region::HQ_BUDGET_MS * 4.0;
    h.app.session.begin_interaction("Exposure").unwrap();
    let from = work.borrow().asked.len();
    for i in 1..=6 {
        h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.1}})).unwrap();
        h.step();
        h.step();
    }
    let moving = main_jobs_since(&work, from);
    assert_eq!(moving.first().map(|j| j.request.quality), Some(Quality::Full), "a drag starts at full quality");
    let last = moving.last().expect("the drag rendered");
    assert_eq!(last.request.quality, Quality::Draft, "too slow: the moving slider drafts");
    assert!(last.display_fit.is_none());
    assert!(h.app.session.interaction.is_some());
    // the slider holds still: the settled render, while the drag is still running
    let from = work.borrow().asked.len();
    let t0 = h.time();
    assert!(h.step_until(TIMEOUT, |_| main_jobs_since(&work, from).iter().any(|j| j.request.quality == Quality::Full)));
    assert!(h.time() - t0 >= crate::region::HQ_SETTLE_S - 0.02, "it waited for the slider to hold still");
    let settled = latest(&work, Slot::Main);
    assert_eq!(settled.request.quality, Quality::Full);
    assert_eq!((settled.request.max_w, settled.request.max_h), (still.request.max_w, still.request.max_h));
    assert_eq!(settled.display_fit, still.display_fit);
    assert!(h.app.session.interaction.is_some(), "settling does not end the drag");
    assert_eq!(adjust_quality(&mut h), "settled");
    // moving again drafts again
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 1.0}})).unwrap();
    h.step();
    assert_eq!(latest(&work, Slot::Main).request.quality, Quality::Draft);
    assert_eq!(adjust_quality(&mut h), "draft");
    h.app.session.end_interaction().unwrap();
    assert!(h.settle(TIMEOUT));
    assert_eq!(latest(&work, Slot::Main).request.quality, Quality::Full);
    assert_eq!(adjust_quality(&mut h), "idle");
    // the next drag of the same slider starts in the fallback: no frozen updates to find out again
    h.app.session.begin_interaction("Exposure").unwrap();
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 1.2}})).unwrap();
    h.step();
    assert_eq!(latest(&work, Slot::Main).request.quality, Quality::Draft);
    h.app.session.end_interaction().unwrap();
}

// Given high quality adjustment previews that keep up, a drag never falls back
#[test]
fn fast_high_quality_adjustments_never_fall_back() {
    let (mut h, work) = detail([1024, 768], 1.0);
    work.borrow_mut().ms = crate::region::HQ_BUDGET_MS * 0.5;
    h.app.session.begin_interaction("Exposure").unwrap();
    let from = work.borrow().asked.len();
    for i in 1..=8 {
        h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.1}})).unwrap();
        h.step();
        h.step();
    }
    assert!(main_jobs_since(&work, from).iter().all(|j| j.request.quality == Quality::Full && j.display_fit.is_some()));
    assert_eq!(adjust_quality(&mut h), "full");
    h.app.session.end_interaction().unwrap();
}

// Given a zoomed view (100 %) too slow at full quality, the zoomed window falls back like the
// whole frame: it is what shows the drag, so it is what is timed, drafted and settled
#[test]
fn slow_high_quality_zoom_windows_fall_back_and_settle() {
    let (mut h, work) = detail([1600, 1200], 1.0);
    h.app.ui.zoom = Zoom::Percent(100.0);
    assert!(h.settle(TIMEOUT));
    work.borrow_mut().ms = crate::region::HQ_BUDGET_MS * 4.0;
    h.app.session.begin_interaction("Exposure").unwrap();
    for i in 1..=6 {
        h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.1}})).unwrap();
        h.step();
        h.step();
    }
    assert_eq!(latest(&work, Slot::Window(PANE_AFTER)).request.quality, Quality::Draft);
    assert!(h.step_until(TIMEOUT, |_| latest(&work, Slot::Window(PANE_AFTER)).request.quality == Quality::Full));
    assert!(h.app.session.interaction.is_some());
    h.app.session.end_interaction().unwrap();
    assert!(h.settle(TIMEOUT));
}

// Given an HDR edit on an HDR display, what shows the drag is the float HDR render: a drag whose
// 8-bit renders keep up but whose HDR renders don't still falls back, HDR drafts included
#[test]
fn slow_hdr_renders_make_a_high_quality_drag_fall_back() {
    struct Hdr;
    impl crate::HdrOutput for Hdr {
        fn headroom(&mut self) -> Option<f32> {
            Some(4.0)
        }
        fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
            Ok(egui::TextureId::User(700 + u64::from(view)))
        }
    }
    let (mut h, work) = detail([1024, 768], 1.0);
    h.app.services.hdr_output = Some(Box::new(Hdr));
    h.app.hdr_headroom = Some(4.0);
    h.app.session.execute("develop.hdr", &json!({"enabled": true})).unwrap();
    assert!(h.settle(TIMEOUT));
    assert!(h.app.renderer.hdr_view.contains_key(&crate::render::HDR_LOUPE), "the HDR view shows");
    {
        let mut w = work.borrow_mut();
        w.ms = crate::region::HQ_BUDGET_MS * 0.2;
        w.hdr_ms = Some(crate::region::HQ_BUDGET_MS * 4.0);
    }
    let from = work.borrow().asked.len();
    h.app.session.begin_interaction("Exposure").unwrap();
    for i in 1..=6 {
        h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.1}})).unwrap();
        h.step();
        h.step();
    }
    // (the HDR texture is what shows the drag: no full-quality 8-bit render is asked under it)
    assert!(main_jobs_since(&work, from).iter().all(|j| j.request.quality == Quality::Draft));
    assert_eq!(latest(&work, Slot::HdrView(crate::render::HDR_LOUPE)).request.quality, Quality::Draft);
    h.app.session.end_interaction().unwrap();
    assert!(h.settle(TIMEOUT));
    assert_eq!(latest(&work, Slot::HdrView(crate::render::HDR_LOUPE)).request.quality, Quality::Full);
}
/// The Histogram panel's luminance shape (a share of the pixels per bin; empty when it shows none).
fn shape(h: &Option<lcc_raster::Histogram>) -> Vec<f64> {
    h.as_ref().map(|h| h.luma.iter().map(|v| f64::from(*v) / f64::from(h.total.max(1))).collect()).unwrap_or_default()
}

fn same_shape(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

/// What the Histogram panel shows over an exposure drag and its release: before the drag, at its
/// last update, every frame after the release, and once everything has settled.
struct HistogramTrace {
    before: Vec<f64>,
    last: Vec<f64>,
    released: Vec<Vec<f64>>,
    settled: Vec<f64>,
}

fn trace_histogram_over_a_drag(h: &mut Headless) -> HistogramTrace {
    let ctx = h.view.ctx.clone();
    let shown = |h: &mut Headless| shape(&crate::panels::edit::shown_histogram(&mut h.app, &ctx, PhotoId(1), lcc_raster::HistogramMode::RgbOverlap));
    // (the panel draws every frame: each step asks it for what it shows)
    let frames = |h: &mut Headless, n: usize| {
        let mut seen = Vec::new();
        for _ in 0..n {
            seen.push(shown(h));
            h.step();
        }
        seen
    };
    frames(h, 2);
    assert!(h.settle(TIMEOUT));
    let before = shown(h);
    h.app.session.begin_interaction("Exposure").unwrap();
    for i in 1..=4 {
        h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.3}})).unwrap();
        frames(h, 2);
    }
    frames(h, 30);
    let last = shown(h);
    h.app.session.end_interaction().unwrap();
    let released = frames(h, 30);
    for _ in 0..5 {
        shown(h);
        assert!(h.settle(TIMEOUT));
    }
    HistogramTrace { before, last, released, settled: shown(h) }
}

struct FakeHdrDisplay;

impl crate::HdrOutput for FakeHdrDisplay {
    fn headroom(&mut self) -> Option<f32> {
        Some(4.0)
    }
    fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
        Ok(egui::TextureId::User(700 + u64::from(view)))
    }
}

// Given high quality adjustment previews, an SDR edit or an HDR edit shown in SDR: the histogram a
// drag ends on is the one the released view keeps (each update is the render at rest, HDR bins
// included), so letting go of the slider does not change it
#[test]
fn high_quality_drags_keep_their_histogram_when_the_slider_is_let_go() {
    for hdr_edit in [false, true] {
        let (mut h, _) = detail([1200, 800], 1.0);
        h.app.session.execute("develop.hdr", &json!({"enabled": hdr_edit})).unwrap();
        let t = trace_histogram_over_a_drag(&mut h);
        assert!(!same_shape(&t.before, &t.last), "the drag changed the histogram (hdr {hdr_edit})");
        for (i, f) in t.released.iter().enumerate() {
            assert!(same_shape(f, &t.last), "hdr {hdr_edit}: frame {i} after the release shows another histogram");
        }
        assert!(same_shape(&t.settled, &t.last), "hdr {hdr_edit}: the settled view shows another histogram");
    }
}

// Given an HDR edit shown in HDR (its drag renders no 8-bit frames), the release goes from the
// drag's live histogram to the released render's, never back to the one from before the drag
#[test]
fn an_hdr_view_drag_never_shows_its_histogram_from_before_on_release() {
    for hq in [false, true] {
        let (mut h, _) = detail([1200, 800], 1.0);
        h.app.ui.settings.high_quality_adjustment_previews = hq;
        h.app.services.hdr_output = Some(Box::new(FakeHdrDisplay));
        h.app.hdr_headroom = Some(4.0);
        h.app.session.execute("develop.hdr", &json!({"enabled": true})).unwrap();
        let t = trace_histogram_over_a_drag(&mut h);
        assert!(!same_shape(&t.before, &t.settled), "the drag changed the histogram (hq {hq})");
        for (i, f) in t.released.iter().enumerate() {
            assert!(!same_shape(f, &t.before), "hq {hq}: frame {i} after the release shows the histogram from before the drag");
            assert!(same_shape(f, &t.last) || same_shape(f, &t.settled), "hq {hq}: frame {i} after the release shows a third histogram");
        }
    }
}

// Given a slider let go at 1:1 (high quality adjustment previews off or on), the part on screen
// is one full-quality window of the final look, the pixels a full-size render has there, instead
// of a render per tile on screen; only the ring of tiles around it is prepared
#[test]
fn a_release_at_one_to_one_shows_one_full_quality_window_instead_of_the_tiles_on_screen() {
    for hq in [false, true] {
        let (mut h, work) = detail([3000, 2000], 1.0);
        h.app.renderer.keep_pixels = true;
        h.app.ui.settings.high_quality_adjustment_previews = hq;
        h.app.ui.zoom = Zoom::Percent(100.0);
        assert!(h.settle(TIMEOUT));
        h.app.session.begin_interaction("Exposure").unwrap();
        for i in 1..=4 {
            h.app.session.execute("develop.set", &json!({"values": {"light.exposure": f64::from(i) * 0.2}})).unwrap();
            h.step();
            h.step();
        }
        assert!(h.settle(TIMEOUT));
        let from = work.borrow().asked.len();
        h.app.session.end_interaction().unwrap();
        assert!(h.settle(TIMEOUT));
        let view = h.app.region_view.expect("a zoomed view");
        let side = view.tile;
        let on_screen = |slot: &Slot| match slot {
            Slot::Tile { col, row, .. } => {
                let (x, y) = (usize::from(*col) * side, usize::from(*row) * side);
                x < view.window.x + view.window.w && x + side > view.window.x && y < view.window.y + view.window.h && y + side > view.window.y
            }
            _ => false,
        };
        assert!(!work.borrow().asked.iter().skip(from).any(|(s, _)| on_screen(s)), "hq {hq}: no tile on screen rendered");
        let (tex, g) = h.app.renderer.window_texture(PANE_AFTER).expect("the window on screen");
        assert!(g.full_quality && g.look == h.app.session.develop_of(PhotoId(1)).unwrap().hash64(), "hq {hq}: the final look at full quality");
        // its pixels are the full-size render's there
        let full = h.app.session.render_job(PhotoId(1), g.full.0, g.full.1, false, true).unwrap().run().rendered.unwrap().image;
        let px = tex.pixels.as_ref().expect("kept pixels");
        // the window's outermost pixels have no context past them (the colour NR median reads one pixel further)
        // and are margin, never on screen; a side on the frame's own edge clamps like the full render does
        let bare = |i: usize, start: usize, len: usize, full: usize| (start > 0 && i < 2) || (start + len < full && i + 2 > len);
        let mut max = 0u8;
        for y in 0..g.window.h {
            for x in 0..g.window.w {
                if bare(x, g.window.x, g.window.w, full.width) || bare(y, g.window.y, g.window.h, full.height) {
                    continue;
                }
                let a = px.pixels[y * g.window.w + x].to_array();
                let b = full.data[(g.window.y + y) * full.width + g.window.x + x];
                max = max.max((0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0));
            }
        }
        assert!(max <= 2, "hq {hq}: the window differs from the full-size render by {max} levels");
    }
}

// Given a photo larger than the preview level shown whole for a moment, its full-resolution
// source is decoded in the background, so a zoom to 1:1 then renders from it with no decode;
// a photo just stepped onto is not decoded at once
#[test]
fn a_short_look_prepares_the_full_resolution_source_for_a_zoom() {
    let (mut h, work) = detail([3000, 2000], 1.0);
    let id = PhotoId(1);
    // (from when the whole photo was first shown: not at once, after the look)
    let since = h.app.whole_since.filter(|(p, _)| *p == id).map(|(_, t)| t).expect("the photo is shown whole");
    assert!(h.step_until(TIMEOUT, |h| h.app.session.media.has_source(id, lcc_engine::SourceLevel::Full)), "decoded after a look");
    let asked = h.time();
    assert!(asked - since >= crate::panels::detail::WARM_FULL_AFTER_S - 0.05, "it waited for the look ({:.2} s)", asked - since);
    let from = work.borrow().asked.len();
    h.app.ui.zoom = Zoom::Percent(100.0);
    assert!(h.settle(TIMEOUT));
    let zoomed: Vec<_> =
        work.borrow().asked.iter().skip(from).filter(|(s, _)| matches!(s, Slot::Tile { .. } | Slot::Window(_))).map(|(_, j)| j.clone()).collect();
    assert!(!zoomed.is_empty(), "the zoom rendered");
    assert!(zoomed.iter().all(|j| matches!(j.source, lcc_engine::media::SourceRef::Loaded(_))), "every 1:1 render starts from the held source");
}
