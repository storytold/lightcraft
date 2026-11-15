//! Background tasks in flight (issue #345): imports, exports, preview builds, downloads, the face scan… Each one
//! registers here while it runs, so every frontend can show them in one place (the desktop app's activity stack) and
//! cancel the ones that can be cancelled (`activity.list`, `activity.cancel`).
//!
//! A job holds a [`TaskGuard`] for as long as it runs; dropping it — when the job ends, fails or its thread unwinds
//! from a panic — removes the row, so a dead worker never leaves a stuck bar. Jobs keep the cancel flag they already
//! check between files or photos: the guard adopts it ([`Cancel::Flag`]), so the stack's ✕, `activity.cancel` and the
//! job's own Cancel all set the same atomic. Numbers are worded by the frontend from `done`, `total` and [`Unit`]; a
//! detail set here is a name (a file, a model), never a sentence, so the frontend can translate everything else.
//!
//! **Batches**: a job that works through many photos (AI Denoise, export…)
//! holds one parent row made with [`Activity::start_batch`]. It counts the photos done and failed
//! ([`TaskGuard::item_done`], [`TaskGuard::item_failed`]), estimates the time left from the measured time per photo
//! ([`TaskInfo::eta_ms`]), and leaves a [`BatchSummary`] when it ends ("200 done, 2 failed", with the reasons). The
//! per-photo rows under it are children ([`TaskGuard::child`], [`Activity::start_child`]): the stack folds them under
//! the parent, and cancelling the parent stops them too.
//!
//! **Names in the text**: a label, detail or stop warning may say `{key}` where a name goes that no catalog
//! translates (a program's, a plug-in's: [`TaskGuard::name`]). `activity.list` gives the English with the names in;
//! [`TaskInfo::templates`] keeps the text as written, so a frontend translates it first and then puts the names in
//! ([`fill`]).
//!
//! **Stopping that throws work away**: a task whose stop loses what it would still bring back (a plug-in's wait for
//! a program) carries a warning ([`TaskGuard::warn_on_stop`], `stopWarning`). Frontends ask before they stop it;
//! `activity.cancel` still stops it at once.
//!
//! **Pause and settings**: a continuous background job the user may want to hold (the face scan, search indexing)
//! carries [`Controls`] ([`TaskGuard::set_controls`]): the commands that pause and resume it and the Settings page with
//! its options. Frontends show a pause (or resume) button and a gear on its row; `activity.pause` runs the same commands.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

/// The longest label or detail kept, in characters (a file name can be anything).
const MAX_TEXT: usize = 200;
/// Reasons kept per batch for its summary (the first ones; `failed` says how many there were).
const MAX_REASONS: usize = 20;
/// Summaries of finished batches kept for the frontends ([`Activity::finished`]).
const MAX_FINISHED: usize = 20;

/// The background tasks in flight. Cheap to clone (one `Arc`), so worker threads can hold it.
#[derive(Clone, Default)]
pub struct Activity {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    tasks: Mutex<Vec<Arc<Entry>>>,
    next: AtomicU64,
    /// Batches that ended, newest last (at most [`MAX_FINISHED`]).
    finished: Mutex<VecDeque<BatchSummary>>,
}

/// What a batch did, kept when its row goes (`activity.list` → `finished`; the desktop app shows it once as a
/// message: "AI Denoise: 198 done, 2 failed").
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSummary {
    /// The batch row's id.
    pub id: u64,
    /// Increases with every summary: a frontend shows the ones above the last `seq` it saw.
    pub seq: u64,
    pub kind: &'static str,
    pub label: String,
    /// Items (photos) finished, failed ones included.
    pub done: u64,
    pub failed: u64,
    /// Items the batch had when it ended (more than `done` when it was cancelled or stopped early).
    pub total: u64,
    pub cancelled: bool,
    /// Why items failed: the first few, each a name and a reason.
    pub reasons: Vec<String>,
    pub seconds: f64,
    /// The job says how it went itself (an export's own message): frontends don't repeat it.
    pub reported: bool,
}

/// How a task can be cancelled.
pub enum Cancel {
    /// Not at all (no ✕): the face scan, AI Denoise.
    No,
    /// With a flag of its own ([`TaskGuard::cancel_flag`]).
    Yes,
    /// With the job's existing flag.
    Flag(Arc<AtomicBool>),
}

/// How the frontend words `done` / `total`: "3 of 25", "12 of 340 MB", "40 %".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    #[default]
    Count,
    Bytes,
    Percent,
}

impl Unit {
    fn from_u8(v: u8) -> Unit {
        match v {
            1 => Unit::Bytes,
            2 => Unit::Percent,
            _ => Unit::Count,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Unit::Count => 0,
            Unit::Bytes => 1,
            Unit::Percent => 2,
        }
    }
}

/// Pause / resume and a Settings page for a continuous background job (the face scan, search indexing): its row gets a
/// pause button (resume while `paused`) and a gear that opens Settings there. The commands are engine command ids, so
/// every frontend runs the same ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Controls {
    /// The job is paused: the row offers resume.
    pub paused: bool,
    /// The command that pauses the job (`faces.pause`).
    pub pause: &'static str,
    /// The command that resumes it (`faces.resume`).
    pub resume: &'static str,
    /// The Settings tab with its options (`app.settings {tab}`).
    pub settings_tab: &'static str,
    /// A section of that tab to bring into view (`app.settings {section}`), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_section: Option<&'static str>,
}

/// One task as the frontends see it (`activity.list`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    pub id: u64,
    /// A stable id: `import`, `export`, `scan`, `previews`, `smartPreviews`, `lightroom`, `merge`, `download`,
    /// `faces`, `denoise`, `findMissing`, `applePhotos`, `renderDng`, `cache`, `focus`, `saveMetadata`, `readMetadata`,
    /// `mlIndex`, `sensorMaps` (camera sensor map builds and Remove Dust).
    pub kind: &'static str,
    /// English; the frontend translates it.
    pub label: String,
    pub done: u64,
    /// 0 while the amount of work isn't known yet (an indeterminate bar).
    pub total: u64,
    pub unit: Unit,
    /// The file, photo or model being worked on, if any.
    pub detail: String,
    pub cancellable: bool,
    /// Cancel was asked for; the job stops at its next check.
    pub cancelling: bool,
    pub age_ms: u64,
    /// The batch row this one belongs to (a photo of an AI Denoise batch); the stack folds it under its parent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// A batch row ([`Activity::start_batch`]): counts photos, with `failed` and `etaMs`.
    pub batch: bool,
    /// Items that failed so far (counted in `done` too).
    pub failed: u64,
    /// Time left (ms), estimated from the measured time per item; none until an item is done, or while the total
    /// isn't known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    /// Stopping the task throws work away (a plug-in's wait for a program: what the program makes afterwards never
    /// comes back): what to tell the user before stopping it, in English. Frontends ask first; `activity.cancel`
    /// stops it at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_warning: Option<String>,
    /// Pause / resume and the Settings page, for a job that has them ([`TaskGuard::set_controls`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controls: Option<Controls>,
    /// The label, detail and stop warning as written, before the names went in (`None`: the task has no names),
    /// for frontends that translate.
    #[serde(skip)]
    pub templates: Option<Templates>,
}

/// A task's texts as written, with `{key}` where each name goes, and the names ([`TaskGuard::name`]). A frontend
/// translates a text and then puts the names in with [`fill`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Templates {
    pub label: String,
    pub detail: String,
    pub stop_warning: Option<String>,
    pub names: Vec<(&'static str, String)>,
}

/// `template` with each `{key}` of `names` replaced by its name, in one pass (a name that contains `{…}` stays as
/// it is); any other `{…}` is kept.
pub fn fill(template: &str, names: &[(&'static str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let found = from.find('}').and_then(|close| {
            let key = from.get(1..close)?;
            let (_, name) = names.iter().find(|(k, _)| *k == key)?;
            Some((name, close))
        });
        match found {
            Some((name, close)) => {
                out.push_str(name);
                rest = from.get(close + 1..).unwrap_or("");
            }
            None => {
                out.push('{');
                rest = from.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

/// Names a task's texts can say (more is a mistake: the rest are dropped).
const MAX_NAMES: usize = 8;

struct Entry {
    id: u64,
    kind: &'static str,
    label: String,
    started: web_time::Instant,
    done: AtomicU64,
    total: AtomicU64,
    unit: AtomicU8,
    detail: Mutex<String>,
    cancel: Option<Arc<AtomicBool>>,
    cancellable: AtomicBool,
    /// Held while a cancel is decided ([`Activity::cancel`], [`Activity::cancel_all`]) and while
    /// cancelling is switched ([`TaskGuard::set_cancellable`], [`TaskHandle::set_cancellable`]): a
    /// step that turns it off and then reads the cancel flag either sees a cancel that was
    /// accepted, or makes the cancel fail. Without it a cancel could pass the check, the step
    /// start, and the flag be set after the step's last look at it: accepted but ignored.
    gate: Mutex<()>,
    parent: Option<u64>,
    batch: bool,
    failed: AtomicU64,
    reasons: Mutex<Vec<String>>,
    /// When the first item was done, and how many were done then: the time per item is measured from there,
    /// so a slow start (loading a model) doesn't stretch the estimate for the rest.
    first: Mutex<Option<(web_time::Instant, u64)>>,
    reported: AtomicBool,
    /// [`TaskGuard::name`]: what `{key}` stands for in the label, detail and stop warning.
    names: Mutex<Vec<(&'static str, String)>>,
    /// [`TaskGuard::warn_on_stop`], as written (names not in).
    stop_warning: Mutex<Option<String>>,
    /// [`TaskGuard::set_controls`].
    controls: Mutex<Option<Controls>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn capped(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

impl Entry {
    fn set_cancellable(&self, yes: bool) {
        let _gate = lock(&self.gate);
        self.cancellable.store(yes, Ordering::Relaxed);
    }

    /// Set the cancel flag if the task can be cancelled now (see [`Entry::gate`]).
    fn try_cancel(&self) -> bool {
        let _gate = lock(&self.gate);
        match &self.cancel {
            Some(flag) if self.is_cancellable() => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    fn is_cancellable(&self) -> bool {
        self.cancel.is_some() && self.cancellable.load(Ordering::Relaxed)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
    }

    fn progress(&self, done: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        let done = if total > 0 { done.min(total) } else { done };
        self.done.store(done, Ordering::Relaxed);
        self.mark_first(done);
    }

    fn mark_first(&self, done: u64) {
        if done > 0 {
            let mut first = lock(&self.first);
            if first.is_none() {
                *first = Some((web_time::Instant::now(), done));
            }
        }
    }

    /// One more item finished (`why`: it failed, and why). A total that was short grows with it.
    fn item(&self, why: Option<&str>) {
        let done = self.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let _ = self.total.try_update(Ordering::Relaxed, Ordering::Relaxed, |t| (t > 0 && t < done).then_some(done));
        self.mark_first(done);
        if let Some(why) = why {
            self.count_failed(why);
        }
    }

    fn count_failed(&self, why: &str) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        let mut r = lock(&self.reasons);
        if r.len() < MAX_REASONS {
            r.push(capped(why));
        }
    }

    /// Time left: the time per item measured since the first one was done (or since the start, while only the first
    /// ones are), times the items left.
    fn eta_ms(&self, done: u64, total: u64) -> Option<u64> {
        if total == 0 || done == 0 || done >= total {
            return None;
        }
        let left = (total - done) as f64;
        let (at, first_done) = (*lock(&self.first))?;
        let per_item = if done > first_done {
            at.elapsed().as_secs_f64() / (done - first_done) as f64
        } else {
            self.started.elapsed().as_secs_f64() / done as f64
        };
        let ms = per_item * left * 1000.0;
        ms.is_finite().then(|| ms.clamp(0.0, 1e15) as u64)
    }

    fn summary(&self, seq: u64) -> BatchSummary {
        BatchSummary {
            id: self.id,
            seq,
            kind: self.kind,
            label: self.label(),
            done: self.done.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            cancelled: self.is_cancelled(),
            reasons: lock(&self.reasons).clone(),
            seconds: self.started.elapsed().as_secs_f64(),
            reported: self.reported.load(Ordering::Relaxed),
        }
    }

    fn detail(&self, text: &str) {
        *lock(&self.detail) = capped(text);
    }

    fn name(&self, key: &'static str, value: &str) {
        let mut names = lock(&self.names);
        if let Some(at) = names.iter().position(|(k, _)| *k == key) {
            if let Some(slot) = names.get_mut(at) {
                slot.1 = capped(value);
            }
        } else if names.len() < MAX_NAMES {
            names.push((key, capped(value)));
        }
    }

    /// The label with the names in.
    fn label(&self) -> String {
        fill(&self.label, &lock(&self.names))
    }

    fn info(&self) -> TaskInfo {
        let (done, total) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        let unit = Unit::from_u8(self.unit.load(Ordering::Relaxed));
        // each read once, so the filled texts and the templates agree; one lock at a time
        let names = lock(&self.names).clone();
        let raw_detail = lock(&self.detail).clone();
        let raw_warning = lock(&self.stop_warning).clone();
        let detail = fill(&raw_detail, &names);
        let stop_warning = raw_warning.as_deref().map(|w| fill(w, &names));
        let label = fill(&self.label, &names);
        let templates = (!names.is_empty()).then(|| Templates { label: self.label.clone(), detail: raw_detail, stop_warning: raw_warning, names });
        TaskInfo {
            id: self.id,
            kind: self.kind,
            label,
            done,
            total,
            unit,
            detail,
            cancellable: self.is_cancellable(),
            cancelling: self.is_cancelled(),
            age_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            parent: self.parent,
            batch: self.batch,
            failed: self.failed.load(Ordering::Relaxed),
            eta_ms: if self.batch && unit == Unit::Count { self.eta_ms(done, total) } else { None },
            stop_warning,
            controls: lock(&self.controls).clone(),
            templates,
        }
    }
}

impl Activity {
    /// Work the user started that the background preparation ([`crate::prep`]) gives way to while
    /// it runs (preparing photos competed with an export, which could hardly start): an export, an
    /// import, any batch over photos (AI Denoise, AI masks, finding eyes…). Its kind, or `None`.
    pub fn user_work(&self) -> Option<&'static str> {
        const KINDS: [&str; 5] = ["export", "import", "merge", "benchmark", "lightroom"];
        lock(&self.inner.tasks).iter().find(|e| e.parent.is_none() && (e.batch || KINDS.contains(&e.kind))).map(|e| e.kind)
    }

    /// Register a task; it is listed until the returned guard is dropped. `label` is English (the frontend
    /// translates it).
    pub fn start(&self, kind: &'static str, label: &str, cancel: Cancel) -> TaskGuard {
        self.register(kind, label, cancel, None, false)
    }

    /// Register a batch: one row for a job over many photos, counting them ([`TaskGuard::item_done`],
    /// [`TaskGuard::item_failed`]) with the time left, and leaving a [`BatchSummary`] when it ends.
    pub fn start_batch(&self, kind: &'static str, label: &str, cancel: Cancel) -> TaskGuard {
        self.register(kind, label, cancel, None, true)
    }

    /// Register a row under row `parent` (one photo of a batch). A `parent` that has ended meanwhile makes it a row
    /// of its own.
    pub fn start_child(&self, parent: u64, kind: &'static str, label: &str, cancel: Cancel) -> TaskGuard {
        let parent = lock(&self.inner.tasks).iter().any(|e| e.id == parent).then_some(parent);
        self.register(kind, label, cancel, parent, false)
    }

    fn register(&self, kind: &'static str, label: &str, cancel: Cancel, parent: Option<u64>, batch: bool) -> TaskGuard {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let cancel = match cancel {
            Cancel::No => None,
            Cancel::Yes => Some(Arc::new(AtomicBool::new(false))),
            Cancel::Flag(flag) => Some(flag),
        };
        let entry = Arc::new(Entry {
            id,
            kind,
            label: capped(label),
            started: web_time::Instant::now(),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            unit: AtomicU8::new(Unit::Count.to_u8()),
            detail: Mutex::new(String::new()),
            cancellable: AtomicBool::new(cancel.is_some()),
            cancel,
            gate: Mutex::new(()),
            parent,
            batch,
            failed: AtomicU64::new(0),
            reasons: Mutex::new(Vec::new()),
            first: Mutex::new(None),
            reported: AtomicBool::new(false),
            names: Mutex::new(Vec::new()),
            stop_warning: Mutex::new(None),
            controls: Mutex::new(None),
        });
        lock(&self.inner.tasks).push(entry.clone());
        TaskGuard { activity: self.clone(), entry }
    }

    /// Whether `other` is this registry (a clone of it), not just another one like it.
    pub fn is(&self, other: &Activity) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Every task in flight, oldest first.
    pub fn list(&self) -> Vec<TaskInfo> {
        lock(&self.inner.tasks).iter().map(|e| e.info()).collect()
    }

    /// Ask task `id` to stop (it stops at its next check). An unknown id (the task may have just ended) or a task
    /// that can't be cancelled is an error.
    pub fn cancel(&self, id: u64) -> Result<(), String> {
        let tasks = lock(&self.inner.tasks);
        let entry = tasks.iter().find(|e| e.id == id).ok_or_else(|| format!("no task {id}"))?;
        if entry.try_cancel() { Ok(()) } else { Err(format!("{} can't be cancelled", entry.label())) }
    }

    /// Task `id`'s pause / resume controls: `Ok(None)` for a task without them, an error for an unknown id.
    pub fn controls(&self, id: u64) -> Result<Option<Controls>, String> {
        let tasks = lock(&self.inner.tasks);
        let entry = tasks.iter().find(|e| e.id == id).ok_or_else(|| format!("no task {id}"))?;
        Ok(lock(&entry.controls).clone())
    }

    /// Ask every cancellable task to stop; returns how many were asked.
    pub fn cancel_all(&self) -> usize {
        let tasks = lock(&self.inner.tasks);
        tasks.iter().filter(|e| e.try_cancel()).count()
    }

    /// Summaries of the batches that ended lately, oldest first (at most [`MAX_FINISHED`]).
    pub fn finished(&self) -> Vec<BatchSummary> {
        lock(&self.inner.finished).iter().cloned().collect()
    }

    /// Tasks that quitting would cut short: cancellable ones not already stopping.
    pub fn running_cancellable(&self) -> Vec<TaskInfo> {
        lock(&self.inner.tasks).iter().filter(|e| e.is_cancellable() && !e.is_cancelled()).map(|e| e.info()).collect()
    }
}

/// A task's place in the list, held by whoever owns the job; dropping it removes the row.
pub struct TaskGuard {
    activity: Activity,
    entry: Arc<Entry>,
}

impl TaskGuard {
    pub fn id(&self) -> u64 {
        self.entry.id
    }

    /// `done` of `total` (`total` 0 = not known yet). `done` is kept at most `total`.
    pub fn progress(&self, done: u64, total: u64) {
        self.entry.progress(done, total);
    }

    /// The file, photo or model being worked on (a name, not a sentence).
    pub fn detail(&self, text: &str) {
        self.entry.detail(text);
    }

    pub fn set_unit(&self, unit: Unit) {
        self.entry.unit.store(unit.to_u8(), Ordering::Relaxed);
    }

    /// The label, detail and stop warning say `{key}` where `value` goes: a name no catalog translates (a
    /// program's, a plug-in's), so the frontend translates the text around it ("Waiting for {program} to finish").
    pub fn name(&self, key: &'static str, value: &str) {
        self.entry.name(key, value);
    }

    /// Stopping this task throws work away: `text` (English, may say `{key}` names) is what the user is told before
    /// a frontend stops it. Only for a task that waits on something outside the app; a job that stops between
    /// files loses nothing already done and needs none.
    pub fn warn_on_stop(&self, text: &str) {
        *lock(&self.entry.stop_warning) = Some(capped(text));
    }

    /// A step that must not be cut short (the Lightroom import's commit) turns cancelling off for its duration.
    pub fn set_cancellable(&self, yes: bool) {
        self.entry.set_cancellable(yes);
    }

    /// Give the row pause / resume and a Settings page ([`Controls`]); `None` takes them away.
    pub fn set_controls(&self, controls: Option<Controls>) {
        *lock(&self.entry.controls) = controls;
    }

    pub fn cancel_flag(&self) -> Option<Arc<AtomicBool>> {
        self.entry.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.entry.is_cancelled()
    }

    /// A batch: one item (photo) finished. `done` counts up by one; a total that was short grows with it.
    pub fn item_done(&self) {
        self.entry.item(None);
    }

    /// A batch: one item failed (`why`: its name and the reason, kept for the summary). Counted in `done` too.
    pub fn item_failed(&self, why: &str) {
        self.entry.item(Some(why));
    }

    /// Items done so far (as [`TaskInfo::done`]).
    pub fn done(&self) -> u64 {
        self.entry.done.load(Ordering::Relaxed)
    }

    /// A batch whose progress is set with [`TaskGuard::progress`] (done counts every item): one of them failed.
    pub fn count_failed(&self, why: &str) {
        self.entry.count_failed(why);
    }

    /// The job says how it went itself (an export's message): its summary is marked `reported`.
    pub fn reports_itself(&self) {
        self.entry.reported.store(true, Ordering::Relaxed);
    }

    /// A row under this one (one photo of the batch), sharing its cancel flag: cancelling the batch stops it too.
    pub fn child(&self, kind: &'static str, label: &str) -> TaskGuard {
        let cancel = match &self.entry.cancel {
            Some(flag) => Cancel::Flag(flag.clone()),
            None => Cancel::No,
        };
        self.activity.register(kind, label, cancel, Some(self.entry.id), false)
    }

    /// For progress from another thread; it does not keep the row.
    pub fn handle(&self) -> TaskHandle {
        TaskHandle { entry: self.entry.clone() }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let id = self.entry.id;
        lock(&self.activity.inner.tasks).retain(|e| e.id != id);
        // a batch that did something leaves its summary
        if self.entry.batch && self.entry.done.load(Ordering::Relaxed) > 0 {
            let mut finished = lock(&self.activity.inner.finished);
            let seq = finished.back().map_or(1, |b| b.seq.saturating_add(1));
            finished.push_back(self.entry.summary(seq));
            while finished.len() > MAX_FINISHED {
                finished.pop_front();
            }
        }
    }
}

impl std::fmt::Debug for TaskGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskGuard").field("id", &self.entry.id).field("kind", &self.entry.kind).finish()
    }
}

/// Progress for a task from another thread (see [`TaskGuard::handle`]).
#[derive(Clone)]
pub struct TaskHandle {
    entry: Arc<Entry>,
}

impl TaskHandle {
    pub fn progress(&self, done: u64, total: u64) {
        self.entry.progress(done, total);
    }

    pub fn detail(&self, text: &str) {
        self.entry.detail(text);
    }

    pub fn is_cancelled(&self) -> bool {
        self.entry.is_cancelled()
    }

    /// [`TaskGuard::set_controls`] from another thread.
    pub fn set_controls(&self, controls: Option<Controls>) {
        *lock(&self.entry.controls) = controls;
    }

    /// [`TaskGuard::item_done`] from another thread.
    pub fn item_done(&self) {
        self.entry.item(None);
    }

    /// [`TaskGuard::item_failed`] from another thread.
    pub fn item_failed(&self, why: &str) {
        self.entry.item(Some(why));
    }

    /// The row's id (the parent of rows a worker starts with [`Activity::start_child`]).
    pub fn id(&self) -> u64 {
        self.entry.id
    }

    /// [`TaskGuard::set_cancellable`] from the worker, at the moment its step starts (an export
    /// handing its files to Apple Photos, which can't be stopped once asked). Read the cancel flag
    /// after turning it off: a cancel accepted before is seen then, any later one is refused.
    pub fn set_cancellable(&self, yes: bool) {
        self.entry.set_cancellable(yes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn names_go_into_the_texts_and_the_templates_keep_them_apart() {
        let a = Activity::default();
        let flag = Arc::new(AtomicBool::new(false));
        let g = a.start("pluginProgram", "Waiting for {program} to finish ({plugin})", Cancel::Flag(flag.clone()));
        g.name("program", "DxO PureRAW");
        g.name("plugin", "ccb: Edit in DxO PureRAW");
        g.detail("The results come back when {program} closes");
        g.warn_on_stop("{program} keeps running.");
        let t = a.list().remove(0);
        assert_eq!(t.label, "Waiting for DxO PureRAW to finish (ccb: Edit in DxO PureRAW)");
        assert_eq!(t.detail, "The results come back when DxO PureRAW closes");
        assert_eq!(t.stop_warning.as_deref(), Some("DxO PureRAW keeps running."));
        let x = t.templates.clone().unwrap_or_default();
        assert_eq!(x.label, "Waiting for {program} to finish ({plugin})", "the text as written, for translating");
        assert_eq!(x.stop_warning.as_deref(), Some("{program} keeps running."));
        // agents see the English with the names in, and the warning; never the templates
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["label"], "Waiting for DxO PureRAW to finish (ccb: Edit in DxO PureRAW)");
        assert_eq!(json["stopWarning"], "DxO PureRAW keeps running.");
        assert!(json.get("templates").is_none(), "{json}");
        // a warning asks the frontend to ask first; `activity.cancel` still stops the task at once
        a.cancel(g.id()).unwrap();
        assert!(flag.load(Ordering::Relaxed));
        // a name set again replaces the old one
        g.name("program", "Other");
        assert_eq!(a.list()[0].label, "Waiting for Other to finish (ccb: Edit in DxO PureRAW)");
    }

    #[test]
    fn a_task_without_names_or_warning_lists_as_before() {
        let a = Activity::default();
        let _g = a.start("export", "Exporting {n}", Cancel::Yes);
        let t = a.list().remove(0);
        assert_eq!(t.label, "Exporting {n}", "no names: the label is left alone");
        assert!(t.templates.is_none() && t.stop_warning.is_none());
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("stopWarning").is_none(), "{json}");
    }

    #[test]
    fn fill_puts_each_name_in_once_and_keeps_the_rest() {
        let names = vec![("a", "{b}".to_string()), ("b", "Bé".to_string())];
        assert_eq!(fill("{a} and {b}", &names), "{b} and Bé", "a name that says {{…}} isn't filled again");
        assert_eq!(fill("{c} {", &names), "{c} {", "unknown keys and a lone brace stay");
        assert_eq!(fill("}{b}{{b}}", &names), "}Bé{Bé}");
        assert_eq!(fill("", &names), "");
        assert_eq!(fill("ünï {b}", &[]), "ünï {b}");
    }

    #[test]
    fn names_are_capped() {
        let a = Activity::default();
        let g = a.start("x", "{k}", Cancel::No);
        g.name("k", &"n".repeat(5000));
        for i in 0..20 {
            g.name(["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7", "k8", "k9"][i % 10], "v");
        }
        let t = a.list().remove(0);
        assert_eq!(t.label.chars().count(), MAX_TEXT);
        assert_eq!(t.templates.unwrap_or_default().names.len(), MAX_NAMES);
    }

    #[test]
    fn guard_drop_removes_the_row() {
        let a = Activity::default();
        let g = a.start("export", "Exporting", Cancel::Yes);
        assert_eq!(a.list().len(), 1);
        drop(g);
        assert!(a.list().is_empty());
    }

    #[test]
    fn guard_drop_on_panic_unwind_removes_the_row() {
        let a = Activity::default();
        let a2 = a.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = a2.start("import", "Importing", Cancel::Yes);
            panic!("synthetic worker panic");
        }));
        assert!(r.is_err());
        assert!(a.list().is_empty());
    }

    #[test]
    fn cancel_sets_the_adopted_flag() {
        let a = Activity::default();
        let flag = Arc::new(AtomicBool::new(false));
        let g = a.start("export", "Exporting", Cancel::Flag(flag.clone()));
        a.cancel(g.id()).unwrap();
        assert!(flag.load(Ordering::Relaxed) && g.is_cancelled());
        assert!(a.list()[0].cancelling);
    }

    #[test]
    fn cancel_errors_instead_of_panicking() {
        let a = Activity::default();
        assert!(a.cancel(42).is_err());
        let g = a.start("faces", "Finding faces", Cancel::No);
        assert!(a.cancel(g.id()).is_err());
        let h = a.start("lightroom", "Importing Lightroom catalog", Cancel::Yes);
        h.set_cancellable(false);
        assert!(a.cancel(h.id()).is_err());
        assert!(!h.is_cancelled());
    }

    #[test]
    fn cancel_all_counts_only_cancellable() {
        let a = Activity::default();
        let (_x, _y, _z) = (a.start("export", "E", Cancel::Yes), a.start("import", "I", Cancel::Yes), a.start("faces", "F", Cancel::No));
        assert_eq!(a.cancel_all(), 2);
        assert!(a.running_cancellable().is_empty());
    }

    #[test]
    fn progress_is_clamped_and_detail_capped() {
        let a = Activity::default();
        let g = a.start("export", "E", Cancel::Yes);
        g.progress(12, 10);
        g.detail(&"é".repeat(300));
        let t = &a.list()[0];
        assert_eq!((t.done, t.total), (10, 10));
        assert_eq!(t.detail.chars().count(), 200);
        g.progress(5, 0); // indeterminate keeps done as given
        assert_eq!(a.list()[0].total, 0);
        assert_eq!(a.list()[0].unit, Unit::Count);
        g.set_unit(Unit::Bytes);
        assert_eq!(a.list()[0].unit, Unit::Bytes);
    }

    #[test]
    fn poisoned_lock_still_works() {
        let a = Activity::default();
        let a2 = a.clone();
        let _ = std::thread::spawn(move || {
            let _held = a2.inner.tasks.lock().unwrap();
            panic!("synthetic panic while holding the task list");
        })
        .join();
        assert!(a.inner.tasks.is_poisoned());
        let _g = a.start("export", "E", Cancel::Yes);
        assert_eq!(a.list().len(), 1);
    }

    /// Codex review of #643: a worker that turns cancelling off and then reads the cancel flag
    /// (an export handing its files to Apple Photos) must either see a cancel that was accepted or
    /// make it fail, never let one be accepted and then ignored. Here the test holds the task's gate
    /// as a cancel does while it decides: the worker's switch waits for it, and then sees the cancel.
    #[test]
    fn a_step_that_turns_cancelling_off_sees_or_refuses_every_cancel() {
        let a = Activity::default();
        let flag = Arc::new(AtomicBool::new(false));
        let g = a.start("export", "Exporting", Cancel::Flag(flag.clone()));
        let handle = g.handle();
        let gate = lock(&g.entry.gate); // a cancel deciding
        let (tx, rx) = std::sync::mpsc::channel();
        let (h, f) = (handle.clone(), flag.clone());
        let worker = std::thread::spawn(move || {
            h.set_cancellable(false);
            tx.send(f.load(Ordering::Relaxed)).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "the switch waits for the cancel");
        flag.store(true, Ordering::Relaxed); // the cancel goes through...
        drop(gate);
        assert!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), "...and the step sees it");
        worker.join().unwrap();
        // once it is off, cancels are refused
        let g2 = a.start("export", "Exporting", Cancel::Yes);
        g2.handle().set_cancellable(false);
        assert!(a.cancel(g2.id()).is_err());
        assert_eq!(a.cancel_all(), 0, "{:?}", a.list());
    }

    /// a batch counts photos done and failed, estimates the time left from the time per photo,
    /// folds its photos' rows under it, and leaves a summary when it ends.
    #[test]
    fn a_batch_counts_estimates_and_leaves_a_summary() {
        let a = Activity::default();
        let flag = Arc::new(AtomicBool::new(false));
        let b = a.start_batch("denoise", "AI Denoise", Cancel::Flag(flag.clone()));
        b.progress(0, 4);
        let t = a.list()[0].clone();
        assert!(t.batch && t.eta_ms.is_none() && t.failed == 0, "{t:?}");
        let child = b.child("denoise", "AI Denoise");
        assert_eq!(a.list()[1].parent, Some(b.id()));
        assert!(a.list()[1].cancellable, "a child shares the batch's cancel");
        std::thread::sleep(std::time::Duration::from_millis(20));
        b.item_done();
        std::thread::sleep(std::time::Duration::from_millis(20));
        b.handle().item_failed("IMG_2.CR3: the file is damaged");
        let t = a.list()[0].clone();
        assert_eq!((t.done, t.failed, t.total), (2, 1, 4));
        let eta = t.eta_ms.expect("an estimate once photos are done");
        assert!((5..60_000).contains(&eta), "{eta}");
        a.cancel(b.id()).unwrap();
        assert!(child.is_cancelled(), "cancelling the batch stops its photos");
        drop(child);
        drop(b);
        assert!(a.list().is_empty());
        let f = a.finished();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].done, f[0].failed, f[0].total, f[0].cancelled), (2, 1, 4, true));
        assert_eq!(f[0].reasons, vec!["IMG_2.CR3: the file is damaged".to_string()]);
        // a batch that did nothing leaves nothing; summaries are numbered
        drop(a.start_batch("export", "Exporting", Cancel::Yes));
        let e = a.start_batch("export", "Exporting", Cancel::Yes);
        e.progress(0, 1);
        e.item_done();
        e.item_done(); // more than the total said: it grows
        assert_eq!((a.list()[0].done, a.list()[0].total), (2, 2));
        drop(e);
        let f = a.finished();
        assert_eq!(f.len(), 2);
        assert!(f[1].seq > f[0].seq);
        assert_eq!(f[1].done, 2);
    }

    #[test]
    fn a_child_of_a_batch_that_ended_is_a_row_of_its_own() {
        let a = Activity::default();
        let b = a.start_batch("ai", "Updating AI masks", Cancel::No);
        let id = b.id();
        drop(b);
        let c = a.start_child(id, "ai", "Analyzing photo for AI masks", Cancel::No);
        assert_eq!(a.list()[0].parent, None);
        drop(c);
        // the estimate is never absurd: total unknown, all done
        let b = a.start_batch("export", "E", Cancel::Yes);
        b.item_done();
        assert_eq!(a.list()[0].eta_ms, None, "total unknown");
        b.progress(1, 1);
        assert_eq!(a.list()[0].eta_ms, None, "all done");
        drop(b);
        // summaries are capped, and so are their reasons
        for _ in 0..(MAX_FINISHED + 5) {
            let g = a.start_batch("export", "E", Cancel::Yes);
            g.item_failed(&"x".repeat(500));
        }
        assert_eq!(a.finished().len(), MAX_FINISHED);
        assert_eq!(a.finished()[0].reasons[0].chars().count(), MAX_TEXT);
    }

    #[test]
    fn handle_updates_from_another_thread() {
        let a = Activity::default();
        let g = a.start("previews", "Building previews", Cancel::Yes);
        let h = g.handle();
        std::thread::spawn(move || h.progress(3, 7)).join().unwrap();
        assert_eq!((a.list()[0].done, a.list()[0].total), (3, 7));
    }
}
