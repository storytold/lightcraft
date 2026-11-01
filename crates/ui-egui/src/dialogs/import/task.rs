use super::scan::{busy_synchronizing, running};
use super::{ImportDialog, ImportUprightMode, ImportWarning, ImportWarningCode, LightcraftApp, Ordering, Value, json};

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
    /// Photo IDs, import options and prepared batches belong to this library opening.
    library: u64,
    /// Files and folders to import (handed to the worker when the task starts).
    queue: Vec<String>,
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
    /// Move: originals moved, and sources left in place (reported by the engine with a reason).
    pub moved: usize,
    pub kept: usize,
    /// Structured diagnostics from foreign XMP develop settings.
    pub warnings: Vec<ImportWarning>,
    undo0: usize,
    first: Option<u64>,
    /// A native document open shows the result in Detail after the import finishes.
    open_documents: bool,
    /// Reading a folder for the Local view: the photos stay out of the library, and nothing is
    /// selected or announced as added.
    pub(super) browse: bool,
    /// Cancel was pressed. Manual imports finish prepared batches; native Add imports discard them.
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
}

impl ImportTask {
    /// Whether it has its row in the activity list yet (it gets one on its first tick).
    pub fn listed(&self) -> bool {
        self.guard.is_some()
    }

    /// An import of `paths` (files or folders) with `library.import` params (without `paths`).
    pub fn new(paths: Vec<String>, params: Value, undo0: usize, browse: bool, session: &lightcraft_engine::Session) -> Self {
        ImportTask { library: session.library_generation(), total: paths.len(), queue: paths, params, undo0, browse, ..Default::default() }
    }

    /// An Auto Import (see [`ImportTask::auto`]).
    pub fn auto(mut self) -> Self {
        self.auto = true;
        self
    }

    /// Manual imports finish prepared batches before a library switch. A cancelled native Add
    /// worker can be detached immediately, without accepting its late results.
    pub(crate) fn blocks_library_switch(&self) -> bool {
        !(self.open_documents && self.guard.as_ref().is_some_and(|g| g.is_cancelled()))
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
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ctx: &egui::Context,
    ) -> Result<Self, String> {
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
        let ctx = ctx.clone();
        let work = move || {
            let files = job.expand(&queue);
            for chunk in files.chunks(lightcraft_engine::import::batch_size()) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let prepared = match lightcraft_engine::guard::catch("import", || job.prepare_files(chunk.to_vec(), &cancel)) {
                    Ok(p) => p,
                    Err(_) => break, // logged; the files readied so far are added
                };
                if let Err(unsent) = tx.send(prepared) {
                    // the import was dropped: nothing will add these, so take back what a Move placed
                    unsent.0.rollback();
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        };
        std::thread::Builder::new().name("lc-import".into()).spawn(work).map(|_| Runner::Thread(rx)).map_err(|e| format!("could not start: {e}"))
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
    if app.import.is_some() || app.scan.is_some() {
        return Err(crate::i18n::tr("An import is already running. Wait for it to finish, then click Import again.").to_string());
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
    app.import = Some(ImportTask::new(queue, params, undo0, false, &app.session));
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
    app.import = Some(ImportTask::new(paths, json!({"mode": "add"}), undo0, false, &app.session));
    Ok(json!({"importing": total}))
}

/// Open documents from the operating system using the normal add-in-place import. Existing
/// photos are selected by the importer's duplicate handling; new photos use the same defaults,
/// sidecars, errors and undo as window drops. No copy or move is implied by opening a document.
pub fn start_documents(app: &mut LightcraftApp, paths: Vec<String>) -> Result<Value, String> {
    let result = start_paths(app, paths)?;
    if let Some(task) = app.import.as_mut() {
        task.open_documents = true;
        // Register before the first worker tick, so even an import that has not started can stop.
        task.guard = Some(app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes));
    }
    Ok(result)
}

/// Native Add workers only prepare metadata. Dropping their receiver rejects both buffered and
/// late results without waiting for I/O. Copy and Move retain their existing completion rules.
pub(super) fn detach_cancelled_documents(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.import.as_ref().is_some_and(|t| t.open_documents && t.guard.as_ref().is_some_and(|g| g.is_cancelled())) {
        return;
    }
    let Some(mut task) = app.import.take() else { return };
    task.run = None;
    task.cancelled = true;
    if task.library == app.session.library_generation() {
        // Keep earlier commits and merge their undo steps in the original library only.
        finish(app, ctx, task);
    }
}

/// Advance the import in progress (called every frame): start its worker, then add the batches it
/// has readied to the catalog.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    detach_cancelled_documents(app, ctx);
    let Some(mut task) = app.import.take() else { return };
    // Check before both starting work and accepting prepared batches. The UI blocks library
    // switches, but a host replacing its session must not commit old photo IDs into a new one.
    if task.library != app.session.library_generation() {
        app.notices.push(crate::i18n::tr("Import stopped because its library changed. Import the photos again in the intended library.").to_string());
        return;
    }
    if task.run.is_none() {
        let mut p = task.params.clone();
        p["paths"] = json!(task.queue);
        let cancel =
            task.guard.as_ref().and_then(|g| g.cancel_flag()).unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let started = lightcraft_engine::cmd::library::import_params(&app.session, &p)
            .and_then(|req| Ok((lightcraft_engine::import::ImportJob::new(&mut app.session, req.opts)?, req.album, req.album_name)))
            .map_err(|e| e.to_string())
            .and_then(|(job, album, name)| ImportRun::start(job, std::mem::take(&mut task.queue), album, name, cancel, ctx));
        if task.auto {
            task.keep_selection = Some(app.session.selection.clone());
        }
        match started {
            Ok(run) => {
                let label = if task.browse { "Reading folder" } else { "Importing" };
                if task.guard.is_none() {
                    task.guard = Some(app.session.activity.start("import", label, lightcraft_engine::activity::Cancel::Flag(run.cancel.clone())));
                }
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
    let steps = app.session.undo.len().saturating_sub(task.undo0);
    app.session.merge_undo(steps, &undo_label(&task));
    if task.auto || task.browse {
        quiet_finish(app, ctx, &task);
        return;
    }
    if task.open_documents && !task.moved_on && (task.first.is_some() || !task.existing.is_empty()) {
        // An album, Local folder or search must not hide the file just opened by Finder.
        let _ = app.run("library.clearFilter", json!({}));
        let _ = app.run("library.source", json!({"kind": "all"}));
        let _ = app.run("view.detail", json!({}));
        if task.first.is_none() {
            let _ = app.run("library.select", json!({"ids": task.existing}));
        }
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
        && (!task.open_documents || !task.moved_on)
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
mod document_tests {
    use super::*;
    use crate::Services;
    use crate::state::ViewMode;

    #[test]
    fn document_paths_use_the_import_command_and_add_mode() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        start_documents(&mut app, vec!["/Volumes/NAS/撮影 café #1.ARW".into(), "/tmp/missing %.dng".into()]).unwrap();
        let task = app.import.as_ref().unwrap();
        assert_eq!(task.queue, ["/Volumes/NAS/撮影 café #1.ARW", "/tmp/missing %.dng"]);
        let mut params = task.params.clone();
        params["paths"] = json!(task.queue);
        let request = lightcraft_engine::cmd::library::import_params(&app.session, &params).unwrap();
        assert_eq!(request.opts.mode, lightcraft_engine::import::ImportMode::Add);
        assert!(task.open_documents);
    }

    #[test]
    fn finished_native_opens_keep_the_view_the_user_chose_while_importing() {
        for duplicate in [false, true] {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
            let id = app.session.catalog.photos().next().unwrap().id.0;
            app.run("library.source", json!({"kind": "picks"})).unwrap();
            app.run("library.filter", json!({"text": "the user's search"})).unwrap();
            app.ui.view = ViewMode::PhotoGrid;
            let source = app.session.source;
            let selection = app.session.selection.clone();
            let mut task = ImportTask::new(Vec::new(), json!({"mode": "add"}), app.session.undo.len(), false, &app.session);
            task.open_documents = true;
            task.moved_on = true;
            task.first = (!duplicate).then_some(id);
            task.imported = usize::from(!duplicate);
            if duplicate {
                task.existing.push(id);
            }
            finish(&mut app, &egui::Context::default(), task);
            assert_eq!(app.session.source, source);
            assert_eq!(app.session.selection, selection);
            assert_eq!(app.session.filter.text, "the user's search");
            assert_eq!(app.ui.view, ViewMode::PhotoGrid);
        }
    }

    #[test]
    fn documents_import_select_and_reopen_duplicates_in_detail() {
        let dir = std::env::temp_dir().join(format!("lc-document-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("撮影 café #1.png");
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[20, 90, 30, 255]; 64] };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
        let ctx = egui::Context::default();
        let mut active = None;
        for pass in 0..2 {
            app.run("library.source", json!({"kind": "picks"})).unwrap();
            app.run("library.filter", json!({"text": "not-the-file-name"})).unwrap();
            app.ui.view = ViewMode::PhotoGrid;
            start_documents(&mut app, vec![file.to_str().unwrap().into()]).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while app.import.is_some() && std::time::Instant::now() < deadline {
                tick(&mut app, &ctx);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(app.import.is_none());
            assert_eq!(app.session.catalog.len(), 1);
            assert_eq!(app.ui.view, ViewMode::Detail);
            let photo = app.session.catalog.photo(app.session.active().unwrap()).unwrap();
            assert!(matches!(&photo.source, lightcraft_catalog::Source::File { path } if path == file.to_str().unwrap()));
            assert!(!photo.local);
            if pass == 0 {
                active = app.session.selection.active;
            }
            assert_eq!(app.session.selection.active, active);
            assert_eq!(app.session.visible().len(), 1);
        }
        assert!(file.exists(), "opening must leave the original in place");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
