//! `library.backup`: a dated folder that opens as the library, sharing unchanged files with the
//! backup before; a row in the activity stack while it runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::json;

use crate::{EngineError, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-backup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(dir: &Path) -> Session {
    let mut s = Session::new();
    s.open_library(dir, true).unwrap();
    s
}

/// The backup in `made`, restored into a folder of its own and opened: a backup is not opened
/// where it is.
/// The folder is removed when the returned guard is dropped (also when a test fails).
fn restored(s: &mut Session, made: &Path, tag: &str) -> (Session, RemoveOnDrop) {
    let to = temp_dir(tag);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": to.to_string_lossy()})).unwrap();
    (open(&to), RemoveOnDrop(to))
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default();
    names.sort();
    names
}

/// Given a library with changes, when it is backed up, then `Backups/<the time>` in its folder
/// opens as the library as it is, and the library goes on as before.
#[test]
fn a_backup_opens_as_the_library() {
    let dir = temp_dir("opens");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let r = s.execute("library.backup", &json!({})).unwrap();
    let made = PathBuf::from(r["report"]["dir"].as_str().unwrap());
    assert_eq!(made, dir.join("Backups").join("2026-09-30 12.00.00 UTC"));
    assert_eq!((r["running"].as_bool(), r["error"].is_null()), (Some(false), true));
    assert_eq!(r["report"]["photos"].as_u64(), Some(s.catalog.len() as u64));
    assert!(s.activity.list().is_empty());
    let expect = s.catalog.to_snapshot();
    // the library goes on; the backup is what it was
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    let (back, _gone) = restored(&mut s, &made, "opens-restored");
    assert_eq!(back.catalog.to_snapshot(), expect);
    assert_eq!(back.catalog.photo(id).unwrap().rating, 4);
    assert_eq!(s.execute("library.backupStatus", &json!({})).unwrap()["report"]["dir"], r["report"]["dir"]);
    drop((s, back));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second backup has a name of its own (also at the same time of day) and shares the files
/// that didn't change with the first.
#[test]
fn a_second_backup_shares_what_did_not_change() {
    let dir = temp_dir("shares");
    let mut s = open(&dir);
    let first = s.execute("library.backup", &json!({})).unwrap();
    let second = s.execute("library.backup", &json!({})).unwrap();
    assert_ne!(first["report"]["dir"], second["report"]["dir"]);
    assert_eq!(backups(&dir.join("Backups")), ["2026-09-30 12.00.00 UTC", "2026-09-30 12.00.00 UTC (2)"]);
    assert_eq!(second["report"]["linked"], second["report"]["segments"]);
    assert!(second["report"]["segments"].as_u64().unwrap() >= 1);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The folder backups go to: one given with the command, or the library's own (a preference
/// saved with it), else `Backups` in the library folder.
#[test]
fn backups_go_to_the_folder_asked_for() {
    let (dir, there, elsewhere) = (temp_dir("where"), temp_dir("where-there"), temp_dir("where-else"));
    let mut s = open(&dir);
    let r = s.execute("library.backup", &json!({"dir": there.to_string_lossy()})).unwrap();
    assert!(PathBuf::from(r["report"]["dir"].as_str().unwrap()).starts_with(&there));
    assert!(backups(&dir.join("Backups")).is_empty(), "given for one backup, not remembered");
    // the library's own folder for them, kept with the library
    // (nothing chosen yet: `folder` says where they go all the same)
    let p = s.execute("library.backupPreferences", &json!({})).unwrap();
    assert!(p["dir"].is_null());
    assert_eq!(p["folder"].as_str().map(PathBuf::from), Some(s.library.as_ref().unwrap().dir.join("Backups")));
    let p = s.execute("library.backupPreferences", &json!({"dir": elsewhere.to_string_lossy()})).unwrap();
    assert_eq!(p["dir"].as_str(), Some(&*elsewhere.to_string_lossy()));
    assert_eq!(p["folder"], p["dir"]);
    s.close_library().unwrap();
    drop(s);
    let mut s = open(&dir);
    assert_eq!(s.execute("library.backupPreferences", &json!({})).unwrap()["dir"].as_str(), Some(&*elsewhere.to_string_lossy()));
    s.execute("library.backup", &json!({})).unwrap();
    assert_eq!(backups(&elsewhere).len(), 1);
    // and back to the default
    let p = s.execute("library.backupPreferences", &json!({"dir": null})).unwrap();
    assert!(p["dir"].is_null());
    assert_eq!(p["folder"].as_str().map(PathBuf::from), Some(s.library.as_ref().unwrap().dir.join("Backups")));
    s.execute("library.backup", &json!({})).unwrap();
    assert_eq!(backups(&dir.join("Backups")).len(), 1);
    drop(s);
    for d in [dir, there, elsewhere] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// While a backup runs it has a row in the activity stack that can stop it; stopped, it leaves
/// no folder, says so, and the row is gone.
#[test]
fn a_running_backup_shows_in_the_activity_stack_and_can_be_stopped() {
    let dir = temp_dir("row");
    let mut s = open(&dir);
    s.backup_hold.store(true, Ordering::SeqCst);
    let r = s.execute("library.backup", &json!({"background": true})).unwrap();
    assert_eq!(r["running"].as_bool(), Some(true));
    let rows = s.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].cancellable), ("backup", "Backing up the library", true));
    assert_eq!(rows[0].unit, crate::activity::Unit::Bytes);
    // one at a time (the menu item is off meanwhile)
    assert!(matches!(s.execute("library.backup", &json!({})), Err(EngineError::Disabled { .. } | EngineError::BadParams { .. })));
    s.activity.cancel(rows[0].id).unwrap();
    s.backup_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.activity.list().is_empty());
    let status = s.execute("library.backupStatus", &json!({})).unwrap();
    assert_eq!((status["running"].as_bool(), status["cancelled"].as_bool()), (Some(false), Some(true)));
    assert!(status["report"].is_null() && status["error"].as_str().is_some_and(|e| e.contains("cancelled")), "{status}");
    assert!(backups(&dir.join("Backups")).is_empty());
    // and the next one runs, and says how far it got
    s.backup_hold.store(true, Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    s.backup_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while s.execute("library.backupStatus", &json!({})).unwrap()["running"] == json!(true) && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let done = s.execute("library.backupStatus", &json!({})).unwrap();
    assert!(done["error"].is_null() && done["cancelled"] == json!(false), "{done}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What can't be backed up says why: a library that isn't a folder, or no library.
#[test]
fn a_library_without_a_folder_is_not_backed_up() {
    let mut s = Session::with_demo();
    let e = s.execute("library.backup", &json!({})).unwrap_err();
    assert!(e.to_string().contains("folder"), "{e}");
    assert!(s.activity.list().is_empty());
    // a folder that can't be written to is the backup's error, not a panic; nothing is left
    let (dir, outside) = (temp_dir("bad-dest"), temp_dir("bad-dest-outside"));
    let mut s = open(&dir);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("not-a-folder"), b"x").unwrap();
    let r = s.execute("library.backup", &json!({"dir": outside.join("not-a-folder").to_string_lossy()})).unwrap();
    assert!(r["report"].is_null() && r["error"].as_str().is_some(), "{r}");
    assert!(s.activity.list().is_empty());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A backup can be checked against what it listed when it was made: untouched it is sound; a
/// file edited by hand is named.
#[test]
fn a_backup_is_checked_against_its_own_list() {
    let dir = temp_dir("check");
    let mut s = open(&dir);
    let r = s.execute("library.backup", &json!({})).unwrap();
    let made = PathBuf::from(r["report"]["dir"].as_str().unwrap());
    let sound = s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap();
    assert_eq!((sound["ok"].as_bool(), &sound["report"]["dir"]), (Some(true), &r["report"]["dir"]));
    let snap = made.join("catalog.snap");
    let edited = std::fs::read_to_string(&snap).unwrap().replacen("\"seq\":", "\"seq\": ", 1);
    std::fs::write(&snap, edited).unwrap();
    let touched = s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap();
    assert_eq!(touched["ok"].as_bool(), Some(false));
    assert!(touched["error"].as_str().unwrap().contains("catalog.snap"), "{touched}");
    assert!(s.execute("library.checkBackup", &json!({})).is_err());
    assert_eq!(
        s.execute("library.checkBackup", &json!({"dir": dir.to_string_lossy()})).unwrap()["ok"].as_bool(),
        Some(false),
        "a library is not a backup"
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup holds every change made so far, also one that was not yet written to the library's
/// log when the backup was asked for; and the library's settings files.
#[test]
fn a_backup_holds_what_was_not_saved_yet() {
    let dir = temp_dir("unsaved");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("library.backupPreferences", &json!({"dir": null})).unwrap();
    // (committed, not yet persisted: commands persist as they return, this does not)
    s.commit("Rate", lightcraft_catalog::Op::SetRating { id, rating: 5 }).unwrap();
    let r = s.execute("library.backup", &json!({})).unwrap();
    let made = PathBuf::from(r["report"]["dir"].as_str().unwrap());
    for name in ["catalog.snap", "prefs.json", "backup.json"] {
        assert!(made.join(name).is_file(), "{name}");
    }
    let (back, _gone) = restored(&mut s, &made, "unsaved-restored");
    assert_eq!(back.catalog.photo(id).unwrap().rating, 5);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Parameters that aren't what they should be are an error, not a backup somewhere else: a
/// folder that isn't a path, a relative one, a flag that isn't one.
#[test]
fn bad_parameters_are_refused() {
    let dir = temp_dir("params");
    let mut s = open(&dir);
    for p in [json!({"dir": 5}), json!({"dir": ["x"]}), json!({"dir": ""}), json!({"dir": "relative/there"}), json!({"background": "yes"})] {
        assert!(matches!(s.execute("library.backup", &p), Err(EngineError::BadParams { .. })), "{p}");
    }
    assert!(matches!(s.execute("library.backupPreferences", &json!({"dir": "relative"})), Err(EngineError::BadParams { .. })));
    assert!(matches!(s.execute("library.checkBackup", &json!({"dir": "relative"})), Err(EngineError::BadParams { .. })));
    assert!(backups(&dir.join("Backups")).is_empty() && s.activity.list().is_empty());
    // a name taken by a backup that was cut short is stepped around
    std::fs::create_dir_all(dir.join("Backups").join("2026-09-30 12.00.00 UTC.partial")).unwrap();
    let r = s.execute("library.backup", &json!({})).unwrap();
    assert!(r["report"]["dir"].as_str().unwrap().ends_with("2026-09-30 12.00.00 UTC (2)"), "{r}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup belongs to the library it is of: another library opened in the session has no
/// backup status, and can be backed up while the first one's still runs.
#[test]
fn a_backup_is_its_librarys() {
    let (a, b) = (temp_dir("lib-a"), temp_dir("lib-b"));
    let mut s = open(&a);
    s.backup_hold.store(true, Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    s.open_library(&b, true).unwrap();
    assert!(s.execute("library.backupStatus", &json!({})).unwrap().is_null());
    s.backup_hold.store(false, Ordering::SeqCst);
    let r = s.execute("library.backup", &json!({})).unwrap();
    assert!(PathBuf::from(r["report"]["dir"].as_str().unwrap()).starts_with(b.join("Backups")), "{r}");
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(backups(&a.join("Backups")).len(), 1);
    drop(s);
    for d in [a, b] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a library with every one of its own files (presets, view, preferences), when it is
/// backed up, then the backup has each of them, as it was in the library when the backup was
/// made. (The library's preferences then note the backup: that is after it.)
#[test]
fn a_backup_takes_every_file_of_the_library() {
    use crate::library::SettingsFile;
    let dir = temp_dir("every-file");
    let mut s = open(&dir);
    // (a library gets its id at its first backup, and notes it in its preferences before the
    // backup is made: this one has it already)
    s.library_id = Some("a library".into());
    for file in SettingsFile::ALL {
        std::fs::write(dir.join(file.name()), format!("{{\"file\":\"{}\"}}", file.name())).unwrap();
    }
    let made = s.execute("library.backup", &json!({})).unwrap();
    let backup = PathBuf::from(made["report"]["dir"].as_str().unwrap());
    for file in SettingsFile::ALL {
        assert_eq!(std::fs::read_to_string(backup.join(file.name())).unwrap(), format!("{{\"file\":\"{}\"}}", file.name()), "{}", file.name());
    }
    assert_eq!(crate::library::SETTINGS_FILES.to_vec(), SettingsFile::ALL.map(SettingsFile::name).to_vec());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given an open library, when something writes a file beside the catalog that the library does
/// not list, then it is refused, and the error says where to list it: a file the library writes
/// is a file its backups take along, so there is no writing one without listing it. A listed
/// file, and a damaged one set aside under its name, are written.
#[test]
fn the_library_writes_only_the_files_it_lists() {
    let dir = temp_dir("listed-files");
    let mut s = open(&dir);
    let files = &mut s.library.as_mut().unwrap().files;
    let e = files.write_atomic("locations.json", b"{}").unwrap_err();
    assert!(e.to_string().contains("locations.json") && e.to_string().contains("SettingsFile"), "{e}");
    assert!(files.append("locations.json", b"{}").is_err());
    assert!(!dir.join("locations.json").exists());
    files.write_atomic("view.json", b"{}").unwrap();
    files.write_atomic("prefs.json.corrupt-1", b"{").unwrap();
    assert!(dir.join("view.json").exists() && dir.join("prefs.json.corrupt-1").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library on disk, when a folder is given for its backups that is one of the library's
/// own (inside the library folder, but for `Backups`) or a backup, then it is refused with the
/// reason and nothing changes: a restore moves the library's folders as the catalog's, and a
/// backup is not added to. A folder outside the library, and `Backups` in it, are taken.
#[test]
fn backups_do_not_go_into_the_librarys_own_folders_or_into_a_backup() {
    let (dir, outside) = (temp_dir("own-folders"), temp_dir("own-folders-outside"));
    let mut s = open(&dir);
    let made = s.execute("library.backup", &json!({})).unwrap();
    let a_backup = PathBuf::from(made["report"]["dir"].as_str().unwrap());
    let library = s.library.as_ref().unwrap().dir.clone();
    for (bad, why) in [
        (library.clone(), "inside the library folder"),
        (library.join("segments"), "inside the library folder"),
        (library.join("Replaced"), "inside the library folder"),
        (library.join("Backups").join("mine"), "inside the library folder"),
        (a_backup.clone(), "inside the library folder"),
    ] {
        for command in ["library.backupPreferences", "library.backup"] {
            let e = s.execute(command, &json!({"dir": bad.to_string_lossy()})).unwrap_err().to_string();
            assert!(e.contains(why), "{command} {}: {e}", bad.display());
        }
    }
    assert!(s.execute("library.backupPreferences", &json!({})).unwrap()["dir"].is_null());
    // a backup that is elsewhere is no place for backups either
    let elsewhere = s.execute("library.backup", &json!({"dir": outside.to_string_lossy()})).unwrap();
    let there = PathBuf::from(elsewhere["report"]["dir"].as_str().unwrap());
    let e = s.execute("library.backupPreferences", &json!({"dir": there.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("is a backup"), "{e}");
    let e = s.execute("library.backupPreferences", &json!({"dir": there.join("deeper").to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("inside one"), "{e}");
    let e = s.execute("library.backup", &json!({"dir": there.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("is a backup"), "{e}");
    // a path that only reads as outside: `..` after a folder that is yet to be made
    let round = format!("{}/not-made/../../{}/segments", outside.display(), library.file_name().unwrap().to_string_lossy());
    for command in ["library.backupPreferences", "library.backup"] {
        let e = s.execute(command, &json!({"dir": round})).unwrap_err().to_string();
        assert!(e.contains("without `..`"), "{command}: {e}");
    }
    assert!(!outside.join("not-made").exists() && !library.join("segments").join("Backups").exists());
    // a file that is merely called what a backup's list is called makes nothing a backup
    let stray = temp_dir("own-folders-stray");
    std::fs::create_dir_all(stray.join("deeper")).unwrap();
    std::fs::write(stray.join("backup.json"), b"{\"an\": \"export\"}").unwrap();
    let p = s.execute("library.backupPreferences", &json!({"dir": stray.join("deeper").to_string_lossy()})).unwrap();
    assert_eq!(p["dir"].as_str(), Some(&*stray.join("deeper").to_string_lossy()));
    let _ = std::fs::remove_dir_all(&stray);
    // what is taken: a folder outside, and `Backups` in the library, which is the usual place
    // and so not a choice
    let p = s.execute("library.backupPreferences", &json!({"dir": outside.to_string_lossy()})).unwrap();
    assert_eq!(p["dir"].as_str(), Some(&*outside.to_string_lossy()));
    let p = s.execute("library.backupPreferences", &json!({"dir": library.join("Backups").to_string_lossy()})).unwrap();
    assert!(p["dir"].is_null(), "{p}");
    assert_eq!(p["folder"].as_str().map(PathBuf::from), Some(library.join("Backups")));
    // a folder set before the rule (or by hand in the preferences) is not used either
    s.backup_dir = Some(library.join("segments"));
    let e = s.execute("library.backup", &json!({})).unwrap_err().to_string();
    assert!(e.contains("can't be used") && e.contains("inside the library folder"), "{e}");
    s.backup_dir = None;
    drop(s);
    for d in [dir, outside] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// `choose` asks with a folder dialog, which only the app has: here it is refused, with what to
/// do instead, and is not taken for a question about the folder.
#[test]
fn choosing_with_a_dialog_is_refused_where_there_is_none() {
    let dir = temp_dir("choose");
    let mut s = open(&dir);
    let e = s.execute("library.backupPreferences", &json!({"choose": true})).unwrap_err().to_string();
    assert!(e.contains("no folder dialog") && e.contains("`dir`"), "{e}");
    assert!(s.execute("library.backupPreferences", &json!({"choose": false})).unwrap()["dir"].is_null());
    // (and it is true or false whatever else is given)
    let e = s.execute("library.backupPreferences", &json!({"dir": null, "choose": "yes"})).unwrap_err().to_string();
    assert!(e.contains("`choose`"), "{e}");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given preferences that can't be saved (prefs.json was unreadable when the library opened, so it
/// is not overwritten), when a folder for backups is chosen, then the choice fails and is not made:
/// backups go where they went.
#[test]
fn a_backup_folder_that_could_not_be_saved_is_not_chosen() {
    let (dir, other) = (temp_dir("unsaved-choice"), temp_dir("unsaved-choice-other"));
    let mut s = open(&dir);
    s.library.as_mut().unwrap().blocked.push("prefs.json");
    let e = s.execute("library.backupPreferences", &json!({"dir": other.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("prefs"), "{e}");
    assert!(s.backup_dir.is_none());
    let p = s.execute("library.backupPreferences", &json!({})).unwrap();
    assert_eq!(p["dir"], json!(null), "{p}");
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The sentences an app says in its person's language are the ones the commands give: a
/// refusal's reason is one of them word for word.
#[test]
fn the_reasons_a_backup_or_restore_is_refused_are_the_listed_sentences() {
    use crate::cmd::backup::{BACKUP_RUNNING, CANCELLED, NOT_ON_DISK, REFUSALS};
    assert_eq!(lightcraft_catalog::backup::BackupError::Cancelled.to_string(), CANCELLED);
    let mut memory = Session::with_demo();
    let e = memory.execute("library.backup", &json!({})).unwrap_err().to_string();
    assert_eq!(e, format!("command `library.backup` is not available right now: {NOT_ON_DISK}"));
    let dir = temp_dir("refusals");
    let mut s = open(&dir);
    s.backup_hold.store(true, Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    let e = s.execute("library.restore", &json!({"dir": dir.to_string_lossy()})).unwrap_err().to_string();
    assert_eq!(e, format!("command `library.restore` is not available right now: {BACKUP_RUNNING}"));
    s.backup_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(REFUSALS.iter().all(|r| !r.is_empty() && !r.contains('{')));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
