//! A content model's manifest: what the model is, its files, who may use it and how it wants its
//! input, as plain data (`content-model.json` beside the weights, or the built-in table).
//!
//! Manifests from files are untrusted: [`parse`] reads bounded JSON and [`validate`] must pass
//! before a manifest is used. Everything the towers allocate from or divide by is bounded here.

use serde::{Deserialize, Serialize};

/// Largest `content-model.json` we read.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest model file we accept (bytes): the so400m checkpoint is about 4.5 GB.
pub const MAX_FILE_BYTES: u64 = 8 << 30;
/// Most files a model may list.
pub const MAX_FILES: usize = 16;

/// Which tower code runs the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Family {
    Siglip,
    Clip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Licence {
    /// SPDX id or a short name.
    pub name: String,
    /// Whether the weights may be used commercially.
    pub commercial: bool,
    #[serde(default)]
    pub url: Option<String>,
}

/// One file of the model, pinned by size and SHA-256 (what `lightcraft-fetch` checks).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSpec {
    /// A plain file name in the model folder.
    pub name: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// Resampling filter for the resize to the model's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Filter {
    Bilinear,
    Bicubic,
}

/// How a photo of any shape becomes the square input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// Stretch the whole photo to the square (SigLIP).
    Squash,
    /// Resize the short edge to the input, then crop the centre (CLIP).
    CenterCrop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    /// Side of the square input in pixels.
    pub size: u32,
    pub resize: Filter,
    pub fit: Fit,
    /// Per channel, on 0..1 pixel values: `(x − mean) / std`.
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenizerKind {
    /// SentencePiece-style BPE from `tokenizer.json` (Gemma, for SigLIP 2).
    Sentencepiece,
    /// CLIP's byte-level BPE (`vocab.json` + `merges.txt`).
    ClipBpe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSpec {
    pub tokenizer: TokenizerKind,
    /// Tokens per text, padding included.
    pub context: u32,
    /// Lower-case texts before tokenizing (SigLIP 2 was trained on lower-cased text).
    pub lowercase: bool,
    /// Prompt template for tag terms, with `{}` for the term.
    pub template: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSpec {
    pub dim: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScoreKind {
    /// `sigmoid(exp(logit_scale)·cos + logit_bias)` with the weights' scale and bias: a probability.
    Sigmoid,
    /// `exp(logit_scale)·cos`: ranks only.
    Softmax,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreSpec {
    pub kind: ScoreKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speed {
    /// Measured by the self-test on this computer.
    #[serde(default)]
    pub cpu_ms_per_image: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Lowercase letters, digits, `.` `_` `-` (a folder name).
    pub id: String,
    pub name: String,
    pub family: Family,
    pub version: String,
    pub licence: Licence,
    /// Where the weights come from (an `https://` page).
    pub source: String,
    pub files: Vec<FileSpec>,
    pub image: ImageSpec,
    pub text: TextSpec,
    pub embedding: EmbeddingSpec,
    pub score: ScoreSpec,
    /// Default per-tag cut-off on [`crate::ContentModel::score`].
    pub tag_threshold: f32,
    #[serde(default)]
    pub speed: Speed,
}

impl Manifest {
    /// The file called `name`, if listed.
    pub fn file(&self, name: &str) -> Option<&FileSpec> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// The default model: SigLIP 2 base, patch 16, 256 px (Google, Apache-2.0).
pub fn builtin_siglip2_b16_256() -> Manifest {
    let f = |name: &str, size: u64, sha256: &str| FileSpec { name: name.into(), size, sha256: sha256.into() };
    Manifest {
        id: "siglip2-b16-256".into(),
        name: "SigLIP 2 Base (256 px)".into(),
        family: Family::Siglip,
        version: "2025-02".into(),
        licence: Licence { name: "Apache-2.0".into(), commercial: true, url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()) },
        source: "https://huggingface.co/google/siglip2-base-patch16-256".into(),
        files: vec![
            f("model.safetensors", 1_500_985_224, "6125cacc01fa93bdc98a0c5101cefcd69b2ed1f8ab4f38d86f4ad5984f5dc863"),
            f("tokenizer.json", 34_363_039, "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322"),
            f("config.json", 276, "7b5aedcb8893e31376e129c1ffd7a5392f1a806dbc793ce53eda220c2ec59edf"),
            f("preprocessor_config.json", 394, "d14ba2ee3fd816f3de8abaddc31953565128eaf37c73ad4bed32101a98465aff"),
        ],
        // preprocessor_config.json: resample 2 (PIL bilinear), 256 × 256, mean = std = 0.5
        image: ImageSpec { size: 256, resize: Filter::Bilinear, fit: Fit::Squash, mean: [0.5; 3], std: [0.5; 3] },
        text: TextSpec { tokenizer: TokenizerKind::Sentencepiece, context: 64, lowercase: true, template: "a photo of {}".into() },
        embedding: EmbeddingSpec { dim: 768 },
        score: ScoreSpec { kind: ScoreKind::Sigmoid },
        tag_threshold: 0.10,
        speed: Speed::default(),
    }
}

/// The models LightCraft knows (offered for download).
pub fn builtin() -> Vec<Manifest> {
    vec![builtin_siglip2_b16_256()]
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ManifestError {
    #[error("`{0}` is not valid: {1}")]
    Field(&'static str, String),
    #[error("not a content-model manifest: {0}")]
    Parse(String),
    #[error("a manifest may be at most {MAX_MANIFEST_BYTES} bytes")]
    TooLarge,
}

fn bad<T>(field: &'static str, why: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError::Field(field, why.into()))
}

/// A model id is a folder name: 1 to 64 characters, starting with a lowercase letter or digit,
/// then lowercase letters, digits, `.`, `_` or `-`. So it is never `.`, `..`, hidden, or a path.
pub fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    first_ok && id.len() <= 64 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// A file name in the model folder: no path separators, not hidden, at most 128 bytes.
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn text(field: &'static str, s: &str, min: usize, max: usize) -> Result<(), ManifestError> {
    let n = s.chars().count();
    if n < min || n > max {
        return bad(field, format!("must be {min} to {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return bad(field, "must not contain control characters");
    }
    Ok(())
}

fn https(field: &'static str, u: &str) -> Result<(), ManifestError> {
    if u.len() <= 500 && u.starts_with("https://") && !u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        Ok(())
    } else {
        bad(field, "must be an https:// address of at most 500 characters")
    }
}

/// Check a manifest from any source.
pub fn validate(m: &Manifest) -> Result<(), ManifestError> {
    if !valid_id(&m.id) {
        return bad("id", "must be 1 to 64 characters: lowercase letters and digits, then also '.', '_' or '-'");
    }
    text("name", &m.name, 1, 120)?;
    text("version", &m.version, 1, 40)?;
    text("licence.name", &m.licence.name, 1, 80)?;
    if let Some(u) = &m.licence.url {
        https("licence.url", u)?;
    }
    https("source", &m.source)?;
    if m.files.is_empty() || m.files.len() > MAX_FILES {
        return bad("files", format!("must list 1 to {MAX_FILES} files"));
    }
    for (i, f) in m.files.iter().enumerate() {
        if !valid_file_name(&f.name) {
            return bad("files.name", "must be a plain file name (letters, digits, '.', '_', '-')");
        }
        if m.files.iter().take(i).any(|g| g.name == f.name) {
            return bad("files.name", format!("`{}` is listed twice", f.name));
        }
        if f.size == 0 || f.size > MAX_FILE_BYTES {
            return bad("files.size", format!("must be 1 to {MAX_FILE_BYTES}"));
        }
        if !(f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            return bad("files.sha256", "must be 64 lowercase hexadecimal digits");
        }
    }
    let i = &m.image;
    if !(16..=1024).contains(&i.size) {
        return bad("image.size", "must be 16 to 1024");
    }
    if i.mean.iter().any(|v| !v.is_finite() || v.abs() > 10.0) {
        return bad("image.mean", "must be finite numbers within ±10");
    }
    if i.std.iter().any(|v| !v.is_finite() || v.abs() < 1e-3 || v.abs() > 10.0) {
        return bad("image.std", "must be finite, not zero, within ±10");
    }
    if !(2..=256).contains(&m.text.context) {
        return bad("text.context", "must be 2 to 256");
    }
    text("text.template", &m.text.template, 2, 200)?;
    if m.text.template.matches("{}").count() != 1 {
        return bad("text.template", "must contain `{}` exactly once");
    }
    if !(1..=4096).contains(&m.embedding.dim) {
        return bad("embedding.dim", "must be 1 to 4096");
    }
    if !(m.tag_threshold.is_finite() && (0.0..=1.0).contains(&m.tag_threshold)) {
        return bad("tagThreshold", "must be a number from 0 to 1");
    }
    if let Some(s) = m.speed.cpu_ms_per_image
        && !(s.is_finite() && (0.0..=1e6).contains(&s))
    {
        return bad("speed.cpuMsPerImage", "must be a number from 0 to 1000000");
    }
    match (m.family, m.text.tokenizer) {
        (Family::Siglip, TokenizerKind::Sentencepiece) | (Family::Clip, TokenizerKind::ClipBpe) => Ok(()),
        _ => bad("text.tokenizer", "does not match the model family"),
    }
}

/// Read a `content-model.json` (at most [`MAX_MANIFEST_BYTES`]) and validate it.
pub fn parse(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| ManifestError::Parse(e.to_string()))?;
    validate(&m)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_manifests_are_valid_and_round_trip() {
        for m in builtin() {
            assert_eq!(validate(&m), Ok(()), "{}", m.id);
            let json = serde_json::to_vec_pretty(&m).unwrap();
            assert_eq!(parse(&json).unwrap(), m);
        }
        let m = builtin_siglip2_b16_256();
        assert_eq!(m.file("model.safetensors").unwrap().size, 1_500_985_224);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"tagThreshold\"") && json.contains("\"cpuMsPerImage\""), "{json}");
    }

    #[test]
    fn hostile_manifests_are_refused() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Manifest)>)> = vec![
            ("id", Box::new(|m| m.id = "../x".into())),
            ("id", Box::new(|m| m.id = String::new())),
            ("name", Box::new(|m| m.name = "a\u{7}".into())),
            ("source", Box::new(|m| m.source = "http://x".into())),
            ("files", Box::new(|m| m.files.clear())),
            ("files.name", Box::new(|m| m.files[0].name = "../model.safetensors".into())),
            ("files.name", Box::new(|m| m.files[1].name = m.files[0].name.clone())),
            ("files.size", Box::new(|m| m.files[0].size = 0)),
            ("files.size", Box::new(|m| m.files[0].size = u64::MAX)),
            ("files.sha256", Box::new(|m| m.files[0].sha256 = "ABC".into())),
            ("image.size", Box::new(|m| m.image.size = 1_000_000)),
            ("image.size", Box::new(|m| m.image.size = 0)),
            ("image.std", Box::new(|m| m.image.std = [0.0; 3])),
            ("image.mean", Box::new(|m| m.image.mean = [f32::NAN; 3])),
            ("text.context", Box::new(|m| m.text.context = 100_000)),
            ("text.template", Box::new(|m| m.text.template = "no slot".into())),
            ("embedding.dim", Box::new(|m| m.embedding.dim = 0)),
            ("embedding.dim", Box::new(|m| m.embedding.dim = u32::MAX)),
            ("tagThreshold", Box::new(|m| m.tag_threshold = f32::INFINITY)),
            ("speed.cpuMsPerImage", Box::new(|m| m.speed.cpu_ms_per_image = Some(-1.0))),
            ("text.tokenizer", Box::new(|m| m.text.tokenizer = TokenizerKind::ClipBpe)),
        ];
        for (field, edit) in cases {
            let mut m = builtin_siglip2_b16_256();
            edit(&mut m);
            match validate(&m) {
                Err(ManifestError::Field(f, _)) => assert_eq!(f, field),
                other => panic!("{field}: {other:?}"),
            }
        }
        assert_eq!(parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]), Err(ManifestError::TooLarge));
        assert!(matches!(parse(b"{\"id\": 3}"), Err(ManifestError::Parse(_))));
        assert!(matches!(parse(b"\xff\xfe"), Err(ManifestError::Parse(_))));
    }
}
