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
pub(super) fn request_window(app: &mut LightcraftApp, ctx: &egui::Context, c: &WindowCtx, v: &WindowView) -> Option<crate::region::RegionView> {
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
    let win = crate::region::window_for(fw, fh, visible, crate::region::max_span(c.texture_side))?;
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
    let shown = app.renderer.textures.get(&v.slot).filter(|t| t.photo == c.id).map(|t| t.key);
    let placed =
        app.region_tiles.iter().any(|((b, k), t)| *b == v.before && Some(*k) == shown && t.photo == c.id && t.window == win && t.full == (fw, fh));
    let job = crate::region::first_window(job, shown, placed, c.interacting);
    let view = crate::region::RegionView { photo: c.id, before: v.before, key: job.key, full: (fw, fh), window: win, settings: look };
    // windows of other photos, looks and zooms of this side can't be drawn any more
    app.region_tiles.retain(|(before, key), t| {
        *before != v.before || (t.is_current(c.id, (fw, fh), look, c.interacting) && (Some(*key) == shown || *key == job.key))
    });
    app.region_tiles.insert((v.before, job.key), view);
    app.renderer.request_window(ctx, v.slot, job, 99);
    Some(view)
}

/// Draw `v`'s window texture over the whole-frame render, if it is the one asked for (or, in a
/// drag, an earlier draft of it).
pub(super) fn draw_window(p: &egui::Painter, app: &LightcraftApp, c: &WindowCtx, v: &WindowView, wanted: &crate::region::RegionView) {
    let Some(tex) = app.renderer.textures.get(&v.slot).filter(|t| t.photo == c.id) else { return };
    let Some(tile) = app.region_tiles.get(&(v.before, tex.key)).filter(|t| t.is_current(c.id, wanted.full, wanted.settings, c.interacting)) else {
        return;
    };
    let (fw, fh) = (tile.full.0 as f32, tile.full.1 as f32);
    let at = v.img.min + vec2(tile.window.x as f32 / fw * v.img.width(), tile.window.y as f32 / fh * v.img.height());
    let dst = Rect::from_min_size(at, vec2(tile.window.w as f32 / fw * v.img.width(), tile.window.h as f32 / fh * v.img.height()));
    p.image(tex.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

#[cfg(test)]
mod bounded_window_tests {
    use super::*;
    use crate::{Services, headless::Headless};
    use std::time::Duration;

    fn exact_job(h: &mut Headless, view: crate::region::RegionView, slot: Slot) -> lightcraft_engine::RenderJob {
        let job = if view.before {
            h.app.session.region_job_before(view.photo, view.full.0, view.full.1, view.window, true)
        } else {
            h.app.session.region_job(view.photo, view.full.0, view.full.1, view.window, true)
        }
        .unwrap();
        h.app.renderer.job_for(slot, job)
    }

    #[test]
    fn a_pan_supersedes_an_exact_window_before_it_starts() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = Duration::from_secs(120);
        h.request("ui.set", serde_json::json!({"view":"detail", "right":"none", "filmstrip":false}), timeout);
        h.request("engine.execute", serde_json::json!({"command":"develop.set", "params":{"control":"effects.dehaze", "value":10.0}}), timeout);
        assert!(h.settle(timeout));
        h.app.ui.zoom = crate::state::Zoom::Percent(200.0);
        assert!(h.step_until(timeout, |h| h.app.region_view.is_some()));
        let first = h.app.region_view.unwrap();
        let exact = exact_job(&mut h, first, Slot::Region);
        assert_eq!(first.key, exact.clone().bounded_window().key);
        assert!(h.step_until(timeout, |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == first.key)));
        assert!(!h.app.renderer.is_pending(Slot::Region), "the exact replacement must wait before entering the worker pool");
        assert!(h.busy(), "headless settling must include the deferred replacement");
        h.app.ui.pan = (0.8, 0.5);
        h.step();
        let pan = h.app.region_view.unwrap();
        assert_ne!(pan.window, first.window);
        let next = exact_job(&mut h, pan, Slot::Region);
        assert_eq!(pan.key, next.clone().bounded_window().key);
        assert_eq!(h.app.renderer.wanted(Slot::Region), Some(pan.key), "the new bounded request goes straight to the worker pool");
        assert!(h.settle(timeout));
        assert_eq!(h.app.renderer.textures[&Slot::Region].key, next.key, "the final window still becomes exact");
    }

    #[test]
    fn bounded_windows_finish_after_pan_photo_split_and_nr_drag() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = Duration::from_secs(120);
        h.request("ui.set", serde_json::json!({"view":"detail", "right":"none", "filmstrip":false}), Duration::from_secs(20));
        assert!(h.settle(timeout));
        h.app.ui.zoom = crate::state::Zoom::Percent(200.0);
        h.app.ui.before_after = crate::state::BeforeAfter::Split;
        h.step();
        // A pane can finish its bounded render before the other pane starts.
        // Record each pane's first request before waiting for both.
        let first = std::cell::Cell::new([h.app.region_view, h.app.region_before_view]);
        assert!(h.step_until(timeout, |h| {
            let mut views = first.get();
            for (seen, current) in views.iter_mut().zip([h.app.region_view, h.app.region_before_view]) {
                if seen.is_none() {
                    *seen = current;
                }
            }
            first.set(views);
            views.iter().all(Option::is_some)
        }));
        let [after, before] = first.get();
        for (view, slot) in [(after.unwrap(), Slot::Region), (before.unwrap(), Slot::RegionBefore)] {
            let exact = exact_job(&mut h, view, slot);
            assert_eq!(view.key, exact.clone().bounded_window().key);
            assert!(h.step_until(timeout, |h| h.app.renderer.textures.get(&slot).is_some_and(|t| t.key == exact.key)));
        }
        let old = h.app.region_view.unwrap();
        h.app.ui.pan = (0.8, 0.5);
        assert!(h.step_until(timeout, |h| h.app.region_view.is_some_and(|v| v.window != old.window)));
        let pan = h.app.region_view.unwrap();
        let exact = exact_job(&mut h, pan, Slot::Region);
        assert_eq!(pan.key, exact.clone().bounded_window().key);
        assert!(h.step_until(timeout, |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == exact.key)));
        h.app.session.begin_interaction("Noise reduction").unwrap();
        h.request(
            "engine.execute",
            serde_json::json!({"command":"develop.set", "params":{"control":"detail.nr_luminance", "value":50.0}}),
            Duration::from_secs(20),
        );
        assert!(h.settle(timeout));
        let draft = h.app.region_view.unwrap();
        let exact = exact_job(&mut h, draft, Slot::Region);
        assert_eq!(draft.key, exact.clone().draft().key);
        h.app.session.end_interaction().unwrap();
        assert!(
            h.step_until(timeout, |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == exact.key)),
            "the final draft must be replaced without another edit"
        );
        let other = h.app.session.visible_cloned().iter().find(|id| **id != draft.photo).copied().unwrap();
        h.request(
            "engine.execute",
            serde_json::json!({"command":"library.select", "params":{"ids":[other.0], "active":other.0}}),
            Duration::from_secs(20),
        );
        assert!(h.step_until(timeout, |h| h.app.region_view.is_some_and(|v| v.photo == other)));
        let photo = h.app.region_view.unwrap();
        let exact = exact_job(&mut h, photo, Slot::Region);
        assert_eq!(photo.key, exact.clone().bounded_window().key);
        assert!(h.step_until(timeout, |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.photo == other && t.key == exact.key)));
        assert!(h.settle(timeout));
        if let Some(path) = std::env::var_os("LIGHTCRAFT_REVIEW_SCREENSHOT") {
            let image = h.paint();
            let image =
                lightcraft_raster::Rgba8 { width: image.size[0], height: image.size[1], data: image.pixels.iter().map(|p| p.to_array()).collect() };
            let png =
                lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(path, png).unwrap();
        }
    }

    /// A window not on screen yet is rendered main's way first (bounded margin, Dehaze proxy),
    /// then the exact window replaces it.
    #[test]
    fn a_new_window_is_bounded_first_then_exact() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        assert_eq!(h.request("ui.set", serde_json::json!({"view":"detail", "right":"none", "filmstrip":false}), Duration::from_secs(20))["ok"], true);
        h.settle(Duration::from_secs(120));
        h.app.ui.zoom = crate::state::Zoom::Percent(200.0);
        assert!(h.step_until(Duration::from_secs(120), |h| h.app.region_view.is_some()), "a window is asked for");
        let first = h.app.region_view.unwrap();
        let exact = h
            .app
            .session
            .region_job(first.photo, first.full.0, first.full.1, first.window, true)
            .map(|j| h.app.renderer.job_for(Slot::Region, j))
            .unwrap();
        assert_ne!(first.key, exact.key);
        assert_eq!(first.key, exact.clone().bounded_window().key, "main's window first");
        let exact_key = exact.key;
        assert!(
            h.step_until(Duration::from_secs(120), |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == exact_key)),
            "then the exact one"
        );
    }

    /// With Dehaze on, the exact window after a drag can take ~0.3 s at 67 MP: the released drag
    /// shows main's window first, then the exact one.
    #[test]
    fn a_released_drag_with_dehaze_is_bounded_first_then_exact() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = Duration::from_secs(120);
        let set = |h: &mut Headless, control: &str, value: f64| {
            h.request("engine.execute", serde_json::json!({"command":"develop.set", "params":{"control":control, "value":value}}), timeout);
        };
        h.request("ui.set", serde_json::json!({"view":"detail", "right":"none", "filmstrip":false}), Duration::from_secs(20));
        set(&mut h, "effects.dehaze", 10.0);
        h.app.ui.zoom = crate::state::Zoom::Percent(200.0);
        assert!(h.settle(timeout));
        h.app.session.begin_interaction("Exposure").unwrap();
        set(&mut h, "light.exposure", 0.5);
        assert!(h.settle(timeout));
        let draft = h.app.region_view.unwrap();
        let exact = exact_job(&mut h, draft, Slot::Region);
        assert_eq!(draft.key, exact.clone().draft().key);
        h.app.session.end_interaction().unwrap();
        assert!(h.step_until(timeout, |h| h.app.region_view.is_some_and(|v| v.key != draft.key)));
        assert_eq!(h.app.region_view.unwrap().key, exact.clone().bounded_window().key, "main's window first");
        assert!(h.step_until(timeout, |h| h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == exact.key)), "then the exact one");
    }
}
