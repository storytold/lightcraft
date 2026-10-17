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
//! The same worker checks every library photo's file now and then ([`Availability::sweep`]), one
//! at a time and only while nothing is asked, for the sidebar's Missing Photos count
//! ([`Availability::count_gone`]): one stream of checks on a share, not one per thing that wants
//! to know.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Checks whether a path exists (injectable: tests use a slow one).
pub type ExistsProbe = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// Called (from the worker) when an answer changed, e.g. to repaint the UI.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

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
        let Some(wake) = &self.background else { return false };
        let mut st = lock(&self.state);
        if !st.sweep.is_empty() {
            return false;
        }
        st.sweep = paths.into();
        self.start(st, wake);
        true
    }

    /// What the sweep under way has left to check (0: none under way).
    pub fn sweeping(&self) -> usize {
        lock(&self.state).sweep.len()
    }

    /// The files found gone, each counted as `count` says (the photos that have it: 0 for a file
    /// no photo has any more). Background mode only: nothing is known in synchronous mode.
    pub fn count_gone(&self, mut count: impl FnMut(&str) -> usize) -> usize {
        lock(&self.state).gone.iter().map(|p| count(p)).fold(0, usize::saturating_add)
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
            // try again on the next question
            lock(&self.state).worker = false;
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
        wait_until("the answers", || a.count_gone(|_| 1) == 2);
        assert_eq!(a.count_gone(|p| usize::from(p == "gone 1") * 3), 3, "each gone file counts as the caller says (photos with that file)");
        let mut seen = Vec::new();
        a.count_gone(|p| {
            seen.push(p.to_string());
            0
        });
        seen.sort();
        assert_eq!(seen, ["gone 1", "gone 2"]);
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
