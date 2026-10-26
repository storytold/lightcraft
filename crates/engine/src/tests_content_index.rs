//! Indexing photo contents and searching them: the background scan of a library, the index file beside it, stale and
//! deleted photos, cancel, model switches, and the searches, with a small stand-in model (its embedding is the
//! picture's colour cast, which is enough to test the machinery without the 1.5 GB model). How well the real model
//! finds things is checked by `the_real_model_finds_the_beach`, when `LIGHTCRAFT_VISION_DIR` points at it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, PhotoId, Source};
use lightcraft_vision::{ContentModel, Manifest, Rgb8, Towers, manifest};
use serde_json::{Value, json};

use crate::Session;
use crate::content_models as cm;
use crate::content_worker::Loader;

const DIM: usize = 8;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-contentindex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// What the stand-in model was asked to do.
#[derive(Default)]
struct Calls {
    images: AtomicUsize,
    loads: AtomicUsize,
    /// Images embedded by each image model loaded, in the order they were loaded. (A worker the session let go of
    /// may still finish the photo it was on, later: counting per instance keeps it apart from the next worker's.)
    per_load: std::sync::Mutex<Vec<Arc<AtomicUsize>>>,
}

impl Calls {
    /// Image models loaded so far.
    fn loaded(&self) -> usize {
        self.per_load.lock().unwrap().len()
    }

    /// Images embedded by the image models loaded after the first `n`.
    fn images_since_load(&self, n: usize) -> usize {
        self.per_load.lock().unwrap().iter().skip(n).map(|c| c.load(Ordering::SeqCst)).sum()
    }
}

/// The stand-in: an image's embedding is its colour cast and brightness; a text's is the colour it names.
struct Fake {
    manifest: Manifest,
    calls: Arc<Calls>,
    /// This instance's images.
    mine: Arc<AtomicUsize>,
    /// Milliseconds per image (to have a scan in progress to cancel).
    slow: u64,
}

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    lightcraft_vision::normalize(&mut v);
    v
}

impl ContentModel for Fake {
    fn embed_images(&self, batch: &[Rgb8]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        std::thread::sleep(Duration::from_millis(self.slow * batch.len() as u64));
        self.calls.images.fetch_add(batch.len(), Ordering::SeqCst);
        self.mine.fetch_add(batch.len(), Ordering::SeqCst);
        Ok(batch
            .iter()
            .map(|img| {
                let n = img.data.len().max(1) as f32;
                let mean = |c: usize| img.data.iter().map(|p| f32::from(p[c]) / 255.0).sum::<f32>() / n;
                let (r, g, b) = (mean(0), mean(1), mean(2));
                let m = (r + g + b) / 3.0;
                normalised(vec![r - m, g - m, b - m, m - 0.4, 0.02, 0.0, 0.0, 0.0])
            })
            .collect())
    }

    fn embed_texts(&self, texts: &[&str]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|t| match *t {
                "green" => Ok(normalised(vec![-0.5, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0])),
                "blue" => Ok(normalised(vec![-0.5, -0.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0])),
                "red" => Ok(normalised(vec![1.0, -0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0])),
                // like nothing any picture is (no photo reaches the cut-off)
                "void" => Ok(normalised(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0])),
                _ => Err(lightcraft_vision::Error::Input(format!("the stand-in knows no `{t}`"))),
            })
            .collect()
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        1.0 / (1.0 + (-(10.0 * cosine - 5.0)).exp())
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

fn loader(calls: Arc<Calls>, slow: u64) -> Loader {
    Arc::new(move |_: &Path, m: &Manifest, towers: Towers| {
        // (image towers: the text tower is loaded too, for searches and the auto-tag vocabulary)
        let mine = Arc::new(AtomicUsize::new(0));
        if towers.image {
            calls.loads.fetch_add(1, Ordering::SeqCst);
            calls.per_load.lock().unwrap().push(mine.clone());
        }
        Ok(Box::new(Fake { manifest: m.clone(), calls: calls.clone(), mine, slow }) as Box<dyn ContentModel>)
    })
}

/// An installed stand-in model `id` in the content folder (its files are a few bytes; the loader ignores them).
fn install(content: &Path, id: &str) -> Manifest {
    let mut m = manifest::builtin_siglip2_b16_256();
    m.id = id.into();
    m.name = format!("Fake {id}");
    m.embedding.dim = DIM as u32;
    // (the stand-in's scores: a cosine of 0.5 and up)
    m.match_threshold = 0.5;
    let bytes = format!("weights of {id}").into_bytes();
    m.files =
        vec![manifest::FileSpec { name: "model.safetensors".into(), size: bytes.len() as u64, sha256: lightcraft_faces::hash::sha256_hex(&bytes) }];
    let home = content.join(id);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("model.safetensors"), &bytes).unwrap();
    std::fs::write(home.join(cm::MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join(cm::RECORD_FILE), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
    m
}

fn choose(content: &Path, id: &str, enabled: bool, pace: Option<&str>) {
    let st = cm::Settings { enabled, model: Some(id.into()), pace: pace.map(str::to_string), ..Default::default() };
    cm::write_settings(content, &st).unwrap();
}

/// A session on the demo library at `d/lib` (on disk), with model `fake` installed and indexing `enabled`.
fn session(d: &Path, calls: &Arc<Calls>, slow: u64, enabled: bool) -> Session {
    let content = d.join("content");
    if !content.join("fake").exists() {
        install(&content, "fake");
        choose(&content, "fake", enabled, Some("low"));
    }
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content);
    s.content.loader = Some(loader(calls.clone(), slow));
    s.open_library(d.join("lib"), true).unwrap();
    s
}

fn status(s: &mut Session) -> Value {
    s.execute("content.status", &json!({})).unwrap()
}

/// Pump until the scan has nothing left (or `until` says so); the last pump's answer.
fn pump_until(s: &mut Session, until: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let v = s.execute("content.pump", &json!({})).unwrap();
        if until(&v) {
            return v;
        }
        assert!(Instant::now() < deadline, "the scan never got there: {v}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn done(v: &Value) -> bool {
    v["pending"] == 0 && v["inFlight"] == 0 && v["total"].as_u64().unwrap_or(0) > 0
}

fn index_file(d: &Path, id: &str) -> PathBuf {
    d.join("lib").join(format!("content-embeddings-{id}.bin"))
}

/// The kind of scene a demo photo shows.
fn kind(s: &Session, id: u64) -> lightcraft_scenes::Kind {
    let Source::Demo { scene } = s.catalog.photo(PhotoId(id)).unwrap().source else { panic!("not a demo photo") };
    lightcraft_scenes::demo_library().into_iter().find(|sc| sc.id == scene).unwrap().kind
}

#[test]
fn a_scan_fills_the_index_shows_in_the_activity_stack_and_a_reopened_library_does_not_scan_again() {
    let d = temp("scan");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, true);
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    // the first pump starts the scan: a row in the activity stack, cancellable, counting photos
    let first = s.execute("content.pump", &json!({})).unwrap();
    assert_eq!(first["active"], true);
    let row = s.activity.list().into_iter().find(|t| t.kind == "content").expect("a row while indexing");
    assert_eq!((row.label.as_str(), row.cancellable, row.total as usize), ("Indexing photo contents", true, total));
    assert!(s.activity.running_cancellable().is_empty(), "quitting does not ask about it");
    let last = pump_until(&mut s, done);
    assert_eq!(last["indexed"].as_u64().unwrap() as usize, total, "{last}");
    assert!(s.activity.list().iter().all(|t| t.kind != "content"), "the row goes when the scan is done");
    assert_eq!(calls.images.load(Ordering::SeqCst), total, "each photo embedded once");
    assert_eq!(calls.loads.load(Ordering::SeqCst), 1, "one image model instance");
    let st = status(&mut s);
    assert_eq!((st["indexed"].as_u64(), st["total"].as_u64(), st["stale"].as_u64()), (Some(total as u64), Some(total as u64), Some(0)));
    assert_eq!((st["indexing"].as_bool(), st["failed"].as_u64(), st["pace"].as_str()), (Some(false), Some(0), Some("low")));
    assert!(st["msPerPhoto"].as_f64().is_some(), "{st}");
    // the file: a header and one record per photo (8 + 16 + 8 × 2 bytes)
    s.close_library().unwrap();
    let file = index_file(&d, "fake");
    let len = std::fs::metadata(&file).unwrap().len() as usize;
    assert!(len >= total * 40 && len < total * 40 + 200, "{len}");
    // the header ("LCCE", version, tag length, the tag, dimension, element type), then whole 40-byte records
    let bytes = std::fs::read(&file).unwrap();
    let header = 4 + 1 + 2 + u16::from_le_bytes([bytes[5], bytes[6]]) as usize + 4 + 1;
    assert_eq!((len - header) % 40, 0, "whole records after a {header}-byte header: {len}");
    drop(s);
    // a new session: the index is read back, nothing is embedded again
    let mut again = session(&d, &calls, 0, true);
    let st = status(&mut again);
    assert_eq!(st["indexed"].as_u64(), Some(total as u64), "{st}");
    let v = pump_until(&mut again, done);
    assert_eq!(v["indexed"].as_u64(), Some(total as u64));
    assert_eq!(calls.images.load(Ordering::SeqCst), total, "nothing was scanned again");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_replaced_original_is_queued_again_and_deleted_photos_are_compacted_away() {
    let d = temp("stale");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, true);
    pump_until(&mut s, done);
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    let mut ids: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    ids.sort();
    // a new original (Reload after the file changed): its content hash changes, so it is stale and embedded again
    let p = s.catalog.photo(ids[0]).unwrap().clone();
    let op = Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("replaced".into()),
        preview_only: None,
        preview_checked: String::new(),
    };
    s.commit("Reload", op).unwrap();
    // (an edit is not a new original: nothing to do for it)
    s.execute("develop.set", &json!({"ids": [ids[1].0], "values": {"light.exposure": 1.0}})).unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let st = status(&mut s);
    assert!(st["stale"] == 1 || st["pending"] == 1, "{st}");
    pump_until(&mut s, done);
    assert_eq!(calls.images.load(Ordering::SeqCst), total + 1, "only the replaced photo again");
    assert_eq!(status(&mut s)["stale"], 0);
    // sixteen photos deleted: dropped from the index, and once more than a fifth of the file is dead it is rewritten
    let gone: Vec<u64> = ids.iter().skip(5).take(16).map(|id| id.0).collect();
    s.execute("photo.delete", &json!({"ids": gone})).unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let v = pump_until(&mut s, |v| done(v) && v["total"].as_u64() == Some((total - 16) as u64));
    assert_eq!(v["indexed"].as_u64(), Some((total - 16) as u64));
    let r = s.execute("content.search", &json!({"text": "green", "limit": 100})).unwrap();
    assert!(r["results"].as_array().unwrap().iter().all(|x| !gone.contains(&x["id"].as_u64().unwrap())), "deleted photos are never found");
    s.close_library().unwrap();
    let st = status(&mut s);
    assert_eq!((st["index"]["records"].as_u64(), st["index"]["deadRecords"].as_u64()), (Some((total - 16) as u64), Some(0)), "{st}");
    let len = std::fs::metadata(index_file(&d, "fake")).unwrap().len() as usize;
    assert!(len < (total - 16) * 40 + 200, "compacted: {len}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn cancel_stops_after_the_batch_keeps_what_is_done_and_turning_indexing_on_resumes() {
    let d = temp("cancel");
    let calls = Arc::new(Calls::default());
    // 30 ms a photo, eight to a batch (low pace): a scan of the demo library takes a few batches
    let mut s = session(&d, &calls, 30, true);
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    pump_until(&mut s, |v| v["indexed"].as_u64().unwrap_or(0) > 0);
    let row = s.activity.list().into_iter().find(|t| t.kind == "content").unwrap();
    s.execute("activity.cancel", &json!({"id": row.id})).unwrap();
    let v = pump_until(&mut s, |v| v["stopped"] == true && v["inFlight"] == 0);
    let kept = v["indexed"].as_u64().unwrap() as usize;
    assert!(kept > 0 && kept < total, "{v}");
    assert!(s.activity.list().iter().all(|t| t.kind != "content"), "the row is gone");
    // nothing more happens while stopped
    for _ in 0..20 {
        s.execute("content.pump", &json!({})).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = status(&mut s);
    assert_eq!((st["indexed"].as_u64(), st["stopped"].as_bool(), st["indexing"].as_bool()), (Some(kept as u64), Some(true), Some(false)));
    assert_eq!(calls.images.load(Ordering::SeqCst), kept, "what was done is kept, nothing more ran");
    // what was done is saved
    s.close_library().unwrap();
    assert!(std::fs::metadata(index_file(&d, "fake")).unwrap().len() as usize >= kept * 40);
    // switched on again: it carries on from where it stopped
    s.execute("content.enable", &json!({"on": true})).unwrap();
    let v = pump_until(&mut s, done);
    assert_eq!(v["indexed"].as_u64(), Some(total as u64));
    assert_eq!(calls.images.load(Ordering::SeqCst), total, "nothing twice");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn another_model_has_its_own_index_and_switching_back_needs_no_rescan() {
    let d = temp("switch");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 20, true);
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    pump_until(&mut s, |v| v["indexed"].as_u64().unwrap_or(0) > 0);
    // switched mid-scan: the scan stops and starts over on the other model's index
    install(&d.join("content"), "other");
    s.execute("content.models.select", &json!({"id": "other"})).unwrap();
    s.execute("content.pace", &json!({"pace": "fast"})).unwrap();
    let v = pump_until(&mut s, done);
    assert_eq!(v["indexed"].as_u64(), Some(total as u64));
    let st = status(&mut s);
    assert_eq!((st["model"].as_str(), st["pace"].as_str()), (Some("other"), Some("fast")));
    assert!(st["index"]["file"].as_str().unwrap().ends_with("content-embeddings-other.bin"));
    s.close_library().unwrap();
    let first = std::fs::metadata(index_file(&d, "fake")).map(|m| m.len()).unwrap_or(0);
    assert!(index_file(&d, "other").is_file());
    // back to the first: its file is read (what it had done is kept), and the scan finishes it
    s.execute("content.models.select", &json!({"id": "fake"})).unwrap();
    let before = calls.loaded();
    let st = status(&mut s);
    assert_eq!(st["model"], "fake");
    let had = st["indexed"].as_u64().unwrap() as usize;
    assert!(had as u64 * 40 <= first, "{st}");
    pump_until(&mut s, done);
    // (counted on the models loaded from here: the first fake's worker, let go of mid-scan, may finish the photo it
    // was on later, as the render gate is shared with every other scan of the process)
    assert_eq!(calls.images_since_load(before), total - had, "only what the first model had not done");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn searches_rank_the_closest_photos_first_and_say_how_much_is_indexed() {
    let d = temp("search");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, false);
    // nothing indexed yet: an answer, empty, that says so
    let r = s.execute("content.search", &json!({"text": "green"})).unwrap();
    assert_eq!((r["results"].as_array().unwrap().len(), r["indexed"].as_u64(), r["complete"].as_bool()), (0, Some(0), Some(false)), "{r}");
    s.execute("content.enable", &json!({"on": true})).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    let r = s.execute("content.search", &json!({"text": "green", "limit": 5})).unwrap();
    let results = r["results"].as_array().unwrap();
    assert_eq!((results.len(), r["indexed"].as_u64(), r["complete"].as_bool()), (5, Some(total as u64), Some(true)), "{r}");
    let scores: Vec<f64> = results.iter().map(|x| x["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "best first: {scores:?}");
    // the first is the photo with the strongest green cast of all
    let index = s.content.index.as_ref().unwrap();
    let green = |id: u64| index.vector(id).unwrap()[1];
    let greenest = s.catalog.photos().map(|p| p.id.0).max_by(|a, b| green(*a).total_cmp(&green(*b))).unwrap();
    assert_eq!(results[0]["id"].as_u64(), Some(greenest));
    // the threshold cuts, `limit` caps
    let some = s.execute("content.search", &json!({"text": "blue", "min": 0.5})).unwrap();
    assert!(some["results"].as_array().unwrap().iter().all(|x| x["score"].as_f64().unwrap() >= 0.5));
    let all = s.execute("content.search", &json!({"text": "blue", "limit": 100000})).unwrap();
    assert_eq!(all["results"].as_array().unwrap().len(), total);
    // the text model is loaded once and kept
    let loads = calls.loads.load(Ordering::SeqCst);
    s.execute("content.search", &json!({"text": "red"})).unwrap();
    assert_eq!(calls.loads.load(Ordering::SeqCst), loads);
    // find similar: the photo itself is left out, and the closest is the same kind of scene
    let id = results[0]["id"].as_u64().unwrap();
    let sim = s.execute("content.similar", &json!({"id": id, "limit": 3})).unwrap();
    let first = sim["results"][0]["id"].as_u64().unwrap();
    assert!(first != id && sim["results"].as_array().unwrap().iter().all(|x| x["id"] != id));
    assert!(sim["results"][0]["score"].as_f64().unwrap() > 0.9, "{sim}");
    // refusals, never crashes
    for bad in [
        json!({}),
        json!({"text": ""}),
        json!({"text": "   "}),
        json!({"text": 5}),
        json!({"text": "x".repeat(600)}),
        json!({"text": "green", "min": "a"}),
    ] {
        assert!(s.execute("content.search", &bad).is_err(), "{bad}");
    }
    let e = s.execute("content.search", &json!({"text": "unknown words"})).unwrap_err().to_string();
    assert!(e.contains("knows no"), "{e}");
    assert!(s.execute("content.similar", &json!({"id": 999_999})).is_err());
    assert!(s.execute("content.reindex", &json!({"ids": "x"})).is_err());
    assert!(s.execute("content.pace", &json!({"pace": "turbo"})).is_err());
    assert_eq!(s.execute("content.pace", &json!({"pace": "fast"})).unwrap()["pace"], "fast");
    assert_eq!(status(&mut s)["pace"], "fast");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn reindex_and_clear_index_start_over_where_asked() {
    let d = temp("reindex");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, true);
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).take(3).collect();
    assert_eq!(s.execute("content.reindex", &json!({"ids": ids})).unwrap()["queued"], 3);
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    assert_eq!(calls.images.load(Ordering::SeqCst), total + 3);
    let r = s.execute("content.clearIndex", &json!({})).unwrap();
    assert!(r["file"].as_str().unwrap().ends_with("content-embeddings-fake.bin"));
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    assert_eq!(calls.images.load(Ordering::SeqCst), 2 * total + 3, "everything again");
    assert_eq!(status(&mut s)["indexed"].as_u64(), Some(total as u64));
    let _ = std::fs::remove_dir_all(&d);
}

/// A JPEG of `w × h` in a solid colour.
fn write_jpeg(path: &Path, rgb: [u8; 3]) {
    let (w, h) = (64usize, 48usize);
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data: vec![[rgb[0], rgb[1], rgb[2], 255]; w * h] };
    let bytes = lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &Default::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn unreadable_offline_and_deleted_photos_and_a_damaged_index_never_crash_the_scan() {
    let d = temp("hostile");
    let calls = Arc::new(Calls::default());
    let content = d.join("content");
    install(&content, "fake");
    choose(&content, "fake", true, Some("normal"));
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content.clone());
    s.content.loader = Some(loader(calls.clone(), 0));
    s.open_library(d.join("lib"), false).unwrap();
    let src = d.join("photos");
    std::fs::create_dir_all(&src).unwrap();
    for (name, rgb) in [("red.jpg", [200, 20, 20]), ("green.jpg", [20, 200, 20]), ("broken.jpg", [0, 0, 200]), ("away.jpg", [90, 90, 90])] {
        write_jpeg(&src.join(name), rgb);
    }
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(s.catalog.photos().count(), 4);
    // after the import: one file is damaged in place, one is gone (its drive unplugged)
    std::fs::write(src.join("broken.jpg"), b"not a jpeg at all").unwrap();
    std::fs::remove_file(src.join("away.jpg")).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let st = status(&mut s);
    assert_eq!((st["indexed"].as_u64(), st["failed"].as_u64(), st["offline"].as_u64()), (Some(2), Some(1), Some(1)), "{st}");
    // a failed photo is not tried again in a loop
    let tried = calls.images.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(2100));
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    assert_eq!(calls.images.load(Ordering::SeqCst), tried);
    assert_eq!(status(&mut s)["failed"], 1);
    // the drive is back: the photo is indexed once reindexed (or a minute later by itself)
    write_jpeg(&src.join("away.jpg"), [90, 90, 90]);
    s.execute("content.reindex", &json!({})).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    assert_eq!(status(&mut s)["indexed"].as_u64(), Some(3));
    // the index file is damaged on disk (garbage, absurd sizes) while the library is closed: rebuilt, never trusted
    s.close_library().unwrap();
    let file = index_file(&d, "fake");
    let mut junk = b"LCCE\x01\x0b\x00fake@123456".to_vec();
    junk.extend_from_slice(&u32::MAX.to_le_bytes());
    junk.extend(vec![0xff; 4000]);
    std::fs::write(&file, &junk).unwrap();
    s.open_library(d.join("lib"), false).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let st = status(&mut s);
    assert_eq!((st["indexed"].as_u64(), st["failed"].as_u64()), (Some(3), Some(1)), "rebuilt (the broken one fails again, once): {st}");
    // the index file cannot be written (a folder in its place): an error in the status, nothing lost, no crash
    s.close_library().unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap();
    s.open_library(d.join("lib"), false).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let st = status(&mut s);
    assert!(st["index"]["saveError"].as_str().is_some_and(|e| e.contains("disk full")), "{st}");
    assert_eq!(st["indexed"].as_u64(), Some(3), "still searchable in memory");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_photo_deleted_mid_batch_the_model_removed_and_the_library_closed_mid_scan_are_safe() {
    let d = temp("midscan");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 40, true);
    let total = s.catalog.photos().filter(|p| p.in_library()).count();
    s.execute("content.pump", &json!({})).unwrap();
    assert!(!s.content.in_flight.is_empty());
    // a photo in the batch being embedded is deleted: its embedding is dropped when it comes back
    let victim = *s.content.in_flight.iter().next().unwrap();
    s.execute("photo.delete", &json!({"ids": [victim.0]})).unwrap();
    pump_until(&mut s, |v| v["indexed"].as_u64().unwrap_or(0) > 0 && v["inFlight"] == 0);
    assert!(s.content.index.as_ref().unwrap().hash(victim.0).is_none(), "a deleted photo is not indexed");
    // another library opened mid-scan: what was running for the old one is dropped, its index saved
    s.execute("content.pump", &json!({})).unwrap();
    s.open_library(d.join("lib2"), true).unwrap();
    assert!(s.content.in_flight.is_empty() && s.content.index.is_none());
    assert!(index_file(&d, "fake").is_file(), "the old library's index was written");
    let v = pump_until(&mut s, |v| v["indexed"].as_u64().unwrap_or(0) > 0);
    assert!(v["total"].as_u64().unwrap() as usize == total);
    // the model removed mid-scan: the scan stops, says nothing is in use, and nothing crashes
    s.execute("content.models.remove", &json!({"id": "fake"})).unwrap();
    let v = s.execute("content.pump", &json!({})).unwrap();
    assert_eq!(v["active"], false);
    assert!(s.activity.list().iter().all(|t| t.kind != "content"));
    let st = status(&mut s);
    assert_eq!((st["model"].as_str(), st["enabled"].as_bool()), (None, Some(false)));
    assert!(s.execute("content.search", &json!({"text": "green"})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_model_that_does_not_load_stops_the_scan_once_with_a_reason() {
    let d = temp("noload");
    let content = d.join("content");
    install(&content, "fake");
    choose(&content, "fake", true, None);
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content);
    let loads = Arc::new(AtomicUsize::new(0));
    let l = loads.clone();
    s.content.loader = Some(Arc::new(move |_: &Path, _: &Manifest, _: Towers| {
        l.fetch_add(1, Ordering::SeqCst);
        Err("the weights are damaged".to_string())
    }));
    s.open_library(d.join("lib"), true).unwrap();
    let v = pump_until(&mut s, |v| v["active"] == false);
    assert_eq!(v["inFlight"], 0);
    for _ in 0..20 {
        s.execute("content.pump", &json!({})).unwrap();
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1, "not retried in a loop");
    let st = status(&mut s);
    assert!(st["error"].as_str().unwrap().contains("the weights are damaged"), "{st}");
    // switching indexing on again tries once more
    s.execute("content.enable", &json!({"on": true})).unwrap();
    pump_until(&mut s, |v| v["active"] == false);
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// The real model (`LIGHTCRAFT_VISION_DIR` = the folder with SigLIP 2 base 256's files): the demo library is indexed,
/// and "a beach" finds a beach.
#[cfg(unix)]
#[test]
fn the_real_model_finds_the_beach() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_VISION_DIR").map(PathBuf::from) else {
        eprintln!("LIGHTCRAFT_VISION_DIR is not set: skipping");
        return;
    };
    let d = temp("real");
    let content = d.join("content");
    let m = manifest::builtin_siglip2_b16_256();
    let home = content.join(&m.id);
    std::fs::create_dir_all(&home).unwrap();
    for f in &m.files {
        std::os::unix::fs::symlink(dir.join(&f.name), home.join(&f.name)).unwrap();
    }
    std::fs::write(home.join(cm::MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join(cm::RECORD_FILE), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
    choose(&content, &m.id, true, Some("fast"));
    let mut s = Session::new().with_fs();
    s.content_models_dir = Some(content);
    s.open_library(d.join("lib"), true).unwrap();
    let t = Instant::now();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let st = status(&mut s);
    eprintln!("indexed {} photos in {:.1} s ({} ms per photo)", st["indexed"], t.elapsed().as_secs_f64(), st["msPerPhoto"]);
    assert_eq!(st["indexed"], st["total"], "{st}");
    let r = s.execute("content.search", &json!({"text": "a beach", "limit": 3})).unwrap();
    let top = r["results"][0]["id"].as_u64().unwrap();
    eprintln!("{r}");
    assert_eq!(kind(&s, top), lightcraft_scenes::Kind::Beach, "{r}");
    let r = s.execute("content.search", &json!({"text": "a flower", "limit": 3})).unwrap();
    assert_eq!(kind(&s, r["results"][0]["id"].as_u64().unwrap()), lightcraft_scenes::Kind::Macro, "{r}");
    let _ = std::fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- the library view's search by contents

/// An indexed demo library with the stand-in model.
fn indexed(name: &str) -> (PathBuf, Arc<Calls>, Session) {
    let d = temp(name);
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, true);
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    (d, calls, s)
}

fn filter(s: &mut Session, p: Value) -> Value {
    s.execute("library.filter", &p).unwrap()
}

/// The stand-in's score of photo `id` for a text vector (sigmoid of 10 × cosine − 5).
fn fake_score(s: &Session, id: PhotoId, text: &[f32]) -> f32 {
    let v = s.content.index.as_ref().unwrap().vector(id.0).unwrap();
    let c: f32 = v.iter().zip(text).map(|(a, b)| a * b).sum();
    1.0 / (1.0 + (-(10.0 * c - 5.0)).exp())
}

fn green() -> Vec<f32> {
    normalised(vec![-0.5, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0])
}

#[test]
fn contents_in_the_filter_rank_the_view_and_and_with_the_other_filters() {
    let (d, _, mut s) = indexed("filter");
    let all = s.visible().len();
    let r = filter(&mut s, json!({"contents": "green"}));
    let n = r["count"].as_u64().unwrap() as usize;
    assert_eq!((r["contents"]["state"].as_str(), r["contents"]["label"].as_str()), (Some("ready"), Some("Contents: green")), "{r}");
    assert!(n > 0 && n < all, "some photos are green, not all: {n} of {all}");
    assert_eq!(r["contents"]["bestMatches"], false);
    // the view: exactly the photos at or above the model's cut-off, best first (Relevance is the sort now)
    assert_eq!(s.sort.key, lightcraft_catalog::SortKey::Relevance);
    let vis = s.visible().to_vec();
    let want: Vec<PhotoId> = {
        let mut v: Vec<(PhotoId, f32)> =
            s.catalog.photos().filter(|p| p.in_library()).map(|p| (p.id, fake_score(&s, p.id, &green()))).filter(|(_, sc)| *sc >= 0.5).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().map(|(id, _)| id).collect()
    };
    assert_eq!(vis.len(), want.len());
    let scores: Vec<f32> = vis.iter().map(|id| s.content_score(*id).unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "best first: {scores:?}");
    assert_eq!(vis.first(), want.first());
    // ANDed with a rating
    for id in vis.iter().step_by(2) {
        s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
    }
    let rated: Vec<PhotoId> = vis.iter().filter(|id| s.catalog.photo(**id).unwrap().rating >= 3).copied().collect();
    assert!(!rated.is_empty() && rated.len() < vis.len());
    let r = filter(&mut s, json!({"rating": 3}));
    assert_eq!(r["count"].as_u64().unwrap() as usize, rated.len(), "{r}");
    // the same as words in the search text, in any mode, quoted or not, `shows:` too
    filter(&mut s, json!({"contents": null, "rating": 0}));
    assert_eq!(s.content_ask(), None);
    assert_eq!(s.sort.key, lightcraft_catalog::SortKey::CaptureDate, "the sort comes back");
    assert_eq!(s.visible().len(), all);
    for text in ["rating:>=3 contents:green", "rating:>=3 contents:\"green\"", "shows:green rating:>=3"] {
        let r = filter(&mut s, json!({"text": text}));
        assert_eq!(r["count"].as_u64().unwrap() as usize, rated.len(), "{text}: {r}");
        assert_eq!(r["contents"]["label"], "Contents: green");
    }
    // another sort can still be chosen
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    let by_name = s.visible().to_vec();
    let names: Vec<String> = by_name.iter().map(|id| s.catalog.photo(*id).unwrap().file_name.to_lowercase()).collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "{names:?}");
    // clearing the search ends it (and the other words stay)
    let r = s.execute("content.clearQuery", &json!({})).unwrap();
    assert_eq!(s.filter.text, "rating:>=3");
    assert_eq!(r["count"].as_u64().unwrap() as usize, s.catalog.photos().filter(|p| p.in_library() && p.rating >= 3).count());
    assert_eq!(s.sort.key, lightcraft_catalog::SortKey::FileName, "a sort the user chose stays");
    assert!(s.execute("content.query", &json!({})).unwrap().is_null());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn more_and_fewer_move_the_cut_off_and_nothing_passing_shows_the_best_matches() {
    let (d, _, mut s) = indexed("morefewer");
    let total = s.visible().len();
    let n0 = filter(&mut s, json!({"contents": "green"}))["count"].as_u64().unwrap();
    let more = s.execute("content.more", &json!({})).unwrap();
    let n1 = more["count"].as_u64().unwrap();
    assert!(n1 >= n0 && more["contents"]["step"] == 1, "{more}");
    let back = s.execute("content.fewer", &json!({})).unwrap();
    assert_eq!(back["count"].as_u64(), Some(n0));
    // fewer and fewer: never down to nothing
    let mut last = n0;
    for _ in 0..12 {
        let r = s.execute("content.fewer", &json!({})).unwrap();
        let n = r["count"].as_u64().unwrap();
        assert!(n >= 1 && n <= last, "{r}");
        last = n;
    }
    // a new question starts again at the model's cut-off
    assert_eq!(filter(&mut s, json!({"contents": "green"}))["count"].as_u64(), Some(last), "same question, same step");
    filter(&mut s, json!({"contents": "blue"}));
    assert_eq!(s.content_query.step, 0);
    // nothing reaches the cut-off: the best matches, said so
    let r = filter(&mut s, json!({"contents": "void"}));
    assert_eq!(r["contents"]["bestMatches"], true, "{r}");
    assert_eq!(r["count"].as_u64().unwrap() as usize, total.min(200));
    let r = s.execute("content.fewer", &json!({})).unwrap();
    assert_eq!(r["count"].as_u64().unwrap() as usize, total.min(100), "{r}");
    // the cut-off is the model's: another model's manifest says another
    assert_eq!(s.content_view().unwrap().min, None);
    // without a search, More / Fewer say so
    s.execute("content.clearQuery", &json!({})).unwrap();
    assert!(s.execute("content.more", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn find_similar_contents_ranks_the_view_from_a_photo() {
    let (d, _, mut s) = indexed("similarview");
    let id = s.visible()[3];
    let r = s.execute("content.similar", &json!({"id": id.0, "show": true})).unwrap();
    let name = s.catalog.photo(id).unwrap().file_name.clone();
    assert_eq!(r["contents"]["label"].as_str(), Some(format!("Similar to {name}").as_str()), "{r}");
    let vis = s.visible().to_vec();
    assert_eq!(vis.first(), Some(&id), "the photo itself first");
    assert_eq!(vis.len(), s.catalog.photos().filter(|p| p.in_library()).count().min(100));
    let scores: Vec<f32> = vis.iter().map(|v| s.content_score(*v).unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    // Fewer: half as many
    let r = s.execute("content.fewer", &json!({})).unwrap();
    assert_eq!(r["count"].as_u64().unwrap() as usize, vis.len().min(50));
    // a text search replaces it; Clear Filters ends everything
    filter(&mut s, json!({"contents": "green"}));
    assert!(matches!(s.content_ask(), Some(crate::content_query::ContentAsk::Text(_))));
    s.execute("content.similar", &json!({"id": id.0, "show": true})).unwrap();
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.content_ask(), None);
    // the photo deleted while shown: the chip says so, nothing stale is shown
    s.execute("content.similar", &json!({"id": id.0, "show": true})).unwrap();
    s.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.visible().len(), 0);
    let v = s.content_view().unwrap();
    assert!(matches!(v.state, crate::content_query::ContentState::Problem(_)), "{v:?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_content_search_never_crashes_without_a_model_an_index_or_a_sensible_query() {
    let d = temp("searchsafe");
    let calls = Arc::new(Calls::default());
    let mut s = session(&d, &calls, 0, false);
    let all = s.visible().len();
    // the index is empty (indexing off): a problem-free answer that shows nothing and says to turn indexing on
    let r = filter(&mut s, json!({"contents": "green"}));
    assert_eq!((r["count"].as_u64(), r["contents"]["hint"]["kind"].as_str()), (Some(0), Some("turnOnIndexing")), "{r}");
    // blank, huge, emoji and quote-only queries
    for q in ["", "   ", "\"", "🦂🦂 scorpion", &"x".repeat(100_000)] {
        let r = s.execute("library.filter", &json!({"contents": q})).unwrap();
        assert!(r["count"].is_u64(), "{q:?}: {r}");
    }
    assert_eq!(s.content_query.contents.as_ref().map(|c| c.chars().count()), Some(crate::content_query::MAX_QUERY_CHARS));
    for text in ["contents:", "contents:\"", "shows: contents:\"\"", "contents:🦂"] {
        assert!(s.execute("library.filter", &json!({"contents": null, "text": text})).is_ok(), "{text}");
    }
    assert!(s.execute("library.filter", &json!({"contents": 5})).is_err());
    // a word the model cannot read: the chip says why, nothing is shown
    let r = filter(&mut s, json!({"text": "", "contents": "unknown words"}));
    assert_eq!((r["count"].as_u64(), r["contents"]["state"].as_str()), (Some(0), Some("problem")), "{r}");
    // the model removed while a search is in view: the problem, never the old ranking
    s.execute("content.enable", &json!({"on": true})).unwrap();
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let n = filter(&mut s, json!({"contents": "green"}))["count"].as_u64().unwrap();
    assert!(n > 0);
    s.execute("content.models.remove", &json!({"id": "fake"})).unwrap();
    assert_eq!(s.visible().len(), 0, "no stale ranking");
    let v = s.content_view().unwrap();
    assert!(matches!(&v.state, crate::content_query::ContentState::Problem(p) if p.contains("No content model")), "{v:?}");
    assert_eq!(v.hint, Some(crate::content_query::ContentHint::TurnOnIndexing));
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.visible().len(), all);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn another_model_switched_in_ranks_again_with_its_own_index() {
    let (d, _, mut s) = indexed("searchswitch");
    let n = filter(&mut s, json!({"contents": "green"}))["count"].as_u64().unwrap();
    assert!(n > 0);
    let content = d.join("content");
    install(&content, "other");
    choose(&content, "other", true, Some("low"));
    s.execute("content.models.select", &json!({"id": "other"})).unwrap();
    // the new model has no index yet: nothing to show (not the old model's ranking), and indexing is under way
    let r = filter(&mut s, json!({}));
    assert_eq!(r["count"].as_u64(), Some(0), "{r}");
    s.execute("content.pump", &json!({"wait": true})).unwrap();
    let r = filter(&mut s, json!({}));
    assert_eq!(r["count"].as_u64(), Some(n), "the same stand-in, indexed again, ranks the same: {r}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_search_model_loads_off_the_session_thread_and_queries_typed_meanwhile_run_when_it_is_ready() {
    let (d, calls, mut s) = indexed("textthread");
    // a search model that takes a while to load
    let slow_calls = calls.clone();
    s.set_content_model_loader(Arc::new(move |_: &Path, m: &Manifest, towers: Towers| {
        if towers.text {
            std::thread::sleep(Duration::from_millis(400));
        }
        slow_calls.loads.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Fake { manifest: m.clone(), calls: slow_calls.clone(), mine: Arc::default(), slow: 0 }) as Box<dyn ContentModel>)
    }));
    let prepared = s.execute("content.prepare", &json!({})).unwrap();
    assert_eq!(prepared["state"], "loading", "{prepared}");
    // typing while it loads: each answer comes at once, the view says it is preparing
    let t = Instant::now();
    for q in ["g", "gr", "gre", "green"] {
        let r = filter(&mut s, json!({"contents": q, "wait": false}));
        assert_eq!(r["contents"]["state"], "preparing", "{r}");
    }
    assert!(t.elapsed() < Duration::from_millis(300), "the session thread never waits: {:?}", t.elapsed());
    // the last one runs once the model is there
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.content_view().unwrap().state == crate::content_query::ContentState::Preparing {
        assert!(Instant::now() < deadline, "never ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let v = s.content_view().unwrap();
    assert_eq!((v.label.as_str(), v.state.clone()), ("Contents: green", crate::content_query::ContentState::Ready));
    assert!(!s.visible().is_empty());
    assert_eq!(s.execute("content.prepare", &json!({})).unwrap()["state"], "ready");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn opening_another_library_ends_the_search_by_contents_before_its_first_view() {
    let (d, calls, mut s) = indexed("newlibrary");
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    filter(&mut s, json!({"text": "rating:>=0 contents:green"}));
    assert_eq!(s.sort.key, lightcraft_catalog::SortKey::Relevance);
    s.open_library(d.join("other"), false).unwrap();
    assert_eq!(s.content_ask(), None);
    assert_eq!(s.filter.text, "rating:>=0", "its words leave the search text");
    assert_eq!(s.sort.key, lightcraft_catalog::SortKey::FileName, "the sort it replaced comes back");
    assert!(s.content_query.ranked.is_none());
    // the first library's index lost nothing: back there, every photo is still indexed
    s.open_library(d.join("lib"), false).unwrap();
    let st = status(&mut s);
    assert_eq!((st["indexed"].as_u64(), st["total"].as_u64().is_some_and(|t| t > 0)), (st["total"].as_u64(), true), "{st}");
    let _ = (calls, std::fs::remove_dir_all(&d));
}
