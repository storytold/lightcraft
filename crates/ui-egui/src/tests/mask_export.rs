//! A control request must not wait for a slow source on the UI thread.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_raster::Rgb32f;
use serde_json::json;

use crate::control::{ControlRequest, Outcome, TAKEN};
use crate::{LightcraftApp, Services};

#[test]
fn control_matte_decode_is_bounded_tracked_and_off_the_ui_thread() {
    let mut session = lightcraft_engine::Session::new();
    let photo = Photo::new(PhotoId(1), Source::File { path: "slow.raw".into() }, "slow.raw", "RAW", 64, 64, "2026-01-01");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    session.execute("library.select", &json!({"ids": [1]})).unwrap();
    session.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let ui = std::thread::current().id();
    let (opened, open) = mpsc::channel();
    let open = Mutex::new(open);
    let (begun, began) = mpsc::channel();
    session.media.file_loader = Some(Arc::new(move |_, edge, _| {
        assert_ne!(std::thread::current().id(), ui);
        assert_eq!(edge, 2561, "a bounded source, never usize::MAX");
        begun.send(()).unwrap();
        open.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        Ok((Rgb32f::new(64, 64), Default::default()))
    }));
    let mut app = LightcraftApp::new(session, Services::default());
    let ctx = egui::Context::default();
    let file = std::env::temp_dir().join(format!("lc-control-matte-{}.png", std::process::id()));
    let (reply, answer) = mpsc::channel();
    let req = ControlRequest {
        method: "engine.execute".into(),
        params: json!({"command": "mask.export", "params": {"path": file, "longEdge": 2561}}),
        reply,
        state: Arc::new(std::sync::atomic::AtomicU8::new(TAKEN)),
    };
    assert!(matches!(crate::control::handle(&mut app, &ctx, &req), Outcome::Later));
    began.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(answer.try_recv().is_err());
    let rows = app.session.activity.list();
    assert!(rows.iter().any(|r| r.kind == "export" && r.label == crate::mask_export::LABEL && !r.cancellable));
    let inspect = ControlRequest { method: "ui.inspect".into(), params: json!({}), ..req };
    assert!(matches!(crate::control::handle(&mut app, &ctx, &inspect), Outcome::Done(_)));
    opened.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let result = loop {
        crate::tasks::poll(&mut app, &ctx);
        if let Ok(value) = answer.try_recv() {
            break value;
        }
        assert!(Instant::now() < deadline, "export never answered");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["width"], 2561);
    assert!(file.exists());
    assert!(!app.session.activity.list().iter().any(|r| r.label == crate::mask_export::LABEL));
    std::fs::remove_file(file).unwrap();
}
