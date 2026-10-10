//! Classic panel columns: a module side drawn as a stack of collapsible panels (Library's
//! Navigator / Catalog / Folders / Collections on the left; Quick Develop / Keywording / Keyword
//! List / Metadata on the right). Each panel has a header that folds it; Solo Mode (`panel.solo`)
//! keeps one open per side; the header's context menu shows or hides panels (`panel.show`), and
//! the order is the user's (`panel.order`). Open/closed state is remembered in `ui.json`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::module::PanelId;
use crate::theme::Tokens;
use crate::widgets::register;

fn state_key(p: PanelId) -> String {
    format!("panel:{}", p.key())
}

/// Whether a Classic panel is unfolded.
pub fn is_open(app: &DacApp, p: PanelId) -> bool {
    !app.ui.sidebar_section_collapsed(&state_key(p))
}

/// Fold or unfold a Classic panel; in Solo Mode, unfolding one folds the others on its side.
pub fn set_open(app: &mut DacApp, p: PanelId, open: bool) {
    if open == is_open(app, p) {
        return;
    }
    if open && app.ui.single_panel {
        let m = crate::module::get(app.ui.module);
        let side: &[PanelId] = if m.left_panels().contains(&p) { m.left_panels() } else { m.right_panels() };
        for other in side.iter().filter(|o| **o != p) {
            if is_open(app, *other) {
                app.ui.toggle_sidebar_section(&state_key(*other));
            }
        }
    }
    app.ui.toggle_sidebar_section(&state_key(p));
}

pub fn toggle(app: &mut DacApp, p: PanelId) {
    let open = is_open(app, p);
    set_open(app, p, !open);
}

/// The panels of one side, in the user's order, without hidden ones.
pub fn ordered(app: &DacApp, declared: &[PanelId]) -> Vec<PanelId> {
    let mut out: Vec<PanelId> = app.ui.panel_order.iter().copied().filter(|p| declared.contains(p)).collect();
    for p in declared {
        if !out.contains(p) {
            out.push(*p);
        }
    }
    out.retain(|p| !app.ui.hidden_panels.contains(p));
    out
}

/// The panel's title.
pub fn title(p: PanelId) -> &'static str {
    match p {
        PanelId::Navigator => "Navigator",
        PanelId::Catalog => "Catalog",
        PanelId::Folders => "Folders",
        PanelId::Collections => "Collections",
        PanelId::QuickDevelop => "Quick Develop",
        PanelId::Keywording => "Keywording",
        PanelId::KeywordList => "Keyword List",
        PanelId::Metadata => "Metadata",
        PanelId::Sources => "Sources",
        PanelId::Presets => "Presets",
        PanelId::Edit => "Edit",
        PanelId::Crop => "Crop",
        PanelId::Remove => "Remove",
        PanelId::Masking => "Masking",
        PanelId::RedEye => "Red Eye",
        PanelId::Versions => "Versions",
        PanelId::Activity => "Activity",
        PanelId::Keywords => "Keywords",
        PanelId::Info => "Info",
    }
}

/// A Classic panel header: a disclosure triangle and the title, the full width; a click folds the
/// panel, the context menu lists the side's panels (show/hide) and Solo Mode. Returns the header
/// rect and whether the panel is open. `widget id`: `classicPanel:<key>`.
pub fn header(app: &mut DacApp, ui: &mut egui::Ui, p: PanelId, side: &[PanelId]) -> (Rect, bool) {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click_and_drag());
    register(ui.ctx(), format!("classicPanel:{}", p.key()), r);
    if resp.clicked() {
        toggle(app, p);
    }
    drag_to_reorder(app, ui, p, side, r, &resp);
    let open = is_open(app, p);
    let name = crate::i18n::tr(title(p));
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, name));
    let painter = ui.painter();
    painter.line_segment([pos2(r.left(), r.top()), pos2(r.right(), r.top())], Stroke::new(1.0, t.divider));
    let col = if resp.hovered() { t.text } else { t.text_dim };
    // at the visible edge when the column scrolls sideways
    let right = ui.clip_rect().right().min(r.right());
    triangle(painter, pos2(right - 16.0, r.center().y), open, col);
    painter.text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, name, t.semibold(12.5), t.text);
    side_menu(app, &resp, side);
    (r, open)
}

/// Where each header of a side was drawn last frame (for dropping a dragged header).
fn rect_id(p: PanelId) -> egui::Id {
    egui::Id::new(("classic-header-rect", p.key()))
}

/// Drag a header up or down its side to move the panel: an insertion line shows where it lands;
/// on release the side's new order goes through `panel.order`.
fn drag_to_reorder(app: &mut DacApp, ui: &egui::Ui, p: PanelId, side: &[PanelId], r: Rect, resp: &egui::Response) {
    ui.data_mut(|d| d.insert_temp(rect_id(p), r));
    if !(resp.dragged() || resp.drag_stopped()) {
        return;
    }
    let Some(y) = ui.ctx().pointer_latest_pos().map(|pos| pos.y) else { return };
    let shown = ordered(app, side);
    let rects: Vec<(PanelId, Rect)> = shown.iter().filter_map(|q| ui.data(|d| d.get_temp::<Rect>(rect_id(*q))).map(|r| (*q, r))).collect();
    // the slot: before the first header whose middle is below the pointer, else at the end
    let slot = rects.iter().position(|(_, r)| y < r.center().y).unwrap_or(rects.len());
    if resp.dragged() {
        let t = Tokens::get(ui.ctx());
        let line_y = rects.get(slot).map(|(_, r)| r.top()).or_else(|| rects.last().map(|(_, r)| r.bottom())).unwrap_or(r.top());
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("classic-reorder")));
        painter.line_segment([pos2(r.left(), line_y), pos2(r.right(), line_y)], Stroke::new(2.0, t.accent));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        return;
    }
    let names: Vec<PanelId> = rects.iter().map(|(q, _)| *q).collect();
    if let Some(order) = reorder(&names, p, slot) {
        // the other sides' panels keep their place in the saved order
        let mut full: Vec<String> = order.iter().map(|q| q.key()).collect();
        full.extend(app.ui.panel_order.iter().filter(|q| !side.contains(q)).map(|q| q.key()));
        let _ = app.run("panel.order", json!({"order": full}));
    }
}

/// `list` with `p` moved to `slot` (an index into `list` before the move); `None` when it
/// doesn't move or isn't there.
pub fn reorder(list: &[PanelId], p: PanelId, slot: usize) -> Option<Vec<PanelId>> {
    let from = list.iter().position(|q| *q == p)?;
    let slot = slot.min(list.len());
    if slot == from || slot == from + 1 {
        return None;
    }
    let mut out = list.to_vec();
    out.remove(from);
    let at = if slot > from { slot - 1 } else { slot };
    out.insert(at.min(out.len()), p);
    Some(out)
}

/// The window edge a side's panels sit on.
fn side_edge(side: &[PanelId]) -> crate::module::Edge {
    let left = side.first().is_some_and(|p| crate::module::LIBRARY_LEFT.contains(p) || matches!(p, PanelId::Sources | PanelId::Presets));
    if left { crate::module::Edge::Left } else { crate::module::Edge::Right }
}

/// The header context menu: every panel of the side with a check (show/hide), Solo Mode, expand /
/// collapse all, and the side's hiding mode (manual, auto hide, auto hide & show).
pub fn side_menu(app: &mut DacApp, resp: &egui::Response, side: &[PanelId]) {
    resp.context_menu(|ui| {
        for q in side {
            let mut shown = !app.ui.hidden_panels.contains(q);
            if ui.checkbox(&mut shown, crate::i18n::tr(title(*q))).changed() {
                let _ = app.run("panel.show", json!({"panel": q.key(), "visible": shown}));
            }
        }
        ui.separator();
        let mut solo = app.ui.single_panel;
        if ui.checkbox(&mut solo, crate::i18n::tr("Solo Mode")).changed() {
            let _ = app.run("panel.solo", json!({"on": solo}));
        }
        if ui.button(crate::i18n::tr("Expand All Panels")).clicked() {
            for q in side {
                set_open(app, *q, true);
            }
        }
        if ui.button(crate::i18n::tr("Collapse All Panels")).clicked() {
            for q in side {
                set_open(app, *q, false);
            }
        }
        ui.separator();
        let e = side_edge(side);
        let edge = e.key();
        let (hide, show) = (app.ui.auto_hide.get(e), app.ui.auto_show.get(e));
        if ui.radio(!hide && !show, crate::i18n::tr("Manual")).clicked() {
            let _ = app.run("panel.autoShow", json!({"edge": edge, "on": false}));
            let _ = app.run("panel.autoHide", json!({"edge": edge, "on": false}));
        }
        if ui.radio(hide, crate::i18n::tr("Auto Hide")).clicked() {
            let _ = app.run("panel.autoHide", json!({"edge": edge, "on": true}));
        }
        if ui.radio(show, crate::i18n::tr("Auto Hide & Show")).clicked() {
            let _ = app.run("panel.autoShow", json!({"edge": edge, "on": true}));
        }
    });
}

/// A small disclosure triangle (down when `open`), painted.
pub fn triangle(p: &egui::Painter, c: egui::Pos2, open: bool, col: Color32) {
    let pts = if open {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// Library's right column: the Classic panels stacked, each under its header.
pub fn right_column(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    let reserve = if app.ui.left_panel { crate::state::LEFT_WIDTH.min } else { 0.0 };
    let width = app.ui.right_width;
    let declared = crate::module::get(app.ui.module).right_panels();
    let resized = super::resizable_side(ui, false, "right_panel", frame, width, crate::state::RIGHT_WIDTH, reserve, |ui| {
        egui::ScrollArea::vertical().id_salt("classic-right-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for p in ordered(app, declared) {
                let (_, open) = header(app, ui, p, declared);
                if open {
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 12, top: 4, bottom: 10 }).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        body(app, ui, p);
                    });
                }
            }
        });
    });
    if let Some(w) = resized {
        app.ui.right_width = w;
    }
}

/// A right-column panel's contents.
fn body(app: &mut DacApp, ui: &mut egui::Ui, p: PanelId) {
    match p {
        PanelId::QuickDevelop => super::quick_develop::show(app, ui),
        PanelId::Keywording => super::keywording::show(app, ui),
        PanelId::KeywordList => super::keyword_list::show(app, ui),
        PanelId::Metadata => super::right::metadata_panel(app, ui),
        _ => {}
    }
}
