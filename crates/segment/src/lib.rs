//! SAM 3 (Segment Anything with Concepts, Meta, 2025) inference in pure Rust on candle.
//!
//! - **Point prompts** ([`Sam3::segment_clicks`]): positive and negative clicks select one
//!   object, via SAM 3's interactive (SAM 2 style) prompt encoder and mask decoder.
//! - **Text prompts** ([`Sam3::segment_text`]): a noun phrase ("the red car", "sky") selects
//!   every instance of that concept, via the CLIP text encoder and the DETR detector.
//!
//! The expensive part, the 32-layer ViT over the 1008 × 1008 input, runs once per image
//! ([`Sam3::encode`]); each click or prompt then reuses the [`Encoded`] features.
//!
//! On macOS the model runs on the GPU through Metal; elsewhere (for now) on the CPU. The
//! weights are not part of LightCraft: they are the `facebook/sam3` checkpoint (SAM License),
//! read from a directory the user downloads them to ([`Sam3::load`]).
//!
//! Ported from the Hugging Face `transformers` implementation (Apache-2.0); see `NOTICE`.
//! This crate is therefore licensed under the Apache License 2.0 only (not the MIT option of
//! the rest of LightCraft). No UI dependencies (L3).
//!
//! [`fetch`] downloads the model, only when the user asks for it (from configurable mirrors,
//! verified before use). Nothing in LightCraft requires the model: without it, AI masks report
//! that it isn't installed and everything else works.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`src/transformers/models/sam3` and `sam3_tracker`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
mod clip;
#[cfg(not(target_arch = "wasm32"))]
mod detector;
#[cfg(not(target_arch = "wasm32"))]
pub mod fetch;
pub mod mask;
#[cfg(not(target_arch = "wasm32"))]
mod neck;
#[cfg(not(target_arch = "wasm32"))]
mod nn;
pub mod tokenizer;
#[cfg(not(target_arch = "wasm32"))]
mod tracker;
#[cfg(not(target_arch = "wasm32"))]
mod vit;
#[cfg(not(target_arch = "wasm32"))]
mod weights;

use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use candle_core::{DType, Device, Tensor};
#[cfg(not(target_arch = "wasm32"))]
use candle_nn::VarBuilder;

#[cfg(not(target_arch = "wasm32"))]
pub use tracker::Click;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SAM 3 model: {0}")]
    Model(String),
    #[error("SAM 3 model files not found in {0} (expected model.safetensors, vocab.json, merges.txt)")]
    Missing(PathBuf),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Candle(#[from] candle_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Side of the square model input.
pub const INPUT: usize = 1008;
/// Side of the low-resolution masks the model predicts.
pub const MASK_SIDE: usize = 288;
#[cfg(not(target_arch = "wasm32"))]
const _: () = assert!(MASK_SIDE == tracker::LOW_RES && INPUT == vit::IMAGE);

/// The files of a model directory.
pub const WEIGHTS_FILE: &str = "model.safetensors";

/// Whether `dir` holds a usable checkpoint.
pub fn is_model_dir(dir: &Path) -> bool {
    dir.join(WEIGHTS_FILE).is_file() && dir.join("vocab.json").is_file() && dir.join("merges.txt").is_file()
}

#[cfg(not(target_arch = "wasm32"))]
/// The best device here: Metal on macOS (when a GPU is available), else the CPU.
pub fn best_device() -> Device {
    #[cfg(target_os = "macos")]
    if let Ok(d) = Device::new_metal(0) {
        return d;
    }
    Device::Cpu
}

#[cfg(not(target_arch = "wasm32"))]
/// A loaded model. The vision backbone loads with it; the point and text heads load on first use.
pub struct Sam3 {
    dir: PathBuf,
    device: Device,
    weights: Arc<weights::Weights>,
    backbone: vit::Vit,
    tracker_neck: Option<neck::Neck>,
    tracker: Option<tracker::Tracker>,
    detector: Option<detector::Detector>,
    tokenizer: Option<tokenizer::Tokenizer>,
}

#[cfg(not(target_arch = "wasm32"))]
/// An image run through the backbone, with the head-specific features computed on demand.
pub struct Encoded {
    backbone: Tensor,
    tracker: Option<tracker::Features>,
    detector: Option<detector::Features>,
}

/// A predicted mask: `MASK_SIDE²` logits over the model's square input (which the image was
/// stretched to), and a confidence 0..1.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub logits: Vec<f32>,
    pub score: f32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Sam3 {
    /// Load from a directory holding `model.safetensors` (the `facebook/sam3` checkpoint in
    /// Hugging Face format), `vocab.json` and `merges.txt`.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let weights = weights::Weights::open(&dir.join(WEIGHTS_FILE))?;
        let backbone = vit::Vit::new(Self::vb(&weights, &device).pp("detector_model.vision_encoder.backbone"))?;
        Ok(Self { dir: dir.to_path_buf(), device, weights, backbone, tracker_neck: None, tracker: None, detector: None, tokenizer: None })
    }

    fn vb<'a>(w: &Arc<weights::Weights>, device: &Device) -> VarBuilder<'a> {
        VarBuilder::from_backend(Box::new(weights::Backend(w.clone())), DType::F32, device.clone())
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Run the backbone on an RGB image (8-bit, row-major, `w × h`), stretched to the square
    /// input like the reference preprocessing.
    pub fn encode(&self, rgb: &[u8], w: usize, h: usize) -> Result<Encoded> {
        let pixels = mask::preprocess(rgb, w, h, INPUT)?;
        let pixels = Tensor::from_vec(pixels, (1, 3, INPUT, INPUT), &self.device)?;
        self.encode_pixels(&pixels)
    }

    /// Run the backbone on preprocessed pixels `[1, 3, 1008, 1008]` (−1..1).
    pub fn encode_pixels(&self, pixels: &Tensor) -> Result<Encoded> {
        Ok(Encoded { backbone: self.backbone.forward(&pixels.to_device(&self.device)?)?, tracker: None, detector: None })
    }

    fn tracker_parts(&mut self) -> Result<(&neck::Neck, &tracker::Tracker)> {
        if self.tracker_neck.is_none() {
            self.tracker_neck = Some(neck::Neck::new(Self::vb(&self.weights, &self.device).pp("tracker_neck"))?);
        }
        if self.tracker.is_none() {
            self.tracker = Some(tracker::Tracker::new(Self::vb(&self.weights, &self.device).pp("tracker_model"))?);
        }
        match (&self.tracker_neck, &self.tracker) {
            (Some(n), Some(t)) => Ok((n, t)),
            _ => Err(Error::Model("tracker failed to load".into())),
        }
    }

    /// Compute the click decoder's features for `enc` now (so the first click is fast).
    pub fn prepare_clicks(&mut self, enc: &mut Encoded) -> Result<()> {
        let (neck, tracker) = self.tracker_parts()?;
        if enc.tracker.is_none() {
            let levels = neck.forward(&enc.backbone, 3)?;
            enc.tracker = Some(tracker.features(&levels)?);
        }
        Ok(())
    }

    /// Segment one object from clicks (positions normalized to the image, 0..1).
    pub fn segment_clicks(&mut self, enc: &mut Encoded, clicks: &[Click]) -> Result<Prediction> {
        if clicks.is_empty() {
            return Err(Error::Model("no clicks".into()));
        }
        let (neck, tracker) = self.tracker_parts()?;
        if enc.tracker.is_none() {
            let levels = neck.forward(&enc.backbone, 3)?;
            enc.tracker = Some(tracker.features(&levels)?);
        }
        let f = enc.tracker.as_ref().ok_or_else(|| Error::Model("tracker features".into()))?;
        // one click: SAM's three alternatives, best by predicted IoU; more: the single mask
        let p = tracker.predict(f, clicks, clicks.len() == 1)?;
        Ok(Prediction { logits: p.logits, score: p.iou * sigmoid(p.object_score) })
    }

    fn detector_parts(&mut self) -> Result<(&detector::Detector, &tokenizer::Tokenizer)> {
        if self.detector.is_none() {
            self.detector = Some(detector::Detector::new(Self::vb(&self.weights, &self.device).pp("detector_model"))?);
        }
        if self.tokenizer.is_none() {
            self.tokenizer = Some(tokenizer::Tokenizer::load(&self.dir)?);
        }
        match (&self.detector, &self.tokenizer) {
            (Some(d), Some(t)) => Ok((d, t)),
            _ => Err(Error::Model("detector failed to load".into())),
        }
    }

    /// Segment every instance of the concept `text` names. Instances scoring above
    /// `threshold` (0..1, 0.5 is the reference default) are merged into one mask; `None` when
    /// nothing matches.
    pub fn segment_text(&mut self, enc: &mut Encoded, text: &str, threshold: f32) -> Result<Option<Prediction>> {
        let (det, tok) = self.detector_parts()?;
        if enc.detector.is_none() {
            enc.detector = Some(det.features(&enc.backbone)?);
        }
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        let out = det.predict(f, &ids, &valid)?;
        Ok(detector::merge(&out, threshold))
    }

    /// Raw detector outputs for `text` (tests and tools).
    pub fn detect(&mut self, enc: &mut Encoded, text: &str) -> Result<detector::Output> {
        let (det, tok) = self.detector_parts()?;
        if enc.detector.is_none() {
            enc.detector = Some(det.features(&enc.backbone)?);
        }
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        det.predict(f, &ids, &valid)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Encoded {
    /// The backbone output `[1, 1024, 72, 72]` (tests).
    pub fn backbone(&self) -> &Tensor {
        &self.backbone
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(not(target_arch = "wasm32"))]
pub use detector::Output as DetectorOutput;

#[cfg(not(target_arch = "wasm32"))]
/// Wait for queued GPU work to finish (benchmarks).
pub fn sync(model: &Sam3) -> Result<()> {
    Ok(model.device.synchronize()?)
}
