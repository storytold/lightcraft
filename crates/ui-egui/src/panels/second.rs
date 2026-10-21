//! The second window (Window ▸ Second Window): the active photo in its own window, for a second
//! display while the main window shows the grid or the tools.
//!
//! It has its own zoom (`view.secondZoom`, kept in `UiState::second_zoom` / `second_pan`; the
//! loupe's is separate). The bar along its top picks Fit, Fill or a percentage, steps with − / +,
//! or slides between Fit and 800 %. Over the photo it behaves as the loupe: the wheel or a pinch
//! zooms around the pointer, a click toggles Fit and the click-zoom ratio (the bottom bar's
//! Click n:1) at the point clicked, a click when zoomed in fits, and a drag pans. In a native
//! window the loupe's zoom shortcuts, as the keymap binds them (Zoom In / Out, Zoom to Fit,
//! Zoom 100%, Toggle Zoom), act on it. Zoomed past its
//! whole-frame render, the part on screen is rendered as a window (`Slot::SecondRegion`) and drawn
//! over it, as the loupe and Compare do.

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::render::Slot;
use crate::state::{ZOOM_STEPS as STEPS, Zoom};
use crate::theme::Tokens;
use crate::widgets::register;

/// Height of the zoom bar (points).
const BAR_H: f32 = 30.0;
/// The most the second window zooms (as the loupe).
pub const MAX_PERCENT: f32 = STEPS[STEPS.len() - 1];

/// Draw the second window (Window ▸ Second Window) when it is open, in its own native window where
/// the platform has them; closed, release its zoom window render.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.ui.second_window {
        app.second_focused = false;
        // closed: its zoom window goes (its whole-frame render stays for a quick reopen)
        if app.second_region.take().is_some() || app.renderer.textures.contains_key(&Slot::SecondRegion) {
            app.second_tiles.clear();
            app.renderer.release(Slot::SecondRegion);
        }
        return;
    }
    let vid = egui::ViewportId::from_hash_of("lightcraft-second-window");
    let builder = egui::ViewportBuilder::default().with_title("LightCraft — Second Window").with_inner_size([960.0, 640.0]);
    ctx.show_viewport_immediate(vid, builder, |ctx, class| {
        // (without native windows — web, headless — egui wraps this in a floating window, whose
        // keys belong to the main window)
        let own_keys = class != egui::ViewportClass::EmbeddedWindow;
        app.second_focused = own_keys && ctx.input(|i| i.viewport().focused.unwrap_or(false));
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ctx, |ui| body(app, ui, own_keys));
        if class != egui::ViewportClass::EmbeddedWindow && ctx.input(|i| i.viewport().close_requested()) {
            app.ui.second_window = false;
        }
    });
}

/// The zoom `zoom_in` (or out) of `zoom` goes to: the next of [`STEPS`] past what is shown (Fit and
/// Fill by the scale the window last drew them at); out past the smallest step, or to Fit's scale
/// or below, is Fit.
pub fn step(app: &LightcraftApp, zoom: Zoom, zoom_in: bool) -> Zoom {
    let (fit, fill) = app.second_scale.unwrap_or((STEPS[0], STEPS[0]));
    let shown = match zoom {
        Zoom::Fit => fit,
        Zoom::Fill => fill,
        Zoom::Percent(p) => p,
    };
    if zoom_in {
        Zoom::Percent(STEPS.iter().copied().find(|s| *s > shown * 1.001).unwrap_or(MAX_PERCENT))
    } else {
        match STEPS.iter().rev().copied().find(|s| *s < shown * 0.999) {
            Some(s) if s > fit * 1.001 => Zoom::Percent(s),
            _ => Zoom::Fit,
        }
    }
}

/// The window's contents: the zoom bar, and the active photo at the window's own zoom.
fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, own_keys: bool) {
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, Sense::hover());
    let Some(id) = app.session.active() else {
        register(ui.ctx(), "view:secondWindow", full);
        ui.painter().text(
            full.center(),
            egui::Align2::CENTER_CENTER,
            crate::i18n::tr("No photo selected"),
            egui::FontId::proportional(14.0),
            Color32::GRAY,
        );
        return;
    };
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H.min(full.height())));
    let area = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
    register(ui.ctx(), "view:secondWindow", area);
    let frame = lightcraft_pipeline::geometry::Frame::with_lens(
        photo.width.max(1) as usize,
        photo.height.max(1) as usize,
        &photo.develop,
        true,
        photo.embedded_lens.as_ref(),
    );
    let aspect = frame.aspect() as f32;
    let native = super::detail::output_px(&frame);
    let ppp = ui.ctx().pixels_per_point();
    // what Fit and Fill come to here, as percentages (for the bar, and for stepping from them)
    let fit = super::detail::fit_rect(area, aspect, Zoom::Fit, native, ppp, (0.5, 0.5));
    let percent = |r: Rect| r.width() * ppp * 100.0 / native[0].max(1) as f32;
    let fit_pct = percent(fit);
    let fill_pct = percent(super::detail::fit_rect(area, aspect, Zoom::Fill, native, ppp, (0.5, 0.5)));
    if fit_pct.is_finite() && fit_pct > 0.0 && fill_pct.is_finite() {
        app.second_scale = Some((fit_pct, fill_pct));
    }
    let now = ui.input(|i| i.time);
    zoom_bar(app, ui, bar, &photo.file_name, fit_pct, now);
    if own_keys {
        zoom_keys(app, ui);
    }

    // the texture's own aspect where it really differs from the frame's (an embedded preview with
    // another crop), as in the loupe
    let display_aspect = shown_texture(app, id)
        .map(|t| t.tex.size())
        .map(|[tw, th]| tw as f32 / th.max(1) as f32)
        .filter(|a| (a / aspect - 1.0).abs() > 0.02)
        .unwrap_or(aspect);
    // where the photo is drawn: eased toward its zoom after a click-zoom or Toggle Zoom
    let anim = egui::Id::new("second-window-anim");
    let drawn = |app: &mut LightcraftApp, ctx: &egui::Context| {
        let target = super::detail::fit_rect(area, display_aspect, app.ui.second_zoom, native, ppp, app.ui.second_pan);
        (target, super::detail::animated_rect(ctx, &mut app.ui.second_zoom_anim, target, anim))
    };

    // the wheel, a pinch, a click or a drag over the photo
    let resp = ui.interact(area, egui::Id::new("second-window-view"), Sense::click_and_drag());
    let (_, img) = drawn(app, ui.ctx());
    let larger = img.width() > area.width() + 0.5 || img.height() > area.height() + 0.5;
    if let Some((zoom, pan)) =
        super::detail::gesture_view(ui, &resp, area, img, native, app.ui.second_zoom, egui::Id::new("second-window-wheel-zoom"))
    {
        let _ = app.run("view.secondZoom", json!({"zoom": zoom, "pan": pan}));
        hold(app, ui, now);
    } else if resp.double_clicked() || (resp.clicked() && !larger) {
        // as the loupe: Fit ↔ the click-zoom ratio, around the point clicked (beside the photo: its
        // nearest edge)
        let at = resp
            .interact_pointer_pos()
            .map(|p| (((p.x - img.left()) / img.width().max(1.0)).clamp(0.0, 1.0), ((p.y - img.top()) / img.height().max(1.0)).clamp(0.0, 1.0)));
        let _ = app.run("view.secondZoom", toggle(app, at));
        app.ui.second_zoom_anim = true;
    } else if resp.clicked() && larger {
        let _ = app.run("view.secondZoom", json!({"zoom": Zoom::Fit}));
        app.ui.second_zoom_anim = true;
    } else if resp.dragged() && larger {
        let _ = app.run("view.secondZoom", json!({"pan": super::detail::panned(area, img, resp.drag_delta())}));
        hold(app, ui, now);
    }
    if larger && resp.hovered() {
        ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    // the window render is asked for at the zoom's end (one render for a whole animation); the photo
    // is drawn where the animation is
    let (img, display) = drawn(app, ui.ctx());

    // a render sized for this window, of the whole frame
    let texture_side = super::detail::texture_side(ui.ctx());
    let side = texture_side.clamp(64, 4096);
    let (w, h) = (((area.width() * ppp) as usize).clamp(64, side), ((area.height() * ppp) as usize).clamp(64, side));
    if let Some(job) = app.session.loupe_job(id, w, h, true).map(|j| app.renderer.job_for(Slot::Second, j))
        && app.renderer.textures.get(&Slot::Second).is_none_or(|t| t.key != job.key)
        && !app.renderer.is_pending(Slot::Second)
    {
        app.renderer.request(Slot::Second, job, 95);
    }
    // zoomed past it: the part on screen, as a window of a frame at the zoom's scale
    let whole_edge = ((fit.width().max(fit.height()) * ppp) as usize).min(side);
    let view = View { id, img, visible: area, aspect, native_long: native[0].max(native[1]), whole_edge, ppp, texture_side, now };
    let window = request_window(app, &view);
    app.second_region = window;
    if window.is_none() {
        app.second_tiles.clear();
        app.renderer.release(Slot::SecondRegion);
    }

    let p = ui.painter_at(area);
    if let Some(t) = shown_texture(app, id) {
        p.image(t.tex.id(), display, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        if window.is_some() {
            draw_window(&p, app, id, photo.develop.hash64(), display);
        }
        register(ui.ctx(), "second:image", display);
    }
}

/// The texture the second window shows of photo `id`: its own render, else the loupe's, else the
/// thumbnail.
fn shown_texture(app: &LightcraftApp, id: PhotoId) -> Option<&crate::render::Tex> {
    let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
    mine(Slot::Second).or_else(|| mine(Slot::Main)).or_else(|| app.renderer.thumb(id))
}

/// A gesture is running: the window there is stays (magnified) until it has been quiet a moment.
fn hold(app: &mut LightcraftApp, ui: &egui::Ui, now: f64) {
    app.second_hold_until = now + crate::region::HOLD_SECS;
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::region::HOLD_SECS));
}

/// The bar along the top: the photo's name, and − / slider / + / the zoom menu at the right.
fn zoom_bar(app: &mut LightcraftApp, ui: &mut egui::Ui, bar: Rect, name: &str, fit_pct: f32, now: f64) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(bar, 0.0, t.chrome);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.divider));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let zoom = app.ui.second_zoom;
    let label = match zoom {
        Zoom::Fit => crate::i18n::tr("Fit").to_string(),
        Zoom::Fill => crate::i18n::tr("Fill").to_string(),
        Zoom::Percent(p) => format!("{p:.0}%"),
    };
    let menu = crate::widgets::dropdown(&mut child, "secondZoom", &label, t.font(13.0), t.text_label);
    egui::Popup::menu(&menu).show(|ui| {
        // the loupe's menu, and as there a choice shows the middle of the photo
        for (text, z) in crate::state::ZOOM_MENU {
            if ui.selectable_label(zoom == z, crate::i18n::tr(text)).clicked() {
                let _ = app.run("view.secondZoom", json!({"zoom": z, "pan": (0.5, 0.5)}));
            }
        }
    });
    child.add_space(8.0);
    let at_most = matches!(zoom, Zoom::Percent(p) if p >= MAX_PERCENT);
    if crate::widgets::icon_button(&mut child, "secondZoomIn", Icon::Plus, vec2(24.0, 24.0), false, !at_most, "Zoom In").clicked() {
        let _ = app.run("view.secondZoom", json!({"step": "in"}));
    }
    // the slider: Fit at the left, 800 % at the right, on a log scale (each doubling the same step)
    let lo = if fit_pct.is_finite() && fit_pct > 0.0 { fit_pct } else { STEPS[0] };
    let span = (MAX_PERCENT / lo).ln();
    let (r, resp) = child.allocate_exact_size(vec2(120.0, 20.0), if span > 0.0 { Sense::click_and_drag() } else { Sense::hover() });
    crate::widgets::announce(&resp, crate::i18n::tr("Zoom"));
    register(child.ctx(), "slider:secondZoom", r);
    let shown = match zoom {
        Zoom::Fit => lo,
        Zoom::Fill => app.second_scale.map_or(lo, |s| s.1),
        Zoom::Percent(p) => p,
    };
    let f = if span > 0.0 { ((shown / lo).ln() / span).clamp(0.0, 1.0) } else { 0.0 };
    let p = child.painter();
    let y = r.center().y;
    p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(2.0, t.track));
    let x = r.left() + f * r.width();
    let knob = if resp.hovered() || resp.dragged() { t.text } else { t.thumb_hover };
    p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(3.0, 14.0)), 1.0, knob);
    if span > 0.0
        && let Some(pp) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let f = ((pp.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
        let z = if f <= 0.005 { Zoom::Fit } else { Zoom::Percent((lo * (f * span).exp()).clamp(1.0, MAX_PERCENT)) };
        let _ = app.run("view.secondZoom", json!({"zoom": z}));
        hold(app, &child, now);
    }
    resp.on_hover_text(crate::i18n::tr("Zoom"));
    if crate::widgets::icon_button(&mut child, "secondZoomOut", Icon::Minus, vec2(24.0, 24.0), false, zoom != Zoom::Fit, "Zoom Out").clicked() {
        let _ = app.run("view.secondZoom", json!({"step": "out"}));
    }
    // the name, in what is left on the left
    let room = Rect::from_min_max(pos2(bar.left() + 12.0, bar.top()), pos2(child.min_rect().left() - 12.0, bar.bottom()));
    if room.width() > 20.0 {
        let g = crate::widgets::one_line(ui.painter(), name, t.font(12.0), t.text_dim, room.width());
        let at = pos2(room.left(), room.center().y - g.size().y / 2.0);
        ui.painter().galley(at, g, t.text_dim);
    }
}

/// Fit ↔ the click-zoom ratio, as the loupe's Toggle Zoom: `at` is the image point clicked; from
/// the keyboard (`None`) it centres on the camera's focus point when View ▸ Zoom to Focus Point is
/// on and the photo has one, else the window keeps its pan.
pub(crate) fn toggle(app: &LightcraftApp, at: Option<(f32, f32)>) -> serde_json::Value {
    if app.ui.second_zoom == Zoom::Fit {
        let focus = || app.ui.settings.zoom_to_focus.then(|| app.session.active().and_then(|id| super::detail::focus_pan(app, id))).flatten();
        let pan = at.or_else(focus).unwrap_or(app.ui.second_pan);
        json!({"zoom": Zoom::Percent(app.ui.click_zoom as f32), "pan": pan})
    } else {
        json!({"zoom": Zoom::Fit})
    }
}

/// The loupe's zoom commands, which also zoom the second window while it has the keyboard.
const ZOOM_COMMANDS: [&str; 5] = ["view.zoom100", "view.zoomFit", "view.zoomIn", "view.zoomOut", "view.zoomToggle"];

/// What the loupe's zoom command `id` does to the second window: its `view.secondZoom` request,
/// and whether it animates (Toggle Zoom does, as in the loupe). `None`: not a zoom command.
fn zoom_request(app: &LightcraftApp, id: &str) -> Option<(serde_json::Value, bool)> {
    Some(match id {
        "view.zoom100" => (json!({"zoom": Zoom::Percent(100.0)}), false),
        "view.zoomFit" => (json!({"zoom": Zoom::Fit}), false),
        "view.zoomIn" => (json!({"step": "in"}), false),
        "view.zoomOut" => (json!({"step": "out"}), false),
        "view.zoomToggle" => (toggle(app, None), true),
        _ => return None,
    })
}

/// Run the loupe's zoom command `id` on the second window (`None`: `id` is not one of them): from
/// its keys, and from the menu bar while the second window has the keyboard.
pub(crate) fn run_zoom(app: &mut LightcraftApp, id: &str) -> Option<Result<serde_json::Value, String>> {
    let (params, animate) = zoom_request(app, id)?;
    let r = app.run("view.secondZoom", params);
    if animate && r.is_ok() {
        app.ui.second_zoom_anim = true;
    }
    Some(r)
}

/// The loupe's zoom shortcuts, as the keymap binds them (Zoom In / Out, Zoom to Fit, Zoom 100%,
/// Toggle Zoom and its Space alias), acting on this window while it has the keyboard. Keys the
/// native menu bar owns are left to it (it routes them here, see [`run_zoom`]), as in the main
/// window.
fn zoom_keys(app: &mut LightcraftApp, ui: &egui::Ui) {
    use crate::shortcuts::{ALIASES, matches, parse, shortcut_of};
    let keymap = &app.ui.settings.keymap;
    let native = &app.native_shortcuts;
    let pressed = |k: Option<(egui::Modifiers, egui::Key)>| k.is_some_and(|(m, k)| ui.input(|i| matches(i, m, k)));
    let bound = |id: &str| shortcut_of(keymap, id).filter(|sc| !native.contains(*sc)).is_some_and(|sc| pressed(parse(sc)));
    // an alias yields to a key the user gave a command, as in the main window
    let taken: Vec<_> = keymap.values().filter_map(|s| parse(s)).collect();
    let alias = |id: &str| {
        ALIASES
            .iter()
            .filter(|(sc, a, _)| *a == id && !native.contains(*sc))
            .filter_map(|(sc, ..)| parse(sc))
            .any(|k| !taken.contains(&k) && pressed(Some(k)))
    };
    let Some(id) = ZOOM_COMMANDS.into_iter().find(|id| bound(id) || alias(id)) else { return };
    let _ = run_zoom(app, id);
}

/// The second window's view, as [`request_window`] needs it.
struct View {
    id: PhotoId,
    /// The rect the photo is drawn in (the zoomed frame).
    img: Rect,
    /// The area whose pixels are on screen.
    visible: Rect,
    aspect: f32,
    /// The photo's own long edge as shown.
    native_long: usize,
    /// Long edge of the whole-frame render.
    whole_edge: usize,
    ppp: f32,
    texture_side: usize,
    /// The frame's time (s).
    now: f64,
}

/// Ask for the window of the frame that is on screen once the view is zoomed past what the
/// whole-frame render holds: cut from a frame of at most the photo's own pixels (beyond 100 % the
/// GPU magnifies it). `None`: no window is wanted (or possible), and the whole-frame render is
/// shown alone. While a gesture runs or the photo is being edited, the window there is stays.
fn request_window(app: &mut LightcraftApp, v: &View) -> Option<crate::region::RegionView> {
    let drawn_long = v.img.width().max(v.img.height()) * v.ppp;
    let frame_edge = app.ui.settings.window_frame_edge(drawn_long, v.native_long);
    if frame_edge as f32 <= v.whole_edge as f32 * crate::region::WINDOW_RATIO {
        return None;
    }
    let editing = app.session.interaction.is_some() && app.session.active() == Some(v.id);
    if app.second_hold_until > v.now || editing {
        return app.second_region.filter(|w| w.photo == v.id);
    }
    let (fw, fh) =
        if v.aspect >= 1.0 { (frame_edge as f32, frame_edge as f32 / v.aspect) } else { (frame_edge as f32 * v.aspect, frame_edge as f32) };
    let (fw, fh) = (fw.round().max(1.0) as usize, fh.round().max(1.0) as usize);
    // what is on screen, in pixels of the window's frame (the drawn frame, scaled)
    let to_px = v.ppp * frame_edge as f32 / drawn_long.max(1.0);
    let visible = (
        to_px * (v.visible.left() - v.img.left()),
        to_px * (v.visible.top() - v.img.top()),
        to_px * (v.visible.right() - v.img.left()),
        to_px * (v.visible.bottom() - v.img.top()),
    );
    let win = crate::region::window_for(fw, fh, visible, crate::region::max_span(v.texture_side))?;
    // refused: what the window reads (a spot's source, an Auto Mask stroke) is too far for one
    // render; the whole-frame render is shown alone
    let job = app.session.region_job(v.id, fw, fh, win, true)?;
    // keyed as the renderer keys the texture (a display profile changes the key)
    let job = app.renderer.job_for(Slot::SecondRegion, job);
    let look = job.settings.hash64();
    let view = crate::region::RegionView { photo: v.id, before: false, key: job.key, full: (fw, fh), window: win, settings: look };
    // windows of other photos, looks and zooms can't be drawn any more
    app.second_tiles.retain(|_, t| t.is_current(v.id, (fw, fh), look, false));
    app.second_tiles.insert(job.key, view);
    // after the whole-frame render (95): that is what a step to the next photo shows first
    app.renderer.request(Slot::SecondRegion, job, 94);
    Some(view)
}

/// Draw the window texture over the whole-frame render (`display`), if it shows this photo with
/// its current look (any look of it while it is being edited). A window made at another zoom is
/// drawn at its place in the frame, magnified or shrunk, until the new one arrives.
fn draw_window(p: &egui::Painter, app: &LightcraftApp, id: PhotoId, look: u64, display: Rect) {
    let Some(tex) = app.renderer.textures.get(&Slot::SecondRegion).filter(|t| t.photo == id) else { return };
    let editing = app.session.interaction.is_some() && app.session.active() == Some(id);
    let Some(tile) = app.second_tiles.get(&tex.key).filter(|t| t.photo == id && (editing || t.settings == look)) else { return };
    let (fw, fh) = (tile.full.0.max(1) as f32, tile.full.1.max(1) as f32);
    let at = display.min + vec2(tile.window.x as f32 / fw * display.width(), tile.window.y as f32 / fh * display.height());
    let dst = Rect::from_min_size(at, vec2(tile.window.w as f32 / fw * display.width(), tile.window.h as f32 / fh * display.height()));
    p.image(tex.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}
