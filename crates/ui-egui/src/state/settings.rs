use super::view::LoupeInfo;
#[cfg(test)]
use super::view::{InfoOverlay, LoupeInfoField};
use super::*;

/// Which view the app opens in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupView {
    /// Whatever was showing at quit.
    #[default]
    Last,
    Grid,
    Detail,
}

/// What the desktop app opens at launch (Settings ▸ General ▸ At launch; issue #1290). `--library`
/// and `LIGHTCRAFT_LIBRARY` come first either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchLibrary {
    /// The library open when the app last quit (holding Alt/Option as the window appears asks once).
    #[default]
    Last,
    /// A window lists the recent libraries to choose from.
    Ask,
}

/// When grid cells show their rating / flag / edited badges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GridBadges {
    /// On hover, on the selection, and on rated or flagged photos.
    #[default]
    Auto,
    Always,
    Never,
}

/// How many libraries File ▸ Open Recent Library remembers.
pub const RECENT_LIBRARIES: usize = 8;

/// Whether the interface uses the dark or the light theme (Settings ▸ Interface ▸ Appearance).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    /// Follow the operating system's dark/light setting (dark when it gives no answer).
    Auto,
    /// The dark theme, whatever the system says (the default: LightCraft's look so far).
    #[default]
    Dark,
    Light,
}

impl AppearanceMode {
    /// The header button's order: Auto, Light, Dark, then Auto again.
    pub fn next(self) -> AppearanceMode {
        match self {
            AppearanceMode::Auto => AppearanceMode::Light,
            AppearanceMode::Light => AppearanceMode::Dark,
            AppearanceMode::Dark => AppearanceMode::Auto,
        }
    }
}

/// The theme the dark appearance uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DarkTheme {
    #[default]
    Charcoal,
    Midnight,
}

/// The theme the light appearance uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LightTheme {
    #[default]
    Silver,
    Paper,
}

impl DarkTheme {
    pub const ALL: [DarkTheme; 2] = [DarkTheme::Charcoal, DarkTheme::Midnight];
    pub fn kind(self) -> crate::theme::ThemeKind {
        match self {
            DarkTheme::Charcoal => crate::theme::ThemeKind::Charcoal,
            DarkTheme::Midnight => crate::theme::ThemeKind::Midnight,
        }
    }
}

impl LightTheme {
    pub const ALL: [LightTheme; 2] = [LightTheme::Silver, LightTheme::Paper];
    pub fn kind(self) -> crate::theme::ThemeKind {
        match self {
            LightTheme::Silver => crate::theme::ThemeKind::Silver,
            LightTheme::Paper => crate::theme::ThemeKind::Paper,
        }
    }
}

/// App-level preferences (Settings dialog). They belong to the app, not a library, and are
/// saved with the UI state in the app's config folder (`ui.json`, key `settings`); library
/// preferences (import defaults, XMP, cache size) live in the library's `prefs.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// Library opened at launch when no `--library` is given (empty = the default location).
    pub library_path: String,
    /// Libraries opened recently, newest first (File ▸ Open Recent Library; at most
    /// [`RECENT_LIBRARIES`]).
    pub recent_libraries: Vec<String>,
    /// When each recent library was last opened (path → the session clock's ISO 8601 time), for
    /// the launch window. A separate key so files from earlier versions, and earlier versions
    /// reading this one, keep their list (issue #1290).
    #[serde(rename = "recentLibrariesOpened", deserialize_with = "lenient")]
    pub recent_opened: std::collections::BTreeMap<String, String>,
    /// Open the last library at launch, or ask (a value this version doesn't know reads as the
    /// default).
    #[serde(deserialize_with = "lenient")]
    pub at_launch: LaunchLibrary,
    pub startup_view: StartupView,
    /// Ask before moving photos to Recently Deleted. Saved under a new key: earlier versions saved
    /// `confirmDelete: false` by default (#1239).
    #[serde(rename = "askBeforeDelete")]
    pub confirm_delete: bool,
    /// Ask before photos dropped on a Folders row move there on disk (the dialog's "Don't ask
    /// again" turns it off).
    pub ask_before_move: bool,
    /// A click on the dimmed backdrop around a dialog closes it, like Esc (off: a stray click
    /// never throws away a dialog's choices).
    pub close_dialogs_outside: bool,
    /// Photos and folders dropped on the window import at once, without the import review.
    pub import_dropped_directly: bool,
    /// GPU rendering allowed (`app.gpu`).
    pub gpu: bool,
    /// Largest long edge (pixels) the user lets the loupe render at; 0 = Automatic (the size it is
    /// drawn at, up to the photo's own pixels and [`LOUPE_EDGE_CEILING`]). Not the old
    /// `previewEdge` key: its 2560 px default was the soft-image bug (issue #323).
    pub preview_limit: u32,
    /// Edit in Other Editor: the application ("" = the system's default for the file type).
    pub external_editor: String,
    /// The file Edit in Other Editor hands over.
    pub external_file: EditFile,
    /// Edit in PhotoCraft: where PhotoCraft is ("" = found where its installer put it; a portable
    /// copy needs its path).
    pub photocraft_path: String,
    /// The file Edit in PhotoCraft hands over.
    pub photocraft_file: EditFile,
    /// Memory the caches may hold together, in MB (0 = automatic; `app.memoryBudget`).
    pub memory_mb: u32,
    /// The Info panel's metadata fields with several photos selected: true (the default) shows the
    /// values the selected photos share and marks each field where they differ (Selected Photos),
    /// false shows the active photo's values (Target Photo) (`view.infoSelectedPhotos`).
    pub info_selected_photos: bool,
    /// Filmstrip: file names above the thumbnails.
    pub film_names: bool,
    /// Filmstrip: rating / flag / edited badges on the thumbnails.
    pub film_badges: bool,
    /// Grid: when to show the rating / flag / edited badges.
    pub grid_badges: GridBadges,
    /// View ▸ View Options ▸ Show message when loading or rendering photos: the loupe says
    /// "Rendering…" while the photo it shows is not the finished render.
    pub loupe_messages: bool,
    /// Shortcuts the user changed (Help ▸ Keyboard Shortcuts): command id → shortcut, `""` = none.
    pub keymap: crate::shortcuts::Keymap,
    /// Auto (follow the system), Dark or Light.
    pub appearance_mode: AppearanceMode,
    /// The theme of the dark appearance.
    pub dark_theme: DarkTheme,
    /// The theme of the light appearance.
    pub light_theme: LightTheme,
    /// Theme shortcuts in the appearance context menu.
    #[serde(deserialize_with = "deserialize_favorite_themes")]
    pub favorite_themes: Vec<String>,
    /// The monitor's ICC profile previews are shown through (`app.displayProfile`; "" = none:
    /// the display is treated as sRGB).
    pub display_profile: String,
    /// The Keywording box: whole keyword paths, or grouped by the keyword they are inside.
    #[serde(deserialize_with = "lenient")]
    pub keyword_chips: KeywordChips,
    /// View ▸ Focus Point: draw the box the camera focused on (`view.focusPoint`).
    pub focus_point: bool,
    /// View ▸ Zoom to Focus Point: Toggle Zoom (Z) centres the zoomed view on the camera's focus point
    /// when the photo has one (`view.zoomToFocus`).
    pub zoom_to_focus: bool,
    /// View ▸ View Options: the lines the loupe's info overlay shows in Info 1 and Info 2.
    #[serde(deserialize_with = "lenient")]
    pub loupe_info: LoupeInfo,
    /// File ▸ Import Lightroom Catalog…: ask whether the catalog merges into the open library or
    /// gets a new one, or always merge (issue #2672; the window's "Don't ask again" picks that).
    #[serde(deserialize_with = "lenient")]
    pub lightroom_import: LightroomImport,
}

/// Where File ▸ Import Lightroom Catalog… puts a catalog (Settings ▸ General ▸ Library). Saved as
/// `"ask"` / `"alwaysMerge"`; anything else in a hand-edited `ui.json` reads as [`Self::Ask`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LightroomImport {
    /// A window asks each time: merge into the open library, or create a new library for it.
    #[default]
    Ask,
    /// Merge into the open library without asking (how imports worked before the window).
    AlwaysMerge,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            library_path: String::new(),
            recent_libraries: Vec::new(),
            recent_opened: Default::default(),
            at_launch: LaunchLibrary::Last,
            startup_view: StartupView::Last,
            confirm_delete: true,
            ask_before_move: true,
            close_dialogs_outside: false,
            import_dropped_directly: false,
            gpu: true,
            preview_limit: 0,
            external_editor: String::new(),
            external_file: EditFile::default(),
            photocraft_path: String::new(),
            photocraft_file: EditFile { format: EditFormat::Psd, ..EditFile::default() },
            memory_mb: 0,
            info_selected_photos: true,
            film_names: true,
            film_badges: true,
            grid_badges: GridBadges::Auto,
            loupe_messages: true,
            keymap: Default::default(),
            appearance_mode: AppearanceMode::Dark,
            dark_theme: DarkTheme::Charcoal,
            light_theme: LightTheme::Silver,
            favorite_themes: Vec::new(),
            display_profile: String::new(),
            keyword_chips: KeywordChips::default(),
            focus_point: false,
            zoom_to_focus: true,
            loupe_info: LoupeInfo::default(),
            lightroom_import: LightroomImport::Ask,
        }
    }
}

/// The editor name that means PhotoCraft (Photo ▸ Edit In ▸ Edit in PhotoCraft): the host finds
/// where it is installed.
pub const PHOTOCRAFT: &str = "PhotoCraft";

/// The file format Edit In renders the copy as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditFormat {
    #[default]
    Tiff,
    /// A flat PSD: PhotoCraft saves back to it in place (a TIFF it saves only through Save As).
    Psd,
}

/// The colour space of the Edit In copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditSpace {
    #[default]
    AdobeRgb,
    ProPhoto,
    DisplayP3,
    Srgb,
}

/// The file Photo ▸ Edit In renders for an editor (Settings ▸ External Editing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditFile {
    pub format: EditFormat,
    pub color_space: EditSpace,
    /// 16 or 8.
    pub bit_depth: u8,
}

impl Default for EditFile {
    fn default() -> Self {
        EditFile { format: EditFormat::Tiff, color_space: EditSpace::AdobeRgb, bit_depth: 16 }
    }
}

impl EditFile {
    /// The `photo.editExternal` params for this file.
    pub fn params(&self) -> serde_json::Value {
        let format = match self.format {
            EditFormat::Tiff => "tiff",
            EditFormat::Psd => "psd",
        };
        let space = match self.color_space {
            EditSpace::AdobeRgb => "adobeRgb",
            EditSpace::ProPhoto => "proPhoto",
            EditSpace::DisplayP3 => "displayP3",
            EditSpace::Srgb => "srgb",
        };
        serde_json::json!({"format": format, "colorSpace": space, "bitDepth": if self.bit_depth == 8 { 8 } else { 16 }})
    }
}

/// Preview size limits offered in Settings → Performance (0 = Automatic).
pub const PREVIEW_LIMITS: [u32; 5] = [0, 1600, 2560, 3840, 5120];

/// The most the loupe ever renders in one go (memory and GPU texture size), whatever the setting.
pub const LOUPE_EDGE_CEILING: u32 = 8192;

/// The most a zoomed frame (the one a window is cut from) may be along its long edge: far more
/// than the photos there are (a window holds only what is on screen, so the frame costs nothing).
pub const WINDOW_FRAME_CEILING: usize = 65536;

/// The smallest GPU texture side the loupe plans for, whatever the host reports.
pub const MIN_TEXTURE_SIDE: usize = 512;

/// Loupe render sizes move in steps of this many pixels.
pub const LOUPE_EDGE_STEP: usize = 64;

/// Long edge of Build Standard-Sized Previews when the limit is Automatic.
pub const STANDARD_PREVIEW_EDGE: u32 = 2560;

impl AppSettings {
    /// The theme to show: the light theme in Light mode, or in Auto when `system` is light; the
    /// dark theme otherwise (Auto with no answer from the system included).
    pub fn theme(&self, system: Option<egui::Theme>) -> crate::theme::ThemeKind {
        let light = match self.appearance_mode {
            AppearanceMode::Light => true,
            AppearanceMode::Dark => false,
            AppearanceMode::Auto => system == Some(egui::Theme::Light),
        };
        if light { self.light_theme.kind() } else { self.dark_theme.kind() }
    }

    /// Pick `kind` and fix the mode to its family (the View ▸ Theme items).
    pub fn select_theme(&mut self, kind: crate::theme::ThemeKind) {
        use crate::theme::ThemeKind;
        match kind {
            ThemeKind::Charcoal => self.dark_theme = DarkTheme::Charcoal,
            ThemeKind::Midnight => self.dark_theme = DarkTheme::Midnight,
            ThemeKind::Silver => self.light_theme = LightTheme::Silver,
            ThemeKind::Paper => self.light_theme = LightTheme::Paper,
        }
        self.appearance_mode = if kind.is_dark() { AppearanceMode::Dark } else { AppearanceMode::Light };
    }

    /// Long edge (pixels) to render the loupe at when it is drawn `wanted_px` wide on screen:
    /// that size (rounded up to [`LOUPE_EDGE_STEP`], so resizing a window doesn't re-render at
    /// every pixel), never above the photo's own `native_long_edge`, the user's limit, the ceiling
    /// or `texture_side`, the largest texture the GPU allows (egui_glow, i.e. the browser build,
    /// panics on a bigger one, and many WebGL devices allow only 2048 or 4096).
    pub fn loupe_edge(&self, wanted_px: f32, native_long_edge: usize, texture_side: usize) -> usize {
        let limit = if self.preview_limit == 0 { LOUPE_EDGE_CEILING } else { self.preview_limit.clamp(512, LOUPE_EDGE_CEILING) } as usize;
        let ceiling = limit.min(texture_side.max(MIN_TEXTURE_SIDE));
        let wanted = if wanted_px.is_nan() { 8.0 } else { wanted_px.clamp(8.0, ceiling as f32) } as usize;
        let stepped = wanted.div_ceil(LOUPE_EDGE_STEP) * LOUPE_EDGE_STEP;
        stepped.min(native_long_edge.max(8)).min(ceiling)
    }

    /// Long edge for warming a neighbouring photo: its loupe size but never above the preview
    /// source level. A full-size decode would replace the open photo's single full-resolution
    /// source in the cache, and nothing is gained by it.
    pub fn prefetch_edge(&self, wanted_px: f32, native_long_edge: usize, texture_side: usize) -> usize {
        self.loupe_edge(wanted_px, native_long_edge, texture_side).min(lightcraft_engine::SourceLevel::Preview.max_edge())
    }

    /// Long edge for the hover (preset / profile) and Before renders, which are stand-ins drawn
    /// over the loupe: capped at the preview source level like the old default.
    pub fn stand_in_edge(&self, loupe_edge: usize, texture_side: usize) -> usize {
        loupe_edge.min(lightcraft_engine::SourceLevel::Preview.max_edge()).min(texture_side.max(MIN_TEXTURE_SIDE))
    }

    /// Long edge of the zoomed frame a window render is cut from, for a photo drawn `drawn_long`
    /// px along its long edge: as drawn, but never above the photo's own pixels (beyond 100 % the
    /// GPU magnifies the window) or the ceiling. The preview size limit is not applied: it is about
    /// the whole-frame render (see [`crate::region::plan`]).
    pub fn window_frame_edge(&self, drawn_long: f32, native_long_edge: usize) -> usize {
        let wanted = if drawn_long.is_nan() { 8.0 } else { drawn_long.clamp(8.0, WINDOW_FRAME_CEILING as f32) } as usize;
        wanted.min(native_long_edge.max(8)).min(WINDOW_FRAME_CEILING)
    }

    /// The edge Build Standard-Sized Previews uses.
    pub fn standard_preview_edge(&self) -> u32 {
        if self.preview_limit == 0 { STANDARD_PREVIEW_EDGE } else { self.preview_limit }
    }
}

/// How the Keywording box lays out the selection's keywords.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeywordChips {
    /// One chip per keyword, each with its whole path ("Places › Portugal › Lisbon").
    #[default]
    Paths,
    /// Keywords inside the same keyword share a line naming it, and their chips say only what is
    /// below it; long lists and the suggestions are shortened.
    Grouped,
}

/// A setting read leniently: a value that isn't one of its own (a hand-edited file, a newer
/// build's choice) is the default, and the rest of the settings file is still read.
fn lenient<'de, D: serde::Deserializer<'de>, T: Default + serde::de::DeserializeOwned>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn deserialize_favorite_themes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<String>::deserialize(d)?;
    let mut seen = std::collections::BTreeSet::new();
    for id in &values {
        if !crate::theme::ThemeKind::ALL.iter().any(|k| k.id() == id) || !seen.insert(id) {
            return Err(serde::de::Error::custom("unknown or duplicate favorite theme"));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod loupe_info_tests {
    use super::*;

    /// The lines chosen in View Options are saved with the app's settings; a settings file from
    /// before they existed, or with a line this build doesn't know, gets the default lines and
    /// keeps the rest of its settings.
    #[test]
    fn view_options_lines_are_saved_with_the_settings() {
        let mut settings = AppSettings::default();
        settings.loupe_info.info2 = [LoupeInfoField::Lens, LoupeInfoField::None, LoupeInfoField::FileName];
        settings.loupe_info.briefly = InfoOverlay::Exposure;
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["loupeInfo"]["info2"], serde_json::json!(["lens", "none", "fileName"]));
        assert_eq!(saved["loupeInfo"]["briefly"], "exposure");
        let read: AppSettings = serde_json::from_value(saved).unwrap();
        assert_eq!(read.loupe_info, settings.loupe_info);

        let old: AppSettings = serde_json::from_value(serde_json::json!({"filmNames": false})).unwrap();
        assert_eq!(old.loupe_info, LoupeInfo::default());
        // lines saved before Show Briefly When Photo Changes existed are kept, with it off
        let lines: AppSettings = serde_json::from_value(serde_json::json!({"loupeInfo": {"info2": ["lens", "none", "fileName"]}})).unwrap();
        assert_eq!(lines.loupe_info.info2, settings.loupe_info.info2);
        assert_eq!(lines.loupe_info.briefly, InfoOverlay::Off);
        let newer: AppSettings =
            serde_json::from_value(serde_json::json!({"filmNames": false, "loupeInfo": {"info1": ["rating", "fileName", "none"]}})).unwrap();
        assert_eq!(newer.loupe_info, LoupeInfo::default());
        assert!(!newer.film_names, "the rest of the settings are still read");
    }

    /// Show message when loading or rendering photos is on until it is turned off, and is saved
    /// with the app's settings.
    #[test]
    fn the_loading_message_choice_is_saved_with_the_settings() {
        assert!(AppSettings::default().loupe_messages);
        let old: AppSettings = serde_json::from_value(serde_json::json!({"filmNames": false})).unwrap();
        assert!(old.loupe_messages, "a settings file from before the choice existed keeps the message");
        let settings = AppSettings { loupe_messages: false, ..AppSettings::default() };
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["loupeMessages"], false);
        let read: AppSettings = serde_json::from_value(saved).unwrap();
        assert!(!read.loupe_messages);
    }
}
