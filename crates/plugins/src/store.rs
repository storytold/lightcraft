//! A plug-in's own metadata namespace: `photo id → {key: value}`, kept per plug-in in a JSON file
//! next to the installed modules. Plug-ins may always read and write their own namespace; it never
//! touches the catalog.
//!
//! Photo ids belong to one library, so the namespace is kept per library (`library id → photo id →
//! {key: value}`), and every read and write goes to the current library ([`NamespaceStore::set_scope`]).
//! On disk: `{"format": 2, "libraries": {…}}`. A file from before (one flat `photo → {…}` map, with
//! no library) is kept whole and claimed by the first library with an id that opens it, so no value
//! is lost.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::{Error, Result};

/// Largest namespace (serialized) a plug-in may keep.
pub const MAX_NAMESPACE_BYTES: usize = 16 << 20;

type Photos = BTreeMap<String, Map<String, Value>>;

/// The on-disk format version written.
const FORMAT: u64 = 2;
/// Where a format-2 file keeps data from before that no library has claimed yet.
const LEGACY_KEY: &str = "(unclaimed)";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct File {
    format: u64,
    libraries: BTreeMap<String, Photos>,
}

#[derive(Debug, Default)]
pub struct NamespaceStore {
    path: Option<PathBuf>,
    libraries: BTreeMap<String, Photos>,
    /// Data from a file written before namespaces were kept per library, not claimed yet.
    legacy: Option<Photos>,
    /// The library reads and writes go to (`""`: none / a library without an id).
    scope: String,
    dirty: bool,
}

impl NamespaceStore {
    /// An empty store kept in memory only.
    pub fn in_memory() -> NamespaceStore {
        NamespaceStore::default()
    }

    /// Loads the store at `path` (missing = empty; unreadable or corrupt = an error, so a bad file
    /// is never silently replaced).
    pub fn open(path: PathBuf) -> Result<NamespaceStore> {
        let bad = |e: String| Error::Io(format!("{}: {e}", path.display()));
        let mut store = NamespaceStore { path: Some(path.clone()), ..NamespaceStore::default() };
        match std::fs::read(&path) {
            Ok(b) if b.len() > MAX_NAMESPACE_BYTES => return Err(bad("too large".into())),
            Ok(b) => {
                let v: Value = serde_json::from_slice(&b).map_err(|e| bad(e.to_string()))?;
                if v.get("format").is_some() {
                    let f: File = serde_json::from_value(v).map_err(|e| bad(e.to_string()))?;
                    if f.format > FORMAT {
                        return Err(bad(format!("written by a newer version (format {})", f.format)));
                    }
                    store.libraries = f.libraries;
                    store.legacy = store.libraries.remove(LEGACY_KEY).filter(|l| !l.is_empty());
                } else {
                    let legacy: Photos = serde_json::from_value(v).map_err(|e| bad(e.to_string()))?;
                    store.legacy = Some(legacy).filter(|l| !l.is_empty());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(bad(e.to_string())),
        }
        Ok(store)
    }

    /// Reads and writes from now on go to library `library` (`""`: no library). The first library
    /// with an id claims data from before namespaces were kept per library. Returns whether it
    /// claimed it (the store then needs saving).
    pub fn set_scope(&mut self, library: &str) -> bool {
        self.scope = library.to_string();
        if library.is_empty() {
            return false;
        }
        let Some(legacy) = self.legacy.take() else { return false };
        let into = self.libraries.entry(library.to_string()).or_default();
        for (photo, fields) in legacy {
            let have = into.entry(photo).or_default();
            for (k, v) in fields {
                have.entry(k).or_insert(v);
            }
        }
        self.dirty = true;
        true
    }

    fn data(&self) -> Option<&Photos> {
        self.libraries.get(&self.scope)
    }

    pub fn get(&self, photo: &str) -> Value {
        self.data().and_then(|d| d.get(photo)).cloned().map(Value::Object).unwrap_or(Value::Object(Map::new()))
    }

    /// Sets (or with `null`, removes) `key` for `photo`.
    pub fn set(&mut self, photo: &str, key: &str, value: Value) -> std::result::Result<(), String> {
        if photo.is_empty() || photo.len() > 128 || key.is_empty() || key.len() > 128 {
            return Err("photo and key must be 1-128 bytes".into());
        }
        if value.is_null() {
            if let Some(d) = self.libraries.get_mut(&self.scope) {
                if let Some(m) = d.get_mut(photo) {
                    m.remove(key);
                    if m.is_empty() {
                        d.remove(photo);
                    }
                }
                if d.is_empty() {
                    self.libraries.remove(&self.scope);
                }
            }
        } else {
            let old = self.libraries.entry(self.scope.clone()).or_default().entry(photo.to_string()).or_default().insert(key.to_string(), value);
            if self.serialized().map_or(usize::MAX, |b| b.len()) > MAX_NAMESPACE_BYTES {
                if let Some(m) = self.libraries.get_mut(&self.scope).and_then(|d| d.get_mut(photo)) {
                    match old {
                        Some(o) => m.insert(key.to_string(), o),
                        None => m.remove(key),
                    };
                }
                return Err(format!("namespace would exceed {MAX_NAMESPACE_BYTES} bytes"));
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Photo ids with data.
    pub fn photos(&self) -> Vec<String> {
        self.data().map(|d| d.keys().cloned().collect()).unwrap_or_default()
    }

    /// The file's contents. Unclaimed data from before stays in its old shape when there is nothing
    /// else, else under [`LEGACY_KEY`]; a save never drops it.
    fn serialized(&self) -> serde_json::Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct FileRef<'a> {
            format: u64,
            libraries: BTreeMap<&'a str, &'a Photos>,
        }
        match &self.legacy {
            Some(l) if self.libraries.is_empty() => serde_json::to_vec(l),
            legacy => {
                let mut libraries: BTreeMap<&str, &Photos> = self.libraries.iter().map(|(k, v)| (k.as_str(), v)).collect();
                if let Some(l) = legacy {
                    libraries.insert(LEGACY_KEY, l);
                }
                serde_json::to_vec(&FileRef { format: FORMAT, libraries })
            }
        }
    }

    /// Writes the store if it changed (atomically: a temporary file renamed over the old one).
    pub fn save(&mut self) -> Result<()> {
        let (Some(path), true) = (&self.path, self.dirty) else { return Ok(()) };
        let bytes = self.serialized().map_err(|e| Error::Io(e.to_string()))?;
        write_atomic(path, &bytes)?;
        self.dirty = false;
        Ok(())
    }
}

pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lcc-ns-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("p.json")
    }

    /// Photo ids are per library: two libraries' photo 1 keep their own values, also after a reload.
    /// (Before, the namespace was keyed by photo id alone and library B saw library A's value.)
    #[test]
    fn libraries_keep_their_own_values() {
        let path = tmp("libs");
        let mut s = NamespaceStore::open(path.clone()).unwrap();
        s.set_scope("A");
        s.set("1", "job", json!("A-job")).unwrap();
        s.set_scope("B");
        assert_eq!(s.get("1"), json!({}), "library B doesn't see library A's photo 1");
        assert!(s.photos().is_empty());
        s.set("1", "job", json!("B-job")).unwrap();
        s.save().unwrap();
        let mut s = NamespaceStore::open(path).unwrap();
        s.set_scope("A");
        assert_eq!(s.get("1")["job"], "A-job");
        s.set_scope("B");
        assert_eq!(s.get("1")["job"], "B-job");
        s.set("1", "job", Value::Null).unwrap();
        assert!(s.photos().is_empty());
        s.set_scope("A");
        assert_eq!(s.photos(), vec!["1".to_string()]);
    }

    /// A file from before is claimed whole by the first library with an id, and not by a session
    /// without one; until then it is saved back unchanged next to new data.
    #[test]
    fn old_files_are_claimed_by_the_first_library() {
        let path = tmp("legacy");
        std::fs::write(&path, br#"{"7":{"job":"old"}}"#).unwrap();
        let mut s = NamespaceStore::open(path.clone()).unwrap();
        assert!(!s.set_scope(""));
        assert_eq!(s.get("7"), json!({}));
        s.set("8", "job", json!("no library")).unwrap();
        s.save().unwrap();
        let mut s = NamespaceStore::open(path.clone()).unwrap();
        assert!(s.set_scope("A"), "claimed");
        assert_eq!(s.get("7")["job"], "old");
        s.save().unwrap();
        let mut s = NamespaceStore::open(path).unwrap();
        assert!(!s.set_scope("B"), "already claimed");
        assert_eq!(s.get("7"), json!({}));
        s.set_scope("A");
        assert_eq!(s.get("7")["job"], "old");
        s.set_scope("");
        assert_eq!(s.get("8")["job"], "no library");
    }

    #[test]
    fn newer_or_corrupt_files_are_refused() {
        let path = tmp("newer");
        std::fs::write(&path, br#"{"format":99,"libraries":{}}"#).unwrap();
        assert!(NamespaceStore::open(path.clone()).is_err());
        std::fs::write(&path, br#"{"format":2,"libraries":7}"#).unwrap();
        assert!(NamespaceStore::open(path).is_err());
    }
}
