use super::scan::{busy_synchronizing, running};
use super::{ImportDialog, ImportUprightMode, ImportWarning, ImportWarningCode, LightcraftApp, Ordering, Value, json, recipe_items};

/// Files per batch in the browser build (each batch joins the catalog as it is ready, so the
/// progress window updates); the desktop's worker uses [`lightcraft_engine::import::batch_size`].
#[cfg(target_arch = "wasm32")]
const BATCH: usize = 8;

/// A running import (see the module docs). The file-system work — expanding folders, probing,
/// reading sidecars, copying or placing files — runs on a worker thread
/// ([`lightcraft_engine::import::ImportJob`]); each batch it readies is added to the catalog on the
/// UI thread between frames, so a slow drive never stalls the window.
#[derive(Debug, Default)]
pub struct ImportTask {
    /// Files and folders to import (handed to the worker when the task starts).
    queue: Vec<String>,
    pub total: usize,
    pub done: usize,
    pub(super) params: Value,
    pub imported: usize,
    /// Brought back from Recently Deleted (`onDeleted: restore`).
    pub restored: usize,
    pub duplicates: usize,
    /// The library photos the skipped duplicates match (shown when nothing new was added).
    existing: Vec<u64>,
    pub failed: usize,
    /// Move: originals moved, and sources left in place (reported by the engine with a reason).
    pub moved: usize,
    pub kept: usize,
    /// Structured diagnostics from foreign XMP develop settings.
    pub warnings: Vec<ImportWarning>,
    undo0: usize,
    first: Option<u64>,
    /// Reading a folder for the Local view: the photos stay out of the library, and nothing is
    /// selected or announced as added.
    pub(super) browse: bool,
    /// Cancel was pressed: no further files are started; batches already readied are added.
    pub cancelled: bool,
    /// Auto Import (the watched folder): the selection stays as it is, a short toast when done.
    auto: bool,
    /// Auto Import: the selection to keep.
    keep_selection: Option<lightcraft_engine::Selection>,
    /// The selection and source as the import last left them: a change between frames is the
    /// user choosing another photo or view, which the end of the import doesn't take away.
    left: Option<(lightcraft_engine::Selection, lightcraft_engine::LibrarySource)>,
    /// The user chose another photo or view while the import ran.
    moved_on: bool,
    run: Option<ImportRun>,
    /// The import's row in the activity stack (its ✕ and `activity.cancel` set the run's cancel flag).
    guard: Option<lightcraft_engine::activity::TaskGuard>,
    /// The run id of the import review this import followed, for its timing record.
    pub(super) scan_id: Option<u64>,
    completion: std::sync::Arc<std::sync::Mutex<ImportCompletion>>,
}

/// How a desktop import ended, shared with its worker thread: its one timing record is logged
/// when both the task and the worker are done (or the task was dropped).
#[derive(Debug, Default)]
struct ImportCompletion {
    id: u64,
    started: Option<web_time::Instant>,
    worker_done: bool,
    worker_failed: bool,
    owner_detached: bool,
    emitted: bool,
    had_errors: bool,
    persist_failed: bool,
    batches: usize,
    persist: &'static str,
    counts: Value,
}
impl ImportCompletion {
    /// Log the import's start record.
    fn new(mode: &str) -> Self {
        let (id, started) = lightcraft_engine::import::begin_import_log("desktop", mode);
        Self { id, started: Some(started), worker_done: true, persist: "not_applicable", ..Default::default() }
    }
    /// Take the task's counts for the terminal record.
    fn snapshot(&mut self, task: &ImportTask) {
        self.counts = json!({"done":task.done,"total":task.total,"scanned":task.done,"imported":task.imported,"restored":task.restored,"duplicates":task.duplicates,"failed":task.failed,"moved":task.moved,"kept":task.kept,"scanId":task.scan_id.unwrap_or(0),"ownerDetached":self.owner_detached});
    }
    /// Log the terminal record, once.
    fn emit(&mut self, status: &'static str, cause: &'static str, persist: &'static str) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        if let Some(started) = self.started {
            lightcraft_engine::import::finish_import_log(self.id, started, status, cause, persist, &self.counts);
        }
    }
    /// Log a dropped import's terminal once its worker is done too.
    fn detached_terminal(&mut self) {
        if self.owner_detached && self.worker_done {
            self.emit("cancelled", "owner_detached", "unknown");
        }
    }
}

impl Drop for ImportTask {
    fn drop(&mut self) {
        if let Some(run) = &self.run {
            run.cancel.store(true, Ordering::Relaxed);
        }
        let mut completion = self.completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if completion.emitted || completion.started.is_none() {
            return;
        }
        completion.owner_detached = true;
        completion.snapshot(self);
        if let Some(run) = &self.run {
            completion.counts["done"] = json!(run.done.load(Ordering::Relaxed));
            completion.counts["scanned"] = completion.counts["done"].clone();
            completion.counts["total"] = json!(run.total.load(Ordering::Relaxed));
        }
        log::info!(target: "lightcraft::import", "kind=import run_id={} event=cancel_requested owner_detached=true elapsed_ms={} last_persist={}", completion.id, completion.started.map_or(0, |s| s.elapsed().as_millis()), completion.persist);
        completion.detached_terminal();
    }
}

impl ImportTask {
    /// Whether it has its row in the activity list yet (it gets one on its first tick).
    pub fn listed(&self) -> bool {
        self.guard.is_some()
    }

    /// An import of `paths` (files or folders) with `library.import` params (without `paths`).
    pub fn new(paths: Vec<String>, params: Value, undo0: usize, browse: bool) -> Self {
        let completion = ImportCompletion::new(params.get("mode").and_then(Value::as_str).unwrap_or("add"));
        ImportTask {
            total: paths.len(),
            queue: paths,
            done: 0,
            params,
            imported: 0,
            restored: 0,
            duplicates: 0,
            existing: Vec::new(),
            failed: 0,
            moved: 0,
            kept: 0,
            warnings: Vec::new(),
            undo0,
            first: None,
            browse,
            cancelled: false,
            auto: false,
            keep_selection: None,
            left: None,
            moved_on: false,
            run: None,
            guard: None,
            scan_id: None,
            completion: std::sync::Arc::new(std::sync::Mutex::new(completion)),
        }
    }

    /// An Auto Import (see [`ImportTask::auto`]).
    pub fn auto(mut self) -> Self {
        self.auto = true;
        self
    }

    /// `{done, total, imported, cancelled}` for `ui.inspect`.
    pub fn status(&self) -> Value {
        json!({"done": self.done, "total": self.total, "imported": self.imported, "cancelled": self.cancelled})
    }
}

/// Where an import's batches are readied: a worker thread, or (browser build: no threads, and no
/// slow drives) inline, one batch per frame.
enum Runner {
    #[cfg(not(target_arch = "wasm32"))]
    Thread(std::sync::mpsc::Receiver<lightcraft_engine::import::Prepared>),
    #[cfg(target_arch = "wasm32")]
    Inline { job: Box<lightcraft_engine::import::ImportJob>, queue: Vec<String>, files: Option<std::collections::VecDeque<String>> },
}

struct ImportRun {
    runner: Runner,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    opts: lightcraft_engine::import::ImportOptions,
    now: String,
    album: Option<u64>,
    album_name: Option<String>,
    completion: std::sync::Arc<std::sync::Mutex<ImportCompletion>>,
}

impl std::fmt::Debug for ImportRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRun").field("cancel", &self.cancel.load(Ordering::Relaxed)).finish()
    }
}

impl Drop for ImportRun {
    /// A dropped import (replaced, or the app closing) stops its worker at the next file.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let mut completion = self.completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(target_arch = "wasm32")]
        {
            completion.worker_done = true;
        }
        completion.detached_terminal();
    }
}

impl ImportRun {
    fn start(
        job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        album: Option<u64>,
        album_name: Option<String>,
        ctx: &egui::Context,
        completion: std::sync::Arc<std::sync::Mutex<ImportCompletion>>,
    ) -> Result<Self, String> {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (total, done) = (job.total.clone(), job.done.clone());
        let (opts, now) = (job.opts.clone(), job.now().to_string());
        completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner).worker_done = false;
        let runner = Self::spawn(job, queue, cancel.clone(), ctx, completion.clone())?;
        Ok(ImportRun { runner, cancel, total, done, opts, now, album, album_name, completion })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(
        mut job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ctx: &egui::Context,
        completion: std::sync::Arc<std::sync::Mutex<ImportCompletion>>,
    ) -> Result<Runner, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let worker_done = job.done.clone();
        let worker_total = job.total.clone();
        let work = move || {
            let failed = lightcraft_engine::guard::catch("import", || {
                let files = job.expand(&queue);
                for chunk in files.chunks(lightcraft_engine::import::batch_size()) {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let prepared = job.prepare_files(chunk.to_vec(), &cancel);
                    if let Err(unsent) = tx.send(prepared) {
                        // the import was dropped: nothing will add these, so take back what a Move placed
                        unsent.0.rollback();
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .is_err();
            ctx.request_repaint();
            let mut completion = completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            completion.worker_failed |= failed;
            completion.worker_done = true;
            if completion.owner_detached {
                completion.counts["done"] = json!(worker_done.load(Ordering::Relaxed));
                completion.counts["scanned"] = completion.counts["done"].clone();
                completion.counts["total"] = json!(worker_total.load(Ordering::Relaxed));
            }
            completion.detached_terminal();
        };
        std::thread::Builder::new().name("lc-import".into()).spawn(work).map(|_| Runner::Thread(rx)).map_err(|e| format!("could not start: {e}"))
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(
        job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        _cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        _ctx: &egui::Context,
        _completion: std::sync::Arc<std::sync::Mutex<ImportCompletion>>,
    ) -> Result<Runner, String> {
        Ok(Runner::Inline { job: Box::new(job), queue, files: None })
    }

    /// Readied batches (at most a few per frame). `None` once the worker is done.
    fn next_batches(&mut self) -> Option<Vec<lightcraft_engine::import::Prepared>> {
        match &mut self.runner {
            #[cfg(not(target_arch = "wasm32"))]
            Runner::Thread(rx) => {
                let mut out = Vec::new();
                while out.len() < 4 {
                    match rx.try_recv() {
                        Ok(p) => out.push(p),
                        Err(std::sync::mpsc::TryRecvError::Empty) => return Some(out),
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return if out.is_empty() { None } else { Some(out) },
                    }
                }
                Some(out)
            }
            #[cfg(target_arch = "wasm32")]
            Runner::Inline { job, queue, files } => {
                if self.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if files.is_none() {
                    *files = Some(job.expand(queue).into());
                    return Some(Vec::new());
                }
                let Some(files) = files.as_mut() else { return None };
                if files.is_empty() {
                    return None;
                }
                let n = files.len().min(BATCH);
                let batch: Vec<String> = files.drain(..n).collect();
                Some(vec![job.prepare_files(batch, &self.cancel)])
            }
        }
    }
}

/// Start importing the dialog's checked files (the dialog's OK / `ui.dialog.confirm`). Refused while another
/// import runs: replacing `app.import` would stop its worker.
pub fn start(app: &mut LightcraftApp, d: &ImportDialog) -> Result<Value, String> {
    busy_synchronizing(app)?;
    if !d.in_library(app) {
        return Err("another library was opened since the photos were scanned: nothing was imported".into());
    }
    // a browse scan that finishes starts its own import, replacing this one
    if app.import.is_some() || app.scan.as_ref().is_some_and(|t| t.browse) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    let queue = d.selected_paths();
    if queue.is_empty() {
        return Err("no photos selected".into());
    }
    let undo0 = app.session.undo.len();
    let mut album = d.album;
    if album.is_none() && !d.new_album.trim().is_empty() {
        let r = app.session.execute("album.create", &json!({"name": d.new_album.trim()})).map_err(|e| e.to_string())?;
        album = r["id"].as_u64();
    }
    let mode = match (d.copy, d.move_files) {
        (false, _) => "add",
        (true, false) => "copy",
        (true, true) => "move",
    };
    let mut params = json!({"mode": mode, "keywords": d.keywords()});
    if !d.recipe.is_empty() {
        params["recipe"] = recipe_items(&d.recipe);
    }
    if !d.on_deleted.is_empty() {
        params["onDeleted"] = json!(d.on_deleted);
    }
    if let Some(a) = album {
        params["album"] = json!(a);
    }
    if !d.preset.is_empty() {
        params["preset"] = json!(d.preset);
    }
    if !d.metadata_preset.is_empty() {
        params["metadataPreset"] = json!(d.metadata_preset);
    }
    if d.copy {
        if !d.destination.trim().is_empty() {
            params["destination"] = json!(d.destination.trim());
        }
        if let Some(o) = d.organize_param()? {
            params["organize"] = json!(o);
        }
        if !d.rename.trim().is_empty() {
            params["rename"] = json!(d.rename.trim());
            params["renameStart"] = json!(1);
        }
        if d.dng && !d.move_files {
            params["dng"] = json!(true);
        }
    }
    let total = queue.len();
    let mut task = ImportTask::new(queue, params, undo0, false);
    task.scan_id = d.scan_id;
    app.import = Some(task);
    app.renderer.forget_imports();
    Ok(json!({"importing": total}))
}

/// Start importing `paths` (files or folders) in the background, e.g. dropped on the window.
pub fn start_paths(app: &mut LightcraftApp, paths: Vec<String>) -> Result<Value, String> {
    busy_synchronizing(app)?;
    if running(app) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    let undo0 = app.session.undo.len();
    let total = paths.len();
    app.import = Some(ImportTask::new(paths, json!({"mode": "add"}), undo0, false));
    Ok(json!({"importing": total}))
}

/// Advance the import in progress (called every frame): start its worker, then add the batches it
/// has readied to the catalog.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(mut task) = app.import.take() else { return };
    if task.run.is_none() {
        let mut p = task.params.clone();
        p["paths"] = json!(task.queue);
        let started = lightcraft_engine::cmd::library::import_params(&app.session, &p)
            .and_then(|req| Ok((lightcraft_engine::import::ImportJob::new(&mut app.session, req.opts)?, req.album, req.album_name)))
            .map_err(|e| e.to_string())
            .and_then(|(job, album, name)| ImportRun::start(job, std::mem::take(&mut task.queue), album, name, ctx, task.completion.clone()));
        if task.auto {
            task.keep_selection = Some(app.session.selection.clone());
        }
        match started {
            Ok(run) => {
                let label = if task.browse { "Reading folder" } else { "Importing" };
                task.guard = Some(app.session.activity.start("import", label, lightcraft_engine::activity::Cancel::Flag(run.cancel.clone())));
                task.run = Some(run);
            }
            Err(e) => {
                let mut completion = task.completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                completion.worker_done = true;
                completion.snapshot(&task);
                completion.emit("error", "start", "not_applicable");
                drop(completion);
                log::warn!("import: {e}");
                app.toast(ctx, crate::i18n::tr_format!("Import failed: {e}", e = e));
                return;
            }
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let Some(run) = task.run.as_mut() else { return };
    let batches = run.next_batches();
    let finished = batches.is_none();
    if task.left.as_ref().is_some_and(|(sel, source)| *sel != app.session.selection || *source != app.session.source) {
        task.moved_on = true;
    }
    for prepared in batches.into_iter().flatten() {
        commit_batch(app, &mut task, prepared);
        // arriving photos don't take over the selection
        if let Some(sel) = &task.keep_selection {
            app.session.selection = sel.clone();
        }
        // a Local folder still on screen: its first photo becomes active once there is one (#1447)
        if task.browse && app.session.source == lightcraft_engine::LibrarySource::Folder {
            app.session.keep_active_in_view();
        }
    }
    task.left = Some((app.session.selection.clone(), app.session.source));
    if let Some(run) = &task.run {
        task.total = run.total.load(Ordering::Relaxed).max(task.total);
        task.done = run.done.load(Ordering::Relaxed);
    }
    if let Some(guard) = &task.guard {
        guard.progress(task.done as u64, task.total as u64);
        // ✕ in the stack (or `activity.cancel`): no further files are started
        if guard.is_cancelled() {
            task.cancelled = true;
        }
    }
    if !finished {
        app.import = Some(task);
        return;
    }
    finish(app, ctx, task);
}

/// Add one readied batch to the catalog (an undo step merged into the import's at the end).
fn commit_batch(app: &mut LightcraftApp, task: &mut ImportTask, prepared: lightcraft_engine::import::Prepared) {
    let Some(run) = task.run.as_ref() else { return };
    let n = prepared.len();
    let (opts, now, album, album_name) = (&run.opts, run.now.as_str(), run.album, run.album_name.clone());
    let r = app.session.execute_fn("library.import", |s| {
        let report = lightcraft_engine::import::commit_prepared(s, opts, now, prepared)?;
        lightcraft_engine::cmd::library::import_batch_done(s, report, album, album_name.as_deref())
    });
    {
        let mut completion = task.completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        completion.batches = completion.batches.saturating_add(1);
        completion.had_errors |= r.is_err();
        completion.persist_failed |= matches!(&r, Err(lightcraft_engine::EngineError::NotSaved(_))) || app.session.unsaved().is_some();
        completion.persist = lightcraft_engine::import::import_persist_outcome(&app.session);
    }
    match r {
        Ok(v) => {
            // a new album (named in the params) is created with the first photos; later batches join it
            if let (Some(a), Some(run)) = (v["album"].as_u64(), task.run.as_mut()) {
                run.album = Some(a);
            }
            let len = |k: &str| v[k].as_array().map_or(0, Vec::len);
            task.imported += len("imported");
            task.restored += len("restored");
            task.duplicates += len("duplicates");
            task.existing.extend(v["duplicates"].as_array().into_iter().flatten().filter_map(|d| d["existing"].as_u64()));
            task.failed += len("failed");
            task.moved += len("moved");
            task.kept += len("kept");
            task.warnings.extend(v["warnings"].as_array().into_iter().flatten().filter_map(|warning| serde_json::from_value(warning.clone()).ok()));
            for k in v["kept"].as_array().into_iter().flatten() {
                log::warn!("import: kept {}: {}", k["path"].as_str().unwrap_or(""), k["reason"].as_str().unwrap_or(""));
            }
            if task.first.is_none() {
                task.first = ["imported", "restored"].iter().find_map(|k| v[*k].as_array().and_then(|a| a.first()).and_then(Value::as_u64));
            }
        }
        Err(e) => {
            log::warn!("import: {e}");
            task.failed += n;
        }
    }
}

fn xmp_warning_text(warning: &ImportWarning) -> String {
    let path = &warning.path;
    match warning.code {
        ImportWarningCode::CameraProfile => crate::i18n::tr_format!(
            "XMP profile \"{profile}\" was not applied for {path}; colors may differ from Lightroom.",
            profile = warning.profile.as_deref().unwrap_or_default(),
            path = path,
        ),
        ImportWarningCode::UprightGeometry => match warning.mode.unwrap_or(ImportUprightMode::Unknown) {
            ImportUprightMode::Off => crate::i18n::tr_format!(
                "Lightroom geometry for {path} was not preserved; saved transform is ignored because Upright is Off.",
                path = path,
            ),
            ImportUprightMode::Guided => crate::i18n::tr_format!(
                "Lightroom geometry for {path} was not preserved; Guided Upright saved transform is unsupported.",
                path = path,
            ),
            ImportUprightMode::Auto => crate::i18n::tr_format!(
                "Lightroom geometry was not preserved for {path}; {mode} will be recalculated.",
                path = path,
                mode = crate::i18n::tr("Auto")
            ),
            ImportUprightMode::Level => crate::i18n::tr_format!(
                "Lightroom geometry was not preserved for {path}; {mode} will be recalculated.",
                path = path,
                mode = crate::i18n::tr("Level")
            ),
            ImportUprightMode::Vertical => crate::i18n::tr_format!(
                "Lightroom geometry was not preserved for {path}; {mode} will be recalculated.",
                path = path,
                mode = crate::i18n::tr("Vertical")
            ),
            ImportUprightMode::Full => crate::i18n::tr_format!(
                "Lightroom geometry was not preserved for {path}; {mode} will be recalculated.",
                path = path,
                mode = crate::i18n::tr("Full")
            ),
            ImportUprightMode::Unknown => {
                crate::i18n::tr_format!("Lightroom geometry for {path} was not preserved; saved Upright transform is unsupported.", path = path,)
            }
        },
        ImportWarningCode::UnmappedXmp => {
            crate::i18n::tr_format!("XMP adjustments were not imported for {path}: {fields}.", path = path, fields = warning.fields.join(", "),)
        }
    }
}

fn xmp_warnings_text(warnings: &[ImportWarning]) -> String {
    crate::i18n::tr_format!(
        "Import warnings ({n}):\n{warnings}",
        n = warnings.len(),
        warnings = warnings.iter().map(xmp_warning_text).collect::<Vec<_>>().join("\n"),
    )
}

/// The name of the import's one undo step.
fn undo_label(task: &ImportTask) -> String {
    if task.imported == 0 && task.restored > 0 {
        crate::i18n::tr_format!("Restore {} Photo{}", task.restored, crate::i18n::plural_s(task.restored))
    } else if task.restored > 0 {
        crate::i18n::tr_format!("Add {} Photo{}, restore {}", task.imported, crate::i18n::plural_s(task.imported), task.restored)
    } else {
        crate::i18n::tr_format!("Add {} Photo{}", task.imported, crate::i18n::plural_s(task.imported))
    }
}

/// The import is done (or cancelled): one undo step, show and select the first photo (unless the
/// user chose another meanwhile), say what happened.
fn finish(app: &mut LightcraftApp, ctx: &egui::Context, task: ImportTask) {
    let worker_failed = {
        let mut completion = task.completion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        completion.worker_done = true;
        completion.snapshot(&task);
        let persist = if completion.batches == 0 { "not_applicable" } else { lightcraft_engine::import::import_persist_outcome(&app.session) };
        let status = if task.cancelled {
            "cancelled"
        } else if completion.worker_failed || completion.had_errors || task.failed > 0 || persist == "failed" {
            "error"
        } else {
            "success"
        };
        let cause = if completion.worker_failed {
            "worker"
        } else if completion.persist_failed || persist == "failed" {
            "persist"
        } else if completion.had_errors {
            "commit"
        } else if task.failed > 0 {
            "file"
        } else if task.cancelled {
            "cancelled"
        } else {
            "none"
        };
        completion.emit(status, cause, persist);
        completion.worker_failed
    };
    let steps = app.session.undo.len().saturating_sub(task.undo0);
    app.session.merge_undo(steps, &undo_label(&task));
    if worker_failed {
        let what = if task.auto { crate::i18n::tr("Auto Import") } else { crate::i18n::tr("Import") };
        let msg = crate::i18n::tr_format!(
            "{what} stopped on an internal error · {} photo{} added; see the log",
            task.imported,
            crate::i18n::plural_s(task.imported),
            what = what,
        );
        app.toast_error(ctx, msg);
        return;
    }
    if task.auto || task.browse {
        quiet_finish(app, ctx, &task);
        return;
    }
    if let Some(f) = task.first.filter(|_| !task.moved_on) {
        // a view the new photos aren't in (an album, Picks, a Local folder) would leave the active
        // photo off screen: show All Photos, as Lightroom shows the import's own collection (#1446)
        if !app.session.visible().contains(&lightcraft_catalog::PhotoId(f)) {
            let _ = app.run("library.source", json!({"kind": "all"}));
        }
        let _ = app.run("library.select", json!({"ids": [f]}));
    }
    // nothing new, only files the library already has: show where they are (Recently Deleted is
    // easy to miss, and the side panel that lists it starts collapsed)
    if task.imported == 0
        && task.restored == 0
        && task.failed == 0
        && task.warnings.is_empty()
        && !task.cancelled
        && show_existing(app, ctx, &task.existing)
    {
        return;
    }
    let mut msg = import_summary(app, &task);
    if task.warnings.is_empty() {
        app.toast(ctx, msg);
    } else {
        msg.push_str("\n\n");
        msg.push_str(&xmp_warnings_text(&task.warnings));
        app.toast_for(ctx, msg, 8.0);
    }
}

/// The end of an automatic import or of one that only browses: a toast only when there is something to warn about.
fn quiet_finish(app: &mut LightcraftApp, ctx: &egui::Context, task: &ImportTask) {
    if !task.warnings.is_empty() {
        app.toast_for(ctx, xmp_warnings_text(&task.warnings), 8.0);
    } else if task.auto && task.imported > 0 {
        app.toast(ctx, crate::i18n::tr_format!("Auto Import: added {} photo{}", task.imported, crate::i18n::plural_s(task.imported)));
    } else if !task.auto && task.failed > 0 {
        app.toast(ctx, crate::i18n::tr_format!("{} photo{} not readable", task.failed, crate::i18n::plural_s(task.failed)));
    }
}

/// What an import did, in a sentence.
fn import_summary(app: &LightcraftApp, task: &ImportTask) -> String {
    let mut msg = if task.cancelled {
        crate::i18n::tr_format!("Import cancelled · {} photo{} added", task.imported, crate::i18n::plural_s(task.imported))
    } else if task.imported == 0 && task.restored > 0 {
        crate::i18n::tr_format!("Restored {} photo{} from Recently Deleted", task.restored, crate::i18n::plural_s(task.restored))
    } else {
        crate::i18n::tr_format!("Imported {} photo{}", task.imported, crate::i18n::plural_s(task.imported))
    };
    if task.restored > 0 && (task.imported > 0 || task.cancelled) {
        msg.push_str(&crate::i18n::tr_format!(" · {} restored from Recently Deleted", task.restored));
    }
    if task.params["mode"] == "move" {
        msg.push_str(&crate::i18n::tr_format!(" · {} moved", task.moved));
        if task.kept > 0 {
            msg.push_str(&crate::i18n::tr_format!(" · {} original{} left at the source", task.kept, crate::i18n::plural_s(task.kept)));
        }
    }
    if task.duplicates > 0 {
        msg.push_str(&crate::i18n::tr_format!(" · {} duplicate{} skipped", task.duplicates, crate::i18n::plural_s(task.duplicates)));
        let deleted = deleted_of(app, &task.existing).len();
        if deleted > 0 {
            msg.push_str(&crate::i18n::tr_format!(" ({} in Recently Deleted)", deleted));
        }
    }
    if task.failed > 0 {
        msg.push_str(&crate::i18n::tr_format!(" · {} not readable", task.failed));
    }
    msg
}

/// The photos among `ids` that are in Recently Deleted (each once).
fn deleted_of(app: &LightcraftApp, ids: &[u64]) -> Vec<u64> {
    let mut v: Vec<u64> =
        ids.iter().copied().filter(|&id| app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).is_some_and(|p| p.deleted)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Select the library photos an import skipped as duplicates of, in Recently Deleted (side panel
/// opened, with how to get them back) or in All Photos, and say so. `false` when there are none.
fn show_existing(app: &mut LightcraftApp, ctx: &egui::Context, existing: &[u64]) -> bool {
    let deleted = deleted_of(app, existing);
    let (kind, mut ids) = if deleted.is_empty() { ("all", existing.to_vec()) } else { ("recentlyDeleted", deleted) };
    ids.sort_unstable();
    ids.dedup();
    ids.retain(|&id| app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).is_some());
    if ids.is_empty() || app.run("library.source", json!({"kind": kind})).is_err() {
        return false;
    }
    let _ = app.run("library.select", json!({"ids": ids}));
    app.ui.left_panel = true;
    let n = ids.len();
    let plural = if n == 1 { "" } else { "s" };
    let msg = if kind == "all" {
        crate::i18n::tr_format!("Already in the library: {} photo{} selected in All Photos", n, plural)
    } else {
        crate::i18n::tr_format!(
            "Already in the library, in Recently Deleted: {} photo{} selected · right-click ▸ Restore, or Delete Permanently to import afresh",
            n,
            plural
        )
    };
    app.toast_for(ctx, msg, 6.0);
    true
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
