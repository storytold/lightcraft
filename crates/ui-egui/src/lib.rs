//! The app's egui frontend: a Lightroom-style UI over `lcc-engine`.
//!
//! The UI is thin: every action goes through [`LccApp::run`], which handles UI commands
//! (views, panels, zoom — see [`menus::ui_commands`]) and forwards everything else to the engine.
//! The same entry point serves menus, shortcuts, buttons and the control channel ([`control`]).
#![forbid(unsafe_code)]
// checked indexing (`get`, `json_ext::JsonExt::field`) only, with no module-level `allow`
#![deny(clippy::indexing_slicing)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod access;
pub mod advanced_search;
pub mod album_picker;
pub mod book;
pub mod brush_live;
pub mod button;
pub mod camera_roll;
pub mod canvas_keys;
#[cfg(not(target_arch = "wasm32"))]
pub mod catalog_ui;
#[cfg(target_arch = "wasm32")]
#[path = "catalog_ui_web.rs"]
pub mod catalog_ui;
pub mod chart_profile;
pub mod color_readout;
mod command_error;
#[cfg(not(target_arch = "wasm32"))]
pub mod comments_ui;
mod json_ext;
/// Review Comments needs the publish services, which the browser build doesn't have.
#[cfg(target_arch = "wasm32")]
pub mod comments_ui {
    use serde_json::Value;

    pub const OPEN: &str = "dialog.reviewComments";
    pub const COMMANDS: &[crate::menus::UiCommand] = &[(OPEN, "Review Comments", None, "")];

    #[derive(Default)]
    pub struct View;

    pub fn enabled(_: &crate::LccApp, id: &str) -> Option<bool> {
        (id == OPEN).then_some(false)
    }

    pub fn run(_: &mut crate::LccApp, id: &str, _: &Value) -> Option<Result<Value, String>> {
        (id == OPEN).then(|| Err("publish services are not available in the browser".to_string()))
    }

    pub fn show(_: &mut crate::LccApp, _: &egui::Context) {}
}
pub mod control;
pub mod creations_ui;
pub mod cull_toast;
pub mod date_picker;
pub mod dialog_frame;
pub mod dng_update_task;
pub mod docking;
mod drop_overlay;
pub mod duplicates_ui;
mod edit_in;
pub mod email_photo;
mod enhance_pump;
pub mod enhance_ui;
pub mod export_module;
pub mod export_task;
pub mod eye_strip;
pub mod eyedropper;
pub mod face_menu;
mod focus_scan;
pub mod folder_poll;
mod frame_rate;
pub mod headless;
pub mod help_link;
pub mod help_overlay;
pub mod i18n;
mod i18n_fork;
pub mod icons;
pub mod import;
pub mod import_report;
mod import_screen;
pub mod import_tab;
pub mod legacy;
pub mod libtools;
pub mod lightroom_destination;
pub mod lightroom_import;
pub mod links;
pub mod loupe_timing;
pub mod lrc_menus;
pub mod machine_test;
pub mod map;
mod mask_thumbs;
mod menu_choices;
pub mod menu_level;
pub mod menubar;
pub mod menus;
pub mod merge;
pub mod modern;
mod modern_dock;
mod modern_input;
mod modern_library;
mod modern_menu;
pub mod modern_sheets;
mod modern_sources;
pub mod module;
pub mod motion;
pub mod overlay;
pub mod panels;
pub mod pick;
pub mod plate;
mod pointer_repair;
pub mod print_ui;
pub mod print_view;
pub mod progress;
mod quality_scan;
pub mod quick_bar;
pub mod rating;
pub mod read_originals;
pub mod recheck;
pub mod region;
pub mod reload;
pub mod render;
pub mod render_dng_task;
pub mod settings_screen;
/// The module shell's frame steps (fork-owned).
mod shell;
pub mod shortcuts;
pub mod sidebar;
pub mod slideshow_export;
pub mod slideshow_ui;
pub mod soft_keyboard;
pub mod softpaint;
pub mod space_hand;
pub mod state;
pub mod surface;
pub mod surface_icons;
pub mod surface_plugin;
pub mod sync;
pub mod tasks;
pub mod template_browser;
pub mod text_field;
pub mod theme;
pub mod timelapse_ui;
pub mod titlebar;
pub mod tool_profiles;
pub mod video_ui;
pub mod video_view;
pub mod view_state;
pub mod web_module;
pub mod web_templates;
pub mod widgets;
pub mod window_place;

#[cfg(test)]
mod i18n_scan;
#[cfg(test)]
mod tests_access;
#[cfg(test)]
mod tests_activity;
#[cfg(test)]
mod tests_adjust_overlay;
#[cfg(test)]
mod tests_adjustment_quality;
#[cfg(test)]
mod tests_album_picker;
#[cfg(test)]
mod tests_album_remove;
#[cfg(test)]
mod tests_appearance;
#[cfg(test)]
mod tests_background_prep;
#[cfg(test)]
mod tests_bar_buttons;
#[cfg(test)]
mod tests_before_after;
#[cfg(test)]
mod tests_bottom_bar_compact;
#[cfg(test)]
mod tests_breadcrumb_more;
#[cfg(test)]
mod tests_browse_active;
#[cfg(test)]
mod tests_button;
#[cfg(test)]
mod tests_camera_colour;
#[cfg(test)]
mod tests_canvas_keys;
#[cfg(test)]
mod tests_canvas_pan;
#[cfg(test)]
mod tests_capture_time;
#[cfg(test)]
mod tests_catalog_ui;
#[cfg(test)]
mod tests_chart_profile;
#[cfg(test)]
mod tests_classic;
#[cfg(test)]
mod tests_clipping;
#[cfg(test)]
mod tests_color_readout;
#[cfg(test)]
mod tests_colour_export;
#[cfg(test)]
mod tests_compact_layout;
#[cfg(test)]
mod tests_compare;
#[cfg(test)]
mod tests_copy_paste_dialogs;
#[cfg(test)]
mod tests_crop_aspects;
#[cfg(test)]
mod tests_crop_frame;
#[cfg(test)]
mod tests_crop_lock_kept;
#[cfg(test)]
mod tests_crop_rotate;
#[cfg(test)]
mod tests_crop_tool;
#[cfg(test)]
mod tests_culling_cells;
#[cfg(test)]
mod tests_curve;
#[cfg(test)]
mod tests_date_picker;
#[cfg(test)]
mod tests_develop_panels;
#[cfg(test)]
mod tests_dialog_fit;
#[cfg(test)]
mod tests_dialog_frame;
#[cfg(test)]
mod tests_dialog_settings;
#[cfg(test)]
mod tests_dialog_tab;
#[cfg(test)]
mod tests_dialog_width;
#[cfg(test)]
mod tests_dialogs;
#[cfg(test)]
mod tests_drop_import;
#[cfg(test)]
mod tests_duplicates_ui;
#[cfg(test)]
mod tests_edit_layout;
#[cfg(test)]
mod tests_empty_library;
#[cfg(test)]
mod tests_enhance_dialog;
#[cfg(test)]
mod tests_error_kinds;
#[cfg(test)]
mod tests_filmstrip;
#[cfg(test)]
mod tests_filmstrip_resize;
#[cfg(test)]
mod tests_filter_search;
#[cfg(test)]
mod tests_focus_point;
#[cfg(test)]
mod tests_folder_add;
#[cfg(test)]
mod tests_folder_drop;
#[cfg(test)]
mod tests_folder_poll;
#[cfg(test)]
mod tests_grading;
#[cfg(test)]
mod tests_grading_mode;
#[cfg(test)]
mod tests_grid;
#[cfg(test)]
mod tests_grid_style;
#[cfg(test)]
mod tests_guided_guides;
#[cfg(test)]
mod tests_hdr_zoom_cache;
#[cfg(test)]
mod tests_help_links;
#[cfg(test)]
mod tests_histogram_drag;
#[cfg(test)]
mod tests_histogram_modes;
#[cfg(test)]
mod tests_hover_pointer;
#[cfg(test)]
mod tests_i18n_coverage;
#[cfg(test)]
mod tests_i18n_sweep;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_immich_ui;
#[cfg(test)]
mod tests_import_body_profiles;
#[cfg(test)]
mod tests_import_drives;
#[cfg(test)]
mod tests_import_loupe;
#[cfg(test)]
mod tests_import_metadata_preset;
#[cfg(test)]
mod tests_import_recipe;
#[cfg(test)]
mod tests_import_report;
#[cfg(test)]
mod tests_import_scan_note;
#[cfg(test)]
mod tests_import_screen;
#[cfg(test)]
mod tests_import_tab;
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
mod tests_lens_db;
#[cfg(test)]
mod tests_levels;
#[cfg(test)]
mod tests_library_grid;
#[cfg(test)]
mod tests_library_problem;
#[cfg(test)]
mod tests_libtools;
#[cfg(test)]
mod tests_lightroom_destination;
#[cfg(test)]
mod tests_loading_cell;
#[cfg(test)]
mod tests_lrc_menus;
#[cfg(test)]
mod tests_marketplace;
#[cfg(test)]
mod tests_mask_effects;
#[cfg(test)]
mod tests_mask_landing;
#[cfg(test)]
mod tests_mask_menus;
#[cfg(test)]
mod tests_mask_range_pick;
#[cfg(test)]
mod tests_mask_sections;
#[cfg(test)]
mod tests_mask_tree;
#[cfg(test)]
mod tests_masking;
#[cfg(test)]
mod tests_masking_layout;
#[cfg(test)]
mod tests_menu_toggles;
#[cfg(test)]
mod tests_menubar;
#[cfg(test)]
mod tests_merge_preview;
#[cfg(test)]
mod tests_midi;
#[cfg(test)]
mod tests_module_help;
#[cfg(test)]
mod tests_modules;
#[cfg(test)]
mod tests_mouse_nav;
#[cfg(test)]
mod tests_offline;
#[cfg(test)]
mod tests_open_lightroom;
#[cfg(test)]
mod tests_osd;
#[cfg(test)]
mod tests_overlay_on;
#[cfg(test)]
mod tests_panel_restore;
#[cfg(test)]
mod tests_panels;
#[cfg(test)]
mod tests_pickers;
#[cfg(test)]
mod tests_pixel_shift;
#[cfg(test)]
mod tests_plugin_jobs;
#[cfg(test)]
mod tests_plugin_signing;
#[cfg(test)]
mod tests_plugin_updates;
#[cfg(test)]
mod tests_preset_favorites;
#[cfg(test)]
mod tests_preset_folders;
#[cfg(test)]
mod tests_preset_import;
#[cfg(test)]
mod tests_presets_manager;
#[cfg(test)]
mod tests_preview_limit;
#[cfg(test)]
mod tests_preview_queue;
#[cfg(test)]
mod tests_print;
#[cfg(test)]
mod tests_profile_manager;
#[cfg(test)]
mod tests_profiles_layout;
#[cfg(test)]
mod tests_progress;
#[cfg(test)]
mod tests_quality_scan_ui;
#[cfg(test)]
mod tests_quick_bar;
#[cfg(test)]
mod tests_quit_unsaved;
#[cfg(test)]
mod tests_rating;
#[cfg(test)]
mod tests_read_originals;
#[cfg(test)]
mod tests_recheck;
#[cfg(test)]
mod tests_remote_render;
#[cfg(test)]
mod tests_rename_person;
#[cfg(test)]
mod tests_render_storms;
#[cfg(test)]
mod tests_retouch;
#[cfg(test)]
mod tests_rules_editor;
#[cfg(test)]
mod tests_sam_setup;
#[cfg(test)]
mod tests_scroll;
#[cfg(test)]
mod tests_settings_back;
#[cfg(test)]
mod tests_settings_categories;
#[cfg(test)]
mod tests_settings_profiles;
#[cfg(test)]
mod tests_settings_tab_row;
#[cfg(test)]
mod tests_settings_window;
#[cfg(test)]
mod tests_slider_drag;
#[cfg(test)]
mod tests_slider_reset;
#[cfg(test)]
mod tests_slideshow;
#[cfg(test)]
mod tests_solo_sections;
#[cfg(test)]
mod tests_space_hand;
#[cfg(test)]
mod tests_space_hold;
#[cfg(test)]
mod tests_stand_in_crop;
#[cfg(test)]
mod tests_surface;
#[cfg(test)]
mod tests_switch_library;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_sync_onboarding;
#[cfg(test)]
mod tests_tallies;
#[cfg(test)]
mod tests_temp_slider;
#[cfg(test)]
mod tests_thumb_churn;
#[cfg(test)]
mod tests_titlebar;
#[cfg(test)]
mod tests_tool_overlay;
#[cfg(test)]
mod tests_tool_pan;
#[cfg(test)]
mod tests_tool_profiles;
#[cfg(test)]
mod tests_tool_row;
#[cfg(test)]
mod tests_unsaved;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_update;
#[cfg(test)]
mod tests_view_state;
#[cfg(test)]
mod tests_viewperf;
#[cfg(test)]
mod tests_vignetting_calibration_ui;
#[cfg(test)]
mod tests_wb_selector;
#[cfg(test)]
mod tests_witchcraft;
#[cfg(test)]
mod tests_zoom;
#[cfg(test)]
mod tests_zoom_keys;

use std::sync::mpsc::{Receiver, Sender};

/// Seconds without a MIDI event after which a knob turn ends (and becomes one undo step).
const MIDI_IDLE: f64 = 0.4;

use lcc_engine::{AiModel, ErrorKind, Session};
use serde_json::Value;

pub use command_error::CommandError;
pub use control::{ControlRequest, ControlResponse};
pub use state::UiState;

pub(crate) const TOAST_SECONDS: f64 = 1.4;

/// How long after a mask slider last moved its overlay stays hidden (ms).
const MASK_ADJUST_HIDE_MS: u64 = 250;
/// How long a picked mask peeks its overlay when it has settings (ms, issue #1109): the user sees
/// which mask is on before judging by its effect takes over.
const MASK_SELECT_PEEK_MS: u64 = 2000;
/// The `path` of a display profile read from the screen rather than chosen as a file.
pub const SCREEN_PROFILE: &str = "screen";

pub type PickFiles = Box<dyn FnMut() -> Vec<String>>;
/// A save dialog: suggested file name → chosen path (`None` = cancelled).
pub type SaveFile = Box<dyn FnMut(&str) -> Option<String>>;
pub type WriteFn = Box<dyn FnMut(&str, &[u8]) -> Result<(), String>>;
/// A writer other threads can use (background export).
pub type SharedWrite = std::sync::Arc<dyn Fn(&str, &[u8]) -> Result<(), String> + Send + Sync>;
pub type PngEncode = Box<dyn Fn(&lcc_raster::Rgba8) -> Vec<u8>>;
/// A folder chooser (`None` = cancelled).
pub type PickFolder = Box<dyn FnMut() -> Option<String>>;
/// Reveal a file in the system file manager (Finder / Explorer / the folder on Linux).
pub type RevealFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Open a URL in the user's browser.
/// Starts listing (or copying) the device's camera roll; the channel answers with the photos'
/// paths, or why there are none (permission denied, …). `Err` when it could not start (Android).
pub type CameraRollFn = Box<dyn FnMut() -> Result<std::sync::mpsc::Receiver<Result<Vec<String>, String>>, String>>;
pub type OpenUrlFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Hands exported files to the device (Android: copies them into Pictures/<app> through
/// MediaStore and, with `share`, opens the share sheet). The channel answers with the saved
/// files' URIs, or why they could not be saved. `Err` when it could not start.
pub type SaveToGalleryFn = Box<dyn FnMut(&[String], bool) -> Result<std::sync::mpsc::Receiver<Result<Vec<String>, String>>, String>>;
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
/// Re-reads the MIDI map and says what happened (`midi.reload`; see `docs/midi.md`).
pub type MidiReload = Box<dyn FnMut() -> Result<String, String>>;
/// Whether an external editor can be started (its name as in [`Services::open_with`]); the
/// error says why not and what to do (e.g. PhotoCraft isn't installed).
pub type CheckEditorFn = Box<dyn Fn(&str) -> Result<(), String>>;
/// Something the host does with the session (web: back up or restore the library in browser
/// storage); the work may finish asynchronously.
pub type HostAction = Box<dyn FnMut(&mut Session) -> Result<Value, String>>;

/// Start or stop the control-surface listener (see [`Services::surface_listener`]).
pub type SurfaceListenerFn = Box<dyn FnMut(Option<u16>) -> Result<(), String>>;

/// The system's dark/light appearance where egui doesn't report one (Linux desktops whose
/// windowing layer says nothing, e.g. many Wayland compositors). The host answers from what it
/// last heard and wakes the UI itself when that changes; it is never polled for changes.
pub type SystemThemeFn = Box<dyn Fn(&egui::Context) -> Option<egui::Theme>>;

/// HDR display output (desktop: an extended-range float window surface; LR-VIEW-HDR-DISPLAY).
/// With it, the loupe shows an HDR edit above SDR white, up to the display's headroom.
pub trait HdrOutput {
    /// The display's peak over SDR white right now when the window shows HDR (`> 1`), else
    /// `None` (an SDR display, HDR switched off in the OS, or unknown). Asked every frame.
    fn headroom(&mut self) -> Option<f32>;
    /// Upload a linear-light render (3 floats per pixel, row-major, SDR white = 1, sRGB
    /// primaries) as the HDR texture of `slot`, replacing that slot's previous one (each slot
    /// keeps its own): a view's ([`render::HDR_LOUPE`]…), or one of a zoomed view's recent
    /// windows, kept to show again.
    fn upload(&mut self, slot: u8, width: usize, height: usize, rgb: &[f32]) -> Result<egui::TextureId, String>;
    /// [`Self::upload`] of a render already encoded as `Rgba16Float` texels for `headroom` (by
    /// [`lcc_engine::hdr_texture::encode_rgba16f`], on a render worker). `None`: this output
    /// doesn't take them (it uploads with another headroom), and [`Self::upload`] is used.
    fn upload_texels(&mut self, slot: u8, width: usize, height: usize, texels: &[u16], headroom: f32) -> Option<Result<egui::TextureId, String>> {
        let _ = (slot, width, height, texels, headroom);
        None
    }
    /// Free `slot`'s texture: nothing shows it any more (a zoomed view's window that left its
    /// recent ones). The slot's next [`Self::upload`] makes a new one.
    fn release(&mut self, slot: u8) {
        let _ = slot;
    }
    /// How much memory the GPU these textures live on offers, in bytes (`None`: unknown). Decides
    /// whether a zoomed view may keep its recent HDR windows ([`render::hdr_cache_fit`]).
    fn gpu_memory(&mut self) -> Option<u64> {
        None
    }
    /// Whether the window surface is extended linear sRGB (macOS EDR / Windows scRGB): when
    /// true, the renderer should skip monitor profile conversion (P2-3).
    fn extended(&self) -> bool {
        false
    }
    /// The headroom [`Self::upload_texels`] takes texels for, when it takes them: an HDR view
    /// rendered on the GPU then has its texels made there and comes back without float samples.
    /// `None` (the default): views come back with their floats, for [`Self::upload`].
    fn texel_headroom(&self) -> Option<f32> {
        None
    }
}

/// Grades a playing clip's frames on the window's GPU (desktop: the egui-wgpu device, through
/// `lcc_gpu::video_lut`), producing the texture to draw without a CPU grade or upload.
pub trait VideoGpu {
    /// Grade linear frame `img` with `lut` (identified by `key`) into a texture to draw (valid
    /// until the next call). An error makes the loupe fall back to the CPU LUT.
    fn grade(&mut self, img: &lcc_raster::Rgb32f, key: u64, lut: &lcc_pipeline::grade::GradeLut) -> Result<(egui::TextureId, [usize; 2]), String>;
}

/// Reading the device's shared storage in place (Android's "All files access"): whether the app
/// may, and a way to ask (the answer arrives later, so `granted` is read again).
pub struct StorageAccess {
    pub granted: Box<dyn Fn() -> bool>,
    pub ask: Box<dyn FnMut() -> Result<(), String>>,
}

/// Platform services injected by the host app (desktop or web).
#[derive(Default)]
pub struct Services {
    /// Native file dialogs for commands, shown off the UI thread and answered later
    /// (`pick`; #191). With it, the synchronous pickers below serve only the buttons inside
    /// dialogs; without it (the web, tests) they answer the commands too.
    pub picker: Option<pick::Picker>,
    /// Failures from legacy path-only dialogs. The UI reports these without replacing the
    /// current dialog or treating the failure as a new command.
    pub dialog_errors: Option<Receiver<String>>,
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
    /// Open dialog for a list file: a collection definition (`.json`) or a keyword list (`.txt`).
    pub pick_list_file: Option<PickFiles>,
    /// Save dialog for a list file (the suggested name carries the extension).
    pub save_list_file: Option<SaveFile>,
    /// Open dialog for slideshow template files (`.json`; Settings ▸ Slideshow ▸ Templates ▸ Import…).
    pub pick_slideshow_templates: Option<PickFiles>,
    /// Save dialog for an exported slideshow template (`.json`).
    pub save_slideshow_template: Option<SaveFile>,
    /// Open dialog for print template files (`.lcprint`; File ▸ Print… ▸ Templates ▸ Import…).
    pub pick_print_templates: Option<PickFiles>,
    /// Save dialog for an exported print template (`.lcprint`).
    pub save_print_template: Option<SaveFile>,
    pub write: Option<WriteFn>,
    /// Thread-safe writer: with it, UI-started exports run in the background (desktop only).
    pub write_shared: Option<SharedWrite>,
    /// PNG encoder (the host links an image encoder; the UI crate stays codec-free).
    pub png: Option<PngEncode>,
    /// Show a file in the system file manager (desktop only).
    pub reveal: Option<RevealFn>,
    /// The host's current log file (`<settings>/logs/<binary>.log`), which Help ▸ Open Log
    /// Folder reveals (#260). None where no log is kept: the web, `--memory`, `<ENV_PREFIX>_NO_PREFS`.
    pub log_file: Option<String>,
    /// Choose a folder (Settings → General → Open Library…; desktop only).
    pub pick_folder: Option<PickFolder>,
    /// Open a Lightroom Classic `.lrcat` catalog for read-only import.
    pub pick_lightroom_catalog: Option<PickFolder>,
    /// Open a web link in the browser (Help menu, About, Discord button).
    pub open_url: Option<OpenUrlFn>,
    /// Android: "All files access" (`app.allFilesAccess`, Settings ▸ Folders).
    pub storage_access: Option<StorageAccess>,
    /// Android: the camera roll (DCIM/Camera through MediaStore; `file.addCameraRoll`).
    pub camera_roll: Option<CameraRollFn>,
    /// Android: exported files into Pictures/<app>, optionally shared (the touch Export sheet).
    pub save_to_gallery: Option<SaveToGalleryFn>,
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
    /// The GPU grade of playing video frames (none: graded on the CPU).
    pub video_gpu: Option<Box<dyn VideoGpu>>,
    /// HDR display output (none: the window is SDR).
    pub hdr_output: Option<Box<dyn HdrOutput>>,
    /// Where control-surface settings are kept (`control-surfaces.json`; none: in memory).
    pub surface_store: Option<Box<dyn surface::SurfaceStore>>,
    /// Open dialog for control-surface files (TactileBridge `settings.json`, `.lp5`, `.lcsurface`).
    pub pick_surface_file: Option<PickFiles>,
    /// Save dialog for an exported `.lcsurface` layout file.
    pub save_surface_file: Option<SaveFile>,
    /// Start (`Some(port)`) or stop (`None`) the loopback listener control surfaces connect to
    /// (desktop only); it serves `surface.*` methods only.
    pub surface_listener: Option<SurfaceListenerFn>,
    /// Installs a surface's plugin into its maker's app (the the app plugin for Logi
    /// Options+; desktop only): Settings ▸ Controls, `surface.installPlugin`.
    pub surface_plugin: Option<std::sync::Arc<dyn surface_plugin::PluginInstaller>>,
}

/// A frame that held the UI thread for [`SLOW_FRAME_MS`] or more, and what ran in it: a stall is
/// told by its causes, not only by its length.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlowFrame {
    pub ms: f64,
    pub logic_ms: f64,
    pub layout_ms: f64,
    /// Commands, control requests and slow panels (`panel.tools 340 ms`) of the frame, in order.
    pub ran: Vec<String>,
    /// When it ended ([`now_ms`]).
    pub at_ms: f64,
}

/// A frame this long or longer is logged and kept as [`LccApp::slowest_frame`].
pub const SLOW_FRAME_MS: f64 = 500.0;
/// The most names a frame's log keeps.
const FRAME_LOG_LEN: usize = 32;
/// A panel that took this long in a frame is named in its log ([`LccApp::lap`]).
const SLOW_PANEL_MS: f64 = 50.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Perf {
    /// Layout of the last frame ([`LccApp::ui`], including commands run from widgets).
    pub frame_ms: f64,
    /// Per-frame logic before layout ([`LccApp::logic`]: control channel, shortcuts,
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
    /// Wall time from the previous frame to this one, measured only after a frame that had work to
    /// show (renders in flight, input): a freeze in painting or presenting shows here.
    pub gap_ms: f64,
    /// The longest [`Self::gap_ms`] since start, and the host's time for the frame before it (its
    /// painting included): a long gap with a short host frame waited outside this thread's work.
    pub max_gap_ms: f64,
    pub max_gap_host_ms: Option<f64>,
    /// The last frame had work to show.
    pub was_busy: bool,
}

/// Why quitting stopped to ask first (`panels::notices`).
#[derive(Clone, Debug, PartialEq)]
pub enum QuitPrompt {
    /// Changes couldn't be written to disk (issue #103).
    Unsaved(String),
    /// Tasks that quitting would cut short are running: an import, an export… (issue #345).
    Tasks(String),
    /// A library backup is due (Library Settings ▸ Back up library): Back Up and Quit, or Skip This Time.
    Backup(String),
}

impl QuitPrompt {
    /// The prompt's message.
    pub fn text(&self) -> &str {
        match self {
            QuitPrompt::Unsaved(text) | QuitPrompt::Tasks(text) | QuitPrompt::Backup(text) => text,
        }
    }
}

/// `v[key] = value` without the panic: sets `key` of the JSON object `v` (null becomes an empty
/// object first, as `IndexMut` does); any other value is left as it is.
pub(crate) fn json_set(v: &mut Value, key: &str, value: Value) {
    if v.is_null() {
        *v = Value::Object(serde_json::Map::new());
    }
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_string(), value);
    }
}

pub struct LccApp {
    /// Per-catalog-revision caches of library-wide results the panels show every frame
    /// (expensive on big libraries).
    pub caches: Caches,
    pub session: Session,
    pub ui: UiState,
    pub services: Services,
    pub renderer: render::Renderer,
    pub perf: Perf,
    /// What this frame ran (commands, control requests, slow panels), for [`SlowFrame`]: at most
    /// [`FRAME_LOG_LEN`] names.
    frame_log: Vec<String>,
    /// The slowest frame since start that took [`SLOW_FRAME_MS`] or more (`ui.inspect` →
    /// `perf.slowestFrame`), with what ran in it.
    pub slowest_frame: Option<SlowFrame>,
    /// macOS: the host draws the traffic lights over our top bar.
    pub integrated_titlebar: bool,
    /// The host installed a native menu bar (no in-window menus then).
    pub native_menu: bool,
    /// Shortcuts the native menu bar currently handles (`Cmd+Z`, `G`…): the egui shortcut handler
    /// leaves them alone so nothing fires twice.
    pub native_shortcuts: std::collections::HashSet<String>,
    /// The keyboard shortcuts editor is waiting for a key press for this command: no shortcut
    /// fires (the native menu bar drops its accelerators too) until it gets one or is cancelled.
    pub recording_shortcut: Option<String>,
    /// A text field had the keyboard last frame (Tab then moves focus as usual).
    pub text_focus: bool,
    /// Tab presses held back from egui's focus navigation (their shift state): they toggle panels.
    pub deferred_tabs: Vec<egui::Modifiers>,
    /// The host is [`headless::Headless`] (it answers viewport screenshot commands itself).
    pub headless_host: bool,
    /// Warnings to show one at a time (damaged settings files…, issue #103).
    pub notices: Vec<String>,
    /// Quitting was stopped to ask first: changes couldn't be saved, or tasks are running.
    pub quit_prompt: Option<QuitPrompt>,
    /// Quit Anyway was chosen: the window may close with unsaved changes.
    pub quit_confirmed: bool,
    /// The quit prompt's answer about a due catalog backup: `Some(true)` Back Up and Quit, `Some(false)` Skip This
    /// Time, `None` not asked (the host backs up when due).
    pub exit_backup: Option<bool>,
    control_rx: Option<Receiver<ControlRequest>>,
    midi_rx: Option<Receiver<lcc_engine::midi::Action>>,
    /// Re-reads the MIDI map file (`midi.reload`); the host app provides it with the MIDI input.
    pub(crate) midi_reload: Option<MidiReload>,
    /// The control a MIDI knob is turning (one undo step) and the UI time of its last event.
    midi_active: Option<(String, f64)>,
    pending_screenshots: Vec<PendingShot>,
    screenshot_token: u64,
    /// Offscreen context for headless screenshots of the windowed app.
    shadow: Option<headless::HeadlessView>,
    /// Test-only: a delay for the sidebar's Missing Photos count worker (a slow disk). Kept here,
    /// not in `ui`, because `ui.set` rebuilds `ui` and would drop it.
    #[cfg(test)]
    pub(crate) missing_count_test_delay: Option<std::time::Duration>,
    /// True while rendering a headless (CPU) screenshot: don't use GPU-only textures.
    headless_screenshotting: bool,
    /// Synthetic input events (from the control channel) injected one step per frame.
    pub synthetic: Vec<egui::Event>,
    /// This frame's input carries synthetic events (an agent's or a script's, [`Self::synthetic`]):
    /// what only the user may do (allow a plug-in's permission) ignores a click in it.
    pub(crate) synthetic_frame: bool,
    /// Input the test harness gives as the user's ([`headless::Headless::click_as_user`]): one
    /// event per frame, never marked synthetic. Nothing outside the harness fills it.
    pub(crate) user_events: Vec<egui::Event>,
    /// Until when (frame time, s) the photographer counts as painting on the loupe: a brush stroke in progress and a
    /// short while after it, as strokes follow one another. Meanwhile only the photo on screen renders; the other
    /// photos a stroke reached (Auto Sync) keep their thumbnails, and the neighbours aren't prepared (ported from
    /// OneSlips, NOTES-1028 note 29).
    pub(crate) paint_hold_until: f64,
    /// [`Self::paint_hold_until`] not reached in this frame.
    pub(crate) paint_hold: bool,
    /// Masks' mask selection, worked out each frame ([`panels::masking::keep_selection`], OneSlips 0d9d4fb7d).
    pub(crate) mask_landing: panels::masking::Landing,
    /// The op (`add`, `subtract`, `intersect`) a People or Remembered picker opened from the selected mask's menu
    /// combines its selection by (`None`: a new mask).
    pub(crate) mask_pick_op: Option<String>,
    /// Files the control channel (`ui.dropFiles`) holds over the window: every frame reports them
    /// as hovered, as the OS does while a drag is over the window.
    drop_hover: Vec<String>,
    /// Files the control channel dropped: delivered as `dropped_files` on the next frame.
    drop_pending: Vec<String>,
    /// The text the UI last put on the clipboard (`ui.inspect` → `copied`).
    pub copied: Option<String>,
    /// Native file dialogs up for commands (`pick`): each command runs again when its closes.
    pub(crate) pending_picks: Vec<pick::Pending>,
    /// The camera roll being listed for `file.addCameraRoll` (`camera_roll::poll`).
    pub(crate) pending_camera_roll: Option<std::sync::mpsc::Receiver<Result<Vec<String>, String>>>,
    /// The watched camera roll's listing in flight (`camera_roll::watch_tick`), apart from
    /// [`Self::pending_camera_roll`] so the watch and the review never fight over one.
    pub(crate) pending_camera_roll_watch: Option<std::sync::mpsc::Receiver<Result<Vec<String>, String>>>,
    /// When the watch last listed the roll, and for which library: a new library lists at once.
    pub(crate) camera_roll_watch_at: Option<(web_time::Instant, std::sync::Arc<()>)>,
    /// Modifiers announced for synthetic input (held from a button down to its release).
    synthetic_mods: egui::Modifiers,
    /// Clear `synthetic_mods` on the next frame.
    synthetic_mods_release: bool,
    /// The mouse's Back / Forward buttons (`Extra1` / `Extra2`) pressed since the last UI pass.
    /// [`Self::raw_input_hook`] takes their events out of egui's input, so no widget treats them
    /// as a press (a slider would jump to the pointer, issue #3303); the shortcuts read them here.
    pub(crate) nav_buttons: Vec<egui::PointerButton>,
    /// Puts back mouse-button releases the window integration dropped (see `pointer_repair`).
    pointer_repair: pointer_repair::PointerRepair,
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
    pub right_panel_rect: Option<egui::Rect>,
    /// Scroll offsets (points) of the photo grid (vertical) and the filmstrip (horizontal) as
    /// drawn last (`ui.inspect` → `scroll`).
    pub grid_scroll: Option<f32>,
    /// Per-photo zoom memory and the mirror mode's pointer rects ([`view_state`]).
    pub view_state: view_state::ViewState,
    pub film_scroll: Option<f32>,
    /// Widget registry from the last frame (automation ids → rects).
    pub widgets: Vec<(String, egui::Rect)>,
    /// A histogram clipping indicator is hovered this frame: the clipping overlay is previewed on
    /// the photo without turning the persistent one on ([`Self::hover_preview`]'s pattern).
    pub clipping_preview: bool,
    /// Since when (egui time) and up to which pass a clipping indicator has been hovered: the
    /// preview waits for the pointer to rest there ([`panels::edit::CLIPPING_PREVIEW_REST_S`]), so a
    /// pointer gliding across the histogram never re-renders the photo (every tile at 100 %).
    pub clipping_hover: Option<(f64, u64)>,
    /// In-progress on-canvas gesture (brush stroke points, gradient drag…).
    pub gesture: Option<panels::detail::Gesture>,
    /// The crop tool's view held still while the crop is dragged, rotated or straightened (issue #60).
    pub crop_layout: Option<panels::detail::CropLayout>,
    /// Space is held over a tool on the photo: the view's hand has the pointer ([`space_hand`]).
    pub space_hand: Option<space_hand::SpaceHand>,
    /// The brush stroke being painted, as drawn on the photo until it's committed (issue #517).
    pub brush_live: Option<brush_live::LiveStroke>,
    /// What the loupe drew last frame: photo and source ("render", "cached", "embedded", "small",
    /// "thumb", "none").
    pub loupe_shown: Option<(lcc_catalog::PhotoId, &'static str)>,
    /// When each photo stepped to in the loupe first showed (`ui.inspect` → `perf.loupeSteps`).
    pub loupe_timing: loupe_timing::LoupeTiming,
    /// How the loupe drew its picture last frame ([`panels::detail::LoupeDrawn`]): where the
    /// picture's corners went and the rect it fills (tests check that no part of it is empty).
    pub loupe_drawn: Option<panels::detail::LoupeDrawn>,
    /// What the loupe's look-ahead was made for last (photo, stepping back, catalog revision,
    /// edge), so it is worked out again only when one of them changes ([`panels::detail`]).
    pub(crate) look_ahead_made_for: Option<(lcc_catalog::PhotoId, bool, u64, usize)>,
    /// The frame ([`render::Renderer::frame`]) in which a cell on screen or the loupe last lacked
    /// its first picture ([`LccApp::pictures_missing`]).
    pub pictures_missing_pass: Option<u64>,
    /// The loupe's last photo and its place in the view, and whether the user stepped back to
    /// the one now on screen: the background preparation goes their way ([`lcc_engine::prep`]).
    pub prep_at: Option<(lcc_catalog::PhotoId, usize)>,
    pub prep_backward: bool,
    /// The module the last frame showed: entering Develop makes room for editing ([`Self::background_prep`]).
    pub prep_module: module::ModuleId,
    /// The loupe's video frames (playback worker, paused still).
    pub video_view: video_view::VideoView,
    /// The zoomed view the loupe asked tiles for last (see [`region`]): the frame, the part its
    /// on-screen tiles cover and their look; for drawing while a pinch runs, and the inspector.
    pub region_view: Option<region::RegionView>,
    /// The same for the Before side of a Before/After view.
    pub region_before_view: Option<region::RegionView>,
    /// The loupe's render sizes while a pinch or two-finger scroll runs.
    pub(crate) size_hold: region::SizeHold,
    /// The size of a slider drag's whole-frame drafts (full size where they keep up, issue #330).
    pub(crate) drag_scale: region::DragScale,
    /// When a drag's drafts give way to the full render in an open interaction.
    pub(crate) still_refine: region::StillRefine,
    /// High quality adjustment previews: whether a drag keeps full quality or falls back to drafts.
    pub(crate) hq_drag: region::HqDrag,
    /// The eye focus strip's results and crops (`view.eyeFocus`).
    pub(crate) eye_strip: eye_strip::EyeStrip,
    /// The quick settings bar in the module bar (`quick_bar`): its fade and the entry it flashes.
    pub(crate) quick_bar: quick_bar::QuickBar,
    /// Where the activity stack ended last frame (None: not shown), so overlays under it keep clear.
    pub(crate) activity_bottom: Option<f32>,
    /// The eye job key of the photo the strip shows (for `ui.inspect`).
    pub(crate) eye_key: Option<u64>,
    /// The photo the loupe shows whole, and since when (host clock, s): after a short look its
    /// full-resolution source is decoded in the background, for a 1:1 zoom to start from.
    pub(crate) whole_since: Option<(lcc_catalog::PhotoId, f64)>,
    /// (photo, look, window frame size) a window was refused for: it reads more than one render holds.
    pub(crate) window_refused: Option<(lcc_catalog::PhotoId, u64, usize)>,
    /// (photo, look) whose 1:1 preview was last asked to be read from disk (see `detail`).
    pub(crate) full_preview_loaded: Option<(lcc_catalog::PhotoId, u64)>,
    /// Photo Merge dialog previews and background merges.
    pub merge: merge::MergeState,
    /// The Enhance dialog's preview and photos.
    pub enhance: enhance_ui::EnhanceUi,
    /// An import in progress (the import review dialog's batches).
    pub import: Option<import::ImportTask>,
    /// The last import's XMP warnings (File ▸ More Imports ▸ Import Warnings…).
    pub import_report: Option<import_report::Report>,
    /// The Find Duplicates window (Library ▸ Find Duplicates…).
    pub duplicates: Option<duplicates_ui::Finder>,
    /// Library ▸ Review Comments.
    pub review_comments: comments_ui::View,
    /// A folder scan in progress (feeds the import review).
    pub scan: Option<import::ScanTask>,
    /// A Synchronize Folder scan in progress (feeds its dialog).
    pub sync: Option<sync::SyncTask>,
    /// `session.folder_changes` came from the Synchronize Folder dialog (and goes with it).
    pub sync_owns_changes: bool,
    /// A Synchronize Folder at work in the background.
    pub sync_run: Option<sync::SyncRun>,
    /// Folder polling (issue #123): the poll at work, when the last one ran.
    pub folder_poll: folder_poll::FolderPoll,
    /// A link (`<scheme>://sync?path=…`) the app was started with, followed once a frame runs.
    pub pending_link: Option<String>,
    /// A Lightroom catalog inspect/import in progress.
    pub lightroom: Option<lightroom_import::LightroomTask>,
    /// Last terminal Lightroom result, exposed by the command's status/wait response.
    pub lightroom_last: Option<Value>,
    /// Catalog dialogs and the Open Recent list (`catalog_ui`).
    pub catalog_ui: catalog_ui::CatalogUi,
    /// A completed import result waiting for another modal dialog to close.
    pub(crate) lightroom_result: Option<state::Dialog>,
    /// The import review of a finished scan, waiting for another dialog to close.
    pub(crate) review_pending: Option<state::Dialog>,
    /// The library generation the window title and the recent list were last updated for.
    pub shown_library: Option<u64>,
    /// A background export in progress.
    pub export: Option<export_task::ExportTask>,
    /// Image exports asked for while `export` runs, started in turn after it (issue #2965).
    pub export_queue: std::collections::VecDeque<export_task::Queued>,
    /// A background Photo ▸ Render to DNG in progress, and the last one's result.
    pub render_dng: Option<render_dng_task::RenderDngTask>,
    pub last_render_dng: Option<serde_json::Value>,
    pub dng_update: Option<dng_update_task::DngUpdateTask>,
    pub last_dng_update: Option<serde_json::Value>,
    /// Background file-system work of other commands (Find Missing Photos, auto import…).
    pub tasks: tasks::Tasks,
    /// Create Camera Profile from Color Chart: its photos and last report.
    pub chart_profile: chart_profile::ChartProfileState,
    /// The files of the last finished background export (`ui.inspect` → `export.last`).
    pub last_export_result: Option<Value>,
    /// The Modern layout's Settings and Export screens (`modern_sheets`).
    pub modern_sheets: modern_sheets::ModernSheets,
    /// The last slideshow export's end has been told (a toast).
    pub slideshow_export_reported: bool,
    /// The look the loupe shows while the pointer rests on a preset or profile (set by the
    /// panels each frame; nothing is committed, no history entry).
    pub hover_preview: Option<HoverPreview>,
    /// The window is in full screen (as last reported by the host, or as last requested).
    pub window_is_fullscreen: bool,
    /// Full Screen Preview (F) put the window into full screen itself: leaving the preview takes
    /// the window back out (issue #2: the preview must cover the taskbar). `None` = no preview.
    preview_window: Option<bool>,
    /// The GPU preference last applied (`app.gpu`), to apply Settings changes once.
    gpu_applied: Option<bool>,
    /// The graphics cards (for editing, second) last handed to the renderer.
    gpu_cards_applied: Option<(String, String)>,
    ml_device_applied: Option<String>,
    /// Say once, when the GPU device has been tried, that no graphics processor was found
    /// ([`panels::notices::gpu_notice`]). The desktop app sets it; headless sessions and tests don't.
    pub gpu_notice: bool,
    /// Fonts the package carries that were missing or damaged, said once
    /// ([`panels::notices::font_notice`]). The desktop app sets it from
    /// [`lcc_engine::fonts::craft_font_problems`]; headless sessions don't.
    pub font_problems: Option<&'static [lcc_engine::fonts::FontProblem]>,
    /// This host may run the start-up update check (when the user turned it on): the desktop app
    /// with a settings folder of its own sets it. Headless sessions (the test harness, `snapshot`,
    /// MCP), `--memory` and `<PREFIX>_NO_PREFS` runs don't, so they never check at start.
    pub startup_update_check: bool,
    /// The window shows SDR only (desktop: eframe's surface is 8-bit, `hdr_display::report`), so
    /// the HDR button says HDR edits are seen through the SDR rendition. Headless/tests: false.
    pub hdr_display_sdr: bool,
    /// The display's headroom when the window shows HDR now ([`HdrOutput::headroom`]): the loupe
    /// then draws HDR edits from their HDR render ([`render::Slot::HdrView`]).
    pub hdr_headroom: Option<f32>,
    /// The last GPU event told (`lcc_gpu::events_after`: the graphics card stopped
    /// responding, was restarted …).
    gpu_event_seen: u64,
    /// The window's size last frame: a resize asks the GPU's batch work (exports) to make room.
    last_screen: egui::Vec2,
    /// The export lanes, processor-only switch and GPU share last applied (install settings).
    export_tuning_applied: Option<(u32, bool, u32, u32, lcc_engine::export::ExportCards)>,
    /// "Test my machine" while it runs (Settings ▸ Performance).
    pub machine: Option<machine_test::MachineTask>,
    /// The memory budget setting last applied (MB, 0 = automatic).
    memory_applied: Option<u32>,
    /// The SAM 3 folder setting last handed to the segmenter (`AppSettings::sam3_dir`).
    sam3_applied: Option<String>,
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
    /// Files that waited for a library and wait for the running import or Synchronize Folder too.
    pub waiting_import: Vec<String>,
    /// Immich: Connections settings, the Import dialog's Immich source, background pump state.
    #[cfg(not(target_arch = "wasm32"))]
    pub immich: panels::connections::ImmichUi,
    /// Settings ▸ Sync: the last status and whether the app is looking for devices.
    #[cfg(not(target_arch = "wasm32"))]
    pub peer_sync: panels::peer_sync::PeerSyncUi,
    /// The Print module's settings and job state.
    pub print: print_ui::PrintUi,
    /// Alt/Option held as the window first appears offers the recent libraries once (issue
    /// #1290); the desktop host arms it when nothing chose the library already.
    pub alt_asks_for_library: bool,
    /// Whether each recent library can be opened, answered by a worker (issue #1290): the launch
    /// list and File ▸ Open Recent Library never look at a folder on the UI thread.
    pub recent_checks: panels::library_problem::RecentChecks,
    /// The activity stack shows every task, not just the first few ("+N more" was clicked).
    pub activity_expanded: bool,
    /// Batches in the activity stack whose per-photo rows are shown.
    pub activity_open: std::collections::HashSet<u64>,
    /// The last batch summary said (`Activity::finished` → `seq`): each is said once.
    pub activity_seen_seq: u64,
    /// Classic: the list of tasks under the module bar's activity area is open (`view.activityList`).
    pub activity_list: bool,
    /// Classic: where the module bar drew its activity area this frame (set by `module_bar`, taken by the stack).
    pub(crate) activity_plate_drawn: Option<egui::Rect>,
    /// …and in the last frame: what the list opens under, and whether `view.activityList` can open it.
    pub(crate) activity_plate: Option<egui::Rect>,
    /// The Map module's view state (P3.3).
    pub map: map::MapUi,
    /// The Classic module help sheet (⌘/, P6.3).
    pub help: help_overlay::HelpOverlay,
    /// A dialog panel asked to be closed (it cannot do it itself: the dialog frame writes the state
    /// back after the body has drawn). Taken by the dialog frame each frame.
    pub close_dialog: bool,
    /// Control surfaces: their engine, pairing and subscribers (`surface`).
    pub surface: surface::SurfaceHost,
}

impl LccApp {
    /// A cell on screen or the loupe has no picture yet in frame `pass` ([`render::Renderer::frame`]),
    /// its first one on its way: this window's own background requests (the neighbours prepared
    /// ahead, real thumbnails behind stand-ins) wait, and so does the engine's background work
    /// ([`lcc_engine::prep::pictures_missing`]).
    pub fn pictures_missing(&mut self, pass: u64) {
        self.pictures_missing_pass = Some(pass);
        lcc_engine::prep::pictures_missing();
    }

    /// Preparing photos in the background ([`lcc_engine::prep`]): the whole view's previews once
    /// the user is culling in the loupe, built from the photo on screen in the direction of travel;
    /// held back while a slider drags.
    fn background_prep(&mut self, ctx: &egui::Context) {
        if self.session.interaction.is_some() {
            lcc_engine::prep::foreground();
        }
        // entering Develop under a small memory budget (a phone): the background build stops and what browsing held is
        // let go, so the photo being edited has the memory (the grid's pictures come back from the disk cache)
        if self.ui.module == module::ModuleId::Develop && self.prep_module != module::ModuleId::Develop && self.session.make_room_for_editing() {
            self.renderer.evict_thumbs(&std::collections::HashSet::new(), 0);
        }
        self.prep_module = self.ui.module;
        let mode = panels::detail::prep_mode(self);
        let loupe = self.ui.fullscreen || self.ui.view == state::ViewMode::Detail;
        let at = if loupe { self.session.active() } else { None };
        self.session.prep_tick(mode, at, self.prep_backward);
        // (the build's render jobs are made a few photos a tick)
        if self.session.prep_feeding() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.session.preview_build.as_ref().is_some_and(|b| b.running()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        // an unfinished build is taken up again once the view has been still, also when nothing
        // else draws a frame
        if let Some(wait) = self.session.prep_wake_in() {
            ctx.request_repaint_after(wait);
        }
    }

    /// Whether a picture was missing in frame `pass` or the one before.
    pub fn pictures_busy(&self, pass: u64) -> bool {
        self.pictures_missing_pass.is_some_and(|p| pass.saturating_sub(p) <= 1)
    }

    /// Whether the selected mask has settings (an effect to judge), see [`panels::detail::mask_has_settings`].
    fn active_mask_has_settings(&self) -> bool {
        let Some(id) = self.session.active_mask else { return false };
        self.session
            .active()
            .and_then(|p| self.session.develop_of(p))
            .is_some_and(|d| d.masks.iter().any(|m| m.id == id && panels::detail::mask_has_settings(m)))
    }

    /// A mask was just picked (created, duplicated, selected, an AI click): with the overlay on
    /// automatic it shows by itself while the mask has nothing to judge yet; one with settings
    /// arms a short peek of its overlay instead ([`MASK_SELECT_PEEK_MS`], issue #1109), so the
    /// user still sees which mask the program has selected before judging by its effect takes
    /// over again. Never latches the overlay on: `mask_overlay` keeps the rule it had.
    fn mask_pick_overlay(&mut self) {
        let has_settings = self.active_mask_has_settings();
        self.ui.mask_peek_until = (self.ui.mask_overlay_auto && !self.ui.mask_overlay_pinned && has_settings)
            .then(|| std::time::Instant::now() + std::time::Duration::from_millis(MASK_SELECT_PEEK_MS));
        self.ui.mask_overlay = self.ui.mask_overlay_pinned || (self.ui.mask_overlay_auto && !has_settings);
    }

    /// How panel sliders are laid out: always pills in the Modern layout, else as Settings say.
    pub(crate) fn slider_layout(&self) -> theme::SliderLayout {
        if self.ui.modern.enabled {
            theme::SliderLayout::Pill
        } else if self.compact() {
            theme::SliderLayout::OneRow
        } else {
            self.ui.settings.slider_layout
        }
    }

    /// Whether the Compact layout is drawn: chosen, and the Modern layout is off.
    pub(crate) fn compact(&self) -> bool {
        self.ui.settings.compact_layout && !self.ui.modern.enabled
    }

    /// The right panel's default width follows the layout (Compact: [`state::COMPACT_RIGHT_WIDTH`],
    /// LAYOUT-DENSE, ported from OneSlips) while it is at the other layout's default; a width the
    /// user dragged to stays.
    fn follow_compact_width(&mut self, ctx: &egui::Context) {
        let key = egui::Id::new("compact-right-width");
        let now = self.compact();
        let was = ctx.data(|d| d.get_temp::<bool>(key));
        if was == Some(now) {
            return;
        }
        ctx.data_mut(|d| d.insert_temp(key, now));
        // (at launch only Compact moves it: Classic's default needs no change)
        if was.is_some() || now {
            self.ui.right_width = state::follow_compact_width(self.ui.right_width, now);
        }
    }

    pub fn new(mut session: Session, services: Services) -> Self {
        if cfg!(test) {
            // unit tests never open the file manager or start other programs
            session.desktop = std::sync::Arc::new(lcc_engine::desktop::Recorder::default());
        }
        // AI mask requests run on the model's worker; frames apply their results (never wait)
        session.segmenter.background = true;
        // generative fills run on their own thread; frames apply them
        session.genfill.background = true;
        session.upright_jobs.background = true;
        // a flag or a step never waits for the drive: the catalog log's own thread writes and
        // syncs it (control replies wait for it, `drain_control`)
        session.wait_for_catalog_log = false;
        Self {
            session,
            #[cfg(not(target_arch = "wasm32"))]
            immich: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            peer_sync: Default::default(),
            print: Default::default(),
            map: Default::default(),
            help: Default::default(),
            close_dialog: false,
            ui: UiState::default(),
            services,
            renderer: render::Renderer::default(),
            perf: Perf::default(),
            frame_log: Vec::new(),
            slowest_frame: None,
            caches: Caches::default(),
            integrated_titlebar: false,
            native_menu: false,
            native_shortcuts: Default::default(),
            recording_shortcut: None,
            text_focus: false,
            deferred_tabs: Vec::new(),
            headless_host: false,
            notices: vec![],
            quit_prompt: None,
            quit_confirmed: false,
            exit_backup: None,
            control_rx: None,
            midi_rx: None,
            midi_reload: None,
            midi_active: None,
            pending_screenshots: vec![],
            screenshot_token: 0,
            #[cfg(test)]
            missing_count_test_delay: None,
            shadow: None,
            headless_screenshotting: false,
            synthetic: vec![],
            synthetic_frame: false,
            user_events: vec![],
            paint_hold_until: f64::NEG_INFINITY,
            paint_hold: false,
            mask_landing: Default::default(),
            mask_pick_op: None,
            drop_hover: vec![],
            drop_pending: vec![],
            copied: None,
            pending_picks: vec![],
            pending_camera_roll: None,
            pending_camera_roll_watch: None,
            camera_roll_watch_at: None,
            synthetic_mods: egui::Modifiers::NONE,
            synthetic_mods_release: false,
            nav_buttons: vec![],
            pointer_repair: pointer_repair::PointerRepair::default(),
            styled: false,
            font_language: i18n::Locale::En,
            chinese_font: None,
            fonts_ready: false,
            last_time: 0.0,
            canvas_rect: None,
            image_rect: None,
            right_panel_rect: None,
            grid_scroll: None,
            view_state: Default::default(),
            film_scroll: None,
            widgets: vec![],
            gesture: None,
            crop_layout: None,
            space_hand: None,
            brush_live: None,
            loupe_shown: None,
            loupe_timing: Default::default(),
            loupe_drawn: None,
            look_ahead_made_for: None,
            pictures_missing_pass: None,
            prep_at: None,
            prep_backward: false,
            prep_module: module::ModuleId::Library,
            video_view: Default::default(),
            region_view: None,
            region_before_view: None,
            size_hold: Default::default(),
            drag_scale: Default::default(),
            still_refine: Default::default(),
            hq_drag: Default::default(),
            eye_strip: Default::default(),
            quick_bar: Default::default(),
            activity_bottom: None,
            eye_key: None,
            whole_since: None,
            window_refused: None,
            full_preview_loaded: None,
            merge: merge::MergeState::default(),
            enhance: enhance_ui::EnhanceUi::default(),
            import: None,
            import_report: None,
            duplicates: None,
            review_comments: comments_ui::View::default(),
            scan: None,
            sync: None,
            sync_owns_changes: false,
            sync_run: None,
            folder_poll: Default::default(),
            pending_link: None,
            lightroom: None,
            lightroom_last: None,
            catalog_ui: Default::default(),
            lightroom_result: None,
            review_pending: None,
            shown_library: None,
            export: None,
            export_queue: Default::default(),
            render_dng: None,
            last_render_dng: None,
            dng_update: None,
            last_dng_update: None,
            tasks: Default::default(),
            chart_profile: Default::default(),
            last_export_result: None,
            modern_sheets: Default::default(),
            slideshow_export_reported: true,
            hover_preview: None,
            clipping_preview: false,
            clipping_hover: None,
            window_is_fullscreen: false,
            preview_window: None,
            gpu_applied: None,
            gpu_cards_applied: None,
            ml_device_applied: None,
            gpu_notice: false,
            font_problems: None,
            startup_update_check: false,
            hdr_display_sdr: false,
            hdr_headroom: None,
            gpu_event_seen: 0,
            last_screen: egui::Vec2::ZERO,
            export_tuning_applied: None,
            machine: None,
            memory_applied: None,
            sam3_applied: None,
            display_applied: None,
            display_error: None,
            screen_profile: None,
            screen_profile_read: None,
            library_problem: None,
            waiting_import: Vec::new(),
            alt_asks_for_library: false,
            recent_checks: Default::default(),
            activity_expanded: false,
            activity_open: Default::default(),
            activity_seen_seq: 0,
            activity_list: false,
            activity_plate_drawn: None,
            activity_plate: None,
            surface: Default::default(),
        }
    }

    /// Supply the browser's separately downloaded Simplified Chinese font before the first frame.
    pub fn with_chinese_font(mut self, bytes: Vec<u8>) -> Self {
        if !bytes.is_empty() {
            self.chinese_font = Some(std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        }
        self
    }

    /// Drive develop controls from a MIDI controller; see `docs/midi.md`.
    pub fn with_midi(mut self, rx: Receiver<lcc_engine::midi::Action>, reload: MidiReload) -> Self {
        self.midi_rx = Some(rx);
        self.midi_reload = Some(reload);
        self
    }

    /// Take requests from `rx` (the desktop app's control-surface listener) without the extra
    /// screenshot support of [`Self::with_control`].
    pub fn with_requests(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        // keep CPU copies of photo textures so `ui.screenshot {"headless": true}` can draw them
        self.renderer.keep_pixels = true;
        self
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
    /// Whether the photos shown are a folder being browsed to cull and import, not the library's.
    pub fn importing(&self) -> bool {
        self.session.source == lcc_engine::LibrarySource::Folder
    }

    /// Whether the eye focus strip shows here: it has its own switch while browsing a folder (on
    /// at first: culling) and in the library (off at first: editing).
    pub fn eye_focus_on(&self) -> bool {
        if self.importing() { self.ui.eye_focus_import } else { self.ui.eye_focus }
    }

    /// Switch the eye focus strip on or off where the user is (a browsed folder or the library).
    pub fn set_eye_focus(&mut self, on: bool) {
        if self.importing() {
            self.ui.eye_focus_import = on;
        } else {
            self.ui.eye_focus = on;
        }
    }

    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.run_typed(id, params).map_err(|e| e.message)
    }

    /// [`LccApp::run`], keeping what kind of error a refusal is: for code that decides on it
    /// (offer a model's download, treat a cancel as one) instead of reading the message.
    pub fn run_typed(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        let id = &*lcc_engine::canonical_id(id);
        self.note_ran(id);
        shortcuts::set_active(self.ui.settings.keymap_set);
        // a brush stroke being painted belongs to the mask and photo it started on: picking another
        // ends it first, so its release can't land on the one picked
        if matches!(id, "mask.select" | "library.select") {
            self.cancel_brush_gesture();
        }
        // a MIDI turn still open when anyone else starts an interaction is closed first: its idle
        // timeout would otherwise end the new interaction instead (the turn's own begin finds none open)
        if id == "develop.beginInteraction" {
            self.end_midi_interaction();
        }
        if let Some(r) = edit_in::intercept(self, id, &params) {
            return r.map_err(CommandError::from);
        }
        if video_ui::blocked(self, id) {
            return Err(CommandError::from(i18n::tr(video_ui::NOT_FOR_VIDEO).to_string()));
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
            && self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lcc_develop::Crop::default())
        {
            serde_json::json!({"aspect": "free"})
        } else {
            params
        };
        // an edit of the crop on an untouched photo keeps the lock the tool shows
        let commits = self.session.commits();
        let stored_lock = touches_crop(id, &params, self.session.active()) && self.store_shown_crop_lock();
        let r = self.session.execute(id, &params).map_err(CommandError::from);
        if stored_lock && self.session.interaction.is_none() {
            let steps = usize::try_from(self.session.commits().wrapping_sub(commits)).unwrap_or(0);
            match &r {
                // the lock and the edit are one undo step, named after the edit
                Ok(_) if steps >= 2 => {
                    let label = self.session.undo.last().map(|e| e.label.clone()).unwrap_or_default();
                    self.session.merge_undo(steps, &label);
                }
                // the edit was refused or changed nothing: take the lock back off as well
                _ if steps == 1 && self.session.undo_step().is_ok() => {
                    self.session.redo.pop();
                }
                _ => {}
            }
        }
        if let Ok(v) = &r {
            edit_in::after_command(self, id, v);
        }
        if let Ok(v) = &r
            && id == "ml.status"
            && (params.get("device").is_some() || params.get("preference").is_some())
            && let Some(dev_choice) = v.get("deviceChoice").and_then(Value::as_str)
        {
            let choice = if dev_choice.eq_ignore_ascii_case("auto") { "" } else { dev_choice.trim() };
            if self.ui.settings.ml_device != choice {
                self.ui.settings.ml_device = choice.to_string();
            }
            self.ml_device_applied = Some(choice.to_string());
        }
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
            // the search field shows the active text or semantic filter: empty while the filters are off, back with them
            if let Some(q) = &self.session.filter.semantic {
                self.ui.semantic_search = true;
                self.ui.search = q.clone();
            } else if !self.session.filter.text.is_empty() {
                self.ui.semantic_search = false;
                self.ui.search = self.session.filter.text.clone();
            } else {
                self.ui.search.clear();
            }
        }
        if r.is_ok() && id == "library.clearFilter" {
            // the search field shows the text filter: cleared from anywhere (the loupe's pill, the
            // menu), it must not keep a search that no longer applies (issue #1648)
            self.ui.search.clear();
        }
        if r.is_ok() && id == "search.semantic" {
            if params.get("clear").and_then(Value::as_bool) == Some(true) {
                if self.ui.semantic_search {
                    self.ui.search.clear();
                }
            } else if let Some(q) = &self.session.filter.semantic {
                self.ui.semantic_search = true;
                self.ui.search = q.clone();
            }
        }
        if r.is_ok() && id == "library.filter" && params.get("semantic").is_some() {
            if let Some(q) = &self.session.filter.semantic {
                self.ui.semantic_search = true;
                if self.ui.search.trim() != q {
                    self.ui.search = q.clone();
                }
            } else if self.ui.semantic_search && params.get("semantic").is_some_and(Value::is_null) {
                self.ui.search.clear();
            }
        }
        if r.is_ok() && id == "mask.addComponent" && matches!(self.ui.tool.as_str(), "colorRange" | "brush" | "object") {
            // The eyedropper, the brush and the object tool take every press on the photo, so the handles of a
            // gradient (or any other shape) added meanwhile would be dead: disarm them. A component of the
            // tool's own kind keeps it (Pick, another colour range; Brush, Brush; Object, another object). An
            // omitted kind is a brush, as in the engine.
            let kind = params.get("kind").and_then(Value::as_str).unwrap_or("brush");
            if kind != self.ui.tool {
                self.ui.tool.clear();
            }
        }
        if r.is_ok() && id == "mask.brushStroke" && self.ui.mask_overlay_auto {
            // a released stroke leaves what the mask now covers on show, so the painted area can be judged
            // (#1042); the next mask slider hides it again (`mask.adjust` below)
            self.ui.mask_overlay = true;
        }
        if r.is_ok() && id == "mask.adjust" {
            // Judge local adjustments on the photo: while a slider is at work nothing covers it, not even
            // an overlay switched on with O (it comes back when the slider rests); one that showed by
            // itself goes, as the mask now has settings (upstream PR 2920's overlay rule).
            if self.ui.mask_overlay_auto {
                self.ui.mask_adjusting_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(MASK_ADJUST_HIDE_MS));
            }
            self.ui.mask_overlay = self.ui.mask_overlay_pinned;
        }
        if r.is_ok() && matches!(id, "mask.add" | "mask.addComponent" | "mask.duplicate" | "mask.select") {
            // A new or picked mask shows its overlay by itself while it has no settings; one with
            // settings is judged by its effect (O, the eye or hovering its row shows the overlay),
            // but still peeks it for a moment so the user sees which mask is on (issue #1109).
            self.mask_pick_overlay();
        }
        if r.is_ok() {
            // These commands finish synchronously, so their completion would otherwise
            // be invisible once the activity stack is gone. Never show success on error.
            let completed = match id {
                "library.compact" => Some(format!("✓ {}", i18n::tr("Optimize Catalog"))),
                // from File ▸ Libraries or Library Settings
                "catalog.backup" => r.as_ref().ok().map(|v| format!("{} {}", i18n::tr("Catalog backed up to"), v["backup"].as_str().unwrap_or(""))),
                "catalog.checkIntegrity" if r.as_ref().is_ok_and(|v| v["ok"] == true) => {
                    Some(i18n::tr("The catalog passed the integrity test").to_string())
                }
                "catalog.checkIntegrity" => Some(i18n::tr("The catalog failed the integrity test: back it up and optimize it").to_string()),
                "catalog.optimize" => Some(i18n::tr("Catalog optimized").to_string()),
                "library.clearPreviews" => Some(format!("✓ {}", i18n::tr("Discard Preview Cache"))),
                // a build asked for while another runs waits for its turn (the menu stays open
                // for the next one); the row of the running build is already on the stack
                "library.buildPreviews" | "library.smartPreviews" if r.as_ref().is_ok_and(|v| v["queued"] == true) => {
                    Some(i18n::tr("Queued: it starts when the running preview build finishes").to_string())
                }
                "develop.autoSync" => Some(format!("{}: {}", i18n::tr("Auto Sync"), i18n::tr(if self.session.auto_sync { "On" } else { "Off" }))),
                // a vignetting calibration left off photos without a fitting reference (Apply, Sync, Paste)
                "vignettingCalibration.apply" | "develop.sync" | "develop.paste" => {
                    let skipped = r
                        .as_ref()
                        .ok()
                        .and_then(|v| v.get("skipped").or_else(|| v.get("vignettingCalibrationSkipped")))
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    match skipped {
                        0 => None,
                        1 => Some(
                            i18n::tr("Vignetting calibration not applied to 1 photo: no reference for its camera, lens and aperture").to_string(),
                        ),
                        n => Some(i18n::tr_format!(
                            "Vignetting calibration not applied to {n} photos: no reference for their camera, lens and aperture",
                            n = n
                        )),
                    }
                }
                _ => None,
            };
            if let Some(message) = completed {
                // Menu clicks can arrive before the next frame updates last_time.
                let now = self.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(self.last_time);
                self.ui.show_toast((message, now + TOAST_SECONDS, None));
            }
        }
        if r.is_ok() && matches!(id, "photo.label" | "photo.clearLabel") {
            let label = if let Some(applied) = r.as_ref().ok().and_then(|v| v.get("label")).and_then(Value::as_str) {
                if applied == "none" { None } else { lcc_catalog::ColorLabel::parse(applied) }
            } else {
                params.get("label").and_then(Value::as_str).filter(|s| *s != "none").and_then(lcc_catalog::ColorLabel::parse)
            };
            let text = match label {
                Some(label) => {
                    let name =
                        self.session.catalog.custom_label_name(label).map(str::to_owned).unwrap_or_else(|| i18n::tr(&format!("{label:?}")).into());
                    i18n::tr_format!("{name} Label", name = name)
                }
                None => i18n::tr("Color label cleared").into(),
            };
            let text = cull_toast::with_count(text, cull_toast::targets(self, &params).len());
            // Native menu clicks can arrive before logic() updates last_time after an idle gap.
            let now = self.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(self.last_time);
            self.ui.show_toast((text, now + TOAST_SECONDS, label));
        }
        if r.is_ok()
            && cull_toast::confirms(id)
            && !matches!(id, "photo.label" | "photo.clearLabel")
            && !(cull_toast::is_flag(id) && shortcuts::overlay_reports_changes(self))
        {
            // every rating and flag change confirms, with the count when several photos changed
            if let Some(text) = cull_toast::feedback(self, id, &cull_toast::targets(self, &params)) {
                let now = self.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(self.last_time);
                self.ui.show_toast((text, now + TOAST_SECONDS, None));
            }
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
            Ok(v) if matches!(id, "faces.detect" | "faces.clearAndRescan") && v["done"] == false => {
                let n = v["photos"].as_u64().unwrap_or(0);
                self.ui.status = if n == 1 {
                    crate::i18n::tr("Detecting faces…").to_string()
                } else {
                    crate::i18n::tr_format!("Detecting faces in {n} photos…", n = n)
                };
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
        if d.crop != lcc_develop::Crop::default() {
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
        if self.session.interaction.is_some() {
            self.store_shown_crop_lock();
        }
        Ok(())
    }

    /// Before an edit of an untouched photo's crop (a drag, the angle, Straighten, Auto, a flip):
    /// store the ratio the tool shows locked into the crop itself. Until then the lock is only
    /// shown (`crop_tool_aspect`), and the edit would leave the crop touched with no lock, which
    /// reads as unlocked (nothing but the lock button and A unlocks it). Returns whether it
    /// stored one (one undo step of its own outside an interaction, which the caller merges with
    /// the edit). Only this photo: Auto Sync carries the edit, not a ratio fitted to this
    /// photo's shape.
    pub(crate) fn store_shown_crop_lock(&mut self) -> bool {
        // nothing chosen yet: the photo's own shape, as `crop_tool_aspect` shows it
        let aspect = self.ui.crop_default_aspect.clone().unwrap_or_else(|| serde_json::json!("original"));
        let untouched = self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lcc_develop::Crop::default());
        if !untouched || aspect == serde_json::json!("free") {
            return false;
        }
        let sync = std::mem::replace(&mut self.session.auto_sync, false);
        // the remembered ratio, turned like the new photo's crop (as a named ratio is)
        let r = self.session.execute("crop.aspect", &serde_json::json!({"aspect": aspect, "orientation": "crop"}));
        self.session.auto_sync = sync;
        match r {
            Ok(_) => true,
            Err(e) => {
                // A remembered value the command refuses (a hand-edited settings file) would refuse
                // every crop edit on an untouched photo: forget it and let this edit go ahead.
                self.ui.crop_default_aspect = None;
                self.ui.status = e.to_string();
                false
            }
        }
    }

    /// Show a transient toast at the bottom of the canvas (like the reference app's HUD).
    /// Advance a running slideshow.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        panels::slideshow::tick(self, ctx);
    }

    /// Leave the full-screen preview: a view or module switch asks for the app's chrome back (Esc
    /// and F keep their own paths). A running slide show ends with the preview, as Esc ends it
    /// (releasing its rendered slides).
    pub(crate) fn leave_fullscreen_preview(&mut self) {
        if self.ui.slideshow.is_some() {
            panels::slideshow::end(self);
        }
        self.ui.fullscreen = false;
    }

    /// Announce the end of a Build Previews run.
    fn preview_build_status(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let b = self.session.preview_build.clone();
        // a queued preview build starts once no build that was asked for is running — it preempts
        // the background preparation's, as a build asked for from the menu does — but never on
        // the frame that says the last one is done: starting a build is the 1.3 s job-making of
        // a big library, and that frame has just painted
        let announcing = b
            .as_ref()
            .is_some_and(|b| b.finished.load(Ordering::Relaxed) && self.ui.preview_build_seen != Some((std::sync::Arc::as_ptr(b) as usize, true)));
        if !announcing && !self.session.preview_queue.is_empty() && !self.session.preview_build.as_ref().is_some_and(|b| b.running() && !b.background)
        {
            self.preview_queue_next();
        }
        let Some(b) = b else { return };
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
                // the frame after this one starts the next queued build, if any
                if !self.session.preview_queue.is_empty() {
                    ctx.request_repaint();
                }
            }
        } else {
            // The activity stack shows the run while it works; poll for the end.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Start the next preview build queued behind one that has ended ([`Session::preview_queue`]);
    /// one that no longer can start says so in the status line, as its requester would have seen.
    fn preview_queue_next(&mut self) {
        if let Some(Err(e)) = lcc_engine::cmd::previews::drain_queue(&mut self.session) {
            self.ui.status = e.to_string();
        }
    }

    /// Announce when saving the library starts failing (changes then live only in memory and are
    /// retried) and when it works again; the top bar's cloud icon shows the state meanwhile.
    fn save_status(&mut self, ctx: &egui::Context) {
        let unsaved = self.session.unsaved().map(|(n, e)| (n, e.to_string()));
        match (unsaved, self.ui.unsaved_seen) {
            (Some((n, e)), false) => {
                self.ui.unsaved_seen = true;
                let t = ctx.input(|i| i.time);
                self.ui.show_toast((
                    crate::i18n::tr_format!(
                        "{n} change{} saved in memory but not written to disk: {e}. {app} will retry",
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
                self.toast(ctx, crate::i18n::tr("Catalog saved"));
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

    /// Whether a rating, flag or label moves on to the next photo: Photo ▸ Auto Advance, or Caps
    /// Lock while it is on (as in other photo libraries).
    pub fn auto_advance_on(&self) -> bool {
        self.ui.auto_advance || self.ui.caps_lock
    }

    /// A toast with a button that runs `command` with `params` (Undo after Update Preset…), for
    /// `secs` seconds; a click runs it once and the toast goes.
    pub fn toast_with_action(
        &mut self,
        ctx: &egui::Context,
        text: impl Into<String>,
        button: impl Into<String>,
        command: &str,
        params: Value,
        secs: f64,
    ) {
        let until = ctx.input(|i| i.time) + secs;
        self.ui.toast_action = Some(state::ToastAction { text: text.into(), button: button.into(), command: command.to_string(), params, until });
        ctx.request_repaint();
    }

    /// A toast that stays `secs` seconds (messages that say where to look or what to do next).
    pub fn toast_for(&mut self, ctx: &egui::Context, text: impl Into<String>, secs: f64) {
        let t = ctx.input(|i| i.time);
        self.ui.show_toast((text.into(), t + secs, None));
    }

    /// Reverse geocoding: say how a place-names download ended (a failure stays readable, with what to do).
    fn place_names_outcome(&mut self, ctx: &egui::Context) {
        use lcc_engine::cmd::geocode::DownloadOutcome;
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
    fn ai_mask_detail(&mut self, ctx: &egui::Context) {
        // Upright worked out in the background for photos an automatic mode reached by sync or paste
        if self.session.upright_poll() {
            ctx.request_repaint();
        }
        if self.session.upright_jobs.pending() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let polled = self.session.segment_poll();
        let now = ctx.input(|i| i.time);
        if polled.changed {
            ctx.request_repaint();
        }
        if let Some(mask) = polled.refine {
            // a new selection from a click or a description: shown by the same rule as a new mask
            self.mask_pick_overlay();
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
        if lcc_engine::segment::Segmenter::AVAILABLE && e.kind == ErrorKind::ModelMissing(AiModel::Sam3) {
            self.offer_sam_download(then);
        } else if lcc_engine::segment::Segmenter::AVAILABLE
            && matches!(
                e.kind,
                ErrorKind::ModelMissing(
                    AiModel::Sam2 | AiModel::Describe | AiModel::People | AiModel::FaceDetector | AiModel::Scene | AiModel::Depth
                )
            )
        {
            // SAM 2.1 was chosen for Object masks, Describe needs the open models, or Retouch needs the MediaPipe models: their downloads are in the
            // AI Models dialog
            if self.ui.dialog.is_none() {
                self.ui.dialog = Some(state::Dialog::Settings { tab: "ai".into() });
            }
        } else {
            self.toast_error(ctx, e.message);
        }
    }

    /// Open the dialog offering the SAM 3 download (never downloads by itself).
    pub fn offer_sam_download(&mut self, then: Option<(&str, &str)>) {
        // the download is on Settings ▸ AI Models, with every other model
        let _ = then;
        if self.ui.dialog.is_none() || matches!(self.ui.dialog, Some(state::Dialog::Settings { .. })) {
            self.ui.dialog = Some(state::Dialog::Settings { tab: "ai".into() });
        }
    }

    /// Detect Faces running in the background (`faces.detect`): when it ends, its faces are in the photos (one undo
    /// step) and its summary is shown. Cheap every frame.
    fn faces_detect_pump(&mut self) {
        let Ok(v) = self.session.execute("faces.detectPump", &serde_json::json!({})) else { return };
        if v.get("active").and_then(Value::as_bool) == Some(true) {
            if let Some(ctx) = self.tasks.repaint.as_ref() {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            return;
        }
        if v.get("done").and_then(Value::as_bool) != Some(true) {
            return;
        }
        self.caches.faces_epoch += 1;
        let text = detect_summary(&v);
        let now = self.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(self.last_time);
        self.ui.show_toast((text, now + TOAST_SECONDS, None));
    }

    /// A toast for an error the user has to read and act on (stays 6 s).
    pub fn toast_error(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let t = ctx.input(|i| i.time);
        self.ui.show_toast((text.into(), t + 6.0, None));
    }

    /// Apply queued MIDI actions. A turn of one knob or fader is one undo step: the interaction opens
    /// on its first event and closes after [`MIDI_IDLE`] seconds without one (they have no release).
    fn drain_midi(&mut self, ctx: &egui::Context) {
        use lcc_engine::midi::Action;

        let now = ctx.input(|i| i.time);
        if let Some(rx) = self.midi_rx.take() {
            // within a frame only the latest fader position and the sum of the knob ticks matter
            let mut actions: Vec<Action> = vec![];
            while let Ok(action) = rx.try_recv() {
                match (action, actions.last_mut()) {
                    (Action::Set { control, value }, Some(Action::Set { control: c, value: v })) if *c == control => *v = value,
                    (Action::Nudge { control, delta }, Some(Action::Nudge { control: c, delta: d })) if *c == control => *d += delta,
                    (action, _) => actions.push(action),
                }
            }
            self.midi_rx = Some(rx);
            for action in actions {
                match action {
                    Action::Command(id) => {
                        self.end_midi_interaction();
                        let _ = self.run(&id, serde_json::json!({}));
                    }
                    Action::Set { control, value } => {
                        let params = serde_json::json!({ "control": control, "value": value });
                        self.midi_edit(now, control, "develop.set", params);
                    }
                    Action::Nudge { control, delta } => {
                        let params = serde_json::json!({ "control": control, "delta": delta });
                        self.midi_edit(now, control, "develop.adjust", params);
                    }
                }
            }
        }
        if let Some((_, last)) = self.midi_active {
            if now - last >= MIDI_IDLE {
                self.end_midi_interaction();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(MIDI_IDLE - (now - last)));
            }
        }
    }

    /// End a Brush stroke being painted, if one is: it is dropped (strokes reach the photo when
    /// released), and the pointer still held paints nothing more until it is let go.
    pub(crate) fn cancel_brush_gesture(&mut self) {
        if let Some(panels::detail::Gesture::Brush { sent, .. }) = self.gesture {
            self.gesture = None;
            self.brush_live = None;
            // a stroke sent live keeps what it painted so far, as one undo step
            if sent > 0 {
                let _ = self.run("develop.endInteraction", serde_json::json!({}));
            }
        }
    }

    /// Close the open MIDI interaction, if any.
    pub(crate) fn end_midi_interaction(&mut self) {
        if self.midi_active.take().is_some() {
            let _ = self.run("develop.endInteraction", serde_json::json!({}));
        }
    }

    /// One edit of `control` from a MIDI controller, inside that control's interaction (one undo step).
    /// Nothing happens without an active photo.
    fn midi_edit(&mut self, now: f64, control: String, command: &str, params: Value) {
        // a mouse drag owns the engine's interaction slot until it ends
        if self.session.active().is_none() || self.ui.dragging_control.is_some() {
            return;
        }
        // the engine's interaction was ended elsewhere: this turn no longer owns anything
        if self.session.interaction.is_none() {
            self.midi_active = None;
        }
        if self.midi_active.as_ref().is_some_and(|(c, _)| *c != control) {
            self.end_midi_interaction();
        }
        if self.midi_active.is_none() {
            let label = lcc_develop::controls::find(&control).map_or("Edit", |c| c.label);
            if self.run("develop.beginInteraction", serde_json::json!({ "label": label })).is_err() {
                return;
            }
            self.midi_active = Some((control.clone(), now));
        }
        if self.run(command, params).is_ok() {
            self.midi_active = Some((control, now));
        }
    }

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            // a request its sender gave up on (the window drew nothing for a while) is not run
            // late: it would happen twice once the sender retries (issue #718)
            if !req.take() {
                continue;
            }
            let reply = req.reply.clone();
            self.note_ran(&req.method);
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(mut v) => {
                    // the UI's own commands don't wait for the catalog log's thread; a reply
                    // to someone outside does: "done" only for what is on disk
                    if let Err(e) = self.session.flush_catalog()
                        && let Some(o) = v.as_object_mut()
                    {
                        o.insert("notSaved".into(), serde_json::Value::String(e.to_string()));
                    }
                    let _ = reply.send(v);
                }
                control::Outcome::Screenshot { path, headless } => {
                    self.screenshot_token += 1;
                    let now = now_ms();
                    self.pending_screenshots.push(PendingShot {
                        token: self.screenshot_token,
                        path,
                        reply,
                        headless: headless && !self.headless_host,
                        not_before: now + 120.0,
                        deadline: now + SCREENSHOT_SETTLE_MS,
                        frames: 0,
                        sent_at: None,
                    });
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// Advance pending screenshots: wait (≥ 3 frames, ≥ 120 ms) until no renders are in flight
    /// (or a timeout), then capture — via the host's compositor, or headlessly
    /// ([`Self::headless_screenshot`]) when asked or when the compositor delivers nothing.
    fn issue_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = now_ms();
        let busy = self.renderer.in_flight() > 0 || self.merge.busy();
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut shadow_ticked = false;
        shots.retain_mut(|s| {
            if let Some(sent) = s.sent_at {
                if now - sent < SCREENSHOT_FALLBACK_MS || self.headless_host {
                    return true;
                }
                // The compositor delivered nothing (display asleep, window occluded): go headless.
                log::warn!("ui.screenshot: no frame from the compositor after {SCREENSHOT_FALLBACK_MS} ms; rendering headlessly");
                s.sent_at = None;
                s.headless = true;
                s.deadline = now + SCREENSHOT_SETTLE_MS;
            }
            s.frames += 1;
            let ready = now >= s.not_before && s.frames >= 3 && (!busy || now > s.deadline);
            if s.headless {
                // The shadow frame requests the renders the UI needs, even while the window shows nothing.
                let img = if ready || !shadow_ticked { self.headless_screenshot(ctx, ready) } else { None };
                shadow_ticked = true;
                if let Some(img) = img {
                    let _ = s.reply.send(control::save_screenshot(self, &img, s.path.as_deref()));
                    return false;
                }
                true
            } else {
                if ready {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(s.token)));
                    s.sent_at = Some(now);
                }
                true
            }
        });
        shots.append(&mut self.pending_screenshots);
        self.pending_screenshots = shots;
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
        theme::set_ui_font_size(ctx, self.ui.settings.ui_font_size);
        theme::set_slider_layout(ctx, self.slider_layout());
        theme::set_compact(ctx, self.compact());
        theme::set_slider_text(ctx, self.ui.settings.slider_text);
        theme::set_slider_control(ctx, self.ui.settings.slider_control);
        let wanted = self.ui.settings.theme(self.system_theme(ctx));
        if theme::current(ctx) != Some(wanted) {
            theme::apply_kind(ctx, wanted);
        }
        theme::refresh_window_on_first_focus(ctx);
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
        self.headless_screenshotting = true;
        view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |ui| self.ui(ui));
        let img = capture.then(|| {
            let mut tex = self.renderer.cpu_textures();
            if let (Some((t, ..)), Some(px)) = (&self.merge.preview, &self.merge.preview_pixels) {
                tex.insert(t.id(), crate::softpaint::CpuTexture::linear(px.clone()));
            }
            view.paint(&tex)
        });
        self.headless_screenshotting = false;
        self.shadow = Some(view);
        img
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|s| s.token == token) {
                let s = self.pending_screenshots.remove(i);
                let _ = s.reply.send(control::save_screenshot(self, &image, s.path.as_deref()));
            }
        }
    }

    /// The process is about to end (settings and library are saved by then): make sure nothing is
    /// inside the GPU driver when it does (issue #620: a render running on a worker thread while
    /// the process exits crashes in the driver). No GPU work starts from here on; the render
    /// workers get up to `timeout` to finish their jobs, and what is left of it goes to GPU work
    /// on other threads (export, denoise, the device warm-up). `false`: the deadline passed first
    /// (the host exits anyway). The app renders nothing afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        lcc_engine::gpu::begin_shutdown();
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        let stopped = self.renderer.shutdown(timeout);
        #[cfg(not(target_arch = "wasm32"))]
        let timeout = timeout.saturating_sub(t0.elapsed());
        let idle = lcc_engine::gpu::wait_idle(timeout);
        stopped && idle
    }

    /// Whether the GPU of the HDR output has room for zoomed views' recent HDR windows on this
    /// screen (`None`: no HDR output, nothing to keep).
    pub(crate) fn hdr_cache_fit(&mut self, ctx: &egui::Context) -> Option<render::HdrCacheFit> {
        let have = self.services.hdr_output.as_mut()?.gpu_memory();
        let content = ctx.content_rect().size();
        let points = content.max(ctx.input(|i| i.viewport().monitor_size).unwrap_or(content));
        let ppp = ctx.pixels_per_point();
        Some(render::hdr_cache_fit([points.x * ppp, points.y * ppp], have))
    }

    /// Per-frame logic before layout (control channel, renders, shortcuts, drops).
    pub fn logic(&mut self, ctx: &egui::Context) {
        i18n::set_language(self.ui.language);
        self.frame_log.clear();
        // painting on the loupe (OneSlips, NOTES-1028 note 29): a brush stroke in progress and a moment after it
        let now = ctx.input(|i| i.time);
        let painting = matches!(self.gesture, Some(panels::detail::Gesture::Brush { .. }));
        if painting {
            self.paint_hold_until = now + panels::detail::PAINT_HOLD_SECS;
        }
        // (a clock that went back never holds for longer than the hold)
        self.paint_hold = painting || (now < self.paint_hold_until && self.paint_hold_until - now <= 2.0 * panels::detail::PAINT_HOLD_SECS);
        if self.paint_hold {
            ctx.request_repaint_after(std::time::Duration::try_from_secs_f64((self.paint_hold_until - now).clamp(0.0, 2.0)).unwrap_or_default());
        }
        widgets::sync_section_folds(ctx, &mut self.ui.section_open);
        let t0 = now_ms();
        panels::library_problem::logic(self, ctx);
        // the display can switch between SDR and HDR (OS setting, another monitor) at any time
        if let Some(h) = self.services.hdr_output.as_mut() {
            self.hdr_headroom = h.headroom();
            self.hdr_display_sdr = self.hdr_headroom.is_none();
        }
        // zoomed HDR views keep their recent windows only where the GPU has room for them, and
        // the textures of windows nothing shows or keeps any more are freed at once
        let cache = self.ui.settings.hdr_zoom_cache && self.hdr_cache_fit(ctx).is_none_or(|f| f.fits());
        self.renderer.set_hdr_cache(cache);
        if let Some(h) = self.services.hdr_output.as_mut() {
            for slot in self.renderer.hdr_unused_slots() {
                h.release(slot);
            }
        }
        self.logic_inner(ctx);
        self.perf.logic_ms = now_ms() - t0;
    }

    fn logic_inner(&mut self, ctx: &egui::Context) {
        theme::set_ui_font_size(ctx, self.ui.settings.ui_font_size);
        theme::set_slider_layout(ctx, self.slider_layout());
        theme::set_compact(ctx, self.compact());
        self.follow_compact_width(ctx);
        theme::set_slider_text(ctx, self.ui.settings.slider_text);
        theme::set_slider_control(ctx, self.ui.settings.slider_control);
        if !self.styled {
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            theme::apply(ctx);
            // File → Add from Device lists cards scanned in the background: show hot-plugs
            let repaint = ctx.clone();
            lcc_engine::devices::on_change(move || repaint.request_repaint());
            // "is the original there?" (grid, Info panel, Missing Photos) answers from a cache a
            // worker fills: a sleeping NAS or a dropped share never stalls a frame
            let repaint = ctx.clone();
            self.session.media.availability.run_in_background(std::sync::Arc::new(move || repaint.request_repaint()));
            // where each raw sits on its sensor (flat-field matching) is read from its header by a worker too
            let repaint = ctx.clone();
            self.session.vignetting_calibration_placements.run_in_background(std::sync::Arc::new(move || repaint.request_repaint()));
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
        self.sync_theme(ctx);
        panels::notices::logic(self);
        panels::cells::auto_read(self);
        // closing the window (or Quit) with changes only in memory: retry, else ask first
        if ctx.input(|i| i.viewport().close_requested()) && !panels::notices::may_close(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        // (once a frame: a pass drawn again after a discard has no input of its own, and still the
        // widgets of the pass thrown away, where a request would find them out of place)
        if ctx.current_pass_index() == 0 {
            let now = ctx.input(|i| i.time);
            let dt = now - self.last_time;
            if dt > 0.0 {
                self.perf.fps = self.perf.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
            }
            // the time between this frame and the last one while the window had work to show (a
            // render on its way, input to answer): what a freeze outside `update` (painting,
            // presenting behind GPU work) adds shows here and not in `update_ms`
            let busy = self.renderer.in_flight() > 0 || self.renderer.queued() > 0 || ctx.input(|i| !i.events.is_empty());
            if self.last_time > 0.0 && self.perf.was_busy && dt.is_finite() && dt > 0.0 {
                self.perf.gap_ms = dt * 1000.0;
                if self.perf.gap_ms > self.perf.max_gap_ms {
                    self.perf.max_gap_ms = self.perf.gap_ms;
                    self.perf.max_gap_host_ms = self.perf.host_frame_ms;
                }
            }
            self.perf.was_busy = busy;
            self.last_time = now;
            let mut lap = now_ms();
            self.drain_control(ctx);
            self.lap(&mut lap, "logic.control");
            self.drain_midi(ctx);
            surface::tick(self, ctx);
            view_state::track(self);
        }
        self.apply_settings(ctx);
        // input this frame: batch work leaves the editing card to the interface for a moment
        // (only where the app tracks it: `gpu::track_interaction`)
        if ctx.input(|i| !i.events.is_empty() || i.pointer.any_down()) {
            lcc_engine::gpu::note_interaction();
        }
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        }
        // (lapped: a slow frame's log names what cost it)
        let mut lap = now_ms();
        self.renderer.poll(ctx, &mut self.session);
        self.lap(&mut lap, "logic.renders");
        merge::poll(self, ctx);
        import::poll_scan(self, ctx);
        self.lap(&mut lap, "logic.folderRead");
        sync::poll(self, ctx);
        sync::poll_run(self, ctx);
        import::tick(self, ctx);
        lightroom_import::tick(self, ctx);
        recheck::tick(self);
        read_originals::tick(self);
        tasks::poll(self, ctx);
        self.lap(&mut lap, "logic.tasks");
        #[cfg(not(target_arch = "wasm32"))]
        panels::update::poll(self, ctx);
        self.preview_build_status(ctx);
        self.save_status(ctx);
        self.slideshow_tick(ctx);
        self.lap(&mut lap, "logic.status");
        panels::faces::pump(self, ctx);
        self.lap(&mut lap, "logic.faces");
        #[cfg(not(target_arch = "wasm32"))]
        panels::connections::pump(self, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::peer_sync::pump(self, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::peer_sync::dialogs(self, ctx);
        // back from an external editor: pick up the files it saved
        let focused = ctx.input(|i| i.focused);
        if focused && !self.ui.was_focused && !self.ui.external_edits.is_empty() {
            let ids = self.ui.external_edits.clone();
            if let Ok(r) = self.session.execute("photo.reload", &serde_json::json!({"ids": ids}))
                && r.get("reloaded").and_then(Value::as_array).is_some_and(|a| !a.is_empty())
            {
                self.toast(ctx, crate::i18n::tr("Updated the edits saved in the external editor"));
            }
        }
        folder_poll::tick(self, ctx, focused && !self.ui.was_focused);
        camera_roll::watch_tick(self, ctx, focused && !self.ui.was_focused);
        if let Some(url) = self.pending_link.take() {
            folder_poll::follow_link(self, ctx, &url);
        }
        self.ui.was_focused = focused;
        // auto import: list the watched folder every few seconds (on a worker thread: it may be on
        // a network share), then import what's new like any import (also on a worker thread)
        // a folder that couldn't be read stops being one once another is watched (or none)
        if self.ui.auto_import_error.as_ref().is_some_and(|(f, _)| self.session.import_defaults.auto_active_folder() != Some(f)) {
            self.ui.auto_import_error = None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(folder) = self.session.import_defaults.auto_active_folder().cloned() {
            const LABEL: &str = "Auto Import";
            let now = ctx.input(|i| i.time);
            if now - self.ui.auto_import_at >= 3.0
                && !import::running(self)
                && self.sync_run.is_none()
                && self.lightroom.is_none()
                && !self.tasks.is_running(LABEL)
            {
                self.ui.auto_import_at = now;
                let scope = self.session.auto_import_scope();
                let work = move || match scope {
                    Some(scope) => scope.list(),
                    None => Ok(Vec::new()),
                };
                let done = move |app: &mut LccApp, _ctx: &egui::Context, listing: Result<Vec<(String, u64)>, String>| {
                    // the watched folder changed while this one was listed: its result is for nothing on screen
                    if app.session.import_defaults.auto_active_folder() != Some(&folder) {
                        return;
                    }
                    let listing = match listing {
                        Ok(l) => {
                            if app.ui.auto_import_error.as_ref().is_some_and(|(f, _)| *f == folder) {
                                app.ui.auto_import_error = None;
                                log::info!("auto import: the watched folder can be read again: {folder}");
                            }
                            l
                        }
                        // removed, renamed, unmounted or not allowed: said once, not every few seconds
                        Err(e) => {
                            let error = Some((folder, e));
                            if app.ui.auto_import_error != error {
                                if let Some((_, e)) = &error {
                                    log::warn!("auto import: can't read the watched folder {e}");
                                }
                                app.ui.auto_import_error = error;
                            }
                            return;
                        }
                    };
                    // an import, a scan (a finished Local browse replaces `app.import`), a sync or a Lightroom
                    // migration that started while the folder was listed: scanning now would mark the new files as
                    // tried without importing them; the next listing picks them up
                    if import::running(app) || app.sync_run.is_some() || app.lightroom.is_some() {
                        return;
                    }
                    let p = serde_json::json!({"listing": listing, "start": false});
                    let Ok(r) = app.session.execute("library.autoImportScan", &p) else { return };
                    import::auto_import_take(app, &r);
                };
                // listed every few seconds: quiet, no row in the activity stack
                if let Err(e) = tasks::spawn(self, LABEL, None, work, done) {
                    log::warn!("{e}");
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
        #[cfg(not(target_arch = "wasm32"))]
        panels::tether_bar::show(self, ctx);
        // automatic XMP writes that failed (a network share offline): tried again every few seconds once their files
        // are reachable, and said once when they start waiting and once when they're written
        #[cfg(not(target_arch = "wasm32"))]
        {
            // the automatic sidecar writes made in the background since the last frame
            self.session.poll_sidecar_writes();
            if self.session.sidecar_writes_pending() > 0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            if self.session.unsaved_sidecars() > 0 {
                let now = ctx.input(|i| i.time);
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
                        "The XMP sidecars of {n} photos couldn't be written (is their folder offline?). {app} keeps trying.",
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
            if let Some(now) = lcc_engine::devices::devices_scanned() {
                import::cards_seen(self, now);
            }
            ctx.request_repaint_after(lcc_engine::devices::MAX_AGE);
        } else {
            self.ui.known_cards = None;
        }
        self.session.persist_if_dirty();
        // an import being finished: kept in the library until done, taken up again after a restart
        if self.session.finishing_tick() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        panels::denoise::pump(self, ctx);
        panels::plugin_filters::pump(self, ctx);
        quality_scan::pump(self, ctx);
        panels::right::dust_pump(self, ctx);
        panels::faces::pump(self, ctx);
        focus_scan::pump(self, ctx);
        eye_strip::reject_pump(self);
        self.faces_detect_pump();
        panels::masking::put_tools_away(self);
        enhance_pump::pump(self, ctx);
        self.collect_screenshots(ctx);
        self.issue_screenshots(ctx);
        edit_in::show(self, ctx);
        if self.fonts_ready {
            shortcuts::handle(self, ctx);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.handle_dropped_files(ctx);
    }

    /// Files dropped on the window: face models open their licence dialog, presets import as
    /// presets, photos and folders go to the import review (or straight in, Settings ▸ Import).
    #[cfg(not(target_arch = "wasm32"))]
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<String> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().to_string()).filter(|p| !p.is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        // a plug-in module dropped while the Plug-in Manager is open installs, not imports
        if panels::plugins::take_drop(self, ctx, &dropped) {
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
                self.toast(ctx, crate::i18n::tr("Open a catalog first, then drop the presets again"));
            } else {
                let _ = self.run("file.importPresets", serde_json::json!({"paths": drop.presets}));
            }
        }
        if drop.photos.is_empty() {
            return;
        }
        // no library open yet (the launch list, a library that failed to open): they wait for one
        let Some(photos) = panels::library_problem::hold_dropped(self, ctx, drop.photos) else { return };
        // the import review (File ▸ Import from Folder…'s scan, then the review), or straight in
        // on a worker thread (Settings ▸ Import)
        import::drop_paths(self, ctx, photos);
    }

    /// The window and the GPU's batch work (exports): a resize makes room for the window's own
    /// drawing first (`lcc_gpu::hold_batch`: the surface is reconfigured, which waits for
    /// the GPU); what happened to the GPU (it stopped responding, it was restarted) is told once.
    fn gpu_housekeeping(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect().size();
        if screen != self.last_screen {
            if self.last_screen != egui::Vec2::ZERO {
                lcc_engine::gpu::hold_batch(std::time::Duration::from_millis(800));
            }
            self.last_screen = screen;
        }
        #[cfg(not(target_arch = "wasm32"))]
        for e in lcc_engine::gpu::events_after(self.gpu_event_seen) {
            self.gpu_event_seen = self.gpu_event_seen.max(e.seq);
            let text = crate::i18n::tr(&e.text).to_string();
            match e.kind {
                lcc_engine::gpu::EventKind::Recovered => self.toast_for(ctx, text, 8.0),
                _ => self.toast_for(ctx, text, 15.0),
            }
        }
    }

    /// Apply settings that act outside the UI state: GPU rendering and window full screen.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        theme::set_ui_font_size(ctx, self.ui.settings.ui_font_size);
        theme::set_slider_layout(ctx, self.slider_layout());
        theme::set_compact(ctx, self.compact());
        theme::set_slider_text(ctx, self.ui.settings.slider_text);
        theme::set_slider_control(ctx, self.ui.settings.slider_control);
        // the cards first: the device warmed up below is made on the chosen one
        let cards = (self.ui.settings.gpu_card.clone(), self.ui.settings.gpu_helper.clone());
        if self.gpu_cards_applied.as_ref() != Some(&cards) {
            #[cfg(not(target_arch = "wasm32"))]
            {
                lcc_engine::gpu::set_card(&cards.0);
                lcc_engine::gpu::set_helper(&cards.1);
            }
            self.gpu_cards_applied = Some(cards);
        }
        let ml_device = self.ui.settings.ml_device.clone();
        if self.ml_device_applied.is_none() && ml_device.is_empty() {
            if let Ok(st) = self.session.execute("ml.status", &serde_json::json!({})) {
                let adopted = st
                    .get("deviceChoice")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
                    .unwrap_or("")
                    .to_string();
                if !adopted.is_empty() {
                    self.ui.settings.ml_device = adopted.clone();
                }
                self.ml_device_applied = Some(adopted);
            }
        } else if self.ml_device_applied.as_ref() != Some(&ml_device)
            && self
                .session
                .execute(
                    "ml.status",
                    &serde_json::json!({
                        "device": ml_device,
                    }),
                )
                .is_ok()
        {
            self.ml_device_applied = Some(ml_device);
        }
        if self.gpu_applied != Some(self.ui.settings.gpu) {
            self.gpu_applied = Some(self.ui.settings.gpu);
            // not the `app.gpu` command: its answer (`available`, `adapter`) waits for the device, and
            // compiling the kernels takes seconds on DX12
            lcc_engine::gpu::set_enabled(self.ui.settings.gpu);
            // GPU device + kernels off the UI thread, once the window is up and only when GPU
            // rendering is on: a broken driver must not keep the window from appearing (issue #136)
            if self.ui.settings.gpu {
                lcc_engine::gpu::warm_up();
            }
        }
        // the SAM 3 model's folder: the default one until a person chooses another
        let sam3 = self.ui.settings.sam3_dir.clone();
        if self.sam3_applied.as_ref() != Some(&sam3) && (!sam3.is_empty() || self.sam3_applied.is_some()) {
            self.session.segmenter.dir = if sam3.is_empty() { lcc_engine::config::default_sam3_dir() } else { Some(std::path::PathBuf::from(&sam3)) };
        }
        self.sam3_applied = Some(sam3);
        let mb = self.ui.settings.memory_mb;
        // automatic at startup: leave the engine's default alone
        if self.memory_applied != Some(mb) && (mb > 0 || self.memory_applied.is_some()) {
            // 0 asks for the automatic budget itself (rounding it to whole MB here would set a budget
            // slightly below automatic)
            let _ = self.session.execute("app.memoryBudget", &serde_json::json!({"mb": mb}));
        }
        self.memory_applied = Some(mb);
        // export lanes and the GPU's share for exports (Settings ▸ Performance, "Test my machine")
        let st = &self.ui.settings;
        let tuning = (st.export_lanes, st.export_cpu_only, st.gpu_export_mb, st.gpu_export_jobs, st.export_cards);
        if self.export_tuning_applied != Some(tuning) {
            crate::machine_test::apply(st);
            self.export_tuning_applied = Some(tuning);
        }
        self.gpu_housekeeping(ctx);
        // the monitor profile (Settings ▸ Display): previews follow it from the next frame
        if self.display_applied.as_ref() != Some(&self.ui.settings.display_profile) {
            let path = self.ui.settings.display_profile.clone();
            match lcc_engine::display::Display::load_opt(&path) {
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
        // Full Screen Preview covers the whole screen: the window goes full screen with it and
        // comes back when it ends, unless it was already full screen (then it stays)
        match (self.ui.fullscreen, self.preview_window) {
            (true, None) => {
                let took = !self.window_is_fullscreen;
                self.preview_window = Some(took);
                if took && self.ui.window_fullscreen.is_none() {
                    self.ui.window_fullscreen = Some(true);
                }
            }
            (false, Some(took)) => {
                self.preview_window = None;
                if took && self.ui.window_fullscreen.is_none() {
                    self.ui.window_fullscreen = Some(false);
                }
            }
            _ => {}
        }
        if let Some(on) = self.ui.window_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
            self.window_is_fullscreen = on;
        }
    }

    /// While no display profile is chosen, show previews through the profile of the screen the
    /// window is on (`services.screen_profile`), read about once a second so moving the window to
    /// another screen is followed. Without the service (or a profile) the display is sRGB.
    fn follow_screen_profile(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
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
        let display = icc.as_deref().and_then(|bytes| match lcc_engine::display::Display::from_bytes(SCREEN_PROFILE, bytes) {
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

    /// Inject synthetic events (pointer events one per frame; key sequences up to the release),
    /// and (Modern layout) turn Back into `modern.back` and swipes into scrolling.
    pub fn raw_input_hook(&mut self, raw: &mut egui::RawInput) {
        modern::before_input(self, raw);
        self.synthetic_input(raw);
        // put back button releases the window integration dropped (OneSlips)
        self.pointer_repair.repair(raw);
        self.take_nav_buttons(raw);
        drop_space_repeats(raw);
        modern::after_input(self, raw);
        view_state::mirror_input(self, raw);
    }

    /// Take the mouse's Back / Forward button events (`Extra1` / `Extra2`) out of `raw` and keep
    /// their presses for the next UI pass's shortcuts (issue #3303). egui treats any pressed
    /// button as the start of a click or drag: left in, Back pressed over a slider would set it
    /// to the pointer, on the photo navigation is about to switch to.
    fn take_nav_buttons(&mut self, raw: &mut egui::RawInput) {
        raw.events.retain(|event| match event {
            egui::Event::PointerButton { button: button @ (egui::PointerButton::Extra1 | egui::PointerButton::Extra2), pressed, .. } => {
                if *pressed {
                    self.nav_buttons.push(*button);
                }
                false
            }
            _ => true,
        });
    }

    fn synthetic_input(&mut self, raw: &mut egui::RawInput) {
        // the frame after a synthetic release / key: modifiers back up
        if std::mem::take(&mut self.synthetic_mods_release) && self.synthetic_mods != egui::Modifiers::NONE {
            self.synthetic_mods = egui::Modifiers::NONE;
            raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        }
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
        self.synthetic_frame = !self.synthetic.is_empty();
        if self.synthetic.is_empty() {
            if !self.user_events.is_empty() {
                let e = self.user_events.remove(0);
                if let egui::Event::PointerButton { pos, .. } = &e {
                    raw.events.push(egui::Event::PointerMoved(*pos));
                }
                raw.events.push(e);
            }
            shell::defer_tabs(self, raw);
            return;
        }
        let pointer =
            |e: &egui::Event| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. });
        let n = if self.synthetic.first().is_some_and(pointer) {
            1
        } else {
            // a key sequence up to its release; a key held down (no release yet) stops before the
            // pointer events that follow, which then still come one per frame
            let release = self.synthetic.iter().position(|e| matches!(e, egui::Event::Key { pressed: false, .. })).map(|i| i + 1);
            let next_pointer = self.synthetic.iter().position(pointer);
            match (release, next_pointer) {
                (Some(r), Some(q)) => r.min(q),
                (r, q) => r.or(q).unwrap_or(self.synthetic.len()),
            }
        };
        if let Some(egui::Event::PointerMoved(p) | egui::Event::PointerButton { pos: p, .. }) = self.synthetic.first() {
            raw.events.push(egui::Event::PointerMoved(*p));
        }
        // egui's `input.modifiers` follow `ModifiersChanged` events, not the modifiers carried by
        // pointer/key events: announce the injected events' modifiers (held from button down to
        // the frame after up), so ⌥-drag, ⇧-click … work through the control channel
        let carried = self.synthetic.iter().take(n).find_map(|e| match e {
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
        raw.events.extend(self.synthetic.drain(..n));
        shell::defer_tabs(self, raw);
    }

    /// Frame timings once layout is done (`t0`: when layout started).
    fn end_frame(&mut self, ctx: &egui::Context, t0: f64) {
        theme::set_ui_font_size(ctx, self.ui.settings.ui_font_size);
        theme::set_slider_layout(ctx, self.slider_layout());
        theme::set_compact(ctx, self.compact());
        theme::set_slider_text(ctx, self.ui.settings.slider_text);
        theme::set_slider_control(ctx, self.ui.settings.slider_control);
        self.text_focus = self.tasks.repaint.as_ref().is_some_and(|c| c.egui_wants_keyboard_input());
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
        self.perf.frame_ms = now_ms() - t0;
        self.perf.update_ms = self.perf.logic_ms + self.perf.frame_ms;
        self.perf.max_update_ms = self.perf.max_update_ms.max(self.perf.update_ms);
        self.note_slow_frame(ctx.cumulative_pass_nr());
    }

    /// Remember that `what` ran in this frame (see [`SlowFrame`]).
    pub(crate) fn note_ran(&mut self, what: &str) {
        if self.frame_log.len() < FRAME_LOG_LEN {
            self.frame_log.push(what.to_string());
        }
    }

    /// Name the part of the frame since `*t` (`what`, e.g. a panel) in the frame's log when it took
    /// [`SLOW_PANEL_MS`] or more, and restart the clock.
    pub(crate) fn lap(&mut self, t: &mut f64, what: &str) {
        let now = now_ms();
        let ms = now - *t;
        *t = now;
        if ms >= SLOW_PANEL_MS {
            self.note_ran(&format!("{what} {ms:.0} ms"));
        }
    }

    /// A frame of [`SLOW_FRAME_MS`] or more (after start-up, `pass` > 10: the first frames load
    /// fonts and open the library) is logged with what ran in it and kept if it is the slowest.
    fn note_slow_frame(&mut self, pass: u64) {
        if self.perf.update_ms < SLOW_FRAME_MS || pass <= 10 {
            return;
        }
        let ran = std::mem::take(&mut self.frame_log);
        log::warn!(
            "slow frame: {:.0} ms on the UI thread (logic {:.0} ms, layout {:.0} ms); ran: {}",
            self.perf.update_ms,
            self.perf.logic_ms,
            self.perf.frame_ms,
            if ran.is_empty() { "nothing named".to_string() } else { ran.join(", ") }
        );
        if self.slowest_frame.as_ref().is_none_or(|s| s.ms < self.perf.update_ms) {
            self.slowest_frame =
                Some(SlowFrame { ms: self.perf.update_ms, logic_ms: self.perf.logic_ms, layout_ms: self.perf.frame_ms, ran, at_ms: now_ms() });
        }
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
        self.gather_shared_presets(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
    }

    /// LR-PREF-SHARED: the libraries of File ▸ Open Recent Library join the shared store (once each;
    /// they are only read), and their named presets, backups' included, are gathered; says so when
    /// that added some.
    fn gather_shared_presets(&mut self, ctx: &egui::Context) {
        if !self.session.uses_app_store() {
            return;
        }
        let paths = self.ui.settings.recent_libraries.clone();
        let mut n = 0;
        match self.run("store.mergeLibraries", serde_json::json!({"paths": paths})) {
            Ok(r) => {
                n += r
                    .get("merged")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|m| m.get("presets").and_then(Value::as_u64).unwrap_or(0))
                    .sum::<u64>()
            }
            Err(e) => log::warn!("store: {e}"),
        }
        match self.run("store.gatherPresets", serde_json::json!({"paths": paths})) {
            Ok(r) => n += r.get("count").and_then(Value::as_u64).unwrap_or(0),
            Err(e) => log::warn!("store: {e}"),
        }
        if n > 0 {
            let msg = crate::i18n::tr_format!("{n} presets from your other catalogs were added.", n = n);
            self.toast(ctx, msg);
        }
        // an older install's presets (the app under an earlier name, on this computer) join too, once
        // each, with no step of the user's: File ▸ More Imports ▸ Import Presets from Older Installs
        // asks again
        // (its camera profiles too, bound to the bodies the older install started with them).
        // No older install on this computer: nothing to ask, and no error at startup (issue #965).
        if lcc_engine::app_store::older_install_stores().is_empty() {
            return;
        }
        if let Ok(r) = self.run("store.importOlderInstalls", serde_json::json!({})) {
            let sum = |key: &str| -> u64 {
                r.get("merged").and_then(Value::as_array).into_iter().flatten().map(|m| m.get(key).and_then(Value::as_u64).unwrap_or(0)).sum()
            };
            let (n, m) = (sum("presets"), sum("cameraProfiles"));
            let msg = match (n, m) {
                (0, 0) => None,
                (n, 0) => Some(crate::i18n::tr_format!("{n} presets from an older install were added.", n = n)),
                (0, m) => Some(crate::i18n::tr_format!("{m} camera profiles from an older install were added.", m = m)),
                (n, m) => Some(crate::i18n::tr_format!("{n} presets and {m} camera profiles from an older install were added.", n = n, m = m)),
            };
            if let Some(msg) = msg {
                self.toast(ctx, msg);
            }
        }
    }

    /// "<app> — <library name>" (just "<app>" for a library that isn't saved).
    pub fn window_title(&self) -> String {
        match panels::settings::open_library_name(self) {
            Some(name) => format!("{}: {name}", lcc_brand::DISPLAY_NAME),
            None => lcc_brand::DISPLAY_NAME.to_string(),
        }
    }

    /// The browser page's title: "<file name>[ •] — <app>" for the active photo (• while
    /// `unsaved`: changes not yet written to storage), else the window title. The web build has no
    /// window for `ViewportCommand::Title`; a host embedding it reads this title to tell which
    /// document a drop opened and whether it is saved (issue #2397).
    pub fn page_title(&self, unsaved: bool) -> String {
        let dot = if unsaved { " •" } else { "" };
        match self.session.active().and_then(|id| self.session.catalog.photo(id)) {
            Some(p) => format!("{}{dot}: {}", p.file_name, lcc_brand::DISPLAY_NAME),
            None => format!("{}{dot}", self.window_title()),
        }
    }

    /// Lay out the whole window.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        i18n::set_language(self.ui.language);
        let ctx = ui.ctx().clone();
        widgets::sync_section_folds(&ctx, &mut self.ui.section_open);
        shortcuts::set_active(self.ui.settings.keymap_set);
        motion::set(&ctx, self.ui.settings.reduce_motion);
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        panels::dialogs::prepare(self, ui);
        let t0 = now_ms();
        if std::mem::take(&mut self.ui.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.library_changed(&ctx);
        // (a tool panel opened some other way still remembers the panel it took the place of)
        self.ui.note_right();
        // a picker armed in another tab or panel than the one on show is put away
        self.ui.check_picker();
        // Auto Sync is remembered app-wide, however it was switched (menu, switch, console)
        self.ui.settings.auto_sync = self.session.auto_sync;
        // panels set it again this frame while the pointer rests on a preset or profile
        self.hover_preview = None;
        // …and the histogram sets this while the pointer rests on a clipping indicator
        self.clipping_preview = false;
        self.ai_mask_detail(&ctx);
        // (lapped like the panels: a slow frame's log names what cost it)
        let mut lap = now_ms();
        self.background_prep(&ctx);
        self.lap(&mut lap, "prep.tick");
        crate::panels::genfill::frame(self, &ctx);
        self.place_names_outcome(&ctx);
        if self.ui.fullscreen {
            // full-screen preview: the photo alone on black (the running tasks still say so,
            // quietly, top right — a slideshow plays undisturbed)
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ui, |ui| if self.ui.slideshow.is_some() { panels::slideshow::show(self, ui) } else { panels::detail::show(self, ui) });
            if self.ui.slideshow.is_none() {
                panels::activity::fullscreen(self, &ctx);
            }
            panels::second::show(self, &ctx);
            panels::notices::show(self, &ctx);
            panels::dialogs::show(self, &ctx);
            catalog_ui::show(self, &ctx);
            panels::library_problem::show(self, &ctx);
            pick::poll(self, &ctx);
            help_link::poll(self, &ctx);
            panels::surface_osd::show(self, &ctx);
            panels::presets::picker_frame(self, &ctx);
            panels::toast(self, &ctx);
            drop_overlay::show(self, &ctx);
            self.widgets = widgets::take_registry(&ctx);
            self.end_frame(&ctx, t0);
            return;
        }
        // Order matters: earlier panels take the full edge (top bar spans the window; the tool strip,
        // right panels and left panel run to the bottom; the bottom bar sits between them).
        let touch = modern::on(self);
        // the Import tab (Settings ▸ Import ▸ Import as a tab) takes the window under the top bar
        import_tab::keep_in_step(self);
        let importing = import_tab::showing(self);
        let mut lap = now_ms();
        let m = if importing && !touch {
            panels::topbar::show(self, ui);
            if module::edge_visible(self, module::Edge::Top) {
                module::module_bar(self, ui);
            }
            module::get(self.ui.module)
        } else if touch {
            modern::edges(self, ui, &ctx)
        } else {
            shell::edges(self, ui, &ctx)
        };
        let t = theme::Tokens::get(&ctx);
        let bg = if matches!(self.ui.view, state::ViewMode::Detail | state::ViewMode::Compare | state::ViewMode::Survey | state::ViewMode::Reference)
        {
            t.canvas
        } else {
            t.grid_bg
        };
        self.lap(&mut lap, "panel.edges");
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(bg)).show(ui, |ui| {
            if importing {
                let mut lap = now_ms();
                import_tab::show(self, ui);
                self.lap(&mut lap, "panel.import");
            } else if touch {
                modern::center(self, ui, m)
            } else {
                m.center(ui, self)
            }
        });
        let mut lap = now_ms();
        module::lights_out(self, &ctx);
        help_overlay::show(self, &ctx);
        panels::second::show(self, &ctx);
        panels::notices::show(self, &ctx);
        import_report::show(self, &ctx);
        duplicates_ui::show(self, &ctx);
        comments_ui::show(self, &ctx);
        panels::dialogs::show(self, &ctx);
        catalog_ui::show(self, &ctx);
        plate::show_setup(self, &ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::plugins::show(self, &ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::marketplace::show(self, &ctx);
        panels::library_problem::show(self, &ctx);
        camera_roll::prompt(self, &ctx);
        export_task::poll(self, &ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::plugins::poll(self, &ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panels::marketplace::poll(self, &ctx);
        render_dng_task::poll(self, &ctx);
        dng_update_task::poll(self, &ctx);
        machine_test::poll(self, &ctx);
        slideshow_export::poll(self, &ctx);
        panels::activity::show(self, &ctx);
        export_task::poll_photos(self, &ctx);
        pick::poll(self, &ctx);
        help_link::poll(self, &ctx);
        camera_roll::poll(self, &ctx);
        panels::grid::drag_feedback(self, &ctx);
        panels::left::album_drag_feedback(self, &ctx);
        panels::left::section_drag_feedback(self, &ctx);
        panels::keyword_list::drag_feedback(self, &ctx);
        panels::surface_osd::show(self, &ctx);
        panels::presets::picker_frame(self, &ctx);
        panels::toast(self, &ctx);
        drop_overlay::show(self, &ctx);
        self.lap(&mut lap, "panel.overlays");
        self.widgets = widgets::take_registry(&ctx);
        self.text_focus = ctx.egui_wants_keyboard_input();
        self.end_frame(&ctx, t0);
    }
}

/// How long a screenshot waits for in-flight renders.
const SCREENSHOT_SETTLE_MS: f64 = 3000.0;
/// How long a windowed screenshot waits for the compositor before falling back to headless.
const SCREENSHOT_FALLBACK_MS: f64 = 2000.0;

/// A `ui.screenshot` request in progress.
struct PendingShot {
    token: u64,
    path: Option<String>,
    reply: Sender<ControlResponse>,
    headless: bool,
    not_before: f64,
    deadline: f64,
    frames: u32,
    /// When the viewport screenshot command went out.
    sent_at: Option<f64>,
}

/// Wall-clock milliseconds since the Unix epoch (`web-time` maps to `Date.now()` on the web).
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
    pub settings: lcc_develop::DevelopSettings,
}

pub fn is_bw(d: &lcc_develop::DevelopSettings) -> bool {
    d.treatment == lcc_develop::Treatment::Bw || d.profile.id == "lc.mono" || d.profile.id.starts_with("lc.bw.")
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
fn sam_download_ended(download: &lcc_engine::segment::DownloadStatus) -> Option<(String, bool)> {
    match &download.error {
        Some(_) if download.cancelled => {
            Some((crate::i18n::tr("SAM 3 download stopped: it resumes where it left off next time.").to_string(), false))
        }
        Some(e) => Some((crate::i18n::tr_format!("The SAM 3 download failed: {e}", e = e), true)),
        None if download.finished => Some((crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready.").to_string(), true)),
        None => None,
    }
}

/// Whether command `id` edits the active photo's crop other than through its lock: the angle
/// (slider, Straighten tool, pointer rotation, Auto), a crop drag, a flip. Such an edit first
/// stores the lock the tool shows.
fn touches_crop(id: &str, params: &Value, active: Option<lcc_catalog::PhotoId>) -> bool {
    match id {
        // (a swap of the orientation too: X keeps the lock the tool shows, 3:2 → 2:3)
        "crop.straighten" | "crop.autoStraighten" | "crop.drag" | "crop.rotateAspect" => true,
        // a rect given outright is the shape asked for (no lock is honoured there); an angle alone turns the crop
        "crop.set" => params.get("rect").is_none_or(Value::is_null),
        // a flip of other photos leaves the active one alone
        "photo.flipHorizontal" | "photo.flipVertical" => match params.get("ids").and_then(Value::as_array) {
            Some(ids) => active.is_some_and(|a| ids.iter().any(|v| v.as_u64() == Some(a.0))),
            None => true,
        },
        _ if id.starts_with("develop.") => {
            let crop_control = |c: &str| c.starts_with("crop.");
            params.get("control").and_then(Value::as_str).is_some_and(crop_control)
                || params.get("values").and_then(Value::as_object).is_some_and(|o| o.keys().any(|k| crop_control(k)))
        }
        _ => false,
    }
}

/// Space is a modifier (held: the hand that pans; pressed: Toggle Zoom), like Shift or Alt, so
/// holding it never fires again and again: the key's auto-repeat presses are dropped before
/// anything sees them (shortcuts, a focused handle or button, the control channel), in the
/// desktop and the touch layout alike. Only the key events go: the typed text (`Event::Text`)
/// stays, so holding Space in a text box still types spaces.
pub(crate) fn drop_space_repeats(raw: &mut egui::RawInput) {
    raw.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Space, pressed: true, repeat: true, .. }));
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
    keyword_tree: Option<(u64, std::sync::Arc<Vec<lcc_catalog::KeywordNode>>)>,
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
    /// The folder tree, made at a catalog revision.
    folder_tree: Option<(u64, std::sync::Arc<Vec<lcc_catalog::FolderNode>>)>,
    /// The catalog's part of the folder tree (before the disk folders are added), by catalog revision.
    folder_base: Option<(u64, std::sync::Arc<Vec<lcc_catalog::FolderNode>>)>,
    /// Folders given a row of their own by Show Parent Folder (folder keys), for this session.
    shown_folders: std::collections::BTreeSet<String>,
    people: Option<(u64, lcc_catalog::Filter, std::sync::Arc<Vec<lcc_catalog::Person>>)>,
    /// The ignored faces (`faces.ignore`) as `(photo, index)`, at a catalog revision.
    ignored_faces: Option<(u64, std::sync::Arc<Vec<(u64, usize)>>)>,
    suggestions: Option<(u64, std::sync::Arc<Vec<String>>)>,
    /// What the Add keyword box offers for the text typed in it, by library revision, the
    /// selection's keywords and that text.
    completions: Option<(u64, std::sync::Arc<panels::keywording::Offers>)>,
    counts: Option<(u64, LibraryCounts)>,
    date_groups: Option<(u64, std::sync::Arc<Vec<lcc_catalog::DateGroup>>)>,
    filter_values: Option<(u64, std::sync::Arc<FilterValues>)>,
    /// The albums' counts as the sidebar shows them, with the tally that follows edits.
    album_counts: Option<AlbumCounts>,
    /// A count of every photo running off the UI thread; the counts above are shown until it is done.
    album_recount: Option<AlbumRecount>,
    /// The library, catalog revision and minute a count of every photo failed at: not tried again until one changes.
    album_recount_failed: Option<(u64, u64, String)>,
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
    smart_problems: Option<(u64, std::sync::Arc<std::collections::HashMap<lcc_catalog::AlbumId, Vec<lcc_catalog::rules::Problem>>>)>,
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
    /// When to ask the background ML indexer again (`focus_scan::pump`).
    pub ml_next_pump: f64,
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
    /// The library filter (any person filter aside) the two above were asked for with: when the sidebar picks another
    /// year, folder or album, they are asked for again.
    pub faces_filter: Option<lcc_catalog::Filter>,
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

/// The albums' counts as the sidebar shows them ([`Caches::album_counts`]).
struct AlbumCounts {
    /// The library they are of ([`lcc_engine::Session::library_generation`]).
    library: u64,
    /// The minute they are of while a smart album asks about "the last N days" (else empty).
    minute: String,
    /// What follows edits to a photo or a few.
    tally: lcc_catalog::tally::AlbumTally,
    /// The counts as the panels take them.
    shown: std::sync::Arc<std::collections::HashMap<lcc_catalog::AlbumId, usize>>,
}

/// A count of every photo on a worker thread ([`Caches::album_counts`]).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct AlbumRecount {
    /// The library, minute and catalog revision it counts.
    library: u64,
    minute: String,
    revision: u64,
    /// The count, sent before the repaint is asked for, so the frame it wakes finds it.
    done: std::sync::mpsc::Receiver<Result<lcc_catalog::tally::AlbumTally, String>>,
    /// The thread, until it has let go of its copy of the catalog.
    worker: std::thread::JoinHandle<()>,
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
    pub problems: Vec<lcc_catalog::rules::Problem>,
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
fn still_right(cat: &lcc_catalog::Catalog, revision: u64, reads: lcc_catalog::Aspects) -> bool {
    revision == cat.revision || cat.aspects_since(revision).is_some_and(|changed| !changed.any_of(reads))
}

/// What the tallies below read of a photo, besides whether it is in the library.
const READS_FILE: lcc_catalog::Aspects = lcc_catalog::Aspects::PRESENCE.with(lcc_catalog::Aspects::FILE);
const READS_META: lcc_catalog::Aspects = lcc_catalog::Aspects::PRESENCE.with(lcc_catalog::Aspects::META);
/// The keyword tree: the keywords photos carry, and the keyword list's own.
const READS_KEYWORDS: lcc_catalog::Aspects = READS_META.with(lcc_catalog::Aspects::KEYWORD_LIST);
const READS_PEOPLE: lcc_catalog::Aspects = READS_META.with(lcc_catalog::Aspects::ANALYSIS);
const READS_CAPTURED: lcc_catalog::Aspects = lcc_catalog::Aspects::PRESENCE.with(lcc_catalog::Aspects::CAPTURED);
const READS_FLAG: lcc_catalog::Aspects = lcc_catalog::Aspects::PRESENCE.with(lcc_catalog::Aspects::FLAG);

impl Caches {
    /// The folders the library's photos were imported from (only those: the Folders panel lists
    /// where the library's photos live, not the disk around them).
    pub fn folder_tree(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<Vec<lcc_catalog::FolderNode>> {
        let key = cat.revision;
        if let Some((k, t)) = &self.folder_tree
            && *k == key
        {
            return t.clone();
        }
        // the tree is made again only when a photo's file or presence changed
        let base = self.folder_base.take().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, b)| b);
        let base = base.unwrap_or_else(|| std::sync::Arc::new(crate::panels::left::sidebar_folders(cat.folder_tree(), &self.shown_folders)));
        self.folder_base = Some((cat.revision, base.clone()));
        self.folder_tree = Some((key, base.clone()));
        base
    }
    /// Gives `path` a row of its own in the Folders list (Show Parent Folder).
    pub(crate) fn show_folder(&mut self, path: &str) {
        let key = lcc_catalog::query::folder_key(path);
        if !key.is_empty() && self.shown_folders.insert(key) {
            self.folder_base = None;
            self.folder_tree = None;
        }
    }
    /// The folder tree as last built, when it still holds (a read-only check cannot rebuild it).
    pub(crate) fn folder_tree_if_fresh(&self, cat: &lcc_catalog::Catalog) -> Option<std::sync::Arc<Vec<lcc_catalog::FolderNode>>> {
        self.folder_tree.as_ref().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, t)| t.clone())
    }
    /// The library's keyword tree.
    pub fn keyword_tree(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<Vec<lcc_catalog::KeywordNode>> {
        let kept = self.keyword_tree.take().filter(|(r, _)| still_right(cat, *r, READS_KEYWORDS)).map(|(_, t)| t);
        let t = kept.unwrap_or_else(|| cat.keyword_tree_shared());
        self.keyword_tree = Some((cat.revision, t.clone()));
        t
    }
    /// The Keywording box's read-only names for the selection: what exported files carry
    /// (`export`), or the keywords with those containing them.
    pub(crate) fn keyword_names(
        &mut self,
        cat: &lcc_catalog::Catalog,
        selection: &[lcc_catalog::PhotoId],
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
        cat: &lcc_catalog::Catalog,
        selection: &[lcc_catalog::PhotoId],
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
    pub(crate) fn selected_photos(&mut self, cat: &lcc_catalog::Catalog, selection: &[lcc_catalog::PhotoId]) -> usize {
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
        cat: &lcc_catalog::Catalog,
        selection: &[lcc_catalog::PhotoId],
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
        cat: &lcc_catalog::Catalog,
        selection: &[lcc_catalog::PhotoId],
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
    pub fn people(&mut self, cat: &lcc_catalog::Catalog, filter: &lcc_catalog::Filter) -> std::sync::Arc<Vec<lcc_catalog::Person>> {
        let key = lcc_catalog::Filter { person: None, people: Vec::new(), ..filter.clone() };
        match &self.people {
            Some((r, f, t)) if *r == cat.revision && *f == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.people_in(&key));
                self.people = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// The in-library faces the user ignored, as `(photo, index)` (People offers to restore them).
    pub fn ignored_faces(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<Vec<(u64, usize)>> {
        match &self.ignored_faces {
            Some((r, v)) if *r == cat.revision => v.clone(),
            _ => {
                let v: Vec<(u64, usize)> = cat
                    .photos()
                    .filter(|p| p.in_library())
                    .flat_map(|p| {
                        p.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == lcc_meta::RegionKind::IgnoredFace).map(move |(i, _)| (p.id.0, i))
                    })
                    .collect();
                let v = std::sync::Arc::new(v);
                self.ignored_faces = Some((cat.revision, v.clone()));
                v
            }
        }
    }
    /// All Photos / Picks / Recently Deleted counts.
    pub fn counts(&mut self, cat: &lcc_catalog::Catalog) -> LibraryCounts {
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
                if p.flag == lcc_catalog::Flag::Pick {
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
    pub fn person_names(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<Vec<String>> {
        let kept = self.person_names.take().filter(|(r, _)| still_right(cat, *r, READS_PEOPLE)).map(|(_, v)| v);
        let v = kept.unwrap_or_else(|| std::sync::Arc::new(cat.people().into_iter().map(|p| p.name).collect::<Vec<_>>()));
        self.person_names = Some((cat.revision, v.clone()));
        v
    }
    /// The By Date tree.
    pub fn date_groups(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<Vec<lcc_catalog::DateGroup>> {
        let kept = self.date_groups.take().filter(|(r, _)| still_right(cat, *r, READS_CAPTURED)).map(|(_, g)| g);
        let g = kept.unwrap_or_else(|| std::sync::Arc::new(cat.date_groups()));
        self.date_groups = Some((cat.revision, g.clone()));
        g
    }
    /// Cameras, lenses and keywords in the library (filter bar pickers). Kept while only other things than metadata
    /// changed (issue #538: a rating, a flag or an edit rebuilt them from every photo, on every change).
    pub fn filter_values(&mut self, cat: &lcc_catalog::Catalog) -> std::sync::Arc<FilterValues> {
        let kept = self.filter_values.take().filter(|(r, _)| still_right(cat, *r, READS_META)).map(|(_, v)| v);
        let v = kept.unwrap_or_else(|| {
            // each value once, then sorted: a library has few distinct cameras, lenses and keywords but many photos
            let distinct = |values: std::collections::HashSet<&str>| values.into_iter().map(str::to_string).collect::<Vec<_>>();
            let lib = || cat.photos().filter(|p| p.in_library());
            let named = |s: &&str| !s.trim().is_empty();
            let mut cameras = distinct(lib().map(|p| p.meta.camera.as_str()).filter(named).collect());
            let mut lenses = distinct(lib().map(|p| p.meta.lens.as_str()).filter(named).collect());
            let mut keywords = distinct(lib().flat_map(|p| p.meta.keywords.iter().map(String::as_str)).collect());
            for v in [&mut cameras, &mut lenses] {
                v.sort_by_cached_key(|s| (s.to_lowercase(), s.clone()));
            }
            keywords.sort();
            std::sync::Arc::new(FilterValues { cameras, lenses, keywords })
        });
        self.filter_values = Some((cat.revision, v.clone()));
        v
    }
    /// What the Add keyword box offers for `typed` (see [`panels::keywording::offers`]). Kept apart
    /// from [`Self::suggestions`]: both are asked for on every frame of typing, and each looks
    /// through the library's keywords.
    pub(crate) fn completions(&mut self, cat: &lcc_catalog::Catalog, current: &[String], typed: &str) -> std::sync::Arc<panels::keywording::Offers> {
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
    pub fn suggestions(&mut self, cat: &lcc_catalog::Catalog, current: &[String], prefix: &str, n: usize) -> std::sync::Arc<Vec<String>> {
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
        cat: &lcc_catalog::Catalog,
        rules: &lcc_catalog::RuleSet,
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
            lcc_catalog::rules::set_now(Some(now.to_string()));
        }
        let problems = rules.check_for(cat, editing.map(lcc_catalog::AlbumId));
        let filter = lcc_catalog::Filter { rule_set: Some(rules.clone()), library_folder: folder, ..Default::default() };
        let count = if problems.is_empty() { cat.query(&filter, &Default::default()).len() } else { 0 };
        let v = std::sync::Arc::new(RulesView { problems, count, albums: panels::dialogs::album_entries(cat, editing) });
        self.rules_view_scans += 1;
        self.rules_view = Some((k, v.clone()));
        v
    }

    /// A count an open dialog shows (photos a filter matches, photos with a keyword…): `count` runs
    /// again only when the catalog or `what` (what is counted) changes, not every frame.
    pub(crate) fn dialog_count(&mut self, cat: &lcc_catalog::Catalog, what: impl std::hash::Hash, count: impl FnOnce() -> usize) -> usize {
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
        cat: &lcc_catalog::Catalog,
    ) -> std::sync::Arc<std::collections::HashMap<lcc_catalog::AlbumId, Vec<lcc_catalog::rules::Problem>>> {
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

    /// How many photos each album of the library `library` ([`lcc_engine::Session::library_generation`])
    /// shows, `now` being the session clock ("in the last…" rules count back from it).
    ///
    /// An edit to a photo or a few is followed at once: a smart album asks only those photos again (a keyword
    /// created and given to them too: the keyword list itself counts in no album). Anything more (an album's rules
    /// or photos changed, a stack, an edit to many photos such as a keyword renamed across them, a new minute while
    /// a smart album asks about "the last N days", another library) counts every photo again, seconds in a large
    /// library. That count runs on a worker thread (natively) while the counts shown before stay (none the first
    /// time, and none of another library); `ctx` repaints when it is done, and edits made meanwhile are followed
    /// from it.
    pub fn album_counts(
        &mut self,
        cat: &lcc_catalog::Catalog,
        library: u64,
        now: &str,
        ctx: &egui::Context,
    ) -> std::sync::Arc<std::collections::HashMap<lcc_catalog::AlbumId, usize>> {
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        self.collect_album_recount(library);
        // (album ids start again in every library: another one's counts would be shown on this one's albums)
        if self.album_counts.as_ref().is_some_and(|c| c.library != library) {
            self.album_counts = None;
        }
        if let Some(counts) = self.album_counts.as_ref().filter(|c| c.minute == minute && c.tally.revision() == cat.revision) {
            return counts.shown.clone();
        }
        if relative {
            // "in the last N days" counts back from the session clock, as in the grid. (Within a minute an edit asks
            // far out as the counts already were between edits, and the next minute counts them all again.)
            lcc_catalog::rules::set_now(Some(now.to_string()));
        }
        if let Some(counts) = self.album_counts.as_mut().filter(|c| c.minute == minute)
            && counts.tally.update(cat)
        {
            counts.shown = std::sync::Arc::new(counts.tally.counts().clone());
            return counts.shown.clone();
        }
        self.recount_albums(cat, library, minute, now, relative, ctx);
        self.album_counts.as_ref().map(|c| c.shown.clone()).unwrap_or_default()
    }

    /// A count of every photo is running ([`Caches::album_counts`]).
    pub fn album_recount_running(&self) -> bool {
        self.album_recount.as_ref().is_some_and(|job| !job.worker.is_finished())
    }

    /// Count every photo for [`Caches::album_counts`]: on a worker thread when one can be started, else here.
    fn recount_albums(&mut self, cat: &lcc_catalog::Catalog, library: u64, minute: &str, now: &str, relative: bool, ctx: &egui::Context) {
        // one at a time: what it finds is followed up to the catalog as it is by then, or counted again
        let failed = self.album_recount_failed.as_ref().is_some_and(|(l, r, m)| *l == library && *r == cat.revision && m == minute);
        if self.album_recount.is_some() || failed {
            return;
        }
        self.album_count_scans += 1;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // (cheap: the copy shares the photos)
            let (copy, ctx, now) = (cat.clone(), ctx.clone(), relative.then(|| now.to_string()));
            let (tx, done) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("lc-album-counts".into()).spawn(move || {
                let counted = lcc_engine::guard::catch("Counting the albums' photos", || {
                    lcc_catalog::rules::set_now(now);
                    lcc_catalog::tally::AlbumTally::of(&copy)
                });
                let _ = tx.send(counted);
                ctx.request_repaint();
            });
            match spawned {
                Ok(worker) => {
                    self.album_recount = Some(AlbumRecount { library, minute: minute.to_string(), revision: cat.revision, done, worker });
                    return;
                }
                Err(e) => log::warn!("album counts: can't start a thread ({e}); counting them here"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (now, relative, ctx);
        // (`album_counts` has set the clock on this thread)
        let counted = lcc_engine::guard::catch("Counting the albums' photos", || lcc_catalog::tally::AlbumTally::of(cat));
        self.finish_album_recount(library, cat.revision, minute.to_string(), counted);
    }

    /// Take the worker's count once it is done; a count of another library is let go (its thread ends on its own).
    fn collect_album_recount(&mut self, library: u64) {
        let Some(job) = &self.album_recount else { return };
        if job.library != library {
            self.album_recount = None;
            return;
        }
        let counted = match job.done.try_recv() {
            Ok(counted) => counted,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::error!("album counts: the count stopped without an answer");
                Err("stopped without an answer".to_string())
            }
        };
        let Some(job) = self.album_recount.take() else { return };
        self.finish_album_recount(job.library, job.revision, job.minute, counted);
    }

    /// The counts a count of every photo made are the ones shown. One that failed (already logged) leaves those
    /// shown before, and isn't tried again for the same library, catalog and minute.
    fn finish_album_recount(&mut self, library: u64, revision: u64, minute: String, counted: Result<lcc_catalog::tally::AlbumTally, String>) {
        match counted {
            Ok(tally) => {
                self.album_recount_failed = None;
                let shown = std::sync::Arc::new(tally.counts().clone());
                self.album_counts = Some(AlbumCounts { library, minute, tally, shown });
            }
            Err(_) => self.album_recount_failed = Some((library, revision, minute)),
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use lcc_catalog::{Album, AlbumId, Catalog, Filter, Op, Photo, PhotoId, RuleSet, Source};

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

    type Counts = std::sync::Arc<std::collections::HashMap<AlbumId, usize>>;

    /// [`super::Caches::album_counts`] of library `library` once a count of every photo it started is done and
    /// taken.
    fn counted_in(c: &mut super::Caches, cat: &Catalog, library: u64, now: &str) -> Counts {
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        loop {
            let shown = c.album_counts(cat, library, now, &ctx);
            if c.album_recount.is_none() {
                return shown;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count is done");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn counted(c: &mut super::Caches, cat: &Catalog, now: &str) -> Counts {
        counted_in(c, cat, 0, now)
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
        let n = counted(&mut c, &cat, now);
        assert_eq!((n[&AlbumId(10)], n[&AlbumId(11)]), (1, 1));
        assert_eq!(c.album_count_scans, 1);
        for _ in 0..10 {
            counted(&mut c, &cat, "2026-09-30T12:00:40");
        }
        assert_eq!(c.album_count_scans, 1, "unchanged frames in the same minute don't rescan");
        // a photo rated: followed from that photo, not counted from every one
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 2);
        assert_eq!(c.album_count_scans, 1);
        // rule change
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 5}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 1);
        // an hour later the 11:59:30 photo has left "in the last hour", with no catalog change
        assert_eq!(counted(&mut c, &cat, "2026-09-30T13:00:10")[&AlbumId(11)], 0);
        assert_eq!(c.album_count_scans, 3, "new rules and a new minute are counted from every photo; the rating was not");
        lcc_catalog::rules::set_now(None);
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
        cat.apply(Op::AddAlbum { album: lcc_catalog::Album::new(AlbumId(20), "Trip") }).unwrap();
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
        counted(&mut c, &cat, "2026-09-30T12:00:00");
        counted(&mut c, &cat, "2027-01-01T00:00:00");
        assert_eq!(c.album_count_scans, 1);
    }

    /// Counting every photo again (an album's rules changed, an edit to many photos) takes seconds in a large
    /// library: it runs off the UI thread, and the sidebar keeps the counts it showed until the new ones are ready.
    #[test]
    fn a_count_of_every_photo_runs_in_the_background_and_the_counts_shown_stay_meanwhile() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        let now = "2026-09-30T12:00:00";
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 1);
        // new rules: every photo is asked again
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        let ctx = egui::Context::default();
        assert_eq!(c.album_counts(&cat, 0, now, &ctx)[&AlbumId(10)], 1, "the frame after the change shows the counts it had");
        assert!(c.album_recount.is_some(), "and every photo is counted elsewhere");
        // done counting is said whether or not a frame has taken the count yet (a folded Albums section takes none)
        let t0 = std::time::Instant::now();
        while c.album_recount_running() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count ends");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // a photo rated meanwhile: followed from what the count finds
        cat.apply(Op::SetRating { id: PhotoId(3), rating: 0 }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 2, "three photos rated at least 1, then one not");
        assert_eq!(c.album_count_scans, 2, "counted whole once for the new rules; the rating was followed");
        // the first time there is nothing to show yet: no count, rather than a wrong one
        let mut fresh = super::Caches::default();
        assert!(fresh.album_counts(&cat, 0, now, &ctx).get(&AlbumId(10)).is_none());
        assert_eq!(counted(&mut fresh, &cat, now)[&AlbumId(10)], 2);
    }

    /// Album ids start again in every library: once another library is open, the albums of the one before never
    /// lend it their counts, nor does a count of that library still running when it closed.
    #[test]
    fn another_library_never_shows_the_counts_of_the_one_before() {
        let library = |rated: &[u8]| {
            let mut cat = Catalog::default();
            for (i, r) in rated.iter().enumerate() {
                cat.apply(photo(i as u64 + 1, "2026-09-30T11:00:00", *r)).unwrap();
            }
            cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
            cat
        };
        let (mut first, second) = (library(&[5, 1, 1]), library(&[5, 5, 1]));
        let (mut c, ctx, now) = (super::Caches::default(), egui::Context::default(), "2026-09-30T12:00:00");
        assert_eq!(counted_in(&mut c, &first, 1, now)[&AlbumId(10)], 1);
        assert!(c.album_counts(&second, 2, now, &ctx).get(&AlbumId(10)).is_none(), "nothing until its own are counted");
        assert_eq!(counted_in(&mut c, &second, 2, now)[&AlbumId(10)], 2);
        // a count of the first library under way when the second opens is let go
        let mut c = super::Caches::default();
        assert_eq!(counted_in(&mut c, &first, 1, now)[&AlbumId(10)], 1);
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})).unwrap();
        first.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(c.album_counts(&first, 1, now, &ctx)[&AlbumId(10)], 1);
        assert!(c.album_recount.is_some(), "the first library is being counted");
        assert_eq!(counted_in(&mut c, &second, 2, now)[&AlbumId(10)], 2, "the second library's own count");
    }
}

#[cfg(test)]
mod tests_display_truth;
#[cfg(test)]
mod tests_docking_acceptance;
#[cfg(test)]
mod tests_modern;
#[cfg(test)]
mod tests_modern_gestures;
#[cfg(test)]
mod tests_modern_library;
#[cfg(test)]
mod tests_modern_sheets;
#[cfg(test)]
mod tests_pill_slider;
