//! The Settings window (⌘,): one row of tabs ([`TABS`]: General, Import, Folders, Performance,
//! Display, Interface, Slideshow, Faces, Profiles, AI Denoise, AI Models, Generative Fill,
//! Shortcuts, Controls, Connections, Sync), those that don't fit under More. The touch screen
//! (`crate::settings_screen`) shows the same tabs' rows by section.
//!
//! Changes apply immediately (no OK/Cancel). Where they are stored:
//! - **app settings** ([`crate::state::AppSettings`]: startup view, delete confirmation, GPU,
//!   preview size, filmstrip/grid badges, last library, monitor profile, whether Import Lightroom
//!   Catalog… asks where a catalog goes) and Auto Advance live in
//!   the UI state,
//!   saved by the host in its config folder (`ui.json`);
//! - **library settings** (import defaults, XMP sidecars, thumbnail cache size, slideshow
//!   playback) go through the `library.preferences` / `library.xmpPreferences` /
//!   `slideshow.playback` commands into the library's `prefs.json`,
//!   so they travel with the library.

use egui::RichText;
use serde_json::{Value, json};

use crate::LccApp;
use crate::state::{GridBadges, PREVIEW_LIMITS, StartupView};
use crate::theme::Tokens;
use crate::widgets::register;

/// Sections `app.settings {section}` can open, with their tab.
pub const SECTIONS: &[(&str, &str)] = &[("lensProfiles", "import"), ("autoImport", "import"), ("semanticSearch", "ai")];

/// (id, label) of the tabs, in the order of the Settings window's one row of tabs (OneSlips,
/// LR-PREF-WINDOW): the settings used most first, the editing helpers and the AI next, the
/// keyboard, control surfaces and other devices last. Tabs that don't fit the window's width are
/// under More at the end of the row.
pub const TABS: &[(&str, &str)] = &[
    ("general", "General"),
    ("import", "Import"),
    ("folders", "Folders"),
    ("performance", "Performance"),
    ("display", "Display"),
    ("interface", "Interface"),
    ("slideshow", "Slideshow"),
    ("faces", "Faces"),
    ("profiles", "Profiles"),
    ("develop", "Develop"),
    ("denoise", "AI Denoise"),
    ("ai", "AI Models"),
    ("genfill", "Generative Fill"),
    ("shortcuts", "Shortcuts"),
    ("controls", "Controls"),
    ("connections", "Connections"),
    ("sync", "Sync"),
];

/// The room Settings ▸ Slideshow wants: its settings column, the gap and the live preview.
pub(crate) const SLIDESHOW_W: f32 = SLIDESHOW_COLUMN_W + 12.0 + super::slide_layout::PREVIEW_W;

/// Thumbnail cache sizes offered (MB).
const CACHE_SIZES: [u32; 5] = [512, 1024, 2048, 4096, 8192];

/// The label column of the Settings window's rows (it gives up room in a narrow window).
pub(crate) const LABEL_W: f32 = 170.0;
/// The label column of the same rows outside the dialog (the slideshow panels).
const PANEL_LABEL_W: f32 = 150.0;

/// The Settings window's size (points, inside its frame) when it first opens, before the user
/// resizes it: tall, so most tabs show whole, and at least as wide as its row of tabs
/// ([`tab_row_width`]), so every tab is in the row (the screen caps both).
pub const DEFAULT_SIZE: [f32; 2] = [900.0, 1340.0];
/// The size every window opened at before the default grew to fit the tabs (PR #830): a size
/// kept at exactly this was never chosen, so it opens at the new default.
const OLD_DEFAULT_SIZE: [f32; 2] = [900.0, 640.0];
/// The smallest the user can make it (a small screen can make it smaller still).
pub const MIN_SIZE: [f32; 2] = [620.0, 420.0];
/// Room kept free around the window on the screen.
const SCREEN_MARGIN: f32 = 12.0;
/// Height of the tab row.
const TAB_H: f32 = 30.0;
/// Room on each side of a tab's label: the most that lets every tab fit the row (long names, as
/// in Russian or German, get less before any tab goes to the More menu).
const TAB_PADS: [f32; 3] = [12.0, 9.0, 7.0];

/// The sizes the Settings window opens at and may be resized between on `screen`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSize {
    pub default: egui::Vec2,
    pub min: egui::Vec2,
    pub max: egui::Vec2,
}

/// The Settings window's sizes on `screen`: `saved` (the size the user left it at, else
/// [`DEFAULT_SIZE`], widened to `tabs_w`, the room its row of tabs takes) within [`MIN_SIZE`] and
/// the screen. A saved size that is no size at all (NaN, negative: a hand-edited settings file),
/// or the old default nobody chose, is the default.
pub fn window_size(screen: egui::Rect, saved: Option<[f32; 2]>, tabs_w: f32) -> WindowSize {
    let finite = |v: f32| v.is_finite() && v > 0.0;
    let max = egui::vec2(
        if finite(screen.width()) { (screen.width() - 2.0 * SCREEN_MARGIN).max(200.0) } else { DEFAULT_SIZE[0] },
        if finite(screen.height()) { (screen.height() - 2.0 * SCREEN_MARGIN).max(160.0) } else { OLD_DEFAULT_SIZE[1] },
    );
    let min = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]).min(max);
    let fresh = [DEFAULT_SIZE[0].max(if finite(tabs_w) { tabs_w.ceil() } else { 0.0 }), DEFAULT_SIZE[1]];
    let want = saved.filter(|[w, h]| finite(*w) && finite(*h) && [*w, *h] != OLD_DEFAULT_SIZE).unwrap_or(fresh);
    let default = egui::vec2(want[0], want[1]).max(min).min(max);
    WindowSize { default, min, max }
}

/// Keeps the Settings window's `size` (as the user left it) for the next time it opens. A window
/// still at the size it opened at (`opened`) when nothing was saved was not sized by the user:
/// nothing is kept, so it follows the default (a new tab, another language, another screen).
pub fn remember_size(app: &mut LccApp, size: egui::Vec2, opened: egui::Vec2) {
    let saved = app.ui.settings.settings_size;
    let differs = |a: egui::Vec2, [w, h]: [f32; 2]| (w - a.x).abs() > 0.5 || (h - a.y).abs() > 0.5;
    if size.x.is_finite()
        && size.y.is_finite()
        && size.x > 0.0
        && size.y > 0.0
        && saved.is_none_or(|kept| differs(size, kept))
        && (saved.is_some() || differs(size, [opened.x, opened.y]))
    {
        app.ui.settings.settings_size = Some([size.x.round(), size.y.round()]);
    }
}

/// The width (points) the Settings window's row of tabs takes with the most room around each
/// label: a window this wide shows every tab, none under More.
pub fn tab_row_width(ctx: &egui::Context) -> f32 {
    let t = Tokens::get(ctx);
    let font = t.font(12.5);
    let pad = TAB_PADS.first().copied().unwrap_or(12.0);
    ctx.fonts_mut(|f| {
        shown_tabs().map(|(_, label)| (f.layout_no_wrap(crate::i18n::tr(label).to_string(), font.clone(), t.text).size().x + 2.0 * pad).ceil()).sum()
    })
}

/// The tabs this build shows (the browser has no device sync, no connections and no control surfaces).
pub fn shown_tabs() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    TABS.iter().filter(|(id, _)| !cfg!(target_arch = "wasm32") || !matches!(*id, "connections" | "sync" | "controls"))
}

/// The tab of the Settings window that `asked` opens: a tab's own id, or one of the touch screen's
/// section names (`library`, `editing`, …: the first tab it holds).
pub fn window_tab(asked: &str) -> &'static str {
    if let Some((id, _)) = shown_tabs().find(|(id, _)| *id == asked) {
        return id;
    }
    match asked {
        "library" => "folders",
        "editing" => "denoise",
        _ => "general",
    }
}

/// Which tabs fit one row `room` wide, given each tab's width: the indices shown (in order)
/// and the ones left for the More menu (`more_w` wide). The `selected` tab is always shown: when it
/// would be in the menu it takes the place of the last tab that fits.
pub fn fit_tabs(widths: &[f32], selected: usize, room: f32, more_w: f32) -> (Vec<usize>, Vec<usize>) {
    let total: f32 = widths.iter().sum();
    if total <= room {
        return ((0..widths.len()).collect(), Vec::new());
    }
    let budget = room - more_w;
    let mut shown = Vec::new();
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        if used + w > budget {
            break;
        }
        used += w;
        shown.push(i);
    }
    if selected < widths.len() && !shown.contains(&selected) {
        let need = widths.get(selected).copied().unwrap_or(0.0);
        while used + need > budget {
            let Some(last) = shown.pop() else { break };
            used -= widths.get(last).copied().unwrap_or(0.0);
        }
        shown.push(selected);
    }
    let hidden = (0..widths.len()).filter(|i| !shown.contains(i)).collect();
    (shown, hidden)
}

/// The tab after (`step` 1) or before (-1) `tab` among those shown, wrapping around.
pub fn step_tab(tab: &str, step: isize) -> &'static str {
    let tabs: Vec<&'static str> = shown_tabs().map(|(id, _)| *id).collect();
    let n = tabs.len() as isize;
    let at = tabs.iter().position(|id| *id == tab).unwrap_or(0) as isize;
    let next = (at + step).rem_euclid(n.max(1)) as usize;
    tabs.get(next).copied().unwrap_or("general")
}

fn tab_focus_id(id: &str) -> egui::Id {
    egui::Id::new("settingsTab").with(id)
}

/// The Settings window's body for `tab` (OneSlips, LR-PREF-WINDOW): one row of tabs, then the
/// tab's settings in a page that scrolls inside the window (`settings:rows`), so a tab never
/// changes the window's size. `footer` is the room to leave under it for the Close button.
pub fn body(app: &mut LccApp, ui: &mut egui::Ui, tab: &mut String, footer: f32) {
    let t = Tokens::get(ui.ctx());
    if !shown_tabs().any(|(id, _)| *id == tab.as_str()) {
        *tab = window_tab(tab).to_string();
    }
    let full = ui.available_size();
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(full.x.max(1.0), (full.y - footer).max(80.0)));
    // the tabs draw in a child of exactly this size: what they hold can't widen or heighten the window
    ui.advance_cursor_after_rect(rect);
    let mut frame = ui.new_child(egui::UiBuilder::new().id_salt("settingsFrame").max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    frame.set_clip_rect(rect.intersect(ui.clip_rect()));
    let before = tab.clone();
    keyboard(&mut frame, tab);
    tab_strip(&mut frame, &t, tab);
    if before == "performance" && *tab != before {
        save_pending_cache_size(app, frame.ctx());
    }
    frame.add_space(8.0);
    // (the page's room: where a pointer scrolls it)
    register(frame.ctx(), "settings:rows", frame.available_rect_before_wrap());
    // a page that goes on below shows its scroll handle all the time: without it, a page a line or
    // two too long looks cut off, not scrollable
    frame.spacing_mut().scroll.dormant_handle_opacity = 0.6;
    egui::ScrollArea::both().id_salt(("settingsTab", tab.as_str())).auto_shrink([false, false]).show(&mut frame, |ui| {
        // room for the vertical scroll bar, so it never covers a control
        ui.set_max_width((ui.available_width() - 12.0).max(120.0));
        crate::dialog_frame::plain_form(ui, LABEL_W, |ui| {
            crate::dialog_frame::group_begin(ui, None);
            tab_body(app, ui, &t, tab);
        });
        register(ui.ctx(), "settings:page", ui.min_rect());
        ui.add_space(8.0);
    });
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

/// Ctrl+Tab / Ctrl+Page Down: the next tab; Ctrl+Shift+Tab / Ctrl+Page Up: the previous one;
/// ← / → while a tab has the keyboard focus: the tab beside it (the focus follows).
fn keyboard(ui: &mut egui::Ui, tab: &mut String) {
    let ctx = ui.ctx().clone();
    let back = ctrl_key(&ctx, egui::Key::Tab, true) || ctrl_key(&ctx, egui::Key::PageUp, false);
    let forward = !back && (ctrl_key(&ctx, egui::Key::Tab, false) || ctrl_key(&ctx, egui::Key::PageDown, false));
    if back || forward {
        *tab = step_tab(tab, if back { -1 } else { 1 }).to_string();
        return;
    }
    let focused = ctx.memory(|m| m.focused());
    let Some(on_tab) = shown_tabs().find(|(id, _)| Some(tab_focus_id(id)) == focused).map(|(id, _)| *id) else { return };
    let step = ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            1
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            -1
        } else {
            0
        }
    });
    if step != 0 {
        let next = step_tab(on_tab, step);
        *tab = next.to_string();
        ctx.memory_mut(|m| {
            // (egui would move the focus by the arrow too, past the tab chosen here)
            m.move_focus(egui::FocusDirection::None);
            m.request_focus(tab_focus_id(next));
        });
    }
}

/// One row of tabs (`button:settingsTab-{id}`), the chosen one underlined. Tabs that don't fit
/// the window's width are in a More menu at its end (`button:settingsTabMore`) rather than on a
/// second row; the chosen tab is always in the row.
fn tab_strip(ui: &mut egui::Ui, t: &Tokens, tab: &mut String) {
    let tabs: Vec<&(&str, &str)> = shown_tabs().collect();
    let font = t.font(12.5);
    let galleys: Vec<_> =
        tabs.iter().map(|(_, label)| ui.painter().layout_no_wrap(crate::i18n::tr(label).to_string(), font.clone(), t.text)).collect();
    let room = ui.available_width();
    let widths_with = |pad: f32| -> Vec<f32> { galleys.iter().map(|g| (g.size().x + 2.0 * pad).ceil()).collect() };
    let pad = TAB_PADS.iter().copied().find(|pad| widths_with(*pad).iter().sum::<f32>() <= room).unwrap_or(7.0);
    let widths = widths_with(pad);
    let more_label = crate::i18n::tr("More").to_string();
    let more_galley = ui.painter().layout_no_wrap(more_label.clone(), font.clone(), t.text);
    let more_w = (more_galley.size().x + 2.0 * pad + 12.0).ceil();
    let selected = tabs.iter().position(|(id, _)| *id == tab.as_str()).unwrap_or(0);
    let (shown, hidden) = fit_tabs(&widths, selected, room, more_w);
    let (strip, _) = ui.allocate_exact_size(egui::vec2(room, TAB_H), egui::Sense::hover());
    let painter = ui.painter_at(strip.expand2(egui::vec2(0.0, 1.0)));
    painter.hline(strip.x_range(), strip.bottom() - 0.5, egui::Stroke::new(1.0, t.divider));
    let mut x = strip.left();
    for i in shown {
        let (Some((id, label)), Some(galley), Some(w)) = (tabs.get(i), galleys.get(i), widths.get(i)) else { continue };
        let r = egui::Rect::from_min_size(egui::pos2(x, strip.top()), egui::vec2(*w, TAB_H));
        x += w;
        let resp = ui.interact(r, tab_focus_id(id), egui::Sense::click());
        let active = i == selected;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, crate::i18n::tr(label)));
        register(ui.ctx(), format!("button:settingsTab-{id}"), r);
        if resp.hovered() && !active {
            painter.rect_filled(r.shrink2(egui::vec2(2.0, 3.0)), 4.0, t.hover);
        }
        let color = if active {
            t.text
        } else if resp.hovered() {
            t.text_label
        } else {
            t.text_dim
        };
        painter.galley(r.center() - galley.size() / 2.0, galley.clone(), color);
        if active {
            let line = egui::Rect::from_min_max(egui::pos2(r.left() + 6.0, r.bottom() - 2.0), egui::pos2(r.right() - 6.0, r.bottom()));
            painter.rect_filled(line, 1.0, t.accent);
        }
        if resp.has_focus() {
            painter.rect_stroke(r.shrink(2.0), 4.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        }
        if resp.clicked() {
            *tab = id.to_string();
        }
    }
    if hidden.is_empty() {
        return;
    }
    let r = egui::Rect::from_min_size(egui::pos2(strip.right() - more_w, strip.top()), egui::vec2(more_w, TAB_H));
    let resp = ui.interact(r, egui::Id::new("settingsTabMore"), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &more_label));
    register(ui.ctx(), "button:settingsTabMore", r);
    if resp.hovered() {
        painter.rect_filled(r.shrink2(egui::vec2(2.0, 3.0)), 4.0, t.hover);
    }
    let text_at = egui::pos2(r.left() + pad, r.center().y - more_galley.size().y / 2.0);
    painter.galley(text_at, more_galley.clone(), if resp.hovered() { t.text_label } else { t.text_dim });
    let chevron = egui::Rect::from_center_size(egui::pos2(r.right() - pad - 3.0, r.center().y), egui::vec2(8.0, 8.0));
    crate::icons::paint(&painter, chevron, crate::icons::Icon::ChevronDown, t.text_dim);
    egui::Popup::menu(&resp).show(|ui| {
        for i in &hidden {
            let Some((id, label)) = tabs.get(*i) else { continue };
            let r = ui.button(crate::i18n::tr(label));
            register(ui.ctx(), format!("button:settingsTab-{id}"), r.rect);
            if r.clicked() {
                *tab = id.to_string();
            }
        }
    });
}

pub(crate) fn tab_body(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, tab: &str) {
    match tab {
        "import" => import_tab(app, ui, t),
        "folders" => super::settings_folders::settings_tab(app, ui, t),
        "performance" => performance_tab(app, ui, t),
        "display" => display_tab(app, ui, t),
        "interface" => interface_tab(app, ui, t),
        "slideshow" => slideshow_tab(app, ui, t),
        "faces" => super::faces::settings_tab(app, ui, t),
        "denoise" => super::denoise::settings_tab(app, ui, t),
        "ai" => super::ai_models::settings_tab(app, ui, t),
        "genfill" => super::genfill::settings_tab(app, ui, t),
        "profiles" => super::settings_profiles::settings_tab(app, ui, t),
        "develop" => super::settings_develop::settings_tab(app, ui, t),
        "shortcuts" => shortcuts_tab(app, ui, t),
        "controls" => super::controls::settings_tab(app, ui, t),
        #[cfg(not(target_arch = "wasm32"))]
        "connections" => super::connections::settings_tab(app, ui, t),
        #[cfg(not(target_arch = "wasm32"))]
        "sync" => super::peer_sync::settings_tab(app, ui, t),
        _ => general_tab(app, ui, t),
    }
}

/// `path` shortened to fit `max` (as `width` measures it) by dropping leading folders, after
/// either kind of separator, so that its end stays (`…/Pictures/Library`,
/// `…\Pictures\Library`): the end says which folder it is. A last folder too long by itself
/// loses its end ([`crate::widgets::elide_head`]).
fn elide_path(path: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(path) <= max {
        return path.to_string();
    }
    // (a separator at the very end is not where a name starts: `…/` alone would say nothing)
    let end = path.trim_end_matches(['/', '\\']).len();
    let mut rest = path;
    while let Some(i) = rest.find(['/', '\\']).filter(|i| path.len() - rest.len() + i + 1 < end) {
        let separator = if rest.get(i..).is_some_and(|r| r.starts_with('\\')) { '\\' } else { '/' };
        rest = rest.get(i + 1..).unwrap_or("");
        let shorter = format!("…{separator}{rest}");
        if width(&shorter) <= max {
            return shorter;
        }
    }
    let name = rest.trim_end_matches(['/', '\\']);
    // (nothing but separators: never an empty label)
    if name.is_empty() { "…".to_string() } else { crate::widgets::elide_head(name, max, width) }
}

/// A folder or file path on one line in the room left in its row (`text:{id}`): shortened from
/// the front when it is too long ([`elide_path`]), with all of it in a tooltip then. `after` is
/// the width of what follows it on the line (a button, a note) and its spacing: the path leaves
/// that room, so those never wrap onto a second line.
fn path_label(ui: &mut egui::Ui, id: &str, path: &str, font: egui::FontId, color: egui::Color32, after: f32) {
    let shown =
        elide_path(path, (ui.available_width() - after).max(0.0), |s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x);
    // (`truncate` is the last resort, for a room narrower than "…")
    let r = ui.add(egui::Label::new(RichText::new(&shown).font(font).color(color)).truncate().show_tooltip_when_elided(false));
    register(ui.ctx(), format!("text:{id}"), r.rect);
    if shown != path {
        r.on_hover_text(path);
    }
}

/// The width of `text` in the style `style`, and the gap before it: what a label takes in a row.
fn label_width(ui: &egui::Ui, text: &str, style: egui::TextStyle) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), style.resolve(ui.style()), egui::Color32::WHITE).size().x + ui.spacing().item_spacing.x
}

/// The same for a button, which has padding either side of its text.
fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    label_width(ui, text, egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.x
}

/// A section heading; in a dialog form it starts a titled group (an inset panel).
pub(super) fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    if crate::dialog_frame::in_form(ui) {
        crate::dialog_frame::group_begin(ui, Some(text));
        return;
    }
    ui.add_space(4.0);
    ui.label(RichText::new(crate::i18n::tr(text)).font(t.semibold(12.5)).color(t.text));
}

/// [`heading`] with a help link (`?`, `help:{topic}`, [`crate::help_link`]) just after it, opening
/// the manual at the part about this group of settings.
pub(crate) fn heading_help(ui: &mut egui::Ui, t: &Tokens, text: &str, topic: &str) {
    if crate::dialog_frame::in_form(ui) {
        crate::dialog_frame::group_begin_help(ui, Some(text), Some(topic));
        return;
    }
    ui.add_space(4.0);
    let r = ui.label(RichText::new(crate::i18n::tr(text)).font(t.semibold(12.5)).color(t.text));
    crate::help_link::beside(ui, r.rect, topic);
}

/// A labelled row: in a dialog form the label is right-aligned in the label column.
pub(super) fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if crate::dialog_frame::in_form(ui) {
        return crate::dialog_frame::form_row(ui, label, add);
    }
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(PANEL_LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(PANEL_LABEL_W);
            // (a label longer than its column wraps in it; it does not push the controls along)
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).wrap());
        });
        // The controls wrap in the room beside the label, each further line starting under the
        // first control, so a row with many choices grows down and never widens the window.
        let wrapped = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), wrapped, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            add(ui)
        })
        .inner
    })
    .inner
}

/// Help text: small and muted; in a dialog form, under the control column.
/// "8 GB", "1.5 GB", "512 MB"; within a few per cent of whole gigabytes it reads as whole ones (a computer's "64 GB"
/// is a little less, so its automatic quarter is "16 GB", not "15.9 GB").
fn memory_size(mb: u64) -> String {
    if mb < 1024 {
        return format!("{mb} MB");
    }
    let gb = mb as f64 / 1024.0;
    if (gb - gb.round()).abs() <= gb * 0.02 { format!("{:.0} GB", gb.round()) } else { format!("{gb:.1} GB") }
}

/// Settings ▸ Performance ▸ Memory for caches: Automatic (what it is on this computer), then the sizes up to the most
/// this computer allows (`max`, [`lcc_engine::memory::max_budget`]), and a size set elsewhere (an agent, another
/// computer's settings) as its own choice. Values in MB, 0 = automatic. Before, the choices stopped at 4 GB whatever
/// the computer had, though the engine takes up to three quarters of its memory.
pub(crate) fn memory_choices(current: u32, auto: usize, max: usize) -> Vec<(u32, String)> {
    let max_mb = u64::try_from(max >> 20).unwrap_or(u64::MAX);
    let mut opts = vec![(0u32, format!("{} ({})", crate::i18n::tr("Automatic"), memory_size(u64::try_from(auto >> 20).unwrap_or(u64::MAX))))];
    for mb in [512u32, 1024, 2048, 4096, 8192, 16_384, 32_768, 65_536, 131_072] {
        if u64::from(mb) <= max_mb {
            opts.push((mb, memory_size(u64::from(mb))));
        }
    }
    if current > 0 && !opts.iter().any(|(v, _)| *v == current) {
        opts.push((current, memory_size(u64::from(current))));
    }
    opts
}

pub(super) fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    if crate::dialog_frame::in_form(ui) {
        crate::dialog_frame::help(ui, text);
        return;
    }
    ui.label(RichText::new(crate::i18n::tr(text)).size(11.0).color(t.text_dim));
}

/// A checkbox addressable as `check:{id}`; true when toggled. In a dialog form it sits in the
/// control column.
pub(super) fn check(ui: &mut egui::Ui, id: &str, value: &mut bool, label: &str) -> bool {
    crate::dialog_frame::form_check(ui, id, value, label)
}

/// Mutually exclusive buttons (`button:{id}-{index}`).
pub(super) fn choices<V: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, options: &[(V, &str)], value: &mut V) -> bool {
    if crate::dialog_frame::plain(ui) && crate::dialog_frame::in_form(ui) {
        // the Settings window: small buttons side by side, the chosen one filled, wrapping in the row
        let mut changed = false;
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, (v, l)) in options.iter().enumerate() {
            if crate::widgets::text_button(ui, &format!("{id}-{i}"), l, *value == *v).clicked() && *value != *v {
                *value = *v;
                changed = true;
            }
        }
        return changed;
    }
    // one segmented control (`button:{id}-{i}` each)
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let chosen = options.iter().position(|(v, _)| *v == *value);
    match crate::widgets::segments(ui, id, &labels, chosen).and_then(|i| options.get(i)) {
        Some((v, _)) if *v != *value => {
            *value = *v;
            true
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------------------- Shortcuts

/// The shortcut editor (the command registry, with conflict handling), the keymap set and
/// keymap file import/export.
fn shortcuts_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::shortcuts::KeymapSet;
    let mut set = app.ui.settings.keymap_set;
    row(ui, t, "Keymap", |ui| {
        if choices(ui, "keymapSet", &[(KeymapSet::Classic, "Classic"), (KeymapSet::Alternative, "Alternative")], &mut set) {
            let key = if set == KeymapSet::Classic { "classic" } else { "alternative" };
            let _ = app.run("app.keymapSet", serde_json::json!({"set": key}));
        }
        crate::help_link::icon(ui, "shortcuts");
    });
    row(ui, t, "Keymap file", |ui| {
        if crate::widgets::text_button(ui, "keymapImport", "Import…", false).clicked() {
            let _ = app.run("app.keymapImport", serde_json::json!({}));
        }
        if crate::widgets::text_button(ui, "keymapExport", "Export…", false).clicked() {
            let _ = app.run("app.keymapExport", serde_json::json!({}));
        }
    });
    super::keymap::body(app, ui, t);
}

// ------------------------------------------------------------------------------------- General

fn general_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    // Classic, Compact (Classic drawn denser) or Modern (Android's default), Classic and its denser
    // twin first; on each, this row switches back (`button:settingsLayout-{0,1,2}`)
    row(ui, t, crate::i18n::tr("Layout"), |ui| {
        let now = if app.ui.modern.enabled {
            2
        } else if app.compact() {
            1
        } else {
            0
        };
        let mut layout = now;
        let changed = choices(ui, "settingsLayout", &[(0, "Classic – Advanced"), (1, "Compact – Dense"), (2, "Modern – Simple")], &mut layout);
        crate::help_link::icon(ui, "layout");
        if changed {
            let _ = match layout {
                2 => app.run("view.modernLayout", json!({"on": true})),
                1 => app.run("view.compactLayout", json!({"on": true})),
                _ => {
                    let _ = app.run("view.modernLayout", json!({"on": false}));
                    app.run("view.compactLayout", json!({"on": false}))
                }
            };
        }
    });
    // where the Settings window lists its categories: Classic and Compact each keep their own
    // (Modern always lists them down the left of its full-window sheet: no choice there)
    if let Some(mut now) = crate::settings_screen::categories(app) {
        use crate::state::SettingsCategories as C;
        row(ui, t, crate::i18n::tr("Settings Categories"), |ui| {
            if choices(ui, "settingsCategories", &[(C::Left, "Left"), (C::Top, "Top")], &mut now) {
                crate::settings_screen::set_categories(app, now);
            }
        });
    }
    // the Edit panel's colour tools: Lightroom Classic's panels, or the colourist's Witchcraft
    // layout (Window ▸ Grading Mode and Ctrl+Shift+G switch it too)
    row(ui, t, crate::i18n::tr("Grading Mode"), |ui| {
        let mut mode = app.ui.settings.grading_mode;
        if choices(
            ui,
            "settingsGradingMode",
            &[(crate::state::GradingMode::Eboda, "Eboda"), (crate::state::GradingMode::Witchcraft, "Witchcraft")],
            &mut mode,
        ) {
            let _ = app.run("view.gradingMode", json!({"mode": mode.id()}));
        }
        crate::help_link::icon(ui, "gradingMode");
    });
    row(ui, t, crate::i18n::tr("Language"), |ui| {
        // a list, not a row of buttons: there are too many languages for one line
        let current = language_label(ui.ctx(), app.ui.language);
        let combo = egui::ComboBox::from_id_salt("settingsLanguage").selected_text(current).width(220.0).height(420.0).show_ui(ui, |ui| {
            // English first, then by each language's own name (as Edit ▸ Language lists them)
            for (i, language) in crate::i18n::Locale::for_picker().into_iter().enumerate() {
                let r = ui.selectable_value(&mut app.ui.language, language, language_label(ui.ctx(), language));
                register(ui.ctx(), format!("button:settingsLanguage-{i}"), r.rect);
            }
        });
        register(ui.ctx(), "combo:settingsLanguage", combo.response.rect);
        crate::i18n::set_language(app.ui.language);
    });
    heading_help(ui, t, crate::i18n::tr("Catalog"), "catalog");
    let location = match &app.session.library {
        Some(l) => l.dir.display().to_string(),
        None => crate::i18n::tr("In memory (nothing is saved)").to_string(),
    };
    row(ui, t, crate::i18n::tr("Location"), |ui| {
        path_label(ui, "settingsLocation", &location, egui::TextStyle::Body.resolve(ui.style()), t.text, 0.0);
    });
    row(ui, t, "", |ui| {
        let can = app.services.pick_folder.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Open Catalog…")));
        register(ui.ctx(), "button:settingsOpenLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.openLibrary", json!({}));
        }
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("New Catalog…")));
        register(ui.ctx(), "button:settingsNewLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.newLibrary", json!({}));
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    // where libraries are folders to choose from (not in the browser, which keeps one)
    if app.services.pick_folder.is_some() {
        use crate::state::LaunchLibrary;
        row(ui, t, crate::i18n::tr("At launch"), |ui| {
            choices(
                ui,
                "settingsAtLaunch",
                &[(LaunchLibrary::Last, "Open the last catalog"), (LaunchLibrary::Ask, "Ask me")],
                &mut app.ui.settings.at_launch,
            );
        });
        hint(ui, t, crate::i18n::tr("Ask me lists your recent catalogs at launch. Holding Alt (Option on a Mac) as the window appears asks once."));
    }
    // File ▸ Import Lightroom or Capture One Catalog… (#2672): the window that asks where a catalog goes, or not
    crate::dialog_frame::note(ui, RichText::new(crate::i18n::tr("When importing a Lightroom or Capture One catalog")).color(t.text_label));
    row(ui, t, "", |ui| {
        use crate::state::LightroomImport;
        choices(
            ui,
            "settingsLightroomImport",
            &[(LightroomImport::Ask, "Ask where to import it"), (LightroomImport::AlwaysMerge, "Always merge into the open catalog")],
            &mut app.ui.settings.lightroom_import,
        );
    });
    heading_help(ui, t, crate::i18n::tr("Startup"), "startup");
    row(ui, t, crate::i18n::tr("Open in"), |ui| {
        choices(
            ui,
            "settingsStartup",
            &[
                (StartupView::Last, "Last view"),
                (StartupView::Grid, "Photo Grid"),
                (StartupView::SquareGrid, "Square Grid"),
                (StartupView::Detail, "Detail View"),
            ],
            &mut app.ui.settings.startup_view,
        );
    });
    // the updater is native only (the web build updates by reloading the page); saved through
    // `update.prefs`, like the update dialog's own checkbox
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut check_at_startup = app.session.updater.prefs().check_at_startup;
        if check(ui, "settings.updateAtStartup", &mut check_at_startup, "Check for updates when the app starts")
            && let Err(e) = app.session.execute("update.prefs", &json!({"checkAtStartup": check_at_startup}))
        {
            // not saved: the box shows the stored value again next frame
            app.toast_error(ui.ctx(), format!("{}: {e}", crate::i18n::tr("Update settings not saved")));
        }
    }
    heading_help(ui, t, crate::i18n::tr("Culling"), "culling");
    check(ui, "settings.autoAdvance", &mut app.ui.auto_advance, "Auto Advance: move to the next photo after rating or flagging");
    check(ui, "settings.confirmDelete", &mut app.ui.settings.confirm_delete, "Confirm before moving photos to Recently Deleted");
    check(ui, "settings.askBeforeMove", &mut app.ui.settings.ask_before_move, "Confirm before moving photos to another folder on disk");
    heading(ui, t, crate::i18n::tr("Dialogs"));
    check(ui, "settings.closeDialogsOutside", &mut app.ui.settings.close_dialogs_outside, "Close a dialog by clicking outside it");
    heading_help(ui, t, crate::i18n::tr("External Editing"), "externalEditing");
    // Photo ▸ Edit In (#616): the window that asks which file to edit, or the remembered choice
    row(ui, t, crate::i18n::tr("Edit In"), |ui| {
        use crate::state::EditInMode;
        choices(
            ui,
            "settingsEditInMode",
            &[
                (EditInMode::Ask, "Ask"),
                (EditInMode::CopyWithAdjustments, "Edit a Copy with Adjustments"),
                (EditInMode::Copy, "Edit a Copy"),
                (EditInMode::Original, "Edit Original"),
            ],
            &mut app.ui.settings.edit_in_mode,
        );
    });
    row(ui, t, crate::i18n::tr("PhotoCraft"), |ui| {
        crate::text_field::TextField::singleline("field:photocraftPath", &mut app.ui.settings.photocraft_path)
            .hint(crate::i18n::tr("Found automatically"))
            .width(220.0)
            .show(ui);
    });
    edit_file_rows(ui, t, "photocraftFile", &mut app.ui.settings.photocraft_file);
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photo ▸ Edit In ▸ Edit in PhotoCraft (⇧⌘E, or right-click a photo) renders a copy with your edits, stacks it with the original and opens it in PhotoCraft. Save there and the copy updates here. As PSD, PhotoCraft saves straight back to it. Leave the location empty unless you use a portable copy.",
        ),
    );
    row(ui, t, crate::i18n::tr("Other editor"), |ui| {
        crate::text_field::TextField::singleline("field:externalEditor", &mut app.ui.settings.external_editor)
            .hint(crate::i18n::tr("System default"))
            .width(220.0)
            .show(ui);
    });
    edit_file_rows(ui, t, "externalFile", &mut app.ui.settings.external_file);
    hint(ui, t, crate::i18n::tr("Edit in Other Editor (⌥⌘E) opens the copy in this application (an app name on macOS, a program path elsewhere)."));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "To hand this editor the original file itself instead (the raw, without a copy: DxO PhotoLab, for example), right-click a photo → Edit In → Open Original in Other Editor (RAW, no copy).",
        ),
    );
}

/// A language's name as the picker shows it: its own name (`Deutsch`), or, when no loaded font
/// has that script's glyphs (Georgian has no face in this build's fonts; Chinese and Japanese in a
/// build without craft-fonts), its English name, so it never draws as empty boxes.
fn language_label(ctx: &egui::Context, language: crate::i18n::Locale) -> String {
    let own = language.name();
    let font = egui::FontId::proportional(13.0);
    if ctx.fonts_mut(|f| f.has_glyphs(&font, own)) {
        return own.to_string();
    }
    let english = match language.code() {
        "zh-hans" => "Chinese (Simplified)",
        "zh-hant" => "Chinese (Traditional, Taiwan)",
        "ja" => "Japanese",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "ka" => "Georgian",
        "pt-br" => "Portuguese (Brazil)",
        "es" => "Spanish",
        "de" => "German",
        "fr" => "French",
        "pl" => "Polish",
        "cs" => "Czech",
        "it" => "Italian",
        _ => language.code(),
    };
    english.to_string()
}

/// The file Edit In hands an editor: format, colour space and bit depth (`button:{id}Format-…`).
fn edit_file_rows(ui: &mut egui::Ui, t: &Tokens, id: &str, file: &mut crate::state::EditFile) {
    use crate::state::{EditFormat, EditSpace};
    row(ui, t, "", |ui| {
        choices(ui, &format!("{id}Format"), &[(EditFormat::Psd, "PSD"), (EditFormat::Tiff, "TIFF")], &mut file.format);
        ui.separator();
        choices(ui, &format!("{id}Depth"), &[(16u8, "16-bit"), (8, "8-bit")], &mut file.bit_depth);
    });
    row(ui, t, "", |ui| {
        choices(
            ui,
            &format!("{id}Space"),
            &[(EditSpace::AdobeRgb, "Adobe RGB"), (EditSpace::ProPhoto, "ProPhoto"), (EditSpace::DisplayP3, "P3"), (EditSpace::Srgb, "sRGB")],
            &mut file.color_space,
        );
    });
}

// -------------------------------------------------------------------------------------- Import

/// A preset picker: `None` = `none_label`. Returns the new choice when it changed.
fn preset_combo(app: &LccApp, ui: &mut egui::Ui, id: &str, current: Option<&str>, none_label: &str) -> Option<Option<String>> {
    let name = |pid: &str| {
        app.session
            .presets
            .iter()
            .find(|p| p.id == pid)
            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
            .unwrap_or_else(|| crate::i18n::tr_format!("{pid} (missing)", pid = pid))
    };
    let text = current.map(name).unwrap_or_else(|| crate::i18n::tr(none_label).to_string());
    let mut out = None;
    let r = crate::widgets::combo_box(id).width(240.0).selected_text(text).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(
        ui,
        |ui| {
            if ui.selectable_label(current.is_none(), crate::i18n::tr(none_label)).clicked() {
                out = Some(None);
                ui.close();
            }
            if let Some(pid) = crate::panels::presets::picker_rows(ui, id, &app.session, current) {
                out = Some(Some(pid));
            }
        },
    );
    register(ui.ctx(), format!("combo:{id}"), r.response.rect);
    out.filter(|v| v.as_deref() != current)
}

/// Cameras of the raws in the library plus those with a stored default, sorted.
fn cameras(app: &LccApp) -> Vec<String> {
    let mut v: Vec<String> = app
        .session
        .catalog
        .photos()
        .filter(|p| p.kind == lcc_catalog::MediaKind::Raw && !p.meta.camera.is_empty())
        .map(|p| p.meta.camera.clone())
        .chain(app.session.import_defaults.cameras.iter().map(|c| c.camera.clone()))
        .collect();
    v.sort_by_key(|c| c.to_lowercase());
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    v
}

fn import_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let d = app.session.import_defaults.clone();
    // (browsing folders needs a file system: the web keeps the import review)
    if !cfg!(target_arch = "wasm32") {
        heading_help(ui, t, crate::i18n::tr("Import Window"), "importWindow");
        check(ui, "settings.importAsTab", &mut app.ui.settings.import_as_tab, "Import as a tab");
        hint(
            ui,
            t,
            crate::i18n::tr(
                "On: Import is a tab beside Library, where a card or folder is culled before its picks are imported. Off: Import… opens the import review.",
            ),
        );
    }
    heading_help(ui, t, crate::i18n::tr("Raw defaults"), "rawDefaults");
    hint(ui, t, crate::i18n::tr("Settings new raw photos start from. Changing them doesn't touch photos already in the catalog."));
    row(ui, t, crate::i18n::tr("Raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsRawPreset", d.raw_preset.as_deref(), "{app} Default") {
            let _ = app.run("library.preferences", json!({"import": {"rawPreset": v}}));
        }
    });
    let mut per = d.per_camera;
    row(ui, t, "", |ui| {
        if check(ui, "settings.perCamera", &mut per, "Use camera-specific defaults") {
            let _ = app.run("library.preferences", json!({"import": {"perCamera": per}}));
        }
    });
    // AI Denoise for the high-ISO photos of a shoot, switched on as they come in
    let mut iso = d.denoise_above_iso.unwrap_or(0);
    row(ui, t, crate::i18n::tr("AI Denoise from"), |ui| {
        let off = crate::i18n::tr("Off");
        let opts = [(0u32, off), (800, "ISO 800"), (1600, "ISO 1600"), (3200, "ISO 3200"), (6400, "ISO 6400"), (12_800, "ISO 12800")];
        // an ISO set by an agent that isn't offered shows as its own choice
        let other = (!opts.iter().any(|(v, _)| *v == iso)).then(|| format!("ISO {iso}"));
        let mut all: Vec<(u32, &str)> = opts.to_vec();
        if let Some(o) = &other {
            all.push((iso, o.as_str()));
        }
        if choices(ui, "settingsDenoiseIso", &all, &mut iso) {
            let _ = app.run("library.preferences", json!({"import": {"denoiseAboveIso": iso}}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "New raw photos shot at this ISO or above start with AI Denoise on, and their denoised pictures are made in the background after the import. Needs a denoise model (Settings, AI Denoise).",
        ),
    );
    let mut dust = d.remove_dust;
    row(ui, t, "", |ui| {
        if check(ui, "settings.removeDust", &mut dust, "Remove dust on import") {
            let _ = app.run("library.preferences", json!({"import": {"removeDust": dust}}));
        }
    });
    hint(ui, t, crate::i18n::tr("After an import, Remove Dust runs on the new raw photos in the background (their camera's dust map included)."));
    let mut constrain = !d.crop_unconstrained;
    row(ui, t, "", |ui| {
        if check(ui, "settings.constrainCrop", &mut constrain, "Constrain crop by default") {
            let _ = app.run("library.preferences", json!({"import": {"constrainCrop": constrain}}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "New photos start with Constrain Crop on: the crop stays inside the photo after a rotation or lens correction. Photos already in the catalog keep theirs.",
        ),
    );
    if per {
        let cams = cameras(app);
        if cams.is_empty() {
            hint(ui, t, crate::i18n::tr("No raw photos yet: cameras appear here once their photos are in the catalog."));
        }
        egui::ScrollArea::vertical().max_height(150.0).id_salt("settingsCameras").show(ui, |ui| {
            for (i, cam) in cams.iter().enumerate() {
                let entry = d.cameras.iter().find(|c| c.camera.eq_ignore_ascii_case(cam));
                row(ui, t, cam, |ui| {
                    // "Raw default" = no entry; otherwise the entry's preset (None = the app default)
                    const RAW_DEFAULT: &str = "\u{1}raw";
                    let current = match entry {
                        None => Some(RAW_DEFAULT),
                        Some(e) => e.preset.as_deref(),
                    };
                    let mut pick = None;
                    let label = match current {
                        Some(RAW_DEFAULT) => crate::i18n::tr("Same as raw default").to_string(),
                        None => crate::i18n::tr("{app} Default").to_string(),
                        Some(pid) => app
                            .session
                            .presets
                            .iter()
                            .find(|p| p.id == pid)
                            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
                            .unwrap_or_else(|| pid.to_string()),
                    };
                    let id = format!("settingsCamera-{i}");
                    let r = crate::widgets::combo_box(&id)
                        .width(240.0)
                        .selected_text(label)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(current == Some(RAW_DEFAULT), crate::i18n::tr("Same as raw default")).clicked() {
                                pick = Some(json!({"camera": cam, "remove": true}));
                                ui.close();
                            }
                            if ui.selectable_label(current.is_none(), crate::i18n::tr("{app} Default")).clicked() {
                                pick = Some(json!({"camera": cam, "preset": null}));
                                ui.close();
                            }
                            if let Some(pid) = crate::panels::presets::picker_rows(ui, &id, &app.session, current) {
                                pick = Some(json!({"camera": cam, "preset": pid}));
                            }
                        });
                    register(ui.ctx(), format!("combo:{id}"), r.response.rect);
                    if let Some(c) = pick {
                        let _ = app.run("library.preferences", json!({"camera": c}));
                    }
                });
            }
        });
    }
    // the default camera profile (issue #723): what photos start from when no camera profile is
    // set on them, per camera or for every camera
    heading_help(ui, t, crate::i18n::tr("Camera profile"), "cameraProfile");
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The camera profile photos start from when none is set on them. Without a choice, a camera's own imported profile stays the default; Built-in colour keeps photos on {app}'s own colour even with imported profiles installed.",
        ),
    );
    let defaults = app.session.camera_profile_defaults.clone();
    let profile_options = |camera: &str| -> Vec<(String, String)> {
        let mut out = vec![("lc".to_string(), crate::i18n::tr("Built-in colour").to_string())];
        for (pid, p) in lcc_raw::dcp::library() {
            if camera.is_empty() || lcc_raw::dcp::covers_model(&p.model, camera) {
                let name = if p.name.is_empty() { pid.trim_start_matches("dcp:").to_string() } else { p.name.clone() };
                out.push((pid, name));
            }
        }
        if let Some(cal) = if camera.is_empty() { None } else { lcc_raw::dcp::calibrated_for_camera(camera) } {
            out.extend(cal.looks.iter().map(|(pid, p)| (pid.clone(), p.name.clone())));
        }
        out
    };
    let camera_default_combo = |ui: &mut egui::Ui, id: String, camera: String, current: Option<&String>| -> Option<Option<String>> {
        let none_label = crate::i18n::tr("Camera default (imported profile)").to_string();
        let shown = match current {
            None => none_label.clone(),
            Some(c) if c == "lc" => crate::i18n::tr("Built-in colour").to_string(),
            Some(c) => profile_options(&camera).iter().find(|(o, _)| o == c).map(|(_, n)| n.clone()).unwrap_or_else(|| c.clone()),
        };
        let mut pick: Option<Option<String>> = None;
        let r = crate::widgets::combo_box(&id).width(240.0).selected_text(shown).show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), &none_label).clicked() {
                pick = Some(None);
            }
            for (pid, name) in profile_options(&camera) {
                if ui.selectable_label(current == Some(&pid), &name).clicked() {
                    pick = Some(Some(pid));
                }
            }
        });
        register(ui.ctx(), format!("combo:{id}"), r.response.rect);
        pick
    };
    row(ui, t, crate::i18n::tr("All cameras"), |ui| {
        let current = defaults.get("");
        if let Some(pick) = camera_default_combo(ui, "settingsCameraDefaultGlobal".into(), String::new(), current) {
            let _ = app.run("library.preferences", json!({"cameraDefault": {"camera": "*", "profile": pick}}));
        }
    });
    let cams = cameras(app);
    if !cams.is_empty() {
        egui::ScrollArea::vertical().max_height(150.0).id_salt("settingsCameraProfiles").show(ui, |ui| {
            for (i, cam) in cams.iter().enumerate() {
                let current = defaults.iter().find(|(k, _)| !k.is_empty() && lcc_raw::dcp::covers_model(k, cam)).map(|(_, v)| v);
                row(ui, t, cam, |ui| {
                    if let Some(pick) = camera_default_combo(ui, format!("settingsCameraDefault-{i}"), cam.clone(), current) {
                        let _ = app.run("library.preferences", json!({"cameraDefault": {"camera": cam, "profile": pick}}));
                    }
                });
            }
        });
    }
    heading(ui, t, crate::i18n::tr("Other images (JPEG, PNG, TIFF, HEIC…)"));
    row(ui, t, crate::i18n::tr("Non-raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsOtherPreset", d.other_preset.as_deref(), "None") {
            let _ = app.run("library.preferences", json!({"import": {"otherPreset": v}}));
        }
    });
    heading_help(ui, t, crate::i18n::tr("Metadata"), "importMetadata");
    hint(ui, t, crate::i18n::tr("Added to photos you import that don't already have it."));
    for (key, label, hint_text, value) in
        [("copyright", "Copyright", "© 2026 Your Name", d.copyright.clone()), ("creator", "Creator", "Your Name", d.creator.clone())]
    {
        row(ui, t, label, |ui| {
            let id = egui::Id::new(("settingsMeta", key));
            let mut text: String = ui.data(|m| m.get_temp(id)).unwrap_or(value.clone());
            let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(crate::i18n::tr(hint_text)).desired_width(240.0));
            register(ui.ctx(), format!("field:settings.{key}"), r.rect);
            if r.lost_focus() && text.trim() != value {
                let _ = app.run("library.preferences", json!({"import": {key: text.trim()}}));
            }
            if r.has_focus() {
                ui.data_mut(|m| m.insert_temp(id, text));
            } else {
                ui.data_mut(|m| m.remove::<String>(id));
            }
        });
    }
    row(ui, t, crate::i18n::tr("Metadata preset"), |ui| {
        let cur = d.metadata_preset.clone();
        let mut pick = None;
        let r = crate::widgets::combo_box("settingsMetaPreset")
            .width(240.0)
            .selected_text(cur.clone().unwrap_or_else(|| crate::i18n::tr("None").into()))
            .show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_none(), crate::i18n::tr("None")).clicked() {
                    pick = Some(String::new());
                }
                for m in &app.session.metadata_presets {
                    if ui.selectable_label(cur.as_deref() == Some(m.name.as_str()), &m.name).clicked() {
                        pick = Some(m.name.clone());
                    }
                }
                ui.separator();
                if ui.button(crate::i18n::tr("Import Presets…")).clicked() {
                    let _ = app.run("file.importPresets", json!({}));
                }
            });
        register(ui.ctx(), "combo:settingsMetaPreset", r.response.rect);
        if let Some(n) = pick {
            let _ = app.run("library.preferences", json!({"import": {"metadataPreset": n}}));
        }
    });
    heading_help(ui, t, crate::i18n::tr("XMP sidecars"), "xmpSidecars");
    let mut xmp = app.session.xmp;
    if check(ui, "settings.autoWriteXmp", &mut xmp.auto_write, "Automatically write changes into XMP sidecars") {
        let _ = app.run("library.xmpPreferences", json!({"autoWrite": xmp.auto_write}));
    }
    if check(ui, "settings.autoReadXmp", &mut xmp.auto_read, "Automatically read XMP sidecars other apps changed (photos unchanged here)") {
        let _ = app.run("library.xmpPreferences", json!({"autoRead": xmp.auto_read}));
    }
    row(ui, t, crate::i18n::tr("Sidecar names"), |ui| {
        use lcc_engine::sidecar::SidecarNaming as N;
        let mut n = xmp.naming;
        if choices(ui, "settingsXmpNaming", &[(N::Stem, "IMG_1.xmp"), (N::Full, "IMG_1.CR3.xmp")], &mut n) {
            let naming = if n == N::Full { "full" } else { "stem" };
            let _ = app.run("library.xmpPreferences", json!({"naming": naming}));
        }
    });
    if !cfg!(target_arch = "wasm32") {
        // issue #123: off by default, as listing folders costs something on a VPN or a slow link
        heading_help(ui, t, crate::i18n::tr("Folder Polling"), "folderPolling");
        hint(
            ui,
            t,
            crate::i18n::tr(
                "Look for photos other apps added to the catalog's folders when the catalog opens, when the app comes to the front and every few minutes. Only folders whose listing changed are scanned.",
            ),
        );
        let mut w = app.session.folder_watch;
        if check(ui, "settings.folderPoll", &mut w.poll, "Check folders for new photos") {
            let _ = app.run("folder.watchPreferences", json!({"poll": w.poll}));
        }
        row(ui, t, crate::i18n::tr("Every"), |ui| {
            let mut m = w.interval_minutes;
            if choices(ui, "settingsFolderPollInterval", &[(1, "1 min"), (5, "5 min"), (15, "15 min"), (60, "1 hour")], &mut m) {
                let _ = app.run("folder.watchPreferences", json!({"intervalMinutes": m}));
            }
        });
        row(ui, t, crate::i18n::tr("New photos"), |ui| {
            use lcc_engine::folder_watch::NewFilePolicy as P;
            let mut p = w.new_files;
            if choices(
                ui,
                "settingsFolderNewFiles",
                &[(P::Ask, crate::i18n::tr("Ask (show “N new”)")), (P::Add, crate::i18n::tr("Add automatically"))],
                &mut p,
            ) {
                let _ = app.run("folder.watchPreferences", json!({"newFiles": p.as_str()}));
            }
        });
        heading(ui, t, crate::i18n::tr("Drag and Drop"));
        check(ui, "settings.importDropped", &mut app.ui.settings.import_dropped_directly, "Import dropped photos without review");
        hint(ui, t, crate::i18n::tr("Off: photos and folders dropped on the window open the import review first."));
        heading(ui, t, crate::i18n::tr("Cameras and Cards"));
        let mut on_card = d.show_on_card;
        if check(ui, "settings.importOnCard", &mut on_card, "Show the import review when a camera or card is connected") {
            let _ = app.run("library.preferences", json!({"import": {"showOnCard": on_card}}));
        }
        let r = ui.scope(|ui| heading_help(ui, t, crate::i18n::tr("Auto Import"), "autoImport")).response;
        register(ui.ctx(), "section:autoImport", r.rect);
        if app.ui.settings_section.as_deref() == Some("autoImport") {
            r.scroll_to_me(Some(egui::Align::TOP));
            app.ui.settings_section = None;
        }
        hint(ui, t, crate::i18n::tr("Photos that arrive in this folder (tethering, a scanner, a sync app) are added as soon as they're complete."));
        if d.auto_folder.is_some() {
            let mut on = !d.auto_paused;
            if check(ui, "settings.autoImportEnabled", &mut on, "Enable Auto Import") {
                let _ = app.run("library.autoImport", json!({"enabled": on}));
            }
        }
        row(ui, t, crate::i18n::tr("Watched folder"), |ui| {
            match &d.auto_folder {
                Some(folder) => path_label(
                    ui,
                    "settingsAutoFolder",
                    folder,
                    egui::TextStyle::Body.resolve(ui.style()),
                    t.text,
                    button_width(ui, crate::i18n::tr("Choose…")) + button_width(ui, crate::i18n::tr("Turn Off")),
                ),
                None => drop(ui.label(RichText::new(crate::i18n::tr("Off")).color(t.text))),
            }
            let can = app.services.pick_folder.is_some();
            let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose…")));
            register(ui.ctx(), "button:settingsAutoFolder", r.rect);
            if r.clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
                && let Err(e) = app.run("library.autoImport", json!({"folder": f}))
            {
                app.toast(ui.ctx(), e);
            }
            if d.auto_folder.is_some() && ui.button(crate::i18n::tr("Turn Off")).clicked() {
                let _ = app.run("library.autoImport", json!({"folder": null}));
            }
        });
        // the last listing of the watched folder failed: say why (it reads "path: reason")
        if let (Some(folder), Some((failed, e))) = (&d.auto_folder, &app.ui.auto_import_error)
            && folder == failed
        {
            let reason = e.strip_prefix(folder.as_str()).and_then(|r| r.strip_prefix(": ")).unwrap_or(e);
            row(ui, t, "", |ui| {
                ui.label(RichText::new(crate::i18n::tr_format!("Can't read this folder: {reason}", reason = reason)).size(11.0).color(t.caution));
            });
        }
        if d.auto_folder.is_some() {
            row(ui, t, "", |ui| {
                let mut copy = d.auto_copy;
                if ui.checkbox(&mut copy, crate::i18n::tr("Copy into the catalog (else use the files where they are)")).changed() {
                    let _ = app.run("library.autoImport", json!({"copy": copy}));
                }
            });
            row(ui, t, crate::i18n::tr("Collection"), |ui| {
                let id = egui::Id::new("auto-album");
                let mut name = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_else(|| d.auto_album.clone().unwrap_or_default());
                let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(crate::i18n::tr("None")).desired_width(180.0));
                if r.lost_focus() {
                    let _ = app.run("library.autoImport", json!({"album": name.trim()}));
                }
                ui.data_mut(|m| m.insert_temp(id, name));
            });
            let o = d.auto_options.clone();
            row(ui, t, "", |ui| {
                let mut sub = o.subfolders;
                if ui.checkbox(&mut sub, crate::i18n::tr("Include subfolders")).changed() {
                    let _ = app.run("library.autoImport", json!({"subfolders": sub}));
                }
            });
            hint(ui, t, crate::i18n::tr("Added to every photo that arrives. The file naming applies to copies; separate keywords with commas."));
            row(ui, t, crate::i18n::tr("File naming"), |ui| {
                let id = egui::Id::new("auto-rename");
                let mut text = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_else(|| o.rename.clone());
                let r = ui
                    .add(egui::TextEdit::singleline(&mut text).hint_text(crate::i18n::tr("keep names, or e.g. {date}_{seq:3}")).desired_width(240.0));
                register(ui.ctx(), "field:settings.autoImportRename", r.rect);
                if r.lost_focus() {
                    let _ = app.run("library.autoImport", json!({"rename": text.trim()}));
                }
                ui.data_mut(|m| m.insert_temp(id, text));
            });
            row(ui, t, crate::i18n::tr("Develop preset"), |ui| {
                if let Some(v) = preset_combo(app, ui, "settingsAutoPreset", o.preset.as_deref(), "None") {
                    let _ = app.run("library.autoImport", json!({"preset": v}));
                }
            });
            row(ui, t, crate::i18n::tr("Metadata preset"), |ui| {
                let cur = o.metadata_preset.clone();
                let mut pick = None;
                let r = crate::widgets::combo_box("settingsAutoMetaPreset")
                    .width(240.0)
                    .selected_text(cur.clone().unwrap_or_else(|| crate::i18n::tr("None").into()))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(cur.is_none(), crate::i18n::tr("None")).clicked() {
                            pick = Some(String::new());
                        }
                        for m in &app.session.metadata_presets {
                            if ui.selectable_label(cur.as_deref() == Some(m.name.as_str()), &m.name).clicked() {
                                pick = Some(m.name.clone());
                            }
                        }
                    });
                register(ui.ctx(), "combo:settingsAutoMetaPreset", r.response.rect);
                if let Some(n) = pick {
                    let _ = app.run("library.autoImport", json!({"metadataPreset": n}));
                }
            });
            row(ui, t, crate::i18n::tr("Keywords"), |ui| {
                let id = egui::Id::new("auto-keywords");
                let mut text = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_else(|| o.keywords.join(", "));
                let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(crate::i18n::tr("None")).desired_width(240.0));
                register(ui.ctx(), "field:settings.autoImportKeywords", r.rect);
                if r.lost_focus() {
                    let _ = app.run("library.autoImport", json!({"keywords": text.trim()}));
                }
                ui.data_mut(|m| m.insert_temp(id, text));
            });
        }
    }
    if app.session.library.is_none() {
        hint(ui, t, crate::i18n::tr("In-memory session: these settings last until {app} quits."));
    }
    lens_profiles(app, ui, t);
}

// ------------------------------------------------------------------------------ Lens profiles

/// Settings ▸ Import ▸ Lens profiles: the lens database lens corrections fall back to (an app setting, not a
/// library one), a folder chooser, and downloading lensfun's database on request (`lens.db.*`).
fn lens_profiles(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::i18n::tr;
    let r = ui.scope(|ui| heading_help(ui, t, tr("Lens profiles"), "lensProfiles")).response;
    register(ui.ctx(), "section:lensProfiles", r.rect);
    if app.ui.settings_section.as_deref() == Some("lensProfiles") {
        r.scroll_to_me(Some(egui::Align::TOP));
        app.ui.settings_section = None;
    }
    hint(ui, t, tr("Lens corrections use these profiles for raw photos whose files carry no correction of their own."));
    let st = app.session.lens_db.status();
    let lenses = st["lenses"].as_u64().unwrap_or(0);
    let dir = st["dir"].as_str().map(str::to_string);
    row(ui, t, tr("Database"), |ui| {
        let text = match &dir {
            Some(d) if lenses > 0 => d.clone(),
            Some(d) => format!("{d} ({})", tr("no lens profiles found")),
            None => tr("None").to_string(),
        };
        // a long path is shortened (in full on hover), so it never widens the dialog
        let label = egui::Label::new(RichText::new(&text).color(if lenses > 0 { t.text } else { t.text_dim })).truncate();
        let l = ui.allocate_ui(egui::vec2(380.0, 20.0), |ui| ui.add(label)).inner.on_hover_text(&text);
        register(ui.ctx(), "label:lensDbStatus", l.rect);
    });
    if lenses > 0 {
        let summary = tr("{lenses} lenses, {cameras} cameras")
            .replace("{lenses}", &lenses.to_string())
            .replace("{cameras}", &st["cameras"].as_u64().unwrap_or(0).to_string());
        let updated = st["updated"].as_str().map(|d| tr("updated {date}").replace("{date}", d));
        row(ui, t, "", |ui| ui.label(RichText::new(updated.map_or(summary.clone(), |u| format!("{summary} · {u}"))).color(t.text_label)));
    }
    if st["origin"] == "env" {
        hint(ui, t, &tr("Set by {var}, which overrides the folder chosen here.").replace("{var}", st["env"].as_str().unwrap_or_default()));
    }
    let running = st["download"]["state"] == "running";
    let configured = st["configured"].is_string();
    row(ui, t, "", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if crate::widgets::text_button(ui, "lensDbFolder", "Choose Folder…", false).clicked()
            && let Err(e) = app.run("app.lensDbFolder", json!({}))
        {
            app.toast(ui.ctx(), e);
        }
        if st["canDownload"] == true {
            let label = if st["downloaded"] == true { "Update" } else { "Download" };
            let b = ui.add_enabled_ui(!running, |ui| crate::widgets::text_button(ui, "lensDbDownload", label, false)).inner;
            if b.clicked()
                && let Err(e) = app.run("lens.db.download", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
        }
        if running && crate::widgets::text_button(ui, "lensDbCancel", "Cancel", false).clicked() {
            let _ = app.run("lens.db.downloadCancel", json!({}));
        }
        if configured
            && !running
            && crate::widgets::text_button(ui, "lensDbRemove", "Remove", false).clicked()
            && let Err(e) = app.run("lens.db.remove", json!({}))
        {
            app.toast(ui.ctx(), e);
        }
    });
    match st["download"]["state"].as_str() {
        Some("running") => {
            // progress and Cancel are in the activity stack; this only repaints so the buttons update when it ends
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        Some("failed") => {
            let e = st["download"]["error"].as_str().unwrap_or_default().to_string();
            row(ui, t, "", |ui| ui.label(RichText::new(e).color(t.caution)));
        }
        _ => {}
    }
    hint(
        ui,
        t,
        tr("Download fetches the lensfun project's database (lensfun.github.io, CC-BY-SA 3.0) into the settings folder. It is not part of {app}."),
    );
}

/// Choose the lens database folder (`{dir}`, else a folder dialog), then use it (`lens.db.setDir`).
pub fn lens_db_folder(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let dir = match p.get("dir").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Choose Lens Database Folder"));
            match crate::pick::ask(app, "app.lensDbFolder", p, "dir", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(d) => d,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog here: pass {dir}".into()),
            }
        }
    };
    app.run("lens.db.setDir", json!({"dir": dir}))
}

// --------------------------------------------------------------------------------- Performance

/// Keep recent zoomed HDR views: offered only where the GPU has room for them on this screen;
/// elsewhere greyed out, with why (an HDR display only: SDR windows keep no HDR textures).
fn hdr_zoom_cache(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    const LABEL: &str = "Keep recent zoomed HDR views in graphics memory";
    let Some(fit) = app.hdr_cache_fit(ui.ctx()) else { return };
    let mb = |b: u64| b.div_ceil(1 << 20);
    if fit.fits() {
        check(ui, "settings.hdrZoomCache", &mut app.ui.settings.hdr_zoom_cache, LABEL);
        hint(
            ui,
            t,
            &crate::i18n::tr_format!(
                "Panning back or zooming in again on an HDR edit shows it at once instead of rendering it again. Uses up to {need} MB of graphics memory on this screen.",
                need = mb(fit.need)
            ),
        );
        return;
    }
    let mut off = false;
    ui.add_enabled_ui(false, |ui| check(ui, "settings.hdrZoomCache", &mut off, LABEL));
    let why = crate::i18n::tr_format!(
        "Not available on this computer: keeping them needs about {need} MB of graphics memory on this screen, and the graphics card has {have} MB. The app uses at most a quarter of it for them, so the rest stays free for the photo itself and other apps. Zoomed HDR views still work; going back to one renders it again.",
        need = mb(fit.need),
        have = fit.have.map_or(0, mb)
    );
    crate::dialog_frame::in_control_column(ui, |ui| {
        ui.label(RichText::new(why).size(11.0).color(t.caution));
    });
}

/// The graphics interface GPU rendering uses: a choice on Windows only (DirectX 12 by default,
/// whose shader compiler can make the start slow on some machines; Vulkan there is opt-in, issue
/// #136). Elsewhere there is one sensible interface.
fn gpu_backend_row(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::state::GpuBackend;
    if !cfg!(target_os = "windows") {
        return;
    }
    let opts = [(GpuBackend::Auto, crate::i18n::tr("Automatic")), (GpuBackend::Dx12, "DirectX 12"), (GpuBackend::Vulkan, "Vulkan")];
    row(ui, t, crate::i18n::tr("Graphics interface"), |ui| {
        choices(ui, "settingsGpuBackend", &opts, &mut app.ui.settings.gpu_backend);
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Applies at next launch. If the app is slow to start with the GPU on, try Vulkan: it prepares the GPU much faster on some computers.",
        ),
    );
}

/// Settings ▸ Performance: the graphics card for editing and the second card (exports, previews,
/// thumbnails). The cards are those listed when the GPU started; a change applies at once.
#[cfg(not(target_arch = "wasm32"))]
fn gpu_card_rows(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use lcc_engine::gpu;
    let cards = gpu::adapters();
    if cards.is_empty() {
        return;
    }
    let label = |e: &gpu::cards::AdapterEntry| {
        let mut l = e.name.clone();
        if cards.iter().filter(|c| c.key.same_model(&e.key)).count() > 1 {
            l = format!("{l} #{}", e.key.index);
        }
        if e.drives_screen {
            l = crate::i18n::tr_format!("{card} (screen)", card = l);
        }
        l
    };
    let name_of = |setting: &str| cards.iter().find(|e| gpu::cards::AdapterKey::parse(setting).is_some_and(|k| k == e.key)).map(label);
    let auto = crate::i18n::tr("Automatic").to_string();
    row(ui, t, crate::i18n::tr("Graphics card for editing"), |ui| {
        let current = name_of(&app.ui.settings.gpu_card).unwrap_or_else(|| auto.clone());
        let combo = crate::widgets::combo_box("settingsGpuCard").selected_text(current).width(260.0).show_ui(ui, |ui| {
            let r = ui.selectable_value(&mut app.ui.settings.gpu_card, String::new(), auto.as_str());
            register(ui.ctx(), "button:settingsGpuCard-auto", r.rect);
            for (i, e) in cards.iter().enumerate() {
                let r = ui.add_enabled_ui(e.usable, |ui| ui.selectable_value(&mut app.ui.settings.gpu_card, e.key.to_string(), label(e))).inner;
                let r = match &e.why_not {
                    Some(why) => r.on_disabled_hover_text(why),
                    None => r,
                };
                register(ui.ctx(), format!("button:settingsGpuCard-{i}"), r.rect);
            }
        });
        register(ui.ctx(), "combo:settingsGpuCard", combo.response.rect);
    });
    // (another card than the editing one; Automatic picks one when there is one)
    let (_, editing, switching) = gpu::card_status();
    let others: Vec<_> = cards.iter().enumerate().filter(|(_, e)| e.usable && editing.as_deref() != Some(e.key.to_string().as_str())).collect();
    // whether the row shows depends only on the cards listed, not on which one is editing yet: the
    // editing card is known once its device is up, and a row that came and went then would move every
    // control below it while the GPU starts
    if cards.iter().filter(|e| e.usable).count() < 2 && app.ui.settings.gpu_helper.is_empty() {
        return;
    }
    row(ui, t, crate::i18n::tr("Second graphics card"), |ui| {
        let off = crate::i18n::tr("Off").to_string();
        // Automatic names the card it uses once that card is set up (short: the hint below says
        // what it does, so the choice fits a small Settings window)
        let auto_now = match gpu::helper_status() {
            ("ready", Some(name)) => format!("{auto} ({name})"),
            _ => auto.clone(),
        };
        let current = match app.ui.settings.gpu_helper.as_str() {
            "" => auto_now.clone(),
            "off" => off.clone(),
            k => name_of(k).unwrap_or_else(|| auto.clone()),
        };
        let combo = crate::widgets::combo_box("settingsGpuHelper").selected_text(current).width(260.0).show_ui(ui, |ui| {
            let r = ui.selectable_value(&mut app.ui.settings.gpu_helper, String::new(), auto_now.as_str());
            register(ui.ctx(), "button:settingsGpuHelper-auto", r.rect);
            let r = ui.selectable_value(&mut app.ui.settings.gpu_helper, "off".to_string(), off.as_str());
            register(ui.ctx(), "button:settingsGpuHelper-off", r.rect);
            for (i, e) in &others {
                let r = ui.selectable_value(&mut app.ui.settings.gpu_helper, e.key.to_string(), label(e));
                register(ui.ctx(), format!("button:settingsGpuHelper-{i}"), r.rect);
            }
        });
        register(ui.ctx(), "combo:settingsGpuHelper", combo.response.rect);
    });
    let status = match gpu::helper_status() {
        _ if switching => crate::i18n::tr("Setting up the chosen graphics card…").to_string(),
        ("ready", Some(name)) => crate::i18n::tr_format!("Exports, previews and thumbnails also use {card}.", card = name),
        ("starting", _) => crate::i18n::tr("Second graphics card: starting…").to_string(),
        ("failed" | "lost", Some(why)) => crate::i18n::tr_format!("Second graphics card not used: {why}", why = why),
        _ => crate::i18n::tr("Exports, previews and thumbnails use the graphics card for editing.").to_string(),
    };
    crate::dialog_frame::in_control_column(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(&status).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&status);
    });
}

/// Settings ▸ Performance ▸ Exports: which graphics cards exports use, shown when the computer has
/// a second one (or the choice isn't the default): both, the card for editing only, or the second
/// card only (to measure its speed).
#[cfg(not(target_arch = "wasm32"))]
fn export_cards_row(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use lcc_engine::export::ExportCards;
    let two = lcc_engine::gpu::adapters().iter().filter(|e| e.usable).count() >= 2;
    let st = &mut app.ui.settings;
    if st.export_cpu_only || (!two && st.export_cards == ExportCards::Both) {
        return;
    }
    let opts = [
        (ExportCards::Both, crate::i18n::tr("Both cards")),
        (ExportCards::Editing, crate::i18n::tr("Card for editing only")),
        (ExportCards::Second, crate::i18n::tr("Second card only")),
    ];
    row(ui, t, crate::i18n::tr("Graphics cards for exports"), |ui| {
        choices(ui, "settingsExportCards", &opts, &mut st.export_cards);
    });
    let why = match st.export_cards {
        ExportCards::Both => "Each photo goes to the card that will finish it sooner.",
        ExportCards::Editing => "The second graphics card is left free for previews and thumbnails.",
        ExportCards::Second => "To measure the second card's speed. If it stops, the card for editing takes over.",
    };
    hint(ui, t, crate::i18n::tr(why));
}

/// The largest thumbnail cache Custom… offers (1 TB).
const MAX_CACHE_GB: u32 = 1024;

/// The custom size being typed or dragged, in GB, until the edit is over (see [`performance_tab`]).
fn pending_gb_id() -> egui::Id {
    egui::Id::new("settings-cache-custom-gb")
}

/// Save a custom size still being typed when the Performance tab (or Settings) is left: its field, which would save
/// it, is no longer drawn.
pub(crate) fn save_pending_cache_size(app: &mut LccApp, ctx: &egui::Context) {
    if let Some(gb) = ctx.data_mut(|d| d.remove_temp::<u32>(pending_gb_id())) {
        let _ = app.run("library.preferences", json!({"cacheMb": gb.saturating_mul(1024)}));
    }
}

/// Exports: photos side by side, the GPU for exports, and "Test my machine", which measures both and
/// sets them (its last report below the button).
fn exports_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    heading_help(ui, t, crate::i18n::tr("Exports"), "exports");
    row(ui, t, crate::i18n::tr("Photos at once"), |ui| {
        // (what Automatic is here: what Test my machine found, else half the processor's threads, up to 12)
        let tested = app.ui.settings.machine_test.as_ref().map(|m| m.lanes).filter(|n| *n > 0);
        let auto = match tested {
            Some(n) => format!("{} ({n}, {})", crate::i18n::tr("Automatic"), crate::i18n::tr("tested")),
            None => {
                let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
                format!("{} ({})", crate::i18n::tr("Automatic"), (cores / 2).clamp(1, 12))
            }
        };
        let mut opts: Vec<(u32, String)> = vec![(0, auto)];
        let cur = app.ui.settings.export_lanes;
        for n in [2u32, 4, 6, 8, 12, 16, 24] {
            opts.push((n, n.to_string()));
        }
        if cur > 0 && !opts.iter().any(|(v, _)| *v == cur) {
            opts.push((cur, cur.to_string()));
            opts.sort_by_key(|(v, _)| *v);
        }
        let opts: Vec<(u32, &str)> = opts.iter().map(|(v, l)| (*v, l.as_str())).collect();
        choices(ui, "settingsExportLanes", &opts, &mut app.ui.settings.export_lanes);
    });
    let mut gpu_exports = !app.ui.settings.export_cpu_only;
    if check(ui, "settings.exportGpu", &mut gpu_exports, "Use the GPU for exports") {
        app.ui.settings.export_cpu_only = !gpu_exports;
    }
    #[cfg(not(target_arch = "wasm32"))]
    export_cards_row(app, ui, t);
    row(ui, t, crate::i18n::tr("Test my machine"), |ui| {
        let running = crate::machine_test::running(app);
        let label = if running { crate::i18n::tr("Testing…") } else { crate::i18n::tr("Test my machine") };
        let r = ui.add_enabled(!running, egui::Button::new(label));
        register(ui.ctx(), "button:settingsTestMachine", r.rect);
        if r.clicked()
            && let Err(e) = crate::machine_test::start(app)
        {
            app.toast_error(ui.ctx(), e);
        }
    });
    let text = match &app.ui.settings.machine_test {
        Some(m) => {
            let when = m.tested.get(..10).unwrap_or(&m.tested);
            format!("{} {when}: {}", crate::i18n::tr("Tested"), crate::i18n::tr(&m.summary))
        }
        None => crate::i18n::tr(
            "Exports a few of your photos (or the demo photos) for a few minutes at several settings, then uses the fastest. Run it again after changing the computer or its graphics driver.",
        )
        .to_string(),
    };
    hint(ui, t, &text);
}

fn performance_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use lcc_engine::gpu;
    heading_help(ui, t, crate::i18n::tr("Rendering"), "rendering");
    #[cfg(target_os = "windows")]
    check(ui, "settings.composeWindow", &mut app.ui.settings.compose_window, "Prevent G-SYNC/FreeSync flicker (applies at next launch)");
    check(ui, "settings.gpu", &mut app.ui.settings.gpu, "Use the GPU for rendering");
    gpu_backend_row(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    gpu_card_rows(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let ml_status = crate::panels::ml_models::cached_status(app, ui.ctx());
        crate::panels::ml_models::ml_device_selector(app, ui, t, &ml_status);
    }
    check(ui, "settings.hdrDisplay", &mut app.ui.settings.hdr_display, "HDR display where the screen supports it (applies at next launch)");
    // `available` waits for a device still being created: say so instead of blocking the frame
    let status = if gpu::enabled() && !gpu::ready() {
        "GPU: starting…".to_string()
    } else if !gpu::available() {
        match gpu::unavailable_reason() {
            Some(why) => crate::i18n::tr_format!("Rendering on the CPU: {why}", why = why),
            None => "No usable GPU found: rendering on the CPU".to_string(),
        }
    } else {
        format!("GPU: {}", gpu::adapter_name().unwrap_or_else(|| "starting…".into()))
    };
    // One line, the whole text on hover: the reason can be a long driver message (no adapter
    // among the backends …), and the GPU state is process-wide, so it may change between frames;
    // a hint that wrapped would move every control below it under the pointer.
    crate::dialog_frame::in_control_column(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(&status)).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&status);
    });
    hdr_zoom_cache(app, ui, t);
    row(ui, t, crate::i18n::tr("Preview size"), |ui| {
        let opts: Vec<(u32, String)> =
            PREVIEW_LIMITS.iter().map(|e| (*e, if *e == 0 { crate::i18n::tr("Automatic").to_string() } else { format!("{e} px") })).collect();
        let opts: Vec<(u32, &str)> = opts.iter().map(|(e, l)| (*e, l.as_str())).collect();
        choices(ui, "settingsPreview", &opts, &mut app.ui.settings.preview_limit);
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Automatic renders the Detail view at the size it is shown, up to the photo's own pixels. Choose a size to cap it: smaller is faster.",
        ),
    );
    check(ui, "settings.highQualityAdjustmentPreviews", &mut app.ui.settings.high_quality_adjustment_previews, "High quality adjustment previews");
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Keeps image quality consistent while adjusting. When an edit is too slow for that, the image updates faster while a slider moves and sharpens as soon as it stops.",
        ),
    );
    row(ui, t, crate::i18n::tr("Memory for caches"), |ui| {
        let opts = memory_choices(app.ui.settings.memory_mb, lcc_engine::memory::default_budget(), lcc_engine::memory::max_budget());
        let opts: Vec<(u32, &str)> = opts.iter().map(|(v, l)| (*v, l.as_str())).collect();
        choices(ui, "settingsMemory", &opts, &mut app.ui.settings.memory_mb);
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos and previews kept in memory, so the photos you go back to show at once. Automatic uses a quarter of this computer's memory, up to 16 GB. Choose more for big shoots, but leave some for other apps.",
        ),
    );
    exports_section(app, ui, t);
    heading_help(ui, t, crate::i18n::tr("Thumbnail cache"), "thumbnailCache");
    let cur = (app.session.cache_bytes() >> 20) as u32;
    // (0: Automatic, a share of the cache drive's free space)
    let chosen = app.session.cache_mb;
    // a size of the user's own, in whole GB: a large library outgrows 8 GB (#3252)
    let custom_id = egui::Id::new("settings-cache-custom");
    let mut custom = (chosen != 0 && !CACHE_SIZES.contains(&chosen)) || ui.data(|d| d.get_temp::<bool>(custom_id)).unwrap_or(false);
    row(ui, t, crate::i18n::tr("Size limit"), |ui| {
        let gb = app.session.automatic_cache_bytes().div_ceil(1 << 30);
        let auto = format!("{} ({gb} GB)", crate::i18n::tr("Automatic"));
        let opts = [(0u32, auto.as_str()), (512, "512 MB"), (1024, "1 GB"), (2048, "2 GB"), (4096, "4 GB"), (8192, "8 GB")];
        // none of them lit while a custom size is chosen
        let mut v = if custom { u32::MAX } else { chosen };
        if choices(ui, "settingsCache", &opts, &mut v) {
            custom = false;
            // a size typed earlier does not come back with Custom…
            ui.data_mut(|d| d.remove::<u32>(pending_gb_id()));
            let _ = app.run("library.preferences", json!({"cacheMb": v}));
        }
    });
    // a size of one's own, on its own line: the field keeps its unit beside it, also while it is typed in
    row(ui, t, "", |ui| {
        if crate::widgets::text_button(ui, "settingsCacheCustom", crate::i18n::tr("Custom…"), custom).clicked() {
            custom = true;
        }
        if custom {
            // the size being typed or dragged is held here, and saved once the edit is over: typing 16 must not set
            // 1 GB on the way
            let edit_id = pending_gb_id();
            let mut gb = ui.data(|d| d.get_temp::<u32>(edit_id)).unwrap_or_else(|| cur.div_ceil(1024).clamp(1, MAX_CACHE_GB));
            let r = crate::widgets::named(ui.add(egui::DragValue::new(&mut gb).range(1..=MAX_CACHE_GB)), crate::i18n::tr("Custom size limit, in GB"));
            register(ui.ctx(), "drag:settingsCacheCustom", r.rect);
            ui.label(RichText::new("GB").color(t.text));
            if r.changed() {
                ui.data_mut(|d| d.insert_temp(edit_id, gb));
            }
            // the focus egui keeps for the field, also while another application is in front (`r.has_focus()` is then
            // false): switching away mid-edit must not save half a number
            let editing = ui.memory(|m| m.has_focus(r.id)) || r.dragged();
            if !editing && ui.data(|d| d.get_temp::<u32>(edit_id)).is_some() {
                ui.data_mut(|d| d.remove::<u32>(edit_id));
                let _ = app.run("library.preferences", json!({"cacheMb": gb.saturating_mul(1024)}));
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(custom_id, custom));
    // (never counted here: the first count walks the whole folder, on a thread of its own that holds a row of the
    // activity stack, and a write in progress holds the size; this dialog draws it every frame)
    let rendered = &app.session.media.rendered;
    let activity = &app.session.activity;
    let used = rendered
        .disk()
        .map(|_| rendered.disk_size_now_with(|| activity.start("cache", "Counting the preview cache", lcc_engine::activity::Cancel::No)));
    if matches!(used, Some(None)) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    row(ui, t, crate::i18n::tr("In use"), |ui| {
        ui.label(RichText::new(in_use_text(used)).color(t.text));
        let r = ui.button(crate::i18n::tr("Clear Cache"));
        register(ui.ctx(), "button:settingsClearCache", r.rect);
        if r.clicked() {
            let _ = app.run("library.clearPreviews", json!({}));
        }
    });
    background_prep(app, ui, t);
    heading(ui, t, crate::i18n::tr("Local folders"));
    row(ui, t, crate::i18n::tr("Forget unchanged photos"), |ui| {
        let opts = [(0u32, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days"), (365, "After a year")];
        let mut v = app.session.forget_local_days;
        if choices(ui, "settingsForgetLocal", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"forgetLocalDays": v}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos seen in Local but never added or changed leave the catalog when their folder hasn't been browsed for this long (checked when the catalog opens). Files stay on disk; browsing the folder shows them again.",
        ),
    );
    #[cfg(not(target_arch = "wasm32"))]
    smart_previews(app, ui, t);
}

/// Settings ▸ Performance ▸ Prepare photos in the background: per mode, what is made ready before
/// the user gets to it (`prefs.backgroundPrep`), app-wide or for this library.
fn background_prep(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use lcc_engine::prep::{Mode, Scope};
    heading_help(ui, t, crate::i18n::tr("Prepare photos in the background"), "backgroundPrep");
    hint(
        ui,
        t,
        crate::i18n::tr(
            "While you go through photos in the loupe, the next ones are made ready before you reach them, and when the computer is otherwise idle the previews of every photo in view are built. The photo on screen always comes first; the work shows in the activity list, where it can be stopped.",
        ),
    );
    let own = app.session.prep.library.is_some();
    if app.session.library.is_some() {
        let mut v = own;
        if check(ui, "settings.prepOwn", &mut v, "Use different settings for this catalog") {
            let r = if v {
                let cur = app.session.prep_settings();
                app.run("prefs.setBackgroundPrep", json!({"library": true, "settings": cur}))
            } else {
                app.run("prefs.setBackgroundPrep", json!({"reset": true}))
            };
            if let Err(e) = r {
                log::warn!("background preparation: {e}");
            }
        }
        hint(ui, t, crate::i18n::tr("For example, a timelapse catalog can prepare nothing while a wedding catalog prepares everything."));
    }
    let cur = app.session.prep_settings();
    let mut patch: Option<Value> = None;
    let scopes = [(Scope::All, "Every photo"), (Scope::Marked, "Picks, rated and labelled"), (Scope::OnScreen, "Only the photo on screen")];
    for (mode, key, title) in [(Mode::Culling, "culling", "Culling (a folder before import)"), (Mode::Library, "library", "Library")] {
        let m = cur.mode(mode);
        heading(ui, t, crate::i18n::tr(title));
        let mut previews = m.previews;
        row(ui, t, crate::i18n::tr("Previews"), |ui| {
            if check(ui, &format!("settings.prep.{key}.previews"), &mut previews, "Build previews for every photo in view") {
                patch = Some(json!({key: {"previews": previews}}));
            }
        });
        hint(ui, t, crate::i18n::tr("Starts once you step through a few photos. Uses about 1.5 MB of disk a photo, and the computer's spare time."));
        let mut ahead = m.ahead;
        row(ui, t, crate::i18n::tr("Photos ahead"), |ui| {
            let opts = [(0u8, "Off"), (3, "3"), (5, "5"), (10, "10"), (20, "20")];
            if choices(ui, &format!("settingsPrepAhead-{key}"), &opts, &mut ahead) {
                patch = Some(json!({key: {"ahead": ahead}}));
            }
        });
        let mut eye = m.eye_ahead;
        row(ui, t, crate::i18n::tr("Eye focus ahead"), |ui| {
            let opts = [(0u8, "Off"), (3, "3"), (5, "5"), (8, "8"), (10, "10"), (20, "20")];
            if choices(ui, &format!("settingsPrepEyes-{key}"), &opts, &mut eye) {
                patch = Some(json!({key: {"eyeAhead": eye}}));
            }
        });
        let mut scope = m.scope;
        row(ui, t, crate::i18n::tr("Which photos"), |ui| {
            if choices(ui, &format!("settingsPrepScope-{key}"), &scopes, &mut scope) {
                patch = Some(json!({key: {"scope": scope}}));
            }
        });
    }
    heading(ui, t, crate::i18n::tr("Editing"));
    let mut ahead = cur.develop.ahead;
    row(ui, t, crate::i18n::tr("Photos ahead"), |ui| {
        let opts = [(0u8, "Off"), (1, "1"), (2, "2"), (3, "3"), (5, "5")];
        if choices(ui, "settingsPrepAhead-develop", &opts, &mut ahead) {
            patch = Some(json!({"develop": {"ahead": ahead}}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The photos each side of the one you edit are made ready with their settings, and the nearest two kept open, so stepping to one and the first slider move on it are quick. Uses memory: a few hundred MB a photo.",
        ),
    );
    let mut scope = cur.develop.scope;
    row(ui, t, crate::i18n::tr("Which photos"), |ui| {
        if choices(ui, "settingsPrepScope-develop", &scopes, &mut scope) {
            patch = Some(json!({"develop": {"scope": scope}}));
        }
    });
    if let Some(patch) = patch
        && let Err(e) = app.run("prefs.setBackgroundPrep", json!({"library": own, "settings": patch}))
    {
        log::warn!("background preparation: {e}");
    }
}

/// Where this library keeps its smart previews (the offline-editing proxies, which can be large):
/// the effective folder, what is in it, and choosing another one.
#[cfg(not(target_arch = "wasm32"))]
fn smart_previews(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    // listing a big folder every frame would be slow: refresh every 2 s and after a change
    let cache = egui::Id::new("smart-location");
    let now = ui.input(|i| i.time);
    let loc: Value = match ui.data(|d| d.get_temp::<(f64, Value)>(cache)) {
        Some((at, v)) if now - at < 2.0 => v,
        _ => {
            let v = app.run("library.smartPreviewsLocation", json!({})).unwrap_or(Value::Null);
            ui.data_mut(|d| d.insert_temp(cache, (now, v.clone())));
            v
        }
    };
    if loc.is_null() {
        return;
    }
    heading_help(ui, t, crate::i18n::tr("Smart previews"), "smartPreviews");
    let path = loc["path"].as_str().unwrap_or_default().to_string();
    let available = loc["available"].as_bool().unwrap_or(true);
    let custom = loc["custom"].as_bool().unwrap_or(false);
    row(ui, t, crate::i18n::tr("Folder"), |ui| {
        path_label(
            ui,
            "settingsSmartFolder",
            &path,
            egui::TextStyle::Body.resolve(ui.style()),
            if available { t.text } else { t.reject },
            if custom { 0.0 } else { label_width(ui, crate::i18n::tr("(catalog default)"), egui::TextStyle::Body) },
        );
        if !custom {
            let r = ui.label(RichText::new(crate::i18n::tr("(catalog default)")).color(t.text));
            register(ui.ctx(), "text:settingsSmartDefault", r.rect);
        }
    });
    let (count, bytes) = (loc["count"].as_u64().unwrap_or(0), loc["bytes"].as_u64().unwrap_or(0));
    if available {
        hint(
            ui,
            t,
            &crate::i18n::tr_format!(
                "{count} smart preview{} · {:.1} MB",
                if count == 1 { "" } else { "s" },
                bytes as f64 / 1048576.0,
                count = count
            ),
        );
    } else {
        hint(
            ui,
            t,
            crate::i18n::tr(
                "This folder is not available (drive disconnected?). Smart previews are not built or used until it is back or you choose another folder.",
            ),
        );
    }
    // what happens to the previews already built when the folder changes
    let mode_id = egui::Id::new("smart-existing");
    let mut mode: u8 = ui.data(|d| d.get_temp(mode_id)).unwrap_or(0);
    if count > 0 {
        row(ui, t, crate::i18n::tr("Existing previews"), |ui| {
            if choices(ui, "settingsSmartExisting", &[(0u8, "Move them"), (1, "Leave them"), (2, "Delete them")], &mut mode) {
                ui.data_mut(|d| d.insert_temp(mode_id, mode));
            }
        });
    }
    let existing = ["move", "leave", "discard"][mode.min(2) as usize];
    let mut result = None;
    ui.horizontal(|ui| {
        ui.add_space(crate::dialog_frame::control_indent(ui));
        if app.services.pick_folder.is_some() {
            let r = ui.button(crate::i18n::tr("Choose Folder…"));
            register(ui.ctx(), "button:settingsSmartChoose", r.rect);
            if r.clicked()
                && let Some(dir) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                result = Some(app.run("library.smartPreviewsLocation", json!({"path": dir, "existing": existing})));
            }
        }
        let r = ui.add_enabled(custom, egui::Button::new(crate::i18n::tr("Use Catalog Folder")));
        register(ui.ctx(), "button:settingsSmartReset", r.rect);
        if r.clicked() {
            result = Some(app.run("library.smartPreviewsLocation", json!({"reset": true, "existing": existing})));
        }
    });
    if let Some(r) = result {
        ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
        match r {
            Ok(v) => {
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let msg = match (existing, v["handled"].as_u64().unwrap_or(0)) {
                    (_, 0) => "Smart previews folder changed".to_string(),
                    ("move", n) => format!("Smart previews folder changed; moved {n}"),
                    ("discard", n) => format!("Smart previews folder changed; deleted {n}"),
                    _ => "Smart previews folder changed".to_string(),
                };
                app.toast(ui.ctx(), if failed > 0 { crate::i18n::tr_format!("{msg} ({failed} failed)", failed = failed, msg = msg) } else { msg });
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    hint(ui, t, crate::i18n::tr("Keep it on a drive with room: smart previews are about 1 MB per photo. The thumbnail cache stays in the catalog."));
}

// ---------------------------------------------------------------------------------- Interface

/// The monitor profile (`app.displayProfile`): previews are shown through the display's ICC
/// profile, so wide-gamut and calibrated displays show colours as they are.
fn display_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    heading_help(ui, t, crate::i18n::tr("Monitor profile"), "display");
    let current = app.renderer.display().cloned();
    let screen = current.as_ref().is_some_and(|d| d.path == crate::SCREEN_PROFILE);
    row(ui, t, crate::i18n::tr("Profile"), |ui| {
        let name = match &current {
            Some(d) => d.profile.description.clone(),
            None => crate::i18n::tr("None (the display is treated as sRGB)").to_string(),
        };
        ui.label(RichText::new(name).color(t.text));
        if screen {
            hint(ui, t, crate::i18n::tr("this screen's profile"));
        }
    });
    row(ui, t, "", |ui| {
        let can = app.services.pick_display_profile.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose Profile…")));
        register(ui.ctx(), "button:settingsDisplayProfile", r.rect);
        if r.clicked()
            && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next())
        {
            app.ui.settings.display_profile = path;
        }
        // with no profile chosen, previews follow the screen's own profile where the host knows it
        let none = if app.services.screen_profile.is_some() { crate::i18n::tr("Use Screen Profile") } else { crate::i18n::tr("Use sRGB") };
        let r = ui.add_enabled(!app.ui.settings.display_profile.is_empty(), egui::Button::new(none));
        register(ui.ctx(), "button:settingsDisplayProfileNone", r.rect);
        if r.clicked() {
            app.ui.settings.display_profile.clear();
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    if let Some(e) = &app.display_error {
        ui.label(RichText::new(e).size(11.0).color(t.reject));
    }
    if let Some(d) = &current {
        let info = d.describe();
        if !screen {
            row(ui, t, crate::i18n::tr("File"), |ui| {
                path_label(ui, "settingsDisplayFile", &d.path, egui::FontId::proportional(11.0), t.text_dim, 0.0);
            });
        }
        row(ui, t, crate::i18n::tr("Type"), |ui| {
            let kind = match d.profile.kind {
                lcc_engine::display::DisplayKind::MatrixTrc => crate::i18n::tr("Matrix/TRC"),
                lcc_engine::display::DisplayKind::Lut => crate::i18n::tr("LUT-based"),
            };
            ui.label(RichText::new(kind).color(t.text));
        });
        row(ui, t, crate::i18n::tr("Primaries (x, y)"), |ui| {
            let p = &info["primaries"];
            let xy = |(k, l): (&str, &str)| format!("{l} {:.3}, {:.3}", p[k][0].as_f64().unwrap_or(0.0), p[k][1].as_f64().unwrap_or(0.0));
            let text = [("red", "R"), ("green", "G"), ("blue", "B")].map(xy).join("   ");
            ui.label(RichText::new(text).size(11.0).color(t.text_dim));
        });
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos are shown through your monitor's ICC profile, so colours are right on wide-gamut and calibrated displays, and the Detail view uses the display's whole gamut. The histogram, the preview caches and exports are not affected.",
        ),
    );
    #[cfg(target_os = "linux")]
    hint(ui, t, crate::i18n::tr("Profiles assigned in your desktop's colour settings are usually in ~/.local/share/icc."));
}

/// LR-PREF-SHARED: presets, profiles and the user's habits are the app's, shared by every library;
/// a library can keep its own instead (`store.useLibrary`). Shown only with a library open and the
/// shared store available (the desktop app).
fn shared_store(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let info = app.session.store_info();
    if info["available"] != serde_json::Value::Bool(true) || app.session.library.is_none() {
        return;
    }
    let mut own = info["libraryKeepsOwn"] == serde_json::Value::Bool(true);
    if check(ui, "settings.libraryKeepsOwnStore", &mut own, "This catalog keeps its own presets, profiles and habits") {
        match app.run("store.useLibrary", serde_json::json!({"keep": own})) {
            Ok(_) => app.toast(
                ui.ctx(),
                crate::i18n::tr(if own {
                    "This catalog now keeps its own presets"
                } else {
                    "This catalog now uses the presets every catalog shares"
                }),
            ),
            Err(e) => app.toast_error(ui.ctx(), e),
        }
    }
    hint(
        ui,
        t,
        crate::i18n::tr("Off (the default): presets, profiles, export presets and your other habits are the same in every catalog on this computer."),
    );
}

fn interface_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let system = app.system_theme(ui.ctx());
    let forced = forced_slider_layout(app);
    appearance(ui, t, &mut app.ui.settings, system, forced);
    heading_help(ui, t, crate::i18n::tr("Filmstrip"), "gridAndFilmstrip");
    check(ui, "settings.filmNames", &mut app.ui.settings.film_names, "Show file names");
    check(ui, "settings.filmBadges", &mut app.ui.settings.film_badges, "Show ratings, flags and edit badges");
    heading(ui, t, crate::i18n::tr("Grid"));
    row(ui, t, crate::i18n::tr("Ratings & flags"), |ui| {
        choices(
            ui,
            "settingsGridBadges",
            &[(GridBadges::Auto, "When rated or hovered"), (GridBadges::Always, "Always"), (GridBadges::Never, "Never")],
            &mut app.ui.settings.grid_badges,
        );
    });
    check(ui, "settings.doubleClickDevelop", &mut app.ui.settings.double_click_develop, "Double-click opens the photo in Develop");
    check(ui, "settings.showFilenames", &mut app.ui.show_filenames, "Square Grid: show file names and formats");
    row(ui, t, crate::i18n::tr("Wheel scroll speed"), |ui| {
        choices(ui, "settingsWheelSpeed", &[(1.0, "Normal"), (2.0, "Fast"), (3.0, "Fastest")], &mut app.ui.settings.wheel_scroll_speed);
    });
    check(ui, "settings.reduceMotion", &mut app.ui.settings.reduce_motion, "Reduce motion");
    hint(ui, t, crate::i18n::tr("Turns off zoom and spinner animations. Fades stay."));
    check(ui, "settings.wheelResizesBrush", &mut app.ui.settings.wheel_resizes_brush, "Wheel resizes brush instead of zooming");
    check(ui, "settings.expandedSliders", &mut app.ui.settings.expanded_sliders, "Expanded sliders");
    hint(ui, t, crate::i18n::tr("Warning, expanding sliders is not XMP compliant."));
    heading(ui, t, crate::i18n::tr("Keywords"));
    row(ui, t, crate::i18n::tr("Keywords of the selection"), |ui| {
        use crate::state::KeywordChips as K;
        choices(ui, "settingsKeywordChips", &[(K::Paths, "Whole names"), (K::Grouped, "Grouped by parent")], &mut app.ui.settings.keyword_chips);
    });
    hint(
        ui,
        t,
        crate::i18n::tr("Grouped: keywords inside the same keyword share a line naming it, and long lists and the suggestions are shortened."),
    );
    heading(ui, t, crate::i18n::tr("Presets"));
    check(ui, "settings.userPresetsFirst", &mut app.ui.settings.user_presets_first, "List User Presets first in the Presets panel");
    shared_store(app, ui, t);
    heading_help(ui, t, crate::i18n::tr("Detail View"), "detailView");
    check(ui, "settings.navigator", &mut app.ui.navigator, "Show the Navigator while zoomed in");
    check(ui, "settings.hideFilterInDevelop", &mut app.ui.settings.hide_filter_in_develop, "Hide filter text in Develop mode");
    row(ui, t, crate::i18n::tr("Info overlay"), |ui| {
        use crate::state::InfoOverlay as I;
        choices(ui, "settingsInfo", &[(I::Off, "Off"), (I::Basic, "Info 1"), (I::Exposure, "Info 2")], &mut app.ui.info_overlay);
    });
    hint(ui, t, crate::i18n::tr("View → View Options chooses the lines Info 1 and Info 2 show."));
    check(ui, "settings.eyeFocus", &mut app.ui.eye_focus, "Show the eye focus strip");
    check(ui, "settings.eyeFocusImport", &mut app.ui.eye_focus_import, "Show the eye focus strip while browsing a folder");
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Each person's sharper eye at 100 % across the top of the loupe. The quick settings bar (point at the space beside the module names) switches it too.",
        ),
    );
    // the overlay that reports every change (and the control surfaces' readout)
    super::surface_osd::settings_section(app, ui, t);
}

/// View ▸ View Options (⌘J), Lightroom Classic's Loupe View tab: whether the info overlay shows and
/// which, the three lines each of Info 1 and Info 2 shows, which of them shows briefly when the
/// photo changes, and whether the loupe shows its loading and rendering message
/// (`view.viewOptions`). Changes apply as they are made.
pub(super) fn view_options(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::state::{InfoOverlay as I, LoupeInfo, LoupeInfoField as F};
    heading(ui, t, crate::i18n::tr("Loupe View"));
    let mut show = app.ui.info_overlay != I::Off;
    if check(ui, "viewOptions.showInfo", &mut show, "Show Info Overlay") {
        app.ui.info_overlay = if show { I::Basic } else { I::Off };
    }
    ui.add_enabled_ui(show, |ui| {
        ui.horizontal(|ui| {
            choices(ui, "viewOptionsInfo", &[(I::Basic, "Info 1"), (I::Exposure, "Info 2")], &mut app.ui.info_overlay);
        });
    });
    for (n, title, defaults) in [(1, "Loupe Info 1", LoupeInfo::INFO1), (2, "Loupe Info 2", LoupeInfo::INFO2)] {
        heading(ui, t, crate::i18n::tr(title));
        let lines = if n == 1 { &mut app.ui.settings.loupe_info.info1 } else { &mut app.ui.settings.loupe_info.info2 };
        for (i, line) in lines.iter_mut().enumerate() {
            let id = format!("viewOptionsInfo{n}Line{}", i + 1);
            let r = crate::widgets::combo_box(&id).width(220.0).selected_text(crate::i18n::tr(line.label())).show_ui(ui, |ui| {
                for f in F::ALL {
                    let key = serde_json::to_value(f).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                    let r = ui.selectable_label(line == f, crate::i18n::tr(f.label()));
                    register(ui.ctx(), format!("button:{id}-{key}"), r.rect);
                    if r.clicked() {
                        *line = *f;
                    }
                }
            });
            register(ui.ctx(), format!("combo:{id}"), r.response.rect);
        }
        let r = ui.add_enabled(*lines != defaults, egui::Button::new(crate::i18n::tr("Use Defaults")));
        register(ui.ctx(), format!("button:viewOptionsDefaults{n}"), r.rect);
        if r.clicked() {
            *lines = defaults;
        }
        // one Info at most shows briefly: ticking one unticks the other
        let info = if n == 1 { I::Basic } else { I::Exposure };
        let mut briefly = app.ui.settings.loupe_info.briefly == info;
        if check(ui, &format!("viewOptions.briefly{n}"), &mut briefly, "Show briefly when photo changes") {
            app.ui.settings.loupe_info.briefly = if briefly { info } else { I::Off };
        }
    }
    heading(ui, t, crate::i18n::tr("General"));
    check(ui, "viewOptions.loupeMessages", &mut app.ui.settings.loupe_messages, "Show message when loading or rendering photos");
}

// ----------------------------------------------------------------------------------- Slideshow

/// A playback duration slider (`slider:{id}`). While it is dragged the value changes in memory
/// (the slideshow reads it); letting go saves it through `slideshow.playback`.
fn duration_slider(app: &mut LccApp, ui: &mut egui::Ui, id: &str, key: &str, value: f64, range: std::ops::RangeInclusive<f64>) {
    let mut v = value;
    let r = ui.add(egui::Slider::new(&mut v, range).logarithmic(key == "slideDuration").suffix(" s").max_decimals(1));
    register(ui.ctx(), format!("slider:{id}"), r.rect);
    if r.changed()
        && let Ok(next) = app.session.slideshow.merged(&json!({ key: v }))
    {
        app.session.slideshow = next;
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("slideshow.playback", json!({ key: v }));
    }
}

/// Width of the Slideshow tab's settings column (the slide preview is beside it).
const SLIDESHOW_COLUMN_W: f32 = 480.0;

fn slideshow_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    // the settings scroll on the left, the composed slide stays in view on the right
    let top_down = egui::Layout::top_down(egui::Align::Min);
    // as tall as the window allows (the dialog's title, tabs and Close button take ~260 pt)
    // (in the dialog: as tall as its content area, which scrolls no further)
    let tall = if crate::dialog_frame::in_form(ui) {
        (ui.clip_rect().height() - 8.0).clamp(200.0, 760.0)
    } else {
        (ui.ctx().content_rect().height() - 260.0).clamp(300.0, 760.0)
    };
    // (no group panels in the plain Settings window, so no room for their edges)
    let pad = if crate::dialog_frame::plain(ui) { 0 } else { crate::dialog_frame::GROUP_PAD as i8 };
    // the touch Settings screen on a narrow screen: the preview above, the settings under it, in
    // the page's own scrolling
    if crate::dialog_frame::finger_sized(ui) && ui.available_width() < SLIDESHOW_COLUMN_W + 12.0 + super::slide_layout::PREVIEW_W {
        super::slide_layout::preview(app, ui, t, f32::INFINITY);
        ui.add_space(6.0);
        super::slide_templates::section(app, ui, t);
        ui.add_space(6.0);
        playback_section(app, ui, t);
        ui.add_space(6.0);
        super::slide_layout::sections(app, ui, t);
        ui.add_space(6.0);
        super::slide_overlays::sections(app, ui, t);
        ui.add_space(6.0);
        super::slide_titles::sections(app, ui, t);
        ui.add_space(6.0);
        export_section(app, ui, t);
        return;
    }
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(SLIDESHOW_COLUMN_W, tall), top_down, |ui| {
            ui.set_width(SLIDESHOW_COLUMN_W);
            // (room inside the scroll area for the group panels' edges)
            let framed = egui::Frame::NONE.inner_margin(egui::Margin { left: pad, right: pad, top: 0, bottom: 0 });
            egui::ScrollArea::vertical().id_salt("settingsSlideshowScroll").max_height(tall).auto_shrink([false, true]).show(ui, |ui| {
                framed.show(ui, |ui| {
                    super::slide_templates::section(app, ui, t);
                    ui.add_space(6.0);
                    playback_section(app, ui, t);
                    ui.add_space(6.0);
                    super::slide_layout::sections(app, ui, t);
                    ui.add_space(6.0);
                    super::slide_overlays::sections(app, ui, t);
                    ui.add_space(6.0);
                    super::slide_titles::sections(app, ui, t);
                    ui.add_space(6.0);
                    export_section(app, ui, t);
                    // (each column closes its own groups)
                    crate::dialog_frame::group_end(ui);
                })
            });
        });
        ui.add_space(12.0);
        ui.allocate_ui_with_layout(egui::vec2(super::slide_layout::PREVIEW_W, tall), top_down, |ui| {
            ui.set_width(super::slide_layout::PREVIEW_W);
            super::slide_layout::preview(app, ui, t, f32::INFINITY);
            crate::dialog_frame::group_end(ui);
        });
    });
}

/// Export: PDF, JPEG sequence or video of the slideshow (View ▸ Export Slideshow…).
fn export_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Export"));
    let running = app.session.slideshow_export.as_ref().is_some_and(|r| r.running());
    ui.horizontal_wrapped(|ui| {
        for (id, label, format) in [
            ("slideshowExportPdf", "Export PDF…", "pdf"),
            ("slideshowExportJpeg", "Export JPEG…", "jpeg"),
            ("slideshowExportVideo", "Export Video…", "video"),
        ] {
            let r = ui.add_enabled(!running, egui::Button::new(crate::i18n::tr(label)));
            register(ui.ctx(), format!("button:{id}"), r.rect);
            if r.clicked() {
                let _ = app.run("dialog.slideshowExport", json!({"format": format}));
            }
        }
    });
    hint(ui, t, crate::i18n::tr("The photos in view as slides, with the settings above. Progress shows in the activity stack."));
}

fn playback_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use lcc_engine::slideshow::{FADE_MAX, SLIDE_MAX, SLIDE_MIN};
    heading_help(ui, t, crate::i18n::tr("Playback"), "slideshow");
    let s = app.session.slideshow.clone();
    let mut v = s.manual;
    if check(ui, "settings.slideshowManual", &mut v, "Manual slideshow") {
        let _ = app.run("slideshow.playback", json!({"manual": v}));
    }
    hint(ui, t, crate::i18n::tr("A manual slideshow shows each slide until → or ← changes it. Exports still use the slide duration."));
    row(ui, t, crate::i18n::tr("Slide duration"), |ui| {
        duration_slider(app, ui, "slideshowSlide", "slideDuration", s.slide_duration, SLIDE_MIN..=SLIDE_MAX)
    });
    row(ui, t, crate::i18n::tr("Fade duration"), |ui| {
        duration_slider(app, ui, "slideshowFade", "fadeDuration", s.fade_duration, 0.0..=FADE_MAX.min(s.slide_duration))
    });
    hint(ui, t, crate::i18n::tr("A slide stays up for its duration, its fade in included. A fade of 0 cuts."));
    row(ui, t, crate::i18n::tr("Fade"), |ui| {
        #[derive(Clone, Copy, PartialEq)]
        enum Fade {
            Cross,
            Black,
            White,
            Color,
        }
        let mut f = match s.fade_color {
            None => Fade::Cross,
            Some([0, 0, 0]) => Fade::Black,
            Some([255, 255, 255]) => Fade::White,
            Some(_) => Fade::Color,
        };
        let opts = [(Fade::Cross, "Crossfade"), (Fade::Black, "Through black"), (Fade::White, "Through white"), (Fade::Color, "Through a colour")];
        if choices(ui, "settingsSlideshowFade", &opts, &mut f) {
            let c = match f {
                Fade::Cross => Value::Null,
                Fade::Black => json!("#000000"),
                Fade::White => json!("#ffffff"),
                Fade::Color => json!([64, 64, 64]),
            };
            let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
        }
        if f == Fade::Color
            && let Some(mut c) = s.fade_color
        {
            let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
            register(ui.ctx(), "button:slideshowFadeColor", r.rect);
            if r.changed() {
                let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
            }
        }
    });
    let mut v = s.random_order;
    if check(ui, "settings.slideshowRandom", &mut v, "Random order") {
        let _ = app.run("slideshow.playback", json!({"randomOrder": v}));
    }
    let mut v = s.repeat;
    if check(ui, "settings.slideshowRepeat", &mut v, "Repeat (off: the slideshow ends after the last photo)") {
        let _ = app.run("slideshow.playback", json!({"repeat": v}));
    }
    let mut v = s.prepare_previews;
    if check(ui, "settings.slideshowPrepare", &mut v, "Prepare previews in advance") {
        let _ = app.run("slideshow.playback", json!({"preparePreviews": v}));
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Renders the next slides while the current one is up, so fades never wait. Off: each slide renders when it is due (less memory and work).",
        ),
    );
    hint(
        ui,
        t,
        crate::i18n::tr("Slideshow in the View menu plays the photos in view from the selected one. These settings are saved with the catalog."),
    );
}

/// Appearance Mode (`button:settingsAppearance-{0,1,2}`: Auto, Dark, Light) above the light and
/// dark theme cards (`radio:theme-{id}`), each with a preview of the chosen theme. The card in
/// use is outlined. Applied on the next frame ([`LccApp::sync_theme`]). Then the font size and the
/// slider rows that apply to the layout drawn (`forced`: [`forced_slider_layout`]).
fn appearance(
    ui: &mut egui::Ui,
    t: &Tokens,
    s: &mut crate::state::AppSettings,
    system: Option<egui::Theme>,
    forced: Option<crate::theme::SliderLayout>,
) {
    use crate::state::{AppearanceMode as M, DarkTheme, LightTheme};
    heading_help(ui, t, crate::i18n::tr("Appearance"), "appearance");
    row(ui, t, crate::i18n::tr("Appearance Mode"), |ui| {
        choices(ui, "settingsAppearance", &[(M::Auto, "Sync with System"), (M::Dark, "Dark Mode"), (M::Light, "Light Mode")], &mut s.appearance_mode);
    });
    ui.add_space(4.0);
    let light_active = s.theme(system) == s.light_theme.kind();
    let available = ui.available_width();
    let mut cards = |ui: &mut egui::Ui, width: f32| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let light: Vec<_> = LightTheme::ALL.iter().map(|l| (*l, l.kind())).collect();
        theme_card(ui, t, "Light Theme", light_active, &light, &mut s.light_theme, &mut s.favorite_themes, width);
        let dark: Vec<_> = DarkTheme::ALL.iter().map(|d| (*d, d.kind())).collect();
        theme_card(ui, t, "Dark Theme", !light_active, &dark, &mut s.dark_theme, &mut s.favorite_themes, width);
    };
    if available < 440.0 {
        ui.vertical(|ui| cards(ui, available));
    } else {
        ui.horizontal_top(|ui| cards(ui, ((available - 12.0) / 2.0).floor()));
    }
    ui.add_space(6.0);
    row(ui, t, crate::i18n::tr("Font Size"), |ui| {
        let options = crate::theme::UiFontSize::ALL.map(|size| (size, size.label()));
        choices(ui, "settingsFontSize", &options, &mut s.ui_font_size);
    });
    let rows = SliderRows::for_layout(forced, s.slider_layout);
    if rows.layout {
        row(ui, t, crate::i18n::tr("Sliders"), |ui| {
            let options = crate::theme::SliderLayout::ALL.map(|layout| (layout, layout.label()));
            choices(ui, "settingsSliderLayout", &options, &mut s.slider_layout);
        });
    }
    // (the choice may have just changed what the rows below apply to)
    let rows = SliderRows::for_layout(forced, s.slider_layout);
    row(ui, t, crate::i18n::tr("Slider text"), |ui| {
        let options = crate::theme::SliderText::ALL.map(|size| (size, size.label()));
        choices(ui, "settingsSliderText", &options, &mut s.slider_text);
    });
    if rows.control {
        row(ui, t, crate::i18n::tr("Slider control"), |ui| {
            let options = crate::theme::SliderControl::ALL.map(|c| (c, c.label()));
            choices(ui, "settingsSliderControl", &options, &mut s.slider_control);
        });
    }
    // (every slider, pills too)
    hint(ui, t, crate::i18n::tr("Hold Shift for fine control"));
    ui.add_space(2.0);
}

/// Which of Settings ▸ Interface's slider rows apply, and so show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SliderRows {
    /// Sliders (One Row, Two Rows, Pill): only where the layout doesn't choose for itself (the
    /// Classic layout; Modern always draws pills, Compact always one dense row).
    pub layout: bool,
    /// Slider control (acceleration): only for sliders with a thumb, which a pill has not.
    pub control: bool,
}

impl SliderRows {
    /// `forced`: the slider layout the interface layout always uses (Modern: pills, Compact: one
    /// row; `None` in Classic, which uses `chosen`, the Sliders setting).
    pub(crate) fn for_layout(forced: Option<crate::theme::SliderLayout>, chosen: crate::theme::SliderLayout) -> SliderRows {
        let drawn = forced.unwrap_or(chosen);
        SliderRows { layout: forced.is_none(), control: drawn != crate::theme::SliderLayout::Pill }
    }
}

/// The slider layout the interface layout drawn now always uses, whatever Settings ▸ Interface ▸
/// Sliders says: pills in Modern, one dense row in Compact, `None` (the setting) in Classic.
pub(crate) fn forced_slider_layout(app: &LccApp) -> Option<crate::theme::SliderLayout> {
    if app.ui.modern.enabled || app.compact() { Some(app.slider_layout()) } else { None }
}

/// One family's card: its title (and "Active" when it is the one shown), a preview of the
/// selected theme and a radio button per theme.
fn theme_card<V: PartialEq + Copy>(
    ui: &mut egui::Ui,
    t: &Tokens,
    title: &str,
    active: bool,
    options: &[(V, crate::theme::ThemeKind)],
    value: &mut V,
    favorites: &mut Vec<String>,
    width: f32,
) {
    egui::Frame::new()
        .fill(t.inset)
        .stroke(egui::Stroke::new(1.0, if active { t.accent } else { t.field_border }))
        .corner_radius(6)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                let inner = (width - 18.0).clamp(120.0, 240.0);
                ui.set_width(inner);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.0)).color(t.text));
                    if active {
                        ui.label(RichText::new(crate::i18n::tr("In Use")).size(11.0).color(t.accent));
                    }
                });
                let shown = options.iter().find(|(v, _)| v == value).or(options.first()).map(|(_, k)| *k).unwrap_or_default();
                theme_preview(ui, shown, inner);
                for (v, kind) in options {
                    let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 24.0));
                    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| row.contains(p));
                    ui.horizontal(|ui| {
                        let r = ui.radio_value(value, *v, crate::i18n::tr(kind.label()));
                        register(ui.ctx(), format!("radio:theme-{}", kind.id()), r.rect);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let favorite = favorites.iter().any(|id| id == kind.id());
                            let r = ui
                                .add_visible(favorite || hovered, egui::Button::new("").min_size(egui::vec2(24.0, 24.0)).frame(false))
                                .on_hover_text(crate::i18n::tr(if favorite { "Remove from favorites" } else { "Add to favorites" }));
                            r.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!(
                                        "{}: {}",
                                        crate::i18n::tr(if favorite { "Remove from favorites" } else { "Add to favorites" }),
                                        crate::i18n::tr(kind.label())
                                    ),
                                )
                            });
                            register(ui.ctx(), format!("button:themeFavorite-{}", kind.id()), r.rect);
                            if favorite || hovered {
                                crate::rating::paint_star(ui.painter(), r.rect.shrink(4.0), favorite, if favorite { t.accent } else { t.text_dim });
                            }
                            if r.clicked() {
                                if favorite {
                                    favorites.retain(|id| id != kind.id());
                                } else {
                                    favorites.push(kind.id().into());
                                }
                            }
                        });
                    });
                }
            });
        });
}

/// A small app window painted with `kind`'s tokens: the top bar with its search field, the
/// photo with the filmstrip under it, the Edit panel (histogram and sliders), the tool strip and
/// the bottom bar.
fn theme_preview(ui: &mut egui::Ui, kind: crate::theme::ThemeKind, width: f32) {
    use egui::{Color32, Rect, Stroke, StrokeKind, pos2, vec2};
    let p = Tokens::for_kind(kind);
    let (rect, _) = ui.allocate_exact_size(vec2(width, (width * 0.56).round().min(130.0)), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let bar = |y: f32, x: f32, w: f32, c: Color32| painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, 2.0)), 1.0, c);
    painter.rect_filled(rect, 3.0, p.canvas);
    // top bar: sidebar and back icons, the centred search field, icons on the right
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), 13.0));
    painter.rect_filled(top, 0.0, p.chrome);
    for x in [6.0, 14.0, 20.0] {
        painter.rect_filled(Rect::from_min_size(top.min + vec2(x, 4.5), vec2(4.0, 4.0)), 1.0, p.icon);
    }
    let search = Rect::from_center_size(top.center(), vec2(rect.width() * 0.34, 8.0));
    painter.rect(search, 2.0, p.field, Stroke::new(1.0, p.field_border), StrokeKind::Inside);
    bar(search.center().y - 1.0, search.center().x - 9.0, 18.0, p.text_dim);
    for i in 0..4 {
        painter.rect_filled(Rect::from_min_size(pos2(top.right() - 10.0 - i as f32 * 9.0, top.top() + 4.5), vec2(4.0, 4.0)), 2.0, p.icon);
    }
    // bottom bar: view buttons, rating stars, a button
    let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 12.0), rect.max);
    painter.rect_filled(bottom, 0.0, p.chrome);
    for x in [6.0, 13.0, 20.0] {
        painter.rect_filled(Rect::from_min_size(bottom.min + vec2(x, 4.0), vec2(4.0, 4.0)), 1.0, p.icon);
    }
    let stars_x = bottom.center().x - 30.0;
    for i in 0..5 {
        painter.circle_filled(pos2(stars_x + i as f32 * 5.0, bottom.center().y), 1.4, p.star);
    }
    let copy = Rect::from_min_size(pos2(stars_x + 28.0, bottom.top() + 2.5), vec2(30.0, 7.0));
    painter.rect(copy, 3.0, p.button, Stroke::new(1.0, p.button_border), StrokeKind::Inside);
    // tool strip on the right edge, the first tool (Edit) active
    let strip = Rect::from_min_max(pos2(rect.right() - 12.0, top.bottom()), pos2(rect.right(), bottom.top()));
    painter.rect_filled(strip, 0.0, p.chrome);
    painter.line_segment([strip.left_top(), strip.left_bottom()], Stroke::new(1.0, p.divider));
    for i in 0..5 {
        let r = Rect::from_min_size(pos2(strip.left() + 3.0, strip.top() + 4.0 + i as f32 * 9.0), vec2(6.0, 6.0));
        if i == 0 {
            painter.rect_filled(r.expand(1.5), 1.5, p.tool_active);
        }
        painter.rect_stroke(r, 1.0, Stroke::new(1.0, if i == 0 { p.text } else { p.icon }), StrokeKind::Inside);
    }
    // Edit panel: histogram, then labelled sliders
    let panel = Rect::from_min_max(pos2(strip.left() - (rect.width() * 0.27).round(), top.bottom()), pos2(strip.left(), bottom.top()));
    painter.rect_filled(panel, 0.0, p.chrome);
    painter.line_segment([panel.left_top(), panel.left_bottom()], Stroke::new(1.0, p.divider));
    let hist = Rect::from_min_size(panel.min + vec2(4.0, 4.0), vec2(panel.width() - 8.0, 14.0));
    painter.rect_filled(hist, 1.0, p.inset);
    let curve = [0.2, 0.55, 0.9, 0.7, 0.5, 0.62, 0.4, 0.3, 0.18, 0.1];
    let mut pts = vec![hist.left_bottom()];
    for (i, h) in curve.iter().enumerate() {
        pts.push(pos2(hist.left() + hist.width() * i as f32 / (curve.len() - 1) as f32, hist.bottom() - hist.height() * h));
    }
    pts.push(hist.right_bottom());
    // the area under the curve, a column per stretch between two points
    for pair in pts.windows(2) {
        if let [a, b] = pair {
            let r = Rect::from_min_max(pos2(a.x, a.y.max(b.y)), pos2(b.x, hist.bottom()));
            painter.rect_filled(r, 0.0, p.track.gamma_multiply(0.5));
        }
    }
    painter.add(egui::Shape::line(pts, Stroke::new(1.0, p.text_dim)));
    let sliders = [0.55, 0.62, 0.35, 0.7, 0.6, 0.45];
    let mut y = hist.bottom() + 7.0;
    bar(y, panel.left() + 5.0, 14.0, p.text);
    y += 6.0;
    for v in sliders {
        if y + 7.0 > panel.bottom() {
            break;
        }
        bar(y, panel.left() + 5.0, 12.0, p.text_label);
        bar(y, panel.right() - 10.0, 5.0, p.text_dim);
        let track_y = y + 5.0;
        painter.line_segment([pos2(panel.left() + 5.0, track_y), pos2(panel.right() - 5.0, track_y)], Stroke::new(1.0, p.track));
        painter.circle_filled(pos2(panel.left() + 5.0 + (panel.width() - 10.0) * v, track_y), 1.8, p.thumb);
        y += 10.0;
    }
    // the photo (its own colours: a photo looks the same in every theme) above the filmstrip
    let film = Rect::from_min_max(pos2(rect.left(), bottom.top() - 18.0), pos2(panel.left(), bottom.top()));
    let stage = Rect::from_min_max(pos2(rect.left(), top.bottom()), pos2(panel.left(), film.top()));
    let photo = Rect::from_center_size(stage.center(), vec2(stage.width() * 0.62, stage.height() - 8.0));
    painter.rect_filled(photo, 0.0, Color32::from_rgb(0x6f, 0x8f, 0xb4));
    let horizon = photo.top() + photo.height() * 0.62;
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(photo.left(), horizon),
            pos2(photo.left() + photo.width() * 0.3, photo.top() + photo.height() * 0.3),
            pos2(photo.left() + photo.width() * 0.55, horizon),
        ],
        Color32::from_rgb(0x4a, 0x55, 0x66),
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(photo.left() + photo.width() * 0.35, horizon),
            pos2(photo.left() + photo.width() * 0.68, photo.top() + photo.height() * 0.2),
            pos2(photo.right(), horizon),
        ],
        Color32::from_rgb(0x5a, 0x63, 0x72),
        Stroke::NONE,
    ));
    painter.rect_filled(Rect::from_min_max(pos2(photo.left(), horizon), photo.max), 0.0, Color32::from_rgb(0x3d, 0x5a, 0x3a));
    painter.line_segment([film.left_top(), film.right_top()], Stroke::new(1.0, p.divider));
    for i in 0..5 {
        let cell = Rect::from_min_size(pos2(film.left() + 4.0 + i as f32 * 17.0, film.top() + 3.0), vec2(15.0, 12.0));
        if cell.right() > film.right() - 2.0 {
            break;
        }
        let selected = i == 1;
        painter.rect_filled(cell, 0.0, if selected { p.cell_selected } else { p.cell });
        let thumb = cell.shrink(2.5);
        painter.rect_filled(thumb, 0.0, Color32::from_rgb(0x6f + 8 * i as u8, 0x80, 0x8f));
        if selected {
            super::grid::selection_ring(&painter, &p, thumb, lcc_engine::SelectionState::Active);
        }
    }
}

// ------------------------------------------------------------------------------- Open Library…

/// Switching libraries is refused while an import (or the folder scan before one) is running: its
/// remaining batches would be committed into the new library (issue #1509).
pub(crate) fn can_switch_library(app: &LccApp) -> Result<(), String> {
    if crate::lightroom_import::is_running(app) {
        return Err("wait for Lightroom catalog import to finish before switching catalogs".into());
    }
    if app.import.is_some() || app.scan.is_some() {
        return Err("wait for the import to finish before switching catalogs".into());
    }
    Ok(())
}

/// `app.openLibrary {path?}`: close the current library and open (or create) the one at `path`,
/// or a folder chosen in a dialog. Remembered as the library to open at launch.
///
/// A Lightroom Classic catalog (its folder, an `.lrdata` / `.lrcat-data` folder or the `.lrcat`
/// itself, see [`lcc_engine::lightroom_folder`]) is not opened: the current library stays
/// and a dialog offers Import Lightroom Catalog… instead; returns `{opened: false,
/// lightroomCatalog: <.lrcat path or null>, path}`.
pub fn open_library(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Open Catalog"));
            match crate::pick::ask(app, "app.openLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    // A Lightroom catalog folder (or part of one) is not a library: before closing the library
    // open now, offer to import it instead (issue #891). The engine refuses it too.
    if let Some(place) = lcc_engine::lightroom_folder::lightroom_catalog_at(std::path::Path::new(&path)) {
        let catalog = place.catalog.map(|c| c.to_string_lossy().into_owned());
        let err = lcc_engine::EngineError::LightroomCatalog { path: path.clone().into(), catalog: catalog.clone().map(Into::into) };
        // the library problem window (no library could be opened) shows the error instead
        if app.library_problem.is_some() {
            return Err(err.to_string());
        }
        app.ui.dialog = Some(crate::state::Dialog::LightroomCatalog { folder: path.clone(), catalog: catalog.clone() });
        return Ok(json!({"opened": false, "lightroomCatalog": catalog, "path": path}));
    }
    // a remembered library that is gone: say so (and forget it) instead of creating an empty one there
    if app.ui.settings.recent_libraries.contains(&path) && !std::path::Path::new(&path).exists() {
        app.ui.settings.recent_libraries.retain(|r| r != &path);
        return Err(format!("{path}: the catalog is no longer there (moved, renamed or on a disconnected drive); removed from Open Recent Catalog"));
    }
    // `demo`: a newly created library starts with the procedural demo photos (the welcome dialog's choice)
    let demo = p.get("demo").and_then(Value::as_bool).unwrap_or(false);
    switch_library(app, &path, demo)
}

/// Close the current library and open the one at `path`. If it can't be opened (locked by
/// another program, unreadable, a file), the error is returned and the current library stays open:
/// the target is checked before anything is closed, and if opening it still fails the previous
/// library is opened again (both errors are reported if that fails too). `demo`: a new library
/// starts with the procedural demo photos.
fn switch_library(app: &mut LccApp, path: &str, demo: bool) -> Result<Value, String> {
    let dir = app.session.check_library(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    // the library being left goes in the list too (also when it was opened at launch)
    let previous_dir = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let previous = previous_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    app.session.close_library().map_err(|e| e.to_string())?;
    let opened = app.session.open_library(&dir, demo).map(|_| ()).map_err(|e| format!("{path}: {e}"));
    app.lightroom_last = None;
    app.lightroom_result = None;
    if matches!(app.ui.dialog.as_ref(), Some(crate::state::Dialog::LightroomResult { .. })) {
        app.ui.dialog = None;
    }
    app.renderer.forget_all();
    if let Err(err) = opened {
        let Some(prev) = previous_dir else { return Err(err) };
        return match app.session.open_library(&prev, false) {
            Ok(_) => Err(err),
            Err(e) => Err(format!("{err}; reopening the previous catalog {} failed too: {e}", prev.display())),
        };
    }
    // Photo IDs are local to a catalog: pins cannot follow them into another library (a refused
    // or failed switch reopens the same catalog, so its pins stay).
    app.ui.reference = None;
    app.ui.compare = None;
    app.ui.brief_info = crate::state::BriefInfo::default();
    if app.ui.view == crate::state::ViewMode::Reference {
        app.ui.view = crate::state::ViewMode::PhotoGrid;
    }
    app.ui.settings.library_path = path.clone();
    if let Some(prev) = previous.filter(|p| p != &path && !app.ui.settings.recent_libraries.contains(p)) {
        remember_library(&mut app.ui.settings.recent_libraries, &prev);
    }
    remember_library(&mut app.ui.settings.recent_libraries, &path);
    Ok(json!({"path": path, "name": library_name(&dir), "photos": app.session.catalog.len()}))
}

/// Put `path` first in the recent list, without duplicates, capped at [`RECENT_LIBRARIES`].
pub fn remember_library(recent: &mut Vec<String>, path: &str) {
    // the same folder spelled differently (trailing slash…) is one library
    recent.retain(|r| std::path::Path::new(r) != std::path::Path::new(path));
    recent.insert(0, path.to_string());
    recent.truncate(crate::state::RECENT_LIBRARIES);
}

/// A library's name as people see it: its folder's name.
pub fn library_name(dir: &std::path::Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

/// The open library's name (`None`: in memory, nothing is saved).
pub fn open_library_name(app: &LccApp) -> Option<String> {
    app.session.library.as_ref().filter(|l| l.on_disk).map(|l| library_name(&l.dir))
}

/// `app.newLibrary {path?, name?}`: create a library and switch to it. With `name`, the library
/// is the folder `name` inside `path` (or inside a location chosen in a dialog); without it,
/// `path` itself, or a new folder named after the default library. The folder must not hold a
/// library already, nor anything else.
pub fn new_library(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(valid_library_name(s, "app.newLibrary")?),
        Some(_) => return Err("app.newLibrary: name must be a string".into()),
    };
    // from the menu: ask for the name first (default prefilled), then for the location
    if name.is_none() && p.get("path").is_none() && app.services.pick_folder.is_some() {
        crate::panels::dialogs::prompt(app, "New Catalog", "Catalog name", lcc_engine::library::DEFAULT_NAME, "app.newLibrary", json!({}), "name");
        return Ok(Value::Null);
    }
    let (location, picked) = match p.get("path").and_then(Value::as_str) {
        Some(x) => (std::path::PathBuf::from(x), false),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("New Catalog: Choose Where to Put It"));
            match crate::pick::ask(app, "app.newLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => (std::path::PathBuf::from(x), true),
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    let dir = match name {
        Some(n) => location.join(n),
        // a chosen location gets a new folder in it, named after the default library
        None if picked => free_name(&location, lcc_engine::library::DEFAULT_NAME),
        None => location,
    };
    if lcc_engine::library::is_library(&dir) {
        return Err(format!("{}: there is a catalog here already; open it with File ▸ Open Catalog…", dir.display()));
    }
    if dir.is_file() {
        return Err(format!("{}: is a file; a catalog is a folder", dir.display()));
    }
    if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{}: the folder isn't empty; choose an empty folder or a new name", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: can't create the catalog folder: {e}", dir.display()))?;
    switch_library(app, &dir.to_string_lossy(), false)
}

/// `app.renameLibrary {name?}`: give the open catalog's folder a new name, keeping it open. Without
/// `name`, asks for one (the current name pre-filled). The name is one folder name next to the
/// catalog (no path separators; the folder must not exist yet). The catalog is saved and closed, its
/// folder renamed, the new folder opened, the photos copied into the catalog's own `Originals/`
/// (stored by absolute path) pointed at the new place, and the recent list and launch library updated.
/// If anything fails before the rename the catalog is reopened untouched; if opening the renamed
/// folder fails the folder is renamed back and reopened. Returns `{path, name, previous, relinked}`.
pub fn rename_library(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    const C: &str = "app.renameLibrary";
    can_switch_library(app)?;
    let old = app
        .session
        .library
        .as_ref()
        .filter(|l| l.on_disk)
        .map(|l| l.dir.clone())
        .ok_or_else(|| format!("{C}: the open catalog isn't saved on disk, so there is no folder to rename"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => {
            crate::panels::dialogs::prompt(app, "Rename Catalog", "Catalog name", &library_name(&old), C, json!({}), "name");
            return Ok(Value::Null);
        }
        Some(Value::String(s)) => valid_library_name(s, C)?.to_string(),
        Some(_) => return Err(format!("{C}: name must be a string")),
    };
    let parent =
        old.parent().filter(|d| !d.as_os_str().is_empty()).ok_or_else(|| format!("{C}: {} has no parent folder to rename within", old.display()))?;
    let new = parent.join(&name);
    if name == library_name(&old) {
        return Err(format!("{C}: the catalog is already named {name:?}"));
    }
    // a case-only change on a case-insensitive disk names the same folder: that is fine
    let same_folder = matches!((std::fs::canonicalize(&old), std::fs::canonicalize(&new)), (Ok(a), Ok(b)) if a == b);
    if new.exists() && !same_folder {
        return Err(format!("{C}: {} exists already; choose another name", new.display()));
    }
    let _task = app.session.activity.start("renameCatalog", "Renaming the catalog", lcc_engine::activity::Cancel::No);
    // save and release the catalog (its log file and lock); on failure it stays open as it was
    app.session.close_library().map_err(|e| format!("{C}: the catalog couldn't be saved, so it was not renamed: {e}"))?;
    app.session.library = None;
    let reopen_old = |app: &mut LccApp| app.session.open_library(&old, false).map(|_| ()).map_err(|e| e.to_string());
    if let Err(e) = std::fs::rename(&old, &new) {
        let why = format!("{C}: can't rename {} to {name:?}: {e}", old.display());
        return Err(match reopen_old(app) {
            Ok(()) => why,
            Err(r) => format!("{why}; reopening the catalog failed too: {r}"),
        });
    }
    if let Err(e) = app.session.open_library(&new, false) {
        let why = format!("{C}: the renamed catalog {} didn't open: {e}", new.display());
        // put the folder back and open it as it was
        if let Err(back) = std::fs::rename(&new, &old) {
            return Err(format!("{why}; renaming it back failed too ({back}): the catalog is now at {}", new.display()));
        }
        return Err(match reopen_old(app) {
            Ok(()) => format!("{why}; the catalog was put back under its old name"),
            Err(r) => format!("{why}; reopening the old catalog failed too: {r}"),
        });
    }
    app.lightroom_last = None;
    app.lightroom_result = None;
    app.renderer.forget_all();
    app.ui.reference = None;
    app.ui.compare = None;
    // photos copied into the catalog (Originals/…) are stored by absolute path: point them at the new
    // folder, with what the catalog knows about those folders. (Not an undo step: it'd point back at nothing.)
    let (old_s, new_s) = (old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned());
    let (undo, redo) = (app.session.undo.len(), app.session.redo.len());
    let inside = app
        .session
        .catalog
        .photos()
        .any(|ph| matches!(&ph.source, lcc_catalog::Source::File { path } if std::path::Path::new(path).starts_with(&old)));
    let mut relinked = 0;
    let mut warning = None;
    if inside {
        match app.run("folder.relocate", json!({"path": old_s, "to": new_s})) {
            Ok(v) => relinked = v.get("relinked").and_then(Value::as_u64).unwrap_or(0),
            Err(e) => warning = Some(format!("the catalog was renamed, but its photos inside it could not be pointed at the new folder: {e}")),
        }
    }
    app.session.undo.truncate(undo);
    app.session.redo.truncate(redo);
    // the launch library and the recent list follow the rename (the old spelling is gone)
    let settings = &mut app.ui.settings;
    settings.recent_libraries.retain(|r| std::path::Path::new(r) != old);
    if let Some(when) = settings.recent_opened.remove(&old_s) {
        settings.recent_opened.insert(new_s.clone(), when);
    }
    app.recent_checks.forget(&old_s);
    settings.library_path = new_s.clone();
    remember_library(&mut settings.recent_libraries, &new_s);
    let mut out = json!({"path": new_s, "name": name, "previous": old_s, "relinked": relinked});
    if let Some(w) = warning {
        out["warning"] = json!(w);
    }
    Ok(out)
}

/// Characters a folder name can't hold on Windows (and `/`, which no system allows).
const RESERVED_NAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn valid_library_name<'a>(s: &'a str, command: &str) -> Result<&'a str, String> {
    let n = s.trim();
    // Windows drops trailing dots and spaces from a folder name: refuse them rather than
    // creating a folder whose name differs from the one asked for (spaces around the name
    // are trimmed first)
    let bad = n.is_empty() || n == "." || n == ".." || n.ends_with(['.', ' ']) || n.contains(RESERVED_NAME_CHARS) || n.chars().any(char::is_control);
    if bad {
        return Err(format!("{command}: {s:?} can't be a catalog name (a folder name without < > : \" / \\ | ? * and not ending in a dot or space)"));
    }
    Ok(n)
}

/// `base` inside `location`, or `base 2`, `base 3`… if that is taken.
pub(crate) fn free_name(location: &std::path::Path, base: &str) -> std::path::PathBuf {
    let first = location.join(base);
    if !first.exists() {
        return first;
    }
    (2..1000u32).map(|i| location.join(format!("{base} {i}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// `app.recentLibraries {remove?: path, clear?: bool}`: the recently opened libraries, newest
/// first, as `[{path, name, exists, checking, open, lastOpened}]`. `exists` is what the last
/// background check found (`false`: moved, on a disconnected drive, not a folder or no longer a
/// library; `null`: not checked yet, or the check couldn't tell — a folder that can't be listed, an
/// unreadable catalog, a lookup or check that failed; `checking` says whether a check is running): the command never
/// looks at a folder itself, as one on an offline network share can take seconds to answer.
/// `lastOpened`: ISO 8601 time, `null` when not known. `remove` forgets that
/// one entry (an error if the list doesn't hold it) and `clear` forgets them all; neither touches
/// anything on disk (issue #1290).
pub fn recent_libraries(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    match p.get("remove") {
        None | Some(Value::Null) => {}
        Some(Value::String(path)) => {
            if !forget_library(&mut app.ui.settings, path) {
                return Err(format!("app.recentLibraries: {path:?} is not in the recent catalogs"));
            }
            app.recent_checks.forget(path);
        }
        Some(_) => return Err("app.recentLibraries: remove must be a catalog path (a string)".into()),
    }
    if p.get("clear").and_then(Value::as_bool) == Some(true) {
        for path in std::mem::take(&mut app.ui.settings.recent_libraries) {
            app.recent_checks.forget(&path);
        }
        app.ui.settings.recent_opened.clear();
    }
    let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let (settings, checks) = (&app.ui.settings, &app.recent_checks);
    let list: Vec<Value> = settings
        .recent_libraries
        .iter()
        .map(|r| {
            let d = std::path::Path::new(r);
            let exists = checks.exists(r);
            json!({"path": r, "name": library_name(d), "exists": exists, "checking": checks.is_checking(r), "open": open.as_deref() == Some(d), "lastOpened": settings.recent_opened.get(r)})
        })
        .collect();
    Ok(Value::Array(list))
}

/// Forget `path` in File ▸ Open Recent Library (and when it was opened); the library itself is
/// left where it is. False if the list didn't hold it.
pub fn forget_library(settings: &mut crate::state::AppSettings, path: &str) -> bool {
    let before = settings.recent_libraries.len();
    settings.recent_libraries.retain(|r| r != path);
    settings.recent_opened.remove(path);
    settings.recent_libraries.len() != before
}

// ---------------------------------------------------------------------------- Display profile

/// `app.displayProfile {path?: string | null}`: show previews through this monitor ICC profile
/// (`""` or `null`: none, the profile of the screen showing the window where the host knows it
/// (macOS), else the display is treated as sRGB) and keep it as the setting; no `path`: report
/// only. A file that can't be used is an error and changes nothing. Returns the profile in use:
/// `{path, description, kind (matrix|lut), primaries {red, green, blue: [x, y]}}`, with
/// `path: null, screen: true` for the screen's profile.
pub fn display_profile(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let path = match p.get("path") {
        None => None,
        Some(Value::Null) => Some(String::new()),
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(_) => return Err("app.displayProfile: path must be a string or null".into()),
    };
    if let Some(path) = path {
        let d = lcc_engine::display::Display::load_opt(&path).map_err(|e| format!("app.displayProfile: {e}"))?;
        app.renderer.set_display(d);
        app.ui.settings.display_profile = path.clone();
        app.display_applied = Some(path);
        app.display_error = None;
        app.screen_profile = None;
        app.screen_profile_read = None;
        if app.ui.settings.display_profile.is_empty() {
            app.apply_screen_profile();
        }
    }
    Ok(match app.renderer.display() {
        Some(d) if d.path == crate::SCREEN_PROFILE => {
            let mut v = d.describe();
            v["path"] = Value::Null;
            v["screen"] = json!(true);
            v
        }
        Some(d) => d.describe(),
        None => json!({"path": null, "description": "sRGB (no display profile)"}),
    })
}

/// The disk cache's "In use": its size once counted, "counting…" while the first count of the folder runs on a
/// thread of its own (`PreviewCache::disk_size_now_with` is `Some(None)` meanwhile), "memory only" without a disk
/// cache.
fn in_use_text(used: Option<Option<u64>>) -> String {
    match used {
        Some(Some(b)) => format!("{:.1} MB", b as f64 / 1048576.0),
        Some(None) => crate::i18n::tr("counting…").to_string(),
        None => crate::i18n::tr("memory only").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{memory_choices, memory_size};

    /// The memory choices follow the computer: Automatic says what it is here, sizes go up to the most this computer
    /// allows (not 4 GB on every computer), and a size set elsewhere stays choosable.
    #[test]
    fn memory_choices_follow_the_computer() {
        let gb = 1usize << 30;
        let o = memory_choices(0, 16 * gb, 48 * gb);
        let values: Vec<u32> = o.iter().map(|(v, _)| *v).collect();
        assert_eq!(values, [0, 512, 1024, 2048, 4096, 8192, 16_384, 32_768], "up to three quarters of a 64 GB computer");
        assert!(o[0].1.ends_with("(16 GB)"), "Automatic says what it is here: {}", o[0].1);
        let small = memory_choices(0, 2 * gb, 6 * gb);
        assert_eq!(small.last().map(|(v, _)| *v), Some(4096));
        let other = memory_choices(20_480, 2 * gb, 6 * gb);
        assert_eq!(other.last().map(|(v, l)| (*v, l.as_str())), Some((20_480, "20 GB")), "a size from elsewhere stays");
        assert_eq!(memory_size(1536), "1.5 GB");
        assert_eq!(memory_size(16_300), "16 GB", "a quarter of a computer's 64 GB");
        assert_eq!(memory_size(512), "512 MB");
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn in_use_reads_counting_until_the_size_is_known() {
        assert_eq!(in_use_text(Some(None)), "counting…");
        assert_eq!(in_use_text(Some(Some(3 * 1048576 / 2))), "1.5 MB");
        assert_eq!(in_use_text(None), "memory only");
    }

    fn app_with_import() -> LccApp {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        app.run("photo.setReference", json!({})).unwrap();
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        app
    }

    #[test]
    fn open_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let pin = app.ui.reference;
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
        assert_eq!(app.ui.reference, pin, "a refused Open Library keeps the reference pin");
    }

    #[test]
    fn new_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let pin = app.ui.reference;
        let err = new_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
        assert_eq!(app.ui.reference, pin, "a refused New Library keeps the reference pin");
    }

    /// The menu items that switch libraries are greyed out meanwhile, like during a Lightroom import.
    #[test]
    fn library_switching_menu_items_are_disabled_while_an_import_runs() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.services.pick_folder = Some(Box::new(|| None));
        assert!(crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(crate::menus::ui_enabled(&app, "app.newLibrary"));
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        assert!(!crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(!crate::menus::ui_enabled(&app, "app.newLibrary"));
    }

    /// The folder scan before an import's review counts too: it ends in an import into the open library.
    #[test]
    fn open_library_is_refused_while_an_import_scan_runs() {
        let dir = lcc_testkit::TempDir::new("lc-ui-switchscan");
        let _ = std::fs::create_dir_all(&dir);
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        app.run("photo.setReference", json!({})).unwrap();
        let pin = app.ui.reference;
        crate::import::open(&mut app, vec![dir.to_string_lossy().into_owned()]).unwrap();
        assert!(app.scan.is_some());
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert_eq!(app.ui.reference, pin, "a refused Open Library during a scan keeps the reference pin");
        let err = new_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert_eq!(app.ui.reference, pin, "a refused New Library during a scan keeps the reference pin");
        assert!(app.scan.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod path_tests {
    use super::elide_path;

    /// Width = characters, so a limit of 20 is "20 characters".
    fn fit(path: &str, max: usize) -> String {
        elide_path(path, max as f32, |s| s.chars().count() as f32)
    }

    /// A path that fits is left alone; one that doesn't loses its leading folders and keeps its end
    /// (with either separator: `Path::display` gives backslashes on Windows); a last folder too long
    /// by itself loses its end, on a character boundary; a separator at the end is not where the
    /// name starts.
    #[test]
    fn a_long_path_loses_its_leading_folders_and_keeps_its_end() {
        for (path, max, want) in [
            ("/Users/me/Pictures", 18, "/Users/me/Pictures"),
            ("", 5, ""),
            ("/Users/me/Pictures/Library", 20, "…/Pictures/Library"),
            ("/Users/me/Pictures/Library", 10, "…/Library"),
            (r"C:\Users\me\Pictures\Library", 20, r"…\Pictures\Library"),
            (r"C:\Users\me/Pictures\Library", 10, r"…\Library"),
            ("/Users/me/a-very-long-folder-name", 8, "a-very-…"),
            ("no-separator-at-all", 8, "no-sepa…"),
            ("/Users/Фотографии-из-отпуска", 8, "Фотогра…"),
            ("/Users/me/Pictures/Library/", 11, "…/Library/"),
            ("/Users/me/a-very-long-folder-name/", 8, "a-very-…"),
            (r"C:\Users\me\Library\", 11, r"…\Library\"),
            ("///", 2, "…"),
            (r"\\server\share\folder", 15, r"…\share\folder"),
        ] {
            assert_eq!(fit(path, max), want, "{path:?} in {max}");
        }
    }
}
