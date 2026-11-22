//! The Show Alternatives bar: over the frames around a photo that didn't work (from its folder on
//! disk, imported or not), what this view is, Apply the same edit, Import Selected and Back.

use crate::json_ext::JsonExt as _;
use serde_json::json;

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Height of the bar.
const BAR_H: f32 = 44.0;

/// The bar, while alternatives are shown (Photo ▸ Show Alternatives). Leaving the folder view any
/// other way ends them.
pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let Some(original) = app.session.alternatives.as_ref().map(|a| a.original) else { return };
    if app.session.source != lcc_engine::LibrarySource::Folder {
        app.session.alternatives = None;
        return;
    }
    let t = Tokens::get(ui.ctx());
    // a strip at the top of the view's own area (the view below gets what is left), drawn on its own layer so
    // a view that senses its whole area never takes the bar's clicks
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_H), egui::Sense::hover());
    let area = egui::Area::new(egui::Id::new("alternatives-bar")).order(egui::Order::Middle).fixed_pos(strip.min).interactable(true);
    area.show(ui.ctx(), |ui| {
        ui.set_min_size(strip.size());
        ui.set_max_size(strip.size());
        ui.painter().rect_filled(strip, 0.0, t.chrome);
        let mut child = ui.new_child(
            egui::UiBuilder::new().max_rect(strip.shrink2(egui::vec2(16.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut child;
        {
            register(ui.ctx(), "alternatives:bar", ui.max_rect());
            let name = app.session.catalog.photo(original).map(|p| p.file_name.clone()).unwrap_or_default();
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr_format!("Alternatives to {name}", name = name)).font(t.semibold(14.0)).color(t.text));
                ui.label(
                    egui::RichText::new(crate::i18n::tr("Frames around it in its folder: select the ones to bring into the catalog"))
                        .font(t.font(12.0))
                        .color(t.text_dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let back = ui.button(crate::i18n::tr("Back"));
                    register(ui.ctx(), "button:alternativesBack", back.rect);
                    if back.clicked() {
                        let _ = app.run("photo.closeAlternatives", json!({}));
                    }
                    let chosen = app.session.selection.ids.iter().filter(|id| **id != original).count();
                    let label = crate::i18n::tr_format!("Import {n} Selected", n = chosen);
                    let import = ui.add_enabled(chosen > 0, egui::Button::new(egui::RichText::new(label).strong()));
                    register(ui.ctx(), "button:alternativesImport", import.rect);
                    if import.clicked() {
                        let same = app.ui.alternatives_same_edit;
                        match app.run("photo.importAlternatives", json!({"sameEdit": same})) {
                            Ok(v) => {
                                let n = v.field("chosen").as_array().map_or(0, Vec::len);
                                app.toast(ui.ctx(), crate::i18n::tr_format!("{n} alternatives in the catalog", n = n));
                            }
                            Err(e) => app.toast(ui.ctx(), e),
                        }
                    }
                    let same = ui.checkbox(&mut app.ui.alternatives_same_edit, crate::i18n::tr("Apply the same edit"));
                    register(ui.ctx(), "check:alternativesSameEdit", same.rect);
                });
            });
        }
    });
}
