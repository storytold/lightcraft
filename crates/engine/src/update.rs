//! Auto updates: checking the release feed, downloading the file for this machine, and starting
//! the install ([`lcc_updater`]).
//!
//! A check runs on a worker thread — the UI never waits for the network — and its result stays in
//! [`Check`] until the next one. A download shows in the activity stack (kind `update`,
//! cancellable) and, once it has arrived and checked out, waits in the cache folder until the user
//! says *Install and Relaunch*. Everything that can go wrong is a sentence in the status or the
//! command's error, never a panic, and a failed update leaves the running installation alone.
//!
//! Settings (check at startup, include prereleases) live in the app's own settings folder
//! (`updates.json`), not the library's: there is one installation per machine, and the updater
//! works with no library open.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::activity::{Activity, Cancel, Unit};

/// The `kind` of the download's activity row.
pub const ACTIVITY_KIND: &str = "update";
/// The row's label (English; the frontend translates it).
pub const ACTIVITY_LABEL: &str = "Downloading update";

/// Settings file in the app's settings folder.
const SETTINGS_FILE: &str = "updates.json";
/// Schema version of that file; a file from a newer build is left alone.
const SETTINGS_VERSION: u32 = 1;

/// What the user chose about updates (persisted in `updates.json`). The default is all off: no
/// start-up check, no prereleases, nothing seen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// Ask the release feed once when the app starts. Off until the user turns it on: a file
    /// that doesn't mention it (or no file) means off; a saved choice is kept as it is.
    pub check_at_startup: bool,
    /// Treat prereleases (`1.2.0-rc.1`) as updates.
    pub prerelease: bool,
    /// The last version the user answered about, so a start-up check does not ask twice.
    pub seen_version: Option<SeenVersion>,
}

/// The last version the user was shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenVersion {
    pub version: String,
    pub skipped: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SettingsFile {
    version: u32,
    updates: Prefs,
}

/// How a downloaded update is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Msi,
    Dmg,
    AppImage,
    Archive,
    Checksums,
}

impl Kind {
    pub fn text(self) -> &'static str {
        match self {
            Kind::Msi => "msi",
            Kind::Dmg => "dmg",
            Kind::AppImage => "appimage",
            Kind::Archive => "archive",
            Kind::Checksums => "checksums",
        }
    }

    fn of(kind: lcc_updater::Kind) -> Kind {
        match kind {
            lcc_updater::Kind::Msi => Kind::Msi,
            lcc_updater::Kind::Dmg => Kind::Dmg,
            lcc_updater::Kind::AppImage => Kind::AppImage,
            lcc_updater::Kind::Archive => Kind::Archive,
            lcc_updater::Kind::Checksums => Kind::Checksums,
        }
    }

    fn to_crate(self) -> lcc_updater::Kind {
        match self {
            Kind::Msi => lcc_updater::Kind::Msi,
            Kind::Dmg => lcc_updater::Kind::Dmg,
            Kind::AppImage => lcc_updater::Kind::AppImage,
            Kind::Archive => lcc_updater::Kind::Archive,
            Kind::Checksums => lcc_updater::Kind::Checksums,
        }
    }
}

/// A newer release the feed offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub version: String,
    pub current: String,
    pub title: String,
    pub tag: String,
    pub published: Option<String>,
    pub notes: String,
    /// The file's name, as the release spells it.
    pub file: String,
    /// Bytes, when the feed says (0: unknown).
    pub size: u64,
    /// One line on what installing it will do on this machine.
    pub action: String,
    /// The install kind.
    pub kind: Kind,
    /// Where it downloads from. Not shown in the status: it is long, and the dialog shows the file.
    pub url: String,
    /// The SHA-256 to check it against, when the release publishes one.
    pub sha256: Option<String>,
}

impl Available {
    fn of(u: &lcc_updater::Update) -> Available {
        Available {
            version: u.version.to_text(),
            current: u.current.to_text(),
            title: u.title.clone(),
            tag: u.tag.clone(),
            published: u.published.clone(),
            notes: u.notes.clone(),
            file: u.asset.name.clone(),
            size: u.asset.size,
            action: u.action.clone(),
            kind: Kind::of(u.asset.kind),
            url: u.asset.url.clone(),
            sha256: u.sha256.clone(),
        }
    }

    /// The update as the downloader needs it.
    fn to_crate(&self) -> lcc_updater::Update {
        lcc_updater::Update {
            current: parse_or_current(&self.current),
            version: parse_or_current(&self.version),
            title: self.title.clone(),
            tag: self.tag.clone(),
            asset: lcc_updater::Asset {
                name: self.file.clone(),
                url: self.url.clone(),
                size: self.size,
                sha256: self.sha256.clone(),
                kind: self.kind.to_crate(),
            },
            sha256: self.sha256.clone(),
            published: self.published.clone(),
            notes: self.notes.clone(),
            action: self.action.clone(),
        }
    }
}

fn parse_or_current(text: &str) -> lcc_updater::Version {
    lcc_updater::Version::parse(text).unwrap_or_else(|_| lcc_updater::Version::current())
}

/// A finished download, waiting to be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the file as it was written.
    pub sha256: String,
    pub file: String,
    pub kind: Kind,
    /// The release notes, kept with the file so the dialog still has them.
    pub notes: String,
}

/// What the last check found.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Check {
    /// No check has run yet, or the last one failed.
    #[default]
    Never,
    /// One is running on a worker thread.
    Running,
    /// This build is the newest the feed knows of.
    Current,
    /// A newer release is available.
    Found(Box<Available>),
}

/// Where the download of the newest release stands.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    Idle,
    /// Fetching `bytes` of `total` (`total` 0 while the size is unknown).
    Running { bytes: u64, total: u64 },
    /// Arrived and checked; waiting for *Install and Relaunch*.
    Ready(Box<Staged>),
    /// The transfer failed (the user can try again).
    Failed(String),
    /// The user cancelled it.
    Cancelled,
}

/// What an install left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub message: String,
    /// Whether the app has to quit for the install to finish.
    pub exit: bool,
}

/// A download in flight: its progress and the flag that stops it.
///
/// The activity row belongs to the worker thread (`TaskGuard` is not `Sync`, and the row should go
/// when the transfer ends, however it ends), so nothing here holds a task handle.
struct Job {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The updater's state in one session.
pub struct Updater {
    prefs: Prefs,
    /// Where `updates.json` lives (`None`: an in-memory session; the settings are not written).
    dir: Option<PathBuf>,
    /// A feed URL that replaces the release's own: Rust callers only (a test, a mirror); no
    /// command sets it, and `update.status` shows it as `feed`.
    manifest_url: Option<String>,
    /// Tests only: one more origin whose feed and files are trusted, plain `http` included
    /// ([`lcc_updater::Options::extra_origin`]). Only this crate's tests can set it.
    extra_origin: Option<String>,
    /// Where downloads are staged.
    stage: PathBuf,
    cache: lcc_updater::Cache,
    check: Check,
    progress: Progress,
    installed: Option<Installed>,
    /// When the last check finished (ISO 8601).
    checked_at: Option<String>,
    /// How a running check reports back.
    worker: Option<Receiver<lcc_updater::Checked>>,
    /// Why the last check or download failed, in words.
    error: Option<String>,
    /// The download in flight.
    job: Option<Job>,
    /// Whether this copy runs from an AppImage, the only kind an AppImage update can replace.
    appimage: bool,
}

impl Default for Updater {
    /// The updater a new session gets: settings in the app's own folder, downloads beside them.
    fn default() -> Self {
        Updater::with_default_dir()
    }
}

impl Updater {
    /// An updater with no settings folder of its own.
    pub fn new(dir: Option<PathBuf>) -> Updater {
        let prefs = dir.as_deref().map(load_prefs).unwrap_or_default();
        let stage = stage_for(dir.as_deref());
        Updater {
            prefs,
            dir,
            manifest_url: None,
            extra_origin: None,
            stage,
            cache: lcc_updater::Cache::default(),
            check: Check::Never,
            progress: Progress::Idle,
            installed: None,
            checked_at: None,
            worker: None,
            error: None,
            job: None,
            appimage: lcc_updater::runs_from_appimage(),
        }
    }

    /// [`Updater::new`] at the app's settings folder, downloads staged in its `updates` folder.
    pub fn with_default_dir() -> Updater {
        Updater::new(crate::config::config_dir())
    }

    /// Where downloads are staged.
    pub fn stage_dir(&self) -> &Path {
        &self.stage
    }

    /// Whether the settings are remembered between runs.
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    // ---------- settings

    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }

    /// Change the settings; `Ok(true)` when something changed and was written. When the file
    /// can't be written the settings are left as they were (the app never acts on a setting that
    /// isn't on disk) and the error names the file and why.
    pub fn set_prefs(&mut self, check_at_startup: Option<bool>, prerelease: Option<bool>) -> Result<bool, String> {
        let before = self.prefs.clone();
        if let Some(v) = check_at_startup {
            self.prefs.check_at_startup = v;
        }
        if let Some(v) = prerelease {
            self.prefs.prerelease = v;
        }
        if self.prefs == before {
            return Ok(false);
        }
        if let Err(e) = self.save() {
            self.prefs = before;
            return Err(e);
        }
        Ok(true)
    }

    /// Remember which version the user answered about, so the next start-up check stays quiet
    /// about it. Bookkeeping: if the file can't be written this is only logged and kept for this
    /// session (the worst outcome is being asked about the version again after a restart).
    pub fn mark_seen(&mut self, version: &str, skipped: bool) {
        self.prefs.seen_version = Some(SeenVersion { version: version.to_string(), skipped });
        if let Err(e) = self.save() {
            log::warn!("updates: {e}");
        }
    }

    /// Whether the user has already been shown `version`.
    pub fn already_seen(&self, version: &str) -> bool {
        self.prefs.seen_version.as_ref().is_some_and(|s| s.version == version)
    }

    /// Write `updates.json` (nothing in an in-memory session). The error names the file and the
    /// reason.
    fn save(&self) -> Result<(), String> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let path = dir.join(SETTINGS_FILE);
        let file = SettingsFile { version: SETTINGS_VERSION, updates: self.prefs.clone() };
        let text = serde_json::to_vec_pretty(&file).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", path.display()))?;
        // written beside and renamed: a crash mid-write cannot leave a half file behind
        let part = dir.join(format!("{SETTINGS_FILE}.part"));
        std::fs::write(&part, &text).and_then(|()| std::fs::rename(&part, &path)).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("{}: {e}", path.display())
        })
    }

    // ---------- state

    pub fn check(&self) -> &Check {
        &self.check
    }

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    pub fn installed(&self) -> Option<&Installed> {
        self.installed.as_ref()
    }

    /// Forget the *Install and Relaunch* outcome (the dialog says it once).
    pub fn clear_installed(&mut self) {
        self.installed = None;
    }

    /// Whether a check is in flight.
    pub fn checking(&self) -> bool {
        matches!(self.check, Check::Running)
    }

    /// Whether a download is in flight.
    pub fn downloading(&self) -> bool {
        matches!(self.progress, Progress::Running { .. })
    }

    /// The newer release on offer, when there is one.
    pub fn available(&self) -> Option<&Available> {
        match &self.check {
            Check::Found(a) => Some(a),
            _ => None,
        }
    }

    /// The downloaded file waiting to be installed, when there is one.
    pub fn staged(&self) -> Option<&Staged> {
        match &self.progress {
            Progress::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// Everything about updates a frontend shows (`update.status`).
    pub fn status(&self) -> Value {
        let mut v = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": platform_text(),
            "supported": platform_supported(),
            "check": match &self.check {
                Check::Never => "never",
                Check::Running => "running",
                Check::Current => "current",
                Check::Found(_) => "found",
            },
            "download": match &self.progress {
                Progress::Idle => "idle",
                Progress::Running { .. } => "running",
                Progress::Ready(_) => "ready",
                Progress::Failed(_) => "failed",
                Progress::Cancelled => "cancelled",
            },
            "checkAtStartup": self.prefs.check_at_startup,
            "prerelease": self.prefs.prerelease,
            "lastChecked": self.checked_at,
            "seenVersion": self.prefs.seen_version.as_ref().map(|s| json!({"version": s.version, "skipped": s.skipped})),
            "repo": lcc_brand::REPOSITORY,
            "page": format!("{}/releases", lcc_brand::REPOSITORY),
            "staging": self.stage.to_string_lossy(),
        });
        // a feed other than the release's own is never silent
        if let Some(feed) = &self.manifest_url {
            v["feed"] = json!(feed);
        }
        if let Check::Found(a) = &self.check {
            v["available"] = available_json(a);
        }
        match &self.progress {
            Progress::Running { bytes, total } => v["progress"] = json!({"bytes": bytes, "total": total}),
            Progress::Ready(s) => v["staged"] = staged_json(s),
            Progress::Failed(why) => v["error"] = json!(why),
            _ => {}
        }
        if let Some(e) = &self.error {
            v["error"] = json!(e);
        }
        if let Some(i) = &self.installed {
            v["installed"] = json!({"version": i.version, "message": i.message, "exit": i.exit});
        }
        v
    }

    // ---------- checking

    /// The options a check runs with (the user's prerelease choice included, and the feed override
    /// when one is set).
    pub fn options(&self) -> lcc_updater::Options {
        lcc_updater::Options {
            prerelease: self.prefs.prerelease,
            manifest_url: self.manifest_url.clone(),
            extra_origin: self.extra_origin.clone(),
            ..Default::default()
        }
    }

    /// Read the release feed from `url` instead of the project's (a mirror or a test feed; it must
    /// be `https://`). `None` goes back to the default. Rust API only: no command reaches it, and
    /// the files the feed offers must still be on this repository's releases.
    pub fn set_manifest_url(&mut self, url: Option<String>) {
        self.manifest_url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    }

    /// Tests only: trust `origin` (`http://127.0.0.1:<port>`) for the feed and its files, as a
    /// local release server needs. Compiled into this crate's tests and nowhere else, so no
    /// command, control request or MCP tool can reach it.
    #[cfg(test)]
    pub(crate) fn trust_test_origin(&mut self, origin: Option<String>) {
        self.extra_origin = origin.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    }

    /// The feed URL in use (`None`: the project's own).
    pub fn manifest_url(&self) -> Option<&str> {
        self.manifest_url.as_deref()
    }

    /// Start a check on a worker thread. `false` when one is already running.
    pub fn start_check(&mut self) -> bool {
        if self.checking() {
            return false;
        }
        let opts = self.options();
        // an answer from the last few seconds costs no request
        if let Some(cached) = self.cache.get(&opts) {
            self.apply(cached);
            return true;
        }
        if !self.cache.begin() {
            return false;
        }
        self.check = Check::Running;
        self.error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("lc-update-check".into()).spawn(move || {
            let checked = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lcc_updater::check(&opts, None, None))) {
                Ok(Ok(Some(u))) => lcc_updater::Checked::Found(Box::new(u)),
                Ok(Ok(None)) => lcc_updater::Checked::Current(env!("CARGO_PKG_VERSION").to_string()),
                Ok(Err(e)) => lcc_updater::Checked::Failed(e.to_string()),
                Err(_) => lcc_updater::Checked::Failed("the check stopped unexpectedly".into()),
            };
            // the receiver is gone when the session ended first: nothing to report, no panic
            let _ = tx.send(checked);
        });
        match spawned {
            Ok(_) => {
                self.worker = Some(rx);
                true
            }
            Err(e) => {
                self.check = Check::Never;
                self.error = Some(format!("could not start the check: {e}"));
                self.cache.end();
                false
            }
        }
    }

    /// Run a check in this thread (a one-shot caller: the CLI, or `wait: true`).
    pub fn check_now(&mut self) {
        let opts = self.options();
        let checked = match lcc_updater::check(&opts, None, None) {
            Ok(Some(u)) => lcc_updater::Checked::Found(Box::new(u)),
            Ok(None) => lcc_updater::Checked::Current(env!("CARGO_PKG_VERSION").to_string()),
            Err(e) => lcc_updater::Checked::Failed(e.to_string()),
        };
        self.apply(checked);
    }

    /// Take a finished check's or download's result. `true` when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let got = match self.worker.as_ref().map(Receiver::try_recv) {
            Some(Ok(checked)) => Some(checked),
            Some(Err(TryRecvError::Disconnected)) => Some(lcc_updater::Checked::Failed("the check stopped unexpectedly".into())),
            // still running, or no check at all
            _ => None,
        };
        if let Some(checked) = got {
            self.worker = None;
            self.apply(checked);
            changed = true;
        }
        if let (Some(job), Progress::Running { .. }) = (&self.job, &self.progress) {
            let now = lock(&job.progress).clone();
            if now != self.progress {
                self.progress = now;
                changed = true;
            }
            if matches!(self.progress, Progress::Ready(_) | Progress::Failed(_) | Progress::Cancelled) {
                self.job = None;
                changed = true;
            }
        }
        changed
    }

    /// Wait for a running check to finish (blocking; a one-shot caller asks for this).
    pub fn wait_for_check(&mut self) {
        while self.checking() {
            if !self.poll() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    /// Wait for a running download to finish (blocking).
    pub fn wait_for_download(&mut self) {
        while self.downloading() {
            if !self.poll() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    /// Record what a check found.
    pub fn apply(&mut self, checked: lcc_updater::Checked) {
        self.checked_at = Some(iso_now());
        self.cache.put(&self.options(), checked.clone());
        self.check = match checked {
            lcc_updater::Checked::Found(u) => Check::Found(Box::new(Available::of(&u))),
            lcc_updater::Checked::Current(_) => Check::Current,
            lcc_updater::Checked::Failed(why) => {
                log::info!("updates: {why}");
                self.error = Some(why);
                // a failed check says nothing about this build: the next one may succeed
                Check::Never
            }
        };
        self.cache.end();
        // a newer release replaces a file staged for an older one
        if let (Some(a), Progress::Ready(s)) = (self.available(), &self.progress)
            && s.version != a.version
        {
            self.progress = Progress::Idle;
        }
    }

    // ---------- downloading

    /// Start fetching the offered release. `Err` says why it cannot start.
    pub fn start_download(&mut self, activity: &Activity) -> Result<(), String> {
        if self.downloading() {
            return Err("an update is already downloading".into());
        }
        let available = self.available().cloned().ok_or("there is no update to download: check again first")?;
        if available.kind == Kind::Checksums {
            return Err("that release offers no file for this system".into());
        }
        if available.kind == Kind::AppImage && !self.appimage {
            let why = format!(
                "this copy of the app was not installed from an AppImage, so version {} can't replace it; update it the way it was installed (your package manager, or the releases page)",
                available.version
            );
            // the dialog shows why, instead of fetching a file it can't install
            self.progress = Progress::Failed(why.clone());
            return Err(why);
        }
        if let Err(e) = std::fs::create_dir_all(&self.stage) {
            return Err(format!("could not create {}: {e}", self.stage.display()));
        }
        self.error = None;
        self.progress = Progress::Running { bytes: 0, total: available.size };
        let progress = Arc::new(Mutex::new(self.progress.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        let task = activity.start(ACTIVITY_KIND, ACTIVITY_LABEL, Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail(&available.file);
        task.progress(0, available.size);
        // progress from the worker does not keep the row alive; the guard below does
        let handle = task.handle();
        let stage = self.stage.clone();
        let update = available.to_crate();
        let worker_progress = progress.clone();
        let worker_cancel = cancel.clone();
        let spawned = std::thread::Builder::new().name("lc-update-download".into()).spawn(move || {
            // the activity row lives exactly as long as the transfer
            let _task = task;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let client = lcc_updater::client().map_err(|e| e.to_string())?;
                let mut report = |bytes: u64, total: Option<u64>| {
                    let total = total.unwrap_or(update.asset.size);
                    *lock(&worker_progress) = Progress::Running { bytes, total };
                    handle.progress(bytes, total);
                };
                let got =
                    lcc_updater::download(&update, &client, &stage, Some(worker_cancel.clone()), Some(&mut report)).map_err(|e| e.to_string())?;
                Ok::<_, String>(Staged {
                    version: update.version.to_text(),
                    path: got.path,
                    bytes: got.bytes,
                    sha256: got.sha256,
                    file: got.name,
                    kind: Kind::of(update.asset.kind),
                    notes: update.notes.clone(),
                })
            }));
            let end = match outcome {
                Ok(Ok(staged)) => Progress::Ready(Box::new(staged)),
                // a cancelled transfer reports the failure the net layer saw; the flag says which
                Ok(Err(_)) if worker_cancel.load(Ordering::Relaxed) => Progress::Cancelled,
                Ok(Err(why)) => Progress::Failed(why),
                Err(_) => Progress::Failed("the download stopped unexpectedly".into()),
            };
            *lock(&worker_progress) = end;
            // the caller takes the result on its next poll
        });
        match spawned {
            Ok(_) => {
                self.job = Some(Job { progress, cancel });
                Ok(())
            }
            Err(e) => {
                self.progress = Progress::Failed(format!("could not start the download: {e}"));
                Err(format!("could not start the download: {e}"))
            }
        }
    }

    /// Stop a running download: the transfer notices and leaves nothing behind.
    pub fn cancel_download(&mut self) -> bool {
        match &self.job {
            Some(job) => {
                job.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Remember how an install ended.
    pub fn set_installed(&mut self, installed: Installed) {
        self.installed = Some(installed);
    }

    /// Record where a download stands (the worker writes through its own handle; this is for a
    /// caller that has the file already, and for tests).
    pub fn set_progress(&mut self, progress: Progress) {
        self.progress = progress;
    }
}

impl Drop for Updater {
    /// Quitting while a download runs stops it instead of leaving a half file behind.
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Where downloads are staged for a settings folder `dir` (a temporary folder without one).
fn stage_for(dir: Option<&Path>) -> PathBuf {
    match dir {
        Some(d) => lcc_updater::stage_dir(d),
        None => std::env::temp_dir().join(format!("{}-updates", lcc_brand::BINARY)),
    }
}

/// Tidy up after an update, once per app start, on a thread of its own so the start never waits
/// for it: log why the last AppImage swap did not happen, if it didn't, and delete the previous
/// AppImage (`<file>.old` beside `$APPIMAGE`) a successful one left. Nothing to do on other
/// installs. Never fails: what it can't do is logged.
pub fn clean_up_after_update() {
    let stage = stage_for(crate::config::config_dir().as_deref());
    let spawned = std::thread::Builder::new().name("update-cleanup".into()).spawn(move || lcc_updater::clean_up_after_update(&stage));
    if let Err(e) = spawned {
        log::warn!("updates: could not start the clean-up after an update: {e}");
    }
}

fn load_prefs(dir: &Path) -> Prefs {
    let path = dir.join(SETTINGS_FILE);
    let Ok(text) = std::fs::read(&path) else { return Prefs::default() };
    match serde_json::from_slice::<SettingsFile>(&text) {
        Ok(f) if f.version <= SETTINGS_VERSION => f.updates,
        Ok(f) => {
            log::warn!("updates: {} was written by a newer version ({})", path.display(), f.version);
            Prefs::default()
        }
        Err(e) => {
            log::warn!("updates: {} could not be read ({e}); the defaults are used", path.display());
            Prefs::default()
        }
    }
}

/// The platform as the dialog words it (`linux-x86_64`).
pub fn platform_text() -> String {
    match lcc_updater::Target::current() {
        Some(t) => format!("{}-{}", t.os.slug(), t.arch),
        None => format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

/// Whether this build has updates at all (the platforms we do not package do not).
pub fn platform_supported() -> bool {
    lcc_updater::Target::current().is_some()
}

/// `YYYY-MM-DDTHH:MM:SSZ` now, by hand (the engine carries no date crate).
pub fn iso_now() -> String {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    iso_from_unix(secs)
}

/// [`iso_now`] for a Unix timestamp.
pub fn iso_from_unix(secs: u64) -> String {
    let days = secs / 86_400;
    let (h, m, s) = ((secs % 86_400) / 3_600, (secs % 3_600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn available_json(a: &Available) -> Value {
    json!({
        "version": a.version,
        "current": a.current,
        "title": a.title,
        "tag": a.tag,
        "published": a.published,
        "notes": a.notes,
        "file": a.file,
        "size": a.size,
        "action": a.action,
        "verified": a.sha256.is_some(),
        "kind": a.kind.text(),
    })
}

fn staged_json(s: &Staged) -> Value {
    json!({
        "version": s.version,
        "file": s.file,
        "bytes": s.bytes,
        "sha256": s.sha256,
        "path": s.path.to_string_lossy(),
        "kind": s.kind.text(),
    })
}

/// The install plan of a staged file, as the dialog words it.
pub fn plan(s: &Staged) -> lcc_updater::Plan {
    lcc_updater::plan(s.kind.to_crate(), &s.path, &s.version)
}

/// Apply a staged update; a manual install shows the file through `desktop`. The returned
/// [`Installed`] says whether the app has to quit.
pub fn install(s: &Staged, desktop: &dyn crate::desktop::Desktop) -> Result<Installed, String> {
    let mut reveal = |f: &std::path::Path| desktop.reveal(f);
    let outcome = lcc_updater::install_with(s.kind.to_crate(), &downloaded(s), &s.version, &mut reveal).map_err(|e| e.to_string())?;
    Ok(Installed { version: s.version.clone(), message: outcome.message, exit: outcome.exit })
}

fn downloaded(s: &Staged) -> lcc_updater::Downloaded {
    lcc_updater::Downloaded { path: s.path.clone(), bytes: s.bytes, sha256: s.sha256.clone(), name: s.file.clone() }
}

/// Delete a staged file the user does not want to install.
pub fn discard(s: &Staged) {
    if let Err(e) = std::fs::remove_file(&s.path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("updates: {}: {e}", s.path.display());
    }
}

#[cfg(test)]
mod tests;
