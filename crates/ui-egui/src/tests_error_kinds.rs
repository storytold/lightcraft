//! The UI decides on what kind of error a command returned, not on its words (#1046): messages get
//! reworded and translated, and a decision that reads them breaks silently when they do.

use lcc_engine::segment::{DownloadStatus, NOT_INSTALLED, Segmenter};
use lcc_engine::{AiModel, ErrorKind};
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{CommandError, LccApp, Services};

fn demo() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
    } else {
        assert_eq!(h.app.ui.dialog, None, "a build without AI masks has nothing to offer");
    }
    h.app.ui.dialog = None;
    h.app.ai_error(&ctx, format!("{NOT_INSTALLED}: but this is some other failure"), None);
    assert_eq!(h.app.ui.dialog, None, "decided on the kind, not the words");
    // another model missing is not SAM 3's: the face model is in the AI Models dialog
    let face = CommandError { message: "x".into(), kind: ErrorKind::ModelMissing(AiModel::FaceDetector) };
    h.app.ai_error(&ctx, face, None);
    assert!(!matches!(h.app.ui.dialog, Some(Dialog::SamModel { .. })), "{:?}", h.app.ui.dialog);
}

/// `run_typed` keeps the engine's kind for the code that decides, and `run` the same message.
#[test]
fn run_typed_keeps_the_kind_and_run_the_message() {
    let dir = lcc_testkit::TempDir::new("lc-ui-error-kinds");
    let mut h = demo();
    let _ = std::fs::remove_dir_all(&dir);
    h.app.session.face_models_dir = Some(dir.join("models"));
    // (the app keeps the AI models and the face models in the same folder)
    h.app.session.segmenter.models_root = Some(dir.join("models"));
    let typed = h.app.run_typed("faces.detect", json!({})).unwrap_err();
    if Segmenter::AVAILABLE {
        assert_eq!(typed.kind, ErrorKind::ModelMissing(AiModel::FaceDetector), "{typed}");
    } else {
        // a build without the open AI models has no face model to download
        assert_ne!(typed.kind, ErrorKind::ModelMissing(AiModel::FaceDetector), "{typed}");
    }
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

/// With SAM 2.1 chosen for Object masks, its missing model opens the AI Models dialog (which
/// draws headless), and `dialog.aiModels` opens it too.
#[test]
fn a_missing_sam2_opens_the_ai_models_dialog() {
    let mut h = demo();
    let ctx = h.view.ctx.clone();
    let missing = CommandError { message: "x".into(), kind: ErrorKind::ModelMissing(AiModel::Sam2) };
    h.app.ai_error(&ctx, missing, Some(("object", "new")));
    if !Segmenter::AVAILABLE {
        assert_eq!(h.app.ui.dialog, None);
        return;
    }
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
    h.app.ui.dialog = None;
    h.app.run("dialog.aiModels", json!({})).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "ai"));
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "ai"), "it stays open while shown");
}

/// In a build with AI masks, the AI Models dialog offers the open MediaPipe people and face
/// models (Retouch, Find Blemishes, Detect Faces) next to SAM 3, BiRefNet and SAM 2.1: each has
/// its Download button.
#[test]
fn the_ai_models_dialog_offers_the_mediapipe_models() {
    if !Segmenter::AVAILABLE {
        lcc_testkit::skip!(Feature, "a build without AI masks offers no models");
        return;
    }
    let mut h = demo();
    h.app.run("dialog.aiModels", json!({})).unwrap();
    h.step();
    h.step();
    let shown =
        |id: &str| h.app.widgets.iter().any(|(w, _)| w == &format!("button:aiModelDownload-{id}") || w == &format!("link:aiModelLicense-{id}"));
    for id in ["sam3", "subject", "sam2", "people", "face"] {
        assert!(shown(id), "the dialog lists {id}");
    }
}

/// The AI Models dialog lists every AI Denoise model the build can download (the own model with
/// `own-model`) with its Download button, which shows the model's terms first.
#[test]
fn the_ai_models_dialog_lists_the_downloadable_denoise_models() {
    let dir = lcc_testkit::TempDir::new("lc-ui-denoise-known");
    let known: Vec<String> = lcc_denoise::known::all().into_iter().filter(|k| k.download.is_some()).map(|k| k.manifest.id).collect();
    if known.is_empty() {
        lcc_testkit::skip!(Feature, "a build without own-model or rawnind-model offers no denoise download");
        return;
    }
    // tall enough for the whole dialog: the denoise models follow the AI mask and MediaPipe ones
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1800.0], 1.0);
    let _ = std::fs::remove_dir_all(&dir);
    h.app.session.set_denoise_models_dir(Some(dir.join("denoise-models")));
    h.app.run("dialog.aiModels", json!({})).unwrap();
    h.step();
    h.step();
    for id in &known {
        let w = format!("denoise:download:{id}");
        assert!(h.app.widgets.iter().any(|(n, _)| n == &w), "the dialog lists {id}");
        // the models are rows of Settings ▸ AI: scroll to the button as a person would
        crate::settings_screen::reveal(&mut h, &w);
        assert_eq!(h.request("ui.clickWidget", json!({"id": w}), std::time::Duration::from_secs(5))["ok"], true, "{id}");
        h.step();
        assert!(
            matches!(h.app.ui.dialog, Some(Dialog::DenoiseModel { accepted: false, .. })),
            "Download shows the terms first: {:?} {:?}",
            h.app.ui.dialog,
            h.app.widgets.iter().find(|(n, _)| n == &w)
        );
        h.app.run("dialog.aiModels", json!({})).unwrap();
        h.step();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
