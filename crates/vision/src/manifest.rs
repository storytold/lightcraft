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
    /// Trained with a sigmoid loss (SigLIP): `sigmoid(exp(logit_scale)·cos + logit_bias)` with the weights' scale and
    /// bias, a probability.
    Sigmoid,
    /// Trained with a softmax over a batch (CLIP): the weights have a scale (`exp(logit_scale)·cos` ranks) but no bias,
    /// so the scaled cosine says nothing on its own. The score is `sigmoid(exp(logit_scale)·cos + offset)` with the
    /// manifest's calibrated [`ScoreSpec::offset`], so every cut-off (search, auto-tags, the vocabulary's own,
    /// Smart Collections) reads the same way for both kinds.
    Softmax,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreSpec {
    pub kind: ScoreKind,
    /// A softmax model's offset (see [`ScoreKind::Softmax`]); [`DEFAULT_SOFTMAX_OFFSET`] when not given. Not used by
    /// sigmoid models (they have their own bias).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f32>,
}

impl ScoreSpec {
    pub const SIGMOID: ScoreSpec = ScoreSpec { kind: ScoreKind::Sigmoid, offset: None };

    /// The offset a softmax model's scores get.
    pub fn softmax_offset(&self) -> f32 {
        self.offset.unwrap_or(DEFAULT_SOFTMAX_OFFSET)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speed {
    /// Measured by the self-test on this computer.
    #[serde(default)]
    pub cpu_ms_per_image: Option<f32>,
}

/// Where a model stands among the others, for the model list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    /// Small and quick, finds less.
    Fastest,
    /// The default balance.
    Fast,
    /// More pixels per photo: finer detail, more work.
    Detailed,
    /// The largest towers: the best search results, the slowest.
    Best,
}

/// How a model compares with the others LightCraft knows (set for the built-in models, measured on a desktop
/// processor; see `docs/content-search.md`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    pub quality: Quality,
    /// Time per photo while indexing, relative to the default model's.
    pub relative_time: f32,
    /// Searches and tags work in English only (the text tower was trained on English).
    #[serde(default)]
    pub english_only: bool,
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
    /// Default cut-off of a search on [`crate::ContentModel::score`]: photos scoring at least this match the query
    /// (the user widens or narrows it from there). Manifests from before it existed get [`DEFAULT_MATCH_THRESHOLD`].
    #[serde(default = "default_match_threshold")]
    pub match_threshold: f32,
    #[serde(default)]
    pub speed: Speed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Hint>,
}

/// The search cut-off of SigLIP 2 base (256 px), measured on real libraries: a dog on a jetty scores 0.05–0.08 for
/// "a dog", a rocky coast 0.03 for "the sea", while unrelated photos stay below 0.005.
pub const DEFAULT_MATCH_THRESHOLD: f32 = 0.02;

/// The auto-tag cut-off of SigLIP 2 base (256 px) with the built-in vocabulary (one prompt, "this is a photo of
/// {}."), chosen on 343 labelled photos (319 real, 24 procedural): precision 0.74, recall 0.69 (see
/// `docs/content-search.md`). Specific tags (bird species, dog breeds, roles) carry their own, higher cut-offs in the
/// vocabulary.
pub const DEFAULT_TAG_THRESHOLD: f32 = 0.01;

/// The softmax offset of a CLIP model whose manifest gives none: CLIP ViT-B/32's (see [`CLIP_B32_OFFSET`]).
pub const DEFAULT_SOFTMAX_OFFSET: f32 = CLIP_B32_OFFSET;

/// CLIP ViT-B/32's calibration, on the same 343 labelled photos as SigLIP 2 base's (see `docs/content-search.md`).
/// The offset puts its balanced tag cut-off (precision 0.56, recall 0.52 with the built-in vocabulary) at the same
/// score as SigLIP 2 base's, 0.01, so the vocabulary's own cut-offs (`@0.1` for species and breeds) are as much
/// stricter for both: `100 · cos − 31` is −4.6 (a score of 0.01) at a cosine of 0.264.
pub const CLIP_B32_OFFSET: f32 = -31.0;
pub const CLIP_B32_TAG_THRESHOLD: f32 = 0.01;
/// Searches typed as they come ("a dog", "the sea") score lower than the tags' sentences: 0.005 gives precision 0.90,
/// recall 0.38 over 30 searches of the labelled photos (SigLIP 2 base at 0.02: 0.94 / 0.44).
pub const CLIP_B32_MATCH_THRESHOLD: f32 = 0.005;
/// Indexing time per photo relative to SigLIP 2 base (256 px), measured end to end on the same library and CPU.
/// (33–101 ms per photo against base's 141–167 on the same libraries and CPU, about half the time in all).
pub const CLIP_B32_RELATIVE_TIME: f32 = 0.5;

/// SigLIP 2 So400m's calibration, on the same labelled photos and searches as the base model's. It finds far more in
/// searches (0.003: precision 0.94, recall 0.70; base at 0.02: 0.94 / 0.44) but tags no better with the built-in
/// vocabulary (0.007: precision 0.68, recall 0.67; base at 0.01: 0.74 / 0.69): its scores run lower.
pub const SO400M_TAG_THRESHOLD: f32 = 0.007;
pub const SO400M_MATCH_THRESHOLD: f32 = 0.003;
/// End to end at Normal pace, 1,175–1,179 ms per photo against base's 141–167 on the same libraries and CPU.
pub const SO400M_RELATIVE_TIME: f32 = 7.5;
/// SigLIP 2 base at 384 px: 576 patches instead of 256, about 2.4× the work (estimated from the patch count, not
/// measured; its cut-offs are the 256 px model's, not calibrated).
pub const B16_384_RELATIVE_TIME: f32 = 2.4;

fn default_match_threshold() -> f32 {
    DEFAULT_MATCH_THRESHOLD
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
        score: ScoreSpec::SIGMOID,
        tag_threshold: DEFAULT_TAG_THRESHOLD,
        match_threshold: DEFAULT_MATCH_THRESHOLD,
        speed: Speed::default(),
        hint: Some(Hint { quality: Quality::Fast, relative_time: 1.0, english_only: false }),
    }
}

/// SigLIP 2 base, patch 16, 384 px: the same towers as [`builtin_siglip2_b16_256`] at a larger
/// input (576 patches, about 2.4× the work per photo), for finer detail.
pub fn builtin_siglip2_b16_384() -> Manifest {
    let mut m = builtin_siglip2_b16_256();
    let f = |name: &str, size: u64, sha256: &str| FileSpec { name: name.into(), size, sha256: sha256.into() };
    m.id = "siglip2-b16-384".into();
    m.name = "SigLIP 2 Base (384 px)".into();
    m.source = "https://huggingface.co/google/siglip2-base-patch16-384".into();
    m.files = vec![
        f("model.safetensors", 1_501_968_264, "ed72c0ace85020ae610fc817c2538b9cae5a477b012a50859c60af5b3ad30857"),
        f("tokenizer.json", 34_363_039, "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322"),
        f("config.json", 276, "b1b2481aa448d0cd29001bcf5244e72fa8f916dd8348bd18abf9e5533105b96c"),
        f("preprocessor_config.json", 394, "fb2817d3523ca3b666c859f15320c7138416bc38ffc515e2963f78c868c51c90"),
    ];
    m.image.size = 384;
    m.hint = Some(Hint { quality: Quality::Detailed, relative_time: B16_384_RELATIVE_TIME, english_only: false });
    m
}

/// SigLIP 2 So400m, patch 16, 384 px: the large towers (27 layers, width 1152, 1152-dim
/// embeddings), 4.5 GB, several times slower than the base model.
pub fn builtin_siglip2_so400m_16_384() -> Manifest {
    let mut m = builtin_siglip2_b16_384();
    let f = |name: &str, size: u64, sha256: &str| FileSpec { name: name.into(), size, sha256: sha256.into() };
    m.id = "siglip2-so400m-16-384".into();
    m.name = "SigLIP 2 So400m (384 px)".into();
    m.source = "https://huggingface.co/google/siglip2-so400m-patch16-384".into();
    m.files = vec![
        f("model.safetensors", 4_544_267_488, "422750e06216f053d85d4176fb6a9d4c5e71ded2d1b7c7e0e6c2efd909a1e427"),
        f("tokenizer.json", 34_363_039, "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322"),
        f("config.json", 537, "5f09151a317b37430e2c12358b94a4b698e9e72ee332986f04ac7df7e29de931"),
        f("preprocessor_config.json", 394, "fb2817d3523ca3b666c859f15320c7138416bc38ffc515e2963f78c868c51c90"),
    ];
    m.embedding.dim = 1152;
    m.tag_threshold = SO400M_TAG_THRESHOLD;
    m.match_threshold = SO400M_MATCH_THRESHOLD;
    m.hint = Some(Hint { quality: Quality::Best, relative_time: SO400M_RELATIVE_TIME, english_only: false });
    m
}

/// CLIP ViT-B/32 (OpenAI, MIT): the small, fast, English-only model (224 px, 512-dim embeddings, 605 MB), a second
/// family with its own tokenizer and scoring. The weights are the `safetensors` conversion of OpenAI's checkpoint in
/// the `openai/clip-vit-base-patch32` repository (Hugging Face's conversion bot, the same tensors as
/// `pytorch_model.bin`).
pub fn builtin_clip_vit_b32_openai() -> Manifest {
    let f = |name: &str, size: u64, sha256: &str| FileSpec { name: name.into(), size, sha256: sha256.into() };
    Manifest {
        id: "clip-vit-b32-openai".into(),
        name: "CLIP ViT-B/32 (OpenAI)".into(),
        family: Family::Clip,
        version: "2021-01".into(),
        licence: Licence { name: "MIT".into(), commercial: true, url: Some("https://github.com/openai/CLIP/blob/main/LICENSE".into()) },
        source: "https://huggingface.co/openai/clip-vit-base-patch32".into(),
        files: vec![
            f("model.safetensors", 605_157_884, "99d28a652e6ec46629ab7047a0ac82c69b1fe11e0ce672c43af65d3a9a3fc05d"),
            f("vocab.json", 862_328, "5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363"),
            f("merges.txt", 524_657, "f526393189112391ce6f9795d4695f704121ce452c3aad1f5335cc41337eba85"),
            f("config.json", 4_186, "b575ef3c36f2a057fa19e221650105052d61cc9c1a972ec15019c6261ec98770"),
            f("preprocessor_config.json", 316, "910e70b3956ac9879ebc90b22fb3bc8a75b6a0677814500101a4c072bd7857bd"),
        ],
        // preprocessor_config.json: shortest edge to 224 (resample 3 = PIL bicubic), centre crop 224, CLIP's mean/std
        image: ImageSpec {
            size: 224,
            resize: Filter::Bicubic,
            fit: Fit::CenterCrop,
            mean: [0.481_454_66, 0.457_827_5, 0.408_210_73],
            std: [0.268_629_54, 0.261_302_58, 0.275_777_1],
        },
        text: TextSpec { tokenizer: TokenizerKind::ClipBpe, context: 77, lowercase: true, template: "a photo of {}".into() },
        embedding: EmbeddingSpec { dim: 512 },
        score: ScoreSpec { kind: ScoreKind::Softmax, offset: Some(CLIP_B32_OFFSET) },
        tag_threshold: CLIP_B32_TAG_THRESHOLD,
        match_threshold: CLIP_B32_MATCH_THRESHOLD,
        speed: Speed::default(),
        hint: Some(Hint { quality: Quality::Fastest, relative_time: CLIP_B32_RELATIVE_TIME, english_only: true }),
    }
}

/// The id of the model offered first (and used when nothing else is chosen).
pub const DEFAULT_MODEL: &str = "siglip2-b16-256";

/// The models LightCraft knows (offered for download), the default first.
pub fn builtin() -> Vec<Manifest> {
    vec![builtin_siglip2_b16_256(), builtin_siglip2_b16_384(), builtin_siglip2_so400m_16_384(), builtin_clip_vit_b32_openai()]
}

/// Where a built-in model's files are downloaded from by default: its Hugging Face repository,
/// pinned to the revision whose files the manifest lists (file `f` is at `<base>/<f>`). The
/// files are checked against the manifest's sizes and SHA-256 whatever the source.
pub fn hugging_face(id: &str) -> Option<&'static str> {
    match id {
        "siglip2-b16-256" => Some("https://huggingface.co/google/siglip2-base-patch16-256/resolve/3f9f96cb90da5dbc758b01813f2f6f1aee24c1ab"),
        "siglip2-b16-384" => Some("https://huggingface.co/google/siglip2-base-patch16-384/resolve/f775b65a79762255128c981547af89addcfe0f88"),
        "siglip2-so400m-16-384" => Some("https://huggingface.co/google/siglip2-so400m-patch16-384/resolve/dd658faac399427308559e2c3ac1e99cbe43845d"),
        // the commit of the repository's safetensors conversion (refs/pr/66), on top of main's 3d74acf9
        "clip-vit-b32-openai" => Some("https://huggingface.co/openai/clip-vit-base-patch32/resolve/c237dc49a33fc61debc9276459120b7eac67e7ef"),
        _ => None,
    }
}

/// The total size of a model's files in bytes.
pub fn total_size(m: &Manifest) -> u64 {
    m.files.iter().fold(0u64, |a, f| a.saturating_add(f.size))
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
    if !(m.match_threshold.is_finite() && (0.0..=1.0).contains(&m.match_threshold)) {
        return bad("matchThreshold", "must be a number from 0 to 1");
    }
    if let Some(s) = m.speed.cpu_ms_per_image
        && !(s.is_finite() && (0.0..=1e6).contains(&s))
    {
        return bad("speed.cpuMsPerImage", "must be a number from 0 to 1000000");
    }
    if let Some(h) = m.hint
        && !(h.relative_time.is_finite() && (0.01..=100.0).contains(&h.relative_time))
    {
        return bad("hint.relativeTime", "must be a number from 0.01 to 100");
    }
    if let Some(o) = m.score.offset
        && !(o.is_finite() && (-200.0..=200.0).contains(&o))
    {
        return bad("score.offset", "must be a number from -200 to 200");
    }
    match (m.family, m.text.tokenizer) {
        (Family::Siglip, TokenizerKind::Sentencepiece) | (Family::Clip, TokenizerKind::ClipBpe) => {}
        _ => return bad("text.tokenizer", "does not match the model family"),
    }
    match (m.family, m.score.kind, m.score.offset) {
        (Family::Siglip, ScoreKind::Sigmoid, None) | (Family::Clip, ScoreKind::Softmax, _) => Ok(()),
        (Family::Siglip, ScoreKind::Sigmoid, Some(_)) => bad("score.offset", "is only for softmax (CLIP) models"),
        _ => bad("score.kind", "does not match the model family (SigLIP: sigmoid, CLIP: softmax)"),
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
        assert_eq!(builtin()[0].id, DEFAULT_MODEL);
        for m in builtin() {
            let base = hugging_face(&m.id).unwrap();
            assert!(base.starts_with("https://huggingface.co/") && m.source.starts_with("https://huggingface.co/"), "{}", m.id);
            assert!(base.starts_with(&format!("{}/resolve/", m.source)), "{base} is in {}", m.source);
            assert!(total_size(&m) > 600_000_000);
        }
        let clip = builtin_clip_vit_b32_openai();
        assert_eq!((clip.family, clip.text.tokenizer, clip.score.kind), (Family::Clip, TokenizerKind::ClipBpe, ScoreKind::Softmax));
        assert_eq!((clip.image.size, clip.text.context, clip.embedding.dim), (224, 77, 512));
        assert_eq!(clip.score.softmax_offset(), CLIP_B32_OFFSET);
        assert_eq!(ScoreSpec { kind: ScoreKind::Softmax, offset: None }.softmax_offset(), DEFAULT_SOFTMAX_OFFSET);
        assert_eq!(builtin_siglip2_so400m_16_384().embedding.dim, 1152);
        assert_eq!(hugging_face("nope"), None);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"tagThreshold\"") && json.contains("\"cpuMsPerImage\""), "{json}");
        // a manifest written before the search cut-off existed gets the default
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("matchThreshold");
        let old: Manifest = serde_json::from_value(v).unwrap();
        assert_eq!(old.match_threshold, DEFAULT_MATCH_THRESHOLD);
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
            ("matchThreshold", Box::new(|m| m.match_threshold = -0.5)),
            ("speed.cpuMsPerImage", Box::new(|m| m.speed.cpu_ms_per_image = Some(-1.0))),
            ("text.tokenizer", Box::new(|m| m.text.tokenizer = TokenizerKind::ClipBpe)),
            ("text.tokenizer", Box::new(|m| m.family = Family::Clip)),
            ("score.kind", Box::new(|m| m.score.kind = ScoreKind::Softmax)),
            ("score.offset", Box::new(|m| m.score.offset = Some(-3.0))),
            ("score.offset", Box::new(|m| m.score.offset = Some(f32::NAN))),
            ("hint.relativeTime", Box::new(|m| m.hint = Some(Hint { quality: Quality::Fast, relative_time: 0.0, english_only: false }))),
            ("hint.relativeTime", Box::new(|m| m.hint = Some(Hint { quality: Quality::Fast, relative_time: f32::NAN, english_only: false }))),
        ];
        for (field, edit) in cases {
            let mut m = builtin_siglip2_b16_256();
            edit(&mut m);
            match validate(&m) {
                Err(ManifestError::Field(f, _)) => assert_eq!(f, field),
                other => panic!("{field}: {other:?}"),
            }
        }
        // the CLIP family: its own tokenizer and scoring, a bounded offset
        let clip_cases: Vec<(&str, Box<dyn Fn(&mut Manifest)>)> = vec![
            ("text.tokenizer", Box::new(|m| m.text.tokenizer = TokenizerKind::Sentencepiece)),
            ("text.tokenizer", Box::new(|m| m.family = Family::Siglip)),
            ("score.kind", Box::new(|m| m.score.kind = ScoreKind::Sigmoid)),
            ("score.offset", Box::new(|m| m.score.offset = Some(1e9))),
            ("score.offset", Box::new(|m| m.score.offset = Some(f32::NEG_INFINITY))),
            ("text.context", Box::new(|m| m.text.context = 1000)),
        ];
        for (field, edit) in clip_cases {
            let mut m = builtin_clip_vit_b32_openai();
            edit(&mut m);
            match validate(&m) {
                Err(ManifestError::Field(f, _)) => assert_eq!(f, field),
                other => panic!("clip {field}: {other:?}"),
            }
        }
        assert_eq!(parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]), Err(ManifestError::TooLarge));
        assert!(matches!(parse(b"{\"id\": 3}"), Err(ManifestError::Parse(_))));
        assert!(matches!(parse(b"\xff\xfe"), Err(ManifestError::Parse(_))));
    }
}
