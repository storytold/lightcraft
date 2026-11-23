//! Deferred Rust regressions: real SDK controls, pointer events, real naming command; no inference.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};

#[derive(Clone, Copy)]
enum Pass { Real, EguiSizing, SdkSizing }

struct Panel {
    app: LightcraftApp,
    view: HeadlessView,
    list: Unnamed,
    now: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
}

impl Panel {
    fn new() -> Self {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let photo = app.session.catalog.photos().next().unwrap().id;
        let mut meta = app.session.catalog.photo(photo).unwrap().meta.clone();
        meta.regions = (0..3).map(|i| lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.1 + f64::from(i) * 0.2, y0: 0.2, x1: 0.25 + f64::from(i) * 0.2, y1: 0.5 },
            kind: lightcraft_meta::RegionKind::Face,
            name: None,
            description: None,
        }).collect();
        app.session.commit("unnamed port fixture", lightcraft_catalog::Op::SetMeta { id: photo, meta: Box::new(meta) }).unwrap();
        let list = Unnamed {
            total: 3, ready: true, ordered: true,
            faces: (0..3).map(|index| UnnamedFace {
                photo: photo.0, index,
                view: lightcraft_geom::Rect { x0: 0.1, y0: 0.2, x1: 0.25, y1: 0.5 },
                suggestion: None,
            }).collect(),
        };
        app.caches.unnamed = Some((app.session.catalog.revision, app.caches.faces_indexed, 0.0, Arc::new(list.clone())));
        let view = HeadlessView::new();
        app.install_framework(&view.ctx);
        Self { app, view, list, now: 0.0, rects: vec![], hits: vec![], elements: vec![] }
    }

    fn frame(&mut self, events: Vec<egui::Event>, pass: Pass) {
        self.now += 1.0 / 60.0;
        let raw = HeadlessView::raw_input(vec2(1200.0, 480.0), 1.0, self.now, events);
        let app = &mut self.app;
        let list = &self.list;
        let mut elements = vec![];
        self.view.run(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("unnamed-port-record"), |ui| {
                    section_header(app, ui, list, pos2(24.0, 48.0), 1150.0);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(24.0, 160.0), pos2(1150.0, 300.0))), |ui| {
                        naming_bar(app, ui);
                    });
                });
                if let Some(record) = record { elements = record.elements.clone(); }
            };
            match pass {
                Pass::Real => draw(ui),
                Pass::EguiSizing => { ui.scope_builder(egui::UiBuilder::new().sizing_pass(), |ui| draw(ui)); }
                Pass::SdkSizing => { craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui)); }
            }
        });
        (self.rects, self.hits) = take_registries(&self.view.ctx);
        self.elements = elements;
    }

    fn target(&self, id: &str) -> Rect {
        let rect = self.rects.iter().find(|(key, _)| key == id).unwrap().1;
        let hit = self.hits.iter().find(|(key, _)| key == id).unwrap().1;
        let element = self.elements.iter().find(|element| element.id == hit.id && element.part.is_none()).expect("automation points at the real SDK component, not a probe");
        assert_eq!(element.component, "Button");
        assert_eq!(element.rect, rect);
        assert_eq!(hit.interact_rect, rect.intersect(hit.interact_rect));
        rect
    }

    fn click(&mut self, id: &str) {
        let pos = self.target(id).center();
        self.frame(vec![egui::Event::PointerMoved(pos)], Pass::Real);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], Pass::Real);
        }
        self.frame(vec![], Pass::Real);
    }

    fn keys(&self) -> std::collections::HashSet<(u64, usize)> {
        self.list.faces.iter().map(|face| (face.photo, face.index)).collect()
    }
}

#[test]
fn unnamed_port_select_all_none_and_clear_keep_exact_face_keys_and_never_name() {
    let mut p = Panel::new();
    let undo = p.app.session.undo.len();
    p.app.ui.unnamed_selected.insert((p.list.faces[0].photo, 1));
    p.app.ui.unnamed_anchor = Some(1);
    p.frame(vec![], Pass::Real);
    p.frame(vec![], Pass::Real);
    p.click("unnamed:selectAll");
    assert_eq!(p.app.ui.unnamed_selected, p.keys());
    assert!(p.view.painted_text().iter().any(|text| text == "Select none"));
    p.click("unnamed:selectAll");
    assert!(p.app.ui.unnamed_selected.is_empty());
    assert_eq!(p.app.ui.unnamed_anchor, Some(1), "select-none preserves the existing range anchor");
    p.click("unnamed:selectAll");
    p.app.ui.unnamed_name = "Not submitted".into();
    p.click("unnamed:clear");
    assert!(p.app.ui.unnamed_selected.is_empty());
    assert!(p.app.ui.unnamed_name.is_empty());
    assert_eq!(p.app.ui.unnamed_anchor, None);
    assert_eq!(p.app.session.undo.len(), undo);
    let photo = lightcraft_catalog::PhotoId(p.list.faces[0].photo);
    assert!(p.app.session.catalog.photo(photo).unwrap().meta.regions.iter().all(|region| region.name.is_none()));
}

#[test]
fn unnamed_port_select_all_then_type_and_return_names_exact_keys_in_one_undo() {
    let mut p = Panel::new();
    p.frame(vec![], Pass::Real);
    p.frame(vec![], Pass::Real);
    p.click("unnamed:selectAll");
    assert_eq!(p.app.ui.unnamed_selected, p.keys());
    let undo = p.app.session.undo.len();
    p.frame(vec![egui::Event::Text("Port Example".into())], Pass::Real);
    for pressed in [true, false] {
        p.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }], Pass::Real);
    }
    let photo = lightcraft_catalog::PhotoId(p.list.faces[0].photo);
    assert!(p.app.session.catalog.photo(photo).unwrap().meta.regions.iter().all(|region| region.name.as_deref() == Some("Port Example")));
    assert_eq!(p.app.session.undo.len(), undo + 1);
    assert!(p.app.ui.unnamed_selected.is_empty() && p.app.ui.unnamed_name.is_empty());
    assert_eq!(p.app.ui.unnamed_anchor, None);
}

#[test]
fn unnamed_port_open_settings_only_when_detection_is_off_and_list_is_empty() {
    let mut p = Panel::new();
    p.list = Unnamed::default();
    p.frame(vec![], Pass::Real);
    p.frame(vec![], Pass::Real);
    p.click("unnamed:openSettings");
    assert_eq!(p.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "faces".into() }));
    p.app.ui.dialog = None;
    let face = UnnamedFace { photo: 17, index: 3, view: lightcraft_geom::Rect { x0: 0.1, y0: 0.2, x1: 0.3, y1: 0.4 }, suggestion: None };
    p.list.faces.push(face);
    p.list.ordered = false;
    p.frame(vec![], Pass::Real);
    p.target("unnamed:selectAll");
    assert!(!p.rects.iter().any(|(id, _)| id == "unnamed:openSettings"));
    assert!(p.view.painted_text().join(" ").contains("Click faces to select them"));
    p.list.faces.clear();
    p.list.ready = true;
    for pending in [7, 0] {
        p.app.caches.faces_pending = pending;
        p.frame(vec![], Pass::Real);
        assert!(!p.rects.iter().any(|(id, _)| id == "unnamed:openSettings" || id == "unnamed:selectAll"));
        let text = p.view.painted_text().join(" ");
        assert!(text.contains(if pending > 0 { "Looking through your photos" } else { "No unnamed faces" }), "{text}");
    }
}

#[test]
fn unnamed_port_sizing_never_registers_or_changes_selection_or_settings() {
    for pass in [Pass::EguiSizing, Pass::SdkSizing] {
        for target in ["unnamed:selectAll", "unnamed:clear", "unnamed:openSettings"] {
            let mut p = Panel::new();
            if target == "unnamed:openSettings" {
                p.list = Unnamed::default();
            } else {
                p.app.ui.unnamed_selected.insert((p.list.faces[0].photo, 1));
                p.app.ui.unnamed_name = "Keep draft".into();
                p.app.ui.unnamed_anchor = Some(1);
            }
            p.frame(vec![], Pass::Real);
            p.frame(vec![], Pass::Real);
            let pos = p.target(target).center();
            let before = (p.app.ui.unnamed_selected.clone(), p.app.ui.unnamed_name.clone(), p.app.ui.unnamed_anchor, p.app.ui.dialog.clone(), p.app.session.undo.len());
            p.frame(vec![egui::Event::PointerMoved(pos)], pass);
            for pressed in [true, false] {
                p.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], pass);
                assert!(!p.rects.iter().any(|(id, _)| id == target));
                assert!(!p.hits.iter().any(|(id, _)| id == target));
            }
            let after = (p.app.ui.unnamed_selected.clone(), p.app.ui.unnamed_name.clone(), p.app.ui.unnamed_anchor, p.app.ui.dialog.clone(), p.app.session.undo.len());
            assert_eq!(before, after);
        }
    }
}
