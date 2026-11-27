//! Previews are made once and used (notes 47–49): the loupe shows a built or prepared preview
//! instead of rendering the photo again, a build skips photos whose previews are there, built
//! previews are on disk when the build says it is done, the import's queue shows one row, and the
//! background preparation follows the user and gives way to a build that was asked for.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::media::QuickSource;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-preview-prep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lightcraft_raster::Rgba8::from_fn(600, 400, |x, y| [(x % 251) as u8, (y % 241) as u8, seed, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

/// A library on disk with `n` photos (600 × 400 PNGs) imported, in name order.
fn library(tag: &str, n: usize) -> (Session, PathBuf, Vec<PhotoId>) {
    let root = temp_dir(tag);
    let paths: Vec<String> = (0..n)
        .map(|i| {
            let p = root.join("shoot").join(format!("p{i:02}.png"));
            png(&p, (i * 20) as u8);
            p.to_string_lossy().to_string()
        })
        .collect();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let mut ids: Vec<(String, PhotoId)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.id)).collect();
    ids.sort();
    (s, root, ids.into_iter().map(|(_, id)| id).collect())
}

fn ids_json(ids: &[PhotoId]) -> Vec<u64> {
    ids.iter().map(|i| i.0).collect()
}

/// FredApps on #3077: building previews (asked for, and the background preparation while culling)
/// writes only into the library's own cache, never next to the photos: the shoot folder holds the
/// same files afterwards, byte for byte.
#[test]
fn building_previews_writes_nothing_next_to_the_photos() {
    let (mut s, root, ids) = library("prep-beside", 4);
    let shoot = root.join("shoot");
    let listing = |dir: &std::path::Path| {
        let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| (p.file_name().unwrap().to_string_lossy().to_string(), std::fs::read(&p).unwrap()))
            .collect();
        v.sort();
        v
    };
    let before = listing(&shoot);
    assert_eq!(before.len(), 4);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["done"], 4, "{r}");
    for id in &ids {
        s.prep_tick(crate::prep::Mode::Library, Some(*id), false);
    }
    if let Some(b) = s.preview_build.clone() {
        assert!(wait_finished(&b), "the background build stops (a late write would slip past the check)");
    }
    assert_eq!(listing(&shoot), before, "nothing was written next to the photos");
    let _ = std::fs::remove_dir_all(&root);
}

/// Note 47: a built preview at least as big as the loupe's size is what the loupe shows (fitted
/// down, no render); a smaller one is not (the photo renders).
#[test]
fn the_loupe_uses_a_built_preview_that_is_big_enough() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("library.buildPreviews", &json!({"ids": [id.0], "edge": 512, "wait": true})).unwrap();
    let r = s.loupe_job(id, 300, 300, true).unwrap().run();
    assert_eq!(r.quick, Some(QuickSource::Cached), "taken from the built preview");
    let img = r.rendered.unwrap().image;
    assert_eq!(img.width.max(img.height), 300, "fitted to the loupe's size");
    // bigger than what was built: rendered
    let r = s.loupe_job(id, 900, 900, true).unwrap().run();
    assert_eq!(r.quick, None, "a preview too small for the loupe is not used");
    assert!(r.rendered.is_ok());
    // a draft (a slider held) never comes from the cache
    let r = s.loupe_job(id, 300, 300, true).unwrap().draft().run();
    assert_eq!(r.quick, None);
}

/// Shrinking a cached view keeps its colour (an average of the pixels it covers) and its aspect,
/// and refuses to enlarge.
#[test]
fn a_cached_view_is_fitted_down_never_up() {
    let img = lightcraft_raster::Rgba8::from_fn(400, 200, |x, _| if x % 2 == 0 { [200, 100, 0, 255] } else { [100, 50, 0, 255] });
    let f = crate::media::fit_cached(&img, 100, 100).unwrap();
    assert_eq!((f.width, f.height), (100, 50));
    assert!(f.data.iter().all(|p| p[0] == 150 && p[1] == 75), "averaged");
    assert!(crate::media::fit_cached(&img, 800, 800).is_none(), "never enlarged");
    assert_eq!(crate::media::fit_cached(&img, 401, 999).map(|f| f.width), Some(400), "a pixel short is the same size");
    assert!(crate::media::fit_cached(&img, 0, 10).is_none());
}

/// Note 47: the rendered previews in memory follow the memory budget (they were capped at
/// 128 MB, about 84 loupe renders, whatever the budget).
#[test]
fn the_memory_for_rendered_previews_grows_with_the_budget() {
    let share = crate::memory::cache_share(20 << 30);
    assert!(crate::media::rendered_budget(share) >= 2 << 30, "a 20 GB budget keeps gigabytes of previews");
    assert_eq!(crate::media::rendered_budget(256 << 20), 128 << 20, "a small budget keeps the old floor where it can");
    assert!(crate::media::rendered_budget(64 << 20) <= 32 << 20, "and never more than the share allows");
}

/// Note 47: the disk cache's default room grows with the library, so a wedding's previews are not
/// pruned while they are built (the fixed 2 GB held about 2 000 standard previews).
#[test]
fn the_disk_cache_default_grows_with_the_library() {
    assert_eq!(crate::media::default_disk_cache_bytes(10), crate::media::DISK_CACHE_BYTES);
    assert!(crate::media::default_disk_cache_bytes(4000) >= 4000 * (1 << 20));
    let (mut s, root, ids) = library("diskroom", 2);
    assert_eq!(s.cache_bytes(), crate::media::DISK_CACHE_BYTES);
    s.cache_mb = 700;
    assert_eq!(s.cache_bytes(), 700 << 20, "a size the user chose is kept");
    assert_eq!(ids.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// Notes 47 and 49: a build skips photos whose previews are there (built before, or by the loupe),
/// renders those whose preview is too small, and its previews are on disk when it is done (they
/// were left to a writer that skips writes when it falls behind).
#[test]
fn a_build_skips_what_is_there_and_leaves_it_on_disk() {
    let (mut s, root, ids) = library("skip", 4);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!((r["done"].as_u64(), r["skipped"].as_u64(), r["failed"].as_u64()), (Some(4), Some(0), Some(0)), "{r}");
    // on disk now (no waiting for a writer), at the size built
    for id in &ids {
        let p = s.catalog.photo(*id).unwrap().clone();
        let key = s.view_key(&p, true);
        let disk = s.media.rendered.disk().expect("a library has a disk cache");
        let (w, h) = disk.dims(key).expect("the view preview is on disk");
        assert_eq!(w.max(h), 300);
    }
    // again: nothing is rendered
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!((r["done"].as_u64(), r["skipped"].as_u64()), (Some(4), Some(4)), "{r}");
    // forced, or bigger: rendered
    let r = s.execute("library.buildPreviews", &json!({"ids": [ids[0].0], "edge": 300, "wait": true, "force": true})).unwrap();
    assert_eq!(r["skipped"], 0, "{r}");
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 500, "wait": true})).unwrap();
    assert_eq!(r["skipped"], 0, "too small for what is asked: built again {r}");
    // a library opened again finds them on disk (memory is empty)
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 500, "wait": true})).unwrap();
    assert_eq!(r["skipped"], 4, "{r}");
    let r = s.loupe_job(ids[1], 400, 400, true).unwrap().run();
    assert_eq!(r.quick, Some(QuickSource::Cached), "the loupe uses the preview built in the last session");
    let _ = std::fs::remove_dir_all(root);
}

/// A forced build renders the grid thumbnail again too, not the one its cache holds (CodeRabbit on #3577:
/// the thumbnail job read its cache first, so `force` rebuilt only the view).
#[test]
fn a_forced_build_renders_the_thumbnail_again() {
    let (mut s, root, ids) = library("force-thumb", 1);
    let build = |s: &mut Session, force: bool| {
        s.execute("library.buildPreviews", &json!({"ids": [ids[0].0], "edge": 300, "wait": true, "force": force})).unwrap()
    };
    build(&mut s, false);
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    let thumb = jobs.iter().find(|j| j.view_cache.is_none() && j.cache.is_some()).expect("a thumbnail job");
    let ((cache, key), generation) = (thumb.cache.clone().unwrap(), thumb.cache_generation);
    let stand_in = lightcraft_raster::Rgba8 { width: 1, height: 1, data: vec![[1, 2, 3, 255]] };
    assert!(cache.put_at(generation, key, std::sync::Arc::new(stand_in)));
    let r = build(&mut s, false);
    assert_eq!(r["skipped"], 1, "{r}");
    assert_eq!(cache.get_at(generation, key).map(|i| i.width), Some(1), "unforced: kept");
    let r = build(&mut s, true);
    assert_eq!(r["failed"], 0, "{r}");
    assert!(cache.get_at(generation, key).is_some_and(|i| i.width > 1), "forced: rendered again");
    let _ = std::fs::remove_dir_all(root);
}

/// Note 47: standard previews are built at least as big as the loupe on this screen asks for.
#[test]
fn standard_previews_are_as_big_as_the_loupe() {
    let mut s = Session::with_demo();
    assert_eq!(crate::cmd::previews::standard_edge(&s), crate::cmd::previews::STANDARD_EDGE);
    s.loupe_edge = 3000;
    assert_eq!(crate::cmd::previews::standard_edge(&s), 3000);
    s.loupe_edge = 1200;
    assert_eq!(crate::cmd::previews::standard_edge(&s), crate::cmd::previews::STANDARD_EDGE);
}

/// Note 48: a background build goes from where the user is, in the direction they step, and
/// follows them when they jump.
#[test]
fn the_background_build_starts_where_the_user_is() {
    use crate::cmd::previews::PreviewBuild;
    let b = PreviewBuild { order: (10..20).collect(), ..Default::default() };
    let mut queue: Vec<Option<u64>> = (10..20).map(Some).collect();
    b.focus_on(PhotoId(14), false);
    let mut took = Vec::new();
    for _ in 0..3 {
        let i = b.next_index(&queue).unwrap();
        took.push(queue[i].take().unwrap());
    }
    assert_eq!(took, vec![14, 15, 16], "forward from the photo on screen");
    b.focus_on(PhotoId(12), true);
    let i = b.next_index(&queue).unwrap();
    assert_eq!(queue[i], Some(12), "jumped back: from there, backwards");
    queue[i] = None;
    assert_eq!(b.next_index(&queue).map(|i| queue[i]), Some(Some(11)));
    // the end reached: the rest the other way
    b.focus_on(PhotoId(19), false);
    for _ in 0..6 {
        let i = b.next_index(&queue).unwrap();
        queue[i] = None;
    }
    assert!(b.next_index(&queue).is_none(), "all taken");
}

/// Note 49: the background preparation gives way to a build that is asked for (no error, no
/// two builds), and is not started again for a view the user stopped it in.
#[test]
fn a_build_asked_for_replaces_the_background_one() {
    let (mut s, root, ids) = library("replace", 3);
    // held back: the photo on screen is being made all along (held until released below, not for a
    // deadline a slow test machine could pass before the build asked for comes)
    let hold = crate::prep::TestHold::new();
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "background": true})).unwrap();
    let bg = s.preview_build.clone().unwrap();
    assert!(bg.background && bg.running());
    assert!(s.activity.list().iter().any(|t| t.label == "Preparing photos in the background"), "{:?}", s.activity.list());
    // a second background build is refused; one that is asked for replaces it
    assert!(s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "background": true})).is_err());
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["done"], 3, "{r}");
    assert!(bg.cancel.load(Ordering::Relaxed), "the background build was stopped");
    drop(hold);
    // its workers stop before their library is removed under them
    assert!(wait_finished(&bg), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Note 48: the background preparation starts once the user steps through photos in the loupe
/// (not when a library opens), builds the view's previews in one cancellable row, and a stop
/// from that row holds for the view.
#[test]
fn the_background_preparation_starts_when_culling_and_can_be_stopped() {
    let (mut s, root, ids) = library("prep", 4);
    let mode = crate::prep::Mode::Library;
    s.prep_tick(mode, None, false);
    s.prep_tick(mode, Some(ids[0]), false);
    assert!(s.preview_build.is_none(), "opening and looking at one photo starts nothing");
    s.prep_tick(mode, Some(ids[1]), false);
    s.prep_tick(mode, Some(ids[2]), false);
    let b = s.preview_build.clone().expect("culling: the view is prepared");
    assert!(b.background);
    assert_eq!(b.total, 4);
    for _ in 0..600 {
        s.prep_tick(mode, Some(ids[2]), false);
        if !b.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!b.running(), "built");
    assert_eq!(b.done.load(Ordering::Relaxed), 4);
    s.prep_tick(mode, Some(ids[3]), false);
    assert!(same_build(&s, &b), "not started again for the same view");
    // switched off: nothing starts in a new view either
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"previews": false}}})).unwrap();
    let off = s.execute("prefs.backgroundPrep", &json!({})).unwrap();
    assert_eq!(off["settings"]["library"]["previews"], false, "{off}");
    assert_eq!(off["settings"]["culling"]["previews"], true, "each mode has its own");
    let _ = std::fs::remove_dir_all(root);
}

/// While editing, the whole view's build follows the Library row (its switch and "which photos");
/// the Editing row's scope is only for the photos kept ready ahead.
#[test]
fn editing_builds_the_view_by_the_library_rows_scope() {
    let (mut s, root, ids) = library("prep-develop", 4);
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"develop": {"scope": "onScreen"}, "library": {"scope": "marked"}}})).unwrap();
    s.execute("photo.rate", &json!({"ids": [ids[1].0], "rating": 3})).unwrap();
    let mode = crate::prep::Mode::Develop;
    for id in &ids {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the Library row asks for the view while editing");
    assert_eq!(b.total, 1, "only the marked photos, as the Library row says");
    b.cancel.store(true, Ordering::Relaxed);
    // its workers stop before their library is removed under them
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// With "which photos" = marked, marking another photo changes what the view's build prepares: a
/// build for the newly marked set follows (it wasn't, as the view itself stayed the same).
#[test]
fn marking_a_photo_prepares_it_in_the_marked_scope() {
    let (mut s, root, ids) = library("prep-marked", 4);
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"scope": "marked"}}})).unwrap();
    s.execute("photo.rate", &json!({"ids": [ids[1].0], "rating": 3})).unwrap();
    let mode = crate::prep::Mode::Library;
    let tick = |s: &mut Session, k: usize| s.prep_tick(mode, Some(ids[k % 3]), false);
    for k in 0..3 {
        tick(&mut s, k);
    }
    let first = s.preview_build.clone().expect("the marked photos are prepared");
    assert_eq!(first.total, 1);
    s.execute("photo.rate", &json!({"ids": [ids[3].0], "rating": 2})).unwrap();
    let mut next = None;
    for k in 0..600 {
        tick(&mut s, k);
        if let Some(b) = s.preview_build.clone().filter(|b| !std::sync::Arc::ptr_eq(b, &first)) {
            next = Some(b);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let next = next.expect("a build for the newly marked set");
    assert_eq!(next.total, 2);
    next.cancel.store(true, Ordering::Relaxed);
    // both builds' workers stop before their library is removed under them
    for b in [&first, &next] {
        assert!(wait_finished(b), "a cancelled build stops");
    }
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Waits (up to 30 s) for a build's workers to finish.
fn wait_finished(b: &crate::cmd::previews::PreviewBuild) -> bool {
    for _ in 0..1500 {
        if b.finished.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// A background build started for one view is stopped when another view is shown (here the
/// library changes under it), not left building photos no longer shown; it doesn't count as
/// stopped by the user, so the view now shown is prepared.
#[test]
fn a_view_change_stops_the_background_build_for_the_old_view() {
    let (mut s, root, ids) = library("prep-view", 6);
    let mode = crate::prep::Mode::Library;
    // the build's lanes wait at their foreground check until released below
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(b.running());
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[5] }).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    let stopped = b.cancel.load(Ordering::Relaxed);
    drop(hold);
    assert!(stopped, "the old view's build is stopped");
    // the user steps on in the view now shown
    for k in 0..600 {
        s.prep_tick(mode, Some(ids[k % 3]), false);
        if s.preview_build.as_ref().is_some_and(|p| !std::sync::Arc::ptr_eq(p, &b)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let next = s.preview_build.clone().expect("a build for the view now shown");
    assert!(!std::sync::Arc::ptr_eq(&next, &b));
    assert_eq!(next.total, 5);
    next.cancel.store(true, Ordering::Relaxed);
    // both builds' workers stop before their library is removed under them
    for b in [&b, &next] {
        assert!(wait_finished(b), "a cancelled build stops");
    }
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

fn same_build(s: &Session, b: &std::sync::Arc<crate::cmd::previews::PreviewBuild>) -> bool {
    s.preview_build.as_ref().is_some_and(|p| std::sync::Arc::ptr_eq(p, b))
}

/// Note 48: a library keeps its own background settings (a timelapse library prepares nothing),
/// saved with it; the app-wide ones apply elsewhere.
#[test]
fn a_library_can_keep_its_own_background_settings() {
    let (mut s, root, _ids) = library("own", 1);
    let r = s.execute("prefs.setBackgroundPrep", &json!({"library": true, "settings": {"culling": {"previews": false, "ahead": 0}}})).unwrap();
    assert_eq!(r["own"], true, "{r}");
    assert_eq!(r["settings"]["culling"]["ahead"], 0);
    assert_eq!(r["app"]["culling"]["ahead"], 5, "the app-wide ones are unchanged");
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    assert_eq!(s.prep_settings().culling.ahead, 0, "kept with the library");
    assert_eq!(s.prep_settings().ahead(crate::prep::Mode::Culling), 0);
    s.execute("prefs.setBackgroundPrep", &json!({"reset": true})).unwrap();
    assert_eq!(s.prep_settings().culling.ahead, 5, "back to the app-wide settings");
    assert!(s.execute("prefs.setBackgroundPrep", &json!({"settings": 3})).is_err());
    let _ = std::fs::remove_dir_all(root);
}

/// Note 47: Build Previews with one photo open (selected) builds every photo in view, as its menu
/// item says ("Visible"); it built only the open photo, so culling after it found nothing built.
#[test]
fn build_previews_with_one_photo_open_builds_the_view() {
    let mut s = Session::with_demo();
    let n = s.visible_cloned().len();
    assert!(n > 2);
    let one = s.visible_cloned()[0];
    s.selection = crate::Selection::single(one);
    let r = s.execute("library.buildPreviews", &json!({"edge": 256, "wait": true})).unwrap();
    assert_eq!(r["total"], n, "{r}");
    let two: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    s.selection = crate::Selection { ids: two.clone(), active: two.first().copied() };
    let r = s.execute("library.buildPreviews", &json!({"edge": 256, "wait": true})).unwrap();
    assert_eq!(r["total"], 2, "a choice of photos is built alone {r}");
}

/// A view the loupe kept only in memory (its deferred disk write skipped when the writer fell behind) is not
/// counted as built: the build renders it and writes it to disk (CodeRabbit on #3077).
#[test]
fn a_preview_kept_only_in_memory_is_built_and_written() {
    let (mut s, root, ids) = library("memonly", 1);
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    assert!(!jobs.is_empty());
    for j in &jobs {
        let img = std::sync::Arc::new(lightcraft_raster::Rgba8::new(300, 200));
        if let Some((cache, key)) = &j.view_cache {
            cache.put_memory(*key, img);
        } else if let Some((cache, key)) = &j.cache {
            cache.put_memory(*key, img);
        }
    }
    assert!(!crate::cmd::previews::already_built(&jobs), "a copy only in memory is not a built preview");
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!((r["skipped"].as_u64(), r["failed"].as_u64()), (Some(0), Some(0)), "{r}");
    let p = s.catalog.photo(ids[0]).unwrap().clone();
    let key = s.view_key(&p, true);
    assert!(s.media.rendered.disk().and_then(|d| d.dims(key)).is_some(), "the view preview is on disk");
    let _ = std::fs::remove_dir_all(root);
}

/// A view preview the cache folder can't take (full, or not writable) fails the photo instead of counting it
/// as built (CodeRabbit on #3077).
#[test]
fn a_preview_that_cannot_be_written_fails_the_build() {
    let (mut s, root, ids) = library("unwritable", 2);
    let dir = s.media.rendered.disk().expect("a library has a disk cache").dir().to_path_buf();
    let _ = std::fs::remove_dir_all(&dir);
    // a file where the cache folder goes: nothing can be written under it
    std::fs::write(&dir, b"not a folder").unwrap();
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["failed"].as_u64(), Some(2), "{r}");
    let _ = std::fs::remove_file(&dir);
    let _ = std::fs::remove_dir_all(root);
}

/// A photo whose preview cache is discarded while it renders is not counted as built: its preview isn't kept
/// (CodeRabbit on #3077).
#[test]
fn a_preview_rendered_for_a_discarded_cache_fails_the_photo() {
    let (mut s, root, ids) = library("discarded", 1);
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    s.execute("library.clearPreviews", &json!({})).unwrap();
    assert!(!crate::cmd::previews::run_photo(jobs), "nothing was kept: the photo is not built");
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    assert!(crate::cmd::previews::run_photo(jobs), "built for the cache as it is now");
    let _ = std::fs::remove_dir_all(root);
}

/// A forced build renders a photo itself even when another build was rendering it and made its previews
/// (CodeRabbit on #3080): the other build's result is not taken as this one's.
#[test]
fn a_forced_build_renders_a_photo_another_build_was_rendering() {
    let (mut s, root, ids) = library("forced-shared", 1);
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    let held = crate::cmd::previews::hold_building(ids[0].0, &jobs).expect("nobody renders it now");
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "force": true})).unwrap();
    let b = s.preview_build.clone().expect("the build");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(b.done.load(Ordering::Relaxed), 0, "not counted while the other build holds it");
    drop(held);
    assert!(wait_finished(&b));
    let counts =
        (b.done.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed), b.shared.load(Ordering::Relaxed), b.skipped.load(Ordering::Relaxed));
    assert_eq!(counts, (1, 0, 0, 0), "rendered here, not taken from the other build");
    let _ = std::fs::remove_dir_all(root);
}

/// A photo another build is rendering is not counted until that render is over: here the other build never
/// made its previews, so this build renders it (CodeRabbit on #3080).
#[test]
fn a_photo_another_build_is_rendering_is_counted_only_once_it_is_built() {
    let (mut s, root, ids) = library("shared", 1);
    let jobs = crate::cmd::previews::jobs_for(&mut s, ids[0], Some(300));
    let held = crate::cmd::previews::hold_building(ids[0].0, &jobs).expect("nobody renders it yet");
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300})).unwrap();
    let b = s.preview_build.clone().expect("the build");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(b.done.load(Ordering::Relaxed), 0, "not counted while the other build holds it");
    drop(held);
    assert!(wait_finished(&b));
    assert_eq!((b.done.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed), b.shared.load(Ordering::Relaxed)), (1, 0, 0));
    let p = s.catalog.photo(ids[0]).unwrap().clone();
    let key = s.view_key(&p, true);
    assert!(s.media.rendered.disk().and_then(|d| d.dims(key)).is_some(), "rendered here, on disk");
    let _ = std::fs::remove_dir_all(root);
}

/// CodeRabbit on #3544: a library preparation setting that can't be saved is not kept, nor is
/// going back to the app-wide settings: the session stays as the command's error says.
#[test]
fn a_prep_setting_that_cant_be_saved_is_not_kept() {
    use crate::prep::PrepSettings;
    let (mut s, root, _) = library("prep-unsaved", 1);
    let mut own = PrepSettings::default();
    own.culling.ahead = 3;
    s.set_prep_settings(own, true).unwrap();
    // prefs.json can't be written: a folder stands in its place
    let prefs = root.join("lib").join("prefs.json");
    let _ = std::fs::remove_file(&prefs);
    std::fs::create_dir_all(prefs.join("in-the-way")).unwrap();
    let mut other = own;
    other.culling.ahead = 5;
    assert!(s.set_prep_settings(other, true).is_err());
    assert_eq!(s.prep_json()["library"], json!(own), "the setting that couldn't be saved is not kept");
    assert!(s.clear_library_prep().is_err());
    assert_eq!(s.prep_json()["library"], json!(own), "nor going back to the app-wide settings");
    let _ = std::fs::remove_dir_all(&root);
}

/// CodeRabbit on #3544: a build asked for with malformed ids fails without stopping the
/// background preparation that is running.
#[test]
fn a_bad_build_request_leaves_the_background_build_running() {
    let _hold = crate::prep::TestHold::new();
    let (mut s, root, ids) = library("prep-bad-ids", 3);
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "background": true})).unwrap();
    let b = s.preview_build.clone().expect("the background build");
    assert!(b.running());
    assert!(s.execute("library.buildPreviews", &json!({"ids": ["x"]})).is_err(), "ids that are not photo ids");
    assert!(!b.cancel.load(std::sync::atomic::Ordering::Relaxed), "the background build was not stopped");
    let _ = std::fs::remove_dir_all(&root);
}

/// CodeRabbit on #3577: smart previews asked for in the background with malformed ids fail without
/// stopping the background preparation either.
#[test]
fn a_bad_smart_preview_request_leaves_the_background_build_running() {
    let _hold = crate::prep::TestHold::new();
    let (mut s, root, ids) = library("prep-bad-smart-ids", 3);
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "background": true})).unwrap();
    let b = s.preview_build.clone().expect("the background build");
    assert!(b.running());
    assert!(s.execute("library.smartPreviews", &json!({"ids": ["x"], "background": true})).is_err(), "ids that are not photo ids");
    assert!(!b.cancel.load(std::sync::atomic::Ordering::Relaxed), "the background build was not stopped");
    // a smart-preview worker that can't start: the background build carries on, and stays the session's
    crate::cmd::previews::test_gate::fail_next_spawn();
    assert!(s.execute("library.smartPreviews", &json!({"ids": ids_json(&ids), "background": true})).is_err(), "the worker didn't start");
    assert!(!b.cancel.load(std::sync::atomic::Ordering::Relaxed), "the background build was not stopped");
    assert!(s.preview_build.as_ref().is_some_and(|now| std::sync::Arc::ptr_eq(now, &b)), "and is still the session's build");
    // (stopped and finished before its folder goes)
    b.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(_hold);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while b.running() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// CodeRabbit on #3544: a build asked for whose worker can't start fails without stopping or
/// replacing the background preparation that is running.
#[test]
fn a_build_that_cant_start_leaves_the_background_build_running() {
    let _hold = crate::prep::TestHold::new();
    let (mut s, root, ids) = library("prep-no-thread", 3);
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "background": true})).unwrap();
    let b = s.preview_build.clone().expect("the background build");
    assert!(b.running());
    crate::cmd::previews::test_gate::fail_next_spawn();
    assert!(s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids)})).is_err(), "the worker didn't start");
    assert!(!b.cancel.load(std::sync::atomic::Ordering::Relaxed), "the background build was not stopped");
    assert!(s.preview_build.as_ref().is_some_and(|now| std::sync::Arc::ptr_eq(now, &b)), "and is still the session's build");
    let _ = std::fs::remove_dir_all(&root);
}

/// CodeRabbit on #3080: a background build that can't start isn't tried again on every tick; the
/// view tries again once [`crate::prep::RETRY_AFTER`] has passed.
#[test]
fn a_build_that_cant_start_waits_before_trying_again() {
    let (mut s, root, ids) = library("prep-retry", 3);
    let mode = crate::prep::Mode::Library;
    s.prep.fail_starts = 1;
    for id in &ids {
        s.prep_tick(mode, Some(*id), false);
    }
    assert!(s.preview_build.is_none(), "the start failed");
    assert!(s.prep.retry_at.is_some());
    // the app is told when to look again (a frame is asked for then)
    assert!(s.prep_retry_in().is_some_and(|d| d <= crate::prep::RETRY_AFTER && d > crate::prep::RETRY_AFTER / 2));
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(s.preview_build.is_none(), "not tried again at once");
    // the wait is over: no more frames asked for it (CodeRabbit on #3077 / #3080)
    s.prep.retry_at = s.prep.retry_at.map(|(view, _)| (view, web_time::Instant::now()));
    assert_eq!(s.prep_retry_in(), None, "a deadline that has passed asks for no frame");
    s.prep_tick(mode, Some(ids[2]), false);
    let b = s.preview_build.clone().expect("tried again");
    assert!(b.background && s.prep.retry_at.is_none());
    b.cancel.store(true, Ordering::Relaxed);
    let _ = wait_finished(&b);
    let _ = std::fs::remove_dir_all(&root);
}

type Build = std::sync::Arc<crate::cmd::previews::PreviewBuild>;

/// Whether the library's `prefs.json` says a background build is unfinished (`prepUnfinished`).
fn marked_unfinished(root: &Path) -> bool {
    let prefs = std::fs::read(root.join("lib").join("prefs.json")).unwrap_or_default();
    let prefs: serde_json::Value = serde_json::from_slice(&prefs).unwrap_or_default();
    prefs["prepUnfinished"].as_bool().unwrap_or(false)
}

/// Ticks from the grid (no photo in the loupe) until a preview build other than `old` is the
/// session's, for up to 12 s.
fn build_after(s: &mut Session, old: &Build) -> Option<Build> {
    for _ in 0..600 {
        s.prep_tick(crate::prep::Mode::Library, None, false);
        if let Some(b) = s.preview_build.clone().filter(|b| !std::sync::Arc::ptr_eq(b, old)) {
            return Some(b);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}

/// #3947: a background build stopped because the view changed is taken up again for the view now
/// shown from the grid, with no photo in the loupe (it started only in the loupe, after three more
/// photos stepped through, so a sort or an import in the grid ended the preparation for good).
#[test]
fn a_build_stopped_by_a_view_change_is_taken_up_again_from_the_grid() {
    let (mut s, root, ids) = library("prep-resume", 6);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(b.running() && s.prep.unfinished, "a build that started is unfinished until it has run to its end");
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[5] }).unwrap();
    s.prep_tick(mode, None, false);
    assert!(b.cancel.load(Ordering::Relaxed), "the old view's build is stopped, from the grid too");
    drop(hold);
    assert!(wait_finished(&b), "a cancelled build stops");
    // the grid only: no photo is opened
    let next = build_after(&mut s, &b).expect("a build for the view now shown, started from the grid");
    assert!(next.background);
    assert_eq!(next.total, 5);
    next.cancel.store(true, Ordering::Relaxed);
    // its workers stop before their library is removed under them
    assert!(wait_finished(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947: the app quit while the background build ran: the library says so in its `prefs.json`, and
/// the next session takes the build up again from the grid. Once it has run to its end nothing is
/// unfinished, and a third session starts nothing.
#[test]
fn an_unfinished_build_is_taken_up_again_after_a_restart() {
    let (mut s, root, ids) = library("prep-restart", 4);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(marked_unfinished(&root), "saved when the build starts");
    // the app quits: the session never sees its build end (the workers are stopped here only
    // because the test's process lives on)
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b));
    drop(s);
    drop(hold);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    assert!(s.prep.unfinished, "read with the library");
    s.prep_tick(mode, None, false);
    let next = s.preview_build.clone().expect("taken up again from the grid");
    assert!(next.background);
    assert_eq!(next.total, 4);
    assert!(wait_finished(&next), "built");
    assert!(!next.cancel.load(Ordering::Relaxed));
    assert_eq!(next.done.load(Ordering::Relaxed), 4);
    s.prep_tick(mode, None, false);
    assert!(!s.prep.unfinished && !marked_unfinished(&root), "run to its end: nothing is unfinished, and the library says so");
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &next), "not started again");
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    assert!(s.preview_build.is_none(), "a library with nothing unfinished starts nothing from the grid");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947: what an unfinished build still has to do is not stored: the build taken up again asks
/// photo by photo whether the previews are on disk, skips those, and leaves every photo with its
/// view render and its 512 px grid thumbnail.
#[test]
fn a_build_taken_up_again_builds_only_what_is_missing() {
    let (mut s, root, ids) = library("prep-missing", 6);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids[..2]), "wait": true})).unwrap();
    assert_eq!((r["done"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(0)), "{r}");
    let asked = s.preview_build.clone().expect("the build asked for");
    // (as left by a build that didn't run to its end)
    s.prep.unfinished = true;
    let b = build_after(&mut s, &asked).expect("taken up again from the grid");
    assert!(b.background);
    assert!(wait_finished(&b), "built");
    let counts = (b.total, b.done.load(Ordering::Relaxed), b.skipped.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed));
    assert_eq!(counts, (6, 6, 2, 0), "the two built before are skipped, the others built");
    let edge = crate::cmd::previews::standard_edge(&s);
    for id in &ids {
        let jobs = crate::cmd::previews::jobs_for(&mut s, *id, Some(edge));
        assert!(crate::cmd::previews::already_built(&jobs), "{id:?}: as the build's own check sees it");
        let thumb = s.thumb_job(*id, 512).and_then(|j| j.cache);
        assert!(thumb.is_some_and(|(cache, key)| cache.stored_dims(key).is_some()), "{id:?}: the 512 px grid thumbnail is on disk");
    }
    s.prep_tick(crate::prep::Mode::Library, None, false);
    assert!(!s.prep.unfinished, "run to its end");
    let _ = std::fs::remove_dir_all(root);
}

/// Note 36 stands with #3947: a library in which no background build was left unfinished starts
/// nothing from the grid, nor after fewer than three photos in the loupe.
#[test]
fn a_library_with_nothing_unfinished_starts_nothing_from_the_grid() {
    let (mut s, root, ids) = library("prep-fresh", 4);
    let mode = crate::prep::Mode::Library;
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    s.prep_tick(mode, Some(ids[0]), false);
    s.prep_tick(mode, Some(ids[1]), false);
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    assert!(s.preview_build.is_none(), "nothing starts");
    assert!(!s.prep.unfinished && !marked_unfinished(&root), "and nothing is noted as unfinished");
    assert_eq!(s.prep_retry_in(), None, "no frame is asked for");
    let _ = std::fs::remove_dir_all(root);
}

/// #3947: previews switched off and on again while the build ran is not the user stopping the
/// build: the view is prepared again (it was taken as stopped from the row, and not prepared
/// again in that session).
#[test]
fn switching_previews_off_and_on_is_not_a_stop_by_the_user() {
    let (mut s, root, ids) = library("prep-toggle", 4);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(b.running());
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"previews": false}}})).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(b.cancel.load(Ordering::Relaxed), "switched off: stopped");
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"previews": true}}})).unwrap();
    // (on again while the lanes may still be stopping)
    s.prep_tick(mode, Some(ids[2]), false);
    drop(hold);
    assert!(wait_finished(&b), "a cancelled build stops");
    // the same photo in the same view: no further steps
    let mut next = None;
    for _ in 0..10 {
        s.prep_tick(mode, Some(ids[2]), false);
        next = s.preview_build.clone().filter(|p| !std::sync::Arc::ptr_eq(p, &b));
        if next.is_some() {
            break;
        }
    }
    let next = next.expect("the same view is prepared again");
    assert!(next.background);
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947 keeps note 48's stop: a build stopped from its row is not started again for that view,
/// from the grid or the loupe, and is not unfinished: a later session doesn't take it up either.
#[test]
fn a_build_stopped_from_its_row_is_not_taken_up_again() {
    let (mut s, root, ids) = library("prep-stopped", 4);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(marked_unfinished(&root));
    // (what the row's ✕ does)
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    drop(hold);
    assert!(wait_finished(&b), "a cancelled build stops");
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    for id in &ids {
        s.prep_tick(mode, Some(*id), false);
    }
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &b), "not started again for this view, from the grid or the loupe");
    assert!(!s.prep.unfinished && !marked_unfinished(&root), "the user said no: nothing is unfinished");
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    assert!(s.preview_build.is_none(), "nor in a later session");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947: an unfinished build is taken up again only once the view has stayed the same for a while
/// ([`crate::prep::SETTLE_AFTER`]): two changes one after the other (an import's batches, two sorts
/// tried) start one build, not two, and the app is told when to look again.
#[test]
fn a_view_that_keeps_changing_starts_one_build_once_it_is_still() {
    let (mut s, root, ids) = library("prep-settle", 7);
    let mode = crate::prep::Mode::Library;
    // (a wait that ends only when the test says so)
    let long = std::time::Duration::from_secs(3600);
    s.prep.settle = Some(long);
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("a view's first build doesn't wait: the steps are its guard");
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[6] }).unwrap();
    s.prep_tick(mode, None, false);
    drop(hold);
    assert!(wait_finished(&b), "the old view's build stops");
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &b), "the view just changed: not yet");
    assert!(s.prep_retry_in().is_some_and(|d| d <= long && d > long / 2), "a frame is asked for when the wait ends");
    // the wait is over, but the view has changed again: it starts anew
    s.prep.not_before = Some(web_time::Instant::now());
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[5] }).unwrap();
    s.prep_tick(mode, None, false);
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &b), "changed again: still no build");
    assert!(s.prep_retry_in().is_some_and(|d| d > long / 2), "the wait started anew");
    // the view has been still for long enough
    s.prep.not_before = Some(web_time::Instant::now());
    assert_eq!(s.prep_retry_in(), None, "a wait that is over asks for no frame");
    s.prep_tick(mode, None, false);
    let next = s.preview_build.clone().filter(|p| !std::sync::Arc::ptr_eq(p, &b)).expect("one build, for the view as it is now");
    assert!(next.background);
    assert_eq!(next.total, 5);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #4093: a view signature is only its photos' ids and their count, so two
/// libraries can carry the same one. The view guards, waits, and step count the first library
/// earned must not follow the session into the second: a build that ran to its end there
/// leaves `started_for` set, and the second library's owed build would never start.
#[test]
fn an_unfinished_build_is_taken_up_when_its_library_opens_after_another() {
    let mode = crate::prep::Mode::Library;
    // the second library, left by a session whose build did not run to its end
    let (mut s2, root2, ids2) = library("prep-switch-second", 4);
    let hold = crate::prep::TestHold::new();
    for id in &ids2[..3] {
        s2.prep_tick(mode, Some(*id), false);
    }
    let b2 = s2.preview_build.clone().expect("the second library's build started");
    assert!(marked_unfinished(&root2), "its build is owed");
    b2.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b2), "a cancelled build stops");
    drop(s2);
    drop(hold);
    // the first library, in the session that switches: its build runs to its end
    let (mut s, root, ids) = library("prep-switch-first", 4);
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the first library's view is prepared");
    assert!(wait_finished(&b), "built");
    s.prep_tick(mode, None, false);
    assert!(!s.prep.unfinished, "run to its end: nothing is owed here");
    // the switch: the second library is owed a build for the same view signature
    s.open_library(root2.join("lib"), false).unwrap();
    assert!(s.prep.unfinished, "read with the second library");
    let next = build_after(&mut s, &b).expect("the second library's owed build is taken up, not blocked by the first library's guards");
    assert!(next.background);
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&root2).unwrap();
}

/// CodeRabbit on #4093 (round 2): the round-1 reset forgets `started_for` when a library
/// opens, so the view-change stop can no longer tell a build that is still running apart:
/// a build left running by the library before would keep building that library's photos
/// and hold up this one's preparation until it ran to its end. A build is stopped when the
/// library it was started in is left, and the library now open is prepared in its place.
#[test]
fn a_running_build_is_stopped_when_its_library_is_left() {
    let mode = crate::prep::Mode::Library;
    // the second library, left by a session whose build did not run to its end
    let (mut s2, root2, ids2) = library("prep-switch-running-second", 4);
    let hold = crate::prep::TestHold::new();
    for id in &ids2[..3] {
        s2.prep_tick(mode, Some(*id), false);
    }
    let b2 = s2.preview_build.clone().expect("the second library's build started");
    assert!(marked_unfinished(&root2), "its build is owed");
    b2.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b2), "a cancelled build stops");
    drop(s2);
    drop(hold);
    // the first library, in the session that switches: its build is still running
    let (mut s, root, ids) = library("prep-switch-running-first", 4);
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the first library's view is prepared");
    assert!(b.running(), "still running when its library is left");
    // the switch, with the first library's build still running
    s.open_library(root2.join("lib"), false).unwrap();
    s.prep_tick(mode, None, false);
    let stopped = b.cancel.load(Ordering::Relaxed);
    drop(hold);
    assert!(stopped, "the build of the library that was left is stopped");
    assert!(wait_finished(&b), "a cancelled build stops");
    let next = build_after(&mut s, &b).expect("the second library's owed build is taken up once the old build stops");
    assert!(next.background);
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&root2).unwrap();
}

/// CodeRabbit on #4093 (round 3): a stop from the build's row only sets the build's cancel
/// flag, so while that build is still running, a view change (or previews switched off) runs
/// the preparation's own stop branches, which clear `started_for` — the user's stop is
/// reclassified as the preparation's, the build stays owed, and it is taken up again once
/// the view settles. The gate holds the build at its first photo, so the stop and the view
/// change both land while it runs.
#[test]
fn a_build_stopped_from_its_row_is_not_reclassified_by_a_view_change() {
    use crate::cmd::previews::test_gate::{self, PreviewGate, ReleaseOnDrop};
    let (mut s, root, ids) = library("prep-stopped-view", 4);
    let mode = crate::prep::Mode::Library;
    let gate = PreviewGate::new();
    // also opens the gate if an assertion below panics, so the worker is never stranded
    let release = ReleaseOnDrop(gate.clone());
    test_gate::install(gate.clone());
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(gate.wait_entered(1), "a lane reached its first photo");
    assert!(b.running(), "held at its first photo");
    assert!(marked_unfinished(&root), "its build is owed");
    // (what the row's stop does: only the cancel flag is set)
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    // the view changes while the stopped build is still running
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[3] }).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(b.running(), "still held: the stop has not landed yet");
    drop(release);
    assert!(wait_finished(&b), "a cancelled build stops");
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    assert!(same_build(&s, &b), "a build the user stopped is not taken up again after the view change");
    assert!(!s.prep.unfinished && !marked_unfinished(&root), "the user said no: nothing is unfinished");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #4093 (round 4): the round-3 guard keeps a user stop while the build is
/// still running, but if previews are switched off and the build ends before they are
/// switched on again, its end is followed with previews unwanted: `started_for` was
/// cleared and the build stayed owed, so switching previews back on took up a build the
/// user had stopped. The gate holds the build at its first photo, so the stop and the
/// switch both land while it runs, and the build ends while previews are off.
#[test]
fn a_build_stopped_from_its_row_is_not_reclassified_by_previews_switched_off() {
    use crate::cmd::previews::test_gate::{self, PreviewGate, ReleaseOnDrop};
    let (mut s, root, ids) = library("prep-stopped-off", 4);
    let mode = crate::prep::Mode::Library;
    let gate = PreviewGate::new();
    // also opens the gate if an assertion below panics, so the worker is never stranded
    let release = ReleaseOnDrop(gate.clone());
    test_gate::install(gate.clone());
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(gate.wait_entered(1), "a lane reached its first photo");
    assert!(b.running(), "held at its first photo");
    assert!(marked_unfinished(&root), "its build is owed");
    // (what the row's stop does: only the cancel flag is set)
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    // previews are switched off while the stopped build is still running
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"previews": false}}})).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(b.running(), "still held: the stop has not landed yet");
    drop(release);
    assert!(wait_finished(&b), "a cancelled build stops");
    // the build's end is followed while previews are off
    s.prep_tick(mode, Some(ids[2]), false);
    // previews on again: a build the user stopped is not taken up again
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"previews": true}}})).unwrap();
    for _ in 0..3 {
        s.prep_tick(mode, None, false);
    }
    assert!(same_build(&s, &b), "a build the user stopped is not taken up again once previews are on again");
    assert!(!s.prep.unfinished && !marked_unfinished(&root), "the user said no: nothing is unfinished");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}
