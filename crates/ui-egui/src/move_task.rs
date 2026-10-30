//! Moving photos to another folder in the app (a drop on a Folders row, Move Selected Photos Here,
//! Move to Folder…): the files move on a worker thread, so a large move to a slow NAS doesn't
//! freeze the window. The activity stack shows it ("Moving photos", k of N, the file in hand) with
//! ✕, which stops it after the file in hand: what moved stays moved, as one undo step. The library
//! follows the files between frames ([`poll`]); see `lightcraft_engine::move_job`.

use std::sync::mpsc::{Receiver, TryRecvError};

use lightcraft_engine::move_job::{MoveFinish, MoveOutcome};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// A move at work: its files on a worker thread, the library's half here.
pub struct MoveRun {
    rx: Receiver<MoveOutcome>,
    finish: Option<MoveFinish>,
    folder: String,
    /// The folder's name, as the toast says it.
    name: String,
    /// The worker, so a test can wait until it has sent its result.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) worker: Option<std::thread::JoinHandle<()>>,
}

impl MoveRun {
    /// `ui.inspect` → `move.running`.
    pub fn status(&self) -> Value {
        json!({"folder": self.folder, "files": self.finish.as_ref().map(MoveFinish::files), "task": self.finish.as_ref().map(MoveFinish::task_id)})
    }
}

/// A move that never ends, with its row, for tests of what the window does while one runs: the
/// sender keeps the worker alive, the guard keeps the row.
#[cfg(test)]
pub(crate) fn stalled(app: &mut LightcraftApp) -> (std::sync::mpsc::Sender<MoveOutcome>, lightcraft_engine::activity::TaskGuard) {
    let (tx, rx) = std::sync::mpsc::channel();
    let guard = app.session.activity.start("move", "Moving photos", lightcraft_engine::activity::Cancel::Yes);
    app.move_run = Some(MoveRun {
        rx,
        finish: None,
        folder: String::new(),
        name: String::new(),
        #[cfg(all(test, not(target_arch = "wasm32")))]
        worker: None,
    });
    (tx, guard)
}

/// The name a toast gives the folder `path`.
fn name_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    std::path::Path::new(trimmed).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Start moving photos into a folder on disk: `params` as `photo.moveToFolder` takes them
/// (`{folder, ids?}`; the selection by default). Returns at once: `{background: true, files}`
/// while it runs, or `{moved: 0, of: 0, folder, stopped: false}` when every photo is there
/// already (said in a toast). An error when the move can't begin (Local photos, another move
/// running…), also logged, shown in the status line as `LightcraftApp::run` does, and told in an error toast
/// (a menu, key or button caller drops the returned error, so the toast is what tells the person why nothing moved).
pub fn start(app: &mut LightcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    start_inner(app, ctx, params).inspect_err(|e| {
        log::warn!("photo.moveToFolder: {e}");
        app.ui.status = e.clone();
        app.toast_error(ctx, e.clone());
    })
}

fn start_inner(app: &mut LightcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    let raw = params.get("folder").and_then(Value::as_str).unwrap_or_default();
    let path = raw.trim_end_matches(['/', '\\']).to_string();
    let name = name_of(raw);
    let started = lightcraft_engine::move_job::start(&mut app.session, params, true).map_err(|e| e.to_string())?;
    let Some((work, finish)) = started else {
        app.toast(ctx, crate::panels::left::moved_text(0, &name));
        return Ok(json!({"moved": 0, "of": 0, "folder": path, "stopped": false}));
    };
    let files = finish.files();
    let (tx, rx) = std::sync::mpsc::channel();
    let repaint = app.tasks.repaint.clone().unwrap_or_else(|| ctx.clone());
    let job = move || {
        // (a panic here ends the worker: `poll` sees the channel closed and lets the library
        // follow whatever had moved, from the move's record)
        if let Ok(out) = lightcraft_engine::guard::catch("move to folder", || work.run()) {
            let _ = tx.send(out);
        }
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    #[cfg_attr(not(test), allow(unused_variables))]
    let worker = std::thread::Builder::new().name("lc-move".into()).spawn(job).map_err(|e| format!("can't start the move: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    job();
    app.move_run = Some(MoveRun {
        rx,
        finish: Some(finish),
        folder: path,
        name,
        #[cfg(all(test, not(target_arch = "wasm32")))]
        worker: Some(worker),
    });
    Ok(json!({"background": true, "files": files}))
}

/// Bring the library along once the files have moved (called every frame): one undo step, the
/// photos that left the view leave the selection, and a toast says how it went.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(run) = app.move_run.as_mut() else { return };
    let out = match run.rx.try_recv() {
        Ok(out) => Some(out),
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(TryRecvError::Disconnected) => None,
    };
    let Some(mut run) = app.move_run.take() else { return };
    let Some(finish) = run.finish.take() else { return };
    let result = match out {
        Some(out) => app.session.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).map_err(|e| e.to_string()),
        None => {
            // the worker died part-way (logged): the move is over; the library follows what moved
            let r = app.session.execute_fn("photo.moveToFolder", |s| finish.abandon(s)).map_err(|e| e.to_string());
            log::warn!("move to {}: stopped by an error; followed: {r:?}", run.folder);
            // (the worker only dies in a panic; the sidecars of the files that arrived are not checked)
            match r {
                Ok(_) => Err(crate::i18n::tr("Moving was stopped by an error (see the log). The library follows the files that had moved; an XMP sidecar may have stayed at the old place.").to_string()),
                Err(e) => Err(e),
            }
        }
    };
    // photos that left the view leave the selection, so nothing done next (Delete, an edit)
    // reaches photos no longer on screen
    let visible: std::collections::HashSet<lightcraft_catalog::PhotoId> = app.session.visible_cloned().into_iter().collect();
    let sel = &mut app.session.selection;
    sel.ids.retain(|id| visible.contains(id));
    if sel.active.is_some_and(|a| !visible.contains(&a)) {
        sel.active = sel.ids.first().copied();
    }
    match &result {
        Ok(r) => {
            let moved = r["moved"].as_u64().unwrap_or(0);
            let of = r["of"].as_u64().unwrap_or(moved);
            let text = if r["stopped"].as_bool() == Some(true) {
                stopped_text(moved, of, &run.name)
            } else {
                crate::panels::left::moved_text(moved, &run.name)
            };
            app.toast(ctx, text);
        }
        Err(e) => app.toast_for(ctx, e.clone(), 12.0),
    }
    app.last_move = Some(match result {
        Ok(r) => r,
        Err(e) => json!({"error": e, "folder": run.folder}),
    });
}

/// What a move stopped part-way says.
pub(crate) fn stopped_text(moved: u64, of: u64, name: &str) -> String {
    if moved == 0 {
        crate::i18n::tr("Stopped: no photo was moved").to_string()
    } else {
        crate::i18n::tr_format!("Stopped: moved {} of {} photos to “{}”. Edit → Undo moves them back.", moved, of, name)
    }
}

/// Wait for a running move and bring the library along (tests, and commands asked to wait).
/// `false` on timeout.
pub fn wait(app: &mut LightcraftApp, ctx: &egui::Context, timeout: std::time::Duration) -> bool {
    let t0 = web_time::Instant::now();
    loop {
        poll(app, ctx);
        if app.move_run.is_none() {
            return true;
        }
        if t0.elapsed() > timeout {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
