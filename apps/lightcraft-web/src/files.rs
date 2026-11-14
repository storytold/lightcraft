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

use std::collections::BTreeMap;
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
    generation: u64,
    generation_exhausted: bool,
}

impl Mirror {
    fn changed(&mut self, name: &str) {
        if !LIBRARY_FILES.contains(&name) {
            return;
        }
        match self.generation.checked_add(1) {
            Some(next) if !self.generation_exhausted => self.generation = next,
            _ => self.generation_exhausted = true,
        }
    }

    fn generation(&self) -> Result<u64, String> {
        if self.generation_exhausted {
            Err("library generation counter exhausted; reload before saving to a provider".into())
        } else {
            Ok(self.generation)
        }
    }

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

/// One immutable point-in-time capture. Generation belongs to this `Files` instance.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub generation: u64,
    pub entries: Vec<(String, Vec<u8>)>,
}

impl Files {
    fn lock(&self) -> std::sync::MutexGuard<'_, Mirror> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set a file's stored content (start-up; not dirty).
    pub fn preload(&self, name: &str, data: Vec<u8>) {
        let mut m = self.lock();
        if m.files.get(name) != Some(&data) {
            m.files.insert(name.to_string(), data);
            m.changed(name);
        }
    }

    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.lock().files.get(name).cloned()
    }

    /// Replace a file.
    pub fn write(&self, name: &str, data: &[u8]) {
        let mut m = self.lock();
        if m.files.get(name).map(Vec::as_slice) != Some(data) {
            m.files.insert(name.to_string(), data.to_vec());
            m.changed(name);
        }
        m.mark(name, Dirty::Rewrite);
    }

    pub fn append(&self, name: &str, data: &[u8]) {
        let mut m = self.lock();
        let changed = !data.is_empty() || !m.files.contains_key(name);
        let f = m.files.entry(name.to_string()).or_default();
        let from = f.len();
        f.extend_from_slice(data);
        if changed {
            m.changed(name);
        }
        m.mark(name, Dirty::Append { from });
    }

    pub fn truncate(&self, name: &str, len: usize) {
        let mut m = self.lock();
        if let Some(f) = m.files.get_mut(name) {
            let changed = f.len() > len;
            f.truncate(len);
            if changed {
                m.changed(name);
            }
            m.mark(name, Dirty::Rewrite);
        }
    }

    pub fn generation(&self) -> Result<u64, String> {
        self.lock().generation()
    }

    /// Copy all library files while holding one lock. No mixed catalog/log generation can escape.
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let m = self.lock();
        let generation = m.generation()?;
        let size = LIBRARY_FILES
            .iter()
            .filter_map(|name| m.files.get(*name))
            .try_fold(0usize, |size, data| size.checked_add(data.len()).ok_or("library capture is too large"))?;
        if size > crate::backup::PROVIDER_MAX_BYTES {
            return Err("library capture exceeds the 80 MiB provider limit".into());
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(LIBRARY_FILES.len()).map_err(|_| "not enough memory for a library capture")?;
        for name in LIBRARY_FILES {
            if let Some(data) = m.files.get(name) {
                let mut copy = Vec::new();
                copy.try_reserve_exact(data.len()).map_err(|_| "not enough memory for a library capture")?;
                copy.extend_from_slice(data);
                entries.push((name.into(), copy));
            }
        }
        Ok(Snapshot { generation, entries })
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

/// Is a browser storage error, by its `DOMException` name, "the entry doesn't exist"? Only
/// `NotFoundError` is; anything else (security, type mismatch, an unreadable store) is a failure.
pub fn is_not_found(dom_exception_name: Option<&str>) -> bool {
    dom_exception_name == Some("NotFoundError")
}

/// Preload the library files read at start-up (`None`: the file doesn't exist yet). A file that
/// exists but couldn't be read stops start-up instead (#1507): opening the library without it
/// would show an incomplete catalog and then save over the stored copy.
pub fn preload_library(files: &Files, reads: Vec<(&str, Result<Option<Vec<u8>>, String>)>) -> Result<(), String> {
    let mut failed = Vec::new();
    for (name, read) in reads {
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

    #[test]
    fn snapshots_are_owned_and_content_generations_ignore_no_op_writes() {
        let files = Files::default();
        assert_eq!(files.generation().unwrap(), 0);
        files.preload("catalog.snap", b"before".to_vec());
        let snapshot = files.snapshot().unwrap();
        let initial = snapshot.generation;
        files.preload("catalog.snap", b"before".to_vec());
        files.write("catalog.snap", b"before");
        files.append("catalog.snap", b"");
        files.truncate("catalog.snap", 100);
        assert_eq!(files.generation().unwrap(), initial);
        files.write("catalog.snap", b"after");
        assert_eq!(files.generation().unwrap(), initial + 1);
        assert_eq!(snapshot.entries, vec![("catalog.snap".into(), b"before".to_vec())]);
        files.append("catalog.snap", b"!");
        files.truncate("catalog.snap", 2);
        assert_eq!(files.generation().unwrap(), initial + 3);
    }

    #[test]
    fn snapshot_only_captures_library_files_and_does_not_consume_dirty_flushes() {
        let files = Files::default();
        files.write("catalog.log", b"journal");
        files.write("unrelated.bin", b"not a library file");
        let snapshot = files.snapshot().unwrap();
        assert_eq!(snapshot.entries, vec![("catalog.log".into(), b"journal".to_vec())]);
        assert!(files.is_dirty());
        assert_eq!(files.take_dirty().len(), 2);
        assert!(!files.is_dirty());
    }

    #[test]
    fn auxiliary_file_changes_leave_the_provider_capture_generation_unchanged() {
        let files = Files::default();
        files.preload("catalog.snap", b"catalog".to_vec());
        let before = files.snapshot().unwrap();
        files.preload("prefs.json.corrupt-1", b"damaged preferences".to_vec());
        files.write("presets.json.corrupt-2", b"damaged presets");
        files.append("presets.json.corrupt-2", b" preserved");
        files.truncate("presets.json.corrupt-2", 4);
        assert_eq!(files.snapshot().unwrap(), before);
        assert_eq!(files.get("presets.json.corrupt-2").as_deref(), Some(b"dama".as_slice()));
        let writes = files.take_dirty();
        assert!(writes.iter().any(|write| write.name() == "presets.json.corrupt-2"), "auxiliary files still reach local storage");
        files.append("catalog.snap", b" changed");
        assert_ne!(files.generation().unwrap(), before.generation, "real backup content changes still advance its generation");
    }

    #[test]
    fn exhausted_cloud_generation_never_wraps_and_does_not_break_local_saves() {
        let files = Files::default();
        files.lock().generation = u64::MAX;
        files.write("catalog.log", b"still stored locally");
        assert_eq!(files.get("catalog.log").as_deref(), Some(b"still stored locally".as_slice()));
        assert!(files.generation().is_err());
        assert!(files.snapshot().is_err());
        assert_eq!(files.take_dirty().len(), 1);
        files.write("catalog.log", b"another local write");
        assert!(files.snapshot().is_err());
        assert_eq!(files.lock().generation, u64::MAX);
    }

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
