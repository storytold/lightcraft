//! The Library module's Keywording panel.

use crate::DacApp;

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    super::right::keywords_panel(app, ui);
}
