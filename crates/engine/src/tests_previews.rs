//! True 1:1 (P1.6): a window of the loupe at 1:1 has the pixels of the full-size export, and the
//! preview store's 1:1 previews, settings and discards.

use lcc_pipeline::{OutputDepth, OutputSpace, PixelWindow};
use serde_json::json;

use crate::Session;
use crate::cmd::previews::{DiscardAfter, discard_full};

/// The largest per-channel difference between the `visible` pixels of `part` (a render of
/// window `win`) and the same pixels of `full`.
fn max_diff(full: &lcc_raster::Rgba8, part: &lcc_raster::Rgba8, win: PixelWindow, visible: PixelWindow) -> u8 {
    let mut worst = 0u8;
    for y in visible.y..visible.y + visible.h {
        for x in visible.x..visible.x + visible.w {
            let a = part.data[(y - win.y) * part.width + (x - win.x)];
            let b = full.data[y * full.width + x];
            for c in 0..3 {
                worst = worst.max(a[c].abs_diff(b[c]));
            }
        }
    }
    worst
}

/// Render photo `id` at full size for export and windows of its 1:1 view, and compare them.
fn check_one_to_one(s: &mut Session, id: lcc_catalog::PhotoId) {
    let p = s.catalog.photo(id).unwrap().clone();
    let export = s.render_export(id, p.width as usize, p.height as usize, OutputSpace::Srgb, OutputDepth::U8).unwrap().image;
    let (w, h) = (export.width, export.height);
    assert!(w.max(h) >= (p.width.max(p.height) as usize) - 1, "export is full size: {w}×{h} of {}×{}", p.width, p.height);
    // the 1:1 frame is the export's frame: the loupe asks for windows of (w, h), with the margin
    // of context it renders around what is on screen (as `region::margin_for` in the UI)
    let margin = ((w.max(h) as f64 * 0.045).min(768.0) as usize).div_ceil(256).max(1) * 256;
    for (fx, fy) in [(0.0, 0.0), (0.37, 0.52), (0.8, 0.7)] {
        let x = ((w as f64 * fx) as usize).min(w.saturating_sub(512));
        let y = ((h as f64 * fy) as usize).min(h.saturating_sub(384));
        let visible = PixelWindow { x, y, w: 512.min(w), h: 384.min(h) };
        let (wx, wy) = (x.saturating_sub(margin), y.saturating_sub(margin));
        let win = PixelWindow { x: wx, y: wy, w: (x + visible.w + margin).min(w) - wx, h: (y + visible.h + margin).min(h) - wy };
        let job = s.region_job(id, w, h, win, true).expect("a window of the 1:1 view");
        let part = job.run().rendered.unwrap().image;
        assert_eq!((part.width, part.height), (win.w, win.h));
        let d = max_diff(&export, &part, win, visible);
        assert!(d <= 2, "1:1 window {win:?} differs from the export by {d} levels");
    }
    // the loupe's tile grid (`region::tiles_for` in the UI): each 1024 px tile is rendered with its
    // margin and cropped back; across a tile boundary — the seam between two renders — the pixels
    // on both sides still equal the export's
    let tile = 1024usize;
    if w > tile && h > tile {
        let (row_y, seam_x) = ((h / tile).saturating_sub(1).min(1) * tile, tile);
        for col in [0usize, 1] {
            let t = PixelWindow { x: col * tile, y: row_y, w: tile.min(w - col * tile), h: tile.min(h - row_y) };
            let (wx, wy) = (t.x.saturating_sub(margin), t.y.saturating_sub(margin));
            let win = PixelWindow { x: wx, y: wy, w: (t.x + t.w + margin).min(w) - wx, h: (t.y + t.h + margin).min(h) - wy };
            let part = s.region_job(id, w, h, win, true).expect("a tile of the 1:1 view").run().rendered.unwrap().image;
            // the 64 px strip of this tile that touches the seam
            let strip = if col == 0 { PixelWindow { x: seam_x - 64, w: 64, ..t } } else { PixelWindow { x: seam_x, w: 64.min(t.w), ..t } };
            let d = max_diff(&export, &part, win, strip);
            assert!(d <= 2, "tile {col} at the seam x = {seam_x} differs from the export by {d} levels");
        }
    }
}

/// A session with `n` photos of `w`×`h` whose sources at `level` are already decoded: textured
/// pictures made in milliseconds (a full-size demo scene takes ten seconds to generate).
fn decoded_photos(n: u64, w: usize, h: usize, level: crate::media::SourceLevel) -> (Session, Vec<lcc_catalog::PhotoId>) {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let mut s = Session::new();
    let ids: Vec<PhotoId> = (1..=n).map(PhotoId).collect();
    for &id in &ids {
        let name = format!("{}.jpg", id.0);
        let p = Photo::new(id, Source::File { path: format!("/nowhere/{name}") }, &name, "JPEG", w as u32, h as u32, "");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let k = id.0 as usize;
        let px = |x: usize, y: usize| {
            let t = ((x * 7 + y * 13 + k * 5) % 97) as f32 / 97.0;
            [0.1 + 0.5 * x as f32 / w as f32 + 0.1 * t, 0.2 + 0.4 * y as f32 / h as f32, 0.15 + 0.3 * t]
        };
        s.media.insert(id, level, std::sync::Arc::new(lcc_raster::Rgb32f::from_fn(w, h, px)));
    }
    (s, ids)
}

// Given a camera-sized photo with tone, local-contrast and sharpening edits, every 1:1 window
// equals the full-resolution export
#[test]
fn one_to_one_pixels_equal_full_resolution_export_pixels() {
    let (mut s, ids) = decoded_photos(1, 3000, 2000, crate::media::SourceLevel::Full);
    let mut d = (*s.develop_of(ids[0]).unwrap()).clone();
    d.light.exposure = 0.4;
    d.effects.clarity = 20.0;
    d.detail.sharpen_amount = 60.0;
    s.set_develop(ids[0], d, "Edits").unwrap();
    check_one_to_one(&mut s, ids[0]);
}

// Given corpus raws (`cargo xtask corpus --download`), the same holds for real files; skipped
// cleanly without them
#[test]
fn one_to_one_pixels_equal_export_pixels_on_corpus_raws() {
    let dir = std::env::var_os("LCC_CORPUS_RAW")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw"));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skipped: no raw corpus at {}", dir.display());
        return;
    };
    // (LCC_CORPUS_RAW_COUNT files, sorted by name; default 3: each is a full-size render)
    let count = std::env::var("LCC_CORPUS_RAW_COUNT").ok().and_then(|n| n.parse().ok()).unwrap_or(3);
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    let step = (files.len() / count.max(1)).max(1);
    let files: Vec<_> = files.into_iter().step_by(step).take(count).collect();
    if files.is_empty() {
        eprintln!("skipped: empty raw corpus");
        return;
    }
    // (the filesystem loader: without it no file decodes)
    let mut s = Session::new().with_fs();
    let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().to_string()).collect();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    assert_eq!(ids.len(), files.len(), "every corpus raw imported");
    for id in &ids {
        check_one_to_one(&mut s, *id);
    }
    eprintln!("1:1 equals export on {} corpus raws", ids.len());
}

// Given 1:1 previews built, they are kept apart from the standard ones and discarded on their own
#[test]
fn one_to_one_previews_are_discarded_on_their_own() {
    let _keys = crate::sensor_maps::keep_generation();
    let (mut s, ids) = decoded_photos(1, 3000, 2000, crate::media::SourceLevel::Full);
    let id = ids[0];
    let p = s.catalog.photo(id).unwrap().clone();
    s.execute("library.buildPreviews", &json!({"size": "full", "ids": [id.0], "wait": true})).unwrap();
    let full = Session::full_view_key(&p);
    // (the view preview is written once the render hands it over)
    assert!(s.media.rendered.contains(full), "1:1 preview built");
    let img = s.media.rendered.get(full).unwrap();
    assert_eq!(img.width.max(img.height), p.width.max(p.height) as usize, "at the photo's own size");
    // a month's policy keeps a fresh one
    assert_eq!(discard_full(&s, &[id], DiscardAfter::Month.max_age()).removed, 0);
    let r = s.execute("library.discardPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["removed"], 1);
    assert!(!s.media.rendered.contains(full));
    let r = s.execute("library.discardPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["removed"], 0, "nothing left to discard");
}

// Given the preview settings, they are validated, applied and reported
#[test]
fn preview_settings_are_validated_and_applied() {
    let mut s = Session::with_demo();
    let r = s.execute("library.previewSettings", &json!({})).unwrap();
    assert_eq!(r, json!({"standardEdge": 2048, "discardFull": "month", "atImport": "minimal"}));
    let r = s.execute("library.previewSettings", &json!({"standardEdge": 1440, "discardFull": "never", "atImport": "full"})).unwrap();
    assert_eq!(r, json!({"standardEdge": 1440, "discardFull": "never", "atImport": "full"}));
    assert!(s.execute("library.previewSettings", &json!({"standardEdge": 10})).is_err());
    assert!(s.execute("library.previewSettings", &json!({"discardFull": "fortnight"})).is_err());
    assert!(s.execute("library.previewSettings", &json!({"atImport": 3})).is_err());
    assert!(s.execute("library.discardPreviews", &json!({"size": "standard"})).is_err());
    assert_eq!(s.preview_prefs.standard_edge(), 1440, "a refused change leaves the settings");
}

/// An absurd export size is refused, not a multiply overflow (it panicked before the fix).
#[test]
fn huge_export_size_is_an_error() {
    let mut s = Session::with_demo();
    let id = s.catalog.photos().next().unwrap().id;
    let huge = usize::MAX >> 8;
    assert!(s.render_export(id, huge, huge, OutputSpace::Srgb, OutputDepth::U8).is_err());
    assert!(s.render_export(id, 70_000, 10, OutputSpace::Srgb, OutputDepth::U8).is_err());
    assert!(s.render_now(id, huge, huge).is_err());
    assert!(s.render_now(id, 10, 70_000).is_err());
}

// Given a photo's preview jobs, the grid thumbnail is made from the source the view decoded, not
// by reading the file again (here: a thumbnail job pointing at a file that is not there)
#[test]
fn preview_build_makes_the_thumbnail_from_the_views_source() {
    use crate::cmd::previews::{jobs_for, run_photo};
    use crate::media::SourceRef;
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let mut jobs = jobs_for(&mut s, id, Some(512));
    assert_eq!(jobs.len(), 2, "view and thumbnail");
    assert!(jobs[0].view_cache.is_some(), "the view renders first");
    jobs[1].source = SourceRef::File {
        path: "/nonexistent/photo.arw".into(),
        max_edge: 512,
        loader: None,
        fallback: None,
        denoise: None,
        options: Default::default(),
        reduced: None,
    };
    assert!(run_photo(jobs), "the thumbnail rendered from the view's decoded source");
}

// Given many photos, a standard preview build renders them side by side and counts every one
#[test]
fn preview_build_of_many_photos_counts_each_once() {
    let (mut s, ids) = decoded_photos(6, 600, 400, crate::media::SourceLevel::Thumb);
    // (more photos than the cache keeps decoded: the rest come from this loader, no file needed)
    s.media.reduced_loader = None;
    s.media.preview_loader = None;
    s.media.file_loader = Some(std::sync::Arc::new(|_, _, _| {
        let mut image = lcc_raster::Rgb32f::new(600, 400);
        image.data.fill([0.3, 0.2, 0.1]);
        Ok((image, Default::default()))
    }));
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    let r = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids, "wait": true})).unwrap();
    assert_eq!(r["done"], json!(ids.len()), "{r}");
    assert_eq!(r["total"], json!(ids.len()));
    assert_eq!(r["failed"], json!(0));
}

// ---- a build asked for while another runs waits for its turn (the preview queue)

/// Wait (at most 30 s) until build `b` is over.
fn wait_done(b: &crate::cmd::previews::PreviewBuild) -> bool {
    for _ in 0..1500 {
        if !b.running() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// A session of `n` synthetic photos whose every decode waits on `gate` (a failed test never
/// leaves its lanes waiting: the gate gives up after a minute): a build's lanes hold in the
/// loader, so a test can ask for a second build while the first truly runs.
fn gated_photos(n: u64, gate: std::sync::Arc<std::sync::atomic::AtomicBool>) -> (Session, Vec<lcc_catalog::PhotoId>) {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let mut s = Session::new();
    let ids: Vec<PhotoId> = (1..=n).map(PhotoId).collect();
    for &id in &ids {
        let name = format!("{}.jpg", id.0);
        let p = Photo::new(id, Source::File { path: format!("/nowhere/{name}") }, &name, "JPEG", 600, 400, "");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.media.file_loader = Some(std::sync::Arc::new(move |_, _, _| {
        let t0 = std::time::Instant::now();
        while !gate.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut image = lcc_raster::Rgb32f::new(600, 400);
        image.data.fill([0.3, 0.2, 0.1]);
        Ok((image, Default::default()))
    }));
    (s, ids)
}

// Given a build running, a second one asked for waits for its turn, then runs: every photo is
// built exactly once by each (the user queues a 1:1 build behind the standard one, as several
// processes at once)
#[test]
fn a_build_asked_for_while_one_runs_waits_for_its_turn() {
    let _keys = crate::sensor_maps::keep_generation();
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut s, ids) = gated_photos(6, gate.clone());
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    let r = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    // the same request again while one runs: not an error, not a second build — it waits
    let q = s.execute("library.buildPreviews", &json!({"size": "full", "ids": ids})).unwrap();
    assert_eq!((q["queued"].as_bool(), q["position"].as_u64()), (Some(true), Some(1)), "{q}");
    assert_eq!(s.preview_queue.len(), 1);
    // the first build ends, and the waiting one runs and completes, every photo once
    gate.store(true, std::sync::atomic::Ordering::Release);
    let first = s.preview_build.clone().unwrap();
    assert!(wait_done(&first), "the first build ended");
    let started = crate::cmd::previews::drain_queue(&mut s).expect("the waiting build starts");
    assert!(started.is_ok(), "{started:?}");
    let second = s.preview_build.clone().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &second), "a fresh build");
    assert!(wait_done(&second), "the second build ended");
    let r = second.json();
    assert_eq!(r["done"], json!(ids.len()), "{r}");
    assert_eq!(r["failed"], json!(0));
    assert_eq!(r["skipped"], json!(0), "1:1 previews are their own build's work");
    assert!(crate::cmd::previews::drain_queue(&mut s).is_none(), "nothing more waits");
}

// Given a request already waiting, the same one is not queued twice; a different one queues
// behind it; and past a full queue the ask is refused
#[test]
fn identical_waiting_requests_are_not_queued_twice_and_the_queue_has_a_cap() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut s, ids) = gated_photos(4, gate.clone());
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids})).unwrap();
    let same = json!({"size": "full", "ids": ids});
    let q = s.execute("library.buildPreviews", &same).unwrap();
    assert_eq!(q["position"], 1, "{q}");
    let q = s.execute("library.buildPreviews", &same).unwrap();
    assert_eq!(q["position"], 1, "still one waiting: {q}");
    let q = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 256, "ids": ids})).unwrap();
    assert_eq!(q["position"], 2, "a different one queues behind it: {q}");
    assert_eq!(s.preview_queue.len(), 2);
    for i in 0..14 {
        let q = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 300 + i, "ids": ids})).unwrap();
        assert_eq!(q["position"], 3 + i, "{q}");
    }
    let e = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 4000, "ids": ids})).unwrap_err();
    assert!(e.to_string().contains("waiting"), "{e}");
    // Stop drops them all and cancels the running one
    let r = s.execute("library.cancelPreviews", &json!({})).unwrap();
    assert_eq!(r["queuedDropped"], json!(16), "{r}");
    assert_eq!(r["cancelled"], true, "{r}");
    assert!(s.preview_queue.is_empty());
    gate.store(true, std::sync::atomic::Ordering::Release);
    let b = s.preview_build.clone().unwrap();
    assert!(wait_done(&b));
    assert!(crate::cmd::previews::drain_queue(&mut s).is_none(), "nothing more starts");
}

// Given Stop with builds waiting, the waiting ones are dropped and nothing starts after the
// running one ends
#[test]
fn stop_drops_the_waiting_builds_and_cancels_the_running_one() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut s, ids) = gated_photos(4, gate.clone());
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids})).unwrap();
    s.execute("library.buildPreviews", &json!({"size": "full", "ids": ids})).unwrap();
    s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 256, "ids": ids})).unwrap();
    let running = s.preview_build.clone().unwrap();
    let r = s.execute("library.cancelPreviews", &json!({})).unwrap();
    assert_eq!(r["queuedDropped"], json!(2), "{r}");
    assert!(running.cancel.load(std::sync::atomic::Ordering::Relaxed), "the running one is stopped");
    gate.store(true, std::sync::atomic::Ordering::Release);
    assert!(wait_done(&running));
    assert!(crate::cmd::previews::drain_queue(&mut s).is_none(), "nothing was left to start");
    assert!(std::sync::Arc::ptr_eq(&running, &s.preview_build.clone().unwrap()), "no build took its place");
}

// Given a plain build running, a smart previews request waits for its turn and then runs
#[test]
fn a_smart_previews_request_waits_for_the_running_build() {
    let _keys = crate::sensor_maps::keep_generation();
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (root, mut s, ids) = crate::tests_preview_prep::library("queue-smart", 4);
    let gate2 = gate.clone();
    s.media.file_loader = Some(std::sync::Arc::new(move |_, _, _| {
        let t0 = std::time::Instant::now();
        while !gate2.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut image = lcc_raster::Rgb32f::new(600, 400);
        image.data.fill([0.3, 0.2, 0.1]);
        Ok((image, Default::default()))
    }));
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids})).unwrap();
    let q = s.execute("library.smartPreviews", &json!({"background": true, "ids": ids})).unwrap();
    assert_eq!((q["queued"].as_bool(), q["position"].as_u64()), (Some(true), Some(1)), "{q}");
    gate.store(true, std::sync::atomic::Ordering::Release);
    let first = s.preview_build.clone().unwrap();
    assert!(wait_done(&first), "the plain build ended");
    let started = crate::cmd::previews::drain_queue(&mut s).expect("the smart build starts");
    assert!(started.is_ok(), "{started:?}");
    let smart = s.preview_build.clone().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &smart), "a fresh build");
    assert!(wait_done(&smart), "the smart build ended");
    let r = s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!(r["what"], "smart previews", "{r}");
    assert_eq!(r["done"], json!(ids.len()), "{r}");
    let _ = std::fs::remove_dir_all(root);
}
