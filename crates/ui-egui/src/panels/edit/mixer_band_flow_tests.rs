//! Actual common Develop Color/B&W parent, SDK palette ownership and genuine scalar-command/Undo equivalence.
//! No canvas, files, forced focus, behavioral substitute or native/window visual acceptance.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::RightPanel;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::{MIXER_BANDS, PointColor, controls};
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;

fn locator(index: usize) -> String {
    format!("mixerBand:{}", MIXER_BANDS[index])
}
fn owned(name: &str) -> bool {
    name.starts_with("mixerBand:")
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Sizing,
    Measure,
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
    photos: [PhotoId; 3],
    width: f32,
    probe: Probe,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    record: Option<Arc<craft::ui::runtime::inspect::Recorded>>,
    record_layer: Option<egui::LayerId>,
    panel: Rect,
    trace: VecDeque<String>,
    prior_locale: crate::i18n::Locale,
    access_checks: std::collections::HashMap<String, String>,
    callback_targets: Vec<(String, bool)>,
}
impl Fixture {
    fn new(width: f32, locale: crate::i18n::Locale) -> Self {
        let prior_locale = crate::i18n::language();
        crate::i18n::set_language(locale);
        let mut session = Session::new();
        let photos = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (index, photo) in photos.into_iter().enumerate() {
            session
                .catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        photo,
                        Source::Demo { scene: 0 },
                        ["Mixer A.jpg", "Selected B.jpg", "Unselected C.jpg"][index],
                        "JPEG",
                        192,
                        128,
                        "2026-11-21T00:00:00",
                    )),
                })
                .unwrap();
            session.selection = Selection::single(photo);
            let mut develop = (*session.develop_of(photo).unwrap()).clone();
            develop.light.exposure = [0.25, -0.5, 0.75][index];
            for (band_index, band) in MIXER_BANDS.iter().enumerate() {
                for (channel_index, channel) in ["hue", "sat", "lum"].iter().enumerate() {
                    assert!(controls::set(
                        &mut develop,
                        &format!("mixer.{band}.{channel}"),
                        -24.0 + index as f64 * 7.0 + band_index as f64 * 2.0 + channel_index as f64
                    ));
                }
                assert!(controls::set(&mut develop, &format!("bw.{band}"), -28.0 + index as f64 * 9.0 + band_index as f64 * 3.0));
            }
            develop.point_colors = vec![PointColor::default(), PointColor { hue_shift: 19.0 + index as f64, ..Default::default() }];
            session.set_develop(photo, develop, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        // Initial UI fixture configuration only; every tested preset/section/selection action uses real input/commands.
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections = vec!["color".into()];
        app.ui.open_flyouts = vec!["mixer".into()];
        app.ui.mixer_mode = "red".into();
        app.ui.right_width = width;
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}},"label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.redo.is_empty());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        crate::i18n::set_language(locale);
        let mut fixture = Self {
            app,
            ctx,
            photos,
            width,
            probe: Probe::Real,
            tick: 0,
            rects: vec![],
            hits: vec![],
            record: None,
            record_layer: None,
            panel: Rect::NOTHING,
            trace: VecDeque::new(),
            prior_locale,
            access_checks: Default::default(),
            callback_targets: vec![],
        };
        let before = fixture.document();
        fixture.settle();
        assert_eq!(fixture.document(), before, "passive actual parent must not edit the document");
        fixture
    }
    fn normal() -> Self {
        Self::new(crate::state::RIGHT_WIDTH.default, crate::i18n::Locale::En)
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert_eq!(self.app.session.catalog.photos().count(), 3);
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        self.tick += 1;
        let input = format!("{events:?}");
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let width = self.width;
        let probe = self.probe;
        let context = self.ctx.clone();
        let mut final_registry = (vec![], vec![]);
        let mut callback_targets = vec![];
        let mut final_record = None;
        let mut final_record_layer = None;
        let mut panel = Rect::NOTHING;
        let output = self.ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(root.ctx());
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(width);
                ui.allocate_ui_with_layout(egui::vec2(width, 2350.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    panel = ui.max_rect();
                    ui.set_clip_rect(ui.clip_rect().intersect(panel));
                    let mut draw = |ui: &mut egui::Ui| {
                        assert!(!crate::panels::settings::native_controls_active(ui.ctx()));
                        let initial_target = app.session.active().and_then(|id| app.session.develop_of(id)).map(|develop| {
                            (MIXER_BANDS.iter().find(|band| **band == app.ui.mixer_mode).copied().unwrap_or("red").to_owned(), crate::is_bw(&develop))
                        });
                        final_record_layer = Some(ui.layer_id());
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("mixer-band-actual-parent-record"), |ui| {
                            crate::panels::right::show(app, ui);
                        });
                        final_record = record;
                        let observations = crate::widgets::take_registries(ui.ctx());
                        if observations.0.iter().any(|(name, _)| owned(name)) {
                            if let Some((band, bw)) = initial_target {
                                for channel in if bw {
                                    vec![format!("bw.{band}")]
                                } else {
                                    ["hue", "sat", "lum"].iter().map(|key| format!("mixer.{band}.{key}")).collect()
                                } {
                                    assert!(
                                        observations.0.iter().any(|(name, _)| name == &format!("slider:{channel}")),
                                        "scalar rows keep the callback-entry target {channel}, not a mid-callback picker change"
                                    );
                                }
                                callback_targets.push((band, bw));
                            }
                        }
                        final_registry = observations;
                    };
                    match probe {
                        Probe::Real => draw(ui),
                        Probe::Disabled => {
                            ui.add_enabled_ui(false, |ui| draw(ui));
                        }
                        Probe::Sizing => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                        Probe::Measure => craft::ui::runtime::measure::run(&context, || draw(ui)),
                    }
                });
            });
            let registry = std::mem::take(&mut final_registry);
            for (name, _) in registry.0.iter().filter(|(name, _)| owned(name)) {
                assert_eq!(registry.0.iter().filter(|(other, _)| other == name).count(), 1, "duplicate actual-pass locator {name}");
                assert_eq!(registry.1.iter().filter(|(other, _)| other == name).count(), 1, "one real response for {name}");
            }
            final_registry = registry;
        });

        if let Some(update) = &output.platform_output.accesskit_update {
            for (_, node) in &update.nodes {
                if let Some(label) = node.label() {
                    self.access_checks.insert(label.to_owned(), format!("{:?}", node.toggled()));
                }
            }
        }
        self.callback_targets = callback_targets;
        self.panel = panel;
        self.record = final_record;
        self.record_layer = final_record_layer;
        (self.rects, self.hits) = final_registry;
        drain(output);
        self.trace.push_back(format!(
            "frame={} probe={probe:?} width={width} input={input} value={:?} native_focus={:?} sdk_focus={:?} aliases={:?}",
            self.tick,
            (&self.app.ui.mixer_mode, &self.app.ui.tool),
            self.ctx.memory(|memory| memory.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.rects.iter().filter(|(name, _)| owned(name)).collect::<Vec<_>>()
        ));
        while self.trace.len() > 18 {
            self.trace.pop_front();
        }
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, name: &str) -> Rect {
        self.rects.iter().find(|(id, _)| id == name).unwrap_or_else(|| panic!("missing {name}; trace={:#?}", self.trace)).1
    }
    fn hit(&self, name: &str) -> crate::widgets::WidgetHit {
        let hits: Vec<_> = self.hits.iter().filter(|(id, _)| id == name).collect();
        assert_eq!(hits.len(), 1, "one actual {name}; trace={:#?}", self.trace);
        hits[0].1
    }
    fn has(&self, name: &str) -> bool {
        self.rects.iter().any(|(id, _)| id == name)
    }
    fn pointer(&mut self, position: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(position),
            Event::PointerButton { pos: position, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_at(&mut self, position: Pos2) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(position)]);
        self.pointer(position, true);
        self.pointer(position, false);
        self.settle();
    }
    fn click(&mut self, name: &str) {
        self.click_at(self.rect(name).center());
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
    fn focus(&self) -> Option<egui::Id> {
        craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| record.stop.id).or_else(|| self.ctx.memory(|memory| memory.focused()))
    }
    fn focused(&self, name: &str) -> bool {
        self.focus() == Some(self.hit(name).id)
    }
    fn tab_to(&mut self, name: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            if self.focused(name) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {name}; trace={:#?}", self.trace);
    }
    /// Enter the real radio group by Tab only when outside; Flat arrows reach its other members.
    fn focus_band(&mut self, index: usize) {
        assert!(index < MIXER_BANDS.len());
        let mut current = (0..MIXER_BANDS.len()).find(|&at| self.focused(&locator(at)));
        if current.is_none() {
            let selected = MIXER_BANDS.iter().position(|band| *band == self.app.ui.mixer_mode).unwrap_or(0);
            self.tab_to(&locator(selected));
            assert!(self.focused(&locator(selected)), "Tab enters at the selected radio member");
            current = Some(selected);
        }
        let mut at = current.unwrap();
        let before = self.state();
        while at != index {
            let (key, next) = if at < index { (Key::ArrowRight, at + 1) } else { (Key::ArrowLeft, at - 1) };
            self.key(key);
            assert!(self.focused(&locator(next)), "real {key:?} must reach {} from {}; trace={:#?}", MIXER_BANDS[next], MIXER_BANDS[at], self.trace);
            assert_eq!(
                self.state(),
                before,
                "radio arrow traversal changes focus only, never the UI choice, photos, interaction, Undo, Redo or journal"
            );
            at = next;
        }
    }

    fn photos_value(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photos().collect::<Vec<_>>()).unwrap()
    }
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(), "selection":self.app.session.selection,
            "revision":self.app.session.catalog.revision, "undo":format!("{:?}",self.app.session.undo), "redo":format!("{:?}",self.app.session.redo),
            "journal":self.app.session.journal, "commits":self.app.session.commits(), "interaction":format!("{:?}",self.app.session.interaction), "autoSync":self.app.session.auto_sync})
    }
    fn state(&self) -> Value {
        json!({"document":self.document(), "ui":self.app.ui, "dragging":self.app.ui.dragging_control,"workers":self.app.renderer.live_workers(),"segmenting":self.app.session.segmenter.busy()})
    }

    fn choose(&mut self, index: usize) {
        let before = self.state();
        self.click(&locator(index));
        self.assert_choice_only(before, index);
    }
    fn assert_choice_only(&self, mut before: Value, index: usize) {
        assert_eq!(self.app.ui.mixer_mode, MIXER_BANDS[index]);
        before["ui"]["mixerMode"] = json!(MIXER_BANDS[index]);
        assert_eq!(self.state(), before, "only the global native UI band assignment may change; trace={:#?}", self.trace);
    }
    fn treatment(&mut self) {
        self.app.run("develop.treatment", json!({})).unwrap();
        self.settle();
    }
    fn numeric(&mut self, control: &str, value: f64) {
        self.click(&format!("sliderValue:{control}"));
        assert_eq!(
            self.ctx.memory(|memory| memory.focused()),
            Some(crate::text_field::id(&format!("sliderField:{control}"))),
            "real value click must focus the existing editor"
        );
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        let before = self.document();
        self.frame(vec![Event::Text(value.to_string())]);
        assert_eq!(self.document(), before, "typing is not a committed scalar edit");
        self.key(Key::Enter);
        self.settle();
    }
    fn native_scalar(&mut self, control: &str, value: f64) {
        let spec = controls::find(control).unwrap();
        self.app.run("develop.beginInteraction", json!({"label":spec.label})).unwrap();
        self.app.run("develop.set", json!({"control":control,"value":value})).unwrap();
        self.app.run("develop.endInteraction", json!({})).unwrap();
        self.settle();
    }
    fn redo_ready(&mut self) {
        self.app.run("develop.merge", json!({"settings":{"light":{"exposure":0.875}},"label":"Fixture redo"})).unwrap();
        self.app.run("edit.undo", json!({})).unwrap();
        self.settle();
        assert!(!self.app.session.redo.is_empty());
    }
    fn same_as(&self, oracle: &Self) {
        assert_eq!(
            self.state(),
            oracle.state(),
            "the UI must equal exactly the genuine existing command, including all photos/history/UI; trace={:#?}",
            self.trace
        );
    }
    fn idle(&mut self) {
        let before = self.state();
        self.settle();
        assert_eq!(self.state(), before, "no passive replay; trace={:#?}", self.trace);
    }
    fn owner_facts(&self, name: &str) -> &craft::ui::runtime::inspect::Facts {
        let hit = self.hit(name);
        let elements: Vec<_> =
            self.record.as_ref().unwrap().elements.iter().filter(|element| element.id == hit.id && element.part.is_none()).collect();
        assert_eq!(elements.len(), 1, "one actual SDK owner for {name}");
        elements[0].facts.as_ref().unwrap()
    }
    fn palette_geometry(&self) {
        let record = self.record.as_ref().unwrap();
        let mut ids = std::collections::HashSet::new();
        let mut previous: Option<Rect> = None;
        let group_label =
            if crate::is_bw(&self.app.session.develop_of(self.app.session.active().unwrap()).unwrap()) { "B&W Mixer" } else { "Color Mixer" };
        assert_eq!(
            record
                .elements
                .iter()
                .filter(|element| element
                    .facts
                    .as_ref()
                    .is_some_and(|facts| facts.role == "RadioGroup" && facts.name.as_deref() == Some(crate::i18n::tr(group_label))))
                .count(),
            1
        );
        for (index, band) in MIXER_BANDS.iter().enumerate() {
            let name = locator(index);
            let rect = self.rect(&name);
            let hit = self.hit(&name);
            assert!(ids.insert(hit.id));
            let elements: Vec<_> = record.elements.iter().filter(|element| element.id == hit.id && element.part.is_none()).collect();
            assert_eq!(elements.len(), 1);
            let element = elements[0];
            let facts = element.facts.as_ref().unwrap();
            assert_eq!(facts.role, "RadioButton");
            assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr(band)));
            assert_eq!(facts.value.as_ref().unwrap().to_lowercase(), super::BAND_COLORS[index]);
            assert!(
                facts.interactive
                    && !facts.disabled
                    && facts.actions.contains(&"click")
                    && facts.actions.contains(&"focus")
                    && element.stop.is_some()
            );
            let areas: Vec<_> = record.hits.iter().filter(|area| area.owner == hit.id && area.part.is_none()).collect();
            assert_eq!(areas.len(), 1);
            assert_eq!(areas[0].rect, hit.interact_rect);
            assert_eq!(rect, hit.interact_rect);
            assert_eq!(Some(hit.layer_id), self.record_layer);
            assert!(self.panel.expand(1.0).contains_rect(rect));
            assert_eq!(self.ctx.layer_id_at(rect.center()), Some(hit.layer_id));
            if let Some(before) = previous {
                assert!(rect.top() >= before.top() - 1.0);
                if (rect.top() - before.top()).abs() < 1.0 {
                    assert!(rect.left() >= before.right() - 1.0)
                } else {
                    assert!(rect.top() >= before.bottom() - 1.0)
                }
            }
            previous = Some(rect);
            let selected = if MIXER_BANDS.contains(&self.app.ui.mixer_mode.as_str()) { self.app.ui.mixer_mode == *band } else { index == 0 };
            assert_eq!(
                self.access_checks.get(crate::i18n::tr(band)).map(String::as_str),
                Some(if selected { "Some(True)" } else { "Some(False)" }),
                "actual accessible selected cue for {band}"
            );
        }
        assert_eq!(ids.len(), 8);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
        crate::i18n::set_language(self.prior_locale);
    }
}

#[test]
fn actual_color_and_bw_band_choices_are_ui_only_and_preserve_redo() {
    let mut f = Fixture::normal();
    for bw in [false, true] {
        if bw {
            f.treatment();
            f.redo_ready();
        }
        for index in [0, 1, 2, 3, 4, 5, 6, 7, 0] {
            let redo = format!("{:?}", f.app.session.redo);
            f.choose(index);
            f.palette_geometry();
            assert!(!f.app.session.redo.is_empty());
            assert_eq!(format!("{:?}", f.app.session.redo), redo);
            assert!(!f.callback_targets.is_empty());
            assert!(f.callback_targets.iter().all(|(band, mode)| band == MIXER_BANDS[index] && *mode == bw));
            f.idle();
        }
    }
}

#[test]
fn natural_flat_group_tabs_space_enter_and_escape_preserve_document() {
    let mut f = Fixture::new(250.0, crate::i18n::Locale::En);
    assert!(f.rect(&locator(4)).top() > f.rect(&locator(0)).top());
    for bw in [false, true] {
        if bw {
            f.treatment();
            f.redo_ready();
        }
        let document = f.document();
        f.tab_to(&locator(0));
        assert_eq!(f.document(), document);
        // The group has ONE Tab entry. Arrows, not repeated Tab, traverse all eight members.
        let before = f.state();
        for index in 1..MIXER_BANDS.len() {
            f.key(Key::ArrowRight);
            assert!(f.focused(&locator(index)), "forward arrow reaches every wrapped member");
            assert_eq!(f.state(), before);
        }
        for index in (0..MIXER_BANDS.len() - 1).rev() {
            f.key(Key::ArrowLeft);
            assert!(f.focused(&locator(index)), "backward arrow reaches every wrapped member");
            assert_eq!(f.state(), before);
        }
        for index in 0..MIXER_BANDS.len() {
            if index > 0 {
                let before = f.state();
                f.key(Key::ArrowRight);
                assert_eq!(f.state(), before, "arrows move focus without committing the next band");
            }
            assert!(f.focused(&locator(index)), "all eight members are reachable with genuine keys");
            let before = f.state();
            f.key(if index % 2 == 0 { Key::Space } else { Key::Enter });
            f.assert_choice_only(before, index);
            assert_eq!(f.document(), document);
            f.palette_geometry();
        }
        let before = f.state();
        f.key(Key::Tab);
        assert_eq!(f.state(), before);
        assert!(!(0..8).any(|index| f.focused(&locator(index))), "Tab exits the radio group instead of advancing a member");
        f.key_mod(Key::Tab, Modifiers { shift: true, ..Default::default() });
        assert!(f.focused(&locator(7)), "reverse Tab re-enters at the selected member");
        assert_eq!(f.state(), before);
        f.key(Key::Escape);
        assert_eq!(f.state(), before);
        f.focus_band(0);
        let before = f.state();
        f.key(Key::Enter);
        f.assert_choice_only(before, 0);
        let before = f.state();
        f.key_mod(Key::Tab, Modifiers { shift: true, ..Default::default() });
        assert_eq!(f.state(), before);
        assert!(!(0..8).any(|index| f.focused(&locator(index))), "Shift+Tab exits the group without trapping focus");
        f.key(Key::Tab);
        assert!(f.focused(&locator(0)), "forward Tab returns to the newly selected member");
        assert_eq!(f.state(), before);
        f.idle();
    }
}

#[test]
fn real_color_channel_edits_match_native_transactions_and_undo_redo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    for (index, channel, value) in [(2, "hue", 31.0), (5, "sat", -42.0), (7, "lum", 53.0)] {
        f.choose(index);
        oracle.choose(index);
        let control = format!("mixer.{}.{channel}", MIXER_BANDS[index]);
        let before = f.photos_value();
        let undo = f.app.session.undo.len();
        let commits = f.app.session.commits();
        f.numeric(&control, value);
        oracle.native_scalar(&control, value);
        f.same_as(&oracle);
        assert_eq!(controls::get(&f.app.session.develop_of(f.photos[0]).unwrap(), &control), Some(value));
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.commits(), commits + 1);
        assert!(f.app.session.redo.is_empty());
        assert_eq!(f.photos_value()[1], before[1]);
        assert_eq!(f.photos_value()[2], before[2]);
        let edited = f.photos_value();
        for fixture in [&mut f, &mut oracle] {
            fixture.app.run("edit.undo", json!({})).unwrap();
            fixture.settle();
        }
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), before);
        assert_eq!(f.app.session.undo.len(), undo);
        for fixture in [&mut f, &mut oracle] {
            fixture.app.run("edit.redo", json!({})).unwrap();
            fixture.settle();
        }
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), edited);
        f.idle();
    }
}

#[test]
fn auto_sync_color_and_bw_edits_match_native_and_keep_unselected_photo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("develop.autoSync", json!({"on":true})).unwrap();
        fixture.settle();
    }
    for (bw, index, value) in [(false, 3, 40.0), (true, 4, -37.0)] {
        if bw {
            f.treatment();
            oracle.treatment();
        }
        f.choose(index);
        oracle.choose(index);
        let control = if bw { format!("bw.{}", MIXER_BANDS[index]) } else { format!("mixer.{}.lum", MIXER_BANDS[index]) };
        let before = f.photos_value();
        let undo = f.app.session.undo.len();
        let commits = f.app.session.commits();
        f.numeric(&control, value);
        oracle.native_scalar(&control, value);
        f.same_as(&oracle);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.commits(), commits + 1);
        for id in &f.photos[..2] {
            assert_eq!(controls::get(&f.app.session.develop_of(*id).unwrap(), &control), Some(value));
        }
        assert_ne!(f.photos_value()[0], before[0]);
        assert_ne!(f.photos_value()[1], before[1]);
        assert_eq!(f.photos_value()[2], before[2]);
        let edited = f.photos_value();
        for fixture in [&mut f, &mut oracle] {
            fixture.app.run("edit.undo", json!({})).unwrap();
            fixture.settle();
        }
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), before);
        for fixture in [&mut f, &mut oracle] {
            fixture.app.run("edit.redo", json!({})).unwrap();
            fixture.settle();
        }
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), edited);
        f.idle();
    }
}

#[test]
fn band_choice_persists_across_real_photo_treatment_and_panel_changes() {
    let mut f = Fixture::normal();
    f.choose(6);
    f.app.run("library.select", json!({"ids":[f.photos[2].0],"active":f.photos[2].0})).unwrap();
    let document = f.document();
    f.settle();
    assert_eq!(f.document(), document);
    assert_eq!(f.app.ui.mixer_mode, "purple");
    assert!(f.has("slider:mixer.purple.hue"));
    f.treatment();
    assert_eq!(f.app.ui.mixer_mode, "purple");
    assert!(f.has("slider:bw.purple"));
    f.palette_geometry();
    f.click("flyout:mixer");
    assert!(!f.rects.iter().any(|(name, _)| owned(name)));
    let document = f.document();
    f.idle();
    assert_eq!(f.document(), document);
    f.click("flyout:mixer");
    assert_eq!(f.app.ui.mixer_mode, "purple");
    assert!(f.has("slider:bw.purple"));
    f.treatment();
    assert!(f.has("slider:mixer.purple.hue"));
    f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
    f.settle();
    assert_eq!(f.app.ui.mixer_mode, "purple");
    assert!(f.has("slider:mixer.purple.hue"));
    f.idle();
}

#[test]
fn narrow_localized_palette_has_real_owners_colors_and_complete_hit_rectangles() {
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De, crate::i18n::Locale::Fr] {
        for width in [250.0, 270.0] {
            let mut f = Fixture::new(width, locale);
            f.palette_geometry();
            assert!(
                f.rect(&locator(4)).top() > f.rect(&locator(0)).top(),
                "natural SDK metrics wrap eight faces into balanced rows at the narrow parent"
            );
            for index in 0..MIXER_BANDS.len() {
                let before = f.state();
                let rect = f.rect(&locator(index));
                f.click_at(egui::pos2(rect.right() - 1.5, rect.center().y));
                f.assert_choice_only(before, index);
                f.palette_geometry();
            }
            f.treatment();
            f.palette_geometry();
            let before = f.state();
            f.click_at(f.rect(&locator(0)).left_center() + egui::vec2(1.5, 0.0));
            f.assert_choice_only(before, 0);
            f.palette_geometry();
            f.idle();
        }
    }
}

#[test]
fn speculative_disabled_and_pending_release_passes_do_not_publish_or_replay_band_choices() {
    for probe in [Probe::Sizing, Probe::Measure, Probe::Disabled] {
        let mut f = Fixture::normal();
        let at = f.rect(&locator(5)).center();
        let before = f.state();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        assert_eq!(f.state(), before);
        f.probe = probe;
        f.pointer(at, false);
        assert_eq!(f.state(), before);
        match probe {
            Probe::Disabled => {
                for index in 0..8 {
                    assert!(f.owner_facts(&locator(index)).disabled);
                }
            }
            _ => {
                assert!(!f.rects.iter().any(|(name, _)| owned(name)));
                assert!(!f.hits.iter().any(|(name, _)| owned(name)));
                assert!(!f.record.as_ref().is_some_and(|record| record.elements.iter().any(|element| {
                    element.facts.as_ref().is_some_and(|facts| {
                        facts.role == "RadioButton"
                            && facts.name.as_ref().is_some_and(|name| MIXER_BANDS.iter().any(|band| crate::i18n::tr(band) == name))
                    })
                })));
            }
        }
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.choose(5);
        f.idle();
        // A valid old-owner key-down is UI-only; only the held key's later release is stale.
        f.focus_band(4);
        let before = f.state();
        f.frame(vec![space(true)]);
        f.assert_choice_only(before, 4);
        let before = f.state();
        f.probe = probe;
        f.frame(vec![space(false)]);
        assert_eq!(f.state(), before);
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
    }
    let mut f = Fixture::normal();
    f.app.ui.mixer_mode = "invalid-stored-band".into();
    let before = f.state();
    f.settle();
    assert_eq!(f.state(), before);
    f.palette_geometry();
    for probe in [Probe::Sizing, Probe::Measure, Probe::Disabled] {
        f.probe = probe;
        f.frame(vec![]);
        assert_eq!(f.state(), before);
    }
    f.probe = Probe::Real;
    f.settle();
    assert_eq!(f.state(), before);
    f.choose(4);
    f.idle();
}

fn hide_parent(f: &mut Fixture, which: u8) {
    match which {
        0 => {
            f.app.run("section.color", json!({})).unwrap();
        }
        1 => {
            f.app.run("panel.info", json!({})).unwrap();
        }
        2 => {
            f.app.run("library.select", json!({"ids":[]})).unwrap();
        }
        _ => unreachable!(),
    }
    f.settle();
    assert!(!f.rects.iter().any(|(name, _)| owned(name)));
}
fn restore_parent(f: &mut Fixture, which: u8) {
    match which {
        0 => {
            f.app.run("section.color", json!({})).unwrap();
        }
        1 => {
            f.app.run("panel.edit", json!({})).unwrap();
        }
        2 => {
            f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
        }
        _ => unreachable!(),
    }
    f.settle();
}
fn space(pressed: bool) -> Event {
    Event::Key { key: Key::Space, physical_key: Some(Key::Space), pressed, repeat: false, modifiers: Modifiers::NONE }
}

#[test]
fn stale_pointer_and_held_key_releases_do_not_replay_after_parent_transitions() {
    for which in 0..3 {
        let mut f = Fixture::normal();
        let at = f.rect(&locator(5)).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        hide_parent(&mut f, which);
        let before = f.state();
        f.pointer(at, false);
        assert_eq!(f.state(), before);
        restore_parent(&mut f, which);
        assert_eq!(f.app.ui.mixer_mode, "red");
        f.choose(3);
        f.focus_band(6);
        let before = f.state();
        f.frame(vec![space(true)]);
        f.assert_choice_only(before, 6);
        hide_parent(&mut f, which);
        let before = f.state();
        f.frame(vec![space(false)]);
        f.frame(vec![]);
        assert_eq!(f.state(), before);
        restore_parent(&mut f, which);
        assert_eq!(f.app.ui.mixer_mode, "purple");
        f.choose(1);
        f.idle();
    }
    // A photo change does not give this global UI preference new document ownership.
    let mut f = Fixture::normal();
    f.focus_band(2);
    let before = f.state();
    f.frame(vec![space(true)]);
    f.assert_choice_only(before, 2);
    f.app.run("library.select", json!({"ids":[f.photos[1].0,f.photos[2].0],"active":f.photos[1].0})).unwrap();
    f.settle();
    let before = f.state();
    f.frame(vec![space(false)]);
    f.idle();
    assert_eq!(f.state(), before);
    assert_eq!(f.app.ui.mixer_mode, "yellow");
}

#[test]
fn neighbor_point_color_and_tat_actions_remain_distinct_from_band_choice() {
    let mut f = Fixture::normal();
    f.click("flyout:pointColor");
    let document = f.document();
    f.click("pointColor:1");
    assert_eq!(f.app.ui.point_color, 1);
    f.click("pointColorVisualize");
    assert!(f.app.ui.point_color_visualize);
    assert_eq!(f.document(), document);
    f.choose(7);
    assert_eq!(f.app.ui.point_color, 1);
    assert!(f.app.ui.point_color_visualize);
    assert!(f.app.ui.tool.is_empty());
    for channel in ["hue", "sat", "lum"] {
        let document = f.document();
        f.click(&format!("button:tatMixer-{channel}"));
        assert_eq!(f.app.ui.tool, format!("tat:{channel}"));
        assert_eq!(f.document(), document);
        f.choose(0);
        assert_eq!(f.app.ui.tool, format!("tat:{channel}"));
        assert_eq!(f.app.ui.point_color, 1);
        assert!(f.app.ui.point_color_visualize);
        f.click(&format!("button:tatMixer-{channel}"));
        assert!(f.app.ui.tool.is_empty());
        assert_eq!(f.document(), document);
    }
    f.choose(5);
    f.idle();
}
