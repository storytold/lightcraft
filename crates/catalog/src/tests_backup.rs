//! Backing up a library ([`crate::backup`]): a dated folder that opens as the library it was
//! taken of, and that shares with the backup before it the files that didn't change.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::backup::{self, BackupError, BackupOptions, BackupReport, Watch};
use crate::journal::{LOG, SEGMENT_PREFIX, SNAPSHOT, segment_file};
use crate::*;

pub(crate) fn temp(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("lc-backup-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn photo(id: u64) -> Photo {
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

pub(crate) fn run(j: &mut Journal, c: &mut Catalog, ops: Vec<Op>) {
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    j.append(&ops).unwrap();
}

pub(crate) fn add(ids: impl IntoIterator<Item = u64>) -> Vec<Op> {
    ids.into_iter().map(|id| Op::AddPhoto { photo: Box::new(photo(id)) }).collect()
}

/// A library folder of photos `1..=n` in segments of 4, compacted, with one change in its log
/// and a presets file beside the catalog.
pub(crate) fn library(dir: &Path, n: u64) -> (Journal, Catalog) {
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(dir).unwrap())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=n));
    j.snapshot(&c).unwrap();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 3 }]);
    std::fs::write(dir.join("presets.json"), b"[]").unwrap();
    (j, c)
}

pub(crate) fn options(name: &str) -> BackupOptions<'_> {
    BackupOptions { name, also: &["presets.json", "view.json"], made: None, library_id: None }
}

pub(crate) fn back_up(library: &Path, dest: &Path, name: &str) -> BackupReport {
    backup::back_up(library, dest, &options(name), &()).unwrap()
}

/// The catalog a folder opens as.
pub(crate) fn opened(dir: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(dir).unwrap())).unwrap().1
}

/// The files of a folder by the names the catalog gives them: the ones in it, and the ones in
/// its segments folder (`segments/…`). Other folders in it are named too (`Backups`).
pub(crate) fn files(dir: &Path) -> Vec<String> {
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
pub(crate) fn copy_of(from: &Path, name: &str) -> PathBuf {
    let to = temp(name);
    std::fs::create_dir_all(to.join("segments")).unwrap();
    for f in files(from) {
        std::fs::copy(from.join(&f), to.join(&f)).unwrap();
    }
    to
}

pub(crate) fn segments(dir: &Path) -> Vec<String> {
    files(dir).into_iter().filter(|n| n.starts_with(SEGMENT_PREFIX)).collect()
}

/// Whether two paths are the same file on disk (one a hard link of the other): `None` where that
/// can't be told (not on Unix), so that an assertion on it is skipped there, not passed.
pub(crate) fn same_file(a: &Path, b: &Path) -> Option<bool> {
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
        let e = backup::back_up(&lib, &dest, &BackupOptions { name: "a", also: &[bad], made: None, library_id: None }, &()).unwrap_err();
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

// ---- backups that are older than a library keeps them

/// A backup of `library` (whose id is `id`) in `dest`, made at `made` (and named after it).
fn back_up_at(library: &Path, id: &str, dest: &Path, made: &str) -> BackupReport {
    let name = made.replace(':', ".").replace('T', " ");
    backup::back_up(library, dest, &BackupOptions { made: Some(made), library_id: Some(id), ..options(&name) }, &()).unwrap()
}

fn names(reports: &[BackupReport]) -> Vec<String> {
    reports.iter().map(|r| r.dir.file_name().unwrap().to_string_lossy().into_owned()).collect()
}

/// The backups of `library` (id `id`) in `dest` that are older than `before`, seen in 2030.
fn old(dest: &Path, library: &Path, id: &str, before: &str) -> Vec<String> {
    names(&backup::outdated(dest, backup::Own { library, id }, before, "2030-01-01T00:00:00"))
}

/// Given a backup, then its list of files says when it was made and which library it is of.
#[test]
fn a_backup_says_when_it_was_made_and_what_it_is_of() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _) = library(&lib, 6);
    drop(j);
    let made = back_up_at(&lib, "one", &dest, "2026-10-11T09:00:00");
    assert_eq!(made.made.as_deref(), Some("2026-10-11T09:00:00"));
    assert_eq!((made.library.clone(), made.library_id.as_deref()), (Some(std::fs::canonicalize(&lib).unwrap()), Some("one")));
    let listed = backup::list(&dest);
    assert_eq!((listed[0].made.clone(), listed[0].library.clone(), listed[0].library_id.clone()), (made.made, made.library, made.library_id));
    // and the backup is still what its list says, and opens
    backup::check(&made.dir).unwrap();
}

/// Given a library's backups of January to June, when the ones older than a time are asked
/// for, then they are the ones made before then, oldest first, but never one of its three
/// newest, however old: there is always more than the backup just made.
#[test]
fn the_backups_a_library_no_longer_keeps_are_its_old_ones_but_never_its_newest_three() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _) = library(&lib, 6);
    drop(j);
    for month in 1..=6 {
        back_up_at(&lib, "one", &dest, &format!("2026-0{month}-10T09:00:00"));
    }
    assert_eq!(old(&dest, &lib, "one", "2026-01-01T00:00:00"), Vec::<String>::new(), "none is that old");
    assert_eq!(old(&dest, &lib, "one", "2026-03-01T00:00:00"), ["2026-01-10 09.00.00", "2026-02-10 09.00.00"]);
    assert_eq!(
        old(&dest, &lib, "one", "2029-01-01T00:00:00"),
        ["2026-01-10 09.00.00", "2026-02-10 09.00.00", "2026-03-10 09.00.00"],
        "the three newest stay, however old"
    );
    assert_eq!(backup::KEEP_AT_LEAST, 3);
}

/// Given a folder that also holds what is not certainly an old backup of this library, then
/// none of it is among the backups the library no longer keeps: another library's backups; the
/// backups of a library that was at this path before (another id); a backup set aside under
/// another name; one from before backups said when and of what; one cut short; a folder of the
/// person's own; and backups whose time is yet to come, or is no time at all.
#[test]
fn only_what_is_certainly_an_old_backup_of_the_library_is_one() {
    let (lib, other, dest) = (temp("lib"), temp("other"), temp("dest"));
    for l in [&lib, &other] {
        let (j, _) = library(l, 6);
        drop(j);
    }
    for month in 1..=5 {
        back_up_at(&lib, "one", &dest, &format!("2026-0{month}-10T09:00:00"));
    }
    let all = "2029-01-01T00:00:00";
    assert_eq!(old(&dest, &lib, "one", all), ["2026-01-10 09.00.00", "2026-02-10 09.00.00"]);

    // another library's, and one of a library that was here before: older than all of them
    back_up_at(&other, "two", &dest, "2025-01-01T00:00:00");
    back_up_at(&lib, "before", &dest, "2025-02-01T00:00:00");
    // the January backup set aside under a name of the person's own
    let kept = dest.join("2026-01-10 keep");
    std::fs::rename(dest.join("2026-01-10 09.00.00"), &kept).unwrap();
    // from before backups said when and of what; cut short; the person's own folder
    backup::back_up(&lib, &dest, &options("unsigned"), &()).unwrap();
    std::fs::create_dir_all(dest.join("2025-12-01 09.00.00.partial")).unwrap();
    std::fs::create_dir_all(dest.join("holiday photos")).unwrap();
    std::fs::write(dest.join("holiday photos").join("backup.json"), b"{\"not\": \"a backup\"}").unwrap();
    // a time yet to come (the clock was wrong), and no time at all
    back_up_at(&lib, "one", &dest, "2031-01-01T00:00:00");
    backup::back_up(&lib, &dest, &BackupOptions { made: Some("0000"), library_id: Some("one"), ..options("no time") }, &()).unwrap();

    assert_eq!(old(&dest, &lib, "one", all), ["2026-02-10 09.00.00"], "what is left of its own: Feb to May, the newest three stay");
    // the other library, and the one that was here before, have their own to keep or not
    assert_eq!(old(&dest, &other, "two", all), Vec::<String>::new());
    assert_eq!(old(&dest, &lib, "before", all), Vec::<String>::new());
    // and a backup dated after "now" is not counted as the newest either: seen in 2026, the real
    // newest three are March to May
    assert_eq!(names(&backup::outdated(&dest, backup::Own { library: &lib, id: "one" }, all, "2026-06-01T00:00:00")), ["2026-02-10 09.00.00"]);
}

/// Given backups that share their unchanged files, when an old one is removed, then it is gone
/// as a whole, and the ones that stay are whole: each still is what its list says, and opens.
/// Only a backup is removed: a folder that is none is refused and left as it is.
#[test]
fn removing_an_old_backup_takes_nothing_from_the_others() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (mut j, mut c) = library(&lib, 12);
    j.snapshot(&c).unwrap();
    let first = back_up_at(&lib, "one", &dest, "2026-01-10T09:00:00");
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(6), rating: 5 }]);
    j.snapshot(&c).unwrap();
    let second = back_up_at(&lib, "one", &dest, "2026-06-10T09:00:00");
    assert!(second.linked > 0, "they share files: {second:?}");
    drop(j);

    backup::remove(&first.dir).unwrap();
    assert!(!first.dir.exists());
    assert_eq!(names(&backup::list(&dest)), ["2026-06-10 09.00.00"]);
    backup::check(&second.dir).unwrap();

    let mine = dest.join("holiday photos");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("a.jpg"), b"x").unwrap();
    assert!(backup::remove(&mine).is_err());
    assert!(backup::remove(&lib).is_err(), "a library is no backup");
    assert!(mine.join("a.jpg").exists() && lib.join("catalog.snap").exists());
}

/// Given two copies of a library (the same id) in two folders that back up to one folder, then
/// neither's old backups are the other's to remove: a backup is a library's own by where the
/// library is, too.
#[test]
fn a_copy_of_a_library_elsewhere_keeps_its_own_backups() {
    let (lib, copy, dest) = (temp("lib"), temp("copy"), temp("dest"));
    for l in [&lib, &copy] {
        let (j, _) = library(l, 6);
        drop(j);
    }
    for month in 1..=4 {
        back_up_at(&copy, "one", &dest, &format!("2025-0{month}-10T09:00:00"));
        back_up_at(&lib, "one", &dest, &format!("2026-0{month}-10T09:00:00"));
    }
    let all = "2029-01-01T00:00:00";
    assert_eq!(old(&dest, &lib, "one", all), ["2026-01-10 09.00.00"], "its own oldest, none of the copy's of 2025");
    assert_eq!(old(&dest, &copy, "one", all), ["2025-01-10 09.00.00"]);
}

/// The times a backup may say it was made at: one made exactly when backups begin to be kept
/// is kept; one made this very moment counts (it is among the newest); and a time that is no
/// time (the 30th of February, a time with a zone after it) says nothing, so that backup stays.
#[test]
fn a_backup_is_old_only_by_a_time_that_is_one() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _) = library(&lib, 6);
    drop(j);
    for made in ["2026-01-10T09:00:00", "2026-02-10T09:00:00", "2026-03-10T09:00:00", "2026-04-10T09:00:00", "2026-05-10T09:00:00"] {
        back_up_at(&lib, "one", &dest, made);
    }
    for (name, no_time) in [("feb 30", "2026-02-30T00:00:00"), ("zoned", "2026-01-01T00:00:00Z"), ("empty", ""), ("words", "a while ago, I think")] {
        backup::back_up(&lib, &dest, &BackupOptions { made: Some(no_time), library_id: Some("one"), ..options(name) }, &()).unwrap();
    }
    let own = backup::Own { library: &lib, id: "one" };
    let at = |before: &str, now: &str| names(&backup::outdated(&dest, own, before, now));
    assert_eq!(at("2026-02-10T09:00:00", "2030-01-01T00:00:00"), ["2026-01-10 09.00.00"], "made exactly then: kept");
    assert_eq!(at("2026-02-10T09:00:01", "2030-01-01T00:00:00"), ["2026-01-10 09.00.00", "2026-02-10 09.00.00"]);
    // now is the moment the newest was made: it counts as one of the three
    assert_eq!(at("2029-01-01T00:00:00", "2026-05-10T09:00:00"), ["2026-01-10 09.00.00", "2026-02-10 09.00.00"]);
    // a moment earlier it is yet to come, and March to April with February are the three
    assert_eq!(at("2029-01-01T00:00:00", "2026-05-10T08:59:59"), ["2026-01-10 09.00.00"]);
    // and a clock that says nothing makes nothing old
    assert_eq!(at("2029-01-01T00:00:00", ""), Vec::<String>::new());
}

/// Given a backup that something else was put into since it was made (notes, an export), when
/// it is to be removed, then it is refused and everything in it stays: what a person put there
/// is not the library's to remove. So is one whose `<name>.partial` is taken.
#[test]
fn a_backup_that_holds_something_else_is_not_removed() {
    let (lib, dest) = (temp("lib"), temp("dest"));
    let (j, _) = library(&lib, 6);
    drop(j);
    let with_notes = back_up_at(&lib, "one", &dest, "2026-01-10T09:00:00");
    let with_folder = back_up_at(&lib, "one", &dest, "2026-02-10T09:00:00");
    let in_the_way = back_up_at(&lib, "one", &dest, "2026-03-10T09:00:00");
    std::fs::write(with_notes.dir.join("notes.txt"), b"before the trip").unwrap();
    std::fs::create_dir_all(with_folder.dir.join("exports")).unwrap();
    std::fs::write(with_folder.dir.join("exports").join("a.jpg"), b"x").unwrap();
    std::fs::create_dir_all(dest.join("2026-03-10 09.00.00.partial")).unwrap();

    // what Finder wrote into a folder that was looked at is no reason to keep a backup
    let browsed = back_up_at(&lib, "one", &dest, "2026-04-10T09:00:00");
    std::fs::write(browsed.dir.join(".DS_Store"), b"os").unwrap();
    backup::remove(&browsed.dir).unwrap();
    assert!(!browsed.dir.exists());

    let e = backup::remove(&with_notes.dir).unwrap_err().to_string();
    assert!(e.contains("notes.txt") && e.contains("nothing was removed"), "{e}");
    let e = backup::remove(&with_folder.dir).unwrap_err().to_string();
    assert!(e.contains("exports") && e.contains("nothing was removed"), "{e}");
    let e = backup::remove(&in_the_way.dir).unwrap_err().to_string();
    assert!(e.contains("in the way") && e.contains("nothing was removed"), "{e}");
    assert!(with_notes.dir.join("notes.txt").is_file() && with_folder.dir.join("exports").join("a.jpg").is_file());
    for b in [&with_notes, &with_folder, &in_the_way] {
        assert!(b.dir.join("backup.json").is_file() && b.dir.join("catalog.snap").is_file(), "{}", b.dir.display());
    }
    backup::check(&in_the_way.dir).unwrap();
}

/// Given a link in the folder backups go to that leads to a backup kept elsewhere, then that
/// backup is not among the ones the library no longer keeps, and removing by the link is
/// refused: only folders of the backup folder itself are the library's to remove.
#[cfg(unix)]
#[test]
fn a_link_to_a_backup_elsewhere_is_not_followed() {
    let (lib, dest, vault) = (temp("lib"), temp("dest"), temp("vault"));
    let (j, _) = library(&lib, 6);
    drop(j);
    let kept = back_up_at(&lib, "one", &vault, "2025-01-10T09:00:00");
    for month in 1..=4 {
        back_up_at(&lib, "one", &dest, &format!("2026-0{month}-10T09:00:00"));
    }
    let link = dest.join("2025-01-10 09.00.00");
    std::os::unix::fs::symlink(&kept.dir, &link).unwrap();
    assert_eq!(old(&dest, &lib, "one", "2029-01-01T00:00:00"), ["2026-01-10 09.00.00"]);
    assert!(backup::remove(&link).is_err());
    backup::check(&kept.dir).unwrap();
    assert!(std::fs::symlink_metadata(&link).is_ok(), "the link is the person's too");
}
