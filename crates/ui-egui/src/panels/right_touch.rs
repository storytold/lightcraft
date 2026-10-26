//! iPad-only parts of the right panel (title row with close button, touch copy, the Remove panel's two find buttons).
//! `right.rs` calls these; each does nothing unless `theme::touch_ui` is on, so desktop is untouched.

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::RightPanel;
use crate::theme::{Tokens, touch_ui};
use crate::widgets::{divider, register, text_button};

/// `touch` on the iPad, `desktop` everywhere else (desktop-isms such as shortcut letters are dropped on touch).
pub(super) fn copy(ui: &egui::Ui, touch: &'static str, desktop: &'static str) -> &'static str {
    if touch_ui(ui.ctx()) { touch } else { desktop }
}

/// The panel's own title row replaces the first heading, so the heading is not repeated.
pub(super) fn skip_header(ui: &egui::Ui, title: &str) -> bool {
    touch_ui(ui.ctx()) && matches!(title, "Keywords" | "Remove" | "Red Eye" | "Info" | "Versions" | "Masking")
}

/// Title and close button above the panel.
pub(super) fn title_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if !touch_ui(ui.ctx()) || app.ui.right == RightPanel::None {
        return;
    }
    let title = match app.ui.right {
        RightPanel::QuickDevelop => "Quick Develop",
        RightPanel::Edit => "Edit",
        RightPanel::Profiles => "Profiles",
        RightPanel::Crop => "Crop & Rotate",
        RightPanel::Remove => "Remove",
        RightPanel::Masking => "Masking",
        RightPanel::RedEye => "Red Eye",
        RightPanel::Info => "Info",
        RightPanel::Keywords => "Keywords",
        RightPanel::Versions => "Versions",
        RightPanel::Activity => "History & Activity",
        RightPanel::None => "",
    };
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    ui.painter().text(pos2(hr.left() + 20.0, hr.center().y), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(15.0), t.text);
    let mut close_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(hr.right() - 48.0, hr.top()), hr.right_bottom()))
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
    );
    let btn = crate::glass::close_button(&mut close_ui);
    register(ui, "button:rightPanelClose", btn.rect);
    if btn.clicked() {
        app.ui.right = RightPanel::None;
        // a mask tool left armed would keep press-and-hold Before and the canvas drag from working
        app.ui.tool.clear();
    }
    divider(ui);
}

/// Find Dust and Find Blemishes side by side (the desktop panel keeps its single "Find Dust Spots" button).
pub(super) fn find_buttons(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        for (id, label, hint, cmd, none, done) in [
            (
                "findDust",
                "Find Dust",
                "Heal the small, soft dark spots sensor dust leaves on smooth areas",
                "spot.findDust",
                "No dust spots found",
                "dust spot",
            ),
            (
                "findBlemishes",
                "Find Blemishes",
                "Heal small facial blemishes on detected faces",
                "spot.findBlemishes",
                "No face blemishes found",
                "facial blemish",
            ),
        ] {
            if text_button(ui, id, crate::i18n::tr(label), false).on_hover_text(crate::i18n::tr(hint)).clicked() {
                match app.run(cmd, json!({})) {
                    Ok(r) => {
                        let n = r["added"].as_u64().unwrap_or(0);
                        let msg = if n == 0 {
                            crate::i18n::tr(none).to_string()
                        } else {
                            format!(
                                "Healed {n} {done}{}",
                                if n == 1 {
                                    ""
                                } else if done.ends_with("blemish") {
                                    "es"
                                } else {
                                    "s"
                                }
                            )
                        };
                        app.toast(ui.ctx(), msg);
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        }
    });
}
