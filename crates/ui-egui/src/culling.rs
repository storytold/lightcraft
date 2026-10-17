//! Assisted Culling runs decoding and scoring off the UI thread; catalog changes remain on it.
use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::cmd::cull::{CullJob, CullResult};
use serde_json::{Value, json};

use crate::LightcraftApp;

pub struct CullTask {
    rx: std::sync::mpsc::Receiver<CullResult>,
    guard: TaskGuard,
    pending: Option<CullResult>,
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
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("lc-assisted-culling".into()).spawn(work).map_err(|e| format!("Assisted Culling could not start: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    work();
    app.culling = Some(CullTask { rx, guard, pending: None });
    app.last_culling_result = None;
    Ok(json!({"background": true, "total": total}))
}

pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = app.culling.as_mut() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    if task.pending.is_none() {
        match task.rx.try_recv() {
            Ok(result) => task.pending = Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.culling = None;
                app.toast(ctx, crate::i18n::tr("Assisted Culling failed"));
                app.last_culling_result = Some(json!({"error": "Assisted Culling worker stopped"}));
                return;
            }
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
        app.culling = Some(CullTask { rx, guard, pending: None });
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
