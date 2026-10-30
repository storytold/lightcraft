//! Move Files to Trash at work in the background (see `lightcraft_engine::trash_job`): once the
//! question is answered it closes at once, and the files move on worker threads under a row in
//! the activity stack, whose ✕ stops the work after the file in hand.
//!
//! First each folder's disk is asked whether it has a Trash. When some say they have none (macOS
//! can tell), the Erase question comes up before any file moves, and the answer goes with the rest
//! as one undo step; Cancel there keeps those photos (and does not ask again). A disk found to
//! have no Trash only as its files are tried (Linux) is asked about after the run
//! (`trash_files::report`). The library takes the photos out between frames, on this thread, when
//! the files have moved.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::trash_job::{Survey, TrashPlan, TrashWork};

use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::panels::trash_files::TrashAsk;
use crate::state::Dialog;

const COMMAND: &str = "photo.trashFiles";
/// What a panic on a worker thread is reported as.
const WHAT: &str = "move files to the Trash";

/// A Move Files to Trash at work.
pub struct TrashRun {
    stage: Stage,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// Its row in the activity stack (its ✕ sets `cancel`).
    guard: TaskGuard,
    /// The library it was started in (`Session::library_generation`).
    library: u64,
    /// How many photos with a file it is about, for the "Stopped" count.
    planned: usize,
    /// Some: a run the Erase question asked for after another run (these ids may be erased).
    erase: Option<Vec<u64>>,
    /// The photos asked about before anything moved: not asked about again afterwards.
    asked: Vec<u64>,
    /// Another library was opened: whatever the worker hands over is dropped or put back.
    stopping: bool,
}

enum Stage {
    /// The disks are asked whether they have a Trash.
    Surveying,
    /// Some have none: the Erase question is up (or waits for the window to be free).
    Asking { plan: Box<TrashPlan>, survey: Survey, ids: Vec<u64>, shown: bool, answer: Option<bool> },
    /// The files are moving.
    Working,
}

enum Msg {
    Surveyed(TrashPlan, Survey),
    Worked(TrashWork),
    /// A panic on the worker thread, before any file moved.
    Crashed(String),
}

impl TrashRun {
    /// The Erase question is up: nothing runs until it is answered.
    pub fn waiting_for_answer(&self) -> bool {
        matches!(self.stage, Stage::Asking { .. })
    }
}

impl Drop for TrashRun {
    /// A dropped run stops its worker after the file in hand.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Run `job` on a worker thread named `name` (inline in the browser, which has none).
fn spawn(name: &str, job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new().name(name.into()).spawn(job).map(|_| ())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        job();
        Ok(())
    }
}

/// Start moving the files of photos `ids` to the Trash (`erase`: these may be erased where
/// there is no Trash, as the Erase question asked after another run). Refused, with the
/// command's words, when there is nothing to move or another run is at work.
pub fn start(app: &mut LightcraftApp, ids: Vec<u64>, erase: Option<Vec<u64>>) -> Result<(), String> {
    let photos: Vec<PhotoId> = ids.iter().map(|i| PhotoId(*i)).collect();
    let plan = TrashPlan::new(&app.session, &photos, COMMAND).map_err(|e| e.to_string())?;
    let planned = plan.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = app.session.activity.start("trash", "Moving files to the Trash", Cancel::Flag(cancel.clone()));
    guard.detail(tr("Checking disks"));
    let (tx, rx) = std::sync::mpsc::channel();
    let (handle, stop) = (guard.handle(), cancel.clone());
    let job = move || {
        let msg = match lightcraft_engine::guard::catch(WHAT, || {
            let survey = plan.survey(&stop, Some(&handle));
            (plan, survey)
        }) {
            Ok((plan, survey)) => Msg::Surveyed(plan, survey),
            // (the plan went with the panic, and its lease with it)
            Err(why) => Msg::Crashed(why),
        };
        let _ = tx.send(msg);
    };
    if spawn("lc-trash-survey", job).is_err() {
        return Err(tr("Could not start moving files to the Trash").to_string());
    }
    let library = app.session.library_generation();
    app.trash_run = Some(TrashRun { stage: Stage::Surveying, rx, cancel, guard, library, planned, erase, asked: Vec::new(), stopping: false });
    Ok(())
}

/// The files move now: `erase` may be erased where their disk has no Trash.
fn start_work(app: &mut LightcraftApp, ctx: &egui::Context, plan: TrashPlan, survey: Survey, erase: &[u64]) {
    let Some(run) = app.trash_run.as_mut() else { return };
    run.stage = Stage::Working;
    run.guard.detail("");
    let (tx, rx) = std::sync::mpsc::channel();
    run.rx = rx;
    let (handle, stop) = (run.guard.handle(), run.cancel.clone());
    let erase: HashSet<PhotoId> = erase.iter().map(|i| PhotoId(*i)).collect();
    let job = move || {
        let msg = match lightcraft_engine::guard::catch(WHAT, || plan.work_real(survey, &erase, &stop, Some(&handle))) {
            Ok(work) => Msg::Worked(work),
            Err(why) => Msg::Crashed(why),
        };
        let _ = tx.send(msg);
    };
    if spawn("lc-trash-work", job).is_err() {
        app.trash_run = None;
        app.toast_for(ctx, tr("Could not start moving files to the Trash"), 12.0);
    }
}

/// The Erase question's OK, for a run that asked it: those files may be erased. `false`: the
/// question is not this run's (a run asks it after its work instead).
pub fn answer_erase(app: &mut LightcraftApp, ids: &[u64]) -> bool {
    let Some(TrashRun { stage: Stage::Asking { ids: asked, shown: true, answer, .. }, .. }) = app.trash_run.as_mut() else { return false };
    if asked.as_slice() != ids {
        return false;
    }
    *answer = Some(true);
    true
}

/// Whether the dialog up is an Erase question.
fn erase_question_up(app: &LightcraftApp) -> bool {
    matches!(app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. })))
}

fn say_stopped(app: &mut LightcraftApp, ctx: &egui::Context, n: usize) {
    app.toast_for(ctx, tr_format!("Stopped: photos not reached stay in the library: {n}", n = n), 12.0);
}

/// Ask the Erase question a finished run left (`trash_erase_later`) once no other dialog is up
/// and no other run is at work (its OK starts a run of its own); dropped when another library was
/// opened meanwhile (those ids are other photos there).
pub fn ask_erase_later(app: &mut LightcraftApp) {
    if app.ui.dialog.is_some() || app.trash_run.is_some() {
        return;
    }
    let Some((mut ids, library)) = app.trash_erase_later.take() else { return };
    // (a run since may have taken some of them out)
    ids.retain(|id| app.session.catalog.photo(PhotoId(*id)).is_some());
    if library == app.session.library_generation() && !ids.is_empty() {
        let since = app.now();
        app.ui.dialog = Some(Dialog::TrashFiles(TrashAsk::Erase { ids, since, library }));
    }
}

/// Follow the run (called every frame): hand what the workers did to the library, ask the
/// Erase question, and say how it went.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    ask_erase_later(app);
    let generation = app.session.library_generation();
    let Some(run) = app.trash_run.as_mut() else { return };
    ctx.request_repaint_after(Duration::from_millis(100));
    // another library was opened: these ids are other photos there
    if run.library != generation && !run.stopping {
        run.cancel.store(true, Ordering::Relaxed);
        if let Stage::Asking { shown, .. } = run.stage {
            app.trash_run = None;
            if shown && erase_question_up(app) {
                app.ui.dialog = None;
            }
            app.toast_for(ctx, tr("Another library was opened since the question came up: nothing was moved to the Trash or erased"), 12.0);
            return;
        }
        // (the worker hands over what it has after the file in hand)
        run.stopping = true;
    }
    // the row's ✕ while the question is up: nothing moved
    if matches!(run.stage, Stage::Asking { .. }) && run.cancel.load(Ordering::Relaxed) {
        let (planned, shown) = (run.planned, matches!(run.stage, Stage::Asking { shown: true, .. }));
        app.trash_run = None;
        if shown && erase_question_up(app) {
            app.ui.dialog = None;
        }
        say_stopped(app, ctx, planned);
        return;
    }
    if !matches!(run.stage, Stage::Asking { .. }) {
        let msg = match run.rx.try_recv() {
            Ok(msg) => msg,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Msg::Crashed(format!("{WHAT} failed unexpectedly")),
        };
        received(app, ctx, msg);
        return;
    }
    let now = app.now();
    let up = erase_question_up(app);
    let free = app.ui.dialog.is_none();
    let Some(run) = app.trash_run.as_mut() else { return };
    let library = run.library;
    let Stage::Asking { ids, shown, answer, .. } = &mut run.stage else { return };
    if !*shown {
        if free {
            app.ui.dialog = Some(Dialog::TrashFiles(TrashAsk::Erase { ids: ids.clone(), since: now, library }));
            *shown = true;
        }
        return;
    }
    let erase = match answer {
        Some(true) => ids.clone(),
        // Cancel, Escape, or another dialog took its place: those photos stay
        _ if !up => Vec::new(),
        _ => return,
    };
    let Stage::Asking { plan, survey, .. } = std::mem::replace(&mut run.stage, Stage::Working) else { return };
    start_work(app, ctx, *plan, survey, &erase);
}

fn received(app: &mut LightcraftApp, ctx: &egui::Context, msg: Msg) {
    let Some(run) = app.trash_run.as_mut() else { return };
    match msg {
        Msg::Surveyed(mut plan, survey) => {
            if run.stopping {
                app.trash_run = None;
                app.toast_for(ctx, tr("Another library was opened since the question came up: nothing was moved to the Trash or erased"), 12.0);
                return;
            }
            if run.cancel.load(Ordering::Relaxed) {
                let planned = run.planned;
                app.trash_run = None;
                say_stopped(app, ctx, planned);
                return;
            }
            if let Err(e) = plan.resolve(&app.session, &survey) {
                app.trash_run = None;
                app.toast_for(ctx, e.to_string(), 12.0);
                return;
            }
            if let Some(erase) = run.erase.clone() {
                start_work(app, ctx, plan, survey, &erase);
                return;
            }
            let ids = survey.no_trash_ids(&plan);
            if ids.is_empty() {
                start_work(app, ctx, plan, survey, &[]);
                return;
            }
            run.asked = ids.clone();
            run.guard.detail(tr("Waiting for an answer"));
            run.stage = Stage::Asking { plan: Box::new(plan), survey, ids, shown: false, answer: None };
        }
        Msg::Worked(work) => {
            let Some(run) = app.trash_run.take() else { return };
            if run.stopping {
                let note = work.put_back();
                let text = tr("Another library was opened while files were moving to the Trash: they were put back");
                app.toast_for(ctx, note.map_or_else(|| text.to_string(), |n| format!("{text} ({n})")), 12.0);
                return;
            }
            let erasing = run.erase.is_some();
            match app.session.execute_fn(COMMAND, |s| work.finish(s, COMMAND)) {
                Ok(r) => crate::panels::trash_files::report(app, &r, &run.asked, run.library, erasing),
                Err(e) => app.toast_for(ctx, e.to_string(), 12.0),
            }
        }
        Msg::Crashed(why) => {
            app.trash_run = None;
            app.toast_for(ctx, why, 12.0);
        }
    }
}

/// Quitting: stop the run after the file in hand and take out of the library the photos whose
/// files moved, waiting at most `timeout` for the worker.
pub fn settle_for_quit(app: &mut LightcraftApp, timeout: Duration) {
    let Some(run) = app.trash_run.take() else { return };
    run.cancel.store(true, Ordering::Relaxed);
    // (surveying or asking: nothing has moved, and the plan goes with the run or its worker)
    if !matches!(run.stage, Stage::Working) {
        return;
    }
    match run.rx.recv_timeout(timeout) {
        Ok(Msg::Worked(work)) => {
            if run.stopping || run.library != app.session.library_generation() {
                work.put_back();
            } else if let Err(e) = app.session.execute_fn(COMMAND, |s| work.finish(s, COMMAND)) {
                log::error!("quit: the photos whose files moved to the Trash could not be taken out of the library: {e}");
            }
        }
        Ok(Msg::Crashed(why)) => log::error!("quit: {why}"),
        Ok(Msg::Surveyed(..)) => {}
        Err(_) => log::error!(
            "quit: the Trash job did not stop within {timeout:?}; files moved so far stay in the Trash or are erased, and their photos show as missing"
        ),
    }
}
