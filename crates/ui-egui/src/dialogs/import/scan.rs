use super::task::ImportTask;
use super::{ImportDialog, LightcraftApp, Ordering, ScanInput, ScanOutput, ScanProgress, Value, json, scan_with};

/// A folder scan running on a worker thread (a network share can take minutes to read; the
/// window must keep answering meanwhile).
pub struct ScanTask {
    progress: std::sync::Arc<ScanProgress>,
    rx: std::sync::mpsc::Receiver<ScanEnd>,
    /// Open the review with "copy into the library" checked (a camera / card).
    pub copy: bool,
    /// Open the review with this album chosen (an album's context menu ▸ Import…).
    pub album: Option<u64>,
    /// Browsing a folder (Local): the photos are read in place instead of opening the review.
    pub(super) browse: bool,
    /// What is being scanned (the review's source).
    sources: Vec<String>,
    /// The scan's row in the activity stack (its ✕ and `activity.cancel` set `progress.cancel`).
    guard: lightcraft_engine::activity::TaskGuard,
    /// Browsing: the folder as the worker resolved it, once it has. The view shows it as given until then
    /// (resolving reads the disk, which a stalled network mount can block: never on this thread).
    resolved: Option<std::sync::mpsc::Receiver<String>>,
}

/// What a scan's worker sends back. Every way it ends sends one, so the probes of earlier scans that
/// [`ScanInput::new`] took from the session always go back to it (a later import doesn't read those files again).
pub(crate) enum ScanEnd {
    /// The scan finished.
    Done(ScanOutput),
    /// Browsing stopped without a listing: the folder couldn't be listed (`failed`), or another folder was
    /// chosen. Never an empty listing, which the browse bookkeeping would take for every file gone.
    Stopped { probes: std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo>, failed: bool },
}

impl ScanEnd {
    /// The probes this end hands back to the session.
    fn into_probes(self) -> std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo> {
        match self {
            ScanEnd::Done(out) => out.probes,
            ScanEnd::Stopped { probes, .. } => probes,
        }
    }
}

/// Stop the running scan (if any) without waiting for its worker: the scan is closed at once, and its
/// channel is kept until the worker ends, so the probes it holds still come back ([`collect_stopped`]).
fn stop_scan(app: &mut LightcraftApp) {
    if let Some(t) = app.scan.take() {
        t.progress.cancel.store(true, Ordering::Relaxed);
        app.scans_stopping.push(t.rx);
    }
}

/// The probes of stopped scans whose workers have ended go back to the session (called every frame).
fn collect_stopped(app: &mut LightcraftApp, ctx: &egui::Context) {
    let probes = &mut app.session.import_probes;
    app.scans_stopping.retain(|rx| match rx.try_recv() {
        Ok(end) => {
            // (newer probes, taken since, win)
            for (k, v) in end.into_probes() {
                probes.entry(k).or_insert(v);
            }
            false
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => true,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
    });
    if !app.scans_stopping.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// The activity row of a scan: the review's ("Scanning folder") or the Local view's ("Reading folder").
fn scan_guard(app: &LightcraftApp, progress: &ScanProgress, browse: bool) -> lightcraft_engine::activity::TaskGuard {
    let label = if browse { "Reading folder" } else { "Scanning folder" };
    app.session.activity.start("scan", label, lightcraft_engine::activity::Cancel::Flag(progress.cancel.clone()))
}

/// An import and a Synchronize Folder never run at once: each readies its files against the
/// library as it was when it started, so both could add the same file.
pub(crate) fn busy_synchronizing(app: &LightcraftApp) -> Result<(), String> {
    if app.sync_run.is_some() { Err(crate::i18n::tr("A folder is being synchronized").to_string()) } else { Ok(()) }
}

/// Settings → Import → "Show the import review when a camera or card is connected", called with
/// each device scan while it's on. The first look only notes the cards already in. A card that
/// shows up after that opens Import from Device once nothing else is open, and a card that is
/// taken out and put back in asks again.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn cards_seen(app: &mut LightcraftApp, now: Vec<lightcraft_engine::devices::Device>) {
    let Some(known) = app.ui.known_cards.as_mut() else {
        app.ui.known_cards = Some(now.into_iter().map(|d| d.root).collect());
        return;
    };
    known.retain(|root| now.iter().any(|d| d.root == *root));
    let Some(card) = now.into_iter().find(|d| !known.contains(&d.root)) else { return };
    // busy: ask when the window is free (the card isn't noted until then)
    if app.import.is_some() || app.scan.is_some() || app.sync_run.is_some() || app.lightroom.is_some() || app.ui.dialog.is_some() {
        return;
    }
    known.push(card.root);
    if let Err(e) = app.run("file.addFromDevice", json!({"path": card.path})) {
        log::warn!("import from {}: {e}", card.name);
    }
}

/// Scan `paths` in the background, then open the review dialog (see [`poll_scan`]).
pub fn open(app: &mut LightcraftApp, paths: Vec<String>) -> Result<Value, String> {
    if app.scan.is_some() {
        return Err("a scan is already running".into());
    }
    if app.review_pending.is_some() {
        return Err("an import review is waiting for the open dialog to close".into());
    }
    let sources = paths.clone();
    let (input, paths) = ScanInput::new(&mut app.session, &paths);
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let job = move || {
        let _ = tx.send(ScanEnd::Done(scan_with(input, &paths, &p)));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse: false, sources, guard, resolved: None });
    Ok(json!({"scanning": true}))
}

/// Show a folder's photos in the Local view (`library.browse` from the UI): the view switches at
/// once, the folder is listed and read on a worker thread (a network share can take minutes),
/// and the photos then join the view in small batches under a progress window. A browse already
/// running is replaced; the import review's scan is not.
pub fn browse(app: &mut LightcraftApp, path: &str, subfolders: Option<bool>) -> Result<Value, String> {
    // one spelling for the view, the listing and the catalog's records: `a/lib/../outside` is `a/outside`. Taken out
    // by name here, without reading the disk (a link on a stalled network mount would freeze the window): the worker
    // resolves the folder, checks it is one, and the view takes its spelling when it says (see `poll_scan`)
    let dir = lightcraft_engine::import::browse_spelling(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    if app.scan.as_ref().is_some_and(|t| !t.browse) || app.import.as_ref().is_some_and(|t| !t.browse) {
        return Err("an import is running".into());
    }
    let dir_s = dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    let subfolders = subfolders.unwrap_or_else(|| app.session.browse.as_ref().is_some_and(|b| b.subfolders));
    let running = app.scan.as_ref().is_some_and(|t| t.browse) || app.import.as_ref().is_some_and(|t| t.browse);
    if running && app.session.browse.as_ref().is_some_and(|b| b.path == dir_s && b.subfolders == subfolders) {
        // already reading this folder: clicking it again must not restart the progress
        app.session.source = lightcraft_engine::LibrarySource::Folder;
        app.session.keep_active_in_view();
        return Ok(json!({"path": dir_s, "subfolders": subfolders, "scanning": true}));
    }
    stop_scan(app);
    app.import = None;
    app.session.browse = Some(lightcraft_engine::Browse { path: dir_s.clone(), subfolders });
    app.session.source = lightcraft_engine::LibrarySource::Folder;
    // the photos the library already knows from this folder show at once: keep or pick the active one
    app.session.keep_active_in_view();
    let (mut input, _) = ScanInput::new(&mut app.session, std::slice::from_ref(&dir_s));
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    // the worker resolves the path as given: on Unix `link/..` is the folder above where the link points, which the
    // spelling (`..` taken out by name) can't tell
    let given = path.to_owned();
    let sort = app.session.sort;
    let (resolved_tx, resolved) = std::sync::mpsc::channel();
    let job = move || {
        let root = match resolve_browse_root(&given) {
            Ok(root) => root,
            Err(e) => {
                log::warn!("browse: can't read {given}: {e}");
                let _ = tx.send(ScanEnd::Stopped { probes: input.take_probes(), failed: true });
                return;
            }
        };
        let _ = resolved_tx.send(root.clone());
        // (never the library's own folder, which the import would otherwise read as photos)
        let files = match lightcraft_engine::import::browse_files(&input, &root, subfolders) {
            Ok(files) => files,
            Err(e) => {
                // no listing is sent: the scan ends as failed, and no record is taken for a file gone
                log::warn!("browse: can't list {root}: {e}");
                let _ = tx.send(ScanEnd::Stopped { probes: input.take_probes(), failed: true });
                return;
            }
        };
        // Nothing is read here: the import that follows reads the files and adds them batch by
        // batch, first screen first, so the view fills while the rest is read (PERF-CULL-1).
        // Another folder chosen meanwhile (`p.cancel`) stops the ordering, and no listing is sent.
        let Some(queue) = lightcraft_engine::import::browse_queue_until(&input, files.clone(), &sort, &p.cancel) else {
            let _ = tx.send(ScanEnd::Stopped { probes: input.take_probes(), failed: false });
            return;
        };
        p.total.store(queue.len(), Ordering::Relaxed);
        p.done.store(queue.len(), Ordering::Relaxed);
        // the files the library knows come too, marked as such: the folder bookkeeping (browse
        // times, records of files gone) needs the whole listing; only the queue is imported
        let queued: std::collections::HashSet<&String> = queue.iter().collect();
        let known: Vec<String> = files.iter().filter(|f| !queued.contains(f)).cloned().collect();
        let known =
            known.into_iter().map(|path| lightcraft_engine::import::ImportCandidate { path, duplicate: Some("path".into()), ..Default::default() });
        let candidates =
            queue.iter().cloned().map(|path| lightcraft_engine::import::ImportCandidate { path, ..Default::default() }).chain(known).collect();
        // the probes of earlier scans go back to the session with the result, for the import that follows
        let _ = tx.send(ScanEnd::Done(lightcraft_engine::import::ScanOutput { candidates, probes: input.take_probes() }));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    // the job takes the probes back out of `input`, so it is an FnMut where it runs in place
    #[cfg(target_arch = "wasm32")]
    {
        let mut job = job;
        job();
    }
    let guard = scan_guard(app, &progress, true);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse: true, sources: Vec::new(), guard, resolved: Some(resolved) });
    app.renderer.forget_imports();
    Ok(json!({"path": dir_s, "subfolders": subfolders, "scanning": true}))
}

/// The browsed folder `given` (the path as `browse` received it, not its [`lightcraft_engine::import::browse_spelling`])
/// as [`lightcraft_engine::import::browse_path`] resolves it, checked to be a folder: on the browse worker, as it reads the disk.
fn resolve_browse_root(given: &str) -> Result<String, String> {
    #[cfg(test)]
    held_resolve(given);
    let dir = lightcraft_engine::import::browse_path(std::path::Path::new(given)).map_err(|e| e.to_string())?;
    if !dir.is_dir() {
        return Err(format!("{given}: not a folder"));
    }
    Ok(dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string())
}

/// Tests: browsed folders whose resolution waits (as on a stalled network mount) while they are listed here.
#[cfg(test)]
pub(crate) static HELD_RESOLVES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn held_resolve(given: &str) {
    let held = || HELD_RESOLVES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|p| p == given);
    let start = std::time::Instant::now();
    while held() && start.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Collect a finished scan and open the review (called every frame).
pub fn poll_scan(app: &mut LightcraftApp, ctx: &egui::Context) {
    show_pending_review(app, ctx);
    collect_stopped(app, ctx);
    let Some(task) = app.scan.as_ref() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    // the worker resolved the browsed folder: the view takes its spelling (before its listing, sent after it)
    if let Some(root) = task.resolved.as_ref().and_then(|r| r.try_recv().ok())
        && let Some(b) = app.session.browse.as_mut().filter(|b| b.path != root)
    {
        b.path = root;
        app.session.keep_active_in_view();
    }
    let Some(task) = app.scan.as_ref() else { return };
    let end = match task.rx.try_recv() {
        Ok(o) => o,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            if task.guard.is_cancelled() {
                // ✕: the worker stops at its next file; don't wait for it (a NAS read can take a while)
                stop_scan(app);
                return;
            }
            let (total, done) = (task.progress.total.load(Ordering::Relaxed), task.progress.done.load(Ordering::Relaxed));
            task.guard.progress(done as u64, total as u64);
            // while the folders are listed the total isn't known: say what is happening instead
            task.guard.detail(if total == 0 { crate::i18n::tr("Looking for photos…") } else { "" });
            return;
        }
        Err(_) => {
            // a cancelled scan whose worker ended without a result was not a failure
            let cancelled = task.guard.is_cancelled() || task.progress.cancel.load(Ordering::Relaxed);
            app.scan = None;
            if !cancelled {
                app.toast(ctx, crate::i18n::tr("Scan failed"));
            }
            return;
        }
    };
    let Some(task) = app.scan.take() else { return };
    let cancelled = task.progress.cancel.load(Ordering::Relaxed);
    let out = match end {
        ScanEnd::Done(out) => out,
        ScanEnd::Stopped { probes, failed } => {
            app.session.import_probes.extend(probes);
            if failed && !cancelled {
                app.toast(ctx, crate::i18n::tr("Scan failed"));
            }
            return;
        }
    };
    // the probes go back to the session however the scan ended (the ones of a scan stopped earlier may be there)
    app.session.import_probes.extend(out.probes);
    if cancelled {
        return;
    }
    if task.browse {
        // the folder and the ones its photos are in count as browsed now, as `library.browse` records them, so
        // their untouched Local photos aren't forgotten while the folder is in use
        if let Some(root) = app.session.browse.as_ref().map(|b| b.path.clone()) {
            let shown: Vec<String> = out.candidates.iter().map(|c| c.path.clone()).collect();
            app.session.note_browsed(&root, &shown);
        }
        // records of files gone from the folder (deleted or moved by another app) that nothing was changed on go,
        // as `library.browse` forgets them (issue #1392)
        if let Some(b) = app.session.browse.clone() {
            let listed: Vec<String> = out.candidates.iter().map(|c| c.path.clone()).collect();
            app.session.forget_gone_from(&b.path, b.subfolders, &listed);
        }
        // what the library doesn't know yet joins the Local view
        let queue: Vec<String> =
            out.candidates.iter().filter(|c| c.duplicate != Some("path".into()) && c.error.is_none()).map(|c| c.path.clone()).collect();
        if !queue.is_empty() {
            let undo0 = app.session.undo.len();
            let total = queue.len();
            let _ = total;
            app.import = Some(ImportTask::new(queue, json!({"mode": "add", "local": true}), undo0, true));
        }
        return;
    }
    if out.candidates.is_empty() {
        app.toast(ctx, crate::i18n::tr("No photos found"));
        return;
    }
    app.renderer.forget_imports();
    let mut d = ImportDialog::new(out.candidates);
    let trashed = d
        .candidates
        .iter()
        .map(|c| {
            c.duplicate.is_some()
                && c.existing.is_some_and(|id| app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).is_some_and(|p| p.deleted))
        })
        .collect();
    d.set_trashed(trashed);
    d.copy = task.copy;
    // only an album the review offers (it may have been deleted while the scan ran)
    d.album = task.album.filter(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(*id)).is_some_and(|a| !a.folder && !a.is_smart()));
    d.sources = task.sources;
    d.library = Some(app.session.library_generation());
    app.review_pending = Some(crate::state::Dialog::Import { opts: Box::new(d) });
    show_pending_review(app, ctx);
}

/// Open the review of a finished scan once no other dialog is open (a face model's licence, Settings…
/// opened while the scan ran stays), checked again shortly while it waits. A review whose library was
/// closed meanwhile (Settings can open another) is dropped.
fn show_pending_review(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.review_pending.is_none() {
        return;
    }
    if let Some(crate::state::Dialog::Import { opts }) = &app.review_pending
        && !opts.in_library(app)
    {
        app.review_pending = None;
        return;
    }
    if app.ui.dialog.is_none() {
        app.ui.dialog = app.review_pending.take();
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Tests: a review scan that never ends while the returned value lives, so a drop meets it running.
#[cfg(test)]
pub(crate) fn stall_scan(app: &mut LightcraftApp) -> Box<dyn std::any::Any> {
    stall_scan_of(app, false)
}

/// Tests: the same, for a Local folder browse scan (`browse`) instead of the review's scan.
#[cfg(test)]
pub(crate) fn stall_scan_of(app: &mut LightcraftApp, browse: bool) -> Box<dyn std::any::Any> {
    Box::new(scan_by_hand(app, browse))
}

/// Tests: a scan whose worker is the test: what it sends through the returned channel is the scan's end.
#[cfg(test)]
pub(crate) fn scan_by_hand(app: &mut LightcraftApp, browse: bool) -> std::sync::mpsc::Sender<ScanEnd> {
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel::<ScanEnd>();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse, sources: Vec::new(), guard, resolved: None });
    tx
}

/// Whether an import or any scan (the review's, or a Local folder browse) is running, or a finished scan's
/// review waits for a dialog to close, which a drop of photos must not replace: `open` refuses while a scan
/// runs, and a browse that finishes replaces `app.import`.
pub fn running(app: &LightcraftApp) -> bool {
    app.import.is_some() || app.scan.is_some() || app.review_pending.is_some()
}

impl ScanTask {
    /// `{done, total}` for `ui.inspect` (total is 0 while the folders are still being listed).
    pub fn status(&self) -> Value {
        json!({"done": self.progress.done.load(Ordering::Relaxed), "total": self.progress.total.load(Ordering::Relaxed)})
    }
}
