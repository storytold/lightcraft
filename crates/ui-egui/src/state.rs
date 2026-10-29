//! UI state (serde: saved as preferences, readable/settable through the control channel).

use serde::{Deserialize, Serialize};

mod dialog;
mod settings;
mod view;

pub use dialog::{Dialog, NameEdit, SyncCounts, default_preset_groups};
pub use settings::{
    AppSettings, AppearanceMode, DarkTheme, EditFile, EditFormat, EditSpace, GridBadges, KeywordChips, LOUPE_EDGE_CEILING, LOUPE_EDGE_STEP,
    LaunchLibrary, LightTheme, LightroomImport, MIN_TEXTURE_SIDE, PHOTOCRAFT, PREVIEW_LIMITS, RECENT_LIBRARIES, STANDARD_PREVIEW_EDGE, StartupView,
    WINDOW_FRAME_CEILING,
};
pub use view::{
    BRIEF_INFO_SECS, BeforeAfter, BriefInfo, CLICK_ZOOMS, CropOverlay, FOCUS_KEYWORDS_PASSES, InfoOverlay, KeywordingView, LEFT_WIDTH, LayoutImage,
    LightsOut, LoupeInfo, LoupeInfoField, MIN_PHOTO_WIDTH, PanelWidth, RIGHT_WIDTH, RightPanel, THUMB_SIZE_MAX, THUMB_SIZE_MIN, THUMB_SIZE_STEP,
    ToolOverlay, ViewMode, Zoom,
};

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

/// A list of ids read leniently: what isn't a list is none, and what isn't text in it is left out.
fn lenient_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

/// The navigation/document/modal/panel state a gesture was admitted under. The same automation
/// id can mean a different control after any of these changes; the request is canceled rather
/// than retargeted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GestureFrame {
    pub(crate) view: crate::state::ViewMode,
    pub(crate) active: Option<lightcraft_catalog::PhotoId>,
    pub(crate) library: u64,
    pub(crate) dialog: Option<std::mem::Discriminant<crate::state::Dialog>>,
    pub(crate) left_panel: bool,
    pub(crate) right: crate::state::RightPanel,
    pub(crate) presets: bool,
    pub(crate) filmstrip: bool,
}

/// A pointer press delivered to the UI, kept so a canceled gesture knows that a button is down.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InjectedPress {
    pub(crate) button: egui::PointerButton,
}

/// A file `ui.dropFiles` delivers as dropped, as a real OS drop arrives in `raw.dropped_files`.
#[derive(Debug)]
pub(crate) struct InjectedDrop(pub(crate) std::path::PathBuf);

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
