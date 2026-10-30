//! Reduce Transparency (Settings ▸ Interface ▸ Reduce transparency): the see-through badges, labels
//! and backdrops are drawn solid.
//!
//! What is solid: the HUD-style pills and bars painted over a photo (rating and flag bars, badges,
//! the filmstrip's, the toasts, the slideshow's controls, the Detail view's labels and the
//! navigator's frame) and the backdrops behind a dialog, a notice or a dragged-in file. What
//! stays as it is: what is drawn *of* the photo (crop shading, mask overlays, the histogram's
//! zones, Lights Out's dimming, the dimming of rejected photos, the before/after wipe): they show
//! the photo's own state, and fades that tell a state change (see [`crate::motion`]). Not covered
//! yet: the print layout's caption pills, the slide layout preview's band, the import review's
//! thumbnail badges and the frame-rate overlay.
//!
//! [`LightcraftApp::sync_theme`](crate::LightcraftApp::sync_theme) stores the setting in the egui
//! context every frame ([`sync`]), so drawing code needs only the `Context`, not the app.

use egui::{Color32, Context, Id};

use crate::theme::Tokens;

fn id() -> Id {
    Id::unique("lightcraft_reduce_transparency")
}

/// Record the setting for this frame.
pub fn sync(ctx: &Context, reduced: bool) {
    if ctx.data(|d| d.get_temp::<bool>(id())) != Some(reduced) {
        ctx.data_mut(|d| d.insert_temp(id(), reduced));
    }
}

/// Whether see-through surfaces should be solid.
pub fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(id())).unwrap_or(false)
}

/// The fill of a pill, badge or bar over a photo: black at `alpha`, or, reduced, a solid near-black
/// that keeps the strength (a hovered badge stays darker than a resting one).
pub fn hud(ctx: &Context, alpha: u8) -> Color32 {
    if reduced(ctx) { Color32::from_gray(((255 - u16::from(alpha)) / 8) as u8) } else { Color32::from_black_alpha(alpha) }
}

/// The backdrop behind a dialog, a notice or a dragged-in file: black at `alpha`, or, reduced, the
/// theme's solid backdrop.
pub fn scrim(ctx: &Context, alpha: u8) -> Color32 {
    if reduced(ctx) { Tokens::get(ctx).grid_bg } else { Color32::from_black_alpha(alpha) }
}

/// `color` without its see-through part when reduced (a dark pill that is not black).
pub fn solid(ctx: &Context, color: Color32) -> Color32 {
    if reduced(ctx) {
        let [r, g, b, _] = color.to_srgba_unmultiplied();
        Color32::from_rgb(r, g, b)
    } else {
        color
    }
}

/// `color` at `factor` of its opacity, or, reduced, as it is.
pub fn fade(ctx: &Context, color: Color32, factor: f32) -> Color32 {
    if reduced(ctx) { color } else { color.gamma_multiply(factor) }
}
