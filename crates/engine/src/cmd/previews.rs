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

/// When 1:1 previews are discarded on their own (the catalog setting "Automatically discard 1:1
/// previews"): this long after they were built or last shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscardAfter {
    Day,
    Week,
    #[default]
    Month,
    Never,
}

impl DiscardAfter {
    pub fn parse(s: &str) -> Option<DiscardAfter> {
        Some(match s {
            "day" => DiscardAfter::Day,
            "week" => DiscardAfter::Week,
            "month" => DiscardAfter::Month,
            "never" => DiscardAfter::Never,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DiscardAfter::Day => "day",
            DiscardAfter::Week => "week",
            DiscardAfter::Month => "month",
            DiscardAfter::Never => "never",
        }
    }

    /// The age past which a 1:1 preview goes (`None`: never).
    pub fn max_age(self) -> Option<std::time::Duration> {
        let day = 24 * 3600;
        match self {
            DiscardAfter::Day => Some(std::time::Duration::from_secs(day)),
            DiscardAfter::Week => Some(std::time::Duration::from_secs(7 * day)),
            DiscardAfter::Month => Some(std::time::Duration::from_secs(30 * day)),
            DiscardAfter::Never => None,
        }
    }
}

/// Which previews an import builds once its photos are in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportPreviews {
    /// Thumbnails only, as the grid asks for them.
    #[default]
    Minimal,
    Standard,
    Full,
}

impl ImportPreviews {
    pub fn parse(s: &str) -> Option<ImportPreviews> {
        Some(match s {
            "minimal" => ImportPreviews::Minimal,
            "standard" => ImportPreviews::Standard,
            "full" | "1:1" => ImportPreviews::Full,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ImportPreviews::Minimal => "minimal",
            ImportPreviews::Standard => "standard",
            ImportPreviews::Full => "full",
        }
    }
}

/// The preview store's settings, per library (saved in its `prefs.json`).
// TODO(P1.5): move into the catalog's own settings once its storage rework lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PreviewPrefs {
    /// Long edge of standard-sized previews (0 = [`STANDARD_EDGE`]).
    pub standard_edge: u32,
    pub discard_full: DiscardAfter,
    pub at_import: ImportPreviews,
}

impl PreviewPrefs {
    pub fn standard_edge(&self) -> usize {
        if self.standard_edge == 0 { STANDARD_EDGE } else { (self.standard_edge as usize).clamp(256, 8192) }
    }

    pub fn json(&self) -> Value {
        json!({
            "standardEdge": self.standard_edge(),
            "discardFull": self.discard_full.as_str(),
            "atImport": self.at_import.as_str(),
        })
    }
}

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
    /// prepared ahead): counted as done, not rendered again.
    pub skipped: AtomicUsize,
    /// Photos left alone because another build was rendering them at that moment.
    pub shared: AtomicUsize,
    /// Started by the background preparation ([`crate::prep`]): it yields to the photo on screen
    /// and to the user's own work, and a build the user (or an import) asks for replaces it.
    pub background: bool,
    /// The photos in build order (ids), and where the user is among them: the next photo built
    /// is the nearest one not built yet from there, in the direction of travel
    /// ([`PreviewBuild::focus_on`]).
    pub order: Vec<u64>,
    pub focus: AtomicUsize,
    pub backward: AtomicBool,
    /// Each photo of `order`'s render jobs, made as the build needs them ([`feed`]).
    pub(crate) queue: Queue,
    /// The view edge the jobs are made for (`None` = 1:1).
    pub(crate) edge: Option<usize>,
}

/// One photo's place in a build's queue: `None` = taken (built, skipped or being built), `Some(None)`
/// = its jobs not made yet ([`feed`] makes them on the UI thread), `Some(Some(jobs))` = ready.
type Slot = Option<Option<Vec<RenderJob>>>;

/// A build's queue, in `order`.
#[derive(Default)]
pub(crate) struct Queue(std::sync::Mutex<Vec<Slot>>);

impl std::fmt::Debug for Queue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Queue({} photos)", self.lock().len())
    }
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
impl PreviewBuild {
    /// How many photos' jobs aren't made yet.
    pub(crate) fn queue_unmade(&self) -> usize {
        self.queue.lock().iter().filter(|s| matches!(s, Some(None))).count()
    }

    /// Whether photo `i` of `order`'s jobs aren't made yet.
    pub(crate) fn queue_unmade_at(&self, i: usize) -> bool {
        self.queue.lock().get(i).is_some_and(|s| matches!(s, Some(None)))
    }
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
        })
    }

    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    /// The user is at photo `id`, moving `backward` or forward: build from there on.
    pub fn focus_on(&self, id: lcc_catalog::PhotoId, backward: bool) {
        if let Some(i) = self.order.iter().position(|p| *p == id.0) {
            self.focus.store(i, Ordering::Relaxed);
            self.backward.store(backward, Ordering::Relaxed);
        }
    }

    /// Index of the next photo to build in `queue` (`None`: none left): the first one still there
    /// from the focus on in the direction of travel, then the rest the other way. The background
    /// preparation starts one past the focus: the photo on screen is the loupe's own render, and
    /// building it as well decoded the same raw twice while the user stepped (2026-11-09).
    pub(crate) fn next_index<T>(&self, queue: &[Option<T>]) -> Option<usize> {
        let n = queue.len();
        let at = self.focus.load(Ordering::Relaxed).min(n.saturating_sub(1));
        let there = |i: &usize| queue.get(*i).is_some_and(Option::is_some);
        let skip = usize::from(self.background);
        if self.backward.load(Ordering::Relaxed) {
            // (the photos before the focus, nearest first; then the focus and the rest after it)
            let ahead = at.checked_sub(skip).and_then(|from| (0..=from).rev().find(there));
            ahead.or_else(|| (at.saturating_add(1).saturating_sub(skip)..n).find(there))
        } else {
            let from = at.saturating_add(skip).min(n);
            (from..n).find(there).or_else(|| (0..from).rev().find(there))
        }
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// A preview build asked for while one was already running, waiting for its turn in
/// [`Session::preview_queue`]: the command to run again and its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedPreview {
    /// `library.buildPreviews` or `library.smartPreviews`.
    pub command: &'static str,
    pub params: Value,
}

/// How many preview builds may wait for the running one (a runaway script asks forever; a person
/// cannot get past a handful).
const QUEUE_MAX: usize = 16;

/// Queue request `p` of `command` behind the running build ([`Session::preview_queue`]): an
/// identical request already waiting is not queued twice. → its place in the queue.
fn enqueue(s: &mut Session, c: &str, command: &'static str, p: &Value) -> Result<Value> {
    // a queued build runs when its turn comes, on the frontend that drains the queue — never as
    // a blocking `wait` run, which would hold that whole frame (the queue's answer is immediate;
    // progress comes from `library.previewProgress`, as any background build's)
    let mut params = p.clone();
    if let Some(o) = params.as_object_mut() {
        o.remove("wait");
    }
    if let Some(place) = s.preview_queue.iter().position(|q| q.command == command && q.params == params) {
        return Ok(json!({"queued": true, "position": place + 1}));
    }
    if s.preview_queue.len() >= QUEUE_MAX {
        return Err(bad(c, format!("{} preview builds are already waiting; stop the running one first", QUEUE_MAX)));
    }
    s.preview_queue.push_back(QueuedPreview { command, params });
    Ok(json!({"queued": true, "position": s.preview_queue.len()}))
}

/// Start the next queued preview build when no build that was asked for is running
/// ([`Session::preview_queue`]); the frontends call this every frame. The background
/// preparation's build gives way to the queued one, as it would to a build asked for from the
/// menu. An entry that no longer can start (its smart previews folder went away, say) is dropped
/// with its reason in the log, and the one after it starts on the next call. `None`: nothing was
/// due.
pub fn drain_queue(s: &mut Session) -> Option<Result<Value>> {
    if s.preview_build.as_ref().is_some_and(|b| b.running() && !b.background) || s.preview_queue.is_empty() {
        return None;
    }
    let next = s.preview_queue.pop_front()?;
    let r = s.execute(next.command, &next.params);
    if let Err(e) = &r {
        log::warn!("queued {}: {e}", next.command);
    }
    Some(r)
}

/// The jobs that build `id`'s previews: the largest grid thumbnail and the view render at `edge`
/// (`None` = 1:1, the photo's full size), the larger first.
pub(crate) fn jobs_for(s: &mut Session, id: lcc_catalog::PhotoId, edge: Option<usize>) -> Vec<RenderJob> {
    let Some(p) = s.catalog.photo(id).cloned() else { return Vec::new() };
    let full = p.width.max(p.height) as usize;
    let e = edge.unwrap_or(full).min(full.max(1)).max(1);
    let thumb = THUMB_SIZES[THUMB_SIZES.len() - 1];
    let (mut view, grid) = (s.loupe_job(id, e, e, true), s.thumb_job(id, thumb));
    // a 1:1 preview is kept under its own key, to be discarded on its own
    if edge.is_none()
        && let Some(j) = view.as_mut()
        && let Some((_, key)) = j.view_cache.as_mut()
    {
        *key = Session::full_view_key(&p);
    }
    // the larger render goes first: the one after it renders from the source it loaded (see
    // `run_photo`), which must not be smaller than what it needs
    if e >= thumb { view.into_iter().chain(grid).collect() } else { grid.into_iter().chain(view).collect() }
}

/// The previews the build of one photo would make are all there already, big enough: its view
/// render at least as large as asked for (`view_cache` of the view job) and its grid thumbnail
/// (`cache` of the thumbnail job). Only file headers are read.
pub(crate) fn already_built(jobs: &[RenderJob]) -> bool {
    !jobs.is_empty()
        && jobs.iter().all(|j| {
            if let Some((cache, key)) = &j.view_cache {
                let want = j.request.max_w.max(j.request.max_h) as f64;
                cache.dims(*key).is_some_and(|(w, h)| w.max(h) as f64 >= want * 0.99)
            } else if let Some((cache, key)) = &j.cache {
                cache.dims(*key).is_some()
            } else {
                false
            }
        })
}

/// Photos some preview build is rendering right now, by (the cache they go to, photo id): two
/// builds (a background one being replaced, and the one replacing it) never render the same photo
/// at once.
static BUILDING: std::sync::Mutex<Vec<(usize, u64)>> = std::sync::Mutex::new(Vec::new());

/// Whether a preview build is rendering `id`'s previews into `s`'s preview cache right now (it holds
/// the photo in [`BUILDING`] until its view render is in the cache). The loupe then waits for that
/// render instead of decoding the same raw a second time beside it ([`Session::being_prepared`]).
pub fn being_built(s: &Session, id: lcc_catalog::PhotoId) -> bool {
    let cache = Arc::as_ptr(&s.media.rendered) as usize;
    BUILDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&(cache, id.0))
}

/// Hold `id` as being built into `s`'s preview cache until the result is dropped, as a build does
/// while it renders the photo: for frontends' tests of [`being_built`]. `None`: a build holds it already.
#[doc(hidden)]
pub fn hold_as_building(s: &Session, id: lcc_catalog::PhotoId) -> Option<Building> {
    let key = (Arc::as_ptr(&s.media.rendered) as usize, id.0);
    let mut b = BUILDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if b.contains(&key) {
        return None;
    }
    b.push(key);
    Some(Building(key))
}

/// Holds a photo in [`BUILDING`] while it is rendered.
pub struct Building((usize, u64));

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
///
/// Previews are made ahead, never for the photo on screen: batch work, on whichever graphics card
/// has room ([`crate::media::background_photo`]: with a second card both cards get photos; the
/// editing card only while the interface is idle). The source is decoded first, outside the
/// photo's turn at a card (a raw's decode is a second or more of processor work; the card's turn
/// is for the renders).
pub(crate) fn run_photo(mut jobs: Vec<RenderJob>) -> bool {
    let Some(first) = jobs.first() else { return true };
    let decoded = match &first.source {
        crate::media::SourceRef::Loaded(d) => (**d).clone(),
        // (an unreadable photo fails here, as its jobs would)
        source => match source.load_source_of(first.source_key) {
            Ok(d) => d,
            Err(e) => {
                log::info!("building previews: {e}");
                return false;
            }
        },
    };
    let src_px = decoded.image.width.saturating_mul(decoded.image.height);
    let out_px = jobs.iter().map(|j| j.request.max_w.saturating_mul(j.request.max_h)).max().unwrap_or(0).min(src_px.max(1));
    for j in &mut jobs {
        j.source = crate::media::SourceRef::Loaded(Box::new(decoded.clone()));
    }
    crate::media::background_photo("building previews", src_px, out_px, || run_photo_jobs(jobs)).unwrap_or(false)
}

/// [`run_photo`]'s renders, inside the photo's turn.
fn run_photo_jobs(jobs: Vec<RenderJob>) -> bool {
    let mut decoded: Option<crate::media::DecodedSource> = None;
    let mut ok = true;
    for mut j in jobs {
        if let Some(src) = &decoded
            && !matches!(j.source, crate::media::SourceRef::Loaded(_))
        {
            j.source = crate::media::SourceRef::Loaded(Box::new(src.clone()));
        }
        // The view render is written to the disk cache here, before the photo counts as built,
        // not left to the cache's deferred writer: a built preview is on disk, for this session
        // and the next. (Taken out of the job: it would only queue the write, and would serve a
        // smaller cached view.)
        let keep = j.view_cache.take().map(|v| (v, j.cache_generation));
        // (and the photo's latest view / thumbnail, on disk before the photo counts as built: the
        // picture an edited photo shows once its settings change again)
        let latest = j.latest.take();
        let thumb = j.cache.as_ref().map(|c| c.1);
        let generation = j.cache_generation;
        let r = j.run();
        if let Ok(img) = r.rendered.as_ref() {
            let img = Arc::new(img.image.clone());
            let real = match keep {
                Some(((cache, key), generation)) => {
                    cache.put_at(generation, key, img.clone());
                    Some(key)
                }
                None => thumb,
            };
            if let (Some((cache, alias)), Some(real)) = (latest, real)
                && r.quick != Some(crate::media::QuickSource::Cached)
            {
                cache.put_latest_with(generation, alias, real, img, true, crate::media::placement_bytes(r.placement.as_ref()).as_deref());
            }
        }
        ok &= r.rendered.is_ok();
        if decoded.is_none() {
            decoded = r.loaded;
        }
    }
    ok
}

/// How many photos render side by side: as many as an export batch for standard previews (each
/// render spreads over the cores already; the others fill them while one decodes), two per
/// graphics card for 1:1 previews (a full-size source each; one card needs two to stay busy, and
/// with a second card two more feed it), within the export's memory-bound count.
fn lanes(n: usize, full: bool) -> usize {
    let l = crate::export::export_parallelism(n);
    if full { l.min(crate::gpu::batch_lanes_hint(2)) } else { l }
}

/// Build every photo of `jobs` on `lanes` threads, nearest the build's focus first. `force`:
/// render even the photos whose previews are there. `activity`: what the background preparation
/// gives way to (the user's own work).
fn run_all(state: &PreviewBuild, task: &TaskGuard, lanes: usize, force: bool, activity: &crate::activity::Activity) {
    let total = state.total;
    let queue = &state.queue;
    // `steady`: the lane that keeps building while the user steps ([`crate::prep::STEADY_WAIT_MS`])
    let work = |steady: bool| {
        loop {
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            // the background preparation pauses completely while the user's own work runs (an
            // export, an import, a batch), and waits while the photo on screen is being made
            // (a lane whose photos were all taken meanwhile stops waiting: the build is done once
            // the last photo is, not once the user stops stepping)
            let left = || queue.lock().iter().any(Option::is_some);
            let wait_on_screen = |c: &AtomicBool| {
                if steady { crate::prep::wait_for_foreground_steady(c) } else { crate::prep::wait_for_foreground_while(c, &left) }
            };
            if state.background && (wait_for_user_work(activity, &state.cancel, task) || wait_on_screen(&state.cancel)) {
                break;
            }
            let next = {
                let mut q = queue.lock();
                match state.next_index(&q) {
                    None => None,
                    // the nearest photo's jobs aren't made yet: [`feed`] makes them next frame
                    Some(i) if q.get(i).is_some_and(|s| matches!(s, Some(None))) => Some(None),
                    Some(i) => {
                        let id = state.order.get(i).copied().unwrap_or(0);
                        Some(q.get_mut(i).and_then(Option::take).flatten().map(|j| (id, j)))
                    }
                }
            };
            let Some(next) = next else { break };
            let Some((id, photo_jobs)) = next else {
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_millis(FEED_WAIT_MS));
                continue;
            };
            if !force && already_built(&photo_jobs) {
                state.skipped.fetch_add(1, Ordering::Relaxed);
            } else if let Some(_held) = Building::take(id, &photo_jobs) {
                // the background preparation's decodes wait at the memory gate behind the photo on
                // screen's, as the other background work does (left out of the port: with a lane
                // that keeps building while the user steps, the photo on screen waited behind it)
                let build = || run_photo(photo_jobs);
                let run = || if state.background { crate::prep::on_background_pool(|| crate::memory::in_background(build)) } else { build() };
                let ok = crate::guard::catch("building previews", run).unwrap_or(false);
                if !ok {
                    state.failed.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                state.shared.fetch_add(1, Ordering::Relaxed);
            }
            let done = state.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            task.progress(done as u64, total as u64);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        let work = &work;
        for k in 1..lanes.max(1) {
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("previews-{k}")).spawn_scoped(sc, move || work(false)).is_err() {
                break;
            }
        }
        work(true);
    });
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lanes;
        work(true);
    }
    state.finished.store(true, Ordering::Relaxed);
    // the build's decode temporaries are gone: hand their pages back now, not at the next idle trim
    crate::memory::release();
}

/// Wait while the user's own work runs ([`crate::activity::Activity::user_work`]), saying so in the
/// build's row. `true`: cancelled meanwhile.
fn wait_for_user_work(activity: &crate::activity::Activity, cancel: &AtomicBool, task: &TaskGuard) -> bool {
    let mut paused = false;
    while let Some(kind) = activity.user_work() {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        if !paused {
            paused = true;
            task.detail(match kind {
                "export" => "Paused while exporting",
                "import" => "Paused while importing",
                _ => "Paused while your other work runs",
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(100));
        #[cfg(target_arch = "wasm32")]
        break;
    }
    if paused {
        task.detail("");
    }
    cancel.load(Ordering::Relaxed)
}

/// The long edge the background preparation builds standard previews at: the library's standard
/// size, or what the loupe asks for on this screen when that is more ([`Session::loupe_edge`]), so
/// the loupe finds what was built.
pub fn standard_edge(s: &Session) -> usize {
    s.preview_prefs.standard_edge().max(s.loupe_edge.min(8192))
}

fn build(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.buildPreviews";
    // (photos added since the library opened: the automatic disk budget makes room for them first)
    s.fit_disk_cache();
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    let force = p.get("force").and_then(Value::as_bool).unwrap_or(false);
    let standard = if background { standard_edge(s) } else { s.preview_prefs.standard_edge() };
    let edge = match str_param(p, "size").unwrap_or("standard") {
        "standard" => Some(p.get("edge").and_then(Value::as_u64).map_or(standard, |e| e.clamp(256, 8192) as usize)),
        "full" | "1:1" => None,
        other => return Err(bad(C, format!("unknown size `{other}` (standard|full)"))),
    };
    if let Some(b) = s.preview_build.as_ref().filter(|b| b.running()) {
        // the background preparation gives way to a build that was asked for; a build asked for
        // while another runs waits for its turn (the menu's builds queue up), and nothing
        // replaces one that was asked for (it carries on, and the photos it built are skipped by
        // the next)
        if b.background && !background {
            b.cancel.store(true, Ordering::Relaxed);
        } else if background {
            return Err(bad(C, "a preview build is already running"));
        } else {
            return enqueue(s, C, "library.buildPreviews", p);
        }
    }
    // 1:1 previews past their time go first (the build may need the room). The background
    // preparation starts a build at every folder and stretch the user moves to: it looks at the
    // dates on disk at most every few minutes (110 ms on the UI thread for 2,597 photos)
    if !background || discard_due() {
        auto_discard(s);
    }
    // explicit ids, else the selection, else everything in view
    let ids = if p.get("ids").is_some() || p.get("id").is_some() || !s.selection.ids.is_empty() { s.targets(p)? } else { s.visible_cloned() };
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32");
    // `feed`: the caller makes the jobs a few photos a frame ([`feed`]), nearest the user first;
    // making all of them here froze the UI for 1.3 s at every build of a 2,597-photo folder (2026-11-10)
    let lazy = background && !wait && p.get("feed").and_then(Value::as_bool).unwrap_or(false);
    let (order, slots): (Vec<u64>, Vec<Slot>) = if lazy {
        (ids.iter().map(|id| id.0).collect(), ids.iter().map(|_| Some(None)).collect())
    } else {
        ids.iter().map(|id| (id.0, jobs_for(s, *id, edge))).filter(|j| !j.1.is_empty()).map(|(id, j)| (id, Some(Some(j)))).unzip()
    };
    let state =
        Arc::new(PreviewBuild { total: order.len(), background, order, edge, queue: Queue(std::sync::Mutex::new(slots)), ..Default::default() });
    if let Some(at) = p.get("from").and_then(Value::as_u64) {
        state.focus_on(lcc_catalog::PhotoId(at), p.get("backward").and_then(Value::as_bool).unwrap_or(false));
    }
    // (the first photos' jobs at once: the lanes start on them without waiting for a frame)
    if lazy {
        feed_build(s, &state, FIRST_FEED);
    }
    s.preview_build = Some(state.clone());
    let task = task_for(s, &state);
    // the background preparation leaves most of the machine to the photo on screen and the user
    let lanes = if background { crate::prep::background_lanes() } else { lanes(state.total, edge.is_none()) };
    if wait {
        run_all(&state, &task, lanes, force, &s.activity);
        return Ok(state.json());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row stays while the worker runs (it is dropped with the closure, however that ends)
        let st = state.clone();
        let activity = s.activity.clone();
        std::thread::Builder::new()
            .name("lc-build-previews".into())
            .spawn(move || run_all(&st, &task, lanes, force, &activity))
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
    }
    Ok(state.json())
}

/// How long a lane waits for [`feed`] to make the nearest photo's jobs before it looks again.
#[cfg(not(target_arch = "wasm32"))]
const FEED_WAIT_MS: u64 = 15;

/// How many photos' jobs a build that is fed makes when it starts ([`feed`]).
const FIRST_FEED: usize = 8;

/// How long [`feed`] makes jobs in one frame (about half a millisecond a photo).
const FEED_BUDGET_MS: f64 = 4.0;

/// How often a background build looks at the 1:1 previews' ages ([`auto_discard`]).
const DISCARD_EVERY_S: u64 = 300;

/// Whether the background preparation's build should apply [`auto_discard`]: not in the last
/// [`DISCARD_EVERY_S`] seconds.
fn discard_due() -> bool {
    static LAST: std::sync::Mutex<Option<web_time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let due = last.is_none_or(|t| t.elapsed().as_secs() >= DISCARD_EVERY_S);
    if due {
        *last = Some(web_time::Instant::now());
    }
    due
}

/// Make the jobs of the running build's photos that have none yet, nearest the user first, for
/// about [`FEED_BUDGET_MS`]: the background preparation calls this every frame
/// ([`crate::Session::prep_tick`]). `true`: photos are still waiting for theirs (call again soon).
pub fn feed(s: &mut Session) -> bool {
    let Some(b) = s.preview_build.clone().filter(|b| b.running() && !b.cancel.load(Ordering::Relaxed)) else { return false };
    let t0 = web_time::Instant::now();
    let mut more = true;
    while more && t0.elapsed().as_secs_f64() * 1e3 < FEED_BUDGET_MS {
        more = feed_build(s, &b, 4);
    }
    more
}

/// Make up to `n` photos' jobs of `b`, nearest its focus first. `true`: some are still unmade.
fn feed_build(s: &mut Session, b: &PreviewBuild, n: usize) -> bool {
    for _ in 0..n {
        // (the lock is not held while the jobs are made: the lanes take photos meanwhile)
        let next = {
            let q = b.queue.lock();
            let unmade: Vec<Option<()>> = q.iter().map(|s| matches!(s, Some(None)).then_some(())).collect();
            b.next_index(&unmade)
        };
        let Some(i) = next else { return false };
        let Some(id) = b.order.get(i).copied() else { return false };
        let jobs = jobs_for(s, lcc_catalog::PhotoId(id), b.edge);
        let mut q = b.queue.lock();
        if let Some(slot) = q.get_mut(i).filter(|s| matches!(s, Some(None))) {
            if jobs.is_empty() {
                // (nothing to build: counted as done, like a photo whose previews are there)
                *slot = None;
                b.skipped.fetch_add(1, Ordering::Relaxed);
                b.done.fetch_add(1, Ordering::Relaxed);
            } else {
                *slot = Some(Some(jobs));
            }
        }
    }
    b.queue.lock().iter().any(|s| matches!(s, Some(None)))
}

/// What discarding 1:1 previews did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Discarded {
    /// 1:1 previews removed.
    pub removed: usize,
    /// 1:1 previews still kept (of the photos looked at).
    pub kept: usize,
}

/// Discard the 1:1 previews of `ids` (memory and disk): all of them, or with `older_than` only
/// those not built or shown for that long. A preview held only in memory (no library on disk) has
/// no age and is kept unless all go.
pub fn discard_full(s: &Session, ids: &[lcc_catalog::PhotoId], older_than: Option<std::time::Duration>) -> Discarded {
    let cache = &s.media.rendered;
    let now = std::time::SystemTime::now();
    let mut d = Discarded::default();
    for id in ids {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let key = Session::full_view_key(p);
        if !cache.contains(key) {
            continue;
        }
        let stale = match older_than {
            None => true,
            Some(age) => cache.disk().and_then(|disk| disk.modified(key)).and_then(|m| now.duration_since(m).ok()).is_some_and(|a| a >= age),
        };
        if stale && cache.remove(key) {
            d.removed += 1;
        } else {
            d.kept += 1;
        }
    }
    d
}

/// Apply the library's "discard 1:1 previews after" setting to every photo.
pub fn auto_discard(s: &Session) -> Discarded {
    let Some(age) = s.preview_prefs.discard_full.max_age() else { return Discarded::default() };
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    discard_full(s, &ids, Some(age))
}

fn discard(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.discardPreviews";
    match str_param(p, "size").unwrap_or("full") {
        "full" | "1:1" => {}
        other => return Err(bad(C, format!("unknown size `{other}` (only full: standard previews go with library.clearPreviews)"))),
    }
    let d = if p.get("auto").and_then(Value::as_bool) == Some(true) {
        auto_discard(s)
    } else {
        let ids = if p.get("ids").is_some() || p.get("id").is_some() || !s.selection.ids.is_empty() { s.targets(p)? } else { s.visible_cloned() };
        discard_full(s, &ids, None)
    };
    Ok(json!({"removed": d.removed, "kept": d.kept}))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.previewSettings";
    let mut next = s.preview_prefs;
    if let Some(v) = p.get("standardEdge") {
        let e = v.as_u64().filter(|e| (256..=8192).contains(e)).ok_or_else(|| bad(C, "standardEdge must be 256–8192 px"))?;
        next.standard_edge = e as u32;
    }
    if let Some(v) = p.get("discardFull") {
        next.discard_full = v.as_str().and_then(DiscardAfter::parse).ok_or_else(|| bad(C, "discardFull must be day, week, month or never"))?;
    }
    if let Some(v) = p.get("atImport") {
        next.at_import = v.as_str().and_then(ImportPreviews::parse).ok_or_else(|| bad(C, "atImport must be minimal, standard or full"))?;
    }
    if next != s.preview_prefs {
        s.preview_prefs = next;
        s.save_prefs()?;
    }
    Ok(s.preview_prefs.json())
}

/// The previews an import asks for ([`PreviewPrefs::at_import`]) of the photos it brought in,
/// built in the background like Build Previews. Errors are logged: the import itself is done.
pub fn build_after_import(s: &mut Session, ids: &[lcc_catalog::PhotoId]) {
    let size = match s.preview_prefs.at_import {
        ImportPreviews::Minimal => return,
        ImportPreviews::Standard => "standard",
        ImportPreviews::Full => "full",
    };
    if ids.is_empty() {
        return;
    }
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    if let Err(e) = build(s, &json!({"size": size, "ids": ids})) {
        log::warn!("previews at import: {e}");
    }
}

/// One photo's smart preview to build or discard: (photo, its file in the smart previews folder,
/// how to load the original at preview size, whether the photo can take a preview-fitted look:
/// vendor raws and DNGs (whose tone can come from their preview); only those proxies go stale when
/// the fit changes; an older encoding makes any proxy stale).
#[cfg(not(target_arch = "wasm32"))]
type SmartJob = (lcc_catalog::PhotoId, std::path::PathBuf, Option<crate::media::SourceRef>, bool);

/// DNG tone fits also go stale, although DNGs keep their own matrix and Kelvin white-balance scale.
/// So do preview-only raws: their proxy is the embedded JPEG, read in the colour space the raw
/// declares, which a look version bump can change too (#2654). And so does a raw developed through a camera
/// profile without a tone curve of its own: it keeps the tone fitted with the photo's own colour.
#[cfg(not(target_arch = "wasm32"))]
fn preview_fitted_look(photo: &lcc_catalog::Photo) -> bool {
    photo.fitted_look()
        || (photo.develops_raw() && photo.format.eq_ignore_ascii_case("DNG"))
        || (photo.kind == lcc_catalog::MediaKind::Raw && photo.preview_only.is_some())
        || crate::camera_profiles::photo_profile(photo).is_some_and(|d| crate::camera_preview::keeps_photo_tone(&d))
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
        SourceRef::File { path, max_edge, loader, denoise, options, .. } => {
            Some(SourceRef::File { path, max_edge, loader, fallback: None, denoise, options, reduced: None })
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
                // atomic: a failed write leaves no partial proxy that would pass for a built one;
                // and synced (issue #134): built so the photo can be edited while its original
                // is offline, when it is the only copy — the sync is small next to the decode
                let r = source
                    .ok_or_else(|| "nothing to build from".to_string())
                    .and_then(|s| s.load_source())
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
                        lcc_catalog::safe_file::write_atomic(&path, &b).map_err(|e| format!("{}: {e}", path.display()))
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
    let dir = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a catalog on disk"))?;
    let discard = p.get("discard").and_then(Value::as_bool).unwrap_or(false);
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    if background && let Some(b) = s.preview_build.as_ref().filter(|b| b.running()) {
        // (the background preparation gives way, as to Build Previews; a build asked for while
        // another runs waits for its turn, as Build Previews does)
        if b.background {
            b.cancel.store(true, Ordering::Relaxed);
        } else {
            return enqueue(s, C, "library.smartPreviews", p);
        }
    }
    let ids = if p.get("ids").is_some() || !s.selection.ids.is_empty() { s.targets(p)? } else { s.visible_cloned() };
    let mut jobs: Vec<SmartJob> = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id).cloned() else { continue };
        if !matches!(ph.source, lcc_catalog::Source::File { .. }) {
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
    let photos: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, lcc_catalog::Source::File { .. })).collect();
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
    let current = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a catalog on disk"))?;
    let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let chosen = str_param(p, "path").filter(|x| !x.trim().is_empty());
    if !reset && chosen.is_none() {
        return Ok(location_json(s));
    }
    let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(C, "smart previews need a catalog on disk"))?;
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
pub fn has_smart_preview(s: &Session, id: lcc_catalog::PhotoId) -> bool {
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
            "{ids?, discard?: bool, background?: bool}: build (or discard) the smart previews of the selected photos (else all in view): compact proxies in the library that keep photos editable and exportable (at proxy size) while their originals are offline; a damaged proxy (cut short) is rebuilt → {built, repaired, removed, failed}; with background (the app's menu) the files are read and written on a worker thread → {total, done, failed, repaired, running, what} like library.buildPreviews (progress: library.previewProgress, stop: library.cancelPreviews), and like it a background request while another build runs waits for its turn → {queued: true, position}",
            always,
            smart
        ),
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviewsLocation",
            "Smart Previews Location",
            [],
            None,
            "{path?: absolute folder, reset?: bool, existing?: move|leave|discard}: without path/reset: where this library keeps its smart previews → {path, default, custom, available, count, bytes}. With path (or reset = back to `Smart Previews` in the library): use that folder instead (must exist or have an existing parent, and accept writes; never falls back to another drive). Smart previews already in the old folder need `existing`: move them, leave them (build again), or discard them → also {handled, failed}",
            always,
            smart_location
        ),
        cmd!(query "photo.smartPreview", "Smart Preview Status", [], None, "{id?} → {smartPreview: bool, originalOnline: bool}", always, |s, p| {
            let id = s.targets(p)?.first().copied().ok_or_else(|| bad("photo.smartPreview", "no photo"))?;
            let online = match s.catalog.photo(id).map(|p| p.source.clone()) {
                Some(lcc_catalog::Source::File { path }) => std::path::Path::new(&path).exists(),
                _ => true,
            };
            Ok(json!({"smartPreview": has_smart_preview(s, id), "originalOnline": online}))
        }),
        cmd!(
            "library.buildPreviews",
            "Build Previews",
            [],
            None,
            "{size?: standard (the library's standard size, or `edge`) | full (1:1), ids?, wait?: bool, force?: bool, from?: id, backward?: bool, background?: bool}: render the grid thumbnail and loupe view of the selected photos (else all in view) into the preview cache (memory and disk), in the background unless `wait`. Photos whose previews are there already (big enough) are skipped unless `force`; the photos nearest `from` go first, in the direction of travel. A build asked for while another runs waits for its turn (an identical waiting request is not queued twice; library.cancelPreviews drops the waiting ones) → {queued: true, position}. `background`: the background preparation's build (at least the loupe's size, two lanes, waits while the photo on screen renders and while an export or import runs; a build asked for replaces it) → {total, done, failed, skipped, shared, background, running}",
            always,
            build
        ),
        cmd!(
            "library.discardPreviews",
            "Discard 1:1 Previews",
            [],
            None,
            "{size?: full, ids?, auto?: bool}: discard the 1:1 previews of the selected photos (else all in view), or with auto those past the library's discardFull age (library.previewSettings); standard previews stay → {removed, kept}",
            always,
            discard
        ),
        cmd!(
            "library.previewSettings",
            "Preview Settings",
            [],
            None,
            "{standardEdge?: 256–8192 px, discardFull?: day|week|month|never, atImport?: minimal|standard|full}: the library's preview store: the size of standard previews, when 1:1 previews are discarded on their own, and which previews an import builds (saved with the library) → {standardEdge, discardFull, atImport}",
            always,
            settings
        ),
        cmd!(query "library.previewProgress", "Preview Build Progress", [], None, "{} → {total, done, failed, repaired, skipped, shared, background, running, cancelled, what, error} | null", always, |s, _| {
            // a poll is the headless frontends' (CLI, MCP) frame: the waiting build starts here,
            // as it does on the app's own poll ([`drain_queue`]'s guards make it a no-op while a
            // build that was asked for still runs)
            drain_queue(s);
            Ok(s.preview_build.as_ref().map_or(Value::Null, |b| b.json()))
        }),
        cmd!(query "prefs.backgroundPrep", "Background Preparation Settings", [], None, "{} → {app, library|null, settings (in use), own: bool (this library keeps its own), building}: what is prepared in the background per mode (culling | library: previews = build the previews of every photo in view in the spare time, ahead = photos each way rendered for the loupe before they are reached, eyeAhead = photos ahead whose eye focus strip is worked out, scope all|marked|onScreen; develop: ahead = photos each way made ready to edit, scope)", always, |s, _| {
            Ok(s.prep_json())
        }),
        cmd!(
            "prefs.setBackgroundPrep",
            "Set Background Preparation",
            [],
            None,
            "{settings?: partial {culling?, library?, develop?} (unknown fields rejected), library?: bool (this library's own settings; else the app-wide ones), reset?: bool (requires an open library and cannot be combined with settings)} → as prefs.backgroundPrep",
            always,
            |s, p| {
                const C: &str = "prefs.setBackgroundPrep";
                let library = match p.get("library") {
                    Some(Value::Bool(value)) => *value,
                    Some(_) => return Err(bad(C, "library must be a boolean")),
                    None => false,
                };
                let reset = match p.get("reset") {
                    Some(Value::Bool(value)) => *value,
                    Some(_) => return Err(bad(C, "reset must be a boolean")),
                    None => false,
                };
                if reset {
                    if p.get("settings").is_some() {
                        return Err(bad(C, "settings cannot be combined with reset"));
                    }
                    if s.library.is_none() {
                        return Err(bad(C, "no catalog is open"));
                    }
                    s.clear_library_prep()?;
                    return Ok(s.prep_json());
                }
                if library && s.library.is_none() {
                    return Err(bad(C, "no catalog is open"));
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
        cmd!(
            "library.cancelPreviews",
            "Cancel Preview Build",
            [],
            None,
            "{}: stop the running preview build and drop the builds waiting behind it → the build's state (null without one), with how many waiting builds were dropped",
            always,
            |s, _| {
                let dropped = s.preview_queue.drain(..).count() as u64;
                if let Some(b) = &s.preview_build {
                    b.cancel.store(true, Ordering::Relaxed);
                }
                let mut r = s.preview_build.as_ref().map_or(Value::Null, |b| b.json());
                if dropped > 0 {
                    if !r.is_object() {
                        r = json!({});
                    }
                    r["queuedDropped"] = json!(dropped);
                }
                Ok(r)
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Building, jobs_for, run_photo};
    use crate::Session;
    use crate::media::SourceRef;

    // Given a preview-only raw, its proxy goes stale with the look version: the embedded JPEG's colour
    // space can change with it (#2654); a rendered photo's proxy does not
    #[test]
    fn preview_only_raws_follow_the_look_version() {
        use lcc_catalog::{MediaKind, Photo, PhotoId, Source};
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
        jobs[1].source = SourceRef::File {
            path: "/nonexistent/photo.arw".into(),
            max_edge: 512,
            loader: None,
            fallback: None,
            denoise: None,
            options: Default::default(),
            reduced: None,
        };
        assert!(run_photo(jobs), "the thumbnail rendered from the view's decoded source");
    }

    // Given a build rendering a photo, the loupe can tell (it waits for that render instead of decoding the
    // raw a second time beside it); before and after, it can't, and another session's cache doesn't count
    #[test]
    fn a_photo_being_built_is_being_prepared_until_its_preview_is_in() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        assert!(!s.being_prepared(id), "nothing builds it yet");
        let jobs = jobs_for(&mut s, id, Some(512));
        let held = Building::take(id.0, &jobs).expect("not held by another build");
        assert!(s.being_prepared(id), "a build holds it");
        let other = Session::with_demo();
        assert!(!other.being_prepared(id), "another cache's photo of the same id");
        drop(held);
        assert!(!s.being_prepared(id), "released once built");
    }

    /// The pool tests take turns: one using every pool leaves the other's photo to run here.
    static POOL_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Given the background preparation's renders, they run on its own pool (lowest priority, below the
    // render workers that make the photo on screen), not on the shared one
    #[test]
    fn background_preparation_renders_on_its_own_pool() {
        let _one_at_a_time = POOL_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pool = crate::prep::background_pool(0).expect("the pool starts");
        let inside = crate::prep::on_background_pool(|| {
            rayon::current_thread_index().is_some() && std::thread::current().name().is_some_and(|n| n.starts_with("lc-prep-"))
        });
        assert!(inside, "a background photo runs on an lc-prep thread");
        assert!(pool.current_num_threads() >= 1);
        // (nested parallel work stays on it)
        let names: Vec<bool> = crate::prep::on_background_pool(|| {
            use rayon::prelude::*;
            (0..64).into_par_iter().map(|_| std::thread::current().name().is_some_and(|n| n.starts_with("lc-prep-"))).collect()
        });
        assert!(names.iter().all(|b| *b));
    }

    // Given photos of the background preparation rendered side by side, no thread ever runs one
    // photo inside another (a worker waiting in one photo's parallel work stole the other photo's
    // render onto its stack when both were installed on one pool: per-thread state mixed, and the
    // inner photo could wait at the memory gate for memory the outer one held)
    #[test]
    fn background_photos_never_run_inside_one_another() {
        let _one_at_a_time = POOL_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        thread_local! { static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
        let nested = std::sync::Arc::new(AtomicBool::new(false));
        let lanes: Vec<_> = (0..4)
            .map(|_| {
                let nested = nested.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        crate::prep::on_background_pool(|| {
                            if DEPTH.with(|d| d.replace(d.get() + 1)) > 0 {
                                nested.store(true, Ordering::Relaxed);
                            }
                            // parallel work whose waits let a worker take other jobs
                            (0..16).into_par_iter().for_each(|_| std::thread::sleep(std::time::Duration::from_millis(1)));
                            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
                        });
                    }
                })
            })
            .collect();
        for l in lanes {
            l.join().unwrap();
        }
        assert!(!nested.load(Ordering::Relaxed), "a photo ran inside another on one thread");
    }

    // Given a background photo and a view render (a render worker's job on the shared pool) at the
    // same time, neither's parallel work ever runs on the other's threads (no render nests inside
    // another on one thread), and both finish (no deadlock)
    #[test]
    fn a_background_photo_and_a_view_render_never_share_a_thread() {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        let _one_at_a_time = POOL_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut s = Session::with_demo();
        let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(2).collect();
        let prep_jobs = jobs_for(&mut s, ids[0], Some(512));
        let view = s.loupe_job(ids[1], 512, 512, true).expect("a view job");
        let crossed = std::sync::Arc::new(AtomicBool::new(false));
        let is_prep = || std::thread::current().name().is_some_and(|n| n.starts_with("lc-prep-"));
        let (tx, rx) = std::sync::mpsc::channel();
        for round in 0..4 {
            let (prep_jobs, mut view, crossed, tx) = (prep_jobs.clone(), view.clone(), crossed.clone(), tx.clone());
            // the background photo: a real render on its pool, then parallel work checked thread by thread
            let c = crossed.clone();
            let t1 = tx.clone();
            std::thread::spawn(move || {
                crate::prep::on_background_pool(|| {
                    let ok = run_photo(prep_jobs);
                    (0..64).into_par_iter().for_each(|_| {
                        if !is_prep() {
                            c.store(true, Ordering::Relaxed);
                        }
                        std::thread::sleep(std::time::Duration::from_micros(200));
                    });
                    let _ = t1.send(("prep", round, ok));
                });
            });
            // the view render, from a thread of its own as the render workers run it (shared pool)
            std::thread::spawn(move || {
                view.view_cache = None;
                let ok = view.run().rendered.is_ok();
                (0..64).into_par_iter().for_each(|_| {
                    if is_prep() {
                        crossed.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_micros(200));
                });
                let _ = tx.send(("view", round, ok));
            });
        }
        drop(tx);
        for _ in 0..8 {
            let (what, round, ok) = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("both renders finish (no deadlock)");
            assert!(ok, "{what} render {round} worked");
        }
        assert!(!crossed.load(Ordering::Relaxed), "a render's parallel work ran on the other's threads");
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

    // Given a photo, its preview build renders as batch work (a turn on whichever card has room,
    // behind the photo on screen), never as the screen's interactive work; the thread is back to
    // interactive work after it, also when the work panics
    #[test]
    fn preview_builds_are_batch_work() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let jobs = jobs_for(&mut s, id, Some(256));
        let before = lcc_gpu::thread_render_counts();
        assert!(run_photo(jobs));
        let after = lcc_gpu::thread_render_counts();
        assert_eq!(
            (after.editing_interactive, after.helper_interactive),
            (before.editing_interactive, before.helper_interactive),
            "nothing of the build renders as the screen's work"
        );
        if !lcc_gpu::available() {
            lcc_testkit::skip!(Gpu, "no GPU adapter: the build rendered on the processor");
            return;
        }
        let batch = (after.editing_batch + after.helper_batch) - (before.editing_batch + before.helper_batch);
        assert!(batch >= 1, "the build's renders are batch work on a card: {before:?} -> {after:?}");
        let r: Result<(), String> = crate::media::background_photo("a test", 1, 1, || panic!("boom"));
        assert!(r.unwrap_err().contains("a test"));
        let mut job = s.loupe_job(id, 128, 128, true).unwrap();
        let b = lcc_gpu::thread_render_counts();
        (job.view_cache, job.cache) = (None, None);
        job.run().rendered.unwrap();
        let a = lcc_gpu::thread_render_counts();
        assert_eq!((a.editing_batch, a.helper_batch), (b.editing_batch, b.helper_batch), "the loupe's render is the screen's work");
    }
}
