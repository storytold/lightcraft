//! Persistent library: commands survive a restart (with and without a clean close).

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(dir: &std::path::Path, seed: bool) -> Session {
    let mut s = Session::new();
    s.open_library(dir, seed).unwrap();
    s
}

#[test]
fn edits_survive_restart_without_close() {
    let dir = temp_dir("crash");
    let mut s = open(&dir, true);
    assert!(s.library.as_ref().unwrap().report.created);
    assert!(s.catalog.len() > 5, "seeded demo");
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    s.execute("photo.flag", &json!({"flag": "reject"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.25})).unwrap();
    // a slider drag: one undo step, logged once at the end
    s.execute("develop.beginInteraction", &json!({"label": "Contrast"})).unwrap();
    for v in [5, 10, 20, 33] {
        s.execute("develop.set", &json!({"control": "light.contrast", "value": v})).unwrap();
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    s.execute("album.create", &json!({"name": "Keepers", "addSelected": true})).unwrap();
    s.execute("preset.create", &json!({"name": "Bright"})).unwrap();
    s.execute("edit.undo", &json!({})).ok();
    s.execute("edit.redo", &json!({})).ok();
    let expect = s.catalog.to_snapshot();
    let info = s.execute("library.info", &json!({})).unwrap();
    assert!(info["logRecords"].as_u64().unwrap() >= 5, "{info}");
    drop(s); // no close: like a crash

    let s2 = open(&dir, true);
    let r = &s2.library.as_ref().unwrap().report;
    assert!(!r.created && r.replayed >= 5, "{r:?}");
    assert_eq!(s2.catalog.to_snapshot(), expect);
    let p = s2.catalog.photo(id).unwrap();
    assert_eq!((p.rating, p.develop.light.exposure, p.develop.light.contrast), (5, 1.25, 33.0));
    assert!(s2.catalog.albums().any(|a| a.name == "Keepers" && a.photos == vec![id]));
    assert!(s2.presets.iter().any(|p| p.name == "Bright" && !p.builtin));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Compaction at the threshold runs on a worker thread (issue #37): commands go on (and stay
/// durable) meanwhile, the frame loop's `persist_if_dirty` finishes it, and a crash at any time
/// keeps every command.
#[test]
fn threshold_compaction_runs_in_the_background() {
    let dir = temp_dir("bg-compact");
    let mut s = open(&dir, true);
    s.library.as_mut().unwrap().journal_mut().policy = lightcraft_catalog::SnapshotPolicy { max_records: 8, max_bytes: u64::MAX };
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    let mut background = 0;
    for k in 0..60usize {
        s.selection = crate::Selection::single(ids[k % ids.len()]);
        s.execute("photo.rate", &json!({"rating": k % 6})).unwrap();
        if s.execute("library.info", &json!({})).unwrap()["persistence"]["snapshotRunning"].as_bool().unwrap() {
            background += 1;
        }
        if k == 30 {
            // a crash while a compaction may be in flight
            let expect = s.catalog.to_snapshot();
            let copy = temp_dir("bg-compact-crash");
            std::fs::create_dir_all(&copy).unwrap();
            for f in ["catalog.snap", "catalog.log"] {
                if dir.join(f).exists() {
                    std::fs::copy(dir.join(f), copy.join(f)).unwrap();
                }
            }
            let s2 = open(&copy, true);
            assert_eq!(s2.catalog.to_snapshot(), expect);
            let _ = std::fs::remove_dir_all(&copy);
        }
    }
    assert!(background > 0, "compactions ran in the background");
    for _ in 0..10_000 {
        s.persist_if_dirty();
        if !s.library.as_ref().unwrap().journal().snapshot_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let info = s.execute("library.info", &json!({})).unwrap();
    let p = &info["persistence"];
    assert!(p["snapshots"].as_u64() >= Some(2) && p["lastSnapshot"]["background"] == true, "{p}");
    // the log holds exactly the records after the snapshot
    assert_eq!(info["logRecords"].as_u64().unwrap(), info["seq"].as_u64().unwrap() - info["snapshotSeq"].as_u64().unwrap(), "{info}");
    let expect = s.catalog.to_snapshot();
    drop(s); // no close: like a crash
    let s2 = open(&dir, true);
    assert_eq!(s2.catalog.to_snapshot(), expect);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_writes_snapshot_and_view_state() {
    let dir = temp_dir("close");
    let mut s = open(&dir, true);
    let ids = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [ids[3].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let expect = s.catalog.to_snapshot();
    s.close_library().unwrap();
    assert_eq!(std::fs::metadata(dir.join("catalog.log")).unwrap().len(), 0);
    drop(s); // one session per library (issue #99)

    let s2 = open(&dir, true);
    let r = &s2.library.as_ref().unwrap().report;
    assert_eq!(r.replayed, 0);
    assert_eq!(s2.catalog.to_snapshot(), expect);
    assert_eq!(s2.selection.active, Some(ids[3]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Thumbnails rendered in one session are served from the disk cache in the next, without
/// loading the source; an edit changes the key.
#[test]
fn thumbnails_come_from_the_disk_cache_after_restart() {
    let dir = temp_dir("thumbs");
    let mut s = open(&dir, true);
    let ids: Vec<_> = s.visible_cloned().into_iter().take(3).collect();
    for id in &ids {
        let job = s.thumb_job(*id, 230).unwrap();
        assert_eq!(job.request.max_w, 256, "bucketed");
        let r = job.run();
        assert!(r.loaded.is_some(), "first render decodes the source");
        s.accept(&r);
        let img = r.rendered.unwrap().image;
        assert_eq!(img.width.max(img.height), 256);
    }
    drop(s);

    let mut s = open(&dir, true);
    for id in &ids {
        let r = s.thumb_job(*id, 240).unwrap().run();
        assert!(r.loaded.is_none(), "served from cache");
        let img = r.rendered.unwrap().image;
        assert_eq!(img.width.max(img.height), 256);
    }
    let info = s.execute("library.info", &json!({})).unwrap();
    assert_eq!(info["cache"]["disk"]["hits"], 3, "{info}");
    // an edit is a new key: rendered (from the source), then cached
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    let r = s.thumb_job(ids[0], 230).unwrap().run();
    assert!(r.loaded.is_some());
    // exact-size renders (loupe, before/after, exports) bypass the thumbnail cache
    assert!(s.render_job(ids[0], 230, 230, true, true).unwrap().cache.is_none());
    assert!(s.render_job(ids[0], 230, 230, false, true).unwrap().cache.is_none());
    s.execute("library.clearPreviews", &json!({})).unwrap();
    assert!(s.thumb_job(ids[1], 230).unwrap().run().loaded.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_library_without_seed_is_empty_and_compacts() {
    let dir = temp_dir("empty");
    let mut s = open(&dir, false);
    assert!(s.catalog.is_empty());
    s.execute("album.create", &json!({"name": "A"})).unwrap();
    s.execute("library.compact", &json!({})).unwrap();
    let info = s.execute("library.info", &json!({})).unwrap();
    assert_eq!(info["logRecords"], 0);
    assert_eq!(info["snapshotSeq"], info["seq"]);
    // persistence timings (issue #37): the append and the compaction were measured
    let p = &info["persistence"];
    assert!(p["appends"].as_u64() >= Some(1), "{p}");
    assert!(p["snapshots"].as_u64() >= Some(1), "{p}");
    assert!(p["lastSnapshot"]["bytes"].as_u64() > Some(0), "{p}");
    assert!(p["lastSnapshot"]["totalMs"].as_f64() >= p["lastSnapshot"]["serializeMs"].as_f64(), "{p}");
    drop(s); // one session per library (issue #99)
    let s2 = open(&dir, true);
    assert_eq!(s2.catalog.len(), 0, "an existing library is never seeded");
    assert_eq!(s2.catalog.albums().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn profile_favorites_and_recent_survive_reopen() {
    let dir = temp_dir("profiles");
    let mut s = open(&dir, true);
    s.execute("profile.favorite", &json!({"id": "lc.bw.sepia"})).unwrap();
    s.execute("profile.favorite", &json!({"id": "lc.vivid"})).unwrap();
    s.execute("profile.favorite", &json!({"id": "lc.vivid"})).unwrap(); // toggled off again
    assert!(s.execute("profile.favorite", &json!({"id": "nope"})).is_err());
    for id in ["lc.neutral", "lc.vivid", "lc.film.cool-fade", "lc.mono", "lc.cine.teal-amber", "lc.muted.bleached", "lc.vivid"] {
        s.execute("develop.profile", &json!({"id": id})).unwrap();
    }
    let want_recent = ["lc.vivid", "lc.muted.bleached", "lc.cine.teal-amber", "lc.mono", "lc.film.cool-fade"];
    assert_eq!(s.profile_recent, want_recent, "newest first, deduplicated, at most 5");
    drop(s); // no clean close: every command persisted already
    let mut s = open(&dir, false);
    assert_eq!(s.profile_favorites, ["lc.bw.sepia"]);
    assert_eq!(s.profile_recent, want_recent);
    let menu = s.execute("profiles.menu", &json!({})).unwrap();
    assert_eq!(menu["favorites"][0]["name"], "Sepia Tone");
    assert_eq!(menu["recent"].as_array().unwrap().len(), 5);
    assert_eq!(menu["groups"][0]["name"], "Basic");
    let list = s.execute("profiles.list", &json!({})).unwrap();
    assert!(list.as_array().unwrap().iter().any(|p| p["id"] == "lc.bw.sepia" && p["favorite"] == true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shown_library_folder_survives_reopen() {
    let dir = temp_dir("libfolder");
    let mut s = open(&dir, true);
    let id = s.catalog.alloc_photo_id();
    let p = lightcraft_catalog::Photo::new(
        id,
        lightcraft_catalog::Source::File { path: "/pics/trip/a.jpg".into() },
        "a.jpg",
        "JPEG",
        60,
        40,
        "2026-01-01T10:00:00",
    );
    s.commit("Add", lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": "/pics/trip"})).unwrap();
    s.save_view();
    drop(s);
    let s = open(&dir, false);
    assert_eq!((s.source, s.library_folder.as_deref()), (crate::LibrarySource::LibraryFolder, Some("/pics/trip")));
    drop(s);
    // a folder that holds none of the library's photos any more opens on everything too
    std::fs::write(dir.join("view.json"), br#"{"source": {"kind": "libraryFolder"}, "library_folder": "/no/such/folder"}"#).unwrap();
    let s = open(&dir, false);
    assert_eq!((s.source, s.library_folder.clone()), (crate::LibrarySource::All, None));
    drop(s);
    // a view file that names the source but no folder opens on everything, never an empty grid
    std::fs::write(dir.join("view.json"), br#"{"source": {"kind": "libraryFolder"}}"#).unwrap();
    let s = open(&dir, false);
    assert_eq!((s.source, s.library_folder), (crate::LibrarySource::All, None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Smart albums with a rule set: all / any / none, nested groups, validation, live updates.
#[test]
fn smart_album_rule_sets() {
    let mut s = crate::Session::with_demo();
    s.clock = Box::new(|| "2026-10-02T12:00:00".to_string());
    let fields = s.execute("album.ruleFields", &serde_json::json!({})).unwrap();
    assert!(fields.as_array().unwrap().iter().any(|f| f["field"] == "keywords" && f["ops"].as_array().unwrap().len() > 3));
    // each field names its field-menu group (null at the top level)
    let group = |id: &str| fields.as_array().unwrap().iter().find(|f| f["field"] == id).map(|f| f["group"].clone());
    assert_eq!(group("filePath"), Some(serde_json::json!("File")));
    assert_eq!(group("rating"), Some(serde_json::Value::Null));
    // choices stay the ids rules store; choiceLabels maps them to what people read
    let flag = fields.as_array().unwrap().iter().find(|f| f["field"] == "copyrightStatus").unwrap();
    assert_eq!(flag["choices"], serde_json::json!(["copyrighted", "publicDomain", "unknown"]));
    assert_eq!(flag["choiceLabels"]["publicDomain"], "Public Domain");
    let album = fields.as_array().unwrap().iter().find(|f| f["field"] == "album").unwrap();
    assert_eq!((album["kind"].as_str(), album["ops"].as_array().map(Vec::len)), (Some("album"), Some(2)), "an album id, is / isn't");
    let bad = s.execute(
        "album.createSmart",
        &serde_json::json!({"name": "Bad", "rules": {"ruleSet": {"rules": [{"field": "rating", "op": "contains", "value": 1}]}}}),
    );
    assert!(bad.is_err(), "operators are checked");
    // yes/no values: "false" means no, anything unreadable is an error rather than a silent yes
    let maybe = s.execute(
        "album.createSmart",
        &serde_json::json!({"name": "Maybe", "rules": {"ruleSet": {"rules": [{"field": "edited", "op": "is", "value": "maybe"}]}}}),
    );
    assert!(maybe.is_err_and(|e| e.to_string().contains("yes or no")), "unreadable yes/no value");
    // the library filter checks a rule set the same way, instead of quietly showing nothing
    for rules in [serde_json::json!([{"field": "edited", "op": "is", "value": "maybe"}]), serde_json::json!([{"field": "nope", "op": "is"}])] {
        let r = s.execute("library.filter", &serde_json::json!({"ruleSet": {"rules": rules}}));
        assert!(r.is_err(), "{rules}");
    }
    assert!(s.filter.rule_set.is_none(), "a refused filter isn't applied");
    s.execute("library.filter", &serde_json::json!({"ruleSet": {"rules": [{"field": "edited", "op": "is", "value": "no"}]}})).unwrap();
    s.execute("library.filter", &serde_json::json!({"ruleSet": null})).unwrap();
    let r = s
        .execute(
            "album.createSmart",
            &serde_json::json!({"name": "Unedited", "rules": {"ruleSet": {"rules": [{"field": "edited", "op": "is", "value": "false"}]}}}),
        )
        .unwrap();
    let unedited = s.catalog.photos().filter(|p| !p.deleted && !p.is_edited()).count();
    assert_eq!(r["count"].as_u64(), Some(unedited as u64));
    let r = s
        .execute(
            "album.createSmart",
            &serde_json::json!({"name": "Good ones", "rules": {"ruleSet": {"match": "all", "rules": [
                {"field": "rating", "op": "gte", "value": 4},
                {"group": {"match": "none", "rules": [{"field": "flag", "op": "is", "value": "reject"}]}}
            ]}}}),
        )
        .unwrap();
    let id = lightcraft_catalog::AlbumId(r["id"].as_u64().unwrap());
    let want = s.catalog.photos().filter(|p| !p.deleted && p.rating >= 4 && p.flag != lightcraft_catalog::Flag::Reject).count();
    assert!(want > 0);
    assert_eq!(s.catalog.album_count(id), want);
    // live: a new 5-star photo joins
    let other = s.catalog.photos().find(|p| p.rating < 4 && !p.deleted).unwrap().id;
    s.execute("photo.rate", &serde_json::json!({"ids": [other.0], "rating": 5})).unwrap();
    assert_eq!(s.catalog.album_count(id), want + 1);
    // the same rules work as a library filter (agents: library.filter {ruleSet})
    s.execute("library.filter", &serde_json::json!({"ruleSet": {"rules": [{"field": "rating", "op": "is", "value": 5}]}})).unwrap();
    assert!(s.visible_cloned().iter().all(|id| s.catalog.photo(*id).unwrap().rating == 5));
    let list = s.execute("albums.list", &serde_json::json!({})).unwrap();
    let a = list.as_array().unwrap().iter().find(|a| a["name"] == "Good ones").unwrap().clone();
    assert!(a.to_string().contains("rating is ≥ 4"), "{a}");
}

/// The filter bar's label multi-select: any of the chosen labels.
#[test]
fn filter_any_of_several_labels() {
    use lightcraft_catalog::ColorLabel;
    let mut s = crate::Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    for (id, l) in ids.iter().zip(["red", "yellow", "green"]) {
        s.execute("photo.label", &serde_json::json!({"ids": [id], "label": l})).unwrap();
    }
    s.execute("library.filter", &serde_json::json!({"labels": ["red", "yellow"]})).unwrap();
    let vis = s.visible_cloned();
    let want = s.catalog.photos().filter(|p| p.in_library() && matches!(p.label, Some(ColorLabel::Red | ColorLabel::Yellow))).count();
    assert_eq!(want, 2);
    assert_eq!(vis.len(), want);
    assert!(vis.iter().all(|id| matches!(s.catalog.photo(*id).unwrap().label, Some(ColorLabel::Red | ColorLabel::Yellow))));
    assert!(s.filter.describe().contains("label red or yellow"));
    s.execute("library.filter", &serde_json::json!({"labels": []})).unwrap();
    assert!(s.visible_cloned().len() > want);
}

/// Quick Collection / target album: B toggles the selection in it (created on first use);
/// another album can be the target; clear empties it; all undoable.
#[test]
fn quick_collection_and_target_album() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    assert!(s.catalog.quick_collection().is_none());
    s.execute("library.select", &serde_json::json!({"ids": [ids[0], ids[1]]})).unwrap();
    let r = s.execute("album.toggleTarget", &serde_json::json!({})).unwrap();
    assert_eq!((r["added"].as_bool(), r["count"].as_u64(), r["name"].as_str()), (Some(true), Some(2), Some("Quick Collection")));
    let quick = s.catalog.quick_collection().expect("created");
    // again: both are in it, so they come out
    assert_eq!(s.execute("album.toggleTarget", &serde_json::json!({})).unwrap()["count"], 0);
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.catalog.album_count(quick), 2);
    // a regular album as the target
    let alb = s.execute("album.create", &serde_json::json!({"name": "Picks"})).unwrap()["id"].as_u64().unwrap();
    s.execute("album.setTarget", &serde_json::json!({"id": alb})).unwrap();
    s.execute("album.toggleTarget", &serde_json::json!({"ids": [ids[2]]})).unwrap();
    assert_eq!(s.catalog.album_count(lightcraft_catalog::AlbumId(alb)), 1);
    assert_eq!(s.catalog.album_count(quick), 2, "the Quick Collection is untouched");
    let smart = s.execute("album.createSmart", &serde_json::json!({"name": "S", "rules": {"rating": 3}})).unwrap()["id"].as_u64().unwrap();
    assert!(s.execute("album.setTarget", &serde_json::json!({"id": smart})).is_err());
    s.execute("album.setTarget", &serde_json::json!({"id": null})).unwrap();
    s.execute("album.clearQuick", &serde_json::json!({})).unwrap();
    assert_eq!(s.catalog.album_count(quick), 0);
}

/// Scaling check (ignored: `cargo test --release -p lightcraft-engine -- --ignored scale --nocapture`):
/// the per-frame / per-click library queries on a 100k-photo catalog.
#[test]
#[ignore]
fn scale_100k_library_queries() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    use std::time::Instant;
    let mut s = crate::Session::new();
    let n = 100_000u64;
    let mut ops = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut p = Photo::new(
            PhotoId(i + 1),
            Source::Demo { scene: (i % 20) as u32 },
            &format!("IMG_{i:06}.jpg"),
            "JPEG",
            6000,
            4000,
            "2026-01-01T00:00:00",
        );
        p.captured = Some(format!("20{:02}-{:02}-{:02}T{:02}:00:00", 10 + i % 16, 1 + i % 12, 1 + i % 28, i % 24));
        p.rating = (i % 6) as u8;
        p.meta.keywords = vec![format!("kw{}", i % 500), "travel|italy".into()];
        p.meta.camera = format!("Model {}", i % 30);
        ops.push(Op::AddPhoto { photo: Box::new(p) });
    }
    let t = Instant::now();
    s.commit("Add", Op::Batch { ops }).unwrap();
    eprintln!("add 100k: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let time = |what: &str, f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("{what}: {ms:.1} ms");
        ms
    };
    let mut worst: Vec<(String, f64)> = Vec::new();
    let mut m = |w: &str, f: &mut dyn FnMut()| worst.push((w.to_string(), time(w, f)));
    m("visible (all)", &mut || {
        let _ = s.visible_cloned();
    });
    m("visible (cached)", &mut || {
        let _ = s.visible_cloned();
    });
    m("filter rating ≥ 4", &mut || {
        s.execute("library.filter", &serde_json::json!({"rating": 4})).unwrap();
        let _ = s.visible_cloned();
    });
    m("filter ruleSet keyword + date", &mut || {
        s.execute("library.filter", &serde_json::json!({"rating": 0, "ruleSet": {"rules": [{"field": "keywords", "op": "contains", "value": "kw42"}, {"field": "captureDate", "op": "is", "value": "2015"}]}})).unwrap();
        let _ = s.visible_cloned();
    });
    s.execute("library.filter", &serde_json::json!({"ruleSet": null})).unwrap();
    m("date groups", &mut || {
        let _ = s.execute("library.groups", &serde_json::json!({})).unwrap();
    });
    m("keyword tree", &mut || {
        let _ = s.execute("keyword.list", &serde_json::json!({})).unwrap();
    });
    m("keyword suggestions", &mut || {
        let _ = s.catalog.keyword_suggestions(&["kw1".to_string()], "kw", 12);
    });
    let alb = s.execute("album.createSmart", &serde_json::json!({"name": "S", "rules": {"rating": 5}})).unwrap()["id"].as_u64().unwrap();
    m("smart album count", &mut || {
        let _ = s.catalog.album_count(lightcraft_catalog::AlbumId(alb));
    });
    m("albums.list", &mut || {
        let _ = s.execute("albums.list", &serde_json::json!({})).unwrap();
    });
    m("select all", &mut || {
        s.execute("library.selectAll", &serde_json::json!({})).unwrap();
    });
    m("rate 100k selected", &mut || {
        s.execute("photo.rate", &serde_json::json!({"rating": 3})).unwrap();
    });
    m("undo", &mut || {
        s.execute("edit.undo", &serde_json::json!({})).unwrap();
    });
    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
    eprintln!("slowest: {:?}", &worst[..3]);
}

/// Issue #99: a library is open in one session at a time. A second opener is refused (nothing
/// read or written), the owning session can reopen it, and it is free again once closed.
#[test]
fn a_library_open_elsewhere_is_refused() {
    let dir = temp_dir("locked");
    let mut s = open(&dir, true);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let log = std::fs::read(dir.join("catalog.log")).unwrap();

    let mut other = Session::new();
    let e = other.open_library(&dir, true).unwrap_err();
    assert!(matches!(e, crate::EngineError::LibraryInUse(_)), "{e:?}");
    assert!(e.to_string().contains("already open in"), "{e}");
    assert!(other.library.is_none());
    assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), log, "the refused opener wrote nothing");

    // the session that has it open may reopen it (Settings → Open Library on the same folder)
    s.close_library().unwrap();
    s.open_library(&dir, true).unwrap();
    assert!(other.open_library(&dir, true).is_err(), "still locked after reopening");
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let expect = s.catalog.to_snapshot();

    drop(s);
    other.open_library(&dir, true).unwrap();
    assert_eq!(other.catalog.to_snapshot(), expect);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A capture-time shift from an agent can't overflow or write a date no one can read back: a
/// shift of more than 10,000 years is refused and nothing changes; an ordinary one still works.
#[test]
fn capture_time_shift_is_bounded() {
    let mut s = crate::Session::with_demo();
    let id = s.catalog.photos().next().unwrap().id;
    let before = s.catalog.photo(id).unwrap().captured.clone();
    for p in [
        serde_json::json!({"ids": [id.0], "shift": 1e30}),
        serde_json::json!({"ids": [id.0], "hours": -1e15}),
        serde_json::json!({"ids": [id.0], "shift": 9.3e18}),
    ] {
        let r = s.execute("photo.setCaptureTime", &p);
        // a real refusal, not the panic guard catching an overflow ("failed unexpectedly")
        assert!(r.as_ref().is_err_and(|e| e.to_string().contains("10,000 years")), "{p}: {r:?}");
        assert_eq!(s.catalog.photo(id).unwrap().captured, before, "{p} changed nothing");
    }
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [id.0], "hours": 1})).unwrap();
    assert_ne!(s.catalog.photo(id).unwrap().captured, before);
    // a shift that would take a photo past year 9999 says so, rather than skipping it quietly
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [id.0], "time": "9999-12-31T12:00:00"})).unwrap();
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [id.0], "hours": 24}));
    assert!(r.as_ref().is_err_and(|e| e.to_string().contains("9999")), "{r:?}");
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("9999-12-31T12:00:00"), "unchanged");
}

/// Values are checked too: an agent's rule set with a date that isn't one, or a rating of 9, is
/// refused with the rule's position, by album.createSmart and library.filter alike.
#[test]
fn smart_rule_values_are_checked_by_commands() {
    let mut s = crate::Session::with_demo();
    let r = s.execute(
        "album.createSmart",
        &serde_json::json!({"name": "Odd", "rules": {"ruleSet": {"rules": [
            {"field": "rating", "op": "gte", "value": 3},
            {"field": "captureDate", "op": "is", "value": "banana"}
        ]}}}),
    );
    let e = r.expect_err("a date that isn't one").to_string();
    assert!(e.contains("rule 2:") && e.contains("needs a date"), "{e}");
    assert!(s.catalog.albums().all(|a| a.name != "Odd"));
    let e = s
        .execute("library.filter", &serde_json::json!({"ruleSet": {"rules": [{"field": "rating", "op": "gte", "value": 9}]}}))
        .expect_err("rating 9")
        .to_string();
    assert!(e.contains("rule 1: no rating 0–5 is ≥ 9"), "{e}");
}

/// A smart album saved with an old album operator (≥, from when Album was a number field) can
/// still be edited: its rule is read as the "isn't" it always meant.
#[test]
fn old_album_operator_rules_stay_editable() {
    use lightcraft_catalog::{Album, AlbumId, Op};
    let mut s = crate::Session::with_demo();
    let trip = s.execute("album.create", &serde_json::json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let rules: lightcraft_catalog::Filter =
        serde_json::from_value(serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "gte", "value": trip}]}})).unwrap();
    let id = s.catalog.alloc_album_id();
    s.catalog.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(rules)), ..Album::new(id, "Old") } }).unwrap();
    s.execute("album.setRules", &serde_json::json!({"id": id.0, "rules": {"rating": 2}})).unwrap();
    let saved = serde_json::to_string(&s.catalog.album(AlbumId(id.0)).unwrap().smart).unwrap();
    assert!(saved.contains(r#""op":"isNot""#), "{saved}");
    s.execute("library.filter", &serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "lte", "value": trip}]}})).unwrap();
}

/// Renaming and changing the rules is one step: both or neither, undone together.
#[test]
fn set_rules_with_a_name_is_one_step() {
    let mut s = crate::Session::with_demo();
    let id = s.execute("album.createSmart", &serde_json::json!({"name": "Old", "rules": {"rating": 3}})).unwrap()["id"].as_u64().unwrap();
    let album = |s: &crate::Session| s.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap().clone();
    let good = serde_json::json!({"ruleSet": {"rules": [{"field": "rating", "op": "gte", "value": 4}]}});
    let bad = serde_json::json!({"ruleSet": {"rules": [{"field": "captureDate", "op": "is", "value": "banana"}]}});
    // refused rules: the name stays too
    assert!(s.execute("album.setRules", &serde_json::json!({"id": id, "name": "New", "replace": true, "rules": bad})).is_err());
    assert_eq!(album(&s).name, "Old");
    let undo = s.undo.len();
    s.execute("album.setRules", &serde_json::json!({"id": id, "name": "New", "replace": true, "rules": good})).unwrap();
    assert_eq!((album(&s).name.as_str(), s.undo.len()), ("New", undo + 1), "one undo step");
    assert!(album(&s).smart.unwrap().rule_set.is_some());
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(album(&s).name, "Old");
    assert!(album(&s).smart.unwrap().rule_set.is_none(), "undone together");
    // a blank name keeps the album's
    s.execute("album.setRules", &serde_json::json!({"id": id, "name": "  ", "rules": {"rating": 5}})).unwrap();
    assert_eq!(album(&s).name, "Old");
}

/// albums.list says which smart albums have rules that no longer check, and why.
#[test]
fn album_list_reports_problems() {
    let mut s = crate::Session::with_demo();
    let trip = s.execute("album.create", &serde_json::json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let rules = serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": trip}]}});
    let id = s.execute("album.createSmart", &serde_json::json!({"name": "In Trip", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    let find = |s: &mut crate::Session| {
        let list = s.execute("albums.list", &serde_json::json!({})).unwrap();
        list.as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap()
    };
    assert_eq!(find(&mut s)["problems"], serde_json::json!([]));
    s.execute("album.delete", &serde_json::json!({"id": trip})).unwrap();
    assert_eq!(find(&mut s)["problems"], serde_json::json!([format!("rule 1: no album {trip}")]));
}

/// "Travel" = keywords contain travel, but not in "Excluded Photos" (red or rejected): the counts
/// follow, and Excluded Photos can't then test Travel back (a loop).
#[test]
fn smart_album_excluding_a_smart_album() {
    let mut s = crate::Session::with_demo();
    let excluded = s
        .execute(
            "album.createSmart",
            &serde_json::json!({"name": "Excluded Photos", "rules": {"ruleSet": {"match": "any", "rules": [
                {"field": "label", "op": "is", "value": "red"}, {"field": "flag", "op": "is", "value": "reject"}]}}}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let keyword = s.catalog.photos().flat_map(|p| p.meta.keywords.clone()).next().expect("a demo keyword");
    let travel = s
        .execute(
            "album.createSmart",
            &serde_json::json!({"name": "Travel", "rules": {"ruleSet": {"rules": [
                {"field": "keywords", "op": "contains", "value": keyword}, {"field": "album", "op": "isNot", "value": excluded}]}}}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let count = |s: &crate::Session| s.catalog.album_count(lightcraft_catalog::AlbumId(travel));
    let want = |s: &crate::Session| {
        s.catalog
            .photos()
            .filter(|p| !p.deleted && p.meta.keywords.iter().any(|k| k.to_lowercase().contains(&keyword.to_lowercase())))
            .filter(|p| p.label != Some(lightcraft_catalog::ColorLabel::Red) && p.flag != lightcraft_catalog::Flag::Reject)
            .count()
    };
    assert_eq!(count(&s), want(&s));
    // reject one of Travel's photos: it leaves Travel
    let before = count(&s);
    let id = s.catalog.photos().find(|p| s.catalog.album_contains(lightcraft_catalog::AlbumId(travel), p)).map(|p| p.id).expect("a travel photo");
    s.execute("photo.flag", &serde_json::json!({"ids": [id.0], "flag": "reject"})).unwrap();
    assert_eq!(count(&s), before - 1);
    // Excluded Photos testing Travel back would include itself
    let looped = s.execute(
        "album.setRules",
        &serde_json::json!({"id": excluded, "rules": {"ruleSet": {"rules": [{"field": "album", "op": "is", "value": travel}]}}, "replace": true}),
    );
    assert!(looped.is_err_and(|e| e.to_string().contains("would make this album include itself")), "a loop is refused");
    // agents read the album by name in the summary
    let list = s.execute("albums.list", &serde_json::json!({})).unwrap();
    let travel_json = list.as_array().unwrap().iter().find(|a| a["id"] == travel).cloned().unwrap();
    assert!(travel_json["rulesText"].as_str().unwrap_or("").contains("album isn't “Excluded Photos”"), "{travel_json}");
}

/// "Update Rules from Current Filter" refuses a loop too: showing A ("Album isn't B") and
/// updating B from that view would make B test itself.
#[test]
fn rules_from_the_view_cant_loop() {
    let mut s = crate::Session::with_demo();
    let b = s.execute("album.createSmart", &serde_json::json!({"name": "B", "rules": {"rating": 2}})).unwrap()["id"].as_u64().unwrap();
    let rules = serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "isNot", "value": b}]}});
    let a = s.execute("album.createSmart", &serde_json::json!({"name": "A", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &serde_json::json!({"kind": "album", "id": a})).unwrap();
    let before = s.catalog.album(lightcraft_catalog::AlbumId(b)).unwrap().smart.clone();
    let r = s.execute("album.setRules", &serde_json::json!({"id": b, "fromView": true}));
    assert!(r.is_err_and(|e| e.to_string().contains("would make this album include itself")), "refused");
    assert_eq!(s.catalog.album(lightcraft_catalog::AlbumId(b)).unwrap().smart, before, "B unchanged");
    assert!(s.catalog.smart_album_problems(lightcraft_catalog::AlbumId(b)).is_empty());
}

/// A rule that stops checking later (the album it tests is deleted) doesn't lock up what doesn't
/// touch it: the filter bar keeps working, and an album's other settings can still be edited. A
/// change to the rules themselves is still checked.
#[test]
fn a_stale_rule_doesnt_block_other_changes() {
    let mut s = crate::Session::with_demo();
    let trip = s.execute("album.create", &serde_json::json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let rules = serde_json::json!({"rules": [{"field": "album", "op": "isNot", "value": trip}]});
    s.execute("library.filter", &serde_json::json!({"ruleSet": rules})).unwrap();
    let smart =
        s.execute("album.createSmart", &serde_json::json!({"name": "Not Trip", "rules": {"ruleSet": rules}})).unwrap()["id"].as_u64().unwrap();
    s.execute("album.delete", &serde_json::json!({"id": trip})).unwrap();
    // the filter bar: a rating still applies on top of the stale rule
    s.execute("library.filter", &serde_json::json!({"rating": 3})).unwrap();
    assert_eq!(s.filter.rating, 3);
    // the album: a partial edit that leaves its rules alone
    s.execute("album.setRules", &serde_json::json!({"id": smart, "rules": {"rating": 2}})).unwrap();
    // changing the rules is still checked
    let bad = serde_json::json!({"ruleSet": {"rules": [{"field": "rating", "op": "gte", "value": 9}]}});
    assert!(s.execute("library.filter", &bad).is_err());
    assert!(s.execute("album.setRules", &serde_json::json!({"id": smart, "rules": bad})).is_err());
}

/// album.setRules refuses a loop through the album filter as well as through the rules.
#[test]
fn an_album_filter_loop_is_refused() {
    let mut s = crate::Session::with_demo();
    let a = s.execute("album.createSmart", &serde_json::json!({"name": "A", "rules": {"rating": 2}})).unwrap()["id"].as_u64().unwrap();
    let r = s.execute("album.setRules", &serde_json::json!({"id": a, "rules": {"album": a}}));
    assert!(r.is_err_and(|e| e.to_string().contains("include itself")), "refused");
    assert_eq!(s.catalog.album(lightcraft_catalog::AlbumId(a)).unwrap().smart.as_ref().and_then(|f| f.album), None);
}
