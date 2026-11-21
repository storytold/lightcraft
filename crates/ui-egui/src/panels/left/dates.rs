use super::rows::{INDENT_STEP, Row, tree_open, tree_toggle};
use super::{Icon, LibrarySource, LightcraftApp, json};

/// By Date: year → month → day; a click filters by that prefix, the triangle opens a level.
pub(super) fn date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let groups = app.caches.date_groups(&app.session.catalog);
    for g in groups.iter() {
        if date_row(app, ui, &g.year, &crate::i18n::date_group_label(&g.year, true), g.count, 0.0) {
            for (m, n) in &g.months {
                let label = crate::i18n::date_group_label(m, true);
                if date_row(app, ui, m, &label, *n, INDENT_STEP) {
                    for (d, n) in g.days.iter().filter(|(d, _)| d.starts_with(m.as_str())) {
                        let label = crate::i18n::date_group_label(d, true);
                        date_row(app, ui, d, &label, *n, 2.0 * INDENT_STEP);
                    }
                }
            }
        }
    }
}

/// One By Date row (`key`: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`); returns whether it is open.
fn date_row(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, label: &str, count: usize, indent: f32) -> bool {
    let open_id = egui::Id::unique(("date-open", key.to_string()));
    let mut open = tree_open(ui, open_id, false);
    let sel = app.session.filter.date.as_deref() == Some(key);
    let resp = Row::new(&format!("date:{key}"), Icon::Clock, label).count(Some(count)).selected(sel).indent(indent).show(app, ui);
    if key.len() < 10 {
        // (the name carries the year, so two Marches in different years differ in a screen reader's list of triangles)
        tree_toggle(ui, &resp, indent, open_id, &mut open, format!("dateToggle:{key}"), &crate::i18n::date_group_label(key, false));
    }
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(key) };
        browse_all_photos(app, !sel);
        let _ = app.run("library.filter", json!({"date": v}));
    }
    open
}

/// By Date and Keywords count every photo in the library, so choosing a row shows those photos
/// from All Photos, not from whatever album or folder happened to be open, where they could be
/// missing (issue #341). Only when choosing (`on`), not when clearing the row again.
pub(crate) fn browse_all_photos(app: &mut LightcraftApp, on: bool) {
    if on && app.session.source != LibrarySource::All {
        let _ = app.run("library.source", json!({"kind": "all"}));
    }
}
