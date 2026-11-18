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
        assert!(e.contains(&lcc_brand::env_var("SAM3_MIRRORS")), "{e}");
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
        let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
        assert!(e.contains(NOT_INSTALLED), "{e}");
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
    for (cmd, p) in [("mask.add", json!({"kind": "object"})), ("mask.add", json!({"kind": "prompt", "text": "sky"})), ("segment.prepare", json!({}))]
    {
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
    assert!(t.elapsed() < Duration::from_secs(1), "nothing waited for the model: {:?}", t.elapsed());
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(!messages.is_empty() && messages.iter().all(|m| !m.is_empty()), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("model.safetensors") || m.contains("SAM 3")), "{messages:?}");
    assert!(s.segmenter.pending_clicks().is_none());
    // the Object mask is there, without a selection; no Describe mask was made from nothing
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.masks.len(), 1);
    assert!(matches!(&d.masks[0].components[0].shape, lcc_develop::MaskShape::Object { seg: None, .. }));
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
    let seg = serde_json::to_value(lcc_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    s.segmenter.dir = None;
    s.execute("mask.add", &json!({"kind": "prompt", "text": "sky", "seg": seg})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().masks.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// In background mode a new Object mask with too many clicks is refused up front, not queued (upstream #3717).
#[test]
fn background_object_clicks_over_the_cap_are_refused_before_queueing() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let dir = damaged_model("later-clicks");
    s.segmenter.dir = Some(dir.clone());
    let id = s.active().unwrap();
    let points: Vec<_> = (0..=crate::segment::MAX_CLICKS).map(|_| json!([0.5, 0.5])).collect();
    let e = s.execute("mask.add", &json!({"kind": "object", "points": points})).unwrap_err().to_string();
    assert!(e.contains("at most"), "{e}");
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    assert!(!s.segmenter.busy());
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
    let many = vec![lcc_geom::Point::new(0.5, 0.5); crate::segment::MAX_CLICKS];
    d.masks.push(lcc_develop::Mask {
        id: 1,
        components: vec![lcc_develop::MaskComponent {
            anchor: None,
            name: None,
            op: lcc_develop::MaskOp::Add,
            invert: false,
            shape: lcc_develop::MaskShape::Object { hint: many, exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
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

/// The Subject model is optional: without it the status says so and the download needs consent;
/// with (tiny, synthetic) weights in its folder it becomes the pipeline's subject model, whose
/// runs show in the activity stack, and Subject masks take its selection.
#[test]
fn subject_model_is_optional_and_used_once_installed() {
    let mut s = Session::with_demo();
    let st = s.execute("segment.subject.status", &json!({})).unwrap();
    assert_eq!((st["installed"].as_bool(), st["active"].as_bool()), (Some(false), Some(false)));
    assert!(s.execute("segment.subject.download", &json!({})).is_err());
    if !Segmenter::AVAILABLE {
        return;
    }
    // no folder set: an error, no download
    assert!(s.execute("segment.subject.download", &json!({"acknowledged": true})).is_err());
    #[cfg(feature = "sam")]
    {
        use lcc_develop::{DevelopSettings, Mask, MaskComponent, MaskShape};
        let dir = tmp("subject");
        std::fs::create_dir_all(&dir).unwrap();

        lcc_segment::birefnet::write_tiny_checkpoint(&dir.join(lcc_segment::birefnet::WEIGHTS_FILE), 8).unwrap();
        s.segmenter.subject.dir = Some(dir.clone());
        let st = s.execute("segment.subject.status", &json!({})).unwrap();
        assert_eq!((st["installed"].as_bool(), st["active"].as_bool()), (Some(true), Some(true)));
        // a Subject mask on a demo photo runs the model (with a row in the activity stack)
        let scene = &lcc_scenes::demo_library()[0];
        let src = scene.render_fit(200);
        let mut set = DevelopSettings::default();
        set.masks.push(Mask {
            components: vec![MaskComponent { anchor: None, name: None, op: Default::default(), invert: false, shape: MaskShape::Subject }],
            ..Default::default()
        });
        let info = lcc_pipeline::SourceInfo::default();
        // a Sky mask never waits for the Subject model (#969)
        let mut sky = DevelopSettings::default();
        sky.masks.push(Mask {
            components: vec![MaskComponent { anchor: None, name: None, op: Default::default(), invert: false, shape: MaskShape::Sky }],
            ..Default::default()
        });
        let sky_auto = lcc_pipeline::auto_mask::for_source(&src, &info, &sky);
        assert_ne!((sky_auto.subject.width, sky_auto.subject.height), (1024, 1024), "a Sky mask ran the Subject model");
        let job = std::thread::spawn(move || lcc_pipeline::auto_mask::for_source(&src, &info, &set));
        let mut seen = false;
        while !job.is_finished() {
            seen |= s.activity.list().iter().any(|t| t.kind == "segment");
            std::thread::sleep(Duration::from_millis(5));
        }
        let auto = job.join().unwrap();
        assert!(seen, "no activity row while the model ran");
        assert_eq!((auto.subject.width, auto.subject.height), (1024, 1024), "the model's grid");
        assert!(s.activity.list().iter().all(|t| t.kind != "segment"), "the row goes when the run ends");
        // removing the weights removes the model
        std::fs::remove_dir_all(&dir).unwrap();
        s.execute("segment.subject.status", &json!({})).unwrap();
        assert!(!lcc_pipeline::auto_mask::has_subject_model());
    }
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

fn prompt_mask(s: &mut Session, id: lcc_catalog::PhotoId, text: &str) {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    let next = d.masks.iter().map(|m| m.id).max().unwrap_or(0) + 1;
    d.masks.push(lcc_develop::Mask {
        id: next,
        components: vec![lcc_develop::MaskComponent {
            anchor: None,
            name: None,
            op: lcc_develop::MaskOp::Add,
            invert: false,
            shape: lcc_develop::MaskShape::Prompt { text: text.into(), seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    s.set_develop(id, d, "Preset").unwrap();
}

fn ai_rows(s: &Session) -> usize {
    s.activity.list().iter().filter(|t| t.kind == "aiMasks").count()
}

/// The app's Recompute AI Masks returns at once and computes on the AI worker as one activity row;
/// undo cancels it, redo computes again, and a preset bringing a Describe mask starts it by itself.
#[test]
fn recompute_runs_on_the_worker_with_one_activity_row() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let dir = damaged_model("recompute");
    s.segmenter.dir = Some(dir.clone());
    let id = s.active().unwrap();
    prompt_mask(&mut s, id, "the sky");
    prompt_mask(&mut s, id, "the tree");
    let t = Instant::now();
    let r = s.execute("masks.recompute", &json!({})).unwrap();
    assert!(t.elapsed() < Duration::from_secs(1), "the UI thread didn't wait: {:?}", t.elapsed());
    assert_eq!(r["photos"][0]["queued"], 2, "{r}");
    assert_eq!(ai_rows(&s), 1, "one row for the batch");
    assert!(s.segmenter.recomputing());
    // undo cancels what is still computing: the row goes, late results are dropped
    s.undo_step().unwrap();
    assert!(!s.segmenter.recomputing());
    assert_eq!(ai_rows(&s), 0);
    // redo computes what is missing again (at the next poll, as in the app)
    s.redo_step().unwrap();
    let t = Instant::now();
    let _ = s.segment_poll();
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(ai_rows(&s), 1);
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(!s.segmenter.recomputing() && ai_rows(&s) == 0, "the row ends with the batch");
    assert!(messages.iter().any(|m| m.contains("model.safetensors") || m.contains("SAM 3")), "{messages:?}");
    // a preset (or paste) bringing a Describe mask starts the batch by itself
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks.clear();
    s.set_develop(id, d, "clear").unwrap();
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks.push(lcc_develop::Mask {
        id: 9,
        components: vec![lcc_develop::MaskComponent {
            anchor: None,
            name: None,
            op: lcc_develop::MaskOp::Add,
            invert: false,
            shape: lcc_develop::MaskShape::Prompt { text: "a dog".into(), seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    let d = crate::smart_masks::completed(&mut s, id, d);
    s.set_develop(id, d, "Preset").unwrap();
    // the batch starts by itself at the next poll; with the damaged model a busy machine can let the
    // worker fail it before we look, so either its row shows or its result has already come back
    let p = s.segment_poll();
    let started = ai_rows(&s) == 1 || s.segmenter.recomputing() || !p.messages.is_empty();
    assert!(started, "a preset bringing a Describe mask starts the batch: {:?}", p.messages);
    // ✕ on the row stops it
    if let Some(row) = s.activity.list().into_iter().find(|t| t.kind == "aiMasks") {
        s.activity.cancel(row.id).unwrap();
        let _ = s.segment_poll();
        assert_eq!(ai_rows(&s), 0);
    }
    let _ = settle(&mut s, Duration::from_secs(60));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Update AI Mask computes the mask again from this photo: what it held (a copied selection) is
/// dropped; an Object without clicks here has to be picked first; Update All AI Masks covers the
/// selection.
#[test]
fn update_ai_mask_replaces_what_the_mask_held() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let seg = serde_json::to_value(lcc_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    s.execute("mask.add", &json!({"kind": "prompt", "text": "sky", "seg": seg})).unwrap();
    let m = s.develop_of(id).unwrap().masks[0].id;
    assert!(s.develop_of(id).unwrap().masks[0].components[0].shape.has_seg());
    let r = s.execute("mask.updateAi", &json!({"id": m})).unwrap();
    assert!(r.get("notes").is_some(), "{r}");
    let d = s.develop_of(id).unwrap();
    assert!(!d.masks[0].components[0].shape.has_seg(), "the copied selection is gone, to be computed here");
    assert!(d.masks[0].needs_compute());
    // undo brings it back in one step
    s.undo_step().unwrap();
    assert!(s.develop_of(id).unwrap().masks[0].components[0].shape.has_seg());
    // an Object without clicks on this photo
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks.push(lcc_develop::Mask {
        id: 7,
        components: vec![lcc_develop::MaskComponent {
            anchor: None,
            name: None,
            op: lcc_develop::MaskOp::Add,
            invert: false,
            shape: lcc_develop::MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    s.set_develop(id, d, "paste").unwrap();
    let e = s.execute("mask.updateAi", &json!({"id": 7})).unwrap_err().to_string();
    assert!(e.contains("pick the object"), "{e}");
    assert!(s.execute("mask.updateAi", &json!({"id": 999})).is_err());
    let r = s.execute("masks.updateAi", &json!({})).unwrap();
    assert_eq!(r["photos"][0]["id"], id.0, "{r}");
    assert!(!s.develop_of(id).unwrap().masks[0].components[0].shape.has_seg());
}

/// With SAM 2.1 chosen for Object clicks, an Object mask without it is `ModelMissing(Sam2)`,
/// while Describe still asks for SAM 3; `ai.models.*` list, refuse without consent and reject
/// unknown ids.
#[test]
#[cfg(feature = "sam")] // the SAM 2.1 entry and model errors exist only in a build with AI masks
fn object_clicks_follow_the_chosen_model() {
    use crate::{AiModel, ErrorKind};
    let root = tmp("choice");
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(root.join("sam3"));
    s.segmenter.models_root = Some(root.clone());
    let st = s.execute("ai.models.status", &json!({})).unwrap();
    assert_eq!(st["objectModelInUse"], "sam3");
    assert!(st["models"].as_array().unwrap().iter().any(|m| m["id"] == "sam2"));
    assert!(s.execute("ai.models.setObjectModel", &json!({"model": "sam5"})).is_err());
    let r = s.execute("ai.models.setObjectModel", &json!({"model": "sam2"})).unwrap();
    assert_eq!(r["objectModelInUse"], "sam2");
    assert!(s.execute("ai.models.download", &json!({"id": "sam2"})).is_err(), "needs consent");
    assert!(s.execute("ai.models.download", &json!({"id": "nope", "acknowledged": true})).is_err());
    let e = s.execute("mask.add", &json!({"kind": "object"})).unwrap_err();
    let p = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err();
    if Segmenter::AVAILABLE {
        assert_eq!(e.kind(), ErrorKind::ModelMissing(AiModel::Sam2), "{e}");
        // without SAM 3, Describe runs on the open models: the OWLv2 finder and SAM 2.1
        assert_eq!(p.kind(), ErrorKind::ModelMissing(AiModel::Describe), "{p}");
        assert_eq!(s.segmenter.describe_needs(), vec!["describe", "sam2"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Installing or removing the Subject or scene model re-renders the photos whose AI masks use an
/// automatic selection (their render key moves on), and only those.
#[test]
fn a_model_change_re_renders_ai_masks_at_once() {
    let mut s = Session::with_demo();
    let plain = s.catalog.photos().map(|p| p.id).find(|p| Some(*p) != s.active()).unwrap();
    let id = s.active().unwrap();
    assert_ne!(id, plain);
    s.execute("mask.add", &json!({"kind": "sky"})).unwrap();
    let key = |s: &mut Session, id| s.render_job(id, 400, 300, false, true).unwrap().key;
    let (k_sky, k_plain) = (key(&mut s, id), key(&mut s, plain));
    lcc_pipeline::scene_model::set_scene_model(Some(std::sync::Arc::new(|_: &[u8], _: usize, _: usize| None)));
    lcc_pipeline::scene_model::set_scene_model(None);
    assert_ne!(key(&mut s, id), k_sky, "the Sky mask renders afresh");
    assert_eq!(key(&mut s, plain), k_plain, "a photo without AI masks keeps its render");
}

/// Select Landscape (`mask.add {kind: "landscape"}`): sky, water and vegetation come from the photo analysis and need no
/// model; mountains, architecture and ground need the EoMT scene model, so without it they are `ModelMissing(Scene)`
/// and add nothing. A class that is none of them is refused.
#[test]
fn select_landscape_adds_the_analysis_classes_and_asks_for_the_scene_model_for_the_rest() {
    use crate::{AiModel, ErrorKind};
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(tmp("landscape-models"));
    let masks = |s: &Session| s.active().and_then(|id| s.develop_of(id)).map_or(0, |d| d.masks.len());
    s.execute("mask.add", &json!({"kind": "landscape", "class": "Sky"})).unwrap();
    s.execute("mask.add", &json!({"kind": "landscape", "class": "lake"})).unwrap();
    s.execute("mask.add", &json!({"kind": "landscape", "class": "vegetation"})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    let classes: Vec<String> = d
        .masks
        .iter()
        .filter_map(|m| match &m.components[0].shape {
            lcc_develop::MaskShape::Landscape { class } => Some(class.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(classes, ["sky", "water", "vegetation"], "loose names are stored as the canonical class");
    let before = masks(&s);
    for class in ["mountains", "building", "sand", "road"] {
        let e = s.execute("mask.add", &json!({"kind": "landscape", "class": class})).unwrap_err();
        if Segmenter::AVAILABLE {
            assert_eq!(e.kind(), ErrorKind::ModelMissing(AiModel::Scene), "{class}: {e}");
        }
    }
    assert_eq!(masks(&s), before, "a refused class adds no mask");
    for bad in
        [json!({"kind": "landscape"}), json!({"kind": "landscape", "class": "dragons"}), json!({"kind": "landscape", "class": "x".repeat(500)})]
    {
        assert_eq!(s.execute("mask.add", &bad).unwrap_err().kind(), ErrorKind::BadParams, "{bad}");
    }
}

/// Remove ▸ Detect Objects (`spot.detectObject`): without an Object model it is an error and changes nothing; hostile
/// strokes (none, NaN, far outside the photo, huge) are errors or clamped, never a panic.
#[test]
fn detect_objects_without_a_model_changes_nothing() {
    use crate::ErrorKind;
    for background in [true, false] {
        let mut s = Session::with_demo();
        s.segmenter.background = background;
        s.segmenter.dir = Some(tmp(&format!("detect-{background}")));
        s.segmenter.models_root = Some(tmp(&format!("detect-open-{background}")));
        let id = s.active().unwrap();
        let counts = |s: &Session| s.develop_of(id).map_or((0, 0), |d| (d.masks.len(), d.spots.len()));
        let before = counts(&s);
        assert_eq!(s.execute("spot.detectObject", &json!({"points": []})).unwrap_err().kind(), ErrorKind::BadParams);
        assert!(s.execute("spot.detectObject", &json!({"points": [[0.5, 0.5]], "mode": "heal"})).is_err());
        let huge: Vec<_> = (0..20_000).map(|i| json!([f64::from(i) * 1e9, -1e12])).collect();
        for p in [json!({"points": [[0.4, 0.4], [0.5, 0.5]]}), json!({"points": huge}), json!({"points": [["x", null]]})] {
            assert!(s.execute("spot.detectObject", &p).is_err());
        }
        assert_eq!(counts(&s), before, "no mask or spot is left behind (background {background})");
    }
}

/// Depth Range (`mask.add {kind: "depthRange"}`) needs the Depth Anything model: without it the
/// error offers its download and no mask is added; hostile bounds are clamped, never a panic.
#[test]
fn depth_range_needs_the_depth_model() {
    use crate::{AiModel, ErrorKind};
    let mut s = Session::with_demo();
    s.segmenter.models_root = Some(tmp("depth-models"));
    let masks = |s: &Session| s.active().and_then(|id| s.develop_of(id)).map_or(0, |d| d.masks.len());
    let before = masks(&s);
    for p in [json!({"kind": "depthRange"}), json!({"kind": "depthRange", "lo": -5.0, "hi": 1e300, "feather": "x"})] {
        let e = s.execute("mask.add", &p).unwrap_err();
        if Segmenter::AVAILABLE {
            assert_eq!(e.kind(), ErrorKind::ModelMissing(AiModel::Depth), "{e}");
        }
    }
    assert_eq!(masks(&s), before, "a refused Depth Range adds no mask");
}
