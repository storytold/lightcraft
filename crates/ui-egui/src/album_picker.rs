//! An album picker: a button that opens a popup to pick an album or a smart album, by browsing a
//! tree like the sidebar's or by typing part of its name.
//!
//! Standalone: it edits an `Option<u64>` album id over a list of [`AlbumEntry`]s, in tree order
//! ([`entries_from`] builds it from a catalog in the sidebar's order). Folders are for browsing,
//! not picking. An entry can be `blocked` with a reason: shown greyed, the reason on hover.
//!
//! With nothing typed it shows the tree, folders closed except those holding the current choice.
//! Typing lists the albums whose path ("Folder / Name") holds every word typed, names starting
//! with it first; ↑ / ↓ move through them, Enter picks, Esc closes. Widget ids for tests and agents:
//! `albumPicker:<id>` (the button), `albumPickerSearch:<id>`, `albumPickerFolder:<folder>:<id>`
//! and `albumPickerItem:<album>:<id>`.

use egui::{Response, RichText, Ui, vec2};

use crate::icons::Icon;
use crate::widgets::register;

/// One row of the picker: an album, a smart album or a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumEntry {
    pub id: u64,
    pub name: String,
    pub parent: Option<u64>,
    /// How deep in folders (0 at the top), from [`set_depths`].
    pub depth: usize,
    pub folder: bool,
    pub smart: bool,
    /// Why it can't be picked, when it can't (shown on hover).
    pub blocked: Option<String>,
}

/// Every album, smart album and folder of `cat` in the sidebar's order (folders first, the user's
/// order, else by name), each folder followed by what it holds. `blocked` says why an album can't
/// be picked, if so.
pub fn entries_from(cat: &lightcraft_catalog::Catalog, blocked: impl Fn(&lightcraft_catalog::Album) -> Option<String>) -> Vec<AlbumEntry> {
    let kids = cat.album_children_by_parent();
    let mut out = Vec::new();
    // depth-first, guarded against a parent loop in a damaged catalog
    let mut seen = std::collections::HashSet::new();
    fn walk(
        parent: Option<lightcraft_catalog::AlbumId>,
        kids: &std::collections::HashMap<Option<lightcraft_catalog::AlbumId>, Vec<&lightcraft_catalog::Album>>,
        blocked: &dyn Fn(&lightcraft_catalog::Album) -> Option<String>,
        seen: &mut std::collections::HashSet<lightcraft_catalog::AlbumId>,
        out: &mut Vec<AlbumEntry>,
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        for a in kids.get(&parent).map(Vec::as_slice).unwrap_or_default() {
            if !seen.insert(a.id) {
                continue;
            }
            out.push(AlbumEntry {
                id: a.id.0,
                name: a.name.clone(),
                parent: a.parent.map(|p| p.0),
                depth,
                folder: a.folder,
                smart: a.is_smart(),
                blocked: if a.folder { None } else { blocked(a) },
            });
            if a.folder {
                walk(Some(a.id), kids, blocked, seen, out, depth + 1);
            }
        }
    }
    walk(None, &kids, &blocked, &mut seen, &mut out, 0);
    out
}

/// Where each id sits in `entries`, so walking up the folders costs their depth, not a scan.
fn index(entries: &[AlbumEntry]) -> std::collections::HashMap<u64, usize> {
    entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect()
}

/// [`ancestors`] with a prebuilt [`index`].
fn ancestors_in(entries: &[AlbumEntry], by_id: &std::collections::HashMap<u64, usize>, id: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut at = by_id.get(&id).and_then(|&i| entries.get(i)).and_then(|e| e.parent);
    while let Some(p) = at {
        // a parent loop in a damaged catalog ends the walk
        if out.contains(&p) || out.len() > 64 {
            break;
        }
        out.push(p);
        at = by_id.get(&p).and_then(|&i| entries.get(i)).and_then(|e| e.parent);
    }
    out
}

/// [`path_label`] with a prebuilt [`index`].
fn path_in(entries: &[AlbumEntry], by_id: &std::collections::HashMap<u64, usize>, id: u64) -> String {
    let Some(entry) = by_id.get(&id).and_then(|&i| entries.get(i)) else { return String::new() };
    let mut parts: Vec<&str> =
        ancestors_in(entries, by_id, id).iter().rev().filter_map(|p| by_id.get(p).and_then(|&i| entries.get(i))).map(|e| e.name.as_str()).collect();
    parts.push(&entry.name);
    parts.join(" / ")
}

/// Fill in each entry's depth from its parents (for lists built by hand).
pub fn set_depths(entries: &mut [AlbumEntry]) {
    let by_id = index(entries);
    let depths: Vec<usize> = entries.iter().map(|e| ancestors_in(entries, &by_id, e.id).len()).collect();
    for (e, d) in entries.iter_mut().zip(depths) {
        e.depth = d;
    }
}

/// The folders holding album `id`, nearest first; empty at the top level or for an unknown id.
pub fn ancestors(entries: &[AlbumEntry], id: u64) -> Vec<u64> {
    ancestors_in(entries, &index(entries), id)
}

/// "Folder / Sub / Name" for album `id`; empty for an unknown id.
pub fn path_label(entries: &[AlbumEntry], id: u64) -> String {
    path_in(entries, &index(entries), id)
}

/// The rows a tree shows with the folders in `open` open: indexes into `entries`.
pub fn visible_tree(entries: &[AlbumEntry], open: &[u64]) -> Vec<usize> {
    let by_id = index(entries);
    let open: std::collections::HashSet<u64> = open.iter().copied().collect();
    (0..entries.len()).filter(|&i| ancestors_in(entries, &by_id, entries[i].id).iter().all(|a| open.contains(a))).collect()
}

/// The albums (not folders) whose path holds every word of `query`, any case: those whose own
/// name starts with the query first, then the rest, each in tree order. Indexes into `entries`.
pub fn search(entries: &[AlbumEntry], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let by_id = index(entries);
    let hits: Vec<usize> = (0..entries.len())
        .filter(|&i| !entries[i].folder)
        .filter(|&i| {
            let path = path_in(entries, &by_id, entries[i].id).to_lowercase();
            words.iter().all(|w| path.contains(w))
        })
        .collect();
    let (first, rest): (Vec<usize>, Vec<usize>) = hits.into_iter().partition(|&i| entries[i].name.to_lowercase().starts_with(&query));
    first.into_iter().chain(rest).collect()
}

/// A small filled triangle centred on `c`: pointing down (`down`), else right.
fn triangle(p: &egui::Painter, c: egui::Pos2, down: bool, color: egui::Color32) {
    let pts = if down {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// What the open popup remembers between frames.
#[derive(Clone, Default)]
struct State {
    query: String,
    open: Vec<u64>,
    /// The highlighted search result (↑ / ↓, Enter).
    highlight: usize,
    focus: bool,
}

/// A button that edits an album id; see the module docs.
pub struct AlbumPicker<'a> {
    id: String,
    value: &'a mut Option<u64>,
    entries: &'a [AlbumEntry],
    width: f32,
}

impl<'a> AlbumPicker<'a> {
    /// `id` must be unique among the pickers on screen (it names their widgets and keeps their
    /// state apart); `value` is the album picked (`None`: none yet); `entries` in tree order.
    pub fn new(id: impl Into<String>, value: &'a mut Option<u64>, entries: &'a [AlbumEntry]) -> Self {
        AlbumPicker { id: id.into(), value, entries, width: 180.0 }
    }

    /// The button's width (180 by default).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// The button and, while open, the popup. The response is `changed()` when an album was picked.
    pub fn show(self, ui: &mut Ui) -> Response {
        let AlbumPicker { id, value, entries, width } = self;
        let chosen = value.filter(|v| entries.iter().any(|e| e.id == *v && !e.folder));
        let text = chosen.map_or_else(|| crate::i18n::tr("Choose an album…").to_string(), |c| path_label(entries, c));
        // the name, and a drop-down arrow painted at the right (the UI font has no arrow glyph)
        let mut button = ui.add_sized(vec2(width, ui.spacing().interact_size.y), egui::Button::new(text).truncate().right_text(" "));
        let arrow_color = crate::theme::Tokens::get(ui.ctx()).icon;
        triangle(ui.painter(), egui::pos2(button.rect.right() - 10.0, button.rect.center().y), true, arrow_color);
        register(ui.ctx(), format!("albumPicker:{id}"), button.rect);
        let key = egui::Id::new(("albumPicker", id.as_str()));
        if button.clicked() {
            // open on the chosen album's folders, with the search empty and ready for typing
            let open = chosen.map(|c| ancestors(entries, c)).unwrap_or_default();
            ui.data_mut(|d| d.insert_temp(key, State { open, focus: true, ..Default::default() }));
        }
        let mut state: State = ui.data(|d| d.get_temp(key)).unwrap_or_default();
        let mut picked: Option<u64> = None;
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(width.max(260.0));
            ui.set_max_width(width.max(260.0) + 120.0);
            let search_box =
                ui.add(egui::TextEdit::singleline(&mut state.query).hint_text(crate::i18n::tr("Search albums")).desired_width(f32::INFINITY));
            register(ui.ctx(), format!("albumPickerSearch:{id}"), search_box.rect);
            if state.focus {
                search_box.request_focus();
                state.focus = false;
            }
            if search_box.changed() {
                state.highlight = 0;
            }
            let t = crate::theme::Tokens::get(ui.ctx());
            // `scroll`: bring the row into view (the arrow keys just moved to it); never every frame,
            // which would fight the mouse wheel
            let row = |ui: &mut Ui, e: &AlbumEntry, label: &str, indent: f32, highlighted: bool, scroll: bool, picked: &mut Option<u64>| {
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                    let icon = if e.smart { Icon::SmartAlbum } else { Icon::Album };
                    crate::icons::paint(ui.painter(), r, icon, if e.blocked.is_some() { t.text_disabled } else { t.icon });
                    let resp = ui.add_enabled(e.blocked.is_none(), egui::Button::selectable(Some(e.id) == chosen || highlighted, label));
                    register(ui.ctx(), format!("albumPickerItem:{}:{id}", e.id), resp.rect);
                    let resp = match &e.blocked {
                        Some(why) => resp.on_disabled_hover_text(why.as_str()),
                        None => resp,
                    };
                    if resp.clicked() {
                        *picked = Some(e.id);
                    }
                    if scroll {
                        resp.scroll_to_me(None);
                    }
                });
            };
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                if state.query.trim().is_empty() {
                    for i in visible_tree(entries, &state.open) {
                        let e = &entries[i];
                        let indent = e.depth as f32 * 16.0;
                        if e.folder {
                            // a folder row folds and unfolds, like the sidebar's: triangle, icon, name
                            let open = state.open.contains(&e.id);
                            let height = ui.spacing().interact_size.y;
                            let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::click());
                            if r.hovered() {
                                ui.painter().rect_filled(rect, 3.0, t.hover);
                            }
                            let x = rect.left() + indent;
                            triangle(ui.painter(), egui::pos2(x + 7.0, rect.center().y), open, if r.hovered() { t.text } else { t.text_dim });
                            let icon = egui::Rect::from_center_size(egui::pos2(x + 23.0, rect.center().y), vec2(14.0, 14.0));
                            crate::icons::paint(ui.painter(), icon, Icon::Folder, t.icon);
                            ui.painter().text(
                                egui::pos2(x + 36.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &e.name,
                                egui::TextStyle::Button.resolve(ui.style()),
                                t.text_label,
                            );
                            register(ui.ctx(), format!("albumPickerFolder:{}:{id}", e.id), r.rect);
                            // what assistive tools announce: the folder, and whether it is open
                            let name = e.name.clone();
                            r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, &name));
                            if r.clicked() {
                                // the click took the keyboard from the search box: give it back
                                state.focus = true;
                                if open {
                                    state.open.retain(|o| *o != e.id);
                                } else {
                                    state.open.push(e.id);
                                }
                            }
                        } else {
                            // past the triangle column, so names line up with folder names
                            row(ui, e, &e.name, indent + 14.0, false, false, &mut picked);
                        }
                    }
                    if entries.iter().all(|e| e.folder) {
                        ui.label(RichText::new(crate::i18n::tr("No albums yet")).weak());
                    }
                } else {
                    let hits = search(entries, &state.query);
                    // ↑ / ↓ move through the results that can be picked, Enter picks one
                    let pickable: Vec<usize> = hits.iter().copied().filter(|&i| entries[i].blocked.is_none()).collect();
                    let (down, up, enter) = ui.input_mut(|i| {
                        (
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                        )
                    });
                    if down {
                        state.highlight = (state.highlight + 1).min(pickable.len().saturating_sub(1));
                    }
                    if up {
                        state.highlight = state.highlight.saturating_sub(1);
                    }
                    let highlighted = pickable.get(state.highlight).copied();
                    if enter {
                        match highlighted {
                            Some(i) => picked = Some(entries[i].id),
                            // Enter took the keyboard from the search box and picked nothing: give it back
                            None => state.focus = true,
                        }
                    }
                    let by_id = index(entries);
                    for i in &hits {
                        let e = &entries[*i];
                        row(
                            ui,
                            e,
                            &path_in(entries, &by_id, e.id),
                            0.0,
                            highlighted == Some(*i),
                            (up || down) && highlighted == Some(*i),
                            &mut picked,
                        );
                    }
                    if hits.is_empty() {
                        ui.label(RichText::new(crate::i18n::tr("No albums match")).weak());
                    }
                }
            });
            // Esc closes the popup and stops there: the dialog around it keeps its edits
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                crate::widgets::take_escape(ui.ctx());
                ui.close();
            }
            if picked.is_some() {
                ui.close();
            }
        });
        ui.data_mut(|d| d.insert_temp(key, state));
        if let Some(p) = picked {
            *value = Some(p);
            button.mark_changed();
        }
        button
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: u64, name: &str, parent: Option<u64>, folder: bool, smart: bool) -> AlbumEntry {
        AlbumEntry { id, name: name.into(), parent, depth: 0, folder, smart, blocked: None }
    }

    /// [UTILS] (folder) > Excluded | Hidden Photos (smart), Uncurated (smart); Trip; Weddings >
    /// Tuscany Wedding
    fn sample() -> Vec<AlbumEntry> {
        let mut v = vec![
            e(1, "[UTILS]", None, true, false),
            e(2, "Excluded | Hidden Photos", Some(1), false, true),
            e(3, "Uncurated", Some(1), false, true),
            e(4, "Trip", None, false, false),
            e(5, "Weddings", None, true, false),
            e(6, "Tuscany Wedding", Some(5), false, true),
        ];
        set_depths(&mut v);
        v
    }

    #[test]
    fn paths_and_depths_follow_the_folders() {
        let v = sample();
        assert_eq!(path_label(&v, 2), "[UTILS] / Excluded | Hidden Photos");
        assert_eq!(path_label(&v, 4), "Trip");
        assert_eq!(path_label(&v, 99), "");
        assert_eq!(v.iter().map(|e| e.depth).collect::<Vec<_>>(), vec![0, 1, 1, 0, 0, 1]);
        assert_eq!(ancestors(&v, 6), vec![5]);
        assert!(ancestors(&v, 4).is_empty());
    }

    #[test]
    fn the_tree_hides_what_closed_folders_hold() {
        let v = sample();
        let ids = |open: &[u64]| visible_tree(&v, open).into_iter().map(|i| v[i].id).collect::<Vec<_>>();
        assert_eq!(ids(&[]), vec![1, 4, 5], "folders closed: only the top level");
        assert_eq!(ids(&[1]), vec![1, 2, 3, 4, 5]);
        assert_eq!(ids(&[1, 5]), vec![1, 2, 3, 4, 5, 6]);
    }

    /// Big libraries stay quick: 20,000 albums in nested folders are laid out and searched in well
    /// under a second (folder paths are worked out once, not per row by scanning every album).
    #[test]
    fn many_albums_stay_quick() {
        let mut v = Vec::new();
        let mut paths = Vec::new();
        for f in 0..200u64 {
            v.push(e(f * 101 + 1, &format!("Folder {f}"), None, true, false));
            for a in 0..100u64 {
                v.push(e(f * 101 + 2 + a, &format!("Wedding {f}-{a}"), Some(f * 101 + 1), false, a % 2 == 0));
                paths.push(format!("folder {f} / wedding {f}-{a}"));
            }
        }
        let want = paths.iter().filter(|p| ["folder", "7", "wedding"].iter().all(|w| p.contains(w))).count();
        let start = std::time::Instant::now();
        set_depths(&mut v);
        let open: Vec<u64> = (0..200u64).map(|f| f * 101 + 1).collect();
        assert_eq!(visible_tree(&v, &open).len(), v.len());
        assert_eq!(search(&v, "folder 7 wedding").len(), want);
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
    }

    /// Every word typed is in the album's path (folders included), in any case; names that start
    /// with it come first; folders themselves aren't results.
    #[test]
    fn search_finds_albums_by_any_part_of_their_path() {
        let v = sample();
        let ids = |q: &str| search(&v, q).into_iter().map(|i| v[i].id).collect::<Vec<_>>();
        assert_eq!(ids("excl"), vec![2]);
        assert_eq!(ids("UTILS"), vec![2, 3], "a folder's name finds what it holds");
        assert_eq!(ids("utils uncur"), vec![3], "every word must match");
        assert_eq!(ids("t"), vec![4, 6, 2, 3], "names starting with it first (Trip, Tuscany Wedding), then the rest, each in sidebar order");
        assert_eq!(ids("  "), Vec::<u64>::new(), "an empty search isn't a search");
        assert_eq!(ids("zzz"), Vec::<u64>::new());
        let mut accents = vec![e(7, "Été", None, false, false)];
        set_depths(&mut accents);
        assert_eq!(search(&accents, "été").len(), 1);
    }
}
