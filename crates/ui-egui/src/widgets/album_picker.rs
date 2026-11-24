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

use craft::theme::Variant;
use craft::ui::buttons::Button;
use craft::ui::contract::overlay::{Dismiss, Outside};
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::controls::disclosure::DisclosureGlyph;
use craft::ui::popover::Popover;
use craft::ui::runtime::focus::Initial;
use craft::ui::primitives::{Glyph, HAlign, IconSpec};
use egui::{Rect, Response, RichText, Ui, vec2};

use crate::icons::Icon;
/// Do not publish invisible SDK layout probes as real automation targets.
fn register(ui: &Ui, id: impl Into<String>, response: &Response) {
    if !measuring(ui) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

fn measuring(ui: &Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

/// Reuse LightCraft's original artwork in the SDK face; no extra interaction or focus stop.
#[derive(craft::ui::contract::MemoProps)]
struct AlbumIcon(Icon);

impl Glyph for AlbumIcon {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, color: egui::Color32, _: bool) {
        crate::icons::paint(painter, rect, self.0, color);
    }
}

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

/// What the open popup remembers between frames.
#[derive(Clone, Default)]
struct State {
    query: String,
    open: Vec<u64>,
    /// The highlighted search result (↑ / ↓, Enter).
    highlight: usize,
    focus: bool,
    popup_open: bool,
}

/// The popup's maximum height: the search field and the list.
const MENU_HEIGHT: f32 = 320.0;

/// A button that edits an album id; see the module docs.
pub struct AlbumPicker<'a> {
    id: String,
    value: &'a mut Option<u64>,
    entries: &'a [AlbumEntry],
    width: f32,
    empty_text: Option<String>,
}

impl<'a> AlbumPicker<'a> {
    /// `id` must be unique among the pickers on screen (it names their widgets and keeps their
    /// state apart); `value` is the album picked (`None`: none yet); `entries` in tree order.
    pub fn new(id: impl Into<String>, value: &'a mut Option<u64>, entries: &'a [AlbumEntry]) -> Self {
        AlbumPicker { id: id.into(), value, entries, width: 180.0, empty_text: None }
    }

    /// The button's width (180 by default).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Text for an empty choice (already localized by the caller); the default is "Choose an album…".
    pub fn empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = Some(text.into());
        self
    }

    /// The button and, while open, the popup. The response is `changed()` when an album was picked.
    pub fn show(self, ui: &mut Ui) -> Response {
        self.show_with_choices(ui, |_| false)
    }

    /// Draw caller-owned choices before the search (e.g. None / New album in the import review).
    /// Return true when an extra choice was picked to close the popup. The caller handles those
    /// actions; `changed()` still reports only an album picked from the tree or search.
    pub fn show_with_choices(self, ui: &mut Ui, choices: impl FnMut(&mut Ui) -> bool) -> Response {
        let AlbumPicker { id, value, entries, width, empty_text } = self;
        let chosen = value.filter(|v| entries.iter().any(|e| e.id == *v && !e.folder));
        let text = chosen.map_or_else(|| empty_text.unwrap_or_else(|| crate::i18n::tr("Choose an album…").to_string()), |c| path_label(entries, c));
        let size = vec2(width.min(ui.available_width()).max(1.0), ui.spacing().interact_size.y.max(24.0));
        // A caller may measure the whole picker in an SDK body. It must not open a popup, run
        // caller choices, claim focus, register hits, or replace the real popup/search state.
        if measuring(ui) {
            return ui.allocate_exact_size(size, egui::Sense::hover()).1;
        }
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        // the closed box of a dropdown: the album at the start, the chevron at the end
        let mut button = super::menu_field_with_id(ui, rect, egui::Id::unique(("albumPickerButton", id.as_str())), &text, &text);
        button = button.on_hover_text(text);
        register(ui, format!("albumPicker:{id}"), &button);
        let key = egui::Id::unique(("albumPicker", id.as_str()));
        let mut state: State = ui.data(|d| d.get_temp(key)).unwrap_or_default();
        if craft::ui::contract::clicked(&button) {
            // Explicit SDK activation drives the legacy popup host, including long releases and
            // keyboard activation; never narrow it back to egui's independent click predicate.
            state.popup_open = !state.popup_open;
            if state.popup_open {
                let open = chosen.map(|c| ancestors(entries, c)).unwrap_or_default();
                state = State { open, focus: true, popup_open: true, ..Default::default() };
            }
        }
        let mut popup_open = state.popup_open;
        let body_interactive = popup_open;
        let mut state = state;
        let mut picked: Option<u64> = None;
        let mut choices = choices;
        let mut chose_extra = false;
        let search_name = format!("albumPickerSearch:{id}");
        Popover::new(crate::i18n::tr("Albums").to_owned(), Bind::bound(&mut popup_open))
            .id(key.with("popover"))
            .opened_by(&button)
            .dismiss(Dismiss::new().outside(Outside::Absorb))
            // the search field takes the keyboard when the popup opens, as typing is how most picks start
            .initial_focus(Initial::FirstField)
            .show(ui, |ui, cx| {
                // A closing popup only fades: its search and rows are gone, for input and for automation alike.
                if !body_interactive {
                    return;
                }
                // A wide popup remembered before a window resize must still fit the current viewport.
                let max_width = (ui.ctx().content_rect().width() - 24.0).max(1.0);
                ui.set_min_width(width.max(260.0).min(max_width));
                ui.set_max_width((width.max(260.0) + 120.0).min(max_width));
                // The caller's choices and the search are real controls: a measuring run of the popup lays out
                // the rest only, never runs the caller's actions or moves the keyboard.
                let real = !measuring(ui);
                // A measuring run leaves the caller's choices the room they took in the last real one, so the popup is
                // measured as tall as it is drawn.
                let choices_room = key.with("choices-height");
                if real {
                    let top = ui.cursor().top();
                    let picked_extra = choices(ui);
                    let height = ui.cursor().top() - top;
                    ui.data_mut(|d| d.insert_temp(choices_room, height));
                    if picked_extra {
                        chose_extra = true;
                        cx.request_close();
                        return;
                    }
                } else if let Some(height) = ui.data(|d| d.get_temp::<f32>(choices_room)).filter(|h| *h > 0.0) {
                    ui.add_space(height);
                }
                // Escape closes the popup and stops there: the dialog around it keeps its edits. The framework popup
                // takes it as the top layer's cancel; with the keyboard in the search box egui still has the key.
                if real && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    crate::widgets::take_escape(ui.ctx());
                    cx.request_close();
                    return;
                }
                // The project's framework text field, registered as `albumPickerSearch:<id>` on its text part. Its
                // Return is the path-search navigator's Enter below; its Escape closes the popup.
                let before = state.query.clone();
                let field = crate::text_field::TextField::singleline(&search_name, &mut state.query)
                    .hint(crate::i18n::tr("Search albums"))
                    .width(f32::INFINITY)
                    .show(ui);
                let search_box = field.response.clone();
                if real && field.cancelled() {
                    cx.request_close();
                    chose_extra = true;
                    return;
                }
                let returned = real && field.ending == Some(crate::text_field::Ending::Return);
                if real && state.focus {
                    crate::text_field::focus(ui.ctx(), &search_name);
                    state.focus = false;
                }
                if state.query != before {
                    state.highlight = 0;
                }
                // `scroll`: bring the row into view (the arrow keys just moved to it); never every frame,
                // which would fight the mouse wheel
                let row = |ui: &mut Ui, e: &AlbumEntry, label: &str, indent: f32, highlighted: bool, scroll: bool, picked: &mut Option<u64>| {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y.max(24.0)), egui::Sense::hover());
                    let rect = Rect::from_min_max(rect.min + vec2(indent.min((rect.width() - 1.0).max(0.0)), 0.0), rect.max);
                    let icon = AlbumIcon(if e.smart { Icon::SmartAlbum } else { Icon::Album });
                    let mut row = Button::new(label.to_owned())
                        .id(egui::Id::unique(("albumPickerItem", id.as_str(), e.id)))
                        .variant(Variant::GHOST)
                        .align(HAlign::Start)
                        .icon(IconSpec::glyph(&icon))
                        .selected(Some(e.id) == chosen || highlighted)
                        .enabled(e.blocked.is_none())
                        .show_at(ui, rect);
                    row.response = row.response.on_hover_text(label);
                    if let Some(why) = &e.blocked {
                        row.response = row.response.on_disabled_hover_text(why.as_str());
                    }
                    register(ui, format!("albumPickerItem:{}:{id}", e.id), &row.response);
                    if !measuring(ui) && row.clicked() {
                        *picked = Some(e.id);
                    }
                    if scroll {
                        row.response.scroll_to_me(None);
                    }
                };
                // (the empty states sit outside the list, which shows nothing then)
                if state.query.trim().is_empty() {
                    if entries.iter().all(|e| e.folder) {
                        ui.label(RichText::new(crate::i18n::tr("No albums yet")).weak());
                    }
                } else if search(entries, &state.query).is_empty() {
                    ui.label(RichText::new(crate::i18n::tr("No albums match")).weak());
                }
                let list_height = (MENU_HEIGHT - search_box.rect.height() - ui.spacing().item_spacing.y).max(48.0);
                egui::ScrollArea::vertical().id_salt(key.with("list")).max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
                    if state.query.trim().is_empty() {
                        for i in visible_tree(entries, &state.open) {
                            let e = &entries[i];
                            let indent = e.depth as f32 * 16.0;
                            if e.folder {
                                // The SDK row owns the whole disclosure action, including its keyboard focus.
                                let open = state.open.contains(&e.id);
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y.max(24.0)), egui::Sense::hover());
                                let rect = Rect::from_min_max(rect.min + vec2(indent.min((rect.width() - 1.0).max(0.0)), 0.0), rect.max);
                                let icon = AlbumIcon(Icon::Folder);
                                let arrow = DisclosureGlyph::new(if open { 1.0 } else { 0.0 });
                                let r = Button::new(e.name.clone())
                                    .id(egui::Id::unique(("albumPickerFolder", id.as_str(), e.id)))
                                    .variant(Variant::GHOST)
                                    .align(HAlign::Start)
                                    .icon(IconSpec::glyph(&icon))
                                    .trailing_icon(IconSpec::glyph(&arrow))
                                    .show_at(ui, rect);
                                register(ui, format!("albumPickerFolder:{}:{id}", e.id), &r.response);
                                crate::widgets::announce_disclosure(&r.response, true, open, &e.name);
                                if !measuring(ui) && r.clicked() {
                                    // the click took the keyboard from the search box: give it back
                                    state.focus = true;
                                    if open {
                                        state.open.retain(|o| *o != e.id);
                                    } else {
                                        state.open.push(e.id);
                                    }
                                }
                            } else {
                                // The shared SDK icon column aligns album and folder names.
                                row(ui, e, &e.name, indent, false, false, &mut picked);
                            }
                        }
                    } else {
                        let hits = search(entries, &state.query);
                        // ↑ / ↓ move through the results that can be picked, Enter picks one
                        let pickable: Vec<usize> = hits.iter().copied().filter(|&i| entries[i].blocked.is_none()).collect();
                        let (down, up, enter) = if measuring(ui) {
                            (false, false, false)
                        } else {
                            ui.input_mut(|i| {
                                (
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || returned,
                                )
                            })
                        };
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
                            row(ui, e, &path_in(entries, &by_id, e.id), 0.0, highlighted == Some(*i), (up || down) && highlighted == Some(*i), &mut picked);
                        }
                    }
                });
                if picked.is_some() {
                    cx.request_close();
                }
            });
        // A pick or a caller's choice closes the popup now, whatever pass the framework takes the close in.
        if picked.is_some() || chose_extra {
            popup_open = false;
        }
        state.popup_open = popup_open;
        ui.data_mut(|d| d.insert_temp(key, state));
        if let Some(p) = picked {
            *value = Some(p);
            button.mark_changed();
        }
        button
    }
}

#[cfg(test)]
#[path = "album_picker/tests.rs"]
mod sdk_tests;

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

    /// Cycles, dangling parents and excessively deep chains finish with bounded paths and depths.
    #[test]
    fn damaged_parent_chains_are_bounded() {
        let mut entries = vec![e(1, "Loop A", Some(2), true, false), e(2, "Loop B", Some(1), true, false), e(3, "Photo", Some(1), false, false)];
        set_depths(&mut entries);
        assert!(ancestors(&entries, 3).len() <= 3);
        assert_eq!(search(&entries, "photo"), vec![2]);
        assert_eq!(visible_tree(&entries, &[1, 2]), vec![0, 1, 2]);
        let mut deep: Vec<_> = (1..1000).map(|id| e(id, "Deep", Some(id + 1), true, false)).collect();
        deep.push(e(1000, "End", None, true, false));
        set_depths(&mut deep);
        assert!(deep.iter().all(|entry| entry.depth <= 65));
        assert!(ancestors(&deep, 1).len() <= 65);
        assert!(path_label(&deep, 1).split(" / ").count() <= 66);
        assert_eq!(path_label(&[e(1, "Orphan", Some(99), false, false)], 1), "Orphan");
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
