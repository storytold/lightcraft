//! Photo ▸ Render to DNG in the background: the renders are set up on the UI thread
//! ([`lcc_engine::render_dng::prepare`]), rendered and written on a worker
//! ([`lcc_engine::render_dng::RenderDngJob::run`]) while the window keeps drawing, and the
//! DNGs are added to the library, stacked on their originals, back on the UI thread when the
//! worker is done ([`lcc_engine::render_dng::finish`]). Its row in the activity stack shows
//! the count and ✕ (cancel between photos).
//!
//! Needs [`crate::Services::write_shared`]; without it (web, headless scripts) the engine command
//! runs synchronously.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};

use lcc_engine::activity::{Cancel, TaskGuard};
use lcc_engine::render_dng::{self, Rendered};
use serde_json::{Value, json};

use crate::LccApp;

pub struct RenderDngTask {
    pub total: usize,
    done: Arc<AtomicUsize>,
    rx: Receiver<(Vec<Rendered>, bool)>,
    guard: TaskGuard,
}

/// Start Render to DNG for `p` on a worker → `{background: true, total}`; `None` when this app
/// can't write from a worker (the engine command then runs as usual).
pub fn start(app: &mut LccApp, p: &Value) -> Option<Result<Value, String>> {
    let write = app.services.write_shared.clone()?;
    if app.render_dng.is_some() {
        return Some(Err("Render to DNG is already running".into()));
    }
    let job = match render_dng::prepare(&mut app.session, p) {
        Ok(j) => j,
        Err(e) => return Some(Err(e.to_string())),
    };
    let total = job.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let guard = app.session.activity.start("renderDng", "Rendering to DNG", Cancel::Flag(cancel.clone()));
    guard.progress(0, total as u64);
    let (tx, rx) = channel();
    let (c, d) = (cancel, done.clone());
    let work = move || {
        let run = || {
            job.run(&mut |path, bytes| write(path, bytes), &mut |i| {
                d.store(i, Ordering::Relaxed);
                !c.load(Ordering::Relaxed)
            })
        };
        // a panicking render reports as cancelled with nothing made instead of leaving the row up
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| (Vec::new(), true));
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("render-dng".into()).spawn(work) {
        return Some(Err(e.to_string()));
    }
    #[cfg(target_arch = "wasm32")]
    work();
    app.render_dng = Some(RenderDngTask { total, done, rx, guard });
    Some(Ok(json!({"background": true, "total": total})))
}

/// Per frame: keep the activity row up to date; when the worker is done, add the DNGs and report.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let Some(task) = &app.render_dng else { return };
    match task.rx.try_recv() {
        Ok((done, cancelled)) => {
            let total = task.total;
            app.render_dng = None;
            let msg = match render_dng::finish(&mut app.session, done, cancelled) {
                Ok(r) => {
                    let ok = r["copies"].as_array().map_or(0, Vec::len);
                    let mut m = crate::i18n::tr_format!("Rendered {ok} of {total} to DNG", ok = ok, total = total);
                    if let Some(first) = r["failed"].as_array().and_then(|f| f.first()) {
                        m += &format!(" · {}", first["error"].as_str().unwrap_or("error"));
                    }
                    if cancelled {
                        m += " · cancelled";
                    }
                    app.last_render_dng = Some(r);
                    m
                }
                Err(e) => e.to_string(),
            };
            app.toast(ctx, msg);
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            task.guard.progress(task.done.load(Ordering::Relaxed) as u64, task.total as u64);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            app.render_dng = None;
            app.toast(ctx, crate::i18n::tr("Render to DNG stopped unexpectedly"));
        }
    }
}
