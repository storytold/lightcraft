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
    fit: Option<[usize; 2]>,
    until: f64,
}

impl SizeHold {
    /// The plan to render this frame: `fresh` normally; the plan from before the gesture while
    /// one is running (`gesturing`) and for [`HOLD_SECS`] after. `now` is in seconds.
    pub fn apply(&mut self, photo: lightcraft_catalog::PhotoId, now: f64, gesturing: bool, fresh: LoupePlan) -> LoupePlan {
        if self.photo != Some(photo) {
            *self = SizeHold { photo: Some(photo), plan: Some(fresh), fit: None, until: f64::NEG_INFINITY };
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

    /// The size the whole frame is shrunk to at rest (the drawn size): the one from before the
    /// gesture while sizes are held (call after [`Self::apply`] in the same frame), else `fresh`.
    pub fn display_fit(&mut self, now: f64, gesturing: bool, fresh: [usize; 2]) -> [usize; 2] {
        match self.fit {
            Some(held) if gesturing || now < self.until => held,
            _ => {
                self.fit = Some(fresh);
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
mod tests;
