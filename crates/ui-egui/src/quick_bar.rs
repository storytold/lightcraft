//! The quick settings bar: a row of view switches in the module bar's blank stretch between the
//! identity plate (or the activity area's progress bars) and the module picker. It fades in over
//! [`FADE_SECS`] while the pointer is in that stretch and out again when it leaves; hovering there
//! dismisses the status line's message at once, so a warning never sits under the bar.
//!
//! Every entry is a command (`view.infoOverlay`, `view.eyeFocus`, …): a click runs it exactly as the
//! menu, a key or the control channel would. A new quick setting is one row in [`ENTRIES`]: its id,
//! label, command, glyph and how to read its state. Plug-ins add buttons of their own: a command
//! declared with `"quickAction": true` in the plug-in's manifest shows after the built-in entries
//! and runs as File ▸ Plug-in Extras would (`plugins.runItem`; see `docs/plugins.md`).
//!
//! A feature turned off from somewhere else (the eye focus strip's ✕) can [`flash`] its entry: the
//! bar stays hidden but that one button blinks twice where it lives, so the user learns where to
//! turn it back on.
//!
//! Commands: `view.quickSettings` (the entries and their state, `{flash: id}` blinks one),
//! `view.changeOverlay` (the on-screen overlay that reports every change, on or off),
//! `view.dismissEyeFocus` (the strip's ✕) and `view.zoomToEye` (the strip's eye glyph).
//!
//! The glyphs are [`crate::icons`] plus a clipping glyph drawn here (original work).

use std::f32::consts::TAU;

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::Icon;
use crate::menus::UiCommand;
use crate::theme::Tokens;
use crate::widgets::register;

/// Seconds the bar takes to fade in, and again to fade out.
pub const FADE_SECS: f32 = 0.2;
/// Seconds a flashed entry blinks for.
pub const FLASH_SECS: f64 = 1.6;
/// How many times it blinks in that time.
const FLASHES: f64 = 2.0;
/// A button's side (points).
const BUTTON: f32 = 24.0;
/// Space between buttons.
const GAP: f32 = 3.0;
/// The bar's padding around its buttons.
const PAD: f32 = 3.0;
/// The room kept between the stretch's ends and what is beside it.
pub const MARGIN: f32 = 12.0;

/// What a built-in entry's glyph is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Icon(Icon),
    /// Two corner triangles, as the histogram's clipping warnings.
    Clipping,
}

/// How an entry reads now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shows {
    /// Highlighted as switched on.
    pub on: bool,
    /// What it is set to, in English (translated where shown).
    pub value: &'static str,
}

impl Shows {
    fn switch(on: bool) -> Shows {
        Shows { on, value: if on { "On" } else { "Off" } }
    }
}

/// A built-in quick setting.
pub struct Entry {
    /// Stable id: the widget is `quick:<id>`.
    pub id: &'static str,
    /// What it is (English; translated where shown).
    pub label: &'static str,
    /// The command a click runs (with no parameters: each of them toggles or cycles).
    pub command: &'static str,
    pub glyph: Glyph,
    /// How it reads now.
    pub state: fn(&LccApp) -> Shows,
}

/// The built-in quick settings, left to right. Add a row here to add one.
pub const ENTRIES: &[Entry] = &[
    Entry { id: "info", label: "Info Overlay", command: "view.infoOverlay", glyph: Glyph::Icon(Icon::Info), state: info_state },
    Entry { id: "overlay", label: "On-Screen Overlay", command: "view.changeOverlay", glyph: Glyph::Icon(Icon::Chat), state: overlay_state },
    Entry {
        id: "eyes",
        label: "Eye Focus Strip",
        command: "view.eyeFocus",
        glyph: Glyph::Icon(Icon::Eye),
        state: |a| Shows::switch(a.eye_focus_on()),
    },
    Entry { id: "clipping", label: "Show Clipping", command: "view.clipping", glyph: Glyph::Clipping, state: |a| Shows::switch(a.ui.show_clipping) },
    Entry {
        id: "focusPoint",
        label: "Show Focus Point",
        command: "view.focusPoint",
        glyph: Glyph::Icon(Icon::Target),
        state: |a| Shows::switch(a.ui.settings.focus_point),
    },
    Entry {
        id: "faceBoxes",
        label: "Face Boxes",
        command: "view.faceBoxes",
        glyph: Glyph::Icon(Icon::FaceBox),
        state: |a| Shows::switch(a.ui.face_boxes),
    },
    Entry {
        id: "filmstrip",
        label: "Filmstrip",
        command: "view.filmstrip",
        glyph: Glyph::Icon(Icon::Filmstrip),
        state: |a| Shows::switch(a.ui.filmstrip),
    },
];

fn info_state(app: &LccApp) -> Shows {
    use crate::state::InfoOverlay as I;
    match app.ui.info_overlay {
        I::Off => Shows { on: false, value: "Off" },
        I::Basic => Shows { on: true, value: "Info 1" },
        I::Exposure => Shows { on: true, value: "Info 2" },
    }
}

fn overlay_state(app: &LccApp) -> Shows {
    Shows::switch(crate::shortcuts::overlay_reports_changes(app))
}

/// The commands this module adds.
pub const COMMANDS: &[UiCommand] = &[
    ("view.quickSettings", "Quick Settings", None, ""),
    ("view.changeOverlay", "On-Screen Overlay", None, "View>Loupe Info"),
    ("view.dismissEyeFocus", "Hide Eye Focus Strip", None, ""),
    ("view.zoomToEye", "Zoom to Eye", None, ""),
];

/// What the bar keeps between frames.
#[derive(Debug, Default)]
pub struct QuickBar {
    /// The entry blinking, and since when (egui time, s).
    flash: Option<(String, f64)>,
    /// An entry asked to blink by a command (which has no clock): it starts with the next frame.
    flash_pending: Option<String>,
    /// How far the bar is faded in (0 hidden … 1 shown), last frame.
    alpha: f32,
}

impl QuickBar {
    /// The entry blinking now (or about to), if any.
    pub fn flashing(&self) -> Option<&str> {
        self.flash_pending.as_deref().or(self.flash.as_ref().map(|(id, _)| id.as_str()))
    }

    /// Whether the bar is (partly) shown.
    pub fn shown(&self) -> bool {
        self.alpha > 0.0
    }
}

/// Blink entry `id` twice while the bar stays hidden.
pub fn flash(app: &mut LccApp, id: &str) {
    app.quick_bar.flash_pending = Some(id.to_string());
    if let Some(ctx) = app.tasks.repaint.as_ref() {
        ctx.request_repaint();
    }
}

/// One button in the bar.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    BuiltIn(usize),
    /// A plug-in's quick action: plug-in id, command id, its label.
    Plugin {
        plugin: String,
        command: String,
        label: String,
    },
}

impl Item {
    fn id(&self) -> String {
        match self {
            Item::BuiltIn(i) => ENTRIES.get(*i).map_or_else(String::new, |e| e.id.to_string()),
            Item::Plugin { plugin, command, .. } => format!("plugin:{plugin}/{command}"),
        }
    }
}

/// The plug-ins' quick actions: (plug-in, command, label) of enabled plug-ins, in install order.
#[cfg(not(target_arch = "wasm32"))]
pub fn plugin_actions() -> Vec<(String, String, String)> {
    let g = lcc_engine::cmd::plugins::manager();
    g.as_ref()
        .into_iter()
        .flat_map(|m| m.list())
        .filter(|p| p.enabled)
        .flat_map(|p| {
            let m = p.plugin.manifest();
            m.commands.iter().filter(|c| c.quick_action).map(|c| (m.id.clone(), c.id.clone(), c.label.clone())).collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
pub fn plugin_actions() -> Vec<(String, String, String)> {
    Vec::new()
}

fn items() -> Vec<Item> {
    (0..ENTRIES.len())
        .map(Item::BuiltIn)
        .chain(plugin_actions().into_iter().map(|(plugin, command, label)| Item::Plugin { plugin, command, label }))
        .collect()
}

/// The stretch of the module bar the bar lives in: from after the plate (or the activity area) to
/// before the module picker. `None` when there is no room for a button.
pub fn zone(full: Rect, plate_right: f32, picker_left: f32) -> Option<Rect> {
    let z = Rect::from_min_max(pos2(plate_right + MARGIN, full.top()), pos2(picker_left - MARGIN, full.bottom()));
    (z.is_finite() && z.width() >= BUTTON + 2.0 * PAD).then_some(z)
}

/// Whether the pointer is in the bar's stretch (nothing on top of it). Hovering there dismisses the
/// status line's message: the bar takes its place.
pub fn hover(app: &mut LccApp, ui: &egui::Ui, zone: Option<Rect>) -> bool {
    let hovered = zone.is_some_and(|z| ui.rect_contains_pointer(z));
    if hovered && !app.ui.status.is_empty() {
        app.ui.status.clear();
    }
    hovered
}

/// The bar's buttons that fit in `zone`, right-aligned: `(item, rect)`, and the bar's rect.
fn layout(items: Vec<Item>, zone: Rect) -> (Vec<(Item, Rect)>, Rect) {
    let fit = (((zone.width() - 2.0 * PAD + GAP) / (BUTTON + GAP)).floor().max(0.0)) as usize;
    let n = items.len().min(fit);
    let w = n as f32 * BUTTON + n.saturating_sub(1) as f32 * GAP + 2.0 * PAD;
    let h = BUTTON + 2.0 * PAD;
    let bar = Rect::from_min_size(pos2(zone.right() - w, zone.center().y - h / 2.0), vec2(w, h));
    let placed = items
        .into_iter()
        .take(n)
        .enumerate()
        .map(|(i, item)| {
            let r = Rect::from_min_size(pos2(bar.left() + PAD + i as f32 * (BUTTON + GAP), bar.top() + PAD), vec2(BUTTON, BUTTON));
            (item, r)
        })
        .collect();
    (placed, bar)
}

/// How bright a flashed entry is `elapsed` seconds into its blinking: two soft pulses from and back
/// to nothing.
pub fn pulse(elapsed: f64) -> f32 {
    if !(0.0..FLASH_SECS).contains(&elapsed) {
        return 0.0;
    }
    (0.5 - 0.5 * (f64::from(TAU) * FLASHES * elapsed / FLASH_SECS).cos()) as f32
}

/// Draw the bar in `zone` (from [`zone`]) as faded in for `hovered` (from [`hover`]), and run what is
/// clicked.
pub fn show(app: &mut LccApp, ui: &mut egui::Ui, zone: Option<Rect>, hovered: bool) {
    let ctx = ui.ctx().clone();
    let now = ui.input(|i| i.time);
    if let Some(id) = app.quick_bar.flash_pending.take() {
        app.quick_bar.flash = Some((id, now));
    }
    if hovered || app.quick_bar.flash.as_ref().is_some_and(|(_, since)| now - since >= FLASH_SECS) {
        app.quick_bar.flash = None;
    }
    let alpha = ctx.animate_bool_with_time(egui::Id::new("quick-settings-bar"), hovered, FADE_SECS);
    app.quick_bar.alpha = alpha;
    let Some(zone) = zone else { return };
    let flash = app.quick_bar.flash.clone();
    if alpha <= 0.0 && flash.is_none() {
        return;
    }
    crate::surface::ensure_loaded(app);
    let t = Tokens::get(&ctx);
    let (placed, bar) = layout(items(), zone);
    let p = ui.painter().clone();
    if alpha > 0.0 {
        p.rect_filled(bar, 6.0, t.inset.gamma_multiply(alpha));
        p.rect_stroke(bar, 6.0, Stroke::new(1.0, t.divider.gamma_multiply(alpha)), egui::StrokeKind::Inside);
        register(&ctx, "region:quickBar", bar);
    }
    let mut clicked: Option<Item> = None;
    for (item, r) in placed {
        let id = item.id();
        let flashing = flash.as_ref().filter(|(f, _)| *f == id).map(|(_, since)| pulse(now - since));
        let a = alpha.max(flashing.unwrap_or(0.0));
        if a <= 0.0 && flashing.is_none() {
            continue;
        }
        register(&ctx, format!("quick:{id}"), r);
        let (label, shows) = match &item {
            Item::BuiltIn(i) => match ENTRIES.get(*i) {
                Some(e) => (crate::i18n::tr(e.label).to_string(), Some((e.state)(app))),
                None => continue,
            },
            Item::Plugin { label, .. } => (label.clone(), None),
        };
        let resp = ui.interact(r, egui::Id::new(("quick-setting", &id)), if a >= 0.5 { Sense::click() } else { Sense::hover() });
        crate::access::named(&resp, egui::WidgetType::Button, &label);
        if flashing.is_some() {
            // alone on the bar's chrome: a backing and the accent ring, so it reads at a glance
            p.rect_filled(r.expand(PAD), 6.0, t.inset.gamma_multiply(a));
            p.rect_stroke(r.expand(PAD), 6.0, Stroke::new(1.5, t.accent.gamma_multiply(a)), egui::StrokeKind::Inside);
        }
        let on = shows.is_some_and(|s| s.on);
        if on {
            p.rect_filled(r, 4.0, t.accent.gamma_multiply(0.35 * a));
        }
        if resp.hovered() && a >= 0.5 {
            p.rect_filled(r, 4.0, t.hover.gamma_multiply(a));
        }
        let colour = if on || resp.hovered() { t.text } else { t.text_dim };
        let colour = colour.gamma_multiply(a);
        let glyph = Rect::from_center_size(r.center(), vec2(16.0, 16.0));
        match &item {
            Item::BuiltIn(i) => {
                if let Some(e) = ENTRIES.get(*i) {
                    paint_glyph(&p, glyph, e.glyph, colour);
                    // the info overlay says which Info it shows
                    if let Some(n) = shows.and_then(|s| s.value.strip_prefix("Info ")) {
                        p.text(r.right_bottom() - vec2(3.0, 1.0), Align2::RIGHT_BOTTOM, n, t.semibold(9.0), colour);
                    }
                }
            }
            Item::Plugin { plugin, .. } => {
                if a < 1.0 {
                    p.rect_filled(glyph, 2.0, t.inset.gamma_multiply(1.0 - a));
                }
                paint_plugin_icon(ui, glyph, plugin);
            }
        }
        let tip = match shows {
            Some(s) => format!("{label}\n{}", crate::i18n::tr(s.value)),
            None => label,
        };
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            clicked = Some(item);
        }
    }
    if (alpha > 0.0 && alpha < 1.0) || flash.is_some() {
        ctx.request_repaint();
    }
    let Some(item) = clicked else { return };
    let r = match item {
        Item::BuiltIn(i) => match ENTRIES.get(i) {
            Some(e) => app.run(e.command, json!({})),
            None => return,
        },
        Item::Plugin { plugin, command, .. } => app.run("plugins.runItem", json!({"plugin": plugin, "command": command})),
    };
    if let Err(e) = r {
        app.toast_error(&ctx, e);
    }
}

/// A plug-in's icon (its own, else its identicon).
#[cfg(not(target_arch = "wasm32"))]
fn paint_plugin_icon(ui: &egui::Ui, r: Rect, plugin: &str) {
    crate::panels::plugin_icon::paint_for(ui, r, plugin);
}

/// The web build runs no plug-ins: there is never one to draw.
#[cfg(target_arch = "wasm32")]
fn paint_plugin_icon(_ui: &egui::Ui, _r: Rect, _plugin: &str) {}

/// Paint a built-in entry's glyph in `r`.
fn paint_glyph(p: &egui::Painter, r: Rect, glyph: Glyph, colour: Color32) {
    match glyph {
        Glyph::Icon(icon) => crate::icons::paint(p, r, icon, colour),
        Glyph::Clipping => {
            // a frame with its two top corners filled: shadow clipping on the left, highlights on the right
            let s = Stroke::new(1.5, colour);
            let f = r.shrink(1.5);
            p.rect_stroke(f, 2.0, s, egui::StrokeKind::Middle);
            let k = f.width() * 0.42;
            p.add(egui::Shape::convex_polygon(vec![f.left_top(), pos2(f.left() + k, f.top()), pos2(f.left(), f.top() + k)], colour, Stroke::NONE));
            p.add(egui::Shape::convex_polygon(vec![pos2(f.right() - k, f.top()), f.right_top(), pos2(f.right(), f.top() + k)], colour, Stroke::NONE));
        }
    }
}

/// The quick bar's commands.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r = match id {
        "view.quickSettings" => {
            // {flash?: id} blinks one entry (as the eye strip's ✕ does); always → the entries
            if let Some(f) = p.get("flash") {
                let Some(f) = f.as_str().filter(|f| items().iter().any(|i| i.id() == *f)) else {
                    return Some(Err(format!(
                        "view.quickSettings: no quick setting `{}`",
                        f.as_str().unwrap_or_default().chars().take(80).collect::<String>()
                    )));
                };
                flash(app, f);
            }
            Ok(state(app))
        }
        "view.changeOverlay" => {
            // {on?: bool} (toggles when omitted) → {on}: the overlay reports every change (Always) or none (Off)
            crate::surface::ensure_loaded(app);
            let on = match p.get("on") {
                None | Some(Value::Null) => !crate::shortcuts::overlay_reports_changes(app),
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => return Some(Err("view.changeOverlay: on is true or false".into())),
                },
            };
            let mode = if on { lcc_surface::settings::OsdMode::Always } else { lcc_surface::settings::OsdMode::Off };
            app.surface.runtime.settings_mut().osd.set_mode(mode);
            app.surface.changed();
            Ok(json!({"on": on}))
        }
        "view.dismissEyeFocus" => {
            // the strip's ✕: off where the user is, and the bar shows where to turn it back on
            app.set_eye_focus(false);
            flash(app, "eyes");
            Ok(json!({"on": false}))
        }
        "view.zoomToEye" => zoom_to_eye(app, p),
        _ => return None,
    };
    Some(r)
}

/// `view.zoomToEye {index?}`: the loupe at 100 % on eye `index` (default the first) of the photo the
/// eye strip shows.
fn zoom_to_eye(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let index = match p.get("index") {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or("view.zoomToEye: index is a whole number")?,
    };
    let report = app.eye_key.and_then(|k| app.eye_strip.report(k)).ok_or("view.zoomToEye: no eyes found on this photo yet")?;
    let eye = report.eyes.get(index).ok_or_else(|| format!("view.zoomToEye: this photo has {} eyes to show", report.eyes.len()))?;
    let center = eye.center;
    crate::eye_strip::zoom_to(app, center);
    Ok(json!({"index": index, "center": center}))
}

/// The entries and their state, for `view.quickSettings` and `ui.inspect`.
pub fn state(app: &LccApp) -> Value {
    let entries: Vec<Value> = items()
        .into_iter()
        .map(|item| match item {
            Item::BuiltIn(i) => match ENTRIES.get(i) {
                Some(e) => {
                    let s = (e.state)(app);
                    json!({"id": e.id, "label": e.label, "command": e.command, "on": s.on, "value": s.value})
                }
                None => Value::Null,
            },
            Item::Plugin { plugin, command, label } => {
                json!({"id": format!("plugin:{plugin}/{command}"), "label": label, "command": "plugins.runItem", "params": {"plugin": plugin, "command": command}})
            }
        })
        .collect();
    json!({"entries": entries, "shown": app.quick_bar.shown(), "flashing": app.quick_bar.flashing()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flash_blinks_twice_and_ends_dark() {
        assert_eq!(pulse(0.0), 0.0);
        assert!(pulse(FLASH_SECS / 4.0) > 0.99, "the first blink peaks");
        assert!(pulse(FLASH_SECS / 2.0) < 0.01, "dark between the blinks");
        assert!(pulse(FLASH_SECS * 0.75) > 0.99, "the second blink peaks");
        assert_eq!(pulse(FLASH_SECS), 0.0);
        assert_eq!(pulse(-1.0), 0.0);
        assert_eq!(pulse(f64::NAN), 0.0);
    }

    #[test]
    fn the_bar_sits_at_the_right_of_its_stretch_and_drops_what_does_not_fit() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 44.0));
        let z = zone(full, 300.0, 900.0).unwrap();
        // the built-in entries (another test may be installing a plug-in meanwhile)
        let built_in = || (0..ENTRIES.len()).map(Item::BuiltIn).collect::<Vec<_>>();
        let (placed, bar) = layout(built_in(), z);
        assert_eq!(placed.len(), ENTRIES.len());
        assert!((bar.right() - z.right()).abs() < 0.01 && bar.left() > z.left(), "{bar:?} in {z:?}");
        assert!(placed.windows(2).all(|w| w[0].1.right() < w[1].1.left()), "left to right, apart");
        let narrow = Rect::from_min_max(pos2(0.0, 0.0), pos2(3.0 * (BUTTON + GAP) + 2.0 * PAD, 44.0));
        let (placed, bar) = layout(built_in(), narrow);
        assert_eq!(placed.len(), 3);
        assert!(bar.left() >= narrow.left() - 0.01);
        assert!(zone(full, 880.0, 900.0).is_none(), "no room, no bar");
        assert!(zone(full, f32::NAN, 900.0).is_none());
    }
}
