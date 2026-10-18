//! Keeps the engine's focus-point reader going (`focus.pump`): it reads the camera's focus point for photos imported
//! before it was read, a few KB of each file, at the pace the face scan uses (nothing new while the user drags, types or
//! scrolls). Its progress is its row in the activity stack.

use serde_json::json;

use crate::LightcraftApp;

/// How often a running reader is asked for what it has read (its batches commit from here).
const BUSY_SECS: f64 = 0.25;

/// Called every frame. Idle, it costs a comparison and wakes nothing; reading, it asks again every [`BUSY_SECS`].
pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if now < app.caches.focus_next_pump {
        return;
    }
    let pace = crate::panels::faces::background_pace(app, ctx, now);
    let Ok(v) = app.session.execute("focus.pump", &json!({"pace": pace})) else { return };
    if v["active"] == true {
        app.caches.focus_next_pump = now + BUSY_SECS;
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(BUSY_SECS));
    }
}
