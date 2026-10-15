//! Real egui pointer frames on a disabled slider: its name doesn't reset it (issue #1436).
use super::{SliderOut, slider, take_registry};
use crate::headless::HeadlessView;
use egui::{Event, Modifiers, PointerButton, Rect, vec2};
use lightcraft_develop::controls;

fn frame(view: &mut HeadlessView, time: f64, enabled: bool, events: Vec<Event>) -> (SliderOut, Rect) {
    let spec = controls::find("light.exposure").unwrap();
    let mut out = SliderOut::default();
    view.run(HeadlessView::raw_input(vec2(330.0, 100.0), 1.0, time, events), |ui| {
        out = slider(ui, spec, 1.25, enabled, None);
    });
    let name = take_registry(&view.ctx).into_iter().find(|(id, _)| id == "sliderLabel:light.exposure").unwrap().1;
    (out, name)
}

#[test]
fn a_disabled_sliders_name_ignores_a_double_click() {
    for enabled in [true, false] {
        let mut view = HeadlessView::new();
        let mut time = 0.0;
        let (_, name) = frame(&mut view, time, enabled, vec![]);
        let pos = name.left_center() + vec2(8.0, 0.0);
        let mut reset = false;
        for pressed in [None, Some(true), Some(false), Some(true), Some(false), None] {
            time += 0.02;
            let events = match pressed {
                None => vec![Event::PointerMoved(pos)],
                Some(pressed) => vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
            };
            let (out, _) = frame(&mut view, time, enabled, events);
            reset |= out.reset || out.value.is_some();
        }
        assert_eq!(reset, enabled, "enabled: {enabled}");
    }
}
