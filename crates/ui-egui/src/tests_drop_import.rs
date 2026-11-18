//! Files dragged from the file manager onto the window: while they hover, the window says what
//! dropping will do; dropped photos and folders open the import review (or, with Settings ▸ Import ▸
//! Import dropped photos without review, are added at once); a drop while a dialog is open, or while
//! an import runs, is refused.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-drop-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// An empty library, a folder `trip` with two photos, a window big enough for the overlay.
fn app(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let session = lcc_engine::Session::new().with_fs();
    let mut h = Headless::new(LccApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 900.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    h
}

fn drop_files(h: &mut Headless, paths: &[String], hover: bool) {
    let r = h.request("ui.dropFiles", json!({"paths": paths, "hover": hover}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
}

fn overlay(h: &Headless) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == "dropOverlay")
}

fn said(h: &Headless, word: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(word))
}

fn photos(h: &Headless) -> usize {
    h.app.session.catalog.photos().count()
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn hovering_says_what_a_drop_will_do_and_lets_go_when_the_files_leave() {
    let dir = Scratch::new("hover");
    let mut h = app(&dir);
    // hovering says photos are reviewed or imported and lets go when they leave
    {
        assert!(!overlay(&h), "nothing is dragged over the window");
        drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], true);
        assert!(overlay(&h), "the window shows where to drop");
        assert!(said(&h, "review 2 items"), "photos are reviewed by default: {:?}", h.painted_text());
        drop_files(&mut h, &[dir.path("trip")], true);
        assert!(said(&h, "review 1 item"), "a folder is reviewed: {:?}", h.painted_text());
        h.app.ui.settings.import_dropped_directly = true;
        drop_files(&mut h, &[dir.path("trip")], true);
        assert!(said(&h, "import 1 item"), "without review: {:?}", h.painted_text());
        drop_files(&mut h, &[], true);
        assert!(!overlay(&h), "the files left the window");
    }
    // hovering presets says they will be imported as presets
    {
        let dir = Scratch::new("hover-presets");
        std::fs::write(dir.path("warm.lcpreset"), "{}").unwrap();
        drop_files(&mut h, &[dir.path("warm.lcpreset")], true);
        assert!(overlay(&h));
        assert!(said(&h, "1 preset"), "{:?}", h.painted_text());
        assert!(!said(&h, "review"), "presets are not reviewed");
    }
    // hovering unknown files says only that dropping imports
    {
        drop_files(&mut h, &[String::new()], true);
        assert!(overlay(&h));
        assert!(said(&h, "Drop to import"), "{:?}", h.painted_text());
    }
}

#[test]
fn dropped_photo_files_open_the_import_review_by_default() {
    assert!(!crate::state::AppSettings::default().import_dropped_directly, "off by default");
    let older: crate::state::AppSettings = serde_json::from_value(json!({})).unwrap();
    assert!(!older.import_dropped_directly, "a settings file from before the option reviews drops");
    let dir = Scratch::new("files-review");
    let mut h = app(&dir);
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], false);
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2, "both files are listed");
    assert_eq!(photos(&h), 0, "nothing is imported until the review is confirmed");
}

#[test]
fn with_the_option_dropped_photos_and_folders_are_imported_at_once() {
    let saved: crate::state::AppSettings = serde_json::from_value(json!({"importDroppedDirectly": true})).unwrap();
    assert!(saved.import_dropped_directly, "the saved choice is kept");
    let dir = Scratch::new("folder-direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.step_until(SETTLE, |h| photos(h) == 2 && h.app.import.is_none()), "the folder's photos arrive");
    assert!(h.app.ui.dialog.is_none(), "no review");
}

#[test]
fn dropped_photo_files_are_imported_at_once_with_the_option() {
    let dir = Scratch::new("direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], false);
    assert!(h.step_until(SETTLE, |h| photos(h) == 2 && h.app.import.is_none()), "both photos arrive");
    assert!(h.app.ui.dialog.is_none(), "no review");
}

#[test]
fn a_drop_while_a_dialog_is_open_leaves_the_dialog_and_imports_nothing() {
    let dir = Scratch::new("dialog");
    let mut h = app(&dir);
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    drop_files(&mut h, &[dir.path("trip/a.png")], true);
    assert!(overlay(&h), "the window still answers the hover");
    assert!(said(&h, "Close the open dialog"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("trip")], false);
    for _ in 0..30 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the open dialog stays");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "no scan or import started");
    assert_eq!(photos(&h), 0);
    assert!(h.app.ui.toast().is_some(), "the person is told why");
}

/// The scan of a dropped folder finishing while another dialog is open (the face model's licence, Settings…)
/// keeps that dialog; the review opens, with the scan's photos, once it is closed.
#[test]
fn a_finished_scan_waits_for_the_open_dialog_before_opening_the_review() {
    let dir = Scratch::new("waits");
    let mut h = app(&dir);
    crate::import::open(&mut h.app, vec![dir.path("trip")]).unwrap();
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    assert!(h.step_until(SETTLE, |h| h.app.scan.is_none()), "the scan ends");
    for _ in 0..10 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the dialog the person had open stays");
    h.app.ui.dialog = None;
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens once the dialog is closed");
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2, "the scan's photos were kept");
}

/// A scan that finishes behind Settings, which then opens another library: the review of the first
/// library's scan doesn't open over the second one once Settings closes, and nothing is imported into it.
#[test]
fn a_waiting_review_is_dropped_when_another_library_opens() {
    let dir = Scratch::new("switch");
    let mut h = app(&dir);
    crate::import::open(&mut h.app, vec![dir.path("trip")]).unwrap();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    assert!(h.step_until(SETTLE, |h| h.app.scan.is_none()), "the scan ends");
    h.step();
    let stale = h.app.review_pending.clone().expect("the review waits behind Settings");
    h.app.run("app.openLibrary", json!({"path": dir.path("other")})).unwrap();
    h.app.ui.dialog = None;
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "no review from the other library opens");
    assert!(h.app.review_pending.is_none());
    // a review confirmed after the switch (an agent holding it) imports nothing either
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &stale).is_err());
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.import.is_none(), "nothing is imported");
    assert_eq!(photos(&h), 0);
}

/// A review waiting behind another dialog counts as an import in progress: a direct drop meanwhile is
/// refused, and the review's OK never replaces an import that runs when it is confirmed.
#[test]
fn a_waiting_review_never_replaces_a_running_import() {
    let dir = Scratch::new("pending-running");
    let mut h = app(&dir);
    write_png(&dir.path("more/c.png"), 3);
    crate::import::open(&mut h.app, vec![dir.path("trip")]).unwrap();
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    assert!(h.step_until(SETTLE, |h| h.app.scan.is_none()), "the scan ends");
    h.step();
    assert!(h.app.review_pending.is_some(), "the review waits behind the dialog");
    assert!(crate::import::start_paths(&mut h.app, vec![dir.path("more")]).is_err(), "no second import starts");
    assert!(h.app.import.is_none());
    h.app.ui.dialog = None;
    h.step();
    let Some(review) = h.app.ui.dialog.clone() else { panic!("the review opens once the dialog is closed") };
    assert!(matches!(review, Dialog::Import { .. }));
    // an import that started anyway (a Local folder browse, say) is the one that runs
    h.app.import = Some(crate::import::ImportTask::new(vec![dir.path("more")], json!({"mode": "add"}), 0, false));
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &review).is_err(), "the review's OK is refused");
    assert_eq!(h.app.import.as_ref().map(|t| t.total), Some(1), "the running import stays");
    assert!(h.step_until(SETTLE, |h| h.app.import.is_none()), "it runs to the end");
    assert_eq!(photos(&h), 1, "only the running import's photo arrives");
}

/// A drop while an import runs would open a review whose OK replaces the running import: it is
/// refused instead, and the card says so.
#[test]
fn a_drop_while_an_import_runs_is_refused() {
    let dir = Scratch::new("running");
    let mut h = app(&dir);
    write_png(&dir.path("more/c.png"), 3);
    let _scan = crate::import::stall_scan(&mut h.app);
    drop_files(&mut h, &[dir.path("more/c.png")], true);
    assert!(said(&h, "An import is running"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("more")], false);
    for _ in 0..30 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    assert!(h.app.scan.is_some() && h.app.import.is_none(), "the running scan was left alone and nothing else started");
    assert!(h.app.ui.toast().is_some(), "the person is told why");
    assert_eq!(photos(&h), 0);
}

/// A Local folder browse scan counts too: a review-first drop would be rejected by `open`, and a
/// direct drop would be replaced when the browse completes.
#[test]
fn a_drop_while_a_browse_scan_runs_is_refused_for_files_and_folders() {
    for direct in [false, true] {
        let dir = Scratch::new("running-browse");
        let mut h = app(&dir);
        write_png(&dir.path("more/c.png"), 3);
        let _scan = crate::import::stall_scan_of(&mut h.app, true);
        drop_files(&mut h, &[dir.path("more/c.png")], true);
        assert!(said(&h, "An import is running"), "{:?}", h.painted_text());
        let dropped = if direct { dir.path("more/c.png") } else { dir.path("more") };
        drop_files(&mut h, &[dropped], false);
        for _ in 0..30 {
            h.step();
        }
        assert!(h.app.ui.dialog.is_none() && h.app.import.is_none(), "direct={direct}: nothing started");
        assert!(h.app.scan.is_some(), "the browse was left alone");
        assert!(h.app.ui.toast().is_some(), "the person is told why");
    }
}

/// While a catalog import is pending the drop is refused, and the card says so instead of
/// promising a review.
#[test]
fn hovering_photos_while_a_lightroom_import_is_pending_says_they_will_be_refused() {
    let dir = Scratch::new("lightroom");
    let mut h = app(&dir);
    let _task = crate::lightroom_import::stall_import(&mut h.app);
    drop_files(&mut h, &[dir.path("trip")], true);
    assert!(said(&h, "Finish the catalog import"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    assert!(h.app.ui.toast().is_some(), "the person is told why");
    assert_eq!(photos(&h), 0);
}

/// A script is not told `ok` for the part of a `paths` list that was strings.
#[test]
fn drop_files_with_a_non_string_path_is_rejected_and_drops_nothing() {
    let dir = Scratch::new("badpaths");
    let mut h = app(&dir);
    let r = h.request("ui.dropFiles", json!({"paths": [dir.path("trip"), 7]}), T);
    assert_eq!(r["ok"], false, "{r}");
    for _ in 0..30 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "nothing was dropped");
    assert_eq!(photos(&h), 0);
}

/// No library open yet (the window of a library that failed to open, or the launch list): the
/// card says the photos wait, and a drop keeps them for the library chosen instead of importing
/// them into the session that library replaces.
#[test]
fn photos_dropped_with_no_library_open_wait_for_one() {
    let dir = Scratch::new("no-library");
    let mut h = app(&dir);
    h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new(dir.path("Gone"), "not found"));
    std::fs::write(dir.path("warm.lcpreset"), "{}").unwrap();
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("warm.lcpreset")], true);
    assert!(said(&h, "once a catalog is open"), "{:?}", h.painted_text());
    assert!(said(&h, "drop the presets again") && !said(&h, "1 preset"), "{:?}", h.painted_text());
    drop_files(&mut h, &[], true);
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], false);
    assert_eq!(h.app.library_problem.as_ref().map(|p| p.pending_import.len()), Some(2));
    assert!(h.app.ui.dialog.is_none() && h.app.import.is_none(), "no review, no import into the temporary session");
    assert_eq!(photos(&h), 0);
    // a preset belongs to a library: refused, not imported into the session the library replaces
    h.app.ui.toasts.clear();
    drop_files(&mut h, &[dir.path("warm.lcpreset")], false);
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("drop the presets again"), "{toast}");
}

/// A fresh temporary folder `tag` with a photo in it and one in a subfolder.
fn nested_folder(tag: &str) -> lcc_testkit::TempDir {
    let root = lcc_testkit::TempDir::new(&format!("lc-drop-import-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let sub = root.join("day 2");
    std::fs::create_dir_all(&sub).unwrap();
    // different pictures: the same bytes twice would import once (a duplicate)
    for (file, shade) in [(root.join("a.png"), 90), (sub.join("b.png"), 200)] {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[shade, 120, 160, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(file, png).unwrap();
    }
    root
}

fn fs_app() -> Headless {
    let session = lcc_engine::Session::new().with_fs();
    let mut h = Headless::new(LccApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 900.0], 1.0);
    h.settle(T);
    h
}

/// Run frames (without other input, as a window left alone) until `done` or the timeout.
fn wait_for(h: &mut Headless, mut done: impl FnMut(&Headless) -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < T {
        h.step();
        if done(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn a_dropped_folder_and_its_subfolders_are_scanned_into_the_review() {
    let dir = nested_folder("review");
    let mut h = fs_app();
    h.dropped.push(dir.to_path_buf());
    let opened = wait_for(&mut h, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
    assert!(opened, "no import review after the drop: {:?}", h.app.ui.dialog);
    let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2, "both files, the subfolder's too");
    assert_eq!(opts.sources, vec![dir.to_string_lossy().to_string()]);
    assert!(h.app.session.activity.list().is_empty(), "the scan's activity row finished: {:?}", h.app.session.activity.list());
    // confirming the review adds the photos and the import's row finishes too
    let r = h.request("ui.dialog.confirm", serde_json::json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(wait_for(&mut h, |h| h.app.import.is_none() && h.app.session.catalog.photos().count() == 2), "the import finished");
    assert!(h.app.session.activity.list().is_empty(), "no row left behind: {:?}", h.app.session.activity.list());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dropped_folder_that_vanished_says_so_and_leaves_nothing_running() {
    let dir = nested_folder("gone").join("missing");
    let mut h = fs_app();
    h.dropped.push(dir);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(wait_for(&mut h, |h| h.app.scan.is_none() && h.app.import.is_none()), "nothing stays running");
    assert!(h.app.session.activity.list().is_empty(), "no row left behind: {:?}", h.app.session.activity.list());
    // the review lists the path with why it can't be read (nothing to check), or a toast says so
    let listed = matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Import { opts })
        if opts.candidates.iter().all(|c| c.error.is_some()) && opts.checked.iter().all(|c| !c));
    assert!(listed || h.app.ui.toast().is_some(), "the failure is shown");
}

/// Auto Import's second listing of `watch/w.png` on its worker thread (the first notes the file's size; the
/// second, size unchanged, imports it).
fn second_auto_import_listing(h: &mut Headless, dir: &Scratch) {
    write_png(&dir.path("watch/w.png"), 3);
    h.app.session.import_defaults.auto_folder = Some(dir.path("watch"));
    assert!(h.step_until(T, |h| h.app.tasks.is_running("Auto Import")), "the watched folder is listed");
    assert!(h.step_until(T, |h| !h.app.tasks.is_running("Auto Import")));
    assert_eq!(photos(h), 0);
    assert!(h.step_until(T, |h| h.app.tasks.is_running("Auto Import")), "and listed again");
}

/// Apply Auto Import's listing now, before another frame can change what runs.
fn apply_auto_import_listing(h: &mut Headless) {
    let ctx = h.view.ctx.clone();
    assert!(crate::tasks::wait_for(&mut h.app, &ctx, "Auto Import", T), "the listing ends");
}

/// An import that starts while Auto Import lists its watched folder: the new photo isn't marked as
/// tried and skipped, the next listing imports it once the other import is done.
#[test]
fn auto_import_waits_for_an_import_that_starts_during_its_listing() {
    let dir = Scratch::new("auto-race");
    let mut h = app(&dir);
    second_auto_import_listing(&mut h, &dir);
    h.app.import = Some(crate::import::ImportTask::new(vec![dir.path("trip")], json!({"mode": "add"}), 0, false));
    apply_auto_import_listing(&mut h);
    assert!(h.step_until(T, |h| photos(h) == 3), "the trip's two photos and the watched one arrive: {}", photos(&h));
    h.app.session.import_defaults.auto_folder = None;
}

/// The same with a Local folder browse: a browse that finishes replaces `app.import`, so Auto Import
/// doesn't start one beside it.
#[test]
fn auto_import_waits_for_a_browse_that_starts_during_its_listing() {
    let dir = Scratch::new("auto-browse");
    let mut h = app(&dir);
    second_auto_import_listing(&mut h, &dir);
    let scan = crate::import::stall_scan_of(&mut h.app, true);
    apply_auto_import_listing(&mut h);
    assert!(h.app.import.is_none(), "no Auto Import for the browse to replace");
    drop(scan);
    assert!(h.step_until(T, |h| h.app.scan.is_none()), "the browse ends");
    assert!(h.step_until(T, |h| photos(h) == 1), "and the watched photo arrives: {}", photos(&h));
    h.app.session.import_defaults.auto_folder = None;
}

/// A watched folder that can't be listed (removed, unmounted, not allowed) is noted with the reason, for
/// Settings ▸ Import to show, and the note goes once the folder can be read again.
#[test]
fn auto_import_notes_a_watched_folder_it_cannot_read() {
    let dir = Scratch::new("auto-unreadable");
    let mut h = app(&dir);
    let watch = dir.path("watch");
    h.app.session.import_defaults.auto_folder = Some(watch.clone());
    assert!(h.step_until(T, |h| h.app.ui.auto_import_error.is_some()), "the missing folder is noted");
    let (folder, e) = h.app.ui.auto_import_error.clone().unwrap();
    assert_eq!(folder, watch);
    assert!(e.starts_with(&watch) && e.len() > watch.len() + 2, "the error says why: {e}");
    // another folder watched: the note was about the old one, so it goes, and a listing of the old one
    // still under way doesn't bring it back
    h.app.session.import_defaults.auto_folder = Some(dir.path("trip"));
    h.step();
    assert!(h.app.ui.auto_import_error.is_none(), "{:?}", h.app.ui.auto_import_error);
    h.app.session.import_defaults.auto_folder = Some(watch.clone());
    assert!(h.step_until(T, |h| h.app.ui.auto_import_error.is_some()), "back to the missing folder: noted again");
    write_png(&dir.path("watch/w.png"), 3);
    assert!(h.step_until(T, |h| h.app.ui.auto_import_error.is_none()), "readable again: the note goes");
    assert!(h.step_until(T, |h| photos(h) == 1), "and the photo arrives");
    h.app.session.import_defaults.auto_folder = None;
}

/// Enable Auto Import switches watching off and on again while the folder stays set; with no folder yet,
/// turning it on opens Settings at the Auto Import section instead.
#[test]
fn enable_auto_import_keeps_the_folder_when_off_and_asks_for_one_when_unset() {
    let dir = Scratch::new("auto-toggle");
    let mut h = app(&dir);
    let checked = |h: &Headless| crate::menubar::checked(&h.app, "app.autoImportToggle");
    let toggle = |h: &mut Headless| {
        crate::menus::run_ui_command(&mut h.app, "app.autoImportToggle", &serde_json::json!({})).expect("a UI command").expect("runs");
    };
    assert_eq!(checked(&h), Some(false));
    // no folder: nothing to switch on, Settings opens at the section
    toggle(&mut h);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })));
    assert_eq!(h.app.ui.settings_section.as_deref(), Some("autoImport"));
    assert_eq!(checked(&h), Some(false));
    h.app.ui.dialog = None;
    // a folder is watched; off keeps it, on watches it again
    std::fs::create_dir_all(dir.path("watch")).expect("watch folder");
    h.app.session.execute("library.autoImport", &serde_json::json!({"folder": dir.path("watch")})).expect("set");
    assert_eq!(checked(&h), Some(true));
    toggle(&mut h);
    assert_eq!(checked(&h), Some(false));
    assert_eq!(h.app.session.import_defaults.auto_folder, Some(dir.path("watch")), "the folder stays");
    let r = h.app.session.execute("library.autoImportScan", &serde_json::json!({})).expect("scan");
    assert!(r["folder"].is_null(), "no scanning while off: {r}");
    toggle(&mut h);
    assert_eq!(checked(&h), Some(true));
}

/// Settings written before the switch existed (a folder, no flag) keep watching.
#[test]
fn old_auto_import_settings_still_watch() {
    let d: lcc_engine::import::ImportDefaults = serde_json::from_str(r#"{"autoFolder":"/x","autoCopy":true}"#).expect("loads");
    assert_eq!(d.auto_active_folder().map(String::as_str), Some("/x"));
    let off: lcc_engine::import::ImportDefaults = serde_json::from_str(r#"{"autoFolder":"/x","autoPaused":true}"#).expect("loads");
    assert!(off.auto_active_folder().is_none() && off.auto_folder.is_some());
}
