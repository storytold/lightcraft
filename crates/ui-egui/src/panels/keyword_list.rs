//! The Library module's Keyword List panel.

use crate::DacApp;

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let tree = app.caches.keyword_tree(&app.session.catalog);
    // the sidebar rows clip to the left panel's visible edge: not here
    let key = egui::Id::new("left-visible-right");
    let before: Option<f32> = ui.data(|d| d.get_temp(key));
    ui.data_mut(|d| d.insert_temp(key, f32::MAX));
    super::left::keyword_rows(app, ui, &tree, 0.0);
    if let Some(b) = before {
        ui.data_mut(|d| d.insert_temp(key, b));
    }
}
