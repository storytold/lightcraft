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
//! A job that says how it ended ([`TaskGuard::finish`]) leaves a row behind when there is something to come back
//! to: it ran long enough to walk away from, or it did not go well. That row stays until it is dismissed
//! (`activity.finished`, `activity.dismiss`), for this session. A guard dropped by a panic leaves "stopped
//! unexpectedly" instead of nothing.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

/// The longest label or detail kept, in characters (a file name can be anything).
const MAX_TEXT: usize = 200;
/// The longest sentence kept of how a task ended, or of why a file failed.
const MAX_SENTENCE: usize = 400;
/// A task that ended well leaves a row once it has run this long ([`Activity::keep_after`]); a shorter one was
/// watched to its end.
pub const KEEP_AFTER_MS: u64 = 10_000;
/// The most tasks that ended well kept: the oldest give way.
const MAX_DONE: usize = 50;
/// The most ended tasks kept in all. Those that did not go well are kept past [`MAX_DONE`], up to here.
const MAX_FINISHED: usize = 200;
/// The most files a task keeps the reason of ([`Finished::failed`] still counts them all).
pub const MAX_FAILURES: usize = 500;
/// The longest path kept of where a task's results are.
const MAX_PATH: usize = 4096;

/// The background tasks in flight. Cheap to clone (one `Arc`), so worker threads can hold it.
#[derive(Clone, Default)]
pub struct Activity {
    inner: Arc<Inner>,
}

struct Inner {
    /// Oldest first.
    tasks: Mutex<Vec<Arc<Entry>>>,
    /// The tasks that ended and left a row, oldest first.
    finished: Mutex<Vec<Kept>>,
    keep_after_ms: AtomicU64,
    next: AtomicU64,
}

impl Default for Inner {
    fn default() -> Self {
        Inner { tasks: Mutex::default(), finished: Mutex::default(), keep_after_ms: AtomicU64::new(KEEP_AFTER_MS), next: AtomicU64::new(0) }
    }
}

/// How a task ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Ending {
    /// It did all it was asked to.
    #[default]
    Done,
    /// It ran to its end, but not for every file or photo.
    Problems,
    /// It could not do its work.
    Failed,
    /// It was stopped: ✕, `activity.cancel`, quitting.
    Stopped,
    /// Its worker died (a bug): the guard was dropped by a panic.
    Died,
}

impl Ending {
    /// Whether the person should be told: everything but a clean end.
    pub fn needs_attention(self) -> bool {
        self != Ending::Done
    }
}

/// A file or photo a task could not do, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub name: String,
    pub reason: String,
}

/// What a job says of itself when it ends ([`TaskGuard::finish`]).
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub ending: Ending,
    /// What happened, as the frontend shows it ("Exported 24 of 25 photos"). Frontend code passes it translated;
    /// engine code leaves it empty, and a frontend words the ending from the task's label.
    pub summary: String,
    /// What else needs reading: what was skipped, or the reason it failed.
    pub detail: String,
    /// The files or photos it could not do. The first [`MAX_FAILURES`] are kept.
    pub failures: Vec<Failure>,
    /// How many it could not do, for a job that counts them without naming them (`failures` counts when it holds
    /// more).
    pub failed: usize,
    /// One of the files it made (or the folder they are in), for showing where they are in the file manager.
    pub path: Option<String>,
}

impl Outcome {
    /// An end that says nothing more than how it went (the frontend words it from the task's name).
    pub fn ended(ending: Ending) -> Self {
        Outcome { ending, ..Default::default() }
    }

    /// The job could not do its work, and this is why.
    pub fn failed(why: impl Into<String>) -> Self {
        Outcome { ending: Ending::Failed, detail: why.into(), ..Default::default() }
    }
}

/// A task that ended, as the frontends see it (`activity.finished`), until it is dismissed.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finished {
    /// The id it had while it ran.
    pub id: u64,
    pub kind: &'static str,
    /// English; the frontend translates it.
    pub label: String,
    pub ending: Ending,
    pub summary: String,
    pub detail: String,
    pub failures: Vec<Failure>,
    /// How many files or photos failed (`failures` may hold fewer, or none when the job only counted them).
    pub failed: usize,
    /// One of the files it made (or their folder), if it made any.
    pub path: Option<String>,
    /// How long it ran.
    pub ran_ms: u64,
    /// How long ago it ended.
    pub age_ms: u64,
}

struct Kept {
    row: Finished,
    ended: web_time::Instant,
}

fn millis(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn sentence(text: &str) -> String {
    text.chars().take(MAX_SENTENCE).collect()
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

/// One task as the frontends see it (`activity.list`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    pub id: u64,
    /// A stable id: `import`, `export`, `scan`, `previews`, `smartPreviews`, `lightroom`, `merge`, `download`,
    /// `faces`, `denoise`, `findMissing`, `applePhotos`, `cache`, `focus`.
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
}

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
        self.done.store(if total > 0 { done.min(total) } else { done }, Ordering::Relaxed);
    }

    fn detail(&self, text: &str) {
        *lock(&self.detail) = capped(text);
    }

    fn info(&self) -> TaskInfo {
        TaskInfo {
            id: self.id,
            kind: self.kind,
            label: self.label.clone(),
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            unit: Unit::from_u8(self.unit.load(Ordering::Relaxed)),
            detail: lock(&self.detail).clone(),
            cancellable: self.is_cancellable(),
            cancelling: self.is_cancelled(),
            age_ms: millis(self.started.elapsed()),
        }
    }
}

impl Activity {
    /// Register a task; it is listed until the returned guard is dropped. `label` is English (the frontend
    /// translates it).
    pub fn start(&self, kind: &'static str, label: &str, cancel: Cancel) -> TaskGuard {
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
        });
        lock(&self.inner.tasks).push(entry.clone());
        TaskGuard { activity: self.clone(), entry, reported: false }
    }

    /// The tasks that ended and left a row, newest first.
    pub fn finished(&self) -> Vec<Finished> {
        lock(&self.inner.finished).iter().rev().map(|k| Finished { age_ms: millis(k.ended.elapsed()), ..k.row.clone() }).collect()
    }

    /// Let go of the row of the ended task `id`. An unknown id (it may have been dismissed already) is an error.
    pub fn dismiss(&self, id: u64) -> Result<(), String> {
        let mut finished = lock(&self.inner.finished);
        let at = finished.iter().position(|k| k.row.id == id).ok_or_else(|| format!("no finished task {id}"))?;
        finished.remove(at);
        Ok(())
    }

    /// Let go of every ended task's row; returns how many there were.
    pub fn dismiss_all(&self) -> usize {
        let mut finished = lock(&self.inner.finished);
        let n = finished.len();
        finished.clear();
        n
    }

    /// How long a task has to run for a clean end to leave a row ([`KEEP_AFTER_MS`] unless set).
    pub fn keep_after(&self, ms: u64) {
        self.inner.keep_after_ms.store(ms, Ordering::Relaxed);
    }

    /// Keep the row of a task that ended, and make room: of those that ended well the oldest give way first.
    fn keep(&self, row: Finished) {
        let mut finished = lock(&self.inner.finished);
        finished.push(Kept { row, ended: web_time::Instant::now() });
        while finished.iter().filter(|k| k.row.ending == Ending::Done).count() > MAX_DONE {
            let Some(at) = finished.iter().position(|k| k.row.ending == Ending::Done) else { break };
            finished.remove(at);
        }
        while finished.len() > MAX_FINISHED {
            // (a clean one goes before one that did not go well, as long as there is one)
            let at = finished.iter().position(|k| k.row.ending == Ending::Done).unwrap_or(0);
            finished.remove(at);
        }
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
        if entry.try_cancel() { Ok(()) } else { Err(format!("{} can't be cancelled", entry.label)) }
    }

    /// Ask every cancellable task to stop; returns how many were asked.
    pub fn cancel_all(&self) -> usize {
        let tasks = lock(&self.inner.tasks);
        tasks.iter().filter(|e| e.try_cancel()).count()
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
    /// The job said how it ended ([`TaskGuard::finish`]).
    reported: bool,
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

    /// A step that must not be cut short (the Lightroom import's commit) turns cancelling off for its duration.
    pub fn set_cancellable(&self, yes: bool) {
        self.entry.set_cancellable(yes);
    }

    pub fn cancel_flag(&self) -> Option<Arc<AtomicBool>> {
        self.entry.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.entry.is_cancelled()
    }

    /// For progress from another thread; it does not keep the row.
    pub fn handle(&self) -> TaskHandle {
        TaskHandle { entry: self.entry.clone() }
    }

    /// The job ended, and this is how. Its row among the running tasks goes; a row among the finished ones takes
    /// its place when there is something to come back to: the task ran for [`KEEP_AFTER_MS`] or more, or it did
    /// not end well. Returns whether that row was kept. A frontend still says at once how the job went (a toast):
    /// the row is what stays, not the announcement.
    pub fn finish(mut self, outcome: Outcome) -> bool {
        self.report(outcome)
    }

    /// Say how the job ended, as [`TaskGuard::finish`] does, but keep its running row until the guard is dropped: for a
    /// job that marks itself finished in between, so that whoever sees the row go finds it finished, and whoever finds
    /// it finished finds the row. Only the first report counts.
    pub fn report(&mut self, outcome: Outcome) -> bool {
        if std::mem::replace(&mut self.reported, true) {
            return false;
        }
        self.leave(outcome)
    }

    fn leave(&self, outcome: Outcome) -> bool {
        let ran_ms = millis(self.entry.started.elapsed());
        if !outcome.ending.needs_attention() && ran_ms < self.activity.inner.keep_after_ms.load(Ordering::Relaxed) {
            return false;
        }
        let failed = outcome.failed.max(outcome.failures.len());
        self.activity.keep(Finished {
            id: self.entry.id,
            kind: self.entry.kind,
            label: self.entry.label.clone(),
            ending: outcome.ending,
            summary: sentence(&outcome.summary),
            detail: sentence(&outcome.detail),
            failures: outcome
                .failures
                .into_iter()
                .take(MAX_FAILURES)
                .map(|f| Failure { name: capped(&f.name), reason: sentence(&f.reason) })
                .collect(),
            failed,
            path: outcome.path.map(|path| path.chars().take(MAX_PATH).collect()),
            ran_ms,
            age_ms: 0,
        });
        true
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let id = self.entry.id;
        lock(&self.activity.inner.tasks).retain(|e| e.id != id);
        // a worker that died never said how it ended: say that it stopped, rather than nothing
        if !self.reported && std::thread::panicking() {
            self.leave(Outcome { ending: Ending::Died, ..Default::default() });
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

    /// How many ended tasks did not end well.
    fn problems(a: &Activity) -> usize {
        a.finished().iter().filter(|f| f.ending.needs_attention()).count()
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
    fn a_long_task_that_ends_well_leaves_a_row_and_a_short_one_does_not() {
        let a = Activity::default();
        // Given a task that ran for less than it takes to walk away
        let quick = a.start("export", "Exporting", Cancel::Yes);
        // When it ends well, Then nothing is kept: it was watched to its end
        assert!(!quick.finish(Outcome { summary: "Exported 2 photos".into(), ..Default::default() }));
        assert!(a.finished().is_empty() && a.list().is_empty());
        // Given a task that ran long enough (here: any time at all)
        a.keep_after(0);
        let long = a.start("export", "Exporting", Cancel::Yes);
        let id = long.id();
        assert!(long.finish(Outcome { summary: "Exported 5,000 photos".into(), path: Some("/out/a.jpg".into()), ..Default::default() }));
        // Then its row among the running tasks is gone, and one among the finished took its place
        assert!(a.list().is_empty());
        let rows = a.finished();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].id, rows[0].kind, rows[0].ending, rows[0].summary.as_str()), (id, "export", Ending::Done, "Exported 5,000 photos"));
        assert_eq!(rows[0].path.as_deref(), Some("/out/a.jpg"));
        assert_eq!(a.finished().first().map(|f| (f.id, f.summary.clone())), Some((id, "Exported 5,000 photos".to_string())));
        // And a clean end asks for nobody's attention
        assert_eq!(problems(&a), 0);
    }

    #[test]
    fn a_task_that_did_not_end_well_always_leaves_a_row_and_asks_for_attention_until_dismissed() {
        let a = Activity::default();
        for (ending, n) in [(Ending::Problems, 1), (Ending::Failed, 2), (Ending::Stopped, 3)] {
            let g = a.start("import", "Importing", Cancel::Yes);
            assert!(g.finish(Outcome { ending, ..Default::default() }), "{ending:?} is kept however short the task");
            assert_eq!(problems(&a), n);
        }
        // newest first
        let rows = a.finished();
        assert_eq!(rows.iter().map(|f| f.ending).collect::<Vec<_>>(), vec![Ending::Stopped, Ending::Failed, Ending::Problems]);
        // each asks until it is dismissed, not until it has been looked at
        a.dismiss(rows[1].id).unwrap();
        assert_eq!(problems(&a), 2);
        a.dismiss_all();
        assert_eq!(problems(&a), 0);
    }

    #[test]
    fn finished_rows_are_dismissed_one_by_one_or_all_at_once() {
        let a = Activity::default();
        let ids: Vec<u64> = (0..3)
            .map(|_| {
                let g = a.start("export", "Exporting", Cancel::Yes);
                let id = g.id();
                g.finish(Outcome { ending: Ending::Failed, ..Default::default() });
                id
            })
            .collect();
        a.dismiss(ids[1]).unwrap();
        assert_eq!(a.finished().iter().map(|f| f.id).collect::<Vec<_>>(), vec![ids[2], ids[0]]);
        assert!(a.dismiss(ids[1]).is_err(), "an error, not a panic, for a row already gone");
        assert_eq!(a.dismiss_all(), 2);
        assert!(a.finished().is_empty());
        assert_eq!(a.dismiss_all(), 0);
    }

    #[test]
    fn what_is_kept_of_an_ending_is_bounded() {
        let a = Activity::default();
        a.keep_after(0);
        // a job with more failures than are kept, long names and long reasons
        let g = a.start("export", "Exporting", Cancel::Yes);
        let failures: Vec<Failure> =
            (0..MAX_FAILURES + 40).map(|i| Failure { name: format!("{i}{}", "é".repeat(300)), reason: "x".repeat(1000) }).collect();
        g.finish(Outcome {
            ending: Ending::Problems,
            summary: "s".repeat(5000),
            detail: "d".repeat(5000),
            failures,
            path: Some("p".repeat(9000)),
            ..Default::default()
        });
        let row = &a.finished()[0];
        assert_eq!((row.failed, row.failures.len()), (MAX_FAILURES + 40, MAX_FAILURES), "all counted, the first kept");
        assert_eq!((row.summary.chars().count(), row.detail.chars().count()), (MAX_SENTENCE, MAX_SENTENCE));
        assert_eq!(row.path.as_ref().map(|p| p.chars().count()), Some(MAX_PATH));
        // a job that only counts what failed (an import) is believed
        let counted = a.start("import", "Importing", Cancel::Yes);
        counted.finish(Outcome { ending: Ending::Problems, failed: 3, ..Default::default() });
        assert_eq!((a.finished()[0].failed, a.finished()[0].failures.len()), (3, 0));
        a.dismiss(a.finished()[0].id).unwrap();
        assert!(row.failures.iter().all(|f| f.name.chars().count() == MAX_TEXT && f.reason.chars().count() == MAX_SENTENCE));
        // many clean endings: the oldest give way, the one that did not go well stays
        for i in 0..MAX_DONE + 10 {
            a.start("export", &format!("Task {i}"), Cancel::Yes).finish(Outcome::default());
        }
        let rows = a.finished();
        assert_eq!(rows.iter().filter(|f| f.ending == Ending::Done).count(), MAX_DONE);
        assert_eq!(rows.first().map(|f| f.label.as_str()), Some(format!("Task {}", MAX_DONE + 9).as_str()), "the newest is kept");
        assert!(rows.iter().any(|f| f.ending == Ending::Problems), "a row that needs looking at is not pushed out by clean ones");
        // and never without end: rows that did not go well are bounded too
        for _ in 0..MAX_FINISHED + 20 {
            a.start("import", "Importing", Cancel::Yes).finish(Outcome { ending: Ending::Failed, ..Default::default() });
        }
        assert_eq!(a.finished().len(), MAX_FINISHED);
    }

    #[test]
    fn a_worker_that_dies_leaves_a_row_saying_so() {
        let a = Activity::default();
        let a2 = a.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = a2.start("previews", "Building previews", Cancel::Yes);
            panic!("synthetic worker panic");
        }));
        assert!(r.is_err());
        assert!(a.list().is_empty(), "no stuck bar");
        let rows = a.finished();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].ending), ("previews", "Building previews", Ending::Died));
        assert_eq!(problems(&a), 1);
        // a guard that is simply dropped (a job that does not report yet) leaves nothing, as before
        drop(a.start("faces", "Finding faces", Cancel::No));
        assert_eq!(a.finished().len(), 1);
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
    fn ids_are_unique_and_list_is_oldest_first() {
        let a = Activity::default();
        let (g1, g2) = (a.start("export", "A", Cancel::Yes), a.start("import", "B", Cancel::Yes));
        assert!(g2.id() > g1.id());
        assert_eq!(a.list().iter().map(|t| t.id).collect::<Vec<_>>(), vec![g1.id(), g2.id()]);
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

    #[test]
    fn handle_updates_from_another_thread() {
        let a = Activity::default();
        let g = a.start("previews", "Building previews", Cancel::Yes);
        let h = g.handle();
        std::thread::spawn(move || h.progress(3, 7)).join().unwrap();
        assert_eq!((a.list()[0].done, a.list()[0].total), (3, 7));
    }
}
