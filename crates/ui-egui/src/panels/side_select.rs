//! Several rows of a sidebar list chosen at once (Cmd/Ctrl+click, Shift+click): what the left
//! panel's Albums, Folders, Keywords and By Date and the right panel's Keyword List do with the
//! click. The rule for the click itself is [`crate::multi_select`]'s.
//!
//! What is chosen in a list of the left panel is what the grid shows, so it lives in the session
//! (`library.source`, `library.filter`); only the anchor for Shift+click is kept here. The
//! Keyword List's rows don't change the grid: its chosen rows are kept here.
//!
//! A plain click does what it always did.

use lightcraft_catalog::AlbumId;
use lightcraft_catalog::keywords::same as same_keyword;
use lightcraft_engine::LibrarySource;
use serde_json::json;

use crate::LightcraftApp;
use crate::multi_select::{Click, ListRows};

/// What the sidebars keep for choosing several rows.
#[derive(Clone, Debug, Default)]
pub struct SideState {
    /// The library all of this is about (its generation): another library starts afresh.
    library: Option<u64>,
    /// The catalog revision the Keyword List's picks were last checked against.
    picks_at: u64,
    pub albums: ListRows<u64>,
    pub folders: ListRows<String>,
    pub keywords: ListRows<String>,
    pub dates: ListRows<String>,
    pub keyword_list: ListRows<String>,
    /// The other keywords picked in the Keyword List beside `keyword_list_selected` (which is the
    /// one picked last, the one Edit takes), in the order picked.
    pub keyword_list_also: Vec<String>,
}

fn same_id(a: &u64, b: &u64) -> bool {
    a == b
}
// (a list's keys are `String`s: these are passed where two keys are told alike)
#[allow(clippy::ptr_arg)]
fn same_date(a: &String, b: &String) -> bool {
    a == b
}
#[allow(clippy::ptr_arg)]
fn same_kw(a: &String, b: &String) -> bool {
    same_keyword(a, b)
}
#[allow(clippy::ptr_arg)]
fn same_dir(a: &String, b: &String) -> bool {
    super::left::same_folder(a, b)
}

fn how(ui: &egui::Ui) -> Click {
    Click::of(ui.input(|i| i.modifiers))
}

/// Each frame, before the panels: a library that was switched forgets everything kept here (its
/// rows are another library's), and a keyword picked in the Keyword List that is gone is let go.
pub(crate) fn frame(app: &mut LightcraftApp) {
    let library = app.session.library_generation();
    if app.ui.side.library != Some(library) {
        app.ui.side = SideState { library: Some(library), ..Default::default() };
    }
    // a keyword picked in the Keyword List that is gone since (deleted, merged away, undone) is
    // not picked any more. Looked up in the cached keyword tree, once per change to the library
    // and while keywords are picked: never a pass over the photos per frame.
    let revision = app.session.catalog.revision;
    if app.ui.side.picks_at != revision && (!app.ui.side.keyword_list_also.is_empty() || app.ui.keyword_list_selected.is_some()) {
        let tree = app.caches.keyword_tree(&app.session.catalog);
        let there = |k: &String| super::keyword_list::in_tree(&tree, k);
        app.ui.side.keyword_list_also.retain(there);
        if app.ui.keyword_list_selected.as_ref().is_some_and(|k| !there(k)) {
            app.ui.keyword_list_selected = app.ui.side.keyword_list_also.pop();
        }
    }
    app.ui.side.picks_at = revision;
}

/// A new frame of the left panel: the rows of the last one are what a click now is on.
pub(crate) fn begin_left(app: &mut LightcraftApp) {
    let s = &mut app.ui.side;
    s.albums.begin_frame();
    s.folders.begin_frame();
    s.keywords.begin_frame();
    s.dates.begin_frame();
}

// ---- Albums

/// Whether album `id` is one the grid shows the photos of.
pub(crate) fn album_shown(app: &LightcraftApp, id: AlbumId) -> bool {
    app.session.source == LibrarySource::Album(id) || (matches!(app.session.source, LibrarySource::Album(_)) && app.session.also_albums.contains(&id))
}

fn albums_shown(app: &LightcraftApp) -> Vec<u64> {
    app.session.source_albums().iter().map(|a| a.0).collect()
}

/// A click on the row of album `id`: it is shown alone, or with Cmd/Ctrl or Shift beside the
/// others chosen.
pub(crate) fn album_clicked(app: &mut LightcraftApp, ui: &egui::Ui, id: AlbumId) {
    let chosen = albums_shown(app);
    let how = how(ui);
    let keys = app.ui.side.albums.click(&chosen, &id.0, how, &same_id);
    show_albums(app, &keys);
}

/// Show these albums (none: All Photos).
fn show_albums(app: &mut LightcraftApp, ids: &[u64]) {
    match ids {
        [] => app.act("library.source", json!({"kind": "all"})),
        [one] => app.act("library.source", json!({"kind": "album", "id": one})),
        many => app.act("library.source", json!({"kind": "album", "ids": many})),
    };
}

// ---- Folders (of the library)

/// Whether the library folder `path` is one the grid shows the photos of.
pub(crate) fn folder_shown(app: &LightcraftApp, path: &str) -> bool {
    app.session.source == LibrarySource::LibraryFolder
        && (app.session.library_folder.as_deref().is_some_and(|f| super::left::same_folder(f, path))
            || app.session.also_library_folders.iter().any(|f| super::left::same_folder(f, path)))
}

/// A click on the row of library folder `path`.
pub(crate) fn folder_clicked(app: &mut LightcraftApp, ui: &egui::Ui, path: &str) {
    let chosen = app.session.source_library_folders();
    let how = how(ui);
    let keys = app.ui.side.folders.click(&chosen, &path.to_string(), how, &same_dir);
    match keys.as_slice() {
        [] => app.act("library.source", json!({"kind": "all"})),
        // a source like an album or a Local folder: it replaces what the grid showed
        [one] => app.act("library.source", json!({"kind": "libraryFolder", "path": one})),
        many => app.act("library.source", json!({"kind": "libraryFolder", "paths": many})),
    };
}

// ---- Keywords and By Date (filters)

/// Whether keyword `path` is one the grid is filtered by.
pub(crate) fn keyword_shown(app: &LightcraftApp, path: &str) -> bool {
    let f = &app.session.filter;
    f.keyword.iter().chain(&f.keywords).any(|k| same_keyword(k, path))
}

/// A click on the Keywords row `path`: the grid shows the photos with it (again: every photo),
/// or with Cmd/Ctrl or Shift the photos with any of the keywords chosen.
pub(crate) fn keyword_clicked(app: &mut LightcraftApp, ui: &egui::Ui, path: &str) {
    let chosen = app.session.filter.keywords_wanted();
    let how = how(ui);
    // a plain click on the one keyword shown clears it, as it always did
    let clears = how == Click::Plain && chosen.len() == 1 && chosen.iter().any(|k| same_keyword(k, path));
    let keys = if clears {
        app.ui.side.keywords.anchor = None;
        Vec::new()
    } else {
        app.ui.side.keywords.click(&chosen, &path.to_string(), how, &same_kw)
    };
    // (All Photos first, then the filter: the order a plain click always had)
    super::left::browse_all_photos(app, !keys.is_empty());
    match keys.as_slice() {
        [] => app.act("library.filter", json!({"keyword": null, "keywords": []})),
        [one] => app.act("library.filter", json!({"keyword": one, "keywords": []})),
        many => app.act("library.filter", json!({"keyword": null, "keywords": many})),
    };
}

/// Whether the By Date row `key` is one the grid is filtered by.
pub(crate) fn date_shown(app: &LightcraftApp, key: &str) -> bool {
    let f = &app.session.filter;
    f.date.iter().chain(&f.dates).any(|d| d == key)
}

/// A click on the By Date row `key`, like [`keyword_clicked`].
pub(crate) fn date_clicked(app: &mut LightcraftApp, ui: &egui::Ui, key: &str) {
    let chosen = app.session.filter.dates_wanted();
    let how = how(ui);
    let clears = how == Click::Plain && chosen.len() == 1 && chosen.iter().any(|d| d == key);
    let keys = if clears {
        app.ui.side.dates.anchor = None;
        Vec::new()
    } else {
        app.ui.side.dates.click(&chosen, &key.to_string(), how, &same_date)
    };
    super::left::browse_all_photos(app, !keys.is_empty());
    match keys.as_slice() {
        [] => app.act("library.filter", json!({"date": null, "dates": []})),
        [one] => app.act("library.filter", json!({"date": one, "dates": []})),
        many => app.act("library.filter", json!({"date": null, "dates": many})),
    };
}

// ---- The Keyword List (right panel)

/// The keywords picked in the Keyword List, in the order picked (the last is the one Edit takes).
pub(crate) fn list_picked(app: &LightcraftApp) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // (those that are gone, deleted or merged away, were let go of by `frame`)
    for k in app.ui.side.keyword_list_also.iter().chain(&app.ui.keyword_list_selected) {
        if !out.iter().any(|o| same_keyword(o, k)) {
            out.push(k.clone());
        }
    }
    out
}

/// Whether `path` is picked in the Keyword List.
pub(crate) fn list_is_picked(app: &LightcraftApp, path: &str) -> bool {
    app.ui.keyword_list_selected.iter().chain(&app.ui.side.keyword_list_also).any(|k| same_keyword(k, path))
}

/// Pick these keywords in the Keyword List (the last is the one Edit takes).
pub(crate) fn list_pick(app: &mut LightcraftApp, mut keys: Vec<String>) {
    app.ui.keyword_list_selected = keys.pop();
    app.ui.side.keyword_list_also = keys;
}

/// A click on the Keyword List's row `path`.
pub(crate) fn list_clicked(app: &mut LightcraftApp, ui: &egui::Ui, path: &str) {
    let chosen = list_picked(app);
    let how = how(ui);
    let keys = app.ui.side.keyword_list.click(&chosen, &path.to_string(), how, &same_kw);
    list_pick(app, keys);
}
