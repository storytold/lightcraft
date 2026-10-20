//! The one place every animation asks whether motion is reduced (Settings ▸ Interface ▸ Reduce motion).
//!
//! The rule: with reduce motion on, things that move or turn stop moving (the click-zoom tween,
//! egui's own widget easing, scrolling to a photo (it jumps instead of gliding), spinners, the activity bar's sweep); opacity fades that tell a state
//! change stay (toasts, the slideshow crossfade and its control bar).
//!
//! [`LightcraftApp::sync_theme`](crate::LightcraftApp::sync_theme) stores the setting in the egui
//! context every frame ([`sync`]), so drawing code needs only the `Context`, not the app.

use egui::{Context, Id};

/// egui's widget easing time when motion is on.
const ANIMATION_TIME: f32 = 0.08;
/// How long the click-zoom tween runs when motion is on.
pub const ZOOM_TWEEN_SECONDS: f32 = 0.22;

fn id() -> Id {
    Id::unique("lightcraft_reduce_motion")
}

/// Record the setting for this frame and set egui's easing time to match (0 when reduced).
pub fn sync(ctx: &Context, reduced: bool) {
    if ctx.data(|d| d.get_temp::<bool>(id())) == Some(reduced) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id(), reduced));
    let time = animation_time(reduced);
    // `scroll_to_rect` / `scroll_to_me` (follow-the-active-photo in the grid and filmstrip, pickers)
    // glide by `scroll_animation`, not `animation_time`: reduced, they jump.
    let scroll = if reduced { egui::style::ScrollAnimation::none() } else { egui::style::ScrollAnimation::default() };
    ctx.all_styles_mut(|s| {
        s.animation_time = time;
        s.scroll_animation = scroll;
    });
}

/// Whether things that move or turn should stand still.
pub fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(id())).unwrap_or(false)
}

/// egui's `animation_time` for the setting.
pub fn animation_time(reduced: bool) -> f32 {
    if reduced { 0.0 } else { ANIMATION_TIME }
}

/// How long a click-zoom tween runs: none when reduced.
pub fn zoom_tween_seconds(ctx: &Context) -> f32 {
    if reduced(ctx) { 0.0 } else { ZOOM_TWEEN_SECONDS }
}
