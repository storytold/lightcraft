//! Several rows of a sidebar list chosen at once (Cmd/Ctrl+click, Shift+click): what the left
//! panel's Albums, Folders, Keywords and By Date and the right panel's Keyword List do with the
//! click, and the context menu and the confirmations for several rows. The rule for the click
//! itself is [`crate::multi_select`]'s.
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
use crate::multi_select::{Click, ListRows, targets};
use crate::state::Dialog;
use crate::widgets::register;

/// A sidebar list in which several rows can be chosen and then deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideList {
    Albums,
    Folders,
    Keywords,
    KeywordList,
}

/// What the sidebars keep for choosing several rows.
#[derive(Clone, Debug, Default)]
pub struct SideState {
    /// The list the Delete key acts on: the one in which several rows were just chosen with
    /// Cmd/Ctrl or Shift, until the next click anywhere. A plain click never sets it, so after
    /// choosing an album the usual way the keys stay with the photos.
    pub keys: Option<SideList>,
    /// What the key was taken over ([`keys_mark`]): any change to it gives the key back.
    keys_at: u64,
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
    /// What Cut or Copy in a sidebar menu took, for Paste in another row's menu.
    pub clip: Option<SideClip>,
}

/// Sidebar rows taken with Cut or Copy.
#[derive(Clone, Debug, PartialEq)]
pub enum SideClip {
    /// Albums and folders of albums: Paste moves them (`cut`) or makes copies of them.
    Albums { ids: Vec<u64>, cut: bool },
    /// Keywords that were cut: Paste nests them inside another keyword. (Copied keywords are text
    /// on the system clipboard, for a keyword box.)
    Keywords { keywords: Vec<String> },
}

/// A confirmation before something happens to several rows at once.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Several {
    /// Delete albums and folders of albums: `albums` and `folders` of them named, with `inside`
    /// more albums and folders inside those folders.
    DeleteAlbums { ids: Vec<u64>, albums: usize, folders: usize, inside: usize },
    /// Delete keywords from every photo and the keyword list; `count` photos have one of them.
    DeleteKeywords { keywords: Vec<String>, count: usize },
    /// Remove folders from the library; `count` photos were imported from them.
    RemoveFolders { paths: Vec<String>, count: usize },
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

/// After a click in `list` that left `chosen` rows chosen: it takes the Delete key when several
/// were chosen with Cmd/Ctrl or Shift.
fn take_keys(app: &mut LightcraftApp, list: SideList, how: Click, chosen: usize) {
    app.ui.side.keys = (how != Click::Plain && chosen > 1).then_some(list);
    app.ui.side.keys_at = keys_mark(app);
}

/// What the Delete key was taken over: what is shown, chosen and selected, and where. When any of
/// it changes (a key, a menu, an agent), the key is the photos' again.
fn keys_mark(app: &LightcraftApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let (s, ui) = (&app.session, &app.ui);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}|{:?}|{:?}|{:?}", s.source, s.library_folder, ui.view, ui.right).hash(&mut h);
    (&s.also_albums, &s.also_library_folders, &s.filter.keyword, &s.filter.keywords).hash(&mut h);
    // (the selection by its size and ends: it can be every photo of a large library, each frame)
    (s.selection.ids.len(), s.selection.ids.first(), s.selection.ids.last(), s.selection.active).hash(&mut h);
    (&ui.keyword_list_selected, &ui.side.keyword_list_also, ui.left_panel, s.library_generation()).hash(&mut h);
    h.finish()
}

/// Each frame, before the panels and the shortcuts: a press anywhere, or any change to what the
/// key was taken over, gives the keys back to the photos (a Cmd/Shift+click in a list takes them
/// again when it is released). A library that was switched forgets everything kept here: its
/// rows are another library's.
pub(crate) fn keys_frame(app: &mut LightcraftApp, ctx: &egui::Context) {
    let library = app.session.library_generation();
    if app.ui.side.library != Some(library) {
        app.ui.side = SideState { library: Some(library), ..Default::default() };
    }
    if app.ui.side.keys.is_some() && (ctx.input(|i| i.pointer.any_pressed()) || app.ui.side.keys_at != keys_mark(app)) {
        app.ui.side.keys = None;
    }
    // a keyword picked in the Keyword List that is gone since (deleted, merged away, undone) is
    // not picked any more. Looked up in the cached keyword tree, once per change to the library
    // and only while several are picked: never a pass over the photos per frame.
    let revision = app.session.catalog.revision;
    if app.ui.side.picks_at != revision && !app.ui.side.keyword_list_also.is_empty() {
        let tree = app.caches.keyword_tree(&app.session.catalog);
        let there = |k: &String| super::keyword_list::in_tree(&tree, k);
        app.ui.side.keyword_list_also.retain(there);
        if app.ui.keyword_list_selected.as_ref().is_some_and(|k| !there(k)) {
            app.ui.keyword_list_selected = app.ui.side.keyword_list_also.pop();
        }
    }
    app.ui.side.picks_at = revision;
}

/// Delete (the photos' delete shortcut, whatever key it is on), when a sidebar list has the key
/// ([`SideState::keys`]): asks before deleting (or removing from the library) the rows chosen
/// there. Returns whether it was about those rows; `false` leaves it to the photos.
pub(crate) fn delete_asked(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let Some(list) = app.ui.side.keys else { return false };
    let dialog = match list {
        SideList::Albums => Some(albums_shown(app)).filter(|ids| ids.len() > 1).map(|ids| delete_albums_dialog(app, &ids)),
        SideList::Keywords => Some(app.session.filter.keywords_wanted()).filter(|k| k.len() > 1).map(|k| delete_keywords_dialog(app, &k)),
        SideList::KeywordList => Some(list_picked(app)).filter(|k| k.len() > 1).map(|k| delete_keywords_dialog(app, &k)),
        SideList::Folders => {
            let paths = app.session.source_library_folders();
            if paths.len() > 1 && paths.iter().any(|p| lightcraft_catalog::folders::is_disk_root(p)) {
                app.toast(ctx, crate::i18n::tr("One of them is a whole disk: remove a disk by itself").to_string());
                return true;
            }
            Some(paths).filter(|p| p.len() > 1).map(|p| remove_folders_dialog(app, &p))
        }
    };
    match dialog {
        Some(d) => {
            app.ui.dialog = Some(d);
            true
        }
        // what was chosen is not there any more: the key is the photos' again, this press too
        None => {
            app.ui.side.keys = None;
            false
        }
    }
}

/// Whether the rows chosen in `list` are what the Delete key acts on now (they are outlined).
pub(crate) fn has_keys(app: &LightcraftApp, list: SideList) -> bool {
    app.ui.side.keys == Some(list)
}

/// Outline a chosen row of the list that has the Delete key.
pub(crate) fn outline(app: &LightcraftApp, ui: &egui::Ui, list: SideList, chosen: bool, row: egui::Rect, inset: f32) {
    if chosen && has_keys(app, list) {
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.painter().rect_stroke(row.shrink2(egui::vec2(inset, 1.0)), 4.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    }
}

/// The confirmation for removing several folders from the library.
fn remove_folders_dialog(app: &LightcraftApp, paths: &[String]) -> Dialog {
    let f = lightcraft_catalog::Filter { library_folders: paths.to_vec(), ..Default::default() };
    let count = app.session.catalog.query(&f, &lightcraft_catalog::Sort::default()).len();
    Dialog::Several(Several::RemoveFolders { paths: paths.to_vec(), count })
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

/// Whether album (or folder of albums) `id` is one the grid shows the photos of.
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
    take_keys(app, SideList::Albums, how, albums_shown(app).len());
}

/// Show these albums (none: All Photos).
fn show_albums(app: &mut LightcraftApp, ids: &[u64]) {
    match ids {
        [] => app.act("library.source", json!({"kind": "all"})),
        [one] => app.act("library.source", json!({"kind": "album", "id": one})),
        many => app.act("library.source", json!({"kind": "album", "ids": many})),
    };
}

/// The albums a context menu opened on album `id` acts on: those shown when it is one of
/// several, else itself.
pub(crate) fn album_targets(app: &LightcraftApp, id: AlbumId) -> Vec<u64> {
    targets(&albums_shown(app), &id.0, &same_id)
}

/// The albums a drag that picked up album `id` moves: those shown when it is one of several.
pub(crate) fn albums_dragged(app: &LightcraftApp, id: AlbumId) -> Vec<u64> {
    album_targets(app, id)
}

/// A menu item that is there but can't be used with several rows chosen, saying why.
fn single_only(ui: &mut egui::Ui, label: &str, why: &str) {
    ui.add_enabled(false, egui::Button::new(crate::i18n::tr(label))).on_disabled_hover_text(crate::i18n::tr(why));
}

/// A menu item of a several-rows menu, registered as `sideMenu:<id>` for the control channel.
fn item(ui: &mut egui::Ui, id: &str, label: String, enabled: bool) -> egui::Response {
    let r = ui.add_enabled(enabled, egui::Button::new(label));
    register(ui.ctx(), format!("sideMenu:{id}"), r.rect);
    r
}

/// The context menu for several albums (and folders of albums) chosen at once.
pub(crate) fn albums_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, ids: &[u64]) {
    let n = ids.len();
    let cat = &app.session.catalog;
    let chosen: Vec<AlbumId> = ids.iter().map(|i| AlbumId(*i)).collect();
    // show them all, select their photos, then the dialog
    let has_photos = chosen.iter().any(|a| cat.album_count(*a) > 0);
    if item(ui, "albums:export", crate::i18n::tr_format!("Export {n} Albums…", n = n), has_photos).clicked() {
        show_albums(app, ids);
        app.act("library.selectAll", json!({}));
        app.act("dialog.export", json!({}));
        ui.close();
    }
    ui.separator();
    // into a folder that is none of them, nor inside one of them
    let cat = &app.session.catalog;
    let inside_chosen = |mut id: AlbumId| {
        for _ in 0..=cat.albums().count() {
            if chosen.contains(&id) {
                return true;
            }
            match cat.album(id).and_then(|a| a.parent) {
                Some(p) => id = p,
                None => break,
            }
        }
        false
    };
    // …nor one a smart album among them shows (it would include itself), nor where they all are
    let all_in = |folder: AlbumId| chosen.iter().all(|a| cat.album(*a).is_some_and(|a| a.parent == Some(folder)));
    let mut folders: Vec<(u64, String)> = cat
        .albums()
        .filter(|f| f.folder && !inside_chosen(f.id) && !all_in(f.id) && chosen.iter().all(|a| cat.album_move_would_loop(*a, f.id).is_none()))
        .map(|f| (f.id.0, f.name.clone()))
        .collect();
    folders.sort_by_key(|(_, name)| name.to_lowercase());
    let any_nested = chosen.iter().any(|a| cat.album(*a).is_some_and(|a| a.parent.is_some()));
    ui.menu_button(crate::i18n::tr_format!("Move {n} Albums to", n = n), |ui| {
        if item(ui, "albums:moveTop", crate::i18n::tr("Top Level").to_string(), any_nested).clicked() {
            app.act("album.move", json!({"ids": ids, "parent": null}));
            ui.close();
        }
        for (folder, name) in &folders {
            if ui.button(name).clicked() {
                app.act("album.move", json!({"ids": ids, "parent": folder}));
                ui.close();
            }
        }
    });
    album_clip_items(app, ui, ids);
    single_only(ui, "Rename…", "Choose one album to rename it");
    let delete = item(ui, "albums:delete", crate::i18n::tr_format!("Delete {n} Albums…", n = n), true)
        .on_hover_text(crate::i18n::tr("Photos stay in All Photos. Deleting a folder also deletes its albums. You can undo this."));
    if delete.clicked() {
        app.ui.dialog = Some(delete_albums_dialog(app, ids));
        ui.close();
    }
}

/// The confirmation for deleting several albums: what is named, and what goes with the folders.
pub(crate) fn delete_albums_dialog(app: &LightcraftApp, ids: &[u64]) -> Dialog {
    let cat = &app.session.catalog;
    let named: Vec<AlbumId> = ids.iter().map(|i| AlbumId(*i)).collect();
    let folders = named.iter().filter(|a| cat.album(**a).is_some_and(|a| a.folder)).count();
    let inside = cat
        .albums()
        .filter(|a| !named.contains(&a.id))
        .filter(|a| {
            // inside one of the folders named, at any depth (a ring, in a damaged library, ends the walk)
            let mut cur = a.parent;
            for _ in 0..=cat.albums().count() {
                match cur {
                    Some(p) if named.contains(&p) => return true,
                    Some(p) => cur = cat.album(p).and_then(|a| a.parent),
                    None => return false,
                }
            }
            false
        })
        .count();
    Dialog::Several(Several::DeleteAlbums { ids: ids.to_vec(), albums: named.len() - folders, folders, inside })
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
    take_keys(app, SideList::Folders, how, app.session.source_library_folders().len());
}

/// The folders a context menu opened on folder `path` acts on.
pub(crate) fn folder_targets(app: &LightcraftApp, path: &str) -> Vec<String> {
    targets(&app.session.source_library_folders(), &path.to_string(), &same_dir)
}

/// The context menu for several library folders chosen at once.
pub(crate) fn folders_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, paths: &[String]) {
    use lightcraft_catalog::ColorLabel;
    let n = paths.len();
    ui.menu_button(crate::i18n::tr("Color Label"), |ui| {
        for l in ColorLabel::ALL {
            let name = crate::i18n::color_label(&app.session.catalog, l);
            if ui.button(name).clicked() {
                app.act("folder.label", json!({"paths": paths, "label": format!("{l:?}").to_lowercase()}));
                ui.close();
            }
        }
        ui.separator();
        if ui.button(crate::i18n::tr("None")).clicked() {
            app.act("folder.label", json!({"paths": paths, "label": "none"}));
            ui.close();
        }
    });
    let sync = item(ui, "folders:synchronize", crate::i18n::tr_format!("Synchronize {n} Folders…", n = n), true).on_hover_text(crate::i18n::tr(
        "Find what changed in each of these folders on disk, then bring the library up to date with them one after another",
    ));
    if sync.clicked() {
        if let Err(e) = crate::sync::open_many(app, paths) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
    single_only(ui, "Rename Folder…", "Choose one folder to rename it");
    ui.separator();
    // a whole disk goes by itself, after its own question
    let disk = paths.iter().any(|p| lightcraft_catalog::folders::is_disk_root(p));
    let remove = item(ui, "folders:remove", crate::i18n::tr_format!("Remove {n} Folders from Library…", n = n), !disk)
        .on_hover_text(crate::i18n::tr("Moves the photos imported from these folders to Recently Deleted; no file is touched"))
        .on_disabled_hover_text(crate::i18n::tr("One of them is a whole disk: remove a disk by itself"));
    if remove.clicked() {
        app.ui.dialog = Some(remove_folders_dialog(app, paths));
        ui.close();
    }
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
    take_keys(app, SideList::Keywords, how, app.session.filter.keywords_wanted().len());
}

/// The keywords a context menu opened on the Keywords row `path` acts on.
pub(crate) fn keyword_targets(app: &LightcraftApp, path: &str) -> Vec<String> {
    targets(&app.session.filter.keywords_wanted(), &path.to_string(), &same_kw)
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

/// The context menu for several keywords chosen at once, in the left panel's Keywords or in the
/// Keyword List (`list`: its items that need one keyword are shown, greyed out).
pub(crate) fn keywords_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, keywords: &[String], list: bool) {
    let n = keywords.len();
    if list {
        single_only(ui, "Create Keyword Tag Inside…", "Choose one keyword to create a keyword inside it");
        single_only(ui, "Edit Keyword Tag…", "Choose one keyword to edit it");
        ui.separator();
    }
    let has_sel = app.session.active().is_some();
    if item(ui, "keywords:add", crate::i18n::tr_format!("Add {n} Keywords to Selected Photos", n = n), has_sel).clicked() {
        app.act("photo.setMeta", json!({"addKeywords": keywords}));
        ui.close();
    }
    if item(ui, "keywords:remove", crate::i18n::tr_format!("Remove {n} Keywords from Selected Photos", n = n), has_sel).clicked() {
        app.act("photo.setMeta", json!({"removeKeywords": keywords}));
        ui.close();
    }
    if list && item(ui, "keywords:show", crate::i18n::tr_format!("Show Photos with Any of {n} Keywords", n = n), true).clicked() {
        super::left::browse_all_photos(app, true);
        app.act("library.filter", json!({"keyword": null, "keywords": keywords}));
        ui.close();
    }
    ui.separator();
    keyword_clip_items(app, ui, keywords);
    if !list {
        single_only(ui, "Rename Keyword…", "Choose one keyword to rename it");
    }
    if item(ui, "keywords:merge", crate::i18n::tr_format!("Merge {n} Keywords into…", n = n), true).clicked() {
        app.ui.dialog = Some(Dialog::MergeKeywords { from: keywords.to_vec(), into: String::new() });
        ui.close();
    }
    if item(ui, "keywords:delete", crate::i18n::tr_format!("Delete {n} Keywords…", n = n), true).clicked() {
        app.ui.dialog = Some(delete_keywords_dialog(app, keywords));
        ui.close();
    }
}

/// The confirmation for deleting several keywords, saying how many photos have one of them.
pub(crate) fn delete_keywords_dialog(app: &LightcraftApp, keywords: &[String]) -> Dialog {
    let cat = &app.session.catalog;
    let paths: Vec<String> = keywords.iter().map(|k| cat.keyword_path(k).unwrap_or_else(|| k.clone())).collect();
    let has = |p: &lightcraft_catalog::Photo| p.meta.keywords.iter().any(|k| paths.iter().any(|g| lightcraft_catalog::keywords::is_under(k, g)));
    let count = cat.photos().filter(|p| p.in_library() && has(p)).count();
    Dialog::Several(Several::DeleteKeywords { keywords: paths, count })
}

// ---- The Keyword List (right panel)

/// The keywords picked in the Keyword List, in the order picked (the last is the one Edit takes).
pub(crate) fn list_picked(app: &LightcraftApp) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // (those that are gone, deleted or merged away, were let go of by `keys_frame`)
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
    let chosen = keys.len();
    list_pick(app, keys);
    take_keys(app, SideList::KeywordList, how, chosen);
}

/// The keywords a context menu opened on the Keyword List's row `path` acts on, or a drag that
/// picked that row up moves.
pub(crate) fn list_targets(app: &LightcraftApp, path: &str) -> Vec<String> {
    targets(&list_picked(app), &path.to_string(), &same_kw)
}

// ---- Cut, Copy and Paste

/// Cut and Copy for these albums, in an album's menu or the menu of several.
pub(crate) fn album_clip_items(app: &mut LightcraftApp, ui: &mut egui::Ui, ids: &[u64]) {
    for (id, label, cut) in [("clip:cut", "Cut", true), ("clip:copy", "Copy", false)] {
        if item(ui, id, crate::i18n::tr(label).to_string(), true).clicked() {
            let names: Vec<String> = ids.iter().filter_map(|i| app.session.catalog.album(AlbumId(*i))).map(|a| a.name.clone()).collect();
            // their names too, for wherever text is pasted
            ui.ctx().copy_text(names.join("\n"));
            app.ui.side.clip = Some(SideClip::Albums { ids: ids.to_vec(), cut });
            let said = if cut {
                "Cut: Paste is in a folder's menu, and in the Albums + menu for the top level"
            } else {
                "Copied: Paste is in a folder's menu, and in the Albums + menu for the top level"
            };
            app.toast(ui.ctx(), crate::i18n::tr(said).to_string());
            ui.close();
        }
    }
}

/// Paste for albums taken with Cut or Copy, into folder `parent` (`None`: the top level). Only
/// there while albums are held; cut albums move (one step), copied ones are duplicated.
pub(crate) fn album_paste_item(app: &mut LightcraftApp, ui: &mut egui::Ui, parent: Option<AlbumId>) {
    let Some(SideClip::Albums { ids, cut }) = app.ui.side.clip.clone() else { return };
    // what is still there
    let ids: Vec<u64> = ids.into_iter().filter(|i| app.session.catalog.album(AlbumId(*i)).is_some()).collect();
    let label = if parent.is_some() { "Paste Into This Folder" } else { "Paste at Top Level" };
    // cut albums can't move into one of themselves, or into a folder inside one
    let cat = &app.session.catalog;
    let inside_itself = cut
        && parent.is_some_and(|mut at| {
            for _ in 0..=cat.albums().count() {
                if ids.contains(&at.0) {
                    return true;
                }
                match cat.album(at).and_then(|a| a.parent) {
                    Some(p) => at = p,
                    None => break,
                }
            }
            false
        });
    let paste = item(ui, "clip:paste", crate::i18n::tr(label).to_string(), !ids.is_empty() && !inside_itself)
        .on_disabled_hover_text(crate::i18n::tr("A folder can't go inside itself"));
    if !paste.clicked() {
        return;
    }
    let to = parent.map(|p| p.0);
    if cut {
        if app.act("album.move", json!({"ids": ids, "parent": to})).is_some() {
            // moved: there is nothing left to paste
            app.ui.side.clip = None;
            app.ui.reveal_album = ids.last().copied();
        }
    } else if let Some(r) = app.act("album.duplicate", json!({"ids": ids, "parent": to})) {
        app.ui.reveal_album = r["ids"].as_array().and_then(|a| a.last()).and_then(serde_json::Value::as_u64);
    }
    ui.close();
}

/// Cut and Copy for these keywords, in a keyword's menu or the menu of several. Copy puts them on
/// the system clipboard as text (`a, b|c`), which a keyword box takes to give them to photos.
pub(crate) fn keyword_clip_items(app: &mut LightcraftApp, ui: &mut egui::Ui, keywords: &[String]) {
    if item(ui, "clip:cut", crate::i18n::tr("Cut").to_string(), true).clicked() {
        app.ui.side.clip = Some(SideClip::Keywords { keywords: keywords.to_vec() });
        app.toast(ui.ctx(), crate::i18n::tr("Cut: Paste is in the menu of the keyword they go inside").to_string());
        ui.close();
    }
    if item(ui, "clip:copy", crate::i18n::tr("Copy").to_string(), true).clicked() {
        ui.ctx().copy_text(keywords.join(", "));
        app.toast(ui.ctx(), crate::i18n::tr("Copied as text: paste it in a keyword box to give the keywords to photos").to_string());
        ui.close();
    }
}

/// Paste for keywords that were cut, inside keyword `parent`: they are nested there, as one
/// step. Only there while keywords are held, and not on one of them.
pub(crate) fn keyword_paste_item(app: &mut LightcraftApp, ui: &mut egui::Ui, parent: &str) {
    let Some(SideClip::Keywords { keywords }) = app.ui.side.clip.clone() else { return };
    let keywords: Vec<String> = keywords.into_iter().filter(|k| app.session.catalog.has_keyword(k)).collect();
    let inside_itself = keywords.iter().any(|k| lightcraft_catalog::keywords::is_under(parent, k));
    let paste = item(ui, "clip:paste", crate::i18n::tr("Paste Inside This Keyword").to_string(), !keywords.is_empty() && !inside_itself)
        .on_disabled_hover_text(crate::i18n::tr("A keyword can't go inside itself"));
    if !paste.clicked() {
        return;
    }
    let params = match keywords.as_slice() {
        [one] => json!({"keyword": one, "parent": parent}),
        many => json!({"keywords": many, "parent": parent}),
    };
    if app.act("keyword.move", params).is_some() {
        app.ui.side.clip = None;
        let into = app.session.catalog.keyword_path(parent).unwrap_or_else(|| parent.to_string());
        // the upper ones moved, and what is below them with them
        for k in keywords.iter().filter(|k| !keywords.iter().any(|o| !same_keyword(o, k) && lightcraft_catalog::keywords::is_under(k, o))) {
            let leaf = k.rsplit('|').next().unwrap_or(k);
            super::keyword_list::follow(app, k, &format!("{into}|{leaf}"));
        }
    }
    ui.close();
}

// ---- The confirmations

pub(crate) fn several_title(s: &Several) -> &'static str {
    match s {
        Several::DeleteAlbums { .. } => "Delete Albums",
        Several::DeleteKeywords { .. } => "Delete Keywords",
        Several::RemoveFolders { .. } => "Remove Folders from Library",
    }
}

pub(crate) fn several_button(s: &Several) -> &'static str {
    match s {
        Several::DeleteAlbums { .. } | Several::DeleteKeywords { .. } => "Delete",
        Several::RemoveFolders { .. } => "Remove",
    }
}

/// What the confirmation says: what goes, and what stays.
pub(crate) fn several_body(ui: &mut egui::Ui, s: &Several) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let dim = |ui: &mut egui::Ui, text: String| {
        ui.label(egui::RichText::new(text).color(t.text_dim));
    };
    match s {
        Several::DeleteAlbums { albums, folders, inside, .. } => {
            let n = albums + folders;
            ui.label(if *folders == 0 {
                crate::i18n::tr_format!("Delete {n} albums?", n = n)
            } else {
                crate::i18n::tr_format!("Delete {n} albums and folders of albums?", n = n)
            });
            if *inside > 0 {
                dim(ui, crate::i18n::tr_format!("{inside} more inside the folders go too.", inside = inside));
            }
            dim(ui, crate::i18n::tr("The photos stay in All Photos. Undo brings the albums back.").to_string());
        }
        Several::DeleteKeywords { keywords, count } => {
            ui.label(crate::i18n::tr_format!("Delete {n} keywords?", n = keywords.len()));
            names(ui, keywords.iter().map(|k| k.replace('|', " › ")).collect());
            dim(
                ui,
                if *count == 0 {
                    crate::i18n::tr("No photo has them; the keywords below them go too.").to_string()
                } else {
                    crate::i18n::tr_format!(
                        "Photos that have one of them: {count}. The keywords below them go too. Undo brings them back.",
                        count = count
                    )
                },
            );
        }
        Several::RemoveFolders { paths, count } => {
            ui.label(crate::i18n::tr_format!(
                "Remove {n} folders from the library? Photos imported from them: {count}.",
                n = paths.len(),
                count = count
            ));
            names(ui, paths.clone());
            dim(ui, crate::i18n::tr("That includes the photos in the folders inside them.").to_string());
            dim(ui, crate::i18n::tr("They move to Recently Deleted and can be restored; no file on disk is touched.").to_string());
        }
    }
}

/// The first few of the rows a confirmation is about, then how many more.
fn names(ui: &mut egui::Ui, all: Vec<String>) {
    const SHOWN: usize = 6;
    let t = crate::theme::Tokens::get(ui.ctx());
    for name in all.iter().take(SHOWN) {
        ui.label(egui::RichText::new(name).color(t.text_dim));
    }
    if all.len() > SHOWN {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("…and {n} more", n = all.len() - SHOWN)).color(t.text_dim));
    }
}

/// Do what the confirmation asked.
pub(crate) fn several_run(app: &mut LightcraftApp, s: &Several) -> Result<serde_json::Value, String> {
    match s {
        Several::DeleteAlbums { ids, .. } => app.run("album.delete", json!({"ids": ids})),
        Several::DeleteKeywords { keywords, .. } => {
            let r = app.run("keyword.delete", json!({"keywords": keywords}));
            if r.is_ok() {
                // none of them is picked in the Keyword List any more
                let gone = |k: &String| keywords.iter().any(|g| lightcraft_catalog::keywords::is_under(k, g));
                let left: Vec<String> = list_picked(app).into_iter().filter(|k| !gone(k)).collect();
                list_pick(app, left);
            }
            r
        }
        Several::RemoveFolders { paths, .. } => app.run("library.removeFolder", json!({"paths": paths})),
    }
}
