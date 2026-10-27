use super::*;
use lightcraft_catalog::{Catalog, Op, Photo, Source};

#[test]
fn render_failures_identify_the_file_for_thumbnail_and_view_slots() {
    let source = Source::File { path: "/photos/trip/IMG_0042.nef".into() };
    for slot in [Slot::Thumb(PhotoId(42)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Region] {
        let message = render_failure_message(slot, PhotoId(42), Some(&source), "unsupported compression");
        assert!(message.contains("/photos/trip/IMG_0042.nef"), "{message}");
        assert!(message.contains("PhotoId(42)"), "{message}");
        assert!(message.ends_with(": unsupported compression"), "{message}");
    }
}

#[test]
fn render_failure_paths_are_escaped_and_missing_sources_still_identified() {
    let source = Source::File { path: "line\nbreak.nef".into() };
    let message = render_failure_message(Slot::Main, PhotoId(42), Some(&source), "decode failed");
    assert!(!message.contains('\n'));
    assert!(message.contains("line\\nbreak.nef"));
    assert!(render_failure_message(Slot::Main, PhotoId(42), None, "decode failed").contains("PhotoId(42), source unavailable"));
    let demo = Source::Demo { scene: 7 };
    assert!(render_failure_message(Slot::Main, PhotoId(42), Some(&demo), "decode failed").contains("demo scene 7"));
}

struct Blocked;
impl RenderOffload for Blocked {
    fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
        Some(job)
    }
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        vec![]
    }
}
fn photo() -> Arc<Photo> {
    Arc::new(Photo::new(PhotoId(1), Source::File { path: "audit.jpg".into() }, "audit.jpg", "JPEG", 6000, 4000, "2026-01-01"))
}
#[test]
fn snapshot_tracks_cow_edits_undo_reload_and_replacement() {
    let mut catalog = Catalog::default();
    catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
    let old = catalog.photo(PhotoId(1)).unwrap().clone();
    let mut r = Renderer::default();
    r.remember_thumb(&old, 384, 77, false);
    r.pending.insert(Slot::Thumb(PhotoId(1)), (77, 10));
    assert!(r.thumb_current(&old, 384, 10));
    assert!(!r.thumb_current(&old, 512, 10));
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.light.exposure = 1.0;
    let undo = catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings), label: "edit".into(), edited: None }).unwrap();
    assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
    catalog.apply(undo).unwrap();
    assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
    catalog
        .apply(Op::SetContent {
            id: PhotoId(1),
            width: 6000,
            height: 4000,
            file_size: 99,
            content_hash: Some("changed".into()),
            preview_only: None,
            preview_checked: String::new(),
        })
        .unwrap();
    assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
    assert!(!r.thumb_current(&photo(), 384, 10));
    r.forget_all();
    assert!(r.thumb_inputs.is_empty());
}
#[test]
fn queue_promotions_running_work_and_cancellation() {
    let p = photo();
    let slot = Slot::Thumb(p.id);
    let mut session = Session::new();
    session.catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
    let job = session.thumb_job(p.id, 384).unwrap();
    let key = job.key;
    let mut r = Renderer::default();
    r.set_offload(Box::new(Blocked));
    r.remember_thumb(&p, 384, key, false);
    r.request(slot, job, 5);
    assert!(r.thumb_current(&p, 384, 5));
    assert!(!r.thumb_current(&p, 384, 10));
    r.request(slot, session.thumb_job(p.id, 384).unwrap(), 10);
    assert_eq!(r.pending.get(&slot), Some(&(key, 10)));
    r.queue.clear(); // running work: cannot reprioritize, just as Renderer::request
    assert!(r.thumb_current(&p, 384, 5));
    r.pending.clear();
    assert!(!r.thumb_current(&p, 384, 10));
    r.evict_thumbs(&Default::default(), 600);
    assert!(r.thumb_inputs.is_empty());
}
fn queued_quick_thumbnail(id: PhotoId) -> (Arc<Photo>, QuickJob) {
    let mut session = Session::new();
    session.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
    let mut raw = (*photo()).clone();
    raw.id = id;
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    session.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    let p = session.catalog.photo(id).unwrap().clone();
    let job = session.thumb_job(id, 384).unwrap();
    (p, session.quick_thumb_job(&job).unwrap())
}

#[test]
fn queued_standin_promotes_from_prefetch_to_visible_without_restarting() {
    let (p, quick) = queued_quick_thumbnail(PhotoId(1));
    let (_, other) = queued_quick_thumbnail(PhotoId(2));
    let slot = Slot::ThumbQuick(p.id);
    // Deterministic queue: no native worker starts.
    let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
    r.remember_thumb(&p, 384, quick.key, true);
    r.request_quick(slot, quick.clone(), 6); // grid prefetch: 5 + 1
    r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9); // filmstrip: 8 + 1
    let request_id = r.requests[&slot].id;
    assert!(r.thumb_current(&p, 384, 5));
    assert!(!r.thumb_current(&p, 384, 10));
    r.request_quick(slot, quick.clone(), 11); // visible grid: 10 + 1
    assert!(r.thumb_current(&p, 384, 10));
    r.request_quick(slot, quick.clone(), 11);
    assert_eq!(r.requests[&slot].id, request_id);
    assert_eq!(r.pool.queued(), 2);
    r.pool.run_inline(1);
    let done = r.pool.try_recv().unwrap();
    assert_eq!(done.slot, slot);
    assert_eq!(done.result.result.request_id, request_id);
    assert_eq!(done.result.result.quick, Some(QuickSource::Embedded));
    // A finished result awaiting collection also must not start another extraction.
    r.request_quick(slot, quick, 6);
    assert!(r.thumb_current(&p, 384, 5));
    assert_eq!(r.pool.queued(), 1);
}

#[test]
fn queued_standin_demotes_when_it_leaves_the_visible_grid() {
    let (p, quick) = queued_quick_thumbnail(PhotoId(1));
    let (_, other) = queued_quick_thumbnail(PhotoId(2));
    let slot = Slot::ThumbQuick(p.id);
    let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
    r.remember_thumb(&p, 384, quick.key, true);
    r.request_quick(slot, quick.clone(), 11);
    r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9);
    assert!(!r.thumb_current(&p, 384, 5));
    r.request_quick(slot, quick, 6);
    assert!(r.thumb_current(&p, 384, 5));
    r.pool.run_inline(1);
    assert_eq!(r.pool.try_recv().unwrap().slot, Slot::ThumbQuick(PhotoId(2)));
}

#[test]
fn failures_retry_after_revision_and_quick_only_is_not_final() {
    let p = photo();
    let slot = Slot::Thumb(p.id);
    let mut r = Renderer::default();
    r.remember_thumb(&p, 384, 77, false);
    r.pending.insert(Slot::ThumbQuick(p.id), (77, 11));
    assert!(!r.thumb_current(&p, 384, 10));
    r.failed.insert(slot, (77, "no decoder".into(), 0, p.id));
    assert!(r.thumb_current(&p, 384, 10));
    assert_eq!(r.failure_of(slot, p.id), Some("no decoder"));
    assert_eq!(r.failure_of(slot, PhotoId(p.id.0 + 1)), None, "another photo's error");
    r.catalog_rev = 1;
    assert!(!r.thumb_current(&p, 384, 10));
}

#[test]
fn a_fast_draft_is_drawn_in_the_frame_that_asked_for_it() {
    let mut s = Session::new();
    s.media.file_loader = Some(Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(8, 8), Default::default()))));
    s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
    let mut r = Renderer::with_worker_threads(1);
    let ctx = egui::Context::default();
    let budget = std::time::Duration::from_secs(10);
    // the last loupe render took longer than the budget (a CPU render): not waited for
    r.last_main_ms = 1e6;
    r.request(Slot::Main, s.render_job(PhotoId(1), 64, 64, false, true).unwrap(), 100);
    r.wait_for_draft(&ctx, &mut s, budget);
    assert!(r.is_pending(Slot::Main) && !r.textures.contains_key(&Slot::Main));
    // a fast one (GPU drafts): the texture is the new render when the wait returns
    r.last_main_ms = 1.0;
    r.request(Slot::Main, s.render_job(PhotoId(1), 32, 32, false, true).unwrap(), 100);
    let want = r.wanted(Slot::Main);
    r.wait_for_draft(&ctx, &mut s, budget);
    assert!(!r.is_pending(Slot::Main));
    assert_eq!(r.textures.get(&Slot::Main).map(|t| t.key), want);
}

#[derive(Default)]
struct Work {
    jobs: Vec<(Slot, RenderJob)>,
    finished: Vec<(Slot, RenderResult, f64)>,
}
struct Manual(std::rc::Rc<std::cell::RefCell<Work>>);
impl RenderOffload for Manual {
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
        self.0.borrow_mut().jobs.push((slot, job));
        None
    }
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        std::mem::take(&mut self.0.borrow_mut().finished)
    }
}
fn fixture() -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
    fixture_with(Session::new(), (*photo()).clone())
}
fn fixture_with(mut s: Session, photo: Photo) -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
    s.media.file_loader = Some(Arc::new(|_, _| {
        let mut image = lightcraft_raster::Rgb32f::new(8, 8);
        image.data.fill([0.3, 0.2, 0.1]);
        Ok((image, Default::default()))
    }));
    // (`with_fs` sets the file system's thumbnail loader, which wins over `file_loader`: thumbnails use the stub too)
    s.media.reduced_loader = None;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    let mut app = crate::LightcraftApp::new(s, crate::Services::default());
    app.renderer.keep_pixels = true;
    let work = std::rc::Rc::new(std::cell::RefCell::new(Work::default()));
    app.renderer.set_offload(Box::new(Manual(work.clone())));
    let ctx = egui::Context::default();
    app.renderer.poll(&ctx, &mut app.session);
    (app, work, ctx)
}
fn take_job(work: &std::rc::Rc<std::cell::RefCell<Work>>) -> (Slot, RenderJob) {
    work.borrow_mut().jobs.pop().unwrap()
}
fn finish(app: &mut crate::LightcraftApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, ctx: &egui::Context, slot: Slot, result: RenderResult) {
    work.borrow_mut().finished.push((slot, result, 0.0));
    app.renderer.poll(ctx, &mut app.session);
}
fn request(app: &mut crate::LightcraftApp) {
    crate::panels::grid::request_thumb(app, PhotoId(1), 256, 10);
}

fn relink_destination() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("lc-render-relinked-{}-{}.nef", std::process::id(), lightcraft_preview::next_tick()));
    std::fs::write(&path, b"replacement source").unwrap();
    path
}

#[test]
fn completed_failure_keeps_requested_source_after_relink() {
    let replacement = relink_destination();
    for slot in [Slot::Thumb(PhotoId(1)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Region] {
        let (mut app, work, ctx) = fixture();
        app.session.media.file_loader = Some(Arc::new(|_, _| Err("simulated decode failure".into())));
        let job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        app.renderer.request(slot, job, 100);
        let (submitted, job) = take_job(&work);
        assert_eq!(submitted, slot);
        let result = job.run();
        assert!(result.rendered.is_err(), "the old request has already failed");
        app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement})).unwrap();
        assert!(
            matches!(&app.session.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path } if path.ends_with(replacement.file_name().unwrap().to_str().unwrap()))
        );
        finish(&mut app, &work, &ctx, slot, result);
        assert_eq!(app.renderer.failure(slot), Some("simulated decode failure"));
        assert!(!app.renderer.is_pending(slot));
        let message = app.renderer.failure_messages.last().expect("the collector emitted a diagnostic");
        assert!(message.contains("audit.jpg"), "{message}");
        assert!(!message.contains("lc-render-relinked-"), "the failed request did not read the replacement: {message}");
        assert!(message.ends_with(": simulated decode failure"), "{message}");
    }
    std::fs::remove_file(replacement).unwrap();
}

#[test]
fn superseded_failure_does_not_take_the_new_requests_source() {
    let replacement = relink_destination();
    let (mut app, work, ctx) = fixture();
    app.session.media.file_loader = Some(Arc::new(|_, _| Err("simulated decode failure".into())));
    let slot = Slot::Main;
    let old_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
    app.renderer.request(slot, old_job, 100);
    let (_, old_job) = take_job(&work);
    let old_result = old_job.run();
    app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement})).unwrap();
    let new_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
    app.renderer.request(slot, new_job, 100);
    let (_, new_job) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, old_result);
    assert!(app.renderer.failure_messages.is_empty(), "obsolete failures stay suppressed");
    assert!(app.renderer.is_pending(slot));
    finish(&mut app, &work, &ctx, slot, new_job.run());
    let message = app.renderer.failure_messages.last().expect("new failure is reported");
    assert!(message.contains("lc-render-relinked-"), "{message}");
    assert!(!message.contains("audit.jpg"), "{message}");
    assert!(app.renderer.requests.is_empty(), "completed request identity is released");
    std::fs::remove_file(replacement).unwrap();
}

#[test]
fn grid_queued_standin_changes_priority_without_restarting() {
    let mut s = Session::new();
    s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
    let mut raw = (*photo()).clone();
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    let (mut app, work, ctx) = fixture_with(s, raw);
    app.renderer.pool = JobPool::new(0); // deterministic: no quick job starts until run_inline
    let id = PhotoId(1);
    let slot = Slot::ThumbQuick(id);

    crate::panels::grid::request_thumb(&mut app, id, 384, 5);
    let request_id = app.renderer.requests[&slot].id;
    assert_eq!(app.renderer.pending[&slot].1, 6);
    assert_eq!(app.renderer.pool.queued(), 1);
    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    assert_eq!(app.renderer.pending[&slot].1, 11, "the grid must promote its queued stand-in");
    assert_eq!(app.renderer.requests[&slot].id, request_id);
    assert_eq!(app.renderer.pool.queued(), 1);

    crate::panels::grid::request_thumb(&mut app, id, 384, 5);
    assert_eq!(app.renderer.pending[&slot].1, 6, "the grid must demote its queued stand-in");
    assert_eq!(app.renderer.requests[&slot].id, request_id);
    assert_eq!(app.renderer.pool.queued(), 1);
    let built = app.renderer.thumb_jobs_built;
    app.renderer.pool.run_inline(1);
    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    assert_eq!(app.renderer.pending[&slot].1, 6, "completed work must not be restarted");
    assert_eq!(app.renderer.requests[&slot].id, request_id);
    assert_eq!(app.renderer.pool.queued(), 0);
    app.renderer.poll(&ctx, &mut app.session);
    assert_eq!(app.renderer.textures[&slot].quick, Some(QuickSource::Embedded));

    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    crate::panels::grid::request_thumb(&mut app, id, 384, 5);
    assert_eq!(app.renderer.thumb_jobs_built, built);
    assert_eq!(app.renderer.pool.queued(), 0);
    assert!(!app.renderer.is_pending(Slot::Thumb(id)));
    assert!(!app.renderer.textures.contains_key(&Slot::Thumb(id)));
    assert!(work.borrow().jobs.is_empty(), "the final embedded thumbnail needs no real render");
}

#[test]
fn unchanged_ready_and_running_frames_build_no_jobs() {
    let (mut app, work, ctx) = fixture();
    request(&mut app);
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    for _ in 0..180 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    let (slot, job) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, job.run());
    for _ in 0..180 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    assert!(work.borrow().jobs.is_empty());
}

#[test]
fn a_slider_drag_leaves_the_edited_photos_thumbnail_until_it_ends() {
    let (mut app, work, ctx) = fixture();
    request(&mut app);
    let (slot, job) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, job.run());
    app.session.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
    app.session.execute("develop.beginInteraction", &serde_json::json!({"label": "Exposure"})).unwrap();
    for v in [0.1, 0.2, 0.3] {
        app.session.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": v})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1, "the drag's frames render no thumbnail of the photo");
    assert!(work.borrow().jobs.is_empty());
    app.session.execute("develop.endInteraction", &serde_json::json!({})).unwrap();
    request(&mut app);
    assert_eq!(app.renderer.thumb_jobs_built, 2, "the thumbnail follows once the drag ends");
}

#[test]
fn unchanged_queued_frames_build_no_jobs() {
    let (mut app, _, _) = fixture();
    app.renderer.set_offload(Box::new(Blocked));
    request(&mut app);
    for _ in 0..180 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    assert_eq!(app.renderer.queue.len(), 1);
}

#[test]
fn resizing_the_disk_cache_keeps_textures_and_skips_rebuilds() {
    let lib = std::env::temp_dir().join(format!("lc-ui-thumb-resize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&lib);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let (mut app, work, ctx) = fixture_with(s, (*photo()).clone());
    request(&mut app);
    let (slot, job) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, job.run());
    assert!(app.renderer.thumb(PhotoId(1)).is_some());
    let cache = app.session.media.rendered.clone();
    app.session.execute("library.preferences", &serde_json::json!({"cacheMb": 300})).unwrap();
    app.renderer.poll(&ctx, &mut app.session);
    // thumbnails are keyed by content: a new budget leaves them (and the cache) valid
    assert!(app.renderer.thumb(PhotoId(1)).is_some());
    assert!(Arc::ptr_eq(&cache, &app.session.media.rendered));
    for _ in 0..30 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    assert!(work.borrow().jobs.is_empty());
    // an explicit clear still drops them
    app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
    app.renderer.poll(&ctx, &mut app.session);
    assert!(app.renderer.thumb(PhotoId(1)).is_none());
    drop(app);
    let _ = std::fs::remove_dir_all(&lib);
}

/// A render that panics (a decoder bug) is a failure like any other: the slot stops waiting,
/// the error is kept (not retried every frame), and the pool's workers live on. Before, the
/// worker died without an answer and the slot stayed pending forever.
#[test]
fn a_panicking_render_is_a_failure_not_a_dead_worker() {
    let (mut app, _, ctx) = fixture_with(Session::new(), (*photo()).clone());
    app.session.media.file_loader = Some(Arc::new(|_, _| panic!("decoder bug")));
    app.renderer.offload = None; // the real job pool
    let slot = Slot::Thumb(PhotoId(1));
    let ask = |app: &mut crate::LightcraftApp| {
        if let Some(job) = app.session.thumb_job(PhotoId(1), 256) {
            app.renderer.request(slot, job, 30);
        }
        app.renderer.poll(&ctx, &mut app.session);
    };
    ask(&mut app);
    let t0 = std::time::Instant::now();
    while app.renderer.is_pending(slot) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the render never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.renderer.poll(&ctx, &mut app.session);
    }
    assert_eq!(app.renderer.failure(slot), Some("the render failed unexpectedly: decoder bug"));
    let workers = app.renderer.live_workers();
    assert!(workers > 0, "the worker that panicked is still there");
    let done = app.renderer.completed;
    for _ in 0..30 {
        ask(&mut app);
    }
    assert!(!app.renderer.is_pending(slot) && app.renderer.completed == done, "no retry loop");
    assert_eq!(app.renderer.live_workers(), workers);
}

#[test]
fn raw_quick_path_frames_build_no_jobs() {
    // an unedited raw: the grid shows its embedded preview (held here until released) and stops
    // there — the camera JPEG at 4 416 × 2 944 already covers a 512 px thumbnail, so decoding the
    // whole raw behind it only costs the file read (45 s a photo on a 1.2 MB/s share)
    let gate = Arc::new(std::sync::Mutex::new(()));
    let held = gate.lock().unwrap();
    let mut s = Session::new();
    let g = gate.clone();
    s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
        drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
    }));
    let mut raw = (*photo()).clone();
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    let (mut app, _, ctx) = fixture_with(s, raw);
    app.renderer.set_offload(Box::new(Blocked));
    let quick = Slot::ThumbQuick(PhotoId(1));
    // the stand-in is running
    for _ in 0..180 {
        request(&mut app);
    }
    assert!(app.renderer.is_pending(quick));
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    drop(held);
    let t0 = std::time::Instant::now();
    while !app.renderer.textures.contains_key(&quick) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the embedded preview never arrived");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.renderer.poll(&ctx, &mut app.session);
    }
    // the embedded preview is the thumbnail: no real render is queued behind it, and the grid
    // asking every frame neither rebuilds the job nor re-requests the preview
    for _ in 0..180 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1, "an unedited raw's embedded preview is the thumbnail");
    assert!(app.renderer.queue.is_empty(), "nothing is decoded behind it");
    assert!(!app.renderer.is_pending(Slot::Thumb(PhotoId(1))));

    // an edit takes the embedded preview out of the picture: the real thumbnail is rendered then
    let mut edited = (*photo()).clone();
    edited.kind = lightcraft_catalog::MediaKind::Raw;
    edited.develop = Arc::new({
        let mut d = edited.camera_defaults();
        d.light.exposure = 0.5;
        d
    });
    let (mut app, _, _ctx) = fixture_with(Session::new(), edited);
    app.renderer.set_offload(Box::new(Blocked));
    for _ in 0..180 {
        request(&mut app);
    }
    assert_eq!(app.renderer.thumb_jobs_built, 1);
    assert!(app.renderer.is_pending(Slot::Thumb(PhotoId(1))) || !app.renderer.queue.is_empty(), "an edited raw renders its thumbnail");
}

#[test]
fn closing_import_review_cancels_queued_and_running_identities() {
    let (mut app, work, ctx) = fixture();
    let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
    app.renderer.request(Slot::Import(0), job.clone(), 10);
    let (slot, running) = take_job(&work);
    app.renderer.set_offload(Box::new(Blocked));
    app.renderer.request(Slot::Import(1), job, 10);
    app.renderer.forget_imports();
    assert!(!app.renderer.is_pending(Slot::Import(0)));
    assert!(!app.renderer.is_pending(Slot::Import(1)));
    assert!(app.renderer.queue.is_empty());
    work.borrow_mut().finished.push((slot, running.run(), 0.0));
    app.renderer.set_offload(Box::new(Manual(work.clone())));
    app.renderer.poll(&ctx, &mut app.session);
    assert!(!app.renderer.textures.contains_key(&slot));
    assert_eq!(app.session.media.source_usage().0, 0);
}

#[test]
fn loupe_shows_superseded_drafts_newer_than_its_pixels_during_a_drag() {
    let (mut app, work, ctx) = fixture();
    let mut jobs = Vec::new();
    for exposure in [0.0, 1.0, 2.0] {
        let mut edit = lightcraft_develop::DevelopSettings::default();
        edit.light.exposure = exposure;
        app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
        let job = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().draft();
        app.renderer.request(Slot::Main, job, 100);
        jobs.push(take_job(&work).1);
    }
    let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
    let mut jobs = jobs.into_iter();
    let (first, second, last) = (jobs.next().unwrap(), jobs.next().unwrap(), jobs.next().unwrap());
    // the first draft lands while the newest is still rendering: the loupe moves on
    finish(&mut app, &work, &ctx, Slot::Main, first.run());
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[0]));
    assert!(app.renderer.is_pending(Slot::Main));
    finish(&mut app, &work, &ctx, Slot::Main, last.run());
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
    // a draft older than the pixels shown never replaces them
    finish(&mut app, &work, &ctx, Slot::Main, second.run());
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
    assert!(!app.renderer.is_pending(Slot::Main));
}

#[test]
fn a_drag_gets_its_live_hdr_histogram_beside_the_drafts() {
    let (mut app, work, ctx) = fixture();
    let mut jobs = Vec::new();
    for exposure in [1.0, 2.0] {
        let mut edit = lightcraft_develop::DevelopSettings::default();
        edit.hdr.enabled = true;
        edit.light.exposure = exposure;
        app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
        let job = app.session.render_job(PhotoId(1), 32, 32, false, true).unwrap();
        app.renderer.request(Slot::HdrHistogram, job, 95);
        jobs.push(take_job(&work).1);
    }
    let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
    let mut jobs = jobs.into_iter();
    let (first, last) = (jobs.next().unwrap(), jobs.next().unwrap());
    // superseded while the drag goes on, it still lands: the histogram keeps moving
    finish(&mut app, &work, &ctx, Slot::HdrHistogram, first.run());
    let live = app.renderer.live_hdr_histogram.clone().expect("a live HDR histogram");
    assert_eq!((live.0, live.1), (PhotoId(1), keys[0]));
    assert!(live.2.hdr.is_some(), "HDR bins");
    finish(&mut app, &work, &ctx, Slot::HdrHistogram, last.run());
    assert_eq!(app.renderer.live_hdr_histogram.as_ref().map(|l| l.1), Some(keys[1]));
    // only the histogram is kept: no texture
    assert!(!app.renderer.textures.contains_key(&Slot::HdrHistogram));
}

#[test]
fn library_reset_rejects_old_job_even_when_photo_id_and_key_are_reused() {
    let (mut app, work, ctx) = fixture();
    request(&mut app);
    let (slot, old) = take_job(&work);
    let old_result = old.run();
    app.renderer.forget_all();
    let photo = app.session.catalog.photo(PhotoId(1)).unwrap().as_ref().clone();
    app.session = lightcraft_engine::Session::new();
    app.session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    app.session.media.file_loader = Some(Arc::new(|_, _| {
        let mut image = lightcraft_raster::Rgb32f::new(8, 8);
        image.data.fill([0.1, 0.2, 0.9]);
        Ok((image, Default::default()))
    }));
    app.renderer.poll(&ctx, &mut app.session);
    request(&mut app);
    let (_, new) = take_job(&work);
    assert_eq!(old_result.key, new.key);
    assert_ne!(old_result.request_id, new.request_id);
    let expected = new.clone().run().rendered.unwrap().image.as_bytes();
    finish(&mut app, &work, &ctx, slot, new.run());
    finish(&mut app, &work, &ctx, slot, old_result);
    assert_eq!(
        app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn late_results_and_errors_cannot_replace_newest_pixels_or_sources() {
    for order in [0, 1, 2] {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, old) = take_job(&work);
        let mut edit = lightcraft_develop::DevelopSettings::default();
        edit.light.exposure = 1.0;
        app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        request(&mut app);
        let (_, new) = take_job(&work);
        let expected = new.clone().run().rendered.unwrap().image.as_bytes();
        if order != 0 {
            finish(&mut app, &work, &ctx, slot, new.run());
            // An obsolete source and error must not poison the current successful request.
            let mut stale = old.run();
            if order == 2 {
                stale.rendered = Err("old failure".into());
            }
            finish(&mut app, &work, &ctx, slot, stale);
        } else {
            finish(&mut app, &work, &ctx, slot, old.run());
            assert!(app.renderer.thumb(PhotoId(1)).is_none());
            assert_eq!(app.session.media.source_usage().0, 0);
            finish(&mut app, &work, &ctx, slot, new.run());
        }
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected
        );
        assert!(app.renderer.failure(slot).is_none());
    }
}

#[test]
fn undo_to_existing_texture_supersedes_pending_edit() {
    let (mut app, work, ctx) = fixture();
    request(&mut app);
    let (slot, first) = take_job(&work);
    let key = first.key;
    finish(&mut app, &work, &ctx, slot, first.run());
    let original = app.renderer.thumb(PhotoId(1)).unwrap().pixels.clone().unwrap();
    let mut edit = lightcraft_develop::DevelopSettings::default();
    edit.light.exposure = 1.0;
    let undo = app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
    request(&mut app);
    let (_, edited) = take_job(&work);
    app.session.catalog.apply(undo).unwrap();
    request(&mut app);
    let (_, restored) = take_job(&work);
    assert_eq!(restored.key, key);
    finish(&mut app, &work, &ctx, slot, restored.run());
    finish(&mut app, &work, &ctx, slot, edited.run());
    assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels, original.pixels);
}

#[test]
fn explicit_clear_repairs_same_key_bad_pixels_and_rejects_old_same_key_job() {
    let (mut app, work, ctx) = fixture();
    let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
    let expected = job.clone().run().rendered.unwrap().image;
    let (cache, key) = job.cache.clone().unwrap();
    let mut bad = expected.clone();
    bad.data.fill([255, 0, 255, 255]);
    cache.put(key, Arc::new(bad.clone()));
    request(&mut app);
    let (slot, corrupt) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, corrupt.run());
    assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels[0].to_array(), [255, 0, 255, 255]);
    // An old job can have the SAME render key and stale decoded pixels.
    let mut old = job.clone();
    old.source = lightcraft_engine::media::SourceRef::Loaded(Box::new(lightcraft_engine::media::DecodedSource::new(
        Arc::new(lightcraft_raster::Rgb32f::new(8, 8)),
        None,
    )));
    app.renderer.textures.clear();
    app.renderer.request(slot, old, 10);
    let (_, old) = take_job(&work);
    app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
    app.renderer.poll(&ctx, &mut app.session);
    assert!(app.renderer.thumb(PhotoId(1)).is_none());
    request(&mut app);
    let (_, repaired) = take_job(&work);
    assert_eq!(repaired.key, job.key);
    finish(&mut app, &work, &ctx, slot, repaired.run());
    finish(&mut app, &work, &ctx, slot, old.run());
    assert_eq!(cache.get(key).unwrap().as_bytes(), expected.as_bytes());
    assert_eq!(
        app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
        expected.as_bytes()
    );
}

#[test]
fn undo_to_embedded_thumb_rejects_late_full_render() {
    let mut s = Session::new();
    s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
    let mut raw = (*photo()).clone();
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    let (mut app, work, ctx) = fixture_with(s, raw);
    app.renderer.pool = JobPool::new(0);
    let id = PhotoId(1);
    let quick_slot = Slot::ThumbQuick(id);
    let full_slot = Slot::Thumb(id);
    let settings_a = app.session.catalog.photo(id).unwrap().develop.clone();

    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    assert_eq!(app.renderer.pool.run_inline(1), 1);
    app.renderer.poll(&ctx, &mut app.session);
    let (key_a, pixels_a) = {
        let shown = app.renderer.thumb(id).unwrap();
        assert_eq!(shown.quick, Some(QuickSource::Embedded));
        (shown.key, shown.pixels.as_ref().unwrap().pixels.clone())
    };
    assert!(app.renderer.textures.contains_key(&quick_slot));
    assert!(!app.renderer.textures.contains_key(&full_slot));
    assert!(work.borrow().jobs.is_empty());

    let mut settings_b = (*settings_a).clone();
    settings_b.light.exposure += 0.5;
    app.session.commit("Exposure", Op::SetDevelop { id, settings: Arc::new(settings_b), label: "Exposure".into(), edited: None }).unwrap();
    assert!(!app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    // Manual owns dispatched jobs: hold B until the real Undo command restores A.
    let (slot, held_b) = take_job(&work);
    assert_eq!(slot, full_slot);
    let key_b = held_b.key;
    assert_ne!(key_b, key_a);
    assert!(app.renderer.is_pending(full_slot));

    app.run("edit.undo", serde_json::json!({})).unwrap();
    assert_eq!(app.session.catalog.photo(id).unwrap().develop.as_ref(), settings_a.as_ref());
    assert!(app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    {
        let restored = app.renderer.thumb(id).unwrap();
        assert_eq!(restored.key, key_a);
        assert_eq!(restored.pixels.as_ref().unwrap().pixels, pixels_a);
        assert_eq!(restored.quick, Some(QuickSource::Embedded));
    }
    assert!(work.borrow().jobs.is_empty(), "restoring embedded-final A needs no full render");
    assert_eq!(app.renderer.pool.queued(), 0);

    let result_b = held_b.run();
    assert_eq!(result_b.key, key_b);
    let rendered_b = result_b.rendered.as_ref().expect("held edited thumbnail must render before the stale-result oracle");
    assert_ne!(color_image(&rendered_b.image).pixels, pixels_a, "the held result must be distinguishable from A");
    finish(&mut app, &work, &ctx, slot, result_b);

    let shown = app.renderer.thumb(id).unwrap();
    assert_eq!(shown.key, key_a, "late edited Thumb(B) must not replace restored embedded ThumbQuick(A)");
    assert_eq!(shown.pixels.as_ref().unwrap().pixels, pixels_a);
    assert_eq!(shown.quick, Some(QuickSource::Embedded));
    let quick = app.renderer.textures.get(&quick_slot).expect("the correct embedded thumbnail must remain installed");
    assert_eq!(quick.key, key_a);
    assert_eq!(quick.quick, Some(QuickSource::Embedded));
    assert_eq!(quick.pixels.as_ref().unwrap().pixels, pixels_a);
    assert!(!app.renderer.is_pending(full_slot));
}
