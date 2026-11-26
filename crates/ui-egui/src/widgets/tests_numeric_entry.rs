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
    track: Rect,
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
            track: Rect::NOTHING,
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
        let registry = take_registry(&self.view.ctx);
        let find = |name: String| registry.iter().find(|(id, _)| id == &name).map(|(_, r)| *r);
        self.rect = find(format!("sliderValue:{}", self.spec.id)).unwrap();
        self.track = find(format!("slider:{}", self.spec.id)).unwrap();
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
    // straight down: no sideways travel, so the press neither types nor scrubs
    let mut h = Harness::new();
    let inside = h.rect.center();
    let outside = egui::pos2(inside.x, 80.0);
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

impl Harness {
    /// The track's width: a scrub moves the value as far as dragging the thumb would.
    fn track_width(&self) -> f32 {
        self.track.width()
    }
    /// Press on the value, move sideways by `dx` in `steps` frames (Shift held when `shift`) and release;
    /// the outputs of every frame.
    fn scrub(&mut self, dx: f32, steps: usize, shift: bool) -> Vec<SliderOut> {
        let modifiers = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
        let start = self.rect.center();
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers };
        let mut outs = vec![self.frame(vec![Event::ModifiersChanged(modifiers), Event::PointerMoved(start)]), self.frame(vec![button(start, true)])];
        for i in 1..=steps {
            let pos = start + vec2(dx * i as f32 / steps as f32, 0.0);
            outs.push(self.frame(vec![Event::PointerMoved(pos)]));
        }
        outs.push(self.frame(vec![button(start + vec2(dx, 0.0), false)]));
        outs.push(self.frame(vec![Event::ModifiersChanged(Modifiers::NONE)]));
        outs
    }
}

#[test]
fn dragging_the_value_sideways_scrubs_it_like_the_thumb() {
    let mut h = Harness::new();
    let width = f64::from(h.track_width());
    assert!(width > 100.0, "the slider has a track ({width})");
    // a fifth of the track: a fifth of the range (Exposure −5..+5 → +2 EV)
    let outs = h.scrub((width / 5.0) as f32, 10, false);
    assert!((h.value - 2.71).abs() < 0.015, "scrubbed to {}", h.value);
    assert_eq!(outs.iter().filter(|o| o.drag_started).count(), 1, "one interaction begins");
    assert_eq!(outs.iter().filter(|o| o.drag_stopped).count(), 1, "and ends, so the scrub is one undo step");
    assert!(outs.iter().filter_map(|o| o.value).count() > 1, "the value follows the pointer while it moves");
    assert!(!h.view.ctx.egui_wants_keyboard_input(), "a scrub opens no value field");
    assert!(h.key(Key::Enter).value.is_none(), "and leaves no draft to commit");
}

#[test]
fn scrubbing_back_and_with_shift() {
    let mut h = Harness::new();
    let width = h.track_width();
    // left lowers the value; on the control's steps and inside its range
    h.scrub(-width, 8, false);
    assert_eq!(h.value, -5.0, "clamped at the minimum");
    // Shift moves a tenth as far
    let mut h = Harness::new();
    h.scrub(width / 5.0, 10, true);
    assert!((h.value - 0.91).abs() < 0.015, "fine scrub to {}", h.value);
    assert!(((h.value / 0.01).round() * 0.01 - h.value).abs() < 1e-9, "on the 0.01 steps");
}

#[test]
fn a_press_that_barely_moves_still_types() {
    // a hand's jitter under the scrub distance is a click: the value can be typed
    let mut h = Harness::new();
    let pos = h.rect.center();
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(pos)]);
    h.frame(vec![button(pos, true)]);
    h.frame(vec![Event::PointerMoved(pos + vec2(1.5, 0.5))]);
    assert_eq!(h.frame(vec![button(pos + vec2(1.5, 0.5), false)]).value, None);
    h.frame(vec![]);
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the field keeps the keyboard after the click");
    h.frame(vec![Event::Text("2".into())]);
    assert_eq!(h.key(Key::Enter).value, Some(2.0));
}

#[test]
fn a_disabled_value_does_not_scrub() {
    let mut h = Harness::new();
    h.enabled = false;
    let width = h.track_width();
    assert!(h.scrub(width / 5.0, 10, false).iter().all(|o| o.value.is_none() && !o.drag_started));
    assert_eq!(h.value, 0.71);
}

#[test]
fn a_long_click_keeps_the_field_open() {
    // A press held past egui's click time (here a second and a half) used to close the field on
    // release; it stays open for typing.
    let mut h = Harness::new();
    let pos = h.rect.center();
    let button = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(pos)]);
    h.frame(vec![button(true)]);
    for _ in 0..75 {
        h.frame(vec![]);
    }
    assert_eq!(h.frame(vec![button(false)]).value, None);
    h.frame(vec![]);
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the field keeps the keyboard after a long click");
    h.frame(vec![Event::Text("2".into())]);
    assert_eq!(h.key(Key::Enter).value, Some(2.0));
}

#[test]
fn leaving_a_slider_disarms_wheel_edits_until_it_is_selected_again() {
    let mut h = Harness::new();
    let inside = h.track.center();
    let outside = egui::pos2(5.0, 80.0);
    let button = |pressed| Event::PointerButton { pos: inside, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let wheel = || Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![Event::PointerMoved(inside)]);
    h.frame(vec![button(true)]);
    h.frame(vec![button(false)]);
    let selected = h.value;
    h.frame(vec![wheel()]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert!(h.value > selected, "selecting a slider enables wheel nudges");
    h.frame(vec![Event::PointerMoved(outside)]);
    let before_return = h.value;
    h.frame(vec![Event::PointerMoved(inside), wheel()]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert_eq!(h.value, before_return, "scrolling back over a previously selected slider must not edit it");
    h.frame(vec![Event::PointerMoved(outside)]);
    let before_arrow = h.value;
    h.key(Key::ArrowUp);
    assert!(h.value > before_arrow, "leaving must preserve keyboard arrows");
    // Sideways scrolling remains independent of vertical wheel selection (#1465).
    let before_sideways = h.value;
    h.frame(vec![
        Event::PointerMoved(inside),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(120.0, 0.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        },
    ]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    let after_sideways = h.value;
    assert!(after_sideways > before_sideways, "a disarmed slider still accepts sideways nudges");
    h.frame(vec![wheel()]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert_eq!(h.value, after_sideways, "sideways nudges do not re-arm vertical wheel edits");
    h.frame(vec![Event::PointerMoved(inside)]);
    h.frame(vec![button(true)]);
    h.frame(vec![button(false)]);
    let selected_again = h.value;
    h.frame(vec![wheel()]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert!(h.value > selected_again, "selecting it again restores wheel nudges");
}

#[test]
fn leaving_the_row_during_a_drag_keeps_dragging_but_release_disarms_the_wheel() {
    let mut h = Harness::new();
    let inside = h.track.center();
    let outside = egui::pos2(h.track.right() - 20.0, 80.0);
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(inside)]);
    h.frame(vec![button(inside, true)]);
    let initial = h.value;
    h.frame(vec![Event::PointerMoved(outside)]);
    assert!(h.value > initial, "an owned drag keeps editing outside the row");
    h.frame(vec![button(outside, false)]);
    let released = h.value;
    h.frame(vec![
        Event::PointerMoved(inside),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        },
    ]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert_eq!(h.value, released, "releasing outside disarms subsequent wheel edits");
}

#[test]
fn a_drag_that_leaves_and_ends_back_in_the_row_disarms_the_wheel() {
    let mut h = Harness::new();
    let inside = h.track.center();
    let outside = egui::pos2(h.track.right() - 20.0, 80.0);
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(inside)]);
    h.frame(vec![button(inside, true)]);
    h.frame(vec![Event::PointerMoved(outside)]);
    h.frame(vec![Event::PointerMoved(inside)]);
    h.frame(vec![button(inside, false)]);
    let released = h.value;
    h.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    }]);
    for _ in 0..40 {
        h.frame(vec![]);
    }
    assert_eq!(h.value, released, "the drag left the row, so the wheel needs a fresh selection");
}

#[test]
fn leaving_and_returning_within_one_frame_still_disarms_the_wheel() {
    let wheel = || Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    // a slow frame merges the moves out of the row and back (or out of the window and back)
    for away in [Event::PointerMoved(egui::pos2(5.0, 80.0)), Event::PointerGone] {
        let mut h = Harness::new();
        let inside = h.track.center();
        let button = |pressed| Event::PointerButton { pos: inside, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.frame(vec![Event::PointerMoved(inside)]);
        h.frame(vec![button(true)]);
        h.frame(vec![button(false)]);
        let selected = h.value;
        h.frame(vec![away.clone(), Event::PointerMoved(inside), wheel()]);
        for _ in 0..40 {
            h.frame(vec![]);
        }
        assert_eq!(h.value, selected, "the pointer left the row between two frames ({away:?})");
    }
}

/// Two sliders between buttons on either side, as the develop panel sits between the photo and the window edge.
struct Panel {
    view: HeadlessView,
    specs: [ControlSpec; 2],
    time: f64,
}
impl Panel {
    fn new() -> Self {
        let specs = [*controls::find("light.exposure").unwrap(), *controls::find("light.contrast").unwrap()];
        let mut p = Self { view: HeadlessView::new(), specs, time: 0.0 };
        for _ in 0..3 {
            p.frame(vec![]);
        }
        p
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.02;
        let specs = &self.specs;
        self.view.run(HeadlessView::raw_input(vec2(400.0, 200.0), 1.0, self.time, events), |ui| {
            let full = ui.max_rect();
            ui.put(Rect::from_min_size(egui::pos2(full.left(), full.top() + 20.0), vec2(40.0, 20.0)), egui::Button::new("left"));
            ui.put(Rect::from_min_size(egui::pos2(full.right() - 40.0, full.top() + 20.0), vec2(40.0, 20.0)), egui::Button::new("right"));
            let column = Rect::from_min_max(egui::pos2(full.left() + 60.0, full.top()), egui::pos2(full.right() - 60.0, full.bottom()));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(column));
            for spec in specs {
                slider(&mut child, spec, spec.default, true, None);
            }
        });
    }
    fn tab(&mut self) {
        self.frame(vec![Event::Key { key: Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![]);
    }
    /// Whether a focus ring is painted around each slider's track, in order.
    fn outlined(&self) -> Vec<bool> {
        let registry = take_registry(&self.view.ctx);
        let stroke = crate::widgets::focus_stroke(&crate::theme::Tokens::get(&self.view.ctx));
        let shapes = self.view.painted_shapes();
        self.specs
            .iter()
            .map(|spec| {
                let track = registry.iter().find(|(id, _)| id == &format!("slider:{}", spec.id)).map(|(_, r)| *r);
                track.is_some_and(|track| {
                    shapes.iter().any(|s| matches!(s, egui::epaint::Shape::Rect(r) if r.stroke == stroke && r.rect.contains_rect(track)))
                })
            })
            .collect()
    }
}

/// The slider Tab selects is outlined, so a keyboard user sees which one ↑ and ↓ will move; the outline follows
/// the focus to the next slider and goes when the focus leaves.
#[test]
fn a_tab_selected_slider_is_outlined() {
    let mut p = Panel::new();
    assert_eq!(p.outlined(), [false, false], "nothing is outlined before Tab");
    let mut walk = Vec::new();
    for _ in 0..12 {
        p.tab();
        let outlined = p.outlined();
        walk.push((super::slider_has_focus(&p.view.ctx), outlined[0], outlined[1]));
    }
    let exposure = walk.iter().position(|(slider, ..)| *slider).expect("Tab reaches a slider");
    assert_eq!(walk[exposure], (true, true, false), "the first slider Tab reaches is outlined, and only it: {walk:?}");
    let contrast = exposure + 1 + walk[exposure + 1..].iter().position(|(slider, ..)| *slider).expect("Tab reaches the second slider");
    assert_eq!(walk[contrast], (true, false, true), "the outline moves to the next slider: {walk:?}");
    assert!(walk[exposure + 1..contrast].iter().all(|(_, e, c)| !e && !c), "and nothing is outlined between them: {walk:?}");
}

/// Light-panel sliders between buttons, each with its value kept as the panel would keep it.
struct Column {
    view: HeadlessView,
    specs: Vec<ControlSpec>,
    enabled: Vec<bool>,
    values: Vec<f64>,
    time: f64,
    registry: Vec<(String, Rect)>,
}
impl Column {
    fn new(ids: &[&str], enabled: &[bool]) -> Self {
        let specs: Vec<ControlSpec> = ids.iter().map(|id| *controls::find(id).unwrap()).collect();
        let values = specs.iter().map(|s| s.default).collect();
        let mut c = Self { view: HeadlessView::new(), specs, enabled: enabled.to_vec(), values, time: 0.0, registry: Vec::new() };
        for _ in 0..3 {
            c.frame(vec![]);
        }
        c
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.02;
        let mut outs = vec![None; self.specs.len()];
        let (specs, values, enabled) = (&self.specs, &self.values, &self.enabled);
        self.view.run(HeadlessView::raw_input(vec2(400.0, 300.0), 1.0, self.time, events), |ui| {
            let full = ui.max_rect();
            ui.put(Rect::from_min_size(egui::pos2(full.left(), full.top() + 20.0), vec2(40.0, 20.0)), egui::Button::new("left"));
            let column = Rect::from_min_max(egui::pos2(full.left() + 60.0, full.top()), egui::pos2(full.right() - 60.0, full.bottom()));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(column));
            for (i, spec) in specs.iter().enumerate() {
                outs[i] = slider(&mut child, spec, values[i], enabled[i], None).value;
            }
            ui.put(Rect::from_min_size(egui::pos2(full.right() - 40.0, full.bottom() - 30.0), vec2(40.0, 20.0)), egui::Button::new("after"));
        });
        self.registry = take_registry(&self.view.ctx);
        for (v, out) in self.values.iter_mut().zip(outs) {
            *v = out.unwrap_or(*v);
        }
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        self.settle();
    }
    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
        self.settle();
    }
    fn click_value(&mut self, id: &str) {
        let pos = self.registry.iter().find(|(w, _)| w == &format!("sliderValue:{id}")).map(|(_, r)| r.center()).unwrap();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        self.settle();
    }
    /// The slider whose value is being typed, if any.
    fn typing(&self) -> Option<&'static str> {
        let focused = self.view.ctx.memory(|m| m.focused())?;
        self.specs.iter().find(|s| crate::text_field::id(&format!("sliderField:{}", s.id)) == focused).map(|s| s.id)
    }
    fn value(&self, id: &str) -> f64 {
        self.values[self.specs.iter().position(|s| s.id == id).unwrap()]
    }
    /// The sliders with a focus ring painted around their track.
    fn outlined(&self) -> Vec<&'static str> {
        let stroke = crate::widgets::focus_stroke(&crate::theme::Tokens::get(&self.view.ctx));
        let shapes = self.view.painted_shapes();
        let track = |id: &str| self.registry.iter().find(|(w, _)| w == &format!("slider:{id}")).map(|(_, r)| *r);
        self.specs
            .iter()
            .filter(|s| {
                track(s.id).is_some_and(|t| {
                    shapes.iter().any(|sh| matches!(sh, egui::epaint::Shape::Rect(r) if r.stroke == stroke && r.rect.contains_rect(t)))
                })
            })
            .map(|s| s.id)
            .collect()
    }
}

/// Typing a value and pressing Tab applies it and types into the next slider's value, skipping a slider that is
/// off; Shift+Tab goes back; Return applies the last and leaves its slider selected.
#[test]
fn tab_in_a_typed_value_types_into_the_next_slider() {
    let mut c = Column::new(&["light.exposure", "light.contrast", "light.highlights", "light.shadows"], &[true, false, true, true]);
    c.click_value("light.exposure");
    assert_eq!(c.typing(), Some("light.exposure"));
    c.type_text("1");
    c.key(Key::Tab, Modifiers::NONE);
    assert_eq!(c.value("light.exposure"), 1.0, "Tab applies the value typed");
    assert_eq!(c.typing(), Some("light.highlights"), "and types into the next slider that is on");
    assert_eq!(c.outlined(), ["light.highlights"], "which is outlined, and only it");
    c.type_text("-30");
    c.key(Key::Tab, Modifiers::NONE);
    assert_eq!(c.typing(), Some("light.shadows"));
    c.type_text("25");
    c.key(Key::Tab, Modifiers::SHIFT);
    assert_eq!((c.value("light.highlights"), c.value("light.shadows")), (-30.0, 25.0));
    assert_eq!(c.typing(), Some("light.highlights"), "Shift+Tab goes back");
    c.type_text("-40");
    c.key(Key::Enter, Modifiers::NONE);
    assert_eq!(c.value("light.highlights"), -40.0);
    assert_eq!(c.typing(), None, "Return ends the typing");
    assert!(super::slider_has_focus(&c.view.ctx), "and leaves the slider selected, so Tab carries on from it");
    assert_eq!(c.value("light.contrast"), 0.0, "the slider that is off is never typed into");
}

/// From the last slider's value Tab leaves the sliders as it does any field: the value is applied and the focus
/// goes to the next control.
#[test]
fn tab_from_the_last_value_applies_it_and_moves_on() {
    let mut c = Column::new(&["light.exposure", "light.contrast"], &[true, true]);
    c.click_value("light.contrast");
    c.type_text("12");
    c.key(Key::Tab, Modifiers::NONE);
    assert_eq!(c.value("light.contrast"), 12.0);
    assert_eq!(c.typing(), None);
    assert!(!super::slider_has_focus(&c.view.ctx), "the focus moves past the sliders");
}

/// Tab goes from slider to slider (a slider's name and value are no stops of their own), and Return on the selected
/// slider types a value, which Esc leaves unchanged.
#[test]
fn tab_goes_slider_to_slider_and_return_types_a_value() {
    let mut c = Column::new(&["light.exposure", "light.contrast", "light.highlights"], &[true, true, true]);
    let mut stops = Vec::new();
    for _ in 0..6 {
        c.key(Key::Tab, Modifiers::NONE);
        stops.push((c.view.ctx.memory(|m| m.focused()), super::slider_has_focus(&c.view.ctx)));
    }
    let first = stops.iter().position(|(_, slider)| *slider).expect("Tab reaches a slider");
    assert!(stops[first..first + 3].iter().all(|(_, slider)| *slider), "three Tabs, three sliders: {stops:?}");
    assert_eq!(stops[first + 1..first + 3].iter().map(|(f, _)| *f).collect::<std::collections::HashSet<_>>().len(), 2);
    // back to the middle slider, then Return
    c.key(Key::Tab, Modifiers::SHIFT);
    while !super::slider_has_focus(&c.view.ctx) || c.view.ctx.memory(|m| m.focused()) != stops[first + 1].0 {
        c.key(Key::Tab, Modifiers::SHIFT);
    }
    c.key(Key::Enter, Modifiers::NONE);
    assert_eq!(c.typing(), Some("light.contrast"), "Return on the selected slider types its value");
    c.type_text("40");
    c.key(Key::Escape, Modifiers::NONE);
    assert_eq!((c.typing(), c.value("light.contrast")), (None, 0.0), "Esc keeps the old value");
    assert!(super::slider_has_focus(&c.view.ctx), "and goes back to the slider");
    c.key(Key::Enter, Modifiers::NONE);
    c.type_text("40");
    c.key(Key::Enter, Modifiers::NONE);
    assert_eq!(c.value("light.contrast"), 40.0, "Return applies it");
}
