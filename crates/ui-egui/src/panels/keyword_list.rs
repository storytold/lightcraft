//! The Keyword List (Lightroom Classic's Keyword List panel), in the right panel's Keywords: every
//! keyword of the library, those without photos too, as a tree with photo counts. A tick box per
//! keyword gives it to the selected photos or takes it away; the arrow shows the photos with it.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::keywords::{KeywordInfo, KeywordNode, same};
use lightcraft_catalog::{Catalog, PhotoId};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

/// Rows are this tall; each level is indented this much.
const ROW_H: f32 = 24.0;
const INDENT: f32 = 14.0;

/// The Keyword List section: a filter box, then the rows.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    crate::widgets::divider(ui);
    // the title, where a dragged keyword goes back to the top level
    let (title, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    register(ui.ctx(), "keywordList:topLevel", title);
    let dragging = app.ui.dragging_keyword.clone();
    let (pointer, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
    let over_title = shown_under(ui, title, pointer);
    if dragging.is_some() && over_title {
        ui.painter().rect_stroke(title.shrink2(vec2(16.0, 6.0)), 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    let heading = if dragging.is_some() { crate::i18n::tr("Drop here for the top level") } else { crate::i18n::tr("Keyword List") };
    ui.painter().text(pos2(title.left() + 24.0, title.center().y + 2.0), Align2::LEFT_CENTER, heading, t.semibold(15.0), t.text);
    if released
        && over_title
        && let Some(k) = dragging.clone()
    {
        drop_keyword(app, ui.ctx(), &k, None);
    }
    let fid = egui::Id::new("keyword-list-filter");
    let mut filter: String = ui.data(|d| d.get_temp(fid)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        crate::text_field::TextField::singleline("field:keywordFilter", &mut filter)
            .hint(crate::i18n::tr("Filter Keywords"))
            .width(ui.available_width() - 82.0)
            .show(ui);
        // + creates a keyword (inside the picked one), − deletes the picked one
        let plus = ui.button("+").on_hover_text(crate::i18n::tr("Create Keyword Tag"));
        register(ui.ctx(), "keywordList:create", plus.rect);
        if plus.clicked() {
            app.ui.dialog = Some(create_dialog(app));
        }
        let picked = app.ui.keyword_list_selected.clone().filter(|k| in_tree(&app.caches.keyword_tree(&app.session.catalog), k));
        let minus = ui.add_enabled(picked.is_some(), egui::Button::new("−")).on_hover_text(crate::i18n::tr("Delete Keyword"));
        register(ui.ctx(), "keywordList:delete", minus.rect);
        if minus.clicked()
            && let Some(k) = picked
        {
            app.ui.dialog = Some(delete_dialog(app, &k));
        }
    });
    ui.data_mut(|d| d.insert_temp(fid, filter.clone()));
    ui.add_space(6.0);
    let tree = app.caches.keyword_tree(&app.session.catalog);
    let open = app.ui.keyword_list_open.clone();
    let rows = rows(&tree, &filter, &|p| open.iter().any(|o| same(o, p)));
    if rows.is_empty() {
        let none = if filter.trim().is_empty() { "No keywords yet" } else { "No keywords match" };
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.label(egui::RichText::new(crate::i18n::tr(none)).color(t.text_dim));
        });
    }
    let selection = app.session.targets(&serde_json::Value::Null);
    let ticks = app.caches.keyword_ticks(&app.session.catalog, &selection);
    for r in &rows {
        row(app, ui, r, &selection, &ticks, filter.trim().is_empty());
    }
    ui.add_space(12.0);
}

/// After the keyword `from` became `to` (renamed, moved or merged), the list's pick and open
/// levels follow it, and the levels containing it open so that it stays in sight.
pub(crate) fn follow(app: &mut LightcraftApp, from: &str, to: &str) {
    use lightcraft_catalog::keywords::{is_under, reparent};
    let to = app.session.catalog.keyword_path(to).unwrap_or_else(|| to.to_string());
    if let Some(k) = app.ui.keyword_list_selected.clone()
        && is_under(&k, from)
    {
        app.ui.keyword_list_selected = Some(reparent(&k, from, &to));
    }
    for o in &mut app.ui.keyword_list_open {
        if is_under(o, from) {
            *o = reparent(o, from, &to).to_lowercase();
        }
    }
    let levels: Vec<&str> = to.split('|').collect();
    for n in 1..levels.len() {
        let parent = levels.get(..n).map(|l| l.join("|").to_lowercase()).unwrap_or_default();
        if !app.ui.keyword_list_open.iter().any(|o| same(o, &parent)) {
            app.ui.keyword_list_open.push(parent);
        }
    }
}

/// The pointer is on the part of `rect` that is shown: inside it and inside the panel's visible
/// (scrolled) area, so a drop never lands on a row or title hidden under the top bar.
fn shown_under(ui: &egui::Ui, rect: Rect, pointer: Option<egui::Pos2>) -> bool {
    pointer.is_some_and(|p| rect.contains(p) && ui.clip_rect().contains(p))
}

/// A keyword being dragged, each frame (after the panels, which take the drop): its name follows
/// the pointer, and the drag ends with the button's release wherever it is (the list may be gone
/// by then) or with Esc.
pub fn drag_feedback(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(keyword) = app.ui.dragging_keyword.clone() else { return };
    let (released, down, at, esc) =
        ctx.input(|i| (i.pointer.any_released(), i.pointer.any_down(), i.pointer.latest_pos(), i.key_pressed(egui::Key::Escape)));
    if released || !down || esc {
        app.ui.dragging_keyword = None;
        return;
    }
    let Some(at) = at else { return };
    let t = Tokens::get(ctx);
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    let name = keyword.rsplit('|').next().unwrap_or(&keyword).to_string();
    egui::Area::new(egui::Id::new("drag-keyword")).order(egui::Order::Tooltip).interactable(false).fixed_pos(at + vec2(14.0, 10.0)).show(ctx, |ui| {
        egui::Frame::NONE.fill(t.accent).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
            ui.label(egui::RichText::new(name).color(Color32::WHITE).font(t.semibold(12.0)));
        });
    });
}

/// Drop the dragged `keyword` inside `parent` (`None`: the top level). Where one of its name is
/// already, ask before merging the two; where it is already, nothing happens.
fn drop_keyword(app: &mut LightcraftApp, ctx: &egui::Context, keyword: &str, parent: Option<&str>) {
    app.ui.dragging_keyword = None;
    let cat = &app.session.catalog;
    let leaf = keyword.rsplit('|').next().unwrap_or(keyword);
    let parent = parent.map(|p| cat.keyword_path(p).unwrap_or_else(|| p.to_string()));
    if parent.as_deref().is_some_and(|p| lightcraft_catalog::keywords::is_under(p, keyword)) {
        return;
    }
    let to = parent.as_deref().map_or_else(|| leaf.to_string(), |p| format!("{p}|{leaf}"));
    if same(&to, keyword) {
        return;
    }
    if cat.has_keyword(&to) {
        app.ui.dialog = Some(Dialog::MoveKeyword { keyword: keyword.to_string(), parent });
        return;
    }
    match app.run("keyword.move", json!({"keyword": keyword, "parent": parent})) {
        Ok(_) => follow(app, keyword, &to),
        Err(e) => app.toast(ctx, e),
    }
}

/// One keyword's row: the triangle, the tick box, the name, the count, and on hover the arrow.
/// `can_fold`: the triangle opens and closes the level (not while a filter opens it).
fn row(app: &mut LightcraftApp, ui: &mut egui::Ui, r: &Row, selection: &[PhotoId], ticks: &Ticks, can_fold: bool) {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
    register(ui.ctx(), format!("keywordRow:{}", r.path), rect);
    if resp.drag_started() {
        app.ui.dragging_keyword = Some(r.path.clone());
    }
    drop_target(app, ui, rect, &r.path);
    let picked = app.ui.keyword_list_selected.as_deref().is_some_and(|k| same(k, &r.path));
    let inner = Rect::from_min_max(rect.min + vec2(16.0, 0.0), rect.max - vec2(16.0, 0.0));
    if picked {
        ui.painter().rect_filled(inner, 4.0, t.canvas);
    } else if resp.hovered() {
        ui.painter().rect_filled(inner, 4.0, t.hover.gamma_multiply(0.6));
    }
    let x = inner.left() + 4.0 + r.depth as f32 * INDENT;
    let cy = rect.center().y;
    // the triangle
    if r.has_children {
        let c = pos2(x + 6.0, cy);
        let tri = Rect::from_center_size(c, vec2(14.0, 14.0));
        let tr = ui.interact(tri, egui::Id::new(("keyword-list-tri", r.path.to_lowercase())), Sense::click());
        register(ui.ctx(), format!("keywordRowToggle:{}", r.path), tri);
        let pts = if r.open {
            vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
        } else {
            vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
        };
        // while a filter opens the levels the triangle does nothing: it doesn't light up either
        let lit = tr.hovered() && can_fold;
        ui.painter().add(egui::Shape::convex_polygon(pts, if lit { t.text } else { t.text_dim }, Stroke::NONE));
        if tr.clicked() && can_fold {
            let key = r.path.to_lowercase();
            if r.open {
                app.ui.keyword_list_open.retain(|o| !same(o, &key));
            } else {
                app.ui.keyword_list_open.push(key);
            }
        }
    }
    // the tick box: does the selection have it?
    let state = ticks.tick(&r.path);
    let boxr = Rect::from_center_size(pos2(x + 22.0, cy), vec2(13.0, 13.0));
    let on = !selection.is_empty();
    // (it senses drags too, so a press there that moves doesn't pick the keyword up)
    let sense = if on { Sense::click_and_drag() } else { Sense::hover() };
    let tb = ui.interact(boxr.expand(3.0), egui::Id::new(("keyword-list-tick", r.path.to_lowercase())), sense);
    register(ui.ctx(), format!("keywordCheck:{}", r.path), boxr);
    let edge = if on { t.text_label } else { t.text_dim.gamma_multiply(0.5) };
    match state {
        Tick::All => {
            ui.painter().rect_filled(boxr, 2.0, t.accent);
            let pts = vec![boxr.left_center() + vec2(2.5, 0.0), boxr.center_bottom() + vec2(-1.0, -3.0), boxr.right_top() + vec2(-2.5, 3.0)];
            ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6, Color32::WHITE)));
        }
        Tick::Some => {
            ui.painter().rect_stroke(boxr, 2.0, Stroke::new(1.0, edge), StrokeKind::Inside);
            ui.painter().line_segment([boxr.left_center() + vec2(3.0, 0.0), boxr.right_center() - vec2(3.0, 0.0)], Stroke::new(1.6, t.text));
        }
        Tick::No => {
            ui.painter().rect_stroke(boxr, 2.0, Stroke::new(1.0, edge), StrokeKind::Inside);
        }
    }
    // (with nothing selected it does nothing, and says nothing)
    let tb = if on {
        tb.on_hover_text(crate::i18n::tr(match state {
            Tick::All => "Remove from Selected Photos",
            _ => "Add to Selected Photos",
        }))
    } else {
        tb
    };
    if tb.clicked() {
        let key = if state == Tick::All { "removeKeywords" } else { "addKeywords" };
        let _ = app.run("photo.setMeta", json!({key: [r.path]}));
    }
    // the count, then the arrow left of it (shown on hover)
    let count = ui.painter().layout_no_wrap(r.count.to_string(), t.font(12.0), t.text_dim);
    let count_rect = Rect::from_min_size(pos2(inner.right() - 6.0 - count.size().x, cy - count.size().y / 2.0), count.size());
    register(ui.ctx(), format!("keywordCount:{}", r.path), count_rect);
    ui.painter().galley(count_rect.min, count, t.text_dim);
    let arrow = Rect::from_center_size(pos2(count_rect.left() - 12.0, cy), vec2(16.0, 16.0));
    let ar = ui.interact(arrow, egui::Id::new(("keyword-list-show", r.path.to_lowercase())), Sense::click());
    register(ui.ctx(), format!("keywordShow:{}", r.path), arrow);
    if resp.hovered() || ar.hovered() {
        ui.painter().text(arrow.center(), Align2::CENTER_CENTER, "→", t.font(13.0), if ar.hovered() { t.text } else { t.text_dim });
    }
    if ar.on_hover_text(crate::i18n::tr("Show Photos with Keyword")).clicked() {
        super::left::browse_all_photos(app, true);
        let _ = app.run("library.filter", json!({"keyword": r.path}));
    }
    // the name, cut short before the arrow
    let room = (arrow.left() - 4.0 - (x + 34.0)).max(0.0);
    let font = t.font(13.0);
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), t.text_label).size().x;
    let name = if measure(&r.name) <= room { r.name.clone() } else { crate::widgets::elide_head(&r.name, room, measure) };
    let label = ui.painter().text(pos2(x + 34.0, cy), Align2::LEFT_CENTER, name, font.clone(), if picked { t.text } else { t.text_label });
    // where new keywords go (Put New Keywords Inside This Keyword): a dot after the name
    if app.session.catalog.default_keyword_parent().as_deref().is_some_and(|k| same(k, &r.path)) {
        let dot = Rect::from_center_size(pos2(label.right() + 7.0, cy), vec2(6.0, 6.0));
        ui.painter().circle_filled(dot.center(), 3.0, t.accent);
        register(ui.ctx(), format!("keywordDefault:{}", r.path), dot);
    }
    let resp = resp.on_hover_text(r.path.replace('|', " › "));
    if resp.clicked() {
        app.ui.keyword_list_selected = Some(r.path.clone());
    }
    if resp.double_clicked() {
        app.ui.dialog = Some(edit_dialog(app, &r.path));
    }
    resp.context_menu(|ui| menu(app, ui, &r.path, selection));
}

/// A row while a keyword or photos are dragged: outlined under the pointer when the drop would
/// do something (a keyword never goes inside itself), and a release there does it: nests the
/// keyword inside this one, or gives this keyword to the photos.
fn drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect, path: &str) {
    let (pointer, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
    if !shown_under(ui, rect, pointer) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let outline = |ui: &egui::Ui| ui.painter().rect_stroke(rect.shrink2(vec2(16.0, 1.0)), 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    if let Some(k) = app.ui.dragging_keyword.clone() {
        if lightcraft_catalog::keywords::is_under(path, &k) {
            return;
        }
        outline(ui);
        if released {
            drop_keyword(app, ui.ctx(), &k, Some(path));
        }
    } else if let Some(ids) = app.ui.dragging_photos.clone() {
        outline(ui);
        if released {
            // only the photos that didn't have it get it
            let lacking = |id: &u64| {
                app.session
                    .catalog
                    .photo(PhotoId(*id))
                    .is_some_and(|p| !p.meta.keywords.iter().any(|k| same(&lightcraft_catalog::keywords::clean(k), path)))
            };
            let n = ids.iter().filter(|id| lacking(id)).count();
            app.ui.dragging_photos = None;
            match app.run("photo.setMeta", json!({"ids": ids, "addKeywords": [path]})) {
                Ok(_) => app.toast(
                    ui.ctx(),
                    crate::i18n::tr_format!(
                        "Added “{keyword}” to {n} photo{}",
                        if n == 1 { "" } else { "s" },
                        keyword = path.replace('|', " › "),
                        n = n
                    ),
                ),
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
    }
}

/// A keyword's context menu.
fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, path: &str, selection: &[PhotoId]) {
    let name = path.replace('|', " › ");
    let item = |ui: &mut egui::Ui, id: &str, label: &str, enabled: bool| {
        let r = ui.add_enabled(enabled, egui::Button::new(label));
        register(ui.ctx(), format!("keywordMenu:{id}"), r.rect);
        r.clicked()
    };
    if item(ui, "create", &crate::i18n::tr_format!("Create Keyword Tag Inside “{name}”…", name = name), true) {
        let mut d = create_dialog(app);
        if let Dialog::KeywordTag { parent, inside, .. } = &mut d {
            *parent = Some(path.to_string());
            *inside = true;
        }
        app.ui.dialog = Some(d);
    }
    if item(ui, "edit", crate::i18n::tr("Edit Keyword Tag…"), true) {
        app.ui.dialog = Some(edit_dialog(app, path));
    }
    let is_default = app.session.catalog.default_keyword_parent().as_deref().is_some_and(|k| same(k, path));
    let label = format!("{}{}", if is_default { "✓ " } else { "" }, crate::i18n::tr("Put New Keywords Inside This Keyword"));
    if item(ui, "defaultParent", &label, true) {
        let _ = app.run("keyword.setDefaultParent", json!({"keyword": if is_default { serde_json::Value::Null } else { json!(path) }}));
    }
    ui.separator();
    let some = !selection.is_empty();
    if item(ui, "add", crate::i18n::tr("Add to Selected Photos"), some) {
        let _ = app.run("photo.setMeta", json!({"addKeywords": [path]}));
    }
    if item(ui, "remove", crate::i18n::tr("Remove from Selected Photos"), some) {
        let _ = app.run("photo.setMeta", json!({"removeKeywords": [path]}));
    }
    if item(ui, "show", crate::i18n::tr("Show Photos with Keyword"), true) {
        super::left::browse_all_photos(app, true);
        let _ = app.run("library.filter", json!({"keyword": path}));
    }
    ui.separator();
    if item(ui, "purge", crate::i18n::tr("Purge Unused Keywords"), true) {
        let _ = app.run("keyword.purgeUnused", json!({}));
    }
    if item(ui, "delete", crate::i18n::tr("Delete Keyword…"), true) {
        app.ui.dialog = Some(delete_dialog(app, path));
    }
}

/// Create Keyword Tag: inside the keyword picked in the list, else the default parent.
pub(crate) fn create_dialog(app: &LightcraftApp) -> Dialog {
    let parent =
        app.ui.keyword_list_selected.clone().filter(|k| app.session.catalog.has_keyword(k)).or_else(|| app.session.catalog.default_keyword_parent());
    let d = KeywordInfo::default();
    Dialog::KeywordTag {
        editing: None,
        name: String::new(),
        inside: parent.is_some(),
        parent,
        synonyms: String::new(),
        include_on_export: d.include_on_export,
        export_containing: d.export_containing,
        export_synonyms: d.export_synonyms,
        person: d.person,
        add_to_selected: false,
    }
}

/// Edit Keyword Tag for `path`, showing its name and attributes.
pub(crate) fn edit_dialog(app: &LightcraftApp, path: &str) -> Dialog {
    let path = app.session.catalog.keyword_path(path).unwrap_or_else(|| path.to_string());
    let info = app.session.catalog.keyword_info(&path).cloned().unwrap_or_default();
    Dialog::KeywordTag {
        name: path.rsplit('|').next().unwrap_or(&path).to_string(),
        editing: Some(path),
        parent: None,
        inside: false,
        synonyms: info.synonyms.join(", "),
        include_on_export: info.include_on_export,
        export_containing: info.export_containing,
        export_synonyms: info.export_synonyms,
        person: info.person,
        add_to_selected: false,
    }
}

/// Delete Keyword, saying how many photos have it.
pub(crate) fn delete_dialog(app: &LightcraftApp, path: &str) -> Dialog {
    let keyword = app.session.catalog.keyword_path(path).unwrap_or_else(|| path.to_string());
    let count = app
        .session
        .catalog
        .photos()
        .filter(|p| p.in_library() && p.meta.keywords.iter().any(|k| lightcraft_catalog::keywords::is_under(k, &keyword)))
        .count();
    Dialog::DeleteKeyword { keyword, count }
}

/// One row of the Keyword List as drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub path: String,
    pub name: String,
    /// Levels below the top (0 = a top-level keyword).
    pub depth: usize,
    pub count: usize,
    pub has_children: bool,
    /// Its children are shown.
    pub open: bool,
}

/// The rows to draw: the tree with its levels open as `open` says (by path, any case). With a
/// `filter`, the keywords whose name contains it (any case) and the keywords containing them,
/// opened so that they show.
pub(crate) fn rows(tree: &[KeywordNode], filter: &str, open: &dyn Fn(&str) -> bool) -> Vec<Row> {
    let filter = filter.trim().to_lowercase();
    let mut out = Vec::new();
    add_rows(tree, 0, &filter, open, &mut out);
    out
}

/// A node holds a match: its name, or a name below it.
fn holds(n: &KeywordNode, filter: &str) -> bool {
    n.name.to_lowercase().contains(filter) || n.children.iter().any(|c| holds(c, filter))
}

fn add_rows(nodes: &[KeywordNode], depth: usize, filter: &str, open: &dyn Fn(&str) -> bool, out: &mut Vec<Row>) {
    for n in nodes {
        // filtering: a match, or a keyword containing one, opened down to it
        let below = n.children.iter().any(|c| holds(c, filter));
        if !filter.is_empty() && !below && !n.name.to_lowercase().contains(filter) {
            continue;
        }
        let is_open = if filter.is_empty() { open(&n.path) } else { below };
        out.push(Row { path: n.path.clone(), name: n.name.clone(), depth, count: n.count, has_children: !n.children.is_empty(), open: is_open });
        if is_open {
            add_rows(&n.children, depth + 1, filter, open, out);
        }
    }
}

/// Whether the selected photos have a keyword: the tick box's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// None of them (or nothing is selected).
    No,
    /// Some of them: the box shows a dash.
    Some,
    /// All of them.
    All,
}

/// How many of a selection's photos have each keyword itself, counted once per selection (and
/// library change) rather than per row and frame: a long list with thousands of photos selected
/// stays fast.
#[derive(Debug, Default)]
pub(crate) struct Ticks {
    photos: usize,
    /// Photos with it, by cleaned lower-case keyword.
    counts: std::collections::HashMap<String, usize>,
}

impl Ticks {
    pub(crate) fn of(catalog: &Catalog, photos: &[PhotoId]) -> Ticks {
        let photos = super::keywording::distinct(catalog, photos);
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        let mut seen = std::collections::HashSet::new();
        for p in &photos {
            seen.clear();
            for k in &p.meta.keywords {
                let key = lightcraft_catalog::keywords::clean(k).to_lowercase();
                if seen.insert(key.clone()) {
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        Ticks { photos: photos.len(), counts }
    }

    /// The tick box of `path`: a photo counts when it has the keyword itself (any case); one with
    /// only a keyword below it doesn't.
    pub(crate) fn tick(&self, path: &str) -> Tick {
        match self.counts.get(&lightcraft_catalog::keywords::clean(path).to_lowercase()).copied().unwrap_or(0) {
            0 => Tick::No,
            n if n >= self.photos => Tick::All,
            _ => Tick::Some,
        }
    }
}

/// The keyword is in the tree (any case).
fn in_tree(tree: &[KeywordNode], path: &str) -> bool {
    tree.iter().any(|n| same(&n.path, path) || (lightcraft_catalog::keywords::is_under(path, &n.path) && in_tree(&n.children, path)))
}

#[cfg(test)]
mod tests {
    use lightcraft_catalog::{Op, Photo, Source};

    use super::*;

    fn library(keywords: &[&[&str]]) -> (Catalog, Vec<PhotoId>) {
        let mut c = Catalog::new();
        let mut ids = Vec::new();
        for k in keywords {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
            p.meta.keywords = k.iter().map(|s| s.to_string()).collect();
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        (c, ids)
    }

    fn shown(rows: &[Row]) -> Vec<(String, usize, usize)> {
        rows.iter().map(|r| (r.path.clone(), r.depth, r.count)).collect()
    }

    /// The list shows the top level, and the keywords below a level once it is open.
    #[test]
    fn the_list_shows_the_levels_that_are_open() {
        let (c, _) = library(&[&["travel|Italy|Rome", "beach"], &["travel|Spain"]]);
        let tree = c.keyword_tree();
        let closed = rows(&tree, "", &|_| false);
        assert_eq!(shown(&closed), [("beach".to_string(), 0, 1), ("travel".to_string(), 0, 2)]);
        assert!(closed[1].has_children && !closed[1].open && !closed[0].has_children);
        let open = rows(&tree, "", &|p| p.eq_ignore_ascii_case("TRAVEL"));
        assert_eq!(
            shown(&open),
            [("beach".to_string(), 0, 1), ("travel".to_string(), 0, 2), ("travel|Italy".to_string(), 1, 1), ("travel|Spain".to_string(), 1, 1)]
        );
    }

    /// Filtering shows the keywords whose name holds the text, whatever its case, with the keywords
    /// containing them opened down to them; keywords below a match aren't shown unless they match.
    #[test]
    fn filtering_shows_the_matches_with_their_parents() {
        let (c, _) = library(&[&["travel|Italy|Rome", "travel|Spain|Ronda", "beach"]]);
        let tree = c.keyword_tree();
        let found = rows(&tree, "RO", &|_| false);
        assert_eq!(
            found.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["travel", "travel|Italy", "travel|Italy|Rome", "travel|Spain", "travel|Spain|Ronda"]
        );
        assert!(found.iter().filter(|r| r.has_children).all(|r| r.open));
        assert_eq!(rows(&tree, "italy", &|_| false).iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["travel", "travel|Italy"]);
        assert!(rows(&tree, "lisbon", &|_| false).is_empty());
    }

    /// The tick box: ticked when every selected photo has the keyword itself, a dash when some do,
    /// empty when none do or nothing is selected. Counted once per selection.
    #[test]
    fn the_tick_box_says_how_many_selected_photos_have_the_keyword() {
        let (c, ids) = library(&[&["Travel|Italy"], &["travel|italy", "beach"], &["travel|spain"]]);
        assert_eq!(Ticks::of(&c, &ids[..2]).tick("travel|Italy"), Tick::All, "whatever the case");
        let all = Ticks::of(&c, &ids);
        assert_eq!(all.tick("travel|Italy"), Tick::Some);
        assert_eq!(all.tick("travel"), Tick::No, "a keyword below doesn't tick its parent");
        assert_eq!(Ticks::of(&c, &[]).tick("beach"), Tick::No);
        assert_eq!(Ticks::of(&c, &ids[2..]).tick("beach"), Tick::No);
        // any letter's case, not only ASCII; a photo with it twice counts once
        let (c, ids) = library(&[&["Ärzte", "ÄRZTE"], &[]]);
        assert_eq!(Ticks::of(&c, &ids).tick("ärzte"), Tick::Some);
    }

    /// The labels the Keyword List and its dialogs pass to `tr` through variables (out of reach of
    /// a literal search) are translated in every language.
    #[test]
    fn labels_passed_through_variables_are_translated() {
        use crate::i18n::Locale;
        let labels = [
            "Include on Export",
            "Export Containing Keywords",
            "Export Synonyms",
            "Person",
            "Merge Keywords",
            "No keywords yet",
            "No keywords match",
            "Add to Selected Photos",
            "Remove from Selected Photos",
            "Keywords",
            "& Containing",
            "Will Export",
            "No keywords are exported",
            "Add to All Selected Photos",
            "Set name",
            "Edit Set…",
            "Edit Keyword Set",
            "A keyword set needs a name",
            "Recent Keywords",
            "Keywords (⌥1–⌥9)",
            "Save as a new set",
            "Import Keywords…",
            "Export Keywords…",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for code in ["de", "es", "fr", "pt-br", "ru", "uk", "ja", "zh-hans", "zh-hant"] {
            let text = std::fs::read_to_string(dir.join(format!("{code}.json"))).unwrap();
            let catalog: std::collections::HashMap<String, String> = serde_json::from_str(&text).unwrap();
            for label in labels {
                assert!(catalog.contains_key(label), "{code}: “{label}”");
            }
        }
        assert_eq!(Locale::ALL.len(), 10, "a language added: list its file above");
    }
}
