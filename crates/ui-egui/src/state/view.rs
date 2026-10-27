use super::*;

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
