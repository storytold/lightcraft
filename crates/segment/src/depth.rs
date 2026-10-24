//! Monocular depth with Depth Anything V2 Small (Yang et al., 2024; checkpoint
//! `depth-anything/Depth-Anything-V2-Small-hf`, Apache-2.0): relative inverse depth (larger =
//! nearer) for a whole image, for depth-based masks.
//!
//! The model is a DINOv2 ViT-S/14 backbone (positional embeddings trained for 37 × 37 patches,
//! interpolated bicubically to the input's patch grid) followed by a DPT neck (reassemble the
//! features of layers 3, 6, 9 and 12 at four scales, fuse them coarse to fine) and a small
//! convolutional head that outputs one value per input pixel.
//!
//! Preprocessing follows the reference image processor (`DPTImageProcessor` with
//! `keep_aspect_ratio`, `ensure_multiple_of = 14`, size 518, bicubic): scale both sides by the
//! factor closer to 1 of 518/height and 518/width (so the short side becomes ≈ 518 when
//! downscaling), round each side to a multiple of 14, resize (antialiased bicubic), ImageNet
//! normalization. A 1600 × 1067 photo runs at 770 × 518.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch code of Hugging Face Transformers (`models/depth_anything/modeling_depth_anything.py`,
//! `models/dinov2/modeling_dinov2.py`, `models/dpt/image_processing_dpt.py`), Copyright The
//! HuggingFace Team, Meta Platforms, Inc. and the Depth Anything authors; translated to Rust on
//! candle and restructured. See NOTICE.

use std::path::Path;

use candle_core::{Device, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, Linear, VarBuilder};

use crate::dino::{self, Block, DINOV2_ATTN};
use crate::grid::{Filter, normalize_padded, resize_rgb, round_even};
use crate::nn::{ln, to_vec};
use crate::{Error, Result, best_device};

const WHAT: &str = "Depth Anything";
const MODEL_TYPE: &str = "depth_anything";
/// The target input size (short side, after rounding to the patch size).
pub const TARGET: usize = 518;
const PATCH: usize = 14;
const PRETRAIN_GRID: usize = 37;
const HIDDEN: usize = 384;
const HEADS: usize = 6;
const LAYERS: usize = 12;
/// Backbone layers whose outputs feed the neck (1-based: after layers 3, 6, 9, 12).
const OUT_LAYERS: [usize; 4] = [3, 6, 9, 12];
const NECK: [usize; 4] = [48, 96, 192, 384];
const FUSION: usize = 64;
const HEAD_HIDDEN: usize = 32;
const EPS: f64 = 1e-6;
/// Largest input side accepted (patch grids beyond this make attention too large).
const MAX_INPUT: usize = 2048;

/// Whether `dir` holds a Depth Anything checkpoint (`model.safetensors` and a `config.json` of
/// `model_type` `depth_anything`).
pub fn is_depth_dir(dir: &Path) -> bool {
    dino::is_model_dir(dir, MODEL_TYPE)
}

/// A depth map over the whole image.
#[derive(Clone, Debug)]
pub struct DepthMap {
    /// The model's input size (e.g. 770 × 518 for a 3:2 photo); the map covers the whole image
    /// (stretched by at most half a patch per side), cell `(x, y)` at normalized
    /// `((x + 0.5) / width, (y + 0.5) / height)`.
    pub width: usize,
    pub height: usize,
    /// Relative inverse depth (larger = nearer), normalized to 0..1 over the image, row-major.
    pub data: Vec<f32>,
}

/// The model input size (`w × h`) for an image of `w × h` pixels (`get_resize_output_image_size`
/// of the DPT processor with `keep_aspect_ratio` and `ensure_multiple_of = 14`).
pub fn input_size(w: usize, h: usize) -> (usize, usize) {
    let (sw, sh) = (TARGET as f64 / w.max(1) as f64, TARGET as f64 / h.max(1) as f64);
    // scale as little as possible
    let s = if (1.0 - sw).abs() < (1.0 - sh).abs() { sw } else { sh };
    let fit = |v: f64| (round_even(v / PATCH as f64) * PATCH).clamp(PATCH, MAX_INPUT / PATCH * PATCH);
    (fit(s * w as f64), fit(s * h as f64))
}

/// `DepthAnythingPreActResidualLayer`: x + conv2(relu(conv1(relu(x)))).
struct Residual {
    c1: dino::Conv3x3,
    c2: dino::Conv3x3,
}

impl Residual {
    fn new(vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            c1: dino::Conv3x3::new(FUSION, FUSION, true, vb.pp("convolution1"))?,
            c2: dino::Conv3x3::new(FUSION, FUSION, true, vb.pp("convolution2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = self.c2.forward(&self.c1.forward(&x.relu()?)?.relu()?)?;
        Ok((y + x)?)
    }
}

/// `DepthAnythingFeatureFusionLayer`.
struct Fusion {
    proj: dino::Pointwise,
    res1: Residual,
    res2: Residual,
}

impl Fusion {
    /// Fuse `x` with the next finer level `residual` (if any), refine, and upsample to `size`
    /// (or 2×), bilinear with aligned corners.
    fn forward(&self, x: &Tensor, residual: Option<&Tensor>, size: Option<(usize, usize)>) -> Result<Tensor> {
        let mut x = x.clone();
        if let Some(r) = residual {
            let r = if r.dims() != x.dims() {
                let (_, _, h, w) = x.dims4()?;
                dino::upsample_bilinear(r, h, w, false)?
            } else {
                r.clone()
            };
            x = (x + self.res1.forward(&r)?)?;
        }
        let x = self.res2.forward(&x)?;
        let (_, _, h, w) = x.dims4()?;
        let (oh, ow) = size.unwrap_or((h * 2, w * 2));
        self.proj.forward(&dino::upsample_bilinear(&x, oh, ow, true)?)
    }
}

/// `DepthAnythingReassembleLayer` resize step.
enum Resize {
    Up(Tensor, Tensor),
    Same,
    Down(Conv2d),
}

/// A loaded Depth Anything V2 Small model.
pub struct DepthModel {
    device: Device,
    patch: Conv2d,
    cls: Tensor,
    /// `[37 × 37, 384]` patch positional embeddings (CPU) and the class token's `[1, 1, 384]`.
    pos_table: Vec<f32>,
    pos_cls: Tensor,
    layers: Vec<Block>,
    norm: LayerNorm,
    reassemble: Vec<(Linear, Resize)>,
    convs: Vec<dino::Conv3x3>,
    fusion: Vec<Fusion>,
    head: (dino::Conv3x3, dino::Conv3x3, dino::Pointwise),
}

impl DepthModel {
    /// Load from a directory holding `model.safetensors` and `config.json` of the
    /// `depth-anything/Depth-Anything-V2-Small-hf` checkpoint, on the best device here.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        let (vb, config) = dino::open_checkpoint(dir, WHAT, MODEL_TYPE, &device)?;
        for (path, want) in [
            (&["backbone_config", "hidden_size"][..], HIDDEN),
            (&["backbone_config", "num_attention_heads"][..], HEADS),
            (&["backbone_config", "patch_size"][..], PATCH),
            (&["fusion_hidden_size"][..], FUSION),
        ] {
            let got = dino::config_usize(&config, path, WHAT)?;
            if got != want {
                return Err(Error::Message(format!(
                    "{WHAT} model: config.json has {} = {got}; only the Small model ({want}) is supported",
                    path.join(".")
                )));
            }
        }
        if config.get("depth_estimation_type").and_then(|t| t.as_str()).is_some_and(|t| t != "relative") {
            return Err(Error::Message(format!("{WHAT} model: only the relative-depth model is supported")));
        }
        dino::named(WHAT, Self::build(vb, device))
    }

    fn build(vb: VarBuilder, device: Device) -> Result<Self> {
        let bb = vb.pp("backbone");
        let emb = bb.pp("embeddings");
        let cfg = Conv2dConfig { stride: PATCH, ..Default::default() };
        let patch = candle_nn::conv2d(3, HIDDEN, PATCH, cfg, emb.pp("patch_embeddings.projection"))?;
        let cls = emb.get((1, 1, HIDDEN), "cls_token")?;
        let pos = emb.get((1, PRETRAIN_GRID * PRETRAIN_GRID + 1, HIDDEN), "position_embeddings")?;
        let pos_cls = pos.narrow(1, 0, 1)?;
        let pos_table = to_vec(&pos.narrow(1, 1, PRETRAIN_GRID * PRETRAIN_GRID)?)?;
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            layers.push(Block::new(HIDDEN, HEADS, EPS, &DINOV2_ATTN, bb.pp(format!("encoder.layer.{i}")))?);
        }
        let neck = vb.pp("neck");
        let mut reassemble = Vec::new();
        let mut convs = Vec::new();
        for (i, &c) in NECK.iter().enumerate() {
            let r = neck.pp(format!("reassemble_stage.layers.{i}"));
            let pw = r.get((c, HIDDEN, 1, 1), "projection.weight")?.reshape((c, HIDDEN))?;
            let proj = Linear::new(pw, Some(r.get(c, "projection.bias")?));
            let resize = match i {
                0 => Resize::Up(r.get((c, c, 4, 4), "resize.weight")?, r.get(c, "resize.bias")?),
                1 => Resize::Up(r.get((c, c, 2, 2), "resize.weight")?, r.get(c, "resize.bias")?),
                2 => Resize::Same,
                _ => Resize::Down(candle_nn::conv2d(c, c, 3, Conv2dConfig { padding: 1, stride: 2, ..Default::default() }, r.pp("resize"))?),
            };
            reassemble.push((proj, resize));
            convs.push(dino::Conv3x3::new(c, FUSION, false, neck.pp(format!("convs.{i}")))?);
        }
        let mut fusion = Vec::new();
        for i in 0..NECK.len() {
            let f = neck.pp(format!("fusion_stage.layers.{i}"));
            fusion.push(Fusion {
                proj: dino::Pointwise::new(FUSION, FUSION, f.pp("projection"))?,
                res1: Residual::new(f.pp("residual_layer1"))?,
                res2: Residual::new(f.pp("residual_layer2"))?,
            });
        }
        let h = vb.pp("head");
        let head = (
            dino::Conv3x3::new(FUSION, FUSION / 2, true, h.pp("conv1"))?,
            dino::Conv3x3::new(FUSION / 2, HEAD_HIDDEN, true, h.pp("conv2"))?,
            dino::Pointwise::new(HEAD_HIDDEN, 1, h.pp("conv3"))?,
        );
        Ok(Self { patch, cls, pos_table, pos_cls, layers, norm: ln(HIDDEN, EPS, bb.pp("layernorm"))?, reassemble, convs, fusion, head, device })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The positional embeddings `[1, 1 + gh·gw, 384]` for a `gh × gw` patch grid.
    fn positions(&self, gh: usize, gw: usize) -> Result<Tensor> {
        let table = if (gh, gw) == (PRETRAIN_GRID, PRETRAIN_GRID) {
            self.pos_table.clone()
        } else {
            dino::interpolate_table(&self.pos_table, PRETRAIN_GRID, HIDDEN, gh, gw)?
        };
        let t = Tensor::from_vec(table, (1, gh * gw, HIDDEN), &self.device)?;
        Ok(Tensor::cat(&[&self.pos_cls, &t], 1)?)
    }

    /// The model on preprocessed pixels `[1, 3, H, W]` (H, W multiples of 14): the predicted
    /// depth `[1, H, W]` (unnormalized), and the intermediate tensors by name (`emb`, `feat0`,
    /// `feat3`, `fused0`, `fused3`) for tests.
    pub fn raw(&self, pixels: &Tensor) -> Result<(Tensor, Vec<(&'static str, Tensor)>)> {
        let pixels = pixels.to_device(&self.device)?;
        let (_, _, h, w) = pixels.dims4()?;
        let (gh, gw) = (h / PATCH, w / PATCH);
        if gh == 0 || gw == 0 || h > MAX_INPUT || w > MAX_INPUT {
            return Err(Error::Message(format!("{WHAT}: input of {w} × {h} pixels is not supported")));
        }
        let mut stages = Vec::new();
        let mut prof = dino::Profile::new("depth");
        let x = self.patch.forward(&pixels)?.flatten_from(2)?.transpose(1, 2)?;
        let mut x = Tensor::cat(&[&self.cls, &x], 1)?.broadcast_add(&self.positions(gh, gw)?)?;
        stages.push(("emb", x.clone()));
        prof.mark("embeddings", &x);
        let mut feats = Vec::with_capacity(OUT_LAYERS.len());
        for (i, layer) in self.layers.iter().enumerate() {
            x = layer.forward(&x)?;
            if OUT_LAYERS.contains(&(i + 1)) {
                feats.push(self.norm.forward(&x)?);
            }
        }
        prof.mark("backbone", &x);
        if let (Some(f0), Some(f3)) = (feats.first(), feats.get(3)) {
            stages.push(("feat0", f0.clone()));
            stages.push(("feat3", f3.clone()));
        }
        // neck: reassemble each level to an image-like map, project to the fusion width
        let mut levels = Vec::with_capacity(feats.len());
        for ((f, (proj, resize)), conv) in feats.iter().zip(&self.reassemble).zip(&self.convs) {
            let tokens = proj.forward(&f.narrow(1, 1, gh * gw)?)?; // [1, N, c]
            let c = tokens.dim(2)?;
            let map = tokens.transpose(1, 2)?.reshape((1, c, gh, gw))?;
            let map = match resize {
                Resize::Up(wt, b) => dino::conv_transpose_patches(&map, wt, b, false)?,
                Resize::Same => map,
                Resize::Down(conv) => conv.forward(&map)?,
            };
            levels.push(conv.forward(&map)?);
        }
        if let Some(l) = levels.last() {
            prof.mark("reassemble", l);
        }
        // fusion, coarse to fine
        levels.reverse();
        let mut fused: Option<Tensor> = None;
        for (i, (level, layer)) in levels.iter().zip(&self.fusion).enumerate() {
            let size = match levels.get(i + 1) {
                Some(next) => {
                    let (_, _, nh, nw) = next.dims4()?;
                    Some((nh, nw))
                }
                None => None,
            };
            let out = match &fused {
                None => layer.forward(level, None, size)?,
                Some(prev) => layer.forward(prev, Some(level), size)?,
            };
            if i == 0 {
                stages.push(("fused0", out.clone()));
            }
            if i + 1 == levels.len() {
                stages.push(("fused3", out.clone()));
            }
            fused = Some(out);
        }
        let fused = fused.ok_or_else(|| Error::Message(format!("{WHAT}: no features")))?;
        prof.mark("fusion", &fused);
        let (c1, c2, c3) = &self.head;
        let d = c1.forward(&fused)?;
        prof.mark("head conv1", &d);
        let d = dino::upsample_bilinear(&d, gh * PATCH, gw * PATCH, true)?;
        prof.mark("head upsample", &d);
        let d = c2.forward(&d)?.relu()?;
        prof.mark("head conv2", &d);
        let d = c3.forward(&d)?.relu()?.squeeze(1)?;
        prof.mark("head conv3", &d);
        Ok((d, stages))
    }

    /// The model input for an RGB image (8-bit, row-major, `w × h`): `[3, ih, iw]` values and
    /// the input size `iw × ih`.
    pub fn preprocess(rgb: &[u8], w: usize, h: usize) -> Result<(Vec<f32>, usize, usize)> {
        let (iw, ih) = input_size(w, h);
        let small = resize_rgb(rgb, w, h, iw, ih, Filter::Bicubic)?;
        Ok((normalize_padded(&small, iw, ih, iw, ih)?, iw, ih))
    }

    /// Estimate the depth of an RGB image (8-bit, row-major, `w × h`).
    pub fn estimate(&self, rgb: &[u8], w: usize, h: usize) -> Result<DepthMap> {
        let mut prof = dino::Profile::new("depth");
        let (pixels, iw, ih) = Self::preprocess(rgb, w, h)?;
        let pixels = Tensor::from_vec(pixels, (1, 3, ih, iw), &self.device)?;
        prof.mark("preprocess", &pixels);
        let (d, _) = self.raw(&pixels)?;
        Ok(normalized(to_vec(&d)?, iw, ih))
    }
}

/// A depth map with `data` scaled to 0..1 (min → 0, max → 1; all zeros when flat).
pub fn normalized(mut data: Vec<f32>, width: usize, height: usize) -> DepthMap {
    let (lo, hi) = data.iter().filter(|v| v.is_finite()).fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let range = hi - lo;
    for v in &mut data {
        *v = if range > 0.0 && v.is_finite() { ((*v - lo) / range).clamp(0.0, 1.0) } else { 0.0 };
    }
    DepthMap { width, height, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_size_matches_the_dpt_processor() {
        // landscape and portrait 3:2 photos: short side 518, long side a multiple of 14
        assert_eq!(input_size(1600, 1067), (770, 518));
        assert_eq!(input_size(1067, 1600), (518, 770));
        assert_eq!(input_size(518, 518), (518, 518));
        // tiny images scale up; huge aspect ratios are capped
        assert_eq!(input_size(100, 100), (518, 518));
        let (w, h) = input_size(100_000, 10);
        assert!(w <= MAX_INPUT && h >= PATCH);
    }

    #[test]
    fn normalization_spans_zero_to_one() {
        let d = normalized(vec![2.0, 4.0, 3.0, f32::NAN], 2, 2);
        assert_eq!(d.data, vec![0.0, 1.0, 0.5, 0.0]);
        assert!(normalized(vec![1.0; 4], 2, 2).data.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn missing_dirs_are_errors() {
        let dir = std::env::temp_dir().join(format!("lc-depth-missing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(!is_depth_dir(&dir));
        let e = DepthModel::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("not found"), "{e}");
        std::fs::write(dir.join("model.safetensors"), [0u8; 4]).unwrap();
        std::fs::write(dir.join("config.json"), br#"{"model_type": "depth_anything", "backbone_config": {"hidden_size": 384, "num_attention_heads": 6, "patch_size": 14}, "fusion_hidden_size": 64}"#).unwrap();
        assert!(is_depth_dir(&dir));
        assert!(DepthModel::load_on(&dir, Device::Cpu).is_err()); // a truncated weights file
        let _ = std::fs::remove_dir_all(&dir);
    }
}
