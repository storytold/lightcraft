//! The hand-painted hit area: the face its caller paints stays under the leaf's keyboard focus ring.

use std::cell::Cell;
use std::sync::Arc;

use craft::ui::contract::Show as _;
use egui::{Color32, Shape, Ui, vec2};

use crate::headless::HeadlessView;
use crate::theme::Tokens;
use crate::widgets::HitArea;

/// A face colour nothing else in these frames paints, so the shapes are told apart.
const FACE: Color32 = Color32::from_rgb(1, 2, 3);

/// One framework root running on a bare egui context, as the dense-button tests do it: no renderer, no app.
struct Frames {
    ctx: egui::Context,
    time: f64,
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Frames { ctx, time: 0.0, shapes: Vec::new() }
    }

    fn frame(&mut self, events: Vec<egui::Event>, mut draw: impl FnMut(&mut Ui)) {
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(400.0, 300.0), 1.0, self.time, events);
        let mut out = self.ctx.run_ui(raw, |ui| draw(ui));
        out.textures_delta.clear();
        self.shapes = out.shapes;
    }

    /// The rectangle shapes of the last frame, in paint order (a paint slot holds its shapes in a `Vec`).
    fn rects(&self) -> Vec<egui::epaint::RectShape> {
        fn walk(shape: &Shape, out: &mut Vec<egui::epaint::RectShape>) {
            match shape {
                Shape::Rect(rect) => out.push(rect.clone()),
                Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &self.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }
}

/// A key pressed and released in one frame: the framework shows a focus ring only for focus a key reached, so a test
/// that only asks for the focus paints none.
fn key(key: egui::Key) -> Vec<egui::Event> {
    [true, false]
        .map(|pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE })
        .to_vec()
}

/// An area's face fills the very rectangle its leaf paints the inset keyboard focus ring in: painted after the leaf,
/// it would hide the ring from keyboard users.
#[test]
fn the_area_face_paints_under_the_keyboard_focus_ring() {
    let mut frames = Frames::new();
    let leaf = Cell::new(egui::Id::NULL);
    let mut draw = |ui: &mut Ui| {
        let (_, rect) = ui.allocate_space(vec2(200.0, 24.0));
        let area = HitArea::new("Segment", ui.scope_id().with("hit-area-ring")).show_at(ui, rect);
        // (the leaf's own id: where the keyboard focus goes)
        leaf.set(area.response.id);
        // the caller's face, over the whole rectangle
        ui.painter().rect_filled(rect, 4.0, FACE);
        area.face_under_ring();
    };
    frames.frame(Vec::new(), &mut draw);
    // the keyboard focus goes to the area, and a key is pressed: the framework paints a focus ring only for focus a key
    // reached, as the ring a person tabbing through the panel sees
    let focused = leaf.get();
    frames.ctx.memory_mut(|memory| memory.request_focus(focused));
    frames.frame(Vec::new(), &mut draw);
    frames.frame(key(egui::Key::X), &mut draw);
    frames.frame(Vec::new(), &mut draw);
    let rects = frames.rects();
    let accent = Tokens::get(&frames.ctx).accent;
    let face = rects.iter().position(|rect| rect.fill == FACE).expect("the caller's face is painted");
    let ring = rects.iter().position(|rect| rect.stroke.width == 2.0 && rect.stroke.color == accent).expect("the keyboard focus ring is painted");
    assert!(face < ring, "the focus ring paints above the face: face at {face}, ring at {ring}");
}
