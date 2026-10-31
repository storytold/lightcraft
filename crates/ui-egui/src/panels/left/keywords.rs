use super::dates::browse_all_photos;
use super::rows::{INDENT_STEP, Row, out_of_sight, tree_open, tree_toggle};
use super::{Icon, KeywordNode, LightcraftApp, json};

/// "Keywords": the library's keyword tree with photo counts (`a|b|c` keywords nest). A click
/// filters the grid by the keyword (children included), the triangle opens a level, and the
/// context menu renames, merges or deletes the keyword across the library.
pub(super) fn keyword_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[KeywordNode], indent: f32) {
    for n in nodes {
        // leaves (most keywords) have no open state to look up
        let open_id = (!n.children.is_empty()).then(|| egui::Id::unique(("kw-open", n.path.to_lowercase())));
        let mut open = open_id.is_some_and(|k| tree_open(ui, k, false));
        let id = format!("keyword:{}", n.path);
        if !out_of_sight(app, ui, &id, &n.name, Some(n.count), indent, false) {
            keyword_row(app, ui, n, &id, indent, open_id, &mut open);
        }
        if open {
            keyword_rows(app, ui, &n.children, indent + INDENT_STEP);
        }
    }
}

/// One drawn row of [`keyword_rows`]; `open_id` and `open` are its triangle's when it has rows inside it.
fn keyword_row(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &KeywordNode, id: &str, indent: f32, open_id: Option<egui::Id>, open: &mut bool) {
    let sel = app.session.filter.keyword.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(&n.path));
    let resp = Row::new(id, Icon::Tag, &n.name).count(Some(n.count)).selected(sel).indent(indent).show(app, ui);
    if let Some(k) = open_id {
        tree_toggle(ui, &resp, indent, k, open, format!("keywordToggle:{}", n.path), &n.name);
    }
    let resp = resp.on_hover_text(if n.children.is_empty() { n.path.clone() } else { format!("{} (includes the keywords below it)", n.path) });
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(n.path) };
        browse_all_photos(app, !sel);
        let _ = app.run("library.filter", json!({"keyword": v}));
    }
    crate::widgets::context_menu(&resp, |ui| {
        let has_sel = app.session.active().is_some();
        if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Add to Selected Photos"))).clicked() {
            let _ = app.run("photo.setMeta", json!({"addKeywords": [n.path]}));
        }
        if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Remove from Selected Photos"))).clicked() {
            let _ = app.run("photo.setMeta", json!({"removeKeywords": [n.path]}));
        }
        ui.separator();
        if ui.button(crate::i18n::tr("Rename Keyword…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: n.path.clone(), to: n.path.clone() });
        }
        if ui.button(crate::i18n::tr("Merge into…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::MergeKeywords { from: vec![n.path.clone()], into: String::new() });
        }
        if ui.button(crate::i18n::tr("Delete Keyword")).clicked() {
            let _ = app.run("keyword.delete", json!({"keyword": n.path}));
        }
    });
}
