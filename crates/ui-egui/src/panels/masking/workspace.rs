//! Detachable mask list and component selection. Layout is a UI preference, never a photo edit.
use egui::{Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::MaskShape;
use serde_json::json;

use super::{component_menu, component_row_menu, create_menu, creation_status, describe_field, kind_label, mask_menu, overlay_options};
use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{icon_button, one_line, register};
use serde_json::Value;

struct RowDrag {
    photo: PhotoId,
    mask: u32,
    component: bool,
    index: usize,
    before: std::sync::Arc<lightcraft_develop::DevelopSettings>,
}

fn drag_handle(app: &LightcraftApp, ui: &mut egui::Ui, row: Rect, mask: u32, index: usize, component: bool, enabled: bool) {
    let t = Tokens::get(ui.ctx());
    let rect = Rect::from_center_size(pos2(row.right() - 14.0, row.center().y), vec2(24.0, 24.0));
    let response = ui.interact(rect, ui.scope_id().with(("maskDrag", mask, component, index)), if enabled { Sense::drag() } else { Sense::hover() });
    register(ui, if component { format!("componentDrag:{index}") } else { format!("maskDrag:{mask}") }, rect);
    paint(ui.painter(), rect.shrink(4.0), Icon::Grip, if enabled { t.text_dim } else { t.text_disabled });
    if enabled {
        if response.drag_started()
            && let Some(photo) = app.session.active()
            && let Some(before) = app.session.develop_of(photo)
        {
            egui::DragAndDrop::set_payload(ui.ctx(), RowDrag { photo, mask, component, index, before });
        }
        response.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(crate::i18n::tr("Drag to reorder"));
    }
}

fn drop_row(app: &mut LightcraftApp, ui: &mut egui::Ui, response: &egui::Response, mask: u32, index: usize, component: bool) {
    let Some(drag) = response.dnd_hover_payload::<RowDrag>() else { return };
    if drag.component != component || (component && drag.mask != mask) || app.session.active() != Some(drag.photo) {
        return;
    }
    // An intervening edit, undo or photo switch must not redirect a stale component index.
    if app.session.develop_of(drag.photo).is_none_or(|d| d.masks != drag.before.masks) {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_hover_pos() else { return };
    let after = pointer.y > response.rect.center().y;
    let slot = index.saturating_add(usize::from(after));
    let to = slot.saturating_sub(usize::from(drag.index < slot));
    if to == drag.index {
        return;
    }
    let y = if after { response.rect.bottom() } else { response.rect.top() };
    let indicator = Rect::from_min_max(pos2(response.rect.left(), y - 1.0), pos2(response.rect.right(), y + 1.0));
    ui.painter().rect_filled(indicator, 1.0, Tokens::get(ui.ctx()).accent);
    register(ui, "maskReorderTarget", indicator);
    if response.dnd_release_payload::<RowDrag>().is_some() {
        let result = if component {
            app.run("mask.component", json!({"id": drag.mask, "component": drag.index, "action": "move", "to": to}))
        } else {
            app.run("mask.move", json!({"id": drag.mask, "to": to}))
        };
        if let Err(error) = result {
            app.toast(ui.ctx(), error);
        }
    }
}

pub fn command(app: &mut LightcraftApp, command: &str, params: &Value) -> Result<Value, String> {
    match command {
        "ui.maskPanel" => {
            let floating = params.get("floating").and_then(Value::as_bool).ok_or("floating must be a boolean")?;
            app.ui.mask_panel_floating = floating;
            Ok(json!({"floating": floating}))
        }
        "mask.selectComponent" => {
            let photo = app.session.active().ok_or("No photo selected")?;
            let mask = match params.get("id") {
                Some(value) => value.as_u64().and_then(|v| u32::try_from(v).ok()).ok_or("id must be a mask ID")?,
                None => app.session.active_mask.ok_or("No mask selected")?,
            };
            let component =
                params.get("component").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or("component must be an index")?;
            let d = app.session.develop_of(photo).unwrap_or_default();
            let shape = &d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(component)).ok_or("No such mask component")?.shape;
            let tool = tool_for(shape);
            app.session.active_mask = Some(mask);
            app.ui.mask_component = Some((photo, mask, component));
            app.ui.tool = tool.into();
            Ok(json!({"mask": mask, "component": component}))
        }
        "mask.brushSettings" => {
            let number = |key: &str, current: f32, lo: f32, hi: f32| -> Result<f32, String> {
                match params.get(key) {
                    None => Ok(current),
                    Some(v) => v
                        .as_f64()
                        .filter(|v| v.is_finite() && (*v >= lo as f64 && *v <= hi as f64))
                        .map(|v| v as f32)
                        .ok_or_else(|| format!("{key} must be between {lo} and {hi}")),
                }
            };
            let flag = |key: &str, current: bool| -> Result<bool, String> {
                params.get(key).map_or(Ok(current), |v| v.as_bool().ok_or_else(|| format!("{key} must be a boolean")))
            };
            let size = number("size", app.ui.brush_size, 0.0025, 0.25)?;
            let feather = number("feather", app.ui.brush_feather, 0.0, 100.0)?;
            let flow = number("flow", app.ui.brush_flow, 1.0, 100.0)?;
            let density = number("density", app.ui.brush_density, 0.0, 100.0)?;
            let erase = flag("erase", app.ui.brush_erase)?;
            let auto_mask = flag("autoMask", app.ui.brush_auto_mask)?;
            let pressure = flag("pressure", app.ui.brush_pressure)?;
            app.ui.brush_size = size;
            app.ui.brush_feather = feather;
            app.ui.brush_flow = flow;
            app.ui.brush_density = density;
            app.ui.brush_erase = erase;
            app.ui.brush_auto_mask = auto_mask;
            app.ui.brush_pressure = pressure;
            Ok(
                json!({"size": size, "feather": feather, "flow": flow, "density": density, "erase": erase, "autoMask": auto_mask, "pressure": pressure}),
            )
        }
        _ => Err(format!("Unknown masking UI command: {command}")),
    }
}

fn tool_for(shape: &MaskShape) -> &'static str {
    match shape {
        MaskShape::Brush { .. } => "brush",
        MaskShape::Linear { .. } => "linear",
        MaskShape::Radial { .. } => "radial",
        MaskShape::ColorRange { .. } => "colorRange",
        MaskShape::Object { .. } => "object",
        _ => "",
    }
}

pub fn component(app: &LightcraftApp, mask: &lightcraft_develop::Mask) -> Option<usize> {
    app.ui
        .mask_component
        .filter(|(photo, mid, i)| Some(*photo) == app.session.active() && *mid == mask.id && *i < mask.components.len())
        .map(|(_, _, i)| i)
        .or_else(|| mask.components.len().checked_sub(1))
}

/// Explicit row/pin selection only: a tool's fallback component is not a deletion target.
pub fn selected_component(app: &LightcraftApp) -> Option<(u32, usize)> {
    let (photo, mask, component) = app.ui.mask_component?;
    if Some(photo) != app.session.active() || Some(mask) != app.session.active_mask {
        return None;
    }
    let d = app.session.develop_of(photo)?;
    d.masks.iter().find(|m| m.id == mask)?.components.get(component)?;
    Some((mask, component))
}

/// A canvas tool only targets the selected component when its kind agrees with that tool.
pub fn tool_component(app: &LightcraftApp, tool: &str) -> Option<usize> {
    let d = app.session.develop_of(app.session.active()?)?;
    let m = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask)?;
    let i = component(app, m)?;
    (tool_for(&m.components.get(i)?.shape) == tool).then_some(i)
}

/// Brush activation shares the canvas fallback, but makes that target explicit in the list.
/// With no brush yet, clear the old row and let the first stroke create one as before.
pub fn select_brush(app: &mut LightcraftApp) -> Result<(), String> {
    let target = app.session.active().and_then(|photo| app.session.develop_of(photo)).and_then(|d| {
        let mask = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask)?;
        let index = tool_component(app, "brush").or_else(|| mask.components.iter().rposition(|c| matches!(c.shape, MaskShape::Brush { .. })))?;
        Some((mask.id, index))
    });
    if let Some((mask, index)) = target {
        command_select(app, mask, index)?;
    } else {
        app.ui.mask_component = None;
    }
    Ok(())
}

fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < to && index > from && index <= to {
        index.saturating_sub(1)
    } else if from > to && index >= to && index < from {
        index.saturating_add(1)
    } else {
        index
    }
}

/// Follow surviving rows through undo/redo of insertion, deletion and reordering.
fn history_component(before: &lightcraft_develop::Mask, after: &lightcraft_develop::Mask, selected: usize) -> usize {
    let (a, b) = (&before.components, &after.components);
    if a.len() != b.len() {
        let prefix = a.iter().zip(b).take_while(|(a, b)| a == b).count();
        if b.len() > a.len() {
            let added = b.len() - a.len();
            if selected >= prefix && a.iter().skip(prefix).eq(b.iter().skip(prefix.saturating_add(added))) {
                return selected.saturating_add(added);
            }
        } else {
            let removed = a.len() - b.len();
            if selected >= prefix && a.iter().skip(prefix.saturating_add(removed)).eq(b.iter().skip(prefix)) {
                // If the selected row itself disappeared, choose its next surviving neighbour
                // (the caller clamps at the end), rather than redirecting an earlier row.
                return selected.saturating_sub(removed).max(prefix);
            }
        }
        return selected;
    }
    let Some(from) = a.iter().zip(b).position(|(a, b)| a != b) else { return selected };
    let Some(to) = a.iter().zip(b).rposition(|(a, b)| a != b) else { return selected };
    for (source, destination) in [(from, to), (to, from)] {
        if a.iter().enumerate().all(|(i, c)| b.get(moved_index(i, source, destination)) == Some(c)) {
            return moved_index(selected, source, destination);
        }
    }
    selected
}

/// Reconcile row selection after an applied command, including edits not yet saved to disk.
pub fn selection_changed(
    app: &mut LightcraftApp,
    command: &str,
    params: &Value,
    result: &Value,
    before: Option<&lightcraft_develop::DevelopSettings>,
) {
    if !matches!(
        command,
        "mask.add"
            | "mask.addComponent"
            | "mask.brushStroke"
            | "mask.select"
            | "mask.delete"
            | "mask.deleteAll"
            | "mask.duplicate"
            | "mask.component"
            | "edit.undo"
            | "edit.redo"
    ) {
        return;
    }
    // Describe queues creation; Object can already have created a placeholder while its
    // segmentation is pending. Only the latter returns the ordinary activeMask field.
    if result.get("pending").and_then(Value::as_bool) == Some(true) && result.get("activeMask").is_none() {
        return;
    }
    let action = params.get("action").and_then(Value::as_str);
    if command == "mask.component" && !matches!(action, Some("delete" | "duplicate" | "move")) {
        return;
    }
    let affected_mask = result.get("mask").or_else(|| params.get("id")).and_then(Value::as_u64).or(app.session.active_mask.map(u64::from));
    if command == "mask.component" && action == Some("move") && affected_mask != app.session.active_mask.map(u64::from) {
        return;
    }
    let Some(photo) = app.session.active() else { return };
    let d = app.session.develop_of(photo).unwrap_or_default();
    let target = if command == "mask.addComponent" {
        params.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).or(app.session.active_mask)
    } else {
        app.session.active_mask
    };
    let Some(m) = d.masks.iter().find(|m| Some(m.id) == target) else {
        app.ui.mask_component = None;
        if app.ui.right == crate::state::RightPanel::Masking {
            app.ui.tool.clear();
        }
        return;
    };
    let previous = app.ui.mask_component.filter(|(pid, mid, _)| *pid == photo && *mid == m.id).map(|(_, _, i)| i);
    if matches!(command, "mask.select" | "mask.delete")
        || (previous.is_none() && (matches!(command, "edit.undo" | "edit.redo") || (command == "mask.component" && action == Some("delete"))))
    {
        // Parent selection stays distinct from the component whose tool/settings are shown.
        app.ui.mask_component = None;
        if app.ui.right == crate::state::RightPanel::Masking {
            app.ui.tool = m.components.last().map(|c| tool_for(&c.shape)).unwrap_or("").into();
        }
        return;
    }
    let changed = params.get("component").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok());
    let preferred = match command {
        "mask.add" | "mask.addComponent" | "mask.duplicate" => None,
        "mask.brushStroke" => changed.or_else(|| m.components.iter().rposition(|c| matches!(c.shape, MaskShape::Brush { .. }))),
        "mask.component" => match (action, changed, previous) {
            (Some("move"), _, selected) => {
                // NotSaved has no success payload, but the validated move is already applied.
                let from = result.get("from").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).or(changed);
                let to = result.get("to").or_else(|| params.get("to")).and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).or_else(|| {
                    let from = from?;
                    match params.get("delta").and_then(Value::as_i64) {
                        Some(-1) => Some(from.saturating_sub(1)),
                        Some(1) => Some(from.saturating_add(1).min(m.components.len().saturating_sub(1))),
                        _ => None,
                    }
                });
                match (from, to) {
                    (Some(from), Some(to)) => Some(selected.map_or(to, |i| moved_index(i, from, to))),
                    _ => selected,
                }
            }
            (Some("duplicate"), Some(i), _) => i.checked_add(1),
            (Some("delete"), Some(i), Some(selected)) if i < selected => selected.checked_sub(1),
            _ => previous,
        },
        "edit.undo" | "edit.redo" => {
            previous.map(|i| before.and_then(|d| d.masks.iter().find(|old| old.id == m.id)).map_or(i, |old| history_component(old, m, i)))
        }
        _ => previous,
    };
    let Some(last) = m.components.len().checked_sub(1) else { return };
    app.session.active_mask = Some(m.id);
    selection_completed(app, photo, m.id, preferred.unwrap_or(last).min(last));
}

/// Follow an applied component creation/edit without activating tools in another panel.
/// Background completion for a different photo or mask cannot redirect the current selection.
pub fn selection_completed(app: &mut LightcraftApp, photo: PhotoId, mask: u32, index: usize) {
    if app.session.active() != Some(photo) || app.session.active_mask != Some(mask) {
        return;
    }
    let tool = app.ui.tool.clone();
    let _ = command_select(app, mask, index);
    if app.ui.right != crate::state::RightPanel::Masking {
        app.ui.tool = tool;
    }
}

fn command_select(app: &mut LightcraftApp, mask: u32, component: usize) -> Result<Value, String> {
    command(app, "mask.selectComponent", &json!({"id": mask, "component": component}))
}

pub fn floating(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.ui.mask_panel_floating || app.ui.right != crate::state::RightPanel::Masking || !crate::docking::exposed(app, crate::docking::Panel::Tools)
    {
        return;
    }
    let Some(photo) = app.session.active() else { return };
    let d = app.session.develop_of(photo).unwrap_or_default();
    if d.masks.is_empty() {
        return;
    }
    let bounds = ctx.content_rect();
    let right = app.widgets.iter().find(|(id, _)| id == "panel:right_panel").map_or(bounds.right() - app.ui.right_width, |(_, r)| r.left());
    let default = [right - 312.0, bounds.top() + 80.0, 296.0, 390.0];
    let [x, y, w, h] =
        app.ui.mask_panel_rect.filter(|rect| rect.iter().all(|v| v.is_finite()) && rect[2] >= 240.0 && rect[3] >= 180.0).unwrap_or(default);
    let size = vec2(w.clamp(240.0, 520.0).min(bounds.width()), h.clamp(180.0, 800.0).min(bounds.height()));
    let pos = pos2(
        x.clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left())),
        y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top())),
    );
    let mut open = true;
    let result = egui::Window::new(crate::i18n::tr("Masks"))
        .id(egui::Id::unique("mask-list-window"))
        .current_pos(pos)
        .default_size(size)
        .min_size(vec2(240.0, 180.0))
        .max_size(vec2(520.0, 800.0))
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            list(app, ui, &d, true);
        });
    if let Some(result) = result {
        let r = result.response.rect;
        crate::widgets::register_response("panel:maskFloating", r, &result.response);
        crate::widgets::register_response("maskPanelTitle", Rect::from_min_size(r.min, vec2((r.width() - 32.0).max(1.0), 26.0)), &result.response);
        app.ui.mask_panel_rect = Some([r.left(), r.top(), r.width(), r.height()]);
    }
    if !open {
        let _ = app.run("ui.maskPanel", json!({"floating": false}));
    }
}

/// The list is shared by the embedded and floating layouts; component controls stay on the right.
pub(super) fn list(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &lightcraft_develop::DevelopSettings, floating: bool) {
    let t = Tokens::get(ui.ctx());
    let response = ui.scope(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Masks")).font(t.semibold(13.0)).color(t.text_label));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let plus = icon_button(ui, "maskCreate", Icon::Plus, vec2(24.0, 24.0), false, true, "Create New Mask");
                egui::Popup::menu(&plus).show(|ui| create_menu(app, ui));
                let all = app.ui.mask_overlay_all;
                if icon_button(ui, "maskOverlayAll", Icon::Stack, vec2(24.0, 24.0), all, true, "Show all overlays").clicked() {
                    let _ = app.run("view.maskOverlayAll", json!({}));
                }
                if icon_button(
                    ui,
                    "maskPanelArrange",
                    Icon::Sidebar,
                    vec2(24.0, 24.0),
                    floating,
                    true,
                    if floating { "Dock to panel" } else { "Float panel" },
                )
                .clicked()
                {
                    let _ = app.run("ui.maskPanel", json!({"floating": !floating}));
                }
            });
        });
        creation_status(app, ui);
        // Reserve the 24 pt footer, 6 pt separator and the two inter-item gaps.
        // Filling the entire offered content size includes the window's title spacing again
        // and causes the outer frame to grow on each repaint.
        let footer = 24.0 + 6.0 + 2.0 * ui.spacing().item_spacing.y;
        let height = if floating { (ui.available_height() - footer).max(100.0) } else { 240.0 };
        egui::ScrollArea::vertical().id_salt(("mask-list", floating)).max_height(height).auto_shrink([false, !floating]).show(ui, |ui| {
            register(ui, "maskReorderViewport", ui.clip_rect());
            for (index, mask) in d.masks.iter().enumerate() {
                ui.push_id(mask.id, |ui| mask_row(app, ui, mask, index, d.masks.len()));
            }
            if egui::DragAndDrop::has_payload_of_type::<RowDrag>(ui.ctx())
                && let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| ui.clip_rect().contains(*p))
            {
                let clip = ui.clip_rect();
                let direction = if pointer.y < clip.top() + 24.0 {
                    1.0
                } else if pointer.y > clip.bottom() - 24.0 {
                    -1.0
                } else {
                    0.0
                };
                if direction != 0.0 {
                    let delta = direction * 240.0 * ui.input(|i| i.stable_dt.min(0.05));
                    ui.scroll_with_delta_animation(vec2(0.0, delta), egui::style::ScrollAnimation::none());
                    ui.ctx().request_repaint();
                }
            }
        });
        ui.separator();
        overlay_footer(app, ui);
    });
    register(ui, if floating { "maskList:floating" } else { "maskList:embedded" }, response.response.rect);
}

fn mask_row(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &lightcraft_develop::Mask, index: usize, count: usize) {
    let t = Tokens::get(ui.ctx());
    let selected = app.session.active_mask == Some(m.id);
    let group = egui::Frame::NONE.fill(if selected { t.inset } else { egui::Color32::TRANSPARENT }).corner_radius(4.0).show(ui, |ui| {
        let renaming = app.ui.renaming_mask.as_ref().is_some_and(|(rid, _)| *rid == m.id);
        let sense = if renaming { Sense::hover() } else { Sense::click() };
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), sense);
        crate::widgets::announce_selected(&resp, m.name.clone(), selected);
        register(ui, format!("mask:{}", m.id), r);
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.5));
        }
        let thumb = Rect::from_min_size(r.min + vec2(4.0, 5.0), vec2(38.0, 38.0));
        ui.painter().rect_filled(thumb, 3.0, egui::Color32::BLACK);
        register(ui, format!("maskThumbnail:{}", m.id), thumb);
        if ui.is_rect_visible(thumb)
            && let Some(photo) = app.session.active()
            && let Some(job) = app.session.mask_thumbnail_job(photo, m.id, 96)
            && let Some(tex) = app.renderer.variant(job)
        {
            let image = super::super::detail::fit_texture_rect(thumb.shrink(3.0), tex.size);
            ui.painter().image(tex.tex.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
            register(ui, format!("maskThumbnailImage:{}", m.id), image);
        }
        ui.painter().rect_stroke(
            thumb,
            3.0,
            Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { t.accent } else { t.text_dim }),
            egui::StrokeKind::Inside,
        );
        let elided = if let Some((rid, name)) = app.ui.renaming_mask.as_mut().filter(|(rid, _)| *rid == m.id) {
            let rid = *rid;
            // Place only the name editor inside the fixed row, leaving the matte and actions visible.
            let name_rect = Rect::from_min_max(pos2(r.left() + 46.0, r.center().y - 12.0), pos2(r.right() - 76.0, r.center().y + 12.0));
            let r = ui.place(
                name_rect,
                egui::TextEdit::singleline(name)
                    .font(t.font(13.0))
                    .margin(egui::Margin::symmetric(4, 3))
                    .desired_width(name_rect.width())
                    .min_size(name_rect.size())
                    .id_salt(("maskRename", rid)),
            );
            register(ui, format!("maskRename:{rid}"), r.rect);
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.ui.renaming_mask = None;
            } else if r.lost_focus() {
                let name = name.trim().to_string();
                app.ui.renaming_mask = None;
                if !name.is_empty() && name != m.name {
                    let _ = app.run("mask.rename", json!({"id": rid, "name": name}));
                }
            }
            false
        } else {
            let color = if m.visible { t.text } else { t.text_disabled };
            let g = one_line(ui.painter(), &m.name, t.font(13.0), color, (r.width() - 126.0).max(0.0));
            let elided = g.elided;
            let at = pos2(r.left() + 50.0, r.center().y - g.size().y / 2.0);
            register(ui, format!("maskName:{}", m.id), Rect::from_min_size(at, g.size()));
            ui.painter().galley(at, g, color);
            elided
        };
        let resp = if elided { resp.on_hover_text(&m.name) } else { resp };
        let eye_rect = Rect::from_center_size(pos2(r.right() - 62.0, r.center().y), vec2(22.0, 24.0));
        let eye = ui.interact(eye_rect, ui.scope_id().with("visible"), Sense::click());
        crate::widgets::announce_selected(&eye, crate::i18n::tr("Show mask"), m.visible);
        register(ui, format!("maskVisible:{}", m.id), eye_rect);
        paint(ui.painter(), eye_rect.shrink(4.0), if m.visible { Icon::Eye } else { Icon::EyeOff }, t.text_dim);
        if eye.clicked() {
            let _ = app.run("mask.visible", json!({"id": m.id}));
        }
        let more_rect = eye_rect.translate(vec2(24.0, 0.0));
        let more = ui.interact(more_rect, ui.scope_id().with("options"), Sense::click());
        register(ui, format!("maskMenu:{}", m.id), more_rect);
        paint(ui.painter(), more_rect.shrink(4.0), Icon::More, t.text_dim);
        egui::Popup::menu(&more).show(|ui| mask_menu(app, ui, m.id, &m.name, m.visible, index, count));
        drag_handle(app, ui, r, m.id, index, false, !renaming && count > 1);
        if !renaming {
            if resp.clicked() {
                let _ = app.run("mask.select", json!({"id": m.id}));
            }
            if resp.double_clicked() {
                app.ui.renaming_mask = Some((m.id, m.name.clone()));
            }
            crate::widgets::context_menu(&resp, |ui| mask_menu(app, ui, m.id, &m.name, m.visible, index, count));
        }
        if selected {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 6, top: 0, bottom: 6 }).show(ui, |ui| {
                for (i, c) in m.components.iter().enumerate() {
                    component_row(app, ui, m, i, c);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let size = vec2(((ui.available_width() - 8.0) / 2.0).max(1.0), 32.0);
                    for (op, label, key, alias) in [("add", "Add", "maskPlus", "maskAddComp"), ("subtract", "Subtract", "maskMinus", "maskSubComp")] {
                        let label = crate::i18n::tr(label);
                        let text = one_line(ui.painter(), label, t.semibold(12.5), t.text, (size.x - 16.0).max(0.0));
                        let button = ui
                            .add_sized(size, egui::Button::new(text).stroke(Stroke::new(1.0, t.button_border)).corner_radius(4.0))
                            .on_hover_text(label);
                        register(ui, format!("button:{key}:{}", m.id), button.rect);
                        register(ui, format!("button:{alias}"), button.rect);
                        egui::Popup::menu(&button).show(|ui| component_menu(app, ui, m.id, op));
                    }
                });
                describe_field(app, ui, false);
            });
        }
    });
    register(ui, format!("maskGroup:{}", m.id), group.response.rect);
    drop_row(app, ui, &group.response, m.id, index, false);
}

fn component_row(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &lightcraft_develop::Mask, i: usize, c: &lightcraft_develop::MaskComponent) {
    let t = Tokens::get(ui.ctx());
    let (kind, icon) = kind_label(&c.shape);
    let label = c.name.clone().unwrap_or_else(|| match &c.shape {
        MaskShape::Prompt { text, .. } => format!("“{text}”"),
        _ => {
            let number = m.components.iter().take(i.saturating_add(1)).filter(|c| kind_label(&c.shape).0 == kind).count();
            format!("{} {number}", crate::i18n::tr(kind))
        }
    });
    let selected = selected_component(app) == Some((m.id, i));
    let renaming = app.ui.renaming_component.as_ref().is_some_and(|(mid, k, _)| *mid == m.id && *k == i);
    let sense = if renaming { Sense::hover() } else { Sense::click() };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), sense);
    crate::widgets::announce_selected(&resp, label.clone(), selected);
    register(ui, format!("component:{i}"), r);
    ui.painter().rect_filled(
        r,
        3.0,
        if selected {
            t.tool_active
        } else if resp.hovered() {
            t.hover
        } else {
            egui::Color32::TRANSPARENT
        },
    );
    let icon_rect = Rect::from_min_size(r.min + vec2(5.0, 6.0), vec2(20.0, 20.0));
    paint(ui.painter(), icon_rect, icon, t.text_label);
    register(ui, format!("maskComponentIcon:{i}"), icon_rect);
    let operation = match c.op {
        lightcraft_develop::MaskOp::Add => None,
        lightcraft_develop::MaskOp::Subtract => Some(("−", "Subtract")),
        lightcraft_develop::MaskOp::Intersect => Some(("∩", "Intersect")),
    };
    let mut text_left = r.left() + 32.0;
    if let Some((symbol, _)) = operation {
        let badge = Rect::from_min_size(pos2(text_left, r.center().y - 8.0), vec2(16.0, 16.0));
        ui.painter().rect_stroke(badge, 3.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
        ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, symbol, t.font(12.5), t.text_label);
        register(ui, format!("maskComponentOp:{i}"), badge);
        text_left = badge.right() + 6.0;
    }
    let text = format!("{label}{}", if c.invert { crate::i18n::tr(" (inverted)") } else { "" });
    if let Some((_, _, name)) = app.ui.renaming_component.as_mut().filter(|(mid, k, _)| *mid == m.id && *k == i) {
        // Keep the row, kind icon, operation badge and menu in place; only the name is editable.
        let name_rect = Rect::from_min_max(pos2(text_left - 4.0, r.center().y - 12.0), pos2(r.right() - 54.0, r.center().y + 12.0));
        let r = ui.place(
            name_rect,
            egui::TextEdit::singleline(name)
                .font(t.font(12.5))
                .margin(egui::Margin::symmetric(4, 3))
                .desired_width(name_rect.width())
                .min_size(name_rect.size())
                .id_salt(("compRename", m.id, i)),
        );
        register(ui, format!("componentRename:{i}"), r.rect);
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
        }
        if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
            app.ui.renaming_component = None;
        } else if r.lost_focus() {
            let name = name.trim().to_string();
            app.ui.renaming_component = None;
            if Some(&name) != c.name.as_ref() {
                let _ = app.run("mask.component", json!({"id": m.id, "component": i, "action": "rename", "name": name}));
            }
        }
    } else {
        let g = one_line(ui.painter(), &text, t.font(12.5), t.text_label, (r.right() - 54.0 - text_left).max(0.0));
        ui.painter().galley(pos2(text_left, r.center().y - g.size().y / 2.0), g, t.text_label);
    }
    let more_rect = Rect::from_center_size(pos2(r.right() - 38.0, r.center().y), vec2(24.0, 24.0));
    let more = ui.interact(more_rect, ui.scope_id().with(("componentMenu", i)), Sense::click());
    register(ui, format!("button:componentMenu{i}"), more_rect);
    paint(ui.painter(), more_rect.shrink(4.0), Icon::More, t.text_dim);
    egui::Popup::menu(&more).show(|ui| component_row_menu(app, ui, m.id, i, &label, m.components.len()));
    drag_handle(app, ui, r, m.id, i, true, !renaming && m.components.len() > 1);
    drop_row(app, ui, &resp, m.id, i, true);
    if !renaming {
        if resp.clicked() {
            let _ = app.run("mask.selectComponent", json!({"id": m.id, "component": i}));
        }
        if resp.double_clicked() {
            app.ui.renaming_component = Some((m.id, i, label.clone()));
        }
        let tooltip = operation.map_or_else(|| text.clone(), |(_, op)| format!("{}: {text}", crate::i18n::tr(op)));
        crate::widgets::context_menu(&resp.on_hover_text(tooltip), |ui| component_row_menu(app, ui, m.id, i, &label, m.components.len()));
    }
}

fn overlay_footer(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let mut overlay = app.ui.mask_overlay;
        let t = Tokens::get(ui.ctx());
        let label = crate::i18n::tr("Show overlay (O)");
        let galley = one_line(ui.painter(), label, t.font(12.5), t.text_label, (ui.available_width() - 84.0).max(0.0));
        let check = ui.checkbox(&mut overlay, galley).on_hover_text(label);
        register(ui, "icon:maskOverlay", check.rect);
        if check.changed() {
            let _ = app.run("view.maskOverlay", json!({"show": overlay}));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let options = icon_button(ui, "maskOverlayOptions", Icon::More, vec2(24.0, 24.0), false, true, "Overlay");
            egui::Popup::menu(&options).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_width(250.0);
                overlay_options(app, ui);
            });
            let (r, swatch) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
            register(ui, "maskOverlaySwatch", r);
            let [red, green, blue] = app.ui.mask_overlay_color;
            ui.painter().circle_filled(r.center(), 8.0, egui::Color32::from_rgb(red, green, blue));
            ui.painter().circle_stroke(r.center(), 8.0, Stroke::new(1.0, Tokens::get(ui.ctx()).text_dim));
            egui::Popup::menu(&swatch).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_width(250.0);
                overlay_options(app, ui);
            });
        });
    });
}
