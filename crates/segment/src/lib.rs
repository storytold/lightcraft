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
//! On macOS the model runs on the GPU through Metal ([`Sam3::load`]). Elsewhere candle runs on
//! the CPU, and the backbone, most of the work, can run outside candle on an accelerator, with
//! the two necks after it ([`BackboneAccel`]: `lightcraft-gpu`'s wgpu kernels on Vulkan / DX12, loaded with
//! [`Sam3::load_accelerated`]); when that can't be built or fails on a photo, the candle backbone
//! takes over ([`Sam3::backbone_fallback`] says why; not while the app is closing: [`BackboneAccel::closing`]).
//! The accelerator can also run the text detector's DETR encoder (else, or once it fails, candle does:
//! [`Sam3::detr_fallback`]). The weights are not part of LightCraft: they are the `facebook/sam3` checkpoint
//! (SAM License), read from a directory the user downloads them to.
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
mod cpu;
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
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

#[cfg(not(target_arch = "wasm32"))]
use candle_core::{DType, Tensor};
#[cfg(not(target_arch = "wasm32"))]
use candle_nn::VarBuilder;

#[cfg(not(target_arch = "wasm32"))]
pub use candle_core::Device;
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
/// Where the backbone's weights are in the checkpoint.
pub const BACKBONE_PREFIX: &str = "detector_model.vision_encoder.backbone";

#[cfg(not(target_arch = "wasm32"))]
/// The vision backbone run outside candle (the wgpu compute kernels of `lightcraft-gpu` on
/// Vulkan / DX12 / Metal). It computes what the candle ViT computes, so everything after the
/// backbone (necks, heads) stays as it is.
pub trait BackboneAccel: Send + Sync {
    /// `pixels`: `[3, 1008, 1008]` (CHW, normalized to −1..1). Returns the backbone output
    /// `[1024, 72, 72]` (CHW), as the candle ViT's `[1, 1024, 72, 72]` without the batch.
    fn forward(&self, pixels: &[f32]) -> std::result::Result<Vec<f32>, String>;
    /// What it runs on, for logs and Settings (e.g. "NVIDIA GeForce RTX 5070 (Vulkan)").
    fn device(&self) -> String;

    /// Whether the app is closing, so that an `Err` from it is no failure of the accelerator: it stays in use, and the
    /// work ends with that error instead of running in candle (no CPU inference while the app quits).
    fn closing(&self) -> bool {
        false
    }

    /// Whether it runs the necks ([`BackboneAccel::neck`]); when not, they are never asked for and run in candle.
    fn runs_necks(&self) -> bool {
        false
    }

    /// Whether it runs the text detector's DETR encoder ([`BackboneAccel::detr_encoder`]); when not, candle does.
    fn runs_detr_encoder(&self) -> bool {
        false
    }

    /// Whether it runs the text detector's mask decoder ([`BackboneAccel::mask_decoder`]); when not, candle does.
    fn runs_mask_decoder(&self) -> bool {
        false
    }

    /// The feature pyramid of the `which` neck for a backbone output `[1024, 72, 72]` (CHW): the
    /// `n` finest levels, each `[256, s, s]` (CHW; s = 288, 144, 72, 36), as the candle `Neck::forward`.
    /// `Err` when this accelerator doesn't run it (the candle neck runs instead).
    fn neck(&self, which: NeckKind, backbone: &[f32], n: usize) -> std::result::Result<Vec<Vec<f32>>, String> {
        let _ = (which, backbone, n);
        Err("no neck on this accelerator".into())
    }

    /// The detector's DETR encoder (6 layers): the image tokens `[5184, 256]` with their sine
    /// position encoding `pos` `[5184, 256]` attend to themselves and to the prompt `text`
    /// `[32, 256]` (`valid` marks its real tokens). Returns the encoder memory `[5184, 256]`.
    /// `Err` when this accelerator doesn't run it (the candle encoder runs instead).
    fn detr_encoder(&self, image: &[f32], pos: &[f32], text: &[f32], valid: &[bool]) -> std::result::Result<Vec<f32>, String> {
        let _ = (image, pos, text, valid);
        Err("no DETR encoder on this accelerator".into())
    }

    /// The detector's mask decoder: the encoder memory `[5184, 256]` attends to the prompt `text` `[32, 256]` (`valid`
    /// marks its real tokens) and is added to it, the pixel decoder brings that up to 288² with the detector neck's
    /// `levels` `[256, 144, 144]` and `[256, 288, 288]` (CHW), and each of the `rows` `[r, 256]` (1 ≤ r ≤ 256, with
    /// `bias` `[r]`) is a 1 × 1 projection of its output. Returns `[r, 288, 288]` (the detector passes its mask
    /// embeddings with the instance projection folded in: the per-query mask logits). `Err` when this accelerator
    /// doesn't run it (the candle decoder runs instead).
    fn mask_decoder(
        &self,
        memory: &[f32],
        text: &[f32],
        valid: &[bool],
        levels: [&[f32]; 2],
        rows: &[f32],
        bias: &[f32],
    ) -> std::result::Result<Vec<f32>, String> {
        let _ = (memory, text, valid, levels, rows, bias);
        Err("no mask decoder on this accelerator".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// The two feature pyramids of the checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeckKind {
    /// `tracker_neck`: the click decoder's (3 levels).
    Tracker,
    /// `detector_model.vision_encoder.neck`: the text detector's (3 levels).
    Detector,
}

#[cfg(not(target_arch = "wasm32"))]
/// Read access to a checkpoint's tensors (for accelerators that upload weights themselves;
/// cloning shares the open file, so an accelerator can keep one and load more layers later).
#[derive(Clone)]
pub struct WeightReader(Arc<weights::Weights>);

#[cfg(not(target_arch = "wasm32"))]
impl WeightReader {
    /// The checkpoint in a model directory (see [`is_model_dir`]).
    pub fn open(dir: &Path) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        Ok(Self(weights::Weights::open(&dir.join(WEIGHTS_FILE))?))
    }

    /// The stored shape of `name`, without reading its data.
    pub fn shape(&self, name: &str) -> Option<Vec<usize>> {
        self.0.shape(name).map(<[usize]>::to_vec)
    }

    /// Tensor `name`: its shape and its values (f32, row-major).
    pub fn read(&self, name: &str) -> Result<(Vec<usize>, Vec<f32>)> {
        let t = self.0.read(name)?;
        Ok((t.dims().to_vec(), t.flatten_all()?.to_vec1::<f32>()?))
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// A loaded model. The vision backbone loads with it (unless an accelerator runs it); the point
/// and text heads load on first use.
pub struct Sam3 {
    dir: PathBuf,
    device: Device,
    weights: Arc<weights::Weights>,
    /// The candle backbone: loaded with the model, or only when an accelerator fails.
    backbone: OnceLock<vit::Vit>,
    accel: Mutex<Option<Box<dyn BackboneAccel>>>,
    /// Why the accelerator `load_accelerated` was given is not (or no longer) used.
    fallback: Mutex<Option<String>>,
    /// Why the DETR encoder runs in candle although there is an accelerator (it failed there once).
    detr_fallback: OnceLock<String>,
    /// The same for the text detector's mask decoder.
    mask_fallback: OnceLock<String>,
    /// The necks the accelerator failed on: candle runs them from then on.
    necks_off: Mutex<Vec<NeckKind>>,
    /// The candle tracker neck: loaded on first use, unless the accelerator runs it.
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

    /// [`Sam3::load`] on the CPU, whatever GPU there is.
    pub fn load_cpu(dir: &Path) -> Result<Self> {
        Self::load_on(dir, Device::Cpu)
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let weights = weights::Weights::open(&dir.join(WEIGHTS_FILE))?;
        hugging_face_format(&weights)?;
        let model = Self::unloaded(dir, device, weights, None);
        model.candle_backbone()?;
        Ok(model)
    }

    /// Load with the backbone on an accelerator `make` builds from the checkpoint, and the rest
    /// on `device`. When `make` fails the candle backbone loads instead, and when the
    /// accelerator fails later, [`Sam3::encode`] falls back to it (logged, not an error), unless the app is closing.
    pub fn load_accelerated(
        dir: &Path,
        device: Device,
        make: impl FnOnce(&WeightReader) -> std::result::Result<Box<dyn BackboneAccel>, String>,
    ) -> Result<Self> {
        let reader = WeightReader::open(dir)?;
        hugging_face_format(&reader.0)?;
        match make(&reader) {
            Ok(accel) => {
                log::info!("SAM 3 backbone on {}", accel.device());
                Ok(Self::unloaded(dir, device, reader.0, Some(accel)))
            }
            Err(e) => {
                log::warn!("SAM 3 backbone accelerator unavailable ({e}); using the {}", device_name(&device));
                let model = Self::unloaded(dir, device, reader.0, None);
                *model.fallback.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                model.candle_backbone()?;
                Ok(model)
            }
        }
    }

    fn unloaded(dir: &Path, device: Device, weights: Arc<weights::Weights>, accel: Option<Box<dyn BackboneAccel>>) -> Self {
        Self {
            dir: dir.to_path_buf(),
            device,
            weights,
            backbone: OnceLock::new(),
            accel: Mutex::new(accel),
            fallback: Mutex::new(None),
            detr_fallback: OnceLock::new(),
            mask_fallback: OnceLock::new(),
            necks_off: Mutex::new(Vec::new()),
            tracker_neck: None,
            tracker: None,
            detector: None,
            tokenizer: None,
        }
    }

    /// The candle backbone, loading it on first use.
    fn candle_backbone(&self) -> Result<&vit::Vit> {
        if let Some(b) = self.backbone.get() {
            return Ok(b);
        }
        let b = vit::Vit::new(Self::vb(&self.weights, &self.device).pp(BACKBONE_PREFIX))?;
        Ok(self.backbone.get_or_init(|| b))
    }

    /// What the backbone runs on: the accelerator's device, else the candle device ("CPU", "Metal GPU").
    pub fn backbone_device(&self) -> String {
        match &*self.accel.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(a) => a.device(),
            None => device_name(&self.device),
        }
    }

    /// Whether the backbone runs on the accelerator (rather than in candle).
    pub fn accelerated(&self) -> bool {
        self.accel.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Why the backbone runs in candle although an accelerator was offered: it could not be built, or it failed on
    /// a photo (from then on the model stays in candle). `None` when there is no such reason.
    pub fn backbone_fallback(&self) -> Option<String> {
        self.fallback.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Why the text detector's DETR encoder runs in candle although the backbone runs on an accelerator: the
    /// accelerator doesn't run it or failed on it (then it isn't asked again). `None` when there is no such reason.
    pub fn detr_fallback(&self) -> Option<String> {
        self.detr_fallback.get().cloned()
    }

    /// Why the text detector's mask decoder runs in candle although the backbone runs on an accelerator: the
    /// accelerator doesn't run it or failed on it (then it isn't asked again). `None` when there is no such reason.
    pub fn mask_fallback(&self) -> Option<String> {
        self.mask_fallback.get().cloned()
    }

    /// What runs everything after the backbone: the model's device, and which of those parts the accelerator runs
    /// (e.g. "CPU; the necks, the DETR encoder and the mask decoder on the GPU").
    pub fn heads_device(&self) -> String {
        let base = device_name(&self.device);
        // what the accelerator runs (none: there is none any more), less what failed on it
        let (necks, detr, masks) = match self.accel.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            Some(a) => (a.runs_necks(), a.runs_detr_encoder(), a.runs_mask_decoder()),
            None => (false, false, false),
        };
        let mut gpu = Vec::new();
        if necks {
            let off = self.necks_off.lock().unwrap_or_else(PoisonError::into_inner);
            match (off.contains(&NeckKind::Tracker), off.contains(&NeckKind::Detector)) {
                (false, false) => gpu.push("the necks"),
                (false, true) => gpu.push("the click neck"),
                (true, false) => gpu.push("the text neck"),
                (true, true) => {}
            }
        }
        if detr && self.detr_fallback.get().is_none() {
            gpu.push("the DETR encoder");
        }
        if masks && self.mask_fallback.get().is_none() {
            gpu.push("the mask decoder");
        }
        match gpu.split_last() {
            None => base,
            Some((last, [])) => format!("{base}; {last} on the GPU"),
            Some((last, rest)) => format!("{base}; {} and {last} on the GPU", rest.join(", ")),
        }
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
        let side = vit::GRID;
        {
            let mut accel = self.accel.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(a) = accel.as_ref() {
                let px = pixels.to_device(&Device::Cpu)?.flatten_all()?.to_vec1::<f32>()?;
                // what the necks can't use is a failure too: a wrong size, or NaN / ∞ from a broken kernel
                let want = vit::HIDDEN * side * side;
                let checked = a.forward(&px).and_then(|out| {
                    if out.len() != want {
                        Err(format!("it returned {} values instead of {want}", out.len()))
                    } else if out.iter().any(|v| !v.is_finite()) {
                        Err("it returned values that are not numbers".into())
                    } else {
                        Ok(out)
                    }
                });
                match checked {
                    Ok(out) => {
                        let backbone = Tensor::from_vec(out, (1, vit::HIDDEN, side, side), &self.device)?;
                        return Ok(Encoded { backbone, tracker: None, detector: None });
                    }
                    Err(e) if a.closing() => return Err(Error::Model(e)),
                    Err(e) => {
                        log::warn!("SAM 3 backbone failed on {} ({e}); using the {} from now on", a.device(), device_name(&self.device));
                        *self.fallback.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("{} failed: {e}", a.device()));
                        *accel = None;
                    }
                }
            }
        }
        let backbone = self.candle_backbone()?.forward(&pixels.to_device(&self.device)?)?;
        Ok(Encoded { backbone, tracker: None, detector: None })
    }

    /// The `n` finest levels of the `which` neck for `backbone`, computed by the accelerator and put on the model's
    /// device; `None` when there is no accelerator, it doesn't run this neck or failed on it (logged once; candle
    /// runs that neck from then on). `Err` when the app is closing.
    fn accel_neck(&self, which: NeckKind, backbone: &Tensor, n: usize) -> Result<Option<Vec<Tensor>>> {
        let accel = self.accel.lock().unwrap_or_else(PoisonError::into_inner);
        // an accelerator for the backbone only: the necks run in candle, and that is no failure
        let Some(a) = accel.as_ref().filter(|a| a.runs_necks()) else { return Ok(None) };
        let mut off = self.necks_off.lock().unwrap_or_else(PoisonError::into_inner);
        if off.contains(&which) {
            return Ok(None);
        }
        let run = || -> std::result::Result<Vec<Tensor>, String> {
            let x = backbone.to_device(&Device::Cpu).and_then(|t| t.flatten_all()).and_then(|t| t.to_vec1::<f32>()).map_err(|e| e.to_string())?;
            let levels = a.neck(which, &x, n)?;
            if levels.len() != n {
                return Err(format!("it returned {} levels instead of {n}", levels.len()));
            }
            levels
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    // what the heads can't use is a failure too: a wrong size, or NaN / ∞ from a broken kernel
                    let side = tracker::LOW_RES >> i;
                    if l.len() != neck::FPN * side * side {
                        return Err(format!("it returned {} values for level {i} instead of {}", l.len(), neck::FPN * side * side));
                    }
                    // without short-cutting, so that it vectorizes (28 M values)
                    if !l.iter().fold(true, |ok, v| ok & v.is_finite()) {
                        return Err("it returned values that are not numbers".into());
                    }
                    Tensor::from_vec(l, (1, neck::FPN, side, side), &self.device).map_err(|e| e.to_string())
                })
                .collect()
        };
        match run() {
            Ok(levels) => Ok(Some(levels)),
            Err(e) if a.closing() => Err(Error::Model(e)),
            Err(e) => {
                let name = match which {
                    NeckKind::Tracker => "tracker",
                    NeckKind::Detector => "detector",
                };
                log::warn!("SAM 3 {name} neck failed on {} ({e}); using the {} for it from now on", a.device(), device_name(&self.device));
                off.push(which);
                Ok(None)
            }
        }
    }

    fn tracker(&mut self) -> Result<&tracker::Tracker> {
        if self.tracker.is_none() {
            self.tracker = Some(tracker::Tracker::new(Self::vb(&self.weights, &self.device).pp("tracker_model"))?);
        }
        self.tracker.as_ref().ok_or_else(|| Error::Model("tracker failed to load".into()))
    }

    /// The click decoder's features for `enc`, once: the tracker neck on the accelerator when it runs it, else in
    /// candle.
    fn tracker_features(&mut self, enc: &mut Encoded) -> Result<()> {
        if enc.tracker.is_some() {
            return Ok(());
        }
        let levels = match self.accel_neck(NeckKind::Tracker, &enc.backbone, 3)? {
            Some(levels) => levels,
            None => {
                if self.tracker_neck.is_none() {
                    self.tracker_neck = Some(neck::Neck::new(Self::vb(&self.weights, &self.device).pp("tracker_neck"))?);
                }
                let neck = self.tracker_neck.as_ref().ok_or_else(|| Error::Model("tracker neck failed to load".into()))?;
                neck.forward(&enc.backbone, 3)?
            }
        };
        enc.tracker = Some(self.tracker()?.features(&levels)?);
        Ok(())
    }

    /// The `which` neck's three finest levels for `enc`, each `[1, 256, s, s]` (tests and tools): on the
    /// accelerator when it runs it, else in candle.
    pub fn neck_levels(&self, which: NeckKind, enc: &Encoded) -> Result<Vec<Tensor>> {
        if let Some(levels) = self.accel_neck(which, &enc.backbone, 3)? {
            return Ok(levels);
        }
        let prefix = match which {
            NeckKind::Tracker => "tracker_neck",
            NeckKind::Detector => "detector_model.vision_encoder.neck",
        };
        neck::Neck::new(Self::vb(&self.weights, &self.device).pp(prefix))?.forward(&enc.backbone, 3)
    }

    /// Compute the click decoder's features for `enc` now (so the first click is fast).
    pub fn prepare_clicks(&mut self, enc: &mut Encoded) -> Result<()> {
        self.tracker()?;
        self.tracker_features(enc)
    }

    /// Segment one object from clicks (positions normalized to the image, 0..1).
    pub fn segment_clicks(&mut self, enc: &mut Encoded, clicks: &[Click]) -> Result<Prediction> {
        if clicks.is_empty() {
            return Err(Error::Model("no clicks".into()));
        }
        self.tracker_features(enc)?;
        let tracker = self.tracker()?;
        let f = enc.tracker.as_ref().ok_or_else(|| Error::Model("tracker features".into()))?;
        // one click: SAM's three alternatives, best by predicted IoU; more: the single mask
        let p = tracker.predict(f, clicks, clicks.len() == 1)?;
        Ok(Prediction { logits: p.logits, score: p.iou * sigmoid(p.object_score) })
    }

    fn detector_parts(&mut self) -> Result<(&detector::Detector, &tokenizer::Tokenizer, detector::EncoderAccel<'_>)> {
        if self.detector.is_none() {
            self.detector = Some(detector::Detector::new(Self::vb(&self.weights, &self.device).pp("detector_model"))?);
        }
        if self.tokenizer.is_none() {
            self.tokenizer = Some(tokenizer::Tokenizer::load(&self.dir)?);
        }
        match (&self.detector, &self.tokenizer) {
            (Some(d), Some(t)) => {
                Ok((d, t, detector::EncoderAccel { accel: &self.accel, failed: &self.detr_fallback, masks_failed: &self.mask_fallback }))
            }
            _ => Err(Error::Model("detector failed to load".into())),
        }
    }

    /// The detector's features for `enc`, once: its neck on the accelerator when it runs it, else in candle.
    fn detector_features(&mut self, enc: &mut Encoded) -> Result<()> {
        if enc.detector.is_none() {
            let levels = self.accel_neck(NeckKind::Detector, &enc.backbone, 3)?;
            let (det, _, _) = self.detector_parts()?;
            enc.detector = Some(det.features(&enc.backbone, levels)?);
        }
        Ok(())
    }

    /// Segment every instance of the concept `text` names. Instances scoring above
    /// `threshold` (0..1, 0.5 is the reference default) are merged into one mask; `None` when
    /// nothing matches.
    pub fn segment_text(&mut self, enc: &mut Encoded, text: &str, threshold: f32) -> Result<Option<Prediction>> {
        self.detector_features(enc)?;
        let (det, tok, accel) = self.detector_parts()?;
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        let out = det.predict(f, &ids, &valid, &accel)?;
        Ok(detector::merge(&out, threshold))
    }

    /// Raw detector outputs for `text` (tests and tools).
    pub fn detect(&mut self, enc: &mut Encoded, text: &str) -> Result<detector::Output> {
        self.detector_features(enc)?;
        let (det, tok, accel) = self.detector_parts()?;
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        det.predict(f, &ids, &valid, &accel)
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
/// A candle device as people call it ("CPU", "Metal GPU").
pub fn device_name(device: &Device) -> String {
    match device {
        Device::Cpu => "CPU".into(),
        Device::Metal(_) => "Metal GPU".into(),
        Device::Cuda(_) => "CUDA GPU".into(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Meta's original checkpoint (`sam3.pt`, or a `sam3.safetensors` conversion of it as on community
/// mirrors) names its tensors differently: say so, rather than name the first tensor it lacks (#3378).
fn hugging_face_format(weights: &weights::Weights) -> Result<()> {
    let first = "detector_model.vision_encoder.backbone.embeddings.patch_embeddings.projection.weight";
    if !weights.contains(first) && (weights.has_prefix("detector.backbone.") || weights.has_prefix("tracker.")) {
        return Err(Error::Model(format!(
            "{WEIGHTS_FILE} is Meta's original checkpoint (sam3.pt, or a sam3.safetensors conversion of it as on community mirrors); \
             LightCraft needs the Hugging Face format: {WEIGHTS_FILE} from facebook/sam3"
        )));
    }
    Ok(())
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A model folder whose checkpoint holds no tensors: enough to load with an accelerator, never to run candle.
    #[allow(clippy::unwrap_used)]
    fn empty_model(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-sam3-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = 2u64.to_le_bytes().to_vec();
        st.extend_from_slice(b"{}");
        std::fs::write(dir.join(WEIGHTS_FILE), st).unwrap();
        std::fs::write(dir.join("vocab.json"), "{}").unwrap();
        std::fs::write(dir.join("merges.txt"), "").unwrap();
        dir
    }

    /// Answers with `values` backbone values, all `fill`, or fails.
    struct Fake {
        values: usize,
        fill: f32,
        fail: bool,
    }

    impl BackboneAccel for Fake {
        fn forward(&self, pixels: &[f32]) -> std::result::Result<Vec<f32>, String> {
            assert_eq!(pixels.len(), 3 * INPUT * INPUT);
            if self.fail { Err("device lost".into()) } else { Ok(vec![self.fill; self.values]) }
        }
        fn device(&self) -> String {
            "Fake GPU (Vulkan)".into()
        }
    }

    const FULL: usize = vit::HIDDEN * vit::GRID * vit::GRID;

    fn with(fake: Fake) -> impl FnOnce(&WeightReader) -> std::result::Result<Box<dyn BackboneAccel>, String> {
        move |_| Ok(Box::new(fake))
    }

    #[test]
    fn the_backbone_runs_on_the_accelerator_without_loading_candle() {
        let dir = empty_model("accel");
        let m = Sam3::load_accelerated(&dir, Device::Cpu, with(Fake { values: FULL, fill: 0.25, fail: false })).unwrap();
        assert!(m.accelerated());
        assert_eq!(m.backbone_device(), "Fake GPU (Vulkan)");
        assert_eq!(m.backbone_fallback(), None);
        // an accelerator for the backbone only: the rest is candle's
        assert_eq!(m.heads_device(), "CPU");
        let enc = m.encode(&[128; 4 * 3 * 3], 4, 3).unwrap();
        assert_eq!(enc.backbone().dims(), [1, vit::HIDDEN, vit::GRID, vit::GRID]);
        assert!(m.backbone.get().is_none(), "the candle backbone was never loaded");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_accelerator_that_cannot_be_built_leaves_the_backbone_to_candle() {
        let dir = empty_model("nomake");
        // the empty checkpoint has no candle backbone: the load fails, but only after trying it
        let r = Sam3::load_accelerated(&dir, Device::Cpu, |_| Err("no adapter".into()));
        assert!(r.is_err_and(|e| !e.to_string().contains("no adapter")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A backbone of `fill`, and neck levels of the right sizes (filled with their index) for the tracker; for the
    /// detector the third level is cut short (`short`) or holds a NaN.
    struct NeckFake {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        short: bool,
    }

    impl BackboneAccel for NeckFake {
        fn forward(&self, _: &[f32]) -> std::result::Result<Vec<f32>, String> {
            Ok(vec![0.5; FULL])
        }
        fn device(&self) -> String {
            "Fake GPU (Vulkan)".into()
        }
        fn runs_necks(&self) -> bool {
            true
        }
        fn neck(&self, which: NeckKind, backbone: &[f32], n: usize) -> std::result::Result<Vec<Vec<f32>>, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!((backbone.len(), backbone[0]), (FULL, 0.5));
            let mut levels: Vec<Vec<f32>> = (0..n).map(|i| vec![i as f32; neck::FPN * (tracker::LOW_RES >> i).pow(2)]).collect();
            if which == NeckKind::Detector {
                if self.short {
                    levels[2].pop();
                } else {
                    levels[2][7] = f32::NAN;
                }
            }
            Ok(levels)
        }
    }

    #[test]
    fn neck_levels_come_from_the_accelerator_and_a_failing_neck_is_left_to_candle() {
        for short in [true, false] {
            let dir = empty_model(&format!("neck-{short}"));
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let fake = NeckFake { calls: calls.clone(), short };
            let m = Sam3::load_accelerated(&dir, Device::Cpu, move |_| Ok(Box::new(fake))).unwrap();
            let enc = m.encode(&[128; 4 * 3 * 3], 4, 3).unwrap();
            assert_eq!(m.heads_device(), "CPU; the necks on the GPU");
            let levels = m.accel_neck(NeckKind::Tracker, enc.backbone(), 3).unwrap().unwrap();
            for (i, (l, side)) in levels.iter().zip([288, 144, 72]).enumerate() {
                assert_eq!(l.dims(), [1, neck::FPN, side, side]);
                assert_eq!(l.flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap(), i as f32);
            }
            // a wrong level: candle's neck from then on, without asking again
            assert!(m.accel_neck(NeckKind::Detector, enc.backbone(), 3).unwrap().is_none());
            assert!(m.accel_neck(NeckKind::Detector, enc.backbone(), 3).unwrap().is_none());
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
            assert_eq!(m.heads_device(), "CPU; the click neck on the GPU");
            // the other neck and the backbone stay on the accelerator
            assert!(m.accel_neck(NeckKind::Tracker, enc.backbone(), 3).unwrap().is_some());
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
            assert!(m.accelerated());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// Fails as the GPU does once the app is closing (the backbone or, with a backbone of zeros, a neck).
    struct Closing {
        backbone: bool,
    }

    impl BackboneAccel for Closing {
        fn forward(&self, _: &[f32]) -> std::result::Result<Vec<f32>, String> {
            if self.backbone { Err("LightCraft is closing".into()) } else { Ok(vec![0.0; FULL]) }
        }
        fn device(&self) -> String {
            "Fake GPU (Vulkan)".into()
        }
        fn closing(&self) -> bool {
            true
        }
        fn runs_necks(&self) -> bool {
            true
        }
        fn neck(&self, _: NeckKind, _: &[f32], _: usize) -> std::result::Result<Vec<Vec<f32>>, String> {
            Err("LightCraft is closing".into())
        }
    }

    #[test]
    fn closing_is_no_accelerator_failure_and_nothing_runs_in_candle() {
        let dir = empty_model("closing");
        let m = Sam3::load_accelerated(&dir, Device::Cpu, with_closing(true)).unwrap();
        let e = m.encode(&[128; 4 * 3 * 3], 4, 3).err().unwrap();
        assert!(e.to_string().contains("LightCraft is closing"), "{e}");
        assert!(m.backbone.get().is_none(), "the candle backbone was never loaded");
        assert!(m.accelerated());
        assert_eq!((m.backbone_device().as_str(), m.backbone_fallback()), ("Fake GPU (Vulkan)", None));
        // a neck: the error, not candle's neck, and the accelerator keeps it
        let m = Sam3::load_accelerated(&dir, Device::Cpu, with_closing(false)).unwrap();
        let enc = m.encode(&[128; 4 * 3 * 3], 4, 3).unwrap();
        for which in [NeckKind::Tracker, NeckKind::Detector] {
            assert!(m.accel_neck(which, enc.backbone(), 3).is_err_and(|e| e.to_string().contains("closing")), "{which:?}");
            assert!(m.neck_levels(which, &enc).is_err_and(|e| e.to_string().contains("closing")), "{which:?}");
        }
        assert_eq!(m.heads_device(), "CPU; the necks on the GPU");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn with_closing(backbone: bool) -> impl FnOnce(&WeightReader) -> std::result::Result<Box<dyn BackboneAccel>, String> {
        move |_| Ok(Box::new(Closing { backbone }))
    }

    #[test]
    fn a_failing_or_wrong_accelerator_is_dropped_for_candle() {
        for (name, fake, why) in [
            ("fail", Fake { values: FULL, fill: 0.0, fail: true }, "device lost"),
            ("short", Fake { values: 10, fill: 0.0, fail: false }, "returned 10 values"),
            ("nan", Fake { values: FULL, fill: f32::NAN, fail: false }, "not numbers"),
        ] {
            let dir = empty_model(name);
            let m = Sam3::load_accelerated(&dir, Device::Cpu, with(fake)).unwrap();
            // the candle backbone takes over (and fails here: the checkpoint is empty)
            assert!(m.encode(&[128; 4 * 3 * 3], 4, 3).is_err(), "{name}");
            assert!(!m.accelerated(), "{name}");
            assert_eq!(m.backbone_device(), "CPU", "{name}");
            let fallback = m.backbone_fallback().unwrap();
            assert!(fallback.starts_with("Fake GPU (Vulkan) failed: ") && fallback.contains(why), "{name}: {fallback}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn meta_original_checkpoint_is_named_as_such() {
        // #3378: a community mirror's `sam3.safetensors` (Meta's key names, C64 RoPE tables) saved as model.safetensors
        let dir = std::env::temp_dir().join(format!("lc-sam3-original-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let header = r#"{"detector.backbone.vision_backbone.trunk.blocks.0.attn.freqs_cis":{"dtype":"C64","shape":[2],"data_offsets":[0,16]},"tracker.sam_mask_decoder.iou_token.weight":{"dtype":"F32","shape":[2],"data_offsets":[16,24]}}"#;
        std::fs::rename(weights::tests::write("original", header, 24), dir.join(WEIGHTS_FILE)).unwrap();
        std::fs::write(dir.join("vocab.json"), "{}").unwrap();
        std::fs::write(dir.join("merges.txt"), "").unwrap();
        let err = Sam3::load_cpu(&dir).err().unwrap().to_string();
        assert!(err.contains("Meta's original checkpoint") && err.contains("facebook/sam3"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
