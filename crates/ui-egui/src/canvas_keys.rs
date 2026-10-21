//! Keyboard and screen-reader access to the handles drawn on the photo (crop corners and edges,
//! spot removals, gradient handles, mask pins). Each handle gets an invisible, focusable stand-in
//! at its place on the canvas: Tab reaches it, a screen reader announces its name and position,
//! and the arrow keys move it (Shift: ten times further). The pointer still grabs the handle as
//! before; the stand-in senses no clicks or drags.

use egui::{Id, Key, Pos2, Rect, Ui, vec2};

/// One arrow press moves a handle this far, as a fraction of the photo's width or height.
pub const STEP: f64 = 0.005;
/// With Shift held.
pub const BIG_STEP: f64 = 0.05;

fn focused_key() -> Id {
    Id::new("lc-canvas-handle-focused")
}

/// The egui id of the stand-in registered as `key` (to focus it, or find it in tests).
pub fn id(key: &str) -> Id {
    Id::new("lc-canvas-handle").with(key)
}

/// Whether a canvas handle holds the keyboard focus: it keeps the arrow keys and Return from the
/// app's shortcuts (next photo, and so on).
pub fn has_focus(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<Id>(focused_key()))
}

/// What the keys did to a focused handle this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct HandleKeys {
    /// The move asked for, in normalized photo units (x right, y down).
    pub delta: Option<(f64, f64)>,
    /// Return or Space was pressed.
    pub activated: bool,
}

/// `(x, y)` normalized, as a screen reader reads it: "x 30%, y 60%".
pub fn position_text(x: f64, y: f64) -> String {
    let pct = |v: f64| if v.is_finite() { (v * 100.0).round() as i64 } else { 0 };
    format!("x {}%, y {}%", pct(x), pct(y))
}

/// The focusable stand-in for a handle drawn at `at`, named `name` with `value` (its position or
/// size), registered for the control channel as `canvasHandle:<key>`.
pub fn handle(ui: &mut Ui, key: &str, name: &str, value: String, at: Pos2) -> HandleKeys {
    let id = id(key);
    let rect = Rect::from_center_size(at, vec2(14.0, 14.0));
    let resp = ui.interact(rect, id, egui::Sense::focusable_noninteractive());
    let label = name.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_value(value);
    });
    crate::widgets::register(ui.ctx(), format!("canvasHandle:{key}"), rect);
    let mut out = HandleKeys::default();
    if !resp.has_focus() {
        return out;
    }
    ui.data_mut(|d| d.insert_temp(focused_key(), id));
    // the arrows move the handle: without the lock egui also moves the focus to the next widget
    // that way, often another handle a few points off
    let lock = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
    ui.memory_mut(|m| m.set_focus_lock_filter(id, lock));
    // a ring shows where the focus is
    ui.painter().circle_stroke(at, 9.0, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
    ui.painter().circle_stroke(at, 11.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
    ui.input_mut(|i| {
        let shift = i.modifiers.shift;
        let m = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        let step = if shift { BIG_STEP } else { STEP };
        let (mut dx, mut dy) = (0.0, 0.0);
        if i.consume_key(m, Key::ArrowLeft) {
            dx -= step;
        }
        if i.consume_key(m, Key::ArrowRight) {
            dx += step;
        }
        if i.consume_key(m, Key::ArrowUp) {
            dy -= step;
        }
        if i.consume_key(m, Key::ArrowDown) {
            dy += step;
        }
        if dx != 0.0 || dy != 0.0 {
            out.delta = Some((dx, dy));
        }
        out.activated = i.consume_key(egui::Modifiers::NONE, Key::Enter) || i.consume_key(egui::Modifiers::NONE, Key::Space);
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_read_as_percentages_and_survive_nan() {
        assert_eq!(position_text(0.304, 0.6), "x 30%, y 60%");
        assert_eq!(position_text(f64::NAN, f64::INFINITY), "x 0%, y 0%");
    }
}
