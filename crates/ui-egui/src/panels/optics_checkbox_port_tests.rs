//! Matched pure in-memory UI observations. No app logic tick, renderer or lens service is run.
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::{DevelopSettings, EmbeddedLens, LensSource};
use serde_json::{Value, json};

use crate::headless::HeadlessView;
use crate::{LightcraftApp, Services};

const CA: &str = "check:optics.removeCA";
const LENS: &str = "check:optics.lensProfile";

/// Test-only oracle: the exact pre-port controls and separate command paths, with hit probes.
fn native(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    let mut ca = d.optics.remove_ca;
    let r = ui.checkbox(&mut ca, crate::i18n::tr("Remove Chromatic Aberration"));
    crate::widgets::register_response(CA, r.rect, &r);
    if r.changed() {
        let _ = app.run("develop.merge", json!({"settings": {"optics": {"remove_ca": ca}}, "label": "Remove CA"}));
    }
    let mut lp = d.optics.lens_profile;
    let r = ui.checkbox(&mut lp, crate::i18n::tr("Enable Lens Corrections"));
    crate::widgets::register_response(LENS, r.rect, &r);
    if r.changed() {
        let ok = app.run("develop.merge", json!({"settings": {"optics": {"lens_profile": lp}}, "label": "Lens Corrections"})).is_ok();
        if ok && lp {
            app.ui.lens_lookup_ids.extend(crate::lens_db::to_look_up(&app.session, id));
        }
    }
}

struct Fixture {
    app: LightcraftApp,
    view: HeadlessView,
    ids: Vec<PhotoId>,
    sdk: bool,
    tick: u32,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    rects: Vec<(String, Rect)>,
    labels: Vec<String>,
    down: Vec<(String, egui::Id)>,
}
impl Fixture {
    fn new(sdk: bool, sync: bool) -> Self {
        let mut session = lightcraft_engine::Session::new();
        let mut ids = Vec::new();
        for i in 0..3 {
            let id = session.catalog.alloc_photo_id();
            let mut p = Photo::new(id, Source::File { path: format!("/fake-optics-{i}.jpg") }, "Fake photo", "JPEG", 8, 8, "2026-01-01T00:00:00");
            if i == 2 {
                p.embedded_lens = Some(EmbeddedLens { source: LensSource::File, ..Default::default() });
            }
            session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        session.selection.ids = ids.clone();
        session.selection.active = Some(ids[0]);
        session.auto_sync = sync;
        let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
        let view = HeadlessView::new();
        app.install_framework(&view.ctx);
        let mut f = Self { app, view, ids, sdk, tick: 0, hits: Vec::new(), rects: Vec::new(), labels: Vec::new(), down: Vec::new() };
        for _ in 0..3 {
            f.frame(vec![], 0, true);
        }
        f
    }
    // mode 1 is SDK measurement; mode 2 is egui's independent sizing pass.
    fn frame(&mut self, events: Vec<Event>, mode: u8, enabled: bool) {
        let id = self.ids[0];
        let d = self.app.session.develop_of(id).unwrap();
        let raw = HeadlessView::raw_input(egui::vec2(520.0, 180.0), 1.0, f64::from(self.tick) / 60.0, events);
        self.tick += 1;
        let app = &mut self.app;
        let sdk = self.sdk;
        let mut labels = Vec::new();
        let mut registry = Default::default();
        let mut down = Vec::new();
        self.view.run(raw, |ui| {
            let _ = crate::widgets::take_registries(ui.ctx());
            down.clear();
            egui::CentralPanel::default().show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let mut sizing;
                let ui = if mode == 2 {
                    sizing = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                    &mut sizing
                } else {
                    ui
                };
                ui.add_enabled_ui(enabled, |ui| {
                    let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("optics-checkbox-test"), |ui| {
                        let mut draw = |ui: &mut egui::Ui| {
                            if sdk {
                                super::optics_checkboxes(app, ui, id, &d);
                            } else {
                                native(app, ui, id, &d);
                            }
                        };
                        if mode == 1 {
                            craft::ui::runtime::measure::run(&ctx, || draw(ui));
                        } else {
                            draw(ui);
                        }
                    });
                    if let Some(record) = record {
                        labels = record
                            .elements
                            .iter()
                            .filter_map(|e| e.facts.as_ref())
                            .filter(|facts| facts.role == "CheckBox")
                            .filter_map(|facts| facts.name.clone())
                            .collect();
                    }
                });
            });
            registry = crate::widgets::take_registries(ui.ctx());
            for (name, hit) in &registry.1 {
                let raw = ui.ctx().read_response(hit.id).expect("current registered checkbox");
                assert_eq!((raw.id, raw.layer_id, raw.interact_rect), (hit.id, hit.layer_id, hit.interact_rect));
                if raw.is_pointer_button_down_on() {
                    down.push((name.clone(), raw.id));
                }
            }
        });
        (self.rects, self.hits) = registry;
        self.down = down;
        self.labels = labels;
    }
    fn click(&mut self, name: &str) {
        let rect = self.rects.iter().find(|(key, _)| key == name).unwrap().1;
        let hit = self.hits.iter().find(|(key, _)| key == name).expect("current checkbox hit").1;
        // SDK rect is the painted box; interact_rect also includes the adjacent label.
        let at = Pos2::new(hit.interact_rect.right() - 6.0, hit.interact_rect.center().y);
        assert!(hit.interact_rect.contains(at));
        if self.sdk {
            assert!(at.x > rect.right() && !rect.contains(at), "pointer must land on the label, not its box");
        }
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
                true,
            );
        }
        self.frame(vec![], 0, true);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }], 0, true);
        }
    }
    fn keyboard_toggle(&mut self, name: &str) {
        let target = self.hits.iter().find(|(key, _)| key == name).unwrap().1.id;
        let mut reached = false;
        for _ in 0..8 {
            self.key(Key::Tab);
            let focus = if self.sdk {
                craft::ui::runtime::focus::focused_rec(&self.view.ctx).map(|r| r.stop.id)
            } else {
                self.view.ctx.memory(|m| m.focused())
            };
            if focus == Some(target) {
                reached = true;
                break;
            }
        }
        assert!(reached, "natural Tab must reach {name}");
        self.key(Key::Space);
        self.frame(vec![], 0, true);
    }
    fn observation(&self) -> Value {
        json!({
            "revision": self.app.session.catalog.revision,
            "settings": self.ids.iter().map(|id| self.app.session.develop_of(*id).unwrap()).collect::<Vec<_>>(),
            "history": self.ids.iter().map(|id| self.app.session.catalog.photo(*id).unwrap().history.iter().map(|step| step.label.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            "journal": self.app.session.journal,
            "undo": self.app.session.undo.iter().map(|step| step.label.clone()).collect::<Vec<_>>(),
            "queue": self.app.ui.lens_lookup_ids,
            "selection": self.app.session.selection.ids,
            "active": self.app.session.selection.active,
        })
    }
}

#[test]
fn optics_checkbox_mouse_and_keyboard_match_native_payload_targets_and_single_undo() {
    for sync in [false, true] {
        for keyboard in [false, true] {
            for (control, label, settings) in
                [(CA, "Remove CA", json!({"optics": {"remove_ca": true}})), (LENS, "Lens Corrections", json!({"optics": {"lens_profile": true}}))]
            {
                let mut old = Fixture::new(false, sync);
                let mut sdk = Fixture::new(true, sync);
                assert_eq!(sdk.labels, ["Remove Chromatic Aberration", "Enable Lens Corrections"]);
                let before = sdk.observation();
                let undo = sdk.app.session.undo.len();
                for f in [&mut old, &mut sdk] {
                    if keyboard {
                        f.keyboard_toggle(control);
                    } else {
                        f.click(control);
                    }
                }
                assert_eq!(sdk.observation(), old.observation(), "{control}, sync={sync}, keyboard={keyboard}");
                assert_eq!(sdk.app.session.journal.last(), Some(&("develop.merge".into(), json!({"settings": settings, "label": label}))));
                assert!(sdk.observation()["revision"].as_u64().unwrap() > before["revision"].as_u64().unwrap());
                assert_eq!(sdk.app.session.undo.len(), undo + 1);
                assert_eq!(sdk.app.session.undo.last().unwrap().label, label);
                if control == CA {
                    assert!(sdk.app.ui.lens_lookup_ids.is_empty());
                } else {
                    let expected = if sync { vec![sdk.ids[0], sdk.ids[1]] } else { vec![sdk.ids[0]] };
                    assert_eq!(sdk.app.ui.lens_lookup_ids, expected, "profile-bearing third photo is excluded");
                }
                for f in [&mut old, &mut sdk] {
                    f.app.run("edit.undo", json!({})).unwrap();
                    f.frame(vec![], 0, true);
                }
                assert_eq!(sdk.observation(), old.observation());
                assert_eq!(sdk.observation()["settings"], before["settings"], "one Undo restores all affected photos");
            }
        }
    }
}

#[test]
fn optics_checkbox_disable_idle_and_failed_enable_never_queue_lookup() {
    for use_sdk in [false, true] {
        let mut f = Fixture::new(use_sdk, true);
        f.click(LENS);
        f.app.ui.lens_lookup_ids.clear();
        f.click(LENS);
        assert!(f.app.ui.lens_lookup_ids.is_empty(), "disable is not a lookup trigger");
        let before = f.observation();
        for _ in 0..3 {
            f.frame(vec![], 0, true);
        }
        assert_eq!(f.observation(), before, "idle frames are pure");
        f.app.session.selection.active = None;
        f.app.session.selection.ids.clear();
        let before = f.observation();
        f.click(LENS);
        assert_eq!(f.observation(), before, "engine refusal changes neither settings nor queue/history");
    }
}

#[test]
fn optics_checkbox_both_measurement_modes_and_disabled_input_are_pure() {
    for control in [CA, LENS] {
        for mode in [1, 2] {
            let mut f = Fixture::new(true, true);
            let before = f.observation();
            let at = f.rects.iter().find(|(key, _)| key == control).unwrap().1.center();
            for pressed in [true, false] {
                f.frame(
                    vec![
                        Event::PointerMoved(at),
                        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
                    ],
                    mode,
                    true,
                );
                assert!(f.rects.is_empty(), "measure mode {mode} cannot publish automation targets");
            }
            f.frame(vec![], 0, true);
            assert_eq!(f.observation(), before, "measuring cannot edit or replay input");
            for pressed in [true, false] {
                f.frame(
                    vec![
                        Event::PointerMoved(at),
                        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
                    ],
                    0,
                    false,
                );
            }
            f.frame(vec![], 0, true);
            assert_eq!(f.observation(), before, "disabled controls cannot edit or queue lookups");
            // A press that started while enabled must not commit on an inert release,
            // nor survive into a later ordinary frame. Prove recovery on this same owner.
            for (release_mode, enabled) in [(mode, true), (0, false)] {
                let baseline = f.observation();
                let hit = f.hits.iter().find(|(k, _)| k == control).unwrap().1;
                let at = hit.interact_rect.center();
                let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
                f.frame(vec![Event::PointerMoved(at)], 0, true);
                f.frame(vec![button(true)], 0, true);
                assert!(f.down.contains(&(control.to_owned(), hit.id)), "the real checkbox owns the enabled press");
                f.frame(vec![button(false)], release_mode, enabled);
                assert_eq!(f.observation(), baseline);
                for _ in 0..3 {
                    f.frame(vec![], 0, true);
                    assert_eq!(f.observation(), baseline);
                }
                let undo = f.app.session.undo.len();
                let prior = f.app.session.develop_of(f.ids[0]).unwrap();
                let prior_flag = if control == CA { prior.optics.remove_ca } else { prior.optics.lens_profile };
                f.click(control);
                let after = f.app.session.develop_of(f.ids[0]).unwrap();
                assert_eq!(if control == CA { after.optics.remove_ca } else { after.optics.lens_profile }, !prior_flag);
                assert_eq!(f.app.session.undo.len(), undo + 1);
                assert!(f.observation()["revision"].as_u64().unwrap() > baseline["revision"].as_u64().unwrap());
                let mut expected_queue: Vec<PhotoId> = serde_json::from_value(baseline["queue"].clone()).unwrap();
                if control == LENS && !prior_flag {
                    expected_queue.extend([f.ids[0], f.ids[1]]);
                }
                assert_eq!(f.app.ui.lens_lookup_ids, expected_queue);
            }
        }
    }
}
