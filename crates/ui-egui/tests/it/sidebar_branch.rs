//! Option-click (Alt-click) on a sidebar tree row's triangle opens or folds the row and every row
//! inside it, also the rows that are not drawn yet because a row above them is folded.
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::{LightcraftApp, Services};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(30);

fn start(session: Session) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1400.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    h
}

fn frames(h: &mut Headless) {
    for _ in 0..3 {
        h.step();
    }
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// A click on the widget `id`, with Option (Alt) held or not.
fn click(h: &mut Headless, id: &str, alt: bool) {
    let r = h.request("ui.clickWidget", json!({"id": id, "alt": alt}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    frames(h);
}

fn make(h: &mut Headless, params: Value) -> u64 {
    let id = h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    frames(h);
    id
}

/// Photos at `paths`, each with its keywords and capture time.
fn library(photos: &[(&str, &[&str], &str)]) -> Session {
    let mut s = Session::new();
    for (i, (path, keywords, taken)) in photos.iter().enumerate() {
        let name = path.rsplit('/').next().unwrap();
        let mut p = Photo::new(PhotoId(i as u64 + 1), Source::File { path: (*path).into() }, name, "JPEG", 60, 40, "2026-03-01T00:00:00");
        p.captured = Some((*taken).to_string());
        p.meta.keywords = keywords.iter().map(|k| (*k).to_string()).collect();
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s
}

fn toggle(kind: &str, id: impl std::fmt::Display) -> String {
    format!("{kind}Toggle:{id}")
}

/// Given folders of albums three deep, when the outer folder's triangle is clicked with Option,
/// then it and the folders inside it fold together, and an Option-click opens them all again,
/// though the inner folders were not drawn while the outer one was folded.
#[test]
fn option_click_folds_and_opens_a_branch_of_album_folders() {
    let mut h = start(Session::new());
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let europe = make(&mut h, json!({"name": "Europe", "folder": true, "parent": trips}));
    let france = make(&mut h, json!({"name": "France", "folder": true, "parent": europe}));
    let best = make(&mut h, json!({"name": "Best", "parent": france}));
    let other = make(&mut h, json!({"name": "Other", "folder": true}));
    let kept = make(&mut h, json!({"name": "Kept", "parent": other}));
    let (best_row, kept_row) = (format!("source:album:{best}"), format!("source:album:{kept}"));
    assert!(has(&h, &best_row), "folders start open");

    click(&mut h, &toggle("album", trips), true);
    assert!(!has(&h, &format!("source:folder:{europe}")) && !has(&h, &best_row), "the branch is folded");
    assert!(has(&h, &kept_row), "a folder beside it is not");

    // a plain click opens the one row: the folders inside it stay folded
    click(&mut h, &toggle("album", trips), false);
    assert!(has(&h, &format!("source:folder:{europe}")), "Trips is open");
    assert!(!has(&h, &format!("source:folder:{france}")) && !has(&h, &best_row), "Europe stays folded");

    // fold it by hand, and Option-click it open: the folders that were never drawn open too
    click(&mut h, &toggle("album", trips), false);
    click(&mut h, &toggle("album", trips), true);
    assert!(has(&h, &best_row), "Europe and France are open");

    // what is said of a row afterwards holds for that row alone
    click(&mut h, &toggle("album", trips), true);
    click(&mut h, &toggle("album", trips), false);
    click(&mut h, &toggle("album", europe), false);
    assert!(has(&h, &format!("source:folder:{france}")) && !has(&h, &best_row), "Europe is open, France stays folded");

    // a folder made inside a branch that was folded this way follows it: folded
    click(&mut h, &toggle("album", trips), true);
    click(&mut h, &toggle("album", trips), false);
    let africa = make(&mut h, json!({"name": "Africa", "folder": true, "parent": trips}));
    let kenya = make(&mut h, json!({"name": "Kenya", "parent": africa}));
    assert!(has(&h, &format!("source:folder:{africa}")) && !has(&h, &format!("source:album:{kenya}")), "Africa is folded, as the branch was left");
}

/// Given the Folders and Keywords trees and By Date, when a top row's triangle is clicked with
/// Option, then everything inside it opens, and an Option-click folds it again.
#[test]
fn option_click_opens_and_folds_folders_keywords_and_dates() {
    let mut h = start(library(&[
        ("/lc-branch/a/b/c/one.jpg", &["Places|Europe|France"], "2026-01-05T10:00:00"),
        ("/lc-branch/a/b/two.jpg", &["Places|Europe|Spain"], "2026-01-06T10:00:00"),
        ("/lc-branch/a/three.jpg", &["Places|Asia"], "2026-02-07T10:00:00"),
    ]));
    // Keywords
    assert!(has(&h, "source:keyword:Places") && !has(&h, "source:keyword:Places|Europe"), "keywords start folded");
    click(&mut h, &toggle("keyword", "Places"), true);
    assert!(has(&h, "source:keyword:Places|Europe|France"), "Places opens with the keywords inside it");
    click(&mut h, &toggle("keyword", "Places"), true);
    assert!(!has(&h, "source:keyword:Places|Europe"), "and folds");
    click(&mut h, &toggle("keyword", "Places"), false);
    assert!(has(&h, "source:keyword:Places|Europe") && !has(&h, "source:keyword:Places|Europe|France"), "a plain click opens one row");
    // By Date
    assert!(has(&h, "source:date:2026") && !has(&h, "source:date:2026-01"), "dates start folded");
    click(&mut h, &toggle("date", "2026"), true);
    assert!(has(&h, "source:date:2026-01") && has(&h, "source:date:2026-01-05") && has(&h, "source:date:2026-02-07"), "the months and their days");
    click(&mut h, &toggle("date", "2026"), true);
    assert!(!has(&h, "source:date:2026-01"));
    // Folders: This Computer, down to the last folder
    let top = toggle("libraryFolder", "/");
    let (below, deepest) = ("source:libfolder:/lc-branch", "source:libfolder:/lc-branch/a/b/c");
    assert!(has(&h, below) && !has(&h, deepest), "This Computer starts open, the folders in it as they begin");
    click(&mut h, &top, true);
    assert!(!has(&h, below), "folded with everything in it");
    click(&mut h, &top, true);
    assert!(has(&h, deepest), "opens down to the last folder, which was not drawn");
}

/// Given a folder on disk in Local with folders inside it, when its triangle is clicked with
/// Option, then opening opens the one folder (a disk's folders are read as they are opened), and
/// folding folds everything inside it.
#[test]
fn option_click_in_local_folds_everything_and_opens_one_row() {
    let root = std::env::temp_dir().join(format!("lc-branch-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    let path = root.to_string_lossy().to_string();
    let mut h = start(Session::new());
    h.app.run("local.addRoot", json!({"path": path})).unwrap();
    frames(&mut h);
    let sub = |name: &str| std::path::Path::new(&path).join(name).to_string_lossy().to_string();
    let (a_path, b_path) = (sub("a"), std::path::Path::new(&sub("a")).join("b").to_string_lossy().to_string());
    let (a, b) = (format!("source:local:{a_path}"), format!("source:local:{b_path}"));
    let root_toggle = toggle("folder", &path);
    assert!(has(&h, &root_toggle), "the folder is listed in Local");
    click(&mut h, &root_toggle, true);
    assert!(h.step_until(T, |h| has(h, &a)), "Option opens the folder");
    // (the folders inside it are listed on a worker: wait for them before saying one is missing)
    h.settle(T);
    frames(&mut h);
    assert!(!has(&h, &b), "and no more than that");
    click(&mut h, &toggle("folder", &a_path), false);
    assert!(h.step_until(T, |h| has(h, &b)), "a plain click opens the one below");
    click(&mut h, &root_toggle, true);
    assert!(!has(&h, &a), "Option folds it");
    click(&mut h, &root_toggle, false);
    assert!(has(&h, &a) && !has(&h, &b), "and what was inside it is folded too");
    let _ = std::fs::remove_dir_all(&root);
}
