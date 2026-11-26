//! Raw-input tests of the production person page, not a copied button or handler.
//! The cached empty page intentionally avoids renderer/model work; synthetic catalog faces
//! still exercise the real person-filter engine path.
use std::sync::Arc;

use super::*;
use crate::headless::HeadlessView;
use crate::state::{Dialog, ViewMode};
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};

const CANONICAL: &str = "Jane Doe";
const REQUESTED: &str = "jane doe";

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    sizing: bool,
    measuring: bool,
    disabled: bool,
    nodes: Vec<egui::accesskit::Node>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        craft::ui::install(&ctx, craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows)))
            .unwrap();
        let mut session = lightcraft_engine::Session::new();
        for (n, name) in [(1, CANONICAL), (2, REQUESTED), (3, "John Roe")] {
            let mut photo = Photo::new(PhotoId(n), Source::Demo { scene: 0 }, "synthetic.jpg", "JPEG", 32, 32, "2026-01-01T00:00:00");
            photo.meta.regions = vec![lightcraft_meta::Region {
                rect: lightcraft_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.5, y1: 0.5 },
                kind: lightcraft_meta::RegionKind::Face,
                name: Some(name.into()),
                description: None,
            }];
            session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.view = ViewMode::People;
        app.ui.person_page = Some(REQUESTED.into());
        app.caches.person_page = Some((
            REQUESTED.into(),
            app.session.catalog.revision,
            app.caches.faces_indexed,
            0.0,
            Arc::new(PersonPage { name: CANONICAL.into(), ..Default::default() }),
        ));
        Self { ctx, app, time: 0.0, rects: vec![], hits: vec![], sizing: false, measuring: false, disabled: false, nodes: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(1200.0, 700.0), 1.0, self.time, events);
        let ctx = self.ctx.clone();
        let mut out = ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                ui.add_enabled_ui(!self.disabled, |ui| {
                    if self.sizing {
                        let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
                        show(&mut self.app, &mut child, REQUESTED);
                    } else {
                        show(&mut self.app, ui, REQUESTED);
                    }
                });
            };
            if self.measuring {
                craft::ui::runtime::measure::run(&ctx, || draw(ui));
            } else {
                draw(ui);
            }
        });
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            self.nodes.extend(update.nodes.into_iter().map(|(_, node)| node));
        }
        (self.rects, self.hits) = take_registries(&ctx);
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }

    fn at(&self, id: &str) -> Pos2 {
        self.rects.iter().find(|(name, _)| name == id).unwrap().1.center()
    }

    fn click(&mut self, id: &str, button: PointerButton) {
        let at = self.at(id);
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        self.frame(vec![pointer(at, button, false)]);
    }

    fn pristine(&self) {
        assert!(self.app.ui.dialog.is_none());
        assert_eq!(self.app.ui.view, ViewMode::People);
        assert!(self.app.session.filter.person.is_none());
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn named_person_rename_primary_release_uses_canonical_identity_and_preserves_catalog() {
    let mut f = Frames::new();
    f.settle();
    let undo = f.app.session.undo.len();
    let before = f.app.session.catalog.photos().map(|p| p.meta.regions.clone()).collect::<Vec<_>>();
    f.click("person:rename", PointerButton::Primary);
    assert!(matches!(&f.app.ui.dialog, Some(Dialog::RenamePerson { from, to, merge: false }) if from == CANONICAL && to == CANONICAL));
    assert_eq!(f.app.session.undo.len(), undo);
    assert_eq!(f.app.session.catalog.photos().map(|p| p.meta.regions.clone()).collect::<Vec<_>>(), before);
    assert!(f.app.session.filter.person.is_none());
}

#[test]
fn named_person_show_photos_filters_the_engine_then_opens_grid() {
    let mut f = Frames::new();
    f.settle();
    f.click("person:photos", PointerButton::Primary);
    assert_eq!(f.app.session.filter.person.as_deref(), Some(CANONICAL));
    assert_eq!(f.app.ui.view, ViewMode::PhotoGrid);
    let mut visible = f.app.session.visible_cloned();
    visible.sort();
    assert_eq!(visible, [PhotoId(1), PhotoId(2)]);
    assert!(f.app.ui.dialog.is_none());
}

#[test]
fn named_person_secondary_cancel_and_disabled_controls_do_not_dispatch() {
    for id in ["person:rename", "person:photos"] {
        let mut f = Frames::new();
        f.settle();
        f.click(id, PointerButton::Secondary);
        f.pristine();
        let at = f.at(id);
        f.frame(vec![pointer(at, PointerButton::Primary, true)]);
        let outside = at + vec2(0.0, 180.0);
        f.frame(vec![Event::PointerMoved(outside), pointer(outside, PointerButton::Primary, false)]);
        f.pristine();
        f.disabled = true;
        f.settle();
        f.click(id, PointerButton::Primary);
        f.pristine();
    }
}

#[test]
fn named_person_long_primary_release_keeps_sdk_activation_through_tooltip() {
    for id in ["person:rename", "person:photos"] {
        let mut f = Frames::new();
        f.settle();
        let at = f.at(id);
        f.frame(vec![Event::PointerMoved(at)]);
        f.frame(vec![pointer(at, PointerButton::Primary, true)]);
        f.time += 2.0;
        f.frame(vec![]);
        f.frame(vec![pointer(at, PointerButton::Primary, false)]);
        if id == "person:rename" {
            assert!(matches!(f.app.ui.dialog, Some(Dialog::RenamePerson { .. })));
        } else {
            assert_eq!(f.app.ui.view, ViewMode::PhotoGrid);
            assert_eq!(f.app.session.filter.person.as_deref(), Some(CANONICAL));
        }
    }
}

#[test]
fn named_person_registration_is_focusable_and_accessible_with_keyboard_activation() {
    for id in ["person:rename", "person:photos"] {
        for key in [Key::Enter, Key::Space] {
            let mut f = Frames::new();
            f.settle();
            let hit = f.hits.iter().find(|(name, _)| name == id).unwrap().1;
            let label = if id == "person:rename" { "Rename Person…" } else { "Show photos" };
            assert!(f.nodes.iter().any(|node| node.label() == Some(label) && node.role() == egui::accesskit::Role::Button));
            f.ctx.memory_mut(|m| m.request_focus(hit.id));
            f.frame(vec![]);
            assert!(f.ctx.memory(|m| m.has_focus(hit.id)), "registration must target the real button, not a hover proxy");
            for pressed in [true, false] {
                f.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }]);
            }
            if id == "person:rename" {
                assert!(matches!(f.app.ui.dialog, Some(Dialog::RenamePerson { .. })));
            } else {
                assert_eq!(f.app.ui.view, ViewMode::PhotoGrid);
            }
        }
    }
}

#[test]
fn named_person_sizing_and_measurement_publish_no_action_targets_or_side_effects() {
    for (sizing, measuring) in [(true, false), (false, true), (true, true)] {
        let mut f = Frames::new();
        f.settle();
        let hit = f.hits.iter().find(|(name, _)| name == "person:rename").unwrap().1;
        f.ctx.memory_mut(|m| m.request_focus(hit.id));
        f.frame(vec![]);
        f.sizing = sizing;
        f.measuring = measuring;
        f.frame(vec![Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        f.pristine();
        f.frame(vec![Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        f.pristine();
        for id in ["person:rename", "person:photos"] {
            assert!(!f.rects.iter().any(|(name, _)| name == id));
            assert!(!f.hits.iter().any(|(name, _)| name == id));
        }
        f.sizing = false;
        f.measuring = false;
        f.settle();
        for id in ["person:rename", "person:photos"] {
            assert_eq!(f.rects.iter().filter(|(name, _)| name == id).count(), 1);
            assert_eq!(f.hits.iter().filter(|(name, _)| name == id).count(), 1);
        }
    }
}

#[test]
fn named_person_empty_engine_name_falls_back_to_requested_identity() {
    for id in ["person:rename", "person:photos"] {
        let mut f = Frames::new();
        f.app.caches.person_page.as_mut().unwrap().4 = Arc::new(PersonPage::default());
        f.settle();
        f.click(id, PointerButton::Primary);
        if id == "person:rename" {
            assert!(matches!(&f.app.ui.dialog, Some(Dialog::RenamePerson { from, to, .. }) if from == REQUESTED && to == REQUESTED));
        } else {
            assert_eq!(f.app.session.filter.person.as_deref(), Some(REQUESTED));
        }
    }
}
