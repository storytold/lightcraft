//! The Masking panel: create masks, list them, edit components and local adjustments.

use egui::{Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, LocalAdjustments, MaskShape, Section, Track};
use serde_json::json;

use super::edit::apply_slider_out;
use super::right::header;
use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, one_line, register, slider, text_button};

#[path = "mask_controls.rs"]
pub(in crate::panels) mod controls;
use controls::{Fill, Hit, measuring};
use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
use craft::ui::menu::{Menu, MenuItem};

const fn spec(id: &'static str, label: &'static str, min: f64, max: f64, step: f64, decimals: u8, track: Track) -> ControlSpec {
    ControlSpec { id, label, section: Section::Light, min, max, default: 0.0, step, decimals, track }
}

/// Local adjustment sliders (key = `LocalAdjustments` field name).
pub const LOCAL: &[ControlSpec] = &[
    spec("temp", "Temp", -100.0, 100.0, 1.0, 0, Track::Temp),
    spec("tint", "Tint", -100.0, 100.0, 1.0, 0, Track::Tint),
    spec("exposure", "Exposure", -4.0, 4.0, 0.01, 2, Track::Centered),
    spec("contrast", "Contrast", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("highlights", "Highlights", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("shadows", "Shadows", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("whites", "Whites", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("blacks", "Blacks", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("texture", "Texture", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("clarity", "Clarity", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("dehaze", "Dehaze", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("hue", "Hue", -100.0, 100.0, 1.0, 0, Track::Rainbow),
    spec("saturation", "Saturation", -100.0, 100.0, 1.0, 0, Track::Gradient { from: "#7a7a7a", to: "#e04a3a" }),
    spec("sharpness", "Sharpness", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("noise", "Noise", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("moire", "Moiré", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("defringe", "Defringe", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("lens_blur", "Lens Blur", 0.0, 100.0, 1.0, 0, Track::Plain),
];

pub fn local_get(a: &LocalAdjustments, key: &str) -> f64 {
    serde_json::to_value(a).ok().and_then(|v| v.get(key).and_then(|x| x.as_f64())).unwrap_or(0.0)
}

fn kind_label(s: &MaskShape) -> (&'static str, Icon) {
    match s {
        MaskShape::Brush { .. } => ("Brush", Icon::Brush),
        MaskShape::Linear { .. } => ("Linear Gradient", Icon::Linear),
        MaskShape::Radial { .. } => ("Radial Gradient", Icon::Radial),
        MaskShape::ColorRange { .. } => ("Color Range", Icon::Picker),
        MaskShape::LuminanceRange { .. } => ("Luminance Range", Icon::Sliders),
        MaskShape::DepthRange { .. } => ("Depth Range", Icon::Sliders),
        MaskShape::Subject => ("Subject", Icon::Subject),
        MaskShape::Sky => ("Sky", Icon::Sky),
        MaskShape::Background => ("Background", Icon::Subject),
        MaskShape::Object { .. } => ("Object", Icon::Subject),
        MaskShape::Prompt { .. } => ("Describe", Icon::Subject),
        MaskShape::People { .. } => ("People", Icon::Subject),
        MaskShape::Landscape { .. } => ("Landscape", Icon::Sky),
    }
}

const TILE: f32 = 52.0;
const TILE_GAP: f32 = 6.0;
/// Room left and right of a tile's label.
const TILE_PAD: f32 = 3.0;

/// Columns, tile width and label size of the Create New Mask grid in `width`, `label(size)` being
/// the widest label at a font size: four tiles of up to 52 pt (at least 48), else three, else two —
/// the first that fits the labels at 10.5 pt, or at 9.5 pt before a column is dropped. A tile grows
/// past 52 pt for a long (translated) label. In a panel too narrow even for that, two tiles whose
/// labels are cut short.
fn tile_layout(width: f32, label: impl Fn(f32) -> f32) -> (usize, f32, f32) {
    let room = |n: usize| ((width - (n as f32 - 1.0) * TILE_GAP) / n as f32).floor();
    for n in [4, 3, 2] {
        let w = room(n);
        if n == 4 && w < 48.0 {
            continue;
        }
        for size in [10.5, 9.5] {
            let need = (label(size) + 2.0 * TILE_PAD).ceil();
            if need <= w {
                return (n, w.min(TILE.max(need)), size);
            }
        }
    }
    (2, room(2).max(24.0), 9.5)
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Masking");
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Create New Mask")).color(t.text_dim));
        ui.add_space(6.0);
        let tiles: [(&str, &str, Icon); 10] = [
            ("object", "Object", Icon::Subject),
            ("prompt", "Describe", Icon::Subject),
            ("subject", "Subject", Icon::Subject),
            ("sky", "Sky", Icon::Sky),
            ("background", "Background", Icon::Subject),
            ("brush", "Brush", Icon::Brush),
            ("linear", "Linear", Icon::Linear),
            ("radial", "Radial", Icon::Radial),
            ("luminanceRange", "Luminance", Icon::Sliders),
            ("colorRange", "Color", Icon::Picker),
        ];
        // four 52 pt tiles a row when they and their labels fit, else fewer (wider for a long label)
        let widest = |size: f32| {
            let font = t.font(size);
            tiles
                .iter()
                .map(|(_, l, _)| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text_dim).size().x)
                .fold(0.0, f32::max)
        };
        let (cols, tile, size) = tile_layout(ui.available_width(), widest);
        egui::Grid::new("mask-tiles").spacing(vec2(TILE_GAP, TILE_GAP)).show(ui, |ui| {
            for (i, (kind, label, icon)) in tiles.iter().enumerate() {
                let (_, r) = ui.allocate_space(vec2(tile, 52.0));
                let resp = Hit::new(&format!("maskNew:{kind}"), crate::i18n::tr(label)).fill(Fill::new(t.inset, t.hover, 4.0)).show_at(ui, r);
                paint(ui.painter(), Rect::from_center_size(r.center() - vec2(0.0, 7.0), vec2(20.0, 20.0)), *icon, t.text_label);
                // (cut short only in a panel too narrow for the label: the full name on hover)
                let label = crate::i18n::tr(label);
                let g = one_line(ui.painter(), label, t.font(size), t.text_dim, tile - 2.0 * TILE_PAD);
                let elided = g.elided;
                let at = pos2(r.center().x, r.bottom() - 9.0) - g.size() / 2.0;
                register(ui, format!("maskNewLabel:{kind}"), Rect::from_min_size(at, g.size()));
                ui.painter().galley(at, g, t.text_dim);
                let resp = if elided { resp.on_hover_text(label) } else { resp };
                if resp.clicked() {
                    match *kind {
                        "colorRange" => {
                            // an empty colour range; clicking the photo samples it
                            let _ = app.run("mask.add", json!({"kind": "colorRange"}));
                            app.ui.tool = "colorRange".into();
                            app.toast(ui.ctx(), crate::i18n::tr("Click the photo to pick a colour · ⇧-click adds more"));
                        }
                        "object" => start_object(app, ui.ctx(), "new"),
                        "prompt" => start_describe(app, "new"),
                        "brush" => {
                            let _ = app.run("tool.brush", json!({"new": true}));
                        }
                        "linear" | "radial" => {
                            app.ui.tool = kind.to_string();
                            let _ = app.run("mask.add", json!({"kind": kind}));
                        }
                        k => {
                            let _ = app.run("mask.add", json!({"kind": k}));
                        }
                    }
                }
                if i % cols == cols - 1 {
                    ui.end_row();
                }
            }
        });
        describe_field(app, ui, true);
        let seg = &app.session.segmenter;
        let status = if seg.analyzing() {
            Some("Analyzing the photo for AI masks…")
        } else if seg.busy() {
            Some("Selecting…")
        } else if seg.detail_busy() || app.ui.detail_due.is_some() {
            Some("Refining the mask's detail…")
        } else {
            None
        };
        if let Some(status) = status {
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                crate::progress::spinner(ui, 14.0);
                ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(status)).color(t.text_dim)).wrap());
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        // the model download, while its dialog is closed
        let download = seg.download_status();
        if download.running && app.ui.dialog.is_none() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // the bar takes what the Details button leaves
                let details = ui.spacing().interact_size.x + 30.0;
                sam_download_bar(ui, download.done, download.total, (ui.available_width() - details).max(96.0));
                if text_button(ui, "maskSamDetails", crate::i18n::tr("Details"), false).clicked() {
                    app.offer_sam_download(None);
                }
            });
        }
    });
    divider(ui);
    // mask list
    egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 8, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Masks")).font(t.semibold(13.0)).color(t.text_label));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let on = app.ui.mask_overlay;
                if icon_button(ui, "maskOverlay", Icon::Eye, vec2(24.0, 24.0), on, true, "Show overlay (O)").clicked() {
                    app.ui.mask_overlay = !on;
                }
                let all = app.ui.mask_overlay_all;
                let all_mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
                let all_base = crate::i18n::tr("Show all overlays");
                let all_tip = match crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.maskOverlayAll") {
                    Some(sc) => format!("{all_base} ({})", crate::menubar::shortcut_text(sc, all_mac)),
                    None => all_base.to_string(),
                };
                if icon_button(ui, "maskOverlayAll", Icon::Stack, vec2(24.0, 24.0), all, true, &all_tip).clicked() {
                    app.ui.mask_overlay_all = !all;
                }
            });
        });
        if d.masks.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No masks yet. Choose a mask type above.")).color(t.text_dim));
        }
        let count = d.masks.len();
        for (index, m) in d.masks.iter().enumerate() {
            let sel = app.session.active_mask == Some(m.id);
            if let Some((rid, name)) = app.ui.renaming_mask.as_mut().filter(|(rid, _)| *rid == m.id) {
                // inline rename: Enter (or leaving the field) commits, Escape cancels
                let rid = *rid;
                let widget = format!("maskRename:{rid}");
                let r = crate::text_field::TextField::singleline(&widget, name).width(f32::INFINITY).select_on_focus(true).show(ui);
                // a field that just appeared takes the keyboard at once
                if !r.editing && r.ending.is_none() {
                    crate::text_field::focus_id(ui.ctx(), r.response.id);
                }
                if r.cancelled() {
                    app.ui.renaming_mask = None;
                } else if r.committed() {
                    let name = name.trim().to_string();
                    app.ui.renaming_mask = None;
                    if !name.is_empty() && name != m.name {
                        let _ = app.run("mask.rename", json!({"id": rid, "name": name}));
                    }
                }
                continue;
            }
            let (_, r) = ui.allocate_space(vec2(ui.available_width(), 30.0));
            let plate = if sel { Fill::new(t.tool_active, t.tool_active, 4.0) } else { Fill::new(t.chrome, t.hover.gamma_multiply(0.7), 4.0) };
            let resp = Hit::new(&format!("mask:{}", m.id), m.name.clone()).fill(plate).selected(sel).show_at(ui, r);
            let icon = m.components.first().map(|c| kind_label(&c.shape).1).unwrap_or(Icon::Mask);
            paint(ui.painter(), Rect::from_min_size(r.min + vec2(8.0, 7.0), vec2(16.0, 16.0)), icon, t.text_label);
            // a long name is cut short (with …) before the eye, the full name on hover
            let color = if m.visible { t.text } else { t.text_disabled };
            let g = one_line(ui.painter(), &m.name, t.font(13.0), color, r.width() - 32.0 - 32.0);
            let elided = g.elided;
            let at = pos2(r.left() + 32.0, r.center().y - g.size().y / 2.0);
            register(ui, format!("maskName:{}", m.id), Rect::from_min_size(at, g.size()));
            ui.painter().galley(at, g, color);
            let resp = if elided { resp.on_hover_text(&m.name) } else { resp };
            // show / hide on hover (and always while hidden)
            // (the pointer test, not `hovered`: over the eye, the row itself no longer counts as hovered)
            if ui.rect_contains_pointer(r) || !m.visible {
                let er = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(22.0, 22.0));
                let eye = Hit::new(&format!("maskVisible:{}", m.id), crate::i18n::tr("Show mask")).selected(m.visible).show_at(ui, er);
                paint(
                    ui.painter(),
                    er.shrink(3.0),
                    if m.visible { Icon::Eye } else { Icon::EyeOff },
                    if eye.hovered() { t.text } else { t.text_dim },
                );
                if eye.clicked() {
                    let _ = app.run("mask.visible", json!({"id": m.id}));
                }
            }
            if resp.clicked() {
                let _ = app.run("mask.select", json!({"id": m.id}));
            }
            if resp.double_clicked() {
                app.ui.renaming_mask = Some((m.id, m.name.clone()));
            }
            // (the framework's secondary click and Shift+F10; carrying photos opens nothing)
            if !measuring(ui) && !crate::widgets::carrying(ui.ctx()) {
                Menu::context(m.name.clone(), &resp.response)
                    .id_salt(("mask-context-menu", m.id))
                    .show(ui, |ui, _| mask_menu(app, ui, m.id, &m.name, m.visible, index, count));
            }
            if sel {
                // right under the selected mask: ＋ adds to it, − takes away from it (any mask
                // type; Describe… opens its field here)
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    let plus = text_button(ui, &format!("maskPlus:{}", m.id), "+", false).on_hover_text(crate::i18n::tr("Add to this mask"));
                    opener_menu(ui, &plus, ("maskPlus", m.id), "Add to this mask", |ui| component_menu(app, ui, "add", ("maskPlus", m.id)));
                    let minus = text_button(ui, &format!("maskMinus:{}", m.id), "−", false).on_hover_text(crate::i18n::tr("Subtract from this mask"));
                    opener_menu(ui, &minus, ("maskMinus", m.id), "Subtract from this mask", |ui| component_menu(app, ui, "subtract", ("maskMinus", m.id)));
                });
                describe_field(app, ui, false);
            }
        }
        if !d.masks.is_empty() {
            ui.add_space(6.0);
            overlay_options(app, ui);
        }
    });
    let Some(mid) = app.session.active_mask else { return };
    let Some(m) = d.masks.iter().find(|m| m.id == mid).cloned() else { return };
    divider(ui);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 8 }).show(ui, |ui| {
        for (i, c) in m.components.iter().enumerate() {
            let (kind, icon) = kind_label(&c.shape);
            let label = c.name.clone().unwrap_or_else(|| match &c.shape {
                MaskShape::Prompt { text, .. } => format!("“{text}”"),
                _ => menu_label(kind).to_string(),
            });
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                paint(ui.painter(), r, icon, t.text_label);
                let op = match c.op {
                    lightcraft_develop::MaskOp::Add => "",
                    lightcraft_develop::MaskOp::Subtract => "− ",
                    lightcraft_develop::MaskOp::Intersect => "∩ ",
                };
                if let Some((_, _, name)) = app.ui.renaming_component.as_mut().filter(|(mid, k, _)| *mid == m.id && *k == i) {
                    // inline rename: Enter (or leaving the field) commits, Escape cancels
                    let widget = format!("componentRename:{i}");
                    let r = crate::text_field::TextField::singleline(&widget, name)
                        .width((ui.available_width() - 30.0).max(1.0))
                        .select_on_focus(true)
                        .show(ui);
                    // a field that just appeared takes the keyboard at once
                    if !r.editing && r.ending.is_none() {
                        crate::text_field::focus_id(ui.ctx(), r.response.id);
                    }
                    if r.cancelled() {
                        app.ui.renaming_component = None;
                    } else if r.committed() {
                        let name = name.trim().to_string();
                        app.ui.renaming_component = None;
                        if Some(&name) != c.name.as_ref() {
                            let _ = app.run("mask.component", json!({"id": m.id, "component": i, "action": "rename", "name": name}));
                        }
                    }
                    return;
                }
                // a long name is cut short (with …) before the options button, not past the panel
                let text = format!("{op}{label}{}", if c.invert { crate::i18n::tr(" (inverted)") } else { "" });
                let room = (ui.available_width() - crate::widgets::text_button_size(ui, "…") - ui.spacing().item_spacing.x).max(0.0);
                let ink = ui.visuals().text_color();
                let galley = one_line(ui.painter(), &text, egui::TextStyle::Body.resolve(ui.style()), ink, room);
                let (_, name_rect) = ui.allocate_space(galley.size());
                let resp = Hit::new(&format!("component:{i}"), text.clone()).show_at(ui, name_rect);
                ui.painter().galley(name_rect.min, galley, ink);
                if resp.double_clicked() {
                    app.ui.renaming_component = Some((m.id, i, label.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let more = text_button(ui, &format!("componentMenu{i}"), "…", false).on_hover_text(crate::i18n::tr("Component options"));
                    component_row_popup(app, ui, &resp, &more, m.id, i, &label, m.components.len());
                });
            });
            range_controls(app, ui, i, &c.shape);
        }
        ui.add_space(6.0);
        // wraps: the five actions are wider than a narrow panel
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let add = text_button(ui, "maskAddComp", crate::i18n::tr("Add"), false);
            opener_menu(ui, &add, "maskAddComp", "Add", |ui| component_menu(app, ui, "add", "maskAddComp"));
            let sub = text_button(ui, "maskSubComp", crate::i18n::tr("Subtract"), false);
            opener_menu(ui, &sub, "maskSubComp", "Subtract", |ui| component_menu(app, ui, "subtract", "maskSubComp"));
            let int = text_button(ui, "maskIntComp", crate::i18n::tr("Intersect"), false);
            opener_menu(ui, &int, "maskIntComp", "Intersect", |ui| component_menu(app, ui, "intersect", "maskIntComp"));
            if text_button(ui, "maskInvert", crate::i18n::tr("Invert"), m.invert).clicked() {
                let _ = app.run("mask.invert", json!({}));
            }
            if icon_button(ui, "maskDelete", Icon::Trash, vec2(26.0, 24.0), false, true, "Delete mask").clicked() {
                let _ = app.run("mask.delete", json!({}));
            }
        });
    });
    if app.ui.tool == "brush" {
        divider(ui);
        brush_settings(app, ui);
    }
    divider(ui);
    super::mask_effects::effect_row(app, ui, &m);
    // (Lens Blur has its own block below)
    for s in LOCAL.iter().filter(|s| s.id != "lens_blur") {
        let v = local_get(&m.adjust, s.id);
        mask_local_scalar(app, ui, (id, m.id), s, v);
    }
    let amt = ControlSpec {
        id: "amount",
        label: "Amount",
        section: Section::Light,
        min: 0.0,
        max: 200.0,
        default: 100.0,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    mask_local_scalar(app, ui, (id, m.id), &amt, m.adjust.amount);
    let refine = ControlSpec {
        id: "refine",
        label: "Refine Edges",
        section: Section::Light,
        min: 0.0,
        max: 100.0,
        default: 0.0,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    mask_local_scalar(app, ui, (id, m.id), &refine, m.refine);
    divider(ui);
    lens_blur_block(app, ui, &m);
    ui.add_space(30.0);
    let _ = Stroke::NONE;
}

/// Only the selected mask's seventeen local adjustments, Amount and Refine use this binding.
/// Scalar keys belong to the SDK's real composite row; no app key handler or hover proxy routes them.
fn mask_local_scalar(app: &mut LightcraftApp, ui: &mut egui::Ui, target: (PhotoId, u32), spec: &ControlSpec, current: f64) {
    use craft::behavior::{EditEvent, NumberCodec, Reader};
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::form::SliderField;
    use craft::ui::runtime::env::{WheelScope, with_env};
    use craft::ui::runtime::keys::{ScalarKeyPolicy, ScalarKeyTrigger};

    #[derive(Clone, Copy)]
    struct Visit {
        owner: (u64, PhotoId, u32),
        revision: u64,
        epoch: u64,
        pass: u64,
        enabled: bool,
        editing: bool,
    }
    fn set(app: &mut LightcraftApp, spec: &ControlSpec, value: f64) -> Result<serde_json::Value, String> {
        if spec.id == "refine" { app.run("mask.refine", json!({"value":value})) } else { app.run("mask.adjust", json!({"values":{spec.id:value}})) }
    }
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    // A structural action earlier in this parent may have retired its cloned mask. Never bind it to a replacement.
    if !real || app.session.active() != Some(target.0) || app.session.active_mask != Some(target.1) {
        ui.allocate_space(vec2(ui.available_width(), Tokens::get(ui.ctx()).slider_row_h));
        return;
    }
    let owner = (app.session.library_generation(), target.0, target.1);
    let pass = ui.ctx().cumulative_pass_nr();
    let enabled = ui.is_enabled();
    let key = ui.scope_id().with(("mask-local-visit", spec.id));
    let revision = app.session.catalog.revision;
    let marker = format!("mask-local:{}", spec.id);
    let mut visit = ui.data(|data| data.get_temp::<Visit>(key)).unwrap_or(Visit { owner, revision, epoch: 0, pass, enabled, editing: false });
    let foreign = visit.owner != owner || visit.revision != revision;
    let retired = visit.pass != pass && visit.pass.checked_add(1) != Some(pass);
    if foreign || retired || visit.enabled != enabled {
        if visit.editing && app.ui.dragging_control.as_deref() == Some(marker.as_str()) {
            // Do not restore an old snapshot across another mask/photo/revision. End that owned interaction;
            // a same-document disable can cancel safely. A missing pass also retires pending SDK proposals.
            let command = if foreign { "develop.endInteraction" } else { "develop.cancelInteraction" };
            let _ = app.run(command, json!({}));
            app.ui.dragging_control = None;
        }
        visit = Visit { owner, revision: app.session.catalog.revision, epoch: visit.epoch.wrapping_add(1), pass, enabled, editing: false };
    }
    let id = ui.scope_id().with(("mask-local", owner, spec.id, visit.epoch));
    let number_spec = *spec;
    let nudge = crate::widgets::nudge_unit(spec);
    let policy = ScalarKeyPolicy::new(nudge).model_step(spec.step).shift_factor(5.0).trigger(ScalarKeyTrigger::HoveredOrFocused);
    let mut control = SliderField::new(crate::i18n::tr(spec.label).to_owned(), Bind::given(current).reset(spec.default), spec.min..=spec.max)
        .id(id)
        .step(spec.step)
        .wheel_step(nudge)
        .key_policy(policy)
        .pointer_model_step(spec.step)
        .pointer_modifiers(craft::ui::value_drag::DragModifiers::new(Some(craft::ui::value_drag::DragModifier::Shift), None))
        .fill(false)
        .enabled(enabled)
        .number_codec(
            NumberCodec::new(Reader::new(), usize::from(spec.decimals)).parse_with(move |text| crate::widgets::typed_value(&number_spec, text)),
        );
    if let Some(face) = mask_local_track::face(spec.track) {
        control = control.track_face(face);
    }
    let out = with_env(ui, |env| env.wheel = Some(WheelScope::Directional), |ui| control.show(ui));
    // PaintUi::{child,labelled} document parent.with(key). Read the actual rendered responses:
    // aliases never stand in for a synthetic hit, layout guess, or the composite's non-slider group.
    for (prefix, child) in [("slider", "slider"), ("sliderValue", "field"), ("sliderField", "field")] {
        if let Some(response) = ui.ctx().read_response(id.with(child)) {
            crate::widgets::register_response(format!("{prefix}:{}", spec.id), response.rect, &response);
            crate::widgets::register_response(format!("{prefix}:maskLocal:{}", spec.id), response.rect, &response);
        }
    }
    crate::widgets::register_geometry(ui.ctx(), format!("sliderRow:maskLocal:{}", spec.id), out.response.rect);
    // The composite's typing proposal is local until Commit. Pointer scrubs and wheel runs remain live.
    let live = ui.input(|input| input.pointer.primary_down() || input.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { .. })));
    if enabled {
        for event in &out.events {
            match event {
                EditEvent::Begin { .. } if live => {
                    if app.run("develop.beginInteraction", json!({"label":spec.label})).is_ok() {
                        app.ui.dragging_control = Some(marker.clone());
                        visit.editing = true;
                    }
                }
                EditEvent::Update { value, .. } if visit.editing && value.is_finite() => {
                    let _ = set(app, spec, *value);
                }
                EditEvent::Commit { after, .. } | EditEvent::Set { after, .. } if after.is_finite() => {
                    let actual = app.session.develop_of(target.0).and_then(|settings| {
                        settings
                            .masks
                            .iter()
                            .find(|mask| mask.id == target.1)
                            .map(|mask| if spec.id == "refine" { mask.refine } else { local_get(&mask.adjust, spec.id) })
                    });
                    if actual.is_some_and(|value| (value - *after).abs() > 1e-12) {
                        let _ = set(app, spec, *after);
                    }
                    if visit.editing {
                        let _ = app.run("develop.endInteraction", json!({}));
                        app.ui.dragging_control = None;
                        visit.editing = false;
                    }
                }
                EditEvent::Cancel { .. } if visit.editing => {
                    let _ = app.run("develop.cancelInteraction", json!({}));
                    app.ui.dragging_control = None;
                    visit.editing = false;
                }
                _ => {}
            }
        }
    }
    visit.revision = app.session.catalog.revision;
    visit.pass = pass;
    ui.data_mut(|data| data.insert_temp(key, visit));
}

/// A display-only SDK track face, using the app's existing WB/gradient palette and SDK contrast cues.
mod mask_local_track {
    use craft::behavior::{Input, Metrics, Sense};
    use craft::theme::Contrast;
    use craft::ui::contract::{Common, Component, Cx, MeasureCx, PaintUi, Role, Styled as _};
    use craft::ui::primitives::PaintShape;
    use craft::ui::style::{ComponentStyle, DataColor, StyleCx, TrackLook};
    use egui::{Color32, CornerRadius, Rect, Response, Vec2, pos2};
    use lightcraft_develop::Track;
    pub(super) fn face(track: Track) -> Option<craft::ui::value_drag::TrackFace<'static>> {
        let stops = match track {
            Track::Temp => vec!["#193961", "#47566a", "#60666d", "#70705b", "#737339", "#75742a"],
            Track::Tint => vec!["#246123", "#2c552c", "#394739", "#4a3d4c", "#6b336b", "#872c87"],
            Track::Rainbow => vec!["#aa0000", "#aaaa00", "#009900", "#00aaaa", "#0044cc", "#aa0077", "#aa0000"],
            Track::Gradient { from, to } => vec![from, to],
            _ => return None,
        };
        Some(craft::ui::value_drag::TrackFace::new(Face { common: Common::new(), stops: stops.into_iter().map(crate::widgets::hex).collect() }))
    }
    #[derive(Clone, Debug, PartialEq, ComponentStyle)]
    pub(super) struct FaceStyle {
        #[style(metrics)]
        metrics: Metrics,
        track: TrackLook,
        high_contrast: bool,
    }
    #[derive(Clone, Component)]
    #[component(unregistered)]
    struct Face {
        #[common]
        common: Common,
        stops: Vec<Color32>,
    }
    impl Component for Face {
        const ROLE: Role = Role::None;
        const INPUT: Input = Input::Display;
        const PARTS: bool = false;
        const STOPS: bool = false;
        const OVERLAYS: bool = false;
        const SHOWS_LABEL: bool = false;
        type Style = FaceStyle;
        type Output = Response;
        fn style(cx: &StyleCx<'_>, _: FaceStyleKey) -> FaceStyle {
            FaceStyle { metrics: Metrics::new(), track: cx.track(), high_contrast: cx.contrast() == Contrast::High }
        }
        fn measure(&mut self, cx: &MeasureCx<'_, Self>) -> Vec2 {
            Vec2::splat(cx.style.track.thickness)
        }
        fn sense(&self) -> Sense {
            Sense::empty()
        }
        fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
            let track = &cx.style.track;
            let rect = track.groove(cx.rect);
            let draw = ui.draw();
            if cx.style.high_contrast || cx.state().is_disabled() {
                draw.fill(&PaintShape::Rect(rect, CornerRadius::same((track.thickness / 2.0).round() as u8)), cx.resolve(&track.unfilled));
                return;
            }
            let segments = self.stops.len().saturating_sub(1);
            if segments == 0 {
                return;
            }
            for i in 0..48 {
                let a = i as f32 / 48.0;
                let b = (i + 1) as f32 / 48.0;
                let position = if cx.flow.is_rtl() { 1.0 - a } else { a };
                let along = position * segments as f32;
                let index = (along.floor() as usize).min(segments.saturating_sub(1));
                if let (Some(from), Some(to)) = (self.stops.get(index), self.stops.get(index + 1)) {
                    let color = crate::widgets::lerp(*from, *to, along - index as f32);
                    let strip =
                        Rect::from_min_max(pos2(rect.left() + rect.width() * a, rect.top()), pos2(rect.left() + rect.width() * b, rect.bottom()));
                    draw.data_fill(&PaintShape::Rect(strip, CornerRadius::ZERO), DataColor::new(color));
                }
            }
        }
        fn finish(self, cx: Cx<'_, Self>) -> Response {
            cx.response.clone()
        }
    }
}

/// The mask's Lens Blur block (`section:maskLensBlur`): a header that folds it and whose eye
/// switches it off (the values kept), which side of the mask it blurs (`button:maskLensBlurSide-*`),
/// then its Amount, Boost and Transition (`lens_blur*` local adjustments, `mask.adjust`).
fn lens_blur_block(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &lightcraft_develop::Mask) {
    let a = &m.adjust;
    let open = !app.ui.section_folded("maskLensBlur");
    let (resp, toggled) = crate::widgets::section_header(ui, "maskLensBlur", "Lens Blur", open, Some(!a.lens_blur_off));
    if let Some(on) = toggled {
        // switched on with nothing to blur: a visible amount, as the Edit section's Apply does
        let mut v = json!({"lens_blur_off": !on});
        if on && a.lens_blur <= 0.0 {
            v["lens_blur"] = json!(50.0);
        }
        let _ = app.run("mask.adjust", json!({"values": v}));
    } else if resp.clicked() {
        app.ui.toggle_folded("maskLensBlur");
    }
    if !open {
        return;
    }
    ui.add_enabled_ui(!a.lens_blur_off, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 6 }).show(ui, |ui| {
            let t = Tokens::get(ui.ctx());
            ui.label(egui::RichText::new(crate::i18n::tr("Blur")).color(t.text_dim).size(11.5));
            let items = [("Selection", "inside"), ("Outside", "outside")];
            let side = if a.lens_blur_outside { 1 } else { 0 };
            if let Some(i) = crate::widgets::segmented(ui, "maskLensBlurSide", &items, Some(side), 2)
                && i != side
            {
                let mut v = json!({"lens_blur_outside": i == 1});
                if a.lens_blur <= 0.0 {
                    v["lens_blur"] = json!(50.0);
                }
                let _ = app.run("mask.adjust", json!({"values": v}));
            }
        });
        for s in LENS_BLUR {
            let v = local_get(a, s.id);
            let out = slider(ui, s, v, true, None);
            apply_slider_out(app, s, out, |app, v| app.run("mask.adjust", json!({"values": {s.id: v}})));
        }
    });
}

/// The sliders of a mask's Lens Blur block (key = `LocalAdjustments` field name).
const LENS_BLUR: &[ControlSpec] = &[
    spec("lens_blur", "Amount", 0.0, 100.0, 1.0, 0, Track::Plain),
    spec("lens_blur_boost", "Boost", 0.0, 100.0, 1.0, 0, Track::Plain),
    spec("lens_blur_transition", "Transition", 0.0, 100.0, 1.0, 0, Track::Plain),
];

/// Translate fixed action labels, but never mutate or translate user-entered mask names.
fn menu_label(source: &str) -> &str {
    crate::i18n::tr(source)
}

/// One row of a framework menu: whether it ran this pass (a click, Enter or Space). A row runs
/// nothing while the menu is only being measured, and the names it publishes are `maskMenu:{key}`.
fn menu_run(ui: &mut egui::Ui, scope: impl std::hash::Hash + std::fmt::Debug, key: &str, label: &str, enabled: bool) -> bool {
    let enabled = enabled && ui.is_enabled();
    let shown = MenuItem::new(menu_label(label).to_owned()).id_salt(("mask-menu", scope, key)).enabled(enabled).show(ui);
    if measuring(ui) {
        return false;
    }
    crate::widgets::register_response(format!("maskMenu:{key}"), shown.response.rect, &shown.response);
    enabled && shown.ran
}

/// The right-click menu of a mask in the Masks list.
fn mask_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, id: u32, name: &str, visible: bool, index: usize, count: usize) {
    use craft::ui::display::Divider;
    let mut run = |ui: &mut egui::Ui, key: &str, label: &str, enabled: bool, cmd: &str, p: serde_json::Value| {
        if menu_run(ui, id, key, label, enabled) {
            let _ = app.run(cmd, p);
        }
    };
    run(ui, "duplicate", "Duplicate Mask", true, "mask.duplicate", json!({"id": id}));
    run(ui, "duplicateInvert", "Duplicate and Invert Mask", true, "mask.duplicate", json!({"id": id, "invert": true}));
    run(ui, "invert", "Invert Mask", true, "mask.invert", json!({"id": id}));
    run(ui, "visible", if visible { "Hide Mask" } else { "Show Mask" }, true, "mask.visible", json!({"id": id}));
    Divider::horizontal().show(ui);
    run(ui, "moveUp", "Move Up", index > 0, "mask.move", json!({"id": id, "delta": -1}));
    run(ui, "moveDown", "Move Down", index + 1 < count, "mask.move", json!({"id": id, "delta": 1}));
    Divider::horizontal().show(ui);
    if menu_run(ui, id, "rename", "Rename…", true) {
        app.ui.renaming_mask = Some((id, name.to_string()));
    }
    if menu_run(ui, id, "delete", "Delete Mask", true) {
        let _ = app.run("mask.delete", json!({"id": id}));
    }
}

/// Overlay colours offered as swatches (the colour of the selected one is used for the tint).
pub const OVERLAY_COLORS: [[u8; 3]; 5] = [[230, 30, 40], [40, 200, 70], [40, 110, 240], [250, 210, 30], [255, 255, 255]];

/// How the selected mask is shown: overlay mode, colour, opacity, pins.
fn overlay_options(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use craft::ui::choice::{Checkbox, Swatch};
    use craft::ui::contract::{Bind, Entry, Props as _, Show as _};
    use craft::ui::pickers::Select;
    use craft::ui::text::NumberField;
    use craft::ui::value_drag::Slider;
    use lightcraft_pipeline::MaskView;

    let t = Tokens::get(ui.ctx());
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let parent_enabled = ui.is_enabled();
    let view = MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Overlay")).color(t.text_dim));
        // Reserve the original dropdown's size and automatic response ID. The SDK owns
        // its list, real focus, proposals and cancellation; only a pick runs the command.
        let g = one_line(ui.painter(), menu_label(view.label()), t.font(12.5), t.text_label, f32::INFINITY);
        let (id, rect) = ui.allocate_space(vec2(g.size().x + 20.0, g.size().y.max(20.0)));
        let entries: Vec<_> = MaskView::ALL.into_iter().map(|v| Entry::new(v.name().to_owned(), menu_label(v.label()))).collect();
        let out = Select::new(crate::i18n::tr("Overlay"), Bind::given(view.name().to_owned()), &entries).id(id).enabled(real).show_at(ui, rect);
        if real {
            crate::widgets::register_response("dropdown:maskOverlayMode", out.response.rect, &out.response);
            if ui.is_enabled()
                && let Some(mode) = out.events.committed()
            {
                let _ = app.run("view.maskOverlayMode", json!({"mode": mode}));
            }
        }
    
    });
    let colored = matches!(view, MaskView::Color | MaskView::ColorOnBw);
    if colored {
        ui.horizontal(|ui| {
            for c in OVERLAY_COLORS {
                let name = format!("maskOverlayColor:{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
                let label = format!("{} #{:02x}{:02x}{:02x}", crate::i18n::tr("Overlay color"), c[0], c[1], c[2]);
                let clicked = {
                    let (id, rect) = ui.allocate_space(vec2(18.0, 18.0));
                    // Palette bytes are encoded sRGB, not linear-light colour values.
                    let color = craft::theme::Srgba::rgb(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
                    let out = Swatch::new(label, color).id(id).selected(app.ui.mask_overlay_color == c).enabled(real).show_at(ui, rect);
                    if real {
                        crate::widgets::register_response(name, out.response.rect, &out.response);
                    }
                    out.clicked
                };
                if real && ui.is_enabled() && clicked {
                    let _ = app.run("view.maskOverlayColor", json!({"color": c}));
                }
            }
        });
    }
    let spec = ControlSpec {
        id: "ui.maskOverlayOpacity",
        label: "Opacity",
        section: Section::Light,
        min: 0.0,
        max: 100.0,
        default: 50.0,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    let enabled = colored && app.ui.mask_overlay;
    let pins_id;
    // Keep the original row geometry and each interactive response ID. Separate SDK
    // slider/number controls retain the existing track and exact-entry locators.
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(w, t.slider_row_h), Sense::hover());
    pins_id = ui.next_auto_id();
    let label_rect = Rect::from_min_size(pos2(row.left() + 24.0, row.top() + 4.0), vec2(w - 46.0, 18.0));
    let track_rect = Rect::from_min_max(pos2(row.left() + 24.0, row.top() + 22.0), pos2(row.right() - 22.0, row.top() + 40.0));
    let value_rect = Rect::from_min_max(pos2(label_rect.right() - 64.0, label_rect.top()), label_rect.max);
    let id = ui.scope_id().with(spec.id);
    let mut value = app.ui.mask_overlay_opacity as f64;
    let label = Hit::new("maskOverlayOpacityLabel", crate::i18n::tr_format!("Reset {name}", name = crate::i18n::tr("Opacity")))
        .id(id.with("label"))
        .enabled(real && enabled)
        .show_at(ui, label_rect);
    let label = if enabled && real {
        label.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = crate::i18n::tr("Opacity")))
    } else {
        label
    };
    ui.painter().text(
        label_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        crate::i18n::tr("Opacity"),
        t.font(12.5),
        if enabled { t.text_label } else { t.text_disabled },
    );
    let out = Slider::new(Bind::bound(&mut value).reset(spec.default), spec.min..=spec.max)
        .id(id)
        .label(crate::i18n::tr("Opacity"))
        .step(spec.step)
        .enabled(real && enabled)
        .show_at(ui, track_rect);
    let field = NumberField::new(
        crate::i18n::tr_format!("{name} value", name = crate::i18n::tr("Opacity")),
        Bind::given(Some(value)).reset(Some(spec.default)),
    )
    .id(id.with("value"))
    .range(spec.min..=spec.max)
    .step(spec.step)
    // A display suffix alone does not teach NumberField to parse percent text.
    // Percent has factor 1 here: both bare 63 and 63% keep the host's 0..100 units.
    .unit_kind(craft::ui::runtime::env::UnitKind::Percent)
    .unit("%")
    .snap_typed(true)
    .fixed_decimals(0)
    .commit_decimals(0)
    .enabled(real && enabled)
    .show_at(ui, value_rect);
    if real {
        crate::widgets::register_response("sliderLabel:ui.maskOverlayOpacity", label.rect, &label);
        crate::widgets::register_response("slider:ui.maskOverlayOpacity", out.response.rect, &out.response);
        crate::widgets::register_response("sliderValue:ui.maskOverlayOpacity", field.response.rect, &field.response);
        crate::widgets::register_response("sliderField:ui.maskOverlayOpacity", field.response.rect, &field.response);
        if enabled && ui.is_enabled() {
            if label.double_clicked() {
                value = spec.default;
            } else if let Some(Some(next)) = field.events.committed() {
                // The original exact-entry field applies only on Return/blur, unlike
                // the live track. A proposal is not a committed per-view edit.
                value = *next;
            }
            // Opacity has always been a per-view value, not a develop command or
            // history entry. Local binding isolates speculative slider passes.
            if value.is_finite() {
                app.ui.mask_overlay_opacity = value as f32;
            }
        }
    }
    let mut pins = app.ui.mask_pins;
    let changed = {
        let out = Checkbox::new(Bind::given(pins)).id(pins_id).label(crate::i18n::tr("Show Pins")).enabled(real).show(ui);
        if real {
            crate::widgets::register_response("check:maskPins", out.response.rect, &out.response);
        }
        if let Some(next) = out.events.committed() {
            pins = *next;
            true
        } else {
            false
        }
    };
    if real && ui.is_enabled() && changed {
        let _ = app.run("view.maskPins", json!({"show": pins}));
    }
    if app.ui.mask_pins {
        // The existing shared tool overlay preference remains owned by the crop/tool lane.
        super::right::tool_overlay_options(app, ui);
    }
}

#[cfg(test)]
#[path = "mask_overlay_options_flow_tests.rs"]
mod mask_overlay_options_flow_tests;

/// Controls for a range component: the selected range (two handles over a dark → light bar),
/// Smoothness, and Show Luminance Map; colour ranges get Refine.
fn range_controls(app: &mut LightcraftApp, ui: &mut egui::Ui, comp: usize, shape: &MaskShape) {
    // Keep this adapter local: other panels and the custom two-handle input have their own owners.
    fn scalar(
        app: &mut LightcraftApp,
        ui: &mut egui::Ui,
        comp: usize,
        spec: &ControlSpec,
        value: f64,
        mut set: impl FnMut(&mut LightcraftApp, f64) -> Result<serde_json::Value, String>,
    ) {
        use craft::behavior::{EditEvent, NumberCodec, Reader};
        use craft::ui::contract::{Bind, Props as _, Show as _};
        use craft::ui::{text::NumberField, value_drag::Slider};

        #[derive(Clone, Copy)]
        struct Visit {
            owner: (u64, Option<PhotoId>, Option<u32>),
            revision: u64,
            epoch: u64,
            editing: bool,
        }
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        let owner = (app.session.library_generation(), app.session.active(), app.session.active_mask);
        let key = ui.scope_id().with(("mask-range-visit", comp, spec.id));
        let revision = app.session.catalog.revision;
        let mut visit = ui.data(|d| d.get_temp::<Visit>(key)).unwrap_or(Visit { owner, revision, epoch: 0, editing: false });
        if visit.owner != owner || visit.revision != revision {
            // A foreign document/structural edit gives the slot a fresh SDK identity. Do not
            // cancel an old engine snapshot over a later edit; finish it without replaying input.
            if real && visit.editing && app.ui.dragging_control.as_deref() == Some(spec.id) {
                let _ = app.run("develop.endInteraction", json!({}));
                app.ui.dragging_control = None;
            }
            visit = Visit { owner, revision: app.session.catalog.revision, epoch: visit.epoch.wrapping_add(1), editing: false };
        }
        let t = Tokens::get(ui.ctx());
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, t.slider_row_h), Sense::hover());
        let label_rect = Rect::from_min_size(pos2(row.left() + 24.0, row.top() + 4.0), vec2(width - 46.0, 18.0));
        let track_rect = Rect::from_min_max(pos2(row.left() + 24.0, row.top() + 22.0), pos2(row.right() - 22.0, row.top() + 40.0));
        let value_rect = Rect::from_min_max(pos2(label_rect.right() - 64.0, label_rect.top()), label_rect.max);
        let id = ui.scope_id().with(("mask-range", owner, comp, spec.id, visit.epoch));
        let enabled = real && ui.is_enabled();
        let caption = crate::i18n::tr(spec.label);
        let label = Hit::new(&format!("maskRangeLabel:{comp}:{}", spec.id), crate::i18n::tr_format!("Reset {name}", name = caption))
            .id(id.with("label"))
            .enabled(enabled)
            .show_at(ui, label_rect);
        let label = label.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = caption));
        ui.painter().text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            caption,
            t.font(12.5),
            if enabled { t.text_label } else { t.text_disabled },
        );
        let track = Slider::new(Bind::given(value).reset(spec.default), spec.min..=spec.max)
            .id(id)
            .label(caption)
            .step(spec.step)
            .enabled(enabled)
            .show_at(ui, track_rect);
        let number_spec = *spec;
        let field = NumberField::new(crate::i18n::tr_format!("{name} value", name = caption), Bind::given(Some(value)).reset(Some(spec.default)))
            .id(id.with("value"))
            .range(spec.min..=spec.max)
            .step(spec.step)
            .codec(
                NumberCodec::new(Reader::new(), usize::from(spec.decimals)).parse_with(move |text| crate::widgets::typed_value(&number_spec, text)),
            )
            .snap_typed(true)
            .fixed_decimals(usize::from(spec.decimals))
            .commit_decimals(usize::from(spec.decimals))
            .enabled(enabled)
            .show_at(ui, value_rect);
        if !real {
            return;
        }
        for (prefix, response) in
            [("slider", &track.response), ("sliderValue", &field.response), ("sliderField", &field.response), ("sliderLabel", &label)]
        {
            // Keep legacy aliases and additionally disambiguate repeated component controls.
            crate::widgets::register_response(format!("{prefix}:{}", spec.id), response.rect, response);
            crate::widgets::register_response(format!("{prefix}:maskRange:{comp}:{}", spec.id), response.rect, response);
        }
        fn apply<T>(
            app: &mut LightcraftApp,
            spec: &ControlSpec,
            event: &EditEvent<T>,
            editing: &mut bool,
            live_begin: bool,
            number: impl Fn(&T) -> Option<f64>,
            set: &mut impl FnMut(&mut LightcraftApp, f64) -> Result<serde_json::Value, String>,
        ) {
            match event {
                EditEvent::Begin { .. } if live_begin => {
                    let _ = app.run("develop.beginInteraction", json!({"label":spec.label}));
                    app.ui.dragging_control = Some(spec.id.to_owned());
                    *editing = true;
                }
                EditEvent::Update { value, .. } if *editing => {
                    if let Some(value) = number(value).filter(|value| value.is_finite()) {
                        let _ = set(app, value);
                    }
                }
                EditEvent::Commit { after, .. } | EditEvent::Set { after, .. } => {
                    let after = number(after).filter(|value| value.is_finite());
                    if let Some(value) = after {
                        let _ = set(app, value);
                    }
                    if *editing {
                        // Clearing an optional field is not a numeric zero. A live edit
                        // with no final value restores its snapshot; a draft does nothing.
                        let end = if after.is_some() { "develop.endInteraction" } else { "develop.cancelInteraction" };
                        let _ = app.run(end, json!({}));
                        app.ui.dragging_control = None;
                        *editing = false;
                    }
                }
                EditEvent::Cancel { .. } if *editing => {
                    let _ = app.run("develop.cancelInteraction", json!({}));
                    app.ui.dragging_control = None;
                    *editing = false;
                }
                _ => {}
            }
        }
        if enabled {
            for event in &track.events {
                apply(app, spec, event, &mut visit.editing, true, |value| Some(*value), &mut set);
            }
            // Typing drafts stay local until commit, as in the original native field.
            // A pointer scrub or wheel run does begin a live engine interaction and keeps one Undo step.
            let live_number = ui.input(|i| i.pointer.primary_down() || i.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { .. })));
            for event in &field.events {
                apply(app, spec, event, &mut visit.editing, live_number, |value| *value, &mut set);
            }
            if label.double_clicked() {
                let _ = set(app, spec.default);
                if visit.editing {
                    let _ = app.run("develop.endInteraction", json!({}));
                    visit.editing = false;
                    app.ui.dragging_control = None;
                }
            }
        } else if visit.editing {
            let _ = app.run("develop.cancelInteraction", json!({}));
            app.ui.dragging_control = None;
            visit.editing = false;
        }
        visit.revision = app.session.catalog.revision;
        ui.data_mut(|d| d.insert_temp(key, visit));
    }
    let t = Tokens::get(ui.ctx());
    let update = |app: &mut LightcraftApp, shape: MaskShape| {
        let _ = app.run("mask.update", json!({"component": comp, "shape": shape}));
    };
    match shape {
        MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
            ui.label(egui::RichText::new(crate::i18n::tr("Select Luminance Range")).color(t.text_dim).size(11.5));
            let (lo, hi) = (*lo, *hi);
            let w = ui.available_width().min(240.0);
            let (_, rect) = ui.allocate_space(vec2(w, 18.0));
            let resp = Hit::new(&format!("lumRange:{comp}"), crate::i18n::tr("Select Luminance Range")).drag(true).show_at(ui, rect);
            let p = ui.painter();
            // dark → light ramp, the selected span outlined
            let n = 24;
            for k in 0..n {
                let a = k as f32 / n as f32;
                let r = Rect::from_min_max(
                    pos2(rect.left() + a * w, rect.top() + 4.0),
                    pos2(rect.left() + (a + 1.0 / n as f32) * w + 0.5, rect.bottom() - 4.0),
                );
                let g = (a * 255.0) as u8;
                p.rect_filled(r, 0.0, egui::Color32::from_gray(g));
            }
            let x = |v: f64| rect.left() + v.clamp(0.0, 1.0) as f32 * w;
            p.rect_stroke(
                Rect::from_min_max(pos2(x(lo), rect.top() + 2.0), pos2(x(hi), rect.bottom() - 2.0)),
                2.0,
                Stroke::new(1.5, t.accent),
                egui::StrokeKind::Middle,
            );
            for v in [lo, hi] {
                p.circle_filled(pos2(x(v), rect.center().y), 5.0, egui::Color32::WHITE);
                p.circle_stroke(pos2(x(v), rect.center().y), 5.0, Stroke::new(1.0, egui::Color32::from_gray(40)));
            }
            // Choose at the press position, then keep that handle for the entire drag. Testing
            // proximity on every frame can move the opposite endpoint after a fast/crossing drag.
            let handle_id = resp.id.with("luminance-handle");
            let nearer_low = |px: f32| (px - x(lo)).abs() <= (px - x(hi)).abs();
            if resp.drag_started() {
                if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                    ui.data_mut(|d| d.insert_temp(handle_id, nearer_low(origin.x)));
                }
                let _ = app.run("develop.beginInteraction", json!({"label": "Luminance Range"}));
            }
            if (resp.dragged() || resp.clicked())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let v = (((pos.x - rect.left()) / w) as f64).clamp(0.0, 1.0);
                let low =
                    if resp.dragged() { ui.data(|d| d.get_temp::<bool>(handle_id)).unwrap_or_else(|| nearer_low(pos.x)) } else { nearer_low(pos.x) };
                let (nlo, nhi) = if low { (v.min((hi - 0.01).max(0.0)), hi) } else { (lo, v.max((lo + 0.01).min(1.0))) };
                update(app, MaskShape::LuminanceRange { lo: nlo, hi: nhi, lo_feather: *lo_feather, hi_feather: *hi_feather });
            }
            if resp.drag_stopped() {
                ui.data_mut(|d| d.remove::<bool>(handle_id));
                let _ = app.run("develop.endInteraction", json!({}));
            }
            // Smoothness: both falloffs at once
            let smooth = ControlSpec {
                id: "smoothness",
                label: "Smoothness",
                section: Section::Light,
                min: 0.0,
                max: 100.0,
                default: 20.0,
                step: 1.0,
                decimals: 0,
                track: Track::Plain,
            };
            let cur = ((lo_feather + hi_feather) / 2.0 / 0.5 * 100.0).clamp(0.0, 100.0);
            scalar(app, ui, comp, &smooth, cur, |app, v| {
                let f = v / 100.0 * 0.5;
                app.run("mask.update", json!({"component": comp, "shape": MaskShape::LuminanceRange { lo, hi, lo_feather: f, hi_feather: f }}))
            });
            let mut map = app.ui.mask_overlay && app.ui.mask_overlay_mode == "colorOnBw";
            let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
            let changed = {
                use craft::ui::contract::{Bind, Props as _, Show as _};
                let out = craft::ui::choice::Checkbox::new(Bind::given(map))
                    .id_salt(("luminance-map", app.session.library_generation(), app.session.active(), app.session.active_mask, comp))
                    .label(crate::i18n::tr("Show Luminance Map"))
                    .enabled(real)
                    .show(ui);
                if real {
                    crate::widgets::register_response(format!("check:lumMap{comp}"), out.response.rect, &out.response);
                }
                if let Some(next) = out.events.committed() {
                    map = *next;
                    true
                } else {
                    false
                }
            };
            if real && ui.is_enabled() && changed {
                // the luminance map: the photo in black & white with the selected range tinted
                if map {
                    app.ui.luminance_map_restore = Some((app.ui.mask_overlay, app.ui.mask_overlay_mode.clone()));
                    app.ui.mask_overlay = true;
                    app.ui.mask_overlay_mode = "colorOnBw".into();
                } else {
                    let (on, mode) = app.ui.luminance_map_restore.take().unwrap_or((false, "color".into()));
                    app.ui.mask_overlay = on;
                    app.ui.mask_overlay_mode = if mode == "colorOnBw" { "color".into() } else { mode };
                }
            }
        }
        MaskShape::Object { edge, .. } | MaskShape::Prompt { edge, .. } => {
            // Edge: how crisp the selection's border is (−100 hard … 0 as computed … 100 soft)
            let spec = ControlSpec {
                id: "edge",
                label: "Edge",
                section: Section::Light,
                min: -100.0,
                max: 100.0,
                default: 0.0,
                step: 1.0,
                decimals: 0,
                track: Track::Centered,
            };
            let base = shape.clone();
            scalar(app, ui, comp, &spec, *edge, |app, v| {
                let mut s = base.clone();
                if let MaskShape::Object { edge, .. } | MaskShape::Prompt { edge, .. } = &mut s {
                    *edge = v.clamp(-100.0, 100.0);
                }
                app.run("mask.update", json!({"component": comp, "shape": s}))
            });
            ui.label(egui::RichText::new(crate::i18n::tr("− harder border · + softer border")).color(t.text_dim).size(11.0));
        }
        MaskShape::ColorRange { samples, refine } => {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::tr_format!("{} sample{}", samples.len(), if samples.len() == 1 { "" } else { "s" }))
                        .color(t.text_dim)
                        .size(11.5),
                );
                let picking = app.ui.tool == "colorRange";
                if text_button(ui, &format!("colorPick{comp}"), crate::i18n::tr("Pick"), picking)
                    .on_hover_text(crate::i18n::tr("Click the photo to pick a colour; ⇧-click adds more (up to 5)"))
                    .clicked()
                {
                    app.ui.tool = if picking { String::new() } else { "colorRange".into() };
                }
            });
            let spec = ControlSpec {
                id: "refine",
                label: "Refine",
                section: Section::Color,
                min: 0.0,
                max: 100.0,
                default: 50.0,
                step: 1.0,
                decimals: 0,
                track: Track::Plain,
            };
            let samples = samples.clone();
            scalar(app, ui, comp, &spec, *refine, |app, v| {
                app.run("mask.update", json!({"component": comp, "shape": MaskShape::ColorRange { samples: samples.clone(), refine: v }}))
            });
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "mask_range_controls_flow_tests.rs"]
mod mask_range_controls_flow_tests;

#[derive(Clone, Copy, PartialEq, Eq)]
struct ComponentMenuTarget {
    library: u64,
    photo: Option<PhotoId>,
    revision: u64,
    mask: u32,
    component: usize,
}

impl ComponentMenuTarget {
    fn current(app: &LightcraftApp, mask: u32, component: usize) -> Self {
        Self { library: app.session.library_generation(), photo: app.session.active(), revision: app.session.catalog.revision, mask, component }
    }
    fn live(self, app: &LightcraftApp) -> bool {
        self == Self::current(app, self.mask, self.component)
            && app.session.active_mask == Some(self.mask)
            && self
                .photo
                .and_then(|id| app.session.develop_of(id))
                .is_some_and(|d| d.masks.iter().any(|m| m.id == self.mask && m.components.get(self.component).is_some()))
    }
    fn key(self) -> (u64, Option<PhotoId>, u64, u32, usize) {
        (self.library, self.photo, self.revision, self.mask, self.component)
    }
}


enum ComponentRowAction {
    Command(serde_json::Value),
    Rename,
}

#[allow(clippy::too_many_arguments)]
fn component_row_popup(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    row: &egui::Response,
    more: &egui::Response,
    mask: u32,
    k: usize,
    label: &str,
    count: usize,
) {
    use craft::ui::contract::{Props as _, ShowContainer as _, Trigger};
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    let target = ComponentMenuTarget::current(app, mask, k);
    if row.secondary_clicked() && crate::widgets::carrying(ui.ctx()) {
        return;
    }
    let owner_key = ui.scope_id().with(("mask-component-captured-owner", row.id, more.id));
    let mut captured = ui.data(|d| d.get_temp::<ComponentMenuTarget>(owner_key)).unwrap_or(target);
    let interactive = captured == target && captured.live(app);
    let mut pending = None;
    let mut targets = Vec::new();
    let mut body_layer = None;
    // Keep SDK-owned opener state: an explicit false open binding bypasses opener triggers.
    // This id belongs to the opener slot, not the target currently occupying that row.
    let shown = craft::ui::menu::Menu::context(crate::i18n::tr("Component options"), row)
        .opened_by(more, Trigger::Click)
        .id_salt(("mask-component-menu", row.id, more.id))
        .show(ui, |ui, menu| {
            if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
                body_layer = Some(ui.layer_id());
            }
            ui.add_enabled_ui(interactive, |ui| {
                pending = component_row_menu(app, ui, captured, label, count, &mut targets);
            });
            if pending.is_some() {
                menu.request_close();
            }
        });
    let opening = craft::ui::runtime::overlay::opening(ui.ctx(), shown.output.response.id);
    let active = body_layer.and_then(|layer| egui::AreaState::load(ui.ctx(), layer.id)).is_some_and(|state| state.interactable);
    if opening && body_layer.is_some() {
        // Only the SDK's genuine open transition establishes a new document owner.
        captured = target;
        ui.data_mut(|d| d.insert_temp(owner_key, captured));
    } else if !interactive && active {
        craft::ui::runtime::overlay::request_close(ui.ctx(), shown.output.response.id, craft::ui::runtime::overlay::CloseReason::Requested);
    }
    #[cfg(test)]
    {
        let receipt = format!(
            "pass={} context_clicked={} options_pressed={} options_clicked={} body={body_layer:?} sdk_id={:?} opening={opening} active={active} interactive={interactive} captured={:?} target={:?}",
            ui.ctx().cumulative_pass_nr(),
            row.secondary_clicked(),
            more.is_pointer_button_down_on(),
            more.clicked(),
            shown.output.response.id,
            captured.key(),
            target.key()
        );
        ui.data_mut(|d| d.insert_temp(egui::Id::unique(("mask-component-opener-diagnostic", mask, k)), receipt));
    }

    // A closing/fading body may still paint. Only CURRENT interactable real layers publish
    // automation or app effects; query after the menu has updated its AreaState.
    for (name, response) in targets {
        if interactive && egui::AreaState::load(ui.ctx(), response.layer_id.id).is_some_and(|state| state.interactable) {
            crate::widgets::register_response(name, response.rect, &response);
        }
    }
    if let Some((action, layer)) = pending
        && ui.is_enabled()
        && interactive
        && !opening
        && captured.live(app)
        && egui::AreaState::load(ui.ctx(), layer.id).is_some_and(|state| state.interactable)
    {
        match action {
            ComponentRowAction::Command(p) => {
                let _ = app.run("mask.component", p);
            }
            ComponentRowAction::Rename => {
                app.ui.renaming_component = Some((mask, k, label.to_owned()));
            }
        }
    }
}

fn component_action(ui: &mut egui::Ui, target: ComponentMenuTarget, key: &str, text: &str, targets: &mut Vec<(String, egui::Response)>) -> bool {
    let (mask, k) = (target.mask, target.component);
    use craft::ui::contract::{Props as _, Show as _};
    let out = craft::ui::menu::MenuItem::new(menu_label(text).to_owned())
        .id_salt(("mask-component-action", target.key(), key))
        .enabled(ui.is_enabled())
        .show(ui);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return false;
    }
    let ran = ui.is_enabled() && out.ran;
    targets.push((format!("maskComponent:{mask}:{k}:{key}"), out.response));
    ran
}

/// The right-click / "…" action body. The host owns lifetime, target identity and dispatch.
fn component_row_menu(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    target: ComponentMenuTarget,
    label: &str,
    count: usize,
    targets: &mut Vec<(String, egui::Response)>,
) -> Option<(ComponentRowAction, egui::LayerId)> {
    use craft::ui::display::Divider;
    let (mask, k) = (target.mask, target.component);
    let mut action = None;
    for (key, text) in
        [("invert", crate::i18n::tr("Invert").to_owned()), ("duplicate", crate::i18n::tr_format!("Duplicate \"{label}\"", label = label))]
    {
        if component_action(ui, target, key, &text, targets) {
            action = Some((ComponentRowAction::Command(json!({"id":mask,"component":k,"action":key})), ui.layer_id()));
        }
    }
    if k > 0 {
        let head = Menu::submenu_row_enabled(ui, crate::i18n::tr("Mode"), ui.is_enabled(), |ui, _| {
            for (op, text) in [("add", "Add"), ("subtract", "Subtract"), ("intersect", "Intersect")] {
                if component_action(ui, target, op, text, targets) {
                    action = Some((ComponentRowAction::Command(json!({"id":mask,"component":k,"action":"op","op":op})), ui.layer_id()));
                }
            }
        });
        if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
            targets.push((format!("maskComponent:{mask}:{k}:mode"), head.response));
        }
    }
    // Keep the existing component-creation menus and their model/download policy unchanged.
    for (label, op) in [("Intersect with", "intersect"), ("Subtract", "subtract")] {
        Menu::submenu_row_enabled(ui, crate::i18n::tr(label), ui.is_enabled(), |ui, _| component_menu(app, ui, op, ("mask-component-row", op, target.key())));
    }
    Divider::horizontal().show(ui);
    if component_action(ui, target, "rename", crate::i18n::tr("Rename…"), targets) {
        action = Some((ComponentRowAction::Rename, ui.layer_id()));
    }
    let del = if count == 1 {
        crate::i18n::tr_format!("Delete \"{label}\" (and the mask)", label = label)
    } else {
        crate::i18n::tr_format!("Delete \"{label}\"", label = label)
    };
    if component_action(ui, target, "delete", &del, targets) {
        action = Some((ComponentRowAction::Command(json!({"id":mask,"component":k,"action":"delete"})), ui.layer_id()));
    }
    action
}


/// Whether the SAM 3 model is missing in a build that could use it: then the download is
/// offered (with `then` to start afterwards) instead of starting an AI mask.
fn needs_model(app: &mut LightcraftApp, kind: &str, op: &str) -> bool {
    let seg = &app.session.segmenter;
    let missing = lightcraft_engine::segment::Segmenter::AVAILABLE && seg.remote_endpoint().is_none() && seg.dir.is_some() && !seg.installed();
    if missing {
        app.offer_sam_download(Some((kind, op)));
    }
    missing
}

/// Start an Object selection (SAM 3 clicks): a new mask, or a component of the selected one
/// combined by `op`; the photo is analyzed meanwhile (in the background).
pub(crate) fn start_object(app: &mut LightcraftApp, ctx: &egui::Context, op: &str) {
    if needs_model(app, "object", op) {
        return;
    }
    let r = if op == "new" {
        app.run_typed("mask.add", json!({"kind": "object"}))
    } else {
        app.run_typed("mask.addComponent", json!({"op": op, "kind": "object"}))
    };
    match r {
        Ok(_) => {
            app.ui.tool = "object".into();
            app.toast(ctx, crate::i18n::tr("Click the object to select it · ⌥-click leaves a part out"));
        }
        Err(e) => app.ai_error(ctx, e, Some(("object", op))),
    }
}

/// Start an AI mask of `kind` (object|prompt) combined by `op` (new|add|subtract|intersect),
/// after the model was installed.
pub(crate) fn begin_ai(app: &mut LightcraftApp, kind: &str, op: &str) -> Result<serde_json::Value, String> {
    match kind {
        "object" => {
            let r = if op == "new" {
                app.run("mask.add", json!({"kind": "object"}))
            } else {
                app.run("mask.addComponent", json!({"op": op, "kind": "object"}))
            }?;
            app.ui.tool = "object".into();
            Ok(r)
        }
        _ => {
            app.ui.describe = Some((op.to_string(), String::new()));
            Ok(serde_json::Value::Null)
        }
    }
}

/// Open the Describe field (a new mask, or a component combined by `op`).
pub(crate) fn start_describe(app: &mut LightcraftApp, op: &str) {
    if needs_model(app, "prompt", op) {
        return;
    }
    app.ui.describe = Some((op.to_string(), String::new()));
}

/// The SAM model's download, `width` wide. The label is a percentage once the size is known and
/// just the model's name before, so the bar keeps its height and its accessible name.
pub(crate) fn sam_download_bar(ui: &mut egui::Ui, done: u64, total: u64, width: f32) -> egui::Response {
    let fraction = crate::progress::fraction_of(done, total);
    let label = match fraction {
        Some(f) => format!("SAM 3: {} %", crate::progress::percent(f)),
        None => "SAM 3".to_string(),
    };
    crate::progress::Progress::new(fraction).label(label).width(width).widget("maskSamProgress").show(ui)
}

/// Whether the segmenter has work (loading, analyzing, a request, a detail pass running or waiting
/// to start): nothing new starts then.
fn segmenter_working(analyzing: bool, busy: bool, detail_busy: bool, detail_due: bool) -> bool {
    analyzing || busy || detail_busy || detail_due
}

/// Enter in the Describe field is the Select button: it starts nothing while the segmenter works.
fn enter_submits(working: bool, lost_focus: bool, enter_pressed: bool) -> bool {
    !working && lost_focus && enter_pressed
}

/// The Describe field: type what to select ("sky", "the red car") and press Return.
fn describe_field(app: &mut LightcraftApp, ui: &mut egui::Ui, new_mask: bool) {
    use craft::ui::buttons::Button;
    let Some((op, mut text)) = app.ui.describe.clone() else { return };
    // a new mask's field sits under the tiles; one that combines, under the selected mask
    if (op == "new") != new_mask {
        return;
    }
    ui.add_space(8.0);
    let prompt = match op.as_str() {
        "subtract" => "Describe what to leave out",
        "intersect" => "Describe what to keep",
        _ => "Describe what to select",
    };
    ui.label(crate::i18n::tr(prompt));
    let mut submit = false;
    let mut cancelled = false;
    // the (translated) Select button is placed first, at the right; the field takes what is left
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let seg = &app.session.segmenter;
        let working = segmenter_working(seg.analyzing(), seg.busy(), seg.detail_busy(), app.ui.detail_due.is_some());
        let go = Button::new(crate::i18n::tr("Select").to_owned())
            .id(egui::Id::unique("button:maskDescribeGo"))
            .size(craft::theme::ControlSize::Sm)
            .loading(working)
            .show(ui);
        if !measuring(ui) {
            crate::widgets::register_response("button:maskDescribeGo", go.response.rect, &go.response);
            if ui.is_enabled() && go.clicked() {
                submit = true;
            }
        }
        let r = crate::text_field::TextField::singleline("maskDescribe", &mut text)
            .hint(crate::i18n::tr("e.g. sky · the red car · car, road"))
            .width(ui.available_width())
            .show(ui);
        if !r.editing && r.ending.is_none() && text.is_empty() {
            crate::text_field::focus_id(ui.ctx(), r.response.id);
        }
        // Enter is the Select button: nothing starts while the segmenter is working
        if enter_submits(working, r.ending.is_some(), r.ending == Some(crate::text_field::Ending::Return)) {
            submit = true;
        }
        cancelled = r.cancelled();
    });
    if cancelled || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.describe = None;
        return;
    }
    app.ui.describe = Some((op.clone(), text.clone()));
    if submit && !text.trim().is_empty() {
        let r = if op == "new" {
            app.run_typed("mask.add", json!({"kind": "prompt", "text": text}))
        } else {
            app.run_typed("mask.addComponent", json!({"op": op, "kind": "prompt", "text": text}))
        };
        match r {
            // the mask appears when the model has found it (a detail pass follows)
            Ok(_) => app.ui.describe = None,
            Err(e) => app.ai_error(ui.ctx(), e, Some(("prompt", op.as_str()))),
        }
    }
}

/// The menu a button opens: the framework's, kept open by the button's own activation (the
/// pointer, a long press and the keyboard alike), and closed by Escape or a click away. `key`
/// tells this opener's menu from the others'. Nothing is drawn while only measuring.
fn opener_menu(ui: &mut egui::Ui, opener: &crate::widgets::TextButtonOutput, key: impl std::hash::Hash + std::fmt::Debug, label: &str, mut body: impl FnMut(&mut egui::Ui)) {
    use craft::ui::contract::{Bind, Trigger};
    if measuring(ui) {
        return;
    }
    let pass = ui.ctx().cumulative_pass_nr();
    let open_id = opener.response.id.with("mask-opener-menu-open");
    let mut open = ui
        .data(|d| d.get_temp::<(u64, bool)>(open_id))
        .filter(|(last, _)| *last == pass || last.saturating_add(1) == pass)
        .is_some_and(|(_, open)| open);
    // The button's own activation toggles; Manual keeps the menu's pointer trigger from toggling a second time.
    if opener.clicked() {
        open = !open;
    }
    Menu::new(crate::i18n::tr(label).to_owned())
        .id_salt(("mask-opener-menu", key))
        .opened_by(&opener.response, Trigger::Manual)
        .open(Bind::bound(&mut open))
        .show(ui, |ui, _| body(ui));
    ui.data_mut(|d| {
        if open {
            d.insert_temp(open_id, (pass, true));
        } else {
            d.remove::<(u64, bool)>(open_id);
        }
    });
}

/// The choices of a component menu: Object, Describe… and the shapes. `scope` tells this menu's
/// rows from those of the other menus with the same `op`.
fn component_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, op: &str, scope: impl std::hash::Hash + std::fmt::Debug + Copy) {
    let item = |ui: &mut egui::Ui, kind: &str, label: &str| -> bool {
        let enabled = ui.is_enabled();
        let shown = MenuItem::new(label.to_owned()).id_salt(("mask-comp", scope, op, kind)).enabled(enabled).show(ui);
        if measuring(ui) {
            return false;
        }
        crate::widgets::register_response(format!("maskComp:{op}:{kind}"), shown.response.rect, &shown.response);
        enabled && shown.ran
    };
    if item(ui, "object", crate::i18n::tr("Object")) {
        start_object(app, ui.ctx(), op);
    }
    if item(ui, "prompt", crate::i18n::tr("Describe…")) {
        start_describe(app, op);
    }
    for (kind, label) in [
        ("brush", "Brush"),
        ("linear", "Linear Gradient"),
        ("radial", "Radial Gradient"),
        ("sky", "Sky"),
        ("subject", "Subject"),
        ("luminanceRange", "Luminance Range"),
    ] {
        // painting only adds or erases
        if kind == "brush" && op == "intersect" {
            continue;
        }
        if item(ui, kind, menu_label(label)) {
            if kind == "brush" {
                let _ = app.run("tool.brush", json!({"op": op}));
            } else {
                let _ = app.run("mask.addComponent", json!({"op": op, "kind": kind}));
            }
        }
    }
}

fn brush_settings(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use craft::ui::contract::{Bind, Props as _, Show as _};

    fn check(ui: &mut egui::Ui, owner: (u64, Option<PhotoId>, Option<u32>), name: &str, label: &str, hint: &str, value: &mut bool) {
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        let next = {
            let out = craft::ui::choice::Checkbox::new(Bind::given(*value))
                .id_salt(("mask-brush", owner, name))
                .label(crate::i18n::tr(label).to_owned())
                .enabled(real)
                .show(ui);
            if real {
                crate::widgets::register_response(format!("check:{name}"), out.response.rect, &out.response);
            }
            out.response.clone().on_hover_text(crate::i18n::tr(hint));
            out.events.committed().copied()
        };
        if real
            && ui.is_enabled()
            && let Some(next) = next
        {
            *value = next;
        }
    }

    // View preferences have no engine gesture or Undo. Keep exact typing commit-only,
    // while actual slider/value scrubs update live and Cancel restores their prior value.
    fn scalar(app: &LightcraftApp, ui: &mut egui::Ui, spec: &ControlSpec, current: f64) -> Option<f64> {
        use craft::behavior::{EditEvent, NumberCodec, Reader};
        use craft::ui::{text::NumberField, value_drag::Slider};

        #[derive(Clone, Copy)]
        struct Visit {
            owner: (u64, Option<PhotoId>, Option<u32>),
            revision: u64,
            epoch: u64,
            pass: u64,
            enabled: bool,
            number_start: Option<f64>,
        }
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        let owner = (app.session.library_generation(), app.session.active(), app.session.active_mask);
        let revision = app.session.catalog.revision;
        let pass = ui.ctx().cumulative_pass_nr();
        let enabled = real && ui.is_enabled();
        let key = ui.scope_id().with(("mask-brush-visit", spec.id));
        let mut visit = ui.data(|data| data.get_temp::<Visit>(key)).unwrap_or(Visit { owner, revision, epoch: 0, pass, enabled, number_start: None });
        let adjacent = visit.pass == pass || visit.pass.checked_add(1) == Some(pass);
        if visit.owner != owner || visit.revision != revision || visit.enabled != enabled || !adjacent {
            // A different target, hidden visit or disabled scope must not revive a pending draft.
            visit = Visit { owner, revision, epoch: visit.epoch.wrapping_add(1), pass, enabled, number_start: None };
        }
        visit.pass = pass;
        let t = Tokens::get(ui.ctx());
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, t.slider_row_h), Sense::hover());
        let label_rect = Rect::from_min_size(pos2(row.left() + 24.0, row.top() + 4.0), vec2(width - 46.0, 18.0));
        let track_rect = Rect::from_min_max(pos2(row.left() + 24.0, row.top() + 22.0), pos2(row.right() - 22.0, row.top() + 40.0));
        let value_rect = Rect::from_min_max(pos2(label_rect.right() - 64.0, label_rect.top()), label_rect.max);
        let id = ui.scope_id().with(("mask-brush", owner, spec.id, visit.epoch));
        let caption = crate::i18n::tr(spec.label);
        let label = Hit::new(&format!("maskBrushLabel:{}", spec.id), crate::i18n::tr_format!("Reset {name}", name = caption))
            .id(id.with("label"))
            .enabled(enabled)
            .show_at(ui, label_rect);
        let label = label.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = caption));
        ui.painter().text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            caption,
            t.font(12.5),
            if enabled { t.text_label } else { t.text_disabled },
        );
        let mut value = current;
        let track = Slider::new(Bind::bound(&mut value).reset(spec.default), spec.min..=spec.max)
            .id(id)
            .label(caption)
            .step(spec.step)
            .enabled(enabled)
            .show_at(ui, track_rect);
        let number_spec = *spec;
        let field = NumberField::new(crate::i18n::tr_format!("{name} value", name = caption), Bind::given(Some(current)).reset(Some(spec.default)))
            .id(id.with("value"))
            .range(spec.min..=spec.max)
            .step(spec.step)
            .codec(
                NumberCodec::new(Reader::new(), usize::from(spec.decimals)).parse_with(move |text| crate::widgets::typed_value(&number_spec, text)),
            )
            .snap_typed(true)
            .fixed_decimals(usize::from(spec.decimals))
            .commit_decimals(usize::from(spec.decimals))
            .enabled(enabled)
            .show_at(ui, value_rect);
        if !real {
            return None;
        }
        for (prefix, response) in
            [("slider", &track.response), ("sliderValue", &field.response), ("sliderField", &field.response), ("sliderLabel", &label)]
        {
            crate::widgets::register_response(format!("{prefix}:{}", spec.id), response.rect, response);
        }
        if enabled {
            let live_number =
                ui.input(|input| input.pointer.primary_down() || input.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { .. })));
            for event in &field.events {
                match event {
                    EditEvent::Begin { .. } => visit.number_start = live_number.then_some(current),
                    EditEvent::Update { value: Some(next), .. } if visit.number_start.is_some() => value = *next,
                    EditEvent::Commit { after, .. } | EditEvent::Set { after, .. } => {
                        if let Some(next) = after {
                            value = *next;
                        } else if let Some(before) = visit.number_start {
                            // An absent numeric value is not zero and does not clear a brush preference.
                            value = before;
                        }
                        visit.number_start = None;
                    }
                    EditEvent::Cancel { .. } => {
                        if let Some(before) = visit.number_start.take() {
                            value = before;
                        }
                    }
                    _ => {}
                }
            }
            if label.double_clicked() {
                value = spec.default;
                visit.number_start = None;
            }
        } else {
            visit.number_start = None;
        }
        ui.data_mut(|data| data.insert_temp(key, visit));
        (enabled && value.is_finite() && value != current).then_some(value.clamp(spec.min, spec.max))
    }

    let t = Tokens::get(ui.ctx());
    let owner = (app.session.library_generation(), app.session.active(), app.session.active_mask);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 0 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Brush")).font(t.semibold(13.0)));
            if text_button(ui, "brushAdd", crate::i18n::tr("Add"), !app.ui.brush_erase).clicked() {
                app.ui.brush_erase = false;
            }
            if text_button(ui, "brushErase", crate::i18n::tr("Erase"), app.ui.brush_erase).clicked() {
                app.ui.brush_erase = true;
            }
        });
        check(ui, owner, "brushAutoMask", "Auto Mask", "Paint only areas like the one under the brush", &mut app.ui.brush_auto_mask);
        check(ui, owner, "brushPressure", "Pen Pressure", "A pen's pressure sets the size and flow of the brush", &mut app.ui.brush_pressure);
    });
    for (id, label, min, max, get) in [
        ("ui.brushSize", "Size", 1.0, 100.0, (app.ui.brush_size * 400.0) as f64),
        ("ui.brushFeather", "Feather", 0.0, 100.0, app.ui.brush_feather as f64),
        ("ui.brushFlow", "Flow", 1.0, 100.0, app.ui.brush_flow as f64),
    ] {
        let s = ControlSpec { id, label, section: Section::Light, min, max, default: min, step: 1.0, decimals: 0, track: Track::Plain };
        if let Some(v) = scalar(app, ui, &s, get) {
            match id {
                "ui.brushSize" => app.ui.brush_size = (v / 400.0) as f32,
                "ui.brushFeather" => app.ui.brush_feather = v as f32,
                _ => app.ui.brush_flow = v as f32,
            }
        }
    }
}

#[cfg(test)]
#[path = "mask_brush_settings_flow_tests.rs"]
mod mask_brush_settings_flow_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detail_pass_counts_as_the_segmenter_working() {
        assert!(segmenter_working(false, false, true, false));
        assert!(segmenter_working(true, false, false, false));
        assert!(segmenter_working(false, true, false, false));
        assert!(!segmenter_working(false, false, false, false));
    }

    #[test]
    fn a_pending_detail_pass_counts_as_the_segmenter_working() {
        // a new prompt finishing before it starts would replace its target, and that mask never refines
        assert!(segmenter_working(false, false, false, true));
    }

    #[test]
    fn enter_in_the_describe_field_starts_nothing_while_the_segmenter_works() {
        assert!(enter_submits(false, true, true));
        assert!(!enter_submits(true, true, true));
        assert!(!enter_submits(false, false, true));
        assert!(!enter_submits(false, true, false));
    }

    /// The grid keeps four 52 pt tiles when the labels fit, and gives a long (translated) label
    /// room by dropping a column rather than drawing it past its tile.
    #[test]
    fn tile_labels_get_the_room_they_need() {
        let short = |size: f32| size * 4.0; // "Luminance"-ish: 42 pt at 10.5
        let long = |size: f32| size * 5.6; // "Background"/"Hintergrund": 59 pt at 10.5, 53 at 9.5
        assert_eq!(tile_layout(224.0, short), (4, 51.0, 10.5));
        // too wide for four at either size: three tiles, wider than 52 so the label fits
        let (n, w, size) = tile_layout(224.0, long);
        assert_eq!((n, size), (3, 10.5));
        assert!(w >= long(10.5) + 2.0 * TILE_PAD && w <= (224.0 - 2.0 * TILE_GAP) / 3.0, "{w}");
        // a smaller label keeps a column: 9.5 pt fits where 10.5 doesn't
        let mid = |size: f32| size * 4.6; // 48.3 at 10.5, 43.7 at 9.5
        assert_eq!(tile_layout(224.0, mid), (4, 51.0, 9.5));
        // a wide panel: tiles stay 52 pt (or as wide as the label)
        assert_eq!(tile_layout(474.0, short).0, 4);
        assert_eq!(tile_layout(474.0, short).1, TILE);
        // far too narrow for any label: two tiles (labels cut short), never a negative width
        let (n, w, _) = tile_layout(60.0, long);
        assert_eq!(n, 2);
        assert!(w >= 24.0);
        assert_eq!(tile_layout(f32::NAN, long).0, 2);
    }
}

#[cfg(test)]
#[path = "mask_component_actions_flow_tests.rs"]
mod mask_component_actions_flow_tests;

#[cfg(test)]
#[path = "mask_local_consumer_flow_tests.rs"]
mod mask_local_consumer_flow_tests;
