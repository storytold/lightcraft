//! A quiet workflow bar: workspaces on the left, file context in the middle, delivery on the right.
use crate::{
    LightcraftApp,
    state::{RightPanel, ViewMode},
    theme::Tokens,
    widgets,
};
use egui::{Align2, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

pub(super) fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("workspace_bar").exact_size(40.0).frame(egui::Frame::NONE.fill(t.canvas).inner_margin(egui::Margin::symmetric(16, 0))).show(
        ui,
        |ui| {
            widgets::announce_region(ui, egui::accesskit::Role::Toolbar, crate::i18n::tr("Workspace"));
            let editing = matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare | ViewMode::Reference);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if tab(ui, "workspaceLibrary", "Library", !editing).clicked() {
                    let _ = app.run("view.photoGrid", json!({}));
                    if !app.ui.left_panel {
                        let _ = app.run("view.leftPanel", json!({}));
                    }
                }
                if tab(ui, "workspaceDevelop", "Develop", editing).clicked() {
                    let _ = app.run("view.detail", json!({}));
                    if app.ui.right != RightPanel::Edit {
                        let _ = app.run("panel.edit", json!({}));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let export = widgets::text_button(ui, "workspaceExport", "Export", false);
                    widgets::register(ui, "icon:share", export.rect);
                    if export.clicked() {
                        let _ = app.run("dialog.export", json!({}));
                    }
                    let import = widgets::text_button(ui, "workspaceImport", "Import Photos…", false);
                    widgets::register(ui, "icon:import", import.rect);
                    if import.clicked()
                        && let Err(e) = app.run("file.addPhotos", json!({}))
                    {
                        app.toast_error(ui.ctx(), e);
                    }
                    if ui.available_width() > 100.0 {
                        let filename = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.file_name.as_str()).unwrap_or("");
                        ui.add(egui::Label::new(egui::RichText::new(filename).font(t.font(11.5)).color(t.text_dim)).truncate())
                            .on_hover_text(filename);
                    }
                });
            });
            let r = ui.max_rect();
            ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.divider));
        },
    );
}

fn tab(ui: &mut egui::Ui, id: &str, label: &str, active: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let label = crate::i18n::tr(label);
    let galley = ui.painter().layout_no_wrap(label.to_string(), t.semibold(12.5), t.text);
    let (r, response) = ui.allocate_exact_size(vec2(galley.size().x + 24.0, 32.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
    widgets::register(ui, format!("button:{id}"), r);
    if response.hovered() {
        ui.painter().rect_filled(r, 4.0, t.hover);
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, t.semibold(12.5), if active { t.text } else { t.text_dim });
    if active {
        let line = Rect::from_min_max(pos2(r.left() + 12.0, r.bottom() - 2.0), pos2(r.right() - 12.0, r.bottom()));
        ui.painter().rect_filled(line, 1.0, t.accent);
    }
    response
}
