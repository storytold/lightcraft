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
pub mod ane;
#[cfg(not(target_arch = "wasm32"))]
mod clip;
#[cfg(not(target_arch = "wasm32"))]
pub mod coreml_vit;
#[cfg(not(target_arch = "wasm32"))]
mod detector;
#[cfg(not(target_arch = "wasm32"))]
pub mod fetch;
pub mod mask;
#[cfg(not(target_arch = "wasm32"))]
mod neck;
#[cfg(not(target_arch = "wasm32"))]
mod nn;
#[cfg(not(target_arch = "wasm32"))]
pub mod remote;
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
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    CoreMl(#[from] lightcraft_coreml::Error),
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
/// Whether [`best_device`] can be a GPU in this build (Metal on macOS). Where it can, a caller that
/// sees the model fail there can load it again with [`Sam3::load_cpu`].
pub const GPU_BUILD: bool = cfg!(target_os = "macos");

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
    /// The Neural Engine's backbone, when it is set up and checked (see [`ane`]).
    ane: Arc<ane::NeuralEngine>,
    /// The fastest backbone candle has run here (ms): what the Neural Engine is compared with.
    candle_ms: std::sync::Mutex<Option<f64>>,
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

    /// [`Sam3::load`] on the CPU, whatever GPU there is.
    pub fn load_cpu(dir: &Path) -> Result<Self> {
        Self::load_on(dir, Device::Cpu)
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let weights = weights::Weights::open(&dir.join(WEIGHTS_FILE))?;
        let backbone = vit::Vit::new(Self::vb(&weights, &device).pp("detector_model.vision_encoder.backbone"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            device,
            weights,
            backbone,
            tracker_neck: None,
            tracker: None,
            detector: None,
            tokenizer: None,
            ane: Arc::default(),
            candle_ms: std::sync::Mutex::new(None),
        })
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

    /// Run the backbone on preprocessed pixels `[1, 3, 1008, 1008]` (−1..1): on the Neural Engine when it was set up
    /// ([`Sam3::start_neural_engine`]) and checked against candle, else (or when it fails) with candle.
    pub fn encode_pixels(&self, pixels: &Tensor) -> Result<Encoded> {
        let warm = self.candle_ms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some();
        let backbone = match self.ane.plan(warm) {
            ane::Plan::Candle => self.candle_backbone(pixels)?,
            ane::Plan::Ane(r) => match self.ane_backbone(&r, pixels) {
                Ok(t) => t,
                Err(why) => {
                    self.ane.fell_back(&why);
                    self.candle_backbone(pixels)?
                }
            },
            ane::Plan::Check(r) => {
                let candle = self.candle_backbone(pixels)?;
                self.check_neural_engine(&r, pixels, &candle);
                candle
            }
        };
        Ok(Encoded { backbone, tracker: None, detector: None })
    }

    /// The backbone on candle's device, timed (the fastest time is kept).
    fn candle_backbone(&self, pixels: &Tensor) -> Result<Tensor> {
        let t = std::time::Instant::now();
        let out = self.backbone.forward(&pixels.to_device(&self.device)?)?;
        self.device.synchronize()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let mut best = self.candle_ms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *best = Some(best.map_or(ms, |b| b.min(ms)));
        Ok(out)
    }

    /// The backbone on the Neural Engine: candle's embedding of the pixels in, `[1, 1024, 72, 72]` out.
    fn ane_backbone(&self, r: &ane::Ready, pixels: &Tensor) -> std::result::Result<Tensor, String> {
        let e = |e: candle_core::Error| e.to_string();
        let embedded = self.backbone.embed(&pixels.to_device(&self.device).map_err(e)?).map_err(|e| e.to_string())?;
        let input = embedded.flatten_all().map_err(e)?.to_vec1::<f32>().map_err(e)?;
        let out = r.run(&[1, vit::GRID * vit::GRID, vit::HIDDEN], &input)?;
        Tensor::from_vec(out, (1, vit::GRID, vit::GRID, vit::HIDDEN), &self.device).and_then(|t| t.permute((0, 3, 1, 2))?.contiguous()).map_err(e)
    }

    /// Runs `pixels` through the Neural Engine too and records how it did against `candle` (the answer in use).
    fn check_neural_engine(&self, r: &ane::Ready, pixels: &Tensor, candle: &Tensor) {
        let candle_ms = self.candle_ms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unwrap_or(f64::INFINITY);
        let run = || -> std::result::Result<(Tensor, f64), String> {
            let t = std::time::Instant::now();
            let out = self.ane_backbone(r, pixels)?;
            Ok((out, t.elapsed().as_secs_f64() * 1000.0))
        };
        let outcome = run().and_then(|(first, t1)| {
            // the first run on a freshly loaded model is slower: time the second
            let (_, t2) = run()?;
            let (a, b) = (first.flatten_all().map_err(|e| e.to_string())?, candle.flatten_all().map_err(|e| e.to_string())?);
            let (a, b) = (a.to_vec1::<f32>().map_err(|e| e.to_string())?, b.to_vec1::<f32>().map_err(|e| e.to_string())?);
            Ok((ane::cosine(&a, &b), t1.min(t2)))
        });
        match outcome {
            Ok((cosine, ane_ms)) => r.judge(ane_ms, candle_ms, cosine),
            Err(why) => self.ane.fell_back(&why),
        }
    }

    /// Starts setting up the Neural Engine's backbone in `cache` on its own thread (see [`ane`]): nothing waits for it
    /// and nothing changes until it is ready and has been checked against candle on a photo. Does nothing where there
    /// is no Neural Engine, and after the first call.
    pub fn start_neural_engine(&self, cache: &Path) {
        self.ane.start(self.weights.clone(), cache.to_path_buf());
    }

    /// Where the Neural Engine path stands.
    pub fn neural_engine(&self) -> ane::Status {
        self.ane.status()
    }

    /// A handle to read the Neural Engine path's status from another thread (it outlives the model).
    pub fn neural_engine_handle(&self) -> Arc<ane::NeuralEngine> {
        Arc::clone(&self.ane)
    }

    /// The first layer's input for preprocessed pixels `[1, 3, 1008, 1008]` (−1..1): `[1, 72, 72, 1024]`.
    pub fn embed_pixels(&self, pixels: &Tensor) -> Result<Tensor> {
        self.backbone.embed(&pixels.to_device(&self.device)?)
    }

    /// An [`Encoded`] from backbone features computed elsewhere: `[1, 1024, 72, 72]`, as the backbone gives.
    pub fn encoded_from(&self, backbone: Tensor) -> Encoded {
        Encoded { backbone, tracker: None, detector: None }
    }

    /// Writes the backbone's layers as a Core ML package in `dir` (see [`coreml_vit`]); returns its path and the name
    /// of the model's output. Reads the backbone's weights only (about 1.3 GB of the checkpoint's 3.4 GB); run it off the
    /// UI thread.
    pub fn write_coreml_backbone(&self, dir: &Path) -> Result<(PathBuf, String)> {
        let vb = Self::vb(&self.weights, &Device::Cpu).pp("detector_model.vision_encoder.backbone");
        coreml_vit::write_backbone(&vb, &coreml_vit::Dims::SAM3, dir)
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
