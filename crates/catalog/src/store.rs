//! Byte storage for the catalog files (`catalog.snap`, `catalog.log`).
//!
//! [`FsStore`] is the native implementation (a library directory); [`MemStore`] keeps files in
//! memory (tests, ephemeral sessions) and lets tests simulate crashes by editing the bytes. A web
//! host can implement [`Store`] over OPFS.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::safe_file::sync_file;

/// A flat namespace of named files with the few operations the journal needs.
pub trait Store: Send {
    /// The whole file, or `None` if it doesn't exist.
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Replace a file atomically: after a crash either the old or the new content is visible,
    /// never a mix (native: write temp + fsync + rename + fsync directory).
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// Create a file that doesn't exist yet: `AlreadyExists` when `name` is taken, and a file
    /// that is there is never replaced, even when another writer creates it at the same moment
    /// (that is the difference from reading first and then calling [`Store::write_atomic`]). Same
    /// durability as [`Store::write_atomic`]: the file appears whole or not at all.
    ///
    /// The default checks and then writes, which is only as safe as the store is unshared: a store
    /// that other writers can reach ([`FsStore`], [`MemStore`]) overrides it with one atomic step.
    fn write_new(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        if self.read(name)?.is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{name} already exists")));
        }
        self.write_atomic(name, data)
    }
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
    /// Append and make durable (fsync): before returning, or, where [`Store::log_progress`] says
    /// `Some` (natively, [`FsStore`]'s own log thread), right after; it is acknowledged once
    /// [`LogProgress::durable_len`] covers it. On an error, part of `data` may have
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
    /// How much of `name` is durable, when appends to it are written by a thread of the store's
    /// own (`None`, the default: every append is durable when it returns).
    fn log_progress(&self, _name: &str) -> Option<LogProgress> {
        None
    }
    /// Wait until every append to `name` handed over is written and synced, or has failed (then
    /// [`Store::log_progress`] says why). The default has nothing to wait for.
    fn flush(&mut self, _name: &str) -> io::Result<()> {
        Ok(())
    }
    /// The directory the files are in, when they are plain files there: such a library uses the
    /// v4 store ([`crate::db`]) beside its log. `None` (the default): JSON snapshots.
    fn dir(&self) -> Option<&Path> {
        None
    }
    /// Is there a folder called `name` among the files? The journal refuses a library laid out
    /// with a folder this version doesn't read (upstream #3962) rather than load part of it and
    /// overwrite the rest. The default: a flat store (no folders), so `false`.
    fn has_folder(&mut self, name: &str) -> io::Result<bool> {
        let _ = name;
        Ok(false)
    }
}

/// What a store's log thread has made durable ([`Store::log_progress`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogProgress {
    /// Bytes of the file written whole and synced, from its start.
    pub durable_len: u64,
    /// Why a write or sync failed (appends after it are left out, so the file never holds a gap),
    /// until the file is cut back or replaced.
    pub failed: Option<String>,
}

/// Files in a directory.
pub struct FsStore {
    dir: PathBuf,
    /// Open append handle (kept between appends; dropped on rewrite/truncate). Used where no
    /// thread writes the log (the browser build, or one that couldn't start).
    appender: Option<(String, std::fs::File)>,
    /// The thread that writes the log (natively; started by the first append).
    #[cfg(not(target_arch = "wasm32"))]
    log: Option<crate::log_writer::LogWriter>,
    /// Keep the JSON snapshot (format 3 storage) instead of the v4 store.
    json_only: bool,
}

impl FsStore {
    /// Use `dir` (created if missing).
    pub fn open(dir: impl AsRef<Path>) -> io::Result<FsStore> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(FsStore {
            dir: dir.as_ref().to_path_buf(),
            appender: None,
            #[cfg(not(target_arch = "wasm32"))]
            log: None,
            json_only: false,
        })
    }
    /// [`FsStore::open`], keeping the JSON snapshot storage ([`Store::dir`] says `None`): for
    /// tools and tests of that format, and the benchmark's baseline.
    pub fn open_json(dir: impl AsRef<Path>) -> io::Result<FsStore> {
        Ok(FsStore { json_only: true, ..FsStore::open(dir)? })
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
    /// Stop the log thread when it writes `name` (it writes and syncs what it was handed first).
    fn stop_log(&mut self, name: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.log_for(name).is_some() {
            self.log = None;
        }
        let _ = name;
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
/// planted at that name into another file. Also writes the lock's owner note.
pub(crate) fn create_staging(tmp: &std::path::Path) -> io::Result<std::fs::File> {
    if std::fs::symlink_metadata(tmp).is_ok() {
        std::fs::remove_file(tmp)?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(tmp)
}

/// Most bytes of one catalog file read into memory.
const MAX_READ: u64 = 8 << 30;

impl Store for FsStore {
    /// Never through a link at the name, nor from a FIFO or a device (issue #2552): a `prefs.json`
    /// linked to a private file would be read as damaged and copied into the library.
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        use std::io::Read;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // every byte handed over, in order
            w.flush()?;
        }
        let Some(mut f) = crate::safe_file::open_regular(&self.path(name), std::fs::OpenOptions::new().read(true))? else { return Ok(None) };
        let mut b = Vec::new();
        (&mut f).take(MAX_READ.saturating_add(1)).read_to_end(&mut b)?;
        if b.len() as u64 > MAX_READ {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{name} is more than the {MAX_READ} bytes allowed")));
        }
        Ok(Some(b))
    }

    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            // (after the appends handed over before)
            return w.replace(data);
        }
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        let tmp = self.path(&format!("{name}.tmp"));
        let mut f = create_staging(&tmp)?;
        let written = (|| {
            f.write_all(data)?;
            sync_file(&f, false)
        })();
        // closed before the rename: Windows can't rename a file that is still open
        drop(f);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, self.path(name))?;
        self.sync_dir();
        Ok(())
    }

    fn write_new(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        // a hard link (or an exclusive create) publishes the complete, synced file under `name`
        // only if nothing is there; no step replaces it
        crate::safe_file::write_new(&self.path(name), data)
    }

    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        use std::io::Write;
        // written here, once the log thread is done (the next append starts it again)
        self.stop_log(name);
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        let tmp = self.path(&format!("{name}.tmp"));
        let written = (|| {
            let mut w = Counter { inner: io::BufWriter::with_capacity(1 << 20, create_staging(&tmp)?), n: 0 };
            fill(&mut w)?;
            w.flush()?;
            let f = w.inner.into_inner().map_err(|e| e.into_error())?;
            sync_file(&f, false)?;
            Ok(w.n)
        })();
        let n = match written {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        std::fs::rename(&tmp, self.path(name))?;
        self.sync_dir();
        Ok(n)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.log.as_ref().is_some_and(|w| w.name != name) {
                self.log = None;
            }
            if self.log.is_none() {
                // opened here, so a link at the name or a failing open is this append's error
                let (f, created) = crate::safe_file::open_or_create_regular(&self.path(name), std::fs::OpenOptions::new().append(true))?;
                if created {
                    self.sync_dir();
                }
                let len = f.metadata()?.len();
                drop(f);
                self.appender = None;
                self.log = crate::log_writer::LogWriter::start(&self.dir, name, len);
            }
            if let Some(w) = self.log.as_ref() {
                if w.append(data).is_ok() {
                    return Ok(());
                }
                // the thread is gone: what it was handed is in its last progress; write here from now on
                self.log = None;
                return Err(io::Error::other("the catalog log writer stopped"));
            }
        }
        if self.appender.as_ref().is_none_or(|(n, _)| n != name) {
            // never through a link at the name: the records would land in the file it points to, and
            // a dangling link would create that file (issue #2552)
            let (f, created) = crate::safe_file::open_or_create_regular(&self.path(name), std::fs::OpenOptions::new().append(true))?;
            if created {
                self.sync_dir();
            }
            self.appender = Some((name.to_string(), f));
        }
        let Some((_, f)) = self.appender.as_mut() else { return Err(io::Error::other("appender not open")) };
        let written = f.write_all(data).and_then(|()| sync_file(f, true));
        if written.is_err() {
            // The handle may be dead (a network share that reconnected, a removed volume): the next
            // attempt reopens the file. Part of `data` may be in the file; the caller cuts it back
            // ([`Store::truncate`]) before appending again.
            self.appender = None;
        }
        written
    }

    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(w) = self.log_for(name) {
            return w.truncate(len);
        }
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        // a link at the name is refused, not cut: a log linked elsewhere would empty that file (issue #2552)
        let Some(f) = crate::safe_file::open_regular(&self.path(name), std::fs::OpenOptions::new().write(true))? else {
            // nothing to cut (an append that failed before creating the file)
            return Ok(());
        };
        f.set_len(len)?;
        sync_file(&f, false)
    }

    fn describe(&self) -> String {
        self.dir.display().to_string()
    }

    fn has_folder(&mut self, name: &str) -> io::Result<bool> {
        // (a link to a folder counts: what it points at is what another build would read)
        match std::fs::metadata(self.path(name)) {
            Ok(md) => Ok(md.is_dir()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            // can't tell: an error, not a guess that would let the open go on to write
            Err(e) => Err(e),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        Some(Box::new(FsStore { dir: self.dir.clone(), appender: None, log: None, json_only: self.json_only }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn log_progress(&self, name: &str) -> Option<LogProgress> {
        self.log_for(name).map(crate::log_writer::LogWriter::progress)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn flush(&mut self, name: &str) -> io::Result<()> {
        match self.log_for(name) {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }

    fn dir(&self) -> Option<&Path> {
        (!self.json_only).then_some(self.dir.as_path())
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
    fn write_new(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::collections::btree_map::Entry;
        match self.files.lock().unwrap_or_else(|e| e.into_inner()).entry(name.to_string()) {
            Entry::Occupied(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{name} already exists"))),
            Entry::Vacant(v) => {
                v.insert(data.to_vec());
                Ok(())
            }
        }
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
    /// A file named `<name>/...` stands for a file inside folder `name`.
    fn has_folder(&mut self, name: &str) -> io::Result<bool> {
        let prefix = format!("{name}/");
        Ok(self.files.lock().unwrap_or_else(|e| e.into_inner()).keys().any(|k| k.starts_with(&prefix)))
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        self.background.then(|| Box::new(self.clone()) as Box<dyn Store>)
    }
}

#[cfg(all(test, unix))]
mod tests_staging {
    #[cfg(unix)]
    use super::*;

    /// A link planted at a staging name (`<name>.tmp`) is removed, never followed: the file it points to keeps its
    /// bytes and the save still lands.
    #[test]
    fn a_link_at_the_staging_name_is_never_written_through() {
        let dir = std::env::temp_dir().join(format!("lcc-staging-link-{}", std::process::id()));
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

    /// A link planted at a catalog name is refused, never read, cut back or appended through: the file it points to
    /// keeps its bytes, a dangling link makes nothing at its target, and the library is refused with a message
    /// instead of having its log's target emptied (issue #2552).
    #[cfg(unix)]
    #[test]
    fn links_at_catalog_names_are_never_read_cut_or_appended_through() {
        use crate::journal::{Journal, LOG};
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("lc-store-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        // not a record: the journal would cut the log back to its last good record, here 0 bytes
        symlink(&victim, dir.join(LOG)).unwrap();
        let opened = Journal::open(Box::new(FsStore::open(&dir).unwrap()));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep", "the log's target was cut back");
        let Err(e) = opened else { panic!("a library with a linked {LOG} was opened") };
        assert!(e.to_string().contains("is a link or not a regular file"), "{e}");

        symlink(&victim, dir.join("x.log")).unwrap();
        assert!(store.read("x.log").is_err());
        assert!(store.append("x.log", b"z").is_err());
        assert!(store.truncate("x.log", 0).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        let target = dir.join("made-through-link");
        symlink(&target, dir.join("y.log")).unwrap();
        assert!(store.append("y.log", b"z").is_err());
        assert!(!target.exists(), "a dangling link's target was created");
        // real files as before (the append handle kept between appends included)
        store.append("z.log", b"ab").unwrap();
        store.append("z.log", b"cd").unwrap();
        store.truncate("z.log", 2).unwrap();
        assert_eq!(store.read("z.log").unwrap().unwrap(), b"ab");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The journal's append and truncate (which recovery uses) refuse a non-regular file at the
    /// name instead of following it, so a link planted at `catalog.log` never receives library
    /// bytes (CWE-59, Audit row 107); the snapshot write already removes such a link (#1502).
    #[test]
    #[cfg(unix)]
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
