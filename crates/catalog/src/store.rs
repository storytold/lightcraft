//! Byte storage for the catalog files (`catalog.snap`, `catalog.log`).
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
    /// Append and make durable (fsync): before returning, or, natively, by the store's own log
    /// thread right after ([`FsStore`]: a write that fails is returned by a later append, and a
    /// read through the store sees every byte handed over). On an error, part of `data` may have
    /// been written (a full disk, a dropped network share): the journal cuts the file back to its
    /// previous length with [`Store::truncate`] before appending again.
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// Cut a file to `len` bytes (drop a torn tail before appending again). A missing file is
    /// left missing.
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()>;
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
    /// The thread that writes the log (natively; started by the first append).
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
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
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
        // Makes the rename durable on POSIX; opening a directory fails on Windows (where
        // `rename` is already journaled by NTFS), so errors are ignored.
        if let Ok(d) = std::fs::File::open(&self.dir) {
            let _ = d.sync_all();
        }
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
        let bytes = match std::fs::read(self.path(name)) {
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
        let tmp = self.path(&format!("{name}.tmp"));
        let mut f = create_staging(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        check_staging(&self.folder, &tmp, &f)?;
        std::fs::rename(&tmp, self.path(name))?;
        self.sync_dir();
        self.reset_sync_log(name, data.len() as u64);
        Ok(())
    }

    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // (the log is small: buffered, and replaced by its thread after the appends before)
            let mut buf = Vec::new();
            fill(&mut buf)?;
            w.replace(&buf)?;
            self.sync_dir();
            return Ok(buf.len() as u64);
        }
        self.folder.check()?;
        let tmp = self.path(&format!("{name}.tmp"));
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
        let n = match written {
            Ok(n) => n,
            Err(e) => {
                if check_staging(&self.folder, &tmp, &file).is_ok() {
                    let _ = std::fs::remove_file(&tmp);
                }
                return Err(e);
            }
        };
        check_staging(&self.folder, &tmp, &file)?;
        std::fs::rename(&tmp, self.path(name))?;
        self.sync_dir();
        self.reset_sync_log(name, n);
        Ok(n)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.log.is_none() && self.sync_log.is_none() && !crate::fault::on_caller(&self.dir) {
                self.folder.check()?;
                check_regular_directory_entry(&self.path(name))?;
                if !self.path(name).exists() {
                    // created here, so the open's errors are this append's and its entry is durable
                    std::fs::OpenOptions::new().create(true).append(true).open(self.path(name))?;
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
        // A note-bearing copy can pass the folder check while an old handle still names the
        // moved-away folder. As the log thread does, reopen the current path for every batch.
        check_regular_directory_entry(&self.path(name))?;
        let existed = self.path(name).exists();
        // Windows append-only handles cannot truncate their own failed write. For LOG,
        // match the native writer's writable handle and seek to the checked length.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false);
        if name == crate::journal::LOG {
            options.write(true);
        } else {
            options.append(true);
        }
        let mut f = options.open(self.path(name))?;
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
        check_regular_directory_entry(&self.path(name))?;
        let f = match std::fs::OpenOptions::new().write(true).open(self.path(name)) {
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
