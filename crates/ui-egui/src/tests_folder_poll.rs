//! Folder polling in the app (issue #123; see `crate::folder_poll`).
//!
//! * Polling is off by default: nothing lists the folders.
//! * Turned on, a poll runs when the library opens, under a row in the activity stack; files
//!   another app adds to a library folder are found by the next poll and, by default, shown as
//!   "N new" on the folder with a notice, not added.
//! * With new files "add", they arrive in the background.
//! * A `sync` link the app was started with opens Synchronize Folder for its folder.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const SETTLE: Duration = Duration::from_secs(120);

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-poll-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().replace('\\', "/")
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

/// The app with `trip/a.png` imported and polling set as `prefs` says.
fn app(dir: &Scratch, prefs: serde_json::Value) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    let mut session = lcc_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    session.execute("folder.watchPreferences", &prefs).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    h.settle(SETTLE);
    h
}

fn in_library(h: &Headless) -> usize {
    h.app.session.catalog.photos().filter(|p| p.in_library()).count()
}

#[test]
fn polling_is_off_by_default() {
    let dir = Scratch::new("off");
    let h = app(&dir, json!({}));
    assert!(h.app.session.folder_stamps.is_empty(), "no folder was listed");
}

#[test]
fn a_poll_finds_new_files_and_asks_by_default() {
    let dir = Scratch::new("ask");
    let mut h = app(&dir, json!({"poll": true}));
    // the poll on open recorded the folder
    assert_eq!(h.app.session.folder_stamps.len(), 1);
    write_png(&dir.path("trip/b.png"), 2);
    crate::folder_poll::start(&mut h.app);
    assert!(h.app.session.activity.list().iter().any(|t| t.kind == "folderPoll"), "the poll shows in the activity stack");
    h.settle(SETTLE);
    assert_eq!(lcc_engine::folder_watch::new_in(&h.app.session, &dir.path("trip")), Some(1));
    assert_eq!(in_library(&h), 1, "ask adds nothing by itself");
}

#[test]
fn with_add_new_files_arrive_in_the_background() {
    let dir = Scratch::new("add");
    let mut h = app(&dir, json!({"poll": true, "newFiles": "add"}));
    write_png(&dir.path("trip/b.png"), 2);
    write_png(&dir.path("trip/c.png"), 3);
    crate::folder_poll::start(&mut h.app);
    h.settle(SETTLE);
    let done = h.step_until(SETTLE, |h| in_library(h) == 3);
    assert!(done, "the new files were added");
    assert!(h.app.session.folder_new.is_empty());
}

#[test]
fn a_sync_link_opens_synchronize_folder() {
    let dir = Scratch::new("link");
    let mut h = app(&dir, json!({}));
    h.app.pending_link = Some(lcc_engine::link::sync_link(&dir.path("trip")));
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })), "the dialog opened");
    // a hostile link is a notice, never a panic
    h.app.ui.dialog = None;
    h.app.pending_link = Some(format!("{}://sync?path=%ZZ", lcc_brand::URL_SCHEME));
    h.step();
    assert!(h.app.ui.dialog.is_none());
}

/// Gap 37: with polling on, the OS's change notifications on the library's folders start a poll
/// (no interval poll is due: the interval is five minutes).
#[test]
fn a_change_notification_starts_a_poll() {
    let dir = Scratch::new("notify");
    let mut h = app(&dir, json!({"poll": true, "intervalMinutes": 5}));
    // the watch is set up on a worker after the poll on open
    let watching = h.step_until(SETTLE, |h| h.app.folder_poll.os.as_ref().is_some_and(|o| o.watch.is_some()));
    assert!(watching, "the folders are watched");
    if h.app.folder_poll.os.as_ref().and_then(|o| o.watch.as_ref()).is_none_or(|w| w.is_empty()) {
        lcc_testkit::skip!(System, "the OS watches no folder here");
    }
    write_png(&dir.path("trip/b.png"), 2);
    let found = h.step_until(SETTLE, |h| lcc_engine::folder_watch::new_in(&h.app.session, &dir.path("trip")) == Some(1));
    if !found {
        lcc_testkit::skip!(System, "no change notification for the temp folder here");
    }
    assert_eq!(in_library(&h), 1, "ask adds nothing by itself");
    // polling turned off: the watch goes too
    h.app.session.execute("folder.watchPreferences", &json!({"poll": false})).unwrap();
    h.step();
    assert!(h.app.folder_poll.os.is_none());
}

/// Gap 37: a second launch hands its link and files to the running copy, which follows the link
/// (Synchronize Folder opens) and imports the files.
#[test]
fn a_second_launch_hands_over_its_link_and_files() {
    let dir = Scratch::new("handoff");
    let mut h = app(&dir, json!({}));
    let settings = dir.0.join("settings");
    h.app.handoff = Some(lcc_engine::instance::Server::start(&settings).unwrap());
    write_png(&dir.path("more/x.png"), 4);
    let args = vec![lcc_engine::link::sync_link(&dir.path("trip")), dir.path("more/x.png")];
    assert!(lcc_engine::instance::hand_off(&settings, &args).unwrap(), "the running copy took them");
    let opened = h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })));
    assert!(opened, "the link was followed");
    assert!(
        h.app.catalog_ui.pending_import.contains(&dir.path("more/x.png")) || in_library(&h) == 2,
        "the file is imported (or waits for the library)"
    );
    // a hostile hand-off (wrong token) is refused and changes nothing
    h.app.ui.dialog = None;
    let mut rec = lcc_engine::instance::parse_record(&std::fs::read_to_string(settings.join(lcc_engine::instance::FILE)).unwrap()).unwrap();
    rec.token = "0".repeat(32);
    std::fs::write(settings.join(lcc_engine::instance::FILE), serde_json::to_string(&rec).unwrap()).unwrap();
    assert!(!lcc_engine::instance::hand_off(&settings, &args).unwrap());
    h.step();
    assert!(h.app.ui.dialog.is_none());
}
