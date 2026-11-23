//! Actual common Develop/Lens Blur parent, standard SDK buttons, original glyphs and genuine command equivalence.
//! No canvas, files, forced focus, behavioral substitute or native/window visual acceptance.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::RightPanel;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::{FocusArea, FocusShape as S, Mask};
use lightcraft_engine::{Selection, Session};
use lightcraft_geom::Point;
use serde_json::Value;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;

const OPTIONS: [(S, &str, &str); 4] =
    [(S::Auto, "Auto", "auto"), (S::Round, "Round", "round"), (S::Wide, "Horizontal", "wide"), (S::Tall, "Vertical", "tall")];
fn moved_area() -> Value {
    json!(FocusArea { center: Point::new(0.57, 0.43), rx: 0.18, ry: 0.12 })
}
fn locator(index: usize) -> String {
    format!("lensBlurShape:{}", OPTIONS[index].2)
}
fn owned(name: &str) -> bool {
    name.starts_with("lensBlurShape:")
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
    drawn: Vec<(String, Rect)>,
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
                        ["Lens Shape A.jpg", "Selected B.jpg", "Unselected C.jpg"][index],
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
            develop.lens_blur.enabled = true;
            develop.lens_blur.amount = [25.0, 35.0, 45.0][index];
            develop.lens_blur.shape = [S::Auto, S::Wide, S::Tall][index];
            develop.lens_blur.area = if index == 0 { None } else { Some(FocusArea { center: Point::new(0.6, 0.4), rx: 0.23, ry: 0.14 }) };
            develop.masks = [17, 93].map(|id| Mask { id, name: "Same focus name".into(), ..Default::default() }).to_vec();
            session.set_develop(photo, develop, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        // Initial UI fixture configuration only; every tested preset/section/selection action uses real input/commands.
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections = vec!["lensBlur".into()];
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
            drawn: vec![],
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
                        assert!(!crate::panels::settings::native_controls_active(ui.ctx()));
                        final_record_layer = Some(ui.layer_id());
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("lens-blur-shape-actual-parent-record"), |ui| {
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

        fn flatten(shape: &egui::Shape, clip: Rect, bounds: &[Rect], out: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        flatten(shape, clip, bounds, out);
                    }
                }
                _ => {
                    if bounds.iter().any(|rect| rect.expand(1.0).contains_rect(shape.visual_bounding_rect())) {
                        out.push((format!("{shape:?}"), clip));
                    }
                }
            }
        }
        self.drawn.clear();
        let bounds: Vec<_> = final_registry.0.iter().filter(|(name, _)| owned(name)).map(|(_, rect)| *rect).collect();
        for shape in &output.shapes {
            flatten(&shape.shape, shape.clip_rect, &bounds, &mut self.drawn);
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
            self.app.session.develop_of(self.photos[0]).unwrap().lens_blur.clone(),
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
    fn merge(&mut self, patch: Value) {
        self.app.run("develop.merge", json!({"settings":patch})).unwrap();
        self.settle();
    }
    fn choose(&mut self, choice: S) {
        let current = &self.app.session.develop_of(self.photos[0]).unwrap().lens_blur;
        // Test-only oracle of the exact original native command guard; it never drives the tested UI.
        if choice != current.shape || current.area.is_some() {
            self.app
                .run(
                    "develop.merge",
                    json!({"settings":{"lens_blur":{"shape":serde_json::to_value(choice).unwrap(),"area":null}},"label":"Lens Blur Shape"}),
                )
                .unwrap();
        }
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
    fn button_geometry(&self) {
        let record = self.record.as_ref().unwrap();
        let mut owners = std::collections::HashSet::new();
        let mut previous: Option<Rect> = None;
        for (index, (shape, label, _)) in OPTIONS.iter().enumerate() {
            let name = locator(index);
            let rect = self.rect(&name);
            let hit = self.hit(&name);
            assert!(owners.insert(hit.id));
            let elements: Vec<_> = record.elements.iter().filter(|element| element.id == hit.id && element.part.is_none()).collect();
            assert_eq!(elements.len(), 1, "one actual SDK button {name}");
            let element = elements[0];
            let facts = element.facts.as_ref().unwrap();
            assert_eq!(facts.role, "Button");
            assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr(label)));
            assert!(
                facts.interactive
                    && !facts.disabled
                    && facts.actions.contains(&"click")
                    && facts.actions.contains(&"focus")
                    && element.stop.is_some()
            );
            let areas: Vec<_> = record.hits.iter().filter(|area| area.owner == hit.id && area.part.is_none()).collect();
            assert_eq!(areas.len(), 1);
            assert_eq!(areas[0].bounds, rect);
            assert_eq!(areas[0].rect, hit.interact_rect);
            assert_eq!(Some(hit.layer_id), self.record_layer);
            assert!(self.panel.expand(1.0).contains_rect(rect), "no panel widening: {name} {rect:?} panel={:?}", self.panel);
            assert!(hit.interact_rect.expand(1.0).contains_rect(rect), "complete button really senses {name}");
            if let Some(before) = previous {
                assert!(rect.top() >= before.top() - 1.0);
                if (rect.top() - before.top()).abs() < 1.0 {
                    assert!(rect.left() >= before.right() - 1.0);
                } else {
                    assert!(rect.top() >= before.bottom() - 1.0);
                }
            }
            previous = Some(rect);
            // Independently render only the unchanged native glyph helper at the actual face's coordinates.
            // Compare every exact original primitive (including Auto), not a fabricated approximate icon box.
            let tokens = crate::theme::Tokens::get(&self.ctx);
            let colour = if self.app.session.develop_of(self.photos[0]).unwrap().lens_blur.shape == *shape { tokens.accent } else { tokens.icon };
            let reference = egui::Context::default();
            let mut expected = Vec::new();
            let mut out = reference.run_ui(HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, 0.0, vec![]), |ui| {
                let _ = crate::widgets::take_registries(ui.ctx());
                super::paint_focus_shape(ui.painter(), rect.shrink(5.0), *shape, colour);
                let (rects, hits) = crate::widgets::take_registries(ui.ctx());
                assert!(rects.is_empty() && hits.is_empty(), "the original glyph is paint only");
            });
            fn primitives(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            primitives(shape, out);
                        }
                    }
                    egui::Shape::Noop => {}
                    _ => out.push((format!("{shape:?}"), shape.visual_bounding_rect())),
                }
            }
            for shape in &out.shapes {
                primitives(&shape.shape, &mut expected);
            }
            assert!(!expected.is_empty(), "original glyph paints {name}");
            for (drawing, bounds) in &expected {
                let actual: Vec<_> = self.drawn.iter().filter(|(value, _)| value == drawing).collect();
                assert_eq!(
                    actual.len(),
                    expected.iter().filter(|(value, _)| value == drawing).count(),
                    "unchanged original glyph {name}: {drawing}; trace={:#?}",
                    self.trace
                );
                assert!(rect.expand(1.0).contains_rect(*bounds), "glyph fits actual SDK face: {name}");
                assert!(actual.iter().all(|(_, clip)| clip.expand(1.0).contains_rect(*bounds)), "no glyph clipping: {name}");
            }
            out.textures_delta.clear();
            out.platform_output.commands.clear();
            out.viewport_output.clear();
            out.shapes.clear();
        }
        assert_eq!(owners.len(), 4);
        let headings: Vec<_> = self.texts.iter().filter(|text| text.text == crate::i18n::tr("Shape")).collect();
        assert_eq!(headings.len(), 1);
        assert!(headings[0].clip.expand(1.0).contains_rect(headings[0].rect));
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
fn actual_parent_all_four_presets_exact_payload_and_undo_redo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    assert_eq!(f.app.session.selected().len(), 2);
    for index in [1, 2, 3, 0] {
        let before = f.photos_value();
        let commits = f.app.session.commits();
        let undo = f.app.session.undo.len();
        f.click(&locator(index));
        oracle.choose(OPTIONS[index].0);
        f.same_as(&oracle);
        assert_eq!(f.app.session.commits(), commits + 1);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.shape, OPTIONS[index].0);
        assert!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.area.is_none());
        assert_eq!(f.app.session.undo.last().unwrap().label, "Lens Blur Shape");
        for other in [1, 2] {
            assert_eq!(f.photos_value()[other], before[other], "Auto Sync off never touches another photo");
        }
        let after = f.photos_value();
        f.app.run("edit.undo", json!({})).unwrap();
        oracle.app.run("edit.undo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), before);
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), after);
        f.idle();
    }
}

#[test]
fn actual_parent_selected_centered_noop_preserves_redo_and_moved_shape_recenters_once() {
    for (index, (shape, _, value)) in OPTIONS.iter().enumerate() {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        for fixture in [&mut f, &mut oracle] {
            fixture.merge(json!({"lens_blur":{"shape":value,"area":null}}));
            fixture.redo_ready();
        }
        let before = f.state();
        f.click(&locator(index));
        oracle.choose(*shape);
        f.same_as(&oracle);
        assert_eq!(f.state(), before, "centered current preset is a true no-op, including Auto");
        for fixture in [&mut f, &mut oracle] {
            fixture.merge(json!({"lens_blur":{"area":moved_area()}}));
        }
        let before = f.photos_value();
        let commits = f.app.session.commits();
        let undo = f.app.session.undo.len();
        f.click(&locator(index));
        oracle.choose(*shape);
        f.same_as(&oracle);
        assert_eq!(f.app.session.commits(), commits + 1);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.shape, *shape);
        assert!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.area.is_none());
        let after = f.photos_value();
        f.app.run("edit.undo", json!({})).unwrap();
        oracle.app.run("edit.undo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), before);
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), after);
        f.idle();
    }
}

#[test]
fn actual_parent_natural_tab_shift_tab_space_and_enter_preserve_button_actions() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    f.tab_to(&locator(0));
    let before = f.state();
    f.key(Key::Space);
    oracle.choose(S::Auto);
    f.same_as(&oracle);
    assert_eq!(f.state(), before);
    for (index, key) in [(1, Key::Space), (2, Key::Enter), (3, Key::Space)] {
        f.key(Key::Tab);
        assert!(f.focused(&locator(index)), "next genuine standard-button tab stop: trace={:#?}", f.trace);
        f.key(key);
        oracle.choose(OPTIONS[index].0);
        f.same_as(&oracle);
    }
    f.key_mod(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused(&locator(2)));
    f.key(Key::Enter);
    oracle.choose(S::Wide);
    f.same_as(&oracle);
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"area":moved_area()}}));
    }
    assert!(f.focused(&locator(2)));
    let commits = f.app.session.commits();
    f.key(Key::Space);
    oracle.choose(S::Wide);
    f.same_as(&oracle);
    assert_eq!(f.app.session.commits(), commits + 1);
    f.app.run("edit.undo", json!({})).unwrap();
    oracle.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    f.idle();
}

#[test]
fn actual_parent_auto_sync_propagates_changed_fields_only_with_genuine_batch_history() {
    for area_only in [false, true] {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        for fixture in [&mut f, &mut oracle] {
            if area_only {
                fixture.merge(json!({"lens_blur":{"area":moved_area()}}));
            }
            fixture.app.run("develop.autoSync", json!({"on":true})).unwrap();
            fixture.settle();
        }
        let before = f.photos_value();
        let selected_before = f.app.session.develop_of(f.photos[1]).unwrap().lens_blur.clone();
        let index = if area_only { 0 } else { 1 };
        let commits = f.app.session.commits();
        let undo = f.app.session.undo.len();
        f.click(&locator(index));
        oracle.choose(OPTIONS[index].0);
        f.same_as(&oracle);
        assert_eq!(f.app.session.commits(), commits + 1);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.photos_value()[2], before[2], "unselected photo is untouched");
        let selected = &f.app.session.develop_of(f.photos[1]).unwrap().lens_blur;
        if area_only {
            assert_eq!(selected.shape, selected_before.shape, "recenter must not force the active shape onto another selected photo");
            // Native Auto Sync compares serialized keys: the cleared optional area is absent, not a null delta.
            assert_eq!(selected.area, selected_before.area, "native recenter leaves the other selected photo's area unchanged");
            assert!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.area.is_none(), "the active area genuinely recenters");
        } else {
            assert_eq!(selected.shape, S::Round);
            assert_eq!(selected.area, selected_before.area, "unchanged active area is not a sync diff");
        }
        let after = f.photos_value();
        f.app.run("edit.undo", json!({})).unwrap();
        oracle.app.run("edit.undo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), before);
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.photos_value(), after);
        f.idle();
    }
}

#[test]
fn actual_parent_localized_accessible_buttons_keep_original_glyphs_inside_narrow_parent() {
    use crate::i18n::Locale;
    for width in [crate::state::RIGHT_WIDTH.default, crate::state::RIGHT_WIDTH.min] {
        assert!(width == 270.0 || width == 250.0);
        for locale in [Locale::En, Locale::De, Locale::Fr, Locale::Ru, Locale::Fi] {
            let mut f = Fixture::new(width, locale);
            let mut oracle = Fixture::new(width, locale);
            f.button_geometry();
            for index in [1, 2, 3, 0] {
                let rect = f.rect(&locator(index));
                let before = f.document();
                // Actual face edge, not merely the center of its custom glyph.
                f.click_at(Pos2::new(rect.right() - 3.0, rect.center().y));
                oracle.choose(OPTIONS[index].0);
                f.same_as(&oracle);
                assert_ne!(f.document(), before);
                f.frame(vec![Event::PointerMoved(rect.center())]);
                f.tick += 60;
                f.frame(vec![]);
                f.settle();
                let tip = crate::i18n::tr(match OPTIONS[index].0 {
                    S::Auto => "Auto: the photo's subject, estimated",
                    S::Round => "Round",
                    S::Wide => "Horizontal oval",
                    S::Tall => "Vertical oval",
                });
                assert!(
                    f.texts.iter().any(|text| text.text == tip && text.clip.expand(1.0).contains_rect(text.rect)),
                    "full localized native tooltip remains painted: {tip}; texts={:?}",
                    f.texts
                );
                f.same_as(&oracle);
            }
            f.button_geometry();
            f.idle();
        }
    }
}

#[test]
fn actual_parent_apply_off_disabled_and_focus_mask_keep_original_shape_eligibility() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"enabled":false}}));
    }
    let before = f.state();
    for index in 0..4 {
        assert!(f.owner_facts(&locator(index)).disabled);
        f.click(&locator(index));
        assert_eq!(f.state(), before, "Apply off cannot choose/recenter");
    }
    f.same_as(&oracle);
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"enabled":true,"amount":0.0}}));
    }
    f.click(&locator(1));
    oracle.choose(S::Round);
    f.same_as(&oracle);
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().lens_blur.amount, 0.0);
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"focus_mask":17}}));
    }
    assert!(!f.rects.iter().any(|(name, _)| owned(name)), "a real focus mask hides the picture-shape row");
    let before = f.state();
    f.key(Key::Enter);
    assert_eq!(f.state(), before);
    f.same_as(&oracle);
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"focus_mask":null}}));
    }
    assert!(f.has(&locator(2)));
    f.click(&locator(2));
    oracle.choose(S::Wide);
    f.same_as(&oracle);
    f.idle();
}

#[test]
fn actual_parent_sizing_sdk_measurement_and_disabled_input_are_pure_without_replay() {
    for probe in [Probe::Sizing, Probe::Measure, Probe::Disabled] {
        for index in 0..4 {
            let mut f = Fixture::normal();
            let mut oracle = Fixture::normal();
            let point = f.rect(&locator(index)).center();
            let before = f.state();
            f.frame(vec![Event::PointerMoved(point)]);
            f.pointer(point, true);
            assert_eq!(f.state(), before);
            f.probe = probe;
            f.pointer(point, false);
            assert_eq!(f.state(), before);
            if matches!(probe, Probe::Sizing | Probe::Measure) {
                assert!(!f.rects.iter().any(|(name, _)| owned(name)), "non-real pass has no real alias");
            } else {
                for choice in 0..4 {
                    assert!(f.owner_facts(&locator(choice)).disabled);
                }
            }
            f.probe = Probe::Real;
            f.settle();
            f.same_as(&oracle);
            assert_eq!(f.state(), before, "no speculative press is replayed");
            f.tab_to(&locator(1));
            f.probe = probe;
            f.key(Key::Enter);
            assert_eq!(f.state(), before);
            f.probe = Probe::Real;
            f.settle();
            assert_eq!(f.state(), before);
            f.click(&locator(1));
            oracle.choose(S::Round);
            f.same_as(&oracle);
            f.idle();
        }
    }
}

fn context(f: &mut Fixture, which: u8) {
    match which {
        0 => {
            f.app.run("library.select", json!({"ids":[f.photos[1].0,f.photos[2].0],"active":f.photos[1].0})).unwrap();
        }
        1 => {
            f.merge(json!({"lens_blur":{"focus_mask":17}}));
        }
        2 => {
            f.app.run("section.lensBlur", json!({})).unwrap();
        }
        3 => {
            f.app.run("panel.info", json!({})).unwrap();
        }
        4 => {
            f.app.run("library.select", json!({"ids":[]})).unwrap();
        }
        _ => unreachable!(),
    }
    f.settle();
}
fn restore(f: &mut Fixture, which: u8) {
    match which {
        0 | 4 => {
            f.app.run("library.select", json!({"ids":[f.photos[0].0,f.photos[1].0],"active":f.photos[0].0})).unwrap();
        }
        1 => {
            f.merge(json!({"lens_blur":{"focus_mask":null}}));
        }
        2 => {
            f.app.run("section.lensBlur", json!({})).unwrap();
        }
        3 => {
            f.app.run("panel.edit", json!({})).unwrap();
        }
        _ => unreachable!(),
    }
    f.settle();
}

#[test]
fn actual_parent_stale_photo_mask_collapse_hidden_and_empty_context_cannot_replay() {
    for which in 0..5 {
        let mut f = Fixture::normal();
        let mut oracle = Fixture::normal();
        let point = f.rect(&locator(2)).center();
        let original_owner = f.hit(&locator(2)).id;
        f.frame(vec![Event::PointerMoved(point)]);
        f.pointer(point, true);
        context(&mut f, which);
        context(&mut oracle, which);
        let before = f.state();
        f.pointer(point, false);
        f.same_as(&oracle);
        assert_eq!(f.state(), before, "stale pointer must not edit new or hidden owner");
        if which == 0 {
            assert_ne!(f.hit(&locator(2)).id, original_owner, "photo-scoped SDK owners");
        }
        restore(&mut f, which);
        restore(&mut oracle, which);
        f.same_as(&oracle);
        f.click(&locator(3));
        oracle.choose(S::Tall);
        f.same_as(&oracle);
        f.tab_to(&locator(2));
        // Standard buttons activate on key-down. This valid old-photo action must match the genuine command.
        let old_commits = f.app.session.commits();
        let old_undo = f.app.session.undo.len();
        f.frame(vec![Event::Key { key: Key::Space, physical_key: Some(Key::Space), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        oracle.choose(S::Wide);
        f.same_as(&oracle);
        assert_eq!(f.app.session.commits(), old_commits + 1);
        assert_eq!(f.app.session.undo.len(), old_undo + 1);
        // Keep that key held while its owner changes or disappears; only its release is stale.
        context(&mut f, which);
        context(&mut oracle, which);
        let before = f.state();
        f.frame(vec![Event::Key { key: Key::Space, physical_key: Some(Key::Space), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        f.frame(vec![]);
        f.same_as(&oracle);
        assert_eq!(f.state(), before, "stale key release must not change any new/hidden owner, document, UI or history field");
        if which == 0 {
            // A NEW key-down after focus fallback is a legitimate activation of the actual native neighbor.
            assert!(f.focused("button:lensBlurEditShape"));
            let document = f.document();
            f.key(Key::Space);
            oracle.click("button:lensBlurEditShape");
            f.same_as(&oracle);
            assert_eq!(f.document(), document, "fresh Move & Resize activation is UI-only, not a shape/history edit");
            assert_eq!(f.app.ui.tool, "lensBlurShape");
        }
        restore(&mut f, which);
        restore(&mut oracle, which);
        f.same_as(&oracle);
        f.idle();
    }
}

#[test]
fn actual_parent_escape_outside_cancel_and_existing_move_resize_tool_recovery() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    let point = f.rect(&locator(2)).center();
    let before = f.state();
    f.frame(vec![Event::PointerMoved(point)]);
    f.pointer(point, true);
    f.pointer(Pos2::new(700.0, 2700.0), false);
    f.same_as(&oracle);
    assert_eq!(f.state(), before);
    f.frame(vec![Event::PointerMoved(point)]);
    f.pointer(point, true);
    f.key(Key::Escape);
    f.pointer(point, false);
    f.same_as(&oracle);
    assert_eq!(f.state(), before);
    f.click(&locator(2));
    oracle.choose(S::Wide);
    f.same_as(&oracle);
    // The unchanged neighboring native button enters the genuine tool; do not set the tool behaviorally.
    for fixture in [&mut f, &mut oracle] {
        fixture.merge(json!({"lens_blur":{"area":moved_area()}}));
        fixture.click("button:lensBlurEditShape");
        assert_eq!(fixture.app.ui.tool, "lensBlurShape");
    }
    let before = f.photos_value();
    f.click(&locator(0));
    oracle.choose(S::Auto);
    f.same_as(&oracle);
    assert!(f.app.ui.tool.is_empty(), "the unchanged real parent recovers the now-unavailable Move & Resize tool");
    f.app.run("edit.undo", json!({})).unwrap();
    oracle.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    assert_eq!(f.photos_value(), before);
    assert!(f.app.ui.tool.is_empty());
    f.idle();
}
