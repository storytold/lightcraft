//! The browser build has one library in browser storage: no catalog files to create, open, back
//! up or move, so the catalog UI (`catalog_ui.rs` natively) is empty here.

use serde_json::Value;

use crate::DacApp;

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogDialog {}

#[derive(Default)]
pub struct CatalogUi {
    pub dialog: Option<CatalogDialog>,
    pub recent_file: Option<std::path::PathBuf>,
}

pub fn run(_: &mut DacApp, _: &str, _: &Value) -> Option<Result<Value, String>> {
    None
}

pub fn enabled(_: &DacApp, id: &str) -> Option<bool> {
    (id.starts_with("catalog.") || matches!(id, "dialog.catalogSettings" | "dialog.exportCatalog" | "dialog.importCatalog")).then_some(false)
}

pub fn recent_items(_: &DacApp) -> Vec<(String, Value)> {
    Vec::new()
}

pub fn show(_: &mut DacApp, _: &egui::Context) {}
