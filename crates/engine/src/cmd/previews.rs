//! Build previews ahead of time: each photo's grid thumbnail and its loupe view (standard size,
//! or 1:1), rendered into the memory + disk preview cache so browsing and the loupe are instant.
//! Runs on a background thread (the app keeps working; progress / cancel by command, and a row in
//! the activity stack, issue #345), or inline with `wait` (CLI, MCP, tests).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::activity::{Cancel, TaskGuard};
use crate::media::{RenderJob, THUMB_SIZES};
use crate::{Result, Session};

/// The long edge of a standard-sized preview.
pub const STANDARD_EDGE: usize = 2048;

/// A preview build in progress.
#[derive(Debug, Default)]
pub struct PreviewBuild {
    pub total: usize,
    pub done: AtomicUsize,
    pub failed: AtomicUsize,
    /// Shared with the build's row in the activity stack (its ✕ sets it).
    pub cancel: Arc<AtomicBool>,
    pub finished: AtomicBool,
    /// Damaged smart previews rebuilt (Build Smart Previews only).
    pub repaired: AtomicUsize,
    /// What is built ("" = previews; "smart previews" — Build Smart Previews runs the same way).
    pub what: &'static str,
    /// Why the whole build failed (e.g. the smart previews folder isn't there).
    pub error: std::sync::Mutex<Option<String>>,
    /// Photos whose previews were there already (from an earlier build, the loupe or the photos
    /// prepared ahead): counted as done, not rendered again (PERF-PREVIEW-REUSE).
    pub skipped: AtomicUsize,
    /// Photos left alone because another job was building them at that moment.
    pub shared: AtomicUsize,
    /// Started by the background preparation ([`crate::prep`]): it yields to the photo on screen,
    /// and a build the user (or an import) asks for replaces it.
    pub background: bool,
    /// The photos in build order (ids), and where the user is among them: the next photo built
    /// is the nearest one not built yet from there, in the direction of travel
    /// ([`PreviewBuild::focus_on`]).
    pub order: Vec<u64>,
    pub focus: AtomicUsize,
    pub backward: AtomicBool,
    /// Test-only: the gate this build's lanes wait at before a photo's first render (see
    /// [`test_gate`]). Captured by `build` on the calling thread; production builds never have one.
    #[cfg(test)]
    pub(crate) gate: Option<Arc<test_gate::PreviewGate>>,
}

impl PreviewBuild {
    pub fn json(&self) -> Value {
        json!({
            "total": self.total,
            "done": self.done.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "running": !self.finished.load(Ordering::Relaxed),
            "cancelled": self.cancel.load(Ordering::Relaxed),
            "what": if self.what.is_empty() { "previews" } else { self.what },
            "repaired": self.repaired.load(Ordering::Relaxed),
            "skipped": self.skipped.load(Ordering::Relaxed),
            "shared": self.shared.load(Ordering::Relaxed),
            "background": self.background,
            "error": self.error(),
            // (the process's renders by who they were for: a build's are background work, the
            // loupe's never; `media::background_gpu`)
            "renders": crate::media::render_counts(),
        })
    }

    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    /// The user is at photo `id`, moving `backward` or forward: build from there on.
    pub fn focus_on(&self, id: lightcraft_catalog::PhotoId, backward: bool) {
        if let Some(i) = self.order.iter().position(|p| *p == id.0) {
            self.focus.store(i, Ordering::Relaxed);
            self.backward.store(backward, Ordering::Relaxed);
        }
    }

    /// Index of the next photo to build in `queue` (`None`: none left): the first one still there
    /// from the focus on in the direction of travel, then the rest the other way.
    pub(crate) fn next_index<T>(&self, queue: &[Option<T>]) -> Option<usize> {
        let n = queue.len();
        let at = self.focus.load(Ordering::Relaxed).min(n.saturating_sub(1));
        let there = |i: &usize| queue.get(*i).is_some_and(Option::is_some);
        if self.backward.load(Ordering::Relaxed) {
            (0..=at).rev().find(there).or_else(|| (at..n).find(there))
        } else {
            (at..n).find(there).or_else(|| (0..at).rev().find(there))
        }
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// The jobs that build `id`'s previews: the largest grid thumbnail and the view render at `edge`
/// (`None` = 1:1, the photo's full size), the larger first.
pub(crate) fn jobs_for(s: &mut Session, id: lightcraft_catalog::PhotoId, edge: Option<usize>) -> Vec<RenderJob> {
    let Some(p) = s.catalog.photo(id) else { return Vec::new() };
    let full = p.width.max(p.height) as usize;
    let e = edge.unwrap_or(full).min(full.max(1)).max(1);
    let thumb = THUMB_SIZES[THUMB_SIZES.len() - 1];
    let (view, grid) = (s.loupe_job(id, e, e, true), s.thumb_job(id, thumb));
    // the larger render goes first: the one after it renders from the source it loaded (see
    // `run_photo`), which must not be smaller than what it needs
    if e >= thumb { view.into_iter().chain(grid).collect() } else { grid.into_iter().chain(view).collect() }
}

/// The previews the build of one photo would make are all there already, big enough: its view
/// render at least as large as asked for (`view_cache` of the view job) and its grid thumbnail
/// (`cache` of the thumbnail job), on disk when the cache has one: a render the loupe kept only in
/// memory (its deferred disk write skipped) is built again, and written. Only file headers are read.
pub(crate) fn already_built(jobs: &[RenderJob]) -> bool {
    !jobs.is_empty()
        && jobs.iter().all(|j| {
            if let Some((cache, key)) = &j.view_cache {
                let want = j.request.max_w.max(j.request.max_h) as f64;
                cache.stored_dims(*key).is_some_and(|(w, h)| w.max(h) as f64 >= want * 0.99)
            } else if let Some((cache, key)) = &j.cache {
                cache.stored_dims(*key).is_some()
            } else {
                false
            }
        })
}

/// Photos some preview build is rendering right now, by (the cache they go to, photo id): two
/// builds (a background one being replaced, and the one replacing it) never render the same photo
/// at once.
static BUILDING: std::sync::Mutex<Vec<(usize, u64)>> = std::sync::Mutex::new(Vec::new());

/// Holds a photo in [`BUILDING`] while it is rendered.
struct Building((usize, u64));

impl Building {
    /// `id` of `jobs` (the cache their results go to tells sessions apart).
    fn take(id: u64, jobs: &[RenderJob]) -> Option<Building> {
        let cache = jobs.iter().find_map(|j| j.view_cache.as_ref().or(j.cache.as_ref())).map_or(0, |(c, _)| Arc::as_ptr(c) as usize);
        let id = (cache, id);
        let mut b = BUILDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if b.contains(&id) {
            return None;
        }
        b.push(id);
        Some(Building(id))
    }

    /// [`Self::take`], waiting while another build renders the photo (a build asked for replacing the
    /// background one may meet it on the same photo): the photo is not counted until that render is over, as
    /// built when it made the previews, else rendered here. With whether it had to wait; `None` when `cancel`
    /// was asked for meanwhile. (On the web, with one thread, a photo held is not waited for: `Some((None, true))`.)
    fn wait(id: u64, jobs: &[RenderJob], cancel: &AtomicBool) -> Option<(Option<Building>, bool)> {
        if let Some(b) = Self::take(id, jobs) {
            return Some((Some(b), false));
        }
        Self::wait_held(id, jobs, cancel)
    }

    /// [`Self::wait`] for a photo another build holds.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_held(id: u64, jobs: &[RenderJob], cancel: &AtomicBool) -> Option<(Option<Building>, bool)> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            if let Some(b) = Self::take(id, jobs) {
                return Some((Some(b), true));
            }
        }
    }

    /// [`Self::wait`] for a photo another build holds: on the web, with one thread, it is not waited for.
    #[cfg(target_arch = "wasm32")]
    fn wait_held(_id: u64, _jobs: &[RenderJob], cancel: &AtomicBool) -> Option<(Option<Building>, bool)> {
        (!cancel.load(Ordering::Relaxed)).then_some((None, true))
    }
}

/// Tests: hold `id` of `jobs` as a build rendering it would, until the returned value is dropped.
#[cfg(test)]
pub(crate) fn hold_building(id: u64, jobs: &[RenderJob]) -> Option<Box<dyn std::any::Any>> {
    Building::take(id, jobs).map(|b| Box::new(b) as Box<dyn std::any::Any>)
}

impl Drop for Building {
    fn drop(&mut self) {
        BUILDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|x| *x != self.0);
    }
}

/// The build's row in the activity stack (`what` as in [`PreviewBuild::what`]).
fn task_for(s: &Session, state: &PreviewBuild) -> TaskGuard {
    let (kind, label) = match state.what {
        "" if state.background => ("previews", "Preparing photos in the background"),
        "" => ("previews", "Building previews"),
        "smart previews" => ("smartPreviews", "Building smart previews"),
        _ => ("smartPreviews", "Discarding smart previews"),
    };
    let task = s.activity.start(kind, label, Cancel::Flag(state.cancel.clone()));
    task.progress(0, state.total as u64);
    task
}

/// Render one photo's jobs in order. The source the first job decodes serves the ones after it
/// (the grid thumbnail is made from the view's source instead of decoding the file again).
/// `None` when `cancel` was asked for before a job started: that photo is not counted as done.
/// Previews are made ahead, never for the photo on screen: background GPU work, on the second
/// graphics card first and behind the photo on screen ([`crate::media::background_photo`]). The
/// source is decoded first, outside the photo's turn at a card (a raw's decode is a second or more
/// of processor work; the card's turn is for the renders).
fn run_photo_cancellable(mut jobs: Vec<RenderJob>, cancel: Option<&AtomicBool>) -> Option<bool> {
    let Some(first) = jobs.first() else { return Some(true) };
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return None;
    }
    let decoded = match &first.source {
        crate::media::SourceRef::Loaded(d) => (**d).clone(),
        // (an unreadable photo fails here, as its jobs would)
        source => match source.load_source() {
            Ok(d) => d,
            Err(e) => {
                log::info!("building previews: {e}");
                return Some(false);
            }
        },
    };
    let src_px = decoded.image.width.saturating_mul(decoded.image.height);
    let out_px = jobs.iter().map(|j| j.request.max_w.saturating_mul(j.request.max_h)).max().unwrap_or(0).min(src_px.max(1));
    for j in &mut jobs {
        j.source = crate::media::SourceRef::Loaded(Box::new(decoded.clone()));
    }
    crate::media::background_photo("building previews", src_px, out_px, || run_photo_jobs(jobs, cancel)).unwrap_or(Some(false))
}

/// [`run_photo_cancellable`]'s work.
fn run_photo_jobs(jobs: Vec<RenderJob>, cancel: Option<&AtomicBool>) -> Option<bool> {
    let mut decoded: Option<crate::media::DecodedSource> = None;
    let mut ok = true;
    for mut j in jobs {
        // between a photo's own previews too (a cancelled 1:1 view skips its grid thumbnail)
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return None;
        }
        if let Some(src) = &decoded
            && !matches!(j.source, crate::media::SourceRef::Loaded(_))
        {
            j.source = crate::media::SourceRef::Loaded(Box::new(src.clone()));
        }
        // The view render is written to the disk cache here, before the photo counts as built,
        // not left to the cache's writer (which skips writes when it falls behind): a built
        // preview is on disk, for this session and the next. (Taken out of the job: it would only
        // queue the write, and would serve a smaller cached view.)
        // A preview that isn't kept fails the photo: it is not built (a full or unwritable cache folder, or the
        // cache discarded while the photo rendered).
        let keep = j.view_cache.take().map(|v| (v, j.cache_generation));
        let r = j.run();
        if let (Some(((cache, key), generation)), Ok(img)) = (keep, r.rendered.as_ref())
            && !cache.put_at(generation, key, Arc::new(img.image.clone()))
        {
            if cache.generation() == generation {
                log::warn!("building previews: the view preview could not be written to the cache folder");
            } else {
                log::warn!("building previews: the preview cache was discarded while the photo rendered");
            }
            ok = false;
        }
        ok &= r.rendered.is_ok();
        if decoded.is_none() {
            decoded = r.loaded;
        }
    }
    Some(ok)
}

/// [`run_photo_cancellable`] for callers that render a photo without a cancel flag.
#[cfg(test)]
pub(crate) fn run_photo(jobs: Vec<RenderJob>) -> bool {
    run_photo_cancellable(jobs, None) == Some(true)
}

/// How many photos render side by side: as many as an export batch for standard previews (each
/// render spreads over the cores already; the others fill them while one decodes), two per graphics
/// card for 1:1 previews (a full-size source each; one card needs two to stay busy, and with a
/// second card two more feed it), within the export's memory-bound count.
fn lanes(n: usize, full: bool) -> usize {
    let l = crate::export::export_parallelism(n);
    if full { l.min(lightcraft_gpu::batch_lanes_hint(2)) } else { l }
}

/// A test-only rendezvous at the start of a photo's jobs, so a cancel can be delivered at a known
/// point instead of racing a render. One gate belongs to the build `build` starts on the test
/// thread that installed it ([`install`]); it is never global and production builds have none.
#[cfg(test)]
pub(crate) mod test_gate {
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    /// The longest a lane waits for a test to open the gate: if a test ends without its
    /// [`ReleaseOnDrop`] running, the build still finishes instead of stranding its worker.
    const MAX_HOLD: Duration = Duration::from_secs(30);
    /// The longest a test waits for a lane to reach the gate.
    const MAX_ENTER_WAIT: Duration = Duration::from_secs(10);

    #[derive(Default)]
    struct State {
        entered: usize,
        released: bool,
    }

    /// The gate a test's preview build waits at before rendering a photo (see `install`).
    #[derive(Default)]
    pub(crate) struct PreviewGate {
        state: Mutex<State>,
        opened: Condvar,
    }

    impl std::fmt::Debug for PreviewGate {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PreviewGate").finish_non_exhaustive()
        }
    }

    impl PreviewGate {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// Called by a lane about to render a photo's first job; returns once the test opens the
        /// gate, or after `MAX_HOLD` so a lost test cannot strand the worker.
        pub(crate) fn enter(&self) {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.entered = st.entered.saturating_add(1);
            self.opened.notify_all();
            let deadline = Instant::now() + MAX_HOLD;
            while !st.released {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, _) = self.opened.wait_timeout(st, remaining).unwrap_or_else(|e| e.into_inner());
                st = next;
            }
        }

        /// Wait (at most `MAX_ENTER_WAIT`) until at least `n` lanes have reached the gate.
        pub(crate) fn wait_entered(&self, n: usize) -> bool {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let deadline = Instant::now() + MAX_ENTER_WAIT;
            loop {
                if st.entered >= n {
                    return true;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (next, _) = self.opened.wait_timeout(st, remaining).unwrap_or_else(|e| e.into_inner());
                st = next;
            }
        }

        /// Open the gate: every lane waiting, or arriving later, passes without waiting.
        pub(crate) fn release(&self) {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.released = true;
            self.opened.notify_all();
        }
    }

    /// Opens the gate when dropped, wherever a test ends (assertion failure, panic, early return).
    pub(crate) struct ReleaseOnDrop(pub(crate) Arc<PreviewGate>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    thread_local! {
        /// The gate for the next preview build started on this thread, taken by `build` so it is
        /// per invocation (never shared by two builds or two tests).
        static NEXT_GATE: std::cell::RefCell<Option<Arc<PreviewGate>>> = const { std::cell::RefCell::new(None) };
    }

    /// Have the next preview build started on this thread wait at `gate` before its first photo.
    pub(crate) fn install(gate: Arc<PreviewGate>) {
        NEXT_GATE.with(|g| *g.borrow_mut() = Some(gate));
    }

    /// Take the gate installed for this build, leaving none for later builds.
    pub(crate) fn take() -> Option<Arc<PreviewGate>> {
        NEXT_GATE.with(|g| g.borrow_mut().take())
    }

    thread_local! {
        /// The next build started on this thread fails to start its worker (a thread the system
        /// won't give), taken by `build`.
        static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Have the next build started on this thread fail to start its worker.
    pub(crate) fn fail_next_spawn() {
        FAIL_SPAWN.with(|f| f.set(true));
    }

    /// Whether this build's worker is to fail to start (once).
    pub(crate) fn take_fail_spawn() -> bool {
        FAIL_SPAWN.with(std::cell::Cell::take)
    }
}

/// One photo's work in a build: its id and its render jobs ([`jobs_for`]).
type PhotoJobs = (u64, Vec<RenderJob>);

/// Build every photo of `jobs` on `lanes` threads. `force`: render even the photos whose previews
/// are there. `task`: the build's own row (`None`: another job's row shows it, e.g. the import's
/// Getting photos ready).
fn run_all(jobs: Vec<PhotoJobs>, state: &PreviewBuild, task: Option<&TaskGuard>, lanes: usize, force: bool) {
    let total = jobs.len();
    let queue = std::sync::Mutex::new(jobs.into_iter().map(Some).collect::<Vec<_>>());
    let work = || {
        loop {
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            // the background preparation waits while the photo on screen is being made
            if state.background && crate::prep::wait_for_foreground(&state.cancel) {
                break;
            }
            let next = {
                let mut q = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.next_index(&q).and_then(|i| q.get_mut(i)).and_then(Option::take)
            };
            let Some((id, photo_jobs)) = next else { break };
            // A cancel accepted while this lane waited for the queue (or for another lane's
            // photo) stops before this photo's first render; a cancelled photo is not counted.
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            if !force && already_built(&photo_jobs) {
                state.skipped.fetch_add(1, Ordering::Relaxed);
            } else if let Some((held, shared)) = Building::wait(id, &photo_jobs, &state.cancel) {
                // another build was rendering this photo: what it made counts, if it made it (not
                // for a forced build, which renders every photo itself)
                if !force && shared && (held.is_none() || already_built(&photo_jobs)) {
                    state.shared.fetch_add(1, Ordering::Relaxed);
                    let done = state.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                    if let Some(task) = task {
                        task.progress(done as u64, total as u64);
                    }
                    continue;
                }
                #[cfg(test)]
                if let Some(gate) = &state.gate {
                    gate.enter();
                }
                // the background preparation's decodes wait at the memory gate behind the photo on
                // screen's, as the other background work does
                let build = || run_photo_cancellable(photo_jobs, Some(&state.cancel));
                let run = || if state.background { crate::memory::in_background(build) } else { build() };
                let outcome = crate::guard::catch("building previews", run).unwrap_or(Some(false));
                let Some(ok) = outcome else { break };
                if !ok {
                    state.failed.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                // cancelled while another build was rendering it: not counted
                break;
            }
            let done = state.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            if let Some(task) = task {
                task.progress(done as u64, total as u64);
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        for k in 1..lanes.max(1) {
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("previews-{k}")).spawn_scoped(sc, work).is_err() {
                break;
            }
        }
        work();
    });
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lanes;
        work();
    }
    state.finished.store(true, Ordering::Relaxed);
}

/// The long edge a standard preview is built at: [`STANDARD_EDGE`], or what the loupe asks for
/// on this screen when that is more ([`Session::loupe_edge`]), so the loupe finds what was built.
pub fn standard_edge(s: &Session) -> usize {
    STANDARD_EDGE.max(s.loupe_edge.min(8192))
}

fn build(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.buildPreviews";
    // tests: this invocation's gate, consumed here so an early error cannot attach it to a later build
    #[cfg(test)]
    let gate = test_gate::take();
    let edge = match str_param(p, "size").unwrap_or("standard") {
        "standard" => Some(p.get("edge").and_then(Value::as_u64).map_or_else(|| standard_edge(s), |e| e.clamp(256, 8192) as usize)),
        "full" | "1:1" => None,
        other => return Err(bad(C, format!("unknown size `{other}` (standard|full)"))),
    };
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    let force = p.get("force").and_then(Value::as_bool).unwrap_or(false);
    // (`row: false`: the job that asked shows the progress in its own row)
    let row = p.get("row").and_then(Value::as_bool).unwrap_or(true);
    // the background preparation gives way to a build that was asked for; nothing replaces a build
    // that was asked for (it carries on, and the photos it built are skipped by the next). The
    // background one is stopped only once this request is known to be good (below): a request
    // with bad ids leaves it running (CodeRabbit on #3544)
    let mut gives_way = None;
    if let Some(b) = s.preview_build.as_ref().filter(|b| b.running()) {
        if b.background && !background {
            gives_way = Some(b.cancel.clone());
        } else {
            return Err(bad(C, "a preview build is already running"));
        }
    }
    s.fit_disk_cache();
    // explicit ids, else the selected photos (more than one: as the menu says), else everything in
    // view (a single photo selected is the one open, not a choice of photos: "Visible" in the menu)
    let ids = if p.get("ids").is_some() || p.get("id").is_some() || (!background && s.selection.ids.len() > 1) {
        s.targets(p)?
    } else {
        s.visible_cloned()
    };
    let jobs: Vec<PhotoJobs> = ids.iter().map(|id| (id.0, jobs_for(s, *id, edge))).filter(|j| !j.1.is_empty()).collect();
    // the background build stays the session's until this one has started (a worker that can't
    // start leaves it running and in place: CodeRabbit on #3544)
    let previous = s.preview_build.clone();
    let order = jobs.iter().map(|j| j.0).collect();
    #[cfg_attr(not(test), allow(unused_mut))]
    let mut build_state = PreviewBuild { total: jobs.len(), background, order, ..Default::default() };
    #[cfg(test)]
    {
        build_state.gate = gate;
    }
    let state = Arc::new(build_state);
    if let Some(at) = p.get("from").and_then(Value::as_u64) {
        state.focus_on(lightcraft_catalog::PhotoId(at), p.get("backward").and_then(Value::as_bool).unwrap_or(false));
    }
    s.preview_build = Some(state.clone());
    let task = row.then(|| task_for(s, &state));
    // the background preparation leaves most of the machine to the photo on screen and the user
    let lanes = if background { crate::prep::background_lanes() } else { lanes(jobs.len(), edge.is_none()) };
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32");
    if wait {
        if let Some(cancel) = gives_way {
            cancel.store(true, Ordering::Relaxed);
        }
        run_all(jobs, &state, task.as_ref(), lanes, force);
        return Ok(state.json());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row stays while the worker runs (it is dropped with the closure, however that ends)
        let st = state.clone();
        #[cfg(test)]
        let started = if test_gate::take_fail_spawn() {
            drop(task);
            Err(std::io::Error::other("no thread (test)"))
        } else {
            std::thread::Builder::new().name("lc-build-previews".into()).spawn(move || run_all(jobs, &st, task.as_ref(), lanes, force))
        };
        #[cfg(not(test))]
        let started = std::thread::Builder::new().name("lc-build-previews".into()).spawn(move || run_all(jobs, &st, task.as_ref(), lanes, force));
        if let Err(e) = started {
            // never started: not left "running", which would hold every later build back, and the
            // build it was to replace carries on
            state.finished.store(true, Ordering::Relaxed);
            s.preview_build = previous;
            return Err(bad(C, format!("could not start: {e}")));
        }
        if let Some(cancel) = gives_way {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = previous;
    Ok(state.json())
}

/// One photo's smart preview to build or discard: (photo, its file in the smart previews folder,
/// how to load the original at preview size, whether the photo can take a preview-fitted look:
/// vendor raws and DNGs (whose tone can come from their preview); only those proxies go stale when
/// the fit changes; an older encoding makes any proxy stale).
#[cfg(not(target_arch = "wasm32"))]
type SmartJob = (lightcraft_catalog::PhotoId, std::path::PathBuf, Option<crate::media::SourceRef>, bool);

/// DNG tone fits also go stale, although DNGs keep their own matrix and Kelvin white-balance scale.
/// So do preview-only raws: their proxy is the embedded JPEG, read in the colour space the raw
/// declares, which a look version bump can change too (#2654).
#[cfg(not(target_arch = "wasm32"))]
fn preview_fitted_look(photo: &lightcraft_catalog::Photo) -> bool {
    photo.fitted_look()
        || (photo.develops_raw() && photo.format.eq_ignore_ascii_case("DNG"))
        || (photo.kind == lightcraft_catalog::MediaKind::Raw && photo.preview_only.is_some())
}

/// Held while one proxy is checked and (re)written, so Build Smart Previews and the opening look
/// refresh ([`refresh_stale_smart_previews`]), on their own threads, never write the same file at once.
#[cfg(not(target_arch = "wasm32"))]
static SMART_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What Build / Discard Smart Previews did.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct SmartCounts {
    /// Proxies there now (complete ones kept, missing or damaged ones written).
    built: usize,
    /// Of those, damaged ones (cut short by a crash or a full drive) that were rebuilt.
    repaired: usize,
    /// Of those, proxies written by an older camera-look fit that were rebuilt from their online originals.
    refreshed: usize,
    /// Of those kept as they were: out of date, but the original can't be read now (offline), so
    /// the stored look stays and the next run with the original online rebuilds them.
    stale_kept: usize,
    removed: usize,
    /// `[[id, why]]`
    failed: Vec<Value>,
}

/// `source` if it reads the photo's original, else `None`. A source decoded from a smart preview
/// (it now carries the proxy's stored facts), a proxy standing in for an offline original, and the
/// proxy fallback of a file source are not the original: refreshing a stale proxy from one of them
/// would stamp its older look as current.
#[cfg(not(target_arch = "wasm32"))]
fn original_only(source: crate::media::SourceRef) -> Option<crate::media::SourceRef> {
    use crate::media::SourceRef;
    match source {
        SourceRef::File { path, max_edge, loader, denoise, .. } => {
            Some(SourceRef::File { path, max_edge, loader, fallback: None, denoise, reduced: None })
        }
        SourceRef::Loaded(d) if d.info.is_some() && !d.from_proxy && !d.reduced => Some(SourceRef::Loaded(d)),
        _ => None,
    }
}

/// The file-system half of Build / Discard Smart Previews (runs on a worker thread in the app).
/// `custom`: the folder was chosen (on another drive, perhaps): it must be there, it is never
/// recreated. With `state`, progress and cancel.
#[cfg(not(target_arch = "wasm32"))]
fn smart_run(
    dir: &std::path::Path,
    custom: bool,
    discard: bool,
    jobs: Vec<SmartJob>,
    state: Option<(&PreviewBuild, &TaskGuard)>,
) -> std::result::Result<SmartCounts, String> {
    let mut n = SmartCounts::default();
    if !discard {
        // a chosen folder on another drive must be there: never recreate it on this one
        if custom {
            crate::smart::check_writable(dir)?;
        } else {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    for (id, path, source, look) in jobs {
        if state.is_some_and(|(st, _)| st.cancel.load(Ordering::Relaxed)) {
            break;
        }
        let _one = SMART_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read either the new shard or a pre-upgrade flat file, without making new work
        // depend on having the original online.
        let path = crate::smart::existing_file_path(dir, path);
        if discard {
            if std::fs::remove_file(&path).is_ok() {
                n.removed += 1;
            }
        } else {
            // a complete, current proxy is kept; a missing or damaged one (cut short by a crash or a
            // full drive) is (re)built, and so is one written by an older encoding or camera-look
            // fit (see `smart::ENCODE_VERSION` and `camera_preview::LOOK_VERSION`) when its
            // original can be read
            let damaged = path.exists();
            let stale = damaged && crate::smart::is_stale_for(&path, look);
            if damaged && !stale && crate::smart::is_valid(&path) {
                n.built += 1;
            } else {
                // a stale proxy is rebuilt from the original only: never from itself (the file
                // fallback or a source decoded from a proxy would stamp the old look as current)
                let source = if stale { source.and_then(original_only) } else { source };
                // (the original is read as work done ahead: any GPU work in it, an AI Denoise
                // picture made first, is background work like Build Previews', `media::background_gpu`)
                // atomic: a failed write leaves no partial proxy that would pass for a built one;
                // and synced (issue #134): built so the photo can be edited while its original
                // is offline, when it is the only copy — the sync is small next to the decode
                let r = source
                    .ok_or_else(|| "nothing to build from".to_string())
                    .and_then(|s| crate::media::background_gpu("building smart previews", || s.load_source()).and_then(|r| r))
                    .and_then(|src| {
                        let info = src.info_or(Default::default());
                        crate::smart::encode(&src.image, Some(&info))
                    })
                    .and_then(|b| {
                        let parent = path.parent().ok_or_else(|| format!("{}: no parent folder", path.display()))?;
                        // Only create the leaf shard: do not recreate a missing external drive.
                        if !parent.is_dir() {
                            std::fs::create_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                        }
                        lightcraft_catalog::safe_file::write_atomic(&path, &b).map_err(|e| format!("{}: {e}", path.display()))
                    });
                match r {
                    Ok(()) => {
                        n.built += 1;
                        if stale {
                            n.refreshed += 1;
                        } else if damaged {
                            n.repaired += 1;
                            if let Some((st, _)) = state {
                                st.repaired.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(e) if stale => {
                        // offline (or unreadable) original: the proxy is all there is, keep its look
                        log::info!("smart preview {} keeps its older look: {e}", path.display());
                        n.built += 1;
                        n.stale_kept += 1;
                    }
                    Err(e) => {
                        n.failed.push(json!([id.0, e]));
                        if let Some((st, _)) = state {
                            st.failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        if let Some((st, task)) = state {
            let done = st.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            task.progress(done as u64, st.total as u64);
        }
    }
    Ok(n)
}

/// Smart previews: build (from the original, at preview size) or discard the proxies that keep
/// photos editable while their originals are offline. With `background` (the app) the files are
/// read and written on a worker thread; progress like Build Previews (`library.previewProgress`).
#[cfg(not(target_arch = "wasm32"))]
fn smart(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviews";
    let dir = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let discard = p.get("discard").and_then(Value::as_bool).unwrap_or(false);
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    if background && let Some(b) = s.preview_build.as_ref().filter(|b| b.running()) {
        // (the background preparation gives way, as to Build Previews)
        if b.background {
            b.cancel.store(true, Ordering::Relaxed);
        } else {
            return Err(bad(C, "a preview build is already running"));
        }
    }
    // (as Build Previews: the photos named, or the selected ones when more than one is, else
    // everything in view)
    let ids = if p.get("ids").is_some() || p.get("id").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
    let mut jobs: Vec<SmartJob> = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id).cloned() else { continue };
        if !matches!(ph.source, lightcraft_catalog::Source::File { .. }) {
            continue;
        }
        let path = crate::smart::sharded_file_path(&dir, &ph);
        let source = (!discard).then(|| s.media.source_ref(&ph, crate::media::SourceLevel::Preview));
        jobs.push((id, path, source, preview_fitted_look(&ph)));
    }
    let custom = s.smart_previews_dir.is_some();
    if background {
        let what = if discard { "discarding smart previews" } else { "smart previews" };
        let state = Arc::new(PreviewBuild { total: jobs.len(), what, ..Default::default() });
        s.preview_build = Some(state.clone());
        let task = task_for(s, &state);
        let st = state.clone();
        std::thread::Builder::new()
            .name("lc-smart-previews".into())
            .spawn(move || {
                if let Err(e) = smart_run(&dir, custom, discard, jobs, Some((&st, &task))) {
                    log::warn!("{C}: {e}");
                    *st.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                }
                st.finished.store(true, Ordering::Relaxed);
            })
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
        return Ok(state.json());
    }
    let r = smart_run(&dir, custom, discard, jobs, None).map_err(|e| bad(C, e))?;
    Ok(
        json!({"built": r.built, "repaired": r.repaired, "refreshed": r.refreshed, "staleKept": r.stale_kept, "removed": r.removed, "failed": r.failed}),
    )
}

/// Bring smart previews up to date, once per fit-and-encoding version: when the smart previews
/// folder has not been checked at this build's versions (its marker, [`crate::smart::look_checked`]),
/// a worker thread (the UI never waits on the drives) looks at the proxies of the photos on disk
/// and rebuilds the stale ones from their originals — one whose stored pixels are an older encoding
/// (every photo), or, for a raw that takes the per-file camera look or a DNG, one written by an older fit.
/// One whose original can't be read (offline) is kept as it is, still marked stale in its header:
/// Build Smart Previews rebuilds it once the original is back. Only existing proxies are touched:
/// none is built, and a missing folder is never created. Returns the thread, if started.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refresh_stale_smart_previews(s: &mut Session) -> Option<std::thread::JoinHandle<()>> {
    let dir = s.media.smart_dir.clone()?;
    if !dir.is_dir() || crate::smart::look_checked(&dir) {
        return None;
    }
    let max_edge = crate::media::SourceLevel::Preview.max_edge();
    let photos: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, lightcraft_catalog::Source::File { .. })).collect();
    let jobs: Vec<SmartJob> = photos
        .iter()
        .map(|p| (p.id, crate::smart::sharded_file_path(&dir, p), Some(s.media.origin_ref(&p.source, max_edge)), preview_fitted_look(p)))
        .collect();
    std::thread::Builder::new()
        .name("lc-smart-look".into())
        .spawn(move || refresh_stale(&dir, jobs))
        .map_err(|e| log::warn!("smart previews: could not start the look refresh: {e}"))
        .ok()
}

/// The body of [`refresh_stale_smart_previews`]: rebuild the stale proxies among `jobs`.
#[cfg(not(target_arch = "wasm32"))]
fn refresh_stale(dir: &std::path::Path, jobs: Vec<SmartJob>) {
    let stale: Vec<SmartJob> = jobs
        .into_iter()
        .map(|(id, path, source, look)| (id, crate::smart::existing_file_path(dir, path), source, look))
        .filter(|(_, path, _, look)| crate::smart::is_stale_for(path, *look))
        .collect();
    if !stale.is_empty() {
        // `custom`: the folder is there already, and must not be recreated if it went away since
        match smart_run(dir, true, false, stale, None) {
            Ok(n) => log::info!("smart previews: {} refreshed to the current camera look, {} kept (original offline)", n.refreshed, n.stale_kept),
            Err(e) => {
                // not marked: the next opening tries again
                log::warn!("smart previews: look refresh: {e}");
                return;
            }
        }
    }
    if let Err(e) = crate::smart::mark_look_checked(dir) {
        log::warn!("smart previews: {e}");
    }
}

/// The smart previews folder: where it is, whether it is custom, what it holds, whether it can be
/// used now (a chosen folder on an unplugged drive is `available: false`).
#[cfg(not(target_arch = "wasm32"))]
fn location_json(s: &Session) -> Value {
    let dir = s.media.smart_dir.clone();
    let default = s.library.as_ref().filter(|l| l.on_disk).map(|l| crate::smart::dir(&l.dir));
    let (count, bytes) = dir.as_deref().map_or((0, 0), crate::smart::stats);
    json!({
        "path": dir.as_ref().map(|d| d.to_string_lossy()),
        "default": default.map(|d| d.to_string_lossy().to_string()),
        "custom": s.smart_previews_dir.is_some(),
        "available": dir.as_ref().is_some_and(|d| d.is_dir()) || s.smart_previews_dir.is_none(),
        "count": count,
        "bytes": bytes,
    })
}

/// Show or change where smart previews are kept (per library).
#[cfg(not(target_arch = "wasm32"))]
fn smart_location(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviewsLocation";
    let current = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let chosen = str_param(p, "path").filter(|x| !x.trim().is_empty());
    if !reset && chosen.is_none() {
        return Ok(location_json(s));
    }
    let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let default = crate::smart::dir(&lib_dir);
    let (new_dir, custom) = match chosen {
        Some(x) if !reset => {
            let d = std::path::PathBuf::from(x.trim());
            if !d.is_absolute() {
                return Err(bad(C, "path must be an absolute folder path"));
            }
            (d.clone(), (d != default).then_some(d))
        }
        _ => (default, None),
    };
    if new_dir == current {
        return Ok(location_json(s));
    }
    // the new place must work before anything moves; there is no fallback to another drive
    crate::smart::check_writable(&new_dir).map_err(|e| bad(C, e))?;
    let (held, _) = crate::smart::stats(&current);
    let existing = match str_param(p, "existing") {
        Some(x) => Some(crate::smart::Existing::parse(x).ok_or_else(|| bad(C, "existing must be move, leave or discard"))?),
        None if held == 0 => Some(crate::smart::Existing::Leave),
        None => None,
    };
    let Some(existing) = existing else {
        return Err(bad(C, format!("{held} smart preview(s) are in {}: pass existing = move, leave or discard", current.display())));
    };
    let (moved, failed) = crate::smart::migrate(&current, &new_dir, existing);
    s.smart_previews_dir = custom;
    s.media.smart_dir = Some(new_dir);
    s.save_prefs()?;
    let mut r = location_json(s);
    r["handled"] = json!(moved);
    r["failed"] = json!(failed);
    Ok(r)
}

/// Whether photo `id` has a smart preview.
pub fn has_smart_preview(s: &Session, id: lightcraft_catalog::PhotoId) -> bool {
    match (&s.media.smart_dir, s.catalog.photo(id)) {
        (Some(dir), Some(p)) => crate::smart::is_valid(&crate::smart::file_path(dir, p)),
        _ => false,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviews",
            "Build Smart Previews",
            [],
            None,
            "{ids?, discard?: bool, background?: bool} — build (or discard) the smart previews of the selected photos (when more than one is selected; else all in view): compact proxies in the library that keep photos editable and exportable (at proxy size) while their originals are offline; a damaged proxy (cut short) is rebuilt → {built, repaired, removed, failed}; with background (the app's menu) the files are read and written on a worker thread → {total, done, failed, repaired, running, what} like library.buildPreviews (progress: library.previewProgress, stop: library.cancelPreviews)",
            always,
            smart
        ),
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviewsLocation",
            "Smart Previews Location",
            [],
            None,
            "{path?: absolute folder, reset?: bool, existing?: move|leave|discard} — without path/reset: where this library keeps its smart previews → {path, default, custom, available, count, bytes}. With path (or reset = back to `Smart Previews` in the library): use that folder instead (must exist or have an existing parent, and accept writes; never falls back to another drive). Smart previews already in the old folder need `existing`: move them, leave them (build again), or discard them → also {handled, failed}",
            always,
            smart_location
        ),
        cmd!(query "photo.smartPreview", "Smart Preview Status", [], None, "{id?} → {smartPreview: bool, originalOnline: bool}", always, |s, p| {
            let id = s.targets(p)?.first().copied().ok_or_else(|| bad("photo.smartPreview", "no photo"))?;
            let online = match s.catalog.photo(id).map(|p| p.source.clone()) {
                Some(lightcraft_catalog::Source::File { path }) => std::path::Path::new(&path).exists(),
                _ => true,
            };
            Ok(json!({"smartPreview": has_smart_preview(s, id), "originalOnline": online}))
        }),
        cmd!(
            "library.buildPreviews",
            "Build Previews",
            [],
            None,
            "{size?: standard (2048 px, or the loupe's size when bigger, or `edge`) | full (1:1), ids?, wait?: bool, force?: bool, from?: id, backward?: bool, background?: bool, row?: bool} — render the grid thumbnail and loupe view of the selected photos (when more than one is selected; else all in view) into the preview cache (memory and disk), in the background unless `wait`. Photos whose previews are there already (big enough) are skipped unless `force`; the photos nearest `from` go first. `background`: the background preparation's build (two lanes, waits while the photo on screen renders; a build asked for replaces it). `row: false`: no row of its own in the activity stack (the caller's row shows it) → {total, done, failed, skipped, shared, running}",
            always,
            build
        ),
        cmd!(query "library.previewProgress", "Preview Build Progress", [], None, "{} → {total, done, failed, repaired, skipped, shared, background, running, cancelled, what, error, renders: {interactive, background, interactiveGpu, backgroundGpu} (the process's renders so far: for what is on screen, and work done ahead such as this build, which goes to the second graphics card first)} | null", always, |s, _| {
            Ok(s.preview_build.as_ref().map_or(Value::Null, |b| b.json()))
        }),
        cmd!(query "prefs.backgroundPrep", "Background Preparation Settings", [], None, "{} → {app, library|null, settings (in use), own: bool (this library keeps its own), building} — what is prepared in the background per mode (culling | library: previews of every photo in view, ahead = photos ahead rendered for the loupe, eyeAhead = photos ahead whose eye focus strip is worked out, scope all|marked|onScreen; develop: ahead = photos kept decoded ready to edit, scope)", always, |s, _| {
            Ok(s.prep_json())
        }),
        cmd!(
            "prefs.setBackgroundPrep",
            "Set Background Preparation",
            [],
            None,
            "{settings?: partial {culling?, library?, develop?}, library?: bool (this library's own settings; else the app-wide ones), reset?: bool (this library goes back to the app-wide settings)} → as prefs.backgroundPrep",
            always,
            |s, p| {
                const C: &str = "prefs.setBackgroundPrep";
                let library = p.get("library").and_then(Value::as_bool).unwrap_or(false);
                if p.get("reset").and_then(Value::as_bool).unwrap_or(false) {
                    s.clear_library_prep()?;
                    return Ok(s.prep_json());
                }
                if library && s.library.is_none() {
                    return Err(bad(C, "no library is open"));
                }
                let base = if library { s.prep.library.unwrap_or(s.prep.app) } else { s.prep.app };
                let next = match p.get("settings") {
                    Some(patch) if patch.is_object() => base.patched(patch).map_err(|e| bad(C, e))?,
                    Some(_) => return Err(bad(C, "settings must be an object")),
                    None => base,
                };
                s.set_prep_settings(next, library)?;
                Ok(s.prep_json())
            }
        ),
        cmd!("library.cancelPreviews", "Cancel Preview Build", [], None, "{}", always, |s, _| {
            if let Some(b) = &s.preview_build {
                b.cancel.store(true, Ordering::Relaxed);
            }
            Ok(s.preview_build.as_ref().map_or(Value::Null, |b| b.json()))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{jobs_for, run_photo};

    use crate::Session;
    use crate::media::SourceRef;

    // 1:1 previews run two photos per graphics card (two more feed a second card), never more than
    // the memory-bound count; standard previews keep the export's count
    #[test]
    fn full_size_previews_run_two_photos_per_graphics_card() {
        let hint = lightcraft_gpu::batch_lanes_hint(2);
        assert_eq!(hint, 2 * lightcraft_gpu::batch_cards().max(1));
        assert_eq!(super::lanes(64, true), crate::export::export_parallelism(64).min(hint));
        assert_eq!(super::lanes(64, false), crate::export::export_parallelism(64));
        assert!(super::lanes(1, true) >= 1);
    }

    // Given a preview-only raw, its proxy goes stale with the look version: the embedded JPEG's colour
    // space can change with it (#2654); a rendered photo's proxy does not
    #[test]
    fn preview_only_raws_follow_the_look_version() {
        use lightcraft_catalog::{MediaKind, Photo, PhotoId, Source};
        let mut ph = Photo::new(PhotoId(1), Source::File { path: "/x.cr3".into() }, "x.cr3", "CR3", 6000, 4000, "2026-10-23T00:00:00");
        ph.kind = MediaKind::Raw;
        assert!(super::preview_fitted_look(&ph), "a developed CR3");
        ph.set_preview_only(Some("unsupported".into()));
        assert!(super::preview_fitted_look(&ph), "a preview-only CR3");
        ph.kind = MediaKind::Image;
        ph.format = "JPG".into();
        ph.set_preview_only(None);
        assert!(!super::preview_fitted_look(&ph), "a JPEG");
    }

    // Given a photo's preview jobs, the grid thumbnail is made from the source the view decoded, not
    // by reading the file again (here: a thumbnail job pointing at a file that is not there)
    #[test]
    fn preview_build_makes_the_thumbnail_from_the_views_source() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let mut jobs = jobs_for(&mut s, id, Some(512));
        assert_eq!(jobs.len(), 2, "view and thumbnail");
        assert!(jobs[0].view_cache.is_some(), "the view renders first");
        jobs[1].source =
            SourceRef::File { path: "/nonexistent/photo.arw".into(), max_edge: 512, loader: None, fallback: None, denoise: None, reduced: None };
        assert!(run_photo(jobs), "the thumbnail rendered from the view's decoded source");
    }

    // Given many photos, a standard preview build renders them side by side and counts every one
    #[test]
    fn preview_build_of_many_photos_counts_each_once() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
        assert!(ids.len() > 4);
        let r = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids, "wait": true})).unwrap();
        assert_eq!(r["done"], json!(ids.len()), "{r}");
        assert_eq!(r["total"], json!(ids.len()));
        assert_eq!(r["failed"], json!(0));
    }

    // Given a photo, its preview build renders as background work (the second graphics card's,
    // design §8 of 2026-10-31) and its loupe render as interactive work (the screen's card only)
    #[test]
    fn preview_builds_are_background_work_and_the_loupe_is_not() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let jobs = jobs_for(&mut s, id, Some(256));
        let n = jobs.len() as u64;
        let c = crate::media::thread_render_counts();
        let (i0, b0) = (c[0], c[1]);
        assert!(run_photo(jobs));
        let c = crate::media::thread_render_counts();
        let (i1, b1) = (c[0], c[1]);
        assert_eq!((i1 - i0, b1 - b0), (0, n), "every render of the build is background work");
        assert!(!crate::media::rendering_in_background(), "the thread is back to interactive work");
        // (not served from the preview just built)
        let mut loupe = s.loupe_job(id, 256, 256, true).unwrap();
        (loupe.view_cache, loupe.cache) = (None, None);
        loupe.run().rendered.unwrap();
        let c = crate::media::thread_render_counts();
        let (i2, b2) = (c[0], c[1]);
        assert_eq!(c[4], 0, "nothing on screen renders on the second card");
        assert_eq!((i2 - i1, b2 - b1), (1, 0), "the loupe's render is interactive");
        let counts = s.execute("library.buildPreviews", &json!({"ids": [id.0], "edge": 256, "wait": true, "force": true})).unwrap();
        assert!(counts["renders"]["background"].as_u64().unwrap() >= n, "{counts}");
    }

    // Given a panic inside background work, it is that work's error and the thread's renders
    // are interactive again (a loupe render on it must never count as, or route to, background work)
    #[test]
    fn background_work_that_panics_leaves_the_thread_interactive() {
        let r: Result<(), String> = crate::media::background_gpu("a test", || panic!("boom"));
        assert!(r.unwrap_err().contains("a test"));
        assert!(!crate::media::rendering_in_background());
    }

    // Given a photo, the view preview a build puts in the cache (background work: on the second
    // graphics card when there is one) equals the loupe's own render of it (the screen's card)
    // within 1 LSB (design §7.6), so a built preview never shows a different photo
    #[test]
    fn a_built_preview_equals_the_loupes_render() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let jobs = jobs_for(&mut s, id, Some(512));
        let (cache, key) = jobs[0].view_cache.clone().expect("the view job keeps its render");
        assert!(run_photo(jobs));
        let built = cache.get(key).expect("the build kept the view preview");
        let mut job = s.loupe_job(id, 512, 512, true).unwrap();
        (job.view_cache, job.cache) = (None, None);
        let r = job.run();
        assert!(r.quick.is_none(), "rendered, not the built preview served again");
        let shown = r.rendered.unwrap().image;
        assert_eq!((built.width, built.height), (shown.width, shown.height));
        let worst = built.data.iter().zip(&shown.data).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y))).max().unwrap_or(0);
        println!("built preview vs loupe render: max |Δ| = {worst} LSB");
        assert!(worst <= 1, "max |Δ| = {worst} LSB");
    }
}
