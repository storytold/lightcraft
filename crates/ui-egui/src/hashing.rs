//! Content hashes of photos that joined the library from Local (see `lightcraft_engine::hashing`):
//! browsing a folder doesn't read whole files, so their hashes are worked out once they are added,
//! on a worker thread, and recorded back here unless another library was opened meanwhile.

use crate::LightcraftApp;

/// The activity stack's label while the files are read.
pub const LABEL: &str = "Checking added photos";

/// Start working out the queued hashes (called every frame); one batch at a time, later ones wait.
pub fn tick(app: &mut LightcraftApp) {
    if !app.session.hashes_queued() || app.tasks.is_running(LABEL) {
        return;
    }
    let library = app.session.library_generation();
    let jobs = app.session.take_hash_jobs();
    if jobs.is_empty() {
        return;
    }
    // Kept to put them back in the queue if the worker can't start (the queue is now empty).
    let ids: Vec<_> = jobs.iter().map(|j| j.id).collect();
    // The row counts the photos; no ✕: the queue lives only in memory, so a stopped batch's photos
    // would never get their hash. The worker owns the row, so it goes however the worker ends.
    let row = app.session.activity.start("hash", LABEL, lightcraft_engine::activity::Cancel::No);
    let total = u64::try_from(jobs.len()).unwrap_or(u64::MAX);
    row.progress(0, total);
    let work = move || {
        let row = row;
        let mut out = Vec::with_capacity(jobs.len());
        for (i, job) in (1u64..).zip(jobs.iter()) {
            out.extend(job.run());
            row.progress(i, total);
        }
        out
    };
    let done = move |app: &mut LightcraftApp, _: &egui::Context, results: Vec<lightcraft_engine::hashing::HashDone>| {
        if app.session.library_generation() != library {
            return;
        }
        for d in results {
            app.session.learn_content_hash(d);
        }
    };
    // On failure the worker closure, and the row with it, is dropped unrun; the photos go back in
    // the queue for a later frame instead of never getting their hash.
    if let Err(e) = crate::tasks::spawn(app, LABEL, None, work, done) {
        log::warn!("{e}");
        app.session.queue_hash(ids);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lightcraft_catalog::Op;
    use serde_json::json;

    use crate::LightcraftApp;

    /// A browsed photo without a hash (as browsing leaves a raw), in a folder of its own, added to
    /// My Photos: the app, the photo, its file and the folder.
    fn added_unhashed_photo(tag: &str) -> (LightcraftApp, lightcraft_catalog::PhotoId, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("lc-ui-hashing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 40, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        let file = dir.join("a.png");
        std::fs::write(&file, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let mut session = lightcraft_engine::Session::new().with_fs();
        session.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
        let p = session.catalog.photos().find(|p| p.local).unwrap().clone();
        let unhashed = Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: None,
            preview_only: None,
            preview_checked: String::new(),
        };
        session.catalog.apply(unhashed).unwrap();
        let mut app = LightcraftApp::new(session, Default::default());
        app.run("photo.addToLibrary", json!({"ids": [p.id.0]})).unwrap();
        (app, p.id, file, dir)
    }

    /// Add to My Photos on a browsed photo without a hash: the hash is worked out on a worker
    /// thread, shown in the activity stack meanwhile, and recorded.
    #[test]
    fn added_photos_get_their_content_hash_in_the_background() {
        let (mut app, id, file, dir) = added_unhashed_photo("bg");
        let ctx = egui::Context::default();
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none(), "not on the UI thread");
        super::tick(&mut app);
        let rows: Vec<_> = app.session.activity.list().into_iter().filter(|r| r.kind == "hash").collect();
        assert_eq!(rows.len(), 1, "one row, not one per layer: {rows:?}");
        assert_eq!(rows[0].label, super::LABEL);
        assert_eq!(rows[0].total, 1, "the row counts the photos: {rows:?}");
        assert!(!rows[0].cancellable, "no ✕: a stopped batch would never be hashed");
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        assert!(app.session.activity.list().iter().all(|r| r.kind != "hash"), "the row goes with the task");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Switching libraries waits for the added photos' hashes, queued or being worked out: their
    /// results would be dropped (another library is open) and the queue isn't rebuilt on reopen.
    #[test]
    fn switching_libraries_waits_for_the_hashes() {
        let (mut app, _id, _file, dir) = added_unhashed_photo("switch");
        let ctx = egui::Context::default();
        assert!(app.session.hashes_queued());
        let err = crate::dialogs::settings::can_switch_library(&app).unwrap_err();
        assert!(err.contains("checking added photos"), "{err}");
        super::tick(&mut app);
        assert!(crate::dialogs::settings::can_switch_library(&app).is_err(), "refused while the worker runs");
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(crate::dialogs::settings::can_switch_library(&app).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the worker thread can't be started, the photos go back in the queue (and their row
    /// goes) instead of never getting their hash; a later tick hashes them.
    #[test]
    fn a_worker_that_cannot_start_leaves_the_photos_queued() {
        let (mut app, id, file, dir) = added_unhashed_photo("nospawn");
        let ctx = egui::Context::default();
        crate::tasks::fail_next_spawn();
        super::tick(&mut app);
        assert!(app.session.hashes_queued(), "the photos are queued again");
        assert!(!app.tasks.is_running(super::LABEL));
        assert!(app.session.activity.list().iter().all(|r| r.kind != "hash"), "no row left behind");
        super::tick(&mut app);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        assert!(!app.session.hashes_queued());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
