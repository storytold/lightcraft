//! A failed save is an error, not a silent success: a command whose journal append fails returns
//! [`EngineError::NotSaved`], its change stays applied and queued, and the next save writes it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::{Flag, MemStore, SnapshotPolicy, Store};
use serde_json::json;

use crate::library::LibraryStores;
use crate::{EngineError, Session};

/// A store whose writes fail while `fail` is set (a full or unplugged disk).
#[derive(Clone, Default)]
struct Flaky {
    files: MemStore,
    fail: Arc<AtomicBool>,
    /// Only snapshots fail (appends work).
    snapshots_only: bool,
    /// Only appends fail (a dead log handle; fresh files can still be written).
    appends_only: bool,
}

impl Flaky {
    fn failing(&self) -> bool {
        self.fail.load(Ordering::SeqCst)
    }
    fn set_failing(&self, on: bool) {
        self.fail.store(on, Ordering::SeqCst);
    }
    /// The files as a crash at this moment would leave them.
    fn crash_copy(&self) -> Flaky {
        let m = MemStore::new();
        *m.files.lock().unwrap() = self.files.files.lock().unwrap().clone();
        Flaky { files: m, ..Default::default() }
    }
}

impl Store for Flaky {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.failing() && !self.appends_only {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.failing() && !self.snapshots_only {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        "flaky".into()
    }
}

fn open(store: &Flaky, seed: bool) -> Session {
    let mut s = Session::new();
    let stores = LibraryStores { dir: "flaky".into(), catalog: Box::new(store.clone()), files: Box::new(MemStore::new()), on_disk: false };
    s.open_library_in(stores, seed).unwrap();
    s
}

#[test]
fn failed_append_fails_the_command_and_is_retried() {
    let store = Flaky::default();
    let mut s = open(&store, true);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    store.set_failing(true);

    let e = s.execute("photo.rate", &json!({"rating": 4})).unwrap_err();
    assert!(matches!(e, EngineError::NotSaved(_)), "{e:?}");
    let msg = e.to_string();
    assert!(
        msg.starts_with("saved in memory but not written to disk: ") && msg.contains("disk full") && msg.ends_with("LightCraft will retry"),
        "{msg}"
    );
    // applied in memory, undoable, queued
    assert_eq!(s.catalog.photo(id).unwrap().rating, 4);
    assert!(!s.undo.is_empty());
    assert_eq!(s.unsaved().map(|u| u.0), Some(1));
    // queries and commands that change nothing are unaffected (they retry the queue, but report
    // their own result)
    let info = s.execute("library.info", &json!({})).unwrap();
    assert_eq!(info["unsavedOps"], 1, "{info}");
    assert!(info["unsavedError"].as_str().unwrap().contains("disk full"), "{info}");
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    // a second change fails too; both stay queued, in order
    let e = s.execute("photo.flag", &json!({"flag": "pick"})).unwrap_err();
    assert!(matches!(e, EngineError::NotSaved(_)));
    assert_eq!(s.unsaved().map(|u| u.0), Some(2));
    // the frame loop backs off instead of retrying every frame
    s.persist_if_dirty();
    assert_eq!(s.unsaved().map(|u| u.0), Some(2));
    // a crash now loses only the unsaved changes; the earlier ones are on disk
    assert_eq!(open(&store.crash_copy(), true).catalog.photo(id).unwrap().rating, 1);

    // the disk comes back: the next save writes the whole queue
    store.set_failing(false);
    s.execute("library.info", &json!({})).unwrap();
    assert_eq!(s.unsaved(), None);
    assert!(s.library.as_ref().unwrap().last_error.is_none());
    let expect = s.catalog.to_snapshot();
    drop(s); // no close: like a crash
    let s2 = open(&store, true);
    assert_eq!(s2.catalog.to_snapshot(), expect);
    let p = s2.catalog.photo(id).unwrap();
    assert_eq!((p.rating, p.flag), (4, Flag::Pick));
}

/// Undo is a command like any other: a failed write of its op is reported, and retried.
#[test]
fn failed_append_of_undo_is_reported_and_retried() {
    let store = Flaky::default();
    let mut s = open(&store, true);
    let id = s.selection.active.unwrap();
    let before = s.catalog.photo(id).unwrap().rating;
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    store.set_failing(true);
    assert!(matches!(s.execute("edit.undo", &json!({})), Err(EngineError::NotSaved(_))));
    assert_eq!(s.catalog.photo(id).unwrap().rating, before);
    store.set_failing(false);
    s.persist().unwrap();
    drop(s);
    assert_eq!(open(&store, true).catalog.photo(id).unwrap().rating, before);
}

/// A failed compaction is not a failed command: the command's own records were appended.
#[test]
fn failed_snapshot_does_not_fail_the_command() {
    let store = Flaky { snapshots_only: true, ..Default::default() };
    let mut s = open(&store, false);
    s.library.as_mut().unwrap().journal_mut().policy = SnapshotPolicy { max_records: 1, max_bytes: u64::MAX };
    store.set_failing(true);
    s.execute("album.create", &json!({"name": "A"})).unwrap();
    s.execute("album.create", &json!({"name": "B"})).unwrap();
    assert_eq!(s.unsaved(), None);
    assert!(s.library.as_ref().unwrap().last_error.is_some(), "the compaction failure is reported");
    drop(s);
    store.set_failing(false);
    assert_eq!(open(&store, false).catalog.albums().count(), 2);
}

/// Issue #101: quitting while the log can't be appended to still saves the queued changes in the
/// closing snapshot (a fresh file), and reports success; they are not appended again later.
#[test]
fn close_saves_queued_changes_in_the_snapshot_when_appends_fail() {
    let store = Flaky { appends_only: true, ..Default::default() };
    let mut s = open(&store, true);
    let id = s.selection.active.unwrap();
    store.set_failing(true);
    assert!(matches!(s.execute("photo.rate", &json!({"rating": 4})), Err(EngineError::NotSaved(_))));
    assert!(matches!(s.execute("photo.flag", &json!({"flag": "pick"})), Err(EngineError::NotSaved(_))));
    assert_eq!(s.unsaved().map(|u| u.0), Some(2));
    s.close_library().unwrap();
    assert_eq!(s.unsaved(), None);
    assert!(s.pending_log.is_empty());
    let expect = s.catalog.to_snapshot();
    drop(s);
    store.set_failing(false);
    let mut s2 = open(&store, true);
    assert_eq!(s2.catalog.to_snapshot(), expect);
    let p = s2.catalog.photo(id).unwrap();
    assert_eq!((p.rating, p.flag), (4, Flag::Pick));
    // later edits continue after the absorbed ops
    s2.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let expect = s2.catalog.to_snapshot();
    drop(s2);
    assert_eq!(open(&store, true).catalog.to_snapshot(), expect);
}

/// When neither the log nor the snapshot can be written, closing fails with `NotSaved` and the
/// changes stay queued (a later retry can still save them).
#[test]
fn close_fails_when_nothing_can_be_saved() {
    let store = Flaky::default();
    let mut s = open(&store, true);
    store.set_failing(true);
    assert!(s.execute("photo.rate", &json!({"rating": 4})).is_err());
    let e = s.close_library().unwrap_err();
    assert!(matches!(e, EngineError::NotSaved(_)), "{e:?}");
    assert_eq!(s.unsaved().map(|u| u.0), Some(1));
    store.set_failing(false);
    s.close_library().unwrap();
    assert_eq!(s.unsaved(), None);
}

/// Issue #2266: a presets.json that can't be written isn't only logged: the preset counts as
/// unsaved (the top bar warns, the web backup refuses) until a later save writes it.
#[test]
fn a_preset_that_cannot_be_written_counts_as_unsaved_until_it_is() {
    let files = Flaky::default();
    let mut s = Session::new();
    let stores = LibraryStores { dir: "flaky".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
    s.open_library_in(stores, true).unwrap();
    assert!(s.unsaved().is_none());
    files.set_failing(true);
    s.execute("preset.create", &json!({"name": "Mine"})).unwrap();
    let (n, why) = s.unsaved().expect("the preset is unsaved");
    assert_eq!(n, 1);
    assert!(why.contains("presets") && why.contains("disk full"), "{why}");
    let stored = |f: &Flaky| f.files.files.lock().unwrap().get("presets.json").cloned().unwrap_or_default();
    assert!(!String::from_utf8_lossy(&stored(&files)).contains("Mine"));
    // still failing: still unsaved
    s.persist().unwrap();
    assert!(s.unsaved().is_some());
    files.set_failing(false);
    s.persist().unwrap();
    assert!(s.unsaved().is_none(), "written once the disk works again");
    assert!(String::from_utf8_lossy(&stored(&files)).contains("Mine"));
}

/// Given a library saved by an earlier version, when it is opened, then the person is told once
/// that it was upgraded and where the library as it was is kept (the files are theirs to
/// delete), and `library.info` lists it; a library that was not upgraded says nothing.
#[test]
fn an_upgraded_library_says_where_what_it_was_is_kept() {
    let dir = std::env::temp_dir().join(format!("lightcraft-kept-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.snap"), include_str!("../../../catalog/src/fixtures/v6-catalog.snap.json")).unwrap();
    std::fs::write(dir.join("catalog.log"), include_str!("../../../catalog/src/fixtures/v6-catalog.log.jsonl")).unwrap();
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    let told = s.take_library_warnings();
    assert!(told.iter().any(|w| w.contains("catalog.snap.v6-8") && w.contains("catalog.log.v6-8") && w.contains("delete")), "{told:?}");
    assert!(dir.join("catalog.snap.v6-8").exists() && dir.join("catalog.log.v6-8").exists());
    let info = s.execute("library.info", &serde_json::json!({})).unwrap();
    assert!(info["settingsWarnings"].to_string().contains("catalog.snap.v6-8"), "{info}");
    s.close_library().unwrap();
    s.open_library(&dir, false).unwrap();
    assert!(!s.take_library_warnings().iter().any(|w| w.contains("upgraded")));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The v6 fixture's log with its second record replaced by garbage (a record after it is still good, so it is damage, not a torn tail).
fn damaged_v6_log() -> String {
    let log = include_str!("../../../catalog/src/fixtures/v6-catalog.log.jsonl");
    log.lines().enumerate().map(|(i, l)| if i == 1 { "not a record" } else { l }).collect::<Vec<_>>().join("\n") + "\n"
}

/// Given a library from an earlier version that has only a damaged log, when it is opened and upgraded, then the person is told
/// where the damaged log is kept (there is no kept snapshot to name it beside).
#[test]
fn an_upgraded_library_with_only_a_damaged_log_says_where_the_log_is_kept() {
    let dir = std::env::temp_dir().join(format!("lightcraft-kept-dmg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.log"), damaged_v6_log()).unwrap();
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    let told = s.take_library_warnings();
    assert!(told.iter().any(|w| w.contains("upgraded") && w.contains("catalog.log.damaged-")), "{told:?}");
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library already in this version's format whose log was damaged was not upgraded: it is not told it was.
#[test]
fn a_current_library_with_a_damaged_log_is_not_told_it_was_upgraded() {
    let dir = std::env::temp_dir().join(format!("lightcraft-cur-dmg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    s.execute("photo.rate", &serde_json::json!({"rating": 3})).unwrap();
    s.execute("photo.rate", &serde_json::json!({"rating": 4})).unwrap();
    s.execute("photo.rate", &serde_json::json!({"rating": 5})).unwrap();
    drop(s);
    let log = std::fs::read_to_string(dir.join("catalog.log")).unwrap();
    let broken: Vec<&str> = log.lines().enumerate().map(|(i, l)| if i == 1 { "not a record" } else { l }).collect();
    std::fs::write(dir.join("catalog.log"), broken.join("\n") + "\n").unwrap();
    let mut s = Session::new();
    let damaged = s.open_library(&dir, false).unwrap().damaged.clone();
    assert!(damaged.is_some_and(|name| dir.join(name).exists()), "the log was found damaged and kept");
    let told = s.take_library_warnings();
    assert!(!told.iter().any(|w| w.contains("upgraded")), "{told:?}");
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The browser's storage: it keeps no copy of a library an upgrade replaces.
struct KeepsNothing(MemStore);

impl Store for KeepsNothing {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.0.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.0.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.0.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.0.truncate(name, len)
    }
    fn keeps_replaced_formats(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "keeps nothing".into()
    }
}

/// Given a store that keeps no replaced files (the browser's) and an older library with a damaged log, when it is opened and
/// upgraded, then the damaged log set aside is not presented as "the library as it was": there is no snapshot to go with it.
#[test]
fn a_store_that_keeps_no_files_does_not_tell_of_a_kept_damaged_log() {
    let m = MemStore::new();
    m.set("catalog.snap", include_bytes!("../../../catalog/src/fixtures/v6-catalog.snap.json").to_vec());
    m.set("catalog.log", damaged_v6_log().into_bytes());
    let mut s = Session::new();
    let stores =
        LibraryStores { dir: "browser".into(), catalog: Box::new(KeepsNothing(m.clone())), files: Box::new(MemStore::new()), on_disk: false };
    let report = s.open_library_in(stores, false).unwrap();
    assert!(report.damaged.is_some() && report.upgraded_from == Some(6), "the log was found damaged and the library upgraded: {report:?}");
    let told = s.take_library_warnings();
    assert!(!told.iter().any(|w| w.contains("upgraded") || w.contains("catalog.log.damaged-")), "{told:?}");
}

/// The same library on a store that does keep files, with no snapshot (only the damaged log): the person is told where the log is kept.
#[test]
fn a_store_that_keeps_files_tells_of_a_kept_damaged_log() {
    let m = MemStore::new();
    m.set("catalog.log", damaged_v6_log().into_bytes());
    let mut s = Session::new();
    let stores = LibraryStores { dir: "kept".into(), catalog: Box::new(m.clone()), files: Box::new(MemStore::new()), on_disk: false };
    s.open_library_in(stores, false).unwrap();
    let told = s.take_library_warnings();
    assert!(told.iter().any(|w| w.contains("upgraded") && w.contains("catalog.log.damaged-")), "{told:?}");
}
