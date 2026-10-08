//! Background Lightroom catalog inspection and import.
//!
//! Reading a catalog, probing its originals and staging its archive can take minutes on a
//! removable or network drive.  This task keeps that work off the owner thread, commits prepared
//! data only on the owner thread, then finishes the archive index on a worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use egui::Align2;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

#[derive(Clone, Copy)]
enum Kind {
    Inspect,
    Import,
}

enum Message {
    Inspected(Result<Value, String>),
    Prepared(Result<Box<lightcraft_engine::lightroom_job::PreparedLightroom>, String>),
    Finalized(Result<(), String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Reading,
    Committing,
    Finalizing,
}

/// A Lightroom task in flight.  The receiver is polled from the egui owner thread.
pub struct LightroomTask {
    kind: Kind,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    done: Arc<AtomicUsize>,
    rx: mpsc::Receiver<Message>,
    phase: Phase,
    cancelled: bool,
}

impl LightroomTask {
    #[cfg(not(target_arch = "wasm32"))]
    fn status(&self) -> Value {
        let kind = match self.kind {
            Kind::Inspect => "inspect",
            Kind::Import => "import",
        };
        let phase = match self.phase {
            Phase::Reading => "reading",
            Phase::Committing => "committing",
            Phase::Finalizing => "finalizing",
        };
        json!({
            "running": true,
            "kind": kind,
            "path": self.path,
            "phase": phase,
            "done": self.done.load(Ordering::Relaxed),
            "total": self.total.load(Ordering::Relaxed),
            "cancelled": self.cancelled || self.cancel.load(Ordering::Relaxed),
        })
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for LightroomTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_arch = "wasm32")]
fn unsupported_wasm() -> Result<Value, String> {
    Err("Lightroom catalog import is unavailable in browser builds; open the catalog in the native LightCraft app".into())
}

/// Whether a Lightroom task currently owns the catalog transition.
pub fn is_running(app: &LightcraftApp) -> bool {
    app.lightroom.is_some()
}

#[cfg(not(target_arch = "wasm32"))]
fn busy(app: &LightcraftApp) -> Result<(), String> {
    if app.lightroom.is_some() {
        return Err("a Lightroom catalog task is already running".into());
    }
    if app.import.is_some() || app.scan.is_some() {
        return Err("an import is running".into());
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_inspect(
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    done: Arc<AtomicUsize>,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("lc-lightroom-inspect".into())
        .spawn(move || {
            let result = lightcraft_engine::guard::catch("Lightroom inspect", || {
                lightcraft_engine::lightroom_catalog::read_with_progress(&path, &cancel, &total, &done)
            })
            .and_then(|r| r.map(inspect_report));
            let _ = tx.send(Message::Inspected(result));
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start Lightroom inspect: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_prepare(
    mut job: lightcraft_engine::lightroom_job::LightroomJob,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("lc-lightroom-import".into())
        .spawn(move || {
            let result =
                lightcraft_engine::guard::catch("Lightroom import", || job.prepare(&cancel)).and_then(|r| r.map(Box::new).map_err(|e| e.to_string()));
            if let Err(mpsc::SendError(Message::Prepared(Ok(prepared)))) = tx.send(Message::Prepared(result)) {
                // The owner dropped the task (for example while closing): staged copies/moves
                // must be put back even though no UI receiver remains to accept the result.
                prepared.rollback();
            }
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start Lightroom import: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_finalize(
    finalization: lightcraft_engine::lightroom_job::LightroomArchiveFinalization,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("lc-lightroom-finalize".into())
        .spawn(move || {
            let result = lightcraft_engine::guard::catch("Lightroom archive finalization", || finalization.finish())
                .and_then(|r| r.map_err(|e| e.to_string()));
            let _ = tx.send(Message::Finalized(result));
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start Lightroom archive finalization: {e}"))
}

#[cfg(target_arch = "wasm32")]
fn spawn_finalize(
    _finalization: lightcraft_engine::lightroom_job::LightroomArchiveFinalization,
    _tx: mpsc::Sender<Message>,
    _ctx: egui::Context,
) -> Result<(), String> {
    unsupported_wasm().map(|_| ())
}

#[cfg(not(target_arch = "wasm32"))]
fn inspect_report(data: lightcraft_engine::lightroom_catalog::CatalogImport) -> Value {
    json!({
        "photos": data.photos.len(),
        "collections": data.collections.iter().filter(|r| r.get("systemOnly").and_then(Value::as_f64).unwrap_or(0.0) == 0.0).count(),
        "missing": data.photos.iter().filter(|p| !std::path::Path::new(&p.path).is_file()).map(|p| &p.path).collect::<Vec<_>>(),
        "warnings": data.warnings,
    })
}

fn terminal(app: &mut LightcraftApp, ctx: &egui::Context, kind: Kind, value: Value) {
    let message = if let Some(e) = value.get("error").and_then(Value::as_str) {
        let warning = value.get("indexWarning").and_then(Value::as_str).map_or(String::new(), |w| format!("; index warning: {w}"));
        if matches!(kind, Kind::Inspect) {
            crate::i18n::tr_format!("Lightroom inspection failed: {e}{warning}", e = e, warning = warning)
        } else {
            crate::i18n::tr_format!("Lightroom import failed: {e}{warning}", e = e, warning = warning)
        }
    } else if value.get("cancelled").and_then(Value::as_bool).unwrap_or(false) {
        crate::i18n::tr(if matches!(kind, Kind::Inspect) { "Lightroom inspection cancelled" } else { "Lightroom import cancelled" }).into()
    } else if matches!(kind, Kind::Inspect) {
        let photos = value["photos"].as_u64().unwrap_or(0);
        let collections = value["collections"].as_u64().unwrap_or(0);
        let missing = value["missing"].as_array().map_or(0, Vec::len);
        crate::i18n::tr_format!(
            "Lightroom catalog inspected: {photos} photos, {collections} collections, {missing} missing",
            photos = photos,
            collections = collections,
            missing = missing
        )
    } else {
        let report = value.get("report").unwrap_or(&value);
        let imported = report["imported"].as_u64().unwrap_or(0);
        let failed = report["failed"].as_array().map_or(0, Vec::len);
        let warning = value.get("indexWarning").and_then(Value::as_str);
        if let Some(warning) = warning {
            crate::i18n::tr_format!(
                "Lightroom import complete: {imported} photos added; index warning: {warning}",
                imported = imported,
                warning = warning
            )
        } else if failed == 0 {
            crate::i18n::tr_format!("Lightroom import complete: {imported} photos added", imported = imported)
        } else {
            crate::i18n::tr_format!("Lightroom import complete: {imported} photos added, {failed} failed", imported = imported, failed = failed)
        }
    };
    app.lightroom_last = Some(value);
    app.lightroom = None;
    app.toast_for(ctx, message, 6.0);
}

#[cfg(not(target_arch = "wasm32"))]
fn start_inspect(app: &mut LightcraftApp, path: PathBuf, ctx: &egui::Context) -> Result<Value, String> {
    busy(app)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    spawn_inspect(path.clone(), cancel.clone(), total.clone(), done.clone(), tx, ctx.clone())?;
    app.lightroom_last = None;
    app.lightroom = Some(LightroomTask { kind: Kind::Inspect, path: path.clone(), cancel, total, done, rx, phase: Phase::Reading, cancelled: false });
    Ok(json!({"running": true, "kind": "inspect", "path": path}))
}

#[cfg(not(target_arch = "wasm32"))]
fn start_import(app: &mut LightcraftApp, path: PathBuf, update_existing: bool, ctx: &egui::Context) -> Result<Value, String> {
    busy(app)?;
    let job = lightcraft_engine::lightroom_job::LightroomJob::new(&mut app.session, path.clone(), update_existing).map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let total = job.total_atomic();
    let done = job.done_atomic();
    let (tx, rx) = mpsc::channel();
    spawn_prepare(job, cancel.clone(), tx, ctx.clone())?;
    app.lightroom_last = None;
    app.lightroom = Some(LightroomTask { kind: Kind::Import, path: path.clone(), cancel, total, done, rx, phase: Phase::Reading, cancelled: false });
    Ok(json!({"running": true, "kind": "import", "path": path}))
}

/// Run Lightroom UI commands before the generic engine dispatcher.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value, ctx: &egui::Context) -> Result<Value, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, id, p, ctx);
        return unsupported_wasm();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = p.get("path").and_then(Value::as_str).map(PathBuf::from);
        let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let status = p.get("status").and_then(Value::as_bool).unwrap_or(false) || path.is_none();
        if status {
            if p.get("cancel").and_then(Value::as_bool).unwrap_or(false)
                && let Some(task) = app.lightroom.as_mut()
            {
                task.cancel();
            }
            if wait && app.lightroom.is_some() {
                return wait_for(app, ctx, std::time::Duration::from_secs(600));
            }
            return Ok(app.lightroom.as_ref().map(LightroomTask::status).or_else(|| app.lightroom_last.clone()).unwrap_or(Value::Null));
        }
        let Some(path) = path else { return Err("missing `path`".into()) };
        let result = if id == "library.inspectLightroom" {
            start_inspect(app, path, ctx)
        } else {
            start_import(app, path, p.get("updateExisting").and_then(Value::as_bool).unwrap_or(false), ctx)
        }?;
        if wait {
            return wait_for(app, ctx, std::time::Duration::from_secs(600));
        }
        Ok(result)
    }
}

/// Poll owner-thread work, commit a prepared import, and finish its archive index.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(mut task) = app.lightroom.take() else { return };
    let message = match task.rx.try_recv() {
        Ok(m) => m,
        Err(mpsc::TryRecvError::Empty) => {
            app.lightroom = Some(task);
            return;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            terminal(app, ctx, task.kind, json!({"error": "Lightroom worker stopped unexpectedly"}));
            return;
        }
    };
    match message {
        Message::Inspected(result) => match result {
            Ok(report) if !task.cancel.load(Ordering::Relaxed) => terminal(app, ctx, task.kind, report),
            Ok(_) => terminal(app, ctx, task.kind, json!({"cancelled": true})),
            Err(e) if task.cancel.load(Ordering::Relaxed) || e.contains("cancelled") => terminal(app, ctx, task.kind, json!({"cancelled": true})),
            Err(e) => terminal(app, ctx, task.kind, json!({"error": e})),
        },
        Message::Prepared(result) => {
            let prepared = match result {
                Ok(p) => p,
                Err(e) if task.cancel.load(Ordering::Relaxed) || e.contains("cancelled") => {
                    terminal(app, ctx, task.kind, json!({"cancelled": true}));
                    return;
                }
                Err(e) => {
                    terminal(app, ctx, task.kind, json!({"error": e}));
                    return;
                }
            };
            if task.cancel.load(Ordering::Relaxed) || prepared.is_cancelled() || !prepared.token().matches(&app.session) {
                prepared.rollback();
                terminal(
                    app,
                    ctx,
                    task.kind,
                    if task.cancel.load(Ordering::Relaxed) {
                        json!({"cancelled": true})
                    } else {
                        json!({"error": "Lightroom import belongs to a different open library"})
                    },
                );
                return;
            }
            task.phase = Phase::Committing;
            let mut finalization = None;
            let mut report = None;
            let committed = app.session.execute_fn("library.importLightroom", |s| {
                let completion = lightcraft_engine::lightroom_job::commit_prepared(s, *prepared)?;
                report = Some(completion.report.clone());
                finalization = Some(completion.finalization);
                Ok(completion.report)
            });
            let report = report.unwrap_or(Value::Null);
            task.phase = Phase::Finalizing;
            let (tx, rx) = mpsc::channel();
            let commit_error = committed.as_ref().err().map(ToString::to_string);
            let Some(finalization) = finalization else {
                terminal(app, ctx, task.kind, json!({"error": commit_error.clone().unwrap_or_else(|| "Lightroom import did not commit".into())}));
                return;
            };
            if let Err(e) = spawn_finalize(finalization, tx, ctx.clone()) {
                let mut result = json!({"report": report, "indexWarning": e});
                if let Some(error) = commit_error {
                    result["error"] = json!(error);
                }
                terminal(app, ctx, task.kind, result);
                return;
            }
            task.rx = rx;
            task.cancelled = false;
            let mut pending = json!({"running": true, "kind": "import", "report": report});
            if let Some(error) = commit_error {
                pending["error"] = json!(error);
            }
            app.lightroom_last = Some(pending);
            app.lightroom = Some(task);
        }
        Message::Finalized(result) => {
            let pending = app.lightroom_last.take().unwrap_or(Value::Null);
            let report = pending.get("report").cloned().unwrap_or(Value::Null);
            match result {
                Ok(()) if pending.get("error").is_none() => terminal(app, ctx, task.kind, report),
                Ok(()) => terminal(app, ctx, task.kind, json!({"report": report, "error": pending["error"].clone()})),
                Err(e) => {
                    let mut value = json!({"report": report, "indexWarning": e});
                    if let Some(error) = pending.get("error") {
                        value["error"] = error.clone();
                    }
                    terminal(app, ctx, task.kind, value);
                }
            }
        }
    }
    ctx.request_repaint();
}

/// Wait for a task in control/headless mode while still applying owner-thread completion.
#[cfg(not(target_arch = "wasm32"))]
pub fn wait_for(app: &mut LightcraftApp, ctx: &egui::Context, timeout: std::time::Duration) -> Result<Value, String> {
    let start = std::time::Instant::now();
    while app.lightroom.is_some() {
        tick(app, ctx);
        if start.elapsed() >= timeout {
            return Err("timed out waiting for Lightroom task".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    Ok(app.lightroom_last.clone().unwrap_or(Value::Null))
}

#[cfg(target_arch = "wasm32")]
pub fn wait_for(_app: &mut LightcraftApp, _ctx: &egui::Context, _timeout: std::time::Duration) -> Result<Value, String> {
    unsupported_wasm()
}

/// The progress window for inspect/import, with cancellation.
pub fn progress(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = app.lightroom.as_ref() else { return };
    let total = task.total.load(Ordering::Relaxed);
    let done = task.done.load(Ordering::Relaxed);
    let title = match task.kind {
        Kind::Inspect => crate::i18n::tr("Inspecting Lightroom Catalog"),
        Kind::Import => crate::i18n::tr("Importing Lightroom Catalog"),
    };
    let text = match task.phase {
        Phase::Reading if total == 0 => crate::i18n::tr("Reading Lightroom catalog…").into(),
        Phase::Reading => crate::i18n::tr_format!("Reading Lightroom catalog… {done} of {total}", done = done, total = total),
        Phase::Committing => crate::i18n::tr("Adding Lightroom photos…").into(),
        Phase::Finalizing => crate::i18n::tr("Saving Lightroom import index…").into(),
    };
    let mut cancel = false;
    let t = Tokens::get(ctx);
    egui::Window::new(title).title_bar(false).resizable(false).anchor(Align2::CENTER_BOTTOM, [0.0, -80.0]).fixed_size([360.0, 92.0]).show(
        ctx,
        |ui| {
            ui.label(egui::RichText::new(text).color(t.text));
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_width(340.0));
            let r =
                ui.add_enabled(task.phase == Phase::Reading && !task.cancel.load(Ordering::Relaxed), egui::Button::new(crate::i18n::tr("Cancel")));
            register(ui.ctx(), "button:lightroomCancel", r.rect);
            cancel = r.clicked();
        },
    );
    if cancel {
        if let Some(task) = app.lightroom.as_mut() {
            task.cancel();
        }
        ctx.request_repaint();
    }
    if app.lightroom.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::inspect_report;
    use lightcraft_engine::lightroom_catalog::CatalogImport;

    #[test]
    fn inspect_report_keeps_empty_catalog_shape() {
        let report = inspect_report(CatalogImport {
            source: "synthetic.lrcat".into(),
            photos: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: vec!["warning".into()],
        });
        assert_eq!(report["photos"], 0);
        assert_eq!(report["collections"], 0);
        assert_eq!(report["missing"], serde_json::json!([]));
        assert_eq!(report["warnings"], serde_json::json!(["warning"]));
    }
}
