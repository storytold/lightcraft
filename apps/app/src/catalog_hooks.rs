//! The catalog's steps of the desktop app's start and exit (fork-owned; `main.rs` calls each once).

use dac_engine::Session;
use dac_ui_egui::DacApp;

/// The catalog's backup schedule (Catalog Settings): checked on exit, like Lightroom.
pub fn backup_if_due(session: &mut Session) {
    match session.backup_catalog_if_due() {
        Ok(Some(dir)) => log::info!("catalog backed up to {}", dir.display()),
        Ok(None) => {}
        Err(e) => log::error!("catalog backup failed: {e}"),
    }
}

/// The catalog chosen to open at startup (the chooser's default), if it still exists.
pub fn startup_catalog() -> Option<std::path::PathBuf> {
    let file = dac_engine::catalog::library::RecentCatalogs::default_file()
        .filter(|_| !dac_brand::env_is_set("NO_PREFS") && !dac_brand::env_is_set("LIBRARY"))?;
    let d = dac_engine::catalog::library::RecentCatalogs::load(&file).default?;
    dac_engine::catalog::library::resolve(&d).ok()
}

/// File ▸ Open Recent Catalog and the startup chooser (not for throwaway sessions).
pub fn remember_recent(app: &mut DacApp, in_memory: bool) {
    if !in_memory && !dac_brand::env_is_set("NO_PREFS") {
        app.catalog_ui.recent_file = dac_engine::catalog::library::RecentCatalogs::default_file();
        if let Some(dir) = app.session.catalog_dir().map(std::path::Path::to_path_buf) {
            app.catalog_ui.touch(&dir);
        }
    }
}
