//! Settings (fork-owned). In the Classic and Compact layouts on a window at least [`ROOMY`] points
//! wide it is a window of normal size showing one category's settings at a time (OneSlips,
//! LR-PREF-WINDOW), its categories listed as Settings ▸ General ▸ Settings Categories says
//! ([`categories`]): down its left side (Classic's default) or in one row of tabs along its top
//! (Compact's default, [`crate::panels::settings::body`]). In the Modern layout and on a narrow
//! window it is a full-window sheet with an icon rail of sections down its left side and the open
//! section's rows beside it:
//!
//! - With room (a window at least [`ROOMY`] points wide: desktops, tablets) the rail always shows
//!   each section's name and a search field; Back (the ‹ button, Esc, Android's Back) closes
//!   Settings.
//! - On a narrow window the rail is icons only while a section is open. Back widens it to show the
//!   names (and the search field); Back again closes Settings. A tap on an icon switches section.
//!
//! The rail's sections are the Settings window's own tabs ([`crate::panels::settings::TABS`]:
//! General, Import, Folders, Performance, Display, Interface, Slideshow, Faces, Profiles, AI
//! Denoise, AI Models, Generative Fill, Shortcuts, Controls, Connections, Sync), one section per
//! tab, in the same order, so both presentations of Settings read the same (PR #830). Slideshow
//! only shows in the Classic layout, which has that module. Each section shows its tab's own rows
//! ([`crate::panels::settings::tab_body`]), so every setting keeps its control and its command.
//!
//! Own design.

use egui::{Align2, Color32, Id, Rect, Sense, Stroke, Vec2, pos2, vec2};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::state::{Dialog, SettingsCategories};
use crate::theme::Tokens;
use crate::widgets::register;

/// The screen's layer id (`Order::Middle`), which is the modal layer while Settings is open, so the
/// dialogs' barrier (`dialogs::prepare`) keeps the popups opened from its rows.
pub const LAYER: &str = "settings-screen";

/// From this width (points) the rail always shows the sections' names.
pub const ROOMY: f32 = 640.0;
const HEADER_H: f32 = 56.0;
const RAIL_WIDE: f32 = 220.0;
const RAIL_NARROW: f32 = 60.0;
const ITEM_H: f32 = 44.0;
/// The category list's width in the Settings window (Settings Categories: Left).
const LIST_W: f32 = 190.0;
/// A category's height in that list: a mouse's, not a finger's.
const LIST_ITEM_H: f32 = 28.0;
/// The Settings window's margin inside its frame.
const WINDOW_PAD: f32 = 16.0;
/// The rows' widest, so a large window doesn't stretch them.
const CONTENT_MAX: f32 = 820.0;

/// A section: its key, name, icon, the tab whose rows it shows, and words a search finds it by.
/// The keys and labels are [`crate::panels::settings::TABS`]'s, in its order.
struct Section {
    key: &'static str,
    label: &'static str,
    icon: Icon,
    tabs: &'static [&'static str],
    words: &'static str,
}

const SECTIONS: &[Section] = &[
    Section {
        key: "general",
        label: "General",
        icon: Icon::Gear,
        tabs: &["general"],
        words: "language catalog library location open new startup culling auto advance confirm external editing photocraft editor",
    },
    Section {
        key: "import",
        label: "Import",
        icon: Icon::Plus,
        tabs: &["import"],
        words: "raw defaults non-raw metadata preset xmp sidecars names cameras cards auto import watched folder copy album lens profiles database",
    },
    Section { key: "folders", label: "Folders", icon: Icon::Folder, tabs: &["folders"], words: "folders watch polling new photos forget unchanged" },
    Section {
        key: "performance",
        label: "Performance",
        icon: Icon::Activity,
        tabs: &["performance"],
        words: "performance rendering preview size memory cache thumbnail clear smart previews gpu",
    },
    Section { key: "display", label: "Display", icon: Icon::Monitor, tabs: &["display"], words: "monitor profile icc srgb screen" },
    Section {
        key: "interface",
        label: "Interface",
        icon: Icon::Palette,
        tabs: &["interface"],
        words: "layout classic modern appearance mode dark light font size sliders slider text pill filmstrip grid badges ratings flags wheel scroll info overlay loupe",
    },
    Section { key: "slideshow", label: "Slideshow", icon: Icon::Slideshow, tabs: &["slideshow"], words: "slide duration fade playback export" },
    Section { key: "faces", label: "Faces", icon: Icon::FaceBox, tabs: &["faces"], words: "faces people recognition" },
    Section { key: "profiles", label: "Profiles", icon: Icon::ProfileGrid, tabs: &["profiles"], words: "profiles hide camera body serial" },
    Section {
        key: "develop",
        label: "Develop",
        icon: Icon::Sliders,
        tabs: &["develop"],
        words: "develop tool profiles tools sliders groups custom hide panel witchcraft eboda",
    },
    Section { key: "denoise", label: "AI Denoise", icon: Icon::Sparkle, tabs: &["denoise"], words: "ai denoise" },
    Section {
        key: "ai",
        label: "AI Models",
        icon: Icon::Brush,
        tabs: &["ai"],
        words: "ai models download sam sam3 sam2 eomt birefnet mediapipe denoise faces recognition masks object describe subject sky landscape people folder location licence",
    },
    Section {
        key: "genfill",
        label: "Generative Fill",
        icon: Icon::Object,
        tabs: &["genfill"],
        words: "generative fill openai service api key consent",
    },
    Section { key: "shortcuts", label: "Shortcuts", icon: Icon::Help, tabs: &["shortcuts"], words: "keyboard keymap keys" },
    Section {
        key: "controls",
        label: "Controls",
        icon: Icon::Sliders,
        tabs: &["controls"],
        words: "control surfaces hardware console dial dials knob keys keypad dialpad actions ring logi logitech mx creative loupedeck tactilebridge plugin pairing",
    },
    Section { key: "connections", label: "Connections", icon: Icon::Share, tabs: &["connections"], words: "connections keychain" },
    Section { key: "sync", label: "Sync", icon: Icon::Cloud, tabs: &["sync"], words: "devices peers pair pairing catalog collections share network" },
];

/// How Settings is drawn ([`presentation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Over the whole window, the sections down its left side, finger-sized: the Modern layout,
    /// and every layout on a window narrower than [`ROOMY`].
    Sheet,
    /// A window of normal size with its categories in a list down its left side.
    WindowLeft,
    /// A window of normal size with its categories in one row of tabs along its top.
    WindowTop,
}

/// Where the Settings window lists its categories in the layout drawn now (Settings ▸ General ▸
/// Settings Categories): by default down the left in Classic, along the top in Compact. `None` in
/// the Modern layout, which always lists them down the left of its full-window sheet.
pub fn categories(app: &LccApp) -> Option<SettingsCategories> {
    if crate::modern::on(app) {
        return None;
    }
    let s = &app.ui.settings;
    Some(if app.compact() {
        s.settings_categories_compact.unwrap_or(SettingsCategories::Top)
    } else {
        s.settings_categories_classic.unwrap_or(SettingsCategories::Left)
    })
}

/// Keep `c` as where the Settings window lists its categories in the layout drawn now (Classic or
/// Compact each keep their own); nothing in the Modern layout, which has no choice.
pub fn set_categories(app: &mut LccApp, c: SettingsCategories) {
    if crate::modern::on(app) {
        return;
    }
    if app.compact() {
        app.ui.settings.settings_categories_compact = Some(c);
    } else {
        app.ui.settings.settings_categories_classic = Some(c);
    }
}

/// How Settings is drawn on a window `width` points wide in the layout drawn now.
pub fn presentation(app: &LccApp, width: f32) -> Presentation {
    match categories(app) {
        Some(_) if width < ROOMY => Presentation::Sheet,
        Some(SettingsCategories::Left) => Presentation::WindowLeft,
        Some(SettingsCategories::Top) => Presentation::WindowTop,
        None => Presentation::Sheet,
    }
}

/// Settings' own state between frames (kept in `LccApp::modern_sheets`).
#[derive(Default)]
pub struct SettingsScreen {
    /// Narrow window: the rail shows the sections' names (Back widened it).
    pub wide: bool,
    /// The search field's text.
    pub search: String,
    /// The tab asked for (`app.settings {tab}`) that was last brought into view, and for how many
    /// frames (a floating area's first frame is an invisible sizing pass that drops the scroll).
    scrolled_to: (String, u8),
}

fn shown(app: &LccApp, s: &Section) -> bool {
    // the browser has no sync and no control surfaces
    if cfg!(target_arch = "wasm32") && matches!(s.key, "sync" | "controls") {
        return false;
    }
    // Slideshow is a Classic module: its settings are Classic's
    s.key != "slideshow" || !crate::modern::on(app)
}

/// A section name of an older build (a saved `ui.json` may still say `library` or `editing`)
/// becomes the tab that took its rows over.
fn legacy(tab: &str) -> &str {
    match tab {
        "library" => "folders",
        "editing" => "denoise",
        other => other,
    }
}

/// The section a tab (or a section's own key) belongs to: a tab name (`app.settings
/// {tab: "performance"}`) opens its section. An older build's section name opens the tab that
/// took its rows over.
pub fn section_of(tab: &str) -> &'static str {
    let asked = legacy(tab);
    SECTIONS.iter().find(|s| s.key == asked || s.tabs.contains(&asked)).map_or("general", |s| s.key)
}

/// Whether `tab` names a section, one of the Settings window's tabs, or an older build's
/// section name.
pub fn known(tab: &str) -> bool {
    let asked = legacy(tab);
    SECTIONS.iter().any(|s| s.key == asked || s.tabs.contains(&asked))
}

/// The section after (`step` 1) or before (-1) `open` among those shown, wrapping around.
fn step_section(app: &LccApp, open: &str, step: isize) -> Option<&'static str> {
    let keys: Vec<&'static str> = SECTIONS.iter().filter(|s| shown(app, s)).map(|s| s.key).collect();
    let n = keys.len() as isize;
    if n == 0 {
        return None;
    }
    let at = keys.iter().position(|k| *k == open).unwrap_or(0) as isize;
    keys.get((at + step).rem_euclid(n) as usize).copied()
}

/// Consume a press of `key` with Ctrl held (and Shift as `shift` says, no Alt): the Control key
/// itself on every platform (on macOS ⌘Tab is the system's), whatever egui reads as Command.
fn ctrl_key(ctx: &egui::Context, key: egui::Key, shift: bool) -> bool {
    ctx.input_mut(|i| {
        let hit = |e: &egui::Event| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.ctrl && m.shift == shift && !m.alt);
        let found = i.events.iter().any(hit);
        if found {
            i.events.retain(|e| !hit(e));
        }
        found
    })
}

/// Back on Settings: on a narrow window from a section to the widened rail (`Some`), else `None`
/// (the caller closes Settings, as Esc does).
pub(crate) fn back(app: &mut LccApp, ctx: &egui::Context) -> Option<&'static str> {
    if !matches!(app.ui.dialog, Some(Dialog::Settings { .. })) {
        return None;
    }
    if ctx.content_rect().width() < ROOMY && !app.modern_sheets.settings.wide {
        app.modern_sheets.settings.wide = true;
        return Some("settingsList");
    }
    None
}

fn close(app: &mut LccApp) {
    app.ui.dialog = None;
    app.ui.settings_section = None;
    app.modern_sheets.settings = SettingsScreen::default();
}

/// Draws Settings over the whole window (every layout).
pub(crate) fn show(app: &mut LccApp, ctx: &egui::Context) {
    let Some(Dialog::Settings { tab }) = app.ui.dialog.clone() else { return };
    let screen = ctx.content_rect();
    // the Classic and Compact layouts with room: a window, its categories down its left side or
    // in one row of tabs along its top (OneSlips, LR-PREF-WINDOW)
    match presentation(app, screen.width()) {
        Presentation::WindowLeft => return window(app, ctx, &tab, true),
        Presentation::WindowTop => return window(app, ctx, &tab, false),
        Presentation::Sheet => {}
    }
    let open = section_of(&tab).to_string();
    let roomy = screen.width() >= ROOMY;
    if roomy {
        app.modern_sheets.settings.wide = false;
    }
    let wide = roomy || app.modern_sheets.settings.wide;
    let mut go_back = false;
    // Esc: Back (a popup or a text field being edited takes it first)
    if !egui::Popup::is_any_open(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        go_back = true;
    }
    let t = Tokens::get(ctx);
    let mut picked: Option<&'static str> = None;
    // Ctrl+Tab / Ctrl+Page Down: the next section; Ctrl+Shift+Tab / Ctrl+Page Up: the one before (LR-PREF-WINDOW)
    let back_key = ctrl_key(ctx, egui::Key::Tab, true) || ctrl_key(ctx, egui::Key::PageUp, false);
    let next_key = !back_key && (ctrl_key(ctx, egui::Key::Tab, false) || ctrl_key(ctx, egui::Key::PageDown, false));
    if back_key || next_key {
        picked = step_section(app, &open, if back_key { -1 } else { 1 });
    }
    // over the layout's floating chrome (also `Middle`), under popups (dropdowns, context menus:
    // `Foreground`), which open from its rows
    let layer = egui::LayerId::new(egui::Order::Middle, Id::new(LAYER));
    ctx.move_to_top(layer);
    ctx.memory_mut(|m| m.set_modal_layer(layer));
    let picked_by_key = picked;
    egui::Area::new(Id::new(LAYER)).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (bg, _) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(bg, 0.0, t.chrome);
        // the header: ‹ (Back), the title, what Back does
        let header = Rect::from_min_size(bg.min, vec2(bg.width(), HEADER_H));
        // macOS draws the traffic lights over the header's top-left corner: clear them like the
        // top bar does (`panels::topbar`, the same 78 the sidebar button starts at); in full
        // screen they are gone, and the room comes back (issue #793).
        let traffic = if app.integrated_titlebar && !app.window_is_fullscreen { 68.0 } else { 0.0 };
        let b = Rect::from_min_size(pos2(header.left() + 10.0 + traffic, header.center().y - 18.0), Vec2::splat(36.0));
        let resp = ui.interact(b, ui.id().with("settings-back"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("Back")));
        register(ui.ctx(), "settings:back", b);
        ui.painter().circle_filled(b.center(), 18.0, if resp.hovered() { t.pressed } else { t.button });
        paint(ui.painter(), Rect::from_center_size(b.center(), Vec2::splat(18.0)), Icon::ChevronLeft, t.text);
        if resp.clicked() {
            go_back = true;
        }
        let name = SECTIONS.iter().find(|s| s.key == open).map_or("General", |s| s.label);
        let title = if roomy {
            format!("{} · {}", crate::i18n::tr("Settings"), crate::i18n::tr(name))
        } else if wide {
            crate::i18n::tr("Settings").to_string()
        } else {
            crate::i18n::tr(name).to_string()
        };
        ui.painter().text(pos2(b.right() + 12.0, header.center().y), Align2::LEFT_CENTER, title, t.semibold(17.0), t.text);
        let hint = if wide { "Back closes Settings" } else { "Back shows all sections" };
        if header.width() > 520.0 {
            ui.painter().text(pos2(header.right() - 16.0, header.center().y), Align2::RIGHT_CENTER, crate::i18n::tr(hint), t.font(12.0), t.text_dim);
        }
        ui.painter().hline(bg.x_range(), header.bottom(), Stroke::new(1.0, t.divider));
        let body = Rect::from_min_max(pos2(bg.left(), header.bottom() + 1.0), bg.max);
        // narrow and widened: the rail is the whole page (the list of sections)
        // a section wider than the room beside the full rail (Slideshow's settings and preview)
        // takes the names' room: the rail shows icons
        let cramped = open == "slideshow" && body.width() - RAIL_WIDE < crate::panels::settings::SLIDESHOW_W + 80.0;
        let names = wide && !(roomy && cramped);
        let rail_w = if wide && !roomy {
            body.width()
        } else if names {
            RAIL_WIDE
        } else {
            RAIL_NARROW
        };
        let rail = Rect::from_min_size(body.min, vec2(rail_w, body.height()));
        picked = rail_ui(app, ui, rail, &open, names, false).or(picked_by_key);
        ui.painter().vline(rail.right(), body.y_range(), Stroke::new(1.0, t.divider));
        let content = Rect::from_min_max(pos2(rail.right() + 1.0, body.top()), body.max);
        if !(wide && !roomy) {
            rows(app, ui, content, &open, &tab);
        }
    });
    // (the tab stays as it was asked for, e.g. `denoise`, until another section is picked)
    let picked_any = picked.is_some();
    let picked = picked.filter(|p| *p != open);
    // a custom thumbnail cache size still being typed is saved on the way out of its section
    if go_back || picked.is_some() {
        crate::panels::settings::save_pending_cache_size(app, ctx);
    }
    if go_back {
        if back(app, ctx).is_none() {
            close(app);
        }
        return;
    }
    if let Some(p) = picked {
        // the rows may have closed Settings or opened another dialog (e.g. a folder picker's)
        if let Some(Dialog::Settings { tab: now }) = &mut app.ui.dialog {
            *now = p.to_string();
        }
        app.ui.settings_section = None;
    }
    // Settings… opens on the section shown last
    if let Some(Dialog::Settings { tab: now }) = &app.ui.dialog
        && app.ui.settings.settings_tab != *now
    {
        app.ui.settings.settings_tab = now.clone();
    }
    if picked.is_some() || app.modern_sheets.settings.wide && picked_any {
        app.modern_sheets.settings.wide = false;
    }
}

/// Settings as a window (the Classic and Compact layouts on a window at least [`ROOMY`] wide;
/// OneSlips, LR-PREF-WINDOW): the categories in a list down its left side (`left`) or in one row
/// of tabs, a category's settings in a page that scrolls inside the window, and Close
/// (`settings:back`). It opens at the size it was left at (`settingsSize` in `ui.json`) and on the
/// category shown last, can be resized from its edges, and keeps its size and place whatever the
/// category. Esc closes it.
fn window(app: &mut LccApp, ctx: &egui::Context, asked: &str, left: bool) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut tab = crate::panels::settings::window_tab(asked).to_string();
    // Esc: Close (a popup or a text field being edited takes it first)
    let mut closing = !egui::Popup::is_any_open(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    // the backdrop: an area below the window
    egui::Area::new(Id::new("settings-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(false).show(ctx, |ui| {
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
    });
    // (the sizes are the content's, inside the frame's margin)
    // (at least as wide as the row of tabs, so none is under More until the user narrows it)
    let tabs_w = if left { 0.0 } else { crate::panels::settings::tab_row_width(ctx) };
    let size = crate::panels::settings::window_size(screen.shrink(WINDOW_PAD), app.ui.settings.settings_size, tabs_w);
    let frame = egui::Frame::window(&ctx.global_style())
        .inner_margin(egui::Margin::same(WINDOW_PAD as i8))
        .corner_radius(egui::CornerRadius::same(16))
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(26)))
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(110) });
    // near the top; a window as tall as the screen allows sits in the middle of it instead, so its
    // bottom (and Close) stays on screen
    let room = (screen.height() - size.default.y - 2.0 * WINDOW_PAD) / 2.0;
    let top = (screen.height() * 0.08).clamp(crate::dialog_frame::SCREEN_MARGIN, 96.0).min(room.max(0.0));
    let shown = egui::Window::new(crate::i18n::tr("Settings"))
        .id(Id::new(LAYER))
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .frame(frame)
        .default_size(size.default)
        .min_size(size.min)
        .max_size(size.max)
        .pivot(Align2::CENTER_TOP)
        .default_pos(pos2(screen.center().x, screen.top() + top))
        .constrain(true)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            // the title centred over the tabs
            ui.vertical_centered(|ui| {
                let h = ui.label(egui::RichText::new(crate::i18n::tr("Settings")).font(t.semibold(16.0)).color(t.text));
                register(ui.ctx(), "dialog:title", h.rect);
            });
            // under the page: a gap and the Close button's row
            let footer = 2.0 * ui.spacing().item_spacing.y + ui.spacing().interact_size.y + 6.0;
            if left {
                listed_body(app, ui, &mut tab, footer);
            } else {
                crate::panels::settings::body(app, ui, &mut tab, footer);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::button::Button::new("Close").widget("settings:back").min_width(72.0).show(ui).clicked() {
                    closing = true;
                }
            });
        });
    if let Some(w) = &shown {
        crate::panels::settings::remember_size(app, w.response.rect.size() - Vec2::splat(2.0 * WINDOW_PAD), size.default);
    }
    let layer = egui::LayerId::new(egui::Order::Middle, Id::new(LAYER));
    ctx.move_to_top(layer);
    ctx.memory_mut(|m| m.set_modal_layer(layer));
    if closing {
        crate::panels::settings::save_pending_cache_size(app, ctx);
        close(app);
        return;
    }
    // the rows may have closed Settings or opened another dialog (e.g. a folder picker's)
    if let Some(Dialog::Settings { tab: now }) = &mut app.ui.dialog
        && *now != tab
    {
        if crate::panels::settings::window_tab(now) != tab {
            // another tab was picked: a section asked for (`app.settings {section}`) was the old tab's
            app.ui.settings_section = None;
        }
        *now = tab.clone();
    }
    // Settings… opens on the tab shown last
    if matches!(app.ui.dialog, Some(Dialog::Settings { .. })) && app.ui.settings.settings_tab != tab {
        app.ui.settings.settings_tab = tab;
    }
}

/// The Settings window's body with its categories listed down the left (Settings Categories:
/// Left): the list (and its search field) beside the open category's page, which scrolls inside
/// the window (`settings:rows`). The list's buttons answer to the tab row's ids too
/// (`button:settingsTab-{id}`). Ctrl+Tab / Ctrl+Page Down: the next category; Ctrl+Shift+Tab /
/// Ctrl+Page Up: the one before. `footer` is the room to leave under it for the Close button.
fn listed_body(app: &mut LccApp, ui: &mut egui::Ui, tab: &mut String, footer: f32) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let full = ui.available_size();
    let rect = Rect::from_min_size(ui.cursor().min, vec2(full.x.max(1.0), (full.y - footer).max(80.0)));
    // the list and the page draw in children of exactly this size: what they hold can't widen or
    // heighten the window
    ui.advance_cursor_after_rect(rect);
    let before = tab.clone();
    let open = section_of(tab);
    let back_key = ctrl_key(&ctx, egui::Key::Tab, true) || ctrl_key(&ctx, egui::Key::PageUp, false);
    let next_key = !back_key && (ctrl_key(&ctx, egui::Key::Tab, false) || ctrl_key(&ctx, egui::Key::PageDown, false));
    let mut picked = if back_key || next_key { step_section(app, open, if back_key { -1 } else { 1 }) } else { None };
    // a category wider than the room beside the named list (Slideshow's settings and preview)
    // takes the names' room: the list shows icons
    let cramped = open == "slideshow" && rect.width() - LIST_W < crate::panels::settings::SLIDESHOW_W + 80.0;
    let list_w = if cramped { LIST_ITEM_H + 8.0 } else { LIST_W.min(rect.width() * 0.4) };
    let list = Rect::from_min_size(rect.min, vec2(list_w, rect.height()));
    picked = rail_ui(app, ui, list, open, !cramped, true).or(picked);
    ui.painter().vline(list.right(), rect.y_range(), Stroke::new(1.0, t.divider));
    if let Some(p) = picked {
        *tab = p.to_string();
    }
    if before == "performance" && *tab != before {
        crate::panels::settings::save_pending_cache_size(app, &ctx);
    }
    let page = Rect::from_min_max(pos2(list.right() + 12.0, rect.top()), rect.max);
    let mut frame =
        ui.new_child(egui::UiBuilder::new().id_salt("settingsListedPage").max_rect(page).layout(egui::Layout::top_down(egui::Align::Min)));
    frame.set_clip_rect(page.intersect(ui.clip_rect()));
    // (the page's room: where a pointer scrolls it)
    register(&ctx, "settings:rows", page);
    // a page that goes on below shows its scroll handle all the time (as the tabbed window's)
    frame.spacing_mut().scroll.dormant_handle_opacity = 0.6;
    let shown = section_of(tab);
    egui::ScrollArea::both().id_salt(("settingsTab", shown)).auto_shrink([false, false]).show(&mut frame, |ui| {
        // room for the vertical scroll bar, so it never covers a control
        ui.set_max_width((ui.available_width() - 12.0).max(120.0));
        crate::dialog_frame::plain_form(ui, crate::panels::settings::LABEL_W, |ui| {
            crate::dialog_frame::group_begin(ui, None);
            crate::panels::settings::tab_body(app, ui, &t, shown);
        });
        register(ui.ctx(), "settings:page", ui.min_rect());
        ui.add_space(8.0);
    });
}

/// The rail: the search field (when wide) and a button per section. Returns the one picked.
/// `dense`: the Settings window's list (mouse-sized rows in the theme's colours), not the
/// full-window sheet's finger-sized rail.
fn rail_ui(app: &mut LccApp, ui: &mut egui::Ui, rail: Rect, open: &str, wide: bool, dense: bool) -> Option<&'static str> {
    let t = Tokens::get(ui.ctx());
    let mut picked = None;
    let (item_h, radius, icon, text) = if dense { (LIST_ITEM_H, 6.0, 16.0, 13.0) } else { (ITEM_H, 12.0, 20.0, 14.0) };
    let inset = if dense { vec2(0.0, 0.0) } else { vec2(8.0, 10.0) };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rail.shrink2(inset)));
    egui::ScrollArea::vertical().id_salt("settings-rail").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let query = app.modern_sheets.settings.search.trim().to_lowercase();
        if wide {
            crate::text_field::TextField::singleline("settingsSearch", &mut app.modern_sheets.settings.search)
                .hint(crate::i18n::tr("Search settings"))
                .width(ui.available_width())
                .show(ui);
            ui.add_space(8.0);
        }
        for s in SECTIONS.iter().filter(|s| shown(app, s)) {
            let matches = query.is_empty() || crate::i18n::tr(s.label).to_lowercase().contains(&query) || s.words.contains(query.as_str());
            if wide && !matches {
                continue;
            }
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - if dense { 8.0 } else { 0.0 }, item_h), Sense::click());
            let name = crate::i18n::tr(s.label);
            let on = s.key == open;
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name));
            register(ui.ctx(), format!("settings:section:{}", s.key), r);
            // the old dialog's tab ids reach the section that holds them
            for tab in s.tabs {
                register(ui.ctx(), format!("button:settingsTab-{tab}"), r);
            }
            let resp = if wide { resp } else { resp.on_hover_text(name) };
            let p = ui.painter();
            if dense {
                if on {
                    p.rect_filled(r, radius, t.pressed);
                } else if resp.hovered() || resp.is_pointer_button_down_on() {
                    p.rect_filled(r, radius, t.hover);
                }
            } else if on {
                p.rect_filled(r, radius, Color32::from_white_alpha(30));
                p.rect_stroke(r, radius, Stroke::new(1.0, Color32::from_white_alpha(24)), egui::StrokeKind::Inside);
            } else if resp.hovered() || resp.is_pointer_button_down_on() {
                p.rect_filled(r, radius, Color32::from_white_alpha(14));
            }
            let c = if on { t.accent } else { t.text_label };
            let icon_x = if dense { 16.0 } else { 22.0 };
            let icon_at = if wide { pos2(r.left() + icon_x, r.center().y) } else { r.center() };
            paint(p, Rect::from_center_size(icon_at, Vec2::splat(icon)), s.icon, c);
            if wide {
                let x = r.left() + icon_x + icon / 2.0 + 10.0;
                p.text(pos2(x, r.center().y), Align2::LEFT_CENTER, name, t.font(text), if on { t.text } else { t.text_label });
            }
            if resp.clicked() {
                picked = Some(s.key);
            }
        }
    });
    picked
}

/// The open section's rows: its old tabs' own, one after another, finger-sized; labels above their
/// controls when narrow.
/// `asked`: the tab Settings was opened on; when it is a later part of the section, the rows scroll
/// to it once.
fn rows(app: &mut LccApp, ui: &mut egui::Ui, area: Rect, open: &str, asked: &str) {
    let t = Tokens::get(ui.ctx());
    let Some(section) = SECTIONS.iter().find(|s| s.key == open) else { return };
    let stacked = area.width() < 520.0;
    // (Slideshow: its settings and their live preview side by side)
    let max = if open == "slideshow" { crate::panels::settings::SLIDESHOW_W + 2.0 * crate::dialog_frame::GROUP_PAD + 40.0 } else { CONTENT_MAX };
    let w = area.width().min(max + 32.0);
    let column = Rect::from_min_size(pos2(area.left() + ((area.width() - w) / 2.0).max(0.0), area.top()), vec2(w, area.height()));
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(column.shrink2(vec2(16.0, 0.0))));
    // (the scrolling column: where a pointer scrolls the rows)
    register(ui.ctx(), "settings:rows", column);
    let was = crate::dialog_frame::set_finger_sized(ui.ctx(), true);
    egui::ScrollArea::vertical().id_salt(("settings-rows", open)).auto_shrink([false, false]).show(&mut inner, |ui| {
        let s = ui.spacing_mut();
        s.item_spacing = vec2(10.0, 10.0);
        s.interact_size = vec2(44.0, 36.0);
        s.button_padding = vec2(12.0, 6.0);
        s.icon_width = 22.0;
        s.icon_width_inner = 13.0;
        s.icon_spacing = 10.0;
        s.combo_height = 480.0;
        for (style, size) in [(egui::TextStyle::Body, 14.5), (egui::TextStyle::Button, 14.5)] {
            if let Some(f) = ui.style_mut().text_styles.get_mut(&style) {
                f.size = size;
            }
        }
        ui.add_space(14.0);
        ui.set_width((ui.available_width() - 12.0).max(120.0));
        for tab in section.tabs {
            if *tab == asked {
                let s = &mut app.modern_sheets.settings.scrolled_to;
                if s.0 != asked {
                    *s = (asked.to_string(), 0);
                }
                if s.1 < 3 {
                    s.1 += 1;
                    if section.tabs.first() != Some(tab) {
                        ui.scroll_to_cursor(Some(egui::Align::TOP));
                    }
                }
            }
            let body = |ui: &mut egui::Ui| {
                crate::dialog_frame::group_begin(ui, None);
                if stacked && *tab == "shortcuts" {
                    ui.label(egui::RichText::new(crate::i18n::tr("Keyboard shortcuts are edited on a wider screen.")).color(t.text_label));
                    return;
                }
                crate::panels::settings::tab_body(app, ui, &t, tab);
            };
            if stacked {
                crate::dialog_frame::stacked_form(ui, 120.0, body);
            } else {
                crate::dialog_frame::form(ui, 190.0, body);
            }
            ui.add_space(6.0);
        }
        ui.add_space(48.0);
    });
    crate::dialog_frame::set_finger_sized(ui.ctx(), was);
}

/// Tests: scrolls Settings' rows until widget `id` is in view (the sections are taller than a small
/// window), as a person scrolling to it would.
#[cfg(test)]
pub(crate) fn reveal(h: &mut crate::headless::Headless, id: &str) {
    use std::time::Duration;
    let t = Duration::from_secs(10);
    let find = |h: &crate::headless::Headless, w: &str| h.app.widgets.iter().rev().find(|(i, _)| i == w).map(|(_, r)| *r);
    for _ in 0..40 {
        h.step();
        let Some(rows) = find(h, "settings:rows") else { panic!("Settings' rows are not on screen") };
        let dy = match find(h, id) {
            Some(w) if w.top() >= rows.top() + 4.0 && w.bottom() <= rows.bottom() - 4.0 => {
                // in view; wait for a scroll's glide to stop (a layout may keep nudging it by a
                // pixel or two, so "stopped" allows that), then it must still be in view
                let mut last = w;
                for _ in 0..30 {
                    for _ in 0..3 {
                        h.step();
                    }
                    let Some(now) = find(h, id) else { break };
                    let still = (now.top() - last.top()).abs() <= 2.0;
                    last = now;
                    if still {
                        if now.top() >= rows.top() + 4.0 && now.bottom() <= rows.bottom() - 4.0 {
                            return;
                        }
                        break;
                    }
                }
                continue;
            }
            Some(w) => (rows.top() + 120.0 - w.center().y).clamp(-500.0, 500.0),
            None => -300.0,
        };
        let r = h.request("ui.hoverWidget", serde_json::json!({"id": "settings:rows", "fx": 0.9, "fy": 0.5}), t);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.scroll", serde_json::json!({"dy": dy}), t);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..8 {
            h.step();
        }
    }
    panic!("{id} never came into view in Settings");
}

#[cfg(test)]
mod tests {
    /// Both presentations of Settings show the same categories, in the same order: the rail's
    /// sections are the Settings window's tabs ([`crate::panels::settings::TABS`], PR #830).
    #[test]
    fn sections_match_the_settings_window_s_tabs() {
        let tabs = crate::panels::settings::TABS;
        assert_eq!(super::SECTIONS.len(), tabs.len(), "one section per tab");
        for (s, (id, label)) in super::SECTIONS.iter().zip(tabs) {
            assert_eq!((s.key, s.label), (*id, *label), "same key and name, in the tab row's order");
            assert_eq!(s.tabs, &[*id], "the section shows exactly its tab's rows");
        }
    }
}
