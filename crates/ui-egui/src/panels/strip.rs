//! The vertical tool strip at the far right (Presets, Edit, Crop, Remove, Masking, Red Eye …).

use egui::vec2;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::icon_button;

/// Height of one tool button.
const BUTTON_H: f32 = 40.0;
/// Space between tool buttons.
const GAP: f32 = 6.0;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
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
            crate::widgets::announce_region(ui, egui::accesskit::Role::Toolbar, crate::i18n::tr("Tool strip"));
            let has_photo = app.session.active().is_some();
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                let sz = vec2(t.strip_w, BUTTON_H);
                if icon_button(ui, "presets", Icon::Presets, sz, app.ui.presets, has_photo, "Presets (Shift+P)").clicked() {
                    let _ = app.run("panel.presets", json!({}));
                }
                for (id, icon, panel, tip) in [
                    ("edit", Icon::Sliders, RightPanel::Edit, "Edit (E)"),
                    ("crop", Icon::Crop, RightPanel::Crop, "Crop & Rotate (C)"),
                    ("remove", Icon::Eraser, RightPanel::Remove, "Remove (H)"),
                    ("masking", Icon::Mask, RightPanel::Masking, "Masking (M)"),
                    ("redeye", Icon::Eye, RightPanel::RedEye, "Red Eye"),
                ] {
                    let on = app.ui.right == panel || (panel == RightPanel::Edit && app.ui.right == RightPanel::Profiles);
                    if icon_button(ui, id, icon, sz, on, has_photo, tip).clicked() {
                        let _ = app.run(&format!("panel.{id}"), json!({}));
                    }
                    if id == "edit" {
                        separator(ui, &t);
                    }
                }
                separator(ui, &t);
                if icon_button(ui, "versions", Icon::Versions, sz, app.ui.right == RightPanel::Versions, has_photo, "Versions (Shift+V)").clicked() {
                    let _ = app.run("panel.versions", json!({}));
                }
                if icon_button(ui, "activity", Icon::Activity, sz, app.ui.right == RightPanel::Activity, true, "History & Activity (Y)").clicked() {
                    let _ = app.run("panel.activity", json!({}));
                }
                if icon_button(ui, "more", Icon::More, sz, false, true, "More").clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::About);
                }
            });
            // bottom icons: two buttons and one gap tall, so Info ends at the bottom margin as
            // Presets starts at the top one
            let bottom = ui.max_rect().bottom();
            let mut child =
                ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
                    egui::pos2(ui.max_rect().left(), bottom - (2.0 * BUTTON_H + GAP)),
                    ui.max_rect().right_bottom(),
                )));
            child.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                let sz = vec2(t.strip_w, BUTTON_H);
                if icon_button(ui, "keywords", Icon::Tag, sz, app.ui.right == RightPanel::Keywords, has_photo, "Keywords (K)").clicked() {
                    let _ = app.run("panel.keywords", json!({}));
                }
                if icon_button(ui, "info", Icon::Info, sz, app.ui.right == RightPanel::Info, has_photo, "Info (I)").clicked() {
                    let _ = app.run("panel.info", json!({}));
                }
            });
        });
}

fn separator(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(t.strip_w - 16.0, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.button_border.gamma_multiply(0.7));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::HeadlessView;

    #[test]
    fn info_ends_as_far_above_the_bottom_as_presets_starts_below_the_top() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        let mut view = HeadlessView::new();
        let mut widgets = vec![];
        for frame in 0..3 {
            let raw = HeadlessView::raw_input(vec2(300.0, 700.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| show(&mut app, ui));
            widgets = crate::widgets::take_registry(&view.ctx);
        }
        let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not registered")).1;
        let (presets, keywords, info) = (rect("icon:presets"), rect("icon:keywords"), rect("icon:info"));
        let (top, bottom) = (presets.top(), 700.0 - info.bottom());
        assert!((top - bottom).abs() < 0.5, "Presets starts {top} below the top, Info ends {bottom} above the bottom");
        assert_eq!(info.top() - keywords.bottom(), GAP, "the bottom buttons are spaced like the top ones");
    }
}
