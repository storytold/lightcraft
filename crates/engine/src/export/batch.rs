use super::ExportOptions;
use super::place::Placer;
use super::prepare::{CANCELLED, Destination, Exported, PreparedExport, prepare_batch};

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
    run_batch_cancellable(items, o, to, write, exists, stop_on_error, progress, None)
}

/// [`run_batch`] that also stops within about a second once `cancel` is set (note 28: a cancelled
/// export stayed "cancelling" for minutes while the photos in flight finished). The photos then in
/// flight are dropped (their lanes stop at their next step and give their memory back); the files
/// already written stay. Returns the files placed until then.
#[allow(clippy::too_many_arguments)]
pub fn run_batch_cancellable(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<serde_json::Value>, String> {
    let lanes = export_parallelism(items.len());
    run_batch_with(items, &mut Placer::new(o, to, write, exists, stop_on_error), progress, lanes, cancel)
}

/// How many photos of a batch of `n` render side by side. Each render already spreads its rows
/// over every core, but parts of it (decode, Smooth Skin, encode) run on one thread, so on a big
/// machine more lanes keep the cores busy: half the cores, up to 12 (memory still caps them
/// through [`crate::memory::export_gate`]). One on wasm32 (no threads).
pub fn export_parallelism(n: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        return 1;
    }
    let lanes = export_lanes_override().unwrap_or_else(|| {
        let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
        (cores / 2).clamp(1, 12)
    });
    lanes.clamp(1, MAX_EXPORT_LANES).min(n.max(1))
}

/// The most photos an export renders side by side, whatever is asked.
pub const MAX_EXPORT_LANES: usize = 64;

/// Export lanes chosen in the install settings (`0`: automatic).
static LANES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Render `lanes` photos side by side in exports from now on (`None`: the automatic number). The
/// environment's `LIGHTCRAFT_EXPORT_LANES` still wins (for measurements).
pub fn set_export_lanes(lanes: Option<usize>) {
    LANES.store(lanes.unwrap_or(0).min(MAX_EXPORT_LANES), std::sync::atomic::Ordering::Relaxed);
}

/// The lanes asked for: `LIGHTCRAFT_EXPORT_LANES`, else [`set_export_lanes`]'s; `None`: automatic.
pub fn export_lanes_override() -> Option<usize> {
    let env = std::env::var("LIGHTCRAFT_EXPORT_LANES").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0);
    env.or(Some(LANES.load(std::sync::atomic::Ordering::Relaxed)).filter(|n| *n > 0))
}

/// Exports render on the GPU when there is one (`false`: on the processor; the install settings,
/// when "Test my machine" found the processor alone faster).
static EXPORT_GPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Let exports use the GPU or not (views and the rest of the app keep it either way).
pub fn set_export_gpu(on: bool) {
    EXPORT_GPU.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether exports use the GPU ([`set_export_gpu`]).
pub fn export_gpu() -> bool {
    EXPORT_GPU.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serializes the tests that change or read the process-wide export tuning (lanes, GPU, mix).
#[cfg(test)]
pub(crate) static TUNING_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`super::export_batch`] with `lanes` photos side by side, on the GPU or not (and with `mix`, on
/// the processor too while the GPU is busy), the files thrown away (a benchmark:
/// [`crate::machine_test`]). Changes nothing process-wide. `cancel` set: stops within a second,
/// `Err("cancelled")`.
pub(crate) fn export_batch_tuned(
    session: &mut crate::Session,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    lanes: usize,
    gpu: bool,
    mix: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut items = prepare_batch(session, ids, o)?;
    for it in &mut items {
        it.gpu = gpu;
        it.mix = gpu && mix;
    }
    let lanes = lanes.clamp(1, MAX_EXPORT_LANES).min(items.len().max(1));
    let to = Destination::default();
    let mut sink = |_: &str, _: &[u8]| Ok(());
    let stop = cancel.clone();
    let files = run_batch_with(items, &mut Placer::new(o, &to, &mut sink, &|_| false, true), &mut |_, _| true, lanes, cancel)?;
    // stopped: what was exported is no measurement
    if stop.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
        return Err("cancelled".into());
    }
    Ok(files)
}

/// Exports render on the processor too while the GPU is busy with other photos (note 28: "Why
/// can't we separate the load at CPU and GPU?"). Off by default; Test my machine turns it on
/// when it measured it faster.
static EXPORT_MIX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Let photos of an export render on the processor while the GPU renders others ([`export_mix`]).
pub fn set_export_mix(on: bool) {
    EXPORT_MIX.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether exports use the processor and the GPU at once ([`set_export_mix`]).
pub fn export_mix() -> bool {
    EXPORT_MIX.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`run_batch`] with `lanes` photos in flight. Without `cancel` the calling thread is one of the
/// lanes; with it the lanes are threads of their own and this one only places the files, looking
/// at `cancel` while it waits, so it can leave at once: lanes still in flight then stop at their
/// next step on their own.
pub(super) fn run_batch_with(
    items: Vec<PreparedExport>,
    placer: &mut Placer<'_>,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    lanes: usize,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<serde_json::Value>, String> {
    placer.single = items.len() == 1;
    let cancelled = || cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
    if cfg!(target_arch = "wasm32") || (cancel.is_none() && (lanes <= 1 || items.len() <= 1)) {
        for (i, item) in items.into_iter().enumerate() {
            if !progress(i, &item.file_name) || cancelled() {
                break;
            }
            let (photo, name, guard, source_dir) = (item.photo, item.file_name.clone(), item.guard.clone(), item.source_dir.clone());
            let r = run_gated(item, &cancelled);
            if cancelled() {
                break;
            }
            placer.place(photo, name, source_dir.as_deref(), &guard, r)?;
        }
        return Ok(std::mem::take(&mut placer.out));
    }
    let heads: Vec<_> = items.iter().map(|i| (i.photo, i.file_name.clone(), i.guard.clone(), i.source_dir.clone())).collect();
    let lanes = lanes.max(1);
    let queue = std::sync::Arc::new(Queue {
        lane: std::sync::Mutex::new(Lane { items: items.into_iter().map(Some).collect(), next: 0, placed: 0, stop: false, done: Default::default() }),
        cv: std::sync::Condvar::new(),
        ahead: lanes,
        cancel: cancel.clone(),
        lanes: std::sync::atomic::AtomicUsize::new(0),
    });
    // stops the lanes however this thread leaves: an error, a cancel, a panic in `progress` or the placer
    let _stop = StopOnDrop(queue.clone());
    // (without a cancel flag the calling thread is a lane too: one thread fewer)
    let threads = if cancel.is_some() { lanes } else { lanes - 1 };
    for k in 0..threads {
        let q = queue.clone();
        // a lane that can't start leaves its photos to the others (and to this thread)
        if std::thread::Builder::new().name(format!("export-{}", k + 1)).spawn(move || q.work()).is_err() {
            break;
        }
        queue.lanes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    for (i, (photo, name, guard, source_dir)) in heads.into_iter().enumerate() {
        if !progress(i, &name) || queue.cancelled() {
            break;
        }
        let Some(e) = queue.result(i) else { break };
        placer.place(photo, name, source_dir.as_deref(), &guard, e)?;
    }
    Ok(std::mem::take(&mut placer.out))
}

/// Stops a [`Queue`] when dropped, also while unwinding from a panic.
struct StopOnDrop(std::sync::Arc<Queue>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Run one photo's export holding its working memory from [`crate::memory::export_gate`]; a panic
/// becomes that photo's error, not the batch's end. `stop()`: the batch was cancelled.
fn run_gated(item: PreparedExport, stop: &dyn Fn() -> bool) -> Result<Exported, String> {
    let _held = crate::memory::export_gate().acquire(item.weight);
    if stop() {
        return Err(CANCELLED.into());
    }
    let what = format!("exporting {}", item.file_name);
    // (its GPU renders take turns with the other photos' and stay behind the window's: `lightcraft_gpu::as_batch`)
    // (on the processor when the GPU is turned off for exports: `set_export_gpu`)
    let gpu = item.gpu;
    let render = || lightcraft_gpu::as_batch(|| item.run_until(stop));
    crate::guard::catch(&what, || if gpu { render() } else { lightcraft_gpu::on_cpu(render) }).and_then(|r| r)
}

/// The photos of a batch shared by its lanes: each lane takes the next one, at most
/// [`Queue::ahead`] past the last placed. That bounds the finished files waiting in memory for
/// their turn too (their working memory is given back to the gate as soon as they are encoded).
struct Queue {
    lane: std::sync::Mutex<Lane>,
    cv: std::sync::Condvar,
    ahead: usize,
    /// The batch's cancel flag ([`run_batch_cancellable`]).
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Lane threads started.
    lanes: std::sync::atomic::AtomicUsize,
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

/// How often the placer looks at the cancel flag while it waits for a photo.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait<'a>(&self, g: std::sync::MutexGuard<'a, Lane>) -> std::sync::MutexGuard<'a, Lane> {
        self.cv.wait_timeout(g, CANCEL_POLL).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0)
    }

    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Stopped: cancelled, or the batch ended early.
    fn stopping(&self) -> bool {
        self.cancelled() || self.lock().stop
    }

    fn finish(&self, i: usize, r: Result<Exported, String>) {
        let mut l = self.lock();
        // (a cancelled or ended batch keeps no results nobody will collect)
        if !l.stop && !self.cancelled() {
            l.done.insert(i, r);
        }
        drop(l);
        self.cv.notify_all();
    }

    /// A lane thread: run photos until none is left or the batch stops.
    fn work(&self) {
        loop {
            let mut l = self.lock();
            let (i, item) = loop {
                if l.stop || self.cancelled() || l.next >= l.items.len() {
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
            let r = run_gated(item, &|| self.stopping());
            self.finish(i, r);
        }
    }

    /// Photo `i`'s result, waiting for it; run here when no lane has started it yet (and this
    /// thread is a lane, or no lane could start). `None`: the batch was cancelled meanwhile.
    fn result(&self, i: usize) -> Option<Result<Exported, String>> {
        let mut l = self.lock();
        loop {
            if self.cancelled() {
                return None;
            }
            if let Some(r) = l.done.remove(&i) {
                l.placed = i + 1;
                drop(l);
                self.cv.notify_all();
                return Some(r);
            }
            let runs_here = self.cancel.is_none() || self.lanes.load(std::sync::atomic::Ordering::Relaxed) == 0;
            if l.next == i && runs_here {
                let Some((_, item)) = l.take_next() else {
                    return Some(Err("export: photo missing from the batch".into()));
                };
                drop(l);
                let r = run_gated(item, &|| self.cancelled());
                self.finish(i, r);
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
