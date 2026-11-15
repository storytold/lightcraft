//! Recognition: indexing faces, suggesting names, naming, with a tiny working network standing in for a real
//! model (its embedding depends on the picture's colours, which is enough to test the machinery; how well real
//! models recognise people is measured separately, on real photos).

use std::path::PathBuf;

use lcc_catalog::{Op, PhotoId};
use lcc_geom::Rect;
use lcc_meta::{Region, RegionKind};
use serde_json::{Value, json};

use crate::Session;

pub(crate) fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-facerec-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A demo session with a tiny model installed and chosen.
pub(crate) fn setup(d: &std::path::Path, dim: u64) -> (Session, String) {
    let mut s = Session::with_demo();
    s.face_models_dir = Some(d.join("models"));
    let model = d.join(format!("tiny{dim}.onnx"));
    std::fs::write(&model, lcc_faces::synthetic::tiny_embedder_model(dim)).unwrap();
    let r = s.execute("faces.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true, "enable": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    s.execute("faces.models.select", &json!({"id": id})).unwrap();
    (s, id)
}

/// Points the session at the real MediaPipe face model from `LC_AI_MODELS_TEST_DIR=<a models folder holding
/// it>` (what the AI Models dialog downloads; nothing is committed) and says whether it is there: the parts of a test that
/// need to find faces run only then.
pub(crate) fn install_face_model(s: &mut Session) -> bool {
    let Some(root) = std::env::var_os("LC_AI_MODELS_TEST_DIR") else {
        lcc_testkit::skip!(Model, "LC_AI_MODELS_TEST_DIR is not set, so the face-finding part is not checked");
        return false;
    };
    s.segmenter.models_root = Some(root.into());
    crate::cmd::face_detect::detector(s).is_ok()
}

pub(crate) fn region(x: f64, name: Option<&str>) -> Region {
    Region { rect: Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 }, kind: RegionKind::Face, name: name.map(str::to_string), description: None }
}

pub(crate) fn set_regions(s: &mut Session, id: PhotoId, regions: Vec<Region>) {
    let mut meta = s.catalog.photo(id).unwrap().meta.clone();
    meta.regions = regions;
    s.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
}

pub(crate) fn two_photos(s: &Session) -> (PhotoId, PhotoId) {
    let mut ids = s.catalog.photos().map(|p| p.id).collect::<Vec<_>>();
    ids.sort();
    (ids[0], ids[1])
}

#[test]
fn indexing_embeds_each_face_once_and_the_scan_row_starts_over_in_another_library() {
    let d = temp("index");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.3, None)]);
    // only a report
    let r = s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    assert_eq!((r["embedded"].clone(), r["photosDone"].clone(), r["pendingPhotos"].clone()), (json!(0), json!(0), json!(2)), "{r}");
    // a generous budget does it all, the named photo first
    let r = s.execute("faces.index", &json!({"budgetMs": 60_000})).unwrap();
    assert_eq!((r["embedded"].clone(), r["pendingPhotos"].clone(), r["indexedFaces"].clone()), (json!(3), json!(0), json!(3)), "{r}");
    // nothing is embedded twice
    let r = s.execute("faces.index", &json!({"budgetMs": 60_000})).unwrap();
    assert_eq!((r["embedded"].clone(), r["photosDone"].clone()), (json!(0), json!(0)));
    // a small budget still makes progress (at least one photo per call), and `ids` go first
    let c = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
    set_regions(&mut s, c, vec![region(0.2, None)]);
    set_regions(&mut s, b, vec![region(0.3, None), region(0.6, None)]);
    let r = s.execute("faces.index", &json!({"budgetMs": 1, "ids": [c.0]})).unwrap();
    assert_eq!(r["photosDone"], 1, "{r}");
    assert_eq!(r["pendingPhotos"], 1, "{r}");
    assert!(s.faces.index.contains(c.0, &region(0.2, None).rect), "the photo asked for first was done first");

    // issue #2995: the scan's whole was kept from the library opened before, so a smaller one showed most of it done
    s.faces.peak = 442_717;
    s.open_library(d.join("other"), true).unwrap();
    let id = s.catalog.photos().map(|p| p.id).max().unwrap();
    set_regions(&mut s, id, vec![region(0.1, None)]);
    let last = s.execute("faces.pump", &json!({})).unwrap();
    let pending = last["pendingPhotos"].as_u64().unwrap();
    assert!(pending > 0, "{last}");
    assert_eq!(last["peak"].as_u64().unwrap(), pending, "{last}");
    let rows = s.activity.list().into_iter().filter(|t| t.kind == "faces").collect::<Vec<_>>();
    assert_eq!(rows.iter().map(|t| (t.total, t.done)).collect::<Vec<_>>(), vec![(pending, 0)]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn suggestions_come_from_the_named_faces_and_are_never_applied() {
    let d = temp("suggest");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann"))]);
    set_regions(&mut s, b, vec![region(0.1, None), region(0.5, Some("Pet?")), region(0.7, None)]);
    // thresholds that accept anything: Ann is the only person with a named face except "Pet?"
    let r = s.execute("faces.suggest", &json!({"ids": [b.0], "threshold": -1.0, "margin": -2.0, "budgetMs": 60_000})).unwrap();
    assert_eq!(r["galleryFaces"], 2, "{r}");
    let faces = r["photos"][0]["faces"].as_array().unwrap();
    assert_eq!(faces.len(), 2, "only the unnamed faces get suggestions: {r}");
    assert!(faces.iter().all(|f| f["suggestion"]["name"].is_string()), "{r}");
    assert_eq!(faces[0]["index"], 0);
    assert_eq!(faces[1]["index"], 2);
    assert!(faces[0]["candidates"].as_array().unwrap().len() == 2);
    // an impossible threshold suggests nobody but still lists the candidates
    let r = s.execute("faces.suggest", &json!({"ids": [b.0], "threshold": 2.0, "budgetMs": 60_000})).unwrap();
    assert!(r["photos"][0]["faces"][0]["suggestion"].is_null());
    assert!(!r["photos"][0]["faces"][0]["candidates"].as_array().unwrap().is_empty());
    // nothing was named by suggesting
    assert!(s.catalog.photo(b).unwrap().meta.regions.iter().filter(|r| r.name.is_none()).count() == 2);
    // pets and other kinds are not people
    let mut meta = s.catalog.photo(b).unwrap().meta.clone();
    meta.regions[1].kind = RegionKind::Pet;
    s.commit("pet", Op::SetMeta { id: b, meta: Box::new(meta) }).unwrap();
    let r = s.execute("faces.suggest", &json!({"ids": [b.0], "threshold": -1.0, "margin": -2.0, "budgetMs": 60_000})).unwrap();
    assert_eq!(r["galleryFaces"], 1, "{r}");
    // odd parameters are tolerated
    for p in [json!({"threshold": "x"}), json!({"margin": null}), json!({"budgetMs": 0}), json!({"ids": []}), json!({"ids": [999999]})] {
        assert!(s.execute("faces.suggest", &p).is_ok(), "{p}");
    }
    // how many faces a match is based on: the count asked for, else the one chosen in Settings (also for null)
    for (asked, used) in [(json!(1), 1), (json!(8), 8), (json!(null), 2)] {
        let r = s.execute("faces.suggest", &json!({"ids": [b.0], "topFaces": asked, "budgetMs": 0})).unwrap();
        assert_eq!(r["topFaces"], used, "{asked}: {r}");
    }
    // and refused, as `faces.matching` refuses it, when it is no count from 1 to 8
    for bad in [json!(0), json!(9), json!(-1), json!(2.5), json!("3")] {
        for cmd in ["faces.suggest", "faces.evaluate"] {
            assert!(s.execute(cmd, &json!({"ids": [b.0], "topFaces": bad})).is_err(), "{cmd} {bad}");
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn naming_one_face_or_many_is_one_undo_step_and_makes_them_yours() {
    let d = temp("name");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    let mut detected = region(0.1, None);
    detected.description = Some("Detected by MediaPipe Face Landmarker".into());
    set_regions(&mut s, a, vec![detected, region(0.5, None)]);
    let undo = s.undo.len();
    let r = s.execute("faces.setName", &json!({"id": a.0, "index": 0, "name": "  Ann  "})).unwrap();
    assert_eq!(r["name"], "Ann");
    let regions = s.catalog.photo(a).unwrap().meta.regions.clone();
    assert_eq!((regions[0].name.as_deref(), regions[0].description.as_deref()), (Some("Ann"), None), "a named detection loses its detected marker");
    assert_eq!(s.undo.len(), undo + 1);
    // a new detection run now leaves it alone
    if install_face_model(&mut s) {
        s.execute("faces.detect", &json!({"id": a.0})).unwrap();
        assert_eq!(s.catalog.photo(a).unwrap().meta.regions.iter().filter(|r| r.name.as_deref() == Some("Ann")).count(), 1);
    }
    // clearing, undo
    s.execute("faces.setName", &json!({"id": a.0, "index": 0, "name": null})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].name, None);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].name.as_deref(), Some("Ann"));
    // bad input
    for p in [
        json!({"id": a.0}),
        json!({"id": a.0, "index": 9, "name": "x"}),
        json!({"id": a.0, "index": 0, "name": 5}),
        json!({"id": a.0, "index": 0, "name": "a\u{7}b"}),
        json!({"id": a.0, "index": 0, "name": "x".repeat(201)}),
        json!({"id": 999999, "index": 0, "name": "x"}),
    ] {
        assert!(s.execute("faces.setName", &p).is_err(), "{p}");
    }

    // many faces are named in one undo step
    let detected = |x: f64| Region { description: Some("Detected by MediaPipe Face Landmarker".into()), ..region(x, None) };
    set_regions(&mut s, a, vec![detected(0.1), region(0.5, None)]);
    set_regions(&mut s, b, vec![detected(0.3)]);
    let undo = s.undo.len();
    let r = s
        .execute(
            "faces.nameFaces",
            &json!({"faces": [{"photo": a.0, "index": 0}, {"photo": a.0, "index": 1}, {"photo": b.0, "index": 0}], "name": "  Jane Doe "}),
        )
        .unwrap();
    assert_eq!((r["named"].clone(), r["photos"].clone()), (json!(3), json!(2)));
    assert_eq!(s.undo.len(), undo + 1, "one step for the whole group");
    for (id, i) in [(a, 0), (a, 1), (b, 0)] {
        let reg = &s.catalog.photo(id).unwrap().meta.regions[i];
        assert_eq!(reg.name.as_deref(), Some("Jane Doe"));
        assert!(reg.description.is_none() || !reg.description.as_deref().unwrap().starts_with("Detected by"), "a named face is the user's now");
    }
    // undo restores every one of them together
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(a).unwrap().meta.regions.iter().chain(s.catalog.photo(b).unwrap().meta.regions.iter()).all(|r| r.name.is_none()));
    // an empty name clears; faces that do not exist are skipped, and with none left it is an error
    s.execute("faces.nameFaces", &json!({"faces": [{"photo": a.0, "index": 0}], "name": "Ann"})).unwrap();
    let r = s
        .execute(
            "faces.nameFaces",
            &json!({"faces": [{"photo": a.0, "index": 0}, {"photo": a.0, "index": 99}, {"photo": 987_654, "index": 0}], "name": null}),
        )
        .unwrap();
    assert_eq!(r["named"], 1);
    assert!(s.catalog.photo(a).unwrap().meta.regions[0].name.is_none());
    for bad in [
        json!({"name": "x"}),
        json!({"faces": "x", "name": "x"}),
        json!({"faces": [{"photo": a.0}], "name": "x"}),
        json!({"faces": [{"photo": 987_654, "index": 0}], "name": "x"}),
        json!({"faces": [{"photo": a.0, "index": 0}], "name": 7}),
        json!({"faces": [{"photo": a.0, "index": 0}], "name": "bad\u{7}name"}),
        json!({"faces": vec![json!({"photo": a.0, "index": 0}); 5001], "name": "x"}),
    ] {
        assert!(s.execute("faces.nameFaces", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn embeddings_are_cached_beside_the_library_and_reset_for_another_model() {
    let d = temp("persist");
    let lib = d.join("lib");
    let models = d.join("models");
    let install = |s: &mut Session, dim: u64| -> String {
        let model = d.join(format!("tiny{dim}.onnx"));
        std::fs::write(&model, lcc_faces::synthetic::tiny_embedder_model(dim)).unwrap();
        let r = s.execute("faces.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true, "enable": true})).unwrap();
        let id = r["installed"]["id"].as_str().unwrap().to_string();
        s.execute("faces.models.select", &json!({"id": id})).unwrap();
        id
    };
    let mut s = Session::new().with_fs();
    s.face_models_dir = Some(models.clone());
    s.open_library(&lib, true).unwrap();
    let (a, _) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    install(&mut s, 64);
    s.execute("faces.index", &json!({"budgetMs": 60_000})).unwrap();
    assert!(lib.join(format!("face-embeddings-{}.bin", s.faces.index.tag.split('@').next().unwrap())).is_file());
    s.close_library().unwrap();
    drop(s);

    // a new session finds the embeddings on disk and has nothing left to do
    let mut s2 = Session::new().with_fs();
    s2.face_models_dir = Some(models);
    s2.open_library(&lib, true).unwrap();
    let r = s2.execute("faces.index", &json!({"budgetMs": 60_000})).unwrap();
    assert_eq!((r["embedded"].clone(), r["indexedFaces"].clone(), r["pendingPhotos"].clone()), (json!(0), json!(2), json!(0)), "{r}");
    // choosing another model starts over: its faces are different numbers, the old ones are not used
    install(&mut s2, 32);
    let r = s2.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    assert_eq!((r["indexedFaces"].clone(), r["pendingPhotos"].clone()), (json!(0), json!(1)), "{r}");
    let r = s2.execute("faces.index", &json!({"budgetMs": 60_000})).unwrap();
    assert_eq!(r["embedded"], 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// Also the scan's activity row: one "Finding faces" row follows the photos left and goes when the scan is done.
#[test]
fn the_background_pump_indexes_by_itself_once_recognition_is_on() {
    let d = temp("pump");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.3, None)]);
    // installing the model switched recognition on; switched off, the pump does nothing, and says so
    assert_eq!(s.execute("faces.enable", &json!({"enabled": false})).unwrap()["enabled"], false);
    assert_eq!(s.execute("faces.pump", &json!({})).unwrap()["active"], false);
    assert_eq!(s.faces.index.len(), 0);
    s.execute("faces.enable", &json!({"enabled": true})).unwrap();
    // `enabled` is looked up at most once a second, so the first pump may still see "off"
    let faces_rows = |s: &Session| s.activity.list().into_iter().filter(|t| t.kind == "faces").collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let (mut last, mut shown) = (json!(null), false);
    while std::time::Instant::now() < deadline {
        last = s.execute("faces.pump", &json!({})).unwrap();
        let rows = faces_rows(&s);
        if last["pendingPhotos"].as_u64().unwrap_or(0) > 0 {
            // while photos are left: one row, not cancellable, its whole the most photos left at once
            assert_eq!(rows.len(), 1, "{last}");
            assert_eq!((rows[0].label.as_str(), rows[0].cancellable), ("Finding faces", false));
            assert_eq!(rows[0].total, last["peak"].as_u64().unwrap(), "{last}");
            assert_eq!(rows[0].done, rows[0].total - last["pendingPhotos"].as_u64().unwrap());
            shown = true;
        }
        if last["active"] == true && last["pendingPhotos"] == 0 && last["indexedFaces"] == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!((last["active"].clone(), last["pendingPhotos"].clone(), last["indexedFaces"].clone()), (json!(true), json!(0), json!(3)), "{last}");
    assert!(shown, "the scan never showed a row: {last}");
    assert_eq!(last["peak"], 0, "{last}");
    assert!(faces_rows(&s).is_empty(), "the row goes when the scan is done");
    // a new face is picked up (the catalog changed), and suggestions use what the pump embedded
    set_regions(&mut s, b, vec![region(0.3, None), region(0.6, None)]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline && s.faces.index.len() < 4 {
        s.execute("faces.pump", &json!({})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(s.faces.index.len(), 4);
    let r = s.execute("faces.suggest", &json!({"ids": [b.0], "threshold": -1.0, "margin": -2.0, "budgetMs": 0})).unwrap();
    assert_eq!(r["photos"][0]["faces"].as_array().unwrap().len(), 2);
    assert!(r["photos"][0]["faces"][0]["suggestion"]["name"].is_string(), "{r}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Pump until the index holds `faces` faces (or a minute passes); the last status.
fn pump_until(s: &mut Session, faces: usize) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline && s.faces.index.len() < faces {
        s.execute("faces.pump", &json!({"pace": "full"})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    s.execute("faces.pump", &json!({"pace": "full"})).unwrap();
    s.execute("faces.status", &json!({})).unwrap()
}

/// The scan pauses (and stays paused: it is saved), its row then offering resume and Settings ▸ Faces; it resumes
/// through the row; Clear empties the index and leaves the scan paused; Restart scans everything again.
#[test]
fn the_face_scan_pauses_resumes_clears_and_restarts() {
    let d = temp("pause");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.3, None)]);
    assert_eq!(s.execute("faces.pause", &json!({})).unwrap()["paused"], true);
    for _ in 0..30 {
        s.execute("faces.pump", &json!({"pace": "full"})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(s.faces.index.len(), 0, "nothing is embedded while paused");
    let st = s.execute("faces.status", &json!({})).unwrap();
    assert_eq!((st["state"].clone(), st["paused"].clone(), st["done"].clone()), (json!("paused"), json!(true), json!(0)), "{st}");
    assert!(st["total"].as_u64().unwrap() > 0, "{st}");
    // the pause is saved: a session started now is paused too
    assert_eq!(s.execute("faces.models.list", &json!({})).unwrap()["paused"], true);
    // the row stays while paused, with resume and the gear's page
    let rows: Vec<_> = s.activity.list().into_iter().filter(|t| t.kind == "faces").collect();
    assert_eq!(rows.len(), 1);
    let c = rows[0].controls.clone().expect("the scan's row has controls");
    assert!(c.paused);
    assert_eq!((c.pause, c.resume, c.settings_tab), ("faces.pause", "faces.resume", "faces"));
    // resumed through the row, as its button does
    s.execute("activity.pause", &json!({"id": rows[0].id, "paused": false})).unwrap();
    let st = pump_until(&mut s, 3);
    assert_eq!((st["state"].clone(), st["indexedFaces"].clone()), (json!("complete"), json!(3)), "{st}");
    // Clear: the index is empty and the scan stays paused, however long it is pumped
    let st = s.execute("faces.clearIndex", &json!({})).unwrap();
    assert_eq!((st["paused"].clone(), st["indexedFaces"].clone(), st["searchedPhotos"].clone()), (json!(true), json!(0), json!(0)), "{st}");
    for _ in 0..30 {
        s.execute("faces.pump", &json!({"pace": "full"})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(s.faces.index.len(), 0);
    assert!(s.catalog.photo(a).unwrap().meta.regions.iter().any(|r| r.name.as_deref() == Some("Ann")), "named faces stay");
    // Restart: everything is scanned again
    assert_eq!(s.execute("faces.restart", &json!({})).unwrap()["paused"], false);
    let st = pump_until(&mut s, 3);
    assert_eq!(st["indexedFaces"], 3, "{st}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_background_pump_saves_what_it_embeds_for_the_next_launch() {
    let d = temp("pumpsave");
    let (lib, models) = (d.join("lib"), d.join("models"));
    let mut s = Session::new().with_fs();
    s.face_models_dir = Some(models);
    s.open_library(&lib, true).unwrap();
    let model = d.join("tiny64.onnx");
    std::fs::write(&model, lcc_faces::synthetic::tiny_embedder_model(64)).unwrap();
    // installing switches recognition on: nothing else is asked for
    s.execute("faces.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true, "enable": true})).unwrap();
    // without the detector (not bundled) photos without faces are left unsearched
    let searching = install_face_model(&mut s);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.3, None)]);
    let photos = s.catalog.photos().count();
    let searched_photos = if searching { photos - 2 } else { 0 };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut last = json!(null);
    while std::time::Instant::now() < deadline {
        last = s.execute("faces.pump", &json!({})).unwrap();
        if last["pendingPhotos"] == 0 && s.faces.index.len() == 3 && s.faces.scanned.len() == searched_photos {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!((s.faces.index.len(), s.faces.scanned.len()), (3, searched_photos), "{last}");
    let (tag, path) = (s.faces.index.tag.clone(), s.faces.index.path.clone().unwrap());
    assert!(path.file_name().unwrap().to_string_lossy().starts_with("face-embeddings-custom-"), "one cache file per model: {}", path.display());
    // the first batch is written at once; whatever is left is written when the session ends, so a launch never redoes
    // work: neither the embeddings nor the list of photos already searched for faces
    s.close_library().unwrap();
    drop(s);
    let mut again = crate::faces_index::Index::default();
    again.reset(&tag, 64, Some(path));
    assert_eq!(again.len(), 3);
    let mut searched = crate::faces_index::Scanned::default();
    searched.reset(Some(lib.join("face-scanned.bin")));
    assert_eq!(searched.len(), searched_photos, "every photo without faces is remembered as searched");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn faces_person_lists_a_persons_faces_and_looks_for_more_even_with_background_scan_off() {
    let d = temp("person");
    let (mut s, _) = setup(&d, 64);
    s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, Some("Bob"))]);
    set_regions(&mut s, b, vec![region(0.1, None), region(0.5, Some("ann"))]);
    let unit = |parts: &[(usize, f32)]| {
        let mut v = vec![0.0f32; 64];
        for (i, x) in parts {
            v[*i] = *x;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect::<Vec<f32>>()
    };
    for (id, x, v) in [(a, 0.1, unit(&[(0, 1.0)])), (a, 0.5, unit(&[(1, 1.0)])), (b, 0.1, unit(&[(0, 0.9), (1, 0.1)])), (b, 0.5, unit(&[(0, 1.0)]))] {
        s.faces.index.insert(id.0, &region(x, None).rect, v);
    }
    // the name is matched without regard to case or surrounding spaces; every face is listed, wherever it is
    let r = s.execute("faces.person", &json!({"name": "  ANN "})).unwrap();
    assert_eq!((r["name"].clone(), r["total"].clone()), (json!("Ann"), json!(2)));
    let mut at: Vec<(u64, u64)> =
        r["confirmed"].as_array().unwrap().iter().map(|f| (f["photo"].as_u64().unwrap(), f["index"].as_u64().unwrap())).collect();
    at.sort();
    let mut want = vec![(a.0, 0), (b.0, 1)];
    want.sort();
    assert_eq!(at, want);
    assert!(r["confirmed"].as_array().unwrap().iter().any(|f| f["rect"]["x0"] == 0.1 && f["rect"]["y1"] == 0.6));
    // recognition is on (installing the model switched it on): the unnamed face that looks like Ann is offered
    assert_eq!(r["ready"], true);
    let more = r["more"].as_array().unwrap();
    assert_eq!((more.len(), more[0]["photo"].clone(), more[0]["index"].clone()), (1, json!(b.0), json!(0)), "{r}");
    assert!(more[0]["score"].as_f64().unwrap() > 0.9);
    // with background scanning switched off, suggestion review remains accessible without loading the ONNX network
    s.execute("faces.enable", &json!({"enabled": false})).unwrap();
    s.faces.embedder = None;
    let r = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    assert_eq!((r["total"].clone(), r["ready"].clone(), r["more"].as_array().unwrap().len()), (json!(2), json!(true), 1));
    assert!(s.faces.embedder.is_none(), "looking up indexed look-alikes does not load the ONNX network");
    // confirming it moves it from one list to the other (and is one undo step) even with background scanning off
    s.execute("faces.setName", &json!({"id": b.0, "index": 0, "name": "Ann"})).unwrap();
    let r = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    assert_eq!((r["total"].clone(), r["ready"].clone(), r["more"].as_array().unwrap().len()), (json!(3), json!(true), 0));
    // with no model chosen at all, `ready` is false and `more` is empty
    s.execute("faces.models.select", &json!({"id": null})).unwrap();
    s.faces.checked = None;
    let r = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    assert_eq!((r["total"].clone(), r["ready"].clone(), r["more"].as_array().unwrap().len()), (json!(3), json!(false), 0));
    // nobody by that name, and bad input, are plain answers and errors
    let r = s.execute("faces.person", &json!({"name": "Nobody"})).unwrap();
    assert_eq!((r["total"].clone(), r["confirmed"].as_array().unwrap().len(), r["ready"].clone()), (json!(0), 0, json!(false)));
    for bad in [json!({}), json!({"name": ""}), json!({"name": "   "}), json!({"name": 7})] {
        assert!(s.execute("faces.person", &bad).is_err(), "{bad}");
    }
    assert!(s.execute("faces.person", &json!({"name": "Ann", "more": u64::MAX})).is_ok());
    // without a chosen model (or a models folder) recognition says so
    for cmd in ["faces.index", "faces.suggest"] {
        let e = s.execute(cmd, &json!({})).unwrap_err().to_string();
        assert!(e.contains("no recognition model is chosen"), "{cmd}: {e}");
    }
    s.face_models_dir = None;
    assert!(s.execute("faces.index", &json!({})).is_err(), "no models folder");
    let _ = std::fs::remove_dir_all(&d);
}

fn unit64(parts: &[(usize, f32)]) -> Vec<f32> {
    let mut v = vec![0.0f32; 64];
    for (i, x) in parts {
        v[*i] = *x;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / n).collect()
}

#[test]
fn unnamed_faces_come_with_look_alikes_together_and_the_name_the_named_ones_suggest() {
    let d = temp("unnamed");
    let (mut s, _) = setup(&d, 64);
    s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    let (a, b) = two_photos(&s);
    let c = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None), region(0.7, None)]);
    set_regions(&mut s, b, vec![region(0.1, None), region(0.5, None)]);
    set_regions(&mut s, c, vec![region(0.3, None)]);
    // three faces like Ann, two like Bob (unnamed), one unembedded
    let put = |s: &mut Session, id: lcc_catalog::PhotoId, x: f64, v: Vec<f32>| s.faces.index.insert(id.0, &region(x, None).rect, v);
    put(&mut s, a, 0.1, unit64(&[(0, 1.0)])); // Ann, named
    put(&mut s, a, 0.5, unit64(&[(1, 1.0)])); // like Bob
    put(&mut s, b, 0.1, unit64(&[(0, 0.97), (2, 0.2)])); // like Ann
    put(&mut s, b, 0.5, unit64(&[(1, 0.98), (2, 0.2)])); // like Bob
    put(&mut s, c, 0.3, unit64(&[(0, 0.95), (3, 0.3)])); // like Ann
    // (a's last face, at 0.7, is not embedded yet)
    let r = s.execute("faces.unnamed", &json!({})).unwrap();
    assert_eq!((r["total"].clone(), r["ordered"].clone(), r["ready"].clone()), (json!(5), json!(true), json!(true)), "{r}");
    let faces = r["faces"].as_array().unwrap();
    let at = |f: &Value| (f["photo"].as_u64().unwrap(), f["index"].as_u64().unwrap());
    let order: Vec<(u64, u64)> = faces.iter().map(at).collect();
    // look-alikes are neighbours: the two like Bob are adjacent, and the three like Ann are adjacent; the unembedded one is last
    let pos = |x: (u64, u64)| order.iter().position(|o| *o == x).unwrap();
    assert_eq!(pos((a.0, 1)).abs_diff(pos((b.0, 1))), 1, "{order:?}");
    assert_eq!(order.last(), Some(&(a.0, 2)), "{order:?}");
    // a face the named ones recognise carries the name they suggest
    let like_ann = faces.iter().find(|f| at(f) == (b.0, 0)).unwrap();
    assert_eq!(like_ann["suggestion"]["name"], "Ann", "{like_ann}");
    assert!(faces.iter().find(|f| at(f) == (b.0, 1)).unwrap()["suggestion"].is_null(), "Bob is nobody the named faces know");
    // each face says which box to show it by; with no scan view it is its own box
    for k in ["x0", "y0", "x1", "y1"] {
        assert!((like_ann["view"][k].as_f64().unwrap() - like_ann["rect"][k].as_f64().unwrap()).abs() < 1e-3, "{like_ann}");
    }
    // a limit cuts the list, not the count
    let r = s.execute("faces.unnamed", &json!({"limit": 2})).unwrap();
    assert_eq!((r["total"].clone(), r["faces"].as_array().unwrap().len()), (json!(5), 2));
    // switched off, suggestions from indexed faces and look-alike order remain available
    s.execute("faces.enable", &json!({"enabled": false})).unwrap();
    let r = s.execute("faces.unnamed", &json!({})).unwrap();
    assert_eq!((r["total"].clone(), r["ordered"].clone(), r["ready"].clone()), (json!(5), json!(true), json!(true)));
    assert_eq!(r["faces"].as_array().unwrap().iter().find(|f| at(f) == (b.0, 0)).unwrap()["suggestion"]["name"], "Ann");
    for odd in [json!({"limit": 0}), json!({"limit": "x"}), json!({"limit": u64::MAX})] {
        assert!(s.execute("faces.unnamed", &odd).is_ok(), "{odd}");
    }

    // every scanned face is shown by the same kind of box
    // a loosely drawn box (from some other tool) and the detector's tighter box of the same face
    let loose = lcc_geom::Rect { x0: 0.2, y0: 0.1, x1: 0.8, y1: 0.9 };
    let tight = lcc_geom::Rect { x0: 0.35, y0: 0.25, x1: 0.65, y1: 0.65 };
    let mut meta = s.catalog.photo(a).unwrap().meta.clone();
    meta.regions = vec![Region { rect: loose, kind: RegionKind::Face, name: Some("Ann".into()), description: None }];
    s.commit("setup", Op::SetMeta { id: a, meta: Box::new(meta) }).unwrap();
    // before the scan has looked at it, the face is shown by its own box
    assert_eq!(s.face_view(a, loose), loose);
    s.faces.index.insert_with_view(a.0, &loose, unit64(&[(0, 1.0)]), &tight);
    let shown = s.face_view(a, loose);
    assert!((shown.x0 - tight.x0).abs() < 1e-3 && (shown.y1 - tight.y1).abs() < 1e-3, "shown by the detector's box once known: {shown:?}");
    // a face nobody has looked at, or in another photo, is shown by its own box
    let other = lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.2, y1: 0.2 };
    assert_eq!(s.face_view(a, other), other);
    // the page of the person uses it too
    let r = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    let view = &r["confirmed"][0]["view"];
    assert!((view["x0"].as_f64().unwrap() - tight.x0).abs() < 1e-3, "{r}");
    // and it survives a restart: the index file keeps it beside the embedding
    let (tag, path) = (s.faces.index.tag.clone(), d.join("view-cache.bin"));
    let mut written = crate::faces_index::Index::default();
    written.reset(&tag, 64, Some(path.clone()));
    written.insert_with_view(a.0, &loose, unit64(&[(0, 1.0)]), &tight);
    written.save().unwrap();
    drop(written);
    let mut again = crate::faces_index::Index::default();
    again.reset(&tag, 64, Some(path));
    let v = again.view(a.0, &loose).expect("the face is remembered");
    assert!((v.x0 - tight.x0).abs() < 1e-3 && (v.y0 - tight.y0).abs() < 1e-3, "{v:?}");
    let _ = std::fs::remove_dir_all(&d);
}

fn face_names(s: &Session, id: PhotoId) -> Vec<Option<String>> {
    s.catalog.photo(id).unwrap().meta.regions.iter().map(|r| r.name.clone()).collect()
}

/// Every in-library spelling of a person becomes the exact new name, including a change of case only,
/// and a person filter follows it. One undo step puts the old names back.
#[test]
fn rename_person_rewrites_every_spelling_merges_only_when_asked_and_rejects_bad_input() {
    let mut s = Session::with_demo();
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, Some("  ANN  ")), region(0.7, Some("Rex"))]);
    let mut pet = region(0.2, Some("Ann"));
    pet.kind = RegionKind::Pet;
    set_regions(&mut s, b, vec![region(0.3, Some("ann")), pet]);
    s.execute("library.filter", &json!({"person": "ANN"})).unwrap();
    let undo = s.undo.len();
    let r = s.execute("faces.renamePerson", &json!({"from": "  ann ", "to": "  Ada  "})).unwrap();
    assert_eq!((r["faces"].clone(), r["photos"].clone(), r["to"].clone(), r["merged"].clone()), (json!(3), json!(2), json!("Ada"), json!(false)));
    assert_eq!(face_names(&s, a), vec![Some("Ada".into()), Some("Ada".into()), Some("Rex".into())]);
    assert_eq!(face_names(&s, b)[0], Some("Ada".into()));
    assert_eq!(face_names(&s, b)[1], Some("Ann".into()), "a pet is not a person");
    assert_eq!(s.catalog.people().iter().find(|p| p.name.eq_ignore_ascii_case("ada")).map(|p| p.name.as_str()), Some("Ada"));
    assert_eq!(s.filter.person.as_deref(), Some("Ada"));
    assert_eq!(s.undo.len(), undo + 1);
    assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Rename Person"));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(face_names(&s, a), vec![Some("Ann".into()), Some("  ANN  ".into()), Some("Rex".into())]);
    assert_eq!(face_names(&s, b)[0], Some("ann".into()));

    // renaming onto someone else is refused until merge is asked for; then one undo step restores both people
    s.filter.person = None;
    set_regions(&mut s, a, vec![region(0.1, Some("Ann"))]);
    set_regions(&mut s, b, vec![region(0.3, Some("Bea"))]);
    let before = (face_names(&s, a), face_names(&s, b), s.undo.len());
    let err = s.execute("faces.renamePerson", &json!({"from": "Ann", "to": "Bea"})).unwrap_err();
    assert!(err.to_string().contains("already another person"), "{err}");
    assert_eq!((face_names(&s, a), face_names(&s, b), s.undo.len()), before, "a refusal changes nothing");
    let r = s.execute("faces.renamePerson", &json!({"from": "Ann", "to": "Bea", "merge": true})).unwrap();
    assert_eq!(r["merged"], true);
    assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Merge People"));
    let names: Vec<String> = s.catalog.people().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["Bea".to_string()]);
    s.execute("edit.undo", &json!({})).unwrap();
    let mut names: Vec<String> = s.catalog.people().into_iter().map(|p| p.name).collect();
    names.sort();
    assert_eq!(names, vec!["Ann".to_string(), "Bea".to_string()]);

    // more faces than `faces.person` lists are still renamed. A bad call, a missing person, or a name that is already
    // exact changes nothing, and deleted or only-browsed photos are not part of the person
    set_regions(&mut s, b, vec![]);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann"))]);
    let undo = s.undo.len();
    for p in [
        json!({}),
        json!({"from": "Ann"}),
        json!({"from": "  ", "to": "Ada"}),
        json!({"from": 1, "to": "Ada"}),
        json!({"from": "Ann", "to": ""}),
        json!({"from": "Ann", "to": 5}),
        json!({"from": "Ann", "to": "a\u{7}b"}),
        json!({"from": "Ann", "to": "x".repeat(201)}),
        json!({"from": "Nobody", "to": "Ada"}),
        json!({"from": "Ann", "to": "Ann"}),
    ] {
        let err = s.execute("faces.renamePerson", &p).unwrap_err();
        assert!(s.undo.len() == undo, "{p} changed the catalog: {err}");
    }
    assert_eq!(face_names(&s, a), vec![Some("Ann".into())]);
    let regions: Vec<_> = (0..5001).map(|i| region(0.1, Some(if i == 0 { "ANN" } else { "Ann" }))).collect();
    set_regions(&mut s, a, regions);
    set_regions(&mut s, b, vec![region(0.2, Some("Ann"))]);
    s.commit("delete", Op::SetDeleted { id: b, deleted: true }).unwrap();
    let listed = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    assert_eq!((listed["total"].clone(), listed["confirmed"].as_array().unwrap().len()), (json!(5001), 5000));
    let r = s.execute("faces.renamePerson", &json!({"from": "ann", "to": "Ada"})).unwrap();
    assert_eq!((r["faces"].clone(), r["photos"].clone()), (json!(5001), json!(1)), "the deleted photo is left alone");
    assert!(s.catalog.photo(a).unwrap().meta.regions.iter().all(|r| r.name.as_deref() == Some("Ada")));
    assert_eq!(s.catalog.photo(b).unwrap().meta.regions[0].name.as_deref(), Some("Ann"));
    let local = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
    set_regions(&mut s, local, vec![region(0.1, Some("Ada"))]);
    s.commit("browse", Op::SetLocal { id: local, local: true }).unwrap();
    let r = s.execute("faces.renamePerson", &json!({"from": "Ada", "to": "Bea"})).unwrap();
    assert_eq!(r["photos"], 1, "the browsed photo is not another person and is not renamed");
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].name.as_deref(), Some("Bea"));
    assert_eq!(s.catalog.photo(local).unwrap().meta.regions[0].name.as_deref(), Some("Ada"));
}

/// Ignoring faces is one undo step, checks the whole selection first, survives reopening the library and can be
/// restored with the face's name and box as they were.
#[test]
fn ignored_faces_survive_reopening_and_can_be_restored() {
    let d = temp("ignore");
    let lib = d.join("library");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, true).unwrap();
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Person"))]);
    set_regions(&mut s, b, vec![region(0.1, None)]);
    let rect = region(0.1, None).rect;
    let faces = json!([{"photo": a.0, "index": 0, "rect": rect}, {"photo": b.0, "index": 0, "rect": rect}]);
    let before = s.undo.len();
    let mut stale = faces.clone();
    stale[1]["rect"] = json!(region(0.3, None).rect);
    assert!(s.execute("faces.ignore", &json!({"faces": stale})).is_err());
    assert_eq!(s.undo.len(), before, "a stale selection changes nothing");
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].kind, RegionKind::Face);
    s.execute("faces.ignore", &json!({"faces": faces})).unwrap();
    assert_eq!(s.undo.len(), before + 1);
    assert!(!s.catalog.people().iter().any(|p| p.name == "Person"), "an ignored face is nobody");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].kind, RegionKind::Face);
    s.execute("faces.ignore", &json!({"faces": faces})).unwrap();
    s.close_library().unwrap();
    drop(s);

    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("faces.ignored", &json!({})).unwrap();
    assert_eq!(r["total"], 2);
    assert_eq!(r["faces"][0]["name"], "Person");
    s.execute("faces.ignore", &json!({"faces": faces, "ignored": false})).unwrap();
    assert_eq!(s.execute("faces.ignored", &json!({})).unwrap()["total"], 0);
    let restored = &s.catalog.photo(a).unwrap().meta.regions[0];
    assert_eq!((restored.kind.clone(), restored.name.as_deref(), restored.rect), (RegionKind::Face, Some("Person"), rect));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

/// Bad input is an error that changes nothing, never a panic.
#[test]
fn ignore_faces_refuses_bad_input_and_ignored_faces_follow_renames() {
    let mut s = Session::with_demo();
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, None)]);
    let before = s.undo.len();
    for p in [
        json!({}),
        json!({"faces": []}),
        json!({"faces": "x"}),
        json!({"faces": [{"photo": a.0, "index": 0}], "ignored": "false"}),
        json!({"faces": [{"photo": a.0}]}),
        json!({"faces": [{"photo": a.0, "index": -1}]}),
        json!({"faces": [{"photo": a.0, "index": u64::MAX}]}),
        json!({"faces": [{"photo": 999_999_999, "index": 0}]}),
        json!({"faces": [{"photo": a.0, "index": 0, "rect": "nope"}]}),
        json!({"faces": [{"photo": a.0, "index": 0}], "ignored": false}),
    ] {
        assert!(s.execute("faces.ignore", &p).is_err(), "{p}");
    }
    for p in [json!({"limit": 0}), json!({"limit": 5001}), json!({"limit": -1}), json!({"offset": -1}), json!({"offset": "a"})] {
        assert!(s.execute("faces.ignored", &p).is_err(), "{p}");
    }
    assert_eq!(s.undo.len(), before);
    assert!(s.execute("faces.ignored", &json!({"offset": u64::MAX})).unwrap()["faces"].as_array().unwrap().is_empty());

    // an ignored face keeps its person's name through renames and merges, and Detect Faces keeps it (its box stops the
    // face being found again)
    set_regions(&mut s, a, vec![region(0.1, Some("Ann"))]);
    set_regions(&mut s, b, vec![region(0.5, Some("Bea"))]);
    let face = json!([{"photo": a.0, "index": 0}]);
    s.execute("faces.ignore", &json!({"faces": face})).unwrap();
    let mut ignored = s.catalog.photo(a).unwrap().meta.regions[0].clone();
    assert!(!crate::cmd::face_detect::is_detected(&ignored));
    ignored.description = Some(format!("{}x", crate::cmd::face_detect::MARK));
    assert!(!crate::cmd::face_detect::is_detected(&ignored), "even one Detect Faces found");
    s.execute("faces.renamePerson", &json!({"from": "Ann", "to": "Bea", "merge": true})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].kind, RegionKind::IgnoredFace);
    s.execute("faces.ignore", &json!({"faces": face, "ignored": false})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions[0].name.as_deref(), Some("Bea"));
}

/// Issue #887: face suggestion review queue and accept/reject actions remain fully functional
/// even when automated background face recognition is turned off.
#[test]
fn face_review_queue_and_actions_remain_accessible_when_background_scan_is_off() {
    let d = temp("reviewqueue_bgoff");
    let (mut s, _) = setup(&d, 64);
    s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Alice")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.1, None), region(0.5, None)]);
    let unit = |parts: &[(usize, f32)]| {
        let mut v = vec![0.0f32; 64];
        for (i, x) in parts {
            v[*i] = *x;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect::<Vec<f32>>()
    };
    s.faces.index.insert(a.0, &region(0.1, None).rect, unit(&[(0, 1.0)])); // Alice
    s.faces.index.insert(a.0, &region(0.5, None).rect, unit(&[(1, 1.0)])); // Unnamed, distinct
    s.faces.index.insert(b.0, &region(0.1, None).rect, unit(&[(0, 0.95)])); // Unnamed, matches Alice
    s.faces.index.insert(b.0, &region(0.5, None).rect, unit(&[(2, 1.0)])); // Unnamed, distinct

    // Toggle background scanning OFF to save system resources, and drop the loaded ONNX network as on a fresh launch
    assert_eq!(s.execute("faces.enable", &json!({"enabled": false})).unwrap()["enabled"], false);
    s.faces.embedder = None;
    s.faces.checked = None;

    // 1. Background indexing pump does NOT run any workers or load the ONNX network
    let pump_res = s.execute("faces.pump", &json!({})).unwrap();
    assert_eq!(pump_res["active"], false);
    assert_eq!(pump_res["indexedFaces"], 4);
    assert!(s.faces.embedder.is_none(), "background pump with recognition off does not load the ONNX network");

    // 2. Person review queue ("More" section) is accessible and returns candidate suggestions without loading the network
    let person_res = s.execute("faces.person", &json!({"name": "Alice"})).unwrap();
    assert_eq!(person_res["ready"], true);
    let more = person_res["more"].as_array().unwrap();
    assert_eq!(more.len(), 1);
    assert_eq!(more[0]["photo"], b.0);
    assert_eq!(more[0]["index"], 0);
    assert!(more[0]["score"].as_f64().unwrap() > 0.9);
    assert!(s.faces.embedder.is_none());

    // 3. Review queue actions: confirm one candidate via faces.setName
    s.execute("faces.setName", &json!({"id": b.0, "index": 0, "name": "Alice"})).unwrap();
    let person_after_confirm = s.execute("faces.person", &json!({"name": "Alice"})).unwrap();
    assert_eq!(person_after_confirm["total"], 2);
    assert_eq!(person_after_confirm["more"].as_array().unwrap().len(), 0);

    // 4. Undo and test batch reject via faces.ignore
    s.execute("edit.undo", &json!({})).unwrap();
    let person_after_undo = s.execute("faces.person", &json!({"name": "Alice"})).unwrap();
    assert_eq!(person_after_undo["more"].as_array().unwrap().len(), 1);
    s.execute("faces.ignore", &json!({"faces": [{"photo": b.0, "index": 0}]})).unwrap();
    let person_after_ignore = s.execute("faces.person", &json!({"name": "Alice"})).unwrap();
    assert_eq!(person_after_ignore["more"].as_array().unwrap().len(), 0);

    // 5. Unnamed faces list and loupe hints (`faces.suggest {budgetMs: 0}`) still provide suggestions without loading ONNX
    s.execute("edit.undo", &json!({})).unwrap(); // restore ignored face
    let unnamed_res = s.execute("faces.unnamed", &json!({})).unwrap();
    assert_eq!(unnamed_res["ready"], true);
    assert_eq!(unnamed_res["ordered"], true);
    let suggestion = unnamed_res["faces"].as_array().unwrap().iter().find(|f| f["photo"] == b.0 && f["index"] == 0).unwrap();
    assert_eq!(suggestion["suggestion"]["name"], "Alice");
    let suggest_res = s.execute("faces.suggest", &json!({"ids": [b.0], "budgetMs": 0})).unwrap();
    assert_eq!(suggest_res["photos"][0]["faces"][0]["suggestion"]["name"], "Alice");
    assert!(s.faces.embedder.is_none(), "read-only queries over cached embeddings leave the ONNX network unloaded");

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unnamed_and_suggested_faces_follow_the_library_filter_when_scoped() {
    let d = temp("scoped");
    let (mut s, _) = setup(&d, 64);
    s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
    let (a, b) = two_photos(&s);
    let c = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.1, None), region(0.5, Some("Ann"))]);
    set_regions(&mut s, c, vec![region(0.3, None)]);
    let put = |s: &mut Session, id: lcc_catalog::PhotoId, x: f64, v: Vec<f32>| s.faces.index.insert(id.0, &region(x, None).rect, v);
    put(&mut s, a, 0.1, unit64(&[(0, 1.0)])); // Ann
    put(&mut s, a, 0.5, unit64(&[(0, 0.97), (2, 0.2)])); // like Ann, in a
    put(&mut s, b, 0.1, unit64(&[(0, 0.96), (3, 0.2)])); // like Ann, in b
    put(&mut s, b, 0.5, unit64(&[(0, 0.99), (1, 0.1)])); // Ann
    put(&mut s, c, 0.3, unit64(&[(0, 0.95), (4, 0.3)])); // like Ann, in c
    let photos = |r: &Value, key: &str| {
        let mut v: Vec<u64> = r[key].as_array().unwrap().iter().map(|f| f["photo"].as_u64().unwrap()).collect();
        v.sort();
        v.dedup();
        v
    };
    // the sidebar picks photos b and c (say one year); a person filter is set too, and is left aside
    s.filter.only = vec![b, c];
    s.filter.person = Some("Ann".into());
    let mut bc = vec![b.0, c.0];
    bc.sort();
    // unscoped, everything is listed as before
    let r = s.execute("faces.unnamed", &json!({})).unwrap();
    assert_eq!(r["total"], 3, "{r}");
    // scoped, only the unnamed faces in b and c
    let r = s.execute("faces.unnamed", &json!({"scope": "filter"})).unwrap();
    assert_eq!((r["total"].clone(), photos(&r, "faces")), (json!(2), bc.clone()), "{r}");
    // a person's page: their faces and the suggestions follow the scope, the count stays the library's, and Ann's
    // face in a still teaches the match (with background recognition off, too)
    s.execute("faces.enable", &json!({"enabled": false})).unwrap();
    let r = s.execute("faces.person", &json!({"name": "Ann", "scope": "filter"})).unwrap();
    assert_eq!(r["total"], 2, "{r}");
    assert_eq!(photos(&r, "confirmed"), vec![b.0], "{r}");
    assert_eq!(photos(&r, "more"), bc, "{r}");
    let r = s.execute("faces.person", &json!({"name": "Ann"})).unwrap();
    assert_eq!(r["more"].as_array().unwrap().len(), 3, "{r}");
    // an odd scope is an error, not a panic
    for bad in [json!("visible"), json!(7), json!({})] {
        assert!(s.execute("faces.unnamed", &json!({"scope": bad})).is_err(), "{bad}");
        assert!(s.execute("faces.person", &json!({"name": "Ann", "scope": bad})).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn mirror_averaging_is_on_by_default_and_can_be_turned_off() {
    let d = temp("mirror_option");
    let (mut s, _) = setup(&d, 64);
    assert_eq!(s.execute("faces.matching", &json!({})).unwrap()["mirrorAverage"], true);
    assert_eq!(s.execute("faces.matching", &json!({"mirrorAverage": false})).unwrap()["mirrorAverage"], false);
    assert!(s.execute("faces.matching", &json!({"mirrorAverage": "no"})).is_err());
    assert_eq!(s.execute("faces.matching", &json!({})).unwrap()["mirrorAverage"], false, "the choice is kept");
    assert_eq!(s.execute("faces.matching", &json!({"mirrorAverage": true})).unwrap()["mirrorAverage"], true);
}
