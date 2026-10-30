//! Option-click (Alt-click) on the triangle of a row in the left sidebar's trees opens or folds
//! the row and every row inside it; a plain click still opens or folds the one row.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::tree_state::{Branch, MAX_ROWS, TreeState};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(20);

fn headless(session: lightcraft_engine::Session) -> Headless {
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 2600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn demo() -> Headless {
    headless(lightcraft_engine::Session::with_demo())
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

/// A click with Option (Alt) held.
fn option_click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id, "alt": true}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn shown(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Given folders of albums one inside another, when the outer one's triangle is clicked with
/// Option held, then it and the folders inside it fold together, and open together at the next
/// such click; a plain click folds and opens the outer one alone.
#[test]
fn option_click_opens_and_folds_the_folders_of_albums_inside() {
    let mut h = demo();
    let make = |h: &mut Headless, params: serde_json::Value| h.app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let trips = make(&mut h, json!({"name": "Trips", "folder": true}));
    let europe = make(&mut h, json!({"name": "Europe", "folder": true, "parent": trips}));
    let north = make(&mut h, json!({"name": "North", "folder": true, "parent": europe}));
    let best = make(&mut h, json!({"name": "Best", "parent": north}));
    let empty = make(&mut h, json!({"name": "Empty", "folder": true, "parent": trips}));
    // (all three open, however folders of albums start)
    for folder in [trips, europe, north] {
        h.app.ui.sidebar_trees.set(&Branch::album(folder), true);
    }
    h.settle(SETTLE);
    let (toggle, inner, album) = (format!("albumToggle:{trips}"), format!("source:folder:{europe}"), format!("source:album:{best}"));
    assert!(shown(&h, &album));

    option_click(&mut h, &toggle);
    assert!(!shown(&h, &inner) && !shown(&h, &album));
    // a plain click opens the outer folder alone: what is inside it is folded with it
    click(&mut h, &toggle);
    assert!(shown(&h, &inner) && !shown(&h, &format!("source:folder:{north}")) && !shown(&h, &album));
    // a folder inside it opened by hand afterwards is open; the one inside that is still folded
    click(&mut h, &format!("albumToggle:{europe}"));
    assert!(shown(&h, &format!("source:folder:{north}")) && !shown(&h, &album));
    click(&mut h, &toggle);

    option_click(&mut h, &toggle);
    assert!(shown(&h, &inner) && shown(&h, &album), "opened with every folder inside it");
    // kept as two things said, of the folder and of what is inside it, however many folders
    // that is; the rows said by hand before are kept beside them, and no longer hold
    let written = serde_json::to_string(&h.app.ui.sidebar_trees).unwrap();
    assert!(written.ends_with(&format!(r#"["in:album:{trips}",true],["album:{trips}",true]]"#)), "{written}");
    assert!(!written.contains(&format!("\"album:{empty}\"")), "nothing is said of each folder inside: {written}");
    // a folder made inside it later opens with the rest
    let late = make(&mut h, json!({"name": "Late", "folder": true, "parent": europe}));
    let in_it = make(&mut h, json!({"name": "In it", "parent": late}));
    h.app.ui.sidebar_trees.set(&Branch::album(late), false);
    option_click(&mut h, &toggle);
    option_click(&mut h, &toggle);
    assert!(shown(&h, &format!("source:album:{in_it}")), "said of what is inside after the fold by hand");

    // started again with those settings: as it was left
    let kept: crate::state::UiState = serde_json::from_str(&serde_json::to_string(&h.app.ui).unwrap()).unwrap();
    let catalog = h.app.session.catalog.clone();
    let mut session = lightcraft_engine::Session::with_demo();
    session.catalog = catalog;
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    app.ui = kept;
    let mut h = Headless::new(app, [1400.0, 2600.0], 1.0);
    h.settle(SETTLE);
    assert!(shown(&h, &inner) && shown(&h, &album) && shown(&h, &format!("source:album:{in_it}")), "after a restart");
}

/// Given a library whose folders nest, when a folder's triangle in Folders is clicked with
/// Option held, then every folder inside it opens down to the last, and folds with it again.
#[test]
fn option_click_opens_and_folds_the_library_folders_inside() {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new();
    for path in ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"] {
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut h = headless(session);
    let row = |path: &str| format!("source:libfolder:{path}");
    assert!(!shown(&h, &row("/pics/trip")), "folded as it starts");

    option_click(&mut h, "libraryFolderToggle:/pics");
    assert!(
        shown(&h, &row("/pics/trip")) && shown(&h, &row("/pics/trip/raw")) && shown(&h, &row("/pics/trip/raw/x")) && shown(&h, &row("/pics/home"))
    );
    option_click(&mut h, "libraryFolderToggle:/pics");
    assert!(!shown(&h, &row("/pics/trip")));
    // a plain click: the folder alone, and what is inside it folded as the Option-click left it
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(shown(&h, &row("/pics/trip")) && !shown(&h, &row("/pics/trip/raw")));
    // opened with what is inside, and kept like rows opened by hand
    click(&mut h, "libraryFolderToggle:/pics");
    option_click(&mut h, "libraryFolderToggle:/pics");
    assert!(shown(&h, &row("/pics/trip/raw/x")));
    // two things are kept: of the folder, and of what is inside it
    let key = lightcraft_catalog::query::folder_key("/pics");
    assert_eq!(serde_json::to_value(&h.app.ui.sidebar_trees).unwrap(), json!([[format!("in:folder:{key}"), true], [format!("folder:{key}"), true]]));
}

/// Given a folder of the library folded with all that is inside it, and opened again alone,
/// when a folder inside it is chosen, then the rows above that one open to show it, for good:
/// the app started again on that folder shows it. Folded with all that is inside it while the
/// grid shows a folder inside, the branch stays folded when the app starts again on that folder.
#[test]
fn a_folder_chosen_inside_a_folded_branch_is_shown_and_stays_shown() {
    use lightcraft_catalog::{Op, Photo, Source};
    let paths = ["/pics/trip/raw/x/1.jpg", "/pics/trip/raw/y/2.jpg", "/pics/trip/3.jpg", "/pics/home/4.jpg"];
    let library = |shown: Option<&str>| {
        let mut session = lightcraft_engine::Session::new();
        for path in paths {
            let id = session.catalog.alloc_photo_id();
            let photo = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        if let Some(folder) = shown {
            session.source = lightcraft_engine::LibrarySource::LibraryFolder;
            session.library_folder = Some(folder.into());
        }
        session
    };
    let started = |ui: crate::state::UiState, shown: Option<&str>| {
        let mut app = LightcraftApp::new(library(shown), Services { png: None, ..Default::default() });
        app.ui = ui;
        let mut h = Headless::new(app, [1400.0, 2600.0], 1.0);
        h.settle(SETTLE);
        h
    };
    let settings = |h: &Headless| -> crate::state::UiState { serde_json::from_str(&serde_json::to_string(&h.app.ui).unwrap()).unwrap() };
    let row = |path: &str| format!("source:libfolder:{path}");
    let choose = |h: &mut Headless, folder: &str| {
        h.app.session.source = lightcraft_engine::LibrarySource::LibraryFolder;
        h.app.session.library_folder = Some(folder.into());
        h.settle(SETTLE);
    };

    let mut h = headless(library(None));
    option_click(&mut h, "libraryFolderToggle:/pics");
    option_click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(shown(&h, &row("/pics/trip")) && !shown(&h, &row("/pics/trip/raw")), "folded with what is inside, and opened alone");
    choose(&mut h, "/pics/trip/raw/x");
    assert!(shown(&h, &row("/pics/trip/raw/x")), "chosen: the rows above it open");
    let h = started(settings(&h), Some("/pics/trip/raw/x"));
    assert!(shown(&h, &row("/pics/trip/raw/x")), "and it is on screen when the app starts on it again");

    // folded with all that is inside it while the grid is on a folder inside
    let mut h = headless(library(None));
    click(&mut h, "libraryFolderToggle:/pics");
    choose(&mut h, "/pics/trip/raw/y");
    assert!(shown(&h, &row("/pics/trip/raw/y")));
    option_click(&mut h, "libraryFolderToggle:/pics");
    click(&mut h, "libraryFolderToggle:/pics");
    assert!(shown(&h, &row("/pics/trip")) && !shown(&h, &row("/pics/trip/raw")));
    let h = started(settings(&h), Some("/pics/trip/raw/y"));
    assert!(shown(&h, &row("/pics/trip")) && !shown(&h, &row("/pics/trip/raw")), "still folded when the app starts on that folder");
}

/// Given a disk in Folders whose first folders only lead to the one where the library branches
/// (rows that open by themselves), when the disk is folded with Option held, then everything
/// inside it folds, those rows too; opened with a plain click it shows its first row only, and
/// opened again with Option it opens down to the last folder, through the rows that lead on.
#[test]
fn option_click_folds_and_opens_the_folders_that_lead_on() {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new();
    for path in ["/Users/me/Pictures/a/x/1.jpg", "/Users/me/Pictures/a/y/2.jpg", "/Users/me/Pictures/b/3.jpg"] {
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut h = headless(session);
    assert!(
        shown(&h, "source:libfolder:/Users") && shown(&h, "source:libfolder:/Users/me") && shown(&h, "source:libfolder:/Users/me/Pictures"),
        "the folders that lead on open by themselves"
    );
    click(&mut h, "libraryFolderToggle:/Users/me/Pictures");
    click(&mut h, "libraryFolderToggle:/Users/me/Pictures/a");
    assert!(shown(&h, "source:libfolder:/Users/me/Pictures/a/x"));

    option_click(&mut h, "libraryFolderToggle:/");
    assert!(!shown(&h, "source:libfolder:/Users"), "folded with all that is inside it");
    click(&mut h, "libraryFolderToggle:/");
    assert!(
        shown(&h, "source:libfolder:/Users") && !shown(&h, "source:libfolder:/Users/me"),
        "opened alone: the folders inside it stay folded, those that lead on too"
    );
    option_click(&mut h, "libraryFolderToggle:/");
    option_click(&mut h, "libraryFolderToggle:/");
    assert!(shown(&h, "source:libfolder:/Users/me/Pictures/a/x"), "opened with all that is inside it, through the folders that lead on");
}

/// Given keywords one inside another and the photos' dates, when a keyword's or a year's
/// triangle is clicked with Option held, then the keywords inside it open down to the last, and
/// the year's months open down to their days.
#[test]
fn option_click_opens_the_keywords_and_the_months_inside() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
    let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["travel|Italy|Rome|Trevi"]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let (rome, trevi, day) = ("source:keyword:travel|Italy|Rome", "source:keyword:travel|Italy|Rome|Trevi", "source:date:2026-03-");
    let a_day = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w.starts_with(day));
    assert!(!shown(&h, "source:keyword:travel|Italy") && !shown(&h, "source:date:2026-03"), "folded as they start");

    option_click(&mut h, "keywordToggle:travel");
    assert!(shown(&h, "source:keyword:travel|Italy") && shown(&h, rome) && shown(&h, trevi), "down to the last keyword");
    option_click(&mut h, "dateToggle:2026");
    assert!(shown(&h, "source:date:2026-03") && a_day(&h), "the months are open down to their days");
    // kept: of each row, and of what is inside it
    assert_eq!(
        serde_json::to_value(&h.app.ui.sidebar_trees).unwrap(),
        json!([["in:keyword:travel", true], ["keyword:travel", true], ["in:date:2026", true], ["date:2026", true]])
    );
    // a row whose rows inside have nothing to open (a month's days, a keyword's last keywords)
    // is said alone
    option_click(&mut h, "dateToggle:2026-03");
    option_click(&mut h, "keywordToggle:travel|Italy|Rome");
    let written = serde_json::to_string(&h.app.ui.sidebar_trees).unwrap();
    assert!(written.contains(r#"["date:2026-03",false]"#) && written.contains(r#"["keyword:travel|italy|rome",false]"#), "{written}");
    assert!(!written.contains("in:date:2026-03") && !written.contains("in:keyword:travel|italy|rome"), "{written}");
    option_click(&mut h, "dateToggle:2026-03");
    option_click(&mut h, "keywordToggle:travel|Italy|Rome");

    option_click(&mut h, "keywordToggle:travel");
    option_click(&mut h, "dateToggle:2026");
    click(&mut h, "keywordToggle:travel");
    click(&mut h, "dateToggle:2026");
    assert!(shown(&h, "source:keyword:travel|Italy") && !shown(&h, rome), "folded with the keyword it is in");
    assert!(shown(&h, "source:date:2026-03") && !a_day(&h), "the months folded with their year");
}

/// Given folders on disk opened one inside another in Local, when the outer one's triangle is
/// clicked with Option held, then the folders opened inside it fold with it: opened again, it
/// shows them folded.
#[test]
fn option_click_folds_the_local_folders_opened_inside() {
    /// Removes the folders however the test ends.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = std::env::temp_dir().join(format!("lc-ui-tree-branch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.clone());
    std::fs::create_dir_all(base.join("Outer").join("Inner").join("Last")).unwrap();
    let text = |p: std::path::PathBuf| p.to_string_lossy().to_string();
    let (place, outer, inner, last) =
        (text(base.clone()), text(base.join("Outer")), text(base.join("Outer").join("Inner")), text(base.join("Outer").join("Inner").join("Last")));
    let (row, toggle) = (|path: &str| format!("source:local:{path}"), |path: &str| format!("folderToggle:{path}"));

    let mut h = demo();
    h.hide_home_above(&base);
    let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": place}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for (open, then) in [(&place, &outer), (&outer, &inner), (&inner, &last)] {
        h.step_until(SETTLE, |h| shown(h, &toggle(open)));
        click(&mut h, &toggle(open));
        h.step_until(SETTLE, |h| shown(h, &row(then)));
        assert!(shown(&h, &row(then)), "{then}");
    }

    // a plain click folds the one row: opened again, what is inside is as it was left, open
    click(&mut h, &toggle(&outer));
    assert!(!shown(&h, &row(&inner)));
    click(&mut h, &toggle(&outer));
    h.step_until(SETTLE, |h| shown(h, &row(&last)));
    assert!(shown(&h, &row(&inner)) && shown(&h, &row(&last)), "a plain fold leaves what is inside open");
    // and so does opening with Option held, which opens the one row
    click(&mut h, &toggle(&outer));
    option_click(&mut h, &toggle(&outer));
    h.step_until(SETTLE, |h| shown(h, &row(&last)));
    assert!(shown(&h, &row(&inner)) && shown(&h, &row(&last)), "opening with Option folds nothing");

    option_click(&mut h, &toggle(&outer));
    assert!(!shown(&h, &row(&inner)));
    // opening with Option opens the one row (what a folder on disk holds is read when it is opened)
    option_click(&mut h, &toggle(&outer));
    h.step_until(SETTLE, |h| shown(h, &row(&inner)));
    h.settle(SETTLE);
    assert!(shown(&h, &row(&inner)) && !shown(&h, &row(&last)), "the folder inside it was folded with it");
    assert!(h.app.ui.sidebar_trees.is_open(&Branch::local(&place, &place)), "the folder it lies in is as it was");
    // a folder inside it opened by hand afterwards is open
    click(&mut h, &toggle(&inner));
    h.step_until(SETTLE, |h| shown(h, &row(&last)));
    assert!(shown(&h, &row(&last)));
}

/// Given rows a script sets over the control channel, when the list says something of a row
/// after what it says of the rows above it, then the row is as the list has it, though the
/// same was said of that row before the script set anything.
#[test]
fn rows_set_over_the_control_channel_hold_in_the_order_given() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
    let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["travel|Italy|Rome"]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for rows in
        [json!([["keyword:travel|italy", true]]), json!([["keyword:travel", true], ["in:keyword:travel", false], ["keyword:travel|italy", true]])]
    {
        let r = h.request("ui.set", json!({"sidebarTrees": rows}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert!(shown(&h, "source:keyword:travel|Italy|Rome"), "listed after the fold of its branch: open");
}

/// A row is as the last thing said of it has it: of the row itself by hand, of all the rows
/// inside a row above it, or that it was opened to show where the grid is. The rows remembered
/// do not grow with the branch: a whole branch is two things said.
#[test]
fn a_row_is_as_the_last_thing_said_of_it_or_of_a_row_above_it() {
    let mut trees = TreeState::default();
    let (outer, inner, last) = (Branch::keyword("a"), Branch::keyword("a|b"), Branch::keyword("a|b|c"));
    // what holds for a row from the rows above it, as the tree asks while it draws
    let under = |trees: &TreeState, rows: &[&Branch]| rows.iter().fold(None, |said, row| trees.inside(row, said));
    let open = |trees: &TreeState| {
        (trees.is_open(&outer), trees.is_open_under(&inner, under(trees, &[&outer])), trees.is_open_under(&last, under(trees, &[&outer, &inner])))
    };
    assert_eq!(open(&trees), (false, false, false), "as keywords start");

    trees.set_with_inside(&outer, true);
    assert_eq!(open(&trees), (true, true, true));
    assert_eq!(trees.len(), 2, "however many rows are inside");
    // said of a row inside it afterwards: that row, and not the rows inside that one
    trees.set(&inner, false);
    assert_eq!(open(&trees), (true, false, true));
    // said of the rows inside that row afterwards: those rows
    trees.set_with_inside(&inner, false);
    assert_eq!(open(&trees), (true, false, false));
    // said of the whole again: every row, whatever was said of each before
    trees.set_with_inside(&outer, true);
    assert_eq!(open(&trees), (true, true, true));
    trees.set_with_inside(&outer, false);
    assert_eq!(open(&trees), (false, false, false));
    // a plain click on the row itself changes the one row: the rows inside it stay as was said
    trees.set(&outer, true);
    assert_eq!(open(&trees), (true, false, false));

    // opened to show where the grid is: open for the session, though the rows inside were folded
    trees.show(&inner, under(&trees, &[&outer]));
    assert_eq!(open(&trees), (true, true, false));
    // as the library opens: a row folded with the rows above it stays folded
    trees.show_unless_left(&last, under(&trees, &[&outer, &inner]));
    assert_eq!(open(&trees), (true, true, false));
    // the whole folded again afterwards: the row that was opened to show the grid too
    trees.set_with_inside(&outer, false);
    trees.set(&outer, true);
    assert_eq!(open(&trees), (true, false, false));
    // a row that is open already is left as it is: nothing more is kept or changed
    trees.set_with_inside(&outer, true);
    let before = serde_json::to_string(&trees).unwrap();
    trees.show(&inner, under(&trees, &[&outer]));
    assert_eq!(serde_json::to_string(&trees).unwrap(), before);

    // a row said folded with the rows above it and then opened to show the grid is open for
    // good: kept, and open when the settings are read again
    trees.set_with_inside(&outer, false);
    trees.set(&outer, true);
    trees.show(&inner, under(&trees, &[&outer]));
    assert_eq!(open(&trees), (true, true, false));
    assert!(serde_json::to_string(&trees).unwrap().ends_with(r#"["keyword:a|b",true]]"#));
    // also when it had been open for the session before the fold
    let mut again = TreeState::default();
    again.show(&inner, None);
    again.set_with_inside(&outer, false);
    again.set(&outer, true);
    assert!(!again.is_open_under(&inner, under(&again, &[&outer])));
    again.show(&inner, under(&again, &[&outer]));
    assert!(again.is_open_under(&inner, under(&again, &[&outer])));
    // with nothing said of the rows inside any row, what holds from above is passed on as it is
    assert_eq!(TreeState::default().inside(&outer, Some((true, 5))), Some((true, 5)));

    // written to the settings and read again: the same, in the order things were said
    let written = serde_json::to_string(&trees).unwrap();
    let read: TreeState = serde_json::from_str(&written).unwrap();
    assert_eq!(open(&read), open(&trees));
    assert_eq!(serde_json::to_string(&read).unwrap(), written);
    // what a script sets is checked: what is said of the rows inside a row names the row
    for (rows, taken) in
        [(r#"[["in:keyword:a",true]]"#, true), (r#"[["in:album:3",false]]"#, true), (r#"[["in:",true]]"#, false), (r#"[["in:nothing",true]]"#, false)]
    {
        assert_eq!(TreeState::parse(&serde_json::from_str(rows).unwrap()).is_ok(), taken, "{rows}");
    }
}

/// Given as many rows as are ever kept, when a row is opened or folded with what is inside it,
/// then the two things that says take the room of the two said longest ago, however many rows
/// are inside it. When they make room themselves one day, what was said of the rows inside goes
/// first: the row stays as it was left, and the rows inside it are as they start again.
#[test]
fn a_whole_branch_takes_the_room_of_two_rows() {
    let mut trees = TreeState::default();
    for i in 0..MAX_ROWS as u64 {
        trees.set(&Branch::album(i), false);
    }
    trees.set_with_inside(&Branch::keyword("travel"), true);
    assert_eq!(trees.len(), MAX_ROWS);
    assert!(
        trees.is_open(&Branch::album(0)) && trees.is_open(&Branch::album(1)) && !trees.is_open(&Branch::album(2)),
        "the two said longest ago made room"
    );
    assert!(trees.is_open(&Branch::keyword("travel")) && trees.inside(&Branch::keyword("travel"), None).is_some_and(|(open, _)| open));

    let mut trees = TreeState::default();
    let folder = Branch::album(9000);
    trees.set_with_inside(&folder, false);
    for i in 0..MAX_ROWS as u64 - 2 {
        trees.set(&Branch::album(i), false);
    }
    assert!(trees.inside(&folder, None).is_some());
    trees.set(&Branch::date("2026"), true);
    assert!(trees.inside(&folder, None).is_none() && !trees.is_open(&folder), "what was said of the rows inside went first");
    trees.set(&Branch::date("2025"), true);
    assert!(trees.is_open(&folder), "then the row: as it starts again");
}

/// What a script sets holds as it is given, in its order, whatever was said before: a row
/// listed after what is said of the rows above it is as it is listed.
#[test]
fn rows_a_script_sets_hold_in_the_order_they_are_given() {
    let (outer, inner) = (Branch::keyword("a"), Branch::keyword("a|b"));
    let inner_open = |trees: &TreeState| trees.is_open_under(&inner, trees.inside(&outer, None));
    let mut before = TreeState::default();
    before.set(&inner, true);
    before.show(&Branch::keyword("z"), None);
    let list = serde_json::json!([["keyword:a", true], ["in:keyword:a", false], ["keyword:a|b", true]]);
    let mut set = TreeState::parse(&list).unwrap();
    set.carry_session_under(&before);
    assert!(inner_open(&set), "listed after the fold of its branch: open");
    assert!(set.is_open(&Branch::keyword("z")), "what was open for the session still is");
    assert_eq!(serde_json::to_value(&set).unwrap(), list);
    // the other way round
    let list = serde_json::json!([["keyword:a|b", true], ["keyword:a", true], ["in:keyword:a", false]]);
    let mut set = TreeState::parse(&list).unwrap();
    set.carry_session_under(&before);
    assert!(!inner_open(&set), "listed before the fold of its branch: folded");
    // and what is said afterwards is later than all of it
    set.set(&inner, true);
    assert!(inner_open(&set));
}

/// Settings that are written and read again while the app runs (`ui.set`) keep when each thing
/// was said: a row opened to show the grid after its branch was folded stays open, and one
/// opened before its branch was folded stays folded.
#[test]
fn settings_set_while_the_app_runs_keep_the_order_of_what_was_said() {
    let (outer, inner, other) = (Branch::keyword("a"), Branch::keyword("a|b"), Branch::keyword("z"));
    let reread = |trees: &TreeState| {
        let mut read: TreeState = serde_json::from_str(&serde_json::to_string(trees).unwrap()).unwrap();
        read.carry_session(trees);
        read
    };
    let inner_open = |trees: &TreeState| trees.is_open_under(&inner, trees.inside(&outer, None));

    let mut trees = TreeState::default();
    trees.set_with_inside(&outer, false);
    trees.set(&outer, true);
    trees.show(&inner, trees.inside(&outer, None));
    assert!(inner_open(&trees) && inner_open(&reread(&trees)), "opened to show the grid after the fold");

    let mut trees = TreeState::default();
    trees.set(&outer, true);
    trees.show(&inner, None);
    trees.set_with_inside(&outer, false);
    trees.set(&outer, true);
    trees.set(&other, true);
    assert!(!inner_open(&trees) && !inner_open(&reread(&trees)), "folded after it was opened to show the grid");
    // and what is said after that is later than everything carried over
    let mut read = reread(&trees);
    read.set_with_inside(&outer, true);
    assert!(inner_open(&read));
}
