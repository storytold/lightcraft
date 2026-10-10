//! Edit In (P4.4), the desktop half: after `photo.editIn` / `photo.openAsLayers` made a file, open
//! it in the preset's application (the host's `open_with`) and reload the photo when the app
//! regains focus, as Edit in External Editor does.

use serde_json::Value;

use crate::DacApp;

/// Called with every engine command's successful result.
pub(crate) fn after_command(app: &mut DacApp, id: &str, r: &Value) {
    if !matches!(id, "photo.editIn" | "photo.openAsLayers") || !r["opened"].is_null() {
        return;
    }
    let w = &r["openWith"];
    let Some(path) = w["path"].as_str() else { return };
    for id in w["reload"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
        if !app.ui.external_edits.contains(&id) {
            app.ui.external_edits.push(id);
        }
    }
    let editor = w["app"].as_str().filter(|a| !a.is_empty()).map(str::to_string).unwrap_or_else(|| app.ui.settings.external_editor.clone());
    if let Some(f) = app.services.open_with.as_mut()
        && let Err(e) = f(path, &editor)
    {
        app.ui.status = format!("Couldn't open the editor: {e}");
    }
}
