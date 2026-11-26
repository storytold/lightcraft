//! The production Photos action row, with its synchronous dispatch replaced by a recorder.
//! No Session, catalog, model, worker, pump, inference, GPU, download or filesystem fixture.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};

const QUEUE: &str = "denoise:queueAll";
const STOP: &str = "denoise:stop";

#[derive(Clone, Copy, Default)]
struct Pass {
    sizing: bool,
    measuring: bool,
}

struct Frames {
    ctx: egui::Context,
    time: f64,
    running: Option<(u64, u64)>,
    queued: u64,
    disabled: bool,
    pass: Pass,
    size: egui::Vec2,
    scale: f32,
    commands: Vec<(String, Value)>,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
    nodes: Vec<egui::accesskit::Node>,
}

impl Frames {
    fn new(running: Option<(u64, u64)>, queued: u64) -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        craft::ui::install(
            &ctx,
            craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows)),
        )
        .unwrap();
        Self {
            ctx,
            time: 0.0,
            running,
            queued,
            disabled: false,
            pass: Pass::default(),
            size: vec2(1000.0, 300.0),
            scale: 1.0,
            commands: vec![],
            rects: vec![],
            hits: vec![],
            elements: vec![],
            nodes: vec![],
        }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(self.size, self.scale, self.time, events);
        let ctx = self.ctx.clone();
        self.elements.clear();
        let mut out = ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                ui.add_enabled_ui(!self.disabled, |ui| {
                    let mut draw_row = |ui: &mut egui::Ui| {
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("denoise-queue-test"), |ui| {
                            photos_queue_actions(ui, self.running, self.queued, |command, params| {
                                self.commands.push((command.into(), params));
                            });
                        });
                        if let Some(record) = record {
                            self.elements = record.elements.clone();
                        }
                    };
                    if self.pass.sizing {
                        let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
                        draw_row(&mut child);
                    } else {
                        draw_row(ui);
                    }
                });
            };
            if self.pass.measuring {
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

    fn target(&self, id: &str) -> (Rect, WidgetHit) {
        assert_eq!(self.rects.iter().filter(|(name, _)| name == id).count(), 1);
        assert_eq!(self.hits.iter().filter(|(name, _)| name == id).count(), 1);
        let rect = self.rects.iter().find(|(name, _)| name == id).unwrap().1;
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        let element =
            self.elements.iter().find(|element| element.id == hit.id && element.part.is_none()).expect("real SDK response owner, not hover probe");
        assert_eq!(element.component, "Button");
        assert_eq!(element.rect, rect);
        assert!(rect.is_finite() && rect.is_positive());
        assert_eq!(hit.interact_rect, rect.intersect(hit.interact_rect));
        (rect, hit)
    }

    fn click(&mut self, id: &str, button: PointerButton, held: f64) {
        let at = self.target(id).0.center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        assert!(self.commands.is_empty(), "activation must wait for release");
        self.time += held;
        self.frame(vec![]);
        self.frame(vec![pointer(at, button, false)]);
        self.frame(vec![]);
    }

    fn focus(&mut self, id: &str) {
        let hit = self.target(id).1;
        self.ctx.memory_mut(|m| m.request_focus(hit.id));
        self.frame(vec![]);
        assert!(self.ctx.memory(|m| m.has_focus(hit.id)));
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.frame(vec![]);
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn expected(id: &str) -> (String, Value) {
    if id == QUEUE { ("denoise.queue".into(), json!({"scope": "withAmount"})) } else { ("denoise.cancel".into(), json!({})) }
}

#[test]
fn photos_queue_stop_visibility_is_running_or_queued_and_queue_is_always_present() {
    for (running, queued) in [(None, 0), (None, 3), (Some((0, 0)), 0), (Some((2, 8)), 5)] {
        let mut f = Frames::new(running, queued);
        f.settle();
        f.target(QUEUE);
        let busy = running.is_some() || queued > 0;
        assert_eq!(f.rects.iter().any(|(id, _)| id == STOP), busy);
        assert_eq!(f.hits.iter().any(|(id, _)| id == STOP), busy);
        if busy {
            f.target(STOP);
        }
        assert!(f.commands.is_empty());
    }
}

#[test]
fn photos_queue_and_stop_primary_release_preserve_exact_command_payload_once() {
    for id in [QUEUE, STOP] {
        for held in [0.0, 2.0] {
            let mut f = Frames::new(Some((2, 8)), 3);
            f.settle();
            f.click(id, PointerButton::Primary, held);
            assert_eq!(f.commands, vec![expected(id)]);
        }
    }
}

#[test]
fn photos_queue_idle_and_stop_queued_only_dispatch_without_running_work() {
    for (id, queued) in [(QUEUE, 0), (STOP, 4)] {
        let mut f = Frames::new(None, queued);
        f.settle();
        f.click(id, PointerButton::Primary, 0.0);
        assert_eq!(f.commands, vec![expected(id)]);
    }
}

#[test]
fn photos_queue_and_stop_secondary_and_outside_release_are_noops() {
    for id in [QUEUE, STOP] {
        let mut f = Frames::new(None, 2);
        f.settle();
        f.click(id, PointerButton::Secondary, 0.0);
        assert!(f.commands.is_empty());
        let at = f.target(id).0.center();
        f.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, true)]);
        let outside = at + vec2(0.0, 180.0);
        f.frame(vec![Event::PointerMoved(outside), pointer(outside, PointerButton::Primary, false)]);
        assert!(f.commands.is_empty());
    }
}

#[test]
fn photos_queue_and_stop_real_owner_focus_keyboard_and_accessible_names() {
    for id in [QUEUE, STOP] {
        for key in [Key::Enter, Key::Space] {
            let mut f = Frames::new(Some((0, 8)), 0);
            f.settle();
            let label = if id == QUEUE { tr("Denoise all photos with an Amount") } else { tr("Stop") };
            assert!(f.nodes.iter().any(|node| node.label() == Some(label) && node.role() == egui::accesskit::Role::Button));
            f.focus(id);
            f.key(key);
            assert_eq!(f.commands, vec![expected(id)]);
        }
    }
}

#[test]
fn photos_queue_and_stop_inherit_disabled_parent_for_pointer_and_keyboard() {
    for id in [QUEUE, STOP] {
        let mut f = Frames::new(None, 2);
        f.settle();
        f.focus(id);
        f.disabled = true;
        f.settle();
        f.target(id); // Still registered under the actual disabled response owner.
        f.click(id, PointerButton::Primary, 0.0);
        f.key(Key::Enter);
        f.key(Key::Space);
        assert!(f.commands.is_empty());
        f.disabled = false;
        f.settle();
        f.click(id, PointerButton::Primary, 0.0);
        assert_eq!(f.commands, vec![expected(id)]);
    }
}

#[test]
fn photos_queue_and_stop_each_sizing_guard_suppresses_targets_and_commands() {
    for pass in [Pass { sizing: true, measuring: false }, Pass { sizing: false, measuring: true }, Pass { sizing: true, measuring: true }] {
        for id in [QUEUE, STOP] {
            let mut f = Frames::new(None, 2);
            f.settle();
            f.focus(id);
            f.pass = pass;
            f.key(Key::Enter);
            assert!(f.commands.is_empty());
            assert!(f.rects.is_empty());
            assert!(f.hits.is_empty());
            f.pass = Pass::default();
            f.settle();
            f.target(QUEUE);
            f.target(STOP);
            assert!(f.commands.is_empty(), "measurement must not defer an action");
        }
    }
}

#[test]
fn photos_queue_owner_survives_stop_visibility_transitions_and_scaled_geometry() {
    for (width, scale) in [(620.0, 1.0), (1000.0, 2.0)] {
        let mut f = Frames::new(None, 0);
        f.size.x = width;
        f.scale = scale;
        f.settle();
        let queue_id = f.target(QUEUE).1.id;
        f.queued = 2;
        f.settle();
        assert_eq!(f.target(QUEUE).1.id, queue_id);
        f.target(STOP);
        f.queued = 0;
        f.running = Some((0, 0));
        f.settle();
        f.target(STOP);
        f.running = None;
        f.settle();
        assert!(!f.rects.iter().any(|(id, _)| id == STOP));
        assert!(!f.hits.iter().any(|(id, _)| id == STOP));
        assert_eq!(f.target(QUEUE).1.id, queue_id);
        assert!(f.commands.is_empty());
    }
}

#[test]
fn photos_queue_and_stop_measurement_cannot_complete_an_inflight_pointer_press() {
    for pass in [Pass { sizing: true, measuring: false }, Pass { sizing: false, measuring: true }, Pass { sizing: true, measuring: true }] {
        for id in [QUEUE, STOP] {
            let mut f = Frames::new(Some((1, 4)), 0);
            f.settle();
            let at = f.target(id).0.center();
            f.frame(vec![Event::PointerMoved(at)]);
            f.frame(vec![pointer(at, PointerButton::Primary, true)]);
            assert!(f.commands.is_empty());
            f.pass = pass;
            f.frame(vec![pointer(at, PointerButton::Primary, false)]);
            assert!(f.commands.is_empty());
            assert!(f.rects.is_empty());
            assert!(f.hits.is_empty());
            f.pass = Pass::default();
            f.settle();
            assert!(f.commands.is_empty());
        }
    }
}
