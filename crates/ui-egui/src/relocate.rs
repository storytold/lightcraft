//! Locating a library folder that is somewhere else now, in the app (`folder.locate`, and
//! `folder.updateLocation` when an agent calls it here; see `lightcraft_engine::relocate`, issue
//! #1112).
//!
//! The person says where the folder is (Synchronize Folder's button when the folder isn't
//! there, or Update Folder Location… on a Folders row). Whether that is the folder is told by
//! looking for the photos' files there, which can take a while on a network share: it runs on a
//! worker thread under a row in the activity stack (its ✕ stops it), and no frame waits for it.
//! Then:
//!
//! * every file is there: the photos are relinked (one undo step, no file touched);
//! * none is: it is not the folder, and a toast says so (the dialog it came from stays open);
//! * only some are (the others are not there, or are not the size the library recorded): a
//!   dialog asks whether to locate the folder there anyway;
//! * a file there is another photo's of the same folder: refused, and a toast says so.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use lightcraft_catalog::query::folder_key;
use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::import::ScanProgress;
use lightcraft_engine::relocate::{Located, Relocation};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;

/// What was asked (`folder.locate`'s parameters).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The folder as the library knows it.
    pub path: String,
    /// Where it is now.
    pub to: String,
    /// A whole disk, or a folder holding disks (`folder.updateLocation` `disk`).
    pub disk: bool,
    /// Locate it there even when only some of its files are (the person agreed).
    pub incomplete: bool,
    /// Open Synchronize Folder on it afterwards: it was asked from that dialog. Dropped by
    /// [`poll`] when the dialog is closed before the files were all looked for.
    pub synchronize: bool,
    /// A person asked, and is asked in turn when only some of the files are there. Not an agent
    /// (`folder.updateLocation`): it is told why not, as by the command anywhere else.
    pub asks: bool,
}

/// The files of a folder being looked for at its new place.
pub struct RelocateTask {
    request: Request,
    progress: Arc<ScanProgress>,
    rx: std::sync::mpsc::Receiver<Result<Located, String>>,
    /// Its row in the activity stack; ✕ stops the looking and nothing is changed.
    guard: TaskGuard,
}

impl Drop for RelocateTask {
    /// A dropped task (the app closing) stops its worker at the next file.
    fn drop(&mut self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

/// Start looking for the folder's files at the new place. Refused while another folder is being
/// located, and while an import or a Synchronize Folder runs (both add and relink photos of
/// folders: the answer could be about a library that is no longer).
pub fn start(app: &mut LightcraftApp, request: Request) -> Result<Value, String> {
    // (a step of the library left, not yet seen by a frame, is not in the way of this one)
    drop_stale(app);
    reap_left_behind(app);
    if app.relocate_left_behind.len() >= MAX_LEFT_BEHIND {
        // (the reads of the libraries left have not ended: a share is not answering)
        return Err(crate::i18n::tr("A folder is already being located").to_string());
    }
    // (a question that waits is about the library as it is: another step meanwhile, an agent's
    // say, would have its answer relink photos that are no longer where they were found)
    if app.relocate.is_some() {
        return Err(crate::i18n::tr("A folder is already being located").to_string());
    }
    if app.relocate_asked.is_some() {
        // (it may wait behind another dialog: `ui.inspect` → `relocate.asking` says for which folder)
        return Err(crate::i18n::tr("A question about locating a folder is waiting for its answer").to_string());
    }
    if app.import.is_some() {
        return Err(crate::i18n::tr("Wait for the import to finish").to_string());
    }
    if app.sync_run.is_some() {
        return Err(crate::i18n::tr("A folder is being synchronized").to_string());
    }
    let plan = Relocation::new(&app.session, &request.path, &request.to, request.disk).map_err(|e| e.to_string())?;
    let name = lightcraft_catalog::folders::folder_label(&plan.from);
    let progress = Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let repaint = app.tasks.repaint.clone();
    let job = move || {
        // (a panic in the looking is its error, said in a toast; nothing was changed)
        let found = lightcraft_engine::guard::catch("locating the folder", || plan.look(&p)).unwrap_or_else(Err);
        let _ = tx.send(found);
        if let Some(c) = repaint {
            c.request_repaint();
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("lc-relocate".into()).spawn(job).map_err(|e| format!("could not start: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    job();
    track(app, request, progress, rx, &name);
    Ok(json!({"started": true}))
}

/// The row in the activity stack and the task that [`poll`] collects the worker's answer from.
fn track(app: &mut LightcraftApp, request: Request, progress: Arc<ScanProgress>, rx: std::sync::mpsc::Receiver<Result<Located, String>>, name: &str) {
    let guard = app.session.activity.start("relocate", "Locating folder", Cancel::Flag(progress.cancel.clone()));
    guard.detail(name);
    app.relocate = Some(RelocateTask { request, progress, rx, guard });
    app.relocate_last = None;
    app.relocate_library = app.session.library_generation();
}

/// A task whose worker is the caller: it answers on the returned sender when it likes, so a
/// test can hold the worker "still reading" while the row is cancelled.
#[cfg(test)]
pub(crate) fn track_held_worker(app: &mut LightcraftApp, request: Request) -> std::sync::mpsc::Sender<Result<Located, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    track(app, request, Arc::new(ScanProgress::default()), rx, "held");
    tx
}

/// How many workers of lookings in libraries that were left are kept while they still read (a
/// share that does not answer holds a worker in `std::fs::metadata`, which nothing can interrupt);
/// a looking is refused beyond that, so library switches can't pile up blocked threads.
pub const MAX_LEFT_BEHIND: usize = 4;

/// The files were looked for (or the question asked) in a library that is no longer open: its
/// plan names that library's photos, and an answer would relink whatever the open library has
/// at the same paths. Dropped, with how it ended; the worker of a looking in progress is told to
/// stop and kept (with its row) in `relocate_left_behind` until it answers, so the next library's
/// first request need not wait for it, and [`reap_left_behind`] and [`MAX_LEFT_BEHIND`] bound them.
fn drop_stale(app: &mut LightcraftApp) {
    if (app.relocate.is_some() || app.relocate_asked.is_some()) && app.relocate_library != app.session.library_generation() {
        let request = app.relocate.as_ref().map(|t| &t.request).or(app.relocate_asked.as_ref().map(|a| &a.0));
        app.relocate_last = request.map(|r| ended(r, json!({"cancelled": true})));
        if let Some(task) = app.relocate.take() {
            task.progress.cancel.store(true, Ordering::Relaxed);
            app.relocate_left_behind.push(task);
        }
        if app.relocate_asked.take().is_some() && matches!(app.ui.dialog, Some(Dialog::LocateFolder { .. })) {
            app.ui.dialog = None;
        }
    }
}

/// Let go of the left-behind workers that have answered (or ended without a word); whether some
/// are still reading.
fn reap_left_behind(app: &mut LightcraftApp) -> bool {
    app.relocate_left_behind.retain(|t| matches!(t.rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    !app.relocate_left_behind.is_empty()
}

/// Each frame: when the looking is done, relink the photos, say why not, or ask.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    drop_stale(app);
    if reap_left_behind(app) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    // Synchronize Folder afterwards is for its dialog, which waits for the answer: closed (or
    // another dialog opened) while the files are looked for, it is not opened again unasked.
    // (Once the question is shown it stands in for that dialog, and carries the wish itself.)
    let gone = |r: &Request| r.synchronize && !synchronizing(app, &r.path);
    if app.relocate.as_ref().is_some_and(|t| gone(&t.request)) || app.relocate_asked.as_ref().is_some_and(|a| !a.2 && !a.0.incomplete && gone(&a.0)) {
        if let Some(t) = &mut app.relocate {
            t.request.synchronize = false;
        }
        if let Some((r, ..)) = &mut app.relocate_asked {
            r.synchronize = false;
        }
    }
    // what the question is about: relinked once agreed to, let go of when the dialog was closed
    if let Some((request, located, shown)) = &mut app.relocate_asked {
        let asking = matches!(app.ui.dialog, Some(Dialog::LocateFolder { .. }));
        if request.incomplete {
            if let Some((request, located, _)) = app.relocate_asked.take() {
                finish(app, ctx, request, located);
            }
        } else if !*shown {
            // asked once nothing else is being filled in: another dialog (an export being set
            // up while the files were looked for) is not taken away; the dialog of the folder
            // that isn't there gives way to the question about it
            let free = match &app.ui.dialog {
                None => true,
                Some(Dialog::SynchronizeFolder { path, .. }) => folder_key(path) == folder_key(&request.path),
                Some(_) => false,
            };
            if free {
                *shown = true;
                app.ui.dialog = Some(Dialog::LocateFolder {
                    path: request.path.clone(),
                    to: located.to.clone(),
                    disk: request.disk,
                    synchronize: request.synchronize,
                    found: located.found,
                    total: located.total,
                    different: located.different,
                });
            }
        } else if !asking {
            // Cancel: that is how it ended
            app.relocate_last = Some(ended(request, json!({"cancelled": true})));
            app.relocate_asked = None;
        }
    }
    let Some(task) = app.relocate.as_ref() else { return };
    // ✕ on its row: a count stopped part-way says nothing. The task and its row stay until the
    // worker has ended, so no second worker starts beside one that is still reading
    let cancelled = task.guard.is_cancelled();
    if !cancelled {
        let (done, total) = (task.progress.done.load(Ordering::Relaxed), task.progress.total.load(Ordering::Relaxed));
        task.guard.progress(done as u64, total as u64);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let located = match task.rx.try_recv() {
        Ok(l) => l,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(_) => Err(crate::i18n::tr("Locating the folder failed").to_string()),
    };
    let Some(task) = app.relocate.take() else { return };
    let request = task.request.clone();
    // (the row goes before anything is said or asked)
    drop(task);
    if cancelled {
        app.relocate_last = Some(ended(&request, json!({"cancelled": true})));
        return;
    }
    let located = match located {
        Ok(l) => l,
        Err(e) => {
            app.relocate_last = Some(ended(&request, json!({"error": e})));
            return app.toast_error(ctx, e);
        }
    };
    let Some(why) = located.refusal(request.incomplete) else { return finish(app, ctx, request, located) };
    if located.found > 0 && request.asks {
        // (asked on the next frame, see above)
        app.relocate_asked = Some((request, located, false));
        ctx.request_repaint();
        return;
    }
    // (the reason in the words the command has for it is the agent's; the person reads the toast)
    app.relocate_last = Some(ended(&request, json!({"error": why})));
    let name = lightcraft_catalog::folders::folder_label(&located.to);
    if located.found == 0 {
        app.toast_error(ctx, crate::i18n::tr_format!("None of this folder's photos are in “{name}”: choose the folder it is now", name = name));
    } else {
        app.toast_error(
            ctx,
            crate::i18n::tr_format!(
                "Only {found} of this folder's {total} photos are in “{name}”: the folder was not located",
                found = located.found,
                total = located.total,
                name = name
            ),
        );
    }
}

/// The dialog open now is Synchronize Folder for `folder` (however the folder is written: with
/// a trailing separator, say).
pub(crate) fn synchronizing(app: &LightcraftApp, folder: &str) -> bool {
    matches!(&app.ui.dialog, Some(Dialog::SynchronizeFolder { path, .. }) if folder_key(path) == folder_key(folder))
}

/// Relink the photos (one undo step) and say so; then Synchronize Folder at the new place when
/// it was asked from that dialog and nothing else was opened meanwhile, or when that dialog is
/// open on the folder.
fn finish(app: &mut LightcraftApp, ctx: &egui::Context, request: Request, located: Located) {
    let to = located.to.clone();
    match app.session.execute_fn("folder.updateLocation", |s| located.commit(s, request.incomplete)) {
        Ok(r) => {
            app.toast(ctx, crate::i18n::tr_format!("Folder located. Photos relinked: {n}", n = r["relinked"].as_u64().unwrap_or(0)));
            app.relocate_last = Some(r);
            // (a Synchronize dialog open on the folder as it was is about a folder the library
            // no longer has, whoever located it: an agent's step too brings it up to date)
            if (request.synchronize && app.ui.dialog.is_none()) || synchronizing(app, &request.path) {
                let name = lightcraft_catalog::folders::folder_label(&to);
                // the folder was chosen by the person, whatever it is: a disk too
                let opened = crate::sync::open(app, &to, &name, request.disk).or_else(|_| crate::sync::open(app, &to, &name, true));
                if let Err(e) = opened {
                    // (the old folder's dialog has nothing left to say: its folder is located)
                    app.ui.dialog = None;
                    app.toast_error(ctx, e);
                }
            }
        }
        Err(e) => {
            app.relocate_last = Some(ended(&request, json!({"error": e.to_string()})));
            app.toast_error(ctx, e.to_string());
        }
    }
}

/// How locating `request`'s folder ended without the command's result: `how` with the `folder`
/// and `path` asked for, so whoever reads it can tell whose end it is.
fn ended(request: &Request, mut how: Value) -> Value {
    how["folder"] = json!(request.path);
    how["path"] = json!(request.to);
    how
}

/// `ui.inspect` → `relocate`: `{running: {done, total} | null, last}`. `last` is how the latest
/// one went: `folder.updateLocation`'s result, `{error, folder, path}` or `{cancelled: true,
/// folder, path}` (✕ on its row, or Cancel in the question); `null` while one runs or waits for
/// the person's answer, and before the first. `asking`: the folder a question waits about
/// (`{folder, path, shown}`; not `shown` while another dialog is open), during which no other
/// folder is located.
pub fn status(app: &LightcraftApp) -> Value {
    let asking = app.relocate_asked.as_ref().map(|(r, _, shown)| json!({"folder": r.path, "path": r.to, "shown": shown}));
    let running =
        app.relocate.as_ref().map(|t| json!({"done": t.progress.done.load(Ordering::Relaxed), "total": t.progress.total.load(Ordering::Relaxed)}));
    json!({"running": running, "asking": asking, "last": app.relocate_last})
}

/// The question when only some of the folder's files are at the new place.
pub fn body(ui: &mut egui::Ui, dlg: &Dialog) {
    let Dialog::LocateFolder { to, found, total, different, .. } = dlg else { return };
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(420.0);
    ui.label(crate::i18n::tr_format!("Only {found} of this folder's {total} photos are in:", found = found, total = total));
    ui.label(egui::RichText::new(to.as_str()).strong());
    if *different > 0 {
        // (what tells a file changed where it lies, or another disk laid out alike, from the folder)
        ui.label(crate::i18n::tr_format!("Files there that are not the size the library recorded: {n}", n = different));
    }
    ui.add_space(6.0);
    ui.label(crate::i18n::tr("Locate the folder there anyway?"));
    // (a photo whose file there is another size is relinked too: Locate Anyway takes every photo of the folder)
    let others = if *different > 0 {
        crate::i18n::tr(
            "Photos with no file there stay missing, and photos whose file there is another size are linked to that file. Synchronize Folder can find them again or remove them from the library. No file is touched.",
        )
    } else {
        crate::i18n::tr(
            "The other photos stay missing there; Synchronize Folder can find them again or remove them from the library. No file is touched.",
        )
    };
    ui.label(egui::RichText::new(others).color(t.text_dim));
}

/// "Locate Anyway": what was found is relinked on the next frame ([`poll`]); the files are not
/// looked for a second time. A dialog with nothing behind it (set by an agent) looks first.
pub fn confirm(app: &mut LightcraftApp, dlg: &Dialog) -> Result<Value, String> {
    let Dialog::LocateFolder { path, to, disk, .. } = dlg else { return Ok(Value::Null) };
    match &mut app.relocate_asked {
        Some((request, located, _)) if request.path == *path && located.to == *to => {
            request.incomplete = true;
            Ok(json!({"started": true}))
        }
        // (a dialog an agent opened: no Synchronize Folder dialog waits behind it, so nothing to open afterwards)
        _ => start(app, Request { path: path.clone(), to: to.clone(), disk: *disk, incomplete: true, synchronize: false, asks: true }),
    }
}
