//! Actual Remove/Masking right-panel routes with bounded in-memory photos. No app loop,
//! renderer, OS service, native-scope manufacture, forced focus or synthetic SDK intent.
use super::*;
use crate::headless::HeadlessView;
use crate::state::{RightPanel, ToolOverlay};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::collections::VecDeque;

const FACE: &str = "dropdown:toolOverlay";
const PINS: &str = "check:maskPins";
const AFTER: &str = "fixture:after-tool-panel";

#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}

fn owned(id: &str) -> bool {
    id == FACE || id.starts_with("toolOverlay:")
}
fn drain(mut output: egui::FullOutput) {
    output.textures_delta.clear();
    output.platform_output.commands.clear();
    output.viewport_output.clear();
    output.shapes.clear();
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 2],
    probe: Probe,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    trace: VecDeque<String>,
    boundary_clicks: usize,
}
impl Fixture {
    fn new(panel: RightPanel, mode: ToolOverlay) -> Self {
        Self::with_masks(panel, mode, true)
    }
    fn with_masks(panel: RightPanel, mode: ToolOverlay, masks: bool) -> Self {
        let mut s = Session::new();
        let photos = [s.catalog.alloc_photo_id(), s.catalog.alloc_photo_id()];
        for (n, id) in photos.into_iter().enumerate() {
            s.catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        id,
                        Source::Demo { scene: 0 },
                        if n == 0 { "Tool view A.jpg" } else { "Tool view B.jpg" },
                        "JPEG",
                        192,
                        128,
                        "2026-11-21T00:00:00",
                    )),
                })
                .unwrap();
            s.selection = Selection::single(id);
            let mut d = (*s.develop_of(id).unwrap()).clone();
            d.light.exposure = if n == 0 { 0.25 } else { -0.5 };
            s.set_develop(id, d, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(s, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        for id in photos {
            app.run("library.select", json!({"ids":[id.0], "active":id.0})).unwrap();
            if masks {
                app.run("mask.add", json!({"kind":"linear", "name":"Unchanged overlay mask"})).unwrap();
            }
        }
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0], "active":photos[0].0})).unwrap();
        app.run("view.maskOverlayMode", json!({"mode":"color"})).unwrap();
        app.run("view.maskOverlayColor", json!({"color":[11,22,33], "opacity":37})).unwrap();
        app.run("view.maskPins", json!({"show":true})).unwrap();
        app.run("view.toolOverlay", json!({"mode":mode.name()})).unwrap();
        app.run(if panel == RightPanel::Masking { "panel.masking" } else { "panel.remove" }, json!({})).unwrap();
        // A genuine redo entry must survive view-only controls, not just an empty history.
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}}, "label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.redo.is_empty());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, photos, probe: Probe::Real, tick: 0, rects: vec![], hits: vec![], trace: VecDeque::new(), boundary_clicks: 0 };
        let document = f.document();
        f.settle();
        assert_eq!(f.document(), document, "passive actual parent cannot edit model/history");
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert_eq!(self.app.session.catalog.photos().count(), 2);
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(!self.app.session.segmenter.analyzing() && !self.app.session.segmenter.busy());
        let event_trace = format!("{events:?}");
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let probe = self.probe;
        let boundary_clicks = &mut self.boundary_clicks;
        let ctx = self.ctx.clone();
        let mut registries = (vec![], vec![]);
        let output = self.ctx.run_ui(raw, |root| {
            // run_ui can discard and rerun. Drain and check INSIDE every actual pass;
            // retain its final registry without merging or deduplicating passes.
            crate::widgets::take_registries(root.ctx());
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(380.0);
                let mut draw = |ui: &mut egui::Ui| {
                    crate::panels::right::show(app, ui);
                };
                ui.allocate_ui(egui::vec2(380.0, 2350.0), |ui| match probe {
                    Probe::Real => draw(ui),
                    Probe::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Probe::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Probe::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                });
                use craft::ui::contract::{Props as _, Show as _};
                let after = craft::ui::buttons::Button::new("After the actual tool panel").id_salt(AFTER).show(ui);
                crate::widgets::register_response(AFTER, after.response.rect, &after.response);
                if after.clicked() {
                    *boundary_clicks += 1;
                }
            });
            let now = crate::widgets::take_registries(root.ctx());
            for (id, _) in now.0.iter().filter(|(id, _)| owned(id)) {
                assert_eq!(now.0.iter().filter(|(other, _)| other == id).count(), 1, "one actual rect per pass for {id}");
                assert_eq!(now.1.iter().filter(|(other, _)| other == id).count(), 1, "one actual owner per pass for {id}");
            }
            registries = if root.ctx().will_discard() { (vec![], vec![]) } else { now };
        });
        (self.rects, self.hits) = registries;
        drain(output);
        self.trace.push_back(format!(
            "frame={} panel={:?} probe={:?} events={} mode={:?} focus={:?} aliases={:?}",
            self.tick,
            self.app.ui.right,
            self.probe,
            event_trace,
            self.app.ui.tool_overlay,
            craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id),
            self.hits.iter().filter(|(id, _)| owned(id)).map(|(id, hit)| (id, hit.id)).collect::<Vec<_>>()
        ));
        if self.trace.len() > 16 {
            self.trace.pop_front();
        }
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == id)
    }
    fn rect(&self, id: &str) -> Rect {
        let all: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(all.len(), 1, "one locator {id}; trace={:#?}", self.trace);
        all[0].1
    }
    fn hit(&self, id: &str) -> crate::widgets::WidgetHit {
        let all: Vec<_> = self.hits.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(all.len(), 1, "one owner {id}; trace={:#?}", self.trace);
        all[0].1
    }
    fn actual_owner(&self, id: &str) -> egui::Id {
        let hit = self.hit(id);
        let response = self.ctx.read_response(hit.id).unwrap_or_else(|| panic!("SDK owner absent for {id}; trace={:#?}", self.trace));
        assert_eq!(response.rect, self.rect(id), "SDK response geometry, not a hover probe for {id}");
        assert_eq!(response.layer_id, hit.layer_id);
        assert!(response.sense.senses_click() && response.sense.is_focusable());
        hit.id
    }
    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    fn click(&mut self, id: &str) {
        let pos = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.pointer(pos, true);
        self.pointer(pos, false);
        self.settle();
    }
    fn key(&mut self, key: Key) {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        self.settle();
    }
    fn focused(&self, id: &str) -> bool {
        craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == self.hit(id).id)
    }
    fn tab_to(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            if self.focused(id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {id}; trace={:#?}", self.trace);
    }
    fn open(&mut self) {
        self.click(FACE);
        self.assert_open();
    }
    fn assert_open(&self) {
        self.actual_owner(FACE);
        for mode in ToolOverlay::ALL {
            self.actual_owner(&format!("toolOverlay:{}", mode.name()));
        }
        assert_eq!(self.rects.iter().filter(|(id, _)| id.starts_with("toolOverlay:")).count(), 4);
    }
    fn row_owners(&self) -> Vec<egui::Id> {
        ToolOverlay::ALL.into_iter().map(|mode| self.actual_owner(&format!("toolOverlay:{}", mode.name()))).collect()
    }
    fn assert_closed(&self, old: &[egui::Id]) {
        assert!(!self.rects.iter().any(|(id, _)| id.starts_with("toolOverlay:")), "closed menu has no live row aliases; trace={:#?}", self.trace);
        assert!(!self.hits.iter().any(|(id, _)| id.starts_with("toolOverlay:")));
        assert!(
            !craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| old.contains(&rec.stop.id)),
            "closed row must not retain focus; trace={:#?}",
            self.trace
        );
    }
    fn choose(&mut self, mode: ToolOverlay) {
        let before_mode = self.app.ui.tool_overlay;
        let before_toast = self.app.ui.toast.clone();
        self.open();
        assert_eq!(self.app.ui.tool_overlay, before_mode, "opening is not a pick");
        assert_eq!(self.app.ui.toast, before_toast, "opening runs no view command");
        let old = self.row_owners();
        self.click(&format!("toolOverlay:{}", mode.name()));
        self.assert_closed(&old);
        self.assert_mode(mode);
        // The real monotonic app clock advances while actual UI frames are rendered.
        // Even a same-mode pick must refresh the command's toast, not silently skip it.
        assert!(self.app.ui.toast.as_ref().unwrap().1 > before_toast.as_ref().unwrap().1, "every explicit pick reaches the genuine view command");
    }
    fn assert_mode(&mut self, mode: ToolOverlay) {
        assert_eq!(self.app.ui.tool_overlay, mode, "exact explicit mode, never implicit cycle; trace={:#?}", self.trace);
        let caption = format!("Tool Overlay: {}", mode.label());
        assert_eq!(self.app.ui.toast.as_ref().map(|toast| toast.0.as_str()), Some(crate::i18n::tr(&caption)), "real UI command toast");
        let toast = self.app.ui.toast.clone();
        self.settle();
        assert_eq!(self.app.ui.tool_overlay, mode);
        assert_eq!(self.app.ui.toast, toast, "idle frames do not replay a command");
    }
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(),"selection":self.app.session.selection,
            "revision":self.app.session.catalog.revision,"activeMask":self.app.session.active_mask,"activeSpot":self.app.session.active_spot,
            "undo":format!("{:?}",self.app.session.undo),"redo":format!("{:?}",self.app.session.redo),
            "journal":self.app.session.journal,"commits":self.app.session.commits(),"interaction":format!("{:?}",self.app.session.interaction)})
    }
    fn neighbors(&self, allow_pins: bool) -> Value {
        let mut ui = serde_json::to_value(&self.app.ui).unwrap();
        ui.as_object_mut().unwrap().remove("toolOverlay");
        if allow_pins {
            ui.as_object_mut().unwrap().remove("maskPins");
        }
        ui
    }
    fn unchanged(&self, document: &Value, neighbors: &Value, allow_pins: bool) {
        assert_eq!(&self.document(), document, "complete photos/history/Undo/Redo/journal are view-only; trace={:#?}", self.trace);
        assert_eq!(&self.neighbors(allow_pins), neighbors, "other serialized preferences unchanged; trace={:#?}", self.trace);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn tool_overlay_remove_parent_all_exact_modes_keep_real_owners_and_full_document_history() {
    for initial in ToolOverlay::ALL {
        let mut f = Fixture::new(RightPanel::Remove, initial);
        let document = f.document();
        let neighbors = f.neighbors(false);
        f.actual_owner(FACE);
        for mode in ToolOverlay::ALL {
            f.choose(mode);
            f.unchanged(&document, &neighbors, false);
        }
        f.choose(ToolOverlay::Never); // selected mode still runs its exact explicit command
        f.unchanged(&document, &neighbors, false);
    }
}
#[test]
fn tool_overlay_masking_parent_exact_modes_preserve_overlay_color_opacity_pins_and_documents() {
    let mut f = Fixture::new(RightPanel::Masking, ToolOverlay::Never);
    let document = f.document();
    let neighbors = f.neighbors(false);
    assert!(f.has(PINS));
    for mode in [ToolOverlay::Never, ToolOverlay::Always, ToolOverlay::Auto, ToolOverlay::Selected, ToolOverlay::Selected] {
        f.choose(mode);
        f.unchanged(&document, &neighbors, false);
    }
}
#[test]
fn tool_overlay_natural_tab_enter_space_and_row_arrows_use_actual_sdk_focus_owners() {
    for panel in [RightPanel::Remove, RightPanel::Masking] {
        for activate in [Key::Enter, Key::Space] {
            let mut f = Fixture::new(panel, ToolOverlay::Auto);
            let document = f.document();
            let neighbors = f.neighbors(false);
            let toast = f.app.ui.toast.clone();
            f.tab_to(FACE);
            f.key(activate);
            f.assert_open();
            assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
            assert_eq!(f.app.ui.toast, toast);
            assert!(f.focused("toolOverlay:auto"), "selected default stop must be real; trace={:#?}", f.trace);
            f.key(Key::ArrowDown);
            assert!(f.focused("toolOverlay:always"), "real Down; trace={:#?}", f.trace);
            f.key(Key::End);
            assert!(f.focused("toolOverlay:never"), "real End; trace={:#?}", f.trace);
            f.key(Key::Home);
            assert!(f.focused("toolOverlay:auto"), "real Home; trace={:#?}", f.trace);
            f.key(Key::ArrowDown);
            f.key(Key::ArrowDown);
            assert!(f.focused("toolOverlay:selected"));
            assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto, "arrows move focus without applying a mode");
            assert_eq!(f.app.ui.toast, toast);
            let old = f.row_owners();
            f.key(activate);
            f.assert_closed(&old);
            f.assert_mode(ToolOverlay::Selected);
            f.unchanged(&document, &neighbors, false);
        }
    }
}
#[test]
fn tool_overlay_escape_outside_absorption_and_tab_dismiss_without_running_or_retaining_rows() {
    for panel in [RightPanel::Remove, RightPanel::Masking] {
        for dismissal in 0..3 {
            let mut f = Fixture::new(panel, ToolOverlay::Auto);
            let document = f.document();
            let neighbors = f.neighbors(false);
            let toast = f.app.ui.toast.clone();
            f.tab_to(FACE);
            f.key(Key::Enter);
            f.key(Key::ArrowDown);
            let old = f.row_owners();
            match dismissal {
                0 => f.key(Key::Escape),
                1 => {
                    f.click(AFTER);
                    assert_eq!(f.boundary_clicks, 0, "outside click is absorbed, not sent below menu");
                }
                _ => f.key(Key::Tab),
            }
            f.assert_closed(&old);
            f.frame(vec![Event::Text("closed popup input".into())]);
            f.settle();
            f.assert_closed(&old);
            assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
            assert_eq!(f.app.ui.toast, toast);
            f.unchanged(&document, &neighbors, false);
            if dismissal == 1 {
                f.click(AFTER);
                assert_eq!(f.boundary_clicks, 1, "a fresh click reaches the actual underlying SDK button");
            }
            f.choose(ToolOverlay::Never); // dismissal did not strand the opener
            f.unchanged(&document, &neighbors, false);
        }
    }
}
#[test]
fn tool_overlay_masking_real_pins_gate_withdraws_capture_and_returns_a_fresh_closed_menu() {
    let mut f = Fixture::new(RightPanel::Masking, ToolOverlay::Auto);
    let document = f.document();
    let neighbors = f.neighbors(true);
    f.click(PINS);
    assert!(!f.app.ui.mask_pins && !f.has(FACE));
    f.tab_to(PINS);
    f.key(Key::Space);
    assert!(f.app.ui.mask_pins && f.has(FACE));
    f.open();
    let old = f.row_owners();
    let at = f.rect("toolOverlay:never").center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.pointer(at, true);
    assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto, "a held row is not a pick");
    // A real external view command withdraws the actual caller while input is captured.
    f.app.run("view.maskPins", json!({"show":false})).unwrap();
    f.frame(vec![]);
    f.pointer(at, false);
    f.settle();
    assert!(!f.has(FACE));
    f.assert_closed(&old);
    assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
    f.unchanged(&document, &neighbors, true);
    f.tab_to(PINS);
    f.key(Key::Space);
    assert!(f.has(FACE));
    f.assert_closed(&old);
    f.choose(ToolOverlay::Selected);
    f.unchanged(&document, &neighbors, true);
}
#[test]
fn tool_overlay_no_selection_or_empty_masking_parent_has_no_live_controls_or_deferred_pick() {
    let mut empty = Fixture::with_masks(RightPanel::Masking, ToolOverlay::Auto, false);
    let document = empty.document();
    let neighbors = empty.neighbors(false);
    assert!(!empty.has(FACE) && !empty.has(PINS));
    empty.key(Key::Enter);
    empty.unchanged(&document, &neighbors, false);
    for panel in [RightPanel::Remove, RightPanel::Masking] {
        let mut f = Fixture::new(panel, ToolOverlay::Auto);
        f.open();
        let old = f.row_owners();
        let at = f.rect("toolOverlay:never").center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        f.app.run("library.select", json!({"ids":[]})).unwrap();
        let document = f.document();
        let neighbors = f.neighbors(false);
        f.frame(vec![]);
        f.pointer(at, false);
        f.key(Key::Enter);
        f.assert_closed(&old);
        assert!(!f.has(FACE));
        assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
        f.unchanged(&document, &neighbors, false);
        f.app.run("library.select", json!({"ids":[f.photos[0].0],"active":f.photos[0].0})).unwrap();
        f.settle();
        assert!(f.has(FACE));
        f.assert_closed(&old);
    }
}
#[test]
fn tool_overlay_disabled_sizing_and_measurement_cannot_open_pick_or_replay_a_closed_row() {
    for panel in [RightPanel::Remove, RightPanel::Masking] {
        for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
            for opened in [false, true] {
                let mut f = Fixture::new(panel, ToolOverlay::Auto);
                f.tab_to(FACE);
                let old = if opened {
                    f.key(Key::Enter);
                    f.row_owners()
                } else {
                    vec![]
                };
                let document = f.document();
                let neighbors = f.neighbors(false);
                let toast = f.app.ui.toast.clone();
                f.probe = probe;
                f.key(Key::Enter);
                if matches!(probe, Probe::Measure | Probe::Sizing) {
                    assert!(!f.has(FACE));
                } else {
                    let hit = f.hit(FACE);
                    assert!(!f.ctx.read_response(hit.id).unwrap().enabled());
                }
                f.assert_closed(&old);
                assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
                assert_eq!(f.app.ui.toast, toast);
                f.probe = Probe::Real;
                f.settle();
                f.assert_closed(&old);
                f.unchanged(&document, &neighbors, false);
                assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
                assert_eq!(f.app.ui.toast, toast);
                f.choose(ToolOverlay::Always);
                f.unchanged(&document, &neighbors, false);
            }
        }
    }
}
#[test]
fn tool_overlay_captured_rows_cannot_retarget_another_photo_or_the_other_reachable_parent() {
    for panel in [RightPanel::Remove, RightPanel::Masking] {
        for switch_parent in [false, true] {
            let mut f = Fixture::new(panel, ToolOverlay::Auto);
            f.open();
            let old = f.row_owners();
            let at = f.rect("toolOverlay:never").center();
            f.frame(vec![Event::PointerMoved(at)]);
            f.pointer(at, true);
            if switch_parent {
                f.app.run(if panel == RightPanel::Masking { "panel.remove" } else { "panel.masking" }, json!({})).unwrap();
            } else {
                f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
            }
            let document = f.document();
            let neighbors = f.neighbors(false);
            f.frame(vec![]);
            f.pointer(at, false);
            f.settle();
            f.assert_closed(&old);
            assert!(f.has(FACE));
            assert_eq!(f.app.ui.tool_overlay, ToolOverlay::Auto);
            f.unchanged(&document, &neighbors, false);
            f.choose(ToolOverlay::Selected);
            f.unchanged(&document, &neighbors, false);
        }
    }
}
