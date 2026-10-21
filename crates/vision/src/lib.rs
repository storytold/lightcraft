//! Content models: image and text embeddings for searching photos by what they show.
//!
//! A [`ContentModel`] turns photos and phrases into vectors in one space; [`ContentModel::score`]
//! says how well a photo matches a phrase. The model behind it is described by a [`Manifest`]
//! (what it is, its files, its licence, how it wants its input), so models are interchangeable:
//! the default is Google's SigLIP 2 (base, patch 16, 256 px) on candle ([`siglip`]); OpenAI's CLIP
//! ViT-B/32 ([`clip`]) is a second family behind the same trait, with its own tokenizer and scoring.
//!
//! The weights are not part of LightCraft: they are read from a folder the user downloads them
//! to ([`load`]). Everything here runs locally. The towers run on candle (on macOS on the GPU through Metal,
//! elsewhere on the CPU), or on a runner another crate provides ([`load_with`], [`accel`]: LightCraft's wgpu GPU
//! runner), with candle as the fallback; the web build has no towers ([`load`] reports that it's unsupported).
//!
//! The SigLIP and CLIP towers are ported from the Hugging Face `transformers` implementation
//! (Apache-2.0); see `NOTICE`. This crate is therefore licensed under the Apache License 2.0 only (not the MIT
//! option of the rest of LightCraft). No UI dependencies (L1).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod accel;
#[cfg(not(target_arch = "wasm32"))]
pub mod clip;
pub mod config;
pub mod manifest;
#[cfg(not(target_arch = "wasm32"))]
mod nn;
pub mod preprocess;
#[cfg(not(target_arch = "wasm32"))]
pub mod siglip;
pub mod tokenizer;
pub mod vocab;
#[cfg(not(target_arch = "wasm32"))]
mod weights;

use std::path::{Path, PathBuf};

pub use manifest::Manifest;

/// An 8-bit sRGB image, row-major, the input of [`ContentModel::embed_images`].
pub type Rgb8 = lightcraft_raster::Image<[u8; 3]>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("content model: {0}")]
    Model(String),
    #[error("content model files not found in {0} (expected {1})")]
    Missing(PathBuf, String),
    #[error("content model: {0}")]
    Input(String),
    #[error("content models are not supported in this build")]
    Unsupported,
    #[error(transparent)]
    Manifest(#[from] manifest::ManifestError),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Candle(#[from] candle_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Most images or texts embedded in one call (bounds the activations held at once).
pub const MAX_BATCH: usize = 64;

/// A model that embeds images and texts into one space.
pub trait ContentModel: Send + Sync {
    /// One L2-normalised embedding per image, in order. At most [`MAX_BATCH`] images.
    fn embed_images(&self, batch: &[Rgb8]) -> Result<Vec<Vec<f32>>>;
    /// One L2-normalised embedding per text, in order. At most [`MAX_BATCH`] texts; the manifest
    /// says how they are prepared (lower-cased, truncated to the context).
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// How well an image embedding matches a text embedding, from 0 to 1: for a sigmoid model (SigLIP) the
    /// probability it was trained to give, for a softmax model (CLIP) its scaled cosine through a sigmoid with a
    /// calibrated offset ([`manifest::ScoreKind`]), so a cut-off means much the same for both.
    fn score(&self, image: &[f32], text: &[f32]) -> f32 {
        self.score_cosine(dot(image, text))
    }
    /// [`ContentModel::score`] from the cosine of the two (normalised) embeddings, so an index
    /// can compute the cosines itself (from f16 rows, in parallel) and score them after.
    fn score_cosine(&self, cosine: f32) -> f32;
    /// How [`ContentModel::score_cosine`] turns a cosine into a score, when it is an affine map (and a sigmoid), so
    /// scores can be computed without the model loaded (auto-tags keep it beside the tags' embeddings). `None`: only
    /// the model knows.
    fn scoring(&self) -> Option<Scoring> {
        None
    }
    fn manifest(&self) -> &Manifest;
    /// What runs the model: "CPU", or "GPU: <adapter>" (both towers loaded must run there to say so).
    fn runs_on(&self) -> String {
        "CPU".into()
    }
    /// Why the GPU stopped running this model (a failure: the CPU took over), if it did.
    fn gpu_fallback(&self) -> Option<String> {
        None
    }
}

/// A score as `scale · cosine + bias`, through a sigmoid for a sigmoid model ([`ContentModel::scoring`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scoring {
    pub scale: f32,
    pub bias: f32,
    pub sigmoid: bool,
}

impl Scoring {
    /// The cosine itself.
    pub const COSINE: Scoring = Scoring { scale: 1.0, bias: 0.0, sigmoid: false };

    pub fn score(&self, cosine: f32) -> f32 {
        let logit = self.scale * cosine + self.bias;
        if self.sigmoid { 1.0 / (1.0 + (-logit).exp()) } else { logit }
    }

    /// The smallest cosine that scores at least `score` (`-inf` when every cosine does, `+inf` when none can); the
    /// scale is positive for every model LightCraft knows.
    pub fn cosine_for(&self, score: f32) -> f32 {
        let logit = if self.sigmoid {
            if score <= 0.0 {
                return f32::NEG_INFINITY;
            }
            if score >= 1.0 {
                return f32::INFINITY;
            }
            (score / (1.0 - score)).ln()
        } else {
            score
        };
        if self.scale > 0.0 && self.scale.is_finite() { (logit - self.bias) / self.scale } else { f32::NEG_INFINITY }
    }
}

/// Which towers to load: indexing needs only the image tower, a search only the text tower.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Towers {
    pub image: bool,
    pub text: bool,
}

impl Towers {
    pub const BOTH: Self = Self { image: true, text: true };
}

/// Whether `dir` holds every file `manifest` lists (sizes not checked).
pub fn is_model_dir(dir: &Path, manifest: &Manifest) -> bool {
    manifest.files.iter().all(|f| dir.join(&f.name).is_file())
}

/// Load the model `manifest` describes from `dir`, on [`best_device`].
#[cfg(not(target_arch = "wasm32"))]
pub fn load(dir: &Path, manifest: &Manifest, towers: Towers) -> Result<Box<dyn ContentModel>> {
    load_with(dir, manifest, towers, None)
}

/// [`load`], running each tower on a runner `accel` makes when it can ([`accel::Accelerator`]: the GPU), with candle
/// on [`best_device`] as the fallback (when no runner can be made, or one fails).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_with(dir: &Path, manifest: &Manifest, towers: Towers, accel: Option<&accel::Accelerator>) -> Result<Box<dyn ContentModel>> {
    manifest::validate(manifest)?;
    match manifest.family {
        manifest::Family::Siglip => Ok(Box::new(siglip::Siglip::load_with(dir, manifest.clone(), towers, best_device(), accel)?)),
        manifest::Family::Clip => Ok(Box::new(clip::Clip::load_with(dir, manifest.clone(), towers, best_device(), accel)?)),
    }
}

/// How [`ContentModel::runs_on`] names a candle device.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn device_name(d: &candle_core::Device) -> String {
    if d.is_cpu() { "CPU".into() } else { "GPU (Metal)".into() }
}

/// The web build has no towers.
#[cfg(target_arch = "wasm32")]
pub fn load(_dir: &Path, _manifest: &Manifest, _towers: Towers) -> Result<Box<dyn ContentModel>> {
    Err(Error::Unsupported)
}

#[cfg(not(target_arch = "wasm32"))]
/// The best device here: Metal on macOS (when a GPU is available), else the CPU.
pub fn best_device() -> candle_core::Device {
    #[cfg(target_os = "macos")]
    if let Ok(d) = candle_core::Device::new_metal(0) {
        return d;
    }
    candle_core::Device::Cpu
}

/// The dot products of every row of `a` (`n` rows of `dim` numbers) with every row of `b` (`m` rows): `n × m` numbers,
/// row-major (`out[i * m + j] = a_i · b_j`). An empty answer when a size does not fit. Natively a matrix product on
/// candle's CPU kernels (80,000 photos × 1,500 tags × 768 numbers in about a second on a desktop processor).
pub fn similarities(a: &[f32], n: usize, b: &[f32], m: usize, dim: usize) -> Vec<f32> {
    if n == 0 || m == 0 || dim == 0 || a.len() != n.saturating_mul(dim) || b.len() != m.saturating_mul(dim) {
        return Vec::new();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let product = || -> candle_core::Result<Vec<f32>> {
            let dev = candle_core::Device::Cpu;
            let a = candle_core::Tensor::from_slice(a, (n, dim), &dev)?;
            let b = candle_core::Tensor::from_slice(b, (m, dim), &dev)?;
            a.matmul(&b.t()?)?.flatten_all()?.to_vec1::<f32>()
        };
        if let Ok(v) = product()
            && v.len() == n * m
        {
            return v;
        }
    }
    let mut out = Vec::with_capacity(n * m);
    for row in a.chunks_exact(dim) {
        out.extend(b.chunks_exact(dim).map(|col| dot(row, col)));
    }
    out
}

/// `v / |v|` (unchanged when `v` is zero or not finite).
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n.is_finite() && n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// Dot product of two equally long vectors (the cosine of normalised ones); 0 on a length mismatch.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_dot_are_safe_on_odd_input() {
        let mut z = vec![0.0f32; 4];
        normalize(&mut z);
        assert_eq!(z, vec![0.0; 4]);
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-6);
        assert_eq!(dot(&v, &[1.0]), 0.0);
    }

    #[test]
    fn scoring_and_its_inverse_agree() {
        let s = Scoring { scale: 112.9, bias: -16.77, sigmoid: true };
        for score in [1e-4f32, 0.01, 0.02, 0.5, 0.9] {
            let c = s.cosine_for(score);
            assert!((s.score(c) - score).abs() < 1e-3 * score.max(0.01), "{score}");
        }
        assert_eq!(s.cosine_for(0.0), f32::NEG_INFINITY);
        assert_eq!(s.cosine_for(1.0), f32::INFINITY);
        assert_eq!(Scoring::COSINE.score(0.25), 0.25);
        assert_eq!(Scoring::COSINE.cosine_for(0.25), 0.25);
    }

    #[test]
    fn similarities_are_every_dot_product() {
        let a = [1.0f32, 0.0, 0.0, 1.0, 1.0, 1.0];
        let b = [2.0f32, 3.0, -1.0, 0.5];
        assert_eq!(similarities(&a, 3, &b, 2, 2), vec![2.0, -1.0, 3.0, 0.5, 5.0, -0.5]);
        assert!(similarities(&a, 4, &b, 2, 2).is_empty(), "sizes that do not fit");
        assert!(similarities(&[], 0, &b, 2, 2).is_empty());
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn the_web_build_reports_unsupported() {
        assert!(matches!(load(Path::new("."), &manifest::builtin_siglip2_b16_256(), Towers::BOTH), Err(Error::Unsupported)));
    }
}
