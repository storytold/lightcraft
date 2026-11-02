//! A prioritized job pool with per-slot de-duplication.
//!
//! Each job targets a *slot* (a grid cell, the loupe…). Submitting for a slot replaces a job for
//! that slot that hasn't started yet, so scrolling or dragging never builds a backlog. Workers
//! take the highest priority first (newest first among equals). Frontends re-prioritize or drop
//! queued jobs when the view changes (e.g. thumbnails scrolled out of view).
//!
//! Native: a fixed set of worker threads, started on first use. wasm32 (no threads): the host
//! calls [`JobPool::run_inline`] once per frame.
//!
//! A pool may keep a few workers for cheap jobs only ([`JobPool::with_fast_lane`]): the stand-ins
//! a view shows first (a raw's embedded JPEG, a cached thumbnail) never wait for a worker while
//! every other one is busy with a render that takes seconds.
//!
//! No worker outlives its pool by more than a deadline (issue #620: a render still inside the GPU
//! driver while the process exits crashes there): [`JobPool::shutdown`] and dropping the pool
//! discard the queued jobs and wait, bounded, for the running ones.

use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub type Job<R> = Box<dyn FnOnce() -> R + Send>;

/// What runs a background job ([`JobPool::run_background_with`]): it calls the closure once.
pub type BackgroundRun = fn(&mut dyn FnMut());

/// `e`, its job run inside the pool's [`BackgroundRun`] when it counts as `background` work.
fn for_background<S, R: 'static>(shared: &Shared<S, R>, e: Entry<S, R>, background: bool) -> Entry<S, R> {
    match shared.background_run.get().filter(|_| background) {
        Some(wrap) => {
            let (wrap, job) = (*wrap, e.job);
            Entry { job: Box::new(move || run_wrapped(wrap, job)), ..e }
        }
        None => e,
    }
}

/// Run `job` inside `wrap`.
fn run_wrapped<R>(wrap: BackgroundRun, job: Job<R>) -> R {
    let mut job = Some(job);
    let mut out = None;
    wrap(&mut || {
        if let Some(j) = job.take() {
            out = Some(j());
        }
    });
    match (out, job) {
        (Some(r), _) => r,
        // (the wrapper didn't call it)
        (None, Some(j)) => j(),
        // (run without a result: the job panicked and `wrap` swallowed it; the worker ends as it
        // would have without `wrap`, its slot freed by `Free`)
        (None, None) => std::panic::resume_unwind(Box::new("a background job panicked")),
    }
}

struct Entry<S, R> {
    slot: S,
    key: u64,
    priority: u32,
    seq: u64,
    job: Job<R>,
}

struct Shared<S, R> {
    queue: Mutex<Vec<Entry<S, R>>>,
    /// Slots with a job running: their next job waits until it ends, so a slot never has two at
    /// once (a slider drag submits faster than renders finish; overlapping renders of one view
    /// were mostly thrown away, and took the GPU from the one that is shown). Locked inside
    /// `queue`.
    busy: Mutex<Vec<S>>,
    cv: Condvar,
    shutdown: AtomicBool,
    /// Which slots' jobs are cheap: the fast lane's workers take only those.
    is_fast: Option<fn(&S) -> bool>,
    /// Background jobs (priority below `.0`) running at once at most (`.1`; see
    /// [`JobPool::cap_background`]), and how many run now (locked inside `queue`).
    background_cap: Mutex<(u32, usize)>,
    background_running: AtomicUsize,
    /// Runs each background job ([`JobPool::run_background_with`]).
    background_run: std::sync::OnceLock<BackgroundRun>,
    /// The worker threads that have not ended yet, and the signal that one did.
    #[cfg(not(target_arch = "wasm32"))]
    live: Mutex<Vec<std::thread::ThreadId>>,
    #[cfg(not(target_arch = "wasm32"))]
    ended: Condvar,
}

/// How long dropping a pool waits for the jobs still running (see [`JobPool::shutdown`]).
const DROP_WAIT: Duration = Duration::from_secs(2);

/// A finished job.
pub struct Done<S, R> {
    pub slot: S,
    /// The key the job was submitted with (to drop stale results).
    pub key: u64,
    pub result: R,
    /// Run time in milliseconds (0 on wasm).
    pub ms: f64,
}

pub struct JobPool<S, R> {
    shared: Arc<Shared<S, R>>,
    tx: Sender<Done<S, R>>,
    rx: Receiver<Done<S, R>>,
    /// A finished job [`Self::wait`] took off the channel, handed out first by [`Self::try_recv`].
    early: Option<Done<S, R>>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    threads: usize,
    /// Of `threads`, the workers that take only fast jobs ([`JobPool::with_fast_lane`]).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fast_threads: usize,
    started: bool,
    seq: u64,
    #[cfg(not(target_arch = "wasm32"))]
    workers: Vec<std::thread::JoinHandle<()>>,
    /// [`JobPool::shutdown`] ran: dropping the pool doesn't wait a second time.
    stopped: bool,
}

impl<S: Copy + Eq + Hash + Send + 'static, R: Send + 'static> JobPool<S, R> {
    /// A pool with `threads` workers (none on wasm; `0` = jobs only run via [`JobPool::run_inline`]).
    pub fn new(threads: usize) -> Self {
        Self::build(threads, 0, None)
    }

    /// A pool with `threads` workers for any job and `fast_threads` more that take only the jobs of
    /// slots `is_fast` names (cheap ones, e.g. a stand-in picture): those always find a worker,
    /// however long the other jobs run. The other workers take fast jobs too, by priority.
    pub fn with_fast_lane(threads: usize, fast_threads: usize, is_fast: fn(&S) -> bool) -> Self {
        Self::build(threads, fast_threads, Some(is_fast))
    }

    fn build(threads: usize, fast_threads: usize, is_fast: Option<fn(&S) -> bool>) -> Self {
        let fast_threads = if is_fast.is_some() { fast_threads } else { 0 };
        let (tx, rx) = channel();
        JobPool {
            shared: Arc::new(Shared {
                queue: Mutex::new(Vec::new()),
                busy: Mutex::new(Vec::new()),
                cv: Condvar::new(),
                shutdown: AtomicBool::new(false),
                is_fast,
                background_cap: Mutex::new((0, usize::MAX)),
                background_running: AtomicUsize::new(0),
                background_run: std::sync::OnceLock::new(),
                #[cfg(not(target_arch = "wasm32"))]
                live: Mutex::new(Vec::new()),
                #[cfg(not(target_arch = "wasm32"))]
                ended: Condvar::new(),
            }),
            tx,
            rx,
            early: None,
            threads,
            fast_threads,
            started: false,
            seq: 0,
            #[cfg(not(target_arch = "wasm32"))]
            workers: Vec::new(),
            stopped: false,
        }
    }

    /// Workers for this machine: cores − 1, between 2 and 8.
    pub fn default_threads() -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(4).clamp(2, 8)
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    }

    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // held while spawning: a worker that ends at once still finds its id listed
            let mut live = self.shared.live.lock().unwrap_or_else(|e| e.into_inner());
            for i in 0..self.threads.saturating_add(self.fast_threads) {
                let shared = self.shared.clone();
                let tx = self.tx.clone();
                let fast_only = i >= self.threads;
                let name = if fast_only { format!("lc-job-fast-{}", i - self.threads) } else { format!("lc-job-{i}") };
                let spawned = std::thread::Builder::new().name(name).spawn(move || {
                    let _ended = Ended(&shared);
                    while let Some((e, background)) = take(&shared, true, fast_only) {
                        let t0 = std::time::Instant::now();
                        // the slot is free again when the job ends, also if it panics
                        let _free = Free(&shared, e.slot, background);
                        let result = (e.job)();
                        let done = Done { slot: e.slot, key: e.key, result, ms: t0.elapsed().as_secs_f64() * 1000.0 };
                        if tx.send(done).is_err() {
                            break;
                        }
                    }
                });
                match spawned {
                    Ok(h) => {
                        live.push(h.thread().id());
                        self.workers.push(h);
                    }
                    Err(e) => log::error!("job pool: worker {i} could not start: {e}"),
                }
            }
        }
    }

    /// At most `max` jobs below `priority` run at once (background work: a whole folder's
    /// thumbnail renders), so workers stay free for what is on screen.
    pub fn cap_background(&self, priority: u32, max: usize) {
        *self.shared.background_cap.lock().unwrap_or_else(|e| e.into_inner()) = (priority, max.max(1));
        self.shared.cv.notify_all();
    }

    /// Run each job counted as background work ([`Self::cap_background`]) inside `run` (the app:
    /// as background GPU work, on the second graphics card first). `run` must call the closure it
    /// is given once and let a panic through; one that doesn't call it has the job run after it.
    /// Set once; a later call changes nothing.
    pub fn run_background_with(&self, run: BackgroundRun) {
        let _ = self.shared.background_run.set(run);
    }

    /// Stop the pool: queued jobs are dropped, no new one starts, and the call waits up to
    /// `timeout` for the jobs that are running. Returns whether every worker has ended; one that
    /// hasn't is left to finish on its own. For owners that are about to end the process, where
    /// dropping the pool may come too late or never (`std::process::exit`).
    pub fn shutdown(&mut self, timeout: Duration) -> bool {
        self.stopped = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            stop(&self.shared, &mut self.workers, timeout)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            close(&self.shared);
            true
        }
    }

    /// Worker threads that have not ended (0 before the first job, and on wasm).
    pub fn live_workers(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.shared.live.lock().map(|l| l.len()).unwrap_or(0)
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// Queue `job` for `slot`, replacing a queued (not yet running) job for the same slot.
    pub fn submit(&mut self, slot: S, key: u64, priority: u32, job: Job<R>) {
        if self.stopped {
            return; // nothing runs it any more
        }
        self.start();
        self.seq += 1;
        let (lock, cv) = (&self.shared.queue, &self.shared.cv);
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.retain(|e| e.slot != slot);
        q.push(Entry { slot, key, priority, seq: self.seq, job });
        cv.notify_all(); // (all: a fast-lane worker woken for a job it may not take would leave it waiting)
    }

    /// Change queued jobs' priorities: `f(slot, priority)` returns the new priority, or `None` to
    /// drop the job. Returns the dropped slots.
    pub fn reprioritize(&self, mut f: impl FnMut(&S, u32) -> Option<u32>) -> Vec<S> {
        let mut dropped = Vec::new();
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.retain_mut(|e| match f(&e.slot, e.priority) {
            Some(p) => {
                e.priority = p;
                true
            }
            None => {
                dropped.push(e.slot);
                false
            }
        });
        dropped
    }

    /// Queued (not started) jobs.
    pub fn queued(&self) -> usize {
        self.shared.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Is a job for `slot` queued?
    pub fn is_queued(&self, slot: S) -> bool {
        self.shared.queue.lock().map(|q| q.iter().any(|e| e.slot == slot)).unwrap_or(false)
    }

    /// A finished job, if any.
    pub fn try_recv(&mut self) -> Option<Done<S, R>> {
        self.early.take().or_else(|| self.rx.try_recv().ok())
    }

    /// Wait up to `timeout` for a job to finish; whether one has (it is the next [`Self::try_recv`]).
    pub fn wait(&mut self, timeout: Duration) -> bool {
        if self.early.is_none() {
            self.early = self.rx.recv_timeout(timeout).ok();
        }
        self.early.is_some()
    }

    /// Run up to `n` jobs on the calling thread (wasm, tests). Returns how many ran.
    pub fn run_inline(&mut self, n: usize) -> usize {
        let mut ran = 0;
        while ran < n {
            let Some((e, background)) = take(&self.shared, false, false) else { break };
            let _free = Free(&self.shared, e.slot, background);
            let result = (e.job)();
            let _ = self.tx.send(Done { slot: e.slot, key: e.key, result, ms: 0.0 });
            ran += 1;
        }
        ran
    }
}

/// Pop the best job (highest priority, newest first; `fast_only`: of the fast slots only). Blocks
/// when `wait` until one is queued or the pool shuts down.
/// The flag says the job counts as background work ([`JobPool::cap_background`]).
fn take<S: PartialEq + Copy, R: 'static>(shared: &Shared<S, R>, wait: bool, fast_only: bool) -> Option<(Entry<S, R>, bool)> {
    let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            return None;
        }
        // (a slot with a job running waits for it: see `Shared::busy`)
        let best = {
            let busy = shared.busy.lock().unwrap_or_else(|e| e.into_inner());
            let cheap = |e: &Entry<S, R>| shared.is_fast.is_some_and(|f| f(&e.slot));
            let (below, max) = *shared.background_cap.lock().unwrap_or_else(|e| e.into_inner());
            let room = shared.background_running.load(Ordering::Relaxed) < max;
            // (a cheap job is never held back as background work)
            let background = |e: &Entry<S, R>| e.priority < below && !cheap(e);
            q.iter()
                .enumerate()
                .filter(|(_, e)| !busy.contains(&e.slot) && (!fast_only || cheap(e)) && (room || !background(e)))
                .max_by_key(|(_, e)| (e.priority, e.seq))
                .map(|(i, e)| (i, background(e)))
        };
        if let Some((i, background)) = best {
            let e = q.swap_remove(i);
            shared.busy.lock().unwrap_or_else(|e| e.into_inner()).push(e.slot);
            if background {
                shared.background_running.fetch_add(1, Ordering::Relaxed);
            }
            return Some((for_background(shared, e, background), background));
        }
        if !wait {
            return None;
        }
        q = shared.cv.wait(q).unwrap_or_else(|e| e.into_inner());
    }
}

/// The job for `slot` ended (`background`: it counted as background work): a job queued for it
/// may start.
fn finished<S: PartialEq, R>(shared: &Shared<S, R>, slot: &S, background: bool) {
    {
        // under the queue lock: a worker is either before its look at `busy` or waiting
        let _q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        if background {
            let n = shared.background_running.load(Ordering::Relaxed);
            shared.background_running.store(n.saturating_sub(1), Ordering::Relaxed);
        }
        let mut busy = shared.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = busy.iter().position(|s| s == slot) {
            busy.swap_remove(i);
        }
    }
    shared.cv.notify_all();
}

/// Frees a job's slot when the job ends (see [`finished`]).
struct Free<'a, S: PartialEq, R>(&'a Shared<S, R>, S, bool);

impl<S: PartialEq, R> Drop for Free<'_, S, R> {
    fn drop(&mut self) {
        finished(self.0, &self.1, self.2);
    }
}

/// Takes a worker off the live list when its thread ends (also when a job panicked).
#[cfg(not(target_arch = "wasm32"))]
struct Ended<'a, S, R>(&'a Shared<S, R>);

#[cfg(not(target_arch = "wasm32"))]
impl<S, R> Drop for Ended<'_, S, R> {
    fn drop(&mut self) {
        let me = std::thread::current().id();
        self.0.live.lock().unwrap_or_else(|e| e.into_inner()).retain(|id| *id != me);
        self.0.ended.notify_all();
    }
}

/// Tell the workers to stop and take the queued jobs away from them.
fn close<S, R>(shared: &Shared<S, R>) {
    // the flag changes under the queue lock: a worker is either before its check or waiting
    let queued = {
        let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        shared.shutdown.store(true, Ordering::Relaxed);
        std::mem::take(&mut *q)
    };
    shared.cv.notify_all();
    drop(queued); // outside the lock: a job may own a lot
}

/// [`JobPool::shutdown`]: wait up to `timeout` for the workers to end, without holding the queue
/// lock, and never for the calling thread (a pool dropped by one of its own jobs).
#[cfg(not(target_arch = "wasm32"))]
fn stop<S, R>(shared: &Shared<S, R>, workers: &mut Vec<std::thread::JoinHandle<()>>, timeout: Duration) -> bool {
    close(shared);
    let me = std::thread::current().id();
    let t0 = std::time::Instant::now();
    let mut live = shared.live.lock().unwrap_or_else(|e| e.into_inner());
    while live.iter().any(|id| *id != me) {
        let Some(left) = timeout.checked_sub(t0.elapsed()).filter(|d| !d.is_zero()) else { break };
        live = shared.ended.wait_timeout(live, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    let running: Vec<std::thread::ThreadId> = live.iter().copied().filter(|id| *id != me).collect();
    drop(live);
    for h in workers.drain(..) {
        let id = h.thread().id();
        // a worker off the list is past its last job: joining it only waits for the thread to unwind
        if id != me && !running.contains(&id) {
            let _ = h.join();
        }
    }
    if !running.is_empty() {
        log::warn!("job pool: {} job(s) still running after {timeout:?}; not waiting for them", running.len());
    }
    running.is_empty()
}

impl<S, R> Drop for JobPool<S, R> {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        stop(&self.shared, &mut self.workers, DROP_WAIT);
        #[cfg(target_arch = "wasm32")]
        close(&self.shared);
    }
}

#[cfg(test)]
mod background_run_tests {
    use super::JobPool;

    thread_local! {
        static INSIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn mark(f: &mut dyn FnMut()) {
        INSIDE.with(|c| c.set(true));
        f();
        INSIDE.with(|c| c.set(false));
    }

    fn skip(_: &mut dyn FnMut()) {}

    // Given a pool whose background jobs run inside a wrapper (the app: the second graphics card
    // first), the jobs below the background priority run inside it and the others don't
    #[test]
    fn background_jobs_run_inside_the_pools_wrapper() {
        let mut pool: JobPool<u32, bool> = JobPool::new(0);
        pool.cap_background(4, 2);
        pool.run_background_with(mark);
        pool.submit(1, 1, 10, Box::new(|| INSIDE.with(|c| c.get())));
        pool.submit(2, 2, 3, Box::new(|| INSIDE.with(|c| c.get())));
        assert_eq!(pool.run_inline(2), 2);
        let mut got: Vec<(u32, bool)> = std::iter::from_fn(|| pool.try_recv()).map(|d| (d.slot, d.result)).collect();
        got.sort();
        assert_eq!(got, vec![(1, false), (2, true)], "only the background job ran in the wrapper");
        assert!(!INSIDE.with(|c| c.get()));
    }

    // Given a wrapper that doesn't call its job, the job still runs (once)
    #[test]
    fn a_wrapper_that_skips_the_job_still_has_it_run() {
        let mut pool: JobPool<u32, u32> = JobPool::new(0);
        pool.cap_background(4, 2);
        pool.run_background_with(skip);
        pool.submit(1, 1, 0, Box::new(|| 7));
        assert_eq!(pool.run_inline(1), 1);
        assert_eq!(pool.try_recv().map(|d| d.result), Some(7));
    }
}
