//! Downloading a denoise model explicitly requested after showing its terms.
//!
//! The transfer is [`lightcraft_fetch`] (pure-Rust HTTPS with resume, size and SHA-256 checks), on a background thread,
//! so the UI never waits for the network. Only a model listed in [`lightcraft_denoise::known::Download`] can be fetched:
//! the address and hash come from our pinned catalog or the user's model catalog, and no token is sent with it. What arrives
//! matches the model's recorded size and SHA-256 or it is thrown away; it then waits in a staging folder until the model
//! is installed (`denoise.models.install`, or by itself once the user has accepted the model's terms in the download).
//!
//! Failure is a message, never a panic: no network, a stalled or truncated transfer, a full disk, a file that is not the
//! one expected, and a panic inside the thread itself all end as [`State::Failed`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::activity::{Activity, Cancel, TaskHandle, Unit};

use lightcraft_denoise::known::Download;

/// The folder inside the models folder where a downloaded file waits to be installed. Its name starts with a dot, so it
/// can never be a model's own folder (ids start with a letter or digit).
pub const STAGING: &str = ".downloads";

/// Where a download stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Fetching (`bytes` of `total` so far); the file is checked as the last step.
    Running {
        bytes: u64,
        total: u64,
    },
    /// Fetched and verified (its SHA-256 is `sha256`), waiting in the staging folder at `path` to be installed.
    Done {
        path: PathBuf,
        sha256: String,
    },
    /// Installed and in use: nothing more to do but tell the user.
    Installing,
    Installed,
    Failed(String),
    Cancelled,
}

struct Shared {
    state: Mutex<State>,
    cancel: Arc<AtomicBool>,
    /// The download's row in the activity stack.
    task: Option<TaskHandle>,
}

impl Shared {
    fn new(total: u64) -> Self {
        Shared { state: Mutex::new(State::Running { bytes: 0, total }), cancel: Arc::new(AtomicBool::new(false)), task: None }
    }

    fn set(&self, s: State) {
        if let (State::Running { bytes, total }, Some(task)) = (&s, &self.task) {
            task.progress(*bytes, *total);
        }
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    fn get(&self) -> State {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The downloads of one session, by model id.
#[derive(Default)]
pub struct Downloads {
    jobs: HashMap<String, Arc<Shared>>,
}

impl Downloads {
    /// Start fetching `spec` into `<models dir>/.downloads/`. Different models can download side by side; the same
    /// one is started once.
    /// The activity stack shows it (as `name`) until the thread ends; its ✕ stops it like [`Downloads::discard`].
    pub fn start(&mut self, spec: Download, models_dir: &Path, activity: &Activity, name: &str) -> Result<(), String> {
        if self.jobs.get(&spec.id).is_some_and(|j| matches!(j.get(), State::Running { .. })) {
            return Err("that model is already downloading".into());
        }
        let staging = models_dir.join(STAGING);
        std::fs::create_dir_all(&staging).map_err(|e| format!("could not create the download folder: {e}"))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let task = activity.start("download", "Downloading model", Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail(name);
        task.progress(0, spec.size_bytes);
        let total = spec.size_bytes;
        let shared = Arc::new(Shared { state: Mutex::new(State::Running { bytes: 0, total }), cancel, task: Some(task.handle()) });
        let worker = shared.clone();
        let id = spec.id.clone();
        std::thread::Builder::new()
            .name("denoise-model-download".into())
            .spawn(move || {
                // the row goes when the thread ends, however it ends
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&spec, &spec.url, &staging, &worker)));
                if outcome.is_err() {
                    worker.set(State::Failed("the download stopped unexpectedly".into()));
                }
            })
            .map_err(|e| format!("could not start the download: {e}"))?;
        self.jobs.insert(id, shared);
        Ok(())
    }

    /// Every download this session has started and not discarded, by model id.
    pub fn snapshot(&self) -> Vec<(String, State)> {
        let mut all: Vec<_> = self.jobs.iter().map(|(id, j)| (id.clone(), j.get())).filter(|(_, s)| *s != State::Cancelled).collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    pub fn running(&self) -> bool {
        self.jobs.values().any(|j| matches!(j.get(), State::Running { .. }))
    }

    /// Stop a running download, or discard a finished one (its staged file is deleted). `true` if there was one.
    pub fn discard(&mut self, id: &str) -> bool {
        let Some(job) = self.jobs.get(id) else { return false };
        job.cancel.store(true, Ordering::Relaxed);
        match job.get() {
            // the thread notices, deletes the partial file and marks itself cancelled
            State::Running { .. } | State::Installing => {}
            State::Done { path, .. } => {
                let _ = std::fs::remove_file(path);
                self.jobs.remove(id);
            }
            State::Installed | State::Failed(_) | State::Cancelled => {
                self.jobs.remove(id);
            }
        }
        true
    }

    /// Downloads that have arrived and been checked, ready to install: (model id, staged file, its SHA-256).
    pub fn finished(&self) -> Vec<(String, PathBuf, String)> {
        let mut out: Vec<_> = self
            .jobs
            .iter()
            .filter_map(|(id, j)| match j.get() {
                State::Done { path, sha256 } => Some((id.clone(), path, sha256)),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Record how installing a finished download went (`Installed` or `Failed`).
    pub fn set_outcome(&mut self, id: &str, state: State) {
        match self.jobs.get(id) {
            Some(j) => j.set(state),
            None => {
                let j = Shared::new(0);
                j.set(state);
                self.jobs.insert(id.to_string(), Arc::new(j));
            }
        }
    }
}

impl Drop for Downloads {
    /// Quitting while a download runs stops it instead of leaving it behind.
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let started = web_time::Instant::now();
        while self.running() && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// What went wrong, in words the user can act on.
#[cfg(not(target_arch = "wasm32"))]
fn explain(e: &lightcraft_fetch::DownloadError) -> String {
    use lightcraft_fetch::DownloadError;
    match e {
        DownloadError::AllMirrorsFailed { errors, .. } => {
            let detail: String = errors.join("; ").chars().take(300).collect();
            format!("could not download the model ({detail}): check the internet connection, or get the file from its page and add it yourself")
        }
        other => other.to_string(),
    }
}

/// Fetch `spec` from its exact URL into `staging`, check it and leave it there.
#[cfg(not(target_arch = "wasm32"))]
fn fetch(spec: &Download, mirror: &str, staging: &Path, shared: &Shared) -> Result<(), lightcraft_fetch::DownloadError> {
    let file = lightcraft_fetch::FileSpec { name: &spec.file_name, size: Some(spec.size_bytes), sha256: Some(&spec.sha256), max: spec.size_bytes };
    lightcraft_fetch::download_url(&file, mirror, staging, &lightcraft_fetch::Options::default(), &shared.cancel, &mut |p| {
        shared.set(State::Running { bytes: p.done.min(spec.size_bytes), total: spec.size_bytes });
    })
}

#[cfg(target_arch = "wasm32")]
fn fetch(_: &Download, _: &str, _: &Path, _: &Shared) -> Result<(), String> {
    Err("the web build downloads no models".into())
}

/// The whole job: reuse a verified earlier copy, else fetch, check and stage. Always ends in a final [`State`].
fn run(spec: &Download, mirror: &str, staging: &Path, shared: &Shared) {
    let target = staging.join(&spec.file_name);
    let part = staging.join(format!("{}.part", spec.file_name));
    match fetch(spec, mirror, staging, shared) {
        Ok(()) => shared.set(State::Done { path: target, sha256: spec.sha256.to_string() }),
        #[cfg(not(target_arch = "wasm32"))]
        Err(lightcraft_fetch::DownloadError::Cancelled) => {
            let _ = std::fs::remove_file(&part);
            shared.set(State::Cancelled);
        }
        #[cfg(not(target_arch = "wasm32"))]
        Err(e) => {
            shared.set(State::Failed(explain(&e)));
        }
        #[cfg(target_arch = "wasm32")]
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            shared.set(State::Failed(e));
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use lightcraft_denoise::hash::sha256_hex;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-denoise-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn spec_for(bytes: &[u8]) -> Download {
        Download {
            id: "test-model".into(),
            url: "https://example.invalid/m/test-model.onnx".into(),
            file_name: "test-model.onnx".into(),
            size_bytes: bytes.len() as u64,
            sha256: sha256_hex(bytes),
        }
    }

    /// A local server that answers every request with `status` and `body` (the whole file, no ranges).
    fn serve(status: &'static str, body: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/m", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = s.write_all(&body);
            }
        });
        base
    }

    /// A local server that sends `body` a few kilobytes at a time: a download that keeps running for a while.
    fn serve_slowly(body: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/m", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                for chunk in body.chunks(4096) {
                    if s.write_all(chunk).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        });
        base
    }

    /// Poll until `done` holds (or fail after 10 s).
    fn wait(what: &str, done: impl Fn() -> bool) {
        let t = std::time::Instant::now();
        while !done() {
            assert!(t.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_running_download_shows_in_activity_and_cancels() {
        use crate::activity::{Activity, Unit};
        let dir = scratch("activity");
        let bytes: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
        let mut spec = spec_for(&bytes);
        spec.url = format!("{}/{}", serve_slowly(bytes), spec.file_name);
        let activity = Activity::default();
        let mut d = Downloads::default();
        d.start(spec, &dir, &activity, "Test Model").unwrap();
        let rows = activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("download", "Downloading model", "Test Model"));
        assert_eq!((rows[0].unit, rows[0].total), (Unit::Bytes, 400_000));
        assert!(rows[0].cancellable);
        activity.cancel(rows[0].id).unwrap();
        wait("the download never stopped", || !d.running());
        assert_eq!(d.jobs["test-model"].get(), State::Cancelled);
        wait("the row stayed", || activity.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetches_checks_and_stages_a_file() {
        let dir = scratch("ok");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let (staging, spec) = (dir.join(STAGING), spec_for(&bytes));
        std::fs::create_dir_all(&staging).unwrap();
        let mirror = serve("200 OK", bytes.clone());
        let shared = Shared::new(spec.size_bytes);
        run(&spec, &mirror, &staging, &shared);
        let State::Done { path, sha256 } = shared.get() else { panic!("{:?}", shared.get()) };
        assert_eq!((std::fs::read(&path).unwrap(), sha256), (bytes, spec.sha256.to_string()));
        assert!(!staging.join("test-model.onnx.part").exists());
        // asked again with the copy still staged: no fetch needed (nothing listens there)
        let again = Shared::new(spec.size_bytes);
        run(&spec, "http://127.0.0.1:1/m", &staging, &again);
        assert!(matches!(again.get(), State::Done { .. }), "{:?}", again.get());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_does_not_match_is_thrown_away() {
        let dir = scratch("bad");
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        // the right size and the wrong content
        let spec = spec_for(&[7u8; 5000]);
        let shared = Shared::new(spec.size_bytes);
        run(&spec, &serve("200 OK", vec![8u8; 5000]), &staging, &shared);
        assert!(matches!(shared.get(), State::Failed(_)), "{:?}", shared.get());
        // nothing is left behind: not the file, not a partial one
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_or_refusing_server_is_a_message() {
        let dir = scratch("down");
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        let spec = spec_for(b"abc");
        for mirror in ["http://127.0.0.1:1/m".to_string(), serve("404 Not Found", b"not found".to_vec())] {
            let shared = Shared::new(3);
            run(&spec, &mirror, &staging, &shared);
            let State::Failed(why) = shared.get() else { panic!("{:?}", shared.get()) };
            assert!(why.contains("internet connection"), "{why}");
        }
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_download_leaves_nothing() {
        let dir = scratch("cancel");
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        let spec = spec_for(b"abc");
        let shared = Shared::new(3);
        shared.cancel.store(true, Ordering::Relaxed);
        run(&spec, &serve("200 OK", b"abc".to_vec()), &staging, &shared);
        assert_eq!(shared.get(), State::Cancelled);
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_the_session_stops_a_running_download() {
        let shared = Arc::new(Shared::new(10));
        // a stand-in for the download thread: notices the cancel flag the way the downloader does
        let worker = shared.clone();
        let handle = std::thread::spawn(move || {
            while !worker.cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            worker.set(State::Cancelled);
        });
        let mut d = Downloads::default();
        d.jobs.insert("m".into(), shared.clone());
        assert!(d.running());
        let started = web_time::Instant::now();
        drop(d);
        assert!(started.elapsed() < Duration::from_millis(900), "quitting must not wait for a transfer to finish");
        assert_eq!(shared.get(), State::Cancelled);
        handle.join().unwrap();
    }

    #[test]
    fn discarding_a_finished_download_deletes_its_file() {
        let dir = scratch("discard");
        let file = dir.join("staged.onnx");
        std::fs::write(&file, b"x").unwrap();
        let mut d = Downloads::default();
        let shared = Arc::new(Shared::new(1));
        shared.set(State::Done { path: file.clone(), sha256: String::new() });
        d.jobs.insert("m".into(), shared);
        assert_eq!(d.snapshot().len(), 1);
        assert!(!d.running());
        assert!(d.discard("m"));
        assert!(!file.exists());
        assert!(d.snapshot().is_empty());
        assert!(!d.discard("m"), "nothing left to discard");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
