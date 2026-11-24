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
#![allow(clippy::indexing_slicing, reason = "test code: clippy.toml allows indexing in tests, but its test check misses cfg(all(test, ...))")]
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
    app_with(services())
}

fn app_with(services: Services) -> Headless {
    let mut session = lcc_engine::Session::new();
    // a fresh updater: the settings are the defaults, not whatever this machine's are
    session.updater = lcc_engine::update::Updater::new(None);
    // a throwaway test key (never a release key): these tests are about the dialog, and a build
    // without a key installs nothing (lcc-engine's update tests cover that)
    let key = lcc_updater::UpdateKey::parse("GX9rI+FshTLGq8g4+s1ep4m+DHaykgM0A5v6iz02jWE=").unwrap();
    session.updater.trust_test_key(Some(key));
    let app = LccApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    // no start-up check: these tests drive the dialog themselves
    h.app.ui.update_checked_at_startup = true;
    h.step();
    h
}

/// Offer `version` as a newer release, as a check would, with its file already downloaded.
fn offer(h: &mut Headless, dir: &Scratch, version: &str, kind: lcc_engine::update::Kind) -> PathBuf {
    offer_with_notes(h, dir, version, kind, "### Fixed\n- a thing")
}

/// [`offer`] with the release's notes.
fn offer_with_notes(h: &mut Headless, dir: &Scratch, version: &str, kind: lcc_engine::update::Kind, notes: &str) -> PathBuf {
    use lcc_engine::update::{Kind, Progress, Staged};
    let file = dir.0.join(format!("app-{version}-{}.bin", kind.text()));
    std::fs::write(&file, b"an installer").unwrap();
    // the hash the download step recorded (install hashes the file again before using it)
    let sha256 = lcc_updater::hash_file(&file).unwrap();
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
        notes: notes.into(),
        action: "run the installer".into(),
    };
    h.app.session.updater.apply(lcc_updater::Checked::Found(Box::new(update)));
    h.app.session.updater.set_progress(Progress::Ready(Box::new(Staged {
        version: version.into(),
        path: file.clone(),
        bytes: 12,
        sha256,
        file: name,
        kind,
        notes: notes.into(),
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
    h.app.ui.startup_update_checking = true;
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.step();
    h.step();
    assert_eq!(h.app.ui.dialog, None, "a start-up check never opens a window");
    assert!(h.app.ui.update_pending.is_none(), "the announcement is made once");
    assert!(h.app.ui.toast().is_some(), "it says so instead");
}

#[test]
fn a_startup_check_respects_skipped_version() {
    let dir = Scratch::new("startup-skip");
    let mut h = app();
    h.app.session.updater.mark_seen("9.9.9", true);
    h.app.ui.startup_update_checking = true;
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.step();
    h.step();
    assert_eq!(h.app.ui.dialog, None);
    assert!(h.app.ui.update_pending.is_none());
    assert!(h.app.ui.toast().is_none(), "skipped version is not toasted");
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
fn the_install_button_is_the_one_to_press_once_the_download_is_ready() {
    let dir = Scratch::new("install-default");
    let mut h = app();
    let file = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    let desktop = std::sync::Arc::new(lcc_engine::desktop::Recorder::default());
    h.app.session.desktop = desktop.clone();
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    h.step();
    // the button holds the keyboard as soon as it appears, so Return is its press
    let install = egui::Id::new("update-install-focus");
    assert!(h.view.ctx.data(|d| d.get_temp::<bool>(install)).unwrap_or(false), "the install button asked for the focus");
    assert!(h.view.ctx.memory(|m| m.focused()).is_some(), "the install button has the keyboard focus");
    assert!(h.app.session.updater.installed().is_none(), "nothing installs by itself");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    h.step();
    assert!(h.app.session.updater.installed().is_some(), "Return pressed the install button");
    assert!(file.exists());
    assert_eq!(desktop.requests(), vec![lcc_engine::desktop::Request::Reveal(file.clone())]);
}

#[test]
fn the_install_button_takes_the_focus_again_when_the_dialog_is_reopened() {
    let dir = Scratch::new("install-reopen");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    let asked = |h: &Headless| h.view.ctx.data(|d| d.get_temp::<bool>(egui::Id::new("update-install-focus"))).unwrap_or(false);
    assert!(asked(&h), "asked for the focus when it first appeared");
    h.app.ui.dialog = None;
    h.step();
    h.step();
    assert!(!asked(&h), "closing the dialog forgets it");
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    assert!(asked(&h), "asked again on the next opening");
}

#[test]
fn return_installs_nothing_before_the_download_is_ready() {
    let dir = Scratch::new("install-not-ready");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.app.session.updater.set_progress(lcc_engine::update::Progress::Idle);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    h.step();
    assert!(h.app.session.updater.installed().is_none(), "Return does not install a file that isn't downloaded");
}

/// A clock the test moves (Unix seconds), given to the updater; starts at a fixed time.
fn clocked(h: &mut Headless) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let now = std::sync::Arc::new(AtomicU64::new(1_800_000_000));
    let seen = now.clone();
    h.app.session.updater.set_test_clock(Box::new(move || seen.load(Ordering::SeqCst)));
    now
}

/// The dialog's text with a check that finished at the clock's start, the clock now `later` seconds on.
fn last_checked_after(later: i64) -> Option<String> {
    use std::sync::atomic::Ordering;
    let mut h = app();
    let now = clocked(&mut h);
    h.app.session.updater.apply(lcc_updater::Checked::Current("1.0.0".into()));
    now.store((1_800_000_000 + later) as u64, Ordering::SeqCst);
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    h.painted_text().into_iter().find(|t| t.starts_with("Last checked"))
}

#[test]
fn the_last_check_reads_as_the_time_since() {
    assert_eq!(last_checked_after(40).as_deref(), Some("Last checked 40 seconds ago"));
    assert_eq!(last_checked_after(12 * 60).as_deref(), Some("Last checked 12 minutes ago"));
    assert_eq!(last_checked_after(50 * 3600).as_deref(), Some("Last checked 50 hours ago"));
    assert_eq!(last_checked_after(48 * 3600).as_deref(), Some("Last checked 48 hours ago"), "no days");
}

#[test]
fn a_check_under_ten_seconds_old_reads_just_now() {
    assert_eq!(last_checked_after(0).as_deref(), Some("Last checked just now"));
    assert_eq!(last_checked_after(9).as_deref(), Some("Last checked just now"));
    assert_eq!(last_checked_after(10).as_deref(), Some("Last checked 10 seconds ago"), "the cut-off");
}

#[test]
fn the_singular_reads_as_one_minute_and_one_hour() {
    assert_eq!(last_checked_after(60).as_deref(), Some("Last checked 1 minute ago"));
    assert_eq!(last_checked_after(3600).as_deref(), Some("Last checked 1 hour ago"));
}

#[test]
fn a_check_in_the_future_reads_just_now_and_does_not_panic() {
    assert_eq!(last_checked_after(-3600).as_deref(), Some("Last checked just now"));
    assert_eq!(last_checked_after(-1_800_000_000).as_deref(), Some("Last checked just now"));
}

#[test]
fn before_any_check_there_is_no_last_checked_line() {
    let mut h = app();
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    assert!(!h.painted_text().iter().any(|t| t.starts_with("Last checked")), "{:?}", h.painted_text());
}

#[test]
fn a_failed_check_still_says_when_it_was_made() {
    use std::sync::atomic::Ordering;
    let mut h = app();
    let now = clocked(&mut h);
    h.app.session.updater.apply(lcc_updater::Checked::Failed("no network".into()));
    now.store(1_800_000_000 + 300, Ordering::SeqCst);
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    assert!(h.painted_text().iter().any(|t| t == "Last checked 5 minutes ago"), "{:?}", h.painted_text());
}

#[test]
fn the_line_follows_the_clock_while_the_window_stays_open_with_no_input() {
    use std::sync::atomic::Ordering;
    let mut h = app();
    let now = clocked(&mut h);
    h.app.session.updater.apply(lcc_updater::Checked::Current("1.0.0".into()));
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    let has = |h: &Headless, text: &str| h.painted_text().iter().any(|t| t == text);
    assert!(has(&h, "Last checked just now"), "{:?}", h.painted_text());
    now.store(1_800_000_000 + 61, Ordering::SeqCst);
    // no key, no pointer: only frames
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "Last checked 1 minute ago"), "{:?}", h.painted_text());
}

#[test]
fn the_window_asks_to_be_redrawn_when_the_words_would_change() {
    use crate::panels::update::next_change;
    assert_eq!(next_change(0), 10, "just now holds until 10 seconds");
    assert_eq!(next_change(9), 1);
    assert_eq!(next_change(10), 1, "seconds count one by one");
    assert_eq!(next_change(59), 1);
    assert_eq!(next_change(60), 60, "a minute reads the same for a minute");
    assert_eq!(next_change(119), 1);
    assert_eq!(next_change(3599), 1);
    assert_eq!(next_change(3600), 3600);
    assert_eq!(next_change(-5), 10, "a clock that went back");
    assert!(next_change(i64::MAX) >= 1);
}

#[test]
fn the_download_sentence_names_what_the_button_does_and_the_app() {
    let dir = Scratch::new("sentence");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Msi);
    h.app.session.updater.set_progress(lcc_engine::update::Progress::Idle);
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    let name = lcc_brand::DISPLAY_NAME;
    let text = h.painted_text();
    let line = text.iter().find(|t| t.starts_with("Download will download")).unwrap_or_else(|| panic!("{text:?}"));
    assert!(line.contains(&format!("signed by the {name} authors")), "{line}");
    assert!(line.ends_with("offer to install it over this version."), "{line}");
    assert!(!text.iter().any(|t| t.contains("SHA-256") || t.starts_with("Installing it will")), "the two old lines are gone: {text:?}");
}

#[test]
fn where_the_download_only_opens_a_folder_the_sentence_says_so() {
    let dir = Scratch::new("sentence-manual");
    let mut h = app();
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    h.app.session.updater.set_progress(lcc_engine::update::Progress::Idle);
    // (the dialog opened directly: the menu command also asks the feed, and a repeat answer would be stamped again)
    h.app.ui.dialog = Some(Dialog::Update);
    h.step();
    h.step();
    let text = h.painted_text();
    let line = text.iter().find(|t| t.starts_with("Download will download")).unwrap_or_else(|| panic!("{text:?}"));
    assert!(line.ends_with("and open its folder, so you can install it over this version."), "{line}");
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
    assert!(h.app.session.updater.options().prerelease);
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

/// A build without an update key (every build until the maintainer adds one) says it can't verify
/// the release, offers no Download, and opens the release's page instead.
#[test]
fn a_build_that_cannot_verify_offers_the_release_page_not_a_download() {
    let dir = Scratch::new("nokey");
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut h = app();
    let seen = opened.clone();
    h.app.services.open_url = Some(Box::new(move |url: &str| {
        seen.lock().unwrap().push(url.to_string());
        Ok(())
    }));
    h.app.session.updater.trust_test_key(None);
    let _ = offer(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive);
    // offered, not downloaded
    h.app.session.updater.set_progress(lcc_engine::update::Progress::Idle);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    h.step();
    let text = h.painted_text().join(" ");
    assert!(text.contains("can't verify updates"), "the dialog says why nothing is downloaded: {text}");
    assert!(!text.contains("checked against the size"), "and doesn't claim a check it doesn't make: {text}");
    assert!(h.app.widgets.iter().all(|(w, _)| w != "button:updateDownload"), "no Download that could only fail");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:updateReleasePage"), "the release page is offered instead");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:updateReleasePage"}));
    h.step();
    assert_eq!(*opened.lock().unwrap(), vec![lcc_engine::update::release_page("v9.9.9")], "the button opens the offered release's page");
    // the same through the command, as an agent would
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.openReleasePage"}));
    assert_eq!(opened.lock().unwrap().len(), 2);
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
type Answers = std::sync::Arc<std::sync::Mutex<Vec<std::sync::mpsc::Sender<crate::pick::PickResult>>>>;

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

/// Tick the welcome box, choose Open Library…, and cancel its folder dialog.
fn tick_open_and_cancel(h: &mut Headless, answers: &Answers) {
    ask(h, "ui.clickWidget", json!({"id": "check:welcomeUpdates"}));
    h.step();
    ask(h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    assert_eq!(answers.lock().unwrap().len(), 1, "the folder dialog is up");
    answers.lock().unwrap()[0].send(Ok(Vec::new())).unwrap();
    run_without_a_check(h, 3);
    assert!(h.app.session.library.is_none(), "cancelled: no catalog");
}

fn welcome_is_up(h: &Headless) -> bool {
    matches!(h.app.catalog_ui.dialog, Some(crate::catalog_ui::CatalogDialog::Welcome { .. }))
}

// Scenario (issue #929): Help > Check for Updates over the welcome dialog is shown on its own, not
// under the welcome card; closing it brings the welcome dialog back
#[test]
fn check_for_updates_over_the_welcome_dialog_is_usable() {
    let (dir, root) = (Scratch::new("welcome-update"), Scratch::new("welcome-update-lib"));
    let mut h = welcome(&dir, &root);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    for _ in 0..3 {
        h.step();
    }
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    assert!(!has(&h, "check:welcomeUpdates"), "the welcome dialog stands aside");
    assert!(has(&h, "check:updateAtStartup"), "the update dialog is up");
    h.app.ui.dialog = None;
    for _ in 0..3 {
        h.step();
    }
    assert!(welcome_is_up(&h) && has(&h, "check:welcomeUpdates"), "the welcome dialog is back");
}

#[test]
fn cancelling_open_catalog_returns_to_the_welcome_dialog() {
    let (dir, root) = (Scratch::new("welcome-back"), Scratch::new("welcome-back-lib"));
    let mut h = welcome(&dir, &root);
    let answers = background_dialogs(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:welcomeOpen"}));
    h.step();
    answers.lock().unwrap()[0].send(Ok(Vec::new())).unwrap();
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
    answers.lock().unwrap()[0].send(Ok(Vec::new())).unwrap();
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
    answers.lock().unwrap()[0].send(Ok(Vec::new())).unwrap();
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
    // Open Library…, answered with a library this time
    ask(&mut h, "ui.menu.invoke", json!({"id": "catalog.open"}));
    h.step();
    assert_eq!(answers.lock().unwrap().len(), 2, "the folder dialog is up again");
    answers.lock().unwrap()[1].send(Ok(vec![entry.to_string_lossy().into_owned()])).unwrap();
    run_without_a_check(&mut h, 3);
    assert!(h.app.session.library.is_some(), "the chosen catalog is open");
    assert_ne!(saved_check_at_startup(&dir), Some(true), "the cancelled welcome choice is not saved");
    h.app.session.close_library().unwrap();
}

/// The desktop app's file dialogs answer later, on another thread: Open Library… from the welcome
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
    answers.lock().unwrap()[0].send(Ok(vec![entry.to_string_lossy().into_owned()])).unwrap();
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
    h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default()
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

#[test]
fn the_release_notes_show_inline_markup_and_open_their_links() {
    use std::sync::{Arc, Mutex};
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    let mut services = services();
    services.open_url = Some(Box::new(move |u: &str| {
        o.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    let dir = Scratch::new("notes-markup");
    let mut h = app_with(services);
    let notes = "### Fixed\n- Press `Cmd+K` for **quick** help, see [the guide](https://example.com/guide)\n- [not a web page](javascript:alert(1)) stays text";
    let _ = offer_with_notes(&mut h, &dir, "9.9.9", lcc_engine::update::Kind::Archive, notes);
    ask(&mut h, "ui.menu.invoke", json!({"id": "app.checkForUpdates"}));
    for _ in 0..10 {
        h.step();
    }
    let text = h.painted_text().join("\n");
    assert!(text.contains("Cmd+K") && text.contains("quick") && text.contains("the guide"), "{text}");
    assert!(!text.contains('`') && !text.contains("**") && !text.contains("](https"), "the markers are not shown: {text}");
    assert!(text.contains("not a web page"), "a link the host won't open is shown as its text: {text}");
    let links: Vec<&str> = h.app.widgets.iter().map(|(w, _)| w.as_str()).filter(|w| w.starts_with("link:notes:")).collect();
    assert_eq!(links, ["link:notes:0"], "only the https link can be clicked");
    ask(&mut h, "ui.clickWidget", json!({"id": "link:notes:0"}));
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(opened.lock().unwrap().as_slice(), ["https://example.com/guide"], "the click opened the link, through the host");
}
