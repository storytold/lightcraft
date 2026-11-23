use std::collections::HashMap;

use super::dates::browse_all_photos;
use super::rows::{MAX_DEPTH, MenuAt, SideEvent, SideRow, SideSpec, entry, host_menu, rule, set_tree_open, side_list, tree_open};
use super::{KeywordNode, LightcraftApp, json};

/// The widget id of a keyword's row: `keyword:<path>`.
fn row_id(path: &str) -> String {
    format!("keyword:{path}")
}

/// Whether the keyword `path` is open: what the user last chose this session (leaves have no
/// open state to look up).
fn open_key(path: &str) -> egui::Id {
    egui::Id::unique(("kw-open", path.to_lowercase()))
}

/// The rows of `nodes` and, for the open ones, of the keywords inside them; `paths` maps each
/// row's key back to its keyword.
fn flatten(ui: &egui::Ui, nodes: &[KeywordNode], depth: usize, rows: &mut Vec<SideRow>, paths: &mut HashMap<String, String>) {
    for n in nodes {
        let id = row_id(&n.path);
        let has_children = !n.children.is_empty();
        let open = has_children && tree_open(ui, open_key(&n.path), false);
        let tip = if has_children { format!("{} (includes the keywords below it)", n.path) } else { n.path.clone() };
        let mut row = SideRow::new(id.clone(), "pin", n.name.clone()).count(Some(n.count)).depth(depth).tip(tip);
        if has_children {
            row = row.group(open, format!("keywordToggle:{}", n.path));
        }
        paths.insert(id, n.path.clone());
        rows.push(row);
        if open && depth < MAX_DEPTH {
            flatten(ui, &n.children, depth + 1, rows, paths);
        }
    }
}

/// "Keywords": the library's keyword tree with photo counts (`a|b|c` keywords nest). A click
/// filters the grid by the keyword (children included), the triangle opens a level, and the
/// context menu renames, merges or deletes the keyword across the library.
pub(super) fn keyword_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[KeywordNode]) {
    let chosen = app.session.filter.keyword.clone();
    let mut rows = Vec::new();
    let mut paths = HashMap::new();
    flatten(ui, nodes, 0, &mut rows, &mut paths);
    // the chosen keyword's row, however its path is cased
    let selected = rows
        .iter()
        .find(|row| paths.get(&row.key).zip(chosen.as_deref()).is_some_and(|(path, k)| k.eq_ignore_ascii_case(path)))
        .map(|row| row.key.clone());
    let spec = SideSpec::new("keywords", crate::i18n::tr("Keywords"), app.ui.show_counts).selected(selected.as_deref());
    let out = side_list(ui, spec, rows);
    let mut request = None;
    for event in &out.events {
        match event {
            SideEvent::Open { key, open } => {
                if let Some(path) = paths.get(key) {
                    set_tree_open(ui, open_key(path), *open);
                }
            }
            SideEvent::Activate(key) => {
                let Some(path) = paths.get(key) else { continue };
                let sel = selected.as_deref() == Some(key.as_str());
                let v = if sel { serde_json::Value::Null } else { json!(path) };
                browse_all_photos(app, !sel);
                let _ = app.run("library.filter", json!({"keyword": v}));
            }
            SideEvent::Menu { key, at } => request = Some((key.clone(), MenuAt::Point(*at))),
        }
    }
    host_menu(ui, "keywords", crate::i18n::tr("Keyword"), out.response.as_ref(), request, |ui, key| {
        let Some(path) = paths.get(key) else { return };
        keyword_menu(app, ui, path);
    });
}

/// A keyword's context menu: give or take the keyword on the selected photos, and rename, merge or
/// delete it across the library.
fn keyword_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, path: &str) {
    let has_sel = app.session.active().is_some();
    if entry("keyword", crate::i18n::tr("Add to Selected Photos")).enabled(has_sel).show(ui) {
        let _ = app.run("photo.setMeta", json!({"addKeywords": [path]}));
    }
    if entry("keyword", crate::i18n::tr("Remove from Selected Photos")).enabled(has_sel).show(ui) {
        let _ = app.run("photo.setMeta", json!({"removeKeywords": [path]}));
    }
    rule(ui);
    if entry("keyword", crate::i18n::tr("Rename Keyword…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: path.to_string(), to: path.to_string() });
    }
    if entry("keyword", crate::i18n::tr("Merge into…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::MergeKeywords { from: vec![path.to_string()], into: String::new() });
    }
    if entry("keyword", crate::i18n::tr("Delete Keyword")).show(ui) {
        let _ = app.run("keyword.delete", json!({"keyword": path}));
    }
}
