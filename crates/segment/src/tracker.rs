//! Point prompts (SAM 3's interactive "tracker" head, SAM 2 style): a prompt encoder for
//! positive/negative clicks and a two-way-transformer mask decoder over the image features.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3_tracker/modeling_sam3_tracker.py`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, ConvTranspose2d, ConvTranspose2dConfig, LayerNorm, VarBuilder};

use crate::nn::{Attention, FeedForward, TORCH_LN_EPS, ln, ln_channels};
use crate::vit::{GRID, IMAGE};
use crate::{Result, nn::to_vec};

const HIDDEN: usize = 256;
const MASK_TOKENS: usize = 4;
/// Low-resolution mask side (4 × the 72 grid).
pub const LOW_RES: usize = 288;

/// The image features the decoder needs, computed once per image.
pub struct Features {
    /// Level 2 (72 × 72, 256 channels).
    pub image: Tensor,
    /// `conv_s0`(level 0): `[1, 32, 288, 288]`.
    pub s0: Tensor,
    /// `conv_s1`(level 1): `[1, 64, 144, 144]`.
    pub s1: Tensor,
}

/// One click: position normalized to the image (0..1), and whether it includes or excludes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub x: f32,
    pub y: f32,
    pub positive: bool,
}

/// A predicted mask: 288 × 288 logits over the (squared) model input, and its quality.
pub struct Prediction {
    pub logits: Vec<f32>,
    pub iou: f32,
    pub object_score: f32,
}

/// Random-Fourier positional encoding of points normalized to 0..1 (`Sam3TrackerPositionalEmbedding`).
struct Fourier {
    pe: Vec<f32>, // [2, 128]
}

impl Fourier {
    fn encode(&self, x: f32, y: f32) -> Vec<f32> {
        let half = self.pe.len() / 2;
        let (cx, cy) = (2.0 * x - 1.0, 2.0 * y - 1.0);
        let proj: Vec<f32> = (0..half)
            .map(|i| {
                let a = cx * self.pe.get(i).copied().unwrap_or(0.0) + cy * self.pe.get(half + i).copied().unwrap_or(0.0);
                2.0 * std::f32::consts::PI * a
            })
            .collect();
        proj.iter().map(|a| a.sin()).chain(proj.iter().map(|a| a.cos())).collect()
    }
}

struct TwoWayBlock {
    self_attn: Attention,
    ln1: LayerNorm,
    t2i: Attention,
    ln2: LayerNorm,
    mlp: FeedForward,
    ln3: LayerNorm,
    ln4: LayerNorm,
    i2t: Attention,
    skip_first_pe: bool,
}

/// `Sam3TrackerAttention` on `[1, L, C]` (the point batch of one is folded away).
fn attn(a: &Attention, q: &Tensor, k: &Tensor, v: &Tensor) -> Result<Tensor> {
    a.forward(q, k, v, None)
}

impl TwoWayBlock {
    fn forward(&self, queries: &Tensor, keys: &Tensor, qpe: &Tensor, kpe: &Tensor) -> Result<(Tensor, Tensor)> {
        let queries = if self.skip_first_pe {
            attn(&self.self_attn, queries, queries, queries)?
        } else {
            let q = (queries + qpe)?;
            (queries + attn(&self.self_attn, &q, &q, queries)?)?
        };
        let queries = self.ln1.forward(&queries)?;
        let (q, k) = ((&queries + qpe)?, (keys + kpe)?);
        let queries = self.ln2.forward(&(&queries + attn(&self.t2i, &q, &k, keys)?)?)?;
        let queries = self.ln3.forward(&(&queries + self.mlp.forward(&queries)?)?)?;
        let (q, k) = ((&queries + qpe)?, (keys + kpe)?);
        let keys = self.ln4.forward(&(keys + attn(&self.i2t, &k, &q, &queries)?)?)?;
        Ok((queries, keys))
    }
}

pub struct Tracker {
    device: Device,
    point_pe: Fourier,
    image_pe: Tensor,
    point_embed: Tensor,
    not_a_point: Tensor,
    no_mask: Tensor,
    conv_s0: Conv2d,
    conv_s1: Conv2d,
    output_tokens: Tensor,
    blocks: Vec<TwoWayBlock>,
    final_t2i: Attention,
    ln_final: LayerNorm,
    up1: ConvTranspose2d,
    up_ln: LayerNorm,
    up2: ConvTranspose2d,
    hyper: Vec<FeedForward>,
    iou_head: FeedForward,
    obj_head: FeedForward,
}

fn read_vec(vb: &VarBuilder, shape: (usize, usize), name: &str) -> Result<Vec<f32>> {
    to_vec(&vb.get(shape, name)?)
}

impl Tracker {
    /// `vb` at `tracker_model`.
    pub fn new(vb: VarBuilder) -> Result<Self> {
        let device = vb.device().clone();
        let pe = vb.pp("prompt_encoder");
        let md = vb.pp("mask_decoder");
        let image_fourier = Fourier { pe: read_vec(&vb, (2, HIDDEN / 2), "shared_image_embedding.positional_embedding")? };
        // image-wide positional embedding: pixel centres of the 72 × 72 grid
        let mut grid = Vec::with_capacity(GRID * GRID * HIDDEN);
        for r in 0..GRID {
            for c in 0..GRID {
                grid.extend(image_fourier.encode((c as f32 + 0.5) / GRID as f32, (r as f32 + 0.5) / GRID as f32));
            }
        }
        let image_pe = Tensor::from_vec(grid, (1, GRID * GRID, HIDDEN), &device)?;
        let t = md.pp("transformer");
        let mut blocks = Vec::new();
        for i in 0..2 {
            let b = t.pp(format!("layers.{i}"));
            blocks.push(TwoWayBlock {
                self_attn: Attention::new(HIDDEN, HIDDEN, 8, b.pp("self_attn"))?,
                ln1: ln(HIDDEN, TORCH_LN_EPS, b.pp("layer_norm1"))?,
                t2i: Attention::new(HIDDEN, HIDDEN / 2, 8, b.pp("cross_attn_token_to_image"))?,
                ln2: ln(HIDDEN, TORCH_LN_EPS, b.pp("layer_norm2"))?,
                mlp: FeedForward::new(HIDDEN, 2048, HIDDEN, 2, false, b.pp("mlp"))?,
                ln3: ln(HIDDEN, TORCH_LN_EPS, b.pp("layer_norm3"))?,
                ln4: ln(HIDDEN, TORCH_LN_EPS, b.pp("layer_norm4"))?,
                i2t: Attention::new(HIDDEN, HIDDEN / 2, 8, b.pp("cross_attn_image_to_token"))?,
                skip_first_pe: i == 0,
            });
        }
        let output_tokens = Tensor::cat(
            &[
                md.get((1, HIDDEN), "obj_score_token.weight")?,
                md.get((1, HIDDEN), "iou_token.weight")?,
                md.get((MASK_TOKENS, HIDDEN), "mask_tokens.weight")?,
            ],
            0,
        )?;
        let up = ConvTranspose2dConfig { stride: 2, ..Default::default() };
        let one = Conv2dConfig::default();
        Ok(Self {
            point_pe: Fourier { pe: read_vec(&pe, (2, HIDDEN / 2), "shared_embedding.positional_embedding")? },
            image_pe,
            point_embed: pe.get((4, HIDDEN), "point_embed.weight")?.to_device(&Device::Cpu)?,
            not_a_point: pe.get((1, HIDDEN), "not_a_point_embed.weight")?.to_device(&Device::Cpu)?,
            no_mask: pe.get((1, HIDDEN), "no_mask_embed.weight")?.reshape((1, HIDDEN, 1, 1))?,
            conv_s0: candle_nn::conv2d(HIDDEN, HIDDEN / 8, 1, one, md.pp("conv_s0"))?,
            conv_s1: candle_nn::conv2d(HIDDEN, HIDDEN / 4, 1, one, md.pp("conv_s1"))?,
            output_tokens,
            blocks,
            final_t2i: Attention::new(HIDDEN, HIDDEN / 2, 8, t.pp("final_attn_token_to_image"))?,
            ln_final: ln(HIDDEN, TORCH_LN_EPS, t.pp("layer_norm_final_attn"))?,
            up1: candle_nn::conv_transpose2d(HIDDEN, HIDDEN / 4, 2, up, md.pp("upscale_conv1"))?,
            up_ln: ln(HIDDEN / 4, 1e-6, md.pp("upscale_layer_norm"))?,
            up2: candle_nn::conv_transpose2d(HIDDEN / 4, HIDDEN / 8, 2, up, md.pp("upscale_conv2"))?,
            hyper: (0..MASK_TOKENS)
                .map(|i| FeedForward::new(HIDDEN, HIDDEN, HIDDEN / 8, 3, false, md.pp(format!("output_hypernetworks_mlps.{i}"))))
                .collect::<Result<_>>()?,
            iou_head: FeedForward::new(HIDDEN, HIDDEN, MASK_TOKENS, 3, true, md.pp("iou_prediction_head"))?,
            obj_head: FeedForward::new(HIDDEN, HIDDEN, 1, 3, false, md.pp("pred_obj_score_head"))?,
            device,
        })
    }

    /// The decoder's image features from the tracker neck's levels (288, 144, 72 per side).
    pub fn features(&self, levels: &[Tensor]) -> Result<Features> {
        let [l0, l1, l2, ..] = levels else { return Err(crate::Error::Model("tracker needs three feature levels".into())) };
        Ok(Features { image: l2.clone(), s0: self.conv_s0.forward(l0)?, s1: self.conv_s1.forward(l1)? })
    }

    /// Sparse prompt embeddings `[1, n + 1, 256]` (a padding point closes the list).
    fn embed_clicks(&self, clicks: &[Click]) -> Result<Tensor> {
        let point_embed = to_vec(&self.point_embed)?;
        let not_a_point = to_vec(&self.not_a_point)?;
        let mut rows = Vec::with_capacity((clicks.len() + 1) * HIDDEN);
        for c in clicks {
            // the reference shifts pixel coordinates (in the 1008² input) by half a pixel
            let (x, y) = ((c.x * IMAGE as f32 + 0.5) / IMAGE as f32, (c.y * IMAGE as f32 + 0.5) / IMAGE as f32);
            let e = self.point_pe.encode(x, y);
            let label = usize::from(c.positive);
            rows.extend(e.iter().enumerate().map(|(i, v)| v + point_embed.get(label * HIDDEN + i).copied().unwrap_or(0.0)));
        }
        rows.extend_from_slice(&not_a_point);
        Ok(Tensor::from_vec(rows, (1, clicks.len() + 1, HIDDEN), &self.device)?)
    }

    /// Predict masks for `clicks`. `multimask`: the three alternatives (best by predicted IoU is
    /// returned), else the single-mask output with SAM 2's stability fallback.
    pub fn predict(&self, f: &Features, clicks: &[Click], multimask: bool) -> Result<Prediction> {
        let sparse = self.embed_clicks(clicks)?;
        let tokens = Tensor::cat(&[self.output_tokens.unsqueeze(0)?, sparse], 1)?; // [1, T, 256]
        let image = f.image.broadcast_add(&self.no_mask)?; // dense prompt: "no mask"
        let (b, c, h, w) = image.dims4()?;
        let keys = image.flatten_from(2)?.transpose(1, 2)?.contiguous()?; // [1, HW, 256]
        let (mut q, mut k) = (tokens.clone(), keys);
        for blk in &self.blocks {
            (q, k) = blk.forward(&q, &k, &tokens, &self.image_pe)?;
        }
        let qf = (&q + &tokens)?;
        let kf = (&k + &self.image_pe)?;
        let q = self.ln_final.forward(&(&q + attn(&self.final_t2i, &qf, &kf, &k)?)?)?;
        let iou_token = q.i((.., 1, ..))?;
        let image = k.transpose(1, 2)?.reshape((b, c, h, w))?;
        let up = (self.up1.forward(&image)? + &f.s1)?;
        let up = ln_channels(&up, &self.up_ln)?.gelu_erf()?;
        let up = (self.up2.forward(&up)? + &f.s0)?.gelu_erf()?; // [1, 32, 288, 288]
        let (_, uc, uh, uw) = up.dims4()?;
        let hyper = (0..MASK_TOKENS)
            .map(|i| self.hyper.get(i).map_or(Err(crate::Error::Model("hypernetwork".into())), |m| m.forward(&q.i((.., 2 + i, ..))?)))
            .collect::<Result<Vec<_>>>()?;
        let hyper = Tensor::stack(&hyper, 1)?; // [1, 4, 32]
        let masks = hyper.matmul(&up.reshape((b, uc, uh * uw))?)?.reshape((b, MASK_TOKENS, uh, uw))?;
        let iou = to_vec(&self.iou_head.forward(&iou_token)?)?;
        let object_score = to_vec(&self.obj_head.forward(&q.i((.., 0, ..))?)?)?.first().copied().unwrap_or(0.0);
        let pick = |i: usize| -> Result<Prediction> {
            Ok(Prediction { logits: to_vec(&masks.i((0, i))?)?, iou: iou.get(i).copied().unwrap_or(0.0), object_score })
        };
        let best_multi = || (1..MASK_TOKENS).max_by(|a, b| iou.get(*a).partial_cmp(&iou.get(*b)).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(1);
        if multimask {
            return pick(best_multi());
        }
        // single-mask output unless it is unstable (area changes > 2 % when the threshold moves ±0.05)
        let single = masks.i((0, 0))?;
        let inter = single.gt(0.05)?.to_dtype(DType::F32)?.sum_all()?.to_scalar::<f32>()?;
        let union = single.gt(-0.05)?.to_dtype(DType::F32)?.sum_all()?.to_scalar::<f32>()?;
        let stability = if union > 0.0 { inter / union } else { 1.0 };
        if stability >= 0.98 { pick(0) } else { pick(best_multi()) }
    }
}
