//! The library couldn't be opened at launch (issue #100): never a silent in-memory session. A
//! blocking window says which library and why, and offers Try Again, Choose Another Library…,
//! Continue Without Saving and Quit. Continuing shows a banner under the top bar for as long as the
//! session is temporary.
//!
//! The same window lists the recent libraries to choose from when Settings ▸ General ▸ At launch
//! is *Ask me*, or when Alt/Option is held as the window first appears (issue #1290): Open, Open
//! Another…, New Library…, Remove from List and Quit, with the arrow keys, Return and Esc.
//! [`launch_plan`] decides at launch which of the two the desktop app starts with.
//!
//! Whether each recent library can be opened is checked on a worker ([`RecentChecks`]), never on
//! the UI thread: a library on a network share whose server is gone, or on a disk that has to
//! spin up, can take seconds to answer. Until its answer comes the list shows the row as
//! "Checking…" (it can still be opened: opening says why if it can't), and File ▸ Open Recent
//! Library keeps it enabled; the same answers grey out the menu's entries.

use std::path::{Path, PathBuf};

use egui::{Align2, RichText, vec2};
use serde_json::json;

use crate::LccApp;
use crate::state::{AppSettings, LaunchLibrary};
use crate::theme::Tokens;
use crate::widgets::register;

/// How long after the window appears holding Alt/Option still asks (egui time, seconds). The
/// modifier keys can't be read before the first frame, and macOS reports one held since before
/// launch only once it changes, so it is watched over the first frames.
pub const ALT_SECONDS: f64 = 2.0;

/// What the desktop app opens at launch (issue #1290).
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// Open this library folder (`None`: there is nowhere to keep one; the problem window says
    /// so). `alt_asks`: holding Alt/Option as the window appears offers the recent libraries.
    Open { dir: Option<PathBuf>, alt_asks: bool },
    /// Open nothing yet: the window lists the recent libraries ([`LibraryProblem::choose`]).
    Ask,
}

/// What to open at launch: `--library` (`flag`), else `$<PREFIX>_LIBRARY` (`env`), else with
/// *Ask me* the list, else the library last opened, else `default` (the default location).
/// `settings` is `None` without a settings file; `memory` (`--memory`) never asks.
pub fn launch_plan(memory: bool, flag: Option<PathBuf>, env: Option<PathBuf>, settings: Option<&AppSettings>, default: Option<PathBuf>) -> Launch {
    if memory {
        return Launch::Open { dir: None, alt_asks: false };
    }
    if let Some(dir) = flag.or(env) {
        return Launch::Open { dir: Some(dir), alt_asks: false };
    }
    if settings.is_some_and(|s| s.at_launch == LaunchLibrary::Ask) {
        return Launch::Ask;
    }
    let last = settings.map(|s| s.library_path.clone()).filter(|p| !p.is_empty()).map(PathBuf::from);
    Launch::Open { dir: last.or(default), alt_asks: true }
}

/// One library in the launch window's list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecentLibrary {
    pub path: String,
    /// When it was last opened (ISO 8601), if known.
    pub opened: Option<String>,
    /// Why it can't be opened (`None`: it can, or it is still being checked), as checked when
    /// the window opened or when opening it failed.
    pub unavailable: Option<String>,
    /// Its check hasn't answered yet ([`RecentChecks`]).
    pub checking: bool,
    /// Its check has gone on for longer than [`SLOW_AFTER`] (the drive may be asleep or offline).
    pub slow: bool,
}

/// The launch window's list of recent libraries (issue #1290).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chooser {
    /// Newest first, as File ▸ Open Recent Library lists them.
    pub libraries: Vec<RecentLibrary>,
    /// The highlighted row (`None`: the list is empty).
    pub selected: Option<usize>,
    /// The person moved the highlight: answers arriving later don't move it again.
    pub touched: bool,
    /// Its rows' checks were started ([`logic`]).
    pub asked: bool,
    /// The library open behind the window (Alt/Option held at launch), by
    /// [`lcc_engine::Session::library_generation`]: Esc keeps it. `None`: no library is open.
    pub over: Option<u64>,
}

/// A library that failed to open (set by the host at launch).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryProblem {
    /// The library folder ("" when there was none to try, e.g. no home folder).
    pub path: String,
    /// Why it didn't open.
    pub error: String,
    /// Continue Without Saving was chosen: the banner shows instead of the window.
    pub dismissed: bool,
    /// Files from the command line, imported once a library opens.
    pub pending_import: Vec<String>,
    /// Try Again reopens `path` as a library folder (`false` in the browser, whose storage isn't
    /// a folder: reloading the page tries again).
    pub can_retry: bool,
    /// Not a problem (yet): the library's catalog is being upgraded to the current format on a
    /// worker thread ([`start_upgrade`]); a progress window shows until it opens.
    pub upgrading: bool,
    /// Not a problem (yet): the library's catalog is being opened on a worker thread
    /// ([`start_open`]), a format upgrade on the way; a small window shows until it opens.
    pub opening: bool,
    /// The window lists the recent libraries to choose from instead (issue #1290); `error` then
    /// says why the last one tried didn't open.
    pub chooser: Option<Chooser>,
    /// The library remembered by the settings was not there at launch (an unmounted drive, say): Try Again
    /// opens it only once a library is found there again, and never creates an empty one.
    pub never_create: bool,
}

/// Why a library didn't open, from its error: each gets its own words and actions. Only a catalog
/// that failed an integrity check is called damaged (a first start used to call another app's
/// library "corrupt").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemKind {
    /// No folder to try (e.g. no home folder).
    NoLocation,
    /// The catalog's data failed its integrity check.
    Damaged,
    /// Another program has it open.
    InUse,
    /// Written by a newer version of the app.
    Newer,
    /// Another app's catalog (e.g. the upstream app's).
    Foreign,
    /// The folder or a file of it isn't there.
    Missing,
    /// Not allowed to read or write it.
    Permission,
    Other,
}

impl ProblemKind {
    pub fn of(path: &str, error: &str) -> ProblemKind {
        let e = error.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
        if path.is_empty() {
            ProblemKind::NoLocation
        } else if has(&["corrupt catalog data"]) {
            ProblemKind::Damaged
        } else if has(&["already open in"]) {
            ProblemKind::InUse
        } else if has(&["newer version of the app"]) {
            ProblemKind::Newer
        } else if has(&["made by another app"]) {
            ProblemKind::Foreign
        } else if has(&["permission denied", "access is denied", "os error 13", "read-only file system", "os error 30"]) {
            ProblemKind::Permission
        } else if has(&["no such file", "not found", "cannot find", "os error 2)", "os error 3)"]) || !std::path::Path::new(path).exists() {
            ProblemKind::Missing
        } else {
            ProblemKind::Other
        }
    }

    /// `ui.inspect` → `libraryProblem.kind`.
    pub fn id(self) -> &'static str {
        match self {
            ProblemKind::NoLocation => "noLocation",
            ProblemKind::Damaged => "damaged",
            ProblemKind::InUse => "inUse",
            ProblemKind::Newer => "newer",
            ProblemKind::Foreign => "foreign",
            ProblemKind::Missing => "missing",
            ProblemKind::Permission => "permission",
            ProblemKind::Other => "other",
        }
    }

    /// The window's title (English source; translated when shown).
    pub fn title(self) -> &'static str {
        match self {
            ProblemKind::Damaged => "Your catalog is damaged",
            ProblemKind::InUse => "Your catalog is open in another program",
            ProblemKind::Newer => "Your catalog needs a newer version",
            ProblemKind::Foreign => "This catalog belongs to another app",
            ProblemKind::Missing => "Your catalog can't be found",
            ProblemKind::Permission => "Your catalog can't be accessed",
            ProblemKind::NoLocation | ProblemKind::Other => "Your catalog couldn't be opened",
        }
    }

    /// What it means and what to do (English source; translated when shown).
    pub fn advice(self) -> &'static str {
        match self {
            ProblemKind::Damaged => {
                "The catalog failed its integrity check. Restore it from a backup (the message above says where the newest one is), or open or create another catalog."
            }
            ProblemKind::InUse => "Close {app} or the other program that has it open, then try again.",
            ProblemKind::Newer => "It was saved by a newer version of {app}. Update {app} to open it; it was left unchanged.",
            ProblemKind::Foreign => {
                "It was made by another app, whose format {app} can't open. It was left unchanged. Create a new catalog, or open another one."
            }
            ProblemKind::Missing => "If it is on a drive that isn't connected, connect it and try again. Otherwise open or create another catalog.",
            ProblemKind::Permission => "{app} isn't allowed to read or write its folder. Check the folder's permissions, then try again.",
            ProblemKind::NoLocation | ProblemKind::Other => {
                "Your photos and edits in it are untouched. You can try again (e.g. after closing another program that has it open or reconnecting its drive), open a different catalog or create a new one, or continue without a catalog, though then nothing you do is saved."
            }
        }
    }

    /// Try Again can help.
    pub fn retryable(self) -> bool {
        !matches!(self, ProblemKind::Foreign | ProblemKind::Newer | ProblemKind::NoLocation)
    }
}

impl LibraryProblem {
    /// The window for a library at `path` that failed to open with `error`.
    pub fn new(path: impl Into<String>, error: impl Into<String>) -> LibraryProblem {
        LibraryProblem { path: path.into(), error: error.into(), can_retry: true, ..Default::default() }
    }

    pub fn kind(&self) -> ProblemKind {
        ProblemKind::of(&self.path, &self.error)
    }

    /// The window listing the recent libraries, each checked now without writing anything
    /// ([`unavailable`]): one that is gone, isn't one of the app's libraries any more or can't be read
    /// is greyed out with the reason. The last library opened is selected, or else the first one
    /// that can be opened.
    #[cfg(test)]
    pub(crate) fn choose(settings: &AppSettings) -> LibraryProblem {
        let mut p = LibraryProblem::choose_later(settings);
        if let Some(c) = p.chooser.as_mut() {
            for l in &mut c.libraries {
                l.unavailable = unavailable(&l.path).map(|u| u.text());
                l.checking = false;
            }
            c.selected = initial_selection(&c.libraries, &settings.library_path);
            c.asked = true;
        }
        p
    }

    /// [`LibraryProblem::choose`] without looking at any folder: every row is "Checking…" until
    /// [`logic`] has the answers from a worker ([`RecentChecks`]). The last library opened is
    /// selected meanwhile (or the first row); once every answer is in, the selection follows the
    /// same rule as [`LibraryProblem::choose`] unless the person moved it.
    pub fn choose_later(settings: &AppSettings) -> LibraryProblem {
        // as many as File ▸ Open Recent Library keeps (a hand-edited settings file may hold more)
        let libraries: Vec<RecentLibrary> = settings
            .recent_libraries
            .iter()
            .take(crate::state::RECENT_LIBRARIES)
            .map(|path| RecentLibrary {
                path: path.clone(),
                opened: settings.recent_opened.get(path).cloned(),
                unavailable: None,
                checking: true,
                slow: false,
            })
            .collect();
        let selected = libraries.iter().position(|l| l.path == settings.library_path).or((!libraries.is_empty()).then_some(0));
        LibraryProblem { chooser: Some(Chooser { libraries, selected, over: None, touched: false, asked: false }), ..Default::default() }
    }

    /// The session promises to save nothing (the failed-library window or Continue Without
    /// Saving): the host writes no settings meanwhile. The launch list is not such a session.
    pub fn saves_nothing(&self) -> bool {
        self.chooser.is_none()
    }

    /// `ui.inspect` → `libraryProblem`.
    pub fn to_json(&self) -> serde_json::Value {
        let chooser = self.chooser.as_ref().map(|c| {
            let libraries: Vec<serde_json::Value> = c
                .libraries
                .iter()
                .map(|l| json!({"path": l.path, "lastOpened": l.opened, "available": l.unavailable.is_none(), "checking": l.checking, "reason": l.unavailable}))
                .collect();
            json!({"libraries": libraries, "selected": c.selected, "libraryOpen": c.over.is_some()})
        });
        json!({
            "path": self.path,
            "error": self.error,
            "temporarySession": self.dismissed,
            "pendingImport": self.pending_import.len(),
            "upgrading": self.upgrading,
            "opening": self.opening,
            "kind": self.kind().id(),
            "upgrade": lcc_engine::library::migration_progress(),
            "chooser": chooser,
        })
    }
}

/// The row first selected: the last library opened if it can be opened, else the first one that
/// can, else the first row.
fn initial_selection(libraries: &[RecentLibrary], last: &str) -> Option<usize> {
    let can = |l: &RecentLibrary| l.unavailable.is_none();
    libraries.iter().position(|l| l.path == last && can(l)).or_else(|| libraries.iter().position(can)).or((!libraries.is_empty()).then_some(0))
}

/// Why a recent library can't be opened, found on a worker and put into words on the UI thread
/// (the interface language is per thread).
#[derive(Clone, Debug, PartialEq)]
pub enum Unavailable {
    /// Nothing at that path (moved, renamed, or on a disconnected drive).
    NotFound,
    /// A file, not a folder.
    NotAFolder,
    /// The folder can't be listed (the system's reason).
    Unreadable(String),
    /// The folder holds none of the app's libraries any more.
    NoLibrary,
    /// The library's catalog file can't be opened (the system's reason).
    CatalogUnreadable(String),
    /// The check itself failed (its worker stopped, or couldn't start).
    CheckFailed(String),
    /// Looking at the path failed without saying it is gone (a parent folder it may not read, a
    /// share that timed out; the system's reason).
    LookupFailed(String),
}

impl Unavailable {
    /// The folder isn't a library that is there to open (gone, a file, or not a library any more):
    /// what greys out File ▸ Open Recent Library and makes `app.recentLibraries` say `exists: false`.
    /// A folder that can't be listed, an unreadable catalog or a check that failed doesn't say the
    /// library is gone (the launch window still shows why, and offers Open only for one it checked).
    pub fn is_missing(&self) -> bool {
        // a folder that refuses to list is there (permissions, macOS privacy): opening it says why
        matches!(self, Unavailable::NotFound | Unavailable::NotAFolder | Unavailable::NoLibrary)
    }

    /// In the interface language.
    pub fn text(&self) -> String {
        match self {
            Unavailable::NotFound => crate::i18n::tr("Not found: moved, renamed or on a disconnected drive").to_string(),
            Unavailable::NotAFolder => crate::i18n::tr("This is a file, not a catalog folder").to_string(),
            Unavailable::Unreadable(e) => format!("{} {e}", crate::i18n::tr("This folder can't be read:")),
            Unavailable::NoLibrary => crate::i18n::tr("There is no {app} catalog in this folder any more").to_string(),
            Unavailable::CatalogUnreadable(e) => format!("{} {e}", crate::i18n::tr("The catalog can't be read:")),
            Unavailable::CheckFailed(e) => e.clone(),
            Unavailable::LookupFailed(e) => format!("{} {e}", crate::i18n::tr("This catalog couldn't be checked:")),
        }
    }
}

/// How old an answer must be to be asked again: a library not found, on its own (a drive plugged
/// back in); any other, when the window comes back to the front (a library moved away meanwhile).
/// Only "not found" goes on a timer: looking again at the others would wake every external drive
/// and network share in the list over and over.
pub const RECHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a row stays "Checking…" before it says the drive isn't answering (it can still be
/// opened: opening says why if it can't).
pub const SLOW_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// The activity stack's row for a check (English: the stack translates it).
const CHECK_LABEL: &str = "Checking recent catalogs";

/// One path's answer and when it came.
#[derive(Clone, Debug)]
struct Answer {
    why: Option<Unavailable>,
    at: web_time::Instant,
}

/// Answers to "can this recent library be opened?", each found on a worker thread of its own (a
/// library on a network share whose server is gone, or on a sleeping disk, can take seconds or
/// never answer; the others don't wait for it).
pub struct RecentChecks {
    known: std::collections::HashMap<String, Answer>,
    /// Paths being checked, and since when.
    pending: std::collections::HashMap<String, web_time::Instant>,
    /// How one path is checked ([`unavailable`]; tests put a slow one here).
    check: std::sync::Arc<dyn Fn(&str) -> Option<Unavailable> + Send + Sync>,
    /// [`RECHECK_AFTER`] (shorter in tests).
    recheck_after: std::time::Duration,
    /// Whether the window had the focus on the last frame: answers are refreshed when it comes back.
    pub(crate) focused: bool,
}

impl Default for RecentChecks {
    /// Nothing known yet, checking with [`unavailable`].
    fn default() -> Self {
        RecentChecks {
            known: Default::default(),
            pending: Default::default(),
            check: std::sync::Arc::new(unavailable),
            recheck_after: RECHECK_AFTER,
            focused: false,
        }
    }
}

impl RecentChecks {
    /// Why `path` can't be opened, if a check said so (`None`: it can, or it isn't known yet).
    pub fn reason(&self, path: &str) -> Option<&Unavailable> {
        self.known.get(path).and_then(|a| a.why.as_ref())
    }

    /// Whether the library at `path` is there: `Some(false)` when a check found it missing
    /// ([`Unavailable::is_missing`]), `Some(true)` when one found it, `None` when no check could tell.
    pub fn exists(&self, path: &str) -> Option<bool> {
        match self.known.get(path)?.why.as_ref() {
            None => Some(true),
            Some(why) if why.is_missing() => Some(false),
            Some(_) => None,
        }
    }

    /// Whether a check of `path` has answered.
    pub fn is_known(&self, path: &str) -> bool {
        self.known.contains_key(path)
    }

    /// Whether `path` is being checked.
    pub fn is_checking(&self, path: &str) -> bool {
        self.pending.contains_key(path)
    }

    /// Since when `path` has been checked, if it is.
    fn checking_since(&self, path: &str) -> Option<web_time::Instant> {
        self.pending.get(path).copied()
    }

    /// A check is running.
    pub fn is_running(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Check paths with another function (tests).
    pub fn with_check(check: impl Fn(&str) -> Option<Unavailable> + Send + Sync + 'static) -> RecentChecks {
        RecentChecks { check: std::sync::Arc::new(check), ..Default::default() }
    }

    /// Ask again about answers older than `after` when the window comes back (tests).
    #[cfg(test)]
    pub fn recheck_after(mut self, after: std::time::Duration) -> RecentChecks {
        self.recheck_after = after;
        self
    }

    /// `path` was just opened (or made): it can be opened.
    pub fn opened(&mut self, path: &str) {
        self.known.insert(path.to_string(), Answer { why: None, at: web_time::Instant::now() });
    }

    /// Opening `path` from the launch window failed. That says nothing about whether it is there
    /// (another program may have it open): its answer is dropped, so it is checked again on the next
    /// frame. The launch list keeps the open's error on that row for as long as it shows.
    pub fn failed(&mut self, path: &str) {
        self.known.remove(path);
    }

    /// `path` left the list: forget its answer.
    pub fn forget(&mut self, path: &str) {
        self.known.remove(path);
    }
}

/// Check each of `paths` on a worker of its own (one that is being checked already is left to
/// finish). The activity stack lists each check while it runs (a quick one doesn't show: rows
/// appear after half a second); none can be stopped, as a stat that hangs can't be.
fn start_checks(app: &mut LccApp, paths: Vec<String>) {
    for path in paths {
        if app.recent_checks.pending.contains_key(&path) {
            continue;
        }
        // asked again about an answer it has (not after a failed open, which dropped it and says why
        // on its row)
        let recheck = app.recent_checks.known.contains_key(&path);
        let check = app.recent_checks.check.clone();
        let p = path.clone();
        let started = crate::tasks::spawn(app, CHECK_LABEL, Some("library"), move || check(&p), {
            let path = path.clone();
            move |app, ctx, why| {
                app.recent_checks.pending.remove(&path);
                app.recent_checks.known.insert(path, Answer { why, at: web_time::Instant::now() });
                apply_answers(app);
                ctx.request_repaint();
            }
        });
        match started {
            Ok(()) => {
                // a row of the launch list waits for this answer again (a library not found looked for
                // again, a window back to the front): without it, the row kept its old answer
                if recheck
                    && let Some(l) =
                        app.library_problem.as_mut().and_then(|p| p.chooser.as_mut()).and_then(|c| c.libraries.iter_mut().find(|l| l.path == path))
                {
                    l.checking = true;
                    l.slow = false;
                    l.unavailable = None;
                }
                app.recent_checks.pending.insert(path, web_time::Instant::now());
            }
            // no worker thread: say so on the row, and don't try again every frame
            Err(e) => {
                log::warn!("{e}");
                app.recent_checks.known.insert(path, Answer { why: Some(Unavailable::CheckFailed(e)), at: web_time::Instant::now() });
                apply_answers(app);
            }
        }
    }
}

/// Checks whose worker stopped without answering (it panicked: `tasks::poll` already said so)
/// end as failed, so the row and the menu don't wait for them forever.
fn reap_checks(app: &mut LccApp) {
    // (every check has the same label: when none is listed any more, the ones still pending died)
    if app.recent_checks.pending.is_empty() || app.tasks.is_running(CHECK_LABEL) {
        return;
    }
    let dead: Vec<String> = app.recent_checks.pending.keys().cloned().collect();
    for path in dead {
        app.recent_checks.pending.remove(&path);
        let why = Unavailable::CheckFailed(crate::i18n::tr("This catalog couldn't be checked").to_string());
        app.recent_checks.known.insert(path, Answer { why: Some(why), at: web_time::Instant::now() });
    }
    apply_answers(app);
}

/// Put the answers in into the launch list; once none is missing, select as
/// [`LibraryProblem::choose`] does unless the person moved the highlight.
fn apply_answers(app: &mut LccApp) {
    let last = app.ui.settings.library_path.clone();
    let checks = &app.recent_checks;
    let Some(c) = app.library_problem.as_mut().and_then(|p| p.chooser.as_mut()) else { return };
    let checking_before = c.libraries.iter().any(|l| l.checking);
    for l in &mut c.libraries {
        if l.checking && !checks.is_checking(&l.path) && checks.is_known(&l.path) {
            l.unavailable = checks.reason(&l.path).map(Unavailable::text);
            l.checking = false;
        }
    }
    if checking_before && !c.libraries.iter().any(|l| l.checking) && !c.touched {
        c.selected = initial_selection(&c.libraries, &last);
    }
}

/// Why the library at `path` can't be opened (`None`: as far as can be told without opening it).
///
/// Read-only: the folder and its catalog file are looked at, never locked or written (the list
/// may name libraries on other drives, network shares or sleeping disks). A library another
/// program has open is therefore found when Open is pressed: the open fails and the window says
/// who has it. Runs on a worker: the reason is put into words on the UI thread.
fn unavailable(path: &str) -> Option<Unavailable> {
    let dir = Path::new(path);
    // only a definite answer marks it missing: a lookup that fails otherwise (a parent folder it may
    // not read, a share that timed out) is "couldn't check", and the library can still be opened
    match std::fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Unavailable::NotFound),
        Err(e) => return Some(Unavailable::LookupFailed(e.to_string())),
        Ok(m) if !m.is_dir() => return Some(Unavailable::NotAFolder),
        Ok(_) => {}
    }
    match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Some(Unavailable::Unreadable(e.to_string())),
        Err(e) => return Some(Unavailable::LookupFailed(e.to_string())),
        Ok(_) => {}
    }
    // opening would make a new, empty library in it
    if !lcc_engine::library::is_library(dir) {
        return Some(Unavailable::NoLibrary);
    }
    // the catalog the open reads first: the snapshot, else the log alone (opened to read, not read)
    let snapshot = dir.join(lcc_catalog::journal::SNAPSHOT);
    let catalog = if snapshot.is_file() { snapshot } else { dir.join(lcc_catalog::journal::LOG) };
    if let Err(e) = std::fs::File::open(&catalog) {
        return Some(Unavailable::CatalogUnreadable(e.to_string()));
    }
    None
}

/// What the user chose.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Choice {
    Retry,
    Choose,
    Temporary,
    Quit,
    /// The launch list: open the selected library.
    Open,
    /// The launch list: forget the selected library.
    Remove,
    /// The launch list: make a library (New Library…).
    New,
    /// The launch list over a library already open: close the window and keep it.
    Keep,
    /// The problem window: the New Library dialog.
    NewCatalog,
}

/// Whether dropped photos wait for a library ([`hold_dropped`]): none is open and the window
/// asking for one is up (not dismissed by Continue Without Saving).
/// Import the files that waited for a library, in the background like files dropped on the window
/// (issue #374). While another import or a Synchronize Folder runs they keep waiting and are
/// imported as soon as it ends, instead of being dropped (upstream #3403).
pub(crate) fn import_waiting(app: &mut LccApp, ctx: &egui::Context) {
    if app.waiting_import.is_empty() {
        return;
    }
    let files = std::mem::take(&mut app.waiting_import);
    if crate::import::start_paths(app, files.clone()).is_err() {
        app.waiting_import = files;
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

pub fn holds_drops(app: &LccApp) -> bool {
    app.session.library.is_none() && app.library_problem.as_ref().is_some_and(|p| !p.dismissed)
}

/// Photos dropped on the window while no library is open (the launch list, or the window of a
/// library that failed to open): kept with the command line's files and imported once a library
/// opens, instead of going into the session the next library replaces. Gives the photos back when
/// a library is open or Continue Without Saving was chosen.
pub fn hold_dropped(app: &mut LccApp, ctx: &egui::Context, photos: Vec<String>) -> Option<Vec<String>> {
    // called every frame, with nothing dropped most of the time: only a real drop is held (and toasted)
    if photos.is_empty() || !holds_drops(app) {
        return Some(photos);
    }
    let problem = app.library_problem.as_mut()?;
    problem.pending_import.extend(photos);
    app.toast(ctx, crate::i18n::tr("The photos are imported once a catalog is open"));
    None
}

/// Open `path` (`None`: ask for a folder) through `app.openLibrary`, seeding a library created
/// by the open with the demo photos when `demo`; on success the problem is over (and
/// command-line files are imported), else it shows the new error.
fn open(app: &mut LccApp, path: Option<String>, demo: bool) {
    let params = match &path {
        Some(p) => json!({"path": p, "demo": demo}),
        None => json!({}),
    };
    match app.run("app.openLibrary", params) {
        Ok(serde_json::Value::Null) => {} // folder dialog cancelled
        Ok(_) => {
            let files = app.library_problem.take().map(|p| p.pending_import).unwrap_or_default();
            // in the background, like files dropped on the window (issue #374)
            app.waiting_import.extend(files);
        }
        Err(e) => {
            // a failed switch reopens the library that was open behind the launch list
            let generation = app.session.library.as_ref().map(|_| app.session.library_generation());
            if let Some(path) = path.as_deref() {
                app.recent_checks.failed(path);
            }
            if let Some(p) = app.library_problem.as_mut() {
                if let Some(c) = p.chooser.as_mut() {
                    if c.over.is_some() {
                        c.over = generation;
                    }
                    if let Some(l) = c.libraries.iter_mut().find(|l| path.as_deref() == Some(l.path.as_str())) {
                        l.unavailable = Some(e.clone());
                        l.checking = false;
                    }
                }
                if let Some(path) = path {
                    p.path = path;
                }
                p.error = e;
                p.dismissed = false;
            }
        }
    }
}

/// Clear the problem once a library is open (e.g. through Settings → Open Library…) — another
/// one than the launch list was opened over; offer the list when Alt/Option is held as the window
/// appears; and the list's keys.
pub fn logic(app: &mut LccApp, ctx: &egui::Context) {
    import_waiting(app, ctx);
    if let Some(p) = &app.library_problem
        && app.session.library.is_some()
        && p.chooser.as_ref().and_then(|c| c.over) != Some(app.session.library_generation())
    {
        // files from the command line go into the library opened another way (New Library…,
        // File ▸ Open Library…), as through the window's own buttons
        let files = app.library_problem.take().map(|p| p.pending_import).unwrap_or_default();
        app.waiting_import.extend(files);
    }
    alt_at_launch(app, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    reap_opening(app);
    reap_checks(app);
    // the launch list asks for fresh answers once
    let listed: Option<Vec<String>> =
        app.library_problem.as_mut().and_then(|p| p.chooser.as_mut()).and_then(|c| {
            (!std::mem::replace(&mut c.asked, true)).then(|| c.libraries.iter().filter(|l| l.checking).map(|l| l.path.clone()).collect())
        });
    if let Some(listed) = listed {
        for path in &listed {
            if !app.recent_checks.is_checking(path) {
                app.recent_checks.forget(path);
            }
        }
        start_checks(app, listed);
        apply_answers(app);
    }
    // File ▸ Open Recent Library: paths never checked; libraries not found, every RECHECK_AFTER (a
    // drive plugged back in; looking at a path that isn't there wakes nothing); the others when the
    // window comes back to the front and their answer is that old. The list as shown (at most
    // RECENT_LIBRARIES).
    let focused = ctx.input(|i| i.focused);
    let regained = focused && !std::mem::replace(&mut app.recent_checks.focused, focused);
    let after = app.recent_checks.recheck_after;
    let stale: Vec<String> = app
        .ui
        .settings
        .recent_libraries
        .iter()
        .take(crate::state::RECENT_LIBRARIES)
        .filter(|p| {
            !app.recent_checks.is_checking(p)
                && app
                    .recent_checks
                    .known
                    .get(*p)
                    .is_none_or(|a| (regained || matches!(a.why, Some(Unavailable::NotFound))) && a.at.elapsed() > after)
        })
        .cloned()
        .collect();
    start_checks(app, stale);
    if app.recent_checks.is_running() {
        // rows still checking say so once it takes long
        let checks = &app.recent_checks;
        if let Some(c) = app.library_problem.as_mut().and_then(|p| p.chooser.as_mut()) {
            for l in c.libraries.iter_mut().filter(|l| l.checking) {
                l.slow = checks.checking_since(&l.path).is_some_and(|t| t.elapsed() > SLOW_AFTER);
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    chooser_keys(app, ctx);
}

/// Alt/Option held during the first [`ALT_SECONDS`] after the window appears opens the launch list
/// once, over the library that opened (opening another one closes it). Not while files from the
/// command line are importing into that library. The list waits for a catalog still opening (or
/// being upgraded) on a worker — it comes over the library once it is there.
fn alt_at_launch(app: &mut LccApp, ctx: &egui::Context) {
    if !app.alt_asks_for_library {
        return;
    }
    let (alt, time) = ctx.input(|i| (i.modifiers.alt, i.time));
    let starting = app.library_problem.as_ref().is_some_and(|p| p.opening || p.upgrading);
    if time > ALT_SECONDS || (app.library_problem.is_some() && !starting) {
        app.alt_asks_for_library = false;
        return;
    }
    if starting || !alt {
        return;
    }
    app.alt_asks_for_library = false;
    if offer_list(app).is_ok() {
        ctx.request_repaint();
    }
}

/// The launch list over the library open now (Alt/Option held at launch, `catalog.chooser`): Esc
/// keeps that library. Refused while an import runs (as every switch is), while a library that
/// couldn't be opened still waits for an answer, and over a temporary session (Continue Without
/// Saving), which must keep saving nothing; nothing changes when the list is already up (a dialog
/// it opened may be over it).
pub fn offer_list(app: &mut LccApp) -> Result<(), String> {
    crate::panels::settings::can_switch_library(app)?;
    match &app.library_problem {
        None => {}
        Some(p) if p.chooser.is_some() => return Ok(()),
        Some(_) => return Err("the catalog that couldn't be opened comes first: open or create a catalog from its window or the File menu".into()),
    }
    let mut p = LibraryProblem::choose_later(&app.ui.settings);
    if let Some(c) = p.chooser.as_mut() {
        c.over = app.session.library.as_ref().map(|_| app.session.library_generation());
    }
    app.library_problem = Some(p);
    Ok(())
}

/// The launch list on screen (no dialog over it, e.g. New Library's name).
fn chooser_shown(app: &LccApp) -> Option<&Chooser> {
    let p = app.library_problem.as_ref().filter(|p| !p.dismissed)?;
    p.chooser.as_ref().filter(|_| app.ui.dialog.is_none())
}

/// ↑ / ↓ move the selection, Return opens it (unless a button has the keyboard focus: it acts
/// itself), Esc quits — or, over a library already open, keeps it. Taken before the shortcuts.
fn chooser_keys(app: &mut LccApp, ctx: &egui::Context) {
    let Some(c) = chooser_shown(app) else { return };
    let (n, selected, over) = (c.libraries.len(), c.selected, c.over.is_some());
    let focused = ctx.memory(|m| m.focused().is_some());
    use egui::{Key, Modifiers};
    let (up, down, enter, esc) = ctx.input_mut(|i| {
        let up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
        let down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
        let enter = !focused && i.consume_key(Modifiers::NONE, Key::Enter);
        (up, down, enter, i.consume_key(Modifiers::NONE, Key::Escape))
    });
    if (up || down) && n > 0 {
        let at = match selected {
            Some(s) if up => s.saturating_sub(1),
            Some(s) => (s + 1).min(n - 1),
            None => 0,
        };
        select(app, at);
    }
    if enter {
        act(app, Choice::Open);
    } else if esc {
        act(app, if over { Choice::Keep } else { Choice::Quit });
    }
}

/// Highlight row `at` of the launch list.
fn select(app: &mut LccApp, at: usize) {
    if let Some(c) = app.library_problem.as_mut().and_then(|p| p.chooser.as_mut())
        && at < c.libraries.len()
    {
        c.selected = Some(at);
        c.touched = true;
    }
}

/// The activity label of the catalog upgrade ([`start_upgrade`]); its worker is watched for one
/// that stops without an answer ([`reap_opening`]).
#[cfg(not(target_arch = "wasm32"))]
const UPGRADE_LABEL: &str = "Upgrade catalog";
/// The activity label of the catalog open ([`start_open`]).
#[cfg(not(target_arch = "wasm32"))]
const OPEN_LABEL: &str = "Open catalog";

/// Upgrade the catalog of the library in `dir` (an older format, see
/// [`lcc_engine::library::needs_migration`]) on a worker thread, then open it; the window shows
/// the progress meanwhile (a library of hundreds of thousands of photos takes tens of seconds,
/// once). `files` are imported once it is open. A failure shows the usual problem window.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_upgrade(app: &mut LccApp, dir: std::path::PathBuf, files: Vec<String>) {
    let path = dir.to_string_lossy().to_string();
    let empty_start = files.is_empty();
    app.library_problem = Some(LibraryProblem { upgrading: true, pending_import: files, ..LibraryProblem::new(path.clone(), "") });
    let started = crate::tasks::spawn(
        app,
        UPGRADE_LABEL,
        None,
        move || lcc_engine::library::migrate_library(&dir).map_err(|e| e.to_string()),
        move |app, _ctx, res: Result<bool, String>| {
            if let Some(p) = app.library_problem.as_mut() {
                p.upgrading = false;
            }
            match res {
                Ok(_) => {
                    open(app, Some(path), false);
                    opened_start_view(app, empty_start);
                }
                Err(e) => {
                    if let Some(p) = app.library_problem.as_mut() {
                        p.error = e;
                    }
                }
            }
        },
    );
    if let Err(e) = started
        && let Some(p) = app.library_problem.as_mut()
    {
        p.upgrading = false;
        p.error = e;
    }
}

/// Open the catalog of the library in `dir` on a worker thread once the window is up, then open
/// it in the session on the interface's thread: the open used to run before the first frame,
/// where a store left in an older format by an update (its byte-verified backup copy), or simply
/// a large one being read, kept the window silent. The worker does only what must not happen
/// there — [`lcc_engine::library::prepare_library`] — so the window shows the opening meanwhile,
/// with the phase and progress of an upgrade on the way
/// ([`lcc_engine::library::migration_progress`]). `files` are imported once it is open; `demo`
/// seeds a library the open creates with the procedural demo photos; `by_argument`: the library
/// was named on the command line (its catalog file double-clicked), so the one the settings
/// remembered goes into the recent list as the library is opened. A failure shows the usual
/// problem window.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_open(app: &mut LccApp, dir: std::path::PathBuf, files: Vec<String>, demo: bool, by_argument: bool) {
    let path = dir.to_string_lossy().to_string();
    let empty_start = files.is_empty();
    app.library_problem = Some(LibraryProblem { opening: true, pending_import: files, ..LibraryProblem::new(path.clone(), "") });
    let started = crate::tasks::spawn(
        app,
        OPEN_LABEL,
        None,
        move || lcc_engine::library::prepare_library(&dir).map_err(|e| e.to_string()),
        move |app, _ctx, res: Result<bool, String>| {
            if let Some(p) = app.library_problem.as_mut() {
                p.opening = false;
            }
            // a library opened from its catalog file is the one in use from now on, as Open
            // Library makes it: the one the settings remembered is kept in the recent list first
            // (the open below puts the new one at its top)
            let previous = app.ui.settings.library_path.clone();
            if by_argument && !previous.is_empty() && previous != path {
                crate::panels::settings::remember_library(&mut app.ui.settings.recent_libraries, &previous);
            }
            match res {
                Ok(_) => {
                    open(app, Some(path), demo);
                    opened_start_view(app, empty_start);
                }
                Err(e) => {
                    if let Some(p) = app.library_problem.as_mut() {
                        p.error = e;
                    }
                }
            }
        },
    );
    if let Err(e) = started
        && let Some(p) = app.library_problem.as_mut()
    {
        p.opening = false;
        p.error = e;
    }
}

/// What an empty library opens on once it is open at launch (a grid with the sidebar, where the
/// way in is) — after the open, so a library with photos keeps the view it was left in.
fn opened_start_view(app: &mut LccApp, empty_start: bool) {
    if empty_start && app.library_problem.is_none() {
        crate::panels::empty_library::open_on_empty_library(app);
    }
}

/// An opening or upgrade whose worker stopped without an answer (it panicked: `tasks::poll`
/// already said so) ends as the usual problem, never a window without a way out.
#[cfg(not(target_arch = "wasm32"))]
fn reap_opening(app: &mut LccApp) {
    let (opening, upgrading) = match app.library_problem.as_ref() {
        Some(p) => (p.opening, p.upgrading),
        None => return,
    };
    let label = match (opening, upgrading) {
        (true, _) if !app.tasks.is_running(OPEN_LABEL) => OPEN_LABEL,
        (_, true) if !app.tasks.is_running(UPGRADE_LABEL) => UPGRADE_LABEL,
        _ => return,
    };
    log::error!("{label}: its worker stopped without an answer");
    if let Some(p) = app.library_problem.as_mut() {
        p.opening = false;
        p.upgrading = false;
        p.error = crate::i18n::tr("Opening the catalog stopped unexpectedly. Try again, or choose another one.").to_string();
    }
}

/// Dim everything behind a blocking window; clicks there go nowhere.
pub(crate) fn dim(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("library-problem-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(true).show(ctx, |ui| {
        // swallows clicks: nothing behind the window can be used meanwhile
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
    });
}

/// The catalog upgrade's progress window.
fn upgrade_window(ctx: &egui::Context, path: &str) {
    let t = Tokens::get(ctx);
    dim(ctx);
    let progress = lcc_engine::library::migration_progress();
    let frame = crate::dialog_frame::card_frame(ctx);
    egui::Window::new(crate::i18n::tr("Upgrading your catalog"))
        .id(egui::Id::new("library-upgrade"))
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 440.0));
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, crate::i18n::tr("Upgrading your catalog"), None);
            ui.label(RichText::new(crate::i18n::tr("{app} is moving the catalog at")).color(t.text_label));
            ui.label(RichText::new(path).font(t.semibold(12.5)).color(t.text));
            ui.label(
                RichText::new(crate::i18n::tr(
                    "to its new catalog format. This happens once; a large library takes a minute. The old files are kept in the \
                     library's backups folder.",
                ))
                .color(t.text_dim),
            );
            let (frac, text) = match &progress {
                Some(p) => (p.fraction(), crate::i18n::tr(p.phase.label()).to_string()),
                None => (0.0, crate::i18n::tr("Starting…").to_string()),
            };
            let text = match &progress {
                Some(p) if p.phase == lcc_engine::catalog::progress::MigrationPhase::Writing => {
                    format!("{text}… {} / {}", p.done, p.total)
                }
                _ => format!("{text}…"),
            };
            let r = ui.add(egui::ProgressBar::new(frac).desired_width(440.0).text(RichText::new(text).font(t.font(11.5))));
            register(ui.ctx(), "progress:libraryUpgrade", r.rect);
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}

/// The window shown while the catalog opens on a worker thread ([`start_open`]): what would
/// otherwise be a silent wait (a window that doesn't answer) while a format upgrade after an
/// update makes its byte-verified copy, or a large store is read. An upgrade on the way moves
/// the bar by its bytes copied; otherwise it sweeps until the library is there.
fn opening_window(ctx: &egui::Context, path: &str) {
    let t = Tokens::get(ctx);
    dim(ctx);
    let progress = lcc_engine::library::migration_progress();
    let frame = crate::dialog_frame::card_frame(ctx);
    egui::Window::new(crate::i18n::tr("Opening your catalog"))
        .id(egui::Id::new("library-opening"))
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 440.0));
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, crate::i18n::tr("Opening your catalog"), None);
            ui.label(RichText::new(crate::i18n::tr("{app} is opening the catalog at")).color(t.text_label));
            ui.label(RichText::new(path).font(t.semibold(12.5)).color(t.text));
            ui.label(
                RichText::new(crate::i18n::tr(
                    "One moment. After an update, a copy of the catalog is kept and checked first; a large catalog takes a little \
                     while.",
                ))
                .color(t.text_dim),
            );
            let (frac, text) = match &progress {
                Some(p) => (p.fraction(), crate::i18n::tr(p.phase.label()).to_string()),
                None => {
                    // nothing to measure yet: the bar sweeps back and forth so it is alive (the
                    // window repaints while it is up, below)
                    let x = (ctx.input(|i| i.time as f32) * 0.4).fract();
                    (if x < 0.5 { x * 2.0 } else { (1.0 - x) * 2.0 }, crate::i18n::tr("Starting…").to_string())
                }
            };
            let text = match &progress {
                Some(p) if p.phase == lcc_engine::catalog::progress::MigrationPhase::Writing => {
                    format!("{text}… {} / {}", p.done, p.total)
                }
                _ => format!("{text}…"),
            };
            let r = ui.add(egui::ProgressBar::new(frac).desired_width(440.0).text(RichText::new(text).font(t.font(11.5))));
            register(ui.ctx(), "progress:libraryOpening", r.rect);
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}

/// The blocking window (until a choice is made).
pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let Some(problem) = app.library_problem.clone() else { return };
    if problem.opening {
        opening_window(ctx, &problem.path);
        return;
    }
    if problem.upgrading {
        upgrade_window(ctx, &problem.path);
        return;
    }
    if problem.dismissed {
        return;
    }
    if let Some(chooser) = &problem.chooser {
        // a dialog it opened (New Library's name) goes first; the list comes back after it
        if app.ui.dialog.is_none() {
            show_chooser(app, ctx, &problem, chooser);
        }
        return;
    }
    let t = Tokens::get(ctx);
    dim(ctx);
    let mut choice = None;
    // a card, as every dialog: rounded, the title inside it, the main action in the accent colour
    let frame = egui::Frame::window(&ctx.global_style())
        .inner_margin(egui::Margin { left: 20, right: 20, top: 16, bottom: 16 })
        .corner_radius(16.0)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(26)))
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: egui::Color32::from_black_alpha(110) });
    let kind = problem.kind();
    let window_id = egui::Id::new("library-problem");
    let shown = egui::Window::new(crate::i18n::tr(kind.title()))
        .id(window_id)
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(480.0)
        .show(ctx, |ui| {
            // a fixed width (narrower on a phone): a wrapped label in an auto-sized, centred window would move it every frame
            let w = (ctx.content_rect().width() - 64.0).clamp(240.0, 460.0);
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 8.0;
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| {
                let h = ui.label(RichText::new(crate::i18n::tr(kind.title())).font(t.semibold(17.0)).color(t.text));
                register(ui.ctx(), "dialog:title", h.rect);
                ui.add_space(2.0);
                if problem.path.is_empty() {
                    ui.label(RichText::new(crate::i18n::tr("{app} couldn't find where to keep your catalog.")).color(t.text_label));
                } else {
                    ui.label(RichText::new(crate::i18n::tr("{app} couldn't open the catalog at")).color(t.text_label));
                    ui.add(egui::Label::new(RichText::new(&problem.path).font(t.semibold(13.0)).color(t.text)).wrap());
                }
                // what went wrong, in a soft caution box
                egui::Frame::NONE.fill(t.caution.gamma_multiply(0.12)).corner_radius(10.0).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(&problem.error).color(t.caution)).wrap());
                });
                ui.add(egui::Label::new(RichText::new(crate::i18n::tr(kind.advice())).color(t.text_dim)).wrap());
            });
            layout.footer(ui, |ui| {
                use crate::button::{Button, Kind};
                ui.add_space(6.0);
                let mut button = |ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, kind: Kind, c: Choice| {
                    if Button::new(label).kind(kind).enabled(enabled).widget(format!("button:{id}")).show(ui).clicked() {
                        choice = Some(c);
                    }
                };
                // the other ways on, wrapping on a narrow window
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    button(ui, "libraryChoose", "Choose Another Catalog…", app.services.pick_folder.is_some(), Kind::Secondary, Choice::Choose);
                    button(ui, "libraryNew", "New Catalog…", true, Kind::Secondary, Choice::NewCatalog);
                    button(ui, "libraryTemporary", "Continue Without Saving", true, Kind::Secondary, Choice::Temporary);
                });
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    button(ui, "libraryRetry", "Try Again", problem.can_retry && kind.retryable(), Kind::Primary, Choice::Retry);
                    button(ui, "libraryQuit", "Quit", true, Kind::Ghost, Choice::Quit);
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    if let Some(c) = choice {
        act(app, c);
    }
}

/// Carry out `choice` (a button or key of either window).
fn act(app: &mut LccApp, choice: Choice) {
    let Some(problem) = app.library_problem.clone() else { return };
    let chosen = problem.chooser.as_ref().and_then(|c| c.selected.and_then(|i| c.libraries.get(i)).map(|l| (l.clone(), c.over)));
    match choice {
        Choice::Retry => {
            // a remembered library that is still not there stays a problem; nothing is created in its place
            let at = lcc_engine::library::resolve_dir(Path::new(&problem.path)).unwrap_or_else(|_| problem.path.clone().into());
            if problem.never_create && !lcc_engine::library::is_library(&at) {
                return;
            }
            open(app, Some(problem.path), false)
        }
        Choice::Choose => open(app, None, false),
        Choice::Open => {
            let Some((library, over)) = chosen.filter(|(l, _)| l.unavailable.is_none()) else { return };
            // the library already open behind the list: nothing to switch
            let open_now = app.session.library.as_ref().filter(|l| l.on_disk).is_some_and(|l| l.dir == Path::new(&library.path));
            if over.is_some() && open_now {
                app.library_problem = None;
            } else {
                open(app, Some(library.path), false);
            }
        }
        Choice::Remove => {
            let Some((library, _)) = chosen else { return };
            crate::panels::settings::forget_library(&mut app.ui.settings, &library.path);
            app.recent_checks.forget(&library.path);
            if let Some(c) = app.library_problem.as_mut().and_then(|p| p.chooser.as_mut()) {
                c.libraries.retain(|l| l.path != library.path);
                c.selected = c.selected.map(|s| s.min(c.libraries.len().saturating_sub(1))).filter(|_| !c.libraries.is_empty());
            }
        }
        Choice::New => {
            // asks for the name, then the place; the list shows again if that's cancelled
            match app.run("app.newLibrary", json!({})) {
                Ok(_) => {}
                Err(e) => {
                    if let Some(p) = app.library_problem.as_mut() {
                        p.error = e;
                    }
                }
            }
        }
        Choice::NewCatalog => {
            // the New Library dialog (it opens the library it creates, which ends the problem)
            if let Some(p) = app.library_problem.as_mut() {
                p.dismissed = true;
            }
            new_catalog_dialog(app);
        }
        Choice::Keep => app.library_problem = None,
        Choice::Temporary => {
            if let Some(p) = app.library_problem.as_mut() {
                p.dismissed = true;
            }
            let files = app.library_problem.as_mut().map(|p| std::mem::take(&mut p.pending_import)).unwrap_or_default();
            // in the background, like files dropped on the window (issue #374)
            app.waiting_import.extend(files);
        }
        Choice::Quit => app.ui.quit = true,
    }
}

/// New Library… from the problem window: the New Library dialog, under the Pictures folder by default.
fn new_catalog_dialog(app: &mut LccApp) {
    let parent = lcc_engine::library::default_dir().and_then(|d| d.parent().map(std::path::Path::to_path_buf));
    let params = json!({"parent": parent.map(|d| d.to_string_lossy().into_owned())});
    if let Err(e) = app.run("catalog.new", params) {
        log::warn!("new catalog: {e}");
    }
}

/// The launch list (issue #1290): the recent libraries, greyed out with the reason when they
/// can't be opened, and Open / Open Another… / New Library… / Remove from List / Quit.
fn show_chooser(app: &mut LccApp, ctx: &egui::Context, problem: &LibraryProblem, chooser: &Chooser) {
    let t = Tokens::get(ctx);
    dim(ctx);
    let mut choice = None;
    let mut clicked_row = None;
    let can_pick = app.services.pick_folder.is_some();
    let open_now = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let frame = crate::dialog_frame::card_frame(ctx);
    egui::Window::new(crate::i18n::tr("Open a Catalog"))
        .id(egui::Id::new("library-chooser"))
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(520.0)
        .show(ctx, |ui| {
            // a fixed width (narrower on a phone): a wrapped label in an auto-sized, centred window would move it every frame
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 500.0));
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, crate::i18n::tr("Open a Catalog"), None);
            ui.label(RichText::new(crate::i18n::tr("Choose the catalog to open.")).color(t.text_label));
            if chooser.libraries.is_empty() {
                let r = ui.label(RichText::new(crate::i18n::tr("No recent catalogs. Open one from a folder, or make a new one.")).color(t.text_dim));
                register(ui.ctx(), "indicator:noRecentLibraries", r.rect);
            }
            egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, library) in chooser.libraries.iter().enumerate() {
                    let r = recent_row(ui, &t, i, library, chooser.selected == Some(i), open_now.as_deref() == Some(Path::new(&library.path)));
                    if r.double_clicked() {
                        clicked_row = Some((i, true));
                    } else if r.clicked() {
                        clicked_row = Some((i, false));
                    }
                }
            });
            if !problem.error.is_empty() {
                ui.add(egui::Label::new(RichText::new(&problem.error).color(t.caution)).wrap());
            }
            let selected = chooser.selected.and_then(|i| chooser.libraries.get(i));
            let mut button = |ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, c: Choice| {
                let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)).min_size(vec2(0.0, 26.0)));
                register(ui.ctx(), format!("button:{id}"), r.rect);
                if r.clicked() {
                    choice = Some(c);
                }
            };
            ui.horizontal(|ui| {
                button(ui, "libraryForget", "Remove from List", selected.is_some(), Choice::Remove);
                ui.label(RichText::new(crate::i18n::tr("The catalog itself stays where it is.")).size(11.0).color(t.text_dim));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                button(ui, "libraryOpen", "Open", selected.is_some_and(|l| l.unavailable.is_none()), Choice::Open);
                button(ui, "libraryOpenAnother", "Open Another…", can_pick, Choice::Choose);
                button(ui, "libraryNew", "New Catalog…", can_pick, Choice::New);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    button(ui, "libraryQuit", "Quit", true, Choice::Quit);
                });
            });
        });
    if let Some((i, open)) = clicked_row {
        select(app, i);
        if open {
            act(app, Choice::Open);
        }
    }
    if let Some(c) = choice {
        act(app, c);
    }
}

/// One row of the launch list: the library's name and when it was last opened, the folder it is
/// in, and why it can't be opened (then greyed out). Registered as `libraryRecent:<index>`.
fn recent_row(ui: &mut egui::Ui, t: &Tokens, i: usize, library: &RecentLibrary, selected: bool, open: bool) -> egui::Response {
    let dir = Path::new(&library.path);
    let name = crate::panels::settings::library_name(dir);
    let parent = dir.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let ok = library.unavailable.is_none();
    let (main, dim) = if ok { (t.text, t.text_dim) } else { (t.text_disabled, t.text_disabled) };
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(name).font(t.semibold(12.5)).color(main)).truncate().selectable(false));
            let when = if open {
                Some(crate::i18n::tr("Open now").to_string())
            } else {
                library
                    .opened
                    .as_deref()
                    .and_then(|iso| iso.get(..10))
                    .map(|day| format!("{} {}", crate::i18n::tr("Last opened"), crate::i18n::date_group_label(day, false)))
            };
            if let Some(when) = when {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(when).size(11.0).color(dim)).selectable(false));
                });
            }
        });
        ui.add(egui::Label::new(RichText::new(parent).size(11.0).color(dim)).truncate().selectable(false));
        if let Some(why) = &library.unavailable {
            ui.add(egui::Label::new(RichText::new(why).size(11.0).color(t.caution)).wrap().selectable(false));
        } else if library.checking {
            let text =
                if library.slow { crate::i18n::tr("Not answering yet: the drive may be asleep or offline") } else { crate::i18n::tr("Checking…") };
            ui.add(egui::Label::new(RichText::new(text).size(11.0).color(dim)).wrap().selectable(false));
        }
    });
    let r = ui.interact(inner.response.rect, egui::Id::new(("library-recent", i)), egui::Sense::click());
    register(ui.ctx(), format!("libraryRecent:{i}"), r.rect);
    let fill = if selected {
        Some(t.tool_active)
    } else if r.hovered() {
        Some(t.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(background, egui::Shape::rect_filled(r.rect, 4.0, fill));
    }
    r
}

/// The banner under the top bar while the session is temporary (Continue Without Saving).
pub fn banner(app: &mut LccApp, ui: &mut egui::Ui) {
    if !app.library_problem.as_ref().is_some_and(|p| p.dismissed) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let mut reopen = false;
    egui::Panel::top("library-problem-banner")
        .exact_size(30.0)
        .frame(egui::Frame::NONE.fill(t.caution).inner_margin(egui::Margin::symmetric(12, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let r = ui.label(
                    RichText::new(crate::i18n::tr("Temporary session: your catalog isn't open, and nothing you do here is saved."))
                        .font(t.semibold(12.5))
                        .color(t.canvas),
                );
                register(ui.ctx(), "indicator:temporarySession", r.rect);
                let b = ui.add(
                    egui::Button::new(RichText::new(crate::i18n::tr("Open Catalog…")).color(t.canvas)).fill(egui::Color32::from_black_alpha(40)),
                );
                register(ui.ctx(), "button:libraryReopen", b.rect);
                reopen = b.clicked();
            });
        });
    if reopen && let Some(p) = app.library_problem.as_mut() {
        p.dismissed = false;
    }
}
