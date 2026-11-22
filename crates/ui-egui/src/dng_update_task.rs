//! Metadata ▸ Update DNG Preview & Metadata in the background: the previews are set up on the UI
//! thread ([`lcc_engine::dng_update::prepare`]), and the files are read, rendered and rewritten on a
//! worker ([`lcc_engine::dng_update::UpdateJob::run`]) while the window keeps drawing; back on the
//! UI thread the session drops what it cached of them ([`lcc_engine::dng_update::finish`]). Its row
//! in the activity stack shows the count and ✕ (cancel between photos).
//!
//! Sources: own design, after `render_dng_task`.

use crate::json_ext::JsonExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};

use lcc_engine::activity::{Cancel, TaskGuard};
use lcc_engine::dng_update::{self, Updated};
use serde_json::{Value, json};

use crate::LccApp;

pub struct DngUpdateTask {
    pub total: usize,
    done: Arc<AtomicUsize>,
    rx: Receiver<(Vec<Updated>, bool)>,
    guard: TaskGuard,
    library_generation: u64,
}

/// Start the update for `p` on a worker → `{background: true, total}`.
pub fn start(app: &mut LccApp, p: &Value) -> Option<Result<Value, String>> {
    if app.dng_update.is_some() {
        return Some(Err("an update of DNG previews is already running".into()));
    }
    let library_generation = app.session.library_generation();
    let job = match dng_update::prepare(&mut app.session, p) {
        Ok(j) => j,
        Err(e) => return Some(Err(e.to_string())),
    };
    let total = job.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let guard = app.session.activity.start("dngUpdate", "Updating DNG previews", Cancel::Flag(cancel.clone()));
    guard.progress(0, total as u64);
    let (tx, rx) = channel();
    let (c, d) = (cancel, done.clone());
    let work = move || {
        let run = || {
            job.run(&mut |i| {
                d.store(i, Ordering::Relaxed);
                !c.load(Ordering::Relaxed)
            })
        };
        // a panicking rewrite reports as cancelled with nothing done instead of leaving the row up
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| (Vec::new(), true));
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("dng-update".into()).spawn(work) {
        return Some(Err(e.to_string()));
    }
    #[cfg(target_arch = "wasm32")]
    work();
    app.dng_update = Some(DngUpdateTask { total, done, rx, guard, library_generation });
    Some(Ok(json!({"background": true, "total": total})))
}

/// The toast's account of a finished update (`dng_update::finish`'s result).
pub fn note(r: &Value, total: usize) -> String {
    let ok = r["updated"].as_array().map_or(0, Vec::len);
    let mut m = crate::i18n::tr_format!("DNG previews updated: {ok} of {total}", ok = ok, total = total);
    let first = |k: &str, field: &str| {
        let f = r[k].as_array()?.first()?;
        Some(format!(" · {}: {}", f["name"].as_str().unwrap_or(""), f[field].as_str()?))
    };
    if let Some(why) = first("failed", "error").or_else(|| first("skipped", "reason")) {
        m += &why;
    }
    if r["cancelled"] == true {
        m += " · cancelled";
    }
    m
}

/// Per frame: keep the activity row up to date; when the worker is done, report.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let Some(task) = &app.dng_update else { return };
    match task.rx.try_recv() {
        Ok((done, cancelled)) => {
            let (total, generation) = (task.total, task.library_generation);
            app.dng_update = None;
            if app.session.library_generation() != generation {
                app.toast(ctx, crate::i18n::tr("DNG previews were updated in a different catalog"));
                return;
            }
            match dng_update::finish(&mut app.session, done, cancelled) {
                Ok(r) => {
                    let failed = r.field("failed").as_array().is_some_and(|f| !f.is_empty());
                    let msg = note(&r, total);
                    app.last_dng_update = Some(r);
                    app.toast_for(ctx, msg, if failed { 15.0 } else { crate::TOAST_SECONDS });
                }
                Err(e) => app.toast_for(ctx, e.to_string(), 15.0),
            }
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            task.guard.progress(task.done.load(Ordering::Relaxed) as u64, task.total as u64);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            app.dng_update = None;
            app.toast(ctx, crate::i18n::tr("Updating the DNG previews stopped unexpectedly"));
        }
    }
}
