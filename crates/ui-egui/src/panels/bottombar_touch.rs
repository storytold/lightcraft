//! The bottom bar: view modes, sort, rating/flags pill, copy/paste settings, zoom and view toggles.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::Flag;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::{ViewMode, Zoom};
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

const VIEW_MODES: &[(&str, Icon, ViewMode, &str)] = &[
    ("photoGrid", Icon::GridPhoto, ViewMode::PhotoGrid, "Photo Grid (G)"),
    ("squareGrid", Icon::GridSquare, ViewMode::SquareGrid, "Square Grid (G toggles)"),
    ("detail", Icon::Single, ViewMode::Detail, "Detail (D)"),
    ("compare", Icon::Compare, ViewMode::Compare, "Compare (Shift+C)"),
    ("survey", Icon::Survey, ViewMode::Survey, "Survey (N)"),
    ("people", Icon::Subject, ViewMode::People, "People"),
];

/// Narrower than this on the iPad, the bar scrolls sideways instead of squeezing its controls together.
const BAR_MIN_W: f32 = 640.0;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let insets = crate::theme::safe_area_insets(ui.ctx());
    let safe_bottom = insets[1].max(if crate::theme::touch_ui(ui.ctx()) { 20.0 } else { 0.0 });
    let safe_left = insets[2];
    let safe_right = insets[3];
    let total_h = t.bottom_bar_h + safe_bottom;
    let glass = crate::glass::active(ui.ctx());
    let frame = if glass {
        // a floating glass shell above the home indicator
        crate::glass::shell(
            egui::Margin { left: 12, right: 12, top: 0, bottom: 0 },
            egui::Margin { left: 10 + safe_left.round() as i8, right: 10 + safe_right.round() as i8, top: 4, bottom: safe_bottom.round() as i8 },
        )
    } else {
        egui::Frame::NONE
            .fill(t.chrome)
            .stroke(if !crate::theme::touch_ui(ui.ctx()) {
                Stroke::NONE
            } else if ui.visuals().dark_mode {
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20))
            } else {
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 24))
            })
            .inner_margin(egui::Margin {
                left: (if crate::theme::touch_ui(ui.ctx()) { 14.0f32 + safe_left } else { 12.0 }).round() as i8,
                right: (if crate::theme::touch_ui(ui.ctx()) { 14.0f32 + safe_right } else { 12.0 }).round() as i8,
                top: 0,
                bottom: safe_bottom.round() as i8,
            })
    };
    egui::Panel::bottom("bottom_bar").exact_size(if glass { t.bottom_bar_h + safe_bottom + 4.0 } else { total_h }).frame(frame).show(ui, |ui| {
        let avail = ui.max_rect();
        if crate::theme::touch_ui(ui.ctx()) && avail.width() < BAR_MIN_W {
            // too narrow for the controls (Split View, Slide Over): keep their layout and scroll the bar sideways
            let out = egui::ScrollArea::horizontal()
                .id_salt("bottom-bar-scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (strip, _) = ui.allocate_exact_size(vec2(BAR_MIN_W, avail.height()), Sense::hover());
                    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(strip));
                    bar_contents(app, &mut inner, strip);
                });
            // a fade and a chevron at each end that has more beyond it: the bar visibly slides
            let (offset, view_w) = (out.state.clamped_offset().x, out.inner_rect.width());
            let painter = ui.painter();
            let fade_w = 34.0;
            if offset > 1.0 {
                let r = Rect::from_min_size(out.inner_rect.min, vec2(fade_w, out.inner_rect.height()));
                crate::glass::fade_edge(painter, r, true);
                paint(painter, Rect::from_center_size(pos2(r.left() + 10.0, r.center().y), vec2(16.0, 16.0)), Icon::ChevronLeft, t.text);
            }
            if offset + view_w < out.content_size.x - 1.0 {
                let r = Rect::from_min_size(pos2(out.inner_rect.right() - fade_w, out.inner_rect.top()), vec2(fade_w, out.inner_rect.height()));
                crate::glass::fade_edge(painter, r, false);
                paint(painter, Rect::from_center_size(pos2(r.right() - 10.0, r.center().y), vec2(16.0, 16.0)), Icon::ChevronRight, t.text);
            }
        } else {
            bar_contents(app, ui, avail);
        }
    });
}

/// Five 19-point stars, three 22-point flags, seven 3-point gaps, a 6-point group gap
/// and 10-point padding at both ends.
const PILL_W: f32 = 208.0;
/// Wider on the iPad, where the pick / reject buttons are 30pt touch targets.
const PILL_W_TOUCH: f32 = 324.0;

/// The stars and the unflag / pick (tick) / reject (cross) buttons, laid out left to right in `pill_ui`.
fn rating_pill_contents(app: &mut LightcraftApp, pill_ui: &mut egui::Ui, active: Option<&lightcraft_catalog::Photo>) {
    let t = Tokens::get(pill_ui.ctx());
    pill_ui.spacing_mut().item_spacing.x = 3.0;
    let rating = active.map(|p| p.rating).unwrap_or(0);
    if let Some(r) = crate::rating::Rating::new("bottom", rating).show(pill_ui) {
        let mut params = json!({"rating": r});
        crate::panels::compare::target_active(app, &mut params);
        let _ = app.run("photo.rate", params);
    }
    pill_ui.add_space(6.0);
    let flag = active.map(|p| p.flag).unwrap_or_default();
    for (id, icon, f) in [("unflag", Icon::Circle, Flag::None), ("pick", Icon::FlagPick, Flag::Pick), ("reject", Icon::FlagReject, Flag::Reject)] {
        let (r, resp) = pill_ui.allocate_exact_size(
            vec2(if crate::theme::touch_ui(pill_ui.ctx()) { 36.0 } else { 22.0 }, if crate::theme::touch_ui(pill_ui.ctx()) { 36.0 } else { 22.0 }),
            Sense::click(),
        );
        register(pill_ui, format!("flag:{id}"), r);
        let on = flag == f && f != Flag::None;
        let name = crate::i18n::tr(match f {
            Flag::Pick => "Pick",
            Flag::Reject => "Reject",
            Flag::None => "Unflag",
        });
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name));
        let c = if on {
            if f == Flag::Reject { t.reject } else { t.pick }
        } else if resp.hovered() {
            t.text
        } else {
            t.icon
        };
        paint(pill_ui.painter(), r.shrink(if crate::theme::touch_ui(pill_ui.ctx()) { 7.0 } else { 3.0 }), icon, c);
        if resp.clicked() {
            let mut params = json!({"flag": match f { Flag::Pick => "pick", Flag::Reject => "reject", Flag::None => "none" }});
            crate::panels::compare::target_active(app, &mut params);
            let _ = app.run("photo.flag", params);
        }
    }
}

/// The bar's controls laid out inside `full` (the bar's inner rectangle, or the wider scrolling strip on a narrow iPad).
fn bar_contents(app: &mut LightcraftApp, ui: &mut egui::Ui, full: Rect) {
    let t = Tokens::get(ui.ctx());
    let compact_zoom = full.width() < 360.0;
    let right_start = right_side(app, ui, full, compact_zoom);
    let left_rect = Rect::from_min_max(full.min, pos2((right_start - 12.0).max(full.left()), full.bottom()));
    let mut left_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
    let left_end = left_ui
        .horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let min_expand_w = if crate::theme::touch_ui(ui.ctx()) { if full.width() < 800.0 { f32::MAX } else { 420.0 } } else { 270.0 };
            if left_rect.width() >= min_expand_w {
                let mode_sz = if crate::theme::touch_ui(ui.ctx()) { vec2(36.0, 36.0) } else { vec2(32.0, 32.0) };
                for &(id, icon, mode, tip) in VIEW_MODES {
                    if icon_button(ui, id, icon, mode_sz, app.ui.view == mode, true, tip).clicked() {
                        let _ = app.run(&format!("view.{id}"), json!({}));
                    }
                }
                ui.add_space(10.0);
                let (sep, _) = ui.allocate_exact_size(vec2(1.0, 22.0), Sense::hover());
                ui.painter().rect_filled(sep, 0.0, t.button_border);
                ui.add_space(10.0);
                let sort = icon_button(ui, "sort", Icon::Sort, vec2(30.0, 30.0), false, true, "Sort");
                egui::Popup::menu(&sort).show(|ui| {
                    crate::widgets::touch_menu_style(ui);
                    sort_menu(app, ui)
                });
                if crate::theme::touch_ui(ui.ctx()) {
                    bulk_buttons(app, ui, &t);
                }
            } else {
                let current_icon = VIEW_MODES.iter().find(|(_, _, mode, _)| *mode == app.ui.view).map_or(Icon::More, |(_, icon, _, _)| *icon);
                let more = icon_button(ui, "viewOptions", current_icon, vec2(40.0, 32.0), true, true, "View");
                paint(ui.painter(), Rect::from_center_size(more.rect.right_bottom() - vec2(5.0, 5.0), vec2(7.0, 7.0)), Icon::ChevronDown, t.text);
                egui::Popup::menu(&more).show(|ui| {
                    crate::widgets::touch_menu_style(ui);
                    for &(id, _, mode, label) in VIEW_MODES {
                        if ui.selectable_label(app.ui.view == mode, crate::i18n::tr(label)).clicked() {
                            let _ = app.run(&format!("view.{id}"), json!({}));
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.menu_button(crate::i18n::tr("Sort"), |ui| sort_menu(app, ui));
                    if crate::theme::touch_ui(ui.ctx()) && matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                        ui.separator();
                        let multi_selected = app.session.selection.ids.len() > 1;
                        let sel_label = if multi_selected {
                            format!("{} ({})", crate::i18n::tr("Deselect"), app.session.selection.ids.len())
                        } else {
                            crate::i18n::tr("Select All").to_string()
                        };
                        if ui.button(sel_label).clicked() {
                            if multi_selected {
                                let _ = app.run("library.selectNone", json!({}));
                            } else {
                                let _ = app.run("library.selectAll", json!({}));
                            }
                            ui.close();
                        }
                        if multi_selected {
                            let exp_label = format!("{} ({})", crate::i18n::tr("Export"), app.session.selection.ids.len());
                            if ui.button(egui::RichText::new(exp_label).color(t.accent)).clicked() {
                                let _ = app.run("dialog.export", json!({}));
                                ui.close();
                            }
                        }
                    }
                    if compact_zoom && matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare) {
                        ui.menu_button(click_zoom_label(app.ui.click_zoom / 100, crate::theme::touch_ui(ui.ctx())), |ui| click_zoom_menu(app, ui));
                    }
                });
                // (the desktop's compact bar has only the menu above)
                if crate::theme::touch_ui(ui.ctx()) {
                    bulk_buttons(app, ui, &t);
                }
            }
        })
        .response
        .rect
        .right();
    centre(app, ui, full, left_end + 12.0, right_start - 12.0);
}

/// Export, Select All / Deselect All and Sync Edits, only while selecting several photos and never at the rater's
/// expense: Export first, then Select All, then Sync Edits, each only if it fits beside the rater.
fn bulk_buttons(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let touch = crate::theme::touch_ui(ui.ctx());
    // on the iPad also whenever several photos are selected; on the desktop only in Select mode, as before
    let selecting = if touch { app.ui.multi_select || app.session.selection.ids.len() > 1 } else { app.ui.multi_select };
    if selecting && !app.session.visible().is_empty() {
        let reserve = if touch { PILL_W_TOUCH + 14.0 } else { 0.0 };
        ui.add_space(8.0);
        let sel_count = app.session.selection.ids.len();
        let all_count = app.session.visible().len();
        let is_all = sel_count >= all_count && all_count > 0;
        if sel_count > 0 && ui.available_width() - reserve >= 100.0 {
            let exp_label = format!("{} ({sel_count})", crate::i18n::tr("Export"));
            let exp_btn = egui::Button::new(egui::RichText::new(exp_label).font(t.semibold(12.5)).color(Color32::WHITE))
                .fill(t.accent)
                .corner_radius(18.0)
                .min_size(vec2(85.0, 36.0));
            if ui.add(exp_btn).clicked() {
                let _ = app.run("dialog.export", json!({}));
            }
            ui.add_space(4.0);
        }
        if ui.available_width() - reserve >= 120.0 {
            let sel_label = if is_all {
                format!("{} ({sel_count})", crate::i18n::tr("Deselect All"))
            } else {
                format!("{} ({all_count})", crate::i18n::tr("Select All"))
            };
            let btn = egui::Button::new(egui::RichText::new(sel_label).font(t.font(12.0))).min_size(vec2(85.0, 36.0));
            if ui.add(btn).clicked() {
                let _ = app.run(if is_all { "library.selectNone" } else { "library.selectAll" }, json!({}));
            }
        }
        if sel_count > 1 && ui.available_width() - reserve >= 96.0 {
            ui.add_space(4.0);
            let sync_btn = egui::Button::new(egui::RichText::new(crate::i18n::tr("Sync Edits")).font(t.font(12.0))).min_size(vec2(80.0, 36.0));
            if ui.add(sync_btn).on_hover_text(crate::i18n::tr("Apply active photo edits to all selected photos")).clicked() {
                let _ = app.run("develop.sync", json!({}));
            }
        }
    }
}

/// The rating/flag pill and copy/paste settings, between `from` and `to` (the side groups): the
/// copy buttons go first when there is no room (they are in the Edit menu too), then the pill.
fn centre(app: &mut LightcraftApp, ui: &mut egui::Ui, full: Rect, from: f32, to: f32) {
    let t = Tokens::get(ui.ctx());
    let has_clip = app.session.clipboard.is_some();
    let label = crate::i18n::tr(if has_clip { "Paste Edit Settings" } else { "Copy Edit Settings" });
    let copy_width = (ui.painter().layout_no_wrap(label.to_string(), t.font(13.0), t.text_label).size().x + 24.0).max(136.0);
    let pill_w = if crate::theme::touch_ui(ui.ctx()) { PILL_W_TOUCH } else { PILL_W };
    let centre_width = pill_w + 10.0 + copy_width + 4.0 + 30.0;
    let room = to - from;
    // no room for it in the bar: the canvas shows it as a floating capsule instead (touch layout)
    app.ui.rating_floating = room < pill_w && crate::theme::touch_ui(ui.ctx());
    if room < pill_w {
        return;
    }
    let with_copy = room >= centre_width;
    let w = if with_copy { centre_width } else { pill_w };
    // where it sits with room to spare (slightly left of centre), kept between the side groups
    let left = (full.center().x - 60.0 - pill_w / 2.0).clamp(from, to - w);
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned());
    let pill_h = if crate::theme::touch_ui(ui.ctx()) { 40.0 } else { 30.0 };
    let pill = Rect::from_min_size(pos2(left, full.center().y - pill_h / 2.0), vec2(pill_w, pill_h));
    // (inside a glass bar the pill is only a faint inset: glass does not sit on opaque dark)
    ui.painter().rect_filled(pill, 15.0, if crate::glass::active(ui.ctx()) { Color32::from_white_alpha(14) } else { t.canvas });
    let mut pill_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(pill.shrink2(vec2(10.0, if pill_h > 30.0 { 2.0 } else { 4.0 })))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    rating_pill_contents(app, &mut pill_ui, active.as_deref());
    // Undo / Redo right after the rater (the top bar has them too), when there is room for them
    // (Copy Edit Settings matters more than a second pair of undo / redo buttons: they appear only when both fit)
    let undo_group = crate::theme::touch_ui(ui.ctx()) && room >= centre_width + 78.0;
    if undo_group {
        let r = Rect::from_min_size(pos2(pill.right() + 8.0, pill.top()), vec2(72.0, pill_h));
        let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
        cu.spacing_mut().item_spacing.x = 4.0;
        if icon_button(&mut cu, "bottomUndo", Icon::Undo, vec2(34.0, pill_h), false, !app.session.undo.is_empty(), "Undo").clicked() {
            let _ = app.run("edit.undo", json!({}));
        }
        if icon_button(&mut cu, "bottomRedo", Icon::Redo, vec2(34.0, pill_h), false, !app.session.redo.is_empty(), "Redo").clicked() {
            let _ = app.run("edit.redo", json!({}));
        }
    }
    // copy / paste settings
    if !with_copy {
        return;
    }
    let copy_r = Rect::from_min_size(pos2(pill.right() + 10.0 + if undo_group { 78.0 } else { 0.0 }, pill.top()), vec2(copy_width, 30.0));
    let cresp = ui.interact(copy_r, egui::Id::unique("copy-settings"), Sense::click());
    register(ui, "button:copySettings", copy_r);
    crate::widgets::announce(&cresp, label.to_string());
    ui.painter().rect_filled(copy_r, 15.0, if cresp.hovered() { t.hover } else { t.canvas });
    ui.painter().text(copy_r.center(), Align2::CENTER_CENTER, label, t.font(13.0), if active.is_some() { t.text_label } else { t.text_disabled });
    if cresp.clicked() && active.is_some() {
        let _ = if has_clip { app.run("develop.paste", json!({})) } else { app.run("develop.copy", json!({})) };
        let msg = if has_clip { "Settings pasted" } else { "Edit settings copied" };
        app.toast(ui.ctx(), crate::i18n::tr(msg));
    }
    let gear_r = Rect::from_min_size(pos2(copy_r.right() + 4.0, pill.top()), vec2(30.0, 30.0));
    let gresp = ui.interact(gear_r, egui::Id::unique("copy-gear"), Sense::click());
    register(ui, "icon:copyGear", gear_r);
    crate::widgets::announce(&gresp, crate::i18n::tr("Copy settings options"));
    ui.painter().rect_filled(gear_r, 15.0, if gresp.hovered() { t.hover } else { t.canvas });
    paint(ui.painter(), gear_r.shrink(7.0), Icon::Gear, t.icon);
    if gresp.clicked() {
        let _ = app.run("dialog.copySettings", json!({}));
    }
}

/// The right-hand group (zoom, view toggles or thumbnail size); returns its left edge.
fn right_side(app: &mut LightcraftApp, ui: &mut egui::Ui, full: Rect, compact_zoom: bool) -> f32 {
    let t = Tokens::get(ui.ctx());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2((full.right() - 360.0).max(full.left()), full.top()), full.right_bottom()))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    // with a tool panel open the bar is too narrow for the rater and every view toggle: the toggles move into a menu
    let condensed = crate::theme::touch_ui(child.ctx()) && full.width() < 900.0;
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
        if condensed {
            let more = icon_button(&mut child, "viewMore", Icon::More, vec2(30.0, 30.0), false, true, "More view options");
            egui::Popup::menu(&more).show(|ui| {
                crate::widgets::touch_menu_style(ui);
                if ui.selectable_label(app.ui.before_after.is_layout(), crate::i18n::tr("Before / After")).clicked() {
                    let _ = app.run("view.beforeAfter", json!({}));
                    ui.close();
                }
                if app.ui.view == ViewMode::Detail && ui.selectable_label(app.ui.face_boxes, crate::i18n::tr("Face boxes")).clicked() {
                    let _ = app.run("view.faceBoxes", json!({}));
                    ui.close();
                }
                if ui.selectable_label(app.ui.filmstrip, crate::i18n::tr("Filmstrip")).clicked() {
                    let _ = app.run("view.filmstrip", json!({}));
                    ui.close();
                }
                ui.separator();
                ui.label(egui::RichText::new(crate::i18n::tr("Tap zoom")).color(t.text_dim));
                click_zoom_menu(app, ui);
            });
        } else {
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
        }
        child.add_space(10.0);
        let zoom_label = match app.ui.zoom {
            Zoom::Fit => "Fit".to_string(),
            Zoom::Fill => "Fill".to_string(),
            Zoom::Percent(p) => format!("{p:.0}%"),
        };
        let zr = crate::widgets::dropdown(&mut child, "zoom", crate::i18n::tr(&zoom_label), t.font(13.0), t.text_label);
        egui::Popup::menu(&zr).show(|ui| {
            crate::widgets::touch_menu_style(ui);
            for (label, z) in [
                ("Fit", Zoom::Fit),
                ("Fill", Zoom::Fill),
                ("50%", Zoom::Percent(50.0)),
                ("100%", Zoom::Percent(100.0)),
                ("200%", Zoom::Percent(200.0)),
                ("400%", Zoom::Percent(400.0)),
            ] {
                if ui.selectable_label(app.ui.zoom == z, crate::i18n::tr(label)).clicked() {
                    app.ui.zoom = z;
                    app.ui.pan = (0.5, 0.5);
                }
            }
        });
        if !compact_zoom && !condensed {
            child.add_space(6.0);
            let zoom_label = click_zoom_label(app.ui.click_zoom / 100, crate::theme::touch_ui(child.ctx()));
            let cz = crate::widgets::dropdown(&mut child, "clickZoom", &zoom_label, t.font(13.0), t.text_label);
            egui::Popup::menu(&cz).show(|ui| {
                crate::widgets::touch_menu_style(ui);
                click_zoom_menu(app, ui)
            });
        }
    } else {
        // thumbnail size slider
        let touch = crate::theme::touch_ui(child.ctx());
        let (r, resp) = child.allocate_exact_size(if touch { vec2(120.0, 36.0) } else { vec2(110.0, 20.0) }, Sense::click_and_drag());
        register(&child, "slider:thumbSize", r);
        let p = child.painter();
        let y = r.center().y;
        let f = (app.ui.thumb_size - 90.0) / (480.0 - 90.0);
        let x = r.left() + f * r.width();
        if touch {
            // a capsule track filled up to a round glass thumb
            let track = Rect::from_min_max(pos2(r.left(), y - 2.0), pos2(r.right(), y + 2.0));
            p.rect_filled(track, 2.0, t.track);
            p.rect_filled(Rect::from_min_max(track.min, pos2(x, track.bottom())), 2.0, t.accent);
            p.circle_filled(pos2(x, y + 1.5), 11.0, Color32::from_black_alpha(70));
            p.circle_filled(pos2(x, y), 10.0, t.thumb_hover);
        } else {
            p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], egui::Stroke::new(2.0, t.track));
            p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(3.0, 14.0)), 1.0, t.thumb_hover);
        }
        if let Some(pp) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            app.ui.thumb_size = 90.0 + ((pp.x - r.left()) / r.width()).clamp(0.0, 1.0) * (480.0 - 90.0);
        }
    }
    if child.min_rect().width() > 0.0 { child.min_rect().left() } else { full.right() }
}

/// What a tap zooms to: the desktop says "Click", the iPad "Tap zoom".
fn click_zoom_label(ratio: u32, touch: bool) -> String {
    if touch { format!("Tap zoom {ratio}:1") } else { crate::i18n::tr_format!("Click {}:1", ratio) }
}

fn click_zoom_menu(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    for pct in crate::state::CLICK_ZOOMS {
        if ui.selectable_label(app.ui.click_zoom == pct, format!("{}:1", pct / 100)).clicked() {
            let _ = app.run("view.clickZoom", json!({"ratio": pct / 100}));
        }
    }
}

fn sort_menu(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use lightcraft_catalog::GroupBy;
    use lightcraft_catalog::SortKey::*;
    let cur = app.session.sort;
    ui.label(egui::RichText::new(crate::i18n::tr("Sort by")).weak());
    for (label, key, k) in [
        ("Capture Date", CaptureDate, "captureDate"),
        ("Import Date", ImportDate, "importDate"),
        ("Modified Date", EditDate, "editDate"),
        ("File Name", FileName, "fileName"),
        ("Rating", Rating, "rating"),
        ("File Size", FileSize, "fileSize"),
        ("Random", Random, "random"),
    ] {
        if ui.selectable_label(cur.key == key, crate::i18n::tr(label)).clicked() {
            let _ = app.run("library.sort", json!({"key": k}));
        }
    }
    if cur.key == Random && ui.button(crate::i18n::tr("Reshuffle")).clicked() {
        let _ = app.run("library.shuffle", json!({}));
    }
    // a view that holds its photos shows what matches now when asked to
    if app.session.hold_view {
        let r = ui.button(crate::i18n::tr("Refresh View"));
        crate::widgets::register(ui, "button:refreshView", r.rect);
        if r.clicked() {
            let _ = app.run("library.refresh", json!({}));
        }
    }
    ui.separator();
    // a shuffle has no direction worth choosing
    ui.add_enabled_ui(cur.key != Random, |ui| {
        if ui.selectable_label(cur.key != Random && cur.ascending, crate::i18n::tr("Ascending")).clicked() {
            let _ = app.run("library.sort", json!({"ascending": true}));
        }
        if ui.selectable_label(cur.key != Random && !cur.ascending, crate::i18n::tr("Descending")).clicked() {
            let _ = app.run("library.sort", json!({"ascending": false}));
        }
    });
    ui.separator();
    ui.label(egui::RichText::new(crate::i18n::tr("Group by date")).weak());
    for (label, g, k) in [
        ("Automatic", GroupBy::Auto, "auto"),
        ("Day", GroupBy::Day, "day"),
        ("Month", GroupBy::Month, "month"),
        ("Year", GroupBy::Year, "year"),
        ("None", GroupBy::None, "none"),
    ] {
        if ui.selectable_label(cur.group == g, crate::i18n::tr(label)).clicked() {
            let _ = app.run("library.sort", json!({"group": k}));
        }
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
                    let panel = egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::unique("bottom_bar")).unwrap().outer_rect;
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
            for _ in 0..20 {
                harness.key_press(egui::Key::Tab);
                harness.run_steps(3);
                people_focused = harness.get_by_label("People").is_focused();
                if people_focused {
                    break;
                }
            }
            assert!(people_focused, "Tab reaches People at {scale}x");
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

/// The rating and pick / reject controls as a floating glass capsule over the photo area, for when a tool panel
/// leaves the bottom bar no room for them: the rating must never be out of sight.
pub fn floating_rating(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.ui.rating_floating || !crate::theme::touch_ui(ctx) {
        return;
    }
    let Some(canvas) = app.canvas_rect else { return };
    let Some(photo) = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned()) else { return };
    let size = vec2(PILL_W_TOUCH + 24.0, 46.0);
    // at the top of the photo area: the bottom edge holds the loupe's notices ("Rendering…", "Original offline")
    let pos = pos2(canvas.center().x - size.x / 2.0, canvas.top() + 12.0);
    // draggable: the capsule keeps wherever it is dropped (kept inside the photo area); it starts at the top centre
    egui::Area::new(egui::Id::unique("floating-rating"))
        .order(egui::Order::Foreground)
        .default_pos(pos)
        .movable(true)
        .constrain_to(canvas.shrink(4.0))
        .show(ctx, |ui| {
            crate::glass::shell(egui::Margin::symmetric(10, 7), egui::Margin::ZERO).show(ui, |ui| {
                ui.horizontal(|ui| {
                    // the grip: six dots that say "drag me"
                    let (grip, _) = ui.allocate_exact_size(vec2(14.0, 30.0), Sense::hover());
                    let t = Tokens::get(ui.ctx());
                    for row in 0..3 {
                        for col in 0..2 {
                            let c = pos2(grip.center().x - 3.0 + col as f32 * 6.0, grip.center().y - 7.0 + row as f32 * 7.0);
                            ui.painter().circle_filled(c, 1.6, t.text_dim);
                        }
                    }
                    rating_pill_contents(app, ui, Some(&*photo));
                });
            });
        });
}
