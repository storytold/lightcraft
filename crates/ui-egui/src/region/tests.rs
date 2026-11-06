use super::*;
use crate::state::AppSettings;

const BIG: usize = 16384;

fn sizes(drawn: f32, canvas: f32, native: usize) -> ViewSizes {
    ViewSizes { drawn_long: drawn, canvas_long: canvas, native_long: native, texture_side: BIG, draft: None, windows: true }
}

fn auto() -> AppSettings {
    AppSettings::default()
}

// Given a fit view on a Retina Mac (about 2800 px), the preview is enough: no original decoded
#[test]
fn a_retina_fit_view_needs_no_window() {
    let p = plan(&auto(), sizes(2800.0, 2800.0, 6000));
    assert_eq!(p.main_edge, 2560, "the preview source level");
    assert_eq!(p.window_edge, None);
}

// Given a 4K display, the fit view is 1.5x the preview: the window makes it sharp
#[test]
fn a_large_fit_view_gets_a_window() {
    let p = plan(&auto(), sizes(3840.0, 3840.0, 6000));
    assert_eq!((p.main_edge, p.window_edge), (2560, Some(3840)));
}

// Given 1:1 on a 24 MP photo in a 2800 px canvas, the whole frame stays canvas-sized and the
// window is cut from the photo's own pixels
#[test]
fn one_to_one_renders_the_canvas_and_a_native_window() {
    let p = plan(&auto(), sizes(6000.0, 2800.0, 6000));
    assert_eq!((p.main_edge, p.window_edge), (2560, Some(6000)));
}

// Given 800 %, the window is still at 100 % (the GPU magnifies it): the cost does not grow
#[test]
fn beyond_one_to_one_the_window_stays_at_native_resolution() {
    let p = plan(&auto(), sizes(48000.0, 2800.0, 6000));
    assert_eq!((p.main_edge, p.window_edge), (2560, Some(6000)));
    let q = plan(&auto(), sizes(480.0 * 100.0, 2800.0, 6000));
    assert_eq!(q.window_edge, Some(6000));
}

// Given a small photo zoomed in, the whole-frame render already holds all its pixels
#[test]
fn a_small_photo_needs_no_window() {
    let p = plan(&auto(), sizes(8000.0, 1400.0, 1000));
    assert_eq!((p.main_edge, p.window_edge), (1000, None));
}

// Given a slider drag at 400 % (drafts at 0.6 scale), the window plan does not change: it is
// the whole-frame draft that gets cheaper
#[test]
fn a_drag_shrinks_the_whole_frame_draft_not_the_window() {
    let still = plan(&auto(), sizes(24000.0, 2800.0, 6000));
    let drag = plan(&auto(), ViewSizes { draft: Some(0.6), ..sizes(24000.0, 2800.0, 6000) });
    assert!(drag.main_edge < still.main_edge, "{drag:?} vs {still:?}");
    assert_eq!(drag.window_edge, still.window_edge);
}

// Given no window (a whole-frame diagnostic), the view at rest renders the
// whole frame at the size it is drawn, but a slider drag drafts at the canvas size like the
// windowed path
#[test]
fn without_a_window_a_drag_drafts_at_the_canvas_size() {
    let no_window = |draft: Option<f32>| ViewSizes { windows: false, draft, ..sizes(24000.0, 2800.0, 6000) };
    let still = plan(&auto(), no_window(None));
    let drag = plan(&auto(), no_window(Some(0.6)));
    assert_eq!(still.window_edge, None);
    assert!(still.main_edge > 2800, "at rest the zoomed frame: {still:?}");
    assert!(drag.main_edge <= 2800, "a drag drafts at most the canvas: {drag:?}");
}

// Given a slider drag, the GPU renders it at full size and the CPU a smaller draft (#330)
#[test]
fn a_drag_shrinks_the_draft_only_on_the_cpu() {
    use lightcraft_catalog::PhotoId;
    let mut gpu = DragScale::default();
    assert_eq!(gpu.scale(None, true, None, 0.0), None, "no drag, no draft");
    assert_eq!(gpu.scale(Some((PhotoId(1), "Exposure")), true, None, 0.0), Some(1.0));
    let mut cpu = DragScale::default();
    assert_eq!(cpu.scale(Some((PhotoId(1), "Exposure")), false, None, 0.0), Some(CPU_DRAFT_SCALE));
}

// Given a drag that started before the GPU was ready, it keeps its size until it ends: the
// resolution never jumps in the middle of a drag
#[test]
fn a_drag_keeps_its_draft_size_until_it_ends() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    assert_eq!(d.scale(drag, false, None, 0.0), Some(CPU_DRAFT_SCALE));
    assert_eq!(d.scale(drag, true, None, 0.0), Some(CPU_DRAFT_SCALE), "the GPU came up mid-drag");
    assert_eq!(d.scale(None, true, None, 0.0), None);
    assert_eq!(d.scale(drag, true, None, 0.0), Some(1.0), "the next drag gets full size");
    // and the other way: the GPU lost mid-drag keeps the drag at full size until it ends
    assert_eq!(d.scale(drag, false, None, 0.0), Some(1.0));
}

/// One whole drag of `drag` at full size whose drafts all take `ms` to show, ending at `now`.
fn timed_drag(d: &mut DragScale, drag: Option<(lightcraft_catalog::PhotoId, &str)>, ms: f64, now: f64) -> Option<f32> {
    let scale = d.scale(drag, true, None, now);
    for key in 1..=6 {
        d.observe(key, ms, None);
    }
    d.scale(None, true, None, now);
    scale
}

// Given full-size drafts over the slider budget twice in a row (a LUT profile or refined mask
// the CPU renders, a photo past a GPU limit, a slow GPU), the next drag of that control on
// that photo drafts small; once could be a busy moment. Other controls and photos keep full size
#[test]
fn a_drag_over_budget_twice_drafts_small_the_next_time() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let exposure = Some((PhotoId(1), "Exposure"));
    assert_eq!(d.scale(exposure, true, Some(7), 0.0), Some(1.0));
    d.observe(7, 400.0, None); // the render from before the drag: not a draft of it
    for (key, ms) in [(8, 300.0), (9, 30.0), (10, 25.0), (11, 9.0)] {
        d.observe(key, ms, None);
        d.observe(key, ms, None); // a render is timed once, however many frames show it
    }
    assert_eq!(d.scale(exposure, true, None, 0.0), Some(1.0), "over budget, but not far enough to give up mid-drag");
    assert_eq!(d.scale(None, true, None, 0.0), None);
    assert_eq!(timed_drag(&mut d, exposure, 25.0, 1.0), Some(1.0), "one slow drag: maybe the machine was busy");
    assert_eq!(d.scale(exposure, true, None, 2.0), Some(CPU_DRAFT_SCALE));
    d.scale(None, true, None, 2.0);
    assert_eq!(d.scale(Some((PhotoId(1), "Contrast")), true, None, 2.0), Some(1.0));
    assert_eq!(d.scale(Some((PhotoId(2), "Exposure")), true, None, 2.0), Some(1.0));
}

// Given a pair found slow, it drafts small only for a while: then a drag tries full size
// again; a slow retry doubles the wait (up to its cap), a fast one forgets the verdict
#[test]
fn a_slow_verdict_expires_and_is_retried() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    timed_drag(&mut d, drag, 25.0, 0.0);
    timed_drag(&mut d, drag, 25.0, 0.0);
    assert_eq!(timed_drag(&mut d, drag, 25.0, RETRY_AFTER_S - 1.0), Some(CPU_DRAFT_SCALE), "slow until the wait is over");
    assert_eq!(timed_drag(&mut d, drag, 25.0, RETRY_AFTER_S), Some(1.0), "then a drag tries full size: still slow");
    let mut at = RETRY_AFTER_S;
    let mut wait = RETRY_AFTER_S * 2.0;
    for _ in 0..6 {
        assert_eq!(timed_drag(&mut d, drag, 25.0, at + wait - 1.0), Some(CPU_DRAFT_SCALE), "the wait doubled to {wait} s");
        assert_eq!(timed_drag(&mut d, drag, 25.0, at + wait), Some(1.0), "a retry after {wait} s, slow again");
        at += wait;
        wait = (wait * 2.0).min(RETRY_MAX_S);
    }
    assert_eq!(wait, RETRY_MAX_S, "the wait stops growing");
    assert_eq!(timed_drag(&mut d, drag, 8.0, at + wait), Some(1.0), "a retry, fast this time");
    assert_eq!(timed_drag(&mut d, drag, 25.0, at + wait + 1.0), Some(1.0), "forgotten: one slow drag is a suspicion again");
    assert_eq!(timed_drag(&mut d, drag, 25.0, at + wait + 2.0), Some(1.0), "suspect: still full size");
    assert_eq!(d.scale(drag, true, None, at + wait + 3.0), Some(CPU_DRAFT_SCALE), "slow from the start again: wait 30 s");
}

// Given a drag only just over budget, it doesn't count against the pair (it would flip between
// the two sizes); one clearly over does
#[test]
fn only_a_drag_clearly_over_budget_counts() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    for t in 0..4 {
        timed_drag(&mut d, drag, 18.0, f64::from(t));
    }
    assert_eq!(d.scale(drag, true, None, 4.0), Some(1.0), "18 ms drafts: just over the 16 ms budget");
    d.scale(None, true, None, 4.0);
    timed_drag(&mut d, drag, VERDICT_MS + 5.0, 5.0);
    timed_drag(&mut d, drag, VERDICT_MS + 5.0, 6.0);
    assert_eq!(d.scale(drag, true, None, 7.0), Some(CPU_DRAFT_SCALE));
}

// Given one slow drag while the machine was busy, then fast ones, the slider keeps full size
#[test]
fn a_busy_moment_doesnt_make_a_slider_draft_small() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    timed_drag(&mut d, drag, 40.0, 0.0);
    for t in 1..=3 {
        assert_eq!(timed_drag(&mut d, drag, 8.0, f64::from(t)), Some(1.0));
    }
    timed_drag(&mut d, drag, 40.0, 4.0);
    assert_eq!(d.scale(drag, true, None, 5.0), Some(1.0), "the fast drags forgot the first slow one");
}

// Given a hostile host clock (NaN, infinite, going backwards), a drag never panics; a clock it
// can't read tries full size
#[test]
fn a_hostile_clock_never_panics() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    timed_drag(&mut d, drag, 25.0, 100.0);
    timed_drag(&mut d, drag, 25.0, 100.0);
    assert_eq!(d.scale(drag, true, None, 50.0), Some(CPU_DRAFT_SCALE), "the clock went backwards: still slow");
    d.scale(None, true, None, 50.0);
    assert_eq!(d.scale(drag, true, None, f64::NAN), Some(1.0));
    d.scale(None, true, None, f64::NAN);
    for now in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -1.0] {
        timed_drag(&mut d, drag, 25.0, now);
    }
}

// Given full-size drafts within budget (the first one, rebuilding the stage caches, may be
// slow), the next drag stays at full size
#[test]
fn a_drag_within_budget_stays_full_size() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    d.scale(drag, true, None, 0.0);
    for (key, ms) in [(1, 120.0), (2, 4.0), (3, 6.0), (4, 30.0), (5, 5.0)] {
        d.observe(key, ms, None);
    }
    d.scale(None, true, None, 0.0);
    assert_eq!(d.scale(drag, true, None, 0.0), Some(1.0));
    // hostile times are ignored, never a panic
    for (key, ms) in [(6, f64::NAN), (7, f64::INFINITY), (8, -1.0)] {
        d.observe(key, ms, None);
    }
    d.scale(None, true, None, 0.0);
    assert_eq!(d.scale(drag, true, None, 0.0), Some(1.0));
}

// Given renders within budget whose frames (the texture upload) take it over, the drag counts
// as over budget: a render time alone said 231 of 234 drafts fit while none showed in 16 ms
#[test]
fn a_drafts_time_includes_the_frame_that_shows_it() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    for _ in 0..2 {
        d.scale(drag, true, None, 0.0);
        for key in 1..=5 {
            d.observe(key, 8.0, Some(14.0)); // the previous frame, which uploaded the last draft
        }
        d.observe(5, 8.0, Some(14.0));
        d.scale(None, true, None, 0.0);
    }
    assert_eq!(d.scale(drag, true, None, 0.0), Some(CPU_DRAFT_SCALE), "8 ms renders + 14 ms frames: over budget");
    // the same renders without the host's frame times (headless) stay within budget
    let mut headless = DragScale::default();
    headless.scale(drag, true, None, 0.0);
    for _ in 0..2 {
        timed_drag(&mut headless, drag, 8.0, 0.0);
    }
    assert_eq!(headless.scale(drag, true, None, 0.0), Some(1.0));
    // a hostile frame time counts as nothing, never a panic
    let mut hostile = DragScale::default();
    hostile.scale(drag, true, None, 0.0);
    for (key, frame) in [(1, f64::NAN), (2, f64::INFINITY), (3, -1.0), (4, f64::NAN)] {
        hostile.observe(key, 8.0, Some(frame));
    }
    hostile.scale(None, true, None, 0.0);
    assert_eq!(hostile.scale(drag, true, None, 0.0), Some(1.0));
}

// Given more slow drags than are remembered, only the one remembered longest ago is forgotten:
// clearing them all would bring back a slow full-size drag for every pair
#[test]
fn a_full_slow_set_forgets_only_the_oldest_drag() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    for photo in 0..=VERDICTS_KEPT as u64 {
        d.remember_slow(PhotoId(photo), "Exposure");
    }
    assert_eq!(d.scale(Some((PhotoId(0), "Exposure")), true, None, 0.0), Some(1.0), "the oldest is forgotten");
    d.scale(None, true, None, 0.0);
    for photo in [1, VERDICTS_KEPT as u64] {
        assert_eq!(d.scale(Some((PhotoId(photo), "Exposure")), true, None, 0.0), Some(CPU_DRAFT_SCALE), "photo {photo}");
        d.scale(None, true, None, 0.0);
    }
    d.remember_slow(PhotoId(1), "Exposure"); // already remembered: nothing is forgotten
    assert_eq!(d.scale(Some((PhotoId(2), "Exposure")), true, None, 0.0), Some(CPU_DRAFT_SCALE));
}

// Given draft frames that took seconds (the window hidden or occluded mid-drag), they count as
// unknown: fast renders don't mark the drag slow because the window wasn't drawing. Two such
// drags, since one slow drag only makes the pair suspect
#[test]
fn a_hidden_windows_frame_times_dont_mark_a_drag_slow() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    for t in 0..2u32 {
        assert_eq!(d.scale(drag, true, None, f64::from(t)), Some(1.0), "drag {}", t + 1);
        for key in 1..=6 {
            d.observe(u64::from(t) * 10 + key, 6.0, Some(4000.0));
        }
        d.scale(None, true, None, f64::from(t));
    }
    assert_eq!(d.scale(drag, true, None, 2.0), Some(1.0), "after two hidden-window drags: still full size");
}

// Given full-size drafts far over budget (a refined mask the CPU renders at 190 ms a step),
// the drag drops to the small draft after a few of them instead of at its end; after a second
// such drag, the next starts small
#[test]
fn a_drag_far_over_budget_drafts_small_before_it_ends() {
    use lightcraft_catalog::PhotoId;
    let mut d = DragScale::default();
    let drag = Some((PhotoId(1), "Exposure"));
    assert_eq!(d.scale(drag, true, None, 0.0), Some(1.0));
    let mut scales = Vec::new();
    for key in 1..=8 {
        d.observe(key, 190.0, None);
        scales.push(d.scale(drag, true, None, 0.0));
    }
    // drafts 1–4 are timed one frame late; the median of 2–4 decides on the fifth frame
    assert_eq!(scales[..4], [Some(1.0); 4], "{scales:?}");
    assert_eq!(scales[4..], [Some(CPU_DRAFT_SCALE); 4], "{scales:?}");
    d.scale(None, true, None, 0.0);
    assert_eq!(timed_drag(&mut d, drag, 190.0, 1.0), Some(1.0), "one such drag: maybe a busy moment");
    assert_eq!(d.scale(drag, true, None, 2.0), Some(CPU_DRAFT_SCALE));
}

// Given a full-size drag zoomed in (a window covers the view), the hidden whole-frame draft is
// the small one: at 1:1 on a Retina display it cost 7–9 ms a step for pixels nobody sees
#[test]
fn a_full_size_drag_under_a_window_drafts_the_whole_frame_small() {
    let zoomed = sizes(24000.0, 2800.0, 6000);
    let small = plan(&auto(), ViewSizes { draft: Some(CPU_DRAFT_SCALE), ..zoomed });
    let full = plan(&auto(), ViewSizes { draft: Some(1.0), ..zoomed });
    assert!(full.window_edge.is_some(), "{full:?}");
    assert_eq!(full, small);
    // at fit without a window, full size stays full size
    let fit = sizes(2800.0, 2800.0, 6000);
    assert_eq!(plan(&auto(), ViewSizes { draft: Some(1.0), ..fit }), plan(&auto(), fit));
}

// Given a drag at fit at full size, the draft is as sharp as the render at rest
#[test]
fn a_full_size_drag_at_fit_keeps_the_resolution() {
    for drawn in [1400.0, 2800.0] {
        let still = plan(&auto(), sizes(drawn, drawn, 6000));
        let drag = plan(&auto(), ViewSizes { draft: Some(1.0), ..sizes(drawn, drawn, 6000) });
        assert_eq!(drag, still, "{drawn}");
    }
}

// Given no window and a full-size drag, the draft is still at most the canvas: a drag at 1:1
// must not render the photo's every pixel each frame
#[test]
fn a_full_size_drag_without_a_window_drafts_at_the_canvas_size() {
    let drag = plan(&auto(), ViewSizes { windows: false, draft: Some(1.0), ..sizes(24000.0, 2800.0, 6000) });
    assert!(drag.main_edge <= 2816, "{drag:?}");
}

// Given the user capped the preview size, the whole-frame render obeys it, and a fit view
// stays on it (no original decoded); a zoomed view still gets its sharp window
#[test]
fn a_preview_limit_caps_the_whole_frame_render_and_spares_the_fit_view() {
    let s = AppSettings { preview_limit: 1600, ..Default::default() };
    let fit = plan(&s, sizes(2800.0, 2800.0, 6000));
    assert_eq!((fit.main_edge, fit.window_edge), (1600, None));
    let zoomed = plan(&s, sizes(6000.0, 2800.0, 6000));
    assert_eq!((zoomed.main_edge, zoomed.window_edge), (1600, Some(6000)));
}

// Given a GPU with 2048 px textures, the whole-frame render obeys it, the window frame need not
// (a window is cut to the texture side separately)
#[test]
fn the_texture_side_caps_the_whole_frame_render_only() {
    let p = plan(&auto(), ViewSizes { texture_side: 2048, ..sizes(6000.0, 2800.0, 6000) });
    assert_eq!((p.main_edge, p.window_edge), (2048, Some(6000)));
}

// Given a picture that is not the frame (an embedded JPEG cropped differently), no window
#[test]
fn a_window_needs_a_picture_with_the_frames_aspect() {
    assert!(same_aspect(1.5, 1.5) && same_aspect(1.51, 1.5));
    assert!(!same_aspect(1.5, 1.0) && !same_aspect(f32::NAN, 1.0) && !same_aspect(1.0, 0.0));
}

// Given a drag at fit on a 4K or 5K display (where the fit view has a window), the window stays:
// it is only the whole-frame draft that gets smaller
#[test]
fn a_drag_at_fit_on_a_large_display_keeps_the_window() {
    for drawn in [3300.0, 3840.0, 4096.0, 5120.0] {
        let still = plan(&auto(), sizes(drawn, drawn, 6000));
        let drag = plan(&auto(), ViewSizes { draft: Some(0.6), ..sizes(drawn, drawn, 6000) });
        assert!(still.window_edge.is_some(), "{drawn}: {still:?}");
        assert_eq!(drag.window_edge, still.window_edge, "{drawn}");
    }
}

// Given a mode whose picture can't be a window (a whole-frame diagnostic, or a
// spot read too far for one render), the whole-frame render is as big as the view
#[test]
fn without_windows_the_whole_frame_is_rendered_at_the_drawn_size() {
    let p = plan(&auto(), ViewSizes { windows: false, ..sizes(6000.0, 2800.0, 6000) });
    assert_eq!((p.main_edge, p.window_edge), (6000, None));
    let fit = plan(&auto(), ViewSizes { windows: false, ..sizes(2800.0, 2800.0, 6000) });
    assert_eq!((fit.main_edge, fit.window_edge), (2816, None));
    let deep = plan(&auto(), ViewSizes { windows: false, ..sizes(48000.0, 2800.0, 6000) });
    assert_eq!(deep.main_edge, 6000, "never above the photo's own pixels");
    let small = plan(&auto(), ViewSizes { windows: false, texture_side: 2048, ..sizes(6000.0, 2800.0, 6000) });
    assert_eq!(small.main_edge, 2048);
}

// Given a user who chose a preview size above the preview source level (3840 or 5120 on a big
// display), the whole-frame render is that big
#[test]
fn a_large_preview_limit_is_honoured() {
    for limit in [3840u32, 5120] {
        let s = AppSettings { preview_limit: limit, ..Default::default() };
        let p = plan(&s, sizes(5120.0, 5120.0, 6000));
        assert_eq!(p.main_edge, limit as usize, "{limit}");
        assert_eq!(p.window_edge, None, "a fit view at the limit needs no window");
    }
    let s = AppSettings { preview_limit: 2560, ..Default::default() };
    assert_eq!(plan(&s, sizes(5120.0, 5120.0, 6000)).main_edge, 2560);
}

// Given a medium format photo wider than 8192 px, 1:1 is its own pixels
#[test]
fn a_window_frame_can_be_as_big_as_the_photo() {
    let p = plan(&auto(), sizes(40000.0, 2800.0, 11648));
    assert_eq!(p.window_edge, Some(11648));
}

// Given a drag at fit on a Retina Mac, the window does not switch on (it would flicker with the drag)
#[test]
fn a_drag_at_fit_does_not_switch_the_window_on() {
    let p = plan(&auto(), ViewSizes { draft: Some(0.6), ..sizes(2800.0, 2800.0, 6000) });
    assert_eq!(p.window_edge, None);
}

// Given a pinch from fit to 400 %, the sizes of the first frame hold until the gesture is quiet
#[test]
fn a_pinch_holds_the_render_sizes() {
    use lightcraft_catalog::PhotoId;
    let (fit, zoomed) = (plan(&auto(), sizes(1400.0, 1400.0, 6000)), plan(&auto(), sizes(24000.0, 1400.0, 6000)));
    assert_ne!(fit, zoomed);
    let mut hold = SizeHold::default();
    let p = PhotoId(1);
    assert_eq!(hold.apply(p, 0.0, false, fit), fit);
    // the pinch runs for a second: every frame would plan something else
    for k in 1..60 {
        assert_eq!(hold.apply(p, k as f64 / 60.0, true, zoomed), fit, "frame {k}");
    }
    // it stopped at t = 59/60: still held for a moment (no flicker between two events)…
    assert!(hold.holding(1.1));
    assert_eq!(hold.apply(p, 1.1, false, zoomed), fit);
    // …then the view is planned for where it ended
    assert!(!hold.holding(59.0 / 60.0 + HOLD_SECS + 0.01));
    assert_eq!(hold.apply(p, 1.3, false, zoomed), zoomed);
    assert_eq!(hold.apply(p, 1.4, false, zoomed), zoomed);
}

// Given a pinch, the display-fit size holds like the render sizes and is retaken after it
#[test]
fn a_pinch_holds_the_display_fit_size() {
    use lightcraft_catalog::PhotoId;
    let (fit, zoomed) = (plan(&auto(), sizes(1400.0, 1400.0, 6000)), plan(&auto(), sizes(24000.0, 1400.0, 6000)));
    let mut hold = SizeHold::default();
    let p = PhotoId(1);
    hold.apply(p, 0.0, false, fit);
    assert_eq!(hold.display_fit(0.0, false, [1000, 800]), [1000, 800]);
    for k in 1..60 {
        let now = k as f64 / 60.0;
        hold.apply(p, now, true, zoomed);
        assert_eq!(hold.display_fit(now, true, [1000 + k, 800 + k]), [1000, 800], "frame {k}");
    }
    // stopped at t = 59/60: still held within the hold time…
    hold.apply(p, 1.1, false, zoomed);
    assert_eq!(hold.display_fit(1.1, false, [2560, 2048]), [1000, 800]);
    // …then the size it ended at is taken, and kept while nothing moves
    hold.apply(p, 1.3, false, zoomed);
    assert_eq!(hold.display_fit(1.3, false, [2560, 2048]), [2560, 2048]);
    assert_eq!(hold.display_fit(1.4, false, [2561, 2049]), [2561, 2049]);
    // another photo forgets the held size
    hold.apply(p, 1.5, true, zoomed);
    hold.display_fit(1.5, true, [2561, 2049]);
    hold.apply(PhotoId(2), 1.6, true, zoomed);
    assert_eq!(hold.display_fit(1.6, true, [640, 480]), [640, 480]);
}

// The plan and display fit become fresh on the exact quiet deadline, in the same frame.
#[test]
fn the_display_fit_and_plan_release_at_the_exact_deadline() {
    use lightcraft_catalog::PhotoId;
    let mut hold = SizeHold::default();
    let photo = PhotoId(1);
    let fit = LoupePlan { main_edge: 800, window_edge: None };
    let zoomed = LoupePlan { main_edge: 1600, window_edge: Some(6000) };
    assert_eq!(hold.apply(photo, 0.0, false, fit), fit);
    assert_eq!(hold.display_fit(0.0, false, [508, 762]), [508, 762]);
    assert_eq!(hold.apply(photo, 1.0, true, zoomed), fit);
    assert_eq!(hold.display_fit(1.0, true, [549, 823]), [508, 762]);
    let before_deadline = 1.0 + HOLD_SECS / 2.0;
    assert_eq!(hold.apply(photo, before_deadline, false, zoomed), fit);
    assert_eq!(hold.display_fit(before_deadline, false, [549, 823]), [508, 762]);
    assert!(hold.holding(before_deadline));
    let deadline = 1.0 + HOLD_SECS;
    assert_eq!(hold.apply(photo, deadline, false, zoomed), zoomed);
    assert_eq!(hold.display_fit(deadline, false, [549, 823]), [549, 823]);
    assert!(!hold.holding(deadline));
}

// Given another photo, the held sizes are forgotten
#[test]
fn another_photo_is_planned_afresh() {
    use lightcraft_catalog::PhotoId;
    let (fit, zoomed) = (plan(&auto(), sizes(1400.0, 1400.0, 6000)), plan(&auto(), sizes(24000.0, 1400.0, 6000)));
    let mut hold = SizeHold::default();
    hold.apply(PhotoId(1), 0.0, false, fit);
    assert_eq!(hold.apply(PhotoId(2), 0.1, true, zoomed), zoomed);
}

// Given side by side, a window is drawn in its pane only; given a wipe, on its side of the line
#[test]
fn a_window_is_clipped_to_its_pane_or_its_side_of_the_wipe() {
    use crate::state::BeforeAfter::*;
    let canvas = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1000.0, 600.0));
    let (left, right) = (
        egui::Rect::from_min_max(egui::pos2(24.0, 24.0), egui::pos2(494.0, 576.0)),
        egui::Rect::from_min_max(egui::pos2(506.0, 24.0), egui::pos2(976.0, 576.0)),
    );
    assert_eq!(window_clip(SideBySide, true, left, canvas, left), left);
    assert_eq!(window_clip(SideBySide, false, right, canvas, right), right);
    let img = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(900.0, 550.0));
    let before = window_clip(Split, true, canvas, canvas, img);
    let after = window_clip(Split, false, canvas, canvas, img);
    assert_eq!((before.max.x, after.min.x), (500.0, 500.0), "the line is the image's middle");
    assert_eq!(window_clip(SplitTopBottom, true, canvas, canvas, img).max.y, 300.0);
    assert_eq!(window_clip(SplitTopBottom, false, canvas, canvas, img).min.y, 300.0);
    // zoomed in, the photo's middle (x = 1600) is off screen: the line is the view's middle
    let zoomed = egui::Rect::from_min_max(egui::pos2(-200.0, -900.0), egui::pos2(3400.0, 1500.0));
    assert_eq!(window_clip(Split, false, canvas, canvas, zoomed).min.x, 500.0);
    assert_eq!(window_clip(SplitTopBottom, true, canvas, canvas, zoomed).max.y, 300.0);
    assert_eq!(window_clip(Off, false, canvas, canvas, img), canvas);
    assert_eq!(window_clip(Original, true, canvas, canvas, img), canvas);
}

// Given a window asked for while another is rendering and the original isn't decoded yet, it
// waits for the decoded original instead of decoding another copy
#[test]
fn a_window_waits_for_the_original_another_is_decoding() {
    assert!(defer_window(true, false));
    assert!(!defer_window(true, true), "the original is in");
    assert!(!defer_window(false, false), "nothing is decoding it");
}

// Hostile numbers give a plan, never a panic
#[test]
fn hostile_sizes_give_a_usable_plan() {
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 0.0] {
        let p = plan(&auto(), ViewSizes { drawn_long: v, canvas_long: v, draft: Some(v), ..sizes(1.0, 1.0, 6000) });
        assert!((8..=2560).contains(&p.main_edge), "{v}: {p:?}");
    }
    let p = plan(&auto(), sizes(1000.0, 1000.0, 0));
    assert!(p.main_edge >= 8);
}

// Given a visible area in the middle of a big frame, the window covers it with a margin, on the grid
#[test]
fn the_window_covers_the_visible_area_with_a_margin_on_the_grid() {
    let w = window_for(48_000, 32_000, (20_000.0, 10_000.0, 21_400.0, 10_900.0), MAX_SPAN).unwrap();
    let m = margin_for(48_000);
    assert!(w.x + m <= 20_000 && w.x + w.w >= 21_400 + m, "{w:?}");
    assert!(w.y + m <= 10_000 && w.y + w.h >= 10_900 + m, "{w:?}");
    assert_eq!((w.x % SNAP, w.y % SNAP, w.w % SNAP, w.h % SNAP), (0, 0, 0, 0));
}

// Given a small pan, the same window is kept (no re-render); a big pan changes it
#[test]
fn small_pans_keep_the_window() {
    let a = window_for(48_000, 32_000, (20_000.0, 10_000.0, 21_400.0, 10_900.0), MAX_SPAN).unwrap();
    let b = window_for(48_000, 32_000, (20_040.0, 10_030.0, 21_440.0, 10_930.0), MAX_SPAN).unwrap();
    assert_eq!(a, b);
    let c = window_for(48_000, 32_000, (26_000.0, 10_000.0, 27_400.0, 10_900.0), MAX_SPAN).unwrap();
    assert_ne!(a, c);
}

// Given a frame smaller than the margin, or a view at its edge, the window is clamped into it
#[test]
fn the_window_stays_inside_the_frame() {
    let w = window_for(3000, 2000, (-500.0, -500.0, 900.0, 700.0), MAX_SPAN).unwrap();
    assert_eq!((w.x, w.y), (0, 0));
    let w = window_for(3000, 2000, (2500.0, 1500.0, 9000.0, 9000.0), MAX_SPAN).unwrap();
    assert!(w.x + w.w <= 3000 && w.y + w.h <= 2000, "{w:?}");
    assert_eq!((w.x + w.w, w.y + w.h), (3000, 2000));
}

// Given a deeper zoom, the margin grows with the wide stages' kernels (3σ of the base, clarity
// and dehaze planes are 0.045 of the frame), up to a cap that bounds the window
#[test]
fn the_margin_grows_with_the_zoom_up_to_a_cap() {
    assert_eq!(margin_for(1000), SNAP, "never less than one grid step");
    assert!(margin_for(8000) > margin_for(3000));
    assert_eq!(margin_for(48_000), 2304, "4.5 % of an 8:1 frame, under the cap");
    assert_eq!(margin_for(80_000), MAX_MARGIN);
    assert_eq!(margin_for(0), SNAP);
    assert!(margin_for(usize::MAX) <= MAX_MARGIN);
    for l in [100, 3000, 8000, 48_000, 400_000] {
        assert_eq!(margin_for(l) % SNAP, 0);
    }
}

// Given a tile rendered for other settings, another photo or another zoom, it is not drawn
#[test]
fn a_tile_is_drawn_only_while_it_shows_what_the_loupe_shows() {
    use lightcraft_catalog::PhotoId;
    let v =
        RegionView { photo: PhotoId(1), before: false, key: 7, full: (6000, 4000), window: PixelWindow { x: 0, y: 0, w: 256, h: 256 }, settings: 11 };
    assert!(v.is_current(PhotoId(1), (6000, 4000), 11, false));
    assert!(!v.is_current(PhotoId(2), (6000, 4000), 11, false), "another photo");
    assert!(!v.is_current(PhotoId(1), (6000, 4001), 11, false), "another zoom");
    assert!(!v.is_current(PhotoId(1), (6000, 4000), 12, false), "another look (an edit, an undo)");
    assert!(v.is_current(PhotoId(1), (6000, 4000), 12, true), "a drag's drafts follow the value");
    assert!(!v.is_current(PhotoId(2), (6000, 4000), 12, true), "…of this photo only");
}

// Given a GPU that allows 2048 px textures, no window is wider than that
#[test]
fn a_window_fits_the_texture_side() {
    for tex in [512, 2048, 4096] {
        let span = max_span(tex);
        assert!(span <= tex.max(SNAP) && span.is_multiple_of(SNAP), "{tex}: {span}");
        let w = window_for(48_000, 32_000, (20_000.0, 10_000.0, 21_400.0, 10_900.0), span).unwrap();
        assert!(w.w <= span && w.h <= span, "{tex}: {w:?}");
    }
    assert_eq!(max_span(100_000), MAX_SPAN);
    assert_eq!(max_span(0), SNAP);
}

// Given nothing visible, or hostile numbers, there is no window (never a panic)
#[test]
fn nothing_visible_or_hostile_input_is_no_window() {
    assert_eq!(window_for(1000, 1000, (2000.0, 0.0, 3000.0, 100.0), MAX_SPAN), None);
    assert_eq!(window_for(1000, 1000, (10.0, 10.0, 10.0, 90.0), MAX_SPAN), None);
    assert_eq!(window_for(0, 1000, (0.0, 0.0, 10.0, 10.0), MAX_SPAN), None);
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(window_for(1000, 1000, (v, 0.0, 10.0, 10.0), MAX_SPAN), None);
    }
    let w = window_for(usize::MAX / 2, usize::MAX / 2, (0.0, 0.0, 1e30, 1e30), MAX_SPAN).unwrap();
    assert!(w.w <= MAX_SPAN && w.h <= MAX_SPAN, "{w:?}");
}

// Given a view of an enormous area, the window is bounded
#[test]
fn a_window_is_bounded() {
    let w = window_for(100_000, 100_000, (0.0, 0.0, 90_000.0, 90_000.0), MAX_SPAN).unwrap();
    assert!(w.w <= MAX_SPAN && w.h <= MAX_SPAN);
}
