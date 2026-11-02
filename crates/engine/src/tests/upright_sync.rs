//! LR-EDIT-GEOM-UPRIGHT per photo: Upright is an analysis of each photo's own lines, like Auto
//! white balance. An automatic mode that Auto Sync, Sync Settings, Paste or a preset carries to
//! other photos is worked out on each of them (in the background in the app, one activity row);
//! a second click on the mode that is on cancels what is running and starts over; Guided Upright,
//! drawn on one photo's lines, is not synced.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;
use lightcraft_develop::Upright;
use lightcraft_geom::Homography;
use serde_json::json;

use crate::Session;
use crate::media::SourceLevel;

/// White posts on a dark wall, the camera rolled by `roll` degrees, as photo `id`'s source.
fn posts(s: &mut Session, id: PhotoId, roll: f64) {
    let (sn, cs) = roll.to_radians().sin_cos();
    let img = lightcraft_raster::Rgb32f::from_fn(600, 400, |x, y| {
        let (dx, dy) = (x as f64 - 300.0, y as f64 - 200.0);
        let u = dx * cs + dy * sn;
        if (u + 1000.0).rem_euclid(110.0) < 22.0 { [0.9; 3] } else { [0.08; 3] }
    });
    let img = Arc::new(img);
    s.media.insert_with_info(id, SourceLevel::Thumb, img.clone(), Default::default());
    s.media.insert_with_info(id, SourceLevel::Preview, img, Default::default());
}

/// Three demo photos with posts rolled differently, all selected (the first active), Auto Sync on.
fn three(background: bool) -> (Session, [PhotoId; 3]) {
    let mut s = Session::with_demo();
    s.upright_jobs.background = background;
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    for (id, roll) in [(a, 2.0), (b, -3.0), (c, 1.0)] {
        posts(&mut s, id, roll);
    }
    s.execute("library.select", &json!({"ids": [a.0, b.0, c.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    (s, [a, b, c])
}

fn transform(s: &Session, id: PhotoId) -> Option<Homography> {
    s.develop_of(id).unwrap().geometry.upright_transform
}

fn rows(s: &Session) -> Vec<crate::activity::TaskInfo> {
    s.activity.list().into_iter().filter(|t| t.kind == "upright").collect()
}

/// Poll until no photo waits for its analysis.
fn finish(s: &mut Session) {
    let t0 = Instant::now();
    while s.upright_jobs.pending() > 0 {
        s.upright_poll();
        assert!(t0.elapsed() < Duration::from_secs(60), "the analyses finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Auto Sync carries the mode; each photo is analysed on its own lines, off the UI thread under one
/// cancellable activity row, the results merged as they arrive; one undo step for all.
#[test]
fn auto_sync_works_out_upright_on_each_photo_in_the_background() {
    let (mut s, [a, b, c]) = three(true);
    let undo = s.undo.len();
    s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    let ta = transform(&s, a).expect("the active photo is analysed at once");
    for id in [b, c] {
        assert_eq!(s.develop_of(id).unwrap().geometry.upright, Upright::Vertical, "the mode travels");
        // (the worker may be quick, but its results only land when polled)
        assert_ne!(transform(&s, id), Some(ta), "never the active photo's correction");
    }
    assert_eq!(s.upright_jobs.pending(), 2);
    let row = rows(&s);
    assert_eq!(row.len(), 1, "one row for the batch: {row:?}");
    assert_eq!((row[0].label.as_str(), row[0].total, row[0].cancellable), ("Upright", 2, true));
    finish(&mut s);
    assert!(rows(&s).is_empty(), "the row goes when done");
    let (tb, tc) = (transform(&s, b).unwrap(), transform(&s, c).unwrap());
    assert_eq!(tb, s.upright_for(b, Upright::Vertical).unwrap(), "b's own correction");
    assert_eq!(tc, s.upright_for(c, Upright::Vertical).unwrap(), "c's own correction");
    assert!(ta != tb && tb != tc && ta != tc, "each photo its own");
    assert_eq!(s.undo.len(), undo + 1, "the results are no undo step of their own");
    s.execute("edit.undo", &json!({})).unwrap();
    for id in [a, b, c] {
        assert_eq!(s.develop_of(id).unwrap().geometry.upright, Upright::Off, "one undo takes all back");
    }
}

/// Paste, Sync Settings and presets carry Upright as a recipe too: the photo it lands on gets its
/// own correction, never the copied photo's; without a worker (CLI, MCP) at once.
#[test]
fn pasted_synced_and_preset_upright_is_worked_out_on_the_photo_it_lands_on() {
    let (mut s, [a, b, c]) = three(false);
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
    s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
    let ta = transform(&s, a);
    s.execute("develop.copy", &json!({"groups": ["geometry"]})).unwrap();
    s.execute("library.select", &json!({"ids": [b.0], "active": b.0})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    assert_eq!(s.develop_of(b).unwrap().geometry.upright, Upright::Vertical);
    assert_eq!(transform(&s, b), Some(s.upright_for(b, Upright::Vertical).unwrap()), "paste: b's own correction");
    assert_ne!(transform(&s, b), ta, "not a's");
    // Sync Settings
    s.execute("library.select", &json!({"ids": [a.0, c.0], "active": a.0})).unwrap();
    s.execute("develop.sync", &json!({"groups": ["geometry"]})).unwrap();
    assert_eq!(transform(&s, c), Some(s.upright_for(c, Upright::Vertical).unwrap()), "sync: c's own correction");
    // a preset saved from a carries the mode, not a's correction
    let preset =
        lightcraft_develop::Preset::from_settings("p", "P", "User", &s.develop_of(a).unwrap(), &[lightcraft_develop::SettingsGroup::Geometry]);
    assert!(!preset.settings["geometry"]["upright_transform"].is_null(), "the preset file keeps what it was saved with");
    let applied = preset.apply(&lightcraft_develop::DevelopSettings::default(), 1.0);
    assert_eq!((applied.geometry.upright, applied.geometry.upright_transform), (Upright::Vertical, None), "worked out where it lands");
}

/// Guided Upright is drawn on one photo's lines, so it isn't synced: Auto Sync and Paste leave the
/// other photos with the Upright they have (mode, correction, guides); the Transform sliders still
/// travel.
#[test]
fn guided_upright_is_not_synced_the_other_photos_keep_their_own() {
    let (mut s, [a, b, c]) = three(false);
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    s.execute("library.select", &json!({"ids": [b.0], "active": b.0})).unwrap();
    s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
    let gb = s.develop_of(b).unwrap().geometry.clone();
    assert!(gb.upright_transform.is_some());
    // Auto Sync: Guided on a; b keeps Vertical with its own correction, c stays Off
    s.execute("library.select", &json!({"ids": [a.0, b.0, c.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("geometry.guides", &json!({"guides": [[0.2, 0.1, 0.25, 0.9], [0.8, 0.1, 0.75, 0.9]]})).unwrap();
    s.execute("develop.set", &json!({"control": "geometry.rotate", "value": 2.0})).unwrap();
    assert_eq!(s.develop_of(a).unwrap().geometry.upright, Upright::Guided);
    let g = s.develop_of(b).unwrap().geometry.clone();
    assert_eq!((g.upright, g.upright_transform, g.guides.len()), (Upright::Vertical, gb.upright_transform, 0), "b keeps its own");
    assert_eq!(g.rotate, 2.0, "the Transform sliders still travel");
    let g = s.develop_of(c).unwrap().geometry.clone();
    assert_eq!((g.upright, g.guides.len()), (Upright::Off, 0), "c keeps Off");
    // Copy / Paste of a Guided photo (presets: the same recipe)
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["geometry"]})).unwrap();
    s.execute("library.select", &json!({"ids": [b.0], "active": b.0})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    let g = s.develop_of(b).unwrap().geometry.clone();
    assert_eq!((g.upright, g.upright_transform, g.guides.len()), (Upright::Vertical, gb.upright_transform, 0), "paste keeps b's own");
}

/// A second click on the mode that is on starts over: the analyses still running are cancelled
/// (their row goes, their results are dropped) and every photo it reaches is analysed afresh, also
/// the ones that had their correction already; the Transform sliders stay, Guided's guides go.
#[test]
fn a_second_click_cancels_the_running_analyses_and_starts_over() {
    let (mut s, [a, b, c]) = three(true);
    s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
    let first = rows(&s);
    assert_eq!(first.len(), 1);
    s.execute("develop.set", &json!({"control": "geometry.rotate", "value": 4.0})).unwrap();
    s.execute("geometry.upright", &json!({"mode": "vertical", "again": true})).unwrap();
    let second = rows(&s);
    assert_eq!(second.len(), 1, "one row: {second:?}");
    assert_ne!(second[0].id, first[0].id, "the first batch was cancelled and a new one started");
    assert_eq!((second[0].done, second[0].total), (0, 2));
    assert_eq!(s.upright_jobs.pending(), 2, "both photos again");
    finish(&mut s);
    for id in [b, c] {
        assert_eq!(transform(&s, id), Some(s.upright_for(id, Upright::Vertical).unwrap()), "its own correction");
        assert_eq!(s.develop_of(id).unwrap().geometry.rotate, 4.0, "the sliders stay");
    }
    // done: a second click analyses every selected photo afresh again
    s.execute("geometry.upright", &json!({"mode": "vertical", "again": true})).unwrap();
    assert_eq!(s.upright_jobs.pending(), 2, "analysed afresh");
    assert!(transform(&s, b).is_none() && transform(&s, c).is_none());
    finish(&mut s);
    assert_eq!(transform(&s, b), Some(s.upright_for(b, Upright::Vertical).unwrap()));
    // a plain run (the Update button, an agent) with an unchanged correction starts nothing
    s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
    assert_eq!(s.upright_jobs.pending(), 0);
    // Guided: the guides go, the sliders stay
    s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
    s.execute("geometry.guides", &json!({"guides": [[0.2, 0.1, 0.25, 0.9], [0.8, 0.1, 0.75, 0.9]]})).unwrap();
    let undo = s.undo.len();
    s.execute("geometry.upright", &json!({"mode": "guided", "again": true})).unwrap();
    let g = s.develop_of(a).unwrap().geometry.clone();
    assert_eq!((g.upright, g.guides.len(), g.rotate), (Upright::Guided, 0, 4.0));
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
}

/// The row's ✕ stops the batch: the row goes, nothing more lands, and the photos left render with
/// a correction worked out per render until they are analysed again.
#[test]
fn cancelling_the_upright_row_stops_the_batch() {
    let (mut s, [_, b, c]) = three(true);
    s.execute("geometry.upright", &json!({"mode": "full"})).unwrap();
    let row = rows(&s);
    assert_eq!(row.len(), 1);
    s.activity.cancel(row[0].id).unwrap();
    s.upright_poll();
    assert!(rows(&s).is_empty(), "the row goes");
    assert_eq!(s.upright_jobs.pending(), 0);
    std::thread::sleep(Duration::from_millis(50));
    s.upright_poll();
    for id in [b, c] {
        assert_eq!(s.develop_of(id).unwrap().geometry.upright, Upright::Full, "the mode stays");
        assert!(transform(&s, id).is_none(), "nothing lands after the cancel");
    }
    // the next click works them out again
    s.execute("geometry.upright", &json!({"mode": "full", "again": true})).unwrap();
    finish(&mut s);
    assert!(transform(&s, b).is_some() && transform(&s, c).is_some());
}

/// UI-thread time of Upright with Auto Sync on 20 selected procedural demo scenes (CPU): the click,
/// and the per-frame merges while the worker analyses the others, against working all of them out
/// on the UI thread. `cargo test -p lightcraft-engine --lib upright_sync_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn upright_sync_bench() {
    for background in [false, true] {
        let mut s = Session::with_demo();
        s.upright_jobs.background = background;
        let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(20).collect();
        assert_eq!(ids.len(), 20, "20 demo scenes");
        let raw: Vec<u64> = ids.iter().map(|i| i.0).collect();
        s.execute("library.select", &json!({"ids": raw, "active": raw[0]})).unwrap();
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        // the previews a photographer has on screen are decoded already
        for id in &ids {
            s.source_now(*id, SourceLevel::Preview).unwrap();
        }
        let t = Instant::now();
        s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
        let click = t.elapsed();
        let (mut polls, mut longest, mut n) = (Duration::ZERO, Duration::ZERO, 0u32);
        while s.upright_jobs.pending() > 0 {
            let p = Instant::now();
            s.upright_poll();
            let e = p.elapsed();
            polls += e;
            longest = longest.max(e);
            n += 1;
            std::thread::sleep(Duration::from_millis(16));
        }
        let wall = t.elapsed();
        let own = ids.iter().filter(|id| transform(&s, **id).is_some()).count();
        let distinct = ids.iter().filter_map(|id| transform(&s, *id)).map(|h| format!("{h:?}")).collect::<std::collections::HashSet<_>>().len();
        eprintln!(
            "background={background}: click {click:?} on the UI thread; {n} frame merges {polls:?} (longest {longest:?}); all corrections in {wall:?}; {own}/20 with a correction, {distinct} distinct"
        );
    }
}
