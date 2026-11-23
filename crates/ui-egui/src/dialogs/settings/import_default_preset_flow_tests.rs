//! Real preset_combo + existing Import callsites. In-memory demo photos/presets; no logic loop or OS service.
use super::{LightcraftApp, Tokens, preset_combo};
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use serde_json::{Value, json};

struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}

// 0 = SDK leaf; 1 = scoped native leaf; 2 = actual scoped Import body; 3 = installed Settings parent.
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    mode: u8,
    target: &'static str,
    enabled: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    painted: Vec<(String, Rect)>,
    emitted: Vec<Option<String>>,
    old_language: crate::i18n::Locale,
}
impl Fixture {
    fn new(mode: u8, target: &'static str) -> Self {
        let mut session = lightcraft_engine::Session::with_demo();
        session.presets = lightcraft_engine::presets::PresetList::new(vec![
            lightcraft_develop::presets::Preset {
                id: "test.a".into(),
                name: "Auto".into(),
                group: "Color".into(),
                settings: json!({"light":{"exposure":0.3}}),
                favorite: false,
                builtin: true,
            },
            lightcraft_develop::presets::Preset {
                id: "test.b".into(),
                name: "User & Amber".into(),
                group: "Custom & Group".into(),
                settings: json!({"light":{"exposure":-0.7}}),
                favorite: false,
                builtin: false,
            },
            lightcraft_develop::presets::Preset {
                id: "test.c".into(),
                name: "Blue".into(),
                group: "Color".into(),
                settings: json!({"light":{"exposure":1.2}}),
                favorite: false,
                builtin: true,
            },
        ]);
        let mut app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        // Seed only through the real command; subsequent choices must come from rendered input.
        app.run(
            "library.preferences",
            json!({"import": {"rawPreset": null, "otherPreset": "test.c", "perCamera": false,
            "copyright": "Kept copyright", "creator": "Kept creator", "showOnCard": true,
            "cameras": [{"camera":"Fixture camera", "preset":"test.c"}]}}),
        )
        .unwrap();
        app.run("app.settings", json!({"tab":"import"})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let old_language = crate::i18n::language();
        let mut f =
            Self { app, ctx, mode, target, enabled: true, tick: 0, rects: vec![], hits: vec![], painted: vec![], emitted: vec![], old_language };
        for _ in 0..3 {
            f.frame(vec![], 0);
        }
        f
    }
    fn face(&self) -> &'static str {
        if self.target == "rawPreset" { "combo:settingsRawPreset" } else { "combo:settingsOtherPreset" }
    }
    fn current(&self) -> Option<&str> {
        if self.target == "rawPreset" {
            self.app.session.import_defaults.raw_preset.as_deref()
        } else {
            self.app.session.import_defaults.other_preset.as_deref()
        }
    }
    fn memory_only(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
    }
    fn frame(&mut self, events: Vec<Event>, probe: u8) {
        self.memory_only();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 1500.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let (mode, target, enabled) = (self.mode, self.target, self.enabled);
        let emitted = &mut self.emitted;
        let mut final_pass = None;
        let output = Output(self.ctx.run_ui(raw, |ui| {
            if mode == 3 {
                assert!(enabled && probe == 0);
                crate::dialogs::show(app, &ctx, false);
            } else {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut draw = |ui: &mut egui::Ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            if mode == 2 {
                                (|| super::super::import::import_tab(app, ui, &Tokens::get(&ctx)))();
                            } else {
                                let current = if target == "rawPreset" {
                                    app.session.import_defaults.raw_preset.clone()
                                } else {
                                    app.session.import_defaults.other_preset.clone()
                                };
                                let (id, label, none) = if target == "rawPreset" {
                                    ("settingsRawPreset", "Raw default", "LightCraft Default")
                                } else {
                                    ("settingsOtherPreset", "Other preset", "None")
                                };
                                let mut draw = || preset_combo(app, ui, id, crate::i18n::tr(label), current.as_deref(), none);
                                let chosen = if mode == 1 { (draw)() } else { draw() };
                                if let Some(next) = chosen {
                                    emitted.push(next.clone());
                                    app.run("library.preferences", json!({"import": {(target): next}})).unwrap();
                                }
                            }
                        });
                    };
                    match probe {
                        1 => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                        2 => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                        _ => draw(ui),
                    }
                });
            }
            final_pass = Some(crate::widgets::take_registries(&ctx));
        }));
        fn text(shape: &egui::Shape, labels: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(t) => labels.push((t.galley.job.text.clone(), Rect::from_min_size(t.pos, t.galley.size()))),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, labels);
                    }
                }
                _ => {}
            }
        }
        self.painted.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.painted);
        }
        (self.rects, self.hits) = final_pass.unwrap();
        drop(output);
        self.memory_only();
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(key, _)| key == id).collect();
        assert_eq!(found.len(), 1, "one locator for {id} per presented pass");
        found[0].1
    }
    fn click_at(&mut self, at: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
            );
        }
        self.frame(vec![], 0);
    }
    fn open(&mut self) {
        self.click_at(self.rect(self.face()).center());
    }
    fn click_choice(&mut self, caption: &str) {
        // Original native picker rows have no app automation IDs. Hit the actual painted row
        // text in the popup layer, never a lookalike closed-face caption in the parent layer.
        let parent = self.hits.iter().find(|(id, _)| id == self.face()).unwrap().1.layer_id;
        let matches: Vec<_> = self
            .painted
            .iter()
            .filter(|(text, rect)| text == caption && self.ctx.layer_id_at(rect.center()).is_some_and(|layer| layer != parent))
            .collect();
        assert_eq!(matches.len(), 1, "one visible popup caption {caption:?}: {:?}", self.painted);
        self.click_at(matches[0].1.center());
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], 0);
        }
        self.frame(vec![], 0);
    }
    fn tab_to_face(&mut self) {
        assert_eq!(self.mode, 0, "SDK-only traversal qualification, not native Settings traversal");
        let id = self.hits.iter().find(|(name, _)| name == self.face()).unwrap().1.id;
        self.frame(vec![Event::PointerGone], 0);
        for _ in 0..32 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach preset selector");
    }
    fn unrelated(&self) -> Value {
        let s = &self.app.session;
        let mut import = serde_json::to_value(&s.import_defaults).unwrap();
        import.as_object_mut().unwrap().remove(self.target);
        json!({"import":import,"settings":self.app.ui.settings,"xmp":s.xmp,"cacheMb":s.cache_mb,"forgetLocalDays":s.forget_local_days,
            "photos":s.catalog.photos().map(|photo| serde_json::to_value(photo).unwrap()).collect::<Vec<_>>(),
            "active":s.selection.active.map(|id| id.0),"selection":s.selection.ids.iter().map(|id|id.0).collect::<Vec<_>>(),
            "revision":s.catalog.revision,"undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits()})
    }
    fn choose_seed(&mut self, id: Option<&str>) {
        self.app.run("library.preferences", json!({"import": {(self.target): id}})).unwrap();
        self.frame(vec![], 0);
        self.emitted.clear();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
        if !std::thread::panicking() {
            self.memory_only();
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 1500.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Output(self.ctx.run_ui(raw, |_| {})));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn import_default_preset_pointer_some_none_and_same_choice_preserve_other_settings() {
    for mode in [0, 1, 2, 3] {
        for target in ["rawPreset", "otherPreset"] {
            let mut f = Fixture::new(mode, target);
            f.choose_seed(Some("test.a"));
            let before = f.unrelated();
            let journal = f.app.session.journal.len();
            f.open();
            f.click_choice("User & Amber");
            assert_eq!(f.current(), Some("test.b"));
            assert_eq!(f.unrelated(), before);
            assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.preferences".into(), json!({"import": {(target): "test.b"}}))]);
            if mode <= 1 {
                assert_eq!(f.emitted, vec![Some("test.b".into())]);
            }
            let stable = f.app.session.journal.clone();
            f.open();
            f.click_choice("User & Amber");
            assert_eq!(f.app.session.journal, stable, "same choice is no change");
            f.open();
            let none = crate::i18n::tr(if target == "rawPreset" { "LightCraft Default" } else { "None" }).to_owned();
            f.click_choice(&none);
            assert_eq!(f.current(), None, "explicit None clears the real import default");
            if mode <= 1 {
                assert_eq!(f.emitted, vec![Some("test.b".into()), None]);
            }
            assert_eq!(f.unrelated(), before);
            let stable = f.app.session.journal.clone();
            for _ in 0..3 {
                f.frame(vec![], 0);
            }
            assert_eq!(f.app.session.journal, stable);
            if mode == 3 {
                assert_eq!(f.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "import".into() }));
            }
        }
    }
}

#[test]
fn import_default_preset_sdk_natural_tab_keyboard_commit_and_escape_cancel() {
    for activation in [Key::Enter, Key::Space] {
        let mut f = Fixture::new(0, "rawPreset");
        let before = f.unrelated();
        let journal = f.app.session.journal.clone();
        f.tab_to_face();
        f.key(activation);
        f.key(Key::End);
        assert_eq!(f.current(), None, "open-list draft is not a model edit");
        f.key(Key::Escape);
        assert!(f.emitted.is_empty());
        assert_eq!(f.app.session.journal, journal);
        f.key(activation);
        f.key(Key::End);
        f.key(Key::Enter);
        assert_eq!(f.current(), Some("test.c"));
        assert_eq!(f.emitted, vec![Some("test.c".into())]);
        assert_eq!(f.unrelated(), before);
    }
}

#[test]
fn import_default_preset_installed_parent_escape_preserves_default_and_reopens() {
    for mode in [0, 1, 2, 3] {
        let mut f = Fixture::new(mode, "rawPreset");
        f.choose_seed(Some("test.a"));
        let before = f.unrelated();
        let journal = f.app.session.journal.clone();
        f.open();
        f.key(Key::ArrowDown);
        f.key(Key::Escape);
        assert_eq!(f.current(), Some("test.a"));
        assert_eq!(f.app.session.journal, journal);
        assert!(f.emitted.is_empty());
        if mode >= 1 {
            assert!(!egui::Popup::is_any_open(&f.ctx));
        } else {
            assert!(!craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
        }
        assert_eq!(f.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "import".into() }));
        f.open();
        f.click_choice("User & Amber");
        assert_eq!(f.current(), Some("test.b"), "same production field reopens");
        assert_eq!(f.unrelated(), before);
    }
}

#[test]
fn import_default_preset_missing_reference_stays_visible_until_real_choice() {
    for mode in [0, 1, 3] {
        let mut f = Fixture::new(mode, "rawPreset");
        f.choose_seed(Some("test.a"));
        // Fixture simulates a previously saved preset disappearing, not an invented input setter.
        f.app.session.presets.retain(|preset| preset.id != "test.a");
        f.frame(vec![], 0);
        let missing = crate::i18n::tr_format!("{pid} (missing)", pid = "test.a");
        assert!(f.painted.iter().any(|(label, _)| label == &missing));
        let journal = f.app.session.journal.clone();
        f.open();
        f.key(Key::Escape);
        assert_eq!(f.current(), Some("test.a"));
        assert_eq!(f.app.session.journal, journal);
        f.open();
        f.click_choice("User & Amber");
        assert_eq!(f.current(), Some("test.b"));
    }
}

#[test]
fn import_default_preset_grouped_entries_localize_stock_and_preserve_user_labels_and_refresh() {
    let mut f = Fixture::new(0, "rawPreset");
    f.open();
    assert!(f.painted.iter().any(|(text, _)| text == "Custom & Group"));
    assert!(f.painted.iter().any(|(text, _)| text == "User & Amber"));
    f.key(Key::Escape);
    // Language alone invalidates translated captions; user labels stay literal.
    crate::i18n::set_language(crate::i18n::Locale::parse_tag("fr").unwrap());
    f.frame(vec![], 0);
    f.open();
    assert!(f.painted.iter().any(|(text, _)| text == crate::i18n::tr("LightCraft Default")));
    assert!(f.painted.iter().any(|(text, _)| text == crate::i18n::builtin_label("Color", true)));
    assert!(f.painted.iter().any(|(text, _)| text == "Custom & Group"));
    assert!(f.painted.iter().any(|(text, _)| text == "User & Amber"));
    f.key(Key::Escape);
    // Generation alone then invalidates the same cached list after a fixture rename.
    f.app.session.presets.iter_mut().find(|preset| preset.id == "test.b").unwrap().name = "Renamed & Amber".into();
    f.frame(vec![], 0);
    f.open();
    f.click_choice("Renamed & Amber");
    assert_eq!(f.current(), Some("test.b"));
}

#[test]
fn import_default_preset_disabled_and_speculative_input_emit_no_change() {
    for mode in [0, 1] {
        let mut f = Fixture::new(mode, "rawPreset");
        f.choose_seed(Some("test.a"));
        let before = f.unrelated();
        let journal = f.app.session.journal.clone();
        f.enabled = false;
        f.frame(vec![], 0);
        f.open();
        assert_eq!(f.current(), Some("test.a"));
        assert_eq!(f.app.session.journal, journal);
        assert!(f.emitted.is_empty());
        assert_eq!(f.unrelated(), before);
    }
    for probe in [1, 2] {
        let mut f = Fixture::new(0, "rawPreset");
        let journal = f.app.session.journal.clone();
        let at = f.rect(f.face()).center();
        for pressed in [true, false] {
            f.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                probe,
            );
            assert!(!f.rects.iter().any(|(id, _)| id == f.face()));
        }
        f.frame(vec![], 0);
        assert!(f.emitted.is_empty());
        assert_eq!(f.app.session.journal, journal);
        assert_eq!(f.current(), None);
    }
}

#[test]
fn import_default_preset_empty_list_and_same_none_return_no_change() {
    for mode in [0, 1, 3] {
        let mut f = Fixture::new(mode, "rawPreset");
        f.app.session.presets.clear();
        f.frame(vec![], 0);
        let journal = f.app.session.journal.clone();
        let before = f.unrelated();
        f.open();
        let caption = crate::i18n::tr("LightCraft Default").to_owned();
        f.click_choice(&caption);
        assert_eq!(f.current(), None);
        assert!(f.emitted.is_empty(), "same None must not emit Some(None)");
        assert_eq!(f.app.session.journal, journal);
        assert_eq!(f.unrelated(), before);
    }
}
