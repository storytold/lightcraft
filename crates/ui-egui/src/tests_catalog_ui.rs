//! Library UI commands (P1.5, the `catalog.*` ids): New / Open / Open Recent, the one recent list
//! and startup choice (issue #871), the library dialogs, and the Read Metadata conflict question.

use serde_json::json;

use crate::catalog_ui::CatalogDialog;
use crate::{LccApp, Services};

fn temp(tag: &str) -> lcc_testkit::TempDir {
    let d = lcc_testkit::TempDir::new(&format!("lc-ui-catalog-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn app_in(dir: &std::path::Path) -> LccApp {
    let mut s = lcc_engine::Session::new().with_fs();
    s.open_library(dir, true).unwrap();
    LccApp::new(s, Services::default())
}

/// The first item of File ▸ Open Recent: the recent library listed first.
fn first_in_file_libraries(app: &LccApp) -> Option<String> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File")?.1;
    let Some(MenuNode::Submenu { children, .. }) = file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Open Recent"))
    else {
        return None;
    };
    children.iter().find_map(|n| match n {
        MenuNode::Item { id, params, .. } if id == "app.openLibrary" => params["path"].as_str().map(str::to_string),
        _ => None,
    })
}

fn same(a: &str, b: &std::path::Path) -> bool {
    std::path::Path::new(a) == b
}

#[test]
fn new_open_and_open_recent_switch_libraries_and_keep_the_list() {
    let root = temp("switch");
    let first = root.join("First");
    let mut app = app_in(&first);
    let photos = app.session.catalog.len();
    assert!(photos > 0);

    // New Library… without a name opens the dialog; with one, creates and opens it
    app.run("catalog.new", json!({})).unwrap();
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::NewCatalog { .. })));
    let r = app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "Second"})).unwrap();
    assert!(r["entry"].as_str().unwrap().ends_with(&format!("Second.{}", lcc_brand::CATALOG_EXT)), "{r}");
    assert!(app.session.catalog.is_empty());
    assert!(app.catalog_ui.dialog.is_none());
    assert_eq!(app.session.catalog_dir(), Some(root.join("Second").as_path()));

    // Open Recent lists both, newest first, from the one recent list, and opens the first one
    let recent = app.run("catalog.openRecent", json!({})).unwrap();
    let recent: Vec<&str> = recent["recent"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(recent.len(), 2, "{recent:?}");
    assert!(same(recent[0], &root.join("Second")) && same(recent[1], &first), "{recent:?}");
    app.run("catalog.openRecent", json!({"path": recent[1]})).unwrap();
    assert_eq!(app.session.catalog.len(), photos);
    assert!(same(&app.ui.settings.recent_libraries[0], &first), "{:?}", app.ui.settings.recent_libraries);

    // a folder that is no library is refused (not silently made into an empty one)
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("x.txt"), b"x").unwrap();
    assert!(app.run("catalog.open", json!({"path": plain.to_string_lossy()})).is_err());
    assert_eq!(app.session.catalog.len(), photos, "still the first library");
    app.run("catalog.open", json!({"path": root.join("Second").to_string_lossy()})).unwrap();
    assert!(app.session.catalog.is_empty());

    // a library gone from where Open Recent remembers it is said so and forgotten
    std::fs::remove_dir_all(&first).unwrap();
    assert!(app.run("catalog.openRecent", json!({"path": first.to_string_lossy()})).is_err());
    assert!(!app.ui.settings.recent_libraries.iter().any(|r| same(r, &first)), "{:?}", app.ui.settings.recent_libraries);
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #871 (AC-1): however a library is opened, it heads File ▸ Libraries and `app.recentLibraries`.
#[test]
fn every_way_of_opening_a_library_puts_it_first_in_file_libraries() {
    let root = temp("one-list");
    let mut app = app_in(&root.join("A"));
    for name in ["B", "C"] {
        lcc_catalog::library::create(&root, name).unwrap();
    }
    let (a, b, c) = (root.join("A"), root.join("B"), root.join("C"));
    app.run("catalog.open", json!({"path": b.to_string_lossy()})).unwrap();
    assert!(first_in_file_libraries(&app).is_some_and(|p| same(&p, &b)), "catalog.open: {:?}", first_in_file_libraries(&app));
    app.run("app.openLibrary", json!({"path": c.to_string_lossy()})).unwrap();
    assert!(first_in_file_libraries(&app).is_some_and(|p| same(&p, &c)), "app.openLibrary");
    let recent = app.run("catalog.openRecent", json!({})).unwrap();
    assert!(recent["recent"][0].as_str().is_some_and(|p| same(p, &c)), "catalog.openRecent lists app.openLibrary's library first: {recent}");
    app.run("catalog.openRecent", json!({"path": a.to_string_lossy()})).unwrap();
    assert!(first_in_file_libraries(&app).is_some_and(|p| same(&p, &a)), "catalog.openRecent");
    let listed = app.run("app.recentLibraries", json!({})).unwrap();
    assert!(listed[0]["path"].as_str().is_some_and(|p| same(p, &a)), "{listed}");
    let r = app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "D"})).unwrap();
    assert!(r["entry"].is_string(), "{r}");
    assert!(first_in_file_libraries(&app).is_some_and(|p| same(&p, &root.join("D"))), "catalog.new");
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #871 (AC-2): the recent list of older versions (`recent-catalogs.json`) joins the one
/// recent list once, with its "always show" choice, and is set aside once the settings are saved; a
/// damaged one is skipped and set aside.
#[test]
fn the_old_recent_catalogs_list_is_carried_over_once() {
    use crate::catalog_ui::{CarryOver, carry_over_recent_catalogs, set_aside_recent_catalogs};
    use crate::panels::library_problem::{Launch, launch_plan};
    use crate::state::{AppSettings, LaunchLibrary};
    let root = temp("carry");
    let kept = lcc_catalog::library::create(&root, "Kept").unwrap();
    let listed = lcc_catalog::library::create(&root, "Listed").unwrap();
    let listed_dir = listed.parent().unwrap().to_string_lossy().into_owned();
    let file = root.join("recent-catalogs.json");
    let old = json!({"paths": [kept, root.join("Gone").join("Gone.x"), listed], "prompt_at_startup": true});
    std::fs::write(&file, old.to_string()).unwrap();
    let mut settings = AppSettings { recent_libraries: vec![listed_dir.clone()], ..Default::default() };
    assert_eq!(settings.at_launch, LaunchLibrary::Last);

    assert_eq!(carry_over_recent_catalogs(&mut settings, &file), CarryOver::Carried { added: 1, ask: true });
    assert_eq!(settings.recent_libraries.len(), 2, "{:?}", settings.recent_libraries);
    assert_eq!(settings.recent_libraries[0], listed_dir, "the settings' own list stays first");
    assert!(same(&settings.recent_libraries[1], kept.parent().unwrap()), "added at the end: {:?}", settings.recent_libraries);
    assert_eq!(settings.at_launch, LaunchLibrary::Ask, "always show → Ask me");
    assert_eq!(launch_plan(false, None, None, Some(&settings), None), Launch::Ask, "the launch asks");
    assert!(file.exists(), "kept until the settings are saved");

    // a start that ended before the save carries it over again, adding nothing twice
    assert_eq!(carry_over_recent_catalogs(&mut settings, &file), CarryOver::Carried { added: 0, ask: true });
    assert_eq!(settings.recent_libraries.len(), 2, "{:?}", settings.recent_libraries);
    set_aside_recent_catalogs(&file);
    assert!(!file.exists(), "set aside");

    // once: a library removed from the list meanwhile doesn't come back
    settings.recent_libraries.truncate(1);
    settings.at_launch = LaunchLibrary::Last;
    assert_eq!(carry_over_recent_catalogs(&mut settings, &file), CarryOver::Nothing);
    assert_eq!(settings.recent_libraries.len(), 1);
    assert_eq!(settings.at_launch, LaunchLibrary::Last);

    // a damaged file: skipped, the settings unchanged, and set aside (not read again)
    std::fs::write(&file, b"{not json").unwrap();
    let before = settings.clone();
    assert!(matches!(carry_over_recent_catalogs(&mut settings, &file), CarryOver::Skipped(_)));
    assert_eq!(settings, before);
    assert!(!file.exists());
    assert_eq!(carry_over_recent_catalogs(&mut settings, &file), CarryOver::Nothing);
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #871 (AC-3): `catalog.promptAtStartup` is Settings ▸ General ▸ At launch.
#[test]
fn prompt_at_startup_sets_at_launch() {
    use crate::panels::library_problem::{Launch, launch_plan};
    use crate::state::LaunchLibrary;
    let root = temp("prompt");
    let mut app = app_in(&root.join("Lib"));
    let r = app.run("catalog.promptAtStartup", json!({"on": true})).unwrap();
    assert_eq!(r, json!({"promptAtStartup": true, "atLaunch": "ask"}));
    assert_eq!(app.ui.settings.at_launch, LaunchLibrary::Ask);
    assert_eq!(launch_plan(false, None, None, Some(&app.ui.settings), None), Launch::Ask);
    app.run("catalog.promptAtStartup", json!({"on": false})).unwrap();
    assert_eq!(app.ui.settings.at_launch, LaunchLibrary::Last);
    // no `on`: switches it
    assert_eq!(app.run("catalog.promptAtStartup", json!({})).unwrap()["promptAtStartup"], true);
    assert_eq!(app.ui.settings.at_launch, LaunchLibrary::Ask);
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #871 (AC-4): `catalog.chooser` opens the launch list over the library open now, as Alt held
/// at launch does; never over the window of a library that couldn't be opened, or over a temporary
/// session (Continue Without Saving), which goes on saving nothing.
#[test]
fn choose_library_opens_the_launch_list() {
    let root = temp("chooser");
    let mut app = app_in(&root.join("Lib"));
    app.ui.settings.recent_libraries = vec![root.join("Lib").to_string_lossy().into_owned()];
    app.run("catalog.chooser", json!({})).unwrap();
    let c = app.library_problem.as_ref().and_then(|p| p.chooser.clone()).expect("the launch list");
    assert_eq!(c.over, Some(app.session.library_generation()), "over the open library");
    assert_eq!(c.libraries.len(), 1);
    assert!(app.catalog_ui.dialog.is_none(), "no second chooser");
    app.run("catalog.chooser", json!({})).expect("already up: nothing to do");
    assert!(app.library_problem.as_ref().is_some_and(|p| p.chooser.is_some()));

    use crate::panels::library_problem::LibraryProblem;
    app.library_problem = Some(LibraryProblem::new(root.join("Gone").to_string_lossy(), "not found"));
    assert!(app.run("catalog.chooser", json!({})).is_err(), "the failure window waits for its answer");
    assert!(app.library_problem.as_ref().is_some_and(|p| p.chooser.is_none() && !p.dismissed), "left as it was");
    app.library_problem.as_mut().unwrap().dismissed = true;
    assert!(app.run("catalog.chooser", json!({})).is_err(), "a temporary session stays one");
    assert!(app.library_problem.as_ref().is_some_and(LibraryProblem::saves_nothing), "still saves nothing");
    app.library_problem = None;
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// The command ids of File's catalog items (Open Recent's included).
fn file_libraries_ids(app: &LccApp) -> Vec<String> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    // (File's catalog group: its items, and Open Recent's)
    let recent = file.iter().find_map(|n| match n {
        MenuNode::Submenu { label, children } if label == "Open Recent" => Some(children.clone()),
        _ => None,
    });
    let Some(recent) = recent else { panic!("no Open Recent submenu in {file:?}") };
    file.iter().chain(&recent).filter_map(|n| if let MenuNode::Item { id, .. } = n { Some(id.clone()) } else { None }).collect()
}

/// Issue #871 (AC-5): the desktop's File ▸ Libraries backs the library up (into its backup folder)
/// and says where; a host with a backup of its own (the web build) shows only that one.
#[test]
fn file_libraries_backs_up_the_library_on_the_desktop() {
    let root = temp("backup");
    let mut app = app_in(&root.join("Lib"));
    let ids = file_libraries_ids(&app);
    assert!(ids.iter().any(|i| i == "catalog.backup"), "{ids:?}");
    assert!(!ids.iter().any(|i| i == "file.backupLibrary"), "{ids:?}");
    let r = app.run("catalog.backup", json!({})).unwrap();
    let backup = r["backup"].as_str().expect("where it went");
    assert!(std::path::Path::new(backup).is_dir(), "{r}");
    assert!(app.ui.toasts.iter().any(|t| t.0.contains(backup)), "a toast says where: {:?}", app.ui.toasts);
    app.services.backup_library = Some(Box::new(|_: &mut lcc_engine::Session| Ok(json!({}))));
    let ids = file_libraries_ids(&app);
    assert!(ids.iter().any(|i| i == "file.backupLibrary") && !ids.iter().any(|i| i == "catalog.backup"), "{ids:?}");
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn catalog_dialogs_open_and_settings_apply() {
    let root = temp("dialogs");
    let mut app = app_in(&root.join("Lib"));
    app.run("dialog.catalogSettings", json!({})).unwrap();
    let Some(CatalogDialog::Settings { settings }) = app.catalog_ui.dialog.clone() else { panic!("settings dialog") };
    assert_eq!(settings["backup"], "weekly", "{settings}");
    app.run("catalog.settings", json!({"backup": "daily", "previews": {"discardFull": "week"}})).unwrap();
    let info = app.run("catalog.info", json!({})).unwrap();
    assert_eq!(info["settings"]["backup"], "daily");
    assert_eq!(info["settings"]["previews"]["discardFull"], "week");

    app.run("dialog.exportCatalog", json!({})).unwrap();
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::ExportCatalog { .. })));
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let r = app.run("catalog.export", json!({"parent": out.to_string_lossy(), "name": "Copy", "scope": "all"})).unwrap();
    app.run("dialog.importCatalog", json!({"path": r["entry"]})).unwrap();
    let Some(CatalogDialog::ImportCatalog { plan: Ok(plan), .. }) = app.catalog_ui.dialog.clone() else { panic!("import preview") };
    assert_eq!(plan["unchanged"].as_u64(), Some(0), "demo photos don't match by path: {plan}");
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_metadata_asks_before_overwriting_catalog_changes() {
    let root = temp("xmp");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 150, 255]; 64] };
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(src.join("a.png"), png).unwrap();
    let mut s = lcc_engine::Session::new().with_fs();
    s.open_library(root.join("Lib"), false).unwrap();
    let mut app = LccApp::new(s, Services::default());
    app.run("library.import", json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = app.session.catalog.photos().next().unwrap().id.0;
    app.run("photo.saveMetadataToFile", json!({"ids": [id]})).unwrap();
    // in step: no question
    app.run("photo.readMetadataFromFile", json!({"ids": [id]})).unwrap();
    assert!(app.catalog_ui.dialog.is_none());
    app.run("photo.rate", json!({"ids": [id], "rating": 5})).unwrap();
    let r = app.run("photo.readMetadataFromFile", json!({"ids": [id]})).unwrap();
    assert_eq!(r["confirm"], "dialog", "{r}");
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::XmpConflict { affected: 1, .. })));
    assert_eq!(app.session.catalog.photos().next().unwrap().rating, 5, "nothing read yet");
    app.run("photo.readMetadataFromFile", json!({"ids": [id], "confirmed": true})).unwrap();
    assert_eq!(app.session.catalog.photos().next().unwrap().rating, 0);
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// First start (maintainer report): the welcome dialog creates a catalog — with the sample photos
/// only when asked for — and opens it.
#[test]
fn welcome_creates_a_catalog_with_or_without_samples() {
    let root = temp("welcome");
    let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    app.run("catalog.welcome", json!({})).unwrap();
    assert!(matches!(&app.catalog_ui.dialog, Some(CatalogDialog::Welcome { demo: false, name, .. }) if name == lcc_brand::LIBRARY_DEFAULT));
    app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "Plain", "demo": false})).unwrap();
    assert!(app.session.catalog.is_empty());
    assert!(app.catalog_ui.dialog.is_none());
    app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "Samples", "demo": true})).unwrap();
    assert!(!app.session.catalog.is_empty(), "sample photos when asked for");
    assert_eq!(app.session.catalog_dir(), Some(root.join("Samples").as_path()));
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// The problem window says "damaged" only for a catalog that failed its integrity check; another
/// app's catalog, a locked, newer, missing or forbidden one get their own words.
#[test]
fn library_problems_are_worded_by_their_cause() {
    use crate::panels::library_problem::{LibraryProblem, ProblemKind};
    let root = temp("problems");
    let here = root.to_string_lossy().to_string();
    // another app's catalog (e.g. the upstream app's): the report's case, opened for real
    let foreign = root.join("Upstream Library");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("catalog.snap"), "{\"format\":\"upstream-catalog\",\"version\":6,\"seq\":0,\"catalog\":{}}\n").unwrap();
    let mut s = lcc_engine::Session::new().with_fs();
    let e = s.open_library(&foreign, false).unwrap_err().to_string();
    let p = LibraryProblem::new(foreign.to_string_lossy(), e.clone());
    assert_eq!(p.kind(), ProblemKind::Foreign, "{e}");
    assert!(!e.to_lowercase().contains("corrupt"), "{e}");
    for kind in [ProblemKind::Foreign, ProblemKind::InUse, ProblemKind::Newer, ProblemKind::Missing, ProblemKind::Permission, ProblemKind::Other] {
        let words = format!("{} {}", kind.title(), kind.advice()).to_lowercase();
        assert!(!words.contains("corrupt") && !words.contains("damaged"), "{kind:?}: {words}");
    }
    assert!(ProblemKind::Damaged.title().contains("damaged"));
    assert_eq!(ProblemKind::of(&here, "corrupt catalog data: catalog.redb: DB corrupted"), ProblemKind::Damaged);
    assert_eq!(ProblemKind::of(&here, "This library is already open in app (pid 7). Close it there first."), ProblemKind::InUse);
    assert_eq!(ProblemKind::of(&here, "this library was written by a newer version of the app (v9)"), ProblemKind::Newer);
    assert_eq!(ProblemKind::of(&here, "catalog storage: Permission denied (os error 13)"), ProblemKind::Permission);
    assert_eq!(ProblemKind::of(&root.join("gone").to_string_lossy(), "catalog storage: something"), ProblemKind::Missing);
    assert_eq!(ProblemKind::of(&here, "something else"), ProblemKind::Other);
    assert_eq!(ProblemKind::of("", "no home"), ProblemKind::NoLocation);
    assert!(!ProblemKind::Foreign.retryable() && ProblemKind::InUse.retryable());
    let _ = std::fs::remove_dir_all(&root);
}

/// Refine Photos asks first (nothing changes yet); confirming applies it.
#[test]
fn refine_photos_asks_before_changing_flags() {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    let first = app.session.visible_cloned()[0];
    app.run("photo.flag", json!({"ids": [first.0], "flag": "pick"})).unwrap();
    let r = app.run("library.refine", json!({})).unwrap();
    assert_eq!(r["confirm"], "dialog", "{r}");
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::Refine { .. })));
    assert_eq!(app.session.catalog.photo(first).unwrap().flag, lcc_catalog::Flag::Pick, "nothing changed yet");
    app.run("library.refine", json!({"confirmed": true})).unwrap();
    assert_eq!(app.session.catalog.photo(first).unwrap().flag, lcc_catalog::Flag::None);
}
