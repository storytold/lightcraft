//! The `content.*` model commands: the list, downloading behind the licence (from a local server, never the
//! internet), checking a folder that is already complete without fetching anything, cancelling, hash mismatches,
//! adding a folder, choosing, removing, the self-test, the indexing setting and the status, and the refusals
//! (no folder, unwritable folder, licence not accepted, hostile manifests and ids, a second job while one runs).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lightcraft_faces::hash::sha256_hex;
use lightcraft_vision::manifest::{self, FileSpec, Manifest};
use serde_json::{Value, json};

use crate::Session;
use crate::content_models::{self as cm, JobState};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-contentmodels-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The content folder is `<d>/models/content`; the mirrors file `<d>/models/content-mirrors.txt`.
fn session(d: &Path) -> Session {
    let mut s = Session::new();
    s.content_models_dir = Some(d.join("models").join("content"));
    s
}

fn content_dir(s: &Session) -> PathBuf {
    s.content_models_dir.clone().unwrap()
}

/// A small model's files: (name, bytes).
fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("model.safetensors", (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect()),
        ("tokenizer.json", b"{\"model\": {}}".to_vec()),
        ("config.json", b"{\"model_type\": \"siglip\"}".to_vec()),
    ]
}

/// The manifest of [`files`] under `id`.
fn tiny(id: &str) -> Manifest {
    let mut m = manifest::builtin_siglip2_b16_256();
    m.id = id.into();
    m.name = format!("Tiny {id}");
    m.source = "https://example.org/tiny".into();
    m.files = files().iter().map(|(n, b)| FileSpec { name: (*n).into(), size: b.len() as u64, sha256: sha256_hex(b) }).collect();
    m
}

/// List `models` in the user's catalog.
fn catalog(s: &Session, models: &[Manifest]) {
    let dir = content_dir(s);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(cm::CATALOG_FILE), serde_json::to_vec(&json!({"models": models})).unwrap()).unwrap();
}

fn mirror(s: &Session, base: &str) {
    std::fs::write(content_dir(s).parent().unwrap().join(cm::MIRRORS_FILE), format!("# test\n{base}\n")).unwrap();
}

struct Server {
    base: String,
    hits: Arc<AtomicUsize>,
}

/// A local server for `/<anything>/<id>/<file>`: the bytes of `files()` by file name (`wrong`: one byte flipped in
/// each; `slow`: a few kilobytes at a time).
fn server(mode: &'static str) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/m", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let table: HashMap<&'static str, Vec<u8>> = files().into_iter().collect();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let table = table.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/");
                let name = path.rsplit('/').next().unwrap_or("");
                let Some(mut body) = table.get(name).cloned() else {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                };
                if mode == "wrong" {
                    body[0] ^= 0xff;
                }
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                if mode == "slow" {
                    for c in body.chunks(2048) {
                        if s.write_all(c).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                } else {
                    let _ = s.write_all(&body);
                }
            });
        }
    });
    Server { base, hits }
}

fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["models"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or(&Value::Null)
}

fn list(s: &mut Session) -> Value {
    s.execute("content.models.list", &json!({})).unwrap()
}

/// Wait for model `id`'s job to end; its final state.
fn finished(s: &Session, id: &str) -> JobState {
    let t = Instant::now();
    loop {
        match s.content_jobs.get(id) {
            Some((_, JobState::Running { .. })) => {}
            Some((_, st)) => return st,
            None => panic!("no job for {id}"),
        }
        assert!(t.elapsed() < Duration::from_secs(20), "the job never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Put `files()` (as `m`'s) in `dir` with a manifest.
fn model_folder(dir: &Path, m: &Manifest) {
    std::fs::create_dir_all(dir).unwrap();
    for (n, b) in files() {
        std::fs::write(dir.join(n), b).unwrap();
    }
    std::fs::write(dir.join(cm::MANIFEST_FILE), serde_json::to_vec_pretty(m).unwrap()).unwrap();
}

#[test]
fn the_list_offers_the_known_models_with_their_licence_and_source() {
    let d = temp("list");
    let mut s = session(&d);
    let l = list(&mut s);
    assert_eq!((l["enabled"].as_bool(), l["model"].is_null(), l["runtime"].as_bool()), (Some(false), true, Some(cfg!(not(target_arch = "wasm32")))));
    let ids: Vec<&str> = l["models"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids, ["siglip2-b16-256", "siglip2-b16-384", "siglip2-so400m-16-384"]);
    let m = find(&l, "siglip2-b16-256");
    assert_eq!((m["state"].as_str(), m["default"].as_bool(), m["installed"].as_bool()), (Some("available"), Some(true), Some(false)));
    assert_eq!(
        (m["licence"]["name"].as_str(), m["licence"]["commercial"].as_bool(), m["publisher"].as_str()),
        (Some("Apache-2.0"), Some(true), Some("Google"))
    );
    assert_eq!(m["downloadHost"], "huggingface.co");
    assert_eq!(m["sizeBytes"], 1_500_985_224u64 + 34_363_039 + 276 + 394);
    assert_eq!((m["supported"].as_bool(), find(&l, "siglip2-so400m-16-384")["embeddingDim"].as_u64()), (Some(true), Some(1152)));
    // no folder at all: an empty list and refusals, never a crash
    let mut bare = Session::new();
    assert_eq!(bare.execute("content.models.list", &json!({})).unwrap()["models"], json!([]));
    for (c, p) in [("content.models.download", json!({"id": "siglip2-b16-256", "acknowledged": true})), ("content.enable", json!({"on": true}))] {
        assert!(bare.execute(c, &p).is_err(), "{c}");
    }
    assert_eq!(bare.execute("content.status", &json!({})).unwrap()["ready"], false);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_download_needs_the_licence_accepted_and_a_known_model() {
    let d = temp("ack");
    let mut s = session(&d);
    catalog(&s, &[tiny("tiny")]);
    let srv = server("ok");
    mirror(&s, &srv.base);
    for p in [json!({"id": "tiny"}), json!({"id": "tiny", "acknowledged": false}), json!({"id": "tiny", "acknowledged": "yes"})] {
        let e = s.execute("content.models.download", &p).unwrap_err().to_string();
        assert!(e.contains("acknowledged"), "{e}");
    }
    assert!(s.execute("content.models.download", &json!({"id": "nope", "acknowledged": true})).is_err());
    assert!(s.execute("content.models.download", &json!({"id": "../x", "acknowledged": true})).is_err());
    assert!(s.execute("content.models.download", &json!({})).is_err());
    assert_eq!(srv.hits.load(Ordering::SeqCst), 0, "nothing was fetched");
    assert!(!content_dir(&s).join("tiny").exists());
    assert!(s.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_download_shows_in_the_activity_stack_is_checked_and_installs_and_chooses_the_model() {
    let d = temp("download");
    let mut s = session(&d);
    catalog(&s, &[tiny("tiny")]);
    let srv = server("slow");
    mirror(&s, &srv.base);
    let r = s.execute("content.models.download", &json!({"id": "tiny", "acknowledged": true})).unwrap();
    assert_eq!((r["started"].as_str(), r["checkOnly"].as_bool()), (Some("tiny"), Some(false)));
    assert!(r["from"].as_str().unwrap().starts_with("127.0.0.1:"), "{r}");
    let rows = s.activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("download", "Downloading model", "Tiny tiny"));
    assert!(rows[0].cancellable);
    assert_eq!(find(&list(&mut s), "tiny")["job"]["state"], "running");
    // a second job while one runs is refused
    let e = s.execute("content.models.download", &json!({"id": "siglip2-b16-256", "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("downloading"), "{e}");
    assert!(matches!(finished(&s, "tiny"), JobState::Done(_)));
    let l = list(&mut s);
    let m = find(&l, "tiny");
    assert_eq!((m["state"].as_str(), m["selected"].as_bool()), (Some("installed"), Some(true)), "{m}");
    assert!(m["accepted"]["acceptedAt"].is_string() && m["accepted"]["licence"] == "Apache-2.0", "{m}");
    assert_eq!(l["model"], "tiny");
    let home = content_dir(&s).join("tiny");
    for (n, b) in files() {
        assert_eq!(std::fs::read(home.join(n)).unwrap(), b);
        assert!(!home.join(format!("{n}.part")).exists());
    }
    assert!(home.join(cm::MANIFEST_FILE).is_file());
    // already installed: nothing to download
    assert!(s.execute("content.models.download", &json!({"id": "tiny", "acknowledged": true})).is_err());
    let st = s.execute("content.status", &json!({})).unwrap();
    assert_eq!((st["model"].as_str(), st["installed"].as_u64(), st["enabled"].as_bool()), (Some("tiny"), Some(1), Some(false)));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_file_with_the_wrong_checksum_is_rejected_and_nothing_is_installed() {
    let d = temp("wrong");
    let mut s = session(&d);
    catalog(&s, &[tiny("tiny")]);
    let srv = server("wrong");
    mirror(&s, &srv.base);
    s.execute("content.models.download", &json!({"id": "tiny", "acknowledged": true})).unwrap();
    let JobState::Failed(why) = finished(&s, "tiny") else { panic!("a damaged file was accepted") };
    assert!(why.contains("damaged or not the official one"), "{why}");
    let m = find(&list(&mut s), "tiny").clone();
    assert_eq!((m["state"].as_str(), m["job"]["state"].as_str()), (Some("available"), Some("failed")));
    let home = content_dir(&s).join("tiny");
    assert!(!home.join("model.safetensors").exists() && !home.join("model.safetensors.part").exists());
    // clearing the failure leaves no partial file
    assert_eq!(s.execute("content.models.downloadCancel", &json!({"id": "tiny"})).unwrap()["discarded"], true);
    assert!(std::fs::read_dir(&home).map(|r| r.flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part"))).unwrap_or(true));
    assert!(find(&list(&mut s), "tiny")["job"].is_null());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn cancelling_a_download_mid_file_leaves_nothing() {
    let d = temp("cancel");
    let mut s = session(&d);
    catalog(&s, &[tiny("tiny")]);
    let srv = server("slow");
    mirror(&s, &srv.base);
    s.execute("content.models.download", &json!({"id": "tiny", "acknowledged": true})).unwrap();
    let home = content_dir(&s).join("tiny");
    let t = Instant::now();
    while !home.join("model.safetensors.part").exists() {
        assert!(t.elapsed() < Duration::from_secs(10), "the download never began");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(s.execute("content.models.downloadCancel", &json!({"id": "tiny"})).unwrap()["cancelled"], true);
    assert_eq!(finished(&s, "tiny"), JobState::Cancelled);
    assert!(!home.exists(), "the model's folder and its files are gone");
    let t = Instant::now();
    while !s.activity.list().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(5), "the activity row stayed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(find(&list(&mut s), "tiny")["state"], "available");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_complete_folder_put_there_by_hand_is_checked_without_downloading() {
    let d = temp("found");
    let mut s = session(&d);
    let m = tiny("tiny");
    catalog(&s, std::slice::from_ref(&m));
    let srv = server("ok");
    mirror(&s, &srv.base);
    // the files, no manifest, no acceptance (as a manual download leaves them)
    let home = content_dir(&s).join("tiny");
    std::fs::create_dir_all(&home).unwrap();
    for (n, b) in files() {
        std::fs::write(home.join(n), b).unwrap();
    }
    let l = list(&mut s);
    assert_eq!((find(&l, "tiny")["state"].as_str(), l["model"].is_null()), (Some("found"), true));
    assert!(s.execute("content.models.select", &json!({"id": "tiny"})).is_err(), "not checked yet");
    let r = s.execute("content.models.download", &json!({"id": "tiny", "acknowledged": true})).unwrap();
    assert_eq!(r["checkOnly"], true);
    assert!(matches!(finished(&s, "tiny"), JobState::Done(_)));
    assert_eq!(srv.hits.load(Ordering::SeqCst), 0, "nothing was downloaded");
    let l = list(&mut s);
    assert_eq!((find(&l, "tiny")["state"].as_str(), l["model"].as_str()), (Some("installed"), Some("tiny")));
    // a file that changed since (the right size, the wrong content) is caught when the folder is added
    let other = d.join("copy");
    model_folder(&other, &m);
    let mut bytes = std::fs::read(other.join("config.json")).unwrap();
    bytes[0] ^= 1;
    std::fs::write(other.join("config.json"), bytes).unwrap();
    s.execute("content.models.remove", &json!({"id": "tiny"})).unwrap();
    let e = s.execute("content.models.install", &json!({"path": other.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("config.json"), "{e}");
    assert!(!home.exists(), "a failed install leaves nothing");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_model_folder_is_added_once_its_licence_is_accepted_and_hostile_ones_are_refused() {
    let d = temp("install");
    let mut s = session(&d);
    let src = d.join("mine");
    model_folder(&src, &tiny("mine"));
    let p = |extra: Value| {
        let mut v = json!({"path": src.to_string_lossy()});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    let e = s.execute("content.models.install", &p(json!({}))).unwrap_err().to_string();
    assert!(e.contains("acknowledged"), "{e}");
    assert!(!content_dir(&s).join("mine").exists());
    let r = s.execute("content.models.install", &p(json!({"acknowledged": true}))).unwrap();
    assert_eq!((r["installed"]["state"].as_str(), r["model"].as_str()), (Some("installed"), Some("mine")));
    assert_eq!(r["installed"]["known"], false);
    // the copy is the model now; the source is untouched
    assert!(content_dir(&s).join("mine").join("model.safetensors").is_file() && src.join("model.safetensors").is_file());
    // hostile or broken folders
    let bad = d.join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    for (what, manifest) in [
        ("not json", b"not json".to_vec()),
        ("a path as id", serde_json::to_vec(&json!({"id": "../x"})).unwrap()),
        ("too large", vec![b' '; manifest::MAX_MANIFEST_BYTES + 10]),
        ("a known id with other files", serde_json::to_vec(&tiny("siglip2-b16-256")).unwrap()),
    ] {
        std::fs::write(bad.join(cm::MANIFEST_FILE), manifest).unwrap();
        assert!(s.execute("content.models.install", &json!({"path": bad.to_string_lossy(), "acknowledged": true})).is_err(), "{what}");
    }
    // a manifest whose files are missing
    std::fs::write(bad.join(cm::MANIFEST_FILE), serde_json::to_vec(&tiny("bad")).unwrap()).unwrap();
    let e = s.execute("content.models.install", &json!({"path": bad.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("model.safetensors"), "{e}");
    assert!(!content_dir(&s).join("bad").exists());
    // an empty folder, a file, nothing
    std::fs::remove_file(bad.join(cm::MANIFEST_FILE)).unwrap();
    for path in [bad.clone(), src.join("config.json"), d.join("nope")] {
        assert!(s.execute("content.models.install", &json!({"path": path.to_string_lossy(), "acknowledged": true})).is_err(), "{path:?}");
    }
    assert_eq!(list(&mut s)["model"], "mine");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn choosing_removing_and_the_indexing_switch_keep_a_usable_model() {
    let d = temp("select");
    let mut s = session(&d);
    for id in ["one", "two"] {
        let src = d.join(id);
        model_folder(&src, &tiny(id));
        s.execute("content.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true, "activate": false})).unwrap();
    }
    // the first one added is in use (nothing was), the second waits
    assert_eq!(list(&mut s)["model"], "one");
    assert!(s.execute("content.models.select", &json!({"id": "siglip2-b16-256"})).is_err(), "not installed");
    assert!(s.execute("content.models.select", &json!({"id": 5})).is_err());
    assert_eq!(s.execute("content.models.select", &json!({"id": "two"})).unwrap()["model"], "two");
    assert_eq!(s.execute("content.enable", &json!({"on": true})).unwrap()["enabled"], true);
    assert_eq!(s.execute("content.status", &json!({})).unwrap()["enabled"], true);
    // removing the model in use: the other takes over
    let r = s.execute("content.models.remove", &json!({"id": "two"})).unwrap();
    assert_eq!((r["model"].as_str(), r["enabled"].as_bool()), (Some("one"), Some(true)));
    // removing the last: nothing chosen, indexing off
    let r = s.execute("content.models.remove", &json!({"id": "one"})).unwrap();
    assert_eq!((r["model"].as_str(), r["enabled"].as_bool()), (None, Some(false)));
    let e = s.execute("content.enable", &json!({"on": true})).unwrap_err().to_string();
    assert!(e.contains("choose a content model"), "{e}");
    // ids that are not models, and models that are not there
    for id in ["../x", "", "settings.json", "siglip2-b16-256"] {
        assert!(s.execute("content.models.remove", &json!({"id": id})).is_err(), "{id}");
    }
    assert!(content_dir(&s).join(cm::SETTINGS_FILE).is_file(), "settings survive");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_model_whose_files_are_damaged_is_not_used_and_says_why() {
    let d = temp("damaged");
    let mut s = session(&d);
    let src = d.join("mine");
    model_folder(&src, &tiny("mine"));
    s.execute("content.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();
    s.execute("content.enable", &json!({"on": true})).unwrap();
    let home = content_dir(&s).join("mine");
    std::fs::write(home.join("model.safetensors"), b"short").unwrap();
    let l = list(&mut s);
    let m = find(&l, "mine");
    assert_eq!(m["state"], "damaged");
    assert!(m["problem"].as_str().unwrap().contains("model.safetensors"), "{m}");
    assert_eq!((l["model"].is_null(), l["enabled"].as_bool()), (true, Some(false)), "a damaged model is not in use");
    let st = s.execute("content.status", &json!({})).unwrap();
    assert_eq!((st["ready"].as_bool(), st["model"].is_null()), (Some(false), true));
    assert!(s.execute("content.models.test", &json!({"id": "mine", "wait": true})).is_err());
    // a broken manifest of a folder that is not a model is reported, not trusted
    let junk = content_dir(&s).join("junk");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join(cm::MANIFEST_FILE), b"{").unwrap();
    std::fs::write(content_dir(&s).join(cm::CATALOG_FILE), b"[1, 2").unwrap();
    let l = list(&mut s);
    assert!(find(&l, "junk").is_null());
    assert_eq!(l["catalog"]["errors"].as_array().unwrap().len(), 2, "{l}");
    // it can be removed
    s.execute("content.models.remove", &json!({"id": "mine"})).unwrap();
    assert!(!home.exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_unwritable_models_folder_is_an_error_not_a_crash() {
    let d = temp("unwritable");
    // the content folder's parent is a file: nothing can be created under it
    std::fs::write(d.join("models"), b"a file").unwrap();
    let mut s = session(&d);
    catalog_unwritable(&mut s);
    let src = d.join("mine");
    model_folder(&src, &tiny("mine"));
    let e = s.execute("content.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("could not create"), "{e}");
    let e = s.execute("content.models.download", &json!({"id": "siglip2-b16-256", "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("could not create"), "{e}");
    assert!(s.activity.list().is_empty());
    assert_eq!(list(&mut s)["models"].as_array().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(&d);
}

/// (no catalog can be written there either: the list still has the built-in models)
fn catalog_unwritable(s: &mut Session) {
    assert!(std::fs::create_dir_all(content_dir(s)).is_err());
}

#[test]
fn the_self_test_of_a_model_that_does_not_load_fails_and_is_recorded() {
    let d = temp("selftest");
    let mut s = session(&d);
    let src = d.join("mine");
    model_folder(&src, &tiny("mine"));
    s.execute("content.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();
    assert!(s.execute("content.models.test", &json!({"id": "nope"})).is_err());
    let r = s.execute("content.models.test", &json!({"id": "mine", "wait": true})).unwrap();
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("does not load"), "{r}");
    let m = find(&list(&mut s), "mine").clone();
    assert_eq!(m["selfTest"]["ok"], false, "{m}");
    // in the background: a row in the activity stack, then the same outcome
    assert_eq!(s.execute("content.models.test", &json!({"id": "mine"})).unwrap()["started"], true);
    assert!(matches!(finished(&s, "mine"), JobState::Failed(_)));
    let _ = std::fs::remove_dir_all(&d);
}

/// The real model (set `LIGHTCRAFT_VISION_DIR` to a folder with SigLIP 2 base 256 px's files; skipped otherwise)
/// passes its self-test and reports its speed.
#[test]
fn the_real_model_passes_its_self_test() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_VISION_DIR") else {
        eprintln!("skipped: set LIGHTCRAFT_VISION_DIR to the SigLIP 2 model folder");
        return;
    };
    let r = cm::self_test(Path::new(&dir), &manifest::builtin_siglip2_b16_256()).unwrap();
    eprintln!("{r}");
    assert_eq!(r["ok"], true);
    assert!(r["msPerImage"].as_f64().unwrap() > 0.0);
}

#[test]
fn a_damaged_second_folder_of_an_installed_model_leaves_the_installed_files_alone() {
    let d = temp("reinstall");
    let mut s = session(&d);
    let m = tiny("mine");
    let first = d.join("first");
    model_folder(&first, &m);
    s.execute("content.models.install", &json!({"path": first.to_string_lossy(), "acknowledged": true})).unwrap();
    // the same model again, its last file damaged: the files before it pass their checks
    let second = d.join("second");
    model_folder(&second, &m);
    let mut bytes = std::fs::read(second.join("config.json")).unwrap();
    bytes[0] ^= 1;
    std::fs::write(second.join("config.json"), bytes).unwrap();
    let e = s.execute("content.models.install", &json!({"path": second.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("config.json"), "{e}");
    let home = content_dir(&s).join("mine");
    for (n, b) in files() {
        assert_eq!(std::fs::read(home.join(n)).ok().as_deref(), Some(b.as_slice()), "{n} is as installed");
        assert!(!home.join(format!("{n}.part")).exists(), "no {n}.part left behind");
    }
    let l = list(&mut s);
    assert_eq!((find(&l, "mine")["state"].as_str(), l["model"].as_str()), (Some("installed"), Some("mine")));
    let _ = std::fs::remove_dir_all(&d);
}
