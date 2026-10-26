//! LightCraft's egui frontend: a Lightroom-style UI over `lightcraft-engine`.
//!
//! The UI is thin: every action goes through [`LightcraftApp::run`], which handles UI commands
//! (views, panels, zoom — see [`menus::ui_commands`]) and forwards everything else to the engine.
//! The same entry point serves menus, shortcuts, buttons and the control channel ([`control`]).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// LightCraft as craft-update knows it, with its 128 px icon and accent colour: the update
/// prompt shows both, and an AppImage's menu entry gets the icon.
#[cfg(not(target_arch = "wasm32"))]
pub const UPDATE_APP: craft_update::app::App = craft_update::app::App {
    icon_png: Some(include_bytes!("../../../assets/app-icon/hicolor/128x128/apps/ai.storyteller.lightcraft.png")),
    accent: Some([0xf2, 0xa5, 0x16]),
    ..craft_update::suite::LIGHTCRAFT
};

pub mod brush_live;
pub mod canvas_keys;
mod command_error;
pub mod control;
pub mod destination;
pub mod dialogs;
pub mod docking;
mod drop_overlay;
pub mod export_task;
mod find_similar;
mod focus_scan;
mod frame_rate;
pub mod headless;
pub mod i18n;
pub mod icons;
pub mod lens_db;
pub mod lightroom_import;
pub mod links;
pub mod loupe_timing;
pub mod menus;
mod missing_sweep;
pub mod panels;
pub mod pick;
pub mod recheck;
pub mod region;
pub mod reload;
pub mod render;
pub mod shortcuts;
pub mod sidebar;
pub mod softpaint;
pub mod space_hand;
pub mod startup;
pub mod state;
pub mod sync;
pub mod tasks;
pub mod theme;
pub mod widgets;
pub mod window_place;

#[cfg(test)]
mod test_budget;
#[cfg(test)]
mod tests;
#[cfg(test)]
use tests::panels as tests_panels;
#[cfg(test)]
use tests::{fixtures as tests_fixtures, render_fixtures as tests_render_fixtures};

use std::sync::mpsc::{Receiver, Sender};

use lightcraft_engine::{AiModel, ErrorKind, Session};
use serde_json::Value;

pub use command_error::CommandError;
pub use control::{ControlRequest, ControlResponse};
pub use dialogs::credits;
pub use dialogs::import;
pub use dialogs::lightroom_destination;
pub use dialogs::merge;
pub use dialogs::print_view;
pub use dialogs::slideshow_export;
pub use dialogs::updates_ui;
pub use menus::menu_level;
pub use menus::menubar;
pub use state::UiState;
pub use widgets::album_picker;
pub use widgets::button;
pub use widgets::date_picker;
pub use widgets::progress;
pub use widgets::rating;
pub use widgets::text_field;

mod app;
mod auto_settings;
mod caches;
mod clock;
mod services;

#[cfg(test)]
use app::{import_tick_allowed, sam_download_ended, shot_ready};
pub(crate) use caches::key_of;
pub use caches::{FilterValues, LibraryCounts, RulesView};
use clock::AppClock;
pub use clock::{monotonic_ms, now_ms};
#[cfg(target_os = "linux")]
pub use services::OpenModelFolderFn;
pub use services::{
    CheckEditorFn, HostAction, OpenAllWithFn, OpenUrlFn, OpenWithFn, PickFiles, PickFolder, PngEncode, RevealFn, SaveFile, ScreenProfileFn, Services,
    SharedWrite, SystemThemeFn, WriteFn,
};
use state::{GestureFrame, InjectedPress};

const TOAST_SECONDS: f64 = 1.4;
/// How long a toast saying why a command was refused stays: a sentence to read, as
/// [`LightcraftApp::toast_error`]'s.
const REFUSAL_SECONDS: f64 = 6.0;

/// A command that didn't run: the whole error (for agents, the log and the status) and what it
/// says to the person who asked (the reason, without the command's id).
pub(crate) struct Refusal {
    error: String,
    why: String,
    /// What kind of error it is, for code that decides on it ([`LightcraftApp::run_typed`]).
    kind: ErrorKind,
}

impl Refusal {
    /// An error that is already text (the interface's own commands). Mostly words for a person
    /// as they are; a command that wraps an engine command hands on the engine's error already
    /// turned into text, and of that only the reason is for the person. `error` stays whole.
    fn plain(error: String) -> Refusal {
        let why = engine_reason(&error).map(sentence).unwrap_or_else(|| error.clone());
        Refusal { why, error, kind: ErrorKind::Other }
    }
}

/// A sentence starts with a capital, whatever the command wrote. Only when it starts with a word:
/// a file name or a path ("img_0012.jpg is a virtual copy", "photos/a.jpg: …") is someone's
/// spelling and stays as it is, and so does a letter whose capital is not one letter (ß).
fn sentence(why: &str) -> String {
    let first_word = why.split_whitespace().next().unwrap_or_default().trim_end_matches([',', ':', ';']);
    let mut letters = why.chars();
    match letters.next() {
        Some(first) if first_word.chars().all(char::is_alphabetic) && first.to_uppercase().count() == 1 => {
            first.to_uppercase().chain(letters).collect()
        }
        _ => why.to_string(),
    }
}

/// The reason in an [`EngineError`](lightcraft_engine::EngineError) that is already text: what
/// follows ``command `id` is not available right now: `` or ``invalid parameters for `id`: ``
/// (its `Disabled` and `BadParams` formats). `None` for anything else, including text that only
/// begins like one: the id must be a command's (letters, digits, `.`, `_`, `-`) between the
/// backticks, so the reason is cut at the first separator and may hold colons and backticks.
fn engine_reason(error: &str) -> Option<&str> {
    const SHAPES: [(&str, &str); 2] = [("command `", "` is not available right now: "), ("invalid parameters for `", "`: ")];
    SHAPES.iter().find_map(|(before, after)| {
        let (id, reason) = error.strip_prefix(before)?.split_once(after)?;
        let is_id = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        (is_id && !reason.trim().is_empty()).then_some(reason)
    })
}

impl From<lightcraft_engine::EngineError> for Refusal {
    fn from(e: lightcraft_engine::EngineError) -> Refusal {
        use lightcraft_engine::EngineError;
        let why = match &e {
            EngineError::Disabled(_, why) => why.clone(),
            EngineError::BadParams { msg, .. } => msg.clone(),
            other => other.to_string(),
        };
        Refusal { error: e.to_string(), why: sentence(&why), kind: e.kind() }
    }
}

/// The `path` of a display profile read from the screen rather than chosen as a file.
pub const SCREEN_PROFILE: &str = "screen";

#[derive(Clone, Copy, Debug, Default)]
pub struct Perf {
    /// Layout of the last frame ([`LightcraftApp::ui`], including commands run from widgets).
    pub frame_ms: f64,
    /// Per-frame logic before layout ([`LightcraftApp::logic`]: control channel, shortcuts,
    /// render polling, pending catalog persistence).
    pub logic_ms: f64,
    /// The whole update of the last frame: logic + layout.
    pub update_ms: f64,
    /// The slowest whole update since start.
    pub max_update_ms: f64,
    /// The previous frame's whole time on this thread, painting (texture uploads) included, as
    /// the host reports it (eframe's `cpu_usage`); `None` without a host report (headless).
    pub host_frame_ms: Option<f64>,
    pub fps: f64,
}

/// Why quitting stopped to ask first (`panels::notices`).
#[derive(Clone, Debug, PartialEq)]
pub enum QuitPrompt {
    /// Changes couldn't be written to disk (issue #103).
    Unsaved(String),
    /// Tasks that quitting would cut short are running: an import, an export… (issue #345).
    Tasks(String),
}

impl QuitPrompt {
    /// The prompt's message.
    pub fn text(&self) -> &str {
        match self {
            QuitPrompt::Unsaved(text) | QuitPrompt::Tasks(text) => text,
        }
    }
}

pub struct LightcraftApp {
    /// Per-catalog-revision caches of library-wide results the panels show every frame
    /// (expensive on big libraries).
    pub caches: Caches,
    pub session: Session,
    pub ui: UiState,
    pub services: Services,
    pub renderer: render::Renderer,
    pub perf: Perf,
    /// macOS: the host draws the traffic lights over our top bar.
    pub integrated_titlebar: bool,
    /// The `movable` flag last given to the window (macOS, `craft_window::title_bar::sync_movable`).
    pub window_movable: Option<bool>,
    /// The host installed a native menu bar (no in-window menus then).
    pub native_menu: bool,
    /// Shortcuts the native menu bar currently handles (`Cmd+Z`, `G`…): the egui shortcut handler
    /// leaves them alone so nothing fires twice.
    pub native_shortcuts: std::collections::HashSet<String>,
    /// The keyboard shortcuts editor is waiting for a key press for this command: no shortcut
    /// fires (the native menu bar drops its accelerators too) until it gets one or is cancelled.
    pub recording_shortcut: Option<String>,
    /// A text field had the keyboard at the end of the last frame. egui drops the focus as Esc
    /// arrives, before the shortcuts are read, so this is how Esc there stays the field's own.
    pub text_focus: bool,
    /// The host is [`headless::Headless`] (it answers viewport screenshot commands itself).
    pub headless_host: bool,
    /// Warnings to show one at a time (damaged settings files…, issue #103).
    pub notices: Vec<String>,
    pub(crate) updates_rt: updates_ui::Runtime,
    /// Quitting was stopped to ask first: changes couldn't be saved, or tasks are running.
    pub quit_prompt: Option<QuitPrompt>,
    /// Keyboard/focus/input state for the prompt above, including its one-frame creation barrier.
    pub(crate) notice_input: panels::notices::PromptInput,
    /// Quit Anyway was chosen: the window may close with unsaved changes.
    pub quit_confirmed: bool,
    control_rx: Option<Receiver<ControlRequest>>,
    pending_screenshots: Vec<PendingShot>,
    /// Synthetic-input control requests waiting for a fresh layout and delivery (plan §4.5).
    pending_gestures: Vec<PendingGesture>,
    /// Completed, nondiscarded UI layouts. A gesture admitted before this advanced may resolve.
    layout_epoch: u64,
    screenshot_token: u64,
    /// Bytes reserved by admitted, not-yet-drained screenshots (in-flight and completed alike).
    shot_bytes: usize,
    /// A geometry change since the last logic call: a completed image from an older epoch is
    /// stale even when the window has meanwhile been resized back to the issued size.
    shot_geometry_epoch: u64,
    last_shot_geometry: Option<ShotGeometry>,
    /// The texture limit the host last reported (`RawInput::max_texture_side`), retained across
    /// passes where the host reports it only once.
    max_texture_side: usize,
    /// Monotonic app clock (`web_time::Instant`), not the wall clock and not egui's per-pass time:
    /// timers must keep advancing while the window is hidden.
    clock: AppClock,
    /// The language already sent to `Context::set_language`, so a logic pass does not allocate a
    /// `String` just to compare it.
    accesskit_language: Option<String>,
    /// When playback was auto-paused because the window became hidden (`None`: it is running, or
    /// the user paused it). On unhide the pause is lifted and the slide keeps its remaining time.
    slideshow_hidden_at: Option<f64>,
    /// The app (not the user) paused the slideshow because the window was hidden.
    slideshow_auto_paused: bool,
    /// Files dropped since the last UI pass, consumed exactly once by the UI phase.
    dropped_files: Vec<egui::DroppedFileHandle>,
    /// Prefix of `dropped_files` that arrived behind a prompt, library window or its picker.
    /// Preserve only this prefix through subsequent dialogs/imports; later arrivals still take
    /// the ordinary refusal path unless they too arrive while the original barrier is up.
    deferred_dropped_file_count: usize,
    /// A synthetic batch was injected into a pass and has not been consumed by a completed
    /// (nondiscarded) UI pass yet: a hidden logic tick must not inject another one.
    synthetic_in_flight: bool,
    /// Completed, nondiscarded root UI passes; host frames, not layout passes.
    ui_passes: u64,
    /// Test-only: the library barrier's per-pass decisions, so a first-return test can observe the
    /// dedicated shield/discard pass and the following background pass rather than only its input
    /// effects (which the global shortcut guard also hides).
    #[cfg(test)]
    pub(crate) library_passes: Vec<panels::library_problem::LibraryPass>,
    /// Test-only: how many times background preparation was advanced (once per logic pass).
    #[cfg(test)]
    pub(crate) prep_ticks: u32,
    /// Test-only: a delay for the sidebar's Missing Photos count worker (a slow disk). Kept here,
    /// not in `ui`, because `ui.set` rebuilds `ui` and would drop it.
    #[cfg(test)]
    pub(crate) missing_count_test_delay: Option<std::time::Duration>,
    /// Offscreen context for headless screenshots of the windowed app.
    shadow: Option<headless::HeadlessView>,
    /// Synthetic input events (from the control channel) injected one step per frame.
    pub synthetic: Vec<egui::Event>,
    /// A gesture was canceled after its synthetic pointer press reached egui. The next
    /// [`Self::raw_input_hook`] cancels that press before egui's next input pass, without a
    /// `PointerMoved` (which can start a drag) or a `PointerButton` release (which can become a
    /// click).
    pointer_cleanup: bool,
    /// Files the control channel (`ui.dropFiles`) holds over the window: every frame reports them
    /// as hovered, as the OS does while a drag is over the window.
    drop_hover: Vec<String>,
    /// Files the control channel dropped: delivered as `dropped_files` on the next frame.
    drop_pending: Vec<String>,
    /// The text the UI last put on the clipboard (`ui.inspect` → `copied`).
    pub copied: Option<String>,
    /// Native file dialogs up for commands (`pick`): each command runs again when its closes.
    pub(crate) pending_picks: Vec<pick::Pending>,
    /// `library.missing` requests from the control channel and the check that answers them on a
    /// worker thread ([`missing_sweep`]).
    pub(crate) missing: missing_sweep::State,
    /// Modifiers announced for synthetic input (held from a button down to its release). This is
    /// only the app's synthetic ownership; the real host state is tracked separately in
    /// `host_mods`, so a canceled synthetic gesture never clears a physically held key.
    synthetic_mods: egui::Modifiers,
    /// Clear `synthetic_mods` on the next frame.
    synthetic_mods_release: bool,
    /// The host's last reported physical modifier state (`ModifiersChanged`), restored after a
    /// synthetic gesture releases instead of unconditionally announcing `NONE`.
    host_mods: egui::Modifiers,
    styled: bool,
    /// The language the installed fonts were built for: the CJK fallback order follows the UI
    /// language's script, so switching language reinstalls them.
    font_language: i18n::Locale,
    /// Browser font loaded separately from WASM so the module stays below its host's size limit.
    chinese_font: Option<std::sync::Arc<egui::FontData>>,
    fonts_ready: bool,
    last_time: f64,
    /// Rect of the photo canvas and the displayed image (screen points) from the last frame.
    pub canvas_rect: Option<egui::Rect>,
    pub image_rect: Option<egui::Rect>,
    /// Scroll offsets (points) of the photo grid (vertical) and the filmstrip (horizontal) as
    /// drawn last (`ui.inspect` → `scroll`).
    pub grid_scroll: Option<f32>,
    pub film_scroll: Option<f32>,
    /// Widget registry from the last completed, nondiscarded frame (automation ids → rects).
    pub widgets: Vec<(String, egui::Rect)>,
    /// Egui hit owners for [`Self::widgets`] from the same completed pass. A press is delivered
    /// only when this id still owns egui's pointer hit (see `control::widget_target_visible`).
    pub widget_hits: Vec<(String, widgets::WidgetHit)>,
    /// Registry collected by the pass being laid out, published only if that pass completes.
    next_widgets: Vec<(String, egui::Rect)>,
    next_widget_hits: Vec<(String, widgets::WidgetHit)>,
    /// In-progress on-canvas gesture (brush stroke points, gradient drag…).
    pub gesture: Option<panels::detail::Gesture>,
    /// Space is held over a tool on the photo: the view's hand has the pointer ([`space_hand`]).
    pub space_hand: Option<space_hand::SpaceHand>,
    /// The brush stroke being painted, as drawn on the photo until it's committed (issue #517).
    pub brush_live: Option<brush_live::LiveStroke>,
    /// What the loupe drew last frame: photo and source ("render", "cached", "embedded", "small",
    /// "thumb", "none").
    pub loupe_shown: Option<(lightcraft_catalog::PhotoId, &'static str)>,
    /// When each photo stepped to in the loupe first showed (`ui.inspect` → `perf.loupeSteps`).
    pub loupe_timing: loupe_timing::LoupeTiming,
    /// The app-wide background preparation settings were handed from the saved app settings to
    /// the session ([`Self::background_prep`]).
    prep_synced: bool,
    /// The loupe's look-ahead: the photo it was made for, the direction of travel (+1 / -1), the
    /// catalog revision and the edge, so it is worked out again only when one of them changes.
    pub(crate) look_ahead: panels::detail::LookAhead,
    /// The window render the loupe asked for last, by job key (see [`region`]); kept for the few
    /// windows whose textures can be on screen, and for the inspector.
    pub region_view: Option<region::RegionView>,
    /// The same for the Before side of a Before/After view.
    pub region_before_view: Option<region::RegionView>,
    /// The loupe's render sizes while a pinch or two-finger scroll runs.
    pub(crate) size_hold: region::SizeHold,
    /// The size of a slider drag's whole-frame drafts (full size where they keep up, issue #330).
    pub(crate) drag_scale: region::DragScale,
    /// (photo, look, window frame size) a window was refused for: it reads more than one render holds.
    pub(crate) window_refused: Option<(lightcraft_catalog::PhotoId, u64, usize)>,
    pub(crate) region_tiles: std::collections::HashMap<(bool, u64), region::RegionView>,
    /// The window render each pane of Compare / Reference asked for last (left, right), as
    /// [`Self::region_view`] is for the loupe.
    pub compare_windows: [Option<region::RegionView>; 2],
    /// Compare / Reference window renders by (pane, job key): which texture shows what.
    pub(crate) compare_tiles: std::collections::HashMap<(u8, u64), region::RegionView>,
    /// Until when (s) a pinch or scroll in Compare / Reference holds the windows there are.
    pub(crate) compare_hold_until: f64,
    /// Photo Merge dialog previews and background merges.
    pub merge: merge::MergeState,
    /// An import in progress (the import review dialog's batches).
    pub import: Option<import::ImportTask>,
    /// A folder scan in progress (feeds the import review).
    pub scan: Option<import::ScanTask>,
    /// Scans stopped without waiting for their workers: each channel is kept until its worker ends, so
    /// the probes it took from the session come back.
    pub(crate) scans_stopping: import::StoppingScans,
    /// Files that waited for a library and wait for the running import or Synchronize Folder too.
    pub waiting_import: panels::library_problem::WaitingImport,
    /// A Synchronize Folder scan in progress (feeds its dialog).
    pub sync: Option<sync::SyncTask>,
    /// `session.folder_changes` came from the Synchronize Folder dialog (and goes with it).
    pub sync_owns_changes: bool,
    /// A Synchronize Folder at work in the background.
    pub sync_run: Option<sync::SyncRun>,
    /// A Lightroom catalog inspect/import in progress.
    pub lightroom: Option<lightroom_import::LightroomTask>,
    /// Last terminal Lightroom result, exposed by the command's status/wait response.
    pub lightroom_last: Option<Value>,
    /// A completed import result waiting for another modal dialog to close.
    pub(crate) lightroom_result: Option<state::Dialog>,
    /// The import review of a finished scan, waiting for another dialog to close.
    pub(crate) review_pending: Option<state::Dialog>,
    /// The library generation the window title and the recent list were last updated for.
    pub shown_library: Option<u64>,
    /// A background export in progress.
    pub export: Option<export_task::ExportTask>,
    /// Background file-system work of other commands (Find Missing Photos, auto import…).
    pub tasks: tasks::Tasks,
    /// The files of the last finished background export (`ui.inspect` → `export.last`).
    pub last_export_result: Option<Value>,
    /// The last slideshow export's end has been told (a toast).
    pub slideshow_export_reported: bool,
    /// The look the loupe shows while the pointer rests on a preset or profile (set by the
    /// panels each frame; nothing is committed, no history entry).
    pub hover_preview: Option<HoverPreview>,
    /// The window is in full screen (as last reported by the host, or as last requested).
    pub window_is_fullscreen: bool,
    /// The GPU preference last applied (`app.gpu`), to apply Settings changes once.
    gpu_applied: Option<bool>,
    /// Say once, when the GPU device has been tried, that no graphics processor was found
    /// ([`panels::notices::gpu_notice`]). The desktop app sets it; headless sessions and tests don't.
    pub gpu_notice: bool,
    /// Tell the user once per session when the drive holding the preview cache runs low
    /// ([`panels::notices::disk_space_notice`]). The desktop app sets it; headless sessions and tests
    /// don't (their drive is whatever the machine has).
    pub disk_space_notice: bool,
    /// The low-disk notice's text while it is queued in [`LightcraftApp::notices`]: it is shown with a
    /// button to Settings ▸ Performance. With the library it was raised for
    /// ([`lightcraft_engine::Session::library_generation`]): another library open drops it.
    pub(crate) disk_notice: Option<(u64, String)>,
    /// The memory budget setting last applied (MB, 0 = automatic).
    memory_applied: Option<u32>,
    /// The hot pixel limit last applied (percent).
    hot_pixels_applied: Option<f32>,
    /// The display profile setting last applied (`app.displayProfile`).
    pub(crate) display_applied: Option<String>,
    /// Why the display profile setting could not be applied (shown in Settings ▸ Display).
    pub display_error: Option<String>,
    /// The screen profile in use while no display profile is chosen (`services.screen_profile`),
    /// and when it was last read (egui time, s).
    pub(crate) screen_profile: Option<Vec<u8>>,
    pub(crate) screen_profile_read: Option<f64>,
    /// The library failed to open at launch: the blocking window, then the temporary-session
    /// banner (issue #100). Cleared once a library opens.
    pub library_problem: Option<panels::library_problem::LibraryProblem>,
    /// First-frame focus/input state for a newly shown blocking library window.
    pub(crate) library_window_input: panels::library_problem::WindowBarrier,
    /// Alt/Option held as the window first appears offers the recent libraries once (issue
    /// #1290); the desktop host arms it when nothing chose the library already.
    pub alt_asks_for_library: bool,
    /// Whether each recent library can be opened, answered by a worker (issue #1290): the launch
    /// list and File ▸ Open Recent Library never look at a folder on the UI thread.
    pub recent_checks: panels::library_problem::RecentChecks,
    /// The activity stack shows every task, not just the first few ("+N more" was clicked).
    pub activity_expanded: bool,
}

/// How long a screenshot waits for in-flight renders before it is captured anyway (and the
/// headless fallback's settle budget).
const SCREENSHOT_SETTLE_MS: f64 = 3000.0;
/// How long a windowed screenshot waits for the compositor before falling back to headless.
const SCREENSHOT_FALLBACK_MS: f64 = 2000.0;
/// Screenshots admitted at once (queued, issued and completed-but-undrained).
const MAX_PENDING_SHOTS: usize = 4;
/// Bytes one screenshot image may use.
const MAX_SHOT_BYTES: usize = 128 * 1024 * 1024;
/// Largest RGBA surface that fits [`MAX_SHOT_BYTES`], checked with the viewport dimensions before
/// layout. The CPU rasterizer's own `softpaint::MAX_IMAGE_PIXELS` may be larger; a snapshot that
/// cannot be captured must be rejected up front rather than after admission.
pub(crate) const MAX_SHOT_PIXELS: usize = MAX_SHOT_BYTES / std::mem::size_of::<egui::Color32>();
/// Bytes all pending/completed screenshots may reserve together.
const MAX_SHOT_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// Slack for PNG framing and deflate overhead while encoding one screenshot. zlib can exceed the
/// raw bytes slightly for incompressible input; this bounds that transient honestly.
const SHOT_ENCODE_SLACK_BYTES: usize = 64 * 1024;
/// Peak working set of capture encoding: every reserved image (≤ [`MAX_SHOT_TOTAL_BYTES`]) plus,
/// for the image being written, its `Rgba8` copy and encoded PNG (each ≤ [`MAX_SHOT_BYTES`] plus
/// [`SHOT_ENCODE_SLACK_BYTES`]). Encoding runs on the UI thread, one image at a time.
const MAX_SHOT_WORKING_SET_BYTES: usize = MAX_SHOT_TOTAL_BYTES + 2 * MAX_SHOT_BYTES + SHOT_ENCODE_SLACK_BYTES;
const _: () = {
    assert!(MAX_SHOT_WORKING_SET_BYTES > MAX_SHOT_TOTAL_BYTES);
    assert!(SHOT_ENCODE_SLACK_BYTES > 0);
    assert!(MAX_SHOT_BYTES.is_multiple_of(std::mem::size_of::<egui::Color32>()));
    assert!(MAX_SHOT_PIXELS * std::mem::size_of::<egui::Color32>() == MAX_SHOT_BYTES);
};
/// Synthetic-input control requests waiting for a fresh layout/delivery at once.
const MAX_PENDING_GESTURES: usize = 32;
/// A control gesture is answered with an error after this long without a completed UI pass and
/// delivery (milliseconds).
/// How long synthetic input may wait without the UI consuming a batch. Each consumed batch
/// restarts this clock, so it is a stall limit, not a cap on a many-step drag's total time.
const GESTURE_TIMEOUT_MS: f64 = 5000.0;
/// Completed UI frames a request waits for before it is issued (a settled, laid-out frame).
const SHOT_SETTLE_FRAMES: u64 = 3;
/// Time a request waits before it is issued, so a control command that triggered a layout change
/// is not captured mid-change.
const SHOT_SETTLE_MS: f64 = 120.0;
/// A settled capture is issued at this instant even while renders are still in flight. The hard
/// admission deadline below still leaves the compositor fallback and the headless settle after it.
const SHOT_FORCE_BUSY_MS: f64 = SCREENSHOT_SETTLE_MS;
/// The texture limit when the host has not reported one.
const MAX_TEXTURE_SIDE: usize = 16_384;

/// The viewport geometry a screenshot was issued for. A completion from an older generation is
/// rejected even if the window has since returned to the same size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShotGeometry {
    /// Viewport size in egui points (the same units as `content_rect`).
    viewport: egui::Vec2,
    /// Egui pixels per point, including UI zoom.
    pixels_per_point: f32,
    /// The UI zoom factor.
    zoom: f32,
    /// [`LightcraftApp::shot_geometry_epoch`] at issue.
    epoch: u64,
}

impl ShotGeometry {
    /// The surface pixels this geometry would produce, before the host's texture limit.
    fn surface_pixels(self) -> Option<(usize, usize)> {
        let w = (self.viewport.x * self.pixels_per_point).round();
        let h = (self.viewport.y * self.pixels_per_point).round();
        if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
            return None;
        }
        Some((w as usize, h as usize))
    }

    /// The surface pixels this geometry can produce, or `None` when it is not a usable surface or
    /// exceeds one side of the host's texture limit.
    fn pixels(self, max_texture_side: usize) -> Option<(usize, usize)> {
        let (w, h) = self.surface_pixels()?;
        (w <= max_texture_side && h <= max_texture_side).then_some((w, h))
    }

    /// Checked `width * height * 4` bytes for this geometry.
    fn bytes(self, max_texture_side: usize) -> Option<usize> {
        let (w, h) = self.pixels(max_texture_side)?;
        w.checked_mul(h)?.checked_mul(std::mem::size_of::<egui::Color32>())
    }

    /// Why [`Self::bytes`] returned `None`: no laid-out surface, or one side over the host's limit.
    /// The two are not the same user action (a driver/GL limit is not "the window has not drawn").
    fn rejection(self, max_texture_side: usize) -> String {
        match self.surface_pixels() {
            None => "ui.screenshot: the window has no usable surface size yet (no frame has been laid out)".to_string(),
            Some((w, h)) => format!("ui.screenshot: the {w}×{h} pixel surface is over the host's {max_texture_side}-pixel per-side texture limit"),
        }
    }
}

/// A synthetic-input control request waiting for a fresh layout and for its last event to be
/// consumed by a completed UI pass.
struct PendingGesture {
    method: String,
    params: Value,
    reply: Sender<ControlResponse>,
    /// [`LightcraftApp::layout_epoch`] at admission; resolution needs a later one.
    admitted_epoch: u64,
    frame: GestureFrame,
    /// Monotonic stall deadline (milliseconds): delivery refreshes it whenever the UI consumes a
    /// batch, so a long but progressing drag is not a timeout.
    deadline_ms: f64,
    /// [`LightcraftApp::ui_passes`] when the events were queued (`None`: not resolved yet).
    resolved_pass: Option<u64>,
    /// Synthetic events queued for this gesture that the host has not injected yet.
    remaining_events: usize,
    /// At least one batch of this gesture's events has reached a pass. Later batches may see the
    /// gesture's own effect (a first click selects, the second opens), so the semantic frame
    /// snapshot is only checked before the first one.
    started: bool,
    /// For a widget gesture, the id and point resolved against the completed layout. Delivery
    /// rechecks the widget's fresh rect before the first event, before each press and (for a click)
    /// before its release.
    target: Option<control::GestureTarget>,
    /// A pointer button this gesture has pressed but not yet released. Cancellation resets the
    /// pointer state before egui's next input pass, so a refused click neither leaves the pointer
    /// down nor turns the cleanup into a click or drag.
    pressed: Option<InjectedPress>,
    result: Value,
}

/// A `ui.screenshot` request in progress.
struct PendingShot {
    token: u64,
    /// Destination identity pinned at admission (revalidated immediately before the write).
    target: Option<lightcraft_engine::originals::WriteTarget>,
    reply: Sender<ControlResponse>,
    headless: bool,
    /// Admitted UI pass count and monotonic deadlines (milliseconds).
    admitted_passes: u64,
    not_before_ms: f64,
    /// At this instant a busy renderer no longer holds the capture back.
    force_ms: f64,
    deadline_ms: f64,
    /// When the viewport screenshot command went out.
    sent_at_ms: Option<f64>,
    /// Bytes this request reserves until it is drained or cancelled.
    reserved: usize,
    /// The one-shot completion slot; only the app owns the strong reference.
    slot: Option<craft_ui::capture::CaptureSlot>,
    /// The geometry and document generation the issued command was bound to.
    issued: Option<ShotGeometry>,
    /// Library generation at issue (a different library changed mid-capture).
    issued_library: u64,
}

/// Whether the settings render in black & white.
/// A temporary look for the loupe (hovering a preset or profile).
#[derive(Clone, Debug, PartialEq)]
pub struct HoverPreview {
    /// What is previewed (e.g. "Preset: Warm Glow").
    pub label: String,
    /// The photo's settings with the look applied.
    pub settings: lightcraft_develop::DevelopSettings,
}

pub fn is_bw(d: &lightcraft_develop::DevelopSettings) -> bool {
    d.treatment == lightcraft_develop::Treatment::Bw || d.profile.id == "lc.mono" || d.profile.id.starts_with("lc.bw.")
}

/// A dropped face model file (`.onnx`).
pub fn is_model_file(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx"))
}

/// Files dropped on the window that are presets rather than photos.
pub fn is_preset_file(path: &str) -> bool {
    let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    ["lcpreset", "lrtemplate", "xmp", "zip", "cube", "dcp", "lmp", "mplumpack", "lceffect"].contains(&ext.as_str())
}

/// Splits dropped files into presets and everything else. An `.xmp` dropped next to a photo with the same
/// name (`IMG_1.xmp` or `IMG_1.CR2.xmp` beside `IMG_1.CR2`) is that photo's sidecar, not a preset, and is left out.
pub fn split_presets(dropped: Vec<String>) -> (Vec<String>, Vec<String>) {
    let dir_of = |p: &str| std::path::Path::new(p).parent().map(|d| d.to_string_lossy().to_lowercase()).unwrap_or_default();
    let stem_of = |p: &str| std::path::Path::new(p).file_stem().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name_of = |p: &str| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let is_xmp = |p: &str| std::path::Path::new(p).extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
    let photo_names: Vec<(String, String)> =
        dropped.iter().filter(|p| !is_preset_file(p)).flat_map(|p| [(dir_of(p), stem_of(p)), (dir_of(p), name_of(p))]).collect();
    let (mut presets, mut photos) = (Vec::new(), Vec::new());
    for p in dropped {
        if !is_preset_file(&p) {
            photos.push(p);
        } else if is_xmp(&p) && photo_names.contains(&(dir_of(&p), stem_of(&p))) {
            // a photo's sidecar travels with the photo
        } else {
            presets.push(p);
        }
    }
    (presets, photos)
}

/// Results recomputed only when the catalog (or their inputs) change.
#[derive(Default)]
pub struct Caches {
    keyword_tree: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::KeywordNode>>)>,
    /// The Keywording box's read-only names (with containing keywords, or Will Export), by library
    /// revision, selection and view.
    keyword_names: Option<(u64, u64, std::sync::Arc<Vec<panels::keywording::Chip>>)>,
    /// The Keywording box's chips, by library revision and selection.
    keyword_chips: Option<(u64, u64, std::sync::Arc<Vec<panels::keywording::Chip>>)>,
    /// How many photos of the library the selection is, by library revision and selection.
    selected_photos: Option<(u64, u64, usize)>,
    /// The Keywording box's chips grouped by the keyword they are inside, by library revision and
    /// selection.
    keyword_groups: Option<(u64, u64, std::sync::Arc<Vec<panels::keywording::Group>>)>,
    /// The Keyword List's tick boxes, by library revision and selection.
    keyword_ticks: Option<(u64, u64, std::sync::Arc<panels::keyword_list::Ticks>)>,
    folder_tree: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::FolderNode>>)>,
    people: Option<(u64, lightcraft_catalog::Filter, std::sync::Arc<Vec<lightcraft_catalog::Person>>)>,
    suggestions: Option<(u64, std::sync::Arc<Vec<String>>)>,
    /// What the Add keyword box offers for the text typed in it, by library revision, the
    /// selection's keywords and that text.
    completions: Option<(u64, std::sync::Arc<panels::keywording::Offers>)>,
    counts: Option<(u64, LibraryCounts)>,
    date_groups: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::DateGroup>>)>,
    filter_values: Option<(u64, std::sync::Arc<FilterValues>)>,
    /// The albums' counts: the minute they are of (while a smart album asks about "the last N days"), the tally
    /// that follows edits, and the counts as the panels take them.
    album_counts:
        Option<(String, lightcraft_catalog::tally::AlbumTally, std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>>)>,
    /// How often the album counts were made from every photo (tests check that unchanged frames, and edits to a
    /// photo or a few, don't).
    pub album_count_scans: usize,
    /// What the open smart-album rule dialog shows besides the rules ([`Caches::rules_view`]).
    rules_view: Option<(u64, std::sync::Arc<RulesView>)>,
    pub rules_view_scans: usize,
    /// A count an open dialog shows ([`Caches::dialog_count`]), by what it counts.
    dialog_count: Option<(u64, usize)>,
    pub dialog_count_scans: usize,
    /// The problems of every saved smart album that has some ([`Caches::smart_album_problems`]).
    smart_problems: Option<(u64, std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, Vec<lightcraft_catalog::rules::Problem>>>)>,
    pub smart_problem_scans: usize,
    /// AI denoise: what the pump last saw, the model list and the downloads being watched.
    pub denoise: panels::denoise::Ui,
    /// Bumped when a face model is installed, removed or chosen, so Settings re-reads the list at once.
    pub faces_epoch: u64,
    /// Face model downloads the user started, followed until installed (see `panels::faces::pump`).
    pub faces_dl_watch: Vec<String>,
    /// Whether the background face indexer is running, how many faces it has embedded, how many photos are left, and
    /// when to ask it again.
    pub faces_active: bool,
    pub faces_indexed: u64,
    pub faces_pending: u64,
    pub faces_next_pump: f64,
    /// When to ask the focus-point reader again (`focus_scan::pump`).
    pub focus_next_pump: f64,
    /// When the user last dragged, typed or scrolled, and when they last moved the pointer (egui time).
    pub last_input: f64,
    /// The pace the face scan was last given, and whether the window was in front then (for `ui.inspect`).
    pub faces_pace: &'static str,
    pub faces_in_front: bool,
    pub last_move: f64,
    /// Name suggestions for the photo in the loupe: (photo, catalog revision, faces indexed, suggestions by region).
    pub face_hints: Option<(u64, u64, u64, std::sync::Arc<panels::faces::Hints>)>,
    /// The open person page: (name, catalog revision, faces indexed, when it was asked for, the page).
    pub person_page: Option<(String, u64, u64, f64, std::sync::Arc<panels::person::PersonPage>)>,
    /// The unnamed faces: (catalog revision, faces indexed, when it was asked for, the list).
    pub unnamed: Option<(u64, u64, f64, std::sync::Arc<panels::unnamed::Unnamed>)>,
    /// The most photos the face scan has had left at once since it last finished (the progress bar's whole).
    pub faces_peak: u64,
    person_names: Option<(u64, std::sync::Arc<Vec<String>>)>,
    /// The grid's date runs, layout and indexes (by the visible list's generation).
    pub grid: panels::grid::GridCache,
    /// What the grid did on its frames (benchmarks and tests check unchanged frames stay cheap).
    pub grid_stats: panels::grid::GridStats,
}

/// The left panel's counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryCounts {
    pub total: usize,
    pub picks: usize,
    pub deleted: usize,
}

/// The values the filter bar's pickers offer.
#[derive(Clone, Debug, Default)]
pub struct FilterValues {
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub keywords: Vec<String>,
}

/// What the smart-album rule dialog shows besides the rules themselves: their problems (each row
/// marks its own), how many photos they match and the albums an Album rule picks from.
pub struct RulesView {
    pub problems: Vec<lightcraft_catalog::rules::Problem>,
    pub count: usize,
    pub albums: Vec<album_picker::AlbumEntry>,
}

pub(crate) fn key_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(parts)
}

/// Whether a tally of the library made when the catalog was at `revision` is still right, given that it reads only
/// `reads` of a photo: nothing changed since, or only other things of some photos did (a rating changes no folder's
/// count, a slider changes none at all). Each of these tallies asks every photo, so one kept is a pass over the
/// library saved on every edit.
fn still_right(cat: &lightcraft_catalog::Catalog, revision: u64, reads: lightcraft_catalog::Aspects) -> bool {
    revision == cat.revision || cat.aspects_since(revision).is_some_and(|changed| !changed.any_of(reads))
}

/// What the tallies below read of a photo, besides whether it is in the library.
const READS_FILE: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::FILE);
const READS_META: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::META);
const READS_PEOPLE: lightcraft_catalog::Aspects = READS_META.with(lightcraft_catalog::Aspects::ANALYSIS);
const READS_CAPTURED: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::CAPTURED);
const READS_FLAG: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::FLAG);

impl Caches {
    /// The folders the library's photos were imported from.
    pub fn folder_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::FolderNode>> {
        let kept = self.folder_tree.take().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, t)| t);
        let t = kept.unwrap_or_else(|| std::sync::Arc::new(cat.folder_tree()));
        self.folder_tree = Some((cat.revision, t.clone()));
        t
    }
    /// The folder tree as last built, when it still holds (a read-only check cannot rebuild it).
    pub(crate) fn folder_tree_if_fresh(&self, cat: &lightcraft_catalog::Catalog) -> Option<std::sync::Arc<Vec<lightcraft_catalog::FolderNode>>> {
        self.folder_tree.as_ref().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, t)| t.clone())
    }
    /// The library's keyword tree.
    pub fn keyword_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::KeywordNode>> {
        let kept = self.keyword_tree.take().filter(|(r, _)| still_right(cat, *r, READS_META)).map(|(_, t)| t);
        let t = kept.unwrap_or_else(|| std::sync::Arc::new(cat.keyword_tree()));
        self.keyword_tree = Some((cat.revision, t.clone()));
        t
    }
    /// The Keywording box's read-only names for the selection: what exported files carry
    /// (`export`), or the keywords with those containing them.
    pub(crate) fn keyword_names(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
        export: bool,
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of((selection, export));
        match &self.keyword_names {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(if export {
                    panels::keywording::will_export(cat, selection)
                } else {
                    panels::keywording::with_containing(cat, selection)
                });
                self.keyword_names = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// The selection's keywords (the Keywording box's chips).
    pub(crate) fn keyword_chips(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of(selection);
        match &self.keyword_chips {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(panels::keywording::chips(cat, selection));
                self.keyword_chips = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// How many photos the selection is: each once, those the library has (what the Keywording
    /// box's "N of M" counts).
    pub(crate) fn selected_photos(&mut self, cat: &lightcraft_catalog::Catalog, selection: &[lightcraft_catalog::PhotoId]) -> usize {
        let key = key_of(selection);
        match self.selected_photos {
            Some((r, k, n)) if r == cat.revision && k == key => n,
            _ => {
                let n = panels::keywording::distinct(cat, selection).len();
                self.selected_photos = Some((cat.revision, key, n));
                n
            }
        }
    }
    /// The selection's keywords grouped by the keyword they are inside (the grouped Keywording box).
    pub(crate) fn keyword_groups(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<Vec<panels::keywording::Group>> {
        let key = key_of(selection);
        match &self.keyword_groups {
            Some((r, k, g)) if *r == cat.revision && *k == key => g.clone(),
            _ => {
                let chips = self.keyword_chips(cat, selection);
                let g = std::sync::Arc::new(panels::keywording::grouped(&chips));
                self.keyword_groups = Some((cat.revision, key, g.clone()));
                g
            }
        }
    }
    /// How many selected photos have each keyword (the Keyword List's tick boxes).
    pub(crate) fn keyword_ticks(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<panels::keyword_list::Ticks> {
        let key = key_of(selection);
        match &self.keyword_ticks {
            Some((r, k, t)) if *r == cat.revision && *k == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(panels::keyword_list::Ticks::of(cat, selection));
                self.keyword_ticks = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// The people named on faces among the photos the filter lets through (its own `person` aside),
    /// with photo counts.
    pub fn people(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        filter: &lightcraft_catalog::Filter,
    ) -> std::sync::Arc<Vec<lightcraft_catalog::Person>> {
        let key = lightcraft_catalog::Filter { person: None, ..filter.clone() };
        match &self.people {
            Some((r, f, t)) if *r == cat.revision && *f == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.people_in(&key));
                self.people = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// All Photos / Picks / Recently Deleted counts.
    pub fn counts(&mut self, cat: &lightcraft_catalog::Catalog) -> LibraryCounts {
        if let Some((r, c)) = self.counts
            && still_right(cat, r, READS_FLAG)
        {
            self.counts = Some((cat.revision, c));
            return c;
        }
        let mut c = LibraryCounts::default();
        for p in cat.photos() {
            if p.in_library() {
                c.total += 1;
                if p.flag == lightcraft_catalog::Flag::Pick {
                    c.picks += 1;
                }
            } else if p.deleted && !p.local {
                c.deleted += 1;
            }
        }
        self.counts = Some((cat.revision, c));
        c
    }
    /// Everyone named on a face in the library (for completing a name as it is typed).
    pub fn person_names(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<String>> {
        let kept = self.person_names.take().filter(|(r, _)| still_right(cat, *r, READS_PEOPLE)).map(|(_, v)| v);
        let v = kept.unwrap_or_else(|| std::sync::Arc::new(cat.people().into_iter().map(|p| p.name).collect::<Vec<_>>()));
        self.person_names = Some((cat.revision, v.clone()));
        v
    }
    /// The By Date tree.
    pub fn date_groups(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::DateGroup>> {
        let kept = self.date_groups.take().filter(|(r, _)| still_right(cat, *r, READS_CAPTURED)).map(|(_, g)| g);
        let g = kept.unwrap_or_else(|| std::sync::Arc::new(cat.date_groups()));
        self.date_groups = Some((cat.revision, g.clone()));
        g
    }
    /// Cameras, lenses and keywords in the library (filter bar pickers).
    pub fn filter_values(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<FilterValues> {
        match &self.filter_values {
            Some((r, v)) if *r == cat.revision => v.clone(),
            _ => {
                let distinct = |mut v: Vec<String>| {
                    v.retain(|s| !s.trim().is_empty());
                    v.sort_by_key(|s| s.to_lowercase());
                    v.dedup();
                    v
                };
                let lib = || cat.photos().filter(|p| p.in_library());
                let v = std::sync::Arc::new(FilterValues {
                    cameras: distinct(lib().map(|p| p.meta.camera.clone()).collect()),
                    lenses: distinct(lib().map(|p| p.meta.lens.clone()).collect()),
                    keywords: cat.keywords().into_iter().map(|(k, _)| k).collect(),
                });
                self.filter_values = Some((cat.revision, v.clone()));
                v
            }
        }
    }
    /// What the Add keyword box offers for `typed` (see [`panels::keywording::offers`]). Kept apart
    /// from [`Self::suggestions`]: both are asked for on every frame of typing, and each looks
    /// through the library's keywords.
    pub(crate) fn completions(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        current: &[String],
        typed: &str,
    ) -> std::sync::Arc<panels::keywording::Offers> {
        let k = key_of((cat.revision, current, typed));
        match &self.completions {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                let v = std::sync::Arc::new(panels::keywording::offers(cat, current, typed));
                self.completions = Some((k, v.clone()));
                v
            }
        }
    }
    /// Keyword suggestions for a photo with `current` keywords and the typed `prefix`.
    pub fn suggestions(&mut self, cat: &lightcraft_catalog::Catalog, current: &[String], prefix: &str, n: usize) -> std::sync::Arc<Vec<String>> {
        let k = key_of((cat.revision, current, prefix, n));
        match &self.suggestions {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                let v = std::sync::Arc::new(cat.keyword_suggestions(current, prefix, n));
                self.suggestions = Some((k, v.clone()));
                v
            }
        }
    }
    /// Every album's photo count for the sidebar (a smart album evaluates its rules, which
    /// scans the catalog). Recomputed when the catalog changes (photos, metadata, album rules
    /// all bump its revision) and — only while some smart album has an "in the last…" rule —
    /// when `now` (the session clock, ISO) enters a new minute, so such counts follow the clock
    /// within a minute without rescanning every frame.
    /// [`RulesView`] for `rules` edited in smart album `editing` (`None`: a new album), within
    /// `folders` when it was made from a folder view (a photo of any of them counts): worked out again only when the catalog, the
    /// rules, the language or (for "in the last…" rules) the minute changes, not every frame.
    pub fn rules_view(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        rules: &lightcraft_catalog::RuleSet,
        editing: Option<u64>,
        folders: Vec<String>,
        now: &str,
    ) -> std::sync::Arc<RulesView> {
        let relative = rules.depends_on_now();
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let k = key_of((cat.revision, serde_json::to_string(rules).unwrap_or_default(), editing, &folders, minute, i18n::language().code()));
        if let Some((key, v)) = &self.rules_view
            && *key == k
        {
            return v.clone();
        }
        if relative {
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let problems = rules.check_for(cat, editing.map(lightcraft_catalog::AlbumId));
        let filter = lightcraft_catalog::Filter { rule_set: Some(rules.clone()), library_folders: folders, ..Default::default() };
        let count = if problems.is_empty() { cat.query(&filter, &Default::default()).len() } else { 0 };
        let v = std::sync::Arc::new(RulesView { problems, count, albums: panels::dialogs::album_entries(cat, editing) });
        self.rules_view_scans += 1;
        self.rules_view = Some((k, v.clone()));
        v
    }

    /// A count an open dialog shows (photos a filter matches, photos with a keyword…): `count` runs
    /// again only when the catalog or `what` (what is counted) changes, not every frame.
    pub(crate) fn dialog_count(&mut self, cat: &lightcraft_catalog::Catalog, what: impl std::hash::Hash, count: impl FnOnce() -> usize) -> usize {
        let k = key_of((cat.revision, what));
        if let Some((key, n)) = self.dialog_count
            && key == k
        {
            return n;
        }
        let n = count();
        self.dialog_count_scans += 1;
        self.dialog_count = Some((k, n));
        n
    }

    /// The problems of every saved smart album that has some (`Catalog::smart_album_problems`),
    /// for the sidebar's ⚠ marks: worked out again only when the catalog changes.
    pub fn smart_album_problems(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, Vec<lightcraft_catalog::rules::Problem>>> {
        if let Some((rev, v)) = &self.smart_problems
            && *rev == cat.revision
        {
            return v.clone();
        }
        let v: std::sync::Arc<std::collections::HashMap<_, _>> = std::sync::Arc::new(
            cat.albums().filter(|a| a.is_smart()).map(|a| (a.id, cat.smart_album_problems(a.id))).filter(|(_, p)| !p.is_empty()).collect(),
        );
        self.smart_problem_scans += 1;
        self.smart_problems = Some((cat.revision, v.clone()));
        v
    }

    pub fn album_counts(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        now: &str,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>> {
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        // the counts of this minute, if there are any: an edit to a photo or a few is followed from them, a smart
        // album asking only those photos again; anything more, or a new minute, is counted from every photo
        let kept = self.album_counts.take().filter(|(of, _, _)| of == minute);
        if let Some((_, tally, shown)) = &kept
            && tally.revision() == cat.revision
        {
            let shown = shown.clone();
            self.album_counts = kept;
            return shown;
        }
        if relative {
            // "in the last N days" counts back from the session clock, as in the grid. (Within a minute an edit asks
            // only the photos it touched, at this second; the others were asked earlier in the minute. That is as
            // far out as the counts already were between edits, and the next minute counts them all again.)
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let tally = match kept {
            Some((_, mut tally, _)) => {
                if !tally.update(cat) {
                    self.album_count_scans += 1;
                }
                tally
            }
            None => {
                self.album_count_scans += 1;
                lightcraft_catalog::tally::AlbumTally::of(cat)
            }
        };
        let shown = std::sync::Arc::new(tally.counts().clone());
        self.album_counts = Some((minute.to_string(), tally, shown.clone()));
        shown
    }
}

#[cfg(test)]
mod cache_tests {
    use lightcraft_catalog::{Album, AlbumId, Catalog, Filter, Op, Photo, PhotoId, RuleSet, Source};

    fn photo(id: u64, captured: &str, rating: u8) -> Op {
        let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("p{id}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.captured = Some(captured.into());
        p.rating = rating;
        Op::AddPhoto { photo: Box::new(p) }
    }

    fn smart(id: u64, rules: serde_json::Value) -> Op {
        let mut a = Album::new(AlbumId(id), format!("smart {id}"));
        let rules: RuleSet = serde_json::from_value(rules).unwrap();
        a.smart = Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() }));
        Op::AddAlbum { album: a }
    }

    /// Smart-album counts: unchanged frames reuse them; metadata and rule changes and (for
    /// "in the last…" rules) the clock crossing into a new minute recompute them.
    #[test]
    fn smart_album_counts_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for (i, d) in ["2026-09-30T11:59:30", "2026-09-29T08:00:00", "2026-01-01T08:00:00"].iter().enumerate() {
            cat.apply(photo(i as u64 + 1, d, if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        cat.apply(smart(11, serde_json::json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}))).unwrap();
        let now = "2026-09-30T12:00:00";
        let n = c.album_counts(&cat, now);
        assert_eq!((n[&AlbumId(10)], n[&AlbumId(11)]), (1, 1));
        assert_eq!(c.album_count_scans, 1);
        for _ in 0..10 {
            c.album_counts(&cat, "2026-09-30T12:00:40");
        }
        assert_eq!(c.album_count_scans, 1, "unchanged frames in the same minute don't rescan");
        // a photo rated: followed from that photo, not counted from every one
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 2);
        assert_eq!(c.album_count_scans, 1);
        // rule change
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 5}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 1);
        // an hour later the 11:59:30 photo has left "in the last hour", with no catalog change
        assert_eq!(c.album_counts(&cat, "2026-09-30T13:00:10")[&AlbumId(11)], 0);
        assert_eq!(c.album_count_scans, 3, "new rules and a new minute are counted from every photo; the rating was not");
        lightcraft_catalog::rules::set_now(None);
    }

    /// The rule dialog's problems, live count and album list are worked out once per change to the
    /// catalog or the rules, not every frame; so are the sidebar's smart-album problems.
    #[test]
    fn rule_dialog_and_problems_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        let rules =
            |v: u64| -> RuleSet { serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": v}]})).unwrap() };
        let now = "2026-09-30T12:00:00";
        let view = c.rules_view(&cat, &rules(3), None, Vec::new(), now);
        assert_eq!((view.count, view.problems.len()), (1, 0));
        for _ in 0..10 {
            c.rules_view(&cat, &rules(3), None, Vec::new(), now);
        }
        assert_eq!(c.rules_view_scans, 1, "unchanged frames reuse it");
        assert_eq!(c.rules_view(&cat, &rules(1), None, Vec::new(), now).count, 3, "a rule change");
        assert_eq!(c.rules_view(&cat, &rules(9), None, Vec::new(), now).problems.len(), 1);
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.rules_view(&cat, &rules(3), None, Vec::new(), now).count, 2, "a catalog change");
        assert_eq!(c.rules_view_scans, 4);
        // the sidebar's problems
        cat.apply(Op::AddAlbum { album: lightcraft_catalog::Album::new(AlbumId(20), "Trip") }).unwrap();
        cat.apply(smart(21, serde_json::json!({"rules": [{"field": "album", "op": "is", "value": 20}]}))).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_none_or(Vec::is_empty));
        for _ in 0..10 {
            c.smart_album_problems(&cat);
        }
        assert_eq!(c.smart_problem_scans, 1);
        cat.apply(Op::RemoveAlbum { id: AlbumId(20) }).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_some_and(|p| !p.is_empty()), "its album is gone");
        assert_eq!(c.smart_problem_scans, 2);
    }

    /// Without "in the last…" rules the clock never causes a rescan.
    #[test]
    fn absolute_rules_ignore_the_clock() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        cat.apply(photo(1, "2026-09-30T11:59:30", 5)).unwrap();
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        c.album_counts(&cat, "2026-09-30T12:00:00");
        c.album_counts(&cat, "2027-01-01T00:00:00");
        assert_eq!(c.album_count_scans, 1);
    }
}
