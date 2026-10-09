//! The model download as a background task the session starts, watches and cancels without
//! ever blocking: progress lives in atomics, messages behind a mutex only ever `try_lock`ed
//! by the session (the download thread holds it for a few instructions at a time).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dac_segment::fetch::{self, Options, Progress};

/// What the session sees of a download.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub running: bool,
    pub done: u64,
    pub total: u64,
    /// The file being fetched.
    pub file: String,
    /// The last download's error (cleared when a new one starts).
    pub error: Option<String>,
    /// The last download finished and the files are in place.
    pub finished: bool,
}

#[derive(Default)]
struct Text {
    file: String,
    error: Option<String>,
}

#[derive(Default)]
struct Shared {
    running: AtomicBool,
    cancel: AtomicBool,
    finished: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    text: Mutex<Text>,
}

/// Clears `running` when the download thread ends, however it ends.
struct Running(Arc<Shared>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct Downloader {
    shared: Arc<Shared>,
    /// The last text seen (when the download thread held the lock at that moment).
    last: Mutex<Text>,
}

fn lock(m: &Mutex<Text>) -> std::sync::MutexGuard<'_, Text> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Downloader {
    pub fn status(&self) -> Status {
        let s = &self.shared;
        if let Ok(t) = s.text.try_lock()
            && let Ok(mut last) = self.last.try_lock()
        {
            last.file.clone_from(&t.file);
            last.error.clone_from(&t.error);
        }
        // (only the session's thread locks `last`)
        let (file, error) = self.last.try_lock().map(|l| (l.file.clone(), l.error.clone())).unwrap_or_default();
        Status {
            running: s.running.load(Ordering::SeqCst),
            done: s.done.load(Ordering::SeqCst),
            total: s.total.load(Ordering::SeqCst),
            file,
            error,
            finished: s.finished.load(Ordering::SeqCst),
        }
    }

    pub fn running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// Ask a running download to stop (it keeps its partial files to resume later).
    pub fn cancel(&self) -> bool {
        let running = self.running();
        if running {
            self.shared.cancel.store(true, Ordering::SeqCst);
        }
        running
    }

    /// Start downloading `files` from `mirrors` into `dir` on a background thread. False when
    /// one is already running.
    pub fn start(&self, files: &'static [fetch::FileSpec], mirrors: Vec<String>, dir: PathBuf, opts: Options) -> Result<bool, String> {
        let s = self.shared.clone();
        if s.running.swap(true, Ordering::SeqCst) {
            return Ok(false);
        }
        s.cancel.store(false, Ordering::SeqCst);
        s.finished.store(false, Ordering::SeqCst);
        s.done.store(0, Ordering::SeqCst);
        s.total.store(files.iter().filter_map(|f| f.size).sum(), Ordering::SeqCst);
        // nothing else runs while `running` was false
        *lock(&s.text) = Text::default();
        *lock(&self.last) = Text::default();
        let guard = Running(s.clone());
        let spawned = std::thread::Builder::new().name("sam3-download".into()).spawn(move || {
            let s = guard.0.clone();
            let mut last_file = String::new();
            let mut on_progress = |p: &Progress| {
                s.done.store(p.done, Ordering::SeqCst);
                s.total.store(p.total, Ordering::SeqCst);
                if p.file != last_file {
                    last_file.clone_from(&p.file);
                    lock(&s.text).file.clone_from(&p.file);
                }
            };
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch::download(files, &mirrors, &dir, &opts, &s.cancel, &mut on_progress)));
            let error = match r {
                Ok(Ok(())) => {
                    s.finished.store(true, Ordering::SeqCst);
                    log::info!("SAM 3 model downloaded to {}", dir.display());
                    None
                }
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some("the download failed unexpectedly".to_string()),
            };
            if let Some(e) = &error {
                log::warn!("SAM 3 download: {e}");
            }
            lock(&s.text).error = error;
            drop(guard);
        });
        match spawned {
            Ok(_) => Ok(true),
            Err(e) => {
                // the closure (and its guard) was dropped: `running` is false again
                Err(format!("could not start the download: {e}"))
            }
        }
    }
}
