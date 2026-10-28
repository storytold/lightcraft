use super::{Session, Value, pick};

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
    /// Local's built-in places as (label, path). None uses the host's home folder.
    pub local_places: Option<Vec<(String, String)>>,
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
