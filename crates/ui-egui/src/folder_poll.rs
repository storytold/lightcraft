//! Folder polling in the app (issue #123; see `lcc_engine::folder_watch`): with polling on
//! (Settings ▸ Library; off by default), the library's folders are listed on a worker thread when
//! the library opens, when the app comes to the front and every few minutes. Only folders whose
//! listing changed are scanned (Synchronize Folder's scan, also on a worker thread). Their new
//! files are then counted for the folder's "N new" badge with a notice (new files: ask, the
//! default) or added in the background (add) as Synchronize Folder adds them. The whole poll is
//! one row in the activity stack; its ✕ stops it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use lcc_engine::activity::{Cancel, TaskGuard};
use lcc_engine::folder_watch::{self, NewFilePolicy, Polled};
use lcc_engine::sync::{FolderChanges, SyncInput, SyncJob, SyncProgress, scan_with};
use web_time::{Duration, Instant};

use crate::LccApp;

/// What the worker hands back.
enum Msg {
    /// The folders' listings.
    Polled(Polled),
    /// A changed folder's scan.
    Scanned(FolderChanges),
}

/// A poll at work.
pub struct PollTask {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// The library it was started for.
    library: Arc<()>,
    /// Folders still being scanned.
    scanning: usize,
    /// New files counted by this poll (for the notice).
    found: Vec<(String, usize)>,
    /// The poll's changed folders and narrowed parents ([`folder_watch::keep_new_in_scope`]).
    scope: (Vec<String>, std::collections::BTreeMap<String, Vec<String>>),
    guard: TaskGuard,
}

impl Drop for PollTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The app's polling state.
#[derive(Default)]
pub struct FolderPoll {
    pub task: Option<PollTask>,
    /// When the last poll started, and for which library.
    last: Option<(Instant, Arc<()>)>,
    /// Scans whose new files wait to be added (new files: add) while another synchronize or
    /// an import runs.
    to_add: Vec<FolderChanges>,
}

/// Start a poll now unless one is running (polling on or not: the person or an agent asked).
pub fn start(app: &mut LccApp) {
    if app.folder_poll.task.is_some() {
        return;
    }
    let dirs = folder_watch::watched_dirs(&app.session.catalog);
    let prev = app.session.folder_stamps.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let c = cancel.clone();
    let job = move || {
        let polled = folder_watch::poll_dirs(&dirs, &prev, &c);
        let _ = tx.send(Msg::Polled(polled));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
    let library = folder_watch::library_token(&app.session);
    let guard = app.session.activity.start("folderPoll", "Checking folders for new photos", Cancel::Flag(cancel.clone()));
    app.folder_poll.last = Some((Instant::now(), library.clone()));
    app.folder_poll.task = Some(PollTask { rx, cancel, library, scanning: 0, found: Vec::new(), scope: Default::default(), guard });
}

/// Called every frame: start a poll when it is due (`came_forward`: the app was just brought to
/// the front), take in what a running one found, and add waiting new files.
pub fn tick(app: &mut LccApp, ctx: &egui::Context, came_forward: bool) {
    collect(app, ctx);
    add_waiting(app, ctx);
    let prefs = app.session.folder_watch;
    if !prefs.poll || app.folder_poll.task.is_some() || cfg!(target_arch = "wasm32") {
        return;
    }
    let library = folder_watch::library_token(&app.session);
    let due = match &app.folder_poll.last {
        // a library just opened
        None => true,
        Some((_, l)) if !Arc::ptr_eq(l, &library) => true,
        Some((at, _)) => came_forward || at.elapsed() >= Duration::from_secs(u64::from(prefs.interval_minutes) * 60),
    };
    if due {
        start(app);
    } else {
        // (wake up for the next poll)
        ctx.request_repaint_after(Duration::from_secs(30));
    }
}

fn collect(app: &mut LccApp, ctx: &egui::Context) {
    let Some(task) = app.folder_poll.task.as_mut() else { return };
    ctx.request_repaint_after(Duration::from_millis(200));
    if task.guard.is_cancelled() || !Arc::ptr_eq(&task.library, &folder_watch::library_token(&app.session)) {
        app.folder_poll.task = None;
        return;
    }
    let mut msgs = Vec::new();
    let ended = loop {
        match task.rx.try_recv() {
            Ok(m) => msgs.push(m),
            Err(TryRecvError::Empty) => break false,
            Err(TryRecvError::Disconnected) => break true,
        }
    };
    for m in msgs {
        match m {
            Msg::Polled(polled) => scan_changed(app, polled),
            Msg::Scanned(changes) => took_scan(app, changes),
        }
    }
    let Some(task) = app.folder_poll.task.as_mut() else { return };
    if ended && task.scanning == 0 {
        let found = std::mem::take(&mut task.found);
        app.folder_poll.task = None;
        notice(app, ctx, &found);
    }
}

/// Keep the listings, then scan the folders that changed, one after the other on a worker.
fn scan_changed(app: &mut LccApp, polled: Polled) {
    let changed = polled.changed.clone();
    let narrow = polled.narrow.clone();
    app.session.folder_stamps = polled.stamps;
    if let Err(e) = app.session.save_folder_stamps() {
        log::warn!("folder poll: {e}");
    }
    let mut inputs = Vec::new();
    for target in folder_watch::scan_targets(&changed) {
        match SyncInput::new(&mut app.session, &target, false) {
            Ok(i) => inputs.push(i),
            Err(e) => log::info!("folder poll: {target}: {e}"),
        }
    }
    let Some(task) = app.folder_poll.task.as_mut() else { return };
    if inputs.is_empty() {
        return;
    }
    task.scanning = inputs.len();
    task.scope = (changed, narrow);
    let (tx, rx) = std::sync::mpsc::channel();
    task.rx = rx;
    let cancel = task.cancel.clone();
    let job = move || {
        for input in inputs {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let mut progress = SyncProgress::default();
            // (✕ on the poll's row stops the scan midway)
            progress.files.cancel = cancel.clone();
            let changes = scan_with(input, &progress);
            if tx.send(Msg::Scanned(changes)).is_err() {
                return;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
}

fn took_scan(app: &mut LccApp, mut changes: FolderChanges) {
    if let Some(task) = app.folder_poll.task.as_mut() {
        task.scanning = task.scanning.saturating_sub(1);
        folder_watch::keep_new_in_scope(&mut changes, &task.scope.0, &task.scope.1);
    }
    if changes.offline {
        return;
    }
    let n = lcc_engine::folder_watch::record_scan(
        &mut app.session,
        &changes.path,
        changes.new.len(),
        &app.folder_poll.task.as_ref().map(|t| t.scope.1.clone()).unwrap_or_default(),
    );
    if n == 0 {
        return;
    }
    if let Some(task) = app.folder_poll.task.as_mut() {
        task.found.push((changes.path.clone(), n));
    }
    if app.session.folder_watch.new_files == NewFilePolicy::Add {
        app.folder_poll.to_add.push(changes);
    }
}

/// Add the waiting folders' new files, one folder at a time, when nothing else synchronizes or
/// imports (and no Synchronize Folder dialog holds a scan).
fn add_waiting(app: &mut LccApp, ctx: &egui::Context) {
    if app.folder_poll.to_add.is_empty() || app.sync_run.is_some() || app.import.is_some() || app.sync.is_some() || app.sync_owns_changes {
        return;
    }
    let changes = app.folder_poll.to_add.remove(0);
    let path = changes.path.clone();
    // (only while the scan is current: the folder's photos unchanged since)
    app.session.folder_changes = Some(changes);
    let Some(changes) = app.session.take_folder_changes(&path) else {
        log::info!("folder poll: {path} changed since it was scanned; the next poll looks again");
        return;
    };
    match SyncJob::start(&mut app.session, changes, folder_watch::only_new()) {
        Ok((work, commit)) => crate::sync::start_run(app, lcc_catalog::folders::folder_label(&path), work, commit),
        Err(e) => app.toast(ctx, format!("{path}: {e}")),
    }
}

/// Follow one of the app's links (`lcc_engine::link`). `sync` opens Synchronize Folder for its
/// folder, which scans in the background and shows what it would do: a link can come from
/// anywhere, so nothing changes in the library without the person's say (the CLI's `open-url`
/// runs `folder.synchronize` directly).
pub fn follow_link(app: &mut LccApp, ctx: &egui::Context, url: &str) {
    let r = lcc_engine::link::parse(url).and_then(|l| match l.command {
        "folder.synchronize" => {
            let path = l.params["path"].as_str().unwrap_or_default().to_string();
            crate::sync::open(app, &path, &lcc_catalog::folders::folder_label(&path), false)
        }
        other => Err(format!("unsupported link command `{other}`")),
    });
    if let Err(e) = r {
        app.toast_error(ctx, e);
    }
}

/// Say what a poll found (new files: ask).
fn notice(app: &mut LccApp, ctx: &egui::Context, found: &[(String, usize)]) {
    if found.is_empty() || app.session.folder_watch.new_files != NewFilePolicy::Ask {
        return;
    }
    let total: usize = found.iter().map(|(_, n)| n).sum();
    let text = match found {
        [(path, n)] => crate::i18n::tr_format!(
            "{n} new photos in “{folder}”: right-click the folder ▸ Add New Photos",
            n = n,
            folder = lcc_catalog::folders::folder_label(path)
        ),
        _ => crate::i18n::tr_format!("{n} new photos in {folders} folders: see the badges in Folders", n = total, folders = found.len()),
    };
    app.toast_for(ctx, text, 8.0);
}
