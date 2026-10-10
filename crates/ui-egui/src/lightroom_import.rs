//! Background Lightroom catalog inspection and import.
//!
//! Reading a catalog, probing its originals and staging its archive can take minutes on a
//! removable or network drive.  This task keeps that work off the owner thread, commits prepared
//! data only on the owner thread, then finishes the archive index on a worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use serde_json::{Value, json};

use crate::LightcraftApp;

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

/// A Lightroom task in flight.  The receiver is polled from the egui owner thread; the activity stack shows it.
pub struct LightroomTask {
    kind: Kind,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    done: Arc<AtomicUsize>,
    rx: mpsc::Receiver<Message>,
    phase: Phase,
    cancelled: bool,
    guard: TaskGuard,
}

impl LightroomTask {
    #[cfg(not(target_arch = "wasm32"))]
    fn new(
        s: &lightcraft_engine::Session,
        kind: Kind,
        path: PathBuf,
        cancel: Arc<AtomicBool>,
        total: Arc<AtomicUsize>,
        done: Arc<AtomicUsize>,
        rx: mpsc::Receiver<Message>,
    ) -> Self {
        let label = match kind {
            Kind::Inspect => "Reading Lightroom catalog",
            Kind::Import => "Importing Lightroom catalog",
        };
        let guard = s.activity.start("lightroom", label, Cancel::Flag(cancel.clone()));
        let task = Self { kind, path, cancel, total, done, rx, phase: Phase::Reading, cancelled: false, guard };
        task.refresh();
        task
    }

    /// Move to `phase`; only reading the catalog can stop, adding the photos runs to the end.
    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.guard.set_cancellable(phase == Phase::Reading);
        self.refresh();
    }

    /// Bring the activity row up to date: the count while reading, then what is being done.
    fn refresh(&self) {
        let detail = match (self.phase, self.kind) {
            (Phase::Reading, Kind::Inspect) => "",
            (Phase::Reading, Kind::Import) => crate::i18n::tr("Reading Lightroom catalog…"),
            (Phase::Committing, _) => crate::i18n::tr("Adding Lightroom photos…"),
            (Phase::Finalizing, _) => crate::i18n::tr("Saving Lightroom import index…"),
        };
        if self.phase == Phase::Reading {
            self.guard.progress(self.done.load(Ordering::Relaxed) as u64, self.total.load(Ordering::Relaxed) as u64);
        } else {
            self.guard.progress(0, 0);
        }
        self.guard.detail(detail);
    }

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
    app.lightroom = Some(LightroomTask::new(&app.session, Kind::Inspect, path.clone(), cancel, total, done, rx));
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
    app.lightroom = Some(LightroomTask::new(&app.session, Kind::Import, path.clone(), cancel, total, done, rx));
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
            task.refresh();
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
            task.enter(Phase::Committing);
            let mut finalization = None;
            let mut report = None;
            let committed = app.session.execute_fn("library.importLightroom", |s| {
                let completion = lightcraft_engine::lightroom_job::commit_prepared(s, *prepared)?;
                report = Some(completion.report.clone());
                finalization = Some(completion.finalization);
                Ok(completion.report)
            });
            let report = report.unwrap_or(Value::Null);
            task.enter(Phase::Finalizing);
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

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::lightroom_catalog::CatalogImport;

    /// A task as `start_import` makes it, fed by a channel the test holds instead of a worker.
    fn fake_import(h: &crate::headless::Headless) -> (LightroomTask, mpsc::Sender<Message>, Arc<AtomicBool>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::channel();
        let (cancel, total, done) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let task = LightroomTask::new(&h.app.session, Kind::Import, "fixture.lrcat".into(), cancel.clone(), total.clone(), done.clone(), rx);
        (task, tx, cancel, total, done)
    }

    fn demo() -> crate::headless::Headless {
        let app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        // no render left running at exit (see tests_activity::demo)
        h.settle(std::time::Duration::from_secs(20));
        h
    }

    #[test]
    fn lightroom_row_is_not_cancellable_while_committing() {
        let mut h = demo();
        let (task, tx, cancel, total, done) = fake_import(&h);
        h.app.lightroom = Some(task);
        total.store(10, Ordering::Relaxed);
        done.store(3, Ordering::Relaxed);
        h.step();
        let rows = h.app.session.activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].label.as_str()), ("lightroom", "Importing Lightroom catalog"));
        assert_eq!((rows[0].done, rows[0].total), (3, 10));
        assert!(rows[0].cancellable, "reading the catalog can stop");
        h.app.lightroom.as_mut().unwrap().enter(Phase::Committing);
        let row = h.app.session.activity.list().remove(0);
        assert!(!row.cancellable, "once photos are being added it runs to the end");
        assert!(h.app.session.activity.cancel(row.id).is_err());
        assert!(!cancel.load(Ordering::Relaxed));
        tx.send(Message::Finalized(Ok(()))).unwrap();
        h.step();
        assert!(h.app.lightroom.is_none());
        assert!(h.app.session.activity.list().is_empty(), "the row goes with the task");
    }

    #[test]
    fn lightroom_cancel_from_the_stack_stops_the_read() {
        let mut h = demo();
        let (task, tx, cancel, _, _) = fake_import(&h);
        h.app.lightroom = Some(task);
        h.step();
        let id = h.app.session.activity.list()[0].id;
        h.app.session.activity.cancel(id).unwrap();
        assert!(cancel.load(Ordering::Relaxed), "the worker sees the stack's ✕");
        tx.send(Message::Prepared(Err("cancelled".into()))).unwrap();
        h.step();
        assert!(h.app.lightroom.is_none());
        assert_eq!(h.app.lightroom_last, Some(serde_json::json!({"cancelled": true})));
        assert!(h.app.session.activity.list().is_empty());
    }

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
