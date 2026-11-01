//! Issue #1290: what opens at launch. Settings ▸ General ▸ At launch opens the last library or
//! asks with a window listing the recent ones (greyed out with the reason when they can't be
//! opened); Alt/Option held as the window appears asks once; one recent entry can be forgotten from
//! File ▸ Open Recent Library, the window and `app.recentLibraries {remove}`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::{ALT_SECONDS, Launch, LibraryProblem, launch_plan};
use crate::state::{AppSettings, LaunchLibrary};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A fresh scratch folder for one test.
fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-launch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A library on disk at `dir` (written, then closed: as left by an earlier run).
fn make_library(dir: &Path) -> String {
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir, false).unwrap();
    s.close_library().unwrap();
    drop(s);
    assert!(lightcraft_engine::library::is_library(dir));
    dir.to_string_lossy().into_owned()
}

/// Enough frames for a synthetic click (move, press, release) and what it starts.
fn settle(h: &mut Headless) {
    for _ in 0..6 {
        h.step();
    }
}

/// The widget `id` was drawn in the last frame.
fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// `Some(s)` as a path, for the launch plan's arguments.
fn path(s: &str) -> Option<PathBuf> {
    Some(PathBuf::from(s))
}

/// `--library`, then `LIGHTCRAFT_LIBRARY`, then the setting; `--memory` never asks. Only the
/// library remembered in Settings is `remembered` (issue #3239: a missing one is reported, not
/// made anew), and not when it is the default location.
#[test]
fn the_launch_plan_puts_the_flag_and_the_variable_before_the_setting() {
    let ask = AppSettings { at_launch: LaunchLibrary::Ask, library_path: "/last".into(), ..AppSettings::default() };
    let last = AppSettings { library_path: "/last".into(), recent_libraries: vec!["/last".into(), "/older".into()], ..AppSettings::default() };
    let at_default = AppSettings { library_path: "/default".into(), ..AppSettings::default() };
    let fresh = AppSettings::default();
    let open = |dir: &str, alt_asks, remembered| Launch::Open { dir: path(dir), alt_asks, remembered };
    // --library, then LIGHTCRAFT_LIBRARY, whatever the setting says; neither asks on Alt
    for s in [None, Some(&ask), Some(&last)] {
        assert_eq!(launch_plan(false, path("/flag"), path("/env"), s, path("/default")), open("/flag", false, false));
        assert_eq!(launch_plan(false, None, path("/env"), s, path("/default")), open("/env", false, false));
    }
    // Ask me asks, with or without a recent list
    assert_eq!(launch_plan(false, None, None, Some(&ask), path("/default")), Launch::Ask);
    let ask_empty = AppSettings { at_launch: LaunchLibrary::Ask, ..AppSettings::default() };
    assert_eq!(launch_plan(false, None, None, Some(&ask_empty), None), Launch::Ask);
    // Open the last library: the last one, else the default place; Alt can still ask
    assert_eq!(launch_plan(false, None, None, Some(&last), path("/default")), open("/last", true, true));
    assert_eq!(launch_plan(false, None, None, Some(&last), None), open("/last", true, true));
    assert_eq!(launch_plan(false, None, None, Some(&at_default), path("/default")), open("/default", true, false));
    assert_eq!(launch_plan(false, None, None, Some(&fresh), path("/default")), open("/default", true, false));
    assert_eq!(launch_plan(false, None, None, None, path("/default")), open("/default", true, false));
    assert_eq!(launch_plan(false, None, None, None, None), Launch::Open { dir: None, alt_asks: true, remembered: false });
    // --memory never asks and opens no folder
    assert_eq!(launch_plan(true, None, None, Some(&ask), path("/default")), Launch::Open { dir: None, alt_asks: false, remembered: false });
    assert_eq!(launch_plan(true, path("/flag"), None, Some(&last), None), Launch::Open { dir: None, alt_asks: false, remembered: false });
}

/// Each listed library is checked when the window opens; the last one opened is selected.
#[test]
fn the_list_checks_each_library_and_selects_the_last_one() {
    let dir = temp("choose");
    let (a, b, held, plain) = (dir.join("A"), dir.join("B"), dir.join("Held"), dir.join("Plain"));
    let (a, b, held_path) = (make_library(&a), make_library(&b), make_library(&held));
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("notes.txt"), b"not a library").unwrap();
    let file = dir.join("a-file.txt");
    std::fs::write(&file, b"a file").unwrap();
    let (gone, plain, file) =
        (dir.join("Gone").to_string_lossy().into_owned(), plain.to_string_lossy().into_owned(), file.to_string_lossy().into_owned());
    // another program has it open: listing doesn't take locks, so it is found when opened
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&held_path), "another LightCraft").unwrap();

    let mut settings = AppSettings {
        recent_libraries: vec![gone.clone(), held_path.clone(), a.clone(), plain.clone(), file.clone(), b.clone()],
        library_path: b.clone(),
        ..AppSettings::default()
    };
    settings.recent_opened.insert(b.clone(), "2026-10-20T08:30:00".into());
    let p = LibraryProblem::choose(&settings);
    assert!(!p.saves_nothing(), "the launch list is not a temporary session: removals are saved");
    let c = p.chooser.as_ref().unwrap();
    let why: Vec<Option<&str>> = c.libraries.iter().map(|l| l.unavailable.as_deref()).collect();
    assert!(why[0].unwrap().contains("Not found"), "{why:?}");
    assert_eq!(why[1], None, "a lock is not probed: {why:?}");
    assert_eq!(why[2], None);
    assert!(why[3].unwrap().contains("no LightCraft library"), "{why:?}");
    assert!(why[4].unwrap().contains("not a library folder"), "{why:?}");
    assert_eq!(why[5], None);
    assert_eq!(c.selected, Some(5), "the last library opened");
    assert_eq!(c.libraries[5].opened.as_deref(), Some("2026-10-20T08:30:00"));
    assert_eq!(c.libraries[2].opened, None, "no time known");
    let json = p.to_json();
    assert_eq!(json["chooser"]["libraries"][0]["available"], false);
    assert_eq!(json["chooser"]["selected"], 5);

    // the last library can't be opened: the first one that can is selected
    settings.library_path = plain.clone();
    assert_eq!(LibraryProblem::choose(&settings).chooser.unwrap().selected, Some(1));
    // none can: the first row, Open greyed out
    settings.recent_libraries = vec![gone.clone(), file.clone()];
    assert_eq!(LibraryProblem::choose(&settings).chooser.unwrap().selected, Some(0));
    // an empty list selects nothing
    settings.recent_libraries.clear();
    let c = LibraryProblem::choose(&settings).chooser.unwrap();
    assert!(c.libraries.is_empty() && c.selected.is_none());
    assert!(LibraryProblem::new("/x", "e").saves_nothing(), "the failed-library window still saves nothing");
    drop(lock);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under `dir`: relative path → (contents, length, modification time).
fn snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, (Option<Vec<u8>>, u64, std::time::SystemTime)> {
    let mut out = std::collections::BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::metadata(&p).unwrap();
            if m.is_dir() {
                todo.push(p.clone());
            }
            let (bytes, len) = if m.is_file() { (contents(&p), m.len()) } else { (Some(Vec::new()), 0) };
            out.insert(p.strip_prefix(dir).unwrap().to_path_buf(), (bytes, len, m.modified().unwrap()));
        }
    }
    out
}

/// The bytes of the file `p`, or `None` for a lock file that a [`lightcraft_catalog::LibraryLock`]
/// holds on Windows: there `LockFileEx` locks the file's bytes, and reading them through another
/// handle fails with os error 33 (`ERROR_LOCK_VIOLATION`). Such a file is compared by its length
/// and modification time only.
fn contents(p: &Path) -> Option<Vec<u8>> {
    match std::fs::read(p) {
        Ok(bytes) => Some(bytes),
        Err(e) if cfg!(windows) && e.raw_os_error() == Some(33) && p.file_name().is_some_and(|n| n == lightcraft_catalog::lock::LOCK) => None,
        Err(e) => panic!("{}: {e}", p.display()),
    }
}

/// Listing the recent libraries (and drawing the window) writes nothing into any of them: no
/// lock file, no owner file, nothing modified — free, held by another program or not a library.
#[test]
fn listing_the_libraries_leaves_every_folder_byte_identical() {
    let dir = temp("readonly");
    let (free, fresh, held) = (dir.join("Free"), dir.join("Fresh"), dir.join("Held"));
    let (free, held_path) = (make_library(&free), make_library(&held));
    // a library whose lock files were never made (copied over from elsewhere, say)
    let fresh_path = make_library(&fresh);
    for f in [lightcraft_catalog::lock::LOCK, lightcraft_catalog::lock::OWNER] {
        let _ = std::fs::remove_file(fresh.join(f));
    }
    std::fs::create_dir_all(dir.join("Plain")).unwrap();
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&held_path), "another LightCraft").unwrap();
    let recent = vec![free.clone(), fresh_path.clone(), held_path.clone(), dir.join("Plain").to_string_lossy().into_owned()];
    let before = snapshot(&dir);
    // (mtime granularity: a write in the same instant would still change contents or add files)
    let settings = AppSettings { recent_libraries: recent.clone(), library_path: free.clone(), ..AppSettings::default() };
    let p = LibraryProblem::choose(&settings);
    assert_eq!(snapshot(&dir), before, "checking the rows must not create or change any file");
    assert!(p.chooser.as_ref().unwrap().libraries.iter().take(3).all(|l| l.unavailable.is_none()));
    let mut h = asking(recent, &free, None);
    settle(&mut h);
    assert!(has(&h, "libraryRecent:2"));
    drop(h);
    assert_eq!(snapshot(&dir), before, "listing must not create or change any file");
    assert!(!fresh.join(lightcraft_catalog::lock::LOCK).exists() && !fresh.join(lightcraft_catalog::lock::OWNER).exists());
    drop(lock);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The app as the desktop host starts it with *Ask me*: no library open, the list showing.
fn asking(recent: Vec<String>, last: &str, picked: Option<String>) -> Headless {
    let session = lightcraft_engine::Session::new().with_fs();
    // (an encoder for the screenshots LIGHTCRAFT_TEST_SHOTS asks for)
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(session, services);
    app.ui.settings.recent_libraries = recent;
    app.ui.settings.library_path = last.to_string();
    app.ui.settings.at_launch = LaunchLibrary::Ask;
    if let Some(p) = picked {
        app.services.pick_folder = Some(Box::new(move || Some(p.clone())));
    }
    app.library_problem = Some(LibraryProblem::choose(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    h
}

/// The window lists every recent library, greys out a missing one and opens the chosen one.
#[test]
fn the_window_lists_the_libraries_and_opens_the_chosen_one() {
    let dir = temp("window");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut h = asking(vec![a.clone(), gone.clone(), b.clone()], &a, None);
    assert!(h.app.session.library.is_none(), "nothing opened before the choice");
    for id in ["libraryRecent:0", "libraryRecent:1", "libraryRecent:2", "button:libraryOpen", "button:libraryOpenAnother", "button:libraryNew"] {
        assert!(has(&h, id), "{id}");
    }
    assert!(has(&h, "button:libraryQuit") && has(&h, "button:libraryForget"));
    assert!(!has(&h, "button:libraryRetry"), "not the failed-library window");
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": Path::new(&p).join("launch-list.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let inspect = h.request("ui.inspect", json!({}), T);
    let c = &inspect["result"]["libraryProblem"]["chooser"];
    assert_eq!(c["selected"], 0, "{c}");
    assert_eq!(c["libraries"][1]["available"], false, "{c}");

    // ↓ onto the missing one: Return does nothing there (Open is greyed out)
    assert_eq!(h.request("ui.key", json!({"key": "ArrowDown"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert!(h.app.session.library.is_none() && h.app.library_problem.is_some());
    // ↓ again, then Open: Beta opens and the window goes away
    h.request("ui.key", json!({"key": "ArrowDown"}), T);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryOpen"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(b.clone()));
    assert!(h.app.library_problem.is_none());
    assert!(!has(&h, "button:libraryOpen"));
    assert_eq!(h.app.ui.settings.library_path, b);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Return opens the selection; a library taken meanwhile stays listed with the reason.
#[test]
fn return_opens_the_selected_library_and_a_failure_says_why() {
    let dir = temp("return");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let mut h = asking(vec![a.clone(), b.clone()], &b, None);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    // another program takes Beta after the window opened: Open says so and keeps the window
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&b), "another LightCraft").unwrap();
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert!(h.app.session.library.is_none());
    let p = h.app.library_problem.as_ref().unwrap();
    assert!(p.error.contains("already open in another LightCraft"), "who has it: {}", p.error);
    assert!(p.chooser.as_ref().unwrap().libraries[1].unavailable.is_some(), "greyed out now");
    drop(lock);
    // ↑ to Alpha, Return: it opens
    h.request("ui.key", json!({"key": "ArrowUp"}), T);
    settle(&mut h);
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(a));
    assert!(h.app.library_problem.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Quit and Esc quit without opening anything.
#[test]
fn quit_and_esc_quit_from_the_window() {
    let dir = temp("quit");
    let a = make_library(&dir.join("Alpha"));
    let mut h = asking(vec![a.clone()], &a, None);
    h.request("ui.key", json!({"key": "Escape"}), T);
    settle(&mut h);
    assert!(h.quit_requested(), "Esc quits when no library is open");
    assert!(h.app.session.library.is_none(), "and opens nothing");
    drop(h);

    let mut h = asking(vec![a.clone()], &a, None);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryQuit"}), T)["ok"], true);
    settle(&mut h);
    assert!(h.quit_requested());
    assert!(h.app.session.library.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open Another… runs the folder dialog of `app.openLibrary`, also with an empty list.
#[test]
fn open_another_asks_for_a_folder_and_an_empty_list_still_offers_it() {
    let dir = temp("another");
    let other = make_library(&dir.join("Elsewhere"));
    let mut h = asking(Vec::new(), "", Some(other.clone()));
    assert!(has(&h, "indicator:noRecentLibraries"));
    assert!(!has(&h, "libraryRecent:0"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(other.clone()));
    assert!(h.app.library_problem.is_none());
    assert_eq!(h.app.ui.settings.recent_libraries.first(), Some(&other), "remembered once opened");
    drop(h);

    // without a folder dialog (the browser) Open Another… and New Library… are greyed out
    let mut h = asking(Vec::new(), "", None);
    h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T);
    h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    settle(&mut h);
    assert!(h.app.library_problem.is_some() && h.app.ui.dialog.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// New Library… asks for a name in front of the window; command-line photos go into it.
#[test]
fn new_library_from_the_window_asks_for_a_name_and_the_list_returns_if_cancelled() {
    let dir = temp("newlib");
    let a = make_library(&dir.join("Alpha"));
    let mut h = asking(vec![a.clone()], &a, Some(dir.to_string_lossy().into_owned()));
    // a photo given on the command line waits for the library chosen
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();
    h.app.library_problem.as_mut().unwrap().pending_import = vec![photo.to_string_lossy().into_owned()];
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T)["ok"], true);
    settle(&mut h);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })), "{:?}", h.app.ui.dialog);
    assert!(!has(&h, "button:libraryOpen"), "the name prompt is in front");
    h.request("ui.dialog.cancel", json!({}), T);
    settle(&mut h);
    assert!(has(&h, "button:libraryOpen"), "the list is back");
    // what the prompt's OK runs: the library is made where the folder dialog says, and opens
    h.app.run("app.newLibrary", json!({"name": "Fresh"})).unwrap();
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.clone()), Some(dir.join("Fresh")));
    assert!(h.app.library_problem.is_none());
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the command-line photo is imported into the new library");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Remove from List in the window forgets the selected entry and deletes nothing.
#[test]
fn remove_from_list_forgets_one_library_and_deletes_nothing() {
    let dir = temp("forget");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut h = asking(vec![a.clone(), gone.clone(), b.clone()], &a, None);
    h.app.ui.settings.recent_opened.insert(gone.clone(), "2026-01-01T00:00:00".into());
    // the window: the missing one can be selected and removed
    h.request("ui.clickWidget", json!({"id": "libraryRecent:1"}), T);
    settle(&mut h);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryForget"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.recent_libraries, [a.clone(), b.clone()]);
    assert!(!h.app.ui.settings.recent_opened.contains_key(&gone), "its time goes too");
    let c = h.app.library_problem.as_ref().unwrap().chooser.clone().unwrap();
    assert_eq!(c.libraries.len(), 2);
    assert_eq!(c.selected, Some(1));
    // removing an existing library leaves its folder and catalog alone
    h.request("ui.clickWidget", json!({"id": "button:libraryForget"}), T);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.recent_libraries, vec![a.clone()]);
    assert!(lightcraft_engine::library::is_library(Path::new(&b)), "nothing is deleted on disk");
    settle(&mut h);
    assert!(!has(&h, "libraryRecent:1"));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// File ▸ Open Recent Library's items.
fn open_recent(app: &LightcraftApp) -> Vec<crate::menubar::MenuNode> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    match file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Open Recent Library")) {
        Some(MenuNode::Submenu { children, .. }) => children.clone(),
        _ => panic!("no Open Recent Library submenu in {file:?}"),
    }
}

/// File ▸ Open Recent Library ▸ Remove from List and `app.recentLibraries {remove}`.
#[test]
fn open_recent_library_and_the_command_remove_one_entry() {
    use crate::menubar::MenuNode;
    let dir = temp("menu");
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    let (first, gone) = (dir.join("first").to_string_lossy().into_owned(), dir.join("gone").to_string_lossy().into_owned());
    app.ui.settings.recent_libraries = vec![first.clone(), gone.clone(), "/elsewhere/third".into()];

    let items = open_recent(&app);
    let Some(MenuNode::Submenu { children, .. }) = items.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Remove from List"))
    else {
        panic!("no Remove from List in {items:?}")
    };
    assert_eq!(children.len(), 3, "every entry, missing ones too");
    let MenuNode::Item { id, params, label, enabled, .. } = &children[1] else { panic!("{children:?}") };
    assert_eq!((id.as_str(), params, *enabled), ("app.recentLibraries", &json!({"remove": gone}), true));
    assert!(label.starts_with("gone — "), "{label}");
    assert_eq!(crate::menubar::display_item_label(id, params, label), label, "a path is never translated");
    // what clicking it runs
    let list = app.run(id, params.clone()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(app.ui.settings.recent_libraries, [first.clone(), "/elsewhere/third".to_string()]);

    // the command: the open library can be forgotten too (it stays open), and nothing is deleted
    let list = app.run("app.recentLibraries", json!({"remove": first})).unwrap();
    // (no frame ran: nothing checked yet, so `exists` is not known; the command never looks itself)
    assert_eq!(list, json!([{"path": "/elsewhere/third", "name": "third", "exists": null, "checking": false, "open": false, "lastOpened": null}]));
    assert!(app.session.library.is_some() && lightcraft_engine::library::is_library(&dir.join("first")));
    let e = app.run("app.recentLibraries", json!({"remove": first})).unwrap_err();
    assert!(e.contains("not in the recent libraries"), "{e}");
    let e = app.run("app.recentLibraries", json!({"remove": 3})).unwrap_err();
    assert!(e.contains("remove must be"), "{e}");
    app.run("app.recentLibraries", json!({"remove": "/elsewhere/third"})).unwrap();
    // an empty list has nothing to remove
    let items = open_recent(&app);
    assert!(!items.iter().any(|n| matches!(n, MenuNode::Submenu { .. })), "{items:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `atLaunch` and `recentLibrariesOpened` round-trip, and older or odd files still load.
#[test]
fn the_setting_and_the_open_times_are_kept_in_the_settings_file() {
    // files from earlier versions: no key, the default; a value from a later version: the default
    let old: crate::UiState = serde_json::from_str(r#"{"settings": {"recentLibraries": ["/a"], "startupView": "detail"}}"#).unwrap();
    assert_eq!(old.settings.at_launch, LaunchLibrary::Last);
    assert!(old.settings.recent_opened.is_empty());
    assert_eq!(old.settings.recent_libraries, ["/a"]);
    let odd: crate::UiState =
        serde_json::from_str(r#"{"settings": {"atLaunch": "someday", "recentLibrariesOpened": 7, "startupView": "detail"}}"#).unwrap();
    assert_eq!(odd.settings.at_launch, LaunchLibrary::Last);
    assert!(odd.settings.recent_opened.is_empty());
    assert_eq!(odd.settings.startup_view, crate::state::StartupView::Detail, "the rest of the file still loads");

    let mut ui = crate::UiState::default();
    ui.settings.at_launch = LaunchLibrary::Ask;
    ui.settings.recent_libraries = vec!["/a".into()];
    ui.settings.recent_opened.insert("/a".into(), "2026-10-23T09:00:00".into());
    let saved = serde_json::to_value(&ui).unwrap();
    assert_eq!(saved["settings"]["atLaunch"], "ask");
    assert_eq!(saved["settings"]["recentLibraries"], json!(["/a"]), "the list keeps its old shape");
    assert_eq!(saved["settings"]["recentLibrariesOpened"], json!({"/a": "2026-10-23T09:00:00"}));
    let back: crate::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(back.settings, ui.settings);
}

/// The Settings choice and `ui.set` change the setting; an opened library gets its time.
#[test]
fn settings_choose_ask_me_and_opening_a_library_records_when() {
    let dir = temp("settings");
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    app.services.pick_folder = Some(Box::new(|| None));
    app.ui.settings.recent_opened.insert("/forgotten".into(), "2020-01-01T00:00:00".into());
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    let first = dir.join("first").to_string_lossy().into_owned();
    // the session clock's time (tests: a fixed one), only for listed libraries
    assert_eq!(h.app.ui.settings.recent_opened.get(&first).map(String::as_str), Some("2026-09-30T12:00:00"));
    assert!(!h.app.ui.settings.recent_opened.contains_key("/forgotten"));

    h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}), T);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:settingsAtLaunch-1"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Ask);
    h.request("ui.clickWidget", json!({"id": "button:settingsAtLaunch-0"}), T);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Last);
    // also through ui.set, as scripts change settings
    let r = h.request("ui.set", json!({"settings": {"atLaunch": "ask"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Ask);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The app as the host starts it with *Open the last library*: `first` open, Alt watched.
fn opened_last(dir: &Path) -> (Headless, String, String) {
    let (first, second) = (make_library(&dir.join("first")), make_library(&dir.join("second")));
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&first, false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    app.ui.settings.recent_libraries = vec![first.clone(), second.clone()];
    app.ui.settings.library_path = first.clone();
    app.alt_asks_for_library = true;
    (Headless::new(app, [1300.0, 900.0], 1.0), first, second)
}

/// Alt held as the window appears shows the list once over the open library; Esc keeps it.
#[test]
fn alt_held_as_the_window_appears_asks_once_over_the_last_library() {
    let dir = temp("alt");
    let (mut h, first, _) = opened_last(&dir);
    settle(&mut h);
    assert!(h.app.library_problem.is_none(), "no Alt: the last library, no window");
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).expect("the list shows");
    assert_eq!(c.selected, Some(0));
    assert!(c.over.is_some(), "over the library that opened");
    assert!(c.libraries[0].unavailable.is_none(), "the open library itself can be chosen");
    assert!(has(&h, "button:libraryOpen"));
    // Esc keeps the library that opened
    h.request("ui.key", json!({"key": "Escape"}), T);
    settle(&mut h);
    assert!(!h.quit_requested(), "Esc doesn't quit while a library is open");
    assert!(h.app.library_problem.is_none());
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(first.clone()));
    // once only
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    assert!(h.app.library_problem.is_none());
    drop(h);

    // choosing another library from it switches to that one
    let dir2 = temp("alt-switch");
    let (mut h, _, second2) = opened_last(&dir2);
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    h.request("ui.key", json!({"key": "ArrowDown"}), T);
    settle(&mut h);
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(second2));
    assert!(h.app.library_problem.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// The Alt watch ends after the first seconds and needs the host to arm it.
#[test]
fn alt_is_only_watched_while_the_window_first_appears_and_when_armed() {
    let frame = |app: &mut LightcraftApp, ctx: &egui::Context, time: f64, alt: bool| {
        let modifiers = if alt { egui::Modifiers::ALT } else { egui::Modifiers::NONE };
        let raw = egui::RawInput { time: Some(time), events: vec![egui::Event::ModifiersChanged(modifiers)], ..Default::default() };
        ctx.run_ui(raw, |ui| crate::panels::library_problem::logic(app, ui.ctx())).textures_delta.clear();
    };
    let new_app = |armed| {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.alt_asks_for_library = armed;
        app
    };
    // too late: the watch ends, a later Alt does nothing
    let (mut app, ctx) = (new_app(true), egui::Context::default());
    frame(&mut app, &ctx, ALT_SECONDS + 0.5, false);
    assert!(!app.alt_asks_for_library);
    frame(&mut app, &ctx, ALT_SECONDS + 0.6, true);
    assert!(app.library_problem.is_none());
    // not armed (--library, LIGHTCRAFT_LIBRARY, --memory, the browser)
    let (mut app, ctx) = (new_app(false), egui::Context::default());
    frame(&mut app, &ctx, 0.1, true);
    assert!(app.library_problem.is_none());
    // armed, in time
    let (mut app, ctx) = (new_app(true), egui::Context::default());
    frame(&mut app, &ctx, 0.1, false);
    assert!(app.alt_asks_for_library && app.library_problem.is_none());
    frame(&mut app, &ctx, 0.2, true);
    assert!(app.library_problem.as_ref().is_some_and(|p| p.chooser.is_some()));
    assert!(!app.alt_asks_for_library);
}

/// A check that waits until the test lets it go (a network share whose server is gone), then
/// answers like the real one for a missing folder. `release` lets every waiting check go.
fn slow_checks() -> (
    crate::panels::library_problem::RecentChecks,
    std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (g, n) = (gate.clone(), calls.clone());
    let checks = crate::panels::library_problem::RecentChecks::with_check(move |p| {
        n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (lock, cv) = &*g;
        let open = lock.lock().unwrap();
        // (bounded: a test that forgets to release still ends)
        let _unused = cv.wait_timeout_while(open, Duration::from_secs(10), |open| !*open).unwrap();
        (!Path::new(p).exists()).then_some(crate::panels::library_problem::Unavailable::NotFound)
    });
    (checks, gate, calls)
}

/// Let every check waiting in [`slow_checks`] answer.
fn release(gate: &std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>) {
    let (lock, cv) = &**gate;
    *lock.lock().unwrap() = true;
    cv.notify_all();
}

/// Review of #2701: the launch list never waits on a folder. Its rows show "Checking…" while a
/// worker looks (the activity stack lists it), the window keeps drawing frames meanwhile, and the
/// answers grey out a missing library and move the selection to one that can be opened.
#[test]
fn a_slow_check_never_holds_the_launch_window() {
    let dir = temp("slow");
    let a = make_library(&dir.join("A"));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![gone.clone(), a.clone()];
    app.ui.settings.library_path = gone.clone();
    let (checks, gate, _) = slow_checks();
    app.recent_checks = checks;
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let t0 = std::time::Instant::now();
    settle(&mut h);
    assert!(t0.elapsed() < Duration::from_secs(5), "frames went on while the check waited: {:?}", t0.elapsed());
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert!(c.libraries.iter().all(|l| l.checking && l.unavailable.is_none()), "{c:?}");
    assert_eq!(c.selected, Some(0), "the last library opened, until the answers say otherwise");
    assert!(h.painted_text().iter().any(|t| t == "Checking…"), "{:?}", h.painted_text());
    assert!(h.app.session.activity.list().iter().any(|t| t.label == "Checking recent libraries"), "the activity stack lists the check");

    release(&gate);
    let done =
        h.step_until(T, |h| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| c.libraries.iter().all(|l| !l.checking)));
    assert!(done, "the answers came back");
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert!(c.libraries[0].unavailable.as_deref().is_some_and(|w| w.contains("Not found")), "{c:?}");
    assert_eq!(c.libraries[1].unavailable, None);
    assert_eq!(c.selected, Some(1), "the first library that can be opened, once known");
    assert!(h.app.session.activity.list().iter().all(|t| t.label != "Checking recent libraries"), "the row goes with the check");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Answers that arrive after the person moved the highlight leave it where they put it.
#[test]
fn late_answers_keep_the_row_the_person_chose() {
    let dir = temp("touched");
    let a = make_library(&dir.join("A"));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![a.clone(), gone.clone()];
    app.ui.settings.library_path = a.clone();
    let (checks, gate, _) = slow_checks();
    app.recent_checks = checks;
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    h.request("ui.key", json!({"key": "ArrowDown"}), T);
    settle(&mut h);
    release(&gate);
    h.step_until(T, |h| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| c.libraries.iter().all(|l| !l.checking)));
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert_eq!(c.selected, Some(1), "still the row moved to, though it can't be opened");
    let _ = std::fs::remove_dir_all(&dir);
}

/// File ▸ Open Recent Library is rebuilt every frame: it never looks at a folder itself. A
/// library is greyed out only once a worker found it missing, and each path is checked once.
#[test]
fn the_recent_menu_takes_its_answers_from_a_worker() {
    use crate::menubar::MenuNode;
    let dir = temp("menu-worker");
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![gone.clone()];
    let (checks, gate, calls) = slow_checks();
    app.recent_checks = checks;
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let enabled = |h: &Headless| match open_recent(&h.app).first() {
        Some(MenuNode::Item { enabled, .. }) => *enabled,
        other => panic!("{other:?}"),
    };
    for _ in 0..30 {
        h.step();
        assert!(enabled(&h), "not known yet: kept enabled (opening says why if it can't)");
    }
    release(&gate);
    assert!(h.step_until(T, |h| !h.app.recent_checks.is_running() && h.app.recent_checks.is_known(&gone)), "answered");
    for _ in 0..30 {
        h.step();
    }
    assert!(!enabled(&h), "found missing: greyed out");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "checked once, not every frame");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reasons are put into words on the UI thread, in the interface language (the worker that
/// found them has none of its own).
#[test]
fn the_reasons_are_in_the_interface_language() {
    let dir = temp("lang");
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.language = crate::i18n::Locale::Fr;
    app.ui.settings.recent_libraries = vec![gone.clone()];
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    assert!(
        h.step_until(T, |h| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| c.libraries.iter().all(|l| !l.checking)))
    );
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    let why = c.libraries[0].unavailable.clone().unwrap_or_default();
    let english = "Not found: moved, renamed or on a disconnected drive";
    crate::i18n::set_language(crate::i18n::Locale::Fr);
    assert_eq!(why, crate::i18n::tr(english), "translated on the UI thread");
    assert_ne!(why, english, "the French catalog has this reason");
    crate::i18n::set_language(crate::i18n::Locale::En);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One drive that never answers doesn't hold the others: each path has its own worker, and the
/// row that waits says the drive isn't answering once it takes long.
#[test]
fn one_hung_drive_doesnt_hold_the_other_libraries() {
    let dir = temp("hung");
    let a = make_library(&dir.join("A"));
    let hung = dir.join("Hung").to_string_lossy().into_owned();
    let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let g = gate.clone();
    let h_path = hung.clone();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(move |p| {
        if p == h_path {
            let (lock, cv) = &*g;
            let open = lock.lock().unwrap();
            let _unused = cv.wait_timeout_while(open, Duration::from_secs(10), |open| !*open).unwrap();
        }
        None
    });
    app.ui.settings.recent_libraries = vec![hung.clone(), a.clone()];
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let a_done = h.step_until(T, |h| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| !c.libraries[1].checking));
    assert!(a_done, "the reachable library answered while the other one hangs");
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert!(c.libraries[0].checking, "the hung one is still checking");
    release(&gate);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A check whose worker dies (a panic) ends as "couldn't be checked" instead of leaving the row,
/// and every later check, waiting forever.
#[test]
fn a_check_that_dies_doesnt_leave_the_list_waiting() {
    let dir = temp("dies");
    let a = make_library(&dir.join("A"));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(|_| panic!("a check that dies"));
    app.ui.settings.recent_libraries = vec![a.clone()];
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let done = h.step_until(T, |h| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| !c.libraries[0].checking));
    assert!(done, "the row stopped waiting");
    assert!(!h.app.recent_checks.is_running(), "no check is left running");
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert!(c.libraries[0].unavailable.as_deref().is_some_and(|w| w.contains("couldn't be checked")), "{c:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library found missing comes back once it is opened again (a drive plugged back in): the
/// menu no longer greys it out.
#[test]
fn opening_a_library_clears_an_old_missing_answer() {
    use crate::menubar::MenuNode;
    let dir = temp("back");
    let a = make_library(&dir.join("A"));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![a.clone()];
    app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(|_| Some(crate::panels::library_problem::Unavailable::NotFound));
    let ctx = egui::Context::default();
    crate::panels::library_problem::logic(&mut app, &ctx);
    assert!(crate::tasks::wait(&mut app, &ctx, T), "checked");
    assert_eq!(app.recent_checks.exists(&a), Some(false), "found missing (the drive was disconnected)");
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.app.run("app.openLibrary", json!({"path": a})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(matches!(open_recent(&h.app).first(), Some(MenuNode::Item { enabled: true, .. })), "{:?}", open_recent(&h.app));
    assert_eq!(h.app.recent_checks.reason(&a), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review of #2701: a library that fails to open from the launch window because another program
/// has it is there all the same. The row shows the open's error, but the menu keeps it enabled and
/// `app.recentLibraries` says it exists; an unreadable catalog doesn't say the library is gone
/// either (`exists: null`).
#[test]
fn a_failed_open_or_an_unreadable_catalog_doesnt_mark_a_library_missing() {
    use crate::menubar::MenuNode;
    let dir = temp("held");
    let held = make_library(&dir.join("Held"));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![held.clone()];
    app.ui.settings.library_path = held.clone();
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let answered =
        |h: &Headless| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| c.libraries.iter().all(|l| !l.checking));
    assert!(h.step_until(T, answered));
    assert_eq!(h.app.recent_checks.exists(&held), Some(true));
    // another program has it: Open from the launch window fails
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&held), "another LightCraft").unwrap();
    settle(&mut h);
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryOpen"}), T);
    assert_eq!(r["ok"], true, "{r} {:?}", h.app.widgets.iter().map(|(w, _)| w.clone()).filter(|w| w.contains("library")).collect::<Vec<_>>());
    settle(&mut h);
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).unwrap();
    assert!(c.libraries[0].unavailable.is_some(), "the row shows why it didn't open: {c:?}");
    assert!(h.step_until(T, |h| !h.app.recent_checks.is_running() && h.app.recent_checks.is_known(&held)), "checked again");
    assert_eq!(h.app.recent_checks.exists(&held), Some(true), "a failed open doesn't mark it missing");
    let list = h.app.run("app.recentLibraries", json!({})).unwrap();
    assert_eq!(list[0]["exists"], true, "{list}");
    assert!(matches!(open_recent(&h.app).first(), Some(MenuNode::Item { enabled: true, .. })), "{:?}", open_recent(&h.app));
    drop(lock);

    // an unreadable catalog: not known to be gone
    h.app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(|_| {
        Some(crate::panels::library_problem::Unavailable::CatalogUnreadable("denied".into()))
    });
    assert!(h.step_until(T, |h| h.app.recent_checks.is_known(&held) && !h.app.recent_checks.is_running()));
    assert_eq!(h.app.run("app.recentLibraries", json!({})).unwrap()[0]["exists"], serde_json::Value::Null);
    assert!(matches!(open_recent(&h.app).first(), Some(MenuNode::Item { enabled: true, .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Photos dropped on the launch list wait for the library chosen (CodeRabbit on #2689): before,
/// they went into the session the library then replaced, and were gone from view.
#[test]
fn photos_dropped_on_the_launch_list_go_into_the_library_chosen() {
    let dir = temp("drop");
    let a = make_library(&dir.join("Alpha"));
    let mut h = asking(vec![a.clone()], &a, None);
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();
    // frames with nothing dropped (the drop code runs every frame) hold nothing and say nothing
    settle(&mut h);
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
    let ctx = h.view.ctx.clone();
    assert_eq!(crate::panels::library_problem::hold_dropped(&mut h.app, &ctx, Vec::new()), Some(Vec::new()));
    assert!(h.app.ui.toast.is_none(), "an empty drop says nothing: {:?}", h.app.ui.toast);
    let photos = vec![photo.to_string_lossy().into_owned()];
    assert!(crate::panels::library_problem::holds_drops(&h.app));
    assert_eq!(crate::panels::library_problem::hold_dropped(&mut h.app, &ctx, photos.clone()), None, "held while no library is open");
    assert_eq!(h.app.library_problem.as_ref().map(|p| p.pending_import.clone()), Some(photos.clone()));
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("once a library is open")), "{:?}", h.app.ui.toast);
    assert_eq!(h.app.session.catalog.len(), 0, "nothing goes into the session the library replaces");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryOpen"}), T)["ok"], true);
    settle(&mut h);
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the dropped photo is imported into the library opened");
    // with a library open, a drop is the usual import
    assert!(!crate::panels::library_problem::holds_drops(&h.app));
    assert_eq!(crate::panels::library_problem::hold_dropped(&mut h.app, &ctx, photos.clone()), Some(photos));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A lookup that fails without saying the library is gone (here a parent folder it may not read;
/// a network share that timed out is the same) is "couldn't check", not "not found": the entry
/// isn't greyed out in File ▸ Open Recent Library (CodeRabbit on #2689).
#[cfg(unix)]
#[test]
fn a_failed_lookup_doesnt_mark_a_recent_library_missing() {
    use std::os::unix::fs::PermissionsExt;
    /// Gives the folder its permissions back and removes the test's folder, also when an assertion
    /// fails (a folder left at mode 000 can't be removed).
    struct Cleanup(std::path::PathBuf, std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.1, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = temp("lookup");
    let locked = dir.join("Locked");
    let _cleanup = Cleanup(dir.clone(), locked.clone());
    let inside = make_library(&locked.join("Inside"));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // running as root (some CI containers): the mode bits don't stop it, so no lookup fails
        lightcraft_testkit::skip!(System, "a folder with no permissions can still be read (running as root)");
        return;
    }
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.settings.recent_libraries = vec![inside.clone(), gone.clone()];
    let mut h = Headless::new(app, [900.0, 600.0], 1.0);
    let answered = h.step_until(T, |h| h.app.recent_checks.is_known(&inside) && h.app.recent_checks.is_known(&gone));
    assert!(answered);
    assert_eq!(h.app.recent_checks.exists(&gone), Some(false), "a library that isn't there is missing");
    assert_eq!(h.app.recent_checks.exists(&inside), None, "one that couldn't be looked at isn't");
    let why = h.app.recent_checks.reason(&inside).map(|w| w.text()).unwrap_or_default();
    assert!(why.starts_with("This library couldn't be checked:"), "{why}");
    // nor is a folder that is there but refuses to be listed (permissions, macOS privacy)
    use crate::panels::library_problem::Unavailable;
    assert!(!Unavailable::Unreadable("denied".into()).is_missing() && !Unavailable::LookupFailed("timed out".into()).is_missing());
    assert!(Unavailable::NotFound.is_missing());
    drop(h);
}

/// A library found is asked about again when the window comes back to the front, not on a timer: a
/// timer would wake every external drive and network share in the list over and over. One not
/// found is looked for again on its own (a drive plugged back in with the app in front).
#[test]
fn recent_libraries_are_checked_again_only_when_the_window_comes_back() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = temp("recheck");
    let a = make_library(&dir.join("A"));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let (calls, gone_calls) = (std::sync::Arc::new(AtomicUsize::new(0)), std::sync::Arc::new(AtomicUsize::new(0)));
    let (c, g, gone_path) = (calls.clone(), gone_calls.clone(), gone.clone());
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(move |p| {
        if p == gone_path {
            g.fetch_add(1, Ordering::SeqCst);
            return Some(crate::panels::library_problem::Unavailable::NotFound);
        }
        c.fetch_add(1, Ordering::SeqCst);
        None
    })
    .recheck_after(Duration::ZERO);
    app.ui.settings.recent_libraries = vec![a.clone(), gone.clone()];
    let mut h = Headless::new(app, [900.0, 600.0], 1.0);
    assert!(h.step_until(T, |h| h.app.recent_checks.exists(&a) == Some(true)));
    assert!(h.step_until(T, |_| gone_calls.load(Ordering::SeqCst) >= 3), "the one not found is looked for again on its own");
    // focused all along, every answer "old": nothing is asked again
    for _ in 0..30 {
        h.step();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no check on a timer");
    // the window went to the back and comes to the front again
    h.app.recent_checks.focused = false;
    assert!(h.step_until(T, |_| calls.load(Ordering::SeqCst) == 2), "asked again when the window comes back");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "once");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library the launch list showed as not found comes back to it once found again (a drive plugged
/// back in while the list is up): the row waits for the new check instead of keeping its old answer
/// (FredApps on #2689, #2774).
#[test]
fn a_library_found_again_comes_back_to_the_launch_list() {
    found_again(false);
}

/// The same when every frame is laid out twice (`request_discard`): an answer applied in a frame is
/// visible through all of its passes, and scheduling does not ask about it again in that frame.
#[test]
fn a_library_found_again_comes_back_to_the_launch_list_across_a_multipass_frame() {
    found_again(true);
}

fn found_again(multipass: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = temp(if multipass { "found-again-multipass" } else { "found-again" });
    let a = make_library(&dir.join("A"));
    let plugged = std::sync::Arc::new(AtomicBool::new(false));
    let p = plugged.clone();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.recent_checks = crate::panels::library_problem::RecentChecks::with_check(move |_| {
        (!p.load(Ordering::SeqCst)).then_some(crate::panels::library_problem::Unavailable::NotFound)
    })
    .recheck_after(Duration::ZERO);
    app.ui.settings.recent_libraries = vec![a.clone()];
    app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    if multipass {
        // The existing context hook egui calls at the start of every pass: ask for the second layout
        // pass on every pass 0 (which egui otherwise does only for e.g. a Grid sizing change).
        h.view.ctx.on_begin_pass(
            "test: force a second layout pass",
            std::sync::Arc::new(|ui: &mut egui::Ui| {
                if ui.ctx().current_pass_index() == 0 {
                    ui.ctx().request_discard("test: force a second layout pass");
                }
            }),
        );
        let passes = h.view.ctx.cumulative_pass_nr();
        h.step();
        assert_eq!(h.view.ctx.cumulative_pass_nr() - passes, 2, "the hook forced the second pass");
    }
    let row = |h: &Headless| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).map(|c| c.libraries[0].clone()).unwrap();
    assert!(
        h.step_until(T, |h| {
            let r = row(h);
            !r.checking && r.unavailable.is_some()
        }),
        "not found first"
    );
    plugged.store(true, Ordering::SeqCst);
    assert!(
        h.step_until(T, |h| {
            let r = row(h);
            !r.checking && r.unavailable.is_none()
        }),
        "found again: it can be opened, {:?}",
        row(&h)
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #3239: Try Again on a remembered library that was missing at launch makes no library in
/// its folder when the folder is there but empty (a share's mount point before the share is
/// connected): the window stays and says so. Once the folder holds the library again, it opens.
#[test]
fn try_again_on_a_missing_remembered_library_makes_nothing_in_an_empty_folder() {
    let dir = temp("retry-existing");
    let mount = dir.join("Share");
    std::fs::create_dir_all(&mount).unwrap();
    let path = mount.to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.library_problem = Some(LibraryProblem { existing_only: true, ..LibraryProblem::new(path.clone(), "missing") });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    settle(&mut h);
    assert!(!lightcraft_engine::library::is_library(&mount), "no catalog is made in the empty folder");
    assert_eq!(std::fs::read_dir(&mount).unwrap().count(), 0, "nothing is written there");
    assert!(h.app.session.library.is_none());
    let p = h.app.library_problem.as_ref().expect("the window stays");
    assert_eq!(p.error, "There is no LightCraft library in this folder any more.");
    assert!(has(&h, "button:libraryRetry"));
    // the share is connected again: Try Again opens the library in it
    make_library(&mount);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(path));
    assert!(h.app.library_problem.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #3239: why Try Again didn't open a remembered library is in the interface language,
/// both when its folder is empty and when the folder itself is gone.
#[test]
fn try_again_says_why_the_remembered_library_is_missing_in_the_interface_language() {
    let dir = temp("retry-lang");
    let mount = dir.join("Share");
    std::fs::create_dir_all(&mount).unwrap();
    let path = mount.to_string_lossy().into_owned();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.ui.language = crate::i18n::Locale::De;
    app.library_problem = Some(LibraryProblem { existing_only: true, ..LibraryProblem::new(path, "missing") });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    settle(&mut h);
    let error = h.app.library_problem.as_ref().map(|p| p.error.clone()).unwrap_or_default();
    assert_eq!(error, "In diesem Ordner ist keine LightCraft-Bibliothek mehr.");
    std::fs::remove_dir(&mount).unwrap();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    settle(&mut h);
    let error = h.app.library_problem.as_ref().map(|p| p.error.clone()).unwrap_or_default();
    crate::i18n::set_language(crate::i18n::Locale::En);
    assert!(error.starts_with("Die Bibliothek ist nicht da:"), "{error}");
    assert!(!mount.exists(), "nothing is made where it was");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #3239: a remembered library whose folder can't be looked into (no permission) isn't
/// reported as gone: Try Again says the folder can't be read, and nothing is written there.
#[cfg(unix)]
#[test]
fn try_again_on_a_remembered_library_it_may_not_read_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp("retry-denied");
    let locked = dir.join("Locked");
    let path = make_library(&locked);
    let names = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name()).collect();
        names.sort();
        names
    };
    let before = names(&locked);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&locked).is_ok();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    app.library_problem = Some(LibraryProblem { existing_only: true, ..LibraryProblem::new(path, "missing") });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    if !readable {
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
        settle(&mut h);
    }
    let error = h.app.library_problem.as_ref().map(|p| p.error.clone()).unwrap_or_default();
    let library = h.app.session.library.is_some();
    drop(h);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        let _ = std::fs::remove_dir_all(&dir);
        lightcraft_testkit::skip!(System, "running as root: a folder with mode 000 is still readable");
        return;
    }
    assert!(error.starts_with("This folder can't be read:"), "{error}");
    assert!(error.to_lowercase().contains("permission denied"), "{error}");
    assert!(!library, "nothing opened");
    assert_eq!(names(&locked), before, "nothing written there");
    let _ = std::fs::remove_dir_all(&dir);
}
