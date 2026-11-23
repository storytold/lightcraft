//! The library filter bar above the grid: rating (≥ / = / ≤), flags, colour labels, kind,
//! edited state and camera / lens / keyword pickers — every control drives `library.filter`.
//! "Save as Smart Album…" keeps the current view as a live album; "Clear" resets the filter.

#[cfg(test)]
#[path = "filterbar_actions_external155_tests.rs"]
mod actions_external155_tests;
#[cfg(test)]
mod sdk_tests;
#[cfg(test)]
mod tests;

use craft::ui::buttons::Button;
use craft::ui::contract::overlay::Trigger;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::menu::{Menu, MenuItem};
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
        t.font(12.5),
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

/// The legacy control is an action menu, not a value-editing field. Keep its non-focus-taking
/// native opener, with the SDK owning the actual popup, checked rows, navigation and dismissal.
fn kind_picker(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let choices = [
        ("all", "All types"),
        ("image", "Photos"),
        ("raw", "Raw"),
        ("video", "Videos"),
        ("hdr", "HDR"),
        ("panorama", "Panoramas"),
        ("hdrPanorama", "HDR Panoramas"),
    ];
    let f = &app.session.filter;
    let current = f.merged.clone().unwrap_or_else(|| {
        match f.kind {
            None => "all",
            Some(MediaKind::Image) => "image",
            Some(MediaKind::Raw) => "raw",
            Some(MediaKind::Video) => "video",
        }
        .to_string()
    });
    let shown = choices.iter().find(|(value, _)| *value == current).map_or("Merged", |(_, label)| *label);
    let active = f.kind.is_some() || f.merged.is_some();
    let t = Tokens::get(ui.ctx());
    // Applying a filter inserts chips before this bar, shifting egui's automatic IDs. A
    // keyboard opener must keep its identity across that layout change to receive focus back.
    let opener = ui.scope_builder(
        egui::UiBuilder::new().scope_id(egui::Id::unique("lightcraft-filter-kind-opener")),
        |ui| crate::widgets::dropdown(ui, "filter-kind", crate::i18n::tr(shown), t.font(12.5), if active { t.text } else { t.text_label }),
    ).inner;
    if active {
        ui.painter().rect_stroke(opener.rect.expand(2.0), 4.0, Stroke::new(1.0, t.accent.gamma_multiply(0.7)), StrokeKind::Outside);
    }
    let measuring = craft::ui::runtime::measure::active(ui.ctx());
    let pass = ui.ctx().cumulative_pass_nr();
    let keyboard_id = opener.id.with("keyboard-menu");
    let mut keyboard_open = ui.ctx().data(|data| data.get_temp::<(u64, bool)>(keyboard_id))
        .filter(|(last, _)| *last == pass || last.saturating_add(1) == pass)
        .is_some_and(|(_, open)| open);
    // External SDK click triggers only see pointer presses. Bridge activation only while
    // this native opener owns focus, leaving all other keys and controls alone.
    if !measuring && ui.is_enabled() && opener.has_focus()
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            || input.consume_key(egui::Modifiers::NONE, egui::Key::Space))
    {
        keyboard_open = true;
    }
    let keyboard_session = keyboard_open;
    let mut chosen = None;
    let mut menu_focus = None;
    let menu = Menu::new(crate::i18n::tr("Type")).id_salt("filter-kind-menu").values().opened_by(&opener, Trigger::Click);
    let menu = if keyboard_session { menu.open(Bind::bound(&mut keyboard_open)) } else { menu };
    let shown = menu.show(ui, |ui, _| {
        for (value, label) in choices {
            let selected = value == current;
            let item = MenuItem::new(crate::i18n::tr(label)).id_salt(value).entry_checked(selected);
            let item = if selected { item.default_stop() } else { item };
            let out = item.show(ui);
            if !craft::ui::runtime::measure::active(ui.ctx()) {
                crate::widgets::register_response(format!("filter:kind-{value}"), out.response.rect, &out.response);
                if ui.memory(|memory| memory.has_focus(out.response.id)) {
                    menu_focus = Some(out.response.id);
                }
            }
            if out.ran {
                chosen = Some(value);
            }
        }
    });
    if !measuring {
        ui.ctx().data_mut(|data| {
            if keyboard_open {
                data.insert_temp(keyboard_id, (pass, true));
            } else {
                data.remove::<(u64, bool)>(keyboard_id);
            }
        });
    }
    if !measuring && shown.output.closed.is_some() {
        // The native opener has no SDK focus stop. With no stop to restore, the menu leaves
        // its disappearing row focused; that dead focus blocks the grid's shortcut pass.
        // Release only this menu's row, never a field focused by an outside click.
        if let Some(focused) = menu_focus {
            ui.memory_mut(|memory| memory.surrender_focus(focused));
        }
        if keyboard_session && matches!(shown.output.closed,
            Some(craft::ui::runtime::overlay::CloseReason::Activated
                | craft::ui::runtime::overlay::CloseReason::Escape
                | craft::ui::runtime::overlay::CloseReason::Enter))
        {
            opener.request_focus();
        }
    }
    if !craft::ui::runtime::measure::active(ui.ctx())
        && ui.is_enabled()
        && let Some(value) = chosen
    {
        let patch = match value {
            "all" => json!({"kind": null, "merged": null}),
            "image" | "raw" | "video" => json!({"kind": value, "merged": null}),
            "hdr" | "panorama" | "hdrPanorama" => json!({"kind": null, "merged": value}),
            _ => return,
        };
        filter(app, patch);
    }
}

/// SDK buttons own both pointer and keyboard activation; sizing must not publish targets or run actions.
fn filter_action(ui: &mut egui::Ui, widget: &str, label: &str, enabled: bool) -> bool {
    let enabled = enabled && ui.is_enabled();
    let shown = Button::new(crate::i18n::tr(label).to_owned())
        .id(egui::Id::unique(widget))
        .size(craft::theme::ControlSize::Sm)
        .enabled(enabled)
        .show(ui);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return false;
    }
    crate::widgets::register_response(widget, shown.response.rect, &shown.response);
    enabled && shown.clicked()
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // one row when there is room, else the metadata pickers and actions go to a second row
    let two_rows = ui.available_width() < WIDE;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), if two_rows { 2.0 * HEIGHT - 8.0 } else { HEIGHT }), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, t.chrome);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.divider));
    register(ui, "filterbar", bar);
    let f = app.session.filter.clone();
    let row = |ui: &mut egui::Ui, i: f32| {
        let r = Rect::from_min_size(bar.min + vec2(16.0, 4.0 + i * (HEIGHT - 8.0)), vec2(bar.width() - 32.0, HEIGHT - 8.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
        c.spacing_mut().item_spacing.x = 4.0;
        c
    };
    let mut row1 = row(ui, 0.0);
    let mut row2 = row(ui, 1.0);
    let ui = &mut row1;

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

    // flags: several can be on at once (any of them matches), as in Lightroom Classic
    caption(ui, "Flag");
    let chosen: Vec<Flag> = f.flags_shown().unwrap_or_default();
    for (id, icon, flag, tip) in [
        ("pick", Icon::FlagPick, Flag::Pick, "Picked"),
        ("reject", Icon::FlagReject, Flag::Reject, "Rejected"),
        ("none", Icon::Circle, Flag::None, "Unflagged"),
    ] {
        let on = chosen.contains(&flag);
        let color = match flag {
            Flag::Pick if on => t.pick,
            Flag::Reject if on => t.reject,
            _ => Color32::TRANSPARENT,
        };
        let icon = if on && flag == Flag::Pick { Icon::FlagPickFilled } else { icon };
        let resp = toggle(ui, id, on, tip, |p, r, c| paint(p, r, icon, if color == Color32::TRANSPARENT { c } else { color }));
        if resp.clicked() {
            // in the bar's order, so the chip reads the same whichever was clicked first
            let names: Vec<String> = [Flag::Pick, Flag::Reject, Flag::None]
                .into_iter()
                .filter(|x| if *x == flag { !on } else { chosen.contains(x) })
                .map(|x| format!("{x:?}").to_lowercase())
                .collect();
            filter(app, json!({"flag": Value::Null, "flags": names}));
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

    // The media-kind flow uses the SDK; all other filter controls keep their existing paths.
    kind_picker(app, ui);
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

    // metadata pickers
    let ui: &mut egui::Ui = if two_rows { &mut row2 } else { &mut row1 };
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
    if filter_action(ui, "button:filterSave", "Save as Smart Album…", filtering) {
        let _ = app.run("dialog.newSmartAlbum", json!({}));
    }
    if filter_action(ui, "button:filterClear", "Clear", filtering) {
        app.ui.search.clear();
        let _ = app.run("library.clearFilter", json!({}));
    }
    // filter presets: apply one, save the current filter, delete
    let presets = crate::widgets::dropdown(ui, "filterPresets", crate::i18n::tr("Presets"), t.font(12.5), t.text);
    register(ui, "button:filterPresets", presets.rect);
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
