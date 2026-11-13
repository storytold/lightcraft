//! One process per library: an exclusive OS lock on `<library>/catalog.lock`, held for as long as
//! the library is open (issue #99).
//!
//! Two processes on the same library (the app and `lightcraft-cli --library`, two app instances,
//! two computers on a shared folder) would each keep their own catalog and `seq` counter, and
//! whichever wrote the last snapshot would silently drop the other's edits. So opening a library
//! takes the lock first, and a second opener gets [`LockError::InUse`] instead.
//!
//! - The lock is the OS's (`flock` on Unix, `LockFileEx` on Windows, via [`std::fs::File::try_lock`]):
//!   it is released explicitly when the [`LibraryLock`] is dropped and, after a crash or a kill, by
//!   the OS when the process ends. A `catalog.lock` file left behind is therefore never "stale":
//!   only a lock that's actually held refuses the open.
//! - Dropping the lock calls [`std::fs::File::unlock`] rather than relying on the handle closing.
//!   On Unix an `flock` lock belongs to the open file description and lasts while *any* descriptor
//!   on it is open; a process that another thread is spawning holds a copy of every descriptor
//!   (`O_CLOEXEC` only closes them at `execve`), so closing ours would leave the library "already
//!   open" for a moment and a re-open would be refused (issue #680). `LOCK_UN` through our
//!   descriptor frees the lock whatever copies a child still holds.
//! - Who holds it (process id, computer, program, version) is written to `catalog.lock.owner`, for
//!   the error message. Only dropping the [`LibraryLock`] removes it, so a program that ends with
//!   `std::process::exit` releases the lock first (`Session::release_library_lock`). Dropping
//!   removes only the note this lock wrote itself, with the bytes it wrote, and only while the
//!   path still leads to the folder it locked (a folder that holds exactly that note is it, whatever
//!   its numbers say: [`OpenedFolder::vouch`]): a note in another folder (a share mounted over the
//!   library's folder), or one another process wrote over ours, belongs to whoever has that library
//!   open (issue #3240). It's a separate file because a Windows lock also blocks reading the
//!   locked file. It's only a hint: it may be missing or left over from a crash. The note holds a
//!   value no other lock's note has ([`LockOwner::token`]), so no two folders hold the same note.
//! - File systems without lock support (some network shares): the library opens unlocked, with a
//!   warning — refusing it would lock the user out of their own library. Locks over SMB/NFS are
//!   advisory and depend on the server; the owner file is then the only hint.
//! - Browser storage (OPFS) has no file locks: see the web host for its own guard.

use std::fs::{File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::opened_folder::OpenedFolder;

/// The lock file in the library directory.
pub const LOCK: &str = "catalog.lock";
/// Who holds the lock (JSON, see [`LockOwner`]).
pub const OWNER: &str = "catalog.lock.owner";

/// Who has the library open (as written in [`OWNER`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LockOwner {
    pub pid: u32,
    /// Computer name, if known.
    pub host: String,
    /// The program (`LightCraft`, `lightcraft-cli`…) and its version.
    pub program: String,
    pub version: String,
    /// When it opened the library (seconds since 1970).
    pub since: u64,
    /// This lock's alone, also among those one process takes in the same second: the note is what
    /// tells the folder that was locked from every other ([`OpenedFolder::vouch`], issue #3240), so
    /// no two may be the same. A count of locks mixed with a random key. Not a secret.
    pub token: String,
}

impl LockOwner {
    /// This process, as `program`.
    pub fn this_process(program: &str) -> LockOwner {
        LockOwner {
            pid: std::process::id(),
            host: host_name(),
            program: program.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            since: web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            token: lock_token(),
        }
    }

    /// "LightCraft 0.3.0 (process 123 on studio-mac)".
    pub fn describe(&self) -> String {
        let program = if self.program.is_empty() { "another LightCraft" } else { self.program.as_str() };
        let version = if self.version.is_empty() { String::new() } else { format!(" {}", self.version) };
        let host = if self.host.is_empty() {
            String::new()
        } else if self.host == host_name() {
            " on this computer".to_string()
        } else {
            format!(" on {}", self.host)
        };
        format!("{program}{version} (process {}{host})", self.pid)
    }
}

/// The computer's name, best effort (empty if unknown).
fn host_name() -> String {
    let env = ["COMPUTERNAME", "HOSTNAME"].iter().find_map(|k| std::env::var(k).ok().filter(|h| !h.trim().is_empty()));
    env.or_else(|| std::fs::read_to_string("/etc/hostname").ok()).map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_default()
}

/// A value no other lock's note has ([`LockOwner::token`]).
fn lock_token() -> String {
    static LOCKS: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(LOCKS.fetch_add(1, Ordering::Relaxed));
    format!("{:016x}", h.finish())
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process has the library open.
    #[error("This library is already open in {}. Close it there first.", .0.as_ref().map(LockOwner::describe).unwrap_or_else(|| "another LightCraft program".into()))]
    InUse(Option<LockOwner>),
}

/// The exclusive lock on a library directory; released on drop (and by the OS if the process
/// dies).
#[derive(Debug)]
pub struct LibraryLock {
    dir: PathBuf,
    /// `None` when the file system can't lock (the library is open unlocked).
    file: Option<File>,
    held: bool,
    /// The folder `dir` named when the lock was taken: the lock file and the owner note are in it
    /// (`None`: it couldn't be looked at).
    folder: Option<OpenedFolder>,
    /// The owner note as this lock wrote it (`None`: it wrote none).
    note: Option<Vec<u8>>,
}

impl LibraryLock {
    /// Lock `dir` (which must exist) for this process. Fails only when another process holds it.
    pub fn acquire(dir: &Path, program: &str) -> Result<LibraryLock, LockError> {
        let folder = OpenedFolder::at(dir).ok();
        let unlocked = |why: io::Error| {
            log::warn!("library {}: can't lock {LOCK} ({why}); opening it without protection against a second program", dir.display());
            LibraryLock { dir: dir.to_path_buf(), file: None, held: false, folder: folder.clone(), note: None }
        };
        let file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK)) {
            Ok(f) => f,
            Err(e) => return Ok(unlocked(e)),
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let owner = std::fs::read(dir.join(OWNER)).ok().and_then(|b| serde_json::from_slice::<LockOwner>(&b).ok());
                return Err(LockError::InUse(owner));
            }
            Err(TryLockError::Error(e)) => return Ok(unlocked(e)),
        }
        let note = serde_json::to_vec_pretty(&LockOwner::this_process(program)).ok().and_then(|json| match write_note(dir, folder.as_ref(), &json) {
            Ok(()) => {
                // from now on the folder that holds this note is the locked one, whatever its numbers say
                if let Some(folder) = &folder {
                    folder.vouch(&json);
                }
                Some(json)
            }
            Err(e) => {
                log::warn!("library {}: can't write {OWNER}: {e}", dir.display());
                None
            }
        });
        Ok(LibraryLock { dir: dir.to_path_buf(), file: Some(file), held: true, folder, note })
    }

    /// The OS lock is held (`false`: the file system can't lock; opened unprotected).
    pub fn held(&self) -> bool {
        self.held
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The folder the lock was taken in, as `dir` named it then (`None`: it couldn't be looked at).
    pub fn folder(&self) -> Option<&OpenedFolder> {
        self.folder.as_ref()
    }

    /// The owner note this lock wrote (`None`: it wrote none, so no folder is told by it).
    pub fn note(&self) -> Option<&[u8]> {
        self.note.as_deref()
    }

    /// Another descriptor on the locked file: what a process spawned by another thread holds
    /// between fork and exec (a `dup` refers to the same open file description). Tests only (the
    /// test that uses it is Unix-only: `flock` semantics).
    #[cfg(all(test, unix))]
    pub(crate) fn descriptor_copy(&self) -> Option<File> {
        self.file.as_ref().and_then(|f| f.try_clone().ok())
    }

    /// Remove the owner note if it is this lock's: the path still leads to the folder the lock was
    /// taken in (or to one that holds this very note), and the note has the bytes this lock wrote
    /// there (issue #3240).
    fn remove_own_note(&self) {
        let Some(ours) = &self.note else { return };
        if self.folder.as_ref().is_some_and(|f| f.check().is_err()) {
            return;
        }
        let path = self.dir.join(OWNER);
        if crate::opened_folder::holds_exactly(&path, ours) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Write the owner note into `dir`: a new file in place of whatever is at the name (a link planted
/// there in a library folder someone else prepared is removed, never written through, as for the
/// staging files, #1502), and only while `dir` still leads to the folder the lock file was opened in.
fn write_note(dir: &Path, folder: Option<&OpenedFolder>, json: &[u8]) -> io::Result<()> {
    if let Some(folder) = folder {
        folder.check()?;
    }
    crate::store::create_staging(&dir.join(OWNER))?.write_all(json)
}

impl Drop for LibraryLock {
    /// Remove the owner note, if it is ours, while still holding the lock (so it can't delete the
    /// next holder's), then release the lock explicitly. Closing the handle isn't enough on Unix:
    /// `flock` lasts while any copy of the descriptor is open, and a process another thread is
    /// spawning holds a copy of every descriptor until it execs (issue #680). If the unlock fails
    /// the handle still closes right after, which releases the lock once every copy is gone, as
    /// before.
    fn drop(&mut self) {
        if self.held {
            self.remove_own_note();
            if let Some(f) = &self.file
                && let Err(e) = f.unlock()
            {
                log::warn!("library {}: can't release {LOCK}: {e}", self.dir.display());
            }
        }
    }
}
