//! Queue native document requests until a usable library and an idle importer are available.
//! Paths follow the same add-in-place import as command-line files and window drops.

use std::collections::VecDeque;
use std::path::PathBuf;

use lightcraft_ui_egui::LightcraftApp;

#[derive(Default)]
pub struct PendingOpens {
    batches: VecDeque<Batch>,
}

struct Batch {
    paths: Vec<String>,
    /// Startup recovery chooses the destination before this becomes an import request.
    library: Option<u64>,
    guard: lightcraft_engine::activity::TaskGuard,
}

impl PendingOpens {
    pub fn enqueue(&mut self, paths: Vec<Result<PathBuf, String>>, app: &mut LightcraftApp) {
        let mut batch = Vec::new();
        for path in paths {
            match path.and_then(|p| {
                if !p.is_absolute() {
                    return Err("The open request contains a path that is not absolute.".into());
                }
                // The catalog stores UTF-8 paths. Never silently change an unrepresentable path
                // into the name of another file. Spaces, Unicode and literal % signs are kept.
                p.into_os_string().into_string().map_err(|_| "This file's name cannot be represented as UTF-8.".into())
            }) {
                Ok(path) => batch.push(path),
                Err(error) => app.notices.push(error),
            }
        }
        if !batch.is_empty() {
            let library = (!lightcraft_ui_egui::import::waiting_for_library(app)).then(|| app.session.library_generation());
            let guard = app.session.activity.start(
                lightcraft_ui_egui::import::DOCUMENT_OPEN_ACTIVITY,
                "Waiting to import opened files",
                lightcraft_engine::activity::Cancel::Yes,
            );
            guard.progress(0, batch.len() as u64);
            self.batches.push_back(Batch { paths: batch, library, guard });
            app.pending_document_opens = self.batches.len();
            app.pending_document_library = self.batches.iter().find_map(|b| b.library);
        }
    }

    pub fn tick(&mut self, app: &mut LightcraftApp) {
        // Cancellation works while a dialog, another worker, or startup recovery blocks import.
        self.batches.retain(|batch| !batch.guard.is_cancelled());
        app.pending_document_opens = self.batches.len();
        app.pending_document_library = self.batches.iter().find_map(|b| b.library);
        if lightcraft_ui_egui::import::waiting_for_library(app) {
            return;
        }
        // Bind every recovery request as soon as its destination is chosen, even if a manual
        // import still has the importer. Later library switches must not retarget this queue.
        for batch in &mut self.batches {
            batch.library.get_or_insert(app.session.library_generation());
        }
        app.pending_document_library = self.batches.front().and_then(|b| b.library);
        if app.import.is_some()
            || app.scan.is_some()
            || lightcraft_ui_egui::import::dialog_open(app)
            || app.lightroom.is_some()
            || app.sync_run.is_some()
            || app.sync_owns_changes
        {
            return;
        }
        if let Some(batch) = self.batches.pop_front() {
            app.pending_document_opens = self.batches.len();
            app.pending_document_library = self.batches.front().and_then(|b| b.library);
            if batch.library != Some(app.session.library_generation()) {
                app.notices.push("The library changed before these photos could be imported. Open the photos again in the intended library.".into());
                return;
            }
            // start_documents uses the same worker, import params and library.import commits as
            // a window drop. The checks above keep requests queued while an importer is busy;
            // report any unexpected start error instead of silently discarding the request.
            if let Err(error) = lightcraft_ui_egui::import::start_documents(app, batch.paths) {
                app.notices.push(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{Services, panels::library_problem::LibraryProblem};

    fn app() -> LightcraftApp {
        LightcraftApp::new(Session::new(), Services::default())
    }

    #[test]
    fn native_paths_become_an_add_in_place_import() {
        let mut app = app();
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(std::env::temp_dir().join("撮影 café #1.ARW")), Ok(std::env::temp_dir().join("missing %.jpg"))], &mut app);
        pending.tick(&mut app);
        let task = app.import.as_ref().unwrap();
        assert_eq!(task.total, 2);
        assert!(pending.batches.is_empty());
    }

    #[test]
    fn cold_start_waits_for_library_and_drains_once() {
        let mut app = app();
        app.library_problem = Some(LibraryProblem::new("/unavailable", "offline"));
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(std::env::temp_dir().join("one.dng"))], &mut app);
        pending.tick(&mut app);
        assert!(app.import.is_none());
        assert_eq!(pending.batches.len(), 1);
        app.library_problem = None;
        pending.tick(&mut app);
        assert_eq!(app.import.as_ref().unwrap().total, 1);
        app.import = None;
        pending.tick(&mut app);
        assert!(app.import.is_none());
    }

    #[test]
    fn warm_requests_wait_in_order_and_temporary_session_can_receive_them() {
        let mut app = app();
        app.library_problem = Some(LibraryProblem { dismissed: true, ..Default::default() });
        let mut pending = PendingOpens::default();
        for path in ["one.jpg", "two.jpg"] {
            pending.enqueue(vec![Ok(std::env::temp_dir().join(path))], &mut app);
        }
        pending.tick(&mut app);
        pending.tick(&mut app);
        assert_eq!(app.import.as_ref().unwrap().total, 1);
        assert_eq!(pending.batches.front().unwrap().paths, &[std::env::temp_dir().join("two.jpg").to_str().unwrap()]);
        assert_eq!(pending.batches.len(), 1);
        app.import = None;
        pending.tick(&mut app);
        assert_eq!(app.import.as_ref().unwrap().total, 1);
        assert!(pending.batches.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn bad_paths_report_errors_without_discarding_the_good_file() {
        use std::os::unix::ffi::OsStringExt;
        let mut app = app();
        let mut pending = PendingOpens::default();
        pending.enqueue(
            vec![
                Ok(std::ffi::OsString::from_vec(b"/tmp/\xff.jpg".to_vec()).into()),
                Ok("relative.jpg".into()),
                Err("invalid URL".into()),
                Ok("/tmp/good.jpg".into()),
            ],
            &mut app,
        );
        pending.tick(&mut app);
        assert_eq!(app.notices.len(), 3);
        assert_eq!(app.import.as_ref().unwrap().total, 1);
    }
}
#[cfg(test)]
mod review_repros {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{Services, import, state::Dialog};
    use serde_json::json;

    fn fixture(tag: &str) -> (std::path::PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("lc-1141-review-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        for (name, color) in [("native.png", [20, 90, 30, 255]), ("manual.png", [180, 10, 40, 255])] {
            let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![color; 64] };
            let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            paths.push(path.to_str().unwrap().into());
        }
        (dir, paths)
    }

    fn finish(app: &mut LightcraftApp) {
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.import.is_some() && std::time::Instant::now() < deadline {
            import::tick(app, &ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.import.is_none());
    }

    #[test]
    fn review_pending_native_open_blocks_an_unrelated_library_switch() {
        let (dir, paths) = fixture("library");
        let mut session = Session::new().with_fs();
        session.open_library(dir.join("A"), false).unwrap();
        let mut app = LightcraftApp::new(session, Services::default());
        let mut pending = PendingOpens::default();
        import::start_paths(&mut app, vec![paths[1].clone()]).unwrap();
        pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
        pending.tick(&mut app);
        assert_eq!(pending.batches.len(), 1);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 1);
        for command in ["app.openLibrary", "app.newLibrary"] {
            let error = app.run(command, json!({"path": dir.join("B").to_str().unwrap()})).unwrap_err();
            assert!(error.contains("before switching libraries"), "{error}");
        }
        assert!(!dir.join("B").exists(), "refuse before creating or opening another library");
        pending.tick(&mut app);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 2, "both requests belong to A");
        assert_eq!(app.pending_document_opens, 0);
        app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()})).unwrap();
        let actual = app.session.catalog.len();
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(actual, 0, "a request queued for A must not silently import into B");
    }

    fn block_probe(app: &mut LightcraftApp, path: String) -> (std::sync::mpsc::Receiver<()>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let original = app.session.media.file_probe.clone().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let ready = release.clone();
        app.session.media.file_probe = Some(Arc::new(move |p| {
            if p == path {
                tx.send(()).unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while !ready.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            original(p)
        }));
        (rx, release)
    }

    #[test]
    fn review_active_native_import_blocks_library_switches() {
        let (dir, paths) = fixture("active-library");
        let mut session = Session::new().with_fs();
        session.open_library(dir.join("A"), false).unwrap();
        let mut app = LightcraftApp::new(session, Services::default());
        let (started, release) = block_probe(&mut app, paths[0].clone());
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
        pending.tick(&mut app);
        import::tick(&mut app, &egui::Context::default());
        started.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        assert_eq!(app.session.activity.list().len(), 1);
        for command in ["app.openLibrary", "app.newLibrary"] {
            let error = app.run(command, json!({"path": dir.join("B").to_str().unwrap()})).unwrap_err();
            assert!(error.contains("before switching libraries"), "{error}");
        }
        release.store(true, std::sync::atomic::Ordering::SeqCst);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 1, "the native photo was imported into A");
        app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()})).unwrap();
        let b_count = app.session.catalog.len();
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(b_count, 0, "native import started in A must not commit into B");
    }

    #[test]
    fn review_import_dialog_keeps_the_native_request_queued() {
        let (dir, paths) = fixture("modal");
        let mut app = LightcraftApp::new(Session::new().with_fs(), Services::default());
        let candidates = lightcraft_engine::import::scan(&mut app.session, &[paths[1].clone()]);
        let d = import::ImportDialog::new(candidates);
        let (started, release) = block_probe(&mut app, paths[0].clone());
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(d.clone()) });
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
        pending.tick(&mut app);
        assert_eq!(pending.batches.len(), 1);
        assert!(app.import.is_none());
        assert_eq!(confirm(&mut app)["ok"], true);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 1, "manual import finishes first");
        pending.tick(&mut app);
        import::tick(&mut app, &egui::Context::default());
        started.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        release.store(true, std::sync::atomic::Ordering::SeqCst);
        finish(&mut app);
        let actual = app.session.catalog.len();
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(actual, 2, "native request must remain queued until the manual import finishes");
    }

    fn confirm(app: &mut LightcraftApp) -> serde_json::Value {
        use lightcraft_ui_egui::control::{ControlRequest, Outcome, handle};
        let (request, _) = ControlRequest::new("ui.dialog.confirm", json!({}));
        let Outcome::Done(response) = handle(app, &egui::Context::default(), &request) else { panic!("expected a response") };
        response
    }

    #[test]
    fn manual_confirmation_preserves_a_blocked_native_worker_and_can_be_retried() {
        let (dir, paths) = fixture("blocked-native");
        let mut app = LightcraftApp::new(Session::new().with_fs(), Services::default());
        let candidates = lightcraft_engine::import::scan(&mut app.session, &[paths[1].clone()]);
        let mut d = import::ImportDialog::new(candidates);
        d.new_album = "Manual photos".into();
        let (started, release) = block_probe(&mut app, paths[0].clone());
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
        pending.tick(&mut app);
        import::tick(&mut app, &egui::Context::default());
        started.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        let dialog = Dialog::Import { opts: Box::new(d) };
        app.ui.dialog = Some(dialog.clone());
        let undo = app.session.undo.len();
        let result = confirm(&mut app);
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("click Import again"));
        assert_eq!(app.ui.dialog, Some(dialog), "keep all choices for retry");
        assert_eq!(app.session.undo.len(), undo, "refuse before creating the manual album");
        assert_eq!(app.session.activity.list().len(), 1, "native worker is still running");
        release.store(true, std::sync::atomic::Ordering::SeqCst);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 1, "native import was not replaced");
        assert_eq!(confirm(&mut app)["ok"], true);
        assert!(app.ui.dialog.is_none());
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 2, "both sets reach the catalog after retry");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn running_import_rejects_prepared_batches_if_a_host_replaces_the_library() {
        let (dir, paths) = fixture("host-replaced");
        let mut session = Session::new().with_fs();
        session.open_library(dir.join("A"), false).unwrap();
        let mut app = LightcraftApp::new(session, Services::default());
        let (started, release) = block_probe(&mut app, paths[0].clone());
        import::start_documents(&mut app, vec![paths[0].clone()]).unwrap();
        import::tick(&mut app, &egui::Context::default());
        started.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        // Bypass the UI guard to prove that the import checks its own destination as well.
        app.session.open_library(dir.join("B"), false).unwrap();
        release.store(true, std::sync::atomic::Ordering::SeqCst);
        finish(&mut app);
        assert!(app.session.catalog.is_empty());
        assert!(app.notices.iter().any(|n| n.contains("library changed")));
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovery_can_choose_a_library_or_continue_without_saving() {
        use lightcraft_ui_egui::panels::library_problem::{self, LibraryProblem};
        for choice in ["retry", "replace", "temporary"] {
            let (dir, paths) = fixture(choice);
            let mut app = LightcraftApp::new(Session::new().with_fs(), Services::default());
            app.library_problem = Some(LibraryProblem::new(dir.join("A").to_str().unwrap(), "offline"));
            let mut pending = PendingOpens::default();
            pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
            pending.tick(&mut app);
            assert!(pending.batches.front().unwrap().library.is_none());
            match choice {
                "temporary" => app.library_problem.as_mut().unwrap().dismissed = true,
                _ => {
                    let library = if choice == "retry" { "A" } else { "B" };
                    app.run("app.openLibrary", json!({"path": dir.join(library).to_str().unwrap()})).unwrap();
                    // The first recovery choice also protects requests before the next tick.
                    assert!(app.run("app.newLibrary", json!({"path": dir.join("C").to_str().unwrap()})).is_err());
                    library_problem::logic(&mut app);
                }
            }
            pending.tick(&mut app);
            finish(&mut app);
            assert_eq!(app.session.catalog.len(), 1);
            assert!(pending.batches.is_empty());
            assert_eq!(app.session.library.is_none(), choice == "temporary");
            drop(app);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn native_request_waits_for_the_import_file_picker() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut rx = Some(rx);
        let mut app = LightcraftApp::new(Session::new(), Services { picker: Some(Box::new(move |_| Ok(rx.take().unwrap()))), ..Default::default() });
        app.run("file.addPhotos", json!({})).unwrap();
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(std::env::temp_dir().join("native.png"))], &mut app);
        pending.tick(&mut app);
        assert!(app.import.is_none());
        assert_eq!(pending.batches.len(), 1);
        tx.send(Vec::new()).unwrap();
        lightcraft_ui_egui::pick::poll(&mut app, &egui::Context::default());
        pending.tick(&mut app);
        assert!(app.import.is_some());
        assert!(pending.batches.is_empty());
    }

    #[test]
    fn reopening_recovery_does_not_retarget_requests_bound_to_a_temporary_session() {
        use lightcraft_ui_egui::panels::library_problem::LibraryProblem;
        let (dir, paths) = fixture("temporary-reopen");
        let mut app = LightcraftApp::new(Session::new().with_fs(), Services::default());
        app.library_problem = Some(LibraryProblem { dismissed: true, ..LibraryProblem::new("/offline", "offline") });
        import::start_paths(&mut app, vec![paths[1].clone()]).unwrap();
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(paths[0].clone().into())], &mut app);
        pending.tick(&mut app);
        finish(&mut app);
        // The temporary-session banner can reopen the recovery window. That does not undo
        // the earlier Continue Without Saving choice for requests already bound to it.
        app.library_problem.as_mut().unwrap().dismissed = false;
        assert!(app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()})).is_err());
        pending.tick(&mut app);
        finish(&mut app);
        assert_eq!(app.session.catalog.len(), 2);
        assert!(app.pending_document_library.is_none());
        app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()})).unwrap();
        assert!(app.session.catalog.is_empty());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod independent_round2 {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{Services, import, state::Dialog};
    use serde_json::json;

    fn fixture(tag: &str) -> (PathBuf, String, LightcraftApp) {
        let dir = std::env::temp_dir().join(format!("lc-independent-r2-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[20, 90, 30, 255]; 64] };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
        let file = dir.join("photo.png");
        std::fs::write(&file, bytes).unwrap();
        let mut session = Session::new().with_fs();
        session.open_library(dir.join("A"), false).unwrap();
        (dir, file.to_str().unwrap().into(), LightcraftApp::new(session, Services::default()))
    }
    fn finish(app: &mut LightcraftApp) {
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.import.is_some() && std::time::Instant::now() < deadline {
            import::tick(app, &ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.import.is_none());
    }
    #[test]
    fn missing_and_malformed_files_release_the_library_switch_guard() {
        let (dir, _, mut app) = fixture("failures");
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"this is not a PNG").unwrap();
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(dir.join("missing.png"))], &mut app);
        pending.enqueue(vec![Ok(bad)], &mut app);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let unreadable = dir.join("unreadable.png");
            std::fs::copy(dir.join("photo.png"), &unreadable).unwrap();
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root reads through permission bits (FreeBSD CI runs tests as root).
            if std::fs::read(&unreadable).is_err() {
                pending.enqueue(vec![Ok(unreadable)], &mut app);
            }
        }
        let batches = pending.batches.len();
        for _ in 0..batches {
            pending.tick(&mut app);
            finish(&mut app);
        }
        assert_eq!(app.pending_document_opens, 0);
        assert!(app.pending_document_library.is_none());
        assert!(app.session.activity.list().is_empty());
        assert!(app.session.catalog.is_empty());
        app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()})).unwrap();
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cancelled_blocked_native_import_must_allow_switching() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (dir, path, mut app) = fixture("cancel-blocked");
        let original = app.session.media.file_probe.clone().unwrap();
        let original_for_job = original.clone();
        let (exited_tx, exited_rx) = std::sync::mpsc::channel();
        struct WorkerExited(std::sync::mpsc::Sender<()>);
        impl Drop for WorkerExited {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let exited = WorkerExited(exited_tx);
        let (tx, rx) = std::sync::mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let ready = release.clone();
        app.session.media.file_probe = Some(Arc::new(move |p| {
            let _keep_until_worker_exits = &exited;
            tx.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !ready.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            original_for_job(p)
        }));
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(path.into())], &mut app);
        pending.tick(&mut app);
        import::tick(&mut app, &egui::Context::default());
        rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        // Only the worker now owns the instrumented probe.
        app.session.media.file_probe = Some(original);
        let task = app.session.activity.list()[0].id;
        app.run("activity.cancel", json!({"id":task})).unwrap();
        // Switching must work immediately after cancellation, even before the next frame.
        let status = app.import.as_ref().map(|t| t.status());
        let result = app.run("app.openLibrary", json!({"path": dir.join("B").to_str().unwrap()}));
        eprintln!("After accepted cancellation: status={status:?}; switch={result:?}; activity={:?}", app.session.activity.list());
        release.store(true, Ordering::SeqCst);
        exited_rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        finish(&mut app);
        assert!(app.session.catalog.is_empty(), "late results must not enter the current library");
        app.run("app.openLibrary", json!({"path": dir.join("A").to_str().unwrap()})).unwrap();
        assert!(app.session.catalog.is_empty(), "late results must not enter the original library either");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result.is_ok(), "accepted cancellation must provide a way out of a blocked native Add import: {result:?}");
    }
    #[test]
    fn quitting_with_queued_native_documents_must_ask_before_discarding() {
        let (dir, path, mut app) = fixture("quit-queued");
        let candidates = lightcraft_engine::import::scan(&mut app.session, std::slice::from_ref(&path));
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(import::ImportDialog::new(candidates)) });
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(path.into())], &mut app);
        pending.tick(&mut app);
        assert_eq!(pending.batches.len(), 1);
        assert!(app.import.is_none());
        let can_close = lightcraft_ui_egui::panels::notices::may_close(&mut app);
        eprintln!(
            "Queued={}, activity={:?}, may_close={can_close}, prompt={:?}",
            app.pending_document_opens,
            app.session.activity.list(),
            app.quit_prompt
        );
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(!can_close, "warn before silently discarding accepted native requests");
    }
    #[test]
    fn cancellation_rejects_batches_prepared_before_the_next_frame() {
        use std::sync::Arc;
        let (dir, path, mut app) = fixture("cancel-prepared");
        let original = app.session.media.file_probe.clone().unwrap();
        let probe = original.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        struct Exited(std::sync::mpsc::Sender<()>);
        impl Drop for Exited {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let exited = Exited(tx);
        app.session.media.file_probe = Some(Arc::new(move |p| {
            let _keep_until_worker_exits = &exited;
            probe(p)
        }));
        import::start_documents(&mut app, vec![path]).unwrap();
        import::tick(&mut app, &egui::Context::default());
        app.session.media.file_probe = Some(original);
        // The worker has sent its batch, but no UI frame has accepted it yet.
        rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        let id = app.session.activity.list()[0].id;
        app.run("activity.cancel", json!({"id": id})).unwrap();
        finish(&mut app);
        assert!(app.session.catalog.is_empty(), "cancellation must reject buffered results too");
        assert!(app.session.activity.list().is_empty());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn library_generation_identifies_an_opening_not_a_folder_name() {
        let (dir, _, mut app) = fixture("identity");
        let initial = app.session.library_generation();
        std::fs::rename(dir.join("A"), dir.join("renamed")).unwrap();
        assert_eq!(initial, app.session.library_generation());
        std::fs::rename(dir.join("renamed"), dir.join("A")).unwrap();
        app.session.open_library(dir.join("A"), false).unwrap();
        assert_ne!(initial, app.session.library_generation());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod round3_queue_regressions {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{Services, import, state::Dialog};
    use serde_json::json;

    #[test]
    fn queued_native_requests_can_be_cancelled_before_the_importer_is_ready() {
        let mut app = LightcraftApp::new(Session::new(), Services::default());
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(import::ImportDialog::default()) });
        let mut pending = PendingOpens::default();
        pending.enqueue(vec![Ok(std::env::temp_dir().join("cancel-queued.png"))], &mut app);
        pending.tick(&mut app);
        let activities = app.session.activity.list();
        assert_eq!(activities.len(), 1, "queued requests need a cancellable activity row");
        app.run("activity.cancel", json!({"id": activities[0].id})).unwrap();
        pending.tick(&mut app);
        assert!(pending.batches.is_empty());
        assert_eq!(app.pending_document_opens, 0);
        assert!(app.pending_document_library.is_none());
        app.ui.dialog = None;
        pending.tick(&mut app);
        assert!(app.import.is_none());
        assert!(lightcraft_ui_egui::panels::notices::may_close(&mut app));
    }
}

#[cfg(test)]
mod round3_quit_regressions {
    use super::*;
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{Services, headless::Headless, import, panels::notices, state::Dialog};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn queued_file_count_and_quit_choices_preserve_or_discard_requests() {
        for n in [1, 2] {
            let mut app = LightcraftApp::new(Session::new(), Services::default());
            app.ui.dialog = Some(Dialog::Import { opts: Box::new(import::ImportDialog::default()) });
            let mut pending = PendingOpens::default();
            // One request can contain several files. The warning counts files, not batches.
            pending.enqueue((0..n).map(|i| Ok(std::env::temp_dir().join(format!("quit-queued-{i}.png")))).collect(), &mut app);
            pending.tick(&mut app);
            assert!(!notices::may_close(&mut app));
            let message = app.quit_prompt.as_ref().unwrap().text();
            let expected = if n == 1 { "1 file you opened hasn't been imported yet." } else { "2 files you opened haven't been imported yet." };
            assert!(message.contains(expected), "{message}");
            let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
            for button in ["button:quitCancel", "button:quitAnyway"] {
                assert!(!notices::may_close(&mut h.app));
                // First-use font installation and the window's sizing pass precede input.
                for _ in 0..4 {
                    h.step();
                }
                assert_eq!(h.request("ui.clickWidget", json!({"id": button}), Duration::from_secs(20))["ok"], true);
                for _ in 0..4 {
                    h.step();
                }
                pending.tick(&mut h.app);
                if button == "button:quitCancel" {
                    assert!(!h.quit_requested());
                    assert!(h.app.quit_prompt.is_none());
                    assert_eq!(pending.batches.len(), 1);
                    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })));
                } else {
                    assert!(h.quit_requested());
                    assert!(pending.batches.is_empty());
                    assert!(h.app.import.is_none());
                }
            }
        }
    }
}
