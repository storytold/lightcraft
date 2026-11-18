//! Where a photo's picture sits on its sensor, for matching flat-field maps ([`crate::vignetting_calibration_library::match_photo`]),
//! read from the raw's header without reading its samples, and in the app without waiting for the file.
//!
//! A raw's placement comes from its header ([`crate::files::placement_of`]): the active area, the default crop, the
//! orientation and the sensor window. [`read`] loads only what that needs: the byte ranges the header planner names
//! ([`lcc_raw::sparse::header_plan`]), or, for formats it hands back whole (a NEF, whose header probe looks at a few
//! masked columns), the file's first [`PREFIXES`] bytes in a buffer of the file's length, the rest zeros. That
//! places every raw measured but the High Efficiency NEF, whose header probe decodes its whole wavelet codestream:
//! such a raw (a file the raw reader recognises but can't place from its start) is read whole, as before. A file
//! that isn't a raw is never read past [`PREFIXES`].
//!
//! The Optics panel asks for the placement whenever it shows a photo. In the app ([`Placements::run_in_background`])
//! the answer comes from a cache that a worker thread fills: an unknown photo is [`Lookup::Pending`] (the map's other
//! checks still run), the worker reads its header and wakes the UI, and [`Placements::generation`] changes so the
//! panel asks again. Everywhere else (CLI, MCP, tests) the header is read at once, as before. Known answers are
//! checked against the file's size and modification time again after [`RECHECK`] (in the app, by the worker).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lcc_catalog::PhotoId;
use lcc_raw::gaintable::SourcePlacement;

/// How long a known placement is used before its file is checked again.
pub const RECHECK: Duration = Duration::from_secs(15);
/// The prefixes tried for a raw the header planner hands back whole.
pub const PREFIXES: [u64; 2] = [1 << 20, 8 << 20];
/// The first bytes read for the header planner.
const PLAN_PREFIX: u64 = 64 << 10;
/// Rounds of the header planner before giving up on it.
const PLAN_ROUNDS: usize = 8;
/// Most photos kept (a few hundred bytes each).
const MAX_KEPT: usize = 16_384;

/// Called (from the worker) when a placement became known.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// What [`Placements::get`] knows about a photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lookup {
    /// Read: where the picture sits, or `None` (not a raw this build places).
    Known(Option<SourcePlacement>),
    /// Being read in the background.
    Pending,
}

/// What [`read`] found and what it cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Read {
    pub placed: Option<SourcePlacement>,
    /// Bytes read from the file.
    pub read: u64,
    /// The file's length.
    pub total: u64,
}

/// The placement of the raw at `path`, from its header only (see the module docs). Blocking: for a worker thread,
/// or for the synchronous mode.
pub fn read(path: &str) -> Option<Read> {
    use std::io::{Read as _, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let total = file.metadata().ok()?.len();
    let len = usize::try_from(total).ok()?;
    if total > crate::bounded_read::MAX_MEDIA_BYTES {
        return None;
    }
    // the file's length, zeros where nothing is loaded (pages nobody touches are never committed)
    let mut buf = vec![0u8; len];
    let mut loaded = lcc_raw::sparse::Ranges::new();
    let mut load = |buf: &mut Vec<u8>, loaded: &mut lcc_raw::sparse::Ranges, start: u64, end: u64| -> Option<()> {
        let end = end.min(total);
        for &(a, b) in loaded.gaps(start, end).spans() {
            let dst = buf.get_mut(usize::try_from(a).ok()?..usize::try_from(b).ok()?)?;
            file.seek(SeekFrom::Start(a)).ok()?;
            file.read_exact(dst).ok()?;
        }
        loaded.insert(start, end);
        Some(())
    };
    load(&mut buf, &mut loaded, 0, PLAN_PREFIX)?;
    for _ in 0..PLAN_ROUNDS {
        match lcc_raw::sparse::header_plan(&buf, &loaded) {
            lcc_raw::sparse::HeaderPlan::Need(ranges) => {
                for &(a, b) in ranges.spans() {
                    load(&mut buf, &mut loaded, a, b)?;
                }
            }
            lcc_raw::sparse::HeaderPlan::Ready => {
                return Some(Read { placed: crate::files::placement_of(&buf), read: loaded.total(), total });
            }
            // (the planner can't say: the prefixes below)
            lcc_raw::sparse::HeaderPlan::Whole(_) => break,
        }
    }
    for prefix in PREFIXES {
        load(&mut buf, &mut loaded, 0, prefix)?;
        if let Some(p) = crate::files::placement_of(&buf) {
            return Some(Read { placed: Some(p), read: loaded.total(), total });
        }
        if prefix >= total {
            break;
        }
    }
    // A raw whose header probe decodes all of its samples (a High Efficiency NEF's wavelet codestream decodes whole):
    // read the rest. Only such a raw gets here; in the app this runs on the worker, never on the UI thread.
    if lcc_raw::probe(&buf).is_some() {
        load(&mut buf, &mut loaded, 0, total)?;
        return Some(Read { placed: crate::files::placement_of(&buf), read: loaded.total(), total });
    }
    Some(Read { placed: None, read: loaded.total(), total })
}

/// The background reader's own two threads for the parallel work a header probe may do (a High Efficiency NEF's
/// probe decodes its whole codestream), so it never takes the shared pool's cores from preview and render work.
/// `None` if the pool can't be made (the shared pool is used then).
fn worker_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(2).thread_name(|i| format!("lc-vignetting-calibration-probe-{i}")).build().ok())
        .as_ref()
}

/// The file's size and modification time (seconds), which a known placement is kept for.
fn stamp(path: &str) -> Option<(u64, i64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Some((m.len(), t))
}

#[derive(Clone)]
struct Kept {
    path: String,
    stamp: Option<(u64, i64)>,
    placed: Option<SourcePlacement>,
    at: Instant,
}

#[derive(Default)]
struct State {
    known: HashMap<PhotoId, Kept>,
    queue: VecDeque<(PhotoId, String)>,
    running: bool,
}

/// Photos' placements, by photo (see the module docs).
#[derive(Clone, Default)]
pub struct Placements {
    state: Arc<Mutex<State>>,
    background: Option<Waker>,
    generation: Arc<AtomicU64>,
}

impl Placements {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// From now on answers come from the cache and a worker thread reads the headers, calling `wake` when one is read.
    pub fn run_in_background(&mut self, wake: Waker) {
        if cfg!(target_arch = "wasm32") {
            return;
        }
        self.background = Some(wake);
    }

    /// Changes whenever a placement read in the background becomes known (ask again then).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Where photo `id`'s raw at `path` sits on its sensor. Synchronous mode: read now if unknown or the file changed.
    /// Background mode: the cached answer, or [`Lookup::Pending`] while the worker reads it; never touches the file.
    pub fn get(&self, id: PhotoId, path: &str) -> Lookup {
        let Some(wake) = self.background.clone() else {
            let now = stamp(path);
            if let Some(k) = self.lock().known.get(&id)
                && k.path == path
                && k.stamp == now
            {
                return Lookup::Known(k.placed);
            }
            let placed = read(path).and_then(|r| r.placed);
            self.keep(id, path, now, placed);
            return Lookup::Known(placed);
        };
        let mut s = self.lock();
        let known = s.known.get(&id).filter(|k| k.path == path).map(|k| (k.placed, k.at.elapsed() >= RECHECK));
        let answer = match known {
            Some((placed, stale)) => {
                if stale {
                    Self::queue(&mut s, id, path);
                }
                Lookup::Known(placed)
            }
            None => {
                Self::queue(&mut s, id, path);
                Lookup::Pending
            }
        };
        if !s.queue.is_empty() && !s.running {
            s.running = true;
            drop(s);
            self.spawn(wake);
        }
        answer
    }

    fn queue(s: &mut State, id: PhotoId, path: &str) {
        if !s.queue.iter().any(|(q, _)| *q == id) {
            s.queue.push_back((id, path.to_string()));
        }
    }

    fn keep(&self, id: PhotoId, path: &str, stamp: Option<(u64, i64)>, placed: Option<SourcePlacement>) {
        let mut s = self.lock();
        if s.known.len() >= MAX_KEPT && !s.known.contains_key(&id) {
            // (the oldest read goes)
            if let Some(old) = s.known.iter().min_by_key(|(_, k)| k.at).map(|(i, _)| *i) {
                s.known.remove(&old);
            }
        }
        s.known.insert(id, Kept { path: path.to_string(), stamp, placed, at: Instant::now() });
    }

    fn spawn(&self, wake: Waker) {
        let me = self.clone();
        let started = std::thread::Builder::new().name("lc-vignetting-calibration-placement".into()).spawn(move || {
            loop {
                let next = {
                    let mut s = me.lock();
                    let next = s.queue.pop_front();
                    if next.is_none() {
                        s.running = false;
                    }
                    next
                };
                let Some((id, path)) = next else { break };
                let now = stamp(&path);
                let same = me.lock().known.get(&id).is_some_and(|k| k.path == path && k.stamp == now && now.is_some());
                if same {
                    // unchanged: only the check's time moves on
                    if let Some(k) = me.lock().known.get_mut(&id) {
                        k.at = Instant::now();
                    }
                    continue;
                }
                // (in a pool of its own: a High Efficiency NEF's probe decodes the whole raw, which must not take the
                // cores previews are made on)
                let placed = worker_pool().map_or_else(|| read(&path), |p| p.install(|| read(&path))).and_then(|r| r.placed);
                me.keep(id, &path, now, placed);
                me.generation.fetch_add(1, Ordering::Relaxed);
                wake();
            }
        });
        if started.is_err() {
            self.lock().running = false;
        }
    }

    /// Forget every photo (another library was opened).
    pub fn clear(&self) {
        let mut s = self.lock();
        s.known.clear();
        s.queue.clear();
    }
}

#[cfg(test)]
#[path = "tests_vignetting_calibration_placement.rs"]
mod tests;
