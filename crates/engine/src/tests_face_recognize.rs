//! Recognition: indexing faces, suggesting names, naming, with a tiny working network standing in for a real
//! model (its embedding depends on the picture's colours, which is enough to test the machinery; how well real
//! models recognise people is measured separately, on real photos).

use std::path::PathBuf;

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_geom::Rect;
use lightcraft_meta::{Region, RegionKind};
use serde_json::{Value, json};

use crate::Session;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-facerec-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A demo session with a tiny model installed and chosen.
fn setup(d: &std::path::Path, dim: u64) -> (Session, String) {
    let mut s = Session::with_demo();
    s.face_models_dir = Some(d.join("models"));
    let model = d.join(format!("tiny{dim}.onnx"));
    std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(dim)).unwrap();
    let r = s.execute("faces.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    s.execute("faces.models.select", &json!({"id": id})).unwrap();
    (s, id)
}

fn region(x: f64, name: Option<&str>) -> Region {
    Region { rect: Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 }, kind: RegionKind::Face, name: name.map(str::to_string), description: None }
}

fn set_regions(s: &mut Session, id: PhotoId, regions: Vec<Region>) {
    let mut meta = s.catalog.photo(id).unwrap().meta.clone();
    meta.regions = regions;
    s.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
}

fn two_photos(s: &Session) -> (PhotoId, PhotoId) {
    let mut ids = s.catalog.photos().map(|p| p.id).collect::<Vec<_>>();
    ids.sort();
    (ids[0], ids[1])
}

#[test]
fn indexing_embeds_each_face_once_and_reports_progress() {
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
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn naming_a_face_is_undoable_and_makes_it_yours() {
    let d = temp("name");
    let (mut s, _) = setup(&d, 64);
    let (a, _) = two_photos(&s);
    let mut detected = region(0.1, None);
    detected.description = Some("Detected by YuNet 2023mar".into());
    set_regions(&mut s, a, vec![detected, region(0.5, None)]);
    let undo = s.undo.len();
    let r = s.execute("faces.setName", &json!({"id": a.0, "index": 0, "name": "  Ann  "})).unwrap();
    assert_eq!(r["name"], "Ann");
    let regions = s.catalog.photo(a).unwrap().meta.regions.clone();
    assert_eq!((regions[0].name.as_deref(), regions[0].description.as_deref()), (Some("Ann"), None), "a named detection loses its detected marker");
    assert_eq!(s.undo.len(), undo + 1);
    // a new detection run now leaves it alone
    s.execute("faces.detect", &json!({"id": a.0})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().meta.regions.iter().filter(|r| r.name.as_deref() == Some("Ann")).count(), 1);
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
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn embeddings_are_cached_beside_the_library_and_reset_for_another_model() {
    let d = temp("persist");
    let lib = d.join("lib");
    let models = d.join("models");
    let install = |s: &mut Session, dim: u64| -> String {
        let model = d.join(format!("tiny{dim}.onnx"));
        std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(dim)).unwrap();
        let r = s.execute("faces.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true})).unwrap();
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
    assert!(lib.join("face-embeddings.bin").is_file());
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

#[test]
fn without_a_chosen_model_recognition_says_so() {
    let d = temp("nomodel");
    let mut s = Session::with_demo();
    assert!(s.execute("faces.index", &json!({})).is_err(), "no models folder");
    s.face_models_dir = Some(d.join("models"));
    for cmd in ["faces.index", "faces.suggest"] {
        let e = s.execute(cmd, &json!({})).unwrap_err().to_string();
        assert!(e.contains("no recognition model is chosen"), "{cmd}: {e}");
    }
    let _: Value = json!(null);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_background_pump_indexes_by_itself_once_recognition_is_on() {
    let d = temp("pump");
    let (mut s, _) = setup(&d, 64);
    let (a, b) = two_photos(&s);
    set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, None)]);
    set_regions(&mut s, b, vec![region(0.3, None)]);
    // off by default: it does nothing, and says so
    assert_eq!(s.execute("faces.pump", &json!({})).unwrap()["active"], false);
    assert_eq!(s.faces.index.len(), 0);
    s.execute("faces.enable", &json!({"enabled": true})).unwrap();
    // `enabled` is looked up at most once a second, so the first pump may still see "off"
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut last = json!(null);
    while std::time::Instant::now() < deadline {
        last = s.execute("faces.pump", &json!({})).unwrap();
        if last["active"] == true && last["pendingPhotos"] == 0 && last["indexedFaces"] == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!((last["active"].clone(), last["pendingPhotos"].clone(), last["indexedFaces"].clone()), (json!(true), json!(0), json!(3)), "{last}");
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
