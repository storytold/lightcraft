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
    /// Folders start-up found beside the files (not files themselves: never dirty, never
    /// flushed), e.g. a `segments` folder the catalog store's `has_folder` check asks about.
    folders: std::collections::BTreeSet<String>,
    /// Dirty files, least recently modified first.
    dirty: Vec<(String, Dirty)>,
    /// Why the last flush to browser storage failed (quota, eviction…), until one succeeds.
    error: Option<String>,
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

    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.lock().files.get(name).cloned()
    }

    /// Note a folder start-up found in browser storage beside the library files (not a file:
    /// it is never flushed back). The catalog's unknown-layout check (#3962) asks the store
    /// about it through [`Store::has_folder`].
    pub fn preload_folder(&self, name: &str) {
        self.lock().folders.insert(name.to_string());
    }

    /// Is there a folder called `name` among the stored library entries? A folder start-up
    /// noted with [`Files::preload_folder`], or a stored entry under `name/` (the mirror's
    /// way of holding a folder's files, like `MemStore`).
    pub fn has_folder(&self, name: &str) -> bool {
        let m = self.lock();
        m.folders.contains(name) || m.files.keys().any(|k| k.starts_with(&format!("{name}/")))
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
    /// The mirror knows the folders start-up found in browser storage (#3962): without this
    /// the trait default (`false`) would let a `segments` library open and be overwritten.
    fn has_folder(&mut self, name: &str) -> io::Result<bool> {
        Ok(self.0.has_folder(name))
    }
}

/// Names of the library files read at start-up.
pub const LIBRARY_FILES: [&str; 6] = ["catalog.snap", "catalog.log", "presets.json", "view.json", "prefs.json", "ui.json"];

/// Is a browser storage error, by its `DOMException` name, "the entry doesn't exist"? Only
/// `NotFoundError` is; anything else (security, type mismatch, an unreadable store) is a failure.
pub fn is_not_found(dom_exception_name: Option<&str>) -> bool {
    dom_exception_name == Some("NotFoundError")
}

/// Does an OPFS failure to open `path` as a directory, by its `DOMException` name, mean "no folder
/// there"? `NotFoundError` (nothing at the path) and `TypeMismatchError` (a regular file of that
/// name, which is not a folder) do. Anything else (security, an unreadable or evicted store) says
/// nothing about the folder: the probe is an error, never "absent" (#1507, #3962).
pub fn means_no_folder(dom_exception_name: Option<&str>) -> bool {
    is_not_found(dom_exception_name) || dom_exception_name == Some("TypeMismatchError")
}

/// The exclusive upper bound of an IndexedDB key range holding every key that starts with
/// `prefix`, and no other: `prefix` with its last character bumped to the next one (`segments/`
/// → `segments0`). IndexedDB orders string keys by UTF-16 code units, and a key that extends
/// `prefix` sorts below that bound whatever follows the prefix; `prefix` + `\u{ffff}` instead
/// would miss a key such as `segments/\u{ffff}part`, which starts with the prefix but sorts after
/// it. An empty `prefix`, or one ending in a character with no next `char`, is returned unchanged
/// (no folder probe asks for one: theirs end in `/`).
pub fn key_range_end(prefix: &str) -> String {
    let mut end = prefix.to_string();
    if let Some(last) = end.pop() {
        end.push(char::from_u32(u32::from(last) + 1).unwrap_or(last));
    }
    end
}

/// Take start-up's answer to "is there a `segments` folder beside the library files?" (#3962):
/// a folder is noted on the mirror, so the catalog store's `has_folder` check refuses the library
/// before anything is written; no folder (a regular file of that name included) changes nothing;
/// a probe that failed stops start-up with the reason, like an unreadable library file (#1507),
/// rather than risk overwriting a library it couldn't see.
pub fn note_segments_probe(files: &Files, probe: Result<bool, String>) -> Result<(), String> {
    match probe {
        Ok(true) => {
            files.preload_folder("segments");
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(e) => Err(format!("couldn't check browser storage for a segments folder beside the library ({e})")),
    }
}

/// What start-up read about the library, taken in the order it is checked: the library files
/// ([`preload_library`]), then the `segments` probe ([`note_segments_probe`]). `Err`: start-up
/// stops with the reason, and the web host doesn't open the library (`load_error`), so nothing
/// in browser storage changes. The reads run at the same time; only their outcomes are ordered.
pub fn preload_start_up(files: &Files, reads: Vec<(&str, Result<Option<Vec<u8>>, String>)>, segments: Result<bool, String>) -> Result<(), String> {
    preload_library(files, reads)?;
    note_segments_probe(files, segments)
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

    /// A `segments` folder in browser storage is a layout this version doesn't read (#3962):
    /// opening through [`FilesStore`] refuses it untouched, like the native and memory
    /// stores — before the fix `FilesStore` reported no folders, so the stored snapshot
    /// would have been replaced by a partial catalog and its log reset.
    #[test]
    fn segments_folder_in_browser_storage_is_refused_untouched() {
        // a saved library, copied the way a reload would read it back
        let mut disk = BTreeMap::new();
        let files = Files::default();
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).unwrap();
        flush(&files, &mut disk);
        let snap_before = disk.get("catalog.snap").cloned().unwrap();
        let log_before = disk.get("catalog.log").cloned().unwrap();

        // the same files, with a `segments` folder beside them in browser storage
        let files2 = Files::default();
        for (k, v) in &disk {
            files2.preload(k, v.clone());
        }
        files2.preload_folder("segments");
        let mut store = files2.store();
        assert!(store.has_folder("segments").unwrap());
        assert!(!store.has_folder("originals").unwrap());
        let mut s2 = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files2.store()), files: Box::new(files2.store()), on_disk: false };
        let Err(e) = s2.open_library_in(stores, true) else { panic!("refused") };
        let msg = e.to_string();
        assert!(msg.contains("segments") && msg.contains("layout isn't one this version reads"), "{msg}");
        assert_eq!(files2.get("catalog.snap").as_deref(), Some(&snap_before[..]), "snapshot untouched");
        assert_eq!(files2.get("catalog.log").as_deref(), Some(&log_before[..]), "log untouched");
        assert!(!files2.is_dirty(), "nothing was written");
    }

    /// The other way a folder shows up in browser storage: a file key under it
    /// (`segments/…`) without an explicit folder entry. `has_folder` sees the key,
    /// and the open is refused the same way.
    #[test]
    fn segments_file_key_in_browser_storage_is_refused_untouched() {
        let mut disk = BTreeMap::new();
        let files = Files::default();
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).unwrap();
        flush(&files, &mut disk);
        let snap_before = disk.get("catalog.snap").cloned().unwrap();

        let files2 = Files::default();
        for (k, v) in &disk {
            files2.preload(k, v.clone());
        }
        files2.preload("segments/0001.seg", b"segment".to_vec());
        let mut store = files2.store();
        assert!(store.has_folder("segments").unwrap());
        let mut s2 = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files2.store()), files: Box::new(files2.store()), on_disk: false };
        let Err(e) = s2.open_library_in(stores, true) else { panic!("refused") };
        assert!(e.to_string().contains("segments"), "{e}");
        assert_eq!(files2.get("catalog.snap").as_deref(), Some(&snap_before[..]), "snapshot untouched");
        assert!(!files2.is_dirty(), "nothing was written");
    }

    /// A saved library as a reload reads it back: its files in a fresh mirror.
    fn saved_library() -> (Files, BTreeMap<String, Vec<u8>>) {
        let mut disk = BTreeMap::new();
        let files = Files::default();
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).unwrap();
        flush(&files, &mut disk);
        let files2 = Files::default();
        for (k, v) in &disk {
            files2.preload(k, v.clone());
        }
        (files2, disk)
    }

    fn open(files: &Files) -> Result<(), String> {
        let mut s = Session::new();
        let stores = LibraryStores { dir: "browser".into(), catalog: Box::new(files.store()), files: Box::new(files.store()), on_disk: false };
        s.open_library_in(stores, true).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Control for #3962: a stored *file* named `segments` (no `segments/…` key, no folder noted)
    /// is not a folder in browser storage, so the library opens and the file keeps its bytes.
    #[test]
    fn lone_segments_file_in_browser_storage_is_no_folder() {
        let (files, _) = saved_library();
        files.preload("segments", b"not a folder".to_vec());
        assert!(!files.store().has_folder("segments").unwrap());
        open(&files).unwrap();
        assert_eq!(files.get("segments").as_deref(), Some(&b"not a folder"[..]));
    }

    /// A `segments` folder in browser storage with no library files beside it is refused too
    /// (#3962), with the layout message, and no `catalog.snap` or `catalog.log` is created: a new
    /// library would otherwise be written beside the folder.
    #[test]
    fn segments_folder_without_a_snapshot_in_browser_storage_is_refused() {
        for noted in [true, false] {
            let files = Files::default();
            if noted {
                files.preload_folder("segments");
            } else {
                files.preload("segments/0001.seg", b"segment".to_vec());
            }
            let e = open(&files).unwrap_err();
            assert!(e.contains("\"segments\" folder") && e.contains("layout isn't one this version reads"), "noted {noted}: {e}");
            assert_eq!(files.get("catalog.snap"), None, "noted {noted}: no snapshot created");
            assert_eq!(files.get("catalog.log"), None, "noted {noted}: no log created");
            assert!(!files.is_dirty(), "noted {noted}: nothing to flush");
        }
    }

    /// OPFS folder probe: a missing entry and a regular file of the name (`TypeMismatchError`
    /// when opened as a directory) are "no folder"; any other failure is an error (#1507, #3962).
    #[test]
    fn only_not_found_and_type_mismatch_mean_no_folder() {
        assert!(means_no_folder(Some("NotFoundError")));
        assert!(means_no_folder(Some("TypeMismatchError")));
        for name in [Some("SecurityError"), Some("NotReadableError"), Some("InvalidStateError"), Some("UnknownError"), Some(""), None] {
            assert!(!means_no_folder(name), "{name:?}");
        }
    }

    /// IndexedDB folder probe: the key range `[prefix, key_range_end(prefix))`, compared the way
    /// IndexedDB compares string keys (UTF-16 code units), holds every key under `segments/` and
    /// nothing else — not the lone `segments` file key, nor neighbours that sort next to it.
    #[test]
    fn key_range_end_bounds_exactly_the_keys_under_the_prefix() {
        let prefix = "segments/";
        let end = key_range_end(prefix);
        assert_eq!(end, "segments0");
        let in_range = |k: &str| k.encode_utf16().ge(prefix.encode_utf16()) && k.encode_utf16().lt(end.encode_utf16());
        for k in ["segments/", "segments/0001.seg", "segments/a/b", "segments/\u{ffff}part", "segments/\u{10ffff}", "segments/~"] {
            assert!(in_range(k), "{k:?} is under the prefix");
        }
        for k in ["segments", "segments.bak", "segments0", "segments00", "segmentsA", "segment", "catalog.snap", "thumbs/x.jpg"] {
            assert!(!in_range(k), "{k:?} is not under the prefix");
        }
        assert_eq!(key_range_end(""), "");
    }

    /// Start-up as the web host runs it (#3962): the library files are taken first, then the
    /// `segments` probe. A folder is noted and the open refused untouched; no folder opens; a
    /// probe that failed stops start-up with its reason (the host then opens nothing); and an
    /// unreadable library file still stops it first, naming the file.
    #[test]
    fn segments_probe_at_start_up() {
        let (_, disk) = saved_library();
        let reads = || disk.iter().map(|(k, v)| (k.as_str(), Ok::<_, String>(Some(v.clone())))).collect::<Vec<_>>();

        let files = Files::default();
        preload_start_up(&files, reads(), Ok(true)).unwrap();
        assert!(files.has_folder("segments"));
        let e = open(&files).unwrap_err();
        assert!(e.contains("layout isn't one this version reads"), "{e}");
        assert_eq!(files.get("catalog.snap"), disk.get("catalog.snap").cloned(), "snapshot untouched");
        assert!(!files.is_dirty(), "nothing written");

        let files = Files::default();
        preload_start_up(&files, reads(), Ok(false)).unwrap();
        assert!(!files.has_folder("segments"));
        open(&files).unwrap();

        let files = Files::default();
        let e = preload_start_up(&files, reads(), Err("UnknownError: the storage backend didn't answer".into())).unwrap_err();
        assert!(e.contains("segments folder") && e.contains("UnknownError"), "{e}");
        assert!(!files.has_folder("segments") && !files.is_dirty());

        let files = Files::default();
        let mut bad = reads();
        bad.push(("prefs.json", Err("NotReadableError: no".into())));
        let e = preload_start_up(&files, bad, Err("UnknownError".into())).unwrap_err();
        assert!(e.contains("prefs.json") && e.contains("NotReadableError"), "{e}");
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
