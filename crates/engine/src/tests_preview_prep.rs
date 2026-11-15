//! Preparing photos in the background ([`crate::prep`]): a build skips photos whose previews are
//! there and leaves what it built on disk, the background build follows the user, gives way to a
//! build that was asked for, to the photo on screen and to an export, is one cancellable row in the
//! activity stack, prepares only the photos its settings name, and photos whose settings changed
//! off screen are rendered again. The settings are app-wide or a library's own.

use std::path::Path;
use std::sync::atomic::Ordering;

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::prep::{Mode, Scope};

/// The test's folder in Temp, removed when the test ends.
fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-preview-prep-{tag}"))
}

/// A 600 × 400 PNG whose pixels differ for every `n` (no two are duplicates).
fn png_n(p: &Path, n: usize) {
    let img = lcc_raster::Rgba8::from_fn(600, 400, |x, y| [((x + n / 256) % 251) as u8, (y % 241) as u8, (n % 256) as u8, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

/// A library on disk with `n` photos (600 × 400 PNGs) imported, in name order.
/// (The folder comes first: bound as `let (root, s, ...)`, the session drops before the folder goes.)
pub(crate) fn library(tag: &str, n: usize) -> (lcc_testkit::TempDir, Session, Vec<PhotoId>) {
    let root = temp_dir(tag);
    let paths: Vec<String> = (0..n)
        .map(|i| {
            let p = root.join("shoot").join(format!("p{i:03}.png"));
            png_n(&p, i * 7 + 1);
            p.to_string_lossy().to_string()
        })
        .collect();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let mut ids: Vec<(String, PhotoId)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.id)).collect();
    ids.sort();
    (root, s, ids.into_iter().map(|(_, id)| id).collect())
}

fn ids_json(ids: &[PhotoId]) -> Vec<u64> {
    ids.iter().map(|i| i.0).collect()
}

/// Whether photo `id`'s standard preview (at `edge`) for its current settings is in the cache.
fn has_view(s: &mut Session, id: PhotoId, edge: usize) -> bool {
    let jobs = crate::cmd::previews::jobs_for(s, id, Some(edge));
    crate::cmd::previews::already_built(&jobs)
}

/// Wait (at most 30 s) until `b` is done.
fn wait_done(b: &crate::cmd::previews::PreviewBuild) -> bool {
    for _ in 0..1500 {
        if !b.running() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// A build skips photos whose previews are there (built before, or by the loupe), renders those
/// whose preview is too small, and its previews are on disk when it is done, for the next session.
#[test]
fn a_build_skips_what_is_there_and_leaves_it_on_disk() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("skip", 4);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!((r["done"].as_u64(), r["skipped"].as_u64(), r["failed"].as_u64()), (Some(4), Some(0), Some(0)), "{r}");
    for id in &ids {
        assert!(has_view(&mut s, *id, 300), "built");
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
    assert!(s.loupe_job(ids[1], 400, 400, true).unwrap().cached_view().is_some(), "the loupe uses the preview built in the last session");
    let _ = std::fs::remove_dir_all(root);
}

/// The background preparation builds standard previews at least as big as the loupe on this
/// screen asks for, so the loupe can show them.
#[test]
fn background_previews_are_as_big_as_the_loupe() {
    let _keys = crate::sensor_maps::keep_generation();
    let mut s = Session::with_demo();
    let standard = s.preview_prefs.standard_edge();
    assert_eq!(crate::cmd::previews::standard_edge(&s), standard);
    s.loupe_edge = standard + 900;
    assert_eq!(crate::cmd::previews::standard_edge(&s), standard + 900);
    s.loupe_edge = 100;
    assert_eq!(crate::cmd::previews::standard_edge(&s), standard);
}

/// A background build goes from where the user is, in the direction they step, and follows them
/// when they jump.
#[test]
fn the_background_build_starts_where_the_user_is() {
    use crate::cmd::previews::PreviewBuild;
    let _keys = crate::sensor_maps::keep_generation();
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
    b.focus_on(PhotoId(19), false);
    for _ in 0..6 {
        let i = b.next_index(&queue).unwrap();
        queue[i] = None;
    }
    assert!(b.next_index(&queue).is_none(), "all taken");
}

/// The background build waits while the photo on screen is being made, shows as one row in the
/// activity stack that can be cancelled, and gives way to a build that is asked for.
#[test]
fn the_background_build_waits_for_the_photo_on_screen_and_gives_way() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("replace", 3);
    // held back: the photo on screen is being made all along
    let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let h = hold.clone();
    let holder = std::thread::spawn(move || {
        while h.load(Ordering::Relaxed) {
            crate::prep::foreground();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "background": true})).unwrap();
    let bg = s.preview_build.clone().unwrap();
    assert!(bg.background && bg.running());
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(bg.done.load(Ordering::Relaxed), 0, "nothing built while the photo on screen is being made");
    let row = s.activity.list().into_iter().find(|t| t.label == "Preparing photos in the background").expect("its row");
    assert!(row.cancellable, "{row:?}");
    // a second background build is refused; one that is asked for replaces it
    assert!(s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "background": true})).is_err());
    hold.store(false, Ordering::Relaxed);
    holder.join().unwrap();
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "wait": true})).unwrap();
    assert_eq!(r["done"], 3, "{r}");
    assert!(bg.cancel.load(Ordering::Relaxed), "the background build was stopped");
    assert!(wait_done(&bg));
    // its row's ✕ stops it: the same flag
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 400, "background": true})).unwrap();
    let bg = s.preview_build.clone().unwrap();
    let row = s.activity.list().into_iter().find(|t| t.label == "Preparing photos in the background").expect("its row");
    assert!(s.activity.cancel(row.id).is_ok(), "the row can be stopped");
    assert!(bg.cancel.load(Ordering::Relaxed));
    assert!(wait_done(&bg));
    let _ = std::fs::remove_dir_all(root);
}

/// The background preparation starts once the user steps through photos in the loupe (not when a
/// library opens), builds the view's previews, and is not started again for the same view.
#[test]
fn the_background_preparation_starts_when_culling() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("prep", 4);
    let mode = Mode::Library;
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
    assert!(s.preview_build.as_ref().is_some_and(|p| std::sync::Arc::ptr_eq(p, &b)), "not started again for the same view");
    let _ = std::fs::remove_dir_all(root);
}

/// "Which photos: Picks, rated and labelled" prepares only those; unflagged photos are left alone.
/// "Only the photo on screen" prepares nothing in the background.
#[test]
fn picks_only_leaves_the_unflagged_photos_alone() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("marked", 6);
    s.selection = crate::Selection { ids: ids.clone(), active: Some(ids[0]) };
    s.execute("photo.flag", &json!({"ids": [ids[1].0], "flag": "pick"})).unwrap();
    s.execute("photo.rate", &json!({"ids": [ids[4].0], "rating": 3})).unwrap();
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"scope": "marked"}}})).unwrap();
    for at in [ids[0], ids[1], ids[2]] {
        s.prep_tick(Mode::Library, Some(at), false);
    }
    let b = s.preview_build.clone().expect("prepared");
    let mut order = b.order.clone();
    order.sort();
    assert_eq!(order, vec![ids[1].0, ids[4].0], "only the pick and the rated photo");
    assert!(wait_done(&b));
    // only the photo on screen: nothing in the background, nothing ahead
    let (root2, mut s, ids) = library("onscreen", 4);
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"scope": "onScreen"}}})).unwrap();
    for at in [ids[0], ids[1], ids[2], ids[3]] {
        s.prep_tick(Mode::Library, Some(at), false);
    }
    assert!(s.preview_build.is_none());
    assert_eq!(s.prep_settings().ahead(Mode::Library), 0);
    assert_eq!(s.prep_settings().eye_ahead(Mode::Library), 0);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

/// With "Picks, rated and labelled" the build holds only the marked photos; the user on an
/// unmarked one is still in the view it was started for, so the build keeps going. It was stopped
/// as if the user had moved on, started again for the same photos and stopped again a frame later,
/// for as long as the user stayed: "Preparing photos in the background ▸ Stopping…" for good, and
/// the loupe and the filmstrip never rendered (26.46.1).
#[test]
fn a_photo_the_build_leaves_out_does_not_restart_it_every_frame() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("marked-loop", 6);
    s.selection = crate::Selection { ids: ids.clone(), active: Some(ids[0]) };
    s.execute("photo.flag", &json!({"ids": [ids[1].0, ids[4].0], "flag": "pick"})).unwrap();
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"scope": "marked"}}})).unwrap();
    // the photo on screen is being made all along, so the build is still running while the user stays
    let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let h = hold.clone();
    let holder = std::thread::spawn(move || {
        while h.load(Ordering::Relaxed) {
            crate::prep::foreground();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    for at in [ids[0], ids[1], ids[2]] {
        s.prep_tick(Mode::Library, Some(at), false);
    }
    let b = s.preview_build.clone().expect("prepared");
    assert!(!b.order.contains(&ids[2].0), "an unmarked photo is not built: {:?}", b.order);
    for _ in 0..20 {
        s.prep_tick(Mode::Library, Some(ids[2]), false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!b.cancel.load(Ordering::Relaxed), "the build was stopped with the user still in its view");
    assert!(s.preview_build.as_ref().is_some_and(|p| std::sync::Arc::ptr_eq(p, &b)), "no other build replaced it");
    hold.store(false, Ordering::Relaxed);
    holder.join().unwrap();
    assert!(wait_done(&b));
    let _ = std::fs::remove_dir_all(root);
}

/// The settings round-trip: app-wide ones in the app's shared settings, and a library's own (a
/// timelapse library prepares nothing while a wedding library prepares everything) saved with it;
/// a reset takes the library back to the app-wide ones.
#[test]
fn the_settings_round_trip_app_wide_and_per_library() {
    let _keys = crate::sensor_maps::keep_generation();
    let root = temp_dir("own");
    let store = root.join("store");
    let mut s2 = Session::new().with_fs().with_app_store(&store);
    s2.open_library(root.join("lib"), false).unwrap();
    // app-wide
    let r = s2.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"ahead": 10, "eyeAhead": 3}}})).unwrap();
    assert_eq!(r["own"], false, "{r}");
    assert_eq!(r["settings"]["library"]["ahead"], 10);
    // this library's own
    let r = s2.execute("prefs.setBackgroundPrep", &json!({"library": true, "settings": {"culling": {"previews": false, "ahead": 0}}})).unwrap();
    assert_eq!(r["own"], true, "{r}");
    assert_eq!(r["settings"]["culling"]["ahead"], 0);
    assert_eq!(r["settings"]["library"]["ahead"], 10, "the library's own start from the app-wide ones");
    assert_eq!(r["app"]["culling"]["ahead"], 5, "the app-wide ones are unchanged");
    // out of range and malformed values
    let r = s2.execute("prefs.setBackgroundPrep", &json!({"library": true, "settings": {"culling": {"eyeAhead": 200}}})).unwrap();
    assert_eq!(r["settings"]["culling"]["eyeAhead"], crate::prep::MAX_AHEAD);
    assert!(s2.execute("prefs.setBackgroundPrep", &json!({"settings": 3})).is_err());
    assert!(s2.execute("prefs.setBackgroundPrep", &json!({"settings": {"library": {"scope": "sometimes"}}})).is_err());
    drop(s2);
    // both come back: the library's own from its prefs, the app-wide ones from the store
    let mut s = Session::new().with_fs().with_app_store(&store);
    s.open_library(root.join("lib"), false).unwrap();
    assert!(s.prep.library.is_some(), "kept with the library");
    assert_eq!(s.prep_settings().culling.ahead, 0);
    assert!(!s.prep_settings().culling.previews);
    assert_eq!(s.prep_settings().ahead(Mode::Culling), 0);
    assert_eq!(s.prep.app.library.ahead, 10, "the app-wide ones kept in the store");
    assert_eq!(s.prep.app.library.eye_ahead, 3);
    assert!(s.execute("prefs.setBackgroundPrep", &json!({"reset": true, "settings": {"culling": {"ahead": 3}}})).is_err());
    assert_eq!(s.prep_settings().culling.ahead, 0, "reset with valid settings keeps the library setting");
    assert!(s.execute("prefs.setBackgroundPrep", &json!({"reset": true, "settings": 3})).is_err());
    assert_eq!(s.prep_settings().culling.ahead, 0, "invalid reset keeps the library setting");
    s.execute("prefs.setBackgroundPrep", &json!({"reset": true})).unwrap();
    assert!(s.prep.library.is_none());
    assert_eq!(s.prep_settings().culling.ahead, 5, "back to the app-wide settings");
    assert_eq!(s.prep_settings().library.ahead, 10);
    assert_eq!(s.prep_settings().scope(Mode::Library), Scope::All);
    let _ = std::fs::remove_dir_all(root);
}

/// `library` and `reset` that aren't true or false, and an unknown setting, are errors that
/// leave the app-wide and the library's own settings as they were.
#[test]
fn background_prep_flags_must_be_booleans() {
    let _keys = crate::sensor_maps::keep_generation();
    let root = temp_dir("flags");
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"culling": {"ahead": 2}}})).unwrap();
    s.execute("prefs.setBackgroundPrep", &json!({"library": true, "settings": {"culling": {"ahead": 1}}})).unwrap();
    let before = s.execute("prefs.backgroundPrep", &json!({})).unwrap();
    for p in [
        json!({"library": "yes", "settings": {"culling": {"ahead": 4}}}),
        json!({"library": 1, "settings": {"culling": {"ahead": 4}}}),
        json!({"reset": "true"}),
        json!({"reset": 1}),
        json!({"settings": {"culling": {"ahead": 4}, "unknown": true}}),
        json!({"library": true, "settings": {"develop": {"ahead": 0, "unknown": true}}}),
    ] {
        assert!(s.execute("prefs.setBackgroundPrep", &p).is_err(), "{p}");
        let after = s.execute("prefs.backgroundPrep", &json!({})).unwrap();
        assert_eq!(after["app"], before["app"], "{p} keeps the app-wide settings");
        assert_eq!(after["library"], before["library"], "{p} keeps the library's own settings");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Resetting a library's settings with no library open is an error and leaves the app-wide
/// settings as they were.
#[test]
fn background_prep_reset_needs_an_open_library() {
    let _keys = crate::sensor_maps::keep_generation();
    let mut s = Session::new();
    s.execute("prefs.setBackgroundPrep", &json!({"settings": {"culling": {"ahead": 2}}})).unwrap();
    assert!(s.execute("prefs.setBackgroundPrep", &json!({"reset": true})).is_err());
    let r = s.execute("prefs.backgroundPrep", &json!({})).unwrap();
    assert_eq!(r["app"]["culling"]["ahead"], 2, "{r}");
    assert_eq!(r["own"], false, "{r}");
}

/// Photos whose settings changed while they were not on screen (Auto Sync, Sync, paste, presets)
/// are rendered again in the background once the edits settle; the photo on screen is left to the
/// loupe.
#[test]
fn photos_changed_off_screen_are_rendered_again_in_the_background() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("resync", 5);
    let edge = crate::cmd::previews::standard_edge(&s);
    for at in [ids[0], ids[1], ids[0]] {
        s.prep_tick(Mode::Library, Some(at), false);
    }
    let first = s.preview_build.clone().expect("the view is prepared");
    for _ in 0..1500 {
        s.prep_tick(Mode::Library, Some(ids[0]), false);
        if !first.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(has_view(&mut s, ids[3], edge));
    // synced from photo 0: photos 2 and 3 (and 0 itself, on screen) change
    for id in [ids[0], ids[2], ids[3]] {
        let mut d = (*s.catalog.photo(id).unwrap().develop).clone();
        d.light.exposure = 0.8;
        s.set_develop(id, d, "Sync").unwrap();
    }
    assert!(!has_view(&mut s, ids[2], edge) && !has_view(&mut s, ids[3], edge));
    let mut again = None;
    for _ in 0..1500 {
        s.prep_tick(Mode::Library, Some(ids[0]), false);
        if let Some(b) = s.preview_build.clone().filter(|b| !std::sync::Arc::ptr_eq(b, &first) && !b.running()) {
            again = Some(b);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let b = again.expect("a build for the changed photos ran");
    assert!(b.background);
    assert_eq!(b.total, 2, "only the photos changed off screen");
    assert!(has_view(&mut s, ids[2], edge) && has_view(&mut s, ids[3], edge), "their previews are made for the new settings");
    let _ = std::fs::remove_dir_all(root);
}

/// While the user's own work runs (an export), the background build pauses completely and its row
/// says so; it carries on once the export ends.
#[test]
fn the_background_build_pauses_while_an_export_runs() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("pause", 3);
    let export = s.activity.start_batch("export", "Exporting", crate::activity::Cancel::Yes);
    assert_eq!(s.activity.user_work(), Some("export"));
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "background": true})).unwrap();
    let b = s.preview_build.clone().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(b.done.load(Ordering::Relaxed), 0, "nothing prepared while the export runs");
    let row = s.activity.list().into_iter().find(|t| t.label == "Preparing photos in the background").expect("its row");
    assert_eq!(row.detail, "Paused while exporting");
    drop(export);
    assert_eq!(s.activity.user_work(), None);
    assert!(wait_done(&b));
    assert_eq!(b.done.load(Ordering::Relaxed), 3, "carried on once the export ended");
    let _ = std::fs::remove_dir_all(root);
}

/// In a library the background build prepares the photos around the one on screen, not the whole
/// (already edited) gallery.
#[test]
fn the_background_build_prepares_around_the_photo_on_screen() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("around", 130);
    let at = ids[100];
    for a in [ids[98], ids[99], at] {
        s.prep_tick(Mode::Library, Some(a), false);
    }
    let b = s.preview_build.clone().expect("prepared");
    b.cancel.store(true, Ordering::Relaxed);
    assert!(b.total <= 3 * crate::prep::AROUND && b.total < ids.len(), "{} of {}", b.total, ids.len());
    assert!(b.order.contains(&at.0) && b.order.contains(&ids[129].0) && !b.order.contains(&ids[0].0));
    assert!(wait_done(&b));
    let _ = std::fs::remove_dir_all(root);
}

/// Two folders of `n` photos each (600 × 400 PNGs), not imported: a library on disk to browse them in.
fn two_folders(tag: &str, n: usize) -> (lcc_testkit::TempDir, Session, [String; 2]) {
    let root = temp_dir(tag);
    let dirs = ["first", "second"].map(|d| root.join(d));
    for (k, dir) in dirs.iter().enumerate() {
        for i in 0..n {
            png_n(&dir.join(format!("p{i:03}.png")), k * 100 + i * 7 + 1);
        }
    }
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    (root, s, dirs.map(|d| d.to_string_lossy().to_string()))
}

/// The ids of the photos in view, in order.
fn in_view(s: &mut Session) -> Vec<PhotoId> {
    s.visible_cloned()
}

/// A folder browsed after another is prepared, not the folder left behind: the build of the first
/// folder stops and one of the second starts (2026-11-09: a wedding browsed after a calibration
/// folder kept "Preparing photos in the background" on the calibration folder's 323 photos, and
/// nothing started for the wedding when it ended).
#[test]
fn browsing_another_folder_prepares_that_folder() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, [first, second]) = two_folders("refolder", 4);
    // held back all along, so the first folder's build is still running when the user moves on
    let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let h = hold.clone();
    let holder = std::thread::spawn(move || {
        while h.load(Ordering::Relaxed) {
            crate::prep::foreground();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    s.execute("library.browse", &json!({"path": first})).unwrap();
    let a = in_view(&mut s);
    assert_eq!(Mode::of(&s), Mode::Culling);
    s.prep_tick(Mode::Culling, Some(a[0]), false);
    let first_build = s.preview_build.clone().expect("the first folder is prepared");
    assert!(first_build.order.contains(&a[0].0));
    s.execute("library.browse", &json!({"path": second})).unwrap();
    let b = in_view(&mut s);
    assert!(b.iter().all(|id| !a.contains(id)), "another folder's photos");
    let mut second_build = None;
    for _ in 0..500 {
        s.prep_tick(Mode::Culling, Some(b[0]), false);
        if let Some(p) = s.preview_build.clone().filter(|p| !std::sync::Arc::ptr_eq(p, &first_build)) {
            second_build = Some(p);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(first_build.cancel.load(Ordering::Relaxed), "the folder left behind is no longer prepared");
    let second_build = second_build.expect("the folder in view is prepared");
    assert!(second_build.background);
    assert_eq!(second_build.total, 4);
    assert!(b.iter().all(|id| second_build.order.contains(&id.0)), "{:?}", second_build.order);
    hold.store(false, Ordering::Relaxed);
    holder.join().unwrap();
    assert!(wait_done(&first_build) && wait_done(&second_build));
    let _ = std::fs::remove_dir_all(root);
}

/// A folder browsed to be culled is prepared as soon as one of its photos is looked at, in the
/// loupe or the grid: the user waited on photo 1 for minutes and nothing was prepared (a library
/// still waits for a few steps, [`crate::prep::STEPS_BEFORE_BUILD`]).
#[test]
fn a_folder_being_culled_is_prepared_from_the_first_photo() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, [first, _]) = two_folders("first-photo", 3);
    s.execute("library.browse", &json!({"path": first})).unwrap();
    let a = in_view(&mut s);
    s.prep_tick(Mode::Culling, Some(a[0]), false);
    let b = s.preview_build.clone().expect("prepared from the first photo in the loupe");
    assert_eq!(b.total, 3);
    assert!(wait_done(&b));
    let (root2, mut s2, [first2, _]) = two_folders("first-photo-grid", 3);
    s2.execute("library.browse", &json!({"path": first2})).unwrap();
    let a2 = in_view(&mut s2);
    s2.execute("library.select", &json!({"ids": [a2[1].0]})).unwrap();
    s2.prep_tick(Mode::Culling, None, false);
    let b2 = s2.preview_build.clone().expect("prepared from the photo selected in the grid");
    assert_eq!(b2.order.get(b2.focus.load(Ordering::Relaxed)), Some(&a2[1].0), "built from the selected photo: {:?}", b2.order);
    assert!(wait_done(&b2));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

/// Stepping through a folder keeps the photo on screen being made all the time: the background
/// build still gets ahead, one photo at a time, instead of waiting for good (2026-11-09: 16 photos
/// in 70 s of culling a wedding).
#[test]
fn the_background_build_keeps_going_while_the_user_steps() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, mut s, ids) = library("steady", 2);
    let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let h = hold.clone();
    let holder = std::thread::spawn(move || {
        while h.load(Ordering::Relaxed) {
            crate::prep::foreground();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "background": true})).unwrap();
    let b = s.preview_build.clone().unwrap();
    let done = wait_done(&b);
    hold.store(false, Ordering::Relaxed);
    holder.join().unwrap();
    assert!(done, "built while the photo on screen was being made all along");
    assert_eq!(b.done.load(Ordering::Relaxed), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// The background build's decodes are background work: at the memory gate they wait behind the
/// photo on screen's instead of making it wait (left out of the port; with a lane that keeps
/// building while the user steps, the photo on screen never finished in a 1.5 s look).
#[test]
fn the_background_builds_decodes_are_background_work() {
    let _keys = crate::sensor_maps::keep_generation();
    let (root, s, ids) = library("bg-decode", 1);
    // a session that has not decoded the photo yet (the import's decode is kept in memory)
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
    let log = seen.clone();
    // (every decode through this loader: no reduced-size or embedded-preview shortcut)
    s.media.reduced_loader = None;
    s.media.preview_loader = None;
    s.media.file_loader = Some(std::sync::Arc::new(move |_, _, _| {
        log.lock().unwrap().push(crate::memory::is_background());
        let mut image = lcc_raster::Rgb32f::new(600, 400);
        image.data.fill([0.3, 0.2, 0.1]);
        Ok((image, Default::default()))
    }));
    s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids), "edge": 300, "background": true})).unwrap();
    let b = s.preview_build.clone().unwrap();
    assert!(wait_done(&b));
    let seen = seen.lock().unwrap().clone();
    assert!(
        !seen.is_empty(),
        "the photo was decoded: done {} skipped {} failed {}",
        b.done.load(Ordering::Relaxed),
        b.skipped.load(Ordering::Relaxed),
        b.failed.load(Ordering::Relaxed)
    );
    assert!(seen.iter().all(|bg| *bg), "every decode of the build is background work: {seen:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// The background preparation starts one past the photo on screen, which the loupe renders
/// itself: building it as well decoded the same raw twice while the user stepped. It is built last
/// on that side, never left out.
#[test]
fn the_background_build_leaves_the_photo_on_screen_to_the_loupe() {
    use crate::cmd::previews::PreviewBuild;
    let _keys = crate::sensor_maps::keep_generation();
    let b = PreviewBuild { order: (10..20).collect(), background: true, ..Default::default() };
    let mut queue: Vec<Option<u64>> = (10..20).map(Some).collect();
    b.focus_on(PhotoId(14), false);
    let mut took = Vec::new();
    while let Some(i) = b.next_index(&queue) {
        took.push(queue[i].take().unwrap());
    }
    assert_eq!(took, vec![15, 16, 17, 18, 19, 14, 13, 12, 11, 10], "ahead first, then the photo on screen, then behind");
    let mut queue: Vec<Option<u64>> = (10..20).map(Some).collect();
    b.focus_on(PhotoId(14), true);
    let mut took = Vec::new();
    while let Some(i) = b.next_index(&queue) {
        took.push(queue[i].take().unwrap());
    }
    assert_eq!(took, vec![13, 12, 11, 10, 14, 15, 16, 17, 18, 19], "stepping back: the same, backwards");
    // at either end of the folder
    let mut queue: Vec<Option<u64>> = (10..20).map(Some).collect();
    b.focus_on(PhotoId(10), true);
    assert_eq!(b.next_index(&queue).map(|i| queue[i]), Some(Some(10)));
    b.focus_on(PhotoId(19), false);
    let i = b.next_index(&queue).unwrap();
    assert_eq!(queue[i].take(), Some(19));
}

fn same_build(s: &Session, b: &std::sync::Arc<crate::cmd::previews::PreviewBuild>) -> bool {
    s.preview_build.as_ref().is_some_and(|p| std::sync::Arc::ptr_eq(p, b))
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
        s.prep_tick(Mode::Library, None, false);
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
    let (root, mut s, ids) = library("prep-resume", 6);
    let mode = Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(b.running() && s.prep.unfinished, "a build that started is unfinished until it has run to its end");
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: ids[5] }).unwrap();
    s.prep_tick(mode, None, false);
    assert!(b.cancel.load(Ordering::Relaxed), "the old view's build is stopped, from the grid too");
    drop(hold);
    assert!(wait_done(&b), "a cancelled build stops");
    // the grid only: no photo is opened
    let next = build_after(&mut s, &b).expect("a build for the view now shown, started from the grid");
    assert!(next.background);
    assert_eq!(next.total, 5);
    next.cancel.store(true, Ordering::Relaxed);
    // its workers stop before their library is removed under them
    assert!(wait_done(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947: the app quit while the background build ran: the library says so in its `prefs.json`, and
/// the next session takes the build up again from the grid. Once it has run to its end nothing is
/// unfinished, and a third session starts nothing.
#[test]
fn an_unfinished_build_is_taken_up_again_after_a_restart() {
    let (root, mut s, ids) = library("prep-restart", 4);
    let mode = Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(marked_unfinished(&root), "saved when the build starts");
    // the app quits: the session never sees its build end (the workers are stopped here only
    // because the test's process lives on)
    b.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&b));
    drop(s);
    drop(hold);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    assert!(s.prep.unfinished, "read with the library");
    s.prep_tick(mode, None, false);
    let next = s.preview_build.clone().expect("taken up again from the grid");
    assert!(next.background);
    assert_eq!(next.total, 4);
    assert!(wait_done(&next), "built");
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
/// view render.
#[test]
fn a_build_taken_up_again_builds_only_what_is_missing() {
    let (root, mut s, ids) = library("prep-missing", 6);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids_json(&ids[..2]), "wait": true})).unwrap();
    assert_eq!((r["done"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(0)), "{r}");
    let asked = s.preview_build.clone().expect("the build asked for");
    // (as left by a build that didn't run to its end)
    s.prep.unfinished = true;
    let b = build_after(&mut s, &asked).expect("taken up again from the grid");
    assert!(b.background);
    assert!(wait_done(&b), "built");
    let counts = (b.total, b.done.load(Ordering::Relaxed), b.skipped.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed));
    assert_eq!(counts, (6, 6, 2, 0), "the two built before are skipped, the others built");
    let edge = crate::cmd::previews::standard_edge(&s);
    for id in &ids {
        assert!(has_view(&mut s, *id, edge), "{id:?}: its preview is on disk");
    }
    s.prep_tick(Mode::Library, None, false);
    assert!(!s.prep.unfinished, "run to its end");
    let _ = std::fs::remove_dir_all(root);
}

/// Note 36 stands with #3947: a library in which no background build was left unfinished starts
/// nothing from the grid, nor after fewer than three photos in the loupe.
#[test]
fn a_library_with_nothing_unfinished_starts_nothing_from_the_grid() {
    let (root, mut s, ids) = library("prep-fresh", 4);
    let mode = Mode::Library;
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
    assert_eq!(s.prep_wake_in(), None, "no frame is asked for");
    let _ = std::fs::remove_dir_all(root);
}

/// #3947: previews switched off and on again while the build ran is not the user stopping the
/// build: the view is prepared again (it was taken as stopped from the row, and not prepared
/// again in that session).
#[test]
fn switching_previews_off_and_on_is_not_a_stop_by_the_user() {
    let (root, mut s, ids) = library("prep-toggle", 4);
    let mode = Mode::Library;
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
    assert!(wait_done(&b), "a cancelled build stops");
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
    assert!(wait_done(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// #3947 keeps note 48's stop: a build stopped from its row is not started again for that view,
/// from the grid or the loupe, and is not unfinished: a later session doesn't take it up either.
#[test]
fn a_build_stopped_from_its_row_is_not_taken_up_again() {
    let (root, mut s, ids) = library("prep-stopped", 4);
    let mode = Mode::Library;
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the view is prepared");
    assert!(marked_unfinished(&root));
    // (what the row's ✕ does)
    s.execute("library.cancelPreviews", &json!({})).unwrap();
    drop(hold);
    assert!(wait_done(&b), "a cancelled build stops");
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
    let (root, mut s, ids) = library("prep-settle", 7);
    let mode = Mode::Library;
    // (a wait that ends only when the test says so)
    let long = std::time::Duration::from_secs(3600);
    s.prep.settle = Some(long);
    let hold = crate::prep::TestHold::new();
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("a view's first build doesn't wait: the steps are its guard");
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: ids[6] }).unwrap();
    s.prep_tick(mode, None, false);
    drop(hold);
    assert!(wait_done(&b), "the old view's build stops");
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &b), "the view just changed: not yet");
    assert!(s.prep_wake_in().is_some_and(|d| d <= long && d > long / 2), "a frame is asked for when the wait ends");
    // the wait is over, but the view has changed again: it starts anew
    s.prep.not_before = Some(web_time::Instant::now());
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: ids[5] }).unwrap();
    s.prep_tick(mode, None, false);
    s.prep_tick(mode, None, false);
    assert!(same_build(&s, &b), "changed again: still no build");
    assert!(s.prep_wake_in().is_some_and(|d| d > long / 2), "the wait started anew");
    // the view has been still for long enough
    s.prep.not_before = Some(web_time::Instant::now());
    assert_eq!(s.prep_wake_in(), None, "a wait that is over asks for no frame");
    s.prep_tick(mode, None, false);
    let next = s.preview_build.clone().filter(|p| !std::sync::Arc::ptr_eq(p, &b)).expect("one build, for the view as it is now");
    assert!(next.background);
    assert_eq!(next.total, 5);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
}

/// CodeRabbit on #4093: a view signature is only its photos' ids and their count, so two
/// libraries can carry the same one. The view guards, waits, and step count the first library
/// earned must not follow the session into the second: a build that ran to its end there
/// leaves `started_for` set, and the second library's owed build would never start.
#[test]
fn an_unfinished_build_is_taken_up_when_its_library_opens_after_another() {
    let mode = Mode::Library;
    // the second library, left by a session whose build did not run to its end
    let (root2, mut s2, ids2) = library("prep-switch-second", 4);
    let hold = crate::prep::TestHold::new();
    for id in &ids2[..3] {
        s2.prep_tick(mode, Some(*id), false);
    }
    let b2 = s2.preview_build.clone().expect("the second library's build started");
    assert!(marked_unfinished(&root2), "its build is owed");
    b2.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&b2), "a cancelled build stops");
    drop(s2);
    drop(hold);
    // the first library, in the session that switches: its build runs to its end
    let (root, mut s, ids) = library("prep-switch-first", 4);
    for id in &ids[..3] {
        s.prep_tick(mode, Some(*id), false);
    }
    let b = s.preview_build.clone().expect("the first library's view is prepared");
    assert!(wait_done(&b), "built");
    s.prep_tick(mode, None, false);
    assert!(!s.prep.unfinished, "run to its end: nothing is owed here");
    // the switch: the second library is owed a build for the same view signature
    s.open_library(root2.join("lib"), false).unwrap();
    assert!(s.prep.unfinished, "read with the second library");
    let next = build_after(&mut s, &b).expect("the second library's owed build is taken up, not blocked by the first library's guards");
    assert!(next.background);
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&next), "a cancelled build stops");
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
    let mode = Mode::Library;
    // the second library, left by a session whose build did not run to its end
    let (root2, mut s2, ids2) = library("prep-switch-running-second", 4);
    let hold = crate::prep::TestHold::new();
    for id in &ids2[..3] {
        s2.prep_tick(mode, Some(*id), false);
    }
    let b2 = s2.preview_build.clone().expect("the second library's build started");
    assert!(marked_unfinished(&root2), "its build is owed");
    b2.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&b2), "a cancelled build stops");
    drop(s2);
    drop(hold);
    // the first library, in the session that switches: its build is still running
    let (root, mut s, ids) = library("prep-switch-running-first", 4);
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
    assert!(wait_done(&b), "a cancelled build stops");
    let next = build_after(&mut s, &b).expect("the second library's owed build is taken up once the old build stops");
    assert!(next.background);
    assert_eq!(next.total, 4);
    next.cancel.store(true, Ordering::Relaxed);
    assert!(wait_done(&next), "a cancelled build stops");
    drop(s);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&root2).unwrap();
}

/// The background build of a folder makes its photos' render jobs a few a tick, nearest the user
/// first, not all of them when it starts: that froze the UI for 1.3 s at every build of a
/// 2,597-photo folder (2026-11-10). It still builds every photo.
#[test]
fn the_background_build_makes_its_jobs_a_few_at_a_time() {
    let (root, mut s, ids) = library("feed", 24);
    let p = json!({"ids": ids_json(&ids), "background": true, "from": ids[12].0, "feed": true});
    s.execute("library.buildPreviews", &p).unwrap();
    let b = s.preview_build.clone().unwrap();
    let unmade = |b: &crate::cmd::previews::PreviewBuild| b.queue_unmade();
    assert!(unmade(&b) >= 24 - 8, "only the first photos' jobs are made when it starts ({} unmade)", unmade(&b));
    assert!(!b.queue_unmade_at(13), "the first made is the one after the user's");
    for _ in 0..1500 {
        crate::cmd::previews::feed(&mut s);
        if !b.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!b.running(), "built");
    assert_eq!(b.done.load(Ordering::Relaxed), 24);
    assert_eq!(b.failed.load(Ordering::Relaxed), 0);
    let edge = crate::cmd::previews::standard_edge(&s);
    assert!(ids.iter().all(|id| has_view(&mut s, *id, edge)), "every photo built");
    let _ = std::fs::remove_dir_all(root);
}
