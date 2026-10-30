//! Several rows of a sidebar list chosen at once, driven through the control channel: Cmd/Ctrl+click
//! and Shift+click in the left panel's Albums, Folders, Keywords and By Date and in the right
//! panel's Keyword List, and what a context menu, a drag and the − button do with several rows.
//!
//! Scenarios:
//! - Given albums in the sidebar, when two are Cmd+clicked, then both are highlighted and the
//!   grid shows the photos of either; Cmd+click on one of them takes it away again.
//! - When one album is clicked and another Shift+clicked, then every album between them is
//!   chosen, but none inside a closed folder; a plain click chooses one alone again.
//! - Given several albums chosen, when the menu of one of them is opened, then it offers what can
//!   be done to all (Delete N Albums… asks first, and is one step to undo); the menu of an album
//!   that is not chosen is that album's own, and the chosen ones stay chosen.
//! - Given two albums chosen, when one is dragged onto a folder, then both move into it.
//! - Keywords and dates in the left panel: Cmd+click filters by any of those chosen; a plain
//!   click on the one keyword shown still clears it.
//! - Folders: Cmd+click shows the photos of both folders; the menu removes both after asking.
//! - The Keyword List: Cmd+click and Shift+click pick several; − and the menu delete them all
//!   after one question; the menu gives them all to the selected photos; dragging one of them
//!   nests them all.
//! - Given several rows just chosen with Cmd/Shift+click, when Delete is pressed, then the
//!   question about those rows comes up; after a click elsewhere, or with one row chosen the
//!   usual way, Delete is the photos' as it always was.
//! - Given an album copied from its menu, when Paste is chosen in a folder's menu, then a copy
//!   of it is inside the folder and the original stays; given albums cut, then Paste moves them
//!   (one step) and there is nothing left to paste.
//! - Given keywords cut in the Keyword List, when Paste is chosen in another keyword's menu, then
//!   they are nested inside it; Copy puts their names on the clipboard as text.

use std::time::Duration;

use lightcraft_catalog::{AlbumId, PhotoId};
use lightcraft_engine::LibrarySource;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::side_select::Several;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in the grid, the left panel open, in a window tall enough for every row.
fn app(right: &str) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1500.0, 3000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "leftPanel": true, "right": right}));
    h
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}
fn cmd_click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id, "cmd": true}));
}
fn shift_click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id, "shift": true}));
}
fn right_click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id, "button": "right"}));
}

fn center(h: &Headless, id: &str) -> egui::Pos2 {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("no {id} on screen"))
}

/// Drag the widget `from` and drop it on the widget `onto`.
fn drag(h: &mut Headless, from: &str, onto: &str) {
    let to = center(h, onto);
    ask(h, "ui.dragWidget", json!({"id": from, "toX": to.x, "toY": to.y, "steps": 12}));
}

/// An album called `name` holding `photos`.
fn album(h: &mut Headless, name: &str, parent: Option<u64>, photos: &[u64]) -> u64 {
    let id = run(h, "album.create", json!({"name": name, "parent": parent}))["id"].as_u64().unwrap();
    if !photos.is_empty() {
        run(h, "library.select", json!({"ids": photos}));
        run(h, "album.addPhotos", json!({"id": id, "ids": photos}));
    }
    id
}

fn row(id: u64) -> String {
    format!("source:album:{id}")
}

fn shown(h: &Headless) -> Vec<u64> {
    h.app.session.source_albums().iter().map(|a| a.0).collect()
}

fn visible(h: &mut Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

/// Four albums `Sel A`…`Sel D`, each with one photo of its own (and A and C sharing one).
fn four_albums(h: &mut Headless) -> ([u64; 4], Vec<u64>) {
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(5).map(|p| p.0).collect();
    let a = album(h, "Sel A", None, &[ids[0], ids[4]]);
    let b = album(h, "Sel B", None, &[ids[1]]);
    let c = album(h, "Sel C", None, &[ids[2], ids[4]]);
    let d = album(h, "Sel D", None, &[ids[3]]);
    ([a, b, c, d], ids)
}

#[test]
fn cmd_click_shows_several_albums_and_takes_one_away_again() {
    let mut h = app("info");
    let ([a, _, c, _], ids) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    assert_eq!(shown(&h), vec![a, c]);
    let mut want = vec![ids[0], ids[2], ids[4]];
    want.sort_unstable();
    assert_eq!(visible(&mut h), want, "the photos of either, each once");
    assert!(has(&h, &format!("highlight:album:{a}")) && has(&h, &format!("highlight:album:{c}")), "both rows are highlighted");
    assert_eq!(crate::i18n::source_title(&h.app.session), "2 albums", "the title says how many are shown");
    // Cmd+click on one of them takes it away…
    cmd_click(&mut h, &row(a));
    assert_eq!(shown(&h), vec![c]);
    assert!(!has(&h, &format!("highlight:album:{a}")));
    // …and on the last one too: back to every photo
    cmd_click(&mut h, &row(c));
    assert_eq!(h.app.session.source, LibrarySource::All);
}

#[test]
fn shift_click_takes_the_albums_between_and_a_plain_click_one_alone() {
    let mut h = app("info");
    let ([a, b, c, d], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    shift_click(&mut h, &row(c));
    assert_eq!(shown(&h), vec![a, b, c], "the row clicked is the last chosen");
    assert_eq!(h.app.session.source, LibrarySource::Album(AlbumId(c)));
    // a second Shift+click from the same row replaces the range
    shift_click(&mut h, &row(b));
    assert_eq!(shown(&h), vec![a, b]);
    // upwards, from another row
    click(&mut h, &row(d));
    shift_click(&mut h, &row(b));
    assert_eq!(shown(&h), vec![d, c, b]);
    // Cmd+Shift adds a range to what is chosen
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    ask(&mut h, "ui.clickWidget", json!({"id": row(d), "cmd": true, "shift": true}));
    assert_eq!(shown(&h), vec![a, c, d]);
    // a plain click on one of several chosen: that one alone
    click(&mut h, &row(c));
    assert_eq!(shown(&h), vec![c]);
}

#[test]
fn shift_click_does_not_take_albums_inside_a_closed_folder() {
    let mut h = app("info");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    // folders are listed first: Sel E (open, with Sel A), Sel Folder (closed, with Sel Inside), then Sel Z
    let outer = run(&mut h, "album.create", json!({"name": "Sel E", "folder": true}))["id"].as_u64().unwrap();
    let before = album(&mut h, "Sel A", Some(outer), &[ids[0]]);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    let inside = album(&mut h, "Sel Inside", Some(folder), &[ids[0]]);
    let after = album(&mut h, "Sel Z", None, &[ids[1]]);
    assert!(has(&h, &row(inside)), "a folder starts open");
    click(&mut h, &format!("albumToggle:{folder}"));
    assert!(!has(&h, &row(inside)), "closed");
    click(&mut h, &row(before));
    shift_click(&mut h, &row(after));
    let chosen = shown(&h);
    assert!(chosen.contains(&before) && chosen.contains(&after), "{chosen:?}");
    assert!(!chosen.contains(&inside), "a row that is not in sight is not taken: {chosen:?}");
}

#[test]
fn the_menu_of_several_albums_deletes_them_after_asking_as_one_step() {
    let mut h = app("info");
    let ([a, b, c, d], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    // the menu of an album that is not chosen is its own, and the chosen ones stay
    right_click(&mut h, &row(d));
    assert!(has(&h, &format!("albumDelete:{d}")), "one album's menu");
    assert!(!has(&h, "sideMenu:albums:delete"));
    assert_eq!(shown(&h), vec![a, c]);
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    // the menu of one of them: what can be done to both
    right_click(&mut h, &row(c));
    assert!(has(&h, "sideMenu:albums:delete") && has(&h, "sideMenu:albums:export"));
    assert!(!has(&h, &format!("albumDelete:{c}")), "not the single album's Delete");
    let undo = h.app.session.undo.len();
    click(&mut h, "sideMenu:albums:delete");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Several(Several::DeleteAlbums { ids: vec![a, c] })));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    let exists = |h: &Headless, id: u64| h.app.session.catalog.album(AlbumId(id)).is_some();
    assert!(exists(&h, a) && exists(&h, c), "Cancel keeps them");
    right_click(&mut h, &row(c));
    click(&mut h, "sideMenu:albums:delete");
    click(&mut h, "button:dialogOk");
    assert!(!exists(&h, a) && !exists(&h, c) && exists(&h, b) && exists(&h, d));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert_eq!(h.app.session.source, LibrarySource::All, "nothing of what was shown is left");
    run(&mut h, "edit.undo", json!({}));
    assert!(exists(&h, a) && exists(&h, c));
}

#[test]
fn dragging_one_of_several_albums_onto_a_folder_moves_them_all() {
    let mut h = app("info");
    let ([a, b, c, _], _) = four_albums(&mut h);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    let parent = |h: &Headless, id: u64| h.app.session.catalog.album(AlbumId(id)).unwrap().parent.map(|p| p.0);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    let undo = h.app.session.undo.len();
    drag(&mut h, &row(c), &format!("source:folder:{folder}"));
    assert_eq!((parent(&h, a), parent(&h, c), parent(&h, b)), (Some(folder), Some(folder), None));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert_eq!(shown(&h), vec![a, c], "still the ones shown");
    // an album that is not one of them goes alone
    drag(&mut h, &row(b), &format!("source:folder:{folder}"));
    assert_eq!(parent(&h, b), Some(folder));
    run(&mut h, "edit.undo", json!({}));
    run(&mut h, "edit.undo", json!({}));
    assert_eq!((parent(&h, a), parent(&h, c)), (None, None));
}

/// The group goes wherever one of its albums still has to move: the dragged row may already be there.
#[test]
fn a_group_can_be_dropped_where_only_the_dragged_album_already_is() {
    let mut h = app("info");
    let ([a, _, c, _], _) = four_albums(&mut h);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    run(&mut h, "album.move", json!({"id": a, "parent": folder}));
    let parent = |h: &Headless, id: u64| h.app.session.catalog.album(AlbumId(id)).unwrap().parent.map(|p| p.0);
    h.app.ui.reveal_album = Some(a);
    run(&mut h, "library.source", json!({"kind": "album", "ids": [a, c]}));
    h.step();
    h.step();
    let undo = h.app.session.undo.len();
    // dragged: the one already at the top level; the other one is in the folder
    drag(&mut h, &row(c), "sidebarSection:albums");
    assert_eq!((parent(&h, a), parent(&h, c)), (None, None), "both at the top level");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    run(&mut h, "edit.undo", json!({}));
    h.app.ui.reveal_album = Some(a);
    h.step();
    h.step();
    // dragged: the one already in the folder; the other one is at the top level
    drag(&mut h, &row(a), &format!("source:folder:{folder}"));
    assert_eq!((parent(&h, a), parent(&h, c)), (Some(folder), Some(folder)), "both in the folder");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
}

/// A drop beside a row that would put the dragged album where it already is still moves the albums
/// chosen with it that are elsewhere.
#[test]
fn a_group_dropped_beside_the_row_above_its_dragged_album_still_moves_the_others() {
    let mut h = app("info");
    let ([a, b, c, _], _) = four_albums(&mut h);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    run(&mut h, "album.move", json!({"id": a, "parent": folder}));
    let parent = |h: &Headless, id: u64| h.app.session.catalog.album(AlbumId(id)).unwrap().parent.map(|p| p.0);
    h.app.ui.reveal_album = Some(a);
    run(&mut h, "library.source", json!({"kind": "album", "ids": [a, c]}));
    h.step();
    h.step();
    let undo = h.app.session.undo.len();
    // (C goes below B, where it already is: for C alone that is nothing)
    drag(&mut h, &row(c), &row(b));
    assert_eq!((parent(&h, a), parent(&h, c)), (None, None), "A comes out of its folder to the top level");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
}

/// A drop beside a row places the dragged album there, and brings the others chosen with it along, as one step.
#[test]
fn a_group_dropped_beside_a_row_places_the_dragged_album_there() {
    let mut h = app("info");
    let ([a, b, c, d], _) = four_albums(&mut h);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    run(&mut h, "album.move", json!({"id": a, "parent": folder}));
    let mine = [a, b, c, d, folder];
    let top =
        |h: &Headless| -> Vec<u64> { h.app.session.catalog.album_children(None).iter().map(|x| x.id.0).filter(|id| mine.contains(id)).collect() };
    h.app.ui.reveal_album = Some(a);
    run(&mut h, "library.source", json!({"kind": "album", "ids": [a, c]}));
    h.step();
    h.step();
    assert_eq!(top(&h), vec![folder, b, c, d]);
    let undo = h.app.session.undo.len();
    // C is dropped below D (the bottom half of a row): after it, where listing by name would not have put it
    drag(&mut h, &row(c), &row(d));
    let after = top(&h);
    let at = |id: u64| after.iter().position(|x| *x == id);
    assert!(at(c) > at(d) && at(d).is_some(), "C was placed after D: {after:?}");
    assert!(after.contains(&a), "A came out of its folder: {after:?}");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(top(&h), vec![folder, b, c, d], "one undo puts both back");
    assert_eq!(h.app.session.catalog.album(AlbumId(a)).unwrap().parent, Some(AlbumId(folder)));
}

#[test]
fn cmd_click_filters_by_any_of_several_keywords() {
    let mut h = app("info");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "addKeywords": ["Sel One"]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "addKeywords": ["Sel Two"]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[2]], "addKeywords": ["Sel Three"]}));
    click(&mut h, "source:keyword:Sel One");
    assert_eq!(h.app.session.filter.keyword.as_deref(), Some("Sel One"), "a plain click is what it was");
    cmd_click(&mut h, "source:keyword:Sel Two");
    assert_eq!(h.app.session.filter.keywords, vec!["Sel One".to_string(), "Sel Two".to_string()]);
    assert_eq!(h.app.session.filter.keyword, None);
    assert_eq!(visible(&mut h), {
        let mut v = vec![ids[0], ids[1]];
        v.sort_unstable();
        v
    });
    assert!(has(&h, "highlight:keyword:Sel One") && has(&h, "highlight:keyword:Sel Two"));
    // the menu of one of them offers what can be done to both
    right_click(&mut h, "source:keyword:Sel Two");
    assert!(has(&h, "sideMenu:keywords:delete") && has(&h, "sideMenu:keywords:merge"));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    // a plain click: that keyword alone; again: every photo
    click(&mut h, "source:keyword:Sel Two");
    assert_eq!((h.app.session.filter.keyword.as_deref(), h.app.session.filter.keywords.len()), (Some("Sel Two"), 0));
    click(&mut h, "source:keyword:Sel Two");
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default());
}

#[test]
fn cmd_click_filters_by_any_of_several_dates() {
    let mut h = app("info");
    // the months of the demo library's first year
    let year: String = h.app.widgets.iter().find_map(|(w, _)| w.strip_prefix("source:date:").map(str::to_string)).expect("a year");
    click(&mut h, &format!("dateToggle:{year}"));
    let years: Vec<String> =
        h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("source:date:").filter(|k| k.len() == 7).map(str::to_string)).collect();
    assert!(years.len() >= 2, "the demo library spans months: {years:?}");
    let all = h.app.session.visible().len();
    click(&mut h, &format!("source:date:{}", years[0]));
    let first = h.app.session.visible().len();
    cmd_click(&mut h, &format!("source:date:{}", years[1]));
    assert_eq!(h.app.session.filter.dates, vec![years[0].clone(), years[1].clone()]);
    let both = h.app.session.visible().len();
    assert!(both > first && both <= all, "{first} then {both} of {all}");
    cmd_click(&mut h, &format!("source:date:{}", years[0]));
    assert_eq!((h.app.session.filter.date.as_deref(), h.app.session.filter.dates.len()), (Some(years[1].as_str()), 0));
}

/// A library of file-backed photos that exist only in the catalog.
fn folders_app(paths: &[&str]) -> Headless {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new();
    for path in paths {
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: (*path).into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1500.0, 2000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "leftPanel": true}));
    h
}

#[test]
fn cmd_click_shows_several_folders_and_the_menu_removes_them_after_asking() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg", "/pics/work/d.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "source:libfolder:/pics/trip");
    cmd_click(&mut h, "source:libfolder:/pics/work");
    assert_eq!(h.app.session.source_library_folders(), vec!["/pics/trip".to_string(), "/pics/work".to_string()]);
    assert_eq!(h.app.session.visible().len(), 3);
    assert_eq!(crate::i18n::source_title(&h.app.session), "2 folders", "the title says how many are shown");
    assert!(has(&h, "highlight:libfolder:/pics/trip") && has(&h, "highlight:libfolder:/pics/work"));
    right_click(&mut h, "source:libfolder:/pics/work");
    assert!(has(&h, "sideMenu:folders:remove"));
    click(&mut h, "sideMenu:folders:remove");
    assert_eq!(
        h.app.ui.dialog,
        Some(Dialog::Several(Several::RemoveFolders { paths: vec!["/pics/trip".to_string(), "/pics/work".to_string()], count: 3 }))
    );
    let undo = h.app.session.undo.len();
    click(&mut h, "button:dialogOk");
    assert_eq!(h.app.session.undo.len(), undo + 1);
    assert_eq!(h.app.session.source, LibrarySource::All);
    assert_eq!(h.app.session.visible().len(), 1, "the photo of the folder that stayed");
    // Shift+click takes the folders between, as drawn
    run(&mut h, "edit.undo", json!({}));
    let rows: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("source:libfolder:/pics/").map(str::to_string)).collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    click(&mut h, &format!("source:libfolder:/pics/{}", rows[0]));
    shift_click(&mut h, &format!("source:libfolder:/pics/{}", rows[2]));
    assert_eq!(h.app.session.source_library_folders().len(), 3);
}

/// The Keyword List with three keywords of its own on the first photos, at the top level:
/// `Sel K1`, `Sel K2`, `Sel K3`, and an empty `Sel Parent`.
fn keyword_list() -> (Headless, Vec<u64>) {
    let mut h = app("keywords");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "addKeywords": ["Sel K1", "Sel K2"]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "addKeywords": ["Sel K2", "Sel K3"]}));
    run(&mut h, "keyword.create", json!({"name": "Sel Parent"}));
    // only these in the list: every row is on screen
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordFilter"}));
    ask(&mut h, "ui.text", json!({"text": "Sel "}));
    assert!(has(&h, "keywordRow:Sel K1") && has(&h, "keywordRow:Sel Parent"));
    (h, ids)
}

fn picked(h: &Headless) -> Vec<String> {
    crate::panels::side_select::list_picked(&h.app)
}

fn keywords_of(h: &Headless, id: u64) -> Vec<String> {
    let mut k: Vec<String> = h.app.session.catalog.photo(PhotoId(id)).unwrap().meta.keywords.iter().filter(|k| k.contains("Sel")).cloned().collect();
    k.sort();
    k
}

#[test]
fn the_keyword_list_picks_several_with_cmd_and_shift() {
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    assert_eq!(picked(&h), vec!["Sel K1"]);
    cmd_click(&mut h, "keywordRow:Sel K3");
    assert_eq!(picked(&h), vec!["Sel K1", "Sel K3"]);
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Sel K3"), "the last picked is the one Edit takes");
    assert!(has(&h, "keywordPicked:Sel K1") && has(&h, "keywordPicked:Sel K3") && !has(&h, "keywordPicked:Sel K2"));
    cmd_click(&mut h, "keywordRow:Sel K1");
    assert_eq!(picked(&h), vec!["Sel K3"]);
    click(&mut h, "keywordRow:Sel K1");
    shift_click(&mut h, "keywordRow:Sel K3");
    assert_eq!(picked(&h), vec!["Sel K1", "Sel K2", "Sel K3"]);
    // picking never changes what the grid shows
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default());
}

#[test]
fn the_keyword_list_deletes_several_after_one_question() {
    let (mut h, ids) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K2");
    let undo = h.app.session.undo.len();
    click(&mut h, "keywordList:delete");
    assert_eq!(
        h.app.ui.dialog,
        Some(Dialog::Several(Several::DeleteKeywords { keywords: vec!["Sel K1".to_string(), "Sel K2".to_string()], count: 2 }))
    );
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(h.app.session.catalog.has_keyword("Sel K1"), "Cancel kept them");
    // from the menu of one of them, the same question
    right_click(&mut h, "keywordRow:Sel K2");
    assert!(has(&h, "sideMenu:keywords:delete") && !has(&h, "keywordMenu:delete"), "the menu for several");
    click(&mut h, "sideMenu:keywords:delete");
    click(&mut h, "button:dialogOk");
    assert!(!h.app.session.catalog.has_keyword("Sel K1") && !h.app.session.catalog.has_keyword("Sel K2"));
    assert!(h.app.session.catalog.has_keyword("Sel K3"));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert!(picked(&h).is_empty(), "nothing that is gone stays picked");
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(keywords_of(&h, ids[0]), vec!["Sel K1", "Sel K2"]);
}

#[test]
fn the_menu_of_several_keywords_gives_them_all_to_the_selected_photos() {
    let (mut h, ids) = keyword_list();
    run(&mut h, "library.select", json!({"ids": [ids[2]]}));
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    // the menu of a keyword that is not picked is its own
    right_click(&mut h, "keywordRow:Sel K2");
    assert!(has(&h, "keywordMenu:delete") && !has(&h, "sideMenu:keywords:add"));
    assert_eq!(picked(&h), vec!["Sel K1", "Sel K3"], "the picked ones stay picked");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    let undo = h.app.session.undo.len();
    right_click(&mut h, "keywordRow:Sel K3");
    click(&mut h, "sideMenu:keywords:add");
    assert_eq!(keywords_of(&h, ids[2]), vec!["Sel K1", "Sel K3"]);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    // …and shows the photos with any of them
    right_click(&mut h, "keywordRow:Sel K3");
    click(&mut h, "sideMenu:keywords:show");
    assert_eq!(h.app.session.filter.keywords, vec!["Sel K1".to_string(), "Sel K3".to_string()]);
}

#[test]
fn dragging_one_of_several_keywords_nests_them_all() {
    let (mut h, ids) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    let undo = h.app.session.undo.len();
    drag(&mut h, "keywordRow:Sel K3", "keywordRow:Sel Parent");
    assert_eq!(keywords_of(&h, ids[0]), vec!["Sel K2", "Sel Parent|Sel K1"]);
    assert_eq!(keywords_of(&h, ids[1]), vec!["Sel K2", "Sel Parent|Sel K3"]);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert_eq!(picked(&h), vec!["Sel Parent|Sel K1", "Sel Parent|Sel K3"], "still picked, where they are now");
    assert!(h.app.ui.dragging_keyword.is_none(), "the drag ended");
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(keywords_of(&h, ids[0]), vec!["Sel K1", "Sel K2"]);
}

fn several_dialog(h: &Headless) -> bool {
    matches!(h.app.ui.dialog, Some(Dialog::Several(_)))
}

#[test]
fn delete_asks_about_several_rows_just_chosen_and_is_the_photos_otherwise() {
    let mut h = app("info");
    let ([a, _, c, _], _) = four_albums(&mut h);
    // one album chosen the usual way: Delete is not about albums
    click(&mut h, &row(a));
    assert!(h.app.ui.side.keys.is_none());
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert!(!several_dialog(&h), "{:?}", h.app.ui.dialog);
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // several chosen with Cmd+click: Delete asks about them
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    assert!(has(&h, &format!("highlight:album:{a}")));
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Several(Several::DeleteAlbums { ids: vec![a, c] })));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(h.app.session.catalog.album(AlbumId(a)).is_some(), "asking deleted nothing");
    // forward Delete too
    ask(&mut h, "ui.key", json!({"key": "Delete"}));
    assert!(several_dialog(&h));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // a click anywhere else gives the key back to the photos, with both albums still shown
    let cell =
        h.app.widgets.iter().find(|(w, _)| w.starts_with("cell:") || w.starts_with("thumb:")).map(|(w, _)| w.clone()).expect("a photo in the grid");
    click(&mut h, &cell);
    assert_eq!(shown(&h), vec![a, c]);
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert!(!several_dialog(&h), "{:?}", h.app.ui.dialog);
}

#[test]
fn delete_asks_about_several_keywords_picked_in_the_keyword_list() {
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    shift_click(&mut h, "keywordRow:Sel K3");
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert_eq!(
        h.app.ui.dialog,
        Some(Dialog::Several(Several::DeleteKeywords { keywords: vec!["Sel K1".to_string(), "Sel K2".to_string(), "Sel K3".to_string()], count: 2 }))
    );
    click(&mut h, "button:dialogOk");
    assert!(!h.app.session.catalog.has_keyword("Sel K2"));
    // nothing is chosen there any more: Delete is the photos' again
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert!(!several_dialog(&h), "{:?}", h.app.ui.dialog);
}

#[test]
fn copy_and_paste_duplicates_an_album_and_cut_and_paste_moves_several() {
    use crate::panels::side_select::SideClip;
    let mut h = app("info");
    let ([a, b, c, _], ids) = four_albums(&mut h);
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    let folder_row = format!("source:folder:{folder}");
    let inside = |h: &Headless| -> Vec<String> {
        let mut n: Vec<String> = h.app.session.catalog.albums().filter(|x| x.parent == Some(AlbumId(folder))).map(|x| x.name.clone()).collect();
        n.sort();
        n
    };
    // nothing held: a folder's menu has no Paste
    right_click(&mut h, &folder_row);
    assert!(has(&h, "sideMenu:clip:copy") && !has(&h, "sideMenu:clip:paste"));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    // Copy one album, Paste into the folder: a copy, and the original stays
    right_click(&mut h, &row(b));
    click(&mut h, "sideMenu:clip:copy");
    assert_eq!(h.app.ui.side.clip, Some(SideClip::Albums { ids: vec![b], cut: false }));
    assert_eq!(h.app.copied.as_deref(), Some("Sel B"), "its name is on the clipboard as text");
    right_click(&mut h, &folder_row);
    click(&mut h, "sideMenu:clip:paste");
    assert_eq!(inside(&h), vec!["Sel B"]);
    assert_eq!(h.app.session.catalog.album(AlbumId(b)).unwrap().parent, None, "the original stays");
    let copy = h.app.session.catalog.albums().find(|x| x.parent == Some(AlbumId(folder))).unwrap();
    assert_eq!(copy.photos.iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[1]], "with its photos");
    assert!(h.app.ui.side.clip.is_some(), "what was copied can be pasted again");
    // Cut two albums chosen together, Paste: both move, as one step, and nothing is held any more
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    right_click(&mut h, &row(c));
    click(&mut h, "sideMenu:clip:cut");
    assert_eq!(h.app.ui.side.clip, Some(SideClip::Albums { ids: vec![a, c], cut: true }));
    let undo = h.app.session.undo.len();
    right_click(&mut h, &folder_row);
    click(&mut h, "sideMenu:clip:paste");
    assert_eq!(inside(&h), vec!["Sel A", "Sel B", "Sel C"]);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert_eq!(h.app.ui.side.clip, None);
    right_click(&mut h, &folder_row);
    assert!(!has(&h, "sideMenu:clip:paste"));
}

#[test]
fn cut_and_paste_nests_keywords_and_copy_puts_them_on_the_clipboard() {
    use crate::panels::side_select::SideClip;
    let (mut h, ids) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    right_click(&mut h, "keywordRow:Sel K3");
    click(&mut h, "sideMenu:clip:copy");
    assert_eq!(h.app.copied.as_deref(), Some("Sel K1, Sel K3"));
    assert_eq!(h.app.ui.side.clip, None, "copied keywords are text, nothing is held to paste here");
    right_click(&mut h, "keywordRow:Sel K3");
    click(&mut h, "sideMenu:clip:cut");
    assert_eq!(h.app.ui.side.clip, Some(SideClip::Keywords { keywords: vec!["Sel K1".to_string(), "Sel K3".to_string()] }));
    let undo = h.app.session.undo.len();
    right_click(&mut h, "keywordRow:Sel Parent");
    click(&mut h, "sideMenu:clip:paste");
    assert_eq!(keywords_of(&h, ids[0]), vec!["Sel K2", "Sel Parent|Sel K1"]);
    assert_eq!(keywords_of(&h, ids[1]), vec!["Sel K2", "Sel Parent|Sel K3"]);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
    assert_eq!(h.app.ui.side.clip, None);
    assert_eq!(picked(&h), vec!["Sel Parent|Sel K1", "Sel Parent|Sel K3"], "still picked, where they are now");
}

#[test]
fn copying_keywords_after_a_cut_leaves_nothing_to_paste() {
    use crate::panels::side_select::SideClip;
    let (mut h, _ids) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    right_click(&mut h, "keywordRow:Sel K1");
    click(&mut h, "sideMenu:clip:cut");
    assert_eq!(h.app.ui.side.clip, Some(SideClip::Keywords { keywords: vec!["Sel K1".to_string()] }));
    // the last thing taken is what Paste offers: a Copy is text, so the earlier Cut is dropped
    right_click(&mut h, "keywordRow:Sel K3");
    click(&mut h, "sideMenu:clip:copy");
    assert_eq!(h.app.copied.as_deref(), Some("Sel K3"));
    assert_eq!(h.app.ui.side.clip, None);
    right_click(&mut h, "keywordRow:Sel Parent");
    assert!(!has(&h, "sideMenu:clip:paste"));
}

// ---- What two independent reviews found

/// A range starts from a row that is chosen: not from an album clicked before All Photos was shown.
#[test]
fn shift_click_does_not_start_from_an_album_that_is_no_longer_shown() {
    let mut h = app("info");
    let ([a, _, c, d], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    click(&mut h, "source:all");
    shift_click(&mut h, &row(d));
    assert_eq!(shown(&h), vec![d], "nothing was chosen: the row alone");
    // chosen by a command, not a click: the range starts from it
    run(&mut h, "library.source", json!({"kind": "album", "id": c}));
    shift_click(&mut h, &row(a));
    assert_eq!(shown(&h).first(), Some(&c));
    assert_eq!(shown(&h).last(), Some(&a));
    assert_eq!(shown(&h).len(), 3);
}

/// Showing one keyword from the Keyword List puts the ones chosen in the sidebar aside: the grid
/// is that keyword's, not those and this one.
#[test]
fn showing_a_keyword_from_the_keyword_list_replaces_the_keywords_chosen_in_the_sidebar() {
    let (mut h, _) = keyword_list();
    click(&mut h, "source:keyword:Sel K1");
    cmd_click(&mut h, "source:keyword:Sel K2");
    assert_eq!(h.app.session.filter.keywords_wanted(), vec!["Sel K1".to_string(), "Sel K2".to_string()]);
    click(&mut h, "keywordShow:Sel K3");
    assert_eq!(h.app.session.filter.keywords_wanted(), vec!["Sel K3".to_string()]);
}

/// The Delete key goes back to the photos when anything else changes what is shown or selected.
#[test]
fn delete_is_the_photos_again_after_a_change_made_without_the_pointer() {
    let mut h = app("info");
    let ([a, _, c, _], ids) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    assert!(h.app.ui.side.keys.is_some());
    // a key, a menu or an agent selects photos
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    ask(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert!(!several_dialog(&h), "{:?}", h.app.ui.dialog);
    assert!(h.app.ui.side.keys.is_none());
}

/// A confirmation for several rows that is refused says why.
#[test]
fn a_refused_question_about_several_albums_says_why() {
    let mut h = app("info");
    let ([a, _, c, _], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    right_click(&mut h, &row(c));
    click(&mut h, "sideMenu:albums:delete");
    // one of them goes while the question is up
    h.app.session.execute("album.delete", &json!({"id": c})).unwrap();
    click(&mut h, "button:dialogOk");
    assert!(h.app.session.catalog.album(AlbumId(a)).is_some(), "refused as a whole");
    let said = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.contains(&c.to_string()), "the toast names the album that is gone: {said:?}");
    // only the reason, not the command it came from, and for as long as any refusal stays
    assert!(!said.contains("album.delete"), "{said:?}");
    let (left, now) = (h.app.ui.toast.as_ref().map_or(0.0, |t| t.1), h.time());
    assert!(left - now > 5.0, "a refusal stays {} s", left - now);
}

/// The control channel's confirm of a refused question about several rows answers with the reason.
#[test]
fn a_refused_confirm_of_several_albums_answers_with_the_reason() {
    let mut h = app("info");
    let ([a, _, c, _], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    right_click(&mut h, &row(c));
    click(&mut h, "sideMenu:albums:delete");
    h.app.session.execute("album.delete", &json!({"id": c})).unwrap();
    let r = h.request("ui.dialog.confirm", json!({}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], false, "{r}");
    let said = r["error"].as_str().unwrap_or_default();
    assert!(said.contains(&c.to_string()), "the error names the album that is gone: {said:?}");
    assert!(h.app.session.catalog.album(AlbumId(a)).is_some(), "refused as a whole");
}

/// A keyword that is gone is not picked any more: the row left alone has its own menu.
#[test]
fn a_keyword_that_is_gone_is_not_picked_any_more() {
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    run(&mut h, "keyword.delete", json!({"keyword": "Sel K3"}));
    assert_eq!(picked(&h), vec!["Sel K1"]);
    right_click(&mut h, "keywordRow:Sel K1");
    assert!(has(&h, "keywordMenu:delete") && !has(&h, "sideMenu:keywords:delete"), "one keyword's menu");
    // one that was picked earlier is let go of too
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    run(&mut h, "keyword.delete", json!({"keyword": "Sel K1"}));
    assert_eq!(picked(&h), vec!["Sel K3"]);
    // and a keyword deleted while it was the only one picked does not come back with the next click
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    run(&mut h, "keyword.delete", json!({"keyword": "Sel K1"}));
    cmd_click(&mut h, "keywordRow:Sel K2");
    assert_eq!(picked(&h), vec!["Sel K2"]);
}

/// With several albums shown there is no telling which one Delete means: it is the photos' Delete
/// (which asks), and Photo ▸ Remove from Album is not on offer; no photo leaves the album chosen last.
#[test]
fn delete_with_several_albums_shown_does_not_take_a_photo_out_of_the_one_chosen_last() {
    let mut h = app("info");
    let ([a, _, c, _], ids) = four_albums(&mut h);
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    assert!(!crate::menubar::in_normal_album(&h.app), "Remove from Album is not on offer for several albums");
    // a photo that is in A and in C (the one chosen last): choosing it is a change, so the key is the photos' again
    run(&mut h, "library.select", json!({"ids": [ids[4]]}));
    ask(&mut h, "ui.key", json!({"key": "Delete"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1 }), "the photos' Delete asks");
    assert_eq!(h.app.session.catalog.album_count(AlbumId(a)), 2);
    assert_eq!(h.app.session.catalog.album_count(AlbumId(c)), 2);
}

/// Cut albums are not pasted into one of themselves.
#[test]
fn cut_albums_are_not_pasted_into_themselves() {
    let mut h = app("info");
    let folder = run(&mut h, "album.create", json!({"name": "Sel Folder", "folder": true}))["id"].as_u64().unwrap();
    let inner = run(&mut h, "album.create", json!({"name": "Sel Inner", "folder": true, "parent": folder}))["id"].as_u64().unwrap();
    right_click(&mut h, &format!("source:folder:{folder}"));
    click(&mut h, "sideMenu:clip:cut");
    let undo = h.app.session.undo.len();
    for target in [folder, inner] {
        right_click(&mut h, &format!("source:folder:{target}"));
        assert!(has(&h, "sideMenu:clip:paste"));
        // (a refusal of the engine would toast its reason: the item must not run the command at all)
        h.app.ui.toast = None;
        click(&mut h, "sideMenu:clip:paste");
        assert!(h.app.ui.toast.is_none(), "greyed out: no command ran to be refused: {:?}", h.app.ui.toast);
        assert_eq!(h.app.session.undo.len(), undo, "greyed out: nothing happens");
        assert!(h.app.ui.side.clip.is_some());
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
    }
}

/// Another library's albums are not what was cut in this one.
#[test]
fn what_was_cut_is_forgotten_when_another_library_opens() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let mut h = app("keywords");
    let ([a, _, c, _], _) = four_albums(&mut h);
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    right_click(&mut h, &row(c));
    click(&mut h, "sideMenu:clip:cut");
    assert!(h.app.ui.side.clip.is_some());
    let dir = Scratch(std::env::temp_dir().join(format!("lc-multi-select-switch-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    h.app.session.open_library(&dir.0, false).unwrap();
    h.settle(SETTLE);
    assert!(h.app.ui.side.clip.is_none() && h.app.ui.side.keys.is_none());
}

/// A keyword picked in the Keyword List is not picked in another library that has the same keyword.
#[test]
fn a_keyword_picked_in_the_keyword_list_is_forgotten_when_another_library_opens() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let mut h = app("keywords");
    let here = Scratch(std::env::temp_dir().join(format!("lc-multi-select-keyword-a-{}", std::process::id())));
    let there = Scratch(std::env::temp_dir().join(format!("lc-multi-select-keyword-b-{}", std::process::id())));
    for dir in [&here, &there] {
        let _ = std::fs::remove_dir_all(&dir.0);
        h.app.session.open_library(&dir.0, false).unwrap();
        h.settle(SETTLE);
        run(&mut h, "keyword.create", json!({"name": "Sel Same"}));
    }
    // Given a keyword picked in the library that is open
    h.app.ui.keyword_list_selected = Some("Sel Same".into());
    // When the other library, which has a keyword by that name, opens
    h.app.session.open_library(&here.0, false).unwrap();
    h.settle(SETTLE);
    // Then its row is not picked
    assert_eq!(h.app.ui.keyword_list_selected, None);
}

/// "Show in Library" after a Lightroom import shows every photo: keywords chosen in the
/// sidebar with Cmd+click are not left in the filter (dates go the same way: both lists are omitted from the default filter's JSON).
#[test]
fn show_in_library_clears_keywords_chosen_with_cmd_click() {
    let mut h = app("info");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "addKeywords": ["Sel One"]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "addKeywords": ["Sel Two"]}));
    click(&mut h, "source:keyword:Sel One");
    cmd_click(&mut h, "source:keyword:Sel Two");
    assert_eq!(h.app.session.filter.keywords.len(), 2);
    h.app.ui.dialog = Some(Dialog::LightroomResult { summary: "done".into(), library: None, details: vec![], first: Some(ids[0]) });
    h.settle(SETTLE);
    click(&mut h, "button:dialogOk");
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default());
    assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(ids[0]));
}

/// "Show in Library" shows every photo: albums and folders put in the filter by an agent (the
/// sidebars never leave them there) are not kept either. Both lists are left out of the default
/// filter's JSON when empty, so the reset patch has to name them.
#[test]
fn show_in_library_clears_albums_and_folders_in_the_filter() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg"]);
    let a = album(&mut h, "Sel A", None, &[]);
    let b = album(&mut h, "Sel B", None, &[]);
    run(&mut h, "library.filter", json!({"albums": [a, b], "libraryFolders": ["/pics/trip", "/pics/home"]}));
    assert!(!h.app.session.filter.albums.is_empty() && !h.app.session.filter.library_folders.is_empty());
    let first = h.app.session.catalog.photos().next().unwrap().id.0;
    h.app.ui.dialog = Some(Dialog::LightroomResult { summary: "done".into(), library: None, details: vec![], first: Some(first) });
    h.settle(SETTLE);
    click(&mut h, "button:dialogOk");
    assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default());
}

/// The smart-album editor counts the photos of every folder the album was made from, not only the
/// first.
#[test]
fn the_smart_album_editor_counts_photos_of_every_folder_the_album_was_made_from() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/home/b.jpg", "/pics/work/c.jpg"]);
    let id =
        run(&mut h, "album.createSmart", json!({"name": "Two folders", "rules": {"libraryFolder": "/pics/trip", "libraryFolders": ["/pics/home"]}}))
            ["id"]
            .as_u64()
            .unwrap();
    h.app.ui.dialog = Some(Dialog::SmartRules { id: Some(id), name: "Two folders".into(), rules: Default::default(), parent: None });
    h.settle(SETTLE);
    let text = h.painted_text().join(" ");
    assert!(text.contains("2 photos match"), "{text}");
}
