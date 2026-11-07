//! Cache pixel validation for status queries. Builds and migrations always validate afresh.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::availability::Waker;

const CAPACITY: usize = 256;
type Validator = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Stamp { modified: metadata.modified().ok()?, len: metadata.len() })
}

#[derive(Default)]
struct State {
    entries: HashMap<PathBuf, (Stamp, Option<bool>)>,
    queue: VecDeque<(PathBuf, Stamp)>,
    worker: bool,
}

impl State {
    fn insert(&mut self, path: &Path, stamp: Stamp, usable: Option<bool>) {
        if (!self.entries.contains_key(path) && self.entries.len() >= CAPACITY) || self.queue.len() >= CAPACITY {
            self.entries.clear();
            self.queue.clear();
        }
        self.entries.insert(path.to_path_buf(), (stamp, usable));
    }
}

#[derive(Default)]
pub(crate) struct UsabilityCache {
    state: Arc<Mutex<State>>,
}

impl UsabilityCache {
    /// App queries return cached status and validate cold or changed proxies on one worker.
    /// An unchecked or changed proxy reports false until its pixels verify.
    /// Sessions without an app waker validate synchronously.
    pub(crate) fn query(&self, path: &Path, wake: Option<Waker>) -> bool {
        match wake {
            Some(wake) => self.background(path, wake, Arc::new(super::is_usable)),
            None => self.is_usable(path),
        }
    }

    fn is_usable(&self, path: &Path) -> bool {
        self.check(path, super::is_usable)
    }

    fn check(&self, path: &Path, validate: impl FnOnce(&Path) -> bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(before) = stamp(path) else {
            state.entries.remove(path);
            return false;
        };
        if let Some((_, Some(usable))) = state.entries.get(path).filter(|(cached, _)| *cached == before) {
            return *usable;
        }
        drop(state);
        let usable = validate(path);
        // A replacement during decoding must not cache the old file's answer for the new one.
        if stamp(path) != Some(before) {
            return false;
        }
        self.state.lock().unwrap_or_else(PoisonError::into_inner).insert(path, before, Some(usable));
        usable
    }

    fn background(&self, path: &Path, wake: Waker, validate: Validator) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(before) = stamp(path) else {
            state.entries.remove(path);
            return false;
        };
        if let Some((_, usable)) = state.entries.get(path).filter(|(cached, _)| *cached == before) {
            return usable.unwrap_or(false);
        }
        state.insert(path, before, None);
        state.queue.push_back((path.to_path_buf(), before));
        let start = !state.worker;
        state.worker = true;
        drop(state);
        if start {
            self.spawn(wake, validate);
        }
        false
    }

    fn spawn(&self, wake: Waker, validate: Validator) {
        let weak = Arc::downgrade(&self.state);
        let started = std::thread::Builder::new().name("lc-smart-status".into()).spawn(move || {
            while let Some(state) = weak.upgrade() {
                let (path, before) = {
                    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let Some(job) = state.queue.pop_front() else {
                        state.worker = false;
                        return;
                    };
                    job
                };
                let usable = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| validate(&path))).unwrap_or(false);
                let unchanged = stamp(&path) == Some(before);
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.entries.get(&path).is_some_and(|(cached, _)| *cached == before) {
                    if unchanged {
                        state.entries.insert(path, (before, Some(usable)));
                    } else {
                        state.entries.remove(&path);
                    }
                }
                drop(state);
                wake();
            }
        });
        if let Err(error) = started {
            log::warn!("Could not start smart preview status worker: {error}");
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.worker = false;
            state.queue.clear();
            state.entries.retain(|_, (_, usable)| usable.is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs::{File, FileTimes};
    use std::time::Duration;

    #[test]
    fn cold_app_queries_do_not_wait_for_pixel_validation_or_start_duplicate_workers() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        let root = crate::smart::tests::temp("status-background");
        let path = root.join("proxy.lcsp");
        std::fs::write(&path, b"proxy").unwrap();
        let cache = UsabilityCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let validator: Validator = Arc::new({
            let calls = calls.clone();
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                started_tx.send(std::thread::current().id()).unwrap();
                release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                true
            }
        });
        let wake: Waker = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        assert!(!cache.background(&path, wake.clone(), validator.clone()));
        let worker = started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(worker, std::thread::current().id());
        assert!(!cache.background(&path, wake.clone(), validator.clone()), "pending checks are coalesced");
        release_tx.send(()).unwrap();
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(cache.background(&path, wake, validator));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_decoder_panic_does_not_strand_the_status_worker() {
        use std::sync::mpsc;
        let root = crate::smart::tests::temp("status-worker-panic");
        let bad = root.join("bad");
        let good = root.join("good");
        std::fs::write(&bad, b"proxy").unwrap();
        std::fs::write(&good, b"proxy").unwrap();
        let cache = UsabilityCache::default();
        let (wake_tx, wake_rx) = mpsc::channel();
        let wake: Waker = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let validate: Validator = Arc::new(|path| {
            assert_ne!(path.file_name().unwrap(), "bad", "injected decoder panic");
            true
        });
        assert!(!cache.background(&bad, wake.clone(), validate.clone()));
        assert!(!cache.background(&good, wake.clone(), validate.clone()));
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!cache.background(&bad, wake.clone(), validate.clone()));
        assert!(cache.background(&good, wake, validate));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn app_status_queries_validate_cold_changed_and_repaired_proxies_in_background() {
        use serde_json::json;
        use std::sync::mpsc;
        let root = crate::smart::tests::temp("status-app-query");
        let mut session = crate::Session::with_demo();
        session.media.smart_dir = Some(root.clone());
        let id = session.catalog.photos().next().unwrap().id;
        let path = crate::smart::file_path(&root, session.catalog.photo(id).unwrap());
        let bytes = crate::smart::encode(&lightcraft_raster::Rgb32f::new(2, 2), None).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        session.media.availability.run_in_background(Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        let query = |session: &mut crate::Session| session.execute("photo.smartPreview", &json!({"id":id.0})).unwrap()["smartPreview"].clone();
        assert_eq!(query(&mut session), json!(false), "a cold app query returns before validation");
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(query(&mut session), json!(true));
        let (_, _, body) = crate::smart::format::split(&bytes).unwrap();
        let offset = bytes.len() - body.len();
        let mut corrupt = bytes.clone();
        corrupt[offset + 4..bytes.len() - 2].fill(0);
        let modified = stamp(&path).unwrap().modified + Duration::from_secs(10);
        std::fs::write(&path, corrupt).unwrap();
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(crate::smart::is_complete(&path), "the damage passes the structural check");
        assert_eq!(query(&mut session), json!(false), "the old valid answer does not survive a file change");
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(query(&mut session), json!(false), "a damaged proxy never becomes usable");
        std::fs::write(&path, bytes).unwrap();
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified + Duration::from_secs(10))).unwrap();
        assert_eq!(query(&mut session), json!(false));
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(query(&mut session), json!(true));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn status_reuses_validation_and_rechecks_changed_or_missing_files() {
        let root = crate::smart::tests::temp("status-cache");
        let path = root.join("proxy.lcsp");
        let cache = UsabilityCache::default();
        let calls = Cell::new(0);
        let validate = |_: &Path| {
            calls.set(calls.get() + 1);
            true
        };
        assert!(!cache.check(&path, validate));
        std::fs::write(&path, b"first").unwrap();
        assert!(cache.check(&path, validate));
        assert!(cache.check(&path, validate));
        assert_eq!(calls.get(), 1, "unchanged status queries do not decode again");
        let modified = stamp(&path).unwrap().modified + Duration::from_secs(10);
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(cache.check(&path, validate));
        assert_eq!(calls.get(), 2, "a new modification time triggers validation");
        std::fs::write(&path, b"longer").unwrap();
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(cache.check(&path, validate));
        assert_eq!(calls.get(), 3, "length also invalidates a coarse modification time");
        std::fs::write(root.join("other.lcsp"), b"longer").unwrap();
        assert!(cache.check(&root.join("other.lcsp"), validate));
        assert_eq!(calls.get(), 4, "another path needs its own validation");
        std::fs::remove_file(&path).unwrap();
        assert!(!cache.check(&path, validate));
        std::fs::write(&path, b"longer").unwrap();
        assert!(cache.check(&path, validate));
        assert_eq!(calls.get(), 5, "a removed file forgets its cached result");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn status_checks_pixels_and_recovers_after_repair() {
        let root = crate::smart::tests::temp("status-corrupt");
        let path = root.join("proxy.lcsp");
        let bytes = crate::smart::encode(&lightcraft_raster::Rgb32f::new(2, 2), None).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let cache = UsabilityCache::default();
        assert!(cache.is_usable(&path));
        let (_, _, body) = crate::smart::format::split(&bytes).unwrap();
        let offset = bytes.len() - body.len();
        let modified = stamp(&path).unwrap().modified + Duration::from_secs(10);
        let mut corrupt = bytes.clone();
        corrupt[offset + 4..bytes.len() - 2].fill(0);
        std::fs::write(&path, &corrupt).unwrap();
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(crate::smart::is_complete(&path), "the JPEG end marker alone does not prove pixel integrity");
        assert!(!cache.is_usable(&path));
        assert!(!cache.is_usable(&path), "invalid answers are cached too");
        std::fs::write(&path, &bytes).unwrap();
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified + Duration::from_secs(10))).unwrap();
        assert!(cache.is_usable(&path));
        let modified = stamp(&path).unwrap().modified + Duration::from_secs(10);
        File::options().write(true).open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(!cache.check(&path, |path| {
            std::fs::write(path, b"replaced during validation").unwrap();
            true
        }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
