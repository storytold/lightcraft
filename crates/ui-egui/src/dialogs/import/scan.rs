use super::task::ImportTask;
use super::{ImportDialog, Lane, LightcraftApp, Ordering, Request, ScanInput, ScanOutput, ScanProgress, Value, json, scan_with};

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
    pub(crate) browse: bool,
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

/// The command of an import's request ([`Request`]): the app runs it ([`start_request`]), with its files and its
/// `library.import` params.
pub const IMPORT_REQUEST: &str = "app:import";

/// `sources`: the folders or files the person chose, which name the import; `counted`: `paths` are the photos the
/// review counted (dropped folders are not counted until they are read).
pub(super) fn import_request(paths: &[String], params: &Value, auto: bool, sources: &[String], counted: bool) -> Request {
    Request {
        command: IMPORT_REQUEST.into(),
        params: json!({"paths": paths, "params": params, "auto": auto}),
        subject: subject_of(sources),
        facts: facts_of(counted.then_some(paths.len()), params["mode"].as_str().unwrap_or("add")),
    }
}

/// What an import is about, for its rows: the source folder's (or file's) name, or how many sources there are. Empty
/// when it is not known where the photos come from.
pub(super) fn subject_of(sources: &[String]) -> String {
    match sources {
        [] => String::new(),
        [one] => {
            std::path::Path::new(one.trim_end_matches(['/', '\\'])).file_name().map_or_else(|| one.clone(), |n| n.to_string_lossy().into_owned())
        }
        many => crate::i18n::tr_format!("{} item{}", many.len(), "s"),
    }
}

/// What else an import waiting in line says: how many photos (when the review counted them) and how they come in.
fn facts_of(photos: Option<usize>, mode: &str) -> String {
    let how = match mode {
        "copy" => crate::i18n::tr("Copy"),
        "move" => crate::i18n::tr("Move"),
        _ => crate::i18n::tr("Add in place"),
    };
    match photos {
        Some(n) => format!("{} · {how}", crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)),
        None => how.to_string(),
    }
}

/// The library lane is taken: an import (or the reading of a Local folder) or a Synchronize Folder runs, or waits
/// to. Each readies its files against the library as it is when it starts, so two side by side could add the same
/// file: one asked for now waits in line ([`Lane::Library`]).
pub(crate) fn library_busy(app: &LightcraftApp) -> bool {
    app.import.is_some()
        || app.scan.as_ref().is_some_and(|t| t.browse)
        || app.sync_run.is_some()
        || app.sync_queued.is_some()
        || app.session.activity.lane_busy(Lane::Library)
        || app.session.activity.lane_waiting(Lane::Library)
}

/// What a task asked for now would start after, as its dialog says it ("Importing · Rome"): what runs in the library
/// lane, else the last task waiting there. `None` when the lane is free.
pub(crate) fn lane_ahead_text(app: &LightcraftApp) -> Option<String> {
    if !library_busy(app) {
        return None;
    }
    let rows = app.session.activity.list();
    let in_lane = |t: &&lightcraft_engine::activity::TaskInfo| t.lane == Some(Lane::Library);
    let ahead = rows.iter().filter(in_lane).find(|t| !t.waiting).or_else(|| rows.iter().rev().find(in_lane));
    // (a Local folder being read holds the lane with a row of its own kind, which has no lane)
    let reading = || app.scan.as_ref().is_some_and(|t| t.browse).then(|| crate::i18n::tr("Reading folder").to_string());
    Some(ahead.map(crate::panels::activity::waiting_name).or_else(reading).unwrap_or_else(|| crate::i18n::tr("Importing").to_string()))
}

/// Put an import of `paths` with `params` in line behind what runs in the library lane, and say so (an Auto Import
/// quietly: nobody asked for it).
pub(super) fn wait_in_line(app: &mut LightcraftApp, request: Request) -> Result<Value, String> {
    if app.session.activity.line_full() {
        return Err(crate::panels::activity::line_full_text());
    }
    let auto = request.params["auto"].as_bool().unwrap_or(false);
    let (id, place) = app.session.activity.enqueue("import", "Importing", request)?;
    if !auto {
        app.queued_toast(place as u64);
    }
    Ok(json!({"queued": id, "place": place}))
}

/// The watched folder was listed (Auto Import): import what is new in it, as an Auto Import. The engine has noted
/// those files as tried by now, so while the library lane is taken (it was free when the listing began) they wait in
/// line instead of being dropped.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn auto_import_listed(app: &mut LightcraftApp, listing: Vec<(String, u64)>) {
    // no room in the line: leave the folder to its next listing, because the scan below notes its files as tried
    if library_busy(app) && app.session.activity.waiting().len() >= lightcraft_engine::activity::MAX_WAITING {
        return;
    }
    let p = json!({"listing": listing, "start": false});
    let Ok(r) = app.session.execute("library.autoImportScan", &p) else { return };
    let Some(mut params) = r.get("import").cloned() else { return };
    let paths: Vec<String> = params["paths"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if let Some(o) = params.as_object_mut() {
        o.remove("paths");
    }
    if library_busy(app) {
        if let Err(e) = wait_in_line(app, import_request(&paths, &params, true, &[], true)) {
            log::warn!("auto import: {e}");
        }
        return;
    }
    let undo0 = app.session.undo.len();
    app.import = Some(ImportTask::new(paths, params, undo0, false).auto());
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
                        .filter(|e| !lightcraft_engine::walk::is_hidden_entry(e))
                        .map(|e| e.path())
                        .filter(|f| f.is_file() && lightcraft_engine::import::is_supported(f))
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

/// Whether an import or any scan (the review's, or a Local folder browse) is running, or a finished scan's review waits
/// for a dialog to close. A drop that was held behind a barrier is tried again only when this is over (a fresh drop
/// while an import runs is not refused: it waits in line, see [`drop_refusal`]).
pub fn running(app: &LightcraftApp) -> bool {
    app.import.is_some() || app.scan.is_some() || app.review_pending.is_some()
}

/// Why a drop of photos is refused now, in the person's language, if it is: a scan (the review's, or a Local folder
/// browse) is running, or a finished scan's review waits for a dialog to close. A drop must not replace them: `open`
/// refuses while a scan runs, and a browse that finishes replaces `app.import`. A drop while an import runs is not
/// refused: it waits in line ([`library_busy`]).
pub fn drop_refusal(app: &LightcraftApp) -> Option<String> {
    let why = match app.scan.as_ref() {
        Some(scan) if scan.browse => "A folder is still being read for the Local view: drop photos when it has finished",
        Some(_) => "A folder is still being scanned for the import review: drop photos when it has finished",
        None if app.review_pending.is_some() => "An import review is waiting to open: drop photos when it is closed",
        None => return None,
    };
    Some(crate::i18n::tr(why).to_string())
}

impl ScanTask {
    /// `{done, total}` for `ui.inspect` (total is 0 while the folders are still being listed).
    pub fn status(&self) -> Value {
        json!({"done": self.progress.done.load(Ordering::Relaxed), "total": self.progress.total.load(Ordering::Relaxed)})
    }
}
