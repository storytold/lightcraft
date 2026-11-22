//! Keeps the engine's focus-point reader (`focus.pump`) and shared ML indexer (`ml.pump`) going at the pace the face
//! scan uses (nothing new while the user drags, types or scrolls). Their progress is their rows in the activity stack.

use crate::json_ext::JsonExt as _;
use serde_json::json;

use crate::LccApp;

/// How often a running reader is asked for what it has read (its batches commit from here).
const BUSY_SECS: f64 = 0.25;

/// Called every frame. Idle, it costs a comparison and wakes nothing; reading, it asks again every [`BUSY_SECS`].
pub fn pump(app: &mut LccApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let pace = crate::panels::faces::background_pace(app, ctx, now);
    if now >= app.caches.focus_next_pump
        && let Ok(v) = app.session.execute("focus.pump", &json!({"pace": pace}))
        && v.field("active") == true
    {
        app.caches.focus_next_pump = now + BUSY_SECS;
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(BUSY_SECS));
    }
    if now >= app.caches.ml_next_pump
        && let Ok(v) = app.session.execute("ml.pump", &json!({"pace": pace}))
    {
        let busy = v.field("active") == true && (v.field("pendingPhotos").as_u64().unwrap_or(0) > 0 || v.field("inFlight").as_u64().unwrap_or(0) > 0);
        app.caches.ml_next_pump = now + if busy { 0.05 } else { 1.0 };
        if v.field("added").as_u64().unwrap_or(0) > 0 {
            ctx.request_repaint();
        }
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}
