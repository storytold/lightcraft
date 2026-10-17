//! Automatic XMP sidecar writes made on a thread of their own, when the host turns them on
//! ([`crate::Session::set_background_sidecar_writes`]; the desktop app does). Writing a sidecar
//! reads the folder and the existing packet and writes a new one: on a network share that is many
//! round trips, which held up the command (and the frame) that changed the photo. Jobs are queued
//! per photo, and a photo changed again before its write started writes only its newest packet.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;

/// One photo's sidecar to write: where (the preferred name and the spellings an existing packet
/// may have), the fresh packet, and the time stamp a backup of an unreadable packet is named with.
pub(crate) struct Job {
    pub id: PhotoId,
    /// The photo's file: when it's gone by the time the write starts (moved or deleted), nothing is written.
    pub original: PathBuf,
    pub preferred: PathBuf,
    pub candidates: Vec<PathBuf>,
    pub packet: String,
    pub stamp: String,
}

/// A finished write: the sidecar's path and the bytes written (`None`: skipped, the photo's file is gone), or why it
/// failed.
pub(crate) type Done = (PhotoId, Result<Option<(PathBuf, Vec<u8>)>, String>);

#[derive(Default)]
struct State {
    queue: VecDeque<PhotoId>,
    jobs: HashMap<PhotoId, Job>,
    busy: usize,
    done: Vec<Done>,
    stop: bool,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub(crate) struct Writer {
    shared: Shared,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, State> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Writer {
    /// The writer thread, or `None` (and a log line) when it can't be started: the writes are then
    /// made as before, in the command.
    pub fn start() -> Option<Writer> {
        let shared: Shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = shared.clone();
        match std::thread::Builder::new().name("lc-sidecar-write".into()).spawn(move || run(&worker)) {
            Ok(thread) => Some(Writer { shared, thread: Some(thread) }),
            Err(e) => {
                log::warn!("could not start the XMP sidecar writer: {e}");
                None
            }
        }
    }

    /// Queue `job`, replacing a job for the same photo that hasn't started.
    pub fn submit(&self, job: Job) {
        let mut s = lock(&self.shared);
        let id = job.id;
        if s.jobs.insert(id, job).is_none() {
            s.queue.push_back(id);
        }
        self.shared.1.notify_all();
    }

    /// The writes finished since the last call.
    pub fn take_done(&self) -> Vec<Done> {
        std::mem::take(&mut lock(&self.shared).done)
    }

    /// Writes queued or being made.
    pub fn pending(&self) -> usize {
        let s = lock(&self.shared);
        s.jobs.len() + s.busy
    }

    /// Wait until every queued write is made, at most `timeout`. Whether it is idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        let mut s = lock(&self.shared);
        while !(s.jobs.is_empty() && s.busy == 0) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            s = self.shared.1.wait_timeout(s, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    }
}

impl Drop for Writer {
    /// The queued writes are still made (the thread ends when the queue is empty).
    fn drop(&mut self) {
        lock(&self.shared).stop = true;
        self.shared.1.notify_all();
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            log::warn!("the XMP sidecar writer ended with a panic");
        }
    }
}

fn run(shared: &Shared) {
    loop {
        let job = {
            let mut s = lock(shared);
            loop {
                if let Some(id) = s.queue.pop_front() {
                    if let Some(job) = s.jobs.remove(&id) {
                        s.busy += 1;
                        break job;
                    }
                    continue;
                }
                if s.stop {
                    return;
                }
                s = shared.1.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
        };
        // a panic here must not end the thread with its job counted as busy forever
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::sidecar::write_job(&job)))
            .unwrap_or_else(|_| Err(format!("{}: writing the sidecar failed", job.preferred.display())));
        let mut s = lock(shared);
        s.busy = s.busy.saturating_sub(1);
        s.done.push((job.id, result));
        shared.1.notify_all();
    }
}
