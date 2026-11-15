//! The library's small files (catalog journal, presets, view state, prefs) on the web.
//!
//! Browser storage is asynchronous on the main thread (OPFS writable streams, IndexedDB), but the
//! catalog [`Store`] is synchronous. So the files are mirrored in memory: they are read once at
//! start-up ([`Files::preload`]), every write changes the mirror immediately and marks the file
//! dirty, and the host flushes dirty files to storage in the background ([`Files::take_dirty`],
//! about once per frame while anything is dirty).
//!
//! Durability: a write is in browser storage a few milliseconds after the command that made it
//! (one flush), not before the command returns as on the desktop. Each flushed file is replaced
//! atomically (OPFS swap file / one IndexedDB transaction), and files are flushed in the order they
//! were last modified, so the journal's recovery rules still hold: e.g. a new `catalog.snap` always
//! lands before the `catalog.log` reset that follows it.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, Mutex};

use lightcraft_engine::catalog::Store;

/// What has to be written for a dirty file.
#[derive(Clone, Debug, PartialEq)]
pub enum FlushOp {
    /// Replace the whole file.
    Write { name: String, data: Vec<u8> },
    /// Bytes appended since the last flush, starting at `offset` (the stored file's length).
    Append { name: String, offset: usize, data: Vec<u8> },
}

impl FlushOp {
    pub fn name(&self) -> &str {
        match self {
            FlushOp::Write { name, .. } | FlushOp::Append { name, .. } => name,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dirty {
    Rewrite,
    /// Only appended since the last flush; the stored file has `from` bytes.
    Append {
        from: usize,
    },
}

#[derive(Default)]
struct Mirror {
    files: BTreeMap<String, Vec<u8>>,
    /// Dirty files, least recently modified first.
    dirty: Vec<(String, Dirty)>,
    /// Why the last flush to browser storage failed (quota, eviction…), until one succeeds.
    error: Option<String>,
    /// Names of kept files that exist in storage but weren't read (see [`Files::hold`]).
    held: BTreeSet<String>,
}

impl Mirror {
    fn mark(&mut self, name: &str, d: Dirty) {
        let prev = self.dirty.iter().position(|(n, _)| n == name).map(|i| self.dirty.remove(i).1);
        let d = match (prev, d) {
            // appends after appends keep the original offset; anything else needs a rewrite
            (Some(Dirty::Append { from }), Dirty::Append { .. }) => Dirty::Append { from },
            (Some(Dirty::Rewrite), _) => Dirty::Rewrite,
            (_, d) => d,
        };
        self.dirty.push((name.to_string(), d));
    }
}

/// Shared in-memory mirror of the library files (cheap to clone).
#[derive(Clone, Default)]
pub struct Files(Arc<Mutex<Mirror>>);

impl Files {
    fn lock(&self) -> std::sync::MutexGuard<'_, Mirror> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set a file's stored content (start-up; not dirty).
    pub fn preload(&self, name: &str, data: Vec<u8>) {
        self.lock().files.insert(name.to_string(), data);
    }

    /// Note that a file exists in storage without reading it: [`Files::create`] never makes a file
    /// of that name (a kept damaged log past the start-up read limit).
    pub fn hold(&self, name: &str) {
        self.lock().held.insert(name.to_string());
    }

    /// Make a file that isn't there yet: `false` (nothing written) when the name is taken, read
    /// or [held](Files::hold). The check and the write are one step.
    pub fn create(&self, name: &str, data: &[u8]) -> bool {
        let mut m = self.lock();
        if m.files.contains_key(name) || m.held.contains(name) {
            return false;
        }
        m.files.insert(name.to_string(), data.to_vec());
        m.mark(name, Dirty::Rewrite);
        true
    }

    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.lock().files.get(name).cloned()
    }

    /// Replace a file.
    pub fn write(&self, name: &str, data: &[u8]) {
        let mut m = self.lock();
        m.files.insert(name.to_string(), data.to_vec());
        m.mark(name, Dirty::Rewrite);
    }

    pub fn append(&self, name: &str, data: &[u8]) {
        let mut m = self.lock();
        let f = m.files.entry(name.to_string()).or_default();
        let from = f.len();
        f.extend_from_slice(data);
        m.mark(name, Dirty::Append { from });
    }

    pub fn truncate(&self, name: &str, len: usize) {
        let mut m = self.lock();
        if let Some(f) = m.files.get_mut(name) {
            f.truncate(len);
            m.mark(name, Dirty::Rewrite);
        }
    }

    pub fn is_dirty(&self) -> bool {
        !self.lock().dirty.is_empty()
    }

    /// Everything to write, in modification order; the files count as clean afterwards (call
    /// [`Files::failed`] for ops that couldn't be written).
    pub fn take_dirty(&self) -> Vec<FlushOp> {
        let mut m = self.lock();
        let dirty = std::mem::take(&mut m.dirty);
        dirty
            .into_iter()
            .map(|(name, d)| {
                let data = m.files.get(&name).cloned().unwrap_or_default();
                match d {
                    Dirty::Append { from } if from <= data.len() => FlushOp::Append { offset: from, data: data[from..].to_vec(), name },
                    _ => FlushOp::Write { name, data },
                }
            })
            .collect()
    }

    /// A flush failed: write these files in full next time (keeps any newer modification order).
    pub fn failed(&self, ops: &[FlushOp]) {
        let mut m = self.lock();
        for op in ops.iter().rev() {
            if !m.dirty.iter().any(|(n, _)| n == op.name()) {
                m.dirty.insert(0, (op.name().to_string(), Dirty::Rewrite));
            } else if let Some(d) = m.dirty.iter_mut().find(|(n, _)| n == op.name()) {
                d.1 = Dirty::Rewrite;
            }
        }
    }

    /// Record the outcome of a flush to browser storage: `Some(why)` when it failed, `None` once
    /// one succeeds. While a flush is failing, the catalog [`Store`] refuses new writes, so the
    /// engine keeps the changes queued and reports them unsaved (`library.info` → `unsavedOps`,
    /// the top bar's warning) instead of pretending they were saved.
    pub fn set_error(&self, e: Option<String>) {
        self.lock().error = e;
    }

    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    /// A catalog [`Store`] over these files.
    pub fn store(&self) -> FilesStore {
        FilesStore(self.clone())
    }
}

/// [`Store`] implementation over [`Files`].
pub struct FilesStore(Files);

impl FilesStore {
    /// Writes fail while browser storage does (see [`Files::set_error`]).
    fn writable(&self) -> io::Result<()> {
        match self.0.error() {
            Some(e) => Err(io::Error::other(format!("browser storage: {e}"))),
            None => Ok(()),
        }
    }
}

impl Store for FilesStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.get(name))
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.writable()?;
        self.0.write(name, data);
        Ok(())
    }
    fn write_new(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.writable()?;
        if self.0.create(name, data) { Ok(()) } else { Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{name} already exists"))) }
    }
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.writable()?;
        self.0.append(name, data);
        Ok(())
    }
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        self.writable()?;
        self.0.truncate(name, len as usize);
        Ok(())
    }
    fn describe(&self) -> String {
        "browser storage".into()
    }
}

/// Names of the library files read at start-up.
pub const LIBRARY_FILES: [&str; 6] = ["catalog.snap", "catalog.log", "presets.json", "view.json", "prefs.json", "ui.json"];

/// How many kept damaged logs start-up reads in full, and how many bytes it reads of them before it
/// stops (the one being read when the limit is passed is kept). A damaged log is as large as the
/// log was, and a folder can hold any number of them: past the limit a copy is only known by name.
pub const MAX_DAMAGED_READS: usize = 32;
pub const MAX_DAMAGED_BYTES: usize = 32 << 20;

/// The kept copies of damaged logs (`catalog.log.damaged-<seq>`, `-<n>`; see the catalog's
/// journal) among the `listed` names of a library folder. Start-up knows them too: the catalog
/// [`Store`] sees only what is preloaded, so a copy left by an earlier page load would look like
/// a free name and be replaced by the next one.
pub fn damaged_logs(listed: Vec<String>) -> Vec<String> {
    // the two shapes the catalog gives a copy: `-<seq>` and `-<seq>-<n>`
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let kept = |n: &String| n.strip_prefix("catalog.log.damaged-").is_some_and(|rest| rest.split('-').count() <= 2 && rest.split('-').all(digits));
    listed.into_iter().filter(kept).collect()
}

/// Read the library into `files` at start-up: the files of [`LIBRARY_FILES`] and the kept damaged
/// logs among the `listed` names (an error listing the folder stops start-up, like a file that
/// can't be read: see [`preload_library`]). `read` gets a file name and returns what browser
/// storage holds. Only the first [`MAX_DAMAGED_READS`] copies (and [`MAX_DAMAGED_BYTES`] of them)
/// are read; the others are held by name ([`Files::hold`]), so none is ever replaced.
pub async fn preload_start_up<R, F>(files: &Files, listed: Result<Vec<String>, String>, read: R) -> Result<(), String>
where
    R: FnMut(String) -> F,
    F: Future<Output = Result<Option<Vec<u8>>, String>>,
{
    preload_start_up_within(files, listed, read, MAX_DAMAGED_READS, MAX_DAMAGED_BYTES).await
}

async fn preload_start_up_within<R, F>(
    files: &Files,
    listed: Result<Vec<String>, String>,
    mut read: R,
    mut reads_left: usize,
    mut bytes_left: usize,
) -> Result<(), String>
where
    R: FnMut(String) -> F,
    F: Future<Output = Result<Option<Vec<u8>>, String>>,
{
    let listed = listed.map_err(|e| format!("couldn't list the library in browser storage ({e})"))?;
    let mut reads = Vec::new();
    for name in LIBRARY_FILES {
        reads.push((name.to_string(), read(name.to_string()).await));
    }
    for name in damaged_logs(listed) {
        if reads_left == 0 || bytes_left == 0 {
            files.hold(&name);
            continue;
        }
        reads_left -= 1;
        let copy = read(name.clone()).await;
        if let Ok(Some(data)) = &copy {
            bytes_left = bytes_left.saturating_sub(data.len());
        }
        reads.push((name, copy));
    }
    preload_library(files, reads)
}

/// Is a browser storage error, by its `DOMException` name, "the entry doesn't exist"? Only
/// `NotFoundError` is; anything else (security, type mismatch, an unreadable store) is a failure.
pub fn is_not_found(dom_exception_name: Option<&str>) -> bool {
    dom_exception_name == Some("NotFoundError")
}

/// Preload the library files read at start-up (`None`: the file doesn't exist yet). A file that
/// exists but couldn't be read stops start-up instead (#1507): opening the library without it
/// would show an incomplete catalog and then save over the stored copy.
pub fn preload_library<N: AsRef<str>>(files: &Files, reads: Vec<(N, Result<Option<Vec<u8>>, String>)>) -> Result<(), String> {
    let mut failed = Vec::new();
    for (name, read) in reads {
        let name = name.as_ref();
        match read {
            Ok(Some(data)) => files.preload(name, data),
            Ok(None) => {}
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("couldn't read the library from browser storage ({})", failed.join("; "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_engine::library::LibraryStores;
    use serde_json::json;

    /// Apply flush ops to a "disk" map, as the backend would.
    fn flush(files: &Files, disk: &mut BTreeMap<String, Vec<u8>>) {
        for op in files.take_dirty() {
            match op {
                FlushOp::Write { name, data } => {
                    disk.insert(name, data);
                }
                FlushOp::Append { name, offset, data } => {
                    let f = disk.entry(name).or_default();
                    assert_eq!(f.len(), offset, "append offset matches the stored length");
                    f.extend_from_slice(&data);
                }
            }
        }
    }

    #[test]
    fn appends_coalesce_and_rewrites_win() {
        let f = Files::default();
        f.preload("log", b"ab".to_vec());
        f.append("log", b"c");
        f.append("log", b"d");
        assert_eq!(f.take_dirty(), vec![FlushOp::Append { name: "log".into(), offset: 2, data: b"cd".to_vec() }]);
        assert!(!f.is_dirty());
        f.write("snap", b"S");
        f.truncate("log", 1);
        f.append("log", b"x");
        let ops = f.take_dirty();
        assert_eq!(
            ops,
            vec![FlushOp::Write { name: "snap".into(), data: b"S".to_vec() }, FlushOp::Write { name: "log".into(), data: b"ax".to_vec() }]
        );
        f.failed(&ops);
        assert_eq!(f.take_dirty().len(), 2);
    }

    #[test]
    fn modification_order_puts_snapshot_before_log_reset() {
        let f = Files::default();
        f.append("catalog.log", b"1\n");
        f.write("catalog.snap", b"snap");
        f.write("catalog.log", b"");
        let names: Vec<_> = f.take_dirty().iter().map(|o| o.name().to_string()).collect();
        assert_eq!(names, ["catalog.snap", "catalog.log"]);
    }

    /// A failing flush (quota exceeded, storage evicted) is not silent: commands report the change
    /// as unsaved and the engine keeps it queued; once storage works again it is written.
    #[test]
    fn flush_failures_show_as_unsaved_and_are_retried() {
        let mut disk = BTreeMap::new();
        let files = Files::default();
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).unwrap();
        flush(&files, &mut disk);
        files.set_error(Some("QuotaExceededError".into()));
        let r = s.execute("photo.rate", &json!({"rating": 5}));
        assert!(matches!(r, Err(lightcraft_engine::EngineError::NotSaved(_))), "{r:?}");
        let (ops, why) = s.unsaved().unwrap();
        assert!(ops > 0 && why.contains("QuotaExceeded"), "{why}");
        files.set_error(None);
        s.persist_if_dirty();
        s.persist().unwrap();
        assert!(s.unsaved().is_none(), "written once storage works again");
        flush(&files, &mut disk);
        let id = s.selection.active.unwrap();
        let files2 = Files::default();
        for (k, v) in &disk {
            files2.preload(k, v.clone());
        }
        let mut s2 = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files2.store()), files: Box::new(files2.store()), on_disk: false };
        s2.open_library_in(stores, true).unwrap();
        assert_eq!(s2.catalog.photo(id).unwrap().rating, 5, "the rating survived");
    }

    /// A backup copies the stored library files, so while a save is failing it would miss the
    /// queued changes: it is refused with the reason until they are written (#1508).
    #[test]
    fn backup_is_refused_while_changes_are_unsaved() {
        let files = Files::default();
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).unwrap();
        assert_eq!(crate::backup::unsaved_refusal(s.unsaved()), None, "everything saved: the backup may start");
        files.set_error(Some("QuotaExceededError".into()));
        let _ = s.execute("photo.rate", &json!({"rating": 5}));
        let refusal = crate::backup::unsaved_refusal(s.unsaved()).expect("refused while the rating is unsaved");
        assert!(refusal.contains("could not be saved") && refusal.contains("QuotaExceeded"), "{refusal}");
        files.set_error(None);
        s.persist_if_dirty();
        s.persist().unwrap();
        assert_eq!(crate::backup::unsaved_refusal(s.unsaved()), None, "allowed again once the change is written");
    }

    #[test]
    fn unsaved_refusal_counts_changes() {
        use crate::backup::unsaved_refusal;
        assert!(unsaved_refusal(Some((1, "quota"))).unwrap().contains("1 change could not"));
        assert!(unsaved_refusal(Some((3, "quota"))).unwrap().contains("3 changes could not"));
    }

    /// Start-up only treats a missing file as absent: a stored file that couldn't be read stops
    /// it, naming the file, instead of opening an incomplete library (#1507).
    #[test]
    fn unreadable_library_file_stops_start_up() {
        let files = Files::default();
        let reads = vec![
            ("catalog.snap", Ok(Some(b"snap".to_vec()))),
            ("catalog.log", Err("NotReadableError: the file could not be read".to_string())),
            ("presets.json", Ok(None)),
        ];
        let e = preload_library(&files, reads).unwrap_err();
        assert!(e.contains("catalog.log") && e.contains("NotReadableError"), "{e}");
    }

    /// A new library (nothing stored yet) still starts.
    #[test]
    fn missing_library_files_are_a_new_library() {
        let files = Files::default();
        let reads = LIBRARY_FILES.iter().map(|n| (*n, Ok(None))).collect();
        preload_library(&files, reads).unwrap();
        assert!(files.get("catalog.snap").is_none());
        let reads = vec![("catalog.snap", Ok(Some(b"snap".to_vec()))), ("catalog.log", Ok(None))];
        preload_library(&files, reads).unwrap();
        assert_eq!(files.get("catalog.snap").as_deref(), Some(&b"snap"[..]));
    }

    #[test]
    fn only_the_names_of_kept_damaged_logs_are_read() {
        let listed = [
            "catalog.log.damaged-7",
            "catalog.log.damaged-7-2",
            "catalog.log.damaged-",
            "catalog.log.damaged-7-2-3",
            "catalog.log.damaged-x",
            "catalog.log",
            "catalog.log.damaged-7.tmp",
        ];
        assert_eq!(damaged_logs(listed.iter().map(|n| n.to_string()).collect()), ["catalog.log.damaged-7", "catalog.log.damaged-7-2"]);
    }

    /// Run a future whose awaits are all ready (the reads of a test are).
    fn block_on<T>(f: impl Future<Output = T>) -> T {
        let mut f = std::pin::pin!(f);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    /// What browser storage holds, as the start-up reads it: the names it lists and the files it
    /// gives. Returns the names read, in order.
    fn start_up(files: &Files, disk: &BTreeMap<String, Vec<u8>>, reads_left: usize, bytes_left: usize) -> Result<Vec<String>, String> {
        let read_names = std::cell::RefCell::new(Vec::new());
        let read = |name: String| {
            let data = Ok(disk.get(&name).cloned());
            read_names.borrow_mut().push(name);
            async move { data }
        };
        let listed = Ok(disk.keys().cloned().collect());
        block_on(preload_start_up_within(files, listed, read, reads_left, bytes_left))?;
        Ok(read_names.into_inner())
    }

    /// Start-up lists the library folder: when that fails, it stops without reading anything.
    #[test]
    fn a_folder_that_cannot_be_listed_stops_start_up() {
        let files = Files::default();
        let read = |_: String| async { panic!("nothing is read when the folder can't be listed") };
        let e = block_on(preload_start_up(&files, Err("NotAllowedError".to_string()), read)).unwrap_err();
        assert!(e.contains("couldn't list the library") && e.contains("NotAllowedError"), "{e}");
    }

    /// Only the library files and the first copies, within the byte limit, are read; every other
    /// copy is held by name.
    #[test]
    fn start_up_reads_a_bounded_number_of_damaged_logs() {
        let mut disk: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        disk.insert("catalog.log".into(), b"log".to_vec());
        for n in 1..=50 {
            disk.insert(format!("catalog.log.damaged-{n}"), vec![0u8; 10]);
        }
        let damaged_read = |names: &[String]| names.iter().filter(|n| n.starts_with("catalog.log.damaged-")).count();

        let files = Files::default();
        let names = start_up(&files, &disk, 32, usize::MAX).unwrap();
        assert_eq!(damaged_read(&names), 32, "{names:?}");
        let held = (1..=50).filter(|n| files.get(&format!("catalog.log.damaged-{n}")).is_none()).count();
        assert_eq!(held, 18, "the copies past the limit aren't read");
        assert!(!files.create("catalog.log.damaged-50", b"new"), "a copy that wasn't read is still taken");
        assert!(!files.create("catalog.log.damaged-1", b"new"), "so is one that was");
        assert!(files.create("catalog.log.damaged-51", b"new"));

        // by bytes: 10 bytes each, a limit of 25 reads three (the third passes it)
        let names = start_up(&Files::default(), &disk, usize::MAX, 25).unwrap();
        assert_eq!(damaged_read(&names), 3, "{names:?}");
    }

    /// A damaged log kept in an earlier page load is known at start-up, so the next one kept at the
    /// same change goes under another name instead of replacing it: whether that copy was read or
    /// is only held by name.
    #[test]
    fn a_damaged_log_kept_in_an_earlier_page_load_is_not_replaced() {
        use lightcraft_engine::catalog::{Journal, Op};

        // one page load: the start-up reads what the folder holds, then open, flush back
        let page_load = |disk: &mut BTreeMap<String, Vec<u8>>, reads_left: usize| {
            let files = Files::default();
            start_up(&files, disk, reads_left, usize::MAX).unwrap();
            let (_, _, report) = Journal::open(Box::new(files.store())).unwrap();
            flush(&files, disk);
            report.damaged
        };
        let damage = |clean: &[u8], mark: &str| {
            let text = String::from_utf8(clean.to_vec()).unwrap();
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            lines[2] = lines[2].replacen("\"op\":{\"op\":\"", &format!("\"op\":{{\"op\":\"{mark}"), 1);
            format!("{}\n", lines.join("\n")).into_bytes()
        };

        let files = Files::default();
        let (mut j, _, _) = Journal::open(Box::new(files.store())).unwrap();
        for _ in 0..6 {
            j.append(&[Op::Batch { ops: vec![] }]).unwrap();
        }
        drop(j);
        let clean = files.get("catalog.log").unwrap();
        let mut disk = BTreeMap::new();
        flush(&files, &mut disk);

        for reads_left in [usize::MAX, 0] {
            let mut disk = disk.clone();
            let first = damage(&clean, "x");
            disk.insert("catalog.log".to_string(), first.clone());
            let name1 = page_load(&mut disk, reads_left).expect("damage reported");
            assert_eq!(disk.get(&name1), Some(&first));

            // another page load, the log damaged again at the same change
            let second = damage(&clean, "y");
            disk.insert("catalog.log".to_string(), second.clone());
            let name2 = page_load(&mut disk, reads_left).expect("damage reported again");
            assert_eq!(name2, format!("{name1}-2"), "(copies read: {reads_left})");
            assert_eq!(disk.get(&name1), Some(&first), "the first copy is untouched");
            assert_eq!(disk.get(&name2), Some(&second));
        }
    }

    #[test]
    fn only_not_found_errors_mean_absent() {
        assert!(is_not_found(Some("NotFoundError")));
        for name in [Some("SecurityError"), Some("TypeMismatchError"), Some("NotReadableError"), Some("InvalidStateError"), None] {
            assert!(!is_not_found(name), "{name:?}");
        }
    }

    #[test]
    fn library_survives_reload_through_flushes() {
        let mut disk = BTreeMap::new();
        let open = |disk: &BTreeMap<String, Vec<u8>>| {
            let files = Files::default();
            for (k, v) in disk {
                files.preload(k, v.clone());
            }
            let mut s = Session::new();
            let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
            s.open_library_in(stores, true).unwrap();
            (s, files)
        };
        let (mut s, files) = open(&disk);
        assert!(!s.catalog.is_empty(), "seeded with the demo photos");
        flush(&files, &mut disk);
        let id = s.selection.active.unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.25})).unwrap();
        s.execute("photo.rate", &json!({"rating": 4})).unwrap();
        s.save_view();
        flush(&files, &mut disk);
        let (s2, _) = open(&disk);
        let p = s2.catalog.photo(id).unwrap();
        assert_eq!(p.rating, 4);
        assert_eq!(p.develop.light.exposure, 1.25);
        assert_eq!(s2.catalog.len(), s.catalog.len(), "not seeded twice");
        assert_eq!(s2.selection.active, s.selection.active);
    }
}
