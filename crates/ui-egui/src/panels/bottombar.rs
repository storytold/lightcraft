//! The bottom bar: view modes, sort, rating/flags pill, copy/paste settings, zoom and view toggles.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::Flag;
use serde_json::json;

use crate::LccApp;
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
    ("people", Icon::People, ViewMode::People, "People"),
];

/// Where the bar keeps the room it had last frame for the Compact Auto Sync switch's name: the
/// points left over with every other item in its full form and the switch unlabelled.
const SWITCH_ROOM: &str = "bottomBarAutoSyncRoom";

/// Whether the Compact Auto Sync switch carries its name: when the name fits next to every other
/// item of the bar in its full form (the room measured last frame, see [`SWITCH_ROOM`]); otherwise
/// the switch alone (its tooltip names it), so the rest of the bar keeps its room.
fn auto_sync_labelled(room: Option<f32>, label_extra: f32) -> bool {
    room.is_some_and(|room| room.is_finite() && room >= label_extra)
}

/// How much wider the Auto Sync switch is with its name: the name, measured, and its padding.
fn auto_sync_label_extra(ui: &egui::Ui, t: &Tokens) -> f32 {
    let label = crate::i18n::tr("Auto Sync");
    ui.painter().layout_no_wrap(label.to_string(), t.font(12.0), t.text_label).size().x + SWITCH_LABEL_PAD
}

/// The labelled switch is its name plus this much wider than the bare switch.
const SWITCH_LABEL_PAD: f32 = 8.0;
/// The switch box's corner radius, the bar's icon buttons' own.
pub(crate) const SWITCH_RADIUS: f32 = 4.0;

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("bottom_bar")
        .exact_size(t.bottom_bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let compact_zoom = full.width() < 360.0;
            let room_id = egui::Id::new(SWITCH_ROOM);
            let extra = auto_sync_label_extra(ui, &t);
            let labelled = auto_sync_labelled(ui.ctx().data(|d| d.get_temp::<f32>(room_id)), extra);
            let (right_start, label_drawn) = right_side(app, ui, full, compact_zoom, labelled);
            // where the right group would start with the switch unlabelled
            let right_start_bare = if label_drawn { right_start + extra } else { right_start };
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
                        let (sep, _) = ui.allocate_exact_size(vec2(1.0, 22.0), Sense::hover());
                        ui.painter().rect_filled(sep, 0.0, t.button_border);
                        ui.add_space(10.0);
                        // Lightroom Classic's Grid toolbar: Painter, then Sort by name (the fork's)
                        let library_grid = library_grid(app);
                        if library_grid
                            && icon_button(ui, "painter", Icon::Brush, vec2(30.0, 30.0), app.ui.lib.painter.is_some(), true, "Painter (⌥⌘K)")
                                .clicked()
                        {
                            let _ = app.run("tool.painter", json!({}));
                        }
                        if library_grid && left_rect.width() >= 520.0 {
                            ui.add_space(6.0);
                            let label = format!("{}: {}", crate::i18n::tr("Sort"), crate::i18n::tr(sort_key_label(app.session.sort.key)));
                            let sort = crate::widgets::dropdown(ui, "sortLabel", &label, t.font(12.5), t.text_label);
                            egui::Popup::menu(&sort).show(|ui| sort_menu(app, ui));
                        } else {
                            let sort = icon_button(ui, "sort", Icon::Sort, vec2(30.0, 30.0), false, true, "Sort");
                            egui::Popup::menu(&sort).show(|ui| sort_menu(app, ui));
                        }
                    } else {
                        let current_icon = VIEW_MODES.iter().find(|(_, _, mode, _)| *mode == app.ui.view).map_or(Icon::More, |(_, icon, _, _)| *icon);
                        let more = icon_button(ui, "viewOptions", current_icon, vec2(40.0, 32.0), true, true, "View");
                        paint(
                            ui.painter(),
                            Rect::from_center_size(more.rect.right_bottom() - vec2(5.0, 5.0), vec2(7.0, 7.0)),
                            Icon::ChevronDown,
                            t.text,
                        );
                        egui::Popup::menu(&more).show(|ui| {
                            for &(id, _, mode, label) in VIEW_MODES {
                                if ui.selectable_label(app.ui.view == mode, crate::i18n::tr(label)).clicked() {
                                    let _ = app.run(&format!("view.{id}"), json!({}));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            crate::widgets::menu_button(ui, crate::i18n::tr("Sort"), |ui| sort_menu(app, ui));
                            if compact_zoom && matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare) {
                                crate::widgets::menu_button(ui, crate::i18n::tr_format!("Click {}:1", app.ui.click_zoom / 100), |ui| {
                                    click_zoom_menu(app, ui)
                                });
                            }
                        });
                    }
                })
                .response
                .rect
                .right();
            centre(app, ui, full, left_end + 12.0, right_start - 12.0);
            // the room the switch's name would have next frame: what is left between the side groups,
            // the switch unlabelled, once the left group and the centre have their full forms
            let left_full = right_start_bare - 12.0 - full.left() >= if library_grid(app) { 520.0 } else { 270.0 };
            let room = if left_full { right_start_bare - 12.0 - (left_end + 12.0) - centre_full_width(app, ui) } else { f32::NEG_INFINITY };
            ui.ctx().data_mut(|d| d.insert_temp(room_id, room));
            if auto_sync_labelled(Some(room), extra) != labelled {
                ui.ctx().request_repaint();
            }
        });
}

/// The centre's width in its full form: the rating/flag pill, then (outside the Library) the copy /
/// paste settings buttons.
fn centre_full_width(app: &LccApp, ui: &egui::Ui) -> f32 {
    let (pill_w, copy) = centre_widths(app, ui);
    if app.ui.module == crate::module::ModuleId::Library { pill_w } else { pill_w + copy }
}

/// The rating/flag pill's width and the copy / paste settings buttons' (with their gaps).
fn centre_widths(app: &LccApp, ui: &egui::Ui) -> (f32, f32) {
    let t = Tokens::get(ui.ctx());
    let label = crate::i18n::tr(if app.session.clipboard.is_some() { "Paste Edit Settings" } else { "Copy Edit Settings" });
    let copy_width = (ui.painter().layout_no_wrap(label.to_string(), t.font(13.0), t.text_label).size().x + 24.0).max(136.0);
    let pill_w = if library_grid(app) { PILL_W + LABELS_W } else { PILL_W };
    (pill_w, 10.0 + copy_width + 4.0 + 30.0)
}

/// Five 19-point stars, three 22-point flags, seven 3-point gaps, a 6-point group gap
/// and 10-point padding at both ends.
const PILL_W: f32 = 208.0;
/// The colour labels after the flags in the Library's Grid, as in Lightroom Classic: a 6-point gap and five
/// 12-point swatches 3 points apart.
const LABELS_W: f32 = 6.0 + 5.0 * 12.0 + 5.0 * 3.0;

/// The Library's Grid, where the toolbar carries Lightroom Classic's Grid items (the fork's).
fn library_grid(app: &LccApp) -> bool {
    app.ui.module == crate::module::ModuleId::Library && matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid)
}

/// The name of a sort key, as the Sort menu lists it.
fn sort_key_label(key: lcc_catalog::SortKey) -> &'static str {
    use lcc_catalog::SortKey::*;
    match key {
        CaptureDate => "Capture Date",
        ImportDate => "Import Date",
        EditDate => "Modified Date",
        FileName => "File Name",
        Rating => "Rating",
        FileSize => "File Size",
        Random => "Random",
        Custom => "Custom Order",
        Quality => "Quality",
    }
}

/// The rating/flag pill and copy/paste settings, between `from` and `to` (the side groups): the
/// copy buttons go first when there is no room (they are in the Edit menu too), then the pill.
fn centre(app: &mut LccApp, ui: &mut egui::Ui, full: Rect, from: f32, to: f32) {
    let t = Tokens::get(ui.ctx());
    let has_clip = app.session.clipboard.is_some();
    let label = crate::i18n::tr(if has_clip { "Paste Edit Settings" } else { "Copy Edit Settings" });
    let labels = library_grid(app);
    let (pill_w, copy_w) = centre_widths(app, ui);
    let copy_width = copy_w - 10.0 - 4.0 - 30.0;
    let centre_width = pill_w + copy_w;
    let room = to - from;
    if room < pill_w {
        return;
    }
    // Lightroom Classic's Grid toolbar has no copy buttons: they stay in Develop's (and the Photo menu)
    let with_copy = room >= centre_width && app.ui.module != crate::module::ModuleId::Library;
    let w = if with_copy { centre_width } else { pill_w };
    // where it sits with room to spare (slightly left of centre), kept between the side groups
    let left = (full.center().x - 60.0 - pill_w / 2.0).clamp(from, to - w);
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned());
    let pill = Rect::from_min_size(pos2(left, full.center().y - 15.0), vec2(pill_w, 30.0));
    ui.painter().rect_filled(pill, t.radius(15.0), t.canvas);
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
        let (r, resp) = pill_ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
        crate::widgets::announce_selected(
            &resp,
            crate::i18n::tr(match f {
                Flag::None => "Unflagged",
                Flag::Pick => "Pick",
                Flag::Reject => "Reject",
            }),
            flag == f,
        );
        register(pill_ui.ctx(), format!("flag:{id}"), r);
        let on = flag == f && f != Flag::None;
        let c = if on {
            if f == Flag::Reject { t.reject } else { t.pick }
        } else if resp.hovered() {
            t.text
        } else {
            t.icon
        };
        // the Pick button shows the flag the photo carries: filled while it is a pick
        let icon = if on && f == Flag::Pick { Icon::FlagPickFilled } else { icon };
        paint(pill_ui.painter(), r.shrink(3.0), icon, c);
        if resp.clicked() {
            let mut params = json!({"flag": match f { Flag::Pick => "pick", Flag::Reject => "reject", Flag::None => "none" }});
            crate::panels::compare::target_active(app, &mut params);
            let _ = app.run("photo.flag", params);
        }
    }
    // the colour labels, after the flags (Lightroom Classic's Grid toolbar)
    if labels {
        use lcc_catalog::ColorLabel as L;
        pill_ui.add_space(6.0);
        let current = active.as_ref().and_then(|p| p.label);
        for (l, key, name) in [
            (L::Red, "red", "Red"),
            (L::Yellow, "yellow", "Yellow"),
            (L::Green, "green", "Green"),
            (L::Blue, "blue", "Blue"),
            (L::Purple, "purple", "Purple"),
        ] {
            let (r, resp) = pill_ui.allocate_exact_size(vec2(12.0, 12.0), Sense::click());
            crate::widgets::announce_selected(&resp, crate::i18n::tr(name), current == Some(l));
            register(pill_ui.ctx(), format!("toolbarLabel:{key}"), r);
            let c = crate::theme::label_color(l);
            pill_ui.painter().rect_filled(r.shrink(1.0), 2.0, if current == Some(l) || resp.hovered() { c } else { c.gamma_multiply(0.75) });
            if current == Some(l) {
                pill_ui.painter().rect_stroke(r, t.radius(3.0), egui::Stroke::new(1.5, t.text), egui::StrokeKind::Outside);
            }
            if resp.clicked() {
                // a click on the photo's own label takes it off, as the label shortcuts do
                let mut params = json!({"label": if current == Some(l) { "none" } else { key }});
                crate::panels::compare::target_active(app, &mut params);
                let _ = app.run("photo.label", params);
            }
        }
    }
    // copy / paste settings
    if !with_copy {
        return;
    }
    let copy_r = Rect::from_min_size(pos2(pill.right() + 10.0, pill.top()), vec2(copy_width, 30.0));
    let cresp = ui.interact(copy_r, egui::Id::new("copy-settings"), Sense::click());
    crate::widgets::announce(&cresp, label.to_string());
    register(ui.ctx(), "button:copySettings", copy_r);
    ui.painter().rect_filled(copy_r, t.radius(15.0), if cresp.hovered() { t.hover } else { t.canvas });
    ui.painter().text(copy_r.center(), Align2::CENTER_CENTER, label, t.font(13.0), if active.is_some() { t.text_label } else { t.text_disabled });
    if cresp.clicked() && active.is_some() {
        let _ = if has_clip { app.run("develop.paste", json!({})) } else { app.run("develop.copy", json!({})) };
        let msg = if has_clip { "Settings pasted" } else { "Edit settings copied" };
        app.toast(ui.ctx(), crate::i18n::tr(msg));
    }
    let gear_r = Rect::from_min_size(pos2(copy_r.right() + 4.0, pill.top()), vec2(30.0, 30.0));
    let gresp = ui.interact(gear_r, egui::Id::new("copy-gear"), Sense::click());
    crate::widgets::announce(&gresp, crate::i18n::tr("Copy Settings…"));
    register(ui.ctx(), "icon:copyGear", gear_r);
    ui.painter().rect_filled(gear_r, t.radius(15.0), if gresp.hovered() { t.hover } else { t.canvas });
    paint(ui.painter(), gear_r.shrink(7.0), Icon::Gear, t.icon);
    if gresp.clicked() {
        let _ = app.run("dialog.copySettings", json!({}));
    }
}

/// The right-hand group (zoom, view toggles or thumbnail size), the Auto Sync switch named when
/// `labelled`; returns its left edge and whether the switch was drawn with its name.
fn right_side(app: &mut LccApp, ui: &mut egui::Ui, full: Rect, compact_zoom: bool, labelled: bool) -> (f32, bool) {
    let t = Tokens::get(ui.ctx());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(
                pos2((full.right() - if app.compact() { 540.0 } else { 360.0 }).max(full.left()), full.top()),
                full.right_bottom(),
            ))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    // Compact: Auto Sync at the far right in the library, so one glance says whether an edit goes to
    // every selected photo (gone on a very narrow bar, its name too when it does not fit: Edit ▸ Auto
    // Sync and the switch under the Edit panel are always there)
    let mut label_drawn = false;
    if app.compact() && !app.importing() && full.width() >= 360.0 {
        let on = app.session.auto_sync;
        label_drawn = labelled;
        if auto_sync_switch(&mut child, &t, on, labelled).clicked()
            && let Err(e) = app.run("develop.autoSync", json!({"on": !on}))
        {
            app.toast_error(child.ctx(), e);
        }
        child.add_space(10.0);
        let (sep, _) = child.allocate_exact_size(vec2(1.0, 22.0), Sense::hover());
        child.painter().rect_filled(sep, 0.0, t.button_border);
        child.add_space(10.0);
    }
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
        // no zoom in Survey; the filmstrip size takes the thumbnail slider's place
        survey_film_slider(app, &mut child, &t);
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
        let zr = crate::widgets::dropdown(&mut child, "zoom", crate::i18n::tr(&zoom_label), t.font(13.0), t.text_label);
        egui::Popup::menu(&zr).show(|ui| {
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
        if !compact_zoom {
            child.add_space(6.0);
            let cz = crate::widgets::dropdown(
                &mut child,
                "clickZoom",
                &crate::i18n::tr_format!("Click {}:1", app.ui.click_zoom / 100),
                t.font(13.0),
                t.text_label,
            );
            egui::Popup::menu(&cz).show(|ui| click_zoom_menu(app, ui));
        }
    } else {
        // Lightroom Classic's Grid toolbar, right to left: thumbnail size, slideshow, previous / next, rotate (the fork's)
        thumb_slider(app, &mut child, &t);
        // (only with room for them: a narrow window keeps the slider and the centre)
        if library_grid(app) && full.width() >= 760.0 {
            child.add_space(8.0);
            for (id, icon, tip, command) in [
                ("slideshowPlay", Icon::Play, "Impromptu Slideshow", "view.slideshow"),
                ("navNext", Icon::ChevronRight, "Next Photo", "library.next"),
                ("navPrevious", Icon::ChevronLeft, "Previous Photo", "library.previous"),
                ("rotateRight", Icon::RotateRight, "Rotate Right", "photo.rotateRight"),
                ("rotateLeft", Icon::RotateLeft, "Rotate Left", "photo.rotateLeft"),
            ] {
                if icon_button(&mut child, id, icon, vec2(26.0, 26.0), false, true, tip).clicked() {
                    let _ = app.run(command, json!({}));
                }
            }
        }
    }
    (if child.min_rect().width() > 0.0 { child.min_rect().left() } else { full.right() }, label_drawn)
}

/// The Compact bottom bar's Auto Sync switch (widget `toggle:autoSync`): its name (when `labelled`)
/// and a small switch, filled with the accent colour while on.
fn auto_sync_switch(ui: &mut egui::Ui, t: &Tokens, on: bool, labelled: bool) -> egui::Response {
    let label = crate::i18n::tr("Auto Sync");
    let text = if on { Color32::WHITE } else { t.text_label };
    let g = ui.painter().layout_no_wrap(if labelled { label.to_string() } else { String::new() }, t.font(12.0), text);
    let knob_w = 22.0;
    let w = if labelled { g.size().x + SWITCH_LABEL_PAD + knob_w + 14.0 } else { knob_w + 14.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label));
    register(ui.ctx(), "toggle:autoSync", r);
    let p = ui.painter();
    let fill = if on {
        t.accent
    } else if resp.hovered() {
        t.hover
    } else {
        t.button
    };
    // rounded as the bar's other buttons are (`icon_button`)
    p.rect_filled(r, SWITCH_RADIUS, fill);
    p.rect_stroke(r, SWITCH_RADIUS, Stroke::new(1.0, if on { t.accent } else { t.button_border }), StrokeKind::Inside);
    if labelled {
        p.galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, text);
    }
    // the switch: a track with its knob at the right when on
    let track = Rect::from_center_size(pos2(r.right() - 8.0 - knob_w / 2.0, r.center().y), vec2(knob_w, 12.0));
    p.rect_filled(track, 6.0, if on { Color32::from_white_alpha(90) } else { t.track });
    let kx = if on { track.right() - 6.0 } else { track.left() + 6.0 };
    p.circle_filled(pos2(kx, track.center().y), 5.0, if on { Color32::WHITE } else { t.thumb });
    let tip = if on { "Auto Sync is on: edits change every selected photo" } else { "Auto Sync is off: edits change the active photo only" };
    resp.on_hover_text(crate::i18n::tr(tip))
}

/// Survey's filmstrip size slider (widget `slider:filmSize`): the filmstrip's height, the one its
/// top edge drags and the loupe views share, offered between 100 and 240 points.
fn survey_film_slider(app: &mut LccApp, child: &mut egui::Ui, t: &Tokens) {
    const RANGE: (f32, f32) = (100.0, 240.0);
    let (r, resp) = child.allocate_exact_size(vec2(110.0, 20.0), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Filmstrip size"));
    register(child.ctx(), "slider:filmSize", r);
    let p = child.painter();
    let y = r.center().y;
    p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], egui::Stroke::new(2.0, t.track));
    let f = ((app.ui.film_height() - RANGE.0) / (RANGE.1 - RANGE.0)).clamp(0.0, 1.0);
    p.rect_filled(Rect::from_center_size(pos2(r.left() + f * r.width(), y), vec2(3.0, 14.0)), 1.0, t.thumb_hover);
    if let Some(pp) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let f = ((pp.x - r.left()) / r.width()).clamp(0.0, 1.0);
        app.ui.film_height = crate::state::FILM_HEIGHT.clamp(RANGE.0 + f * (RANGE.1 - RANGE.0));
    }
}

/// The thumbnail size slider at the right end of a grid's toolbar.
fn thumb_slider(app: &mut LccApp, child: &mut egui::Ui, t: &Tokens) {
    {
        // thumbnail size slider
        let (r, resp) = child.allocate_exact_size(vec2(110.0, 20.0), Sense::click_and_drag());
        crate::widgets::announce(&resp, crate::i18n::tr("Thumbnail size"));
        register(child.ctx(), "slider:thumbSize", r);
        let p = child.painter();
        let y = r.center().y;
        p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], egui::Stroke::new(2.0, t.track));
        let (min, max) = (crate::state::THUMB_SIZE_MIN, crate::state::THUMB_SIZE_MAX);
        let f = (app.ui.thumb_size - min) / (max - min);
        let x = r.left() + f * r.width();
        p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(3.0, 14.0)), 1.0, t.thumb_hover);
        if let Some(pp) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            app.ui.thumb_size = min + ((pp.x - r.left()) / r.width()).clamp(0.0, 1.0) * (max - min);
        }
    }
}

fn click_zoom_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    for pct in crate::state::CLICK_ZOOMS {
        if ui.selectable_label(app.ui.click_zoom == pct, format!("{}:1", pct / 100)).clicked() {
            let _ = app.run("view.clickZoom", json!({"ratio": pct / 100}));
        }
    }
}

fn sort_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    use lcc_catalog::GroupBy;
    use lcc_catalog::SortKey::*;
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
        ("Quality", Quality, "quality"),
    ] {
        if ui.selectable_label(cur.key == key, crate::i18n::tr(label)).clicked() {
            let _ = app.run("library.sort", json!({"key": k}));
        }
    }
    // Custom Order: the order set by hand in a regular album (drag photos in the grid)
    let in_album = crate::panels::grid::custom_order_album(app).is_some();
    ui.add_enabled_ui(in_album, |ui| {
        if ui.selectable_label(cur.key == Custom && in_album, crate::i18n::tr("Custom Order")).clicked() {
            let _ = app.run("library.sort", json!({"key": "custom"}));
        }
    });
    if cur.key == Random && ui.button(crate::i18n::tr("Reshuffle")).clicked() {
        let _ = app.run("library.shuffle", json!({}));
    }
    // a view that holds its photos shows what matches now when asked to
    if app.session.hold_view {
        let r = ui.button(crate::i18n::tr("Refresh View"));
        crate::widgets::register(ui.ctx(), "button:refreshView", r.rect);
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
                    let mut app = LccApp::new(lcc_engine::Session::new(), Default::default());
                    app.ui.view = view;
                    let mut ready = false;
                    let mut harness =
                        egui_kittest::Harness::builder().with_size(vec2(width, 100.0)).with_pixels_per_point(scale).wgpu().build_ui_state(
                            move |ui, app: &mut LccApp| {
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
                    let stem = format!("app-bottom-{view:?}-{width}-{scale}x");
                    harness.render().unwrap().save(directory.join(format!("{stem}.png"))).unwrap();
                    let panel = egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("bottom_bar")).unwrap().outer_rect;
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
                    let mut app = LccApp::new(lcc_engine::Session::new(), Default::default());
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
            let app = LccApp::new(lcc_engine::Session::new(), Default::default());
            let mut ready = false;
            let mut harness = egui_kittest::Harness::builder().with_size(vec2(280.0, 640.0)).with_pixels_per_point(scale).build_ui_state(
                move |ui, app: &mut LccApp| {
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
