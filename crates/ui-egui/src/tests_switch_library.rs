//! Switching libraries must not leave the old library's sidebar numbers, keyword tree or date
//! groups on screen. A library loaded from disk starts at catalog revision 0, and so does a brand
//! new empty one, so caches keyed on the revision alone would see "nothing changed".

use serde_json::json;

use crate::{LightcraftApp, Services};

#[test]
fn opening_another_library_drops_the_old_librarys_cached_panels() {
    let dir = std::env::temp_dir().join(format!("lc-ui-switchlib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    // library A with photos, reopened from disk (as at launch)
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&a, true).unwrap();
    session.close_library().unwrap();
    session.open_library(&a, false).unwrap();
    assert!(!session.catalog.is_empty());

    let mut app = LightcraftApp::new(session, Services::default());
    let (cat, caches) = (&app.session.catalog, &mut app.caches);
    assert!(caches.counts(cat).total > 0);
    assert!(!caches.keyword_tree(cat).is_empty());
    assert!(!caches.date_groups(cat).is_empty());

    // the grid's own list is cached too
    assert!(!app.session.visible().is_empty());

    app.run("app.openLibrary", json!({"path": b.to_string_lossy()})).unwrap();
    assert!(app.session.catalog.is_empty(), "library B is empty");
    let (cat, caches) = (&app.session.catalog, &mut app.caches);
    assert_eq!(caches.counts(cat).total, 0, "All Photos count");
    assert_eq!(caches.counts(cat).picks, 0);
    assert!(caches.keyword_tree(cat).is_empty(), "keywords");
    assert!(caches.date_groups(cat).is_empty(), "By Date");
    assert!(app.session.visible().is_empty(), "grid");

    let _ = std::fs::remove_dir_all(&dir);
}

fn temp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app_in(dir: &std::path::Path) -> LightcraftApp {
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    LightcraftApp::new(session, Services::default())
}

#[test]
fn switching_libraries_dismisses_the_previous_lightroom_result() {
    let dir = temp("switch-lightroom-result");
    let mut app = app_in(&dir);
    let result = || crate::state::Dialog::LightroomResult {
        summary: "Imported into first".into(),
        library: Some(dir.join("first").to_string_lossy().into_owned()),
        details: Vec::new(),
        first: Some(1),
    };
    app.ui.dialog = Some(result());
    app.lightroom_result = Some(result());
    app.lightroom_last = Some(json!({"imported": 1}));
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Second"})).unwrap();
    assert!(app.ui.dialog.is_none(), "the old destination and photo ID must not act on the new library");
    assert!(app.lightroom_result.is_none());
    assert!(app.lightroom_last.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

fn recent_submenu(app: &LightcraftApp) -> Vec<crate::menubar::MenuNode> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    match file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Open Recent Library")) {
        Some(MenuNode::Submenu { children, .. }) => children.clone(),
        _ => panic!("no Open Recent Library submenu in {file:?}"),
    }
}

/// Issue #902: File ▸ New Library… creates a named library and opens it; it shows in the window
/// title and in Open Recent Library, newest first.
#[test]
fn new_library_creates_opens_and_is_remembered_and_named() {
    let dir = temp("newlib");
    let mut app = app_in(&dir);
    app.library_changed(&egui::Context::default());
    assert_eq!(app.window_title(), "LightCraft — first");

    let r = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Client A"})).unwrap();
    assert_eq!(r["name"], "Client A");
    let a = dir.join("Client A");
    assert_eq!(app.session.library.as_ref().unwrap().dir, a);
    assert!(lightcraft_engine::library::is_library(&a), "the catalog is written");
    assert_eq!(app.window_title(), "LightCraft — Client A");
    assert_eq!(app.ui.settings.library_path, a.to_string_lossy());

    // a second New Library at the same place is refused and the current one stays open
    let e = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Client A"})).unwrap_err();
    assert!(e.contains("library here already"), "{e}");
    let e = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "../x"})).unwrap_err();
    assert!(e.contains("library name"), "{e}");
    assert_eq!(app.session.library.as_ref().unwrap().dir, a);

    let list = app.run("app.recentLibraries", json!({})).unwrap();
    let names: Vec<_> = list.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["Client A", "first"]);
    assert_eq!(list[0]["open"], true);
    assert_eq!(list[1]["exists"], true);

    // Open Recent Library reopens "first"
    let items = recent_submenu(&app);
    let first = items
        .iter()
        .find_map(|n| match n {
            crate::menubar::MenuNode::Item { id, params, enabled, .. }
                if id == "app.openLibrary" && params["path"].as_str().is_some_and(|p| p.ends_with("first")) =>
            {
                assert!(*enabled);
                Some(params.clone())
            }
            _ => None,
        })
        .expect("first in Open Recent Library");
    app.run("app.openLibrary", first).unwrap();
    assert_eq!(app.window_title(), "LightCraft — first");
    let _ = std::fs::remove_dir_all(&dir);
}

/// From the menu, New Library… asks for a name (default prefilled), then where to put it.
#[test]
fn new_library_from_the_menu_asks_for_a_name_then_a_location() {
    let dir = temp("newlibprompt");
    let mut app = app_in(&dir);
    let loc = dir.to_string_lossy().into_owned();
    app.services.pick_folder = Some(Box::new(move || Some(loc.clone())));
    app.run("app.newLibrary", json!({})).unwrap();
    let Some(crate::state::Dialog::TextPrompt { value, command, key, .. }) = app.ui.dialog.as_mut() else {
        panic!("no name prompt: {:?}", app.ui.dialog)
    };
    assert_eq!(value, lightcraft_engine::library::DEFAULT_NAME);
    assert_eq!((command.as_str(), key.as_str()), ("app.newLibrary", "name"));
    *value = "Wedding".into();
    // what the dialog's OK runs
    app.ui.dialog = None;
    app.run("app.newLibrary", json!({"name": "Wedding"})).unwrap();
    assert_eq!(app.session.library.as_ref().unwrap().dir, dir.join("Wedding"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A remembered library that's gone is greyed out in the menu; opening it anyway is an error that
/// keeps the current library open and drops it from the list. The list is capped.
#[test]
fn missing_recent_libraries_are_greyed_out_and_pruned_and_the_list_is_capped() {
    let dir = temp("recentlib");
    let mut app = app_in(&dir);
    let gone = dir.join("gone").to_string_lossy().into_owned();
    app.ui.settings.recent_libraries = vec![gone.clone()];
    let disabled = recent_submenu(&app)
        .iter()
        .any(|n| matches!(n, crate::menubar::MenuNode::Item { params, enabled: false, .. } if params["path"] == gone.as_str()));
    assert!(disabled, "missing library greyed out");
    let e = app.run("app.openLibrary", json!({"path": gone})).unwrap_err();
    assert!(e.contains("no longer there"), "{e}");
    assert!(!std::path::Path::new(&gone).exists(), "no empty library created in its place");
    assert!(app.ui.settings.recent_libraries.is_empty());
    assert!(app.session.library.as_ref().unwrap().dir.ends_with("first"));

    for i in 0..20 {
        crate::panels::settings::remember_library(&mut app.ui.settings.recent_libraries, &format!("/x/{i}"));
    }
    assert_eq!(app.ui.settings.recent_libraries.len(), crate::state::RECENT_LIBRARIES);
    assert_eq!(app.ui.settings.recent_libraries[0], "/x/19");
    app.run("app.clearRecentLibraries", json!({})).unwrap();
    assert!(app.ui.settings.recent_libraries.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library another program has open is refused before the current one is closed: the current
/// library stays open with its photos, and the window title keeps its name.
#[test]
fn switching_to_a_locked_library_keeps_the_current_one_open() {
    let dir = temp("lockedlib");
    let mut app = app_in(&dir);
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Other"})).unwrap();
    app.run("app.openLibrary", json!({"path": dir.join("first").to_string_lossy()})).unwrap();
    // "another program" holds Other's lock
    let other = dir.join("Other");
    let held = lightcraft_catalog::LibraryLock::acquire(&other, "someone else").unwrap();
    let e = app.run("app.openLibrary", json!({"path": other.to_string_lossy()})).unwrap_err();
    assert!(e.contains("already open"), "{e}");
    assert!(app.session.library.as_ref().unwrap().dir.ends_with("first"), "the current library stays");
    assert!(app.session.library.as_ref().unwrap().on_disk);
    assert!(app.ui.settings.library_path.ends_with("first"));
    drop(held);
    app.run("app.openLibrary", json!({"path": other.to_string_lossy()})).unwrap();
    assert!(app.session.library.as_ref().unwrap().dir.ends_with("Other"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Library names that can't be folder names everywhere (Windows' reserved characters, a
/// trailing dot) are refused; spaces around a name are trimmed.
#[test]
fn library_names_must_work_as_folder_names_on_every_system() {
    let dir = temp("libnames");
    let mut app = app_in(&dir);
    for bad in ["a<b", "a>b", "a:b", "a\"b", "a/b", "a\\b", "a|b", "a?b", "a*b", "name.", "..", " ", "a\u{7}"] {
        let e = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": bad})).unwrap_err();
        assert!(e.contains("library name"), "{bad:?}: {e}");
    }
    assert!(app.session.library.as_ref().unwrap().dir.ends_with("first"));
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "  Trip 2026  "})).unwrap();
    assert_eq!(app.session.library.as_ref().unwrap().dir, dir.join("Trip 2026"));
    let _ = std::fs::remove_dir_all(&dir);
}
