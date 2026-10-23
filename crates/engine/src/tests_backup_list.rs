//! `library.backups` as the Restore Library from Backup list reads it: each backup by name, newest
//! first, and whether it is of the open library.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-backup-list-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A library on disk in `dir`, and the clock its session goes by.
fn open(dir: &Path) -> (Session, Arc<Mutex<String>>) {
    let mut s = Session::new();
    let now = Arc::new(Mutex::new("2026-01-10T09:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    s.open_library(dir, true).unwrap();
    (s, now)
}

/// Back the library up at `time`; the backup's answer.
fn back_up_at(s: &mut Session, now: &Arc<Mutex<String>>, time: &str) -> Value {
    *now.lock().unwrap() = time.to_string();
    let r = s.execute("library.backup", &json!({})).unwrap();
    assert!(r["error"].is_null(), "{r}");
    s.persist_if_dirty();
    r
}

fn prefs(s: &mut Session, p: Value) -> Value {
    s.execute("library.backupPreferences", &p).unwrap()
}

/// `library.backups` lists the backups in a folder for someone to choose from: each by its
/// name, with when it was made and whether it is of the library in the open folder; and it says
/// when the folder asked about is a backup itself (and so holds none).
#[test]
fn the_backups_in_a_folder_are_listed_to_choose_from() {
    let (a, b, shared) = (temp_dir("list-a"), temp_dir("list-b"), temp_dir("list-shared"));
    let (mut s, now) = open(&a);
    prefs(&mut s, json!({"dir": shared.to_string_lossy()}));
    back_up_at(&mut s, &now, "2026-01-10T09:00:00");
    s.close_library().unwrap();
    s.open_library(&b, true).unwrap();
    prefs(&mut s, json!({"dir": shared.to_string_lossy()}));
    back_up_at(&mut s, &now, "2026-01-11T09:00:00");
    back_up_at(&mut s, &now, "2026-01-12T09:00:00");

    let r = s.execute("library.backups", &json!({})).unwrap();
    assert_eq!(r["isBackup"], json!(false), "{r}");
    let listed: Vec<(&str, &str, bool)> = r["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["name"].as_str().unwrap(), b["made"].as_str().unwrap(), b["ofThisLibrary"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        listed,
        [
            ("2026-01-12 09.00.00 UTC", "2026-01-12T09:00:00", true),
            ("2026-01-11 09.00.00 UTC", "2026-01-11T09:00:00", true),
            ("2026-01-10 09.00.00 UTC", "2026-01-10T09:00:00", false),
        ],
        "newest first; A's is not of the open library"
    );
    // a library that was moved still knows its backups; one made anew where it was does not
    // take them for its own
    s.close_library().unwrap();
    drop(s);
    let moved = temp_dir("list-moved");
    std::fs::rename(&b, &moved).unwrap();
    let own = |s: &mut Session| -> Vec<bool> {
        let r = s.execute("library.backups", &json!({"dir": shared.to_string_lossy()})).unwrap();
        r["backups"].as_array().unwrap().iter().map(|b| b["ofThisLibrary"].as_bool().unwrap()).collect()
    };
    let (mut s, _) = open(&moved);
    assert_eq!(own(&mut s), [true, true, false]);
    s.close_library().unwrap();
    drop(s);
    let (mut s, now) = open(&b);
    assert_eq!(own(&mut s), [false, false, false], "a new library in B's place, never backed up");
    prefs(&mut s, json!({"dir": shared.to_string_lossy()}));
    back_up_at(&mut s, &now, "2026-01-09T09:00:00");
    assert_eq!(own(&mut s), [false, false, false, true], "its own is the oldest by its time");
    // asked about a backup itself
    let one = shared.join("2026-01-10 09.00.00 UTC");
    let r = s.execute("library.backups", &json!({"dir": one.to_string_lossy()})).unwrap();
    assert_eq!((r["isBackup"].clone(), r["backups"].clone()), (json!(true), json!([])), "{r}");
    drop(s);
    for d in [a, b, shared, moved] {
        let _ = std::fs::remove_dir_all(d);
    }
}
