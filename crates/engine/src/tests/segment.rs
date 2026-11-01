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
    for (cmd, p) in [
        ("mask.add", json!({"kind": "object"})),
        ("mask.add", json!({"kind": "prompt", "text": "sky"})),
        ("mask.add", json!({"kind": "people", "face": [0.4, 0.2, 0.5, 0.35], "parts": ["Hair"]})),
        ("segment.prepare", json!({})),
    ] {
        let t = Instant::now();
        let e = s.execute(cmd, &p).unwrap_err().to_string();
        assert!(t.elapsed() < Duration::from_millis(500), "{cmd}");
        if Segmenter::AVAILABLE {
            assert!(e.starts_with(NOT_INSTALLED), "{cmd}: {e}");
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
    let r = s.execute("mask.add", &json!({"kind": "people", "face": [0.4, 0.2, 0.5, 0.35], "parts": ["Skin"]})).unwrap();
    assert_eq!(r["pending"], true);
    assert!(t.elapsed() < Duration::from_secs(1), "nothing waited for the model: {:?}", t.elapsed());
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(!messages.is_empty() && messages.iter().all(|m| !m.is_empty()), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("model.safetensors") || m.contains("SAM 3")), "{messages:?}");
    assert!(s.segmenter.pending_clicks().is_none());
    // the Object mask is there, without a selection; no Describe or People mask was made from nothing
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
        for (cmd, p) in [
            ("mask.add", json!({"kind": "object"})),
            ("mask.add", json!({"kind": "prompt", "text": "sky"})),
            ("mask.add", json!({"kind": "people", "face": [0.4, 0.2, 0.5, 0.35]})),
            ("segment.prepare", json!({})),
        ] {
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

fn face_region(rect: [f64; 4], name: Option<&str>) -> lightcraft_meta::Region {
    let r = lightcraft_geom::Rect { x0: rect[0], y0: rect[1], x1: rect[2], y1: rect[3] };
    lightcraft_meta::Region { rect: r, kind: lightcraft_meta::RegionKind::Face, name: name.map(str::to_string), description: None }
}

fn set_regions(s: &mut Session, id: lightcraft_catalog::PhotoId, regions: Vec<lightcraft_meta::Region>) {
    let mut meta = s.catalog.photo(id).unwrap().meta.clone();
    meta.regions = regions;
    s.commit("setup", lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
}

/// `photo.faces` lists the face regions (not pets) in mask coordinates too, and a People mask
/// takes its person by index or by box; a stored selection needs no model, and the parameters
/// are checked before the model is.
#[test]
fn people_masks_pick_a_face_and_check_their_parameters() {
    use lightcraft_develop::MaskShape;
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let pet = lightcraft_meta::Region { kind: lightcraft_meta::RegionKind::Pet, ..face_region([0.0, 0.0, 0.1, 0.1], None) };
    set_regions(&mut s, id, vec![pet, face_region([0.1, 0.2, 0.3, 0.4], Some("Ann")), face_region([0.6, 0.2, 0.8, 0.4], None)]);
    let faces = s.execute("photo.faces", &json!({})).unwrap()["faces"].clone();
    assert_eq!(faces.as_array().unwrap().len(), 2);
    assert_eq!((faces[0]["index"].as_u64(), faces[0]["region"].as_u64(), faces[0]["name"].as_str()), (Some(0), Some(1), Some("Ann")));
    assert_eq!(faces[1]["face"], json!([0.6, 0.2, 0.8, 0.4]), "upright photo: the same box");
    // after Rotate Right the box is in the turned frame (the left of the photo is its top)
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let turned = s.execute("photo.faces", &json!({})).unwrap()["faces"][0].clone();
    assert_eq!(turned["rect"], json!({"x0": 0.1, "y0": 0.2, "x1": 0.3, "y1": 0.4}), "the region itself is unchanged");
    let f: Vec<f64> = serde_json::from_value(turned["face"].clone()).unwrap();
    assert!((f[0] - 0.6).abs() < 1e-9 && (f[1] - 0.1).abs() < 1e-9 && (f[2] - 0.8).abs() < 1e-9 && (f[3] - 0.3).abs() < 1e-9, "{f:?}");
    s.execute("photo.rotateLeft", &json!({})).unwrap();

    // parameters: a face that isn't there, neither face nor person, an unknown part, a bad box
    for (p, want) in [
        (json!({"kind": "people", "person": 5}), "no face 5"),
        (json!({"kind": "people"}), "needs `person`"),
        (json!({"kind": "people", "person": 0, "parts": ["Lips"]}), "Lips"),
        (json!({"kind": "people", "face": [0.5, 0.5, 0.2, 0.9]}), "inside the photo"),
        (json!({"kind": "people", "face": "left"}), "inside the photo"),
        (json!({"kind": "people", "person": 0, "parts": "Hair"}), "list of names"),
    ] {
        let e = s.execute("mask.add", &p).unwrap_err().to_string();
        assert!(e.contains(want), "{p}: {e}");
    }
    assert!(s.develop_of(id).unwrap().masks.is_empty());

    // a stored selection: no model needed; a box alone takes the index of the face it matches
    let seg = serde_json::to_value(lightcraft_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    s.execute("mask.add", &json!({"kind": "people", "face": [0.61, 0.21, 0.79, 0.41], "parts": ["Hair"], "seg": seg})).unwrap();
    s.execute("mask.addComponent", &json!({"op": "subtract", "kind": "people", "person": 0, "parts": ["Skin"], "seg": seg})).unwrap();
    let d = s.develop_of(id).unwrap();
    let c = &d.masks[0].components;
    let MaskShape::People { person, parts, face, seg: Some(_), .. } = &c[0].shape else { panic!("{:?}", c[0].shape) };
    assert_eq!((*person, parts.as_slice(), *face), (1, &["Hair".to_string()][..], Some([0.61, 0.21, 0.79, 0.41])));
    let MaskShape::People { person, face, .. } = &c[1].shape else { panic!("{:?}", c[1].shape) };
    assert_eq!((*person, *face, c[1].op), (0, Some([0.1, 0.2, 0.3, 0.4]), lightcraft_develop::MaskOp::Subtract));
}

/// With `LIGHTCRAFT_SAM3_DIR` set: a People request runs through the real model, end to end
/// (in waiting and in background mode). The demo photos have no people, so on them it checks the
/// plumbing (the request finishes, with a selection or a "no person" answer); a portrait given
/// as `LIGHTCRAFT_PEOPLE_PHOTO` (and its face box as `LIGHTCRAFT_PEOPLE_FACE`, `x0,y0,x1,y1` on
/// normalized; default: its first face region) must give a selection.
#[cfg(feature = "sam")]
#[test]
fn people_masks_with_the_real_model() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_SAM3_DIR").map(std::path::PathBuf::from).filter(|d| lightcraft_segment::is_model_dir(d)) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SAM3_DIR to the SAM 3 model folder");
        return;
    };
    let photo = std::env::var_os("LIGHTCRAFT_PEOPLE_PHOTO").map(std::path::PathBuf::from);
    let mut s = match &photo {
        Some(path) => {
            let mut s = Session::new().with_fs();
            s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
            let id = s.catalog.photos().next().unwrap().id;
            s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
            s
        }
        None => {
            lightcraft_testkit::skip!(Input, "set LIGHTCRAFT_PEOPLE_PHOTO to a photo with people (the demo photos have none)");
            Session::with_demo()
        }
    };
    s.segmenter.dir = Some(dir);
    let id = s.active().unwrap();
    let face: Vec<f64> = match std::env::var("LIGHTCRAFT_PEOPLE_FACE") {
        Ok(v) => v.split(',').map(|x| x.trim().parse().unwrap()).collect(),
        Err(_) => serde_json::from_value(s.execute("photo.faces", &json!({})).unwrap()["faces"][0]["face"].clone())
            .unwrap_or_else(|_| vec![0.42, 0.25, 0.58, 0.45]),
    };
    let t = Instant::now();
    let r = s.execute("mask.add", &json!({"kind": "people", "face": face, "parts": ["Hair", "Skin"]}));
    eprintln!(
        "People (Hair, Skin) on {}: {:?} in {:?}",
        photo.as_deref().map_or("a demo photo".into(), |p| p.display().to_string()),
        r.as_ref().map(|_| "a mask"),
        t.elapsed()
    );
    match &r {
        Ok(_) => {
            let d = s.develop_of(id).unwrap();
            let lightcraft_develop::MaskShape::People { seg: Some(seg), .. } = &d.masks[0].components[0].shape else { panic!("a People selection") };
            assert!(seg.logits().is_some_and(|l| crate::segment::people::selects_anything(&l)));
            // the zoomed-in pass runs too (or says there's nothing to refine)
            s.execute("mask.refineDetail", &json!({})).unwrap();
        }
        Err(e) => {
            assert!(photo.is_none(), "a photo with people gives a selection: {e}");
            let e = e.to_string();
            assert!(e.contains("No person") || e.contains("was found on this person"), "{e}");
        }
    }
    // background mode: queued, then applied (or a message) by polling
    s.segmenter.background = true;
    let before = s.develop_of(id).unwrap().masks.len();
    let r = s.execute("mask.add", &json!({"kind": "people", "face": face, "parts": ["Entire Person"]})).unwrap();
    assert_eq!(r["pending"], true);
    let messages = settle(&mut s, Duration::from_secs(600));
    let after = s.develop_of(id).unwrap().masks.len();
    assert!(after == before + 1 || messages.iter().any(|m| m.contains("No person")), "{messages:?}");
    if photo.is_some() {
        assert_eq!(after, before + 1, "{messages:?}");
    }
}
