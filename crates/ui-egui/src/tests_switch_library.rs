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
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&second, true).unwrap();
    let new_photo = session.visible_cloned()[0];
    session.close_library().unwrap();
    session.open_library(&first, true).unwrap();
    let old_reference = session.visible_cloned()[1];
    let mut app = LightcraftApp::new(session, Services::default());
    app.run("library.select", json!({"ids": [old_reference.0]})).unwrap();
    app.run("photo.setReference", json!({})).unwrap();
    app.run("view.reference", json!({})).unwrap();
    let held = lightcraft_catalog::LibraryLock::acquire(&second, "another program").unwrap();
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

/// Issue #2718: a library chosen in File ▸ Open Recent Library opens after the window has closed
/// the menu and said which library is opening; it used to open in the frame of the click, leaving
/// the menu on screen with no word of what was happening while a large library opened.
#[test]
fn open_recent_library_says_it_is_opening_before_it_opens() {
    let dir = temp("open-recent-message");
    let second = dir.join("second");
    // (dropped: its lock on the library goes with it)
    lightcraft_engine::Session::new().with_fs().open_library(&second, false).unwrap();
    let mut app = app_in(&dir);
    app.ui.settings.recent_libraries = vec![second.to_string_lossy().into_owned()];
    let mut h = crate::headless::Headless::new(app, [1280.0, 800.0], 1.0);
    h.settle(std::time::Duration::from_secs(10));
    let t = std::time::Duration::from_secs(5);
    for (method, id) in [("ui.clickWidget", "menu:File"), ("ui.hoverWidget", "menusub:Open Recent Library")] {
        let r = h.request(method, json!({"id": id}), t);
        assert_eq!(r["ok"], true, "{method} {id}: {r}");
        h.settle(std::time::Duration::from_secs(10));
    }
    let find = |h: &crate::headless::Headless, id: &str| h.app.widgets.iter().filter(|(w, _)| w == id).map(|(_, r)| *r).collect::<Vec<_>>();
    let submenu = find(&h, "menu-level:2")[0];
    // the submenu lists the recent libraries in order (the open one too)
    let nth = h.app.ui.settings.recent_libraries.iter().position(|r| std::path::Path::new(r) == second).expect("second is recent");
    let mut items: Vec<_> = find(&h, "menuitem:app.openLibrary").into_iter().filter(|r| submenu.contains_rect(*r)).collect();
    items.sort_by(|a, b| a.top().total_cmp(&b.top()));
    let item = items[nth];
    let at = item.center();
    let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    for event in [egui::Event::PointerMoved(at), click(true), click(false)] {
        h.events.push(event);
        h.step();
    }
    // the frame after the click: the menu is closed, the message is up, the library not opened yet
    h.step();
    let open = |h: &crate::headless::Headless| h.app.session.library.as_ref().map(|l| l.dir.clone());
    assert_eq!(open(&h), Some(dir.join("first")), "the old library is still open");
    let message = h.app.opening_library.as_ref().map(|p| p.message.clone());
    assert_eq!(message.as_deref(), Some("Opening library “second”…"));
    assert!(!find(&h, "libraryOpening").is_empty(), "the message is drawn");
    assert!(find(&h, "menu-level:1").is_empty() && find(&h, "menu-level:2").is_empty(), "the menu is closed");
    // the next frame opens it
    h.step();
    assert_eq!(
        open(&h).and_then(|d| d.canonicalize().ok()),
        second.canonicalize().ok(),
        "the new library is open: {:?}",
        h.app.ui.toast.as_ref().map(|t| &t.0)
    );
    assert!(h.app.opening_library.is_none());
    assert!(find(&h, "libraryOpening").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2718: a library chosen in the Open Library folder dialog waits for its message too.
#[test]
fn a_library_chosen_in_open_library_says_it_is_opening_before_it_opens() {
    let dir = temp("open-picked-message");
    let second = dir.join("second");
    let mut app = app_in(&dir);
    let answer: std::sync::Arc<std::sync::Mutex<Vec<std::sync::mpsc::Sender<Vec<String>>>>> = Default::default();
    let a = answer.clone();
    app.services.picker = Some(Box::new(move |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        a.lock().unwrap().push(tx);
        Ok(rx)
    }));
    let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(std::time::Duration::from_secs(10));
    assert_eq!(h.app.run("app.openLibrary", json!({})).unwrap(), serde_json::Value::Null);
    answer.lock().unwrap()[0].send(vec![second.to_string_lossy().into_owned()]).unwrap();
    let t = std::time::Duration::from_secs(5);
    assert!(h.step_until(t, |h| h.app.opening_library.is_some()), "the answer waits");
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.clone()), Some(dir.join("first")), "not opened yet");
    assert!(h.step_until(t, |h| h.app.opening_library.is_none()));
    let open = h.app.session.library.as_ref().and_then(|l| l.dir.canonicalize().ok());
    assert_eq!(open, second.canonicalize().ok(), "{:?}", h.app.ui.toast.as_ref().map(|t| &t.0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The web build's page title names the active photo (a host embedding the page reads it to know
/// which document a drop opened), with • while changes are not yet in storage (issue #2397).
#[test]
fn page_title_names_the_active_photo_and_unsaved_changes() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let ids = app.session.visible_cloned();
    let name = |app: &LightcraftApp, id: lightcraft_catalog::PhotoId| app.session.catalog.photo(id).unwrap().file_name.clone();
    app.run("library.select", json!({"ids": [ids[0].0]})).unwrap();
    assert_eq!(app.page_title(false), format!("{} — LightCraft", name(&app, ids[0])));
    assert_eq!(app.page_title(true), format!("{} • — LightCraft", name(&app, ids[0])));
    app.run("library.select", json!({"ids": [ids[1].0]})).unwrap();
    assert_eq!(app.page_title(false), format!("{} — LightCraft", name(&app, ids[1])));
    // no photo: the window title
    let empty = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    assert!(empty.session.active().is_none());
    assert_eq!(empty.page_title(false), "LightCraft");
    assert_eq!(empty.page_title(true), "LightCraft •");
}
