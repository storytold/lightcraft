//! The vertical tool strip at the far right (Presets, Edit, Crop, Remove, Masking, Red Eye …).

use egui::vec2;
use serde_json::json;

use crate::DacApp;
use crate::icons::Icon;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::icon_button;

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
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
                ui.spacing_mut().item_spacing.y = 6.0;
                let sz = vec2(t.strip_w, 40.0);
                if icon_button(ui, "presets", Icon::Presets, sz, app.ui.presets, has_photo, "Presets (Shift+P)").clicked() {
                    let _ = app.run("panel.presets", json!({}));
                }
                // the module's right group in the user's order, then the other panels (Library offers
                // the editing tools as a way into Develop)
                let mut order = crate::module::right_group(app);
                for p in crate::module::get(crate::module::ModuleId::Develop).right_panels() {
                    if !order.contains(p) && !app.ui.hidden_panels.contains(p) {
                        order.push(*p);
                    }
                }
                let group = order.len().min(crate::module::get(app.ui.module).right_panels().len());
                for (i, p) in order.iter().enumerate() {
                    if i == group && i > 0 {
                        separator(ui, &t);
                    }
                    let Some((id, icon, panel, tip, needs_photo)) = entry(*p) else { continue };
                    let on = app.ui.right == panel || (panel == RightPanel::Edit && app.ui.right == RightPanel::Profiles);
                    if icon_button(ui, id, icon, sz, on, has_photo || !needs_photo, tip).clicked() {
                        let _ = app.run(&format!("panel.{id}"), json!({}));
                    }
                }
                separator(ui, &t);
                if icon_button(ui, "more", Icon::More, sz, false, true, "More").clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::About);
                }
            });
        });
}

/// The strip button of a right-group panel: (widget id, icon, panel, tooltip, needs a photo).
fn entry(p: crate::module::PanelId) -> Option<(&'static str, Icon, RightPanel, &'static str, bool)> {
    use crate::module::PanelId as P;
    Some(match p {
        P::Edit => ("edit", Icon::Sliders, RightPanel::Edit, "Edit", true),
        P::Crop => ("crop", Icon::Crop, RightPanel::Crop, "Crop & Rotate", true),
        P::Remove => ("remove", Icon::Eraser, RightPanel::Remove, "Remove", true),
        P::Masking => ("masking", Icon::Mask, RightPanel::Masking, "Masking", true),
        P::RedEye => ("redeye", Icon::Eye, RightPanel::RedEye, "Red Eye", true),
        P::Versions => ("versions", Icon::Versions, RightPanel::Versions, "Versions", true),
        P::Activity => ("activity", Icon::Activity, RightPanel::Activity, "History & Activity", false),
        P::Keywords => ("keywords", Icon::Tag, RightPanel::Keywords, "Keywords", true),
        P::Info => ("info", Icon::Info, RightPanel::Info, "Info", true),
        _ => return None,
    })
}

fn separator(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(t.strip_w - 16.0, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.button_border.gamma_multiply(0.7));
}
