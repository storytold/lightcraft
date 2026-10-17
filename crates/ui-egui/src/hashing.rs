//! Content hashes of photos that joined the library from Local (see `lightcraft_engine::hashing`):
//! browsing a folder doesn't read whole files, so their hashes are worked out once they are added,
//! on a worker thread, and recorded back here unless another library was opened meanwhile.

use lightcraft_engine::hashing::HashJob;

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
    let work = move || jobs.iter().filter_map(HashJob::run).collect::<Vec<_>>();
    let done = move |app: &mut LightcraftApp, _: &egui::Context, results: Vec<lightcraft_engine::hashing::HashDone>| {
        if app.session.library_generation() != library {
            return;
        }
        for d in results {
            app.session.learn_content_hash(d);
        }
    };
    if let Err(e) = crate::tasks::spawn(app, LABEL, Some("hash"), work, done) {
        log::warn!("{e}");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lightcraft_catalog::Op;
    use serde_json::json;

    use crate::LightcraftApp;

    /// Add to My Photos on a browsed photo without a hash (as browsing leaves a raw): the hash is
    /// worked out on a worker thread, shown in the activity stack meanwhile, and recorded.
    #[test]
    fn added_photos_get_their_content_hash_in_the_background() {
        let dir = std::env::temp_dir().join(format!("lc-ui-hashing-{}", std::process::id()));
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
        let ctx = egui::Context::default();
        app.run("photo.addToLibrary", json!({"ids": [p.id.0]})).unwrap();
        assert!(app.session.catalog.photo(p.id).unwrap().content_hash.is_none(), "not on the UI thread");
        super::tick(&mut app);
        let rows = app.session.activity.list();
        assert!(rows.iter().any(|r| r.kind == "hash" && r.label == super::LABEL), "{rows:?}");
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(p.id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        assert!(app.session.activity.list().iter().all(|r| r.kind != "hash"), "the row goes with the task");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
