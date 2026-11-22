//! The Masking panel: create masks, list them, edit components and local adjustments.

use egui::{Align2, Rect, Sense, Stroke, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{ControlSpec, LocalAdjustments, MaskShape, Section, Track};
use serde_json::json;

use super::edit::apply_slider_out;
use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{divider, flyout_row, icon_button, one_line, register, slider, text_button};

const fn spec(id: &'static str, label: &'static str, min: f64, max: f64, step: f64, decimals: u8, track: Track) -> ControlSpec {
    ControlSpec { id, label, section: Section::Light, min, max, default: 0.0, step, decimals, track, shade: lcc_develop::Shade::Plain }
}

/// `s` with the grey ramp of its Edit twin (drawn only in the Compact layout).
const fn shaded(s: ControlSpec, shade: lcc_develop::Shade) -> ControlSpec {
    ControlSpec { shade, ..s }
}

/// Slider `s` as Settings ▸ Expanded sliders allows: off, the mask Hue keeps Lightroom's range
/// (about ±34°, [`lcc_develop::LOCAL_HUE_STANDARD`]); on, ±180°. Only the range shown changes; a
/// stored value past it is kept as it is.
pub fn local_spec(s: &ControlSpec, expanded: bool) -> ControlSpec {
    if s.id == "hue" && !expanded { ControlSpec { min: -lcc_develop::LOCAL_HUE_STANDARD, max: lcc_develop::LOCAL_HUE_STANDARD, ..*s } } else { *s }
}

/// Local adjustment sliders (key = `LocalAdjustments` field name).
pub const LOCAL: &[ControlSpec] = &[
    spec("temp", "Temp", -100.0, 100.0, 1.0, 0, Track::Temp),
    spec("tint", "Tint", -100.0, 100.0, 1.0, 0, Track::Tint),
    shaded(spec("exposure", "Exposure", -4.0, 4.0, 0.01, 2, Track::Centered), lcc_develop::Shade::Ramp),
    shaded(spec("contrast", "Contrast", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::ReverseRamp),
    shaded(spec("highlights", "Highlights", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::Ramp),
    shaded(spec("shadows", "Shadows", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::Ramp),
    shaded(spec("whites", "Whites", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::Ramp),
    shaded(spec("blacks", "Blacks", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::Ramp),
    spec("texture", "Texture", -100.0, 100.0, 1.0, 0, Track::Centered),
    shaded(spec("clarity", "Clarity", -100.0, 100.0, 1.0, 0, Track::Centered), lcc_develop::Shade::ReverseRamp),
    spec("dehaze", "Dehaze", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("hue", "Hue", -lcc_develop::LOCAL_HUE_MAX, lcc_develop::LOCAL_HUE_MAX, 1.0, 0, Track::Rainbow),
    spec("saturation", "Saturation", -100.0, 100.0, 1.0, 0, Track::Gradient { from: "#7a7a7a", to: "#e04a3a" }),
    spec("sharpness", "Sharpness", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("noise", "Noise", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("moire", "Moiré", -100.0, 100.0, 1.0, 0, Track::Centered),
    spec("defringe", "Defringe", -100.0, 100.0, 1.0, 0, Track::Centered),
];

/// The mask's Color tint (Lightroom's Color swatch under Saturation, issue #1145): the hue in
/// degrees round the wheel and how strongly the masked area takes it.
pub const TINT: [ControlSpec; 2] = [
    spec("color_hue", "Hue", 0.0, 360.0, 1.0, 0, Track::Rainbow),
    spec("color_sat", "Saturation", 0.0, 100.0, 1.0, 0, Track::Gradient { from: "#7a7a7a", to: "#e04a3a" }),
];

/// The swatch colour of a tint: `hue` in degrees, `sat` 0..100 (no tint: `None`).
pub(crate) fn tint_swatch(hue: f64, sat: f64) -> Option<egui::Color32> {
    if !(sat.is_finite() && hue.is_finite()) || sat <= 0.0 {
        return None;
    }
    let h = (hue.rem_euclid(360.0) / 360.0) as f32;
    let s = (sat.clamp(0.0, 100.0) / 100.0) as f32;
    Some(egui::Color32::from(egui::ecolor::Hsva::new(h, s, 1.0, 1.0)))
}

/// The mask's slider sections (issue #642), as Lightroom Classic groups them: (fold key, title,
/// the [`LOCAL`] sliders in it). Color Grading follows Color ([`grading_section`]).
pub const GROUPS: &[(&str, &str, &[&str])] = &[
    ("light", "Light", &["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]),
    ("color", "Color", &["temp", "tint", "hue", "saturation", "color_sat"]),
    ("effects", "Effects", &["texture", "clarity", "dehaze"]),
    ("detail", "Detail", &["sharpness", "noise", "moire", "defringe"]),
];

/// The colour-grading zones of a mask: (key in the `grade_<key>_*` fields, label).
pub const GRADE_ZONES: [(&str, &str); 4] = [("shadows", "Shadows"), ("midtones", "Midtones"), ("highlights", "Highlights"), ("global", "Global")];

/// The sliders of one colour-grading zone (ids are the `LocalAdjustments` field names).
pub fn grade_specs(zone: &str) -> [ControlSpec; 3] {
    let (h, s, l): (&'static str, &'static str, &'static str) = match zone {
        "midtones" => ("grade_midtones_hue", "grade_midtones_sat", "grade_midtones_lum"),
        "highlights" => ("grade_highlights_hue", "grade_highlights_sat", "grade_highlights_lum"),
        "global" => ("grade_global_hue", "grade_global_sat", "grade_global_lum"),
        _ => ("grade_shadows_hue", "grade_shadows_sat", "grade_shadows_lum"),
    };
    let c = |id, label, min, max, track| ControlSpec {
        id,
        label,
        section: Section::Grading,
        min,
        max,
        default: 0.0,
        step: 1.0,
        decimals: 0,
        track,
        shade: lcc_develop::Shade::Plain,
    };
    [c(h, "Hue", 0.0, 360.0, Track::Rainbow), c(s, "Saturation", 0.0, 100.0, Track::Plain), c(l, "Luminance", -100.0, 100.0, Track::Centered)]
}

/// Does any slider of a section differ from 0?
fn group_changed(a: &LocalAdjustments, ids: &[&str]) -> bool {
    ids.iter().any(|k| local_get(a, k).abs() > 1e-9)
}

/// A mask slider section's title row: click folds it shut and open again (`section:mask:<key>`, kept
/// in [`crate::state::UiState::folded_sections`], so open unless folded). A dot after the title
/// marks a section whose sliders are not all at 0, so a folded one still shows it does something.
/// Says whether the section is open.
fn section_header(app: &mut LccApp, ui: &mut egui::Ui, key: &str, title: &str, changed: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let id = format!("mask:{key}");
    let open = !app.ui.section_folded(&id);
    let title = crate::i18n::tr(title);
    // (Compact: the Edit panel's banded section header, LAYOUT-DENSE)
    let dense = t.dense;
    let h = if dense { crate::theme::COMPACT_SECTION_H } else { 30.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("section:{id}"), r);
    // the section's sliders share one name column (Compact)
    crate::widgets::begin_slider_group(ui, &id);
    let p = ui.painter();
    if dense {
        crate::widgets::paint_section_band(p, r, &t, resp.hovered());
    } else if resp.hovered() {
        p.rect_filled(r, 0.0, t.chrome.gamma_multiply(1.06));
    }
    let (chev_x, title_x, colour) = if dense {
        (r.left() + crate::widgets::PAD_L + 5.0, r.left() + crate::widgets::PAD_L + 16.0, t.text)
    } else {
        (r.left() + 20.0, r.left() + 32.0, t.text_label)
    };
    let chevron = Rect::from_center_size(pos2(chev_x, r.center().y), vec2(12.0, 12.0));
    paint(p, chevron, if open { Icon::ChevronDown } else { Icon::ChevronRight }, if dense { t.text } else { t.icon });
    let g = p.layout_no_wrap(title.to_string(), t.semibold(13.0), colour);
    let at = pos2(title_x, r.center().y - g.size().y / 2.0);
    let end = at.x + g.size().x;
    p.galley(at, g, colour);
    if changed {
        p.circle_filled(pos2(end + 8.0, r.center().y), 2.5, t.accent);
    }
    if resp.clicked() {
        app.ui.toggle_folded(&id);
    }
    open
}

/// The Color Grading section inside a mask (issue #642): one zone at a time (Shadows, Midtones,
/// Highlights, Global, picked by the buttons), its Hue, Saturation and Luminance.
fn grading_section(app: &mut LccApp, ui: &mut egui::Ui, a: &LocalAdjustments) {
    let ids: Vec<&str> = GRADE_ZONES.iter().flat_map(|(z, _)| grade_specs(z).map(|s| s.id)).collect();
    if !section_header(app, ui, "grading", "Color Grading", group_changed(a, &ids)) {
        return;
    }
    let zone_id = egui::Id::new("maskGradeZone");
    let zone: &str = ui.ctx().data(|d| d.get_temp::<usize>(zone_id)).and_then(|i| GRADE_ZONES.get(i)).map_or("shadows", |(z, _)| z);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 2, bottom: 6 }).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, (z, label)) in GRADE_ZONES.iter().enumerate() {
                if text_button(ui, &format!("maskGradeZone:{z}"), label, *z == zone).clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(zone_id, i));
                }
            }
        });
    });
    for s in grade_specs(zone) {
        let v = local_get(a, s.id);
        let out = slider(ui, &s, v, true, None);
        apply_slider_out(app, &s, out, |app, v| app.run("mask.adjust", json!({"values": {s.id: v}})));
    }
}

pub fn local_get(a: &LocalAdjustments, key: &str) -> f64 {
    serde_json::to_value(a).ok().and_then(|v| v.get(key).and_then(|x| x.as_f64())).unwrap_or(0.0)
}

/// Every kind of mask, in Create New Mask's order, with its tile label (OneSlips 1138a5edd): the tiles and the Add /
/// Subtract / Intersect menus offer all of them, in the same order, with the same icons.
pub(crate) const MASK_KINDS: [(&str, &str); 13] = [
    ("object", "Object"),
    ("prompt", "Describe"),
    ("subject", "Subject"),
    ("sky", "Sky"),
    ("background", "Background"),
    ("people", "People"),
    ("concept", "Remembered"),
    ("brush", "Brush"),
    ("linear", "Linear"),
    ("radial", "Radial"),
    ("luminanceRange", "Luminance"),
    ("colorRange", "Color"),
    // (Depth Anything V2 Small; without it the AI Models settings offer its download, issue #1144)
    ("depthRange", "Depth"),
];

/// The icon of a mask kind, by its `mask.add` kind id: the one table the Create New Mask tiles and
/// the mask list share.
fn kind_icon(kind: &str) -> Icon {
    match kind {
        "brush" => Icon::Brush,
        "linear" => Icon::Linear,
        "radial" => Icon::Radial,
        "colorRange" => Icon::Picker,
        "luminanceRange" => Icon::Luminance,
        "depthRange" => Icon::Depth,
        "subject" => Icon::Subject,
        "sky" => Icon::Sky,
        "background" => Icon::Background,
        "object" => Icon::Object,
        "prompt" => Icon::Describe,
        "people" => Icon::People,
        "landscape" => Icon::Landscape,
        "concept" => Icon::Tag,
        _ => Icon::Mask,
    }
}

fn kind_label(s: &MaskShape) -> (&'static str, Icon) {
    let (kind, label) = match s {
        MaskShape::Brush { .. } => ("brush", "Brush"),
        MaskShape::Linear { .. } => ("linear", "Linear Gradient"),
        MaskShape::Radial { .. } => ("radial", "Radial Gradient"),
        MaskShape::ColorRange { .. } => ("colorRange", "Color Range"),
        MaskShape::LuminanceRange { .. } => ("luminanceRange", "Luminance Range"),
        MaskShape::DepthRange { .. } => ("depthRange", "Depth Range"),
        MaskShape::Subject => ("subject", "Subject"),
        MaskShape::Sky => ("sky", "Sky"),
        MaskShape::Background => ("background", "Background"),
        MaskShape::Object { .. } => ("object", "Object"),
        MaskShape::Prompt { .. } => ("prompt", "Describe"),
        MaskShape::People { .. } => ("people", "People"),
        MaskShape::Landscape { .. } => ("landscape", "Landscape"),
        MaskShape::Concept { .. } => ("object", "Object"),
    };
    (label, kind_icon(kind))
}

/// A component's row in the mask tree, named after its source as the mask is: the name the
/// photographer gave it, else what it selects ("Groom - Clothes, Hair", "Wedding dress", "Water",
/// "“white car”"), the kind's words in the photographer's language.
pub(crate) fn component_label(c: &lcc_develop::MaskComponent) -> String {
    if let Some(n) = c.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    match &c.shape {
        // a person's role and parts, a taught object's or a prompt's own words: never translated
        MaskShape::People { .. } | MaskShape::Concept { .. } | MaskShape::Prompt { .. } => lcc_develop::recipe::shape_name(&c.shape),
        shape => crate::i18n::tr(&lcc_develop::recipe::shape_name(shape)).to_string(),
    }
}

/// The sign before a component's row in the mask tree: + Add, − Subtract, ∩ Intersect; none on the
/// first component, which starts the mask. Drawn as vector icons, so they never depend on a font
/// having the glyph (no bundled face has ∩).
pub(crate) fn component_op_sign(index: usize, op: lcc_develop::MaskOp) -> Option<Icon> {
    match op {
        _ if index == 0 => None,
        lcc_develop::MaskOp::Add => Some(Icon::Plus),
        lcc_develop::MaskOp::Subtract => Some(Icon::Minus),
        lcc_develop::MaskOp::Intersect => Some(Icon::Intersect),
    }
}

const TILE_MAX: f32 = 68.0;
const TILE_GAP: f32 = 6.0;
/// Room left and right of a tile's label.
const TILE_PAD: f32 = 3.0;

/// Columns, tile width and label size of the Create New Mask grid in `width`, `label(size)` being
/// the widest label at a font size: four tiles (up to 68 pt when room allows, at least 48 pt),
/// else three, else two — the first that fits the labels at 10.5 pt, or at 9.5 pt before a column is dropped.
/// A tile grows past 68 pt for a long (translated) label. In a panel too narrow even for that, two tiles whose
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
                let max_tile = TILE_MAX.max(need);
                let target = w.min(max_tile);
                return (n, target, size);
            }
        }
    }
    (2, room(2).max(24.0), 9.5)
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let popup_open_at_start = egui::Popup::is_any_open(ui.ctx());
    let t = Tokens::get(ui.ctx());
    let d = app.session.develop_of(id).unwrap_or_default();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 10 }).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Create New Mask")).font(t.semibold(13.0)).color(t.text_label));
        });
        ui.add_space(6.0);
        let tiles = MASK_KINDS;
        // four 52 pt tiles a row when they and their labels fit, else fewer (wider for a long label)
        let widest = |size: f32| {
            let font = t.font(size);
            tiles
                .iter()
                .map(|(_, l)| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text_dim).size().x)
                .fold(0.0, f32::max)
        };
        let (cols, tile, size) = tile_layout(ui.available_width(), widest);
        let compact = crate::theme::compact(ui.ctx());
        let grid_w = cols as f32 * tile + (cols as f32 - 1.0) * TILE_GAP;
        let pad = ((ui.available_width() + 4.0 - grid_w) / 2.0).clamp(0.0, (ui.available_width() - grid_w).max(0.0));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(pad);
            egui::Grid::new("mask-tiles").spacing(vec2(TILE_GAP, TILE_GAP)).show(ui, |ui| {
                for (i, (kind, label)) in tiles.iter().enumerate() {
                    // Compact: shorter tiles with a smaller icon
                    let (tile_h, icon, icon_up, label_up) = if compact { (36.0, 14.0, 5.0, 7.0) } else { (52.0, 20.0, 7.0, 9.0) };
                    let (r, resp) = ui.allocate_exact_size(vec2(tile, tile_h), Sense::click());
                    crate::widgets::announce(&resp, crate::i18n::tr(label));
                    register(ui.ctx(), format!("maskNew:{kind}"), r);
                    let active = app.ui.tool == *kind
                        || (matches!(*kind, "luminanceRange") && (app.ui.tool == "lumRange" || app.ui.tool == "luminanceRange"))
                        || (matches!(*kind, "concept") && app.ui.concept_pick)
                        || (matches!(*kind, "people") && app.ui.people_pick.is_some())
                        || app.ui.mask_new.as_ref().is_some_and(|(_, k)| k == *kind);
                    ui.painter().rect_filled(
                        r,
                        4.0,
                        if active {
                            t.tool_active
                        } else if resp.hovered() {
                            t.hover
                        } else {
                            t.inset
                        },
                    );
                    paint(
                        ui.painter(),
                        Rect::from_center_size(r.center() - vec2(0.0, icon_up), vec2(icon, icon)),
                        kind_icon(kind),
                        if active { t.text } else { t.text_label },
                    );
                    // (cut short only in a panel too narrow for the label: the full name on hover)
                    let label = crate::i18n::tr(label);
                    let g = one_line(ui.painter(), label, t.font(size), if active { t.text } else { t.text_dim }, tile - 2.0 * TILE_PAD);
                    let elided = g.elided;
                    let at = pos2(r.center().x, r.bottom() - label_up) - g.size() / 2.0;
                    register(ui.ctx(), format!("maskNewLabel:{kind}"), Rect::from_min_size(at, g.size()));
                    ui.painter().galley(at, g, if active { t.text } else { t.text_dim });
                    let resp = if elided { resp.on_hover_text(label) } else { resp };
                    if resp.clicked() {
                        if active {
                            disarm(app);
                            app.ui.people_pick = None;
                            app.ui.concept_pick = false;
                            app.mask_pick_op = None;
                        } else {
                            start_new(app, ui.ctx(), kind);
                        }
                    }
                    if i % cols == cols - 1 {
                        ui.end_row();
                    }
                }
            });
        });
        super::mask_pickers::people_picker(app, ui, id);
        super::mask_pickers::concept_picker(app, ui);
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
        // one line kept for the status whether or not there is one: the AI masks' background work must not move the
        // controls under it (one line, cut short when narrow, so it never grows either)
        ui.add_space(4.0);
        let line = ui.text_style_height(&egui::TextStyle::Body);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), line), egui::Sense::hover());
        if let Some(status) = status {
            let label = egui::Label::new(egui::RichText::new(crate::i18n::tr(status)).color(t.text_dim)).truncate();
            ui.put(rect, |ui: &mut egui::Ui| {
                ui.horizontal(|ui| {
                    crate::progress::spinner(ui, 14.0);
                    ui.add(label)
                })
                .response
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
        // the AI mask models: downloads, and which one computes Object masks
        if lcc_engine::segment::Segmenter::AVAILABLE && !download.running {
            ui.add_space(2.0);
            if text_button(ui, "maskAiModels", crate::i18n::tr("AI Models…"), false).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
            }
        }
        // the active mask's Amount, right under the tiles (as Camera Raw / Lightroom Classic place it)
        if let Some((index, m)) = d.masks.iter().enumerate().find(|(_, m)| app.session.active_mask == Some(m.id)) {
            ui.add_space(6.0);
            amount_slider(app, ui, index, m);
        }
    });
    divider(ui);
    let mut hovered_row = None;
    // mask list
    egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 8, bottom: 8 }).show(ui, |ui| {
        if d.masks.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No masks yet. Choose a mask type above.")).color(t.text_dim));
        }
        let count = d.masks.len();
        for (index, m) in d.masks.iter().enumerate() {
            let sel = app.session.active_mask == Some(m.id);
            if let Some((rid, name)) = app.ui.renaming_mask.as_mut().filter(|(rid, _)| *rid == m.id) {
                // inline rename: Enter (or leaving the field) commits, Escape cancels
                let rid = *rid;
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width()).id_salt(("maskRename", rid)));
                register(ui.ctx(), format!("maskRename:{rid}"), r.rect);
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.ui.renaming_mask = None;
                } else if r.lost_focus() {
                    let name = name.trim().to_string();
                    app.ui.renaming_mask = None;
                    // an empty name gives the mask back its automatic one (what it selects)
                    if name != m.name && (!name.is_empty() || !m.components.is_empty()) {
                        let _ = app.run("mask.rename", json!({"id": rid, "name": name}));
                    }
                }
                continue;
            }
            // only the selected mask's branch opens by itself; the chevron folds it, or opens another's
            let collapsed = !app.ui.mask_branch_open(m.id, sel);
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
            crate::widgets::announce_selected(&resp, m.name.clone(), Some(m.id) == app.session.active_mask);
            register(ui.ctx(), format!("mask:{}", m.id), r);
            // the mask's own row (name, eye, menu) is where its effect is judged, so the pointer on
            // it shows no overlay; a component row under it shows the whole mask's
            if sel || resp.hovered() {
                ui.painter().rect_filled(r, 4.0, if sel { t.tool_active } else { t.hover.gamma_multiply(0.7) });
            }

            // Collapse / expand chevron on the left
            let chev_r = Rect::from_center_size(pos2(r.left() + 10.0, r.center().y), vec2(14.0, 14.0));
            register(ui.ctx(), format!("maskCollapse:{}", m.id), chev_r);
            register(ui.ctx(), format!("button:maskCollapse:{}", m.id), chev_r);
            let chev_resp = ui.interact(chev_r, egui::Id::new(("maskCollapse", m.id)), Sense::click());
            paint(
                ui.painter(),
                chev_r.shrink(2.0),
                if collapsed { Icon::ChevronRight } else { Icon::ChevronDown },
                if chev_resp.hovered() { t.text } else { t.text_dim },
            );
            if chev_resp.clicked() {
                app.ui.toggle_mask_branch(m.id, sel);
            }

            // the mask's thumbnail (white on black over the photo); its kind's icon until it is ready
            let thumb_r = Rect::from_min_size(pos2(r.left() + 21.0, r.center().y - 16.0), vec2(32.0, 32.0));
            match crate::mask_thumbs::thumb(app, ui.ctx(), id, &d, m) {
                Some(tex) => {
                    crate::mask_thumbs::paint(ui.painter(), thumb_r, Some(&tex), if sel { t.text } else { t.text_dim.gamma_multiply(0.5) });
                    register(ui.ctx(), format!("maskThumb:{}", m.id), thumb_r);
                }
                None => {
                    let icon = m.components.first().map(|c| kind_label(&c.shape).1).unwrap_or(Icon::Mask);
                    paint(ui.painter(), Rect::from_center_size(thumb_r.center(), vec2(20.0, 20.0)), icon, t.text_label);
                }
            }
            // a stale AI mask (copied, synced, a preset's): an amber badge on its icon; click to update
            let mut badge_clicked = false;
            if let Some(why) = mask_ai_status(m).filter(|_| !app.session.segmenter.recomputing()) {
                let br = Rect::from_center_size(pos2(thumb_r.right() - 4.0, thumb_r.top() + 4.0), vec2(10.0, 10.0));
                register(ui.ctx(), format!("maskStale:{}", m.id), br);
                let badge = ui.interact(br.expand(2.0), egui::Id::new(("maskStale", m.id)), Sense::click());
                crate::widgets::announce(&badge, crate::i18n::tr(why));
                ui.painter().circle_filled(br.center(), 4.5, egui::Color32::from_rgb(240, 170, 40));
                ui.painter().text(br.center(), Align2::CENTER_CENTER, "!", t.semibold(8.5), egui::Color32::BLACK);
                let tip = if why == PICK_OBJECT {
                    crate::i18n::tr(why).to_string()
                } else {
                    format!("{}\n{}", crate::i18n::tr(why), crate::i18n::tr("Click to update it."))
                };
                let badge = badge.on_hover_text(tip);
                if badge.clicked() {
                    badge_clicked = true;
                    if why != PICK_OBJECT
                        && let Err(e) = app.run("mask.updateAi", json!({"id": m.id}))
                    {
                        app.ai_error(ui.ctx(), e, None);
                    }
                }
            }
            // a long name is cut short (with …) before the right buttons, the full name on hover
            let color = if m.visible { t.text } else { t.text_disabled };
            let right_room = if sel || ui.rect_contains_pointer(r) { 92.0 } else { 28.0 };
            // a mask from before automatic names ("Mask 1") shows what it selects
            let shown = if lcc_develop::recipe::generic_name(&m.name) && !m.components.is_empty() {
                lcc_develop::recipe::content_name(m)
            } else {
                m.name.clone()
            };
            let text_x = thumb_r.right() + 6.0;
            let g = one_line(ui.painter(), &shown, t.font(13.0), color, (r.right() - text_x - right_room).max(10.0));
            let elided = g.elided;
            let at = pos2(text_x, r.center().y - g.size().y / 2.0);
            register(ui.ctx(), format!("maskName:{}", m.id), Rect::from_min_size(at, g.size()));
            ui.painter().galley(at, g, color);
            let resp = if elided { resp.on_hover_text(&shown) } else { resp };

            let right = r.right();
            let cy = r.center().y;

            // the eye, always on the row: the mask's effect on / off, a quick before / after of
            // that mask; O is the overlay's
            let mut eye_clicked = false;
            {
                let er = Rect::from_center_size(pos2(right - 14.0, cy), vec2(22.0, 22.0));
                register(ui.ctx(), format!("maskVisible:{}", m.id), er);
                let eye = ui.interact(er, egui::Id::new(("maskVisible", m.id)), Sense::click());
                crate::widgets::announce_selected(&eye, crate::i18n::tr("Show mask"), m.visible);
                paint(
                    ui.painter(),
                    er.shrink(3.0),
                    if m.visible { Icon::Eye } else { Icon::EyeOff },
                    if eye.hovered() { t.text } else { t.text_dim },
                );
                let eye = eye.on_hover_text(crate::i18n::tr("The mask's effect on / off"));
                if eye.clicked() {
                    eye_clicked = true;
                    let _ = app.run("mask.visible", json!({"id": m.id}));
                }
            }

            // delete only on hover over the parent mask (issue #642, #694)
            let mut bin_clicked = false;
            if ui.rect_contains_pointer(r) {
                let br = Rect::from_center_size(pos2(right - 36.0, cy), vec2(22.0, 22.0));
                register(ui.ctx(), format!("maskDeleteRow:{}", m.id), br);
                register(ui.ctx(), "button:maskDelete", br);
                register(ui.ctx(), format!("button:maskDeleteRow:{}", m.id), br);
                let bin = ui.interact(br, egui::Id::new(("maskDeleteRow", m.id)), Sense::click());
                crate::widgets::announce(&bin, crate::i18n::tr("Delete Mask"));
                paint(ui.painter(), br.shrink(3.0), Icon::Trash, if bin.hovered() { t.text } else { t.text_dim });
                let bin = bin.on_hover_text(crate::i18n::tr("Delete Mask"));
                if bin.clicked() {
                    bin_clicked = true;
                    let _ = app.run("mask.delete", json!({"id": m.id}));
                }
            }

            // minus button inline with parent mask (issue #694)
            let mut minus_clicked = false;
            if sel || ui.rect_contains_pointer(r) {
                let mr = Rect::from_center_size(pos2(right - 58.0, cy), vec2(20.0, 20.0));
                register(ui.ctx(), format!("maskMinus:{}", m.id), mr);
                register(ui.ctx(), format!("button:maskMinus:{}", m.id), mr);
                register(ui.ctx(), format!("button:maskMinus{}", m.id), mr);
                let minus = ui.interact(mr, egui::Id::new(("maskMinus", m.id)), Sense::click());
                crate::widgets::announce(&minus, crate::i18n::tr("Subtract from this mask"));
                paint(ui.painter(), mr.shrink(3.0), Icon::Minus, if minus.hovered() { t.text } else { t.text_dim });
                let minus = minus.on_hover_text(crate::i18n::tr("Subtract from this mask"));
                if minus.clicked() {
                    minus_clicked = true;
                    if !sel {
                        let _ = app.run("mask.select", json!({"id": m.id}));
                    }
                    app.ui.collapsed_masks.remove(&m.id);
                }
                egui::Popup::menu(&minus).show(|ui| component_menu(app, ui, "subtract"));
            }

            // plus button inline with parent mask (issue #694)
            let mut plus_clicked = false;
            if sel || ui.rect_contains_pointer(r) {
                let pr = Rect::from_center_size(pos2(right - 78.0, cy), vec2(20.0, 20.0));
                register(ui.ctx(), format!("maskPlus:{}", m.id), pr);
                register(ui.ctx(), format!("button:maskPlus:{}", m.id), pr);
                register(ui.ctx(), format!("button:maskPlus{}", m.id), pr);
                let plus = ui.interact(pr, egui::Id::new(("maskPlus", m.id)), Sense::click());
                crate::widgets::announce(&plus, crate::i18n::tr("Add to this mask"));
                paint(ui.painter(), pr.shrink(3.0), Icon::Plus, if plus.hovered() { t.text } else { t.text_dim });
                let plus = plus.on_hover_text(crate::i18n::tr("Add to this mask"));
                if plus.clicked() {
                    plus_clicked = true;
                    if !sel {
                        let _ = app.run("mask.select", json!({"id": m.id}));
                    }
                    app.ui.collapsed_masks.remove(&m.id);
                }
                egui::Popup::menu(&plus).show(|ui| component_menu(app, ui, "add"));
            }

            if resp.clicked()
                && !eye_clicked
                && !bin_clicked
                && !minus_clicked
                && !plus_clicked
                && !badge_clicked
                && !chev_resp.clicked()
                && !popup_open_at_start
            {
                // the mask row: any new-mask tool is put away and the mask's top component is the
                // one edited: a Brush arms the brush on it (paint adds strokes, ⌥ erases), a
                // gradient shows its handles
                disarm(app);
                let top = if m.components.is_empty() { None } else { app.run("mask.editComponent", json!({"id": m.id, "component": 0})).ok() };
                if top.is_none() {
                    let _ = app.run("mask.select", json!({"id": m.id}));
                    if app.ui.active_mask_component.is_none() {
                        app.ui.active_mask_component = Some(0);
                    }
                }
            }
            if resp.double_clicked() {
                app.ui.renaming_mask = Some((m.id, m.name.clone()));
            }
            crate::widgets::context_menu(&resp, |ui| mask_menu(app, ui, m.id, &m.name, m.visible, index, count));

            if sel {
                describe_field(app, ui, false);
            }

            // Sub masks (components) directly under the parent mask (issue #694)
            if !collapsed {
                let cur_d = app.session.develop_of(id).unwrap_or_default();
                if let Some(cur_m) = cur_d.masks.iter().find(|x| x.id == m.id) {
                    if sel {
                        if app.ui.active_mask_component.is_none() && !cur_m.components.is_empty() {
                            app.ui.active_mask_component = Some(0);
                        } else if let Some(ci) = app.ui.active_mask_component
                            && ci >= cur_m.components.len()
                        {
                            app.ui.active_mask_component = cur_m.components.len().checked_sub(1);
                        }
                    }
                    for (i, c) in cur_m.components.iter().enumerate() {
                        let (_, icon) = kind_label(&c.shape);
                        let label = component_label(c);
                        let comp_sel = sel && app.ui.active_mask_component == Some(i);
                        if let Some((_, _, name)) = app.ui.renaming_component.as_mut().filter(|(mid, k, _)| *mid == m.id && *k == i) {
                            let r =
                                ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width() - 30.0).id_salt(("compRename", m.id, i)));
                            register(ui.ctx(), format!("componentRename:{i}"), r.rect);
                            if !r.has_focus() && !r.lost_focus() {
                                r.request_focus();
                            }
                            if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                                app.ui.renaming_component = None;
                            } else if r.lost_focus() {
                                let name = name.trim().to_string();
                                app.ui.renaming_component = None;
                                if Some(&name) != c.name.as_ref() {
                                    let _ = app.run("mask.component", json!({"id": m.id, "component": i, "action": "rename", "name": name}));
                                }
                            }
                            continue;
                        }
                        ui.add_space(2.0);
                        let full_w = ui.available_width();
                        let indent = 20.0;
                        let (full_r, _) = ui.allocate_exact_size(vec2(full_w, 36.0), Sense::hover());
                        let r = Rect::from_min_size(pos2(full_r.min.x + indent, full_r.min.y), vec2(full_w - indent, 36.0));
                        let resp = ui.interact(r, egui::Id::new(("compRow", m.id, i)), Sense::click());
                        crate::widgets::announce_selected(&resp, label.clone(), comp_sel);
                        register(ui.ctx(), format!("component:{i}"), r);
                        if ui.rect_contains_pointer(r) {
                            hovered_row = Some(m.id);
                        }
                        if comp_sel || resp.hovered() {
                            ui.painter().rect_filled(r, 4.0, if comp_sel { t.tool_active } else { t.hover.gamma_multiply(0.7) });
                        }

                        // a branch of the tree: a line from the mask down to this row, then across to it
                        let x0 = full_r.left() + 10.0;
                        let branch = Stroke::new(1.0, t.button_border);
                        ui.painter().line_segment([pos2(x0, full_r.top() - 2.0), pos2(x0, r.center().y)], branch);
                        ui.painter().line_segment([pos2(x0, r.center().y), pos2(r.left(), r.center().y)], branch);
                        // how the component combines, in a column of its own (the first one starts the mask)
                        let opr = Rect::from_center_size(pos2(r.left() + 11.0, r.center().y), vec2(14.0, 14.0));
                        if let Some(op) = component_op_sign(i, c.op) {
                            register(ui.ctx(), format!("componentOp:{i}"), opr);
                            paint(ui.painter(), opr.shrink(1.0), op, t.text_label);
                        }
                        let icon_r = Rect::from_min_size(pos2(opr.right() + 4.0, r.center().y - 14.0), vec2(28.0, 28.0));
                        register(ui.ctx(), format!("componentIcon:{i}"), icon_r);
                        // the component's own thumbnail (made into a mask of its own)
                        let alone = lcc_develop::Mask {
                            components: vec![lcc_develop::MaskComponent { op: lcc_develop::MaskOp::Add, ..c.clone() }],
                            on_photo: cur_m.on_photo,
                            ..Default::default()
                        };
                        match crate::mask_thumbs::thumb(app, ui.ctx(), id, &cur_d, &alone) {
                            Some(tex) => {
                                crate::mask_thumbs::paint(
                                    ui.painter(),
                                    icon_r,
                                    Some(&tex),
                                    if comp_sel { t.text } else { t.text_dim.gamma_multiply(0.5) },
                                );
                                register(ui.ctx(), format!("componentThumb:{i}"), icon_r);
                            }
                            None => paint(
                                ui.painter(),
                                Rect::from_center_size(icon_r.center(), vec2(18.0, 18.0)),
                                icon,
                                if comp_sel { t.text } else { t.text_label },
                            ),
                        }

                        let text = format!("{label}{}", if c.invert { crate::i18n::tr(" (inverted)") } else { "" });
                        let text_x = icon_r.right() + 6.0;
                        let g = one_line(
                            ui.painter(),
                            &text,
                            t.font(12.5),
                            if comp_sel { t.text } else { t.text_dim },
                            (r.right() - 74.0 - text_x).max(10.0),
                        );
                        let at = pos2(text_x, r.center().y - g.size().y / 2.0);
                        ui.painter().galley(at, g, if comp_sel { t.text } else { t.text_dim });

                        // Invert this component alone, lit while it is inverted
                        let inv_r = Rect::from_center_size(pos2(r.right() - 58.0, r.center().y), vec2(20.0, 20.0));
                        register(ui.ctx(), format!("componentInvert:{i}"), inv_r);
                        register(ui.ctx(), format!("button:componentInvert:{i}"), inv_r);
                        let comp_inv = ui.interact(inv_r, egui::Id::new(("componentInvert", m.id, i)), Sense::click());
                        crate::widgets::announce_selected(&comp_inv, crate::i18n::tr("Invert"), c.invert);
                        if c.invert {
                            ui.painter().rect_filled(inv_r, 3.0, t.pressed);
                        }
                        paint(ui.painter(), inv_r.shrink(3.0), Icon::MaskInvert, if comp_inv.hovered() || c.invert { t.text } else { t.text_dim });
                        let comp_inv = comp_inv.on_hover_text(crate::i18n::tr("Invert"));
                        if comp_inv.clicked() {
                            let _ = app.run("mask.component", json!({"id": m.id, "component": i, "action": "invert"}));
                        }

                        // Bin icon next to the three dots
                        let bin_r = Rect::from_center_size(pos2(r.right() - 36.0, r.center().y), vec2(20.0, 20.0));
                        register(ui.ctx(), format!("componentDelete:{i}"), bin_r);
                        register(ui.ctx(), format!("button:componentDelete:{i}"), bin_r);
                        register(ui.ctx(), format!("button:componentDelete{i}"), bin_r);
                        let comp_bin = ui.interact(bin_r, egui::Id::new(("componentDelete", m.id, i)), Sense::click());
                        crate::widgets::announce(&comp_bin, crate::i18n::tr("Delete component"));
                        paint(ui.painter(), bin_r.shrink(2.5), Icon::Trash, if comp_bin.hovered() { t.text } else { t.text_dim });
                        let comp_bin = comp_bin.on_hover_text(crate::i18n::tr("Delete component"));
                        if comp_bin.clicked() {
                            let _ = app.run("mask.component", json!({"id": m.id, "component": i, "action": "delete"}));
                            if app.ui.active_mask_component == Some(i) {
                                app.ui.active_mask_component = i.checked_sub(1);
                            }
                        }

                        // Options (three dots) button
                        let more_r = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(20.0, 20.0));
                        register(ui.ctx(), format!("button:componentMenu{i}"), more_r);
                        register(ui.ctx(), format!("button:componentMenu:{i}"), more_r);
                        let more = ui.interact(more_r, egui::Id::new(("componentMenu", m.id, i)), Sense::click());
                        let more = more.on_hover_text(crate::i18n::tr("Component options"));
                        let g_dots = ui.painter().layout_no_wrap("…".to_string(), t.font(13.0), if more.hovered() { t.text } else { t.text_dim });
                        let at_dots = pos2(more_r.center().x - g_dots.size().x / 2.0, more_r.center().y - g_dots.size().y / 2.0);
                        ui.painter().galley(at_dots, g_dots, if more.hovered() { t.text } else { t.text_dim });
                        egui::Popup::menu(&more).show(|ui| component_row_menu(app, ui, m.id, i, &label, cur_m.components.len()));

                        if resp.clicked()
                            && !comp_bin.clicked()
                            && !comp_inv.clicked()
                            && !more.clicked()
                            && !popup_open_at_start
                            && let Err(e) = app.run("mask.editComponent", json!({"id": m.id, "component": i}))
                        {
                            app.toast(ui.ctx(), e);
                        }
                        if resp.double_clicked() {
                            app.ui.renaming_component = Some((m.id, i, label.clone()));
                        }
                        crate::widgets::context_menu(&resp, |ui| component_row_menu(app, ui, m.id, i, &label, cur_m.components.len()));

                        // each component's sliders in a scope of its own: two radial components each have a
                        // Feather slider, and without it both took the same egui id
                        ui.push_id(("componentControls", m.id, i), |ui| range_controls(app, ui, i, &c.shape));
                        if comp_sel
                            && (app.ui.show_people_who || app.ui.role_draft.is_some())
                            && let MaskShape::People { .. } = &c.shape
                        {
                            who_picker(app, ui, id, i, &c.shape);
                        }
                    }

                    // Core combination action bar and refinement sliders for the active mask
                    if sel {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let total_w = 4.0 * 32.0 + 3.0 * 4.0;
                            let pad = ((ui.available_width() - total_w) / 2.0).max(0.0);
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.add_space(pad);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let add = mask_icon(ui, "maskAddComp", Icon::MaskAdd, "Add", false);
                            egui::Popup::menu(&add).show(|ui| component_menu(app, ui, "add"));
                            let sub = mask_icon(ui, "maskSubComp", Icon::MaskSubtract, "Subtract", false);
                            egui::Popup::menu(&sub).show(|ui| component_menu(app, ui, "subtract"));
                            let int = mask_icon(ui, "maskIntComp", Icon::MaskIntersect, "Intersect", false);
                            egui::Popup::menu(&int).show(|ui| component_menu(app, ui, "intersect"));
                            if mask_icon(ui, "maskInvert", Icon::MaskInvert, "Invert", cur_m.invert).clicked() {
                                let _ = app.run("mask.invert", json!({}));
                            }
                        });
                        ui.add_space(6.0);
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
                            shade: lcc_develop::Shade::Plain,
                        };
                        let out = slider(ui, &refine, cur_m.refine, true, None);
                        apply_slider_out(app, &refine, out, |app, v| app.run("mask.refine", json!({"value": v})));
                        let feather = ControlSpec { id: "maskFeather", label: "Feather", ..refine };
                        let out = slider(ui, &feather, cur_m.feather, true, None);
                        apply_slider_out(app, &feather, out, |app, v| app.run("mask.feather", json!({"value": v})));
                    }
                }
            }
            ui.add_space(4.0);
        }
    });
    app.ui.mask_hover = hovered_row;
    let Some(mid) = app.session.active_mask else {
        if !d.masks.is_empty() {
            divider(ui);
            egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 4 }).show(ui, |ui| {
                show_overlay_row(app, ui);
            });
        }
        // the brush or object tool armed for a new mask: its settings before the first stroke
        if app.ui.tool == "brush" {
            divider(ui);
            brush_settings(app, ui);
        }
        if app.ui.tool == "object" {
            divider(ui);
            object_settings(app, ui);
        }
        next_mask_settings(app, ui, !d.masks.is_empty());
        return;
    };
    let Some(m) = d.masks.iter().find(|m| m.id == mid).cloned() else { return };
    divider(ui);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 4 }).show(ui, |ui| {
        show_overlay_row(app, ui);
    });
    if app.ui.tool == "brush" {
        divider(ui);
        brush_settings(app, ui);
    }
    if app.ui.tool == "object" {
        divider(ui);
        object_settings(app, ui);
    }
    divider(ui);
    super::mask_effects::effect_row(app, ui, &m, true);
    for (key, title, ids) in GROUPS {
        if !section_header(app, ui, key, title, group_changed(&m.adjust, ids)) {
            continue;
        }
        for s in LOCAL.iter().filter(|s| ids.contains(&s.id)).map(|s| local_spec(s, app.ui.settings.expanded_sliders)).collect::<Vec<_>>() {
            let s = &s;
            let v = local_get(&m.adjust, s.id);
            let out = slider(ui, s, v, true, None);
            apply_slider_out(app, s, out, |app, v| app.run("mask.adjust", json!({"values": {s.id: v}})));
        }
        if *key == "light" {
            // the mask's own point curves, after the tone sliders (the Tone Curve's editor)
            let open = app.ui.flyout_open("maskCurve");
            if flyout_row(ui, "maskCurve", "Curve", Icon::Curve, open).clicked() {
                app.ui.toggle_flyout("maskCurve");
            }
            if open {
                crate::widgets::slider_group(ui, "maskCurve", |ui| {
                    super::edit::curve_editor(app, ui, id, &d, super::edit::CurveTarget::Mask(m.id));
                });
            }
        }
        if *key == "color" {
            // the tint swatch under Saturation; its row opens the hue and strength sliders
            let open = app.ui.flyout_open("maskTint");
            let row = flyout_row(ui, "maskTint", "Color", Icon::Target, open);
            let sw = Rect::from_center_size(pos2(row.rect.right() - 36.0, row.rect.center().y), vec2(16.0, 16.0));
            register(ui.ctx(), "maskTint:swatch", sw);
            let t = Tokens::get(ui.ctx());
            match tint_swatch(m.adjust.color_hue, m.adjust.color_sat) {
                Some(c) => {
                    ui.painter().rect_filled(sw, 2.0, c);
                }
                // no tint: an empty box crossed through, as Lightroom shows it
                None => {
                    ui.painter().line_segment([sw.left_bottom(), sw.right_top()], Stroke::new(1.0, t.text_dim));
                }
            }
            ui.painter().rect_stroke(sw, 2.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
            if row.clicked() {
                app.ui.toggle_flyout("maskTint");
            }
            if open {
                crate::widgets::slider_group(ui, "maskTint", |ui| {
                    for s in &TINT {
                        let v = local_get(&m.adjust, s.id);
                        let out = slider(ui, s, v, true, None);
                        apply_slider_out(app, s, out, |app, v| app.run("mask.adjust", json!({"values": {s.id: v}})));
                    }
                });
            }
            divider(ui);
            grading_section(app, ui, &m.adjust);
            // the mask's own Color Mixer and Point Color (a colour photo's tools, as globally)
            if !crate::is_bw(&d) {
                let open = app.ui.flyout_open("maskMixer");
                if flyout_row(ui, "maskMixer", "Color Mixer", Icon::Target, open).clicked() {
                    app.ui.toggle_flyout("maskMixer");
                }
                if open {
                    mask_mixer(app, ui, &m);
                }
            }
            if !crate::is_bw(&d) {
                let open = app.ui.flyout_open("maskPointColor");
                if flyout_row(ui, "maskPointColor", "Point Color", Icon::Picker, open).clicked() {
                    app.ui.toggle_flyout("maskPointColor");
                }
                if open {
                    mask_point_color(app, ui, &m);
                }
            }
        }
    }
    if app.ui.roles_window {
        roles_window(app, ui.ctx());
    }
    ui.add_space(30.0);
    let _ = Stroke::NONE;
}

/// With no mask selected: the effect menu and sliders for the next new mask (set the look first,
/// then draw; `mask.adjust` / `mask.applyEffect` with no mask set them), and Reset Settings on New
/// Mask. With no masks on the photo at all (`enabled = false`, issue #1190: the sliders look like
/// the image's own and changing them does nothing to it) they are dimmed and inert; the
/// "No masks yet" hint above says to create one first.
fn next_mask_settings(app: &mut LccApp, ui: &mut egui::Ui, enabled: bool) {
    let t = Tokens::get(ui.ctx());
    divider(ui);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 0 }).show(ui, |ui| {
        let r = ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr("Settings for the next mask")).color(t.text_dim)).wrap());
        register(ui.ctx(), "maskNextSettings", r.rect);
        let mut on = app.session.mask_reset_new;
        let r = ui.checkbox(&mut on, crate::i18n::tr("Reset settings on new mask"));
        register(ui.ctx(), "maskResetNew", r.rect);
        if r.changed() {
            let _ = app.run("mask.next", json!({"resetNew": on}));
        }
    });
    // (id 0: no mask; the effect menu then sets the next mask's settings)
    let shown = lcc_develop::Mask { id: 0, adjust: app.session.mask_next, ..Default::default() };
    super::mask_effects::effect_row(app, ui, &shown, enabled);
    for (key, title, ids) in GROUPS {
        if !section_header(app, ui, key, title, group_changed(&shown.adjust, ids)) {
            continue;
        }
        for s in LOCAL.iter().filter(|s| ids.contains(&s.id)).map(|s| local_spec(s, app.ui.settings.expanded_sliders)).collect::<Vec<_>>() {
            let s = &s;
            let v = local_get(&shown.adjust, s.id);
            let out = slider(ui, s, v, enabled, None);
            apply_slider_out(app, s, out, |app, v| app.run("mask.adjust", json!({"values": {s.id: v}})));
        }
    }
}

/// The mask Color Mixer's slider ids, per group (hue, sat, lum) and band (`MIXER_BANDS`' order).
const MASK_MIXER_IDS: [[&str; 8]; 3] = [
    [
        "maskMixer.red.hue",
        "maskMixer.orange.hue",
        "maskMixer.yellow.hue",
        "maskMixer.green.hue",
        "maskMixer.aqua.hue",
        "maskMixer.blue.hue",
        "maskMixer.purple.hue",
        "maskMixer.magenta.hue",
    ],
    [
        "maskMixer.red.sat",
        "maskMixer.orange.sat",
        "maskMixer.yellow.sat",
        "maskMixer.green.sat",
        "maskMixer.aqua.sat",
        "maskMixer.blue.sat",
        "maskMixer.purple.sat",
        "maskMixer.magenta.sat",
    ],
    [
        "maskMixer.red.lum",
        "maskMixer.orange.lum",
        "maskMixer.yellow.lum",
        "maskMixer.green.lum",
        "maskMixer.aqua.lum",
        "maskMixer.blue.lum",
        "maskMixer.purple.lum",
        "maskMixer.magenta.lum",
    ],
];

/// The active mask's Color Mixer: Hue | Saturation | Luminance tabs, the group's targeted adjustment
/// (tool `maskMixer:<group>`: drag up / down on the photo), a slider per colour (the photo mixer's
/// specs) and Reset while anything is set.
fn mask_mixer(app: &mut LccApp, ui: &mut egui::Ui, m: &lcc_develop::Mask) {
    let groups = &super::edit::MIXER_GROUPS[..3];
    if !groups.iter().any(|(_, k)| *k == app.ui.mask_mixer_group) {
        app.ui.mask_mixer_group = "hue".into();
    }
    let key = app.ui.mask_mixer_group.clone();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 6, bottom: 2 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let tool = format!("maskMixer:{key}");
            let active = app.ui.tool == tool;
            if icon_button(
                ui,
                "maskMixerTargeted",
                Icon::Target,
                vec2(22.0, 22.0),
                active,
                true,
                crate::i18n::tr("Targeted adjustment: drag up/down on the photo to adjust the colours there"),
            )
            .clicked()
            {
                app.ui.tool = if active { String::new() } else { tool };
            }
            for (label, k) in groups {
                if super::edit::group_tab(ui, &format!("maskMixerGroup-{k}"), crate::i18n::tr(label), key == *k).clicked() {
                    app.ui.mask_mixer_group = k.to_string();
                    if app.ui.tool.starts_with("maskMixer:") {
                        app.ui.tool.clear();
                    }
                }
            }
        });
    });
    let gi = groups.iter().position(|(_, k)| *k == key).unwrap_or(0);
    let Some(ids) = MASK_MIXER_IDS.get(gi) else { return };
    for ((band, name), wid) in lcc_develop::MIXER_BANDS.iter().zip(super::edit::BAND_NAMES).zip(ids) {
        let Some(base) = lcc_develop::controls::find(&format!("mixer.{band}.{key}")) else { continue };
        let spec = ControlSpec { id: wid, ..*base };
        let hsl = m.mixer.bands().get(lcc_develop::MIXER_BANDS.iter().position(|b| b == band).unwrap_or(0)).copied().unwrap_or_default();
        let v = match key.as_str() {
            "sat" => hsl.sat,
            "lum" => hsl.lum,
            _ => hsl.hue,
        };
        let out = slider(ui, &spec, v, true, Some(name));
        let field = format!("{band}.{key}");
        apply_slider_out(app, &spec, out, |app, v| app.run("mask.mixer", json!({"id": m.id, "values": {field.clone(): v}})));
    }
    if !m.mixer.is_neutral() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 8 }).show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "maskMixerReset", crate::i18n::tr("Reset"), false).clicked() {
                    let _ = app.run("mask.mixer", json!({"id": m.id, "reset": true}));
                }
            });
        });
    }
}

/// The local Point Color sliders: (widget id, the global control's field, the `mask.pointColorSet` key).
const MASK_POINT_FIELDS: [(&str, &str, &str); 8] = [
    ("maskPointColor.hueShift", "hueShift", "hue_shift"),
    ("maskPointColor.satShift", "satShift", "sat_shift"),
    ("maskPointColor.lumShift", "lumShift", "lum_shift"),
    ("maskPointColor.variance", "variance", "variance"),
    ("maskPointColor.range", "range", "range"),
    ("maskPointColor.hueRange", "hueRange", "hue_range"),
    ("maskPointColor.satRange", "satRange", "sat_range"),
    ("maskPointColor.lumRange", "lumRange", "lum_range"),
];

/// The active mask's Point Color: its samples' swatches (+ the eyedropper, tool `maskPointColor`), the selected
/// sample's shifts and range, and Delete; the same sliders as the photo's Point Color.
fn mask_point_color(app: &mut LccApp, ui: &mut egui::Ui, m: &lcc_develop::Mask) {
    let t = Tokens::get(ui.ctx());
    let n = m.point_colors.len();
    if app.ui.mask_point_color >= n && n > 0 {
        app.ui.mask_point_color = n - 1;
    }
    let sel = app.ui.mask_point_color;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let active = app.ui.tool == "maskPointColor";
            let full = n >= lcc_develop::MAX_POINT_COLORS;
            if icon_button(ui, "maskPointColorPicker", Icon::Picker, vec2(26.0, 26.0), active, !full, crate::i18n::tr("Sample a colour on the photo"))
                .clicked()
            {
                app.ui.tool = if active { String::new() } else { "maskPointColor".into() };
            }
            for (i, p) in m.point_colors.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Point Color"), i + 1));
                register(ui.ctx(), format!("maskPointColor:{i}"), r);
                ui.painter().rect_filled(r.shrink(2.0), 3.0, super::edit::oklch_color(p.lum, p.chroma, p.hue));
                if i == sel {
                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.text), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    app.ui.mask_point_color = i;
                }
            }
        });
    });
    let Some(pc) = m.point_colors.get(sel).copied() else {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 2, bottom: 8 }).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a colour on the photo with the eyedropper.")).size(12.0).color(t.text_dim));
        });
        return;
    };
    for (wid, field, key) in MASK_POINT_FIELDS {
        let Some(base) = lcc_develop::controls::find(&format!("pointColor.0.{field}")) else { continue };
        let spec = ControlSpec { id: wid, ..*base };
        let v = match key {
            "hue_shift" => pc.hue_shift,
            "sat_shift" => pc.sat_shift,
            "lum_shift" => pc.lum_shift,
            "variance" => pc.variance,
            "range" => pc.range,
            "hue_range" => pc.hue_range,
            "sat_range" => pc.sat_range,
            _ => pc.lum_range,
        };
        let out = slider(ui, &spec, v, true, None);
        apply_slider_out(app, &spec, out, |app, v| app.run("mask.pointColorSet", json!({"id": m.id, "index": sel, "values": {key: v}})));
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if text_button(ui, "maskPointColorDelete", crate::i18n::tr("Delete"), false).clicked() {
                let _ = app.run("mask.pointColorDelete", json!({"id": m.id, "index": sel}));
            }
        });
    });
}

/// The active mask's Amount slider, under a line naming the mask (its name, else "Mask N").
fn amount_slider(app: &mut LccApp, ui: &mut egui::Ui, index: usize, m: &lcc_develop::Mask) {
    let t = Tokens::get(ui.ctx());
    let name = if m.name.trim().is_empty() { format!("{} {}", crate::i18n::tr("Mask"), index + 1) } else { m.name.clone() };
    ui.vertical_centered(|ui| {
        let r = ui.add(egui::Label::new(egui::RichText::new(name).font(t.semibold(13.0)).color(t.text_label)).truncate());
        register(ui.ctx(), "maskAmountName", r.rect);
    });
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
        shade: lcc_develop::Shade::Plain,
    };
    let out = slider(ui, &amt, m.adjust.amount, true, None);
    apply_slider_out(app, &amt, out, |app, v| app.run("mask.adjust", json!({"values": {"amount": v}})));
}

/// Translate fixed action labels, but never mutate or translate user-entered mask names.
fn menu_label(source: &str) -> &str {
    crate::i18n::tr(source)
}

/// The photo's right-click menu in Masking: a pin right-clicked opens its mask's menu (→ `true`:
/// nothing else is shown); otherwise the selected AI mask can be updated from here too.
pub(crate) fn photo_mask_menu(app: &mut LccApp, ui: &mut egui::Ui) -> bool {
    let Some(d) = app.session.active().and_then(|p| app.session.develop_of(p)) else { return false };
    if let Some((index, m)) = app.ui.mask_menu_for.and_then(|id| d.masks.iter().enumerate().find(|(_, m)| m.id == id)) {
        mask_menu(app, ui, m.id, &m.name, m.visible, index, d.masks.len());
        return true;
    }
    if let Some(m) = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask).filter(|m| m.uses_ai()) {
        let b = ui.button(menu_label("Update AI Mask"));
        register(ui.ctx(), "photoMenu:updateAiMask", b.rect);
        if b.clicked() {
            if let Err(e) = app.run("mask.updateAi", json!({"id": m.id})) {
                app.ai_error(ui.ctx(), e, None);
            }
            ui.close();
        }
        ui.separator();
    }
    false
}

/// What an AI mask's row badge and panel say when its AI selection isn't there for this photo: an
/// Object without clicks here (copied or synced from another photo) is picked again, any other AI
/// part not computed for this photo needs updating. `None`: up to date.
pub(crate) fn mask_ai_status(m: &lcc_develop::Mask) -> Option<&'static str> {
    if !m.uses_ai() || !m.needs_compute() {
        return None;
    }
    let unpicked =
        m.components.iter().any(|c| matches!(&c.shape, MaskShape::Object { hint, seg: None, detail, .. } if hint.is_empty() && detail.is_empty()));
    Some(if unpicked { PICK_OBJECT } else { NEEDS_UPDATE })
}

/// See [`mask_ai_status`].
pub(crate) const PICK_OBJECT: &str = "Pick the object on this photo: click it with the Object tool.";
/// See [`mask_ai_status`].
pub(crate) const NEEDS_UPDATE: &str = "This mask's AI selection wasn't computed for this photo yet.";

/// The right-click menu of a mask in the Masks list.
fn mask_menu(app: &mut LccApp, ui: &mut egui::Ui, id: u32, name: &str, visible: bool, index: usize, count: usize) {
    let mut run = |ui: &mut egui::Ui, label: &str, enabled: bool, cmd: &str, p: serde_json::Value| {
        if ui.add_enabled(enabled, egui::Button::new(menu_label(label))).clicked() {
            let _ = app.run(cmd, p);
            ui.close();
        }
    };
    run(ui, "Duplicate Mask", true, "mask.duplicate", json!({"id": id}));
    run(ui, "Duplicate and Invert Mask", true, "mask.duplicate", json!({"id": id, "invert": true}));
    run(ui, "Invert Mask", true, "mask.invert", json!({"id": id}));
    run(ui, if visible { "Hide Mask" } else { "Show Mask" }, true, "mask.visible", json!({"id": id}));
    ui.separator();
    // compute the mask's AI selections again from this photo (one mask, or every AI mask on it)
    let photo = app.session.active();
    let d = photo.and_then(|p| app.session.develop_of(p));
    let ai = d.as_ref().and_then(|d| d.masks.iter().find(|m| m.id == id)).is_some_and(|m| m.uses_ai());
    let any_ai = d.as_ref().is_some_and(|d| d.masks.iter().any(|m| m.uses_ai()));
    let b = ui.add_enabled(ai, egui::Button::new(menu_label("Update AI Mask")));
    register(ui.ctx(), format!("maskMenu:update:{id}"), b.rect);
    if b.clicked() {
        if let Err(e) = app.run("mask.updateAi", json!({"id": id})) {
            app.ai_error(ui.ctx(), e, None);
        }
        ui.close();
    }
    if ui.add_enabled(any_ai, egui::Button::new(menu_label("Update All AI Masks"))).clicked() {
        if let Some(p) = photo {
            let _ = app.run("masks.updateAi", json!({"ids": [p.0]}));
        }
        ui.close();
    }
    ui.separator();
    super::mask_pickers::remember_item(app, ui, id);
    ui.separator();
    let mut run = |ui: &mut egui::Ui, label: &str, enabled: bool, cmd: &str, p: serde_json::Value| {
        if ui.add_enabled(enabled, egui::Button::new(menu_label(label))).clicked() {
            let _ = app.run(cmd, p);
            ui.close();
        }
    };
    run(ui, "Move Up", index > 0, "mask.move", json!({"id": id, "delta": -1}));
    run(ui, "Move Down", index + 1 < count, "mask.move", json!({"id": id, "delta": 1}));
    ui.separator();
    if ui.button(crate::i18n::tr("Rename…")).clicked() {
        app.ui.renaming_mask = Some((id, name.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("Delete Mask")).clicked() {
        let _ = app.run("mask.delete", json!({"id": id}));
        ui.close();
    }
}

/// Overlay colours offered as swatches (the colour of the selected one is used for the tint).
pub const OVERLAY_COLORS: [[u8; 3]; 5] = [[230, 30, 40], [40, 200, 70], [40, 110, 240], [250, 210, 30], [255, 255, 255]];

/// Width of the Color Overlay Settings popup: the swatch row plus a short Opacity slider, so the
/// popup fits beside the right panel instead of reaching over the photo (#1146).
pub(crate) const COLOR_OVERLAY_POPUP_WIDTH: f32 = 220.0;

/// Color Overlay Settings popup: custom color selection swatches and opacity control.
pub(crate) fn color_overlay_popup(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // the slider takes all the width it is offered, and a popup offers the whole window
    ui.set_width(COLOR_OVERLAY_POPUP_WIDTH);
    ui.label(egui::RichText::new(crate::i18n::tr("Color Overlay Settings")).font(t.semibold(12.0)).color(t.text_label));
    ui.add_space(4.0);

    ui.label(egui::RichText::new(crate::i18n::tr("Custom Colors:")).color(t.text_dim));
    ui.horizontal(|ui| {
        for c in OVERLAY_COLORS {
            let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
            crate::widgets::announce(&resp, format!("{} #{:02x}{:02x}{:02x}", crate::i18n::tr("Overlay color"), c[0], c[1], c[2]));
            register(ui.ctx(), format!("maskOverlayColor:{:02x}{:02x}{:02x}", c[0], c[1], c[2]), r);
            ui.painter().rect_filled(r.shrink(2.0), 3.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
            if app.ui.mask_overlay_color == c {
                ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, t.text), egui::StrokeKind::Inside);
            }
            if resp.clicked() {
                let _ = app.run("view.maskOverlayColor", json!({"color": c}));
            }
        }
    });

    ui.add_space(6.0);
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
        shade: lcc_develop::Shade::Plain,
    };
    let out = slider(ui, &spec, app.ui.mask_overlay_opacity as f64, true, None);
    if let Some(v) = out.value {
        app.ui.mask_overlay_opacity = v as f32;
    }
}

/// Dedicated Mask Overlay display options menu under the "..." button.
pub(crate) fn overlay_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    use lcc_pipeline::MaskView;
    let t = Tokens::get(ui.ctx());
    let current_mode = MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default();
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;

    // 1. Overlay Modes
    for v in MaskView::ALL {
        let is_current = v == current_mode;
        let b = egui::Button::new(format!("      {}", menu_label(v.label())));
        let r = ui.add(b);
        register(ui.ctx(), format!("maskOverlayMode:{}", v.name()), r.rect);
        if is_current {
            let c = egui::Rect::from_center_size(egui::pos2(r.rect.left() + 11.0, r.rect.center().y), egui::vec2(13.0, 13.0));
            crate::icons::paint(ui.painter(), c, Icon::Check, t.text);
        }
        if r.clicked() {
            // choosing how it looks doesn't switch the overlay on for good (OneSlips 0c58de7ad; ⇧O and Show Overlay do)
            let _ = app.run("view.maskOverlayMode", json!({"mode": v.name(), "show": false}));
            ui.close();
        }
    }

    ui.separator();

    // 2. Quick Toggles & Pins
    // Automatically Toggle Overlay
    let auto_on = app.ui.mask_overlay_auto;
    let b_auto = egui::Button::new(format!("      {}", crate::i18n::tr("Automatically Toggle Overlay")));
    let r_auto = ui.add(b_auto);
    register(ui.ctx(), "menuitem:maskOverlayAuto", r_auto.rect);
    if auto_on {
        let c = egui::Rect::from_center_size(egui::pos2(r_auto.rect.left() + 11.0, r_auto.rect.center().y), egui::vec2(13.0, 13.0));
        crate::icons::paint(ui.painter(), c, Icon::Check, t.text);
    }
    if r_auto.clicked() {
        let _ = app.run("view.maskOverlayAuto", json!({"auto": !auto_on}));
        ui.close();
    }

    // Show Unselected Mask Pins (Alt+V / ⌥V)
    let unselected_on = app.ui.mask_unselected_pins;
    let mut b_unsel = egui::Button::new(format!("      {}", crate::i18n::tr("Show Unselected Mask Pins")));
    b_unsel = b_unsel.shortcut_text(if mac { "⌥V" } else { "Alt+V" });
    let r_unsel = ui.add(b_unsel);
    register(ui.ctx(), "menuitem:maskUnselectedPins", r_unsel.rect);
    if unselected_on {
        let c = egui::Rect::from_center_size(egui::pos2(r_unsel.rect.left() + 11.0, r_unsel.rect.center().y), egui::vec2(13.0, 13.0));
        crate::icons::paint(ui.painter(), c, Icon::Check, t.text);
    }
    if r_unsel.clicked() {
        let _ = app.run("view.maskUnselectedPins", json!({"show": !unselected_on}));
        ui.close();
    }

    // Show Pins and Tools (V)
    let pins_on = app.ui.mask_pins;
    let mut b_pins = egui::Button::new(format!("      {}", crate::i18n::tr("Show Pins and Tools")));
    b_pins = b_pins.shortcut_text("V");
    let r_pins = ui.add(b_pins);
    register(ui.ctx(), "menuitem:maskPins", r_pins.rect);
    register(ui.ctx(), "menuitem:view.maskPins", r_pins.rect);
    if pins_on {
        let c = egui::Rect::from_center_size(egui::pos2(r_pins.rect.left() + 11.0, r_pins.rect.center().y), egui::vec2(13.0, 13.0));
        crate::icons::paint(ui.painter(), c, Icon::Check, t.text);
    }
    if r_pins.clicked() {
        let _ = app.run("view.maskPins", json!({"show": !pins_on}));
        ui.close();
    }

    // Show All Overlays (⇧⌘O / Ctrl+Shift+O)
    let all_on = app.ui.mask_overlay_all;
    let mut b_all = egui::Button::new(format!("      {}", crate::i18n::tr("Show All Overlays")));
    b_all = b_all.shortcut_text(if mac { "⇧⌘O" } else { "Ctrl+Shift+O" });
    let r_all = ui.add(b_all);
    register(ui.ctx(), "menuitem:maskOverlayAll", r_all.rect);
    register(ui.ctx(), "maskOverlayAll", r_all.rect);
    if all_on {
        let c = egui::Rect::from_center_size(egui::pos2(r_all.rect.left() + 11.0, r_all.rect.center().y), egui::vec2(13.0, 13.0));
        crate::icons::paint(ui.painter(), c, Icon::Check, t.text);
    }
    if r_all.clicked() {
        let _ = app.run("view.maskOverlayAll", json!({}));
        ui.close();
    }

    ui.separator();

    // 3. Color Overlay Settings submenu
    crate::widgets::menu_button(ui, crate::i18n::tr("Color Overlay Settings…"), |ui| {
        color_overlay_popup(app, ui);
    });
}

/// The Compact Show Overlay switch (OneSlips 0c58de7ad): a button with the overlay's own icon (a frame with a tinted
/// area, not the Masking tool's) and its name, lit in the overlay's colour while the overlay is switched on to stay
/// (O); the tooltip says which state it is in. Same widget ids as Classic's checkbox (`check:maskOverlay`,
/// `maskOverlay`).
fn overlay_button(app: &mut LccApp, ui: &mut egui::Ui) {
    let on = app.ui.mask_overlay;
    let tip = if on { "Overlay always on: O turns it off" } else { "Show overlay (O)" };
    let b = icon_button(ui, "maskOverlayOn", Icon::MaskOverlay, vec2(20.0, 20.0), on, true, crate::i18n::tr(tip));
    let t = Tokens::get(ui.ctx());
    let label =
        ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr("Show Overlay")).font(t.font(12.0)).color(t.text_label)).sense(Sense::click()));
    register(ui.ctx(), "check:maskOverlay", b.rect);
    register(ui.ctx(), "maskOverlay", b.rect);
    if on {
        // lit: the tinted area in the overlay's own colour, and a ring round the button
        let [r, g, bl] = app.ui.mask_overlay_color;
        let c = egui::Color32::from_rgb(r, g, bl);
        ui.painter().circle_filled(b.rect.center() + vec2(0.7, 0.35), 3.2, c.gamma_multiply(0.85));
        ui.painter().rect_stroke(b.rect.shrink(0.5), t.radius(3.0), Stroke::new(1.0, c), egui::StrokeKind::Inside);
    }
    if b.clicked() || label.clicked() {
        let _ = app.run("view.maskOverlay", json!({"show": !on}));
    }
}

/// The Show Overlay row with toggle checkbox, color swatch, and "..." menu.
fn show_overlay_row(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        if crate::theme::compact(ui.ctx()) {
            overlay_button(app, ui);
        } else {
            let mut show_overlay = app.ui.mask_overlay;
            let r = ui.checkbox(&mut show_overlay, crate::i18n::tr("Show Overlay"));
            register(ui.ctx(), "check:maskOverlay", r.rect);
            register(ui.ctx(), "maskOverlay", r.rect);
            if r.changed() {
                let _ = app.run("view.maskOverlay", json!({"show": show_overlay}));
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let more = ui.button("…").on_hover_text(crate::i18n::tr("Mask Overlay Options"));
            register(ui.ctx(), "button:maskOverlayMenu", more.rect);
            register(ui.ctx(), "maskOverlayMode", more.rect);
            egui::Popup::menu(&more).show(|ui| {
                overlay_menu(app, ui);
            });

            let all = app.ui.mask_overlay_all;
            let all_mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
            let all_base = crate::i18n::tr("Show all overlays");
            let all_tip = match crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.maskOverlayAll") {
                Some(sc) => format!("{all_base} ({})", crate::menubar::shortcut_text(sc, all_mac)),
                None => all_base.to_string(),
            };
            if icon_button(ui, "maskOverlayAll", Icon::Stack, vec2(20.0, 20.0), all, true, &all_tip).clicked() {
                let _ = app.run("view.maskOverlayAll", json!({}));
            }

            let [cr, cg, cb] = app.ui.mask_overlay_color;
            let (swatch_rect, swatch_resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            swatch_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, false, crate::i18n::tr("Overlay Color")));
            register(ui.ctx(), "maskOverlayColor", swatch_rect);
            register(ui.ctx(), "button:maskOverlayColor", swatch_rect);
            ui.painter().rect_filled(swatch_rect.shrink(1.0), 3.0, egui::Color32::from_rgb(cr, cg, cb));
            ui.painter().rect_stroke(swatch_rect.shrink(1.0), 3.0, Stroke::new(1.0, t.divider), egui::StrokeKind::Inside);
            let swatch_resp = swatch_resp.on_hover_text(crate::i18n::tr("Overlay Color and Opacity"));
            egui::Popup::menu(&swatch_resp).show(|ui| {
                color_overlay_popup(app, ui);
            });
        });
    });
}

/// Controls for a range component: the selected range (two handles over a dark → light bar),
/// Smoothness, and Show Luminance Map; colour ranges get Refine.
/// Who a People component is about: everyone or the n-th person (as it was made), a person named
/// on this photo, or a role of the shoot (`people.setRole`). Choosing one stores it in the mask's
/// recipe and finds that person here (`masks.recompute`); on other photos presets, Paste and Sync
/// find them again.
fn who_picker(app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId, comp: usize, shape: &MaskShape) {
    let MaskShape::People { person, parts, who, .. } = shape else { return };
    let tr = crate::i18n::tr;
    let mut names: Vec<String> = app
        .session
        .catalog
        .photo(photo)
        .map(|p| {
            p.meta
                .regions
                .iter()
                .filter(|r| r.kind == lcc_meta::RegionKind::Face)
                .filter_map(|r| r.name.clone())
                .filter(|n| !n.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let mut roles: Vec<String> = app.session.people_store().roles.iter().map(|b| b.role.clone()).collect();
    roles.sort_by_key(|r| r.to_lowercase());
    roles.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let current = match who {
        Some(lcc_develop::recipe::Who::Name(n)) => n.clone(),
        Some(lcc_develop::recipe::Who::Role(r)) => format!("{} ({})", r, tr("role")),
        None if *person > 0 => format!("{} {person}", tr("Person")),
        None => tr("Everyone").to_string(),
    };
    let mut chosen: Option<Option<lcc_develop::recipe::Who>> = None;
    // (wrapped: in a narrow panel, Compact's above all, Set Role… and Roles… go to a second line
    // instead of past the edge)
    ui.horizontal_wrapped(|ui| {
        ui.add_space(if crate::theme::compact(ui.ctx()) { 4.0 } else { 20.0 });
        ui.label(tr("Who"));
        let r = egui::ComboBox::from_id_salt(("maskWho", comp)).selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(who.is_none() && *person == 0, tr("Everyone")).clicked() {
                chosen = Some(None);
            }
            for n in &names {
                let on = matches!(who, Some(lcc_develop::recipe::Who::Name(x)) if x.eq_ignore_ascii_case(n));
                if ui.selectable_label(on, n).clicked() {
                    chosen = Some(Some(lcc_develop::recipe::Who::Name(n.clone())));
                }
            }
            if !roles.is_empty() {
                ui.separator();
            }
            for r in &roles {
                let on = matches!(who, Some(lcc_develop::recipe::Who::Role(x)) if x.eq_ignore_ascii_case(r));
                if ui.selectable_label(on, r).clicked() {
                    chosen = Some(Some(lcc_develop::recipe::Who::Role(r.clone())));
                }
            }
        });
        register(ui.ctx(), format!("maskWho:{comp}"), r.response.rect);
        let open = app.ui.role_draft.is_some();
        if text_button(ui, "maskSetRole", tr("Set Role…"), open).clicked() {
            app.ui.role_draft = if open { None } else { Some((String::new(), names.first().cloned().unwrap_or_default())) };
        }
        let shown = app.ui.roles_window;
        if text_button(ui, "maskRoles", tr("Roles…"), shown).clicked() {
            app.ui.roles_window = !shown;
        }
    });
    // who has a role in this shoot (the photo's folder): "Bride" is Sam here
    let mut set: Option<(String, String)> = None;
    if let Some((role, person)) = app.ui.role_draft.as_mut() {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            crate::text_field::TextField::singleline("maskRoleName", role).hint(tr("Role")).width(110.0).show(ui);
            crate::text_field::TextField::singleline("maskRolePerson", person).hint(tr("Person")).width(110.0).show(ui);
        });
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            let ready = !role.trim().is_empty() && !person.trim().is_empty();
            if ui.add_enabled_ui(ready, |ui| text_button(ui, "maskRoleSet", tr("Set for This Folder"), false)).inner.clicked() {
                set = Some((role.trim().to_string(), person.trim().to_string()));
            }
        });
    }
    if let Some((role, person)) = set {
        // the photo's folder; a photo outside any folder (demo, a phone import) keeps it to itself
        let scope = if app.session.photo_context(photo).folder.is_some() { json!("folder") } else { json!({"ids": [photo.0]}) };
        match app.run("people.setRole", json!({"role": role, "person": person, "id": photo.0, "scope": scope})) {
            Ok(_) => {
                app.ui.role_draft = None;
                let shape = MaskShape::People { person: 0, parts: parts.clone(), who: Some(lcc_develop::recipe::Who::Role(role)), seg: None };
                let _ = app.run("mask.update", json!({"component": comp, "shape": shape}));
                let _ = app.run("masks.recompute", json!({"id": photo.0}));
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    if let Some(w) = chosen {
        let shape = MaskShape::People { person: 0, parts: parts.clone(), who: w, seg: None };
        let _ = app.run("mask.update", json!({"component": comp, "shape": shape}));
        let _ = app.run("masks.recompute", json!({"id": photo.0}));
    }
}

/// What a role binding's scope covers, in words.
fn scope_words(scope: &serde_json::Value) -> String {
    let tr = crate::i18n::tr;
    if let Some(f) = scope.get("folder").and_then(serde_json::Value::as_str) {
        let name = std::path::Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or(f);
        return format!("{} {name}", tr("Folder"));
    }
    if scope.get("album").is_some() {
        return tr("Album").to_string();
    }
    let dates = scope.get("dates").unwrap_or(scope);
    if let (Some(a), Some(b)) = (dates.get("from").and_then(serde_json::Value::as_str), dates.get("to").and_then(serde_json::Value::as_str)) {
        return format!("{a} – {b}");
    }
    if let Some(n) = scope.get("photos").or_else(|| scope.get("ids")).and_then(serde_json::Value::as_array).map(Vec::len) {
        return crate::i18n::tr_format!("{n} photos", n = n);
    }
    tr("Everywhere").to_string()
}

/// The Roles window: every role binding (who is the Bride, and where), the ones that apply to
/// this photo first, each with Clear. Roles are set from a People mask's Set Role….
fn roles_window(app: &mut LccApp, ctx: &egui::Context) {
    let tr = crate::i18n::tr;
    let r = app.run("people.roles", json!({})).unwrap_or_default();
    let here: Vec<(String, String)> = r
        .get("here")
        .and_then(serde_json::Value::as_array)
        .map(|v| v.iter().filter_map(|b| Some((b["role"].as_str()?.to_string(), b["person"].as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let all: Vec<serde_json::Value> = r.get("roles").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    let mut open = true;
    let mut clear: Option<(String, serde_json::Value)> = None;
    egui::Window::new(tr("Roles")).id(egui::Id::new("rolesWindow")).open(&mut open).collapsible(false).resizable(false).default_width(320.0).show(
        ctx,
        |ui| {
            let t = Tokens::get(ui.ctx());
            register(ui.ctx(), "window:roles", ui.max_rect());
            if all.is_empty() {
                ui.label(egui::RichText::new(tr("No roles yet. Give one with Set Role… under a People mask.")).color(t.text_dim));
                return;
            }
            egui::Grid::new("rolesGrid").num_columns(4).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                for (i, b) in all.iter().take(500).enumerate() {
                    let (role, person) = (b["role"].as_str().unwrap_or_default(), b["person"].as_str().unwrap_or_default());
                    let applies = here.iter().any(|(r, p)| r == role && p == person);
                    let colour = if applies { t.text } else { t.text_dim };
                    ui.label(egui::RichText::new(role).color(colour).strong());
                    ui.label(egui::RichText::new(person).color(colour));
                    ui.label(egui::RichText::new(scope_words(&b["scope"])).color(t.text_dim).size(11.5));
                    if text_button(ui, &format!("roleClear:{i}"), tr("Clear"), false).clicked() {
                        // (as `people.clearRole` takes a scope: dates as {from, to}, photos as {ids})
                        let sc = &b["scope"];
                        let scope = match (sc.get("dates"), sc.get("photos")) {
                            (Some(d), _) => d.clone(),
                            (_, Some(p)) => json!({"ids": p}),
                            _ => sc.clone(),
                        };
                        clear = Some((role.to_string(), scope));
                    }
                    ui.end_row();
                }
            });
        },
    );
    if !open {
        app.ui.roles_window = false;
    }
    if let Some((role, scope)) = clear
        && let Err(e) = app.run("people.clearRole", json!({"role": role, "scope": scope}))
    {
        app.toast(ctx, e);
    }
}

fn range_controls(app: &mut LccApp, ui: &mut egui::Ui, comp: usize, shape: &MaskShape) {
    let t = Tokens::get(ui.ctx());
    let update = |app: &mut LccApp, shape: MaskShape| {
        let _ = app.run("mask.update", json!({"component": comp, "shape": shape}));
    };
    match shape {
        MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Select Luminance Range")).color(t.text_dim));
                let picking = app.ui.tool == "lumRange";
                if text_button(ui, &format!("lumPick{comp}"), crate::i18n::tr("Pick"), picking)
                    .on_hover_text(crate::i18n::tr("Click the photo to pick its tone, or drag over an area to pick its tones; ⇧ widens the range"))
                    .clicked()
                {
                    app.ui.tool = if picking { String::new() } else { "lumRange".into() };
                }
            });
            ui.add_space(4.0);
            let (lo, hi) = (*lo, *hi);
            let total_w = ui.available_width();
            let track_h = 18.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(total_w, 24.0), Sense::click_and_drag());
            crate::widgets::announce(&resp, crate::i18n::tr("Select Luminance Range"));
            let track_rect = Rect::from_min_size(rect.min, vec2(total_w, track_h));
            register(ui.ctx(), format!("lumRange:{comp}"), track_rect);
            let p = ui.painter();
            // smooth horizontal gradient: black to white
            let mut mesh = egui::Mesh::default();
            mesh.vertices.push(egui::epaint::Vertex { pos: track_rect.left_top(), uv: egui::epaint::WHITE_UV, color: egui::Color32::BLACK });
            mesh.vertices.push(egui::epaint::Vertex { pos: track_rect.left_bottom(), uv: egui::epaint::WHITE_UV, color: egui::Color32::BLACK });
            mesh.vertices.push(egui::epaint::Vertex { pos: track_rect.right_top(), uv: egui::epaint::WHITE_UV, color: egui::Color32::WHITE });
            mesh.vertices.push(egui::epaint::Vertex { pos: track_rect.right_bottom(), uv: egui::epaint::WHITE_UV, color: egui::Color32::WHITE });
            mesh.indices.extend_from_slice(&[0, 1, 2, 2, 1, 3]);
            p.add(egui::Shape::mesh(mesh));
            p.rect_stroke(track_rect, 0.0, Stroke::new(1.0, egui::Color32::from_gray(60)), egui::StrokeKind::Inside);

            let x = |v: f64| track_rect.left() + v.clamp(0.0, 1.0) as f32 * total_w;
            // High/low threshold indicators: vertical line across track and circular pin handle at bottom
            for v in [lo, hi] {
                let px = x(v);
                p.line_segment([pos2(px, track_rect.top()), pos2(px, track_rect.bottom())], Stroke::new(1.5, t.accent));
                p.circle_filled(pos2(px, track_rect.bottom()), 4.5, egui::Color32::WHITE);
                p.circle_stroke(pos2(px, track_rect.bottom()), 4.5, Stroke::new(1.5, t.accent));
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
                let v = (((pos.x - track_rect.left()) / total_w) as f64).clamp(0.0, 1.0);
                let low =
                    if resp.dragged() { ui.data(|d| d.get_temp::<bool>(handle_id)).unwrap_or_else(|| nearer_low(pos.x)) } else { nearer_low(pos.x) };
                let (nlo, nhi) = if low { (v.min((hi - 0.01).max(0.0)), hi) } else { (lo, v.max((lo + 0.01).min(1.0))) };
                update(app, MaskShape::LuminanceRange { lo: nlo, hi: nhi, lo_feather: *lo_feather, hi_feather: *hi_feather });
            }
            if resp.drag_stopped() {
                ui.data_mut(|d| d.remove::<bool>(handle_id));
                let _ = app.run("develop.endInteraction", json!({}));
            }
            ui.add_space(4.0);
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
                shade: lcc_develop::Shade::Plain,
            };
            let cur = ((lo_feather + hi_feather) / 2.0 / 0.5 * 100.0).clamp(0.0, 100.0);
            let out = slider(ui, &smooth, cur, true, None);
            apply_slider_out(app, &smooth, out, |app, v| {
                let f = v / 100.0 * 0.5;
                app.run("mask.update", json!({"component": comp, "shape": MaskShape::LuminanceRange { lo, hi, lo_feather: f, hi_feather: f }}))
            });
            let mut map = app.ui.mask_overlay && app.ui.mask_overlay_mode == "colorOnBw";
            let r = ui.checkbox(&mut map, crate::i18n::tr("Show Luminance Map"));
            register(ui.ctx(), format!("check:lumMap{comp}"), r.rect);
            if r.changed() {
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
        MaskShape::DepthRange { lo, hi, feather } => {
            // near (white, left) to far (black, right), as Lightroom's depth track
            ui.add_space(4.0);
            let (lo, hi, feather) = (*lo, *hi, *feather);
            let total_w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(vec2(total_w, 24.0), Sense::click_and_drag());
            crate::widgets::announce(&resp, crate::i18n::tr("Depth Range"));
            let track_rect = Rect::from_min_size(rect.min, vec2(total_w, 18.0));
            register(ui.ctx(), format!("depthRange:{comp}"), track_rect);
            let p = ui.painter();
            let mut mesh = egui::Mesh::default();
            for (pos, c) in [
                (track_rect.left_top(), egui::Color32::WHITE),
                (track_rect.left_bottom(), egui::Color32::WHITE),
                (track_rect.right_top(), egui::Color32::BLACK),
                (track_rect.right_bottom(), egui::Color32::BLACK),
            ] {
                mesh.vertices.push(egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color: c });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 2, 1, 3]);
            p.add(egui::Shape::mesh(mesh));
            p.rect_stroke(track_rect, 0.0, Stroke::new(1.0, egui::Color32::from_gray(60)), egui::StrokeKind::Inside);
            let x = |v: f64| track_rect.left() + v.clamp(0.0, 1.0) as f32 * total_w;
            for v in [lo, hi] {
                let px = x(v);
                p.line_segment([pos2(px, track_rect.top()), pos2(px, track_rect.bottom())], Stroke::new(1.5, t.accent));
                p.circle_filled(pos2(px, track_rect.bottom()), 4.5, egui::Color32::WHITE);
                p.circle_stroke(pos2(px, track_rect.bottom()), 4.5, Stroke::new(1.5, t.accent));
            }
            let handle_id = resp.id.with("depth-handle");
            let nearer_low = |px: f32| (px - x(lo)).abs() <= (px - x(hi)).abs();
            if resp.drag_started() {
                if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                    ui.data_mut(|d| d.insert_temp(handle_id, nearer_low(origin.x)));
                }
                let _ = app.run("develop.beginInteraction", json!({"label": "Depth Range"}));
            }
            if (resp.dragged() || resp.clicked())
                && let Some(pos) = resp.interact_pointer_pos()
                && total_w > 0.0
            {
                let v = (((pos.x - track_rect.left()) / total_w) as f64).clamp(0.0, 1.0);
                let low =
                    if resp.dragged() { ui.data(|d| d.get_temp::<bool>(handle_id)).unwrap_or_else(|| nearer_low(pos.x)) } else { nearer_low(pos.x) };
                let (nlo, nhi) = if low { (v.min((hi - 0.01).max(0.0)), hi) } else { (lo, v.max((lo + 0.01).min(1.0))) };
                update(app, MaskShape::DepthRange { lo: nlo, hi: nhi, feather });
            }
            if resp.drag_stopped() {
                ui.data_mut(|d| d.remove::<bool>(handle_id));
                let _ = app.run("develop.endInteraction", json!({}));
            }
            ui.add_space(4.0);
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
                shade: lcc_develop::Shade::Plain,
            };
            let out = slider(ui, &smooth, (feather / 0.5 * 100.0).clamp(0.0, 100.0), true, None);
            apply_slider_out(app, &smooth, out, |app, v| {
                app.run("mask.update", json!({"component": comp, "shape": MaskShape::DepthRange { lo, hi, feather: v / 100.0 * 0.5 }}))
            });
        }
        MaskShape::Radial { feather, .. } => {
            // the softness of the edge, as the inner ring on the photo shows it (both stay in step)
            let spec = ControlSpec {
                id: "feather",
                label: "Feather",
                section: Section::Light,
                min: 0.0,
                max: 100.0,
                default: 50.0,
                step: 1.0,
                decimals: 0,
                track: Track::Plain,
                shade: lcc_develop::Shade::Plain,
            };
            let out = slider(ui, &spec, *feather, true, None);
            let base = shape.clone();
            apply_slider_out(app, &spec, out, |app, v| {
                let mut s = base.clone();
                if let MaskShape::Radial { feather, .. } = &mut s {
                    *feather = v.clamp(0.0, 100.0);
                }
                app.run("mask.update", json!({"component": comp, "shape": s}))
            });
        }
        MaskShape::ColorRange { samples, refine, areas } => {
            ui.horizontal(|ui| {
                // the picked colours, as swatches
                for (i, s) in samples.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, super::range_pick::oklab_color(*s));
                    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, t.button_border), egui::StrokeKind::Inside);
                    register(ui.ctx(), format!("colorSwatch:{comp}:{i}"), r);
                }
                if samples.is_empty() {
                    ui.label(egui::RichText::new(crate::i18n::tr("No colour picked")).color(t.text_dim));
                }
                let picking = app.ui.tool == "colorRange";
                if text_button(ui, &format!("colorPick{comp}"), crate::i18n::tr("Pick"), picking)
                    .on_hover_text(crate::i18n::tr(
                        "Click or drag over the photo to pick colours; ⇧ adds more (up to 5), ⌥-click removes one, drag a pin to move it",
                    ))
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
                shade: lcc_develop::Shade::Plain,
            };
            let out = slider(ui, &spec, *refine, true, None);
            let samples = samples.clone();
            apply_slider_out(app, &spec, out, |app, v| {
                app.run(
                    "mask.update",
                    json!({"component": comp, "shape": MaskShape::ColorRange { samples: samples.clone(), refine: v, areas: areas.clone() }}),
                )
            });
        }
        _ => {}
    }
}

/// The right-click / "…" menu of one component of the selected mask.
fn component_row_menu(app: &mut LccApp, ui: &mut egui::Ui, mask: u32, k: usize, label: &str, count: usize) {
    let comp_data = app.session.active().and_then(|p| app.session.develop_of(p)).and_then(|d| {
        let m = d.masks.iter().find(|m| m.id == mask)?;
        let is_people = matches!(m.components.get(k).map(|c| &c.shape), Some(MaskShape::People { .. }));
        Some((m.uses_ai(), is_people))
    });
    if comp_data.is_some_and(|(_, is_people)| is_people) {
        let text = if app.ui.show_people_who {
            format!("✓ {}", crate::i18n::tr("Show Who & Roles"))
        } else {
            crate::i18n::tr("Show Who & Roles").to_string()
        };
        let b = ui.button(text);
        register(ui.ctx(), "maskMenu:toggleWho", b.rect);
        if b.clicked() {
            app.ui.show_people_who = !app.ui.show_people_who;
            ui.close();
        }
        ui.separator();
    }
    if comp_data.is_some_and(|(ai, _)| ai) {
        if ui.button(menu_label("Update AI Mask")).clicked() {
            if let Err(e) = app.run("mask.updateAi", json!({"id": mask})) {
                app.ai_error(ui.ctx(), e, None);
            }
            ui.close();
        }
        ui.separator();
    }
    let mut run = |ui: &mut egui::Ui, text: &str, p: serde_json::Value| {
        if ui.button(menu_label(text)).clicked() {
            let mut p = p;
            crate::json_set(&mut p, "id", json!(mask));
            crate::json_set(&mut p, "component", json!(k));
            let _ = app.run("mask.component", p);
            ui.close();
        }
    };
    run(ui, "Invert", json!({"action": "invert"}));
    run(ui, &crate::i18n::tr_format!("Duplicate \"{label}\"", label = label), json!({"action": "duplicate"}));

    if k > 0 {
        crate::widgets::menu_button(ui, crate::i18n::tr("Mode"), |ui| {
            for (op, text) in [("add", "Add"), ("subtract", "Subtract"), ("intersect", "Intersect")] {
                run(ui, text, json!({"action": "op", "op": op}));
            }
        });
    }
    crate::widgets::menu_button(ui, crate::i18n::tr("Intersect with"), |ui| component_menu(app, ui, "intersect"));
    crate::widgets::menu_button(ui, crate::i18n::tr("Subtract"), |ui| component_menu(app, ui, "subtract"));
    ui.separator();
    if ui.button(crate::i18n::tr("Rename…")).clicked() {
        app.ui.renaming_component = Some((mask, k, label.to_string()));
        ui.close();
    }
    let del = if count == 1 {
        crate::i18n::tr_format!("Delete \"{label}\" (and the mask)", label = label)
    } else {
        crate::i18n::tr_format!("Delete \"{label}\"", label = label)
    };
    if ui.button(del).clicked() {
        let _ = app.run("mask.component", json!({"id": mask, "component": k, "action": "delete"}));
        ui.close();
    }
}

/// Whether the model an AI mask needs is missing in a build that could use it (SAM 3, or the open
/// models chosen for Object or serving Describe): then the download is offered (with `then` to start afterwards) instead of starting an AI mask.
fn needs_model(app: &mut LccApp, kind: &str, op: &str) -> bool {
    let seg = &app.session.segmenter;
    if kind == "object" && seg.object_model_in_use() == "sam2" {
        // SAM 2.1 was chosen for Object masks: without it, the AI Models dialog offers it
        let missing = lcc_engine::segment::Segmenter::AVAILABLE && !seg.open_model_installed("sam2");
        if missing && app.ui.dialog.is_none() {
            app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
        }
        return missing;
    }
    if kind == "prompt" && seg.models_root.is_some() && !seg.installed() && seg.remote_endpoint().is_none() {
        // without SAM 3, Describe runs on the open OWLv2 finder and SAM 2.1: the AI Models dialog
        // offers whichever is missing
        let missing = lcc_engine::segment::Segmenter::AVAILABLE && !seg.describe_needs().is_empty();
        if missing && app.ui.dialog.is_none() {
            app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
        }
        return missing;
    }
    let missing = lcc_engine::segment::Segmenter::AVAILABLE && seg.remote_endpoint().is_none() && seg.dir.is_some() && !seg.installed();
    if missing {
        app.offer_sam_download(Some((kind, op)));
    }
    missing
}

/// Start an Object selection (SAM 3 clicks): a new mask, or a component of the selected one
/// combined by `op`; the photo is analyzed meanwhile (in the background).
pub(crate) fn start_object(app: &mut LccApp, ctx: &egui::Context, op: &str) {
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
            if let Some(d) = app.session.active().and_then(|id| app.session.develop_of(id))
                && let Some(m) = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask)
            {
                app.ui.active_mask_component = Some(m.components.len().saturating_sub(1));
            }
            app.toast(ctx, crate::i18n::tr("Click the object to select it · ⌥-click leaves a part out"));
        }
        Err(e) => app.ai_error(ctx, e, Some(("object", op))),
    }
}

/// Start an AI mask of `kind` (object|prompt) combined by `op` (new|add|subtract|intersect),
/// after the model was installed.
pub(crate) fn begin_ai(app: &mut LccApp, kind: &str, op: &str) -> Result<serde_json::Value, String> {
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
pub(crate) fn start_describe(app: &mut LccApp, op: &str) {
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
fn describe_field(app: &mut LccApp, ui: &mut egui::Ui, new_mask: bool) {
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
    // the (translated) Select button is placed first, at the right; the field takes what is left
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let seg = &app.session.segmenter;
        let working = segmenter_working(seg.analyzing(), seg.busy(), seg.detail_busy(), app.ui.detail_due.is_some());
        if crate::button::Button::new("Select").small().busy(working).widget("button:maskDescribeGo").show(ui).clicked() {
            submit = true;
        }
        let r = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(crate::i18n::tr("e.g. sky · the red car · car, road"))
                .desired_width(ui.available_width()),
        );
        register(ui.ctx(), "maskDescribe", r.rect);
        if !r.has_focus() && !r.lost_focus() && text.is_empty() {
            r.request_focus();
        }
        // Enter is the Select button: nothing starts while the segmenter is working
        if enter_submits(working, r.lost_focus(), ui.input(|i| i.key_pressed(egui::Key::Enter))) {
            submit = true;
        }
    });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
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

/// Add / Subtract / Intersect with: every kind Create New Mask offers ([`MASK_KINDS`]), in its order and with its icons
/// (OneSlips 1138a5edd; Background, People, Remembered and Color were missing). People and Remembered open their
/// picker, whose selection then combines with the selected mask.
fn component_menu(app: &mut LccApp, ui: &mut egui::Ui, op: &str) {
    let t = Tokens::get(ui.ctx());
    for (kind, label) in MASK_KINDS {
        // painting only adds or erases
        if kind == "brush" && op == "intersect" {
            continue;
        }
        let label = match kind {
            "prompt" => "Describe…",
            "linear" => "Linear Gradient",
            "radial" => "Radial Gradient",
            "luminanceRange" => "Luminance Range",
            "colorRange" => "Color Range",
            "depthRange" => "Depth Range",
            "people" => "People…",
            "concept" => "Remembered…",
            _ => label,
        };
        // the tile's icon before the name
        let b = ui.button(format!("      {}", menu_label(label)));
        paint(ui.painter(), Rect::from_center_size(pos2(b.rect.left() + 13.0, b.rect.center().y), vec2(14.0, 14.0)), kind_icon(kind), t.text_label);
        register(ui.ctx(), format!("maskComp:{op}:{kind}"), b.rect);
        if b.clicked() {
            start_component(app, ui.ctx(), kind, op);
        }
    }
}

/// A component of `kind` the selected mask combines by `op`: tools wait for the photo, pickers open, the rest is made
/// at once.
pub(crate) fn start_component(app: &mut LccApp, ctx: &egui::Context, kind: &str, op: &str) {
    match kind {
        "object" => return start_object(app, ctx, op),
        "prompt" => return start_describe(app, op),
        "people" | "concept" => {
            // one picker at a time, its selection combined with the selected mask
            app.mask_pick_op = Some(op.to_string());
            app.ui.people_pick = (kind == "people").then(|| (0, Vec::new()));
            app.ui.concept_pick = kind == "concept";
            return;
        }
        "brush" => {
            let _ = app.run("tool.brush", json!({"op": op}));
        }
        _ => {
            if let Err(e) = app.run_typed("mask.addComponent", json!({"op": op, "kind": kind})) {
                return app.ai_error(ctx, e, None);
            }
            if matches!(kind, "linear" | "radial" | "colorRange" | "luminanceRange" | "depthRange") {
                app.ui.tool = kind.to_string();
            }
        }
    }
    if let Some(d) = app.session.active().and_then(|id| app.session.develop_of(id))
        && let Some(m) = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask)
    {
        app.ui.active_mask_component = Some(m.components.len().saturating_sub(1));
    }
}

/// Auto Mask range: how different a pixel may be from the one under the brush and still take the
/// paint, −100 (tight) … 0 (Lightroom's limits, a double-click goes back) … 100 (wide). Each stroke
/// keeps the range it was painted with.
fn auto_range_slider(app: &mut LccApp, ui: &mut egui::Ui) {
    let max = lcc_develop::AUTO_RANGE_MAX;
    let s = ControlSpec {
        id: "ui.brushAutoRange",
        label: "Auto Mask range",
        section: Section::Light,
        min: -max,
        max,
        default: 0.0,
        step: 1.0,
        decimals: 0,
        track: Track::Centered,
        shade: lcc_develop::Shade::Plain,
    };
    let out = slider(ui, &s, f64::from(app.ui.brush_auto_range), true, None);
    if let Some(v) = out.value.filter(|v| v.is_finite()) {
        app.ui.brush_auto_range = v.clamp(-max, max) as f32;
    }
}

fn brush_settings(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // the eraser keeps its own settings: shown and changed while erasing (Erase picked, or ⌥ / Alt
    // held to switch modes)
    let erasing = app.ui.brush_erase != ui.input(|i| i.modifiers.alt);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 6, bottom: 0 }).show(ui, |ui| {
        // wraps at narrow panel widths (and long translations) rather than widening the panel
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Brush")).font(t.semibold(13.0)));
            if text_button(ui, "brushAdd", crate::i18n::tr("Add"), !erasing).clicked() {
                app.ui.brush_erase = false;
            }
            if text_button(ui, "brushErase", crate::i18n::tr("Erase"), erasing).clicked() {
                app.ui.brush_erase = true;
            }
            ui.add_space(8.0);
            // Lightroom's brush A / B: two presets, each with its own size, feather, flow, density
            for (slot, id, label) in [(0u8, "brushA", "A"), (1, "brushB", "B")] {
                if text_button(ui, id, label, app.ui.brush_slot == slot).clicked() && app.ui.brush_slot != slot {
                    let _ = app.run("brush.preset", json!({"slot": label.to_ascii_lowercase()}));
                }
            }
            // (no extra space before Done: a space that ends the row widens the panel past its minimum)
            if text_button(ui, "brushDone", crate::i18n::tr("Done"), false).clicked() {
                disarm(app);
            }
        });
        let mut auto = app.ui.brush_auto_mask;
        if ui
            .checkbox(&mut auto, crate::i18n::tr("Auto Mask"))
            .on_hover_text(crate::i18n::tr("Paint only areas like the one under the brush"))
            .changed()
        {
            app.ui.brush_auto_mask = auto;
        }
    });
    // under Auto Mask, lined up with the brush's other sliders
    if app.ui.brush_auto_mask {
        auto_range_slider(app, ui);
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 0, bottom: 0 }).show(ui, |ui| {
        let mut pressure = app.ui.brush_pressure;
        if ui
            .checkbox(&mut pressure, crate::i18n::tr("Pen Pressure"))
            .on_hover_text(crate::i18n::tr("A pen's pressure sets the size and flow of the brush"))
            .changed()
        {
            app.ui.brush_pressure = pressure;
        }
    });
    let set = app.ui.brush_set(erasing);
    for (id, label, min, max, get) in [
        ("ui.brushSize", "Size", 1.0, 100.0, set.size as f64),
        ("ui.brushFeather", "Feather", 0.0, 100.0, set.feather as f64),
        ("ui.brushFlow", "Flow", 1.0, 100.0, set.flow as f64),
        ("ui.brushDensity", "Density", 0.0, 100.0, set.density as f64),
    ] {
        let s = ControlSpec {
            id,
            label,
            section: Section::Light,
            min,
            max,
            default: min,
            step: 1.0,
            decimals: 0,
            track: Track::Plain,
            shade: lcc_develop::Shade::Plain,
        };
        let out = slider(ui, &s, get, true, None);
        if let Some(v) = out.value {
            let mut s = app.ui.brush_set(erasing);
            let v = v as f32;
            match id {
                "ui.brushSize" => s.size = v,
                "ui.brushFeather" => s.feather = v,
                "ui.brushDensity" => s.density = v,
                _ => s.flow = v,
            }
            app.ui.set_brush_set(erasing, s);
        }
    }
}

/// A masks-panel icon button named `label` (translated; also its hover text), addressable as
/// `button:{id}` like the text button it replaced, and `icon:{id}`.
fn mask_icon(ui: &mut egui::Ui, id: &str, icon: Icon, label: &str, active: bool) -> egui::Response {
    let name = crate::i18n::tr(label);
    let r = crate::widgets::icon_button_named(ui, id, icon, vec2(32.0, 28.0), active, true, name, name);
    crate::widgets::register(ui.ctx(), format!("button:{id}"), r.rect);
    r
}

/// The on-photo tools that belong to Masking (armed from its panel or a shortcut). Colour Range's
/// eyedropper is not one: it also samples from Edit.
fn object_settings(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 6, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Object Selection")).font(t.semibold(13.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "objectDone", crate::i18n::tr("Done"), false).clicked() {
                    disarm(app);
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(crate::i18n::tr("Click an object on the photo to select it · ⌥-click leaves a part out"))
                .font(t.font(11.5))
                .color(t.text_dim),
        );
    });
}

/// What a Create New Mask tile of `kind` does when clicked (also Tools ▸ Create New Mask ▸ Select People / Color Range /
/// Luminance Range): arm the tool for the first click on the photo, open the picker under the tiles, or make the mask.
pub(crate) fn start_new(app: &mut LccApp, ctx: &egui::Context, kind: &str) {
    match kind {
        // armed: the first click on the photo makes the mask and picks there
        "colorRange" => match arm_new(app, "colorRange") {
            Ok(_) => app.toast(ctx, crate::i18n::tr("Click the photo to pick a colour · ⇧-click adds more")),
            Err(e) => app.toast(ctx, e),
        },
        "object" if !needs_model(app, "object", "new") => match arm_new(app, "object") {
            Ok(_) => app.toast(ctx, crate::i18n::tr("Click the object to select it · ⌥-click leaves a part out")),
            Err(e) => app.toast(ctx, e),
        },
        "object" => {}
        "prompt" => start_describe(app, "new"),
        // a picker under the tiles: who and which parts, or which object
        "people" => {
            app.ui.concept_pick = false;
            app.mask_pick_op = None;
            app.ui.people_pick = Some((0, Vec::new()));
        }
        "concept" => {
            app.ui.people_pick = None;
            app.mask_pick_op = None;
            app.ui.concept_pick = true;
        }
        "brush" | "linear" | "radial" => {
            let _ = app.run(&format!("tool.{kind}"), json!({"new": true}));
        }
        // no depth model: the AI Models settings offer its download
        "depthRange" => {
            if let Err(e) = app.run_typed("mask.add", json!({"kind": "depthRange"})) {
                app.ai_error(ctx, e, None);
            }
        }
        k => {
            let _ = app.run("mask.add", json!({"kind": k}));
        }
    }
}

pub(crate) fn is_mask_tool(tool: &str) -> bool {
    matches!(
        tool,
        "brush" | "linear" | "radial" | "object" | "prompt" | "depthRange" | "luminanceRange" | "lumRange" | "colorRange" | "maskPointColor"
    ) || tool.starts_with("maskMixer:")
}

/// Arm mask tool `kind` (`brush`, `linear`, `radial`, `colorRange`, `object`) for a new mask: no mask is selected, and the
/// first stroke or drag on the photo makes the mask (one masking mode at a time: a selected mask
/// and a tool waiting for a new one are never at once).
pub(crate) fn arm_new(app: &mut LccApp, kind: &str) -> Result<serde_json::Value, String> {
    let photo = app.session.active().ok_or_else(|| format!("tool.{kind}: no photo"))?;
    app.cancel_brush_gesture();
    app.run("mask.select", json!({"id": null}))?;
    app.ui.active_mask_component = None;
    app.ui.mask_new = Some((photo.0, kind.into()));
    app.ui.mask_drawing = None;
    app.ui.tool = kind.into();
    app.ui.tool_photo = Some(photo.0);
    app.ui.right = crate::state::RightPanel::Masking;
    app.ui.view = crate::state::ViewMode::Detail;
    Ok(json!({"armed": kind}))
}

/// Put every mask tool away: the tool on the photo and a tool waiting for a new mask.
pub(crate) fn disarm(app: &mut LccApp) {
    if is_mask_tool(&app.ui.tool) {
        app.ui.tool.clear();
    }
    app.ui.mask_new = None;
    app.ui.mask_drawing = None;
}

/// `mask.editComponent {id, component}`: make component `k` of mask `mid` the one edited on the
/// photo. The mask is selected and any tool waiting for a new mask put away; a Brush component
/// arms the brush on it (strokes add to it, erasing takes from it, by its own Add or Subtract),
/// any other kind puts the brush away so its handles take the drags.
pub(crate) fn edit_component(app: &mut LccApp, p: &serde_json::Value) -> Result<serde_json::Value, String> {
    let c = "mask.editComponent";
    let photo = app.session.active().ok_or_else(|| format!("{c}: no photo"))?;
    let mid = p.get("id").and_then(serde_json::Value::as_u64).and_then(|v| u32::try_from(v).ok()).or(app.session.active_mask);
    let mid = mid.ok_or_else(|| format!("{c}: no mask selected"))?;
    let k = p
        .get("component")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| format!("{c}: `component` must be a component index"))?;
    let d = app.session.develop_of(photo).unwrap_or_default();
    let m = d.masks.iter().find(|m| m.id == mid).ok_or_else(|| format!("{c}: no mask {mid}"))?;
    let shape = &m.components.get(k).ok_or_else(|| format!("{c}: mask {mid} has no component {k}"))?.shape;
    let brush = matches!(shape, MaskShape::Brush { .. });
    app.cancel_brush_gesture();
    disarm(app);
    if app.session.active_mask != Some(mid) {
        app.run("mask.select", json!({"id": mid}))?;
    }
    app.ui.active_mask_component = Some(k);
    // the component's own tool on the photo (its handles, eyedropper or clicks)
    let tool = match shape {
        MaskShape::Brush { .. } => "brush",
        MaskShape::Linear { .. } => "linear",
        MaskShape::Radial { .. } => "radial",
        MaskShape::ColorRange { .. } => "colorRange",
        MaskShape::LuminanceRange { .. } => "luminanceRange",
        MaskShape::DepthRange { .. } => "depthRange",
        MaskShape::Object { .. } => "object",
        MaskShape::Prompt { .. } => "prompt",
        _ => "",
    };
    if !tool.is_empty() {
        app.ui.right = crate::state::RightPanel::Masking;
        app.ui.tool = tool.into();
        app.ui.tool_photo = Some(photo.0);
    }
    if brush {
        app.ui.brush_erase = false;
    }
    Ok(json!({"mask": mid, "component": k, "brush": brush}))
}

/// Leaving Masking or the loupe puts its tools away, checked once a frame whatever path left it:
/// coming back never finds the brush armed (it hid the overlay elsewhere and made pin drags paint).
/// One masking mode at a time: another photo puts the tools away, and so does a mask selected
/// (its row, pin or a command) while a tool waits for a new mask.
pub(crate) fn put_tools_away(app: &mut LccApp) {
    sync_reset_new(app);
    let photo = app.session.active().map(|p| p.0);
    let left = app.ui.right != crate::state::RightPanel::Masking || app.ui.view != crate::state::ViewMode::Detail;
    let armed = is_mask_tool(&app.ui.tool) || app.ui.mask_new.is_some();
    let carried = carry_component(app, photo);
    let other_photo = app.ui.tool_photo.is_some() && app.ui.tool_photo != photo && !carried;
    let picked = app.ui.mask_new.is_some() && app.session.active_mask.is_some() && app.ui.mask_drawing.is_none();
    if armed && (left || other_photo || picked) {
        app.cancel_brush_gesture();
        disarm(app);
    }
    if is_mask_tool(&app.ui.tool) {
        if app.ui.tool_photo.is_none() || carried {
            app.ui.tool_photo = photo;
        }
    } else {
        app.ui.tool_photo = None;
    }
    app.ui.mask_held = photo.zip(app.session.active_mask).map(|(p, m)| (p, m, app.ui.active_mask_component));
    keep_selection(app);
}

/// What Masks remembers to land on a mask (OneSlips 0d9d4fb7d, test notes 2026-10-25 notes 8 and 9).
#[derive(Default)]
pub(crate) struct Landing {
    /// The mask selected last on each photo, this session.
    memory: std::collections::HashMap<u64, u32>,
    /// The photo on show and whether Masks was open, last frame: a change is "entering Masks".
    seen: Option<(Option<u64>, bool)>,
    /// The mask selected when a tool was armed for a new one (photo, mask).
    before_arm: Option<(u64, Option<u32>)>,
}

/// Which mask Masks has selected, worked out every frame:
///
/// - entering Masks, or another photo while in Masks, lands on a mask when the photo has any and none is selected
///   (another photo's linked copy is selected by the engine): the only one; else the one selected last on this
///   photo; else the first;
/// - a tool armed for a new mask deselects the selected one ([`arm_new`]); put away without making a mask (Esc, the
///   tile again) the one selected before is selected again.
pub(crate) fn keep_selection(app: &mut LccApp) {
    let photo = app.session.active().map(|p| p.0);
    let in_masks = app.ui.right == crate::state::RightPanel::Masking && app.ui.view == crate::state::ViewMode::Detail;
    let d = app.session.active().and_then(|id| app.session.develop_of(id)).unwrap_or_default();
    let has = |id: u32| d.masks.iter().any(|m| m.id == id);
    let armed = app.ui.mask_new.is_some();
    if armed {
        if app.mask_landing.before_arm.is_none_or(|(q, _)| Some(q) != photo) {
            let before = app.mask_landing.memory.get(&photo.unwrap_or_default()).copied();
            app.mask_landing.before_arm = photo.map(|p| (p, app.session.active_mask.or(before)));
        }
    } else if let Some((p, before)) = app.mask_landing.before_arm.take()
        && app.session.active_mask.is_none()
        && photo == Some(p)
        && let Some(id) = before.filter(|id| has(*id))
    {
        // put away without making a mask: the one selected before
        let _ = app.run("mask.select", json!({"id": id}));
    }
    let seen = (photo, in_masks);
    let entered = in_masks && app.mask_landing.seen != Some(seen);
    app.mask_landing.seen = Some(seen);
    let Some(p) = photo.filter(|_| in_masks) else { return };
    if entered && !armed && !app.session.active_mask.is_some_and(has) {
        let memory = app.mask_landing.memory.get(&p).copied().filter(|id| has(*id));
        let landing = match d.masks.as_slice() {
            [only] => Some(only.id),
            masks => memory.or_else(|| masks.first().map(|m| m.id)),
        };
        if let Some(id) = landing {
            let _ = app.run("mask.select", json!({"id": id}));
        }
    }
    if let Some(id) = app.session.active_mask.filter(|id| has(*id)) {
        app.mask_landing.memory.insert(p, id);
    }
}

/// Keep "Reset settings on new mask" in the saved UI settings: the saved choice goes to the engine
/// at start, and a `mask.next {resetNew}` from any surface comes back to be saved.
fn sync_reset_new(app: &mut LccApp) {
    let now = app.session.mask_reset_new;
    match app.ui.mask_reset_seen {
        Some(seen) if seen != now => app.ui.mask_reset_new = now,
        _ => app.session.mask_reset_new = app.ui.mask_reset_new,
    }
    app.ui.mask_reset_seen = Some(app.session.mask_reset_new);
}

/// Another photo with a mask selected: the engine selected its linked copy there (or none). The
/// component edited on the photo before stays the one edited when the copy has one of the same
/// kind at that place; otherwise none is. Returns whether a brush armed on it stays armed.
fn carry_component(app: &mut LccApp, photo: Option<u64>) -> bool {
    let Some((q, old_mid, comp)) = app.ui.mask_held else { return false };
    let Some(p) = photo.filter(|p| *p != q) else { return false };
    let Some(k) = comp else { return false };
    let kind_of = |pid: u64, mid: u32| {
        app.session
            .develop_of(PhotoId(pid))
            .and_then(|d| d.masks.iter().find(|m| m.id == mid).and_then(|m| m.components.get(k).map(|c| std::mem::discriminant(&c.shape))))
    };
    let before = kind_of(q, old_mid);
    let now = app.session.active_mask.and_then(|mid| kind_of(p, mid));
    if before.is_none() || before != now {
        app.ui.active_mask_component = None;
        return false;
    }
    app.ui.active_mask_component = Some(k);
    let brush = app.session.develop_of(PhotoId(p)).is_some_and(|d| {
        d.masks
            .iter()
            .any(|m| Some(m.id) == app.session.active_mask && m.components.get(k).is_some_and(|c| matches!(c.shape, MaskShape::Brush { .. })))
    });
    brush && app.ui.tool == "brush" && app.ui.mask_new.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    // issue #1145: Settings ▸ Expanded sliders off keeps the mask Hue to Lightroom's range
    #[test]
    fn expanded_sliders_widen_only_the_mask_hue() {
        let hue = LOCAL.iter().find(|s| s.id == "hue").unwrap();
        let off = local_spec(hue, false);
        assert!(off.max < 40.0 && off.min > -40.0 && off.max > 30.0, "{off:?}");
        let on = local_spec(hue, true);
        assert_eq!((on.min, on.max), (-180.0, 180.0));
        for s in LOCAL.iter().filter(|s| s.id != "hue") {
            let n = local_spec(s, false);
            assert_eq!((n.min, n.max), (s.min, s.max), "{}", s.id);
        }
    }

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

    #[test]
    fn the_color_overlay_popup_stays_narrow_on_a_wide_window() {
        let app = LccApp::new(lcc_engine::Session::new(), crate::Services { png: None, ..Default::default() });
        // the headless view's context has the app's fonts and theme installed
        let mut h = crate::headless::Headless::new(app, [3200.0, 1400.0], 1.0);
        h.settle(std::time::Duration::from_secs(30));
        let ctx = h.view.ctx.clone();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(3200.0, 1400.0))), ..Default::default() };
        let mut width = 0.0;
        let mut out = ctx.run_ui(input, |root| {
            egui::Area::new(egui::Id::new("overlayPopupTest")).show(root.ctx(), |ui| {
                color_overlay_popup(&mut h.app, ui);
                width = ui.min_rect().width();
            });
        });
        out.textures_delta.clear();
        assert!(width > 0.0 && width <= COLOR_OVERLAY_POPUP_WIDTH + 1.0, "popup is {width} points wide");
    }
}
