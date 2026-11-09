//! Find Similar Photos in the app. The first search reads every photo of the library, which on the
//! UI thread left the window not responding for as long as that took: hours in a library of tens of
//! thousands of raws (issue #3468). The photos are read on a worker thread instead, under a row in
//! the activity stack that counts them and can stop. What was read before a stop is kept, so the
//! next search goes on from there.
//!
//! From a menu the command answers at once and says how it ended in a toast. A request from the
//! control channel (an agent, `mcp --connect`) gets the same `{photos}` as ever, when the search
//! ends; one that gave up waiting can ask again and is answered from what was read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use lightcraft_engine::cmd::similar;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::control::{self, Outcome};

/// The activity stack's label (the menu item's name, already translated).
pub const LABEL: &str = "Find Similar Photos";

/// How a search ended.
enum End {
    /// The command's answer.
    Found(Value),
    /// Nothing was asked of the view any more: why.
    Dropped(&'static str),
    Failed(String),
}

/// Start a search for `p`: `end` runs on the UI thread when it is over. How many contents it has to
/// read, or why it can't start.
fn start(app: &mut LightcraftApp, p: &Value, end: impl FnOnce(&mut LightcraftApp, &egui::Context, End) + Send + 'static) -> Result<usize, String> {
    similar::no_search_running(&app.session).map_err(|why| format!("library.findSimilar: {why}"))?;
    let (search, reads) = similar::plan(&mut app.session, p).map_err(|e| e.to_string())?;
    let count = reads.len();
    let library = app.session.library_generation();
    let cancel = Arc::new(AtomicBool::new(false));
    let (flag, stopped) = (cancel.clone(), cancel.clone());
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        let total = count as u64;
        task.progress(0, total);
        lightcraft_engine::guard::catch(LABEL, || similar::measure(reads, &flag, &|done| task.progress(done as u64, total)))
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, found: Result<Vec<similar::Signature>, String>| {
        let ended = match found {
            _ if app.session.library_generation() != library => End::Dropped("another library was opened before the search ended"),
            Err(e) => End::Failed(e),
            Ok(found) => {
                similar::keep(&mut app.session, found);
                if stopped.load(Ordering::Relaxed) {
                    End::Dropped("the search was stopped")
                } else {
                    match app.session.execute_fn("library.findSimilar", |s| similar::apply(s, &search)) {
                        Ok(answer) => End::Found(answer),
                        Err(e) => End::Failed(e.to_string()),
                    }
                }
            }
        };
        end(app, ctx, ended);
    };
    crate::tasks::spawn_tracked(app, LABEL, similar::KIND, cancel, work, done)?;
    Ok(count)
}

/// `library.findSimilar` from a menu: start reading → `{background: true, count}`. The grid shows
/// what it found, and a toast says how many.
pub fn command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let count = start(app, p, |app, ctx, end| match end {
        End::Found(answer) => {
            let n = answer["photos"].as_array().map_or(0, Vec::len);
            app.ui.view = crate::state::ViewMode::PhotoGrid;
            app.toast(ctx, crate::i18n::tr_format!("{n} similar photo{} · View ▸ Clear Filters to see all", if n == 1 { "" } else { "s" }, n = n));
        }
        // (whoever stopped it knows; a search of another library has nothing to say about this one)
        End::Dropped(_) => {}
        End::Failed(e) => app.toast(ctx, e),
    })?;
    Ok(json!({"background": true, "count": count}))
}

/// `library.findSimilar` from the control channel: `reply` gets the command's answer when the search
/// ends, and the view stays where the agent left it.
pub fn ask(app: &mut LightcraftApp, p: &Value, reply: Sender<Value>) -> Outcome {
    let started = start(app, p, move |_, _, end| {
        let answer = match end {
            End::Found(answer) => control::success(answer),
            End::Dropped(why) => control::failure(format!("library.findSimilar: {why}")),
            End::Failed(e) => control::failure(e),
        };
        // (a caller that gave up has no one to send to; what was read stays known for its next request)
        let _ = reply.send(answer);
    });
    match started {
        Ok(_) => Outcome::Later,
        Err(e) => Outcome::Done(control::failure(e)),
    }
}

/// Another library was opened: stop a search of the one before, which has nothing left to find (it
/// could go on reading for hours). Its row goes when the photo being read is done.
pub fn library_changed(app: &LightcraftApp) {
    for row in app.session.activity.list().into_iter().filter(|r| r.kind == similar::KIND) {
        // (a row that has just ended can't be stopped: nothing to do)
        let _ = app.session.activity.cancel(row.id);
    }
}
