//! The models LightCraft recognises by their SHA-256, with what is honestly known about each.
//!
//! The licence and provenance texts here are what the user reads before enabling a model, so they say what
//! is *not* known too. Weights are never bundled except where the bundling decision says so (the YuNet
//! detector, 232 KB, MIT); everything else is an opt-in the user installs themselves.

use crate::manifest::{Colour, Commercial, InputSpec, Licence, ModelManifest, OutputSpec, Resize, Role, Thresholds};

/// Detector output decoders built into LightCraft.
pub const DECODERS: &[&str] = &["yunet-v2"];

/// Models that ship inside LightCraft (they are not installed by the user).
pub const BUNDLED: &[&str] = &["yunet-2023mar"];

pub const YUNET_SHA256: &str = "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const SFACE_SHA256: &str = "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79";
pub const AURAFACE_SHA256: &str = "a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60";

fn licence(name: &str, commercial: Commercial, url: &str, notice: &str) -> Licence {
    Licence { name: name.into(), commercial, url: Some(url.into()), notice: notice.into() }
}

/// YuNet 2023mar (OpenCV Zoo): the face detector LightCraft bundles.
pub fn yunet() -> ModelManifest {
    ModelManifest {
        id: "yunet-2023mar".into(),
        name: "YuNet (face detector)".into(),
        version: "2023mar".into(),
        role: Role::Detector,
        licence: licence(
            "MIT",
            Commercial::Yes,
            "https://github.com/opencv/opencv_zoo/blob/main/models/face_detection_yunet/LICENSE",
            "MIT licence, copyright Shiqi Yu. Bundled with LightCraft.",
        ),
        source: Some("https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet".into()),
        sha256: Some(YUNET_SHA256.into()),
        size_bytes: Some(232_589),
        provenance:
            "Trained on WIDER FACE, whose terms are not settled by the weights' MIT licence. A detector: it finds faces, it does not identify anyone."
                .into(),
        input: InputSpec { width: 640, height: 640, colour: Colour::Bgr, mean: [0.0; 3], std: [1.0; 3], resize: Resize::Letterbox },
        output: OutputSpec::Detector { decoder: "yunet-v2".into() },
        thresholds: Thresholds { match_cosine: None, score: Some(0.6), nms_iou: Some(0.3) },
    }
}

/// SFace 2021dec (OpenCV Zoo). Its match threshold is a starting point: on 755 named faces (busts, matched across
/// shots more than five seconds apart) 0.55 gave 97.5% right suggestions for 88% of faces; `faces.evaluate` checks it on
/// your own photos.
pub fn sface() -> ModelManifest {
    ModelManifest {
        id: "sface-2021dec".into(),
        name: "SFace (face recogniser)".into(),
        version: "2021dec".into(),
        role: Role::Embedder,
        licence: licence(
            "Apache-2.0 (as labelled by OpenCV Zoo)",
            Commercial::Unknown,
            "https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface",
            "Labelled Apache-2.0, but what it was trained on is not documented, and two questions about commercial use (opencv_zoo issues 313 and 318) are unanswered. Fine to try for yourself; do not redistribute.",
        ),
        source: Some("https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface".into()),
        sha256: Some(SFACE_SHA256.into()),
        size_bytes: Some(38_696_353),
        provenance: "Undocumented. The original SFace repository mentions CASIA-WebFace, VGGFace2 and MS1MV2.".into(),
        input: InputSpec { width: 112, height: 112, colour: Colour::Rgb, mean: [0.0; 3], std: [1.0; 3], resize: Resize::Stretch },
        output: OutputSpec::Embedding { dim: 128 },
        thresholds: Thresholds { match_cosine: Some(0.55), ..Thresholds::default() },
    }
}

/// AuraFace v1 `glintr100` (fal.ai). Starting-point threshold: on the same 755 faces 0.40 gave 96.6% right suggestions
/// for 56% of faces (0.30: 97.3% for 85%).
pub fn auraface() -> ModelManifest {
    ModelManifest {
        id: "auraface-v1".into(),
        name: "AuraFace v1 (face recogniser)".into(),
        version: "1".into(),
        role: Role::Embedder,
        licence: licence(
            "Apache-2.0",
            Commercial::Yes,
            "https://huggingface.co/fal/AuraFace-v1",
            "Apache-2.0. Its training data is described only as a commercial dataset, with no dataset named and no consent statement, and it covers some ethnicities less well. About 261 MB, and slow on a CPU.",
        ),
        source: Some("https://huggingface.co/fal/AuraFace-v1".into()),
        sha256: Some(AURAFACE_SHA256.into()),
        size_bytes: Some(260_694_151),
        provenance: "Undisclosed: \"a commercial dataset comprising face images from various sources\".".into(),
        input: InputSpec::default(),
        output: OutputSpec::Embedding { dim: 512 },
        thresholds: Thresholds { match_cosine: Some(0.40), ..Thresholds::default() },
    }
}

/// Every model we know by hash.
pub fn all() -> Vec<ModelManifest> {
    vec![yunet(), sface(), auraface()]
}

/// The known model whose file has this SHA-256 (lowercase hex).
pub fn lookup(sha256: &str) -> Option<ModelManifest> {
    all().into_iter().find(|m| m.sha256.as_deref() == Some(sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::validate;

    #[test]
    fn known_models_are_valid_and_unique() {
        let all = all();
        for m in &all {
            assert_eq!(validate(m), Ok(()), "{}", m.id);
        }
        let mut ids: Vec<_> = all.iter().map(|m| &m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "unique ids");
        let mut hashes: Vec<_> = all.iter().filter_map(|m| m.sha256.as_deref()).collect();
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), all.len(), "unique hashes");
    }

    #[test]
    fn lookup_finds_by_hash() {
        assert_eq!(lookup(AURAFACE_SHA256).map(|m| m.id), Some("auraface-v1".to_string()));
        assert_eq!(lookup(YUNET_SHA256).map(|m| m.role), Some(Role::Detector));
        assert!(lookup(&"0".repeat(64)).is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn unresolved_models_say_so() {
        assert_eq!(sface().licence.commercial, Commercial::Unknown);
        assert!(sface().provenance.to_lowercase().contains("undocumented"));
        assert!(auraface().provenance.to_lowercase().contains("undisclosed"));
    }
}
