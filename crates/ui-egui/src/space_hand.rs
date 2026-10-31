//! Space held over a tool on the photo: the view's hand, as in Lightroom (issue #1895).
//!
//! Crop, Masking, Remove, Red Eye and the pickers (white balance, colour range, point colour, a
//! targeted adjustment) take the primary click and drag on the photo, so the loupe's own click
//! zoom and drag pan are out of reach while one is open. Holding Space lends them back until the
//! key goes up: a click zooms (Fit ↔ the click-zoom ratio, at the point clicked), a drag pans the
//! zoomed photo, and the tool sees neither. A quick tap with no click still toggles the zoom, on
//! its release. Without a tool Space toggles the zoom on the press, as before. A held key's
//! auto-repeat toggles neither (`shortcuts::handle` lets only a fresh press through).

use egui::{CursorIcon, Key, Rect, Sense};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::{RightPanel, ViewMode};

/// The longest Space press (s) that still toggles the zoom when it lets go without a click.
pub(crate) const TAP: f64 = 0.4;

/// Space is held over a tool on the photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceHand {
    /// When the key went down (egui input time, s).
    since: f64,
    /// A pointer button went down while it was held: letting go is not a tap.
    used: bool,
}

/// A tool on the photo takes the primary click and drag, so Space has to lend them to the view.
pub(crate) fn tool_owns_pointer(app: &LightcraftApp) -> bool {
    // the full-screen preview shows no tool overlays, but a picker still takes its click there
    let panel = !app.ui.fullscreen && matches!(app.ui.right, RightPanel::Crop | RightPanel::Masking | RightPanel::Remove | RightPanel::RedEye);
    let picker = matches!(app.ui.tool.as_str(), "wbPicker" | "colorRange" | "pointColor") || app.ui.tool.starts_with("tat:");
    app.ui.view == ViewMode::Detail && app.ui.slideshow.is_none() && app.session.active().is_some() && (panel || picker)
}

/// Space went down (a fresh press, not a repeat). Over a tool the hand takes over until the key
/// is up and the press does nothing else (true); otherwise false leaves it to Toggle Zoom.
pub(crate) fn begin(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    if !tool_owns_pointer(app) {
        return false;
    }
    // in the middle of a stroke or a drag the press neither zooms nor lends the pointer
    let (now, busy) = ctx.input(|i| (i.time, i.pointer.any_down()));
    if !busy && app.space_hand.is_none() {
        app.space_hand = Some(SpaceHand { since: now, used: false });
    }
    true
}

/// Runs every frame before the shortcuts: ends the hold once Space is up and a drag it started has
/// ended, or at once when the tool closes or a dialog opens. A quick tap with no click toggles the
/// zoom as it ends.
pub(crate) fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(mut hand) = app.space_hand else { return };
    let (held, pointer, pressed, now) = ctx.input(|i| (i.key_down(Key::Space), i.pointer.any_down(), i.pointer.any_pressed(), i.time));
    // a quick click can go down and up within one frame: no button is down then, but it pressed
    hand.used |= pointer || pressed;
    let tool = tool_owns_pointer(app) && app.ui.dialog.is_none();
    // held, or let go in the middle of a pan: the hand stays until the button is up
    if tool && (held || pointer) {
        app.space_hand = Some(hand);
        return;
    }
    app.space_hand = None;
    if tool && !hand.used && now - hand.since <= TAP {
        let _ = app.run("view.zoomToggle", json!({}));
    }
}

/// The hand over the photo while Space is held. Registered above the loupe and the tool overlays
/// (the navigator stays above it), so it takes the click or the drag and they get neither: they
/// still draw, without the pointer. `main` is the pane the photo `img` is laid out in; `before`
/// the Before pane of a side-by-side or stacked view with its photo, where a press there acts.
pub(crate) fn interact(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, main: (Rect, Rect), before: Option<(Rect, Rect)>) {
    if app.space_hand.is_none() {
        return;
    }
    let resp = ui.interact(canvas, egui::Id::unique("loupe-hand"), Sense::click_and_drag());
    crate::widgets::register(ui, "loupe:hand", canvas);
    let at = resp.interact_pointer_pos().or(resp.hover_pos());
    let (pane, img) = match (before, at) {
        (Some((pane, img)), Some(q)) if pane.contains(q) => (pane, img),
        _ => main,
    };
    if resp.is_pointer_button_down_on()
        && let Some(hand) = app.space_hand.as_mut()
    {
        hand.used = true;
    }
    let zoomed = crate::panels::detail::zoomed(img, pane);
    crate::panels::detail::click_zoom_and_pan(app, &resp, img, pane);
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(match (zoomed, resp.dragged()) {
            (true, true) => CursorIcon::Grabbing,
            (true, false) => CursorIcon::Grab,
            (false, _) => CursorIcon::ZoomIn,
        });
    }
}
