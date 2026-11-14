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
//!
//! What is kept for the files that are there does not grow with the library, which the grid and a
//! sweep go through whole: [`MAX_THERE`] of them are kept, and the oldest found is forgotten first
//! unless it was asked about meanwhile (a file that is not kept is unknown, which counts as there). A
//! file found gone is kept for as long as it is gone (the offline badges and the Missing Photos count
//! are made of those), and a sweep adds to what is kept only the files it finds gone.

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

/// How many files found there are kept: far more than the grid and the filmstrip show at once, and
/// enough for the originals of all but a very large library (next to nothing is forgotten there;
/// the smart previews that were looked at take places too). Each takes a few hundred bytes with its
/// path: about 20 MB for paths of 50 characters, a few tens of MB at most. (The files found gone
/// are kept besides, for as long as they are gone.)
const MAX_THERE: usize = 65_536;

fn fs_exists(path: &str) -> bool {
    // the browser build has no file system: its originals are always "there"
    cfg!(target_arch = "wasm32") || std::path::Path::new(path).exists()
}

/// What the last check of a file found, and how the file stands in [`State::there`].
#[derive(Clone, Copy)]
struct Kept {
    /// Whether the file was there.
    there: bool,
    /// When it was checked.
    at: std::time::Instant,
    /// Whether [`State::there`] has an entry for the file (one at most): from when it was found there
    /// until its turn to be forgotten comes, even if it was found gone meanwhile.
    listed: bool,
    /// Whether the file was asked about since it was listed or last spared ([`State::make_room`]), or
    /// found there again while its old entry was still listed.
    used: bool,
}

#[derive(Default)]
struct State {
    /// path → what the last check of it found
    known: HashMap<String, Kept>,
    /// The files found there, oldest first, each once ([`Kept::listed`]): [`MAX_THERE`] at most. One
    /// found gone since stays listed until its turn, which only unlists it: it is kept as gone.
    there: VecDeque<String>,
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

impl State {
    /// Keep what a check found for `path`, and say whether that differs from what was kept (nothing
    /// kept differs). All the files found gone are kept, and [`MAX_THERE`] of the files found there:
    /// room for one more is made by forgetting the oldest ([`State::make_room`]), and a forgotten file
    /// is unknown, which counts as there.
    fn remember(&mut self, path: String, ok: bool) -> bool {
        let old = self.known.get(&path).copied();
        let (listed, used) = old.map_or((false, false), |k| (k.listed, k.used));
        let mut kept = Kept { there: ok, at: std::time::Instant::now(), listed, used };
        if ok && !listed {
            self.make_room();
            self.there.push_back(path.clone());
            kept.listed = true;
            kept.used = false;
        } else if ok && old.is_some_and(|k| !k.there) {
            // found there again while its entry from before is still listed: that entry's turn only spares it
            kept.used = true;
        }
        self.known.insert(path, kept);
        old.map(|k| k.there) != Some(ok)
    }

    /// Make room in `there` for one more file: the oldest listed is forgotten, unless it was asked about
    /// since it was listed (or last spared), which spares it once: it goes to the back, and the next
    /// oldest is looked at. A file found gone since is only unlisted: its answer stays.
    fn make_room(&mut self) {
        while self.there.len() >= MAX_THERE {
            let Some(oldest) = self.there.pop_front() else { break };
            let Some(kept) = self.known.get_mut(&oldest) else { continue };
            if !kept.there {
                kept.listed = false;
            } else if kept.used {
                kept.used = false;
                self.there.push_back(oldest);
            } else {
                self.known.remove(&oldest);
            }
        }
    }

    /// The sweep's list goes, and the room it took (a path for every photo of the library) with it: its last
    /// path is taken, it is over, or it is dropped.
    fn end_sweep(&mut self) {
        self.sweep = VecDeque::new();
    }
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
        st.there.clear();
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

    /// Does `path` exist? Synchronous mode: checked now. Background mode: the cached answer (`None`
    /// while unknown, and for a file found there that was forgotten: see [`MAX_THERE`]); unknown or
    /// stale answers are queued for the worker. Asking about a kept answer spares it once when its
    /// turn to be forgotten comes.
    pub fn exists(&self, path: &str) -> Option<bool> {
        let Some(wake) = &self.background else {
            return Some((self.probe)(path));
        };
        let mut st = lock(&self.state);
        let now = std::time::Instant::now();
        let cached = st.known.get_mut(path).map(|kept| {
            kept.used = true;
            *kept
        });
        if cached.is_none_or(|kept| now.saturating_duration_since(kept.at) >= RECHECK) && !st.queued.contains(path) {
            st.queued.insert(path.to_string());
            st.queue.push_back(path.to_string());
            self.start(st, wake);
        }
        cached.map(|kept| kept.there)
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
    /// with a kept answer less than [`RECHECK`] old. One sweep at a time: `false` (and nothing
    /// queued) while one is under way, or in synchronous mode. A sweep's answers wake only when a
    /// file is found gone or back. It keeps an answer only for a file found gone, or one it already
    /// keeps an answer for: the files found there are not kept (see [`MAX_THERE`]), so a sweep of a
    /// library leaves what is gone behind, not the library.
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
        st.end_sweep();
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
                                st.end_sweep();
                                st.worker = false;
                                return;
                            };
                            if st.sweep.is_empty() {
                                // (the last path: the list gives back its room now, not when the worker next finds nothing asked,
                                // which a share that answers slowly can put off for long)
                                st.end_sweep();
                            }
                            let fresh = st.known.get(&p).is_some_and(|kept| kept.at.elapsed() < RECHECK);
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
                    if gone_changed {
                        st.gone_generation += 1;
                    }
                    // what a sweep finds there is kept only where an answer is kept already: a library's
                    // worth of paths is not what this cache is for
                    let changed = (asked || !ok || st.known.contains_key(&path)) && st.remember(path, ok);
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
            st.end_sweep();
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
        // Cache publication precedes its wake callback. Check these two notifications before
        // querying another unknown path, whose notification must not satisfy this assertion.
        wait_until("availability wake notifications", || woke.load(Ordering::SeqCst) >= 2);
        assert!(woke.load(Ordering::SeqCst) >= 2);
        assert!(a.is_offline("gone") && !a.is_offline("here") && !a.is_offline("unknown"));
        assert_eq!(on_caller.load(Ordering::SeqCst), 0, "never probed on the calling thread");
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

    /// The worker has answered all it was given and stopped.
    fn wait_idle(a: &Availability) {
        wait_until("the worker to finish", || !lock(&a.state).worker);
    }

    /// How many answers are kept.
    fn remembered(a: &Availability) -> usize {
        lock(&a.state).known.len()
    }

    /// What is kept for `path`: whether the file was there.
    fn kept_there(st: &State, path: &str) -> Option<bool> {
        st.known.get(path).map(|kept| kept.there)
    }

    /// Make the answer kept for `path` `by` older (a sweep skips what was checked a moment ago).
    fn age(a: &Availability, path: &str, by: Duration) {
        let mut st = lock(&a.state);
        let kept = st.known.get_mut(path).expect("an answer is kept for the path");
        kept.at = kept.at.checked_sub(by).expect("the clock has run for longer than that");
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
        wait_idle(&a);
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

    #[test]
    fn a_sweep_keeps_only_the_files_it_finds_gone() {
        let (a, _, _) = logged();
        // a library's worth of files, a few of them gone
        let paths: Vec<String> = (0..2_000).map(|i| if i % 500 == 0 { format!("gone {i}") } else { format!("here {i}") }).collect();
        assert!(a.sweep(paths));
        wait_idle(&a);
        assert_eq!(a.gone_len(), 4);
        assert_eq!(remembered(&a), 4, "the files found there are not kept");
        assert_eq!(a.exists("gone 500"), Some(false));
        assert_eq!(a.exists("here 7"), None, "not kept: unknown, which counts as there");
        assert!(!a.is_offline("here 7"));
    }

    #[test]
    fn a_sweep_still_updates_the_answer_kept_for_a_file() {
        let mut a = Availability::default();
        let there = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let t = there.clone();
        a.set_probe(Arc::new(move |_| t.load(Ordering::SeqCst)));
        a.run_in_background(Arc::new(|| {}));
        assert_eq!(a.exists("moved"), None);
        wait_until("the answer", || a.is_offline("moved"));
        // the drive comes back, and the answer is old enough for the sweep to check the file again
        there.store(true, Ordering::SeqCst);
        age(&a, "moved", RECHECK + Duration::from_secs(1));
        assert!(a.sweep(vec!["moved".into()]));
        wait_idle(&a);
        assert!(!a.is_gone("moved"));
        assert_eq!(a.exists("moved"), Some(true), "the kept answer follows the sweep, not what was found before");
    }

    #[test]
    fn the_files_found_there_are_kept_up_to_a_limit_and_the_files_found_gone_are_not_forgotten() {
        let (a, _, _) = logged();
        let gone: Vec<String> = (0..3).map(|i| format!("gone {i}")).collect();
        for p in &gone {
            assert_eq!(a.exists(p), None);
        }
        wait_until("the gone files", || gone.iter().all(|p| a.is_gone(p)));
        // far more files than are kept are asked about, as the grid scrolls through a large library
        let n = MAX_THERE + 1_000;
        for i in 0..n {
            a.exists(&format!("here {i}"));
        }
        wait_idle(&a);
        assert_eq!(remembered(&a), MAX_THERE + 3, "the last {MAX_THERE} found there and every one found gone");
        assert!(gone.iter().all(|p| a.is_offline(p)), "a file found gone stays so, however much is asked after it");
        assert_eq!(a.exists(&format!("here {}", n - 1)), Some(true), "the newest is kept");
        assert_eq!(a.exists(&format!("here {}", n - MAX_THERE)), Some(true), "the oldest of the kept");
        // (last: asking about a forgotten file makes room for its new answer)
        assert_eq!(a.exists(&format!("here {}", n - MAX_THERE - 1)), None, "the first one forgotten: unknown counts as there");
        assert_eq!(a.exists("here 0"), None);
        assert!(!a.is_offline("here 0"));
    }

    #[test]
    fn a_sweep_that_is_over_gives_back_the_room_its_list_took() {
        let (a, _, _) = logged();
        assert!(a.sweep((0..1_000).map(|i| format!("sweep {i}")).collect()));
        wait_idle(&a);
        assert_eq!(lock(&a.state).sweep.capacity(), 0);
    }

    #[test]
    fn a_sweep_dropped_for_another_library_gives_back_the_room_its_list_took() {
        let (a, _, _) = logged();
        let busy = busy_flag(&a);
        assert!(a.sweep((0..1_000).map(|i| format!("sweep {i}")).collect()));
        assert!(lock(&a.state).sweep.capacity() >= 1_000);
        a.restart_sweeps();
        assert_eq!(lock(&a.state).sweep.capacity(), 0);
        busy.store(false, Ordering::SeqCst);
        wait_idle(&a);
    }

    #[test]
    fn a_sweep_gives_back_the_room_its_list_took_as_soon_as_its_last_path_is_taken() {
        let mut a = Availability::default();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        // the last path is taken from the list, and then the worker waits on it (a share that does not answer)
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (l, h) = (log.clone(), hold.clone());
        a.set_probe(Arc::new(move |p| {
            held(&l).push(p.to_string());
            while p == "sweep 999" && h.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            true
        }));
        a.run_in_background(Arc::new(|| {}));
        assert!(a.sweep((0..1_000).map(|i| format!("sweep {i}")).collect()));
        wait_until("the last path is being checked", || held(&log).iter().any(|p| p == "sweep 999"));
        assert_eq!(a.sweeping(), 0, "nothing of the sweep is left to check");
        assert_eq!(lock(&a.state).sweep.capacity(), 0, "the list goes with its last path, not once the worker has nothing else to do");
        hold.store(false, Ordering::SeqCst);
        wait_idle(&a);
    }

    #[test]
    fn an_answer_found_again_keeps_its_place_in_the_list() {
        let mut st = State::default();
        assert!(st.remember("same".into(), true), "the first answer is a change");
        for _ in 0..2 * MAX_THERE {
            assert!(!st.remember("same".into(), true), "the same answer again is none");
        }
        assert_eq!(st.there.len(), 1, "listed once, however often it is found again");
        assert_eq!(kept_there(&st, "same"), Some(true), "and still kept");
    }

    #[test]
    fn a_file_found_there_again_after_being_gone_is_not_forgotten_by_its_old_entry() {
        let mut st = State::default();
        st.remember("back".into(), true);
        st.remember("back".into(), false);
        st.remember("back".into(), true);
        assert_eq!(st.there.len(), 1, "listed once, however often it comes and goes");
        for i in 0..MAX_THERE - 1 {
            st.remember(format!("here {i}"), true);
        }
        assert_eq!(kept_there(&st, "back"), Some(true), "found there among the last {MAX_THERE}");
        // one more: the oldest entry is `back`'s from before it was gone, and it was found there again since
        st.remember("one more".into(), true);
        assert_eq!(kept_there(&st, "back"), Some(true), "not forgotten by the entry it had before");
        assert_eq!(kept_there(&st, "here 0"), None, "the oldest found there goes instead");
    }

    #[test]
    fn a_file_found_there_again_once_its_old_entry_is_dropped_is_listed_again() {
        let mut st = State::default();
        st.remember("moved".into(), true);
        st.remember("moved".into(), false);
        // its turn comes with the files found there after it: it is unlisted, and still kept as gone
        for i in 0..MAX_THERE {
            st.remember(format!("here {i}"), true);
        }
        assert_eq!(kept_there(&st, "moved"), Some(false));
        assert_eq!(st.there.len(), MAX_THERE);
        assert!(st.remember("moved".into(), true), "found back");
        assert_eq!(kept_there(&st, "moved"), Some(true));
        assert_eq!(st.there.back().map(String::as_str), Some("moved"), "listed as the newest");
        assert_eq!(st.there.len(), MAX_THERE, "the oldest made room for it");
        assert_eq!(kept_there(&st, "here 0"), None);
    }

    #[test]
    fn an_answer_asked_about_is_spared_once_when_its_turn_to_be_forgotten_comes() {
        let mut a = Availability::default();
        a.run_in_background(Arc::new(|| {}));
        {
            let mut st = lock(&a.state);
            st.remember("shown".into(), true);
            for i in 0..MAX_THERE - 1 {
                st.remember(format!("here {i}"), true);
            }
        }
        // (the Info panel asks about the selected photo's smart preview on every frame)
        assert_eq!(a.exists("shown"), Some(true));
        let mut st = lock(&a.state);
        st.remember("new 1".into(), true);
        st.remember("new 2".into(), true);
        assert_eq!(kept_there(&st, "shown"), Some(true), "the oldest, but asked about: spared");
        assert_eq!(kept_there(&st, "here 0"), None, "the files found there after it go first");
        assert_eq!(kept_there(&st, "here 1"), None);
        assert_eq!(kept_there(&st, "here 2"), Some(true));
        // not asked about since: at its next turn it goes
        for i in 0..MAX_THERE {
            st.remember(format!("later {i}"), true);
        }
        assert_eq!(kept_there(&st, "shown"), None);
    }

    #[test]
    fn a_check_that_finds_a_file_there_again_does_not_spare_it() {
        let mut st = State::default();
        st.remember("old".into(), true);
        for i in 0..MAX_THERE - 1 {
            st.remember(format!("here {i}"), true);
        }
        // checked again, as a sweep does: nobody asked about it
        assert!(!st.remember("old".into(), true), "the same answer again is none");
        st.remember("one more".into(), true);
        assert_eq!(kept_there(&st, "old"), None, "the oldest goes: a check again is no reason to keep it");
    }

    #[test]
    fn a_file_listed_again_is_not_spared_for_having_been_asked_about_before_it_was_gone() {
        let mut a = Availability::default();
        a.run_in_background(Arc::new(|| {}));
        lock(&a.state).remember("moved".into(), true);
        assert_eq!(a.exists("moved"), Some(true), "asked about");
        let mut st = lock(&a.state);
        st.remember("moved".into(), false);
        // enough files found there after it for its old entry to come up: it is unlisted
        for i in 0..MAX_THERE {
            st.remember(format!("here {i}"), true);
        }
        st.remember("moved".into(), true);
        // and as many again for its new entry to come up
        for i in 0..MAX_THERE {
            st.remember(format!("later {i}"), true);
        }
        assert_eq!(kept_there(&st, "moved"), None, "the ask from before does not spare the new entry");
    }

    #[test]
    fn a_file_found_gone_after_being_found_there_is_not_forgotten_with_the_files_found_there() {
        let mut a = Availability::default();
        let there = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let t = there.clone();
        a.set_probe(Arc::new(move |p| p != "moved" || t.load(Ordering::SeqCst)));
        a.run_in_background(Arc::new(|| {}));
        assert_eq!(a.exists("moved"), None);
        wait_until("found there", || a.exists("moved") == Some(true));
        // the drive goes away, and the answer is old enough for the file to be checked again
        there.store(false, Ordering::SeqCst);
        age(&a, "moved", RECHECK + Duration::from_secs(1));
        assert_eq!(a.exists("moved"), Some(true), "the old answer is used while the file is checked again");
        wait_until("found gone", || a.is_offline("moved"));
        // a long scroll pushes out the files found there, the first of them `moved`'s old answer
        for i in 0..MAX_THERE + 5 {
            a.exists(&format!("here {i}"));
        }
        wait_idle(&a);
        assert!(a.is_offline("moved"), "found gone: not forgotten with the answers found there");
    }
}
