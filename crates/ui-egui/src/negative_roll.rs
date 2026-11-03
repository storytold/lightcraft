//! Sync Roll in the app (`negative.syncRoll`, `docs/negative-conversion.md`): every frame of the roll is read and
//! measured on a worker thread (a raw's decode takes a second or more), with a row in the activity stack that counts
//! the frames and can stop. The roll's analysis is then written to every frame through the session's command wrapper
//! (one undo step, saved, sidecars auto-written), unless another library was opened meanwhile. Agents get the same
//! with `wait: true`, which runs the engine's command in place.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_engine::cmd::negative;
use serde_json::{Value, json};

use crate::LightcraftApp;

/// What measuring a roll gives, or why it failed.
type Measured = Result<negative::RollMeasured, String>;

/// The activity stack's label while the frames are read.
pub const LABEL: &str = "Syncing roll";

/// `negative.syncRoll` from the app: start measuring the frames in the background → `{background: true, count}`.
/// `None` hands the command to the engine (with `wait: true`).
pub fn command(app: &mut LightcraftApp, p: &Value) -> Option<Result<Value, String>> {
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let plan = match negative::roll_plan(&mut app.session, p) {
        Ok(plan) => plan,
        Err(e) => return Some(Err(e.to_string())),
    };
    let count = plan.frames.len();
    let library = app.session.library_generation();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let stopped = cancel.clone();
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        let total = plan.frames.len() as u64;
        task.progress(0, total);
        let measured = negative::roll_measure(&plan, &flag, &|done| task.progress(done as u64, total));
        (plan, measured)
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, (plan, measured): (negative::RollPlan, Measured)| {
        // (stopped while the last frame was read or the roll pooled: nothing is written)
        if app.session.library_generation() != library || stopped.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let measured = match measured {
            Ok(m) => m,
            Err(e) if e == "stopped" => return,
            Err(e) => return app.toast_error(ctx, e),
        };
        let unreadable = measured.skipped.len();
        // as the command: one undo step, the durable save and the automatic XMP sidecar write
        match app.session.execute_fn("negative.syncRoll", |s| negative::roll_apply(s, plan, measured)) {
            Ok(r) if r["warning"].is_string() => {
                app.toast_error(ctx, crate::i18n::tr("Roll synced, but the frames' film bases differ: they look scanned differently (light, exposure or scanner). Undo, and sync each scanning setup on its own").to_string());
            }
            Ok(r) => {
                let synced = r["photos"].as_array().map_or(0, Vec::len);
                // (the frames skipped there: those unreadable, then those edited while the roll was read)
                let changed = r["skipped"].as_array().map_or(0, Vec::len).saturating_sub(unreadable);
                let message = if unreadable + changed == 0 {
                    crate::i18n::tr_format!("Roll synced: {n} frames share one film base and range", n = synced)
                } else if changed == 0 {
                    crate::i18n::tr_format!("Roll synced: {n} frames ({m} could not be read)", n = synced, m = unreadable)
                } else {
                    crate::i18n::tr_format!(
                        "Roll synced: {n} frames ({m} could not be read; {c} edited during the sync were left as they were)",
                        n = synced,
                        m = unreadable,
                        c = changed
                    )
                };
                app.toast(ctx, message);
            }
            Err(e) => app.toast_error(ctx, e.to_string()),
        }
    };
    if let Err(e) = crate::tasks::spawn_tracked(app, LABEL, "negativeRoll", cancel, work, done) {
        return Some(Err(e));
    }
    Some(Ok(json!({"background": true, "count": count})))
}
