//! The file reads of an import (or a scan) that can stop answering (LR-BEHAV-BG-TASKS).
//!
//! **Not used yet:** this is the tested core. The import, the scans and Synchronize Folder move onto it in the
//! changes that follow (✕ ending them at once, then the activity row naming the stalled file and offering Skip), so
//! nothing outside this module's tests calls [`Pool::run`] today.
//!
//! A network drive that drops its connection leaves a read blocked inside the operating system, and a blocked read
//! cannot be interrupted. An import that waited for every read (as a scope of threads does: it cannot end before each
//! thread has) hung with it, and the only way out was to quit. Here the reads run on detached threads that pull their
//! jobs from a shared queue ([`Pool::run`]), and the calling thread only coordinates. It looks every
//! 250 ms at:
//!
//! - **✕:** the run stops at once. Jobs under way are given [`CANCEL_GRACE`] to finish (what they made is kept, as
//!   before), then abandoned; jobs still queued are never started.
//! - **A stall:** a job that has made no progress for [`ReadWatch::stall_after`] is published
//!   ([`ReadWatch::stalled`]), for the activity row to name ("Waiting for IMG_0457.jpg · 14 s"). Progress is a byte
//!   read or written, or the end of a step: work that takes long without waiting on the disk (queueing for memory,
//!   encoding a DNG) says so with [`progressed`], so a healthy slow job is never reported.
//! - **Skip:** [`ReadWatch::skip`] gives up the one read the row names (the longest without progress; the next is
//!   named afterwards). It is reported as failed ("skipped: no answer after N s"), its thread is abandoned (it stays
//!   blocked until the system lets it go, and its late result is dropped, never applied; it keeps its memory permit
//!   until then) and a fresh thread takes its place, so the run goes on with the same number of readers.
//! - **A drive that is gone:** the [`MAX_SKIPPED`]th skipped read ends the run with an error ([`ReadWatch::ended`])
//!   instead of leaving yet another thread blocked.
//!
//! - **Earlier runs:** an abandoned read stays blocked after its run is over, and the next run builds a new pool. The
//!   abandoned threads still alive are counted across runs ([`StuckReaders`], [`MAX_STUCK_READERS`]), whichever drive
//!   they wait on; once that many are blocked, a run is not started (and a Skip starts no replacement reader) and the
//!   run ends with an error saying so, so repeated cancel and restart cannot pile up blocked threads. The count is
//!   looked at when a run starts and after each Skip; ✕ can still abandon the readers of the runs under way (at most
//!   [`crate::import::workers`] per pool, and Synchronize Folder runs two) on top, so the bound is the cap plus that.
//!
//! Several pools can run at once under one [`ReadWatch`] (Synchronize Folder probes the new files while it checks
//! the library's photos): the watch names the longest stall of all, and Skip acts on the pool that has it.

// (no caller outside the tests yet, see above: the tests use all of it, so only the non-test build allows it. On wasm32
// the pool is not built at all, which the follow-up keeps allowing)
#![cfg_attr(not(test), allow(dead_code))]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use web_time::Instant;

/// A read that has made no progress for this long is reported as stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(10);
/// The skipped reads that end a run with an error: each leaves a thread blocked until the system lets it go.
pub const MAX_SKIPPED: usize = 4;
/// The abandoned reads that keep a new run from starting, over all runs of the process (eight runs' worth of
/// [`MAX_SKIPPED`]). It is looked at when a run starts and after a Skip, and ✕ can abandon the readers of the runs
/// under way on top: the blocked threads stay bounded by this plus a few runs' readers, not unbounded.
pub const MAX_STUCK_READERS: usize = 32;
/// How often the coordinating thread looks at ✕, Skip and the readers.
const POLL: Duration = Duration::from_millis(250);
/// Once ✕ is pressed, how long jobs under way get to finish before they are abandoned.
const CANCEL_GRACE: Duration = Duration::from_millis(400);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many abandoned reader threads are still alive (blocked in the operating system), shared by every run that
/// uses it. A thread counts from the moment it is abandoned until it truly exits, however long after its run ended.
#[derive(Debug)]
pub struct StuckReaders {
    alive: AtomicUsize,
    cap: usize,
}

impl StuckReaders {
    pub fn new(cap: usize) -> Self {
        StuckReaders { alive: AtomicUsize::new(0), cap }
    }

    /// The process-wide count: the session will start with it, once the readers are wired in.
    pub fn global() -> std::sync::Arc<StuckReaders> {
        static GLOBAL: std::sync::OnceLock<std::sync::Arc<StuckReaders>> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| std::sync::Arc::new(StuckReaders::new(MAX_STUCK_READERS))).clone()
    }

    /// Abandoned readers still blocked.
    pub fn alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }

    fn full(&self) -> bool {
        self.alive() >= self.cap
    }
}

/// A read that has gone without progress ([`ReadWatch::stalled`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StalledRead {
    /// The file's name.
    pub what: String,
    /// How long it has been without progress.
    pub waited: Duration,
}

/// What the reads of an import or scan are doing, shared between the run (which publishes it) and whoever shows it
/// and offers Skip. Everything is behind `&self`: hand out clones of the `Arc`.
#[derive(Debug)]
pub struct ReadWatch {
    stall_after_ms: AtomicU64,
    poll_ms: AtomicU64,
    /// The longest stall of each running pool: (pool, the file's name, since when).
    stalls: Mutex<Vec<(u64, String, Instant)>>,
    pools: AtomicU64,
    /// Skip was asked for this read (its name and since when it stalled).
    skip: Mutex<Option<(String, Instant)>>,
    skipped: AtomicUsize,
    ended: Mutex<Option<String>>,
    stuck: std::sync::Arc<StuckReaders>,
}

impl Default for ReadWatch {
    fn default() -> Self {
        ReadWatch {
            stall_after_ms: AtomicU64::new(millis(STALL_AFTER)),
            poll_ms: AtomicU64::new(millis(POLL)),
            stalls: Mutex::new(Vec::new()),
            pools: AtomicU64::new(0),
            skip: Mutex::new(None),
            skipped: AtomicUsize::new(0),
            ended: Mutex::new(None),
            stuck: StuckReaders::global(),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl ReadWatch {
    /// A watch whose runs count their abandoned readers in `stuck` (the session's, once the readers are wired in).
    pub fn with_stuck(after: Duration, stuck: std::sync::Arc<StuckReaders>) -> Self {
        ReadWatch { stuck, ..ReadWatch::new(after) }
    }

    /// A watch that calls a read stalled after `after` ([`STALL_AFTER`] by default).
    pub fn new(after: Duration) -> Self {
        let watch = ReadWatch::default();
        watch.set_stall_after(after);
        watch
    }

    /// How long a read goes without progress before it counts as stalled.
    pub fn stall_after(&self) -> Duration {
        Duration::from_millis(self.stall_after_ms.load(Ordering::Relaxed))
    }

    pub fn set_stall_after(&self, after: Duration) {
        self.stall_after_ms.store(millis(after), Ordering::Relaxed);
    }

    /// How often the reads are looked at, for tests that cannot wait a quarter of a second.
    #[cfg(test)]
    pub(crate) fn set_poll(&self, every: Duration) {
        self.poll_ms.store(millis(every).max(1), Ordering::Relaxed);
    }

    fn poll(&self) -> Duration {
        Duration::from_millis(self.poll_ms.load(Ordering::Relaxed))
    }

    /// The read that has gone longest without progress, if one has for [`ReadWatch::stall_after`].
    pub fn stalled(&self) -> Option<StalledRead> {
        let stalls = lock(&self.stalls);
        stalls.iter().min_by_key(|(_, _, since)| *since).map(|(_, what, since)| StalledRead { what: what.clone(), waited: since.elapsed() })
    }

    fn register_pool(&self) -> u64 {
        self.pools.fetch_add(1, Ordering::Relaxed)
    }

    /// Say which read of `pool` has stalled the longest (`None`: none has).
    fn publish(&self, pool: u64, stalled: Option<(String, Instant)>) {
        let mut stalls = lock(&self.stalls);
        stalls.retain(|(p, ..)| *p != pool);
        if let Some((what, since)) = stalled {
            stalls.push((pool, what, since));
        }
        // a Skip is for the read that was named: one that answered meanwhile (or was given up) takes it with it
        let mut asked = lock(&self.skip);
        if asked.as_ref().is_some_and(|(what, since)| !stalls.iter().any(|(_, w, s)| w == what && s == since)) {
            *asked = None;
        }
    }

    /// Give up the read that is named ([`ReadWatch::stalled`]), at the next look: only that read, and only while it is
    /// still the one named (a Skip for a read that answered meanwhile does nothing). Nothing happens when no read is
    /// stalled.
    pub fn skip(&self) {
        let stalls = lock(&self.stalls);
        *lock(&self.skip) = stalls.iter().min_by_key(|(_, _, since)| *since).map(|(_, what, since)| (what.clone(), *since));
    }

    /// Whether Skip was asked for the read `shown` (the one the row named when it was asked) and that read is the one
    /// named now, which is `pool`'s; asking clears it.
    fn take_skip_for(&self, pool: u64, shown: &(String, Instant)) -> bool {
        let stalls = lock(&self.stalls);
        let named_here =
            stalls.iter().min_by_key(|(_, _, since)| *since).is_some_and(|(p, what, since)| *p == pool && (what, since) == (&shown.0, &shown.1));
        named_here && lock(&self.skip).take().is_some_and(|asked| asked == *shown)
    }

    /// How many reads were given up by Skip so far.
    pub fn skipped(&self) -> usize {
        self.skipped.load(Ordering::Relaxed)
    }

    /// Why the run cannot go on, once it cannot: too many reads were skipped, or a reader could not be started.
    pub fn ended(&self) -> Option<String> {
        lock(&self.ended).clone()
    }

    fn blocked_elsewhere(&self) -> String {
        format!(
            "a drive is not answering: {} reads that were given up are still waiting for it (try again later, or restart LightCraft)",
            self.stuck.alive()
        )
    }

    fn end(&self, why: String) {
        lock(&self.ended).get_or_insert(why);
    }
}

/// How one job of a [`Pool::run`] came out.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(super) enum Outcome<R> {
    Done(R),
    /// It was skipped, or its worker panicked: why.
    Failed(String),
    /// Never started, or cut off before it finished: by ✕, or because the run ended (too many reads skipped, too many
    /// given-up reads still blocked, or a reader that could not be started).
    Unreached,
}

/// Where one run of reads happens ([`Pool::run`]).
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct Pool<'a> {
    pub watch: &'a ReadWatch,
    /// ✕: set from outside (the activity row's, or the run's own).
    pub cancel: &'a AtomicBool,
    /// Thread names: `<name>-<n>`.
    pub name: &'static str,
    /// Reads at once.
    pub threads: usize,
}

/// A thread of the pool. It is *abandoned* when its job is given up: it never takes another job, and what it
/// finishes with goes to the `late` handler.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Worker {
    /// The job it is on (its index) and when it last made progress.
    busy: Mutex<Option<(usize, Instant)>>,
    abandoned: std::sync::Arc<AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// The pool thread this is, for [`progressed`].
    static CURRENT: std::cell::RefCell<Option<std::sync::Arc<Worker>>> = const { std::cell::RefCell::new(None) };
}

/// The abandon flag of the pool thread this is (`None` on any other thread), for the memory gate to tell the permits of
/// reads that were given up from the others.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn abandon_flag() -> Option<std::sync::Arc<AtomicBool>> {
    CURRENT.with(|current| current.borrow().as_ref().map(|worker| worker.abandoned.clone()))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn abandon_flag() -> Option<std::sync::Arc<AtomicBool>> {
    None
}

/// The job on this thread has made progress (a chunk read or written, a wait for memory over, a step done): its
/// stall clock starts again. Does nothing on a thread that is not a pool's.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn progressed() {
    CURRENT.with(|current| {
        if let Some(worker) = current.borrow().as_ref()
            && let Some(busy) = lock(&worker.busy).as_mut()
        {
            busy.1 = Instant::now();
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn progressed() {}

type Message<R> = (usize, Result<R, String>);

/// Where the workers hand in what they finished, until the run is over: a late hand-in is refused, so that its sender
/// disposes of it.
#[cfg(not(target_arch = "wasm32"))]
struct Inbox<R>(Mutex<Option<std::sync::mpsc::Sender<Message<R>>>>);

#[cfg(not(target_arch = "wasm32"))]
impl<R> Inbox<R> {
    fn hand_in(&self, message: Message<R>) -> Result<(), Message<R>> {
        match lock(&self.0).as_ref() {
            Some(tx) => tx.send(message).map_err(|e| e.0),
            None => Err(message),
        }
    }

    fn close(&self) {
        *lock(&self.0) = None;
    }
}

#[cfg(not(target_arch = "wasm32"))]
type Work<J, R> = std::sync::Arc<dyn Fn(J) -> R + Send + Sync>;

/// What every thread of one run shares.
#[cfg(not(target_arch = "wasm32"))]
struct Shared<J, R> {
    jobs: Mutex<std::sync::mpsc::Receiver<(usize, J)>>,
    inbox: Inbox<R>,
    work: Work<J, R>,
    late: std::sync::Arc<dyn Fn(R) + Send + Sync>,
    init: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// The run is over: a job still queued is not started.
    over: AtomicBool,
    stuck: std::sync::Arc<StuckReaders>,
}

/// Gives up the thread's place in the [`StuckReaders`] count when it exits, if it was abandoned.
#[cfg(not(target_arch = "wasm32"))]
struct Exit(std::sync::Arc<Worker>, std::sync::Arc<StuckReaders>);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Exit {
    fn drop(&mut self) {
        if self.0.abandoned.load(Ordering::SeqCst) {
            self.1.alive.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Abandon `w` (its job is given up), counting it until its thread exits. Call with its `busy` lock held and `Some`.
#[cfg(not(target_arch = "wasm32"))]
fn abandon(w: &Worker, stuck: &StuckReaders) {
    if !w.abandoned.swap(true, Ordering::SeqCst) {
        stuck.alive.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker_loop<J: Send + 'static, R: Send + 'static>(me: std::sync::Arc<Worker>, shared: std::sync::Arc<Shared<J, R>>) {
    let _exit = Exit(me.clone(), shared.stuck.clone());
    CURRENT.with(|current| *current.borrow_mut() = Some(me.clone()));
    // (a reader that cannot be set up still takes jobs: one that died here would be handed jobs it never takes)
    if let Err(why) = crate::guard::catch("import", || (shared.init)()) {
        log::warn!("import: a reader's set-up failed: {why}");
    }
    loop {
        // (the lock is held while waiting: the other idle workers queue behind it, and a worker blocked in a read does
        // not hold it)
        let next = lock(&shared.jobs).recv();
        let Ok((i, job)) = next else { return };
        // (checked and claimed under one lock: the end-of-run sweep sets `over` first, so it either sees this job and
        // abandons the thread, or the thread sees `over` and never starts it)
        {
            let mut busy = lock(&me.busy);
            if shared.over.load(Ordering::SeqCst) || me.abandoned.load(Ordering::SeqCst) {
                return;
            }
            *busy = Some((i, Instant::now()));
        }
        let result = crate::guard::catch("import", || (shared.work)(job));
        // decided under the lock the coordinator abandons under, so that it never abandons the next job by mistake
        let abandoned = {
            let mut busy = lock(&me.busy);
            *busy = None;
            me.abandoned.load(Ordering::SeqCst)
        };
        if abandoned {
            if let Ok(r) = result {
                (shared.late)(r);
            }
            return;
        }
        if let Err((_, Ok(r))) = shared.inbox.hand_in((i, result)) {
            (shared.late)(r);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Pool<'_> {
    fn halted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.watch.ended().is_some()
    }

    /// Run `jobs` on [`Pool::threads`] detached threads; the outcome of each, in order. `label` names job `i` for the
    /// row; `work` does one; `late` disposes of a result that arrives after its job was given up (nothing may be
    /// applied: a copy that was made is taken back); `init` runs first on each new thread; `on_done` counts each job
    /// settled (`None`: failed).
    ///
    /// Jobs are handed out by this thread, no more than there are readers at a time, so that ✕ stops the run
    /// within a file or two. If no thread can be started the jobs run here, one after another.
    pub(super) fn run<J: Send + 'static, R: Send + 'static>(
        &self,
        jobs: Vec<J>,
        label: &dyn Fn(usize) -> String,
        work: Work<J, R>,
        late: std::sync::Arc<dyn Fn(R) + Send + Sync>,
        init: std::sync::Arc<dyn Fn() + Send + Sync>,
        on_done: &dyn Fn(Option<&R>),
    ) -> Vec<Outcome<R>> {
        use std::sync::Arc;
        use std::sync::mpsc;
        let total = jobs.len();
        let mut outcomes: Vec<Outcome<R>> = (0..total).map(|_| Outcome::Unreached).collect();
        if total == 0 || self.halted() {
            return outcomes;
        }
        if self.watch.stuck.full() {
            self.watch.end(self.watch.blocked_elsewhere());
            return outcomes;
        }
        let pool = self.watch.register_pool();
        let (job_tx, job_rx) = mpsc::channel::<(usize, J)>();
        let (result_tx, result_rx) = mpsc::channel::<Message<R>>();
        let shared = Arc::new(Shared {
            jobs: Mutex::new(job_rx),
            inbox: Inbox(Mutex::new(Some(result_tx))),
            work: work.clone(),
            late: late.clone(),
            init,
            over: AtomicBool::new(false),
            stuck: self.watch.stuck.clone(),
        });
        let start = |n: usize| -> std::io::Result<Arc<Worker>> {
            let me = Arc::new(Worker::default());
            let (mine, shared) = (me.clone(), shared.clone());
            std::thread::Builder::new().name(format!("{}-{n}", self.name)).spawn(move || worker_loop(mine, shared))?;
            Ok(me)
        };
        let mut live: Vec<Arc<Worker>> = Vec::new();
        for n in 0..self.threads.clamp(1, total) {
            match start(n) {
                Ok(w) => live.push(w),
                Err(e) => {
                    log::warn!("import: could not start a reader: {e}");
                    break;
                }
            }
        }
        if live.is_empty() {
            // no thread to be had: read here, one after another
            for (job, slot) in jobs.into_iter().zip(outcomes.iter_mut()) {
                if self.halted() {
                    break;
                }
                *slot = match crate::guard::catch("import", || work(job)) {
                    Ok(r) => {
                        on_done(Some(&r));
                        Outcome::Done(r)
                    }
                    Err(e) => {
                        on_done(None);
                        Outcome::Failed(e)
                    }
                };
            }
            return outcomes;
        }
        let mut jobs: Vec<Option<J>> = jobs.into_iter().map(Some).collect();
        let mut started = live.len();
        let (mut next, mut outstanding) = (0usize, 0usize);
        let mut halted_at: Option<Instant> = None;
        let settle = |outcomes: &mut Vec<Outcome<R>>, outstanding: &mut usize, (i, result): Message<R>| {
            let Some(slot) = outcomes.get_mut(i) else { return };
            if !matches!(slot, Outcome::Unreached) {
                // its job was given up already: this is the late answer
                if let Ok(r) = result {
                    late(r);
                }
                return;
            }
            *outstanding = outstanding.saturating_sub(1);
            *slot = match result {
                Ok(r) => {
                    on_done(Some(&r));
                    Outcome::Done(r)
                }
                Err(e) => {
                    on_done(None);
                    Outcome::Failed(e)
                }
            };
        };
        loop {
            if halted_at.is_none() && self.halted() {
                halted_at = Some(Instant::now());
            }
            if halted_at.is_none() {
                while outstanding < live.len() && next < total {
                    let Some(job) = jobs.get_mut(next).and_then(Option::take) else {
                        next += 1;
                        continue;
                    };
                    if job_tx.send((next, job)).is_err() {
                        self.watch.end("the readers stopped unexpectedly".into());
                        break;
                    }
                    next += 1;
                    outstanding += 1;
                }
            }
            if outstanding == 0 && (halted_at.is_some() || next >= total) {
                break;
            }
            if let Some(at) = halted_at
                && (self.watch.ended().is_some() || at.elapsed() >= CANCEL_GRACE)
            {
                break;
            }
            match result_rx.recv_timeout(self.watch.poll()) {
                Ok(message) => settle(&mut outcomes, &mut outstanding, message),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // jobs without progress: the longest is published for the row, and given up when Skip was asked for
            let after = self.watch.stall_after();
            let stalled = live
                .iter()
                .enumerate()
                .filter_map(|(k, w)| (*lock(&w.busy)).filter(|(_, since)| since.elapsed() >= after).map(|(i, since)| (k, i, since)))
                .min_by_key(|&(_, _, since)| since);
            self.watch.publish(pool, stalled.map(|(_, i, since)| (label(i), since)));
            if let Some((k, i, since)) = stalled
                && halted_at.is_none()
                && self.watch.take_skip_for(pool, &(label(i), since))
                && let Some(w) = live.get(k)
            {
                {
                    let busy = lock(&w.busy);
                    if *busy != Some((i, since)) {
                        continue; // it answered just now
                    }
                    abandon(w, &self.watch.stuck);
                }
                live.remove(k);
                let secs = since.elapsed().as_secs();
                if let Some(slot) = outcomes.get_mut(i) {
                    *slot = Outcome::Failed(format!("skipped: no answer after {secs} s"));
                }
                on_done(None);
                outstanding = outstanding.saturating_sub(1);
                let skipped = self.watch.skipped.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                if skipped >= MAX_SKIPPED {
                    self.watch.end(format!("the drive is not answering ({} is the last file it stopped on)", label(i)));
                } else if self.watch.stuck.full() {
                    self.watch.end(self.watch.blocked_elsewhere());
                } else {
                    match start(started) {
                        Ok(w) => {
                            started += 1;
                            live.push(w);
                        }
                        Err(e) => self.watch.end(format!("could not start a reader: {e}")),
                    }
                }
            }
        }
        // Over (or stopped): jobs still queued are not started, and the readers still at one are abandoned: they
        // dispose of their own results. What arrived already is taken, so nothing is lost between the last look and
        // here.
        shared.over.store(true, Ordering::SeqCst);
        for w in &live {
            let busy = lock(&w.busy);
            if busy.is_some() {
                abandon(w, &self.watch.stuck);
            }
        }
        drop(job_tx);
        shared.inbox.close();
        while let Ok(message) = result_rx.try_recv() {
            settle(&mut outcomes, &mut outstanding, message);
        }
        self.watch.publish(pool, None);
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Skip is for the read that was named when it was asked: once that read answered and another is named, the
    /// request does nothing.
    #[test]
    fn a_stale_skip_does_nothing() {
        let watch = ReadWatch::default();
        let old = Instant::now();
        watch.publish(0, Some(("a.jpg".into(), old)));
        watch.skip();
        // a.jpg answered; b.jpg stalled since
        watch.publish(0, Some(("b.jpg".into(), Instant::now())));
        assert!(watch.skip.lock().unwrap().is_none(), "the request went with the read it was for");
        assert!(!watch.take_skip_for(0, &("b.jpg".into(), Instant::now())));
        // a request for the read that is named is taken, once
        let now = Instant::now();
        watch.publish(0, Some(("b.jpg".into(), now)));
        watch.skip();
        assert!(watch.take_skip_for(0, &("b.jpg".into(), now)));
        assert!(!watch.take_skip_for(0, &("b.jpg".into(), now)));
        // nothing named: Skip asks for nothing
        watch.publish(0, None);
        watch.skip();
        assert!(watch.skip.lock().unwrap().is_none());
    }
}
