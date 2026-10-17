//! Help ▸ Check for Updates… in the app (see `lcc_engine::update`).
//!
//! Scenarios, in the words of someone updating:
//!
//! * Help ▸ Check for Updates… opens the dialog, which shows this build and asks the release feed.
//! * A newer release is listed with its file, size, install action and notes, and it can be
//!   downloaded, installed or skipped.
//! * Skipping remembers the version, so a start-up check does not ask about it again.
//! * The two settings are in the dialog and are saved.
//! * The start-up check is off until the user turns it on (Settings ▸ General, the welcome dialog
//!   or this dialog), and headless sessions never check at start.
//! * An install that only lays the file out never quits the app, and says what to do instead.
//!
//! The fixtures (a scratch folder, a fake downloaded file) are set up the short way this crate's
//! other UI tests use: a failed `unwrap` there fails the test, which is the point.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    r
}

/// The PNG encoder and file writer a real host provides (`ui.screenshot` needs them).
fn services() -> Services {
    Services {
        png: Some(Box::new(|img: &lcc_raster::Rgba8| {
            lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    }
}

/// The app with an in-memory session (no library, no network) and the start-up check already done.
fn app() -> Headless {
    let mut session = lcc_engine::Session::new();
    // a fresh updater: the settings are the defaults, not whatever this machine's are
    session.updater = lcc_engine::update::Updater::new(None);
    let app = LccApp::new(session, services());
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    // no start-up check: these tests drive the dialog themselves
    h.app.ui.update_checked_at_startup = true;
    h.step();
    h
}

/// Offer `version` as a newer release, as a check would, with its file already downloaded.
fn offer(h: &mut Headless, dir: &Scratch, version: &str, kind: lcc_engine::update::Kind) -> PathBuf {
    use lcc_engine::update::{Kind, Progress, Staged};
    let file = dir.0.join(format!("app-{version}-{}.bin", kind.text()));
    std::fs::write(&file, b"an installer").unwrap();
    let engine_kind = match kind {
        Kind::Msi => lcc_updater::Kind::Msi,
        Kind::Dmg => lcc_updater::Kind::Dmg,
        Kind::AppImage => lcc_updater::Kind::AppImage,
        Kind::Archive => lcc_updater::Kind::Archive,
        Kind::Checksums => lcc_updater::Kind::Checksums,
    };
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let update = lcc_updater::Update {
        current: lcc_updater::Version::current(),
        version: lcc_updater::Version::parse(version).unwrap(),
        title: format!("App {version}"),
        tag: format!("v{version}"),
        asset: lcc_updater::Asset { name: name.clone(), url: "http://127.0.0.1:1/app.bin".into(), size: 12, sha256: None, kind: engine_kind },
        sha256: Some("ab".repeat(32)),
        published: Some("2026-10-12".into()),
        notes: "### Fixed\n- a thing".into(),
        action: "run the installer".into(),
    };
    h.app.session.updater.apply(lcc_updater::Checked::Found(Box::new(update)));
    h.app.session.updater.set_progress(Progress::Ready(Box::new(Staged {
        version: version.into(),
        path: file.clone(),
        bytes: 12,
        sha256: "ab".repeat(32),
        file: name,
        kind,
        notes: "### Fixed\n- a thing".into(),
    })));
    file
}

#[test]
fn the_help_menu_opens_the_dialog_and_it_lists_what_is_available() {
    let dir = Scratch::new("listing");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    assert_eq!(h.app.ui.dialog, Some(Dialog::Update), "the dialog is up");
    // Esc closes the dialog (the window's own Close is registered as the frame's)
    assert!(h.app.widgets.iter().any(|(w, _)| w == "dialog:window"), "the window is laid out");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:updateCheck"));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:updateInstall"), "a downloaded file can be installed");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:updateSkip"));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:dialogOk"), "Close button is present in footer");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:updateAtStartup"));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:updatePrerelease"));
    let text = h.painted_text().join(" ");
    assert!(text.contains("9.9.9"), "the version is shown: {text}");
    assert!(text.contains("is available"), "{text}");
    // Esc ends it, like the other dialogs that only show something
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.step();
    assert_eq!(h.app.ui.dialog, None, "Esc closes the dialog");
}

#[test]
fn update_dialog_close_button_in_footer_dismisses_dialog() {
    let mut h = app();
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    assert_eq!(h.app.ui.dialog, Some(Dialog::Update));
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.dialog, None, "Close button closes the update dialog");
}

#[test]
fn a_startup_check_that_found_a_release_waits_until_the_dialog_is_opened() {
    let dir = Scratch::new("startup");
    let mut h = app();
    // as the start-up path does: the worker's answer is pending, and no window opened by itself
    h.app.ui.update_pending = Some("9.9.9".into());
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.step();
    h.step();
    assert_eq!(h.app.ui.dialog, None, "a start-up check never opens a window");
    assert!(h.app.ui.update_pending.is_none(), "the announcement is made once");
    assert!(h.app.ui.toast.is_some(), "it says so instead");
}

#[test]
fn a_manual_install_never_quits_the_app() {
    let dir = Scratch::new("manual");
    let mut h = app();
    let file = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    let desktop = std::sync::Arc::new(lcc_engine::desktop::Recorder::default());
    h.app.session.desktop = desktop.clone();
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    // the archive path is a manual install: it shows the file and stays open
    h.request("ui.clickWidget", json!({"id": "button:updateInstall"}), T);
    h.step();
    assert!(!h.quit_requested(), "nothing to quit for");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Update));
    assert!(file.exists(), "the downloaded file is still there for the user to unpack");
    let installed = h.app.session.updater.installed().expect("an outcome");
    assert!(!installed.exit);
    assert!(installed.message.contains("Quit the app"), "{}", installed.message);
    // the file is shown through the session's desktop, never by starting the real file manager
    assert_eq!(desktop.requests(), vec![lcc_engine::desktop::Request::Reveal(file.clone())]);
}

#[test]
fn skipping_remembers_the_version() {
    let dir = Scratch::new("skip");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:updateSkip"}));
    h.step();
    assert!(h.app.session.updater.already_seen("9.9.9"), "the version is remembered");
    // the dialog stays open until it is closed, and the update is no longer announced as new
    assert_eq!(h.app.ui.dialog, Some(Dialog::Update));
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.step();
    assert_eq!(h.app.ui.dialog, None, "Esc ends the dialog");
}

#[test]
fn the_settings_in_the_dialog_are_saved() {
    let mut h = app();
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    assert!(!h.app.session.updater.prefs().check_at_startup, "off by default");
    ask(&mut h, "ui.clickWidget", json!({"id": "check:updateAtStartup"}));
    h.step();
    assert!(h.app.session.updater.prefs().check_at_startup, "turning it on is saved");
    ask(&mut h, "ui.clickWidget", json!({"id": "check:updatePrerelease"}));
    h.step();
    assert!(h.app.session.updater.prefs().prerelease);
    // the dialog is still usable after a settings change
    assert_eq!(h.app.ui.dialog, Some(Dialog::Update));
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.step();
    assert_eq!(h.app.ui.dialog, None);
}

#[test]
fn the_download_button_says_it_cannot_start_when_there_is_nothing_to_fetch() {
    let mut h = app();
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    // nothing on offer: no Download button is drawn at all
    assert!(h.app.widgets.iter().all(|(w, _)| w != "button:updateDownload"), "no update, no Download");
    // and the command says why rather than panicking
    let r = h.app.session.execute("update.download", &json!({}));
    assert!(r.is_err());
}

#[test]
fn a_checksum_list_is_never_offered_for_install() {
    let dir = Scratch::new("sums");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Checksums);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    let err = h.app.session.execute("update.install", &json!({"confirm": true})).expect_err("refused");
    assert!(err.to_string().contains("no file for this system"), "{err}");
}

// ---------------------------------------------------------------- the start-up check is opt-in
//
// Nothing here may reach the network: each frame asserts that no check was armed or started, so
// on a build that would check, the test fails at the first frame, long before the 4 s are up.

/// What `updates.json` in `dir` says about the start-up check (`None`: no file, or no such key).
fn saved_check_at_startup(dir: &Scratch) -> Option<bool> {
    let text = std::fs::read(dir.0.join("updates.json")).ok()?;
    let file: serde_json::Value = serde_json::from_slice(&text).unwrap();
    assert_eq!(file["version"], 1, "the settings file stays at version 1: {file}");
    file["updates"]["checkAtStartup"].as_bool()
}

/// The app as `session` and its host would start it, with the updater's settings in `dir`.
fn started(mut session: lcc_engine::Session, dir: &Scratch, desktop: bool) -> Headless {
    session.updater = lcc_engine::update::Updater::new(Some(dir.0.clone()));
    let mut app = LccApp::new(session, services());
    // what the desktop app sets for a run with a settings folder of its own
    app.startup_update_check = desktop;
    Headless::new(app, [1200.0, 900.0], 1.0)
}

/// Run `frames` headless frames (1/60 s each), asserting at every one that no check was armed or
/// started.
fn run_without_a_check(h: &mut Headless, frames: usize) {
    for frame in 0..frames {
        h.step();
        assert_eq!(h.app.ui.update_checked_at, 0.0, "frame {frame}: a start-up check was scheduled");
        assert_eq!(h.app.session.updater.check(), &lcc_engine::update::Check::Never, "frame {frame}: a check ran");
        assert!(!h.app.session.updater.checking(), "frame {frame}: a check is running");
    }
}

/// Run frames until the start-up check is scheduled, and say when it would run (egui time, s).
/// Never steps up to that time, so no check runs.
fn scheduled_check(h: &mut Headless) -> f64 {
    for _ in 0..3 {
        h.step();
        if h.app.ui.update_checked_at > 0.0 {
            break;
        }
    }
    assert!(!h.app.session.updater.checking(), "nothing runs before the delay");
    h.app.ui.update_checked_at
}

#[test]
fn an_empty_settings_folder_never_checks_at_start() {
    let dir = Scratch::new("first-start");
    let mut h = started(lcc_engine::Session::new(), &dir, true);
    // ten seconds of the desktop app with no updates.json
    run_without_a_check(&mut h, 600);
    assert!(!h.app.session.updater.prefs().check_at_startup, "off until the user turns it on");
    assert_eq!(saved_check_at_startup(&dir), None, "nothing was written");
}

#[test]
fn headless_sessions_never_check_at_start_even_when_the_user_asked_for_it() {
    let dir = Scratch::new("headless");
    std::fs::write(dir.0.join("updates.json"), br#"{"version": 1, "updates": {"checkAtStartup": true}}"#).unwrap();
    // the harness, `snapshot` and `--memory`/`--demo` runs: a demo session, and a host that
    // doesn't set the start-up flag
    let mut h = started(lcc_engine::Session::with_demo(), &dir, false);
    assert!(h.app.session.updater.prefs().check_at_startup, "the preference is on");
    run_without_a_check(&mut h, 600);
}

#[test]
fn turning_the_check_on_in_settings_saves_it_and_the_next_start_checks_once() {
    let dir = Scratch::new("settings");
    let mut h = started(lcc_engine::Session::new(), &dir, true);
    run_without_a_check(&mut h, 2);
    ask(&mut h, "engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "check:settings.updateAtStartup"}));
    h.step();
    assert!(h.app.session.updater.prefs().check_at_startup, "the box turns it on");
    assert_eq!(saved_check_at_startup(&dir), Some(true), "and it is saved");
    // turning it on doesn't check now: that is for the next start
    run_without_a_check(&mut h, 2);
    drop(h);

    let mut next = started(lcc_engine::Session::new(), &dir, true);
    let at = scheduled_check(&mut next);
    assert!((3.0..=5.0).contains(&at), "the next start checks about 4 s in (scheduled at {at} s)");
}

#[test]
fn the_dialog_turns_the_check_on_and_the_next_start_checks() {
    let dir = Scratch::new("dialog");
    let mut h = started(lcc_engine::Session::new(), &dir, true);
    run_without_a_check(&mut h, 1);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "check:updateAtStartup"}));
    h.step();
    assert_eq!(saved_check_at_startup(&dir), Some(true));
    drop(h);
    let mut next = started(lcc_engine::Session::new(), &dir, true);
    assert!(scheduled_check(&mut next) > 0.0, "the next start checks");
}

/// The welcome dialog of a first start, its catalog location in `root`.
fn welcome(dir: &Scratch, root: &Scratch) -> Headless {
    let mut h = started(lcc_engine::Session::new().with_fs(), dir, true);
    h.app.run("catalog.welcome", json!({})).unwrap();
    if let Some(crate::catalog_ui::CatalogDialog::Welcome { parent, .. }) = &mut h.app.catalog_ui.dialog {
        *parent = root.0.to_string_lossy().into_owned();
    }
    run_without_a_check(&mut h, 2);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:welcomeUpdates"), "the welcome dialog offers the check");
    h
}

#[test]
fn the_welcome_dialog_saves_the_check_only_when_it_is_ticked() {
    // left unticked: the catalog is created and nothing about updates is saved
    let (dir, root) = (Scratch::new("welcome-off"), Scratch::new("welcome-off-lib"));
    let mut h = welcome(&dir, &root);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeCreate"}));
    h.step();
    assert!(h.app.session.library.is_some(), "the catalog is open");
    assert!(!h.app.session.updater.prefs().check_at_startup, "unticked: off");
    assert_ne!(saved_check_at_startup(&dir), Some(true));
    h.app.session.close_library().unwrap();
    drop(h);

    // ticked: saved as on with the new catalog
    let (dir, root) = (Scratch::new("welcome-on"), Scratch::new("welcome-on-lib"));
    let mut h = welcome(&dir, &root);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    assert_eq!(saved_check_at_startup(&dir), None, "nothing is saved before a catalog is open");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeCreate"}));
    h.step();
    assert!(h.app.session.library.is_some(), "the catalog is open");
    assert_eq!(saved_check_at_startup(&dir), Some(true), "ticked: on");
    run_without_a_check(&mut h, 2);
    h.app.session.close_library().unwrap();
}

#[test]
fn quitting_from_the_welcome_dialog_saves_nothing() {
    let (dir, root) = (Scratch::new("welcome-quit"), Scratch::new("welcome-quit-lib"));
    let mut h = welcome(&dir, &root);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeQuit"}));
    h.step();
    assert!(h.quit_requested(), "Quit quits");
    assert!(!dir.0.join("updates.json").exists(), "nothing was saved");
    assert!(!h.app.session.updater.prefs().check_at_startup);
}

/// The answers to the folder dialogs a host shows elsewhere, in the order they were asked for.
type Answers = std::sync::Arc<std::sync::Mutex<Vec<std::sync::mpsc::Sender<Vec<String>>>>>;

/// Give the app the desktop's kind of folder dialog: shown elsewhere, answered frames later (the
/// synchronous one is never asked).
fn background_dialogs(h: &mut Headless) -> Answers {
    let answers: Answers = Default::default();
    let a = answers.clone();
    h.app.services.picker = Some(Box::new(move |_req| {
        let (tx, rx) = std::sync::mpsc::channel();
        a.lock().unwrap().push(tx);
        Ok(rx)
    }));
    h.app.services.pick_folder = Some(Box::new(|| panic!("the background dialog answers")));
    h.step();
    answers
}

/// Tick the welcome box, choose Open Catalog…, and cancel its folder dialog.
fn tick_open_and_cancel(h: &mut Headless, answers: &Answers) {
    ask(h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    assert_eq!(answers.lock().unwrap().len(), 1, "the folder dialog is up");
    answers.lock().unwrap()[0].send(Vec::new()).unwrap();
    run_without_a_check(h, 3);
    assert!(h.app.session.library.is_none(), "cancelled: no catalog");
}

fn welcome_is_up(h: &Headless) -> bool {
    matches!(h.app.catalog_ui.dialog, Some(crate::catalog_ui::CatalogDialog::Welcome { .. }))
}

#[test]
fn cancelling_open_catalog_returns_to_the_welcome_dialog() {
    let (dir, root) = (Scratch::new("welcome-back"), Scratch::new("welcome-back-lib"));
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    answers.lock().unwrap()[0].send(Vec::new()).unwrap();
    run_without_a_check(&mut h, 3);
    assert!(h.app.session.library.is_none(), "cancelled: no catalog");
    assert!(welcome_is_up(&h), "cancelling the folder dialog leaves the welcome dialog up");
}

#[test]
fn the_welcome_dialog_waits_while_its_folder_dialog_is_up() {
    let (dir, root) = (Scratch::new("welcome-wait"), Scratch::new("welcome-wait-lib"));
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    h.step();
    // Create, while the folder dialog is up: nothing happens
    let _ = h.request("ui.clickWidget", json!({"id": "button:welcomeCreate"}), T);
    run_without_a_check(&mut h, 2);
    assert!(h.app.session.library.is_none(), "no catalog was created behind the folder dialog");
    assert!(welcome_is_up(&h), "the welcome dialog is still there, waiting");
    assert_eq!(saved_check_at_startup(&dir), None, "nothing saved");
    // the folder dialog is cancelled: back to the welcome dialog, nothing open
    answers.lock().unwrap()[0].send(Vec::new()).unwrap();
    run_without_a_check(&mut h, 3);
    assert!(h.app.session.library.is_none(), "cancelled: no catalog");
    assert!(welcome_is_up(&h), "back to the welcome dialog");
    assert_eq!(saved_check_at_startup(&dir), None, "nothing saved");
}

#[test]
fn another_open_while_the_welcome_folder_dialog_is_up_does_not_take_its_choice() {
    let (dir, root) = (Scratch::new("welcome-other"), Scratch::new("welcome-other-lib"));
    let other = lcc_catalog::library::create(&root.0, "Other").unwrap();
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    // an agent opens a catalog by path while the welcome's folder dialog is still up
    ask(&mut h, "engine.execute", json!({"command": "catalog.open", "params": {"path": other.to_string_lossy()}}));
    run_without_a_check(&mut h, 2);
    assert!(h.app.session.library.is_some(), "the agent's catalog is open");
    assert_ne!(saved_check_at_startup(&dir), Some(true), "the welcome's choice is not the agent's open");
    // and the welcome's folder dialog is cancelled
    answers.lock().unwrap()[0].send(Vec::new()).unwrap();
    run_without_a_check(&mut h, 3);
    assert_ne!(saved_check_at_startup(&dir), Some(true), "nothing saved");
    h.app.session.close_library().unwrap();
}

#[test]
fn a_cancelled_open_forgets_the_box_before_the_next_welcome_dialog() {
    let (dir, root) = (Scratch::new("welcome-cancel"), Scratch::new("welcome-cancel-lib"));
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    tick_open_and_cancel(&mut h, &answers);
    // File ▸ Welcome…, and a catalog created with the box left unticked
    h.app.run("catalog.welcome", json!({})).unwrap();
    if let Some(crate::catalog_ui::CatalogDialog::Welcome { parent, .. }) = &mut h.app.catalog_ui.dialog {
        *parent = root.0.to_string_lossy().into_owned();
    }
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeCreate"}));
    run_without_a_check(&mut h, 2);
    assert!(h.app.session.library.is_some(), "the catalog is open");
    assert_ne!(saved_check_at_startup(&dir), Some(true), "unticked: the earlier, cancelled choice is not saved");
    assert!(!h.app.session.updater.prefs().check_at_startup);
    h.app.session.close_library().unwrap();
}

#[test]
fn a_cancelled_open_forgets_the_box_before_a_later_open_catalog() {
    let (dir, root) = (Scratch::new("welcome-cancel-menu"), Scratch::new("welcome-cancel-menu-lib"));
    let entry = lcc_catalog::library::create(&root.0, "Existing").unwrap();
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    tick_open_and_cancel(&mut h, &answers);
    // File ▸ Open Catalog…, answered with a catalog this time
    ask(&mut h, "ui.menu.invoke", json!({"id": "catalog.open"}));
    h.step();
    assert_eq!(answers.lock().unwrap().len(), 2, "the folder dialog is up again");
    answers.lock().unwrap()[1].send(vec![entry.to_string_lossy().into_owned()]).unwrap();
    run_without_a_check(&mut h, 3);
    assert!(h.app.session.library.is_some(), "the chosen catalog is open");
    assert_ne!(saved_check_at_startup(&dir), Some(true), "the cancelled welcome choice is not saved");
    h.app.session.close_library().unwrap();
}

/// The desktop app's file dialogs answer later, on another thread: Open Catalog… from the welcome
/// dialog returns before a catalog is chosen, and the catalog opens when the dialog closes. The
/// ticked box still counts then.
#[test]
fn a_ticked_welcome_box_survives_a_file_dialog_that_answers_later() {
    let (dir, root) = (Scratch::new("welcome-later"), Scratch::new("welcome-later-lib"));
    let entry = lcc_catalog::library::create(&root.0, "Existing").unwrap();
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    assert_eq!(answers.lock().unwrap().len(), 1, "the folder dialog is up");
    assert!(h.app.session.library.is_none(), "no catalog chosen yet");
    assert_eq!(saved_check_at_startup(&dir), None, "nothing is saved before a catalog is open");
    // the dialog closes with the catalog chosen
    answers.lock().unwrap()[0].send(vec![entry.to_string_lossy().into_owned()]).unwrap();
    run_without_a_check(&mut h, 3);
    assert!(h.app.session.library.is_some(), "the chosen catalog is open");
    assert_eq!(saved_check_at_startup(&dir), Some(true), "and the ticked box is saved");
    h.app.session.close_library().unwrap();
}

// ------------------------------------------------- a settings folder that can't be written (AC-8)

/// Makes a scratch folder unwritable (0o555) until dropped; `None` when it stays writable
/// (running as root), and the test is skipped.
#[cfg(unix)]
struct ReadOnly(PathBuf);

#[cfg(unix)]
fn read_only(dir: &Scratch) -> Option<ReadOnly> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o555)).unwrap();
    let guard = ReadOnly(dir.0.clone());
    if std::fs::write(dir.0.join("probe"), b"x").is_ok() {
        eprintln!("skipped: {} is still writable (running as root?)", dir.0.display());
        return None;
    }
    Some(guard)
}

#[cfg(unix)]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
fn toast_text(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn the_welcome_dialog_says_so_when_its_choice_cannot_be_saved() {
    let (dir, root) = (Scratch::new("welcome-ro"), Scratch::new("welcome-ro-lib"));
    let Some(_ro) = read_only(&dir) else { return };
    let mut h = welcome(&dir, &root);
    ask(&mut h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeCreate"}));
    run_without_a_check(&mut h, 2);
    assert!(h.app.session.library.is_some(), "the catalog is open all the same");
    let toast = toast_text(&h);
    assert!(toast.contains("updates.json"), "the error is shown, naming the file: {toast:?}");
    assert!(!h.app.session.updater.prefs().check_at_startup, "the setting is still off: it isn't on disk");
    h.app.session.close_library().unwrap();
}

#[cfg(unix)]
#[test]
fn the_settings_checkbox_says_so_when_it_cannot_be_saved() {
    let dir = Scratch::new("settings-ro");
    let Some(_ro) = read_only(&dir) else { return };
    let mut h = started(lcc_engine::Session::new(), &dir, true);
    run_without_a_check(&mut h, 1);
    ask(&mut h, "engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}));
    h.step();
    ask(&mut h, "ui.clickWidget", json!({"id": "check:settings.updateAtStartup"}));
    run_without_a_check(&mut h, 2);
    let toast = toast_text(&h);
    assert!(toast.contains("updates.json"), "the error is shown, naming the file: {toast:?}");
    assert!(!h.app.session.updater.prefs().check_at_startup, "the box shows what is stored: off");
}
