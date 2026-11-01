//! The tool strip: one horizontal row of tool icons at the top of the right-hand panel, under the
//! histogram (Edit, Crop, Remove, Masking â€¦, rarely used tools in its "â€¦" menu), as Lightroom
//! Classic has it, so the panel keeps the whole width for its sliders. While the
//! panel is closed, the same tools stand in a vertical strip at the window's right edge, to open it
//! again.

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

/// Height of one line of the horizontal tool row.
pub const ROW_H: f32 = 30.0;

/// The narrowest a tool button in the row gets (pt): a hit target of at least 24 Ã— 24, so a
/// narrow panel wraps the row onto a second line instead of shrinking the buttons below it.
pub const ROW_BUTTON_MIN: f32 = 24.0;

/// The widest a tool button in the row gets (pt).
const ROW_BUTTON_MAX: f32 = 30.0;
const ROW_GAP: f32 = 2.0;
const ROW_SIDE: f32 = 8.0;

/// The tools in the horizontal row: id (`icon:{id}`), icon, panel, tooltip, enabled without a photo.
const ROW_TOOLS: [(&str, Icon, RightPanel, &str, bool); 8] = [
    ("edit", Icon::Sliders, RightPanel::Edit, "Edit (E)", false),
    ("crop", Icon::Crop, RightPanel::Crop, "Crop & Rotate (C)", false),
    ("remove", Icon::Eraser, RightPanel::Remove, "Remove (H)", false),
    ("masking", Icon::Mask, RightPanel::Masking, "Masking (M)", false),
    ("versions", Icon::Versions, RightPanel::Versions, "Versions (Shift+V)", false),
    ("activity", Icon::Activity, RightPanel::Activity, "History & Activity (Y)", true),
    ("keywords", Icon::Tag, RightPanel::Keywords, "Keywords (K)", false),
    ("info", Icon::Info, RightPanel::Info, "Info (I)", false),
];

/// How the row's `count` buttons fit a panel `w` wide: buttons per line, lines, and the button
/// width. One line while the buttons can be at least [`ROW_BUTTON_MIN`] wide, else as many lines
/// as that takes (a panel too narrow for even one keeps one button a line).
pub(crate) fn row_fit(w: f32, count: usize) -> (usize, usize, f32) {
    let count = count.max(1);
    let room = if w.is_finite() { (w - 2.0 * ROW_SIDE).max(0.0) } else { 0.0 };
    // (a cast of a finite, non-negative number of buttons; at least one a line)
    let fits = (((room + ROW_GAP) / (ROW_BUTTON_MIN + ROW_GAP)).floor() as usize).clamp(1, count);
    let lines = count.div_ceil(fits);
    // the lines share the buttons evenly, so the last isn't a lone one
    let per_line = count.div_ceil(lines);
    let bw = ((room - ROW_GAP * (per_line as f32 - 1.0)) / per_line as f32).clamp(ROW_BUTTON_MIN, ROW_BUTTON_MAX);
    (per_line, lines, bw)
}

/// One button of the horizontal row.
#[derive(Clone, Copy)]
enum RowItem {
    Tool(usize),
    Presets,
    More,
}

/// The horizontal tool row at the top of the right-hand panel (under the histogram): the tools,
/// Presets, and "â€¦" with the rarely used ones (Red Eye) and About. Same widget ids as the
/// vertical strip (`icon:edit` â€¦), so scripts and shortcuts don't change. On a panel too narrow
/// for every button at [`ROW_BUTTON_MIN`] it wraps onto a second line (Tab and screen readers go
/// through it left to right, line by line).
pub fn row(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let has_photo = app.session.active().is_some();
    // the tools, Presets, â€¦
    let items: Vec<RowItem> = (0..ROW_TOOLS.len()).map(RowItem::Tool).chain([RowItem::Presets, RowItem::More]).collect();
    let w = ui.available_width();
    let (per_line, lines, bw) = row_fit(w, items.len());
    let (r, _) = ui.allocate_exact_size(vec2(w, ROW_H * lines as f32), egui::Sense::hover());
    crate::widgets::register(ui, "toolRow", r);
    let mut outer = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Center)));
    // the same toolbar as the strip at the window's edge, standing in the panel now
    crate::widgets::announce_region(&outer, egui::accesskit::Role::Toolbar, crate::i18n::tr("Tool strip"));
    let sz = vec2(bw, ROW_H - 4.0);
    for (line, chunk) in items.chunks(per_line).enumerate() {
        let n = chunk.len() as f32;
        let total = bw * n + ROW_GAP * (n - 1.0);
        let line_rect = egui::Rect::from_min_size(egui::pos2(r.left(), r.top() + ROW_H * line as f32), vec2(r.width(), ROW_H));
        let mut child = outer.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_center_size(line_rect.center(), vec2(total, ROW_H)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut child;
        ui.spacing_mut().item_spacing.x = ROW_GAP;
        for item in chunk {
            match *item {
                RowItem::Tool(i) => {
                    let Some(&(id, icon, panel, tip, without_photo)) = ROW_TOOLS.get(i) else { continue };
                    let on = app.ui.right == panel || (panel == RightPanel::Edit && app.ui.right == RightPanel::Profiles);
                    if icon_button(ui, id, icon, sz, on, has_photo || without_photo, tip).clicked() {
                        let _ = app.run(&format!("panel.{id}"), json!({}));
                    }
                }
                RowItem::Presets => {
                    if icon_button(ui, "presets", Icon::Presets, sz, app.ui.presets, has_photo, "Presets (Shift+P)").clicked() {
                        let _ = app.run("panel.presets", json!({}));
                    }
                }
                RowItem::More => more_menu(app, ui, sz, has_photo),
            }
        }
    }
    let p = ui.painter();
    p.hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, t.divider));
}

/// The row's "â€¦" button and its menu: Red Eye and About.
fn more_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, sz: egui::Vec2, has_photo: bool) {
    let more = icon_button(ui, "more", Icon::More, sz, app.ui.right == RightPanel::RedEye, true, "More tools");
    egui::Popup::menu(&more).show(|ui| {
        let r = ui.add_enabled(has_photo, egui::Button::new(crate::i18n::tr("Red Eye")));
        crate::widgets::register_response("menuItem:toolRow:redeye", r.rect, &r);
        if r.clicked() {
            let _ = app.run("panel.redeye", json!({}));
        }
        let r = ui.button(crate::i18n::tr("About LightCraft"));
        crate::widgets::register_response("menuItem:toolRow:about", r.rect, &r);
        if r.clicked() {
            app.ui.dialog = Some(crate::state::Dialog::About);
        }
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
