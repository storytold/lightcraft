//! UI state (serde: saved as preferences, readable/settable through the control channel).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewMode {
    /// Justified rows ("Photo Grid").
    #[default]
    PhotoGrid,
    SquareGrid,
    Detail,
    /// Two photos side by side (select | candidate), synced zoom.
    Compare,
    /// The selected photos tiled.
    Survey,
    /// A reference photo (left, fixed) beside the active photo (right, being edited).
    Reference,
    /// A card per person named on faces (close-up, name, photo count).
    People,
}

/// The right-hand tool/panel shown next to the tool strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RightPanel {
    #[default]
    None,
    Edit,
    Crop,
    Remove,
    Masking,
    RedEye,
    Versions,
    Activity,
    Keywords,
    Info,
    /// The profile browser (part of Edit).
    Profiles,
}

impl RightPanel {
    pub fn is_edit_tool(self) -> bool {
        matches!(self, RightPanel::Edit | RightPanel::Profiles | RightPanel::Crop | RightPanel::Remove | RightPanel::Masking | RightPanel::RedEye)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Zoom {
    #[default]
    Fit,
    Fill,
    /// 100 % = one image pixel per physical screen pixel.
    Percent(f32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BeforeAfter {
    #[default]
    Off,
    /// Hold `\`: show the original.
    Original,
    SideBySide,
    Split,
    /// Before above after.
    TopBottom,
    /// One image split horizontally: before above the line.
    SplitTopBottom,
}

impl BeforeAfter {
    /// One of the Before / After layouts (two views or a split line), which the bottom bar's
    /// Before/After (Y) button stands for. Show Original (`\`) is a view of its own, not a layout.
    pub fn is_layout(self) -> bool {
        !matches!(self, BeforeAfter::Off | BeforeAfter::Original)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CropOverlay {
    #[default]
    Thirds,
    Grid,
    Golden,
    Diagonal,
    Triangle,
    Spiral,
    None,
}

/// Tool Overlay: when the retouching tools' pins (Remove / Heal / Clone spots, mask pins) show on
/// the photo. Shift+H steps through the modes in this order (`view.toolOverlay`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolOverlay {
    /// While the pointer is over the photo (hidden while a new spot or brush stroke is painted).
    #[default]
    Auto,
    Always,
    /// Only the selected spot's or mask's.
    Selected,
    /// None; the tool still works (a click still adds a spot).
    Never,
}

impl ToolOverlay {
    pub const ALL: [ToolOverlay; 4] = [ToolOverlay::Auto, ToolOverlay::Always, ToolOverlay::Selected, ToolOverlay::Never];

    /// The id used by `view.toolOverlay {mode}` and the saved settings.
    pub fn name(self) -> &'static str {
        match self {
            ToolOverlay::Auto => "auto",
            ToolOverlay::Always => "always",
            ToolOverlay::Selected => "selected",
            ToolOverlay::Never => "never",
        }
    }

    pub fn parse(s: &str) -> Option<ToolOverlay> {
        ToolOverlay::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            ToolOverlay::Auto => "Auto",
            ToolOverlay::Always => "Always",
            ToolOverlay::Selected => "Selected",
            ToolOverlay::Never => "Never",
        }
    }

    /// The mode Shift+H steps to.
    pub fn next(self) -> ToolOverlay {
        match self {
            ToolOverlay::Auto => ToolOverlay::Always,
            ToolOverlay::Always => ToolOverlay::Selected,
            ToolOverlay::Selected => ToolOverlay::Never,
            ToolOverlay::Never => ToolOverlay::Auto,
        }
    }

    /// Whether a pin shows: `selected` = its spot or mask is the selected one, `live` = the pointer
    /// is over the photo (or a pin is being dragged), which is what Auto waits for.
    pub fn shows(self, selected: bool, live: bool) -> bool {
        match self {
            ToolOverlay::Auto => live,
            ToolOverlay::Always => true,
            ToolOverlay::Selected => selected,
            ToolOverlay::Never => false,
        }
    }
}

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

/// The loupe's info overlay (Cmd+I cycles; I in the full-screen preview).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InfoOverlay {
    #[default]
    Off,
    /// File name, capture date and dimensions.
    Basic,
    /// File name, camera, lens and exposure (shutter, aperture, ISO, focal length).
    Exposure,
}

impl InfoOverlay {
    pub fn next(self) -> InfoOverlay {
        match self {
            InfoOverlay::Off => InfoOverlay::Basic,
            InfoOverlay::Basic => InfoOverlay::Exposure,
            InfoOverlay::Exposure => InfoOverlay::Off,
        }
    }
}

/// One line of the loupe's info overlay, chosen in View ▸ View Options (Lightroom Classic's Loupe
/// Info 1 / Loupe Info 2). A line with nothing to show for a photo is left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoupeInfoField {
    #[default]
    None,
    FileName,
    /// The capture date and time (the import date when the photo has none).
    CaptureDate,
    /// Width × height in pixels.
    Dimensions,
    /// Shutter speed, aperture, ISO and focal length.
    Exposure,
    Camera,
    Lens,
    /// Camera and lens on one line.
    CameraAndLens,
}

impl LoupeInfoField {
    /// Every field, in the order View Options lists them.
    pub const ALL: &'static [LoupeInfoField] = &[
        LoupeInfoField::None,
        LoupeInfoField::FileName,
        LoupeInfoField::CaptureDate,
        LoupeInfoField::Dimensions,
        LoupeInfoField::Exposure,
        LoupeInfoField::Camera,
        LoupeInfoField::Lens,
        LoupeInfoField::CameraAndLens,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LoupeInfoField::None => "None",
            LoupeInfoField::FileName => "File Name",
            LoupeInfoField::CaptureDate => "Capture Date",
            LoupeInfoField::Dimensions => "Dimensions",
            LoupeInfoField::Exposure => "Exposure",
            LoupeInfoField::Camera => "Camera",
            LoupeInfoField::Lens => "Lens",
            LoupeInfoField::CameraAndLens => "Camera & Lens",
        }
    }
}

/// The three lines each mode of the info overlay shows: Info 1 ([`InfoOverlay::Basic`]) and
/// Info 2 ([`InfoOverlay::Exposure`]), and which of them Show Briefly When Photo Changes shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoupeInfo {
    pub info1: [LoupeInfoField; 3],
    pub info2: [LoupeInfoField; 3],
    /// Show Briefly When Photo Changes: while the info overlay is off, this Info shows for
    /// [`BRIEF_INFO_SECS`] each time the loupe changes to another photo ([`InfoOverlay::Off`]:
    /// neither).
    pub briefly: InfoOverlay,
}

/// Passes after Add Keywords within which the Add keyword box takes the cursor (the panel may
/// need a pass to open first).
pub const FOCUS_KEYWORDS_PASSES: u64 = 2;

/// How long Show Briefly When Photo Changes shows the info overlay (seconds).
pub const BRIEF_INFO_SECS: f64 = 3.0;

/// When the loupe last changed photo, for Show Briefly When Photo Changes
/// ([`LoupeInfo::briefly`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BriefInfo {
    /// The photo the loupe showed last.
    pub photo: Option<lightcraft_catalog::PhotoId>,
    /// When the loupe changed to it (seconds on egui's clock).
    pub since: f64,
    /// The brief info overlay shows this frame.
    pub shown: bool,
}

impl LoupeInfo {
    /// Info 1's lines before they are changed (and after Use Defaults).
    pub const INFO1: [LoupeInfoField; 3] = [LoupeInfoField::FileName, LoupeInfoField::CaptureDate, LoupeInfoField::Dimensions];
    /// Info 2's lines before they are changed (and after Use Defaults).
    pub const INFO2: [LoupeInfoField; 3] = [LoupeInfoField::FileName, LoupeInfoField::Exposure, LoupeInfoField::CameraAndLens];
}

impl Default for LoupeInfo {
    fn default() -> Self {
        LoupeInfo { info1: LoupeInfo::INFO1, info2: LoupeInfo::INFO2, briefly: InfoOverlay::Off }
    }
}

/// View ▸ Loupe Overlay ▸ Layout Image (Lightroom Classic's loupe overlay): a graphic with
/// transparency, such as a magazine cover's masthead and cover lines, drawn over the photo in the
/// loupe (`panels::layout_overlay`). Saved with the UI state, so the layout stays chosen across
/// launches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayoutImage {
    /// The graphic's file; empty until one is chosen.
    pub path: String,
    /// Drawn over the loupe (⌥⌘O).
    pub show: bool,
    /// The graphic's opacity, 0..=1.
    pub opacity: f32,
    /// How dark the photo is outside the graphic's box, 0..=1 (0: not darkened).
    pub matte: f32,
    /// Bumped by each Choose Layout Image…, so choosing the same file again reads it again (after
    /// it was edited elsewhere).
    #[serde(skip)]
    pub revision: u32,
}

impl Default for LayoutImage {
    fn default() -> Self {
        LayoutImage { path: String::new(), show: false, opacity: 1.0, matte: 0.0, revision: 0 }
    }
}

impl LayoutImage {
    /// The opacity and matte steps View ▸ Loupe Overlay offers.
    pub const OPACITY_STEPS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
    pub const MATTE_STEPS: [f32; 4] = [0.0, 0.25, 0.5, 0.75];
}

/// Lights Out (View ▸ Lights Out, Shift+L cycles On → Dim → Off → On; Esc turns the lights back
/// on): everything but the photo is veiled in black, the panels stay where they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LightsOut {
    #[default]
    On,
    /// The interface at 20 % brightness.
    Dim,
    /// The interface black.
    Off,
}

impl LightsOut {
    pub fn next(self) -> LightsOut {
        match self {
            LightsOut::On => LightsOut::Dim,
            LightsOut::Dim => LightsOut::Off,
            LightsOut::Off => LightsOut::On,
        }
    }

    /// Opacity of the black veil over everything but the photo.
    pub fn veil(self) -> f32 {
        match self {
            LightsOut::On => 0.0,
            LightsOut::Dim => 0.8,
            LightsOut::Off => 1.0,
        }
    }
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

/// Click-zoom ratios offered (percent): 1:1, 2:1, 3:1, 4:1, 8:1.
pub const CLICK_ZOOMS: [u32; 5] = [100, 200, 300, 400, 800];

/// Grid thumbnail size (points): the bottom-bar slider's range, and the `=` / `-` keys' step
/// (`view.thumbSizeUp` / `view.thumbSizeDown`), about a thirteenth of the range.
pub const THUMB_SIZE_MIN: f32 = 90.0;
pub const THUMB_SIZE_MAX: f32 = 480.0;
pub const THUMB_SIZE_STEP: f32 = 30.0;

/// Width limits of a side panel the user resizes (points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelWidth {
    pub min: f32,
    pub default: f32,
    pub max: f32,
}

impl PanelWidth {
    /// `w` within the limits (the default when it isn't a number).
    pub fn clamp(&self, w: f32) -> f32 {
        if w.is_finite() { w.clamp(self.min, self.max) } else { self.default }
    }
}

/// The left sidebar (sources, albums, folders).
pub const LEFT_WIDTH: PanelWidth = PanelWidth { min: 200.0, default: 268.0, max: 480.0 };
/// The right panel (Edit, Masking, Info, …).
pub const RIGHT_WIDTH: PanelWidth = PanelWidth { min: 250.0, default: 270.0, max: 520.0 };
/// The photo area the side panels always leave free (as far as their minimum widths allow).
pub const MIN_PHOTO_WIDTH: f32 = 360.0;

/// What the Keywording box shows: the keywords (chips to edit), them with the keywords containing
/// them, or what exported files will carry (read only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeywordingView {
    #[default]
    Keywords,
    Containing,
    WillExport,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiState {
    /// How large the app's window was and whether it was zoomed, to open it like that again (the
    /// desktop app; absent until a window has rested at a size). Read leniently: a window that
    /// is not one reads as none, and never costs the other settings.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "crate::window_place::WindowPlace::lenient")]
    pub window: Option<crate::window_place::WindowPlace>,
    /// Shared panel arrangement; absent in legacy settings until their first layout.
    pub docking: Option<crate::docking::Workspace>,
    /// The arrangement generation `docking` was saved in ([`crate::docking::DOCKING_VERSION`]);
    /// settings files are read through [`crate::docking::drop_outdated`].
    pub docking_version: u32,
    #[serde(default = "crate::i18n::default_language")]
    pub language: crate::i18n::Locale,
    /// The Build Previews run last announced (its identity, finished?).
    #[serde(skip)]
    pub preview_build_seen: Option<(usize, bool)>,
    /// Saving the library is failing (announced with a toast; the top bar shows a warning until
    /// a later save succeeds).
    #[serde(skip)]
    pub unsaved_seen: bool,
    /// Help ▸ Debug ▸ Frame Rate (`view.frameRate`): the frame-rate display
    /// (`crate::frame_rate`). A debugging aid, never saved; `LIGHTCRAFT_FPS=1` starts with it on.
    #[serde(skip)]
    pub frame_rate: bool,
    pub view: ViewMode,
    pub left_panel: bool,
    pub right: RightPanel,
    /// The right panel F8 (`view.rightPanel`) hid, which the next F8 shows again.
    #[serde(skip)]
    pub right_hidden: RightPanel,
    /// Widths of the left sidebar and of the right panel in points (dragging their inner edge
    /// resizes them; within [`LEFT_WIDTH`] / [`RIGHT_WIDTH`]).
    pub left_width: f32,
    pub right_width: f32,
    /// Presets column open (opens to the left of the Edit panel).
    pub presets: bool,
    /// Presets column: show a live thumbnail of the photo with each preset.
    pub preset_thumbs: bool,
    pub filmstrip: bool,
    pub zoom: Zoom,
    /// Pan offset of the loupe when zoomed (image-normalized centre).
    pub pan: (f32, f32),
    /// Zoom applied by a click on the image (and Z / Space), in percent (100 = 1:1, 200 = 2:1);
    /// one of [`CLICK_ZOOMS`].
    pub click_zoom: u32,
    /// Animate the loupe rect toward its target (set by a click-zoom).
    #[serde(skip)]
    pub zoom_anim: bool,
    pub before_after: BeforeAfter,
    pub thumb_size: f32,
    /// Open Edit sections by id.
    pub open_sections: Vec<String>,
    /// Open flyouts (curve, mixer, grading…).
    pub open_flyouts: Vec<String>,
    /// Solo Mode (`view.soloSections`): opening one Edit section closes the others.
    pub single_panel: bool,
    pub show_clipping: bool,
    pub histogram: bool,
    /// Soft proofing (S in the loupe): render as `proof` would hold the photo.
    pub soft_proof: bool,
    pub proof: lightcraft_engine::pipeline::Proof,
    /// Soft proofing against a printer / paper ICC profile: (file, name), the intent and Simulate
    /// Paper & Ink. With a profile, `proof.profile` names its table.
    #[serde(skip)]
    pub proof_profile: Option<(String, String)>,
    #[serde(skip)]
    pub proof_intent: lightcraft_engine::proof::ProofIntent,
    pub proof_paper_ink: bool,
    /// Masking: show the selected mask as a rendered overlay (O), how (`MaskView` name, ⇧O cycles),
    /// in which colour and opacity (0..100, colour views), and whether pins are drawn.
    pub mask_overlay: bool,
    pub mask_overlay_mode: String,
    pub mask_overlay_color: [u8; 3],
    pub mask_overlay_opacity: f32,
    pub mask_pins: bool,
    /// When the Remove tool's spot pins and the mask pins show (Shift+H cycles). Mask pins also
    /// need [`mask_pins`](Self::mask_pins).
    pub tool_overlay: ToolOverlay,
    /// Masking: show every visible mask's region as one overlay at once (Ctrl+Shift+O / Cmd+Shift+O),
    /// instead of just the selected mask's ([`mask_overlay`]).
    pub mask_overlay_all: bool,
    /// The overlay (on, mode) to go back to when Show Luminance Map is turned off.
    #[serde(skip)]
    pub luminance_map_restore: Option<(bool, String)>,
    /// Mirroring of the triangle / spiral crop guides (0..4).
    pub crop_overlay_orient: u8,
    pub crop_overlay: CropOverlay,
    /// Last explicitly chosen aspect, retained across photos and app launches; `None` (nothing
    /// chosen yet) crops locked to the photo's own shape, as Lightroom starts (#991).
    pub crop_default_aspect: Option<serde_json::Value>,
    pub show_filenames: bool,
    /// What the square grid's caption shows: `filename`, `exposure` (shutter · aperture · ISO ·
    /// focal length) or `date`.
    pub grid_info: String,
    /// Photo counts next to sources and albums in the left panel.
    pub show_counts: bool,
    /// Face / pet boxes (read from XMP) over the photo in the loupe.
    pub face_boxes: bool,
    /// The left sidebar's sections: their order, which are hidden and which are folded shut.
    pub sidebar: crate::sidebar::SidebarLayout,
    /// What settings files said before [`UiState::sidebar`]: the ids of the folded sections. Read
    /// so they stay folded ([`UiState::sanitized`] moves them over), never written.
    #[serde(skip_serializing)]
    pub collapsed_sidebar: Vec<String>,
    /// A face's name being typed in the loupe.
    #[serde(skip)]
    pub name_edit: Option<NameEdit>,
    /// The person whose page the People view shows (their faces and the faces that look like them); `None`: everyone.
    #[serde(skip)]
    pub person_page: Option<String>,
    /// The person page a photo in the loupe was opened from (`None`: the People view's unnamed faces), and that photo:
    /// Escape goes back there (not to the grid).
    #[serde(skip)]
    pub person_from: Option<(Option<String>, u64)>,
    /// The last person page left: the People view offers a way back to it next to its title.
    #[serde(skip)]
    pub last_person: Option<String>,
    /// Assisted Culling's burst settings last confirmed (gap in seconds, similarity), for this
    /// session; `None` until the dialog is first confirmed (then 10 s, 0.93).
    #[serde(skip)]
    pub cull_burst: Option<(f32, f32)>,
    /// "More" faces the user hid with ×, for this session: (photo, region index).
    #[serde(skip)]
    pub dismissed_faces: std::collections::HashSet<(u64, usize)>,
    /// The unnamed faces selected in the People view (photo, region), the name being typed for them, the face last
    /// clicked (Shift-click selects a range from it) and whether the name box should take the keyboard.
    #[serde(skip)]
    pub unnamed_selected: std::collections::HashSet<(u64, usize)>,
    #[serde(skip)]
    pub unnamed_name: String,
    #[serde(skip)]
    pub unnamed_anchor: Option<usize>,
    #[serde(skip)]
    pub unnamed_focus: bool,
    /// Local sidebar locations hidden with “Remove from Local” (folders on disk are untouched).
    pub hidden_locations: Vec<String>,
    /// Copies opened in an external editor this session (reloaded when the window is focused
    /// again), and whether the window had focus last frame.
    #[serde(skip)]
    pub external_edits: Vec<u64>,
    #[serde(skip)]
    pub was_focused: bool,
    /// When the watched folder was last scanned (monotonic app time, so a hidden window keeps
    /// scanning).
    #[serde(skip)]
    pub auto_import_at: f64,
    /// When the automatic XMP writes that failed were last tried again (monotonic app time), and how
    /// many were waiting then.
    #[serde(skip)]
    pub sidecar_retry_at: f64,
    #[serde(skip)]
    pub sidecars_waiting: usize,
    /// The cards and cameras already seen (volume roots) while Settings → Import → "Show the
    /// import review when a camera or card is connected" is on; `None` = not looked yet.
    #[serde(skip)]
    pub known_cards: Option<Vec<String>>,
    /// The develop control whose slider is being dragged (geometry sliders show a grid).
    #[serde(skip)]
    pub dragging_control: Option<String>,
    pub search: String,
    /// Focus the search field on the next frame (Edit → Find…).
    #[serde(skip)]
    pub focus_search: bool,
    /// Put the cursor in the Keywords panel's Add keyword box (Add Keywords): the egui pass it was
    /// asked on. A box drawn more than [`FOCUS_KEYWORDS_PASSES`] passes later doesn't take it, so
    /// a request made while the panel can't show expires.
    #[serde(skip)]
    pub focus_keywords: Option<u64>,
    /// A mask being renamed in the Masks list: its id and the edited name.
    #[serde(skip)]
    pub renaming_mask: Option<(u32, String)>,
    /// The Describe field (AI mask from a text prompt) while open: how the selection combines
    /// (`new` mask, or `add`/`subtract`/`intersect` on the selected one) and the text typed.
    #[serde(skip)]
    pub describe: Option<(String, String)>,
    /// A SAM 3 download was started from the app (to report its end once).
    #[serde(skip)]
    pub sam_downloading: bool,
    /// When to start the zoomed-in detail pass of an AI mask (app time) and which mask: set by
    /// each click or description, so the pass runs once the clicking stops (monotonic app time).
    #[serde(skip)]
    pub detail_due: Option<(f64, u32)>,
    /// A mask component being renamed inline: (mask id, component index, name).
    pub renaming_component: Option<(u32, usize, String)>,
    /// Close the window on the next frame (File → Quit).
    #[serde(skip)]
    pub quit: bool,
    /// Photos being dragged from the grid (dropped on an album to add them).
    #[serde(skip)]
    pub dragging_photos: Option<Vec<u64>>,
    /// An album just made: the Albums tree opens the folders down to it, once.
    #[serde(skip)]
    pub reveal_album: Option<u64>,
    /// A keyword just made from the sidebar: Keywords opens down to it, once.
    #[serde(skip)]
    pub reveal_keyword: Option<String>,
    /// A folder Photo ▸ Show in Folders went to (its Folders row's path): the sidebar's Folders
    /// section opens down to it and scrolls to it, once.
    #[serde(skip)]
    pub reveal_folder: Option<String>,
    /// An album or folder row being dragged in the sidebar (dropped on a folder to move it there).
    #[serde(skip)]
    pub dragging_album: Option<u64>,
    /// A sidebar section being dragged by its header (dropped between sections to move it there).
    #[serde(skip)]
    pub dragging_section: Option<crate::sidebar::SidebarSection>,
    /// Selected curve channel in the Curve flyout.
    pub curve_channel: String,
    /// Selected mixer mode: "hue" | "saturation" | "luminance" | "all".
    pub mixer_mode: String,
    /// Selected colour grading tab: "3way" | "shadows" | "midtones" | "highlights".
    pub grading_mode: String,
    /// Colour grading wheels (shadows, midtones, highlights) hidden for viewing by their eye, and
    /// the photo that applies to: not an edit, and forgotten on moving to another photo or quitting.
    #[serde(skip)]
    pub grading_hidden: Option<(lightcraft_catalog::PhotoId, [bool; 3])>,
    /// Active on-canvas tool: "", "brush", "linear", "radial", "wbPicker", "straighten", "remove".
    pub tool: String,
    /// Guided Upright: the selected guide (photo id + index) while the tool is open. A Delete /
    /// Backspace on it removes that guide alone; a click away or leaving the tool clears it.
    #[serde(skip)]
    pub selected_guide: Option<(u64, usize)>,
    pub brush_size: f32,
    pub brush_feather: f32,
    pub brush_flow: f32,
    pub brush_erase: bool,
    /// Brush Auto Mask: dabs stick to areas like the one under the brush centre.
    pub brush_auto_mask: bool,
    /// Brush Pen Pressure: a pen's pressure scales each dab's size and flow (a mouse paints at full pressure).
    pub brush_pressure: bool,
    /// Remove tool brush: size (fraction of the long edge), feather and opacity (0..100).
    pub remove_size: f32,
    pub remove_feather: f32,
    pub remove_opacity: f32,
    /// Selected Point Color sample.
    pub point_color: usize,
    /// Point Color "Visualize range": the selected sample's range in colour, the rest grey.
    pub point_color_visualize: bool,
    /// Light panel "Visualize HDR": grey below SDR white, colour bands above (HDR edits).
    pub hdr_visualize: bool,
    /// Red Eye panel: selected correction, and whether new ones are pet eyes.
    pub eye: usize,
    pub eye_pet: bool,
    /// Remove tool: Visualize Spots (high-pass black/white view) and its threshold 0..100.
    pub visualize_spots: bool,
    pub spots_threshold: f32,
    /// The library filter bar above the grid.
    pub filter_bar: bool,
    /// Folders kept in Local (Browse Folder…, Keep in Local / Add to Local; `local.addRoot`):
    /// listed after the built-in locations, across restarts, whatever is browsed.
    pub local_roots: Vec<String>,
    /// The top-level Local row listed for this session only, for a browsed folder outside every
    /// kept location; it stays while browsing below it (see `panels::left::local_places`).
    #[serde(skip)]
    pub local_browse_root: Option<String>,
    /// Culling: after a rating, flag or colour-label key, move to the next photo.
    pub auto_advance: bool,
    /// Full-screen preview (F): the photo alone on black, no chrome.
    pub fullscreen: bool,
    /// Lights Out (Shift+L): not saved, every launch starts with the lights on.
    #[serde(skip)]
    pub lights_out: LightsOut,
    /// Window ▸ Second Window.
    pub second_window: bool,
    /// The keyword painter: clicking a photo in the grid toggles this keyword on it.
    #[serde(skip)]
    pub keyword_painter: Option<String>,
    /// What the Keywording box shows (Lightroom Classic's Keyword Tags views).
    pub keywording_view: KeywordingView,
    /// The Keywording box, grouped: every keyword of this selection (its hash) is shown, however
    /// many lines that takes ("+N more" was clicked). Another selection's list is short again.
    #[serde(skip)]
    pub keyword_chips_all: Option<u64>,
    /// Sections of the right panel folded shut by a click on their title (`keywording`,
    /// `keywordList`); the others are open.
    #[serde(deserialize_with = "lenient_ids")]
    pub folded_sections: Vec<String>,
    /// The Keyword List's open levels (lower-case paths).
    pub keyword_list_open: Vec<String>,
    /// The keyword picked in the Keyword List (− deletes it, Edit edits it).
    #[serde(skip)]
    pub keyword_list_selected: Option<String>,
    /// A keyword being dragged in the Keyword List (onto another to nest it).
    #[serde(skip)]
    pub dragging_keyword: Option<String>,
    /// A running slideshow (full screen; see [`crate::panels::slideshow`]).
    #[serde(skip)]
    pub slideshow: Option<Box<crate::panels::slideshow::Run>>,
    /// Info overlay on the loupe.
    pub info_overlay: InfoOverlay,
    /// When the loupe last changed photo (View Options' Show Briefly When Photo Changes).
    #[serde(skip)]
    pub brief_info: BriefInfo,
    /// The loupe's layout image overlay (View ▸ Loupe Overlay).
    pub layout_image: LayoutImage,
    /// Navigator mini map in the loupe while zoomed in.
    pub navigator: bool,
    /// App preferences (Settings dialog).
    pub settings: AppSettings,
    /// Window full screen requested (⇧⌘F); the host applies it (`None` = no change pending).
    #[serde(skip)]
    pub window_fullscreen: Option<bool>,
    /// Compare view: (select, candidate) photo ids.
    #[serde(skip)]
    pub compare: Option<(u64, u64)>,
    /// Reference view: the reference photo.
    #[serde(skip)]
    pub reference: Option<u64>,
    /// Transient toast text, expiry (seconds of app time), and optional colour-label styling.
    #[serde(skip)]
    pub toast: Option<(String, f64, Option<lightcraft_catalog::ColorLabel>)>,
    /// The result of the last Find Missing Photos (it searches in the background).
    #[serde(skip)]
    pub last_find_missing: Option<serde_json::Value>,
    /// The outcome (success or error) of the last Find Missing Photos task.
    #[serde(skip)]
    pub last_find_missing_result: Option<Result<serde_json::Value, String>>,
    /// Injected delay for Find Missing Photos worker (test-only, for timeout testing).
    #[cfg(test)]
    #[serde(skip)]
    pub find_missing_test_delay: Option<std::time::Duration>,
    /// The library (its `Session::library_generation`) whose preview-only raws were rechecked
    /// (`crate::recheck`).
    #[serde(skip)]
    pub rechecked_library: Option<u64>,
    #[serde(skip)]
    pub status: String,
    #[serde(skip)]
    pub dialog: Option<Dialog>,
    /// The Print dialog's page, selection and drag.
    #[serde(skip)]
    pub print: crate::print_view::PrintUi,
}

/// Settings group ids (`SettingsGroup` serde names) a new preset includes by default: everything
/// but crop, masks, spots and red eye.
pub fn default_preset_groups() -> Vec<String> {
    lightcraft_develop::SettingsGroup::default_copy().iter().filter_map(|g| serde_json::to_value(g).ok()?.as_str().map(str::to_string)).collect()
}

impl Dialog {
    /// A fresh Create Preset dialog.
    pub fn create_preset() -> Dialog {
        Dialog::CreatePreset { name: String::new(), group: "User Presets".into(), groups: default_preset_groups() }
    }
}

/// A face's name being typed in the loupe: which face, and what has been typed so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameEdit {
    pub photo: u64,
    pub index: usize,
    pub text: String,
    /// Just opened: the text box takes the keyboard focus once.
    pub fresh: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Dialog {
    /// A Lightroom import result stays readable after its activity row disappears.
    LightroomResult {
        summary: String,
        library: Option<String>,
        details: Vec<String>,
        first: Option<u64>,
    },
    /// File ▸ Print…: the Print module. Its state is the session's (`print.*`); the page on show
    /// and the selected cell are [`UiState::print`].
    Print,
    ContactSheet {
        options: lightcraft_engine::contact_sheet::Options,
    },
    /// View ▸ Export Slideshow…: format, size, quality, frame rate, container.
    SlideshowExport {
        settings: lightcraft_engine::slideshow_export::ExportSettings,
    },
    /// `parent`: the folder to create it in (none: the top level). `include_selected`: the new album
    /// holds the selected photos (Lightroom's "Include selected photos"; folders hold none).
    NewAlbum {
        name: String,
        folder: bool,
        #[serde(default)]
        parent: Option<u64>,
        #[serde(default = "yes")]
        include_selected: bool,
    },
    RenameAlbum {
        id: u64,
        name: String,
    },
    /// One text field; OK runs `command` with `params` plus `{key: value}` (rename a preset or a
    /// version, move a preset to a new group…).
    TextPrompt {
        title: String,
        hint: String,
        value: String,
        command: String,
        params: serde_json::Value,
        key: String,
    },
    /// Edit Capture Time: `mode` "set" (`time`; the others shift along), "shift" (by `days`,
    /// `hours`, `minutes`) or "zone" (time-zone shift by `zone` hours).
    CaptureTime {
        mode: String,
        time: String,
        days: i32,
        hours: i32,
        minutes: i32,
        zone: f32,
    },
    /// The import review (File → Import Photos…).
    Import {
        opts: Box<crate::import::ImportDialog>,
    },
    /// Edit the colour label names (red, yellow, green, blue, purple; empty = the colour's name).
    LabelNames {
        names: Vec<String>,
        /// Also save the names as a label set of this name (empty = don't).
        save_as: String,
    },
    /// Batch rename the selected photos with a file-name template.
    Rename {
        template: String,
        start: u32,
    },
    /// Rename a keyword on every photo (children included).
    RenameKeyword {
        from: String,
        to: String,
    },
    /// Rename every face of `from` to `to`. `merge` is the user's yes to combining with the person
    /// `to` already names.
    RenamePerson {
        from: String,
        to: String,
        merge: bool,
    },
    /// Create a keyword, or edit one (`editing`): Lightroom Classic's Create / Edit Keyword Tag.
    KeywordTag {
        /// The keyword being edited; `None`: a new one.
        editing: Option<String>,
        name: String,
        /// A new keyword goes inside this one when `inside` is on (the keyword picked in the
        /// list, or the default parent).
        parent: Option<String>,
        inside: bool,
        /// Comma-separated.
        synonyms: String,
        include_on_export: bool,
        export_containing: bool,
        export_synonyms: bool,
        person: bool,
        /// A new keyword is given to the selected photos.
        add_to_selected: bool,
        /// Opened from the sidebar's Keywords header: the new keyword is shown there once made.
        #[serde(default)]
        reveal: bool,
    },
    /// Edit Keyword Set: a set's name and its nine slots (an empty one is an empty slot). `replaces`:
    /// the set being edited (renamed when the name changes); `None` saves a new set (from Recent
    /// Keywords).
    KeywordSet {
        replaces: Option<String>,
        name: String,
        slots: Vec<String>,
        /// Save as a new set, leaving the edited one (Lightroom Classic's Save as New Preset).
        #[serde(default)]
        as_new: bool,
    },
    /// Move a keyword inside `parent` (`None`: the top level) where one of its name is already:
    /// asks before merging the two.
    MoveKeyword {
        keyword: String,
        parent: Option<String>,
    },
    /// Delete a keyword from every photo and the keyword list, after asking.
    DeleteKeyword {
        keyword: String,
        /// Photos that have it (or one below it).
        count: usize,
    },
    /// Merge keywords into another one on every photo.
    MergeKeywords {
        from: Vec<String>,
        into: String,
    },
    /// Auto-stack by capture time: the largest gap between consecutive shots, in seconds.
    AutoStack {
        gap: f32,
    },
    /// Save the current view (source + filter) as a smart album.
    NewSmartAlbum {
        name: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    /// Help ▸ What's New.
    WhatsNew,
    /// Photo ▸ Assisted Culling: reject photos below this focus score (0 = none), pick the best
    /// of each burst; shots at most `burst_gap` seconds apart and at least `similarity` alike are
    /// a burst.
    Cull {
        reject_below: f32,
        pick_best: bool,
        burst_gap: f32,
        similarity: f32,
    },
    /// Help ▸ System Info: (label, value) rows.
    DenoiseModel {
        info: serde_json::Value,
        accepted: bool,
    },
    /// A face model file the user chose: what it is, its licence terms, and the "I accept" box.
    FaceModel {
        path: String,
        /// The engine's `faces.models.inspect` answer.
        info: serde_json::Value,
        accepted: bool,
    },
    SystemInfo {
        rows: Vec<(String, String)>,
    },
    /// Every metadata field of a photo's file (`photo.allMetadata`), filtered by `search`.
    AllMetadata {
        title: String,
        rows: serde_json::Value,
        search: String,
    },
    /// Create (`id` None) or edit a smart album's rules.
    SmartRules {
        id: Option<u64>,
        name: String,
        rules: lightcraft_catalog::RuleSet,
        /// The folder a new smart album is created in (ignored when editing).
        #[serde(default)]
        parent: Option<u64>,
    },
    /// `groups`: the settings groups the preset includes (`SettingsGroup` ids).
    CreatePreset {
        name: String,
        group: String,
        #[serde(default = "default_preset_groups")]
        groups: Vec<String>,
    },
    CopySettings {
        groups: Vec<String>,
    },
    /// Paste Selected Settings: which of the copied groups to paste.
    PasteSettings {
        groups: Vec<String>,
    },
    /// `resize` is used unless `full_size`; `limit_kb` 0 = no limit; `dir` empty = default export folder.
    Export {
        opts: lightcraft_engine::export::ExportOptions,
        full_size: bool,
        resize: lightcraft_engine::export::Resize,
        /// Name typed for "Save as Preset".
        #[serde(default)]
        preset_name: String,
        limit_kb: u32,
        dir: String,
    },
    /// Photo Merge (HDR / Panorama / HDR Panorama) options; the preview lives in the app.
    Merge {
        opts: crate::merge::MergeDialog,
    },
    /// Settings (preferences): `tab` = general | import | performance | interface.
    Settings {
        tab: String,
    },
    /// Object and Describe masks need the SAM 3 model, which isn't installed: offer to download
    /// it (size, licence, progress). `then`: the AI mask to start once it is there (`kind`
    /// object|prompt, `op` new|add|subtract|intersect).
    SamModel {
        then: Option<(String, String)>,
        /// Why the download couldn't start (shown in the dialog).
        #[serde(default)]
        error: Option<String>,
    },
    /// Open Library was given a Lightroom Classic catalog (issue #891): offer File ▸ Import
    /// Lightroom Catalog… with `catalog` (the `.lrcat` found; `None`: ask for it) instead.
    /// `folder`: what was chosen.
    LightroomCatalog {
        folder: String,
        catalog: Option<String>,
    },
    /// File ▸ Import Lightroom Catalog…: where the catalog at `catalog` goes (issue #2672, see
    /// [`crate::lightroom_destination`]). Merge into the open library `library` (`photos` in it), or
    /// (`new_library`) create the library `name` in the folder `location`, switch to it and import
    /// there. `dont_ask`: merge from now on without asking. `generation`: the library the window
    /// was opened for (`Session::library_generation`): another one opened meanwhile isn't merged into.
    LightroomDestination {
        /// The `.lrcat` file.
        catalog: String,
        /// The open library's name (its folder's name).
        library: String,
        /// The photos in the open library.
        photos: usize,
        /// "Create a new library" is chosen (else "Merge into the open library").
        new_library: bool,
        /// The new library's name (prefilled from the catalog's file name).
        name: String,
        /// The folder the new library goes in (prefilled with the open library's parent folder).
        location: String,
        /// "Don't ask again (always merge into the open library)".
        dont_ask: bool,
        /// `Session::library_generation` when the window opened.
        generation: u64,
    },
    /// Save the selected mask's sliders as a mask effect preset: which sliders (`FIELDS` names,
    /// the colour as `color_sat`), whether it carries Amount and the brush settings.
    SaveEffect {
        name: String,
        group: String,
        fields: Vec<String>,
        amount: bool,
        brush: bool,
    },
    /// Manage Effect Presets: search, the selected effects, an inline rename (old, new),
    /// a delete waiting for confirmation, a new group's name being typed.
    ManageEffects {
        search: String,
        selected: Vec<String>,
        rename: Option<(String, String)>,
        confirm_delete: bool,
        new_group: String,
    },
    /// Confirm deleting mask effect presets.
    DeleteEffects {
        names: Vec<String>,
    },
    /// Confirm moving photos to Recently Deleted.
    ConfirmDelete {
        count: usize,
    },
    /// Confirm moving photos dropped on a Folders row into that folder on disk
    /// (`photo.moveToFolder`). `name`: the folder's name, as the question says it.
    MovePhotos {
        ids: Vec<u64>,
        folder: String,
        name: String,
        dont_ask: bool,
        /// The library the photos were dropped in (`Session::library_generation`): ids are
        /// numbered per library, so in another one they name other photos.
        library: u64,
    },
    BuildSmartPreviews {
        params: serde_json::Value,
        path: std::path::PathBuf,
        count: usize,
        estimated_bytes: u64,
        checking: bool,
        free_bytes: Option<u64>,
        space_error: Option<String>,
    },
    /// Confirm taking a folder's photos out of the library (`library.removeFolder`).
    RemoveFolder {
        path: String,
        /// What the question calls it (a folder's last two names, a disk's name).
        name: String,
        count: usize,
        /// A whole disk or share (`library.removeFolder` takes it only on request).
        disk: bool,
    },
    /// Synchronize Folder: what changed in a library folder on disk, and what to do about it
    /// (`folder.synchronize`; the scan runs in [`crate::sync`]).
    SynchronizeFolder {
        path: String,
        /// What the dialog calls it (a folder's last two names).
        name: String,
        /// A whole disk, or a folder holding disks (`folder.synchronize` `disk`).
        #[serde(default)]
        disk: bool,
        /// What the scan found (`None` while it runs).
        counts: Option<SyncCounts>,
        import_new: bool,
        #[serde(default = "yes")]
        relink_moved: bool,
        remove_missing: bool,
        read_metadata: bool,
    },
    About,
    Shortcuts,
    /// View ▸ View Options (⌘J): the info overlay and the lines of Info 1 and Info 2. Changes apply
    /// as they are made.
    ViewOptions,
}

fn yes() -> bool {
    true
}

/// How many changes a Synchronize Folder scan found, by kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncCounts {
    /// The folder isn't there (moved, renamed, or on a disk that isn't connected).
    #[serde(default)]
    pub offline: bool,
    pub new: usize,
    pub duplicates: usize,
    pub unreadable: usize,
    pub missing: usize,
    pub metadata: usize,
    #[serde(default)]
    pub moved: usize,
}

impl SyncCounts {
    /// The folder is there and the scan found nothing to bring in, relink, remove or read: the
    /// dialog only says so (files it skipped are not changes).
    pub fn up_to_date(&self) -> bool {
        !self.offline && self.new + self.moved + self.missing + self.metadata == 0
    }
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            window: None,
            docking: None,
            docking_version: crate::docking::DOCKING_VERSION,
            language: crate::i18n::default_language(),
            preview_build_seen: None,
            unsaved_seen: false,
            frame_rate: std::env::var("LIGHTCRAFT_FPS").is_ok_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes")),
            luminance_map_restore: None,
            view: ViewMode::Detail,
            left_panel: false,
            left_width: LEFT_WIDTH.default,
            right_width: RIGHT_WIDTH.default,
            right: RightPanel::Edit,
            right_hidden: RightPanel::None,
            presets: false,
            preset_thumbs: false,
            filmstrip: true,
            zoom: Zoom::Fit,
            pan: (0.5, 0.5),
            click_zoom: 100,
            zoom_anim: false,
            before_after: BeforeAfter::Off,
            thumb_size: 220.0,
            open_sections: vec!["light".into()],
            open_flyouts: vec![],
            single_panel: false,
            show_clipping: false,
            histogram: true,
            soft_proof: false,
            proof: lightcraft_engine::pipeline::Proof { dest_warning: false, ..Default::default() },
            proof_profile: None,
            proof_intent: lightcraft_engine::proof::ProofIntent::Perceptual,
            proof_paper_ink: false,
            mask_overlay: true,
            mask_overlay_mode: "color".into(),
            mask_overlay_color: [230, 30, 40],
            mask_overlay_opacity: 50.0,
            mask_pins: true,
            tool_overlay: ToolOverlay::Auto,
            mask_overlay_all: false,
            crop_overlay: CropOverlay::Thirds,
            crop_default_aspect: None,
            crop_overlay_orient: 0,
            show_filenames: true,
            grid_info: "filename".into(),
            show_counts: true,
            face_boxes: true,
            sidebar: Default::default(),
            collapsed_sidebar: Vec::new(),
            name_edit: None,
            person_page: None,
            person_from: None,
            last_person: None,
            cull_burst: None,
            dismissed_faces: Default::default(),
            unnamed_selected: Default::default(),
            unnamed_name: String::new(),
            unnamed_anchor: None,
            unnamed_focus: false,
            hidden_locations: Vec::new(),
            dragging_control: None,
            external_edits: Vec::new(),
            was_focused: true,
            auto_import_at: 0.0,
            sidecar_retry_at: 0.0,
            sidecars_waiting: 0,
            known_cards: None,
            search: String::new(),
            focus_search: false,
            focus_keywords: None,
            renaming_mask: None,
            describe: None,
            detail_due: None,
            sam_downloading: false,
            renaming_component: None,
            quit: false,
            dragging_photos: None,
            reveal_album: None,
            reveal_keyword: None,
            reveal_folder: None,
            dragging_album: None,
            dragging_section: None,
            curve_channel: "parametric".into(),
            mixer_mode: "hue".into(),
            grading_mode: "3way".into(),
            grading_hidden: None,
            tool: String::new(),
            selected_guide: None,
            brush_size: 0.04,
            brush_feather: 50.0,
            brush_flow: 60.0,
            brush_erase: false,
            brush_auto_mask: false,
            brush_pressure: true,
            remove_size: 0.02,
            remove_feather: 50.0,
            remove_opacity: 100.0,
            point_color: 0,
            point_color_visualize: false,
            hdr_visualize: false,
            eye: 0,
            eye_pet: false,
            visualize_spots: false,
            spots_threshold: 50.0,
            auto_advance: false,
            fullscreen: false,
            lights_out: LightsOut::On,
            slideshow: None,
            second_window: false,
            keyword_painter: None,
            keywording_view: KeywordingView::default(),
            keyword_chips_all: None,
            folded_sections: Vec::new(),
            keyword_list_open: Vec::new(),
            keyword_list_selected: None,
            dragging_keyword: None,
            info_overlay: InfoOverlay::Off,
            brief_info: BriefInfo::default(),
            layout_image: LayoutImage::default(),
            navigator: true,
            settings: AppSettings::default(),
            window_fullscreen: None,
            filter_bar: false,
            local_roots: Vec::new(),
            local_browse_root: None,
            compare: None,
            reference: None,
            toast: None,
            last_find_missing: None,
            last_find_missing_result: None,
            #[cfg(test)]
            find_missing_test_delay: None,
            rechecked_library: None,
            status: String::new(),
            dialog: None,
            print: Default::default(),
        }
    }
}

impl UiState {
    /// Which colour grading wheels (shadows, midtones, highlights) are hidden for viewing `photo`.
    pub fn grading_hidden_for(&self, photo: lightcraft_catalog::PhotoId) -> [bool; 3] {
        match self.grading_hidden {
            Some((p, h)) if p == photo => h,
            _ => [false; 3],
        }
    }

    /// Show or hide colour grading wheel `i` (0 shadows, 1 midtones, 2 highlights) on `photo`.
    pub fn set_grading_hidden(&mut self, photo: lightcraft_catalog::PhotoId, i: usize, hidden: bool) {
        let mut h = self.grading_hidden_for(photo);
        if let Some(slot) = h.get_mut(i) {
            *slot = hidden;
        }
        self.grading_hidden = h.iter().any(|x| *x).then_some((photo, h));
    }

    /// A right-panel section that folds by its title is folded shut.
    pub fn section_folded(&self, id: &str) -> bool {
        self.folded_sections.iter().any(|s| s == id)
    }
    pub fn toggle_folded(&mut self, id: &str) {
        if self.section_folded(id) {
            self.folded_sections.retain(|s| s != id);
        } else {
            self.folded_sections.push(id.to_string());
        }
    }

    pub fn section_open(&self, id: &str) -> bool {
        self.open_sections.iter().any(|s| s == id)
    }
    pub fn toggle_section(&mut self, id: &str) {
        if self.section_open(id) {
            self.open_sections.retain(|s| s != id);
        } else if self.single_panel {
            self.solo_section(id);
        } else {
            self.open_sections.push(id.to_string());
        }
    }
    /// Opens `id` and closes every other Edit section (⌥-click on a section header).
    pub fn solo_section(&mut self, id: &str) {
        self.open_sections.clear();
        self.open_sections.push(id.to_string());
    }
    /// Turns Solo Mode on or off. Turning it on keeps only the section opened last.
    pub fn set_single_panel(&mut self, on: bool) {
        self.single_panel = on;
        if on {
            let keep = self.open_sections.len().saturating_sub(1);
            self.open_sections.drain(..keep);
        }
    }
    /// Whether the sidebar section with this id is folded shut (an unknown id: no).
    pub fn sidebar_section_collapsed(&self, id: &str) -> bool {
        crate::sidebar::SidebarSection::from_id(id).is_some_and(|s| self.sidebar.is_collapsed(s))
    }
    pub fn flyout_open(&self, id: &str) -> bool {
        self.open_flyouts.iter().any(|s| s == id)
    }
    pub fn toggle_flyout(&mut self, id: &str) {
        if self.flyout_open(id) {
            self.open_flyouts.retain(|s| s != id);
        } else {
            self.open_flyouts.push(id.to_string());
        }
    }
    /// Clamp values restored from disk.
    pub fn sanitized(mut self) -> Self {
        self.thumb_size = self.thumb_size.clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
        self.left_width = LEFT_WIDTH.clamp(self.left_width);
        self.right_width = RIGHT_WIDTH.clamp(self.right_width);
        self.brush_size = self.brush_size.clamp(0.002, 0.5);
        self.sidebar.adopt_collapsed(&std::mem::take(&mut self.collapsed_sidebar));
        self.dialog = None;
        self.fullscreen = false;
        if !crate::state::PREVIEW_LIMITS.contains(&self.settings.preview_limit) {
            self.settings.preview_limit = AppSettings::default().preview_limit;
        }
        match self.settings.startup_view {
            StartupView::Last => {}
            StartupView::Grid => self.view = ViewMode::PhotoGrid,
            StartupView::Detail => self.view = ViewMode::Detail,
        }
        self
    }
}

/// A setting read leniently: a value that isn't one of its own (a hand-edited file, a newer
/// build's choice) is the default, and the rest of the settings file is still read.
fn lenient<'de, D: serde::Deserializer<'de>, T: Default + serde::de::DeserializeOwned>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// A list of ids read leniently: what isn't a list is none, and what isn't text in it is left out.
fn lenient_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect())
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
