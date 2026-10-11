//! File → Import from Device for a camera or phone that doesn't mount as a disk (PTP/MTP; issue
//! #1005): its new photos are fetched into a staging folder on a worker thread, with a row and ✕
//! in the activity stack, then the import review opens on them with Move checked, like a card
//! (`lightcraft_engine::camera_import` has the rules). A cancelled fetch keeps what it finished:
//! choosing the camera again carries on from there.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use lightcraft_engine::camera_import::{Fetched, Known, Progress, staging_dir};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// A camera being read on a worker thread.
pub struct CameraTask {
    progress: Arc<Progress>,
    rx: std::sync::mpsc::Receiver<Result<Fetched, String>>,
    /// The camera's name (toasts, the staging folder).
    name: String,
    /// The album the review should add to (an album's context menu ▸ Import from Device).
    album: Option<u64>,
    guard: lightcraft_engine::activity::TaskGuard,
}

impl CameraTask {
    /// The task for a fetch reporting into `progress` and sending its result on `rx`, with its row
    /// in the activity stack.
    fn new(
        app: &LightcraftApp,
        name: String,
        album: Option<u64>,
        progress: Arc<Progress>,
        rx: std::sync::mpsc::Receiver<Result<Fetched, String>>,
    ) -> Self {
        // English: the activity stack translates labels
        let guard = app.session.activity.start("camera", "Reading camera", lightcraft_engine::activity::Cancel::Flag(progress.cancel.clone()));
        guard.set_unit(lightcraft_engine::activity::Unit::Bytes);
        guard.detail(&name);
        CameraTask { progress, rx, name, album, guard }
    }

    /// `{camera, bytesDone, bytesTotal, filesDone, filesTotal, listed}` for `ui.inspect`.
    pub fn inspect(&self) -> Value {
        let p = &self.progress;
        json!({
            "camera": self.name,
            "listed": p.listed.load(Ordering::Relaxed),
            "filesDone": p.files_done.load(Ordering::Relaxed),
            "filesTotal": p.files_total.load(Ordering::Relaxed),
            "bytesDone": p.bytes_done.load(Ordering::Relaxed),
            "bytesTotal": p.bytes_total.load(Ordering::Relaxed),
        })
    }
}

/// The `file.addFromDevice` params for device `d` (a camera by its id, a card by its DCIM folder),
/// for every menu that lists devices.
pub fn device_params(d: &lightcraft_engine::devices::Device) -> Value {
    match &d.camera {
        Some(id) => json!({"camera": id, "name": d.name}),
        None => json!({"path": d.path}),
    }
}

/// Start reading camera `id` (from `library.devices`); the review opens when its photos are staged
/// ([`poll`]), with `album` chosen when one is given.
pub fn start(app: &mut LightcraftApp, id: &str, name: &str, album: Option<u64>) -> Result<Value, String> {
    if app.camera.is_some() {
        return Err("a camera is already being read".into());
    }
    if app.scan.is_some() || app.import.is_some() {
        return Err("an import is running".into());
    }
    crate::import::busy_synchronizing(app)?;
    let name = if name.trim().is_empty() { "Camera".to_string() } else { name.trim().to_string() };
    let known = Known::of(&app.session.catalog);
    let dir = staging_dir(&name);
    let progress = Arc::new(Progress::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let p = progress.clone();
    let camera = id.to_string();
    let job = move || {
        let r = lightcraft_engine::camera_import::fetch_camera(&camera, &known, &dir, &p).map_err(|e| e.to_string());
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("lightcraft-camera".into()).spawn(job).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    job();
    app.camera = Some(CameraTask::new(app, name.clone(), album, progress, rx));
    Ok(json!({"reading": name}))
}

/// Collect a finished fetch and open the review (called every frame).
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = app.camera.as_ref() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let r = match task.rx.try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            if task.guard.is_cancelled() {
                // ✕: the worker stops at its next read; what it finished stays staged for next time
                app.camera = None;
                return;
            }
            let p = &task.progress;
            let (done, total) = (p.bytes_done.load(Ordering::Relaxed), p.bytes_total.load(Ordering::Relaxed));
            task.guard.progress(done, total);
            // while the card is listed nothing is known yet: say what is happening instead
            task.guard.detail(if total == 0 { crate::i18n::tr("Looking for photos…") } else { &task.name });
            return;
        }
        Err(_) => Err(crate::i18n::tr("the camera stopped answering").to_string()),
    };
    let Some(task) = app.camera.take() else { return };
    let name = task.name;
    let fetched = match r {
        Ok(f) => f,
        Err(e) => {
            if !task.progress.cancel.load(Ordering::Relaxed) {
                app.toast(ctx, crate::i18n::tr_format!("Couldn't read {name}: {error}", name = name, error = e));
            }
            return;
        }
    };
    let failed = fetched.failed.len();
    let unreadable = || {
        let s = if failed == 1 { "" } else { "s" };
        crate::i18n::tr_format!("Couldn't read {n} photo{s} from {name}", n = failed, s = s, name = name)
    };
    if fetched.staged.is_empty() {
        let msg = if failed > 0 {
            unreadable()
        } else if fetched.known > 0 {
            crate::i18n::tr_format!("No new photos on {name} ({n} already in the library)", name = name, n = fetched.known)
        } else {
            crate::i18n::tr_format!("No photos found on {name}", name = name)
        };
        app.toast(ctx, msg);
        return;
    }
    if failed > 0 {
        let msg = unreadable();
        app.toast(ctx, msg);
    }
    if let Err(e) = crate::import::open_staged(app, fetched.staged, fetched.folder, task.album) {
        app.toast(ctx, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::Headless;
    use lightcraft_engine::camera_import::Failure;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(60);

    fn demo() -> Headless {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        h.settle(T);
        h
    }

    /// A camera task whose worker already finished with `r` (or, with `None`, is still running).
    fn task(h: &mut Headless, name: &str, r: Option<Result<Fetched, String>>) -> Arc<Progress> {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(r) = r {
            tx.send(r).unwrap();
        }
        // keep the sender alive for a running task, so the channel reads as empty, not closed
        std::mem::forget(tx);
        h.app.camera = Some(CameraTask::new(&h.app, name.into(), None, progress.clone(), rx));
        progress
    }

    fn toast(h: &Headless) -> String {
        h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
    }

    fn png(path: &std::path::Path) {
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(path, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }

    #[test]
    fn staged_photos_open_the_review_with_copy_and_move_checked() {
        let mut h = demo();
        let dir = std::env::temp_dir().join(format!("lc-camui-{}", std::process::id())).join(lightcraft_engine::camera_import::STAGING).join("X100T");
        std::fs::create_dir_all(dir.join("DCIM/123_FUJI")).unwrap();
        let files: Vec<String> = ["A.png", "B.png"]
            .iter()
            .map(|n| {
                let p = dir.join("DCIM/123_FUJI").join(n);
                png(&p);
                p.to_string_lossy().to_string()
            })
            .collect();
        let folder = dir.to_string_lossy().to_string();
        task(&mut h, "X100T", Some(Ok(Fetched { staged: files, fetched: 2, folder: folder.clone(), ..Default::default() })));
        assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. }))), "no review");
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert!(opts.copy && opts.move_files, "copy {} move {}", opts.copy, opts.move_files);
        assert_eq!(opts.candidates.len(), 2);
        assert_eq!(opts.sources, [folder]);
        assert_eq!(crate::import::source_summary(&opts.sources), "Folder “X100T” (and its subfolders)");
        assert!(h.app.camera.is_none());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn an_albums_import_from_device_keeps_the_album() {
        let mut h = demo();
        let dir =
            std::env::temp_dir().join(format!("lc-camui-album-{}", std::process::id())).join(lightcraft_engine::camera_import::STAGING).join("X100T");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("A.png");
        png(&file);
        let album = h.app.session.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
        let progress = Arc::new(Progress::default());
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Fetched {
            staged: vec![file.to_string_lossy().to_string()],
            fetched: 1,
            folder: dir.to_string_lossy().to_string(),
            ..Default::default()
        }))
        .unwrap();
        h.app.camera = Some(CameraTask::new(&h.app, "X100T".into(), Some(album), progress, rx));
        assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. }))), "no review");
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert_eq!(opts.album, Some(album));
        assert!(opts.copy && opts.move_files);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn menus_offer_cameras_by_id_and_cards_by_folder() {
        let card = lightcraft_engine::devices::Device { name: "EOS_DIGITAL".into(), path: "E:\\DCIM".into(), root: "E:\\".into(), camera: None };
        assert_eq!(device_params(&card), json!({"path": "E:\\DCIM"}));
        let cam =
            lightcraft_engine::devices::Device { name: "X100T".into(), path: String::new(), root: String::new(), camera: Some("usb#04cb".into()) };
        assert_eq!(device_params(&cam), json!({"camera": "usb#04cb", "name": "X100T"}));
    }

    #[test]
    fn nothing_new_on_the_camera_is_said_without_a_review() {
        let mut h = demo();
        task(&mut h, "X100T", Some(Ok(Fetched { known: 3, ..Default::default() })));
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert_eq!(toast(&h), "No new photos on X100T (3 already in the library)");
        assert!(h.app.ui.dialog.is_none());
        task(&mut h, "X100T", Some(Ok(Fetched::default())));
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert_eq!(toast(&h), "No photos found on X100T");
    }

    #[test]
    fn unreadable_files_are_reported() {
        let mut h = demo();
        let failed = vec![Failure { name: "A.RAF".into(), error: "camera: gone".into() }];
        task(&mut h, "X100T", Some(Ok(Fetched { failed: failed.clone(), ..Default::default() })));
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert_eq!(toast(&h), "Couldn't read 1 photo from X100T");
        let two = vec![failed[0].clone(), failed[0].clone()];
        task(&mut h, "X100T", Some(Ok(Fetched { failed: two, ..Default::default() })));
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert_eq!(toast(&h), "Couldn't read 2 photos from X100T");
        task(&mut h, "X100T", Some(Err("camera: The device is not connected.".into())));
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert_eq!(toast(&h), "Couldn't read X100T: camera: The device is not connected.");
    }

    #[test]
    fn the_activity_row_shows_bytes_and_cancel_drops_the_task() {
        let mut h = demo();
        let p = task(&mut h, "X100T", None);
        h.step();
        let row = |h: &Headless| h.app.session.activity.list().into_iter().find(|t| t.kind == "camera");
        let r = row(&h).expect("a camera row");
        assert_eq!((r.done, r.total, r.label.as_str()), (0, 0, "Reading camera"));
        assert_eq!(r.detail, "Looking for photos…");
        p.bytes_total.store(4000, Ordering::Relaxed);
        p.bytes_done.store(1000, Ordering::Relaxed);
        h.step();
        let r = row(&h).unwrap();
        assert_eq!((r.done, r.total, r.unit, r.detail.as_str()), (1000, 4000, lightcraft_engine::activity::Unit::Bytes, "X100T"));
        // the stack's ✕ sets the fetch's own flag
        h.app.session.activity.cancel(r.id).unwrap();
        assert!(p.cancel.load(Ordering::Relaxed));
        h.step();
        assert!(h.app.camera.is_none());
        assert!(row(&h).is_none(), "the row goes with the task");
    }

    #[test]
    fn a_camera_that_cant_be_read_is_a_message_not_a_crash() {
        let mut h = demo();
        let r = h.request(
            "engine.execute",
            json!({"command": "file.addFromDevice", "params": {"camera": "\\\\?\\usb#no-such-camera", "name": "Ghost"}}),
            T,
        );
        assert_eq!(r["ok"], true, "{r}");
        // a second camera while one is read is refused
        let again = h.request("engine.execute", json!({"command": "file.addFromDevice", "params": {"camera": "x", "name": "Ghost"}}), T);
        if h.app.camera.is_some() {
            assert_eq!(again["ok"], false, "{again}");
        }
        assert!(h.step_until(T, |h| h.app.camera.is_none()));
        assert!(toast(&h).starts_with("Couldn't read Ghost: "), "{}", toast(&h));
        assert!(h.app.ui.dialog.is_none());
    }
}
