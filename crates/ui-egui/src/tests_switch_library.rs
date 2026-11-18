//! Switching libraries must not leave the old library's sidebar numbers, keyword tree or date
//! groups on screen. A library loaded from disk starts at catalog revision 0, and so does a brand
//! new empty one, so caches keyed on the revision alone would see "nothing changed".

use serde_json::json;

use crate::{LccApp, Services};

#[test]
fn opening_another_library_drops_the_old_librarys_cached_panels() {
    let dir = lcc_testkit::TempDir::new("lc-ui-switchlib");
    let _ = std::fs::remove_dir_all(&dir);
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    // library A with photos, reopened from disk (as at launch)
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(&a, true).unwrap();
    session.close_library().unwrap();
    session.open_library(&a, false).unwrap();
    assert!(!session.catalog.is_empty());

    let mut app = LccApp::new(session, Services::default());
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

fn temp(tag: &str) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("lc-ui-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app_in(dir: &std::path::Path) -> LccApp {
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    LccApp::new(session, Services::default())
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
        capture_one: false,
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

/// Photo IDs are local to a library: the brief info overlay's last photo is forgotten, so a photo
/// in the new library with the same ID shows the info again.
#[test]
fn switching_libraries_restarts_the_brief_info_overlay() {
    let dir = temp("switch-brief-info");
    let mut app = app_in(&dir);
    app.ui.brief_info = crate::state::BriefInfo { photo: Some(lcc_catalog::PhotoId(1)), since: 5.0, shown: true };
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Second"})).unwrap();
    assert_eq!(app.ui.brief_info, crate::state::BriefInfo::default());
    let _ = std::fs::remove_dir_all(dir);
}

fn recent_submenu(app: &LccApp) -> Vec<crate::menubar::MenuNode> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    match file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Open Recent")) {
        Some(MenuNode::Submenu { children, .. }) => children.clone(),
        _ => panic!("no Open Recent submenu in {file:?}"),
    }
}

/// Issue #902: File ▸ New Library… creates a named library and opens it; it shows in the window
/// title and in Open Recent Library, newest first.
#[test]
fn new_library_creates_opens_and_is_remembered_and_named() {
    let dir = temp("newlib");
    let mut app = app_in(&dir);
    app.library_changed(&egui::Context::default());
    assert_eq!(app.window_title(), format!("{}: first", lcc_brand::DISPLAY_NAME));

    let r = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Client A"})).unwrap();
    assert_eq!(r["name"], "Client A");
    let a = dir.join("Client A");
    assert_eq!(app.session.library.as_ref().unwrap().dir, a);
    assert!(lcc_engine::library::is_library(&a), "the catalog is written");
    assert_eq!(app.window_title(), format!("{}: Client A", lcc_brand::DISPLAY_NAME));
    assert_eq!(app.ui.settings.library_path, a.to_string_lossy());

    // a second New Library at the same place is refused and the current one stays open
    let e = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Client A"})).unwrap_err();
    assert!(e.contains("catalog here already"), "{e}");
    let e = app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "../x"})).unwrap_err();
    assert!(e.contains("catalog name"), "{e}");
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
    assert_eq!(app.window_title(), format!("{}: first", lcc_brand::DISPLAY_NAME));
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
    assert_eq!(value, lcc_engine::library::DEFAULT_NAME);
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
    // the menu takes what a background check found (issue #1290 review): let it answer first
    let ctx = egui::Context::default();
    crate::panels::library_problem::logic(&mut app, &ctx);
    assert!(crate::tasks::wait(&mut app, &ctx, std::time::Duration::from_secs(20)), "the check answered");
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

#[test]
fn reference_photo_ids_do_not_cross_library_boundaries() {
    let dir = temp("reference-switch");
    let (first, second) = (dir.join("first"), dir.join("second"));
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(&second, true).unwrap();
    let new_photo = session.visible_cloned()[0];
    session.close_library().unwrap();
    session.open_library(&first, true).unwrap();
    let old_reference = session.visible_cloned()[1];
    let mut app = LccApp::new(session, Services::default());
    app.run("library.select", json!({"ids": [old_reference.0]})).unwrap();
    app.run("photo.setReference", json!({})).unwrap();
    app.run("view.reference", json!({})).unwrap();
    let held = lcc_catalog::LibraryLock::acquire(&second, "another program").unwrap();
    assert!(app.run("app.openLibrary", json!({"path": second.to_string_lossy()})).is_err());
    assert_eq!(app.ui.reference, Some(old_reference.0), "a refused switch keeps the live pin");
    assert_eq!(app.ui.view, crate::state::ViewMode::Reference);
    drop(held);
    app.run("app.openLibrary", json!({"path": second.to_string_lossy()})).unwrap();
    assert!(app.session.catalog.photo(old_reference).is_some(), "the second library reuses this numeric ID");
    assert_eq!(app.ui.reference, None, "the first library's pin must not acquire a different photo");
    assert_eq!(app.ui.view, crate::state::ViewMode::PhotoGrid, "an empty Reference view is not left on screen");
    app.run("library.select", json!({"ids": [new_photo.0]})).unwrap();
    let r = app.run("view.reference", json!({})).unwrap();
    assert_eq!(r["reference"], new_photo.0);
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "Empty"})).unwrap();
    assert_eq!(app.ui.reference, None, "New Library clears the old reference too");
    assert!(app.run("view.reference", json!({})).is_err(), "an empty library cannot pin a reference");
    assert_eq!(app.ui.reference, None);
    app.session.close_library().unwrap();
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
    let held = lcc_catalog::LibraryLock::acquire(&other, "someone else").unwrap();
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
        assert!(e.contains("catalog name"), "{bad:?}: {e}");
    }
    assert!(app.session.library.as_ref().unwrap().dir.ends_with("first"));
    app.run("app.newLibrary", json!({"path": dir.to_string_lossy(), "name": "  Trip 2026  "})).unwrap();
    assert_eq!(app.session.library.as_ref().unwrap().dir, dir.join("Trip 2026"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The web build's page title names the active photo (a host embedding the page reads it to know
/// which document a drop opened), with • while changes are not yet in storage (issue #2397).
#[test]
fn page_title_names_the_active_photo_and_unsaved_changes() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    let ids = app.session.visible_cloned();
    let name = |app: &LccApp, id: lcc_catalog::PhotoId| app.session.catalog.photo(id).unwrap().file_name.clone();
    app.run("library.select", json!({"ids": [ids[0].0]})).unwrap();
    assert_eq!(app.page_title(false), format!("{}: {}", name(&app, ids[0]), lcc_brand::DISPLAY_NAME));
    assert_eq!(app.page_title(true), format!("{} •: {}", name(&app, ids[0]), lcc_brand::DISPLAY_NAME));
    app.run("library.select", json!({"ids": [ids[1].0]})).unwrap();
    assert_eq!(app.page_title(false), format!("{}: {}", name(&app, ids[1]), lcc_brand::DISPLAY_NAME));
    // no photo: the window title
    let empty = LccApp::new(lcc_engine::Session::new(), Services::default());
    assert!(empty.session.active().is_none());
    assert_eq!(empty.page_title(false), lcc_brand::DISPLAY_NAME);
    assert_eq!(empty.page_title(true), format!("{} •", lcc_brand::DISPLAY_NAME));
}

fn rename_app(tag: &str) -> (lcc_testkit::TempDir, std::path::PathBuf, LccApp) {
    let root = lcc_testkit::TempDir::new(&format!("lc-ui-renamelib-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let lib = root.join("Old Name");
    std::fs::create_dir_all(&lib).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(&lib, true).unwrap();
    let mut app = LccApp::new(session, Services::default());
    app.ui.settings.recent_libraries = vec![lib.to_string_lossy().into_owned()];
    app.ui.settings.library_path = lib.to_string_lossy().into_owned();
    (root, lib, app)
}

#[test]
fn rename_catalog_moves_the_folder_and_keeps_the_library_open() {
    let (root, lib, mut app) = rename_app("ok");
    let photos = app.session.catalog.len();
    assert!(photos > 0);
    let r = app.run("app.renameLibrary", json!({"name": "New Name"})).unwrap();
    let new = root.join("New Name");
    assert!(new.is_dir() && !lib.exists(), "folder renamed");
    assert!(lcc_engine::library::is_library(&new));
    assert_eq!(r["path"], new.to_string_lossy().as_ref());
    assert_eq!(app.session.catalog.len(), photos, "reopened with its photos");
    assert_eq!(app.session.library.as_ref().map(|l| l.dir.clone()), Some(new.clone()));
    assert_eq!(app.ui.settings.recent_libraries, vec![new.to_string_lossy().into_owned()]);
    assert_eq!(app.ui.settings.library_path, new.to_string_lossy().as_ref());
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_catalog_follows_photos_stored_inside_the_catalog() {
    let (root, lib, mut app) = rename_app("inside");
    let originals = lib.join("Originals");
    std::fs::create_dir_all(&originals).unwrap();
    let file = originals.join("a.jpg");
    std::fs::write(&file, b"x").unwrap();
    let id = app.session.catalog.alloc_photo_id();
    let photo = lcc_catalog::Photo::new(
        id,
        lcc_catalog::Source::File { path: file.to_string_lossy().into() },
        "a.jpg",
        "JPEG",
        60,
        40,
        "2026-01-01T10:00:00",
    );
    app.session.commit("add", lcc_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    let r = app.run("app.renameLibrary", json!({"name": "Renamed"})).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    let moved = root.join("Renamed").join("Originals").join("a.jpg");
    assert!(moved.is_file());
    let p = app.session.catalog.photo(id).expect("photo kept");
    assert!(matches!(&p.source, lcc_catalog::Source::File { path } if std::path::Path::new(path) == moved), "{:?}", p.source);
    // undoing must not point the photo back at a folder that is gone
    let _ = app.run("edit.undo", json!({}));
    let p = app.session.catalog.photo(id).expect("photo kept");
    assert!(matches!(&p.source, lcc_catalog::Source::File { path } if std::path::Path::new(path) == moved), "{:?}", p.source);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_catalog_refuses_bad_names_and_leaves_the_library_alone() {
    let (root, lib, mut app) = rename_app("bad");
    std::fs::create_dir_all(root.join("Taken")).unwrap();
    let photos = app.session.catalog.len();
    for bad in ["", "  ", "a/b", "..", "x\\y", "Taken", "Old Name", "ends."] {
        let e = app.run("app.renameLibrary", json!({"name": bad})).unwrap_err();
        assert!(e.contains("app.renameLibrary"), "{bad:?}: {e}");
        assert!(lib.is_dir(), "{bad:?} left the folder");
        assert_eq!(app.session.library.as_ref().map(|l| l.dir.clone()), Some(lib.clone()), "{bad:?}");
        assert_eq!(app.session.catalog.len(), photos);
    }
    assert!(app.run("app.renameLibrary", json!({"name": 5})).is_err());
    // no name: asks for one
    app.run("app.renameLibrary", json!({})).unwrap();
    assert!(matches!(app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_catalog_needs_a_catalog_on_disk() {
    let mut app = LccApp::new(lcc_engine::Session::new(), Services::default());
    assert!(!crate::menus::ui_enabled(&app, "app.renameLibrary"));
    assert!(app.run("app.renameLibrary", json!({"name": "x"})).is_err());
}
