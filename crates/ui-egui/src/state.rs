//! UI state (serde: saved as preferences, readable/settable through the control channel).

pub use crate::theme::UiFontSize;
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

impl ViewMode {
    /// The two Library grids ([`Self::PhotoGrid`], [`Self::SquareGrid`]).
    pub fn is_grid(self) -> bool {
        matches!(self, ViewMode::PhotoGrid | ViewMode::SquareGrid)
    }
}

/// The right-hand tool/panel shown next to the tool strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// A tool that takes the panel's place while it is used (Crop, Remove, Masking, Red Eye): on
    /// closing, the panel shown before it comes back ([`UiState::leave_tool`]).
    pub fn is_tool(self) -> bool {
        matches!(self, RightPanel::Crop | RightPanel::Remove | RightPanel::Masking | RightPanel::RedEye)
    }

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

impl CropOverlay {
    /// Every guide, in the order the cycle visits them (`None` hides the guides).
    pub const ALL: [CropOverlay; 7] = [
        CropOverlay::Thirds,
        CropOverlay::Grid,
        CropOverlay::Golden,
        CropOverlay::Diagonal,
        CropOverlay::Triangle,
        CropOverlay::Spiral,
        CropOverlay::None,
    ];

    /// The next guide after `self` among those in `cycle` (Tools ▸ Crop Guide Overlay ▸ Choose
    /// Overlays to Cycle). An empty cycle falls back to all of them; a current guide outside the
    /// cycle steps to the first one in cycle order after it.
    pub fn next_in(self, cycle: &[CropOverlay]) -> CropOverlay {
        let all = CropOverlay::ALL;
        let at = all.iter().position(|o| *o == self).unwrap_or(0);
        let allowed = |o: &CropOverlay| cycle.is_empty() || cycle.contains(o);
        (1..=all.len()).filter_map(|d| all.get((at + d) % all.len())).copied().find(allowed).unwrap_or(self)
    }
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
    SquareGrid,
    Detail,
}

/// What the desktop app opens at launch (Settings ▸ General ▸ At launch; issue #1290). `--library`
/// and `<PREFIX>_LIBRARY` come first either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchLibrary {
    /// The library open when the app last quit (holding Alt/Option as the window appears asks once).
    #[default]
    Last,
    /// A window lists the recent libraries to choose from.
    Ask,
}

/// The Edit panel's grading mode (Window ▸ Grading Mode, `view.gradingMode`): a presentation
/// choice — both modes edit the same photo settings, they only lay the colour tools out
/// differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradingMode {
    /// The Lightroom Classic panels, as the Edit panel always had them.
    Eboda,
    /// The DaVinci Resolve style colour-grading palettes: primaries wheels, curves and scopes
    /// (the default — the fork's own face, with Lightroom's panels one Settings row away).
    #[default]
    Witchcraft,
}

impl GradingMode {
    pub fn id(self) -> &'static str {
        match self {
            GradingMode::Eboda => "eboda",
            GradingMode::Witchcraft => "witchcraft",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "eboda" => Some(GradingMode::Eboda),
            "witchcraft" => Some(GradingMode::Witchcraft),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        match self {
            GradingMode::Eboda => GradingMode::Witchcraft,
            GradingMode::Witchcraft => GradingMode::Eboda,
        }
    }
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

/// How long Show Briefly When Photo Changes shows the info overlay (seconds).
pub const BRIEF_INFO_SECS: f64 = 3.0;

/// When the loupe last changed photo, for Show Briefly When Photo Changes
/// ([`LoupeInfo::briefly`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BriefInfo {
    /// The photo the loupe showed last.
    pub photo: Option<lcc_catalog::PhotoId>,
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
/// How many libraries File ▸ Open Recent Library remembers.
pub const RECENT_LIBRARIES: usize = 8;

/// Whether the interface uses the dark or the light theme (Settings ▸ Interface ▸ Appearance).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    /// Follow the operating system's dark/light setting (dark when it gives no answer).
    Auto,
    /// The dark theme, whatever the system says (the default: the app's look so far).
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
    /// Where the SAM 3 model is kept (Settings ▸ AI Models); empty = the default folder.
    pub sam3_dir: String,
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
    /// Settings ▸ Import ▸ "Import dropped photos without review": photos and folders dropped on the
    /// window are added at once with the library's import settings (Settings ▸ Import) instead of
    /// opening the import review. Off by default: a drop opens the review, as Lightroom does.
    pub import_dropped_directly: bool,
    /// Settings ▸ Import ▸ "Import as a tab" (OneSlips, LR-IMP-WINDOW): Import is a workspace tab
    /// beside Library, where a browsed folder or card is culled and then imported. On by default;
    /// off, Import… opens the import review (the dialog) as before.
    pub import_as_tab: bool,
    /// The Import tab's settings, kept between sessions ([`crate::import_tab::ImportSettings`]).
    pub import_tab: crate::import_tab::ImportSettings,
    /// The Settings section shown last: Settings… opens there again (LR-PREF-WINDOW).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub settings_tab: String,
    /// The Settings window's size in points as the user left it (`None`: the default size; fitted
    /// to the screen when the window opens; OneSlips, LR-PREF-WINDOW).
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub settings_size: Option<[f32; 2]>,
    /// Where the Settings window lists its categories in the Classic layout (Settings ▸ General ▸
    /// Settings Categories; `None`: [`SettingsCategories::Left`]). The Modern layout always lists
    /// them down the left of a full-window sheet.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub settings_categories_classic: Option<SettingsCategories>,
    /// The same in the Compact layout (`None`: [`SettingsCategories::Top`]).
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub settings_categories_compact: Option<SettingsCategories>,
    /// GPU rendering allowed (`app.gpu`).
    pub gpu: bool,
    /// The graphics interface GPU rendering uses (Settings ▸ Performance; applies at next launch).
    /// The window keeps the platform's own; `<PREFIX>_GPU_BACKEND` / `WGPU_BACKEND` win over it.
    pub gpu_backend: GpuBackend,
    /// The graphics card for editing (Settings ▸ Performance): a card's key
    /// (`lcc_gpu::cards::AdapterKey`), or empty for Automatic (the card the screen is connected to).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub gpu_card: String,
    /// The second graphics card, for exports, previews and thumbnails: empty for Automatic (another
    /// card when there is one), `off`, or a card's key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub gpu_helper: String,
    /// Hardware device for AI semantic search vector embeddings (Settings ▸ Performance / Settings ▸ AI Models):
    /// empty or "auto" for Automatic, "cpu" for CPU, or a card's key.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub ml_device: String,
    /// Windows DirectComposition to prevent VRR flicker; opt-in, applies at next launch.
    pub compose_window: bool,
    /// An HDR (extended-range) window where the display offers one; off: always an SDR window.
    /// Applies at next launch. Off helps where the compositor resamples HDR windows on an SDR
    /// monitor (KDE with mixed scaling: small text looked stretched).
    pub hdr_display: bool,
    /// Largest long edge (pixels) the user lets the loupe render at; 0 = Automatic (the size it is
    /// drawn at, up to the photo's own pixels and [`LOUPE_EDGE_CEILING`]). Not the old
    /// `previewEdge` key: its 2560 px default was the soft-image bug (issue #323).
    pub preview_limit: u32,
    /// Keep settled rendering quality during adjustments instead of adaptive drafts.
    pub high_quality_adjustment_previews: bool,
    /// Zoomed HDR views keep their last few windows in GPU memory, so panning back or zooming in
    /// again shows them at once. Used only where the GPU has room for them
    /// ([`crate::render::hdr_cache_fit`]); the Settings dialog says when it hasn't.
    pub hdr_zoom_cache: bool,
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
    /// Photos an export renders side by side (0 = automatic; `app.exportTuning`, "Test my machine").
    #[serde(deserialize_with = "lenient")]
    pub export_lanes: u32,
    /// Exports render on the processor only (the GPU stays for the views); "Test my machine" sets
    /// it when the processor alone was as fast.
    #[serde(deserialize_with = "lenient")]
    pub export_cpu_only: bool,
    /// Device memory exports may use together on the GPU, MB (0 = automatic: 55 % of the card's).
    #[serde(deserialize_with = "lenient")]
    pub gpu_export_mb: u32,
    /// Exports sharing the GPU at once (0 = automatic).
    #[serde(deserialize_with = "lenient")]
    pub gpu_export_jobs: u32,
    /// The graphics cards exports use when there is a second one (Settings ▸ Performance):
    /// both (the default), the card for editing only, or the second card only (to measure it).
    #[serde(deserialize_with = "lenient")]
    pub export_cards: lcc_engine::export::ExportCards,
    /// The last "Test my machine" (Settings ▸ Performance): what it measured and chose.
    #[serde(deserialize_with = "lenient")]
    pub machine_test: Option<lcc_engine::machine_test::MachineReport>,
    /// Filmstrip: file names above the thumbnails.
    pub film_names: bool,
    /// Filmstrip: rating / flag / edited badges on the thumbnails.
    pub film_badges: bool,
    /// Sliders that opt in reach past Lightroom's range (Settings ▸ Interface ▸ Sliders; only the
    /// mask Hue so far: ±180° instead of about ±34°). Off: the standard range; values already past
    /// it are kept, never rewritten.
    pub expanded_sliders: bool,
    /// Grid: when to show the rating / flag / edited badges.
    pub grid_badges: GridBadges,
    /// How far a mouse-wheel notch scrolls the grid and filmstrip, times [`crate::theme::WHEEL_STRIP_SHARE`]
    /// (1 = a third of the view; Preferences → Interface → Wheel scroll speed).
    pub wheel_scroll_speed: f32,
    /// View ▸ Lock Zoom Position (`view.lockZoomPosition`): one zoom and pan for every photo (the
    /// default); off, each photo keeps its own ([`crate::view_state`]).
    pub lock_zoom: bool,
    /// View ▸ Scroll By Row (`view.scrollByRow`): a wheel notch moves the grid a whole row.
    pub scroll_by_row: bool,
    /// View ▸ Enable Mirror Image Mode (`view.mirrorImage`): the views are shown flipped left to
    /// right; photos, edits and exports are untouched.
    pub mirror_image: bool,
    /// Reduce motion (Settings ▸ Interface): no zoom glide, widget easing or turning spinners.
    pub reduce_motion: bool,
    /// When a brush tool is active, the mouse wheel changes its size instead of zooming.
    pub wheel_resizes_brush: bool,
    /// View ▸ View Options ▸ Show message when loading or rendering photos: the loupe says
    /// "Rendering…" while the photo it shows is not the finished render.
    pub loupe_messages: bool,
    /// Shortcuts the user changed (Help ▸ Keyboard Shortcuts): command id → shortcut, `""` = none.
    pub keymap: crate::shortcuts::Keymap,
    /// The keymap set the user's changes apply over (Classic by default).
    pub keymap_set: crate::shortcuts::KeymapSet,
    /// Auto (follow the system), Dark or Light.
    pub appearance_mode: AppearanceMode,
    /// The theme of the dark appearance.
    pub dark_theme: DarkTheme,
    /// The theme of the light appearance.
    pub light_theme: LightTheme,
    /// Interface font size (Settings ▸ Interface).
    pub ui_font_size: UiFontSize,
    /// Panel sliders on one row or two (Settings ▸ Interface).
    pub slider_layout: crate::theme::SliderLayout,
    /// The Compact layout (Settings ▸ General ▸ Layout, `view.compactLayout`): Classic's panels
    /// drawn denser (one-line slider rows, short section headers, sharper corners). Off: Classic
    /// as it always was. The Modern layout overrides it.
    pub compact_layout: bool,
    /// The Edit panel's grading mode (`view.gradingMode`): Witchcraft, the DaVinci Resolve style
    /// colour-grading palettes (the default), or Eboda, the Lightroom Classic panels. Both edit
    /// the same photo; a value this version doesn't know reads as the default.
    #[serde(deserialize_with = "lenient")]
    pub grading_mode: GradingMode,
    /// The Develop tool profiles (Settings ▸ Develop, `view.toolProfile`): which of the Edit panel's
    /// groups and sliders show. Read leniently: unknown ids in a stored profile are dropped.
    pub tool_profiles: crate::tool_profiles::ToolProfiles,
    /// Auto Sync (Edit ▸ Auto Sync, `develop.autoSync`), remembered app-wide: it stays as left
    /// whichever library is open and after a restart.
    pub auto_sync: bool,
    /// Panel slider names and values compact or at the regular text size (Settings ▸ Interface).
    pub slider_text: crate::theme::SliderText,
    /// How dragging a slider and scrubbing its value follow the pointer (Settings ▸ Interface).
    pub slider_control: crate::theme::SliderControl,
    /// Theme shortcuts in the appearance context menu.
    #[serde(deserialize_with = "deserialize_favorite_themes")]
    pub favorite_themes: Vec<String>,
    /// The monitor's ICC profile previews are shown through (`app.displayProfile`; "" = none:
    /// the display is treated as sRGB).
    pub display_profile: String,
    /// The colour readout under the histogram: RGB, HSL or Lab (`view.colorReadout`).
    pub color_readout: crate::color_readout::ColorReadout,
    /// The Keywording box: whole keyword paths, or grouped by the keyword they are inside.
    #[serde(deserialize_with = "lenient")]
    pub keyword_chips: KeywordChips,
    /// View ▸ Focus Point: draw the box the camera focused on (`view.focusPoint`).
    pub focus_point: bool,
    /// View ▸ Zoom to Focus Point: Toggle Zoom (Z) centres the zoomed view on the camera's focus point
    /// when the photo has one (`view.zoomToFocus`).
    pub zoom_to_focus: bool,
    /// View ▸ Space Zooms To: where Toggle Zoom (Space / Z) centres the zoomed photo while Zoom to
    /// Focus Point is on (`view.spaceZoomsTo`).
    #[serde(deserialize_with = "lenient")]
    pub space_zoom: SpaceZoom,
    /// Double-clicking a grid photo opens it in Develop instead of the Library loupe (issue #144;
    /// off by default, as in Lightroom Classic).
    pub double_click_develop: bool,
    /// Hide active filter text / indicator in Develop mode (Settings ▸ Interface ▸ Detail).
    pub hide_filter_in_develop: bool,
    /// Metadata > Show Metadata for Target Photo Only: the Metadata panel shows and edits the active photo,
    /// not the whole selection's mixed values (`metadata.targetOnly`).
    pub metadata_target_only: bool,
    /// Presets panel: list the "User Presets" group first, after Favorites (issue #476; off by
    /// default, as Lightroom Classic keeps groups in name order).
    pub user_presets_first: bool,
    /// View ▸ View Options: the lines the loupe's info overlay shows in Info 1 and Info 2.
    #[serde(deserialize_with = "lenient")]
    pub loupe_info: LoupeInfo,
    /// File ▸ Import Lightroom or Capture One Catalog…: ask whether the catalog merges into the open library or
    /// gets a new one, or always merge (issue #2672; the window's "Don't ask again" picks that).
    #[serde(deserialize_with = "lenient")]
    pub lightroom_import: LightroomImport,
    /// Photo ▸ Edit In: ask which file to edit (a copy with the adjustments, a copy, the original),
    /// or always use the choice made with the window's "Don't ask again" (issue #616).
    #[serde(deserialize_with = "lenient")]
    pub edit_in_mode: EditInMode,
}

/// What Photo ▸ Edit In hands the editor (Settings ▸ External Editing). Saved as `"ask"`,
/// `"copyWithAdjustments"`, `"copy"` or `"original"`; anything else reads as [`Self::Ask`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditInMode {
    /// A window asks each time.
    #[default]
    Ask,
    /// A copy rendered with the edits.
    CopyWithAdjustments,
    /// A copy of the original file, without the edits (JPEG, TIFF, PNG, PSD, WebP; a raw gets a
    /// copy with the adjustments).
    Copy,
    /// The original file itself (same file types as [`Self::Copy`]).
    Original,
}

impl EditInMode {
    /// The `photo.editIn` mode, `None` for [`Self::Ask`].
    pub fn mode(self) -> Option<&'static str> {
        match self {
            EditInMode::Ask => None,
            EditInMode::CopyWithAdjustments => Some("copyWithAdjustments"),
            EditInMode::Copy => Some("copy"),
            EditInMode::Original => Some("original"),
        }
    }

    /// The setting for a `photo.editIn` mode (unknown: [`Self::Ask`]).
    pub fn from_mode(mode: &str) -> Self {
        match mode {
            "copyWithAdjustments" => EditInMode::CopyWithAdjustments,
            "copy" => EditInMode::Copy,
            "original" => EditInMode::Original,
            _ => EditInMode::Ask,
        }
    }
}

/// Where File ▸ Import Lightroom or Capture One Catalog… puts a catalog (Settings ▸ General ▸ Library). Saved as
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

/// The graphics interface for GPU rendering (`AppSettings::gpu_backend`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    /// The platform default (DirectX 12 on Windows, Metal on macOS, Vulkan elsewhere).
    #[default]
    Auto,
    /// DirectX 12 (Windows).
    Dx12,
    /// Vulkan: on Windows it builds the GPU's shaders much faster than DirectX 12 on some machines.
    Vulkan,
}

impl GpuBackend {
    /// The name `lcc_gpu::backend::set_compute_preference` takes.
    pub fn name(self) -> &'static str {
        match self {
            GpuBackend::Auto => "auto",
            GpuBackend::Dx12 => "dx12",
            GpuBackend::Vulkan => "vulkan",
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            library_path: String::new(),
            recent_libraries: Vec::new(),
            sam3_dir: String::new(),
            recent_opened: Default::default(),
            at_launch: LaunchLibrary::Last,
            startup_view: StartupView::Last,
            confirm_delete: true,
            ask_before_move: true,
            close_dialogs_outside: false,
            import_dropped_directly: false,
            import_as_tab: true,
            import_tab: Default::default(),
            settings_tab: String::new(),
            settings_size: None,
            settings_categories_classic: None,
            settings_categories_compact: None,
            gpu: true,
            gpu_backend: GpuBackend::Auto,
            gpu_card: String::new(),
            gpu_helper: String::new(),
            ml_device: String::new(),
            compose_window: false,
            hdr_display: true,
            preview_limit: 0,
            high_quality_adjustment_previews: false,
            hdr_zoom_cache: true,
            external_editor: String::new(),
            external_file: EditFile::default(),
            photocraft_path: String::new(),
            photocraft_file: EditFile { format: EditFormat::Psd, ..EditFile::default() },
            memory_mb: 0,
            export_lanes: 0,
            export_cpu_only: false,
            gpu_export_mb: 0,
            gpu_export_jobs: 0,
            export_cards: Default::default(),
            machine_test: None,
            film_names: true,
            film_badges: true,
            expanded_sliders: false,
            grid_badges: GridBadges::Auto,
            wheel_scroll_speed: 1.0,
            lock_zoom: true,
            scroll_by_row: false,
            mirror_image: false,
            reduce_motion: false,
            wheel_resizes_brush: true,
            loupe_messages: true,
            keymap: Default::default(),
            keymap_set: Default::default(),
            appearance_mode: AppearanceMode::Dark,
            dark_theme: DarkTheme::Charcoal,
            light_theme: LightTheme::Silver,
            ui_font_size: UiFontSize::default(),
            slider_layout: Default::default(),
            compact_layout: false,
            grading_mode: GradingMode::Witchcraft,
            tool_profiles: Default::default(),
            auto_sync: false,
            slider_text: Default::default(),
            slider_control: Default::default(),
            favorite_themes: Vec::new(),
            display_profile: String::new(),
            color_readout: Default::default(),
            keyword_chips: KeywordChips::default(),
            focus_point: false,
            zoom_to_focus: true,
            space_zoom: SpaceZoom::default(),
            double_click_develop: false,
            hide_filter_in_develop: false,
            metadata_target_only: false,
            user_presets_first: false,
            loupe_info: LoupeInfo::default(),
            lightroom_import: LightroomImport::Ask,
            edit_in_mode: EditInMode::Ask,
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
        self.loupe_edge(wanted_px, native_long_edge, texture_side).min(lcc_engine::SourceLevel::Preview.max_edge())
    }

    /// Long edge for the hover (preset / profile) and Before renders, which are stand-ins drawn
    /// over the loupe: capped at the preview source level like the old default.
    pub fn stand_in_edge(&self, loupe_edge: usize, texture_side: usize) -> usize {
        loupe_edge.min(lcc_engine::SourceLevel::Preview.max_edge()).min(texture_side.max(MIN_TEXTURE_SIDE))
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

/// Click-zoom ratios offered (percent): 1:1, 2:1, 3:1, 4:1, 8:1, 11:1.
pub const CLICK_ZOOMS: [u32; 6] = [100, 200, 300, 400, 800, 1100];

/// The deepest zoom (percent): 11:1.
pub const MAX_ZOOM: f32 = 1100.0;

/// The fixed zoom levels below Fit and Fill, as (name, percent): what Zoom In / Out step through
/// and `view.zoomLevel` names.
pub const ZOOM_LEVELS: [(&str, f32); 9] = [
    ("1:4", 25.0),
    ("1:3", 100.0 / 3.0),
    ("1:2", 50.0),
    ("1:1", 100.0),
    ("2:1", 200.0),
    ("3:1", 300.0),
    ("4:1", 400.0),
    ("8:1", 800.0),
    ("11:1", 1100.0),
];

/// The zoom a level name stands for: `fit`, `fill` or one of [`ZOOM_LEVELS`].
pub fn zoom_level(name: &str) -> Option<Zoom> {
    match name {
        "fit" | "Fit" => Some(Zoom::Fit),
        "fill" | "Fill" => Some(Zoom::Fill),
        _ => ZOOM_LEVELS.iter().find(|(n, _)| *n == name).map(|(_, p)| Zoom::Percent(*p)),
    }
}

/// The next fixed level after `cur` (percent) going in (`up`) or out; `None` past the last one
/// out (Fit).
pub fn zoom_step(cur: f32, up: bool) -> Option<f32> {
    // (a little slack: 1:3 is not a whole percentage)
    if up {
        Some(ZOOM_LEVELS.iter().map(|l| l.1).find(|p| *p > cur + 0.01).unwrap_or(MAX_ZOOM))
    } else {
        ZOOM_LEVELS.iter().rev().map(|l| l.1).find(|p| *p < cur - 0.01)
    }
}

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
/// The filmstrip's height (points; 142 was its fixed height): small enough for a few rows of
/// tiny thumbnails, large enough to judge expressions on them. Its top edge drags it.
pub const FILM_HEIGHT: PanelWidth = PanelWidth { min: 96.0, default: 142.0, max: 360.0 };
/// The right panel (Edit, Masking, Info, …).
pub const RIGHT_WIDTH: PanelWidth = PanelWidth { min: 250.0, default: 270.0, max: 520.0 };
/// The right panel's default width in the Compact layout (LAYOUT-DENSE, ported from OneSlips): the
/// tool row stands in the panel, so it gets the old strip's 48 points, and the tracks are about as
/// long as Lightroom's.
pub const COMPACT_RIGHT_WIDTH: f32 = 318.0;

/// The right panel's width `w` after the layout turned Compact (`compact`) or Classic: a width at
/// the other layout's default moves to this one's, any other stays.
pub fn follow_compact_width(w: f32, compact: bool) -> f32 {
    let (from, to) = if compact { (RIGHT_WIDTH.default, COMPACT_RIGHT_WIDTH) } else { (COMPACT_RIGHT_WIDTH, RIGHT_WIDTH.default) };
    if (w - from).abs() < 0.5 { to } else { w }
}
/// The Import tab's settings column (Import Settings, on the right of the browsed photos).
pub const IMPORT_SETTINGS_WIDTH: PanelWidth = PanelWidth { min: 260.0, default: 330.0, max: 720.0 };
/// The Import tab's Folders panel (the cards, drives and folders on the left of the browsed photos): its right edge
/// drags it wider to read long folder names.
pub const IMPORT_SOURCES_WIDTH: PanelWidth = PanelWidth { min: 180.0, default: 230.0, max: 640.0 };
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

/// Where Toggle Zoom (Space / Z) centres the zoomed photo (View ▸ Space Zooms To).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpaceZoom {
    /// The camera's focus point; without one, the main subject's sharpest open eye (else the
    /// largest face).
    #[default]
    FocusPoint,
    /// The main subject's sharpest open eye (else the largest face); without a face, the camera's
    /// focus point.
    Eye,
}

impl SpaceZoom {
    pub const ALL: [SpaceZoom; 2] = [SpaceZoom::FocusPoint, SpaceZoom::Eye];

    /// The id `view.spaceZoomsTo` takes.
    pub fn id(self) -> &'static str {
        match self {
            SpaceZoom::FocusPoint => "focusPoint",
            SpaceZoom::Eye => "eye",
        }
    }

    /// The menu's words.
    pub fn label(self) -> &'static str {
        match self {
            SpaceZoom::FocusPoint => "Camera Focus Point, Else the Eyes",
            SpaceZoom::Eye => "Main Subject's Eyes, Else the Focus Point",
        }
    }
}

/// One of the Masking brush's settings: Size (Lightroom's 1..100, a size on the screen,
/// [`brush_core_points`]), Feather, Flow and Density (0..100). The A / B presets and the eraser's
/// own settings are each one, like Lightroom's brush A, B and Erase.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrushPreset {
    pub size: f32,
    pub feather: f32,
    pub flow: f32,
    pub density: f32,
}

impl Default for BrushPreset {
    fn default() -> Self {
        Self { size: BRUSH_SIZE_DEFAULT, feather: 50.0, flow: 60.0, density: 100.0 }
    }
}

/// The brush Size slider's range and starting value: Lightroom's Size, a size on the screen.
pub const BRUSH_SIZE_MIN: f32 = 1.0;
pub const BRUSH_SIZE_MAX: f32 = 100.0;
pub const BRUSH_SIZE_DEFAULT: f32 = 15.0;

/// The radius of the brush's solid core, in screen points, at Size `size` (1..100): the same at every
/// zoom, as in Lightroom (Size → core radius measured at Fit on a ~1000-point-wide photo,
/// `lcc_develop::brush_model::size_to_core`).
pub fn brush_core_points(size: f32) -> f32 {
    (1000.0 * lcc_develop::brush_model::size_to_core(f64::from(size))) as f32
}

/// A brush Size read from settings, a preset or a script: Lightroom's 1..100. One below 1 is a size
/// saved as a fraction of the photo (before Size was a size on the screen), brought to the Size
/// that paints the same at Fit.
pub fn brush_size_of(v: f32) -> f32 {
    if !v.is_finite() || v <= 0.0 {
        BRUSH_SIZE_DEFAULT
    } else if v < BRUSH_SIZE_MIN {
        lcc_develop::brush_model::core_to_size(f64::from(v)) as f32
    } else {
        v.min(BRUSH_SIZE_MAX)
    }
}

impl BrushPreset {
    /// Clamp values restored from disk (NaN falls back to the default); a size saved as a photo
    /// fraction becomes the screen Size that paints the same ([`brush_size_of`]).
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let fix = |v: f32, lo: f32, hi: f32, def: f32| if v.is_finite() { v.clamp(lo, hi) } else { def };
        Self {
            size: brush_size_of(self.size),
            feather: fix(self.feather, 0.0, 100.0, d.feather),
            flow: fix(self.flow, 1.0, 100.0, d.flow),
            density: fix(self.density, 0.0, 100.0, d.density),
        }
    }
}

/// Where the Settings window of the Classic and Compact layouts lists its categories (Settings ▸
/// General ▸ Settings Categories).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsCategories {
    /// A list down the window's left side, as the Modern layout's (in a window of normal size).
    Left,
    /// One row of tabs along the window's top.
    Top,
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

/// A short message at the bottom of the photo view: its text, when it goes (app time, s), and the colour label it is
/// shown in, if any.
pub type Toast = (String, f64, Option<lcc_catalog::ColorLabel>);

/// A toast with a button (Undo…): its text, the button's label, the command the button runs and
/// its parameters, and when it goes (app time, s). One at a time; a newer one replaces it.
#[derive(Clone, Debug)]
pub struct ToastAction {
    pub text: String,
    pub button: String,
    pub command: String,
    pub params: serde_json::Value,
    pub until: f64,
}

/// The crop-mask strengths (percent) the Crop panel and `view.cropMaskOpacity` offer; 0 is Off.
pub const CROP_MASK_OPACITY_LEVELS: [u8; 6] = [0, 20, 50, 60, 80, 100];
/// The strength a fresh settings file starts with (close to the former fixed alpha of 150).
pub const DEFAULT_CROP_MASK_OPACITY: u8 = 60;

/// The mask colour alpha (0..=255) for a strength in percent; anything above 100 counts as 100.
pub fn crop_mask_alpha(percent: u8) -> u8 {
    (u16::from(percent.min(100)) * 255 / 100) as u8
}

/// The most toasts on screen at once: a newer one pushes the oldest out.
pub const MAX_TOASTS: usize = 3;

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
    #[serde(default = "crate::i18n::default_language")]
    pub language: crate::i18n::Locale,
    /// The eye focus strip across the top of the loupe (`view.eyeFocus`): each person's sharper
    /// eye at 100 %, with how sharp it is. On and off apart while browsing a folder to cull it (on
    /// at first) and in the library (off at first); see `LccApp::eye_focus_on`.
    #[serde(rename = "eyeFocusLibrary")]
    pub eye_focus: bool,
    pub eye_focus_import: bool,
    /// Android: the first-start question whether to add the camera roll was answered (either
    /// way); Settings ▸ Folders offers it again.
    pub camera_roll_asked: bool,
    /// Android: the camera roll is watched (`camera_roll::watch_tick`): it is listed when the
    /// library opens, when the app comes to the front and every few minutes, and photos new to
    /// the library are imported without the review. Set by adding the roll for keeps (the
    /// first-start prompt's Add Camera Roll, Settings ▸ Folders); the toggle there turns it off.
    pub camera_roll_watch: bool,
    /// The Build Previews run last announced (its identity, finished?).
    #[serde(skip)]
    pub preview_build_seen: Option<(usize, bool)>,
    /// Saving the library is failing (announced with a toast; the top bar shows a warning until
    /// a later save succeeds).
    #[serde(skip)]
    pub unsaved_seen: bool,
    /// Help ▸ Debug ▸ Frame Rate (`view.frameRate`): the frame-rate display
    /// (`crate::frame_rate`). A debugging aid, never saved; `<PREFIX>_FPS=1` starts with it on.
    #[serde(skip)]
    pub frame_rate: bool,
    pub view: ViewMode,
    /// The grid style the user chose last (`view.photoGrid` / `view.squareGrid`; #794). Every path
    /// back to a grid uses it, so Square Grid survives a restart and detours through a photo.
    pub grid_style: ViewMode,
    /// The Modern layout (fork-owned; see [`crate::modern`]). Saved as `touch` before it replaced the touch layout.
    #[serde(alias = "touch")]
    pub modern: crate::modern::ModernUi,
    /// The workspace tab on show, Import or Library (with Settings ▸ Import ▸ Import as a tab).
    #[serde(skip)]
    pub tab: crate::import_tab::Tab,
    /// The library view the Library tab goes back to when the Import tab browsed a folder.
    #[serde(skip)]
    pub library_source: Option<lcc_engine::LibrarySource>,
    /// The view showed a browsed folder last frame ([`crate::import_tab::keep_in_step`]).
    #[serde(skip)]
    pub import_was_browsing: bool,
    /// Working on browsed photos without importing them: the Library tab shows Develop, Print,
    /// Export or Slideshow on the folder's photos while the browse stays on
    /// ([`crate::import_tab::open_without_import`], [`crate::import_tab::keep_in_step`]).
    #[serde(skip)]
    pub import_develop: bool,
    /// The library filter the Import tab put aside on entering it (the tab has no filter bar and a
    /// card's files must not hide behind a Library filter); restored when the Library tab returns.
    #[serde(skip)]
    pub import_shelved_filter: bool,
    pub left_panel: bool,
    pub right: RightPanel,
    /// The panel a toggle (a panel key or button: I, M, R, Q…) opened, with what the right column showed
    /// before it: toggling it off puts back exactly that, closed included. Forgotten once another
    /// toggle opens a panel.
    #[serde(skip)]
    pub toggle_return: Option<(RightPanel, RightPanel)>,
    /// The panel on show before a tool panel ([`RightPanel::is_tool`]) took its place, put back
    /// when the tool closes ([`RightPanel::None`]: the panel was hidden, and stays hidden).
    #[serde(skip)]
    pub tool_return: Option<RightPanel>,
    /// The panel [`UiState::note_right`] last saw.
    #[serde(skip)]
    pub right_seen: RightPanel,
    /// The right panel (and Library's right column) is folded away by the tool strip's top button;
    /// the strip stays, and choosing any of its tools unfolds it.
    pub right_collapsed: bool,
    /// The right panel F8 (`view.rightPanel`) hid, which the next F8 shows again.
    #[serde(skip)]
    pub right_hidden: RightPanel,
    /// Widths of the left sidebar and of the right panel in points (dragging their inner edge
    /// resizes them; within [`LEFT_WIDTH`] / [`RIGHT_WIDTH`]).
    pub left_width: f32,
    /// The Presets Manager's size as the user left it (its content, in points); opened at it
    /// again, capped to the screen (`panels::presets_manager::window_size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presets_manager_size: Option<[f32; 2]>,
    /// The Profile Manager's size as the user left it (as `presets_manager_size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_manager_size: Option<[f32; 2]>,
    pub right_width: f32,
    /// Width of the Import tab's settings column in points (its left edge is dragged to resize
    /// it; within [`IMPORT_SETTINGS_WIDTH`]).
    pub import_settings_width: f32,
    /// Width of the Import tab's Folders panel in points (its right edge is dragged to resize it; within
    /// [`IMPORT_SOURCES_WIDTH`]).
    pub import_sources_width: f32,
    /// Presets panel: show a live thumbnail of the photo with each preset.
    pub preset_thumbs: bool,
    pub filmstrip: bool,
    /// Height of the filmstrip (points): its top edge drags it between [`FILM_HEIGHT`]'s bounds;
    /// kept with the UI settings, so it comes back after a restart.
    pub film_height: f32,
    /// The shot info strip under the photo in the loupe (`view.shotInfo`): aperture, shutter,
    /// ISO, focal length, lens, size in pixels.
    pub shot_info: bool,
    pub zoom: Zoom,
    /// Pan offset of the loupe when zoomed (image-normalized centre).
    pub pan: (f32, f32),
    /// Zoom applied by a click on the image (and Z / Space), in percent (100 = 1:1, 200 = 2:1);
    /// one of [`CLICK_ZOOMS`].
    pub click_zoom: u32,
    /// Animate the loupe rect toward its target (set by a click-zoom).
    #[serde(skip)]
    pub zoom_anim: bool,
    /// A momentary before/after layout (`\`, the Original Photo box): a view mode, not a
    /// workspace, so it never comes back after a restart looking like edits not showing (#1110).
    #[serde(skip)]
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
    /// Which distribution the Histogram panel draws and how tall its bins are, as the *names* of a
    /// [`lcc_raster::HistogramMode`] / [`lcc_raster::HistogramScale`]
    /// (`view.histogram.mode`, `view.histogram.scale`). Stored as text so a value another build
    /// wrote reads as the default instead of failing the whole settings file, and read leniently so
    /// even a value of the wrong *kind* costs only these two settings ([`UiState::sanitized`]
    /// normalizes what they hold).
    #[serde(deserialize_with = "lenient")]
    pub histogram_mode: String,
    #[serde(deserialize_with = "lenient")]
    pub histogram_scale: String,
    /// A second histogram under the first, drawing another distribution (the name of a
    /// [`lcc_raster::HistogramMode`]; empty: none), so e.g. RGB Overlay and RGB Parade show
    /// together (`view.histogram.second`).
    #[serde(default, deserialize_with = "lenient")]
    pub histogram_second: String,
    /// Soft proofing (S in the loupe): render as `proof` would hold the photo.
    pub soft_proof: bool,
    pub proof: lcc_engine::pipeline::Proof,
    /// Soft proofing against a printer / paper ICC profile: (file, name), the intent and Simulate
    /// Paper & Ink. With a profile, `proof.profile` names its table.
    #[serde(skip)]
    pub proof_profile: Option<(String, String)>,
    #[serde(skip)]
    pub proof_intent: lcc_engine::proof::ProofIntent,
    pub proof_paper_ink: bool,
    /// Masking: show the selected mask as a rendered overlay (O), how (`MaskView` name, ⇧O cycles),
    /// in which colour and opacity (0..100, colour views), and whether pins are drawn.
    pub mask_overlay: bool,
    pub mask_overlay_mode: String,
    pub mask_overlay_color: [u8; 3],
    pub mask_overlay_opacity: f32,
    pub mask_pins: bool,
    /// Whether unselected mask pins are shown (Alt+V / Option+V).
    #[serde(default = "yes")]
    pub mask_unselected_pins: bool,
    /// Automatically toggle mask overlay while adjusting or editing.
    #[serde(default = "yes")]
    pub mask_overlay_auto: bool,
    /// When the Remove tool's spot pins and the mask pins show (Shift+H cycles). Mask pins also
    /// need [`mask_pins`](Self::mask_pins).
    pub tool_overlay: ToolOverlay,
    /// Masking: show every visible mask's region as one overlay at once (Ctrl+Shift+O / Cmd+Shift+O),
    /// instead of just the selected mask's ([`mask_overlay`]). A look, not a setting: not kept
    /// between sessions, so the overlay never comes back by itself.
    #[serde(skip)]
    pub mask_overlay_all: bool,
    /// The overlay (on, mode) to go back to when Show Luminance Map is turned off.
    #[serde(skip)]
    pub luminance_map_restore: Option<(bool, String)>,
    /// The mask overlay was switched on with O or the eye by the Masks title: it stays on through
    /// edits (it only hides while a slider is at work). Off, the overlay shows by itself only for a
    /// mask without settings ([`crate::panels::detail::mask_has_settings`]).
    #[serde(skip)]
    pub mask_overlay_pinned: bool,
    /// A mask slider is at work until then: no mask overlay covers the photo.
    #[serde(skip)]
    pub mask_adjusting_until: Option<std::time::Instant>,
    /// A mask was picked (created, selected, an AI click) until then: its overlay shows for a
    /// moment even when it has settings (issue #1109), so the user sees which mask is on; then
    /// judging by its effect takes over again (the overlay auto rule, upstream PR 2920).
    #[serde(skip)]
    pub mask_peek_until: Option<std::time::Instant>,
    /// The mask whose row in the Masks list is under the pointer: its overlay shows while nothing is
    /// being adjusted.
    #[serde(skip)]
    pub mask_hover: Option<u32>,
    /// The mask whose pin was right-clicked on the photo: the photo's menu is that mask's.
    pub mask_menu_for: Option<u32>,
    /// The mask whose pin the pointer rests on (its overlay shows, as for a hovered row).
    pub mask_pin_hover: Option<u32>,
    /// Active mask component index within the currently selected mask.
    #[serde(skip)]
    pub active_mask_component: Option<usize>,
    /// A mask tool armed for a new mask (photo, `mask.add` kind): no mask is selected, and the
    /// mask is made by the first stroke or drag on the photo. Selecting a mask, another photo or
    /// leaving Masking puts it away (one masking mode at a time).
    #[serde(skip)]
    pub mask_new: Option<(u64, String)>,
    /// The photo the armed mask tool belongs to: another photo puts it away.
    #[serde(skip)]
    pub tool_photo: Option<u64>,
    /// What was selected in Masks last frame (photo, mask, the component edited): on another photo
    /// the engine selects the mask's linked copy, and the same component (and an armed brush on
    /// it) carries over when it is the same kind.
    #[serde(skip)]
    pub mask_held: Option<(u64, u32, Option<usize>)>,
    /// A linear or radial gradient being drawn by a drag of a tool armed for a new mask
    /// (mask, component, the drag's start in image coordinates).
    #[serde(skip)]
    pub mask_drawing: Option<(u32, usize, lcc_geom::Point)>,
    /// Mask IDs whose sub-actions are collapsed in the masking panel.
    #[serde(default)]
    pub collapsed_masks: std::collections::HashSet<u32>,
    /// Masks other than the selected one whose components are shown all the same (the chevron):
    /// only the selected mask's branch is open by itself.
    #[serde(skip)]
    pub expanded_masks: std::collections::HashSet<u32>,
    /// Mirroring of the triangle / spiral crop guides (0..4).
    pub crop_overlay_orient: u8,
    pub crop_overlay: CropOverlay,
    /// The guides Cycle Crop Overlay visits (Choose Overlays to Cycle…); never empty.
    pub crop_overlay_cycle: Vec<CropOverlay>,
    /// Book ▸ Book Preferences…: what a new book starts with.
    pub book_prefs: crate::book::BookPrefs,
    /// How dark the mask outside the crop is, in percent: one of `CROP_MASK_OPACITY_LEVELS`.
    pub crop_mask_opacity: u8,
    /// Last explicitly chosen aspect, retained across photos and app launches; `None` (nothing
    /// chosen yet) crops locked to the photo's own shape, as Lightroom starts (#991).
    pub crop_default_aspect: Option<serde_json::Value>,
    pub show_filenames: bool,
    /// What the square grid's caption shows: `filename`, `exposure` (shutter · aperture · ISO ·
    /// focal length) or `date`.
    pub grid_info: String,
    /// Photo counts next to sources and albums in the left panel.
    pub show_counts: bool,
    /// Face / pet boxes (read from XMP) over the photo in the loupe, switched on (View ▸ Face
    /// Boxes). Off by default (they get in the way of looking at the photo); they also show while a
    /// face is being named or a photo was opened from the People view
    /// ([`crate::panels::faces::face_boxes_shown`]). A new key: earlier builds saved `faceBoxes: true`
    /// without the user choosing it, so only a choice made since is kept.
    #[serde(rename = "faceBoxesOn")]
    pub face_boxes: bool,
    /// Photo > People > Draw Face Regions is on: a drag over the loupe's photo draws a face box.
    #[serde(skip)]
    pub draw_faces: bool,
    /// HDR edits are shown in HDR on an HDR display (View ▸ HDR Preview, `view.hdrPreview`);
    /// off, every view shows their SDR rendition, as on an SDR display.
    pub hdr_preview: bool,
    /// The left sidebar's sections: their order, which are hidden and which are folded shut.
    pub sidebar: crate::sidebar::SidebarLayout,
    /// The ids of other folded panels (the Classic modules' `panel:<key>` panels), saved here. Settings files
    /// from before [`UiState::sidebar`] listed the folded sidebar sections here too: [`UiState::sanitized`]
    /// moves those over, so only the `panel:<key>` ids stay (unknown ids are dropped).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collapsed_sidebar: Vec<String>,
    /// Preset and profile groups the user folded or unfolded (`preset:<group>`, `profile:<group>`,
    /// `lut:<group>` → open). A group not listed is folded, unless it holds the current preset or
    /// profile ([`UiState::group_is_open`]). The Export dialog's sections too (`export:<section>`;
    /// one not listed shows its default).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub group_open: std::collections::BTreeMap<String, bool>,
    /// The folded or unfolded sections of the Book, Slideshow and Map side panels (`<module>:<title>` → open),
    /// as the user left them; a section not listed shows its default. Entries that aren't `true`/`false`
    /// are dropped on load, so a hand-edited file can't keep the other settings from loading.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "deserialize_section_open")]
    pub section_open: std::collections::BTreeMap<String, bool>,
    /// Tree rows the user folded or unfolded in the sidebar (folders, collections/albums, etc.),
    /// remembered across sessions (issue #774).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tree_open: std::collections::BTreeMap<String, bool>,
    /// A face's name being typed in the loupe.
    #[serde(skip)]
    pub name_edit: Option<NameEdit>,
    /// The face box the pointer last rested on in the loupe (photo, region): the "chosen face" of Photo ▸ People.
    #[serde(skip)]
    pub face_focus: Option<(u64, usize)>,
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
    /// The "More" faces selected on a person's page (photo, region), the name being typed to assign them to, the face
    /// last clicked (Shift-click selects a range from it) and whether the name box should take the keyboard.
    #[serde(skip)]
    pub person_selected: std::collections::HashSet<(u64, usize)>,
    #[serde(skip)]
    pub person_assign: String,
    #[serde(skip)]
    pub person_anchor: Option<usize>,
    #[serde(skip)]
    pub person_assign_focus: bool,
    /// Local sidebar locations hidden with “Remove from Local” (folders on disk are untouched).
    pub hidden_locations: Vec<String>,
    /// Copies opened in an external editor this session (reloaded when the window is focused
    /// again), and whether the window had focus last frame.
    #[serde(skip)]
    pub external_edits: Vec<u64>,
    #[serde(skip)]
    pub was_focused: bool,
    /// When the watched folder was last scanned (egui time).
    #[serde(skip)]
    pub auto_import_at: f64,
    /// The watched folder that couldn't be listed and why, as the last listing found it: logged once when it
    /// starts and once when the folder can be read again, and shown in Settings ▸ Import.
    #[serde(skip)]
    pub auto_import_error: Option<(String, String)>,
    /// When the automatic XMP writes that failed were last tried again (egui time), and how many were waiting then.
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
    /// Search box uses natural-language (semantic) search (`library.filter {"semantic": …}`) instead of text match.
    #[serde(skip)]
    pub semantic_search: bool,
    /// Focus the search field on the next frame (Edit → Find…).
    #[serde(skip)]
    pub focus_search: bool,
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
    /// each click or description, so the pass runs once the clicking stops.
    #[serde(skip)]
    pub detail_due: Option<(f64, u32)>,
    /// A mask component being renamed inline: (mask id, component index, name).
    pub renaming_component: Option<(u32, usize, String)>,
    /// The Set Role row under a People component, while open: (role, person).
    #[serde(skip)]
    pub role_draft: Option<(String, String)>,
    /// The People picker under Create New Mask, while open: (person, 0 = everyone; parts).
    #[serde(skip)]
    pub people_pick: Option<(u32, Vec<String>)>,
    /// The photo whose faces the People picker asked the AI worker for (asked once), and when.
    #[serde(skip)]
    pub people_asked: Option<(lcc_catalog::PhotoId, web_time::Instant)>,
    /// The Remembered picker under Create New Mask is open.
    #[serde(skip)]
    pub concept_pick: bool,
    /// The Roles window (who has which role, and where) is open.
    pub roles_window: bool,
    /// Show the Who / Set Role / Roles row under a selected People mask component.
    #[serde(default)]
    pub show_people_who: bool,
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
    /// Selected channel of a mask's Curve (no parametric one): "master" | "red" | "green" | "blue".
    pub mask_curve_channel: String,
    /// Selected mixer mode: "hue" | "saturation" | "luminance" | "all".
    pub mixer_mode: String,
    /// What the Color Mixer adjusts by: "hsl" (eight sliders per Hue / Saturation / Luminance) |
    /// "color" (one colour at a time). HSL by default.
    pub mixer_adjust: String,
    /// The HSL group shown: "hue" | "sat" | "lum" | "all".
    pub mixer_group: String,
    /// The Witchcraft HSL Curves panel's curve: "hue" | "sat" | "lum".
    pub hsl_curve: String,
    /// The Primaries palette's wheels: "color" (Lift / Gamma / Gain / Offset) or "log"
    /// (Shadow / Midtone / Highlight / Offset). Which set is edited; both always render.
    pub primaries_tab: String,
    /// Selected colour grading tab: "3way" | "shadows" | "midtones" | "highlights".
    pub grading_mode: String,
    /// Colour grading wheels (shadows, midtones, highlights) hidden for viewing by their eye, and
    /// the photo that applies to: not an edit, and forgotten on moving to another photo or quitting.
    #[serde(skip)]
    pub grading_hidden: Option<(lcc_catalog::PhotoId, [bool; 3])>,
    /// Active on-canvas tool: "", "brush", "linear", "radial", "wbPicker", "straighten", "remove".
    pub tool: String,
    /// The panel layout a picker ([`is_picker`]) was armed in: when it changes (another tab, mixer mode, curve
    /// channel or panel) the picker is put away.
    #[serde(skip)]
    pub picker_armed_in: Option<String>,
    /// Guided Upright: the selected guide (photo id + index) while the tool is open. A Delete /
    /// Backspace on it removes that guide alone; a click away or leaving the tool clears it.
    #[serde(skip)]
    pub selected_guide: Option<(u64, usize)>,
    /// White Balance Selector: side of the averaged square, in preview pixels (1, 3, 5, 9, 15).
    pub wb_sample: u32,
    /// The Remove panel's tool last used ("remove", "fill", "generate", "heal", "clone"): the panel
    /// opens on it again (this session).
    pub remove_tool: String,
    /// Tools ▸ Remove Options ▸ Detect Objects: a Remove stroke picks the object under it (`spot.detectObject`).
    pub remove_detect_objects: bool,
    /// The brush's Size (Lightroom's 1..100, a size on the screen: the core is [`brush_core_points`] at
    /// every zoom; a stroke stores it in photo units as painted), Feather, Flow and Density.
    pub brush_size: f32,
    pub brush_feather: f32,
    pub brush_flow: f32,
    /// Brush Density (0..100): the most a stroke's paint can reach.
    pub brush_density: f32,
    /// Which of Lightroom's two brush presets is in use: 0 = A, 1 = B.
    pub brush_slot: u8,
    /// The A and B brush presets; the slot in use is mirrored by the `brush_*` fields above and
    /// saved back when switching (`brush.preset`).
    pub brush_presets: [BrushPreset; 2],
    /// The eraser's own Size, Feather, Flow and Density, used while erasing (Erase picked, or ⌥ /
    /// Alt held to switch modes) and remembered beside the brush's.
    pub erase_brush: BrushPreset,
    /// ⌥ / Alt held over the photo with the brush this frame (it switches painting and erasing).
    #[serde(skip)]
    pub brush_alt: bool,
    pub brush_erase: bool,
    /// Brush Auto Mask: dabs stick to areas like the one under the brush centre.
    pub brush_auto_mask: bool,
    /// Masking's "Reset settings on new mask", saved with the UI settings (the engine holds it as
    /// `Session::mask_reset_new` while the app runs).
    pub mask_reset_new: bool,
    /// The engine's `mask_reset_new` last frame: a change there came from a command.
    #[serde(skip)]
    pub mask_reset_seen: Option<bool>,
    /// Brush Auto Mask range, −100 (tight) … 0 (Lightroom's limits) … 100 (wide): kept with each stroke painted
    /// ([`lcc_develop::BrushStroke::auto_range`]), so changing it leaves the strokes already painted.
    pub brush_auto_range: f32,
    /// Brush Pen Pressure: a pen's pressure scales each dab's size and flow (a mouse paints at full pressure).
    pub brush_pressure: bool,
    /// Remove tool brush: size (fraction of the long edge), feather and opacity (0..100).
    pub remove_size: f32,
    pub remove_feather: f32,
    pub remove_opacity: f32,
    /// Find Blemishes also looks on the body's skin (shoulders, arms, back).
    pub blemish_body: bool,
    /// Generative fill choices of the Remove panel (prompt, estimate; not saved).
    #[serde(skip)]
    pub genfill: crate::panels::genfill::GenfillUi,
    /// Generative fill: the service last used (its id; empty: the first one set up), how many
    /// variations (each is a paid result; 1 unless changed). Remembered.
    pub genfill_provider: String,
    pub genfill_variations: u32,
    /// Selected Point Color sample.
    pub point_color: usize,
    /// Selected sample of the active mask's local Point Color.
    pub mask_point_color: usize,
    /// The active mask's Color Mixer group shown (`hue`, `sat`, `lum`).
    pub mask_mixer_group: String,
    /// Point Color "Visualize range": the selected sample's range in colour, the rest grey.
    pub point_color_visualize: bool,
    /// Light panel "Visualize HDR": grey below SDR white, colour bands above (HDR edits).
    pub hdr_visualize: bool,
    /// The sRGB colour under the pointer in the loupe and the pass it was read in (shown under
    /// the histogram; [`crate::color_readout`]).
    pub hover_color: Option<(u64, [u8; 3])>,
    /// Red Eye panel: selected correction, and whether new ones are pet eyes.
    pub eye: usize,
    pub eye_pet: bool,
    /// Remove tool: Visualize Spots (high-pass black/white view) and its threshold 0..100.
    pub visualize_spots: bool,
    pub spots_threshold: f32,
    /// The library filter bar above the grid.
    pub filter_bar: bool,
    /// The filter bar's Metadata columns (issue #218): the field each column lists; empty = hidden.
    pub filter_columns: Vec<String>,
    /// The search box at the top of each Metadata column, by column: the text narrows that
    /// column's values as it is typed. Not saved; emptied with the columns and the filter bar.
    #[serde(skip)]
    pub filter_column_search: Vec<String>,
    /// The People view's search box: the text narrows the person cards by name as it is typed.
    /// Not saved; the People button (or emptying the box) shows everyone again.
    #[serde(skip)]
    pub people_search: String,
    /// Folders kept in Local (Browse Folder…, Keep in Local / Add to Local; `local.addRoot`):
    /// listed after the built-in locations, across restarts, whatever is browsed.
    pub local_roots: Vec<String>,
    /// The top-level Local row listed for this session only, for a browsed folder outside every
    /// kept location; it stays while browsing below it (see `panels::left::local_places`).
    #[serde(skip)]
    pub local_browse_root: Option<String>,
    /// Culling: after a rating, flag or colour-label key, move to the next photo.
    pub auto_advance: bool,
    /// Caps Lock is on, as last seen from a letter key: it turns Auto Advance on while it lasts
    /// (see [`crate::shortcuts::caps_lock_from`]).
    #[serde(skip)]
    pub caps_lock: bool,
    /// Show Alternatives: the frames brought in get the original's edit.
    #[serde(skip)]
    pub alternatives_same_edit: bool,
    /// Full-screen preview (F): the photo alone on black, no chrome.
    pub fullscreen: bool,
    /// Window ▸ Second Window.
    pub second_window: bool,
    /// The Classic module on screen, the one before it (⌘⌥↑), and every other module's saved
    /// panel layout (see [`crate::module`]).
    pub module: crate::module::ModuleId,
    pub previous_module: Option<crate::module::ModuleId>,
    pub layouts: std::collections::BTreeMap<crate::module::ModuleId, crate::module::ModuleLayout>,
    /// Modules hidden from the picker (its context menu).
    pub hidden_modules: Vec<crate::module::ModuleId>,
    /// While a placeholder module is on screen: the view it was entered from (a view change leaves it).
    #[serde(skip)]
    pub placeholder_from: Option<(ViewMode, RightPanel)>,
    /// The module bar (F5), the right panel group (F8) and the toolbar (T).
    pub module_bar: bool,
    pub right_edge: bool,
    pub toolbar: bool,
    /// Edges that auto-show at the window edge when hidden, and those peeking now.
    pub auto_show: crate::module::EdgeFlags,
    /// Edges that, hidden, appear on a click at the window edge (Classic's "Auto Hide") and hide
    /// again once the pointer leaves them.
    pub auto_hide: crate::module::EdgeFlags,
    #[serde(skip)]
    pub peek: crate::module::EdgeFlags,
    /// The right group's order and hidden panels for the module on screen.
    pub panel_order: Vec<crate::module::PanelId>,
    pub hidden_panels: Vec<crate::module::PanelId>,
    /// Whether these settings know the Develop module's Classic left column. An older file has
    /// never seen the optional "My Photos" panel that column offers, so [`Self::sanitized`] keeps
    /// that panel hidden rather than letting it appear beside the column it works with.
    #[serde(default)]
    pub develop_classic: bool,
    pub screen_mode: crate::module::ScreenMode,
    #[serde(skip)]
    pub lights_out: crate::module::LightsOut,
    pub identity_plate: crate::module::IdentityPlate,
    /// The Book module's book and view (Phase 3, `crate::book`).
    pub book: crate::book::BookUi,
    /// What the secondary window shows, and the photo a locked loupe holds.
    pub second_mode: crate::module::SecondMode,
    pub second_locked: Option<u64>,
    /// The secondary window's own filter bar (its grid and filmstrip).
    pub second_filter: crate::panels::second::SecondFilter,
    /// The secondary window's filmstrip.
    pub second_filmstrip: bool,
    /// The grid cell under the pointer (the secondary window's Live loupe).
    #[serde(skip)]
    pub hovered_photo: Option<u64>,
    /// Painter, grid cell style, Metadata panel preset ([`crate::libtools`]).
    pub lib: crate::libtools::LibTools,
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
    /// A preset being dragged (its id; onto a folder to move it there).
    #[serde(skip)]
    pub dragging_preset: Option<String>,
    /// A preset folder being dragged (its path; onto another to nest it).
    #[serde(skip)]
    pub dragging_preset_group: Option<String>,
    /// A running slideshow (full screen; see [`crate::panels::slideshow`]).
    #[serde(skip)]
    pub slideshow: Option<Box<crate::panels::slideshow::Run>>,
    /// The Slideshow module (templates, saved slideshows, the slide settings).
    pub slides: crate::slideshow_ui::SlideshowState,
    /// The Web module's templates, template folders and Show Guides.
    pub web: crate::web_templates::WebState,
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
    pub toasts: Vec<Toast>,
    /// The toast with a button, if one is showing ([`ToastAction`]).
    #[serde(skip)]
    pub toast_action: Option<ToastAction>,
    /// The result of the last Find Missing Photos (it searches in the background).
    #[serde(skip)]
    pub last_find_missing: Option<serde_json::Value>,
    /// The report of the last Validate DNG Files (`dng.validate`'s answer; it runs in the background).
    #[serde(skip)]
    pub last_dng_validation: Option<serde_json::Value>,
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
    /// A Settings section to bring into view once (`app.settings {section}`), e.g. `lensProfiles`.
    #[serde(skip)]
    pub settings_section: Option<String>,
    /// Develop ▸ Lens Corrections: the lens maker picked in the lens picker before a model (issue #307).
    #[serde(skip)]
    pub lens_pick_make: Option<String>,
    /// Develop ▸ Lens Corrections ▸ Vignetting Calibration: `vignettingCalibration.photo` for (photo, settings hash), so the panel
    /// reads the raw's header once per change, not every frame.
    #[serde(skip)]
    pub vignetting_calibration_info: Option<(lcc_catalog::PhotoId, u64, serde_json::Value)>,
    /// The photo whose raw Create is measuring on a worker thread (its button waits meanwhile).
    #[serde(skip)]
    pub vignetting_calibration_creating: Option<lcc_catalog::PhotoId>,
    /// What the last Create said about its reference (`vignettingCalibration.create`'s result: exposure, fall-off), for that photo.
    #[serde(skip)]
    pub vignetting_calibration_report: Option<(lcc_catalog::PhotoId, serde_json::Value)>,
    /// A Borderline reference Create measured, kept while the Create Anyway question is open (confirming doesn't
    /// measure again).
    #[serde(skip)]
    pub vignetting_calibration_pending: Option<(lcc_catalog::PhotoId, Box<lcc_engine::cmd::vignetting_calibration::Measured>)>,
    /// The start-up update check has run (once per session; Help ▸ Check for Updates… is manual).
    #[serde(skip)]
    pub update_checked_at_startup: bool,
    /// When an update check was last started (egui time, s).
    #[serde(skip)]
    pub update_checked_at: f64,
    /// A newer release was found at start-up and the user has not been told yet.
    #[serde(skip)]
    pub update_pending: Option<String>,
    /// The start-up update check is currently in flight.
    #[serde(skip)]
    pub startup_update_checking: bool,
    /// The Print dialog's page, selection and drag.
    #[serde(skip)]
    pub print: crate::print_view::PrintUi,
}

/// Settings group ids (`SettingsGroup` serde names) a new preset includes by default: everything
/// but crop, masks, spots and red eye.
pub fn default_preset_groups() -> Vec<String> {
    lcc_develop::SettingsPick::of_groups(lcc_develop::SettingsGroup::default_preset()).to_ids()
}

fn default_true() -> bool {
    true
}

impl Dialog {
    /// A fresh Create Preset dialog.
    /// Ticked and grouped as the last one was (the session's [`lcc_engine::app_store::DialogMemory`]).
    pub fn create_preset(s: &lcc_engine::Session) -> Dialog {
        Dialog::CreatePreset { name: String::new(), group: s.dialogs.preset_group.clone(), groups: s.dialogs.new_preset.to_ids() }
    }

    /// A fresh Create Preset dialog pre-filling `group`.
    pub fn create_preset_in(s: &lcc_engine::Session, group: Option<&str>) -> Dialog {
        Dialog::CreatePreset {
            name: String::new(),
            group: group.map_or_else(|| s.dialogs.preset_group.clone(), str::to_string),
            groups: s.dialogs.new_preset.to_ids(),
        }
    }

    /// A fresh Create Preset Folder dialog.
    pub fn create_preset_folder(parent: Option<String>) -> Dialog {
        Dialog::CreatePresetFolder { name: String::new(), parent }
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
    /// Tools ▸ Crop Guide Overlay ▸ Choose Overlays to Cycle…: the guides `view.cropOverlay` visits.
    CropOverlays {
        picked: Vec<CropOverlay>,
    },
    /// Book ▸ Book Preferences…: what `book.new` starts a book with.
    BookPrefs {
        prefs: crate::book::BookPrefs,
    },
    /// File ▸ Export ▸ Timelapse Video… (`crate::timelapse_ui`); `size` is "WxH".
    Timelapse {
        fps: u32,
        size: String,
        fill: bool,
        hold: u32,
        crossfade: u32,
        prores: bool,
        quality: u8,
    },
    /// File ▸ Email Photo… (`crate::email_photo`): the long edge (pixels) and JPEG quality of the copies.
    EmailPhoto {
        long_edge: u32,
        quality: u8,
    },
    /// A Lightroom import result stays readable after its activity row disappears.
    LightroomResult {
        summary: String,
        library: Option<String>,
        details: Vec<String>,
        first: Option<u64>,
        /// The catalog was Capture One's (the same import reads it); the title and note say so.
        capture_one: bool,
    },
    /// File ▸ Print…: the Print module. Its state is the session's (`print.*`); the page on show
    /// and the selected cell are [`UiState::print`].
    Print,
    ContactSheet {
        options: lcc_engine::contact_sheet::Options,
    },
    /// View ▸ Export Slideshow…: format, size, quality, frame rate, container.
    SlideshowExport {
        settings: lcc_engine::slideshow_export::ExportSettings,
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
    /// Edit Capture Time: `mode` "set" (`time`; the others shift along, or with `each` every photo gets
    /// `time`), "shift" (by `days`, `hours`, `minutes`) or "zone" (time-zone shift by `zone` hours).
    CaptureTime {
        mode: String,
        time: String,
        days: i32,
        hours: i32,
        minutes: i32,
        zone: f32,
        /// "set" with several photos: every one gets `time` instead of moving by the active photo's change
        /// (issue #4205).
        #[serde(default)]
        each: bool,
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
    /// Rename a single photo.
    RenamePhoto {
        id: u64,
        name: String,
    },
    /// Batch rename the selected photos with a file-name template.
    Rename {
        template: String,
        start: u32,
        /// What each `{text:Label}` of the template becomes (typed for this rename only).
        #[serde(default)]
        texts: std::collections::BTreeMap<String, String>,
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
    /// Library ▸ Advanced Search…: the structured query panel (`crate::advanced_search`), shown
    /// over the grid it filters.
    AdvancedSearch {
        #[serde(default)]
        search: crate::advanced_search::Search,
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
        /// Reject photos where a subject's eyes are closed / blurry (`photo.rejectEyes`).
        #[serde(default)]
        closed_eyes: bool,
        #[serde(default)]
        blurry_eyes: bool,
        /// Soft eyes are rejected as well as blurry ones (`photo.rejectEyes` `soft`).
        #[serde(default)]
        soft_eyes: bool,
        /// Everyone's eyes count, not only the main subjects' (`mainOnly: false`).
        #[serde(default)]
        everyone: bool,
        /// Apply the suggested star ratings (`photo.analyze` `applyRatings`; photos the user
        /// rated are left alone).
        #[serde(default = "default_true")]
        suggest_stars: bool,
        /// Apply the suggested Pick / Reject flags (`photo.analyze` `applyFlags`; photos the user
        /// flagged are left alone).
        #[serde(default)]
        suggest_flags: bool,
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
    /// Library ▸ Validate DNG Files: what the check found (it runs in the background). `damaged` and `unverifiable`
    /// hold `name: why` lines.
    DngValidation {
        summary: String,
        damaged: Vec<String>,
        unverifiable: Vec<String>,
    },
    /// Help ▸ Check for Updates…: the release feed's answer, the download and the install
    /// (the panel state lives in the session's updater; see [`crate::panels::update`]).
    Update,
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
        rules: lcc_catalog::RuleSet,
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
    /// Create a new preset folder (or subfolder under `parent`).
    CreatePresetFolder {
        name: String,
        parent: Option<String>,
    },
    CopySettings {
        groups: Vec<String>,
    },
    /// Synchronize Settings (Lightroom Classic's Sync…): the groups the active photo's settings
    /// carry to the other selected photos, plus single Basic Tone controls when Light is unchecked.
    SyncSettings {
        groups: Vec<String>,
        controls: Vec<String>,
    },
    /// Sync Copies: the groups the active photo's settings carry to its master and virtual copies.
    SyncCopies {
        groups: Vec<String>,
    },
    /// Sync Snapshots: the groups the active photo's current edit carries into each of its snapshots.
    SyncSnapshots {
        groups: Vec<String>,
    },
    /// Update Preset with Current Settings: which settings preset `id` keeps (pre-ticked with what it
    /// holds).
    UpdatePreset {
        id: String,
        name: String,
        groups: Vec<String>,
    },
    /// Paste Selected Settings: which of the copied groups to paste.
    PasteSettings {
        groups: Vec<String>,
    },
    /// `resize` is used unless `full_size`; `limit_kb` 0 = no limit; `dir` empty = default export folder.
    Export {
        opts: lcc_engine::export::ExportOptions,
        full_size: bool,
        resize: lcc_engine::export::Resize,
        /// Name typed for "Save as Preset".
        #[serde(default)]
        preset_name: String,
        limit_kb: u32,
        dir: String,
    },
    /// Photo ▸ Enhance… options; the preview lives in the app (`crate::enhance_ui`).
    Enhance {
        opts: crate::enhance_ui::EnhanceDialog,
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
    /// Create Camera Profile from Color Chart; the photos and the report live in the app.
    ChartProfile {
        opts: crate::chart_profile::ChartProfileDialog,
    },
    SamModel {
        then: Option<(String, String)>,
        /// Why the download couldn't start (shown in the dialog).
        #[serde(default)]
        error: Option<String>,
    },
    /// The AI Models dialog: every AI mask model's download, and which one computes Object
    /// masks (`dialog.aiModels`). `error`: why the last action failed.
    AiModels {
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
    /// File ▸ Import Lightroom or Capture One Catalog…: where the catalog at `catalog` goes (issue #2672, see
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
        /// "Create a new catalog" is chosen (else "Merge into the open library").
        new_library: bool,
        /// The new library's name (prefilled from the catalog's file name).
        name: String,
        /// The folder the new library goes in (prefilled with the open library's parent folder).
        location: String,
        /// "Don't ask again (always merge into the open catalog)".
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
    /// The Presets Manager (LR-PRE-MANAGE): the search; the row whose Delete (or Restore
    /// Built-ins) waits for a yes (`preset:<id>`, `group:<path>`, `restore`); the row being renamed
    /// in place and its text.
    PresetsManager {
        query: String,
        confirm: Option<String>,
        rename: Option<(String, String)>,
    },
    /// The Profile Manager (LR-PROF-MANAGE): looks, LUT profiles and camera profiles in one
    /// table. The search; the row whose Delete waits for a yes (`profile:<id>`, `group:<key>`);
    /// the row renamed in place and its text (`profile:<id>`, `group:<key>`).
    ProfileManager {
        query: String,
        confirm: Option<String>,
        rename: Option<(String, String)>,
    },
    /// Confirm deleting mask effect presets.
    DeleteEffects {
        names: Vec<String>,
    },
    /// Before a generative fill provider is used for the first time: what is uploaded (the crop,
    /// shown), the provider's data policy and the cost; OK agrees and generates spot `index`.
    GenfillConsent {
        provider: String,
        index: usize,
        prompt: String,
        variations: u32,
    },
    /// Stop a task whose stop throws work away (a plug-in's wait for a program: "Stop Waiting…" in the activity list).
    /// `task` and `warning` are shown as given (translated, names in); confirming runs `activity.cancel` for `id`.
    StopWaiting {
        id: u64,
        task: String,
        warning: String,
    },
    /// Create measured a reference that doesn't quite look like a flat field (Borderline): `reasons` (translated) say
    /// why; Create Anyway keeps the map measured meanwhile ([`UiState::vignetting_calibration_pending`]) for `photo`.
    VignettingCreateAnyway {
        photo: u64,
        reasons: String,
    },
    /// Create measured a reference for a combination (camera model, lens, aperture, a zoom's focal length) that has one
    /// in use: `question` (translated) names it; Replace withdraws it and keeps the new one, in one undo step.
    VignettingReplace {
        photo: u64,
        question: String,
    },
    /// Confirm moving photos to Recently Deleted or deleting from disk.
    ConfirmDelete {
        count: usize,
        /// How many originals would be moved to the trash by "Delete from Disk".
        #[serde(default)]
        disk_files: usize,
    },
    /// Confirm removing photos from the library for good: the selection in Recently Deleted
    /// (`photo.deletePermanently`), or `all` of it (`library.emptyRecentlyDeleted`).
    ConfirmPurge {
        count: usize,
        all: bool,
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
    /// Photo ▸ Delete Rejected Photos…: how many rejects the view has, and whether their files go
    /// to the system trash too (`library.deleteRejected`).
    DeleteRejected {
        count: usize,
        recycle: bool,
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
        /// With `import_new`, the files whose content a photo already has come in too ("Don't
        /// import files already in the library" unticked; the library's setting, saved on Synchronize).
        #[serde(default)]
        import_duplicates: bool,
        #[serde(default = "yes")]
        relink_moved: bool,
        remove_missing: bool,
        read_metadata: bool,
    },
    About,
    Shortcuts,
    /// Library ▸ View Options (⌘J): grid cell style, index numbers, badges, then the loupe's info
    /// overlay and the lines of Info 1 and Info 2 (`view.viewOptions`). Changes apply as they are made.
    ViewOptions,
    /// Develop ▸ Optics ▸ Save as…: name the preset new photos start from.
    NewPhotoDefault {
        /// Empty = the engine's own name ([`lcc_engine::import::NEW_PHOTO_DEFAULT`]).
        #[serde(default)]
        name: String,
    },
    /// Edit ▸ Edit Watermarks…
    Watermark {
        wm: lcc_engine::export::Watermark,
        #[serde(default)]
        preset_name: String,
    },
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
    /// Files whose content a photo already has.
    pub duplicates: usize,
    /// Of those, the ones "Don't import files already in the catalog" unticked brings in (not copies
    /// of a photo in Recently Deleted: see `lcc_engine::sync::FolderChanges::importable_duplicates`).
    #[serde(default)]
    pub copies: usize,
    pub unreadable: usize,
    pub missing: usize,
    pub metadata: usize,
    #[serde(default)]
    pub moved: usize,
}

impl SyncCounts {
    /// The folder is there and the scan found nothing to bring in, relink, remove or read: the
    /// dialog only says so (files it skipped are not changes). Files the library has elsewhere
    /// ([`SyncCounts::copies`]) can be brought in by unticking "Don't import files already in the
    /// library", so the dialog keeps that choice for them.
    pub fn up_to_date(&self) -> bool {
        !self.offline && self.new == 0 && self.copies == 0 && self.moved == 0 && self.missing == 0 && self.metadata == 0
    }
    /// Whether Synchronize, with Import new photos as `import_new` says and the box under it as
    /// `import_duplicates` says, would bring in, relink, remove or read nothing: [`SyncCounts::up_to_date`],
    /// but new files count only when Import new photos is ticked, and files the library has elsewhere
    /// only when it is and that box brings them in.
    pub fn nothing_to_do(&self, import_new: bool, import_duplicates: bool) -> bool {
        let has_incoming = import_new && (self.new != 0 || (import_duplicates && self.copies != 0));
        !self.offline && !has_incoming && self.moved == 0 && self.missing == 0 && self.metadata == 0
    }
}

/// Whether `tool` is a picker: a targeted adjustment (`tat:*`, drag on a colour or tone) or a colour sampler
/// (Point Color, Color Range). The white balance picker is left out: it puts itself away after one pick and keeps
/// working across panels (W).
pub fn is_picker(tool: &str) -> bool {
    tool.starts_with("tat:") || matches!(tool, "pointColor" | "colorRange")
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            window: None,
            docking: None,
            eye_focus: false,
            eye_focus_import: true,
            language: crate::i18n::default_language(),
            camera_roll_asked: false,
            camera_roll_watch: false,
            preview_build_seen: None,
            unsaved_seen: false,
            frame_rate: lcc_brand::env("FPS").is_some_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes")),
            luminance_map_restore: None,
            mask_overlay_pinned: false,
            mask_adjusting_until: None,
            mask_peek_until: None,
            mask_pin_hover: None,
            mask_menu_for: None,
            mask_hover: None,
            active_mask_component: None,
            mask_new: None,
            tool_photo: None,
            mask_held: None,
            mask_drawing: None,
            collapsed_masks: Default::default(),
            expanded_masks: Default::default(),
            view: ViewMode::Detail,
            grid_style: ViewMode::PhotoGrid,
            modern: crate::modern::ModernUi::default(),
            tab: Default::default(),
            library_source: None,
            import_was_browsing: false,
            import_develop: false,
            import_shelved_filter: false,
            left_panel: false,
            left_width: LEFT_WIDTH.default,
            presets_manager_size: None,
            profile_manager_size: None,
            right_width: RIGHT_WIDTH.default,
            import_settings_width: IMPORT_SETTINGS_WIDTH.default,
            import_sources_width: IMPORT_SOURCES_WIDTH.default,
            right: RightPanel::Edit,
            toggle_return: None,
            tool_return: None,
            picker_armed_in: None,
            right_seen: RightPanel::Edit,
            right_collapsed: false,
            right_hidden: RightPanel::None,
            preset_thumbs: false,
            filmstrip: true,
            film_height: FILM_HEIGHT.default,
            shot_info: true,
            zoom: Zoom::Fit,
            pan: (0.5, 0.5),
            click_zoom: 100,
            zoom_anim: false,
            before_after: BeforeAfter::Off,
            thumb_size: 220.0,
            // "primaries" (the Witchcraft palette) opens with the app: in Eboda the section never
            // renders, so the extra entry is invisible there; a user who closes it keeps that
            open_sections: vec!["light".into(), "primaries".into()],
            open_flyouts: vec![],
            single_panel: false,
            show_clipping: false,
            histogram: true,
            histogram_mode: lcc_raster::HistogramMode::default().name().into(),
            histogram_scale: lcc_raster::HistogramScale::default().name().into(),
            histogram_second: String::new(),
            soft_proof: false,
            proof: lcc_engine::pipeline::Proof { dest_warning: false, ..Default::default() },
            proof_profile: None,
            proof_intent: lcc_engine::proof::ProofIntent::Perceptual,
            proof_paper_ink: false,
            mask_overlay: true,
            mask_overlay_mode: "color".into(),
            mask_overlay_color: [230, 30, 40],
            mask_overlay_opacity: 50.0,
            mask_pins: true,
            mask_unselected_pins: true,
            mask_overlay_auto: true,
            tool_overlay: ToolOverlay::Auto,
            mask_overlay_all: false,
            crop_overlay: CropOverlay::Thirds,
            crop_overlay_cycle: CropOverlay::ALL.to_vec(),
            book_prefs: Default::default(),
            crop_default_aspect: None,
            crop_overlay_orient: 0,
            crop_mask_opacity: DEFAULT_CROP_MASK_OPACITY,
            show_filenames: true,
            grid_info: "filename".into(),
            show_counts: true,
            face_boxes: false,
            draw_faces: false,
            hdr_preview: true,
            sidebar: Default::default(),
            collapsed_sidebar: Vec::new(),
            group_open: Default::default(),
            section_open: Default::default(),
            tree_open: Default::default(),
            name_edit: None,
            face_focus: None,
            person_page: None,
            person_from: None,
            last_person: None,
            cull_burst: None,
            dismissed_faces: Default::default(),
            unnamed_selected: Default::default(),
            unnamed_name: String::new(),
            unnamed_anchor: None,
            unnamed_focus: false,
            person_selected: Default::default(),
            person_assign: String::new(),
            person_anchor: None,
            person_assign_focus: false,
            hidden_locations: Vec::new(),
            dragging_control: None,
            external_edits: Vec::new(),
            was_focused: true,
            auto_import_at: 0.0,
            auto_import_error: None,
            sidecar_retry_at: 0.0,
            sidecars_waiting: 0,
            known_cards: None,
            search: String::new(),
            semantic_search: false,
            focus_search: false,
            renaming_mask: None,
            describe: None,
            detail_due: None,
            sam_downloading: false,
            renaming_component: None,
            roles_window: false,
            show_people_who: false,
            role_draft: None,
            people_pick: None,
            people_asked: None,
            concept_pick: false,
            quit: false,
            dragging_photos: None,
            reveal_album: None,
            reveal_keyword: None,
            reveal_folder: None,
            dragging_album: None,
            dragging_section: None,
            curve_channel: "parametric".into(),
            mask_curve_channel: "master".into(),
            mixer_mode: "red".into(),
            mixer_adjust: "hsl".into(),
            mixer_group: "hue".into(),
            hsl_curve: "hue".into(),
            primaries_tab: "color".into(),
            grading_mode: "3way".into(),
            grading_hidden: None,
            tool: String::new(),
            selected_guide: None,
            wb_sample: 5,
            remove_tool: "remove".into(),
            remove_detect_objects: false,
            brush_size: BRUSH_SIZE_DEFAULT,
            brush_feather: 50.0,
            brush_flow: 60.0,
            brush_density: 100.0,
            brush_slot: 0,
            brush_presets: [BrushPreset::default(), BrushPreset { size: 5.0, feather: 20.0, flow: 100.0, density: 100.0 }],
            erase_brush: BrushPreset { flow: 100.0, ..BrushPreset::default() },
            brush_alt: false,
            brush_erase: false,
            brush_auto_mask: false,
            mask_reset_new: false,
            mask_reset_seen: None,
            brush_auto_range: 0.0,
            brush_pressure: true,
            remove_size: 0.02,
            remove_feather: 50.0,
            remove_opacity: 100.0,
            blemish_body: false,
            genfill: Default::default(),
            genfill_provider: String::new(),
            genfill_variations: 1,
            point_color: 0,
            mask_point_color: 0,
            mask_mixer_group: "hue".into(),
            point_color_visualize: false,
            hdr_visualize: false,
            hover_color: None,
            eye: 0,
            eye_pet: false,
            visualize_spots: false,
            spots_threshold: 50.0,
            auto_advance: false,
            caps_lock: false,
            alternatives_same_edit: true,
            fullscreen: false,
            slideshow: None,
            slides: Default::default(),
            web: Default::default(),
            second_window: false,
            module: Default::default(),
            previous_module: None,
            layouts: Default::default(),
            hidden_modules: Vec::new(),
            placeholder_from: None,
            module_bar: true,
            right_edge: true,
            toolbar: true,
            auto_show: Default::default(),
            auto_hide: Default::default(),
            peek: Default::default(),
            panel_order: Vec::new(),
            // Develop's Classic column offers upstream's My Photos sidebar as a panel of its own:
            // there, but out of the way until it is asked for (the panel header's menu, or
            // `panel.show {"panel": "sources", "visible": true}`)
            hidden_panels: vec![crate::module::PanelId::Sources],
            develop_classic: true,
            screen_mode: Default::default(),
            lights_out: Default::default(),
            identity_plate: Default::default(),
            book: Default::default(),
            second_mode: Default::default(),
            second_locked: None,
            second_filter: Default::default(),
            second_filmstrip: true,
            hovered_photo: None,
            lib: Default::default(),
            keywording_view: KeywordingView::default(),
            keyword_chips_all: None,
            folded_sections: Vec::new(),
            keyword_list_open: Vec::new(),
            keyword_list_selected: None,
            dragging_keyword: None,
            dragging_preset: None,
            dragging_preset_group: None,
            info_overlay: InfoOverlay::Off,
            brief_info: BriefInfo::default(),
            layout_image: LayoutImage::default(),
            navigator: true,
            settings: AppSettings::default(),
            window_fullscreen: None,
            // Lightroom Classic shows the Library Filter bar over the Grid (the fork's default)
            filter_bar: true,
            filter_columns: Vec::new(),
            filter_column_search: Vec::new(),
            people_search: String::new(),
            local_roots: Vec::new(),
            local_browse_root: None,
            compare: None,
            reference: None,
            toasts: Vec::new(),
            toast_action: None,
            last_dng_validation: None,
            last_find_missing: None,
            last_find_missing_result: None,
            #[cfg(test)]
            find_missing_test_delay: None,
            rechecked_library: None,
            status: String::new(),
            dialog: None,
            settings_section: None,
            lens_pick_make: None,
            vignetting_calibration_info: None,
            vignetting_calibration_creating: None,
            vignetting_calibration_report: None,
            vignetting_calibration_pending: None,
            update_checked_at_startup: false,
            update_checked_at: 0.0,
            update_pending: None,
            startup_update_checking: false,
            print: Default::default(),
        }
    }
}

impl UiState {
    /// The newest toast on screen.
    pub fn toast(&self) -> Option<&Toast> {
        self.toasts.last()
    }

    /// Take the newest toast off the screen.
    pub fn take_toast(&mut self) -> Option<Toast> {
        self.toasts.pop()
    }

    /// Show a toast below those already showing: one no longer hides another (upstream #1936). The same message again
    /// (same text and label) is not shown twice: it moves to the newest place and stays until the later of its times.
    /// Past [`MAX_TOASTS`] the oldest goes.
    pub fn show_toast(&mut self, toast: Toast) {
        let (text, until, label) = toast;
        let until = match self.toasts.iter().position(|t| t.0 == text && t.2 == label) {
            Some(at) => self.toasts.remove(at).1.max(until),
            None => until,
        };
        self.toasts.push((text, until, label));
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.drain(..self.toasts.len() - MAX_TOASTS);
        }
    }

    /// Let go of the toasts whose time is over at `now`.
    pub fn expire_toasts(&mut self, now: f64) {
        self.toasts.retain(|t| t.1 >= now);
    }

    /// Which colour grading wheels (shadows, midtones, highlights) are hidden for viewing `photo`.
    pub fn grading_hidden_for(&self, photo: lcc_catalog::PhotoId) -> [bool; 3] {
        match self.grading_hidden {
            Some((p, h)) if p == photo => h,
            _ => [false; 3],
        }
    }

    /// Show or hide colour grading wheel `i` (0 shadows, 1 midtones, 2 highlights) on `photo`.
    pub fn set_grading_hidden(&mut self, photo: lcc_catalog::PhotoId, i: usize, hidden: bool) {
        let mut h = self.grading_hidden_for(photo);
        if let Some(slot) = h.get_mut(i) {
            *slot = hidden;
        }
        self.grading_hidden = h.iter().any(|x| *x).then_some((photo, h));
    }

    /// Where a picker is armed: the panel and the tabs on show in it.
    fn picker_layout(&self) -> String {
        format!(
            "{:?}|{:?}|{}|{}|{}|{}|{}|{}",
            self.view,
            self.right,
            self.flyout_open("pointColor"),
            self.mixer_adjust,
            self.mixer_group,
            self.mixer_mode,
            self.curve_channel,
            self.grading_mode
        )
    }

    /// Once a frame: a picker armed in one layout is put away when the layout changes (another Color Mixer tab,
    /// Hue / Saturation / Luminance, a mixer mode, the curve channel, another panel or view), so its cursor and its
    /// claim on the photo's clicks go with it.
    pub fn check_picker(&mut self) {
        if !is_picker(&self.tool) {
            self.picker_armed_in = None;
            return;
        }
        let here = self.picker_layout();
        match &self.picker_armed_in {
            Some(armed) if *armed != here => {
                self.tool.clear();
                self.picker_armed_in = None;
            }
            Some(_) => {}
            None => self.picker_armed_in = Some(here),
        }
    }

    /// A slider (or dial) was touched: a picker armed is put away.
    pub fn put_away_picker(&mut self) {
        if is_picker(&self.tool) {
            self.tool.clear();
            self.picker_armed_in = None;
        }
    }

    /// Notes the panel on show: a tool panel opened from another panel (or from none) remembers
    /// that one, to put it back when the tool closes; any other panel forgets it.
    pub fn note_right(&mut self) {
        if self.right == self.right_seen {
            return;
        }
        if self.right.is_tool() {
            if !self.right_seen.is_tool() {
                self.tool_return = Some(self.right_seen);
            }
        } else {
            self.tool_return = None;
        }
        self.right_seen = self.right;
    }

    /// The panel to show when the tool panel on show closes: the one shown before it, or none
    /// when the panel was hidden (Edit when nothing is known).
    pub fn leave_tool(&mut self) -> RightPanel {
        self.note_right();
        let back = self.tool_return.take().unwrap_or(RightPanel::Edit);
        if back.is_tool() { RightPanel::Edit } else { back }
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

    /// Whether a mask's components are collapsed in the Masking panel.
    pub fn is_mask_collapsed(&self, id: u32) -> bool {
        self.collapsed_masks.contains(&id)
    }

    /// Toggle whether a mask's components are collapsed in the Masking panel.
    pub fn toggle_mask_collapsed(&mut self, id: u32) {
        if self.collapsed_masks.contains(&id) {
            self.collapsed_masks.remove(&id);
        } else {
            self.collapsed_masks.insert(id);
        }
    }

    /// Whether a mask's components show under it in the Masking panel: the selected mask's unless
    /// its chevron folded them, another mask's only when its chevron opened them.
    pub fn mask_branch_open(&self, id: u32, selected: bool) -> bool {
        if selected { !self.collapsed_masks.contains(&id) } else { self.expanded_masks.contains(&id) }
    }

    /// The chevron on a mask's row: folds or opens its components (see [`Self::mask_branch_open`]).
    pub fn toggle_mask_branch(&mut self, id: u32, selected: bool) {
        if selected {
            self.toggle_mask_collapsed(id);
        } else if !self.expanded_masks.remove(&id) {
            self.expanded_masks.insert(id);
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
    /// Whether the preset / profile group `key` is unfolded: as the user left it, else open only
    /// when it holds the current preset or profile (`holds_current`).
    pub fn group_is_open(&self, key: &str, holds_current: bool) -> bool {
        self.group_open.get(key).copied().unwrap_or(holds_current)
    }

    /// Whether the sidebar tree row `key` is open (remembered across sessions, issue #774).
    pub fn tree_is_open(&self, key: &str, default: bool) -> bool {
        self.tree_open.get(key).copied().unwrap_or(default)
    }

    /// Open or close the sidebar tree row `key`.
    pub fn set_tree_open(&mut self, key: &str, open: bool) {
        self.tree_open.insert(key.to_string(), open);
    }

    /// Whether the sidebar section or panel with this id is folded shut (one never folded: no).
    pub fn sidebar_section_collapsed(&self, id: &str) -> bool {
        match crate::sidebar::SidebarSection::from_id(id) {
            Some(s) => self.sidebar.is_collapsed(s),
            None => self.collapsed_sidebar.iter().any(|c| c == id),
        }
    }
    /// Fold or unfold the sidebar section or panel with this id.
    pub fn toggle_sidebar_section(&mut self, id: &str) {
        if let Some(s) = crate::sidebar::SidebarSection::from_id(id) {
            self.sidebar.toggle_collapsed(s);
        } else if self.collapsed_sidebar.iter().any(|c| c == id) {
            self.collapsed_sidebar.retain(|c| c != id);
        } else {
            self.collapsed_sidebar.push(id.to_string());
        }
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
        self.clamp_ranges();
        self.sidebar.adopt_collapsed(&self.collapsed_sidebar);
        self.collapsed_sidebar.retain(|id| id.starts_with("panel:"));
        // A settings file written before Develop had its Classic column knows nothing of the
        // "My Photos" panel that column offers (upstream's sidebar, kept for upstream pulls): it
        // starts hidden there, so the column is what Develop shows, as for a fresh install.
        if !self.develop_classic {
            self.develop_classic = true;
            let hide = |hidden: &mut Vec<crate::module::PanelId>| {
                if !hidden.contains(&crate::module::PanelId::Sources) {
                    hidden.push(crate::module::PanelId::Sources);
                }
            };
            hide(&mut self.hidden_panels);
            for layout in self.layouts.values_mut() {
                hide(&mut layout.hidden);
            }
        }
        self.dialog = None;
        self.fullscreen = false;
        // a hand-edited file (or a value from another version) must not leave a photo view in the
        // grid-style slot (#794)
        if !self.grid_style.is_grid() {
            self.grid_style = ViewMode::PhotoGrid;
        }
        match self.settings.startup_view {
            StartupView::Last => {}
            StartupView::Grid => self.view = ViewMode::PhotoGrid,
            StartupView::SquareGrid => self.view = ViewMode::SquareGrid,
            StartupView::Detail => self.view = ViewMode::Detail,
        }
        self
    }

    /// Whether the brush erases now: Erase picked, or ⌥ / Alt held to switch modes (the panel, the
    /// cursor and the [ / ] keys then show and change the eraser's settings).
    pub fn brush_erasing(&self) -> bool {
        self.brush_erase != self.brush_alt
    }

    /// The brush's settings for painting (`erase` false) or erasing.
    pub fn brush_set(&self, erase: bool) -> BrushPreset {
        if erase {
            self.erase_brush
        } else {
            BrushPreset { size: self.brush_size, feather: self.brush_feather, flow: self.brush_flow, density: self.brush_density }
        }
    }

    /// Store `s` as the painting (`erase` false) or erasing settings, in the sliders' ranges.
    pub fn set_brush_set(&mut self, erase: bool, s: BrushPreset) {
        let s = BrushPreset { size: s.size.max(BRUSH_SIZE_MIN), ..s }.sanitized();
        if erase {
            self.erase_brush = s;
        } else {
            (self.brush_size, self.brush_feather, self.brush_flow, self.brush_density) = (s.size, s.feather, s.flow, s.density);
        }
    }

    /// The filmstrip's height (points) every view that shows it leaves room for.
    pub fn film_height(&self) -> f32 {
        FILM_HEIGHT.clamp(self.film_height)
    }

    /// Put the numeric and enumerated settings back in their valid ranges. Shared by loading a
    /// settings file ([`Self::sanitized`]) and `ui.set`, which merges values from a script.
    pub fn clamp_ranges(&mut self) {
        self.thumb_size = self.thumb_size.clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
        self.left_width = LEFT_WIDTH.clamp(self.left_width);
        self.right_width = RIGHT_WIDTH.clamp(self.right_width);
        self.import_settings_width = IMPORT_SETTINGS_WIDTH.clamp(self.import_settings_width);
        self.import_sources_width = IMPORT_SOURCES_WIDTH.clamp(self.import_sources_width);
        self.film_height = FILM_HEIGHT.clamp(self.film_height);
        // a histogram mode or scale another build wrote (or a hand-edited file) is the default
        if lcc_raster::HistogramMode::parse(&self.histogram_mode).is_none() {
            self.histogram_mode = lcc_raster::HistogramMode::default().name().into();
        }
        if !self.histogram_second.is_empty() && lcc_raster::HistogramMode::parse(&self.histogram_second).is_none() {
            self.histogram_second.clear();
        }
        if lcc_raster::HistogramScale::parse(&self.histogram_scale).is_none() {
            self.histogram_scale = lcc_raster::HistogramScale::default().name().into();
        }
        self.brush_slot = self.brush_slot.min(1);
        self.brush_presets = self.brush_presets.map(BrushPreset::sanitized);
        let cur = BrushPreset { size: self.brush_size, feather: self.brush_feather, flow: self.brush_flow, density: self.brush_density }.sanitized();
        (self.brush_size, self.brush_feather, self.brush_flow, self.brush_density) = (cur.size, cur.feather, cur.flow, cur.density);
        self.erase_brush = self.erase_brush.sanitized();
        if !crate::state::PREVIEW_LIMITS.contains(&self.settings.preview_limit) {
            self.settings.preview_limit = AppSettings::default().preview_limit;
        }
    }
}

/// The `sectionOpen` map, keeping only `name: true|false` entries (anything else is ignored).
fn deserialize_section_open<'de, D: serde::Deserializer<'de>>(d: D) -> Result<std::collections::BTreeMap<String, bool>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let Some(map) = value.as_object() else { return Ok(Default::default()) };
    Ok(map.iter().filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b))).take(1024).collect())
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
}

#[cfg(test)]
mod toast_tests {
    use super::{MAX_TOASTS, UiState};

    /// Two messages close together both stay on screen; the same one again is not shown twice; past the limit the
    /// oldest goes; each expires at its own time.
    #[test]
    fn toasts_stack_instead_of_replacing_one_another() {
        let mut ui = UiState::default();
        ui.show_toast(("Red Label".into(), 2.0, None));
        ui.show_toast(("Cache cleared".into(), 3.0, None));
        assert_eq!(ui.toasts.len(), 2);
        assert_eq!(ui.toast().map(|t| t.0.as_str()), Some("Cache cleared"));
        ui.show_toast(("Red Label".into(), 4.0, None));
        assert_eq!(ui.toasts.len(), 2, "the same message is shown once");
        assert_eq!(ui.toast().map(|t| (t.0.as_str(), t.1)), Some(("Red Label", 4.0)));
        for i in 0..5 {
            ui.show_toast((format!("m{i}"), 10.0, None));
        }
        assert_eq!(ui.toasts.len(), MAX_TOASTS);
        assert_eq!(ui.toasts.first().map(|t| t.0.as_str()), Some("m2"));
        ui.show_toast(("short".into(), 1.0, None));
        ui.expire_toasts(5.0);
        assert!(ui.toasts.iter().all(|t| t.0 != "short"));
    }
}
