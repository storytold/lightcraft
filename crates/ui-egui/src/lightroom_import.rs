//! Background Lightroom catalog inspection and import.
//!
//! Reading a catalog, probing its originals and staging its archive can take minutes on a
//! removable or network drive.  This task keeps that work off the owner thread, commits prepared
//! data only on the owner thread, then finishes the archive index on a worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::lightroom_job::{LightroomProgress, LightroomStep};
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
    progress: LightroomProgress,
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
        progress: LightroomProgress,
        rx: mpsc::Receiver<Message>,
    ) -> Self {
        let label = match kind {
            Kind::Inspect => "Reading Lightroom catalog",
            Kind::Import => "Importing Lightroom catalog",
        };
        let guard = s.activity.start("lightroom", label, Cancel::Flag(cancel.clone()));
        let task = Self { kind, path, cancel, progress, rx, phase: Phase::Reading, cancelled: false, guard };
        task.refresh();
        task
    }

    /// Move to `phase`; only reading the catalog can stop, adding the photos runs to the end.
    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.guard.set_cancellable(phase == Phase::Reading);
        self.refresh();
    }

    /// Bring the activity row up to date: while reading, the step under way and its own count
    /// (reading the photos takes nearly all of an import, so it is counted, not waited out);
    /// then what is being done.
    fn refresh(&self) {
        let (step, done, total) = self.progress.step();
        let detail = match (self.phase, self.kind, step) {
            (Phase::Reading, Kind::Inspect, _) => "",
            (Phase::Reading, Kind::Import, LightroomStep::Catalog) => crate::i18n::tr("Reading Lightroom catalog…"),
            (Phase::Reading, Kind::Import, LightroomStep::ReadPhotos) => crate::i18n::tr("Reading photos…"),
            (Phase::Reading, Kind::Import, LightroomStep::AddPhotos) => crate::i18n::tr("Adding photos…"),
            (Phase::Committing, ..) => crate::i18n::tr("Adding Lightroom photos…"),
            (Phase::Finalizing, ..) => crate::i18n::tr("Saving Lightroom import index…"),
        };
        if self.phase == Phase::Reading {
            self.guard.progress(done as u64, total as u64);
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
        let (step, done, total) = self.progress.step();
        let step = match step {
            LightroomStep::Catalog => "catalog",
            LightroomStep::ReadPhotos => "readPhotos",
            LightroomStep::AddPhotos => "addPhotos",
        };
        json!({
            "running": true,
            "kind": kind,
            "path": self.path,
            "phase": phase,
            "step": step,
            "done": done,
            "total": total,
            "fraction": self.progress.fraction(),
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

/// Tests: a catalog import that never ends while the returned value lives, so a drop meets it pending.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn stall_import(app: &mut LightcraftApp) -> Box<dyn std::any::Any> {
    let (tx, rx) = mpsc::channel::<Message>();
    let task = LightroomTask::new(
        &app.session,
        Kind::Import,
        PathBuf::from("catalog.lrcat"),
        Arc::new(AtomicBool::new(false)),
        LightroomProgress::default(),
        rx,
    );
    app.lightroom = Some(task);
    Box::new(tx)
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
    progress: LightroomProgress,
    tx: mpsc::Sender<Message>,
    ctx: egui::Context,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("lc-lightroom-inspect".into())
        .spawn(move || {
            let result = lightcraft_engine::guard::catch("Lightroom inspect", || {
                lightcraft_engine::lightroom_catalog::read_with_progress(&path, &cancel, &progress.catalog_total, &progress.catalog_done)
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

/// What `status` and the result window show for an inspected catalog: counts, missing originals,
/// warnings and skipped images.
#[cfg(not(target_arch = "wasm32"))]
fn inspect_report(data: lightcraft_engine::lightroom_catalog::CatalogImport) -> Value {
    json!({
        "photos": data.photos.len(),
        "collections": data.collections.iter().filter(|r| r.get("systemOnly").and_then(Value::as_f64).unwrap_or(0.0) == 0.0).count(),
        "missing": data.photos.iter().filter(|p| !p.original_is_file()).map(|p| &p.path).collect::<Vec<_>>(),
        "warnings": data.warnings,
        "skipped": data.skipped,
    })
}

/// The task is over: keep its result (with the catalog's `path`) for `status` and say how it went.
fn terminal(app: &mut LightcraftApp, ctx: &egui::Context, task: &LightroomTask, mut value: Value) {
    let kind = task.kind;
    if let Some(o) = value.as_object_mut() {
        o.entry("path").or_insert_with(|| json!(task.path));
        if let Some(library) = app.session.library.as_ref().filter(|l| l.on_disk) {
            o.entry("library").or_insert_with(|| json!(library.dir.to_string_lossy()));
        }
    }
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
    if matches!(kind, Kind::Import) && value.get("cancelled") != Some(&Value::Bool(true)) {
        app.lightroom_result = Some(result_dialog(&value, &message, &app.session.catalog));
        show_result(app);
    }
    app.lightroom_last = Some(value);
    app.lightroom = None;
    app.toast_for(ctx, message, 6.0);
}

fn show_result(app: &mut LightcraftApp) {
    if app.ui.dialog.is_none() {
        app.ui.dialog = app.lightroom_result.take();
    }
}

/// The lasting result window for an inspect or import: counts, failures, missing originals, the
/// skipped images (one line, in the UI language) and at most eight warnings. Show in Library goes
/// to the lowest mapped photo that is not in Recently Deleted, where All Photos can show it.
fn result_dialog(value: &Value, summary: &str, catalog: &lightcraft_catalog::Catalog) -> crate::state::Dialog {
    let report = value.get("report").unwrap_or(value);
    let mut details = Vec::new();
    if let Some(photos) = report["photos"].as_u64() {
        details.push(crate::i18n::tr_format!(
            "{photos} catalog records processed; {collections} collections.",
            photos = photos,
            collections = report["collections"].as_u64().unwrap_or(0)
        ));
    }
    if let Some(failures) = report["failed"].as_array() {
        details
            .extend(failures.iter().take(4).map(|failure| format!("{}: {}", failure[0].as_str().unwrap_or(""), failure[1].as_str().unwrap_or(""))));
    }
    let missing = report["missing"].as_array().map_or(0, Vec::len);
    if missing > 0 {
        details.push(crate::i18n::tr_format!(
            "{missing} originals are missing. Their catalog records are available; use Find Missing Photos to relink them.",
            missing = missing
        ));
    }
    let in_trash = report["inRecentlyDeleted"].as_u64().unwrap_or(0);
    if in_trash > 0 {
        details.push(crate::i18n::tr_format!(
            "{in_trash} catalog photos are in this library's Recently Deleted and stay there; restore them from Recently Deleted to see them.",
            in_trash = in_trash
        ));
    }
    // images whose catalog records are incomplete come as data (`skipped`), said here in the UI
    // language; their ids are in the report for the CLI and agents
    let skipped = report["skipped"].as_array().map_or(0, Vec::len);
    if skipped > 0 {
        details.push(crate::i18n::tr_format!(
            "{skipped} catalog images were not imported because their records in the catalog are incomplete.",
            skipped = skipped
        ));
    }
    if let Some(warning) = value.get("indexWarning").and_then(Value::as_str) {
        details.push(warning.to_string());
    }
    if let Some(warnings) = report["warnings"].as_array() {
        // Results can contain one warning per image; keep the dialog bounded.
        details.extend(warnings.iter().take(8).filter_map(Value::as_str).map(str::to_string));
        if warnings.len() > 8 {
            details.push(crate::i18n::tr_format!("{count} more warnings.", count = warnings.len() - 8));
        }
    }
    // a catalog photo can map to a library photo left in Recently Deleted (#2589), which All
    // Photos doesn't show
    let live = |id: &u64| catalog.photo(lightcraft_catalog::PhotoId(*id)).is_some_and(|p| !p.deleted);
    let first = value.get("error").is_none().then(|| report["mapping"].as_object()?.values().filter_map(Value::as_u64).filter(live).min()).flatten();
    crate::state::Dialog::LightroomResult {
        summary: summary.to_string(),
        library: report["library"].as_str().or_else(|| value["library"].as_str()).map(str::to_string),
        details,
        first,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_inspect(app: &mut LightcraftApp, path: PathBuf, ctx: &egui::Context) -> Result<Value, String> {
    busy(app)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = LightroomProgress::default();
    let (tx, rx) = mpsc::channel();
    spawn_inspect(path.clone(), cancel.clone(), progress.clone(), tx, ctx.clone())?;
    app.lightroom_last = None;
    app.lightroom = Some(LightroomTask::new(&app.session, Kind::Inspect, path.clone(), cancel, progress, rx));
    Ok(json!({"running": true, "kind": "inspect", "path": path}))
}

#[cfg(not(target_arch = "wasm32"))]
fn start_import(app: &mut LightcraftApp, path: PathBuf, update_existing: bool, ctx: &egui::Context) -> Result<Value, String> {
    busy(app)?;
    let job = lightcraft_engine::lightroom_job::LightroomJob::new(&mut app.session, path.clone(), update_existing).map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = job.progress();
    let (tx, rx) = mpsc::channel();
    spawn_prepare(job, cancel.clone(), tx, ctx.clone())?;
    app.lightroom_last = None;
    app.lightroom = Some(LightroomTask::new(&app.session, Kind::Import, path.clone(), cancel, progress, rx));
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
    show_result(app);
    let Some(mut task) = app.lightroom.take() else { return };
    let message = match task.rx.try_recv() {
        Ok(m) => m,
        Err(mpsc::TryRecvError::Empty) => {
            task.refresh();
            app.lightroom = Some(task);
            return;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            terminal(app, ctx, &task, json!({"error": "Lightroom worker stopped unexpectedly"}));
            return;
        }
    };
    // The task's cancel flag says whether it was cancelled (the worker only stops for it), not an
    // error's words: a real failure that mentions "cancelled" is still reported as a failure.
    match message {
        Message::Inspected(result) => match result {
            Ok(report) if !task.cancel.load(Ordering::Relaxed) => terminal(app, ctx, &task, report),
            Ok(_) => terminal(app, ctx, &task, json!({"cancelled": true})),
            Err(_) if task.cancel.load(Ordering::Relaxed) => terminal(app, ctx, &task, json!({"cancelled": true})),
            Err(e) => terminal(app, ctx, &task, json!({"error": e})),
        },
        Message::Prepared(result) => {
            let prepared = match result {
                Ok(p) => p,
                Err(_) if task.cancel.load(Ordering::Relaxed) => {
                    terminal(app, ctx, &task, json!({"cancelled": true}));
                    return;
                }
                Err(e) => {
                    terminal(app, ctx, &task, json!({"error": e}));
                    return;
                }
            };
            if task.cancel.load(Ordering::Relaxed) || prepared.is_cancelled() || !prepared.token().matches(&app.session) {
                prepared.rollback();
                terminal(
                    app,
                    ctx,
                    &task,
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
                terminal(app, ctx, &task, json!({"error": commit_error.clone().unwrap_or_else(|| "Lightroom import did not commit".into())}));
                return;
            };
            if let Err(e) = spawn_finalize(finalization, tx, ctx.clone()) {
                let mut result = json!({"report": report, "indexWarning": e});
                if let Some(error) = commit_error {
                    result["error"] = json!(error);
                }
                terminal(app, ctx, &task, result);
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
                Ok(()) if pending.get("error").is_none() => terminal(app, ctx, &task, report),
                Ok(()) => terminal(app, ctx, &task, json!({"report": report, "error": pending["error"].clone()})),
                Err(e) => {
                    let mut value = json!({"report": report, "indexWarning": e});
                    if let Some(error) = pending.get("error") {
                        value["error"] = error.clone();
                    }
                    terminal(app, ctx, &task, value);
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
    fn fake_import(h: &crate::headless::Headless) -> (LightroomTask, mpsc::Sender<Message>, Arc<AtomicBool>, LightroomProgress) {
        let (tx, rx) = mpsc::channel();
        let (cancel, progress) = (Arc::new(AtomicBool::new(false)), LightroomProgress::default());
        let task = LightroomTask::new(&h.app.session, Kind::Import, "fixture.lrcat".into(), cancel.clone(), progress.clone(), rx);
        (task, tx, cancel, progress)
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
        let (task, tx, cancel, progress) = fake_import(&h);
        h.app.lightroom = Some(task);
        progress.catalog_total.store(10, Ordering::Relaxed);
        progress.catalog_done.store(3, Ordering::Relaxed);
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
        let (task, tx, cancel, _) = fake_import(&h);
        h.app.lightroom = Some(task);
        h.step();
        let id = h.app.session.activity.list()[0].id;
        h.app.session.activity.cancel(id).unwrap();
        assert!(cancel.load(Ordering::Relaxed), "the worker sees the stack's ✕");
        tx.send(Message::Prepared(Err("cancelled".into()))).unwrap();
        h.step();
        assert!(h.app.lightroom.is_none());
        assert_eq!(h.app.lightroom_last, Some(serde_json::json!({"cancelled": true, "path": "fixture.lrcat"})));
        assert!(h.app.session.activity.list().is_empty());
    }

    /// Whether the task was cancelled is the task's flag, not an error's words (#1046): a cancel
    /// reads as one whatever the message says, and a failure that mentions "cancelled" is a failure.
    #[test]
    fn lightroom_cancel_is_the_flag_not_the_words() {
        let mut h = demo();
        let (task, tx, cancel, _) = fake_import(&h);
        h.app.lightroom = Some(task);
        cancel.store(true, Ordering::Relaxed);
        tx.send(Message::Prepared(Err("Lightroom-Import abgebrochen".into()))).unwrap();
        h.step();
        assert_eq!(h.app.lightroom_last, Some(serde_json::json!({"cancelled": true, "path": "fixture.lrcat"})));

        for message in
            [Message::Prepared(Err("the drive was ejected: copy cancelled by the system".into())), Message::Inspected(Err("cancelled".into()))]
        {
            let (task, tx, _, _) = fake_import(&h);
            h.app.lightroom = Some(task);
            tx.send(message).unwrap();
            h.step();
            let last = h.app.lightroom_last.clone().unwrap_or_default();
            assert!(last.get("cancelled").is_none() && last["error"].as_str().is_some_and(|e| e.contains("cancelled")), "{last}");
        }
    }

    #[test]
    fn lightroom_result_stays_readable_and_shows_photos_hidden_by_a_filter() {
        let mut h = demo();
        let id = h.app.session.catalog.photos().next().unwrap().id.0;
        h.app.session.source = lightcraft_engine::LibrarySource::Picks;
        h.app.session.filter.text = "no-photo-has-this-name".into();
        h.app.session.filter.only = vec![lightcraft_catalog::PhotoId(u64::MAX)];
        h.app.session.filter.labels = vec![lightcraft_catalog::ColorLabel::Red];
        h.app.session.filter.rule_set = Some(lightcraft_catalog::RuleSet {
            mode: lightcraft_catalog::rules::Match::All,
            rules: vec![lightcraft_catalog::rules::Rule::Field { field: "rating".into(), op: "is".into(), value: json!(5) }],
        });
        let (task, _tx, _, _) = fake_import(&h);
        let value = json!({"photos": 1, "imported": 1, "collections": 0, "missing": ["D:/Photos/a.NEF"], "mapping": {"1": id}, "library": "C:/Pictures/LightCraft Library"});
        terminal(&mut h.app, &h.view.ctx, &task, value);
        assert!(h.app.ui.toast.as_ref().unwrap().0.contains("1 photos added"));
        h.step();
        h.step();
        let text = h.painted_text().join(" ");
        assert!(text.contains("Destination library"), "{text}");
        assert!(text.contains("C:/Pictures/LightCraft Library"), "{text}");
        assert!(text.contains("1 originals are missing"), "{text}");
        assert!(text.contains("Show in Library"), "{text}");
        // The activity and toast can go away; the modal result remains until dismissed.
        h.app.ui.toast = None;
        h.step();
        assert!(h.app.ui.dialog.is_some());
        let dialog = h.app.ui.dialog.clone().unwrap();
        crate::panels::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All);
        assert_eq!(h.app.session.filter, lightcraft_catalog::Filter::default());
        assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(id));
        assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
        assert!(!h.app.session.visible().is_empty());
    }

    /// Show in Library goes to a photo All Photos shows: not a master left in Recently Deleted
    /// whose new virtual copy the import added (#2589), and nowhere when no mapped photo is shown.
    #[test]
    fn show_in_library_skips_photos_in_recently_deleted() {
        use lightcraft_catalog::{Op, Photo, Source};
        let mut s = lightcraft_engine::Session::new();
        let mut add = |deleted: bool| {
            let id = s.catalog.alloc_photo_id();
            let mut p = Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "a.jpg", "JPEG", 2, 2, "now");
            p.deleted = deleted;
            s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id.0
        };
        let (trashed, live) = (add(true), add(false));
        assert!(trashed < live);
        let first = |mapping: Value| {
            let crate::state::Dialog::LightroomResult { first, .. } = result_dialog(&json!({"report": {"mapping": mapping}}), "done", &s.catalog)
            else {
                panic!("not a Lightroom result")
            };
            first
        };
        assert_eq!(first(json!({"1": trashed, "2": live})), Some(live));
        assert_eq!(first(json!({"1": trashed})), None, "only a photo in Recently Deleted: no Show in Library");
        assert_eq!(first(json!({"1": live + 100})), None, "no longer in the library");
    }

    #[test]
    fn lightroom_failure_stays_readable_without_offering_a_success_action() {
        let mut h = demo();
        let (task, _tx, _, _) = fake_import(&h);
        terminal(&mut h.app, &h.view.ctx, &task, json!({"error": "catalog or WAL exceeds 1 GiB import limit"}));
        h.step();
        h.step();
        assert!(h.painted_text().join(" ").contains("exceeds 1 GiB import limit"));
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::LightroomResult { first: None, .. })));
        assert!(!h.app.widgets.iter().any(|(id, _)| id == "button:dialogCancel"));
    }

    #[test]
    fn lightroom_result_waits_for_an_existing_dialog() {
        let mut h = demo();
        h.app.ui.dialog = Some(crate::state::Dialog::About);
        let (task, _tx, _, _) = fake_import(&h);
        terminal(&mut h.app, &h.view.ctx, &task, json!({"error": "catalog could not be read"}));
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::About)));
        assert!(h.app.lightroom_result.is_some());
        h.app.ui.dialog = None;
        tick(&mut h.app, &h.view.ctx);
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::LightroomResult { .. })));
        assert!(h.app.lightroom_result.is_none());
    }

    #[test]
    fn skipped_images_are_one_line() {
        let value = json!({"report": {"photos": 3, "collections": 0, "warnings": ["w1", "w2"], "skipped": [
            {"image": 7, "reason": "missingFile"},
            {"image": 8, "reason": "missingRoot"},
            {"image": 9, "reason": "masterSkipped"},
        ]}});
        crate::i18n::set_language(crate::i18n::Locale::En);
        let crate::state::Dialog::LightroomResult { details: en, .. } = result_dialog(&value, "done", &Default::default()) else {
            panic!("not a Lightroom result")
        };
        assert_eq!(en.len(), 4, "{en:?}");
        assert_eq!(en[1], "3 catalog images were not imported because their records in the catalog are incomplete.");
        assert_eq!(&en[2..], ["w1", "w2"]);
    }

    #[test]
    fn photos_left_in_recently_deleted_are_one_line() {
        let value = json!({"report": {"photos": 3, "collections": 0, "inRecentlyDeleted": 2}});
        crate::i18n::set_language(crate::i18n::Locale::En);
        let crate::state::Dialog::LightroomResult { details, .. } = result_dialog(&value, "done", &Default::default()) else {
            panic!("not a Lightroom result")
        };
        assert_eq!(details.len(), 2, "{details:?}");
        assert!(details[1].starts_with("2 catalog photos are in this library's Recently Deleted"), "{details:?}");
        let value = json!({"report": {"photos": 3, "collections": 0, "inRecentlyDeleted": 0}});
        let crate::state::Dialog::LightroomResult { details, .. } = result_dialog(&value, "done", &Default::default()) else {
            panic!("not a Lightroom result")
        };
        assert_eq!(details.len(), 1, "no line when none are: {details:?}");
    }

    #[test]
    fn skipped_images_do_not_take_the_warning_slots() {
        let warnings: Vec<_> = (0..10).map(|i| format!("w{i}")).collect();
        let skipped: Vec<_> = (0..500).map(|i| json!({"image": i, "reason": "missingFile"})).collect();
        let value = json!({"report": {"warnings": warnings, "skipped": skipped}});
        let crate::state::Dialog::LightroomResult { details, .. } = result_dialog(&value, "done", &Default::default()) else {
            panic!("not a Lightroom result")
        };
        // one line for the skipped images, then the usual eight warnings and the rest counted
        assert_eq!(details.len(), 10, "{details:?}");
        assert!(details[0].starts_with("500 catalog images were not imported"));
        assert_eq!(details[9], "2 more warnings.");
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
            skipped: Vec::new(),
        });
        assert_eq!(report["photos"], 0);
        assert_eq!(report["collections"], 0);
        assert_eq!(report["missing"], serde_json::json!([]));
        assert_eq!(report["warnings"], serde_json::json!(["warning"]));
    }
}
