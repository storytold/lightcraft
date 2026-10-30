//! Several rows of a sidebar list chosen at once, driven through the control channel: Cmd/Ctrl+click
//! and Shift+click in the left panel's Albums, Folders, Keywords and By Date and in the right
//! panel's Keyword List.
//!
//! Scenarios:
//! - Given albums in the sidebar, when two are Cmd+clicked, then both are highlighted and the
//!   grid shows the photos of either; Cmd+click on one of them takes it away again.
//! - When one album is clicked and another Shift+clicked, then every album between them is
//!   chosen, but none inside a closed folder; a plain click chooses one alone again.
//! - Keywords and dates in the left panel: Cmd+click filters by any of those chosen; a plain
//!   click on the one keyword shown still clears it.
//! - Folders: Cmd+click shows the photos of both folders; Shift+click takes those between.
//! - The Keyword List: Cmd+click and Shift+click pick several, and picking never changes the grid.

use std::time::Duration;

use lightcraft_catalog::AlbumId;
use lightcraft_engine::LibrarySource;
use serde_json::{Value, json};

use crate::headless::Headless;
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
fn cmd_click_shows_several_folders_and_shift_click_takes_those_between() {
    let mut h = folders_app(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg", "/pics/work/d.jpg"]);
    click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "source:libfolder:/pics/trip");
    cmd_click(&mut h, "source:libfolder:/pics/work");
    assert_eq!(h.app.session.source_library_folders(), vec!["/pics/trip".to_string(), "/pics/work".to_string()]);
    assert_eq!(h.app.session.visible().len(), 3);
    assert_eq!(crate::i18n::source_title(&h.app.session), "2 folders", "the title says how many are shown");
    assert!(has(&h, "highlight:libfolder:/pics/trip") && has(&h, "highlight:libfolder:/pics/work"));
    // Shift+click takes the folders between, as drawn
    click(&mut h, "source:all");
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

/// A keyword that is gone is not picked any more.
#[test]
fn a_keyword_that_is_gone_is_not_picked_any_more() {
    let (mut h, _) = keyword_list();
    click(&mut h, "keywordRow:Sel K1");
    cmd_click(&mut h, "keywordRow:Sel K3");
    run(&mut h, "keyword.delete", json!({"keyword": "Sel K3"}));
    assert_eq!(picked(&h), vec!["Sel K1"]);
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
    click(&mut h, &row(a));
    cmd_click(&mut h, &row(c));
    assert!(!crate::menubar::in_normal_album(&h.app), "Remove from Album is not on offer for several albums");
    // a photo that is in A and in C (the one chosen last)
    run(&mut h, "library.select", json!({"ids": [ids[4]]}));
    ask(&mut h, "ui.key", json!({"key": "Delete"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1 }), "the photos' Delete asks");
    assert_eq!(h.app.session.catalog.album_count(AlbumId(a)), 2);
    assert_eq!(h.app.session.catalog.album_count(AlbumId(c)), 2);
}

/// Another library's rows are not what was chosen in this one.
#[test]
fn what_was_chosen_is_forgotten_when_another_library_opens() {
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
    assert!(h.app.ui.side.albums.anchor.is_some());
    let dir = Scratch(std::env::temp_dir().join(format!("lc-multi-select-switch-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    h.app.session.open_library(&dir.0, false).unwrap();
    h.settle(SETTLE);
    assert!(h.app.ui.side.albums.anchor.is_none());
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
