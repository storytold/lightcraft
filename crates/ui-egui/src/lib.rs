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
pub mod dialogs;
pub mod docking;
mod drop_overlay;
pub mod export_task;
mod focus_scan;
mod frame_rate;
pub mod headless;
pub mod i18n;
pub mod icons;
pub mod lightroom_import;
pub mod links;
pub mod menus;
mod model_setup;
pub mod panels;
pub mod pick;
pub mod recheck;
pub mod region;
pub mod reload;
pub mod render;
pub mod shortcuts;
pub mod sidebar;
pub mod softpaint;
pub mod state;
pub mod sync;
pub mod tasks;
pub mod theme;
pub mod widgets;
pub mod window_place;

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
    pub(crate) model_setup: model_setup::Pending,
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
    /// The brush stroke being painted, as drawn on the photo until it's committed (issue #517).
    pub brush_live: Option<brush_live::LiveStroke>,
    /// What the loupe drew last frame: photo and source ("render", "cached", "embedded", "small",
    /// "thumb", "none").
    pub loupe_shown: Option<(lightcraft_catalog::PhotoId, &'static str)>,
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
    /// The memory budget setting last applied (MB, 0 = automatic).
    memory_applied: Option<u32>,
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
    ["lcpreset", "lrtemplate", "xmp", "zip", "cube", "lmp", "mplumpack", "lceffect"].contains(&ext.as_str())
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
