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
        speed: None,
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
        speed: Some(4.6),
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
        speed: Some(1.0),
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

/// Where the user's "Download" button fetches a model from. The URL is pinned to a commit of the model's own
/// repository, and what arrives is checked against the manifest's size and SHA-256 before anything uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub id: String,
    pub url: String,
    /// The name the file is kept under while it waits for the user to accept its terms.
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Pinned download addresses by model id. Only models whose terms allow us to point at them are here: a model
/// that is marked non-commercial is never offered for download (the user brings the file themselves).
const SOURCES: &[(&str, &str, &str)] = &[
    (
        "sface-2021dec",
        "https://github.com/opencv/opencv_zoo/raw/25f423d0e04c31a17254620e58febd7386da523b/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
        "face_recognition_sface_2021dec.onnx",
    ),
    ("auraface-v1", "https://huggingface.co/fal/AuraFace-v1/resolve/af6d057c9b0ec4071d4c49c80e3539258798b609/glintr100.onnx", "glintr100.onnx"),
];

/// How to download the model with this id, if LightCraft offers to.
pub fn download(id: &str) -> Option<Download> {
    let (_, url, file_name) = SOURCES.iter().find(|(i, _, _)| *i == id)?;
    let m = all().into_iter().find(|m| m.id == id)?;
    if m.licence.commercial == Commercial::No || BUNDLED.contains(&id) {
        return None;
    }
    Some(Download { id: m.id, url: (*url).into(), file_name: (*file_name).into(), size_bytes: m.size_bytes?, sha256: m.sha256? })
}

/// "github.com" for a download address: what the user is told the file comes from.
pub fn host(url: &str) -> &str {
    url.strip_prefix("https://").and_then(|r| r.split('/').next()).unwrap_or("")
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
    fn downloads_are_pinned_checked_and_only_for_models_we_may_point_at() {
        for m in all() {
            let Some(d) = download(&m.id) else { continue };
            assert!(d.url.starts_with("https://"), "{}: https only", d.id);
            assert!(!d.url.contains("/main/") && !d.url.contains("/master/"), "{}: pinned to a commit, not a branch", d.id);
            assert!(d.file_name.ends_with(".onnx") && !d.file_name.contains(['/', '\\']), "{}", d.file_name);
            assert_eq!((Some(d.size_bytes), Some(d.sha256.as_str())), (m.size_bytes, m.sha256.as_deref()), "{}", d.id);
            assert!(m.licence.commercial != Commercial::No, "{} is non-commercial and must not be offered", d.id);
        }
        assert_eq!(download("sface-2021dec").map(|d| host(&d.url).to_string()).as_deref(), Some("github.com"));
        assert_eq!(download("auraface-v1").map(|d| host(&d.url).to_string()).as_deref(), Some("huggingface.co"));
        // the bundled detector, unknown ids and path tricks have nothing to download
        for id in ["yunet-2023mar", "nope", "", "../sface-2021dec"] {
            assert!(download(id).is_none(), "{id}");
        }
        // every address in the table belongs to a known model
        for (id, _, _) in SOURCES {
            assert!(all().iter().any(|m| m.id == *id), "{id}");
        }
    }

    #[test]
    fn unresolved_models_say_so() {
        assert_eq!(sface().licence.commercial, Commercial::Unknown);
        assert!(sface().provenance.to_lowercase().contains("undocumented"));
        assert!(auraface().provenance.to_lowercase().contains("undisclosed"));
    }
}
