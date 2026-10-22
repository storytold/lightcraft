//! `library.missing` in the app. The engine's command looks at every photo's file on the calling
//! thread, which here is the UI's: on a share with hundreds of thousands of photos that froze the
//! window for minutes. A request from the control channel (and so an agent, `mcp --connect`) goes
//! the way of the person's own long jobs instead: the files are checked on a worker thread, under
//! a row in the activity stack that counts them and can stop the check, and the request is
//! answered with the same `[{id, path}]` once the check ends. A second request meanwhile joins the
//! check that is running and gets its answer, rather than starting another walk of the share.
//!
//! A check on a big share can outlast what a caller waits for (the control server gives a request
//! 60 s). An answer that nobody was waiting for any more is kept for a few minutes and given to
//! the next request, so that asking again after a timeout is not another walk of the share that
//! times out again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use web_time::Instant;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::cmd::missing;
use serde_json::Value;

use crate::LightcraftApp;
use crate::control::{self, Outcome};

/// The activity stack's label (the command's name, already translated).
pub const LABEL: &str = "Missing Photos";

/// The activity row's kind.
const KIND: &str = "missing";

/// How long an answer nobody was waiting for is kept.
const KEEP: Duration = Duration::from_secs(10 * 60);

/// The requests waiting for a check, and the answer kept for the next one.
#[derive(Default)]
pub struct State {
    /// Each request with the library ([`lightcraft_engine::Session::library_generation`]) it asked about.
    waiting: Vec<(u64, Sender<Value>)>,
    /// A finished check's answer that no request received: its library and when it was kept.
    kept: Option<(u64, Instant, Value)>,
}

/// Answer `library.missing` from the control channel: give the answer kept from a check nobody
/// waited for, or start the check, or join the one running for this library. The answer goes to
/// `reply` when the check ends.
pub fn ask(app: &mut LightcraftApp, reply: Sender<Value>) -> Outcome {
    let library = app.session.library_generation();
    if let Some((kept_for, at, answer)) = app.missing.kept.take()
        && kept_for == library
        && at.elapsed() < KEEP
    {
        return Outcome::Done(answer);
    }
    app.missing.waiting.push((library, reply));
    if app.tasks.is_running(LABEL) && app.missing.waiting.iter().filter(|(l, _)| *l == library).count() > 1 {
        return Outcome::Later;
    }
    let files = missing::checked(&app.session.catalog);
    let total = files.len() as u64;
    let exists = app.session.media.availability.probe();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        task.progress(0, total);
        // (a panic is this check's failure, answered to the callers: they would wait for it in vain)
        lightcraft_engine::guard::catch("Missing Photos", || {
            missing::sweep(files, |f| exists(f), || flag.load(Ordering::Relaxed), |done| task.progress(done as u64, total))
        })
    };
    let done = move |app: &mut LightcraftApp, _ctx: &egui::Context, checked: Result<Option<Vec<(PhotoId, String)>>, String>| {
        let (answer, complete) = match checked {
            _ if app.session.library_generation() != library => {
                (control::failure("Missing Photos: another library was opened before the check ended"), false)
            }
            Err(e) => (control::failure(e), false),
            Ok(None) => (control::failure("Missing Photos: the check was stopped"), false),
            Ok(Some(gone)) => (control::success(missing::missing_json(gone)), true),
        };
        let (theirs, others): (Vec<_>, Vec<_>) = std::mem::take(&mut app.missing.waiting).into_iter().partition(|(l, _)| *l == library);
        app.missing.waiting = others;
        // (a request whose caller gave up has no one to send to)
        let received = theirs.into_iter().fold(false, |received, (_, waiting)| waiting.send(answer.clone()).is_ok() || received);
        if complete && !received {
            app.missing.kept = Some((library, Instant::now(), answer));
        }
    };
    if let Err(e) = crate::tasks::spawn_tracked(app, LABEL, KIND, cancel, work, done) {
        for (_, waiting) in std::mem::take(&mut app.missing.waiting) {
            let _ = waiting.send(control::failure(&e));
        }
    }
    Outcome::Later
}
