use super::geometry::CanvasMap;
use super::retouch::{SPOT_BODIES, SpotBodies, brush_circles, feather_band, grab_radius, spot_band, spot_under};
use super::{Color32, LightcraftApp, Pos2, Rect, RightPanel, json, vec2};
use crate::state::ViewMode;

// ------------------------------------------------------------------------ brush by mouse

/// The brush the pointer paints with on the photo: the Remove tool's, or the Masking brush.
fn brush_in_use(app: &LightcraftApp, right: RightPanel) -> bool {
    right == RightPanel::Remove || (right == RightPanel::Masking && app.ui.tool == "brush")
}

/// The mouse wheel over the photo resizes the brush in use for the next stroke (up: larger, ×1.1 a
/// notch) and Shift+wheel feathers it (+5 a notch). Over a Remove spot it resizes and feathers that
/// spot instead (selecting it), so a stroke just painted can still be adjusted; elsewhere it lets go
/// of the selected spot, whose settings the brush otherwise follows.
/// As in Lightroom, a smooth-scrolling mouse and a two-finger trackpad swipe do the same (a line's
/// worth of points a notch) instead of zooming or panning; pinch and ⌘-wheel still zoom.
pub(super) fn brush_wheel(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, right: RightPanel) {
    let id = egui::Id::unique("brush-wheel");
    let now = ui.input(|i| i.time);
    // the spot under the pointer now (last frame's outlines, this frame's pointer: it may have moved with the wheel)
    // with each spot's radius as it is now: the wheel may have just resized it
    let hovered = ui
        .data(|d| d.get_temp::<SpotBodies>(egui::Id::unique(SPOT_BODIES)))
        .filter(|_| right == RightPanel::Remove)
        .and_then(|mut b| {
            let spots = app.session.active().and_then(|p| app.session.develop_of(p)).map(|d| d.spots.clone()).unwrap_or_default();
            for (spot, _, _, r) in &mut b.bodies {
                if let Some(s) = spots.get(*spot) {
                    *r = grab_radius(s, b.long);
                }
            }
            spot_under(&b.bodies, b.active, resp.hover_pos()?)
        })
        .map(|(index, ..)| index);
    let in_use = brush_in_use(app, right) && resp.hovered();
    // one wheel gesture on a spot is one undo step: it ends after a pause, off that spot, or when the tool goes
    if let Some((spot, last)) = ui.data(|d| d.get_temp::<(usize, f64)>(egui::Id::unique(WHEEL_EDIT))) {
        let pressed = ui.input(|i| i.pointer.any_down());
        if !in_use || pressed || hovered != Some(spot) || now - last > WHEEL_EDIT_PAUSE {
            end_wheel_edit(app, ui.ctx());
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_EDIT_PAUSE));
        }
    }
    if !in_use {
        ui.data_mut(|d| d.remove_temp::<bool>(id));
        return;
    }
    let line = ui.ctx().options(|o| o.input_options.line_scroll_speed).max(1.0);
    let ours = |e: &egui::Event| match e {
        egui::Event::MouseWheel { unit, delta, modifiers, .. } if delta.is_finite() => {
            // macOS reports most mice in points (Magic Mouse, smooth-scrolling wheels), a notched wheel in lines
            let notches = if *unit == egui::MouseWheelUnit::Point { *delta / line } else { *delta };
            if *modifiers == egui::Modifiers::NONE {
                Some((notches.y, 0.0))
            } else if *modifiers == egui::Modifiers::SHIFT {
                // some platforms turn Shift+wheel into a sideways scroll
                Some((0.0, notches.y + notches.x))
            } else {
                None
            }
        }
        _ => None,
    };
    let (size, feather) = ui.input(|i| i.events.iter().filter_map(ours).fold((0.0f32, 0.0f32), |a, b| (a.0 + b.0, a.1 + b.1)));
    if size != 0.0 || feather != 0.0 {
        ui.input_mut(|i| i.events.retain(|e| ours(e).is_none()));
        ui.data_mut(|d| d.insert_temp(id, true));
        let (k, df) = (1.1f32.powf(size.clamp(-10.0, 10.0)), 5.0 * feather.clamp(-10.0, 10.0));
        match hovered {
            Some(index) => {
                // the spot as it is now: the overlay saw it before this frame's edits
                let current =
                    app.session.active().and_then(|p| app.session.develop_of(p)).and_then(|d| d.spots.get(index).map(|s| (s.size, s.feather)));
                let Some((size, feather)) = current else { return };
                if app.session.active_spot != Some(index) {
                    let _ = app.run("spot.select", json!({"index": index}));
                }
                if ui.data(|d| d.get_temp::<(usize, f64)>(egui::Id::unique(WHEEL_EDIT))).is_none() {
                    let _ = app.run("develop.beginInteraction", json!({"label": "Edit Spot"}));
                }
                ui.data_mut(|d| d.insert_temp(egui::Id::unique(WHEEL_EDIT), (index, now)));
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_EDIT_PAUSE));
                let mut p = json!({"index": index});
                if k != 1.0 {
                    p["size"] = json!((size * k as f64).clamp(0.001, 0.25));
                }
                if df != 0.0 {
                    p["feather"] = json!((feather + df as f64).clamp(0.0, 100.0));
                }
                let _ = app.run("spot.update", p);
            }
            None => {
                deselect_spot(app, right);
                let _ = crate::menus::resize_brush(app, k, df);
            }
        }
    }
    // egui glides a wheel's scroll over the next frames: none of it may pan or zoom the photo
    if ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        if ui.input(|i| i.smooth_scroll_delta == egui::Vec2::ZERO) {
            ui.data_mut(|d| d.remove_temp::<bool>(id));
        }
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
}

/// The spot a wheel gesture is editing and when it last turned, as `(index, time)`; its edits are one undo step.
const WHEEL_EDIT: &str = "brush-wheel-edit";
/// Seconds without a turn that end a wheel gesture.
const WHEEL_EDIT_PAUSE: f64 = 0.5;

/// Commits a wheel gesture's spot edits as one undo step.
fn end_wheel_edit(app: &mut LightcraftApp, ctx: &egui::Context) {
    if ctx.data_mut(|d| d.remove_temp::<(usize, f64)>(egui::Id::unique(WHEEL_EDIT))).is_some() && app.session.interaction.is_some() {
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// Every frame, whatever is drawn: ends a wheel gesture whose loupe is gone (another view, another
/// tool), which `brush_wheel` no longer runs to end, or that has paused, so no later edit joins it.
pub(crate) fn settle_wheel_edit(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some((_, last)) = ctx.data(|d| d.get_temp::<(usize, f64)>(egui::Id::unique(WHEEL_EDIT))) else { return };
    let drawn = app.ui.view == ViewMode::Detail && app.ui.right == RightPanel::Remove && !app.ui.fullscreen;
    if !drawn || ctx.input(|i| i.time) - last > WHEEL_EDIT_PAUSE {
        end_wheel_edit(app, ctx);
    }
}

/// Lets go of the selected Remove spot, so the brush (which shows a selected spot's settings) is the next stroke's alone.
fn deselect_spot(app: &mut LightcraftApp, right: RightPanel) {
    if right == RightPanel::Remove && app.session.active_spot.is_some() {
        let _ = app.run("spot.select", json!({"index": null}));
    }
}

/// ⌃⌥-drag (or ⌥-right-drag) on the photo sets the brush in use for the next stroke the way
/// Photoshop does: across for the size, up and down for the feather (down: softer), shown on the
/// circle where the drag started. True while it owns the pointer (nothing is painted).
pub(super) fn brush_drag(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, right: RightPanel, map: &CanvasMap) -> bool {
    let id = egui::Id::unique("brush-resize");
    if !brush_in_use(app, right) {
        ui.data_mut(|d| d.remove_temp::<(Pos2, f32, f32)>(id));
        return false;
    }
    let remove = right == RightPanel::Remove;
    let current = |app: &LightcraftApp| {
        if remove { (app.ui.remove_size, app.ui.remove_feather) } else { (app.ui.brush_size, app.ui.brush_feather) }
    };
    let (mods, secondary) = ui.input(|i| (i.modifiers, i.pointer.secondary_down()));
    if resp.drag_started()
        && ((mods.ctrl && mods.alt) || (mods.alt && secondary))
        && let Some(o) = ui.input(|i| i.pointer.press_origin())
    {
        deselect_spot(app, right);
        let (s0, f0) = current(app);
        ui.data_mut(|d| d.insert_temp(id, (o, s0, f0)));
    }
    let Some((o, s0, f0)) = ui.data(|d| d.get_temp::<(Pos2, f32, f32)>(id)) else { return false };
    if let Some(q) = ui.input(|i| i.pointer.latest_pos()) {
        let dv = q - o;
        let (s, f) = current(app);
        let size = s0 * (dv.x / 200.0).exp();
        let feather = (f0 + dv.y * 0.3).clamp(0.0, 100.0);
        if size != s || feather != f {
            let _ = crate::menus::resize_brush(app, size / s.max(1e-6), feather - f);
        }
    }
    let (s, f) = current(app);
    let long = map.rect.width().max(map.rect.height());
    let r = s * long;
    let p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    let band = if remove { spot_band(feather_band(app.ui.tool == "fill", r, f)) } else { (r * (1.0 - f / 100.0), r) };
    brush_circles(&p, o, band);
    let label = format!("{} {:.0} · {} {:.0}", crate::i18n::tr("Size"), s * 1000.0, crate::i18n::tr("Feather"), f);
    let at = o + vec2(0.0, r + 14.0);
    let galley = p.layout_no_wrap(label, egui::FontId::proportional(12.0), Color32::WHITE);
    let rect = Rect::from_center_size(at, galley.size() + vec2(10.0, 4.0));
    p.rect_filled(rect, 4.0, Color32::from_black_alpha(160));
    p.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    if !ui.input(|i| i.pointer.any_down()) {
        ui.data_mut(|d| d.remove_temp::<(Pos2, f32, f32)>(id));
    }
    true
}
