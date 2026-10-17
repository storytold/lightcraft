//! Reload from Disk in the app: the files are read on a worker thread (a raw's decode takes a
//! second or more, and a selection of them froze the window), with a row in the activity stack
//! that counts the files and can stop. What came of it is applied back here, as one undo step,
//! unless another library was opened meanwhile. Agents get the same with `wait: true`, which runs
//! the engine's command in place.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_engine::cmd::reload;
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The activity stack's label while the files are read (the menu item's name, already translated).
pub const LABEL: &str = "Reload from Disk";

/// `photo.reload` from the app: start reading the targeted files in the background →
/// `{background: true, count}`. `None` hands the command to the engine: with `wait: true`, or when
/// no targeted photo has a file (it then answers at once, refusing an empty selection as usual).
pub fn command(app: &mut LightcraftApp, p: &Value) -> Option<Result<Value, String>> {
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let items = match reload::plan(&app.session, p) {
        Ok(items) => items,
        Err(e) => return Some(Err(format!("photo.reload: {e}"))),
    };
    if items.is_empty() {
        return None;
    }
    let count = items.len();
    // the photos shown from their embedded preview, to say afterwards whether they decode now
    let preview_only: Vec<_> =
        items.iter().filter(|i| app.session.catalog.photo(i.id).is_some_and(|ph| ph.preview_only.is_some())).map(|i| i.id).collect();
    let library = app.session.library_generation();
    let probe = reload::file_probe(&app.session);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        let total = items.len() as u64;
        task.progress(0, total);
        let results = reload::probe(probe.as_ref(), &items, &flag, &|done| task.progress(done as u64, total));
        (items, results)
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, (items, results)| {
        if app.session.library_generation() != library {
            return;
        }
        if let Err(e) = reload::apply(&mut app.session, items, results) {
            return app.toast_error(ctx, e.to_string());
        }
        let cleared = preview_only.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|ph| ph.preview_only.is_none())).count();
        let message = match (count, preview_only.len(), cleared) {
            (1, 1, 1) => crate::i18n::tr("This raw is now developed from its raw data").to_string(),
            (1, 1, _) => crate::i18n::tr("This raw variant still can't be decoded").to_string(),
            (_, _, 0) => return,
            (_, _, 1) => crate::i18n::tr("1 raw can now be developed from its raw data").to_string(),
            (_, _, n) => crate::i18n::tr_format!("{n} raws can now be developed from their raw data", n = n),
        };
        app.toast(ctx, message);
    };
    if let Err(e) = crate::tasks::spawn_tracked(app, LABEL, "reload", cancel, work, done) {
        return Some(Err(e));
    }
    Some(Ok(json!({"background": true, "count": count})))
}
