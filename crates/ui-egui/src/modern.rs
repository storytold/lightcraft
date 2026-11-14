//! The Modern layout (fork-owned; the "Modern – Simple" choice in Settings ▸ General ▸ Layout, and
//! Android's default; `view.modernLayout` elsewhere). Library and Develop only, photo first, every
//! tool one tap away, finger- and mouse-friendly.
//!
//! Develop: the photo fills the window. Floating over it: the ☰ menu with undo, redo and before
//! (hold) in the top left, the Library / Develop switch at the top centre, the histogram (two sizes:
//! a tap switches them, a drag moves it, long-press or right-click opens its menu), and the dock
//! ([`crate::modern_dock`]) along the bottom or down the side, whose groups open their card of pill
//! sliders or the tool's panel. A tap on the photo puts the open card away, or else hides and shows
//! the chrome; pressing and holding it shows the original. The ☰ button opens the whole menu bar
//! ([`crate::modern_menu`]). Library is the grid under a slim bar; a tap opens a photo.
//!
//! Own design (patterns of touch photo and drawing apps; no assets taken from any). The Classic
//! layout is untouched: `LccApp::ui` calls in here only while [`ModernUi::enabled`] is set.

use egui::{Align2, Color32, Id, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::modern_dock::{Group, glass};
use crate::module::{self, ModuleId};
use crate::state::{BeforeAfter, Dialog, ViewMode};
use crate::theme::Tokens;
use crate::widgets::register;

/// The Modern layout's state, saved with the UI state (`modern` in `ui.json` and `ui.set`, formerly `touch`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModernUi {
    /// The Modern layout is on (Android's default).
    pub enabled: bool,
    /// Develop's floating chrome is shown (a tap on the photo, with no card open, toggles it).
    pub chrome: bool,
    /// The dock group whose card is open, if any.
    pub group: Option<Group>,
    /// The dock runs down the side (a rail) instead of along the bottom.
    pub dock_side: bool,
    /// The filmstrip shows under the photo while the chrome does.
    pub filmstrip: bool,
    /// Library's filter bar shows over the grid.
    pub filters: bool,
    /// Library's Info card shows at the right of the grid ([`crate::modern_library`]).
    pub info: bool,
    /// Library's second bar shows under the top one: delete, flag, rating and Info.
    pub actions: bool,
    /// The histogram over the photo in Develop: off, small or large. (Saved under a new name: the
    /// touch layout's `histogram` was off by default, and Modern shows it.)
    #[serde(rename = "histogramSize")]
    pub histogram: HistogramPlace,
    /// Where the histogram sits: its top-left corner as a fraction of the room it can move in
    /// (0…1 each way), so it stays put across rotations and window sizes.
    pub histogram_at: [f32; 2],
    /// The menu bar is open on this menu ([`crate::modern_menu`]).
    #[serde(skip)]
    pub menu: Option<String>,
    /// The menu the bar last scrolled its open title into view for: it scrolls again only when the
    /// open menu changes, so a finger can scroll the bar away from the open menu's title.
    #[serde(skip)]
    pub(crate) menu_scrolled: Option<String>,
    /// The open group's card shows one of its sections in full (`modern_dock::Group::sections`).
    #[serde(skip)]
    pub more: Option<String>,
    /// The histogram's own menu is open, at this point.
    #[serde(skip)]
    histogram_menu: Option<Pos2>,
    /// The press on the histogram already opened its menu (so its release doesn't switch the size).
    #[serde(skip)]
    histogram_held: bool,
    /// The original is showing because the photo (or Before) is held.
    #[serde(skip)]
    holding: bool,
    /// The screen's edges hidden by the display cutout and the system bars (points), from the host
    /// (Android) or `modern.insets`; the whole UI keeps clear of them.
    #[serde(skip)]
    pub insets: Insets,
    /// Library: a long press started picking several photos; taps add or remove.
    #[serde(skip)]
    pub selecting: bool,
    /// Library: Import's menu is open.
    #[serde(skip)]
    import_menu: bool,
    /// A popup (menu, dropdown) was open last frame.
    #[serde(skip)]
    popup_open: bool,
    /// Back had nothing to close: the host hands it to the system (Android puts the app away).
    #[serde(skip)]
    system_back: bool,
    /// When Back was last taken (s, input time): the key and the system's back callback can both
    /// report one press.
    #[serde(skip)]
    last_back: Option<f64>,
    #[serde(skip)]
    scroller: crate::modern_input::Scroller,
    /// The last press came from a mouse (or pen without touch): it goes straight to the widgets, so
    /// a scroll bar, a text selection or a drag work as on Classic. Android's presses are a finger's.
    #[serde(skip)]
    mouse: bool,
    /// Tests: presses without touch events count as a mouse's (else, in tests, as a finger's).
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) mouse_in_tests: bool,
    /// A long press on photos this frame, where.
    #[serde(skip)]
    long_press: Option<Pos2>,
}

/// How Develop's histogram shows in the Modern layout (`modern.histogram {mode}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistogramPlace {
    Off,
    /// A small chip (the default); a tap makes it large.
    #[default]
    #[serde(alias = "pinned", alias = "floating")]
    Small,
    /// Larger, with the photo's camera settings under it.
    Large,
}

impl HistogramPlace {
    fn parse(s: &str) -> Option<HistogramPlace> {
        Some(match s.to_ascii_lowercase().as_str() {
            "off" => HistogramPlace::Off,
            "small" | "pinned" | "floating" => HistogramPlace::Small,
            "large" => HistogramPlace::Large,
            _ => return None,
        })
    }
}

/// Screen edges to keep clear of (points).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    /// The most an edge may take (points): a bad report can't hide the whole UI.
    const MAX: f32 = 400.0;

    /// From the host's pixels (left, top, right, bottom) at `pixels_per_point`.
    pub fn from_px(px: [i32; 4], pixels_per_point: f32) -> Insets {
        let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
        let pt = |v: i32| (v.max(0) as f32 / ppp).min(Insets::MAX);
        Insets { left: pt(px[0]), top: pt(px[1]), right: pt(px[2]), bottom: pt(px[3]) }
    }

    fn clean(v: Option<f64>) -> f32 {
        v.filter(|v| v.is_finite()).map_or(0.0, |v| (v as f32).clamp(0.0, Insets::MAX))
    }
}

// (derivable only where `cfg!(target_os = "android")` is false)

// (derivable only where `cfg!(target_os = "android")` is false)
#[allow(clippy::derivable_impls)]
impl Default for ModernUi {
    fn default() -> Self {
        ModernUi {
            enabled: cfg!(target_os = "android"),
            chrome: true,
            group: None,
            dock_side: false,
            filmstrip: false,
            filters: false,
            info: false,
            actions: false,
            histogram: HistogramPlace::Small,
            histogram_at: [1.0, 0.0],
            menu: None,
            menu_scrolled: None,
            more: None,
            histogram_menu: None,
            histogram_held: false,
            holding: false,
            insets: Insets::default(),
            selecting: false,
            import_menu: false,
            popup_open: false,
            system_back: false,
            last_back: None,
            scroller: Default::default(),
            mouse: false,
            #[cfg(test)]
            mouse_in_tests: false,
            long_press: None,
        }
    }
}

/// The shape of the screen, which decides where panels go and how big the chrome is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    TabletLandscape,
    TabletPortrait,
    PhoneLandscape,
    PhonePortrait,
}

impl Form {
    /// From the window's size in points: a short side under 600 points is a phone.
    pub fn of(size: Vec2) -> Form {
        let phone = size.x.min(size.y) < 600.0;
        match (phone, size.x >= size.y) {
            (false, true) => Form::TabletLandscape,
            (false, false) => Form::TabletPortrait,
            (true, true) => Form::PhoneLandscape,
            (true, false) => Form::PhonePortrait,
        }
    }
    /// Panels open beside the photo (landscape) rather than under it.
    pub fn side_panel(self) -> bool {
        matches!(self, Form::TabletLandscape | Form::PhoneLandscape)
    }
    fn phone(self) -> bool {
        matches!(self, Form::PhoneLandscape | Form::PhonePortrait)
    }
}

/// Bars and buttons: finger-sized.
const BAR_H: f32 = 52.0;
const BUTTON: f32 = 44.0;
/// How long a press waits before it shows the original, or opens the histogram's menu (s).
const HOLD_SECS: f64 = 0.45;
/// How far a finger may move and still be a tap or a hold (points).
const SLOP: f32 = 10.0;
/// The floating chrome's distance from the window's edge.
const MARGIN: f32 = 12.0;
/// A histogram let go this close to a corner snaps into it (points).
const SNAP: f32 = 48.0;

/// How far below the window's top the Modern layout's floating top row ends (with a gap), for
/// notices drawn over the photo.
pub(crate) fn chrome_top(app: &LccApp) -> f32 {
    app.ui.modern.insets.top + MARGIN + BUTTON + 16.0
}

/// Shows the histogram (small) or hides it (Window ▸ Panels ▸ Histogram, Shortcuts ▸ Show Histogram).
pub(crate) fn toggle_histogram(app: &mut LccApp) {
    app.ui.modern.histogram = if histogram_shown(app) { HistogramPlace::Off } else { HistogramPlace::Small };
}

pub(crate) fn histogram_shown(app: &LccApp) -> bool {
    app.ui.modern.histogram != HistogramPlace::Off
}

/// The Modern layout is on.
pub(crate) fn on(app: &LccApp) -> bool {
    app.ui.modern.enabled
}

// ------------------------------------------------------------------------ input: Back, insets, scrolling

/// Before the frame's input reaches egui: the screen's insets become egui's safe area (so every
/// panel, dialog and the touch chrome keep clear of the camera cutout and the system bars), and a
/// Back press (the Android key or gesture, as egui's `BrowserBack`) runs [`back`].
pub(crate) fn before_input(app: &mut LccApp, raw: &mut egui::RawInput) {
    let i = app.ui.modern.insets;
    raw.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 { left: i.left, right: i.right, top: i.top, bottom: i.bottom }));
}

/// Back presses among the frame's input (the host's and the control channel's): [`back`].
fn back_keys(app: &mut LccApp, raw: &mut egui::RawInput) {
    let mut backs = 0;
    raw.events.retain(|e| match e {
        egui::Event::Key { key: egui::Key::BrowserBack, pressed, .. } => {
            backs += usize::from(*pressed);
            false
        }
        _ => true,
    });
    if backs == 0 {
        return;
    }
    // one press can arrive twice (the key, and the system's back callback): a second within
    // this long is the same press
    let now = raw.time.unwrap_or(0.0);
    if app.ui.modern.last_back.is_some_and(|t| (now - t).abs() < 0.25) {
        return;
    }
    app.ui.modern.last_back = Some(now);
    if back(app).is_none() {
        app.ui.modern.system_back = true;
    }
}

/// After the synthetic input is in: in the Modern layout, swipes scroll (see [`crate::modern_input`]).
pub(crate) fn after_input(app: &mut LccApp, raw: &mut egui::RawInput) {
    back_keys(app, raw);
    if !app.ui.modern.enabled {
        app.ui.modern.scroller = Default::default();
        return;
    }
    // the bar scrolls its open menu's title into view only when the open menu changes: forget
    // the last one when the bar closes, so opening the same menu again still scrolls to it
    if app.ui.modern.menu.is_none() {
        app.ui.modern.menu_scrolled = None;
    }
    // Esc closes the Modern layout's own floating things first (before any shortcut sees it)
    let esc = |e: &egui::Event| matches!(e, egui::Event::Key { key: egui::Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none());
    if raw.events.iter().any(esc) && escape(app) {
        raw.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Escape, .. }));
    }
    let widgets = std::mem::take(&mut app.widgets);
    let rect_of = |id: &str| widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r);
    let modal = app.ui.dialog.is_some() || app.ui.modern.popup_open || app.ui.modern.menu.is_some();
    let develop = app.ui.module != ModuleId::Library;
    let canvas = if develop { rect_of("modern:canvas").or(app.canvas_rect) } else { None };
    let photos = [rect_of("grid:scroll"), rect_of("filmstrip")];
    let strips = [rect_of("modern:strip"), rect_of("modern:menubar")];
    // the strips scroll whichever way a finger swipes, even while the menu bar is open — the
    // modal rule below would otherwise hand a sideways swipe to the title button under it, and
    // the bar (and the dock) could not be scrolled at all until the menu was closed
    let strip_over_modal = app.ui.modern.menu.is_some() && app.ui.dialog.is_none();
    let region = |p: Pos2| {
        use crate::modern_input::Region;
        if strip_over_modal && strips.iter().flatten().any(|r| r.contains(p)) {
            return Some(Region::Strip);
        }
        if modal {
            return Some(Region::Free);
        }
        // the photo in Develop has gestures of its own (tap, hold, crop, brush), and so do the
        // floating cards and the histogram over it
        if canvas.is_some_and(|c| c.contains(p)) && !strips.iter().flatten().any(|r| r.contains(p)) && !photos[1].is_some_and(|r| r.contains(p)) {
            return None;
        }
        if strips.iter().flatten().any(|r| r.contains(p)) {
            Some(Region::Strip)
        } else if photos.iter().flatten().any(|r| r.contains(p)) {
            Some(Region::Photos)
        } else {
            Some(Region::Free)
        }
    };
    // a finger's press or a mouse's? (a touch screen reports touch events with its presses)
    let pressed = raw.events.iter().any(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. }));
    if pressed {
        let touch = raw.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }));
        #[cfg(test)]
        let desktop_mouse = app.ui.modern.mouse_in_tests;
        #[cfg(not(test))]
        let desktop_mouse = true;
        app.ui.modern.mouse = !touch && !cfg!(target_os = "android") && desktop_mouse;
    }
    let mouse = app.ui.modern.mouse;
    let region = |p: Pos2| if mouse { None } else { region(p) };
    let toggle = app.ui.modern.selecting;
    let outcome = app.ui.modern.scroller.process(raw, region, toggle);
    app.widgets = widgets;
    if outcome.long_press.is_some() {
        app.ui.modern.long_press = outcome.long_press;
    }
    if outcome.busy
        && let Some(ctx) = app.tasks.repaint.as_ref()
    {
        ctx.request_repaint();
    }
}

/// Back (the Android gesture, the ‹ button, `modern.back`; Esc does the same for what it closes):
/// closes the innermost thing that is open and says what (`None`: nothing was, so the system's
/// Back applies: Android puts the app away). In order: a dialog, menu or popup; the menu bar; the
/// histogram's menu; picking several photos; the open dock card; Develop (back to the Library);
/// the Library's filter bar.
pub(crate) fn back(app: &mut LccApp) -> Option<&'static str> {
    let esc = |app: &mut LccApp| {
        for pressed in [true, false] {
            app.synthetic.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    };
    // the first-start camera roll question: Back is Not Now
    if crate::camera_roll::prompt_due(app) {
        app.ui.camera_roll_asked = true;
        return Some("prompt");
    }
    if on(app)
        && let Some(what) = crate::modern_sheets::back(app)
    {
        return Some(what);
    }
    if app.ui.dialog.is_some() {
        esc(app);
        return Some("dialog");
    }
    if app.ui.modern.import_menu {
        app.ui.modern.import_menu = false;
        return Some("menu");
    }
    if app.ui.modern.popup_open {
        esc(app);
        app.ui.modern.popup_open = false;
        return Some("menu");
    }
    if !app.ui.modern.enabled {
        return None;
    }
    if app.ui.modern.menu.take().is_some() {
        return Some("menuBar");
    }
    if app.ui.modern.histogram_menu.take().is_some() {
        return Some("menu");
    }
    if app.ui.modern.selecting && app.ui.module == ModuleId::Library {
        app.ui.modern.selecting = false;
        let keep: Vec<u64> = app.session.active().map(|id| id.0).into_iter().collect();
        let _ = app.run("library.select", json!({"ids": keep}));
        return Some("selection");
    }
    if app.ui.modern.more.take().is_some() {
        return Some("section");
    }
    if app.ui.modern.group.is_some() && app.ui.module != ModuleId::Library {
        crate::modern_dock::close(app);
        return Some("card");
    }
    if app.ui.module != ModuleId::Library {
        back_to_library(app);
        return Some("develop");
    }
    if app.ui.modern.filters {
        app.ui.modern.filters = false;
        // the filter bar's Metadata column searches go with it
        app.ui.filter_column_search.clear();
        return Some("filters");
    }
    None
}

/// Esc in the Modern layout: what Back closes of its own (the menu bar, the histogram's menu, the
/// open card), before Esc's usual meaning. Returns whether it closed one.
pub(crate) fn escape(app: &mut LccApp) -> bool {
    if !on(app) || app.ui.dialog.is_some() {
        return false;
    }
    if app.ui.modern.menu.take().is_some() || app.ui.modern.histogram_menu.take().is_some() {
        return true;
    }
    if app.ui.modern.more.take().is_some() {
        return true;
    }
    if app.ui.modern.group.is_some() && app.ui.module != ModuleId::Library {
        crate::modern_dock::close(app);
        return true;
    }
    false
}

/// Whether Back had nothing left to close since the last call: the host then does the system's
/// Back (Android: the app goes to the background).
pub fn take_system_back(app: &mut LccApp) -> bool {
    std::mem::take(&mut app.ui.modern.system_back)
}

/// The top-left corner for floating cards (the activity stack) in the Modern layout: inside the
/// screen's insets and below the top bar, so they never cover its buttons (Back, Import, …).
pub(crate) fn overlay_origin(app: &LccApp) -> Pos2 {
    let i = app.ui.modern.insets;
    let extra = if app.ui.modern.actions && app.ui.module == ModuleId::Library { BAR_H } else { 0.0 };
    pos2(i.left + 8.0, i.top + BAR_H + extra + 14.0)
}

/// The Modern layout's edges (replacing `shell::edges`): the screen's insets and Library's bar.
/// Returns the module, which draws the centre.
pub(crate) fn edges(app: &mut LccApp, ui: &mut egui::Ui, ctx: &egui::Context) -> &'static dyn module::Module {
    module::sync(app);
    let m = module::get(app.ui.module);
    let form = Form::of(ctx.content_rect().size());
    app.ui.modern.popup_open = egui::Popup::is_any_open(ctx);
    // a tap's release (and a hold's timing, a glide) comes on a later frame: ask for it here, as
    // the host draws only on input otherwise (Android)
    if app.ui.modern.scroller.busy() {
        ctx.request_repaint();
    }
    // the cutout and the system bars: every panel below keeps clear of them
    let i = app.ui.modern.insets;
    let fill = egui::Frame::NONE.fill(Tokens::get(ctx).chrome);
    if i.top > 0.5 {
        egui::Panel::top("modern-inset-top").exact_size(i.top).resizable(false).frame(fill).show(ui, |_| {});
    }
    if i.bottom > 0.5 {
        egui::Panel::bottom("modern-inset-bottom").exact_size(i.bottom).resizable(false).frame(fill).show(ui, |_| {});
    }
    if i.left > 0.5 {
        egui::Panel::left("modern-inset-left").exact_size(i.left).resizable(false).frame(fill).show(ui, |_| {});
    }
    if i.right > 0.5 {
        egui::Panel::right("modern-inset-right").exact_size(i.right).resizable(false).frame(fill).show(ui, |_| {});
    }
    if app.ui.modern.insets != Insets::default() {
        // behind the cutout and the system bars: the chrome's colour, not whatever was there
        let t = Tokens::get(ctx);
        ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.viewport_rect(), 0.0, t.chrome);
    }
    if app.ui.module == ModuleId::Library {
        library_bar(app, ui, form);
    }
    m
}

/// The centre: the module's view, then (Develop) the photo's gestures and the floating chrome.
pub(crate) fn center(app: &mut LccApp, ui: &mut egui::Ui, m: &'static dyn module::Module) {
    let area = ui.max_rect();
    if app.ui.module == ModuleId::Library {
        let before = app.session.active();
        // the grid alone: its filmstrip stays away, its filter bar shows when asked for
        let (film, filters) = (app.ui.filmstrip, app.ui.filter_bar);
        app.ui.filmstrip = false;
        app.ui.filter_bar = app.ui.modern.filters;
        m.center(ui, app);
        app.ui.filmstrip = film;
        if app.ui.filter_bar == app.ui.modern.filters {
            app.ui.filter_bar = filters;
        }
        long_press(app);
        if app.ui.modern.selecting && app.session.selection.ids.is_empty() {
            app.ui.modern.selecting = false;
        }
        if !app.ui.modern.selecting && app.ui.modern.menu.is_none() {
            open_tapped(app, ui, area, before);
        }
        crate::modern_library::show(app, ui, area);
        let bar_top = area.top() - BAR_H;
        crate::modern_menu::show(app, ui, area, area.left() + MARGIN + BUTTON + 8.0, bar_top + (BAR_H - 40.0) / 2.0);
        return;
    }
    // the filmstrip shows only with the chrome (and when asked for); the saved setting is kept
    let film = app.ui.filmstrip;
    app.ui.filmstrip = app.ui.modern.filmstrip && app.ui.modern.chrome;
    // a tool's overlay (crop frame, brush) stays clear of its card and the dock
    let (l, r, b) = crate::modern_dock::tool_room(app, area);
    if l + r + b > 0.0 {
        let photo = Rect::from_min_max(pos2(area.left() + l, area.top() + MARGIN + BUTTON + 12.0), pos2(area.right() - r, area.bottom() - b));
        ui.scope_builder(egui::UiBuilder::new().max_rect(photo), |ui| m.center(ui, app));
    } else {
        m.center(ui, app);
    }
    app.ui.filmstrip = film;
    let canvas = app.canvas_rect.unwrap_or(area).intersect(area);
    // a full-window screen (Settings, Import, Export) covers everything: no chrome over it
    if matches!(app.ui.dialog, Some(Dialog::Settings { .. } | Dialog::Import { .. } | Dialog::Export { .. })) {
        return;
    }
    if app.session.active().is_none() {
        // nothing to edit: the empty view keeps its own buttons; only the way back shows
        develop_top(app, ui, area);
        return;
    }
    gestures(app, ui, canvas);
    if !app.ui.modern.chrome {
        hint(ui, area);
        return;
    }
    let film = if app.ui.modern.filmstrip { app.ui.film_height() } else { 0.0 };
    let room = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - film));
    histogram(app, ui, room);
    let dock = crate::modern_dock::dock(app, ui, room);
    crate::modern_dock::card(app, ui, room, dock);
    develop_top(app, ui, area);
    crate::modern_menu::show(app, ui, area, area.left() + MARGIN + BUTTON + 8.0, area.top() + MARGIN + 2.0);
    focus_fade(app, ui.ctx());
}

/// Modern's floating layers that fade while an adjustment is dragged (the histogram stays).
const FADE_LAYERS: [(egui::Order, &str); 8] = [
    (egui::Order::Middle, "modern-dock"),
    (egui::Order::Middle, "modern-card"),
    (egui::Order::Middle, "modern-top"),
    (egui::Order::Middle, "modern-top-right"),
    (egui::Order::Middle, "modern-switch"),
    (egui::Order::Middle, "activity-stack"),
    (egui::Order::Foreground, "modern-menubar"),
    (egui::Order::Foreground, "modern-menu-drop"),
];
const CHROME_ALPHA: &str = "modern-chrome-alpha";

/// How opaque the floating chrome is (1: shown; 0: faded while an adjustment is dragged).
#[cfg(test)]
pub(crate) fn chrome_alpha(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(Id::new(CHROME_ALPHA))).unwrap_or(1.0)
}

/// How long the chrome takes to fade out and back (s).
const FADE_SECS: f32 = 0.2;

/// While a slider, a colour wheel or the tone curve is dragged, everything floating over the photo
/// fades away (over [`FADE_SECS`]) except that one control and the histogram, so the photo can be
/// seen whole; it fades back when the finger lifts. Done after drawing: each shape outside the
/// dragged control's rectangle has its colours faded. Own design.
fn focus_fade(app: &LccApp, ctx: &egui::Context) {
    let adjusting = app.ui.dragging_control.is_some() || app.session.interaction.is_some();
    let t = ctx.animate_bool_with_time(Id::new("modern-focus-fade"), adjusting, FADE_SECS);
    let alpha = (1.0 - t).clamp(0.0, 1.0);
    ctx.data_mut(|d| d.insert_temp(Id::new(CHROME_ALPHA), alpha));
    if t <= 0.0 {
        return;
    }
    // the control being dragged stays as it is (its place kept while the chrome fades back)
    let key = Id::new("modern-focus-rect");
    let now = ctx.dragged_id().and_then(|id| ctx.read_response(id)).map(|r| r.rect);
    if let Some(r) = now {
        ctx.data_mut(|d| d.insert_temp(key, r));
    }
    let keep = now.or_else(|| ctx.data(|d| d.get_temp::<Rect>(key))).map(|r| r.expand(2.0));
    ctx.graphics_mut(|g| {
        for (order, name) in FADE_LAYERS {
            let list = g.entry(egui::LayerId::new(order, Id::new(name)));
            for i in 0..list.next_idx().0 {
                list.mutate_shape(egui::layers::ShapeIdx(i), |c| {
                    let bounds = c.shape.visual_bounding_rect();
                    if keep.is_some_and(|k| bounds.is_finite() && k.contains_rect(bounds)) {
                        return;
                    }
                    egui::epaint::shape_transform::adjust_colors(&mut c.shape, move |col| {
                        if *col != Color32::PLACEHOLDER {
                            *col = col.gamma_multiply(alpha);
                        }
                    });
                });
            }
        }
    });
}

// ------------------------------------------------------------------------ Library

fn library_bar(app: &mut LccApp, ui: &mut egui::Ui, form: Form) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("modern-library-bar").exact_size(BAR_H).frame(egui::Frame::NONE.fill(t.chrome)).show(ui, |ui| {
        let r = ui.max_rect();
        let y = r.center().y - BUTTON / 2.0;
        if app.ui.modern.selecting {
            // picking several: how many, and the way out (Back does the same)
            let close = Rect::from_min_size(pos2(r.left() + 6.0, y), Vec2::splat(BUTTON));
            if round_button(ui, close, "modern:endSelect", Icon::Close, "Done Selecting", false, true).clicked() {
                back(app);
            }
            let n = app.session.selection.ids.len();
            ui.painter().text(
                pos2(close.right() + 8.0, r.center().y),
                Align2::LEFT_CENTER,
                format!("{n} {}", crate::i18n::tr("Selected")),
                t.semibold(17.0),
                t.text,
            );
        } else {
            let menu = Rect::from_min_size(pos2(r.left() + MARGIN - 4.0, y), Vec2::splat(BUTTON));
            if round_button(ui, menu, "modern:menu", Icon::Menu, "Menu", app.ui.modern.menu.is_some(), true).clicked() {
                crate::modern_menu::toggle(app);
            }
            if app.ui.modern.menu.is_none() {
                // on a narrow window beside ☰, clear of the buttons at the right
                let at = if r.width() < 560.0 { Rect::from_min_size(pos2(menu.right() + 4.0, r.top()), vec2(170.0, r.height())) } else { r };
                switch(app, ui, at);
            }
        }
        if app.ui.modern.menu.is_some() {
            return;
        }
        let mut x = r.right() - 8.0;
        let mut button = |icon: Icon, id: &str, label: &str, active: bool| -> bool {
            x -= BUTTON;
            let rect = Rect::from_min_size(pos2(x, y), Vec2::splat(BUTTON));
            round_button(ui, rect, id, icon, label, active, true).clicked()
        };
        let opened = button(Icon::Plus, "modern:import", "Import", app.ui.modern.import_menu);
        if opened {
            app.ui.modern.import_menu = !app.ui.modern.import_menu;
        }
        let square = app.ui.view == ViewMode::SquareGrid;
        if button(if square { Icon::GridPhoto } else { Icon::GridSquare }, "modern:gridStyle", "Grid Style", false) {
            let _ = app.run(if square { "view.photoGrid" } else { "view.squareGrid" }, json!({}));
        }
        if button(Icon::More, "modern:actionsButton", "Photo Actions", app.ui.modern.actions) {
            app.ui.modern.actions = !app.ui.modern.actions;
        }
        if button(Icon::Filter, "modern:filters", "Filter", app.ui.modern.filters) {
            app.ui.modern.filters = !app.ui.modern.filters;
            if !app.ui.modern.filters {
                app.ui.filter_column_search.clear();
            }
        }
        if app.ui.modern.import_menu && !opened {
            import_menu(app, ui, pos2(r.right() - 8.0, r.bottom() + 4.0));
        }
        let _ = form;
    });
    if app.ui.modern.actions && app.ui.modern.menu.is_none() {
        actions_bar(app, ui);
    }
}

/// Library's second bar: delete, flag and rating for the selected photos, and the Info card.
fn actions_bar(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (rating, flag) =
        app.session.active().and_then(|id| app.session.catalog.photo(id)).map_or((0, lcc_catalog::Flag::None), |p| (p.rating, p.flag));
    let has = app.session.active().is_some();
    egui::Panel::top("modern-actions-bar").exact_size(BAR_H).frame(egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider))).show(
        ui,
        |ui| {
            let r = ui.max_rect();
            let y = r.center().y - BUTTON / 2.0;
            let mut x = r.left() + MARGIN - 4.0;
            let mut next = |w: f32| {
                let rect = Rect::from_min_size(pos2(x, y), vec2(w, BUTTON));
                x += w;
                rect
            };
            let gap = BUTTON * 0.25;
            let mut run: Option<(&str, Value)> = None;
            if round_button(ui, next(BUTTON), "modern:delete", Icon::Trash, "Delete Photo", false, has).clicked() {
                run = Some(("photo.delete", json!({})));
            }
            let picked = flag == lcc_catalog::Flag::Pick;
            let icon = if picked { Icon::FlagPickFilled } else { Icon::FlagPick };
            if round_button(ui, next(BUTTON), "modern:flag", icon, "Flag as Pick", picked, has).clicked() {
                run = Some(("photo.flag", json!({"flag": if picked { "none" } else { "pick" }})));
            }
            let _ = next(gap);
            for n in 1..=5u8 {
                let on = rating >= n;
                if round_button(
                    ui,
                    next(BUTTON * 0.8),
                    &format!("modern:star{n}"),
                    if on { Icon::StarFilled } else { Icon::Star },
                    "Rating",
                    false,
                    has,
                )
                .clicked()
                {
                    run = Some(("photo.rate", json!({"rating": if rating == n { 0 } else { n }})));
                }
            }
            // Info at the right
            let info = Rect::from_min_size(pos2(r.right() - 8.0 - BUTTON, y), Vec2::splat(BUTTON));
            if round_button(ui, info, "modern:infoButton", Icon::Info, "Info", app.ui.modern.info, true).clicked() {
                app.ui.modern.info = !app.ui.modern.info;
            }
            if let Some((id, p)) = run
                && let Err(e) = app.run(id, p)
            {
                app.ui.status = e;
            }
        },
    );
}

/// The Library / Develop switch at the top centre of `bar`.
fn switch(app: &mut LccApp, ui: &mut egui::Ui, bar: Rect) {
    let t = Tokens::get(ui.ctx());
    let names = [("Library", ModuleId::Library), ("Develop", ModuleId::Develop)];
    let font = t.semibold(13.0);
    let widths: Vec<f32> =
        names.iter().map(|(n, _)| ui.painter().layout_no_wrap(crate::i18n::tr(n).to_string(), font.clone(), t.text).size().x + 28.0).collect();
    let h = 34.0;
    let w = widths.iter().sum::<f32>() + 6.0;
    let r = Rect::from_center_size(pos2(bar.center().x, bar.center().y), vec2(w, h));
    glass(ui.painter(), r, h / 2.0, &t);
    let mut x = r.left() + 3.0;
    let mut picked = None;
    for ((name, module), cw) in names.iter().zip(widths) {
        let cell = Rect::from_min_size(pos2(x, r.top() + 3.0), vec2(cw, h - 6.0));
        x += cw;
        let on = app.ui.module == *module || (*module == ModuleId::Develop && app.ui.module != ModuleId::Library);
        let id = format!("modern:mode:{}", name.to_ascii_lowercase());
        let resp = ui.interact(cell, ui.id().with(&id), Sense::click());
        let label = crate::i18n::tr(name);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
        register(ui.ctx(), id, cell);
        if on {
            ui.painter().rect_filled(cell, cell.height() / 2.0, Color32::from_white_alpha(40));
        }
        ui.painter().text(cell.center(), Align2::CENTER_CENTER, label, font.clone(), if on { t.text } else { t.text_label });
        if resp.clicked() && !on {
            picked = Some(*module);
        }
    }
    match picked {
        Some(ModuleId::Library) => back_to_library(app),
        Some(_) if app.session.active().is_some() => open_develop(app),
        _ => {}
    }
}

/// Import's choices: photos, a folder, and the camera roll where the host has one.
fn import_menu(app: &mut LccApp, ui: &egui::Ui, right_top: Pos2) {
    let t = Tokens::get(ui.ctx());
    let mut items: Vec<(&str, &str, &str)> =
        vec![("modern:import:photos", "Import Photos…", "file.addPhotos"), ("modern:import:folder", "Import Folder…", "file.addFolder")];
    if app.services.camera_roll.is_some() {
        items.push(("modern:import:cameraRoll", "Camera Roll", "file.addCameraRoll"));
    }
    if crate::import_tab::enabled(app) {
        items.push(("modern:import:tab", "Browse Tab", "view.importTab"));
    }
    let size = vec2(220.0, BUTTON * items.len() as f32 + 8.0);
    let min = pos2(right_top.x - size.x, right_top.y);
    let mut chosen = None;
    egui::Area::new(Id::new("modern-import-area")).fixed_pos(min).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
        let (bg, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(bg, 10.0, t.chrome);
        ui.painter().rect_stroke(bg, 10.0, Stroke::new(1.0, t.divider), egui::StrokeKind::Inside);
        for (i, (id, label, command)) in items.iter().enumerate() {
            let row = Rect::from_min_size(pos2(bg.left(), bg.top() + 4.0 + BUTTON * i as f32), vec2(size.x, BUTTON));
            let resp = ui.interact(row, Id::new(*id), Sense::click());
            register(ui.ctx(), *id, row);
            if resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(row.shrink(2.0), 6.0, t.pressed);
            }
            ui.painter().text(pos2(row.left() + 16.0, row.center().y), Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(14.0), t.text);
            if resp.clicked() {
                chosen = Some(*command);
            }
        }
    });
    let tapped_away = ui.input(|i| i.pointer.any_click())
        && chosen.is_none()
        && !ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| Rect::from_min_size(min, size).contains(p));
    if let Some(command) = chosen {
        app.ui.modern.import_menu = false;
        if let Err(e) = app.run(command, json!({})) {
            app.toast_error(ui.ctx(), e);
        }
    } else if tapped_away {
        app.ui.modern.import_menu = false;
    }
}

/// A long press on a grid photo starts picking several, with that photo; later ones add or remove.
fn long_press(app: &mut LccApp) {
    let Some(at) = app.ui.modern.long_press.take() else { return };
    let Some(id) =
        app.widgets.iter().rev().find(|(w, r)| w.starts_with("thumb:") && r.contains(at)).and_then(|(w, _)| w["thumb:".len()..].parse::<u64>().ok())
    else {
        return;
    };
    let mode = if app.ui.modern.selecting { "toggle" } else { "replace" };
    if app.run("library.select", json!({"ids": [id], "mode": mode})).is_ok() {
        app.ui.modern.selecting = true;
    }
}

/// A tap on a photo in the grid opens it in Develop (the grid's own click picks it first).
fn open_tapped(app: &mut LccApp, ui: &egui::Ui, area: Rect, before: Option<lcc_catalog::PhotoId>) {
    if !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) || ui.ctx().any_popup_open() {
        return;
    }
    // (a tap on a card floating over the grid, the search pill or the Info card, is the card's)
    let at = ui.input(|i| i.pointer.interact_pos());
    let on_grid = at.is_some_and(|p| area.contains(p) && ui.ctx().layer_id_at(p).is_none_or(|l| l == ui.layer_id()));
    let tapped = ui.input(|i| {
        i.pointer.primary_released()
            && on_grid
            && !i.pointer.is_decidedly_dragging()
            && i.pointer.button_clicked(egui::PointerButton::Primary)
            && !i.modifiers.any()
    });
    // a single photo picked by this tap (a tap on empty grid clears the selection instead);
    // `before` is kept for a future "tap the picked photo again" rule
    let _ = before;
    if tapped && app.session.active().is_some() && app.session.selection.ids.len() <= 1 {
        open_develop(app);
    }
}

fn open_develop(app: &mut LccApp) {
    let _ = app.run("module.develop", json!({}));
    if app.ui.view != ViewMode::Detail {
        app.ui.view = ViewMode::Detail;
    }
    app.ui.modern.chrome = true;
}

fn back_to_library(app: &mut LccApp) {
    crate::modern_dock::close(app);
    stop_holding(app);
    let _ = app.run("module.library", json!({}));
    if !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
        app.ui.view = app.ui.grid_style;
    }
}

// ------------------------------------------------------------------------ Develop: the photo

/// Tap: put the open card away, or else show or hide the chrome. Press and hold: the original while
/// held. A tool's own gestures (crop, brush, heal) come first: while one is open the photo is its.
fn gestures(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect) {
    if app.ui.modern.group.is_some_and(Group::is_tool) {
        return;
    }
    let resp = ui.interact(canvas, Id::new("modern-canvas"), Sense::click());
    register(ui.ctx(), "modern:canvas", canvas);
    let (held_for, moved) = ui.input(|i| {
        let held = i.pointer.press_start_time().map(|s| i.time - s).unwrap_or(0.0);
        let moved = match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => a.distance(b),
            _ => 0.0,
        };
        (held, moved)
    });
    let down = resp.is_pointer_button_down_on();
    if down && !app.ui.modern.holding && held_for >= HOLD_SECS && moved < SLOP {
        app.ui.modern.holding = true;
        if app.ui.before_after == BeforeAfter::Off {
            app.ui.before_after = BeforeAfter::Original;
        }
    }
    if down && !app.ui.modern.holding {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let was_holding = app.ui.modern.holding;
    // only a lifted finger ends the hold (#1035): the Before button's hold is down off the photo,
    // and ending it here whenever the photo itself wasn't pressed flipped the original back at once
    if !down && was_holding && !ui.input(|i| i.pointer.any_down()) {
        stop_holding(app);
    }
    if resp.clicked() && !was_holding {
        if app.ui.modern.group.is_some() {
            crate::modern_dock::close(app);
        } else {
            app.ui.modern.chrome = !app.ui.modern.chrome;
        }
    }
}

fn stop_holding(app: &mut LccApp) {
    app.ui.modern.holding = false;
    if app.ui.before_after == BeforeAfter::Original {
        app.ui.before_after = BeforeAfter::Off;
    }
}

// ------------------------------------------------------------------------ Develop: chrome

/// The histogram over the photo (`modern.histogram`): small or large. A tap switches the size, a drag
/// moves it (let go near a corner and it snaps there; its place is kept as a fraction of the room
/// it has), a long press or right-click opens its menu.
fn histogram(app: &mut LccApp, ui: &egui::Ui, area: Rect) {
    let place = app.ui.modern.histogram;
    if place == HistogramPlace::Off {
        app.ui.modern.histogram_menu = None;
        return;
    }
    let Some(id) = app.session.active() else { return };
    // the same type, scale and histogram as Classic's panel (View ▸ Histogram Mode): the waveforms'
    // small render of their own, a drag's live HDR histogram
    let scale = crate::panels::histogram::scale_of(app);
    let mode = crate::panels::histogram::mode_of(app);
    let hist = crate::panels::edit::shown_histogram(app, ui.ctx(), id, mode);
    let t = Tokens::get(ui.ctx());
    let phone = Form::of(ui.ctx().content_rect().size()).phone();
    let large = place == HistogramPlace::Large;
    let size = match (large, phone) {
        (false, true) => vec2(150.0, 66.0),
        (false, false) => vec2(196.0, 84.0),
        (true, true) => vec2(260.0, 140.0),
        (true, false) => vec2(360.0, 184.0),
    };
    // below the top row of buttons, inside the window
    let room =
        Rect::from_min_max(pos2(area.left() + MARGIN, area.top() + MARGIN + BUTTON + 8.0), pos2(area.right() - MARGIN, area.bottom() - MARGIN));
    let span = (room.size() - size).max(Vec2::ZERO);
    let fraction = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let at = app.ui.modern.histogram_at.map(fraction);
    let min = room.min + vec2(span.x * at[0], span.y * at[1]);
    let info = if large { camera_line(app, id) } else { String::new() };
    let ctx = ui.ctx().clone();
    let mut moved = None;
    let mut released_drag = false;
    let mut tapped = false;
    let mut menu_at = None;
    egui::Area::new(Id::new("modern-histogram")).fixed_pos(min).order(egui::Order::Middle).movable(false).show(&ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, crate::i18n::tr("Histogram")));
        register(ui.ctx(), "modern:histogram", r);
        glass(ui.painter(), r, 12.0, &t);
        let plot = if info.is_empty() { r.shrink(6.0) } else { Rect::from_min_max(r.min + vec2(6.0, 6.0), pos2(r.right() - 6.0, r.bottom() - 22.0)) };
        ui.painter().rect_filled(plot, 6.0, crate::panels::histogram::SCOPE_BG);
        if let Some(h) = &hist {
            crate::panels::histogram::paint_plot(ui.painter(), plot, h, mode, scale);
        }
        if !info.is_empty() {
            ui.painter().text(pos2(r.left() + 10.0, r.bottom() - 11.0), Align2::LEFT_CENTER, &info, t.font(11.0), t.text_label);
        }
        // a long press opens the menu (once per press); its release then switches nothing
        let held = ui.input(|i| i.pointer.press_start_time().map(|s| i.time - s).unwrap_or(0.0));
        let still = ui.input(|i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => a.distance(b) < SLOP,
            _ => true,
        });
        if resp.is_pointer_button_down_on() && !app.ui.modern.histogram_held {
            if held >= HOLD_SECS && still {
                app.ui.modern.histogram_held = true;
                menu_at = resp.interact_pointer_pos();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if resp.secondary_clicked() {
            menu_at = resp.interact_pointer_pos();
        }
        if resp.dragged() && !app.ui.modern.histogram_held {
            moved = Some(resp.drag_delta());
        }
        released_drag = resp.drag_stopped();
        tapped = resp.clicked() && !app.ui.modern.histogram_held;
        if !resp.is_pointer_button_down_on() && !resp.drag_stopped() && !resp.clicked() {
            app.ui.modern.histogram_held = false;
        }
        if resp.clicked() || resp.drag_stopped() {
            app.ui.modern.histogram_held = false;
        }
    });
    let f = |v: f32, s: f32| if s > 0.0 { fraction(v / s) } else { 0.0 };
    if let Some(d) = moved {
        let to = min + d - room.min;
        app.ui.modern.histogram_at = [f(to.x, span.x), f(to.y, span.y)];
    }
    if released_drag {
        // near a corner: into it
        let [x, y] = app.ui.modern.histogram_at;
        let snap = |v: f32, s: f32| {
            if v * s < SNAP {
                0.0
            } else if (1.0 - v) * s < SNAP {
                1.0
            } else {
                v
            }
        };
        app.ui.modern.histogram_at = [snap(x, span.x), snap(y, span.y)];
    }
    if tapped {
        app.ui.modern.histogram = if large { HistogramPlace::Small } else { HistogramPlace::Large };
    }
    if menu_at.is_some() {
        app.ui.modern.histogram_menu = menu_at;
    }
    histogram_menu(app, &ctx, area, large);
}

/// The histogram's menu: its type (RGB, luminance, … waveform, RGB parade), linear or log scale,
/// size, back to the corner, hide.
fn histogram_menu(app: &mut LccApp, ctx: &egui::Context, area: Rect, large: bool) {
    let Some(at) = app.ui.modern.histogram_menu else { return };
    let t = Tokens::get(ctx);
    let mode = crate::panels::histogram::mode_of(app);
    let scale = crate::panels::histogram::scale_of(app);
    // (label, key, ticked); "" is a divider
    let mut items: Vec<(String, String, bool)> =
        lcc_raster::HistogramMode::ALL.iter().map(|m| (crate::i18n::tr(m.label()).to_string(), format!("mode:{}", m.name()), *m == mode)).collect();
    items.push((String::new(), String::new(), false));
    for s in lcc_raster::HistogramScale::ALL {
        items.push((crate::i18n::tr(s.label()).to_string(), format!("scale:{}", s.name()), s == scale));
    }
    items.push((String::new(), String::new(), false));
    items.push((crate::i18n::tr(if large { "Small" } else { "Large" }).to_string(), "size".into(), false));
    items.push((crate::i18n::tr("Back to the Corner").to_string(), "corner".into(), false));
    items.push((crate::i18n::tr("Hide Histogram").to_string(), "hide".into(), false));
    const ROW: f32 = 34.0;
    const GAP: f32 = 9.0;
    let height = 8.0 + items.iter().map(|(l, _, _)| if l.is_empty() { GAP } else { ROW }).sum::<f32>();
    let size = vec2(230.0, height.min(area.height() - 16.0));
    let min = pos2(at.x.min(area.right() - size.x - 8.0).max(area.left() + 8.0), at.y.min(area.bottom() - size.y - 8.0).max(area.top() + 8.0));
    let mut chosen: Option<String> = None;
    egui::Area::new(Id::new("modern-histogram-menu")).fixed_pos(min).order(egui::Order::Foreground).show(ctx, |ui| {
        let (bg, _) = ui.allocate_exact_size(size, Sense::hover());
        register(ui.ctx(), "modern:histogramMenu", bg);
        glass(ui.painter(), bg, 12.0, &t);
        let mut y = bg.top() + 4.0;
        for (label, key, ticked) in &items {
            if label.is_empty() {
                ui.painter().hline(bg.x_range().shrink(10.0), y + GAP / 2.0, Stroke::new(1.0, Color32::from_white_alpha(30)));
                y += GAP;
                continue;
            }
            let row = Rect::from_min_size(pos2(bg.left() + 4.0, y), vec2(size.x - 8.0, ROW));
            y += ROW;
            let id = format!("modern:histogram:{key}");
            let resp = ui.interact(row, ui.id().with(&id), Sense::click());
            register(ui.ctx(), id, row);
            if resp.hovered() || resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(row, 8.0, Color32::from_white_alpha(22));
            }
            if *ticked {
                paint(ui.painter(), Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), Vec2::splat(14.0)), Icon::Check, t.accent);
            }
            ui.painter().text(pos2(row.left() + 28.0, row.center().y), Align2::LEFT_CENTER, label, t.font(14.0), t.text);
            if resp.clicked() {
                chosen = Some(key.clone());
            }
        }
    });
    let away = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !Rect::from_min_size(min, size).contains(p)));
    match chosen.as_deref() {
        Some("size") => app.ui.modern.histogram = if large { HistogramPlace::Small } else { HistogramPlace::Large },
        Some("corner") => app.ui.modern.histogram_at = [1.0, 0.0],
        Some("hide") => app.ui.modern.histogram = HistogramPlace::Off,
        Some(k) if k.starts_with("mode:") => app.ui.histogram_mode = k.trim_start_matches("mode:").to_string(),
        Some(k) if k.starts_with("scale:") => app.ui.histogram_scale = k.trim_start_matches("scale:").to_string(),
        _ => {}
    }
    if chosen.is_some() || away {
        app.ui.modern.histogram_menu = None;
    }
}

/// The photo's camera settings, as the large histogram shows them: ISO, focal length, aperture, shutter.
fn camera_line(app: &LccApp, id: lcc_catalog::PhotoId) -> String {
    let Some(p) = app.session.catalog.photo(id) else { return String::new() };
    let m = &p.meta;
    let mut parts = Vec::new();
    if let Some(iso) = m.iso {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(f) = m.focal_mm.filter(|f| f.is_finite() && *f > 0.0) {
        parts.push(format!("{f:.0} mm"));
    }
    if let Some(a) = m.aperture.filter(|a| a.is_finite() && *a > 0.0) {
        parts.push(format!("ƒ/{a:.1}"));
    }
    if !m.shutter.trim().is_empty() {
        parts.push(format!("{} s", m.shutter.trim().trim_end_matches(" s").trim_end_matches('s')));
    }
    parts.join(" · ")
}

/// Develop's top row: ☰ in the top left, the Library / Develop switch in the middle, undo, redo and
/// before (hold) beside ☰ (on a phone, undo and redo in the top right: before is a hold on the
/// photo). While the menu bar is open, only ☰ stays (the bar takes the rest of the row).
fn develop_top(app: &mut LccApp, ui: &mut egui::Ui, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let menu_open = app.ui.modern.menu.is_some();
    let narrow = area.width() < 640.0;
    let left_n = if menu_open || narrow { 1.0 } else { 4.0 };
    let top = area.top() + MARGIN;
    let pill = |n: f32| vec2(BUTTON * n + 4.0, BUTTON + 4.0);
    let mut extra: Vec<(&str, Icon, &str, bool)> = Vec::new();
    if !menu_open {
        extra.push(("modern:undo", Icon::RotateLeft, "Undo", !app.session.undo.is_empty()));
        extra.push(("modern:redo", Icon::RotateRight, "Redo", !app.session.redo.is_empty()));
    }
    let mut ran: Option<&str> = None;
    egui::Area::new(Id::new("modern-top")).fixed_pos(pos2(area.left() + MARGIN, top)).order(egui::Order::Middle).show(ui.ctx(), |ui| {
        let (r, _) = ui.allocate_exact_size(pill(left_n), Sense::hover());
        glass(ui.painter(), r, r.height() / 2.0, &t);
        let at = |i: f32| Rect::from_min_size(pos2(r.left() + 2.0 + BUTTON * i, r.top() + 2.0), Vec2::splat(BUTTON));
        if round_button(ui, at(0.0), "modern:menu", Icon::Menu, "Menu", menu_open, true).clicked() {
            crate::modern_menu::toggle(app);
        }
        if menu_open || narrow {
            return;
        }
        for (i, (id, icon, label, enabled)) in extra.iter().enumerate() {
            if round_button(ui, at(1.0 + i as f32), id, *icon, label, false, *enabled).clicked() {
                ran = Some(id);
            }
        }
        before_button(app, ui, at(3.0));
    });
    if narrow && !menu_open {
        let size = pill(extra.len() as f32);
        egui::Area::new(Id::new("modern-top-right")).fixed_pos(pos2(area.right() - MARGIN - size.x, top)).order(egui::Order::Middle).show(
            ui.ctx(),
            |ui| {
                let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                glass(ui.painter(), r, r.height() / 2.0, &t);
                for (i, (id, icon, label, enabled)) in extra.iter().enumerate() {
                    let b = Rect::from_min_size(pos2(r.left() + 2.0 + BUTTON * i as f32, r.top() + 2.0), Vec2::splat(BUTTON));
                    if round_button(ui, b, id, *icon, label, false, *enabled).clicked() {
                        ran = Some(id);
                    }
                }
            },
        );
    }
    match ran {
        Some("modern:undo") => {
            let _ = app.run("edit.undo", json!({}));
        }
        Some("modern:redo") => {
            let _ = app.run("edit.redo", json!({}));
        }
        _ => {}
    }
    if menu_open {
        return;
    }
    egui::Area::new(Id::new("modern-switch")).fixed_pos(pos2(area.center().x - 110.0, top + 2.0)).order(egui::Order::Middle).show(ui.ctx(), |ui| {
        let (r, _) = ui.allocate_exact_size(vec2(220.0, BUTTON), Sense::hover());
        switch(app, ui, r);
    });
}

/// Before: press and hold to see the original.
fn before_button(app: &mut LccApp, ui: &mut egui::Ui, rect: Rect) {
    let before = round_button(ui, rect, "modern:before", Icon::BeforeAfter, "Hold to See the Original", app.ui.modern.holding, true);
    if before.is_pointer_button_down_on() {
        if !app.ui.modern.holding {
            app.ui.modern.holding = true;
            if app.ui.before_after == BeforeAfter::Off {
                app.ui.before_after = BeforeAfter::Original;
            }
        }
    } else if app.ui.modern.holding && !ui.input(|i| i.pointer.any_down()) {
        stop_holding(app);
    }
}

/// The first look at Develop: a faint pill saying that a tap brings the tools.
fn hint(ui: &mut egui::Ui, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let r = Rect::from_center_size(pos2(area.center().x, area.bottom() - 22.0), vec2(36.0, 5.0));
    ui.painter().rect_filled(r, 2.5, t.text_dim.gamma_multiply(0.6));
}

// ------------------------------------------------------------------------ widgets

/// A round icon button (44 points), registered as `id` for the control channel.
pub(crate) fn round_button(ui: &mut egui::Ui, rect: Rect, id: &str, icon: Icon, label: &str, active: bool, enabled: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let name = crate::i18n::tr(label);
    // (the same button can be in two places in one frame, as the module switches: one egui id each)
    let resp = ui.interact(rect, ui.id().with(id), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, active, name));
    register(ui.ctx(), id, rect);
    let p = ui.painter();
    if active {
        p.circle_filled(rect.center(), rect.width() / 2.0 - 2.0, t.tool_active);
    } else if resp.is_pointer_button_down_on() && enabled {
        p.circle_filled(rect.center(), rect.width() / 2.0 - 2.0, t.pressed);
    }
    let c = if !enabled {
        t.text_disabled
    } else if active {
        t.pick
    } else {
        t.text
    };
    paint(p, Rect::from_center_size(rect.center(), Vec2::splat(22.0)), icon, c);
    resp
}

/// The Modern layout's commands (`menus_fork` lists them).
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("view.modernLayout", "Modern Layout", None, "Window>Panels"),
    ("view.compactLayout", "Compact Layout", None, "Window>Panels"),
    ("modern.chrome", "Show Modern Tools", None, ""),
    ("modern.group", "Open a Dock Group", None, ""),
    ("modern.dock", "Dock at the Side", None, ""),
    ("modern.menu", "Show the Menu Bar", None, ""),
    ("modern.back", "Back", None, ""),
    ("modern.insets", "Screen Edges to Keep Clear", None, ""),
    ("modern.histogram", "Modern Histogram", None, ""),
];

/// Runs a Modern layout command; `None`: not one. `modern.group {group}` opens a dock group's card
/// (`{group: null}` closes it), `modern.dock {side}` moves the dock, `modern.menu {menu?, on?}`
/// opens the menu bar on a menu.
pub(crate) fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let flag = |p: &Value, now: bool| p.get("on").and_then(Value::as_bool).unwrap_or(!now);
    Some(match id {
        "view.modernLayout" => {
            app.ui.modern.enabled = flag(p, app.ui.modern.enabled);
            if !app.ui.modern.enabled {
                crate::modern_dock::close(app);
                stop_holding(app);
                app.ui.modern.menu = None;
            }
            Ok(json!({"on": app.ui.modern.enabled}))
        }
        // {on?}: the Compact layout (Classic drawn denser); on turns the Modern layout off
        "view.compactLayout" => {
            let on = flag(p, app.compact());
            if on && app.ui.modern.enabled {
                app.ui.modern.enabled = false;
                crate::modern_dock::close(app);
                stop_holding(app);
                app.ui.modern.menu = None;
            }
            app.ui.settings.compact_layout = on;
            Ok(json!({"on": on}))
        }
        "modern.chrome" => {
            app.ui.modern.chrome = flag(p, app.ui.modern.chrome);
            Ok(json!({"on": app.ui.modern.chrome}))
        }
        "modern.group" => {
            let names = "presets, light, colour, tone, detail, optics, effects, crop, masks, heal, dodge or burn";
            match p.get("group") {
                None => return Some(Err(format!("modern.group: give `group` ({names}, or null to close)"))),
                Some(Value::Null) => {
                    crate::modern_dock::close(app);
                    Ok(json!({"group": null}))
                }
                Some(Value::String(name)) => {
                    let Some(g) = Group::parse(name) else {
                        return Some(Err(format!("modern.group: unknown group `{name}` ({names})")));
                    };
                    if app.ui.modern.group != Some(g)
                        && let Err(e) = crate::modern_dock::open(app, g)
                    {
                        return Some(Err(format!("modern.group: {e}")));
                    }
                    app.ui.modern.chrome = true;
                    Ok(json!({"group": app.ui.modern.group}))
                }
                Some(other) => return Some(Err(format!("modern.group: `group` must be a name ({names}) or null, not {other}"))),
            }
        }
        "modern.dock" => {
            app.ui.modern.dock_side = p.get("side").and_then(Value::as_bool).unwrap_or(!app.ui.modern.dock_side);
            Ok(json!({"side": app.ui.modern.dock_side}))
        }
        "modern.menu" => {
            // naming a menu opens it; else `on`, or a toggle
            let open = p.get("on").and_then(Value::as_bool).unwrap_or(p.get("menu").is_some_and(Value::is_string) || app.ui.modern.menu.is_none());
            if !open {
                app.ui.modern.menu = None;
            } else {
                let menu = p.get("menu").and_then(Value::as_str).unwrap_or(crate::modern_menu::SHORTCUTS);
                let titles = crate::modern_menu::menu_titles(app);
                let Some(title) = titles.iter().find(|t| t.eq_ignore_ascii_case(menu)) else {
                    return Some(Err(format!("modern.menu: unknown menu `{menu}` ({})", titles.join(", "))));
                };
                app.ui.modern.menu = Some(title.clone());
            }
            Ok(json!({"menu": app.ui.modern.menu}))
        }
        "modern.back" => {
            let closed = back(app);
            if closed.is_none() {
                app.ui.modern.system_back = true;
            }
            Ok(json!({"handled": closed.is_some(), "closed": closed}))
        }
        "modern.insets" => {
            let mut v = [0.0f32; 4];
            for (slot, key) in v.iter_mut().zip(["left", "top", "right", "bottom"]) {
                match p.get(key) {
                    None | Some(Value::Null) => {}
                    Some(Value::Number(n)) => *slot = Insets::clean(n.as_f64()),
                    Some(other) => return Some(Err(format!("modern.insets: `{key}` must be a number of points, not {other}"))),
                }
            }
            app.ui.modern.insets = Insets { left: v[0], top: v[1], right: v[2], bottom: v[3] };
            Ok(serde_json::to_value(app.ui.modern.insets).unwrap_or(Value::Null))
        }
        "modern.histogram" => {
            let place = match p.get("mode") {
                None | Some(Value::Null) => {
                    if app.ui.modern.histogram == HistogramPlace::Off {
                        HistogramPlace::Small
                    } else {
                        HistogramPlace::Off
                    }
                }
                Some(Value::String(m)) => match HistogramPlace::parse(m) {
                    Some(h) => h,
                    None => return Some(Err(format!("modern.histogram: unknown mode `{m}` (off, small or large)"))),
                },
                Some(other) => return Some(Err(format!("modern.histogram: `mode` must be off, small or large, not {other}"))),
            };
            if let Some(at) = p.get("at") {
                let xy: Option<Vec<f32>> = at
                    .as_array()
                    .filter(|a| a.len() == 2)
                    .map(|a| a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0) as f32).collect());
                match xy.as_deref() {
                    Some([x, y]) => app.ui.modern.histogram_at = [*x, *y],
                    _ => return Some(Err("modern.histogram: `at` must be [x, y], each 0…1".into())),
                }
            }
            app.ui.modern.histogram = place;
            Ok(json!({"mode": place, "at": app.ui.modern.histogram_at}))
        }
        _ => return None,
    })
}
