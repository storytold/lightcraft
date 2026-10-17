//! The vertical tool strip at the far right: Develop's editing tools (Presets, Edit, Crop, Remove,
//! Masking, Red Eye) and Library's own panels (Versions, History & Activity), each with More below.

use egui::vec2;
use serde_json::json;

use crate::LccApp;
use crate::icons::Icon;
use crate::module::{ModuleId, PanelId};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::icon_button;

/// Height of one tool button.
const BUTTON_H: f32 = 40.0;
/// Space between tool buttons.
const GAP: f32 = 6.0;

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::right("tool_strip")
        .exact_size(t.strip_w)
        .resizable(false)
        .frame(
            egui::Frame::NONE
                .fill(t.chrome)
                .inner_margin(egui::Margin { left: 0, right: 0, top: 8, bottom: 8 })
                .stroke(egui::Stroke::new(1.0, t.divider)),
        )
        .show(ui, |ui| {
            let has_photo = app.session.active().is_some();
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                let sz = vec2(t.strip_w, BUTTON_H);
                // the Presets column: Develop's, as in Lightroom Classic
                if app.ui.module == ModuleId::Develop && icon_button(ui, "presets", Icon::Presets, sz, app.ui.presets, has_photo, "Presets").clicked()
                {
                    let _ = app.run("panel.presets", json!({}));
                }
                // Only the active module's tools: Develop's editing tools, Library's own panels
                // (`Library`'s Versions and History & Activity). The Library panels sat in Develop's
                // strip as if they were Develop's tools (#108), so each module declares its own.
                let side = strip_panels(app);
                for p in &side {
                    let Some((id, icon, panel, tip, needs_photo)) = entry(*p) else { continue };
                    let on = app.ui.right == panel || (panel == RightPanel::Edit && app.ui.right == RightPanel::Profiles);
                    let video = crate::video_ui::blocked(app, &format!("panel.{id}"));
                    let tip = if video { crate::video_ui::NOT_FOR_VIDEO } else { tip };
                    let resp = icon_button(ui, id, icon, sz, on, (has_photo || !needs_photo) && !video, tip);
                    // The tool in use is radio-style (Lightroom Classic): in Develop, clicking it
                    // leaves it alone. It used to close the panel, and a Detail view with no editing
                    // panel is Library's, so the click threw the user back into the grid. Library's
                    // strip is the tab strip over its Classic column, where a click on the tab that
                    // is open closes it and the column comes back (as `panel.<id>` does).
                    if resp.clicked() && (app.ui.module == ModuleId::Library || app.ui.right != panel) {
                        let _ = app.run(&format!("panel.{id}"), json!({}));
                    }
                    // the same header menu as the Classic columns: show/hide, solo, hiding mode
                    crate::panels::classic::side_menu(app, &resp, strip_declared(app.ui.module));
                }
                separator(ui, &t);
                if icon_button(ui, "more", Icon::More, sz, false, true, "More").clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::About);
                }
            });
        });
}

/// The panels a module's tool strip lists. Develop's are its editing tools; Library's are its own
/// panels, which open over its Classic right column. The other modules have no strip.
pub fn strip_declared(module: ModuleId) -> &'static [PanelId] {
    match module {
        ModuleId::Develop => &[PanelId::Edit, PanelId::Crop, PanelId::Remove, PanelId::Masking, PanelId::RedEye],
        ModuleId::Library => &[PanelId::Versions, PanelId::Activity],
        _ => &[],
    }
}

/// The active module's strip panels, in the user's order and without hidden ones.
pub fn strip_panels(app: &LccApp) -> Vec<PanelId> {
    let declared = strip_declared(app.ui.module);
    let mut out: Vec<PanelId> = app.ui.panel_order.iter().copied().filter(|p| declared.contains(p)).collect();
    for p in declared {
        if !out.contains(p) {
            out.push(*p);
        }
    }
    out.retain(|p| !app.ui.hidden_panels.contains(p));
    out
}

/// The strip button of a strip panel: (widget id, icon, panel, tooltip, needs a photo).
fn entry(p: PanelId) -> Option<(&'static str, Icon, RightPanel, &'static str, bool)> {
    use crate::module::PanelId as P;
    Some(match p {
        P::Edit => ("edit", Icon::Sliders, RightPanel::Edit, "Edit", true),
        P::Crop => ("crop", Icon::Crop, RightPanel::Crop, "Crop & Rotate", true),
        P::Remove => ("remove", Icon::Eraser, RightPanel::Remove, "Remove", true),
        P::Masking => ("masking", Icon::Mask, RightPanel::Masking, "Masking", true),
        P::RedEye => ("redeye", Icon::RedEye, RightPanel::RedEye, "Red Eye", true),
        P::Versions => ("versions", Icon::Versions, RightPanel::Versions, "Versions (Shift+V)", true),
        P::Activity => ("activity", Icon::Activity, RightPanel::Activity, "History & Activity", false),
        _ => return None,
    })
}

fn separator(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(t.strip_w - 16.0, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.button_border.gamma_multiply(0.7));
}
