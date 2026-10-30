//! Synchronize Folder in the app: the folder is scanned on a worker thread (a network share can
//! take minutes; no frame waits for it) while its dialog is open, and the dialog then says what
//! changed and what to do about it. Synchronize runs `folder.synchronize`, which acts on exactly
//! the scan the dialog showed (see `lightcraft_engine::sync`). Both the scan and the run show a row in the activity
//! stack (issue #345); its ✕ stops them like the dialog's Cancel.
//!
//! The scan need not be waited for: Continue in Background closes the dialog and the scan goes
//! on under its row. When it is done, a notice in the activity stack says so ([`SyncReady`]);
//! a click on it opens the dialog with what was found, whenever the person is ready for it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::sync::{FolderChanges, SyncChoice, SyncCommit, SyncInput, SyncJob, SyncProgress, SyncStep, scan_with};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{Dialog, SyncCounts};
use crate::theme::Tokens;

/// A running scan of the dialog's folder.
pub struct SyncTask {
    path: String,
    /// What the dialog calls the folder (its row's detail).
    name: String,
    disk: bool,
    /// The dialog was closed with Continue in Background: the scan goes on without it.
    background: bool,
    /// What was ticked in the dialog when it was closed that way (a dialog that had scanned
    /// before and was scanning again keeps its ticks): the dialog opens with them again.
    choice: SyncChoice,
    progress: Arc<SyncProgress>,
    rx: std::sync::mpsc::Receiver<FolderChanges>,
    /// Its row in the activity stack; ✕ cancels the scan and closes the dialog.
    guard: TaskGuard,
}

/// What a scan's row shows: (done, of how many, detail). While the folder is listed, which comes
/// first and on a network share can take minutes, a bar that moves on its own and how many
/// folders and files were listed (a count stuck at "0 of 64085" looks like a scan that stopped);
/// then the files and photos checked.
pub(crate) fn scan_row(progress: &SyncProgress, name: &str) -> (u64, u64, String) {
    match progress.listing() {
        Some((folders, files)) => (0, 0, format!("{} · {name}", listing_text(folders, files))),
        None => {
            let (done, total) = progress.counts();
            (done as u64, total as u64, name.to_string())
        }
    }
}

fn listing_text(folders: usize, files: usize) -> String {
    crate::i18n::tr_format!("Listing folders — folders: {folders}, files: {files}", folders = folders, files = files)
}

/// Open the dialog for folder `path` (called `name` in it) and start scanning. One folder at a
/// time: while another folder is scanned in the background, or what was found in it waits to
/// be looked at, this is refused (minutes of scanning are not thrown away unasked); the same
/// folder is scanned afresh.
pub fn open(app: &mut LightcraftApp, path: &str, name: &str, disk: bool) -> Result<(), String> {
    if let Some(other) = app.sync.as_ref().filter(|t| t.background && t.path != path) {
        return Err(crate::i18n::tr_format!("“{name}” is still being scanned: wait for it, or stop it with ✕ on its row", name = other.name));
    }
    if let Some(other) = app.sync_ready.as_ref().filter(|r| r.path != path) {
        return Err(crate::i18n::tr_format!("Look at the changes found in “{name}” first, or dismiss them", name = other.name));
    }
    // (a folder that can't be synchronized leaves what there is alone)
    let input = SyncInput::new(&mut app.session, path, disk).map_err(|e| e.to_string())?;
    cancel(app);
    let progress = Arc::new(SyncProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let job = move || {
        let _ = tx.send(scan_with(input, &p));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = app.session.activity.start("sync", "Looking for changes", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(name);
    app.sync = Some(SyncTask {
        path: path.to_string(),
        name: name.to_string(),
        disk,
        background: false,
        choice: SyncChoice::default(),
        progress,
        rx,
        guard,
    });
    let d = SyncChoice::default();
    app.ui.dialog = Some(Dialog::SynchronizeFolder {
        path: path.to_string(),
        name: name.to_string(),
        disk,
        counts: None,
        import_new: d.import_new,
        relink_moved: d.relink_moved,
        remove_missing: d.remove_missing,
        read_metadata: d.read_metadata,
    });
    Ok(())
}

/// A scan that finished in the background, waiting to be looked at: the notice in the activity
/// stack. Its changes are `session.folder_changes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReady {
    pub path: String,
    /// What the dialog calls the folder.
    pub name: String,
    pub disk: bool,
    pub counts: SyncCounts,
    /// What was ticked when the dialog was closed.
    pub choice: SyncChoice,
}

impl SyncReady {
    /// How many changes there are to look at.
    pub fn changes(&self) -> usize {
        let c = &self.counts;
        c.new + c.moved + c.missing + c.metadata
    }
}

/// A click on the notice: open the dialog with what the background scan found.
pub fn review(app: &mut LightcraftApp) {
    // (a dialog that is open is not taken away; the notice waits)
    if app.ui.dialog.is_some() {
        return;
    }
    let Some(ready) = app.sync_ready.take() else { return };
    let d = ready.choice;
    app.ui.dialog = Some(Dialog::SynchronizeFolder {
        path: ready.path,
        name: ready.name,
        disk: ready.disk,
        counts: Some(ready.counts),
        import_new: d.import_new,
        relink_moved: d.relink_moved,
        remove_missing: d.remove_missing,
        read_metadata: d.read_metadata,
    });
}

/// ✕ on the notice: what the scan found is let go of.
pub fn dismiss(app: &mut LightcraftApp) {
    if app.sync_ready.is_some() {
        cancel(app);
    }
}

/// Another library was opened: a scan of the one before, running or waiting to be looked at,
/// says nothing about this one.
pub(crate) fn library_changed(app: &mut LightcraftApp) {
    if app.sync.is_some() || app.sync_ready.is_some() || app.sync_owns_changes {
        cancel(app);
        if matches!(app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })) {
            app.ui.dialog = None;
        }
    }
}

/// Stop a running scan and let go of a finished one the dialog made (one an agent made with
/// `folder.scanChanges` is the agent's).
fn cancel(app: &mut LightcraftApp) {
    if let Some(t) = app.sync.take() {
        t.progress.files.cancel.store(true, Ordering::Relaxed);
    }
    app.sync_ready = None;
    if std::mem::take(&mut app.sync_owns_changes) {
        app.session.folder_changes = None;
    }
}

/// Collect a finished scan into the dialog (called every frame). A scan whose dialog was closed
/// is cancelled.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let open_for = match &app.ui.dialog {
        Some(Dialog::SynchronizeFolder { path, .. }) => Some(path.clone()),
        _ => None,
    };
    let background = app.sync.as_ref().is_some_and(|t| t.background);
    if open_for.is_none() && !background {
        // (what a background scan found is kept for its notice)
        if app.sync.is_some() || (app.sync_owns_changes && app.sync_ready.is_none()) {
            cancel(app);
        }
        return;
    }
    let Some(task) = app.sync.as_ref() else { return };
    if !background && open_for.as_deref() != Some(task.path.as_str()) {
        cancel(app);
        return;
    }
    if task.guard.is_cancelled() {
        // ✕ on its row: a scan stopped part-way has nothing to show
        cancel(app);
        if !background {
            app.ui.dialog = None;
        }
        return;
    }
    let (done, total, detail) = scan_row(&task.progress, &task.name);
    task.guard.progress(done, total);
    task.guard.detail(&detail);
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let changes = match task.rx.try_recv() {
        Ok(c) => c,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(_) => {
            app.sync = None;
            if !background {
                app.ui.dialog = None;
            }
            app.toast(ctx, crate::i18n::tr("Scan failed"));
            return;
        }
    };
    let Some(task) = app.sync.take() else { return };
    let c = SyncCounts {
        offline: changes.offline,
        new: changes.new.len(),
        duplicates: changes.duplicates,
        unreadable: changes.unreadable.len(),
        missing: changes.missing.len(),
        metadata: changes.metadata.len(),
        moved: changes.moved.len(),
    };
    if background {
        let ready = SyncReady { path: task.path.clone(), name: task.name.clone(), disk: task.disk, counts: c, choice: task.choice };
        if !ready.counts.offline && ready.changes() == 0 {
            // nothing to look at: said and done
            app.toast(ctx, format!("{}: {}", ready.name, crate::i18n::tr("The library is up to date with this folder.")));
            return;
        }
        app.session.folder_changes = Some(changes);
        app.sync_owns_changes = true;
        app.sync_ready = Some(ready);
        return;
    }
    app.session.folder_changes = Some(changes);
    app.sync_owns_changes = true;
    if let Some(Dialog::SynchronizeFolder { counts, .. }) = &mut app.ui.dialog {
        *counts = Some(c);
        // the button was Continue in Background until now: Synchronize waits a moment
        app.sync_scanned_at = ctx.input(|i| i.time);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLE_SECONDS));
    }
}

/// How long Synchronize stays unavailable after the scan ended (s). The dialog's one button
/// reads Continue in Background while the folder is scanned and Synchronize after: a click meant
/// for the first, landing as the scan ends, must not start the second.
pub const SETTLE_SECONDS: f64 = 0.6;

/// Whether the dialog's Synchronize can be pressed yet ([`SETTLE_SECONDS`]).
pub fn settled(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    ctx.input(|i| i.time) - app.sync_scanned_at >= SETTLE_SECONDS
}

/// How far the scan is: the folders and files listed while it lists the folder, else the files
/// and photos checked (done, of how many).
enum ScanStage {
    Listing(usize, usize),
    Checking(usize, usize),
}

fn progress(app: &LightcraftApp) -> Option<ScanStage> {
    app.sync.as_ref().map(|t| match t.progress.listing() {
        Some((folders, files)) => ScanStage::Listing(folders, files),
        None => {
            let (done, total) = t.progress.counts();
            ScanStage::Checking(done, total)
        }
    })
}

/// The dialog's body.
pub fn body(app: &LightcraftApp, ui: &mut egui::Ui, dlg: &mut Dialog) {
    let Dialog::SynchronizeFolder { path, name, counts, import_new, relink_moved, remove_missing, read_metadata, .. } = dlg else { return };
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(420.0);
    ui.label(crate::i18n::tr("Bring the library up to date with this folder and the folders inside it:"));
    ui.label(egui::RichText::new(name.as_str()).strong());
    ui.label(egui::RichText::new(path.as_str()).color(t.text_dim));
    ui.add_space(6.0);
    let Some(c) = counts.as_ref() else {
        ui.horizontal(|ui| {
            crate::progress::spinner(ui, 14.0);
            match progress(app) {
                Some(ScanStage::Listing(folders, files)) => ui.label(listing_text(folders, files)),
                Some(ScanStage::Checking(done, total)) if total > 0 => ui.label(format!("{} {done} / {total}", crate::i18n::tr("Scanning…"))),
                _ => ui.label(crate::i18n::tr("Scanning…")),
            }
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr("You can go on working: a notice at the top left says when the scan is done.")).color(t.text_dim),
        );
        return;
    };
    if c.offline {
        ui.label(crate::i18n::tr("This folder isn't there: it was moved or renamed, or its disk isn't connected."));
        return;
    }
    let choice = |ui: &mut egui::Ui, on: &mut bool, n: usize, text: String, widget: &str| {
        let r = ui.add_enabled(n > 0, egui::Checkbox::new(on, text));
        crate::widgets::register(ui, widget, r.rect);
    };
    let count = |text: &str, n: usize| format!("{} ({n})", crate::i18n::tr(text));
    let dim = |ui: &mut egui::Ui, s: &str| ui.label(egui::RichText::new(s).color(t.text_dim));
    let skipped = |ui: &mut egui::Ui| {
        if c.duplicates > 0 {
            dim(ui, &count("Skipped: files already in the library elsewhere", c.duplicates));
        }
        if c.unreadable > 0 {
            dim(ui, &count("Skipped: files that can't be read", c.unreadable));
        }
    };
    if c.up_to_date() {
        // nothing to choose: the answer takes the choices' place, and the one button closes
        // (`only_shows`)
        ui.label(crate::i18n::tr("The library is up to date with this folder."));
        skipped(ui);
        return;
    }
    choice(ui, import_new, c.new, count("Import new photos", c.new), "syncImportNew");
    choice(ui, relink_moved, c.moved, count("Relink photos whose file was renamed or moved", c.moved), "syncRelinkMoved");
    choice(ui, remove_missing, c.missing, count("Remove missing photos from the library", c.missing), "syncRemoveMissing");
    choice(ui, read_metadata, c.metadata, count("Read metadata updates from XMP sidecars", c.metadata), "syncReadMetadata");
    ui.add_space(6.0);
    if c.missing > 0 {
        dim(ui, crate::i18n::tr("Missing photos move to Recently Deleted and can be restored; no file on disk is touched."));
    }
    if c.metadata > 0 {
        dim(ui, crate::i18n::tr("Reading a sidecar replaces the photo's rating, labels, metadata and edits with what it says."));
    }
    skipped(ui);
}

/// Synchronize with the dialog's choices: the scan the dialog showed is handed to a worker
/// thread at once ([`SyncRun`]); nothing is read from disk on the UI thread. A scan that went
/// stale (the folder's photos changed meanwhile) is refused and made again, keeping the choices.
/// A folder that is up to date has nothing to hand over: confirming it only closes the dialog.
pub fn confirm(app: &mut LightcraftApp, dlg: &Dialog) -> Result<Value, String> {
    if matches!(dlg, Dialog::SynchronizeFolder { counts: None, .. }) {
        // Continue in Background: the scan goes on without the dialog, which the caller closes
        return match &mut app.sync {
            Some(task) => {
                task.background = true;
                if let Dialog::SynchronizeFolder { import_new, relink_moved, remove_missing, read_metadata, .. } = dlg {
                    task.choice = SyncChoice {
                        import_new: *import_new,
                        relink_moved: *relink_moved,
                        remove_missing: *remove_missing,
                        read_metadata: *read_metadata,
                    };
                }
                Ok(json!({"background": true}))
            }
            None => Err("the folder is still being scanned".into()),
        };
    }
    let Dialog::SynchronizeFolder { path, name, disk, counts: Some(counts), import_new, relink_moved, remove_missing, read_metadata, .. } = dlg
    else {
        return Err("the folder is still being scanned".into());
    };
    // (before the busy checks: nothing is imported or written, so a running import or
    // synchronize can't clash with it, and the dialog closes either way. Like every result the
    // dialog shows, it is the scan's: a file added since is found by the next Synchronize)
    if counts.up_to_date() {
        return Ok(json!({"upToDate": true}));
    }
    if app.sync_run.is_some() {
        return Err(crate::i18n::tr("A folder is already being synchronized").to_string());
    }
    // (an import readies its files against the library as it was: both could add the same file)
    if app.import.is_some() {
        return Err(crate::i18n::tr("Wait for the import to finish").to_string());
    }
    let choice = SyncChoice { import_new: *import_new, relink_moved: *relink_moved, remove_missing: *remove_missing, read_metadata: *read_metadata };
    app.sync_owns_changes = false;
    let started = match app.session.take_folder_changes(path) {
        Some(changes) => SyncJob::start(&mut app.session, changes, choice).map_err(|e| e.to_string()),
        None => Err(format!("{path} changed since it was scanned: scan again")),
    };
    let (work, commit) = match started {
        Ok(halves) => halves,
        Err(e) => {
            // look again, in the background, keeping what was ticked
            if open(app, path, name, *disk).is_ok()
                && let Some(Dialog::SynchronizeFolder { import_new: i, relink_moved: l, remove_missing: m, read_metadata: x, .. }) =
                    &mut app.ui.dialog
            {
                (*i, *l, *m, *x) = (*import_new, *relink_moved, *remove_missing, *read_metadata);
            }
            return Err(e);
        }
    };
    let progress = Arc::new(SyncProgress::default());
    let crashed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let (p, c) = (progress.clone(), crashed.clone());
    let job = move || {
        // (a panic in the work ends it; what was handed over is still committed, and the end
        // says it failed)
        if lightcraft_engine::guard::catch("synchronize", || work.run(&p, |step| tx.send(step).is_ok())).is_err() {
            c.store(true, Ordering::Relaxed);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = app.session.activity.start("sync", "Synchronizing folder", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(name);
    app.sync_run = Some(SyncRun { name: name.clone(), progress, crashed, rx, commit: Some(commit), stopping: false, not_saved: Vec::new(), guard });
    Ok(json!({"started": true}))
}

/// A synchronize at work: the file-system half on a worker thread, its steps committed here
/// between frames ([`poll_run`]).
pub struct SyncRun {
    name: String,
    progress: Arc<SyncProgress>,
    /// The work ended in a panic (logged).
    crashed: Arc<std::sync::atomic::AtomicBool>,
    rx: std::sync::mpsc::Receiver<SyncStep>,
    /// Steps that went into the catalog but could not be saved (a full disk…), and why.
    not_saved: Vec<String>,
    commit: Option<SyncCommit>,
    /// Cancel was pressed: nothing new is started; what was readied is still committed.
    stopping: bool,
    /// Its row in the activity stack (its ✕ is Cancel).
    guard: TaskGuard,
}

impl SyncRun {
    /// (done, of how many) — for a progress row.
    pub fn counts(&self) -> (usize, usize) {
        self.progress.counts()
    }
    /// Stop after the piece in progress.
    pub fn cancel(&mut self) {
        self.stopping = true;
        self.progress.files.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for SyncRun {
    /// A dropped run (the app closing) stops its worker at the next piece.
    fn drop(&mut self) {
        self.progress.files.cancel.store(true, Ordering::Relaxed);
    }
}

/// Tests: a synchronize that stays running while the returned sender lives, so a drop that is
/// released when another barrier goes meets a running Synchronize Folder deterministically
/// (without waiting for a real folder's walk). The caller clears `sync_run` when done.
#[cfg(test)]
pub(crate) fn stall_run(app: &mut LightcraftApp, name: &str) -> Box<dyn std::any::Any> {
    let progress = Arc::new(SyncProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let guard = app.session.activity.start("sync", "Synchronizing folder", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(name);
    app.sync_run = Some(SyncRun {
        name: name.to_string(),
        progress,
        crashed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        rx,
        commit: None,
        stopping: false,
        not_saved: Vec::new(),
        guard,
    });
    Box::new(tx)
}

/// Commit what the worker readied (called every frame); when it is done, one undo step and a
/// word on what happened.
pub fn poll_run(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(run) = app.sync_run.as_mut() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    if run.guard.is_cancelled() {
        run.stopping = true;
    }
    let (done, total) = run.counts();
    run.guard.progress(done as u64, total as u64);
    let mut steps = Vec::new();
    let finished = loop {
        match run.rx.try_recv() {
            Ok(step) => steps.push(step),
            Err(std::sync::mpsc::TryRecvError::Empty) => break false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
        }
    };
    let Some(mut commit) = run.commit.take() else { return };
    let mut not_saved = Vec::new();
    for step in steps {
        // (a step that went in but couldn't be saved — a full disk — is said at the end)
        if let Err(e) = app.session.execute_fn("folder.synchronize", |s| {
            commit.apply(s, step);
            Ok(Value::Null)
        }) {
            not_saved.push(e.to_string());
        }
    }
    let Some(run) = app.sync_run.as_mut() else { return };
    run.not_saved.extend(not_saved);
    if !finished {
        run.commit = Some(commit);
        return;
    }
    let (name, stopped, crashed, mut not_saved) =
        (run.name.clone(), run.stopping, run.crashed.load(Ordering::Relaxed), std::mem::take(&mut run.not_saved));
    app.sync_run = None;
    let mut report = None;
    if let Err(e) = app.session.execute_fn("folder.synchronize", |s| {
        report = Some(commit.finish(s));
        Ok(Value::Null)
    }) {
        not_saved.push(e.to_string());
    }
    let r = report.unwrap_or_default();
    for (path, e) in &r.failed {
        log::warn!("synchronize {name}: {path}: {e}");
    }
    for e in &not_saved {
        log::warn!("synchronize {name}: not saved: {e}");
    }
    let mut parts: Vec<String> =
        [("Imported", r.imported), ("Relinked", r.relinked), ("Removed", r.removed), ("Metadata read", r.read), ("Failed", r.failed.len())]
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(what, n)| format!("{} {n}", crate::i18n::tr(what)))
            .collect();
    // how it ended comes first: never "up to date" for a run that was stopped or failed
    if crashed {
        parts.insert(0, crate::i18n::tr("Stopped by an error (see the log)").to_string());
    } else if stopped {
        parts.insert(0, crate::i18n::tr("Stopped").to_string());
    }
    if let Some(e) = not_saved.first() {
        parts.push(format!("{}: {e}", crate::i18n::tr("Not saved")));
    }
    let text = if parts.is_empty() { crate::i18n::tr("The library is up to date with this folder.").to_string() } else { parts.join(" · ") };
    if not_saved.is_empty() && !crashed {
        app.toast(ctx, format!("{name}: {text}"));
    } else {
        app.toast_for(ctx, format!("{name}: {text}"), 12.0);
    }
}
