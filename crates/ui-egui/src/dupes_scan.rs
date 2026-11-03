//! Keeps the engine's duplicate finder going (`duplicates.pump`): Photo ▸ Find Duplicate Photos hashes the library's
//! photos on a worker thread at the pace the face scan uses (nothing new while the user drags, types or scrolls), and
//! shows the groups when it is done. Its progress is its row in the activity stack.

use serde_json::json;

use crate::LightcraftApp;

/// How often a running finder is asked for what it has hashed.
const BUSY_SECS: f64 = 0.25;

/// Called every frame. Idle, it costs a comparison and wakes nothing; hashing, it asks again every [`BUSY_SECS`].
pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context, now: f64) {
    if now < app.caches.dupes_next_pump {
        return;
    }
    // the review window is open: the user is waiting for this run, so it gets the machine
    let watching = matches!(app.ui.dialog, Some(crate::state::Dialog::Duplicates { done: false, .. }));
    let pace = if watching { "normal" } else { crate::panels::faces::background_pace(app, ctx, now) };
    let Ok(v) = app.session.execute("duplicates.pump", &json!({"pace": pace})) else { return };
    if v["active"] == true {
        app.caches.dupes_next_pump = now + BUSY_SECS;
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(BUSY_SECS));
    }
}
