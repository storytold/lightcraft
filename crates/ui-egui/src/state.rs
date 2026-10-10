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

/// App-level preferences (Settings dialog). They belong to the app, not a library, and are
/// saved with the UI state in the app's config folder (`ui.json`, key `settings`); library
/// preferences (import defaults, XMP, cache size) live in the library's `prefs.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// Library opened at launch when no `--library` is given (empty = the default location).
    pub library_path: String,
    pub startup_view: StartupView,
    /// Ask before moving photos to Recently Deleted (keyboard and menu).
    pub confirm_delete: bool,
    /// GPU rendering allowed (`app.gpu`).
    pub gpu: bool,
    /// Largest long edge (pixels) the user lets the loupe render at; 0 = Automatic (the size it is
    /// drawn at, up to the photo's own pixels and [`LOUPE_EDGE_CEILING`]). Not the old
    /// `previewEdge` key: its 2560 px default was the soft-image bug (issue #323).
    pub preview_limit: u32,
    /// Edit in External Editor: the application ("" = the system's default for TIFF files).
    pub external_editor: String,
    /// Memory the caches may hold together, in MB (0 = automatic; `app.memoryBudget`).
    pub memory_mb: u32,
    /// Filmstrip: file names above the thumbnails.
    pub film_names: bool,
    /// Filmstrip: rating / flag / edited badges on the thumbnails.
    pub film_badges: bool,
    /// Grid: when to show the rating / flag / edited badges.
    pub grid_badges: GridBadges,
    /// Shortcuts the user changed (Help ▸ Keyboard Shortcuts): command id → shortcut, `""` = none.
    pub keymap: crate::shortcuts::Keymap,
    /// The keymap set the user's changes apply over (Classic by default).
    pub keymap_set: crate::shortcuts::KeymapSet,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            library_path: String::new(),
            startup_view: StartupView::Last,
            confirm_delete: false,
            gpu: true,
            preview_limit: 0,
            external_editor: String::new(),
            memory_mb: 0,
            film_names: true,
            film_badges: true,
            grid_badges: GridBadges::Auto,
            keymap: Default::default(),
            keymap_set: Default::default(),
        }
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
        self.loupe_edge(wanted_px, native_long_edge, texture_side).min(dac_engine::SourceLevel::Preview.max_edge())
    }

    /// Long edge for the hover (preset / profile) and Before renders, which are stand-ins drawn
    /// over the loupe: capped at the preview source level like the old default.
    pub fn stand_in_edge(&self, loupe_edge: usize, texture_side: usize) -> usize {
        loupe_edge.min(dac_engine::SourceLevel::Preview.max_edge()).min(texture_side.max(MIN_TEXTURE_SIDE))
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiState {
    #[serde(default = "crate::i18n::default_language")]
    pub language: crate::i18n::Locale,
    /// The Build Previews run last announced (its identity, finished?).
    #[serde(skip)]
    pub preview_build_seen: Option<(usize, bool)>,
    /// Saving the library is failing (announced with a toast; the top bar shows a warning until
    /// a later save succeeds).
    #[serde(skip)]
    pub unsaved_seen: bool,
    pub view: ViewMode,
    pub left_panel: bool,
    pub right: RightPanel,
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
    /// Single-panel mode: opening one section closes the others.
    pub single_panel: bool,
    pub show_clipping: bool,
    pub histogram: bool,
    /// Soft proofing (S in the loupe): render as `proof` would hold the photo.
    pub soft_proof: bool,
    pub proof: dac_engine::pipeline::Proof,
    /// Masking: show the selected mask as a rendered overlay (O), how (`MaskView` name, ⇧O cycles),
    /// in which colour and opacity (0..100, colour views), and whether pins are drawn.
    pub mask_overlay: bool,
    pub mask_overlay_mode: String,
    pub mask_overlay_color: [u8; 3],
    pub mask_overlay_opacity: f32,
    pub mask_pins: bool,
    /// The overlay (on, mode) to go back to when Show Luminance Map is turned off.
    #[serde(skip)]
    pub luminance_map_restore: Option<(bool, String)>,
    /// Mirroring of the triangle / spiral crop guides (0..4).
    pub crop_overlay_orient: u8,
    pub crop_overlay: CropOverlay,
    pub show_filenames: bool,
    /// What the square grid's caption shows: `filename`, `exposure` (shutter · aperture · ISO ·
    /// focal length) or `date`.
    pub grid_info: String,
    /// Photo counts next to sources and albums in the left panel.
    pub show_counts: bool,
    /// Face / pet boxes (read from XMP) over the photo in the loupe.
    pub face_boxes: bool,
    /// Left-sidebar sections folded shut by their header (`albums`, `local`, `byDate`,
    /// `keywords`); the rest are open.
    pub collapsed_sidebar: Vec<String>,
    /// A face's name being typed in the loupe.
    #[serde(skip)]
    pub name_edit: Option<NameEdit>,
    /// The person whose page the People view shows (their faces and the faces that look like them); `None`: everyone.
    #[serde(skip)]
    pub person_page: Option<String>,
    /// The person page a photo in the loupe was opened from, and that photo: Escape goes back to the page (not the grid).
    #[serde(skip)]
    pub person_from: Option<(String, u64)>,
    /// The last person page left: the People view offers a way back to it next to its title.
    #[serde(skip)]
    pub last_person: Option<String>,
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
    /// When the watched folder was last scanned (egui time).
    #[serde(skip)]
    pub auto_import_at: f64,
    /// The develop control whose slider is being dragged (geometry sliders show a grid).
    #[serde(skip)]
    pub dragging_control: Option<String>,
    pub search: String,
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
    /// Close the window on the next frame (File → Quit).
    #[serde(skip)]
    pub quit: bool,
    /// Photos being dragged from the grid (dropped on an album to add them).
    #[serde(skip)]
    pub dragging_photos: Option<Vec<u64>>,
    /// An album just made: the Albums tree opens the folders down to it, once.
    #[serde(skip)]
    pub reveal_album: Option<u64>,
    /// An album or folder row being dragged in the sidebar (dropped on a folder to move it there).
    #[serde(skip)]
    pub dragging_album: Option<u64>,
    /// Selected curve channel in the Curve flyout.
    pub curve_channel: String,
    /// Selected mixer mode: "hue" | "saturation" | "luminance" | "all".
    pub mixer_mode: String,
    /// Selected colour grading wheel: "3way" | "shadows" | "midtones" | "highlights" | "global".
    pub grading_mode: String,
    /// Active on-canvas tool: "", "brush", "linear", "radial", "wbPicker", "straighten", "remove".
    pub tool: String,
    pub brush_size: f32,
    pub brush_feather: f32,
    pub brush_flow: f32,
    pub brush_erase: bool,
    /// Brush Auto Mask: dabs stick to areas like the one under the brush centre.
    pub brush_auto_mask: bool,
    /// Remove tool brush: size (fraction of the long edge), feather and opacity (0..100).
    pub remove_size: f32,
    pub remove_feather: f32,
    pub remove_opacity: f32,
    /// Selected Point Color sample.
    pub point_color: usize,
    /// Point Color "Visualize range": the selected sample's range in colour, the rest grey.
    pub point_color_visualize: bool,
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
    #[serde(skip)]
    pub peek: crate::module::EdgeFlags,
    /// The right group's order and hidden panels for the module on screen.
    pub panel_order: Vec<crate::module::PanelId>,
    pub hidden_panels: Vec<crate::module::PanelId>,
    pub screen_mode: crate::module::ScreenMode,
    #[serde(skip)]
    pub lights_out: crate::module::LightsOut,
    pub identity_plate: crate::module::IdentityPlate,
    /// What the secondary window shows, and the photo a locked loupe holds.
    pub second_mode: crate::module::SecondMode,
    pub second_locked: Option<u64>,
    /// The grid cell under the pointer (the secondary window's Live loupe).
    #[serde(skip)]
    pub hovered_photo: Option<u64>,
    /// The keyword painter: clicking a photo in the grid toggles this keyword on it.
    #[serde(skip)]
    pub keyword_painter: Option<String>,
    /// A running slideshow (full screen): seconds per photo, when the next one is due (egui
    /// time), paused.
    #[serde(skip)]
    pub slideshow: Option<(f64, f64, bool)>,
    /// Info overlay on the loupe.
    pub info_overlay: InfoOverlay,
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
    pub toast: Option<(String, f64, Option<dac_catalog::ColorLabel>)>,
    /// The result of the last Find Missing Photos (it searches in the background).
    #[serde(skip)]
    pub last_find_missing: Option<serde_json::Value>,
    #[serde(skip)]
    pub status: String,
    #[serde(skip)]
    pub dialog: Option<Dialog>,
}

/// Settings group ids (`SettingsGroup` serde names) a new preset includes by default: everything
/// but crop, masks, spots and red eye.
pub fn default_preset_groups() -> Vec<String> {
    dac_develop::SettingsGroup::default_copy().iter().filter_map(|g| serde_json::to_value(g).ok()?.as_str().map(str::to_string)).collect()
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
    /// `parent`: the folder to create it in (none: the top level).
    NewAlbum {
        name: String,
        folder: bool,
        #[serde(default)]
        parent: Option<u64>,
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
    /// of each burst.
    Cull {
        reject_below: f32,
        pick_best: bool,
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
        rules: dac_catalog::RuleSet,
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
        opts: dac_engine::export::ExportOptions,
        full_size: bool,
        resize: dac_engine::export::Resize,
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
    /// Confirm moving photos to Recently Deleted.
    ConfirmDelete {
        count: usize,
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
    About,
    Shortcuts,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            language: crate::i18n::default_language(),
            preview_build_seen: None,
            unsaved_seen: false,
            luminance_map_restore: None,
            view: ViewMode::Detail,
            left_panel: false,
            left_width: LEFT_WIDTH.default,
            right_width: RIGHT_WIDTH.default,
            right: RightPanel::Edit,
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
            proof: dac_engine::pipeline::Proof { dest_warning: false, ..Default::default() },
            mask_overlay: true,
            mask_overlay_mode: "color".into(),
            mask_overlay_color: [230, 30, 40],
            mask_overlay_opacity: 50.0,
            mask_pins: true,
            crop_overlay: CropOverlay::Thirds,
            crop_overlay_orient: 0,
            show_filenames: true,
            grid_info: "filename".into(),
            show_counts: true,
            face_boxes: true,
            collapsed_sidebar: Vec::new(),
            name_edit: None,
            person_page: None,
            person_from: None,
            last_person: None,
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
            search: String::new(),
            focus_search: false,
            renaming_mask: None,
            describe: None,
            detail_due: None,
            sam_downloading: false,
            renaming_component: None,
            quit: false,
            dragging_photos: None,
            reveal_album: None,
            dragging_album: None,
            curve_channel: "parametric".into(),
            mixer_mode: "hue".into(),
            grading_mode: "3way".into(),
            tool: String::new(),
            brush_size: 0.04,
            brush_feather: 50.0,
            brush_flow: 60.0,
            brush_erase: false,
            brush_auto_mask: false,
            remove_size: 0.02,
            remove_feather: 50.0,
            remove_opacity: 100.0,
            point_color: 0,
            point_color_visualize: false,
            eye: 0,
            eye_pet: false,
            visualize_spots: false,
            spots_threshold: 50.0,
            auto_advance: false,
            fullscreen: false,
            slideshow: None,
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
            peek: Default::default(),
            panel_order: Vec::new(),
            hidden_panels: Vec::new(),
            screen_mode: Default::default(),
            lights_out: Default::default(),
            identity_plate: Default::default(),
            second_mode: Default::default(),
            second_locked: None,
            hovered_photo: None,
            keyword_painter: None,
            info_overlay: InfoOverlay::Off,
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
            status: String::new(),
            dialog: None,
        }
    }
}

impl UiState {
    pub fn section_open(&self, id: &str) -> bool {
        self.open_sections.iter().any(|s| s == id)
    }
    pub fn toggle_section(&mut self, id: &str) {
        if self.section_open(id) {
            self.open_sections.retain(|s| s != id);
        } else {
            if self.single_panel {
                self.open_sections.clear();
            }
            self.open_sections.push(id.to_string());
        }
    }
    pub fn sidebar_section_collapsed(&self, id: &str) -> bool {
        self.collapsed_sidebar.iter().any(|s| s == id)
    }
    pub fn toggle_sidebar_section(&mut self, id: &str) {
        if self.sidebar_section_collapsed(id) {
            self.collapsed_sidebar.retain(|s| s != id);
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
        self.thumb_size = self.thumb_size.clamp(90.0, 480.0);
        self.left_width = LEFT_WIDTH.clamp(self.left_width);
        self.right_width = RIGHT_WIDTH.clamp(self.right_width);
        self.brush_size = self.brush_size.clamp(0.002, 0.5);
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
