use super::{Color32, LightcraftApp, PhotoId, Rect, Slot, pos2, vec2};

/// Whether an overlay can be rendered on a region tile without changing what the tile means. Mask (one or all)
/// overlays are evaluated from the same mask alpha as the full render; Point Color range and Visualize
/// HDR act pixel by pixel (the HDR rendition is rendered for the same window); Visualize Spots' high
/// pass reads about 3σ = 1.8 % of the long edge, inside the window's margin
/// ([`crate::region::margin_for`]: 4.5 %). Each is tested equal to the whole-frame render's pixels.
pub(crate) fn overlay_supports_window(overlay: lightcraft_pipeline::Overlay) -> bool {
    use lightcraft_pipeline::Overlay as O;
    match overlay {
        O::None | O::Mask { .. } | O::AllMasks { .. } | O::PointColorRange(_) | O::HdrRange | O::Spots(_) => true,
        // clipping and greyscale previews act pixel by pixel on the finished image
        o if o.per_pixel() => true,
        _ => false,
    }
}

/// What a window render of the loupe needs to know about the view it belongs to.
pub(super) struct WindowCtx {
    pub(super) id: PhotoId,
    pub(super) overlay: lightcraft_pipeline::Overlay,
    /// Long edge of the frame the windows are cut from (`None`: no windows).
    pub(super) frame_edge: Option<usize>,
    pub(super) drawn_long: f32,
    pub(super) aspect: f32,
    pub(super) ppp: f32,
    pub(super) texture_side: usize,
    pub(super) crop_tool: bool,
    pub(super) interacting: bool,
    /// Hash of the photo's develop settings.
    pub(super) look: u64,
    /// A pinch or two-finger scroll is running: keep the windows there are.
    pub(super) holding: bool,
}

/// One side of the loupe that gets a window render.
#[derive(Clone, Copy)]
pub(super) struct WindowView {
    pub(super) slot: Slot,
    /// The Before side (the photo without its edits).
    pub(super) before: bool,
    /// The rect the photo is drawn in.
    pub(super) img: Rect,
    /// The rect it is requested for (the drawn rect without a click-zoom animation).
    pub(super) target: Rect,
    /// The area whose pixels are on screen.
    pub(super) visible: Rect,
}

/// Ask for the window of `v` that holds what is on screen; remember it so its texture can be
/// drawn where it belongs. `None`: no window is wanted (or possible) for this view.
pub(super) fn request_window(app: &mut LightcraftApp, c: &WindowCtx, v: &WindowView) -> Option<crate::region::RegionView> {
    let frame_edge = c.frame_edge?;
    // while a pinch or scroll runs the windows there are keep being drawn, magnified: no new one
    // per frame (zooming out would ask for ever wider ones)
    if c.holding {
        return if v.before { app.region_before_view } else { app.region_view }.filter(|w| w.photo == c.id);
    }
    let (fw, fh) =
        if c.aspect >= 1.0 { (frame_edge as f32, frame_edge as f32 / c.aspect) } else { (frame_edge as f32 * c.aspect, frame_edge as f32) };
    let (fw, fh) = (fw.round().max(1.0) as usize, fh.round().max(1.0) as usize);
    // what is on screen, in pixels of the window's frame (the drawn frame, scaled)
    let to_px = c.ppp * frame_edge as f32 / c.drawn_long.max(1.0);
    let visible = (
        to_px * (v.visible.left() - v.target.left()),
        to_px * (v.visible.top() - v.target.top()),
        to_px * (v.visible.right() - v.target.left()),
        to_px * (v.visible.bottom() - v.target.top()),
    );
    let max_span = crate::region::max_span(c.texture_side);
    // panning (the After side, no edit in progress): the window asked for reaches ahead in the
    // direction the view moves, so it still covers the view when it arrives
    let pans = !v.before && !c.interacting;
    let moved = match app.pan_seen {
        Some((p, f, at)) if pans && p == c.id && f == (fw, fh) => (visible.0 - at.0, visible.1 - at.1),
        _ => (0.0, 0.0),
    };
    if !v.before {
        app.pan_seen = Some((c.id, (fw, fh), (visible.0, visible.1)));
    }
    let ask = if moved == (0.0, 0.0) { visible } else { crate::region::lead(visible, moved) };
    // (a lead too wide for one window is cut down around its middle: it must still hold the view)
    let holds_view = |w: &lightcraft_engine::pipeline::PixelWindow| crate::region::covers(std::slice::from_ref(w), clamp_visible(visible, (fw, fh)));
    let win = crate::region::window_for(fw, fh, ask, max_span).filter(holds_view).or_else(|| crate::region::window_for(fw, fh, visible, max_span))?;
    // a window waits for the original another window job is decoding (see `defer_window`)
    let original_held = app.session.media.has_source(c.id, lightcraft_engine::SourceLevel::for_size(fw.max(fh)));
    let window_pending = app.renderer.is_pending(Slot::Region) || app.renderer.is_pending(Slot::RegionBefore);
    if crate::region::defer_window(window_pending, original_held) {
        return if v.before { app.region_before_view } else { app.region_view }.filter(|w| w.photo == c.id);
    }
    let job = if v.before {
        app.session.region_job_before(c.id, fw, fh, win, !c.crop_tool)
    } else {
        app.session.region_job(c.id, fw, fh, win, !c.crop_tool)
    };
    // refused: what the window reads (a spot's source, an Auto Mask stroke) is too far for one
    // render, so the whole frame is rendered at the drawn size instead (see `region::plan`)
    let Some(job) = job else {
        app.window_refused = Some((c.id, c.look, frame_edge));
        return None;
    };
    // a drag renders drafts of the window, as it does of the whole frame. The Before tile stays
    // plain; the After tile carries the same mask overlay as the whole-frame render.
    let job = if c.interacting { job.draft() } else { job };
    let overlay = if v.before { lightcraft_pipeline::Overlay::None } else { c.overlay };
    let job = if v.before { job } else { job.with_hidden_grading(app.ui.grading_hidden_for(c.id)) };
    // the After window is proofed like the whole-frame render under it; the Before side is
    // never proofed (its whole-frame render isn't either)
    let proof = app.ui.soft_proof.then_some(app.ui.proof);
    let job = job.with_overlay(overlay).with_proof(if v.before { None } else { proof });
    // keyed as the renderer keys the texture (a display profile changes the key)
    let job = app.renderer.job_for(v.slot, job);
    // The overlay and the proof are part of the tile's identity too, so changing mask view/colour
    // or the proof never leaves a stale region tile on screen while its replacement renders.
    let proof_key = if v.before { 0 } else { proof.map_or(0, |p| p.key()) };
    let look = job.settings.hash64() ^ overlay.key().rotate_left(17) ^ proof_key.rotate_left(41);
    let view = crate::region::RegionView { photo: c.id, before: v.before, key: job.key, full: (fw, fh), window: win, settings: look };
    // windows of other photos, looks and zooms of this side can't be drawn any more
    app.region_tiles.retain(|(before, _), t| *before != v.before || t.is_current(c.id, (fw, fh), look, c.interacting));
    if !v.before {
        forget_unused_tiles(app);
        let tiles = &app.region_tiles;
        app.renderer.retain_pan_tiles(|t| tiles.contains_key(&(false, t.key)));
    }
    // what is on screen is sharp already, from the windows made so far: nothing to render
    if pans && crate::region::covers(&sharp_windows(app, c, &view), clamp_visible(visible, (fw, fh))) {
        let shown = app.renderer.textures.get(&Slot::Region).and_then(|t| app.region_tiles.get(&(false, t.key))).copied();
        return Some(shown.filter(|t| t.is_current(c.id, view.full, view.settings, false)).unwrap_or(view));
    }
    app.region_tiles.insert((v.before, job.key), view);
    app.renderer.request(v.slot, job, 99);
    Some(view)
}

/// `visible` (frame pixels) inside a `full` frame.
fn clamp_visible(visible: (f32, f32, f32, f32), full: (usize, usize)) -> (f32, f32, f32, f32) {
    let (x0, y0, x1, y1) = visible;
    (x0.max(0.0), y0.max(0.0), x1.min(full.0 as f32), y1.min(full.1 as f32))
}

/// What of the frame `view` is cut from is on screen in `v` (frame pixels, inside the frame).
fn visible_px(c: &WindowCtx, v: &WindowView, view: &crate::region::RegionView) -> (f32, f32, f32, f32) {
    let frame_edge = c.frame_edge.unwrap_or(view.full.0.max(view.full.1));
    let to_px = c.ppp * frame_edge as f32 / c.drawn_long.max(1.0);
    let visible = (
        to_px * (v.visible.left() - v.target.left()),
        to_px * (v.visible.top() - v.target.top()),
        to_px * (v.visible.right() - v.target.left()),
        to_px * (v.visible.bottom() - v.target.top()),
    );
    clamp_visible(visible, view.full)
}

/// Window views past this many are only kept while their render is on screen, kept or on its way
/// (a long pan asks for a new window every few frames).
const TILE_VIEWS: usize = 64;

/// Forget the window views ([`LightcraftApp::region_tiles`]) of the After side whose render is
/// nowhere any more, once there are many.
fn forget_unused_tiles(app: &mut LightcraftApp) {
    if app.region_tiles.len() <= TILE_VIEWS {
        return;
    }
    let r = &app.renderer;
    let shown = r.textures.get(&Slot::Region).map(|t| t.key);
    let used = |key: u64| {
        shown == Some(key) || r.wanted(Slot::Region) == Some(key) || r.wanted(Slot::RegionNear) == Some(key) || r.pan_tiles().any(|t| t.key == key)
    };
    app.region_tiles.retain(|(before, key), _| *before || used(*key));
}

/// The windows (frame pixels) the After side of the zoomed view `view` already shows sharp: the
/// full-quality window on screen, whole, and the [pan tiles](crate::render::Renderer::pan_tiles)
/// of its look, their inner parts ([`crate::region::inner`]).
fn sharp_windows(app: &LightcraftApp, c: &WindowCtx, view: &crate::region::RegionView) -> Vec<lightcraft_engine::pipeline::PixelWindow> {
    let current = |key: u64| {
        app.region_tiles.get(&(false, key)).filter(|t| app.renderer.is_full_window(key) && t.is_current(c.id, view.full, view.settings, false))
    };
    let on_screen = app.renderer.textures.get(&Slot::Region).filter(|t| t.photo == c.id).and_then(|t| current(t.key)).map(|t| t.window);
    let tiles = app.renderer.pan_tiles().filter(|t| t.photo == c.id).filter_map(|t| current(t.key)).map(|t| crate::region::inner(t.window, t.full));
    on_screen.into_iter().chain(tiles).collect()
}

/// Once the zoomed view (its After side, `windows`) is still and sharp: make the windows around it
/// ahead, at low priority and one at a time, nearest first, so a pan finds them sharp. They never
/// hold up the view's own renders (background work, below [`super::PREFETCH_PRIORITY`]).
pub(super) fn prefetch_near(app: &mut LightcraftApp, c: &WindowCtx, windows: &[(WindowView, crate::region::RegionView)]) {
    let Some((v, view)) = windows.iter().find(|(v, _)| !v.before) else { return };
    // (a window made ahead for a view since left: given up, its view is gone from `region_tiles`)
    let tiles = &app.region_tiles;
    app.renderer.drop_near_unless(|key| tiles.contains_key(&(false, key)));
    if c.interacting
        || c.holding
        || app.renderer.is_pending(Slot::Region)
        || app.renderer.is_pending(Slot::RegionNear)
        || app.renderer.pan_tile_count() + 1 >= crate::render::PAN_TILES
    {
        return;
    }
    let (fw, fh) = view.full;
    let visible = visible_px(c, v, view);
    let have = sharp_windows(app, c, view);
    if !crate::region::covers(&have, visible) {
        return;
    }
    let max_span = crate::region::max_span(c.texture_side);
    let proof = app.ui.soft_proof.then_some(app.ui.proof);
    for area in crate::region::around(visible, view.full) {
        if crate::region::covers(&have, area) {
            continue;
        }
        let Some(win) = crate::region::window_for(fw, fh, area, max_span) else { continue };
        // (refused: what that window reads is too far for one render; another may still be made)
        let Some(job) = app.session.region_job(c.id, fw, fh, win, !c.crop_tool) else { continue };
        let job = job.with_hidden_grading(app.ui.grading_hidden_for(c.id)).with_overlay(c.overlay).with_proof(proof);
        let job = app.renderer.job_for(Slot::Region, job);
        let proof_key = proof.map_or(0, |p| p.key());
        let look = job.settings.hash64() ^ c.overlay.key().rotate_left(17) ^ proof_key.rotate_left(41);
        // (another look than the view's: an edit came in between, the view asks again first)
        if look != view.settings {
            return;
        }
        app.region_tiles.insert((false, job.key), crate::region::RegionView { key: job.key, window: win, ..*view });
        app.renderer.prefetch_near(job, super::PREFETCH_PRIORITY - 1);
        return;
    }
}

/// Draw the pan tiles of `v` (the After side, no edit in progress) over the whole-frame render,
/// oldest first, each where it belongs: the sharp pixels made before the view moved on or ahead
/// of it. The rects drawn (screen points).
pub(super) fn draw_pan_tiles(p: &egui::Painter, app: &LightcraftApp, c: &WindowCtx, v: &WindowView, wanted: &crate::region::RegionView) -> Vec<Rect> {
    if v.before || c.interacting {
        return Vec::new();
    }
    let mut out = Vec::new();
    for tex in app.renderer.pan_tiles().filter(|t| t.photo == c.id && app.renderer.is_full_window(t.key)) {
        let Some(tile) = app.region_tiles.get(&(false, tex.key)).filter(|t| t.is_current(c.id, wanted.full, wanted.settings, false)) else {
            continue;
        };
        let (fw, fh) = (tile.full.0 as f32, tile.full.1 as f32);
        let (w, i) = (tile.window, crate::region::inner(tile.window, tile.full));
        let at = v.img.min + vec2(i.x as f32 / fw * v.img.width(), i.y as f32 / fh * v.img.height());
        let dst = Rect::from_min_size(at, vec2(i.w as f32 / fw * v.img.width(), i.h as f32 / fh * v.img.height()));
        let (ww, wh) = (w.w.max(1) as f32, w.h.max(1) as f32);
        let uv = Rect::from_min_max(
            pos2(i.x.saturating_sub(w.x) as f32 / ww, i.y.saturating_sub(w.y) as f32 / wh),
            pos2((i.x + i.w).saturating_sub(w.x) as f32 / ww, (i.y + i.h).saturating_sub(w.y) as f32 / wh),
        );
        p.image(tex.tex.id(), dst, uv, Color32::WHITE);
        out.push(dst.intersect(p.clip_rect()));
    }
    out
}

/// Draw `v`'s window texture over the whole-frame render, if it is the one asked for (or, in a
/// drag, an earlier draft of it). The rect it was drawn in (screen points), if it was.
pub(super) fn draw_window(p: &egui::Painter, app: &LightcraftApp, c: &WindowCtx, v: &WindowView, wanted: &crate::region::RegionView) -> Option<Rect> {
    let tex = app.renderer.textures.get(&v.slot).filter(|t| t.photo == c.id)?;
    let tile = app.region_tiles.get(&(v.before, tex.key)).filter(|t| t.is_current(c.id, wanted.full, wanted.settings, c.interacting))?;
    let (fw, fh) = (tile.full.0 as f32, tile.full.1 as f32);
    let at = v.img.min + vec2(tile.window.x as f32 / fw * v.img.width(), tile.window.y as f32 / fh * v.img.height());
    let dst = Rect::from_min_size(at, vec2(tile.window.w as f32 / fw * v.img.width(), tile.window.h as f32 / fh * v.img.height()));
    p.image(tex.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    Some(dst.intersect(p.clip_rect()))
}
