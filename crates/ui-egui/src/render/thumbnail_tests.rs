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
fn capture_readiness_excludes_prefetch_but_keeps_photo_and_thumbnail_work() {
    let p = photo();
    let mut session = Session::new();
    session.catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
    // Keep real requests queued without starting native workers.
    let mut renderer = Renderer { pool: JobPool::new(0), ..Default::default() };
    renderer.prefetch(Slot::Prefetch(0), session.thumb_job(p.id, 384).unwrap(), 1);
    renderer.prefetch(Slot::Prefetch(1), session.thumb_job(p.id, 384).unwrap(), 1);
    assert_eq!(renderer.in_flight(), 2);
    assert_eq!(renderer.in_flight_without_prefetch(), 0);
    for (index, slot) in [Slot::Main, Slot::Thumb(p.id), Slot::Variant(7), Slot::Preview, Slot::ThumbQuick(p.id)].into_iter().enumerate() {
        renderer.request(slot, session.thumb_job(p.id, 384).unwrap(), 10);
        assert_eq!(renderer.in_flight_without_prefetch(), index + 1);
        assert_eq!(renderer.in_flight(), index + 3);
    }
    assert_eq!(renderer.pool.run_inline(7), 7);
    // Waiting observes a completed job without swallowing the result needed by poll.
    assert!(renderer.wait_for_worker(std::time::Duration::ZERO));
    renderer.poll(&egui::Context::default(), &mut session);
    assert_eq!(renderer.in_flight_without_prefetch(), 0);
    assert_eq!(renderer.in_flight(), 0);
}
#[test]
fn snapshot_tracks_cow_edits_undo_reload_and_replacement() {
    let mut catalog = Catalog::default();
    catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
    let old = catalog.photo(PhotoId(1)).unwrap().clone();
    let mut r = Renderer::default();
    r.remember_thumb(&old, 384, 77, false, 10);
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
    r.remember_thumb(&p, 384, key, false, 5);
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
    r.remember_thumb(&p, 384, quick.key, true, 5);
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
    r.remember_thumb(&p, 384, quick.key, true, 10);
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
    r.remember_thumb(&p, 384, 77, false, 10);
    r.pending.insert(Slot::ThumbQuick(p.id), (77, 11));
    assert!(!r.thumb_current(&p, 384, 10));
    r.failed.insert(slot, (77, "no decoder".into(), 0, p.id));
    assert!(r.thumb_current(&p, 384, 10));
    assert_eq!(r.failure_of(slot, p.id), Some("no decoder"));
    assert_eq!(r.failure_of(slot, PhotoId(p.id.0 + 1)), None, "another photo's error");
    r.catalog_rev = 1;
    assert!(!r.thumb_current(&p, 384, 10));
}

/// A larger texture only serves a smaller bucket when it is the recorded request's picture.
/// This isolates the key guard: under a `contains_key`-only check the first assertion below
/// would return `true`.
#[test]
fn larger_texture_for_another_key_does_not_count_as_current() {
    let p = photo();
    let slot = Slot::Thumb(p.id);
    let ctx = egui::Context::default();
    let tex = |key| Tex {
        look: None,
        key,
        photo: p.id,
        tex: ctx.load_texture(format!("thumb-{key}"), egui::ColorImage::new([1, 1], vec![egui::Color32::BLACK]), Default::default()),
        size: [1, 1],
        histogram: None,
        ms: 0.0,
        quick: None,
        pixels: None,
    };
    let mut r = Renderer::default();
    // A completed larger bucket: the remembered request is bucket 512, key 77.
    r.remember_thumb(&p, 512, 77, false, 10);
    // Another key's pixels (for example the picture from before an edit) may still be
    // displayed, but they are not the remembered request's picture.
    r.textures.insert(slot, tex(66));
    assert!(!r.thumb_current(&p, 256, 10), "a mismatched texture key must not serve a smaller bucket");
    r.textures.insert(slot, tex(77));
    assert!(r.thumb_current(&p, 256, 10), "the recorded key's larger texture serves the smaller bucket");
}

/// Issue #2682: writes to other photos in the background (faces found, metadata read) moved
/// the catalog revision on and sent every undecodable file back to the decoder, 37 times each
/// in 154 s of idle. A failure is tried again only when its own photo changes, when the
/// catalog can't say which photos did, or, at any change, when its file is known to be missing.
#[test]
fn a_failed_thumbnail_is_retried_only_when_its_photo_changes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut catalog = Catalog::default();
    let p = photo();
    let other = PhotoId(2);
    catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
    let other_photo = Photo::new(other, Source::File { path: "other.jpg".into() }, "other.jpg", "JPEG", 60, 40, "2026-01-01");
    catalog.apply(Op::AddPhoto { photo: Box::new(other_photo) }).unwrap();
    // the files are there (an undecodable file is still a file)
    let here = Arc::new(AtomicBool::new(true));
    let mut files = lightcraft_engine::availability::Availability::default();
    let h = here.clone();
    files.set_probe(Arc::new(move |_| h.load(Ordering::SeqCst)));
    let slot = Slot::Thumb(p.id);
    let mut r = Renderer::default();
    r.follow_catalog(&catalog, &files);
    r.failed.insert(slot, (77, "unrecognised image format".into(), r.catalog_rev, p.id));
    assert!(!r.request_needed(slot, 77, 10), "a failure stands");
    // the other photo changes, op after op, as background work does
    for rating in 1..=5 {
        catalog.apply(Op::SetRating { id: other, rating }).unwrap();
        r.follow_catalog(&catalog, &files);
        assert!(!r.request_needed(slot, 77, 10), "another photo's change (rating {rating}) retries nothing");
    }
    // a change to the photo itself is a reason to try again
    catalog.apply(Op::SetRating { id: p.id, rating: 2 }).unwrap();
    r.follow_catalog(&catalog, &files);
    assert!(r.request_needed(slot, 77, 10), "its own photo changed");
    // so is a change the catalog can't attribute to photos
    r.failed.insert(slot, (77, "unrecognised image format".into(), r.catalog_rev, p.id));
    assert!(!r.request_needed(slot, 77, 10));
    let album = catalog.alloc_album_id();
    catalog.apply(Op::AddAlbum { album: lightcraft_catalog::Album::new(album, "Trip") }).unwrap();
    r.follow_catalog(&catalog, &files);
    assert!(r.request_needed(slot, 77, 10), "which photos changed is unknown");
    // a file known to be missing (its drive unplugged) is tried at every change, as before, so
    // the photo comes back with its drive
    r.failed.insert(slot, (77, "file not found".into(), r.catalog_rev, p.id));
    here.store(false, Ordering::SeqCst);
    catalog.apply(Op::SetRating { id: other, rating: 1 }).unwrap();
    r.follow_catalog(&catalog, &files);
    assert!(r.request_needed(slot, 77, 10), "a missing file is tried again");
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
/// An unedited raw: its embedded camera preview is its final thumbnail (the grid's quick path).
fn raw_quick_fixture() -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
    let mut s = Session::new();
    s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
    let mut raw = (*photo()).clone();
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    fixture_with(s, raw)
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

/// Issue #2682: the grid sent each video to the image decoder for a thumbnail, which failed with
/// "unrecognised image format", and asked again on every pass. A video is known by its kind, by
/// the format a Lightroom catalog gives it, or by its extension; none asks for a render, so none
/// fails and its cell keeps the plain placeholder.
#[test]
fn a_video_asks_the_decoders_for_no_thumbnail() {
    use lightcraft_catalog::MediaKind;
    let clip = |name: &str, format: &str, kind: MediaKind| {
        let mut p = Photo::new(PhotoId(1), Source::File { path: name.into() }, name, format, 1920, 1080, "2022-11-25");
        p.kind = kind;
        p
    };
    for video in [
        clip("VID_20221125_083409_527.mp4", "MP4", MediaKind::Video),
        clip("clip.MOV", "VIDEO", MediaKind::Image),
        clip("20221223_100006.mp4", "", MediaKind::Image),
    ] {
        let name = video.file_name.clone();
        let (mut app, work, ctx) = fixture_with(Session::new(), video);
        for _ in 0..30 {
            request(&mut app);
            app.renderer.poll(&ctx, &mut app.session);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 0, "{name}");
        assert!(work.borrow().jobs.is_empty() && app.renderer.queue.is_empty(), "{name}: nothing is decoded");
        assert_eq!(app.renderer.failure(Slot::Thumb(PhotoId(1))), None, "{name}: nothing failed, so the cell has no \"!\"");
    }
    // a photo is still asked for
    let (mut app, _, _) = fixture();
    request(&mut app);
    assert_eq!(app.renderer.thumb_jobs_built, 1);
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

/// An app built by this crate's tests renders on two workers, not the machine's six: several
/// tests run at once, and each of them used to start six (issue #3067).
#[test]
fn a_test_app_renders_on_two_workers() {
    let (mut app, _, ctx) = fixture_with(Session::new(), (*photo()).clone());
    app.renderer.offload = None; // the real job pool
    let slot = Slot::Thumb(PhotoId(1));
    let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
    app.renderer.request(slot, job, 30);
    assert_eq!(app.renderer.live_workers(), TEST_WORKERS, "the pool starts all its workers with its first job");
    let t0 = std::time::Instant::now();
    while app.renderer.is_pending(slot) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the render never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.renderer.poll(&ctx, &mut app.session);
    }
    assert!(app.renderer.textures.contains_key(&slot), "{:?}", app.renderer.failure(slot));
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
    // A decoded embedded preview can differ from the catalog's square metadata. Its
    // superseded colour-only draft must retain the identity that enables aspect correction.
    let mut first_result = first.run();
    first_result.rendered.as_mut().unwrap().image = lightcraft_raster::Rgba8::new(8, 4);
    finish(&mut app, &work, &ctx, Slot::Main, first_result);
    assert_eq!(app.renderer.textures[&Slot::Main].size, [8, 4]);
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[0]));
    assert!(app.renderer.is_pending(Slot::Main));
    assert_eq!(
        app.renderer.textures[&Slot::Main].look,
        Some((geometry_look(&lightcraft_develop::DevelopSettings::default()), true)),
        "colour-only superseded pixels retain their geometry"
    );
    let current_look = geometry_look(&last.settings);
    finish(&mut app, &work, &ctx, Slot::Main, last.run());
    assert_eq!(app.renderer.textures[&Slot::Main].look, Some((current_look, true)));
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
    // a draft older than the pixels shown never replaces them
    finish(&mut app, &work, &ctx, Slot::Main, second.run());
    assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
    assert!(!app.renderer.is_pending(Slot::Main));
    assert!(app.renderer.offload_looks.is_empty(), "completed offload identities are released");
}

#[test]
fn superseded_crop_keeps_its_own_geometry_instead_of_the_latest_requests() {
    let (mut app, work, ctx) = fixture();
    let old = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().draft();
    let old_look = geometry_look(&old.settings);
    app.renderer.request(Slot::Main, old, 100);
    let (_, old) = take_job(&work);
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.crop.geometry.rect = lightcraft_geom::Rect::new(0.0, 0.0, 0.5, 1.0);
    let new_look = geometry_look(&settings);
    app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings), label: "crop".into(), edited: None }).unwrap();
    let current = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().draft();
    app.renderer.request(Slot::Main, current, 100);
    let (_, current) = take_job(&work);
    finish(&mut app, &work, &ctx, Slot::Main, old.run());
    assert_eq!(app.renderer.textures[&Slot::Main].look, Some((old_look, true)));
    assert_ne!(old_look, new_look);
    finish(&mut app, &work, &ctx, Slot::Main, current.run());
    assert_eq!(app.renderer.textures[&Slot::Main].look, Some((new_look, true)));
}

#[test]
fn hidden_grading_keeps_the_completed_textures_geometry_identity() {
    let (mut app, work, ctx) = fixture();
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.grading.shadows.sat = 40.0;
    settings.crop.geometry.rect = lightcraft_geom::Rect::new(0.1, 0.2, 0.8, 0.7);
    app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings.clone()), label: "grade".into(), edited: None }).unwrap();
    let expected = geometry_look(&settings);
    let job = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().with_hidden_grading([true, false, false]);
    assert_ne!(job.settings.hash64(), settings.hash64(), "the view really hides a nonzero wheel");
    app.renderer.request(Slot::Main, job, 100);
    let (slot, job) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, job.run());
    assert_eq!(app.renderer.textures[&Slot::Main].look, Some((expected, true)));
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

/// First request: other buckets must not replace the pending job. Returns the pending key.
fn thumb_buckets_pending_request(app: &mut crate::LightcraftApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, ctx: &egui::Context) -> u64 {
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    // A print page's cells show the same photo at different sizes. Before any texture arrives,
    // each frame's other bucket used to replace (and restart) the pending job.
    crate::panels::grid::request_thumb(app, id, 256, 10);
    let built = app.renderer.thumb_jobs_built;
    assert_eq!(built, 1);
    let (submitted, pending) = take_job(work);
    assert_eq!(submitted, slot);
    for _ in 0..12 {
        crate::panels::grid::request_thumb(app, id, 512, 10);
        crate::panels::grid::request_thumb(app, id, 384, 8);
        crate::panels::grid::request_thumb(app, id, 256, 10);
    }
    assert_eq!(app.renderer.thumb_jobs_built, built, "another bucket must not replace the pending thumbnail");
    assert!(work.borrow().jobs.is_empty(), "the pending render is not restarted");
    assert_eq!(app.renderer.pending[&slot].0, pending.key);
    let pending_key = pending.key;
    finish(app, work, ctx, slot, pending.run());
    assert_eq!(app.renderer.textures[&slot].key, pending_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 256, "the completed request's bucket is remembered");
    assert!(!app.renderer.is_pending(slot));
    pending_key
}

/// A larger bucket still upgrades; smaller cells wait behind it. Returns the larger key.
fn thumb_buckets_larger_upgrades(
    app: &mut crate::LightcraftApp,
    work: &std::rc::Rc<std::cell::RefCell<Work>>,
    ctx: &egui::Context,
    pending_key: u64,
) -> u64 {
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    // With a completed 256 thumbnail, the larger cell asks for its bucket once; the smaller
    // cells then wait behind it instead of replacing it back every frame.
    let built = app.renderer.thumb_jobs_built;
    crate::panels::grid::request_thumb(app, id, 512, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 1, "a larger bucket still upgrades");
    let (submitted, larger) = take_job(work);
    assert_eq!(submitted, slot);
    assert_ne!(larger.key, pending_key);
    let larger_key = app.renderer.pending[&slot].0;
    assert_eq!(larger_key, larger.key);
    for _ in 0..12 {
        crate::panels::grid::request_thumb(app, id, 512, 10);
        crate::panels::grid::request_thumb(app, id, 384, 10);
        crate::panels::grid::request_thumb(app, id, 256, 10);
    }
    assert_eq!(app.renderer.thumb_jobs_built, built + 1, "smaller cells keep the larger render in flight");
    assert!(work.borrow().jobs.is_empty(), "the larger render is not restarted");
    assert_eq!(app.renderer.pending[&slot].0, larger_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 512, "the in-flight bucket is still the larger one");
    finish(app, work, ctx, slot, larger.run());
    assert_eq!(app.renderer.textures[&slot].key, larger_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 512);
    assert!(!app.renderer.is_pending(slot));
    larger_key
}

fn thumb_buckets_largest_serves_smaller(app: &mut crate::LightcraftApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, larger_key: u64) {
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    // Once the largest visible bucket is complete, every smaller cell reuses those pixels.
    // The 8x8 fixture texture cannot show its requested edge, so the remembered bucket, key
    // and photo identity are what prove no render was built. Repeated frames at all sizes
    // (the old bug alternated 256/512 forever) must settle with nothing further requested.
    let built = app.renderer.thumb_jobs_built;
    for _ in 0..24 {
        crate::panels::grid::request_thumb(app, id, 256, 10);
        crate::panels::grid::request_thumb(app, id, 384, 10);
        crate::panels::grid::request_thumb(app, id, 512, 10);
        crate::panels::grid::request_thumb(app, id, 384, 8);
    }
    assert_eq!(app.renderer.thumb_jobs_built, built, "completed larger thumbnails serve smaller cells");
    assert!(work.borrow().jobs.is_empty());
    assert!(!app.renderer.is_pending(slot));
    assert_eq!(app.renderer.textures[&slot].key, larger_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 512);
    assert_eq!(app.renderer.thumb_inputs[&id].2, larger_key);
}

fn thumb_buckets_edit_requests_again(
    app: &mut crate::LightcraftApp,
    work: &std::rc::Rc<std::cell::RefCell<Work>>,
    ctx: &egui::Context,
    larger_key: u64,
) {
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    // The renderer remembers the photo through a `Weak<Photo>` only. That is enough for
    // `Catalog`'s `Arc::make_mut` to copy the photo on edit (std: an existing weak pointer
    // forces a new allocation even with a single strong reference), so the identity below is
    // invalidated without this test holding an extra `Arc<Photo>`.
    let remembered = app.renderer.thumb_inputs[&id].0.clone();
    let remembered_ptr = remembered.as_ptr();
    assert!(remembered.upgrade().is_some(), "the remembered Weak points at the live catalog photo");
    assert_eq!(Arc::strong_count(app.session.catalog.photo(id).unwrap()), 1, "the catalog is the only strong owner; the renderer keeps just a Weak");

    // An edit is a different photo/content key: the completed larger picture must not be
    // reused for a smaller cell, and the replacement is requested as before.
    let mut edit = lightcraft_develop::DevelopSettings::default();
    edit.light.exposure = 3.0;
    app.session.catalog.apply(Op::SetDevelop { id, settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
    let current = app.session.catalog.photo(id).unwrap();
    assert_ne!(Arc::as_ptr(current), remembered_ptr, "make_mut moved the edited photo to a new allocation");
    assert_eq!(remembered.strong_count(), 0, "the renderer's Weak now points at the old, unowned allocation");
    assert_eq!(Arc::strong_count(current), 1, "the edit left one strong owner; no extra Arc was needed");
    let built = app.renderer.thumb_jobs_built;
    crate::panels::grid::request_thumb(app, id, 256, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 1, "an edited photo is requested again");
    let (submitted, edited) = take_job(work);
    assert_eq!(submitted, slot);
    assert_ne!(edited.key, larger_key);
    let edited_key = edited.key;
    finish(app, work, ctx, slot, edited.run());
    assert_eq!(app.renderer.textures[&slot].key, edited_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 256);
    assert!(!app.renderer.is_pending(slot));
}

#[test]
fn cells_at_different_thumbnail_buckets_keep_one_pending_request() {
    let (mut app, work, ctx) = fixture();
    let pending_key = thumb_buckets_pending_request(&mut app, &work, &ctx);
    let larger_key = thumb_buckets_larger_upgrades(&mut app, &work, &ctx, pending_key);
    thumb_buckets_largest_serves_smaller(&mut app, &work, larger_key);
    thumb_buckets_edit_requests_again(&mut app, &work, &ctx, larger_key);
}

/// The remembered larger bucket is not a picture by itself: once its texture is evicted, a
/// smaller cell must render again rather than trust the stale record.
#[test]
fn an_evicted_larger_thumbnail_is_rebuilt_for_a_smaller_cell() {
    let (mut app, work, ctx) = fixture();
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    let (submitted, larger) = take_job(&work);
    assert_eq!(submitted, slot);
    let larger_key = larger.key;
    finish(&mut app, &work, &ctx, slot, larger.run());
    assert_eq!(app.renderer.textures[&slot].key, larger_key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 512);
    assert!(!app.renderer.is_pending(slot));

    // An LRU trim drops the pixels; the remembered bucket/key must not stand in for them.
    app.renderer.textures.remove(&slot);
    let built = app.renderer.thumb_jobs_built;
    crate::panels::grid::request_thumb(&mut app, id, 256, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 1, "an evicted larger thumbnail is rebuilt");
    let (submitted, rebuilt) = take_job(&work);
    assert_eq!(submitted, slot);
    assert_ne!(rebuilt.key, larger_key);
    assert_eq!(app.renderer.pending[&slot].0, rebuilt.key);
    assert_eq!(app.renderer.thumb_inputs[&id].1, 256);
    let rebuilt_key = rebuilt.key;
    finish(&mut app, &work, &ctx, slot, rebuilt.run());
    assert_eq!(app.renderer.textures[&slot].key, rebuilt_key);
}

#[test]
fn a_coalesced_thumbnail_promotes_once_for_a_higher_priority_caller() {
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
    let mut app = crate::LightcraftApp::new(s, crate::Services::default());
    app.renderer.pool = JobPool::new(0); // deterministic: nothing starts
    let slot = Slot::Thumb(PhotoId(1));
    crate::panels::grid::request_thumb(&mut app, PhotoId(1), 256, 5);
    assert_eq!(app.renderer.pending[&slot].1, 5);
    let first = app.renderer.pending[&slot].0;
    // An on-screen caller at another bucket takes over the off-screen request once...
    crate::panels::grid::request_thumb(&mut app, PhotoId(1), 512, 10);
    let promoted = app.renderer.pending[&slot];
    assert_eq!(promoted.1, 10);
    assert_ne!(promoted.0, first, "the promoted bucket is the one rendered");
    assert_eq!(app.renderer.thumb_jobs_built, 2, "one promotion");
    // ...and the caller it replaced then waits behind that priority instead of replacing it
    // back. Equal priorities also keep the request in flight (the print cells' case).
    for _ in 0..6 {
        crate::panels::grid::request_thumb(&mut app, PhotoId(1), 256, 5);
        crate::panels::grid::request_thumb(&mut app, PhotoId(1), 384, 8);
        crate::panels::grid::request_thumb(&mut app, PhotoId(1), 512, 10);
    }
    assert_eq!(app.renderer.pending[&slot], promoted);
    assert_eq!(app.renderer.pool.queued(), 1);
    assert_eq!(app.renderer.thumb_jobs_built, 2, "a promoted request is not replaced in turn");
}

#[test]
fn a_stale_texture_does_not_disable_cross_bucket_coalescing() {
    let (mut app, work, ctx) = fixture();
    let id = PhotoId(1);
    let slot = Slot::Thumb(id);
    crate::panels::grid::request_thumb(&mut app, id, 256, 10);
    let (_, first) = take_job(&work);
    finish(&mut app, &work, &ctx, slot, first.run());
    let old_key = app.renderer.textures[&slot].key;

    // The edit leaves the old texture displayed until the replacement renders.
    let mut edit = lightcraft_develop::DevelopSettings::default();
    edit.light.exposure = 2.0;
    app.session.catalog.apply(Op::SetDevelop { id, settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
    let built = app.renderer.thumb_jobs_built;
    crate::panels::grid::request_thumb(&mut app, id, 384, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 1);
    let (_, edited) = take_job(&work);
    let new_key = app.renderer.pending[&slot].0;
    assert_ne!(new_key, old_key);
    assert_eq!(app.renderer.textures[&slot].key, old_key, "the old picture stays until the replacement arrives");

    // The stale texture is not the recorded request's picture: other buckets must still keep
    // the replacement in flight rather than restarting it every frame.
    for _ in 0..12 {
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
        crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    }
    assert_eq!(app.renderer.thumb_jobs_built, built + 1, "a stale texture must not disable coalescing");
    assert_eq!(app.renderer.pending[&slot].0, new_key);
    assert!(work.borrow().jobs.is_empty());

    // Once the recorded key has arrived, smaller cells reuse it; a larger cell upgrades.
    finish(&mut app, &work, &ctx, slot, edited.run());
    assert_eq!(app.renderer.textures[&slot].key, new_key);
    crate::panels::grid::request_thumb(&mut app, id, 256, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 1);
    assert!(!app.renderer.is_pending(slot));
    crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    assert_eq!(app.renderer.thumb_jobs_built, built + 2);
    assert_ne!(app.renderer.pending[&slot].0, new_key, "the larger bucket is requested once");
}

#[test]
fn raw_quick_preview_coalesces_across_buckets_without_priority_ping_pong() {
    let mut s = Session::new();
    s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
    let mut raw = (*photo()).clone();
    raw.kind = lightcraft_catalog::MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    let (mut app, work, ctx) = fixture_with(s, raw);
    app.renderer.pool = JobPool::new(0); // deterministic quick renders via run_inline
    let id = PhotoId(1);
    let quick = Slot::ThumbQuick(id);
    let full = Slot::Thumb(id);

    crate::panels::grid::request_thumb(&mut app, id, 384, 5);
    assert_eq!(app.renderer.pending[&quick].1, 6);
    let first = app.renderer.pending[&quick].0;
    assert!(work.borrow().jobs.is_empty(), "the stand-in renders before the real thumbnail");

    // The on-screen caller at another bucket promotes the queued stand-in once.
    crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    let promoted = app.renderer.pending[&quick];
    assert_eq!(promoted.1, 11);
    assert_ne!(promoted.0, first);
    assert_eq!(app.renderer.thumb_jobs_built, 2);

    // The replaced caller waits for it; neither bucket rebuilds the queued stand-in.
    for _ in 0..6 {
        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    }
    assert_eq!(app.renderer.pending[&quick], promoted);
    assert_eq!(app.renderer.pool.queued(), 1);
    assert_eq!(app.renderer.thumb_jobs_built, 2);

    assert_eq!(app.renderer.pool.run_inline(1), 1);
    app.renderer.poll(&ctx, &mut app.session);
    assert_eq!(app.renderer.textures[&quick].key, promoted.0);
    assert!(!app.renderer.is_pending(quick));

    // An unedited raw's embedded preview is final. Repeating its bucket must not queue
    // a full sensor render behind it, and a smaller cell reuses a preview that arrived
    // at a larger bucket instead of asking for another one every frame.
    let built = app.renderer.thumb_jobs_built;
    crate::panels::grid::request_thumb(&mut app, id, 512, 10);
    assert!(!app.renderer.is_pending(full));
    assert!(work.borrow().jobs.is_empty());
    assert_eq!(app.renderer.thumb_jobs_built, built);
    for _ in 0..6 {
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
    }
    assert!(!app.renderer.is_pending(full));
    assert!(!app.renderer.is_pending(quick));
    assert!(work.borrow().jobs.is_empty(), "the embedded preview is not rebuilt for a smaller cell");
    assert_eq!(app.renderer.thumb_jobs_built, built);
    assert_eq!(app.renderer.textures[&quick].key, promoted.0);
    assert_eq!(app.renderer.embedded_thumbs[&id].1, 512, "the embedded preview's bucket is remembered");
}

/// The embedded preview is final only at the bucket whose pixels actually arrived. A larger
/// preview that was evicted, or whose job failed at that key, must not make a smaller cell
/// believe it is covered: that cell asks for its own quick render.
#[test]
fn missing_or_failed_larger_embedded_preview_does_not_cover_a_smaller_cell() {
    for failed_larger in [false, true] {
        let (mut app, _work, ctx) = raw_quick_fixture();
        app.renderer.pool = JobPool::new(0); // deterministic quick renders via run_inline
        let id = PhotoId(1);
        let quick = Slot::ThumbQuick(id);
        crate::panels::grid::request_thumb(&mut app, id, 512, 10);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.textures.contains_key(&quick));
        assert_eq!(app.renderer.embedded_thumbs[&id].1, 512);
        let key = app.renderer.embedded_thumbs[&id].2;
        assert_eq!(app.renderer.textures[&quick].key, key);

        // The larger picture is gone: evicted, or its job failed at that key.
        app.renderer.textures.remove(&quick);
        if failed_larger {
            app.renderer.failed.insert(quick, (key, "no embedded preview".into(), app.renderer.catalog_rev, id));
        }
        let built = app.renderer.thumb_jobs_built;
        crate::panels::grid::request_thumb(&mut app, id, 256, 10);
        assert_eq!(app.renderer.thumb_jobs_built, built + 1, "a smaller cell must request its own quick render");
        let smaller_key = app.renderer.pending[&quick].0;
        assert_ne!(smaller_key, key);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.textures[&quick].key, smaller_key);
    }
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
