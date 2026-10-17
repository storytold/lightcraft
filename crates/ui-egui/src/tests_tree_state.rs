//! The left sidebar's trees open as they were left: what was opened or folded in Albums, By
//! Date, Keywords, Local and Folders is kept with the settings and is there again after a restart.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::UiState;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(20);

/// The demo library with a keyword inside another, in a window tall enough for every section.
fn app(ui: Option<UiState>) -> Headless {
    app_showing(ui, |_| {})
}

/// The same, with `show` run before the first frame: what the library has the grid show as the
/// app starts.
fn app_showing(ui: Option<UiState>, show: impl FnOnce(&mut LightcraftApp)) -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    if let Some(ui) = ui {
        app.ui = ui;
    }
    show(&mut app);
    let mut h = Headless::new(app, [1400.0, 2600.0], 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    // `show` may leave the grid on a folder with no photos: no photo to give the keyword to (a
    // command given no photos is refused)
    if !ids.is_empty() {
        let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["travel|Italy"]}}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

/// The settings the app would have written, read back: what `ui.json` holds.
fn settings(h: &Headless) -> UiState {
    serde_json::from_str(&serde_json::to_string(&h.app.ui).unwrap()).unwrap()
}

/// The app started again with the settings it would have written.
fn restarted(h: &Headless) -> Headless {
    app(Some(settings(h)))
}

/// A library of file-backed photos that exist only in the catalog (`paths`), started with the
/// settings `ui` and, when `shown` is given, with the grid on that folder of the library.
fn folders_app(paths: &[&str], ui: Option<UiState>, shown: Option<&str>) -> Headless {
    let mut app = LightcraftApp::new(folders_library(paths, shown), Services { png: None, ..Default::default() });
    if let Some(ui) = ui {
        app.ui = ui;
    }
    let mut h = Headless::new(app, [1400.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// A library as it is when it has just been opened: its photos, and the folder its view was
/// left on.
fn folders_library(paths: &[&str], shown: Option<&str>) -> lightcraft_engine::Session {
    let mut session = lightcraft_engine::Session::new();
    for path in paths {
        add_photo(&mut session, path);
    }
    if let Some(folder) = shown {
        show_folder(&mut session, folder);
    }
    session
}

fn add_photo(session: &mut lightcraft_engine::Session, path: &str) {
    use lightcraft_catalog::{Op, Photo, Source};
    let id = session.catalog.alloc_photo_id();
    let photo = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
}

fn show_folder(session: &mut lightcraft_engine::Session, folder: &str) {
    session.source = lightcraft_engine::LibrarySource::LibraryFolder;
    session.library_folder = Some(folder.into());
}

fn folder_row(path: &str) -> String {
    format!("source:libfolder:{path}")
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn shown(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn written(h: &Headless) -> Value {
    serde_json::to_value(&h.app.ui).unwrap()
}

/// Given the sidebar as it starts (Albums' folders open, the dates and keywords folded), when a
/// folder of albums is folded and a year and a keyword are opened, and the app is started again
/// with its settings, then each is as it was left; the settings hold those three rows, in the
/// order they were changed, and no others. Put back as they start, they are kept as that.
#[test]
fn the_sidebars_trees_open_as_they_were_left() {
    let mut h = app(None);
    let folder = h.app.session.catalog.albums().find(|a| a.folder).map(|a| a.id).expect("the demo has a folder of albums");
    let inside = h.app.session.catalog.albums().find(|a| a.parent == Some(folder)).map(|a| a.id.0).expect("with an album in it");
    let (album, month, keyword) = (format!("source:album:{inside}"), "source:date:2026-03".to_string(), "source:keyword:travel|Italy".to_string());
    let (album_toggle, year_toggle, keyword_toggle) = (format!("albumToggle:{}", folder.0), "dateToggle:2026", "keywordToggle:travel");
    assert!(shown(&h, &album) && !shown(&h, &month) && !shown(&h, &keyword), "as the trees start");
    assert!(written(&h).get("sidebarTrees").is_none(), "nothing to keep yet");

    click(&mut h, &album_toggle);
    click(&mut h, year_toggle);
    click(&mut h, keyword_toggle);
    assert!(!shown(&h, &album) && shown(&h, &month) && shown(&h, &keyword));
    assert_eq!(written(&h)["sidebarTrees"], json!([[format!("album:{}", folder.0), false], ["date:2026", true], ["keyword:travel", true]]));

    // started again: as it was left
    let mut h = restarted(&h);
    assert!(!shown(&h, &album) && shown(&h, &month) && shown(&h, &keyword), "after a restart");
    // and the other rows as they start: a month is folded, the photos' other folders of albums open
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("source:date:2026-03-")), "the month is folded");

    // put back as they start: the same three rows, and a restart shows them so
    click(&mut h, &album_toggle);
    click(&mut h, year_toggle);
    click(&mut h, keyword_toggle);
    assert_eq!(written(&h)["sidebarTrees"], json!([[format!("album:{}", folder.0), true], ["date:2026", false], ["keyword:travel", false]]));
    let h = restarted(&h);
    assert!(shown(&h, &album) && !shown(&h, &month) && !shown(&h, &keyword));
}

/// Given a folder on disk listed in Local, when it is opened and the app is started again, then
/// it is open; folded again and restarted, it is folded.
#[test]
fn a_local_folder_opens_as_it_was_left() {
    /// Removes the folders however the test ends.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = std::env::temp_dir().join(format!("lc-ui-trees-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.clone());
    std::fs::create_dir_all(base.join("Sub")).unwrap();
    let (place, sub) = (base.to_string_lossy().to_string(), base.join("Sub").to_string_lossy().to_string());
    let (toggle, row) = (format!("folderToggle:{place}"), format!("source:local:{sub}"));

    let mut h = app(None);
    h.hide_home_above(&base);
    let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": place}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| shown(h, &toggle));
    assert!(!shown(&h, &row), "folded as it starts");
    click(&mut h, &toggle);
    h.step_until(SETTLE, |h| shown(h, &row));
    assert!(shown(&h, &row));

    let mut h = restarted(&h);
    h.hide_home_above(&base);
    h.step_until(SETTLE, |h| shown(h, &row));
    assert!(shown(&h, &row), "open after a restart");
    click(&mut h, &toggle);
    assert!(!shown(&h, &row));
    let mut h = restarted(&h);
    h.hide_home_above(&base);
    h.step_until(SETTLE, |h| shown(h, &toggle));
    assert!(!shown(&h, &row), "folded after a restart");
}

/// Given a library whose folders nest, when a folder of the Folders tree is opened by hand and
/// the app is started again, then it is open; folded and started again, it is folded.
#[test]
fn a_folder_of_the_library_opens_as_it_was_left() {
    let paths = ["/pics/trip/1.jpg", "/pics/trip/day1/2.jpg", "/pics/home/3.jpg"];
    let mut h = folders_app(&paths, None, None);
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(shown(&h, &folder_row("/pics/trip")) && !shown(&h, &folder_row("/pics/trip/day1")), "as the tree starts");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(shown(&h, &folder_row("/pics/trip/day1")));

    let mut h = folders_app(&paths, Some(settings(&h)), None);
    assert!(shown(&h, &folder_row("/pics/trip/day1")), "open after a restart");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    click(&mut h, "libraryFolderToggle:/");
    let h = folders_app(&paths, Some(settings(&h)), None);
    assert!(shown(&h, "libraryFolderToggle:/") && !shown(&h, &folder_row("/pics")), "the disk is folded after a restart");
}

/// Given the grid on a folder whose year holds one month (so the year starts open, and showing
/// the folder changed nothing to see of it), when photos of a second month are imported (the
/// year would now start folded), then the year is still open and the shown folder still on
/// screen: a row opened to show where the grid is stays open.
#[test]
fn the_shown_folder_stays_on_screen_when_a_folder_appears_beside_the_ones_above_it() {
    let mut h = folders_app(&["/photos/2026/05/trip/1.jpg", "/photos/2026/05/other/2.jpg"], None, None);
    show_folder(&mut h.app.session, "/photos/2026/05/trip");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/photos/2026/05/trip")), "revealed");
    add_photo(&mut h.app.session, "/photos/2026/06/more/3.jpg");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/photos/2026/06")), "the new month is listed");
    assert!(shown(&h, &folder_row("/photos/2026/05/trip")), "and the shown folder is still on screen");
    // and after a restart
    let h = folders_app(
        &["/photos/2026/05/trip/1.jpg", "/photos/2026/05/other/2.jpg", "/photos/2026/06/more/3.jpg"],
        Some(settings(&h)),
        Some("/photos/2026/05/trip"),
    );
    assert!(shown(&h, &folder_row("/photos/2026/05/trip")));
}

/// Given the grid on a folder of the library and a folder above it folded by hand, when the app
/// is started again with the grid still on that folder, then the folder above is still folded:
/// showing where the grid is does not undo what was left. Choosing another folder inside it
/// opens it, as that always has.
#[test]
fn a_folder_folded_above_the_shown_one_is_still_folded_after_a_restart() {
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let mut h = folders_app(&paths, None, None);
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "revealed");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(!shown(&h, &folder_row("/pics/trip/raw/x")), "folded by hand: it sticks");

    let mut h = folders_app(&paths, Some(settings(&h)), Some("/pics/trip/raw/x"));
    assert!(shown(&h, &folder_row("/pics/trip")) && !shown(&h, &folder_row("/pics/trip/raw/x")), "still folded after a restart");
    assert!(!h.app.ui.sidebar_trees.is_open(&crate::tree_state::Branch::folder("/pics/trip", false)));
    // another folder is chosen: the rows above it open
    show_folder(&mut h.app.session, "/pics/trip/raw/y");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/y")));

    // a library that had nothing folded: the shown folder is revealed as the app starts
    let h = folders_app(&paths, None, Some("/pics/trip/raw/x"));
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "nothing was left folded: revealed at the start");
}

/// Given a folder on disk browsed in Local and a folder above it folded by hand, when the app
/// is started again still browsing that folder, then the folder above is still folded.
#[test]
fn a_local_folder_folded_above_the_browsed_one_is_still_folded_after_a_restart() {
    /// Removes the folders however the test ends.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = std::env::temp_dir().join(format!("lc-ui-trees-above-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.clone());
    std::fs::create_dir_all(base.join("Above").join("Browsed")).unwrap();
    std::fs::create_dir_all(base.join("Above").join("Other")).unwrap();
    let text = |p: std::path::PathBuf| p.to_string_lossy().to_string();
    let (place, above, browsed) = (text(base.clone()), text(base.join("Above")), text(base.join("Above").join("Browsed")));
    let (toggle, row) = (format!("folderToggle:{above}"), format!("source:local:{browsed}"));
    let above_row = crate::tree_state::Branch::local(&place, &above);

    let mut h = app(None);
    h.hide_home_above(&base);
    for (command, params) in [("local.addRoot", json!({"path": place})), ("library.browse", json!({"path": browsed}))] {
        let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step_until(SETTLE, |h| shown(h, &row));
    assert!(shown(&h, &row) && h.app.ui.sidebar_trees.is_open(&above_row), "the folders above the browsed one open");
    click(&mut h, &toggle);
    assert!(!shown(&h, &row) && !h.app.ui.sidebar_trees.is_open(&above_row), "folded by hand: it sticks");

    let browse = browsed.clone();
    let mut h = app_showing(Some(settings(&h)), move |app| app.run("library.browse", json!({"path": browse})).map(|_| ()).unwrap());
    h.hide_home_above(&base);
    h.step_until(SETTLE, |h| shown(h, &toggle));
    h.settle(SETTLE);
    assert!(shown(&h, &toggle), "the folder above is listed");
    assert!(!h.app.ui.sidebar_trees.is_open(&above_row) && !shown(&h, &row), "and still folded after a restart");

    // the browsed folder is left for one outside (the place itself) and chosen again: that is a
    // choice, and the folder above opens, as in Folders
    for path in [&place, &browsed] {
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": path}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    h.step_until(SETTLE, |h| shown(h, &row));
    assert!(h.app.ui.sidebar_trees.is_open(&above_row) && shown(&h, &row), "chosen again: on screen");

    // folded once more, and another folder inside it is browsed: the folder above opens
    click(&mut h, &toggle);
    assert!(!h.app.ui.sidebar_trees.is_open(&above_row));
    let other = text(base.join("Above").join("Other"));
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": other}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step_until(SETTLE, |h| shown(h, &format!("source:local:{other}")));
    assert!(h.app.ui.sidebar_trees.is_open(&above_row) && shown(&h, &format!("source:local:{other}")));
}

/// The trees' rows over the control channel: `ui.set` takes a list of rows, and refuses anything
/// else with what the list looks like, leaving the trees as they were. Setting something else
/// keeps the rows and their order.
#[test]
fn a_script_sets_the_trees_rows_and_is_told_when_they_are_not_rows() {
    let mut h = app(None);
    let r = h.request("ui.set", json!({"sidebarTrees": [["date:2026", true], ["keyword:travel", true]]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(shown(&h, "source:date:2026-03") && shown(&h, "source:keyword:travel|Italy"));
    for odd in [json!(null), json!(5), json!({"date:2026": false}), json!([["date:2026", "folded"]]), json!([["dates:2026", true]])] {
        let r = h.request("ui.set", json!({"sidebarTrees": odd}), T);
        assert_eq!(r["ok"], false, "{odd}: {r}");
        assert!(r["error"].as_str().is_some_and(|e| e.contains("`sidebarTrees` is a list of rows")), "{odd}: {r}");
    }
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(written(&h)["sidebarTrees"], json!([["date:2026", true], ["keyword:travel", true]]));
    h.settle(SETTLE);
    assert!(shown(&h, "source:date:2026-03") && shown(&h, "source:keyword:travel|Italy"), "still open");
}

/// A settings file whose trees are not a list of rows is read all the same, with its other
/// settings: the trees are then as they start.
#[test]
fn trees_that_are_no_rows_never_cost_the_other_settings() {
    for odd in [json!(null), json!(5), json!("open"), json!({"album:1": false}), json!([5, ["album:1"], ["date:2026", true]])] {
        let mut file = serde_json::to_value(UiState::default()).unwrap();
        file["leftPanel"] = json!(false);
        file["sidebarTrees"] = odd.clone();
        let read: UiState = serde_json::from_value(file).unwrap_or_else(|e| panic!("{odd}: {e}"));
        assert_eq!(serde_json::to_value(&read).unwrap()["leftPanel"], json!(false), "{odd}");
        assert!(read.sidebar_trees.len() <= 1, "{odd}");
    }
    let mut file = serde_json::to_value(UiState::default()).unwrap();
    file.as_object_mut().unwrap().remove("sidebarTrees");
    assert!(serde_json::from_value::<UiState>(file).unwrap().sidebar_trees.is_empty(), "settings from before");
}

/// Given a folder of the library chosen while the app runs, when the rows above it open to show
/// it, then the settings keep nothing of that: they hold what was opened or folded by hand.
/// Settings set over the control channel meanwhile leave those rows open; after a restart on
/// another source they are as they start.
#[test]
fn rows_that_open_to_show_the_grid_are_not_kept() {
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let mut h = folders_app(&paths, None, None);
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "revealed");
    assert!(written(&h).get("sidebarTrees").is_none(), "{}", written(&h)["sidebarTrees"]);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "still on screen after settings were set");

    let h = folders_app(&paths, Some(settings(&h)), None);
    assert!(!shown(&h, &folder_row("/pics/trip")), "as the tree starts after a restart on All Photos");
}

/// Given a folder folded by hand above the one the grid shows, when another library is opened
/// and then this one again (it opens on the folder it was left on), then the folder is still
/// folded: opening a library is no choice of a folder, any more than starting the app is.
#[test]
fn a_folder_folded_above_the_shown_one_is_still_folded_when_its_library_is_opened_again() {
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let mut h = folders_app(&paths, None, None);
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert!(!shown(&h, &folder_row("/pics/trip/raw/x")), "folded by hand");

    h.app.session = folders_library(&["/other/a/1.jpg", "/other/b/2.jpg"], Some("/other/a"));
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/other/a")), "the other library, on its folder");
    h.app.session = folders_library(&paths, Some("/pics/trip/raw/x"));
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip")) && !shown(&h, &folder_row("/pics/trip/raw/x")), "still folded");
    assert_eq!(written(&h)["sidebarTrees"], json!([["folder:/pics/trip", false]]));
}

/// Given a folder folded by hand and the sidebar hidden as the app starts, when a folder inside
/// it is chosen while the sidebar is hidden and the sidebar is then shown, then the rows above
/// the chosen folder are open: it was chosen, whether or not the sidebar saw it. The same of a
/// folder the app started on, left and chosen again.
#[test]
fn a_folder_chosen_while_the_sidebar_is_hidden_is_shown_when_it_comes_back() {
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let mut h = folders_app(&paths, None, None);
    click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    assert_eq!(written(&h)["sidebarTrees"], json!([["folder:/pics", true], ["folder:/pics/trip", false]]));
    let left = settings(&h);

    // hidden as the app starts, on All Photos
    let mut ui = left.clone();
    ui.left_panel = false;
    let mut app = LightcraftApp::new(folders_library(&paths, None), Services { png: None, ..Default::default() });
    app.ui = ui;
    let mut h = Headless::new(app, [1400.0, 1600.0], 1.0);
    h.settle(SETTLE);
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "chosen while the sidebar was hidden: on screen when it is shown");

    // started on x (the folder above it stays folded), left for y, and chosen again
    let mut h = folders_app(&paths, Some(left), Some("/pics/trip/raw/x"));
    assert!(!shown(&h, &folder_row("/pics/trip/raw/x")), "as it was left");
    show_folder(&mut h.app.session, "/pics/home");
    h.settle(SETTLE);
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "chosen again: on screen");
}

/// Given a library that opens on All Photos while it still holds the folder it last showed, and
/// a folder folded by hand above that one, when the folder is chosen, then the rows above it
/// open: only what the grid shows as the library opens is no choice.
#[test]
fn a_folder_the_library_last_showed_is_a_choice_when_it_is_chosen() {
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let mut h = folders_app(&paths, None, None);
    click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    click(&mut h, "libraryFolderToggle:/pics/trip");
    let left = settings(&h);

    let mut library = folders_library(&paths, None);
    library.library_folder = Some("/pics/trip/raw/x".into());
    let mut app = LightcraftApp::new(library, Services { png: None, ..Default::default() });
    app.ui = left;
    let mut h = Headless::new(app, [1400.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!shown(&h, &folder_row("/pics/trip/raw/x")), "All Photos: nothing to show, the fold as it was left");
    show_folder(&mut h.app.session, "/pics/trip/raw/x");
    h.settle(SETTLE);
    assert!(shown(&h, &folder_row("/pics/trip/raw/x")), "chosen: on screen");
}

/// Given a folder of albums folded by hand, when an album is made inside it, then the folder
/// opens to show the new album, and is kept as open (it was somebody's fold, and is open now).
#[test]
fn a_folder_of_albums_opens_for_an_album_made_inside_it() {
    let mut h = app(None);
    let folder = h.app.session.catalog.albums().find(|a| a.folder).map(|a| a.id).expect("the demo has a folder of albums");
    click(&mut h, &format!("albumToggle:{}", folder.0));
    assert_eq!(written(&h)["sidebarTrees"], json!([[format!("album:{}", folder.0), false]]));
    let r = h.request("engine.execute", json!({"command": "album.create", "params": {"name": "Weddings", "parent": folder.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let made = r["result"]["id"].as_u64().expect("the new album's id");
    h.app.ui.reveal_album = Some(made);
    h.settle(SETTLE);
    assert!(shown(&h, &format!("source:album:{made}")), "the new album is on screen");
    assert_eq!(written(&h)["sidebarTrees"], json!([[format!("album:{}", folder.0), true]]));
}
