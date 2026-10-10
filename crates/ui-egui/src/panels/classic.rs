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
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    register(ui.ctx(), format!("classicPanel:{}", p.key()), r);
    if resp.clicked() {
        toggle(app, p);
    }
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

/// The header context menu: every panel of the side with a check (show/hide), and Solo Mode.
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
