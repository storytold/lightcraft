//! Select groups or individual faces and assign the selection to an existing or new person.
use std::collections::HashSet;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

type FaceKey = (u64, usize);

#[derive(Clone, Debug, Default)]
pub struct View {
    response: Option<Arc<Value>>,
    group: Option<usize>,
    selected: HashSet<FaceKey>,
    anchor: Option<(Option<usize>, usize, bool)>,
    drag: Option<DragSelection>,
    name: String,
    person_search: String,
    last_name: Option<String>,
    error: Option<String>,
    strict: bool,
    restart: bool,
}

#[derive(Clone, Debug)]
struct DragSelection {
    start: egui::Pos2,
    current: egui::Pos2,
    selecting: bool,
    initial: HashSet<FaceKey>,
}

impl View {
    /// Return false at the overview so the caller can leave for People.
    pub(crate) fn back(&mut self) -> bool {
        if self.group.is_none() {
            return false;
        }
        self.group = None;
        self.anchor = None;
        self.drag = None;
        true
    }
    /// Drop cached responses and selections after metadata/history changes, preserving view preferences.
    pub(crate) fn invalidate(&mut self) {
        *self = self.fresh();
    }
    /// Reset transient review state while retaining mode and the last assigned person.
    fn fresh(&self) -> Self {
        Self { strict: self.strict, last_name: self.last_name.clone(), ..Default::default() }
    }
}

/// Extract the catalog photo/region key used by selection state.
fn key(face: &Value) -> Option<FaceKey> {
    Some((face["photo"].as_u64()?, usize::try_from(face["index"].as_u64()?).ok()?))
}

/// Enumerate a group card's members or the single face represented by a review tile.
fn keys(item: &Value, cards: bool) -> impl Iterator<Item = FaceKey> + '_ {
    let faces = if cards { item["faces"].as_array().map(Vec::as_slice).unwrap_or_default() } else { std::slice::from_ref(item) };
    faces.iter().filter_map(key)
}

/// Toggle a tile or repeat the anchor action over a range within the current view.
fn select(view: &mut View, items: &[Value], cards: bool, index: usize, shift: bool) {
    let Some(item) = items.get(index) else { return };
    if let Some((_, anchor, selecting)) = view.anchor.filter(|(scope, _, _)| shift && *scope == view.group) {
        for item in items.get(anchor.min(index)..=anchor.max(index)).unwrap_or_default() {
            for face in keys(item, cards) {
                if selecting {
                    view.selected.insert(face);
                } else {
                    view.selected.remove(&face);
                }
            }
        }
    } else {
        let faces: Vec<_> = keys(item, cards).collect();
        let all = faces.iter().all(|f| view.selected.contains(f));
        for face in faces {
            if all {
                view.selected.remove(&face);
            } else {
                view.selected.insert(face);
            }
        }
        view.anchor = Some((view.group, index, !all));
    }
}

/// Navigate into a group without reselecting faces the user has excluded.
fn review(view: &mut View, groups: &[Value], index: usize) {
    if groups.get(index).is_none() {
        return;
    }
    view.group = Some(index);
    view.anchor = None;
}

/// Lay out the face-review panel inside its standard outer margin.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    egui::Frame::NONE.inner_margin(16).show(ui, |ui| show_inner(app, ui));
}

/// Poll grouping and dispatch explicit assignments and navigation.
fn show_inner(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let Some(mut view) = app.ui.unnamed_groups.take() else { return };
    let mut back = false;
    let mut refresh = false;
    ui.horizontal_wrapped(|ui| {
        let b = ui.button("‹ People");
        register(ui.ctx(), "faceGroups:back", b.rect);
        back = b.clicked();
        ui.heading("Unnamed groups");
        let b = ui.button("Refresh groups");
        register(ui.ctx(), "faceGroups:refresh", b.rect);
        refresh = b.clicked();
        let b = ui.checkbox(&mut view.strict, "Fewer mistakes");
        register(ui.ctx(), "faceGroups:strict", b.rect);
        refresh |= b.on_hover_text("Use a tighter threshold and keep unnamed groups close to their first face").changed();
    });
    if back {
        return;
    }
    if refresh {
        view = view.fresh();
        view.restart = true;
    }
    if view.error.is_none() && view.response.as_ref().is_none_or(|r| r["complete"] != true) {
        let mut params = json!({"restart": view.restart});
        if view.restart || view.response.is_some() {
            params["strict"] = json!(view.strict);
        }
        match app.session.execute("faces.groupUnnamed", &params) {
            Ok(r) => {
                view.restart = false;
                view.strict = r["strict"].as_bool().unwrap_or(view.strict);
                view.response = Some(Arc::new(r));
            }
            Err(e) => view.error = Some(e.to_string()),
        }
        ui.ctx().request_repaint();
    }
    if let Some(error) = &view.error {
        ui.label(error);
        if view.response.is_none() && ui.button("Open face settings").clicked() {
            let _ = app.run("app.settings", json!({"tab": "faces"}));
        }
    }
    let response = view.response.clone().unwrap_or_default();
    if response["complete"] != true && view.error.is_none() {
        ui.label(format!("Grouping similar faces… {} of {}", response["processed"].as_u64().unwrap_or(0), response["total"].as_u64().unwrap_or(0)));
    } else if response["complete"] == true {
        let empty = Vec::new();
        let groups = response["groups"].as_array().unwrap_or(&empty);
        ui.horizontal_wrapped(|ui| {
            if view.group.is_some() {
                let b = ui.button("‹ All groups");
                register(ui.ctx(), "faceGroups:all", b.rect);
                if b.clicked() {
                    view.group = None;
                    view.anchor = None;
                }
            }
            let unindexed = response["unindexed"].as_u64().unwrap_or(0);
            ui.label(format!("{} groups · {unindexed} faces awaiting recognition", groups.len()));
            if app.caches.faces_pending > 0 {
                ui.label(format!("Scanning: {} photos remaining", app.caches.faces_pending));
            }
        });
        let (items, cards) = match view.group {
            Some(i) => (groups.get(i).and_then(|g| g["faces"].as_array()).unwrap_or(&empty).as_slice(), false),
            None => (groups.as_slice(), true),
        };
        let assigned = assignment_bar(app, ui, groups, items, cards, &mut view);
        if let Some(name) = assigned {
            let faces: Vec<_> = groups
                .iter()
                .filter_map(|g| g["faces"].as_array())
                .flatten()
                .filter(|f| key(f).is_some_and(|k| view.selected.contains(&k)))
                .cloned()
                .collect();
            match app.run("faces.nameFaces", json!({"faces": faces, "name": name, "unnamedOnly": true})) {
                Ok(_) => {
                    view = view.fresh();
                    view.last_name = Some(name);
                    // Reuse the grouping, removing accepted members, without interrupting assignment to regroup.
                    view.response = Some(Arc::new(json!({"complete": false})));
                }
                Err(e) => view.error = Some(e),
            }
        } else {
            ui.separator();
            ui.label(if cards {
                "Click groups to select several. Shift-click selects a range. Review opens the faces in a group."
            } else {
                "Click faces to include or exclude them. Shift-click extends that action to a range. Shift-drag a box to exclude faces."
            });
            if items.is_empty() {
                ui.label("No unnamed faces with recognition data. New faces appear after scanning.");
            }
            grid(app, ui, items, cards, &mut view);
        }
    }
    app.ui.unnamed_groups = Some(view);
}

/// Edit cross-group selection and return the person explicitly chosen for assignment.
fn assignment_bar(app: &mut LightcraftApp, ui: &mut egui::Ui, groups: &[Value], items: &[Value], cards: bool, view: &mut View) -> Option<String> {
    let mut assign = None;
    let people = app.caches.person_names(&app.session.catalog);
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("{} faces selected", view.selected.len()));
        let all = !items.is_empty() && items.iter().flat_map(|item| keys(item, cards)).all(|k| view.selected.contains(&k));
        let b = ui.button(if all { "Deselect all" } else { "Select all" });
        register(ui.ctx(), "faceGroups:selectAll", b.rect);
        if b.clicked() {
            for face in items.iter().flat_map(|item| keys(item, cards)) {
                if all {
                    view.selected.remove(&face);
                } else {
                    view.selected.insert(face);
                }
            }
        }
        let b = ui.add_enabled(!view.selected.is_empty(), egui::Button::new("Clear selection"));
        register(ui.ctx(), "faceGroups:clear", b.rect);
        if b.clicked() {
            view.selected.clear();
            view.anchor = None;
        }
    });
    let n = view.selected.len();
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(n > 0 && !people.is_empty(), |ui| {
            let combo = egui::ComboBox::from_id_salt("face-groups-person")
                .width(240.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text("Assign to existing person…")
                .show_ui(ui, |ui| {
                    crate::text_field::TextField::singleline("field:groupPersonSearch", &mut view.person_search)
                        .hint("Find a person…")
                        .width(240.0)
                        .show(ui);
                    let search = view.person_search.trim().to_lowercase();
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for name in people.iter().filter(|name| name.to_lowercase().contains(&search)) {
                            let b = ui.button(name);
                            register(ui.ctx(), format!("faceGroups:person:{name}"), b.rect);
                            if b.clicked() {
                                assign = Some(name.clone());
                                ui.close();
                            }
                        }
                    });
                });
            register(ui.ctx(), "faceGroups:existing", combo.response.rect);
        });
        let field = crate::text_field::TextField::singleline("field:groupName", &mut view.name).hint("Or enter a new name…").width(220.0).show(ui);
        let chosen = view.name.trim();
        let b = ui.add_enabled(n > 0 && !chosen.is_empty(), egui::Button::new(format!("Assign {n} faces")));
        register(ui.ctx(), "faceGroups:name", b.rect);
        if b.clicked() || (n > 0 && !chosen.is_empty() && field.ending == Some(crate::text_field::Ending::Return)) {
            assign = Some(people.iter().find(|p| p.to_lowercase() == chosen.to_lowercase()).cloned().unwrap_or_else(|| chosen.to_string()));
        }
    });
    // Keep common and suggested names one click away; the searchable picker includes every person.
    let mut quick = Vec::new();
    for name in view
        .last_name
        .iter()
        .cloned()
        .chain(
            groups.iter().filter(|g| keys(g, true).any(|k| view.selected.contains(&k))).filter_map(|g| g["suggestion"].as_str().map(str::to_string)),
        )
        .chain(people.iter().cloned())
    {
        if people.contains(&name) && !quick.contains(&name) {
            quick.push(name);
        }
        if quick.len() == 6 {
            break;
        }
    }
    if !quick.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Quick assign:");
            for name in quick {
                let b = ui.add_enabled(n > 0, egui::Button::new(&name));
                register(ui.ctx(), format!("faceGroups:assign:{name}"), b.rect);
                if b.on_hover_text(format!("Assign all {n} selected faces to {name}")).clicked() {
                    assign = Some(name);
                }
            }
        });
    }
    assign
}

/// Request a cached face render using the display rectangle supplied by the engine.
fn paint_face(app: &mut LightcraftApp, ui: &egui::Ui, face: &Value, r: Rect) {
    let Some(photo) = face["photo"].as_u64() else { return };
    let Ok(rect) = serde_json::from_value::<lightcraft_geom::Rect>(face["view"].clone()) else { return };
    if let Some(job) = app.session.face_job(lightcraft_catalog::PhotoId(photo), rect, (r.width() * ui.ctx().pixels_per_point()).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        ui.painter().image(tex.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}

/// Virtualize visible tiles and apply click, range, and marquee selection gestures.
fn grid(app: &mut LightcraftApp, ui: &mut egui::Ui, items: &[Value], cards: bool, view: &mut View) {
    let t = Tokens::get(ui.ctx());
    let edge = super::people::card_edge(app.ui.thumb_size);
    let cols = ((ui.available_width() + 12.0) / (edge + 12.0)).floor().max(1.0) as usize;
    let row_h = edge + if cards { 64.0 } else { 32.0 };
    let mut shown = 0usize;
    let mut open_group = None;
    let gesture = ui.interact(ui.available_rect_before_wrap(), ui.id().with(("face-groups-drag", view.group)), Sense::drag());
    let mut tiles = Vec::new();
    let scroll = egui::ScrollArea::vertical()
        .scroll_source(egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, ..Default::default() })
        .id_salt(("face-groups", cards, view.group))
        .show_rows(ui, row_h, items.len().div_ceil(cols), |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for col in 0..cols {
                        let i = row.saturating_mul(cols).saturating_add(col);
                        let Some(item) = items.get(i) else { break };
                        let (r, _) = ui.allocate_exact_size(vec2(edge, row_h - 8.0), Sense::hover());
                        let image = Rect::from_min_size(r.min, vec2(edge, edge));
                        let resp = ui.interact(image, ui.id().with(("face-group-tile", cards, view.group, i)), Sense::click());
                        tiles.push((image, i));
                        ui.painter().rect_filled(image, 3.0, t.canvas);
                        if cards {
                            let faces = item["faces"].as_array().map(Vec::as_slice).unwrap_or_default();
                            if let Some(face) = faces.first() {
                                paint_face(app, ui, face, image);
                                shown += 1;
                            }
                            let previews = faces.len().saturating_sub(1).min(3);
                            let mini = (edge / 3.0).floor();
                            let bottom = image.bottom() - if item["suggestion"].is_string() { 24.0 } else { 2.0 };
                            for (j, face) in faces.iter().skip(1).take(previews).enumerate() {
                                let tile = Rect::from_min_size(
                                    pos2(image.right() - previews.saturating_sub(j) as f32 * (mini + 2.0), bottom - mini),
                                    vec2(mini, mini),
                                );
                                paint_face(app, ui, face, tile);
                                ui.painter().rect_stroke(tile, 0.0, Stroke::new(1.0, t.canvas), StrokeKind::Inside);
                                shown += 1;
                            }
                        } else {
                            paint_face(app, ui, item, image);
                            shown += 1;
                        }
                        let (count, selected) = keys(item, cards)
                            .fold((0usize, 0usize), |(count, selected), k| (count + 1, selected + usize::from(view.selected.contains(&k))));
                        let label = if cards {
                            if selected == 0 { format!("{count} faces") } else { format!("{selected} of {count} selected") }
                        } else if selected > 0 {
                            "✓ Selected".into()
                        } else {
                            "Not selected".into()
                        };
                        if selected > 0 {
                            ui.painter().rect_stroke(image, 3.0, Stroke::new(2.0, Color32::from_rgb(110, 160, 255)), StrokeKind::Inside);
                            let mark = Rect::from_min_size(image.min + vec2(5.0, 5.0), vec2(22.0, 22.0));
                            ui.painter().rect_filled(mark, 3.0, Color32::from_rgb(50, 100, 200));
                            ui.painter().text(
                                mark.center(),
                                Align2::CENTER_CENTER,
                                if selected == count { "✓" } else { "−" },
                                t.font(16.0),
                                Color32::WHITE,
                            );
                        }
                        if let Some(name) = item["suggestion"].as_str().filter(|_| cards) {
                            let strip = Rect::from_min_max(pos2(image.left(), image.bottom() - 22.0), image.right_bottom());
                            ui.painter().rect_filled(strip, 0.0, Color32::from_black_alpha(190));
                            let name = super::people::fit(ui.painter(), name, t.font(12.0), edge - 8.0);
                            ui.painter().text(strip.center(), Align2::CENTER_CENTER, name, t.font(12.0), Color32::WHITE);
                        }
                        ui.painter().text(pos2(r.left(), image.bottom() + 12.0), Align2::LEFT_CENTER, &label, t.font(12.0), t.text);
                        register(ui.ctx(), format!("faceGroups:{}:{i}", if cards { "group" } else { "face" }), image);
                        crate::widgets::announce_selected(
                            &resp,
                            format!("{} {label}", item["suggestion"].as_str().unwrap_or("Unnamed")),
                            selected > 0,
                        );
                        let mut open = false;
                        if cards {
                            let b = ui
                                .push_id(("face-group-review", i), |ui| {
                                    ui.put(Rect::from_min_size(pos2(r.left(), image.bottom() + 26.0), vec2(edge, 22.0)), egui::Button::new("Review"))
                                })
                                .inner;
                            register(ui.ctx(), format!("faceGroups:review:{i}"), b.rect);
                            open = b.clicked() || resp.double_clicked();
                        }
                        if open {
                            open_group = Some(i);
                        } else if resp.clicked() {
                            select(view, items, cards, i, ui.input(|i| i.modifiers.shift));
                        }
                    }
                });
            }
        });
    if let Some(i) = open_group {
        review(view, items, i);
    }
    // A drag-only background can claim the press immediately. Wait for actual movement
    // so an ordinary Review/face click cannot restore an old selection on release.
    if view.drag.is_none()
        && (gesture.dragged() || gesture.drag_stopped())
        && let Some((start, current)) = ui.input(|i| i.pointer.press_origin().zip(i.pointer.interact_pos()))
        && scroll.inner_rect.contains(start)
        && start.distance(current) >= 6.0
    {
        let started_selected = tiles
            .iter()
            .find(|(r, _)| r.contains(start))
            .and_then(|(_, i)| items.get(*i))
            .is_some_and(|item| keys(item, cards).any(|k| view.selected.contains(&k)));
        view.drag =
            Some(DragSelection { start, current, selecting: !ui.input(|i| i.modifiers.shift) && !started_selected, initial: view.selected.clone() });
        view.anchor = None;
    }
    if let Some(drag) = &mut view.drag {
        if let Some(current) = ui.input(|i| i.pointer.interact_pos()) {
            drag.current = current;
        }
        let selection = Rect::from_two_pos(drag.start, drag.current);
        view.selected.clone_from(&drag.initial);
        for (_, i) in tiles.iter().filter(|(r, _)| r.intersects(selection)) {
            if let Some(item) = items.get(*i) {
                for face in keys(item, cards) {
                    if drag.selecting {
                        view.selected.insert(face);
                    } else {
                        view.selected.remove(&face);
                    }
                }
            }
        }
        let painter = ui.painter().with_clip_rect(scroll.inner_rect);
        painter.rect_filled(selection, 0.0, Color32::from_rgba_unmultiplied(70, 130, 255, 35));
        painter.rect_stroke(selection, 0.0, Stroke::new(1.0, Color32::from_rgb(110, 160, 255)), StrokeKind::Inside);
        if gesture.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            view.drag = None;
        }
        ui.ctx().request_repaint();
    }
    app.renderer.want_variants(shown);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Back must preserve review selection before returning through the overview to People.
    #[test]
    fn face_groups_back_returns_through_overview_and_people() {
        let (mut h, _) = fixture();
        assert!(
            !h.app.widgets.iter().any(|(id, _)| id.starts_with("ignoredFaces:") || id == "faceGroups:ignored" || id == "faceGroups:ignoreSelected")
        );
        for command in ["faces.ignore", "faces.ignored"] {
            assert!(h.app.session.execute(command, &json!({})).unwrap_err().to_string().contains("unknown command"));
        }
        click(&mut h, "faceGroups:group:0");
        click(&mut h, "faceGroups:review:0");
        h.app.run("view.back", json!({})).unwrap();
        let view = h.app.ui.unnamed_groups.as_ref().unwrap();
        assert!(view.group.is_none());
        assert_eq!(view.selected.len(), 2);
        h.app.run("view.back", json!({})).unwrap();
        assert!(h.app.ui.unnamed_groups.is_none());
        assert_eq!(h.app.ui.view, crate::state::ViewMode::People);
    }

    /// Create named and unnamed demo regions with a completed two-group response for UI tests.
    fn fixture() -> (crate::headless::Headless, Vec<lightcraft_catalog::PhotoId>) {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let ids: Vec<_> = app.session.catalog.photos().take(5).map(|p| p.id).collect();
        assert_eq!(ids.len(), 5);
        let rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.6, y1: 0.6 };
        for (i, id) in ids.iter().enumerate() {
            let mut meta = app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = vec![lightcraft_meta::Region {
                rect,
                kind: lightcraft_meta::RegionKind::Face,
                name: (i == 4).then(|| "Existing Person".to_string()),
                description: None,
            }];
            app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
        }
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        h.step();
        h.app.run("view.unnamedGroups", json!({})).unwrap();
        let faces: Vec<_> = ids.iter().take(4).map(|id| json!({"photo": id.0, "index": 0, "rect": rect, "view": rect, "contentHash": h.app.session.catalog.photo(*id).and_then(|p| p.content_hash.as_ref())})).collect();
        h.app.ui.unnamed_groups = Some(View {
            response: Some(Arc::new(json!({"complete": true, "total": 4, "unindexed": 0,
                "groups": [{"faces": &faces[..2], "suggestion": "Existing Person"}, {"faces": &faces[2..]}]}))),
            ..Default::default()
        });
        h.settle(Duration::from_secs(15));
        (h, ids)
    }

    /// Dispatch a registered widget click and advance the headless UI through its result.
    fn click(h: &mut crate::headless::Headless, id: &str) {
        let r = h.request("ui.clickWidget", json!({"id": id}), Duration::from_secs(15));
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }

    /// Optionally export the current synthetic review for visual inspection.
    fn screenshot(h: &mut crate::headless::Headless) {
        if let Some(path) = std::env::var_os("LIGHTCRAFT_GROUPS_SCREENSHOT") {
            let inspected = h.request("ui.inspect", json!({}), Duration::from_secs(15));
            println!("Face grouping review perf: {}", inspected["result"]["perf"]);
            let shot = h.snapshot(Duration::from_secs(15));
            let img = lightcraft_raster::Rgba8::from_fn(shot.size[0], shot.size[1], |x, y| shot.pixels[y * shot.size[0] + x].to_array());
            let png = lightcraft_engine::export::encode_image(
                &img,
                &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
            )
            .unwrap();
            std::fs::write(path, png).unwrap();
        }
    }

    /// One quick assignment must name every selected group as a single undo step.
    #[test]
    fn face_groups_assign_multiple_groups_to_an_existing_person_in_one_click() {
        let (mut h, ids) = fixture();
        click(&mut h, "faceGroups:group:0");
        click(&mut h, "faceGroups:group:1");
        assert_eq!(h.app.ui.unnamed_groups.as_ref().unwrap().selected.len(), 4);
        screenshot(&mut h);
        let before = h.app.session.undo.len();
        click(&mut h, "faceGroups:assign:Existing Person");
        assert_eq!(h.app.session.undo.len(), before + 1);
        for id in ids.iter().take(4) {
            assert_eq!(h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.as_deref(), Some("Existing Person"));
        }
        h.app.run("edit.undo", json!({})).unwrap();
        assert!(ids.iter().take(4).all(|id| h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.is_none()));
    }

    /// Review navigation must preserve exclusions and selections across multiple groups.
    #[test]
    fn face_groups_keep_individual_selections_when_moving_between_groups() {
        let (mut h, ids) = fixture();
        click(&mut h, "faceGroups:review:0");
        assert!(h.app.ui.unnamed_groups.as_ref().unwrap().selected.is_empty(), "Review alone never changes the selection");
        click(&mut h, "faceGroups:all");
        click(&mut h, "faceGroups:group:0");
        click(&mut h, "faceGroups:review:0");
        assert_eq!(h.app.ui.unnamed_groups.as_ref().unwrap().selected, HashSet::from([(ids[0].0, 0), (ids[1].0, 0)]));
        click(&mut h, "faceGroups:face:0");
        assert_eq!(h.app.ui.unnamed_groups.as_ref().unwrap().selected, HashSet::from([(ids[1].0, 0)]), "a face click must exclude exactly that face");
        click(&mut h, "faceGroups:all");
        assert_eq!(
            h.app.ui.unnamed_groups.as_ref().unwrap().selected,
            HashSet::from([(ids[1].0, 0)]),
            "returning to groups must preserve exclusions"
        );
        click(&mut h, "faceGroups:group:1");
        assert_eq!(h.app.ui.unnamed_groups.as_ref().unwrap().selected, HashSet::from([(ids[1].0, 0), (ids[2].0, 0), (ids[3].0, 0)]));
        click(&mut h, "faceGroups:assign:Existing Person");
        assert!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.is_none());
        for id in ids.iter().take(4).skip(1) {
            assert_eq!(h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.as_deref(), Some("Existing Person"));
        }
    }

    /// Searching the person picker must keep it open and assign the complete selection.
    #[test]
    fn face_groups_existing_person_picker_searches_and_assigns_the_whole_selection() {
        let (mut h, ids) = fixture();
        click(&mut h, "faceGroups:selectAll");
        click(&mut h, "faceGroups:existing");
        click(&mut h, "field:groupPersonSearch");
        h.request("ui.text", json!({"text": "Existing"}), Duration::from_secs(15));
        h.step();
        assert!(
            h.app.widgets.iter().any(|(id, _)| id == "faceGroups:person:Existing Person"),
            "the existing person must remain available after typing in the picker"
        );
        click(&mut h, "faceGroups:person:Existing Person");
        for id in ids.iter().take(4) {
            assert_eq!(h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.as_deref(), Some("Existing Person"));
        }
    }

    /// A marquee exclusion must affect reviewed faces without clearing other group selections.
    #[test]
    fn face_groups_shift_drag_excludes_multiple_faces_and_keeps_other_groups_selected() {
        let (mut h, ids) = fixture();
        click(&mut h, "faceGroups:selectAll");
        click(&mut h, "faceGroups:review:0");
        let to = h.app.widgets.iter().find(|(id, _)| id == "faceGroups:face:1").map(|(_, r)| r.center()).unwrap();
        let r = h.request(
            "ui.dragWidget",
            json!({"id": "faceGroups:face:0", "fx": 0.1, "fy": 0.1,
            "toX": to.x, "toY": to.y, "shift": true}),
            Duration::from_secs(15),
        );
        assert_eq!(r["ok"], true, "{r}");
        h.settle(Duration::from_secs(15));
        let selected = &h.app.ui.unnamed_groups.as_ref().unwrap().selected;
        assert_eq!(selected.len(), 2, "the drag excludes the two reviewed faces");
        assert!(!selected.contains(&(ids[0].0, 0)) && !selected.contains(&(ids[1].0, 0)));
        assert!(selected.contains(&(ids[2].0, 0)) && selected.contains(&(ids[3].0, 0)));
        click(&mut h, "faceGroups:assign:Existing Person");
        assert!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.is_none());
        assert!(h.app.session.catalog.photo(ids[1]).unwrap().meta.regions[0].name.is_none());
    }

    /// Shift-click must repeat exclusion, and reopening review must not reselect the range.
    #[test]
    fn face_groups_shift_click_excludes_a_range_after_deselecting_its_first_face() {
        let (mut h, _) = fixture();
        let view = h.app.ui.unnamed_groups.as_mut().unwrap();
        let response = Arc::make_mut(view.response.as_mut().unwrap());
        let faces: Vec<_> = response["groups"].as_array().unwrap().iter().flat_map(|g| g["faces"].as_array().unwrap()).cloned().collect();
        response["groups"] = json!([{"faces": faces}]);
        h.step();
        click(&mut h, "faceGroups:selectAll");
        click(&mut h, "faceGroups:review:0");
        click(&mut h, "faceGroups:face:0");
        h.request("ui.clickWidget", json!({"id": "faceGroups:face:3", "shift": true}), Duration::from_secs(15));
        h.settle(Duration::from_secs(15));
        assert!(h.app.ui.unnamed_groups.as_ref().unwrap().selected.is_empty(), "Shift-click excludes all four faces in the range");
        click(&mut h, "faceGroups:all");
        click(&mut h, "faceGroups:review:0");
        assert!(h.app.ui.unnamed_groups.as_ref().unwrap().selected.is_empty(), "reviewing again must not reselect excluded faces");
    }

    /// Range selection must consistently repeat inclusion or exclusion for cards and faces.
    #[test]
    fn face_groups_shift_selects_ranges_of_groups_and_faces() {
        let items = vec![
            json!({"faces": [{"photo": 1, "index": 0}]}),
            json!({"faces": [{"photo": 2, "index": 0}]}),
            json!({"faces": [{"photo": 3, "index": 0}]}),
        ];
        let mut view = View::default();
        select(&mut view, &items, true, 0, false);
        select(&mut view, &items, true, 2, true);
        assert_eq!(view.selected.len(), 3);
        view.group = Some(0);
        view.anchor = None;
        view.selected.clear();
        let faces = vec![json!({"photo": 1, "index": 0}), json!({"photo": 1, "index": 1}), json!({"photo": 1, "index": 2})];
        select(&mut view, &faces, false, 0, false);
        select(&mut view, &faces, false, 2, true);
        assert_eq!(view.selected.len(), 3);
        select(&mut view, &faces, false, 1, false);
        assert_eq!(view.selected.len(), 2);
        select(&mut view, &faces, false, 2, true);
        assert_eq!(view.selected, HashSet::from([(1, 0)]), "Shift-click repeats exclusion over the range");
    }
}
