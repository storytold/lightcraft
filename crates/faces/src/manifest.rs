//! A face model's manifest: what the model is, what it needs and who may use it, as plain data.
//!
//! A manifest comes from the known-model table, from a suggestion made for a file the user dropped in, or
//! from a `face-model.json` next to a model. All of those are untrusted: [`validate`] must pass before a
//! manifest is used, and [`parse`] applies it after reading JSON of bounded size.

use serde::{Deserialize, Serialize};

/// Largest `face-model.json` we read.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest model file we accept (bytes).
pub const MAX_MODEL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Finds faces (boxes, and landmarks when the model has them).
    Detector,
    /// Turns an aligned face into a vector whose distance says whether two faces are the same person.
    Embedder,
}

/// Whether the weights may be used commercially.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Commercial {
    Yes,
    /// Research or personal use only. Never bundled or redistributed by LightCraft.
    No,
    /// The licence or the training data is unclear: the user is told so before installing.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Licence {
    /// SPDX id or a short name ("MIT", "Apache-2.0", "InsightFace non-commercial research").
    pub name: String,
    pub commercial: Commercial,
    pub url: Option<String>,
    /// Plain-language terms shown before the model is enabled.
    pub notice: String,
}

impl Default for Licence {
    fn default() -> Self {
        Self { name: String::new(), commercial: Commercial::Unknown, url: None, notice: String::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Colour {
    Rgb,
    Bgr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resize {
    /// Stretch the crop to the input size (aligned face crops).
    Stretch,
    /// Keep the aspect ratio and pad (whole photos for a detector).
    Letterbox,
}

/// How an image becomes the model's input: NCHW float, `(pixel − mean) / std` per channel, pixel in 0..255.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InputSpec {
    pub width: u32,
    pub height: u32,
    pub colour: Colour,
    pub mean: [f32; 3],
    pub std: [f32; 3],
    pub resize: Resize,
}

impl Default for InputSpec {
    /// ArcFace-style: 112 × 112, RGB, `(x − 127.5) / 127.5`.
    fn default() -> Self {
        Self { width: 112, height: 112, colour: Colour::Rgb, mean: [127.5; 3], std: [127.5; 3], resize: Resize::Stretch }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OutputSpec {
    /// One vector of `dim` numbers per face.
    Embedding { dim: u32 },
    /// A detector whose outputs a built-in decoder understands (`"yunet-v2"`).
    Detector { decoder: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Thresholds {
    /// Embedders: cosine similarity at or above which two faces are suggested as a match.
    pub match_cosine: Option<f32>,
    /// Detectors: lowest detection score kept.
    pub score: Option<f32>,
    /// Detectors: overlap above which two boxes are one face.
    pub nms_iou: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelManifest {
    /// Lowercase letters, digits, `.` `_` `-`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub role: Role,
    #[serde(default)]
    pub licence: Licence,
    /// Where the weights come from (an `https://` page or file), for the user's information.
    #[serde(default)]
    pub source: Option<String>,
    /// Lowercase hex SHA-256 of the `.onnx` file, when known.
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// What the model was trained on, in plain words; says "undisclosed" when it is.
    #[serde(default)]
    pub provenance: String,
    #[serde(default)]
    pub input: InputSpec,
    pub output: OutputSpec,
    #[serde(default)]
    pub thresholds: Thresholds,
    /// How fast it is as a multiple of a ResNet-100 model's speed (`1.0`): `2.0` is twice as fast. Shown instead of
    /// timings, which depend on the computer; leave it out when you have not measured it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

/// The reference model the `speed` ratios are measured against.
pub const SPEED_REFERENCE: &str = "a ResNet-100 model";

/// A speed ratio in plain words, as times faster than the reference ("4.6× faster than a ResNet-100 model").
pub fn describe_speed(speed: f32) -> String {
    if !(speed.is_finite() && speed > 0.0) {
        return String::new();
    }
    let times = |x: f32| {
        let s = format!("{x:.1}");
        s.strip_suffix(".0").map_or(s.clone(), str::to_string)
    };
    if speed >= 1.15 {
        format!("{}× faster than {SPEED_REFERENCE}", times(speed))
    } else if speed > 0.87 {
        format!("Same speed as {SPEED_REFERENCE}")
    } else {
        format!("{}× slower than {SPEED_REFERENCE}", times(1.0 / speed))
    }
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ManifestError {
    #[error("`{0}` is not valid: {1}")]
    Field(&'static str, String),
    #[error("not a face-model manifest: {0}")]
    Parse(String),
    #[error("a manifest may be at most {MAX_MANIFEST_BYTES} bytes")]
    TooLarge,
}

fn bad<T>(field: &'static str, why: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError::Field(field, why.into()))
}

/// A model id is a folder name: 1 to 64 characters, starting with a lowercase letter or digit, then
/// lowercase letters, digits, `.`, `_` or `-`. So it is never `.`, `..`, hidden, or a path.
pub fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    first_ok && id.len() <= 64 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

fn text(field: &'static str, s: &str, min: usize, max: usize) -> Result<(), ManifestError> {
    let n = s.chars().count();
    if n < min || n > max {
        return bad(field, format!("must be {min} to {max} characters"));
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        return bad(field, "must not contain control characters");
    }
    Ok(())
}

fn url(field: &'static str, u: &Option<String>) -> Result<(), ManifestError> {
    match u {
        None => Ok(()),
        Some(u) if u.len() <= 500 && u.starts_with("https://") && !u.chars().any(|c| c.is_control() || c.is_whitespace()) => Ok(()),
        Some(_) => bad(field, "must be an https:// address of at most 500 characters"),
    }
}

fn ratio(field: &'static str, v: Option<f32>, lo: f32, hi: f32) -> Result<(), ManifestError> {
    match v {
        Some(v) if !(v.is_finite() && (lo..=hi).contains(&v)) => bad(field, format!("must be a number from {lo} to {hi}")),
        _ => Ok(()),
    }
}

/// Check a manifest from any source. Everything the rest of LightCraft relies on (sizes it will allocate,
/// numbers it will divide by, text it will show) is bounded here.
pub fn validate(m: &ModelManifest) -> Result<(), ManifestError> {
    if !valid_id(&m.id) {
        return bad("id", "must be 1 to 64 characters: lowercase letters and digits, then also '.', '_' or '-'");
    }
    text("name", &m.name, 1, 120)?;
    text("version", &m.version, 1, 40)?;
    text("licence.name", &m.licence.name, 0, 80)?;
    text("licence.notice", &m.licence.notice, 0, 800)?;
    text("provenance", &m.provenance, 0, 800)?;
    url("licence.url", &m.licence.url)?;
    url("source", &m.source)?;
    if let Some(h) = &m.sha256
        && !(h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    {
        return bad("sha256", "must be 64 lowercase hexadecimal digits");
    }
    if m.size_bytes.is_some_and(|s| s == 0 || s > MAX_MODEL_BYTES) {
        return bad("sizeBytes", format!("must be 1 to {MAX_MODEL_BYTES}"));
    }
    let i = &m.input;
    if !(16..=4096).contains(&i.width) || !(16..=4096).contains(&i.height) {
        return bad("input.width/height", "must be 16 to 4096");
    }
    if i.mean.iter().any(|v| !v.is_finite() || v.abs() > 1000.0) {
        return bad("input.mean", "must be finite numbers within ±1000");
    }
    if i.std.iter().any(|v| !v.is_finite() || v.abs() < 1e-3 || v.abs() > 1000.0) {
        return bad("input.std", "must be finite, not zero, within ±1000");
    }
    match &m.output {
        OutputSpec::Embedding { dim } => {
            if m.role != Role::Embedder {
                return bad("output", "an embedding output needs the embedder role");
            }
            if !(1..=4096).contains(dim) {
                return bad("output.dim", "must be 1 to 4096");
            }
        }
        OutputSpec::Detector { decoder } => {
            if m.role != Role::Detector {
                return bad("output", "a detector output needs the detector role");
            }
            if !crate::known::DECODERS.contains(&decoder.as_str()) {
                return bad("output.decoder", format!("unknown decoder `{}`", decoder.chars().take(40).collect::<String>()));
            }
        }
    }
    ratio("thresholds.matchCosine", m.thresholds.match_cosine, -1.0, 1.0)?;
    ratio("thresholds.score", m.thresholds.score, 0.0, 1.0)?;
    ratio("thresholds.nmsIou", m.thresholds.nms_iou, 0.0, 1.0)?;
    ratio("speed", m.speed, 0.01, 1000.0)?;
    Ok(())
}

/// Read a `face-model.json` (at most [`MAX_MANIFEST_BYTES`]) and validate it.
pub fn parse(bytes: &[u8]) -> Result<ModelManifest, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let m: ModelManifest = serde_json::from_slice(bytes).map_err(|e| ManifestError::Parse(e.to_string()))?;
    validate(&m)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> ModelManifest {
        ModelManifest {
            id: "example-r50".into(),
            name: "Example R50".into(),
            version: "1".into(),
            role: Role::Embedder,
            licence: Licence {
                name: "MIT".into(),
                commercial: Commercial::Yes,
                url: Some("https://example.org/licence".into()),
                notice: String::new(),
            },
            source: Some("https://example.org/model.onnx".into()),
            sha256: Some("a".repeat(64)),
            size_bytes: Some(1234),
            provenance: "synthetic".into(),
            input: InputSpec::default(),
            output: OutputSpec::Embedding { dim: 512 },
            thresholds: Thresholds { match_cosine: Some(0.4), ..Default::default() },
            speed: Some(1.7),
        }
    }

    #[test]
    fn speed_reads_as_times_faster_than_the_reference() {
        assert_eq!(describe_speed(7.0), "7× faster than a ResNet-100 model");
        assert_eq!(describe_speed(4.6), "4.6× faster than a ResNet-100 model");
        assert_eq!(describe_speed(1.7), "1.7× faster than a ResNet-100 model");
        assert_eq!(describe_speed(1.0), "Same speed as a ResNet-100 model");
        assert_eq!(describe_speed(0.5), "2× slower than a ResNet-100 model");
        for odd in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(describe_speed(odd), "", "{odd}");
        }
    }

    #[test]
    fn a_good_manifest_passes_and_round_trips() {
        let m = good();
        assert_eq!(validate(&m), Ok(()));
        let json = serde_json::to_vec(&m).unwrap();
        assert_eq!(parse(&json).unwrap(), m);
    }

    #[test]
    fn every_field_is_bounded() {
        let cases: Vec<(&str, Box<dyn Fn(&mut ModelManifest)>)> = vec![
            ("empty id", Box::new(|m| m.id = String::new())),
            ("uppercase id", Box::new(|m| m.id = "Bad".into())),
            ("path in id", Box::new(|m| m.id = "../etc".into())),
            ("dot id", Box::new(|m| m.id = ".".into())),
            ("dotdot id", Box::new(|m| m.id = "..".into())),
            ("hidden id", Box::new(|m| m.id = ".hidden".into())),
            ("slash id", Box::new(|m| m.id = "a/b".into())),
            ("backslash id", Box::new(|m| m.id = r"a\b".into())),
            ("long id", Box::new(|m| m.id = "a".repeat(65))),
            ("empty name", Box::new(|m| m.name = String::new())),
            ("control in name", Box::new(|m| m.name = "a\u{7}b".into())),
            ("huge notice", Box::new(|m| m.licence.notice = "x".repeat(801))),
            ("http source", Box::new(|m| m.source = Some("http://example.org/m.onnx".into()))),
            ("file source", Box::new(|m| m.source = Some("file:///etc/passwd".into()))),
            ("space in url", Box::new(|m| m.source = Some("https://a b".into()))),
            ("short sha", Box::new(|m| m.sha256 = Some("abc".into()))),
            ("uppercase sha", Box::new(|m| m.sha256 = Some("A".repeat(64)))),
            ("zero size", Box::new(|m| m.size_bytes = Some(0))),
            ("huge size", Box::new(|m| m.size_bytes = Some(u64::MAX))),
            ("tiny input", Box::new(|m| m.input.width = 4)),
            ("huge input", Box::new(|m| m.input.height = 1 << 20)),
            ("nan mean", Box::new(|m| m.input.mean[1] = f32::NAN)),
            ("inf mean", Box::new(|m| m.input.mean[0] = f32::INFINITY)),
            ("zero std", Box::new(|m| m.input.std[2] = 0.0)),
            ("nan std", Box::new(|m| m.input.std[0] = f32::NAN)),
            ("zero dim", Box::new(|m| m.output = OutputSpec::Embedding { dim: 0 })),
            ("huge dim", Box::new(|m| m.output = OutputSpec::Embedding { dim: 10_000_000 })),
            ("role mismatch", Box::new(|m| m.role = Role::Detector)),
            (
                "unknown decoder",
                Box::new(|m| {
                    m.role = Role::Detector;
                    m.output = OutputSpec::Detector { decoder: "made-up".into() };
                }),
            ),
            ("nan threshold", Box::new(|m| m.thresholds.match_cosine = Some(f32::NAN))),
            ("threshold range", Box::new(|m| m.thresholds.score = Some(1.5))),
            ("nan speed", Box::new(|m| m.speed = Some(f32::NAN))),
            ("zero speed", Box::new(|m| m.speed = Some(0.0))),
            ("huge speed", Box::new(|m| m.speed = Some(1e9))),
        ];
        for (what, f) in cases {
            let mut m = good();
            f(&mut m);
            assert!(validate(&m).is_err(), "{what} must be rejected");
        }
    }

    #[test]
    fn hostile_json_is_an_error() {
        assert!(matches!(parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]), Err(ManifestError::TooLarge)));
        for bytes in [
            &b""[..],
            b"null",
            b"[]",
            b"{",
            b"{\"id\": 1}",
            b"\xff\xfe\x00",
            b"{\"id\":\"x\",\"name\":\"n\",\"version\":\"1\",\"role\":\"embedder\"}",
        ] {
            assert!(parse(bytes).is_err(), "{:?}", String::from_utf8_lossy(bytes));
        }
    }
}
