//! Is a file there? — answered without blocking the caller.
//!
//! On a sleeping NAS, a dropped share or a USB drive spinning up, one `stat` can take as long as
//! the OS or SMB timeout. The UI asks about files every frame (grid cells, the Info panel, the
//! Missing Photos view), so in the app the answers come from a cache that a worker thread fills
//! ([`Availability::run_in_background`]): an unknown file is reported as `None` ("assume it is
//! there"; the render worker finds out if it isn't), known answers are rechecked after
//! [`RECHECK`]. Everywhere else (CLI, MCP, tests) the default is synchronous: every question is a
//! fresh check, as before.
//!
//! The same worker can check a list of files, such as every library photo's, now and then
//! ([`Availability::sweep`]), one at a time and only while nothing is asked. That is for the
//! Missing Photos count ([`Availability::is_gone`]): one stream of checks on a share, not one per
//! thing that wants to know. The app starts the sweeps; nothing here does.
//!
//! A sweep of a large library on a share takes tens of minutes, and its `stat`s compete with the
//! reads of an import or a scan on the same share. So the worker starts no sweep check while the
//! caller says files are busy ([`Availability::sweep_yields_to`]); what is asked still gets
//! answered (within [`YIELD_POLL`]), and the sweep goes on where it stopped when the work ends.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Checks whether a path exists (injectable: tests use a slow one).
pub type ExistsProbe = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// Called (from the worker) when an answer changed, e.g. to repaint the UI (a sweep's first look
/// at a file that is there changes no answer anyone sees, so it wakes nobody).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Whether file work that a sweep should not compete with is going on.
pub type Busy = Arc<dyn Fn() -> bool + Send + Sync>;

/// How often a sweep that is giving way looks whether the work has ended.
const YIELD_POLL: Duration = Duration::from_millis(250);

/// How long a cached answer is used before it is checked again (it is still returned meanwhile).
pub const RECHECK: Duration = Duration::from_secs(15);

fn fs_exists(path: &str) -> bool {
    // the browser build has no file system: its originals are always "there"
    cfg!(target_arch = "wasm32") || std::path::Path::new(path).exists()
}

#[derive(Default)]
struct State {
    /// path → (exists, when checked)
    known: HashMap<String, (bool, std::time::Instant)>,
    queue: VecDeque<String>,
    queued: HashSet<String>,
    /// What a sweep has left to check: after the queue, whatever it holds.
    sweep: VecDeque<String>,
    /// The files found not there (and not found there since).
    gone: HashSet<String>,
    worker: bool,
    /// Bumped when the sweep under way is dropped for another one ([`Availability::restart_sweeps`]).
    sweep_epoch: u64,
    /// Whether file work a sweep gives way to is going on.
    busy: Option<Busy>,
    /// Bumped whenever a file is found gone, or found back.
    gone_generation: u64,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// File availability, synchronous or cached + background-checked. Cloning shares the cache.
#[derive(Clone)]
pub struct Availability {
    probe: ExistsProbe,
    background: Option<Waker>,
    state: Arc<Mutex<State>>,
}

impl Default for Availability {
    fn default() -> Self {
        Self { probe: Arc::new(fs_exists), background: None, state: Arc::default() }
    }
}

impl std::fmt::Debug for Availability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Availability").field("background", &self.background.is_some()).finish()
    }
}

impl Availability {
    /// Replace the existence check (tests: a slow or counting probe). Forgets cached answers.
    pub fn set_probe(&mut self, probe: ExistsProbe) {
        self.probe = probe;
        let mut st = lock(&self.state);
        st.known.clear();
        st.gone.clear();
        st.gone_generation += 1;
    }

    /// Answer from the cache and check on a worker thread from now on; `wake` runs when an
    /// answer changed. No effect in the browser build (no threads, no file system).
    pub fn run_in_background(&mut self, wake: Waker) {
        if cfg!(target_arch = "wasm32") {
            return;
        }
        self.background = Some(wake);
    }

    /// The sweep starts no check while `busy` says so (the caller's import, scan or Local read in
    /// flight); asked files are answered all the same, and the sweep resumes where it was left when
    /// `busy` turns false. `busy` should read state that a task's guard owns (the activity stack), so
    /// a task that never ends cleanly can't hold the sweep for good.
    pub fn sweep_yields_to(&self, busy: Busy) {
        lock(&self.state).busy = Some(busy);
    }

    /// The existence check itself (blocking: for worker threads, e.g. the Rename preview's).
    pub fn probe(&self) -> ExistsProbe {
        self.probe.clone()
    }

    pub fn is_background(&self) -> bool {
        self.background.is_some()
    }

    /// Does `path` exist? Synchronous mode: checked now. Background mode: the cached answer
    /// (`None` while unknown); unknown or stale answers are queued for the worker.
    pub fn exists(&self, path: &str) -> Option<bool> {
        let Some(wake) = &self.background else {
            return Some((self.probe)(path));
        };
        let mut st = lock(&self.state);
        let now = std::time::Instant::now();
        let cached = st.known.get(path).copied();
        if cached.is_none_or(|(_, at)| now.saturating_duration_since(at) >= RECHECK) && !st.queued.contains(path) {
            st.queued.insert(path.to_string());
            st.queue.push_back(path.to_string());
            self.start(st, wake);
        }
        cached.map(|c| c.0)
    }

    /// Known to be missing (unknown counts as present).
    pub fn is_offline(&self, path: &str) -> bool {
        self.exists(path) == Some(false)
    }

    /// Changes whenever a file is found gone or found back: what [`Availability::is_offline`]
    /// says changed for some file. (A file found there at its first check changes nothing.)
    pub fn gone_generation(&self) -> u64 {
        lock(&self.state).gone_generation
    }

    /// Paths waiting for the worker.
    pub fn pending(&self) -> usize {
        lock(&self.state).queue.len()
    }

    /// Check `paths` too, in the background and after whatever is asked meanwhile, skipping those
    /// checked less than [`RECHECK`] ago. One sweep at a time: `false` (and nothing queued) while
    /// one is under way, or in synchronous mode. A sweep's answers wake only when a file is found
    /// gone or back.
    pub fn sweep(&self, paths: Vec<String>) -> bool {
        self.sweep_checked(None, paths)
    }

    fn sweep_checked(&self, epoch: Option<u64>, paths: Vec<String>) -> bool {
        let Some(wake) = &self.background else { return false };
        let mut st = lock(&self.state);
        if !st.sweep.is_empty() || epoch.is_some_and(|e| e != st.sweep_epoch) {
            return false;
        }
        st.sweep = paths.into();
        self.start(st, wake);
        true
    }

    /// Drop what the sweep under way has left (another library was opened: its files are not the
    /// ones to check now) and say which sweeps may start from here ([`Availability::sweep_in`]): a
    /// list of files being made for the old one is then refused.
    pub fn restart_sweeps(&self) -> u64 {
        let mut st = lock(&self.state);
        st.sweep.clear();
        st.sweep_epoch += 1;
        st.sweep_epoch
    }

    /// [`Availability::sweep`], unless [`Availability::restart_sweeps`] was called since it
    /// returned `epoch`.
    pub fn sweep_in(&self, epoch: u64, paths: Vec<String>) -> bool {
        self.sweep_checked(Some(epoch), paths)
    }

    /// Whether the file at `path` was found gone (and not found there since): `false` for a file
    /// not checked yet.
    pub fn is_gone(&self, path: &str) -> bool {
        lock(&self.state).gone.contains(path)
    }

    /// How many files are found gone.
    pub fn gone_len(&self) -> usize {
        lock(&self.state).gone.len()
    }

    /// What the sweep under way has left to check (0: none under way, though its last file may still
    /// be in flight).
    pub fn sweeping(&self) -> usize {
        lock(&self.state).sweep.len()
    }

    /// Start the worker, unless it runs.
    fn start(&self, mut st: MutexGuard<'_, State>, wake: &Waker) {
        if !st.worker {
            st.worker = true;
            drop(st);
            self.spawn_worker(wake.clone());
        }
    }

    fn spawn_worker(&self, wake: Waker) {
        let (state, probe) = (self.state.clone(), self.probe.clone());
        let started = std::thread::Builder::new().name("lc-availability".into()).spawn(move || {
            loop {
                // what was asked first; a sweep's next file when nothing is (unless it is asked
                // about too, or was checked a moment ago)
                let (path, asked) = {
                    let mut st = lock(&state);
                    let giving_way = if st.queue.is_empty() && !st.sweep.is_empty() { st.busy.clone() } else { None };
                    if let Some(busy) = giving_way {
                        drop(st);
                        if busy() {
                            // (the worker stays: a question asked meanwhile is answered within YIELD_POLL)
                            std::thread::sleep(YIELD_POLL);
                            continue;
                        }
                        st = lock(&state);
                    }
                    match st.queue.pop_front() {
                        Some(p) => (p, true),
                        None => loop {
                            let Some(p) = st.sweep.pop_front() else {
                                st.worker = false;
                                return;
                            };
                            let fresh = st.known.get(&p).is_some_and(|(_, at)| at.elapsed() < RECHECK);
                            if !fresh && !st.queued.contains(&p) {
                                break (p, false);
                            }
                        },
                    }
                };
                let ok = probe(&path);
                let wakes = {
                    let mut st = lock(&state);
                    if asked {
                        st.queued.remove(&path);
                    }
                    let gone_changed = if ok { st.gone.remove(&path) } else { st.gone.insert(path.clone()) };
                    let prev = st.known.insert(path, (ok, std::time::Instant::now()));
                    let changed = prev.map(|p| p.0) != Some(ok);
                    if gone_changed {
                        st.gone_generation += 1;
                    }
                    (asked && changed) || gone_changed
                };
                if wakes {
                    wake();
                }
            }
        });
        if started.is_err() {
            // try again on the next question; a sweep nobody works on would refuse every new one
            let mut st = lock(&self.state);
            st.worker = false;
            st.sweep.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn synchronous_by_default() {
        let mut a = Availability::default();
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        a.set_probe(Arc::new(move |p| {
            c.fetch_add(1, Ordering::SeqCst);
            p == "here"
        }));
        assert_eq!(a.exists("here"), Some(true));
        assert_eq!(a.exists("gone"), Some(false));
        assert_eq!(a.exists("gone"), Some(false));
        assert_eq!(n.load(Ordering::SeqCst), 3, "every question is a fresh check");
    }

    #[test]
    fn background_answers_from_the_cache_without_blocking() {
        let mut a = Availability::default();
        let caller = std::thread::current().id();
        let on_caller = Arc::new(AtomicUsize::new(0));
        let c = on_caller.clone();
        a.set_probe(Arc::new(move |p| {
            if std::thread::current().id() == caller {
                c.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(200)); // a sleeping NAS
            p == "here"
        }));
        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        a.run_in_background(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        let t0 = std::time::Instant::now();
        assert_eq!(a.exists("here"), None, "unknown at first");
        assert_eq!(a.exists("gone"), None);
        assert!(t0.elapsed() < Duration::from_millis(150), "the caller never waits for the probe");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while (a.exists("here").is_none() || a.exists("gone").is_none()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.exists("here"), Some(true));
        assert_eq!(a.exists("gone"), Some(false));
        assert!(a.is_offline("gone") && !a.is_offline("here") && !a.is_offline("unknown"));
        assert_eq!(on_caller.load(Ordering::SeqCst), 0, "never probed on the calling thread");
        assert!(woke.load(Ordering::SeqCst) >= 2);
        assert!(a.gone_generation() >= 2);
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A background `Availability` whose probe logs what it checks; files named `gone…` are not there.
    fn logged() -> (Availability, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
        let mut a = Availability::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        a.set_probe(Arc::new(move |p| {
            held(&l).push(p.to_string());
            !p.starts_with("gone")
        }));
        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        a.run_in_background(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        (a, log, woke)
    }

    #[test]
    fn the_files_a_sweep_finds_gone_are_counted() {
        let (a, _, _) = logged();
        assert!(a.sweep(vec!["here".into(), "gone 1".into(), "also here".into(), "gone 2".into()]));
        wait_until("the sweep", || a.sweeping() == 0 && a.pending() == 0);
        wait_until("the answers", || a.gone_len() == 2);
        assert!(a.is_gone("gone 1") && a.is_gone("gone 2") && !a.is_gone("here") && !a.is_gone("also here"));
    }

    #[test]
    fn files_found_there_at_first_look_change_nothing_for_missing_photos() {
        let (a, _, woke) = logged();
        let before = a.gone_generation();
        a.sweep(vec!["a".into(), "b".into(), "c".into()]);
        wait_until("the answers", || ["a", "b", "c"].iter().all(|p| lock(&a.state).known.contains_key(*p)));
        assert_eq!(a.gone_generation(), before, "nothing went missing");
        assert_eq!(woke.load(Ordering::SeqCst), 0, "a sweep finding files there wakes nobody");
        a.sweep(vec!["gone".into()]);
        wait_until("the gone file", || a.gone_generation() != before);
        wait_until("the wake", || woke.load(Ordering::SeqCst) == 1);
    }

    #[test]
    fn what_is_asked_is_checked_before_the_rest_of_a_sweep_and_one_sweep_runs_at_a_time() {
        let mut a = Availability::default();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        // the first file of the sweep holds the worker until the test has asked about another
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (l, g) = (log.clone(), gate.clone());
        a.set_probe(Arc::new(move |p| {
            if p == "sweep 0" {
                let (open, cv) = &*g;
                let mut open = held(open);
                while !*open {
                    open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
                }
            }
            held(&l).push(p.to_string());
            true
        }));
        a.run_in_background(Arc::new(|| {}));
        assert!(a.sweep((0..50).map(|i| format!("sweep {i}")).collect()));
        assert!(!a.sweep(vec!["another".into()]), "a sweep under way: the next one waits for its turn");
        assert_eq!(a.exists("asked"), None, "not known yet");
        *held(&gate.0) = true;
        gate.1.notify_all();
        wait_until("the log", || held(&log).len() == 51);
        let log = held(&log);
        // (at most the file the worker was already checking comes first)
        assert!(matches!(log.iter().position(|p| p == "asked"), Some(0 | 1)), "asked next, ahead of the sweep: {log:?}");
        assert!(!log.iter().any(|p| p == "another"));
    }

    #[test]
    fn a_new_sweep_replaces_the_one_under_way_and_a_list_made_for_the_old_one_is_refused() {
        let mut a = Availability::default();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        // the old sweep's first file holds the worker until the new one is in place
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (l, g) = (log.clone(), gate.clone());
        a.set_probe(Arc::new(move |p| {
            if p == "old 0" {
                let (open, cv) = &*g;
                let mut open = held(open);
                while !*open {
                    open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
                }
            }
            held(&l).push(p.to_string());
            true
        }));
        a.run_in_background(Arc::new(|| {}));
        let old = a.restart_sweeps();
        assert!(a.sweep_in(old, (0..20).map(|i| format!("old {i}")).collect()));
        wait_until("the worker takes the first file", || a.sweeping() == 19);
        // another library is opened: what is left of the old sweep goes, and a list still being made for it too
        let new = a.restart_sweeps();
        assert_eq!(a.sweeping(), 0);
        assert!(!a.sweep_in(old, vec!["old late".into()]), "a list made for the old library is refused");
        assert!(a.sweep_in(new, (0..3).map(|i| format!("new {i}")).collect()));
        *held(&gate.0) = true;
        gate.1.notify_all();
        wait_until("the new sweep", || held(&log).len() == 4);
        assert_eq!(*held(&log), ["old 0", "new 0", "new 1", "new 2"]);
    }

    #[test]
    fn files_found_gone_are_told_apart_from_files_never_checked() {
        let (a, _, _) = logged();
        assert!(!a.is_gone("gone 1") && a.gone_len() == 0);
        assert!(a.sweep(vec!["gone 1".into(), "here".into()]));
        wait_until("the sweep", || a.sweeping() == 0 && a.pending() == 0 && a.gone_len() == 1);
        assert!(a.is_gone("gone 1") && !a.is_gone("here") && !a.is_gone("never asked"));
    }

    fn busy_flag(a: &Availability) -> Arc<std::sync::atomic::AtomicBool> {
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let b = busy.clone();
        a.sweep_yields_to(Arc::new(move || b.load(Ordering::SeqCst)));
        busy
    }

    #[test]
    fn a_sweep_gives_way_to_file_work_answers_what_is_asked_and_resumes_after() {
        let (a, log, _) = logged();
        let busy = busy_flag(&a);
        assert!(a.sweep((0..20).map(|i| format!("sweep {i}")).collect()));
        std::thread::sleep(YIELD_POLL * 3);
        assert!(held(&log).is_empty(), "no sweep check while files are busy");
        assert_eq!(a.sweeping(), 20, "nothing of the sweep is lost");
        assert_eq!(a.exists("asked"), None);
        wait_until("the answer", || a.exists("asked") == Some(true));
        assert_eq!(*held(&log), ["asked"], "what is asked is answered, the sweep still waits");
        busy.store(false, Ordering::SeqCst);
        wait_until("the sweep", || a.sweeping() == 0);
        wait_until("the log", || held(&log).len() == 21);
        // it went on from its first file, not from scratch
        assert_eq!(held(&log).iter().filter(|p| *p == "sweep 0").count(), 1);
    }

    #[test]
    fn a_sweep_stopped_part_way_goes_on_from_there() {
        let mut a = Availability::default();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l = log.clone();
        // the worker is held inside the fourth check until the sweep is told to give way
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let h = hold.clone();
        a.set_probe(Arc::new(move |p| {
            while p == "sweep 3" && h.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            held(&l).push(p.to_string());
            true
        }));
        a.run_in_background(Arc::new(|| {}));
        let busy = busy_flag(&a);
        busy.store(false, Ordering::SeqCst);
        assert!(a.sweep((0..100).map(|i| format!("sweep {i}")).collect()));
        wait_until("a few checks", || held(&log).len() >= 3);
        busy.store(true, Ordering::SeqCst);
        hold.store(false, Ordering::SeqCst);
        std::thread::sleep(YIELD_POLL * 2);
        let stopped = held(&log).len();
        assert_eq!(stopped, 4, "stopped part way, after the check it was in");
        std::thread::sleep(YIELD_POLL * 2);
        assert_eq!(held(&log).len(), stopped, "paused");
        assert_eq!(a.sweeping(), 100 - stopped, "what is left is kept");
        busy.store(false, Ordering::SeqCst);
        wait_until("the sweep", || a.sweeping() == 0 && held(&log).len() == 100);
        let mut all = held(&log).clone();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 100, "every file checked once, none again from the start");
    }

    #[test]
    fn a_sweep_that_yields_to_nothing_runs_as_before() {
        let (a, log, _) = logged();
        assert!(a.sweep(vec!["a".into(), "b".into()]));
        wait_until("the sweep", || held(&log).len() == 2);
    }

    #[test]
    fn a_sweep_skips_files_checked_a_moment_ago() {
        let (a, log, _) = logged();
        assert_eq!(a.exists("a"), None);
        wait_until("the answer", || a.exists("a").is_some());
        a.sweep(vec!["a".into(), "b".into()]);
        wait_until("b", || held(&log).iter().any(|p| p == "b"));
        wait_until("the sweep", || a.sweeping() == 0);
        assert_eq!(held(&log).iter().filter(|p| *p == "a").count(), 1, "`a` was checked a moment ago");
    }
}
