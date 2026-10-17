//! A set of installed filters, keyed by manifest id.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{Error, Filter, Limits, Result};

/// Installed filters. Thread-safe; a poisoned lock is recovered (the map is never left half-updated).
#[derive(Debug, Default)]
pub struct Registry {
    limits: Limits,
    filters: Mutex<BTreeMap<String, Arc<Filter>>>,
}

/// What [`Registry::load_folder`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderReport {
    /// Ids of the filters installed.
    pub loaded: Vec<String>,
    /// `(file, error)` for each `.wasm` that failed to load.
    pub failed: Vec<(String, String)>,
}

impl Registry {
    /// An empty registry loading modules with `limits`.
    pub fn new(limits: Limits) -> Self {
        Self { limits, filters: Mutex::new(BTreeMap::new()) }
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Filter>>> {
        self.filters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Installs `filter`, replacing one with the same id.
    pub fn install(&self, filter: Filter) -> Arc<Filter> {
        let f = Arc::new(filter);
        self.map().insert(f.id().to_string(), f.clone());
        f
    }

    /// Loads `bytes` with the registry's limits and installs the filter.
    pub fn install_bytes(&self, bytes: &[u8]) -> Result<Arc<Filter>> {
        Ok(self.install(Filter::load(bytes, self.limits.clone())?))
    }

    /// Reads a `.wasm` file (refusing anything that isn't a regular file or exceeds the module
    /// limit), loads and installs it.
    pub fn install_path(&self, path: &std::path::Path) -> Result<Arc<Filter>> {
        Ok(self.install(load_file(path, &self.limits)?))
    }

    pub fn get(&self, id: &str) -> Option<Arc<Filter>> {
        self.map().get(id).cloned()
    }

    /// Installed filters, sorted by id.
    pub fn list(&self) -> Vec<Arc<Filter>> {
        self.map().values().cloned().collect()
    }

    /// Uninstalls `id`; false if it wasn't installed.
    pub fn remove(&self, id: &str) -> bool {
        self.map().remove(id).is_some()
    }

    /// Installs every `*.wasm` directly in `dir` (not recursive), in file-name order. A bad
    /// module is reported and skipped; the others still load.
    pub fn load_folder(&self, dir: &std::path::Path) -> Result<FolderReport> {
        let rd = std::fs::read_dir(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
        let mut files: Vec<std::path::PathBuf> =
            rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wasm"))).collect();
        files.sort();
        files.truncate(1000);
        let mut report = FolderReport::default();
        for f in files {
            match self.install_path(&f) {
                Ok(p) => report.loaded.push(p.id().to_string()),
                Err(e) => report.failed.push((f.display().to_string(), e.to_string())),
            }
        }
        Ok(report)
    }
}

fn load_file(path: &std::path::Path, limits: &Limits) -> Result<Filter> {
    use std::io::Read;
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
    let meta = std::fs::metadata(path).map_err(io)?;
    if !meta.is_file() {
        return Err(Error::Io(format!("{} is not a file", path.display())));
    }
    if meta.len() > limits.max_module_bytes as u64 {
        return Err(Error::Module(format!("{} is {} bytes (limit {})", path.display(), meta.len(), limits.max_module_bytes)));
    }
    let f = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    f.take(limits.max_module_bytes as u64 + 1).read_to_end(&mut bytes).map_err(io)?;
    Ok(Filter::load(&bytes, limits.clone())?.with_source(path.display().to_string()))
}
