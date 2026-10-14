//! Real egui pointer and keyboard frames on the production slider readout.
use super::{SliderOut, slider, take_registry};
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect, vec2};
use lightcraft_develop::{ControlSpec, controls};

struct Harness {
    view: HeadlessView,
    spec: ControlSpec,
    value: f64,
    enabled: bool,
    time: f64,
    rect: Rect,
}
impl Harness {
    fn new() -> Self {
        let mut h = Self {
            view: HeadlessView::new(),
            spec: *controls::find("light.exposure").unwrap(),
            value: 0.71,
            enabled: true,
            time: 0.0,
            rect: Rect::NOTHING,
        };
        for _ in 0..3 {
            h.frame(vec![]);
        }
        h
    }
    fn frame(&mut self, events: Vec<Event>) -> SliderOut {
        self.time += 0.02;
        let mut out = SliderOut::default();
        self.view.run(HeadlessView::raw_input(vec2(330.0, 100.0), 1.0, self.time, events), |ui| {
            out = slider(ui, &self.spec, self.value, self.enabled, None);
        });
        self.rect = take_registry(&self.view.ctx).into_iter().find(|(id, _)| id == &format!("sliderValue:{}", self.spec.id)).unwrap().1;
        if let Some(v) = out.value {
            self.value = v;
        }
        out
    }
    fn key(&mut self, key: Key) -> SliderOut {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }])
    }
}

#[test]
fn opening_release_honors_return_and_escape_immediately() {
    for key in [Key::Enter, Key::Escape] {
        let mut h = Harness::new();
        let pos = h.rect.center();
        h.frame(vec![Event::PointerMoved(pos)]);
        h.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        let out = h.frame(vec![
            Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
            Event::Text("2".into()),
            Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
        ]);
        assert_eq!(out.value, (key == Key::Enter).then_some(2.0));
        assert!(!h.view.ctx.egui_wants_keyboard_input());
        assert!(h.key(Key::Enter).value.is_none(), "no retired draft can commit later");
    }
}

#[test]
fn dragging_away_from_readout_does_not_capture_keyboard() {
    let mut h = Harness::new();
    let inside = h.rect.center();
    let outside = egui::pos2(5.0, 80.0);
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    for events in [vec![Event::PointerMoved(inside)], vec![button(inside, true)], vec![Event::PointerMoved(outside)], vec![button(outside, false)]] {
        let out = h.frame(events);
        assert_eq!(out.value, None, "a cancelled readout gesture must not emit a value");
        assert_eq!(h.value, 0.71, "a cancelled readout gesture must preserve the stored value");
    }
    assert!(!h.view.ctx.egui_wants_keyboard_input(), "a cancelled readout gesture must not retain keyboard focus");
}

#[test]
fn typing_after_a_released_click_still_edits() {
    // Issue #1362: a normal click (press, a few frames held, release) must leave the field
    // editing, so the value can be typed without holding the button down.
    let mut h = Harness::new();
    let pos = h.rect.center();
    let button = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(pos)]);
    h.frame(vec![button(true)]);
    for _ in 0..4 {
        h.frame(vec![]);
    }
    assert_eq!(h.frame(vec![button(false)]).value, None);
    h.frame(vec![]);
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the field keeps the keyboard after the click");
    h.frame(vec![Event::Text("2".into())]);
    assert_eq!(h.key(Key::Enter).value, Some(2.0));
}
