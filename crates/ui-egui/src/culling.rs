//! Assisted Culling uses a native worker or one browser source per frame; catalog changes stay on the owner.
use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::cmd::cull::{CullJob, CullResult};
use serde_json::{Value, json};

use crate::LightcraftApp;

enum Runner {
    #[cfg(not(target_arch = "wasm32"))]
    Thread(std::sync::mpsc::Receiver<CullResult>),
    #[cfg(any(target_arch = "wasm32", test))]
    Inline { run: Box<lightcraft_engine::cmd::cull::CullRun>, frame: Option<u64> },
}

pub struct CullTask {
    runner: Runner,
    guard: TaskGuard,
    pending: Option<CullResult>,
}

impl Runner {
    fn poll(&mut self, guard: &TaskGuard, ctx: &egui::Context) -> Result<Option<CullResult>, std::sync::mpsc::TryRecvError> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Thread(rx) => {
                let _ = guard;
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                match rx.try_recv() {
                    Ok(result) => Ok(Some(result)),
                    Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
                    Err(error) => Err(error),
                }
            }
            #[cfg(any(target_arch = "wasm32", test))]
            Self::Inline { run, frame } => {
                if guard.is_cancelled() {
                    return Ok(run.step(Some(&guard.handle())));
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                let now = ctx.cumulative_frame_nr();
                let previous = frame.replace(now);
                // Paint the initial activity state and do not repeat work on a layout discard.
                if previous.is_none() || previous == Some(now) {
                    return Ok(None);
                }
                Ok(lightcraft_engine::memory::in_background(|| run.step(Some(&guard.handle()))))
            }
        }
    }
}

impl CullTask {
    /// Still decoding. A finished result that waits for an interaction to end isn't work a
    /// headless settle should wait for: a client that never ends its drag would stall every request.
    pub fn is_working(&self) -> bool {
        self.pending.is_none()
    }
}

impl Drop for CullTask {
    fn drop(&mut self) {
        if let Some(flag) = self.guard.cancel_flag() {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

pub fn start(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.culling.is_some() {
        return Err("Assisted Culling is already running".into());
    }
    let job = CullJob::prepare(&mut app.session, params).map_err(|e| e.to_string())?;
    let total = job.len();
    let guard = app.session.activity.start("culling", "Assisted Culling", Cancel::Yes);
    guard.progress(0, total as u64);
    #[cfg(not(target_arch = "wasm32"))]
    let runner = {
        let progress = guard.handle();
        let repaint = app.tasks.repaint.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let work = move || {
            let result = lightcraft_engine::memory::in_background(|| job.run(Some(&progress)));
            let _ = tx.send(result);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        };
        std::thread::Builder::new().name("lc-assisted-culling".into()).spawn(work).map_err(|e| format!("Assisted Culling could not start: {e}"))?;
        Runner::Thread(rx)
    };
    #[cfg(target_arch = "wasm32")]
    let runner = Runner::Inline { run: Box::new(job.into_steps()), frame: None };
    app.culling = Some(CullTask { runner, guard, pending: None });
    app.last_culling_result = None;
    Ok(json!({"background": true, "total": total}))
}

pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = app.culling.as_mut() else { return };
    if task.pending.is_none() {
        match task.runner.poll(&task.guard, ctx) {
            Ok(Some(result)) => task.pending = Some(result),
            Ok(None) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.culling = None;
                app.toast(ctx, crate::i18n::tr("Assisted Culling failed"));
                app.last_culling_result = Some(json!({"error": "Assisted Culling worker stopped"}));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
        }
    }
    // Cancellation accepted after the worker finished must still prevent catalog changes.
    if task.guard.is_cancelled() {
        app.culling = None;
        app.last_culling_result = Some(json!({"cancelled": true}));
        app.toast(ctx, crate::i18n::tr("Stopped"));
        return;
    }
    // A drag suppresses AutoWrite for the whole command. Keep its result and cancel action live
    // until the interaction ends, so flag changes to other photos also reach their sidecars.
    if app.session.interaction.is_some() {
        return;
    }
    task.guard.set_cancellable(false);
    if task.guard.is_cancelled() {
        app.culling = None;
        app.last_culling_result = Some(json!({"cancelled": true}));
        app.toast(ctx, crate::i18n::tr("Stopped"));
        return;
    }
    let Some(result) = task.pending.take() else { return };
    let report = app.session.execute_fn("photo.analyze", |s| result.apply(s)).map_err(|e| e.to_string());
    app.culling = None;
    match report {
        Ok(report) => {
            let message = if report["cancelled"] == true {
                crate::i18n::tr("Stopped").to_string()
            } else {
                format!(
                    "{}: {} · {}: {} · {}: {} · {}: {}",
                    crate::i18n::tr("Assisted Culling"),
                    report["photos"].as_array().map_or(0, Vec::len),
                    crate::i18n::tr("Rejected"),
                    report["rejected"],
                    crate::i18n::tr("Picked"),
                    report["picked"],
                    crate::i18n::tr("Failed"),
                    report["failed"].as_array().map_or(0, Vec::len)
                )
            };
            app.last_culling_result = Some(report);
            app.toast(ctx, message);
        }
        Err(error) => {
            app.last_culling_result = Some(json!({"error": error}));
            app.toast_for(ctx, error, 12.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use lightcraft_catalog::Flag;
    use lightcraft_engine::Session;

    use super::*;
    use crate::Services;

    fn completed(app: &mut LightcraftApp, params: &Value) {
        let job = CullJob::prepare(&mut app.session, params).unwrap();
        let guard = app.session.activity.start("culling", "Assisted Culling", Cancel::Yes);
        let result = job.run(Some(&guard.handle()));
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(result).unwrap();
        app.culling = Some(CullTask { runner: Runner::Thread(rx), guard, pending: None });
    }

    fn inline(app: &mut LightcraftApp) -> Vec<lightcraft_catalog::PhotoId> {
        let ids: Vec<_> = app.session.catalog.photos().take(3).map(|p| p.id).collect();
        assert_eq!(ids.len(), 3);
        for id in &ids {
            app.session.catalog.apply(lightcraft_catalog::Op::SetFlag { id: *id, flag: Flag::None }).unwrap();
        }
        let job = CullJob::prepare(&mut app.session, &json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "rejectBelow": 101})).unwrap();
        let guard = app.session.activity.start("culling", "Assisted Culling", Cancel::Yes);
        guard.progress(0, job.len() as u64);
        app.culling = Some(CullTask { runner: Runner::Inline { run: Box::new(job.into_steps()), frame: None }, guard, pending: None });
        ids
    }

    fn frame(app: &mut LightcraftApp, ctx: &egui::Context) {
        let mut output = ctx.run_ui(Default::default(), |ui| poll(app, ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn cooperative_culling_yields_to_controls_and_cancel_discards_partial_work() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let ids = inline(&mut app);
        let before = app.session.undo.len();
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        let task = app.session.activity.list().into_iter().find(|t| t.kind == "culling").unwrap();
        assert_eq!((task.done, task.total), (0, 3), "first frame paints the cancellable activity before decoding");
        frame(&mut app, &ctx);
        let task = app.session.activity.list().into_iter().find(|t| t.kind == "culling").unwrap();
        assert_eq!((task.done, task.total), (1, 3));
        assert!(task.cancellable);
        let _ = app.run("view.detail", json!({})).unwrap();
        app.run("activity.cancel", json!({"id": task.id})).unwrap();
        frame(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert_eq!(app.last_culling_result.as_ref().unwrap()["cancelled"], true);
        assert!(app.session.activity.list().iter().all(|t| t.kind != "culling"));
        assert_eq!(app.session.undo.len(), before);
        assert!(ids.iter().all(|id| {
            let photo = app.session.catalog.photo(*id).unwrap();
            photo.analysis.is_none() && photo.flag == Flag::None
        }));
    }

    #[test]
    fn cooperative_culling_analyzes_once_per_frame_and_commits_one_batch() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let ids = inline(&mut app);
        let before = app.session.undo.len();
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        for done in 1..=3 {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                poll(&mut app, ui.ctx());
                poll(&mut app, ui.ctx());
            });
            output.textures_delta.clear();
            let task = app.session.activity.list().into_iter().find(|t| t.kind == "culling").unwrap();
            assert_eq!(task.done, done, "a repeated layout pass must not decode a second photo");
            assert!(ids.iter().all(|id| app.session.catalog.photo(*id).unwrap().analysis.is_none()));
        }
        frame(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert_eq!(app.last_culling_result.as_ref().unwrap()["rejected"], 3);
        assert!(ids.iter().all(|id| app.session.catalog.photo(*id).unwrap().flag == Flag::Reject));
        assert_eq!(app.session.undo.len(), before + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert!(ids.iter().all(|id| {
            let photo = app.session.catalog.photo(*id).unwrap();
            photo.analysis.is_none() && photo.flag == Flag::None
        }));
    }

    #[test]
    fn invalid_targets_start_no_worker_and_change_no_scores() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let before = app.session.undo.len();
        for ids in [json!("1"), json!([1, "bad"])] {
            assert!(app.run("photo.analyze", json!({"ids": ids, "background": true})).is_err());
            assert!(app.culling.is_none());
            assert!(!app.session.activity.list().iter().any(|t| t.kind == "culling"));
        }
        assert_eq!(app.session.undo.len(), before);
        assert!(app.session.catalog.photos().all(|p| p.analysis.is_none()));
    }

    #[test]
    fn completed_culling_defers_other_photo_flags_and_auto_write_until_drag_ends() {
        let dir = std::env::temp_dir().join(format!("lc-cull-deferred-xmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = |seed| {
            let image = lightcraft_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, seed, 255]);
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap()
        };
        let active_path = dir.join("active.png");
        let other_path = dir.join("other.png");
        std::fs::write(&active_path, png(60)).unwrap();
        std::fs::write(&other_path, png(61)).unwrap();
        let mut session = Session::new().with_fs();
        let imported = session.execute("library.import", &json!({"paths": [active_path.to_string_lossy(), other_path.to_string_lossy()]})).unwrap();
        assert_eq!(imported["imported"].as_array().unwrap().len(), 2);
        let active = session.catalog.photos().find(|p| p.file_name == "active.png").unwrap().id;
        let other = session.catalog.photos().find(|p| p.file_name == "other.png").unwrap().id;
        session.execute("library.select", &json!({"ids": [active.0]})).unwrap();
        session.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
        let mut app = LightcraftApp::new(session, Services::default());
        completed(&mut app, &json!({"ids": [other.0], "rejectBelow": 101}));
        let before = app.session.undo.len();
        app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        app.run("develop.set", json!({"control": "light.exposure", "value": 1.0})).unwrap();
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        poll(&mut app, &ctx);
        assert!(app.culling.as_ref().unwrap().pending.is_some(), "the completed result survives every frame of the drag");
        assert!(app.last_culling_result.is_none());
        assert_eq!(app.session.undo.len(), before);
        let photo = app.session.catalog.photo(other).unwrap();
        assert!(photo.analysis.is_none());
        assert_eq!(photo.flag, Flag::None);
        assert!(!other_path.with_extension("xmp").exists());
        app.run("develop.endInteraction", json!({})).unwrap();
        assert!(active_path.with_extension("xmp").is_file());
        assert!(!other_path.with_extension("xmp").exists(), "ending the drag only saves its own photo");
        poll(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert_eq!(app.last_culling_result.as_ref().unwrap()["rejected"], 1);
        assert_eq!(app.session.undo.len(), before + 2, "drag and culling remain separate undo steps");
        assert_eq!(app.session.catalog.photo(other).unwrap().flag, Flag::Reject);
        let sidecar = other_path.with_extension("xmp");
        let saved = std::fs::read_to_string(&sidecar).unwrap();
        let data = lightcraft_engine::sidecar::parse_sidecar(&saved, lightcraft_engine::crs::Target::Rendered).unwrap();
        assert_eq!(data.flag, Some(Flag::Reject), "AutoWrite saves the culling flag on the other photo");
        app.run("edit.undo", json!({})).unwrap();
        let saved = std::fs::read_to_string(&sidecar).unwrap();
        let data = lightcraft_engine::sidecar::parse_sidecar(&saved, lightcraft_engine::crs::Target::Rendered).unwrap();
        assert_eq!(data.flag, Some(Flag::None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn completed_culling_remains_cancellable_while_waiting_for_a_drag() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let id = app.session.active().unwrap();
        completed(&mut app, &json!({"ids": [id.0], "rejectBelow": 101}));
        app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        assert!(app.culling.as_ref().unwrap().pending.is_some());
        assert!(!app.culling.as_ref().unwrap().is_working(), "a headless settle doesn't wait for the drag to end");
        let task = app.session.activity.list().into_iter().find(|t| t.kind == "culling").unwrap();
        assert!(task.cancellable);
        app.run("activity.cancel", json!({"id": task.id})).unwrap();
        poll(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert_eq!(app.last_culling_result.as_ref().unwrap()["cancelled"], true);
        assert!(app.session.catalog.photo(id).unwrap().analysis.is_none());
        assert_eq!(app.session.catalog.photo(id).unwrap().flag, Flag::None);
        app.run("develop.endInteraction", json!({})).unwrap();
        poll(&mut app, &ctx);
        assert!(app.session.catalog.photo(id).unwrap().analysis.is_none());
    }

    #[test]
    fn disconnected_worker_removes_activity_and_allows_another_run() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let id = app.session.active().unwrap();
        let before = app.session.undo.len();
        let guard = app.session.activity.start("culling", "Assisted Culling", Cancel::Yes);
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        app.culling = Some(CullTask { runner: Runner::Thread(rx), guard, pending: None });
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert!(!app.session.activity.list().iter().any(|t| t.kind == "culling"));
        assert_eq!(app.last_culling_result.as_ref().unwrap()["error"], "Assisted Culling worker stopped");
        assert_eq!(app.session.undo.len(), before);
        assert!(app.session.catalog.photo(id).unwrap().analysis.is_none());
        assert_eq!(app.session.catalog.photo(id).unwrap().flag, Flag::None);
        start(&mut app, &json!({"ids": [id.0]})).unwrap();
        assert!(app.culling.is_some());
    }

    #[test]
    fn completed_culling_after_library_open_is_discarded() {
        let dir = std::env::temp_dir().join(format!("lc-cull-deferred-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        let id = app.session.active().unwrap();
        completed(&mut app, &json!({"ids": [id.0], "rejectBelow": 101}));
        app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        assert!(app.culling.as_ref().unwrap().pending.is_some());
        app.session.open_library(&dir, true).unwrap();
        poll(&mut app, &ctx);
        assert!(app.culling.is_none());
        assert!(app.last_culling_result.as_ref().unwrap()["error"].as_str().unwrap().contains("analysis was discarded"));
        assert!(app.session.catalog.photo(id).unwrap().analysis.is_none());
        assert_eq!(app.session.catalog.photo(id).unwrap().flag, Flag::None);
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }
}
