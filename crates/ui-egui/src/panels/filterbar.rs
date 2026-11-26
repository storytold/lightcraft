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

use craft::ui::buttons::{Button, ToggleButton};
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
use craft::ui::controls::disclosure::DisclosureGlyph;
use craft::ui::display::{Shade, Text};
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::primitives::IconSpec;
use craft::ui::style::TypeRole;
use egui::{Color32, Rect, Sense, Stroke, vec2};
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

/// Whether this pass draws nothing the user can reach: a sizing or measuring run publishes no
/// automation names and runs no command.
fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

/// A small label before a group of controls.
fn caption(ui: &mut egui::Ui, text: &str) {
    Text::new(crate::i18n::tr(text).to_owned()).role(TypeRole::Caption).shade(Shade::Muted).show(ui);
}

/// What a filter toggle shows: one of the app's icons (in its own ink when `tint` is not
/// transparent, else the framework button's), or a colour-label swatch.
#[derive(craft::ui::contract::MemoProps)]
enum FilterGlyph {
    Icon { icon: Icon, tint: Color32 },
    Swatch(Color32),
}

impl craft::ui::primitives::Glyph for FilterGlyph {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, color: Color32, _: bool) {
        match self {
            FilterGlyph::Icon { icon, tint } => {
                let ink = if *tint == Color32::TRANSPARENT { color } else { *tint };
                paint(painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), *icon, ink);
            }
            FilterGlyph::Swatch(swatch) => {
                painter.circle_filled(rect.center(), 5.5, *swatch);
            }
        }
    }
}

/// A square toggle with an icon or a colour swatch: the framework's icon-only toggle button,
/// published as `filter:<id>`. True when this pass switched it.
fn toggle(ui: &mut egui::Ui, id: &str, on: bool, tip: &str, glyph: FilterGlyph) -> bool {
    let label = crate::i18n::tr(tip).to_owned();
    let out = ToggleButton::new(label.clone(), Bind::given(on))
        .id(egui::Id::unique(("filter-toggle", id)))
        .icon(IconSpec::glyph(&glyph))
        .icon_only()
        .tooltip(label)
        .show(ui);
    if measuring(ui) {
        return false;
    }
    crate::widgets::register_response(format!("filter:{id}"), out.response.rect, &out.response);
    ui.is_enabled() && out.events.committed().is_some()
}

/// A dropdown face: the framework push button showing `text` and the menu chevron, lit while its
/// filter `active`, published as `dropdown:<id>`. The caller hangs a framework menu from the
/// returned response.
fn dropdown_face(ui: &mut egui::Ui, id: &str, text: &str, active: bool) -> egui::Response {
    let shown = Button::new(text.to_owned())
        .id(egui::Id::unique(("filter-dropdown", id)))
        .size(craft::theme::ControlSize::Sm)
        .variant(craft::theme::Variant::GHOST)
        .selected(active)
        .trailing_icon(DisclosureGlyph::dropdown())
        .show(ui);
    if !measuring(ui) {
        crate::widgets::register_response(format!("dropdown:{id}"), shown.response.rect, &shown.response);
    }
    shown.response
}

/// Before a dropdown's menu is shown: remember that a press on its opener (rather than a key) opens it.
fn note_opening(ui: &egui::Ui, opener: &egui::Response) {
    if ui.input(|i| i.pointer.any_pressed()) && opener.contains_pointer() {
        ui.data_mut(|d| d.insert_temp(opener.id.with("lc-menu-by-pointer"), true));
    }
}

/// When a dropdown's menu closes: one the pointer opened gives the keyboard back to the grid, so its shortcuts
/// keep working, even when the framework returns the focus to the opener a few frames after the close; one a key
/// opened leaves the focus on its opener, where the framework returns it.
fn menu_closed(ui: &egui::Ui, opener: &egui::Response, closed: bool) {
    let by_pointer = opener.id.with("lc-menu-by-pointer");
    let release = opener.id.with("lc-menu-release");
    let pass = ui.ctx().cumulative_pass_nr();
    if closed {
        if ui.data(|d| d.get_temp::<bool>(by_pointer)).unwrap_or(false) {
            ui.data_mut(|d| d.insert_temp(release, pass));
        }
        ui.data_mut(|d| d.remove::<bool>(by_pointer));
    }
    let Some(since) = ui.data(|d| d.get_temp::<u64>(release)) else { return };
    if let Some(rec) = craft::ui::runtime::focus::focused_rec(ui.ctx()).filter(|rec| rec.stop.id == opener.id) {
        ui.memory_mut(|m| m.surrender_focus(rec.stop.root));
        ui.data_mut(|d| d.remove::<u64>(release));
    } else if pass.saturating_sub(since) > 60 {
        ui.data_mut(|d| d.remove::<u64>(release));
    }
}

/// One row of a value menu: radio-marked while `checked`, the lead when the menu opens. `caption` is
/// shown as it is (it may be a user's keyword or camera name). True when the row ran.
fn value_row(ui: &mut egui::Ui, salt: impl egui::AsIdSalt, caption: &str, checked: bool) -> bool {
    let row = MenuItem::new(caption.replace('&', "&&")).id_salt(salt).entry_checked(checked);
    let row = if checked { row.default_stop() } else { row };
    let out = row.show(ui);
    !measuring(ui) && out.ran
}

/// A dropdown showing `current`; `items` are (label, filter patch, selected).
fn picker(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, current: &str, active: bool, items: Vec<(String, Value, bool)>) {
    let user_names = matches!(id, "camera" | "lens" | "keyword");
    let shown = if active && user_names { current.to_owned() } else { crate::i18n::tr(current).to_owned() };
    let face = dropdown_face(ui, &format!("filter-{id}"), &shown, active);
    let mut chosen = None;
    // A measuring or sizing run shows no layer: its rows would claim the menu's ids a second time in the pass.
    if measuring(ui) {
        return;
    }
    note_opening(ui, &face);
    let menu = Menu::new(shown).id_salt(("filter-menu", id)).values().opened_by(&face, Trigger::Click).show(ui, |ui, _| {
        for (label, patch, sel) in &items {
            // a camera, lens or keyword the library names is data; the "Any …" row is a built-in label
            let literal = user_names && patch.get(id).is_some_and(|value| !value.is_null());
            let caption = if literal { label.clone() } else { crate::i18n::tr(label).to_owned() };
            if value_row(ui, (id, label.as_str()), &caption, *sel) {
                chosen = Some(patch.clone());
            }
        }
    });
    menu_closed(ui, &face, menu.output.closed.is_some());
    if let Some(p) = chosen {
        filter(app, p);
    }
}

/// The media-kind dropdown: a framework menu of radio rows from the framework button that opens it.
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
    let opener = dropdown_face(ui, "filter-kind", crate::i18n::tr(shown), active);
    if measuring(ui) {
        return;
    }
    let mut chosen = None;
    note_opening(ui, &opener);
    let menu = Menu::new(crate::i18n::tr("Type")).id_salt("filter-kind-menu").values().opened_by(&opener, Trigger::Click).show(ui, |ui, _| {
        for (value, label) in choices {
            let item = MenuItem::new(crate::i18n::tr(label).replace('&', "&&")).id_salt(value).entry_checked(value == current);
            let item = if value == current { item.default_stop() } else { item };
            let out = item.show(ui);
            if !measuring(ui) {
                crate::widgets::register_response(format!("filter:kind-{value}"), out.response.rect, &out.response);
                if out.ran {
                    chosen = Some(value);
                }
            }
        }
    });
    menu_closed(ui, &opener, menu.output.closed.is_some());
    if !measuring(ui)
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

/// Framework buttons own both pointer and keyboard activation; sizing must not publish targets or run actions.
fn filter_action(ui: &mut egui::Ui, widget: &str, label: &str, enabled: bool) -> bool {
    let enabled = enabled && ui.is_enabled();
    let shown =
        Button::new(crate::i18n::tr(label).to_owned()).id(egui::Id::unique(widget)).size(craft::theme::ControlSize::Sm).enabled(enabled).show(ui);
    if measuring(ui) {
        return false;
    }
    crate::widgets::register_response(widget, shown.response.rect, &shown.response);
    enabled && shown.clicked()
}

/// What the filter presets menu asked for.
enum PresetAction {
    Apply(String),
    Delete(String),
    Save,
}

/// The saved filters: apply one (right-click deletes it) or save the current filter.
fn presets_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, filtering: bool) {
    let face = dropdown_face(ui, "filterPresets", crate::i18n::tr("Presets"), false);
    if !measuring(ui) {
        register(ui, "button:filterPresets", face.rect);
    }
    if measuring(ui) {
        return;
    }
    let names: Vec<String> = app.session.filter_presets.iter().map(|f| f.name.clone()).collect();
    let mut action = None;
    note_opening(ui, &face);
    let menu = Menu::new(crate::i18n::tr("Presets")).id_salt("filter-presets-menu").opened_by(&face, Trigger::Click).show(ui, |ui, _| {
        // a preset's name is the user's: shown as written, never read for a mnemonic marker
        for name in &names {
            let row = MenuItem::new(name.replace('&', "&&")).id_salt(("filter-preset", name.as_str())).show(ui);
            if !measuring(ui) && row.ran {
                action = Some(PresetAction::Apply(name.clone()));
            }
            Menu::context(crate::i18n::tr("Filter preset"), &row.response).id_salt(("filter-preset-context", name.as_str())).show(ui, |ui, _| {
                let delete = MenuItem::new(crate::i18n::tr("Delete Preset")).id_salt("delete").show(ui);
                if !measuring(ui) && delete.ran {
                    action = Some(PresetAction::Delete(name.clone()));
                }
            });
        }
        if names.is_empty() {
            MenuItem::heading(crate::i18n::tr("No filter presets yet")).show(ui);
        }
        craft::ui::display::Divider::horizontal().show(ui);
        let save = MenuItem::new(crate::i18n::tr("Save Current Filter as Preset…")).id_salt("save").enabled(filtering).show(ui);
        if !measuring(ui) && filtering && save.ran {
            action = Some(PresetAction::Save);
        }
    });
    menu_closed(ui, &face, menu.output.closed.is_some());
    match action {
        Some(PresetAction::Apply(name)) => {
            let _ = app.run("filter.applyPreset", json!({"name": name}));
        }
        Some(PresetAction::Delete(name)) => {
            let _ = app.run("filter.deletePreset", json!({"name": name}));
        }
        Some(PresetAction::Save) => {
            crate::panels::dialogs::prompt(app, "Save Filter Preset", "Preset name", "", "filter.savePreset", json!({}), "name");
        }
        None => {}
    }
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
        if toggle(ui, id, on, tip, FilterGlyph::Icon { icon, tint: color }) {
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
        if toggle(ui, &format!("label-{name}"), on, &tip, FilterGlyph::Swatch(label_color(l))) {
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
    presets_menu(app, ui, filtering);
}
