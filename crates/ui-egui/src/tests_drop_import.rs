//! Files dragged from the file manager onto the window: while they hover, the window says what
//! dropping will do; dropping photos or folders opens the import review (unless Settings ▸ Import
//! says to import dropped files directly); a drop while a dialog is open is refused.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

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
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// An empty library, a folder `trip` with two photos, a window big enough for the overlay.
fn app(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 900.0], 1.0);
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

#[test]
fn hovering_photos_says_they_will_be_reviewed_and_lets_go_when_they_leave() {
    let dir = Scratch::new("hover");
    let mut h = app(&dir);
    assert!(!overlay(&h), "nothing is dragged over the window");
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], true);
    assert!(overlay(&h), "the window shows where to drop");
    assert!(said(&h, "review 2 items"), "{:?}", h.painted_text());
    drop_files(&mut h, &[], true);
    assert!(!overlay(&h), "the files left the window");
}

#[test]
fn hovering_presets_says_they_will_be_imported_as_presets() {
    let dir = Scratch::new("hover-presets");
    let mut h = app(&dir);
    std::fs::write(dir.path("warm.lcpreset"), "{}").unwrap();
    drop_files(&mut h, &[dir.path("warm.lcpreset")], true);
    assert!(overlay(&h));
    assert!(said(&h, "1 preset"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "presets are not reviewed");
}

#[test]
fn hovering_unknown_files_says_only_that_dropping_imports() {
    let dir = Scratch::new("hover-unknown");
    let mut h = app(&dir);
    drop_files(&mut h, &[String::new()], true);
    assert!(overlay(&h));
    assert!(said(&h, "Drop to import"), "{:?}", h.painted_text());
}

#[test]
fn dropping_a_folder_opens_the_import_review_and_imports_nothing_yet() {
    let dir = Scratch::new("review");
    let mut h = app(&dir);
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2);
    assert_eq!(photos(&h), 0, "nothing is imported until the review is confirmed");
    assert!(!overlay(&h), "the files were let go");
}

#[test]
fn dropping_imports_directly_when_settings_say_so() {
    let dir = Scratch::new("direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    drop_files(&mut h, &[dir.path("trip")], false);
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
    assert!(h.app.ui.toast.is_some(), "the person is told why");
}

/// A prompt owns the library and the top layer: a drop while it is up stays queued (it is not
/// imported behind the prompt), and the drop-hover card is not painted above it.
#[test]
fn a_drop_and_its_hover_card_wait_behind_a_notice_prompt() {
    let dir = Scratch::new("prompt");
    let mut h = app(&dir);
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    h.step();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:noticeOk"), "the notice is laid out");
    drop_files(&mut h, &[dir.path("trip")], true);
    assert!(!overlay(&h), "the drop-hover Tooltip layer must not cover the prompt");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:noticeOk"), "the prompt still owns the window");
    drop_files(&mut h, &[dir.path("trip")], false);
    assert_eq!(photos(&h), 0, "nothing was imported behind the prompt");
    assert!(h.app.ui.dialog.is_none() && h.app.import.is_none() && h.app.scan.is_none(), "the drop was not handled yet");
    assert_eq!(h.app.dropped_files.len(), 1, "the dropped folder is queued until the prompt is gone");
    assert_eq!(h.app.notices.len(), 1, "the notice is still up");
    let r = h.request("ui.clickWidget", json!({"id": "button:noticeOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.notices.is_empty(), "OK dismisses the notice");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the queued drop opens its review after the prompt");
    assert_eq!(photos(&h), 0, "the review still imports nothing before it is confirmed");
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
    h.app.ui.settings.import_dropped_directly = true;
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
    assert!(h.app.ui.toast.is_some(), "the person is told why");
    assert_eq!(photos(&h), 0);
}

/// An import that is running stays the one that runs: a direct drop doesn't start a second.
#[test]
fn a_direct_drop_while_an_import_runs_is_refused_too() {
    let dir = Scratch::new("running-direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    write_png(&dir.path("more/c.png"), 3);
    let _scan = crate::import::stall_scan(&mut h.app);
    drop_files(&mut h, &[dir.path("more")], false);
    assert!(h.app.import.is_none(), "no second import");
    assert!(h.app.ui.toast.is_some());
}

/// A Local folder browse counts too: a review-first drop's OK would replace it, and a direct drop
/// would replace it at once.
#[test]
fn a_drop_while_a_browse_runs_is_refused_in_both_modes() {
    for direct in [false, true] {
        let dir = Scratch::new("running-browse");
        let mut h = app(&dir);
        h.app.ui.settings.import_dropped_directly = direct;
        write_png(&dir.path("more/c.png"), 3);
        let _browse = crate::import::stall_browse(&mut h.app, &dir.path("local"));
        drop_files(&mut h, &[dir.path("more/c.png")], true);
        assert!(said(&h, "An import is running"), "{:?}", h.painted_text());
        drop_files(&mut h, &[dir.path("more")], false);
        for _ in 0..30 {
            h.step();
        }
        assert!(h.app.ui.dialog.is_none() && h.app.scan.is_none(), "direct={direct}: nothing started");
        assert!(h.app.import.as_ref().and_then(|t| t.folder()).is_some(), "direct={direct}: the browse was left alone");
        assert!(h.app.ui.toast.is_some(), "the person is told why");
    }
}

/// While a Lightroom catalog import is pending the drop is refused, and the card says so instead of
/// promising a review.
#[test]
fn hovering_photos_while_a_lightroom_import_is_pending_says_they_will_be_refused() {
    let dir = Scratch::new("lightroom");
    let mut h = app(&dir);
    let _task = crate::lightroom_import::stall_import(&mut h.app);
    drop_files(&mut h, &[dir.path("trip")], true);
    assert!(said(&h, "Finish Lightroom catalog import"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    assert!(h.app.ui.toast.is_some(), "the person is told why");
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
    assert!(said(&h, "once a library is open"), "{:?}", h.painted_text());
    assert!(said(&h, "drop the presets again") && !said(&h, "1 preset"), "{:?}", h.painted_text());
    drop_files(&mut h, &[], true);
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], false);
    assert_eq!(h.app.library_problem.as_ref().map(|p| p.pending_import.len()), Some(2));
    assert!(h.app.ui.dialog.is_none() && h.app.import.is_none(), "no review, no import into the temporary session");
    assert_eq!(photos(&h), 0);
    // a preset belongs to a library: refused, not imported into the session the library replaces
    h.app.ui.toast = None;
    drop_files(&mut h, &[dir.path("warm.lcpreset")], false);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("drop the presets again"), "{toast}");
}
