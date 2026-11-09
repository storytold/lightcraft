//! Model-independent review and restoration of persistent ignored face crops.
use std::collections::HashSet;

use egui::{Color32, Rect, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::{LightcraftApp, widgets::register};

const PAGE_SIZE: usize = 1000;

#[derive(Clone, Debug, Default)]
pub struct View {
    faces: Option<std::sync::Arc<Vec<Value>>>,
    views: Vec<Option<lightcraft_geom::Rect>>,
    total: usize,
    offset: usize,
    selected: HashSet<(u64, usize)>,
    revision: Option<u64>,
    index_stamp: Option<(u64, u64)>,
    anchor: Option<(usize, bool)>,
    error: Option<String>,
}

impl View {
    fn set_faces(&mut self, faces: Vec<Value>) {
        self.views = faces.iter().map(|face| serde_json::from_value(face["view"].clone()).ok()).collect();
        self.faces = Some(std::sync::Arc::new(faces));
    }

    /// Refresh after catalog/history changes and discard potentially stale selection keys.
    pub(crate) fn invalidate(&mut self) {
        *self = Self::default();
    }

    /// Move to another bounded page, dropping selections whose review entries are no longer shown.
    fn page(&mut self, offset: usize) {
        self.faces = None;
        self.views.clear();
        self.selected.clear();
        self.anchor = None;
        self.error = None;
        self.offset = offset;
    }
}

/// Read a retained catalog region's stable review key.
fn key(face: &Value) -> Option<(u64, usize)> {
    Some((face["photo"].as_u64()?, usize::try_from(face["index"].as_u64()?).ok()?))
}

/// Show retained crops without loading a recognition model or constructing unnamed groups.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let Some(mut view) = app.ui.ignored_faces.take() else { return };
    let t = crate::theme::Tokens::get(ui.ctx());
    let index_stamp = (app.caches.faces_epoch, app.caches.faces_indexed);
    let regions_changed = view.revision.is_some_and(|revision| {
        revision != app.session.catalog.revision
            && app
                .session
                .catalog
                .aspects_since(revision)
                .is_none_or(|aspects| aspects.any_of(lightcraft_catalog::Aspects::META.with(lightcraft_catalog::Aspects::PRESENCE)))
    });
    let crops_changed = view.index_stamp.is_some_and(|stamp| stamp != index_stamp);
    if regions_changed || crops_changed {
        // Refetch on catalog changes, but only discard selection when the displayed entries
        // change. Background detections elsewhere must not move the review back to page one.
        match app.session.execute("faces.ignored", &json!({"offset": view.offset, "limit": PAGE_SIZE})) {
            Ok(response) => {
                let faces = response["faces"].as_array().cloned().unwrap_or_default();
                view.total = response["total"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or_default();
                if view.faces.as_deref() != Some(&faces) {
                    view.selected.clear();
                    view.anchor = None;
                }
                view.set_faces(faces);
                view.error = None;
                if view.offset >= view.total && view.offset > 0 {
                    view.page(view.total.saturating_sub(1) / PAGE_SIZE * PAGE_SIZE);
                }
            }
            Err(error) => {
                view.selected.clear();
                view.anchor = None;
                view.faces = None;
                view.error = Some(error.to_string());
            }
        }
    }
    view.revision = Some(app.session.catalog.revision);
    view.index_stamp = Some(index_stamp);
    let mut back = false;
    egui::Frame::NONE.inner_margin(16).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let button = crate::widgets::arrow_button(ui, crate::i18n::tr("People"), crate::icons::Icon::ChevronLeft, true);
            register(ui, "ignoredFaces:back", button.rect);
            back = button.clicked();
            ui.heading(crate::i18n::tr("Ignored faces"));
            let button = ui.button(crate::i18n::tr("Refresh ignored faces"));
            register(ui, "ignoredFaces:refresh", button.rect);
            if button.clicked() {
                view.invalidate();
            }
        });
        if view.faces.is_none() && view.error.is_none() {
            view.revision = Some(app.session.catalog.revision);
            match app.session.execute("faces.ignored", &json!({"offset": view.offset, "limit": PAGE_SIZE})) {
                Ok(response) => {
                    view.total = response["total"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or_default();
                    view.set_faces(response["faces"].as_array().cloned().unwrap_or_default());
                }
                Err(error) => view.error = Some(error.to_string()),
            }
        }
        if let Some(error) = &view.error {
            ui.label(error);
        }
        let faces = view.faces.clone().unwrap_or_default();
        let mut restore = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::i18n::tr_format!("Ignored faces: {total} · Selected: {selected}", total = view.total, selected = view.selected.len()));
            let button = ui.button(crate::i18n::tr("Select all"));
            register(ui, "ignoredFaces:selectAll", button.rect);
            if button.clicked() {
                view.selected.extend(faces.iter().filter_map(key));
            }
            let button = ui.button(crate::i18n::tr("Clear selection"));
            register(ui, "ignoredFaces:clear", button.rect);
            if button.clicked() {
                view.selected.clear();
                view.anchor = None;
            }
            let button = ui.add_enabled(!view.selected.is_empty(), egui::Button::new(crate::i18n::tr("Restore selected faces")));
            register(ui, "ignoredFaces:restore", button.rect);
            restore = button.clicked();
        });
        let mut requested_page = None;
        if view.total > PAGE_SIZE {
            ui.horizontal(|ui| {
                let previous = ui.add_enabled(view.offset > 0, egui::Button::new(crate::i18n::tr("Previous page")));
                register(ui, "ignoredFaces:previous", previous.rect);
                if previous.clicked() {
                    requested_page = Some(view.offset.saturating_sub(PAGE_SIZE));
                }
                ui.label(format!("{} {} / {}", crate::i18n::tr("Page"), view.offset / PAGE_SIZE + 1, view.total.div_ceil(PAGE_SIZE)));
                let next = ui.add_enabled(view.offset.saturating_add(faces.len()) < view.total, egui::Button::new(crate::i18n::tr("Next page")));
                register(ui, "ignoredFaces:next", next.rect);
                if next.clicked() {
                    requested_page = Some(view.offset.saturating_add(PAGE_SIZE));
                }
            });
        }
        if let Some(offset) = requested_page {
            view.page(offset);
            return;
        }
        ui.label(crate::i18n::tr(
            "Click faces to select or deselect. Shift-click extends that action to a range. Restoration keeps the original name and box.",
        ));
        if restore {
            let selected: Vec<_> = faces.iter().filter(|f| key(f).is_some_and(|k| view.selected.contains(&k))).cloned().collect();
            match app.run("faces.ignore", json!({"faces": selected, "ignored": false})) {
                Ok(_) => {
                    // Keep the review's place; the next frame clamps a now-empty last page.
                    view.page(view.offset);
                }
                Err(error) => view.error = Some(error),
            }
            return;
        }
        if faces.is_empty() {
            ui.label(crate::i18n::tr("No ignored faces."));
        }
        let edge = super::people::card_edge(app.ui.thumb_size);
        let cols = ((ui.available_width() + 12.0) / (edge + 12.0)).floor().max(1.0) as usize;
        let mut shown = 0;
        egui::ScrollArea::vertical().show_rows(ui, edge + 32.0, faces.len().div_ceil(cols), |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let index = row.saturating_mul(cols).saturating_add(col);
                        let Some(face) = faces.get(index) else { break };
                        let Some(face_key) = key(face) else { continue };
                        let (area, _) = ui.allocate_exact_size(vec2(edge, edge + 24.0), Sense::hover());
                        let image = Rect::from_min_size(area.min, vec2(edge, edge));
                        let response = ui.interact(image, ui.scope_id().with(("ignored-face", face_key)), Sense::click());
                        register(ui, format!("ignoredFaces:face:{index}"), image);
                        if let Some(rect) = view.views.get(index).copied().flatten()
                            && let Some(job) = app.session.face_job(
                                lightcraft_catalog::PhotoId(face_key.0),
                                rect,
                                (edge * ui.ctx().pixels_per_point()).ceil() as usize,
                            )
                            && let Some(texture) = app.renderer.variant(job)
                        {
                            ui.painter().image(texture.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        shown += 1;
                        if response.clicked() {
                            let (start, selecting) =
                                view.anchor.filter(|_| ui.input(|i| i.modifiers.shift)).unwrap_or((index, !view.selected.contains(&face_key)));
                            for key in faces.get(start.min(index)..=start.max(index)).unwrap_or_default().iter().filter_map(key) {
                                if selecting {
                                    view.selected.insert(key);
                                } else {
                                    view.selected.remove(&key);
                                }
                            }
                            if !ui.input(|i| i.modifiers.shift) || view.anchor.is_none() {
                                view.anchor = Some((index, selecting));
                            }
                        }
                        let selected = view.selected.contains(&face_key);
                        if selected {
                            super::grid::selection_ring(ui.painter(), &t, image, lightcraft_engine::SelectionState::Selected);
                        }
                        crate::widgets::announce_selected(&response, crate::i18n::tr("Ignored face"), selected);
                        ui.painter().text(
                            pos2(area.left(), image.bottom() + 12.0),
                            egui::Align2::LEFT_CENTER,
                            if selected { crate::i18n::tr("✓ Selected") } else { crate::i18n::tr("Not selected") },
                            t.font(12.0),
                            t.text,
                        );
                    }
                });
            }
        });
        app.renderer.want_variants(shown);
    });
    if !back {
        app.ui.ignored_faces = Some(view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_face_errors_include_codes_and_fields_without_changing_the_string() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        h.step();
        let timeout = std::time::Duration::from_secs(15);
        for method in ["engine.execute", "ui.menu.invoke", "command"] {
            let response = h.request(method, json!({"command": "faces.ignore", "params": {"faces": []}}), timeout);
            assert_eq!(response["ok"], false);
            assert!(response["error"].as_str().unwrap().contains("select between"));
            assert_eq!(response["errorDetails"], json!({"code": "invalidInput", "field": "faces"}));
        }
        let response = h.request("engine.execute", json!({"command": "no.such"}), timeout);
        assert_eq!(response["ok"], false);
        assert!(response.get("errorDetails").is_none());
    }

    #[test]
    fn ignored_navigation_discards_old_destinations_but_invalid_person_preserves_review() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        let id = app.session.catalog.photos().next().unwrap().id.0;
        app.ui.person_from = Some((Some("Earlier".into()), id));
        app.ui.unnamed_selected.insert((id, 0));
        app.ui.unnamed_anchor = Some(42);
        app.run("view.ignoredFaces", json!({})).unwrap();
        assert!(app.ui.person_from.is_none());
        assert!(app.ui.unnamed_selected.is_empty());
        assert!(app.ui.unnamed_anchor.is_none());
        app.ui.ignored_faces.as_mut().unwrap().offset = PAGE_SIZE;
        for params in [json!({}), json!({"name": "   "}), json!({"name": 5})] {
            assert!(app.run("view.person", params).is_err());
            assert_eq!(app.ui.ignored_faces.as_ref().unwrap().offset, PAGE_SIZE);
        }
        app.run("view.detail", json!({})).unwrap();
        app.run("view.back", json!({})).unwrap();
        assert_eq!(app.ui.view, crate::state::ViewMode::PhotoGrid);
        app.run("view.person", json!({"name": "Person"})).unwrap();
        assert!(app.ui.ignored_faces.is_none());
    }

    fn screenshot_if_requested(h: &mut crate::headless::Headless, variable: &str) {
        let Some(path) = std::env::var_os(variable) else { return };
        let timeout = std::time::Duration::from_secs(15);
        let shot = h.snapshot(timeout);
        let image = lightcraft_raster::Rgba8::from_fn(shot.size[0], shot.size[1], |x, y| shot.pixels[y * shot.size[0] + x].to_array());
        let png = lightcraft_engine::export::encode_image(
            &image,
            &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
        )
        .unwrap();
        std::fs::write(path, png).unwrap();
    }

    /// The primary workflow: select an unnamed range, ignore it in one step, and restore it.
    #[test]
    fn unnamed_range_can_be_ignored_and_restored_as_batches() {
        use lightcraft_meta::{Region, RegionKind};
        for language in ["english", "german"] {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
            app.run(&format!("app.language.{language}"), json!({})).unwrap();
            let id = app.session.catalog.photos().next().unwrap().id;
            let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
            meta.regions = (0..3)
                .map(|i| Region {
                    rect: lightcraft_geom::Rect { x0: 0.1 + f64::from(i) * 0.2, y0: 0.2, x1: 0.25 + f64::from(i) * 0.2, y1: 0.5 },
                    kind: RegionKind::Face,
                    name: None,
                    description: Some("retained".into()),
                })
                .collect();
            let original = meta.regions.clone();
            app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
            // At this width the naming controls must wrap for Ignore to remain clickable.
            let mut h = crate::headless::Headless::new(app, crate::window_place::MIN_SIZE, 1.0);
            let timeout = std::time::Duration::from_secs(15);
            h.step();
            h.app.run("view.people", json!({})).unwrap();
            h.settle(timeout);
            for (index, shift) in [(0, false), (2, true)] {
                let r = h.request("ui.clickWidget", json!({"id": format!("unnamed-face:{}:{index}", id.0), "shift": shift}), timeout);
                assert_eq!(r["ok"], true, "{r}");
                h.step();
            }
            assert_eq!(h.app.ui.unnamed_selected.len(), 3);
            screenshot_if_requested(
                &mut h,
                if language == "german" { "LIGHTCRAFT_UNNAMED_GERMAN_SCREENSHOT" } else { "LIGHTCRAFT_UNNAMED_SCREENSHOT" },
            );
            let before = h.app.session.undo.len();
            assert_eq!(h.request("ui.clickWidget", json!({"id": "unnamed:ignore"}), timeout)["ok"], true);
            h.step();
            assert_eq!(h.app.session.undo.len(), before + 1);
            assert!(h.app.ui.unnamed_selected.is_empty());
            assert!(h.app.session.catalog.photo(id).unwrap().meta.regions.iter().all(|r| r.kind == RegionKind::IgnoredFace));
            assert_eq!(h.app.session.execute("faces.unnamed", &json!({})).unwrap()["total"], 0);
            h.app.run("edit.undo", json!({})).unwrap();
            assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions, original);
            h.app.run("edit.redo", json!({})).unwrap();
            h.app.run("view.ignoredFaces", json!({})).unwrap();
            h.settle(timeout);
            screenshot_if_requested(&mut h, "LIGHTCRAFT_IGNORED_BATCH_SCREENSHOT");
            let before = h.app.session.undo.len();
            for widget in ["ignoredFaces:selectAll", "ignoredFaces:restore"] {
                let r = h.request("ui.clickWidget", json!({"id": widget}), timeout);
                assert_eq!(r["ok"], true, "{r}");
                h.step();
            }
            assert_eq!(h.app.session.undo.len(), before + 1);
            assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions, original);
            h.app.run("app.language.english", json!({})).unwrap();
        }
    }

    /// Removing a region must not transfer its selection to the face that takes its index.
    #[test]
    fn unnamed_selection_is_cleared_before_an_index_can_be_reused() {
        use lightcraft_meta::{Region, RegionKind};
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        let id = app.session.catalog.photos().next().unwrap().id;
        let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = (0..2)
            .map(|i| Region {
                rect: lightcraft_geom::Rect { x0: 0.1 + f64::from(i) * 0.3, y0: 0.2, x1: 0.3 + f64::from(i) * 0.3, y1: 0.5 },
                kind: RegionKind::Face,
                name: None,
                description: None,
            })
            .collect();
        app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = std::time::Duration::from_secs(15);
        h.step();
        h.app.run("view.people", json!({})).unwrap();
        h.settle(timeout);
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("unnamed-face:{}:0", id.0)}), timeout)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.unnamed_selected.len(), 1);
        h.app.session.execute("photo.removeRegion", &json!({"id": id.0, "index": 0})).unwrap();
        // Inject an in-flight stale review: prevent the usual refresh for this click. The
        // unnamed action must send its old reviewed box, not read the replacement's box.
        h.app.caches.unnamed.as_mut().unwrap().0 = h.app.session.catalog.revision;
        let before = (h.app.session.catalog.revision, h.app.session.undo.len());
        assert_eq!(h.request("ui.clickWidget", json!({"id": "unnamed:ignore"}), timeout)["ok"], true);
        h.step();
        assert_eq!((h.app.session.catalog.revision, h.app.session.undo.len()), before);
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].kind, RegionKind::Face);
        h.app.caches.unnamed.as_mut().unwrap().0 = h.app.session.catalog.revision.wrapping_sub(1);
        h.step();
        assert!(h.app.ui.unnamed_selected.is_empty());
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].kind, RegionKind::Face);
        assert!(!h.app.widgets.iter().any(|(id, _)| id == "unnamed:ignore"));
    }

    fn check_range_clear_and_refresh(h: &mut crate::headless::Headless, timeout: std::time::Duration) {
        for (index, shift) in [(0, false), (2, true)] {
            assert_eq!(h.request("ui.clickWidget", json!({"id": format!("ignoredFaces:face:{index}"), "shift": shift}), timeout)["ok"], true);
            h.step();
        }
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().selected.len(), 3);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:clear"}), timeout)["ok"], true);
        h.step();
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().selected.is_empty());
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().anchor.is_none());
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:face:0"}), timeout)["ok"], true);
        h.step();
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:refresh"}), timeout)["ok"], true);
        h.step();
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().selected.is_empty());
    }

    fn check_external_shrink(h: &mut crate::headless::Headless, id: lightcraft_catalog::PhotoId, timeout: std::time::Duration) {
        // An external mutation (not Restore's own reset) shrinks the last page and takes
        // the revision branch's clamp back to the final valid page.
        h.app.session.execute("edit.undo", &json!({})).unwrap();
        h.step();
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:next"}), timeout)["ok"], true);
        h.step();
        h.app.session.execute("photo.removeRegion", &json!({"id": id.0, "index": PAGE_SIZE})).unwrap();
        h.step();
        let view = h.app.ui.ignored_faces.as_ref().unwrap();
        assert_eq!((view.offset, view.total), (0, PAGE_SIZE));
        assert_eq!(view.faces.as_ref().unwrap().len(), PAGE_SIZE);
        assert!(view.selected.is_empty());
    }

    /// A library over one page remains reviewable, and changing pages cannot restore stale selections.
    #[test]
    fn ignored_review_pages_and_restores_the_last_face() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let id = app.session.catalog.photos().next().unwrap().id;
        let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
        let region = lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.6, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::IgnoredFace,
            name: None,
            description: None,
        };
        meta.regions = vec![region; PAGE_SIZE + 1];
        app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = std::time::Duration::from_secs(15);
        h.step();
        h.app.run("view.people", json!({})).unwrap();
        h.settle(timeout);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "unnamed:ignored"}), timeout)["ok"], true);
        h.settle(timeout);
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().len(), PAGE_SIZE);
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().total, PAGE_SIZE + 1);
        check_range_clear_and_refresh(&mut h, timeout);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:selectAll"}), timeout)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().selected.len(), PAGE_SIZE);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:next"}), timeout)["ok"], true);
        h.step();
        let view = h.app.ui.ignored_faces.as_ref().unwrap();
        assert_eq!(view.offset, PAGE_SIZE);
        assert_eq!(view.faces.as_ref().unwrap().len(), 1);
        assert!(view.selected.is_empty());
        for (widget, offset) in [("ignoredFaces:previous", 0), ("ignoredFaces:next", PAGE_SIZE)] {
            assert_eq!(h.request("ui.clickWidget", json!({"id": widget}), timeout)["ok"], true);
            h.step();
            assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().offset, offset);
            assert!(h.app.ui.ignored_faces.as_ref().unwrap().selected.is_empty());
        }
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:selectAll"}), timeout)["ok"], true);
        h.step();
        h.app.session.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();
        h.step();
        let view = h.app.ui.ignored_faces.as_ref().unwrap();
        assert_eq!(view.offset, PAGE_SIZE, "an unrelated catalog revision keeps the page");
        assert_eq!(view.selected.len(), 1, "and its selection");
        let other = h.app.session.catalog.photos().find(|p| p.id != id).unwrap().id;
        let mut meta = h.app.session.catalog.photo(other).unwrap().meta.clone();
        let mut detected = h.app.session.catalog.photo(id).unwrap().meta.regions[0].clone();
        detected.kind = lightcraft_meta::RegionKind::Face;
        meta.regions.push(detected);
        h.app.session.commit("background detections", lightcraft_catalog::Op::SetMeta { id: other, meta: Box::new(meta) }).unwrap();
        h.step();
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().offset, PAGE_SIZE);
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().selected.len(), 1);
        if let Some(path) = std::env::var_os("LIGHTCRAFT_IGNORED_PAGED_SCREENSHOT") {
            let shot = h.snapshot(timeout);
            let image = lightcraft_raster::Rgba8::from_fn(shot.size[0], shot.size[1], |x, y| shot.pixels[y * shot.size[0] + x].to_array());
            let png = lightcraft_engine::export::encode_image(
                &image,
                &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
            )
            .unwrap();
            std::fs::write(path, png).unwrap();
        }
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:selectAll"}), timeout)["ok"], true);
        h.step();
        assert_eq!(h.request("ui.clickWidget", json!({"id": "ignoredFaces:restore"}), timeout)["ok"], true);
        h.step();
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[PAGE_SIZE].kind, lightcraft_meta::RegionKind::Face);
        let view = h.app.ui.ignored_faces.as_ref().unwrap();
        assert_eq!(view.total, PAGE_SIZE);
        assert_eq!(view.offset, 0);
        assert_eq!(view.faces.as_ref().unwrap().len(), PAGE_SIZE);
        check_external_shrink(&mut h, id, timeout);
    }

    /// Restore a retained assignment through the actual panel without any recognition model.
    #[test]
    fn ignored_review_restores_without_a_model_and_back_returns_to_people() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let id = app.session.catalog.photos().next().unwrap().id;
        let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
        let rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.6, y1: 0.6 };
        meta.regions =
            vec![lightcraft_meta::Region { rect, kind: lightcraft_meta::RegionKind::IgnoredFace, name: Some("Person".into()), description: None }];
        app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        let timeout = std::time::Duration::from_secs(15);
        h.step();
        h.app.run("app.language.german", json!({})).unwrap();
        h.app.run("view.people", json!({})).unwrap();
        h.settle(timeout);
        assert!(h.painted_text().iter().any(|text| text == "Ignorierte Gesichter…"));
        assert_eq!(h.request("ui.clickWidget", json!({"id": "unnamed:ignored"}), timeout)["ok"], true);
        h.settle(timeout);
        for label in [
            "Ignorierte Gesichter",
            "Ignorierte Gesichter aktualisieren",
            "Ignorierte Gesichter: 1 · Ausgewählt: 0",
            "Ausgewählte Gesichter wiederherstellen",
        ] {
            assert!(h.painted_text().iter().any(|text| text == label), "missing translated label: {label}");
        }
        for widget in ["ignoredFaces:selectAll", "ignoredFaces:restore"] {
            let response = h.request("ui.clickWidget", json!({"id": widget}), timeout);
            assert_eq!(response["ok"], true, "{response}");
            h.step();
            if widget == "ignoredFaces:selectAll" {
                assert!(h.painted_text().iter().any(|text| text == "Ignorierte Gesichter: 1 · Ausgewählt: 1"));
            }
        }
        let region = &h.app.session.catalog.photo(id).unwrap().meta.regions[0];
        assert_eq!(region.kind, lightcraft_meta::RegionKind::Face);
        assert_eq!(region.name.as_deref(), Some("Person"));
        assert_eq!(region.rect, rect);
        assert_eq!(crate::i18n::history_label("Ignore Faces", &[]), "Gesichter ignorieren");
        assert_eq!(crate::i18n::history_label("Restore Ignored Faces", &[]), "Ignorierte Gesichter wiederherstellen");
        h.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].kind, lightcraft_meta::RegionKind::IgnoredFace);
        h.step();
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().len(), 1);
        h.app.run("edit.redo", json!({})).unwrap();
        h.step();
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().is_empty());
        h.app.run("edit.undo", json!({})).unwrap();
        h.step();
        // Direct session calls bypass the UI command hook, including library membership changes.
        h.app.session.selection = lightcraft_engine::Selection::single(id);
        let selected = h.request("ui.clickWidget", json!({"id": "ignoredFaces:selectAll"}), timeout);
        assert_eq!(selected["ok"], true);
        h.step();
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().selected.len(), 1);
        h.app.session.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
        h.step();
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().is_empty());
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().selected.is_empty());
        h.app.session.execute("photo.restore", &json!({"ids": [id.0]})).unwrap();
        h.step();
        assert_eq!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().len(), 1);
        h.app.session.execute("faces.ignore", &json!({"faces": [{"photo": id.0, "index": 0, "rect": rect}], "ignored": false})).unwrap();
        h.step();
        assert!(h.app.ui.ignored_faces.as_ref().unwrap().faces.as_ref().unwrap().is_empty());
        h.app.session.execute("edit.undo", &json!({})).unwrap();
        h.step();
        if let Some(path) = std::env::var_os("LIGHTCRAFT_IGNORED_SCREENSHOT") {
            let inspected = h.request("ui.inspect", json!({}), timeout);
            println!("Ignored-face review perf: {}", inspected["result"]["perf"]);
            let shot = h.snapshot(timeout);
            let image = lightcraft_raster::Rgba8::from_fn(shot.size[0], shot.size[1], |x, y| shot.pixels[y * shot.size[0] + x].to_array());
            let png = lightcraft_engine::export::encode_image(
                &image,
                &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
            )
            .unwrap();
            std::fs::write(path, png).unwrap();
        }
        h.app.run("view.back", json!({})).unwrap();
        assert!(h.app.ui.ignored_faces.is_none());
        h.app.run("app.language.english", json!({})).unwrap();
    }
}
