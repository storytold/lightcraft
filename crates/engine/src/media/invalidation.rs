//! A proxy worker's invalidation follows cache replacements for the same library directory.
//! Before a worker writes or removes its first proxy it records that on disk ([`Pending`]), so a
//! process exit before its cache clear reopens the library with a new thumbnail namespace.

use lightcraft_preview::PreviewCache;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

type Current = Mutex<State>;
struct State {
    cache: Arc<PreviewCache>,
}

impl Drop for State {
    fn drop(&mut self) {
        // A session can close while render jobs still hold the cache. Once its last
        // invalidation owner leaves, those jobs must not write into a future session's disk cache.
        self.cache.retire();
    }
}
static LIBRARIES: OnceLock<Mutex<HashMap<std::path::PathBuf, Weak<Current>>>> = OnceLock::new();

/// Marker files are empty and sit at the top of the thumbnail folder, outside the cache's
/// two-digit shard folders, so pruning and clearing never touch them. A full drive can refuse
/// even an empty file (APFS and HFS+ do); see [`Pending::before_change`].
const MARKER_PREFIX: &str = "smart-previews-changing-";
/// The file names of the markers of workers still running in this process: opening their
/// library keeps them. Names are unique (process, time, counter), so they match however the
/// library's path is spelled.
static RUNNING: Mutex<Vec<OsString>> = Mutex::new(Vec::new());

#[derive(Clone)]
pub(crate) struct Invalidation(Arc<Current>);

impl Invalidation {
    pub(crate) fn new(cache: Arc<PreviewCache>) -> Self {
        Self(Arc::new(Mutex::new(State { cache })))
    }

    /// Library paths are resolved before attaching. A worker retains this entry even if its
    /// session closes, so a new session opening that directory joins the same invalidation.
    pub(crate) fn for_disk(dir: &Path, cache: Arc<PreviewCache>) -> Self {
        let mut libraries = LIBRARIES.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
        libraries.retain(|_, value| value.strong_count() > 0);
        let current = match libraries.get(dir).and_then(Weak::upgrade) {
            Some(current) => current,
            None => {
                let current = Arc::new(Mutex::new(State { cache: cache.clone() }));
                libraries.insert(dir.to_path_buf(), Arc::downgrade(&current));
                current
            }
        };
        {
            let mut previous = current.lock().unwrap_or_else(PoisonError::into_inner);
            let different_namespace = previous.cache.disk().zip(cache.disk()).is_some_and(|(old, new)| old.namespace() != new.namespace());
            if !Arc::ptr_eq(&previous.cache, &cache) && !different_namespace {
                previous.cache.retire();
            }
            previous.cache = cache;
        }
        Self(current)
    }

    pub(crate) fn clear(&self) {
        let current = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        current.cache.clear();
        if current.cache.generation() == u64::MAX {
            // No active cache owns this directory. Clear on this worker before releasing
            // the lock that a replacement takes, even if nobody ever reopens the old session.
            if let Some(disk) = current.cache.disk() {
                disk.clear();
            }
        }
    }

    /// The thumbnail changes of one proxy run (see [`Pending`]).
    pub(crate) fn pending(&self) -> Pending {
        Pending { invalidation: self.clone(), marker: None, recorded: false, unrecorded: false, changed: false, refused: None }
    }
}

/// What a run is about to do to a proxy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Change {
    /// Remove it (Discard Smart Previews): never refused, the drive may need the space.
    Remove,
    /// Write it where there was none, or over one that could not be read (a repair).
    Write,
    /// Write it over a usable proxy: a format conversion or a newer camera look.
    Replace,
}

/// One run of Build / Discard Smart Previews or of the look refresh. It clears the thumbnail
/// cache once, when it ends, if it changed a proxy. Before its first change,
/// [`Pending::before_change`] creates a durable marker in the thumbnail folder, which is removed
/// after that clear. If the process ends in between, the marker stays: the next opening finds it
/// ([`interrupted_runs`]) and takes a new thumbnail namespace before reading any thumbnail. Every
/// change counts, even one that leaves correct thumbnails: an interrupted discard leaves those of
/// the removed proxies, whose picture a later first build replaces.
pub(crate) struct Pending {
    invalidation: Invalidation,
    marker: Option<PathBuf>,
    recorded: bool,
    /// The drive was full: no marker, so every replacement is followed by a clear.
    unrecorded: bool,
    changed: bool,
    /// Why recording failed, kept so every later call in the run (another lane's) is refused too:
    /// a retry that happens to succeed must not write while the run reports the failure.
    refused: Option<String>,
}

impl Pending {
    /// Call before each proxy written or removed; only the first call does I/O.
    ///
    /// When the drive is out of space (it can refuse even an empty file), the thumbnail cache is
    /// cleared at once, in every namespace. That removes every thumbnail of the proxies about to
    /// change and frees space, and the marker is tried again. If that fails too, the run goes on
    /// without a marker and clears the cache again after each proxy it replaces, so a thumbnail
    /// rendered from a proxy before its replacement can outlive an exit only between that write
    /// and the clear after it. A first build or a repair needs no such clear: no thumbnail shows
    /// a picture it replaces, and each clear resets every texture and decoded source in the app.
    /// Any other failure (a
    /// read-only folder, where nothing could be cleared either) stops a run that writes, before
    /// it changes anything. A discard goes ahead: what it leaves behind still shows the photo.
    pub(crate) fn before_change(&mut self, change: Change) -> Result<(), String> {
        if self.recorded {
            return Ok(());
        }
        if let Some(e) = &self.refused {
            return Err(e.clone());
        }
        let dir = self.invalidation.0.lock().unwrap_or_else(PoisonError::into_inner).cache.disk().map(|disk| disk.dir().to_path_buf());
        // (a memory-only cache does not outlive the process)
        if let Some(dir) = dir {
            match create_marker(&dir) {
                Ok(marker) => self.marker = Some(marker),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded) => {
                    log::warn!("thumbnail cache: {}: {e}; clearing it before smart previews change", dir.display());
                    self.invalidation.clear();
                    match create_marker(&dir) {
                        Ok(marker) => self.marker = Some(marker),
                        Err(e) => {
                            log::warn!("thumbnail cache: {}: {e}; continuing without a marker, clearing after each replaced proxy", dir.display());
                            self.unrecorded = true;
                        }
                    }
                }
                Err(e) if change == Change::Remove => log::warn!("thumbnail cache: {}: {e}; discarding without a marker", dir.display()),
                Err(e) => {
                    let e = format!("could not record the thumbnail cache change in {}: {e}", dir.display());
                    self.refused = Some(e.clone());
                    return Err(e);
                }
            }
        }
        self.recorded = true;
        Ok(())
    }

    /// A proxy was written or removed: the thumbnail cache is cleared when the run ends, and
    /// after each replacement while the run has no marker.
    pub(crate) fn changed(&mut self, change: Change) {
        self.changed = true;
        if self.unrecorded && change == Change::Replace {
            self.invalidation.clear();
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.changed {
            self.invalidation.clear();
        }
        // Only after the clear. A marker left behind costs a new namespace at the next opening.
        if let Some(marker) = self.marker.take() {
            if let Err(e) = std::fs::remove_file(&marker)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("thumbnail cache: {}: {e}", marker.display());
            }
            finished(&marker);
        }
    }
}

/// Create and sync an empty marker in `dir`. It is registered as running before it exists, so
/// an opening in this process never takes it for a leftover.
fn create_marker(dir: &Path) -> std::io::Result<PathBuf> {
    static N: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let marker = dir.join(format!("{MARKER_PREFIX}{}-{stamp}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    if let Some(name) = marker.file_name() {
        RUNNING.lock().unwrap_or_else(PoisonError::into_inner).push(name.to_os_string());
    }
    #[cfg(test)]
    if let Some(e) = crate::smart::tests_conversion::injected_marker_failure(dir) {
        finished(&marker);
        return Err(e);
    }
    let created = std::fs::create_dir_all(dir).and_then(|()| std::fs::File::create(&marker)).and_then(|file| file.sync_all());
    match created {
        Ok(()) => {
            lightcraft_catalog::safe_file::sync_dir(dir);
            Ok(marker)
        }
        Err(e) => {
            // (it may exist without being synced)
            let _ = std::fs::remove_file(&marker);
            finished(&marker);
            Err(e)
        }
    }
}

fn finished(marker: &Path) {
    if let Some(name) = marker.file_name() {
        RUNNING.lock().unwrap_or_else(PoisonError::into_inner).retain(|m| m != name);
    }
}

/// The markers in the thumbnail folder `thumbs` of runs that ended without clearing the
/// cache: not those of runs still going in this process.
pub(crate) fn interrupted_runs(thumbs: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(thumbs) else { return Vec::new() };
    let running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|name| name.starts_with(MARKER_PREFIX)))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| !running.contains(&e.file_name()))
        .map(|e| e.path())
        .collect()
}
