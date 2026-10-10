//! Add exported files to Apple Photos (macOS; issue #236).
//!
//! Photos is asked through its scripting interface, the way Apple documents for importing: a
//! constant AppleScript ([`SCRIPT`]) run by `/usr/bin/osascript` imports the files, into an album
//! (made when missing) if one is named, and returns the new media items' ids. Album names and file
//! paths are never written into the script: they reach it as separate arguments (`on run argv`),
//! so no name or path can change what the script does and nothing needs escaping.
//!
//! This sends Apple Events to Photos' import command; it does not synthesize keystrokes or clicks.
//! The first time, macOS asks the user whether LightCraft may control Photos (System Settings ›
//! Privacy & Security › Automation). A packaged app needs the `com.apple.security.automation.
//! apple-events` entitlement and an `NSAppleEventsUsageDescription` for that (packaging/macos).
//!
//! An import can take minutes (and waits for the user to answer that prompt), so each one is a job
//! ([`ImportJob`], kept in [`Imports`]) that a front end may run on a worker and look at later
//! (`export.photosImports`). One runs at a time, and an export that will add its files reserves
//! the slot before it writes them ([`Reservation`]): a retry while one is reserved or running is
//! refused, not queued, so files are never sent twice by accident and an export is never turned
//! away after writing its files.
//!
//! The process runner is injectable ([`Runner`]): [`crate::Session::apple_photos`] holds the real one
//! on macOS (installed by [`crate::Session::with_fs`]) and nothing elsewhere; tests use fakes and
//! never start osascript. Not in the web build.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

/// Run by osascript with `argv` = [`MARKER`], the album name (empty: none), then the files' POSIX
/// paths. Prints the imported media items' ids, one per line.
///
/// The album is looked for by its exact name among the albums at the top level of the library
/// (Photos' `album` elements are every album, folders included, in no set order): albums inside
/// folders are never used. None there: a new top-level album is made. More than one: nothing is
/// imported (error [`AMBIGUOUS_ALBUM`]).
pub const SCRIPT: &str = r#"on run argv
	if (count of argv) < 3 then error "LightCraft: no files to import" number -50
	set albumName to item 2 of argv
	set mediaFiles to {}
	repeat with i from 3 to (count of argv)
		set end of mediaFiles to (POSIX file (item i of argv)) as alias
	end repeat
	set importedItems to {}
	tell application "Photos"
		with timeout of 7200 seconds
			if albumName is not "" then
				set topLevel to {}
				repeat with candidate in (every album whose name is albumName)
					set candidateName to name of candidate
					set candidateParent to missing value
					try
						set candidateParent to parent of candidate
					on error errText number errNum
						-- no parent folder: the album is at the top level
						if {-1728, -2763} does not contain errNum then error errText number errNum
					end try
					considering case
						set sameName to (candidateName is albumName)
					end considering
					if sameName and candidateParent is missing value then set end of topLevel to contents of candidate
				end repeat
				if (count of topLevel) > 1 then error "LightCraft: several top-level albums have this name" number 23601
				if (count of topLevel) is 1 then
					set targetAlbum to item 1 of topLevel
				else
					set targetAlbum to make new album named albumName
				end if
			end if
			try
				if albumName is "" then
					set importedItems to import mediaFiles skip check duplicates false
				else
					set importedItems to import mediaFiles into targetAlbum skip check duplicates false
				end if
			on error errText number errNum
				-- -2763: Photos returned nothing (it imported nothing); anything else is a real error
				if errNum is not -2763 then error errText number errNum
			end try
		end timeout
	end tell
	try
		if importedItems is missing value then set importedItems to {}
	on error
		set importedItems to {}
	end try
	set mediaIds to {}
	tell application "Photos"
		repeat with mediaItem in importedItems
			set end of mediaIds to (id of mediaItem)
		end repeat
	end tell
	set AppleScript's text item delimiters to linefeed
	set output to mediaIds as text
	set AppleScript's text item delimiters to ""
	return output
end run"#;

/// The first script argument, always the same: osascript's option parsing stops at the first
/// argument that isn't an option, so an album name or path starting with `-` is never read as one.
pub const MARKER: &str = "lightcraft-photos-import";

/// The error number [`SCRIPT`] raises when more than one top-level album has the name asked for.
pub const AMBIGUOUS_ALBUM: i64 = 23601;

/// Where macOS keeps Photos.
pub const PHOTOS_APP: &str = "/System/Applications/Photos.app";

/// How many finished imports a session remembers (`export.photosImports`).
pub const KEPT_JOBS: usize = 32;

/// Why there is no Photos runner in this session.
pub fn unavailable() -> &'static str {
    if cfg!(target_os = "macos") {
        "Apple Photos isn't connected in this session (it is in the desktop app and lightcraft-cli)"
    } else {
        "Apple Photos is only available on macOS"
    }
}

/// What osascript gave back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// The exit code (`None`: ended by a signal).
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Why osascript gave nothing back.
#[derive(Clone, Debug, PartialEq)]
pub enum RunError {
    /// Photos isn't installed.
    NoPhotos,
    /// osascript couldn't be started.
    Spawn(String),
    /// Still running after this long: stopped.
    Timeout(Duration),
}

/// Runs osascript with these arguments, stopping it after the timeout.
pub type Runner = Arc<dyn Fn(&[String], Duration) -> Result<Output, RunError> + Send + Sync>;

/// osascript's arguments: the script, one `-e` per line, then [`MARKER`], the album name and the
/// paths, each a whole argument of its own, unchanged.
pub fn osascript_args(album: &str, paths: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(SCRIPT.lines().count() * 2 + 2 + paths.len());
    for line in SCRIPT.lines() {
        args.push("-e".to_string());
        args.push(line.to_string());
    }
    args.push(MARKER.to_string());
    args.push(album.to_string());
    args.extend(paths.iter().cloned());
    args
}

/// The media item ids the script printed (one per line; blank lines ignored).
pub fn parse_ids(stdout: &str) -> Vec<String> {
    stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// The AppleScript error number at the end of osascript's message (`… (-1743)`).
fn error_number(stderr: &str) -> Option<i64> {
    let s = stderr.trim_end();
    let open = s.rfind('(')?;
    s.get(open + 1..)?.strip_suffix(')')?.trim().parse().ok()
}

/// What to tell the user when osascript failed importing into `album`.
pub fn explain(out: &Output, album: &str) -> String {
    match error_number(&out.stderr) {
        Some(-1743 | -1744) => "LightCraft isn't allowed to control Photos. Allow it in System Settings › Privacy & Security › Automation \
             (turn on Photos under LightCraft, or under your terminal app for lightcraft-cli), then try again."
            .into(),
        Some(-600 | -10810 | -10814) => "Apple Photos couldn't be found or opened on this Mac.".into(),
        Some(-128) => "The import was cancelled in Photos.".into(),
        Some(-1712) => "Photos didn't answer in time; it may still be importing. Check Photos before trying again.".into(),
        Some(-43) => "Photos couldn't find a file to add (it was moved or deleted after the export).".into(),
        Some(AMBIGUOUS_ALBUM) => format!(
            "More than one album at the top level of Photos is named “{album}”, so nothing was added. Rename one of them in Photos, or use another album name."
        ),
        _ => {
            let detail = out.stderr.lines().map(str::trim).find(|l| !l.is_empty());
            match (detail, out.status) {
                (Some(d), _) => format!("Adding to Apple Photos failed: {d}"),
                (None, Some(code)) => format!("Adding to Apple Photos failed (osascript exited with {code})."),
                (None, None) => "Adding to Apple Photos failed (osascript was stopped).".into(),
            }
        }
    }
}

/// What to tell the user when osascript gave nothing back.
pub fn explain_run_error(e: &RunError) -> String {
    match e {
        RunError::NoPhotos => "Apple Photos couldn't be found on this Mac.".into(),
        RunError::Spawn(why) => format!("Couldn't run /usr/bin/osascript to add the photos to Apple Photos: {why}"),
        RunError::Timeout(t) => {
            format!("Photos didn't finish adding the photos within {} s; it may still be importing. Check Photos before trying again.", t.as_secs())
        }
    }
}

/// How long one import may take: ten minutes, plus five seconds a file, at most two hours. The ten
/// minutes are for a person: the first import waits for the user to answer macOS's permission
/// prompt, and Photos may ask about duplicates; a first try with one minute timed out while the
/// prompt was still up.
pub fn timeout_for(files: usize) -> Duration {
    Duration::from_secs(600u64.saturating_add(5u64.saturating_mul(files as u64)).min(7200))
}

/// What an import gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    /// The new media items in Photos.
    pub ids: Vec<String>,
    /// Files asked for.
    pub requested: usize,
    pub album: Option<String>,
}

impl Imported {
    /// When Photos added fewer files than asked: why that can be.
    pub fn warning(&self) -> Option<String> {
        (self.ids.len() < self.requested).then(|| {
            format!("Photos added {} of {} files; the others may be duplicates it skipped or files it couldn't read.", self.ids.len(), self.requested)
        })
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({"imported": self.ids.len(), "requested": self.requested, "ids": self.ids, "album": self.album});
        if let Some(w) = self.warning() {
            v["warning"] = json!(w);
        }
        v
    }
}

/// `paths` (absolute paths of existing files) and `album` checked for an import; the album name
/// trimmed. Nothing is started for a bad request.
pub fn check(paths: &[String], album: &str) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no files to add to Apple Photos".into());
    }
    if let Some(p) = paths.iter().find(|p| !std::path::Path::new(p.as_str()).is_absolute()) {
        return Err(format!("not an absolute path: {p}"));
    }
    if let Some(p) = paths.iter().find(|p| p.contains('\0')) {
        return Err(format!("a path can't contain a NUL character: {}", p.escape_debug()));
    }
    if let Some(p) = paths.iter().find(|p| !std::path::Path::new(p.as_str()).is_file()) {
        return Err(format!("no such file: {p}"));
    }
    let album = album.trim();
    if album.contains('\0') {
        return Err("an album name can't contain a NUL character".into());
    }
    Ok(album.to_string())
}

/// Files per osascript run. The paths are arguments, and macOS limits a process's arguments to
/// 1 MiB in all: 200 paths of the longest length macOS opens (1,024 bytes; [`check`] finds no
/// file at a longer one) stay below a quarter of that.
pub const BATCH: usize = 200;

/// Run osascript for a checked request (`album` trimmed; see [`check`]): [`BATCH`] files at a
/// time, all within [`timeout_for`] the whole request. The first run makes the album when it is
/// missing; the others find it.
fn execute(runner: &Runner, paths: &[String], album: &str) -> Result<Imported, String> {
    let limit = timeout_for(paths.len());
    let deadline = std::time::Instant::now() + limit;
    let mut ids = Vec::new();
    for (i, batch) in paths.chunks(BATCH).enumerate() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let out = if left.is_zero() { Err(RunError::Timeout(limit)) } else { runner(&osascript_args(album, batch), left) };
        let failed = match out {
            Ok(out) if out.status == Some(0) => {
                ids.extend(parse_ids(&out.stdout));
                continue;
            }
            Ok(out) => explain(&out, album),
            // the request's limit, not what was left of it for this batch
            Err(RunError::Timeout(_)) => explain_run_error(&RunError::Timeout(limit)),
            Err(e) => explain_run_error(&e),
        };
        if i == 0 {
            return Err(failed);
        }
        return Err(format!("{failed} (Photos had added {} of the {} files before that.)", ids.len(), paths.len()));
    }
    Ok(Imported { ids, requested: paths.len(), album: (!album.is_empty()).then(|| album.to_string()) })
}

/// Add `paths` (absolute paths of existing files) to Photos, into `album` when it isn't blank,
/// here and now.
pub fn import(runner: &Runner, paths: &[String], album: &str) -> Result<Imported, String> {
    let album = check(paths, album)?;
    execute(runner, paths, &album)
}

/// How an import ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Imported(Imported),
    /// Photos (or osascript) failed: why, for the user.
    Failed(String),
    /// Photos wasn't asked: the export that reserved the import was cancelled, wrote no files or
    /// stopped early.
    Skipped(String),
}

/// One import: reserved by an export still writing its files, running, or finished.
#[derive(Debug)]
pub struct ImportJob {
    pub id: u64,
    pub album: Option<String>,
    /// Files asked for (0 while an export is still writing them).
    requested: AtomicUsize,
    /// Reserved by an export that hasn't handed over its files yet.
    exporting: AtomicBool,
    /// Whoever started it reports the outcome itself (an export's toast or result): the app
    /// doesn't announce it again.
    quiet: AtomicBool,
    outcome: Mutex<Option<Outcome>>,
    announced: AtomicBool,
    /// When Photos was asked (the import started): `None` while an export still writes its files.
    started: Mutex<Option<std::time::Instant>>,
}

impl ImportJob {
    /// When Photos was asked; `None` while the export that reserved the job still writes.
    pub fn started(&self) -> Option<std::time::Instant> {
        *self.started.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn finished(&self) -> bool {
        self.outcome.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Record the outcome, unless there already is one.
    fn finish(&self, o: Outcome) {
        let mut g = self.outcome.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_none() {
            *g = Some(o);
        }
        self.exporting.store(false, Ordering::Relaxed);
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn requested(&self) -> usize {
        self.requested.load(Ordering::Relaxed)
    }

    pub fn quiet(&self) -> bool {
        self.quiet.load(Ordering::Relaxed)
    }

    /// True once: when the job has finished and nobody has announced it yet (never for a quiet one).
    pub fn take_announcement(&self) -> bool {
        !self.quiet() && self.finished() && !self.announced.swap(true, Ordering::Relaxed)
    }

    /// Why another import can't start now.
    fn busy(&self) -> String {
        if self.exporting.load(Ordering::Relaxed) {
            format!(
                "An export is about to add its files to Apple Photos (import {}). Wait for it to finish (export.photosImports), then try again.",
                self.id
            )
        } else {
            format!(
                "Apple Photos is still adding {} file(s) from an earlier request (import {}). Wait for it to finish (export.photosImports), then try again.",
                self.requested(),
                self.id
            )
        }
    }

    /// `{job, running, requested, album}` (`exporting: true` while an export is still writing its
    /// files), and once finished `{imported, ids, warning?}`, `{error}` or `{skipped}`.
    pub fn json(&self) -> Value {
        let mut v = json!({"job": self.id, "running": true, "requested": self.requested(), "album": self.album});
        if self.exporting.load(Ordering::Relaxed) {
            v["exporting"] = json!(true);
        }
        match self.outcome() {
            None => {}
            Some(Outcome::Imported(i)) => {
                if let (Some(o), Some(done)) = (v.as_object_mut(), i.to_json().as_object()) {
                    o.extend(done.clone());
                }
                v["running"] = json!(false);
            }
            Some(Outcome::Failed(e)) => {
                v["running"] = json!(false);
                v["error"] = json!(e);
            }
            Some(Outcome::Skipped(why)) => {
                v["running"] = json!(false);
                v["skipped"] = json!(why);
            }
        }
        v
    }
}

/// A session's imports (the last [`KEPT_JOBS`], oldest first), shared with the threads that run
/// them.
#[derive(Clone, Debug, Default)]
pub struct Imports(Arc<Mutex<(u64, Vec<Arc<ImportJob>>)>>);

impl Imports {
    fn jobs(&self) -> std::sync::MutexGuard<'_, (u64, Vec<Arc<ImportJob>>)> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn all(&self) -> Vec<Arc<ImportJob>> {
        self.jobs().1.clone()
    }

    pub fn get(&self, id: u64) -> Option<Arc<ImportJob>> {
        self.jobs().1.iter().find(|j| j.id == id).cloned()
    }

    pub fn latest(&self) -> Option<Arc<ImportJob>> {
        self.jobs().1.last().cloned()
    }

    /// The import that is reserved or running, if any (there is at most one).
    pub fn running(&self) -> Option<Arc<ImportJob>> {
        self.jobs().1.iter().find(|j| !j.finished()).cloned()
    }

    /// A new job, unless one is reserved or running: two would race in Photos, and a retry of one
    /// that is still going would add its files twice.
    fn begin(&self, requested: usize, album: &str, exporting: bool) -> Result<Arc<ImportJob>, String> {
        let mut g = self.jobs();
        if let Some(r) = g.1.iter().find(|j| !j.finished()) {
            return Err(r.busy());
        }
        g.0 = g.0.saturating_add(1);
        let job = Arc::new(ImportJob {
            id: g.0,
            album: (!album.is_empty()).then(|| album.to_string()),
            requested: AtomicUsize::new(requested),
            exporting: AtomicBool::new(exporting),
            quiet: AtomicBool::new(true),
            outcome: Mutex::new(None),
            announced: AtomicBool::new(false),
            started: Mutex::new(None),
        });
        g.1.push(job.clone());
        let excess = g.1.len().saturating_sub(KEPT_JOBS);
        g.1.drain(..excess);
        Ok(job)
    }

    /// Reserve the import slot for an export that will add the files it writes: from now until
    /// the export hands them over ([`Reservation::run`] / [`Reservation::start`]) or ends (the
    /// reservation dropped), other imports are refused. An error while one is reserved or running.
    pub fn reserve(&self, runner: Runner, album: &str) -> Result<Reservation, String> {
        let album = album.trim();
        if album.contains('\0') {
            return Err("an album name can't contain a NUL character".into());
        }
        let job = self.begin(0, album, true)?;
        Ok(Reservation { runner, album: album.to_string(), job: Some(job) })
    }

    /// [`Imports::wait_idle_within`] with the [`WaitLimits::default`].
    pub fn wait_idle(&self) -> bool {
        self.wait_idle_within(WaitLimits::default())
    }

    /// Wait until no import is reserved or running: a process about to end (lightcraft-cli, an
    /// MCP server at EOF, a snapshot script) calls this so it never abandons osascript or an
    /// import's result. The deadline follows the job as it goes: while an export still writes
    /// the files it reserved the import for, `limits.writing` from the start of the wait; once
    /// Photos was asked, the import's own time limit (`limits.import`, the one osascript runs
    /// under) plus `limits.margin` from when it started; never longer than `limits.cap` in all.
    /// True when nothing is left reserved or running.
    pub fn wait_idle_within(&self, limits: WaitLimits) -> bool {
        let t0 = std::time::Instant::now();
        loop {
            let Some(job) = self.running() else { return true };
            let waited = t0.elapsed();
            if waited >= limits.cap {
                return false;
            }
            let expired = match job.started() {
                None => waited >= limits.writing,
                Some(at) => at.elapsed() >= (limits.import)(job.requested()).saturating_add(limits.margin),
            };
            if expired {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// How long [`Imports::wait_idle_within`] waits.
#[derive(Clone, Copy, Debug)]
pub struct WaitLimits {
    /// An export still writing the files it will add: how much longer it may write.
    pub writing: Duration,
    /// A running import's time limit for this many files.
    pub import: fn(usize) -> Duration,
    /// Added to that limit.
    pub margin: Duration,
    /// The longest the whole wait may take.
    pub cap: Duration,
}

impl Default for WaitLimits {
    /// An hour of writing (what `lightcraft-cli snapshot` gives a background export), then the
    /// import's osascript limit ([`timeout_for`], at most two hours) and half a minute; three hours
    /// and a minute in all.
    fn default() -> Self {
        Self { writing: Duration::from_secs(3600), import: timeout_for, margin: Duration::from_secs(30), cap: Duration::from_secs(3600 + 7200 + 60) }
    }
}

impl crate::Session {
    /// Before a front end without a window ends (lightcraft-cli, the MCP server, a headless
    /// snapshot), on every exit path: wait for an Apple Photos import still reserved or running
    /// (`export.addToPhotos {wait: false}`, a background export still writing), bounded
    /// ([`Imports::wait_idle`], [`WaitLimits::default`]), so the process never abandons osascript
    /// or the import's result. True when it had to wait.
    pub fn wait_for_photos_imports(&self) -> bool {
        let Some(job) = self.apple_photos_imports.running() else { return false };
        log::warn!("Apple Photos: waiting for import {} to finish before exiting", job.id);
        if !self.apple_photos_imports.wait_idle() {
            log::warn!("Apple Photos: gave up waiting for import {}; Photos may still be importing", job.id);
        }
        true
    }
}

/// Ask Photos to import `paths` (checked; `album` trimmed) for `job`: here, or on a worker thread.
fn launch(runner: &Runner, job: &Arc<ImportJob>, paths: Vec<String>, album: String, background: bool) {
    job.requested.store(paths.len(), Ordering::Relaxed);
    *job.started.lock().unwrap_or_else(PoisonError::into_inner) = Some(std::time::Instant::now());
    job.exporting.store(false, Ordering::Relaxed);
    if !background {
        run_to_completion(runner, job, &paths, &album);
        return;
    }
    let (r, j) = (runner.clone(), job.clone());
    let started = std::thread::Builder::new().name("apple-photos".into()).spawn(move || run_to_completion(&r, &j, &paths, &album));
    if let Err(e) = started {
        job.finish(Outcome::Failed(format!("Couldn't start adding the photos to Apple Photos: {e}")));
    }
}

/// The message for an import whose runner panicked.
const STOPPED: &str = "Adding to Apple Photos stopped unexpectedly.";

/// Finishes its job as failed when dropped without an outcome: however the work ends (a panic
/// unwinding through it included), the job finishes and the import slot is freed.
struct Completion<'a>(&'a ImportJob);

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        self.0.finish(Outcome::Failed(STOPPED.into()));
    }
}

/// Run the import for `job` and record its outcome, on this thread. A panic in the runner is
/// that import's error, never a job that stays "running" (holding the slot) or a panic for the
/// caller.
fn run_to_completion(runner: &Runner, job: &ImportJob, paths: &[String], album: &str) {
    let _done = Completion(job);
    let out = crate::guard::catch("the Apple Photos import", || execute(runner, paths, album)).unwrap_or_else(|_| Err(STOPPED.into()));
    job.finish(out.map_or_else(Outcome::Failed, Outcome::Imported));
}

/// Add `paths` to Photos as a job of `imports`: on a worker thread (`background`; the job is
/// returned running, and announced by the app when it ends) or here (returned finished). An
/// error, and no job, for a bad request or while another import is reserved or running.
pub fn import_job(runner: &Runner, imports: &Imports, paths: Vec<String>, album: &str, background: bool) -> Result<Arc<ImportJob>, String> {
    let album = check(&paths, album)?;
    let job = imports.begin(paths.len(), &album, false)?;
    job.quiet.store(!background, Ordering::Relaxed);
    launch(runner, &job, paths, album, background);
    Ok(job)
}

/// The import slot an export holds while it writes its files ([`Imports::reserve`]). Handed the
/// files with [`Reservation::run`] or [`Reservation::start`]; dropped unused (the export failed,
/// was refused or never started) it frees the slot, the job ending as skipped.
pub struct Reservation {
    runner: Runner,
    album: String,
    job: Option<Arc<ImportJob>>,
}

impl Reservation {
    pub fn job(&self) -> Option<&Arc<ImportJob>> {
        self.job.as_ref()
    }

    /// The absolute paths of the files an export wrote (`files`, as `export_batch` reports them:
    /// entries with a `path`; skipped and failed ones are left out), or why there is nothing to do.
    fn take(&mut self, files: &[Value], cancelled: bool) -> Result<(Arc<ImportJob>, Vec<String>), Value> {
        let Some(job) = self.job.take() else { return Err(json!({"error": "this import was already handed its files"})) };
        let paths: Vec<String> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(absolute).collect();
        let skipped = if cancelled {
            Some("the export was cancelled")
        } else if paths.is_empty() {
            Some("the export wrote no files")
        } else {
            None
        };
        if let Some(why) = skipped {
            job.finish(Outcome::Skipped(why.into()));
            return Err(job.json());
        }
        if let Err(e) = check(&paths, &self.album) {
            job.finish(Outcome::Failed(e));
            return Err(job.json());
        }
        Ok((job, paths))
    }

    /// Add the files an export wrote and wait for Photos (a worker thread, or a caller without a
    /// window). The finished job's [`ImportJob::json`]; never an error for the export.
    pub fn run(mut self, files: &[Value], cancelled: bool) -> Value {
        match self.take(files, cancelled) {
            Ok((job, paths)) => {
                launch(&self.runner, &job, paths, self.album.clone(), false);
                job.json()
            }
            Err(done) => done,
        }
    }

    /// Like [`Reservation::run`] without waiting: Photos works on a worker thread; the running
    /// job is returned, and the app announces its outcome.
    pub fn start(mut self, files: &[Value]) -> Value {
        match self.take(files, false) {
            Ok((job, paths)) => {
                job.quiet.store(false, Ordering::Relaxed);
                launch(&self.runner, &job, paths, self.album.clone(), true);
                job.json()
            }
            Err(done) => done,
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.finish(Outcome::Skipped("the export ended before writing its files".into()));
        }
    }
}

/// `path` made absolute against the working directory (export folders may be given relative).
fn absolute(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    std::env::current_dir().map(|d| d.join(p).to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string())
}

/// The real runner: `/usr/bin/osascript`, stopped after the timeout.
#[cfg(target_os = "macos")]
pub fn osascript() -> Runner {
    Arc::new(|args: &[String], timeout: Duration| {
        use std::io::Read;
        use std::process::{Command, Stdio};
        if !std::path::Path::new(PHOTOS_APP).exists() {
            return Err(RunError::NoPhotos);
        }
        let mut child = Command::new("/usr/bin/osascript")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RunError::Spawn(e.to_string()))?;
        // read both pipes while it runs: a long list of ids must not fill a pipe and stall it
        fn drain(r: Option<impl Read + Send + 'static>) -> std::io::Result<std::thread::JoinHandle<String>> {
            std::thread::Builder::new().name("osascript-output".into()).spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut buf);
                }
                String::from_utf8_lossy(&buf).into_owned()
            })
        }
        let pipes = drain(child.stdout.take()).and_then(|out| Ok((out, drain(child.stderr.take())?)));
        let (stdout, stderr) = match pipes {
            Ok(p) => p,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Spawn(e.to_string()));
            }
        };
        let deadline = std::time::Instant::now().checked_add(timeout);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if deadline.is_some_and(|d| std::time::Instant::now() >= d) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Timeout(timeout));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Spawn(e.to_string()));
                }
            }
        };
        Ok(Output { status: status.code(), stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that records the arguments it got and answers `answer`.
    fn fake(answer: Result<Output, RunError>) -> (Runner, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let c = calls.clone();
        let r: Runner = Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            answer.clone()
        });
        (r, calls)
    }

    fn ok(stdout: &str) -> Result<Output, RunError> {
        Ok(Output { status: Some(0), stdout: stdout.into(), stderr: String::new() })
    }

    fn failed(stderr: &str) -> Output {
        Output { status: Some(1), stdout: String::new(), stderr: stderr.into() }
    }

    /// The script part of an argument list: everything before the marker.
    fn script_of(args: &[String]) -> &[String] {
        let at = args.iter().position(|a| a == MARKER).unwrap();
        &args[..at]
    }

    #[test]
    fn names_and_paths_are_arguments_never_script() {
        let hostile = [
            "",
            "Holiday \"2026\"",
            "back\\slash\\",
            "line\nbreak\r\nand\ttab",
            "end tell\ndo shell script \"rm -rf ~\"",
            "-e",
            "--help",
            "日本の写真 📷 Ünïcödé",
            "'single' & \"double\" » «",
        ];
        let base = osascript_args("", &["/tmp/a.jpg".into()]);
        for album in hostile {
            let paths = vec!["/tmp/with \"quotes\".jpg".to_string(), "/tmp/new\nline/é\\x.jpg".to_string(), format!("/tmp/-{album}.jpg")];
            let args = osascript_args(album, &paths);
            // the script is the same whatever the input…
            assert_eq!(script_of(&args), script_of(&base));
            assert!(script_of(&args).chunks(2).all(|c| c[0] == "-e" && SCRIPT.lines().any(|l| l == c[1])));
            // …and the input follows the marker as whole arguments, unchanged
            let at = args.iter().position(|a| a == MARKER).unwrap();
            assert_eq!(&args[at + 1], album);
            assert_eq!(&args[at + 2..], &paths[..]);
        }
        // the script reads them from argv only
        assert!(SCRIPT.starts_with("on run argv") && SCRIPT.contains("item 2 of argv") && SCRIPT.contains("item i of argv"));
        assert!(!SCRIPT.contains("do shell script"));
        // it imports into the album (made when missing) without skipping Photos' duplicate check
        assert!(SCRIPT.contains("make new album named albumName"));
        assert!(SCRIPT.contains("import mediaFiles into targetAlbum skip check duplicates false"));
    }

    /// Photos' `album` elements are every album, those in folders too, in no set order: the script
    /// keeps the top-level ones of exactly that name and refuses when there are several.
    #[test]
    fn the_album_is_a_single_top_level_one() {
        let lookup = SCRIPT.find("every album whose name is albumName").unwrap();
        let choose = SCRIPT.find("set targetAlbum to item 1 of topLevel").unwrap();
        let make = SCRIPT.find("make new album named albumName").unwrap();
        let import = SCRIPT.find("import mediaFiles into targetAlbum").unwrap();
        assert!(lookup < choose && choose < make && make < import);
        let filter = &SCRIPT[lookup..choose];
        assert!(filter.contains("parent of candidate") && filter.contains("candidateParent is missing value"), "{filter}");
        assert!(filter.contains("considering case"), "exact names: {filter}");
        assert!(filter.contains(&format!(
            "if (count of topLevel) > 1 then error \"LightCraft: several top-level albums have this name\" number {AMBIGUOUS_ALBUM}"
        )));
        let m = explain(&failed(&format!("execution error: LightCraft: several top-level albums have this name ({AMBIGUOUS_ALBUM})")), "Trip");
        assert!(m.contains("More than one album at the top level of Photos is named “Trip”") && m.contains("nothing was added"), "{m}");
    }

    #[test]
    fn ids_are_one_per_line() {
        assert_eq!(parse_ids("A1B2/L0/001\nC3D4/L0/001\n"), vec!["A1B2/L0/001", "C3D4/L0/001"]);
        assert_eq!(parse_ids(" X/L0/001 \r\n\r\n Y/L0/001"), vec!["X/L0/001", "Y/L0/001"]);
        assert!(parse_ids("").is_empty() && parse_ids("\n \n").is_empty());
    }

    #[test]
    fn errors_say_what_to_do() {
        let says = |stderr: &str, part: &str| {
            let m = explain(&failed(stderr), "Trip");
            assert!(m.contains(part), "{stderr:?}: {m}");
        };
        says("146:180: execution error: Not authorized to send Apple events to Photos. (-1743)", "System Settings › Privacy & Security › Automation");
        says("execution error: Photos got an error: … (-1744)", "Automation");
        says("execution error: Can’t get application \"Photos\". (-10814)", "couldn't be found");
        says("execution error: Photos got an error: Application isn’t running. (-600)", "couldn't be found or opened");
        says("execution error: User canceled. (-128)", "cancelled");
        says("execution error: Photos got an error: AppleEvent timed out. (-1712)", "didn't answer in time");
        says("execution error: File not found (-43)", "couldn't find a file");
        says("execution error: Photos got an error: Something odd happened. (-2700)", "Something odd happened");
        assert!(explain(&Output { status: Some(3), ..Default::default() }, "").contains("exited with 3"));
        assert!(explain(&Output { status: None, ..Default::default() }, "").contains("stopped"));
        assert!(explain_run_error(&RunError::NoPhotos).contains("couldn't be found"));
        assert!(explain_run_error(&RunError::Spawn("denied".into())).contains("denied"));
        assert!(explain_run_error(&RunError::Timeout(Duration::from_secs(65))).contains("65 s"));
        // a stray parenthesis is not an error number
        assert!(explain(&failed("execution error: (not a number)"), "").contains("not a number"));
    }

    fn files(tag: &str, n: usize) -> (std::path::PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("lc-photos-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = (0..n)
            .map(|i| {
                let p = dir.join(format!("photo {i} é.jpg"));
                std::fs::write(&p, b"jpeg").unwrap();
                p.to_string_lossy().to_string()
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn an_import_passes_the_files_and_returns_the_ids() {
        let (dir, paths) = files("import", 2);
        let (runner, calls) = fake(ok("ID-1/L0/001\nID-2/L0/001\n"));
        let got = import(&runner, &paths, "  Trip “2026”  ").unwrap();
        assert_eq!(got, Imported { ids: vec!["ID-1/L0/001".into(), "ID-2/L0/001".into()], requested: 2, album: Some("Trip “2026”".into()) });
        assert_eq!(got.warning(), None);
        let args = calls.lock().unwrap()[0].clone();
        let at = args.iter().position(|a| a == MARKER).unwrap();
        assert_eq!(&args[at + 1..], &[vec!["Trip “2026”".to_string()], paths.clone()].concat()[..]);
        // fewer back than asked: a partial import, said so
        let (runner, _) = fake(ok("ID-1/L0/001\n"));
        let got = import(&runner, &paths, "").unwrap();
        assert_eq!((got.ids.len(), got.album.clone()), (1, None));
        assert!(got.warning().unwrap().contains("1 of 2"));
        assert_eq!(got.to_json()["warning"], json!(got.warning().unwrap()));
        // a failed run is the mapped message
        let (runner, _) = fake(Ok(failed("execution error: Not authorized to send Apple events to Photos. (-1743)")));
        assert!(import(&runner, &paths, "x").unwrap_err().contains("Automation"));
        // a timeout names the request's limit
        let (runner, _) = fake(Err(RunError::Timeout(Duration::from_secs(70))));
        assert!(import(&runner, &paths, "x").unwrap_err().contains(&format!("{} s", timeout_for(2).as_secs())));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Codex review of #643: every path was an argument of one osascript run, so a big export
    /// (10,000 files) went over macOS's 1 MiB argument limit and Photos got nothing.
    #[test]
    fn big_imports_run_in_batches_within_the_argument_limit() {
        let long = format!("/{}", "d/".repeat(500)); // about 1,000 bytes, near the longest path macOS opens
        let paths: Vec<String> = (0..10_000).map(|i| format!("{long}{i:05}.jpg")).collect();
        let runs = Arc::new(Mutex::new(Vec::<(usize, usize, Duration)>::new()));
        let r = runs.clone();
        let runner: Runner = Arc::new(move |args: &[String], limit| {
            let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            r.lock().unwrap().push((files, args.iter().map(|a| a.len() + 1).sum(), limit));
            Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
        });
        let got = execute(&runner, &paths, "Big").unwrap();
        assert_eq!((got.ids.len(), got.requested, got.warning()), (10_000, 10_000, None));
        let runs = runs.lock().unwrap().clone();
        assert_eq!(runs.len(), 10_000 / BATCH);
        assert!(runs.iter().all(|&(files, _, _)| files == BATCH));
        let biggest = runs.iter().map(|&(_, bytes, _)| bytes).max().unwrap();
        assert!(biggest < 1 << 18, "{biggest} bytes of arguments in one run");
        // every run gets what is left of the request's limit, never more
        assert!(runs.iter().all(|&(_, _, limit)| limit <= timeout_for(10_000)));

        // a later batch failing: the error says how far Photos got
        let calls = Arc::new(Mutex::new(0));
        let c = calls.clone();
        let runner: Runner = Arc::new(move |args: &[String], _| {
            let mut n = c.lock().unwrap();
            *n += 1;
            let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            if *n == 2 {
                return Ok(failed("execution error: Photos got an error: User canceled. (-128)"));
            }
            Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
        });
        let e = execute(&runner, &paths[..450], "Big").unwrap_err();
        assert!(e.contains("cancelled") && e.contains("200 of the 450 files"), "{e}");
        assert_eq!(*calls.lock().unwrap(), 2, "nothing more is sent after a failed batch");

        // the batches share the request's one deadline: time a batch takes comes off the next one's
        let limits = Arc::new(Mutex::new(Vec::<Duration>::new()));
        let l = limits.clone();
        let runner: Runner = Arc::new(move |args: &[String], limit| {
            l.lock().unwrap().push(limit);
            std::thread::sleep(Duration::from_millis(300));
            let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
        });
        execute(&runner, &paths[..450], "Big").unwrap();
        let limits = limits.lock().unwrap().clone();
        assert_eq!(limits.len(), 3);
        let whole = timeout_for(450);
        assert!(limits[0] <= whole && limits[0] > whole - Duration::from_millis(250), "{limits:?}");
        assert!(limits[1] <= whole - Duration::from_millis(300) && limits[2] <= whole - Duration::from_millis(600), "{limits:?}");
    }

    #[test]
    fn bad_requests_never_reach_osascript() {
        let (dir, paths) = files("bad", 1);
        let (runner, calls) = fake(ok(""));
        assert!(import(&runner, &[], "").unwrap_err().contains("no files"));
        assert!(import(&runner, &["relative/a.jpg".into()], "").unwrap_err().contains("absolute"));
        assert!(import(&runner, &[format!("{}/missing.jpg", dir.display())], "").unwrap_err().contains("no such file"));
        assert!(import(&runner, &[format!("{}/a\0b.jpg", dir.display())], "").unwrap_err().contains("NUL"));
        assert!(import(&runner, &paths, "a\0b").unwrap_err().contains("NUL"));
        assert!(calls.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn after_an_export_the_written_files_are_added() {
        let (dir, paths) = files("after", 2);
        let exported = vec![
            json!({"path": paths[0], "width": 10}),
            json!({"skipped": "/elsewhere/x.jpg"}),
            json!({"photo": 3, "error": "x"}),
            json!({"path": paths[1]}),
        ];
        let (runner, calls) = fake(ok("A/L0/001\nB/L0/001"));
        let imports = Imports::default();
        let out = imports.reserve(runner.clone(), " Album ").unwrap().run(&exported, false);
        assert_eq!(out["imported"], 2);
        assert_eq!(out["album"], "Album");
        assert_eq!((out["job"].as_u64(), out["running"].as_bool()), (Some(1), Some(false)));
        let args = calls.lock().unwrap()[0].clone();
        assert_eq!(&args[args.len() - 2..], &paths[..], "only the files written, in order");
        // nothing written, or cancelled: Photos isn't asked, and the slot is free again
        let out = imports.reserve(runner.clone(), "").unwrap().run(&[json!({"skipped": "x"})], false);
        assert!(out["skipped"].as_str().unwrap().contains("wrote no files"), "{out}");
        let out = imports.reserve(runner.clone(), "").unwrap().run(&exported, true);
        assert!(out["skipped"].as_str().unwrap().contains("cancelled"), "{out}");
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(imports.running().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An export that will add its files reserves the slot before writing them: a competing
    /// import is refused, naming the export's job, and the export is never turned away after
    /// writing. Every way the export can end frees the slot.
    #[test]
    fn an_export_reserves_the_import_before_writing() {
        let (dir, paths) = files("reserve", 1);
        let (runner, calls) = fake(ok("A\n"));
        let imports = Imports::default();
        let reserved = imports.reserve(runner.clone(), "Trip").unwrap();
        assert_eq!(reserved.job().map(|j| j.json()), Some(json!({"job": 1, "running": true, "exporting": true, "requested": 0, "album": "Trip"})));
        // while the export writes: other imports and exports are refused, naming it
        let e = import_job(&runner, &imports, paths.clone(), "", false).unwrap_err();
        assert!(e.contains("An export is about to add its files to Apple Photos (import 1)"), "{e}");
        assert!(imports.reserve(runner.clone(), "").err().unwrap().contains("(import 1)"));
        assert!(calls.lock().unwrap().is_empty());
        // the export hands over its files: they are imported under its job
        let out = reserved.run(&[json!({"path": paths[0]})], false);
        assert_eq!((out["job"].as_u64(), out["imported"].as_u64(), out.get("exporting")), (Some(1), Some(1), None), "{out}");
        // an export that fails or never runs (its reservation dropped) frees the slot
        let dropped = imports.reserve(runner.clone(), "").unwrap();
        let id = dropped.job().unwrap().id;
        drop(dropped);
        let j = imports.get(id).unwrap();
        assert!(j.finished() && !j.take_announcement(), "skipped quietly");
        assert!(j.json()["skipped"].as_str().unwrap().contains("ended before writing"));
        // a file the export reported but that is gone: Photos isn't asked, the slot is freed
        let out = imports.reserve(runner.clone(), "").unwrap().run(&[json!({"path": format!("{}/gone.jpg", dir.display())})], false);
        assert!(out["error"].as_str().unwrap().contains("no such file"), "{out}");
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(imports.running().is_none());
        assert_eq!(import_job(&runner, &imports, paths, "", false).unwrap().id, 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Review of #236: a runner that panics, here or on a worker, ends its import as failed and
    /// frees the slot; it never stays "running", refusing every later import.
    #[test]
    fn a_panicking_runner_frees_the_slot() {
        let (dir, paths) = files("panic", 1);
        let panicky: Runner = Arc::new(|_: &[String], _| panic!("runner exploded"));
        let (good, _) = fake(ok("A\n"));
        let imports = Imports::default();
        let stopped = |j: &ImportJob| j.json()["error"].as_str().is_some_and(|e| e.contains("stopped unexpectedly"));
        // waiting here
        let job = import_job(&panicky, &imports, paths.clone(), "", false).unwrap();
        assert!(job.finished() && stopped(&job), "{}", job.json());
        assert!(imports.running().is_none());
        // on a worker
        let job = import_job(&panicky, &imports, paths.clone(), "", true).unwrap();
        let t0 = std::time::Instant::now();
        while !job.finished() {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stopped(&job), "{}", job.json());
        // an export's reservation, waiting and not
        let out = imports.reserve(panicky.clone(), "").unwrap().run(&[json!({"path": paths[0]})], false);
        assert!(out["error"].as_str().unwrap().contains("stopped unexpectedly"), "{out}");
        assert!(imports.running().is_none());
        let started = imports.reserve(panicky, "").unwrap().start(&[json!({"path": paths[0]})]);
        let job = imports.get(started["job"].as_u64().unwrap()).unwrap();
        while !job.finished() {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stopped(&job));
        // the slot is free: the next import runs
        assert_eq!(import_job(&good, &imports, paths, "", false).unwrap().json()["imported"], 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A process about to end waits for an import still running, so it never abandons osascript.
    #[test]
    fn waiting_for_idle_outlasts_a_running_import() {
        let (dir, paths) = files("idle", 1);
        let (runner, go) = delayed();
        let imports = Imports::default();
        let job = import_job(&runner, &imports, paths, "", true).unwrap();
        assert!(!job.finished());
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            go.send(()).unwrap();
        });
        imports.wait_idle();
        assert!(job.finished() && imports.running().is_none());
        release.join().unwrap();
        // nothing running: returns at once
        imports.wait_idle();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Review of #236: shutdown while an export is still writing the files it reserved the import
    /// for. The deadline follows the job: the write allowance while the export writes, then the
    /// import's own time limit from when Photos was asked, not one deadline fixed at the start
    /// (which gave up with the import still inside its limit). Scaled down: 300 ms of writing,
    /// imports limited to 1.5 s (+ 0.1 s).
    #[test]
    fn shutdown_follows_an_export_into_its_import() {
        let (dir, paths) = files("shutdown", 2);
        let limits = WaitLimits {
            writing: Duration::from_millis(300),
            import: |_| Duration::from_millis(1500),
            margin: Duration::from_millis(100),
            cap: Duration::from_secs(20),
        };
        let taking = |ms: u64| -> Runner {
            Arc::new(move |args: &[String], _| {
                std::thread::sleep(Duration::from_millis(ms));
                let n = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
                Ok(Output { status: Some(0), stdout: "ID\n".repeat(n), stderr: String::new() })
            })
        };
        let imports = Imports::default();
        // the export is still writing when the wait begins; Photos then takes 1 s, past the write
        // allowance but inside the import's own limit
        let reserved = imports.reserve(taking(1000), "").unwrap();
        let written: Vec<Value> = paths.iter().map(|p| json!({"path": p})).collect();
        let exporter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            reserved.run(&written, false)
        });
        let t0 = std::time::Instant::now();
        assert!(imports.wait_idle_within(limits), "waited for the import, not just the write allowance");
        assert!(t0.elapsed() >= Duration::from_millis(1000), "{:?}", t0.elapsed());
        assert_eq!(exporter.join().unwrap()["imported"], 2);
        // an import past its own limit is given up on at its deadline (not the cap)
        let job = import_job(&taking(3000), &imports, paths.clone(), "", true).unwrap();
        let t0 = std::time::Instant::now();
        assert!(!imports.wait_idle_within(limits));
        let waited = t0.elapsed();
        assert!(waited >= Duration::from_millis(1500) && waited < Duration::from_millis(2900), "{waited:?}");
        while !job.finished() {
            std::thread::sleep(Duration::from_millis(20));
        }
        // an export that never hands its files over: the write allowance, then it's left
        let held = imports.reserve(taking(0), "").unwrap();
        let t0 = std::time::Instant::now();
        assert!(!imports.wait_idle_within(limits));
        let waited = t0.elapsed();
        assert!(waited >= Duration::from_millis(300) && waited < Duration::from_millis(1500), "{waited:?}");
        drop(held);
        assert!(imports.wait_idle_within(limits));
        // and the cap holds over everything
        let job = import_job(&taking(1000), &imports, paths, "", true).unwrap();
        let t0 = std::time::Instant::now();
        assert!(!imports.wait_idle_within(WaitLimits { cap: Duration::from_millis(200), ..limits }));
        assert!(t0.elapsed() < Duration::from_millis(900));
        while !job.finished() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A runner that waits for `go` (at most 20 s, so a regression fails instead of hanging), then
    /// answers one id per file.
    fn delayed() -> (Runner, std::sync::mpsc::Sender<()>) {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let r: Runner = Arc::new(move |args: &[String], _| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
            let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
        });
        (r, go)
    }

    /// Imports run as jobs: in the background the caller gets the job at once and its outcome
    /// later; while one runs a second is refused (a retry must not add the files twice).
    #[test]
    fn a_slow_import_runs_as_a_job_and_refuses_a_second_one() {
        let (dir, paths) = files("job", 2);
        let (runner, go) = delayed();
        let imports = Imports::default();
        let t0 = std::time::Instant::now();
        let job = import_job(&runner, &imports, paths.clone(), " Trip ", true).unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5), "the caller doesn't wait for Photos");
        assert_eq!(job.json(), json!({"job": 1, "running": true, "requested": 2, "album": "Trip"}));
        assert_eq!(imports.running().map(|j| j.id), Some(1));
        let e = import_job(&runner, &imports, paths.clone(), "", true).unwrap_err();
        assert!(e.contains("still adding 2 file(s) from an earlier request (import 1)"), "{e}");
        let e = imports.reserve(runner.clone(), "").err().unwrap();
        assert!(e.contains("still adding"), "{e}");
        go.send(()).unwrap();
        while !job.finished() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the job finishes");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(job.take_announcement() && !job.take_announcement(), "announced once");
        let done = imports.get(1).unwrap().json();
        assert_eq!((done["running"].as_bool(), done["imported"].as_u64(), done["album"].as_str()), (Some(false), Some(2), Some("Trip")), "{done}");
        // finished: the next one may start; a failed one says why
        let (fails, _) = fake(Err(RunError::Spawn("denied".into())));
        let job = import_job(&fails, &imports, paths, "", false).unwrap();
        assert_eq!(job.id, 2);
        assert!(job.json()["error"].as_str().unwrap().contains("denied"));
        assert!(imports.running().is_none() && imports.latest().is_some_and(|j| j.id == 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_remembers_its_last_imports() {
        let (dir, paths) = files("kept", 1);
        let (runner, _) = fake(ok("A\n"));
        let imports = Imports::default();
        for _ in 0..KEPT_JOBS + 3 {
            import_job(&runner, &imports, paths.clone(), "", false).unwrap();
        }
        let all = imports.all();
        assert_eq!(all.len(), KEPT_JOBS);
        assert_eq!((all[0].id, all[KEPT_JOBS - 1].id), (4, KEPT_JOBS as u64 + 3));
        assert!(imports.get(1).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn timeouts_grow_with_the_batch_and_stay_bounded() {
        assert_eq!(timeout_for(1), Duration::from_secs(605));
        assert_eq!(timeout_for(100), Duration::from_secs(1100));
        assert_eq!(timeout_for(usize::MAX), Duration::from_secs(7200));
    }
}
