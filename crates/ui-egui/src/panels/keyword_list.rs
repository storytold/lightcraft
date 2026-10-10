//! The Library module's Keyword List panel (Classic): every keyword in the library as a tree with
//! photo counts, created keywords included; a filter (text, matching synonyms too, and All /
//! People / Other); a mark per row toggles the keyword on the selected photos; the row's menu
//! edits its synonyms, export options and person flag; Import / Export keyword list files.

use std::collections::BTreeMap;

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// One keyword row: its path, its own name, depth, count, whether it has children.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub count: usize,
}

/// Every keyword (from the photos, with counts, and the created ones), in tree order.
pub(crate) fn rows(tree: &[dac_catalog::KeywordNode], attrs: &BTreeMap<String, dac_engine::cmd::keyword_list::KeywordAttrs>) -> Vec<Row> {
    // path (lowercase) → (path as written, count)
    let mut all: BTreeMap<String, (String, usize)> = BTreeMap::new();
    fn walk(nodes: &[dac_catalog::KeywordNode], all: &mut BTreeMap<String, (String, usize)>, depth: usize) {
        if depth > 32 {
            return;
        }
        for n in nodes {
            all.insert(n.path.to_lowercase(), (n.path.clone(), n.count));
            walk(&n.children, all, depth + 1);
        }
    }
    walk(tree, &mut all, 0);
    for k in attrs.keys() {
        let mut path = String::new();
        for part in k.split('|').take(32) {
            if !path.is_empty() {
                path.push('|');
            }
            path.push_str(part);
            all.entry(path.to_lowercase()).or_insert_with(|| (path.clone(), 0));
        }
    }
    // sorted by the path's parts, so children follow their parent
    let mut v: Vec<(String, usize)> = all.into_values().collect();
    v.sort_by(|a, b| a.0.to_lowercase().split('|').cmp(b.0.to_lowercase().split('|')));
    v.into_iter()
        .map(|(path, count)| Row { name: path.rsplit('|').next().unwrap_or(&path).to_string(), depth: path.matches('|').count(), path, count })
        .collect()
}

fn attrs_of<'a>(app: &'a DacApp, path: &str) -> Option<&'a dac_engine::cmd::keyword_list::KeywordAttrs> {
    dac_engine::cmd::keyword_list::attrs(&app.session.keyword_attrs, path)
}

/// Whether the keyword (or one below it) is on all, some or none of the selected photos.
fn mark(app: &DacApp, path: &str) -> (usize, usize) {
    let ids = app.session.targets(&json!({}));
    let has =
        ids.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|p| p.meta.keywords.iter().any(|k| k.eq_ignore_ascii_case(path)))).count();
    (has, ids.len())
}

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // the toolbar: filter field, kind, + and the ⋯ menu
    let fid = egui::Id::new("kwlist-filter");
    let kid = egui::Id::new("kwlist-kind");
    let mut filter: String = ui.data(|d| d.get_temp(fid)).unwrap_or_default();
    let mut kind: u8 = ui.data(|d| d.get_temp(kid)).unwrap_or(0);
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text(crate::i18n::tr("Filter Keywords"))
                .desired_width((ui.available_width() - 64.0).max(60.0)),
        );
        register(ui.ctx(), "field:keywordListFilter", r.rect);
        let plus = icon_button(ui, "keywordCreate", Icon::Plus, vec2(24.0, 22.0), false, true, "Create Keyword Tag");
        if plus.clicked() {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: "Create Keyword Tag".into(),
                hint: "Keyword (a|b nests it under a)".into(),
                value: String::new(),
                command: "keyword.create".into(),
                params: json!({}),
                key: "keyword".into(),
            });
        }
        let more = icon_button(ui, "keywordListMore", Icon::More, vec2(24.0, 22.0), false, true, "Keyword List");
        egui::Popup::menu(&more).show(|ui| {
            if app.services.pick_list_file.is_some() && ui.button(crate::i18n::tr("Import Keywords…")).clicked() {
                let picked = app.services.pick_list_file.as_mut().map(|f| f()).unwrap_or_default();
                if let Some(path) = picked.first() {
                    match app.run("keyword.importList", json!({"path": path})) {
                        Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("{n} keywords imported", n = r["keywords"].as_u64().unwrap_or(0))),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
                ui.close();
            }
            if app.services.save_list_file.is_some() && ui.button(crate::i18n::tr("Export Keywords…")).clicked() {
                let path = app.services.save_list_file.as_mut().and_then(|f| f("Keywords.txt"));
                if let Some(path) = path
                    && let Err(e) = app.run("keyword.exportList", json!({"path": path}))
                {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
            if ui.button(crate::i18n::tr("Purge Unused Keywords")).clicked() {
                match app.run("keyword.removeUnused", json!({})) {
                    Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("{n} keywords removed", n = r["removed"].as_array().map_or(0, Vec::len))),
                    Err(e) => app.toast(ui.ctx(), e),
                }
                ui.close();
            }
        });
    });
    ui.horizontal(|ui| {
        for (i, label) in ["All", "People", "Other"].iter().enumerate() {
            let r = ui.selectable_label(kind == i as u8, egui::RichText::new(crate::i18n::tr(label)).size(11.5));
            register(ui.ctx(), format!("keywordListKind:{}", label.to_lowercase()), r.rect);
            if r.clicked() {
                kind = i as u8;
            }
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(fid, filter.clone());
        d.insert_temp(kid, kind);
    });
    let tree = app.caches.keyword_tree(&app.session.catalog);
    let all = rows(&tree, &app.session.keyword_attrs);
    let needle = filter.trim().to_lowercase();
    // a row shows when it (or one below it) passes the filter
    let passes = |app: &DacApp, r: &Row| {
        let a = attrs_of(app, &r.path);
        let person = a.is_some_and(|a| a.person);
        let kind_ok = match kind {
            1 => person,
            2 => !person,
            _ => true,
        };
        let text_ok = needle.is_empty()
            || r.name.to_lowercase().contains(&needle)
            || a.is_some_and(|a| a.synonyms.iter().any(|s| s.to_lowercase().contains(&needle)));
        kind_ok && text_ok
    };
    let shown: Vec<bool> = all.iter().map(|r| passes(app, r)).collect();
    let filtering = !needle.is_empty() || kind != 0;
    let mut skip_below: Option<usize> = None;
    for (i, r) in all.iter().enumerate() {
        if let Some(d) = skip_below {
            if r.depth > d {
                continue;
            }
            skip_below = None;
        }
        let lower = r.path.to_lowercase();
        let subtree_shown = shown.get(i).copied().unwrap_or(false)
            || all.iter().zip(shown.iter()).skip(i + 1).take_while(|(x, _)| x.depth > r.depth).any(|(_, s)| *s);
        if !subtree_shown {
            continue;
        }
        let has_kids = all.get(i + 1).is_some_and(|n| n.depth > r.depth);
        let open_id = egui::Id::new(("kwlist-open", lower.clone()));
        let open: bool = filtering || ui.data(|d| d.get_temp(open_id)).unwrap_or(false);
        row(app, ui, &t, r, has_kids, open, open_id);
        if has_kids && !open {
            skip_below = Some(r.depth);
        }
    }
    if all.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("No keywords yet")).color(t.text_dim));
    }
}

fn row(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, r: &Row, has_kids: bool, open: bool, open_id: egui::Id) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    register(ui.ctx(), format!("keywordList:{}", r.path), rect);
    let edge = ui.clip_rect().right().min(rect.right());
    let indent = 14.0 * r.depth as f32;
    let sel = app.session.filter.keyword.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(&r.path));
    if sel {
        ui.painter().rect_filled(rect.shrink2(vec2(2.0, 1.0)), 3.0, t.canvas);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect.shrink2(vec2(2.0, 1.0)), 3.0, t.hover.gamma_multiply(0.6));
    }
    // the mark: ✓ every selected photo has it, – some do; a click toggles it on the selection
    let (has, of) = mark(app, &r.path);
    let m = Rect::from_center_size(pos2(rect.left() + 10.0 + indent, rect.center().y), vec2(14.0, 14.0));
    register(ui.ctx(), format!("keywordMark:{}", r.path), m);
    let mr = ui.interact(m, egui::Id::new(("kwlist-mark", r.path.to_lowercase())), Sense::click());
    ui.painter().rect_stroke(m, 2.0, egui::Stroke::new(1.0, if mr.hovered() { t.text } else { t.text_dim }), egui::StrokeKind::Inside);
    let stroke = egui::Stroke::new(1.6, t.text);
    if has > 0 && has == of {
        // a check mark, drawn (no font glyph needed)
        ui.painter().line_segment([m.center() + vec2(-4.0, 0.0), m.center() + vec2(-1.0, 3.0)], stroke);
        ui.painter().line_segment([m.center() + vec2(-1.0, 3.0), m.center() + vec2(4.0, -3.5)], stroke);
    } else if has > 0 {
        ui.painter().line_segment([m.center() + vec2(-3.5, 0.0), m.center() + vec2(3.5, 0.0)], stroke);
    }
    if mr.on_hover_text(crate::i18n::tr("Add to or remove from the selected photos")).clicked() && of > 0 {
        let key = if has == of { "removeKeywords" } else { "addKeywords" };
        let _ = app.run("photo.setMeta", json!({key: [r.path]}));
    }
    // disclosure triangle
    if has_kids {
        let c = pos2(rect.left() + 26.0 + indent, rect.center().y);
        let tr = Rect::from_center_size(c, vec2(14.0, 18.0));
        register(ui.ctx(), format!("keywordListToggle:{}", r.path), tr);
        let tresp = ui.interact(tr, egui::Id::new(("kwlist-tri", r.path.to_lowercase())), Sense::click());
        super::classic::triangle(ui.painter(), c, open, t.text_dim);
        if tresp.clicked() {
            ui.data_mut(|d| d.insert_temp(open_id, !open));
        }
    }
    let person = attrs_of(app, &r.path).is_some_and(|a| a.person);
    let excluded = attrs_of(app, &r.path).is_some_and(|a| a.exclude_on_export);
    let name = if excluded { format!("[{}]", r.name) } else { r.name.clone() };
    let col = if r.count == 0 { t.text_dim } else { t.text_label };
    let lr = ui.painter().text(pos2(rect.left() + 36.0 + indent, rect.center().y), Align2::LEFT_CENTER, &name, t.font(12.5), col);
    if person {
        crate::icons::paint(
            ui.painter(),
            Rect::from_min_size(pos2(lr.right() + 5.0, rect.center().y - 6.0), vec2(12.0, 12.0)),
            Icon::FaceBox,
            t.text_dim,
        );
    }
    ui.painter().text(pos2(edge - 10.0, rect.center().y), Align2::RIGHT_CENTER, r.count.to_string(), t.font(11.5), t.text_dim);
    let resp = resp.on_hover_text(&r.path);
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(r.path) };
        // like the sidebar's keyword rows: the filter applies to all photos
        super::left::browse_all_photos(app, !sel);
        let _ = app.run("library.filter", json!({"keyword": v}));
    }
    resp.context_menu(|ui| edit_menu(app, ui, &r.path));
}

/// The row menu: the keyword's attributes, edited in place, and the library-wide actions.
fn edit_menu(app: &mut DacApp, ui: &mut egui::Ui, path: &str) {
    let a = attrs_of(app, path).cloned().unwrap_or_default();
    ui.label(egui::RichText::new(path.replace('|', " › ")).strong());
    let sid = egui::Id::new(("kwlist-syn", path.to_lowercase()));
    let mut syn: String = ui.data(|d| d.get_temp(sid)).unwrap_or_else(|| a.synonyms.join(", "));
    ui.label(crate::i18n::tr("Synonyms"));
    let r = ui.add(egui::TextEdit::singleline(&mut syn).hint_text(crate::i18n::tr("comma separated")).desired_width(200.0));
    ui.data_mut(|d| d.insert_temp(sid, syn.clone()));
    if r.lost_focus() {
        let _ = app.run("keyword.setAttributes", json!({"keyword": path, "synonyms": syn}));
        ui.data_mut(|d| d.remove::<String>(sid));
    }
    for (label, key, on) in [
        ("Include on Export", "includeOnExport", !a.exclude_on_export),
        ("Export Containing Keywords", "exportParents", !a.no_parents_on_export),
        ("Export Synonyms", "exportSynonyms", !a.no_synonyms_on_export),
        ("Person", "person", a.person),
    ] {
        let mut v = on;
        if ui.checkbox(&mut v, crate::i18n::tr(label)).changed() {
            let _ = app.run("keyword.setAttributes", json!({"keyword": path, key: v}));
        }
    }
    ui.separator();
    let has_sel = app.session.active().is_some();
    if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Add to Selected Photos"))).clicked() {
        let _ = app.run("photo.setMeta", json!({"addKeywords": [path]}));
        ui.close();
    }
    if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Remove from Selected Photos"))).clicked() {
        let _ = app.run("photo.setMeta", json!({"removeKeywords": [path]}));
        ui.close();
    }
    if ui.button(crate::i18n::tr("Create Keyword Tag inside…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
            title: "Create Keyword Tag".into(),
            hint: "Keyword".into(),
            value: format!("{path}|"),
            command: "keyword.create".into(),
            params: json!({}),
            key: "keyword".into(),
        });
        ui.close();
    }
    if ui.button(crate::i18n::tr("Use as Keyword Shortcut (⇧K)")).clicked() {
        let _ = app.run("keyword.setShortcut", json!({"keyword": path}));
        ui.close();
    }
    if ui.button(crate::i18n::tr("Rename Keyword…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: path.to_string(), to: path.to_string() });
        ui.close();
    }
    if ui.button(crate::i18n::tr("Delete Keyword")).clicked() {
        let _ = app.run("keyword.delete", json!({"keyword": path}));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_keywords_join_the_tree_in_order() {
        let mut attrs = BTreeMap::new();
        attrs.insert("Places|Italy|Rome".to_string(), Default::default());
        let r = rows(&[], &attrs);
        let paths: Vec<&str> = r.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, ["Places", "Places|Italy", "Places|Italy|Rome"]);
        assert_eq!(r.iter().map(|x| x.depth).collect::<Vec<_>>(), [0, 1, 2]);
    }
}
