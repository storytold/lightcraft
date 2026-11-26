//! Actual common right Crop parent, the Overlay dropdown (a framework value menu) and real input only.
//! No canvas, files, forced focus, behavioral substitute or native/window visual acceptance.
use super::*;
use crate::headless::HeadlessView;
use crate::state::{CropOverlay as O, RightPanel};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;

const OPTIONS: [(O, &str, &str); 7] = [
    (O::Thirds, "Thirds", "thirds"),
    (O::Grid, "Grid", "grid"),
    (O::Golden, "Golden Ratio", "golden"),
    (O::Diagonal, "Diagonal", "diagonal"),
    (O::Triangle, "Triangle", "triangle"),
    (O::Spiral, "Spiral", "spiral"),
    (O::None, "None", "none"),
];
const FLIP: &str = "button:cropOverlayOrient";
const FACE: &str = "dropdown:cropOverlay";
/// The menu row of choice `index`.
fn locator(index: usize) -> String {
    format!("cropOverlay:{}", OPTIONS[index].2)
}
fn owned(name: &str) -> bool {
    name == FACE || name.starts_with("cropOverlay:") || name == FLIP
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
        let photos = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (index, photo) in photos.into_iter().enumerate() {
            session
                .catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        photo,
                        Source::Demo { scene: 0 },
                        if index == 0 { "Crop A.jpg" } else { "Other B.jpg" },
                        "JPEG",
                        192,
                        128,
                        "2026-11-21T00:00:00",
                    )),
                })
                .unwrap();
            session.selection = Selection::single(photo);
            let mut develop = (*session.develop_of(photo).unwrap()).clone();
            develop.light.exposure = if index == 0 { 0.25 } else { -0.5 };
            session.set_develop(photo, develop, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        app.run("panel.crop", json!({})).unwrap();
        assert_eq!(app.ui.right, RightPanel::Crop);
        app.ui.right_width = width;
        app.ui.crop_overlay_orient = 3; // Initial saved view preference, before any input.
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
        assert_eq!(self.app.session.catalog.photos().count(), 2);
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
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("crop-overlay-actual-parent-record"), |ui| {
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
        fn texts(shape: &egui::Shape, out: &mut Vec<PaintedText>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push(PaintedText { text: text.galley.job.text.clone(), rect: Rect::from_min_size(text.pos, text.galley.size()) })
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        texts(shape, out);
                    }
                }
                _ => {}
            }
        }
        self.texts.clear();
        for shape in &output.shapes {
            texts(&shape.shape, &mut self.texts);
        }
        self.panel = panel;
        self.record = final_record;
        self.record_layer = final_record_layer;
        (self.rects, self.hits) = final_registry;
        drain(output);
        self.trace.push_back(format!(
            "frame={} probe={probe:?} width={width} input={input} value={:?} orient={} native_focus={:?} sdk_focus={:?} aliases={:?}",
            self.tick,
            self.app.ui.crop_overlay,
            self.app.ui.crop_overlay_orient,
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
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(), "selection":self.app.session.selection,
            "revision":self.app.session.catalog.revision, "undo":format!("{:?}",self.app.session.undo), "redo":format!("{:?}",self.app.session.redo),
            "journal":self.app.session.journal, "commits":self.app.session.commits(), "interaction":format!("{:?}",self.app.session.interaction)})
    }
    fn state(&self) -> Value {
        json!({"document":self.document(), "ui":self.app.ui, "dragging":self.app.ui.dragging_control})
    }
    fn expect_choice(&self, mut expected: Value, choice: O) {
        expected["ui"]["cropOverlay"] = json!(choice);
        assert_eq!(self.state(), expected, "only the chosen view preference may change; trace={:#?}", self.trace);
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
    /// Whether the Overlay menu is open: its rows are live.
    fn menu_open(&self) -> bool {
        self.rects.iter().any(|(name, _)| name.starts_with("cropOverlay:"))
    }
    /// Opens the menu from its face with a real click (unless it is open already).
    fn open(&mut self) {
        if !self.menu_open() {
            self.click(FACE);
        }
        assert!(self.menu_open(), "the face opens the menu; trace={:#?}", self.trace);
        self.menu_geometry();
    }
    /// Picks choice `index` with real clicks: the face, then its row.
    fn choose(&mut self, index: usize) {
        self.open();
        self.click(&locator(index));
        assert!(!self.menu_open(), "a pick closes the menu; trace={:#?}", self.trace);
    }
    /// The face names the chosen overlay; each of the seven rows is one real, labelled SDK owner with its
    /// localized label painted inside it.
    fn menu_geometry(&self) {
        let face = self.rect(FACE);
        assert_eq!(Some(self.hit(FACE).layer_id), self.record_layer, "the face belongs to the actual parent's layer");
        assert!(self.panel.expand(1.0).contains_rect(face), "no panel widening: {face:?} panel={:?}", self.panel);
        let shown = OPTIONS.iter().find(|(choice, _, _)| *choice == self.app.ui.crop_overlay).map_or("", |(_, label, _)| crate::i18n::tr(label));
        assert!(
            self.texts.iter().any(|text| text.text == shown && face.expand(1.0).intersects(text.rect)),
            "the face says {shown}; texts={:?}",
            self.texts
        );
        let mut owners = std::collections::HashSet::new();
        for (index, (_, label, _)) in OPTIONS.iter().enumerate() {
            let name = locator(index);
            let rect = self.rect(&name);
            let hit = self.hit(&name);
            assert!(owners.insert(hit.id), "one owner per row");
            let painted: Vec<_> =
                self.texts.iter().filter(|text| text.text == crate::i18n::tr(label) && rect.expand(1.0).intersects(text.rect)).collect();
            assert_eq!(painted.len(), 1, "full localized label is painted once: {name}; texts={:?}", self.texts);
            assert!(rect.expand(1.0).contains_rect(painted[0].rect), "the whole localized label sits in its row: {name} {:?} {rect:?}", painted[0]);
            if index > 0 {
                assert!(rect.top() >= self.rect(&locator(index - 1)).bottom() - 1.0, "rows keep their order: {name}");
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
fn crop_overlay_actual_parent_all_seven_pointer_choices_change_only_the_view_preference() {
    let mut f = Fixture::normal();
    assert!(f.has(FACE) && !f.menu_open());
    for (index, (choice, _, _)) in OPTIONS.iter().enumerate() {
        let before = f.state();
        f.open();
        assert_eq!(f.state(), before, "opening the menu is not a choice");
        f.click(&locator(index));
        assert!(!f.menu_open());
        f.expect_choice(before, *choice);
        assert_eq!(f.has(FLIP), matches!(choice, O::Triangle | O::Spiral));
        f.idle();
    }
}

#[test]
fn crop_overlay_actual_parent_normal_and_minimum_panel_localized_labels_have_complete_real_hits() {
    use crate::i18n::Locale;
    for width in [crate::state::RIGHT_WIDTH.default, crate::state::RIGHT_WIDTH.min] {
        for locale in [Locale::En, Locale::De, Locale::Fr, Locale::Ru, Locale::Fi] {
            let mut f = Fixture::new(width, locale);
            for (index, (choice, _, _)) in OPTIONS.iter().enumerate() {
                assert_eq!(f.app.ui.right_width, width, "no widening the user's panel preference");
                let before = f.state();
                f.choose(index);
                f.expect_choice(before, *choice);
                // the closed face names the new choice in the user's language
                f.open();
                f.key(Key::Escape);
                assert!(!f.menu_open());
            }
        }
    }
}

#[test]
fn crop_overlay_actual_parent_tab_enter_arrows_and_space_pick_observed_owners() {
    for activate in [Key::Enter, Key::Space] {
        let mut f = Fixture::normal();
        let untouched = f.state();
        f.tab_to(FACE);
        assert_eq!(f.state(), untouched, "Tab traversal is not a develop or preference edit");
        f.key(activate);
        assert!(f.menu_open(), "the key opens the menu; trace={:#?}", f.trace);
        assert!(f.focused(&locator(0)), "the chosen Thirds is the menu's first stop; trace={:#?}", f.trace);
        assert_eq!(f.state(), untouched, "opening is not a choice");
        f.key(Key::ArrowDown);
        assert!(f.focused(&locator(1)), "Down moves to Grid; trace={:#?}", f.trace);
        assert_eq!(f.state(), untouched, "arrows focus without choosing");
        f.key(activate);
        assert!(!f.menu_open());
        f.expect_choice(untouched, O::Grid);
        f.idle();
    }
}

#[test]
fn crop_overlay_actual_parent_conditional_flip_cycles_once_and_retains_orientation_across_choices() {
    let mut f = Fixture::normal();
    assert!(!f.has(FLIP));
    let before = f.state();
    f.choose(4);
    f.expect_choice(before, O::Triangle);
    assert!(f.has(FLIP));
    let before = f.state();
    f.click(FLIP);
    let mut expected = before;
    expected["ui"]["cropOverlayOrient"] = json!(0);
    assert_eq!(f.state(), expected, "retained empty-payload orientation command advances once");
    let before = f.state();
    f.choose(1);
    f.expect_choice(before, O::Grid);
    assert!(!f.has(FLIP));
    assert_eq!(f.app.ui.crop_overlay_orient, 0);
    let before = f.state();
    f.choose(5);
    f.expect_choice(before, O::Spiral);
    assert!(f.has(FLIP));
    assert_eq!(f.app.ui.crop_overlay_orient, 0);
    f.tab_to(FLIP);
    for (key, orientation) in [(Key::Enter, 1), (Key::Space, 2)] {
        let mut expected = f.state();
        expected["ui"]["cropOverlayOrient"] = json!(orientation);
        f.key(key);
        assert_eq!(f.state(), expected, "one natural keyboard activation");
        f.idle();
    }
    for orientation in [3, 0] {
        let mut expected = f.state();
        expected["ui"]["cropOverlayOrient"] = json!(orientation);
        f.click(FLIP);
        assert_eq!(f.state(), expected, "exact four-state cycle");
    }
    f.idle();
}

#[test]
fn crop_overlay_actual_parent_disabled_sizing_and_measurement_never_publish_speculative_choices_or_replay_releases() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        for row in [false, true] {
            let mut f = Fixture::normal();
            f.choose(4);
            let target = if row {
                f.open();
                locator(1)
            } else {
                FLIP.to_owned()
            };
            let before = f.state();
            let pos = f.rect(&target).center();
            f.frame(vec![Event::PointerMoved(pos)]);
            f.pointer(pos, true);
            assert_eq!(f.state(), before, "press alone is not activation");
            f.probe = probe;
            f.pointer(pos, false);
            assert_eq!(f.state(), before, "no speculative/disabled release action; trace={:#?}", f.trace);
            if matches!(probe, Probe::Sizing | Probe::Measure) {
                assert!(!f.rects.iter().any(|(name, _)| owned(name)), "speculative passes publish no targets");
                assert!(!f.hits.iter().any(|(name, _)| owned(name)));
            } else if !row {
                assert!(f.owner_facts(FLIP).disabled, "disabled owner {FLIP}");
            }
            f.probe = Probe::Real;
            f.settle();
            assert_eq!(f.state(), before, "reenabling cannot replay");
            let before = f.state();
            f.choose(1);
            f.expect_choice(before, O::Grid);
        }
    }
}

#[test]
fn crop_overlay_actual_parent_speculative_or_disabled_keyboard_cannot_choose_a_focused_row() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        let mut f = Fixture::normal();
        f.tab_to(FACE);
        f.key(Key::Enter);
        f.key(Key::ArrowDown);
        assert!(f.focused(&locator(1)));
        let before = f.state();
        f.probe = probe;
        f.key(Key::Enter);
        assert_eq!(f.state(), before, "probe cannot turn focus into a choice; trace={:#?}", f.trace);
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before, "no late keyboard replay");
        let before = f.state();
        f.choose(1);
        f.expect_choice(before, O::Grid);
    }
}

#[test]
fn crop_overlay_actual_parent_stale_capture_cannot_cross_photo_hidden_parent_or_missing_selection() {
    for change in ["photo", "hidden", "selection"] {
        let mut f = Fixture::normal();
        f.open();
        let pos = f.rect(&locator(1)).center();
        f.frame(vec![Event::PointerMoved(pos)]);
        f.pointer(pos, true);
        assert_eq!(f.app.ui.crop_overlay, O::Thirds);
        match change {
            "photo" => {
                f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
            }
            "hidden" => {
                f.app.run("panel.info", json!({})).unwrap();
            }
            _ => {
                f.app.run("library.select", json!({"ids":[]})).unwrap();
            }
        }
        let document = f.document();
        f.frame(vec![]);
        assert_eq!(f.document(), document, "drawing the new parent does not edit either photo");
        assert_eq!(f.app.ui.crop_overlay, O::Thirds);
        let before = f.state();
        f.pointer(pos, false);
        assert_eq!(f.state(), before, "old release has no new owner action; trace={:#?}", f.trace);
        if change == "photo" {
            assert!(!f.menu_open(), "the other photo's panel has no open menu; trace={:#?}", f.trace);
        } else {
            assert!(!f.rects.iter().any(|(name, _)| owned(name)));
        }
        if change == "hidden" {
            f.app.run("panel.crop", json!({})).unwrap();
        }
        if change == "selection" {
            f.app.run("library.select", json!({"ids":[f.photos[1].0],"active":f.photos[1].0})).unwrap();
        }
        let before = f.state();
        f.settle();
        assert_eq!(f.state(), before, "returning does not replay old capture");
        let before = f.state();
        f.choose(1);
        f.expect_choice(before, O::Grid);
        f.idle();
    }
}

#[test]
fn crop_overlay_actual_parent_escape_and_release_outside_cancel_without_late_view_or_document_edits() {
    for escape in [false, true] {
        let mut f = Fixture::normal();
        let before = f.state();
        f.open();
        let pos = f.rect(&locator(1)).center();
        f.frame(vec![Event::PointerMoved(pos)]);
        f.pointer(pos, true);
        if escape {
            f.key(Key::Escape);
        }
        let release = if escape { pos } else { egui::pos2(1050.0, 2500.0) };
        f.frame(vec![Event::PointerMoved(release)]);
        f.pointer(release, false);
        assert_eq!(f.state(), before, "cancelled pointer capture must not activate; trace={:#?}", f.trace);
        f.idle();
        let before = f.state();
        f.choose(1);
        f.expect_choice(before, O::Grid);
    }
}

#[test]
fn crop_overlay_actual_parent_retained_genuine_redo_and_undo_do_not_revert_view_preferences() {
    let mut f = Fixture::normal();
    f.choose(5);
    f.click(FLIP);
    let first = json!(f.app.session.catalog.photo(f.photos[0]).unwrap());
    let other = json!(f.app.session.catalog.photo(f.photos[1]).unwrap());
    let ui = json!(f.app.ui);
    f.app.run("edit.redo", json!({})).unwrap();
    f.settle();
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().light.exposure, 0.9);
    assert_eq!(json!(f.app.session.catalog.photo(f.photos[1]).unwrap()), other);
    assert_eq!(json!(f.app.ui), ui, "view-only choice creates no Undo step");
    f.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    assert_eq!(json!(f.app.session.catalog.photo(f.photos[0]).unwrap()), first, "original complete photo is restored by its genuine undo");
    assert_eq!(json!(f.app.session.catalog.photo(f.photos[1]).unwrap()), other);
    assert_eq!(json!(f.app.ui), ui);
    assert!(!f.app.session.redo.is_empty());
    f.idle();
}
