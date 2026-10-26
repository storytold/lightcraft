//! LightCraft's egui frontend: a Lightroom-style UI over `lightcraft-engine`.
//!
//! The UI is thin: every action goes through [`LightcraftApp::run`], which handles UI commands
//! (views, panels, zoom — see [`menus::ui_commands`]) and forwards everything else to the engine.
//! The same entry point serves menus, shortcuts, buttons and the control channel ([`control`]).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod album_picker;
pub mod brush_live;
pub mod button;
pub mod canvas_keys;
mod command_error;
pub mod control;
pub mod credits;
pub mod date_picker;
pub mod docking;
mod drop_overlay;
pub mod export_task;
mod focus_scan;
mod frame_rate;
pub mod headless;
pub mod i18n;
pub mod icons;
pub mod import;
pub mod lightroom_destination;
pub mod lightroom_import;
pub mod links;
pub mod menu_level;
pub mod menubar;
pub mod menus;
pub mod merge;
mod model_setup;
pub mod panels;
pub mod pick;
pub mod print_view;
pub mod progress;
pub mod rating;
pub mod recheck;
pub mod region;
pub mod reload;
pub mod render;
pub mod shortcuts;
pub mod sidebar;
pub mod slideshow_export;
pub mod softpaint;
pub mod state;
pub mod sync;
pub mod tasks;
pub mod text_field;
pub mod theme;
pub mod updates_ui;
pub mod widgets;
pub mod window_place;

#[cfg(test)]
mod tests_activity;
#[cfg(test)]
mod tests_album_picker;
#[cfg(test)]
mod tests_album_remove;
#[cfg(test)]
mod tests_appearance;
#[cfg(test)]
mod tests_before_after;
#[cfg(test)]
mod tests_browse_active;
#[cfg(test)]
mod tests_button;
#[cfg(test)]
mod tests_canvas_keys;
#[cfg(test)]
mod tests_canvas_pan;
#[cfg(test)]
mod tests_capture_time;
#[cfg(test)]
mod tests_compare;
#[cfg(test)]
mod tests_copy_paste_dialogs;
#[cfg(test)]
mod tests_crop_rotate;
#[cfg(test)]
mod tests_crop_tool;
#[cfg(test)]
mod tests_curve;
#[cfg(test)]
mod tests_date_picker;
#[cfg(test)]
mod tests_dialog_fit;
#[cfg(test)]
mod tests_dialog_primary;
#[cfg(test)]
mod tests_dialog_settings;
#[cfg(test)]
mod tests_dialog_tab;
#[cfg(test)]
mod tests_dialogs;
#[cfg(test)]
mod tests_drop_import;
#[cfg(test)]
mod tests_edit_layout;
#[cfg(test)]
mod tests_edit_panel;
#[cfg(test)]
mod tests_empty_library;
#[cfg(test)]
mod tests_error_kinds;
#[cfg(test)]
mod tests_filmstrip;
#[cfg(test)]
mod tests_focus_point;
#[cfg(test)]
mod tests_folder_drop;
#[cfg(test)]
mod tests_grading;
#[cfg(test)]
mod tests_grid;
#[cfg(test)]
mod tests_guided_guides;
#[cfg(test)]
mod tests_histogram_drag;
#[cfg(test)]
mod tests_info_dialogs;
#[cfg(test)]
mod tests_keymap;
#[cfg(test)]
mod tests_keyword_files;
#[cfg(test)]
mod tests_keyword_list;
#[cfg(test)]
mod tests_keyword_set;
#[cfg(test)]
mod tests_keyword_sidebar;
#[cfg(test)]
mod tests_keywording;
#[cfg(test)]
mod tests_labels;
#[cfg(test)]
mod tests_launch_library;
#[cfg(test)]
mod tests_layout_image;
#[cfg(test)]
mod tests_library_problem;
#[cfg(test)]
mod tests_lifecycle_037;
#[cfg(test)]
mod tests_lightroom_destination;
#[cfg(test)]
mod tests_lights_out;
#[cfg(test)]
mod tests_mask_effects;
#[cfg(test)]
mod tests_masking;
#[cfg(test)]
mod tests_masking_layout;
#[cfg(test)]
mod tests_menubar;
#[cfg(test)]
mod tests_merge_preview;
#[cfg(test)]
mod tests_offline;
#[cfg(test)]
mod tests_open_lightroom;
#[cfg(test)]
mod tests_panels;
#[cfg(test)]
mod tests_preset_favorites;
#[cfg(test)]
mod tests_preset_import;
#[cfg(test)]
mod tests_preview_limit;
#[cfg(test)]
mod tests_print;
#[cfg(test)]
mod tests_profile_menu;
#[cfg(test)]
mod tests_profiles_layout;
#[cfg(test)]
mod tests_progress;
#[cfg(test)]
mod tests_quit_unsaved;
#[cfg(test)]
mod tests_rating;
#[cfg(test)]
mod tests_recheck;
#[cfg(test)]
mod tests_rename_person;
#[cfg(test)]
mod tests_rules_editor;
#[cfg(test)]
mod tests_sam_setup;
#[cfg(test)]
mod tests_scroll;
#[cfg(test)]
mod tests_slider_reset;
#[cfg(test)]
mod tests_slideshow;
#[cfg(test)]
mod tests_solo_sections;
#[cfg(test)]
mod tests_switch_library;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_tallies;
#[cfg(test)]
mod tests_temp_slider;
#[cfg(test)]
mod tests_titlebar;
#[cfg(test)]
mod tests_tool_overlay;
#[cfg(test)]
mod tests_unsaved;
#[cfg(test)]
mod tests_view_cost;
#[cfg(test)]
mod tests_wb_selector;
#[cfg(test)]
mod tests_zoom;
#[cfg(test)]
mod tests_zoom_keys;

use std::sync::mpsc::{Receiver, Sender};

use lightcraft_engine::{AiModel, ErrorKind, Session};
use serde_json::Value;

pub use command_error::CommandError;
pub use control::{ControlRequest, ControlResponse};
pub use state::UiState;

const TOAST_SECONDS: f64 = 1.4;
/// The `path` of a display profile read from the screen rather than chosen as a file.
pub const SCREEN_PROFILE: &str = "screen";

pub type PickFiles = Box<dyn FnMut() -> Vec<String>>;
/// A save dialog: suggested file name → chosen path (`None` = cancelled).
pub type SaveFile = Box<dyn FnMut(&str) -> Option<String>>;
pub type WriteFn = Box<dyn FnMut(&str, &[u8]) -> Result<(), String>>;
/// A writer other threads can use (background export).
pub type SharedWrite = std::sync::Arc<dyn Fn(&str, &[u8]) -> Result<(), String> + Send + Sync>;
pub type PngEncode = Box<dyn Fn(&lightcraft_raster::Rgba8) -> Vec<u8>>;
/// A folder chooser (`None` = cancelled).
pub type PickFolder = Box<dyn FnMut() -> Option<String>>;
/// Reveal a file in the system file manager (Finder / Explorer / the folder on Linux).
pub type RevealFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Open a URL in the user's browser.
pub type OpenUrlFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Open a file in an application (`app` = "" for the system's default one).
pub type OpenWithFn = Box<dyn FnMut(&str, &str) -> Result<(), String>>;
/// Open several files with one start of an application (`app` as for [`OpenWithFn`]).
pub type OpenAllWithFn = Box<dyn FnMut(&[String], &str) -> Result<(), String>>;
/// Linux model-folder work: wait for the default opener on a background task.
#[cfg(target_os = "linux")]
pub type OpenModelFolderFn = std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
/// The ICC profile of the screen showing the window, as the system colour-manages it (`None`:
/// not known).
pub type ScreenProfileFn = Box<dyn FnMut() -> Option<Vec<u8>>>;
/// Whether an external editor can be started (its name as in [`Services::open_with`]); the
/// error says why not and what to do (e.g. PhotoCraft isn't installed).
pub type CheckEditorFn = Box<dyn Fn(&str) -> Result<(), String>>;
/// Something the host does with the session (web: back up or restore the library in browser
/// storage); the work may finish asynchronously.
pub type HostAction = Box<dyn FnMut(&mut Session) -> Result<Value, String>>;

/// The system's dark/light appearance where egui doesn't report one (Linux desktops whose
/// windowing layer says nothing, e.g. many Wayland compositors). The host answers from what it
/// last heard and wakes the UI itself when that changes; it is never polled for changes.
pub type SystemThemeFn = Box<dyn Fn(&egui::Context) -> Option<egui::Theme>>;

/// Platform services injected by the host app (desktop or web).
#[derive(Default)]
pub struct Services {
    /// Native file dialogs for commands, shown off the UI thread and answered later
    /// (`pick`; #191). With it, the synchronous pickers below serve only the buttons inside
    /// dialogs; without it (the web, tests) they answer the commands too.
    pub picker: Option<pick::Picker>,
    /// Show an open dialog for photos; returns paths.
    pub pick_files: Option<PickFiles>,
    /// Local ONNX model; a manifest must accompany it.
    pub pick_denoise_model: Option<PickFiles>,
    /// Open dialog for a face model (`.onnx`; Settings ▸ Faces ▸ Add a model file…).
    pub pick_model_file: Option<PickFiles>,
    /// Open dialog for preset files (`.lcpreset`, `.xmp`, `.lrtemplate`, `.zip`, `.dng`, Luminar `.lmp` / `.mplumpack`).
    pub pick_preset_files: Option<PickFiles>,
    /// Open dialog for a GPS track log (`.gpx`; Photo ▸ Auto-Tag from Tracklog…).
    pub pick_tracklog: Option<PickFiles>,
    /// Open dialog for a monitor ICC profile (`.icc` / `.icm`; Settings ▸ Display; desktop only).
    pub pick_display_profile: Option<PickFiles>,
    /// The profile of the screen showing the window, used while none is chosen in Settings ▸
    /// Display (macOS, whose windows aren't colour-managed: issue #787).
    pub screen_profile: Option<ScreenProfileFn>,
    /// Save dialog for an exported `.lcpreset` file.
    pub save_preset_file: Option<SaveFile>,
    /// Open dialog for a keyword list file (`.txt`, Photo Supreme's `.utf8`; File ▸ Import Keywords…).
    pub pick_keyword_list: Option<PickFiles>,
    /// Save dialog for an exported keyword list (File ▸ Export Keywords…).
    pub save_keyword_list: Option<SaveFile>,
    /// Open dialog for point-curve preset files (`.lccurve`).
    pub pick_curve_preset_files: Option<PickFiles>,
    /// Save dialog for an exported `.lccurve` file.
    pub save_curve_preset_file: Option<SaveFile>,
    /// Open dialog for slideshow template files (`.json`; Settings ▸ Slideshow ▸ Templates ▸ Import…).
    pub pick_slideshow_templates: Option<PickFiles>,
    /// Save dialog for an exported slideshow template (`.json`).
    pub save_slideshow_template: Option<SaveFile>,
    /// Open dialog for print template files (`.lcprint`; File ▸ Print… ▸ Templates ▸ Import…).
    pub pick_print_templates: Option<PickFiles>,
    /// Save dialog for an exported print template (`.lcprint`).
    pub save_print_template: Option<SaveFile>,
    pub write: Option<WriteFn>,
    /// Create-new writer (never replaces an existing path), for screenshots of destinations that
    /// did not exist when the request was admitted. Required for such captures: falling back to
    /// [`Self::write`] would reopen the admission-to-write race.
    pub write_new: Option<WriteFn>,
    /// Thread-safe writer: with it, UI-started exports run in the background (desktop only).
    pub write_shared: Option<SharedWrite>,
    /// PNG encoder (the host links an image encoder; the UI crate stays codec-free).
    pub png: Option<PngEncode>,
    /// Show a file in the system file manager (desktop only).
    pub reveal: Option<RevealFn>,
    /// The host's current log file (`<settings>/logs/lightcraft.log`), which Help ▸ Open Log
    /// Folder reveals (#260). None where no log is kept: the web, `--memory`, `LIGHTCRAFT_NO_PREFS`.
    pub log_file: Option<String>,
    /// Choose a folder (Settings → General → Open Library…; desktop only).
    pub pick_folder: Option<PickFolder>,
    /// Open a Lightroom Classic `.lrcat` catalog for read-only import.
    pub pick_lightroom_catalog: Option<PickFolder>,
    /// Open a web link in the browser (Help menu, About, Discord button).
    pub open_url: Option<OpenUrlFn>,
    /// Open a file in an external editor (Edit in External Editor; desktop only).
    pub open_with: Option<OpenWithFn>,
    /// Open several files with one start of the editor (Open Original; desktop only): an editor
    /// that keeps one window takes them all. None = [`Services::open_with`] for each.
    pub open_all_with: Option<OpenAllWithFn>,
    /// Optional worker-safe model-folder opener alongside `open_with` (Linux desktop only).
    #[cfg(target_os = "linux")]
    pub open_model_folder: Option<OpenModelFolderFn>,
    /// Checked before Edit In renders a copy, so a missing editor leaves no copy behind (none =
    /// assume it can).
    pub check_editor: Option<CheckEditorFn>,
    /// File ▸ Back Up Library…: save the whole library (catalog and originals) as one file the
    /// user keeps (web only: there the library lives in browser storage, which the browser may
    /// clear; on the desktop it is a folder backed up like any other).
    pub backup_library: Option<HostAction>,
    /// File ▸ Restore Library from Backup… (web only; keeps the current library).
    pub restore_library: Option<HostAction>,
    /// The system appearance for Appearance Mode ▸ Auto, ahead of egui's own report.
    pub system_theme: Option<SystemThemeFn>,
}

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

impl LightcraftApp {
    /// Build the app with default UI, renderer, cache and control state, and move AI segmentation
    /// to a worker so frames apply results instead of waiting.
    pub fn new(mut session: Session, services: Services) -> Self {
        // AI mask requests run on the model's worker; frames apply their results (never wait)
        session.segmenter.background = true;
        Self {
            session,
            ui: UiState::default(),
            services,
            renderer: render::Renderer::default(),
            perf: Perf::default(),
            caches: Caches::default(),
            integrated_titlebar: false,
            window_movable: None,
            native_menu: false,
            native_shortcuts: Default::default(),
            recording_shortcut: None,
            text_focus: false,
            headless_host: false,
            notices: vec![],
            updates_rt: Default::default(),
            quit_prompt: None,
            notice_input: Default::default(),
            quit_confirmed: false,
            control_rx: None,
            pending_screenshots: vec![],
            pending_gestures: vec![],
            layout_epoch: 0,
            screenshot_token: 0,
            shot_bytes: 0,
            shot_geometry_epoch: 0,
            last_shot_geometry: None,
            max_texture_side: MAX_TEXTURE_SIDE,
            clock: AppClock::new(),
            accesskit_language: None,
            slideshow_hidden_at: None,
            slideshow_auto_paused: false,
            dropped_files: vec![],
            synthetic_in_flight: false,
            ui_passes: 0,
            #[cfg(test)]
            library_passes: vec![],
            shadow: None,
            synthetic: vec![],
            pointer_cleanup: false,
            drop_hover: vec![],
            drop_pending: vec![],
            copied: None,
            pending_picks: vec![],
            synthetic_mods: egui::Modifiers::NONE,
            synthetic_mods_release: false,
            host_mods: egui::Modifiers::NONE,
            styled: false,
            font_language: i18n::Locale::En,
            chinese_font: None,
            fonts_ready: false,
            last_time: 0.0,
            canvas_rect: None,
            image_rect: None,
            grid_scroll: None,
            film_scroll: None,
            widgets: vec![],
            widget_hits: vec![],
            next_widgets: vec![],
            next_widget_hits: vec![],
            gesture: None,
            brush_live: None,
            loupe_shown: None,
            region_view: None,
            region_before_view: None,
            size_hold: Default::default(),
            drag_scale: Default::default(),
            window_refused: None,
            region_tiles: Default::default(),
            compare_windows: [None; 2],
            compare_tiles: Default::default(),
            compare_hold_until: f64::NEG_INFINITY,
            merge: merge::MergeState::default(),
            import: None,
            scan: None,
            sync: None,
            sync_owns_changes: false,
            sync_run: None,
            lightroom: None,
            lightroom_last: None,
            lightroom_result: None,
            review_pending: None,
            shown_library: None,
            export: None,
            tasks: Default::default(),
            last_export_result: None,
            slideshow_export_reported: true,
            hover_preview: None,
            window_is_fullscreen: false,
            gpu_applied: None,
            gpu_notice: false,
            memory_applied: None,
            display_applied: None,
            display_error: None,
            screen_profile: None,
            screen_profile_read: None,
            library_problem: None,
            library_window_input: Default::default(),
            alt_asks_for_library: false,
            recent_checks: Default::default(),
            activity_expanded: false,
            model_setup: Default::default(),
        }
    }

    /// Supply the browser's separately downloaded Simplified Chinese font before the first frame.
    pub fn with_chinese_font(mut self, bytes: Vec<u8>) -> Self {
        if !bytes.is_empty() {
            self.chinese_font = Some(std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        }
        self
    }

    /// Attach the control-channel receiver and keep CPU copies of photo textures, which headless
    /// screenshots need; returns the app for chaining.
    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        // keep CPU copies of photo textures so `ui.screenshot {"headless": true}` can draw them
        self.renderer.keep_pixels = true;
        self
    }

    /// Seconds on the app's monotonic clock. Every timer that must advance while the window is
    /// hidden (imports, autosave, playback, toasts, captures) uses this, never `i.time`.
    pub fn tick_now(&self) -> f64 {
        self.clock.now()
    }

    /// The app clock in milliseconds, for control deadlines and timers.
    fn tick_now_ms(&self) -> f64 {
        self.clock.now() * 1000.0
    }

    /// The immutable identity a capture is bound to: the library generation. It changes when a
    /// different library is opened, not on every edit, import batch or keyword op (`catalog.revision`
    /// bumps on all of those), so a screenshot during interactive editing still belongs to the
    /// document the user asked for.
    fn shot_document_generation(&self) -> u64 {
        self.session.library_generation()
    }

    /// Give a headless driver its simulated time (`None` returns to the monotonic host clock).
    pub fn set_simulated_clock(&mut self, seconds: Option<f64>) {
        self.clock.simulated = seconds.filter(|s| s.is_finite() && *s >= 0.0);
    }

    /// The window's current surface geometry as native points plus native pixels per point.
    /// `run_logic` refreshes `input.viewports`, so this is fresh even while the UI is not laid out;
    /// without one, the last laid-out content rect is used.
    fn shot_geometry(&self, ctx: &egui::Context) -> ShotGeometry {
        // Both `viewport.inner_rect` and the content rect are in egui points, and `pixels_per_point`
        // already includes the UI zoom (egui-winit divides by the zoomed ppp). Multiplying by the
        // zoom again would reserve a surface that is too small, so the zoom is only tracked as a
        // change signal.
        let zoom = ctx.zoom_factor();
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let pixels_per_point = ctx.input(|i| i.pixels_per_point());
        let viewport = ctx.input(|i| {
            i.viewport().inner_rect.filter(|r| r.width() > 0.0 && r.height() > 0.0).map_or_else(|| i.content_rect().size(), |inner| inner.size())
        });
        ShotGeometry { viewport, pixels_per_point, zoom, epoch: self.shot_geometry_epoch }
    }

    /// Track geometry changes as a generation. Called once per logic tick from the refreshed
    /// viewport information, so a hidden window still notices a resize.
    fn note_shot_geometry(&mut self, ctx: &egui::Context) {
        let mut now = self.shot_geometry(ctx);
        if let Some(last) = self.last_shot_geometry
            && (last.viewport != now.viewport || last.pixels_per_point != now.pixels_per_point || last.zoom != now.zoom)
        {
            self.shot_geometry_epoch = self.shot_geometry_epoch.wrapping_add(1);
            now.epoch = self.shot_geometry_epoch;
        }
        self.last_shot_geometry = Some(now);
    }

    /// The texture limit the host reported (16 384 unless it said otherwise).
    fn max_texture_side(&self) -> usize {
        self.max_texture_side
    }

    /// Retain the host's `RawInput::max_texture_side`. Hosts (egui-winit, `Headless`) report it
    /// only when it changes, so keep the last value instead of assuming 16 384.
    fn note_max_texture_side(&mut self, ctx: &egui::Context) {
        if let Some(limit) = ctx.input(|i| i.raw.max_texture_side)
            && limit > 0
        {
            self.max_texture_side = limit;
        }
    }

    /// Reserve a new image's `bytes` against the aggregate screenshot budget, or report why not.
    fn reserve_shot_bytes(&mut self, bytes: usize) -> Result<(), String> {
        if bytes > MAX_SHOT_BYTES {
            return Err(format!("ui.screenshot: {bytes} bytes is over the {MAX_SHOT_BYTES}-byte per-image limit"));
        }
        let total = self.shot_bytes.checked_add(bytes).ok_or("ui.screenshot: byte reservation overflow")?;
        if total > MAX_SHOT_TOTAL_BYTES {
            return Err(format!("ui.screenshot: {total} bytes reserved is over the {MAX_SHOT_TOTAL_BYTES}-byte limit"));
        }
        self.shot_bytes = total;
        Ok(())
    }

    /// A surface that grew after admission (a stale `content_rect` on Wayland, a resize between
    /// issue and completion): bound the *new image* against the per-image limit, then reserve only
    /// the growth against the aggregate. The old per-image check would compare the delta to the
    /// per-image cap and name the delta as if it were the image.
    fn grow_shot_reservation(&mut self, current_reserved: usize, image_bytes: usize) -> Result<(), String> {
        if image_bytes > MAX_SHOT_BYTES {
            return Err(format!("ui.screenshot: {image_bytes} bytes is over the {MAX_SHOT_BYTES}-byte per-image limit"));
        }
        let extra = image_bytes.saturating_sub(current_reserved);
        let total = self.shot_bytes.checked_add(extra).ok_or("ui.screenshot: byte reservation overflow")?;
        if total > MAX_SHOT_TOTAL_BYTES {
            return Err(format!("ui.screenshot: {total} bytes reserved is over the {MAX_SHOT_TOTAL_BYTES}-byte limit"));
        }
        self.shot_bytes = total;
        Ok(())
    }

    /// Release a reservation; never let an accounting bug underflow `usize`.
    fn release_shot_bytes(&mut self, bytes: usize) {
        self.shot_bytes = self.shot_bytes.saturating_sub(bytes);
    }

    /// The open person page, the way back to it, and the photo it was opened from follow a rename.
    fn follow_renamed_person(&mut self, from: &str, to: &str) {
        let same = |n: &str| n.trim().to_lowercase() == from.trim().to_lowercase();
        if self.ui.person_page.as_deref().is_some_and(same) {
            self.ui.person_page = Some(to.to_string());
        }
        if self.ui.last_person.as_deref().is_some_and(same) {
            self.ui.last_person = Some(to.to_string());
        }
        if let Some((Some(name), photo)) = self.ui.person_from.clone()
            && same(&name)
        {
            self.ui.person_from = Some((Some(to.to_string()), photo));
        }
    }

    /// Run a UI or engine command by id. The single entry point for every frontend path.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.run_typed(id, params).map_err(|e| e.message)
    }

    /// [`LightcraftApp::run`], keeping what kind of error a refusal is: for code that decides on it
    /// (offer a model's download, treat a cancel as one) instead of reading the message.
    pub fn run_typed(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        if let Some(result) = model_setup::intercept(self, id, &params) {
            return result.map_err(CommandError::from);
        }
        if let Some(r) = menus::run_ui_command(self, id, &params) {
            if r.is_ok() {
                docking::reveal_command(self, id)?;
            }
            return r.map_err(CommandError::from);
        }
        let params = if id == "crop.aspect"
            && params.get("aspect").and_then(Value::as_str) == Some("toggle")
            && self.crop_tool_aspect().is_some()
            && self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lightcraft_develop::Crop::default())
        {
            serde_json::json!({"aspect": "free"})
        } else {
            params
        };
        let r = self.session.execute(id, &params).map_err(CommandError::from);
        if r.is_ok() && matches!(id, "crop.aspect" | "crop.rotateAspect") {
            self.ui.crop_default_aspect = if id == "crop.aspect" && params.get("aspect").and_then(Value::as_str) == Some("original") {
                Some(serde_json::json!("original"))
            } else {
                self.session
                    .active()
                    .and_then(|photo| self.session.develop_of(photo))
                    .map(|d| d.crop.aspect.map_or(serde_json::json!("free"), |(w, h)| serde_json::json!([w as f64 / 100.0, h as f64 / 100.0])))
            };
        }
        if r.is_ok() && id == "library.toggleFilters" {
            // the search field shows the text filter: empty while the filters are off, back with them
            self.ui.search = self.session.filter.text.clone();
        }
        if r.is_ok() && id == "library.clearFilter" {
            // the search field shows the text filter: cleared from anywhere (the loupe's pill, the
            // menu), it must not keep a search that no longer applies (issue #1648)
            self.ui.search.clear();
        }
        if r.is_ok() && id == "mask.adjust" {
            // Judge local adjustments on the photo, without the selection overlay obscuring them.
            // Keep it hidden after release; O / the overlay eye can show it again.
            self.ui.mask_overlay = false;
        }
        if r.is_ok() && matches!(id, "mask.add" | "mask.addComponent" | "mask.duplicate" | "mask.select") {
            // A new or newly picked mask shows its overlay again, as in Lightroom: hidden by a slider
            // drag above, it stayed hidden while the Overlay menu still named a mode (issue #1800).
            self.ui.mask_overlay = true;
        }
        if r.is_ok() {
            // These commands finish synchronously, so their completion would otherwise
            // be invisible once the activity stack is gone. Never show success on error.
            let completed = match id {
                "library.compact" => Some(format!("✓ {}", i18n::tr("Optimize Library"))),
                "library.clearPreviews" => Some(format!("✓ {}", i18n::tr("Discard Preview Cache"))),
                "develop.autoSync" => Some(format!("{}: {}", i18n::tr("Auto Sync"), i18n::tr(if self.session.auto_sync { "On" } else { "Off" }))),
                _ => None,
            };
            if let Some(message) = completed {
                let now = self.tick_now();
                self.ui.toast = Some((message, now + TOAST_SECONDS, None));
            }
        }
        if r.is_ok() && id == "photo.label" {
            let label = params.get("label").and_then(Value::as_str).and_then(lightcraft_catalog::ColorLabel::parse);
            let text = match label {
                Some(label) => {
                    let name =
                        self.session.catalog.custom_label_name(label).map(str::to_owned).unwrap_or_else(|| i18n::tr(&format!("{label:?}")).into());
                    i18n::tr_format!("{name} Label", name = name)
                }
                None => i18n::tr("Color label cleared").into(),
            };
            let now = self.tick_now();
            self.ui.toast = Some((text, now + TOAST_SECONDS, label));
        }
        match &r {
            Err(e) => {
                log::warn!("{id}: {e}");
                self.ui.status = e.message.clone();
                // no detector yet: offer it (its terms first) instead of only saying so
                if id == "faces.detect" && e.kind == ErrorKind::ModelMissing(AiModel::FaceDetector) {
                    panels::faces::offer_detector(self);
                }
            }
            Ok(v) if id == "faces.detect" => self.ui.status = detect_summary(v),
            Ok(v) if id == "faces.renamePerson" => {
                let to = v["to"].as_str().unwrap_or("").to_string();
                let from = params.get("from").and_then(Value::as_str).unwrap_or("");
                if !to.is_empty() {
                    self.follow_renamed_person(from, &to);
                }
            }
            Ok(_) => {}
        }
        r
    }

    /// The remembered ratio belongs to the tool until the user starts editing the crop.
    pub(crate) fn crop_tool_aspect(&self) -> Option<(u32, u32)> {
        let photo = self.session.active()?;
        let d = self.session.develop_of(photo)?;
        if d.crop != lightcraft_develop::Crop::default() {
            return d.crop.aspect;
        }
        // nothing chosen yet: locked to the photo's own shape, as Lightroom starts (#991); one
        // Lock toggle (remembered as "free") unlocks it for good
        let remembered = self.ui.crop_default_aspect.clone().unwrap_or_else(|| Value::String("original".into()));
        match &remembered {
            Value::String(s) if s == "original" => {
                let p = self.session.catalog.photo(photo)?;
                let (w, h) = (p.width.max(1).saturating_mul(100), p.height.max(1).saturating_mul(100));
                Some(if d.orientation.swaps_axes() { (h, w) } else { (w, h) })
            }
            Value::Array(a) => {
                let number = |i: usize| {
                    let v = a.get(i)?.as_f64()? * 100.0;
                    (v.is_finite() && v >= 1.0 && v <= f64::from(u32::MAX)).then_some(v.round() as u32)
                };
                Some((number(0)?, number(1)?))
            }
            _ => None,
        }
    }

    /// Called only after a crop gesture begins, inside its undo transaction.
    pub(crate) fn begin_crop(&mut self) -> Result<(), String> {
        // nothing chosen yet: the photo's own shape, as `crop_tool_aspect` shows it
        let aspect = self.ui.crop_default_aspect.clone().unwrap_or_else(|| serde_json::json!("original"));
        if self.session.interaction.is_some()
            && self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lightcraft_develop::Crop::default())
            && aspect != serde_json::json!("free")
            // the remembered ratio, turned like the new photo's crop (as a named ratio is)
            && let Err(e) = self.session.execute("crop.aspect", &serde_json::json!({"aspect": aspect, "orientation": "crop"}))
        {
            // A remembered value the command refuses (a hand-edited settings file) would refuse
            // every crop drag on an untouched photo: forget it and let this drag go ahead.
            self.ui.crop_default_aspect = None;
            self.ui.status = e.to_string();
        }
        Ok(())
    }

    /// Advance a running slideshow, but not while the window is hidden: in 0.36 `i.time` froze
    /// there, and a minimized show must not keep waking the event loop or requesting full-size
    /// slide renders overnight. Playback is auto-paused on hide and resumed on unhide with the
    /// slide's remaining time preserved.
    fn slideshow_tick(&mut self, ctx: &egui::Context, now: f64) {
        let hidden = ctx.input(|i| i.viewport().visible() == Some(false));
        let Some(run) = self.ui.slideshow.as_mut() else {
            self.slideshow_hidden_at = None;
            self.slideshow_auto_paused = false;
            return;
        };
        if hidden {
            if self.slideshow_hidden_at.is_none() {
                self.slideshow_hidden_at = Some(now);
                if !run.play.is_paused() {
                    run.play.toggle_pause(now);
                    self.slideshow_auto_paused = true;
                }
            }
            return;
        }
        if self.slideshow_hidden_at.take().is_some() && self.slideshow_auto_paused {
            self.slideshow_auto_paused = false;
            if run.play.is_paused() {
                run.play.toggle_pause(now);
            }
        }
        panels::slideshow::tick(self, ctx, now);
    }

    /// Announce the end of a Build Previews run.
    fn preview_build_status(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let Some(b) = self.session.preview_build.clone() else { return };
        let key = std::sync::Arc::as_ptr(&b) as usize;
        if b.finished.load(Ordering::Relaxed) {
            if self.ui.preview_build_seen != Some((key, true)) {
                self.ui.preview_build_seen = Some((key, true));
                let (done, failed) = (b.done.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed));
                let plural = if done == 1 { "" } else { "s" };
                let mut msg = match (b.error(), b.what) {
                    (Some(e), what) => format!("{}: {e}", if what.is_empty() { "previews" } else { what }),
                    (None, "") => crate::i18n::tr_format!("Previews ready for {done} photo{plural}", done = done, plural = plural),
                    (None, what) => {
                        crate::i18n::tr_format!("Done ({what}): {done} photo{plural}", done = done, plural = plural, what = crate::i18n::tr(what))
                    }
                };
                if failed > 0 {
                    msg.push_str(&crate::i18n::tr_format!(" · {failed} couldn't be rendered", failed = failed));
                }
                self.toast(ctx, msg);
            }
        } else {
            // The activity stack shows the run while it works; poll for the end.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Announce when saving the library starts failing (changes then live only in memory and are
    /// retried) and when it works again; the top bar's cloud icon shows the state meanwhile.
    fn save_status(&mut self, ctx: &egui::Context) {
        let unsaved = self.session.unsaved().map(|(n, e)| (n, e.to_string()));
        match (unsaved, self.ui.unsaved_seen) {
            (Some((n, e)), false) => {
                self.ui.unsaved_seen = true;
                let t = self.tick_now();
                self.ui.toast = Some((
                    crate::i18n::tr_format!(
                        "{n} change{} saved in memory but not written to disk: {e} — LightCraft will retry",
                        if n == 1 { "" } else { "s" },
                        n = n,
                        e = e
                    ),
                    t + 6.0,
                    None,
                ));
            }
            (None, true) => {
                self.ui.unsaved_seen = false;
                self.toast(ctx, crate::i18n::tr("Library saved"));
            }
            (Some(_), true) => ctx.request_repaint_after(std::time::Duration::from_secs(2)), // retry
            (None, false) => {}
        }
    }

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        self.toast_for(ctx, text, TOAST_SECONDS);
    }

    /// Auto straighten (the crop panel's Auto, a double-click with the Straighten tool), saying so
    /// when it finds nothing to level by instead of leaving the photo as it is without a word.
    pub fn auto_straighten(&mut self, ctx: &egui::Context) {
        match self.run("crop.autoStraighten", serde_json::json!({})) {
            Ok(r) if r.get("changed").and_then(Value::as_bool) == Some(false) => {
                self.toast(ctx, crate::i18n::tr("No horizon or vertical lines to level by"));
            }
            Ok(_) => {}
            Err(e) => self.toast(ctx, e),
        }
    }

    /// A toast that stays `secs` seconds (messages that say where to look or what to do next).
    pub fn toast_for(&mut self, _ctx: &egui::Context, text: impl Into<String>, secs: f64) {
        let t = self.tick_now();
        self.ui.toast = Some((text.into(), t + secs, None));
    }

    /// Reverse geocoding: say how a place-names download ended (a failure stays readable, with what to do).
    fn place_names_outcome(&mut self, ctx: &egui::Context) {
        use lightcraft_engine::cmd::geocode::DownloadOutcome;
        match self.session.place_names.take_outcome() {
            Some(DownloadOutcome::Done(_)) => self.toast(ctx, crate::i18n::tr("Place names downloaded")),
            Some(DownloadOutcome::Cancelled) => self.toast(ctx, crate::i18n::tr("Place names download cancelled")),
            Some(DownloadOutcome::Failed(e)) => self.toast_error(ctx, e),
            None if self.session.place_names.downloading() => ctx.request_repaint_after(std::time::Duration::from_millis(500)),
            None => {}
        }
    }

    /// AI masks: apply finished background requests (clicks, descriptions, detail passes) and
    /// show their errors; start a zoomed-in detail pass once the clicking stops
    /// (`ui.detail_due`); watch the model download. Never waits for the model.
    fn ai_mask_detail(&mut self, ctx: &egui::Context, now: f64) {
        let polled = self.session.segment_poll();
        if polled.changed {
            ctx.request_repaint();
        }
        if let Some(mask) = polled.refine {
            // a new selection from a click or a description: show it, as `run` does for new masks
            self.ui.mask_overlay = true;
            self.ui.detail_due = Some((now + 0.3, mask));
        }
        if let Some(e) = polled.messages.into_iter().last() {
            self.ai_error(ctx, e, None);
        }
        let seg = &self.session.segmenter;
        if seg.busy() || seg.pending_clicks().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let download = seg.download_status();
        if download.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // a download finishing while its dialog is closed: say so once
        if self.ui.sam_downloading && !download.running {
            self.ui.sam_downloading = false;
            let open = matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. }));
            match sam_download_ended(&download) {
                _ if open => {}
                Some((text, true)) => self.toast_error(ctx, text),
                Some((text, false)) => self.toast(ctx, text),
                None => {}
            }
        }
        if let Some((due, mask)) = self.ui.detail_due {
            if now >= due && !self.session.segmenter.busy() && !self.session.segmenter.detail_busy() {
                self.ui.detail_due = None;
                if let Err(e) = self.run("mask.refineDetail", serde_json::json!({"id": mask})) {
                    log::warn!("detail pass: {e}");
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        if self.session.segmenter.detail_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// An AI mask error: when the model isn't installed, the dialog that offers to download it
    /// (`then`: the AI mask to start afterwards); otherwise a toast. Decided on the error's kind
    /// ([`ErrorKind::ModelMissing`]), not its words.
    pub fn ai_error(&mut self, ctx: &egui::Context, e: impl Into<CommandError>, then: Option<(&str, &str)>) {
        let e = e.into();
        if lightcraft_engine::segment::Segmenter::AVAILABLE && e.kind == ErrorKind::ModelMissing(AiModel::Sam3) {
            self.offer_sam_download(then);
        } else {
            self.toast_error(ctx, e.message);
        }
    }

    /// Open the dialog offering the SAM 3 download (never downloads by itself).
    pub fn offer_sam_download(&mut self, then: Option<(&str, &str)>) {
        if self.ui.dialog.is_none() || matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. })) {
            self.ui.dialog = Some(state::Dialog::SamModel { then: then.map(|(k, o)| (k.to_string(), o.to_string())), error: None });
        }
    }

    /// A toast for an error the user has to read and act on (stays 6 s).
    pub fn toast_error(&mut self, _ctx: &egui::Context, text: impl Into<String>) {
        let t = self.tick_now();
        self.ui.toast = Some((text.into(), t + 6.0, None));
    }

    /// Take the control receiver for this frame, run every queued request whose sender still
    /// wants it, and reply directly or admit its screenshot/gesture to a later completed pass.
    /// Requests abandoned by their sender are skipped rather than executed late.
    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            // a request its sender gave up on (the window drew nothing for a while) is not run
            // late: it would happen twice once the sender retries (issue #718)
            if !req.take() {
                continue;
            }
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Screenshot { target, headless } => {
                    if let Err(error) = self.admit_shot(ctx, target, headless, reply.clone()) {
                        let _ = reply.send(serde_json::json!({"ok": false, "error": error}));
                    }
                }
                control::Outcome::Gesture { method, params } => {
                    if let Err(error) = self.admit_gesture(ctx, method, params, reply.clone()) {
                        let _ = reply.send(serde_json::json!({"ok": false, "error": error}));
                    }
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// The navigation/document/modal/panel state that decides what an automation id means.
    fn gesture_frame(&self) -> GestureFrame {
        GestureFrame {
            view: self.ui.view,
            active: self.session.active(),
            library: self.session.library_generation(),
            dialog: self.ui.dialog.as_ref().map(std::mem::discriminant),
            left_panel: self.ui.left_panel,
            right: self.ui.right,
            presets: self.ui.presets,
            filmstrip: self.ui.filmstrip,
        }
    }

    /// Queue a synthetic-input request. Nothing is answered and no event is queued until a
    /// completed layout newer than this admission exists (see [`Self::resolve_pending_gestures`]).
    fn admit_gesture(&mut self, ctx: &egui::Context, method: String, params: Value, reply: Sender<ControlResponse>) -> Result<(), String> {
        if self.pending_gestures.len() >= MAX_PENDING_GESTURES {
            return Err(format!("{}: {MAX_PENDING_GESTURES} control gestures are already pending", method));
        }
        let now = self.tick_now_ms();
        self.pending_gestures.push(PendingGesture {
            method,
            params,
            reply,
            admitted_epoch: self.layout_epoch,
            frame: self.gesture_frame(),
            deadline_ms: now + GESTURE_TIMEOUT_MS,
            resolved_pass: None,
            remaining_events: 0,
            started: false,
            target: None,
            pressed: None,
            result: Value::Null,
        });
        ctx.request_repaint();
        Ok(())
    }

    /// Resolve one queued gesture against the fresh widgets/image rect of the pass that just
    /// completed. A changed view/document/dialog/panel state cancels instead of retargeting.
    fn resolve_pending_gestures(&mut self, ctx: &egui::Context) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let frame = self.gesture_frame();
        let epoch = self.layout_epoch;
        let mut gestures = std::mem::take(&mut self.pending_gestures);
        let mut keep = Vec::with_capacity(gestures.len());
        for mut gesture in gestures.drain(..) {
            if gesture.resolved_pass.is_some() {
                // Already handed to the delivery queue; `reply_delivered_gestures` (or
                // `expire_gestures`) owns its deadline and must remove its unsent events too.
                keep.push(gesture);
                continue;
            }
            if now > gesture.deadline_ms {
                // Only this request failed to get a fresh layout. A later gesture has its own
                // deadline and must not be canceled with it.
                let _ =
                    gesture.reply.send(serde_json::json!({"ok": false, "error": format!("{} timed out before the UI consumed it", gesture.method)}));
                continue;
            }
            if epoch <= gesture.admitted_epoch {
                keep.push(gesture);
                continue;
            }
            if gesture.frame != frame {
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: the view, document, dialog or panel layout changed before a fresh layout", gesture.method),
                }));
                continue;
            }
            let before = self.synthetic.len();
            match control::gesture_events(self, ctx, &gesture.method, &gesture.params) {
                Ok(outcome) => {
                    gesture.target = outcome.target;
                    gesture.result = outcome.result;
                    gesture.resolved_pass = Some(self.ui_passes);
                    gesture.remaining_events = self.synthetic.len().saturating_sub(before);
                    gesture.started = gesture.remaining_events == 0;
                    keep.push(gesture);
                }
                Err(error) => {
                    let _ = gesture.reply.send(serde_json::json!({"ok": false, "error": error}));
                }
            }
        }
        self.pending_gestures = keep;
    }

    /// Answer gestures whose last synthetic event a completed, nondiscarded UI pass consumed.
    /// A zero-event action waits for one completed pass after resolution.
    ///
    /// Delivery does not compare the frame again: the gesture's own event may legitimately change
    /// the view, document, dialog or panel (a click that opens a dialog, a key that starts Compare),
    /// so a second snapshot would cancel exactly the input that did its job. Freshness is checked
    /// before resolution ([`Self::resolve_pending_gestures`]) and again immediately before the
    /// batch is injected ([`Self::drop_gestures_stale_before_injection`]).
    fn reply_delivered_gestures(&mut self) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        // Cancel only the gestures that actually stalled: their unsent events are at the front of
        // the queue, and gestures queued behind them are independent input that must survive.
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if self.pending_gestures[i].resolved_pass.is_none() || now <= self.pending_gestures[i].deadline_ms {
                i += 1;
                continue;
            }
            let at_head = self.head_gesture() == Some(i);
            let gesture = self.pending_gestures.remove(i);
            self.cancel_gesture_events(&gesture, at_head);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} timed out before the UI consumed its last event", gesture.method),
            }));
        }
        if !self.synthetic.is_empty() || self.synthetic_in_flight {
            return;
        }
        let mut gestures = std::mem::take(&mut self.pending_gestures);
        let mut keep = Vec::with_capacity(gestures.len());
        for mut gesture in gestures.drain(..) {
            let Some(resolved) = gesture.resolved_pass else {
                keep.push(gesture);
                continue;
            };
            if self.ui_passes <= resolved {
                keep.push(gesture);
                continue;
            }
            if gesture.remaining_events > 0 {
                // A defensive queue reset (a non-head gesture owned unsent events) dropped this
                // batch. It must not become an `ok: true` for input no panel ever saw.
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: its queued input was dropped before delivery", gesture.method),
                }));
                continue;
            }
            let result = std::mem::take(&mut gesture.result);
            let _ = gesture.reply.send(serde_json::json!({"ok": true, "result": result}));
        }
        self.pending_gestures = keep;
    }

    /// The gesture whose events sit at the head of the synthetic queue.
    fn head_gesture(&self) -> Option<usize> {
        self.pending_gestures.iter().position(|gesture| gesture.resolved_pass.is_some() && gesture.remaining_events > 0)
    }

    /// Fail resolved gestures whose view/document/dialog/panel snapshot changed before their
    /// first synthetic batch reached the next completed pass. A hidden logic tick runs between
    /// passes and may change what an automation id or a photo click means; fail closed instead of
    /// delivering it against the new state.
    ///
    /// A widget gesture also has to keep aiming at the requested widget. egui attributes a click
    /// to the widget under the press point in the *previous completed pass*, while the control
    /// batch spans several host frames (a move, then press, then release). Recheck the target
    /// before the first event and before every press; if it moved or disappeared, cancel this
    /// gesture with an error instead of clicking whatever is now at those coordinates.
    ///
    /// Only the gesture at the head of the queue is checked, and later batches of the same gesture
    /// may see its own partial effect (the first click of a double click selects the photo the
    /// second one opens). A second, concurrently queued gesture is checked when its events reach
    /// the head.
    fn drop_gestures_stale_before_injection(&mut self, ctx: &egui::Context) -> bool {
        let Some(head) = self.head_gesture() else { return false };
        let first_batch = !self.pending_gestures[head].started;
        if first_batch && self.pending_gestures[head].frame != self.gesture_frame() {
            let gestures = std::mem::take(&mut self.pending_gestures);
            for gesture in gestures {
                if gesture.resolved_pass.is_none() {
                    self.pending_gestures.push(gesture);
                } else {
                    if gesture.pressed.is_some() {
                        self.queue_pointer_cleanup();
                    }
                    let _ = gesture.reply.send(serde_json::json!({
                        "ok": false,
                        "error": format!("{} canceled: the view, document, dialog or panel layout changed before delivery", gesture.method),
                    }));
                }
            }
            self.synthetic.clear();
            return true;
        }
        let Some(target) = self.pending_gestures[head].target.clone() else { return false };
        let press_next = matches!(self.synthetic.first(), Some(egui::Event::PointerButton { pressed: true, .. }));
        let release_next = matches!(self.synthetic.first(), Some(egui::Event::PointerButton { pressed: false, .. }));
        // A drag legitimately ends outside the widget it started on; only a click must still find
        // its target at the release that egui attributes the click to. A hover must still find a
        // visible widget for its single pointer move.
        let check_release = release_next && target.check_release;
        let check_visible = first_batch && target.check_visible;
        if !first_batch && !press_next && !check_release && !check_visible {
            return false;
        }
        let latest = control::widget_rect(self, &target.id);
        let latest_hit = control::widget_hit(self, &target.id);
        // A press must go to the widget that owns the point. A click's release must too (a drag
        // may legitimately end outside the widget it started on), so an overlay that appears
        // between press and release cancels the click before the release rather than activating it.
        // A hover's single move must also still find the widget visible: the documented contract
        // rejects a clipped or newly covered target instead of moving the pointer at stale coordinates.
        let must_own = press_next || (release_next && target.check_release) || check_visible;
        // A quit prompt or settings notice owns input even before its Foreground areas have
        // settled (egui's first layout pass is non-interactable), and even when a control request
        // is resolved against the registry of that first pass. Only the prompt's own buttons pass.
        let notice_ok = !must_own || crate::panels::notices::allows_target(self, latest_hit);
        let still_aimed = latest.is_some_and(|rect| {
            // A changed widget cancels any gesture before its first event. Owned presses, click
            // releases and hovers must also be inside egui's clipped, visible part and be the
            // topmost widget hit: a target scrolled out of its panel, or one under a non-modal
            // overlay, must not be pressed or hovered at stale coordinates.
            let geometry_ok = rect == target.rect || rect.contains(target.at);
            geometry_ok && notice_ok && (!must_own || control::widget_target_visible(ctx, latest_hit, target.at))
        });
        if still_aimed {
            return false;
        }
        let outside_registered = !target.rect.contains(target.at);
        let reason = match latest {
            None => "disappeared",
            Some(rect) if !(rect == target.rect || rect.contains(target.at)) => "moved",
            Some(_) if outside_registered => "has no visible interaction area at the requested point",
            Some(_) => "is no longer visible",
        };
        let phase = if first_batch {
            "its first event"
        } else if check_release {
            "its release"
        } else {
            "its press"
        };
        // Only this gesture's unsent events are removed: they are at the front of the queue, and
        // gestures resolved after it are checked in turn when they reach the head. Failing the
        // whole click would also discard unrelated input.
        let gesture = self.pending_gestures.remove(head);
        self.cancel_gesture_events(&gesture, true);
        let _ = gesture.reply.send(serde_json::json!({
            "ok": false,
            "error": format!("{} canceled: widget `{}` {reason} before {phase}; nothing was retargeted", gesture.method, target.id),
        }));
        true
    }

    /// Remove a canceled gesture's unsent events from the front of the queue and release a pointer
    /// button it left down. Callers cancel in queue order, so the events removed are exactly this
    /// gesture's unsent prefix; a later gesture's events stay queued.
    fn cancel_gesture_events(&mut self, gesture: &PendingGesture, at_head: bool) {
        if at_head {
            let unsent = gesture.remaining_events.min(self.synthetic.len());
            self.synthetic.drain(..unsent);
        } else if gesture.remaining_events > 0 {
            // The queue order makes only the head own unsent events. If a caller ever breaks that
            // invariant, fail safe: stale synthetic input must not reach an unrelated widget, even
            // at the cost of dropping the rest of this batch instead of panicking or guessing a
            // prefix. `reply_delivered_gestures` fails any gesture whose events disappeared this
            // way instead of reporting a false success.
            log::warn!("canceled non-head gesture still owned {} unsent synthetic events; dropping the queue", gesture.remaining_events);
            self.synthetic.clear();
            self.synthetic_in_flight = false;
        }
        if gesture.pressed.is_some() {
            self.queue_pointer_cleanup();
        }
    }

    /// Cancel every synthetic gesture that a newly shown prompt or blocking library window makes
    /// unsafe.
    ///
    /// A gesture whose input is still queued must not be delivered under the barrier or later when
    /// it is gone. A gesture whose press already reached egui must be released without a click. A
    /// gesture that already delivered all its events may still complete its reply: the barrier may
    /// well be its own result (e.g. a key that opened the quit prompt). `all` is for a batch already
    /// injected this host frame before the barrier was discovered after `raw_input_hook`: the
    /// barrier filter drops it before any panel sees it, so even a fully injected gesture must fail
    /// rather than report a successful delivery.
    fn cancel_gestures_for_barrier(&mut self, all: bool, reason: &str) {
        // All not-yet-injected events belong to the queue's head, one gesture at a time. Cancel
        // them in order so only that gesture's unsent prefix is removed.
        while let Some(head) = self.head_gesture() {
            let gesture = self.pending_gestures.remove(head);
            self.cancel_gesture_events(&gesture, true);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} canceled: {reason} before its input was delivered", gesture.method),
            }));
        }
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if all || self.pending_gestures[i].resolved_pass.is_none() || self.pending_gestures[i].pressed.is_some() {
                let gesture = self.pending_gestures.remove(i);
                self.cancel_gesture_events(&gesture, false);
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: {reason} before its input was delivered", gesture.method),
                }));
            } else {
                i += 1;
            }
        }
    }

    /// Cancel a press that reached egui without making it a click or starting a drag. The reset
    /// itself happens before egui's next input pass in [`Self::raw_input_hook`], where replacing
    /// the pointer state and stopping the drag is safe (the same approach as the sibling
    /// SoundCraft/VectorCraft apps). No pointer movement is injected: moving beyond
    /// `max_click_dist` suppresses a click but can also start a drag on a slider or curve.
    fn queue_pointer_cleanup(&mut self) {
        self.pointer_cleanup = true;
    }

    /// Apply a queued pointer cancellation before egui processes the next input pass.
    fn apply_pointer_cleanup(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if !std::mem::take(&mut self.pointer_cleanup) {
            return;
        }
        ctx.input_mut(|input| input.pointer = egui::PointerState::default());
        ctx.stop_dragging();
        if self.synthetic_mods != egui::Modifiers::NONE {
            // The cleanup removes only the app's synthetic modifiers, never a real host change.
            // A host `ModifiersChanged` in this same raw batch is already authoritative and was
            // recorded before this call; otherwise restore the host's last known state (which may
            // be a physically held Shift), rather than clearing it unconditionally with NONE.
            let host_changed = raw.events.iter().any(|event| matches!(event, egui::Event::ModifiersChanged(_)));
            self.synthetic_mods = egui::Modifiers::NONE;
            if !host_changed {
                raw.events.push(egui::Event::ModifiersChanged(self.host_mods));
            }
        }
        self.synthetic_mods_release = false;
    }

    /// Fail pending gestures at the monotonic deadline even when no UI pass runs (a hidden window
    /// whose paint request the host ignored). `logic` calls this every tick.
    fn expire_gestures(&mut self) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        if !self.pending_gestures.iter().any(|gesture| now > gesture.deadline_ms) {
            return;
        }
        // Cancel exactly the stalled requests, in queue order, so a later gesture's input survives.
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if now <= self.pending_gestures[i].deadline_ms {
                i += 1;
                continue;
            }
            let at_head = self.head_gesture() == Some(i);
            let gesture = self.pending_gestures.remove(i);
            self.cancel_gesture_events(&gesture, at_head);
            let _ = gesture.reply.send(serde_json::json!({"ok": false, "error": format!("{} timed out before the UI consumed it", gesture.method)}));
        }
    }

    /// Admit a `ui.screenshot` request against the bounded capture budget. Nothing is written and
    /// no capture is issued here: [`Self::issue_screenshots`] waits for a settled, nondiscarded UI
    /// pass and a fresh geometry generation.
    fn admit_shot(
        &mut self,
        ctx: &egui::Context,
        target: Option<lightcraft_engine::originals::WriteTarget>,
        headless: bool,
        reply: Sender<ControlResponse>,
    ) -> Result<(), String> {
        if self.pending_screenshots.len() >= MAX_PENDING_SHOTS {
            return Err(format!("ui.screenshot: {MAX_PENDING_SHOTS} requests are already pending; wait for a reply first"));
        }
        let geometry = self.last_shot_geometry.unwrap_or_else(|| self.shot_geometry(ctx));
        let bytes = geometry.bytes(self.max_texture_side()).ok_or_else(|| geometry.rejection(self.max_texture_side()))?;
        self.reserve_shot_bytes(bytes)?;
        self.screenshot_token = self.screenshot_token.wrapping_add(1);
        let now = self.tick_now_ms();
        self.pending_screenshots.push(PendingShot {
            token: self.screenshot_token,
            target,
            reply,
            headless: headless && !self.headless_host,
            admitted_passes: self.ui_passes,
            not_before_ms: now + SHOT_SETTLE_MS,
            // Capture the current settled frame at this instant even if renders are still in flight
            // (0.36 did this at its three-second deadline, so automation never got an error instead
            // of an image while a slider/import kept the renderer busy).
            force_ms: now + SHOT_FORCE_BUSY_MS,
            // Settle + forced capture + compositor fallback + the headless settle, all from admission.
            deadline_ms: now + SHOT_SETTLE_MS + SCREENSHOT_SETTLE_MS + SCREENSHOT_FALLBACK_MS + SCREENSHOT_SETTLE_MS,
            sent_at_ms: None,
            reserved: bytes,
            slot: None,
            issued: None,
            issued_library: self.shot_document_generation(),
        });
        ctx.request_repaint();
        Ok(())
    }

    /// Advance pending screenshots: wait (≥ 3 completed UI frames, ≥ 120 ms) until no renders are
    /// in flight — or until the forced-capture instant, so sustained rendering cannot turn a
    /// request into a timeout — then capture through the compositor's one-shot callback, or
    /// headlessly ([`Self::headless_screenshot`]) when asked or when the compositor delivers nothing.
    fn issue_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let busy = self.renderer.in_flight() > 0 || self.merge.busy();
        let max_side = self.max_texture_side();
        let library = self.shot_document_generation();
        let geometry = self.last_shot_geometry.unwrap_or_else(|| self.shot_geometry(ctx));
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut shadow_ticked = false;
        let mut keep = Vec::with_capacity(shots.len());
        for mut shot in shots.drain(..) {
            // Hard deadline from admission: a compositor fallback does not extend it.
            if now > shot.deadline_ms {
                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": format!("screenshot {} timed out", shot.token)}));
                if let Some(slot) = shot.slot.take() {
                    slot.cancel();
                }
                self.release_shot_bytes(shot.reserved);
                continue;
            }
            if let Some(sent) = shot.sent_at_ms {
                if now - sent < SCREENSHOT_FALLBACK_MS || self.headless_host {
                    keep.push(shot);
                    continue;
                }
                // The compositor delivered nothing (display asleep, window occluded): go headless.
                log::warn!("ui.screenshot: no frame from the compositor after {SCREENSHOT_FALLBACK_MS} ms; rendering headlessly");
                if let Some(slot) = shot.slot.take() {
                    slot.cancel();
                }
                shot.sent_at_ms = None;
                shot.issued = None;
                shot.headless = true;
            }
            // A busy renderer only defers the capture until the forced instant; after that the
            // settled frame that exists now is captured (0.36's deadline behavior), never an error.
            let ready = shot_ready(now, &shot, busy, self.ui_passes);
            if !ready {
                keep.push(shot);
                continue;
            }
            if shot.headless {
                // The shadow frame requests the renders the UI needs, even while the window shows nothing.
                let img = if shadow_ticked { None } else { self.headless_screenshot(ctx, true) };
                shadow_ticked = true;
                if let Some(img) = img {
                    let bytes = img.size[0].checked_mul(img.size[1]).and_then(|p| p.checked_mul(std::mem::size_of::<egui::Color32>()));
                    match bytes {
                        Some(bytes) if bytes > shot.reserved => {
                            // A stale/undersized reservation is not a user error: grow it to the
                            // actual image and encode it when the per-image and aggregate caps allow.
                            if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                                self.release_shot_bytes(shot.reserved);
                                continue;
                            }
                            shot.reserved = bytes;
                        }
                        Some(_) => {}
                        None => {
                            let reply = serde_json::json!({
                                "ok": false,
                                "error": "ui.screenshot: the rendered image has invalid dimensions",
                            });
                            let _ = shot.reply.send(reply);
                            self.release_shot_bytes(shot.reserved);
                            continue;
                        }
                    }
                    let _ = shot.reply.send(control::save_screenshot(self, &img, shot.target.as_ref()));
                    self.release_shot_bytes(shot.reserved);
                    continue;
                }
                keep.push(shot);
                continue;
            }
            // The compositor path needs a real, laid-out UI and fresh fonts; a queued request on a
            // hidden window is kept alive by RequestPaintWhileHidden from `logic`.
            if !self.fonts_ready || self.ui_passes == 0 {
                keep.push(shot);
                continue;
            }
            // Revalidate the reservation against the geometry the command is actually bound to.
            let Some(bytes) = geometry.bytes(max_side) else {
                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": geometry.rejection(max_side)}));
                self.release_shot_bytes(shot.reserved);
                continue;
            };
            if bytes > shot.reserved {
                if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                    let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                    self.release_shot_bytes(shot.reserved);
                    continue;
                }
                shot.reserved = bytes;
            }
            // One slot and one callback per issue; the callback only holds a weak slot and wakes
            // the viewport. File writes and replies happen in `collect_screenshots`.
            let (slot, callback) = craft_ui::capture::CaptureSlot::new(ctx, egui::ViewportId::ROOT, MAX_SHOT_BYTES);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
            log::debug!("ui.screenshot {}: issued at {:?}, {} bytes reserved", shot.token, geometry.viewport, shot.reserved);
            shot.slot = Some(slot);
            shot.sent_at_ms = Some(now);
            shot.issued = Some(geometry);
            shot.issued_library = library;
            keep.push(shot);
        }
        self.pending_screenshots = keep;
        if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// The system's appearance: the host's answer first, then egui's (none: unknown).
    pub fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.services.system_theme.as_ref().and_then(|read| read(ctx)).or_else(|| ctx.system_theme())
    }

    /// Restyle `ctx` when the theme the settings select (Appearance Mode, the dark and light
    /// themes, the system's appearance in Auto) is not the one it shows. A no-op otherwise.
    pub fn sync_theme(&mut self, ctx: &egui::Context) {
        let wanted = self.ui.settings.theme(self.system_theme(ctx));
        if theme::current(ctx) != Some(wanted) {
            theme::apply_kind(ctx, wanted);
        }
    }

    /// Draw the UI into an offscreen context at the window's size; with `capture`, rasterize it
    /// on the CPU (photo textures come from the renderer's CPU copies).
    pub fn headless_screenshot(&mut self, main: &egui::Context, capture: bool) -> Option<egui::ColorImage> {
        let mut view = self.shadow.take().unwrap_or_default();
        let size = main.input(|i| i.content_rect()).size();
        let size = if size.x >= 1.0 && size.y >= 1.0 { size } else { egui::vec2(1600.0, 1000.0) };
        // Feed native viewport points; the shadow context applies the copied UI zoom.
        let zoom = main.zoom_factor();
        let size = size * zoom;
        let ppp = main.pixels_per_point() / zoom;
        let time = main.input(|i| i.time);
        if view.frames() == 0 {
            // warm-up pass: activates our fonts (pending font definitions live in `Memory`, which
            // is replaced below)
            view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |_| {});
        }
        // same scroll offsets, open sections, style… as the window
        let memory = main.memory(|m| m.clone());
        view.ctx.memory_mut(|m| *m = memory);
        view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |ui| self.draw_window(ui));
        let img = capture.then(|| {
            let mut tex = self.renderer.cpu_textures();
            if let (Some((t, ..)), Some(px)) = (&self.merge.preview, &self.merge.preview_pixels) {
                tex.insert(t.id(), crate::softpaint::CpuTexture::linear(px.clone()));
            }
            view.paint(&tex)
        });
        self.shadow = Some(view);
        img
    }

    /// Take each completed capture once, reject stale generations before writing anything, and
    /// reply exactly once. A late callback after cancel/drop finds no slot and is inert.
    fn collect_screenshots(&mut self, _ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let current = self.last_shot_geometry;
        let library = self.shot_document_generation();
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut keep = Vec::with_capacity(shots.len());
        for mut shot in shots.drain(..) {
            let completed = shot.slot.as_ref().and_then(craft_ui::capture::CaptureSlot::take);
            match completed {
                Some(Ok(image)) => {
                    let stale =
                        shot.issued.is_none_or(|issued| current.is_none_or(|now| now.epoch != issued.epoch)) || shot.issued_library != library;
                    if stale {
                        // A resize/DPI/zoom or library change between issue and completion: never
                        // combine old pixels with new geometry. Retry with a fresh slot.
                        shot.slot = None;
                        shot.sent_at_ms = None;
                        shot.issued = None;
                        shot.not_before_ms = now + SHOT_SETTLE_MS;
                        keep.push(shot);
                        continue;
                    }
                    let pixels = image.size[0].checked_mul(image.size[1]).and_then(|p| p.checked_mul(std::mem::size_of::<egui::Color32>()));
                    let Some(bytes) = pixels else {
                        let reply = serde_json::json!({
                            "ok": false,
                            "error": "ui.screenshot: the rendered image has invalid dimensions",
                        });
                        let _ = shot.reply.send(reply);
                        self.release_shot_bytes(shot.reserved);
                        continue;
                    };
                    if bytes > shot.reserved {
                        // Wayland reports no `inner_rect`, so the reservation can be based on a stale
                        // content rect while the compositor returns the new, larger surface. Treat a
                        // larger-but-legal image as a stale reservation and grow it; never mix it
                        // with geometry of another epoch (checked above).
                        if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                            let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                            self.release_shot_bytes(shot.reserved);
                            continue;
                        }
                        shot.reserved = bytes;
                    }
                    self.release_shot_bytes(shot.reserved);
                    let _ = shot.reply.send(control::save_screenshot(self, &image, shot.target.as_ref()));
                }
                Some(Err(error)) => {
                    let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error.to_string()}));
                    self.release_shot_bytes(shot.reserved);
                }
                None => {
                    if now > shot.deadline_ms {
                        let _ = shot.reply.send(serde_json::json!({"ok": false, "error": format!("screenshot {} timed out", shot.token)}));
                        if let Some(slot) = shot.slot.take() {
                            slot.cancel();
                        }
                        self.release_shot_bytes(shot.reserved);
                    } else {
                        keep.push(shot);
                    }
                }
            }
        }
        self.pending_screenshots = keep;
    }

    /// The process is about to end (settings and library are saved by then): make sure nothing is
    /// inside the GPU driver when it does (issue #620: a render running on a worker thread while
    /// the process exits crashes in the driver). No GPU work starts from here on; the render
    /// workers get up to `timeout` to finish their jobs, and what is left of it goes to GPU work
    /// on other threads (export, denoise, the device warm-up). `false`: the deadline passed first
    /// (the host exits anyway). The app renders nothing afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        lightcraft_engine::gpu::begin_shutdown();
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        let stopped = self.renderer.shutdown(timeout);
        #[cfg(not(target_arch = "wasm32"))]
        let timeout = timeout.saturating_sub(t0.elapsed());
        let idle = lightcraft_engine::gpu::wait_idle(timeout);
        stopped && idle
    }

    /// Background and queue work. Called before layout on every host pass and, while the window is
    /// hidden, from `Context::run_logic`. It may read the refreshed viewport fields only: never
    /// `i.time`, events, keys or dropped files.
    pub fn logic(&mut self, ctx: &egui::Context) {
        i18n::set_language(self.ui.language);
        // The fork's AccessKit root language and bidi runs follow the locale; presentation only.
        // Compare against the cached string: `Context::language()` allocates on every call, and
        // this runs on every UI pass and every hidden logic tick.
        let language = self.ui.language.code();
        if self.accesskit_language.as_deref() != Some(language) {
            ctx.set_language(Some(language.to_owned()));
            self.accesskit_language = Some(language.to_owned());
        }
        let t0 = monotonic_ms();
        self.note_max_texture_side(ctx);
        self.note_shot_geometry(ctx);
        self.ensure_fonts(ctx);
        // Applying the theme belongs to logic: a host that only calls `logic` (and every hidden
        // tick) must see an Appearance Mode / system-theme change, and the UI pass that follows
        // then draws with the new visuals. `ui` keeps its own call for a settings change made by
        // a command during the same pass; it is a no-op when the theme already matches.
        self.sync_theme(ctx);
        self.logic_background(ctx);
        self.perf.logic_ms = monotonic_ms() - t0;
    }

    /// Install/refresh fonts once and watch the device and availability caches. Setup, not input.
    fn ensure_fonts(&mut self, ctx: &egui::Context) {
        if !self.styled {
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            theme::apply(ctx);
            Self::drop_legacy_panel_state(ctx);
            // File → Add from Device lists cards scanned in the background: show hot-plugs
            let repaint = ctx.clone();
            lightcraft_engine::devices::on_change(move || repaint.request_repaint());
            // "is the original there?" (grid, Info panel, Missing Photos) answers from a cache a
            // worker fills: a sleeping NAS or a dropped share never stalls a frame
            let repaint = ctx.clone();
            self.session.media.availability.run_in_background(std::sync::Arc::new(move || repaint.request_repaint()));
            self.styled = true;
            self.font_language = self.ui.language;
        } else if self.font_language != self.ui.language {
            // Shared Han characters take the active language's forms (Japanese faces for 日本語,
            // the Simplified Chinese face for 简体中文): rebuild the fallback order.
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            self.font_language = self.ui.language;
        } else {
            self.fonts_ready = true;
        }
    }

    /// 0.36 stored each `PanelState` under the panel's exact `Id`; 0.37 stores it under
    /// `parent_ui.scope_id().with_salt(id_salt)`, so the old entries are unreachable and the
    /// panels reset once by simply not finding them. Remove the old keys anyway so egui memory
    /// does not retain dead entries (no document, workspace JSON, catalog or XMP data is
    /// migrated; nothing above a user's photo edits depends on them).
    fn drop_legacy_panel_state(ctx: &egui::Context) {
        for salt in ["top_bar", "bottom_bar", "tool_strip", "library-problem-banner"] {
            ctx.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(egui::Id::unique(salt)));
        }
    }

    /// Queues and timers. Each consumed queue is drained once per call; time-gated work uses the
    /// monotonic app clock, so a hidden `run_logic` keeps it advancing and a second layout pass is
    /// a no-op rather than a duplicate.
    fn logic_background(&mut self, ctx: &egui::Context) {
        panels::notices::logic(self);
        // closing the window (or Quit) with changes only in memory: retry, else ask first
        if ctx.input(|i| i.viewport().close_requested()) && !panels::notices::may_close(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let now = self.tick_now();
        self.drain_control(ctx);
        self.apply_settings(ctx);
        if self.queued_input_needs_repaint() {
            ctx.request_repaint();
        }
        self.renderer.poll(ctx, &mut self.session);
        merge::poll(self, ctx);
        import::poll_scan(self, ctx);
        sync::poll(self, ctx);
        sync::poll_run(self, ctx);
        // Import commits batches and must not run twice in one host frame. The first layout pass
        // and a hidden logic tick both report pass index 0; a second pass reports 1. A wall-clock
        // gate cannot express that (a slow discarded first pass would re-run it).
        if import_tick_allowed(ctx) {
            import::tick(self, ctx);
        }
        lightroom_import::tick(self, ctx);
        recheck::tick(self);
        tasks::poll(self, ctx);
        updates_ui::tick(self, ctx);
        self.preview_build_status(ctx);
        self.save_status(ctx);
        self.ai_mask_detail(ctx, now);
        self.place_names_outcome(ctx);
        self.slideshow_tick(ctx, now);
        panels::denoise::pump_background(self, ctx, now);
        panels::faces::pump_background(self, ctx, now);
        model_setup::pump(self, ctx, now);
        focus_scan::pump(self, ctx, now);
        // auto import: list the watched folder every few seconds (on a worker thread: it may be on
        // a network share), then import what's new like any import (also on a worker thread)
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(folder) = self.session.import_defaults.auto_folder.clone() {
            const LABEL: &str = "Auto Import";
            if now - self.ui.auto_import_at >= 3.0
                && self.import.is_none()
                && self.sync_run.is_none()
                && self.lightroom.is_none()
                && !self.tasks.is_running(LABEL)
            {
                self.ui.auto_import_at = now;
                let work = move || lightcraft_engine::cmd::library::list_auto_import_folder(&folder);
                let done = |app: &mut LightcraftApp, _ctx: &egui::Context, listing: Result<Vec<(String, u64)>, String>| {
                    let listing = match listing {
                        Ok(l) => l,
                        Err(e) => return log::debug!("auto import: {e}"),
                    };
                    let p = serde_json::json!({"listing": listing, "start": false});
                    let Ok(r) = app.session.execute("library.autoImportScan", &p) else { return };
                    let Some(mut params) = r.get("import").cloned().filter(|_| app.import.is_none()) else { return };
                    let paths: Vec<String> =
                        params["paths"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                    if let Some(o) = params.as_object_mut() {
                        o.remove("paths");
                    }
                    let undo0 = app.session.undo.len();
                    app.import = Some(import::ImportTask::new(paths, params, undo0, false).auto());
                };
                // listed every few seconds: quiet, no row in the activity stack
                if let Err(e) = tasks::spawn(self, LABEL, None, work, done) {
                    log::warn!("{e}");
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
        // automatic XMP writes that failed (a network share offline): tried again every few seconds once their files
        // are reachable, and said once when they start waiting and once when they're written
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.session.unsaved_sidecars() > 0 {
                if now - self.ui.sidecar_retry_at >= 10.0 {
                    self.ui.sidecar_retry_at = now;
                    self.session.retry_unsaved_sidecars();
                }
                ctx.request_repaint_after(std::time::Duration::from_secs(10));
            }
            let waiting = self.session.unsaved_sidecars();
            if waiting > 0 && self.ui.sidecars_waiting == 0 {
                self.toast_for(
                    ctx,
                    crate::i18n::tr_format!(
                        "The XMP sidecars of {n} photos couldn't be written (is their folder offline?). LightCraft keeps trying.",
                        n = waiting
                    ),
                    8.0,
                );
            } else if waiting == 0 && self.ui.sidecars_waiting > 0 {
                self.toast(ctx, crate::i18n::tr("Wrote the XMP sidecars that were waiting"));
            }
            self.ui.sidecars_waiting = waiting;
        }
        // Settings → Import: open the import review when a camera or card is connected
        #[cfg(not(target_arch = "wasm32"))]
        if self.session.import_defaults.show_on_card {
            if let Some(now) = lightcraft_engine::devices::devices_scanned() {
                import::cards_seen(self, now);
            }
            ctx.request_repaint_after(lightcraft_engine::devices::MAX_AGE);
        } else {
            self.ui.known_cards = None;
        }
        self.session.persist_if_dirty();
        self.expire_gestures();
        self.collect_screenshots(ctx);
        self.issue_screenshots(ctx);
        self.request_paint_while_hidden(ctx);
    }

    /// While hidden, queued captures and an injected gesture step need a real UI pass; a pending
    /// screenshot command paints by itself once issued.
    fn request_paint_while_hidden(&self, ctx: &egui::Context) {
        // `logic` runs per layout pass; one command per host frame is enough.
        if ctx.current_pass_index() != 0 {
            return;
        }
        // `ViewportInfo::visible()`: minimized or fully occluded is `Some(false)`; `None`
        // (headless, `RawInput::default()`) is normal and must not be treated as hidden.
        let hidden = ctx.input(|i| i.viewport().visible() == Some(false));
        if hidden && self.needs_ui_pass_while_hidden() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    /// A hidden window needs a real UI pass for queued/settling captures, for control gestures
    /// that still have to resolve or deliver, and for an injected synthetic step. An already
    /// issued screenshot has its own compositor-driven paint and must not request another
    /// `RequestPaintWhileHidden`; its fallback and timeout keep running from `logic`.
    fn needs_ui_pass_while_hidden(&self) -> bool {
        !self.pending_gestures.is_empty()
            || self.pointer_cleanup
            || self.pending_screenshots.iter().any(|shot| shot.sent_at_ms.is_none())
            || !self.synthetic.is_empty()
            || self.synthetic_in_flight
    }

    /// Queued synthetic input, including a queued pointer cancellation that is not part of
    /// `synthetic`, needs the next visible frame even when an earlier stall left the synthetic
    /// queue empty.
    fn queued_input_needs_repaint(&self) -> bool {
        !self.synthetic.is_empty() || self.pointer_cleanup
    }

    /// Input, layout and setup for a known UI phase: at most once per host frame, and never from a
    /// hidden logic tick. `current_pass_index() == 0` is the first pass of this `run_ui`; a second
    /// pass has empty events.
    fn ui_pass(&mut self, ctx: &egui::Context) {
        if ctx.current_pass_index() != 0 {
            return;
        }
        // A prompt owns the keyboard and focus. A notice may have been added by this frame's logic
        // after `raw_input_hook`; capture its Tab/Enter/Space/Escape and drop everything else
        // before any panel, focused control or shortcut reads the input. A new prompt gets a
        // shielding pass whose background is not drawn at all (see `draw_window`).
        let batch_in_flight = self.synthetic_in_flight;
        if panels::notices::prepare_ui_pass(self, ctx) {
            self.cancel_gestures_for_barrier(batch_in_flight, "a modal prompt appeared");
        }
        panels::library_problem::prepare_ui_pass(self, ctx, batch_in_flight);
        let now = self.tick_now();
        // the last pass's time is fine here: this is a UI phase, not a hidden tick
        let t = ctx.input(|i| i.time);
        let dt = t - self.last_time;
        if dt > 0.0 {
            self.perf.fps = self.perf.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
        }
        self.last_time = t;
        panels::denoise::pump_ui_input(self, ctx, now);
        panels::faces::pump_ui_input(self, ctx, now);
        // A prompt owns the library too: files dropped while it is up wait in the queue and are
        // imported on the first UI pass after it goes away, never behind it.
        if !panels::notices::blocks_input(self) {
            self.import_dropped_files(ctx);
        }
        self.check_external_editor(ctx);
        // Recent-library checks and launch-list keys read focus/time/keys: they belong to the UI
        // phase, not the hidden-capable `logic` tick (plan §4.5 / migration input contract).
        panels::library_problem::logic(self, ctx);
        if self.fonts_ready && !updates_ui::blocks_input(self) {
            shortcuts::handle(self, ctx);
        }
    }

    /// Take the host's dropped files, exactly once. The queue is filled by `raw_input_hook`, so a
    /// hidden logic tick can never replay the last pass's drop. Web hosts call this in their UI
    /// phase (they need `bytes_async`, which is not available on a native file path).
    pub fn take_dropped_files(&mut self) -> Vec<egui::DroppedFileHandle> {
        std::mem::take(&mut self.dropped_files)
    }

    /// Import native drops as presets, face models or photos.
    fn import_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = self.take_dropped_files();
        if dropped.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.handle_dropped_files(ctx, dropped);
        #[cfg(target_arch = "wasm32")]
        let _ = (dropped, ctx);
    }

    /// Files dropped on the window: face models open their licence dialog, presets import as
    /// presets, photos and folders go to the import review (or straight in, Settings ▸ Import).
    #[cfg(not(target_arch = "wasm32"))]
    fn handle_dropped_files(&mut self, ctx: &egui::Context, dropped: Vec<egui::DroppedFileHandle>) {
        let dropped: Vec<String> =
            dropped.iter().map(|f| egui::DroppedFile::path(f.as_ref()).to_string_lossy().to_string()).filter(|p| !p.is_empty()).collect();
        if dropped.is_empty() {
            return;
        }
        // a dialog is on screen: the review would replace it, losing what it holds
        if self.ui.dialog.is_some() {
            self.toast(ctx, crate::i18n::tr("Close the open dialog, then drop the files again"));
            return;
        }
        let drop = drop_overlay::classify(dropped);
        if let Some(model) = drop.models.first()
            && let Err(e) = panels::faces::open_dialog(self, model)
        {
            self.toast(ctx, e);
        }
        if !drop.presets.is_empty() {
            // presets live in a library: with none open they would go with the session it replaces
            if panels::library_problem::holds_drops(self) {
                self.toast(ctx, crate::i18n::tr("Open a library first, then drop the presets again"));
            } else {
                let _ = self.run("file.importPresets", serde_json::json!({"paths": drop.presets}));
            }
        }
        if drop.photos.is_empty() {
            return;
        }
        // no library open yet (the launch list, a library that failed to open): they wait for one
        let Some(photos) = panels::library_problem::hold_dropped(self, ctx, drop.photos) else { return };
        if self.lightroom.is_some() {
            self.toast(ctx, crate::i18n::tr("Finish Lightroom catalog import before adding photos"));
            return;
        }
        if import::running(self) {
            self.toast(ctx, crate::i18n::tr("An import is running"));
            return;
        }
        let started = if self.ui.settings.import_dropped_directly {
            // read and added on a worker thread (dropped folders can be large, or on a slow drive)
            import::start_paths(self, photos).map(|_| ())
        } else {
            import::open(self, photos).map(|_| ())
        };
        if let Err(e) = started {
            self.toast(ctx, e);
        }
    }

    /// A file dropped by the host, kept for the next UI pass (web hosts consume their own queue).
    fn queue_dropped_files(&mut self, raw: &mut egui::RawInput) {
        if !raw.dropped_files.is_empty() {
            self.dropped_files.append(&mut raw.dropped_files);
        }
    }

    /// Back from an external editor: pick up the files it saved. Reads `i.focused`, which
    /// `run_logic` does not refresh, so it belongs to the UI phase.
    fn check_external_editor(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        if focused && !self.ui.was_focused && !self.ui.external_edits.is_empty() {
            let ids = self.ui.external_edits.clone();
            if let Ok(r) = self.session.execute("photo.reload", &serde_json::json!({"ids": ids}))
                && r["reloaded"].as_array().is_some_and(|a| !a.is_empty())
            {
                self.toast(ctx, crate::i18n::tr("Updated the edits saved in the external editor"));
            }
        }
        self.ui.was_focused = focused;
    }

    /// Apply settings that act outside the UI state: GPU rendering and window full screen.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        if self.gpu_applied != Some(self.ui.settings.gpu) {
            self.gpu_applied = Some(self.ui.settings.gpu);
            let _ = self.session.execute("app.gpu", &serde_json::json!({"enabled": self.ui.settings.gpu}));
            // GPU device + kernels off the UI thread, once the window is up and only when GPU
            // rendering is on: a broken driver must not keep the window from appearing (issue #136)
            if self.ui.settings.gpu {
                lightcraft_engine::gpu::warm_up();
            }
        }
        let mb = self.ui.settings.memory_mb;
        // automatic at startup: leave the engine's default alone
        if self.memory_applied != Some(mb) && (mb > 0 || self.memory_applied.is_some()) {
            let mb = if mb == 0 { (lightcraft_engine::memory::default_budget() >> 20) as u32 } else { mb };
            let _ = self.session.execute("app.memoryBudget", &serde_json::json!({"mb": mb}));
        }
        self.memory_applied = Some(mb);
        // the monitor profile (Settings ▸ Display): previews follow it from the next frame
        if self.display_applied.as_ref() != Some(&self.ui.settings.display_profile) {
            let path = self.ui.settings.display_profile.clone();
            match lightcraft_engine::display::Display::load_opt(&path) {
                Ok(d) => {
                    self.renderer.set_display(d);
                    self.display_error = None;
                }
                Err(msg) => {
                    // a profile that can't be used: previews are sRGB, the setting is kept
                    self.renderer.set_display(None);
                    log::warn!("display profile: {msg}");
                    self.toast_error(ctx, format!("{}: {msg}", crate::i18n::tr("Display profile not used")));
                    self.display_error = Some(msg);
                }
            }
            self.display_applied = Some(path);
            // none chosen again: the screen's profile is read afresh
            self.screen_profile = None;
            self.screen_profile_read = None;
        }
        if self.ui.settings.display_profile.is_empty() {
            self.follow_screen_profile(ctx);
        }
        if let Some(fs) = ctx.input(|i| i.viewport().fullscreen) {
            self.window_is_fullscreen = fs;
        }
        if let Some(on) = self.ui.window_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
            self.window_is_fullscreen = on;
        }
    }

    /// While no display profile is chosen, show previews through the profile of the screen the
    /// window is on (`services.screen_profile`), read about once a second so moving the window to
    /// another screen is followed. Without the service (or a profile) the display is sRGB.
    fn follow_screen_profile(&mut self, _ctx: &egui::Context) {
        let now = self.tick_now();
        if self.services.screen_profile.is_none() || self.screen_profile_read.is_some_and(|t| (0.0..1.0).contains(&(now - t))) {
            return;
        }
        self.screen_profile_read = Some(now);
        self.apply_screen_profile();
    }

    /// Read the screen's profile now and show previews through it (sRGB without one).
    pub(crate) fn apply_screen_profile(&mut self) {
        let Some(read) = self.services.screen_profile.as_mut() else { return };
        let icc = read();
        if icc == self.screen_profile {
            return;
        }
        let display = icc.as_deref().and_then(|bytes| match lightcraft_engine::display::Display::from_bytes(SCREEN_PROFILE, bytes) {
            Ok(d) => Some(std::sync::Arc::new(d)),
            Err(msg) => {
                // a profile we can't use: previews stay sRGB, as before #787
                log::warn!("screen profile: {msg}");
                None
            }
        });
        self.renderer.set_display(display);
        self.screen_profile = icc;
    }

    /// Take the host's dropped files once and inject synthetic events (pointer events one per
    /// frame; key sequences up to the release).
    ///
    /// The hook also runs before a hidden logic-only tick, so it injects at most one batch and
    /// keeps that batch in flight until a completed UI pass consumed it. It never injects while
    /// the fonts are not installed (the pass would draw nothing and the events would be lost).
    pub fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let file = |p: &String| (!p.is_empty()).then(|| std::path::PathBuf::from(p));
        if !self.drop_pending.is_empty() {
            raw.dropped_files = std::mem::take(&mut self.drop_pending)
                .into_iter()
                .map(|p| std::sync::Arc::new(InjectedDrop(p.into())) as egui::DroppedFileHandle)
                .collect();
        }
        if !self.drop_hover.is_empty() {
            raw.hovered_files = self.drop_hover.iter().map(|p| egui::HoveredFile { path: file(p), ..Default::default() }).collect();
        }
        // Queue after injecting, so a `ui.dropFiles` request is consumed exactly once by the UI phase.
        self.queue_dropped_files(raw);
        // Track the real host modifier state separately from the app's synthetic ownership. A host
        // change means the host now owns the modifier state: relinquish any synthetic claim and do
        // not later synthesise a release for a key that is physically held.
        if let Some(host_mods) = raw.events.iter().rev().find_map(|event| match event {
            egui::Event::ModifiersChanged(modifiers) => Some(*modifiers),
            _ => None,
        }) {
            self.host_mods = host_mods;
            self.synthetic_mods = egui::Modifiers::NONE;
            self.synthetic_mods_release = false;
        }
        // A prompt owns all input. On its first host frame this also removes pointer/touch input
        // before egui computes its hit test; later frames keep only key releases, the prompt's own
        // keys, and AccessKit actions targeted at a prompt button. See `notices::filter_raw_input`.
        let batch_in_flight = self.synthetic_in_flight;
        if crate::panels::notices::prepare_raw_input(self, ctx, raw) {
            // A prompt appearing during a synthetic drag must abort the drag, not let its remaining
            // move/release events act under the prompt or after it is dismissed. If a whole batch
            // was already injected, it is unsafe as well: this frame's prompt filter drops it
            // before a panel sees it, so it must not report a successful delivery.
            self.cancel_gestures_for_barrier(batch_in_flight, "a modal prompt appeared");
            self.queue_pointer_cleanup();
        }
        // Enforce the stall deadline before injecting another event: a request that expired during
        // a long frame gap must not get one more event on the frame that resumes.
        self.expire_gestures();
        // A canceled press is released by resetting egui's pointer state before its next input
        // pass, even while another batch is in flight or the fonts are not ready, so the pointer
        // cannot stay down. No move/release event is injected: either could start a drag or
        // become a click on the widget being canceled.
        self.apply_pointer_cleanup(ctx, raw);
        if !self.fonts_ready || self.synthetic_in_flight {
            return;
        }
        if self.drop_gestures_stale_before_injection(ctx) {
            // The stale check can cancel the head gesture while its press is already down.
            self.apply_pointer_cleanup(ctx, raw);
            return;
        }
        // the frame after a synthetic release / key: restore the host's physical modifiers, not
        // necessarily NONE (Shift may still be held while a synthetic Alt gesture is released)
        if std::mem::take(&mut self.synthetic_mods_release) && self.synthetic_mods != egui::Modifiers::NONE {
            self.synthetic_mods = egui::Modifiers::NONE;
            raw.events.push(egui::Event::ModifiersChanged(self.host_mods));
        }
        if self.synthetic.is_empty() {
            return;
        }
        let n = match self.synthetic[0] {
            egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } => 1,
            _ => self.synthetic.iter().position(|e| matches!(e, egui::Event::Key { pressed: false, .. })).map_or(self.synthetic.len(), |i| i + 1),
        };
        // A press needs the pointer at its position before the button event; the move that is
        // itself the first event is delivered by the drain below, so it must not be sent twice.
        if let Some(egui::Event::PointerButton { pos: p, .. }) = self.synthetic.first() {
            raw.events.push(egui::Event::PointerMoved(*p));
        }
        // egui's `input.modifiers` follow `ModifiersChanged` events, not the modifiers carried by
        // pointer/key events: announce the injected events' modifiers (held from button down to
        // the frame after up), so ⌥-drag, ⇧-click … work through the control channel
        let carried = self.synthetic[..n].iter().find_map(|e| match e {
            egui::Event::PointerButton { pressed, modifiers, .. } => Some((*modifiers, !pressed)),
            egui::Event::Key { modifiers, .. } => Some((*modifiers, true)),
            _ => None,
        });
        if let Some((m, ends)) = carried {
            if m != self.synthetic_mods {
                self.synthetic_mods = m;
                raw.events.push(egui::Event::ModifiersChanged(m));
            }
            self.synthetic_mods_release = ends;
        }
        let blocked_by_prompt = self.synthetic[..n].iter().any(|event| !crate::panels::notices::prompt_allows_event(self, event));
        let head = self.head_gesture();
        if let Some(head) = head {
            let gesture = &mut self.pending_gestures[head];
            for event in &self.synthetic[..n] {
                match event {
                    egui::Event::PointerButton { button, pressed: true, .. } => {
                        gesture.pressed = Some(InjectedPress { button: *button });
                    }
                    egui::Event::PointerButton { button, pressed: false, .. }
                        if gesture.pressed.as_ref().is_some_and(|press| press.button == *button) =>
                    {
                        gesture.pressed = None;
                    }
                    _ => {}
                }
            }
            gesture.started = true;
            gesture.remaining_events = gesture.remaining_events.saturating_sub(n);
        }
        raw.events.extend(self.synthetic.drain(..n));
        // The same barrier applies after injection: only the prompt's own keys and targeted
        // AccessKit actions survive; host input and `ui.text`/clipboard never reach the background.
        crate::panels::notices::filter_raw_input(self, raw);
        if blocked_by_prompt && let Some(index) = head {
            // A settled prompt is already laid out, so this is not the prompt-creation-cancels-all
            // path: only the gesture whose batch the barrier removed fails. Allowed prompt keys,
            // pointer input and targeted AccessKit actions still report a successful delivery.
            let gesture = self.pending_gestures.remove(index);
            self.cancel_gesture_events(&gesture, true);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} canceled: the open modal prompt blocked this input", gesture.method),
            }));
        }
        // The 5 s deadline is a delivery-stall limit, not a limit on a gesture's frame count: a
        // drag feeds one event per host frame, so every batch that the app consumes proves the UI
        // is alive and restarts the clock for the whole queue.
        let now = self.tick_now_ms();
        for gesture in &mut self.pending_gestures {
            gesture.deadline_ms = now + GESTURE_TIMEOUT_MS;
        }
        self.synthetic_in_flight = true;
    }

    /// Lights Out: veil everything but the photo (the loupe's image, or the grid / Compare /
    /// Survey area) in black. Painted above the panels, below dialogs and notices.
    fn lights_out(&self, ctx: &egui::Context) {
        let a = self.ui.lights_out.veil();
        if a <= 0.0 {
            return;
        }
        let screen = ctx.content_rect();
        let keep = if self.ui.view == state::ViewMode::Detail { self.image_rect.or(self.canvas_rect) } else { self.canvas_rect };
        let veil = egui::Color32::from_black_alpha((a.clamp(0.0, 1.0) * 255.0).round() as u8);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("lights-out")));
        let k = keep.unwrap_or(egui::Rect::NOTHING).intersect(screen);
        if !k.is_positive() {
            p.rect_filled(screen, 0.0, veil);
            return;
        }
        // four bands around the photo
        use egui::{Rect, pos2};
        p.rect_filled(Rect::from_min_max(screen.min, pos2(screen.right(), k.top())), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.bottom()), screen.max), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.top()), pos2(k.left(), k.bottom())), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(k.right(), k.top()), pos2(screen.right(), k.bottom())), 0.0, veil);
    }

    /// Frame timings once layout is done (`t0`: when layout started).
    fn end_frame(&mut self, ctx: &egui::Context, t0: f64) {
        if self.ui.frame_rate {
            frame_rate::show(ctx, self);
        } else {
            frame_rate::forget(ctx);
        }
        panels::edit::wheel_pointer_frame_end(ctx);
        ctx.output(|o| {
            for c in &o.commands {
                if let egui::OutputCommand::CopyText(text) = c {
                    self.copied = Some(text.clone());
                }
            }
        });
        self.perf.frame_ms = monotonic_ms() - t0;
        self.perf.update_ms = self.perf.logic_ms + self.perf.frame_ms;
        self.perf.max_update_ms = self.perf.max_update_ms.max(self.perf.update_ms);
    }

    /// Once per library opened (at launch too): name it in the window title, so nobody edits in
    /// the wrong one, and put it first in File ▸ Open Recent Library.
    pub(crate) fn library_changed(&mut self, ctx: &egui::Context) {
        let generation = self.session.library_generation();
        if self.shown_library == Some(generation) {
            return;
        }
        self.shown_library = Some(generation);
        if let Some(l) = self.session.library.as_ref().filter(|l| l.on_disk) {
            let path = l.dir.to_string_lossy().into_owned();
            panels::settings::remember_library(&mut self.ui.settings.recent_libraries, &path);
            // it is open: whatever an earlier check said about it is over
            self.recent_checks.opened(&path);
            // when, for the launch window (issue #1290); only the listed libraries keep a time
            let settings = &mut self.ui.settings;
            settings.recent_opened.insert(path, (self.session.clock)());
            settings.recent_opened.retain(|p, _| settings.recent_libraries.contains(p));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
    }

    /// "LightCraft — <library name>" (just "LightCraft" for a library that isn't saved).
    pub fn window_title(&self) -> String {
        match panels::settings::open_library_name(self) {
            Some(name) => format!("LightCraft — {name}"),
            None => "LightCraft".to_string(),
        }
    }

    /// The browser page's title: "<file name>[ •] — LightCraft" for the active photo (• while
    /// `unsaved`: changes not yet written to storage), else the window title. The web build has no
    /// window for `ViewportCommand::Title`; a host embedding it reads this title to tell which
    /// document a drop opened and whether it is saved (issue #2397).
    pub fn page_title(&self, unsaved: bool) -> String {
        let dot = if unsaved { " •" } else { "" };
        match self.session.active().and_then(|id| self.session.catalog.photo(id)) {
            Some(p) => format!("{}{dot} — LightCraft", p.file_name),
            None => format!("{}{dot}", self.window_title()),
        }
    }

    /// Lay out the whole window: input/setup for the known UI phase, then presentation, then the
    /// frame bookkeeping. A hidden `run_logic` never reaches this.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        i18n::set_language(self.ui.language);
        let ctx = ui.ctx().clone();
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        self.sync_theme(&ctx);
        self.ui_pass(&ctx);
        self.draw_window(ui);
        self.finish_ui_pass(&ctx);
    }

    /// A completed, nondiscarded pass is a settled frame; a discarded pass threw its widgets away
    /// and must not advance the geometry generation a capture binds to.
    fn finish_ui_pass(&mut self, ctx: &egui::Context) {
        // Any pass consumed the injected batch; a discarded pass still read its events and the
        // next pass has none. Only a nondiscarded pass, however, is a settled frame a capture may
        // bind to and a control gesture may resolve against.
        self.synthetic_in_flight = false;
        if ctx.will_discard() {
            self.next_widgets.clear();
            self.next_widget_hits.clear();
            return;
        }
        self.widgets = std::mem::take(&mut self.next_widgets);
        self.widget_hits = std::mem::take(&mut self.next_widget_hits);
        panels::notices::finish_ui_pass(self, ctx);
        panels::library_problem::finish_ui_pass(self, ctx);
        self.ui_passes = self.ui_passes.wrapping_add(1);
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.resolve_pending_gestures(ctx);
        self.reply_delivered_gestures();
        if self.pointer_cleanup {
            // A canceled press may be discovered by a timeout after the synthetic queue emptied;
            // nothing else may ask for the frame that applies its cancellation.
            ctx.request_repaint();
        }
    }

    /// Presentation only: the window's panels, dialogs and overlays.
    fn draw_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let t0 = monotonic_ms();
        panels::library_problem::begin_draw(self);
        // A command or shortcut drawn before this can create the window after `ui_pass`; catch it
        // here before any panel draws with the still-focused background text field.
        let batch_in_flight = self.synthetic_in_flight;
        panels::library_problem::prepare_ui_pass(self, &ctx, batch_in_flight);
        // A newly shown blocking library window gets the same barrier as a prompt: its first Area
        // layout is non-interactable and hit testing still uses the previous pass, so background
        // panels (and a focused text field) must not draw in that pass.
        if panels::library_problem::shield_pass(self, &ctx) {
            #[cfg(test)]
            self.library_passes.push(panels::library_problem::LibraryPass::Shield);
            panels::library_problem::show(self, &ctx, false);
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            ctx.request_discard("library problem: shield the window's creation frame");
            self.end_frame(&ctx, t0);
            return;
        }
        // A newly shown prompt gets one pass with no background. egui's first Area layout is
        // non-interactable and its hit test uses the previous pass, so drawing panels here would
        // let a pointer press/release in the prompt's creation frame act underneath it. `ui()` then
        // reruns this pass with empty raw input, where the settled prompt and the full window draw
        // together. This is a state/input-routing barrier, not a wholesale input disable: once
        // settled, raw pointer input still reaches the prompt backdrop (a no-op) or its buttons.
        if panels::notices::shield_pass(self, &ctx) {
            panels::notices::show(self, &ctx);
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            ctx.request_discard("notice: shield the prompt's creation frame");
            self.end_frame(&ctx, t0);
            return;
        }
        #[cfg(test)]
        self.library_passes.push(panels::library_problem::LibraryPass::Background);
        panels::dialogs::prepare(self, ui);
        // Freeze this frame's notice barrier before `notices::show` can dismiss a prompt: the same
        // click or key must never be acted on again by the dialog drawn after it.
        let notice_blocks = panels::notices::blocks_input(self);
        if std::mem::take(&mut self.ui.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.library_changed(&ctx);
        // panels set it again this frame while the pointer rests on a preset or profile
        self.hover_preview = None;
        if self.ui.fullscreen {
            // full-screen preview: the photo alone on black
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ui, |ui| if self.ui.slideshow.is_some() { panels::slideshow::show(self, ui) } else { panels::detail::show(self, ui) });
            panels::second::show(self, &ctx);
            if notice_blocks {
                panels::notices::restore_prompt_pointer(self, &ctx);
            }
            panels::notices::show(self, &ctx);
            if notice_blocks {
                panels::notices::hide_background_pointer(&ctx);
            }
            panels::dialogs::show(self, &ctx, notice_blocks);
            updates_ui::show(self, &ctx);
            panels::library_problem::show(self, &ctx, notice_blocks);
            panels::toast(self, &ctx);
            drop_overlay::show(self, &ctx);
            if notice_blocks {
                // Keep the real press/release state for the next pass: an ordinary click spans
                // host frames, and resetting it above must only hide it from this pass's dialogs.
                panels::notices::restore_prompt_pointer(self, &ctx);
            }
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            self.end_frame(&ctx, t0);
            return;
        }
        // Order matters: earlier panels take the full edge (top bar spans the window; the tool strip,
        // right panels and left panel run to the bottom; the bottom bar sits between them).
        panels::topbar::show(self, ui);
        panels::library_problem::banner(self, ui);
        panels::strip::show(self, ui);
        // filled: the gaps the dock leaves between its panes show chrome, not the dark clear colour
        let chrome = theme::Tokens::get(&ctx).chrome;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(chrome)).show(ui, |ui| docking::show(self, ui));
        self.lights_out(&ctx);
        panels::second::show(self, &ctx);
        if notice_blocks {
            panels::notices::restore_prompt_pointer(self, &ctx);
        }
        panels::notices::show(self, &ctx);
        if notice_blocks {
            panels::notices::hide_background_pointer(&ctx);
        }
        panels::dialogs::show(self, &ctx, notice_blocks);
        updates_ui::show(self, &ctx);
        panels::library_problem::show(self, &ctx, notice_blocks);
        export_task::poll(self, &ctx);
        slideshow_export::poll(self, &ctx);
        panels::activity::show(self, &ctx);
        export_task::poll_photos(self, &ctx);
        pick::poll(self, &ctx);
        panels::grid::drag_feedback(self, &ctx);
        panels::left::album_drag_feedback(self, &ctx);
        panels::left::section_drag_feedback(self, &ctx);
        panels::keyword_list::drag_feedback(self, &ctx);
        panels::toast(self, &ctx);
        drop_overlay::show(self, &ctx);
        // AppKit moves the window on a drag anywhere in the title-bar band unless the window isn't
        // movable: it stops being movable while the pointer is on a bar control or a panel over it
        #[cfg(target_os = "macos")]
        craft_window::title_bar::sync_movable(&ctx, &mut self.window_movable);
        if notice_blocks {
            // Keep the real press/release state for the next pass: an ordinary click spans host
            // frames, and resetting it above must only hide it from this pass's dialogs.
            panels::notices::restore_prompt_pointer(self, &ctx);
        }
        (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
        self.text_focus = ctx.egui_wants_keyboard_input();
        self.end_frame(&ctx, t0);
    }
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

/// Import commits batches at most once per host frame. The first layout pass and a hidden
/// `run_logic` tick both report pass index 0; a discarded frame's later passes report 1+. A
/// wall-clock gate cannot express that (a slow first pass could re-run it within one frame).
fn import_tick_allowed(ctx: &egui::Context) -> bool {
    ctx.current_pass_index() == 0
}

/// Whether a pending request may be issued now: it has waited for the settle time and completed
/// UI frames, and either no render is in flight or the forced-capture instant has passed.
fn shot_ready(now_ms: f64, shot: &PendingShot, busy: bool, ui_passes: u64) -> bool {
    let settled = ui_passes.saturating_sub(shot.admitted_passes) >= SHOT_SETTLE_FRAMES && now_ms >= shot.not_before_ms;
    settled && (!busy || now_ms >= shot.force_ms)
}

/// The navigation/document/modal/panel state a gesture was admitted under. The same automation
/// id can mean a different control after any of these changes; the request is canceled rather
/// than retargeted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GestureFrame {
    view: crate::state::ViewMode,
    active: Option<lightcraft_catalog::PhotoId>,
    library: u64,
    dialog: Option<std::mem::Discriminant<crate::state::Dialog>>,
    left_panel: bool,
    right: crate::state::RightPanel,
    presets: bool,
    filmstrip: bool,
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

/// A pointer press delivered to the UI, kept so a canceled gesture knows that a button is down.
#[derive(Clone, Copy, Debug)]
struct InjectedPress {
    button: egui::PointerButton,
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

/// A monotonic clock for the app's own timers.
///
/// `web_time::Instant` is backed by the browser's `performance.now()` on WASM and by
/// `std::time::Instant` natively: it never jumps with the wall clock and has no `Date.now()`
/// zero-origin problem. A headless host may supply its simulated time instead.
#[derive(Clone, Copy, Debug)]
struct AppClock {
    start: web_time::Instant,
    simulated: Option<f64>,
}

impl AppClock {
    /// Start the clock at the current monotonic instant with no simulated time.
    fn new() -> Self {
        Self { start: web_time::Instant::now(), simulated: None }
    }

    /// Seconds on the clock: the supplied simulated time when set, otherwise time since `new`.
    fn now(&self) -> f64 {
        self.simulated.unwrap_or_else(|| self.start.elapsed().as_secs_f64())
    }
}

/// Monotonic milliseconds since the first call, for durations and deadlines.
pub fn monotonic_ms() -> f64 {
    static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(web_time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Wall-clock milliseconds since the Unix epoch, only for names that must be unique in the world
/// (`library-restored-<ms>`), never for timing or deadlines.
pub fn now_ms() -> f64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
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

/// "Found 3 faces in 2 photos" for a `faces.detect` result.
fn detect_summary(v: &Value) -> String {
    let photos = v["photos"].as_array().map(Vec::as_slice).unwrap_or_default();
    let counts: Vec<usize> = photos.iter().map(|p| p["faces"].as_array().map_or(0, Vec::len)).collect();
    let (faces, with) = (counts.iter().sum::<usize>(), counts.iter().filter(|n| **n > 0).count());
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (faces, photos.len()) {
        (0, 1) => "No faces found".into(),
        (0, n) => format!("No faces found in {}", plural(n, "photo")),
        (f, 1) => format!("Found {}", plural(f, "face")),
        (f, _) => format!("Found {} in {}", plural(f, "face"), plural(with, "photo")),
    }
}

/// What to say when a SAM 3 download ended while its dialog was closed: the text, and whether it is
/// shown as an error toast. A cancel is told by `cancelled`, not by the error's words.
fn sam_download_ended(download: &lightcraft_engine::segment::DownloadStatus) -> Option<(String, bool)> {
    match &download.error {
        Some(_) if download.cancelled => {
            Some((crate::i18n::tr("SAM 3 download stopped: it resumes where it left off next time.").to_string(), false))
        }
        Some(e) => Some((crate::i18n::tr_format!("The SAM 3 download failed: {e}", e = e), true)),
        None if download.finished => Some((crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready.").to_string(), true)),
        None => None,
    }
}

/// A file `ui.dropFiles` delivers as dropped, as a real OS drop arrives in `raw.dropped_files`.
#[derive(Debug)]
struct InjectedDrop(std::path::PathBuf);

impl egui::DroppedFile for InjectedDrop {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }

    #[cfg(target_arch = "wasm32")]
    fn bytes_async(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + '_>> {
        Box::pin(async { Err("the web build has no files to read".to_string()) })
    }
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

#[cfg(test)]
mod drop_tests {
    #[test]
    fn photo_sidecars_are_not_presets() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let dropped = v(&["/a/IMG_1.CR2", "/a/IMG_1.xmp", "/a/IMG_2.jpg", "/a/IMG_2.jpg.XMP", "/a/Look.xmp", "/b/IMG_1.xmp"]);
        let (presets, photos) = super::split_presets(dropped);
        assert_eq!(photos, v(&["/a/IMG_1.CR2", "/a/IMG_2.jpg"]));
        assert_eq!(presets, v(&["/a/Look.xmp", "/b/IMG_1.xmp"]));
    }

    #[test]
    fn detection_results_are_summarised_in_words() {
        let r =
            |counts: &[usize]| serde_json::json!({"photos": counts.iter().map(|n| serde_json::json!({"faces": vec![0; *n]})).collect::<Vec<_>>()});
        assert_eq!(super::detect_summary(&r(&[0])), "No faces found");
        assert_eq!(super::detect_summary(&r(&[1])), "Found 1 face");
        assert_eq!(super::detect_summary(&r(&[3])), "Found 3 faces");
        assert_eq!(super::detect_summary(&r(&[0, 0])), "No faces found in 2 photos");
        assert_eq!(super::detect_summary(&r(&[2, 0, 1])), "Found 3 faces in 2 photos");
        assert_eq!(super::detect_summary(&serde_json::json!({})), "No faces found in 0 photos");
    }

    #[test]
    fn dropped_face_models_are_told_apart() {
        for p in ["/a/model.onnx", "/a/dir/M.ONNX"] {
            assert!(super::is_model_file(p), "{p}");
        }
        for p in ["/a/model.onnx.jpg", "/a/onnx", "/a/b.png"] {
            assert!(!super::is_model_file(p), "{p}");
        }
    }

    #[test]
    fn dropped_presets_are_told_apart_from_photos() {
        for p in ["/a/Look.lrtemplate", "/a/b.XMP", "/a/pack.zip", "/a/x.lcpreset", "/a/Magic Hour.mplumpack", "/a/Pop.lmp", "/a/Bundle.LMP"] {
            assert!(super::is_preset_file(p), "{p}");
        }
        for p in ["/a/IMG_1.CR2", "/a/b.dng", "/a/c.jpg", "/a/folder"] {
            assert!(!super::is_preset_file(p), "{p}");
        }
    }
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
    /// `folder` when it was made from a folder view: worked out again only when the catalog, the
    /// rules, the language or (for "in the last…" rules) the minute changes, not every frame.
    pub fn rules_view(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        rules: &lightcraft_catalog::RuleSet,
        editing: Option<u64>,
        folder: Option<String>,
        now: &str,
    ) -> std::sync::Arc<RulesView> {
        let relative = rules.depends_on_now();
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let k = key_of((cat.revision, serde_json::to_string(rules).unwrap_or_default(), editing, &folder, minute, i18n::language().code()));
        if let Some((key, v)) = &self.rules_view
            && *key == k
        {
            return v.clone();
        }
        if relative {
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let problems = rules.check_for(cat, editing.map(lightcraft_catalog::AlbumId));
        let filter = lightcraft_catalog::Filter { rule_set: Some(rules.clone()), library_folder: folder, ..Default::default() };
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
        let view = c.rules_view(&cat, &rules(3), None, None, now);
        assert_eq!((view.count, view.problems.len()), (1, 0));
        for _ in 0..10 {
            c.rules_view(&cat, &rules(3), None, None, now);
        }
        assert_eq!(c.rules_view_scans, 1, "unchanged frames reuse it");
        assert_eq!(c.rules_view(&cat, &rules(1), None, None, now).count, 3, "a rule change");
        assert_eq!(c.rules_view(&cat, &rules(9), None, None, now).problems.len(), 1);
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.rules_view(&cat, &rules(3), None, None, now).count, 2, "a catalog change");
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

#[cfg(test)]
mod tests_model_setup;

#[cfg(test)]
mod tests_docking_acceptance;
