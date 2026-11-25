//! Actual common right-panel Red/Pet route. Tiny memory records; no app/render loop,
//! native-scope manufacture, forced focus, synthetic SDK intent or substituted parent.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const PUPIL: &str = "redEye.pupilSize";
const DARKEN: &str = "redEye.darken";
const CATCH: &str = "check:redeye.catchlight";
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
fn owned(id: &str) -> bool {
    id.contains(":redEye.") || id == CATCH || id.starts_with("button:eyeMode-") || matches!(id, "button:eyeDelete" | "button:eyeDeleteAll")
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
    records: BTreeMap<String, Arc<craft::ui::runtime::inspect::Recorded>>,
    trace: VecDeque<String>,
}
impl Fixture {
    fn new(index: usize, eyes: bool) -> Self {
        let mut s = Session::new();
        let photos = [s.catalog.alloc_photo_id(), s.catalog.alloc_photo_id()];
        for (n, id) in photos.into_iter().enumerate() {
            s.catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        id,
                        Source::Demo { scene: 0 },
                        if n == 0 { "Red/Pet A.jpg" } else { "Other B.jpg" },
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
            app.run("library.select", json!({"ids":[id.0],"active":id.0})).unwrap();
            if eyes {
                for (x, pet) in [(0.25, false), (0.65, true)] {
                    app.run("redeye.add", json!({"center":[x,0.4],"rx":0.03,"ry":0.02,"pet":pet})).unwrap();
                }
            }
        }
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("panel.redeye", json!({})).unwrap();
        app.ui.eye = index; // initial genuine selected-correction state, before any input/parent frame
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}},"label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.redo.is_empty());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f =
            Self { app, ctx, photos, probe: Probe::Real, tick: 0, rects: vec![], hits: vec![], records: BTreeMap::new(), trace: VecDeque::new() };
        let before = f.document();
        f.settle();
        assert_eq!(f.document(), before, "passive actual parent changes no model/history");
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert_eq!(self.app.session.catalog.photos().count(), 2);
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        let input = format!("{events:?}");
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
        let ctx = self.ctx.clone();
        let mut final_registry = (vec![], vec![]);
        let output = self.ctx.run_ui(raw, |root| {
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
            });
            let registry = crate::widgets::take_registries(root.ctx());
            for (name, _) in registry.0.iter().filter(|(name, _)| owned(name)) {
                assert_eq!(registry.0.iter().filter(|(other, _)| other == name).count(), 1, "one rect per ACTUAL pass: {name}");
                assert_eq!(
                    registry.1.iter().filter(|(other, _)| other == name).count(),
                    usize::from(!name.starts_with("sliderThumb:")),
                    "one actual owner per pass: {name}"
                );
            }
            final_registry = if root.ctx().will_discard() { (vec![], vec![]) } else { registry };
        });
        (self.rects, self.hits) = final_registry;
        drain(output);
        self.records.clear();
        if let Some(photo) = self.app.session.active() {
            for control in [PUPIL, DARKEN] {
                let key = egui::Id::unique(("red-eye-field-record", photo, self.app.ui.eye, control));
                if let Some(record) = craft::ui::runtime::inspect::recorded(&self.ctx, key) {
                    self.records.insert(control.into(), record);
                }
            }
        }
        self.trace.push_back(format!(
            "frame={} probe={probe:?} input={input} active={:?} eye={} dragging={:?} interaction={:?} focus={:?} aliases={:?}",
            self.tick,
            self.app.session.active(),
            self.app.ui.eye,
            self.app.ui.dragging_control,
            self.app.session.interaction,
            craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| record.stop.id),
            self.hits
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
        if id.contains(":redEye.") {
            let record = self.records.get(id.split_once(':').unwrap().1).unwrap();
            let role = if id.starts_with("slider:") {
                "Slider"
            } else if id.starts_with("sliderValue:") {
                "SpinButton"
            } else {
                "Label"
            };
            let all: Vec<_> = record
                .elements
                .iter()
                .enumerate()
                .filter(|(_, element)| element.id == hit.id && element.facts.as_ref().is_some_and(|facts| facts.role == role))
                .collect();
            assert_eq!(all.len(), 1, "actual SDK {role} owner, not proxy: {id}; trace={:#?}", self.trace);
            let (index, element) = all[0];
            let mut rect = record.stop_rect(index).unwrap();
            if role == "Slider" {
                let thumb = record
                    .hits
                    .iter()
                    .find(|part| part.owner == element.id && part.part.is_some_and(|part| part.key == craft::ui::value_drag::Slider::THUMB))
                    .unwrap();
                rect = rect.shrink2(egui::vec2((thumb.bounds.width() * 0.5).min(rect.width() * 0.5), 0.0));
                assert_eq!(self.rect(&id.replace("slider:", "sliderThumb:")), thumb.bounds);
            }
            assert_eq!(self.rect(id), rect, "exact recorded child geometry for {id}");
            assert!(hit.interact_rect.contains(rect.center()), "real clipped interaction contains locator centre");
        } else {
            let response = self.ctx.read_response(hit.id).unwrap();
            assert_eq!(response.rect, self.rect(id));
            assert_eq!(response.layer_id, hit.layer_id);
            assert!(response.sense.senses_click() && response.sense.is_focusable());
        }
        hit.id
    }
    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    fn click(&mut self, id: &str) {
        self.tick += 30;
        let pos = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.pointer(pos, true);
        self.pointer(pos, false);
        self.settle();
    }
    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn tab_to(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|record| record.stop.id == self.hit(id).id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab cannot reach {id}; trace={:#?}", self.trace);
    }
    fn draft(&mut self, control: &str, text: &str) {
        self.tab_to(&format!("sliderValue:{control}"));
        self.key_mod(Key::A, Modifiers::COMMAND);
        self.frame(vec![Event::Text(text.into())]);
    }
    fn type_value(&mut self, control: &str, text: &str) {
        self.draft(control, text);
        self.key(Key::Enter);
    }
    fn begin_drag(&mut self, control: &str, fraction: f32) -> Pos2 {
        self.tick += 30;
        let thumb = self.rect(&format!("sliderThumb:{control}")).center();
        let span = self.rect(&format!("slider:{control}"));
        let end = egui::pos2(span.left() + span.width() * fraction, span.center().y);
        self.frame(vec![Event::PointerMoved(thumb)]);
        self.pointer(thumb, true);
        self.frame(vec![Event::PointerMoved(thumb.lerp(end, 0.5))]);
        self.frame(vec![Event::PointerMoved(end)]);
        end
    }
    fn double_thumb(&mut self, control: &str) {
        self.tick += 30;
        let pos = self.rect(&format!("sliderThumb:{control}")).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        for _ in 0..2 {
            self.pointer(pos, true);
            self.pointer(pos, false);
            self.frame(vec![]);
        }
        self.settle();
    }
    fn develop(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.photos[0]).unwrap()).clone()
    }
    fn metadata(&self) -> Value {
        let mut photo = serde_json::to_value(self.app.session.catalog.photo(self.photos[0]).unwrap()).unwrap();
        for field in ["develop", "history", "history_cursor", "edited"] {
            photo.as_object_mut().unwrap().remove(field);
        }
        photo
    }
    fn other(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photo(self.photos[1]).unwrap()).unwrap()
    }
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(),"selection":self.app.session.selection,
        "revision":self.app.session.catalog.revision,"undo":format!("{:?}",self.app.session.undo),"redo":format!("{:?}",self.app.session.redo),
        "journal":self.app.session.journal,"commits":self.app.session.commits(),"interaction":format!("{:?}",self.app.session.interaction)})
    }
    fn preferences(&self) -> Value {
        let mut ui = serde_json::to_value(&self.app.ui).unwrap();
        ui.as_object_mut().unwrap().remove("eyePet");
        ui
    }
    fn state(&self) -> Value {
        json!({"document":self.document(),"ui":self.preferences(),"eye":self.app.ui.eye,"pet":self.app.ui.eye_pet,"dragging":self.app.ui.dragging_control})
    }
    fn undo(&mut self) {
        self.app.run("edit.undo", json!({})).unwrap();
        self.settle();
    }
    fn redo(&mut self) {
        self.app.run("edit.redo", json!({})).unwrap();
        self.settle();
    }
    fn idle(&mut self) {
        let before = self.state();
        self.settle();
        assert_eq!(self.state(), before, "no passive replay; trace={:#?}", self.trace);
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
fn set_expected(settings: &mut DevelopSettings, index: usize, control: &str, value: f64) {
    match control {
        PUPIL => settings.red_eye[index].pupil_size = value,
        DARKEN => settings.red_eye[index].darken = value,
        _ => panic!("unexpected control"),
    }
}

#[test]
fn red_eye_actual_parent_existing_modes_are_ui_only_with_or_without_corrections() {
    for eyes in [false, true] {
        let mut f = Fixture::new(if eyes { 1 } else { 0 }, eyes);
        let before = f.document();
        let preferences = f.preferences();
        for (name, pet) in [("pet", true), ("red", false)] {
            let id = format!("button:eyeMode-{name}");
            f.actual_owner(&id);
            f.click(&id);
            assert_eq!(f.app.ui.eye_pet, pet);
            assert_eq!(f.document(), before);
            assert_eq!(f.preferences(), preferences);
        }
        f.tab_to("button:eyeMode-pet");
        f.key(Key::Enter);
        assert!(f.app.ui.eye_pet);
        f.tab_to("button:eyeMode-red");
        f.key(Key::Space);
        assert!(!f.app.ui.eye_pet);
        assert_eq!(f.document(), before);
        assert_eq!(f.preferences(), preferences);
        assert_eq!(f.has(&format!("slider:{PUPIL}")), eyes);
        assert_eq!(f.has("button:eyeDelete"), eyes);
        assert_eq!(f.has(CATCH), eyes);
        f.idle();
    }
}
#[test]
fn red_eye_actual_parent_red_and_pet_numeric_drags_target_one_correction_with_one_undo() {
    for index in [0, 1] {
        let mut f = Fixture::new(index, true);
        let other = f.other();
        let preferences = f.preferences();
        for (control, fraction, value, field) in [(PUPIL, 0.71, 71.0, "pupilSize"), (DARKEN, 0.28, 28.0, "darken")] {
            let cid = format!("redEye.{index}.{field}");
            for prefix in ["sliderLabel", "slider", "sliderValue"] {
                f.actual_owner(&format!("{prefix}:{control}"));
            }
            let before = f.develop();
            let undo = f.app.session.undo.len();
            let journal = f.app.session.journal.len();
            let photo = f.app.session.catalog.photo(f.photos[0]).unwrap();
            let history = photo.history[..photo.history_index().map_or(0, |index| index + 1)].to_vec();
            let metadata = f.metadata();
            let end = f.begin_drag(control, fraction);
            assert!(f.app.session.interaction.is_some(), "real begin/live before release; trace={:#?}", f.trace);
            assert_eq!(f.app.session.undo.len(), undo, "live updates do not append Undo");
            f.pointer(end, false);
            f.settle();
            let mut expected = before.clone();
            set_expected(&mut expected, index, control, value);
            assert_eq!(f.develop(), expected, "exact selected Red/Pet delta, not a replacement/retype");
            assert_eq!(f.other(), other);
            assert_eq!(f.preferences(), preferences);
            assert_eq!(f.app.session.undo.len(), undo + 1);
            assert!(f.app.session.redo.is_empty());
            let photo = f.app.session.catalog.photo(f.photos[0]).unwrap();
            assert_eq!(photo.history.len(), history.len() + 1);
            assert_eq!(&photo.history[..history.len()], history.as_slice());
            assert_eq!(photo.history.last().unwrap().settings.as_ref(), &expected);
            assert_eq!(photo.history.last().unwrap().label, lightcraft_develop::controls::find(&cid).unwrap().label);
            assert_eq!(f.metadata(), metadata, "all photo fields outside develop/history/edit timestamp remain intact");
            assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
            let commands = &f.app.session.journal[journal..];
            assert_eq!(commands.iter().filter(|(command, _)| command == "develop.beginInteraction").count(), 1);
            assert_eq!(commands.iter().filter(|(command, _)| command == "develop.endInteraction").count(), 1);
            assert!(commands.iter().any(|(command, payload)| command == "develop.set" && payload == &json!({"control":cid,"value":value})));
            assert!(commands.iter().all(|(command, payload)| match command.as_str() {
                "develop.beginInteraction" | "develop.endInteraction" => true,
                "develop.set" => payload["control"] == cid && payload.as_object().unwrap().len() == 2,
                _ => false,
            }));
            f.undo();
            assert_eq!(f.develop(), before);
            f.redo();
            assert_eq!(f.develop(), expected);
            assert_eq!(f.other(), other);
            f.idle();
        }
    }
}
#[test]
fn red_eye_actual_parent_natural_numeric_keyboard_codec_limits_steps_and_reset_keep_units() {
    for index in [0, 1] {
        let mut f = Fixture::new(index, true);
        let other = f.other();
        let mut expected = f.develop();
        let undo = f.app.session.undo.len();
        f.actual_owner(&format!("sliderValue:{PUPIL}"));
        f.type_value(PUPIL, "83.2");
        set_expected(&mut expected, index, PUPIL, 83.0);
        assert_eq!(f.develop(), expected, "existing zero-decimal codec");
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.actual_owner(&format!("slider:{DARKEN}"));
        f.tab_to(&format!("slider:{DARKEN}"));
        f.key(Key::ArrowRight);
        set_expected(&mut expected, index, DARKEN, 51.0);
        assert_eq!(f.develop(), expected);
        assert_eq!(f.app.session.undo.len(), undo + 2);
        f.type_value(PUPIL, "140");
        set_expected(&mut expected, index, PUPIL, 100.0);
        assert_eq!(f.develop(), expected);
        assert_eq!(f.app.session.undo.len(), undo + 3);
        f.double_thumb(PUPIL);
        set_expected(&mut expected, index, PUPIL, 50.0);
        assert_eq!(f.develop(), expected, "genuine default reset");
        assert_eq!(f.app.session.undo.len(), undo + 4);
        assert_eq!(f.other(), other);
        assert!(f.app.session.interaction.is_none());
        f.idle();
    }
}
#[test]
fn red_eye_actual_parent_numeric_escape_invalid_input_and_blur_preserve_genuine_redo() {
    let mut f = Fixture::new(1, true);
    let before = f.document();
    f.draft(PUPIL, "77");
    assert_eq!(f.document(), before, "draft is not an engine edit");
    f.key(Key::Escape);
    assert_eq!(f.document(), before, "Escape preserves full model, revision, nonempty Undo/Redo and journal");
    f.draft(DARKEN, "not-a-number");
    f.key(Key::Enter);
    assert_eq!(f.document(), before, "invalid text cannot become zero/default");
    f.key(Key::Escape);
    assert_eq!(f.document(), before);
    let mut expected = f.develop();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.draft(DARKEN, "34");
    f.key(Key::Tab);
    set_expected(&mut expected, 1, DARKEN, 34.0);
    assert_eq!(f.develop(), expected, "original blur commits a valid exact value");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    let commands = &f.app.session.journal[journal..];
    assert_eq!(
        commands
            .iter()
            .filter(|(command, payload)| command == "develop.set" && payload == &json!({"control":"redEye.1.darken","value":34.0}))
            .count(),
        1
    );
    assert!(f.app.session.interaction.is_none());
    f.idle();
}
#[test]
fn red_eye_actual_parent_retained_catchlight_has_natural_owner_exact_pet_index_and_undo() {
    let mut f = Fixture::new(1, true);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.actual_owner(CATCH);
    f.click(CATCH);
    let mut expected = before.clone();
    expected.red_eye[1].catchlight = Some(lightcraft_geom::Point::new(-0.35, -0.35));
    assert_eq!(f.develop(), expected);
    assert_eq!(f.other(), other);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert_eq!(&f.app.session.journal[journal..], &[("redeye.catchlight".into(), json!({"index":1,"on":true}))]);
    f.undo();
    assert_eq!(f.develop(), before);
    let journal = f.app.session.journal.len();
    f.tab_to(CATCH);
    f.key(Key::Space);
    assert_eq!(f.develop(), expected);
    assert_eq!(&f.app.session.journal[journal..], &[("redeye.catchlight".into(), json!({"index":1,"on":true}))]);
    f.key(Key::Space);
    assert_eq!(f.develop(), before);
    f.idle();
    let mut red = Fixture::new(0, true);
    assert!(!red.has(CATCH));
    let before = red.document();
    red.key(Key::Space);
    assert_eq!(red.document(), before);
}
#[test]
fn red_eye_actual_parent_retained_delete_and_delete_all_keep_exact_payloads_and_per_eye_undo() {
    for index in [0, 1] {
        let mut f = Fixture::new(index, true);
        let before = f.develop();
        let other = f.other();
        let undo = f.app.session.undo.len();
        let journal = f.app.session.journal.len();
        f.actual_owner("button:eyeDelete");
        f.tab_to("button:eyeDelete");
        f.key(Key::Enter);
        let mut expected = before.clone();
        expected.red_eye.remove(index);
        assert_eq!(f.develop(), expected);
        assert_eq!(f.other(), other);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(&f.app.session.journal[journal..], &[("redeye.delete".into(), json!({"index":index}))]);
        assert!(!f.has("button:eyeDeleteAll"));
        f.undo();
        assert_eq!(f.develop(), before);
        f.redo();
        assert_eq!(f.develop(), expected);
        f.idle();
    }
    let mut f = Fixture::new(1, true);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.actual_owner("button:eyeDeleteAll");
    f.click("button:eyeDeleteAll");
    let mut expected = before.clone();
    expected.red_eye.clear();
    assert_eq!(f.develop(), expected);
    assert_eq!(f.other(), other);
    assert_eq!(f.app.session.undo.len(), undo + 2);
    assert_eq!(&f.app.session.journal[journal..], &[("redeye.delete".into(), json!({"index":1})), ("redeye.delete".into(), json!({"index":0}))]);
    assert!(!f.has("button:eyeDelete") && !f.has(CATCH) && !f.has(&format!("slider:{PUPIL}")));
    f.undo();
    let mut one = before.clone();
    one.red_eye.truncate(1);
    assert_eq!(f.develop(), one);
    f.undo();
    assert_eq!(f.develop(), before);
    f.redo();
    f.redo();
    assert_eq!(f.develop(), expected);
    f.idle();
}
#[test]
fn red_eye_actual_parent_disabled_sizing_measurement_missing_selection_and_empty_never_replay() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Fixture::new(1, true);
        let targets: Vec<_> = [
            format!("slider:{PUPIL}"),
            format!("sliderValue:{DARKEN}"),
            "button:eyeMode-pet".into(),
            CATCH.into(),
            "button:eyeDelete".into(),
            "button:eyeDeleteAll".into(),
        ]
        .iter()
        .map(|id| f.rect(id).center())
        .collect();
        let before = f.state();
        f.probe = probe;
        f.settle();
        for pos in targets {
            f.pointer(pos, true);
            f.pointer(pos, false);
            f.key(Key::Space);
            f.key(Key::Enter);
            f.frame(vec![Event::Text("88".into())]);
        }
        assert_eq!(f.state(), before, "disabled/speculative controls cannot edit or run commands; trace={:#?}", f.trace);
        if !matches!(probe, Probe::Disabled) {
            assert!(!f.rects.iter().any(|(id, _)| owned(id)));
        }
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.actual_owner(&format!("slider:{PUPIL}"));
        f.idle();
    }
    let mut f = Fixture::new(1, true);
    let pos = f.rect(&format!("sliderValue:{PUPIL}")).center();
    f.app.run("library.select", json!({"ids":[]})).unwrap();
    f.settle();
    assert!(!f.rects.iter().any(|(id, _)| owned(id)));
    let before = f.document();
    f.pointer(pos, true);
    f.pointer(pos, false);
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    f.app.run("library.select", json!({"ids":[f.photos[0].0],"active":f.photos[0].0})).unwrap();
    let before = f.document();
    f.settle();
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    let mut empty = Fixture::new(0, false);
    let before = empty.document();
    empty.key(Key::Enter);
    empty.frame(vec![Event::Text("88".into())]);
    assert_eq!(empty.document(), before);
    assert!(!empty.has(&format!("slider:{PUPIL}")) && !empty.has(CATCH) && !empty.has("button:eyeDelete"));
}
#[test]
fn red_eye_actual_parent_numeric_capture_cannot_retarget_photos_replacements_hidden_parent_or_foreign_interaction() {
    // library.select genuinely ends/commits a live drag on the old photo; retain that policy.
    let mut f = Fixture::new(1, true);
    let original = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let end = f.begin_drag(PUPIL, 0.82);
    assert!(f.app.session.interaction.is_some());
    f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
    f.settle();
    let mut committed = original.clone();
    set_expected(&mut committed, 1, PUPIL, 82.0);
    assert_eq!(f.develop(), committed);
    assert_eq!(f.other(), other);
    assert_eq!(f.app.session.undo.len(), undo + 1, "selection keeps the existing single commit");
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    let before = f.document();
    let before_state = f.state();
    f.pointer(end, false);
    assert_eq!(f.document(), before, "old drag release cannot replay into the selected photo");
    assert_eq!(f.state(), before_state, "release preserves the complete actual-parent state");
    // Immediate Enter belongs to the naturally focused Catchlight, verified separately below.
    f.undo();
    assert_eq!(f.develop(), original);
    f.redo();
    assert_eq!(f.develop(), committed);
    assert_eq!(f.other(), other);
    // Disabling the current parent without a commit command cancels only its own live preview.
    let mut f = Fixture::new(1, true);
    let original = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let end = f.begin_drag(DARKEN, 0.22);
    assert!(f.app.session.interaction.is_some());
    f.probe = Probe::Disabled;
    f.settle();
    assert_eq!(f.develop(), original);
    assert_eq!(f.other(), other);
    assert_eq!(f.app.session.undo.len(), undo);
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    f.probe = Probe::Real;
    f.settle();
    let before = f.document();
    f.pointer(end, false);
    assert_eq!(f.document(), before, "disabled capture cannot replay on release");
    f.tab_to(&format!("sliderValue:{DARKEN}"));
    assert_eq!(f.document(), before, "natural traversal to the fresh numeric owner is inert");
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    // A real deletion shifts the correction at the same index; its old draft cannot submit.
    let mut f = Fixture::new(0, true);
    f.draft(DARKEN, "91");
    let old = f.hit(&format!("sliderValue:{DARKEN}")).id;
    f.app.run("redeye.delete", json!({"index":0})).unwrap();
    f.settle();
    assert_ne!(f.hit(&format!("sliderValue:{DARKEN}")).id, old);
    let before = f.document();
    f.tab_to(&format!("sliderValue:{DARKEN}"));
    assert_eq!(f.document(), before, "replacement and natural focus cannot submit the removed owner's draft");
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    // Real panel commands end interactions themselves; a hidden draft never reappears.
    let mut f = Fixture::new(1, true);
    f.draft(PUPIL, "79");
    let old = f.hit(&format!("sliderValue:{PUPIL}")).id;
    f.app.run("panel.remove", json!({})).unwrap();
    f.settle();
    assert!(!f.has(&format!("sliderValue:{PUPIL}")));
    f.app.run("panel.redeye", json!({})).unwrap();
    f.settle();
    assert_ne!(f.hit(&format!("sliderValue:{PUPIL}")).id, old);
    let before = f.document();
    f.tab_to(&format!("sliderValue:{PUPIL}"));
    assert_eq!(f.document(), before, "a returning numeric owner cannot recover the hidden parent's draft");
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    // Losing our exact interaction token must not cancel/commit a later owner's transaction.
    let mut f = Fixture::new(1, true);
    let end = f.begin_drag(PUPIL, 0.75);
    f.app.run("develop.endInteraction", json!({})).unwrap();
    f.app.run("develop.beginInteraction", json!({"label":"Foreign edit"})).unwrap();
    f.app.run("develop.set", json!({"control":"light.exposure","value":0.8})).unwrap();
    let before = f.document();
    f.pointer(end, false);
    assert_eq!(f.document(), before, "lost capture cannot finish or cancel the foreign interaction");
    f.tab_to(&format!("sliderValue:{PUPIL}"));
    assert_eq!(f.document(), before, "natural focus cannot acquire the foreign interaction");
    f.key(Key::Enter);
    assert_eq!(f.document(), before);
    assert_eq!(f.app.session.interaction.as_ref().unwrap().label, "Foreign edit");
    f.app.run("develop.cancelInteraction", json!({})).unwrap();
    f.settle();
    f.idle();
}
#[test]
fn red_eye_diagnostic_measurement_existing_delete_remains_full_model_no_edit() {
    let mut f = Fixture::new(1, true);
    let before = f.state();
    f.actual_owner("button:eyeDelete");
    let pos = f.rect("button:eyeDelete").center();
    f.probe = Probe::Measure;
    f.settle();
    f.tick += 30;
    f.frame(vec![Event::PointerMoved(pos)]);
    f.pointer(pos, true);
    f.pointer(pos, false);
    f.settle();
    assert_eq!(f.state(), before, "diagnostic retains full state/no-command/no-replay requirement");
}

#[test]
fn red_eye_actual_parent_post_selection_immediate_enter_belongs_to_observed_catchlight_with_one_undo() {
    let mut f = Fixture::new(1, true);
    let end = f.begin_drag(PUPIL, 0.82);
    f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
    f.settle();
    let before_release = f.state();
    f.pointer(end, false);
    assert_eq!(f.state(), before_release, "the old drag release is inert across the entire domain");
    assert_eq!(f.app.session.active(), Some(f.photos[1]));
    assert_eq!(f.app.ui.eye, 1);
    let owner = f.actual_owner(CATCH);
    // The focused slider went with the old photo. The framework leaves the focus nowhere when a focused stop goes away
    // after the pass that focused it (next 8206a076a); a fallback may also land on the surviving Catchlight owner.
    // Either way the promise holds: the Enter after the switch belongs to nothing stale.
    let focused = craft::ui::runtime::focus::focused_rec(&f.ctx).map(|record| record.stop.id);
    if focused.is_none() {
        assert_eq!(f.ctx.memory(|memory| memory.focused()), None, "observe the real owner; never request focus");
        let (document, preferences, undo, journal) = (f.document(), f.preferences(), f.app.session.undo.len(), f.app.session.journal.len());
        f.key(Key::Enter);
        assert_eq!(f.app.session.journal.len(), journal, "no stale numeric or other-domain replay");
        assert_eq!(f.app.session.undo.len(), undo, "no undo step");
        assert_eq!(f.document(), document, "document and selection unchanged");
        assert_eq!(f.preferences(), preferences);
        f.idle();
        return;
    }
    assert_eq!(focused, Some(owner), "observe the real owner; never request focus");
    assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(owner));
    let before = f.document();
    let preferences = f.preferences();
    let before_other = f.other();
    let before_photo = f.app.session.catalog.photo(f.photos[1]).unwrap();
    assert!(before_photo.history_cursor.is_none());
    let old_history = before_photo.history.clone();
    let old_edited = before_photo.edited.clone();
    let old_settings = before_photo.develop.clone();
    let mut expected_settings = (*old_settings).clone();
    expected_settings.red_eye[1].catchlight = Some(lightcraft_geom::Point::new(-0.35, -0.35));
    let expected_inverse = Op::Batch {
        ops: vec![
            Op::SetHistory { id: f.photos[1], history: old_history.clone() },
            Op::SetDevelop { id: f.photos[1], settings: old_settings, label: "Catchlight".into(), edited: old_edited },
        ],
    };
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.key(Key::Enter); // Immediate genuine activation of the observed current Catchlight owner.
    assert_eq!(&f.app.session.journal[journal..], &[("redeye.catchlight".into(), json!({"index":1,"on":true}))]);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    let inverse = f.app.session.undo.last().unwrap();
    assert_eq!(inverse.label, "Catchlight");
    assert_eq!(serde_json::to_value(&inverse.op).unwrap(), serde_json::to_value(&expected_inverse).unwrap());
    assert!(inverse.folder.is_none() && f.app.session.redo.is_empty() && f.app.session.interaction.is_none());
    let mut expected_other = before_other.clone();
    expected_other["develop"] = serde_json::to_value(&expected_settings).unwrap();
    expected_other["history"].as_array_mut().unwrap().push(json!({"label":"Catchlight","settings":expected_settings}));
    // Only the genuine engine edit timestamp is volatile. Its old value is verified in the exact inverse above and restored by Undo below.
    let edited = f.other()["edited"].clone();
    assert!(!edited.is_null());
    expected_other["edited"] = edited;
    let mut expected = before.clone();
    expected["photos"][1] = expected_other.clone();
    expected["journal"].as_array_mut().unwrap().push(json!(["redeye.catchlight",{"index":1,"on":true}]));
    expected["commits"] = json!(before["commits"].as_u64().unwrap() + 1);
    expected["revision"] = json!(before["revision"].as_u64().unwrap() + 1);
    let mut expected_undo = before["undo"].as_str().unwrap().strip_suffix(']').unwrap().to_owned();
    if undo > 0 {
        expected_undo.push_str(", ");
    }
    expected_undo.push_str(&format!(
        "UndoEntry {{ label: {:?}, op: {:?}, folder: None, retry_inverse: [], retry_sidecars: [] }}",
        "Catchlight", expected_inverse
    ));
    expected_undo.push(']');
    expected["undo"] = json!(expected_undo);
    assert_eq!(f.document(), expected, "one genuine Catchlight change only; no stale numeric or other-domain replay");
    assert_eq!(f.preferences(), preferences);
    f.undo();
    assert_eq!(f.other(), before_other, "Undo restores the entire second photo, including old history and edited timestamp");
    assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[0]).unwrap()).unwrap(), before["photos"][0]);
    f.redo();
    assert_eq!(f.other(), expected_other);
    assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[0]).unwrap()).unwrap(), before["photos"][0]);
    f.idle();
}
