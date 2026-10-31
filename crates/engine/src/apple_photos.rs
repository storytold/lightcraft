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

use crate::activity::{Activity, Cancel, TaskGuard};

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
    /// Photos wasn't asked: the export that reserved the import was cancelled (`cancelled`), wrote
    /// no files or stopped early. `why` is for people; code decides on `cancelled`.
    Skipped {
        why: String,
        cancelled: bool,
    },
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
            Some(Outcome::Skipped { why, cancelled }) => {
                v["running"] = json!(false);
                v["skipped"] = json!(why);
                if cancelled {
                    v["cancelled"] = json!(true);
                }
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
        Ok(Reservation { runner, album: album.to_string(), job: Some(job), activity: None })
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

/// The activity stack's kind for an Apple Photos import of its own (`activity.list`).
pub const ACTIVITY_KIND: &str = "applePhotos";
/// ...and its label (translated by the frontend).
pub const ACTIVITY_LABEL: &str = "Adding to Apple Photos";

/// A row in the activity stack for an import that runs on its own (not as the last step of an
/// export, whose row covers it): no ✕ (Photos can't be stopped once asked), no count (Photos
/// reports none until it is done), the album as its detail.
fn row(activity: Option<&Activity>, album: &str) -> Option<TaskGuard> {
    let row = activity?.start(ACTIVITY_KIND, ACTIVITY_LABEL, Cancel::No);
    row.progress(0, 0);
    if !album.is_empty() {
        row.detail(album);
    }
    Some(row)
}

/// Ask Photos to import `paths` (checked; `album` trimmed) for `job`: here, or on a worker thread,
/// holding `row` (the activity stack's) until Photos is done.
fn launch(runner: &Runner, job: &Arc<ImportJob>, paths: Vec<String>, album: String, background: bool, row: Option<TaskGuard>) {
    job.requested.store(paths.len(), Ordering::Relaxed);
    *job.started.lock().unwrap_or_else(PoisonError::into_inner) = Some(std::time::Instant::now());
    job.exporting.store(false, Ordering::Relaxed);
    if !background {
        let _row = row;
        run_to_completion(runner, job, &paths, &album);
        return;
    }
    let (r, j) = (runner.clone(), job.clone());
    let started = std::thread::Builder::new().name("apple-photos".into()).spawn(move || {
        let _row = row;
        run_to_completion(&r, &j, &paths, &album)
    });
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
/// returned running, and announced by the app when it ends) or here (returned finished), with a
/// row in `activity` while Photos works. An error, and no job, for a bad request or while
/// another import is reserved or running.
pub fn import_job(
    runner: &Runner,
    imports: &Imports,
    paths: Vec<String>,
    album: &str,
    background: bool,
    activity: Option<&Activity>,
) -> Result<Arc<ImportJob>, String> {
    let album = check(&paths, album)?;
    let job = imports.begin(paths.len(), &album, false)?;
    job.quiet.store(!background, Ordering::Relaxed);
    let row = row(activity, &album);
    launch(runner, &job, paths, album, background, row);
    Ok(job)
}

/// The import slot an export holds while it writes its files ([`Imports::reserve`]). Handed the
/// files with [`Reservation::run`] or [`Reservation::start`]; dropped unused (the export failed,
/// was refused or never started) it frees the slot, the job ending as skipped.
pub struct Reservation {
    runner: Runner,
    album: String,
    job: Option<Arc<ImportJob>>,
    /// Where [`Reservation::start`]'s import shows while Photos works.
    activity: Option<Activity>,
}

impl Reservation {
    /// Show the import [`Reservation::start`] leaves running in `activity`. ([`Reservation::run`]
    /// runs on the export's worker, and the export's own row covers it.)
    pub fn shown_in(mut self, activity: Activity) -> Self {
        self.activity = Some(activity);
        self
    }

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
            job.finish(Outcome::Skipped { why: why.into(), cancelled });
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
                launch(&self.runner, &job, paths, self.album.clone(), false, None);
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
                let row = row(self.activity.as_ref(), &self.album);
                launch(&self.runner, &job, paths, self.album.clone(), true, row);
                job.json()
            }
            Err(done) => done,
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.finish(Outcome::Skipped { why: "the export ended before writing its files".into(), cancelled: false });
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
mod tests;
