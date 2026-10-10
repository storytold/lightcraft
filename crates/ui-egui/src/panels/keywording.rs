//! The Library module's Keywording panel (Classic): the selected photos' keywords as editable
//! text (keywords only some of them have end in `*` and are left alone), a field that adds
//! keywords, nine suggestions (keywords used together with these, else the most used), the
//! keyword set (⌥1–⌥9; built-in sets and the user's) and the keyword shortcut (⇧K).

use serde_json::json;

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The keywords every target photo has, and those only some have (in first-seen order).
pub(crate) fn shared_keywords(app: &DacApp) -> (Vec<String>, Vec<String>) {
    let ids = app.session.targets(&json!({}));
    let lists: Vec<Vec<String>> = ids.iter().filter_map(|id| app.session.catalog.photo(*id)).map(|p| p.meta.keywords.clone()).collect();
    let mut all: Vec<String> = Vec::new();
    for l in &lists {
        for k in l {
            if !all.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                all.push(k.clone());
            }
        }
    }
    let (common, partial): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|k| lists.iter().all(|l| l.iter().any(|x| x.eq_ignore_ascii_case(k))));
    (common, partial)
}

/// The text of the keyword tags field: shared keywords, then the partial ones marked `*`.
pub(crate) fn tags_text(common: &[String], partial: &[String]) -> String {
    common.iter().cloned().chain(partial.iter().map(|k| format!("{k}*"))).collect::<Vec<_>>().join(", ")
}

/// What editing the tags text changes: (added, removed). Entries ending in `*` stay as they were;
/// a partial keyword whose `*` was taken off is added to every photo.
pub(crate) fn tags_diff(text: &str, common: &[String], partial: &[String]) -> (Vec<String>, Vec<String>) {
    let entries: Vec<String> = text.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    let kept = |k: &String| entries.iter().any(|e| e.trim_end_matches('*').trim().eq_ignore_ascii_case(k));
    let mut added: Vec<String> = Vec::new();
    for e in &entries {
        if e.ends_with('*') {
            continue;
        }
        if !common.iter().any(|c| c.eq_ignore_ascii_case(e)) && !added.iter().any(|a| a.eq_ignore_ascii_case(e)) {
            added.push(e.clone());
        }
    }
    let removed: Vec<String> = common.iter().chain(partial.iter()).filter(|k| !kept(k)).cloned().collect();
    (added, removed)
}

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ids = app.session.targets(&json!({}));
    if ids.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("No photo selected")).color(t.text_dim));
        return;
    }
    let (common, partial) = shared_keywords(app);
    // the text field, reset whenever the selection or its keywords change
    let now = tags_text(&common, &partial);
    let sig = egui::Id::new(("kwd-tags", ids.first().map(|i| i.0), ids.len(), now.clone()));
    let tid = egui::Id::new("kwd-tags-text");
    let mut text: String =
        ui.data(|d| d.get_temp::<(egui::Id, String)>(tid)).filter(|(s, _)| *s == sig).map(|(_, x)| x).unwrap_or_else(|| now.clone());
    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Tags")).size(11.5).color(t.text_dim));
    let r = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(f32::INFINITY));
    register(ui.ctx(), "field:keywordTags", r.rect);
    ui.data_mut(|d| d.insert_temp(tid, (sig, text.clone())));
    if r.lost_focus() && text != now {
        let (added, removed) = tags_diff(&text, &common, &partial);
        if !added.is_empty() || !removed.is_empty() {
            let _ = app.run("photo.setMeta", json!({"addKeywords": added, "removeKeywords": removed}));
        }
        ui.data_mut(|d| d.remove::<(egui::Id, String)>(tid));
    }
    // a field that adds (Enter)
    let aid = egui::Id::new("kwd-add");
    let mut add: String = ui.data(|d| d.get_temp(aid)).unwrap_or_default();
    let r = ui.add(egui::TextEdit::singleline(&mut add).hint_text(crate::i18n::tr("Click here to add keywords")).desired_width(f32::INFINITY));
    register(ui.ctx(), "field:keywordAdd", r.rect);
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !add.trim().is_empty() {
        let kws: Vec<String> = add.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let _ = app.run("photo.setMeta", json!({"addKeywords": kws}));
        add.clear();
    }
    let typed = add.rsplit(',').next().unwrap_or("").trim().to_string();
    ui.data_mut(|d| d.insert_temp(aid, add));
    ui.add_space(6.0);
    // suggestions: completions of what is typed, else co-occurring, else most used
    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Suggestions")).size(11.5).color(t.text_dim));
    let mut have = common.clone();
    have.extend(partial.iter().cloned());
    let suggestions = (*app.caches.suggestions(&app.session.catalog, &have, &typed, 9)).clone();
    grid(app, ui, "kwd-suggest", &suggestions, &common, |k| json!({"addKeywords": [k]}));
    ui.add_space(6.0);
    super::right::keyword_set(app, ui, &common);
    ui.add_space(6.0);
    shortcut(app, ui);
}

/// Up to nine keywords as a 3 × 3 grid of buttons; a click runs `photo.setMeta` with `params(k)`.
fn grid(app: &mut DacApp, ui: &mut egui::Ui, id: &str, kws: &[String], have: &[String], params: impl Fn(&str) -> serde_json::Value) {
    let t = Tokens::get(ui.ctx());
    if kws.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("None yet")).size(11.5).color(t.text_dim));
        return;
    }
    let gap = ui.spacing().item_spacing.x;
    let bw = ((ui.available_width() - 2.0 * gap - 1.0) / 3.0).floor().max(30.0);
    egui::Grid::new(id).num_columns(3).spacing([gap, 4.0]).show(ui, |ui| {
        for (i, k) in kws.iter().take(9).enumerate() {
            let on = have.iter().any(|x| x.eq_ignore_ascii_case(k));
            let short = k.rsplit('|').next().unwrap_or(k);
            let r = ui
                .add_sized([bw, 20.0], egui::Button::new(egui::RichText::new(short).size(11.5)).selected(on).truncate())
                .on_hover_text(k.replace('|', " › "));
            register(ui.ctx(), format!("kwdSuggest:{k}"), r.rect);
            if r.clicked() {
                let _ = app.run("photo.setMeta", params(k));
            }
            if i % 3 == 2 {
                ui.end_row();
            }
        }
    });
}

/// The keyword shortcut: the keyword ⇧K toggles; set here or with ⌥⇧K (`keyword.setShortcut`).
fn shortcut(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let current = app.session.keyword_shortcut.clone().unwrap_or_default();
    let sid = egui::Id::new("kwd-shortcut");
    // what is typed lives only while the field is being edited; otherwise it shows the setting
    let mut text: String = ui.data(|d| d.get_temp(sid)).unwrap_or_else(|| current.clone());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Keyword Shortcut (⇧K)")).size(11.5).color(t.text_dim));
        let r =
            ui.add(egui::TextEdit::singleline(&mut text).hint_text(crate::i18n::tr("none")).desired_width((ui.available_width() - 2.0).max(40.0)));
        register(ui.ctx(), "field:keywordShortcut", r.rect);
        if r.lost_focus() && text.trim() != current {
            let _ = app.run("keyword.setShortcut", json!({"keyword": text.trim()}));
        }
        if r.has_focus() {
            ui.data_mut(|d| d.insert_temp(sid, text.clone()));
        } else {
            ui.data_mut(|d| d.remove::<String>(sid));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn editing_the_tags_adds_and_removes_but_leaves_partial_ones() {
        let (common, partial) = (v(&["rome", "food"]), v(&["night"]));
        assert_eq!(tags_text(&common, &partial), "rome, food, night*");
        // removed food, added sea, night* left alone
        assert_eq!(tags_diff("rome, night*, sea", &common, &partial), (v(&["sea"]), v(&["food"])));
        // night's * taken off: every photo gets it
        assert_eq!(tags_diff("rome, food, night", &common, &partial), (v(&["night"]), v(&[])));
        // emptied: everything goes
        assert_eq!(tags_diff("", &common, &partial), (v(&[]), v(&["rome", "food", "night"])));
        // case and spaces don't count as changes
        assert_eq!(tags_diff(" ROME ,food,  night* ", &common, &partial), (v(&[]), v(&[])));
    }
}
