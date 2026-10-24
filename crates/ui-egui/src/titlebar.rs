//! The integrated title bar's window gestures (macOS: the native title bar is hidden and the top
//! bar takes its place, so dragging and double-click-to-zoom have to be done here).
//!
//! Pure policy, no egui widgets: [`command_for`] maps a gesture on the bar's empty space and the
//! window's current state to the viewport command the host should run.

use egui::{CursorIcon, Id, LayerId, Order, PointerButton, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;

/// One caption button: as wide as the native Windows ones.
pub const BUTTON_WIDTH: f32 = 46.0;
/// The three caption buttons together.
pub const WIDTH: f32 = 3.0 * BUTTON_WIDTH;
/// Resize zone thickness along an edge, and the side of a corner zone.
const EDGE: f32 = 5.0;
const CORNER: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// Left to right.
pub const CAPTIONS: [Caption; 3] = [Caption::Minimize, Caption::Maximize, Caption::Close];

/// Caption button ids (stable, so tests and agents can find them).
pub fn caption_id(c: Caption) -> Id {
    Id::new(("titlebar-caption", c as u8))
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

fn toggle_maximize(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(ctx)));
}

/// The bar's background: a press-and-drag on empty space moves the window, a double-click toggles
/// maximize. Call it before laying out the bar so its widgets, registered later, take the clicks.
pub fn drag_area(ui: &Ui, rect: Rect) {
    let ctx = ui.ctx();
    let resp = ui.interact(rect, Id::new("titlebar-drag"), Sense::click_and_drag());
    // egui hands a drag that starts on a click-only widget to the drag-sensing
    // background below it, so remember whether the press itself landed on bare background.
    if ctx.input(|i| i.pointer.any_pressed()) {
        let bare = ctx.viewport(|v| v.hits.click.is_some_and(|w| w.id == resp.id));
        ctx.data_mut(|d| d.insert_temp(resp.id, bare));
    }
    if resp.double_clicked() {
        toggle_maximize(ctx);
    } else if resp.drag_started_by(PointerButton::Primary) && ctx.data(|d| d.get_temp(resp.id).unwrap_or(false)) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Minimize, Maximize/Restore and Close at the right end of `bar` (the app bar's outer rect), full
/// bar height down to its bottom border, flush with the window's edge so a maximized window's
/// corner hits Close. Close runs `app.quit`, which asks about unsaved changes first.
pub fn caption_buttons(app: &mut LightcraftApp, ui: &mut Ui, bar: Rect) {
    let t = Tokens::get(ui.ctx());
    let max = maximized(ui.ctx());
    let mut clicked = None;
    for (i, c) in CAPTIONS.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(bar.right() - WIDTH + i as f32 * BUTTON_WIDTH, bar.top()), vec2(BUTTON_WIDTH, bar.height() - 1.0));
        let resp = ui.interact(r, caption_id(c), Sense::click());
        let close = c == Caption::Close;
        let fill = match (resp.is_pointer_button_down_on(), resp.hovered()) {
            (true, _) if close => t.caption_close.gamma_multiply(0.8),
            (_, true) if close => t.caption_close,
            (true, _) => t.tool_active,
            (_, true) => t.hover,
            _ => egui::Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(r, 0.0, fill);
        let ink = if close && resp.hovered() { t.caption_close_text } else { t.icon };
        paint_glyph(ui, c, max, r.center(), Stroke::new(1.0, ink));
        let tip = match c {
            Caption::Minimize => crate::i18n::tr("Minimize"),
            Caption::Maximize if max => crate::i18n::tr("Restore"),
            Caption::Maximize => crate::i18n::tr("Maximize"),
            Caption::Close => crate::i18n::tr("Close"),
        };
        if resp.on_hover_text(tip).clicked() {
            clicked = Some(c);
        }
    }
    match clicked {
        Some(Caption::Minimize) => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
        Some(Caption::Maximize) => toggle_maximize(ui.ctx()),
        Some(Caption::Close) => {
            app.run("app.quit", json!({})).ok();
        }
        None => {}
    }
}

/// The 10 pt line glyphs of the caption buttons.
fn paint_glyph(ui: &Ui, c: Caption, maximized: bool, at: egui::Pos2, s: Stroke) {
    let p = ui.painter();
    let g = Rect::from_center_size(at, vec2(10.0, 10.0));
    match c {
        Caption::Minimize => {
            p.line_segment([g.left_center(), g.right_center()], s);
        }
        // Restore: a front square with the corner of the one behind it.
        Caption::Maximize if maximized => {
            let front = Rect::from_min_max(g.min + vec2(0.0, 2.0), g.max - vec2(2.0, 0.0));
            p.rect_stroke(front, 0.0, s, egui::StrokeKind::Middle);
            p.line(vec![g.min + vec2(2.0, 2.0), g.min + vec2(2.0, 0.0), g.right_top(), g.max - vec2(0.0, 2.0), g.max - vec2(2.0, 2.0)], s);
        }
        Caption::Maximize => {
            p.rect_stroke(g, 0.0, s, egui::StrokeKind::Middle);
        }
        Caption::Close => {
            p.line_segment([g.left_top(), g.right_bottom()], s);
            p.line_segment([g.right_top(), g.left_bottom()], s);
        }
    }
}

/// The edge and corner zones of a window with content rect `w`: (zone, direction, cursor). Corners
/// come last so they win where they overlap the edges.
fn zones(w: Rect) -> [(Rect, ResizeDirection, CursorIcon); 8] {
    use ResizeDirection::*;
    let (e, c) = (EDGE, CORNER);
    [
        (Rect::from_min_max(w.left_top(), pos2(w.right(), w.top() + e)), North, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(w.left(), w.bottom() - e), w.right_bottom()), South, CursorIcon::ResizeVertical),
        (Rect::from_min_max(w.left_top(), pos2(w.left() + e, w.bottom())), West, CursorIcon::ResizeHorizontal),
        (Rect::from_min_max(pos2(w.right() - e, w.top()), w.right_bottom()), East, CursorIcon::ResizeHorizontal),
        (Rect::from_min_size(w.left_top(), vec2(c, c)), NorthWest, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(w.right_top() - vec2(c, 0.0), vec2(c, c)), NorthEast, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.left_bottom() - vec2(0.0, c), vec2(c, c)), SouthWest, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.right_bottom() - vec2(c, c), vec2(c, c)), SouthEast, CursorIcon::ResizeNwSe),
    ]
}

/// Invisible resize zones along the window's edges, above everything else; a press in one starts
/// an OS resize in that direction. None while the window is maximized or fullscreen.
pub fn resize_zones(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if maximized(&ctx) || ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let w = ctx.content_rect();
    let layer = LayerId::new(Order::Foreground, Id::new("titlebar-resize"));
    let zui = ui.new_child(egui::UiBuilder::new().layer_id(layer).max_rect(w));
    for (i, (r, dir, cursor)) in zones(w).into_iter().enumerate() {
        let resp = zui.interact(r, Id::new(("titlebar-resize", i)), Sense::drag()).on_hover_cursor(cursor);
        if resp.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

/// What the user did on the bar's empty space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A primary-button drag began: the window follows the pointer.
    DragStarted,
    /// A primary-button double-click: zoom the window, or restore it when already zoomed.
    DoubleClicked,
}

/// The window as the host reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// Zoomed (macOS) / maximized (elsewhere).
    pub maximized: bool,
    /// In the OS's full-screen mode.
    pub fullscreen: bool,
}

/// The viewport command for `gesture`, if any. Full screen has no title bar to drag or zoom.
pub fn command_for(gesture: Gesture, window: WindowState) -> Option<ViewportCommand> {
    if window.fullscreen {
        return None;
    }
    match gesture {
        Gesture::DragStarted => Some(ViewportCommand::StartDrag),
        Gesture::DoubleClicked => Some(ViewportCommand::Maximized(!window.maximized)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: WindowState = WindowState { maximized: false, fullscreen: false };
    const ZOOMED: WindowState = WindowState { maximized: true, fullscreen: false };
    const FULL: WindowState = WindowState { maximized: false, fullscreen: true };

    // Scenario: dragging the title bar moves the window
    #[test]
    fn given_a_normal_window_when_dragging_then_the_window_follows_the_pointer() {
        assert_eq!(command_for(Gesture::DragStarted, NORMAL), Some(ViewportCommand::StartDrag));
    }

    // Scenario: a zoomed window can still be dragged (macOS lets you pull it free)
    #[test]
    fn given_a_zoomed_window_when_dragging_then_the_window_follows_the_pointer() {
        assert_eq!(command_for(Gesture::DragStarted, ZOOMED), Some(ViewportCommand::StartDrag));
    }

    // Scenario: double-click zooms
    #[test]
    fn given_a_normal_window_when_double_clicking_then_it_zooms() {
        assert_eq!(command_for(Gesture::DoubleClicked, NORMAL), Some(ViewportCommand::Maximized(true)));
    }

    // Scenario: double-click again restores
    #[test]
    fn given_a_zoomed_window_when_double_clicking_then_it_is_restored() {
        assert_eq!(command_for(Gesture::DoubleClicked, ZOOMED), Some(ViewportCommand::Maximized(false)));
    }

    // Scenario: full screen ignores both
    #[test]
    fn given_a_full_screen_window_then_neither_gesture_does_anything() {
        assert_eq!(command_for(Gesture::DragStarted, FULL), None);
        assert_eq!(command_for(Gesture::DoubleClicked, FULL), None);
    }
}
