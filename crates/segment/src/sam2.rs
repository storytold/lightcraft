//! SAM 2.1 (Segment Anything 2, Meta, 2024) image segmentation from clicks, in pure Rust on candle.
//!
//! The image path of `Sam2Model`: the Hiera backbone and FPN neck ([`crate::hiera`]) encode the
//! image stretched to 1024 × 1024 once ([`Sam2::encode`]); the "no memory" embedding is added
//! to the coarsest (64 × 64) level, and the two finer levels are projected for the mask
//! decoder. Each click then runs the prompt encoder and the two-way mask decoder shared with
//! SAM 3's tracker ([`crate::tracker`]) on those features ([`Sam2::segment_clicks`]), giving
//! 256 × 256 mask logits over the stretched square image. No video memory or tracking.
//!
//! The weights are `facebook/sam2.1-hiera-tiny` (Apache-2.0) in Hugging Face format (its
//! `sam2_video` checkpoint: this reads only the image-model tensors). Other SAM 2 / 2.1 sizes
//! load too: the Hiera hyper-parameters come from `config.json`.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 2 code of Hugging Face Transformers (`models/sam2/modeling_sam2.py`,
//! `processing_sam2.py`, `image_processing_sam2.py`), Copyright The HuggingFace Team and Meta
//! Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;

use crate::hiera::{FPN, FpnNeck, Hiera, HieraConfig};
use crate::mask::{IMAGENET_MEAN, IMAGENET_STD, preprocess_normalized};
use crate::tracker::{Click, Decoded, Features, Tracker};
use crate::{Error, Prediction, Result, WEIGHTS_FILE, weights};

/// Side of the low-resolution masks SAM 2 predicts (4 × the 64 grid), over the stretched
/// square image.
pub const SAM2_MASK_SIDE: usize = 256;
/// Side of SAM 2's square input.
pub const SAM2_INPUT: usize = 1024;

/// The config file next to the weights.
const CONFIG_FILE: &str = "config.json";

/// Whether `dir` holds a SAM 2 checkpoint in Hugging Face format (`model.safetensors` and a
/// `config.json` whose `model_type` is `sam2` or `sam2_video`).
pub fn is_sam2_dir(dir: &Path) -> bool {
    dir.join(WEIGHTS_FILE).is_file() && read_config(dir).is_ok()
}

fn read_config(dir: &Path) -> Result<serde_json::Value> {
    let path = dir.join(CONFIG_FILE);
    let meta = std::fs::metadata(&path).map_err(|_| Error::Sam2Missing(dir.to_path_buf()))?;
    if meta.len() > 1 << 20 {
        return Err(Error::Sam2(format!("{}: too large for a config file", path.display())));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| Error::Sam2(format!("{}: {e}", path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Sam2(format!("{}: {e}", path.display())))?;
    match v.get("model_type").and_then(|t| t.as_str()) {
        Some("sam2" | "sam2_video") => Ok(v),
        other => Err(Error::Sam2(format!("{}: not a SAM 2 model (model_type {other:?})", path.display()))),
    }
}

/// SAM 2 errors keep their own name (the shared modules report `Error::Model`).
fn sam2_err(e: Error) -> Error {
    match e {
        Error::Model(m) => Error::Sam2(m),
        e => e,
    }
}

/// A loaded SAM 2 model (image encoder, prompt encoder and mask decoder).
pub struct Sam2 {
    device: Device,
    image: usize,
    backbone: Hiera,
    neck: FpnNeck,
    /// `[1, 1, 1, 256]`.
    no_memory: Tensor,
    decoder: Tracker,
}

/// An image run through the encoder: the mask decoder's three feature levels.
pub struct Sam2Encoded {
    features: Features,
}

impl Sam2 {
    /// Load from a directory holding `model.safetensors` and `config.json` (the
    /// `facebook/sam2.1-hiera-tiny` checkpoint in Hugging Face format), on [`crate::best_device`].
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, crate::best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        Self::load_inner(dir, device).map_err(sam2_err)
    }

    fn load_inner(dir: &Path, device: Device) -> Result<Self> {
        if !dir.join(WEIGHTS_FILE).is_file() {
            return Err(Error::Sam2Missing(dir.to_path_buf()));
        }
        let cfg = HieraConfig::from_json(&read_config(dir)?)?;
        let w = weights::Weights::open(&dir.join(WEIGHTS_FILE))?;
        let vb = VarBuilder::from_backend(Box::new(weights::Backend(w)), DType::F32, device.clone());
        Self::from_vb(&cfg, vb, device)
    }

    fn from_vb(cfg: &HieraConfig, vb: VarBuilder, device: Device) -> Result<Self> {
        let backbone = Hiera::new(cfg, vb.pp("vision_encoder.backbone"))?;
        let neck = FpnNeck::new(cfg, vb.pp("vision_encoder.neck"))?;
        let no_memory = vb.get((1, 1, FPN), "no_memory_embedding")?.reshape((1, 1, 1, FPN))?;
        let decoder = Tracker::with_geometry(vb.clone(), cfg.image, cfg.grid())?;
        Ok(Self { device, image: cfg.image, backbone, neck, no_memory, decoder })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Side of the predicted masks: [`SAM2_MASK_SIDE`] for the released checkpoints (1/4 of the
    /// 1024 input).
    pub fn mask_side(&self) -> usize {
        self.image / 4
    }

    /// Run the image encoder on an RGB image (8-bit, row-major, `w × h`), stretched to the
    /// 1024 × 1024 input as the reference preprocessing does (antialiased bilinear, ImageNet
    /// mean and standard deviation).
    pub fn encode(&self, rgb: &[u8], w: usize, h: usize) -> Result<Sam2Encoded> {
        let pixels = preprocess_normalized(rgb, w, h, self.image, IMAGENET_MEAN, IMAGENET_STD).map_err(sam2_err)?;
        let pixels = Tensor::from_vec(pixels, (1, 3, self.image, self.image), &self.device)?;
        self.encode_pixels(&pixels)
    }

    /// Run the image encoder on preprocessed pixels `[1, 3, 1024, 1024]`.
    pub fn encode_pixels(&self, pixels: &Tensor) -> Result<Sam2Encoded> {
        self.encode_inner(pixels).map_err(sam2_err)
    }

    fn encode_inner(&self, pixels: &Tensor) -> Result<Sam2Encoded> {
        let stages = self.backbone.forward(&pixels.to_device(&self.device)?)?;
        let levels = self.neck.forward(&stages)?;
        let [l0, l1, l2] = &levels[..] else { return Err(Error::Model("neck: expected three levels".into())) };
        let l2 = l2.broadcast_add(&self.no_memory)?;
        Ok(Sam2Encoded { features: self.decoder.features_nhwc(l0, l1, &l2)? })
    }

    /// Segment one object from clicks (positions normalized to the image, 0..1). One click: the
    /// best of SAM's three alternatives by predicted IoU; more: the single-mask output (with
    /// SAM 2's stability fallback). Logits are `SAM2_MASK_SIDE²` over the stretched image; the
    /// score is the predicted IoU times the object-presence probability.
    pub fn segment_clicks(&self, enc: &Sam2Encoded, clicks: &[Click]) -> Result<Prediction> {
        self.segment_clicks_with(enc, clicks, clicks.len() == 1)
    }

    /// [`Self::segment_clicks`] choosing the output: `multimask` = best of the three
    /// alternatives, else the single-mask output (the reference's `multimask_output=False`).
    pub fn segment_clicks_with(&self, enc: &Sam2Encoded, clicks: &[Click], multimask: bool) -> Result<Prediction> {
        if clicks.is_empty() {
            return Err(Error::Sam2("no clicks".into()));
        }
        if clicks.len() > 1024 {
            return Err(Error::Sam2(format!("too many clicks ({})", clicks.len())));
        }
        let p = self.decoder.predict(&enc.features, clicks, multimask).map_err(sam2_err)?;
        Ok(Prediction { logits: p.logits, score: p.iou * crate::sigmoid(p.object_score) })
    }

    /// All four decoder outputs for `clicks` (`[4, 256, 256]` logits, predicted IoUs, object
    /// score logit); tests and tools.
    pub fn decode(&self, enc: &Sam2Encoded, clicks: &[Click]) -> Result<Decoded> {
        if clicks.is_empty() {
            return Err(Error::Sam2("no clicks".into()));
        }
        self.decoder.decode(&enc.features, clicks).map_err(sam2_err)
    }

    /// The prompt encoder's sparse embeddings `[1, n + 1, 256]` (tests).
    pub fn sparse_embeddings(&self, clicks: &[Click]) -> Result<Tensor> {
        self.decoder.embed_clicks(clicks).map_err(sam2_err)
    }

    /// The backbone's four stage outputs and the neck's three levels (all channel-last; before the
    /// decoder's projections); tests.
    pub fn encoder_stages(&self, pixels: &Tensor) -> Result<(Vec<Tensor>, Vec<Tensor>)> {
        let stages = self.backbone.forward(&pixels.to_device(&self.device)?).map_err(sam2_err)?;
        let levels = self.neck.forward(&stages).map_err(sam2_err)?;
        Ok((stages, levels))
    }
}

impl Sam2Encoded {
    /// The decoder's features, channel-first: the 64 × 64 image embedding (with the no-memory
    /// embedding), and the projected 256 × 256 and 128 × 128 levels (tests).
    pub fn features(&self) -> Result<(Tensor, Tensor, Tensor)> {
        self.features.planar().map_err(sam2_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-sam2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A tiny Hiera with random weights runs end to end: shapes, windows, pooling, decoder.
    #[test]
    fn tiny_random_model_runs() {
        let cfg = HieraConfig {
            embed: 8,
            blocks_per_stage: vec![1, 2, 2, 1],
            dims: vec![8, 16, 32, 64],
            heads: vec![1, 2, 2, 4],
            windows: vec![8, 4, 6, 4],
            global: vec![3],
            pos_bkg: [3, 3],
            q_pool: 3,
            image: 128,
            top_down: vec![2, 3],
        };
        let map = candle_nn::VarMap::new();
        let vb = VarBuilder::from_varmap(&map, DType::F32, &Device::Cpu);
        let model = Sam2::from_vb(&cfg, vb, Device::Cpu).unwrap();
        assert_eq!(model.mask_side(), 32);
        let rgb: Vec<u8> = (0..50 * 40 * 3).map(|i| (i * 7 % 256) as u8).collect();
        let enc = model.encode(&rgb, 50, 40).unwrap();
        let (image, s0, s1) = enc.features().unwrap();
        assert_eq!((image.dims(), s0.dims(), s1.dims()), (&[1, 256, 8, 8][..], &[1, 32, 32, 32][..], &[1, 64, 16, 16][..]));
        let one = [Click { x: 0.3, y: 0.6, positive: true }];
        let p = model.segment_clicks(&enc, &one).unwrap();
        assert_eq!(p.logits.len(), 32 * 32);
        assert!(p.logits.iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&p.score));
        let two = [one[0], Click { x: 1.0, y: 0.0, positive: false }];
        assert_eq!(model.segment_clicks(&enc, &two).unwrap().logits.len(), 32 * 32);
        let d = model.decode(&enc, &two).unwrap();
        assert_eq!((d.masks.dims(), d.iou.len()), (&[4, 32, 32][..], 4));
        // bad inputs are errors
        assert!(model.segment_clicks(&enc, &[]).is_err());
        assert!(model.encode(&rgb, 50, 41).is_err());
        assert!(model.encode_pixels(&Tensor::zeros((1, 3, 64, 64), DType::F32, &Device::Cpu).unwrap()).is_err());
    }

    #[test]
    fn model_and_features_can_cross_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Sam2>();
        shared::<Sam2Encoded>();
    }

    #[test]
    fn missing_or_garbage_model_dirs_are_errors() {
        let d = tmp("empty");
        assert!(!is_sam2_dir(&d));
        let e = Sam2::load_on(&d, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("SAM 2 model files not found"), "{e}");
        // weights present but garbage, config of another model
        std::fs::write(d.join(WEIGHTS_FILE), b"not safetensors").unwrap();
        std::fs::write(d.join(CONFIG_FILE), br#"{"model_type": "sam3"}"#).unwrap();
        assert!(!is_sam2_dir(&d));
        assert!(Sam2::load_on(&d, Device::Cpu).err().unwrap().to_string().contains("not a SAM 2 model"));
        std::fs::write(d.join(CONFIG_FILE), br#"{"model_type": "sam2_video"}"#).unwrap();
        assert!(is_sam2_dir(&d));
        let e = Sam2::load_on(&d, Device::Cpu).err().unwrap().to_string();
        assert!(e.starts_with("SAM 2 model:"), "{e}");
        std::fs::write(d.join(CONFIG_FILE), b"{ broken").unwrap();
        assert!(Sam2::load_on(&d, Device::Cpu).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
