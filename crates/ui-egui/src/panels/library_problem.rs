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

use crate::LightcraftApp;
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

/// What to open at launch: `--library` (`flag`), else `$LIGHTCRAFT_LIBRARY` (`env`), else with
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
    /// [`lightcraft_engine::Session::library_generation`]: Esc keeps it. `None`: no library is open.
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
    /// The window lists the recent libraries to choose from instead (issue #1290); `error` then
    /// says why the last one tried didn't open.
    pub chooser: Option<Chooser>,
}

impl LibraryProblem {
    /// The window for a library at `path` that failed to open with `error`.
    pub fn new(path: impl Into<String>, error: impl Into<String>) -> LibraryProblem {
        LibraryProblem { path: path.into(), error: error.into(), can_retry: true, ..Default::default() }
    }

    /// The window listing the recent libraries, each checked now without writing anything
    /// ([`unavailable`]): one that is gone, isn't a LightCraft library any more or can't be read
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
        json!({"path": self.path, "error": self.error, "temporarySession": self.dismissed, "pendingImport": self.pending_import.len(), "chooser": chooser})
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
    /// The folder holds no LightCraft library any more.
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
            Unavailable::NotAFolder => crate::i18n::tr("This is a file, not a library folder").to_string(),
            Unavailable::Unreadable(e) => format!("{} {e}", crate::i18n::tr("This folder can't be read:")),
            Unavailable::NoLibrary => crate::i18n::tr("There is no LightCraft library in this folder any more").to_string(),
            Unavailable::CatalogUnreadable(e) => format!("{} {e}", crate::i18n::tr("The library's catalog can't be read:")),
            Unavailable::CheckFailed(e) => e.clone(),
            Unavailable::LookupFailed(e) => format!("{} {e}", crate::i18n::tr("This library couldn't be checked:")),
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
const CHECK_LABEL: &str = "Checking recent libraries";

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
fn start_checks(app: &mut LightcraftApp, paths: Vec<String>) {
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
fn reap_checks(app: &mut LightcraftApp) {
    // (every check has the same label: when none is listed any more, the ones still pending died)
    if app.recent_checks.pending.is_empty() || app.tasks.is_running(CHECK_LABEL) {
        return;
    }
    let dead: Vec<String> = app.recent_checks.pending.keys().cloned().collect();
    for path in dead {
        app.recent_checks.pending.remove(&path);
        let why = Unavailable::CheckFailed(crate::i18n::tr("This library couldn't be checked").to_string());
        app.recent_checks.known.insert(path, Answer { why: Some(why), at: web_time::Instant::now() });
    }
    apply_answers(app);
}

/// Put the answers in into the launch list; once none is missing, select as
/// [`LibraryProblem::choose`] does unless the person moved the highlight.
fn apply_answers(app: &mut LightcraftApp) {
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
    if !lightcraft_engine::library::is_library(dir) {
        return Some(Unavailable::NoLibrary);
    }
    // the catalog the open reads first: the snapshot, else the log alone (opened to read, not read)
    let snapshot = dir.join(lightcraft_catalog::journal::SNAPSHOT);
    let catalog = if snapshot.is_file() { snapshot } else { dir.join(lightcraft_catalog::journal::LOG) };
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
}

/// Whether dropped photos wait for a library ([`hold_dropped`]): none is open and the window
/// asking for one is up (not dismissed by Continue Without Saving).
pub fn holds_drops(app: &LightcraftApp) -> bool {
    app.session.library.is_none() && app.library_problem.as_ref().is_some_and(|p| !p.dismissed)
}

/// Photos dropped on the window while no library is open (the launch list, or the window of a
/// library that failed to open): kept with the command line's files and imported once a library
/// opens, instead of going into the session the next library replaces. Gives the photos back when
/// a library is open or Continue Without Saving was chosen.
pub fn hold_dropped(app: &mut LightcraftApp, ctx: &egui::Context, photos: Vec<String>) -> Option<Vec<String>> {
    // called every frame, with nothing dropped most of the time: only a real drop is held (and toasted)
    if photos.is_empty() || !holds_drops(app) {
        return Some(photos);
    }
    let problem = app.library_problem.as_mut()?;
    problem.pending_import.extend(photos);
    app.toast(ctx, crate::i18n::tr("The photos are imported once a library is open"));
    None
}

/// Open `path` (`None`: ask for a folder) through `app.openLibrary`; on success the problem is
/// over (and command-line files are imported), else it shows the new error.
fn open(app: &mut LightcraftApp, path: Option<String>) {
    let params = match &path {
        Some(p) => json!({"path": p}),
        None => json!({}),
    };
    match app.run("app.openLibrary", params) {
        Ok(serde_json::Value::Null) => {} // folder dialog cancelled
        Ok(_) => {
            let files = app.library_problem.take().map(|p| p.pending_import).unwrap_or_default();
            // in the background, like files dropped on the window (issue #374)
            if !files.is_empty()
                && let Err(e) = crate::import::start_paths(app, files)
            {
                log::warn!("import: {e}");
            }
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
pub fn logic(app: &mut LightcraftApp, ctx: &egui::Context) {
    if let Some(p) = &app.library_problem
        && app.session.library.is_some()
        && p.chooser.as_ref().and_then(|c| c.over) != Some(app.session.library_generation())
    {
        // files from the command line go into the library opened another way (New Library…,
        // File ▸ Open Library…), as through the window's own buttons
        let files = app.library_problem.take().map(|p| p.pending_import).unwrap_or_default();
        if !files.is_empty()
            && let Err(e) = crate::import::start_paths(app, files)
        {
            log::warn!("import: {e}");
        }
    }
    alt_at_launch(app, ctx);
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
/// command line are importing into that library.
fn alt_at_launch(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.alt_asks_for_library {
        return;
    }
    let (alt, time) = ctx.input(|i| (i.modifiers.alt, i.time));
    if time > ALT_SECONDS || app.library_problem.is_some() {
        app.alt_asks_for_library = false;
        return;
    }
    if !alt {
        return;
    }
    app.alt_asks_for_library = false;
    if crate::panels::settings::can_switch_library(app).is_err() {
        return;
    }
    let mut p = LibraryProblem::choose_later(&app.ui.settings);
    if let Some(c) = p.chooser.as_mut() {
        c.over = app.session.library.as_ref().map(|_| app.session.library_generation());
    }
    app.library_problem = Some(p);
    ctx.request_repaint();
}

/// The launch list on screen (no dialog over it, e.g. New Library's name).
fn chooser_shown(app: &LightcraftApp) -> Option<&Chooser> {
    let p = app.library_problem.as_ref().filter(|p| !p.dismissed)?;
    p.chooser.as_ref().filter(|_| app.ui.dialog.is_none())
}

/// ↑ / ↓ move the selection, Return opens it (unless a button has the keyboard focus: it acts
/// itself), Esc quits — or, over a library already open, keeps it. Taken before the shortcuts.
fn chooser_keys(app: &mut LightcraftApp, ctx: &egui::Context) {
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
fn select(app: &mut LightcraftApp, at: usize) {
    if let Some(c) = app.library_problem.as_mut().and_then(|p| p.chooser.as_mut())
        && at < c.libraries.len()
    {
        c.selected = Some(at);
        c.touched = true;
    }
}

/// Dim everything behind the window; it swallows clicks, so nothing there can be used meanwhile.
fn dim(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("library-problem-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(true).show(ctx, |ui| {
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
    });
}

/// The blocking window (until a choice is made).
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(problem) = app.library_problem.clone() else { return };
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
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    egui::Window::new(crate::i18n::tr("Your library couldn't be opened"))
        .id(egui::Id::new("library-problem"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(460.0)
        .show(ctx, |ui| {
            // a fixed width: a wrapped label in an auto-sized, centred window would move it every frame
            ui.set_width(440.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            if problem.path.is_empty() {
                ui.label(RichText::new(crate::i18n::tr("LightCraft couldn't find where to keep your library.")).color(t.text));
            } else {
                ui.label(RichText::new(crate::i18n::tr("LightCraft couldn't open the library at")).color(t.text_label));
                ui.label(RichText::new(&problem.path).font(t.semibold(12.5)).color(t.text));
            }
            ui.add(egui::Label::new(RichText::new(&problem.error).color(t.caution)).wrap());
            ui.label(
                RichText::new(
                    "Your photos and edits in it are untouched. You can try again (e.g. after closing another program that has it open \
                     or reconnecting its drive), open a different library or create a new one, or continue without a library — \
                     then nothing you do is saved.",
                )
                .color(t.text_dim),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let mut button = |ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, c: Choice| {
                    let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), format!("button:{id}"), r.rect);
                    if r.clicked() {
                        choice = Some(c);
                    }
                };
                button(ui, "libraryRetry", "Try Again", problem.can_retry && !problem.path.is_empty(), Choice::Retry);
                button(ui, "libraryChoose", "Choose Another Library…", app.services.pick_folder.is_some(), Choice::Choose);
                button(ui, "libraryTemporary", "Continue Without Saving", true, Choice::Temporary);
                button(ui, "libraryQuit", "Quit", true, Choice::Quit);
            });
        });
    if let Some(c) = choice {
        act(app, c);
    }
}

/// Carry out `choice` (a button or key of either window).
fn act(app: &mut LightcraftApp, choice: Choice) {
    let Some(problem) = app.library_problem.clone() else { return };
    let chosen = problem.chooser.as_ref().and_then(|c| c.selected.and_then(|i| c.libraries.get(i)).map(|l| (l.clone(), c.over)));
    match choice {
        Choice::Retry => open(app, Some(problem.path)),
        Choice::Choose => open(app, None),
        Choice::Open => {
            let Some((library, over)) = chosen.filter(|(l, _)| l.unavailable.is_none()) else { return };
            // the library already open behind the list: nothing to switch
            let open_now = app.session.library.as_ref().filter(|l| l.on_disk).is_some_and(|l| l.dir == Path::new(&library.path));
            if over.is_some() && open_now {
                app.library_problem = None;
            } else {
                open(app, Some(library.path));
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
        Choice::Keep => app.library_problem = None,
        Choice::Temporary => {
            if let Some(p) = app.library_problem.as_mut() {
                p.dismissed = true;
            }
            let files = app.library_problem.as_mut().map(|p| std::mem::take(&mut p.pending_import)).unwrap_or_default();
            // in the background, like files dropped on the window (issue #374)
            if !files.is_empty()
                && let Err(e) = crate::import::start_paths(app, files)
            {
                log::warn!("import: {e}");
            }
        }
        Choice::Quit => app.ui.quit = true,
    }
}

/// The launch list (issue #1290): the recent libraries, greyed out with the reason when they
/// can't be opened, and Open / Open Another… / New Library… / Remove from List / Quit.
fn show_chooser(app: &mut LightcraftApp, ctx: &egui::Context, problem: &LibraryProblem, chooser: &Chooser) {
    let t = Tokens::get(ctx);
    dim(ctx);
    let mut choice = None;
    let mut clicked_row = None;
    let can_pick = app.services.pick_folder.is_some();
    let open_now = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    egui::Window::new(crate::i18n::tr("Open a Library"))
        .id(egui::Id::new("library-chooser"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(520.0)
        .show(ctx, |ui| {
            // a fixed width: a wrapped label in an auto-sized, centred window would move it every frame
            ui.set_width(500.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(RichText::new(crate::i18n::tr("Choose the library to open.")).color(t.text_label));
            if chooser.libraries.is_empty() {
                let r = ui.label(RichText::new(crate::i18n::tr("No recent libraries. Open one from a folder, or make a new one.")).color(t.text_dim));
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
                ui.label(RichText::new(crate::i18n::tr("The library itself stays where it is.")).size(11.0).color(t.text_dim));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                button(ui, "libraryOpen", "Open", selected.is_some_and(|l| l.unavailable.is_none()), Choice::Open);
                button(ui, "libraryOpenAnother", "Open Another…", can_pick, Choice::Choose);
                button(ui, "libraryNew", "New Library…", can_pick, Choice::New);
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
pub fn banner(app: &mut LightcraftApp, ui: &mut egui::Ui) {
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
                    RichText::new(crate::i18n::tr("Temporary session — your library isn't open, and nothing you do here is saved."))
                        .font(t.semibold(12.5))
                        .color(t.canvas),
                );
                register(ui.ctx(), "indicator:temporarySession", r.rect);
                let b = ui.add(
                    egui::Button::new(RichText::new(crate::i18n::tr("Open Library…")).color(t.canvas)).fill(egui::Color32::from_black_alpha(40)),
                );
                register(ui.ctx(), "button:libraryReopen", b.rect);
                reopen = b.clicked();
            });
        });
    if reopen && let Some(p) = app.library_problem.as_mut() {
        p.dismissed = false;
    }
}
