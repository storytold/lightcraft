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

/// A background build's photos are planned a slice at a time, a frame each ([`crate::cmd::previews::jobs_for`]
/// costs every photo a settings hash and a cache key: all of a big view in one frame froze the app for
/// seconds), and the build starts once every photo is planned.
#[test]
fn a_background_build_plans_a_slice_of_photos_a_frame() {
    let (mut s, root, ids) = library("prep-slices", 7);
    s.prep.slice = Some(2);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    s.prep_tick(mode, None, false);
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    assert_eq!(s.prep.planned, 2, "the first slice, not the view");
    assert!(s.preview_build.is_none() && s.prep_planning(), "still planning");
    let mut planned = s.prep.planned;
    for _ in 0..10 {
        s.prep_tick(mode, Some(ids[2]), false);
        assert!(s.prep.planned - planned <= 2, "a frame plans a slice at most");
        planned = s.prep.planned;
        if s.preview_build.is_some() {
            break;
        }
    }
    let b = s.preview_build.clone().expect("every photo planned: the build starts");
    assert_eq!((planned, b.total), (7, 7));
    assert!(!s.prep_planning(), "nothing left to plan");
    drop(hold);
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Steps through the first photos of a view in `mode` until its build's photos are being planned, a slice
/// (two photos) a frame.
fn start_planning(s: &mut Session, ids: &[PhotoId], mode: crate::prep::Mode) {
    s.prep.slice = Some(2);
    s.prep_tick(mode, None, false);
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    assert_eq!(s.prep.planned, 2, "the first slice, not the view");
    assert!(s.preview_build.is_none() && s.prep_planning(), "still planning");
}

/// Ticks on (the user at the third photo) until the plan is done and the build starts.
fn finish_planning(s: &mut Session, ids: &[PhotoId], mode: crate::prep::Mode) -> std::sync::Arc<crate::cmd::previews::PreviewBuild> {
    for _ in 0..30 {
        s.prep_tick(mode, Some(ids[2]), false);
        if let Some(b) = &s.preview_build {
            return b.clone();
        }
    }
    panic!("the build never started");
}

/// Every photo's view and grid previews are there under the keys its settings make now.
fn all_built(s: &mut Session, ids: &[PhotoId]) -> bool {
    let edge = crate::cmd::previews::standard_edge(s);
    ids.iter().all(|id| crate::cmd::previews::already_built(&crate::cmd::previews::jobs_for(s, *id, Some(edge))))
}

/// CodeRabbit on #3936: a preview cache cleared between planning slices isn't saved into by the jobs of the slices
/// before (they would fail, and the photos not be built): the plan begins again against the cache as it is now.
#[test]
fn a_cache_cleared_while_planning_is_planned_again() {
    let (mut s, root, ids) = library("prep-plan-clear", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    s.execute("library.clearPreviews", &json!({})).unwrap();
    let b = finish_planning(&mut s, &ids, mode);
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    assert_eq!((b.total, b.failed.load(Ordering::Relaxed)), (7, 0), "no photo is lost to the cache it was planned for");
    assert!(all_built(&mut s, &ids), "every photo is built into the cache as it is now");
    assert_eq!(s.prep.planned, 2 + 7, "the two photos planned before the clear are planned again");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Opens another library in `s` holding 7 photos of the same names as [`library`]'s (so the same photo ids, in the
/// same order): a view that looks like the one before. Its folder, and its photos by name.
fn open_other_library(s: &mut Session, tag: &str) -> (PathBuf, Vec<PhotoId>) {
    let other = temp_dir(tag);
    let paths: Vec<String> = (0..7)
        .map(|i| {
            let p = other.join("shoot").join(format!("p{i:02}.png"));
            png(&p, (i * 20 + 7) as u8);
            p.to_string_lossy().to_string()
        })
        .collect();
    s.open_library(other.join("lib"), false).unwrap();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let mut now: Vec<(String, PhotoId)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.id)).collect();
    now.sort();
    (other, now.into_iter().map(|(_, id)| id).collect())
}

/// Steps through the first three photos of the view now shown (the build waits for three steps in a view).
fn step_through(s: &mut Session, ids: &[PhotoId], mode: crate::prep::Mode) {
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
}

/// CodeRabbit on #3936: another library that shows the same photo ids (and as many photos) is not the one whose
/// photos were being planned: the plan of the library before is dropped, not resumed against it.
#[test]
fn a_library_opened_while_planning_is_planned_afresh() {
    let (mut s, root, ids) = library("prep-plan-lib-a", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let (other, now) = open_other_library(&mut s, "prep-plan-lib-b");
    assert_eq!(now, ids, "the same photo ids, and as many photos: a view that looks the same");
    step_through(&mut s, &now, mode);
    let b = finish_planning(&mut s, &now, mode);
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    assert_eq!((b.total, b.failed.load(Ordering::Relaxed)), (7, 0), "no photo is rendered for the library before");
    assert!(all_built(&mut s, &now), "every photo of the library now open is built");
    assert_eq!(s.prep.planned, 2 + 7);
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

/// A library just opened (no op applied to its catalog yet: the catalog doesn't know what changed since its
/// revision) is planned through all its slices and its build starts, as it did before plans could be dropped.
#[test]
fn a_library_just_opened_is_planned_through_to_its_build() {
    let (s, root, _) = library("prep-plan-reopen", 7);
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    let ids = s.visible_cloned();
    assert_eq!(ids.len(), 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let b = finish_planning(&mut s, &ids, mode);
    assert_eq!(b.total, 7);
    assert_eq!(s.prep.planned, 7, "planned once, not again on every frame");
    drop(hold);
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #3936: opening another library that shows the same photo ids (and as many photos) stops the
/// background build of the library before, which renders for a cache that is gone, and prepares the new one.
#[test]
fn a_library_opened_while_its_build_runs_stops_it_and_prepares_the_new_one() {
    let (mut s, root, ids) = library("prep-run-a", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    step_through(&mut s, &ids, mode);
    let before = s.preview_build.clone().expect("the first library's build");
    let (other, now) = open_other_library(&mut s, "prep-run-b");
    assert_eq!(now, ids);
    let mut next = None;
    for k in 0..200 {
        s.prep_tick(mode, Some(now[k % 3]), false);
        if s.preview_build.as_ref().is_some_and(|p| !std::sync::Arc::ptr_eq(p, &before)) {
            next = s.preview_build.clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let next = next.expect("a build for the library now open");
    assert!(before.cancel.load(Ordering::Relaxed), "the first library's build was stopped");
    drop(hold);
    for b in [&before, &next] {
        assert!(wait_finished(b), "the builds end");
    }
    assert_eq!((next.total, next.failed.load(Ordering::Relaxed)), (7, 0));
    assert!(all_built(&mut s, &now), "every photo of the library now open is built");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

/// CodeRabbit on #3936: a build the user stopped declines its view for that library only: another library that
/// shows the same photo ids (and as many photos) is prepared.
#[test]
fn a_library_opened_after_a_stopped_build_is_prepared() {
    let (mut s, root, ids) = library("prep-declined-a", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    step_through(&mut s, &ids, mode);
    let stopped = s.preview_build.clone().expect("the first library's build");
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    assert!(wait_finished(&stopped), "a cancelled build stops");
    for _ in 0..5 {
        s.prep_tick(mode, Some(ids[2]), false);
    }
    assert!(s.preview_build.as_ref().is_some_and(|p| std::sync::Arc::ptr_eq(p, &stopped)), "not built again for the view the user stopped");
    let (other, now) = open_other_library(&mut s, "prep-declined-b");
    assert_eq!(now, ids);
    step_through(&mut s, &now, mode);
    let next = finish_planning(&mut s, &now, mode);
    assert!(!std::sync::Arc::ptr_eq(&next, &stopped), "a build for the library now open");
    drop(hold);
    assert!(wait_finished(&next), "the build ends");
    assert_eq!((next.total, next.failed.load(Ordering::Relaxed)), (7, 0));
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

/// CodeRabbit on #3936: `library.cancelPreviews` while the photos are still being planned (no build runs yet) ends
/// the plan, and its view is not planned again; another view is.
#[test]
fn cancelling_previews_while_planning_ends_the_plan() {
    let (mut s, root, ids) = library("prep-plan-cancel", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    assert!(!s.prep_planning(), "nothing left to plan");
    for _ in 0..30 {
        s.prep_tick(mode, Some(ids[2]), false);
    }
    assert!(s.preview_build.is_none(), "no build for the view that was stopped");
    assert_eq!(s.prep.planned, 2, "and no planning of it");
    // another view is prepared, once the user has stepped through a few of its photos
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[6] }).unwrap();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = finish_planning(&mut s, &ids, mode);
    assert_eq!(b.total, 6);
    drop(hold);
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #3936: a photo edited after its slice was planned is planned again, so its build renders (and keys)
/// it as it is now; a rating changes nothing a render is made from, and the plan goes on.
#[test]
fn an_edit_while_planning_plans_the_photos_again_but_a_rating_does_not() {
    let (mut s, root, ids) = library("prep-plan-edit", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    // (the view's first photo is planned in the first slice: whichever order the view shows them in)
    let planned_first = s.visible_cloned()[0];
    s.execute("photo.rate", &json!({"ids": [planned_first.0], "rating": 3})).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert_eq!(s.prep.planned, 4, "a rating: the plan went on with the next slice");
    s.selection = crate::Selection::single(planned_first);
    let before = s.catalog.photo(planned_first).unwrap().develop.hash64();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert_ne!(s.catalog.photo(planned_first).unwrap().develop.hash64(), before, "the photo was edited");
    let b = finish_planning(&mut s, &ids, mode);
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    assert_eq!((b.total, b.failed.load(Ordering::Relaxed)), (7, 0));
    assert!(all_built(&mut s, &ids), "the edited photo is built as it is now");
    assert_eq!(s.prep.planned, 4 + 7, "the edit began the plan again");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #3936: no preview job reads a photo's analysis (focus and exposure scores, kept by assisted culling
/// in the background), so one stored while the photos are being planned doesn't begin the plan again.
#[test]
fn an_analysis_stored_while_planning_does_not_begin_the_plan_again() {
    let (mut s, root, ids) = library("prep-plan-analysis", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let planned_first = s.visible_cloned()[0];
    s.catalog.apply(lightcraft_catalog::Op::SetAnalysis { id: planned_first, analysis: Some(lightcraft_catalog::Analysis::default()) }).unwrap();
    let b = finish_planning(&mut s, &ids, mode);
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    assert_eq!((b.total, b.failed.load(Ordering::Relaxed)), (7, 0));
    assert_eq!(s.prep.planned, 7, "each photo planned once: the plan went on");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #3936: a flood of changes that touch nothing a render is made from (more than the catalog remembers
/// photo by photo, over the ticks of one plan) doesn't begin the plan again: each tick looks only at what changed
/// since the tick before.
#[test]
fn many_unrelated_changes_over_a_plan_do_not_begin_it_again() {
    let (mut s, root, ids) = library("prep-plan-flood", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let rate = |s: &mut Session, n: usize| {
        for k in 0..n {
            s.catalog.apply(lightcraft_catalog::Op::SetRating { id: ids[k % ids.len()], rating: (k % 5) as u8 }).unwrap();
        }
    };
    let mut started = None;
    for _ in 0..30 {
        // (more than 8,192 over the plan, far fewer than that between two ticks)
        rate(&mut s, 3000);
        s.prep_tick(mode, Some(ids[2]), false);
        if let Some(b) = &s.preview_build {
            started = Some(b.clone());
            break;
        }
    }
    let b = started.expect("the build started");
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    assert_eq!((b.total, b.failed.load(Ordering::Relaxed)), (7, 0));
    assert_eq!(s.prep.planned, 7, "each photo planned once: the plan went on");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// The rows the background preparation has in the activity stack.
fn prep_rows(s: &Session) -> Vec<crate::activity::TaskInfo> {
    s.activity.list().into_iter().filter(|t| t.kind == "previews").collect()
}

/// @eliets2 on #3936: a big view takes seconds to plan, and nothing showed that the preparation was under way: its
/// row is there from the plan's start, with no count (the amount of work isn't known yet), and ends with the plan.
#[test]
fn the_row_shows_while_the_photos_are_planned() {
    let (mut s, root, ids) = library("prep-plan-row", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let rows = prep_rows(&s);
    assert_eq!(rows.len(), 1, "one row while the photos are planned: {rows:?}");
    assert_eq!(rows[0].label, "Preparing photos in the background");
    assert_eq!((rows[0].done, rows[0].total), (0, 0), "no count while planning");
    assert!(rows[0].cancellable, "it has its ✕");
    assert!(s.preview_build.is_none(), "no build yet");
    // (another view is shown: the plan ends with its row, the view shown now is planned again after three steps)
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[6] }).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(!s.prep_planning() && prep_rows(&s).is_empty(), "the plan's row ends with it: {:?}", prep_rows(&s));
    drop(hold);
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// The row of the plan is the row of the build that follows it: one row from the plan's start to the build's end, and
/// it counts from the build's start.
#[test]
fn the_row_of_a_plan_is_the_row_of_its_build() {
    let (mut s, root, ids) = library("prep-plan-row-build", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let planning = prep_rows(&s);
    assert_eq!(planning.len(), 1, "one row while the photos are planned: {planning:?}");
    let b = finish_planning(&mut s, &ids, mode);
    let building = prep_rows(&s);
    assert_eq!(building.len(), 1, "still one row once the build runs: {building:?}");
    assert_eq!(building[0].id, planning[0].id, "the plan's row carries on");
    assert_eq!(building[0].total, 7, "counting from the build's start");
    drop(hold);
    assert!(wait_finished(&b), "the build ends");
    for _ in 0..500 {
        if prep_rows(&s).is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(prep_rows(&s).is_empty(), "the row goes with the build");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// The ✕ of the plan's row ends the plan, and no build starts: that view isn't planned again (another one is), as when
/// the build is stopped from its row.
#[test]
fn stopping_the_row_while_planning_ends_the_plan() {
    let (mut s, root, ids) = library("prep-plan-row-stop", 7);
    let mode = crate::prep::Mode::Library;
    let hold = crate::prep::TestHold::new();
    start_planning(&mut s, &ids, mode);
    let row = prep_rows(&s).pop().expect("the plan's row");
    s.activity.cancel(row.id).expect("the row can be stopped");
    s.prep_tick(mode, Some(ids[2]), false);
    assert!(!s.prep_planning(), "nothing left to plan");
    assert!(prep_rows(&s).is_empty(), "the row is gone");
    for _ in 0..30 {
        s.prep_tick(mode, Some(ids[2]), false);
    }
    assert!(s.preview_build.is_none(), "no build for the view that was stopped");
    assert_eq!(s.prep.planned, 2, "and no planning of it");
    // another view is prepared, once the user has stepped through a few of its photos
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: ids[6] }).unwrap();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = finish_planning(&mut s, &ids, mode);
    assert_eq!(b.total, 6);
    drop(hold);
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A tick with the loupe open over a view that did not change neither copies the visible list nor
/// hashes it again: a frame asks every time, and a view of millions made every frame cost seconds.
#[test]
fn a_tick_over_an_unchanged_view_neither_copies_nor_hashes_it() {
    let (mut s, root, ids) = library("prep-signature", 4);
    let mode = crate::prep::Mode::Library;
    let copies = s.visible_copies();
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert_eq!(s.prep.signatures, 1, "the view is hashed once");
    for k in 0..20 {
        s.prep_tick(mode, Some(ids[k % 3]), false);
    }
    assert_eq!((s.prep.signatures, s.visible_copies() - copies), (1, 0), "20 more frames: the same view is not hashed or copied again");
    // an edit that leaves the same photos in view is one more look at it, and no other view
    s.execute("photo.rate", &json!({"ids": [ids[1].0], "rating": 3})).unwrap();
    s.prep_tick(mode, Some(ids[2]), false);
    assert_eq!(s.prep.signatures, 2);
    assert!(!b.cancel.load(Ordering::Relaxed), "the same view: its build goes on");
    drop(hold);
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_finished(&b), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}
