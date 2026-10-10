//! A plug-in's own metadata namespace: `photo id → {key: value}`, kept per plug-in in a JSON file
//! next to the installed modules. Plug-ins may always read and write their own namespace; it never
//! touches the catalog.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::{Error, Result};

/// Largest namespace (serialized) a plug-in may keep.
pub const MAX_NAMESPACE_BYTES: usize = 16 << 20;

#[derive(Debug, Default)]
pub struct NamespaceStore {
    path: Option<PathBuf>,
    data: BTreeMap<String, Map<String, Value>>,
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
        let data = match std::fs::read(&path) {
            Ok(b) if b.len() > MAX_NAMESPACE_BYTES => return Err(Error::Io(format!("{}: too large", path.display()))),
            Ok(b) => serde_json::from_slice(&b).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
        };
        Ok(NamespaceStore { path: Some(path), data, dirty: false })
    }

    pub fn get(&self, photo: &str) -> Value {
        self.data.get(photo).cloned().map(Value::Object).unwrap_or(Value::Object(Map::new()))
    }

    /// Sets (or with `null`, removes) `key` for `photo`.
    pub fn set(&mut self, photo: &str, key: &str, value: Value) -> std::result::Result<(), String> {
        if photo.is_empty() || photo.len() > 128 || key.is_empty() || key.len() > 128 {
            return Err("photo and key must be 1-128 bytes".into());
        }
        if value.is_null() {
            if let Some(m) = self.data.get_mut(photo) {
                m.remove(key);
                if m.is_empty() {
                    self.data.remove(photo);
                }
            }
        } else {
            self.data.entry(photo.to_string()).or_default().insert(key.to_string(), value);
            if serde_json::to_vec(&self.data).map_or(usize::MAX, |b| b.len()) > MAX_NAMESPACE_BYTES {
                if let Some(m) = self.data.get_mut(photo) {
                    m.remove(key);
                }
                return Err(format!("namespace would exceed {MAX_NAMESPACE_BYTES} bytes"));
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Photo ids with data.
    pub fn photos(&self) -> Vec<String> {
        self.data.keys().cloned().collect()
    }

    /// Writes the store if it changed (atomically: a temporary file renamed over the old one).
    pub fn save(&mut self) -> Result<()> {
        let (Some(path), true) = (&self.path, self.dirty) else { return Ok(()) };
        let bytes = serde_json::to_vec(&self.data).map_err(|e| Error::Io(e.to_string()))?;
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
