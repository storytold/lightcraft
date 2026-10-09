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
    // a local mirror that has nothing
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/sam3", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
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
    assert!(matches!(&d.masks[0].components[0].shape, dac_develop::MaskShape::Object { seg: None, .. }));
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
    let seg = serde_json::to_value(dac_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
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
    let many = vec![dac_geom::Point::new(0.5, 0.5); crate::segment::MAX_CLICKS];
    d.masks.push(dac_develop::Mask {
        id: 1,
        components: vec![dac_develop::MaskComponent {
            name: None,
            op: dac_develop::MaskOp::Add,
            invert: false,
            shape: dac_develop::MaskShape::Object { hint: many, exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
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
