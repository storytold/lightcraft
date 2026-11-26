//! Content models: image and text embeddings for searching photos by what they show.
//!
//! A [`ContentModel`] turns photos and phrases into vectors in one space; [`ContentModel::score`]
//! says how well a photo matches a phrase. The model behind it is described by a [`Manifest`]
//! (what it is, its files, its licence, how it wants its input), so models are interchangeable:
//! the default is Google's SigLIP 2 (base, patch 16, 256 px) on candle ([`siglip`]); a second
//! family (CLIP) drops in behind the same trait.
//!
//! The weights are not part of LightCraft: they are read from a folder the user downloads them
//! to ([`load`]). Everything here runs locally. On macOS the towers run on the GPU through Metal,
//! elsewhere on the CPU; the web build has no towers ([`load`] reports that it's unsupported).
//!
//! The SigLIP towers are ported from the Hugging Face `transformers` implementation (Apache-2.0);
//! see `NOTICE`. This crate is therefore licensed under the Apache License 2.0 only (not the MIT
//! option of the rest of LightCraft). No UI dependencies (L1).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod config;
pub mod manifest;
pub mod preprocess;
#[cfg(not(target_arch = "wasm32"))]
pub mod siglip;
pub mod tokenizer;
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
    /// How well an image embedding matches a text embedding. For a sigmoid model this is a
    /// probability (0..1) that a threshold can be applied to; for a softmax model it only ranks.
    fn score(&self, image: &[f32], text: &[f32]) -> f32;
    fn manifest(&self) -> &Manifest;
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
    manifest::validate(manifest)?;
    match manifest.family {
        manifest::Family::Siglip => Ok(Box::new(siglip::Siglip::load_on(dir, manifest.clone(), towers, best_device())?)),
        manifest::Family::Clip => Err(Error::Model(format!("{}: CLIP models are not supported yet", manifest.id))),
    }
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

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn the_web_build_reports_unsupported() {
        assert!(matches!(load(Path::new("."), &manifest::builtin_siglip2_b16_256(), Towers::BOTH), Err(Error::Unsupported)));
    }
}
