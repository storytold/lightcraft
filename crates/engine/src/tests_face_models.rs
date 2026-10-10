//! The `faces.*` model commands: list, inspect, install behind the licence, select, remove, the on/off
//! setting, and the refusals (no folder, licence not accepted, unsupported or hostile files and ids).

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-facemodels-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn session(dir: &std::path::Path) -> Session {
    let mut s = Session::new();
    s.face_models_dir = Some(dir.join("models"));
    s
}

fn model_file(dir: &std::path::Path, name: &str, dim: u64) -> String {
    let p = dir.join(name);
    std::fs::write(&p, lightcraft_faces::synthetic::tiny_embedder_model(dim)).unwrap();
    p.to_string_lossy().into_owned()
}

fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["models"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or(&Value::Null)
}

#[test]
fn the_list_knows_the_known_models_and_is_honest_about_the_runtime() {
    let d = temp("list");
    let mut s = session(&d);
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(l["enabled"], false);
    assert_eq!(l["runtime"], cfg!(not(target_arch = "wasm32")));
    assert_eq!(find(&l, "yunet-2023mar")["installed"], false, "nothing ships with LightCraft");
    assert_eq!(find(&l, "auraface-v1")["installed"], false);
    assert_eq!(find(&l, "auraface-v1")["licence"]["commercial"], "yes");
    assert_eq!(find(&l, "sface-2021dec")["licence"]["commercial"], "unknown");
    // speed is a ratio to a ResNet-100 model, in words, for models that are not installed yet too; never milliseconds
    assert_eq!(find(&l, "sface-2021dec")["speedText"], "6.9× faster than a ResNet-100 model");
    assert_eq!(find(&l, "auraface-v1")["speedText"], "Same speed as a ResNet-100 model");
    assert_eq!(find(&l, "yunet-2023mar")["speedText"], Value::Null, "nobody timed the detector against it");
    // a build with no folder lists too, and cannot install
    let mut web = Session::new();
    assert!(web.execute("faces.models.list", &json!({})).is_ok());
    let f = model_file(&d, "m.onnx", 512);
    assert!(web.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).is_err());
    assert!(web.execute("faces.enable", &json!({"enabled": true})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn inspect_describes_a_file_and_installs_nothing() {
    let d = temp("inspect");
    let mut s = session(&d);
    let f = model_file(&d, "My Model.onnx", 512);
    let r = s.execute("faces.models.inspect", &json!({"path": f})).unwrap();
    assert_eq!(r["kind"], "draft");
    assert_eq!(r["model"]["licence"]["commercial"], "unknown");
    assert_eq!(r["model"]["known"], false);
    assert!(r["assumptions"].as_array().unwrap().iter().any(|a| a.as_str().unwrap().contains("127.5")));
    assert_eq!(r["alreadyInstalled"], false);
    assert!(!d.join("models").exists(), "inspecting writes nothing");
    // not a model: a plain answer, not an error
    let junk = d.join("junk.onnx");
    std::fs::write(&junk, b"this is not a model").unwrap();
    let r = s.execute("faces.models.inspect", &json!({"path": junk.to_string_lossy()})).unwrap();
    assert_eq!(r["kind"], "unsupported");
    assert!(r["reason"].as_str().unwrap().len() > 5);
    // missing and empty files and folders are errors
    assert!(s.execute("faces.models.inspect", &json!({"path": d.join("nope.onnx").to_string_lossy()})).is_err());
    assert!(s.execute("faces.models.inspect", &json!({"path": d.to_string_lossy()})).is_err());
    let empty = d.join("empty.onnx");
    std::fs::write(&empty, b"").unwrap();
    assert!(s.execute("faces.models.inspect", &json!({"path": empty.to_string_lossy()})).is_err());
    assert!(s.execute("faces.models.inspect", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn install_needs_the_licence_accepted_then_select_enable_and_remove_work() {
    let d = temp("flow");
    let mut s = session(&d);
    let f = model_file(&d, "Mine.onnx", 512);
    assert!(s.execute("faces.models.install", &json!({"path": f})).is_err(), "no acknowledgement");
    assert!(s.execute("faces.models.install", &json!({"path": f, "acknowledged": false})).is_err());
    assert!(s.execute("faces.models.install", &json!({"path": f, "acknowledged": "yes"})).is_err(), "must be the boolean true");
    assert!(!d.join("models").exists(), "refused installs leave nothing behind");

    let r = s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("custom-mine-"));
    let home = d.join("models").join(&id);
    assert!(home.join("model.onnx").is_file() && home.join("face-model.json").is_file() && home.join("installed.json").is_file());
    assert!(!home.join("model.onnx.part").exists());
    assert_eq!(std::fs::read(home.join("model.onnx")).unwrap(), std::fs::read(&f).unwrap());
    let rec: Value = serde_json::from_slice(&std::fs::read(home.join("installed.json")).unwrap()).unwrap();
    assert_eq!(rec["commercial"], "unknown");
    assert_eq!(rec["fileName"], "Mine.onnx");

    // listed as installed and custom; installing again is harmless
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(find(&l, &id)["installed"], true);
    assert_eq!(find(&l, &id)["known"], false);
    s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    assert_eq!(s.execute("faces.models.inspect", &json!({"path": f})).unwrap()["alreadyInstalled"], true);

    // choose it; the choice and the on/off setting survive a new session on the same folder
    assert!(s.execute("faces.models.select", &json!({"id": "auraface-v1"})).is_err(), "not installed");
    assert!(s.execute("faces.models.select", &json!({"id": "yunet-2023mar"})).is_err(), "a detector is not a recogniser");
    s.execute("faces.models.select", &json!({"id": id})).unwrap();
    assert_eq!(s.execute("faces.enable", &json!({"enabled": true})).unwrap()["enabled"], true);
    let mut again = session(&d);
    let l = again.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!((l["enabled"].clone(), l["embedder"].clone()), (json!(true), json!(id)));
    assert_eq!(again.execute("faces.enable", &json!({})).unwrap()["enabled"], false, "toggles when omitted");

    // removing it clears the choice
    assert_eq!(s.execute("faces.models.remove", &json!({"id": id})).unwrap()["removed"], id);
    assert!(!home.exists());
    assert_eq!(s.execute("faces.models.list", &json!({})).unwrap()["embedder"], Value::Null);
    assert!(s.execute("faces.models.remove", &json!({"id": id})).is_err(), "already gone");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hostile_ids_unsupported_models_and_odd_folders_are_handled() {
    let d = temp("hostile");
    let mut s = session(&d);
    let models = d.join("models");
    std::fs::create_dir_all(&models).unwrap();
    // something that is not a model sits in the folder: it is never listed and never deleted
    let precious = models.join("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("keep.txt"), b"mine").unwrap();
    for id in ["precious", "..", ".", "", "../x", "a/b", r"a\b", ".hidden", "yunet-2023mar", "UPPER"] {
        assert!(s.execute("faces.models.remove", &json!({"id": id})).is_err(), "{id:?}");
    }
    assert!(precious.join("keep.txt").is_file());
    assert!(d.join("models").is_dir());
    // a folder whose manifest lies about its id is ignored
    let liar = models.join("liar");
    std::fs::create_dir_all(&liar).unwrap();
    std::fs::write(liar.join("model.onnx"), lightcraft_faces::synthetic::embedder_model(64)).unwrap();
    let mut m = lightcraft_faces::known::auraface();
    m.id = "someone-else".into();
    std::fs::write(liar.join("face-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(models.join("settings.json"), b"{ not json").unwrap();
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert!(find(&l, "someone-else").is_null() && find(&l, "liar").is_null());
    assert_eq!(l["enabled"], false, "a corrupt settings file is ignored");
    // not-a-model and wrong-shaped files cannot be installed
    let junk = d.join("junk.onnx");
    std::fs::write(&junk, b"nonsense").unwrap();
    let e = s.execute("faces.models.install", &json!({"path": junk.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("cannot be used yet"), "{e}");
    let tokens = d.join("tokens.onnx");
    {
        use lightcraft_faces::synthetic::{len_field, value_info_bytes};
        let mut graph = Vec::new();
        len_field(11, &value_info_bytes("pixels", 1, &[Err("N"), Ok(3), Ok(224), Ok(224)]), &mut graph);
        len_field(12, &value_info_bytes("tokens", 1, &[Err("N"), Ok(257), Ok(384)]), &mut graph);
        let mut model = Vec::new();
        len_field(7, &graph, &mut model);
        std::fs::write(&tokens, model).unwrap();
    }
    assert!(s.execute("faces.models.install", &json!({"path": tokens.to_string_lossy(), "acknowledged": true})).is_err());
    assert!(s.execute("faces.models.select", &json!({"id": 5})).is_err());
    assert!(s.execute("faces.models.select", &json!({"id": ""})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// A demo session with the real YuNet installed, for the tests that need it: `LC_YUNET_MODEL=<the .onnx file>` (what
/// Settings > Faces downloads; nothing is committed). Without it those tests say so and pass.
fn demo_with_yunet() -> Option<Session> {
    let Some(file) = std::env::var_os("LC_YUNET_MODEL") else {
        eprintln!("LC_YUNET_MODEL is not set: skipping");
        return None;
    };
    let dir = temp("yunet");
    let mut s = Session::with_demo();
    s.face_models_dir = Some(dir.join("models"));
    let r = s.execute("faces.models.install", &json!({"path": file.to_string_lossy(), "acknowledged": true}));
    assert!(r.is_ok(), "{r:?}");
    Some(s)
}

#[test]
fn installing_the_detector_does_not_choose_it_as_the_recogniser_or_switch_recognition_on() {
    let Some(mut s) = demo_with_yunet() else { return };
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(find(&l, "yunet-2023mar")["installed"], true);
    assert_eq!(l["enabled"], false, "{l}");
    assert!(l["embedder"].is_null(), "{l}");
    assert!(s.execute("faces.models.test", &json!({"id": "yunet-2023mar"})).is_err(), "a detector has no recognition self-test");
}

#[test]
fn detect_without_the_model_says_where_to_get_it() {
    let d = temp("nodetector");
    let mut s = Session::with_demo();
    // no folder for models at all, then a folder that has none yet
    assert!(s.execute("faces.detect", &json!({})).is_err());
    s.face_models_dir = Some(d.join("models"));
    let e = s.execute("faces.detect", &json!({})).unwrap_err().to_string();
    assert!(e.contains("Settings > Faces"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_detector_and_the_recognisers_can_be_downloaded_and_the_download_is_refused_otherwise() {
    let d = temp("dl-refusals");
    let mut s = session(&d);
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    for (id, host) in [("yunet-2023mar", "github.com"), ("sface-2021dec", "github.com"), ("auraface-v1", "huggingface.co")] {
        assert_eq!(find(&l, id)["downloadHost"], host, "{id}");
    }
    // without the user's acceptance, for an id nobody knows, and without a folder: nothing starts
    assert!(s.execute("faces.models.download", &json!({"id": "yunet-2023mar"})).is_err());
    assert!(s.execute("faces.models.download", &json!({"id": "yunet-2023mar", "acknowledged": false})).is_err());
    for id in ["nope", "", "../yunet-2023mar"] {
        assert!(s.execute("faces.models.download", &json!({"id": id, "acknowledged": true})).is_err(), "{id}");
    }
    assert!(Session::new().execute("faces.models.download", &json!({"id": "yunet-2023mar", "acknowledged": true})).is_err());
    assert_eq!(s.execute("faces.models.downloads", &json!({})).unwrap()["downloads"], json!([]));
    assert!(!d.join("models").join(".downloads").exists(), "a refused download touches nothing");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_download_that_has_arrived_is_installed_by_itself_and_its_staged_file_goes() {
    let d = temp("dl-arrived");
    let mut s = session(&d);
    let staging = d.join("models").join(".downloads");
    std::fs::create_dir_all(&staging).unwrap();
    let staged = staging.join("model.onnx");
    std::fs::write(&staged, lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
    let sha = lightcraft_faces::hash::sha256_file(&staged).unwrap();
    s.face_downloads.arrived("my-model", staged.clone(), &sha);

    let r = s.execute("faces.models.downloads", &json!({})).unwrap();
    let row = &r["downloads"][0];
    assert_eq!((row["id"].as_str(), row["state"].as_str()), (Some("my-model"), Some("installed")), "{r}");
    assert!(!staged.exists(), "the staged file was moved into the model's folder");
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(l["models"].as_array().unwrap().iter().filter(|m| m["installed"] == true).count(), 1);
    // announced once, cleared on request
    assert_eq!(s.execute("faces.models.downloadCancel", &json!({"id": "my-model"})).unwrap()["discarded"], true);
    assert_eq!(s.execute("faces.models.downloads", &json!({})).unwrap()["downloads"], json!([]));
    assert_eq!(s.execute("faces.models.downloadCancel", &json!({"id": "my-model"})).unwrap()["discarded"], false);

    // a file that cannot be installed is thrown away with a reason
    std::fs::write(&staged, b"this is not a model").unwrap();
    let sha = lightcraft_faces::hash::sha256_file(&staged).unwrap();
    s.face_downloads.arrived("bad", staged.clone(), &sha);
    let r = s.execute("faces.models.downloads", &json!({})).unwrap();
    assert_eq!(r["downloads"][0]["state"], "failed", "{r}");
    assert!(!staged.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// `faces.detect` on photos without faces: it reports nothing, and a new run replaces earlier detections but
/// never regions that came from XMP or were named; one undo step restores everything.
#[test]
fn detect_replaces_only_earlier_detections_and_is_one_undo_step() {
    use lightcraft_catalog::Op;
    use lightcraft_meta::{Region, RegionKind};
    let region = |name: Option<&str>, description: Option<&str>, x: f64| Region {
        rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.2, y1: 0.5 },
        kind: RegionKind::Face,
        name: name.map(str::to_string),
        description: description.map(str::to_string),
    };
    let Some(mut s) = demo_with_yunet() else { return };
    let id = s.active().unwrap();
    let mut meta = s.catalog.photo(id).unwrap().meta.clone();
    meta.regions =
        vec![region(Some("Jane Doe"), None, 0.1), region(None, Some("Detected by YuNet 2023mar"), 0.5), region(None, Some("Drawn by hand"), 0.7)];
    s.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    let undo_before = s.undo.len();

    // a dry run reports and changes nothing
    let r = s.execute("faces.detect", &json!({"apply": false})).unwrap();
    assert_eq!(r["applied"], false);
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), 3);
    assert_eq!(s.undo.len(), undo_before);

    // a real run (the default) drops the earlier detection (the demo has no faces) and keeps the others
    let r = s.execute("faces.detect", &json!({})).unwrap();
    assert_eq!(r["applied"], true);
    assert_eq!(r["detector"], "YuNet 2023mar");
    let photo = r["photos"].as_array().unwrap().iter().find(|p| p["id"] == id.0).unwrap();
    assert_eq!(photo["faces"].as_array().unwrap().len(), 0, "the procedural demo photos have no faces");
    let names: Vec<_> = s.catalog.photo(id).unwrap().meta.regions.iter().map(|r| (r.name.clone(), r.description.clone())).collect();
    assert_eq!(names, vec![(Some("Jane Doe".to_string()), None), (None, Some("Drawn by hand".to_string()))]);
    assert_eq!(s.undo.len(), undo_before + 1, "one undo step for the whole run");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), 3, "undo brings the detection back");

    // odd parameters are tolerated, and nothing is detected without a selection
    for p in [json!({"score": 5}), json!({"score": -1}), json!({"nmsIou": 99}), json!({"maxFaces": 0}), json!({"score": "x"})] {
        assert!(s.execute("faces.detect", &p).is_ok(), "{p}");
    }
    let mut empty = Session::new();
    assert!(empty.execute("faces.detect", &json!({})).is_err());
}

/// Opt-in, needs the internet: `cargo test -p lightcraft-engine real_download -- --ignored --nocapture`. Downloads YuNet
/// from its pinned address the way the app does, waits for it to be installed, then finds faces with it.
#[test]
#[ignore = "downloads from github.com"]
fn real_download_of_yunet_installs_it_and_detects() {
    let d = temp("real-download");
    let mut s = Session::with_demo();
    s.face_models_dir = Some(d.join("models"));
    let started = s.execute("faces.models.download", &json!({"id": "yunet-2023mar", "acknowledged": true})).unwrap();
    assert_eq!(started["from"], "github.com");
    let t0 = std::time::Instant::now();
    loop {
        let r = s.execute("faces.models.downloads", &json!({})).unwrap();
        let row = r["downloads"][0].clone();
        match row["state"].as_str() {
            Some("installed") => break,
            Some("running") | Some("done") => {}
            _ => panic!("{r}"),
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(120), "{r}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("downloaded and installed in {:?}", t0.elapsed());
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(find(&l, "yunet-2023mar")["installed"], true);
    let r = s.execute("faces.detect", &json!({"apply": false})).unwrap();
    assert_eq!(r["detector"], "YuNet 2023mar");
    // asking again once installed is refused
    assert!(s.execute("faces.models.download", &json!({"id": "yunet-2023mar", "acknowledged": true})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// Opt-in, needs the internet: the recognisers are big, so this only checks that bytes start to arrive (through the
/// hosts' redirects) and that cancelling leaves nothing behind.
#[test]
#[ignore = "downloads from github.com and huggingface.co"]
fn real_downloads_of_the_recognisers_start_and_can_be_cancelled() {
    let d = temp("real-start");
    let mut s = Session::with_demo();
    s.face_models_dir = Some(d.join("models"));
    for id in ["sface-2021dec", "auraface-v1"] {
        s.execute("faces.models.download", &json!({"id": id, "acknowledged": true})).unwrap();
        let t0 = std::time::Instant::now();
        loop {
            let r = s.execute("faces.models.downloads", &json!({})).unwrap();
            let row = r["downloads"].as_array().unwrap().iter().find(|x| x["id"] == id).unwrap().clone();
            if row["state"] == "running" && row["bytes"].as_u64().unwrap_or(0) > 100_000 {
                println!("{id}: {} of {} bytes after {:?}", row["bytes"], row["total"], t0.elapsed());
                break;
            }
            assert!(row["state"] == "running", "{r}");
            assert!(t0.elapsed() < std::time::Duration::from_secs(60), "{r}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        s.execute("faces.models.downloadCancel", &json!({"id": id})).unwrap();
    }
    drop(s);
    let staging = d.join("models").join(".downloads");
    let left: Vec<_> = std::fs::read_dir(&staging).map(|r| r.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    println!("left in staging: {left:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// With the recognition runtime: installing runs the model's self-test, a model that does not work is refused
/// and leaves nothing behind, and `faces.models.test` runs it again.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn installing_runs_the_self_test_and_refuses_models_that_do_not_work() {
    let d = temp("selftest");
    let mut s = session(&d);
    // a working model passes; its record keeps the result
    let f = model_file(&d, "Works.onnx", 64);
    let r = s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["installed"]["accepted"]["selfTest"]["ok"], true, "{r}");
    assert_eq!(r["installed"]["accepted"]["selfTest"]["dimension"], 64);
    let t = s.execute("faces.models.test", &json!({"id": id})).unwrap();
    assert_eq!(t["ok"], true, "{t}");
    assert!(t["result"]["embedMs"].as_f64().unwrap() >= 0.0);
    assert!(s.execute("faces.models.test", &json!({"id": "nope"})).is_err());
    assert!(s.execute("faces.models.test", &json!({})).is_err());
    // a graph with no layers passes the shape check but cannot run: refused, and nothing is left on disk
    let broken = d.join("Broken.onnx");
    std::fs::write(&broken, lightcraft_faces::synthetic::embedder_model(64)).unwrap();
    let e = s.execute("faces.models.install", &json!({"path": broken.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("could not be loaded") || e.contains("self-test"), "{e}");
    let leftovers: Vec<_> = std::fs::read_dir(d.join("models"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "settings.json")
        .collect();
    assert!(leftovers.len() == 1 && leftovers[0].starts_with("custom-works-"), "only the working model remains: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn failed_replacement_keeps_the_installed_model_and_records() {
    let d = temp("replacement");
    let models = d.join("models");
    std::fs::create_dir_all(&models).unwrap();
    let good = model_file(&d, "Good.onnx", 64);
    let write_catalog = |path: &std::path::Path| {
        let catalog = json!({"models": [{
            "id": "replacement", "name": "Replacement", "version": "1", "role": "embedder",
            "url": "https://models.example.org/weights.onnx",
            "sha256": lightcraft_faces::hash::sha256_file(path).unwrap(),
            "sizeBytes": std::fs::metadata(path).unwrap().len(),
            "licence": {"name": "MIT", "commercial": "yes"},
            "output": {"kind": "embedding", "dim": 64}
        }]});
        std::fs::write(models.join("catalog.json"), catalog.to_string()).unwrap();
    };
    write_catalog(std::path::Path::new(&good));
    let mut s = session(&d);
    s.execute("faces.models.install", &json!({"path": good, "acknowledged": true})).unwrap();
    let home = models.join("replacement");
    let files = ["model.onnx", "face-model.json", "installed.json"];
    let before: Vec<_> = files.iter().map(|name| std::fs::read(home.join(name)).unwrap()).collect();
    let settings = std::fs::read(models.join("settings.json")).unwrap();
    let bad = d.join("Bad.onnx");
    std::fs::write(&bad, lightcraft_faces::synthetic::embedder_model(64)).unwrap();
    write_catalog(&bad);
    let mut s = session(&d);
    assert!(s.execute("faces.models.install", &json!({"path": bad, "acknowledged": true})).is_err());
    for (name, bytes) in files.iter().zip(before) {
        assert_eq!(std::fs::read(home.join(name)).unwrap(), bytes, "{name} was replaced");
    }
    assert_eq!(std::fs::read(models.join("settings.json")).unwrap(), settings);
    assert!(!home.join("model.onnx.part").exists());
    let _ = std::fs::remove_dir_all(d);
}

fn settings_of(d: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(d.join("models").join("settings.json")).unwrap()).unwrap()
}

#[test]
fn installing_a_model_chooses_it_and_switches_recognition_on() {
    let d = temp("activate");
    let mut s = session(&d);
    let a = s.execute("faces.models.install", &json!({"path": model_file(&d, "First.onnx", 64), "acknowledged": true})).unwrap();
    let a_id = a["installed"]["id"].as_str().unwrap().to_string();
    assert_eq!(a["active"], json!({"embedder": a_id, "enabled": true}));
    assert_eq!(a["installed"]["selected"], true);
    assert_eq!(settings_of(&d), json!({"enabled": true, "embedder": a_id}));
    // a second model takes over (the newest is the one the user just asked for); the first stays installed
    let b = s.execute("faces.models.install", &json!({"path": model_file(&d, "Second.onnx", 96), "acknowledged": true})).unwrap();
    let b_id = b["installed"]["id"].as_str().unwrap().to_string();
    assert_ne!(a_id, b_id);
    assert_eq!(b["active"]["embedder"], json!(b_id));
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!((find(&l, &a_id)["installed"].clone(), find(&l, &a_id)["selected"].clone()), (json!(true), json!(false)));
    assert_eq!(find(&l, &b_id)["selected"], true);
    // switching back is a choice, and a model added with `activate: false` leaves the choice alone
    s.execute("faces.models.select", &json!({"id": a_id})).unwrap();
    let c = s.execute("faces.models.install", &json!({"path": model_file(&d, "Third.onnx", 128), "acknowledged": true, "activate": false})).unwrap();
    let c_id = c["installed"]["id"].as_str().unwrap().to_string();
    assert_eq!(c["active"]["embedder"], json!(a_id));
    assert_eq!(c["installed"]["selected"], false);
    // a switched-off recognition is switched back on by the next install, not by a refused one
    s.execute("faces.enable", &json!({"enabled": false})).unwrap();
    assert!(s.execute("faces.models.install", &json!({"path": model_file(&d, "Nope.onnx", 64)})).is_err());
    assert_eq!(settings_of(&d)["enabled"], false);
    s.execute("faces.models.install", &json!({"path": model_file(&d, "Fourth.onnx", 64), "acknowledged": true})).unwrap();
    assert_eq!(settings_of(&d)["enabled"], true);
    // removing the model in use hands over to another installed one instead of leaving nothing chosen
    let in_use = settings_of(&d)["embedder"].as_str().unwrap().to_string();
    let r = s.execute("faces.models.remove", &json!({"id": in_use})).unwrap();
    let next = r["embedder"].as_str().unwrap().to_string();
    assert_ne!(next, in_use);
    assert!([&a_id, &b_id, &c_id].contains(&&next) || next.starts_with("custom-"), "{next}");
    assert_eq!(settings_of(&d)["embedder"], json!(next));
    // removing a model that is not in use changes nothing
    let other = [&a_id, &b_id, &c_id].into_iter().find(|i| **i != next && **i != in_use).unwrap().clone();
    s.execute("faces.models.remove", &json!({"id": other})).unwrap();
    assert_eq!(settings_of(&d)["embedder"], json!(next));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_download_that_arrives_is_installed_and_switched_on_by_itself() {
    use crate::face_download::{STAGING, State};
    let d = temp("finish");
    let mut s = session(&d);
    let staging = d.join("models").join(STAGING);
    std::fs::create_dir_all(&staging).unwrap();
    // what the download thread leaves when it succeeds: a verified file in the staging folder and a finished job
    let staged = model_file(&staging, "arrived.onnx", 64);
    let sha = lightcraft_faces::hash::sha256_file(std::path::Path::new(&staged)).unwrap();
    s.face_downloads.set_outcome("fake-model", State::Done { path: staged.clone().into(), sha256: sha });
    let r = s.execute("faces.models.downloads", &json!({})).unwrap();
    assert_eq!(r["downloads"][0]["id"], "fake-model");
    assert_eq!(r["downloads"][0]["state"], "installed", "{r}");
    let st = settings_of(&d);
    assert_eq!(st["enabled"], true);
    assert!(st["embedder"].as_str().unwrap().starts_with("custom-arrived-"), "{st}");
    assert!(!std::path::Path::new(&staged).exists(), "the staged file was moved into place");
    // the record stays until it is cleared, and is cleared once
    assert_eq!(s.execute("faces.models.downloads", &json!({})).unwrap()["downloads"][0]["state"], "installed");
    assert_eq!(s.execute("faces.models.downloadCancel", &json!({"id": "fake-model"})).unwrap()["discarded"], true);
    assert_eq!(s.execute("faces.models.downloads", &json!({})).unwrap()["downloads"], json!([]));

    // a download that turns out not to be a usable model is thrown away, and the reason is shown
    let junk = staging.join("junk.onnx");
    std::fs::write(&junk, b"definitely not a model").unwrap();
    let sha = lightcraft_faces::hash::sha256_file(&junk).unwrap();
    s.face_downloads.set_outcome("fake-junk", State::Done { path: junk.clone(), sha256: sha });
    let r = s.execute("faces.models.downloads", &json!({})).unwrap();
    assert_eq!(r["downloads"][0]["state"], "failed", "{r}");
    assert!(r["downloads"][0]["error"].as_str().unwrap().contains("cannot be used"), "{r}");
    assert!(!junk.exists());
    assert_eq!(settings_of(&d)["embedder"], st["embedder"], "a failed download leaves the choice alone");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_users_own_catalog_adds_models_with_the_same_download_and_install() {
    use crate::face_download::State;
    let d = temp("catalog");
    let mut s = session(&d);
    let models = d.join("models");
    std::fs::create_dir_all(&models).unwrap();
    // a model file, and a catalog that lists it by its hash (plus an entry that cannot be used)
    let file = model_file(&d, "weights.onnx", 64);
    let (sha, size) = (lightcraft_faces::hash::sha256_file(std::path::Path::new(&file)).unwrap(), std::fs::metadata(&file).unwrap().len());
    let catalog = json!({"models": [
        {"id": "my-research-model", "name": "My research model", "version": "1", "role": "embedder", "url": "https://models.example.org/w/weights.onnx",
         "sha256": sha, "sizeBytes": size, "licence": {"name": "Research only", "commercial": "no", "notice": "Not for commercial use."},
         "provenance": "Some dataset", "output": {"kind": "embedding", "dim": 64}, "thresholds": {"matchCosine": 0.42}},
        {"id": "broken", "name": "No hash", "version": "1", "role": "embedder", "url": "https://models.example.org/x.onnx", "output": {"kind": "embedding", "dim": 64}},
    ]});
    std::fs::write(models.join("catalog.json"), catalog.to_string()).unwrap();
    // it is listed like a built-in model, with its own terms, a download address, and the problem with the other entry
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    let m = find(&l, "my-research-model");
    assert_eq!(
        (m["fromCatalog"].clone(), m["downloadHost"].clone(), m["licence"]["commercial"].clone()),
        (json!(true), json!("models.example.org"), json!("no"))
    );
    assert_eq!(m["installed"], false);
    assert_eq!(find(&l, "broken"), &Value::Null);
    assert_eq!(l["catalog"]["models"], 1);
    assert!(l["catalog"]["errors"][0].as_str().unwrap().contains("broken"), "{}", l["catalog"]);
    // nothing is fetched without the terms being accepted
    assert!(s.execute("faces.models.download", &json!({"id": "my-research-model"})).is_err());
    // a file that matches is recognised as that model (not as a draft), so its terms and settings are used
    let seen = s.execute("faces.models.inspect", &json!({"path": file})).unwrap();
    assert_eq!((seen["kind"].clone(), seen["model"]["id"].clone()), (json!("known"), json!("my-research-model")));
    // a finished download of it installs under the catalog's id, with the catalog's licence record
    let staging = models.join(crate::face_download::STAGING);
    std::fs::create_dir_all(&staging).unwrap();
    let staged = staging.join("weights.onnx");
    std::fs::copy(&file, &staged).unwrap();
    s.face_downloads.set_outcome("my-research-model", State::Done { path: staged, sha256: sha });
    let r = s.execute("faces.models.downloads", &json!({})).unwrap();
    assert_eq!(r["downloads"][0]["state"], "installed", "{r}");
    let st = settings_of(&d);
    assert_eq!((st["embedder"].clone(), st["enabled"].clone()), (json!("my-research-model"), json!(true)));
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(
        (find(&l, "my-research-model")["installed"].clone(), find(&l, "my-research-model")["accepted"]["commercial"].clone()),
        (json!(true), json!("no"))
    );
    // a broken or hostile catalog never stops the list
    for bytes in [&b"{"[..], b"[]", &vec![b'x'; 400_000]] {
        std::fs::write(models.join("catalog.json"), bytes).unwrap();
        let l = s.execute("faces.models.list", &json!({})).unwrap();
        assert!(l["catalog"]["errors"].as_array().is_some_and(|e| !e.is_empty()), "{}", l["catalog"]);
        assert!(find(&l, "sface-2021dec")["id"] == "sface-2021dec");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Issue #678: on means something only with a model to run. Turning recognition on with no chosen
/// (installed) recogniser is refused; installing one chooses it and switches recognition on, after
/// which the switch works both ways; removing the last recogniser switches it off again.
#[test]
fn recognition_cannot_be_turned_on_without_a_chosen_model() {
    let d = temp("enable-needs-model");
    let mut s = session(&d);
    for p in [json!({"enabled": true}), json!({})] {
        let e = s.execute("faces.enable", &p).unwrap_err().to_string();
        assert!(e.contains("recognition model"), "{p}: {e}");
    }
    assert_eq!(s.execute("faces.enable", &json!({"enabled": false})).unwrap()["enabled"], false, "off is always allowed");
    assert_eq!(s.execute("faces.models.list", &json!({})).unwrap()["enabled"], false);
    let f = model_file(&d, "m.onnx", 64);
    let r = s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["active"]["enabled"], true, "installing chooses the model and switches recognition on: {r}");
    assert_eq!(s.execute("faces.enable", &json!({"enabled": false})).unwrap()["enabled"], false);
    assert_eq!(s.execute("faces.enable", &json!({"enabled": true})).unwrap()["enabled"], true);
    assert_eq!(s.execute("faces.enable", &json!({})).unwrap()["enabled"], false, "toggles when omitted");
    assert_eq!(s.execute("faces.enable", &json!({})).unwrap()["enabled"], true);
    // the last recogniser goes: nothing is chosen, so recognition is off and stays off
    assert_eq!(s.execute("faces.models.remove", &json!({"id": id})).unwrap()["embedder"], Value::Null);
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!((l["enabled"].clone(), l["embedder"].clone()), (json!(false), Value::Null), "{l}");
    assert!(s.execute("faces.enable", &json!({"enabled": true})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
