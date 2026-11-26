//! Grading eye regressions: in-memory photos only; no renderer, services or captures.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::WidgetHit;
use egui::{Event, Key, Modifiers, PointerButton};
use std::collections::HashMap;

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    photos: [PhotoId; 2],
    photo: PhotoId,
    time: f64,
    enabled: bool,
    last_eye_down: Option<egui::Id>,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    shapes: Vec<egui::epaint::ClippedShape>,
    nodes: HashMap<egui::Id, (Option<String>, Option<bool>, Option<String>)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
}

impl Frames {
    fn new(mode: &str) -> Self {
        use lightcraft_catalog::{Op, Photo, Source};
        let mut session = lightcraft_engine::Session::new();
        let mut ids = Vec::new();
        for scene in 0..2 {
            let id = session.catalog.alloc_photo_id();
            let mut photo = Photo::new(id, Source::Demo { scene }, "eye-memory.jpg", "JPEG", 8, 8, "2026-01-01T00:00:00");
            let mut d = DevelopSettings::default();
            d.grading.shadows.hue = 220.0;
            d.grading.shadows.sat = 60.0;
            d.grading.midtones.sat = 50.0;
            d.grading.highlights.sat = 40.0;
            photo.develop = std::sync::Arc::new(d);
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            ids.push(id);
        }
        session.execute("library.select", &json!({"ids": [ids[0].0], "active": ids[0].0})).unwrap();
        let mut app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
        app.ui.grading_mode = mode.into();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.install_framework(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let mut frames = Self {
            ctx,
            app,
            photos: [ids[0], ids[1]],
            photo: ids[0],
            time: 0.0,
            enabled: true,
            last_eye_down: None,
            rects: Vec::new(),
            hits: Vec::new(),
            shapes: Vec::new(),
            nodes: HashMap::new(),
            elements: Vec::new(),
        };
        frames.settle();
        frames
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(400.0, 1000.0), 1.0, self.time, events);
        let ctx = self.ctx.clone();
        let mut output = ctx.run_ui(raw, |ui| {
            crate::widgets::take_registries(&ctx);
            ui.add_enabled_ui(self.enabled, |ui| {
                let d = self.app.session.catalog.photo(self.photo).unwrap().develop.clone();
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("grading-eye-port"), |ui| {
                    grading(&mut self.app, ui, &d, self.photo);
                });
                (self.rects, self.hits) = crate::widgets::take_registries(&ctx);
                self.nodes.clear();
                self.last_eye_down = None;
                for (_, hit) in self.hits.iter().filter(|(k, _)| k.starts_with("wheelEye:")) {
                    if ctx.read_response(hit.id).is_some_and(|r| r.is_pointer_button_down_on()) {
                        self.last_eye_down = Some(hit.id);
                    }
                    // a button-role toggle reports its state as pressed (toggled), not selected
                    if let Some(node) = ctx.accesskit_node_builder(hit.id, |n| {
                        (
                            n.label().map(str::to_owned),
                            n.toggled().and_then(|t| (t == egui::accesskit::Toggled::True).then_some(true)),
                            n.description().map(str::to_owned),
                        )
                    }) {
                        self.nodes.insert(hit.id, node);
                    }
                }
                self.elements = record.iter().flat_map(|record| record.elements.iter().cloned()).collect();
            });
        });
        output.textures_delta.clear();
        self.shapes = output.shapes;
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame(Vec::new());
        }
    }

    fn rect(&self, key: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == &format!("wheelEye:{key}")).unwrap().1
    }

    fn hit(&self, key: &str) -> WidgetHit {
        self.hits.iter().find(|(name, _)| name == &format!("wheelEye:{key}")).unwrap().1
    }

    fn pointer(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }]);
    }

    fn click(&mut self, key: &str) {
        let pos = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.pointer(pos, PointerButton::Primary, true);
        self.pointer(pos, PointerButton::Primary, false);
        self.settle();
    }

    fn key(&mut self, key: Key, pressed: bool) {
        self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
    }

    fn tap(&mut self, key: Key) {
        self.key(key, true);
        self.key(key, false);
        self.settle();
    }

    fn tab_to(&mut self, key: &str) {
        self.frame(vec![Event::PointerGone]);
        let id = self.hit(key).id;
        for _ in 0..80 {
            if self.ctx.memory(|memory| memory.has_focus(id)) {
                return;
            }
            self.tap(Key::Tab);
        }
        panic!("eye {key} was not naturally Tab reachable");
    }

    fn eye_paths(&self, key: &str) -> usize {
        let rect = self.rect(key).expand(1.0);
        self.shapes
            .iter()
            .filter(|shape| matches!(&shape.shape, egui::Shape::Path(_)) && rect.contains_rect(shape.shape.visual_bounding_rect()))
            .count()
    }

    fn durable(&self) -> serde_json::Value {
        json!({"photos": self.photos.map(|id| serde_json::to_value(self.app.session.catalog.photo(id).unwrap()).unwrap()),
            "revision": self.app.session.catalog.revision, "undo": self.app.session.undo.len(), "interaction": self.app.session.interaction.is_some()})
    }

    fn preview(&mut self) -> lightcraft_engine::RenderJob {
        let hidden = self.app.ui.grading_hidden_for(self.photo);
        self.app.session.loupe_job(self.photo, 8, 8, true).unwrap().with_hidden_grading(hidden)
    }
}

#[test]
fn grading_eye_public_owner_geometry_and_focus_only_face_in_both_layouts() {
    for mode in ["3way", "shadows"] {
        let keys: &[&str] = if mode == "3way" { &["shadows", "midtones", "highlights"] } else { &["shadows"] };
        for key in keys {
            // The next Tab stop may itself be an eye. Observe each wheel's initial face in a fresh layout.
            let mut h = Frames::new(mode);
            assert_eq!(h.rects.iter().filter(|(name, _)| name.starts_with("wheelEye:")).count(), keys.len());
            let rect = h.rect(key);
            assert_eq!(rect.size(), vec2(16.0, 16.0));
            let hit = h.hit(key);
            assert_eq!(hit.interact_rect, rect, "the actual SDK target must remain exactly 16 by 16");
            let element = h.elements.iter().find(|element| element.id == hit.id && element.component == "IconButton").unwrap();
            assert_eq!(element.facts.as_ref().unwrap().role, "Button");
            assert!(element.stop.is_some(), "rest-hidden eye must still have a Tab stop");
            assert!(!h.ctx.memory(|m| m.has_focus(hit.id)), "{mode}/{key}: initial owner is focused");
            assert!(h.ctx.input(|i| i.pointer.hover_pos().is_none()));
            assert_eq!(h.eye_paths(key), 0, "{mode}/{key}: unfocused, unhovered eye must be invisible");
            h.tab_to(key);
            assert!(h.ctx.input(|input| input.pointer.hover_pos().is_none()));
            assert!(h.eye_paths(key) >= 2, "keyboard focus alone reveals the eye glyph");
            assert!(
                h.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Rect(outline) if outline.stroke.width > 0.0 && rect.expand(4.0).contains_rect(outline.rect)
                )),
                "keyboard focus must have a visible outline, not merely a hidden Tab stop"
            );
            h.tap(Key::Tab);
            assert!(!h.ctx.memory(|m| m.has_focus(hit.id)), "{mode}/{key}: Tab did not leave owner");
            assert_eq!(h.eye_paths(key), 0, "{mode}/{key}: ordinary blur must hide eye again");
        }
        let h = Frames::new(mode);
        let node = &h.nodes[&h.hit("shadows").id];
        assert_eq!(node.0.as_deref(), Some(crate::i18n::tr("Show / hide")));
        assert_ne!(node.1, Some(true));
        assert_eq!(node.2.as_deref(), Some(format!("{}: {}", crate::i18n::tr("Shadows"), crate::i18n::tr("Hide this wheel's effect")).as_str()));
    }
}

#[test]
fn grading_eye_primary_release_changes_actual_preview_only_and_never_resets_wheel() {
    for mode in ["3way", "shadows"] {
        let mut h = Frames::new(mode);
        let before = h.durable();
        let shown = h.preview();
        let pos = h.rect("shadows").center();
        h.pointer(pos, PointerButton::Primary, true);
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3], "not on press");
        assert_eq!(h.durable(), before);
        h.pointer(pos, PointerButton::Primary, false);
        h.settle();
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [true, false, false]);
        let hidden = h.preview();
        assert_eq!(hidden.settings.grading.shadows.sat, 0.0);
        assert_eq!(hidden.settings.grading.midtones.sat, 50.0);
        assert_eq!(hidden.settings.grading.highlights.sat, 40.0);
        assert_ne!(hidden.key, shown.key);
        assert!(hidden.cache.is_none() && hidden.view_cache.is_none());
        assert_eq!(h.durable(), before);
        let node = &h.nodes[&h.hit("shadows").id];
        assert_eq!(node.1, Some(true));
        assert_eq!(node.2.as_deref(), Some(format!("{}: {}", crate::i18n::tr("Shadows"), crate::i18n::tr("Show this wheel's effect")).as_str()));
        h.click("shadows");
        assert_eq!(h.preview().settings.grading.shadows.sat, 60.0);
        // Another rapid click must toggle preview, not the native double-click reset.
        h.click("shadows");
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [true, false, false]);
        assert_eq!(h.durable(), before);
    }
}

#[test]
fn grading_eye_enter_and_space_reach_the_same_preview_action_without_hover() {
    for mode in ["3way", "shadows"] {
        let mut h = Frames::new(mode);
        let before = h.durable();
        h.tab_to("shadows");
        h.tap(Key::Enter);
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [true, false, false]);
        h.tap(Key::Space);
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
        h.tap(Key::Escape);
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
        assert_eq!(h.durable(), before);
    }
}

#[test]
fn grading_eye_wheels_and_photos_have_separate_temporary_preview_flags() {
    let mut h = Frames::new("3way");
    let before = h.durable();
    h.click("shadows");
    h.click("highlights");
    assert_eq!(h.app.ui.grading_hidden_for(h.photos[0]), [true, false, true]);
    assert_eq!(h.app.ui.grading_hidden_for(h.photos[1]), [false; 3]);
    h.photo = h.photos[1];
    h.settle();
    assert_eq!(h.preview().settings.grading.shadows.sat, 60.0);
    h.click("midtones");
    assert_eq!(h.app.ui.grading_hidden_for(h.photos[1]), [false, true, false]);
    assert_eq!(h.app.ui.grading_hidden_for(h.photos[0]), [false; 3], "existing UI only retains flags for the current photo");
    assert_eq!(h.durable(), before);
}

#[test]
fn grading_eye_cancelled_pointer_and_keyboard_gestures_never_edit_or_retarget() {
    for cancel in ["outside", "secondary", "escape", "blur", "photo", "key-escape"] {
        let mut h = Frames::new("3way");
        let before = h.durable();
        let pos = h.rect("shadows").center();
        if cancel == "key-escape" {
            h.tab_to("shadows");
            // Escape and a stray activation-key release are not activations.
            // Public SDK Enter/Space activation happens on key press, not release.
            h.tap(Key::Escape);
            h.key(Key::Space, false);
        } else if cancel == "secondary" {
            h.pointer(pos, PointerButton::Secondary, true);
            h.pointer(pos, PointerButton::Secondary, false);
        } else {
            h.pointer(pos, PointerButton::Primary, true);
            match cancel {
                "outside" => h.pointer(pos + vec2(30.0, -30.0), PointerButton::Primary, false),
                "escape" => {
                    h.tap(Key::Escape);
                    h.pointer(pos, PointerButton::Primary, false);
                }
                "blur" => {
                    h.frame(vec![Event::WindowFocused(false)]);
                    h.pointer(pos, PointerButton::Primary, false);
                }
                "photo" => {
                    h.photo = h.photos[1];
                    h.settle();
                    h.pointer(pos, PointerButton::Primary, false);
                }
                _ => unreachable!(),
            }
        }
        h.settle();
        assert_eq!(h.app.ui.grading_hidden_for(h.photos[0]), [false; 3], "{cancel}");
        assert_eq!(h.app.ui.grading_hidden_for(h.photos[1]), [false; 3], "{cancel}");
        assert_eq!(h.durable(), before, "{cancel}: native wheel drag/reset must never run");
    }
}

#[test]
fn grading_eye_native_sizing_and_sdk_measurement_publish_no_eye_or_preview_edits() {
    for native in [true, false] {
        let mut h = Frames::new("3way");
        let before = h.durable();
        let journal = h.app.session.journal.clone();
        let ctx = h.ctx.clone();
        let eye = h.hit("shadows");
        let pos = eye.interact_rect.center();
        h.frame(vec![Event::PointerMoved(pos)]);
        h.pointer(pos, PointerButton::Primary, true);
        assert_eq!(h.last_eye_down, Some(eye.id));
        h.time += 0.02;
        let raw = HeadlessView::raw_input(
            vec2(400.0, 1000.0),
            1.0,
            h.time,
            vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
        );
        let mut callbacks = 0;
        let mut out = ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                callbacks += 1;
                assert_eq!(ui.is_sizing_pass(), native);
                assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), !native);
                crate::widgets::take_registries(&ctx);
                let d = h.app.session.catalog.photo(h.photo).unwrap().develop.clone();
                grading(&mut h.app, ui, &d, h.photo);
                let (rects, hits) = crate::widgets::take_registries(&ctx);
                assert!(!rects.iter().any(|(n, _)| n.starts_with("wheelEye:")) && !hits.iter().any(|(n, _)| n.starts_with("wheelEye:")));
                assert_eq!(h.durable(), before);
                assert_eq!(h.app.session.journal, journal);
                assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
            };
            if native {
                let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                draw(&mut child);
            } else {
                craft::ui::runtime::measure::run(&ctx, || draw(ui));
            }
        });
        out.textures_delta.clear();
        assert!(callbacks > 0);
        h.settle();
        assert_eq!(h.hit("shadows").id, eye.id);
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
        assert_eq!(h.app.session.journal, journal);
        h.click("shadows");
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), [true, false, false]);
        assert_eq!(h.durable(), before);
        assert_eq!(h.app.session.journal, journal);
    }
}

#[test]
fn grading_eye_inherited_disabled_scope_refuses_pointer_and_keyboard() {
    let mut h = Frames::new("3way");
    h.tab_to("shadows");
    h.enabled = false;
    h.settle();
    let before = h.durable();
    h.click("shadows");
    h.tap(Key::Enter);
    h.tap(Key::Space);
    assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
    assert_eq!(h.durable(), before);
    let id = h.hit("shadows").id;
    assert!(h.elements.iter().find(|element| element.id == id).unwrap().facts.as_ref().unwrap().disabled);
}

#[test]
fn grading_preview_hide_two_wheels_switch_layout_restore_one_then_return_without_edits() {
    let mut h = Frames::new("3way");
    let before = h.durable();
    let journal = h.app.session.journal.clone();
    let initial = h.preview();
    h.click("shadows");
    h.click("highlights");
    let mode = |h: &mut Frames, key: &str| {
        let name = format!("icon:grading-{key}");
        let owners = h.hits.iter().filter(|(k, _)| k == &name).collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        let hit = owners[0].1;
        assert!(h.elements.iter().any(|e| e.id == hit.id && e.part.is_none()));
        let at = hit.interact_rect.center();
        h.frame(vec![Event::PointerMoved(at)]);
        h.pointer(at, PointerButton::Primary, true);
        h.pointer(at, PointerButton::Primary, false);
        h.settle();
        assert_eq!(h.app.ui.grading_mode, key);
    };
    mode(&mut h, "shadows");
    assert_eq!(h.hits.iter().filter(|(k, _)| k.starts_with("wheelEye:")).count(), 1);
    let muted = h.preview();
    assert_eq!(muted.settings.grading.shadows.sat, 0.0);
    assert_eq!(muted.settings.grading.highlights.sat, 0.0);
    assert_eq!(muted.settings.grading.midtones.sat, 50.0);
    assert_ne!(initial.key, muted.key);
    assert!(muted.cache.is_none() && muted.view_cache.is_none());
    h.tab_to("shadows");
    h.tap(Key::Space);
    mode(&mut h, "3way");
    assert_eq!(h.hits.iter().filter(|(k, _)| k.starts_with("wheelEye:")).count(), 3);
    assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false, false, true]);
    let partial = h.preview();
    assert_eq!(partial.settings.grading.shadows.sat, 60.0);
    assert_eq!(partial.settings.grading.highlights.sat, 0.0);
    h.click("highlights");
    assert_eq!(h.preview().key, initial.key);
    assert_eq!(h.durable(), before);
    assert_eq!(h.app.session.journal, journal);
    assert_eq!(h.app.renderer.in_flight(), 0);
    assert_eq!(h.app.renderer.live_workers(), 0);
}

#[test]
fn grading_all_preview_masks_match_complete_loupe_request_and_each_eye_description() {
    let mut h = Frames::new("3way");
    let before = h.durable();
    let journal = h.app.session.journal.clone();
    let base = h.preview();
    let baseline = (*base.settings).clone();
    let mut prev = 0usize;
    // Gray code: exactly one real eye click takes us to each known mask.
    for mask in [0usize, 1, 3, 2, 6, 7, 5, 4, 0] {
        if prev != mask {
            h.click(GRADING_WHEELS[(prev ^ mask).trailing_zeros() as usize].0);
        }
        prev = mask;
        let hidden = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
        assert_eq!(h.app.ui.grading_hidden_for(h.photo), hidden);
        let mut expected = baseline.clone();
        if hidden[0] {
            expected.grading.shadows = Default::default();
        }
        if hidden[1] {
            expected.grading.midtones = Default::default();
        }
        if hidden[2] {
            expected.grading.highlights = Default::default();
        }
        let expected_key = base.key ^ expected.hash64() ^ baseline.hash64() ^ (mask as u64).wrapping_mul(0xd6e8_feb8_6659_fd93);
        let job = h.preview();
        assert_eq!(*job.settings, expected);
        assert_eq!(job.key, expected_key);
        if mask != 0 {
            assert!(job.cache.is_none() && job.view_cache.is_none() && job.preview_cut.is_none());
        }
        let mut owners = std::collections::HashSet::new();
        let mut descriptions = std::collections::HashSet::new();
        for (i, (key, label)) in GRADING_WHEELS.into_iter().enumerate() {
            let id = h.hit(key).id;
            assert!(owners.insert(id));
            let node = &h.nodes[&id];
            let description = |hidden| {
                format!(
                    "{}: {}",
                    crate::i18n::tr(label),
                    crate::i18n::tr(if hidden { "Show this wheel's effect" } else { "Hide this wheel's effect" })
                )
            };
            assert_eq!(node.0.as_deref(), Some(crate::i18n::tr("Show / hide")));
            assert_eq!(node.1, hidden[i].then_some(true));
            let actual = node.2.as_ref().expect("each mounted eye has a description");
            assert_eq!(actual, &description(hidden[i]));
            assert_ne!(actual, &description(!hidden[i]));
            assert!(descriptions.insert(actual));
        }
        assert_eq!(h.durable(), before);
        assert_eq!(h.app.session.journal, journal);
        assert_eq!(h.app.renderer.in_flight(), 0);
        assert_eq!(h.app.renderer.live_workers(), 0);
    }
}
#[test]
fn grading_cancelled_eye_releases_overlap_for_ordinary_wheel_drag_end_and_double_click_reset() {
    let mut h = Frames::new("3way");
    let before = h.durable();
    let eye = h.rect("shadows").center();
    let center = h.rects.iter().find(|(n, _)| n == "wheel:shadows").unwrap().1.center();
    h.frame(vec![Event::PointerMoved(eye)]);
    h.pointer(eye, PointerButton::Primary, true);
    assert_eq!(h.last_eye_down, Some(h.hit("shadows").id));
    h.frame(vec![Event::PointerMoved(center)]);
    h.pointer(center, PointerButton::Primary, false);
    h.settle();
    assert_eq!(h.durable(), before);
    assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
    let undo = h.app.session.undo.len();
    let at = center + vec2(18.0, -18.0);
    h.pointer(center, PointerButton::Primary, true);
    h.frame(vec![Event::PointerMoved(at)]);
    assert!(h.app.session.interaction.is_some());
    h.pointer(at, PointerButton::Primary, false);
    h.settle();
    assert!(h.app.session.interaction.is_none());
    assert_eq!(h.app.session.undo.len(), undo + 1);
    let wheel = h.preview().settings.grading.shadows;
    assert_ne!((wheel.hue, wheel.sat), (220.0, 60.0));
    assert!(wheel.sat > 0.0);
    // A separated double click away from both the eye and the neutral wheel center must reset the real wheel.
    h.time += 1.0;
    for _ in 0..2 {
        h.pointer(at, PointerButton::Primary, true);
        h.pointer(at, PointerButton::Primary, false);
    }
    h.settle();
    let wheel = h.preview().settings.grading.shadows;
    assert_eq!((wheel.hue, wheel.sat), (0.0, 0.0));
    assert!(h.app.session.interaction.is_none());
    assert_eq!(h.app.ui.grading_hidden_for(h.photo), [false; 3]);
}
