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

/// Egui ids/layers shared by the window drawing and the control-channel barrier: the two blocking
/// windows and their dim.
const DIM_ID: &str = "library-problem-dim";
const ERROR_WINDOW_ID: &str = "library-problem";
const CHOOSER_WINDOW_ID: &str = "library-chooser";

/// The named `Foreground` layer used by the dim and the two blocking library windows.
fn foreground_layer(id: &str) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(id))
}

/// The full-screen backdrop that takes the modal layer while the library window is up.
pub(crate) fn dim_layer() -> egui::LayerId {
    foreground_layer(DIM_ID)
}

/// Whether `layer` is one of the blocking library windows' own layers (the error window or the
/// launch list). Their registered controls are the only targets allowed through the dim.
fn is_window_layer(layer: egui::LayerId) -> bool {
    layer == foreground_layer(ERROR_WINDOW_ID) || layer == foreground_layer(CHOOSER_WINDOW_ID)
}

/// Whether a control-channel press must be explicitly refused because the library window's dim
/// owns the modal layer and the target is not one of that window's own controls. Ordinary dialogs
/// keep their existing backdrop semantics; only this blocking window is different.
pub(crate) fn refuses_control_target(top_modal: Option<egui::LayerId>, target: egui::LayerId) -> bool {
    top_modal == Some(dim_layer()) && !is_window_layer(target)
}

/// Which blocking window is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowMode {
    Error,
    Chooser,
}

/// What one draw pass did for the blocking library window (test instrumentation).
///
/// The first-return tests used to prove shielding only through a rating shortcut, which the global
/// shortcut guard now blocks on every settled blocking frame. Recording the pass itself keeps
/// those regressions non-vacuous: removing the shield pass, its discard or its settlement changes
/// this sequence even though the rating shortcut would still stay blocked.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryPass {
    /// The dedicated shield pass for a newly visible window: only the window drew, background
    /// input was dropped, and egui discarded the pass.
    Shield,
    /// A pass that went on to draw the background (with the visible window, when there is one).
    Background,
}

/// First-frame input state for the blocking library window.
///
/// Its `Foreground` areas are non-interactable during their first layout pass, and a text field
/// behind it may still own the keyboard focus. The frame that creates a window therefore draws
/// only that window and drops background input, just as a new notice prompt does.
#[derive(Default)]
pub(crate) struct WindowBarrier {
    /// The window whose completed layout has already settled. `None` until the first pass is
    /// accepted (and again once the window or its banner replaces it).
    shown: Option<WindowMode>,
    /// The next pass 0 is the shielding pass for a newly shown window. Also armed while the
    /// window is hidden by a prompt or by a dialog it opened, so the first pass that actually
    /// draws it again is shielded rather than reusing a background pass's hit-test snapshot.
    transition: bool,
    /// The window actually drew in the pass being completed (reset at the start of every draw).
    drawn: Option<WindowMode>,
    /// The shield pass for the current transition has already run (the discarded pass drew only
    /// the window); the following pass may settle it. Without this, a window that appeared in the
    /// middle of a pass could settle without ever having had a dedicated shield pass.
    shielded: bool,
    /// The current transition may have dropped an injected synthetic batch; cancel its gesture
    /// once so it reports an error instead of a false `ok: true`.
    cancel_injected: bool,
}

impl WindowBarrier {
    /// Begin a new shield transition. It stays armed until a completed pass has drawn the window
    /// after a dedicated shield pass.
    fn begin_transition(&mut self) {
        self.transition = true;
        self.shielded = false;
        self.cancel_injected = true;
    }
}

/// The blocking library window that is present (not dismissed), if any: the launch chooser when
/// it is shown, otherwise the error window.
fn present_mode(app: &LightcraftApp) -> Option<WindowMode> {
    let problem = app.library_problem.as_ref().filter(|p| !p.dismissed)?;
    Some(if problem.chooser.is_some() { WindowMode::Chooser } else { WindowMode::Error })
}

/// Whether the open dialog is the launch list's own New Library name prompt.
///
/// Any other dialog (Settings opened before Alt/Option created the chooser, for example) is not
/// owned by the launch list: the blocking window stays visible over it and keeps owning input,
/// instead of stepping aside for a dialog it did not open.
fn chooser_owns_dialog(app: &LightcraftApp) -> bool {
    app.library_problem.as_ref().is_some_and(|p| p.chooser.is_some())
        && matches!(
            app.ui.dialog.as_ref(),
            Some(crate::state::Dialog::TextPrompt { command, .. }) if command == "app.newLibrary"
        )
}

/// The window actually drawn this pass. A prompt suppresses it, and the launch list yields only to
/// the New Library name dialog it opened itself.
fn visible_mode(app: &LightcraftApp) -> Option<WindowMode> {
    let mode = present_mode(app)?;
    if crate::panels::notices::blocks_input(app) {
        return None;
    }
    if mode == WindowMode::Chooser && chooser_owns_dialog(app) {
        return None;
    }
    Some(mode)
}

/// Whether the blocking library window is actually on screen over the rest of the UI this frame,
/// so a background dialog's own raw input handlers must stay still.
pub(crate) fn blocks_dialog_input(app: &LightcraftApp) -> bool {
    visible_mode(app).is_some()
}

/// A window drawn before its barrier saw it (a command late in a draw pass created it) must not
/// settle this pass: arm the transition so the next frame gets the shielded first draw.
fn note_first_draw(app: &mut LightcraftApp, mode: WindowMode) {
    if app.library_window_input.shown != Some(mode) && !app.library_window_input.transition {
        app.library_window_input.begin_transition();
    }
}

/// Surrender egui's current keyboard focus, so a background field cannot receive text in the
/// window's first shielded pass.
fn clear_focus(ctx: &egui::Context) {
    ctx.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
}

/// Note a new blocking window before any panel or shortcut reads this pass's input. Its first
/// frame clears a background text field's focus and drops text, pointer, wheel, keys and
/// accessibility input. `keys_down` is cleared so no key is left held after the discarded pass;
/// touch End/Cancel still reach egui to clear its private touch state. Modifier and window-focus
/// changes also survive, as for a prompt.
pub(crate) fn prepare_ui_pass(app: &mut LightcraftApp, ctx: &egui::Context, batch_in_flight: bool) {
    if ctx.current_pass_index() != 0 {
        return;
    }
    // A pass 0 starts a new shield cycle; `shield_pass` records that its dedicated pass ran.
    app.library_window_input.shielded = false;
    let Some(mode) = visible_mode(app) else {
        if app.library_problem.as_ref().is_none_or(|p| p.dismissed) {
            app.library_window_input = WindowBarrier::default();
        } else if !app.library_window_input.transition {
            // The window is present but a prompt or the dialog it opened hides it. The next pass
            // that actually draws it must be shielded: while hidden, its controls are absent from
            // the previous pass's widget snapshot, and egui's hit test would belong to the
            // background.
            app.library_window_input.begin_transition();
        }
        return;
    };
    if app.library_window_input.shown != Some(mode) {
        app.library_window_input.begin_transition();
    }
    if !app.library_window_input.transition {
        return;
    }
    // A background text field may have kept or regained focus while the window was hidden by a
    // prompt or dialog; the shielded first frame must not let it receive the host's text.
    clear_focus(ctx);
    if batch_in_flight && std::mem::take(&mut app.library_window_input.cancel_injected) {
        // The library barrier below removes this pass's already-injected events before any panel
        // sees them. Like the prompt barrier, the gesture that queued them must fail, never report
        // a successful delivery of input that was discarded.
        app.cancel_gestures_for_barrier(true, "a blocking library window appeared");
    }
    ctx.stop_dragging();
    ctx.input_mut(|input| {
        // The discarded pass must not leave a key held merely because its release arrived in the
        // window's creation frame.
        input.keys_down.clear();
        input.pointer = egui::PointerState::default();
        input.smooth_scroll_delta = egui::Vec2::ZERO;
        input.events.retain(|event| {
            matches!(
                event,
                egui::Event::ModifiersChanged(_)
                    | egui::Event::WindowFocused(_)
                    | egui::Event::Touch { phase: egui::TouchPhase::End | egui::TouchPhase::Cancel, .. }
            )
        });
    });
}

/// Whether `draw_window` must render only the new blocking window for this first pass.
pub(crate) fn shield_pass(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let shield = ctx.current_pass_index() == 0 && app.library_window_input.transition && visible_mode(app).is_some();
    if shield {
        app.library_window_input.shielded = true;
    }
    shield
}

/// Reset the per-pass draw witness before any panel runs. `show` marks the window only when it
/// actually draws it; a prompt/dialog hiding the window must not inherit a previous pass's mark.
pub(crate) fn begin_draw(app: &mut LightcraftApp) {
    app.library_window_input.drawn = None;
}

/// A completed, nondiscarded pass has laid the window out. Only an actual draw after the shield
/// pass settles it; a window hidden by a prompt or by the dialog it opened, or one that appeared
/// midway through this pass (the banner's Open Library… button), keeps the transition armed so
/// the first pass that draws it again is shielded.
pub(crate) fn finish_ui_pass(app: &mut LightcraftApp, ctx: &egui::Context) {
    if ctx.will_discard() {
        return;
    }
    if let Some(mode) = visible_mode(app) {
        if app.library_window_input.drawn == Some(mode) && (!app.library_window_input.transition || app.library_window_input.shielded) {
            app.library_window_input.shown = Some(mode);
            app.library_window_input.transition = false;
            app.library_window_input.shielded = false;
            app.library_window_input.cancel_injected = false;
        } else if !app.library_window_input.transition {
            app.library_window_input.begin_transition();
        }
    } else if app.library_problem.as_ref().is_none_or(|p| p.dismissed) {
        app.library_window_input = WindowBarrier::default();
    } else if !app.library_window_input.transition {
        // A prompt appeared or changed during the draw and now hides the window.
        app.library_window_input.begin_transition();
    }
}

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
    /// The root UI frame that applied this answer ([`egui::Context::cumulative_frame_nr_for`]).
    /// Scheduling must not ask about a check answer again in the frame that delivered it, which
    /// also covers every layout pass of that frame; `None` for an answer no check worker made
    /// (`opened`, or a check that couldn't start).
    delivered_frame: Option<u64>,
}

impl Answer {
    /// An answer from a check worker, delivered in root UI frame `delivered_frame`.
    fn from_check(why: Option<Unavailable>, delivered_frame: u64) -> Answer {
        Answer { why, at: web_time::Instant::now(), delivered_frame: Some(delivered_frame) }
    }

    /// An answer no check worker made, with no publication fence: it may be asked about again on
    /// the next frame as before.
    fn local(why: Option<Unavailable>) -> Answer {
        Answer { why, at: web_time::Instant::now(), delivered_frame: None }
    }
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
        self.known.insert(path.to_string(), Answer::local(None));
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
                let delivered_frame = ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT);
                app.recent_checks.known.insert(path, Answer::from_check(why, delivered_frame));
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
                app.recent_checks.known.insert(path, Answer::local(Some(Unavailable::CheckFailed(e))));
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
        app.recent_checks.known.insert(path, Answer::local(Some(why)));
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
    let batch_in_flight = app.synthetic_in_flight;
    alt_at_launch(app, ctx);
    // Alt/Option can create the launch list here, after the frame's input was accepted. Drop
    // background text/pointer/keys and give it a shield pass before its own key handler reads them.
    // An already injected synthetic batch is dropped by that barrier and must fail its gesture.
    prepare_ui_pass(app, ctx, batch_in_flight);
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
    // The frame whose UI phase is scheduling now. An answer applied during this frame (by
    // `logic_background`'s `tasks::poll`, or a hidden logic tick just before the first visible UI)
    // has this number too, so it is shown for at least this whole frame: scheduling it again in
    // the same frame would erase it before any completed frame showed it, and a discarded layout
    // pass must not change that. Unknown paths have no answer and still start immediately.
    let frame = ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT);
    let stale: Vec<String> = app
        .ui
        .settings
        .recent_libraries
        .iter()
        .take(crate::state::RECENT_LIBRARIES)
        .filter(|p| {
            !app.recent_checks.is_checking(p)
                && app.recent_checks.known.get(*p).is_none_or(|a| {
                    a.delivered_frame != Some(frame) && (regained || matches!(a.why, Some(Unavailable::NotFound))) && a.at.elapsed() > after
                })
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
    // A quit prompt or settings notice sits above the launch list; its Escape/Return must not
    // quit or switch libraries behind the prompt.
    if !crate::panels::notices::blocks_input(app) {
        chooser_keys(app, ctx);
    }
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

/// The launch list on screen (not covered by the New Library name dialog it opened).
fn chooser_shown(app: &LightcraftApp) -> Option<&Chooser> {
    let p = app.library_problem.as_ref().filter(|p| !p.dismissed)?;
    p.chooser.as_ref().filter(|_| !chooser_owns_dialog(app))
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
/// The dim is `Foreground` and takes the modal layer: a `Middle` dialog such as Settings may be
/// open underneath, and without this the dialog's own modal layer would let its controls receive
/// clicks through the dim. Dialogs are drawn before this window, and a quit prompt or settings
/// notice suppresses this window entirely (`show` returns early), so neither is covered by it.
/// Pressing the dim raises its area, so each window is moved back above it after being shown.
fn dim(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let dim = egui::Area::new(egui::Id::unique(DIM_ID)).order(egui::Order::Foreground).fixed_pos(screen.min).interactable(true).show(ctx, |ui| {
        // swallows clicks: nothing behind the window can be used meanwhile
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
    });
    // A bare interactable Area does not become the modal layer by itself. Without this, a
    // Settings dialog opened before the library window can keep the modal layer and receive a
    // control-channel press that the dim is meant to cover.
    ctx.memory_mut(|memory| memory.set_modal_layer(dim.response.layer_id));
}

/// The blocking window (until a choice is made). `blocked` freezes the barrier of a quit prompt
/// or settings notice that was on screen when this frame began: the launch list/error window is
/// `Order::Foreground` too, so drawing it after the prompt could cover it and let the same
/// click or key act twice. It returns on the first pass after the prompt goes away.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, blocked: bool) {
    if blocked || crate::panels::notices::blocks_input(app) {
        return;
    }
    let Some(problem) = app.library_problem.clone() else { return };
    if problem.dismissed {
        return;
    }
    if let Some(chooser) = &problem.chooser {
        // its own New Library name dialog goes first; the list comes back after it. An unrelated
        // dialog (Settings) does not hide the list: the blocking window stays above it.
        if !chooser_owns_dialog(app) {
            note_first_draw(app, WindowMode::Chooser);
            show_chooser(app, ctx, &problem, chooser);
            app.library_window_input.drawn = Some(WindowMode::Chooser);
        }
        return;
    }
    let t = Tokens::get(ctx);
    note_first_draw(app, WindowMode::Error);
    dim(ctx);
    let mut choice = None;
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    let shown = egui::Window::new(crate::i18n::tr("Your library couldn't be opened"))
        .id(egui::Id::unique(ERROR_WINDOW_ID))
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
                    register(ui, format!("button:{id}"), r.rect);
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
    if let Some(w) = &shown {
        // A press on the dim raises its area; keep this window (and its own buttons) above it.
        ctx.move_to_top(w.response.layer_id);
    }
    app.library_window_input.drawn = Some(WindowMode::Error);
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
    let shown = egui::Window::new(crate::i18n::tr("Open a Library"))
        .id(egui::Id::unique(CHOOSER_WINDOW_ID))
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
                register(ui, "indicator:noRecentLibraries", r.rect);
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
                register(ui, format!("button:{id}"), r.rect);
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
    if let Some(w) = &shown {
        // A press on the dim raises its area; keep the launch list above it.
        ctx.move_to_top(w.response.layer_id);
    }
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
    let r = ui.interact(inner.response.rect, egui::Id::unique(("library-recent", i)), egui::Sense::click());
    register(ui, format!("libraryRecent:{i}"), r.rect);
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
                register(ui, "indicator:temporarySession", r.rect);
                let b = ui.add(
                    egui::Button::new(RichText::new(crate::i18n::tr("Open Library…")).color(t.canvas)).fill(egui::Color32::from_black_alpha(40)),
                );
                register(ui, "button:libraryReopen", b.rect);
                reopen = b.clicked();
            });
        });
    if reopen && let Some(p) = app.library_problem.as_mut() {
        p.dismissed = false;
        // The banner was drawn before the window; reopening it here would otherwise settle in this
        // same pass, whose background already consumed its input. Shield the next frame's first
        // actual window draw.
        app.library_window_input.begin_transition();
        clear_focus(ui.ctx());
        ui.ctx().request_repaint();
    }
}
