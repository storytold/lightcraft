//! The on-screen overlay (OSD): a pill in the window that reports what just changed, as
//! Lightroom's does: the value a slider or dial moves ("Exposure +0.33", "on 56 photos" with
//! Auto Sync), a rating, flag or label ("Picked", "3 stars" on 12 photos), a profile applied, an
//! undo… and a control surface's own readout (the value a dial turns, the command a key ran, a
//! page or profile change).
//!
//! Every command that changes something reports itself in the shared dispatch
//! (`lcc_engine::change_report`, filled by `Session::execute`), whatever ran it: keyboard,
//! mouse, menus, sliders, a console or an agent. This module turns the latest report into words
//! and an icon (the control-surface icon pack). A slider dragged updates the same pill with its
//! running value. One look for both: Settings, Interface, Overlay (`osd.*` commands): what it
//! reports (Off, Control surface only, Always), its size, opacity, how long it stays, where it
//! sits and its colour. Brief and never in the way: it takes no input and fades.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: TactileBridge's OSD overlay
//! (`OsdOverlay.cs`): its pill shape, layout, positions, opacity, colour and fade; its settings
//! (SettingsForm's OSD tab).

use egui::{Align2, Color32, Rect, pos2, vec2};
use lcc_catalog::Flag;
use lcc_engine::change_report::{ChangeKind, ChangeReport};
use lcc_surface as ls;
use ls::runtime::Shown;
use ls::settings::{OsdMode, OsdPosition};
use serde_json::{Value, json};

use super::settings::{choices, hint, row};
use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::theme::Tokens;

/// The pill's size (points) at size 1, as TactileBridge's.
const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 72.0;
/// The extra height of the line naming how many photos changed.
const CAPTION_H: f32 = 18.0;
/// The fade after the display time (s).
const FADE: f64 = 0.4;

/// What the pill says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readout {
    /// The name ("Exposure", "Picked", "Undo: Exposure").
    pub title: String,
    /// The value, when it reports one ("+0.33").
    pub value: String,
    /// The value's place in its range (0…1), drawn as a bar.
    pub bar: Option<f64>,
    /// The icon (the control-surface pack's), when it has one.
    pub icon: Option<&'static str>,
    /// How many photos it changed, when more than one ("on 56 photos").
    pub caption: String,
}

/// A change report shown in the window, and when it came (app clock, s).
#[derive(Clone, Debug)]
pub struct ActionShown {
    pub report: ChangeReport,
    pub at: f64,
}

/// The surface's own readout: (readout, since when in s).
pub fn surface_content(app: &LccApp) -> Option<(Readout, f64)> {
    let now_ms = app.surface.now() * 1000.0;
    let (shown, at) = app.surface.runtime.shown(now_ms)?;
    let r = match shown {
        Shown::Value { target } => {
            let spec = ls::targets::target_spec(target)?;
            let v = crate::surface::value(app, target);
            let title = tr(spec.label).to_string();
            // a mask's value; with none selected, the next mask's (note 24: "New mask · Exposure")
            let title = match spec.kind {
                ls::TargetKind::Mask if app.session.active_mask.is_none() => format!("{} · {}", tr("New mask"), title),
                ls::TargetKind::Mask => format!("{} · {}", tr("Mask"), title),
                _ => title,
            };
            let icon = ls::icons::icon_for(&ls::Binding::value(target));
            Readout { title, value: v.map(|v| spec.format(v)).unwrap_or_default(), bar: v.map(|v| spec.position(v)), icon, caption: String::new() }
        }
        // a command without a short label of its own names itself by id: show its registry label
        Shown::Command { label, icon } => {
            let text = if !label.contains(' ') && label.contains('.') {
                crate::surface::registry_label(app, label).unwrap_or_else(|| label.clone())
            } else {
                tr(label).to_string()
            };
            Readout { title: text, icon: *icon, ..Default::default() }
        }
        Shown::Notice { text } => Readout { title: notice_text(text), ..Default::default() },
    };
    Some((r, at / 1000.0))
}

/// Notices come in English from the engine ("Dial: Exposure", "Apply to all selected: On"):
/// translate their parts.
fn notice_text(text: &str) -> String {
    match text.split_once(": ") {
        Some((head, tail)) => {
            let tail = match tail {
                "On" | "Off" | "page" => tr(tail).to_string(),
                t => ls::targets::all_targets().into_iter().find(|s| s.label == t).map_or_else(|| t.to_string(), |s| tr(s.label).to_string()),
            };
            format!("{}: {tail}", tr(head))
        }
        None => tr(text).to_string(),
    }
}

/// "on 56 photos" (nothing for one photo).
fn photos_caption(n: usize) -> String {
    if n > 1 { tr_format!("on {n} photos", n = n) } else { String::new() }
}

/// The name of profile `id` as the profile browser shows it.
fn profile_name(app: &LccApp, id: &str) -> String {
    if let Some(p) = lcc_engine::presets::PROFILES.iter().find(|p| p.id == id) {
        return crate::i18n::profile_label(id, p.name).to_string();
    }
    if let Some(p) = app.session.lut_profiles.iter().find(|p| p.id == id) {
        return p.name.clone();
    }
    id.to_string()
}

fn command_icon(command: &str, params: Value) -> Option<&'static str> {
    ls::icons::icon_for(&ls::Binding::command_with(command, params))
}

/// What a change report says in the window.
pub fn describe(app: &LccApp, r: &ChangeReport) -> Readout {
    let caption = photos_caption(r.photos);
    // a value: its name and where it is now, on the photo being edited
    if let Some(control) = r.control.as_deref()
        && let Some(spec) = ls::targets::target_spec(control)
    {
        let v = crate::surface::value(app, control);
        return Readout {
            title: tr(spec.label).to_string(),
            value: v.map(|v| spec.format(v)).unwrap_or_default(),
            bar: v.map(|v| spec.position(v)),
            icon: ls::icons::icon_for(&ls::Binding::value(control)),
            caption,
        };
    }
    match r.command.as_str() {
        "edit.undo" | "edit.redo" => {
            let what = r.detail.as_deref().map(|d| crate::i18n::history_label(d, &app.session.presets)).unwrap_or_default();
            let title = match (r.command == "edit.undo", what.is_empty()) {
                (true, true) => tr("Undo").to_string(),
                (false, true) => tr("Redo").to_string(),
                (true, false) => tr_format!("Undo: {what}", what = what),
                (false, false) => tr_format!("Redo: {what}", what = what),
            };
            let icon = command_icon(&r.command, json!({}));
            return Readout { title, icon, ..Default::default() };
        }
        "develop.profile" => {
            let name = r.params.get("id").and_then(Value::as_str).map(|id| profile_name(app, id)).unwrap_or_default();
            let title = if r.photos > 1 {
                tr_format!("Profile {name} applied to {n} photos", name = name, n = r.photos)
            } else {
                tr_format!("Profile {name}", name = name)
            };
            return Readout { title, ..Default::default() };
        }
        _ => {}
    }
    match r.kind {
        ChangeKind::Rating(n) => Readout {
            title: match n {
                0 => tr("No stars").to_string(),
                1 => tr("1 star").to_string(),
                n => tr_format!("{n} stars", n = n),
            },
            icon: command_icon("photo.rate", json!({"rating": n})),
            caption,
            ..Default::default()
        },
        ChangeKind::Flag(f) => {
            let (title, cmd) = match f {
                Flag::Pick => ("Picked", "photo.pick"),
                Flag::Reject => ("Rejected", "photo.reject"),
                Flag::None => ("Unflagged", "photo.unflag"),
            };
            Readout { title: tr(title).to_string(), icon: command_icon(cmd, json!({})), caption, ..Default::default() }
        }
        ChangeKind::Label(l) => {
            let title = match l {
                Some(c) => tr_format!("Label: {name}", name = crate::i18n::color_label(&app.session.catalog, c)),
                None => tr("No label").to_string(),
            };
            let name = l.map(|c| format!("{c:?}").to_lowercase());
            Readout { title, icon: command_icon("photo.label", json!({"label": name})), caption, ..Default::default() }
        }
        ChangeKind::Develop | ChangeKind::Other => Readout {
            title: crate::i18n::history_label(&r.label, &app.session.presets),
            icon: command_icon(&r.command, r.params.clone()),
            caption,
            ..Default::default()
        },
    }
}

/// Take the newest change report, when the mode shows them (each once).
fn take_report(app: &mut LccApp) {
    let Some(r) = app.session.last_change.as_ref() else { return };
    if r.seq == app.surface.osd_seq {
        return;
    }
    app.surface.osd_seq = r.seq;
    if app.surface.settings().osd.effective_mode() != OsdMode::Always {
        return;
    }
    let report = r.clone();
    let at = app.surface.now();
    app.surface.action = Some(ActionShown { report, at });
}

/// What the pill shows now and its age (s): the newest of the surface's readout and the last
/// change (the surface's wins a tie: a dial's readout names the value it turns, with its bar).
pub fn current(app: &mut LccApp) -> Option<(Readout, f64)> {
    take_report(app);
    let now = app.surface.now();
    let surface = surface_content(app);
    let mode = app.surface.settings().osd.effective_mode();
    let action = app.surface.action.as_ref().filter(|_| mode == OsdMode::Always).map(|a| (describe(app, &a.report), a.at));
    let (r, at) = match (surface, action) {
        (Some(s), Some(a)) => {
            if a.1 > s.1 + 0.05 {
                a
            } else {
                s
            }
        }
        (Some(s), None) => s,
        (None, Some(a)) => a,
        (None, None) => return None,
    };
    Some((r, (now - at).max(0.0)))
}

/// Whether an on-photo tool is in use: cropping, brushing, healing or cloning, drawing a mask, a picker or targeted
/// adjustment armed, or a drag on the photo going on. The overlay stays off the photo then: "The photographer needs to
/// see the pixels he is working with" (test notes 2026-10-24 note 15); the tool's own readout (the brush cursor, the
/// crop's angle by the pointer) says what changes. Sliders in the panel still show it.
pub fn on_photo_tool(app: &LccApp, ctx: &egui::Context) -> bool {
    use crate::state::RightPanel;
    let photo_view = matches!(app.ui.view, crate::state::ViewMode::Detail);
    if !photo_view {
        return false;
    }
    if !app.ui.tool.is_empty() || matches!(app.ui.right, RightPanel::Crop | RightPanel::Remove | RightPanel::RedEye) {
        return true;
    }
    // a drag that started on the photo (panning, a mask handle, a pin)
    let canvas = app.canvas_rect;
    ctx.input(|i| i.pointer.any_down() && i.pointer.press_origin().zip(canvas).is_some_and(|(at, c)| c.contains(at)))
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    app.surface.osd_held = on_photo_tool(app, ctx);
    if app.surface.osd_held {
        // nothing over the photo; what changed meanwhile is not saved up for later either
        take_report(app);
        app.surface.action = None;
        return;
    }
    let Some((readout, age)) = current(app) else { return };
    let osd = app.surface.settings().osd.clone();
    let display = f64::from(osd.display_ms) / 1000.0;
    let fade = if age <= display { 1.0 } else { (1.0 - (age - display) / FADE).clamp(0.0, 1.0) } as f32;
    if fade <= 0.0 || readout.title.is_empty() {
        if fade <= 0.0 {
            app.surface.action = None;
        }
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(30));
    let area = app.canvas_rect.unwrap_or_else(|| ctx.content_rect());
    paint(ctx, area, &osd, &readout, fade);
}

/// Draw `r` over `area` with the overlay settings `osd` (`fade` 0…1).
pub fn paint(ctx: &egui::Context, area: Rect, osd: &ls::settings::OsdSettings, r: &Readout, fade: f32) {
    let s = osd.size.clamp(0.5, 2.0) as f32;
    let height = (HEIGHT + if r.caption.is_empty() { 0.0 } else { CAPTION_H }) * s;
    let y = match osd.position {
        OsdPosition::TopEdge => area.top() + 20.0 + height / 2.0,
        OsdPosition::Top => area.top() + 80.0 + height / 2.0,
        OsdPosition::BottomEdge => area.bottom() - 20.0 - height / 2.0,
        OsdPosition::Bottom => area.bottom() - 80.0 - height / 2.0,
    };
    let opacity = osd.opacity.clamp(0.1, 1.0) as f32 * fade;
    let (cr, cg, cb) = ls::settings::rgb(&osd.background).unwrap_or((32, 32, 32));
    let t = Tokens::get(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("surface_osd")));
    let width = (WIDTH * s).min(area.width() - 16.0).max(160.0);
    let rect = Rect::from_center_size(pos2(area.center().x, y), vec2(width, height));
    let radius = (HEIGHT * s / 2.0).min(height / 2.0);
    painter.rect_filled(rect, radius, Color32::from_rgb(cr, cg, cb).gamma_multiply(opacity));
    painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, Color32::from_white_alpha(40).gamma_multiply(fade)), egui::StrokeKind::Inside);
    crate::widgets::register(ctx, "osd:surface", rect);
    let title_c = Color32::from_rgb(220, 220, 220).gamma_multiply(fade);
    let value_c = Color32::WHITE.gamma_multiply(fade);
    let main = Rect::from_min_size(rect.min, vec2(rect.width(), HEIGHT * s));
    // the icon (the same one the surface's key shows) at the left of the pill
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 40.0 * s, main.center().y), vec2(40.0 * s, 40.0 * s));
    let has_icon = r.icon.is_some_and(|name| crate::surface_icons::paint_with(ctx, &painter, name, icon_rect, Color32::WHITE.gamma_multiply(fade)));
    let left = if has_icon { rect.left() + 70.0 * s } else { rect.left() + 30.0 * s };
    let right = rect.right() - 30.0 * s;
    if !r.caption.is_empty() {
        let at = pos2(if has_icon { left } else { rect.center().x }, main.bottom() - 2.0 * s);
        let align = if has_icon { Align2::LEFT_CENTER } else { Align2::CENTER_CENTER };
        painter.text(at, align, &r.caption, t.font(12.5 * s), title_c);
    }
    if r.value.is_empty() {
        let center = if has_icon { pos2((left + right) / 2.0, main.center().y) } else { main.center() };
        painter.text(center, Align2::CENTER_CENTER, &r.title, t.semibold(17.0 * s), value_c);
        return;
    }
    let text_y = rect.top() + 26.0 * s;
    painter.text(pos2(left, text_y), Align2::LEFT_CENTER, &r.title, t.font(14.0 * s), title_c);
    painter.text(pos2(right, text_y), Align2::RIGHT_CENTER, &r.value, t.semibold(18.0 * s), value_c);
    if let Some(p) = r.bar {
        let track = Rect::from_min_max(pos2(left, rect.top() + 46.0 * s), pos2(right, rect.top() + 50.0 * s));
        painter.rect_filled(track, 2.0, Color32::from_white_alpha(80).gamma_multiply(fade));
        let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * p.clamp(0.0, 1.0) as f32, track.bottom()));
        painter.rect_filled(fill, 2.0, Color32::from_rgb(100, 180, 255).gamma_multiply(fade));
    }
}

// ------------------------------------------------------------------------------------- settings

/// What the overlay reports, as Settings names it.
pub const MODES: &[(OsdMode, &str)] = &[(OsdMode::Off, "Off"), (OsdMode::Surface, "Control surface only"), (OsdMode::Always, "Always")];
/// Where it sits (TactileBridge's names).
pub const POSITIONS: &[(OsdPosition, &str)] =
    &[(OsdPosition::Bottom, "Down"), (OsdPosition::BottomEdge, "Down Low"), (OsdPosition::Top, "Up"), (OsdPosition::TopEdge, "Up High")];

/// Settings, Interface, Overlay (also in Settings, Controls): one set of settings for every
/// overlay.
pub fn settings_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    crate::surface::ensure_loaded(app);
    let mut s = app.surface.settings().osd.clone();
    let before = s.clone();
    super::settings::heading_help(ui, t, "On-Screen Overlay", "changeOverlay");
    let mut mode = s.effective_mode();
    row(ui, t, "Show changes", |ui| {
        if choices(ui, "osdMode", MODES, &mut mode) {
            s.set_mode(mode);
        }
    });
    hint(ui, t, "A short note in the window for every change: a slider's value, a rating, a profile applied, and how many photos it changed.");
    row(ui, t, "Size", |ui| {
        let r = ui.add(egui::Slider::new(&mut s.size, 0.5..=2.0).fixed_decimals(2).suffix("×"));
        crate::widgets::register(ui.ctx(), "slider:osd.size", r.rect);
    });
    row(ui, t, "Opacity", |ui| {
        let r = ui.add(egui::Slider::new(&mut s.opacity, 0.1..=1.0).fixed_decimals(2));
        crate::widgets::register(ui.ctx(), "slider:osd.opacity", r.rect);
    });
    row(ui, t, "Shown for", |ui| {
        let r = ui.add(egui::Slider::new(&mut s.display_ms, 500..=10_000).suffix(" ms"));
        crate::widgets::register(ui.ctx(), "slider:osd.duration", r.rect);
    });
    row(ui, t, "Position", |ui| {
        choices(ui, "osdPosition", POSITIONS, &mut s.position);
    });
    row(ui, t, "Background", |ui| {
        crate::text_field::TextField::singleline("field:osdBackground", &mut s.background).width(90.0).show(ui);
    });
    if s != before {
        let colour_ok = ls::settings::rgb(&s.background).is_some();
        let rt = app.surface.runtime.settings_mut();
        if colour_ok {
            rt.osd = s;
            app.surface.changed();
        } else {
            // keep the half-typed colour in the field without applying it
            rt.osd.background = s.background;
        }
    }
}

// ------------------------------------------------------------------------------------- commands

fn parse_mode(s: &str) -> Option<OsdMode> {
    Some(match s.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
        "off" => OsdMode::Off,
        "surface" | "controlsurface" | "controlsurfaceonly" => OsdMode::Surface,
        "always" | "on" => OsdMode::Always,
        _ => return None,
    })
}

fn parse_position(s: &str) -> Option<OsdPosition> {
    Some(match s.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
        "down" | "bottom" => OsdPosition::Bottom,
        "downlow" | "bottomedge" => OsdPosition::BottomEdge,
        "up" | "top" => OsdPosition::Top,
        "uphigh" | "topedge" => OsdPosition::TopEdge,
        _ => return None,
    })
}

fn settings_json(osd: &ls::settings::OsdSettings) -> Value {
    let position = match osd.position {
        OsdPosition::Bottom => "down",
        OsdPosition::BottomEdge => "downLow",
        OsdPosition::Top => "up",
        OsdPosition::TopEdge => "upHigh",
    };
    json!({
        "mode": osd.effective_mode(),
        "size": osd.size,
        "opacity": osd.opacity,
        "displayMs": osd.display_ms,
        "position": position,
        "background": osd.background,
    })
}

/// `osd.*` commands: `osd.settings {mode?, size?, opacity?, displayMs?, position?, background?}`
/// (every overlay setting; replies with them all), `osd.mode {mode?}` (no mode: the next one,
/// Off → Control surface only → Always, for a console key) and `osd.state` (what it shows now).
pub fn command(app: &mut LccApp, id: &str, p: &Value) -> Result<Value, String> {
    crate::surface::ensure_loaded(app);
    let mut osd = app.surface.settings().osd.clone();
    match id {
        "osd.settings" => {
            if let Some(m) = p.get("mode") {
                let m = m.as_str().and_then(parse_mode).ok_or("osd.settings: `mode` is off, surface or always")?;
                osd.set_mode(m);
            }
            if let Some(v) = p.get("size") {
                osd.size = v.as_f64().filter(|v| v.is_finite()).ok_or("osd.settings: `size` must be a number (0.5–2)")?.clamp(0.5, 2.0);
            }
            if let Some(v) = p.get("opacity") {
                osd.opacity = v.as_f64().filter(|v| v.is_finite()).ok_or("osd.settings: `opacity` must be a number (0.1–1)")?.clamp(0.1, 1.0);
            }
            if let Some(v) = p.get("displayMs") {
                let ms = v.as_f64().filter(|v| v.is_finite()).ok_or("osd.settings: `displayMs` must be a number (500–10000)")?;
                osd.display_ms = ms.clamp(500.0, 10_000.0) as u32;
            }
            if let Some(v) = p.get("position") {
                osd.position = v.as_str().and_then(parse_position).ok_or("osd.settings: `position` is down, downLow, up or upHigh")?;
            }
            if let Some(v) = p.get("background") {
                let c = v.as_str().filter(|c| ls::settings::rgb(c).is_some()).ok_or("osd.settings: `background` is a colour like #202020")?;
                osd.background = ls::settings::hex_color(c, "#202020");
            }
        }
        "osd.mode" => {
            let next = match p.get("mode") {
                Some(m) => m.as_str().and_then(parse_mode).ok_or("osd.mode: `mode` is off, surface or always")?,
                None => match osd.effective_mode() {
                    OsdMode::Off => OsdMode::Surface,
                    OsdMode::Surface => OsdMode::Always,
                    OsdMode::Always => OsdMode::Off,
                },
            };
            osd.set_mode(next);
        }
        "osd.state" => {
            let shown = current(app).map(|(r, age)| json!({"title": r.title, "value": r.value, "caption": r.caption, "icon": r.icon, "ageS": age}));
            // `held`: kept off the photo while an on-photo tool is in use (note 15)
            return Ok(json!({"settings": settings_json(&osd), "shown": shown, "held": app.surface.osd_held}));
        }
        other => return Err(format!("unknown command `{other}`")),
    }
    if app.surface.settings().osd != osd {
        app.surface.runtime.settings_mut().osd = osd.clone();
        app.surface.changed();
    }
    Ok(settings_json(&osd))
}
