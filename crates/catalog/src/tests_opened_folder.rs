//! Issue #3240: a library's files are written to the folder it was opened from, not to whatever its
//! path leads to later (a share mounted over the folder, or the folder moved away and another put
//! at its path). A folder's identity is its device and inode number, so the tests that move a
//! folder run on Unix; the rest hold everywhere.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(unix)]
use crate::journal::{LOG, SNAPSHOT};
#[cfg(unix)]
use crate::lock::OWNER;
use crate::*;

/// The start of the sentence a refused write ends with.
const REFUSED: &str = "is no longer the folder this library was opened from";

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-opened-folder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The files in `dir` with their bytes, by name.
#[cfg(unix)]
fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// Another library's folder where `dir` was, and the folder that was there moved to the path
/// returned: what a share mounted over `dir` looks like (the path leads somewhere else, and the
/// folder that was opened is out of reach).
#[cfg(unix)]
fn put_another_folder_at(dir: &Path) -> PathBuf {
    let away = dir.with_extension("away");
    std::fs::rename(dir, &away).unwrap();
    std::fs::create_dir(dir).unwrap();
    std::fs::write(dir.join(SNAPSHOT), b"their snapshot").unwrap();
    std::fs::write(dir.join(LOG), b"their log\n").unwrap();
    std::fs::write(dir.join(OWNER), b"their note").unwrap();
    away
}

/// Every way a store writes is refused in a folder that replaced the opened one, whether or not
/// the log's own thread has started, and nothing is written in either folder.
#[cfg(unix)]
#[test]
fn a_store_writes_nothing_into_a_folder_that_replaced_the_one_it_opened() {
    for log_running in [false, true] {
        let base = scratch(&format!("store-{log_running}"));
        let dir = base.join("lib");
        let mut store = FsStore::open(&dir).unwrap();
        store.write_atomic(SNAPSHOT, b"ours").unwrap();
        if log_running {
            store.append(LOG, b"1\n").unwrap();
            store.flush().unwrap();
        }
        let away = put_another_folder_at(&dir);
        let (theirs, ours) = (files_in(&dir), files_in(&away));

        let results = [
            ("write_atomic", store.write_atomic(SNAPSHOT, b"x")),
            ("write_atomic_with", store.write_atomic_with(SNAPSHOT, &mut |w| w.write_all(b"x")).map(|_| ())),
            ("write_atomic of the log", store.write_atomic(LOG, b"")),
            ("write_atomic of a settings file", store.write_atomic("prefs.json", b"{}")),
            ("truncate", store.truncate(LOG, 0)),
        ];
        let appended = store.append(LOG, b"2\n");
        let flushed = store.flush();

        // what matters most: nothing was written, in either folder
        assert_eq!(files_in(&dir), theirs, "the folder at the path now is untouched (log thread running: {log_running})");
        assert_eq!(files_in(&away), ours, "and so is the folder that was opened: saving stops (log thread running: {log_running})");

        // and each write was refused, with the reason
        for (what, r) in results {
            let e = r.expect_err(what);
            assert!(e.to_string().contains(REFUSED), "{what}: {e}");
        }
        if log_running {
            // handed over to the log's thread, which refuses to write it: held, and said so
            appended.expect("the thread takes it");
            let e = flushed.expect_err("the thread can't write it");
            assert!(e.to_string().contains(REFUSED), "{e}");
            let (held, why) = store.unwritten().expect("held, not dropped");
            assert!(held == 1 && why.contains(REFUSED), "{held}: {why}");
        } else {
            let e = appended.expect_err("the first append");
            assert!(e.to_string().contains(REFUSED), "{e}");
        }

        if log_running {
            // the folder that was opened is back at the path: what was held is written, into it
            std::fs::rename(&dir, base.join("lib.theirs")).unwrap();
            std::fs::rename(&away, &dir).unwrap();
            store.flush().expect("saving resumes");
            assert_eq!(store.unwritten(), None);
            assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"1\n2\n");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// A compaction that starts after the folder was replaced writes through a second handle on the
/// same files: it is bound to the folder the store opened too.
#[cfg(unix)]
#[test]
fn the_snapshot_writer_of_a_store_is_bound_to_the_folder_the_store_opened() {
    let base = scratch("worker");
    let dir = base.join("lib");
    let store = FsStore::open(&dir).unwrap();
    let _away = put_another_folder_at(&dir);
    let theirs = files_in(&dir);
    let mut worker = store.background_writer().expect("a store on disk has one");
    let e = worker.write_atomic_with(SNAPSHOT, &mut |w| w.write_all(b"ours")).expect_err("refused");
    assert!(e.to_string().contains(REFUSED), "{e}");
    assert_eq!(files_in(&dir), theirs);
    let _ = std::fs::remove_dir_all(&base);
}

/// A journal's save (the closing snapshot) in a folder that replaced the opened one fails with the
/// reason and writes nothing; with the folder back, the same call saves.
#[cfg(unix)]
#[test]
fn a_journal_cannot_snapshot_into_a_folder_that_replaced_the_one_it_opened() {
    let base = scratch("journal");
    let dir = base.join("lib");
    let (mut journal, catalog, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    let away = put_another_folder_at(&dir);
    let theirs = files_in(&dir);
    let e = journal.snapshot(&catalog).expect_err("the path leads to another library's folder");
    assert!(e.to_string().contains(REFUSED), "{e}");
    assert_eq!(files_in(&dir), theirs);

    std::fs::rename(&dir, base.join("lib.theirs")).unwrap();
    std::fs::rename(&away, &dir).unwrap();
    journal.snapshot(&catalog).expect("the folder is back");
    let _ = std::fs::remove_dir_all(&base);
}

/// A file system that numbers its folders anew at every mount (FAT and exFAT on Linux, sshfs) gives
/// the folder other numbers: it is the opened folder again while it holds the note this session's
/// lock wrote, for every way a store writes, the log's thread included. (A mount can't be faked in
/// a test: the numbers the store recorded at open are made different from the folder's.)
#[cfg(unix)]
#[test]
fn a_store_writes_into_a_folder_with_other_numbers_while_the_folder_holds_this_sessions_note() {
    let dir = scratch("renumbered");
    let note = b"the note this session's lock wrote";
    let folder = OpenedFolder::recorded_under_other_numbers(&dir);
    folder.vouch(note);
    let mut store = FsStore::in_folder(folder);

    let e = store.write_atomic(SNAPSHOT, b"x").expect_err("other numbers, and no note in the folder");
    assert!(e.to_string().contains(REFUSED), "{e}");
    assert!(files_in(&dir).is_empty(), "nothing was written");

    std::fs::write(dir.join(OWNER), note).unwrap();
    store.write_atomic(SNAPSHOT, b"one").expect("the note vouches for the folder");
    store.write_atomic_with(SNAPSHOT, &mut |w| w.write_all(b"two")).expect("a snapshot written in pieces too");
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"two");
    store.append(LOG, b"1\n").expect("the first append starts the log's thread");
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"1\n");

    // another program's note over this session's: what the thread is handed is held, not written
    std::fs::write(dir.join(OWNER), b"theirs").unwrap();
    store.append(LOG, b"2\n").expect("the thread takes it");
    let e = store.flush().expect_err("the thread can't write it");
    assert!(e.to_string().contains(REFUSED), "{e}");
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"1\n");

    std::fs::write(dir.join(OWNER), note).unwrap();
    store.flush().expect("this session's note is back: what was held is written");
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"1\n2\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A copy of the library's folder (the note in it too) put at the path counts as the opened folder,
/// but its log is not the one the log's thread wrote: the thread opens the log by its path for each
/// batch and appends only while it is the length the thread left. A log that isn't is refused like a
/// failed write (held, reported): not written on through a file kept open in the folder that was
/// moved away, where the copy would never get it, and not appended to at its end, where it would
/// have a gap. The log reset of a snapshot replaces it and takes what was held.
#[cfg(unix)]
#[test]
fn a_log_that_is_not_the_one_the_thread_wrote_is_not_appended_to() {
    let base = scratch("log-copy");
    let dir = base.join("lib");
    let note = b"the note this session's lock wrote";
    let mut store = FsStore::open(&dir).unwrap();
    store.folder().vouch(note);
    std::fs::write(dir.join(OWNER), note).unwrap();
    store.append(LOG, b"1\n").unwrap();
    store.flush().unwrap();

    // a copy made now, which the folder that was opened outgrows, put in its place
    let copy = base.join("copy");
    std::fs::create_dir(&copy).unwrap();
    for (name, bytes) in files_in(&dir) {
        std::fs::write(copy.join(name), bytes).unwrap();
    }
    store.append(LOG, b"2\n").unwrap();
    store.flush().unwrap();
    let away = base.join("away");
    std::fs::rename(&dir, &away).unwrap();
    std::fs::rename(&copy, &dir).unwrap();
    let (theirs, ours) = (files_in(&dir), files_in(&away));

    store.append(LOG, b"3\n").unwrap();
    let e = store.flush().expect_err("the log at the path is not the one this session wrote");
    assert!(e.to_string().contains("holds 2 bytes, the log this session wrote has 4"), "{e}");
    assert_eq!(files_in(&dir), theirs, "nothing is appended to it");
    assert_eq!(files_in(&away), ours, "and nothing is written on through a file kept open in the folder that was moved away");
    assert_eq!(store.unwritten().map(|(n, _)| n), Some(1), "held, not dropped");
    store.append(LOG, b"4\n").expect("later records are taken: retrying can't heal this, only a replace of the log does");
    assert_eq!(files_in(&dir), theirs, "and not written either");

    store.write_atomic(LOG, b"tail\n3\n4\n").unwrap();
    assert_eq!(store.unwritten(), None, "the snapshot's log holds what was held");
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"tail\n3\n4\n", "the replacement preserves both held records");
    store.append(LOG, b"5\n").unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"tail\n3\n4\n5\n", "and appending goes on");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_folder_is_the_opened_one_while_its_path_leads_to_it() {
    let dir = scratch("same");
    let folder = OpenedFolder::at(&dir).unwrap();
    folder.check().unwrap();
    std::fs::write(dir.join("catalog.snap"), b"x").unwrap();
    std::fs::create_dir(dir.join("thumbs")).unwrap();
    folder.check().expect("what is written in it changes nothing");
    assert!(folder.same_folder(&folder.clone()));
    assert_eq!(folder.path(), dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_that_is_not_a_folder_cannot_be_opened() {
    let dir = scratch("not-a-folder");
    let file = dir.join("file");
    std::fs::write(&file, b"x").unwrap();
    assert!(OpenedFolder::at(&file).expect_err("a file").to_string().contains("is not a folder"));
    assert!(OpenedFolder::at(&dir.join("missing")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder that is gone is said to be gone, on every system.
#[test]
fn a_folder_that_is_gone_is_reported_as_gone() {
    let base = scratch("gone");
    let dir = base.join("lib");
    std::fs::create_dir(&dir).unwrap();
    let folder = OpenedFolder::at(&dir).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    let e = folder.check().expect_err("nothing is there");
    let msg = e.to_string();
    assert!(msg.contains(REFUSED) && msg.contains("nothing is there now") && msg.contains(&dir.display().to_string()), "{msg}");
    let _ = std::fs::remove_dir_all(&base);
}

/// The reason names the path and what to do; a folder moved and replaced is told from a gone one.
#[cfg(unix)]
#[test]
fn a_replaced_folder_is_refused_until_the_opened_one_is_back() {
    let base = scratch("replaced");
    let dir = base.join("lib");
    std::fs::create_dir(&dir).unwrap();
    let folder = OpenedFolder::at(&dir).unwrap();
    let away = put_another_folder_at(&dir);
    let msg = folder.check().expect_err("another folder is at the path").to_string();
    assert!(msg.contains(REFUSED) && msg.contains("moved or replaced") && msg.contains(&dir.display().to_string()), "{msg}");
    assert!(msg.contains("put the original folder back"), "{msg}");
    assert!(!folder.same_folder(&OpenedFolder::at(&dir).unwrap()), "the other folder is another folder");

    std::fs::rename(&dir, base.join("lib.theirs")).unwrap();
    std::fs::rename(&away, &dir).unwrap();
    folder.check().expect("the opened folder is back");
    let _ = std::fs::remove_dir_all(&base);
}

/// A library reached through a link is the folder the link leads to: a link that stays is no
/// reason to refuse, and one pointed at another folder is.
#[cfg(unix)]
#[test]
fn a_link_to_the_folder_is_followed() {
    let base = scratch("link");
    let (one, two, link) = (base.join("one"), base.join("two"), base.join("link"));
    std::fs::create_dir(&one).unwrap();
    std::fs::create_dir(&two).unwrap();
    std::os::unix::fs::symlink(&one, &link).unwrap();
    let folder = OpenedFolder::at(&link).unwrap();
    folder.check().expect("the link still leads to it");
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&two, &link).unwrap();
    assert!(folder.check().is_err(), "the link leads to another folder now");
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_snapshot_swap_during_fill_cannot_publish_another_librarys_staging_file() {
    let dir = scratch("stream-swap-publish");
    let mut store = FsStore::open(&dir).unwrap();
    let mut away = None;
    let result = store.write_atomic_with(SNAPSHOT, &mut |writer| {
        writer.write_all(b"our pending snapshot")?;
        away = Some(put_another_folder_at(&dir));
        std::fs::write(dir.join(format!("{SNAPSHOT}.tmp")), b"their pending snapshot")?;
        Ok(())
    });
    assert!(result.is_err(), "a folder swapped during a streamed save must be refused");
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"their snapshot");
    assert_eq!(std::fs::read(dir.join(format!("{SNAPSHOT}.tmp"))).unwrap(), b"their pending snapshot");
    let _ = std::fs::remove_dir_all(away.unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn a_snapshot_swap_during_fill_cannot_remove_another_librarys_staging_file() {
    let dir = scratch("stream-swap-cleanup");
    let mut store = FsStore::open(&dir).unwrap();
    let mut away = None;
    let result = store.write_atomic_with(SNAPSHOT, &mut |writer| {
        writer.write_all(b"our pending snapshot")?;
        away = Some(put_another_folder_at(&dir));
        std::fs::write(dir.join(format!("{SNAPSHOT}.tmp")), b"their pending snapshot")?;
        Err(std::io::Error::other("streaming save failed"))
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"their snapshot");
    assert_eq!(std::fs::read(dir.join(format!("{SNAPSHOT}.tmp"))).unwrap(), b"their pending snapshot");
    let _ = std::fs::remove_dir_all(away.unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failed_caller_append_rolls_back_its_opened_file_not_the_replacement() {
    let dir = scratch("caller-held-file-rollback");
    let log = crate::journal::LOG;
    crate::fault::write_on_caller(&dir, true);
    let mut store = FsStore::open(&dir).unwrap();
    store.append(log, b"first\n").unwrap();
    crate::fault::install(
        &dir,
        std::sync::Arc::new(|step| if step == crate::fault::Step::Sync { Err(std::io::Error::other("injected sync failure")) } else { Ok(()) }),
    );
    assert!(store.append(log, b"torn\n").is_err());
    crate::fault::remove(&dir);
    let held = dir.join("held.log");
    std::fs::rename(dir.join(log), &held).unwrap();
    let replacement = b"other\nextra\n";
    std::fs::write(dir.join(log), replacement).unwrap();
    assert!(store.truncate(log, 6).is_err(), "cleanup must refuse the replacement");
    assert_eq!(std::fs::read(&held).unwrap(), b"first\n", "the retained descriptor must allow rollback on every native platform");
    assert_eq!(std::fs::read(dir.join(log)).unwrap(), replacement);
    store.write_atomic(log, b"first\n").unwrap();
    store.append(log, b"next\n").unwrap();
    assert_eq!(store.read(log).unwrap().unwrap(), b"first\nnext\n");
    crate::fault::write_on_caller(&dir, false);
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn a_caller_append_reopens_the_file_after_a_note_bearing_copy_replaces_the_folder() {
    let base = scratch("caller-append-copy");
    let dir = base.join("lib");
    let note = b"the note this session's lock wrote";
    crate::fault::write_on_caller(&dir, true);
    let mut store = FsStore::open(&dir).unwrap();
    store.folder().vouch(note);
    std::fs::write(dir.join(OWNER), note).unwrap();
    store.append(LOG, b"1\n").unwrap();
    let copy = base.join("copy");
    std::fs::create_dir(&copy).unwrap();
    for (name, bytes) in files_in(&dir) {
        std::fs::write(copy.join(name), bytes).unwrap();
    }
    let away = base.join("away");
    std::fs::rename(&dir, &away).unwrap();
    std::fs::rename(&copy, &dir).unwrap();
    store.append(LOG, b"2\n").unwrap();
    crate::fault::write_on_caller(&dir, false);
    assert_eq!(store.read(LOG).unwrap().unwrap(), b"1\n2\n", "the accepted folder must receive the append");
    assert_eq!(std::fs::read(away.join(LOG)).unwrap(), b"1\n", "the moved-away folder must not receive it");
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn a_caller_append_refuses_a_changed_log_and_a_snapshot_preserves_every_op() {
    for longer in [false, true] {
        let base = scratch(&format!("caller-log-length-{longer}"));
        let dir = base.join("lib");
        crate::fault::write_on_caller(&dir, true);
        let store = FsStore::open(&dir).unwrap();
        let note = b"the note this session's lock wrote";
        store.folder().vouch(note);
        std::fs::write(dir.join(OWNER), note).unwrap();
        let (mut journal, mut catalog, _) = Journal::open(Box::new(store)).unwrap();
        let add = |n| Op::AddAlbum { album: Album::new(AlbumId(n), format!("album {n}")) };
        let first = add(1);
        catalog.apply(first.clone()).unwrap();
        journal.append(&[first]).unwrap();
        let copy = base.join("copy");
        std::fs::create_dir(&copy).unwrap();
        for (name, bytes) in files_in(&dir) {
            std::fs::write(copy.join(name), bytes).unwrap();
        }
        let second = add(2);
        catalog.apply(second.clone()).unwrap();
        journal.append(&[second]).unwrap();
        if longer {
            let mut bytes = std::fs::read(copy.join(LOG)).unwrap();
            bytes.extend_from_slice(&[b'x'; 4096]);
            std::fs::write(copy.join(LOG), bytes).unwrap();
        }
        let away = base.join("away");
        std::fs::rename(&dir, &away).unwrap();
        std::fs::rename(&copy, &dir).unwrap();
        let replacement = std::fs::read(dir.join(LOG)).unwrap();
        let acknowledged = std::fs::read(away.join(LOG)).unwrap();
        let third = add(3);
        catalog.apply(third.clone()).unwrap();
        let result = journal.append(&[third]);
        crate::fault::write_on_caller(&dir, false);
        assert!(result.is_err(), "a changed fallback log must not acknowledge a record after a gap");
        assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), replacement, "even the failed-append cleanup must leave the foreign log alone");
        assert_eq!(std::fs::read(away.join(LOG)).unwrap(), acknowledged);
        assert!(journal.wants_snapshot(), "the synchronous error requests a snapshot of the live catalog");
        journal.snapshot_with_unlogged(&catalog, 1).unwrap();
        let fourth = add(4);
        catalog.apply(fourth.clone()).unwrap();
        journal.append(&[fourth]).unwrap();
        journal.flush().unwrap();
        drop(journal);
        let (reopened, loaded, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
        assert_eq!(reopened.seq(), 4);
        for n in 1..=4 {
            assert_eq!(loaded.album(AlbumId(n)).unwrap().name, format!("album {n}"));
        }
        drop(reopened);
        let _ = std::fs::remove_dir_all(base);
    }
}

#[cfg(unix)]
#[test]
fn a_log_open_expectation_survives_a_copy_before_the_first_append() {
    for caller in [true, false] {
        let base = scratch(&format!("open-log-copy-{caller}"));
        let dir = base.join("lib");
        crate::fault::write_on_caller(&dir, caller);
        let note = b"the note this session's lock wrote";
        let open = || {
            let store = FsStore::open(&dir).unwrap();
            store.folder().vouch(note);
            std::fs::write(dir.join(OWNER), note).unwrap();
            Journal::open(Box::new(store)).unwrap()
        };
        let add = |n| Op::AddAlbum { album: Album::new(AlbumId(n), format!("album {n}")) };
        let (mut seed, mut catalog, _) = open();
        let first = add(1);
        catalog.apply(first.clone()).unwrap();
        seed.append(&[first]).unwrap();
        seed.flush().unwrap();
        let copy = base.join("copy");
        std::fs::create_dir(&copy).unwrap();
        for (name, bytes) in files_in(&dir) {
            std::fs::write(copy.join(name), bytes).unwrap();
        }
        let second = add(2);
        catalog.apply(second.clone()).unwrap();
        seed.append(&[second]).unwrap();
        seed.flush().unwrap();
        drop(seed);
        let (mut journal, mut catalog, _) = open();
        std::fs::rename(&dir, base.join("away")).unwrap();
        std::fs::rename(&copy, &dir).unwrap();
        let replacement = std::fs::read(dir.join(LOG)).unwrap();
        let third = add(3);
        catalog.apply(third.clone()).unwrap();
        let result = journal.append(&[third]);
        let unlogged = u64::from(result.is_err());
        let refused = result.is_err() || journal.flush().is_err();
        crate::fault::write_on_caller(&dir, false);
        assert!(refused, "the first append must use the log length read at open");
        assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), replacement);
        journal.snapshot_with_unlogged(&catalog, unlogged).unwrap();
        drop(journal);
        let (reopened, loaded, _) = open();
        assert_eq!(reopened.seq(), 3);
        for n in 1..=3 {
            assert!(loaded.album(AlbumId(n)).is_some(), "every acknowledged and unlogged op must reload");
        }
        drop(reopened);
        let _ = std::fs::remove_dir_all(base);
    }
}

#[cfg(unix)]
#[test]
fn a_log_sync_failure_can_only_roll_back_the_file_that_failed() {
    for caller in [true, false] {
        let base = scratch(&format!("failed-log-copy-{caller}"));
        let dir = base.join("lib");
        crate::fault::write_on_caller(&dir, caller);
        let mut store = FsStore::open(&dir).unwrap();
        let note = b"the note this session's lock wrote";
        store.folder().vouch(note);
        std::fs::write(dir.join(OWNER), note).unwrap();
        store.append(LOG, b"1\n").unwrap();
        store.flush().unwrap();
        let copy = base.join("copy");
        std::fs::create_dir(&copy).unwrap();
        let foreign = b"this is a longer log whose records must not be removed\n";
        std::fs::write(copy.join(LOG), foreign).unwrap();
        std::fs::write(copy.join(OWNER), note).unwrap();
        let dir_for_hook = dir.clone();
        let away = base.join("away");
        let away_for_hook = away.clone();
        let swapped = std::sync::atomic::AtomicBool::new(false);
        crate::fault::install(
            &dir,
            std::sync::Arc::new(move |step| {
                if step == crate::fault::Step::Sync {
                    if !swapped.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        std::fs::rename(&dir_for_hook, &away_for_hook)?;
                        std::fs::rename(&copy, &dir_for_hook)?;
                    }
                    return Err(std::io::Error::other("injected failed sync"));
                }
                Ok(())
            }),
        );
        let result = store.append(LOG, b"2\n");
        if caller {
            assert!(result.is_err());
            assert!(store.truncate(LOG, 2).is_err(), "cleanup must refuse the replacement log");
        } else {
            result.unwrap();
            assert!(store.flush().is_err());
            assert!(store.truncate(LOG, 2).is_err(), "cleanup must refuse the replacement log");
        }
        crate::fault::remove(&dir);
        crate::fault::write_on_caller(&dir, false);
        assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), foreign, "a torn-tail rollback must never touch the replacement");
        assert_eq!(std::fs::read(away.join(LOG)).unwrap(), b"1\n", "only our own failed append is rolled back");
        store.write_atomic(LOG, b"tail\n1\n2\n").unwrap();
        store.append(LOG, b"3\n").unwrap();
        store.flush().unwrap();
        assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"tail\n1\n2\n3\n");
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }
}

#[cfg(unix)]
#[test]
fn a_log_read_for_repair_does_not_permit_truncating_a_shorter_copy() {
    let base = scratch("read-repair-copy");
    let dir = base.join("lib");
    let mut store = FsStore::open(&dir).unwrap();
    let note = b"the note this session's lock wrote";
    store.folder().vouch(note);
    std::fs::write(dir.join(OWNER), note).unwrap();
    std::fs::write(dir.join(LOG), b"good\ntorn").unwrap();
    assert_eq!(store.read(LOG).unwrap().unwrap(), b"good\ntorn");
    let copy = base.join("copy");
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(copy.join(OWNER), note).unwrap();
    std::fs::write(copy.join(LOG), b"x\n").unwrap();
    let away = base.join("away");
    std::fs::rename(&dir, &away).unwrap();
    std::fs::rename(&copy, &dir).unwrap();
    assert!(store.truncate(LOG, 5).is_err(), "initial repair must use the length read, not extend a replacement");
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"x\n");
    assert_eq!(std::fs::read(away.join(LOG)).unwrap(), b"good\ntorn");
    store.write_atomic(LOG, b"whole\n").unwrap();
    store.append(LOG, b"next\n").unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"whole\nnext\n");
    drop(store);
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn a_replaced_staging_name_is_neither_published_nor_removed() {
    for fail_fill in [false, true] {
        let dir = scratch(&format!("staging-name-{fail_fill}"));
        let mut store = FsStore::open(&dir).unwrap();
        store.write_atomic(SNAPSHOT, b"committed snapshot").unwrap();
        let tmp = dir.join(format!("{SNAPSHOT}.tmp"));
        let result = store.write_atomic_with(SNAPSHOT, &mut |writer| {
            writer.write_all(b"our pending snapshot")?;
            std::fs::remove_file(&tmp)?;
            std::fs::write(&tmp, b"another save's staging file")?;
            if fail_fill { Err(std::io::Error::other("stream failed")) } else { Ok(()) }
        });
        assert!(result.is_err(), "a staging name no longer belonging to this save must be refused");
        assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"committed snapshot");
        assert_eq!(std::fs::read(&tmp).unwrap(), b"another save's staging file");
        let _ = std::fs::remove_dir_all(dir);
    }
}
