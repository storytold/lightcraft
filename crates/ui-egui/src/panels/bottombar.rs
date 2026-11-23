//! The bottom bar: view modes, sort, rating/flags pill, copy/paste settings, zoom and view toggles.

use craft::ui::buttons::Button;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
use craft::ui::controls::disclosure::DisclosureGlyph;
use craft::ui::display::Divider;
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::value_drag::Slider;
use egui::{Rect, pos2, vec2};
use lightcraft_catalog::Flag;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::{ViewMode, Zoom};
use crate::theme::Tokens;
use crate::widgets::icon_button;

const VIEW_MODES: &[(&str, Icon, ViewMode, &str)] = &[
    ("photoGrid", Icon::GridPhoto, ViewMode::PhotoGrid, "Photo Grid (G)"),
    ("squareGrid", Icon::GridSquare, ViewMode::SquareGrid, "Square Grid (G toggles)"),
    ("detail", Icon::Single, ViewMode::Detail, "Detail (D)"),
    ("compare", Icon::Compare, ViewMode::Compare, "Compare (Shift+C)"),
    ("survey", Icon::Survey, ViewMode::Survey, "Survey (N)"),
    ("people", Icon::Subject, ViewMode::People, "People"),
];

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("bottom_bar")
        .exact_size(t.bottom_bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            crate::widgets::announce_region(ui, egui::accesskit::Role::Toolbar, crate::i18n::tr("Bottom bar"));
            let full = ui.max_rect();
            let compact_zoom = full.width() < 360.0;
            let right_start = right_side(app, ui, full, compact_zoom);
            let left_rect = Rect::from_min_max(full.min, pos2((right_start - 12.0).max(full.left()), full.bottom()));
            let mut left_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
            let left_end = left_ui
                .horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if left_rect.width() >= 270.0 {
                        for &(id, icon, mode, tip) in VIEW_MODES {
                            if icon_button(ui, id, icon, vec2(32.0, 32.0), app.ui.view == mode, true, tip).clicked() {
                                let _ = app.run(&format!("view.{id}"), json!({}));
                            }
                        }
                        ui.add_space(10.0);
                        Divider::vertical().show(ui);
                        ui.add_space(10.0);
                        let sort = icon_button(ui, "sort", Icon::Sort, vec2(30.0, 30.0), false, true, "Sort");
                        let sort_enabled = ui.is_enabled();
                        sort_menu(app, ui, &sort.response, sort_enabled);
                    } else {
                        let current_icon = VIEW_MODES.iter().find(|(_, _, mode, _)| *mode == app.ui.view).map_or(Icon::More, |(_, icon, _, _)| *icon);
                        let more = icon_button(ui, "viewOptions", current_icon, vec2(40.0, 32.0), true, true, "View");
                        paint(
                            ui.painter(),
                            Rect::from_center_size(more.rect.right_bottom() - vec2(5.0, 5.0), vec2(7.0, 7.0)),
                            Icon::ChevronDown,
                            t.text,
                        );
                        let sort_enabled = ui.is_enabled();
                        view_menu(app, ui, &more.response, compact_zoom, sort_enabled);
                    }
                })
                .response
                .rect
                .right();
            centre(app, ui, full, left_end + 12.0, right_start - 12.0);
        });
}

/// Five 19-point stars, three 22-point flags, seven 3-point gaps, a 6-point group gap
/// and 10-point padding at both ends.
const PILL_W: f32 = 208.0;

/// The rating/flag pill and copy/paste settings, between `from` and `to` (the side groups): the
/// copy buttons go first when there is no room (they are in the Edit menu too), then the pill.
fn centre(app: &mut LightcraftApp, ui: &mut egui::Ui, full: Rect, from: f32, to: f32) {
    let t = Tokens::get(ui.ctx());
    let has_clip = app.session.clipboard.is_some();
    let label = crate::i18n::tr(if has_clip { "Paste Edit Settings" } else { "Copy Edit Settings" });
    let copy_width = (ui.painter().layout_no_wrap(label.to_string(), t.font(13.0), t.text_label).size().x + 24.0).max(136.0);
    let centre_width = PILL_W + 10.0 + copy_width + 4.0 + 30.0;
    let room = to - from;
    if room < PILL_W {
        return;
    }
    let with_copy = room >= centre_width;
    let w = if with_copy { centre_width } else { PILL_W };
    // where it sits with room to spare (slightly left of centre), kept between the side groups
    let left = (full.center().x - 60.0 - PILL_W / 2.0).clamp(from, to - w);
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned());
    let pill = Rect::from_min_size(pos2(left, full.center().y - 15.0), vec2(PILL_W, 30.0));
    ui.painter().rect_filled(pill, 15.0, t.canvas);
    let mut pill_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(pill.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    pill_ui.spacing_mut().item_spacing.x = 3.0;
    let rating = active.as_ref().map(|p| p.rating).unwrap_or(0);
    if let Some(r) = crate::rating::Rating::new("bottom", rating).show(&mut pill_ui) {
        let mut params = json!({"rating": r});
        crate::panels::compare::target_active(app, &mut params);
        let _ = app.run("photo.rate", params);
    }
    pill_ui.add_space(6.0);
    let flag = active.as_ref().map(|p| p.flag).unwrap_or_default();
    for (id, icon, f) in [("unflag", Icon::Circle, Flag::None), ("pick", Icon::FlagPick, Flag::Pick), ("reject", Icon::FlagReject, Flag::Reject)] {
        let generation = app.session.library_generation();
        let active = app.session.active();
        let selection = app.session.selected();
        let culling = crate::panels::compare::culling(app);
        let owner = egui::Id::unique(("lc-bottom-flag", id, generation, active, &selection, culling));
        // Keep the original allocation slot; the flag's sensing belongs to the framework button.
        let (_, rect) = pill_ui.allocate_space(vec2(22.0, 22.0));
        let real = !pill_ui.is_sizing_pass() && !craft::ui::runtime::measure::active(pill_ui.ctx());
        let enabled = pill_ui.is_enabled();
        let hovered = pill_ui.ctx().read_response(owner).is_some_and(|response| response.hovered());
        // lit while the photo carries the flag; the unflag circle never lights
        let on = flag == f && f != Flag::None;
        let glyph = FlagGlyph {
            // the Pick button shows the flag the photo carries: filled while it is a pick
            icon: if on && f == Flag::Pick { Icon::FlagPickFilled } else { icon },
            ink: if on {
                if f == Flag::Reject { t.reject } else { t.pick }
            } else if hovered {
                t.text
            } else {
                t.icon
            },
        };
        let label = crate::i18n::tr(match f {
            Flag::None => "Unflagged",
            Flag::Pick => "Pick",
            Flag::Reject => "Reject",
        });
        // A measuring press must never arm a later real release: disable before framework sensing.
        let shown = craft::ui::buttons::IconButton::new(craft::ui::primitives::IconSpec::glyph(&glyph))
            .id(owner)
            .label(label)
            .selected(flag == f)
            .enabled(real && enabled)
            .show_at(&mut pill_ui, rect);
        if real {
            crate::widgets::register_response(format!("flag:{id}"), shown.response.rect, &shown.response);
        }
        if real
            && enabled
            && pill_ui.is_visible()
            && pill_ui.clip_rect().intersects(shown.response.rect)
            && shown.clicked()
            && app.session.library_generation() == generation
            && app.session.active() == active
            && app.session.selected() == selection
            && crate::panels::compare::culling(app) == culling
        {
            let mut params = json!({"flag": match f { Flag::Pick => "pick", Flag::Reject => "reject", Flag::None => "none" }});
            crate::panels::compare::target_active(app, &mut params);
            let _ = app.run("photo.flag", params);
        }
    }
    // copy / paste settings
    if !with_copy {
        return;
    }
    let copy_r = Rect::from_min_size(pos2(pill.right() + 10.0, pill.top()), vec2(copy_width, 30.0));
    let gear_r = Rect::from_min_size(pos2(copy_r.right() + 4.0, pill.top()), vec2(30.0, 30.0));
    settings_transfer_sdk(app, ui, &t, copy_r, gear_r, label, active.is_some());
}

// A held transfer belongs to its library, active source, ordered selection, view context,
// clipboard contents and chosen groups. Hash the existing values, not a fabricated UI epoch.
fn settings_transfer_owner(app: &LightcraftApp) -> egui::Id {
    egui::Id::unique((
        "lc-bottom-settings-transfer",
        app.session.library_generation(),
        app.session.active(),
        app.session.selected(),
        crate::panels::compare::culling(app),
        &app.session.clipboard,
        &app.session.copy_groups,
    ))
}

fn settings_transfer_sdk(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, copy_r: Rect, gear_r: Rect, label: &str, can_copy: bool) {
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let enabled = ui.is_enabled();
    let has_clip = app.session.clipboard.is_some();
    let mut context = settings_transfer_owner(app);
    // Disable before sensing: a measuring/sizing press must not arm an ordinary release.
    let primary = craft::ui::buttons::Button::new(label.to_owned())
        .id(context.with("primary"))
        .variant(craft::theme::Variant::GHOST)
        .enabled(real && enabled && can_copy)
        .show_at(ui, copy_r);
    crate::widgets::register_response("button:copySettings", primary.response.rect, &primary.response);
    if real
        && enabled
        && can_copy
        && ui.is_visible()
        && ui.clip_rect().intersects(primary.response.rect)
        && primary.clicked()
        && settings_transfer_owner(app) == context
    {
        // Paste remains selection-wide in Compare. Do not use Pick's target_active adapter.
        let _ = if has_clip { app.run("develop.paste", json!({})) } else { app.run("develop.copy", json!({})) };
        let msg = if has_clip { "Settings pasted" } else { "Edit settings copied" };
        app.toast(ui.ctx(), crate::i18n::tr(msg));
        context = settings_transfer_owner(app);
    }
    let glyph = SettingsGearGlyph { ink: t.icon };
    let gear = craft::ui::buttons::IconButton::new(craft::ui::primitives::IconSpec::glyph(&glyph))
        .id(context.with("gear"))
        .label(crate::i18n::tr("Copy Settings…"))
        // Keep the native gear's policy: it does not itself require an active photo.
        .enabled(real && enabled)
        .show_at(ui, gear_r);
    crate::widgets::register_response("icon:copyGear", gear.response.rect, &gear.response);
    if real
        && enabled
        && ui.is_visible()
        && ui.clip_rect().intersects(gear.response.rect)
        && gear.clicked()
        && settings_transfer_owner(app) == context
    {
        let _ = app.run("dialog.copySettings", json!({}));
    }
}

// Only the app's existing glyph ink/geometry is local; the SDK owns all interaction.
#[derive(craft::ui::contract::MemoProps)]
struct SettingsGearGlyph {
    ink: egui::Color32,
}
impl craft::ui::primitives::Glyph for SettingsGearGlyph {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, _: egui::Color32, _: bool) {
        paint(painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), Icon::Gear, self.ink);
    }
}

// Preserve LightCraft's flag drawings (outlined, or filled for a pick) inside the public SDK
// IconButton. This glyph has no sensing, event handling, component state or command binding.
#[derive(craft::ui::contract::MemoProps)]
struct FlagGlyph {
    icon: Icon,
    ink: egui::Color32,
}
impl craft::ui::primitives::Glyph for FlagGlyph {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, _: egui::Color32, _: bool) {
        paint(painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), self.icon, self.ink);
    }
}

/// The right-hand group (zoom, view toggles or thumbnail size); returns its left edge.
fn right_side(app: &mut LightcraftApp, ui: &mut egui::Ui, full: Rect, compact_zoom: bool) -> f32 {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2((full.right() - 360.0).max(full.left()), full.top()), full.right_bottom()))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    if matches!(app.ui.view, ViewMode::Compare | ViewMode::Survey)
        && icon_button(
            &mut child,
            "autoAdvance",
            Icon::ChevronRight,
            vec2(30.0, 30.0),
            app.ui.auto_advance,
            true,
            "Auto Advance after rating or flagging",
        )
        .clicked()
    {
        let _ = app.run("view.autoAdvance", json!({}));
    }
    if app.ui.view == ViewMode::Survey {
        // no zoom or thumbnail size in Survey
    } else if matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare) {
        if icon_button(&mut child, "beforeAfter", Icon::BeforeAfter, vec2(30.0, 30.0), app.ui.before_after.is_layout(), true, "Before/After (Y)")
            .clicked()
        {
            let _ = app.run("view.beforeAfter", json!({}));
        }
        if app.ui.view == ViewMode::Detail
            && icon_button(&mut child, "faceBoxes", Icon::FaceBox, vec2(30.0, 30.0), app.ui.face_boxes, true, "Face boxes").clicked()
        {
            let _ = app.run("view.faceBoxes", json!({}));
        }
        if icon_button(&mut child, "filmstrip", Icon::Filmstrip, vec2(30.0, 30.0), app.ui.filmstrip, true, "Filmstrip (/)").clicked() {
            let _ = app.run("view.filmstrip", json!({}));
        }
        child.add_space(10.0);
        let zoom_label = match app.ui.zoom {
            Zoom::Fit => "Fit".to_string(),
            Zoom::Fill => "Fill".to_string(),
            Zoom::Percent(p) => format!("{p:.0}%"),
        };
        let zr = dropdown_face(&mut child, "zoom", crate::i18n::tr(&zoom_label));
        let mut zoom_pick = None;
        if !measuring(&child) {
        Menu::new(crate::i18n::tr("Zoom")).id_salt("zoom-menu").values().opened_by(&zr, Trigger::Click).show(&mut child, |ui, _| {
            for (label, z) in [
                ("Fit", Zoom::Fit),
                ("Fill", Zoom::Fill),
                ("50%", Zoom::Percent(50.0)),
                ("100%", Zoom::Percent(100.0)),
                ("200%", Zoom::Percent(200.0)),
                ("400%", Zoom::Percent(400.0)),
            ] {
                if value_row(ui, ("zoom", label), crate::i18n::tr(label), app.ui.zoom == z) {
                    zoom_pick = Some(z);
                }
            }
        });
        }
        if let Some(z) = zoom_pick {
            app.ui.zoom = z;
            app.ui.pan = (0.5, 0.5);
        }
        if !compact_zoom {
            child.add_space(6.0);
            let cz = dropdown_face(&mut child, "clickZoom", &crate::i18n::tr_format!("Click {}:1", app.ui.click_zoom / 100));
            let mut ratio = None;
            Menu::new(crate::i18n::tr("Click zoom")).id_salt("click-zoom-menu").values().opened_by(&cz, Trigger::Click).show(&mut child, |ui, _| {
                ratio = click_zoom_rows(app.ui.click_zoom, ui);
            });
            if let Some(ratio) = ratio {
                let _ = app.run("view.clickZoom", json!({"ratio": ratio}));
            }
        }
    } else {
        // thumbnail size slider
        let (min, max) = (crate::state::THUMB_SIZE_MIN, crate::state::THUMB_SIZE_MAX);
        let (rect, _) = child.allocate_exact_size(vec2(110.0, 20.0), egui::Sense::hover());
        let mut size = f64::from(app.ui.thumb_size);
        // The framework slider is the screen readers' slider with its value and range, which Increment /
        // Decrement and the arrow keys move one step.
        let shown = Slider::new(Bind::bound(&mut size), f64::from(min)..=f64::from(max))
            .id_salt("thumb-size")
            .label(crate::i18n::tr("Thumbnail size"))
            .step(f64::from(crate::state::THUMB_SIZE_STEP))
            .show_at(&mut child, rect);
        if !measuring(&child) {
            crate::widgets::register_response("slider:thumbSize", rect, &shown.response);
            if child.is_enabled() && size as f32 != app.ui.thumb_size {
                app.ui.thumb_size = (size as f32).clamp(min, max);
            }
        }
    }
    if child.min_rect().width() > 0.0 { child.min_rect().left() } else { full.right() }
}

/// Whether this pass draws nothing the user can reach: a sizing or measuring run publishes no
/// automation names and runs no command.
fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

/// A dropdown face: the framework push button showing `text` and the menu chevron, published as
/// `dropdown:<id>`. The caller hangs a framework menu from the returned response.
fn dropdown_face(ui: &mut egui::Ui, id: &str, text: &str) -> egui::Response {
    let shown = Button::new(text.to_owned())
        .id_salt(("bottom-dropdown", id))
        .variant(craft::theme::Variant::GHOST)
        .trailing_icon(DisclosureGlyph::dropdown())
        .show(ui);
    if !measuring(ui) {
        crate::widgets::register_response(format!("dropdown:{id}"), shown.response.rect, &shown.response);
    }
    shown.response
}

/// One row of a value menu: radio-marked while `checked`, the lead when the menu opens. True when the row ran.
fn value_row(ui: &mut egui::Ui, salt: impl egui::AsIdSalt, caption: &str, checked: bool) -> bool {
    let row = MenuItem::new(caption.replace('&', "&&")).id_salt(salt).entry_checked(checked);
    let row = if checked { row.default_stop() } else { row };
    let out = row.show(ui);
    !measuring(ui) && out.ran
}

/// The click-zoom ratios as value rows; the command that sets the one that ran.
fn click_zoom_rows(current: u32, ui: &mut egui::Ui) -> Option<serde_json::Value> {
    let mut ran = None;
    for pct in crate::state::CLICK_ZOOMS {
        if value_row(ui, ("click-zoom", pct), &format!("{}:1", pct / 100), current == pct) {
            ran = Some(json!({"ratio": pct / 100}));
        }
    }
    ran
}

/// One checked sort option in the Sort menu. The framework owns its focus and hit target.
fn sort_choice(ui: &mut egui::Ui, id: &str, label: &str, checked: bool, enabled: bool) -> bool {
    let measuring = measuring(ui);
    let row = MenuItem::new(crate::i18n::tr(label).replace('&', "&&")).id_salt(id).entry_checked(checked).enabled(enabled && !measuring).show(ui);
    if measuring {
        return false;
    }
    crate::widgets::register_response(id, row.response.rect, &row.response);
    enabled && row.ran
}

/// The rows of the Sort menu: what to sort by, direction and date groups. The row that ran is
/// left in `pick` for the caller to dispatch once the menu is drawn.
fn sort_rows(app: &LightcraftApp, ui: &mut egui::Ui, enabled: bool, pick: &mut Option<(&'static str, serde_json::Value)>) {
    use lightcraft_catalog::GroupBy;
    use lightcraft_catalog::SortKey::*;
    let cur = app.session.sort;
    MenuItem::heading(crate::i18n::tr("Sort by")).show(ui);
    for (label, key, k) in [
        ("Capture Date", CaptureDate, "captureDate"),
        ("Import Date", ImportDate, "importDate"),
        ("Modified Date", EditDate, "editDate"),
        ("File Name", FileName, "fileName"),
        ("Rating", Rating, "rating"),
        ("File Size", FileSize, "fileSize"),
        ("Random", Random, "random"),
    ] {
        if sort_choice(ui, &format!("sort:key:{k}"), label, cur.key == key, enabled) {
            *pick = Some(("library.sort", json!({"key": k})));
        }
    }
    if cur.key == Random {
        let row = MenuItem::new(crate::i18n::tr("Reshuffle")).id_salt("sort-reshuffle").enabled(enabled).show(ui);
        if !measuring(ui) && enabled && row.ran {
            *pick = Some(("library.shuffle", json!({})));
        }
    }
    // a view that holds its photos shows what matches now when asked to
    if app.session.hold_view {
        let row = MenuItem::new(crate::i18n::tr("Refresh View")).id_salt("sort-refresh-view").enabled(enabled).show(ui);
        if !measuring(ui) {
            crate::widgets::register_response("button:refreshView", row.response.rect, &row.response);
            if enabled && row.ran {
                *pick = Some(("library.refresh", json!({})));
            }
        }
    }
    Divider::horizontal().show(ui);
    // a shuffle has no direction worth choosing
    if sort_choice(ui, "sort:ascending", "Ascending", cur.key != Random && cur.ascending, enabled && cur.key != Random) {
        *pick = Some(("library.sort", json!({"ascending": true})));
    }
    if sort_choice(ui, "sort:descending", "Descending", cur.key != Random && !cur.ascending, enabled && cur.key != Random) {
        *pick = Some(("library.sort", json!({"ascending": false})));
    }
    Divider::horizontal().show(ui);
    MenuItem::heading(crate::i18n::tr("Group by date")).show(ui);
    for (label, g, k) in [
        ("Automatic", GroupBy::Auto, "auto"),
        ("Day", GroupBy::Day, "day"),
        ("Month", GroupBy::Month, "month"),
        ("Year", GroupBy::Year, "year"),
        ("None", GroupBy::None, "none"),
    ] {
        if sort_choice(ui, &format!("sort:group:{k}"), label, cur.group == g, enabled) {
            *pick = Some(("library.sort", json!({"group": k})));
        }
    }
}

/// The Sort menu hanging from the Sort button `opener`.
fn sort_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, opener: &egui::Response, enabled: bool) {
    // A measuring or sizing run shows no layer: its rows would claim the menu's ids a second time in the pass.
    if measuring(ui) {
        return;
    }
    let mut pick = None;
    Menu::new(crate::i18n::tr("Sort")).id_salt("sort-menu").opened_by(opener, Trigger::Click).show(ui, |ui, _| {
        sort_rows(app, ui, enabled, &mut pick);
    });
    if let Some((command, params)) = pick {
        let _ = app.run(command, params);
    }
}

/// The compact bar's View menu hanging from the view button `opener`: the view modes, then Sort and
/// (when it is not on the bar) Click zoom as submenus.
fn view_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, opener: &egui::Response, compact_zoom: bool, enabled: bool) {
    if measuring(ui) {
        return;
    }
    let mut pick: Option<(&'static str, serde_json::Value)> = None;
    let mut view = None;
    Menu::new(crate::i18n::tr("View")).id_salt("view-options-menu").opened_by(opener, Trigger::Click).show(ui, |ui, _| {
        for &(id, _, mode, label) in VIEW_MODES {
            let row = MenuItem::new(crate::i18n::tr(label).replace('&', "&&")).id_salt(("view-mode", id)).check(app.ui.view == mode).enabled(enabled);
            let out = row.show(ui);
            if !measuring(ui) && enabled && out.ran {
                view = Some(id);
            }
        }
        Divider::horizontal().show(ui);
        Menu::submenu_row_enabled(ui, crate::i18n::tr("Sort"), enabled, |ui, _| sort_rows(app, ui, enabled, &mut pick));
        if compact_zoom && matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare) {
            let label = crate::i18n::tr_format!("Click {}:1", app.ui.click_zoom / 100);
            Menu::submenu_row_enabled(ui, label, enabled, |ui, _| {
                if let Some(params) = click_zoom_rows(app.ui.click_zoom, ui) {
                    pick = Some(("view.clickZoom", params));
                }
            });
        }
    });
    if let Some(id) = view {
        let _ = app.run(&format!("view.{id}"), json!({}));
    }
    if let Some((command, params)) = pick {
        let _ = app.run(command, params);
    }
}

#[cfg(test)]
mod visual_regressions {
    use super::*;

    fn scaled_input(size: egui::Vec2, scale: f32) -> egui::RawInput {
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(scale);
        input
    }

    #[test]
    fn capture_bottom_bar_visual_fixtures() {
        let Some(directory) = std::env::var_os("CRAFT_UI_VISUAL_FIXTURES").map(std::path::PathBuf::from) else { return };
        std::fs::create_dir_all(&directory).unwrap();
        for width in [280.0, 400.0, 600.0, 1000.0] {
            for view in [
                ViewMode::PhotoGrid,
                ViewMode::SquareGrid,
                ViewMode::Detail,
                ViewMode::Compare,
                ViewMode::Survey,
                ViewMode::Reference,
                ViewMode::People,
            ] {
                for scale in [1.0, 1.5, 2.0] {
                    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
                    app.ui.view = view;
                    let mut ready = false;
                    let mut harness =
                        egui_kittest::Harness::builder().with_size(vec2(width, 100.0)).with_pixels_per_point(scale).wgpu().build_ui_state(
                            move |ui, app: &mut LightcraftApp| {
                                if !ready {
                                    crate::theme::install_fonts(ui.ctx());
                                    crate::theme::apply(ui.ctx());
                                    ready = true;
                                    return;
                                }
                                crate::widgets::take_registry(ui.ctx());
                                show(app, ui);
                            },
                            app,
                        );
                    harness.input_mut().max_texture_side = Some(8192);
                    harness.run_steps(4);
                    let stem = format!("lightcraft-bottom-{view:?}-{width}-{scale}x");
                    harness.render().unwrap().save(directory.join(format!("{stem}.png"))).unwrap();
                    let panel_id = harness.ctx.viewport_id().root_ui_id().with("bottom_bar");
                    let panel = egui::containers::panel::PanelState::load(&harness.ctx, panel_id).unwrap().outer_rect;
                    let controls: serde_json::Map<String, serde_json::Value> = crate::widgets::take_registry(&harness.ctx)
                        .into_iter()
                        .map(|(id, r)| (id, json!([r.left(), r.top(), r.width(), r.height()])))
                        .collect();
                    std::fs::write(
                        directory.join(format!("{stem}.json")),
                        serde_json::to_vec_pretty(&json!({
                            "screen": [width, 100.0], "scale": scale,
                            "regions": {"bottom-bar": [panel.left(), panel.top(), panel.width(), panel.height()]},
                            "controls": controls
                        }))
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn bottom_bar_controls_do_not_overlap_in_narrow_canvas() {
        for width in [280.0, 400.0, 600.0, 1000.0] {
            for view in [
                ViewMode::PhotoGrid,
                ViewMode::SquareGrid,
                ViewMode::Detail,
                ViewMode::Compare,
                ViewMode::Survey,
                ViewMode::Reference,
                ViewMode::People,
            ] {
                for scale in [1.0, 1.5, 2.0] {
                    let ctx = egui::Context::default();
                    crate::theme::install_fonts(&ctx);
                    crate::theme::apply(&ctx);
                    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
                    app.ui.view = view;
                    let mut output = ctx.run_ui(scaled_input(vec2(width, 100.0), scale), |ui| show(&mut app, ui));
                    output.textures_delta.clear();
                    assert_eq!(ctx.content_rect().size(), vec2(width, 100.0));
                    assert_eq!(ctx.pixels_per_point(), scale);
                    let controls = crate::widgets::take_registry(&ctx);
                    assert!(!controls.is_empty());
                    for (index, (id, rect)) in controls.iter().enumerate() {
                        assert!(rect.left() >= 0.0 && rect.right() <= width, "{view:?} at {width}: {id} outside viewport: {rect:?}");
                        for (other_id, other) in controls.iter().skip(index + 1) {
                            assert!(!rect.intersect(*other).is_positive(), "{view:?} at {width}: {id} overlaps {other_id}: {rect:?} / {other:?}");
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn compact_view_menu_opens_and_changes_view_by_keyboard() {
        use egui_kittest::kittest::Queryable;

        for scale in [1.0, 1.5, 2.0] {
            let app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
            let mut ready = false;
            let mut harness = egui_kittest::Harness::builder().with_size(vec2(280.0, 640.0)).with_pixels_per_point(scale).build_ui_state(
                move |ui, app: &mut LightcraftApp| {
                    if !ready {
                        crate::theme::install_fonts(ui.ctx());
                        crate::theme::apply(ui.ctx());
                        // the framework menus need the runtime installed, as the app does at startup
                        app.install_framework(ui.ctx());
                        ready = true;
                        return;
                    }
                    crate::shortcuts::handle(app, ui.ctx());
                    crate::widgets::take_registry(ui.ctx());
                    show(app, ui);
                },
                app,
            );
            harness.run_steps(4);
            assert_eq!(harness.ctx.content_rect().size(), vec2(280.0, 640.0));
            assert_eq!(harness.ctx.pixels_per_point(), scale);
            let mut focused = false;
            for _ in 0..12 {
                harness.key_press(egui::Key::Tab);
                harness.run_steps(3);
                focused = harness.get_by_label("View").is_focused();
                if focused {
                    break;
                }
            }
            assert!(focused, "Tab reaches the compact View control at {scale}x");
            harness.key_press(egui::Key::Space);
            harness.run_steps(3);
            let mut people_focused = false;
            // the menu's rows are one roving group: the arrow keys move through them
            for _ in 0..20 {
                harness.key_press(egui::Key::ArrowDown);
                harness.run_steps(3);
                people_focused = harness.get_by_label("People").is_focused();
                if people_focused {
                    break;
                }
            }
            assert!(people_focused, "ArrowDown reaches People at {scale}x");
            harness.key_press(egui::Key::Space);
            harness.run_steps(3);
            assert_eq!(harness.state().ui.view, ViewMode::People, "keyboard activation dispatches the view command");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::HeadlessView;

    #[test]
    fn rating_and_flag_buttons_keep_padding_inside_the_pill() {
        for scale in [1.0, 2.0] {
            for room in [240.0, 500.0] {
                let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
                let mut view = HeadlessView::new();
                for frame in 0..3 {
                    let raw = HeadlessView::raw_input(vec2(700.0, 40.0), scale, frame as f64 / 60.0, vec![]);
                    view.run(raw, |ui| {
                        let full = ui.max_rect();
                        centre(&mut app, ui, full, 20.0, 20.0 + room);
                    });
                    let widgets = crate::widgets::take_registry(&view.ctx);
                    let star = widgets.iter().find(|(id, _)| id == "star:bottom:1").unwrap().1;
                    let pill = view
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Rect(rect) if rect.rect.contains(star.center()) && rect.rect.height() == 30.0 => Some(rect.rect),
                            _ => None,
                        })
                        .unwrap();
                    let inner = pill.shrink2(vec2(10.0, 4.0));
                    for (id, rect) in &widgets {
                        if id.starts_with("star:bottom:") || id.starts_with("flag:") {
                            assert!(inner.contains_rect(*rect), "{id} {rect:?} exceeds the pill's padded bounds {inner:?} at scale {scale}");
                        }
                    }
                    assert!(pill.left() >= 20.0 && pill.right() <= 20.0 + room);
                    if let Some((_, copy)) = widgets.iter().find(|(id, _)| id == "button:copySettings") {
                        assert!(copy.left() >= pill.right() + 10.0);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "bottombar/sort_popup_external_20261119.rs"]
mod sort_popup_external_20261119;

#[cfg(test)]
#[path = "bottombar/pick_flow_tests.rs"]
mod pick_flow_tests;

#[cfg(test)]
#[path = "bottombar/copy_paste_flow_tests.rs"]
mod copy_paste_flow_tests;
