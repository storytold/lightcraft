//! The Detail (loupe) view: the developed photo, before/after, zoom & pan, the filmstrip, and the
//! on-canvas tools (crop, brush/gradient masks, remove spots, white-balance picker).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{DevelopSettings, MaskShape};
use lightcraft_geom::{Affine, Point};
use lightcraft_pipeline::geometry::Frame;
use serde_json::json;

use crate::LightcraftApp;
use crate::render::Slot;
use crate::state::{BeforeAfter, RightPanel, ToolOverlay, Zoom};
use crate::theme::Tokens;
use crate::widgets::{register, register_geometry};

mod crop;
mod filmstrip;
mod geometry;
mod info;
mod masks;
mod retouch;
mod window;

pub(crate) use crop::crop_angle_label;
use crop::crop_overlay;
#[cfg(test)]
pub(crate) use crop::readout_rect;
#[cfg(test)]
use crop::{flipped_corners, outside_crop_mesh, rotate_cursor_turn};
#[cfg(test)]
use filmstrip::film_label;
#[cfg(test)]
pub(crate) use filmstrip::film_spoken;
pub(crate) use filmstrip::filmstrip;
use filmstrip::navigator;
pub use geometry::CanvasMap;
#[cfg(test)]
pub(crate) use geometry::focus_box;
use geometry::{animated_rect, middle_pan};
pub(crate) use geometry::{fit_rect, fit_texture_rect, focus_pan, navigate_gesture, output_px, pan_image, snap_fit_rect, texture_side};
#[cfg(test)]
use info::RENDERING_AFTER_SECS;
use info::{
    FILTER_PILL_HEIGHT, FILTER_PILL_INSET, brief_info, filter_pill, filter_pill_shown, info_overlay, info_overlay_extent, label_badge_rect,
    preview_only_pill, quick_name,
};
pub(crate) use info::{draw_focus_point, failed_pill, loupe_pill, rendering_overdue};
use masks::{RegionEdit, color_range_pick, mask_overlay, region_overlay};
use retouch::{eye_overlay, remove_overlay};
pub(crate) use window::overlay_supports_window;
use window::{WindowCtx, WindowView, draw_window, request_window};

/// An in-progress on-canvas gesture.
#[derive(Clone, Debug)]
pub enum Gesture {
    Brush {
        points: Vec<Point>,
        /// Pen pressure (0..1) at each point; empty when no pen pressure was reported (a mouse) or the
        /// brush's Pen Pressure switch is off.
        pressure: Vec<f32>,
    },
    Spot {
        points: Vec<Point>,
    },
    /// Remove tool: dragging spot `spot`'s target (or its source).
    SpotMove {
        spot: usize,
        source: bool,
    },
    CropHandle {
        handle: u8,
        start: lightcraft_geom::Rect,
        angle: f64,
    },
    /// Straighten tool: a line drawn along something that should be level (or plumb).
    StraightenLine {
        a: Pos2,
    },
    CropRotate {
        start_angle: f64,
        a0: f32,
    },
    Pan {
        start: (f32, f32),
        at: Pos2,
    },
    /// Dragging component `comp` of `mask`: its pin (`handle` 0) or a linear gradient's start (1)
    /// or end (2).
    MaskHandle {
        mask: u32,
        comp: usize,
        handle: u8,
        original_shape: MaskShape,
    },
    /// Guided Upright: a guide being drawn from `a` (normalized transformed coordinates).
    Guide {
        a: Point,
    },
    /// Guided Upright: dragging the endpoint `end` (0 = `guide.0`, 1 = `guide.1`) of guide `i`,
    /// from where the pointer went down (`from` — egui forgets the press origin at the release).
    GuideEndpoint {
        i: usize,
        end: usize,
        from: Pos2,
    },
    /// Red Eye: an ellipse being dragged from corner `a` (normalized).
    Eye {
        a: Point,
    },
    /// Targeted adjustment drag from `at` (normalized); `acc` = vertical travel not yet applied.
    Targeted {
        at: Point,
        acc: f32,
    },
    /// A drag of one of the histogram's tonal zones (see [`super::histogram`]): the control the
    /// press locked and the unrounded value its sideways travel has reached. It holds no canvas
    /// geometry: the zone stays locked wherever the pointer goes.
    HistogramDrag {
        control: &'static str,
        value: f64,
    },
}

/// Draw the Develop loupe: the optional filmstrip, the fitted or zoomed photo (before/after and
/// full-screen aware), crop overlays and the render requests for the current view, or, when no
/// photo is active, the empty-library message if the library is empty and "No photo selected"
/// otherwise.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.max_rect();
    let fullscreen = app.ui.fullscreen;
    let show_film = app.ui.filmstrip && !fullscreen;
    let film_h = if show_film { t.film_h } else { 0.0 };
    let canvas = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - film_h));
    app.canvas_rect = Some(canvas);
    if show_film {
        filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), canvas.bottom()), full.max));
    }
    let Some(id) = app.session.active() else {
        if super::empty_library::shown(&app.session) {
            super::empty_library::show(app, ui, canvas);
        } else {
            super::empty_message(ui, canvas, "No photo selected", "Choose a photo in the grid or filmstrip");
        }
        return;
    };
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    brief_info(app, ui.ctx(), id);
    let d = (*photo.develop).clone();
    // the full-screen preview shows the photo only: no tool overlays
    let right = if fullscreen { RightPanel::None } else { app.ui.right };
    let crop_tool = right == RightPanel::Crop;
    let frame = Frame::with_lens(photo.width.max(1) as usize, photo.height.max(1) as usize, &d, !crop_tool, photo.embedded_lens.as_ref());
    let aspect = frame.aspect() as f32;
    let ppp = ui.ctx().pixels_per_point();
    let area = canvas.shrink(if fullscreen {
        0.0
    } else if crop_tool {
        48.0
    } else {
        24.0
    });
    let native = output_px(&frame);
    let texture_side = texture_side(ui.ctx());
    // two views (before, after): side by side or stacked
    let split = matches!(app.ui.before_after, BeforeAfter::SideBySide | BeforeAfter::TopBottom);
    let split_view = matches!(app.ui.before_after, BeforeAfter::Split | BeforeAfter::SplitTopBottom);
    let areas: Vec<Rect> = match app.ui.before_after {
        BeforeAfter::SideBySide => {
            let half = area.width() / 2.0 - 6.0;
            vec![
                Rect::from_min_size(area.min, vec2(half, area.height())),
                Rect::from_min_size(pos2(area.center().x + 6.0, area.top()), vec2(half, area.height())),
            ]
        }
        BeforeAfter::TopBottom => {
            let half = area.height() / 2.0 - 14.0;
            vec![
                Rect::from_min_size(area.min, vec2(area.width(), half)),
                Rect::from_min_size(pos2(area.left(), area.center().y + 14.0), vec2(area.width(), half)),
            ]
        }
        _ => vec![area],
    };
    let main_area = *areas.last().unwrap_or(&area);
    let visible_texture_aspect = app
        .renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .or_else(|| app.renderer.textures.get(&Slot::Preview).filter(|t| t.photo == id))
        .or_else(|| app.renderer.thumb(id))
        .map(|tex| tex.size[0].max(1) as f32 / tex.size[1].max(1) as f32);
    // Follow the texture only where it really differs (an unsupported raw's embedded JPEG with
    // another crop): a texture's whole-pixel size is a little off the exact aspect, and the crop
    // tool's overlays need the frame's own geometry.
    let display_aspect = visible_texture_aspect.filter(|a| !crop_tool && (a / aspect - 1.0).abs() > 0.02).unwrap_or(aspect);
    let resp = ui.interact(canvas, egui::Id::unique("loupe"), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Photo"));
    let mut target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
    let mut img_rect = animated_rect(ui.ctx(), &mut app.ui.zoom_anim, target_rect);
    let in_pane = |pane: Rect, image: Rect| image.translate(pane.center() - main_area.center());
    let mut navigating = false;
    for &view_area in &areas {
        let view_img = in_pane(view_area, img_rect);
        if navigate_gesture(app, ui, &resp, view_area, view_img, native) || middle_pan(app, ui, &resp, view_area, view_img) {
            navigating = true;
            target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
            img_rect = target_rect;
            break;
        }
    }
    let before_img = in_pane(areas[0], img_rect);
    let before_target = in_pane(areas[0], target_rect);
    app.image_rect = Some(img_rect);
    // request renders: the loupe at display resolution (drafts during drags)
    let interacting = app.session.interaction.is_some();
    let overlay = view_overlay(app, &d);
    // a drag drafts at full size where the GPU draws the view (by the GPU's own rule, `handles`;
    // `ready` first: `available` would wait for a device still being created)
    let gpu = interacting
        && lightcraft_engine::gpu::handles(
            lightcraft_pipeline::OutputDepth::U8,
            app.ui.soft_proof && lightcraft_pipeline::proof_profile(app.ui.proof.profile).is_some(),
            overlay,
        )
        && lightcraft_engine::gpu::ready()
        && lightcraft_engine::gpu::available();
    let shown = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id && t.quick.is_none()).map(|t| (t.key, t.ms));
    let drag = app.session.interaction.as_ref().map(|i| (i.photo, i.label.as_str()));
    let now = ui.input(|i| i.time);
    let draft = app.drag_scale.scale(drag, gpu, shown.map(|(key, _)| key), now);
    // render at the final size: a click-zoom animation only changes how the result is drawn
    let native_long = native[0].max(native[1]);
    let drawn_long = target_rect.width().max(target_rect.height()) * ppp;
    // the whole frame at about canvas size; the zoomed part of it is a window render (below)
    let look = d.hash64();
    // Diagnostic views that need whole-frame context, or reads too far for one window, still
    // render the whole frame at the size it is drawn. Mask overlays and soft proofing (both act
    // pixel by pixel) are window-safe.
    let sizes = crate::region::ViewSizes {
        drawn_long,
        canvas_long: canvas.width().max(canvas.height()) * ppp,
        native_long,
        texture_side,
        draft,
        windows: overlay_supports_window(overlay),
    };
    let mut plan = crate::region::plan(&app.ui.settings, sizes);
    if plan.window_edge.is_some_and(|edge| app.window_refused == Some((id, look, edge))) {
        plan = crate::region::plan(&app.ui.settings, crate::region::ViewSizes { windows: false, ..sizes });
    }
    // time the full-size drafts where they are what shows: under a window the whole-frame draft
    // is small anyway, and the frame's time is the window's upload
    if let Some((key, ms)) = shown.filter(|_| plan.window_edge.is_none()) {
        app.drag_scale.observe(key, ms, app.perf.host_frame_ms);
    }
    // a pinch changes the zoom every frame: keep the sizes of before it until it is quiet
    let plan = app.size_hold.apply(id, now, navigating, plan);
    // the drawn size in pixels, clamped first: a NaN or negative float must not become a size
    let drawn_px = |points: f32| (points * ppp).round().clamp(0.0, 65_536.0) as usize;
    let fit_px = app.size_hold.display_fit(now, navigating, [drawn_px(target_rect.width()), drawn_px(target_rect.height())]);
    if app.size_hold.holding(now) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::region::HOLD_SECS));
    }
    // At rest the whole frame renders at no less than the preview source level: the source is then
    // copied 1:1 (no resample in the pipeline, whose prefilter and bilinear pass soften detail near
    // the output's limit) and shrunk once, by the Lanczos display fit below.
    // (zoomed in under a window render, the whole frame is hidden: it stays canvas-sized)
    let want = if interacting || plan.window_edge.is_some() {
        plan.main_edge
    } else {
        // (a user's explicit Preview size below the preview level still caps it)
        let limit = if app.ui.settings.preview_limit == 0 { usize::MAX } else { app.ui.settings.preview_limit as usize };
        let floor = lightcraft_engine::SourceLevel::Preview.max_edge().min(limit).min(native_long);
        plan.main_edge.max(floor).min(texture_side.max(crate::state::MIN_TEXTURE_SIDE))
    };
    // The short side is rounded, not truncated: a truncated box (2560 x 1706 for a 2560 x 1707
    // source) made the render a slightly anisotropic resample of its source, a 2-tap blur along
    // one axis only. A box that matches the source to rounding is rendered at exactly its size.
    let short = |e: usize, a: f32| ((e as f32 / a).round() as usize).max(1);
    let (rw, rh) = if aspect >= 1.0 { (want, short(want, aspect)) } else { (short(want, 1.0 / aspect), want) };
    // hover and before renders are stand-ins: the preview size is plenty and keeps them cheap
    let se = app.ui.settings.stand_in_edge(want, texture_side);
    let (sw, sh) = if aspect >= 1.0 { (se, short(se, aspect)) } else { (short(se, 1.0 / aspect), se) };
    if let Some(job) = app.session.loupe_job(id, rw.max(8), rh.max(8), !crop_tool) {
        let job = if interacting { job.draft() } else { job };
        // at rest the render is shrunk to exactly the size it is drawn at with a Lanczos filter on
        // the worker, so the window draws it texel for texel instead of shrinking it with 2-tap
        // sampling (the render is up to a 64 px step larger, or the preview level)
        let job = if interacting { job } else { job.with_display_fit(fit_px[0], fit_px[1]) };
        let job = job.with_overlay(overlay).with_proof(app.ui.soft_proof.then_some(app.ui.proof));
        let job = job.with_hidden_grading(app.ui.grading_hidden_for(id));
        app.renderer.request(Slot::Main, job, 100);
        // a drag's draft that renders in a few ms (GPU) is drawn in this frame, not the next (zoomed
        // in, what changes on screen is the window's draft)
        #[cfg(not(target_arch = "wasm32"))]
        if interacting && plan.window_edge.is_none() {
            let frame = ui.input(|i| i.predicted_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
            // (a frame time that is not a number waits for nothing)
            let budget = std::time::Duration::try_from_secs_f32(frame * 0.4).unwrap_or_default();
            app.renderer.wait_for_draft(ui.ctx(), &mut app.session, budget);
        }
    }
    // hovering a preset or profile: the photo with that look, shown instead of the loupe render
    // once it is ready (nothing is committed)
    let hover_key = match app.hover_preview.clone() {
        Some(h) if !interacting => app.session.preview_job(id, sw.max(8), sh.max(8), !crop_tool, &h.settings).map(|job| {
            // (the same display fit as the After view, drawn texel for texel like it)
            let job = job.with_display_fit(fit_px[0], fit_px[1]);
            let key = job.key;
            app.renderer.request(Slot::Hover, job, 105);
            (key, h.label)
        }),
        _ => None,
    };
    // once this photo is on screen: prepare its neighbours in filmstrip order (source decoded and
    // kept, view render cached) so stepping to them is instant
    if !interacting && !app.renderer.is_pending(Slot::Main) && app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.photo == id) {
        let ids = app.session.visible_cloned();
        if let Some(i) = ids.iter().position(|p| *p == id) {
            let next = ids.get(i + 1).copied();
            let prev = i.checked_sub(1).and_then(|j| ids.get(j)).copied();
            for (n, nid) in [next, prev].into_iter().enumerate() {
                let Some(nid) = nid else { continue };
                let Some(np) = app.session.catalog.photo(nid).cloned() else { continue };
                let nf = Frame::with_lens(np.width.max(1) as usize, np.height.max(1) as usize, &np.develop, !crop_tool, np.embedded_lens.as_ref());
                let na = nf.aspect() as f32;
                let nr = fit_rect(main_area, na, app.ui.zoom, output_px(&nf), ppp, app.ui.pan);
                let nw =
                    app.ui.settings.prefetch_edge(nr.width().max(nr.height()) * ppp, output_px(&nf).into_iter().max().unwrap_or(1), texture_side);
                let (w, h) = if na >= 1.0 { (nw, (nw as f32 / na) as usize) } else { ((nw as f32 * na) as usize, nw) };
                if let Some(job) = app.session.loupe_job(nid, w.max(8), h.max(8), !crop_tool) {
                    app.renderer.prefetch(Slot::Prefetch(n as u8), job, PREFETCH_PRIORITY);
                }
            }
        }
    }
    // until the loupe has this photo: show its cached render / embedded preview / a thumbnail
    if app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        && let Some(q) = app.session.quick_view_job(id, want.max(8), !crop_tool)
    {
        app.renderer.request_quick(Slot::Preview, q, 110);
    }
    let show_before = app.ui.before_after == BeforeAfter::Original || ui.input(|i| i.key_down(egui::Key::Backslash));
    if (split || show_before || split_view)
        && let Some(job) = app.session.render_job(id, sw.max(8), sh.max(8), true, !crop_tool)
    {
        // (the same display fit as the After view, so both panes are the same texture size)
        let job = if interacting { job } else { job.with_display_fit(fit_px[0], fit_px[1]) };
        app.renderer.request(Slot::Before, job, 90);
    }
    // zoomed past what the whole-frame render holds: also render the part on screen, at no more
    // than 100 % (the GPU magnifies beyond that), for each side that is shown. Mask overlays and
    // soft proofing use the same window; diagnostic overlays that need whole-frame context do not.
    let windowable_view = hover_key.is_none() && overlay_supports_window(overlay) && crate::region::same_aspect(display_aspect, aspect);
    let window_ctx = WindowCtx {
        id,
        overlay,
        frame_edge: plan.window_edge.filter(|_| windowable_view),
        drawn_long,
        aspect,
        ppp,
        texture_side,
        crop_tool,
        interacting,
        look,
        holding: navigating || app.size_hold.holding(now),
    };
    // (slot, before side?, rect to draw in, rect it is requested for, area whose pixels show)
    let mut window_views: Vec<WindowView> = Vec::new();
    if window_ctx.frame_edge.is_some() {
        let after = WindowView { slot: Slot::Region, before: false, img: img_rect, target: target_rect, visible: main_area };
        if split {
            window_views.push(WindowView { slot: Slot::RegionBefore, before: true, img: before_img, target: before_target, visible: areas[0] });
            window_views.push(after);
        } else if show_before {
            window_views.push(WindowView { slot: Slot::RegionBefore, before: true, img: img_rect, target: target_rect, visible: canvas });
        } else if split_view {
            window_views.push(WindowView { slot: Slot::RegionBefore, before: true, img: img_rect, target: target_rect, visible: canvas });
            window_views.push(WindowView { visible: canvas, ..after });
        } else {
            window_views.push(WindowView { visible: canvas, ..after });
        }
    }
    let mut windows: Vec<(WindowView, crate::region::RegionView)> = Vec::new();
    for v in window_views {
        if let Some(view) = request_window(app, &window_ctx, &v) {
            windows.push((v, view));
        }
    }
    for (slot, before) in [(Slot::Region, false), (Slot::RegionBefore, true)] {
        let shown = windows.iter().find(|(v, _)| v.before == before).map(|(_, view)| *view);
        if before {
            app.region_before_view = shown;
        } else {
            app.region_view = shown;
        }
        if shown.is_none() {
            app.region_tiles.retain(|(b, _), _| *b != before);
            app.renderer.release(slot);
        }
    }
    let p = ui.painter_at(canvas);
    // what a view slot shows: its own render of this photo, else the stand-ins (no blank frame
    // between photos, and the full render replaces them in place)
    let draw = |slot: Slot, r: Rect, clip: Rect| -> &'static str {
        let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
        let (tex, what) = if let Some(t) = mine(slot) {
            (t, "render")
        } else if let Some(t) = mine(Slot::Preview) {
            (t, t.quick.map(quick_name).unwrap_or("preview"))
        } else if let Some(t) = app.renderer.thumb(id) {
            (t, "thumb")
        } else {
            return "none";
        };
        let image_rect = snap_fit_rect(fit_texture_rect(r, tex.size), tex.size, ppp);
        p.with_clip_rect(clip).image(tex.tex.id(), image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        what
    };
    // soft proofing: a paper-white surround and the proof's name, as Lightroom shows it
    if app.ui.soft_proof && !fullscreen {
        p.rect_filled(canvas, 0.0, Color32::from_gray(238));
        let label = crate::i18n::tr_format!("Proof Preview · {}", app.ui.proof.space.label());
        p.text(pos2(canvas.right() - 16.0, canvas.top() + 14.0), Align2::RIGHT_CENTER, label, t.font(12.5), Color32::from_gray(60));
    }
    let shown;
    // the Before label's pill, painted once the window renders are down (they would cover it)
    let mut before_badge: Option<(Rect, std::sync::Arc<egui::Galley>)> = None;
    // below what else lives in the canvas' top-left corner: the filter pill, the info overlay
    let pill_inset = if !fullscreen && filter_pill_shown(app) { FILTER_PILL_INSET + FILTER_PILL_HEIGHT } else { 0.0 };
    let badge_top_inset = pill_inset.max(info_overlay_extent(app, &p, &photo));
    if split {
        // each view stays in its pane: zoomed in, a photo reaches past it (and would cover the
        // other view's margins with its whole-frame render, magnified and blurred)
        let br = before_img;
        draw(Slot::Before, br, areas[0]);
        shown = draw(Slot::Main, img_rect, main_area);
        // the labels under each photo, or under its pane when the photo reaches past it
        let label_at = |r: Rect, pane: Rect| pos2(r.left().max(pane.left()), r.bottom().min(pane.bottom()) + 14.0);
        p.text(label_at(br, areas[0]), Align2::LEFT_CENTER, crate::i18n::tr("Before"), t.font(12.0), t.text_dim);
        p.text(label_at(img_rect, main_area), Align2::LEFT_CENTER, crate::i18n::tr("After"), t.font(12.0), t.text_dim);
    } else if show_before {
        shown = draw(Slot::Before, img_rect, canvas);
        // on the photo itself, so it gets the hover label's dark pill to stay readable over light
        // pictures; painted after the window renders below, which would cover it zoomed in
        let g = p.layout_no_wrap(crate::i18n::tr("Before").to_string(), t.font(12.0), Color32::WHITE);
        before_badge = Some((label_badge_rect(img_rect, canvas, g.size(), badge_top_inset), g));
    } else if let Some((_key, label)) = &hover_key
        && let Some(tex) = app.renderer.textures.get(&Slot::Hover).filter(|t| t.photo == id)
    {
        let hover_rect = fit_texture_rect(img_rect, tex.size);
        p.image(tex.tex.id(), hover_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let g = p.layout_no_wrap(label.clone(), t.font(12.0), Color32::WHITE);
        let bg = label_badge_rect(hover_rect, canvas, g.size(), badge_top_inset);
        p.rect_filled(bg, 4.0, Color32::from_black_alpha(160));
        p.galley(bg.min + vec2(8.0, 4.0), g, Color32::WHITE);
        shown = "hover";
    } else {
        shown = draw(Slot::Main, img_rect, canvas);
        if shown == "none" {
            match app.renderer.failure_of(Slot::Main, id) {
                Some(e) => {
                    p.text(
                        canvas.center() - vec2(0.0, 10.0),
                        Align2::CENTER_CENTER,
                        crate::i18n::tr("This photo can't be opened"),
                        t.semibold(14.0),
                        t.text,
                    );
                    p.text(canvas.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, e, t.font(12.0), t.text_dim);
                }
                None if app.ui.settings.loupe_messages => {
                    p.text(canvas.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(13.0), t.text_dim);
                }
                None => {}
            }
        }
    }
    app.loupe_shown = Some((id, shown));
    // a wipe shows the Before whole-frame render on its side of the line…
    let line = crate::region::wipe_line(img_rect, canvas);
    if app.ui.before_after == BeforeAfter::Split
        && let Some(tex) = app.renderer.textures.get(&Slot::Before).filter(|t| t.photo == id)
    {
        let before_rect = fit_texture_rect(img_rect, tex.size);
        let u = ((line.x - before_rect.left()) / before_rect.width().max(1.0)).clamp(0.0, 1.0);
        let left = Rect::from_min_max(before_rect.min, pos2(before_rect.left() + u * before_rect.width(), before_rect.bottom()));
        p.image(tex.tex.id(), left, Rect::from_min_max(pos2(0.0, 0.0), pos2(u, 1.0)), Color32::WHITE);
    }
    if app.ui.before_after == BeforeAfter::SplitTopBottom
        && let Some(tex) = app.renderer.textures.get(&Slot::Before).filter(|t| t.photo == id)
    {
        let before_rect = fit_texture_rect(img_rect, tex.size);
        let v = ((line.y - before_rect.top()) / before_rect.height().max(1.0)).clamp(0.0, 1.0);
        let top = Rect::from_min_max(before_rect.min, pos2(before_rect.right(), before_rect.top() + v * before_rect.height()));
        p.image(tex.tex.id(), top, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, v)), Color32::WHITE);
    }
    // …and the window renders go over the whole-frame ones, all of them, where they belong in the
    // frame (clipped to their pane, or their side of the line)
    for (v, view) in &windows {
        // (holding `\` in a wipe shows the Before everywhere: it is the Before view alone then)
        let mode = if show_before { BeforeAfter::Original } else { app.ui.before_after };
        let clip = crate::region::window_clip(mode, v.before, v.visible, canvas, img_rect);
        draw_window(&p.with_clip_rect(clip), app, &window_ctx, v, view);
    }
    if let Some((bg, g)) = before_badge {
        p.rect_filled(bg, 4.0, Color32::from_black_alpha(160));
        p.galley(bg.min + vec2(8.0, 4.0), g, Color32::WHITE);
        register(ui, "canvas:beforeLabel", bg);
    }
    // the line of a wipe over both
    if app.ui.before_after == BeforeAfter::Split {
        p.line_segment([pos2(line.x, img_rect.top()), pos2(line.x, img_rect.bottom())], Stroke::new(1.5, Color32::WHITE));
    }
    if app.ui.before_after == BeforeAfter::SplitTopBottom {
        p.line_segment([pos2(img_rect.left(), line.y), pos2(img_rect.right(), line.y)], Stroke::new(1.5, Color32::WHITE));
    }
    if app.ui.show_clipping && !show_before && !fullscreen {
        clipping_overlay(app, &p, img_rect);
    }
    // View ▸ Loupe Overlay ▸ Layout Image, over the photo (the After view in a before/after)
    super::layout_overlay::draw(app, &p, img_rect);
    register(ui, "canvas:image", img_rect);
    let map = CanvasMap::new(&frame, img_rect);
    info_overlay(app, &p, canvas, &photo);
    if !fullscreen {
        filter_pill(app, ui, canvas);
    }
    if app.ui.face_boxes {
        // a name box left open on another photo is dropped
        if app.ui.name_edit.as_ref().is_some_and(|e| e.photo != id.0) {
            app.ui.name_edit = None;
        }
        let hints = super::faces::hints_for(app, id.0);
        let people = if app.ui.name_edit.is_some() { app.caches.person_names(&app.session.catalog) } else { Default::default() };
        // while the name box is open and recognition is not set up, the box offers the next step
        let setup = if app.ui.name_edit.is_some() { super::faces::setup(app, ui.ctx()) } else { super::faces::Setup::Running };
        match region_overlay(ui, &p, &map, &photo, d.orientation, hints.as_deref(), &people, &mut app.ui.name_edit, setup) {
            Some(RegionEdit::SetUp) => {
                app.ui.name_edit = None;
                super::faces::take_step(app, ui.ctx(), setup);
            }
            Some(RegionEdit::Remove(index)) => {
                let _ = app.run("photo.removeRegion", json!({"id": id.0, "index": index}));
            }
            Some(RegionEdit::Name(index, name)) => {
                app.ui.name_edit = None;
                let _ = app.run("faces.setName", json!({"id": id.0, "index": index, "name": name}));
            }
            Some(RegionEdit::Resize(index, r)) => {
                let _ = app.run("photo.setRegion", json!({"id": id.0, "index": index, "rect": {"x0": r.x0, "y0": r.y0, "x1": r.x1, "y1": r.y1}}));
            }
            None => {}
        }
    }
    if app.ui.settings.focus_point && !crop_tool {
        draw_focus_point(&p.with_clip_rect(img_rect.intersect(canvas)), img_rect, &photo, "canvas:focusPoint");
    }
    // a fine grid while a transform (geometry) slider is dragged, to judge verticals
    if app.ui.dragging_control.as_deref().is_some_and(|c| c.starts_with("geometry.")) {
        let n = 12;
        let stroke = Stroke::new(1.0, Color32::from_white_alpha(70));
        for i in 1..n {
            let fx = img_rect.left() + img_rect.width() * i as f32 / n as f32;
            p.line_segment([pos2(fx, img_rect.top()), pos2(fx, img_rect.bottom())], stroke);
        }
        let rows = ((n as f32) * img_rect.height() / img_rect.width()).round().max(2.0) as usize;
        for i in 1..rows {
            let fy = img_rect.top() + img_rect.height() * i as f32 / rows as f32;
            p.line_segment([pos2(img_rect.left(), fy), pos2(img_rect.right(), fy)], stroke);
        }
    }
    // Cursor selection must also run on gesture frames, when tool input is suppressed.
    if !matches!(right, RightPanel::Crop | RightPanel::Masking | RightPanel::Remove | RightPanel::RedEye) {
        general_cursor(app, ui, &resp, img_rect, main_area);
    }
    if !navigating {
        match right {
            RightPanel::Crop => crop_overlay(app, ui, &resp, &map, &frame, &d, id),
            RightPanel::Masking => mask_overlay(app, ui, &resp, &map, &d),
            RightPanel::Remove => remove_overlay(app, ui, &resp, &map, &d),
            RightPanel::RedEye => eye_overlay(app, ui, &resp, &map, &d),
            _ => {
                // side by side / stacked: Before shows the same frame, so a click in its view (white
                // balance, colour samples, targeted adjustment) means the same point of the photo.
                // Zoomed in, a view's image reaches past its pane: what counts is the pane clicked.
                // A click-zoom there centres both views on the point clicked (#1742).
                let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
                let (map, img, pane) = match (split, areas.first(), pointer) {
                    (true, Some(before), Some(q)) if before.contains(q) => (CanvasMap::new(&frame, before_img), before_img, *before),
                    _ => (map, img_rect, main_area),
                };
                general_interaction(app, ui, &resp, &map, img, pane, native, aspect)
            }
        }
    }
    // drawn and hit-tested above the loupe and its tools: clicks on it pan
    let nav = navigator(app, ui, canvas, img_rect, id);
    // what is on screen isn't the render on its way (a stand-in, the last render after an edit,
    // a magnified frame under its window): say so once that has lasted a moment
    let whole: &[Slot] = if show_before && !split {
        &[Slot::Before]
    } else if split || split_view {
        &[Slot::Main, Slot::Before]
    } else {
        &[Slot::Main]
    };
    let coming = !matches!(shown, "none" | "hover")
        && whole.iter().chain(windows.iter().map(|(v, _)| &v.slot)).any(|s| app.renderer.is_pending(*s) && !app.renderer.retrying(*s));
    let keys =
        [Slot::Main, Slot::Before, Slot::Region, Slot::RegionBefore].map(|s| app.renderer.textures.get(&s).filter(|t| t.photo == id).map(|t| t.key));
    let rendering = ui.ctx().data_mut(|d| rendering_overdue(d.get_temp_mut_or(egui::Id::unique("loupe-rendering"), None), keys, coming, now));
    // the render failed while a stand-in is up: what is on screen is all there will be (also while
    // a library change retries it)
    let failed = (!rendering
        && (!show_before || split)
        && !matches!(shown, "render" | "none" | "hover")
        && (!app.renderer.is_pending(Slot::Main) || app.renderer.retrying(Slot::Main)))
    .then(|| app.renderer.failure_of(Slot::Main, id))
    .flatten();
    if rendering || failed.is_some() {
        // over the photo (the After in two views); in the crop tool's wide margin, under it
        let over = if crop_tool { canvas } else { img_rect.intersect(if split { main_area } else { canvas }) };
        let over = if over.is_positive() { over } else { canvas };
        match failed {
            Some(e) => failed_pill(ui, app, &photo, canvas, over, nav, "loupe:renderFailed", e),
            // View Options' Show message when loading or rendering photos
            None if app.ui.settings.loupe_messages => {
                loupe_pill(ui, canvas, over, nav, "loupe:rendering", crate::i18n::tr("Rendering…"), false);
            }
            None => {}
        }
    }
    if let Some(why) = photo.preview_only.as_deref().filter(|_| !fullscreen) {
        preview_only_pill(ui, canvas, why);
    }
    crate::widgets::context_menu(&resp, |ui| {
        crate::widgets::menu_button(ui, crate::i18n::tr("Zoom"), |ui| {
            for (label, cmd) in [("Fit", "view.zoomFit"), ("100%", "view.zoom100"), ("Zoom In", "view.zoomIn"), ("Zoom Out", "view.zoomOut")] {
                if ui.button(crate::i18n::tr(label)).clicked() {
                    let _ = app.run(cmd, json!({}));
                }
            }
        });
        ui.separator();
        super::grid::context_menu(app, ui, id);
    });
}

/// Neighbour prefetch: below on-screen thumbnails, above background thumbnail refreshes.
const PREFETCH_PRIORITY: u32 = 4;

/// Targeted adjustment tool: dragging up/down on the photo raises/lowers the tone-curve region or
/// the colour-mixer bands under the press point (`develop.targeted`, one call per whole step, all
/// in one interaction = one undo step).
fn targeted_drag(app: &mut LightcraftApp, resp: &egui::Response, map: &CanvasMap, target: &str) {
    if resp.drag_started()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let _ = app.run("develop.beginInteraction", json!({"label": "Targeted Adjustment"}));
        app.gesture = Some(Gesture::Targeted { at: map.norm(q), acc: 0.0 });
    }
    if resp.dragged()
        && let Some(Gesture::Targeted { at, acc }) = &mut app.gesture
    {
        *acc -= resp.drag_delta().y * 0.5;
        let step = acc.trunc();
        if step != 0.0 {
            *acc -= step;
            let at = *at;
            let mut p = json!({"target": target, "x": at.x, "y": at.y, "delta": step});
            if target == "curve" && app.ui.curve_channel != "parametric" {
                p["channel"] = json!(app.ui.curve_channel);
            }
            let _ = app.run("develop.targeted", p);
        }
    }
    if resp.drag_stopped() && matches!(app.gesture, Some(Gesture::Targeted { .. })) {
        app.gesture = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// The diagnostic overlay the loupe shows (the selected mask, Point Color's visualized range,
/// Visualize Spots).
pub(crate) fn view_overlay(app: &LightcraftApp, d: &DevelopSettings) -> lightcraft_pipeline::Overlay {
    use lightcraft_pipeline::{MaskView, Overlay};
    if app.ui.fullscreen {
        return Overlay::None;
    }
    // Show every visible mask's region at once (Ctrl+Shift+O / Cmd+Shift+O), ahead of the selected-mask overlay
    // so the shortcut is not shadowed by the always-on single-mask overlay.
    if app.ui.right == RightPanel::Masking && app.ui.mask_overlay_all && d.masks.iter().any(|m| m.visible && !m.components.is_empty()) {
        return Overlay::AllMasks {
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    if app.ui.right == RightPanel::Masking
        && app.ui.mask_overlay
        && let Some(m) = app.session.active_mask.and_then(|id| d.masks.iter().find(|m| m.id == id))
        && !m.components.is_empty()
    {
        return Overlay::Mask {
            id: m.id.min(u16::MAX as u32) as u16,
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    if app.ui.right == RightPanel::Remove && app.ui.visualize_spots {
        return Overlay::Spots(app.ui.spots_threshold.clamp(0.0, 100.0).round() as u8);
    }
    let edit = app.ui.right == RightPanel::Edit;
    if edit && app.ui.point_color_visualize && app.ui.flyout_open("pointColor") && app.ui.point_color < d.point_colors.len() {
        return Overlay::PointColorRange(app.ui.point_color as u8);
    }
    if edit && app.ui.hdr_visualize && d.hdr.enabled {
        return Overlay::HdrRange;
    }
    Overlay::None
}

fn clipping_overlay(app: &LightcraftApp, p: &egui::Painter, r: Rect) {
    // Highlight clipped regions using the histogram's extremes isn't spatial; show a subtle frame hint.
    if let Some(h) = app.renderer.textures.get(&Slot::Main).and_then(|t| t.histogram.as_ref()) {
        let (lo, hi) = h.clipping();
        if hi > 0.003 {
            p.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::from_rgb(255, 60, 60)), StrokeKind::Outside);
        }
        if lo > 0.003 {
            p.rect_stroke(r.expand(3.0), 0.0, Stroke::new(2.0, Color32::from_rgb(60, 120, 255)), StrokeKind::Outside);
        }
    }
}

fn general_cursor(app: &LightcraftApp, ui: &egui::Ui, resp: &egui::Response, img: Rect, canvas: Rect) {
    // A panel or overlay under the pointer keeps its own cursor.
    if !resp.dragged() && !resp.hovered() {
        return;
    }
    let cursor = match app.ui.tool.as_str() {
        "wbPicker" | "colorRange" | "pointColor" => egui::CursorIcon::Crosshair,
        tool if tool.starts_with("tat:") => egui::CursorIcon::ResizeVertical,
        _ if img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0 => {
            if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            }
        }
        _ => return,
    };
    ui.ctx().set_cursor_icon(cursor);
}

fn general_interaction(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    img: Rect,
    canvas: Rect,
    native: [usize; 2],
    aspect: f32,
) {
    if app.ui.tool == "wbPicker" {
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            let _ = app.run("develop.wbPick", json!({"x": n.x, "y": n.y}));
            app.ui.tool.clear();
        }
        return;
    }
    if let Some(target) = app.ui.tool.strip_prefix("tat:").map(str::to_string) {
        targeted_drag(app, resp, map, &target);
        return;
    }
    if app.ui.tool == "colorRange" {
        color_range_pick(app, ui, resp, map);
        return;
    }
    if app.ui.tool == "pointColor" {
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            if let Ok(r) = app.run("pointColor.pick", json!({"x": n.x, "y": n.y})) {
                app.ui.point_color = r["index"].as_u64().unwrap_or(0) as usize;
            }
            app.ui.tool.clear();
        }
        return;
    }
    // click toggles Fit ↔ the chosen click-zoom ratio at the clicked point; drag pans when zoomed
    let zoomed = img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0;
    if resp.double_clicked() || (resp.clicked() && !zoomed) {
        if let Some(q) = resp.interact_pointer_pos() {
            let u = ((q.x - img.left()) / img.width()).clamp(0.0, 1.0);
            let v = ((q.y - img.top()) / img.height()).clamp(0.0, 1.0);
            app.ui.pan = (u, v);
        }
        app.ui.zoom = if matches!(app.ui.zoom, Zoom::Fit) { Zoom::Percent(app.ui.click_zoom as f32) } else { Zoom::Fit };
        app.ui.zoom_anim = true;
    } else if resp.clicked() && zoomed {
        app.ui.zoom = Zoom::Fit;
        app.ui.zoom_anim = true;
    }
    if zoomed && resp.dragged() {
        pan_image(app, canvas, img, resp.drag_delta());
    }
    let _ = (native, aspect);
}

#[cfg(test)]
mod tests {
    /// The rotation pointer's arc bulges away from the side of the crop box it is on, and follows
    /// a box that is itself rotated.
    #[test]
    fn rotate_cursor_faces_away_from_the_crop_box() {
        use egui::{Pos2, pos2};
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let near = |a: f32, b: f32| ((a - b + PI).rem_euclid(2.0 * PI) - PI).abs() < 1e-3;
        let pts = [pos2(100.0, 100.0), pos2(300.0, 100.0), pos2(300.0, 200.0), pos2(100.0, 200.0)];
        let turn = |x: f32, y: f32| super::rotate_cursor_turn(&pts, pos2(x, y), false);
        assert!(near(turn(200.0, 80.0), 0.0), "above: as drawn");
        assert!(near(turn(320.0, 150.0), FRAC_PI_2), "right");
        assert!(near(turn(200.0, 230.0), PI), "below");
        assert!(near(turn(70.0, 150.0), -FRAC_PI_2), "left");
        assert!(near(turn(290.0, 60.0), 0.0), "above the top edge near its end: still up");
        // off each corner: diagonally out from it
        assert!(near(turn(80.0, 80.0), -FRAC_PI_4), "top-left corner");
        assert!(near(turn(320.0, 80.0), FRAC_PI_4), "top-right corner");
        assert!(near(turn(320.0, 220.0), 3.0 * FRAC_PI_4), "bottom-right corner");
        assert!(near(turn(80.0, 220.0), -3.0 * FRAC_PI_4), "bottom-left corner");
        assert!(near(super::rotate_cursor_turn(&pts, pos2(200.0, 110.0), true), 0.0), "dragged back over the top edge: still up");
        // a box turned 90°: its top edge is now on the right
        let turned = [pts[3], pts[0], pts[1], pts[2]].map(|p| pos2(400.0 - p.y, p.x));
        assert!(near(super::rotate_cursor_turn(&turned, pos2(420.0, 200.0), false), FRAC_PI_2));
        // a box turned 30°: straight out from its top edge is 30° clockwise of up
        let c = pos2(200.0, 150.0);
        let rot = |p: Pos2| {
            let (s, co) = (30f32.to_radians().sin(), 30f32.to_radians().cos());
            let d = p - c;
            pos2(c.x + d.x * co - d.y * s, c.y + d.x * s + d.y * co)
        };
        let tilted = pts.map(rot);
        assert!(near(super::rotate_cursor_turn(&tilted, rot(pos2(200.0, 60.0)), false), 30f32.to_radians()));
    }

    use super::{film_label, flipped_corners, outside_crop_mesh, texture_side};
    use egui::{Color32, Pos2, Rect, pos2};

    /// A rotated `w` × `h` box centred on `centre`.
    fn box_at(centre: Pos2, w: f32, h: f32, angle: f32) -> [Pos2; 4] {
        let (s, c) = angle.to_radians().sin_cos();
        [(-w, -h), (w, -h), (w, h), (-w, h)].map(|(x, y)| pos2(centre.x + (x * c - y * s) / 2.0, centre.y + (x * s + y * c) / 2.0))
    }

    /// Signed distances from a convex polygon's edges, positive inwards (its winding either way).
    fn sides(poly: &[Pos2], p: Pos2) -> Vec<f32> {
        let n = poly.len() as f32;
        let centre = pos2(poly.iter().map(|q| q.x).sum::<f32>() / n, poly.iter().map(|q| q.y).sum::<f32>() / n);
        (0..poly.len())
            .map(|i| {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let nrm = egui::vec2(a.y - b.y, b.x - a.x).normalized();
                nrm.dot(p - a) * nrm.dot(centre - a).signum()
            })
            .collect()
    }

    /// Issue #731: the dimming is the photo minus the crop box at any angle. Every point of the
    /// photo outside the box lies in exactly one of the mesh's triangles; no point inside the box
    /// or outside the photo lies in any. (Four quads joining photo and box corners crossed and
    /// overlapped once the box was rotated.)
    #[test]
    fn the_dimming_outside_a_rotated_crop_is_exactly_the_photo_minus_the_box() {
        let img = Rect::from_min_max(pos2(100.0, 50.0), pos2(500.0, 650.0));
        // boxes inside the photo, poking out of it, and bigger than it
        for (w, h) in [(240.0, 360.0), (420.0, 300.0), (700.0, 900.0)] {
            for angle in [0.0f32, 5.0, 15.0, 30.0, -45.0, 90.0, 137.0, 200.0] {
                let quad = box_at(img.center() + egui::vec2(30.0, -20.0), w, h, angle);
                let mesh = outside_crop_mesh(img, &quad, Color32::BLACK);
                let tris: Vec<[Pos2; 3]> = mesh
                    .indices
                    .chunks(3)
                    .map(|t| [mesh.vertices[t[0] as usize].pos, mesh.vertices[t[1] as usize].pos, mesh.vertices[t[2] as usize].pos])
                    .collect();
                let (mut inside, mut outside) = (0, 0);
                let mut y = img.top() - 20.0;
                while y < img.bottom() + 20.0 {
                    let mut x = img.left() - 20.0;
                    while x < img.right() + 20.0 {
                        let p = pos2(x, y);
                        let in_box = sides(&quad, p);
                        let in_img = sides(&[img.left_top(), img.right_top(), img.right_bottom(), img.left_bottom()], p);
                        // a pixel on an edge may fall either way: skip the boundaries themselves
                        if in_box.iter().chain(&in_img).any(|d| d.abs() < 1.0) {
                            x += 7.0;
                            continue;
                        }
                        let expected = if in_img.iter().all(|d| *d > 0.0) && !in_box.iter().all(|d| *d > 0.0) { 1 } else { 0 };
                        // a sample on the seam between two of a strip's fan triangles is in both
                        // or in neither, depending on rounding: count it both ways
                        let covered = |eps: f32| tris.iter().filter(|t| sides(*t, p).iter().all(|d| *d > eps)).count();
                        let (at_least, at_most) = (covered(0.01), covered(-0.01));
                        assert!(
                            at_least <= expected && expected <= at_most,
                            "{w}×{h} box at {angle}°: ({x}, {y}) lies in {at_least}..={at_most} triangle(s), not {expected}"
                        );
                        if expected == 1 {
                            outside += 1
                        } else if in_img.iter().all(|d| *d > 0.0) {
                            inside += 1
                        }
                        x += 7.0;
                    }
                    y += 7.0;
                }
                // (a box bigger than the photo swallows it whole at some angles)
                assert!(inside > 50 && (outside > 50 || w > img.width()), "{w}×{h} at {angle}°: {outside} dimmed, {inside} clear samples");
            }
        }
    }

    /// A flat or broken box dims nothing rather than something arbitrary (or panicking).
    #[test]
    fn a_degenerate_crop_box_dims_nothing() {
        let img = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let point = [pos2(10.0, 10.0); 4];
        let line = [pos2(10.0, 10.0), pos2(90.0, 10.0), pos2(90.0, 10.0), pos2(10.0, 10.0)];
        let nan = [pos2(f32::NAN, 10.0), pos2(90.0, 10.0), pos2(90.0, 50.0), pos2(10.0, 50.0)];
        for quad in [point, line, nan] {
            assert!(outside_crop_mesh(img, &quad, Color32::BLACK).indices.is_empty(), "{quad:?}");
        }
        assert!(outside_crop_mesh(Rect::NAN, &box_at(pos2(50.0, 50.0), 20.0, 20.0, 10.0), Color32::BLACK).indices.is_empty());
    }

    /// A raw's embedded JPEG shown while the raw decodes, or the last render while an edit renders,
    /// looked like the finished photo: the loupe says "Rendering…" once a render has been coming for
    /// a moment with nothing new on screen, and doesn't blink while slow drafts arrive.
    #[test]
    fn the_loupe_says_rendering_when_the_screen_falls_behind() {
        use super::{RENDERING_AFTER_SECS as WAIT, rendering_overdue};
        let key = |k| [Some(k), None, None, None];
        let mut wait = None;
        assert!(!rendering_overdue(&mut wait, key(1), false, 0.0), "nothing coming");
        // an edit after an idle spell: the wait starts now, not at the last frame drawn
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0), "a render just asked for");
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 0.8), "a quick render never shows it");
        assert!(rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 1.2), "nothing new for the wait");
        assert!(rendering_overdue(&mut wait, key(2), true, 31.0 + WAIT), "once up, a slow draft doesn't blink it");
        assert!(!rendering_overdue(&mut wait, key(3), false, 32.0), "done");
        // a drag whose drafts keep up: always something coming, a new draft every 0.1 s
        for i in 1..30u32 {
            assert!(!rendering_overdue(&mut wait, key(u64::from(i)), true, 40.0 + f64::from(i) * 0.1), "draft {i}");
        }
    }

    /// Issue #652: a native host reports the GPU's texture limit in the first frame's input only.
    /// The loupe must still know it in every later frame (it read the raw input, found nothing and
    /// cut every window render to 2048 px).
    #[test]
    fn the_texture_limit_outlives_the_frame_it_was_reported_in() {
        let ctx = egui::Context::default();
        assert_eq!(texture_side(&ctx), 2048, "until the host says: the smallest limit there is");
        let mut seen = Vec::new();
        for reported in [Some(8192), None, None] {
            let raw = egui::RawInput { max_texture_side: reported, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| seen.push(texture_side(ui.ctx())));
            out.textures_delta.clear();
        }
        assert_eq!(seen, [8192, 8192, 8192]);
    }

    #[test]
    fn film_labels_cut_on_characters_not_bytes() {
        // the name from #266: byte 13 falls inside '限'
        assert_eq!(film_label("202407層三限定訂閱圖(4)"), "202407層三限定訂閱圖…");
        assert_eq!(film_label("写真"), "写真");
        // exactly 14 characters (42 bytes) is shown whole
        assert_eq!(film_label("一二三四五六七八九十一二三四"), "一二三四五六七八九十一二三四");
        assert_eq!(film_label("IMG_20240712_153012"), "IMG_20240712_…");
        assert_eq!(film_label("DSC_0001"), "DSC_0001");
        assert_eq!(film_label(""), "");
    }

    /// With the photo flipped, the crop box's corners reach the screen mirrored; the dimming
    /// outside the box pairs each photo corner with the box corner on the same side of the screen
    /// (it used to pair them in data order, so a flipped photo was dimmed inside the box).
    #[test]
    fn flipped_crop_box_corners_come_in_screen_order() {
        let screen = [pos2(10.0, 10.0), pos2(90.0, 10.0), pos2(90.0, 50.0), pos2(10.0, 50.0)];
        // data order (top-left, top-right, bottom-right, bottom-left) as each flip shows it
        let h = [screen[1], screen[0], screen[3], screen[2]];
        let v = [screen[3], screen[2], screen[1], screen[0]];
        let hv = [screen[2], screen[3], screen[0], screen[1]];
        assert_eq!(flipped_corners(&screen, false, false), screen);
        assert_eq!(flipped_corners(&h, true, false), screen);
        assert_eq!(flipped_corners(&v, false, true), screen);
        assert_eq!(flipped_corners(&hv, true, true), screen);
    }
}
