//! The shared AI queue in the session (#491): its row in the activity stack, cancelling from the row, the device it
//! reports, `ml.status`, `ml.models.*`, `ml.pump`, `ml.index`, and `search.semantic`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lcc_ml::{Config, Ctx, Job, JobError, Outcome, Preference};
use serde_json::{Value, json};

use crate::Session;
use crate::ml::{Output, ROW_KIND, ROW_LABEL};

fn wait_for(mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ml-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_solid_png(path: &Path, rgba: [u8; 4]) {
    let (w, h) = (32usize, 32usize);
    let img = lcc_raster::Rgba8 { width: w, height: h, data: vec![rgba; w * h] };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A session whose queue sees a GPU called "Test GPU" (no driver touched).
fn session() -> Session {
    let mut s = Session::new();
    s.ml_queue_with(Config::default(), Box::new(|| Ok("Test GPU".into())));
    s
}

/// A job that runs until `open` is set or it is cancelled.
fn held(key: &str, open: Arc<AtomicBool>, started: Arc<AtomicBool>) -> Job<Output> {
    Job::new(key, move |ctx: &Ctx| -> Result<Output, JobError> {
        started.store(true, Ordering::SeqCst);
        while !open.load(Ordering::SeqCst) {
            ctx.check()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(Box::new(1u32))
    })
    .name("IMG_0001.CR3")
}

fn rows(s: &Session) -> Vec<crate::activity::TaskInfo> {
    s.activity.list().into_iter().filter(|t| t.kind == ROW_KIND).collect()
}

fn find_model<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["models"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or(&Value::Null)
}

#[test]
fn a_busy_queue_has_one_cancellable_row_that_goes_when_done() {
    let mut s = session();
    let (open, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    s.ml_submit(held("test:1", open.clone(), started.clone())).unwrap();
    s.ml_submit(Job::new("test:2", |_: &Ctx| Ok(Box::new(2u32) as Output))).unwrap();
    wait_for(|| started.load(Ordering::SeqCst));
    let r = rows(&s);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].label, ROW_LABEL);
    assert!(r[0].cancellable);
    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["running"], "test:1");
    assert_eq!(st["pending"], 1);
    assert_eq!(st["device"]["kind"], "gpu");

    open.store(true, Ordering::SeqCst);
    let mut done = Vec::new();
    wait_for(|| {
        done.extend(s.ml_queue().drain());
        done.len() == 2 && rows(&s).is_empty()
    });
    let two = done.iter().find(|f| f.key == "test:2").unwrap();
    match &two.outcome {
        Outcome::Done(v) => assert_eq!(v.downcast_ref::<u32>(), Some(&2)),
        o => panic!("{o:?}"),
    }
    // the batch left its summary
    let summary = s.activity.finished().into_iter().find(|b| b.kind == ROW_KIND).unwrap();
    assert_eq!((summary.done, summary.failed, summary.cancelled), (2, 0, false));
}

#[test]
fn the_rows_cancel_stops_the_batch() {
    let mut s = session();
    let (open, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    s.ml_submit(held("test:1", open, started.clone())).unwrap();
    for i in 0..3 {
        s.ml_submit(Job::new(format!("test:w{i}"), |_: &Ctx| Ok(Box::new(0u32) as Output))).unwrap();
    }
    wait_for(|| started.load(Ordering::SeqCst));
    let id = rows(&s)[0].id;
    s.activity.cancel(id).unwrap();
    let mut done = Vec::new();
    wait_for(|| {
        done.extend(s.ml_queue().drain());
        done.len() == 4 && rows(&s).is_empty()
    });
    assert!(done.iter().all(|f| matches!(f.outcome, Outcome::Cancelled)));
    let summary = s.activity.finished().into_iter().find(|b| b.kind == ROW_KIND).unwrap();
    assert!(summary.cancelled);
    // the queue takes new work afterwards
    s.ml_submit(Job::new("test:after", |_: &Ctx| Ok(Box::new(0u32) as Output))).unwrap();
    wait_for(|| s.ml_queue().drain().iter().any(|f| f.key == "test:after" && matches!(f.outcome, Outcome::Done(_))));
}

#[test]
fn a_job_failing_on_the_gpu_finishes_on_the_cpu_and_status_says_why() {
    let mut s = session();
    s.ml_submit(Job::new(
        "test:gpu",
        |ctx: &Ctx| {
            if ctx.device().is_gpu() { Err(JobError::Device("device lost".into())) } else { Ok(Box::new(()) as Output) }
        },
    ))
    .unwrap();
    wait_for(|| s.ml_queue().drain().iter().any(|f| matches!(f.outcome, Outcome::Done(_))));
    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["device"]["kind"], "cpu");
    assert!(st["device"]["whyCpu"].as_str().unwrap().contains("device lost"));
}

#[test]
fn ml_status_before_any_job_starts_nothing() {
    let mut s = Session::new();
    let st = s.execute("ml.status", &json!({})).unwrap();
    assert!(st["device"].is_null());
    assert_eq!(st["pending"], 0);
    assert_eq!(st["threaded"], false);
}

#[test]
fn ml_status_sets_device_reports_available_devices_and_refuses_junk() {
    let mut s = session();
    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["deviceChoice"], "auto");
    assert!(st["availableDevices"].is_array());
    assert!(st["availableDevices"].as_array().unwrap().iter().any(|d| d["id"] == "auto"));
    assert!(st["availableDevices"].as_array().unwrap().iter().any(|d| d["id"] == "cpu"));

    let st = s.execute("ml.status", &json!({"device": "cpu"})).unwrap();
    assert_eq!(st["deviceChoice"], "cpu");
    assert_eq!(st["preference"], "cpu");

    // a device that is not a string, or not one there is, is refused and changes nothing
    for bad in [json!({"device": 123}), json!({"device": true}), json!({"device": []}), json!({"device": {}})] {
        assert!(s.execute("ml.status", &bad).is_err(), "{bad:?}");
    }
    for bad in [json!({"device": "potato"}), json!({"device": "vulkan:10de:9999:Fake#1"})] {
        assert!(s.execute("ml.status", &bad).is_err(), "{bad:?}");
    }
    assert_eq!(s.execute("ml.status", &json!({})).unwrap()["preference"], "cpu");

    // "auto", and an empty device, go back to the automatic choice
    for auto in ["auto", ""] {
        s.execute("ml.status", &json!({"device": "cpu"})).unwrap();
        let st = s.execute("ml.status", &json!({"device": auto})).unwrap();
        assert_eq!(st["deviceChoice"], "auto", "{auto:?}");
        assert_eq!(st["preference"], "auto", "{auto:?}");
    }
}

#[test]
fn ml_status_preference_is_session_only_while_device_persists() {
    let temp = temp_dir("device-stick");
    let mut s = session();
    s.ml_models_dir = Some(temp.clone());
    // `preference` is session-only and does not write settings.json
    let st = s.execute("ml.status", &json!({"preference": "cpu"})).unwrap();
    assert_eq!(st["preference"], "cpu");
    assert_eq!(st["deviceChoice"], "cpu");

    let mut s_check = session();
    s_check.ml_models_dir = Some(temp.clone());
    let st_check = s_check.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st_check["deviceChoice"], "auto", "session-only preference does not persist across sessions");
    assert_eq!(st_check["preference"], "auto");

    // `device` persists to settings.json across sessions
    s.execute("ml.status", &json!({"device": "cpu"})).unwrap();
    let mut s2 = session();
    s2.ml_models_dir = Some(temp.clone());
    let st2 = s2.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st2["deviceChoice"], "cpu");
    assert_eq!(st2["preference"], "cpu");

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn turning_background_indexing_off_lasts_across_sessions() {
    // #1133: the Settings ▸ AI Models checkbox was forgotten on quit, so indexing resumed on the next launch
    let temp = temp_dir("enable-stick");
    let mut s = session();
    s.ml_models_dir = Some(temp.clone());
    assert_eq!(s.execute("ml.status", &json!({})).unwrap()["enabled"], true, "on by default");
    s.execute("ml.enable", &json!({"enabled": false})).unwrap();

    let mut s2 = session();
    s2.ml_models_dir = Some(temp.clone());
    let pump = s2.execute("ml.pump", &json!({})).unwrap();
    assert_eq!((pump["enabled"].clone(), pump["active"].clone()), (json!(false), json!(false)), "{pump}");
    assert_eq!(s2.execute("ml.status", &json!({})).unwrap()["enabled"], false);
    assert_eq!(s2.execute("ml.models.list", &json!({})).unwrap()["enabled"], false);

    s2.execute("ml.enable", &json!({"enabled": true})).unwrap();
    let mut s3 = session();
    s3.ml_models_dir = Some(temp.clone());
    assert_eq!(s3.execute("ml.status", &json!({})).unwrap()["enabled"], true);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn ml_status_sets_the_preference_and_refuses_junk() {
    let mut s = session();
    let st = s.execute("ml.status", &json!({"preference": "cpu"})).unwrap();
    assert_eq!(st["preference"], "cpu");
    s.ml_submit(Job::new("test:k", |ctx: &Ctx| Ok(Box::new(ctx.device().kind) as Output))).unwrap();
    wait_for(|| {
        s.ml_queue().drain().iter().any(|f| match &f.outcome {
            Outcome::Done(v) => v.downcast_ref::<lcc_ml::Kind>() == Some(&lcc_ml::Kind::Cpu),
            _ => false,
        })
    });
    for bad in [json!({"preference": "tpu"}), json!({"preference": 3})] {
        assert!(s.execute("ml.status", &bad).is_err());
    }
    assert_eq!(s.execute("ml.status", &json!({})).unwrap()["preference"], Preference::Cpu.as_str());
}

#[test]
fn ml_models_list_install_select_remove_and_tamper_rejection() {
    let d = temp_dir("models");
    let mut s = Session::new();
    s.ml_models_dir = Some(d.join("ml-models"));

    let l = s.execute("ml.models.list", &json!({})).unwrap();
    assert_eq!(l["enabled"], true);
    assert_eq!(l["runtime"], true);
    let known = find_model(&l, lcc_ml::known::CLIP_VIT_B32_ID);
    assert_eq!(known["installed"], false);
    assert_eq!(known["licence"]["spdx"], "MIT");
    assert_eq!(known["licence"]["commercial"], "allowed");

    let src = d.join("synthetic-src");
    lcc_ml::Clip::write_synthetic_dir(&src, "test-clip").unwrap();

    // Must acknowledge licence
    assert!(s.execute("ml.models.install", &json!({"path": src.to_string_lossy()})).is_err());
    assert!(s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": false})).is_err());
    assert!(s.execute("ml.models.download", &json!({"id": lcc_ml::known::CLIP_VIT_B32_ID})).is_err());
    assert!(s.execute("ml.models.download", &json!({"id": lcc_ml::known::CLIP_VIT_B32_ID, "acknowledged": false})).is_err());

    // Tampered file is rejected
    let bad_src = d.join("bad-src");
    lcc_ml::Clip::write_synthetic_dir(&bad_src, "bad-clip").unwrap();
    std::fs::write(bad_src.join("vocab.json"), b"{}").unwrap();
    assert!(s.execute("ml.models.install", &json!({"path": bad_src.to_string_lossy(), "acknowledged": true})).is_err());

    // Valid install succeeds and selects the model
    let r = s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();
    assert_eq!(r["installed"]["id"], "test-clip");
    assert_eq!(r["selected"], "test-clip");

    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["modelId"], "test-clip");
    assert_eq!(st["modelInstalled"], true);

    // Enable / disable toggle
    let r = s.execute("ml.enable", &json!({"enabled": false})).unwrap();
    assert_eq!(r["enabled"], false);
    let r = s.execute("ml.enable", &json!({"enabled": true})).unwrap();
    assert_eq!(r["enabled"], true);

    // Removing the model clears it from disk
    let r = s.execute("ml.models.remove", &json!({"id": "test-clip"})).unwrap();
    assert_eq!(r["id"], "test-clip");
    assert_eq!(r["removed"], true);
    assert!(!d.join("ml-models").join("test-clip").exists());

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ml_index_and_semantic_search_rank_matching_photos_and_persist_store() {
    let d = temp_dir("semantic");
    let lib_dir = d.join("lib");
    let models_dir = d.join("ml-models");
    let src = d.join("synthetic-src");
    lcc_ml::Clip::write_synthetic_dir(&src, "test-clip").unwrap();

    let warm_path = d.join("photos").join("sunset.png");
    let cool_path = d.join("photos").join("ocean.png");
    write_solid_png(&warm_path, [240, 30, 20, 255]);
    write_solid_png(&cool_path, [20, 50, 240, 255]);

    let mut s = Session::new().with_fs();
    s.ml_models_dir = Some(models_dir.clone());
    s.open_library(&lib_dir, false).unwrap();

    let imp = s.execute("library.import", &json!({"paths": [warm_path.to_string_lossy(), cool_path.to_string_lossy()]})).unwrap();
    assert_eq!(imp["imported"].as_array().unwrap().len(), 2, "{imp}");
    let warm_id = s.catalog.photos().find(|p| p.file_name == "sunset.png").unwrap().id;
    let cool_id = s.catalog.photos().find(|p| p.file_name == "ocean.png").unwrap().id;

    s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();

    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["indexedPhotos"], 0);
    assert_eq!(st["unindexedPhotos"], 2);

    // Index synchronously via ml.index {wait: true}
    let idx = s.execute("ml.index", &json!({"wait": true})).unwrap();
    assert_eq!(idx["indexedPhotos"], 2, "{idx}");
    assert_eq!(idx["unindexedPhotos"], 0, "{idx}");

    // Semantic search for "sunset" ranks warm photo first; "ocean" ranks cool photo first
    let sunset = s.execute("search.semantic", &json!({"text": "golden sunset"})).unwrap();
    let sunset_matches = sunset["matches"].as_array().unwrap();
    assert_eq!(sunset_matches.len(), 2, "{sunset}");
    assert_eq!(sunset_matches[0]["id"], warm_id.0, "{sunset}");
    assert!(sunset_matches[0]["score"].as_f64().unwrap() > sunset_matches[1]["score"].as_f64().unwrap());

    let ocean = s.execute("search.semantic", &json!({"text": "deep blue ocean"})).unwrap();
    let ocean_matches = ocean["matches"].as_array().unwrap();
    assert_eq!(ocean_matches.len(), 2, "{ocean}");
    assert_eq!(ocean_matches[0]["id"], cool_id.0, "{ocean}");
    assert!(ocean_matches[0]["score"].as_f64().unwrap() > ocean_matches[1]["score"].as_f64().unwrap());

    // Scope = "selected" restricts to selection
    s.selection = crate::Selection::single(cool_id);
    let scoped = s.execute("search.semantic", &json!({"text": "sunset", "scope": "selected"})).unwrap();
    let scoped_matches = scoped["matches"].as_array().unwrap();
    assert_eq!(scoped_matches.len(), 1);
    assert_eq!(scoped_matches[0]["id"], cool_id.0);

    // Invalid scope or empty text returns error
    assert!(s.execute("search.semantic", &json!({"text": "   "})).is_err());
    assert!(s.execute("search.semantic", &json!({"text": "sunset", "scope": "bogus"})).is_err());

    // Close library and verify embedding store was persisted to disk and reloads without re-indexing
    s.close_library().unwrap();
    drop(s);
    let store_path = lib_dir.join(lcc_ml::store::store_file_name("test-clip"));
    assert!(store_path.is_file(), "store file written at {}", store_path.display());

    let mut s2 = Session::new().with_fs();
    s2.ml_models_dir = Some(models_dir);
    s2.open_library(&lib_dir, false).unwrap();
    s2.execute("ml.models.select", &json!({"id": "test-clip"})).unwrap();
    let st2 = s2.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st2["indexedPhotos"], 2, "{st2}");
    assert_eq!(st2["unindexedPhotos"], 0, "{st2}");

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn content_replaced_invalidates_embedding_and_background_pump_reindexes() {
    let d = temp_dir("replace");
    let models_dir = d.join("ml-models");
    let src = d.join("synthetic-src");
    lcc_ml::Clip::write_synthetic_dir(&src, "test-clip").unwrap();

    let p1 = d.join("photos").join("one.png");
    write_solid_png(&p1, [240, 30, 20, 255]);

    let mut s = Session::new().with_fs();
    s.ml_models_dir = Some(models_dir);
    s.execute("library.import", &json!({"paths": [p1.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();

    // Pump background indexing until done
    wait_for(|| {
        let r = s.execute("ml.pump", &json!({"pace": "fast"})).unwrap();
        r["indexedPhotos"] == 1 && r["pendingPhotos"] == 0 && r["inFlight"] == 0
    });

    // Overwrite photo content on disk with the blue image and reload -> invalidates old embedding
    write_solid_png(&p1, [20, 50, 240, 255]);
    s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();

    let st = s.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["indexedPhotos"], 0, "{st}");
    assert_eq!(st["unindexedPhotos"], 1, "{st}");

    wait_for(|| {
        let r = s.execute("ml.pump", &json!({"pace": "fast"})).unwrap();
        r["indexedPhotos"] == 1 && r["pendingPhotos"] == 0 && r["inFlight"] == 0
    });

    let ocean = s.execute("search.semantic", &json!({"text": "ocean"})).unwrap();
    let sunset = s.execute("search.semantic", &json!({"text": "sunset"})).unwrap();
    assert!(
        ocean["matches"][0]["score"].as_f64().unwrap() > sunset["matches"][0]["score"].as_f64().unwrap(),
        "replaced photo now matches ocean better than sunset"
    );

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn semantic_search_filters_grid_composes_with_ratings_and_updates_as_photos_index() {
    let d = temp_dir("semantic-filter");
    let models_dir = d.join("ml-models");
    let src = d.join("synthetic-src");
    lcc_ml::Clip::write_synthetic_dir(&src, "test-clip").unwrap();

    let warm1 = d.join("photos").join("a_warm.png");
    let warm2 = d.join("photos").join("b_warm_bright.png");
    let cool1 = d.join("photos").join("c_ocean.png");
    write_solid_png(&warm1, [180, 60, 40, 255]);
    write_solid_png(&warm2, [250, 20, 10, 255]);
    write_solid_png(&cool1, [15, 40, 245, 255]);

    let mut s = Session::new().with_fs();
    s.ml_models_dir = Some(models_dir);
    s.execute("library.import", &json!({"paths": [warm1.to_string_lossy(), warm2.to_string_lossy(), cool1.to_string_lossy()]})).unwrap();
    let warm1_id = s.catalog.photos().find(|p| p.file_name == "a_warm.png").unwrap().id;
    let warm2_id = s.catalog.photos().find(|p| p.file_name == "b_warm_bright.png").unwrap().id;
    let cool1_id = s.catalog.photos().find(|p| p.file_name == "c_ocean.png").unwrap().id;

    // Rate warm1 = 4, warm2 = 2, cool1 = 5
    s.execute("photo.rate", &json!({"ids": [warm1_id.0], "rating": 4})).unwrap();
    s.execute("photo.rate", &json!({"ids": [warm2_id.0], "rating": 2})).unwrap();
    s.execute("photo.rate", &json!({"ids": [cool1_id.0], "rating": 5})).unwrap();

    // Set a semantic filter BEFORE the model is installed: shows 0 photos initially,
    // then once the model is installed and background indexing finishes, automatically encodes and populates the view!
    let f0 = s.execute("library.filter", &json!({"semantic": "golden sunset"})).unwrap();
    assert_eq!(f0["count"], 0, "before model is installed or embeddings finish, semantic filter matches 0");
    assert_eq!(s.filter.semantic.as_deref(), Some("golden sunset"));
    s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();

    wait_for(|| {
        let r = s.execute("ml.pump", &json!({"pace": "fast"})).unwrap();
        r["indexedPhotos"] == 3 && r["pendingPhotos"] == 0 && r["inFlight"] == 0
    });

    // Now the semantic filter has refreshed automatically from the newly indexed embeddings:
    // Both warm photos match "golden sunset" (above default threshold 0.15), while the blue ocean photo does not.
    // Under the default sort (CaptureDate), `visible()` ranks the stronger match (`warm2_id`) before `warm1_id`.
    let vis = s.visible().to_vec();
    assert_eq!(vis, vec![warm2_id, warm1_id], "ranked by similarity: strongest warm match first, cool excluded");

    // Composes with rating filter (`rating >= 3`): keeps only `warm1_id`
    let f_rated = s.execute("library.filter", &json!({"rating": 3, "ratingOp": "atLeast"})).unwrap();
    assert_eq!(f_rated["count"], 1);
    assert_eq!(s.visible(), &[warm1_id]);

    // Filter chips include both `Rating ≥ 3★` and `AI search: “golden sunset”`
    let chips = crate::filter_chips(&s.filter, &s.catalog);
    let ai_chip = chips.iter().find(|c| c.label.starts_with("AI search:")).expect("AI search chip present");
    assert_eq!(ai_chip.label, "AI search: “golden sunset”");

    // Removing the rating filter brings back both warm photos; switching to "deep blue ocean" via search.semantic {filter: true}
    // switches the grid to `cool1_id`
    s.execute("library.filter", &json!({"rating": 0})).unwrap();
    let ocean = s.execute("search.semantic", &json!({"text": "deep blue ocean", "filter": true})).unwrap();
    assert_eq!(ocean["filtered"], 1);
    assert_eq!(s.visible(), &[cool1_id]);

    // Clearing the AI search chip clears the semantic filter and restores all 3 photos
    let chips = crate::filter_chips(&s.filter, &s.catalog);
    let clear = chips.iter().find(|c| c.label.starts_with("AI search:")).unwrap().clear.clone();
    s.execute("library.filter", &clear).unwrap();
    assert!(s.filter.semantic.is_none());
    assert_eq!(s.visible().len(), 3);

    let _ = std::fs::remove_dir_all(&d);
}

/// AI search runs CLIP's blocks on the GPU through wgpu (any OS) and gives what the CPU gives.
#[test]
fn clip_on_the_gpu_matches_the_cpu() {
    use lcc_ml::{Encoder, Priority, Queue, RgbImage, cosine};
    if let Err(e) = lcc_gpu::clip::adapter() {
        lcc_testkit::skip!(Gpu, "no GPU for the CLIP kernels: {e}");
        return;
    }
    let dir = temp_dir("clip-gpu");
    lcc_ml::Clip::write_synthetic_dir(&dir, "test-clip").unwrap();
    let cpu = Arc::new(lcc_ml::Clip::load_lazy(&dir).unwrap());
    let gpu = Arc::new(lcc_ml::Clip::load_lazy(&dir).unwrap().with_accelerator(crate::ml::clip_accelerator()));
    let img = RgbImage::new(16, 16, (0..16 * 16 * 3).map(|i| (i * 7 % 256) as u8).collect()).unwrap();
    let run = |clip: Arc<lcc_ml::Clip>, gpu: bool| {
        let probe: lcc_ml::Probe = if gpu { Box::new(lcc_gpu::clip::adapter) } else { Box::new(|| Err("cpu".into())) };
        let pref = if gpu { Preference::Auto } else { Preference::Cpu };
        let q: Queue<(Vec<Vec<f32>>, Vec<f32>)> = Queue::new(Config { preference: pref, ..Config::default() }, probe, None);
        let im = img.clone();
        q.submit(
            Job::new("both", move |ctx| Ok((clip.encode_images(&[im.clone(), im.clone()], ctx)?, clip.encode_text("sunset", ctx)?)))
                .priority(Priority::Interactive),
        )
        .unwrap();
        let mut out = None;
        wait_for(|| {
            for f in q.drain() {
                if let Outcome::Done(v) = f.outcome {
                    out = Some(v);
                }
            }
            out.is_some()
        });
        (out.unwrap(), q.status().device.unwrap())
    };
    let ((ci, ct), _) = run(cpu, false);
    let ((gi, gt), device) = run(gpu, true);
    assert!(device.is_gpu(), "fell back to the CPU: {:?}", device.why_cpu);
    assert!(cosine(&ci[0], &gi[0]) > 0.999 && cosine(&ci[1], &gi[1]) > 0.999);
    assert!(cosine(&ct, &gt) > 0.999);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pump background indexing until `cond` holds for its result (or 30 s pass); the last result.
fn pump_ml_until(s: &mut Session, mut cond: impl FnMut(&Value) -> bool) -> Value {
    let start = Instant::now();
    loop {
        let v = s.execute("ml.pump", &json!({"pace": "full"})).unwrap();
        if cond(&v) || start.elapsed() > Duration::from_secs(30) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Search indexing pauses (saved: a new session is paused too), its row then offering resume and Settings ▸ AI Models;
/// it resumes, Clear empties the index and leaves indexing paused, Restart indexes everything again; and switching
/// semantic search off stops indexing and refuses searches, clearing a description filter.
#[test]
fn search_indexing_pauses_resumes_clears_restarts_and_semantic_search_switches_off() {
    let d = temp_dir("pause");
    let lib_dir = d.join("lib");
    let models_dir = d.join("ml-models");
    let src = d.join("synthetic-src");
    lcc_ml::Clip::write_synthetic_dir(&src, "test-clip").unwrap();
    let (warm, cool) = (d.join("photos").join("sunset.png"), d.join("photos").join("ocean.png"));
    write_solid_png(&warm, [240, 30, 20, 255]);
    write_solid_png(&cool, [20, 50, 240, 255]);
    let mut s = Session::new().with_fs();
    s.ml_models_dir = Some(models_dir.clone());
    s.open_library(&lib_dir, false).unwrap();
    s.execute("library.import", &json!({"paths": [warm.to_string_lossy(), cool.to_string_lossy()]})).unwrap();
    s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();

    // paused before anything runs: pumping indexes nothing, and a row offers resume
    assert_eq!(s.execute("ml.pause", &json!({})).unwrap()["paused"], true);
    for _ in 0..40 {
        s.execute("ml.pump", &json!({"pace": "full"})).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let st = s.execute("ml.indexStatus", &json!({})).unwrap();
    assert_eq!((st["state"].clone(), st["done"].clone(), st["total"].clone()), (json!("paused"), json!(0), json!(2)), "{st}");
    let rows: Vec<_> = s.activity.list().into_iter().filter(|t| t.kind == ROW_KIND).collect();
    assert_eq!(rows.len(), 1, "a paused index with photos left keeps its row");
    let c = rows[0].controls.clone().expect("the indexing row has controls");
    assert!(c.paused);
    assert_eq!((c.resume, c.settings_tab, c.settings_section), ("ml.resume", "ai", Some("semanticSearch")));
    let mut again = Session::new().with_fs();
    again.ml_models_dir = Some(models_dir.clone());
    assert_eq!(again.execute("ml.indexStatus", &json!({})).unwrap()["paused"], true, "the pause is saved");
    drop(again);

    // resumed through the row: it indexes to the end
    s.execute("activity.pause", &json!({"id": rows[0].id, "paused": false})).unwrap();
    let v = pump_ml_until(&mut s, |v| v["indexedPhotos"] == 2);
    assert_eq!(v["indexedPhotos"], 2, "{v}");
    assert_eq!(s.execute("ml.indexStatus", &json!({})).unwrap()["state"], "complete");

    // Clear: empty, paused, and it stays empty
    let st = s.execute("ml.clearIndex", &json!({})).unwrap();
    assert_eq!((st["paused"].clone(), st["indexedPhotos"].clone()), (json!(true), json!(0)), "{st}");
    for _ in 0..40 {
        s.execute("ml.pump", &json!({"pace": "full"})).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(s.execute("ml.indexStatus", &json!({})).unwrap()["indexedPhotos"], 0);

    // Restart: indexes everything again
    assert_eq!(s.execute("ml.restart", &json!({})).unwrap()["paused"], false);
    let v = pump_ml_until(&mut s, |v| v["indexedPhotos"] == 2);
    assert_eq!(v["indexedPhotos"], 2, "{v}");

    // semantic search switched off: a description filter goes, searches are refused, nothing is indexed
    s.execute("library.filter", &json!({"semantic": "golden sunset"})).unwrap();
    assert!(s.filter.semantic.is_some());
    s.execute("ml.enable", &json!({"enabled": false})).unwrap();
    assert!(s.filter.semantic.is_none(), "the description filter is cleared");
    assert!(s.execute("search.semantic", &json!({"text": "sunset"})).is_err());
    assert!(s.execute("library.filter", &json!({"semantic": "sunset"})).is_err());
    let v = s.execute("ml.pump", &json!({})).unwrap();
    assert_eq!((v["active"].clone(), v["enabled"].clone()), (json!(false), json!(false)), "{v}");
    assert_eq!(s.execute("ml.indexStatus", &json!({})).unwrap()["state"], "off");
    assert!(!s.semantic_search_on());
    s.execute("ml.enable", &json!({"enabled": true})).unwrap();
    assert!(s.execute("search.semantic", &json!({"text": "sunset"})).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}
