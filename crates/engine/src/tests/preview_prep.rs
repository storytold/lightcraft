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

/// Opening another library stops the build the preparation started for the old one and forgets
/// its views, even when the new library's view has the same signature (same photo ids and count:
/// CodeRabbit on #3080); the new library is prepared once the user steps through it.
#[test]
fn opening_another_library_stops_the_old_librarys_preparation() {
    let (mut s, root, ids) = library("prep-reopen", 4);
    let mode = crate::prep::Mode::Library;
    // the build's lanes wait at their foreground check until released below
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let old = s.preview_build.clone().expect("the view is prepared");
    assert!(old.running());
    let paths: Vec<String> = (0..4)
        .map(|i| {
            let p = root.join("other-shoot").join(format!("q{i:02}.png"));
            png(&p, (i * 30) as u8);
            p.to_string_lossy().to_string()
        })
        .collect();
    s.close_library().unwrap();
    s.open_library(root.join("other-lib"), false).unwrap();
    let stopped = old.cancel.load(Ordering::Relaxed);
    drop(hold);
    assert!(stopped, "the old library's build is stopped when another library opens");
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let mut other: Vec<(String, PhotoId)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.id)).collect();
    other.sort();
    let other: Vec<PhotoId> = other.into_iter().map(|(_, id)| id).collect();
    for k in 0..600 {
        s.prep_tick(mode, Some(other[k % 3]), false);
        if s.preview_build.as_ref().is_some_and(|p| !std::sync::Arc::ptr_eq(p, &old)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let next = s.preview_build.clone().expect("the new library is prepared");
    assert!(!std::sync::Arc::ptr_eq(&next, &old), "a build of its own");
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    for b in [&old, &next] {
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
