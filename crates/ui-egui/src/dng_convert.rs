//! Nikon High Efficiency NEF through Adobe DNG Converter in the app (issue #3921): the conversion runs on a worker
//! thread with a row in the activity stack (`dngConvert`, cancellable), and what came of it is applied back here as
//! Reload from Disk is ([`crate::reload`]). The Preview only notice says what is happening: converting, why a
//! conversion failed, or how to get the converter. Agents get the engine's command with `wait: true`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::EngineError;
use lightcraft_engine::cmd::dng_convert;
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The activity stack's label while files are converted.
pub const LABEL: &str = "Converting with Adobe DNG Converter";

/// What the app knows about conversions this session.
#[derive(Debug, Default)]
pub struct DngState {
    /// Photos being converted now.
    pub running: BTreeSet<PhotoId>,
    /// Photos waiting for the conversion running now to finish (one batch at a time: the converter is a heavy
    /// program, and several batches would start [`lightcraft_engine::dng_converter::PARALLEL`] copies each).
    pub queue: Vec<PhotoId>,
    /// Why a photo's last conversion failed.
    pub failed: BTreeMap<PhotoId, String>,
    /// Photos converted automatically once already (opened in Edit with Settings ▸ Performance ▸ Convert … when opened):
    /// a failure isn't retried on every frame.
    pub tried: BTreeSet<PhotoId>,
}

/// What the Preview only notice shows for a High Efficiency NEF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Being converted.
    Converting,
    /// Waiting for the conversion running now.
    Queued,
    /// The last conversion failed, and why.
    Failed(String),
    /// The converter is installed: it can be run.
    Ready,
    /// No converter is installed (or none where the setting says).
    NotFound,
}

/// The converter this app's settings point to (Settings ▸ Performance), if it is installed. The app's own settings,
/// not the engine's process-wide copy of them ([`lightcraft_engine::dng_converter::settings`], which the app keeps in
/// step), so what the notice offers and what runs are the same.
pub fn converter(app: &LightcraftApp) -> Option<std::path::PathBuf> {
    use lightcraft_engine::dng_converter as dc;
    let set = app.ui.settings.dng_converter_path.trim();
    dc::resolve((!set.is_empty()).then(|| std::path::Path::new(set)), &dc::default_locations(), std::path::Path::is_file)
}

/// The notice's status for photo `id`; `None` when it isn't a High Efficiency NEF shown from its preview.
pub fn status(app: &LightcraftApp, id: PhotoId) -> Option<Status> {
    let p = app.session.catalog.photo(id)?;
    if !dng_convert::is_candidate(p) {
        return None;
    }
    Some(if app.dng.running.contains(&id) {
        Status::Converting
    } else if app.dng.queue.contains(&id) {
        Status::Queued
    } else if let Some(e) = app.dng.failed.get(&id) {
        Status::Failed(e.clone())
    } else if converter(app).is_some() {
        Status::Ready
    } else {
        Status::NotFound
    })
}

/// Start converting photo `id`, open in Edit, when the user turned automatic conversion on (once a session; a
/// photo merely selected in the grid isn't converted).
pub fn auto_convert(app: &mut LightcraftApp, ctx: &egui::Context, id: PhotoId) {
    if !app.ui.settings.dng_converter_auto || app.dng.tried.contains(&id) {
        return;
    }
    if !matches!(status(app, id), Some(Status::Ready)) {
        return;
    }
    app.dng.tried.insert(id);
    convert(app, ctx, id);
}

/// Convert photo `id` now (the notice's button), or after the conversion running now; says so when it can't start.
pub fn convert(app: &mut LightcraftApp, ctx: &egui::Context, id: PhotoId) {
    if app.dng.running.contains(&id) || app.dng.queue.contains(&id) {
        return;
    }
    if !app.dng.running.is_empty() {
        app.dng.failed.remove(&id);
        app.dng.queue.push(id);
        return;
    }
    match command(app, &json!({"ids": [id.0]})) {
        Some(Err(e)) => app.toast_error(ctx, e),
        None => {
            // nothing to convert here: the engine says why
            if let Err(e) = app.run("photo.convertHighEfficiency", json!({"ids": [id.0], "wait": true})) {
                app.toast_error(ctx, e);
            }
        }
        Some(Ok(_)) => {}
    }
}

/// The next queued photo that still needs converting, once nothing runs.
fn start_queued(app: &mut LightcraftApp, ctx: &egui::Context) {
    while app.dng.running.is_empty() && !app.dng.queue.is_empty() {
        let id = app.dng.queue.remove(0);
        if app.session.catalog.photo(id).is_some_and(|p| dng_convert::is_candidate(p)) {
            convert(app, ctx, id);
        }
    }
}

/// `photo.convertHighEfficiency` from the app: start converting the targeted High Efficiency NEFs in the background →
/// `{background: true, count}`. `None` hands the command to the engine: with `wait: true`, or when nothing targeted
/// can be converted (it then answers with the reason).
pub fn command(app: &mut LightcraftApp, p: &Value) -> Option<Result<Value, String>> {
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let planned = match dng_convert::plan(&mut app.session, p) {
        Ok(planned) => planned,
        Err(EngineError::BadParams { cmd, msg }) if cmd.is_empty() => {
            return Some(Err(EngineError::BadParams { cmd: "photo.convertHighEfficiency".into(), msg }.to_string()));
        }
        Err(e) => return Some(Err(e.to_string())),
    };
    let planned: Vec<_> = planned.into_iter().filter(|p| !app.dng.running.contains(&p.item.id)).collect();
    if planned.is_empty() {
        return None;
    }
    let converter = match converter(app) {
        Some(c) => c,
        None => {
            return Some(Err(dng_convert::converter_or_err()
                .err()
                .map_or_else(|| "Adobe DNG Converter was not found".to_string(), |e| e.to_string())));
        }
    };
    let count = planned.len();
    let ids: Vec<PhotoId> = planned.iter().map(|p| p.item.id).collect();
    for id in &ids {
        app.dng.running.insert(*id);
        app.dng.failed.remove(id);
        app.dng.queue.retain(|q| q != id);
    }
    let library = app.session.library_generation();
    let probe = lightcraft_engine::cmd::reload::file_probe(&app.session);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        let total = planned.len() as u64;
        task.progress(0, total);
        let outcome = dng_convert::run(&converter, probe.as_ref(), &planned, &flag, &|done| task.progress(done as u64, total));
        (planned, outcome)
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, (planned, outcome): (Vec<dng_convert::Planned>, dng_convert::Outcome)| {
        for p in &planned {
            app.dng.running.remove(&p.item.id);
        }
        if app.session.library_generation() != library {
            app.dng.queue.clear();
            return;
        }
        for (p, r) in planned.iter().zip(&outcome.converted) {
            if let Err(e) = r {
                app.dng.failed.insert(p.item.id, e.clone());
            }
        }
        let r = app.session.execute_fn("photo.convertHighEfficiency", |s| dng_convert::apply(s, planned, outcome));
        match r {
            Err(e) => app.toast_error(ctx, e.to_string()),
            Ok(v) => {
                let converted = v["converted"].as_array().map_or(0, Vec::len);
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let message = match (converted, failed) {
                    (0, _) => crate::i18n::tr("Adobe DNG Converter could not convert this raw: it still shows its embedded preview").to_string(),
                    (1, 0) => crate::i18n::tr("This raw is now developed from its raw data").to_string(),
                    (n, 0) => crate::i18n::tr_format!("{n} raws can now be developed from their raw data", n = n),
                    (n, f) => crate::i18n::tr_format!("{n} raws converted, {f} failed", n = n, f = f),
                };
                if converted == 0 { app.toast_error(ctx, message) } else { app.toast(ctx, message) }
            }
        }
        start_queued(app, ctx);
    };
    let label = crate::i18n::tr(LABEL).to_string();
    if let Err(e) = crate::tasks::spawn_tracked(app, &label, "dngConvert", cancel, work, done) {
        for id in ids {
            app.dng.running.remove(&id);
        }
        return Some(Err(e));
    }
    Some(Ok(json!({"background": true, "count": count})))
}

/// Settings ▸ Performance ▸ Converted DNGs ▸ Delete: the converted DNGs are deleted, and this library's photos that developed from them are
/// read again in the background (Reload from Disk), so they show their embedded preview.
pub fn clear_cache(app: &mut LightcraftApp, ctx: &egui::Context) {
    match app.run("library.clearDngCache", json!({"reload": false})) {
        Err(e) => app.toast_error(ctx, e),
        Ok(r) => {
            let ids = r["converted"].clone();
            if ids.as_array().is_some_and(|a| !a.is_empty())
                && let Err(e) = app.run("photo.reload", json!({"ids": ids}))
            {
                app.toast_error(ctx, e);
            }
        }
    }
}

#[cfg(test)]
mod tests;
