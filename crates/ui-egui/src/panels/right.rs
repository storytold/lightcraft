//! The right-hand panel next to the tool strip: Edit, Crop, Remove, Masking, Red Eye, Info,
//! Keywords, Versions, Activity.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, register, slider, text_button};

mod crop;
mod history;
mod info;
mod keywords;

#[cfg(test)]
use crop::aspect_label;
pub(crate) use crop::tool_overlay_options;
use crop::{crop, red_eye, remove};
#[cfg(test)]
use history::{HistoryRow, history_row, short_time};
use history::{activity, versions};
use info::info;
#[cfg(test)]
pub(crate) use info::mixed_meta;
use keywords::keywords;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        // (taken on every pass the panel is drawn; one from earlier than the pass before is forgotten)
        let held = held_offset(ui.ctx());
        let Some(id) = app.session.active() else {
            // the Keyword List is the library's, not a photo's: it is there without a selection
            if app.ui.right == RightPanel::Keywords {
                // (without a selection there is no Add keyword box for Add Keywords to put the cursor in)
                app.ui.focus_keywords = None;
                egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if fold_header(app, ui, "keywording", "Keywords") {
                        padded(ui, |ui| {
                            ui.label(egui::RichText::new(crate::i18n::tr("Select photos to give them keywords.")).color(t.text_dim));
                        });
                    }
                    super::keyword_list::show(app, ui);
                });
                return;
            }
            let r = ui.max_rect();
            // every tab (Edit, Info, Versions…) says it: nothing to show until photos come in
            if super::empty_library::shown(&app.session) {
                super::empty_message(ui, r, "No photos yet", "Import photos to get started");
            } else {
                super::empty_message(ui, r, "No photo selected", "Select a photo to edit");
            }
            return;
        };
        let mut area = egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]);
        if let Some(offset) = held {
            area = area.vertical_scroll_offset(offset);
        }
        // (where the pass starts: by its end egui may have pulled a shrunken panel's offset back)
        let area_id = ui.data(|d| d.get_temp::<egui::Id>(egui::Id::unique(AREA)));
        let start = held.or_else(|| area_id.and_then(|a| egui::scroll_area::State::load(ui.ctx(), a)).map(|s| s.clamped_offset().y));
        ui.data_mut(|d| match start {
            Some(o) => {
                d.insert_temp(egui::Id::unique(OFFSET), o);
            }
            None => d.remove::<f32>(egui::Id::unique(OFFSET)),
        });
        let scrolled = area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match app.ui.right {
                RightPanel::Edit => super::edit::show(app, ui, id),
                RightPanel::Profiles => super::profiles::show(app, ui, id),
                RightPanel::Crop => crop(app, ui, id),
                RightPanel::Remove => remove(app, ui, id),
                RightPanel::Masking => super::masking::show(app, ui, id),
                RightPanel::RedEye => red_eye(app, ui, id),
                RightPanel::Info => info(app, ui, id),
                RightPanel::Keywords => keywords(app, ui, id),
                RightPanel::Versions => versions(app, ui, id),
                RightPanel::Activity => activity(app, ui, id),
                RightPanel::None => {}
            }
        });
        ui.data_mut(|d| d.insert_temp(egui::Id::unique(AREA), scrolled.id));
    });
}

/// The panel's scroll area, its offset as the last pass started, and how far
/// [`hold_list_in_place`] asks it to move.
const AREA: &str = "right-scroll-id";
const OFFSET: &str = "right-scroll-offset";
const HOLD: &str = "right-scroll-hold";

/// Where [`hold_list_in_place`] holds the panel: its offset as the pass before started, moved by as much as
/// the Keywords box grew. Set on the scroll area as it starts, so the pass drawn again shows the list
/// in place (a scroll asked for during a pass only lands a frame later).
fn held_offset(ctx: &egui::Context) -> Option<f32> {
    let (grew, asked) = ctx.data_mut(|d| d.remove_temp::<(f32, u64)>(egui::Id::unique(HOLD)))?;
    if asked.checked_add(1) != Some(ctx.cumulative_pass_nr()) {
        return None;
    }
    let offset = ctx.data(|d| d.get_temp::<f32>(egui::Id::unique(OFFSET)))?;
    Some((offset + grew).max(0.0)).filter(|o| o.is_finite())
}

pub fn header(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(15.0), t.text);
}

/// A panel title that folds its section shut and open again (`section:{id}`; kept in
/// [`crate::state::UiState::folded_sections`]). Says whether the section is open.
pub fn fold_header(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, title: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let title = crate::i18n::tr(title);
    fold_title(app, ui, id, r, &resp, title)
}

/// [`fold_header`] for a title whose room is already taken (`r`, answering as `resp`).
pub fn fold_title(app: &mut LightcraftApp, ui: &egui::Ui, id: &str, r: Rect, resp: &egui::Response, title: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let open = !app.ui.section_folded(id);
    crate::widgets::announce_disclosure(resp, true, open, title);
    register(ui, format!("section:{id}"), r);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 0.0, t.chrome.gamma_multiply(1.06));
    }
    if resp.has_focus() {
        p.rect_stroke(r.shrink(2.0), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    }
    p.text(pos2(r.left() + 24.0, r.center().y + 2.0), Align2::LEFT_CENTER, title, t.semibold(15.0), t.text);
    let chevron = Rect::from_center_size(pos2(r.right() - 30.0, r.center().y + 2.0), vec2(14.0, 14.0));
    paint(p, chevron, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.icon);
    if resp.clicked() {
        app.ui.toggle_folded(id);
    }
    open
}

pub fn label_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(12.5), t.text_dim);
    ui.painter().text(pos2(r.left() + 110.0, r.center().y), Align2::LEFT_CENTER, value, t.font(12.5), t.text_label);
}

fn padded(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 6 }).show(ui, add);
}

#[cfg(test)]
mod tests;
