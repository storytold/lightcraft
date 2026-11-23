//! Real SDK checkbox frames and unchanged app command paths; authored for admitted integration QA.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect, Ui, vec2};

const CONSTRAIN: &str = "check:crop.constrain";
const VISUALIZE: &str = "check:remove.visualizeSpots";
const CATCHLIGHT: &str = "check:redeye.catchlight";

#[derive(Clone, Copy)]
enum Panel { Crop, Remove, Eye }

#[derive(Clone, Copy)]
enum Pass { Normal, NativeSizing, SdkMeasure }

struct Controls {
    app: LightcraftApp,
    ctx: egui::Context,
    photo: PhotoId,
    panel: Panel,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
}

impl Controls {
    fn new(panel: Panel) -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.install_framework(&ctx);
        let photo = app.session.visible_cloned()[0];
        app.run("library.select", json!({"ids": [photo.0], "active": photo.0})).unwrap();
        let mut this = Self { app, ctx, photo, panel, time: 0.0, rects: vec![], hits: vec![] };
        this.settle();
        this
    }

    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        crate::widgets::take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(1000.0, 3000.0), 1.0, self.time, events);
        let app = &mut self.app;
        let photo = self.photo;
        let panel = self.panel;
        let mut draw = |ui: &mut Ui| {
            ui.set_width(280.0);
            match panel {
                Panel::Crop => super::crop(app, ui, photo),
                Panel::Remove => super::remove(app, ui, photo),
                Panel::Eye => super::red_eye(app, ui, photo),
            }
        };
        let mut output = self.ctx.run_ui(raw, |ui| match pass {
            Pass::Normal => draw(ui),
            Pass::NativeSizing => {
                let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                draw(&mut child);
            }
            Pass::SdkMeasure => {
                let ctx = ui.ctx().clone();
                craft::ui::runtime::measure::run(&ctx, || draw(ui));
            }
        });
        output.textures_delta.clear();
        (self.rects, self.hits) = crate::widgets::take_registries(&self.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..3 { self.frame(vec![], Pass::Normal); }
    }

    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == id)
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn response_id(&self, id: &str) -> egui::Id {
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        let response = self.ctx.read_response(hit.id).unwrap();
        assert_eq!(response.rect, self.rect(id), "target must be the SDK response, not a probe");
        assert!(response.sense.senses_click() && response.sense.is_focusable());
        hit.id
    }

    fn press(&mut self, id: &str) -> egui::Pos2 {
        let pos = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(pos)], Pass::Normal);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Pass::Normal);
        pos
    }

    fn release(&mut self, pos: egui::Pos2) {
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Pass::Normal);
        self.settle();
    }

    fn click(&mut self, id: &str) {
        let pos = self.press(id);
        self.release(pos);
    }

    fn focus(&self, id: &str) {
        let response_id = self.response_id(id);
        self.ctx.memory_mut(|m| m.request_focus(response_id));
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }], Pass::Normal);
        }
        self.settle();
    }

    fn eyes(&mut self) {
        // Ensure a known empty start even if a future demo gains eye corrections.
        self.app.run("develop.merge", json!({"settings": {"red_eye": []}})).unwrap();
        for (x, pet) in [(0.25, false), (0.65, true)] {
            self.app.run("redeye.add", json!({"center": [x, 0.4], "rx": 0.03, "ry": 0.03, "pet": pet})).unwrap();
        }
        self.app.ui.eye = 1;
        self.settle();
    }
}

#[test]
fn crop_checkboxes_external155_constrain_pointer_keyboard_and_single_undo() {
    let mut h = Controls::new(Panel::Crop);
    h.response_id(CONSTRAIN);
    let before = h.app.session.develop_of(h.photo).unwrap();
    let undo = h.app.session.undo.len();
    let history = h.app.session.catalog.photo(h.photo).unwrap().history.len();
    let pos = h.press(CONSTRAIN);
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before, "press alone cannot dispatch");
    h.release(pos);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one release is one edit");
    assert_eq!(h.app.session.undo.last().unwrap().label, "Constrain Crop");
    assert_eq!(h.app.session.catalog.photo(h.photo).unwrap().history.len(), history + 1);
    let mut expected = (*before).clone();
    expected.geometry.constrain_crop = !before.geometry.constrain_crop;
    assert_eq!(h.app.session.develop_of(h.photo).unwrap().as_ref(), &expected, "only the requested setting changed");
    assert_eq!(h.app.session.undo_step().unwrap(), "Constrain Crop");
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
    h.settle();
    h.focus(CONSTRAIN);
    h.key(Key::Space);
    assert_eq!(h.app.session.undo.len(), undo + 1);
    assert_eq!(h.app.session.develop_of(h.photo).unwrap().as_ref(), &expected);
}

#[test]
fn crop_checkboxes_external155_visualize_is_ui_only_for_pointer_and_keyboard() {
    let mut h = Controls::new(Panel::Remove);
    h.response_id(VISUALIZE);
    let before = h.app.session.develop_of(h.photo).unwrap();
    let history = h.app.session.catalog.photo(h.photo).unwrap().history.clone();
    let undo = h.app.session.undo.len();
    let redo = h.app.session.redo.len();
    let original = h.app.ui.visualize_spots;
    let threshold = h.app.ui.spots_threshold;
    h.click(VISUALIZE);
    assert_eq!(h.app.ui.visualize_spots, !original);
    h.focus(VISUALIZE);
    h.key(Key::Space);
    assert_eq!(h.app.ui.visualize_spots, original);
    assert_eq!(h.app.ui.spots_threshold, threshold);
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
    assert_eq!(h.app.session.catalog.photo(h.photo).unwrap().history, history);
    assert_eq!((h.app.session.undo.len(), h.app.session.redo.len()), (undo, redo));
}

#[test]
fn crop_checkboxes_external155_catchlight_is_pet_only_and_uses_selected_index() {
    let mut h = Controls::new(Panel::Eye);
    h.eyes();
    h.response_id(CATCHLIGHT);
    let before = h.app.session.develop_of(h.photo).unwrap();
    let undo = h.app.session.undo.len();
    h.click(CATCHLIGHT);
    let changed = h.app.session.develop_of(h.photo).unwrap();
    assert!(changed.red_eye[1].catchlight.is_some());
    assert_eq!(changed.red_eye[0], before.red_eye[0], "selected index 1 must not affect red eye 0");
    let mut expected = (*before).clone();
    expected.red_eye[1].catchlight = Some(lightcraft_geom::Point::new(-0.35, -0.35));
    assert_eq!(changed.as_ref(), &expected);
    assert_eq!(h.app.session.undo.len(), undo + 1);
    assert_eq!(h.app.session.undo_step().unwrap(), "Catchlight");
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
    h.settle();
    h.focus(CATCHLIGHT);
    h.key(Key::Space);
    assert_eq!(h.app.session.develop_of(h.photo).unwrap().as_ref(), &expected);
    h.key(Key::Space);
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before, "second activation turns catchlight off");
    h.app.ui.eye = 0;
    h.settle();
    assert!(!h.has(CATCHLIGHT), "non-pet selected eye must not expose the checkbox");
    let undo = h.app.session.undo.len();
    h.key(Key::Space);
    assert_eq!(h.app.session.undo.len(), undo, "hidden old target cannot toggle a pet eye");
    assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
    h.app.run("develop.merge", json!({"settings": {"red_eye": []}})).unwrap();
    h.settle();
    assert!(!h.has(CATCHLIGHT), "empty eye list exposes no stale catchlight target");
}

#[test]
fn crop_checkboxes_external155_both_measurement_modes_publish_no_checkbox_targets_or_edits() {
    for (panel, id) in [(Panel::Crop, CONSTRAIN), (Panel::Remove, VISUALIZE), (Panel::Eye, CATCHLIGHT)] {
        let mut h = Controls::new(panel);
        if matches!(panel, Panel::Eye) { h.eyes(); }
        for pass in [Pass::NativeSizing, Pass::SdkMeasure] {
            h.settle();
            h.focus(id);
            let before = h.app.session.develop_of(h.photo).unwrap();
            let history = h.app.session.catalog.photo(h.photo).unwrap().history.clone();
            let ui_before = h.app.ui.visualize_spots;
            let undo = h.app.session.undo.len();
            h.frame(vec![Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], pass);
            for target in [CONSTRAIN, VISUALIZE, CATCHLIGHT] {
                assert!(!h.has(target), "speculative pass published {target}");
                assert!(!h.hits.iter().any(|(name, _)| name == target));
            }
            assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
            assert_eq!(h.app.session.catalog.photo(h.photo).unwrap().history, history);
            assert_eq!(h.app.ui.visualize_spots, ui_before);
            assert_eq!(h.app.session.undo.len(), undo);
            h.frame(vec![Event::Key { key: Key::Space, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }], pass);
            assert_eq!(h.app.session.develop_of(h.photo).unwrap(), before);
            assert_eq!(h.app.ui.visualize_spots, ui_before);
        }
    }
}
