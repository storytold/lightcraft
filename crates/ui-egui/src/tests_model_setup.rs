//! Synthetic setup flows: model installation must resume the requested action on its original targets.
use std::path::{Path, PathBuf};

use dac_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use dac_denoise::manifest::{DenoiserManifest, Domain, Gain};
use dac_engine::Session;
use serde_json::json;

use crate::{DacApp, Services, model_setup, state::Dialog};

fn setup(name: &str) -> (DacApp, PathBuf, PhotoId) {
    let dir = std::env::temp_dir().join(format!("lc-model-flow-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), true).unwrap();
    s.set_denoise_models_dir(Some(dir.join("denoise")));
    s.execute("denoise.settings", &json!({"runOn": "cpu"})).unwrap();
    let id = PhotoId(s.catalog.photos().map(|p| p.id.0).max().unwrap() + 1);
    let mut raw = Photo::new(
        id,
        Source::File { path: dir.join("synthetic.dng").to_string_lossy().into_owned() },
        "synthetic.dng",
        "DNG",
        128,
        128,
        "2026-10-07",
    );
    raw.kind = MediaKind::Raw;
    s.commit("fixture", Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    (DacApp::new(s, Services::default()), dir, id)
}

fn denoise_file(dir: &Path) -> PathBuf {
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = dac_denoise::synthetic::smoothing_onnx();
    std::fs::write(source.join("model.onnx"), &bytes).unwrap();
    let m = DenoiserManifest {
        id: "test-smoothing".into(),
        name: "Synthetic smoothing".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: Some(bytes.len() as u64),
        provenance: "Synthetic tests".into(),
        domain: Domain::BayerToRgb,
        tile: 64,
        overlap: 16,
        gain: Gain::None,
    };
    std::fs::write(source.join("denoise-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    source.join("model.onnx")
}

fn install_denoise(app: &mut DacApp, dir: &Path) {
    app.run("denoise.models.install", json!({"path": denoise_file(dir), "acknowledged": true})).unwrap();
}

#[test]
fn accepting_a_local_model_installs_in_background_and_resumes_the_pending_action() {
    let (mut app, dir, raw) = setup("local-install");
    let path = denoise_file(&dir);
    app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    let info = app.run("denoise.models.inspect", json!({"path": path})).unwrap();
    assert!(crate::panels::denoise::install(&mut app, &info, false).is_err());
    assert!(!dir.join("denoise/test-smoothing/model.onnx").exists());
    let result = crate::panels::denoise::install(&mut app, &info, true).unwrap();
    assert_eq!(result["started"], "local-install");
    assert_eq!(app.ui.dialog, Some(Dialog::Settings { tab: "denoise".into() }));
    let ctx = egui::Context::default();
    for _ in 0..600 {
        crate::panels::denoise::pump(&mut app, &ctx);
        if app.session.denoise_photo_state(raw) != dac_engine::denoise::PhotoState::NoModel {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    resume(&mut app);
    assert_eq!(amount(&app, raw), 50.0);
    assert!(dir.join("denoise/test-smoothing/model.onnx").is_file());
    assert!(path.is_file(), "a user-supplied original is preserved");
    drop(app);
    let _ = std::fs::remove_dir_all(dir);
}

fn amount(app: &DacApp, id: PhotoId) -> f64 {
    app.session.develop_of(id).unwrap().enhance.denoise
}
fn resume(app: &mut DacApp) {
    model_setup::pump(app, &egui::Context::default());
}

#[test]
fn denoise_installation_enables_only_the_requesting_photo_once_and_is_undoable() {
    let (mut app, dir, raw) = setup("denoise");
    let other = app.session.active().unwrap();
    app.session.auto_sync = true;
    let r = app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    assert_eq!(r["setupRequired"], true);
    assert_eq!(app.ui.dialog, Some(Dialog::Settings { tab: "denoise".into() }));
    assert_eq!(amount(&app, raw), 0.0, "setup alone never changes the photo");
    app.run("library.select", json!({"ids": [other.0]})).unwrap();
    let undo = app.session.undo.len();
    install_denoise(&mut app, &dir);
    resume(&mut app);
    assert_eq!(amount(&app, raw), 50.0);
    assert_eq!(amount(&app, other), 0.0);
    assert_eq!(app.session.active(), Some(other));
    assert_eq!(app.session.undo.len(), undo + 1);
    resume(&mut app);
    assert_eq!(app.session.undo.len(), undo + 1, "a later frame cannot apply it again");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(amount(&app, raw), 0.0);
    app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    app.run("develop.set", json!({"ids": [raw.0], "control": "enhance.denoise", "value": 75})).unwrap();
    app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    assert_eq!(amount(&app, raw), 75.0, "enabling an already chosen amount keeps it");
    app.run("denoise.toggle", json!({"id": raw.0, "enabled": false})).unwrap();
    assert_eq!(amount(&app, raw), 75.0, "switching off keeps the Amount");
    assert_eq!(app.session.develop_of(raw).unwrap().denoise_amount(), 0.0, "but mixes nothing in");
    drop(app);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancelled_denoise_requests_do_not_apply_after_installation() {
    for (name, command, params) in [
        ("cancel-action", "modelSetup.cancel", json!({"kind": "denoise"})),
        ("cancel-toggle", "denoise.toggle", json!({"enabled": false})),
        ("cancel-download", "denoise.models.downloadCancel", json!({"id": "rawnind-bayer"})),
    ] {
        let (mut app, dir, raw) = setup(name);
        app.run("library.select", json!({"ids": [raw.0]})).unwrap();
        app.run("denoise.toggle", json!({"enabled": true})).unwrap();
        app.run(command, params).unwrap();
        install_denoise(&mut app, &dir);
        resume(&mut app);
        assert_eq!(amount(&app, raw), 0.0, "{name}");
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn pending_actions_do_not_cross_libraries_or_relinked_sources() {
    for changed in ["library", "source"] {
        let (mut app, dir, raw) = setup(changed);
        app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
        if changed == "library" {
            app.session.open_library(dir.join("other-library"), true).unwrap();
        } else {
            app.session
                .commit(
                    "relink",
                    Op::Relink {
                        id: raw,
                        file_name: "another.dng".into(),
                        source: Source::File { path: dir.join("another.dng").to_string_lossy().into_owned() },
                        format: Some("DNG".into()),
                    },
                )
                .unwrap();
        }
        install_denoise(&mut app, &dir);
        resume(&mut app);
        assert!(!model_setup::denoise_requested(&app, raw));
        assert!(app.session.catalog.photos().all(|p| p.develop.enhance.denoise == 0.0));
        assert!(app.ui.status.contains("cancelled"));
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn hostile_or_inapplicable_toggle_arguments_are_errors_without_changes() {
    let (mut app, dir, raw) = setup("hostile");
    let jpeg = app.session.active().unwrap();
    for params in [json!({"id": "wrong"}), json!({"id": u64::MAX}), json!({"id": raw.0, "enabled": 1}), json!({"id": jpeg.0, "enabled": true})] {
        assert!(app.run("denoise.toggle", params).is_err());
        assert_eq!(amount(&app, raw), 0.0);
    }
    assert!(app.run("modelSetup.cancel", json!({"kind": "nonsense"})).is_err());
    drop(app);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn model_installation_waits_for_an_active_slider_gesture_to_finish() {
    let (mut app, dir, raw) = setup("interaction");
    app.run("denoise.toggle", json!({"id": raw.0, "enabled": true})).unwrap();
    app.session.begin_interaction("Exposure").unwrap();
    install_denoise(&mut app, &dir);
    resume(&mut app);
    assert_eq!(amount(&app, raw), 0.0);
    assert!(app.session.interaction.is_some());
    app.session.end_interaction().unwrap();
    resume(&mut app);
    assert_eq!(amount(&app, raw), 50.0);
    drop(app);
    let _ = std::fs::remove_dir_all(dir);
}
