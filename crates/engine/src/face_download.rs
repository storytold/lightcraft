//! Downloading a face model the user asked for (Settings ▸ Faces ▸ Download).
//!
//! LightCraft has no HTTP stack of its own (the usual TLS crates bring in C or assembly), so the
//! transfer is done by the computer's own `curl`, started hidden on a background thread. Only a model listed in
//! [`lightcraft_faces::known::download`] can be fetched: the address is a pinned constant, never something typed
//! or passed in. What arrives must match the model's recorded size and SHA-256 or it is thrown away; it then waits
//! in a staging folder until the user has read and accepted the model's terms (`faces.models.install`).
//!
//! Failure is a message, never a panic: no `curl`, no network, a stalled or truncated transfer, a full disk, a file
//! that is not the one expected, and a panic inside the thread itself all end as [`State::Failed`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_faces::hash::sha256_file;
use lightcraft_faces::known::Download;

/// The folder inside the models folder where a downloaded file waits for the user to accept its terms. Its name
/// starts with a dot, so it can never be a model's own folder (ids start with a letter or digit).
pub const STAGING: &str = ".downloads";

/// How often progress is looked at (and a cancel noticed).
const POLL: Duration = Duration::from_millis(100);

/// Where a download stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Fetching (`bytes` of `total` so far); once `bytes == total` the file is being checked.
    Running {
        bytes: u64,
        total: u64,
    },
    /// Fetched and verified, waiting in the staging folder at `path`.
    Done {
        path: PathBuf,
    },
    Failed(String),
    Cancelled,
}

struct Shared {
    state: Mutex<State>,
    cancel: AtomicBool,
}

impl Shared {
    fn new(total: u64) -> Self {
        Shared { state: Mutex::new(State::Running { bytes: 0, total }), cancel: AtomicBool::new(false) }
    }

    fn set(&self, s: State) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    fn get(&self) -> State {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// The downloads of one session, by model id.
#[derive(Default)]
pub struct Downloads {
    jobs: HashMap<String, Arc<Shared>>,
}

impl Downloads {
    /// Start fetching `spec` into `<models dir>/.downloads/`. One download runs at a time.
    pub fn start(&mut self, spec: Download, models_dir: &Path) -> Result<(), String> {
        if self.jobs.values().any(|j| matches!(j.get(), State::Running { .. })) {
            return Err("another model is still downloading: wait for it to finish, or cancel it".into());
        }
        let staging = models_dir.join(STAGING);
        std::fs::create_dir_all(&staging).map_err(|e| format!("could not create the download folder: {e}"))?;
        let shared = Arc::new(Shared::new(spec.size_bytes));
        let id = spec.id.clone();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("face-model-download".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&spec, &staging, &worker, &Fetcher::system())));
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
            // the thread notices, stops curl, deletes the partial file and marks itself cancelled
            State::Running { .. } => {}
            State::Done { path } => {
                let _ = std::fs::remove_file(path);
                self.jobs.remove(id);
            }
            State::Failed(_) | State::Cancelled => {
                self.jobs.remove(id);
            }
        }
        true
    }

    /// The staged file at `path` has been installed (its copy is the model now) or is no longer wanted.
    pub fn staged_file_used(&mut self, path: &Path) {
        self.jobs.retain(|_, j| !matches!(j.get(), State::Done { path: p } if p == path));
    }
}

impl Drop for Downloads {
    /// Quitting while a download runs stops it, and the `curl` it started, instead of leaving both behind.
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let started = std::time::Instant::now();
        while self.running() && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// How the file is fetched: the system `curl`, or (in tests) something else.
struct Fetcher {
    program: OsString,
    /// Only `https` addresses, also across redirects. Off only in tests, which fetch a local file.
    https_only: bool,
}

impl Fetcher {
    fn system() -> Self {
        // Windows 10 and later ship curl.exe in System32; asking for it there (not by a bare name that is looked up in
        // several folders) means nothing else on the machine can stand in for it
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            let p = Path::new(&root).join("System32").join("curl.exe");
            if p.is_file() {
                return Fetcher { program: p.into_os_string(), https_only: true };
            }
        }
        Fetcher { program: "curl".into(), https_only: true }
    }
}

enum FetchError {
    Cancelled,
    Failed(String),
}

fn curl_args(url: &str, part: &Path, max_bytes: u64, https_only: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    // fail on an HTTP error (no error page saved as the model), follow the redirects the hosts use, say nothing but errors;
    // give up if the connection cannot be made in 20 s or runs slower than 2 kB/s for 30 s
    for s in [
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "20",
        "--speed-limit",
        "2000",
        "--speed-time",
        "30",
    ] {
        a.push(s.into());
    }
    a.push("--max-filesize".into());
    a.push(max_bytes.to_string().into());
    if https_only {
        for s in ["--proto", "=https", "--proto-redir", "=https"] {
            a.push(s.into());
        }
    }
    a.push("--output".into());
    a.push(part.as_os_str().to_owned());
    a.push(url.into());
    a
}

/// What went wrong, in words the user can act on.
fn explain_exit(code: Option<i32>, stderr: &str) -> String {
    let detail: String = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(200).collect();
    let why = match code {
        Some(6) | Some(7) => "could not reach the server: check the internet connection",
        Some(22) => "the server refused the download",
        Some(28) => "the connection stalled or timed out",
        Some(63) => "the file is bigger than expected",
        Some(23) => "the file could not be written (is the disk full?)",
        _ => "the download failed",
    };
    if detail.is_empty() { why.to_string() } else { format!("{why} ({detail})") }
}

const NO_CURL: &str =
    "LightCraft downloads models with curl, which this computer does not have. Use “Open page” to get the file yourself, then drop it on the window";

fn fetch(fetcher: &Fetcher, spec: &Download, part: &Path, shared: &Shared) -> Result<(), FetchError> {
    if shared.cancelled() {
        return Err(FetchError::Cancelled);
    }
    let mut cmd = Command::new(&fetcher.program);
    cmd.args(curl_args(&spec.url, part, spec.size_bytes.saturating_add(4096), fetcher.https_only))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // no console window flashing up from the app
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| {
        FetchError::Failed(if e.kind() == std::io::ErrorKind::NotFound { NO_CURL.to_string() } else { format!("could not start curl: {e}") })
    })?;
    loop {
        if shared.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FetchError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut text = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.by_ref().take(4096).read_to_string(&mut text);
                }
                return Err(FetchError::Failed(explain_exit(status.code(), &text)));
            }
            Ok(None) => {
                let bytes = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0).min(spec.size_bytes);
                shared.set(State::Running { bytes, total: spec.size_bytes });
                std::thread::sleep(POLL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FetchError::Failed(format!("lost track of the download: {e}")));
            }
        }
    }
}

/// The whole job: reuse a verified earlier copy, else fetch, check and stage. Always ends in a final [`State`].
fn run(spec: &Download, staging: &Path, shared: &Shared, fetcher: &Fetcher) {
    let target = staging.join(&spec.file_name);
    let part = staging.join(format!("{}.part", spec.file_name));
    let _ = std::fs::remove_file(&part);
    let matches_spec = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.len() == spec.size_bytes) && sha256_file(p).is_ok_and(|h| h == spec.sha256);
    // fetched earlier and never installed: no need to fetch again
    if target.is_file() {
        if matches_spec(&target) {
            shared.set(State::Done { path: target });
            return;
        }
        let _ = std::fs::remove_file(&target);
    }
    let fail = |msg: String| {
        let _ = std::fs::remove_file(&part);
        shared.set(State::Failed(msg));
    };
    match fetch(fetcher, spec, &part, shared) {
        Ok(()) => {}
        Err(FetchError::Cancelled) => {
            let _ = std::fs::remove_file(&part);
            shared.set(State::Cancelled);
            return;
        }
        Err(FetchError::Failed(msg)) => return fail(msg),
    }
    shared.set(State::Running { bytes: spec.size_bytes, total: spec.size_bytes });
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if size != spec.size_bytes {
        return fail(format!("the download is incomplete or not the expected file ({size} bytes, expected {})", spec.size_bytes));
    }
    if !sha256_file(&part).is_ok_and(|h| h == spec.sha256) {
        return fail("the downloaded file is not the one LightCraft expects (its checksum differs), so it was thrown away".into());
    }
    if shared.cancelled() {
        let _ = std::fs::remove_file(&part);
        shared.set(State::Cancelled);
        return;
    }
    match std::fs::rename(&part, &target) {
        Ok(()) => shared.set(State::Done { path: target }),
        Err(e) => fail(format!("could not save the downloaded file: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_faces::hash::sha256_hex;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-face-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file_url(p: &Path) -> String {
        format!("file:///{}", p.to_string_lossy().replace('\\', "/").trim_start_matches('/'))
    }

    fn spec_for(bytes: &[u8], url: String) -> Download {
        Download { id: "test-model".into(), url, file_name: "test-model.onnx".into(), size_bytes: bytes.len() as u64, sha256: sha256_hex(bytes) }
    }

    /// Tests fetch a local file with the real curl (there on every CI image); where it is missing they say so and pass.
    fn local_fetcher() -> Option<Fetcher> {
        let f = Fetcher { program: "curl".into(), https_only: false };
        match Command::new(&f.program).arg("--version").stdout(Stdio::null()).status() {
            Ok(s) if s.success() => Some(f),
            _ => {
                eprintln!("curl is not installed here: skipping");
                None
            }
        }
    }

    #[test]
    fn curl_is_told_to_accept_https_only() {
        let a: Vec<String> =
            curl_args("https://example.org/m.onnx", Path::new("out.part"), 100, true).iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let at = |flag: &str| a.iter().position(|s| s == flag).and_then(|i| a.get(i + 1)).cloned();
        assert_eq!(at("--proto").as_deref(), Some("=https"));
        assert_eq!(at("--proto-redir").as_deref(), Some("=https"));
        assert_eq!(at("--max-filesize").as_deref(), Some("100"));
        assert_eq!(at("--output").as_deref(), Some("out.part"));
        assert!(a.contains(&"--fail".to_string()));
        assert_eq!(a.last().map(String::as_str), Some("https://example.org/m.onnx"));
        let open = curl_args("file:///x", Path::new("o"), 1, false);
        assert!(!open.iter().any(|s| s == "--proto"));
    }

    #[test]
    fn exit_codes_become_sentences() {
        assert!(explain_exit(Some(6), "curl: (6) Could not resolve host: x").contains("internet connection"));
        assert!(explain_exit(Some(22), "").contains("refused"));
        assert!(explain_exit(Some(28), "").contains("timed out"));
        assert_eq!(explain_exit(None, "\n\n"), "the download failed");
        // a hostile or enormous message is cut, never copied whole
        assert!(explain_exit(Some(1), &"x".repeat(100_000)).len() < 300);
        assert!(explain_exit(Some(1), &"é".repeat(1000)).chars().count() < 300);
    }

    #[test]
    fn fetches_checks_and_stages_a_file() {
        let Some(f) = local_fetcher() else { return };
        let dir = scratch("ok");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let src = dir.join("source.bin");
        std::fs::write(&src, &bytes).unwrap();
        let (staging, spec) = (dir.join(STAGING), spec_for(&bytes, file_url(&src)));
        std::fs::create_dir_all(&staging).unwrap();
        let shared = Shared::new(spec.size_bytes);
        run(&spec, &staging, &shared, &f);
        let State::Done { path } = shared.get() else { panic!("{:?}", shared.get()) };
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!staging.join("test-model.onnx.part").exists());
        // asked again with the copy still staged: no fetch needed (a program that does not exist would fail)
        let again = Shared::new(spec.size_bytes);
        run(&spec, &staging, &again, &Fetcher { program: "no-such-program-here".into(), https_only: true });
        assert!(matches!(again.get(), State::Done { .. }), "{:?}", again.get());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_does_not_match_is_thrown_away() {
        let Some(f) = local_fetcher() else { return };
        let dir = scratch("bad");
        let src = dir.join("source.bin");
        std::fs::write(&src, b"not the model at all").unwrap();
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        // the right size, the wrong content
        let mut spec = spec_for(b"not the model at all", file_url(&src));
        spec.sha256 = sha256_hex(b"something else entirely");
        let shared = Shared::new(spec.size_bytes);
        run(&spec, &staging, &shared, &f);
        let State::Failed(why) = shared.get() else { panic!("{:?}", shared.get()) };
        assert!(why.contains("checksum"), "{why}");
        // the wrong size
        spec.size_bytes += 10;
        let shared = Shared::new(spec.size_bytes);
        run(&spec, &staging, &shared, &f);
        assert!(matches!(shared.get(), State::Failed(w) if w.contains("incomplete")), "{:?}", shared.get());
        // nothing is left behind
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_or_program_is_a_message() {
        let dir = scratch("missing");
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        let spec = spec_for(b"abc", "https://example.invalid/m.onnx".into());
        let shared = Shared::new(3);
        run(&spec, &staging, &shared, &Fetcher { program: "no-such-program-here".into(), https_only: true });
        assert!(matches!(shared.get(), State::Failed(w) if w.contains("curl")), "{:?}", shared.get());
        if let Some(f) = local_fetcher() {
            let shared = Shared::new(3);
            run(&spec_for(b"abc", file_url(&dir.join("nope.bin"))), &staging, &shared, &f);
            assert!(matches!(shared.get(), State::Failed(_)), "{:?}", shared.get());
        }
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_download_leaves_nothing() {
        let dir = scratch("cancel");
        let staging = dir.join(STAGING);
        std::fs::create_dir_all(&staging).unwrap();
        let spec = spec_for(b"abc", "https://example.invalid/m.onnx".into());
        let shared = Shared::new(3);
        shared.cancel.store(true, Ordering::Relaxed);
        run(&spec, &staging, &shared, &Fetcher { program: "no-such-program-here".into(), https_only: true });
        assert_eq!(shared.get(), State::Cancelled);
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_the_session_stops_a_running_download() {
        let shared = Arc::new(Shared::new(10));
        // a stand-in for the download thread: notices the cancel flag the way `fetch` does
        let worker = shared.clone();
        let handle = std::thread::spawn(move || {
            while !worker.cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            worker.set(State::Cancelled);
        });
        let mut d = Downloads::default();
        d.jobs.insert("m".into(), shared.clone());
        assert!(d.running());
        let started = std::time::Instant::now();
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
        shared.set(State::Done { path: file.clone() });
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
