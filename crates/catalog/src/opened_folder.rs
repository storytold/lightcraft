//! The folder a library was opened from, and whether its path still leads to it (issue #3240).
//!
//! A library's files are written by path (`<library>/catalog.snap`, …), and a path can come to lead
//! somewhere else while the library is open: a share mounted over the folder, or the folder moved
//! away and another one put at its path. A session that went on writing then replaced the catalog
//! of the library on the share with its own.
//!
//! [`OpenedFolder`] remembers which folder the path named when the library was opened;
//! [`OpenedFolder::check`] asks, with one `stat`, whether it still does. Every write of a library
//! file asks first ([`crate::FsStore`], its log thread, [`crate::LibraryLock`]) and is refused,
//! writing nothing, when the answer is no. The refusal is an ordinary write error, so the changes
//! stay queued and the app says why they aren't saved. The next write asks again: with the folder
//! that was opened back at the path, saving goes on.
//!
//! - **Unix:** a folder is its device and inode number. A volume that is mounted again (a drive
//!   plugged in again, a share reconnected) gets another device number: its folder counts as the
//!   same when its inode number and its creation time agree (without one: the note below), so
//!   saving goes on there as it did before this check. A clone of the volume, with the same inode
//!   numbers and creation times, counts as that volume mounted again: nothing the check can see
//!   tells them apart. A folder moved away keeps its own inode number; one deleted and made again
//!   may be given the same inode number back, so on the same device two creation times that are
//!   both known must agree too.
//! - **The note of the lock:** some file systems number a folder anew each time they are mounted
//!   (FAT and exFAT on Linux, sshfs, cifs without server inode numbers) and some keep no creation
//!   time to go by (NFS). A folder that holds exactly the owner note this session's lock wrote
//!   ([`OpenedFolder::vouch`]) is the opened folder whatever its numbers say: the note holds a
//!   value found in no other lock's note, so no other library has it. A copy of this library's
//!   folder made while it was open does, and counts as the library (the log in it is checked when
//!   it is written to: [`crate::FsStore`]); a share mounted over the folder, with a library of its
//!   own or none, never does.
//! - **Windows:** `std` offers the volume serial number and the file index only on nightly
//!   (`windows_by_handle`, rust-lang/rust#63010), so there is no identity to compare yet. The check
//!   only asks that the path is still a folder, and a share mounted over a folder isn't noticed.
//! - A check is not a lock: a folder replaced between the `stat` and the write that follows it is
//!   still written to. Closing that window takes I/O relative to an open directory handle
//!   (`openat`, `renameat`), which the library doesn't use.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

/// Which folder a path named: the device and the inode number the system reports for it, and when
/// the folder was made where the file system keeps that.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
struct FolderId {
    dev: u64,
    ino: u64,
    born: Option<SystemTime>,
}

impl FolderId {
    #[cfg(unix)]
    fn of(folder: &std::fs::Metadata) -> Option<FolderId> {
        use std::os::unix::fs::MetadataExt;
        Some(FolderId { dev: folder.dev(), ino: folder.ino(), born: made_at(folder.created()) })
    }

    #[cfg(not(unix))]
    fn of(_: &std::fs::Metadata) -> Option<FolderId> {
        None
    }

    /// Whether `other` is the same folder: the same inode number, and then, where both creation
    /// times are known, the same creation time, else the same device number. A volume that is mounted
    /// again gets another device number, and saving went on there before there was a check; two
    /// creation times that differ are two folders, also on one device (an inode number is given
    /// again to a folder made later). Without both times another device is another folder: a
    /// refused save is better than a write into another volume's folder with that inode number.
    fn is(&self, other: &FolderId) -> bool {
        self.ino == other.ino
            && match (self.born, other.born) {
                (Some(was), Some(now)) => was == now,
                _ => self.dev == other.dev,
            }
    }
}

/// When a folder was made, where the file system says. macOS reports 0 (the Unix epoch) for a file
/// system that keeps no creation time (NFS, macFUSE): the same for every folder there, so no folder
/// is told from another by it.
#[cfg(unix)]
fn made_at(created: io::Result<SystemTime>) -> Option<SystemTime> {
    created.ok().filter(|t| *t != SystemTime::UNIX_EPOCH)
}

/// The folder a library was opened from (see the module docs). Clones share one record, so the
/// stores, the log's thread and the lock of a library all ask about the same folder.
#[derive(Clone, Debug)]
pub struct OpenedFolder {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    /// `None` where the system gives a folder no identity (Windows, for now).
    id: Option<FolderId>,
    /// The last check refused: a change is logged when it starts and when it ends, not at every write.
    refused: AtomicBool,
    /// The owner note this session's lock wrote into the folder (see [`OpenedFolder::vouch`]); set once.
    note: OnceLock<Vec<u8>>,
}

impl OpenedFolder {
    /// The folder `path` names now (a link is followed). Fails when it names no folder.
    pub fn at(path: &Path) -> io::Result<OpenedFolder> {
        let folder = std::fs::metadata(path)?;
        if !folder.is_dir() {
            return Err(io::Error::other(format!("{} is not a folder", path.display())));
        }
        Ok(OpenedFolder::new(path, FolderId::of(&folder)))
    }

    fn new(path: &Path, id: Option<FolderId>) -> OpenedFolder {
        OpenedFolder { inner: Arc::new(Inner { path: path.to_path_buf(), id, refused: AtomicBool::new(false), note: OnceLock::new() }) }
    }

    /// The path the folder was opened by.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// `other` was recorded for the same folder (always, where folders have no identity). A folder
    /// that holds the note this session's lock wrote is the same whatever its numbers say
    /// ([`OpenedFolder::vouch`]): the library opened again after a drive was mounted again keeps its lock.
    pub fn same_folder(&self, other: &OpenedFolder) -> bool {
        match (self.inner.id, other.inner.id) {
            (Some(a), Some(b)) => a.is(&b) || self.holds_note(other.path()),
            (a, b) => a.is_none() && b.is_none(),
        }
    }

    /// Record the owner note this session's lock wrote into the folder ([`crate::LibraryLock`]); the
    /// first one is kept. From then on a folder at the path that holds exactly these bytes is the
    /// opened folder even when its numbers say otherwise: some file systems number a folder anew at
    /// every mount (FAT and exFAT on Linux, sshfs), so a drive plugged in again would be refused for
    /// the rest of the session. The bytes are this lock's alone (the process, the program, the time
    /// it was taken and a value found in no other note): another library's folder doesn't have them
    /// (issue #3240), only a copy of this one made while it was open, which counts as it.
    pub fn vouch(&self, note: &[u8]) {
        let _ = self.inner.note.set(note.to_vec());
    }

    /// The note [`OpenedFolder::vouch`] recorded (`None`: none was, the numbers decide alone).
    pub fn note(&self) -> Option<&[u8]> {
        self.inner.note.get().map(Vec::as_slice)
    }

    /// `Ok` while the path leads to the folder that was opened (or to one that holds this session's
    /// note: [`OpenedFolder::vouch`]); else an error that says what is there now and what to do.
    pub fn check(&self) -> io::Result<()> {
        let inner = &*self.inner;
        let now = match std::fs::metadata(&inner.path) {
            Ok(now) => now,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(self.refuse("nothing is there now")),
            // The drive doesn't answer: the write would fail the same way, with less to go on.
            Err(e) => {
                let what = format!("can't tell whether {} is still the folder this library was opened from: {e}", inner.path.display());
                return Err(io::Error::new(e.kind(), what));
            }
        };
        if !now.is_dir() {
            return Err(self.refuse("it isn't a folder now"));
        }
        if let (Some(was), Some(is)) = (inner.id, FolderId::of(&now))
            && !was.is(&is)
            && !self.holds_note(&inner.path)
        {
            // (the same device number is the same volume's: another folder on it was put at the path)
            return Err(self.refuse(if was.dev == is.dev { "it was moved or replaced" } else { "a different drive or share is there now" }));
        }
        if inner.refused.swap(false, Ordering::Relaxed) {
            log::info!("library folder {}: the folder this library was opened from is back", inner.path.display());
        }
        Ok(())
    }

    /// The error for a path that doesn't lead to the opened folder, `what` being what is there.
    fn refuse(&self, what: &str) -> io::Error {
        let path = self.inner.path.display();
        let why = format!(
            "{path} is no longer the folder this library was opened from ({what}), so nothing is written to it: put the original folder back"
        );
        if !self.inner.refused.swap(true, Ordering::Relaxed) {
            log::warn!("library folder {why}");
        }
        io::Error::other(why)
    }

    /// Whether the folder at `dir` holds the note this session's lock wrote ([`OpenedFolder::vouch`]).
    fn holds_note(&self, dir: &Path) -> bool {
        self.note().is_some_and(|ours| holds_exactly(&dir.join(crate::lock::OWNER), ours))
    }
}

/// Whether `path` is a plain file that holds exactly `bytes`. A link, a folder or a file of another
/// size is no match and is not read (a pipe would block, a large file would be read for nothing).
pub(crate) fn holds_exactly(path: &Path, bytes: &[u8]) -> bool {
    let plain = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes.len() as u64);
    plain && std::fs::read(path).is_ok_and(|now| now == bytes)
}

#[cfg(all(test, unix))]
impl OpenedFolder {
    /// `path` as a volume that numbers its folders anew at every mount would have recorded it: other
    /// device and inode numbers than the folder has now, and no creation time (a mount can't be
    /// faked in a test).
    pub(crate) fn recorded_under_other_numbers(path: &Path) -> OpenedFolder {
        let now = std::fs::metadata(path).ok().and_then(|m| FolderId::of(&m));
        OpenedFolder::new(path, now.map(|n| FolderId { dev: n.dev.wrapping_add(1), ino: n.ino.wrapping_add(1), born: None }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-opened-folder-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn id(dev: u64, ino: u64, born: Option<u64>) -> FolderId {
        FolderId { dev, ino, born: born.map(|s| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s)) }
    }

    /// A folder is the same one when its inode number agrees and then its device number does or, where
    /// both creation times are known, its creation time: a volume that is mounted again has another
    /// device number, and an inode number is given again to a folder made later. Anything less sure
    /// is another folder.
    #[test]
    fn a_folder_on_a_volume_mounted_again_is_the_same_folder_only_when_it_was_made_at_the_same_time() {
        let was = id(7, 100, Some(1_000));
        assert!(was.is(&id(7, 100, Some(1_000))), "the same");
        assert!(!was.is(&id(7, 100, Some(2_000))), "the inode number of a folder that is gone, given to one made later");
        assert!(was.is(&id(9, 100, Some(1_000))), "mounted again under another device number");
        assert!(!was.is(&id(9, 100, Some(2_000))), "another volume's folder that has that inode number");
        assert!(!was.is(&id(9, 100, None)), "no creation time to tell by");
        assert!(!id(7, 100, None).is(&id(9, 100, None)), "none known at all");
        assert!(id(7, 100, None).is(&id(7, 100, None)), "the same device, and none to tell by");
        assert!(id(7, 100, None).is(&id(7, 100, Some(1_000))), "a creation time known for one folder only is no reason to refuse");
        assert!(!was.is(&id(7, 101, Some(1_000))), "another folder on the volume");
        assert!(!was.is(&id(9, 101, Some(1_000))), "another folder on another volume");
    }

    /// The same rule in a check, with the device number recorded at open made different from the
    /// one reported now (a mount can't be faked in a test).
    #[cfg(unix)]
    #[test]
    fn a_check_accepts_a_remounted_volume_and_refuses_another_volumes_folder() {
        let dir = scratch("remount");
        let real = FolderId::of(&std::fs::metadata(&dir).unwrap()).unwrap();
        let recorded = |id: FolderId| OpenedFolder::new(&dir, Some(id));

        let remounted = recorded(FolderId { dev: real.dev.wrapping_add(1), ..real });
        match real.born {
            Some(_) => remounted.check().expect("the same inode number and creation time: the same folder, mounted again"),
            None => assert!(remounted.check().is_err(), "no creation time to tell by: refused"),
        }
        let other = recorded(FolderId { dev: real.dev.wrapping_add(1), ino: real.ino.wrapping_add(1), ..real });
        let msg = other.check().expect_err("another volume's folder").to_string();
        assert!(msg.contains("a different drive or share is there now"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same rule in a check, with the creation time recorded at open made different from the one
    /// reported now (a folder made again under the inode number of one that was deleted can't be
    /// arranged in a test): the same device and inode number with another creation time is another
    /// folder, unless it holds the note this session's lock wrote.
    #[cfg(unix)]
    #[test]
    fn a_folder_made_again_under_the_same_inode_number_is_another_folder_unless_it_holds_the_note() {
        let dir = scratch("made-again");
        let real = FolderId::of(&std::fs::metadata(&dir).unwrap()).unwrap();
        let Some(born) = real.born else {
            OpenedFolder::new(&dir, Some(real)).check().expect("no creation time to tell by: the numbers decide");
            return;
        };
        let earlier = OpenedFolder::new(&dir, Some(FolderId { born: Some(born + std::time::Duration::from_secs(3600)), ..real }));
        let msg = earlier.check().expect_err("the same numbers, made at another time").to_string();
        assert!(msg.contains("it was moved or replaced"), "{msg}");
        assert!(!earlier.same_folder(&OpenedFolder::at(&dir).unwrap()), "not the folder that lock was taken in");

        let note = b"the note of the lock";
        earlier.vouch(note);
        std::fs::write(dir.join(crate::lock::OWNER), note).unwrap();
        earlier.check().expect("this session's note: the folder it locked, whatever its numbers say");
        assert!(earlier.same_folder(&OpenedFolder::at(&dir).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// macOS reports a creation time of 0 for a file system that keeps none (NFS, macFUSE), the same
    /// for every folder on it: that is no creation time, or any two such folders that have an inode
    /// number in common would be taken for one folder mounted again.
    #[cfg(unix)]
    #[test]
    fn a_creation_time_of_zero_is_no_creation_time() {
        let made = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(made_at(Ok(made)), Some(made));
        assert_eq!(made_at(Ok(SystemTime::UNIX_EPOCH)), None);
        assert_eq!(made_at(Err(io::Error::other("this file system keeps none"))), None);
    }

    /// A file system that numbers its folders anew at every mount (FAT and exFAT on Linux, sshfs)
    /// gives a folder that was opened other numbers: it is that folder when it holds the note this
    /// session's lock wrote, and no other folder is. (A mount can't be faked in a test: the numbers
    /// recorded at open are made different from the folder's.)
    #[cfg(unix)]
    #[test]
    fn a_folder_with_other_numbers_is_the_opened_one_only_while_it_holds_the_note_the_lock_wrote() {
        let dir = scratch("note");
        let note = br#"{"pid":4242,"host":"here","program":"LightCraft","version":"0.2.0","since":1}"#;
        let renumbered = OpenedFolder::recorded_under_other_numbers(&dir);
        let msg = renumbered.check().expect_err("other numbers, and no note vouched for").to_string();
        assert!(msg.contains("a different drive or share is there now"), "{msg}");

        renumbered.vouch(note);
        assert_eq!(renumbered.note(), Some(note.as_slice()));
        let path = dir.join(crate::lock::OWNER);
        assert!(renumbered.check().is_err(), "the note is vouched for, the folder holds none");
        let longer = [note.as_slice(), b" ".as_slice()].concat();
        for other in [note.to_ascii_uppercase(), longer, note[1..].to_vec(), Vec::new()] {
            std::fs::write(&path, &other).unwrap();
            assert!(renumbered.check().is_err(), "another note: {}", String::from_utf8_lossy(&other));
        }
        std::fs::write(&path, note).unwrap();
        renumbered.check().expect("this session's note: the folder it locked, whatever its numbers say");

        // what is no note: a link to one, a folder
        let elsewhere = dir.join("elsewhere");
        std::fs::rename(&path, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(renumbered.check().is_err(), "a link to the bytes is no note");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(renumbered.check().is_err(), "a folder is no note");

        // the first note is the one that counts
        renumbered.vouch(b"another");
        assert_eq!(renumbered.note(), Some(note.as_slice()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The lock is handed over to a library opened again from the same folder, and a folder that
    /// holds the note this session's lock wrote is that folder whatever its numbers say (a drive
    /// that was mounted again): not one with no such note.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_holds_the_locks_note_is_the_same_folder_for_the_hand_over() {
        let dir = scratch("hand-over");
        let other = scratch("hand-over-other");
        let note = b"the note of the lock";
        let at_open = OpenedFolder::recorded_under_other_numbers(&dir);
        let now = OpenedFolder::at(&dir).unwrap();
        assert!(!at_open.same_folder(&now), "other numbers, no note");

        at_open.vouch(note);
        assert!(!at_open.same_folder(&now), "the note is vouched for, the folder holds none");
        std::fs::write(dir.join(crate::lock::OWNER), note).unwrap();
        assert!(at_open.same_folder(&now));
        assert!(!at_open.same_folder(&OpenedFolder::at(&other).unwrap()), "another folder holds no such note");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }
}
