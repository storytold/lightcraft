//! Byte storage for the catalog files (`catalog.snap`, its segments in `segments/`, `catalog.log`).
//!
//! [`FsStore`] is the native implementation (a library directory); [`MemStore`] keeps files in
//! memory (tests, ephemeral sessions) and lets tests simulate crashes by editing the bytes. A web
//! host can implement [`Store`] over OPFS. [`FsStore`] writes only while its directory still leads
//! to the folder it opened ([`crate::OpenedFolder`], issue #3240).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::opened_folder::OpenedFolder;

/// A flat namespace of named files with the few operations the journal needs.
pub trait Store: Send {
    /// The whole file, or `None` if it doesn't exist.
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Replace a file atomically: after a crash either the old or the new content is visible,
    /// never a mix (native: write temp + fsync + rename + fsync directory).
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// [`Store::write_atomic`] with streamed content: `fill` writes the new content into the
    /// given writer (an error aborts and leaves the old file). Same durability: the file is
    /// replaced only once all of it is written (native: and fsynced). Returns its length.
    ///
    /// The default buffers the content in memory; [`FsStore`] streams it into the temp file, so
    /// a large snapshot is never held in memory as a whole.
    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        let mut buf = Vec::new();
        fill(&mut buf)?;
        self.write_atomic(name, &buf)?;
        Ok(buf.len() as u64)
    }
    /// Write a file that doesn't exist yet, streamed like [`Store::write_atomic_with`]: its
    /// content is durable when this returns, but its place in the folder need not be before
    /// [`Store::sync`]. For writing several new files that only count once something else names
    /// them (a snapshot's segments): one sync of the folder for all of them. A name that is
    /// taken is an [`io::ErrorKind::AlreadyExists`] error and that file keeps its content. The
    /// default can't promise that (its replacing write would overwrite): such a store refuses.
    fn write_new_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        let _ = (name, fill);
        Err(io::Error::new(io::ErrorKind::Unsupported, "this store can't create new files"))
    }
    /// Make the files written with [`Store::write_new_with`] durable in their folders (the
    /// library's, and its `segments/` folder).
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Append and make durable (fsync): before returning, or, natively, by the store's own log
    /// thread right after ([`FsStore`]: a write that fails is returned by a later append, and a
    /// read through the store sees every byte handed over). On an error, part of `data` may have
    /// been written (a full disk, a dropped network share): the journal cuts the file back to its
    /// previous length with [`Store::truncate`] before appending again.
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// Cut a file to `len` bytes (drop a torn tail before appending again). A missing file is
    /// left missing.
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()>;
    /// Remove a file (a missing one is not an error). The default can't: such a store keeps what
    /// it was asked to remove.
    fn remove(&mut self, name: &str) -> io::Result<()> {
        let _ = name;
        Err(io::Error::new(io::ErrorKind::Unsupported, "this store can't remove files"))
    }
    /// The names of the files it holds that it can be asked for by name (a file with a name no
    /// other call would accept isn't listed). The default knows none.
    fn list(&mut self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Whether the catalog's photos can be kept in segment files of their own (see
    /// [`crate::segments`]): the store can hold any number of files, list them and remove them.
    /// The default can't, and its catalog stays in one file.
    fn segmented(&self) -> bool {
        false
    }
    /// Human-readable location (diagnostics).
    fn describe(&self) -> String;
    /// Another handle on the same files that a worker thread can write `catalog.snap` through
    /// while this one keeps appending to the log (background compaction). `None` (the default):
    /// snapshots are written synchronously.
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        None
    }
    /// Wait until every append handed over is in the file and synced ([`FsStore`] writes them on a
    /// thread of its own; the default has nothing to wait for). An error says why some aren't: they
    /// are still held and retried, never dropped.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Records handed over by [`Store::append`] that can't be written yet (a write or sync on the
    /// store's own thread failed: they are held and retried), and why. `None`: nothing held back.
    fn unwritten(&self) -> Option<(usize, String)> {
        None
    }
}

/// Appends and truncates open the name's own file, so anything else at the name is the library's
/// problem: a link planted there redirects the write outside the directory (CWE-59), and a device
/// hangs the open. Refused with an error the open can show, before anything is written through.
pub(crate) fn check_regular_directory_entry(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.is_file() => Ok(()),
        Ok(md) => Err(io::Error::other(format!("{} is a {:?}, not a regular file", path.display(), md.file_type()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The synchronous fallback's acknowledged log length. Failed records stay with the caller;
/// `Journal::append` requests a snapshot on an error rather than acknowledging a sequence gap.
struct SynchronousLog {
    good: u64,
    torn: Option<std::fs::File>,
}

fn check_log_len(len: u64, good: u64) -> io::Result<()> {
    if len == good {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "catalog log holds {len} bytes, the log this session read or wrote has {good}: it is another file, so a snapshot must replace it"
    )))
}

/// Roll back only the descriptor whose append failed, never a new file at its old path.
pub(crate) fn rollback_torn(file: &mut Option<std::fs::File>, good: u64) -> io::Result<()> {
    if let Some(f) = file.as_ref() {
        if f.metadata()?.len() < good {
            return Err(io::Error::other("the failed log was shortened: a snapshot must replace it"));
        }
        f.set_len(good)?;
        f.sync_data()?;
        *file = None;
    }
    Ok(())
}

/// Files in a directory.
pub struct FsStore {
    dir: PathBuf,
    /// The folder `dir` named when the store was opened. Every write asks it first and is refused
    /// when `dir` leads somewhere else now (issue #3240); so does the log's thread.
    folder: OpenedFolder,
    /// Set only after using the synchronous fallback; do not switch to a fresh thread afterward
    /// and forget which records that fallback acknowledged.
    sync_log: Option<SynchronousLog>,
    /// Length of the log the journal read, retained through the first asynchronous or caller append.
    known_log_len: Option<u64>,
    /// The thread that writes the log (natively). An append to a file of the library's own folder starts it when none runs
    /// (a segment's appends are written without it, and so are all appends once the synchronous fallback has been used); `remove`
    /// of the log's file ends it, and the next append starts it again.
    #[cfg(not(target_arch = "wasm32"))]
    log: Option<crate::log_writer::LogWriter>,
}

impl FsStore {
    /// Use `dir` (created if missing). The store writes to the folder `dir` names now, and only
    /// while it still does.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<FsStore> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(FsStore::in_folder(OpenedFolder::at(dir.as_ref())?))
    }
    /// A store in a folder that was recorded already, so that the stores, the lock and the caches
    /// of one library ask about the same folder.
    pub fn in_folder(folder: OpenedFolder) -> FsStore {
        FsStore {
            dir: folder.path().to_path_buf(),
            folder,
            sync_log: None,
            known_log_len: None,
            #[cfg(not(target_arch = "wasm32"))]
            log: None,
        }
    }
    /// The folder the store opened.
    pub fn folder(&self) -> &OpenedFolder {
        &self.folder
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// Where the file `name` is. A name is a file of the library's folder, or of its segments
    /// folder (`segments/<file>`): anything else (an absolute name, `..`, deeper folders) would
    /// leave the library and is refused before the file system is touched. The log has one
    /// spelling only: the store knows it (its thread, its recorded length) by that name, while a
    /// case-insensitive folder would let another spelling reach the same file behind its back.
    fn path(&self, name: &str) -> io::Result<PathBuf> {
        let valid_part = |p: &str| !matches!(p, "" | "." | "..") && !p.contains(['\\', ':', '\0', '/']);
        let other_spelling_of_the_log = name != crate::journal::LOG && name.eq_ignore_ascii_case(crate::journal::LOG);
        let ok = match name.split_once('/') {
            None => valid_part(name) && name != crate::segments::SEGMENT_DIR && !other_spelling_of_the_log,
            Some((folder, file)) => folder == crate::segments::SEGMENT_DIR && valid_part(file),
        };
        if ok {
            Ok(self.dir.join(name))
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is not a name of the catalog's files")))
        }
    }
    /// The staging file of `name` (`<name>.tmp`, beside it). A name that itself ends in `.tmp` (in any case: a folder that
    /// ignores case makes `a.TMP` the file `a.tmp`) is a staging name, which a write clears (a leftover of a crashed save):
    /// no file is written under one, so a write never removes a file that was stored. Such a name can still be read,
    /// listed and removed, so that a leftover can be.
    fn staging_path(&self, name: &str) -> io::Result<PathBuf> {
        if name.as_bytes().last_chunk::<4>().is_some_and(|end| end.eq_ignore_ascii_case(b".tmp")) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is a staging name, no file is written under it")));
        }
        self.path(&format!("{name}.tmp"))
    }
    /// The folder a file is in: the library's, or the one its name states (`segments/…`).
    fn folder_of(&self, name: &str) -> io::Result<PathBuf> {
        Ok(self.path(name)?.parent().map_or_else(|| self.dir.clone(), Path::to_path_buf))
    }
    /// The folder of `name`, if it is the library's own and there: `Ok(None)` when the segments
    /// folder isn't there (yet), an error when it is a file or a link (to a folder elsewhere).
    /// Nothing is created, so reading, cutting and removing never make a folder.
    fn existing_folder_of(&self, name: &str) -> io::Result<Option<PathBuf>> {
        Self::existing_folder(self.folder_of(name)?, &self.dir)
    }
    /// `folder` if it is the library's folder or a real folder inside it; `Ok(None)` when it isn't
    /// there (yet), an error when it is a file or a link.
    fn existing_folder(folder: PathBuf, library: &Path) -> io::Result<Option<PathBuf>> {
        if folder == library {
            return Ok(Some(folder));
        }
        match std::fs::symlink_metadata(&folder) {
            Ok(m) if m.is_dir() => Ok(Some(folder)),
            Ok(_) => Err(io::Error::other(format!("{} is not a folder", folder.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", folder.display()))),
        }
    }
    /// Make the folder's entries durable. Where a folder can't be opened to be synced (Windows,
    /// where `rename` is journaled by NTFS anyway) nothing is asked of it; on POSIX a failure is
    /// the caller's to know.
    fn sync_folder_checked(folder: &Path) -> io::Result<()> {
        #[cfg(windows)]
        {
            let _ = folder;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            match std::fs::File::open(folder) {
                Ok(d) => d.sync_all().or_else(ignore_unsyncable),
                // (no such folder: nothing in it to make durable)
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => ignore_unsyncable(e),
            }
        }
    }
    /// [`Self::sync_folder_checked`] where a failure changes nothing for the caller (the file is
    /// written and renamed already; the entry's durability is best effort there).
    fn sync_folder(folder: &Path) {
        let _ = Self::sync_folder_checked(folder);
    }
    /// The log thread, when it writes `name`.
    #[cfg(not(target_arch = "wasm32"))]
    fn log_for(&self, name: &str) -> Option<&crate::log_writer::LogWriter> {
        self.log.as_ref().filter(|w| w.name == name)
    }

    /// Wait until the log thread has written and synced everything handed to it.
    pub fn flush_log(&self) -> io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = &self.log {
            return w.flush();
        }
        Ok(())
    }

    fn reset_sync_log(&mut self, name: &str, len: u64) {
        if name == crate::journal::LOG {
            self.known_log_len = Some(len);
            if self.sync_log.is_some() {
                self.sync_log = Some(SynchronousLog { good: len, torn: None });
            }
        }
    }

    fn sync_dir(&self) {
        Self::sync_folder(&self.dir);
    }
    /// The new content of `name`, written and fsynced into its staging file (`<name>.tmp`, in the
    /// folder of the file): the staging file's path and the content's length. The staging file
    /// is removed again if writing fails.
    fn stage(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<(PathBuf, u64)> {
        use std::io::Write;
        self.folder.check()?;
        let tmp = self.staging_path(name)?;
        // (a file in a folder of the library: the folder is made when its first file is)
        self.folder_for(name)?;
        let mut file = create_staging(&tmp)?;
        let written = (|| {
            let mut w = Counter { inner: io::BufWriter::with_capacity(1 << 20, &mut file), n: 0 };
            fill(&mut w)?;
            w.flush()?;
            let n = w.n;
            drop(w);
            file.sync_all()?;
            Ok(n)
        })();
        match written {
            Ok(n) => {
                check_staging(&self.folder, &tmp, &file)?;
                Ok((tmp, n))
            }
            Err(e) => {
                if check_staging(&self.folder, &tmp, &file).is_ok() {
                    let _ = std::fs::remove_file(&tmp);
                }
                Err(e)
            }
        }
    }
    /// Make sure the folder the file `name` goes into is there (the segments folder is made with
    /// its first file). It has to be a folder of the library's own: a file of that name, or a
    /// link to a folder elsewhere, is an error that says so.
    fn folder_for(&self, name: &str) -> io::Result<PathBuf> {
        let folder = self.folder_of(name)?;
        if folder == self.dir {
            return Ok(folder);
        }
        match std::fs::symlink_metadata(&folder) {
            Ok(m) if m.is_dir() => Ok(folder),
            Ok(_) => Err(io::Error::other(format!("{} is not a folder", folder.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&folder).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", folder.display())))?;
                // (the new folder's entry in the library folder must survive a crash too: once per library)
                Self::sync_folder(&self.dir);
                Ok(folder)
            }
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", folder.display()))),
        }
    }
}

/// A folder sync that failed because this file system can't sync folders (network and FUSE file
/// systems answer EINVAL, ENOTSUP or EBADF to fsync on a directory) is "can't be synced here",
/// as on Windows; anything else is a real I/O error.
#[cfg(not(windows))]
fn ignore_unsyncable(e: io::Error) -> io::Result<()> {
    if unsyncable(&e) { Ok(()) } else { Err(e) }
}

#[cfg(not(windows))]
fn unsyncable(e: &io::Error) -> bool {
    use io::ErrorKind::{InvalidInput, NotFound, Unsupported};
    // (EBADF 9 and EINVAL 22 everywhere; "not supported" is a platform's own number: on Linux 95,
    // on the BSDs and macOS 45, and on macOS also the kernel's EOPNOTSUPP 102, which is ENETRESET
    // on Linux and must fail there)
    let platform: &[i32] = if cfg!(any(target_os = "linux", target_os = "android")) {
        &[95]
    } else if cfg!(target_vendor = "apple") {
        &[45, 102]
    } else if cfg!(any(target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly")) {
        &[45]
    } else {
        &[]
    };
    matches!(e.kind(), InvalidInput | Unsupported | NotFound) || e.raw_os_error().is_some_and(|c| matches!(c, 9 | 22) || platform.contains(&c))
}

/// The file `tmp` becomes `dest` only if `dest` doesn't exist: a name that is taken is an
/// `AlreadyExists` error and its file is untouched (`rename` would replace it). A hard link is
/// the no-replace move `std` offers on every platform; on a file system without them (some
/// network shares, FAT) see [`rename_if_absent`]. `tmp` is removed afterwards, and if that fails
/// (a virus scanner holding it open) the next write of the name clears it.
fn commit_new(tmp: &Path, dest: &Path) -> io::Result<()> {
    let linked = match std::fs::hard_link(tmp, dest) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => rename_if_absent(tmp, dest),
        Ok(()) => Ok(()),
    };
    let _ = std::fs::remove_file(tmp);
    linked
}

/// [`commit_new`] where hard links aren't there: the name is looked up (a dangling link counts
/// as taken), then `tmp` renamed. A file that appears between the two is replaced: the one
/// window a file system without a no-replace move leaves.
fn rename_if_absent(tmp: &Path, dest: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dest) {
        Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", dest.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::rename(tmp, dest),
        Err(e) => Err(e),
    }
}

/// Whether a directory entry whose type was asked for is a file. An entry that is gone by the
/// time it is looked at isn't one; any other failure is the caller's (a listing that skipped
/// the entry would pass for complete).
fn is_listed_file(kind: io::Result<std::fs::FileType>) -> io::Result<bool> {
    match kind {
        Ok(t) => Ok(t.is_file()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// A new staging file at `tmp`: whatever is already there (a leftover from a crashed save, or a link) is removed first
/// (a link itself, never what it points to), then the file is created exclusively, so a write never follows a link
/// planted at that name into another file (issue #1502). Also writes the lock's owner note.
pub(crate) fn create_staging(tmp: &std::path::Path) -> io::Result<std::fs::File> {
    if std::fs::symlink_metadata(tmp).is_ok() {
        std::fs::remove_file(tmp)?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(tmp)
}

/// Recheck after a potentially long fill: neither the folder nor the staging name may now
/// belong to another save. Holding `file` also prevents Unix inode reuse while comparing it.
pub(crate) fn check_staging(folder: &OpenedFolder, path: &Path, file: &std::fs::File) -> io::Result<()> {
    folder.check()?;
    let named = std::fs::symlink_metadata(path)?;
    if !named.is_file() {
        return Err(io::Error::other("the snapshot staging file was replaced"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
            return Err(io::Error::other("the snapshot staging file was replaced"));
        }
    }
    #[cfg(not(unix))]
    let _ = file; // Folder/file identity on Windows remains outside this Unix mitigation.
    Ok(())
}

impl Store for FsStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // what was handed over, written or still held
            return w.read();
        }
        if self.existing_folder_of(name)?.is_none() {
            return Ok(None);
        }
        let bytes = match std::fs::read(self.path(name)?) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if name == crate::journal::LOG && self.known_log_len.is_none() {
            self.known_log_len = Some(bytes.as_ref().map_or(0, |b| b.len() as u64));
        }
        Ok(bytes)
    }

    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // (after the appends handed over before)
            w.replace(data)?;
            self.sync_dir();
            return Ok(());
        }
        self.folder.check()?;
        let tmp = self.staging_path(name)?;
        let folder = self.folder_for(name)?;
        let mut f = create_staging(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        check_staging(&self.folder, &tmp, &f)?;
        std::fs::rename(&tmp, self.path(name)?)?;
        // (renamed already: a folder that can't be synced is no reason to say nothing was written)
        Self::sync_folder(&folder);
        self.reset_sync_log(name, data.len() as u64);
        Ok(())
    }

    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // (the log is small: buffered, and replaced by its thread after the appends before)
            let mut buf = Vec::new();
            fill(&mut buf)?;
            w.replace(&buf)?;
            self.sync_dir();
            return Ok(buf.len() as u64);
        }
        let (tmp, n) = self.stage(name, fill)?;
        std::fs::rename(&tmp, self.path(name)?)?;
        // (renamed already, as on `main`: a folder that can't be synced is no reason to say nothing was written)
        Self::sync_folder(&self.folder_of(name)?);
        self.reset_sync_log(name, n);
        Ok(n)
    }

    fn sync(&mut self) -> io::Result<()> {
        // (as `sync_dir`: where a folder can't be opened, renames are journaled anyway). The
        // files written with `write_new_with` are the segments, in their folder; the library
        // folder too, for that folder itself when it is new.
        // A failure is the snapshot's: `catalog.snap` must not name segments whose entries may
        // not survive a crash.
        // (the segments folder, where it is: a link or a file there is an error, as for every other operation on it)
        if let Some(segments) = Self::existing_folder(self.dir.join(crate::segments::SEGMENT_DIR), &self.dir)? {
            Self::sync_folder_checked(&segments)?;
        }
        Self::sync_folder_checked(&self.dir)
    }

    fn write_new_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        let (tmp, n) = self.stage(name, fill)?;
        commit_new(&tmp, &self.path(name)?)?;
        self.reset_sync_log(name, n);
        Ok(n)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // (not a segment: that is appended to by the code below, which checks its folder on every append)
            if self.log.is_none()
                && self.sync_log.is_none()
                && !name.starts_with(crate::segments::SEGMENT_PREFIX)
                && !crate::fault::on_caller(&self.dir)
            {
                self.folder.check()?;
                let path = self.path(name)?;
                check_regular_directory_entry(&path)?;
                if !path.exists() {
                    // created here, so the open's errors are this append's and its entry is durable
                    std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
                    self.sync_dir();
                }
                let expected = if name == crate::journal::LOG { self.known_log_len } else { None };
                self.log = crate::log_writer::LogWriter::start(self.folder.clone(), name, expected);
            }
            // (another name than the log's is written here, as without a thread)
            if let Some(w) = self.log_for(name) {
                return w.append(data);
            }
        }
        self.folder.check()?;
        // (a file in a folder of the library's own: the segments folder is made by a write, not here)
        if self.existing_folder_of(name)?.is_none() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{name:?}: its folder is not there")));
        }
        // A note-bearing copy can pass the folder check while an old handle still names the
        // moved-away folder. As the log thread does, reopen the current path for every batch.
        let path = self.path(name)?;
        check_regular_directory_entry(&path)?;
        let existed = path.exists();
        // Windows append-only handles cannot truncate their own failed write. For LOG,
        // match the native writer's writable handle and seek to the checked length.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false);
        if name == crate::journal::LOG {
            options.write(true);
        } else {
            options.append(true);
        }
        let mut f = options.open(&path)?;
        if !existed {
            self.sync_dir();
        }
        if name == crate::journal::LOG {
            let len = f.metadata()?.len();
            let log = self.sync_log.get_or_insert(SynchronousLog { good: self.known_log_len.unwrap_or(len), torn: None });
            check_log_len(len, log.good)?;
            std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(log.good))?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let written = crate::fault::check(&self.dir, crate::fault::Step::Write)
            .and_then(|()| f.write_all(data))
            .and_then(|()| crate::fault::check(&self.dir, crate::fault::Step::Sync))
            .and_then(|()| f.sync_data());
        #[cfg(target_arch = "wasm32")]
        let written = f.write_all(data).and_then(|()| f.sync_data());
        if name == crate::journal::LOG
            && let Some(log) = self.sync_log.as_mut()
        {
            log.torn = written.is_err().then_some(f);
            if written.is_ok() {
                log.good = log.good.saturating_add(data.len() as u64);
            }
        }
        // A failed write can leave part of `data`; the caller cuts it back with `truncate`
        // before retrying. The file is reopened for that retry too.
        written
    }

    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            return w.truncate(len);
        }
        self.folder.check()?;
        if name == crate::journal::LOG
            && let Some(log) = self.sync_log.as_mut()
        {
            rollback_torn(&mut log.torn, log.good)?;
        }
        if self.existing_folder_of(name)?.is_none() {
            return Ok(());
        }
        let path = self.path(name)?;
        check_regular_directory_entry(&path)?;
        let f = match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f,
            // nothing to cut (an append that failed before creating the file)
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if name == crate::journal::LOG
            && let Some(good) = self.sync_log.as_ref().map(|log| log.good).or(self.known_log_len)
        {
            // Also covers initial repair before any append. Our own torn bytes, if any, were
            // rolled back through their held descriptor, never through this possibly replaced path.
            check_log_len(f.metadata()?.len(), good)?;
        }
        f.set_len(len)?;
        f.sync_all()?;
        self.reset_sync_log(name, len);
        Ok(())
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        // What was handed to the log's thread is written first (a failure is the caller's: nothing is dropped). The thread
        // goes only once the file is gone (it would read a missing file as another log), and the next append starts one.
        #[cfg(not(target_arch = "wasm32"))]
        let thread = self.log_for(name).is_some();
        #[cfg(not(target_arch = "wasm32"))]
        if thread {
            self.flush_log()?;
        }
        self.folder.check()?;
        // (a file in a folder of the library's own, as for writing and listing: never through a link)
        if self.existing_folder_of(name)?.is_some() {
            match std::fs::remove_file(self.path(name)?) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if thread {
            self.log = None;
        }
        // (a removed log is a log of no bytes: what the next append is checked against)
        self.reset_sync_log(name, 0);
        Ok(())
    }

    fn list(&mut self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if is_listed_file(entry.file_type())?
                && let Ok(name) = entry.file_name().into_string()
                && self.path(&name).is_ok()
            {
                names.push(name);
            }
        }
        // and the segments, by the names the catalog gives them (`segments/…`): in a folder of
        // the library's own (a file of that name, or a link to a folder elsewhere, is an error:
        // what is listed here may be removed)
        let folder = self.dir.join(crate::segments::SEGMENT_DIR);
        let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", folder.display()));
        match std::fs::symlink_metadata(&folder) {
            Ok(m) if !m.is_dir() => return Err(io::Error::other(format!("{} is not a folder", folder.display()))),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(names),
            Err(e) => return Err(named(e)),
        }
        match std::fs::read_dir(&folder) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if is_listed_file(entry.file_type())?
                        && let Ok(name) = entry.file_name().into_string()
                        && self.path(&format!("{}{name}", crate::segments::SEGMENT_PREFIX)).is_ok()
                    {
                        names.push(format!("{}{name}", crate::segments::SEGMENT_PREFIX));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(named(e)),
        }
        Ok(names)
    }

    fn segmented(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        self.dir.display().to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        Some(Box::new(FsStore { dir: self.dir.clone(), folder: self.folder.clone(), sync_log: None, known_log_len: None, log: None }))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_log()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn unwritten(&self) -> Option<(usize, String)> {
        self.log.as_ref().and_then(|w| w.unwritten())
    }
}

/// Counts the bytes written through it.
struct Counter<W> {
    inner: W,
    n: u64,
}

impl<W: io::Write> io::Write for Counter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let k = self.inner.write(buf)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// In-memory files. Clones share the same files (so a test can "reopen" or corrupt them).
#[derive(Clone, Default)]
pub struct MemStore {
    pub files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Offer a [`Store::background_writer`] (off by default: snapshots stay synchronous).
    pub background: bool,
    /// Keep the catalog in one file, like a store that can't hold segments (see
    /// [`Store::segmented`]; off by default).
    pub single_file: bool,
}

impl MemStore {
    pub fn new() -> MemStore {
        MemStore::default()
    }
    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }
    pub fn set(&self, name: &str, data: Vec<u8>) {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), data);
    }
}

impl Store for MemStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.get(name))
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.set(name, data.to_vec());
        Ok(())
    }
    fn write_new_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        let mut data = Vec::new();
        fill(&mut data)?;
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if files.contains_key(name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{name} exists")));
        }
        let n = data.len() as u64;
        files.insert(name.to_string(), data);
        Ok(n)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).entry(name.to_string()).or_default().extend_from_slice(data);
        Ok(())
    }
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        if let Some(f) = self.files.lock().unwrap_or_else(|e| e.into_inner()).get_mut(name) {
            f.truncate(len as usize);
        }
        Ok(())
    }
    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
        Ok(())
    }
    fn list(&mut self) -> io::Result<Vec<String>> {
        Ok(self.files.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect())
    }
    fn segmented(&self) -> bool {
        !self.single_file
    }
    fn describe(&self) -> String {
        "memory".into()
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        self.background.then(|| Box::new(self.clone()) as Box<dyn Store>)
    }
}

// (its one test plants a symlink, so the module is Unix-only: elsewhere `use super::*` would be unused)
#[cfg(all(test, unix))]
mod tests_staging {
    use super::*;

    /// A link planted at a staging name (`<name>.tmp`) is removed, never followed: the file it points to keeps its
    /// bytes and the save still lands (issue #1502).
    #[test]
    fn a_link_at_the_staging_name_is_never_written_through() {
        let dir = std::env::temp_dir().join(format!("lc-staging-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("catalog.snap.tmp")).unwrap();
        store.write_atomic("catalog.snap", b"{}").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(dir.join("catalog.snap")).unwrap(), b"{}");
        std::os::unix::fs::symlink(&victim, dir.join("catalog.log.tmp")).unwrap();
        store.write_atomic_with("catalog.log", &mut |w| w.write_all(b"[1]")).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), b"[1]");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The journal's append and truncate (which recovery uses) refuse a non-regular file at the
    /// name instead of following it, so a link planted at `catalog.log` never receives library
    /// bytes (CWE-59, Audit row 107); the snapshot write already removes such a link (#1502).
    #[test]
    fn append_and_truncate_refuse_a_link_at_the_name() {
        let dir = std::env::temp_dir().join(format!("lc-log-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let victim = dir.join("victim.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("catalog.log")).unwrap();
        let mut store = FsStore::open(&dir).unwrap();
        assert!(store.append("catalog.log", b"appended").is_err());
        assert!(store.truncate("catalog.log", 0).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(dir.join("catalog.log")).unwrap().file_type().is_symlink());
        // A regular file keeps working.
        std::fs::remove_file(dir.join("catalog.log")).unwrap();
        std::fs::write(dir.join("catalog.log"), b"abc").unwrap();
        store.truncate("catalog.log", 1).unwrap();
        assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), b"a");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, not(windows)))]
mod tests_sync_errors {
    use super::unsyncable;
    use std::io::{Error, ErrorKind};

    /// A file system that can't sync a folder (EINVAL, ENOTSUP, EBADF) does not fail a save; a
    /// real I/O error (EIO, ENOSPC) does.
    #[test]
    fn only_real_io_errors_fail_a_folder_sync() {
        for code in [9, 22] {
            assert!(unsyncable(&Error::from_raw_os_error(code)), "{code}");
        }
        #[cfg(target_os = "linux")]
        {
            assert!(unsyncable(&Error::from_raw_os_error(95)));
            assert!(!unsyncable(&Error::from_raw_os_error(102)), "ENETRESET on Linux");
        }
        #[cfg(target_vendor = "apple")]
        for code in [45, 102] {
            assert!(unsyncable(&Error::from_raw_os_error(code)), "{code}");
        }
        assert!(unsyncable(&Error::from(ErrorKind::Unsupported)) && unsyncable(&Error::from(ErrorKind::InvalidInput)));
        for code in [5, 28] {
            assert!(!unsyncable(&Error::from_raw_os_error(code)), "{code}");
        }
    }
}

#[cfg(test)]
mod tests_listing {
    #[cfg(unix)]
    use super::{FsStore, Store};
    use super::{is_listed_file, rename_if_absent};
    use std::io::{Error, ErrorKind};

    /// Where hard links aren't there, a taken name is still refused and left as it was, and a
    /// free one is taken by the rename.
    #[test]
    fn the_fallback_without_hard_links_refuses_a_taken_name() {
        let dir = std::env::temp_dir().join(format!("lc-no-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tmp, dest) = (dir.join("x.tmp"), dir.join("x"));
        std::fs::write(&tmp, b"new").unwrap();
        rename_if_absent(&tmp, &dest).unwrap();
        assert_eq!((std::fs::read(&dest).unwrap(), tmp.exists()), (b"new".to_vec(), false));
        std::fs::write(&tmp, b"newer").unwrap();
        assert_eq!(rename_if_absent(&tmp, &dest).unwrap_err().kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file whose name the store would refuse for every other call isn't listed, so a name
    /// that is listed can always be read and removed.
    #[cfg(unix)]
    #[test]
    fn a_listed_name_is_one_the_store_accepts() {
        let dir = std::env::temp_dir().join(format!("lc-listing-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        std::fs::write(dir.join("a:b"), b"").unwrap();
        std::fs::write(dir.join("catalog.snap"), b"").unwrap();
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        std::fs::write(dir.join("segments/1:1.json"), b"").unwrap();
        assert_eq!(store.list().unwrap(), ["catalog.snap"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory entry whose type can't be read fails the listing (it would pass for complete
    /// without it); one that vanished meanwhile is just not there.
    #[test]
    fn a_failed_file_type_is_an_error_not_a_missing_file() {
        let e = is_listed_file(Err(Error::from(ErrorKind::PermissionDenied))).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::PermissionDenied);
        assert!(!is_listed_file(Err(Error::from(ErrorKind::NotFound))).unwrap());
        let dir = std::env::temp_dir().join(format!("lc-listing-type-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("f"), b"").unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(is_listed_file(entry.file_type()).unwrap(), entry.file_name() == "f");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, unix))]
mod tests_segment_folder {
    use super::*;

    /// A library whose `segments` is a link to a folder elsewhere (with `seg.json` in it) or a plain file.
    fn library(tag: &str, link: bool) -> io::Result<(PathBuf, PathBuf, FsStore)> {
        let root = std::env::temp_dir().join(format!("lc-seg-folder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, outside) = (root.join("lib"), root.join("outside"));
        std::fs::create_dir_all(&outside)?;
        std::fs::write(outside.join("seg.json"), b"keep")?;
        let store = FsStore::open(&dir)?;
        if link {
            std::os::unix::fs::symlink(&outside, dir.join("segments"))?;
        } else {
            std::fs::write(dir.join("segments"), b"not a folder")?;
        }
        Ok((root, outside, store))
    }

    /// `read` never returns the bytes of a file behind a `segments` link (or a file named `segments`).
    #[test]
    fn read_refuses_a_segments_folder_that_is_not_a_real_folder() {
        for link in [true, false] {
            let (root, _, mut store) = library(&format!("read{link}"), link).unwrap();
            assert!(store.read("segments/seg.json").is_err(), "link: {link}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// `append` and `truncate` never change a file behind a `segments` link (or a file named `segments`).
    #[test]
    fn append_and_truncate_refuse_a_segments_folder_that_is_not_a_real_folder() {
        for link in [true, false] {
            let (root, outside, mut store) = library(&format!("cut{link}"), link).unwrap();
            assert!(store.append("segments/seg.json", b"more").is_err(), "append, link: {link}");
            assert!(store.truncate("segments/seg.json", 0).is_err(), "truncate, link: {link}");
            assert_eq!(std::fs::read(outside.join("seg.json")).unwrap(), b"keep");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// A library with a real `segments` folder, or none yet, works as before: reads and cuts
    /// of a missing file are "nothing", and no folder is made by them.
    #[test]
    fn a_real_or_missing_segments_folder_is_unchanged() {
        let root = std::env::temp_dir().join(format!("lc-seg-folder-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = FsStore::open(&root).unwrap();
        assert_eq!(store.read("segments/1.json").unwrap(), None);
        store.truncate("segments/1.json", 0).unwrap();
        store.remove("segments/1.json").unwrap();
        assert!(!root.join("segments").exists());
        std::fs::create_dir_all(root.join("segments")).unwrap();
        std::fs::write(root.join("segments/1.json"), b"abc").unwrap();
        assert_eq!(store.read("segments/1.json").unwrap(), Some(b"abc".to_vec()));
        store.append("segments/1.json", b"d").unwrap();
        // (a segment is appended to through the handle that checks its folder; the log's thread is not started for it)
        #[cfg(not(target_arch = "wasm32"))]
        assert!(store.log.is_none());
        store.truncate("segments/1.json", 2).unwrap();
        assert_eq!(std::fs::read(root.join("segments/1.json")).unwrap(), b"ab");
        assert_eq!(store.read("segments/2.json").unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests_names_and_sync {
    use super::*;

    fn library(tag: &str) -> (PathBuf, FsStore) {
        let dir = std::env::temp_dir().join(format!("lc-names-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStore::open(&dir).unwrap();
        (dir, store)
    }

    /// A name that ends in `.tmp` is a staging name, which a write clears: no write is made under one, so no stored
    /// file is lost to a staging file, while a leftover can still be read, listed and removed.
    #[test]
    fn no_file_is_written_under_a_staging_name() {
        let (dir, mut store) = library("staging");
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        std::fs::write(dir.join("segments/a.json.tmp"), b"old").unwrap();
        let fill = &mut |w: &mut dyn io::Write| w.write_all(b"new");
        for name in ["segments/a.json.tmp", "x.tmp", "x.TMP"] {
            assert_eq!(store.write_new_with(name, fill).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.write_atomic_with(name, fill).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.write_atomic(name, b"new").unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert_eq!(std::fs::read(dir.join("segments/a.json.tmp")).unwrap(), b"old", "the leftover was not touched");
        assert!(!dir.join("x.tmp").exists() && !dir.join("x.tmp.tmp").exists());
        assert_eq!(store.read("segments/a.json.tmp").unwrap(), Some(b"old".to_vec()));
        assert!(store.list().unwrap().contains(&"segments/a.json.tmp".to_string()));
        store.remove("segments/a.json.tmp").unwrap();
        assert!(!dir.join("segments/a.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The log is known by one spelling (its thread and recorded length are kept under it), and a folder that ignores case
    /// would let another one reach the same file: no call accepts one.
    #[test]
    fn the_log_has_one_spelling() {
        let (dir, mut store) = library("log-name");
        std::fs::write(dir.join("catalog.log"), b"abc").unwrap();
        for name in ["CATALOG.LOG", "Catalog.log"] {
            assert_eq!(store.remove(name).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.read(name).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.truncate(name, 1).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.append(name, b"d").unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
            assert_eq!(store.write_atomic(name, b"d").unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), b"abc");
        assert_eq!(store.read("catalog.log").unwrap(), Some(b"abc".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A log made by `write_new_with` is the length the store recorded for it: after a read that found no log, cutting it
    /// back is not refused as the replacement of a log of another length.
    #[test]
    fn a_log_created_by_write_new_with_can_be_cut() {
        let (dir, mut store) = library("new-log");
        assert_eq!(store.read("catalog.log").unwrap(), None);
        store.write_new_with("catalog.log", &mut |w| w.write_all(b"abc")).unwrap();
        store.truncate("catalog.log", 2).unwrap();
        assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), b"ab");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `sync` is for the folder that holds the new segments: a link or a file at `segments` is an error (the entries of the
    /// folder it stands for are not made durable), a missing folder is nothing to sync, a real one is synced.
    #[cfg(unix)]
    #[test]
    fn sync_refuses_a_segments_folder_that_is_not_a_real_folder() {
        let (dir, mut store) = library("sync");
        store.sync().unwrap();
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        store.sync().unwrap();
        std::fs::remove_dir(dir.join("segments")).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("segments")).unwrap();
        assert!(store.sync().unwrap_err().to_string().contains("not a folder"));
        std::fs::remove_file(dir.join("segments")).unwrap();
        std::fs::write(dir.join("segments"), b"not a folder").unwrap();
        assert!(store.sync().unwrap_err().to_string().contains("not a folder"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
