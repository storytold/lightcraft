//! The UI decides on what kind of error a command returned, not on its words (#1046): messages get
//! reworded and translated, and a decision that reads them breaks silently when they do.

use lightcraft_engine::segment::{DownloadStatus, NOT_INSTALLED, Segmenter};
use lightcraft_engine::{AiModel, ErrorKind};
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{CommandError, LightcraftApp, Services};

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    Headless::new(app, [1200.0, 800.0], 1.0)
}

/// An AI mask error offers the model on `ModelMissing(Sam3)`, whatever the message says, and a
/// message that merely starts like the engine's is a toast.
#[test]
fn ai_mask_errors_offer_the_model_by_kind() {
    let mut h = demo();
    let ctx = h.view.ctx.clone();
    let missing = CommandError { message: "Das SAM-3-Modell ist nicht installiert".into(), kind: ErrorKind::ModelMissing(AiModel::Sam3) };
    h.app.ai_error(&ctx, missing, Some(("object", "new")));
    if Segmenter::AVAILABLE {
        assert_eq!(h.app.ui.dialog, Some(Dialog::SamModel { then: Some(("object".into(), "new".into())), error: None }));
    } else {
        assert_eq!(h.app.ui.dialog, None, "a build without AI masks has nothing to offer");
    }
    h.app.ui.dialog = None;
    h.app.ai_error(&ctx, format!("{NOT_INSTALLED}: but this is some other failure"), None);
    assert_eq!(h.app.ui.dialog, None, "decided on the kind, not the words");
    // another model missing is not SAM 3's
    let face = CommandError { message: "x".into(), kind: ErrorKind::ModelMissing(AiModel::FaceDetector) };
    h.app.ai_error(&ctx, face, None);
    assert_eq!(h.app.ui.dialog, None);
}

/// `run_typed` keeps the engine's kind for the code that decides, and `run` the same message.
#[test]
fn run_typed_keeps_the_kind_and_run_the_message() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("lc-ui-error-kinds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    h.app.session.face_models_dir = Some(dir.join("models"));
    let typed = h.app.run_typed("faces.detect", json!({})).unwrap_err();
    assert_eq!(typed.kind, ErrorKind::ModelMissing(AiModel::FaceDetector), "{typed}");
    h.app.ui.dialog = None;
    let text = h.app.run("faces.detect", json!({})).unwrap_err();
    assert_eq!(text, typed.message);
    let unknown = h.app.run_typed("no.such.command", json!({})).unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::UnknownCommand, "{unknown}");
    // an error the UI raised itself is `Other`
    assert_eq!(CommandError::from("Tick the box to accept the model's terms first".to_string()).kind, ErrorKind::Other);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SAM 3 download that ended while its dialog was closed: a cancel is told by the status's
/// `cancelled`, not by the error's words.
#[test]
fn a_cancelled_sam_download_is_told_by_its_flag() {
    let ended = |error: &str, cancelled: bool| {
        crate::sam_download_ended(&DownloadStatus { error: Some(error.into()), cancelled, ..DownloadStatus::default() }).unwrap()
    };
    let (text, error) = ended("Download abgebrochen", true);
    assert!(!error && text.starts_with("SAM 3 download stopped"), "{text}");
    let (text, error) = ended("could not download model.safetensors: the mirror cancelled the transfer", false);
    assert!(error && text.starts_with("The SAM 3 download failed: "), "{text}");
    assert_eq!(crate::sam_download_ended(&DownloadStatus::default()), None);
}
