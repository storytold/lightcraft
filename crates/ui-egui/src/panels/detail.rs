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
mod look_ahead;
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
use info::camera_preview_badge;
use info::{
    FILTER_PILL_HEIGHT, FILTER_PILL_INSET, brief_info, filter_pill, filter_pill_shown, info_overlay, info_overlay_extent, label_badge_rect,
    preview_only_pill, quick_name,
};
pub(crate) use info::{draw_focus_point, failed_pill, loupe_pill, rendering_overdue};
use look_ahead::BEHIND;
pub use look_ahead::LookAhead;
pub(crate) use look_ahead::{Prepare, look_ahead};
use masks::{RegionContext, RegionEdit, color_range_pick, mask_overlay, region_overlay};
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
        show_no_photo(app, ui, canvas);
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
    let area = canvas.shrink(loupe_margin(fullscreen, crop_tool));
    let native = output_px(&frame);
    let texture_side = texture_side(ui.ctx());
    // two views (before, after): side by side or stacked
    let split = matches!(app.ui.before_after, BeforeAfter::SideBySide | BeforeAfter::TopBottom);
    let split_view = matches!(app.ui.before_after, BeforeAfter::Split | BeforeAfter::SplitTopBottom);
    let areas = view_areas(app.ui.before_after, area);
    let main_area = *areas.last().unwrap_or(&area);
    let visible_texture_aspect = visible_texture_aspect(app, id, &d, crop_tool);
    // Follow the texture only where it really differs (an unsupported raw's embedded JPEG with
    // another crop): a texture's whole-pixel size is a little off the exact aspect, and the crop
    // tool's overlays need the frame's own geometry.
    let display_aspect = visible_texture_aspect.filter(|a| !crop_tool && (a / aspect - 1.0).abs() > 0.02).unwrap_or(aspect);
    let resp = ui.interact(canvas, egui::Id::unique("loupe"), Sense::click_and_drag());
    announce_photo(app, &resp, &photo);
    let mut target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
    let mut img_rect = animated_rect(ui.ctx(), &mut app.ui.zoom_anim, target_rect);
    let in_pane = |pane: Rect, image: Rect| image.translate(pane.center() - main_area.center());
    let navigating = areas.iter().any(|&view_area| {
        let view_img = in_pane(view_area, img_rect);
        navigate_gesture(app, ui, &resp, view_area, view_img, native) || middle_pan(app, ui, &resp, view_area, view_img)
    });
    if navigating {
        target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
        img_rect = target_rect;
    }
    let before_img = in_pane(areas[0], img_rect);
    let before_target = in_pane(areas[0], target_rect);
    app.image_rect = Some(img_rect);
    // request renders: the loupe at display resolution (drafts during drags)
    let interacting = app.session.interaction.is_some();
    let overlay = view_overlay(app, &d);
    // a drag drafts at full size where the GPU draws the view (by the GPU's own rule, `handles`;
    // `ready` first: `available` would wait for a device still being created)
    let gpu = interacting && gpu_draws_view(app, overlay);
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
    let plan = loupe_plan(app, id, look, sizes);
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
    let want = whole_frame_edge(app, interacting, &plan, native_long, texture_side);
    // The short side is rounded, not truncated: a truncated box (2560 x 1706 for a 2560 x 1707
    // source) made the render a slightly anisotropic resample of its source, a 2-tap blur along
    // one axis only. A box that matches the source to rounding is rendered at exactly its size.
    let short = |e: usize, a: f32| ((e as f32 / a).round() as usize).max(1);
    let (rw, rh) = if aspect >= 1.0 { (want, short(want, aspect)) } else { (short(want, 1.0 / aspect), want) };
    note_loupe_edge(app, sizes.canvas_long, !interacting && !split && !split_view);
    // hover and before renders are stand-ins: the preview size is plenty and keeps them cheap
    let se = app.ui.settings.stand_in_edge(want, texture_side);
    let (sw, sh) = if aspect >= 1.0 { (se, short(se, aspect)) } else { (short(se, 1.0 / aspect), se) };
    let ask = LoupeAsk { id, with_crop: !crop_tool, interacting, fit_px, overlay };
    let settling = request_main_or_settle(app, ui, &ask, [rw, rh], plan.window_edge.is_none(), now);
    // hovering a preset or profile: the photo with that look, shown instead of the loupe render
    // once it is ready (nothing is committed)
    let hover_key = request_hover(app, &ask, [sw, sh]);
    if !interacting {
        prefetch_neighbours(app, id, main_area, ppp, texture_side, !crop_tool);
    }
    // until the loupe has this photo: show its cached render / embedded preview / a thumbnail
    request_stand_in(app, id, want, !crop_tool);
    if !interacting {
        // (what Settings ▸ Performance lets this mode prepare ahead)
        let prepare = prepare_ahead(app);
        look_ahead(app, id, want.max(8), !crop_tool, prepare);
    }
    let show_before = app.ui.before_after == BeforeAfter::Original || ui.input(|i| i.key_down(egui::Key::Backslash));
    // (the Before render waits for a settled photo like the loupe's own)
    if !settling && (split || show_before || split_view) {
        request_before(app, &ask, [sw, sh]);
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
    // (stepping fast: no window is asked for until the photo stays, as for the frame above)
    let window_views = if window_ctx.frame_edge.is_some() && !settling {
        let mode = ViewMode { split, show_before, split_view };
        window_views(mode, &Views { img_rect, target_rect, before_img, before_target, areas: &areas, main_area, canvas })
    } else {
        Vec::new()
    };
    let windows = request_windows(app, &window_ctx, window_views);
    let single = hover_key.is_none() && !split && !show_before && !split_view && !interacting;
    let camera_full = request_camera_full(app, &photo, single && !windows.is_empty(), native_long.min(texture_side));
    let p = ui.painter_at(canvas);
    let draw = |slot: Slot, r: Rect, clip: Rect| draw_slot(app, &p, id, ppp, slot, r, clip);
    // soft proofing: a paper-white surround and the proof's name, as Lightroom shows it
    if app.ui.soft_proof && !fullscreen {
        proof_surround(app, &p, &t, canvas);
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
        split_labels(&p, &t, [(br, areas[0]), (img_rect, main_area)]);
    } else if show_before {
        shown = draw(Slot::Before, img_rect, canvas);
        // on the photo itself, so it gets the hover label's dark pill to stay readable over light
        // pictures; painted after the window renders below, which would cover it zoomed in
        let g = p.layout_no_wrap(crate::i18n::tr("Before").to_string(), t.font(12.0), Color32::WHITE);
        before_badge = Some((label_badge_rect(img_rect, canvas, g.size(), badge_top_inset), g));
    } else if let Some((_key, label)) = &hover_key
        && let Some(tex) = app.renderer.textures.get(&Slot::Hover).filter(|t| t.photo == id)
    {
        draw_hover(&p, &t, tex, label, [img_rect, canvas], badge_top_inset);
        shown = "hover";
    } else {
        shown = draw(Slot::Main, img_rect, canvas);
        if shown == "none" {
            none_message(app, &p, &t, canvas, id);
        }
    }
    // a wipe shows the Before whole-frame render on its side of the line…
    let line = crate::region::wipe_line(img_rect, canvas);
    draw_wipe_before(app, &p, id, img_rect, line);
    let camera_full_drawn = camera_full && draw_camera_full(app, &p, id, img_rect, display_aspect, window_cover(&windows));
    // …and the window renders go over the whole-frame ones, all of them, where they belong in the
    // frame (clipped to their pane, or their side of the line)
    let drawn = draw_windows(app, &p, &window_ctx, &windows, [canvas, img_rect], (show_before, split));
    let shown = shown_after_windows(shown, camera_full_drawn, drawn);
    app.loupe_shown = Some((id, shown));
    app.loupe_timing.observe(id, shown, now);
    note_pictures_missing(app, shown);
    // the camera's own JPEG is on screen, not LightCraft's rendering of the raw: say so (once), so
    // colour and tone are not judged on it (PERF-CULL-4; Lightroom Classic badges its embedded
    // previews). Not over a window render, which is LightCraft's own.
    if camera_jpeg_shown(app, &photo, shown, drawn) {
        camera_preview_badge(ui, canvas, img_rect.intersect(canvas));
    }
    if let Some((bg, g)) = before_badge {
        paint_badge(&p, bg, g);
        register(ui, "canvas:beforeLabel", bg);
    }
    // the line of a wipe over both
    draw_wipe_line(app.ui.before_after, &p, img_rect, line);
    let map = CanvasMap::new(&frame, img_rect);
    let over = OverPhoto { photo: &photo, d: &d, map: &map, canvas, img_rect, fullscreen, crop_tool, clipping: !show_before };
    photo_overlays(app, ui, &p, &over);
    // Cursor selection must also run on gesture frames, when tool input is suppressed.
    if !matches!(right, RightPanel::Crop | RightPanel::Masking | RightPanel::Remove | RightPanel::RedEye) {
        general_cursor(app, ui, &resp, img_rect, main_area);
    }
    if !navigating {
        let tv = ToolView { right, map: &map, frame: &frame, d: &d, id, split, native, aspect };
        tool_interaction(app, ui, &resp, &tv, &Views { img_rect, target_rect, before_img, before_target, areas: &areas, main_area, canvas });
    }
    // Space held over a tool: the view's hand takes the pointer from it (#1895)
    crate::space_hand::interact(app, ui, canvas, (main_area, img_rect), split.then(|| (areas[0], before_img)));
    // drawn and hit-tested above the loupe and its tools: clicks on it pan
    let nav = navigator(app, ui, canvas, img_rect, id);
    // what is on screen isn't the render on its way (a stand-in, the last render after an edit,
    // a magnified frame under its window): say so once that has lasted a moment
    let coming = render_coming(app, shown, &windows, ViewMode { split, show_before, split_view });
    let keys =
        [Slot::Main, Slot::Before, Slot::Region, Slot::RegionBefore].map(|s| app.renderer.textures.get(&s).filter(|t| t.photo == id).map(|t| t.key));
    let rendering = ui.ctx().data_mut(|d| rendering_overdue(d.get_temp_mut_or(egui::Id::unique("loupe-rendering"), None), keys, coming, now));
    // the render failed while a stand-in is up: what is on screen is all there will be (also while
    // a library change retries it)
    let failed = failed_render(app, id, !rendering && (!show_before || split), shown);
    if rendering || failed.is_some() {
        let over = if crop_tool { canvas } else { img_rect.intersect(if split { main_area } else { canvas }) };
        let over = if over.is_positive() { over } else { canvas };
        status_pill(ui, app, &photo, [canvas, over], nav, failed);
    }
    if !fullscreen {
        photo.preview_only.as_deref().inspect(|why| preview_only_pill(ui, canvas, why));
    }
    loupe_context_menu(app, &resp, id);
}

/// Names the loupe for screen readers as the grid names the same photo (file, rating, flag, label), not
/// just "Photo".
fn announce_photo(app: &LightcraftApp, resp: &egui::Response, photo: &lightcraft_catalog::Photo) {
    crate::widgets::announce(resp, crate::i18n::tr_format!("Photo: {name}", name = super::grid::cell_description(app, photo)));
}

/// The loupe's right-click menu: zoom commands and the grid's photo menu.
fn loupe_context_menu(app: &mut LightcraftApp, resp: &egui::Response, id: PhotoId) {
    crate::widgets::context_menu(resp, |ui| {
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

/// A fine grid while a transform (geometry) slider is dragged, to judge verticals.
fn transform_grid(app: &LightcraftApp, p: &egui::Painter, img_rect: Rect) {
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
}

/// The photo's face boxes and the name box on them.
fn face_boxes(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    map: &CanvasMap,
    photo: &lightcraft_catalog::Photo,
    d: &DevelopSettings,
) {
    let id = photo.id;
    // a name box left open on another photo is dropped
    if app.ui.name_edit.as_ref().is_some_and(|e| e.photo != id.0) {
        app.ui.name_edit = None;
    }
    let hints = super::faces::hints_for(app, id.0);
    let people = if app.ui.name_edit.is_some() { app.caches.person_names(&app.session.catalog) } else { Default::default() };
    // while the name box is open and recognition is not set up, the box offers the next step
    let setup = if app.ui.name_edit.is_some() { super::faces::setup(app, ui.ctx()) } else { super::faces::Setup::Running };
    let regions = RegionContext { photo, orient: d.orientation, hints: hints.as_deref(), people: &people, setup };
    match region_overlay(ui, p, map, &regions, &mut app.ui.name_edit) {
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

/// A wipe shows the Before whole-frame render on its side of the line.
fn draw_wipe_before(app: &LightcraftApp, p: &egui::Painter, id: PhotoId, img_rect: Rect, line: Pos2) {
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
}

/// Once a photo is on screen, prepare its neighbours in filmstrip order (source decoded and kept,
/// view render cached) so stepping to them is instant.
fn prefetch_neighbours(app: &mut LightcraftApp, id: PhotoId, main_area: Rect, ppp: f32, texture_side: usize, with_crop: bool) {
    // only once this photo is on screen (nor while a cell on screen waits for its first picture: those come before
    // anything bigger)
    if app.renderer.is_pending(Slot::Main)
        || app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        || app.pictures_busy(app.renderer.frame())
    {
        return;
    }
    let ids = app.session.visible_cloned();
    let mode = prep_mode(app);
    let prep = app.session.prep_settings();
    let ahead = prep.ahead(mode);
    // editing: the next photos are decoded and kept, ready for their edits (their view render is not
    // what is wanted, and a cached one would leave the photo undecoded)
    let develop = mode == lightcraft_engine::prep::Mode::Develop;
    let Some(i) = ids.iter().position(|p| *p == id).filter(|_| ahead > 0) else { return };
    let mut targets = app.look_ahead.targets_n(&ids, i, ahead, BEHIND);
    if prep.scope(mode) == lightcraft_engine::prep::Scope::Marked {
        targets.retain(|t| lightcraft_engine::prep::marked(&app.session, *t));
    }
    // a few at a time, nearest first (the pool keeps workers free for the photo the user goes to)
    let mut room = PREFETCH_AT_ONCE.saturating_sub(app.renderer.prefetches_pending());
    let todo: Vec<(usize, PhotoId)> = targets.into_iter().filter_map(|nid| ids.iter().position(|p| *p == nid).map(|at| (at, nid))).collect();
    for (at, nid) in todo {
        if room == 0 {
            break;
        }
        let n = prefetch_slot(at);
        let Some(mut job) = neighbour_job(app, nid, main_area, ppp, texture_side, with_crop) else { continue };
        if develop {
            job.view_cache = None;
        }
        // (asked for already, in this slot: queued, running or made)
        if app.renderer.prefetch_asked(n, job.key) {
            continue;
        }
        room -= 1;
        app.renderer.prefetch(n, job, PREFETCH_PRIORITY);
    }
}

/// The render a neighbour is prepared with: its loupe render at the size it will be drawn at.
fn neighbour_job(
    app: &mut LightcraftApp,
    nid: PhotoId,
    main_area: Rect,
    ppp: f32,
    texture_side: usize,
    with_crop: bool,
) -> Option<lightcraft_engine::media::RenderJob> {
    let np = app.session.catalog.photo(nid)?;
    let nf = Frame::with_lens(np.width.max(1) as usize, np.height.max(1) as usize, &np.develop, with_crop, np.embedded_lens.as_ref());
    let na = nf.aspect() as f32;
    let nr = fit_rect(main_area, na, app.ui.zoom, output_px(&nf), ppp, app.ui.pan);
    let nw = app.ui.settings.prefetch_edge(nr.width().max(nr.height()) * ppp, output_px(&nf).into_iter().max().unwrap_or(1), texture_side);
    let (w, h) = if na >= 1.0 { (nw, (nw as f32 / na) as usize) } else { ((nw as f32 * na) as usize, nw) };
    app.session.loupe_job(nid, w.max(8), h.max(8), with_crop)
}

/// What Settings ▸ Performance lets the mode the user is in prepare ahead.
fn prepare_ahead(app: &LightcraftApp) -> Prepare {
    let mode = prep_mode(app);
    let prep = app.session.prep_settings();
    Prepare::of(prep.ahead(mode), prep.scope(mode))
}

/// The most the loupe shows a whole photo at (the canvas's long side; a photo of another shape than
/// this one may fill it), noted while it shows one view fitted (`single`): standard previews are
/// built at least this big.
fn note_loupe_edge(app: &mut LightcraftApp, canvas_long: f32, single: bool) {
    if single && matches!(app.ui.zoom, Zoom::Fit) && canvas_long.is_finite() && canvas_long >= 1.0 {
        app.session.loupe_edge = canvas_long.ceil() as usize;
    }
}

/// The rects of the two views of a before/after (side by side or stacked), else the one area.
fn view_areas(mode: BeforeAfter, area: Rect) -> Vec<Rect> {
    match mode {
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
    }
}

/// Which before/after layout is up.
#[derive(Clone, Copy)]
struct ViewMode {
    split: bool,
    show_before: bool,
    split_view: bool,
}

/// The rects the loupe's views are drawn in.
struct Views<'a> {
    img_rect: Rect,
    target_rect: Rect,
    before_img: Rect,
    before_target: Rect,
    areas: &'a [Rect],
    main_area: Rect,
    canvas: Rect,
}

/// The window renders wanted for the layout: (slot, before side?, rect to draw in, rect it is
/// requested for, area whose pixels show).
fn window_views(mode: ViewMode, v: &Views) -> Vec<WindowView> {
    let mut out: Vec<WindowView> = Vec::new();
    let after = WindowView { slot: Slot::Region, before: false, img: v.img_rect, target: v.target_rect, visible: v.main_area };
    if mode.split {
        out.push(WindowView {
            slot: Slot::RegionBefore,
            before: true,
            img: v.before_img,
            target: v.before_target,
            visible: v.areas.first().copied().unwrap_or(v.canvas),
        });
        out.push(after);
    } else if mode.show_before {
        out.push(WindowView { slot: Slot::RegionBefore, before: true, img: v.img_rect, target: v.target_rect, visible: v.canvas });
    } else if mode.split_view {
        out.push(WindowView { slot: Slot::RegionBefore, before: true, img: v.img_rect, target: v.target_rect, visible: v.canvas });
        out.push(WindowView { visible: v.canvas, ..after });
    } else {
        out.push(WindowView { visible: v.canvas, ..after });
    }
    out
}

/// What the canvas says when nothing could be drawn: why the photo failed, or that it is rendering.
fn none_message(app: &LightcraftApp, p: &egui::Painter, t: &Tokens, canvas: Rect, id: PhotoId) {
    match app.renderer.failure_of(Slot::Main, id) {
        Some(e) => {
            p.text(canvas.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, crate::i18n::tr("This photo can't be opened"), t.semibold(14.0), t.text);
            p.text(canvas.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, e, t.font(12.0), t.text_dim);
        }
        None if app.ui.settings.loupe_messages => {
            p.text(canvas.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(13.0), t.text_dim);
        }
        None => {}
    }
}

/// The render failed while a stand-in is up: what is on screen is all there will be (also while a
/// library change retries it).
fn failed_render<'a>(app: &'a LightcraftApp, id: PhotoId, settled: bool, shown: &str) -> Option<&'a str> {
    (settled && !matches!(shown, "render" | "none" | "hover") && (!app.renderer.is_pending(Slot::Main) || app.renderer.retrying(Slot::Main)))
        .then(|| app.renderer.failure_of(Slot::Main, id))
        .flatten()
}

/// The pill over the photo (the After in two views; in the crop tool's wide margin, under it): the
/// failure, or View Options' "Show message when loading or rendering photos".
fn status_pill(
    ui: &egui::Ui,
    app: &LightcraftApp,
    photo: &lightcraft_catalog::Photo,
    [canvas, over]: [Rect; 2],
    nav: Option<Rect>,
    failed: Option<&str>,
) {
    match failed {
        Some(e) => failed_pill(ui, app, photo, canvas, over, nav, "loupe:renderFailed", e),
        None if app.ui.settings.loupe_messages => {
            loupe_pill(ui, canvas, over, nav, "loupe:rendering", crate::i18n::tr("Rendering…"), false);
        }
        None => {}
    }
}

/// Soft proofing: a paper-white surround and the proof's name, as Lightroom shows it.
fn proof_surround(app: &LightcraftApp, p: &egui::Painter, t: &Tokens, canvas: Rect) {
    p.rect_filled(canvas, 0.0, Color32::from_gray(238));
    let label = crate::i18n::tr_format!("Proof Preview · {}", app.ui.proof.space.label());
    p.text(pos2(canvas.right() - 16.0, canvas.top() + 14.0), Align2::RIGHT_CENTER, label, t.font(12.5), Color32::from_gray(60));
}

/// Request the window renders, remember which sides have one, and release the slot of a side without.
fn request_windows(app: &mut LightcraftApp, ctx: &WindowCtx, views: Vec<WindowView>) -> Vec<(WindowView, crate::region::RegionView)> {
    let mut windows: Vec<(WindowView, crate::region::RegionView)> = Vec::new();
    for v in views {
        if let Some(view) = request_window(app, ctx, &v) {
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
    windows
}

/// The aspect of the texture on screen for this photo and geometry, if one is.
fn visible_texture_aspect(app: &LightcraftApp, id: PhotoId, d: &DevelopSettings, crop_tool: bool) -> Option<f32> {
    // A previous crop/rotation can stay visible while its replacement renders. Its pixels
    // must not override the current frame geometry, even when it is the same photo.
    let current_texture = |t: &&crate::render::Tex| t.photo == id && t.look == Some((crate::render::geometry_look(d), !crop_tool));
    app.renderer
        .textures
        .get(&Slot::Main)
        .filter(current_texture)
        .or_else(|| app.renderer.textures.get(&Slot::Preview).filter(current_texture))
        .or_else(|| app.renderer.thumb(id).filter(current_texture))
        .map(|tex| tex.size[0].max(1) as f32 / tex.size[1].max(1) as f32)
}

/// Whether the GPU draws the view (by the GPU's own rule, `handles`; `ready` first: `available`
/// would wait for a device still being created).
fn gpu_draws_view(app: &LightcraftApp, overlay: lightcraft_pipeline::Overlay) -> bool {
    lightcraft_engine::gpu::handles(
        lightcraft_pipeline::OutputDepth::U8,
        app.ui.soft_proof && lightcraft_pipeline::proof_profile(app.ui.proof.profile).is_some(),
        overlay,
    ) && lightcraft_engine::gpu::ready()
        && lightcraft_engine::gpu::available()
}

/// The line of a wipe.
fn draw_wipe_line(mode: BeforeAfter, p: &egui::Painter, img_rect: Rect, line: Pos2) {
    let stroke = Stroke::new(1.5, Color32::WHITE);
    match mode {
        BeforeAfter::Split => {
            p.line_segment([pos2(line.x, img_rect.top()), pos2(line.x, img_rect.bottom())], stroke);
        }
        BeforeAfter::SplitTopBottom => {
            p.line_segment([pos2(img_rect.left(), line.y), pos2(img_rect.right(), line.y)], stroke);
        }
        _ => {}
    }
}

/// Whether the render on its way is not what is on screen (a stand-in, the last render after an
/// edit, a magnified frame under its window).
fn render_coming(app: &LightcraftApp, shown: &str, windows: &[(WindowView, crate::region::RegionView)], mode: ViewMode) -> bool {
    let whole: &[Slot] = if mode.show_before && !mode.split {
        &[Slot::Before]
    } else if mode.split || mode.split_view {
        &[Slot::Main, Slot::Before]
    } else {
        &[Slot::Main]
    };
    !matches!(shown, "none" | "hover")
        && whole.iter().chain(windows.iter().map(|(v, _)| &v.slot)).any(|s| app.renderer.is_pending(*s) && !app.renderer.retrying(*s))
}

/// What the active tool needs to know about the loupe.
struct ToolView<'a> {
    right: RightPanel,
    map: &'a CanvasMap,
    frame: &'a Frame,
    d: &'a DevelopSettings,
    id: PhotoId,
    split: bool,
    native: [usize; 2],
    aspect: f32,
}

/// Route the pointer to the right panel's tool on the photo, or to the general interactions.
fn tool_interaction(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, tv: &ToolView, v: &Views) {
    let (map, d) = (tv.map, tv.d);
    match tv.right {
        RightPanel::Crop => crop_overlay(app, ui, resp, map, tv.frame, d, tv.id),
        RightPanel::Masking => mask_overlay(app, ui, resp, map, d),
        RightPanel::Remove => remove_overlay(app, ui, resp, map, d),
        RightPanel::RedEye => eye_overlay(app, ui, resp, map, d),
        _ => {
            // side by side / stacked: Before shows the same frame, so a click in its view (white
            // balance, colour samples, targeted adjustment) means the same point of the photo.
            // Zoomed in, a view's image reaches past its pane: what counts is the pane clicked.
            // A click-zoom there centres both views on the point clicked (#1742).
            let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
            let (map, img, pane) = match (tv.split, v.areas.first(), pointer) {
                (true, Some(before), Some(q)) if before.contains(q) => (CanvasMap::new(tv.frame, v.before_img), v.before_img, *before),
                _ => (CanvasMap::new(tv.frame, v.img_rect), v.img_rect, v.main_area),
            };
            general_interaction(app, ui, resp, &map, &PhotoPlacement { img, canvas: pane, native: tv.native, aspect: tv.aspect })
        }
    }
}

/// The loupe's render plan, without a window for a view the GPU already refused one for.
fn loupe_plan(app: &LightcraftApp, id: PhotoId, look: u64, sizes: crate::region::ViewSizes) -> crate::region::LoupePlan {
    let plan = crate::region::plan(&app.ui.settings, sizes);
    if plan.window_edge.is_some_and(|edge| app.window_refused == Some((id, look, edge))) {
        return crate::region::plan(&app.ui.settings, crate::region::ViewSizes { windows: false, ..sizes });
    }
    plan
}

fn whole_frame_edge(app: &LightcraftApp, interacting: bool, plan: &crate::region::LoupePlan, native_long: usize, texture_side: usize) -> usize {
    if interacting || plan.window_edge.is_some() {
        plan.main_edge
    } else {
        // (a user's explicit Preview size below the preview level still caps it)
        let limit = if app.ui.settings.preview_limit == 0 { usize::MAX } else { app.ui.settings.preview_limit as usize };
        let floor = lightcraft_engine::SourceLevel::Preview.max_edge().min(limit).min(native_long);
        plan.main_edge.max(floor).min(texture_side.max(crate::state::MIN_TEXTURE_SIDE))
    }
}

/// What a loupe render request is for.
struct LoupeAsk {
    id: PhotoId,
    with_crop: bool,
    interacting: bool,
    fit_px: [usize; 2],
    overlay: lightcraft_pipeline::Overlay,
}

/// Stepping fast (an arrow held down): the render waits until the photo stays, the camera JPEG or
/// thumbnail shows meanwhile, and a render queued for a photo already left is dropped. Otherwise
/// the loupe's own render is requested. Whether the photo is still settling.
fn request_main_or_settle(app: &mut LightcraftApp, ui: &egui::Ui, a: &LoupeAsk, size: [usize; 2], whole_draft: bool, now: f64) -> bool {
    let settling = !a.interacting && app.loupe_timing.settling(a.id, now);
    if !settling {
        request_main(app, ui, a, size, whole_draft);
        return false;
    }
    drop_unsettled(&mut app.renderer, a.id);
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::loupe_timing::SETTLE_SECS / 2.0));
    true
}

/// While the photo settles: drop the raw renders still queued for a photo the user is stepping
/// past (one already running finishes). Its whole-frame render unless that is on screen already,
/// the zoomed-in windows (a region render decodes the raw as much as the frame) and the Before
/// view, which is a raw render too.
pub(crate) fn drop_unsettled(renderer: &mut crate::render::Renderer, photo: PhotoId) {
    if renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != photo) {
        renderer.drop_queued(Slot::Main);
    }
    for slot in [Slot::Region, Slot::RegionBefore, Slot::Before] {
        renderer.drop_queued(slot);
    }
}

/// Zoomed in on a raw as shot (unedited, or only its camera profile picked; `zoomed`: one view,
/// window renders up): its camera JPEG at full size under the window render, so a focus check
/// doesn't wait for the raw (PERF-CULL-5; Capture One's Cull view shows the embedded JPEG at 100 %
/// too). Released when the view is not zoomed any more (180 MB). Whether it is wanted.
fn request_camera_full(app: &mut LightcraftApp, photo: &lightcraft_catalog::Photo, zoomed: bool, edge: usize) -> bool {
    let wanted = zoomed && lightcraft_engine::import::looks_as_shot(photo);
    if wanted {
        if let Some(q) = app.session.embedded_full_job(photo.id, edge) {
            app.renderer.request_quick(Slot::QuickFull, q, 98);
        }
    } else if app.renderer.textures.contains_key(&Slot::QuickFull) || app.renderer.is_pending(Slot::QuickFull) {
        app.renderer.forget(Slot::QuickFull);
    }
    wanted
}

/// The camera JPEG at full size, magnified like the whole-frame render, until the window is
/// there: only where the window will go (`cover`, see [`window_cover`]). A window capped smaller
/// than the view leaves strips around it, and those keep the developed whole-frame render rather
/// than an unlabelled camera JPEG (CodeRabbit on #3544). Whether it was drawn.
fn draw_camera_full(app: &LightcraftApp, p: &egui::Painter, id: PhotoId, img_rect: Rect, display_aspect: f32, cover: Option<Rect>) -> bool {
    let Some(cover) = cover else { return false };
    let Some(tex) = app.renderer.textures.get(&Slot::QuickFull).filter(|t| t.photo == id) else { return false };
    if !crate::region::same_aspect(tex.size[0].max(1) as f32 / tex.size[1].max(1) as f32, display_aspect) {
        return false;
    }
    p.with_clip_rect(p.clip_rect().intersect(cover)).image(
        tex.tex.id(),
        img_rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    true
}

/// Where on screen the requested window renders go (each its window's share of its view's drawn
/// rect), joined: `None` without any.
fn window_cover(windows: &[(WindowView, crate::region::RegionView)]) -> Option<Rect> {
    windows
        .iter()
        .map(|(v, w)| {
            let (fw, fh) = (w.full.0.max(1) as f32, w.full.1.max(1) as f32);
            let at = v.img.min + vec2(w.window.x as f32 / fw * v.img.width(), w.window.y as f32 / fh * v.img.height());
            Rect::from_min_size(at, vec2(w.window.w as f32 / fw * v.img.width(), w.window.h as f32 / fh * v.img.height()))
        })
        .reduce(|a, b| a.union(b))
}

/// Which window renders [`draw_windows`] drew.
#[derive(Clone, Copy)]
struct WindowsDrawn {
    /// Any of them.
    any: bool,
    /// The window of the view shown whole: the After side's in a split view, the Before's when
    /// the Before is shown alone; what the timing and the source shown are about. Holding `\`
    /// side by side or top/bottom keeps both panes, and the After pane is still the one shown.
    shown: bool,
}

/// Draw the window renders over the whole-frame ones, each where it belongs in the frame
/// (clipped to its pane, or its side of the line).
fn draw_windows(
    app: &LightcraftApp,
    p: &egui::Painter,
    ctx: &WindowCtx,
    windows: &[(WindowView, crate::region::RegionView)],
    [canvas, img_rect]: [Rect; 2],
    (show_before, split): (bool, bool),
) -> WindowsDrawn {
    let mut out = WindowsDrawn { any: false, shown: false };
    // (holding `\` in a wipe shows the Before everywhere: it is the Before view alone then; side by
    // side or top/bottom, both panes stay, and each window stays in its own)
    let before_alone = show_before && !split;
    for (v, view) in windows {
        let mode = if before_alone { BeforeAfter::Original } else { app.ui.before_after };
        let clip = crate::region::window_clip(mode, v.before, v.visible, canvas, img_rect);
        let drawn = draw_window(&p.with_clip_rect(clip), app, ctx, v, view);
        out.any |= drawn;
        out.shown |= drawn && (!v.before || before_alone);
    }
    out
}

/// What is on screen once the window renders are drawn: the camera JPEG at full size stands in
/// for the whole-frame render until a window covers it, and a window rendered over a stand-in
/// (thumbnail or camera JPEG) before the whole frame is the raw's own render.
fn shown_after_windows(shown: &'static str, camera_full_drawn: bool, drawn: WindowsDrawn) -> &'static str {
    if camera_full_drawn && !drawn.any {
        "embedded"
    } else if drawn.shown && !matches!(shown, "render" | "cached") {
        "render"
    } else {
        shown
    }
}

/// No picture of the photo on screen yet, or only a small one while its full-size stand-in is on its
/// way: background work waits (the photo on screen comes first, at its cheapest first).
fn note_pictures_missing(app: &mut LightcraftApp, shown: &str) {
    let coming = app.renderer.is_pending(Slot::Preview);
    if (shown == "none" && (coming || app.renderer.is_pending(Slot::Main))) || (!crate::loupe_timing::is_full_size(shown) && coming) {
        app.pictures_missing(app.renderer.frame());
    }
}

/// Whether the camera's own JPEG is what is on screen (not under a window render).
fn camera_jpeg_shown(app: &LightcraftApp, photo: &lightcraft_catalog::Photo, shown: &str, drawn: WindowsDrawn) -> bool {
    !drawn.any && (shown == "embedded" || (shown == "thumb" && app.session.embedded_thumb_is_final(photo)))
}

/// Request the loupe's own render at `[rw, rh]` (drafts during drags).
fn request_main(app: &mut LightcraftApp, ui: &egui::Ui, a: &LoupeAsk, [rw, rh]: [usize; 2], whole_draft: bool) {
    let (id, interacting, fit_px, overlay) = (a.id, a.interacting, a.fit_px, a.overlay);
    if let Some(mut job) = app.session.loupe_job(id, rw.max(8), rh.max(8), a.with_crop) {
        // editing: the photo is rendered from its source, which stays decoded for the first slider
        // draft (a cached view render would show at once but leave the source unread until then)
        if prep_mode(app) == lightcraft_engine::prep::Mode::Develop {
            job.view_cache = None;
        }
        let job = if interacting { job.draft() } else { job };
        // at rest the render is shrunk to exactly the size it is drawn at with a Lanczos filter on
        // the worker, so the window draws it texel for texel instead of shrinking it with 2-tap
        // sampling (the render is up to a 64 px step larger, or the preview level)
        let job = if interacting { job } else { job.with_display_fit(fit_px[0], fit_px[1]) };
        let job = job.with_overlay(overlay).with_proof(app.ui.soft_proof.then_some(app.ui.proof));
        let job = job.with_hidden_grading(app.ui.grading_hidden_for(id));
        app.renderer.request(Slot::Main, job, 100);
        // the photo on screen comes first: background preparation waits while it is being made
        // (not once it is there: asking every frame held the background build back for good; nor
        // once its render failed and nothing is on the way, which would hold it back as long as
        // the photo stays on screen)
        let pending = app.renderer.is_pending(Slot::Main);
        let failed = app.renderer.failure_of(Slot::Main, id).is_some();
        let shown = app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.photo == id);
        if holds_foreground(pending, failed, shown) {
            signal_foreground();
        }
        // a drag's draft that renders in a few ms (GPU) is drawn in this frame, not the next (zoomed
        // in, what changes on screen is the window's draft)
        #[cfg(not(target_arch = "wasm32"))]
        if interacting && whole_draft {
            let frame = ui.input(|i| i.predicted_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
            // (a frame time that is not a number waits for nothing)
            let budget = std::time::Duration::try_from_secs_f32(frame * 0.4).unwrap_or_default();
            app.renderer.wait_for_draft(ui.ctx(), &mut app.session, budget);
        }
    }
}

/// Whether the loupe holds background preparation back: while its render is on its way (also a
/// retry after a failure), or while the photo isn't shown yet and its render hasn't failed.
fn holds_foreground(pending: bool, failed: bool, shown: bool) -> bool {
    pending || (!failed && !shown)
}

/// Tell background preparation the loupe is waiting for its render ([`lightcraft_engine::prep::foreground`]).
fn signal_foreground() {
    #[cfg(test)]
    FOREGROUND_SIGNALS.with(|n| n.set(n.get() + 1));
    lightcraft_engine::prep::foreground();
}

#[cfg(test)]
thread_local! {
    /// Tests: how often this thread's loupe told background preparation to wait.
    pub(crate) static FOREGROUND_SIGNALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Hovering a preset or profile: the photo with that look, shown instead of the loupe render once
/// it is ready (nothing is committed). The key of its render and the label to show.
fn request_hover(app: &mut LightcraftApp, a: &LoupeAsk, [sw, sh]: [usize; 2]) -> Option<(u64, String)> {
    match app.hover_preview.clone() {
        Some(h) if !a.interacting => app.session.preview_job(a.id, sw.max(8), sh.max(8), a.with_crop, &h.settings).map(|job| {
            // (the same display fit as the After view, drawn texel for texel like it)
            let job = job.with_display_fit(a.fit_px[0], a.fit_px[1]);
            let key = job.key;
            app.renderer.request(Slot::Hover, job, 105);
            (key, h.label)
        }),
        _ => None,
    }
}

/// The Before render, at the stand-in size.
fn request_before(app: &mut LightcraftApp, a: &LoupeAsk, [sw, sh]: [usize; 2]) {
    if let Some(job) = app.session.render_job(a.id, sw.max(8), sh.max(8), true, a.with_crop) {
        // (the same display fit as the After view, so both panes are the same texture size)
        let job = if a.interacting { job } else { job.with_display_fit(a.fit_px[0], a.fit_px[1]) };
        app.renderer.request(Slot::Before, job, 90);
    }
}

/// What a view slot shows: its own render of this photo, else the stand-ins (no blank frame
/// between photos, and the full render replaces them in place). Returns what it drew.
fn draw_slot(app: &LightcraftApp, p: &egui::Painter, id: PhotoId, ppp: f32, slot: Slot, r: Rect, clip: Rect) -> &'static str {
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
}

/// The hovered look's render over the photo, with its label.
fn draw_hover(p: &egui::Painter, t: &Tokens, tex: &crate::render::Tex, label: &str, [img_rect, canvas]: [Rect; 2], badge_top_inset: f32) {
    let hover_rect = fit_texture_rect(img_rect, tex.size);
    p.image(tex.tex.id(), hover_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    let g = p.layout_no_wrap(label.to_string(), t.font(12.0), Color32::WHITE);
    let bg = label_badge_rect(hover_rect, canvas, g.size(), badge_top_inset);
    paint_badge(p, bg, g);
}

/// A label on a dark pill.
fn paint_badge(p: &egui::Painter, bg: Rect, g: std::sync::Arc<egui::Galley>) {
    p.rect_filled(bg, 4.0, Color32::from_black_alpha(160));
    p.galley(bg.min + vec2(8.0, 4.0), g, Color32::WHITE);
}

/// No photo is active: the empty-library message if the library is empty, else "No photo selected".
fn show_no_photo(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    if super::empty_library::shown(&app.session) {
        super::empty_library::show(app, ui, canvas);
    } else {
        super::empty_message(ui, canvas, "No photo selected", "Choose a photo in the grid or filmstrip");
    }
}

/// The margin round the photo: none full screen, wide in the crop tool.
fn loupe_margin(fullscreen: bool, crop_tool: bool) -> f32 {
    if fullscreen {
        0.0
    } else if crop_tool {
        48.0
    } else {
        24.0
    }
}

/// The labels under each photo, or under its pane when the photo reaches past it.
fn split_labels(p: &egui::Painter, t: &Tokens, [before, after]: [(Rect, Rect); 2]) {
    let label_at = |r: Rect, pane: Rect| pos2(r.left().max(pane.left()), r.bottom().min(pane.bottom()) + 14.0);
    p.text(label_at(before.0, before.1), Align2::LEFT_CENTER, crate::i18n::tr("Before"), t.font(12.0), t.text_dim);
    p.text(label_at(after.0, after.1), Align2::LEFT_CENTER, crate::i18n::tr("After"), t.font(12.0), t.text_dim);
}

/// What is drawn over the photo.
struct OverPhoto<'a> {
    photo: &'a lightcraft_catalog::Photo,
    d: &'a DevelopSettings,
    map: &'a CanvasMap,
    canvas: Rect,
    img_rect: Rect,
    fullscreen: bool,
    crop_tool: bool,
    /// the clipping overlay may show (not in the Before view)
    clipping: bool,
}

/// The overlays over the photo: clipping, layout image, info, filter pill, faces, focus point and
/// the transform grid.
fn photo_overlays(app: &mut LightcraftApp, ui: &mut egui::Ui, p: &egui::Painter, o: &OverPhoto) {
    let (canvas, img_rect) = (o.canvas, o.img_rect);
    if app.ui.show_clipping && o.clipping && !o.fullscreen {
        clipping_overlay(app, p, img_rect);
    }
    // View ▸ Loupe Overlay ▸ Layout Image, over the photo (the After view in a before/after)
    super::layout_overlay::draw(app, p, img_rect);
    register(ui, "canvas:image", img_rect);
    info_overlay(app, p, canvas, o.photo);
    if !o.fullscreen {
        filter_pill(app, ui, canvas);
    }
    if app.ui.face_boxes {
        face_boxes(app, ui, p, o.map, o.photo, o.d);
    }
    if app.ui.settings.focus_point && !o.crop_tool {
        draw_focus_point(&p.with_clip_rect(img_rect.intersect(canvas)), img_rect, o.photo, "canvas:focusPoint");
    }
    transform_grid(app, p, img_rect);
}

fn request_stand_in(app: &mut LightcraftApp, id: PhotoId, edge: usize, with_crop: bool) {
    if app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        && let Some(q) = app.session.quick_view_job(id, edge.max(8), with_crop)
    {
        app.renderer.request_quick(Slot::Preview, q, 110);
    }
}

/// Neighbour prefetch: below on-screen thumbnails, above background thumbnail refreshes.
const PREFETCH_PRIORITY: u32 = 4;
/// Photos prepared ahead at once (each render spreads over the cores already): the others wait,
/// so the photo the user steps to next finds a worker free.
const PREFETCH_AT_ONCE: usize = 2;
/// Prepared-ahead slots, by the photo's place in the view: a step keeps the photos still ahead in
/// the slots they have (nothing is asked for twice), and the jump to another part of the shoot
/// replaces what was waiting.
const PREFETCH_SLOTS: usize = 32;

fn prefetch_slot(at: usize) -> Slot {
    Slot::Prefetch((at % PREFETCH_SLOTS) as u8)
}

/// What the user is doing, for the background preparation: editing (a tool panel open), else
/// culling a folder or browsing the library.
pub fn prep_mode(app: &LightcraftApp) -> lightcraft_engine::prep::Mode {
    match app.ui.right {
        RightPanel::Edit | RightPanel::Crop | RightPanel::Remove | RightPanel::Masking | RightPanel::RedEye => lightcraft_engine::prep::Mode::Develop,
        _ => lightcraft_engine::prep::Mode::of(&app.session),
    }
}

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

/// The diagnostic overlay the loupe shows (an Alt-drag preview, the selected mask, Point Color's
/// visualized range, Visualize Spots).
pub(crate) fn view_overlay(app: &LightcraftApp, d: &DevelopSettings) -> lightcraft_pipeline::Overlay {
    use lightcraft_pipeline::{MaskView, Overlay};
    if app.ui.fullscreen {
        return Overlay::None;
    }
    // Lightroom's temporary previews: clipping, greyscale, sharpening detail / mask
    if app.ui.right == RightPanel::Edit
        && app.ui.alt_held
        && let Some(o) = app.ui.dragging_control.as_deref().and_then(Overlay::for_control)
    {
        return o;
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

/// Where the photo is on the canvas.
struct PhotoPlacement {
    /// the photo's rectangle
    img: Rect,
    /// the canvas it is shown on
    canvas: Rect,
    /// the photo's size in pixels
    native: [usize; 2],
    aspect: f32,
}

fn general_interaction(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, placement: &PhotoPlacement) {
    let PhotoPlacement { img, canvas, native, aspect } = *placement;
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
    click_zoom_and_pan(app, resp, img, canvas);
    let _ = (native, aspect);
}

/// The photo `img` reaches past its pane `canvas`: a drag pans it.
pub(crate) fn zoomed(img: Rect, canvas: Rect) -> bool {
    img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0
}

/// The loupe's own click and drag (also Space's hand over a tool, `space_hand`): a click toggles
/// Fit ↔ the chosen click-zoom ratio at the clicked point, a drag pans when zoomed.
pub(crate) fn click_zoom_and_pan(app: &mut LightcraftApp, resp: &egui::Response, img: Rect, canvas: Rect) {
    let zoomed = zoomed(img, canvas);
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
}

#[cfg(test)]
mod tests {
    /// The loupe's hold on background preparation (CodeRabbit on #3080): on while its render is on
    /// its way, a retry after a failure included; off once the photo is shown, and off once its
    /// render failed with nothing on the way (it held the build back as long as the photo stayed).
    #[test]
    fn a_render_on_its_way_holds_background_preparation_even_after_a_failure() {
        use super::holds_foreground;
        assert!(holds_foreground(true, false, false), "the first render on its way");
        assert!(holds_foreground(true, true, false), "a retry on its way after a failure");
        assert!(holds_foreground(true, false, true), "a newer render on its way over the shown photo");
        assert!(holds_foreground(false, false, false), "not shown yet, nothing failed");
        assert!(!holds_foreground(false, false, true), "shown");
        assert!(!holds_foreground(false, true, false), "failed, nothing on the way");
    }

    /// The camera JPEG goes only where the window render will: a window of the left half of the
    /// frame covers the left half of the drawn photo, not the strips beside it.
    #[test]
    fn the_camera_jpeg_covers_only_where_the_window_goes() {
        use super::{WindowView, window_cover};
        use crate::region::RegionView;
        use egui::{Rect, pos2};
        use lightcraft_pipeline::PixelWindow;
        assert_eq!(window_cover(&[]), None, "no window: no camera JPEG");
        let img = Rect::from_min_max(pos2(100.0, 50.0), pos2(700.0, 450.0));
        let v = WindowView { slot: crate::render::Slot::Region, before: false, img, target: img, visible: img };
        let w = RegionView {
            photo: lightcraft_catalog::PhotoId(1),
            before: false,
            key: 1,
            full: (6000, 4000),
            window: PixelWindow { x: 0, y: 1000, w: 3000, h: 2000 },
            settings: 0,
        };
        let r = window_cover(&[(v, w)]).unwrap();
        assert_eq!(r, Rect::from_min_max(pos2(100.0, 150.0), pos2(400.0, 350.0)), "the window's share of the drawn rect");
    }

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
