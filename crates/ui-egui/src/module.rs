//! Classic modules (Library, Develop, Map, Book, Slideshow, Print, Web) and the panel framework
//! around them: four panel edges (top = module bar, left, right, bottom = filmstrip) plus the
//! toolbar, each shown, hidden or auto-shown per module; screen modes and lights out.
//!
//! A module is stateless ([`Module`]); its state lives in [`crate::state::UiState`] so it is saved
//! with `ui.json`. The edges of the module on screen are the live `UiState` fields
//! (`left_panel`, `right_edge`, `module_bar`, `filmstrip`, `toolbar`, …); every other module's are
//! kept in [`crate::state::UiState::layouts`] and swapped in by `module.switch`.
//!
//! Library and Develop re-home the existing views: Library shows the grids, the loupe, Compare,
//! Survey and People; Develop is the loupe with an editing panel (Edit, Crop, Remove, Masking, Red
//! Eye) or the Reference view. Opening an editing panel from Library therefore *is* entering
//! Develop, and going back to a grid is entering Library ([`sync`]). Map is [`crate::map`]; Book,
//! Slideshow, Print and Web are placeholders until Phase 3, so the module picker is complete.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DacApp;
use crate::state::{RightPanel, ViewMode};
use crate::theme::Tokens;
use crate::widgets::register;

/// The Classic modules, in module-picker order (⌘⌥1 … ⌘⌥7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModuleId {
    #[default]
    Library,
    Develop,
    Map,
    Book,
    Slideshow,
    Print,
    Web,
}

impl ModuleId {
    pub const ALL: [ModuleId; 7] =
        [ModuleId::Library, ModuleId::Develop, ModuleId::Map, ModuleId::Book, ModuleId::Slideshow, ModuleId::Print, ModuleId::Web];

    pub fn key(self) -> &'static str {
        match self {
            ModuleId::Library => "library",
            ModuleId::Develop => "develop",
            ModuleId::Map => "map",
            ModuleId::Book => "book",
            ModuleId::Slideshow => "slideshow",
            ModuleId::Print => "print",
            ModuleId::Web => "web",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModuleId::Library => "Library",
            ModuleId::Develop => "Develop",
            ModuleId::Map => "Map",
            ModuleId::Book => "Book",
            ModuleId::Slideshow => "Slideshow",
            ModuleId::Print => "Print",
            ModuleId::Web => "Web",
        }
    }

    pub fn parse(s: &str) -> Option<ModuleId> {
        ModuleId::ALL.into_iter().find(|m| m.key().eq_ignore_ascii_case(s) || m.label().eq_ignore_ascii_case(s))
    }

    /// Map … Web: modules with views of their own (Map since P3.3; the others are placeholders
    /// filled in Phase 3). Like a placeholder, they stay until a view or panel changes under them.
    pub fn is_placeholder(self) -> bool {
        !matches!(self, ModuleId::Library | ModuleId::Develop)
    }

    /// The command that switches to this module (`module.library`, …; ⌘⌥1–7 in the Classic keymap).
    pub fn command(self) -> &'static str {
        match self {
            ModuleId::Library => "module.library",
            ModuleId::Develop => "module.develop",
            ModuleId::Map => "module.map",
            ModuleId::Book => "module.book",
            ModuleId::Slideshow => "module.slideshow",
            ModuleId::Print => "module.print",
            ModuleId::Web => "module.web",
        }
    }
}

/// A panel edge (F5 top, F6 bottom, F7 left, F8 right).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

    pub fn parse(s: &str) -> Option<Edge> {
        match s {
            "top" => Some(Edge::Top),
            "bottom" | "filmstrip" => Some(Edge::Bottom),
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }
}

/// One flag per edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EdgeFlags {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl EdgeFlags {
    pub fn get(&self, e: Edge) -> bool {
        match e {
            Edge::Top => self.top,
            Edge::Bottom => self.bottom,
            Edge::Left => self.left,
            Edge::Right => self.right,
        }
    }

    pub fn set(&mut self, e: Edge, on: bool) {
        match e {
            Edge::Top => self.top = on,
            Edge::Bottom => self.bottom = on,
            Edge::Left => self.left = on,
            Edge::Right => self.right = on,
        }
    }
}

/// The panels a module offers in its side groups. The right group is a tab strip (one panel open
/// at a time), so its order is the strip's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PanelId {
    /// Library left: sources, albums, folders, keywords.
    Sources,
    /// Develop left: the presets column.
    Presets,
    Edit,
    Crop,
    Remove,
    Masking,
    RedEye,
    Versions,
    Activity,
    Keywords,
    Info,
    /// Library left (Classic): the loupe's overview and zoom presets.
    Navigator,
    /// Library left: All Photographs, Quick Collection, Previous Import, Missing …
    Catalog,
    /// Library left: disks and the folders photos were imported from.
    Folders,
    /// Library left: collections, collection sets and smart collections.
    Collections,
    /// Library right: relative develop adjustments for the selection.
    QuickDevelop,
    /// Library right: the selected photos' keywords, suggestions and keyword sets.
    Keywording,
    /// Library right: every keyword in the library with counts and attributes.
    KeywordList,
    /// Library right: the selected photos' metadata.
    Metadata,
}

impl PanelId {
    pub fn parse(s: &str) -> Option<PanelId> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn key(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    /// The right-hand panel this is, if it is one.
    pub fn right_panel(self) -> Option<RightPanel> {
        Some(match self {
            PanelId::Edit => RightPanel::Edit,
            PanelId::Crop => RightPanel::Crop,
            PanelId::Remove => RightPanel::Remove,
            PanelId::Masking => RightPanel::Masking,
            PanelId::RedEye => RightPanel::RedEye,
            PanelId::Versions => RightPanel::Versions,
            PanelId::Activity => RightPanel::Activity,
            PanelId::Keywords => RightPanel::Keywords,
            PanelId::Info => RightPanel::Info,
            PanelId::Sources
            | PanelId::Presets
            | PanelId::Navigator
            | PanelId::Catalog
            | PanelId::Folders
            | PanelId::Collections
            | PanelId::QuickDevelop
            | PanelId::Keywording
            | PanelId::KeywordList
            | PanelId::Metadata => return None,
        })
    }
}

/// A module's panel state, saved per module in `ui.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleLayout {
    /// Edges shown (top = module bar, bottom = filmstrip).
    pub shown: EdgeFlags,
    /// Edges that appear while the pointer rests at the window's edge when hidden (auto hide & show).
    pub auto_show: EdgeFlags,
    /// Edges that appear on a click at the window's edge when hidden (auto hide).
    pub auto_hide: EdgeFlags,
    pub toolbar: bool,
    /// The right panel open last in this module.
    pub right: RightPanel,
    /// Develop's presets column.
    pub presets: bool,
    /// Solo mode: opening one section closes the others.
    pub solo: bool,
    /// The right group's panel order (tab strip order); empty = the module's declared order.
    pub order: Vec<PanelId>,
    /// Panels hidden from the right group (its context menu).
    pub hidden: Vec<PanelId>,
}

impl Default for ModuleLayout {
    fn default() -> Self {
        ModuleLayout {
            shown: EdgeFlags { top: true, bottom: true, left: false, right: true },
            auto_show: EdgeFlags::default(),
            auto_hide: EdgeFlags::default(),
            toolbar: true,
            right: RightPanel::None,
            presets: false,
            solo: false,
            order: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

/// Screen modes (⇧F cycles; ⌘⇧F returns to normal).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenMode {
    #[default]
    Normal,
    /// Full screen, the menu (top) bar still shown.
    FullScreenMenu,
    /// Full screen without the menu bar.
    FullScreen,
    /// Full screen with every panel hidden.
    FullScreenHidePanels,
}

impl ScreenMode {
    pub fn parse(s: &str) -> Option<ScreenMode> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn next(self) -> ScreenMode {
        match self {
            ScreenMode::Normal => ScreenMode::FullScreenMenu,
            ScreenMode::FullScreenMenu => ScreenMode::FullScreen,
            ScreenMode::FullScreen | ScreenMode::FullScreenHidePanels => ScreenMode::Normal,
        }
    }

    pub fn window_fullscreen(self) -> bool {
        self != ScreenMode::Normal
    }
}

/// Lights out (L cycles off → dim → black → off): everything but the photo darkens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LightsOut {
    #[default]
    Off,
    Dim,
    Black,
}

impl LightsOut {
    pub fn next(self) -> LightsOut {
        match self {
            LightsOut::Off => LightsOut::Dim,
            LightsOut::Dim => LightsOut::Black,
            LightsOut::Black => LightsOut::Off,
        }
    }

    /// Opacity of the black veil.
    pub fn alpha(self) -> f32 {
        match self {
            LightsOut::Off => 0.0,
            LightsOut::Dim => 0.8,
            LightsOut::Black => 1.0,
        }
    }
}

/// The identity plate at the left of the module bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdentityPlate {
    /// Text shown; empty = the app's name.
    pub text: String,
    /// Draw the brand mark before the text.
    pub mark: bool,
    /// A graphical plate: an SVG, PNG or JPEG file shown instead of the mark and text (empty =
    /// the styled text plate).
    pub image: String,
    /// The text plate's font size (points).
    pub size: f32,
    /// The text plate's colour (`None` = the theme's text colour).
    pub color: Option<[u8; 3]>,
    pub bold: bool,
}

impl Default for IdentityPlate {
    fn default() -> Self {
        IdentityPlate { text: String::new(), mark: true, image: String::new(), size: 17.0, color: None, bold: false }
    }
}

/// `#rrggbb` (or `rrggbb`) → RGB.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some([c(0)?, c(2)?, c(4)?])
}

/// What the secondary window shows (⇧G / ⇧E / ⇧C / ⇧N, ⌘⇧↩).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecondMode {
    Grid,
    /// The active photo, following the main window.
    #[default]
    Loupe,
    /// The photo under the pointer in the main window (else the active one).
    Live,
    /// The photo that was active when it was locked.
    Locked,
    Compare,
    Survey,
    Slideshow,
}

impl SecondMode {
    pub fn parse(s: &str) -> Option<SecondMode> {
        serde_json::from_value(json!(s)).ok()
    }
}

/// A key a module binds over the global keymap: (shortcut, command id, params JSON).
pub type ModuleKey = (&'static str, &'static str, &'static str);

/// A Classic module: which panels it offers and how it draws its toolbar and centre.
pub trait Module: Sync {
    fn id(&self) -> ModuleId;
    fn left_panels(&self) -> &'static [PanelId];
    fn right_panels(&self) -> &'static [PanelId];
    fn toolbar(&self, ui: &mut egui::Ui, app: &mut DacApp);
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp);
    /// Module-local keys, layered over the global keymap (Classic set only).
    fn keymap(&self) -> &'static [ModuleKey];
}

struct Library;
struct Develop;
struct Placeholder(ModuleId);

/// Library's Classic columns.
pub const LIBRARY_LEFT: &[PanelId] = &[PanelId::Navigator, PanelId::Catalog, PanelId::Folders, PanelId::Collections];
pub const LIBRARY_RIGHT: &[PanelId] = &[PanelId::QuickDevelop, PanelId::Keywording, PanelId::KeywordList, PanelId::Metadata];
const DEVELOP_RIGHT: &[PanelId] = &[
    PanelId::Edit,
    PanelId::Crop,
    PanelId::Remove,
    PanelId::Masking,
    PanelId::RedEye,
    PanelId::Versions,
    PanelId::Activity,
    PanelId::Keywords,
    PanelId::Info,
];

/// Library: (`[` / `]` rate in the grids, see `shortcuts::grid_bracket_command`); `\` the filter bar (in a loupe
/// it stays Show Original, see `shortcuts::handle`); `=` / `-` thumbnail size; Home / End the first
/// and last photo.
pub const LIBRARY_KEYS: &[ModuleKey] = &[
    ("\\", "view.filterBar", "{}"),
    ("=", "view.thumbLarger", "{}"),
    ("-", "view.thumbSmaller", "{}"),
    ("Home", "library.first", "{}"),
    ("End", "library.last", "{}"),
];

/// Develop: ⌘U Auto (tone), ⇧⌘U Auto white balance, ⇧Q cycles the selected spot's mode
/// (Remove → Heal → Clone), Home / End the first and last photo.
pub const DEVELOP_KEYS: &[ModuleKey] = &[
    ("Cmd+U", "develop.auto", "{}"),
    ("Cmd+Shift+U", "develop.wb", r#"{"mode": "auto"}"#),
    ("Shift+Q", "spot.cycleMode", "{}"),
    ("Home", "library.first", "{}"),
    ("End", "library.last", "{}"),
];

impl Module for Library {
    fn id(&self) -> ModuleId {
        ModuleId::Library
    }
    fn left_panels(&self) -> &'static [PanelId] {
        LIBRARY_LEFT
    }
    fn right_panels(&self) -> &'static [PanelId] {
        LIBRARY_RIGHT
    }
    fn toolbar(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        crate::panels::bottombar::show(app, ui);
    }
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        views(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        LIBRARY_KEYS
    }
}

impl Module for Develop {
    fn id(&self) -> ModuleId {
        ModuleId::Develop
    }
    fn left_panels(&self) -> &'static [PanelId] {
        &[PanelId::Sources, PanelId::Presets]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        DEVELOP_RIGHT
    }
    fn toolbar(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        crate::panels::bottombar::show(app, ui);
    }
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        views(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        DEVELOP_KEYS
    }
}

impl Module for Placeholder {
    fn id(&self) -> ModuleId {
        self.0
    }
    fn left_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn toolbar(&self, _ui: &mut egui::Ui, _app: &mut DacApp) {}
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        let t = Tokens::get(ui.ctx());
        let mut area = ui.available_rect_before_wrap();
        // the filmstrip stays across modules
        if edge_visible(app, Edge::Bottom) {
            let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
            area.max.y = film.top();
            crate::panels::detail::filmstrip(app, ui, film);
        }
        app.canvas_rect = Some(area);
        ui.allocate_rect(area, Sense::hover());
        register(ui.ctx(), format!("view:module:{}", self.0.key()), area);
        let p = ui.painter();
        p.text(area.center() - vec2(0.0, 14.0), Align2::CENTER_CENTER, crate::i18n::tr(self.0.label()), t.font(28.0), t.text);
        p.text(area.center() + vec2(0.0, 20.0), Align2::CENTER_CENTER, crate::i18n::tr("Coming in Phase 3"), t.font(14.0), t.text_dim);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        &[]
    }
}

static LIBRARY: Library = Library;
static DEVELOP: Develop = Develop;
static SLIDESHOW: Placeholder = Placeholder(ModuleId::Slideshow);
static PRINT: Placeholder = Placeholder(ModuleId::Print);
static WEB: Placeholder = Placeholder(ModuleId::Web);

pub fn get(id: ModuleId) -> &'static dyn Module {
    match id {
        ModuleId::Library => &LIBRARY,
        ModuleId::Develop => &DEVELOP,
        ModuleId::Map => &crate::map::MAP,
        ModuleId::Book => &crate::book::BOOK,
        ModuleId::Slideshow => &SLIDESHOW,
        ModuleId::Print => &PRINT,
        ModuleId::Web => &WEB,
    }
}

/// The Library / Develop centre: the view the user is in.
fn views(ui: &mut egui::Ui, app: &mut DacApp) {
    use crate::panels;
    match app.ui.view {
        ViewMode::PhotoGrid | ViewMode::SquareGrid => panels::grid::show(app, ui),
        ViewMode::Detail => panels::detail::show(app, ui),
        ViewMode::Compare => panels::compare::show_compare(app, ui),
        ViewMode::Survey => panels::compare::show_survey(app, ui),
        ViewMode::Reference => panels::compare::show_reference(app, ui),
        ViewMode::People => panels::people::show(app, ui),
    }
}

/// The module the current view and right panel belong to (Library or Develop).
fn implied(view: ViewMode, right: RightPanel) -> ModuleId {
    if view == ViewMode::Reference || (view == ViewMode::Detail && right.is_edit_tool()) { ModuleId::Develop } else { ModuleId::Library }
}

/// The live panel state as a layout.
fn capture(app: &DacApp) -> ModuleLayout {
    let u = &app.ui;
    let right = match u.module {
        ModuleId::Library if u.right.is_edit_tool() => RightPanel::None,
        _ => u.right,
    };
    ModuleLayout {
        shown: EdgeFlags { top: u.module_bar, bottom: u.filmstrip, left: u.left_panel, right: u.right_edge },
        auto_show: u.auto_show,
        auto_hide: u.auto_hide,
        toolbar: u.toolbar,
        right,
        presets: u.presets,
        solo: u.single_panel,
        order: u.panel_order.clone(),
        hidden: u.hidden_panels.clone(),
    }
}

/// Put `l`'s edges (and with `full`, its right panel) on screen.
fn apply(app: &mut DacApp, l: &ModuleLayout, full: bool) {
    let u = &mut app.ui;
    u.module_bar = l.shown.top;
    u.filmstrip = l.shown.bottom;
    u.left_panel = l.shown.left;
    u.right_edge = l.shown.right;
    u.auto_show = l.auto_show;
    u.auto_hide = l.auto_hide;
    u.toolbar = l.toolbar;
    u.single_panel = l.solo;
    u.panel_order = l.order.clone();
    u.hidden_panels = l.hidden.clone();
    if full {
        u.presets = l.presets;
        u.right = l.right;
    }
}

/// Leave the current module for `to`, saving its panels and restoring `to`'s (`full`: its right
/// panel too). A module seen for the first time keeps the edges as they are.
fn swap(app: &mut DacApp, to: ModuleId, full: bool) {
    let from = app.ui.module;
    if from == to {
        return;
    }
    let saved = capture(app);
    app.ui.layouts.insert(from, saved);
    app.ui.previous_module = Some(from);
    app.ui.module = to;
    if let Some(l) = app.ui.layouts.get(&to).cloned() {
        apply(app, &l, full);
    } else if full && to == ModuleId::Library && app.ui.right.is_edit_tool() {
        app.ui.right = RightPanel::None;
    }
}

/// Follow the view: opening an editing panel from Library enters Develop, returning to a grid
/// enters Library. Placeholder modules stay until a view or panel changes under them.
pub fn sync(app: &mut DacApp) {
    let now = (app.ui.view, app.ui.right);
    if app.ui.module.is_placeholder() {
        match app.ui.placeholder_from {
            None => app.ui.placeholder_from = Some(now),
            Some(was) if was != now => {
                app.ui.placeholder_from = None;
                swap(app, implied(now.0, now.1), false);
            }
            _ => {}
        }
        return;
    }
    let want = implied(now.0, now.1);
    if want != app.ui.module {
        swap(app, want, false);
    }
}

/// `module.switch {module}`.
pub fn switch(app: &mut DacApp, to: ModuleId) -> Result<Value, String> {
    if app.ui.module == to {
        return Ok(json!({"module": to}));
    }
    match to {
        ModuleId::Develop => {
            if app.session.active().is_none() {
                // Develop needs a photo: take the first one shown
                let first = app.session.visible().first().copied();
                let Some(first) = first else { return Err("Develop needs a photo: the library is empty".into()) };
                app.run("library.select", json!({"ids": [first.0]}))?;
            }
            swap(app, to, true);
            if app.ui.view != ViewMode::Reference {
                app.ui.view = ViewMode::Detail;
            }
            if !app.ui.right.is_edit_tool() {
                app.ui.right = RightPanel::Edit;
            }
        }
        ModuleId::Library => {
            swap(app, to, true);
            if app.ui.view == ViewMode::Reference {
                app.ui.view = ViewMode::Detail;
            }
            if app.ui.right.is_edit_tool() {
                app.ui.right = RightPanel::None;
            }
            app.ui.tool.clear();
        }
        _ => {
            swap(app, to, true);
            app.ui.placeholder_from = Some((app.ui.view, app.ui.right));
        }
    }
    let _ = app.session.end_interaction();
    Ok(json!({"module": app.ui.module, "previous": app.ui.previous_module}))
}

fn bool_param(p: &Value, key: &str) -> Option<bool> {
    p.get(key).and_then(Value::as_bool)
}

fn edge_shown(app: &DacApp, e: Edge) -> bool {
    match e {
        Edge::Top => app.ui.module_bar,
        Edge::Bottom => app.ui.filmstrip,
        Edge::Left => app.ui.left_panel,
        Edge::Right => app.ui.right_edge,
    }
}

fn set_edge(app: &mut DacApp, e: Edge, on: bool) {
    match e {
        Edge::Top => app.ui.module_bar = on,
        Edge::Bottom => app.ui.filmstrip = on,
        Edge::Left => app.ui.left_panel = on,
        Edge::Right => app.ui.right_edge = on,
    }
    app.ui.peek.set(e, false);
}

fn edges_json(app: &DacApp) -> Value {
    json!({
        "module": app.ui.module,
        "top": app.ui.module_bar, "bottom": app.ui.filmstrip, "left": app.ui.left_panel, "right": app.ui.right_edge,
        "toolbar": app.ui.toolbar, "autoShow": app.ui.auto_show, "autoHide": app.ui.auto_hide, "solo": app.ui.single_panel,
    })
}

/// UI commands of the module shell: `(id, label, shortcut in the alternative set, menu)`. Their
/// Classic keys come from [`crate::shortcuts::CLASSIC`].
pub const SHELL_COMMANDS: &[crate::menus::UiCommand] = &[
    ("module.switch", "Switch Module", None, ""),
    ("module.library", "Library", None, "Window>Modules"),
    ("module.develop", "Develop", None, "Window>Modules"),
    ("module.map", "Map", None, "Window>Modules"),
    ("module.book", "Book", None, "Window>Modules"),
    ("module.slideshow", "Slideshow", None, "Window>Modules"),
    ("module.print", "Print", None, "Window>Modules"),
    ("module.web", "Web", None, "Window>Modules"),
    ("module.previous", "Go Back to Previous Module", None, "Window"),
    ("module.setVisible", "Show Module in Picker", None, ""),
    ("panel.toggle", "Toggle Panel", None, ""),
    ("panel.top", "Show Module Picker", None, "Window>Panels"),
    ("panel.bottom", "Show Filmstrip", None, "Window>Panels"),
    ("panel.left", "Show Left Module Panels", None, "Window>Panels"),
    ("panel.right", "Show Right Module Panels", None, "Window>Panels"),
    ("panel.sides", "Toggle Side Panels", None, "Window>Panels"),
    ("panel.all", "Toggle All Panels", None, "Window>Panels"),
    ("panel.toolbar", "Show Toolbar", None, "View"),
    ("panel.autoShow", "Auto Hide & Show", None, ""),
    ("panel.autoHide", "Auto Hide", None, ""),
    ("panel.solo", "Solo Mode", None, "Window>Panels"),
    ("panel.show", "Show Panel", None, ""),
    ("panel.order", "Panel Order", None, ""),
    ("panel.state", "Panel State", None, ""),
    ("view.loupe", "Loupe", None, "View"),
    ("view.screenMode", "Next Screen Mode", None, "Window>Screen Mode"),
    ("view.screenModeNormal", "Normal", None, "Window>Screen Mode"),
    ("view.lightsOut", "Next Lights Out Mode", None, "Window>Lights Out"),
    ("view.identityPlate", "Identity Plate", None, ""),
    ("dialog.identityPlateImage", "Choose Identity Plate Image…", None, ""),
    ("second.grid", "Secondary Grid", None, "Window>Secondary Display"),
    ("second.loupe", "Secondary Loupe", None, "Window>Secondary Display"),
    ("second.live", "Secondary Loupe – Live", None, "Window>Secondary Display"),
    ("second.locked", "Secondary Loupe – Locked", None, "Window>Secondary Display"),
    ("second.compare", "Secondary Compare", None, "Window>Secondary Display"),
    ("second.survey", "Secondary Survey", None, "Window>Secondary Display"),
    ("second.slideshow", "Secondary Slideshow", None, "Window>Secondary Display"),
    ("second.filter", "Secondary Window Filter", None, ""),
    ("second.filmstrip", "Secondary Filmstrip", None, "Window>Secondary Display"),
    ("photo.flagToggle", "Toggle Flagged Status", None, "Photo>Set Flag"),
    // the Book module (crate::book)
    ("book.new", "New Book", None, ""),
    ("book.get", "Book Document", None, ""),
    ("book.autoLayout", "Auto Layout", None, ""),
    ("book.clearLayout", "Clear Layout", None, ""),
    ("book.addPage", "Add Page", None, ""),
    ("book.removePage", "Remove Page", None, ""),
    ("book.movePage", "Move Page", None, ""),
    ("book.template", "Change Page Template", None, ""),
    ("book.place", "Place Photo", None, ""),
    ("book.swap", "Swap Photos", None, ""),
    ("book.text", "Set Cell Text", None, ""),
    ("book.pageText", "Page Text", None, ""),
    ("book.textPreset", "Text Style Preset", None, ""),
    ("book.guides", "Guides", None, ""),
    ("book.favorite", "Favorite Template", None, ""),
    ("book.templates", "Page Templates", None, ""),
    ("book.presets", "Auto Layout Presets", None, ""),
    ("book.settings", "Book Settings", None, ""),
    ("book.cell", "Cell Settings", None, ""),
    ("book.photoText", "Photo Text", None, ""),
    ("book.type", "Type", None, ""),
    ("book.background", "Background", None, ""),
    ("book.pageNumbers", "Page Numbers", None, ""),
    ("book.view", "Book View", None, ""),
    ("book.go", "Go to Book Page", None, ""),
    ("book.select", "Select Book Cell", None, ""),
    ("book.export", "Export Book…", None, ""),
    ("book.exportStatus", "Book Export Status", None, ""),
    ("book.save", "Save Book", None, ""),
    ("book.open", "Open Saved Book", None, ""),
    ("book.saved", "Saved Books", None, ""),
    ("book.deleteSaved", "Delete Saved Book", None, ""),
];

/// Is `id` a shell command, and is it enabled?
pub fn enabled(app: &DacApp, id: &str) -> Option<bool> {
    if !SHELL_COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    Some(match id {
        "module.develop" | "view.loupe" | "photo.flagToggle" => {
            app.session.active().is_some() || (id == "module.develop" && !app.session.catalog.is_empty())
        }
        "module.previous" => app.ui.previous_module.is_some(),
        _ => true,
    })
}

/// Run a shell command; `None`: not one.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if let Some(r) = crate::map::run(app, id, p) {
        return Some(r);
    }
    if !SHELL_COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    if crate::book::is_book_command(id) {
        return Some(crate::book::run(app, id, p));
    }
    Some(run_inner(app, id, p))
}

fn run_inner(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    if let Some(m) = id.strip_prefix("module.").and_then(ModuleId::parse) {
        return switch(app, m);
    }
    if let Some(e) = id.strip_prefix("panel.").and_then(Edge::parse) {
        let on = bool_param(p, "show").unwrap_or(!edge_shown(app, e));
        set_edge(app, e, on);
        return Ok(edges_json(app));
    }
    if let Some(mode) = id.strip_prefix("second.").and_then(SecondMode::parse) {
        return second(app, mode);
    }
    match id {
        "second.filter" => crate::panels::second::set_filter(app, p),
        "second.filmstrip" => {
            app.ui.second_filmstrip = bool_param(p, "show").unwrap_or(!app.ui.second_filmstrip);
            Ok(json!({"filmstrip": app.ui.second_filmstrip}))
        }
        "module.switch" => {
            let name = p.get("module").and_then(Value::as_str).ok_or("missing module (library|develop|map|book|slideshow|print|web)")?;
            let m = ModuleId::parse(name).ok_or_else(|| format!("unknown module: {name}"))?;
            switch(app, m)
        }
        "module.previous" => {
            let m = app.ui.previous_module.ok_or("no previous module")?;
            switch(app, m)
        }
        "module.setVisible" => {
            let name = p.get("module").and_then(Value::as_str).ok_or("missing module")?;
            let m = ModuleId::parse(name).ok_or_else(|| format!("unknown module: {name}"))?;
            let show = bool_param(p, "visible").unwrap_or(app.ui.hidden_modules.contains(&m));
            app.ui.hidden_modules.retain(|x| *x != m);
            if !show {
                if m == app.ui.module {
                    return Err("the module in use can't be hidden".into());
                }
                app.ui.hidden_modules.push(m);
            }
            Ok(json!({"module": m, "visible": show, "hidden": app.ui.hidden_modules}))
        }
        "panel.toggle" => {
            let name = p.get("edge").and_then(Value::as_str).ok_or("missing edge (top|bottom|left|right|toolbar)")?;
            if name == "toolbar" {
                app.ui.toolbar = bool_param(p, "show").unwrap_or(!app.ui.toolbar);
                return Ok(edges_json(app));
            }
            let e = Edge::parse(name).ok_or_else(|| format!("unknown edge: {name}"))?;
            let on = bool_param(p, "show").unwrap_or(!edge_shown(app, e));
            set_edge(app, e, on);
            Ok(edges_json(app))
        }
        "panel.sides" => {
            // Tab: both side panels; shows them when either is hidden
            let on = bool_param(p, "show").unwrap_or(!(app.ui.left_panel || app.ui.right_edge));
            set_edge(app, Edge::Left, on);
            set_edge(app, Edge::Right, on);
            Ok(edges_json(app))
        }
        "panel.all" => {
            let on = bool_param(p, "show").unwrap_or(!Edge::ALL.iter().all(|e| edge_shown(app, *e)));
            for e in Edge::ALL {
                set_edge(app, e, on);
            }
            Ok(edges_json(app))
        }
        "panel.toolbar" => {
            app.ui.toolbar = bool_param(p, "show").unwrap_or(!app.ui.toolbar);
            Ok(edges_json(app))
        }
        "panel.autoShow" => {
            let name = p.get("edge").and_then(Value::as_str).ok_or("missing edge")?;
            let e = Edge::parse(name).ok_or_else(|| format!("unknown edge: {name}"))?;
            let on = bool_param(p, "on").unwrap_or(!app.ui.auto_show.get(e));
            app.ui.auto_show.set(e, on);
            if on {
                app.ui.auto_hide.set(e, false);
            }
            Ok(edges_json(app))
        }
        "panel.autoHide" => {
            let name = p.get("edge").and_then(Value::as_str).ok_or("missing edge")?;
            let e = Edge::parse(name).ok_or_else(|| format!("unknown edge: {name}"))?;
            let on = bool_param(p, "on").unwrap_or(!app.ui.auto_hide.get(e));
            app.ui.auto_hide.set(e, on);
            if on {
                app.ui.auto_show.set(e, false);
            }
            Ok(edges_json(app))
        }
        "panel.solo" => {
            app.ui.single_panel = bool_param(p, "on").unwrap_or(!app.ui.single_panel);
            Ok(edges_json(app))
        }
        "panel.show" => {
            let name = p.get("panel").and_then(Value::as_str).ok_or("missing panel")?;
            let panel = PanelId::parse(name).ok_or_else(|| format!("unknown panel: {name}"))?;
            let show = bool_param(p, "visible").unwrap_or(app.ui.hidden_panels.contains(&panel));
            app.ui.hidden_panels.retain(|x| *x != panel);
            if !show {
                app.ui.hidden_panels.push(panel);
                if panel.right_panel() == Some(app.ui.right) {
                    app.ui.right = RightPanel::None;
                }
            }
            Ok(json!({"panel": panel, "visible": show, "hidden": app.ui.hidden_panels}))
        }
        "panel.order" => {
            let list = p.get("order").and_then(Value::as_array).ok_or("missing order (a list of panel ids)")?;
            let mut order = Vec::new();
            for v in list.iter().take(64) {
                let name = v.as_str().ok_or("order: panel ids are strings")?;
                let panel = PanelId::parse(name).ok_or_else(|| format!("unknown panel: {name}"))?;
                if !order.contains(&panel) {
                    order.push(panel);
                }
            }
            app.ui.panel_order = order;
            Ok(json!({"order": right_group(app).iter().map(|p| p.key()).collect::<Vec<_>>()}))
        }
        "panel.state" => Ok(json!({
            "edges": edges_json(app),
            "rightGroup": right_group(app).iter().map(|p| p.key()).collect::<Vec<_>>(),
            "hidden": app.ui.hidden_panels,
            "layouts": app.ui.layouts,
            "screenMode": app.ui.screen_mode,
            "lightsOut": app.ui.lights_out,
            "hiddenModules": app.ui.hidden_modules,
        })),
        "view.loupe" => {
            if app.ui.module != ModuleId::Library {
                switch(app, ModuleId::Library)?;
            }
            if app.ui.right.is_edit_tool() {
                app.ui.right = RightPanel::None;
            }
            app.ui.view = ViewMode::Detail;
            Ok(json!({"module": app.ui.module}))
        }
        "view.screenMode" => {
            let mode = match p.get("mode").and_then(Value::as_str) {
                Some(m) => {
                    ScreenMode::parse(m).ok_or_else(|| format!("unknown screen mode: {m} (normal|fullScreenMenu|fullScreen|fullScreenHidePanels)"))?
                }
                None => app.ui.screen_mode.next(),
            };
            Ok(set_screen_mode(app, mode))
        }
        "view.screenModeNormal" => Ok(set_screen_mode(app, ScreenMode::Normal)),
        "view.lightsOut" => {
            app.ui.lights_out = match p.get("mode").and_then(Value::as_str) {
                Some(m) => serde_json::from_value(json!(m)).map_err(|_| format!("unknown lights-out mode: {m} (off|dim|black)"))?,
                None => app.ui.lights_out.next(),
            };
            Ok(json!({"lightsOut": app.ui.lights_out}))
        }
        "dialog.identityPlateImage" => {
            let req = crate::pick::PickRequest::file(crate::i18n::tr("Identity Plate"), crate::i18n::tr("Images"), &["svg", "png", "jpg", "jpeg"]);
            match crate::pick::ask(app, "view.identityPlate", p, "image", req, |_| None) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(path) => app.run("view.identityPlate", json!({"image": path})),
                    None => Ok(Value::Null),
                },
                crate::pick::Picked::Later => Ok(Value::Null),
                crate::pick::Picked::Unavailable => Err("no file dialog on this platform: run view.identityPlate {image: path}".into()),
            }
        }
        "view.identityPlate" => {
            if let Some(t) = p.get("text").and_then(Value::as_str) {
                // a name, not a document: cap it
                app.ui.identity_plate.text = t.chars().take(80).collect();
            }
            if let Some(m) = bool_param(p, "mark") {
                app.ui.identity_plate.mark = m;
            }
            if let Some(v) = p.get("size") {
                let s = v.as_f64().filter(|s| s.is_finite()).ok_or("view.identityPlate: size must be a number")?;
                app.ui.identity_plate.size = (s as f32).clamp(9.0, 32.0);
            }
            match p.get("color") {
                None => {}
                Some(Value::Null) => app.ui.identity_plate.color = None,
                Some(Value::String(s)) if s.is_empty() => app.ui.identity_plate.color = None,
                Some(Value::String(s)) => {
                    app.ui.identity_plate.color = Some(parse_hex(s).ok_or_else(|| format!("view.identityPlate: color `{s}` is not #rrggbb"))?)
                }
                Some(_) => return Err("view.identityPlate: color is \"#rrggbb\" or null".into()),
            }
            if let Some(b) = bool_param(p, "bold") {
                app.ui.identity_plate.bold = b;
            }
            if let Some(path) = p.get("image").and_then(Value::as_str) {
                if path.is_empty() {
                    app.ui.identity_plate.image.clear();
                } else {
                    // checked now, so a bad file is an answer, not a silently empty plate
                    crate::plate::load(path)?;
                    app.ui.identity_plate.image = path.to_string();
                }
            }
            Ok(json!(app.ui.identity_plate))
        }
        "photo.flagToggle" => {
            let active = app.session.active().ok_or("no photo selected")?;
            let picked = app.session.catalog.photo(active).is_some_and(|ph| ph.flag == dac_catalog::Flag::Pick);
            app.run(if picked { "photo.unflag" } else { "photo.pick" }, json!({}))
        }
        _ => Err(format!("unknown shell command: {id}")),
    }
}

fn set_screen_mode(app: &mut DacApp, mode: ScreenMode) -> Value {
    let was = app.ui.screen_mode;
    app.ui.screen_mode = mode;
    if was.window_fullscreen() != mode.window_fullscreen() {
        app.ui.window_fullscreen = Some(mode.window_fullscreen());
    }
    json!({"screenMode": mode})
}

fn second(app: &mut DacApp, mode: SecondMode) -> Result<Value, String> {
    app.ui.second_mode = mode;
    app.ui.second_window = true;
    if mode == SecondMode::Locked {
        app.ui.second_locked = app.session.active().map(|p| p.0);
    }
    Ok(json!({"mode": mode, "locked": app.ui.second_locked}))
}

/// The right group's panels for the module on screen, in the user's order, without hidden ones.
pub fn right_group(app: &DacApp) -> Vec<PanelId> {
    let declared = get(app.ui.module).right_panels();
    let mut out: Vec<PanelId> = app.ui.panel_order.iter().copied().filter(|p| declared.contains(p)).collect();
    for p in declared {
        if !out.contains(p) {
            out.push(*p);
        }
    }
    out.retain(|p| !app.ui.hidden_panels.contains(p));
    out
}

/// Whether an edge is drawn this frame (shown, or peeking while auto-shown), given the module and
/// the screen mode.
pub fn edge_visible(app: &DacApp, e: Edge) -> bool {
    if app.ui.screen_mode == ScreenMode::FullScreenHidePanels {
        return app.ui.peek.get(e);
    }
    let m = get(app.ui.module);
    let has = match e {
        Edge::Left => !m.left_panels().is_empty(),
        Edge::Right => !m.right_panels().is_empty(),
        Edge::Top | Edge::Bottom => true,
    };
    has && (edge_shown(app, e) || app.ui.peek.get(e))
}

/// Auto show: a hidden edge marked auto-show appears while the pointer is at the window's edge and
/// hides again once it leaves the panel.
pub fn auto_show(app: &mut DacApp, ctx: &egui::Context) {
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let clicked = ctx.input(|i| i.pointer.primary_pressed());
    let r = ctx.content_rect();
    const AT: f32 = 4.0;
    for e in Edge::ALL {
        if edge_shown(app, e) && app.ui.screen_mode != ScreenMode::FullScreenHidePanels {
            app.ui.peek.set(e, false);
            continue;
        }
        let hover = app.ui.auto_show.get(e) || app.ui.screen_mode == ScreenMode::FullScreenHidePanels;
        let auto = hover || app.ui.auto_hide.get(e);
        if !auto {
            app.ui.peek.set(e, false);
            continue;
        }
        let (at_edge, inside) = match e {
            Edge::Left => (pos.x <= r.left() + AT, pos.x <= r.left() + app.ui.left_width + 24.0),
            Edge::Right => (pos.x >= r.right() - AT, pos.x >= r.right() - app.ui.right_width - 72.0),
            Edge::Top => (pos.y <= r.top() + AT, pos.y <= r.top() + 110.0),
            Edge::Bottom => (pos.y >= r.bottom() - AT, pos.y >= r.bottom() - 160.0),
        };
        let peek = app.ui.peek.get(e);
        // auto hide & show: resting at the edge; auto hide: a click there
        if at_edge && !peek && (hover || clicked) {
            app.ui.peek.set(e, true);
        } else if peek && !inside {
            app.ui.peek.set(e, false);
        }
    }
}

/// The identity plate's context menu: a graphic from a file, back to text, the brand mark, bold.
fn plate_menu(app: &mut DacApp, ui: &mut egui::Ui, plate: Rect) {
    let resp = ui.interact(plate, egui::Id::new("identity-plate"), Sense::click());
    resp.context_menu(|ui| {
        if ui.button(crate::i18n::tr("Choose Plate Image…")).clicked() {
            let _ = app.run("dialog.identityPlateImage", json!({}));
            ui.close();
        }
        if !app.ui.identity_plate.image.is_empty() && ui.button(crate::i18n::tr("Use Text Plate")).clicked() {
            let _ = app.run("view.identityPlate", json!({"image": ""}));
            ui.close();
        }
        let mut mark = app.ui.identity_plate.mark;
        if ui.checkbox(&mut mark, crate::i18n::tr("Show Brand Mark")).changed() {
            let _ = app.run("view.identityPlate", json!({"mark": mark}));
        }
        let mut bold = app.ui.identity_plate.bold;
        if ui.checkbox(&mut bold, crate::i18n::tr("Bold")).changed() {
            let _ = app.run("view.identityPlate", json!({"bold": bold}));
        }
    });
}

/// The module bar (top edge): identity plate, activity, module picker.
pub fn module_bar(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("module_bar")
        .exact_size(44.0)
        .frame(
            egui::Frame::NONE
                .fill(t.chrome)
                .inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 0 })
                .stroke(egui::Stroke::new(1.0, t.divider)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            register(ui.ctx(), "region:moduleBar", full);
            // identity plate
            let ip = app.ui.identity_plate.clone();
            let image = (!ip.image.is_empty()).then(|| crate::plate::texture(ui.ctx(), &ip.image)).flatten();
            let plate = if let Some(tex) = image {
                // a graphical plate: the file, 32 px tall, instead of the mark and text
                let [w, h] = tex.size();
                let height = 32.0;
                let width = (w as f32 * height / h.max(1) as f32).min(full.width() * 0.4);
                let r = Rect::from_min_size(pos2(full.left(), full.center().y - height / 2.0), vec2(width, height));
                ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
                r
            } else {
                let mut x = full.left();
                if ip.mark {
                    let r = Rect::from_center_size(pos2(x + 13.0, full.center().y), vec2(24.0, 24.0));
                    paint_mark(ui.painter(), r);
                    x += 32.0;
                }
                let text = if ip.text.trim().is_empty() { dac_brand::DISPLAY_NAME.to_string() } else { ip.text.clone() };
                let color = ip.color.map_or(t.text, |[r, g, b]| egui::Color32::from_rgb(r, g, b));
                let size = if ip.size.is_finite() { ip.size.clamp(9.0, 32.0) } else { 17.0 };
                let font = if ip.bold { t.semibold(size) } else { t.font(size) };
                let g = ui.painter().layout_no_wrap(text, font, color);
                let plate = Rect::from_min_size(pos2(full.left(), full.center().y - 12.0), vec2(x - full.left() + g.size().x, 24.0));
                ui.painter().galley(pos2(x, full.center().y - g.size().y / 2.0), g, color);
                plate
            };
            register(ui.ctx(), "region:identityPlate", plate);
            plate_menu(app, ui, plate);
            // activity: the status line (imports, exports, builds report here)
            if !app.ui.status.is_empty() {
                let w = (full.width() * 0.3).max(120.0);
                let status: String = app.ui.status.chars().take(120).collect();
                let g = ui.painter().layout(status, t.font(12.0), t.text_dim, w);
                ui.painter().galley(pos2(plate.right() + 28.0, full.center().y - g.size().y / 2.0), g, t.text_dim);
            }
            // module picker, right-aligned
            let shown: Vec<ModuleId> = ModuleId::ALL.into_iter().filter(|m| !app.ui.hidden_modules.contains(m)).collect();
            let sizes: Vec<(ModuleId, f32)> = shown
                .iter()
                .map(|m| (*m, ui.painter().layout_no_wrap(crate::i18n::tr(m.label()).to_string(), t.font(15.0), t.text).size().x))
                .collect();
            let sep = 22.0;
            let total: f32 = sizes.iter().map(|(_, w)| w).sum::<f32>() + sep * sizes.len().saturating_sub(1) as f32;
            let mut x = (full.right() - total).max(plate.right() + 40.0);
            let mut switch_to = None;
            for (i, (m, w)) in sizes.iter().enumerate() {
                if i > 0 {
                    ui.painter().text(pos2(x - sep / 2.0, full.center().y), Align2::CENTER_CENTER, "|", t.font(13.0), t.text_dim.gamma_multiply(0.5));
                }
                let r = Rect::from_min_size(pos2(x, full.top() + 6.0), vec2(*w, full.height() - 12.0));
                let id = format!("module:{}", m.key());
                register(ui.ctx(), &id, r);
                let resp = ui.interact(r, egui::Id::new(&id), Sense::click());
                let on = *m == app.ui.module;
                let color = if on {
                    t.text
                } else if resp.hovered() {
                    t.text.gamma_multiply(0.85)
                } else {
                    t.text_dim
                };
                ui.painter().text(pos2(x, full.center().y), Align2::LEFT_CENTER, crate::i18n::tr(m.label()), t.font(15.0), color);
                if resp.clicked() {
                    switch_to = Some(*m);
                }
                resp.context_menu(|ui| picker_menu(app, ui));
                x += w + sep;
            }
            let bg = ui.interact(full, egui::Id::new("module-bar-bg"), Sense::click());
            bg.context_menu(|ui| picker_menu(app, ui));
            if let Some(m) = switch_to
                && let Err(e) = app.run(m.command(), json!({}))
            {
                app.ui.status = e;
            }
        });
}

/// The picker's context menu: tick a module to show it in the picker.
fn picker_menu(app: &mut DacApp, ui: &mut egui::Ui) {
    for m in ModuleId::ALL {
        let mut on = !app.ui.hidden_modules.contains(&m);
        if ui.add_enabled(m != app.ui.module, egui::Checkbox::new(&mut on, crate::i18n::tr(m.label()))).changed() {
            let _ = app.run("module.setVisible", json!({"module": m.key(), "visible": on}));
        }
    }
    ui.separator();
    if ui.button(crate::i18n::tr("Show All")).clicked() {
        app.ui.hidden_modules.clear();
        ui.close();
    }
}

/// The brand mark: a six-blade aperture (drawn in code, after `brand/logo.svg`).
fn paint_mark(p: &egui::Painter, r: Rect) {
    let c = r.center();
    let rad = r.width().min(r.height()) / 2.0;
    p.circle_filled(c, rad, Color32::from_rgb(0x12, 0x34, 0x47));
    let blades = [Color32::from_rgb(0x2b, 0x6f, 0x8f), Color32::from_rgb(0x34, 0x8a, 0xa8), Color32::from_rgb(0x3f, 0xa3, 0xbf)];
    for i in 0..6 {
        let a0 = std::f32::consts::TAU * i as f32 / 6.0 - std::f32::consts::FRAC_PI_2;
        let a1 = a0 + std::f32::consts::TAU / 6.0;
        let inner = c + vec2((a0 + 1.2).cos(), (a0 + 1.2).sin()) * rad * 0.35;
        let pts: Vec<egui::Pos2> = std::iter::once(inner)
            .chain((0..=6).map(|k| {
                let a = a0 + (a1 - a0) * k as f32 / 6.0;
                c + vec2(a.cos(), a.sin()) * rad * 0.92
            }))
            .collect();
        p.add(egui::Shape::convex_polygon(pts, blades.get(i % 3).copied().unwrap_or(Color32::GRAY), egui::Stroke::NONE));
    }
    p.circle_stroke(c, rad, egui::Stroke::new(1.0, Color32::from_rgb(0x1d, 0x2b, 0x34)));
}

/// Lights out: veil everything but the photo.
pub fn lights_out(app: &DacApp, ctx: &egui::Context) {
    let a = app.ui.lights_out.alpha();
    if a <= 0.0 {
        return;
    }
    let screen = ctx.content_rect();
    let keep = app.image_rect.filter(|_| matches!(app.ui.view, ViewMode::Detail | ViewMode::Reference)).or(app.canvas_rect).unwrap_or(Rect::NOTHING);
    let veil = Color32::from_black_alpha((a * 255.0).round() as u8);
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("lights-out")));
    // four bands around the photo
    let k = keep.intersect(screen);
    if !k.is_positive() {
        p.rect_filled(screen, 0.0, veil);
        return;
    }
    p.rect_filled(Rect::from_min_max(screen.min, pos2(screen.right(), k.top())), 0.0, veil);
    p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.bottom()), screen.max), 0.0, veil);
    p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.top()), pos2(k.left(), k.bottom())), 0.0, veil);
    p.rect_filled(Rect::from_min_max(pos2(k.right(), k.top()), pos2(screen.right(), k.bottom())), 0.0, veil);
}
