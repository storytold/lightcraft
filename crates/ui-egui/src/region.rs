//! Sharp zoomed views (issue #323). The loupe renders the whole frame in one texture, which can't
//! be as large as a 1:1 or 8:1 view of a big photo. Past that size it also renders just the window
//! of the frame that is on screen, at the zoom scale, and draws it over the whole-frame render.
//!
//! This module is the geometry: when a window is needed and which one. Windows are snapped to a
//! grid and carry a margin, so panning by a few pixels re-uses the render and the spatial stages
//! (clarity, dehaze…) have the context they read around the visible edge.

use dac_engine::pipeline::PixelWindow;

/// Windows start and end on multiples of this many pixels of the zoomed frame.
pub const SNAP: usize = 256;
/// The most context rendered beyond the visible pixels (a multiple of [`SNAP`]).
pub const MAX_MARGIN: usize = 768;
/// The most a window may span on either axis: a bound on its texture whatever the window size.
pub const MAX_SPAN: usize = if cfg!(target_arch = "wasm32") { 3072 } else { 6144 };

/// A zoomed view the loupe asked tiles for: which photo and side, the size of the zoomed frame,
/// the part of it the on-screen tiles cover, and the look they are rendered with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionView {
    pub photo: dac_catalog::PhotoId,
    /// The Before side of a Before/After view (the photo without its edits).
    pub before: bool,
    pub key: u64,
    pub full: (usize, usize),
    pub window: PixelWindow,
    /// The look its tiles are rendered with ([`TileKey::look`]: settings, overlay, proof, quality).
    pub settings: u64,
    /// The side of its tiles ([`tile_side`]).
    pub tile: usize,
}

impl RegionView {
    /// Whether this window's pixels still belong in a loupe showing `photo` with `settings` in a
    /// frame of `full` pixels (they are drawn until the next window arrives, if so).
    ///
    /// While a slider is dragged (`drafting`) a window made for an earlier value of it still
    /// belongs: drafts follow the drag, and waiting for an exact match would show none.
    pub fn is_current(&self, photo: dac_catalog::PhotoId, full: (usize, usize), settings: u64, drafting: bool) -> bool {
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
    /// 1.0, or less while a slider is dragged (a draft of the whole-frame render).
    pub draft_scale: f32,
    /// Whether a window can be drawn over the whole-frame render (not with an overlay, soft
    /// proofing, or a read too far for one window): if not, the whole frame is rendered at the
    /// size it is drawn.
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

/// The loupe's render plan. The cost of a render follows the canvas, not the zoom: the whole-frame
/// render is canvas-sized, and the window holds only the pixels on screen at no more than 100 %.
pub fn plan(settings: &crate::state::AppSettings, v: ViewSizes) -> LoupePlan {
    let canvas_long = if v.canvas_long.is_finite() { v.canvas_long.max(8.0) } else { 8.0 };
    let drawn_long = if v.drawn_long.is_finite() { v.drawn_long.max(8.0) } else { canvas_long };
    let scale = if v.draft_scale.is_finite() { v.draft_scale.clamp(0.1, 1.0) } else { 1.0 };
    if !v.windows {
        // the whole frame at the size it is drawn (an overlay or soft proof has no window); while a
        // slider drags, a canvas-sized draft as with windows: a drag at 1:1 must not render the
        // photo's every pixel each frame
        let long = if scale < 1.0 { drawn_long.min(canvas_long) } else { drawn_long };
        let main_edge = settings.loupe_edge(long * scale, v.native_long, v.texture_side);
        return LoupePlan { main_edge, window_edge: None };
    }
    // a user who chose a size above the preview source level (3840, 5120) means it; otherwise the
    // original is not decoded for the whole-frame render
    let source_cap = if settings.preview_limit == 0 { dac_engine::SourceLevel::Preview.max_edge() } else { settings.preview_limit as usize };
    let edge_at = |scale: f32| settings.loupe_edge(drawn_long.min(canvas_long) * scale, v.native_long, v.texture_side).min(source_cap);
    let main_edge = edge_at(scale);
    let frame_edge = settings.window_frame_edge(drawn_long, v.native_long);
    // a user who capped the preview size has chosen speed over sharpness at fit
    let fit = drawn_long <= canvas_long * 1.05;
    // (against the undrafted whole-frame render: a drag must not switch the window off and on)
    let window_edge = (frame_edge as f32 > edge_at(1.0) as f32 * WINDOW_RATIO && !(fit && settings.preview_limit != 0)).then_some(frame_edge);
    LoupePlan { main_edge, window_edge }
}

/// How long the render sizes stay as they were after a pinch or two-finger scroll stops (s).
pub const HOLD_SECS: f64 = 0.25;

/// Holds the loupe's render sizes while a gesture runs (a continuous pinch changes the zoom every
/// frame; re-planning each time would be a stream of renders of every size in between). The view
/// keeps being drawn from the renders it has, magnified by the GPU, and is re-planned once the
/// gesture has been quiet for [`HOLD_SECS`].
#[derive(Clone, Debug, Default)]
pub struct SizeHold {
    photo: Option<dac_catalog::PhotoId>,
    plan: Option<LoupePlan>,
    until: f64,
}

impl SizeHold {
    /// The plan to render this frame: `fresh` normally; the plan from before the gesture while
    /// one is running (`gesturing`) and for [`HOLD_SECS`] after. `now` is in seconds.
    pub fn apply(&mut self, photo: dac_catalog::PhotoId, now: f64, gesturing: bool, fresh: LoupePlan) -> LoupePlan {
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

/// Where a window of one side of a Before/After view may be drawn: its own pane in side by side
/// and stacked views (a window carries margin beyond what its pane shows, which would otherwise
/// spill into the neighbour), its side of the line in a wipe, else the canvas.
pub fn window_clip(mode: crate::state::BeforeAfter, before_side: bool, pane: egui::Rect, canvas: egui::Rect, img: egui::Rect) -> egui::Rect {
    use crate::state::BeforeAfter;
    let (min, max) = (canvas.min, canvas.max);
    let clip = match (mode, before_side) {
        (BeforeAfter::Split, true) => egui::Rect::from_min_max(min, egui::pos2(img.center().x, max.y)),
        (BeforeAfter::SplitTopBottom, true) => egui::Rect::from_min_max(min, egui::pos2(max.x, img.center().y)),
        (BeforeAfter::Split, false) => egui::Rect::from_min_max(egui::pos2(img.center().x, min.y), max),
        (BeforeAfter::SplitTopBottom, false) => egui::Rect::from_min_max(egui::pos2(min.x, img.center().y), max),
        (BeforeAfter::SideBySide | BeforeAfter::TopBottom, _) => pane,
        _ => canvas,
    };
    clip.intersect(canvas)
}

/// Whether the Before window waits for the After's. Both are cut from the same decoded original;
/// asked for together while it isn't held yet, each worker decodes its own copy (two full-size
/// decodes, two uploads, one thrown away). The Before follows once the original is in.
pub fn defer_before_window(before_side: bool, after_pending: bool, original_held: bool) -> bool {
    before_side && after_pending && !original_held
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

// ---------------------------------------------------------------------------------------------
// The tile grid (P1.6). A zoomed view is cut into fixed tiles of the zoomed frame: each is its own
// render (the tile plus a margin of context, cropped back to the tile) and its own texture, kept
// in a [`TileCache`] by photo, side, frame size, place and look. Panning re-uses every tile already
// rendered and asks only for the ones that come into view; going back to a look (undo, a toggle)
// finds its tiles still there while the memory bound allows.

/// The side of a tile in pixels of the zoomed frame (a multiple of [`SNAP`]): small enough that a
/// pan asks for a strip of tiles, big enough that the margin each one renders stays cheap.
pub const TILE: usize = 1024;

/// The bytes the tile textures may hold together before the least recently drawn go.
pub const TILE_BUDGET: usize = if cfg!(target_arch = "wasm32") { 96 << 20 } else { 384 << 20 };

/// The tile side on a host whose textures are at most `texture_side` px.
pub fn tile_side(texture_side: usize) -> usize {
    TILE.min(max_span(texture_side))
}

/// A tile of a frame: its column and row in the grid of `side`-pixel tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub col: u16,
    pub row: u16,
}

/// The pixels of a `full_w × full_h` frame tile `t` covers (`None`: outside the frame).
pub fn tile_rect(full: (usize, usize), side: usize, t: TileId) -> Option<PixelWindow> {
    let side = side.max(1);
    let x = (t.col as usize).checked_mul(side)?;
    let y = (t.row as usize).checked_mul(side)?;
    if x >= full.0 || y >= full.1 {
        return None;
    }
    Some(PixelWindow { x, y, w: side.min(full.0 - x), h: side.min(full.1 - y) })
}

/// The window rendered for a tile: the tile and the margin of context the spatial stages read
/// around it ([`margin_for`]), inside the frame.
pub fn tile_render_window(full: (usize, usize), tile: PixelWindow) -> PixelWindow {
    let m = margin_for(full.0.max(full.1));
    let x0 = tile.x.saturating_sub(m);
    let y0 = tile.y.saturating_sub(m);
    let x1 = tile.x.saturating_add(tile.w).saturating_add(m).min(full.0);
    let y1 = tile.y.saturating_add(tile.h).saturating_add(m).min(full.1);
    PixelWindow { x: x0, y: y0, w: x1.saturating_sub(x0).max(1), h: y1.saturating_sub(y0).max(1) }
}

/// The most tiles one view asks for in a frame (a bound whatever the zoom and canvas).
pub const MAX_TILES: usize = 64;
/// The most tiles from the centre of the view, on each axis, that are looked at.
const MAX_REACH: usize = 5;

/// The tiles of a `full` frame that `visible` (frame pixels, `(x0, y0, x1, y1)`) touches, then a
/// ring of `ring` tiles around them (asked for at a lower priority, so a pan finds them ready):
/// `(tile, on screen?)`, nearest the centre of the view first, at most [`MAX_TILES`].
pub fn tiles_for(full: (usize, usize), side: usize, visible: (f32, f32, f32, f32), ring: usize) -> Vec<(TileId, bool)> {
    let side = side.max(1);
    if full.0 == 0 || full.1 == 0 || [visible.0, visible.1, visible.2, visible.3].iter().any(|v| !v.is_finite()) {
        return Vec::new();
    }
    let (cols, rows) = (full.0.div_ceil(side).min(u16::MAX as usize), full.1.div_ceil(side).min(u16::MAX as usize));
    let range = |lo: f32, hi: f32, full: usize, n: usize| -> Option<(usize, usize)> {
        let (lo, hi) = (lo.max(0.0), hi.min(full as f32));
        if hi <= lo {
            return None;
        }
        let a = (lo as usize / side).min(n.saturating_sub(1));
        let b = ((hi.ceil() as usize).saturating_sub(1) / side).min(n.saturating_sub(1));
        Some((a, b))
    };
    let Some((c0, c1)) = range(visible.0, visible.2, full.0, cols) else { return Vec::new() };
    let Some((r0, r1)) = range(visible.1, visible.3, full.1, rows) else { return Vec::new() };
    let (cx, cy) = ((visible.0 + visible.2) / 2.0, (visible.1 + visible.3) / 2.0);
    // never more than a bounded block around the centre, whatever the view spans
    let near = |lo: usize, hi: usize, c: f32| {
        let mid = ((c.max(0.0) as usize) / side).clamp(lo, hi);
        (lo.max(mid.saturating_sub(MAX_REACH)), hi.min(mid.saturating_add(MAX_REACH)))
    };
    let (c0, c1) = near(c0, c1, cx);
    let (r0, r1) = near(r0, r1, cy);
    let mut out = Vec::new();
    let (rc0, rc1) = (c0.saturating_sub(ring), (c1 + ring).min(cols.saturating_sub(1)));
    let (rr0, rr1) = (r0.saturating_sub(ring), (r1 + ring).min(rows.saturating_sub(1)));
    for row in rr0..=rr1 {
        for col in rc0..=rc1 {
            let on = (c0..=c1).contains(&col) && (r0..=r1).contains(&row);
            out.push((TileId { col: col as u16, row: row as u16 }, on));
        }
    }
    let dist = |t: &TileId| {
        let x = (t.col as f32 + 0.5) * side as f32 - cx;
        let y = (t.row as f32 + 0.5) * side as f32 - cy;
        x * x + y * y
    };
    out.sort_by(|a, b| b.1.cmp(&a.1).then(dist(&a.0).total_cmp(&dist(&b.0))));
    out.truncate(MAX_TILES);
    out
}

/// What a tile's pixels are: the photo, the pane it is shown in (the loupe's After or Before side,
/// a Compare / Reference photo: [`crate::render::PANE_AFTER`]…), the zoomed frame, the place in
/// it, and the look (develop settings, overlay, proof and quality, hashed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub photo: dac_catalog::PhotoId,
    pub pane: u8,
    pub full: (usize, usize),
    pub tile: TileId,
    pub look: u64,
}

impl TileKey {
    /// The Before side of the loupe's Before/After view.
    pub fn before(&self) -> bool {
        self.pane == crate::render::PANE_BEFORE
    }

    /// The same tile with any look.
    fn same_place(&self, o: &TileKey) -> bool {
        self.photo == o.photo && self.pane == o.pane && self.full == o.full && self.tile == o.tile
    }
}

struct TileEntry<T> {
    value: T,
    bytes: usize,
    used: u64,
    born: u64,
    /// Cut from a 1:1 preview, waiting for its render.
    placeholder: bool,
}

/// Tile textures, least recently drawn out first once they hold more than the budget.
pub struct TileCache<T> {
    map: std::collections::HashMap<TileKey, TileEntry<T>>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl<T> Default for TileCache<T> {
    fn default() -> Self {
        Self::new(TILE_BUDGET)
    }
}

impl<T> TileCache<T> {
    pub fn new(budget: usize) -> Self {
        TileCache { map: Default::default(), bytes: 0, budget, clock: 0 }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Whether the rendered tile `k` is held (a placeholder does not count).
    pub fn has_render(&self, k: &TileKey) -> bool {
        self.map.get(k).is_some_and(|e| !e.placeholder)
    }

    /// Whether anything (render or placeholder) is held for `k`.
    pub fn contains(&self, k: &TileKey) -> bool {
        self.map.contains_key(k)
    }

    /// The pixels to draw for `k`: its own; else, when `any_look` (a slider drag, whose drafts
    /// follow the drag), the newest of this place with another look. Marks it used.
    pub fn draw(&mut self, k: &TileKey, any_look: bool) -> Option<&T> {
        self.clock += 1;
        let found = if self.map.contains_key(k) {
            Some(*k)
        } else if any_look {
            self.map.iter().filter(|(o, _)| o.same_place(k)).max_by_key(|(_, e)| e.born).map(|(o, _)| *o)
        } else {
            None
        };
        let clock = self.clock;
        let e = self.map.get_mut(&found?)?;
        e.used = clock;
        Some(&e.value)
    }

    /// Keep `value` (`bytes` of texture) for `k`; a render replaces a placeholder, never the
    /// other way round. Evicts the least recently drawn beyond the budget (never `k` itself).
    pub fn insert(&mut self, k: TileKey, value: T, bytes: usize, placeholder: bool) {
        if placeholder && self.has_render(&k) {
            return;
        }
        self.clock += 1;
        if let Some(old) = self.map.insert(k, TileEntry { value, bytes, used: self.clock, born: self.clock, placeholder }) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        while self.bytes > self.budget {
            let Some(victim) = self.map.iter().filter(|(o, _)| **o != k).min_by_key(|(_, e)| e.used).map(|(o, _)| *o) else { break };
            if let Some(e) = self.map.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(e.bytes);
            }
        }
    }

    /// Drop the tiles `keep` says no to.
    pub fn retain(&mut self, mut keep: impl FnMut(&TileKey) -> bool) {
        let mut freed = 0usize;
        self.map.retain(|k, e| {
            let k2 = keep(k);
            if !k2 {
                freed = freed.saturating_add(e.bytes);
            }
            k2
        });
        self.bytes = self.bytes.saturating_sub(freed);
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    pub fn keys(&self) -> impl Iterator<Item = &TileKey> {
        self.map.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.map.values().map(|e| &e.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&TileKey, &T)> {
        self.map.iter().map(|(k, e)| (k, &e.value))
    }
}

/// The part `tile` of `img`, a render of `window` (both in frame pixels): `None` when the tile
/// is not inside the render.
pub fn crop_tile(img: &dac_raster::Rgba8, window: PixelWindow, tile: PixelWindow) -> Option<dac_raster::Rgba8> {
    let ox = tile.x.checked_sub(window.x)?;
    let oy = tile.y.checked_sub(window.y)?;
    if ox.checked_add(tile.w)? > img.width || oy.checked_add(tile.h)? > img.height || tile.w == 0 || tile.h == 0 {
        return None;
    }
    let mut data = Vec::with_capacity(tile.w.checked_mul(tile.h)?);
    for y in oy..oy + tile.h {
        let start = y.checked_mul(img.width)?.checked_add(ox)?;
        data.extend_from_slice(img.data.get(start..start.checked_add(tile.w)?)?);
    }
    Some(dac_raster::Rgba8 { width: tile.w, height: tile.h, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppSettings;

    const BIG: usize = 16384;

    fn sizes(drawn: f32, canvas: f32, native: usize) -> ViewSizes {
        ViewSizes { drawn_long: drawn, canvas_long: canvas, native_long: native, texture_side: BIG, draft_scale: 1.0, windows: true }
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
        let drag = plan(&auto(), ViewSizes { draft_scale: 0.6, ..sizes(24000.0, 2800.0, 6000) });
        assert!(drag.main_edge < still.main_edge, "{drag:?} vs {still:?}");
        assert_eq!(drag.window_edge, still.window_edge);
    }

    // Given no window (an overlay, soft proofing), the view at rest renders the whole frame at the
    // size it is drawn, but a slider drag drafts at the canvas size like the windowed path
    #[test]
    fn without_a_window_a_drag_drafts_at_the_canvas_size() {
        let no_window = |draft_scale: f32| ViewSizes { windows: false, draft_scale, ..sizes(24000.0, 2800.0, 6000) };
        let still = plan(&auto(), no_window(1.0));
        let drag = plan(&auto(), no_window(0.6));
        assert_eq!(still.window_edge, None);
        assert!(still.main_edge > 2800, "at rest the zoomed frame: {still:?}");
        assert!(drag.main_edge <= 2800, "a drag drafts at most the canvas: {drag:?}");
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
            let drag = plan(&auto(), ViewSizes { draft_scale: 0.6, ..sizes(drawn, drawn, 6000) });
            assert!(still.window_edge.is_some(), "{drawn}: {still:?}");
            assert_eq!(drag.window_edge, still.window_edge, "{drawn}");
        }
    }

    // Given a mode whose picture can't be a window (a mask overlay, soft proofing, a spot read too
    // far for one render), the whole-frame render is as big as the view, as it was before windows
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
        let p = plan(&auto(), ViewSizes { draft_scale: 0.6, ..sizes(2800.0, 2800.0, 6000) });
        assert_eq!(p.window_edge, None);
    }

    // Given a pinch from fit to 400 %, the sizes of the first frame hold until the gesture is quiet
    #[test]
    fn a_pinch_holds_the_render_sizes() {
        use dac_catalog::PhotoId;
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
        use dac_catalog::PhotoId;
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
        assert_eq!(window_clip(Off, false, canvas, canvas, img), canvas);
        assert_eq!(window_clip(Original, true, canvas, canvas, img), canvas);
    }

    // Given Before and After windows asked for at once at first open, only the After's goes: the
    // Before's waits for the decoded original instead of decoding another copy
    #[test]
    fn the_before_window_waits_for_the_original_the_after_is_decoding() {
        assert!(defer_before_window(true, true, false));
        assert!(!defer_before_window(true, true, true), "the original is in");
        assert!(!defer_before_window(true, false, false), "nothing is decoding it");
        assert!(!defer_before_window(false, true, false), "the After never waits");
    }

    // Hostile numbers give a plan, never a panic
    #[test]
    fn hostile_sizes_give_a_usable_plan() {
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 0.0] {
            let p = plan(&auto(), ViewSizes { drawn_long: v, canvas_long: v, draft_scale: v, ..sizes(1.0, 1.0, 6000) });
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
        assert_eq!(margin_for(48_000), MAX_MARGIN);
        assert_eq!(margin_for(0), SNAP);
        assert!(margin_for(usize::MAX) <= MAX_MARGIN);
        for l in [100, 3000, 8000, 48_000, 400_000] {
            assert_eq!(margin_for(l) % SNAP, 0);
        }
    }

    // Given a tile rendered for other settings, another photo or another zoom, it is not drawn
    #[test]
    fn a_tile_is_drawn_only_while_it_shows_what_the_loupe_shows() {
        use dac_catalog::PhotoId;
        let v = RegionView {
            photo: PhotoId(1),
            before: false,
            key: 7,
            full: (6000, 4000),
            window: PixelWindow { x: 0, y: 0, w: 256, h: 256 },
            settings: 11,
            tile: TILE,
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

    // Given the named zoom levels, each is found, Zoom In / Out walk them in order, and out of
    // 1:4 is Fit
    #[test]
    fn zoom_levels_run_from_one_quarter_to_eleven_to_one() {
        use crate::state::{MAX_ZOOM, ZOOM_LEVELS, Zoom, zoom_level, zoom_step};
        assert_eq!(zoom_level("fit"), Some(Zoom::Fit));
        assert_eq!(zoom_level("fill"), Some(Zoom::Fill));
        assert_eq!(zoom_level("1:1"), Some(Zoom::Percent(100.0)));
        assert_eq!(zoom_level("11:1"), Some(Zoom::Percent(MAX_ZOOM)));
        assert_eq!(zoom_level("5:1"), None);
        let mut up = vec![];
        let mut cur = 20.0;
        while cur < MAX_ZOOM {
            cur = zoom_step(cur, true).unwrap();
            up.push(cur);
        }
        assert_eq!(up, ZOOM_LEVELS.map(|l| l.1).to_vec());
        assert_eq!(zoom_step(MAX_ZOOM, true), Some(MAX_ZOOM), "11:1 is the deepest");
        assert_eq!(zoom_step(100.0 / 3.0, false), Some(25.0), "1:3 out is 1:4");
        assert_eq!(zoom_step(25.0, false), None, "1:4 out is Fit");
    }

    fn key(col: u16, look: u64) -> TileKey {
        TileKey { photo: dac_catalog::PhotoId(1), pane: 0, full: (8256, 5504), tile: TileId { col, row: 0 }, look }
    }

    // Given a 45 MP frame, the tiles cover it exactly: none overlap, none spill past the edge
    #[test]
    fn tiles_cover_the_frame_exactly() {
        let full = (8256, 5504);
        let all = tiles_for(full, TILE, (0.0, 0.0, full.0 as f32, full.1 as f32), 0);
        assert_eq!(all.len(), 9 * 6);
        let area: usize = all.iter().filter_map(|(t, _)| tile_rect(full, TILE, *t)).map(|r| r.w * r.h).sum();
        assert_eq!(area, full.0 * full.1);
        assert_eq!(tile_rect(full, TILE, TileId { col: 8, row: 5 }), Some(PixelWindow { x: 8192, y: 5120, w: 64, h: 384 }));
        assert_eq!(tile_rect(full, TILE, TileId { col: 9, row: 0 }), None);
    }

    // Given a view, its on-screen tiles come first (nearest the centre first), then a ring
    #[test]
    fn on_screen_tiles_first_then_the_ring() {
        let t = tiles_for((8256, 5504), TILE, (2100.0, 2100.0, 2600.0, 2400.0), 1);
        assert_eq!(t[0], (TileId { col: 2, row: 2 }, true));
        assert_eq!(t.iter().filter(|x| x.1).count(), 1);
        assert_eq!(t.len(), 9, "one tile and the eight around it");
        assert!(tiles_for((8256, 5504), TILE, (f32::NAN, 0.0, 1.0, 1.0), 1).is_empty());
        assert!(tiles_for((8256, 5504), TILE, (9000.0, 0.0, 9100.0, 10.0), 1).is_empty(), "off the frame");
        assert!(tiles_for((0, 0), TILE, (0.0, 0.0, 1.0, 1.0), 1).is_empty());
        assert!(tiles_for((1 << 30, 1 << 30), 1, (0.0, 0.0, 1e9, 1e9), 1).len() <= MAX_TILES, "bounded whatever the zoom");
    }

    // Given a tile, it is rendered with the margin of context around it, inside the frame, and
    // cropped back to the tile
    #[test]
    fn a_tile_renders_with_its_margin_and_is_cropped_back() {
        let full = (8256, 5504);
        let tile = tile_rect(full, TILE, TileId { col: 0, row: 1 }).unwrap();
        let win = tile_render_window(full, tile);
        let m = margin_for(8256);
        assert_eq!(win, PixelWindow { x: 0, y: 1024 - m, w: 1024 + m, h: 1024 + 2 * m });
        let img = dac_raster::Rgba8::from_fn(win.w, win.h, |x, y| [(x % 251) as u8, (y % 251) as u8, 0, 255]);
        let part = crop_tile(&img, win, tile).unwrap();
        assert_eq!((part.width, part.height), (1024, 1024));
        assert_eq!(part.data[0], [0, (m % 251) as u8, 0, 255], "the tile's first pixel");
        assert!(crop_tile(&img, PixelWindow { x: 4096, ..win }, tile).is_none(), "not inside the render");
    }

    // Given more tiles than the budget holds, the least recently drawn go and the bytes stay bounded
    #[test]
    fn the_tile_cache_is_bounded_and_least_recently_drawn_go_first() {
        let mut c = TileCache::<u32>::new(3 * 100);
        for i in 0..3 {
            c.insert(key(i, 1), i as u32, 100, false);
        }
        assert!(c.draw(&key(0, 1), false).is_some(), "touch the first");
        c.insert(key(3, 1), 3, 100, false);
        assert_eq!(c.len(), 3);
        assert!(c.bytes() <= 300);
        assert!(c.contains(&key(0, 1)) && !c.contains(&key(1, 1)), "the untouched oldest went");
        c.insert(key(9, 1), 9, 10_000, false);
        assert!(c.contains(&key(9, 1)), "a tile larger than the budget is still kept while drawn");
        assert_eq!(c.len(), 1);
    }

    // Given tiles of two looks, each look finds its own (undo is instant); a drag draws any look
    #[test]
    fn tiles_are_cached_per_look() {
        let mut c = TileCache::<u32>::new(1 << 20);
        c.insert(key(0, 1), 1, 10, false);
        c.insert(key(0, 2), 2, 10, false);
        assert_eq!(c.draw(&key(0, 1), false), Some(&1));
        assert_eq!(c.draw(&key(0, 2), false), Some(&2));
        assert_eq!(c.draw(&key(0, 3), false), None, "another look is not drawn at rest");
        assert_eq!(c.draw(&key(0, 3), true), Some(&2), "a drag draws the newest of the place");
        assert_eq!(c.draw(&key(1, 3), true), None, "never another place");
    }

    // Given a placeholder from a 1:1 preview, its render replaces it, never the other way round
    #[test]
    fn a_render_replaces_a_placeholder_and_not_the_reverse() {
        let mut c = TileCache::<u32>::new(1 << 20);
        c.insert(key(0, 1), 1, 10, true);
        assert!(c.contains(&key(0, 1)) && !c.has_render(&key(0, 1)));
        c.insert(key(0, 1), 2, 10, false);
        assert!(c.has_render(&key(0, 1)));
        c.insert(key(0, 1), 3, 10, true);
        assert_eq!(c.draw(&key(0, 1), false), Some(&2));
        assert_eq!(c.bytes(), 10);
        c.retain(|_| false);
        assert_eq!((c.len(), c.bytes()), (0, 0));
    }
}
