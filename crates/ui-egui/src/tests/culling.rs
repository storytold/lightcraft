//! Issue #1251: slow analysis never holds the owner thread, and cancellation commits nothing.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

struct Release(Arc<AtomicBool>);
impl Release {
    fn open(&self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.open();
    }
}

fn blocked() -> (Headless, Release, std::sync::mpsc::Receiver<bool>) {
    let mut session = lightcraft_engine::Session::new();
    let photo =
        Photo::new(PhotoId(1), Source::File { path: "synthetic-cull.png".into() }, "synthetic-cull.png", "PNG", 48, 32, "2026-01-01T00:00:00");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    session.execute("library.select", &json!({"ids": [1]})).unwrap();
    session.media.file_loader = Some(Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(48, 32), Default::default()))));
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 800.0], 1.0);
    assert!(h.settle(T));
    h.app.session.media.forget(PhotoId(1));
    let gate = Arc::new(AtomicBool::new(false));
    let work_gate = gate.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    h.app.session.media.file_loader = Some(Arc::new(move |_, _| {
        let _ = tx.send(lightcraft_engine::memory::is_background());
        while !work_gate.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok((lightcraft_raster::Rgb32f::new(48, 32), Default::default()))
    }));
    (h, Release(gate), rx)
}

#[test]
fn slow_culling_leaves_the_ui_responsive_and_cancel_discards_scores() {
    let (mut h, gate, entered) = blocked();
    let started = h.app.run("photo.analyze", json!({"ids": [1], "background": true})).unwrap();
    assert_eq!(started["background"], true);
    assert!(entered.recv_timeout(T).unwrap(), "culling decodes use the bounded background memory gate");
    assert!(h.busy(), "settling must wait for the background analysis");
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["ok"], true, "the worker is still blocked but control requests answer");
    let task = h.app.session.activity.list().into_iter().find(|t| t.kind == "culling").unwrap();
    assert!(task.cancellable);
    assert_eq!(task.total, 1);
    assert!(h.app.session.catalog.photo(PhotoId(1)).unwrap().analysis.is_none());
    assert!(h.app.run("photo.analyze", json!({"ids": [1]})).is_err(), "one run at a time");
    h.app.run("activity.cancel", json!({"id": task.id})).unwrap();
    gate.open();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    assert_eq!(h.app.last_culling_result.as_ref().unwrap()["cancelled"], true);
    assert!(h.app.session.catalog.photo(PhotoId(1)).unwrap().analysis.is_none());
    assert!(!h.app.session.activity.list().iter().any(|t| t.kind == "culling"));
}

#[test]
fn culling_commits_one_undo_step_and_reuses_scores_for_new_flag_choices() {
    let (mut h, gate, entered) = blocked();
    let undo = h.app.session.undo.len();
    h.app.run("photo.analyze", json!({"ids": [1], "background": true})).unwrap();
    entered.recv_timeout(T).unwrap();
    gate.open();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    assert!(h.app.session.catalog.photo(PhotoId(1)).unwrap().analysis.is_some());
    assert_eq!(h.app.session.undo.len(), undo + 1);
    h.app.session.media.forget(PhotoId(1));
    h.app.session.media.file_loader = Some(Arc::new(|_, _| Err("offline".into())));
    h.app.run("photo.analyze", json!({"ids": [1], "rejectBelow": 101, "background": true})).unwrap();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::Reject);
    assert_eq!(h.app.last_culling_result.as_ref().unwrap()["failed"], json!([]));
    h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::None);
}

#[test]
fn deleted_photo_during_analysis_is_not_scored_or_flagged() {
    let (mut h, gate, entered) = blocked();
    h.app.run("photo.analyze", json!({"ids": [1], "rejectBelow": 101, "background": true})).unwrap();
    entered.recv_timeout(T).unwrap();
    h.app.session.commit("Delete", Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
    let undo = h.app.session.undo.len();
    gate.open();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    let photo = h.app.session.catalog.photo(PhotoId(1)).unwrap();
    assert!(photo.deleted && photo.analysis.is_none());
    assert_eq!(photo.flag, lightcraft_catalog::Flag::None);
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(h.app.last_culling_result.as_ref().unwrap()["failed"][0][0], 1);
}

#[test]
fn unreadable_source_reports_failure_and_can_be_retried() {
    let (mut h, _gate, _entered) = blocked();
    h.app.session.media.file_loader = Some(Arc::new(|_, _| Err("source unreadable".into())));
    let undo = h.app.session.undo.len();
    h.app.run("photo.analyze", json!({"ids": [1], "rejectBelow": 101, "background": true})).unwrap();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    let report = h.app.last_culling_result.as_ref().unwrap();
    assert_eq!(report["photos"], json!([]));
    assert_eq!(report["failed"], json!([[1, "source unreadable"]]));
    assert!(!h.app.session.activity.list().iter().any(|t| t.kind == "culling"));
    assert!(h.app.session.catalog.photo(PhotoId(1)).unwrap().analysis.is_none());
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::None);
    assert_eq!(h.app.session.undo.len(), undo);
    h.app.session.media.file_loader = Some(Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(48, 32), Default::default()))));
    h.app.run("photo.analyze", json!({"ids": [1], "background": true})).unwrap();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    assert_eq!(h.app.last_culling_result.as_ref().unwrap()["failed"], json!([]));
    assert!(h.app.session.catalog.photo(PhotoId(1)).unwrap().analysis.is_some());
}

#[test]
fn switching_libraries_while_decoding_discards_the_old_result() {
    let dir = std::env::temp_dir().join(format!("lc-cull-running-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut h, gate, entered) = blocked();
    h.app.run("photo.analyze", json!({"ids": [1], "rejectBelow": 101, "background": true})).unwrap();
    entered.recv_timeout(T).unwrap();
    h.app.run("app.openLibrary", json!({"path": dir.to_string_lossy()})).unwrap();
    let undo = h.app.session.undo.len();
    gate.open();
    assert!(h.step_until(T, |h| h.app.culling.is_none()));
    assert!(h.app.last_culling_result.as_ref().unwrap()["error"].as_str().unwrap().contains("analysis was discarded"));
    assert!(!h.app.session.activity.list().iter().any(|t| t.kind == "culling"));
    assert_eq!(h.app.session.catalog.len(), 0);
    assert_eq!(h.app.session.undo.len(), undo);
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}
