//! Content hashes of photos that joined the library from Local (see `lightcraft_engine::hashing`):
//! browsing a folder doesn't read whole files, so their hashes are worked out once they are added,
//! on a worker thread, and recorded back here unless another library was opened meanwhile. A file
//! that can't be read then (a share gone offline, a file changed since) is tried again later.

use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;

use crate::LightcraftApp;

/// The activity stack's label while the files are read.
pub const LABEL: &str = "Checking added photos";

/// The first wait before photos whose file couldn't be read are tried again; it doubles after each
/// round that fails, up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// Added photos whose hash couldn't be worked out (their file unreadable or changed), waiting to be
/// queued again. They don't hold up switching libraries: a file can stay offline for days.
#[derive(Debug, Default)]
pub struct Retry {
    ids: Vec<PhotoId>,
    /// The library they belong to (photo ids are per library).
    library: u64,
    /// When they are queued again.
    at: Option<Instant>,
    /// The wait after the next failed round.
    wait: Duration,
}

impl Retry {
    fn fail(&mut self, library: u64, ids: Vec<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        if self.library != library {
            self.ids.clear();
            self.library = library;
        }
        for id in ids {
            if !self.ids.contains(&id) {
                self.ids.push(id);
            }
        }
        let wait = if self.wait.is_zero() { RETRY_FIRST } else { self.wait };
        let now = Instant::now();
        self.at = Some(now.checked_add(wait).unwrap_or(now));
        self.wait = wait.saturating_mul(2).min(RETRY_MAX);
    }

    /// The photos due to be queued again in `library` (those of another library are dropped).
    fn due(&mut self, library: u64, now: Instant) -> Vec<PhotoId> {
        if self.library != library {
            *self = Retry::default();
            return Vec::new();
        }
        if self.at.is_none_or(|at| now < at) {
            return Vec::new();
        }
        self.at = None;
        std::mem::take(&mut self.ids)
    }
}

/// Start working out the queued hashes (called every frame); one batch at a time, later ones wait.
pub fn tick(app: &mut LightcraftApp) {
    let library = app.session.library_generation();
    let again = app.hash_retry.due(library, Instant::now());
    if !again.is_empty() {
        app.session.queue_hash(again);
    }
    if !app.session.hashes_queued() || app.tasks.is_running(LABEL) {
        return;
    }
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
        #[cfg(test)]
        drop(tests::HOLD.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let mut out = Vec::with_capacity(jobs.len());
        let mut failed = Vec::new();
        for (i, job) in (1u64..).zip(jobs.iter()) {
            match job.run() {
                Some(d) => out.push(d),
                None => failed.push(job.id),
            }
            row.progress(i, total);
        }
        (out, failed)
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, (results, failed): (Vec<lightcraft_engine::hashing::HashDone>, Vec<PhotoId>)| {
        if app.session.library_generation() != library {
            return;
        }
        for d in results {
            app.session.learn_content_hash(d);
        }
        app.hash_retry.fail(library, failed);
        // `tick` ran before this frame's results were applied: photos queued meanwhile wait for
        // the next frame, which nothing else may ask for.
        if app.session.hashes_queued() {
            ctx.request_repaint();
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

    /// Held by a test to keep a started batch from finishing (its row goes with it) until it has
    /// looked at it.
    pub(super) static HOLD: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let hold = HOLD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        super::tick(&mut app);
        let rows: Vec<_> = app.session.activity.list().into_iter().filter(|r| r.kind == "hash").collect();
        assert_eq!(rows.len(), 1, "one row, not one per layer: {rows:?}");
        assert_eq!(rows[0].label, super::LABEL);
        assert_eq!(rows[0].total, 1, "the row counts the photos: {rows:?}");
        assert!(!rows[0].cancellable, "no ✕: a stopped batch would never be hashed");
        drop(hold);
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
        let err = crate::panels::settings::can_switch_library(&app).unwrap_err();
        assert!(err.contains("checking added photos"), "{err}");
        super::tick(&mut app);
        assert!(crate::panels::settings::can_switch_library(&app).is_err(), "refused while the worker runs");
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(crate::panels::settings::can_switch_library(&app).is_ok());
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

    /// A file that can't be read when its hash is worked out (here: it changed size since it was
    /// added) is tried again later, on a growing wait, and doesn't hold up switching libraries.
    #[test]
    fn a_file_that_cannot_be_read_is_tried_again_later() {
        let (mut app, id, file, dir) = added_unhashed_photo("retry");
        let ctx = egui::Context::default();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, [bytes.as_slice(), b"more"].concat()).unwrap();
        super::tick(&mut app);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none());
        assert!(!app.session.hashes_queued(), "not queued again at once: the file would be read every frame");
        assert_eq!(app.hash_retry.ids, vec![id]);
        assert!(crate::panels::settings::can_switch_library(&app).is_ok(), "an offline file doesn't block switching");
        super::tick(&mut app);
        assert!(!app.tasks.is_running(super::LABEL), "not before its wait");
        std::fs::write(&file, &bytes).unwrap();
        app.hash_retry.at = Some(std::time::Instant::now());
        super::tick(&mut app);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        assert!(app.hash_retry.ids.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Add to My Photos undone while its batch was being worked out, then redone (FredApps on
    /// #2479): the batch's result is dropped (the photo was Local again), and the redo queues the
    /// photo once more, so the worker records its hash.
    #[test]
    fn a_redone_add_is_hashed_after_its_first_result_was_dropped() {
        let (mut app, id, file, dir) = added_unhashed_photo("redo");
        let ctx = egui::Context::default();
        let hold = HOLD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        super::tick(&mut app);
        app.run("edit.undo", json!({})).unwrap();
        assert!(app.session.catalog.photo(id).unwrap().local);
        drop(hold);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none(), "not recorded on a Local photo");
        assert!(!app.session.hashes_queued());
        app.run("edit.redo", json!({})).unwrap();
        assert!(app.session.hashes_queued(), "the redo queues it again");
        super::tick(&mut app);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each failed round waits twice as long as the one before, up to an hour; photos of another
    /// library are dropped.
    #[test]
    fn retries_back_off_and_stay_in_their_library() {
        let mut r = super::Retry::default();
        let a = lightcraft_catalog::PhotoId(1);
        r.fail(7, vec![a]);
        assert_eq!(r.wait, super::RETRY_FIRST * 2);
        r.fail(7, vec![a]);
        assert_eq!(r.ids, vec![a], "once each");
        assert_eq!(r.wait, super::RETRY_FIRST * 4);
        for _ in 0..20 {
            r.fail(7, vec![a]);
        }
        assert_eq!(r.wait, super::RETRY_MAX);
        let later = std::time::Instant::now() + super::RETRY_MAX * 2;
        assert!(r.due(7, std::time::Instant::now()).is_empty(), "not yet");
        assert!(r.due(8, later).is_empty(), "another library's photos are dropped");
        assert!(r.ids.is_empty());
    }

    /// A batch that finishes while more photos wait in the queue asks for another frame: the tick
    /// that would take them ran before the batch's result was applied.
    #[test]
    fn a_finished_batch_asks_for_a_frame_when_more_photos_wait() {
        let (mut app, id, _file, dir) = added_unhashed_photo("repaint");
        super::tick(&mut app);
        app.session.queue_hash([id]);
        let t0 = std::time::Instant::now();
        loop {
            // a fresh, settled context each time: only the batch's own result can ask it for a frame
            let ctx = egui::Context::default();
            for _ in 0..8 {
                if !ctx.has_requested_repaint() {
                    break;
                }
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            }
            assert!(!ctx.has_requested_repaint());
            crate::tasks::poll(&mut app, &ctx);
            if app.tasks.is_empty() {
                assert!(ctx.has_requested_repaint(), "the queued photos would wait for unrelated input");
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
