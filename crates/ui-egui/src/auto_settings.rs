//! Auto Settings decodes a selection off the UI thread and commits it as one undo step.
use crate::LightcraftApp;
use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::media::{SourceLevel, SourceRef};
use lightcraft_pipeline::SourceInfo;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

const LABEL: &str = "Auto Settings";
struct Input {
    id: PhotoId,
    settings: Arc<DevelopSettings>,
    source: SourceRef,
    info: SourceInfo,
}
struct Prepared {
    library: u64,
    commits: u64,
    inputs: Vec<Input>,
}
struct Completed {
    library: u64,
    commits: u64,
    /// Each photo's settings as they were read, and with Auto.
    settings: Vec<(PhotoId, Arc<DevelopSettings>, DevelopSettings)>,
    /// Photos left out because they could not be read.
    skipped: usize,
}

fn prepare(app: &mut LightcraftApp, params: &Value) -> Result<Prepared, String> {
    let ids = app.session.targets(params).map_err(|e| e.to_string())?;
    let mut inputs = Vec::with_capacity(ids.len());
    for id in ids {
        let photo = app.session.catalog.photo(id).ok_or("Auto Settings: no such photo")?.clone();
        let settings = app.session.develop_of(id).ok_or("Auto Settings: no develop settings")?;
        let info = app.session.source_info(id);
        let source = app.session.media.source_ref(&photo, SourceLevel::Thumb);
        inputs.push(Input { id, settings, source, info });
    }
    Ok(Prepared { library: app.session.library_generation(), commits: app.session.commits(), inputs })
}
impl Prepared {
    fn run(self, progress: Option<&lightcraft_engine::activity::TaskHandle>) -> Result<Completed, String> {
        let total = self.inputs.len() as u64;
        let mut settings = Vec::with_capacity(self.inputs.len());
        let mut failed: Option<String> = None;
        let mut skipped = 0;
        for (i, input) in self.inputs.into_iter().enumerate() {
            if progress.is_some_and(|p| p.is_cancelled()) {
                return Err("Auto Settings cancelled".into());
            }
            // a photo that can't be read (offline, no decoder) is left out, not the whole selection
            let source = match input.source.load_source() {
                Ok(source) => source,
                Err(error) => {
                    failed.get_or_insert(error);
                    skipped += 1;
                    continue;
                }
            };
            let info = source.info_or(input.info);
            let auto = lightcraft_pipeline::auto::auto_tone(&source.image, &info, &input.settings);
            let mut develop = (*input.settings).clone();
            develop.light.exposure = auto.exposure;
            develop.light.contrast = auto.contrast;
            develop.light.highlights = auto.highlights;
            develop.light.shadows = auto.shadows;
            develop.light.whites = auto.whites;
            develop.light.blacks = auto.blacks;
            develop.color.vibrance = auto.vibrance;
            develop.color.saturation = auto.saturation;
            settings.push((input.id, input.settings, develop));
            if let Some(p) = progress {
                p.progress(i as u64 + 1, total);
            }
        }
        if progress.is_some_and(|p| p.is_cancelled()) {
            return Err("Auto Settings cancelled".into());
        }
        if let (true, Some(error)) = (settings.is_empty(), failed) {
            return Err(error);
        }
        Ok(Completed { library: self.library, commits: self.commits, settings, skipped })
    }
}
fn apply(app: &mut LightcraftApp, completed: Completed) -> Result<usize, String> {
    // An undo or a live edit changes a photo's settings without a commit: compare the settings too.
    let moved = completed.settings.iter().any(|(id, before, _)| app.session.develop_of(*id).is_none_or(|now| now != *before));
    if app.session.library_generation() != completed.library || app.session.commits() != completed.commits || moved {
        return Err("Auto Settings was not applied because the library changed; try again".into());
    }
    let ops: Vec<Op> = completed.settings.into_iter().filter_map(|(id, _, d)| app.session.develop_op(id, d, "Auto")).collect();
    if !ops.is_empty() {
        // through the command wrapper, so sidecars and the durable save follow as for any edit
        app.session
            .execute_fn("develop.auto", |s| {
                s.commit("Auto", Op::Batch { ops })?;
                Ok(Value::Null)
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(completed.skipped)
}
pub(crate) fn command(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(LABEL) {
        return Err("Auto Settings is already running".into());
    }
    let prepared = prepare(app, params)?;
    let count = prepared.inputs.len();
    let cancel = Arc::new(AtomicBool::new(false));
    crate::tasks::spawn_tracked(
        app,
        LABEL,
        "auto",
        cancel,
        move |progress| prepared.run(Some(&progress)),
        |app, ctx, result| match result.and_then(|done| apply(app, done)) {
            Ok(0) => app.toast(ctx, crate::i18n::tr("Auto Settings").to_owned()),
            Ok(skipped) => app.toast(ctx, format!("{}: {skipped} photo(s) could not be read", crate::i18n::tr("Auto Settings"))),
            Err(error) => app.toast(ctx, error),
        },
    )?;
    Ok(json!({"pending": true, "photos": count}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn app() -> LightcraftApp {
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default())
    }
    #[test]
    fn selection_auto_is_deferred_and_commits_once() {
        let mut app = app();
        let ids: Vec<_> = app.session.catalog.photos().take(3).map(|p| p.id).collect();
        app.session.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
        let before = app.session.commits();
        let reply = app.run("develop.auto", json!({})).unwrap();
        assert_eq!(reply["pending"], true);
        assert_eq!(reply["photos"], 3);
        assert_eq!(app.session.commits(), before, "decoding cannot mutate the UI session");
        assert!(app.tasks.is_running(LABEL));
        assert!(crate::tasks::wait_for(&mut app, &egui::Context::default(), LABEL, std::time::Duration::from_secs(60)));
        assert_eq!(app.session.commits(), before + 1);
        for id in ids {
            assert_eq!(app.session.develop_of(id).unwrap().color.vibrance, 30.0);
        }
    }
    #[test]
    fn selection_auto_does_not_overwrite_newer_edits_or_another_library() {
        let mut app = app();
        let done = prepare(&mut app, &json!({})).unwrap().run(None).unwrap();
        app.session.execute("develop.set", &json!({"control":"light.exposure", "value": 1.25})).unwrap();
        let before = app.session.commits();
        assert!(apply(&mut app, done).is_err());
        assert_eq!(app.session.commits(), before);
        let done = prepare(&mut app, &json!({})).unwrap().run(None).unwrap();
        app.session = lightcraft_engine::Session::with_demo();
        assert!(apply(&mut app, done).is_err());
    }
}
