//! Synchronize Folder in the app: the folder is scanned on a worker thread (a network share can
//! take minutes; no frame waits for it) while its dialog is open, and the dialog then says what
//! changed and what to do about it. Synchronize runs `folder.synchronize`, which acts on exactly
//! the scan the dialog showed (see `lightcraft_engine::sync`). Both the scan and the run show a row in the activity
//! stack (issue #345); its ✕ stops them like the dialog's Cancel.
//!
//! A Synchronize asked for while an import or another synchronize runs waits in line behind it (the library lane,
//! [`lightcraft_engine::activity::Lane::Library`]). When its turn comes it scans the folder again ([`SyncRescan`]) and
//! then runs as it was asked, but it removes or relinks only the photos its dialog showed: one that went missing
//! meanwhile was never shown to the person, so it is left alone, and the end says so.
//!
//! Synchronize N Folders (several Folders rows chosen at once) opens one dialog: the folders are scanned one after
//! another under one row ([`ManyScan`]), each says what changed, and one set of choices goes for all of them. Each
//! folder with something to do then waits in the library lane as a Synchronize of its own, in the sidebar's order.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use lightcraft_engine::activity::{Cancel, Ending, Failure, Outcome, Request, TaskGuard};
use lightcraft_engine::sync::{FolderChanges, SyncChoice, SyncCommit, SyncInput, SyncJob, SyncProgress, SyncStep, scan_with};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{Dialog, SyncCounts, SyncFolder};
use crate::theme::Tokens;

/// A running scan of the dialog's folder.
pub struct SyncTask {
    path: String,
    progress: Arc<SyncProgress>,
    rx: std::sync::mpsc::Receiver<FolderChanges>,
    /// Its row in the activity stack; ✕ cancels the scan and closes the dialog.
    guard: TaskGuard,
}

/// Open the dialog for folder `path` (called `name` in it) and start scanning.
pub fn open(app: &mut LightcraftApp, path: &str, name: &str, disk: bool) -> Result<(), String> {
    cancel(app);
    let input = SyncInput::new(&mut app.session, path, disk).map_err(|e| e.to_string())?;
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
    // (kind `scan`, not `sync`: it only reads, and doesn't hold up an import)
    let guard = app.session.activity.start("scan", "Looking for changes", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(name);
    app.sync = Some(SyncTask { path: path.to_string(), progress, rx, guard });
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

/// Stop a running scan and let go of a finished one the dialog made (one an agent made with
/// `folder.scanChanges` is the agent's).
fn cancel(app: &mut LightcraftApp) {
    if let Some(t) = app.sync.take() {
        t.progress.files.cancel.store(true, Ordering::Relaxed);
    }
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
    let Some(open_for) = open_for else {
        if app.sync.is_some() || app.sync_owns_changes {
            cancel(app);
        }
        return;
    };
    let Some(task) = app.sync.as_ref() else { return };
    if task.path != open_for {
        cancel(app);
        return;
    }
    if task.guard.is_cancelled() {
        // ✕ on its row: a scan stopped part-way has nothing to show
        if let Some(task) = app.sync.take() {
            task.progress.files.cancel.store(true, Ordering::Relaxed);
            task.guard.finish(Outcome::ended(Ending::Stopped));
        }
        cancel(app);
        app.ui.dialog = None;
        return;
    }
    let (done, total) = task.progress.counts();
    task.guard.progress(done as u64, total as u64);
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let changes = match task.rx.try_recv() {
        Ok(c) => c,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(_) => {
            // the worker died (a panic, logged)
            if let Some(task) = app.sync.take() {
                task.guard.finish(Outcome::ended(Ending::Died));
            }
            app.ui.dialog = None;
            app.toast(ctx, crate::i18n::tr("Scan failed"));
            return;
        }
    };
    if let Some(task) = app.sync.take() {
        task.guard.finish(if changes.offline {
            Outcome::failed(crate::i18n::tr("This folder isn't there: it was moved or renamed, or its disk isn't connected."))
        } else {
            Outcome::default()
        });
    }
    let c = SyncCounts {
        offline: changes.offline,
        new: changes.new.len(),
        duplicates: changes.duplicates,
        unreadable: changes.unreadable.len(),
        missing: changes.missing.len(),
        metadata: changes.metadata.len(),
        moved: changes.moved.len(),
    };
    app.session.folder_changes = Some(changes);
    app.sync_owns_changes = true;
    if let Some(Dialog::SynchronizeFolder { counts, .. }) = &mut app.ui.dialog {
        *counts = Some(c);
    }
}

/// How far the scan is (files probed, of how many).
fn progress(app: &LightcraftApp) -> Option<(usize, usize)> {
    app.sync.as_ref().map(|t| t.progress.counts())
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
            ui.spinner();
            match progress(app) {
                Some((done, total)) if total > 0 => ui.label(format!("{} {done} / {total}", crate::i18n::tr("Scanning…"))),
                _ => ui.label(crate::i18n::tr("Scanning…")),
            }
        });
        return;
    };
    if c.offline {
        ui.label(crate::i18n::tr("This folder isn't there: it was moved or renamed, or its disk isn't connected."));
        return;
    }
    let choice = |ui: &mut egui::Ui, on: &mut bool, n: usize, text: String, widget: &str| {
        let r = ui.add_enabled(n > 0, egui::Checkbox::new(on, text));
        crate::widgets::register(ui.ctx(), widget, r.rect);
    };
    let count = |text: &str, n: usize| format!("{} ({n})", crate::i18n::tr(text));
    choice(ui, import_new, c.new, count("Import new photos", c.new), "syncImportNew");
    choice(ui, relink_moved, c.moved, count("Relink photos whose file was renamed or moved", c.moved), "syncRelinkMoved");
    choice(ui, remove_missing, c.missing, count("Remove missing photos from the library", c.missing), "syncRemoveMissing");
    choice(ui, read_metadata, c.metadata, count("Read metadata updates from XMP sidecars", c.metadata), "syncReadMetadata");
    ui.add_space(6.0);
    let dim = |ui: &mut egui::Ui, s: &str| ui.label(egui::RichText::new(s).color(t.text_dim));
    if c.new + c.moved + c.missing + c.metadata == 0 {
        dim(ui, crate::i18n::tr("The library is up to date with this folder."));
    }
    if c.missing > 0 {
        dim(ui, crate::i18n::tr("Missing photos move to Recently Deleted and can be restored; no file on disk is touched."));
    }
    if c.metadata > 0 {
        dim(ui, crate::i18n::tr("Reading a sidecar replaces the photo's rating, labels, metadata and edits with what it says."));
    }
    if c.duplicates > 0 {
        dim(ui, &count("Skipped: files already in the library elsewhere", c.duplicates));
    }
    if c.unreadable > 0 {
        dim(ui, &count("Skipped: files that can't be read", c.unreadable));
    }
}

/// Synchronize with the dialog's choices: the scan the dialog showed is handed to a worker
/// thread at once ([`SyncRun`]); nothing is read from disk on the UI thread. A scan that went
/// stale (the folder's photos changed meanwhile) is refused and made again, keeping the choices.
/// While an import or another synchronize runs it waits in line instead ([`SyncRescan`]).
pub fn confirm(app: &mut LightcraftApp, dlg: &Dialog) -> Result<Value, String> {
    let Dialog::SynchronizeFolder { path, name, disk, counts: Some(_), import_new, relink_moved, remove_missing, read_metadata, .. } = dlg else {
        return Err("the folder is still being scanned".into());
    };
    let choice = SyncChoice { import_new: *import_new, relink_moved: *relink_moved, remove_missing: *remove_missing, read_metadata: *read_metadata };
    // what the dialog showed: only these are removed or relinked, whenever the run comes
    let shown =
        app.session.folder_changes.as_ref().filter(|c| lightcraft_catalog::query::folder_key(&c.path) == lightcraft_catalog::query::folder_key(path));
    let (missing, moved): (Vec<u64>, Vec<u64>) =
        shown.map(|c| (c.missing.iter().map(|m| m.id).collect(), c.moved.iter().map(|m| m.id).collect())).unwrap_or_default();
    let request = sync_request(path, name, *disk, choice, &missing, &moved);
    // (an import readies its files against the library as it was: both could add the same file)
    if crate::import::library_busy(app) {
        if std::mem::take(&mut app.sync_owns_changes) {
            app.session.folder_changes = None;
        }
        let (id, place) = app.session.activity.enqueue("sync", "Synchronizing folder", request)?;
        app.queued_toast(place as u64);
        return Ok(json!({"queued": id, "place": place}));
    }
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
    let guard = app.session.activity.start("sync", "Synchronizing folder", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(name);
    guard.set_request(request);
    run(app, name, work, commit, progress, guard, 0);
    Ok(json!({"started": true}))
}

// ---- Synchronize N Folders

/// The scans of a Synchronize N Folders dialog: one folder after another, under one row of the activity stack.
pub struct ManyScan {
    guard: TaskGuard,
    /// The folder being scanned (its place in the dialog), how far it is, and where its result comes.
    at: Option<(usize, Arc<SyncProgress>, std::sync::mpsc::Receiver<FolderChanges>)>,
    /// Chosen folders left out because another chosen one takes them in: back in the dialog if that one can't be
    /// synchronized after all.
    covered: Vec<SyncFolder>,
}

impl ManyScan {
    /// Stop the folder being scanned.
    fn stop(&self) {
        if let Some((_, progress, _)) = &self.at {
            progress.files.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// `path` as a list of lower-cased names, to tell whether one folder is inside another.
fn names(path: &str) -> Vec<String> {
    lightcraft_catalog::query::folder_key(path).to_lowercase().split('/').filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// Whether folder `inner` (its [`names`]) is inside folder `outer`.
fn inside(inner: &[String], outer: &[String]) -> bool {
    inner.len() > outer.len() && inner.starts_with(outer)
}

/// The folders Synchronize N Folders goes through, for library folders `paths` as the sidebar chose them, and the
/// ones it leaves out because another chosen one takes them in (synchronizing a folder takes in the folders inside
/// it). `order` is the sidebar's folders, as drawn (depth first): the folders go in that order, each once. A whole
/// disk is listed but left out, saying why: it is synchronized by itself, after its own question.
pub fn many_folders(paths: &[String], order: &[String]) -> (Vec<SyncFolder>, Vec<SyncFolder>) {
    use lightcraft_catalog::query::folder_key;
    let place = |p: &str| {
        let k = folder_key(p);
        order.iter().position(|o| folder_key(o) == k).unwrap_or(order.len())
    };
    let mut chosen: Vec<(usize, Vec<String>, &String)> = paths.iter().map(|p| (place(p), names(p), p)).collect();
    // (a folder the sidebar doesn't show goes last, by name)
    chosen.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    chosen.dedup_by(|a, b| a.1 == b.1);
    let disk = |p: &str| lightcraft_catalog::folders::is_disk_root(p);
    let folder = |p: &str| SyncFolder {
        path: p.to_string(),
        name: lightcraft_catalog::folders::folder_label(p),
        skipped: disk(p).then(|| crate::i18n::tr("A whole disk: synchronize it by itself").to_string()),
        ..Default::default()
    };
    // (a disk left out takes in nothing)
    let outers: Vec<&Vec<String>> = chosen.iter().filter(|(_, _, p)| !disk(p)).map(|(_, n, _)| n).collect();
    let (covered, listed): (Vec<_>, Vec<_>) = chosen.iter().partition(|(_, n, _)| outers.iter().any(|o| inside(n, o)));
    (listed.iter().map(|(_, _, p)| folder(p)).collect(), covered.iter().map(|(_, _, p)| folder(p)).collect())
}

/// The sidebar's library folders as drawn, depth first.
fn sidebar_order(app: &LightcraftApp) -> Vec<String> {
    fn walk(nodes: &[lightcraft_catalog::FolderNode], out: &mut Vec<String>) {
        for n in nodes {
            out.push(n.path.clone());
            walk(&n.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&app.session.catalog.folder_tree(), &mut out);
    out
}

/// Open the Synchronize N Folders dialog for library folders `paths` and start scanning the first of them.
pub fn open_many(app: &mut LightcraftApp, paths: &[String]) -> Result<(), String> {
    let (folders, covered) = many_folders(paths, &sidebar_order(app));
    if folders.is_empty() {
        return Err("no folder to synchronize".into());
    }
    cancel_many(app);
    let guard = app.session.activity.start("scan", "Looking for changes", Cancel::Yes);
    app.sync_scans = Some(ManyScan { guard, at: None, covered });
    let d = SyncChoice::default();
    app.ui.dialog = Some(Dialog::SynchronizeFolders {
        folders,
        import_new: d.import_new,
        relink_moved: d.relink_moved,
        remove_missing: d.remove_missing,
        read_metadata: d.read_metadata,
        library: app.session.library_generation(),
    });
    Ok(())
}

/// Stop the scans of a Synchronize N Folders dialog that closed (Cancel, or another dialog): nothing to report, so
/// its row goes without a trace, as the single folder's does.
fn cancel_many(app: &mut LightcraftApp) {
    if let Some(scan) = app.sync_scans.take() {
        scan.stop();
    }
}

/// Folder `at` of the dialog can't be synchronized (`why`): the chosen folders it took in come back after it, to be
/// scanned themselves (those inside one of them stay taken in by it).
pub(crate) fn skip(folders: &mut Vec<SyncFolder>, covered: &mut Vec<SyncFolder>, at: usize, why: String) {
    let Some(f) = folders.get_mut(at) else { return };
    f.skipped = Some(why);
    let outer = names(&f.path);
    let (back, stay): (Vec<SyncFolder>, Vec<SyncFolder>) = std::mem::take(covered).into_iter().partition(|c| inside(&names(&c.path), &outer));
    *covered = stay;
    let keys: Vec<Vec<String>> = back.iter().map(|c| names(&c.path)).collect();
    let (top, under): (Vec<_>, Vec<_>) = back.into_iter().zip(&keys).partition(|(_, k)| !keys.iter().any(|o| inside(k, o)));
    covered.extend(under.into_iter().map(|(c, _)| c));
    let at = at.saturating_add(1).min(folders.len());
    folders.splice(at..at, top.into_iter().map(|(c, _)| c));
}

/// Collect a finished folder scan into the Synchronize N Folders dialog and start the next (called every frame). Its
/// ✕ stops them and closes the dialog; so does opening another library (the folders and photos found were the
/// other library's).
pub fn poll_many(app: &mut LightcraftApp, ctx: &egui::Context) {
    let library = match &app.ui.dialog {
        Some(Dialog::SynchronizeFolders { library, .. }) => *library,
        _ => {
            cancel_many(app);
            return;
        }
    };
    let Some(mut scan) = app.sync_scans.take() else { return };
    if scan.guard.is_cancelled() || library != app.session.library_generation() {
        scan.stop();
        if scan.guard.is_cancelled() {
            scan.guard.finish(Outcome::ended(Ending::Stopped));
        }
        app.ui.dialog = None;
        return;
    }
    let Some(Dialog::SynchronizeFolders { folders, .. }) = &mut app.ui.dialog else { return };
    if let Some((i, progress, rx)) = &scan.at {
        let i = *i;
        let found = match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let (done, total) = progress.counts();
                scan.guard.detail(&folders.get(i).map(|f| format!("{} · {done} / {total}", f.name)).unwrap_or_default());
                app.sync_scans = Some(scan);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Ok(changes) => Some(changes),
            // the worker died (a panic, logged)
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        scan.at = None;
        match found {
            Some(c) => {
                if let Some(f) = folders.get_mut(i) {
                    f.counts = Some(SyncCounts {
                        offline: c.offline,
                        new: c.new.len(),
                        duplicates: c.duplicates,
                        unreadable: c.unreadable.len(),
                        missing: c.missing.len(),
                        metadata: c.metadata.len(),
                        moved: c.moved.len(),
                    });
                    f.missing = c.missing.iter().map(|m| m.id).collect();
                    f.moved = c.moved.iter().map(|m| m.id).collect();
                }
            }
            None => skip(folders, &mut scan.covered, i, crate::i18n::tr("Scan failed").to_string()),
        }
    }
    // the next folder not looked at yet; one that can't be scanned says why and is passed over
    while let Some(i) = folders.iter().position(|f| !f.settled()) {
        let Some(f) = folders.get(i) else { break };
        match SyncInput::new(&mut app.session, &f.path, false) {
            Ok(input) => {
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
                scan.guard.progress(i as u64, folders.len() as u64);
                scan.guard.detail(&f.name);
                scan.at = Some((i, progress, rx));
                app.sync_scans = Some(scan);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            // (a folder holding disks, mostly: the engine's reason is for agents)
            Err(_) => {
                skip(folders, &mut scan.covered, i, crate::i18n::tr("Can't be synchronized with the others: synchronize it by itself").to_string())
            }
        }
    }
    // all looked at
    scan.guard.progress(folders.len() as u64, folders.len() as u64);
    scan.guard.finish(Outcome::default());
}

/// Whether the Synchronize N Folders dialog has scanned every folder (its Synchronize waits for that).
pub fn many_scanned(dlg: &Dialog) -> bool {
    matches!(dlg, Dialog::SynchronizeFolders { folders, .. } if folders.iter().all(SyncFolder::settled))
}

/// Whether a folder's scan found something the choices would do.
fn has_work(c: &SyncCounts, choice: &SyncChoice) -> bool {
    !c.offline
        && ((choice.import_new && c.new > 0)
            || (choice.relink_moved && c.moved > 0)
            || (choice.remove_missing && c.missing > 0)
            || (choice.read_metadata && c.metadata > 0))
}

/// What the folders' scans found, added up (folders not there count nothing).
fn found(folders: &[SyncFolder]) -> SyncCounts {
    let mut sum = SyncCounts::default();
    for c in folders.iter().filter(|f| f.skipped.is_none()).filter_map(|f| f.counts.as_ref()).filter(|c| !c.offline) {
        sum.new = sum.new.saturating_add(c.new);
        sum.moved = sum.moved.saturating_add(c.moved);
        sum.missing = sum.missing.saturating_add(c.missing);
        sum.metadata = sum.metadata.saturating_add(c.metadata);
        sum.duplicates = sum.duplicates.saturating_add(c.duplicates);
        sum.unreadable = sum.unreadable.saturating_add(c.unreadable);
    }
    sum
}

/// Whether a scan found nothing to do.
fn up_to_date(c: &SyncCounts) -> bool {
    !c.offline && c.new == 0 && c.moved == 0 && c.missing == 0 && c.metadata == 0
}

/// What a folder's scan found, naming only what there is: "1500 new, 1 missing".
fn changes_found(c: &SyncCounts) -> String {
    let mut parts = Vec::new();
    if c.new > 0 {
        parts.push(crate::i18n::tr_format!("{n} new", n = c.new));
    }
    if c.moved > 0 {
        parts.push(crate::i18n::tr_format!("{n} moved", n = c.moved));
    }
    if c.missing > 0 {
        parts.push(crate::i18n::tr_format!("{n} missing", n = c.missing));
    }
    if c.metadata > 0 {
        parts.push(crate::i18n::tr_format!("{n} with changed sidecars", n = c.metadata));
    }
    parts.join(", ")
}

/// The body of the Synchronize N Folders dialog: a line per folder, then the choices with the folders' totals.
pub fn many_body(app: &LightcraftApp, ui: &mut egui::Ui, dlg: &mut Dialog) {
    let Dialog::SynchronizeFolders { folders, import_new, relink_moved, remove_missing, read_metadata, .. } = dlg else { return };
    let t = Tokens::get(ui.ctx());
    let scanning = app.sync_scans.as_ref().and_then(|s| s.at.as_ref().map(|(i, p, _)| (*i, p.counts())));
    ui.set_min_width(460.0);
    ui.label(crate::i18n::tr("Bring the library up to date with these folders and the folders inside them, one after another:"));
    ui.add_space(4.0);
    egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
        for (i, f) in folders.iter().enumerate() {
            let state = match (&f.skipped, &f.counts) {
                (Some(why), _) => why.clone(),
                (None, None) => match scanning {
                    Some((at, (done, total))) if at == i && total > 0 => format!("{} {done} / {total}", crate::i18n::tr("Scanning…")),
                    Some((at, _)) if at == i => crate::i18n::tr("Scanning…").to_string(),
                    _ => crate::i18n::tr("Waiting to be scanned").to_string(),
                },
                (None, Some(c)) if c.offline => {
                    crate::i18n::tr("This folder isn't there: it was moved or renamed, or its disk isn't connected.").to_string()
                }
                (None, Some(c)) if up_to_date(c) => crate::i18n::tr("Up to date").to_string(),
                (None, Some(c)) => changes_found(c),
            };
            let r = ui
                .horizontal(|ui| {
                    ui.label(egui::RichText::new(f.name.as_str()).strong()).on_hover_text(f.path.as_str());
                    ui.label(egui::RichText::new(state).color(t.text_dim));
                })
                .response;
            crate::widgets::register(ui.ctx(), format!("syncFolder:{i}"), r.rect);
        }
    });
    ui.add_space(6.0);
    let sum = found(folders);
    let choice = |ui: &mut egui::Ui, on: &mut bool, n: usize, text: String, widget: &str| {
        let r = ui.add_enabled(n > 0, egui::Checkbox::new(on, text));
        crate::widgets::register(ui.ctx(), widget, r.rect);
    };
    let count = |text: &str, n: usize| format!("{} ({n})", crate::i18n::tr(text));
    choice(ui, import_new, sum.new, count("Import new photos", sum.new), "syncImportNew");
    choice(ui, relink_moved, sum.moved, count("Relink photos whose file was renamed or moved", sum.moved), "syncRelinkMoved");
    choice(ui, remove_missing, sum.missing, count("Remove missing photos from the library", sum.missing), "syncRemoveMissing");
    choice(ui, read_metadata, sum.metadata, count("Read metadata updates from XMP sidecars", sum.metadata), "syncReadMetadata");
    ui.add_space(6.0);
    let dim = |ui: &mut egui::Ui, s: &str| ui.label(egui::RichText::new(s).color(t.text_dim));
    if sum.missing > 0 {
        dim(ui, crate::i18n::tr("Missing photos move to Recently Deleted and can be restored; no file on disk is touched."));
    }
    if sum.metadata > 0 {
        dim(ui, crate::i18n::tr("Reading a sidecar replaces the photo's rating, labels, metadata and edits with what it says."));
    }
    if sum.duplicates > 0 {
        dim(ui, &count("Skipped: files already in the library elsewhere", sum.duplicates));
    }
    if sum.unreadable > 0 {
        dim(ui, &count("Skipped: files that can't be read", sum.unreadable));
    }
}

/// Synchronize N Folders' Synchronize: each folder with something to do waits in the library lane as a Synchronize of
/// its own, in the dialog's order, removing or relinking only the photos its scan found (the first starts at once if
/// nothing else runs there). Folders up to date, not there or left out are passed over.
pub fn confirm_many(app: &mut LightcraftApp, dlg: &Dialog) -> Result<Value, String> {
    let Dialog::SynchronizeFolders { folders, import_new, relink_moved, remove_missing, read_metadata, library } = dlg else {
        return Err("not a Synchronize Folders dialog".into());
    };
    // another library opened meanwhile (from the menu bar, an agent): the photos found are another library's
    if *library != app.session.library_generation() {
        return Err(crate::i18n::tr("Another library was opened: nothing was done").to_string());
    }
    if !many_scanned(dlg) {
        return Err("the folders are still being scanned".into());
    }
    let choice = SyncChoice { import_new: *import_new, relink_moved: *relink_moved, remove_missing: *remove_missing, read_metadata: *read_metadata };
    let to_do: Vec<&SyncFolder> = folders.iter().filter(|f| f.skipped.is_none() && f.counts.as_ref().is_some_and(|c| has_work(c, &choice))).collect();
    // all or none: a line too full for them all takes none of them (the dialog stays, nothing half done)
    let room = lightcraft_engine::activity::MAX_WAITING.saturating_sub(app.session.activity.waiting().len());
    if to_do.len() > room {
        return Err(crate::i18n::tr_format!("Too many tasks are waiting: only {n} more can wait in line", n = room));
    }
    let mut queued = Vec::new();
    for f in &to_do {
        let request = sync_request(&f.path, &f.name, false, choice, &f.missing, &f.moved);
        let (id, _) = app.session.activity.enqueue("sync", "Synchronizing folder", request)?;
        queued.push(id);
    }
    let all_up_to_date = folders.iter().all(|f| f.skipped.is_none() && f.counts.as_ref().is_some_and(up_to_date));
    let text = match queued.len() {
        0 if all_up_to_date => crate::i18n::tr("Nothing to do: the library is up to date with these folders").to_string(),
        0 if !up_to_date(&found(folders)) => crate::i18n::tr("Nothing to do with the choices ticked").to_string(),
        0 => crate::i18n::tr("Nothing was synchronized").to_string(),
        1 => crate::i18n::tr("Synchronizing the folder: see it under the bell").to_string(),
        n => crate::i18n::tr_format!("Synchronizing {n} folders one after another: see them under the bell", n = n),
    };
    let now = app.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(app.last_time);
    app.ui.toast = Some((text, now + crate::TOAST_SECONDS, None));
    Ok(json!({"queued": queued}))
}

/// Hand the work of a synchronize to a worker thread, its steps committed between frames ([`poll_run`]). `left_alone`:
/// photos found missing (or moved) that its dialog never showed, said at the end.
fn run(
    app: &mut LightcraftApp,
    name: &str,
    work: lightcraft_engine::sync::SyncWork,
    commit: SyncCommit,
    progress: Arc<SyncProgress>,
    guard: TaskGuard,
    left_alone: usize,
) {
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
    app.sync_run = Some(SyncRun {
        name: name.to_string(),
        progress,
        crashed,
        rx,
        commit: Some(commit),
        stopping: false,
        not_saved: Vec::new(),
        guard: Some(guard),
        left_alone,
    });
}

/// The command of a Synchronize Folder's request ([`Request`]): the app runs it ([`start_request`]).
pub const SYNC_REQUEST: &str = "app:synchronize";

/// What a synchronize was asked, to run it later in line or again: the folder, the dialog's choices, and the missing
/// and moved photos it showed (the only ones it may remove or relink).
fn sync_request(path: &str, name: &str, disk: bool, choice: SyncChoice, missing: &[u64], moved: &[u64]) -> Request {
    Request {
        command: SYNC_REQUEST.into(),
        params: json!({
            "path": path, "name": name, "disk": disk,
            "importNew": choice.import_new, "relinkMoved": choice.relink_moved, "removeMissing": choice.remove_missing, "readMetadata": choice.read_metadata,
            "missing": missing, "moved": moved,
        }),
        subject: name.to_string(),
    }
}

/// A synchronize that waited in line, scanning its folder again on a worker thread before it runs ([`poll_rescan`]).
pub struct SyncRescan {
    name: String,
    choice: SyncChoice,
    /// The missing and moved photos its dialog showed.
    missing: std::collections::HashSet<u64>,
    moved: std::collections::HashSet<u64>,
    /// The library it was asked in ([`lightcraft_engine::Session::library_generation`]): its photo ids mean nothing in
    /// another one.
    library: u64,
    progress: Arc<SyncProgress>,
    rx: std::sync::mpsc::Receiver<FolderChanges>,
    /// Its row, from the line through the scan to the end of the run.
    guard: TaskGuard,
}

/// Start a synchronize that waited in line (`request`, made by [`sync_request`]): it scans the folder again, under the
/// row it had while it waited, and runs when the scan is in ([`poll_rescan`]).
pub fn start_request(app: &mut LightcraftApp, _ctx: &egui::Context, request: &Request) -> Result<(), String> {
    let p = &request.params;
    let path = p["path"].as_str().ok_or("no folder to synchronize")?;
    let name = p["name"].as_str().unwrap_or(path).to_string();
    let flag = |k: &str| p[k].as_bool().unwrap_or(false);
    let ids = |k: &str| p[k].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let choice = SyncChoice {
        import_new: flag("importNew"),
        relink_moved: flag("relinkMoved"),
        remove_missing: flag("removeMissing"),
        read_metadata: flag("readMetadata"),
    };
    let input = SyncInput::new(&mut app.session, path, flag("disk")).map_err(|e| e.to_string())?;
    let progress = Arc::new(SyncProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let pr = progress.clone();
    let job = move || {
        let _ = tx.send(scan_with(input, &pr));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = app.session.activity.start("sync", "Synchronizing folder", Cancel::Flag(progress.files.cancel.clone()));
    guard.detail(&name);
    guard.set_request(request.clone());
    let library = app.session.library_generation();
    app.sync_queued = Some(SyncRescan { name, choice, missing: ids("missing"), moved: ids("moved"), library, progress, rx, guard });
    Ok(())
}

/// Run the waiting synchronize once its scan is in (called every frame): only the missing and moved photos its dialog
/// showed are acted on.
pub fn poll_rescan(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = app.sync_queued.as_ref() else { return };
    let (done, total) = task.progress.counts();
    task.guard.progress(done as u64, total as u64);
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let got = task.rx.try_recv();
    if matches!(got, Err(std::sync::mpsc::TryRecvError::Empty)) && !task.guard.is_cancelled() {
        return;
    }
    let Some(task) = app.sync_queued.take() else { return };
    let SyncRescan { name, choice, missing, moved, library, progress, guard, .. } = task;
    if library != app.session.library_generation() {
        // another library was opened while it scanned: its photos are not these
        progress.files.cancel.store(true, Ordering::Relaxed);
        guard.finish(Outcome {
            ending: Ending::Stopped,
            detail: crate::i18n::tr("Another library was opened: nothing was done").into(),
            ..Default::default()
        });
        return;
    }
    let mut changes = match got {
        _ if guard.is_cancelled() => {
            progress.files.cancel.store(true, Ordering::Relaxed);
            guard.finish(Outcome::ended(Ending::Stopped));
            return;
        }
        Ok(c) => c,
        Err(_) => {
            // the scan's worker died (a panic, logged)
            guard.finish(Outcome::ended(Ending::Died));
            app.toast(ctx, crate::i18n::tr("Scan failed"));
            return;
        }
    };
    if changes.offline {
        let why = crate::i18n::tr("This folder isn't there: it was moved or renamed, or its disk isn't connected.").to_string();
        guard.finish(Outcome::failed(why.clone()));
        app.toast(ctx, format!("{name}: {why}"));
        return;
    }
    // only what the person saw is removed or relinked; what they would have been asked about is said
    let (missing_before, moved_before) = (changes.missing.len(), changes.moved.len());
    changes.missing.retain(|m| missing.contains(&m.id));
    changes.moved.retain(|m| moved.contains(&m.id));
    let left_alone = if choice.remove_missing { missing_before - changes.missing.len() } else { 0 }
        + if choice.relink_moved { moved_before - changes.moved.len() } else { 0 };
    match SyncJob::start(&mut app.session, changes, choice) {
        Ok((work, commit)) => {
            // (a fresh count, the same ✕: its row's flag stops the run as it stopped the scan)
            let run_progress = Arc::new(SyncProgress {
                files: lightcraft_engine::import::ScanProgress { cancel: progress.files.cancel.clone(), ..Default::default() },
                ..Default::default()
            });
            run(app, &name, work, commit, run_progress, guard, left_alone);
        }
        Err(e) => {
            guard.finish(Outcome::failed(e.to_string()));
            app.toast(ctx, format!("{name}: {e}"));
        }
    }
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
    /// Its row in the activity stack (its ✕ is Cancel); taken at the end to say how the run went.
    guard: Option<TaskGuard>,
    /// Photos found missing or moved that its dialog never showed (a run that waited in line): left alone, and said.
    left_alone: usize,
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

/// Commit what the worker readied (called every frame); when it is done, one undo step and a
/// word on what happened.
pub fn poll_run(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(run) = app.sync_run.as_mut() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    if run.guard.as_ref().is_some_and(TaskGuard::is_cancelled) {
        run.stopping = true;
    }
    let (done, total) = run.counts();
    if let Some(guard) = &run.guard {
        guard.progress(done as u64, total as u64);
    }
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
    let (name, stopped, crashed, mut not_saved, guard, left_alone) =
        (run.name.clone(), run.stopping, run.crashed.load(Ordering::Relaxed), std::mem::take(&mut run.not_saved), run.guard.take(), run.left_alone);
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
    if left_alone > 0 {
        parts.push(crate::i18n::tr_format!("{n} more missing, left alone: synchronize again to review them", n = left_alone));
    }
    let text = if parts.is_empty() { crate::i18n::tr("The library is up to date with this folder.").to_string() } else { parts.join(" · ") };
    if let Some(guard) = guard {
        let ending = if crashed {
            Ending::Died
        } else if stopped {
            Ending::Stopped
        } else if !r.failed.is_empty() || !not_saved.is_empty() {
            Ending::Problems
        } else {
            Ending::Done
        };
        let failures = r.failed.iter().map(|(path, e)| Failure { name: path.clone(), reason: e.clone() }).collect();
        guard.finish(Outcome { ending, summary: format!("{name}: {text}"), failures, ..Default::default() });
    }
    if not_saved.is_empty() && !crashed {
        app.toast(ctx, format!("{name}: {text}"));
    } else {
        app.toast_for(ctx, format!("{name}: {text}"), 12.0);
    }
}
