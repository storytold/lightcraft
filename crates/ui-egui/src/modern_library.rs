//! The Modern layout's Library extras (fork-owned): a floating search pill over the grid (text
//! search through the library filter, with ★3+ and Picks chips), and the Info card at the right of
//! the grid to read and edit the selected photos' metadata.
//!
//! The Info card writes through the same commands as Classic's Metadata panel (`photo.setMeta`,
//! `photo.rate`, `photo.label`, `photo.flag`) on every selected photo. A field whose value differs
//! across the selection shows `< mixed >` and is written only when it is edited; leaving a mixed
//! field empty changes nothing.
//!
//! Own design.

use std::sync::Arc;

use egui::{Align2, Color32, Id, Rect, Stroke, pos2, vec2};
use lcc_catalog::Photo;
use serde_json::{Value, json};

use crate::LccApp;
use crate::modern_dock::glass;
use crate::panels::metadata::{MIXED, common};
use crate::theme::Tokens;
use crate::widgets::register;

const CARD_W: f32 = 300.0;
const MARGIN: f32 = 12.0;

/// The Library's floating extras over `area` (the grid): the search pill, and the Info card when on.
pub(crate) fn show(app: &mut LccApp, ui: &egui::Ui, area: Rect) {
    // on a phone the Info card is a sheet along the bottom, where the search pill sits
    let sheet = phone(area);
    if !(sheet && app.ui.modern.info) {
        search(app, ui, area);
    }
    if app.ui.modern.info {
        info_card(app, ui, area);
    }
}

/// A phone-sized window (the Info card is a bottom sheet there).
fn phone(area: Rect) -> bool {
    area.width() < 600.0
}

// ------------------------------------------------------------------------ search

fn search(app: &mut LccApp, ui: &egui::Ui, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let w = (area.width() - 2.0 * MARGIN).min(440.0);
    if w < 160.0 {
        return;
    }
    let h = 36.0;
    let min = pos2(area.center().x - w / 2.0, area.bottom() - MARGIN - h);
    egui::Area::new(Id::new("modern-search")).fixed_pos(min).order(egui::Order::Middle).show(ui.ctx(), |ui| {
        let (bg, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
        glass(ui.painter(), bg, h / 2.0, &t);
        register(ui.ctx(), "modern:search", bg);
        crate::icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(bg.left() + 18.0, bg.center().y), egui::Vec2::splat(16.0)),
            crate::icons::Icon::Search,
            t.text_dim,
        );
        // the chips at the right: ★3+ and Picks
        let stars = app.session.filter.rating >= 3 && app.session.filter.rating_op == lcc_catalog::query::RatingOp::AtLeast;
        let picks = app.session.filter.flag == Some(lcc_catalog::Flag::Pick);
        let mut x = bg.right() - 4.0;
        let mut chip = |ui: &mut egui::Ui, id: &str, label: &str, on: bool| -> bool {
            let g = ui.painter().layout_no_wrap(crate::i18n::tr(label).to_string(), t.semibold(12.0), t.text);
            let cw = g.size().x + 20.0;
            x -= cw;
            let r = Rect::from_min_size(pos2(x, bg.top() + 4.0), vec2(cw, h - 8.0));
            x -= 4.0;
            let resp = ui.interact(r, ui.id().with(id), egui::Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, crate::i18n::tr(label)));
            register(ui.ctx(), format!("modern:search:{id}"), r);
            let fill = if on {
                t.accent
            } else if resp.hovered() {
                Color32::from_white_alpha(30)
            } else {
                Color32::from_white_alpha(16)
            };
            ui.painter().rect_filled(r, r.height() / 2.0, fill);
            ui.painter().galley_with_override_text_color(r.center() - g.size() / 2.0, g, if on { Color32::WHITE } else { t.text_label });
            resp.clicked()
        };
        let mut change: Option<Value> = None;
        if chip(ui, "picks", "Picks", picks) {
            change = Some(json!({"flag": if picks { Value::Null } else { json!("pick") }}));
        }
        if chip(ui, "stars", "★ 3+", stars) {
            change = Some(if stars { json!({"rating": 0}) } else { json!({"rating": 3, "ratingOp": "atLeast"}) });
        }
        // semantic search switched off (Settings ▸ AI Models): no AI chip, the field searches the text
        let semantic_on = app.session.semantic_search_on();
        if !semantic_on {
            app.ui.semantic_search = false;
        }
        let current = if app.ui.semantic_search { app.session.filter.semantic.clone().unwrap_or_default() } else { app.session.filter.text.clone() };
        if semantic_on && chip(ui, "ai", "AI", app.ui.semantic_search) {
            app.ui.semantic_search = !app.ui.semantic_search;
            if !current.trim().is_empty() {
                change = Some(if app.ui.semantic_search {
                    json!({"text": "", "semantic": current})
                } else {
                    json!({"semantic": Value::Null, "text": current})
                });
            }
        }
        // the text field between the icon and the chips
        let field = Rect::from_min_max(pos2(bg.left() + 32.0, bg.top() + 4.0), pos2(x - 2.0, bg.bottom() - 4.0));
        let mut text = current.clone();
        let hint = if app.ui.semantic_search { crate::i18n::tr("Describe photos…") } else { crate::i18n::tr("Search photos") };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let r = crate::text_field::TextField::singleline("modernSearch", &mut text)
            .hint(hint)
            .width(field.width().max(40.0))
            .frame(false)
            .show(&mut child);
        crate::access::label(&r.response, "Search photos");
        if text != current && change.is_none() {
            change = Some(if app.ui.semantic_search { json!({"semantic": text}) } else { json!({"text": text}) });
        }
        if let Some(p) = change
            && let Err(e) = app.run("library.filter", p)
        {
            app.toast_error(ui.ctx(), e);
        }
    });
}

// ------------------------------------------------------------------------ Info card

fn info_card(app: &mut LccApp, ui: &egui::Ui, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let sheet = phone(area);
    // a card at the right; on a phone a sheet along the bottom (its height last frame places it)
    let w = if sheet { area.width() } else { CARD_W.min(area.width() - 2.0 * MARGIN) };
    if w < 160.0 {
        return;
    }
    let (min, max_h) = if sheet {
        let max_h = (area.height() * 0.6).max(160.0);
        let last: f32 = ctx.data(|d| d.get_temp(Id::new("modern-info-h"))).unwrap_or(max_h);
        (pos2(area.left(), area.bottom() - last.min(max_h)), max_h)
    } else {
        let top = area.top() + MARGIN;
        (pos2(area.right() - MARGIN - w, top), (area.bottom() - MARGIN - 48.0 - top).max(120.0))
    };
    let photos: Vec<Arc<Photo>> = app.session.selected().iter().filter_map(|i| app.session.catalog.photo(*i).cloned()).collect();
    let ids: Vec<u64> = photos.iter().map(|p| p.id.0).collect();
    egui::Area::new(Id::new("modern-info")).fixed_pos(min).order(egui::Order::Middle).show(&ctx, |ui| {
        ui.set_max_height(max_h);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::same(12))
            .corner_radius(if sheet { egui::CornerRadius { nw: 20, ne: 20, sw: 0, se: 0 } } else { egui::CornerRadius::same(16) })
            .fill(t.chrome.gamma_multiply(if sheet { 0.97 } else { 0.92 }))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(28)));
        let shown = frame.show(ui, |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Info")).font(t.semibold(14.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // ✕: put it away (I, or the ⓘ button, does the same)
                    if crate::button::Button::icon_only(crate::icons::Icon::Close, "Close")
                        .kind(crate::button::Kind::Ghost)
                        .widget("modern:info:close")
                        .show(ui)
                        .clicked()
                    {
                        app.ui.modern.info = false;
                    }
                    let what = match photos.len() {
                        0 => String::new(),
                        1 => photos.first().map(|p| p.file_name.clone()).unwrap_or_default(),
                        n => crate::i18n::tr_format!("{n} photos", n = n),
                    };
                    ui.add(egui::Label::new(egui::RichText::new(what).size(11.5).color(t.text_dim)).truncate());
                });
            });
            if photos.is_empty() {
                ui.label(egui::RichText::new(crate::i18n::tr("Select photos to see their info.")).color(t.text_dim));
                return;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("modern-info-scroll").max_height(max_h - 60.0).show(ui, |ui| {
                ui.set_width(w - 24.0);
                crate::panels::metadata::rating(app, ui, &photos);
                flag_row(app, ui, &photos);
                crate::panels::metadata::label(app, ui, &photos);
                for (key, label) in
                    [("title", "Title"), ("caption", "Caption"), ("keywords", "Keywords"), ("creator", "Creator"), ("copyright", "Copyright")]
                {
                    field(app, ui, &photos, &ids, key, label);
                }
                ui.add_space(4.0);
                ui.separator();
                for (key, label) in [
                    ("camera", "Camera"),
                    ("lens", "Lens"),
                    ("exposure", "Exposure"),
                    ("focal", "Focal Length"),
                    ("iso", "ISO"),
                    ("captured", "Captured"),
                    ("dimensions", "Dimensions"),
                ] {
                    read_only(ui, &photos, key, label);
                }
            });
        });
        register(&ctx, "modern:info", shown.response.rect);
        ctx.data_mut(|d| d.insert_temp(Id::new("modern-info-h"), shown.response.rect.height()));
    });
}

/// Pick / reject / none for the selection.
fn flag_row(app: &mut LccApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    use lcc_catalog::Flag;
    let first = photos.first().map(|p| p.flag);
    let shared = first.filter(|f| photos.iter().all(|p| p.flag == *f));
    let options = [(Flag::Pick, "Pick", "pick"), (Flag::Reject, "Reject", "reject"), (Flag::None, "Unflagged", "none")];
    let labels: Vec<&str> = options.iter().map(|(_, l, _)| *l).collect();
    let chosen = shared.and_then(|f| options.iter().position(|(o, _, _)| *o == f));
    if let Some(i) = crate::widgets::segments(ui, "modernInfo:flag", &labels, chosen)
        && let Some((_, _, key)) = options.get(i)
        && let Err(e) = app.run("photo.flag", json!({"flag": key, "ids": photos.iter().map(|p| p.id.0).collect::<Vec<_>>()}))
    {
        app.toast_error(ui.ctx(), e);
    }
    ui.add_space(4.0);
}

/// An editable field: `< mixed >` when the selection differs, written to every selected photo when
/// an edit ends (and only then, and not when a mixed field is left empty). Keywords are a comma list.
fn field(app: &mut LccApp, ui: &mut egui::Ui, photos: &[Arc<Photo>], ids: &[u64], key: &str, label: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim));
    let shared = common(photos, key);
    let shown = shared.clone().unwrap_or_default();
    let state = Id::new(("modern-info-field", key));
    let mut text: String = ui.data(|d| d.get_temp(state)).unwrap_or_else(|| shown.clone());
    let widget = format!("modernInfo:{key}");
    let f = crate::text_field::TextField::singleline(&widget, &mut text).width(f32::INFINITY);
    let f = if shared.is_none() { f.hint(MIXED) } else { f };
    let r = f.show(ui);
    crate::access::label(&r.response, label);
    // the photos an edit saves to are the ones selected when it began
    let targets_id = state.with("targets");
    let targets: Vec<u64> = ui.data(|d| d.get_temp(targets_id)).unwrap_or_else(|| ids.to_vec());
    if r.editing {
        ui.data_mut(|d| {
            d.insert_temp(state, text.clone());
            d.insert_temp(targets_id, targets.clone());
        });
    } else {
        ui.data_mut(|d| {
            d.remove::<String>(state);
            d.remove::<Vec<u64>>(targets_id);
        });
    }
    if r.committed() && text != shown && !(shared.is_none() && text.trim().is_empty()) {
        let value = if key == "keywords" { json!(text.split(',').map(str::trim).filter(|k| !k.is_empty()).collect::<Vec<_>>()) } else { json!(text) };
        if let Err(e) = app.run("photo.setMeta", json!({"ids": targets, key: value})) {
            app.toast_error(ui.ctx(), e);
        }
    }
    ui.add_space(2.0);
}

fn read_only(ui: &mut egui::Ui, photos: &[Arc<Photo>], key: &str, label: &str) {
    let t = Tokens::get(ui.ctx());
    let v = common(photos, key);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(96.0, 18.0), egui::Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(11.5), t.text_dim);
        let (text, color) = match v.as_deref() {
            None => (MIXED.to_string(), t.text_dim),
            Some("") => (String::new(), t.text_dim),
            // a capture time of a year or a month alone shows as such ("1983", "April 1983")
            Some(s) if key == "captured" && lcc_catalog::dates::partial_capture(s).is_some() => (crate::i18n::display_time(s), t.text_label),
            Some(s) => (s.to_string(), t.text_label),
        };
        ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(color)).truncate());
    });
}

/// The Library extras' commands (`menus_fork` lists them).
pub const COMMANDS: &[crate::menus::UiCommand] = &[("modern.info", "Show Info Card", None, "")];

/// `modern.info {on?}` shows or hides the Info card; `None`: not one of these.
pub(crate) fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        "modern.info" => {
            app.ui.modern.info = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.modern.info);
            Some(Ok(json!({"on": app.ui.modern.info})))
        }
        _ => None,
    }
}
