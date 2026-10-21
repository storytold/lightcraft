//! Sharp zoomed views (issue #323). The loupe renders the whole frame in one texture, which can't
//! be as large as a 1:1 or 8:1 view of a big photo. Past that size it also renders just the window
//! of the frame that is on screen, at the zoom scale, and draws it over the whole-frame render.
//!
//! This module is the geometry: when a window is needed and which one. Windows are snapped to a
//! grid and carry a margin, so panning by a few pixels re-uses the render and the spatial stages
//! (clarity, dehaze…) have the context they read around the visible edge.

use lightcraft_engine::pipeline::PixelWindow;

/// Windows start and end on multiples of this many pixels of the zoomed frame.
pub const SNAP: usize = 256;
/// The most context rendered beyond the visible pixels (a multiple of [`SNAP`]).
/// 4.5 % of an 11:1 view of a 24 MP frame, snapped to [`SNAP`]. The old 768 px cap left Dehaze
/// tens of codes off the full-frame render from 4:1 up (#1146).
pub const MAX_MARGIN: usize = 3072;
/// The most a window may span on either axis: a bound on its texture whatever the window size.
pub const MAX_SPAN: usize = if cfg!(target_arch = "wasm32") { 3072 } else { 6144 };

/// A window render the loupe asked for: which photo, the size of the zoomed frame it is a window
/// of, and the window. The texture that comes back is drawn at this place of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionView {
    pub photo: lightcraft_catalog::PhotoId,
    /// The Before side of a Before/After view (the photo without its edits).
    pub before: bool,
    pub key: u64,
    pub full: (usize, usize),
    pub window: PixelWindow,
    /// Hash of the render state it was rendered with (develop settings plus any window-safe overlay).
    pub settings: u64,
}

impl RegionView {
    /// Whether this window's pixels still belong in a loupe showing `photo` with `settings` in a
    /// frame of `full` pixels (they are drawn until the next window arrives, if so).
    ///
    /// While a slider is dragged (`drafting`) a window made for an earlier value of it still
    /// belongs: drafts follow the drag, and waiting for an exact match would show none.
    pub fn is_current(&self, photo: lightcraft_catalog::PhotoId, full: (usize, usize), settings: u64, drafting: bool) -> bool {
        self.photo == photo && self.full == full && (drafting || self.settings == settings)
    }
}

/// Context to render around the visible pixels of a frame `full_long` pixels along: enough for the
/// wide stages (base, clarity and dehaze planes have sigmas up to 0.02 of it) up to a cap.
pub fn margin_for(full_long: usize) -> usize {
    let want = (full_long as f64 * 0.045).min(MAX_MARGIN as f64) as usize;
    want.div_ceil(SNAP).max(1) * SNAP
}

/// The widest a window may be on a host whose GPU textures are at most `texture_side` px: a
/// window is one texture, and a texture over the limit makes some backends (egui_glow, i.e. the
/// browser build) panic. A multiple of [`SNAP`], at least one grid step, at most [`MAX_SPAN`].
pub fn max_span(texture_side: usize) -> usize {
    (texture_side.min(MAX_SPAN) / SNAP * SNAP).max(SNAP)
}

/// A window render is worth it only when the whole-frame render is this much smaller than the
/// zoomed frame: a little softness (a Retina fit view of 2800 px from the 2560 px preview) is not
/// worth decoding the original for.
pub const WINDOW_RATIO: f32 = 1.25;

/// How big the loupe's view of the photo is, and what the host allows.
#[derive(Clone, Copy, Debug)]
pub struct ViewSizes {
    /// Long edge of the photo as drawn (physical px): the zoomed frame.
    pub drawn_long: f32,
    /// Long edge of the canvas it is drawn in (physical px).
    pub canvas_long: f32,
    /// The photo's own long edge as shown (after crop and rotation).
    pub native_long: usize,
    /// The largest texture the GPU takes.
    pub texture_side: usize,
    /// While a slider is dragged, the scale of the whole-frame draft against the render at rest
    /// (1.0 or [`CPU_DRAFT_SCALE`], from [`DragScale`]); a draft is never above the canvas size.
    /// `None`: no drag.
    pub draft: Option<f32>,
    /// Whether a window can be drawn over the whole-frame render (not with a diagnostic overlay
    /// that needs whole-frame context, or a read too far for one window). Mask
    /// overlays and soft proofing are window-safe.
    pub windows: bool,
}

/// What the loupe renders for a view: the whole frame at about canvas size, and, once the view is
/// zoomed past what that holds, the window on screen at no more than 100 %.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoupePlan {
    /// Long edge of the whole-frame render ([`Slot::Main`](crate::render::Slot::Main)): never more
    /// than the canvas shows, the user's preview limit, the texture side or the preview source level.
    pub main_edge: usize,
    /// Long edge of the zoomed frame the window is cut from (never above the photo's own pixels:
    /// beyond 100 % the GPU magnifies the window), or `None`: the whole-frame render is enough.
    pub window_edge: Option<usize>,
}

/// Size of a slider drag's whole-frame draft when full size would be too slow (the CPU renders
/// it, or full-size drafts of this drag went over [`DRAFT_BUDGET_MS`]), against the render at rest.
pub const CPU_DRAFT_SCALE: f32 = 0.6;

/// The slider budget (ms): a drag whose full-size drafts take longer gets smaller drafts next time.
/// A draft's time is its render plus the frame that shows it (the texture upload is part of it).
pub const DRAFT_BUDGET_MS: f64 = 16.0;

/// Full-size drafts this many times over budget give up on full size in the middle of the drag:
/// a drag at 200 ms a step is worse than one jump in resolution.
const GIVE_UP_FACTOR: f64 = 2.0;

/// The full-size drafts (after the first) a drag times before it may give up on full size.
const GIVE_UP_AFTER: usize = 3;

/// The most (photo, control) pairs [`DragScale`] keeps a verdict on; past it, the one remembered
/// longest ago is forgotten.
const VERDICTS_KEPT: usize = 256;

/// A host frame time (ms) longer than this is not a texture upload but a window that was hidden,
/// occluded or stalled: it counts as unknown, like a frame time from no host at all.
const FRAME_MS_KNOWN: f64 = 1000.0;

/// A drag counts against its (photo, control) pair only when its full-size drafts took longer than
/// this (ms): a quarter over the budget, so a drag that only just misses it keeps full size.
const VERDICT_MS: f64 = DRAFT_BUDGET_MS * 1.25;

/// How long (s) a pair found slow drafts small before a drag of it tries full size again: the
/// machine may only have been busy. Each retry that is slow too doubles the wait, up to
/// [`RETRY_MAX_S`]. A retry costs at most a few slow drafts before the drag gives up on full size.
const RETRY_AFTER_S: f64 = 30.0;

/// The longest wait (s) before a slow pair tries full size again.
const RETRY_MAX_S: f64 = 240.0;

/// What [`DragScale`] remembers about the drags of one control on one photo.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict {
    /// One drag was over budget at full size: maybe a busy moment, so the next stays full size.
    Suspect,
    /// Two drags in a row were: drafts are small until `until` (the host's clock, s), then a drag
    /// tries full size again; `wait` is how long this verdict holds (s).
    Slow { until: f64, wait: f64 },
}

/// The size of a slider drag's whole-frame draft, chosen when the drag starts and kept until it
/// ends (a change in the middle would make the resolution jump), unless its full-size drafts are
/// far over budget. A drag renders at full size when the GPU draws the view and the same control
/// on this photo isn't known to be too slow at that size; otherwise at [`CPU_DRAFT_SCALE`] (issue
/// #330). Whether the GPU drew a render is not enough on its own: some views (a LUT profile,
/// refined masks, a photo past a GPU limit) fall back to the CPU per render, and a large draft
/// costs a texture upload too, which only the time a draft took to show tells.
///
/// A drag's time is wall-clock time, what the user waits for. So that a moment the machine was busy
/// (another app, a backup) can't make a slider draft small for the rest of the session, a pair is
/// slow only after two drags in a row over [`VERDICT_MS`], and only for a while: after
/// [`RETRY_AFTER_S`] a drag tries full size again, a fast one forgets the verdict and a slow one
/// doubles the wait. A verdict is for its photo only: the same control on another photo (another
/// size, other edits) is timed afresh.
#[derive(Clone, Debug, Default)]
pub struct DragScale {
    /// The drag running now: (photo, control) and its scale.
    current: Option<((lightcraft_catalog::PhotoId, String), f32)>,
    /// How long the current drag's full-size drafts took to show (ms).
    samples: Vec<f64>,
    /// Key of the whole-frame render last timed (each render is timed once).
    seen: Option<u64>,
    /// The render time (ms) of a draft that reached the screen this frame: the time of the frame
    /// that uploads it is only known on the next one.
    pending: Option<f64>,
    /// Pairs whose full-size drafts went over budget, the one remembered longest ago first.
    verdicts: std::collections::VecDeque<((lightcraft_catalog::PhotoId, String), Verdict)>,
}

impl DragScale {
    /// The draft scale for this frame, or `None` without a drag. `drag` is the photo and control
    /// being dragged; `gpu` whether the GPU draws this view; `shown` the key of the whole-frame
    /// render on screen (a render from before the drag is not one of its drafts); `now` the host's
    /// clock (s; egui's `input.time`).
    pub fn scale(&mut self, drag: Option<(lightcraft_catalog::PhotoId, &str)>, gpu: bool, shown: Option<u64>, now: f64) -> Option<f32> {
        let Some((photo, control)) = drag else {
            self.finish(now);
            return None;
        };
        match &self.current {
            Some(((p, c), s)) if *p == photo && c == control => return Some(*s),
            Some(_) => self.finish(now),
            None => {}
        }
        let key = (photo, control.to_string());
        let small = matches!(self.verdict(&key), Some(Verdict::Slow { until, .. }) if now < until);
        let s = if gpu && !small { 1.0 } else { CPU_DRAFT_SCALE };
        self.current = Some((key, s));
        self.seen = shown;
        Some(s)
    }

    /// Once per frame: `key` identifies the whole-frame render on screen and `render_ms` is how
    /// long it took; `last_frame_ms` is how long the previous frame took, painting included (the
    /// host's report; `None` without one, e.g. headless). Times the current drag's full-size
    /// drafts, each as its render plus the frame that uploaded and showed it, and gives up on full
    /// size when they are far over budget.
    pub fn observe(&mut self, key: u64, render_ms: f64, last_frame_ms: Option<f64>) {
        let full = matches!(self.current, Some((_, s)) if s >= 1.0);
        if !full {
            self.pending = None;
            return;
        }
        if let Some(render) = self.pending.take() {
            let frame = last_frame_ms.filter(|ms| ms.is_finite() && (0.0..=FRAME_MS_KNOWN).contains(ms)).unwrap_or(0.0);
            if self.samples.len() < 1024 {
                self.samples.push(render + frame);
            }
            if self.samples.len() > GIVE_UP_AFTER
                && median_after_first(&self.samples).is_some_and(|m| m > DRAFT_BUDGET_MS * GIVE_UP_FACTOR)
                && let Some((_, s)) = self.current.as_mut()
            {
                *s = CPU_DRAFT_SCALE;
                return;
            }
        }
        if self.seen == Some(key) {
            return;
        }
        self.seen = Some(key);
        if render_ms.is_finite() && render_ms >= 0.0 {
            self.pending = Some(render_ms);
        }
    }

    /// Remember drags of `control` on `photo` as too slow for full-size drafts, for good (tests).
    #[cfg(test)]
    pub fn remember_slow(&mut self, photo: lightcraft_catalog::PhotoId, control: &str) {
        self.set_verdict((photo, control.to_string()), Some(Verdict::Slow { until: f64::INFINITY, wait: RETRY_MAX_S }));
    }

    fn verdict(&self, key: &(lightcraft_catalog::PhotoId, String)) -> Option<Verdict> {
        self.verdicts.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    /// Set or (`None`) forget a pair's verdict. A new pair forgets the one remembered longest ago
    /// when the list is full (not all of them: each forgotten pair costs one more slow drag).
    fn set_verdict(&mut self, key: (lightcraft_catalog::PhotoId, String), verdict: Option<Verdict>) {
        let at = self.verdicts.iter().position(|(k, _)| *k == key);
        match (at, verdict) {
            (Some(i), Some(v)) => {
                if let Some(entry) = self.verdicts.get_mut(i) {
                    entry.1 = v;
                }
            }
            (Some(i), None) => {
                self.verdicts.remove(i);
            }
            (None, Some(v)) => {
                while self.verdicts.len() >= VERDICTS_KEPT {
                    self.verdicts.pop_front();
                }
                self.verdicts.push_back((key, v));
            }
            (None, None) => {}
        }
    }

    /// The drag ended. A drag timed at full size updates its pair's verdict: over [`VERDICT_MS`]
    /// a first time makes it suspect, a second slow for [`RETRY_AFTER_S`], and a slow retry
    /// doubles the wait; within it, the verdict is forgotten. A drag never timed at full size (it
    /// drafted small, or was too short) changes nothing.
    fn finish(&mut self, now: f64) {
        if let Some((key, _)) = self.current.take()
            && let Some(median) = median_after_first(&self.samples)
        {
            let verdict = if median > VERDICT_MS {
                let wait = match self.verdict(&key) {
                    None => None,
                    Some(Verdict::Suspect) => Some(RETRY_AFTER_S),
                    Some(Verdict::Slow { wait, .. }) => Some((wait * 2.0).min(RETRY_MAX_S)),
                };
                Some(wait.map_or(Verdict::Suspect, |wait| Verdict::Slow { until: now + wait, wait }))
            } else {
                None
            };
            self.set_verdict(key, verdict);
        }
        self.samples.clear();
        self.seen = None;
        self.pending = None;
    }
}

/// The median of a drag's draft times (ms), leaving out the first draft (it rebuilds the stage
/// caches the others reuse); `None` before a second draft.
fn median_after_first(samples: &[f64]) -> Option<f64> {
    let mut rest: Vec<f64> = samples.iter().skip(1).copied().collect();
    rest.sort_by(f64::total_cmp);
    rest.get(rest.len() / 2).copied()
}

/// The loupe's render plan. The cost of a render follows the canvas, not the zoom: the whole-frame
/// render is canvas-sized, and the window holds only the pixels on screen at no more than 100 %.
pub fn plan(settings: &crate::state::AppSettings, v: ViewSizes) -> LoupePlan {
    let canvas_long = if v.canvas_long.is_finite() { v.canvas_long.max(8.0) } else { 8.0 };
    let drawn_long = if v.drawn_long.is_finite() { v.drawn_long.max(8.0) } else { canvas_long };
    let scale = v.draft.map_or(1.0, |s| if s.is_finite() { s.clamp(0.1, 1.0) } else { 1.0 });
    if !v.windows {
        // the whole frame at the size it is drawn (a whole-frame diagnostic has no
        // window); while a slider drags, a canvas-sized draft as with windows: a drag at 1:1 must
        // not render the photo's every pixel each frame
        let long = if v.draft.is_some() { drawn_long.min(canvas_long) } else { drawn_long };
        let main_edge = settings.loupe_edge(long * scale, v.native_long, v.texture_side);
        return LoupePlan { main_edge, window_edge: None };
    }
    // a user who chose a size above the preview source level (3840, 5120) means it; otherwise the
    // original is not decoded for the whole-frame render
    let source_cap = if settings.preview_limit == 0 { lightcraft_engine::SourceLevel::Preview.max_edge() } else { settings.preview_limit as usize };
    let edge_at = |scale: f32| settings.loupe_edge(drawn_long.min(canvas_long) * scale, v.native_long, v.texture_side).min(source_cap);
    let frame_edge = settings.window_frame_edge(drawn_long, v.native_long);
    // a user who capped the preview size has chosen speed over sharpness at fit
    let fit = drawn_long <= canvas_long * 1.05;
    // (against the undrafted whole-frame render: a drag must not switch the window off and on)
    let window_edge = (frame_edge as f32 > edge_at(1.0) as f32 * WINDOW_RATIO && !(fit && settings.preview_limit != 0)).then_some(frame_edge);
    // with a window over it, the whole-frame draft is hidden (it feeds the histogram and stands in
    // where the window has no pixels yet): the small draft does, and the window keeps the frame time
    let scale = if window_edge.is_some() && v.draft.is_some() { scale.min(CPU_DRAFT_SCALE) } else { scale };
    LoupePlan { main_edge: edge_at(scale), window_edge }
}

/// How long the render sizes stay as they were after a pinch or two-finger scroll stops (s).
pub const HOLD_SECS: f64 = 0.25;

/// Holds the loupe's render sizes while a gesture runs (a continuous pinch changes the zoom every
/// frame; re-planning each time would be a stream of renders of every size in between). The view
/// keeps being drawn from the renders it has, magnified by the GPU, and is re-planned once the
/// gesture has been quiet for [`HOLD_SECS`].
#[derive(Clone, Debug, Default)]
pub struct SizeHold {
    photo: Option<lightcraft_catalog::PhotoId>,
    plan: Option<LoupePlan>,
    until: f64,
}

impl SizeHold {
    /// The plan to render this frame: `fresh` normally; the plan from before the gesture while
    /// one is running (`gesturing`) and for [`HOLD_SECS`] after. `now` is in seconds.
    pub fn apply(&mut self, photo: lightcraft_catalog::PhotoId, now: f64, gesturing: bool, fresh: LoupePlan) -> LoupePlan {
        if self.photo != Some(photo) {
            *self = SizeHold { photo: Some(photo), plan: Some(fresh), until: f64::NEG_INFINITY };
            return fresh;
        }
        if gesturing {
            self.until = now + HOLD_SECS;
        }
        match self.plan {
            Some(held) if gesturing || now < self.until => held,
            _ => {
                self.plan = Some(fresh);
                fresh
            }
        }
    }

    /// Whether sizes are being held at `now` (the caller asks for a repaint when it ends).
    pub fn holding(&self, now: f64) -> bool {
        now < self.until
    }
}

/// Where the line of a wipe crosses: the middle of the part of the photo that is on screen. At
/// Fit that is the photo's middle; zoomed in, the photo's own middle is mostly off screen, and a
/// line there left the whole view to one side (#1351).
pub fn wipe_line(img: egui::Rect, canvas: egui::Rect) -> egui::Pos2 {
    let shown = img.intersect(canvas);
    if shown.is_positive() { shown.center() } else { img.center() }
}

/// Where a window of one side of a Before/After view may be drawn: its own pane in side by side
/// and stacked views (a window carries margin beyond what its pane shows, which would otherwise
/// spill into the neighbour), its side of the line in a wipe, else the canvas.
pub fn window_clip(mode: crate::state::BeforeAfter, before_side: bool, pane: egui::Rect, canvas: egui::Rect, img: egui::Rect) -> egui::Rect {
    use crate::state::BeforeAfter;
    let (min, max) = (canvas.min, canvas.max);
    let line = wipe_line(img, canvas);
    let clip = match (mode, before_side) {
        (BeforeAfter::Split, true) => egui::Rect::from_min_max(min, egui::pos2(line.x, max.y)),
        (BeforeAfter::SplitTopBottom, true) => egui::Rect::from_min_max(min, egui::pos2(max.x, line.y)),
        (BeforeAfter::Split, false) => egui::Rect::from_min_max(egui::pos2(line.x, min.y), max),
        (BeforeAfter::SplitTopBottom, false) => egui::Rect::from_min_max(egui::pos2(min.x, line.y), max),
        (BeforeAfter::SideBySide | BeforeAfter::TopBottom, _) => pane,
        _ => canvas,
    };
    clip.intersect(canvas)
}

/// Whether a window waits for the one already being rendered. All windows are cut from the same
/// decoded original; asked for while it isn't held yet (the Before and After together, in either
/// order, or a new window while the first is still decoding), each worker decodes its own copy
/// (two or three full-size decodes and uploads, all but one thrown away). The waiting window
/// follows once the original is in.
pub fn defer_window(window_pending: bool, original_held: bool) -> bool {
    window_pending && !original_held
}

/// Whether the picture on screen has the frame's aspect (to the one pixel a texture's whole-pixel
/// size is off by): a window is a part of the frame, so it can only be placed over a picture that
/// is the frame (an unsupported raw's embedded JPEG may be cropped differently).
pub fn same_aspect(shown: f32, frame: f32) -> bool {
    shown.is_finite() && frame.is_finite() && frame > 0.0 && (shown / frame - 1.0).abs() <= 0.02
}

/// The window of a `full_w × full_h` frame to render so that `visible` — `(x0, y0, x1, y1)` in the
/// frame's pixels, as much of the frame as is on screen — is covered: expanded by a margin,
/// snapped outwards to the [`SNAP`] grid (margin: see [`margin_for`]) and clamped into the frame,
/// at most `max_span` ([`max_span`]) wide or high. `None`: nothing is visible.
pub fn window_for(full_w: usize, full_h: usize, visible: (f32, f32, f32, f32), max_span: usize) -> Option<PixelWindow> {
    if full_w == 0 || full_h == 0 || [visible.0, visible.1, visible.2, visible.3].iter().any(|v| !v.is_finite()) {
        return None;
    }
    let margin = margin_for(full_w.max(full_h));
    let axis = |lo: f32, hi: f32, full: usize| -> Option<(usize, usize)> {
        let (lo, hi) = (lo.max(0.0), hi.min(full as f32));
        if hi <= lo {
            return None;
        }
        let a = (lo as usize).saturating_sub(margin) / SNAP * SNAP;
        let b = ((hi.ceil() as usize).saturating_add(margin)).div_ceil(SNAP).saturating_mul(SNAP).min(full);
        // a window larger than MAX_SPAN keeps the visible part (centred on it), snapped
        if b - a > max_span {
            let mid = ((lo + hi) / 2.0) as usize;
            let a = mid.saturating_sub(max_span / 2) / SNAP * SNAP;
            return Some((a, (a + max_span).min(full)));
        }
        Some((a, b))
    };
    let (x0, x1) = axis(visible.0, visible.2, full_w)?;
    let (y0, y1) = axis(visible.1, visible.3, full_h)?;
    Some(PixelWindow { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

#[cfg(test)]
mod tests {
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
        let v = RegionView {
            photo: PhotoId(1),
            before: false,
            key: 7,
            full: (6000, 4000),
            window: PixelWindow { x: 0, y: 0, w: 256, h: 256 },
            settings: 11,
        };
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
}
