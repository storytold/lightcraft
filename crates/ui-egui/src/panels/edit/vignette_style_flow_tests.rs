//! Actual common Develop parent, standard SDK radios and real input; expected edits use the original genuine command.
//! No canvas, files, forced focus, behavioral substitute or native/window visual acceptance.
use super::*;
use crate::headless::HeadlessView;
use crate::state::RightPanel;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_develop::VignetteStyle as V;
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;

const OPTIONS: [(V, &str, &str); 3] = [
    (V::HighlightPriority, "Highlight", "highlightPriority"),
    (V::ColorPriority, "Color", "colorPriority"),
    (V::PaintOverlay, "Paint", "paintOverlay"),
];
fn locator(index: usize) -> String {
    format!("button:vignetteStyle-{}", OPTIONS[index].2)
}
fn owned(name: &str) -> bool {
    name.starts_with("button:vignetteStyle-")
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Sizing,
    Measure,
}
#[derive(Clone, Debug)]
struct PaintedText {
    text: String,
    rect: Rect,
    clip: Rect,
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
    texts: Vec<PaintedText>,
    panel: Rect,
    trace: VecDeque<String>,
    prior_locale: crate::i18n::Locale,
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
                        ["Vignette A.jpg", "Selected B.jpg", "Unselected C.jpg"][index],
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
            develop.vignette.amount = [-25.0, -40.0, 10.0][index];
            develop.vignette.midpoint = [35.0, 60.0, 75.0][index];
            session.set_develop(photo, develop, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        // Initial UI fixture configuration only; every tested style/section/selection action uses real input/commands.
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections = vec!["effects".into()];
        app.ui.right_width = width;
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}},"label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.redo.is_empty());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
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
            texts: vec![],
            panel: Rect::NOTHING,
            trace: VecDeque::new(),
            prior_locale,
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
                        final_record_layer = Some(ui.layer_id());
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("vignette-style-actual-parent-record"), |ui| {
                            crate::panels::right::show(app, ui);
                        });
                        final_record = record;
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
            let registry = crate::widgets::take_registries(root.ctx());
            for (name, _) in registry.0.iter().filter(|(name, _)| owned(name)) {
                assert_eq!(registry.0.iter().filter(|(other, _)| other == name).count(), 1, "duplicate actual-pass locator {name}");
                assert_eq!(registry.1.iter().filter(|(other, _)| other == name).count(), 1, "one real response for {name}");
            }
            final_registry = registry;
        });
        fn texts(shape: &egui::Shape, clip: Rect, out: &mut Vec<PaintedText>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push(PaintedText { text: text.galley.job.text.clone(), rect: Rect::from_min_size(text.pos, text.galley.size()), clip })
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        texts(shape, clip, out);
                    }
                }
                _ => {}
            }
        }
        self.texts.clear();
        for shape in &output.shapes {
            texts(&shape.shape, shape.clip_rect, &mut self.texts);
        }
        self.panel = panel;
        self.record = final_record;
        self.record_layer = final_record_layer;
        (self.rects, self.hits) = final_registry;
        drain(output);
        self.trace.push_back(format!(
            "frame={} probe={probe:?} width={width} input={input} value={:?} native_focus={:?} sdk_focus={:?} aliases={:?}",
            self.tick,
            self.app.session.develop_of(self.photos[0]).unwrap().vignette.style,
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
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(), "selection":self.app.session.selection,
            "revision":self.app.session.catalog.revision, "undo":format!("{:?}",self.app.session.undo), "redo":format!("{:?}",self.app.session.redo),
            "journal":self.app.session.journal, "commits":self.app.session.commits(), "interaction":format!("{:?}",self.app.session.interaction), "autoSync":self.app.session.auto_sync})
    }
    fn state(&self) -> Value {
        json!({"document":self.document(), "ui":self.app.ui, "dragging":self.app.ui.dragging_control})
    }
    fn choose(&mut self, choice: V) {
        self.app.run("develop.merge", json!({"settings":{"vignette":{"style":serde_json::to_value(choice).unwrap()}}})).unwrap();
        self.settle();
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
    fn radio_geometry(&self) {
        let record = self.record.as_ref().unwrap();
        assert_eq!(
            record
                .elements
                .iter()
                .filter(|element| element
                    .facts
                    .as_ref()
                    .is_some_and(|facts| facts.role == "RadioGroup" && facts.name.as_deref() == Some(crate::i18n::tr("Vignette"))))
                .count(),
            1,
            "one common keyboard group, not a group per row"
        );
        let radio_count = record
            .elements
            .iter()
            .filter(|element| {
                element.facts.as_ref().is_some_and(|facts| facts.role == "RadioButton")
                    && self.hits.iter().any(|(name, hit)| owned(name) && hit.id == element.id)
            })
            .count();
        assert_eq!(radio_count, 3, "three actual members of the one Vignette group");
        let mut owners = std::collections::HashSet::new();
        for (index, (_, label, _)) in OPTIONS.iter().enumerate() {
            let name = locator(index);
            let rect = self.rect(&name);
            let hit = self.hit(&name);
            assert!(owners.insert(hit.id));
            // Arena members have real SDK sensing owners, not separate native cache entries.
            // Compare both alias boxes to the independently recorded, fully labelled real hit.
            let elements: Vec<_> = record.elements.iter().filter(|element| element.id == hit.id && element.part.is_none()).collect();
            assert_eq!(elements.len(), 1, "one actual SDK owner for {name}");
            let element = elements[0];
            let facts = element.facts.as_ref().unwrap();
            let areas: Vec<_> = record.hits.iter().filter(|area| area.owner == hit.id && area.part.is_none()).collect();
            assert_eq!(areas.len(), 1, "one actual SDK sensing area for {name}");
            assert_eq!(areas[0].bounds, rect, "alias is the completed real labelled owner");
            assert_eq!(areas[0].rect, hit.interact_rect, "alias retains the actual expanded/clipped interaction area");
            assert_eq!(Some(hit.layer_id), self.record_layer, "record and alias belong to the actual parent's layer");
            assert!(facts.interactive && facts.actions.contains(&"click") && facts.actions.contains(&"focus"));
            assert!(element.stop.is_some() && !facts.disabled, "real enabled SDK focus stop for {name}");
            assert!(self.panel.expand(1.0).contains_rect(rect), "no panel widening: {name} {rect:?} panel={:?}", self.panel);
            assert!(hit.interact_rect.expand(1.0).contains_rect(rect), "full label row must really be clickable: {name}");
            assert!(
                record.elements.iter().any(|element| element.id == hit.id
                    && element
                        .facts
                        .as_ref()
                        .is_some_and(|facts| facts.role == "RadioButton" && facts.name.as_deref() == Some(crate::i18n::tr(label)))),
                "actual labelled SDK radio owner {name}"
            );
            let painted: Vec<_> =
                self.texts.iter().filter(|text| text.text == crate::i18n::tr(label) && rect.expand(1.0).intersects(text.rect)).collect();
            assert_eq!(painted.len(), 1, "full localized label is painted once: {name}; texts={:?}", self.texts);
            let text = painted[0];
            assert!(rect.expand(1.0).contains_rect(text.rect), "wrapped label fits its real owner: {name} {text:?}");
            assert!(text.clip.expand(1.0).contains_rect(text.rect), "localized label must not be clipped: {name} {text:?}");
            assert!(
                record.hits.iter().any(|part| part.owner == hit.id && part.bounds.contains(text.rect.center())),
                "label belongs to actual sensing owner"
            );
            if index > 0 {
                assert!((rect.top() - self.rect(&locator(index - 1)).top()).abs() < 1.0, "preserve three-choice order");
                assert!(rect.left() > self.rect(&locator(index - 1)).left());
                assert!(self.rect(&locator(index - 1)).right() <= rect.left() + 1.0, "complete neighbouring labels/hits cannot overlap");
            }
        }
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
fn vignette_style_actual_parent_all_choices_and_selected_reactivation_match_the_genuine_command() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    f.same_as(&oracle);
    for index in [1, 2, 0, 0] {
        f.radio_geometry();
        let undo = f.app.session.undo.len();
        let before = serde_json::to_value(f.app.session.catalog.photo(f.photos[1]).unwrap()).unwrap();
        let outside = serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap();
        let rect = f.rect(&locator(index));
        f.click_at(egui::pos2(rect.right() - 2.0, rect.center().y));
        oracle.choose(OPTIONS[index].0);
        f.same_as(&oracle);
        assert_eq!(f.app.session.undo.len(), undo + 1, "exactly the original single action, including genuine current-style reactivation");
        assert!(f.app.session.redo.is_empty());
        assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[1]).unwrap()).unwrap(), before);
        assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap(), outside);
        assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().vignette.style, OPTIONS[index].0);
        f.idle();
    }
}

#[test]
fn vignette_style_actual_parent_normal_minimum_and_localized_labels_have_complete_real_hits() {
    use crate::i18n::Locale;
    for width in [crate::state::RIGHT_WIDTH.default, crate::state::RIGHT_WIDTH.min] {
        for locale in [Locale::En, Locale::De, Locale::Fr, Locale::Ru, Locale::Fi] {
            let mut f = Fixture::new(width, locale);
            let mut oracle = Fixture::new(width, locale);
            for (index, (choice, _, _)) in OPTIONS.iter().enumerate() {
                f.radio_geometry();
                assert_eq!(f.app.ui.right_width, width);
                let rect = f.rect(&locator(index));
                f.click_at(egui::pos2(rect.right() - 2.0, rect.center().y));
                oracle.choose(*choice);
                f.same_as(&oracle);
            }
        }
    }
}

#[test]
fn vignette_style_actual_parent_one_tab_stop_flat_arrows_and_space_enter_activate_observed_owners() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    let initial = f.state();
    f.tab_to(&locator(0));
    assert_eq!(f.state(), initial, "Tab is not an edit");
    f.key(Key::ArrowRight);
    assert!(f.focused(&locator(1)), "Right must focus real Color; trace={:#?}", f.trace);
    assert_eq!(f.state(), initial, "an arrow is not a command");
    f.key(Key::Space);
    oracle.choose(V::ColorPriority);
    f.same_as(&oracle);
    let before = f.state();
    f.key(Key::ArrowRight);
    assert!(f.focused(&locator(2)), "Right must focus real Paint; trace={:#?}", f.trace);
    assert_eq!(f.state(), before);
    f.key(Key::Enter);
    oracle.choose(V::PaintOverlay);
    f.same_as(&oracle);
    let before = f.state();
    f.key(Key::ArrowLeft);
    assert!(f.focused(&locator(1)));
    f.key(Key::ArrowLeft);
    assert!(f.focused(&locator(0)));
    assert_eq!(f.state(), before);
    f.key(Key::Space);
    oracle.choose(V::HighlightPriority);
    f.same_as(&oracle);
    let before = f.state();
    f.key(Key::Tab);
    assert!(!(0..3).any(|index| f.focused(&locator(index))), "one Tab stop, not one per member");
    assert_eq!(f.state(), before);
    f.key_mod(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused(&locator(0)), "Shift-Tab returns to the selected member; trace={:#?}", f.trace);
    assert_eq!(f.state(), before);
}

#[test]
fn vignette_style_actual_parent_genuine_undo_redo_restores_complete_photo_history_and_other_selection() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    let photos = serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap();
    f.click(&locator(2));
    oracle.choose(V::PaintOverlay);
    f.same_as(&oracle);
    let edited = serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap();
    f.app.run("edit.undo", json!({})).unwrap();
    oracle.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    assert_eq!(serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap(), photos);
    f.app.run("edit.redo", json!({})).unwrap();
    oracle.app.run("edit.redo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    assert_eq!(serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap(), edited);
    f.idle();
}

#[test]
fn vignette_style_actual_parent_auto_sync_preserves_the_existing_selected_batch_and_single_undo_redo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("develop.autoSync", json!({"on":true})).unwrap();
        fixture.settle();
    }
    f.same_as(&oracle);
    let photos = serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap();
    let outside = serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap();
    let undo = f.app.session.undo.len();
    f.click(&locator(1));
    oracle.choose(V::ColorPriority);
    f.same_as(&oracle);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    for photo in [f.photos[0], f.photos[1]] {
        assert_eq!(f.app.session.develop_of(photo).unwrap().vignette.style, V::ColorPriority);
    }
    assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap(), outside);
    let edited = serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap();
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("edit.undo", json!({})).unwrap();
        fixture.settle();
    }
    f.same_as(&oracle);
    assert_eq!(serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap(), photos);
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("edit.redo", json!({})).unwrap();
        fixture.settle();
    }
    f.same_as(&oracle);
    assert_eq!(serde_json::to_value(f.app.session.catalog.photos().collect::<Vec<_>>()).unwrap(), edited);
}

#[test]
fn vignette_style_actual_parent_zero_amount_and_disabled_effect_section_keep_the_existing_style_action() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("develop.merge", json!({"settings":{"vignette":{"amount":0.0}}})).unwrap();
        fixture.app.run("develop.sectionEnabled", json!({"section":"effects","enabled":false})).unwrap();
        fixture.settle();
    }
    f.same_as(&oracle);
    f.radio_geometry();
    f.click(&locator(2));
    oracle.choose(V::PaintOverlay);
    f.same_as(&oracle);
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().vignette.amount, 0.0);
    assert!(!f.app.session.develop_of(f.photos[0]).unwrap().section_enabled("effects"));
}

#[test]
fn vignette_style_actual_parent_disabled_sizing_and_measurement_release_is_inert_without_late_replay() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        for target in 0..3 {
            let mut f = Fixture::normal();
            let mut oracle = Fixture::normal();
            let before = f.state();
            let pos = f.rect(&locator(target)).center();
            f.frame(vec![Event::PointerMoved(pos)]);
            f.pointer(pos, true);
            assert_eq!(f.state(), before);
            f.probe = probe;
            f.pointer(pos, false);
            assert_eq!(f.state(), before, "disabled/speculative release must not call develop.merge; trace={:#?}", f.trace);
            if matches!(probe, Probe::Sizing | Probe::Measure) {
                assert!(!f.rects.iter().any(|(name, _)| owned(name)));
                assert!(!f.hits.iter().any(|(name, _)| owned(name)));
            } else {
                for (name, _) in f.hits.iter().filter(|(name, _)| owned(name)) {
                    assert!(f.owner_facts(name).disabled);
                }
            }
            f.probe = Probe::Real;
            f.settle();
            assert_eq!(f.state(), before, "no release replay");
            f.click(&locator(1));
            oracle.choose(V::ColorPriority);
            f.same_as(&oracle);
        }
    }
}

#[test]
fn vignette_style_actual_parent_speculative_or_disabled_keyboard_cannot_commit_focused_unselected_style() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        f.tab_to(&locator(0));
        f.key(Key::ArrowRight);
        assert!(f.focused(&locator(1)), "trace={:#?}", f.trace);
        let before = f.state();
        f.probe = probe;
        f.key(Key::Enter);
        assert_eq!(f.state(), before, "no speculative keyboard action; trace={:#?}", f.trace);
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before, "no delayed key replay");
        f.click(&locator(1));
        oracle.choose(V::ColorPriority);
        f.same_as(&oracle);
    }
}

#[test]
fn vignette_style_actual_parent_capture_cannot_cross_photo_collapsed_section_hidden_parent_or_empty_selection() {
    for transition in 0..4 {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        let pos = f.rect(&locator(2)).center();
        let initial = f.state();
        f.frame(vec![Event::PointerMoved(pos)]);
        f.pointer(pos, true);
        assert_eq!(f.state(), initial);
        for fixture in [&mut f, &mut oracle] {
            match transition {
                0 => {
                    fixture.app.run("library.select", json!({"ids":[fixture.photos[1].0],"active":fixture.photos[1].0})).unwrap();
                }
                1 => {
                    fixture.app.run("section.effects", json!({})).unwrap();
                }
                2 => {
                    fixture.app.run("panel.info", json!({})).unwrap();
                }
                _ => {
                    fixture.app.run("library.select", json!({"ids":[]})).unwrap();
                }
            }
            fixture.settle();
        }
        f.same_as(&oracle);
        let before = f.state();
        f.pointer(pos, false);
        f.settle();
        assert_eq!(f.state(), before, "capture belongs to the old visible owner; transition={transition} trace={:#?}", f.trace);
        f.same_as(&oracle);
        if transition != 0 {
            assert!(!f.rects.iter().any(|(name, _)| owned(name)));
        }
        for fixture in [&mut f, &mut oracle] {
            match transition {
                0 | 3 => {
                    fixture.app.run("library.select", json!({"ids":[fixture.photos[0].0,fixture.photos[1].0],"active":fixture.photos[0].0})).unwrap();
                }
                1 => {
                    fixture.app.run("section.effects", json!({})).unwrap();
                }
                _ => {
                    fixture.app.run("panel.edit", json!({})).unwrap();
                }
            }
            fixture.settle();
        }
        f.same_as(&oracle);
        f.idle();
        f.click(&locator(1));
        oracle.choose(V::ColorPriority);
        f.same_as(&oracle);
    }
}

#[test]
fn vignette_style_actual_parent_escape_and_release_outside_cancel_without_any_document_or_history_edit() {
    for escape in [false, true] {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        let before = f.state();
        let pos = f.rect(&locator(2)).center();
        f.frame(vec![Event::PointerMoved(pos)]);
        f.pointer(pos, true);
        if escape {
            f.key(Key::Escape);
            f.pointer(pos, false);
        } else {
            f.pointer(egui::pos2(400.0, 2500.0), false);
        }
        f.settle();
        assert_eq!(f.state(), before, "cancellation is not a style or Undo action; trace={:#?}", f.trace);
        f.same_as(&oracle);
        f.idle();
        f.click(&locator(1));
        oracle.choose(V::ColorPriority);
        f.same_as(&oracle);
    }
}
