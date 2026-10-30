use super::rows::{INDENT_STEP, Inside, Row, tree_toggle};
use super::{Branch, Icon, LibrarySource, LightcraftApp, Said, json};

/// By Date: year → month → day; a click filters by that prefix, the triangle opens a level.
pub(super) fn date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let groups = app.caches.date_groups(&app.session.catalog);
    for g in groups.iter() {
        if date_row(app, ui, &g.year, &crate::i18n::date_group_label(&g.year, true), g.count, 0.0, None) {
            let in_the_year = app.ui.sidebar_trees.inside(&Branch::date(&g.year), None);
            for (m, n) in &g.months {
                let label = crate::i18n::date_group_label(m, true);
                if date_row(app, ui, m, &label, *n, INDENT_STEP, in_the_year) {
                    for (d, n) in g.days.iter().filter(|(d, _)| d.starts_with(m.as_str())) {
                        let label = crate::i18n::date_group_label(d, true);
                        date_row(app, ui, d, &label, *n, 2.0 * INDENT_STEP, None);
                    }
                }
            }
        }
    }
}

/// One By Date row (`key`: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`); returns whether it is open.
fn date_row(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, label: &str, count: usize, indent: f32, above: Option<Said>) -> bool {
    let branch = Branch::date(key);
    let mut open = app.ui.sidebar_trees.is_open_under(&branch, above);
    let sel = app.session.filter.date.as_deref() == Some(key);
    let resp = Row::new(&format!("date:{key}"), Icon::Clock, label).count(Some(count)).selected(sel).indent(indent).show(app, ui);
    if key.len() < 10 {
        // (a year's months open and fold with it; a month's days have nothing to open)
        let inside = if key.len() < 7 { Inside::OpensAndFolds } else { Inside::Alone };
        tree_toggle(ui, &mut app.ui.sidebar_trees, &resp, indent, &branch, &mut open, format!("dateToggle:{key}"), inside);
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
