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

/// A finished write: the photo, the original it was queued for, and the sidecar's path and the bytes written (`None`:
/// skipped, the original wasn't there), or why it failed.
pub(crate) type Done = (PhotoId, PathBuf, Result<Option<(PathBuf, Vec<u8>)>, String>);

#[derive(Default)]
struct State {
    queue: VecDeque<PhotoId>,
    jobs: HashMap<PhotoId, Job>,
    busy: usize,
    done: Vec<Done>,
    stop: bool,
    /// The folder of the write being made.
    current: Option<PathBuf>,
    /// The batch running (jobs queued since the writer was last idle): how many are done of how many, and its row in
    /// the activity stack while it runs.
    batch_done: u64,
    batch_total: u64,
    row: Option<crate::activity::TaskHandle>,
    /// The thread has made every queued write and ended.
    exited: bool,
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
    /// A batch of writes holds a row in `activity` while it runs (the stack shows it once it is 0.5 s old, so a quick
    /// write after an edit doesn't flash one).
    pub fn start(activity: crate::activity::Activity) -> Option<Writer> {
        let shared: Shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = shared.clone();
        let thread = move || {
            run(&worker, &activity);
            lock(&worker).exited = true;
            worker.1.notify_all();
        };
        match std::thread::Builder::new().name("lc-sidecar-write".into()).spawn(thread) {
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
            s.batch_total += 1;
            if let Some(row) = &s.row {
                row.progress(s.batch_done, s.batch_total);
            }
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

    /// The folder of the write being made, or of the next one queued.
    pub fn stuck_folder(&self) -> Option<PathBuf> {
        let s = lock(&self.shared);
        s.current.clone().or_else(|| s.queue.front().and_then(|id| s.jobs.get(id)).and_then(|j| j.preferred.parent().map(PathBuf::from)))
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

/// How long dropping the writer waits for its queued writes: callers have usually waited for them already
/// ([`crate::Session::flush_sidecar_writes`]), so this only bounds a write that is stuck.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);

impl Writer {
    /// Stop taking jobs and wait at most `limit` for the queued ones. A write still stuck then (a network share that
    /// stopped answering) is left to its thread, which makes the rest of the queue and ends on its own: the caller
    /// (the UI thread, closing a library or turning background writes off) doesn't hang on it.
    fn shutdown(&mut self, limit: Duration) {
        let until = Instant::now() + limit;
        let mut s = lock(&self.shared);
        s.stop = true;
        self.shared.1.notify_all();
        while !s.exited {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            s = self.shared.1.wait_timeout(s, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        let exited = s.exited;
        drop(s);
        let Some(t) = self.thread.take() else { return };
        if !exited {
            log::warn!("XMP sidecar writes were still running at shutdown; they finish in the background");
        } else if t.join().is_err() {
            log::warn!("the XMP sidecar writer ended with a panic");
        }
    }
}

impl Drop for Writer {
    /// The queued writes are still made (the thread ends when the queue is empty).
    fn drop(&mut self) {
        self.shutdown(SHUTDOWN_LIMIT);
    }
}

fn run(shared: &Shared, activity: &crate::activity::Activity) {
    // the batch's row: no ✕, since a write left unmade would leave its sidecar behind the edit (a failed one waits for a
    // retry anyway)
    let mut row: Option<crate::activity::TaskGuard> = None;
    loop {
        let job = {
            let mut s = lock(shared);
            loop {
                if let Some(id) = s.queue.pop_front() {
                    if let Some(job) = s.jobs.remove(&id) {
                        s.busy += 1;
                        s.current = job.preferred.parent().map(PathBuf::from);
                        let guard = row.get_or_insert_with(|| activity.start("sidecars", "Writing XMP sidecars", crate::activity::Cancel::No));
                        guard.progress(s.batch_done, s.batch_total);
                        s.row = Some(guard.handle());
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
        s.current = None;
        s.batch_done += 1;
        if s.queue.is_empty() && s.busy == 0 {
            // the batch is over: its row goes
            (s.batch_done, s.batch_total, s.row) = (0, 0, None);
            row = None;
        } else if let Some(r) = &row {
            r.progress(s.batch_done, s.batch_total);
        }
        s.done.push((job.id, job.original, result));
        shared.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write that never finishes (a network share that stopped answering; here a FIFO nobody writes to) doesn't
    /// hold up whoever drops the writer for longer than the shutdown limit; the queued write is still made once the
    /// share answers again.
    /// A batch of writes holds one row in the activity stack ("Writing XMP sidecars", n of N, no ✕) for as long as it
    /// runs, and the row goes when the queue is empty. The stack itself shows a row only once it is 0.5 s old, so the
    /// quick write after an edit never flashes one; a slow share does.
    #[cfg(unix)]
    #[test]
    fn a_write_batch_holds_one_activity_row_while_it_runs() {
        let dir = std::env::temp_dir().join(format!("lc-sidecar-writer-activity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let activity = crate::activity::Activity::default();
        let mut w = Writer::start(activity.clone()).unwrap();
        let packet = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_string();
        let mut sidecars = Vec::new();
        for i in 0..3u64 {
            let original = dir.join(format!("{i}.png"));
            std::fs::write(&original, b"x").unwrap();
            let sidecar = dir.join(format!("{i}.xmp"));
            // the first write blocks on a FIFO until the test lets it go
            if i == 0 && !std::process::Command::new("mkfifo").arg(&sidecar).status().is_ok_and(|s| s.success()) {
                return;
            }
            w.submit(Job {
                id: PhotoId(i),
                original,
                preferred: sidecar.clone(),
                candidates: vec![sidecar.clone()],
                packet: packet.clone(),
                stamp: "s".into(),
            });
            sidecars.push(sidecar);
        }
        while lock(&w.shared).busy == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
        let rows = activity.list();
        assert_eq!(rows.len(), 1, "one row for the batch: {rows:?}");
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].total, rows[0].cancellable), ("sidecars", "Writing XMP sidecars", 3, false));
        drop(std::fs::OpenOptions::new().write(true).open(&sidecars[0]).unwrap());
        assert!(w.wait_idle(Duration::from_secs(10)));
        let until = Instant::now() + Duration::from_secs(5);
        while !activity.list().is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(activity.list().is_empty(), "the row goes with the batch");
        w.shutdown(Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn shutting_down_a_blocked_writer_waits_only_so_long() {
        let dir = std::env::temp_dir().join(format!("lc-sidecar-writer-blocked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("a.png");
        std::fs::write(&original, b"x").unwrap();
        let sidecar = dir.join("a.xmp");
        if !std::process::Command::new("mkfifo").arg(&sidecar).status().is_ok_and(|s| s.success()) {
            return;
        }
        let mut w = Writer::start(crate::activity::Activity::default()).unwrap();
        let packet = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_string();
        w.submit(Job { id: PhotoId(1), original, preferred: sidecar.clone(), candidates: vec![sidecar.clone()], packet, stamp: "s".into() });
        // the write has started (and is stuck reading the FIFO)
        while lock(&w.shared).busy == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
        let start = Instant::now();
        w.shutdown(Duration::from_millis(200));
        assert!(start.elapsed() < Duration::from_secs(5), "returned after {:?}", start.elapsed());
        // the share answers: the write goes on and lands
        drop(std::fs::OpenOptions::new().write(true).open(&sidecar).unwrap());
        let until = Instant::now() + Duration::from_secs(30);
        while std::fs::symlink_metadata(&sidecar).is_ok_and(|m| !m.is_file()) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        // (still the FIFO, reading it would wait for a writer forever: a slow machine fails here instead)
        assert!(std::fs::symlink_metadata(&sidecar).is_ok_and(|m| m.is_file()), "the queued write replaced the FIFO");
        assert!(std::fs::read_to_string(&sidecar).is_ok_and(|s| s.contains("xmpmeta")), "the queued write was made");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
