//! Files dragged from the file manager onto the window: while they hover, the window says what
//! dropping will do; dropping photos or folders opens the import review (unless Settings ▸ Import
//! says to import dropped files directly); a drop while a dialog is open is refused.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::pick::PickRequest;
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

/// A folder dialog that answers "cancelled": enough to make the launch list's New Library button
/// and its name prompt available.
fn services() -> Services {
    Services { png: None, pick_folder: Some(Box::new(|| -> Option<String> { None })), ..Default::default() }
}

/// An empty library, a folder `trip` with two photos, a window big enough for the overlay.
fn app(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, services()), [1300.0, 900.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    h
}

/// Folder dialogs the host shows "elsewhere": each request is recorded with the sender that
/// answers it, so a test can cancel or choose after the app has moved on to its next UI pass.
type Asked = Arc<Mutex<Vec<(PickRequest, Sender<Vec<String>>)>>>;

/// A no-library window whose New Library and Open Another… folder dialogs are asynchronous, as on
/// the desktop hosts that run pickers off the UI thread (and in the browser).
fn app_with_picker(dir: &Scratch) -> (Headless, Asked) {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let session = lightcraft_engine::Session::new().with_fs();
    let mut app = LightcraftApp::new(session, services());
    let asked: Asked = Default::default();
    let a = asked.clone();
    app.services.picker = Some(Box::new(move |req| {
        let (tx, rx) = std::sync::mpsc::channel();
        a.lock().unwrap().push((req, tx));
        Ok(rx)
    }));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    (h, asked)
}

fn asked_dialogs(asked: &Asked) -> usize {
    asked.lock().unwrap().len()
}

/// Answer the oldest folder dialog: `paths` empty cancels it.
fn answer_picker(asked: &Asked, paths: Vec<String>) {
    let sender = asked.lock().unwrap().remove(0).1;
    sender.send(paths).unwrap();
}

/// A library folder that can be chosen by Open Another…, made outside the no-library app.
fn library_at(dir: &Scratch, rel: &str) -> String {
    let path = dir.path(rel);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&path, false).unwrap();
    session.close_library().unwrap();
    path
}

fn drop_files(h: &mut Headless, paths: &[String], hover: bool) {
    let r = h.request("ui.dropFiles", json!({"paths": paths, "hover": hover}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
}

/// The same window with a library already open, so a launch list over it is the real "over"
/// case rather than a list with no library behind it.
fn app_with_library(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.path("library"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, services()), [1300.0, 900.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    h
}

/// Draw the launch list (`over_open_library`: Alt-at-launch over the library that opened, else the
/// list with no library behind it), then wait until it holds still. Each recent library's row says
/// "Checking…" until a worker answers; the answer drops that line, so the centred window shrinks and
/// its buttons move. A click aimed before then can find its button gone from under the pointer by
/// the release, and `ui.clickWidget` refuses it.
fn draw_launch_list(h: &mut Headless, over_open_library: bool) {
    let mut problem = crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings);
    if over_open_library && let Some(c) = problem.chooser.as_mut() {
        c.over = Some(h.app.session.library_generation());
    }
    h.app.library_problem = Some(problem);
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| w == "button:libraryOpen")), "the launch list is drawn");
    let answered =
        |h: &Headless| h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).is_some_and(|c| c.libraries.iter().all(|l| !l.checking));
    assert!(h.step_until(T, answered), "every recent library is checked");
    wait_until_still(h, "button:libraryOpen");
}

/// Run frames until widget `id` keeps its rect from one frame to the next (an anchored window
/// takes a frame to move to its new size).
fn wait_until_still(h: &mut Headless, id: &str) {
    let rect = |h: &Headless| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let mut last = rect(h);
    for _ in 0..60 {
        h.step();
        let now = rect(h);
        if now.is_some() && now == last {
            return;
        }
        last = now;
    }
    panic!("{id} never held still");
}

/// The launch list as Alt-at-launch shows it over the library that opened.
fn show_launch_list(h: &mut Headless) {
    draw_launch_list(h, true);
    assert!(crate::panels::library_problem::blocks_file_drops(&h.app), "the launch list owns the drop barrier");
}

/// The same list with no library open: photos still go through the deliberate hold path.
fn show_no_library_launch_list(h: &mut Headless) {
    draw_launch_list(h, false);
    assert!(!crate::panels::library_problem::blocks_file_drops(&h.app), "with no library open the drop path runs");
}

fn user_presets(h: &Headless) -> usize {
    h.app.session.presets.iter().filter(|p| !p.builtin).count()
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

/// A photo dropped while the launch list is over an already open library must wait in the queue:
/// importing it behind the list would add it to the library the list is asking to replace or keep.
#[test]
fn a_photo_drop_waits_behind_the_launch_list_over_an_open_library() {
    let dir = Scratch::new("chooser-photo");
    let mut h = app_with_library(&dir);
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], true);
    assert!(!overlay(&h), "the drop card must not promise an import above the launch list");
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert!(h.app.ui.dialog.is_none(), "no import review opens behind the launch list");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "no scan or import starts behind the launch list");
    assert_eq!(photos(&h), 0, "nothing is imported behind the launch list");
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the launch list to go");
    // Esc keeps the library that is open (the launch list's own Keep), then the queued drop runs
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "the launch list closes");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the queued drop opens its review");
    assert_eq!(photos(&h), 0, "the review still imports nothing before it is confirmed");
    assert!(h.app.dropped_files.is_empty(), "the queued drop was consumed once the list was gone");
}

/// Preset drops run a command directly (no review), so the barrier must hold those too: a preset
/// imported behind the list would change the open library the person has not chosen to keep.
#[test]
fn a_preset_drop_waits_behind_the_launch_list_over_an_open_library() {
    let dir = Scratch::new("chooser-preset");
    let mut h = app_with_library(&dir);
    std::fs::write(dir.path("warm.lcpreset"), r#"{"id": "user.warm", "name": "Warm", "group": "Test", "settings": {"light": {"exposure": 0.5}}}"#)
        .unwrap();
    let before = user_presets(&h);
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("warm.lcpreset")], false);
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(user_presets(&h), before, "no preset is imported behind the launch list");
    assert_eq!(h.app.dropped_files.len(), 1, "the preset waits for the launch list to go");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "Keep closes the launch list");
    assert!(h.step_until(SETTLE, |h| user_presets(h) == before + 1), "the queued preset imports once the list is gone");
    assert!(h.app.dropped_files.is_empty());
}

/// A queued drop must survive the launch list hiding itself behind its own New Library name prompt:
/// that prompt does not make the list stop owning the library. Cancelling the prompt brings the
/// list back; only after Keep (Esc) does the drop import.
#[test]
fn a_queued_photo_drop_survives_the_choosers_new_library_prompt_and_cancel() {
    let dir = Scratch::new("chooser-new-cancel");
    let mut h = app_with_library(&dir);
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the launch list");
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::TextPrompt { .. }))), "New Library asks for a name");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits behind the name prompt too");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TextPrompt { .. })), "the prompt stays open");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "nothing starts behind the prompt");
    assert_eq!(photos(&h), 0);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.ui.dialog.is_none()), "Cancel closes the name prompt");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:libraryOpen"), "the launch list is back");
    assert_eq!(h.app.dropped_files.len(), 1, "cancel kept the queued drop");
    assert!(h.app.ui.dialog.is_none(), "no review opens behind the returning list");
    assert_eq!(photos(&h), 0);
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "Keep closes the launch list");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the queued drop opens its review");
    assert!(h.app.dropped_files.is_empty(), "the queued drop was consumed once the list was gone");
}

/// With no library open a drop during the list's New Library name prompt would once have hit the
/// generic "close the open dialog" refusal and been lost. It now waits for the prompt to close,
/// then goes through the deliberate paths: photos are held for the library chosen, presets refused.
#[test]
fn a_drop_during_the_no_library_name_prompt_is_held_or_refused_after_cancel() {
    let dir = Scratch::new("newlib-prompt-drop");
    std::fs::write(dir.path("warm.lcpreset"), r#"{"id": "user.warm", "name": "Warm", "group": "Test", "settings": {"light": {"exposure": 0.5}}}"#)
        .unwrap();
    let mut h = app(&dir);
    show_no_library_launch_list(&mut h);
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::TextPrompt { .. }))), "New Library asks for a name");
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("warm.lcpreset")], false);
    assert_eq!(h.app.dropped_files.len(), 2, "the drop waits behind the name prompt");
    assert_eq!(h.app.library_problem.as_ref().map(|p| p.pending_import.len()), Some(0), "nothing is held behind the prompt");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.ui.dialog.is_none()), "Cancel closes the name prompt");
    assert!(h.step_until(SETTLE, |h| h.app.dropped_files.is_empty()), "the drop is consumed once the prompt is gone");
    assert_eq!(
        h.app.library_problem.as_ref().map(|p| p.pending_import.clone()),
        Some(vec![dir.path("trip/a.png")]),
        "the photo waits for the library chosen"
    );
    assert_eq!(user_presets(&h), 0, "the preset is refused, not imported into the temporary session");
    assert_eq!(photos(&h), 0, "nothing is imported into the temporary session");
}

/// The no-library New Library flow asks for a name, then for a place on the host's asynchronous
/// folder picker. A drop queued during either step waits: it must not be consumed before the new
/// library exists. Confirming the picker opens the library and the queued photo imports into it.
#[test]
fn a_queued_photo_drop_survives_the_async_new_library_picker_and_imports_on_confirmation() {
    let dir = Scratch::new("newlib-async-confirm");
    let (mut h, asked) = app_with_picker(&dir);
    show_no_library_launch_list(&mut h);
    h.app.ui.settings.import_dropped_directly = true;
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::TextPrompt { .. }))), "New Library asks for a name");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.pending_picks.len() == 1), "the folder picker is up");
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(asked_dialogs(&asked), 1, "one asynchronous folder dialog was started");
    assert!(h.app.session.library.is_none(), "no library exists until the picker answers");
    assert!(crate::panels::library_problem::blocks_file_drops(&h.app), "the folder picker still owns the drop barrier");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits through the picker");
    assert!(h.app.ui.dialog.is_none(), "the name prompt is gone; only the picker is up");
    assert_eq!(h.app.library_problem.as_ref().map(|p| p.pending_import.len()), Some(0), "nothing is held while the picker is still open");
    assert!(h.app.scan.is_none() && h.app.import.is_none() && photos(&h) == 0, "nothing starts behind the picker");

    let parent = dir.path("new-library-parent");
    std::fs::create_dir_all(&parent).unwrap();
    answer_picker(&asked, vec![parent]);
    assert!(h.step_until(SETTLE, |h| h.app.pending_picks.is_empty()), "the picker answered");
    assert!(h.step_until(SETTLE, |h| h.app.session.library.is_some()), "the new library opens");
    assert!(
        h.step_until(SETTLE, |h| h.app.dropped_files.is_empty() && photos(h) == 1 && h.app.import.is_none()),
        "the queued photo imports into the new library"
    );
    assert_eq!(user_presets(&h), 0);
}

/// Cancelling the asynchronous place picker leaves the launch list up. The queued photo then takes
/// the no-library hold path (kept for the library chosen) instead of being lost behind the picker.
#[test]
fn a_queued_drop_survives_the_async_new_library_picker_cancel_then_takes_the_no_library_path() {
    let dir = Scratch::new("newlib-async-cancel");
    std::fs::write(dir.path("warm.lcpreset"), r#"{"id": "user.warm", "name": "Warm", "group": "Test", "settings": {"light": {"exposure": 0.5}}}"#)
        .unwrap();
    let (mut h, asked) = app_with_picker(&dir);
    show_no_library_launch_list(&mut h);
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::TextPrompt { .. }))), "New Library asks for a name");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.pending_picks.len() == 1), "the folder picker is up");
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("warm.lcpreset")], false);
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 2, "both files wait through the picker");
    assert!(h.app.session.library.is_none());

    answer_picker(&asked, vec![]);
    assert!(h.step_until(SETTLE, |h| h.app.pending_picks.is_empty()), "the picker was cancelled");
    assert!(h.step_until(SETTLE, |h| h.app.dropped_files.is_empty()), "the drop takes the no-library path now");
    assert!(h.app.session.library.is_none(), "cancel created no library");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:libraryOpen"), "the launch list is still up");
    assert_eq!(
        h.app.library_problem.as_ref().map(|p| p.pending_import.clone()),
        Some(vec![dir.path("trip/a.png")]),
        "the photo waits for the library chosen"
    );
    assert_eq!(user_presets(&h), 0, "the preset is refused with no library");
    assert_eq!(photos(&h), 0, "nothing is imported into the temporary session");
}

/// Open Another… has the same asynchronous folder picker with no library open. A queued drop must
/// wait for it, then import into the library the person opens.
#[test]
fn a_queued_photo_drop_survives_the_async_open_library_picker_and_imports_on_confirmation() {
    let dir = Scratch::new("open-async-confirm");
    let target = library_at(&dir, "chosen-library");
    let (mut h, asked) = app_with_picker(&dir);
    show_no_library_launch_list(&mut h);
    h.app.ui.settings.import_dropped_directly = true;
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.pending_picks.len() == 1), "the folder picker is up");
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(asked_dialogs(&asked), 1);
    assert!(h.app.session.library.is_none());
    assert!(crate::panels::library_problem::blocks_file_drops(&h.app), "the folder picker still owns the drop barrier");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits through the picker");

    answer_picker(&asked, vec![target.clone()]);
    assert!(h.step_until(SETTLE, |h| h.app.pending_picks.is_empty()), "the picker answered");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "opening the library ends the launch list");
    assert!(
        h.step_until(SETTLE, |h| h.app.dropped_files.is_empty() && photos(h) == 1 && h.app.import.is_none()),
        "the queued photo imports into the opened library"
    );
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(target));
}

/// A library opened from the asynchronous picker can start files that were already waiting in
/// `pending_import` (the command line, an earlier no-library drop). A drop queued while the picker
/// was up must not be handed to the ordinary drop path while that import runs: it waits for the
/// pending import to end and is imported too, instead of being refused and lost.
#[test]
fn a_queued_drop_waits_for_the_pending_import_started_when_the_picker_opens_the_library() {
    let dir = Scratch::new("pending-import-picker");
    let target = library_at(&dir, "chosen-library");
    let (mut h, asked) = app_with_picker(&dir);
    show_no_library_launch_list(&mut h);
    h.app.ui.settings.import_dropped_directly = true;
    let pending = dir.path("trip/a.png");
    h.app.library_problem.as_mut().unwrap().pending_import.push(pending);

    let r = h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.pending_picks.len() == 1), "the folder picker is up");
    drop_files(&mut h, &[dir.path("trip/b.png")], false);
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits through the picker");
    assert!(h.app.session.library.is_none());

    answer_picker(&asked, vec![target.clone()]);
    assert!(h.step_until(SETTLE, |h| h.app.pending_picks.is_empty()), "the picker answered");
    assert!(h.step_until(SETTLE, |h| photos(h) == 1), "the pending import arrives first");
    assert!(h.app.import.is_some(), "the pending import still runs when the drop is released");
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the pending import instead of being refused");
    assert!(
        h.step_until(SETTLE, |h| photos(h) == 2 && h.app.dropped_files.is_empty() && h.app.import.is_none()),
        "both the pending file and the queued drop are imported (photos: {}, queued: {:?})",
        photos(&h),
        h.app.dropped_files.len()
    );
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(target));
}

/// A drop queued behind the launch list may find another dialog open when the list goes (the
/// person opened one while the barrier was up). It waits for that dialog instead of taking the
/// ordinary "close the open dialog" refusal, so it is not lost.
#[test]
fn a_queued_drop_waits_for_a_dialog_opened_before_the_launch_list_closes() {
    let dir = Scratch::new("deferred-dialog");
    let mut h = app_with_library(&dir);
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the launch list");
    h.app.library_problem = None;
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    for _ in 0..10 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the dialog stays");
    assert_eq!(h.app.dropped_files.len(), 1, "the deferred drop is not refused while the dialog is open");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "nothing starts behind the dialog");
    h.app.ui.dialog = None;
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the drop opens its review once the dialog is closed");
    assert!(h.app.dropped_files.is_empty(), "the deferred drop was consumed only when safe");
}

/// A drop queued behind the launch list may be released while a Synchronize Folder runs. It
/// waits for that run to finish instead of taking `import::start_paths`'s "A folder is being
/// synchronized" refusal and being lost. A drop made while no barrier was up still gets that
/// refusal (see `tests_sync::a_synchronize_and_an_import_never_run_at_once`).
#[test]
fn a_queued_drop_waits_for_a_running_synchronize_instead_of_being_refused() {
    let dir = Scratch::new("deferred-sync");
    let mut h = app_with_library(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the launch list");
    let _run = crate::sync::stall_run(&mut h.app, "trip");
    assert!(h.app.sync_run.is_some(), "the synchronize is running");
    h.app.library_problem = None;
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.sync_run.is_some(), "the synchronize still runs");
    assert_eq!(h.app.dropped_files.len(), 1, "the deferred drop waits for the running synchronize instead of being refused");
    assert!(!said(&h, "A folder is being synchronized"), "the deferred drop is not refused: {:?}", h.painted_text());
    assert_eq!(photos(&h), 0, "nothing is imported during the synchronize");
    h.app.sync_run = None;
    assert!(
        h.step_until(SETTLE, |h| h.app.dropped_files.is_empty() && photos(h) == 1 && h.app.import.is_none()),
        "the queued drop imports once the synchronize is done (queued: {:?}, photos: {})",
        h.app.dropped_files,
        photos(&h)
    );
}

/// The same for a Lightroom catalog import: a drop queued behind the launch list waits while the
/// task owns the catalog, instead of being refused with "Finish Lightroom catalog import…".
#[test]
fn a_queued_drop_waits_for_a_running_lightroom_import_instead_of_being_refused() {
    let dir = Scratch::new("deferred-lightroom");
    let mut h = app_with_library(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the launch list");
    let task = crate::lightroom_import::stall_import(&mut h.app);
    h.app.library_problem = None;
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.lightroom.is_some(), "the Lightroom import still runs");
    assert_eq!(h.app.dropped_files.len(), 1, "the deferred drop waits for the Lightroom import instead of being refused");
    assert!(!said(&h, "Finish Lightroom catalog import"), "the deferred drop is not refused: {:?}", h.painted_text());
    assert_eq!(photos(&h), 0, "nothing is imported during the Lightroom import");
    drop(task);
    assert!(h.step_until(SETTLE, |h| h.app.lightroom.is_none()), "the Lightroom task ends");
    // the stub's disconnect reports a stopped task and opens its result window; close it so the
    // queued drop is released (a real import's result window is another dialog the guard waits for)
    h.app.ui.dialog = None;
    assert!(
        h.step_until(SETTLE, |h| h.app.dropped_files.is_empty() && photos(h) == 1 && h.app.import.is_none()),
        "the queued drop imports once the Lightroom task is done (queued: {:?}, photos: {})",
        h.app.dropped_files,
        photos(&h)
    );
}

/// Cancelling Open Another…'s asynchronous picker keeps the launch list up; the queued photo is
/// then held for the library chosen rather than dropped behind the cancelling picker.
#[test]
fn a_queued_photo_drop_survives_the_async_open_library_picker_cancel() {
    let dir = Scratch::new("open-async-cancel");
    let (mut h, asked) = app_with_picker(&dir);
    show_no_library_launch_list(&mut h);
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.pending_picks.len() == 1), "the folder picker is up");
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits through the picker");

    answer_picker(&asked, vec![]);
    assert!(h.step_until(SETTLE, |h| h.app.pending_picks.is_empty()), "the picker was cancelled");
    assert!(h.step_until(SETTLE, |h| h.app.dropped_files.is_empty()), "the drop takes the no-library path now");
    assert!(h.app.session.library.is_none());
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:libraryOpen"), "the launch list is still up");
    assert_eq!(
        h.app.library_problem.as_ref().map(|p| p.pending_import.clone()),
        Some(vec![dir.path("trip/a.png")]),
        "the photo waits for the library chosen"
    );
    assert_eq!(photos(&h), 0);
}

/// A valid face-model drop on a no-library launch list or error window must not open its licence
/// dialog behind that window; it is refused like a preset, while photos still take the hold path.
#[test]
fn a_model_drop_on_a_no_library_window_does_not_open_its_dialog_behind_it() {
    let dir = Scratch::new("model-window");
    std::fs::write(dir.path("face.onnx"), lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
    for (tag, problem) in [
        ("error", crate::panels::library_problem::LibraryProblem::new(dir.path("Gone"), "not found")),
        ("chooser", crate::panels::library_problem::LibraryProblem::choose_later(&crate::state::AppSettings::default())),
    ] {
        let mut h = app(&dir);
        h.app.library_problem = Some(problem);
        for _ in 0..4 {
            h.step();
        }
        drop_files(&mut h, &[dir.path("face.onnx")], true);
        assert!(said(&h, "drop the model again"), "{tag}: the card says the model waits: {:?}", h.painted_text());
        drop_files(&mut h, &[dir.path("face.onnx")], false);
        for _ in 0..10 {
            h.step();
        }
        assert!(h.app.ui.dialog.is_none(), "{tag}: no face model dialog behind the window");
        let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains("drop the model again"), "{tag}: {toast}");
    }
}

/// A model dropped over the launch list is held like a photo/preset, then opens its licence dialog
/// once the list is kept away.
#[test]
fn a_model_drop_waits_behind_the_launch_list_and_opens_after_keep() {
    let dir = Scratch::new("chooser-model");
    std::fs::write(dir.path("face.onnx"), lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
    let mut h = app_with_library(&dir);
    h.app.session.face_models_dir = Some(std::path::PathBuf::from(dir.path("models")));
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("face.onnx")], false);
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "no licence dialog opens behind the launch list");
    assert_eq!(h.app.dropped_files.len(), 1, "the model waits for the launch list to go");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "Keep closes the launch list");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::FaceModel { .. }))), "the queued model opens its licence dialog");
    assert!(h.app.dropped_files.is_empty());
}

/// A failed switch leaves the launch list showing its error over the library that stayed open
/// (`Chooser { over: Some(generation), .. }`, not a separate error window). A queued drop must wait
/// there too: importing it would change the library the person has not chosen to keep.
#[test]
fn a_photo_drop_waits_behind_the_choosers_failed_switch_error_until_keep() {
    let dir = Scratch::new("failed-switch");
    let mut h = app_with_library(&dir);
    let mut problem = crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings);
    let generation = h.app.session.library_generation();
    let Some(c) = problem.chooser.as_mut() else { panic!("the launch list has its chooser") };
    c.over = Some(generation);
    problem.path = dir.path("Gone");
    problem.error = format!("{}: the library is no longer there", dir.path("Gone"));
    h.app.library_problem = Some(problem);
    assert!(
        h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| w == "button:libraryOpen")),
        "the launch list is drawn over the library that stayed open"
    );
    assert!(said(&h, "no longer there"), "the failed switch's error is shown: {:?}", h.painted_text());
    assert!(crate::panels::library_problem::blocks_file_drops(&h.app), "the failed-switch list owns the drop barrier");
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1, "the drop waits for the failed-switch list");
    assert!(h.app.ui.dialog.is_none(), "no import review opens behind the list");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "nothing starts behind the list");
    assert_eq!(photos(&h), 0);
    // Esc is the list's Keep: it closes the list and leaves the library that stayed open.
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.library_problem.is_none()), "Keep closes the launch list");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the queued drop opens after Keep");
    assert!(h.app.dropped_files.is_empty());
    assert_eq!(photos(&h), 0, "the review still imports nothing before it is confirmed");
}

/// A drop queued while the launch list is up belongs to the library the person then opens with
/// its Open button: the switch must not drop it, and it imports into the new library.
#[test]
fn a_queued_photo_drop_imports_into_the_library_opened_from_the_list() {
    let dir = Scratch::new("switch-queue");
    // Made (and flushed) by a session that is dropped before the app opens it: `close_library`
    // saves but keeps the library and its OS lock until the session is dropped, and the launch
    // list's Open would then be refused with "already open" by this same process's other descriptor.
    let second = library_at(&dir, "second");
    let mut h = app_with_library(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    h.app.ui.settings.library_path = dir.path("library");
    h.app.ui.settings.recent_libraries = vec![dir.path("library"), second.clone()];
    show_launch_list(&mut h);
    drop_files(&mut h, &[dir.path("trip/a.png")], false);
    assert_eq!(h.app.dropped_files.len(), 1);
    let r = h.request("ui.clickWidget", json!({"id": "libraryRecent:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.library_problem.as_ref().and_then(|p| p.chooser.as_ref()).and_then(|c| c.selected), Some(1), "the second library is selected");
    let r = h.request("ui.clickWidget", json!({"id": "button:libraryOpen"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        h.step_until(SETTLE, |h| h.app.library_problem.is_none()),
        "the switch ends the launch list; problem: {:?}",
        h.app.library_problem.as_ref().map(|p| (&p.path, &p.error))
    );
    assert!(
        h.step_until(SETTLE, |h| h.app.import.is_none() && photos(h) == 1),
        "the queued drop imports into the opened library (queued: {:?}, photos: {}, import running: {})",
        h.app.dropped_files,
        photos(&h),
        h.app.import.is_some()
    );
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(second));
    assert!(h.app.dropped_files.is_empty());
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

/// A Local folder browse scan counts too: a review-first drop would be rejected by `open`, and a
/// direct drop would be replaced when the browse completes.
#[test]
fn a_drop_while_a_browse_scan_runs_is_refused_in_both_modes() {
    for direct in [false, true] {
        let dir = Scratch::new("running-browse");
        let mut h = app(&dir);
        h.app.ui.settings.import_dropped_directly = direct;
        write_png(&dir.path("more/c.png"), 3);
        let _scan = crate::import::stall_scan_of(&mut h.app, true);
        drop_files(&mut h, &[dir.path("more/c.png")], true);
        assert!(said(&h, "An import is running"), "{:?}", h.painted_text());
        drop_files(&mut h, &[dir.path("more")], false);
        for _ in 0..30 {
            h.step();
        }
        assert!(h.app.ui.dialog.is_none() && h.app.import.is_none(), "direct={direct}: nothing started");
        assert!(h.app.scan.is_some(), "the browse was left alone");
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

/// Finish the protected batch through the ordinary review/direct path, checking exact provenance.
fn finish_mixed_drop_batch(h: &mut Headless, expected: &[String], direct: bool) {
    if !direct {
        assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the protected batch reaches review");
        let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        let mut actual = opts.candidates.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        actual.sort();
        let mut wanted = expected.to_vec();
        wanted.sort();
        assert_eq!(actual, wanted, "review contains exactly the protected arrivals");
        assert_eq!(photos(h), 0, "review mode still requires confirmation");
        let reply = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(reply["ok"], true, "{reply}");
    }
    assert!(h.step_until(SETTLE, |h| photos(h) == expected.len() && !crate::import::running(&h.app)), "the protected batch imports");
    let mut paths = h
        .app
        .session
        .catalog
        .photos()
        .map(|p| match &p.source {
            lightcraft_catalog::Source::File { path } => path.clone(),
            other => panic!("unexpected imported source: {other:?}"),
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut wanted = expected.to_vec();
    wanted.sort();
    assert_eq!(paths, wanted, "only protected files entered the catalog");
    assert!(h.app.dropped_files.is_empty(), "the protected arrivals were consumed");
    let revision = h.app.session.catalog.revision;
    // Inspect every later pass, not only the final catalog count: duplicate import suppression
    // must not disguise replay of a consumed raw-input drop.
    for _ in 0..60 {
        h.step();
        assert!(!crate::import::running(&h.app), "no replay starts another scan/import");
        assert!(h.app.ui.dialog.is_none(), "no replay opens another review");
        assert!(h.app.dropped_files.is_empty());
        assert_eq!(photos(h), expected.len());
        assert_eq!(h.app.session.catalog.revision, revision, "no later import mutates the catalog");
    }
}

#[test]
fn a_new_drop_outside_the_barrier_is_refused_while_an_older_drop_waits_for_a_dialog() {
    for direct in [false, true] {
        let dir = Scratch::new("mixed-provenance-dialog");
        let mut h = app_with_library(&dir);
        h.app.ui.settings.import_dropped_directly = direct;
        let a = dir.path("trip/a.png");
        let b = dir.path("trip/b.png");
        show_launch_list(&mut h);
        drop_files(&mut h, std::slice::from_ref(&a), false);
        assert_eq!(h.app.dropped_files.len(), 1);
        h.app.library_problem = None;
        h.app.ui.dialog = Some(Dialog::WhatsNew);
        h.step();
        assert!(!crate::panels::library_problem::blocks_file_drops(&h.app));
        assert!(!crate::panels::notices::blocks_input(&h.app));
        drop_files(&mut h, &[b], false);
        assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the ordinary dialog survives B");
        assert!(h.app.scan.is_none() && h.app.import.is_none());
        assert_eq!(photos(&h), 0);
        assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Close the open dialog")), "B is refused immediately: {:?}", h.app.ui.toast);
        assert_eq!(h.app.dropped_files.len(), 1, "B cannot inherit A's protected provenance");
        h.app.ui.dialog = None;
        finish_mixed_drop_batch(&mut h, &[a], direct);
    }
}

#[test]
fn a_new_drop_outside_the_barrier_is_refused_while_an_older_drop_waits_for_an_import() {
    for direct in [false, true] {
        let dir = Scratch::new("mixed-provenance-import");
        let mut h = app_with_library(&dir);
        h.app.ui.settings.import_dropped_directly = direct;
        let a = dir.path("trip/a.png");
        let b = dir.path("trip/b.png");
        show_launch_list(&mut h);
        drop_files(&mut h, std::slice::from_ref(&a), false);
        let stalled = crate::import::stall_scan(&mut h.app);
        h.app.library_problem = None;
        h.step();
        assert!(!crate::panels::library_problem::blocks_file_drops(&h.app));
        assert!(!crate::panels::notices::blocks_input(&h.app));
        drop_files(&mut h, &[b], false);
        assert!(h.app.scan.is_some() && h.app.import.is_none(), "the original stalled import scan survives B");
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(photos(&h), 0);
        assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("An import is running")), "B is refused immediately: {:?}", h.app.ui.toast);
        assert_eq!(h.app.dropped_files.len(), 1, "B cannot inherit A's protected provenance");
        drop(stalled); // Channel-only fixture: no worker/process to stop.
        assert!(h.step_until(T, |h| h.app.scan.is_none()), "the original stalled scan ends");
        finish_mixed_drop_batch(&mut h, &[a], direct);
    }
}

#[test]
fn both_drops_really_made_behind_the_barrier_remain_protected() {
    for direct in [false, true] {
        let dir = Scratch::new("mixed-provenance-both-protected");
        let mut h = app_with_library(&dir);
        h.app.ui.settings.import_dropped_directly = direct;
        let a = dir.path("trip/a.png");
        let b = dir.path("trip/b.png");
        show_launch_list(&mut h);
        drop_files(&mut h, std::slice::from_ref(&a), false);
        drop_files(&mut h, std::slice::from_ref(&b), false);
        assert_eq!(h.app.dropped_files.len(), 2, "both arrivals really occurred behind the barrier");
        h.app.library_problem = None;
        h.app.ui.dialog = Some(Dialog::WhatsNew);
        for _ in 0..10 {
            h.step();
        }
        assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)));
        assert_eq!(h.app.dropped_files.len(), 2, "both protected arrivals survive the later dialog");
        assert!(h.app.scan.is_none() && h.app.import.is_none());
        h.app.ui.dialog = None;
        finish_mixed_drop_batch(&mut h, &[a, b], direct);
    }
}

/// Public take also serves web hosts: barrier reentry preserves order and consuming clears
/// protection, so a later ordinary drop cannot inherit the consumed batch's provenance.
#[test]
fn public_drop_take_preserves_reentered_barrier_order_and_clears_protection() {
    let dir = Scratch::new("mixed-provenance-public-take");
    let mut h = app_with_library(&dir);
    let a = dir.path("trip/a.png");
    let b = dir.path("trip/b.png");
    show_launch_list(&mut h);
    drop_files(&mut h, std::slice::from_ref(&a), false);
    assert_eq!(h.app.dropped_files.len(), 1);
    h.app.library_problem = None;
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.app.dropped_files.len(), 1, "A remains held behind the ordinary dialog");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)));
    // Reenter before any pass without a blocker can consume A.
    h.app.ui.dialog = None;
    show_launch_list(&mut h);
    drop_files(&mut h, std::slice::from_ref(&b), false);
    assert_eq!(h.app.dropped_files.len(), 2);
    assert_eq!(photos(&h), 0);
    assert!(h.app.scan.is_none() && h.app.import.is_none());
    // Match the web ownership boundary without stepping a native consuming pass.
    h.app.library_problem = None;
    let taken = h.app.take_dropped_files();
    let paths = taken.iter().map(|f| egui::DroppedFile::path(f.as_ref()).to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert_eq!(paths, vec![a.clone(), b], "public take preserves arrival order through barrier reentry");
    assert!(h.app.take_dropped_files().is_empty(), "ownership transfers only once");
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    drop_files(&mut h, &[a], false);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)));
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Close the open dialog")), "consumed protection cannot shelter a later drop");
    assert!(h.app.dropped_files.is_empty());
    h.app.ui.dialog = None;
    for _ in 0..60 {
        h.step();
        assert!(h.app.dropped_files.is_empty());
        assert!(!crate::import::running(&h.app));
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(photos(&h), 0, "neither taken handles nor refused arrivals replay");
    }
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

/// The same with a Local folder browse: a browse that finishes replaces `app.import` with its own, so
/// Auto Import doesn't start one beside it.
#[test]
fn auto_import_waits_for_a_browse_that_starts_during_its_listing() {
    use lightcraft_engine::import::{ImportCandidate, ScanOutput};
    let dir = Scratch::new("auto-browse");
    let mut h = app(&dir);
    second_auto_import_listing(&mut h, &dir);
    let scan = crate::import::stall_scan_of(&mut h.app, true);
    apply_auto_import_listing(&mut h);
    assert!(h.app.import.is_none(), "no Auto Import for the browse to replace");
    // the browse finds one photo of its own and imports it
    let Ok(tx) = scan.downcast::<std::sync::mpsc::Sender<ScanOutput>>() else { panic!("the stalled scan's sender") };
    let found = ImportCandidate { path: dir.path("trip/a.png"), ..Default::default() };
    tx.send(ScanOutput { candidates: vec![found], probes: Default::default() }).unwrap();
    drop(tx);
    assert!(h.step_until(T, |h| h.app.import.is_some()), "the browse starts its import");
    assert!(h.step_until(T, |h| h.app.import.is_none()), "and it ends");
    assert!(h.step_until(T, |h| photos(h) == 2), "the browsed photo and the watched one arrive: {}", photos(&h));
    h.app.session.import_defaults.auto_folder = None;
}
