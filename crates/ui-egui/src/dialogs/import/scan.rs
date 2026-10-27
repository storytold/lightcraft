use super::task::ImportTask;
use super::*;

/// A folder scan running on a worker thread (a network share can take minutes to read; the
/// window must keep answering meanwhile).
pub struct ScanTask {
    progress: std::sync::Arc<ScanProgress>,
    rx: std::sync::mpsc::Receiver<ScanOutput>,
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
        let _ = tx.send(scan_with(input, &paths, &p));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse: false, sources, guard });
    Ok(json!({"scanning": true}))
}

/// Show a folder's photos in the Local view (`library.browse` from the UI): the view switches at
/// once, the folder is listed and read on a worker thread (a network share can take minutes),
/// and the photos then join the view in small batches under a progress window. A browse already
/// running is replaced; the import review's scan is not.
pub fn browse(app: &mut LightcraftApp, path: &str, subfolders: Option<bool>) -> Result<Value, String> {
    let dir = std::path::absolute(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    if !dir.is_dir() {
        return Err(format!("{path}: not a folder"));
    }
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
    if let Some(t) = app.scan.take() {
        t.progress.cancel.store(true, Ordering::Relaxed);
    }
    app.import = None;
    app.session.browse = Some(lightcraft_engine::Browse { path: dir_s.clone(), subfolders });
    app.session.source = lightcraft_engine::LibrarySource::Folder;
    // the photos the library already knows from this folder show at once: keep or pick the active one
    app.session.keep_active_in_view();
    let (input, _) = ScanInput::new(&mut app.session, std::slice::from_ref(&dir_s));
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let root = dir_s.clone();
    let job = move || {
        let files: Vec<String> = if subfolders {
            lightcraft_engine::import::expand(&[root], None)
        } else {
            let mut v: Vec<String> = std::fs::read_dir(&root)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|f| {
                            f.is_file()
                                && lightcraft_engine::import::is_supported(f)
                                && !f.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
                        })
                        .map(|f| f.to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        };
        let _ = tx.send(scan_with(input, &files, &p));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = scan_guard(app, &progress, true);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse: true, sources: Vec::new(), guard });
    app.renderer.forget_imports();
    Ok(json!({"path": dir_s, "subfolders": subfolders, "scanning": true}))
}

/// Collect a finished scan and open the review (called every frame).
pub fn poll_scan(app: &mut LightcraftApp, ctx: &egui::Context) {
    show_pending_review(app, ctx);
    let Some(task) = app.scan.as_ref() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let out = match task.rx.try_recv() {
        Ok(o) => o,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            if task.guard.is_cancelled() {
                // ✕: the worker stops at its next file; don't wait for it (a NAS read can take a while)
                app.scan = None;
                return;
            }
            let (total, done) = (task.progress.total.load(Ordering::Relaxed), task.progress.done.load(Ordering::Relaxed));
            task.guard.progress(done as u64, total as u64);
            // while the folders are listed the total isn't known: say what is happening instead
            task.guard.detail(if total == 0 { crate::i18n::tr("Looking for photos…") } else { "" });
            return;
        }
        Err(_) => {
            app.scan = None;
            app.toast(ctx, crate::i18n::tr("Scan failed"));
            return;
        }
    };
    let Some(task) = app.scan.take() else { return };
    if task.progress.cancel.load(Ordering::Relaxed) {
        return;
    }
    app.session.import_probes = out.probes;
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
    let progress = std::sync::Arc::new(ScanProgress::default());
    let (tx, rx) = std::sync::mpsc::channel::<ScanOutput>();
    let guard = scan_guard(app, &progress, false);
    app.scan = Some(ScanTask { progress, rx, copy: false, album: None, browse, sources: Vec::new(), guard });
    Box::new(tx)
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
