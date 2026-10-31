//! The library filter bar above the grid: rating (≥ / = / ≤), flags, colour labels, kind,
//! edited state and camera / lens / keyword pickers — every control drives `library.filter`.
//! "Save as Smart Album…" keeps the current view as a live album; "Clear" resets the filter.

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, vec2};
use lightcraft_catalog::{ColorLabel, Flag, MediaKind, RatingOp};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

pub const HEIGHT: f32 = 40.0;
/// Bar width (points) below which the controls take two rows.
const WIDE: f32 = 1240.0;
/// Between two controls of a row.
const GAP: f32 = 4.0;

/// Colour-label swatches (UI colours, our own choice).
pub use crate::theme::label_color;

fn filter(app: &mut LightcraftApp, patch: Value) {
    let _ = app.run("library.filter", patch);
}

/// A small label before a group of controls.
fn caption(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.font(12.0)).color(t.text_dim));
}

/// A square toggle with an icon or a colour swatch.
fn toggle(ui: &mut egui::Ui, id: &str, on: bool, tip: &str, draw: impl FnOnce(&egui::Painter, Rect, Color32)) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    crate::widgets::announce_selected(&resp, crate::i18n::tr(tip), on);
    register(ui, format!("filter:{id}"), r);
    let p = ui.painter();
    if on {
        p.rect_filled(r, 5.0, t.tool_active);
        p.rect_stroke(r, 5.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(r, 5.0, t.hover);
    }
    draw(
        p,
        r.shrink(5.0),
        if on {
            t.text
        } else if resp.hovered() {
            t.text_label
        } else {
            t.icon
        },
    );
    resp.on_hover_text(crate::i18n::tr(tip))
}

/// A dropdown showing `current`; `items` are (label, filter patch, selected).
fn picker(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, current: &str, active: bool, items: Vec<(String, Value, bool)>) {
    let t = Tokens::get(ui.ctx());
    let r = crate::widgets::dropdown(
        ui,
        &format!("filter-{id}"),
        if active && matches!(id, "camera" | "lens" | "keyword") { current } else { crate::i18n::tr(current) },
        t.label(),
        if active { t.text } else { t.text_label },
    );
    if active {
        ui.painter().rect_stroke(r.rect.expand(2.0), 4.0, Stroke::new(1.0, t.accent.gamma_multiply(0.7)), StrokeKind::Outside);
    }
    let mut chosen = None;
    egui::Popup::menu(&r).show(|ui| {
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (label, patch, sel) in &items {
                if ui
                    .selectable_label(
                        *sel,
                        if matches!(id, "camera" | "lens" | "keyword") && patch.get(id).is_some_and(|value| !value.is_null()) {
                            label.as_str()
                        } else {
                            crate::i18n::tr(label)
                        },
                    )
                    .clicked()
                {
                    chosen = Some(patch.clone());
                }
            }
        });
    });
    if let Some(p) = chosen {
        filter(app, p);
    }
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // How wide the three parts of the bar were when last drawn (rating, flag and label; type and
    // edited; the metadata pickers and the buttons): their words are longer in some languages,
    // and what a row holds follows what fits, not a width that suits English.
    let widths_id = egui::Id::unique("filterbar:widths");
    let seen: [f32; 3] = ui.data(|d| d.get_temp(widths_id)).unwrap_or([0.0; 3]);
    let room = ui.available_width() - 32.0;
    // One row when there is room, else the metadata pickers and actions go to a second row. Type
    // and edited go down with them when they would run off the first, and stand on a row of
    // their own between the two when they fit on neither.
    // (Each part is measured from where it begins, so it measures the same on whichever row it
    // stands, and parts that share a row have a gap between them: what fits is then a matter of
    // the widths alone, and the bar comes to rest. Measured with the gap before it, a part was
    // four points wider when it followed another, and in that band the bar changed its mind
    // every frame.)
    let [first, second, third] = seen;
    let two_rows = ui.available_width() < WIDE || first + GAP + second + GAP + third > room;
    let (second_row, third_row) = match (two_rows, first + GAP + second <= room, second + GAP + third <= room) {
        (false, _, _) => (0, 0),
        (true, true, _) => (0, 1),
        (true, false, true) => (1, 1),
        (true, false, false) => (1, 2),
    };
    let rows = third_row as f32 + 1.0;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), rows * (HEIGHT - 8.0) + 8.0), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, t.chrome);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.divider));
    register(ui, "filterbar", bar);
    let f = app.session.filter.clone();
    let row = |ui: &mut egui::Ui, i: f32| {
        let r = Rect::from_min_size(bar.min + vec2(16.0, 4.0 + i * (HEIGHT - 8.0)), vec2(bar.width() - 32.0, HEIGHT - 8.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
        c.spacing_mut().item_spacing.x = GAP;
        c
    };
    let mut rows = [row(ui, 0.0), row(ui, 1.0), row(ui, 2.0)];
    let [ui, ..] = &mut rows;
    let began = ui.cursor().left();

    // rating: operator + stars
    caption(ui, "Rating");
    let op_label = match f.rating_op {
        RatingOp::AtLeast => "≥",
        RatingOp::Exactly => "=",
        RatingOp::AtMost => "≤",
    };
    picker(
        app,
        ui,
        "ratingOp",
        op_label,
        false,
        [("≥  at least", "atLeast", RatingOp::AtLeast), ("=  exactly", "exactly", RatingOp::Exactly), ("≤  at most", "atMost", RatingOp::AtMost)]
            .into_iter()
            .map(|(l, k, op)| (l.to_string(), json!({"ratingOp": k}), f.rating_op == op))
            .collect(),
    );
    if let Some(r) = crate::rating::Rating::new("filter", f.rating).automation_ids("filter:star").show(ui) {
        filter(app, json!({"rating": r}));
    }
    ui.add_space(14.0);

    // flags (one at a time; click again to clear)
    caption(ui, "Flag");
    for (id, icon, flag, tip) in [
        ("pick", Icon::FlagPick, Flag::Pick, "Picked"),
        ("reject", Icon::FlagReject, Flag::Reject, "Rejected"),
        ("none", Icon::Circle, Flag::None, "Unflagged"),
    ] {
        let on = f.flag == Some(flag);
        let color = match flag {
            Flag::Pick if on => t.pick,
            Flag::Reject if on => t.reject,
            _ => Color32::TRANSPARENT,
        };
        let icon = if on && flag == Flag::Pick { Icon::FlagPickFilled } else { icon };
        let resp = toggle(ui, id, on, tip, |p, r, c| paint(p, r, icon, if color == Color32::TRANSPARENT { c } else { color }));
        if resp.clicked() {
            filter(app, json!({"flag": if on { Value::Null } else { json!(id) }}));
        }
    }
    ui.add_space(14.0);

    // colour labels
    caption(ui, "Label");
    // several labels can be on at once (any of them matches); lit are those `label` and `labels` both let through
    let chosen: Vec<ColorLabel> = f.labels_shown().unwrap_or_default();
    for l in ColorLabel::ALL {
        let on = chosen.contains(&l);
        let name = format!("{l:?}").to_lowercase();
        let tip = crate::i18n::tr_format!("{} label (click more labels to show any of them)", crate::i18n::color_label(&app.session.catalog, l));
        let resp = toggle(ui, &format!("label-{name}"), on, &tip, |p, r, _| {
            p.circle_filled(r.center(), 5.5, label_color(l));
        });
        if resp.clicked() {
            let mut next = chosen.clone();
            if on {
                next.retain(|x| *x != l);
            } else {
                next.push(l);
            }
            next.sort();
            let names: Vec<String> = next.iter().map(|x| format!("{x:?}").to_lowercase()).collect();
            filter(app, json!({"label": Value::Null, "labels": names}));
        }
    }
    ui.add_space(14.0);
    let first_w = ui.min_rect().right() - began;

    // kind + edited
    let Some(ui) = rows.get_mut(second_row) else { return };
    let began = ui.cursor().left();
    let kind_label = match f.kind {
        None => "All types",
        Some(MediaKind::Image) => "Photos",
        Some(MediaKind::Raw) => "Raw",
        Some(MediaKind::Video) => "Videos",
    };
    let kind_label = match f.merged.as_deref() {
        Some("hdr") => "HDR",
        Some("panorama") => "Panoramas",
        Some("hdrPanorama") => "HDR Panoramas",
        Some(_) => "Merged",
        None => kind_label,
    };
    picker(
        app,
        ui,
        "kind",
        kind_label,
        f.kind.is_some() || f.merged.is_some(),
        [
            ("All types", Value::Null, None),
            ("Photos", json!("image"), Some(MediaKind::Image)),
            ("Raw", json!("raw"), Some(MediaKind::Raw)),
            ("Videos", json!("video"), Some(MediaKind::Video)),
        ]
        .into_iter()
        .map(|(l, v, k)| (l.to_string(), json!({"kind": v, "merged": null}), f.kind == k && f.merged.is_none()))
        .chain(
            [("HDR", "hdr"), ("Panoramas", "panorama"), ("HDR Panoramas", "hdrPanorama")]
                .into_iter()
                .map(|(l, m)| (l.to_string(), json!({"kind": null, "merged": m}), f.merged.as_deref() == Some(m))),
        )
        .collect(),
    );
    let edited_label = match f.edited {
        None => "Edited or not",
        Some(true) => "Edited",
        Some(false) => "Unedited",
    };
    picker(
        app,
        ui,
        "edited",
        edited_label,
        f.edited.is_some(),
        [("Edited or not", Value::Null, None), ("Edited", json!(true), Some(true)), ("Unedited", json!(false), Some(false))]
            .into_iter()
            .map(|(l, v, e)| (l.to_string(), json!({"edited": v}), f.edited == e))
            .collect(),
    );

    let second_w = ui.min_rect().right() - began;

    // metadata pickers
    let Some(ui) = rows.get_mut(third_row) else { return };
    let began = ui.cursor().left();
    let values = app.caches.filter_values(&app.session.catalog);
    let (cameras, lenses, keywords) = (values.cameras.clone(), values.lenses.clone(), values.keywords.clone());
    for (id, key, all_label, current, values) in [
        ("camera", "camera", "Camera", f.camera.clone(), cameras),
        ("lens", "lens", "Lens", f.lens.clone(), lenses),
        ("keyword", "keyword", "Keyword", f.keyword.clone(), keywords),
    ] {
        let mut items = vec![(format!("Any {}", all_label.to_lowercase()), json!({key: Value::Null}), current.is_none())];
        items.extend(values.into_iter().map(|v| (v.clone(), json!({key: v.clone()}), current.as_deref() == Some(v.as_str()))));
        let shown = current.clone().unwrap_or_else(|| all_label.to_string());
        let shown = if shown.chars().count() > 22 { format!("{}…", shown.chars().take(21).collect::<String>()) } else { shown };
        picker(app, ui, id, &shown, current.is_some(), items);
    }

    // save / clear
    ui.add_space(14.0);
    let filtering = app.session.filter != Default::default();
    let save = ui.add_enabled(filtering, egui::Button::new(egui::RichText::new(crate::i18n::tr("Save as Smart Album…")).font(t.label())));
    register(ui, "button:filterSave", save.rect);
    if save.clicked() {
        let _ = app.run("dialog.newSmartAlbum", json!({}));
    }
    let clear = ui.add_enabled(filtering, egui::Button::new(egui::RichText::new(crate::i18n::tr("Clear")).font(t.label())));
    register(ui, "button:filterClear", clear.rect);
    if clear.clicked() {
        app.ui.search.clear();
        let _ = app.run("library.clearFilter", json!({}));
    }
    // filter presets: apply one, save the current filter, delete
    let presets = crate::widgets::dropdown(ui, "filterPresets", crate::i18n::tr("Presets"), t.label(), t.text);
    register(ui, "button:filterPresets", presets.rect);
    let now = [first_w, second_w, ui.min_rect().right() - began];
    // (a number that is none would never equal itself: it is not kept, nor asked about again)
    if now.iter().all(|w| w.is_finite()) && now.iter().zip(seen).any(|(now, seen)| (now - seen).abs() > 0.5) {
        ui.data_mut(|d| d.insert_temp(widths_id, now));
        ui.ctx().request_repaint();
    }
    egui::Popup::menu(&presets).show(|ui| {
        let names: Vec<String> = app.session.filter_presets.iter().map(|f| f.name.clone()).collect();
        for n in &names {
            let r = ui.button(n);
            if r.clicked() {
                let _ = app.run("filter.applyPreset", json!({"name": n}));
            }
            crate::widgets::context_menu(&r, |ui| {
                if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                    let _ = app.run("filter.deletePreset", json!({"name": n}));
                }
            });
        }
        if names.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No filter presets yet")).color(t.text_dim));
        }
        ui.separator();
        if ui.add_enabled(filtering, egui::Button::new(crate::i18n::tr("Save Current Filter as Preset…"))).clicked() {
            crate::panels::dialogs::prompt(app, "Save Filter Preset", "Preset name", "", "filter.savePreset", json!({}), "name");
        }
    });
}
