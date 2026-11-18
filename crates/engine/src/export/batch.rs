use super::ExportOptions;
use super::place::Placer;
use super::prepare::{Destination, Exported, PreparedExport};

/// Run prepared exports in order: pick each one's path, and hand the bytes (and sidecars) to
/// `write`. `exists` tells whether a path is taken. The conflict policy applies to a file and its
/// sidecars as one: with Unique both get the same free name, with Skip the photo is skipped when
/// either is taken. A path that is a catalogued original (or its sidecar) is refused whatever the
/// policy. `progress(done, next file)` is called before each photo; returning false cancels the
/// rest. Returns one JSON object per photo: `{path, width, height, bytes, sidecars}`,
/// `{skipped: path}` or (unless `stop_on_error`) `{photo, file, error}`.
///
/// Several photos render side by side ([`export_parallelism`], within the memory of
/// [`crate::memory::export_gate`]), so one photo's decode, encode or GPU wait overlaps another's
/// render (issue #496). Paths, writes, results and `progress` still follow the batch order, and the
/// files are the same as one at a time.
pub fn run_batch(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let lanes = export_parallelism(items.len());
    run_batch_with(items, &mut Placer::new(o, to, write, exists, stop_on_error), progress, lanes)
}

/// How many photos of a batch of `n` render side by side. Each render already spreads its rows
/// over every core; the others fill the cores while it decodes, encodes or waits for the GPU.
/// One on wasm32 (no threads).
pub fn export_parallelism(n: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    (cores / 4).clamp(1, 4).min(n.max(1))
}

/// [`run_batch`] with `lanes` photos in flight (the calling thread is one of them).
pub(super) fn run_batch_with(
    items: Vec<PreparedExport>,
    placer: &mut Placer<'_>,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    lanes: usize,
) -> Result<Vec<serde_json::Value>, String> {
    placer.single = items.len() == 1;
    if lanes <= 1 || items.len() <= 1 {
        for (i, item) in items.into_iter().enumerate() {
            if !progress(i, &item.file_name) {
                break;
            }
            let (photo, name, guard, source_dir) = (item.photo, item.file_name.clone(), item.guard.clone(), item.source_dir.clone());
            placer.place(photo, name, source_dir.as_deref(), &guard, run_gated(item))?;
        }
        return Ok(std::mem::take(&mut placer.out));
    }
    let heads: Vec<_> = items.iter().map(|i| (i.photo, i.file_name.clone(), i.guard.clone(), i.source_dir.clone())).collect();
    let queue = Queue {
        lane: std::sync::Mutex::new(Lane { items: items.into_iter().map(Some).collect(), next: 0, placed: 0, stop: false, done: Default::default() }),
        cv: std::sync::Condvar::new(),
        ahead: lanes,
    };
    std::thread::scope(|sc| {
        // stops the lanes however this thread leaves the scope: a panic in `progress` or the placer
        // would otherwise leave them waiting and the scope's join would never return
        let _stop = StopOnDrop(&queue);
        for k in 1..lanes {
            let q = &queue;
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("export-{k}")).spawn_scoped(sc, move || q.work()).is_err() {
                break;
            }
        }
        let placed = || {
            for (i, (photo, name, guard, source_dir)) in heads.into_iter().enumerate() {
                if !progress(i, &name) {
                    break;
                }
                let e = queue.result(i);
                placer.place(photo, name, source_dir.as_deref(), &guard, e)?;
            }
            Ok::<_, String>(())
        };
        placed()
    })?;
    Ok(std::mem::take(&mut placer.out))
}

/// Stops a [`Queue`] when dropped, also while unwinding from a panic.
struct StopOnDrop<'a>(&'a Queue);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Run one photo's export holding its working memory from [`crate::memory::export_gate`]; a panic
/// becomes that photo's error, not the batch's end.
fn run_gated(item: PreparedExport) -> Result<Exported, String> {
    let _held = crate::memory::export_gate().acquire(item.weight);
    let what = format!("exporting {}", item.file_name);
    crate::guard::catch(&what, || item.run()).and_then(|r| r)
}

/// The photos of a batch shared by its lanes: each lane takes the next one, at most
/// [`Queue::ahead`] past the last placed. That bounds the finished files waiting in memory for
/// their turn too (their working memory is given back to the gate as soon as they are encoded).
struct Queue {
    lane: std::sync::Mutex<Lane>,
    cv: std::sync::Condvar,
    ahead: usize,
}

struct Lane {
    items: Vec<Option<PreparedExport>>,
    /// The next photo to start.
    next: usize,
    /// Photos handed to the placer so far.
    placed: usize,
    stop: bool,
    done: std::collections::HashMap<usize, Result<Exported, String>>,
}

impl Lane {
    fn take_next(&mut self) -> Option<(usize, PreparedExport)> {
        let i = self.next;
        let item = self.items.get_mut(i)?.take()?;
        self.next += 1;
        Some((i, item))
    }
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait<'a>(&self, g: std::sync::MutexGuard<'a, Lane>) -> std::sync::MutexGuard<'a, Lane> {
        self.cv.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, i: usize, r: Result<Exported, String>) {
        self.lock().done.insert(i, r);
        self.cv.notify_all();
    }

    /// A lane thread: run photos until none is left or the batch stops.
    fn work(&self) {
        loop {
            let mut l = self.lock();
            let (i, item) = loop {
                if l.stop || l.next >= l.items.len() {
                    return;
                }
                if l.next < l.placed.saturating_add(self.ahead) {
                    match l.take_next() {
                        Some(t) => break t,
                        None => return,
                    }
                }
                l = self.wait(l);
            };
            drop(l);
            self.finish(i, run_gated(item));
        }
    }

    /// Photo `i`'s result, waiting for it; run here when no lane has started it yet.
    fn result(&self, i: usize) -> Result<Exported, String> {
        let mut l = self.lock();
        loop {
            if let Some(r) = l.done.remove(&i) {
                l.placed = i + 1;
                drop(l);
                self.cv.notify_all();
                return r;
            }
            if l.next == i {
                let Some((_, item)) = l.take_next() else {
                    return Err("export: photo missing from the batch".into());
                };
                drop(l);
                self.finish(i, run_gated(item));
                l = self.lock();
                continue;
            }
            l = self.wait(l);
        }
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }
}
