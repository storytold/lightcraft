//! Persistent library: commands survive a restart (with and without a clean close).

use serde_json::json;

use crate::Session;

/// The test's folder in Temp, removed when the test ends.
fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lc-engine-{tag}"))
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
    s.library.as_mut().unwrap().journal_mut().policy = lcc_catalog::SnapshotPolicy { max_records: 8, max_bytes: u64::MAX };
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    let mut background = 0;
    for k in 0..60usize {
        s.selection = crate::Selection::single(ids[k % ids.len()]);
        s.execute("photo.rate", &json!({"rating": k % 6})).unwrap();
        if s.execute("library.info", &json!({})).unwrap()["persistence"]["snapshotRunning"].as_bool().unwrap() {
            background += 1;
        }
        // Windows locks the open store's byte ranges, so a running library can't be copied there
        if k == 30 && cfg!(not(windows)) {
            // a crash after a compaction: copying the store while the background thread writes it
            // tears the copy (no real crash leaves a half-old, half-new file)
            for _ in 0..10_000 {
                s.persist_if_dirty();
                if !s.library.as_ref().unwrap().journal().snapshot_running() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let expect = s.catalog.to_snapshot();
            let copy = temp_dir("bg-compact-crash");
            std::fs::create_dir_all(&copy).unwrap();
            for f in ["catalog.snap", "catalog.log", "catalog.redb"] {
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
    let _keys = crate::sensor_maps::keep_generation();
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
    // (each thumbnail read from disk; the record of a photo's kept thumbnail is read too, so no exact count)
    assert!(info["cache"]["disk"]["hits"].as_u64().is_some_and(|h| h >= 3), "every thumbnail read from disk: {info}");
    assert_eq!(info["cache"]["disk"]["misses"], 0, "nothing looked for on disk was missing: {info}");
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
    // while it runs it has a row in the activity stack (it can't be stopped)
    let rows = s.activity.list();
    let running = s.library.as_ref().is_some_and(|l| l.journal().snapshot_running());
    assert!(!running || rows.iter().any(|r| r.kind == "compaction" && !r.cancellable), "{rows:?}");
    // the compaction is written on a worker (issue #448: a large catalog frozen the UI while it
    // was serialised): the frame loop's persist finishes it
    for _ in 0..1000 {
        s.persist_if_dirty();
        if s.execute("library.info", &json!({})).unwrap()["logRecords"] == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    s.persist_if_dirty();
    assert!(s.activity.list().iter().all(|r| r.kind != "compaction"), "the row goes when the compaction has landed");
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
fn preset_at_import_and_hidden_marks_survive_reopen() {
    let dir = temp_dir("preset-marks");
    let mut s = open(&dir, true);
    let id = s.presets.first().unwrap().id.clone();
    s.execute("preset.forImport", &json!({"id": id})).unwrap();
    s.execute("preset.hide", &json!({"id": id})).unwrap();
    drop(s);
    let s = open(&dir, false);
    assert_eq!(s.preset_at_import, std::slice::from_ref(&id));
    assert_eq!(s.preset_hidden, [id]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shown_library_folder_survives_reopen() {
    let dir = temp_dir("libfolder");
    let mut s = open(&dir, true);
    let id = s.catalog.alloc_photo_id();
    let p =
        lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: "/pics/trip/a.jpg".into() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.commit("Add", lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
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
    let id = lcc_catalog::AlbumId(r["id"].as_u64().unwrap());
    let want = s.catalog.photos().filter(|p| !p.deleted && p.rating >= 4 && p.flag != lcc_catalog::Flag::Reject).count();
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

/// `label` and `labels` set together (only through `library.filter`, or a filter saved with the
/// single label) AND, and the chip says what they let through, not their union.
#[test]
fn filter_label_and_labels_together_chip() {
    use lcc_catalog::ColorLabel;
    let mut s = crate::Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    for (id, l) in ids.iter().zip(["red", "yellow", "green"]) {
        s.execute("photo.label", &json!({"ids": [id], "label": l})).unwrap();
    }
    let reds = s.catalog.photos().filter(|p| p.in_library() && p.label == Some(ColorLabel::Red)).count();
    let chip = |s: &Session| crate::filter_chips(&s.filter, &s.catalog).into_iter().find(|c| c.label.starts_with("Label")).expect("a label chip");
    s.execute("library.filter", &json!({"label": "red", "labels": ["yellow", "red"]})).unwrap();
    assert_eq!(s.visible_cloned().len(), reds);
    assert_eq!(chip(&s).label, "Label: Red");
    // nothing passes both: the chip names both constraints
    s.execute("library.filter", &json!({"label": "red", "labels": ["yellow", "green"]})).unwrap();
    assert_eq!(s.visible_cloned().len(), 0);
    assert_eq!(chip(&s).label, "Label: Red and (Yellow or Green)");
    s.execute("library.filter", &chip(&s).clear).unwrap();
    assert_eq!((s.filter.label, s.filter.labels.len()), (None, 0));
}

/// The filter bar's label multi-select: any of the chosen labels.
#[test]
fn filter_any_of_several_labels() {
    use lcc_catalog::ColorLabel;
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

#[test]
fn photo_label_toggle() {
    use lcc_catalog::ColorLabel;
    let mut s = crate::Session::with_demo();
    let id = s.selection.active.unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().label, None);

    // toggle red -> sets red
    let r = s.execute("photo.label", &serde_json::json!({"label": "red", "toggle": true})).unwrap();
    assert_eq!(r["label"], "red");
    assert_eq!(s.catalog.photo(id).unwrap().label, Some(ColorLabel::Red));

    // toggle red again -> clears red (None)
    let r = s.execute("photo.label", &serde_json::json!({"label": "red", "toggle": true})).unwrap();
    assert_eq!(r["label"], "none");
    assert_eq!(s.catalog.photo(id).unwrap().label, None);

    // toggle yellow -> sets yellow
    let r = s.execute("photo.label", &serde_json::json!({"label": "yellow", "toggle": true})).unwrap();
    assert_eq!(r["label"], "yellow");
    assert_eq!(s.catalog.photo(id).unwrap().label, Some(ColorLabel::Yellow));

    // toggle green while yellow -> sets green (not cleared because yellow != green)
    let r = s.execute("photo.label", &serde_json::json!({"label": "green", "toggle": true})).unwrap();
    assert_eq!(r["label"], "green");
    assert_eq!(s.catalog.photo(id).unwrap().label, Some(ColorLabel::Green));

    // toggle green again -> clears green
    let r = s.execute("photo.label", &serde_json::json!({"label": "green", "toggle": true})).unwrap();
    assert_eq!(r["label"], "none");
    assert_eq!(s.catalog.photo(id).unwrap().label, None);

    // multi-selection:
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    // photo 0: red, photo 1: none
    s.execute("photo.label", &serde_json::json!({"ids": [ids[0]], "label": "red"})).unwrap();
    s.execute("photo.label", &serde_json::json!({"ids": [ids[1]], "label": "none"})).unwrap();

    // toggle red on both -> not all have red, so sets red on both
    let r = s.execute("photo.label", &serde_json::json!({"ids": ids, "label": "red", "toggle": true})).unwrap();
    assert_eq!(r["label"], "red");
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(ids[0])).unwrap().label, Some(ColorLabel::Red));
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(ids[1])).unwrap().label, Some(ColorLabel::Red));

    // toggle red on both again -> all have red, so clears both
    let r = s.execute("photo.label", &serde_json::json!({"ids": ids, "label": "red", "toggle": true})).unwrap();
    assert_eq!(r["label"], "none");
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(ids[0])).unwrap().label, None);
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(ids[1])).unwrap().label, None);
}

/// Issue #213: the filter bar's keyword (and camera, lens) pickers take several values, any of which
/// matches; the facets still combine. One chip clears a facet, and the list follows keyword renames.
#[test]
fn filter_any_of_several_keywords() {
    use serde_json::json;
    let mut s = crate::Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().filter(|p| p.in_library()).take(3).map(|p| p.id.0).collect();
    for (id, k) in ids.iter().zip(["zebra", "yak", "xerus"]) {
        s.execute("photo.setMeta", &json!({"ids": [id], "keywords": [k]})).unwrap();
    }
    s.execute("library.filter", &json!({"keywords": ["zebra", "yak"]})).unwrap();
    let mut vis: Vec<u64> = s.visible_cloned().iter().map(|id| id.0).collect();
    vis.sort();
    assert_eq!(vis, ids[..2].to_vec());
    let chips = crate::filter_chips(&s.filter, &s.catalog);
    let chip = chips.iter().find(|c| c.label.starts_with("Keyword:")).unwrap();
    assert_eq!(chip.label, "Keyword: zebra or yak");
    // another facet narrows the any-of list
    let camera = s.catalog.photo(lcc_catalog::PhotoId(ids[1])).unwrap().meta.camera.clone();
    s.execute("library.filter", &json!({"cameras": [camera]})).unwrap();
    assert!(s.visible_cloned().iter().all(|id| s.catalog.photo(*id).unwrap().meta.keywords.iter().any(|k| k == "zebra" || k == "yak")));
    s.execute("library.filter", &json!({"cameras": []})).unwrap();
    // a renamed keyword stays chosen, a deleted one drops out
    s.execute("keyword.rename", &json!({"from": "yak", "to": "Bison"})).unwrap();
    assert_eq!(s.filter.keywords, vec!["zebra".to_string(), "Bison".to_string()]);
    s.execute("keyword.delete", &json!({"keyword": "zebra"})).unwrap();
    assert_eq!(s.filter.keywords, vec!["Bison".to_string()]);
    let chip = crate::filter_chips(&s.filter, &s.catalog).into_iter().find(|c| c.label.starts_with("Keyword:")).unwrap();
    s.execute("library.filter", &chip.clear).unwrap();
    assert!(s.filter.keywords.is_empty() && s.filter.keyword.is_none());
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
    assert_eq!(s.catalog.album_count(lcc_catalog::AlbumId(alb)), 1);
    assert_eq!(s.catalog.album_count(quick), 2, "the Quick Collection is untouched");
    let smart = s.execute("album.createSmart", &serde_json::json!({"name": "S", "rules": {"rating": 3}})).unwrap()["id"].as_u64().unwrap();
    assert!(s.execute("album.setTarget", &serde_json::json!({"id": smart})).is_err());
    s.execute("album.setTarget", &serde_json::json!({"id": null})).unwrap();
    s.execute("album.clearQuick", &serde_json::json!({})).unwrap();
    assert_eq!(s.catalog.album_count(quick), 0);
}

/// Catalog panel: Quick Collection as a source (empty before it exists), ⌘⇧B saves it as an
/// album and clears it in one undo step, Previous Import shows the latest import.
#[test]
fn catalog_panel_sources_and_save_quick_collection() {
    let mut s = crate::Session::with_demo();
    assert_eq!(s.execute("library.showQuickCollection", &serde_json::json!({})).unwrap()["count"], 0);
    assert!(s.execute("album.saveQuick", &serde_json::json!({})).is_err(), "nothing to save");
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("library.select", &serde_json::json!({"ids": ids})).unwrap();
    s.execute("album.toggleTarget", &serde_json::json!({})).unwrap();
    assert_eq!(s.execute("library.source", &serde_json::json!({"kind": "quickCollection"})).unwrap()["count"], 2);
    let r = s.execute("album.saveQuick", &serde_json::json!({"name": "Keepers"})).unwrap();
    let saved = lcc_catalog::AlbumId(r["id"].as_u64().unwrap());
    let quick = s.catalog.quick_collection().unwrap();
    assert_eq!((s.catalog.album_count(saved), s.catalog.album_count(quick)), (2, 0));
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert!(s.catalog.album(saved).is_none());
    assert_eq!(s.catalog.album_count(quick), 2);
    let latest = s.catalog.photos().filter(|p| p.in_library()).map(|p| p.imported.clone()).max().unwrap();
    let want = s.catalog.photos().filter(|p| p.in_library() && !p.deleted && p.imported == latest).count();
    let n = s.execute("library.source", &serde_json::json!({"kind": "previousImport"})).unwrap()["count"].as_u64().unwrap();
    assert_eq!(n as usize, want);
    assert!(n > 0);
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

/// The view pages: a window of the visible photos without copying them all, and
/// `catalog.query` pages the current view the same way.
#[test]
fn visible_photos_page_by_offset_and_limit() {
    let mut s = Session::with_demo();
    let all = s.visible_cloned();
    assert!(all.len() > 3);
    assert_eq!(s.visible_page(1, 2), (all.len(), all[1..3].to_vec()));
    assert_eq!(s.visible_page(all.len() - 1, 10), (all.len(), vec![all[all.len() - 1]]));
    assert_eq!(s.visible_page(usize::MAX, 10), (all.len(), vec![]));
    assert_eq!(s.visible_position(all[2]), Some(2));
    let r = s.execute("catalog.query", &json!({"offset": 1, "limit": 2})).unwrap();
    assert_eq!(r["total"], all.len());
    let ids: Vec<u64> = r["photos"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![all[1].0, all[2].0]);
    // a hostile offset is an empty page, not a crash
    let r = s.execute("catalog.query", &json!({"offset": u64::MAX, "limit": u64::MAX})).unwrap();
    assert_eq!(r["photos"].as_array().map(Vec::len), Some(0));
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

/// Edit Capture Time keeps the zone it is given (`…+01:00`), for one photo and with `each`, and a
/// later shift keeps it too; a time without a zone stays zone-less.
#[test]
fn capture_time_keeps_the_zone_given() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let captured = |s: &crate::Session, id| s.catalog.photo(id).unwrap().captured.clone();
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[0].0], "time": "2026-07-10T08:47:46+01:00"})).unwrap();
    assert_eq!(captured(&s, ids[0]).as_deref(), Some("2026-07-10T08:47:46+01:00"));
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[0].0, ids[1].0], "time": "2016-04-09 11:05-05:30", "each": true})).unwrap();
    for id in &ids {
        assert_eq!(captured(&s, *id).as_deref(), Some("2016-04-09T11:05:00-05:30"));
    }
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[0].0], "hours": 1})).unwrap();
    assert_eq!(captured(&s, ids[0]).as_deref(), Some("2016-04-09T12:05:00-05:30"), "a shift keeps the zone");
    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[1].0], "time": "2026-01-01T10:00:00"})).unwrap();
    assert_eq!(captured(&s, ids[1]).as_deref(), Some("2026-01-01T10:00:00"), "no zone given, none stored");
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[1].0], "time": "2026-01-01T10:00:00+1:00"}));
    assert!(r.is_err(), "a malformed zone is refused: {r:?}");
}

/// A capture time that doesn't fully read (a Lightroom catalog's `captureTime` is copied
/// verbatim) is left as it is and counted, not shifted into a clean-looking wrong time, and it
/// isn't mistaken for one that would leave years 0000–9999.
#[test]
fn capture_time_shift_counts_photos_whose_time_does_not_read() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let (good, bad) = (ids[0], ids[1]);
    for (id, t) in [(good, "2026-01-01T10:00:00"), (bad, "2026-01-01T10:00:00 and more")] {
        s.catalog.apply(lcc_catalog::Op::SetCaptured { id, captured: Some(t.into()) }).unwrap();
    }
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [good.0, bad.0], "hours": 1})).unwrap();
    assert_eq!(r["unreadable"], 1, "the unreadable photo is counted: {r}");
    assert_eq!(r["changed"], 1, "{r}");
    let captured = |s: &crate::Session, id| s.catalog.photo(id).unwrap().captured.clone();
    assert_eq!(captured(&s, good).as_deref(), Some("2026-01-01T11:00:00"));
    assert_eq!(captured(&s, bad).as_deref(), Some("2026-01-01T10:00:00 and more"), "left as it was");
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(captured(&s, good).as_deref(), Some("2026-01-01T10:00:00"), "one undo step");
    // a shift past year 9999 for the readable photo is still the out-of-range refusal, and the
    // unreadable one alone isn't
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [bad.0], "hours": 24 * 365 * 9000})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["unreadable"].as_u64()), (Some(0), Some(1)), "{r}");
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [good.0, bad.0], "hours": 24 * 365 * 9000}));
    assert!(r.as_ref().is_err_and(|e| e.to_string().contains("1 photo(s) outside")), "{r:?}");
}

/// Setting the active photo's time moves the others by its change, so an active photo whose own
/// time doesn't read can't anchor it: refused, nothing changes. Alone, or setting every photo's
/// time, it gets the time, which repairs it.
#[test]
fn capture_time_set_refuses_an_active_photo_whose_time_does_not_read() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let (bad, other) = (ids[0], ids[1]);
    let broken = "2026-01-01T10:00:00 and more";
    for (id, t) in [(bad, broken), (other, "2026-01-01T12:00:00")] {
        s.catalog.apply(lcc_catalog::Op::SetCaptured { id, captured: Some(t.into()) }).unwrap();
    }
    s.execute("library.select", &serde_json::json!({"ids": [bad.0, other.0], "active": bad.0})).unwrap();
    assert_eq!(s.active(), Some(bad));
    let captured = |s: &crate::Session, id| s.catalog.photo(id).unwrap().captured.clone();
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [bad.0, other.0], "time": "2026-01-01T11:00:00"}));
    assert!(r.as_ref().is_err_and(|e| e.to_string().contains("can't be read")), "{r:?}");
    assert_eq!(captured(&s, bad).as_deref(), Some(broken), "refused: unchanged");
    assert_eq!(captured(&s, other).as_deref(), Some("2026-01-01T12:00:00"), "refused: the others don't move");
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [bad.0], "time": "2026-01-01T11:00:00"})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["unreadable"].as_u64()), (Some(1), Some(0)), "{r}");
    assert_eq!(captured(&s, bad).as_deref(), Some("2026-01-01T11:00:00"), "alone, it gets the time");
    s.catalog.apply(lcc_catalog::Op::SetCaptured { id: bad, captured: Some(broken.into()) }).unwrap();
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [bad.0, other.0], "time": "2026-01-01T11:00:00", "each": true})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["unreadable"].as_u64()), (Some(2), Some(0)), "{r}");
    assert_eq!(captured(&s, bad).as_deref(), Some("2026-01-01T11:00:00"), "each repairs it");
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
    use lcc_catalog::{Album, AlbumId, Op};
    let mut s = crate::Session::with_demo();
    let trip = s.execute("album.create", &serde_json::json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let rules: lcc_catalog::Filter =
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
    let album = |s: &crate::Session| s.catalog.album(lcc_catalog::AlbumId(id)).unwrap().clone();
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
    assert_eq!(find(&mut s)["problems"], serde_json::json!([format!("rule 1: no collection {trip}")]));
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
    let count = |s: &crate::Session| s.catalog.album_count(lcc_catalog::AlbumId(travel));
    let want = |s: &crate::Session| {
        s.catalog
            .photos()
            .filter(|p| !p.deleted && p.meta.keywords.iter().any(|k| k.to_lowercase().contains(&keyword.to_lowercase())))
            .filter(|p| p.label != Some(lcc_catalog::ColorLabel::Red) && p.flag != lcc_catalog::Flag::Reject)
            .count()
    };
    assert_eq!(count(&s), want(&s));
    // reject one of Travel's photos: it leaves Travel
    let before = count(&s);
    let id = s.catalog.photos().find(|p| s.catalog.album_contains(lcc_catalog::AlbumId(travel), p)).map(|p| p.id).expect("a travel photo");
    s.execute("photo.flag", &serde_json::json!({"ids": [id.0], "flag": "reject"})).unwrap();
    assert_eq!(count(&s), before - 1);
    // Excluded Photos testing Travel back would include itself
    let looped = s.execute(
        "album.setRules",
        &serde_json::json!({"id": excluded, "rules": {"ruleSet": {"rules": [{"field": "album", "op": "is", "value": travel}]}}, "replace": true}),
    );
    assert!(looped.is_err_and(|e| e.to_string().contains("would make this collection include itself")), "a loop is refused");
    // agents read the album by name in the summary
    let list = s.execute("albums.list", &serde_json::json!({})).unwrap();
    let travel_json = list.as_array().unwrap().iter().find(|a| a["id"] == travel).cloned().unwrap();
    assert!(travel_json["rulesText"].as_str().unwrap_or("").contains("collection isn't “Excluded Photos”"), "{travel_json}");
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
    let before = s.catalog.album(lcc_catalog::AlbumId(b)).unwrap().smart.clone();
    let r = s.execute("album.setRules", &serde_json::json!({"id": b, "fromView": true}));
    assert!(r.is_err_and(|e| e.to_string().contains("would make this collection include itself")), "refused");
    assert_eq!(s.catalog.album(lcc_catalog::AlbumId(b)).unwrap().smart, before, "B unchanged");
    assert!(s.catalog.smart_album_problems(lcc_catalog::AlbumId(b)).is_empty());
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
    assert_eq!(s.catalog.album(lcc_catalog::AlbumId(a)).unwrap().smart.as_ref().and_then(|f| f.album), None);
}

/// Given a library whose thumbnail cache is from before (`.jpg` files in `thumbs`), when it is
/// opened, then the cache is tidied in the background with a row in the activity stack: the
/// thumbnails are taken over under a name that is no picture's, the folder is tagged as a
/// cache, and the next time the library opens there is nothing to tidy and no row.
#[test]
fn a_thumbnail_cache_from_before_is_tidied_in_the_background_when_the_library_opens() {
    let dir = temp_dir("tidy-thumbs");
    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    assert!(s.activity.list().is_empty(), "a new library has nothing to tidy");
    s.close_library().unwrap();
    drop(s);
    // the cache as an older build left it
    let thumbs = dir.join("thumbs");
    let _ = std::fs::remove_dir_all(&thumbs);
    let img = lcc_raster::Rgba8 { width: 32, height: 20, data: vec![[90, 120, 150, 255]; 640] };
    let jpeg = lcc_preview::encode_jpeg(&img).unwrap();
    let names: Vec<String> = (0..500u16).map(|i| lcc_preview::hash_bytes(&i.to_le_bytes()).to_string()).collect();
    for hex in &names {
        std::fs::create_dir_all(thumbs.join(&hex[..2])).unwrap();
        std::fs::write(thumbs.join(&hex[..2]).join(format!("{hex}.jpg")), &jpeg).unwrap();
    }

    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    // (five hundred files to rename: the row is there when the library has just opened)
    let rows = s.activity.list();
    assert!(rows.iter().any(|r| r.kind == "cache" && r.label == "Tidying the thumbnail cache" && r.cancellable), "{rows:?}");
    // (a machine busy with other tests can take a while)
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(120) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.activity.list().is_empty());
    for hex in &names {
        let shard = thumbs.join(&hex[..2]);
        assert!(shard.join(format!("{hex}.lcthumb")).is_file() && !shard.join(format!("{hex}.jpg")).exists(), "{hex}");
    }
    assert!(thumbs.join("CACHEDIR.TAG").is_file());
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    assert!(s.activity.list().is_empty(), "tidied once");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_hold_of_the_view_is_kept_with_the_library_and_holds_nothing_over_from_another() {
    let dir = temp_dir("holdview");
    let mut s = open(&dir, true);
    assert!(!s.hold_view, "off in a library that never asked for it");
    s.execute("library.holdView", &json!({"on": true})).unwrap();
    // a filter over the demo photos, and one of them edited out from under it: held
    s.execute("library.filter", &json!({"rating": 0, "flag": "none"})).unwrap();
    let shown = s.visible_cloned();
    let id = *shown.first().expect("the seeded library has unflagged photos");
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.flag", &json!({"flag": "pick"})).unwrap();
    assert!(s.visible_cloned().contains(&id));
    s.close_library().unwrap();
    drop(s);
    // opened again: the hold is on, and what was held is not (a library opens on what matches)
    let mut s = open(&dir, false);
    assert!(s.hold_view);
    s.execute("library.filter", &json!({"rating": 0, "flag": "none"})).unwrap();
    assert!(!s.visible_cloned().contains(&id));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Capture times read from file names: a name with a date sets it (plus any shift), a name
/// without one is skipped and counted, a dry run changes nothing, and it is one undo step.
#[test]
fn capture_time_from_filename() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(3).map(|p| p.id).collect();
    let names = ["IMG_20260921_143000.jpg", "DSC_1234.NEF", "IMG_20260230_120000.jpg"];
    let ops = ids
        .iter()
        .zip(names)
        .map(|(id, n)| {
            let source = s.catalog.photo(*id).unwrap().source.clone();
            lcc_catalog::Op::Relink { id: *id, file_name: n.into(), source, format: None }
        })
        .collect();
    s.commit("names", lcc_catalog::Op::Batch { ops }).unwrap();
    let before: Vec<_> = ids.iter().map(|id| s.catalog.photo(*id).unwrap().captured.clone()).collect();
    let all: Vec<_> = ids.iter().map(|i| i.0).collect();

    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": all, "fromFilename": true, "dryRun": true})).unwrap();
    assert_eq!(r["noDateInName"], 2, "{r}");
    assert_eq!(r["captured"][0], "2026-09-21T14:30:00");
    let now: Vec<_> = ids.iter().map(|id| s.catalog.photo(*id).unwrap().captured.clone()).collect();
    assert_eq!(now, before, "a dry run changes nothing");

    s.execute("photo.setCaptureTime", &serde_json::json!({"ids": all, "fromFilename": true, "hours": 2})).unwrap();
    assert_eq!(s.catalog.photo(ids[0]).unwrap().captured.as_deref(), Some("2026-09-21T16:30:00"));
    assert_eq!(s.catalog.photo(ids[1]).unwrap().captured, before[1], "no date in the name: unchanged");
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.catalog.photo(ids[0]).unwrap().captured, before[0], "one undo step");

    // nothing to read, and contradictory options, are refusals rather than silent no-ops
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": [ids[1].0], "fromFilename": true}));
    assert!(r.is_err(), "{r:?}");
    let r = s.execute("photo.setCaptureTime", &serde_json::json!({"ids": all, "fromFilename": true, "fromFile": true}));
    assert!(r.is_err(), "{r:?}");
}

/// Collections are the user-facing name of albums: `collection.*` and `collections.list` run the
/// `album.*` commands, the journal keeps the album ids, and the old ids still work.
#[test]
fn collection_ids_are_aliases_of_album_ids() {
    let mut s = Session::with_demo();
    let made = s.execute("collection.create", &json!({"name": "Keepers"})).unwrap();
    let id = made["id"].as_u64().unwrap();
    s.execute("album.rename", &json!({"id": id, "name": "Best"})).unwrap();
    let list = s.execute("collections.list", &json!({})).unwrap();
    let old = s.execute("albums.list", &json!({})).unwrap();
    assert_eq!(list, old);
    assert!(list.to_string().contains("Best"), "{list}");
    assert_eq!(crate::find_command("collection.addPhotos").map(|c| c.id), Some("album.addPhotos"));
    assert_eq!(crate::canonical_id("library.import"), "library.import");
    assert!(matches!(s.execute("collection.noSuchThing", &json!({})), Err(crate::EngineError::UnknownCommand(_))));
}

/// A library whose catalog is upgraded says so once, names the unchanged copy it kept and how to
/// go back; `library.info` reports both.
#[test]
fn an_upgraded_library_says_where_its_older_copy_is() {
    let dir = temp_dir("upgrade-notice");
    {
        let mut s = open(&dir, true);
        s.execute("album.create", &json!({"name": "Keepers"})).unwrap();
    }
    // the catalog as a format-11 build left it
    {
        let db = redb::Database::open(dir.join(lcc_catalog::db::DB_FILE)).unwrap();
        let w = db.begin_write().unwrap();
        w.open_table(redb::TableDefinition::<&str, &[u8]>::new("meta")).unwrap().insert("version", b"11".as_slice()).unwrap();
        w.commit().unwrap();
    }
    let mut s = open(&dir, false);
    let notes = s.take_library_warnings();
    let note = notes.iter().find(|n| n.contains("upgraded from format v11")).expect("the upgrade is told");
    let info = s.execute("library.info", &json!({})).unwrap();
    let kept = info["load"]["upgradeBackup"].as_str().expect("library.info names the copy").to_string();
    assert_eq!(info["load"]["upgradedFrom"], 11);
    assert!(note.contains(&kept) && note.contains("to go back"), "{note}");
    assert!(std::path::Path::new(&kept).join(lcc_catalog::db::DB_FILE).is_file());
    assert!(s.take_library_warnings().iter().all(|n| !n.contains("upgraded from")), "told once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_selection_restored_from_view_json_names_each_photo_once() {
    // `view.json` is a plain file a user, a sync tool or another version can leave with a photo listed twice;
    // the selection never repeats one (a count or a batch would count it twice)
    let dir = temp_dir("view-dup-selection");
    let mut s = open(&dir, true);
    let ids = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [ids[1].0, ids[2].0], "active": ids[1].0})).unwrap();
    s.close_library().unwrap();
    drop(s);
    let path = dir.join("view.json");
    let mut view: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    view["selection"]["ids"] = json!([ids[1].0, ids[1].0, ids[2].0, ids[1].0]);
    std::fs::write(&path, serde_json::to_vec(&view).unwrap()).unwrap();
    let s = open(&dir, false);
    assert_eq!(s.selection.ids, [ids[1], ids[2]], "first mention kept, in order");
    assert_eq!(s.selection.active, Some(ids[1]));
    let _ = std::fs::remove_dir_all(&dir);
}
