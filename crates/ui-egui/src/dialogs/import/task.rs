use super::scan::{busy_synchronizing, running};
use super::{ImportDialog, ImportUprightMode, ImportWarning, ImportWarningCode, LightcraftApp, Ordering, Value, json};

/// Files per batch in the browser build (each batch joins the catalog as it is ready, so the
/// progress window updates); the desktop's worker uses [`lightcraft_engine::import::batch_size`].
#[cfg(target_arch = "wasm32")]
const BATCH: usize = 8;

/// The worker's loop: ready `files` batch by batch with `prepare` and send each to `tx`. `ended` is set when the loop
/// ends by itself (every file, a stop, or an import that was dropped); a panic in `prepare` (logged) stops the loop
/// and leaves it unset, so the receiving side can tell a worker that died from one that finished.
#[cfg(not(target_arch = "wasm32"))]
fn feed_batches(
    files: Vec<String>,
    batch: usize,
    cancel: &std::sync::atomic::AtomicBool,
    tx: &std::sync::mpsc::Sender<lightcraft_engine::import::Prepared>,
    ctx: &egui::Context,
    ended: &std::sync::atomic::AtomicBool,
    mut prepare: impl FnMut(Vec<String>) -> lightcraft_engine::import::Prepared,
) {
    for chunk in files.chunks(batch.max(1)) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let prepared = match lightcraft_engine::guard::catch("import", || prepare(chunk.to_vec())) {
            Ok(p) => p,
            Err(_) => {
                // logged; the files readied so far are added, and the import ends as one that died
                ctx.request_repaint();
                return;
            }
        };
        if let Err(unsent) = tx.send(prepared) {
            // the import was dropped: nothing will add these, so take back what a Move placed
            unsent.0.rollback();
            break;
        }
        ctx.request_repaint();
    }
    ended.store(true, Ordering::Release);
    ctx.request_repaint();
}

/// A running import (see the module docs). The file-system work — expanding folders, probing,
/// reading sidecars, copying or placing files — runs on a worker thread
/// ([`lightcraft_engine::import::ImportJob`]); each batch it readies is added to the catalog on the
/// UI thread between frames, so a slow drive never stalls the window.
#[derive(Debug, Default)]
pub struct ImportTask {
    /// Files and folders to import (handed to the worker when the task starts).
    queue: Vec<String>,
    /// The files and folders the import was asked for: what the names of unreadable files are relative to.
    roots: Vec<String>,
    pub total: usize,
    pub done: usize,
    params: Value,
    pub imported: usize,
    /// Brought back from Recently Deleted (`onDeleted: restore`).
    pub restored: usize,
    pub duplicates: usize,
    /// The library photos the skipped duplicates match (shown when nothing new was added).
    existing: Vec<u64>,
    pub failed: usize,
    /// The files that could not be read, as (path, reason) the engine reported them: the first
    /// [`lightcraft_engine::activity::MAX_FAILURES`] (`failed` counts them all).
    failures: Vec<(String, String)>,
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
    /// The worker that readies the files died (a panic, logged) before it got to the last one.
    died: bool,
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
}

impl ImportTask {
    /// An import of `paths` (files or folders) with `library.import` params (without `paths`).
    pub fn new(paths: Vec<String>, params: Value, undo0: usize, browse: bool) -> Self {
        ImportTask { total: paths.len(), queue: paths, params, undo0, browse, ..Default::default() }
    }

    /// Name the files this import cannot read by where they are below `folders` (the sources the files were
    /// found in, or the files and folders it was given), rather than by their file name alone.
    pub fn named_below(mut self, folders: Vec<String>) -> Self {
        self.roots = folders;
        self
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
    /// The batches the worker readies, and whether it came to its end by itself (a stop, a dropped import or the
    /// last file) rather than by a panic: a channel that closes without it was a worker that died.
    #[cfg(not(target_arch = "wasm32"))]
    Thread(std::sync::mpsc::Receiver<lightcraft_engine::import::Prepared>, std::sync::Arc<std::sync::atomic::AtomicBool>),
    #[cfg(target_arch = "wasm32")]
    Inline { job: Box<lightcraft_engine::import::ImportJob>, queue: Vec<String>, files: Option<std::collections::VecDeque<String>> },
}

impl Runner {
    /// The worker stopped without coming to its end: a panic (logged), so the files it did not get to were not
    /// read. Only meaningful once [`ImportRun::next_batches`] has said it is done.
    fn died(&self) -> bool {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Runner::Thread(_, ended) => !ended.load(Ordering::Acquire),
            #[cfg(target_arch = "wasm32")]
            Runner::Inline { .. } => false,
        }
    }
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
    }
}

impl ImportRun {
    fn start(
        job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        album: Option<u64>,
        album_name: Option<String>,
        ctx: &egui::Context,
    ) -> Result<Self, String> {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (total, done) = (job.total.clone(), job.done.clone());
        let (opts, now) = (job.opts.clone(), job.now().to_string());
        let runner = Self::spawn(job, queue, cancel.clone(), ctx)?;
        Ok(ImportRun { runner, cancel, total, done, opts, now, album, album_name })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(
        mut job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ctx: &egui::Context,
    ) -> Result<Runner, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_ended = ended.clone();
        let ctx = ctx.clone();
        let work = move || {
            let files = job.expand(&queue);
            feed_batches(files, lightcraft_engine::import::batch_size(), &cancel, &tx, &ctx, &worker_ended, |chunk| {
                job.prepare_files(chunk, &cancel)
            });
        };
        std::thread::Builder::new()
            .name("lc-import".into())
            .spawn(work)
            .map(|_| Runner::Thread(rx, ended))
            .map_err(|e| format!("could not start: {e}"))
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(
        job: lightcraft_engine::import::ImportJob,
        queue: Vec<String>,
        _cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        _ctx: &egui::Context,
    ) -> Result<Runner, String> {
        Ok(Runner::Inline { job: Box::new(job), queue, files: None })
    }

    /// Readied batches (at most a few per frame). `None` once the worker is done.
    fn next_batches(&mut self) -> Option<Vec<lightcraft_engine::import::Prepared>> {
        match &mut self.runner {
            #[cfg(not(target_arch = "wasm32"))]
            Runner::Thread(rx, _) => {
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
    app.import = Some(ImportTask::new(queue, params, undo0, false).named_below(d.sources.clone()));
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
    app.import = Some(ImportTask::new(paths.clone(), json!({"mode": "add"}), undo0, false).named_below(paths));
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
            .and_then(|(job, album, name)| ImportRun::start(job, std::mem::take(&mut task.queue), album, name, ctx));
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
    if finished {
        task.died = run.runner.died();
    }
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
            let room = lightcraft_engine::activity::MAX_FAILURES.saturating_sub(task.failures.len());
            task.failures.extend(
                v["failed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(room)
                    .filter_map(|f| Some((f.get(0)?.as_str()?.to_string(), f.get(1)?.as_str()?.to_string()))),
            );
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
fn finish(app: &mut LightcraftApp, ctx: &egui::Context, mut task: ImportTask) {
    let steps = app.session.undo.len().saturating_sub(task.undo0);
    app.session.merge_undo(steps, &undo_label(&task));
    if task.auto || task.browse {
        quiet_finish(app, ctx, &mut task);
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
    // (still reported to the activity stack below: the toast is the only part this takes the place of)
    let existing_shown = task.imported == 0
        && task.restored == 0
        && task.failed == 0
        && task.warnings.is_empty()
        && !task.cancelled
        && show_existing(app, ctx, &task.existing);
    let (mut msg, summary, detail) = import_summary(app, &task);
    report(&mut task, summary, detail);
    if existing_shown {
        return;
    }
    if task.warnings.is_empty() {
        app.toast(ctx, msg);
    } else {
        msg.push_str("\n\n");
        msg.push_str(&xmp_warnings_text(&task.warnings));
        app.toast_for(ctx, msg, 8.0);
    }
}

/// The end of an automatic import or of one that only browses: a toast only when there is something to warn about.
fn quiet_finish(app: &mut LightcraftApp, ctx: &egui::Context, task: &mut ImportTask) {
    // (a browse has no summary: its row is worded from its name, "Reading folder", and says how many files could not be read)
    let summary = if task.auto {
        crate::i18n::tr_format!("Auto Import: added {} photo{}", task.imported, crate::i18n::plural_s(task.imported))
    } else {
        String::new()
    };
    let detail = with_warnings(not_readable_text(task.failed), task);
    report(task, summary, detail);
    if !task.warnings.is_empty() {
        app.toast_for(ctx, xmp_warnings_text(&task.warnings), 8.0);
    } else if task.auto && task.imported > 0 {
        app.toast(ctx, crate::i18n::tr_format!("Auto Import: added {} photo{}", task.imported, crate::i18n::plural_s(task.imported)));
    } else if !task.auto && task.failed > 0 {
        app.toast(ctx, crate::i18n::tr_format!("{} photo{} not readable", task.failed, crate::i18n::plural_s(task.failed)));
    }
}

/// What an import did, in a sentence, with its first part (the row's summary) and the rest (the row's detail).
fn import_summary(app: &LightcraftApp, task: &ImportTask) -> (String, String, String) {
    let mut msg = if task.cancelled {
        crate::i18n::tr_format!("Import cancelled · {} photo{} added", task.imported, crate::i18n::plural_s(task.imported))
    } else if task.imported == 0 && task.restored > 0 {
        crate::i18n::tr_format!("Restored {} photo{} from Recently Deleted", task.restored, crate::i18n::plural_s(task.restored))
    } else {
        crate::i18n::tr_format!("Imported {} photo{}", task.imported, crate::i18n::plural_s(task.imported))
    };
    let summary = msg.clone();
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
    // what was done is the first line of the row the import leaves; the rest is what needs reading under it
    let detail = msg.get(summary.len()..).unwrap_or_default().trim_start_matches(" · ").to_string();
    (msg, summary, with_warnings(detail, task))
}

/// How an import ended: stopped, failed to read every file, with problems, or well.
fn ending_of(task: &ImportTask) -> lightcraft_engine::activity::Ending {
    use lightcraft_engine::activity::Ending;
    if task.cancelled {
        Ending::Stopped
    } else if task.died {
        Ending::Died
    } else if task.failed > 0 && task.imported == 0 && task.restored == 0 {
        Ending::Failed
    } else if task.failed > 0 || !task.warnings.is_empty() {
        Ending::Problems
    } else {
        Ending::Done
    }
}

/// The unreadable files, as the second line of an import's row (nothing when all could be read).
pub(crate) fn not_readable_text(failed: usize) -> String {
    if failed == 0 { String::new() } else { crate::i18n::tr_format!("{} photo{} not readable", failed, crate::i18n::plural_s(failed)) }
}

/// `detail` with how many XMP warnings the import met (the toast that spells them out passes; the row stays).
fn with_warnings(mut detail: String, task: &ImportTask) -> String {
    let n = task.warnings.len();
    if n > 0 {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(&crate::i18n::tr_format!("{} import warning{}", n, crate::i18n::plural_s(n)));
    }
    detail
}

/// How a failed file is named in the import's row: by where it is below the folder it was found in (two subfolders
/// can hold the same name), else by its own name.
pub(crate) fn failure_name(path: &str, roots: &[String]) -> String {
    let p = std::path::Path::new(path);
    roots
        .iter()
        .filter_map(|root| p.strip_prefix(root).ok())
        .find(|rel| !rel.as_os_str().is_empty())
        .or_else(|| p.file_name().map(std::path::Path::new))
        .map_or_else(|| path.to_string(), |rel| rel.to_string_lossy().into_owned())
}

/// The import says how it ended to the activity stack, on every way out of [`finish`] (a task dropped without
/// saying is no row at all): one that ran long, was stopped or could not read every file stays in
/// `activity.finished`, naming the files it could not read and why.
fn report(task: &mut ImportTask, summary: String, detail: String) {
    use lightcraft_engine::activity::{Failure, Outcome};
    let ending = ending_of(task);
    let failures = task.failures.iter().map(|(path, reason)| Failure { name: failure_name(path, &task.roots), reason: reason.clone() }).collect();
    if let Some(guard) = task.guard.take() {
        guard.finish(Outcome { ending, summary, detail, failed: task.failed, failures, ..Default::default() });
    }
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
mod tests {
    use super::*;

    /// The worker's end is told from its death: a channel that closes after every batch was sent is a finished
    /// import, one that closes after a panic in readying a batch is an import that died (and ends as `Died`).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_worker_that_panics_is_told_from_one_that_finished() {
        let ctx = egui::Context::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        for (panics, died) in [(false, false), (true, true)] {
            let (tx, rx) = std::sync::mpsc::channel();
            let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let files: Vec<String> = (0..3).map(|i| format!("f{i}")).collect();
            let mut seen = 0;
            feed_batches(files, 1, &cancel, &tx, &ctx, &ended, |_| {
                seen += 1;
                if panics && seen == 2 {
                    panic!("a bug in readying a batch");
                }
                lightcraft_engine::import::Prepared::default()
            });
            drop(tx);
            let sent = rx.try_iter().count();
            assert_eq!(sent, if panics { 1 } else { 3 }, "batches readied before the end");
            assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)), "the channel closed");
            let runner = Runner::Thread(rx, ended);
            assert_eq!(runner.died(), died, "panics: {panics}");
        }
    }

    #[test]
    fn an_import_whose_worker_died_ends_as_died() {
        use lightcraft_engine::activity::Ending;
        let mut task = ImportTask::new(vec!["a".into()], json!({}), 0, false);
        task.imported = 2;
        assert_eq!(ending_of(&task), Ending::Done);
        task.died = true;
        assert_eq!(ending_of(&task), Ending::Died);
        task.cancelled = true;
        assert_eq!(ending_of(&task), Ending::Stopped, "a stop the person asked for stays a stop");
    }

    /// Every way out of `finish` tells the activity stack how the import ended (a guard dropped without
    /// saying leaves no row): an Auto Import and a folder read that met unreadable files, and an import that
    /// only found duplicates it showed.
    #[test]
    fn every_way_an_import_ends_leaves_its_row_when_it_needs_one() {
        use lightcraft_engine::activity::{Cancel, Ending};
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.session.activity.keep_after(0);
        let ctx = egui::Context::default();
        let started = |h: &mut crate::headless::Headless| h.app.session.activity.start("import", "Importing", Cancel::Yes);
        // an Auto Import that could not read a file, though it was short
        let mut auto = ImportTask::new(vec![], json!({}), 0, false).auto();
        auto.failed = 1;
        auto.imported = 2;
        auto.guard = Some(started(&mut h));
        finish(&mut h.app, &ctx, auto);
        // a folder read for the Local view that could read none
        let mut browse = ImportTask::new(vec![], json!({}), 0, true);
        browse.failed = 3;
        browse.guard = Some(started(&mut h));
        finish(&mut h.app, &ctx, browse);
        // one that found only photos the library has, which it shows (and says so in a toast, as ever)
        let existing: Vec<u64> = h.app.session.catalog.photos().take(2).map(|p| p.id.0).collect();
        let mut quiet = ImportTask::new(vec![], json!({}), 0, false);
        quiet.existing = existing;
        quiet.guard = Some(started(&mut h));
        finish(&mut h.app, &ctx, quiet);
        let rows = h.app.session.activity.finished();
        assert!(h.app.session.activity.list().is_empty());
        let endings: Vec<(Ending, usize)> = rows.iter().rev().map(|r| (r.ending, r.failed)).collect();
        assert_eq!(endings, vec![(Ending::Problems, 1), (Ending::Failed, 3), (Ending::Done, 0)], "{rows:?}");
    }

    /// An import that met XMP warnings says so in its row, not only in the toast that passes.
    #[test]
    fn an_import_with_xmp_warnings_says_so_in_its_row() {
        use lightcraft_engine::activity::Cancel;
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.session.activity.keep_after(0);
        let ctx = egui::Context::default();
        let warning =
            || ImportWarning { path: "a.jpg".into(), code: ImportWarningCode::UnmappedXmp, profile: None, mode: None, fields: vec!["x".into()] };
        for (auto, browse) in [(true, false), (false, true), (false, false)] {
            let mut task = ImportTask::new(vec![], json!({}), 0, browse);
            task.auto = auto;
            task.imported = 1;
            task.warnings = vec![warning()];
            task.guard = Some(h.app.session.activity.start("import", "Importing", Cancel::Yes));
            finish(&mut h.app, &ctx, task);
        }
        let rows = h.app.session.activity.finished();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.iter().all(|r| r.detail.contains("1 import warning")), "{rows:?}");
    }
}
