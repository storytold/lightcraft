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
    /// may still finish the batch it was on, later: counting per instance keeps it apart from the next worker's.)
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
        let mine = Arc::new(AtomicUsize::new(0));
        calls.loads.fetch_add(1, Ordering::SeqCst);
        if towers.image {
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
    let st = cm::Settings { enabled, model: Some(id.into()), pace: pace.map(str::to_string) };
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
    assert_eq!(calls.loads.load(Ordering::SeqCst), 1, "one model instance");
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
    // (counted on the models loaded from here: the first fake's worker, let go of mid-scan, finishes the batch it was
    // on, and on a busy machine that can be after the switch back)
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
