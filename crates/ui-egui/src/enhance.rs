//! Photo ▸ Enhance ▸ Super Resolution / AI Sharpen in the desktop app: the engine's
//! [`lightcraft_engine::enhance::EnhanceJob`] on a worker thread, with a row in the activity stack (cancellable),
//! finished on the UI thread ([`poll`], every frame): the new TIFF is imported, stacked and selected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use lightcraft_engine::activity::{Cancel, TaskGuard, Unit};
use lightcraft_engine::enhance::{EnhanceJob, EnhanceOutput};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};

/// Steps of the activity row's progress.
const STEPS: u64 = 1000;

/// A running enhancement.
pub struct EnhanceTask {
    job: EnhanceJob,
    progress: Arc<Mutex<(f32, String)>>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<EnhanceOutput, String>>,
    _guard: TaskGuard,
}

#[derive(Default)]
pub struct EnhanceState {
    pub task: Option<EnhanceTask>,
}

/// What a job is called in the activity stack and its messages (sentence case, as activity labels are).
fn title(factor: usize) -> &'static str {
    if factor == 1 { "AI sharpening" } else { "Super resolution" }
}

/// Start enhancing the selected photo at `factor` (1: AI Sharpen, 2: Super Resolution).
pub fn start(app: &mut LightcraftApp, factor: usize) -> Result<Value, String> {
    if app.enhance.task.is_some() {
        return Err(tr("an enhancement is already running").into());
    }
    // the job takes minutes and the web build has no worker thread to give it (nor a models folder)
    if cfg!(target_arch = "wasm32") {
        return Err(tr("Super Resolution and AI Sharpen run in the desktop app").into());
    }
    let id = match app.session.selected().as_slice() {
        [id] => *id,
        [] => return Err(tr("select a photo").into()),
        _ => return Err(tr("select a single photo").into()),
    };
    let job = app.session.plan_enhance(id, factor).map_err(|e| e.to_string())?;
    let progress = Arc::new(Mutex::new((0.0, "Rendering".to_string())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let (j, p, c) = (job.clone(), progress.clone(), cancel.clone());
    let work = move || {
        let run = || {
            j.run(&|f, stage| {
                if let Ok(mut g) = p.lock() {
                    *g = (f, stage.to_string());
                }
                !c.load(Ordering::Relaxed)
            })
        };
        // a panicking job reports an error instead of leaving its row up forever
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| Err("the enhancement failed unexpectedly".into()));
        let _ = tx.send(r);
    };
    std::thread::Builder::new().name("enhance".into()).spawn(work).map_err(|e| e.to_string())?;
    let guard = app.session.activity.start("enhance", title(factor), Cancel::Flag(cancel.clone()));
    guard.set_unit(Unit::Percent);
    guard.progress(0, STEPS);
    app.enhance.task = Some(EnhanceTask { job, progress, cancel, rx, _guard: guard });
    Ok(json!({"started": true, "id": id.0, "factor": factor}))
}

/// Finish a job that is done, or move its row's progress on.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(t) = &app.enhance.task else { return };
    let r = match t.rx.try_recv() {
        Ok(r) => r,
        // the worker is gone without an answer: the row must not stay up forever
        Err(TryRecvError::Disconnected) => Err(tr("the enhancement stopped without a result").into()),
        Err(TryRecvError::Empty) => {
            let (f, stage) = t.progress.lock().map(|g| g.clone()).unwrap_or_default();
            let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
            t._guard.progress((f * STEPS as f32) as u64, STEPS);
            t._guard.detail(tr(&stage));
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
    };
    let Some(t) = app.enhance.task.take() else { return };
    let what = tr(title(t.job.factor()));
    // a cancel counts until the result is written, also one pressed while the job was encoding
    if t.cancel.load(Ordering::Relaxed) {
        app.toast(ctx, tr_format!("{what} cancelled", what = what));
        return;
    }
    match r.map_err(lightcraft_engine::EngineError::Other).and_then(|out| app.session.finish_enhance(&t.job, out)) {
        Ok(v) => {
            let message = tr_format!("{what} added: {w} × {h}", what = what, w = v["width"], h = v["height"]);
            app.toast(ctx, message);
        }
        Err(e) => {
            app.ui.status = e.to_string();
            app.toast_error(ctx, tr_format!("{what} failed: {e}", what = what, e = e));
        }
    }
}
