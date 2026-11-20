//! Content hashes of photos that joined the library from Local (see `lightcraft_engine::hashing`):
//! browsing a folder doesn't read whole files, so their hashes are worked out once they are added,
//! on a worker thread, and recorded back here unless another library was opened meanwhile. A file
//! that can't be read then (a share gone offline, a file changed since), or a result the session
//! turns down, is tried again later.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use lightcraft_catalog::PhotoId;
// (`std::time::Instant::now` panics on wasm32)
use web_time::Instant;

use crate::LightcraftApp;

/// The activity stack's label while the files are read.
pub const LABEL: &str = "Checking added photos";

/// The first wait before a photo whose file couldn't be read is tried again; it doubles each time
/// that photo fails again, up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// Added photos whose hash couldn't be worked out (their file unreadable or changed, or the result
/// no longer applied), waiting to be queued again. They don't hold up switching libraries: a file
/// can stay offline for days.
#[derive(Debug, Default)]
pub struct Retry {
    /// Each photo waiting and when it is queued again, in the order they first failed. A photo that
    /// fails later doesn't move the others' times.
    waiting: Vec<(PhotoId, Instant)>,
    /// Each photo's wait after its next failure: it grows while the photo keeps failing.
    waits: HashMap<PhotoId, Duration>,
    /// The library they belong to (photo ids are per library).
    library: u64,
}

impl Retry {
    /// `ids` failed at `now`: each waits its own time (one already waiting keeps its time).
    fn fail(&mut self, library: u64, ids: Vec<PhotoId>, now: Instant) {
        if ids.is_empty() {
            return;
        }
        if self.library != library {
            *self = Retry { library, ..Retry::default() };
        }
        // a set of those waiting: a long round of failures stays linear, not quadratic
        let mut waiting: HashSet<PhotoId> = self.waiting.iter().map(|&(id, _)| id).collect();
        for id in ids {
            if !waiting.insert(id) {
                continue;
            }
            let wait = self.waits.get(&id).copied().unwrap_or(RETRY_FIRST);
            self.waiting.push((id, now.checked_add(wait).unwrap_or(now)));
            self.waits.insert(id, wait.saturating_mul(2).min(RETRY_MAX));
        }
    }

    /// `id`'s hash was recorded: a later failure starts from the first wait again.
    fn recorded(&mut self, id: PhotoId) {
        self.waits.remove(&id);
    }

    /// The photos due to be queued again in `library` at `now` (those of another library are dropped).
    fn due(&mut self, library: u64, now: Instant) -> Vec<PhotoId> {
        if self.library != library {
            *self = Retry::default();
            return Vec::new();
        }
        let (due, waiting) = std::mem::take(&mut self.waiting).into_iter().partition(|&(_, at)| at <= now);
        self.waiting = waiting;
        due.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
    }

    /// When the next photo is due.
    fn next(&self) -> Option<Instant> {
        self.waiting.iter().map(|&(_, at)| at).min()
    }
}

/// Start working out the queued hashes (called every frame); one batch at a time, later ones wait.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let library = app.session.library_generation();
    let again = app.hash_retry.due(library, Instant::now());
    if !again.is_empty() {
        app.session.queue_hash(again);
    }
    // nothing else may ask for a frame when the retry is due (an idle window draws none)
    if let Some(at) = app.hash_retry.next() {
        ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
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
        let mut failed = failed;
        for d in results {
            let id = d.id;
            // A result the session no longer takes (the photo was relinked meanwhile, say) is tried
            // again, as the engine does where it hashes at once; one whose photo wants no hash any
            // more (Local again, deleted) is dropped when it comes due.
            if app.session.learn_content_hash(d) {
                app.hash_retry.recorded(id);
            } else {
                failed.push(id);
            }
        }
        app.hash_retry.fail(library, failed, Instant::now());
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
        super::tick(&mut app, &ctx);
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
        let err = crate::dialogs::settings::can_switch_library(&app).unwrap_err();
        assert!(err.contains("checking added photos"), "{err}");
        super::tick(&mut app, &ctx);
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
        super::tick(&mut app, &ctx);
        assert!(app.session.hashes_queued(), "the photos are queued again");
        assert!(!app.tasks.is_running(super::LABEL));
        assert!(app.session.activity.list().iter().all(|r| r.kind != "hash"), "no row left behind");
        super::tick(&mut app, &ctx);
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
        super::tick(&mut app, &ctx);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none());
        assert!(!app.session.hashes_queued(), "not queued again at once: the file would be read every frame");
        assert_eq!(app.hash_retry.waiting.iter().map(|w| w.0).collect::<Vec<_>>(), vec![id]);
        assert!(crate::dialogs::settings::can_switch_library(&app).is_ok(), "an offline file doesn't block switching");
        super::tick(&mut app, &ctx);
        assert!(!app.tasks.is_running(super::LABEL), "not before its wait");
        std::fs::write(&file, &bytes).unwrap();
        app.hash_retry.waiting[0].1 = web_time::Instant::now();
        super::tick(&mut app, &ctx);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        assert!(app.hash_retry.waiting.is_empty());
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
        super::tick(&mut app, &ctx);
        app.run("edit.undo", json!({})).unwrap();
        assert!(app.session.catalog.photo(id).unwrap().local);
        drop(hold);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none(), "not recorded on a Local photo");
        assert!(!app.session.hashes_queued());
        app.run("edit.redo", json!({})).unwrap();
        assert!(app.session.hashes_queued(), "the redo queues it again");
        super::tick(&mut app, &ctx);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&file.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each time a photo fails again it waits twice as long as before, up to an hour; photos of
    /// another library are dropped.
    #[test]
    fn retries_back_off_and_stay_in_their_library() {
        let mut r = super::Retry::default();
        let a = lightcraft_catalog::PhotoId(1);
        let t0 = web_time::Instant::now();
        r.fail(7, vec![a], t0);
        assert_eq!(r.next(), Some(t0 + super::RETRY_FIRST));
        r.fail(7, vec![a], t0);
        assert_eq!(r.waiting.len(), 1, "once each");
        assert_eq!(r.due(7, t0 + super::RETRY_FIRST), vec![a]);
        r.fail(7, vec![a], t0);
        assert_eq!(r.next(), Some(t0 + super::RETRY_FIRST * 2), "it failed again: a longer wait");
        for _ in 0..20 {
            r.due(7, t0 + super::RETRY_MAX * 2);
            r.fail(7, vec![a], t0);
        }
        assert_eq!(r.next(), Some(t0 + super::RETRY_MAX));
        r.recorded(a);
        r.due(7, t0 + super::RETRY_MAX);
        r.fail(7, vec![a], t0);
        assert_eq!(r.next(), Some(t0 + super::RETRY_FIRST), "recorded meanwhile: from the first wait again");
        let b = lightcraft_catalog::PhotoId(2);
        r.fail(7, vec![b, a, b], t0);
        assert_eq!(r.waiting.iter().map(|w| w.0).collect::<Vec<_>>(), vec![a, b], "each once, in the order they first failed");
        assert!(r.due(7, t0).is_empty(), "not yet");
        assert!(r.due(8, t0 + super::RETRY_MAX * 2).is_empty(), "another library's photos are dropped");
        assert!(r.waiting.is_empty());
    }

    /// A photo that fails while another waits doesn't put the other's retry off: each keeps its
    /// own time.
    #[test]
    fn a_new_failure_keeps_an_earlier_retry_on_time() {
        let mut r = super::Retry::default();
        let (a, b) = (lightcraft_catalog::PhotoId(1), lightcraft_catalog::PhotoId(2));
        let t0 = web_time::Instant::now();
        r.fail(7, vec![a], t0);
        r.fail(7, vec![b], t0 + Duration::from_secs(20));
        assert!(r.due(7, t0 + super::RETRY_FIRST - Duration::from_secs(1)).is_empty());
        assert_eq!(r.due(7, t0 + super::RETRY_FIRST), vec![a], "still due at its own time");
        assert_eq!(r.next(), Some(t0 + Duration::from_secs(20) + super::RETRY_FIRST));
        assert_eq!(r.due(7, t0 + Duration::from_secs(20) + super::RETRY_FIRST), vec![b]);
    }

    /// A result the session turns down (the photo was relinked to a copy of its file while it was
    /// read) is tried again, and the photo then gets its hash.
    #[test]
    fn a_result_the_session_turns_down_is_tried_again() {
        let (mut app, id, file, dir) = added_unhashed_photo("rejected");
        let ctx = egui::Context::default();
        let hold = HOLD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        super::tick(&mut app, &ctx);
        let copy = dir.join("b.png");
        std::fs::copy(&file, &copy).unwrap();
        let path = copy.to_string_lossy().to_string();
        app.session
            .catalog
            .apply(Op::Relink { id, file_name: "b.png".into(), source: lightcraft_catalog::Source::File { path: path.clone() }, format: None })
            .unwrap();
        drop(hold);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert!(app.session.catalog.photo(id).unwrap().content_hash.is_none(), "worked out for the other path");
        assert_eq!(app.hash_retry.waiting.iter().map(|w| w.0).collect::<Vec<_>>(), vec![id], "tried again later");
        app.hash_retry.waiting[0].1 = web_time::Instant::now();
        super::tick(&mut app, &ctx);
        assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
        assert_eq!(app.session.catalog.photo(id).unwrap().content_hash, lightcraft_engine::hashing::file_hash(&path));
        assert!(app.hash_retry.waiting.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A batch that finishes while more photos wait in the queue asks for another frame: the tick
    /// that would take them ran before the batch's result was applied.
    #[test]
    fn a_finished_batch_asks_for_a_frame_when_more_photos_wait() {
        let (mut app, id, _file, dir) = added_unhashed_photo("repaint");
        super::tick(&mut app, &egui::Context::default());
        app.session.queue_hash([id]);
        let t0 = web_time::Instant::now();
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

    /// Photos waiting to be tried again ask for a frame when their wait is over: an idle window
    /// draws none, and the retry happens only in a frame.
    #[test]
    fn a_waiting_retry_asks_for_a_frame_at_its_time() {
        let (mut app, id, _file, dir) = added_unhashed_photo("retry-frame");
        // a fresh, settled context: only the tick can ask it for a frame
        let settled = || {
            let ctx = egui::Context::default();
            for _ in 0..8 {
                if !ctx.has_requested_repaint() {
                    break;
                }
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            }
            assert!(!ctx.has_requested_repaint());
            ctx
        };
        app.session.take_hash_jobs();
        let ctx = settled();
        super::tick(&mut app, &ctx);
        assert!(!ctx.has_requested_repaint(), "nothing waits: no frame asked for");
        let library = app.session.library_generation();
        app.hash_retry.fail(library, vec![id], web_time::Instant::now());
        let ctx = settled();
        super::tick(&mut app, &ctx);
        assert!(!app.session.hashes_queued(), "not yet due");
        assert!(ctx.has_requested_repaint(), "the retry would wait for unrelated input");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
