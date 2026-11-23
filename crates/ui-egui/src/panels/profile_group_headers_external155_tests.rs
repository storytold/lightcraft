//! In-memory SDK regressions. Deferred execution: no Cargo/native work in the source-only lane.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton};

#[derive(Clone, Copy)]
enum Pass { Real, EguiSizing, SdkSizing }

// Two groups deliberately share a title; count and source are independent of their display name.
const GROUPS: [(&str, bool, usize); 4] = [
    ("Artistic", false, 3), ("Artistic", true, 1), ("Favorites", false, 2), ("Empty folder", true, 0),
];

struct Frames {
    view: HeadlessView,
    app: LightcraftApp,
    now: f64,
    width: f32,
    enabled: bool,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
    expanded: Vec<(egui::Id, Option<bool>)>,
    bodies: Vec<bool>,
}

impl Frames {
    fn new() -> Self {
        let view = HeadlessView::new();
        view.ctx.enable_accesskit();
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.install_framework(&view.ctx);
        Self { view, app, now: 0.0, width: 420.0, enabled: true, rects: vec![], hits: vec![], elements: vec![], expanded: vec![], bodies: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        let _ = take_registries(&self.view.ctx);
        self.now += 0.02;
        let raw = HeadlessView::raw_input(vec2(self.width, 600.0), 1.0, self.now, events);
        let mut elements = vec![];
        let mut expanded = vec![];
        let mut bodies = vec![];
        let enabled = self.enabled;
        self.view.run(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                let (_, recorded) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("profile-group-header-fixture"), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        bodies.clear();
                        for (title, imported, count) in GROUPS {
                            bodies.push(profile_group_header(ui, title, imported, count));
                        }
                    });
                });
                if let Some(recorded) = recorded {
                    elements = recorded.elements.clone();
                    expanded = elements.iter().filter(|e| e.component == "ProfileGroupHeader" && e.part.is_none())
                        .map(|e| (e.id, ui.ctx().accesskit_node_builder(e.id, |n| n.is_expanded()).flatten())).collect();
                }
            };
            match pass {
                Pass::Real => draw(ui),
                Pass::EguiSizing => { ui.scope_builder(egui::UiBuilder::new().sizing_pass().invisible(), |ui| draw(ui)); }
                Pass::SdkSizing => { craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui)); }
            }
        });
        (self.rects, self.hits) = take_registries(&self.view.ctx);
        self.elements = elements;
        self.expanded = expanded;
        self.bodies = bodies;
    }

    fn settle(&mut self) { for _ in 0..3 { self.frame(vec![], Pass::Real); } }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Modifiers::NONE };
        for pressed in [true, false] {
            self.frame(vec![Event::ModifiersChanged(modifiers), Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }], Pass::Real);
        }
    }
    fn target(&self, key: &str) -> (Rect, WidgetHit) {
        let rect = self.rects.iter().find(|(k, _)| k == key).unwrap().1;
        let hit = self.hits.iter().find(|(k, _)| k == key).unwrap().1;
        let element = self.elements.iter().find(|e| e.id == hit.id && e.part.is_none()).expect("locator is real SDK response, not a hover probe");
        assert_eq!(element.component, "ProfileGroupHeader");
        assert_eq!(element.rect, rect);
        assert_eq!(hit.interact_rect, rect);
        assert_eq!(rect.height(), 32.0);
        (rect, hit)
    }
    fn state(&self, imported: bool, title: &str) -> Option<bool> {
        self.view.ctx.data(|d| d.get_temp(egui::Id::unique(("profile-group", imported, title))))
    }
    fn click(&mut self, at: egui::Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(at)], Pass::Real);
        for pressed in [true, false] { self.frame(vec![pointer(at, button, pressed)], Pass::Real); }
    }
    fn snapshot(&self) -> (serde_json::Value, usize, usize, u64, Vec<String>) {
        let id = self.app.session.catalog.photos().next().unwrap().id;
        (serde_json::to_value(self.app.session.develop_of(id).unwrap_or_default().as_ref()).unwrap(),
         self.app.session.undo.len(), self.app.session.redo.len(), self.app.session.catalog.revision, self.app.session.profile_favorites.clone())
    }
}

fn pointer(pos: egui::Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn profile_groups_default_open_empty_and_same_named_sources_have_single_real_owner() {
    for width in [280.0, 420.0, 760.0] {
        let mut f = Frames::new(); f.width = width; f.settle();
        assert_eq!(f.bodies, [true; 4]);
        assert_eq!(f.rects.len(), 4);
        assert_eq!(f.hits.len(), 4);
        let owners: Vec<_> = f.elements.iter().filter(|e| e.component == "ProfileGroupHeader").collect();
        assert_eq!(owners.len(), 4, "no nested disclosure node or extra hit/focus owner");
        assert!(owners.iter().all(|e| e.part.is_none() && e.stop.is_some()));
        assert!(owners.iter().all(|e| e.facts.as_ref().is_some_and(|facts| facts.interactive && facts.role == "Button")));
        assert_eq!(owners.iter().filter(|e| e.facts.is_some()).count(), 4);
        for key in ["profileGroup:Artistic", "profileGroup:lut:Artistic", "profileGroup:Favorites", "profileGroup:lut:Empty folder"] {
            let (r, hit) = f.target(key);
            assert_eq!(r.width(), f.rects[0].1.width());
            assert!(r.width() > width - 40.0, "row fills the offered content width");
            assert_eq!(f.expanded.iter().find(|(id, _)| *id == hit.id).unwrap().1, Some(true));
        }
        assert_ne!(f.target("profileGroup:Artistic").1.id, f.target("profileGroup:lut:Artistic").1.id);
        assert_eq!(f.state(false, "Artistic"), None, "default-open is not persisted until activated");
    }
}

#[test]
fn profile_groups_chevron_title_count_and_blank_row_toggle_same_response_independently() {
    for offset in [22.0, 48.0, 200.0, 370.0] {
        let mut f = Frames::new(); f.settle();
        let before = f.snapshot();
        let (rect, hit) = f.target("profileGroup:Artistic");
        let at = if offset == 370.0 { pos2(rect.right() - 22.0, rect.center().y) } else { pos2(rect.left() + offset, rect.center().y) };
        f.click(at, PointerButton::Primary);
        assert_eq!(f.state(false, "Artistic"), Some(false));
        assert_eq!(f.state(true, "Artistic"), None);
        f.frame(vec![], Pass::Real);
        assert_eq!(f.bodies, [false, true, true, true]);
        assert_eq!(f.target("profileGroup:Artistic").1.id, hit.id);
        assert_eq!(f.expanded.iter().find(|(id, _)| *id == hit.id).unwrap().1, Some(false));
        let imported = f.target("profileGroup:lut:Artistic").0.center();
        f.click(imported, PointerButton::Primary); f.frame(vec![], Pass::Real);
        assert_eq!(f.bodies, [false, false, true, true]);
        f.click(at, PointerButton::Primary); f.frame(vec![], Pass::Real);
        assert_eq!(f.bodies, [true, false, true, true]);
        let empty = f.target("profileGroup:lut:Empty folder").0.center();
        f.click(empty, PointerButton::Primary); f.frame(vec![], Pass::Real);
        assert_eq!(f.state(true, "Empty folder"), Some(false));
        assert_eq!(f.bodies, [true, false, true, false]);
        assert!(f.view.painted_text().iter().any(|s| s == "0"));
        assert_eq!(f.snapshot(), before, "disclosure cannot mutate settings, history, catalog or favorites");
    }
}

#[test]
fn profile_groups_unforced_tab_enter_space_and_shift_tab_visit_each_owner_once() {
    let mut f = Frames::new(); f.settle();
    let before = f.snapshot();
    let first = f.target("profileGroup:Artistic").1.id;
    let second = f.target("profileGroup:lut:Artistic").1.id;
    assert_eq!(f.view.ctx.memory(|m| m.focused()), None);
    f.key(Key::Tab, false);
    assert_eq!(f.view.ctx.memory(|m| m.focused()), Some(first));
    f.key(Key::Enter, false);
    assert_eq!(f.state(false, "Artistic"), Some(false));
    f.key(Key::Tab, false);
    assert_eq!(f.view.ctx.memory(|m| m.focused()), Some(second));
    f.key(Key::Space, false);
    assert_eq!(f.state(true, "Artistic"), Some(false));
    f.key(Key::Tab, true);
    assert_eq!(f.view.ctx.memory(|m| m.focused()), Some(first));
    f.key(Key::Space, false);
    assert_eq!(f.state(false, "Artistic"), Some(true));
    assert_eq!(f.state(true, "Artistic"), Some(false));
    assert_eq!(f.snapshot(), before);
}

#[test]
fn profile_groups_secondary_cancelled_and_disabled_clicks_do_not_toggle() {
    let mut f = Frames::new(); f.settle();
    let at = f.target("profileGroup:Artistic").0.center();
    f.click(at, PointerButton::Secondary);
    assert_eq!(f.state(false, "Artistic"), None);
    f.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, true)], Pass::Real);
    let outside = pos2(12.0, 500.0);
    f.frame(vec![Event::PointerMoved(outside)], Pass::Real);
    f.frame(vec![pointer(outside, PointerButton::Primary, false)], Pass::Real);
    assert_eq!(f.state(false, "Artistic"), None);
    f.enabled = false; f.settle();
    f.click(at, PointerButton::Primary);
    assert_eq!(f.state(false, "Artistic"), None);
}

#[test]
fn profile_groups_both_sizing_modes_publish_nothing_and_cannot_persist_release() {
    for pass in [Pass::EguiSizing, Pass::SdkSizing] {
        let mut f = Frames::new(); f.settle();
        let before = f.snapshot();
        let at = f.target("profileGroup:Artistic").0.center();
        f.frame(vec![Event::PointerMoved(at)], Pass::Real);
        f.frame(vec![pointer(at, PointerButton::Primary, true)], Pass::Real);
        f.frame(vec![pointer(at, PointerButton::Primary, false)], pass);
        assert!(f.rects.is_empty() && f.hits.is_empty());
        for (title, imported, _) in GROUPS { assert_eq!(f.state(imported, title), None); }
        assert_eq!(f.snapshot(), before);
        // Existing closed state also survives measurement without being reinitialized.
        f.view.ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("profile-group", true, "Artistic")), false));
        f.frame(vec![], pass);
        assert_eq!(f.state(true, "Artistic"), Some(false));
        assert!(f.rects.is_empty() && f.hits.is_empty());
    }
}

#[test]
fn profile_groups_translation_count_and_original_face_positions_are_preserved() {
    struct Restore(crate::i18n::Locale);
    impl Drop for Restore { fn drop(&mut self) { crate::i18n::set_language(self.0); } }
    let _restore = Restore(crate::i18n::language());
    crate::i18n::set_language(crate::i18n::Locale::De);
    let mut f = Frames::new(); f.settle();
    let painted = f.view.painted_text();
    assert!(painted.iter().any(|s| s == crate::i18n::builtin_label("Favorites", true)));
    assert!(painted.iter().any(|s| s == "Artistic"), "imported label stays literal");
    for count in ["3", "1", "2", "0"] { assert!(painted.iter().any(|s| s == count)); }
    fn shapes<'a>(shape: &'a egui::Shape, texts: &mut Vec<&'a egui::epaint::TextShape>, paths: &mut Vec<&'a egui::epaint::PathShape>) {
        match shape {
            egui::Shape::Vec(v) => for s in v { shapes(s, texts, paths); },
            egui::Shape::Text(s) => texts.push(s), egui::Shape::Path(s) => paths.push(s), _ => {},
        }
    }
    let mut texts = vec![]; let mut paths = vec![];
    for s in &f.view.shapes { shapes(&s.shape, &mut texts, &mut paths); }
    let r = f.target("profileGroup:Artistic").0;
    assert!(texts.iter().any(|s| s.galley.job.text == "3" && (s.pos.x + s.galley.size().x - (r.right() - 22.0)).abs() < 0.01));
    assert!(texts.iter().any(|s| (s.pos.x - (r.left() + 36.0)).abs() < 0.01 && (s.pos.y + s.galley.size().y / 2.0 - r.center().y).abs() < 0.01));
    assert!(paths.iter().any(|s| s.points == vec![pos2(r.left() + 18.0, r.center().y - 2.0), pos2(r.left() + 22.0, r.center().y + 2.0), pos2(r.left() + 26.0, r.center().y - 2.0)]));
}
