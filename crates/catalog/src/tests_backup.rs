//! Backing up a library ([`crate::backup`]): a dated folder that opens as the library it was
//! taken of, and that shares with the backup before it the files that didn't change.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::backup::{self, BackupError, BackupOptions, BackupReport, Watch};
use crate::journal::{LOG, SEGMENT_PREFIX, SNAPSHOT, segment_file};
use crate::*;

fn temp(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("lc-backup-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn photo(id: u64) -> Photo {
    Photo::new(
        PhotoId(id),
        Source::File { path: format!("/pictures/2019/trip/IMG_{id:06}.jpg") },
        &format!("IMG_{id:06}.jpg"),
        "JPEG",
        4032,
        3024,
        "2026-10-10T12:00:00",
    )
}

fn run(j: &mut Journal, c: &mut Catalog, ops: Vec<Op>) {
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    j.append(&ops).unwrap();
}

fn add(ids: impl IntoIterator<Item = u64>) -> Vec<Op> {
    ids.into_iter().map(|id| Op::AddPhoto { photo: Box::new(photo(id)) }).collect()
}

/// A library folder of photos `1..=n` in segments of 4, compacted, with one change in its log
/// and a presets file beside the catalog.
fn library(dir: &Path, n: u64) -> (Journal, Catalog) {
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(dir).unwrap())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=n));
    j.snapshot(&c).unwrap();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 3 }]);
    std::fs::write(dir.join("presets.json"), b"[]").unwrap();
    (j, c)
}

fn options(name: &str) -> BackupOptions<'_> {
    BackupOptions { name, also: &["presets.json", "view.json"] }
}

fn back_up(library: &Path, dest: &Path, name: &str) -> BackupReport {
    backup::back_up(library, dest, &options(name), &()).unwrap()
}

/// The catalog a folder opens as.
fn opened(dir: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(dir).unwrap())).unwrap().1
}

/// The files of a folder by the names the catalog gives them: the ones in it, and the ones in
/// its segments folder (`segments/…`). Other folders in it are named too (`Backups`).
fn files(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        if name == "segments" && entry.path().is_dir() {
            names.extend(std::fs::read_dir(entry.path()).unwrap().map(|e| format!("segments/{}", e.unwrap().file_name().into_string().unwrap())));
        } else {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// A copy of a backup's (or library's) files, file by file (no links).
fn copy_of(from: &Path, name: &str) -> PathBuf {
    let to = temp(name);
    std::fs::create_dir_all(to.join("segments")).unwrap();
    for f in files(from) {
        std::fs::copy(from.join(&f), to.join(&f)).unwrap();
    }
    to
}

fn segments(dir: &Path) -> Vec<String> {
    files(dir).into_iter().filter(|n| n.starts_with(SEGMENT_PREFIX)).collect()
}

/// Whether two paths are the same file on disk (one a hard link of the other): `None` where that
/// can't be told (not on Unix), so that an assertion on it is skipped there, not passed.
fn same_file(a: &Path, b: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (std::fs::metadata(a).unwrap(), std::fs::metadata(b).unwrap());
        Some((a.dev(), a.ino()) == (b.dev(), b.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        None
    }
}

/// Given a library, when it is backed up, then the backup is a folder of its own that opens as
/// the library as it was, with its catalog, its log and the settings files beside them.
#[test]
fn a_backup_opens_as_the_library_it_was_taken_of() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, c) = library(&lib, 12);
    let report = back_up(&lib, &dest, "2026-10-10 12.00.00");
    assert_eq!(report.dir, dest.join("2026-10-10 12.00.00"));
    assert_eq!((report.photos, report.segments, report.linked), (12, 3, 0));
    assert_eq!(report.copied, report.files);
    let names = files(&report.dir);
    assert!(names.contains(&SNAPSHOT.to_string()) && names.contains(&LOG.to_string()) && names.contains(&"presets.json".to_string()), "{names:?}");
    assert!(names.contains(&backup::MARKER.to_string()));
    assert_eq!(segments(&report.dir), segments(&lib));
    assert_eq!(
        report.bytes,
        names.iter().filter(|n| *n != backup::MARKER).map(|n| std::fs::metadata(report.dir.join(n)).unwrap().len()).sum::<u64>()
    );
    // the library is untouched and goes on
    drop(j);
    assert_eq!(opened(&lib).to_snapshot(), c.to_snapshot());
    // and the backup opens as it (its log's change included)
    let back = opened(&report.dir);
    assert_eq!(back.to_snapshot(), c.to_snapshot());
    assert_eq!(back.photo(PhotoId(1)).unwrap().rating, 3);
    assert_eq!(files(&dest), ["2026-10-10 12.00.00"], "nothing else is left in the folder");
}

/// Given a backup, when the library changes a little and is backed up again, then the second
/// backup copies what changed and shares the rest with the first: the same files on disk.
#[test]
fn a_second_backup_shares_the_files_that_did_not_change() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (mut j, mut c) = library(&lib, 12);
    // (the change waiting in the log goes into its segment first, so only one changes below)
    j.snapshot(&c).unwrap();
    let first = back_up(&lib, &dest, "2026-10-10 12.00.00");
    let then = c.clone();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(6), rating: 5 }]);
    j.snapshot(&c).unwrap();
    let second = back_up(&lib, &dest, "2026-10-11 12.00.00");
    assert_eq!((second.segments, second.linked), (3, 2), "{second:?}");
    // (`catalog.snap` is copied every time, and in a library this small it is most of the
    // rest: it states the defaults of develop settings, 2.6 kB)
    assert!(second.copied_bytes < first.copied_bytes * 3 / 4, "{} then {}", first.copied_bytes, second.copied_bytes);
    let (old, new) = (segments(&first.dir), segments(&second.dir));
    let shared: Vec<&String> = new.iter().filter(|n| old.contains(n)).collect();
    assert_eq!(shared.len(), 2);
    for name in shared {
        assert_ne!(same_file(&first.dir.join(name), &second.dir.join(name)), Some(false), "{name} was copied");
    }
    // each backup is the library of its day
    assert_eq!(opened(&second.dir).to_snapshot(), c.to_snapshot());
    assert_eq!(opened(&first.dir).to_snapshot(), then.to_snapshot());
}

/// Backups don't depend on each other or on the library: removing an older backup, or the
/// library's own files, leaves a backup that still opens.
#[test]
fn a_backup_stands_on_its_own() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (mut j, mut c) = library(&lib, 12);
    let first = back_up(&lib, &dest, "a");
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(6), rating: 5 }]);
    j.snapshot(&c).unwrap();
    let second = back_up(&lib, &dest, "b");
    let expected = c.to_snapshot();
    // the library moves on and replaces every segment; the first backup is thrown away
    run(&mut j, &mut c, (1..=12).map(|id| Op::SetRating { id: PhotoId(id), rating: 1 }).collect());
    j.snapshot(&c).unwrap();
    drop(j);
    std::fs::remove_dir_all(&first.dir).unwrap();
    std::fs::remove_dir_all(&lib).unwrap();
    assert_eq!(opened(&second.dir).to_snapshot(), expected);
}

/// A watcher that does something to the library while it is being backed up.
struct Meanwhile<F: Fn(&str)> {
    at: F,
    cancel: AtomicBool,
}

impl<F: Fn(&str)> Watch for Meanwhile<F> {
    fn progress(&self, name: &str, _done: u64, _total: u64) {
        (self.at)(name);
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Given a library that is compacted while it is being backed up (the catalog changes under the
/// copy: new segments, old ones removed, the log reset), then the backup is still one moment of
/// the library: it opens, as the catalog before that compaction or after it.
#[test]
fn a_backup_taken_while_the_library_is_saved_is_one_moment_of_it() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, c) = library(&lib, 40);
    let before = c.to_snapshot();
    let state = Mutex::new((j, c, 0u32));
    let watch = Meanwhile {
        at: |name: &str| {
            let mut s = state.lock().unwrap();
            // as a segment is about to be copied, twice over, and then once more just before
            // the log is read (the last moment a compaction can slip in): every segment is replaced
            if (name.starts_with(SEGMENT_PREFIX) && s.2 < 2) || (name == LOG && s.2 == 2) {
                s.2 += 1;
                let (j, c, k) = &mut *s;
                run(j, c, (1..=40).map(|id| Op::SetRating { id: PhotoId(id), rating: (*k % 5) as u8 + 1 }).collect());
                j.snapshot(c).unwrap();
            }
        },
        cancel: AtomicBool::new(false),
    };
    let report = backup::back_up(&lib, &dest, &options("live"), &watch).unwrap();
    let s = state.lock().unwrap();
    assert_eq!(s.2, 3);
    let back = opened(&report.dir).to_snapshot();
    assert!(back == s.1.to_snapshot() || back == before, "the backup is neither the library before nor after");
    assert_eq!(back, s.1.to_snapshot(), "it was started over until the library held still");
    assert_eq!(segments(&report.dir), segments(&lib));
    assert_eq!(files(&dest), ["live"]);
    // nothing of the passes that were started over is left in it
    assert_eq!(backup::check(&report.dir).unwrap(), report);
}

/// Given a backup that is cancelled, or that can't be finished, then no folder of it is left,
/// and the backups there were are as they were.
#[test]
fn a_backup_that_does_not_finish_leaves_nothing() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&lib, 12);
    drop(j);
    let first = back_up(&lib, &dest, "a");
    let kept = files(&first.dir);

    // cancelled after the first file
    struct CancelAfterOne(AtomicU64);
    impl Watch for CancelAfterOne {
        fn progress(&self, _: &str, _: u64, _: u64) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn cancelled(&self) -> bool {
            self.0.load(Ordering::Relaxed) >= 1
        }
    }
    let e = backup::back_up(&lib, &dest, &options("b"), &CancelAfterOne(AtomicU64::new(0))).unwrap_err();
    assert!(matches!(e, BackupError::Cancelled), "{e}");
    assert_eq!(files(&dest), ["a"]);

    // a segment of the library is damaged: no backup is made of a library that doesn't open
    let name = segments(&lib).remove(1);
    let mut bytes = std::fs::read(lib.join(&name)).unwrap();
    let at = bytes.windows(4).position(|w| w == b"4032").unwrap();
    bytes[at] = b'5';
    std::fs::write(lib.join(&name), &bytes).unwrap();
    // (a later backup would share the undamaged copy the first one holds, so make it a new library)
    let fresh = temp("fresh-dest");
    let e = backup::back_up(&lib, &fresh, &options("c"), &()).unwrap_err();
    assert!(matches!(&e, BackupError::Library(msg) if msg.contains(&name)), "{e}");
    assert!(files(&fresh).is_empty());
    assert_eq!((files(&dest), files(&first.dir)), (vec!["a".to_string()], kept));

    // a name that is taken is not written over
    let e = backup::back_up(&lib, &dest, &options("a"), &()).unwrap_err();
    assert!(matches!(e, BackupError::Exists(_)), "{e}");
    assert_eq!(opened(&first.dir).len(), 12);
}

/// What can't be a backup is refused with what is wrong: no library there, a name that is a
/// path, a folder that can't be made.
#[test]
fn what_cannot_be_backed_up_says_why() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let empty = temp("empty");
    let e = backup::back_up(&empty, &dest, &options("a"), &()).unwrap_err();
    assert!(matches!(&e, BackupError::Library(msg) if msg.contains(SNAPSHOT)), "{e}");
    let (j, _c) = library(&lib, 4);
    drop(j);
    for name in ["", ".", "..", "a/b", "a\\b", "../a"] {
        let e = backup::back_up(&lib, &dest, &options(name), &()).unwrap_err();
        assert!(matches!(e, BackupError::Name(_)), "{name:?}: {e}");
    }
    std::fs::write(dest.join("file"), b"x").unwrap();
    let e = backup::back_up(&lib, &dest.join("file"), &options("a"), &()).unwrap_err();
    assert!(matches!(e, BackupError::Io(_)), "{e}");
    assert_eq!(files(&dest), ["file"]);
    // a snapshot that names files outside the library is not followed
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(lib.join(SNAPSHOT)).unwrap()).unwrap();
    v["segments"][0]["name"] = "../presets.json".into();
    std::fs::write(lib.join(SNAPSHOT), serde_json::to_vec(&v).unwrap()).unwrap();
    let e = backup::back_up(&lib, &dest, &options("a"), &()).unwrap_err();
    assert!(matches!(e, BackupError::Library(_)), "{e}");
    assert_eq!(files(&dest), ["file"]);
}

/// A library in one file (not yet in segments, or of a store without them) is backed up as it is.
#[test]
fn a_one_file_library_is_backed_up_too() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    std::fs::write(lib.join(SNAPSHOT), include_str!("fixtures/v8-catalog.snap.json")).unwrap();
    std::fs::write(lib.join(LOG), include_str!("fixtures/v8-catalog.log.jsonl")).unwrap();
    let report = back_up(&lib, &dest, "a");
    assert_eq!((report.segments, report.photos), (0, 3));
    assert_eq!(std::fs::read(report.dir.join(SNAPSHOT)).unwrap(), std::fs::read(lib.join(SNAPSHOT)).unwrap());
    assert_eq!(std::fs::read(report.dir.join(LOG)).unwrap(), std::fs::read(lib.join(LOG)).unwrap());
    // the backup is the old format still; opening it upgrades the backup's copy, not the library
    assert_eq!(opened(&report.dir).len(), 3);
    assert!(std::str::from_utf8(&std::fs::read(lib.join(SNAPSHOT)).unwrap()).unwrap().contains(r#""version":8"#));
}

/// A file is shared with the backup before only once it was read and is what it should be:
/// one that was damaged there (a failing disk) is copied afresh from the library, so damage in
/// an old backup is never handed on to the next.
#[test]
fn a_damaged_file_in_the_backup_before_is_not_shared() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, c) = library(&lib, 12);
    drop(j);
    let first = back_up(&lib, &dest, "a");
    let name = segments(&first.dir).remove(0);
    let mut bytes = std::fs::read(first.dir.join(&name)).unwrap();
    let at = bytes.windows(4).position(|w| w == b"4032").unwrap();
    bytes[at] = b'5';
    std::fs::write(first.dir.join(&name), &bytes).unwrap();
    let second = back_up(&lib, &dest, "b");
    assert_eq!((second.linked, second.segments), (2, 3));
    assert_ne!(same_file(&first.dir.join(&name), &second.dir.join(&name)), Some(true));
    backup::check(&second.dir).unwrap();
    assert_eq!(opened(&second.dir).to_snapshot(), c.to_snapshot());
}

/// What counts as the backup before: the newest folder that is a finished backup. A folder
/// being written, one without a marker, and one whose snapshot lists a file of the same name
/// but another size or checksum share nothing.
#[test]
fn only_a_finished_backup_of_the_same_files_is_shared_with() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&lib, 12);
    drop(j);
    let first = back_up(&lib, &dest, "a");
    // copies of it that are not backups, with names that sort after it
    for (name, marker) in [("z.partial", true), ("z-unmarked", false)] {
        std::fs::create_dir_all(dest.join(name).join("segments")).unwrap();
        for f in files(&first.dir) {
            if marker || f != backup::MARKER {
                std::fs::copy(first.dir.join(&f), dest.join(name).join(&f)).unwrap();
            }
        }
    }
    let second = back_up(&lib, &dest, "b");
    assert_eq!(second.linked, 3);
    for name in segments(&second.dir) {
        assert_ne!(same_file(&first.dir.join(&name), &second.dir.join(&name)), Some(false), "{name}");
    }
    // a backup whose snapshot states another checksum for a file of the same name
    let (other_lib, other_dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&other_lib, 12);
    drop(j);
    let made = back_up(&other_lib, &other_dest, "a");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(made.dir.join(SNAPSHOT)).unwrap()).unwrap();
    v["segments"][0]["crc"] = 7.into();
    std::fs::write(made.dir.join(SNAPSHOT), serde_json::to_vec(&v).unwrap()).unwrap();
    let next = back_up(&other_lib, &other_dest, "b");
    assert_eq!(next.linked, 2);
    assert_eq!(opened(&next.dir).len(), 12);
}

/// A folder left by a backup that was cut short (`<name>.partial`) is never taken for a backup,
/// is in nobody's way while it may still be written (the folder, or its `segments` folder where a
/// backup writes most, changed less than an hour ago), and is removed by a later backup once it
/// is old. A folder of that name that holds anything else is not touched.
// (a folder's time is set through a handle on the folder, which Windows does not give to File::open)
#[cfg(unix)]
#[test]
fn what_a_backup_cut_short_left_is_cleared_away_later() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&lib, 12);
    drop(j);
    let then = || std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    let old_top = |dir: &Path| std::fs::File::open(dir).unwrap().set_modified(then()).unwrap();
    let old = |dir: &Path| {
        old_top(&dir.join("segments"));
        old_top(dir);
    };
    let partial = |name: &str, extra: Option<&str>| {
        let dir = dest.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(dir.join("segments")).unwrap();
        std::fs::write(dir.join(segment_file("1-1")), b"half").unwrap();
        if let Some(extra) = extra {
            std::fs::write(dir.join(extra), b"mine").unwrap();
        }
        dir
    };
    let (stale, fresh, foreign) = (partial("x.partial", None), partial("y.partial", None), partial("z.partial", Some("thesis.txt")));
    // one whose folder is quiet but whose segments are being written now (another backup, to the same place)
    let writing = partial("w.partial", None);
    // (one that Finder visited is still ours)
    std::fs::write(stale.join(".DS_Store"), b"os").unwrap();
    old(&stale);
    old(&foreign);
    old_top(&writing);
    // a name whose partial folder is there is taken
    assert!(matches!(backup::back_up(&lib, &dest, &options("y"), &()), Err(BackupError::Exists(_))));
    back_up(&lib, &dest, "a");
    assert!(!stale.exists(), "an old one is removed");
    assert!(fresh.exists(), "one that may be being written is not");
    assert!(writing.join("segments").exists(), "one whose segments are being written is not");
    assert!(foreign.join("thesis.txt").exists(), "a folder holding something else is not ours to remove");
}

/// Sizes a snapshot states are not trusted: ones that would overflow when added are an error
/// like any other damage, and leave nothing behind.
#[test]
fn absurd_sizes_in_a_snapshot_are_an_error() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&lib, 12);
    drop(j);
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(lib.join(SNAPSHOT)).unwrap()).unwrap();
    for k in 0..2 {
        v["segments"][k]["bytes"] = u64::MAX.into();
    }
    std::fs::write(lib.join(SNAPSHOT), serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(matches!(backup::back_up(&lib, &dest, &options("a"), &()), Err(BackupError::Library(_))));
    assert!(files(&dest).is_empty());
    // and the other files to take along are plain names, none of them the catalog's own
    for bad in ["../x", "a/b", SNAPSHOT, LOG, backup::MARKER, "segments/1-1.json", "segments", ""] {
        let e = backup::back_up(&lib, &dest, &BackupOptions { name: "a", also: &[bad] }, &()).unwrap_err();
        assert!(matches!(e, BackupError::Name(_)), "{bad:?}: {e}");
    }
}

/// A backup lists what it holds, and can be read against that list: a file that was edited by
/// hand (the snapshot, the log, a segment, a settings file), removed, or added since is named.
#[test]
fn a_backup_that_was_touched_is_found_out() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _c) = library(&lib, 12);
    drop(j);
    let made = back_up(&lib, &dest, "a");
    assert_eq!(backup::check(&made.dir).unwrap(), made);
    assert_eq!(made.contents.len() as u64, made.files);
    let segment = segments(&made.dir).remove(1);
    for name in [SNAPSHOT, LOG, "presets.json", segment.as_str()] {
        let copy = copy_of(&made.dir, "touched");
        // one character changed: still JSON, the same length
        let mut bytes = std::fs::read(copy.join(name)).unwrap();
        let at = bytes.iter().position(|b| b.is_ascii_digit()).unwrap_or(0);
        bytes[at] = if bytes[at] == b'7' { b'8' } else { b'7' };
        std::fs::write(copy.join(name), &bytes).unwrap();
        let e = backup::check(&copy).unwrap_err();
        assert!(matches!(&e, BackupError::Changed(msg) if msg.contains(name)), "{name}: {e}");
        // removed
        std::fs::remove_file(copy.join(name)).unwrap();
        let e = backup::check(&copy).unwrap_err();
        assert!(matches!(&e, BackupError::Changed(msg) if msg.contains(name) && msg.contains("missing")), "{name}: {e}");
    }
    // a file that wasn't there
    let copy = copy_of(&made.dir, "added");
    std::fs::write(copy.join("notes.txt"), b"mine").unwrap();
    assert!(matches!(backup::check(&copy), Err(BackupError::Changed(msg)) if msg.contains("notes.txt")));
    // what a file manager writes into a folder somebody looked at is no change of the backup,
    // beside it and among its segments; a file that is none of those is
    let copy = copy_of(&made.dir, "looked-at");
    for name in [".DS_Store", "Thumbs.db", "desktop.ini", "._catalog.snap", "segments/.DS_Store"] {
        std::fs::write(copy.join(name), b"os").unwrap();
    }
    assert_eq!(backup::check(&copy).unwrap(), made);
    std::fs::write(copy.join("segments/notes.txt"), b"mine").unwrap();
    assert!(matches!(backup::check(&copy), Err(BackupError::Changed(msg)) if msg.contains("notes.txt")));
    // a link to the file elsewhere is not the file: the backup has to stand on its own
    #[cfg(unix)]
    {
        let copy = copy_of(&made.dir, "linked");
        std::fs::remove_file(copy.join(&segment)).unwrap();
        std::os::unix::fs::symlink(made.dir.join(&segment), copy.join(&segment)).unwrap();
        assert!(matches!(backup::check(&copy), Err(BackupError::Changed(msg)) if msg.contains(&segment)));
    }
    // a folder that is no backup, and a marker that points outside it
    assert!(matches!(backup::check(&lib), Err(BackupError::Changed(msg)) if msg.contains("not a backup")));
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(made.dir.join(backup::MARKER)).unwrap()).unwrap();
    v["contents"]["../x"] = serde_json::json!({"bytes": 1, "crc": 1});
    let copy = copy_of(&made.dir, "outside");
    std::fs::write(copy.join(backup::MARKER), serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(matches!(backup::check(&copy), Err(BackupError::Changed(msg)) if msg.contains("not a file of the backup")));
}
