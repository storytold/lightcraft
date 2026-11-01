//! AI masks without the SAM 3 model (it is never required): every request is a clear error that
//! comes back at once, nothing waits on the network or the model, a damaged model is an error
//! (not a crash or a stuck "busy"), and the download command needs consent and fails cleanly.
//! No test touches the internet: downloads go to a local server.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::Session;
use crate::segment::{NOT_INSTALLED, Segmenter};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-sam3-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A folder that looks installed but holds no usable model.
fn damaged_model(name: &str) -> std::path::PathBuf {
    let d = tmp(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
    std::fs::write(d.join("vocab.json"), b"{}").unwrap();
    std::fs::write(d.join("merges.txt"), b"#version: 0.2\n").unwrap();
    d
}

/// Poll until the worker is idle (or `limit` passes), collecting messages.
fn settle(s: &mut Session, limit: Duration) -> Vec<String> {
    let t = Instant::now();
    let mut messages = Vec::new();
    loop {
        let p = s.segment_poll();
        messages.extend(p.messages);
        if !s.segmenter.busy() && !s.segmenter.detail_busy() {
            messages.extend(s.segment_poll().messages);
            return messages;
        }
        assert!(t.elapsed() < limit, "the worker never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn status_and_model_commands_without_the_model() {
    let mut s = Session::with_demo();
    let st = s.execute("segment.model.status", &json!({})).unwrap();
    assert_eq!(st["available"], Segmenter::AVAILABLE);
    assert_eq!(st["installed"], false);
    assert_eq!(st["download"]["running"], false);
    assert!(st["license"].as_str().unwrap().contains("SAM License"));
    // downloading needs the user's consent, and names the size and the licence
    let e = s.execute("segment.model.download", &json!({})).unwrap_err().to_string();
    assert!(e.contains("acknowledged") && e.contains("SAM License") && e.contains("GB"), "{e}");
    let e = s.execute("segment.model.download", &json!({"acknowledged": "yes"})).unwrap_err().to_string();
    assert!(e.contains("acknowledged"), "{e}");
    assert_eq!(s.execute("segment.model.cancel", &json!({})).unwrap()["cancelled"], false);
    if !Segmenter::AVAILABLE {
        assert!(s.execute("segment.model.download", &json!({"acknowledged": true})).is_err());
        return;
    }
    // no folder, then no download location: clear errors, nothing started
    let e = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("no folder"), "{e}");
    let dir = tmp("nomirrors");
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(dir.join("none.txt"));
    if s.segmenter.mirrors().is_empty() {
        let e = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap_err().to_string();
        assert!(e.contains("LIGHTCRAFT_SAM3_MIRRORS"), "{e}");
    }
    assert!(!s.segmenter.download_status().running);
}

#[cfg(feature = "sam")]
#[test]
fn a_failing_download_ends_with_an_error_and_never_blocks() {
    use std::io::{BufRead, BufReader, Write};
    // a local mirror that has nothing; its first answer waits until the test has made its second
    // request, so the download is still running then however the threads are scheduled (a 404 at
    // once could end it before, and the second request would rightly start a new one)
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/sam3", l.local_addr().unwrap());
    let (release, released) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut first = Some(released);
        for mut c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            if let Some(released) = first.take() {
                let _ = released.recv_timeout(Duration::from_secs(10));
            }
            let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<h1>404</h1>\n");
        }
    });
    let dir = tmp("download404");
    let mirrors = dir.with_extension("mirrors.txt");
    std::fs::write(&mirrors, format!("# test mirror\n{base}\n")).unwrap();
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(mirrors.clone());
    let t = Instant::now();
    let r = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap();
    assert_eq!(r["started"], true);
    assert!(t.elapsed() < Duration::from_secs(1), "starting a download returns at once");
    // a second request while it runs starts nothing
    let again = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap();
    assert!(again["started"] == false || again["downloading"] == false);
    let _ = release.send(());
    let t = Instant::now();
    while s.segmenter.download_status().running {
        assert!(t.elapsed() < Duration::from_secs(30), "the download never ended");
        // AI mask requests meanwhile: an error at once, never a wait
        let q = Instant::now();
        let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
        assert!(e.contains(NOT_INSTALLED), "{e}");
        assert!(q.elapsed() < Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = s.segmenter.download_status();
    assert!(!st.finished);
    assert!(!st.cancelled, "a failure is not a cancel");
    let e = st.error.unwrap();
    assert!(e.contains("not found"), "{e}");
    assert!(!s.segmenter.installed());
    // nothing was left behind under a real name, and no error page was saved
    for f in ["model.safetensors", "vocab.json", "merges.txt", "vocab.json.part"] {
        assert!(!dir.join(f).exists(), "{f}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&mirrors);
}

/// The app (background mode) never waits: with the model missing every request fails at once;
/// with a damaged model the requests are queued, fail on the worker, and come back as messages.
#[test]
fn background_requests_never_wait_and_failures_come_back_as_messages() {
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let id = s.active().unwrap();
    s.segmenter.dir = Some(tmp("missing"));
    s.segmenter.models_root = Some(tmp("missing-open"));
    for (cmd, p) in [("mask.add", json!({"kind": "object"})), ("mask.add", json!({"kind": "prompt", "text": "sky"})), ("segment.prepare", json!({}))]
    {
        let t = Instant::now();
        let e = s.execute(cmd, &p).unwrap_err().to_string();
        assert!(t.elapsed() < Duration::from_millis(500), "{cmd}");
        if Segmenter::AVAILABLE {
            // Object offers the open SAM 2.1, Describe needs SAM 3
            let want = if p["kind"] == "prompt" { NOT_INSTALLED } else { "The SAM 2.1 model is not installed" };
            assert!(e.starts_with(want), "{cmd}: {e}");
        } else {
            assert!(e.contains("not available"), "{cmd}: {e}");
        }
    }
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    if !Segmenter::AVAILABLE {
        return;
    }
    // a model folder whose weights are damaged
    let dir = damaged_model("damaged");
    s.segmenter.dir = Some(dir.clone());
    assert!(s.segmenter.installed());
    let t = Instant::now();
    s.execute("mask.add", &json!({"kind": "object"})).unwrap();
    let r = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap();
    assert_eq!(r["pending"], true);
    assert!(s.segmenter.pending_clicks().is_some_and(|p| p.hint.len() == 1));
    // a second click while the first is on its way builds on it
    let r = s.execute("mask.objectPoint", &json!({"x": 0.6, "y": 0.5, "exclude": true})).unwrap();
    assert_eq!((r["include"].as_u64(), r["exclude"].as_u64()), (Some(1), Some(1)));
    let r = s.execute("mask.add", &json!({"kind": "prompt", "text": "the sky"})).unwrap();
    assert_eq!(r["pending"], true);
    assert!(t.elapsed() < Duration::from_secs(1), "nothing waited for the model: {:?}", t.elapsed());
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(!messages.is_empty() && messages.iter().all(|m| !m.is_empty()), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("model.safetensors") || m.contains("SAM 3")), "{messages:?}");
    assert!(s.segmenter.pending_clicks().is_none());
    // the Object mask is there, without a selection; no Describe mask was made from nothing
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.masks.len(), 1);
    assert!(matches!(&d.masks[0].components[0].shape, lightcraft_develop::MaskShape::Object { seg: None, .. }));
    assert!(!s.segmenter.busy() && !s.segmenter.loaded());
    let _ = std::fs::remove_dir_all(&dir);
}

/// CLI / MCP (waiting mode): a damaged model is an error from the command, quickly.
#[test]
fn waiting_requests_with_a_damaged_model_are_errors() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let dir = damaged_model("damaged-wait");
    s.segmenter.dir = Some(dir.clone());
    let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
    assert!(e.contains("model.safetensors") || e.contains("SAM 3"), "{e}");
    let e = s.execute("mask.add", &json!({"kind": "object", "points": [[0.5, 0.5]]})).unwrap_err().to_string();
    assert!(!e.is_empty());
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    assert!(!s.segmenter.busy());
    // a stored segmentation needs no model at all (replayed or pasted masks)
    let seg = serde_json::to_value(lightcraft_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    s.segmenter.dir = None;
    s.execute("mask.add", &json!({"kind": "prompt", "text": "sky", "seg": seg})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().masks.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Too many clicks on one Object selection are refused, not sent to the model.
#[test]
fn object_clicks_are_capped() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let dir = damaged_model("clicks");
    s.segmenter.dir = Some(dir.clone());
    let id = s.active().unwrap();
    let mut d = (*s.develop_of(id).unwrap()).clone();
    let many = vec![lightcraft_geom::Point::new(0.5, 0.5); crate::segment::MAX_CLICKS];
    d.masks.push(lightcraft_develop::Mask {
        id: 1,
        components: vec![lightcraft_develop::MaskComponent {
            name: None,
            op: lightcraft_develop::MaskOp::Add,
            invert: false,
            shape: lightcraft_develop::MaskShape::Object { hint: many, exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    s.set_develop(id, d, "test").unwrap();
    s.active_mask = Some(1);
    let e = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap_err().to_string();
    assert!(e.contains("at most"), "{e}");
    assert!(!s.segmenter.busy());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "sam")]
#[test]
fn a_running_model_download_shows_in_activity_and_cancels() {
    use std::io::{BufRead, BufReader, Write};
    // a local mirror that sends its files slowly, so the download is still running when it is cancelled
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/sam3", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n");
            for _ in 0..250 {
                if c.write_all(&[b' '; 4000]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });
    let dir = tmp("download-activity");
    let mirrors = dir.with_extension("activity-mirrors.txt");
    std::fs::write(&mirrors, format!("# test mirror\n{base}\n")).unwrap();
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(mirrors.clone());
    assert_eq!(s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap()["started"], true);
    let rows = s.activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("download", "Downloading model", "SAM 3"));
    assert_eq!(rows[0].unit, crate::activity::Unit::Bytes);
    assert!(rows[0].cancellable);
    s.execute("activity.cancel", &json!({"id": rows[0].id})).unwrap();
    let t = Instant::now();
    while s.segmenter.download_status().running || !s.activity.list().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(10), "the download never stopped");
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = s.segmenter.download_status();
    assert!(st.cancelled, "the app tells a cancel by the flag (#1046): {st:?}");
    let e = st.error.unwrap_or_default();
    assert!(e.contains("cancelled"), "{e}");
    assert!(!s.segmenter.installed());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&mirrors);
}

/// Frontends decide on the error's kind, not its words (#1046): a missing model is
/// `ModelMissing(Sam3)` whether the app queues requests (background) or a command waits (CLI, MCP);
/// a build without AI masks is not a missing model (there is nothing to download).
#[test]
fn a_missing_model_is_model_missing_by_kind() {
    use crate::{AiModel, ErrorKind};
    for background in [true, false] {
        let mut s = Session::with_demo();
        s.segmenter.background = background;
        s.segmenter.dir = Some(tmp(&format!("kind-{background}")));
        for (cmd, p) in
            [("mask.add", json!({"kind": "object"})), ("mask.add", json!({"kind": "prompt", "text": "sky"})), ("segment.prepare", json!({}))]
        {
            let e = s.execute(cmd, &p).unwrap_err();
            if Segmenter::AVAILABLE {
                assert_eq!(e.kind(), ErrorKind::ModelMissing(AiModel::Sam3), "{cmd} (background {background}): {e}");
                assert!(e.to_string().starts_with(NOT_INSTALLED), "the message is unchanged: {e}");
            } else {
                assert_eq!(e.kind(), ErrorKind::Other, "{cmd}: {e}");
            }
        }
    }
}

/// The openly licensed AI models (`ai.models.*`): listed with size and licence, downloads need consent and a
/// known id, and a model whose files are in place counts as installed (nothing is downloaded).
#[test]
fn open_models_are_listed_need_consent_and_are_found_in_the_models_folder() {
    let mut s = Session::with_demo();
    let root = tmp("open-models");
    s.segmenter.models_root = Some(root.clone());
    let st = s.execute("ai.models.status", &json!({})).unwrap();
    assert_eq!(st["available"], Segmenter::AVAILABLE);
    let models = st["models"].as_array().unwrap();
    if !Segmenter::AVAILABLE {
        assert!(models.is_empty());
        assert!(s.execute("ai.models.download", &json!({"id": "scene", "acknowledged": true})).is_err());
        return;
    }
    let ids: Vec<&str> = models.iter().filter_map(|m| m["id"].as_str()).collect();
    for id in ["people", "face", "subject", "scene", "sam2", "sam3"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    for m in models {
        assert_eq!(m["installed"], false, "{m}");
        assert!(m["sizeBytes"].as_u64().unwrap() > 0 || m["id"] == "sam3", "{m}");
        if m["id"] != "sam3" {
            assert_eq!(m["open"], true, "{m}");
            assert!(m["mirrors"].as_u64().unwrap() >= 1, "openly licensed models download without configuration: {m}");
            assert!(["MIT", "Apache-2.0"].contains(&m["license"].as_str().unwrap()), "{m}");
        }
    }
    // consent, and a known id
    let e = s.execute("ai.models.download", &json!({"id": "scene"})).unwrap_err().to_string();
    assert!(e.contains("acknowledged") && e.contains("MB") && e.contains("MIT"), "{e}");
    let e = s.execute("ai.models.download", &json!({"acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("scene"), "names the models: {e}");
    let e = s.execute("ai.models.download", &json!({"id": "nope", "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("unknown"), "{e}");
    assert_eq!(s.execute("ai.models.cancel", &json!({"id": "scene"})).unwrap()["cancelled"], false);
    // missing: an error that says how to get it
    let e = s.segmenter.require_model("scene").unwrap_err();
    assert!(e.contains(crate::segment::models::MODEL_NOT_INSTALLED) && e.contains("ai.models.download"), "{e}");
    assert_eq!(crate::segment::models::missing_model(&e), Some("scene"), "the UI can offer the download: {e}");
    assert_eq!(crate::segment::models::missing_model("something else"), None);
    let typed = crate::segment::models::segment_error(e.clone());
    assert_eq!(typed.kind(), crate::ErrorKind::ModelMissing(crate::AiModel::Open("scene")), "typed for the frontend");
    assert_eq!(typed.to_string(), e, "the message is kept");
    assert_eq!(crate::segment::models::segment_error("something else".into()).kind(), crate::ErrorKind::Other);
    // in place (a file of the pinned size): installed, and a download starts nothing
    let dir = root.join("eomt-s-coco");
    std::fs::create_dir_all(&dir).unwrap();
    let f = std::fs::File::create(dir.join("model.safetensors")).unwrap();
    f.set_len(95_927_796).unwrap();
    drop(f);
    assert!(s.segmenter.require_model("scene").is_err(), "config.json is part of the model");
    std::fs::write(dir.join("config.json"), vec![b' '; 6_426]).unwrap();
    assert_eq!(s.segmenter.require_model("scene").unwrap(), dir);
    let r = s.execute("ai.models.download", &json!({"id": "scene", "acknowledged": true})).unwrap();
    assert_eq!((r["started"].as_bool(), r["installed"].as_bool()), (Some(false), Some(true)), "{r}");
    let st = s.execute("ai.models.status", &json!({})).unwrap();
    assert!(st["models"].as_array().unwrap().iter().any(|m| m["id"] == "scene" && m["installed"] == true), "{st}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Subject, Sky, Background, People and Landscape without their model: made by the heuristic, and
/// the result says so (naming the model to get); Update says what's missing.
#[test]
fn automatic_masks_without_their_model_are_approximate() {
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(tmp("auto-none"));
    s.segmenter.dir = Some(tmp("auto-none-sam3"));
    for (p, model) in [
        (json!({"kind": "subject"}), "subject"),
        (json!({"kind": "background"}), "subject"),
        (json!({"kind": "sky"}), "scene"),
        (json!({"kind": "people"}), "people"),
        (json!({"kind": "people", "parts": ["Lips"]}), "face"),
        (json!({"kind": "landscape", "class": "water"}), "scene"),
    ] {
        let r = s.execute("mask.add", &p).unwrap();
        if Segmenter::AVAILABLE {
            assert_eq!((r["approximate"].as_bool(), r["model"].as_str()), (Some(true), Some(model)), "{p}: {r}");
            assert!(r["note"].as_str().unwrap().contains("ai.models.download"), "{r}");
        } else {
            assert!(r.get("approximate").is_none(), "{r}");
        }
        assert!(r.get("pending").is_none(), "nothing to wait for: {r}");
    }
    let e = s.execute("mask.add", &json!({"kind": "landscape", "class": "lava"})).unwrap_err().to_string();
    assert!(e.contains("naturalGround"), "{e}");
    // (review of PR #2706) the model named is the one that is missing: one person needs the face
    // model though the people model is in place, face parts beside hair need the people model
    if Segmenter::AVAILABLE {
        let put = |root: &std::path::Path, dir: &str, file: &str, size: u64| {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::File::create(root.join(dir).join(file)).unwrap().set_len(size).unwrap();
        };
        let root = tmp("auto-people-only");
        put(&root, "mediapipe-selfie-multiclass", "selfie_multiclass_256x256.tflite", 16_371_837);
        s.segmenter.models_root = Some(root);
        let r = s.execute("mask.add", &json!({"kind": "people", "person": 2})).unwrap();
        assert_eq!(r["model"], "face", "{r}");
        let root = tmp("auto-face-only");
        put(&root, "mediapipe-face-landmarker", "face_landmarker.task", 3_758_596);
        s.segmenter.models_root = Some(root);
        let r = s.execute("mask.add", &json!({"kind": "people", "parts": ["Hair", "Lips"]})).unwrap();
        assert_eq!(r["model"], "people", "{r}");
        s.segmenter.models_root = Some(tmp("auto-none"));
    }
    // a People part the models don't know is refused, not stored selecting nothing
    let e = s.execute("mask.add", &json!({"kind": "people", "parts": ["Hair", "Elbows"]})).unwrap_err().to_string();
    assert!(e.contains("Elbows") && e.contains("iris and pupil"), "{e}");
    // a stored segmentation needs no model
    let seg = serde_json::to_value(lightcraft_develop::SegMask::from_logits(2, &[4.0, -4.0, 4.0, -4.0])).unwrap();
    let r = s.execute("mask.add", &json!({"kind": "sky", "seg": seg})).unwrap();
    assert!(r.get("approximate").is_none(), "{r}");
    if Segmenter::AVAILABLE {
        s.execute("mask.add", &json!({"kind": "subject"})).unwrap();
        let e = s.execute("mask.recompute", &json!({})).unwrap_err().to_string();
        assert!(e.contains(crate::segment::models::MODEL_NOT_INSTALLED), "{e}");
    }
}

/// `mask.recompute` with a malformed `component` is an error: it never falls back to recomputing
/// every automatic selection of the mask (one model run each).
#[test]
fn recomputing_a_malformed_component_is_refused() {
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(tmp("recompute-bad"));
    s.segmenter.dir = Some(tmp("recompute-bad-sam3"));
    let seg = serde_json::to_value(lightcraft_develop::SegMask::from_logits(2, &[4.0, -4.0, 4.0, -4.0])).unwrap();
    s.execute("mask.add", &json!({"kind": "sky", "seg": seg})).unwrap();
    for component in [json!(-1), json!("0"), json!(0.5)] {
        let e = s.execute("mask.recompute", &json!({"component": component})).unwrap_err().to_string();
        assert!(e.contains("component"), "{component}: {e}");
    }
    // the stored segmentation is untouched
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert!(d.masks.iter().any(|m| matches!(m.components[0].shape, lightcraft_develop::MaskShape::Sky { seg: Some(_) })));
}

/// A model whose files are in place but unusable: an error, never a crash or a stuck worker; in
/// the app the mask is added at once (heuristic) and the failure comes back as a message.
#[cfg(feature = "sam")]
#[test]
fn a_damaged_automatic_model_is_an_error_not_a_crash() {
    let root = tmp("auto-damaged");
    let dir = root.join("eomt-s-coco");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::File::create(dir.join("model.safetensors")).unwrap().set_len(95_927_796).unwrap();
    std::fs::write(dir.join("config.json"), vec![b' '; 6_426]).unwrap();
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(root.clone());
    let e = s.execute("mask.add", &json!({"kind": "sky"})).unwrap_err().to_string();
    assert!(e.contains("EoMT"), "{e}");
    assert!(!s.segmenter.busy());
    s.segmenter.background = true;
    let t = Instant::now();
    let r = s.execute("mask.add", &json!({"kind": "sky"})).unwrap();
    assert_eq!(r["pending"], true, "{r}");
    assert!(t.elapsed() < Duration::from_secs(1), "the app never waits");
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(messages.iter().any(|m| m.contains("EoMT")), "{messages:?}");
    // the mask is there (heuristic), without a segmentation
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert!(d.masks.iter().any(|m| matches!(m.components[0].shape, lightcraft_develop::MaskShape::Sky { seg: None })));
    let _ = std::fs::remove_dir_all(&root);
}

/// With the real models (`LIGHTCRAFT_AI_MODELS_TEST_DIR`: a models folder holding them, e.g. the
/// app's `<settings>/models`): every automatic mask the installed models allow is computed, stored
/// with the mask, and the analysis shows in the activity stack while it runs. Skipped without it.
#[cfg(feature = "sam")]
#[test]
fn automatic_masks_with_the_real_models() {
    let Some(root) = std::env::var_os("LIGHTCRAFT_AI_MODELS_TEST_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set LIGHTCRAFT_AI_MODELS_TEST_DIR");
        return;
    };
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(root);
    s.segmenter.background = true;
    let id = s.active().unwrap();
    let mut wanted = Vec::new();
    for p in [
        json!({"kind": "subject"}),
        json!({"kind": "sky"}),
        json!({"kind": "landscape", "class": "vegetation"}),
        json!({"kind": "people"}),
        json!({"kind": "people", "parts": ["Hair", "Lips"]}),
    ] {
        let r = s.execute("mask.add", &p).unwrap();
        if r["pending"] == true {
            wanted.push(r["activeMask"].as_u64().unwrap());
        }
    }
    let mut saw_row = false;
    let t = Instant::now();
    let mut messages = Vec::new();
    while s.segmenter.busy() {
        saw_row |= s.activity.list().iter().any(|r| r.kind == "ai");
        messages.extend(s.segment_poll().messages);
        assert!(t.elapsed() < Duration::from_secs(600), "never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    messages.extend(s.segment_poll().messages);
    eprintln!("computed {} masks; messages: {messages:?}", wanted.len());
    assert!(s.activity.list().iter().all(|r| r.kind != "ai"), "rows end with the work");
    let d = s.develop_of(id).unwrap();
    for m in &wanted {
        let mask = d.masks.iter().find(|x| u64::from(x.id) == *m).unwrap();
        let shape = &mask.components[0].shape;
        let stored = crate::segment::AutoKind::of(shape).and_then(|k| k.seg_of(shape).cloned()).flatten();
        // (the demo photos have no people: those may come back as "not found")
        if stored.is_none() {
            assert!(!messages.is_empty(), "mask {m}: no segmentation and no message");
        }
    }
    assert!(wanted.is_empty() || saw_row, "the analysis showed in the activity stack");
}
