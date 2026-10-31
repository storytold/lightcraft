//! Byte storage for the catalog files (`catalog.snap`, `catalog.log`).
//!
//! [`FsStore`] is the native implementation (a library directory); [`MemStore`] keeps files in
//! memory (tests, ephemeral sessions) and lets tests simulate crashes by editing the bytes. A web
//! host can implement [`Store`] over OPFS.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// Append and make durable (fsync) before returning. On an error, part of `data` may have
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
    /// Whether the files an upgrade to a newer catalog format replaces are kept beside the
    /// library (see [`crate::journal`] → *Format versions*): the way back to the build that wrote
    /// them. `false` where there is no such build to go back to (the browser always runs the
    /// newest one) and room is short.
    fn keeps_replaced_formats(&self) -> bool {
        true
    }
    /// Human-readable location (diagnostics).
    fn describe(&self) -> String;
    /// Another handle on the same files that a worker thread can write `catalog.snap` through
    /// while this one keeps appending to the log (background compaction). `None` (the default):
    /// snapshots are written synchronously.
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        None
    }
}

/// Appends and truncates open the name's own file, so anything else at the name is the library's
/// problem: a link planted there redirects the write outside the directory (CWE-59), and a device
/// hangs the open. Refused with an error the open can show, before anything is written through.
fn check_regular_directory_entry(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.is_file() => Ok(()),
        Ok(md) => Err(io::Error::other(format!("{} is a {:?}, not a regular file", path.display(), md.file_type()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Files in a directory.
pub struct FsStore {
    dir: PathBuf,
    /// Open append handle (kept between appends; dropped on rewrite/truncate).
    appender: Option<(String, std::fs::File)>,
}

impl FsStore {
    /// Use `dir` (created if missing).
    pub fn open(dir: impl AsRef<Path>) -> io::Result<FsStore> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(FsStore { dir: dir.as_ref().to_path_buf(), appender: None })
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
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

impl Store for FsStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(name)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        let tmp = self.path(&format!("{name}.tmp"));
        let staged = (|| {
            let mut f = create_staging(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, self.path(name))
        })();
        if let Err(e) = staged {
            // no staging file is left beside the library (a kept copy is all or nothing)
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        self.sync_dir();
        Ok(())
    }

    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        use std::io::Write;
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        let tmp = self.path(&format!("{name}.tmp"));
        let written = (|| {
            let mut w = Counter { inner: io::BufWriter::with_capacity(1 << 20, create_staging(&tmp)?), n: 0 };
            fill(&mut w)?;
            w.flush()?;
            let f = w.inner.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            Ok(w.n)
        })();
        let n = match written {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if let Err(e) = std::fs::rename(&tmp, self.path(name)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        self.sync_dir();
        Ok(n)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        if self.appender.as_ref().is_none_or(|(n, _)| n != name) {
            check_regular_directory_entry(&self.path(name))?;
            let existed = self.path(name).exists();
            let f = std::fs::OpenOptions::new().create(true).append(true).open(self.path(name))?;
            if !existed {
                self.sync_dir();
            }
            self.appender = Some((name.to_string(), f));
        }
        let Some((_, f)) = self.appender.as_mut() else { return Err(io::Error::other("appender not open")) };
        let written = f.write_all(data).and_then(|()| f.sync_data());
        if written.is_err() {
            // The handle may be dead (a network share that reconnected, a removed volume): the next
            // attempt reopens the file. Part of `data` may be in the file; the caller cuts it back
            // ([`Store::truncate`]) before appending again.
            self.appender = None;
        }
        written
    }

    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        check_regular_directory_entry(&self.path(name))?;
        let f = match std::fs::OpenOptions::new().write(true).open(self.path(name)) {
            Ok(f) => f,
            // nothing to cut (an append that failed before creating the file)
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        f.set_len(len)?;
        f.sync_all()
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        if self.appender.as_ref().is_some_and(|(n, _)| n == name) {
            self.appender = None;
        }
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn describe(&self) -> String {
        self.dir.display().to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        Some(Box::new(FsStore { dir: self.dir.clone(), appender: None }))
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
    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
        Ok(())
    }
    fn describe(&self) -> String {
        "memory".into()
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        self.background.then(|| Box::new(self.clone()) as Box<dyn Store>)
    }
}

#[cfg(all(test, unix))]
mod tests_staging {
    use super::*;

    /// A link planted at a staging name (`<name>.tmp`) is removed, never followed: the file it points to keeps its
    /// bytes and the save still lands (issue #1502).
    #[cfg(unix)]
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

#[cfg(test)]
mod tests_failed_write {
    use super::*;

    fn dir_with_a_directory_at(tag: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(name)).unwrap();
        dir
    }

    /// A write that can't land (here the rename fails, the name being a directory) leaves no staging file behind: the
    /// files a library keeps before an upgrade are "all or nothing", and a `.tmp` beside them is a copy lying about.
    #[test]
    fn write_atomic_that_fails_leaves_no_staging_file() {
        let dir = dir_with_a_directory_at("write-atomic-fails", "catalog.snap.v6-8");
        let mut store = FsStore::open(&dir).unwrap();
        assert!(store.write_atomic("catalog.snap.v6-8", b"{}").is_err());
        assert!(!dir.join("catalog.snap.v6-8.tmp").exists(), "a staging file was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The streamed write cleans up the same way when the rename fails (it already did when the content can't be written).
    #[test]
    fn write_atomic_with_that_fails_to_rename_leaves_no_staging_file() {
        let dir = dir_with_a_directory_at("write-atomic-with-fails", "catalog.log");
        let mut store = FsStore::open(&dir).unwrap();
        assert!(store.write_atomic_with("catalog.log", &mut |w| w.write_all(b"[1]")).is_err());
        assert!(!dir.join("catalog.log.tmp").exists(), "a staging file was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
