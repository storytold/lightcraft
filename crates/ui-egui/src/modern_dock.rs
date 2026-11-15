//! The Modern layout's dock (fork-owned): one translucent bar of icon segments along the bottom of
//! Develop, or down its side as a rail. The adjustment groups (Presets, Light, Colour, Tone, Detail,
//! Optics, Effects) come first; after a divider the tools (Crop, Masks, Heal, Dodge, Burn) stand out
//! in their own colour; the last button moves the dock between the bottom and the side. A tap on a
//! group opens its card over the photo, next to the dock: pill sliders for the adjustment groups, the
//! tool's panel for the tools. A second tap, Back or Esc puts it away. When the window is too narrow
//! (or short) for every button, the dock scrolls by drag or wheel.
//!
//! Own design (the dock, icon-segment and card patterns of touch drawing apps; no assets taken).

use egui::{Align2, Color32, Id, Rect, Sense, Stroke, Vec2, pos2, vec2};
use lcc_develop::{ControlSpec, controls};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::panels::edit;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::register;

/// A dock button: a group of adjustments, or a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Presets,
    Light,
    Colour,
    Tone,
    Detail,
    Optics,
    Effects,
    Crop,
    Masks,
    Heal,
    Dodge,
    Burn,
}

impl Group {
    /// The adjustment groups, in the dock's order.
    pub const ADJUST: [Group; 7] = [Group::Presets, Group::Light, Group::Colour, Group::Tone, Group::Detail, Group::Optics, Group::Effects];
    /// The tools, after the divider.
    pub const TOOLS: [Group; 5] = [Group::Crop, Group::Masks, Group::Heal, Group::Dodge, Group::Burn];

    pub fn label(self) -> &'static str {
        match self {
            Group::Presets => "Presets",
            Group::Light => "Light",
            Group::Colour => "Colour",
            Group::Tone => "Tone",
            Group::Detail => "Detail",
            Group::Optics => "Optics",
            Group::Effects => "Effects",
            Group::Crop => "Crop",
            Group::Masks => "Masks",
            Group::Heal => "Heal",
            Group::Dodge => "Dodge",
            Group::Burn => "Burn",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Group::Presets => Icon::Presets,
            Group::Light => Icon::Sun,
            Group::Colour => Icon::Palette,
            Group::Tone => Icon::Curve,
            Group::Detail => Icon::Search,
            Group::Optics => Icon::Circle,
            Group::Effects => Icon::Sparkle,
            Group::Crop => Icon::Crop,
            Group::Masks => Icon::Mask,
            Group::Heal => Icon::Eraser,
            Group::Dodge => Icon::Dodge,
            Group::Burn => Icon::Burn,
        }
    }

    pub fn is_tool(self) -> bool {
        Group::TOOLS.contains(&self)
    }

    /// The control-channel id of its dock button.
    pub fn widget_id(self) -> String {
        format!("modern:group:{}", self.label().to_ascii_lowercase())
    }

    /// A group by its name, any case; the touch layout's names (`basic`, `color`, `remove`) still work.
    pub fn parse(s: &str) -> Option<Group> {
        match s.to_ascii_lowercase().as_str() {
            "basic" => Some(Group::Light),
            "color" => Some(Group::Colour),
            "remove" => Some(Group::Heal),
            _ => Group::ADJUST.into_iter().chain(Group::TOOLS).find(|g| g.label().eq_ignore_ascii_case(s)),
        }
    }

    /// The Edit panel's sections the group opens in full (its "more" buttons): together they hold
    /// every develop setting.
    pub fn sections(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Group::Light => &[("light", "Basic"), ("profile", "Profile & Treatment")],
            Group::Colour => &[("mixer", "Color Mixer"), ("grading", "Color Grading"), ("calibration", "Calibration")],
            Group::Tone => &[("curve", "Tone Curve")],
            Group::Detail => &[("detail", "Detail"), ("retouch", "Retouch")],
            Group::Optics => &[("optics", "Lens Corrections"), ("geometry", "Transform")],
            Group::Effects => &[("effects", "Effects"), ("plugins", "Plug-in Filters")],
            _ => &[],
        }
    }

    /// The pill sliders of an adjustment group, in order (none for Presets and the tools).
    fn control_ids(self) -> &'static [&'static str] {
        match self {
            Group::Light => &["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"],
            Group::Colour => &["wb.temp", "wb.tint", "color.vibrance", "color.saturation"],
            Group::Tone => {
                &["curve.highlights", "curve.lights", "curve.darks", "curve.shadows", "curve.splitShadows", "curve.splitMid", "curve.splitHighlights"]
            }
            Group::Detail => &[
                "detail.sharpenAmount",
                "detail.sharpenRadius",
                "detail.sharpenDetail",
                "detail.sharpenMasking",
                "detail.nrLuminance",
                "detail.nrColor",
            ],
            Group::Optics => &["optics.distortion", "optics.vignetting", "optics.caRed", "optics.caBlue", "optics.vignettingCalibrationAmount"],
            Group::Effects => &["effects.texture", "effects.clarity", "effects.dehaze", "vignette.amount", "grain.amount"],
            _ => &[],
        }
    }
}

/// The section a long press (or right-click) on a slider opens: the one with its other settings.
fn section_of(control: &str) -> Option<&'static str> {
    let prefix = control.split('.').next().unwrap_or("");
    Some(match (prefix, control) {
        (_, "color.saturation" | "color.vibrance") => "mixer",
        ("vignette" | "grain", _) => "effects",
        ("curve", _) => "curve",
        ("wb", _) => "light",
        ("detail", _) => "detail",
        ("optics", _) => "optics",
        _ => return None,
    })
}

/// The tools' colour on the dock: amber, so they stand apart from the adjustment groups.
const TOOL_COLOUR: Color32 = Color32::from_rgb(246, 192, 111);
/// A dock button at the bottom (icon over its name) and on the side rail (icon only).
const ITEM: Vec2 = vec2(58.0, 54.0);
const RAIL_ITEM: Vec2 = vec2(48.0, 46.0);
const PAD: f32 = 5.0;
/// From the window's edge.
const MARGIN: f32 = 12.0;
/// The floating top row's height from the window's top (☰, the menu bar, undo, redo), with a gap:
/// the side dock starts below it.
const TOP_ROW: f32 = MARGIN + 48.0 + 12.0;
/// A tool's card (Crop, Masks, Heal…), at the right edge.
const TOOL_CARD_W: f32 = 340.0;

/// While a tool is open, the room the photo leaves for the chrome so the tool's overlay (a crop
/// frame and its handles, a brush) is never under it: (left, right, bottom) in points.
pub(crate) fn tool_room(app: &LccApp, area: Rect) -> (f32, f32, f32) {
    if !app.ui.modern.chrome || !app.ui.modern.group.is_some_and(Group::is_tool) {
        return (0.0, 0.0, 0.0);
    }
    let right = (TOOL_CARD_W + 2.0 * MARGIN).min(area.width() * 0.45);
    if app.ui.modern.dock_side { (RAIL_ITEM.x + 2.0 * PAD + 2.0 * MARGIN, right, MARGIN) } else { (MARGIN, right, ITEM.y + 2.0 * PAD + 2.0 * MARGIN) }
}

/// The height the floating dock takes from the bottom of the photo's room, when the chrome shows
/// (0 when it doesn't; a rail down the side takes only its margin). Whatever else floats over the
/// photo's bottom edge — the loupe's shot-info strip — keeps clear of this.
pub(crate) fn bottom_clearance(app: &LccApp) -> f32 {
    if !app.ui.modern.chrome {
        return 0.0;
    }
    if app.ui.modern.dock_side { MARGIN } else { ITEM.y + 2.0 * PAD + 2.0 * MARGIN }
}

/// The group's controls this photo has. White balance is on the photo's own scale, as the Edit
/// panel shows it: Kelvin for raw files, relative (−100..100) for rendered ones.
pub(crate) fn specs(app: &LccApp, g: Group) -> Vec<&'static ControlSpec> {
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id));
    let kelvin = active.is_some_and(|p| p.develops_raw() && !p.relative_wb());
    // the flat field's Amount only while the photo has a map (the rest is in Lens Corrections, which the group opens)
    let vignetting_calibration = active.is_some_and(|p| p.develop.optics.vignetting_calibration.is_some());
    g.control_ids()
        .iter()
        .filter_map(|id| match *id {
            "wb.temp" if !kelvin => Some(&edit::REL_TEMP),
            "wb.tint" if !kelvin => Some(&edit::REL_TINT),
            "optics.vignettingCalibrationAmount" if !vignetting_calibration => None,
            id => controls::find(id),
        })
        .collect()
}

/// The develop control and value a slider's value sets (relative white balance → Kelvin and tint).
fn develop_value(spec: &ControlSpec, v: f64) -> (&'static str, f64) {
    match spec.id {
        "wb.tempRel" => ("wb.temp", edit::rel_to_k(v)),
        "wb.tintRel" => ("wb.tint", v),
        id => (controls::find(id).map_or("", |s| s.id), v),
    }
}

/// A control's value on the active photo (the white balance in effect, as the Edit panel shows it).
pub(crate) fn value_of(app: &LccApp, spec: &ControlSpec) -> Option<f64> {
    let id = app.session.active()?;
    let d = app.session.develop_of(id).unwrap_or_default();
    match spec.id {
        "wb.tempRel" => return Some(edit::k_to_rel(d.wb.temp)),
        "wb.tintRel" => return Some(d.wb.tint.clamp(-100.0, 100.0)),
        _ => {}
    }
    if spec.id.starts_with("wb.") {
        let info = app.session.source_info(id);
        let (temp, tint) = lcc_engine::pipeline::local::effective_wb(&info, &d);
        return Some(if spec.id == "wb.temp" { temp } else { tint });
    }
    Some(controls::get(&d, spec.id).unwrap_or(spec.default))
}

/// Opens a group's card (a second tap on the open one closes it). A tool opens its panel; Dodge and
/// Burn start a brush mask that lightens or darkens.
pub(crate) fn open(app: &mut LccApp, g: Group) -> Result<(), String> {
    if app.ui.modern.group == Some(g) {
        close(app);
        return Ok(());
    }
    if app.session.active().is_none() {
        return Err("no photo is open".into());
    }
    match g {
        Group::Crop => app.ui.right = RightPanel::Crop,
        Group::Masks => app.ui.right = RightPanel::Masking,
        Group::Heal => {
            app.ui.right = RightPanel::Remove;
            if app.ui.tool.is_empty() {
                app.ui.tool = "remove".into();
            }
        }
        Group::Dodge | Group::Burn => {
            app.ui.right = RightPanel::Masking;
            let (name, exposure) = if g == Group::Dodge { ("Dodge", 0.35) } else { ("Burn", -0.35) };
            app.run("mask.add", json!({"kind": "brush", "name": crate::i18n::tr(name).to_string()}))?;
            app.run("mask.adjust", json!({"values": {"exposure": exposure}}))?;
        }
        _ => {
            if app.ui.right.is_edit_tool() {
                app.ui.right = RightPanel::Edit;
            }
        }
    }
    app.ui.modern.group = Some(g);
    app.ui.modern.more = None;
    Ok(())
}

/// Puts the open card away; a tool's panel goes back to Edit, so its overlay leaves the photo.
pub(crate) fn close(app: &mut LccApp) {
    app.ui.modern.more = None;
    if app.ui.modern.group.take().is_some_and(Group::is_tool) && app.ui.right.is_edit_tool() {
        app.ui.right = RightPanel::Edit;
    }
}

/// The glass behind the Modern layout's floating cards: the chrome's colour, partly see-through,
/// with a faint light edge.
pub(crate) fn glass(p: &egui::Painter, r: Rect, radius: f32, t: &Tokens) {
    p.rect_filled(r, radius, t.chrome.gamma_multiply(0.86));
    p.rect_stroke(r, radius, Stroke::new(1.0, Color32::from_white_alpha(28)), egui::StrokeKind::Inside);
}

enum Slot {
    Group(Group),
    Divider,
    Switch,
}

/// Draws the dock in `area` (the photo's room) and returns its rectangle.
pub(crate) fn dock(app: &mut LccApp, ui: &egui::Ui, area: Rect) -> Rect {
    let t = Tokens::get(ui.ctx());
    let side = app.ui.modern.dock_side;
    let slots: Vec<Slot> = Group::ADJUST
        .into_iter()
        .map(Slot::Group)
        .chain([Slot::Divider])
        .chain(Group::TOOLS.into_iter().map(Slot::Group))
        .chain([Slot::Divider, Slot::Switch])
        .collect();
    let item = if side { RAIL_ITEM } else { ITEM };
    let divider = 9.0;
    let along = |s: &Slot| match s {
        Slot::Divider => divider,
        _ if side => item.y,
        _ => item.x,
    };
    let full: f32 = slots.iter().map(along).sum::<f32>() + 2.0 * PAD;
    let rect = if side {
        // below the top row (☰ and the menu bar, undo, redo): centred in the room left under it, and
        // scrolling when that is too short
        let top = area.top() + TOP_ROW;
        let room = (area.bottom() - MARGIN - top).max(item.y + 2.0 * PAD);
        let h = full.min(room).max(item.y + 2.0 * PAD);
        Rect::from_min_size(pos2(area.left() + MARGIN, top + (room - h) / 2.0), vec2(item.x + 2.0 * PAD, h))
    } else {
        let w = full.min(area.width() - 2.0 * MARGIN).max(item.x + 2.0 * PAD);
        Rect::from_min_size(pos2(area.center().x - w / 2.0, area.bottom() - MARGIN - item.y - 2.0 * PAD), vec2(w, item.y + 2.0 * PAD))
    };
    let mut chosen: Option<Slot> = None;
    egui::Area::new(Id::new("modern-dock")).fixed_pos(rect.min).order(egui::Order::Middle).show(ui.ctx(), |ui| {
        let (bg, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
        // the scroller's strip: a swipe along it scrolls the dock (`modern::after_input`)
        register(ui.ctx(), "modern:strip", bg);
        register(ui.ctx(), "modern:dock", bg);
        glass(ui.painter(), bg, 18.0, &t);
        let inner = bg.shrink(PAD);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        let body = |ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for s in &slots {
                match s {
                    Slot::Divider => {
                        let (r, _) = ui.allocate_exact_size(if side { vec2(item.x, divider) } else { vec2(divider, item.y) }, Sense::hover());
                        let c = Color32::from_white_alpha(46);
                        if side {
                            ui.painter().hline(r.x_range().shrink(6.0), r.center().y, Stroke::new(1.0, c));
                        } else {
                            ui.painter().vline(r.center().x, r.y_range().shrink(8.0), Stroke::new(1.0, c));
                        }
                    }
                    Slot::Group(g) => {
                        let on = app.ui.modern.group == Some(*g);
                        let resp = segment(ui, item, &g.widget_id(), g.icon(), g.label(), on, g.is_tool(), !side);
                        if on {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            chosen = Some(Slot::Group(*g));
                        }
                    }
                    Slot::Switch => {
                        let (icon, label) = if side { (Icon::PanelBottom, "Dock at Bottom") } else { (Icon::Sidebar, "Dock at Side") };
                        let short = if side { "Bottom" } else { "Side" };
                        let resp = segment(ui, item, "modern:dockSwitch", icon, short, false, false, !side);
                        let resp = resp.on_hover_text(crate::i18n::tr(label));
                        if resp.clicked() {
                            chosen = Some(Slot::Switch);
                        }
                    }
                }
            }
        };
        let hidden = egui::scroll_area::ScrollBarVisibility::AlwaysHidden;
        if side {
            egui::ScrollArea::vertical().id_salt("modern-dock-scroll-v").scroll_bar_visibility(hidden).show(&mut child, |ui| ui.vertical(body));
        } else {
            egui::ScrollArea::horizontal().id_salt("modern-dock-scroll-h").scroll_bar_visibility(hidden).show(&mut child, |ui| ui.horizontal(body));
        }
    });
    match chosen {
        Some(Slot::Group(g)) => {
            if let Err(e) = open(app, g) {
                app.toast_error(ui.ctx(), e);
            }
        }
        Some(Slot::Switch) => app.ui.modern.dock_side = !side,
        _ => {}
    }
    rect
}

/// An icon segment: the icon, with its name under it when `labelled`; the open one sits on a soft
/// highlight. Tools draw in their own colour.
#[allow(clippy::too_many_arguments)]
fn segment(ui: &mut egui::Ui, size: Vec2, id: &str, icon: Icon, label: &str, on: bool, tool: bool, labelled: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let name = crate::i18n::tr(label);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
    register(ui.ctx(), id, r);
    let p = ui.painter();
    let cell = r.shrink(2.0);
    if on {
        p.rect_filled(cell, 12.0, Color32::from_white_alpha(36));
        p.rect_stroke(cell, 12.0, Stroke::new(1.0, Color32::from_white_alpha(30)), egui::StrokeKind::Inside);
    } else if resp.is_pointer_button_down_on() {
        p.rect_filled(cell, 12.0, t.pressed);
    } else if resp.hovered() {
        p.rect_filled(cell, 12.0, Color32::from_white_alpha(14));
    }
    let c = if tool {
        TOOL_COLOUR
    } else if on {
        t.accent
    } else {
        t.text
    };
    let resp = if labelled { resp } else { resp.on_hover_text(name) };
    if labelled {
        paint(p, Rect::from_center_size(pos2(r.center().x, r.top() + 19.0), Vec2::splat(20.0)), icon, c);
        let text = if on { t.text } else { t.text_label };
        p.text(pos2(r.center().x, r.bottom() - 12.0), Align2::CENTER_CENTER, name, t.font(10.5), text);
    } else {
        paint(p, Rect::from_center_size(r.center(), Vec2::splat(21.0)), icon, c);
    }
    resp
}

/// The open group's card, next to the dock: pill sliders (two columns when wide), the presets, or the
/// tool's panel.
pub(crate) fn card(app: &mut LccApp, ui: &egui::Ui, area: Rect, dock: Rect) {
    let Some(g) = app.ui.modern.group else { return };
    let t = Tokens::get(ui.ctx());
    let side = app.ui.modern.dock_side;
    // a section of the group opened in full (Tone Curve, Color Grading, all of Effects…)
    let more = app.ui.modern.more.clone().filter(|k| g.sections().iter().any(|(s, _)| s == k));
    let sliders = !g.control_ids().is_empty() && more.is_none();
    let width: f32 = if more.is_some() {
        420.0
    } else if sliders && !side {
        600.0
    } else {
        340.0
    };
    let gap = 10.0;
    // a tool's card sits at the right edge, clear of the photo its overlay is on (the photo makes
    // room for it: `tool_room`)
    let tool = g.is_tool();
    let (min, max_w, max_h) = if tool {
        let w = TOOL_CARD_W.min(area.width() - 2.0 * MARGIN).max(120.0);
        // under the histogram when it sits in that corner (last frame's place)
        let left = area.right() - MARGIN - w;
        let hist = app.widgets.iter().rev().find(|(id, _)| id == "modern:histogram").map(|(_, r)| *r);
        let top = hist.filter(|h| h.right() > left && h.top() < area.top() + 200.0).map_or(area.top() + 64.0, |h| h.bottom() + gap);
        (pos2(left, top), w, (area.bottom() - MARGIN - top).max(80.0))
    } else if side {
        let left = dock.right() + gap;
        let w = width.min(area.right() - MARGIN - left).max(120.0);
        (pos2(left, area.top() + 64.0), w, (area.bottom() - MARGIN - area.top() - 64.0).max(80.0))
    } else {
        let w = width.min(area.width() - 2.0 * MARGIN).max(120.0);
        let room = (dock.top() - gap - (area.top() + 64.0)).max(80.0);
        (pos2(area.center().x - w / 2.0, 0.0), w, room.min(area.height() * 0.5))
    };
    let columns = sliders && max_w >= 520.0;
    let id = Id::new("modern-card");
    // the card's height last frame places it above the dock (it grows upwards)
    let last_h: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(200.0);
    let min = if side || tool { min } else { pos2(min.x, dock.top() - gap - last_h.min(max_h)) };
    egui::Area::new(id).fixed_pos(min).order(egui::Order::Middle).show(ui.ctx(), |ui| {
        // an area lays out in the size it had last frame, and a scroll area would never grow past
        // it: give it all the room it may take (auto-shrink then fits it to the sliders)
        ui.set_max_height(max_h);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::same(12))
            .corner_radius(16.0)
            .fill(t.chrome.gamma_multiply(0.86))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(28)));
        let shown = frame.show(ui, |ui| {
            ui.set_width(max_w - 24.0);
            ui.horizontal(|ui| {
                if let Some(key) = &more {
                    // ‹ back to the group's sliders
                    let name = g.sections().iter().find(|(k, _)| k == key).map_or("", |(_, l)| *l);
                    let b = crate::button::Button::new(name).icon(Icon::ChevronLeft).kind(crate::button::Kind::Ghost).widget("modern:moreBack");
                    if b.show(ui).clicked() {
                        app.ui.modern.more = None;
                    }
                } else {
                    ui.label(egui::RichText::new(crate::i18n::tr(g.label())).font(t.semibold(14.0)).color(t.text));
                }
            });
            ui.add_space(6.0);
            let photo = app.session.active();
            egui::ScrollArea::vertical().id_salt(("modern-card-scroll", g.label(), side, more.clone())).max_height(max_h - 60.0).show(ui, |ui| {
                ui.set_width(max_w - 24.0);
                if let (Some(key), Some(photo)) = (&more, photo) {
                    // the Colour Grading card matches the grading mode (Settings ▸ General):
                    // Witchcraft shows the colourist's own Primaries palette and HSL curves where
                    // Eboda shows the 3-way wheels
                    if *key == "grading" && app.ui.settings.grading_mode == crate::state::GradingMode::Witchcraft {
                        edit::show_section(app, ui, photo, "primaries");
                        ui.add_space(10.0);
                        // a black-and-white photo has no HSL curves (the B&W mix has its own card)
                        if !crate::is_bw(&app.session.develop_of(photo).unwrap_or_default()) {
                            edit::show_section(app, ui, photo, "hsl");
                        }
                    } else {
                        edit::show_section(app, ui, photo, key);
                    }
                } else if sliders {
                    let all = specs(app, g);
                    if columns {
                        let half = all.len().div_ceil(2);
                        ui.columns(2, |cols| {
                            for (i, col) in cols.iter_mut().enumerate() {
                                for spec in all.iter().skip(i * half).take(half) {
                                    pill(app, col, spec);
                                }
                            }
                        });
                    } else {
                        for spec in all {
                            pill(app, ui, spec);
                        }
                    }
                } else if g == Group::Presets {
                    crate::panels::presets::show(app, ui, crate::module::DEVELOP_LEFT, false);
                } else {
                    crate::panels::right::tool_only(app, ui);
                }
                // the group's full sections: every setting the Edit panel has is a tap away
                if more.is_none() && !g.sections().is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for (key, label) in g.sections() {
                            let b = crate::button::Button::new(label).icon(Icon::ChevronRight).widget(format!("modern:more:{key}"));
                            if b.show(ui).clicked() {
                                app.ui.modern.more = Some((*key).to_string());
                            }
                        }
                    });
                }
            });
        });
        let r = shown.response.rect;
        register(ui.ctx(), "modern:card", r);
        ui.ctx().data_mut(|d| d.insert_temp(id, r.height()));
    });
}

fn pill(app: &mut LccApp, ui: &mut egui::Ui, spec: &'static ControlSpec) {
    let Some(v) = value_of(app, spec) else { return };
    let section = section_of(spec.id);
    let out = crate::widgets::pill(ui, spec, v, true, section.is_some());
    if out.more {
        app.ui.modern.more = section.map(str::to_string);
    }
    edit::apply_slider_out(app, spec, out, |app, v| {
        let (control, value) = develop_value(spec, v);
        app.run("develop.set", json!({"control": control, "value": value}))
    });
}
