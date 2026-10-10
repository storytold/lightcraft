//! Text prompts (SAM 3's concept detector): the prompt's CLIP features fused into the image
//! features by a DETR encoder, 200 object queries refined by a DETR decoder (with box
//! relative-position bias and a presence token), per-query masks from a pixel decoder.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the project contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/modeling_sam3.py`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{D, Device, IndexOp, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, GroupNorm, LayerNorm, Linear, VarBuilder};

use crate::clip::TextEncoder;
use crate::neck::{FPN, Neck};
use crate::nn::{Attention, DecoderMlp, Mlp, TORCH_LN_EPS, cxcywh_to_xyxy, inverse_sigmoid, linear, ln, padding_bias, sine_grid, to_vec};
use crate::{Error, Prediction, Result};

const HIDDEN: usize = FPN;
const HEADS: usize = 8;
const QUERIES: usize = 200;
const LAYERS: usize = 6;

/// Detector image features, computed once per image.
pub struct Features {
    /// Levels 0..=2 (288, 144, 72 per side).
    levels: Vec<Tensor>,
    /// Level 2 flattened `[1, 5184, 256]` and its sine position encoding.
    flat: Tensor,
    pos: Tensor,
    side: usize,
}

/// Everything the detector predicts for one prompt.
pub struct Output {
    /// Per-query mask logits `[200, 288, 288]` flattened.
    pub masks: Tensor,
    /// Per-query logits (before the presence score).
    pub logits: Vec<f32>,
    /// Whether the concept is in the image at all (logit).
    pub presence: f32,
    /// Boxes (x0, y0, x1, y1), normalized.
    pub boxes: Vec<[f32; 4]>,
    /// The text features `[1, 32, 256]` (tests).
    pub text: Tensor,
}

struct EncoderLayer {
    ln1: LayerNorm,
    self_attn: Attention,
    ln2: LayerNorm,
    cross_attn: Attention,
    ln3: LayerNorm,
    mlp: Mlp,
}

struct DecoderLayer {
    self_attn: Attention,
    self_ln: LayerNorm,
    text_attn: Attention,
    text_ln: LayerNorm,
    vision_attn: Attention,
    vision_ln: LayerNorm,
    mlp: Mlp,
    mlp_ln: LayerNorm,
}

pub struct Detector {
    device: Device,
    neck: Neck,
    text: TextEncoder,
    text_proj: Linear,
    encoder: Vec<EncoderLayer>,
    decoder: Vec<DecoderLayer>,
    out_ln: LayerNorm,
    box_head: DecoderMlp,
    query_embed: Tensor,
    reference_points: Tensor,
    presence_token: Tensor,
    presence_head: DecoderMlp,
    presence_ln: LayerNorm,
    ref_point_head: DecoderMlp,
    rpb_x: DecoderMlp,
    rpb_y: DecoderMlp,
    // dot-product scoring
    score_text_mlp: DecoderMlp,
    score_text_ln: LayerNorm,
    score_text_proj: Linear,
    score_query_proj: Linear,
    // mask decoder
    prompt_attn: Attention,
    prompt_ln: LayerNorm,
    pixel_convs: Vec<Conv2d>,
    pixel_norms: Vec<GroupNorm>,
    instance_proj: Conv2d,
    mask_embedder: Vec<Linear>,
}

impl Detector {
    /// `vb` at `detector_model`.
    pub fn new(vb: VarBuilder) -> Result<Self> {
        let e = vb.pp("detr_encoder");
        let mut encoder = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = e.pp(format!("layers.{i}"));
            encoder.push(EncoderLayer {
                ln1: ln(HIDDEN, TORCH_LN_EPS, l.pp("layer_norm1"))?,
                self_attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("self_attn"))?,
                ln2: ln(HIDDEN, TORCH_LN_EPS, l.pp("layer_norm2"))?,
                cross_attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("cross_attn"))?,
                ln3: ln(HIDDEN, TORCH_LN_EPS, l.pp("layer_norm3"))?,
                mlp: Mlp::new(HIDDEN, 2048, false, l.pp("mlp"))?,
            });
        }
        let d = vb.pp("detr_decoder");
        let mut decoder = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = d.pp(format!("layers.{i}"));
            decoder.push(DecoderLayer {
                self_attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("self_attn"))?,
                self_ln: ln(HIDDEN, TORCH_LN_EPS, l.pp("self_attn_layer_norm"))?,
                text_attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("text_cross_attn"))?,
                text_ln: ln(HIDDEN, TORCH_LN_EPS, l.pp("text_cross_attn_layer_norm"))?,
                vision_attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("vision_cross_attn"))?,
                vision_ln: ln(HIDDEN, TORCH_LN_EPS, l.pp("vision_cross_attn_layer_norm"))?,
                mlp: Mlp::new(HIDDEN, 2048, false, l.pp("mlp"))?,
                mlp_ln: ln(HIDDEN, TORCH_LN_EPS, l.pp("mlp_layer_norm"))?,
            });
        }
        let s = vb.pp("dot_product_scoring");
        let m = vb.pp("mask_decoder");
        let p = m.pp("pixel_decoder");
        let conv3 = Conv2dConfig { padding: 1, ..Default::default() };
        Ok(Self {
            device: vb.device().clone(),
            neck: Neck::new(vb.pp("vision_encoder.neck"))?,
            text: TextEncoder::new(vb.pp("text_encoder.text_model"))?,
            text_proj: linear(1024, HIDDEN, vb.pp("text_projection"))?,
            encoder,
            decoder,
            out_ln: ln(HIDDEN, TORCH_LN_EPS, d.pp("output_layer_norm"))?,
            box_head: DecoderMlp::new(HIDDEN, HIDDEN, 4, 3, d.pp("box_head"))?,
            query_embed: d.get((QUERIES, HIDDEN), "query_embed.weight")?.unsqueeze(0)?,
            reference_points: d.get((QUERIES, 4), "reference_points.weight")?.unsqueeze(0)?,
            presence_token: d.get((1, HIDDEN), "presence_token.weight")?.unsqueeze(0)?,
            presence_head: DecoderMlp::new(HIDDEN, HIDDEN, 1, 3, d.pp("presence_head"))?,
            presence_ln: ln(HIDDEN, TORCH_LN_EPS, d.pp("presence_layer_norm"))?,
            ref_point_head: DecoderMlp::new(2 * HIDDEN, HIDDEN, HIDDEN, 2, d.pp("ref_point_head"))?,
            rpb_x: DecoderMlp::new(2, HIDDEN, HEADS, 2, d.pp("box_rpb_embed_x"))?,
            rpb_y: DecoderMlp::new(2, HIDDEN, HEADS, 2, d.pp("box_rpb_embed_y"))?,
            score_text_mlp: DecoderMlp::new(HIDDEN, 2048, HIDDEN, 2, s.pp("text_mlp"))?,
            score_text_ln: ln(HIDDEN, TORCH_LN_EPS, s.pp("text_mlp_out_norm"))?,
            score_text_proj: linear(HIDDEN, HIDDEN, s.pp("text_proj"))?,
            score_query_proj: linear(HIDDEN, HIDDEN, s.pp("query_proj"))?,
            prompt_attn: Attention::new(HIDDEN, HIDDEN, HEADS, m.pp("prompt_cross_attn"))?,
            prompt_ln: ln(HIDDEN, TORCH_LN_EPS, m.pp("prompt_cross_attn_norm"))?,
            pixel_convs: (0..3).map(|i| Ok(candle_nn::conv2d(HIDDEN, HIDDEN, 3, conv3, p.pp(format!("conv_layers.{i}")))?)).collect::<Result<_>>()?,
            pixel_norms: (0..3).map(|i| Ok(candle_nn::group_norm(8, HIDDEN, 1e-5, p.pp(format!("norms.{i}")))?)).collect::<Result<_>>()?,
            instance_proj: candle_nn::conv2d(HIDDEN, HIDDEN, 1, Conv2dConfig::default(), m.pp("instance_projection"))?,
            mask_embedder: (0..3).map(|i| linear(HIDDEN, HIDDEN, m.pp(format!("mask_embedder.layers.{i}")))).collect::<Result<_>>()?,
        })
    }

    /// The detector's feature pyramid for the backbone output `[1, 1024, 72, 72]`.
    pub fn features(&self, backbone: &Tensor) -> Result<Features> {
        let levels = self.neck.forward(backbone, 3)?;
        let last = levels.last().ok_or_else(|| Error::Model("detector neck".into()))?;
        let (_, _, side, _) = last.dims4()?;
        let flat = last.flatten_from(2)?.transpose(1, 2)?.contiguous()?;
        let pos = sine_grid(side, side, HIDDEN / 2, &self.device)?.flatten_from(2)?.transpose(1, 2)?.contiguous()?;
        Ok(Features { levels, flat, pos, side })
    }

    /// Box relative-position bias `[1, heads, 1 + 200, side²]` for reference boxes (cx, cy, w, h).
    fn rpb(&self, boxes: &Tensor, side: usize) -> Result<Tensor> {
        let xyxy = cxcywh_to_xyxy(boxes)?.squeeze(0)?; // [200, 4]
        let coords = Tensor::arange(0u32, side as u32, &self.device)?.to_dtype(candle_core::DType::F32)?.affine(1.0 / side as f64, 0.0)?;
        let coords = coords.reshape((1, side, 1))?;
        let log_scale = |d: Tensor| -> Result<Tensor> {
            let d = (d * 8.0)?;
            Ok(d.sign()?.mul(&((d.abs()? + 1.0)?.log()? / (8f64.ln()))?)?)
        };
        let pick = |a: usize, b: usize| -> Result<Tensor> { Ok(Tensor::cat(&[xyxy.narrow(1, a, 1)?, xyxy.narrow(1, b, 1)?], 1)?.unsqueeze(1)?) };
        let dy = log_scale(coords.broadcast_sub(&pick(1, 3)?)?)?; // [200, side, 2]
        let dx = log_scale(coords.broadcast_sub(&pick(0, 2)?)?)?;
        let ey = self.rpb_y.forward(&dy)?; // [200, side, heads]
        let ex = self.rpb_x.forward(&dx)?;
        let m = ey.unsqueeze(2)?.broadcast_add(&ex.unsqueeze(1)?)?; // [200, side, side, heads]
        let m = m.reshape((QUERIES, side * side, HEADS))?.permute((2, 0, 1))?; // [heads, 200, side²]
        let m = m.pad_with_zeros(1, 1, 0)?; // the presence token attends everywhere
        Ok(m.unsqueeze(0)?.contiguous()?)
    }

    /// Sine embedding of boxes (cx, cy, w, h) `[1, 200, 512]` (y, x, w, h features).
    fn box_sine(&self, boxes: &Tensor) -> Result<Tensor> {
        let feats = HIDDEN / 2;
        let dim_t: Vec<f32> = (0..feats).map(|i| 10000f64.powf(2.0 * (i / 2) as f64 / feats as f64) as f32).collect();
        let dim_t = Tensor::from_vec(dim_t, (1, 1, feats), &self.device)?;
        let even: Vec<f32> = (0..feats).map(|i| if i % 2 == 0 { 1.0 } else { 0.0 }).collect();
        let even = Tensor::from_vec(even, (1, 1, feats), &self.device)?;
        let one = |i: usize| -> Result<Tensor> {
            let p = (boxes.narrow(D::Minus1, i, 1)? * (2.0 * std::f64::consts::PI))?.broadcast_div(&dim_t)?;
            Ok((p.sin()?.broadcast_mul(&even)? + p.cos()?.broadcast_mul(&(1.0 - &even)?)?)?)
        };
        Ok(Tensor::cat(&[one(1)?, one(0)?, one(2)?, one(3)?], D::Minus1)?)
    }

    /// Run the detector for one tokenized prompt.
    pub fn predict(&self, f: &Features, ids: &[u32], valid: &[bool]) -> Result<Output> {
        let text = self.text_proj.forward(&self.text.forward(ids, valid)?)?; // [1, 32, 256]
        let text_bias = padding_bias(valid, &self.device)?;

        // DETR encoder: image tokens attend to themselves and to the prompt
        let mut x = f.flat.clone();
        for l in &self.encoder {
            let h = l.ln1.forward(&x)?;
            let hp = (&h + &f.pos)?;
            x = (&x + l.self_attn.forward(&hp, &hp, &h, None)?)?;
            let h = l.ln2.forward(&x)?;
            x = (&x + l.cross_attn.forward(&h, &text, &text, Some(&text_bias))?)?;
            let h = l.ln3.forward(&x)?;
            x = (&x + l.mlp.forward(&h)?)?;
        }
        let memory = x;

        // DETR decoder: presence token + 200 queries with iterative box refinement
        let mut boxes = candle_nn::ops::sigmoid(&self.reference_points)?;
        let mut h = Tensor::cat(&[self.presence_token.clone(), self.query_embed.clone()], 1)?;
        let key_pos = (&memory + &f.pos)?;
        let mut queries = None;
        let mut presence = None;
        for l in &self.decoder {
            let qpos = self.ref_point_head.forward(&self.box_sine(&boxes)?)?.pad_with_zeros(1, 1, 0)?;
            let bias = self.rpb(&boxes, f.side)?;
            let q = (&h + &qpos)?;
            h = l.self_ln.forward(&(&h + l.self_attn.forward(&q, &q, &h, None)?)?)?;
            let q = (&h + &qpos)?;
            h = l.text_ln.forward(&(&h + l.text_attn.forward(&q, &text, &text, Some(&text_bias))?)?)?;
            let q = (&h + &qpos)?;
            h = l.vision_ln.forward(&(&h + l.vision_attn.forward(&q, &key_pos, &memory, Some(&bias))?)?)?;
            h = l.mlp_ln.forward(&(&h + l.mlp.forward(&h)?)?)?;
            let qh = self.out_ln.forward(&h.narrow(1, 1, QUERIES)?)?;
            boxes = candle_nn::ops::sigmoid(&(self.box_head.forward(&qh)? + inverse_sigmoid(&boxes)?)?)?;
            let p = self.presence_head.forward(&self.presence_ln.forward(&h.narrow(1, 0, 1)?)?)?;
            presence = Some(p.clamp(-10f32, 10f32)?);
            queries = Some(qh);
        }
        let (Some(queries), Some(presence)) = (queries, presence) else { return Err(Error::Model("decoder has no layers".into())) };

        // dot-product scoring against the mean of the (refined) prompt tokens
        let t = self.score_text_ln.forward(&(self.score_text_mlp.forward(&text)? + &text)?)?;
        let w: Vec<f32> = valid.iter().map(|v| if *v { 1.0 } else { 0.0 }).collect();
        let count = w.iter().sum::<f32>().max(1.0);
        let w = Tensor::from_vec(w, (1, valid.len(), 1), &self.device)?;
        let pooled = (t.broadcast_mul(&w)?.sum(1)? / f64::from(count))?; // [1, 256]
        let pt = self.score_text_proj.forward(&pooled)?; // [1, 256]
        let pq = self.score_query_proj.forward(&queries)?; // [1, 200, 256]
        let logits = (pq.matmul(&pt.unsqueeze(2)?)? / (HIDDEN as f64).sqrt())?.clamp(-12f32, 12f32)?;

        // masks: the encoder memory attends to the prompt, then a pixel decoder up to 288²
        let mem = (&memory + self.prompt_attn.forward(&self.prompt_ln.forward(&memory)?, &text, &text, Some(&text_bias))?)?;
        let [l0, l1, _] = &f.levels[..] else { return Err(Error::Model("detector levels".into())) };
        let mut prev = mem.transpose(1, 2)?.reshape((1, HIDDEN, f.side, f.side))?;
        for (i, skip) in [l1, l0].into_iter().enumerate() {
            let (_, _, sh, sw) = skip.dims4()?;
            prev = (prev.upsample_nearest2d(sh, sw)? + skip)?;
            let (Some(conv), Some(norm)) = (self.pixel_convs.get(i), self.pixel_norms.get(i)) else {
                return Err(Error::Model("pixel decoder".into()));
            };
            prev = norm.forward(&conv.forward(&prev)?)?.relu()?;
        }
        let inst = self.instance_proj.forward(&prev)?; // [1, 256, 288, 288]
        let (_, c, mh, mw) = inst.dims4()?;
        let mut e = queries.clone();
        for (i, l) in self.mask_embedder.iter().enumerate() {
            e = l.forward(&e)?;
            if i + 1 < self.mask_embedder.len() {
                e = e.relu()?;
            }
        }
        let masks = e.matmul(&inst.reshape((1, c, mh * mw))?)?.reshape((QUERIES, mh, mw))?;
        let boxes = cxcywh_to_xyxy(&boxes)?;
        let bv = to_vec(&boxes)?;
        Ok(Output {
            masks,
            logits: to_vec(&logits)?,
            presence: to_vec(&presence)?.first().copied().unwrap_or(-10.0),
            boxes: bv.as_chunks::<4>().0.to_vec(),
            text,
        })
    }
}

/// The instances of `out` scoring above `threshold` (sigmoid(logit) × sigmoid(presence)),
/// merged into one mask (the per-pixel maximum of their probabilities, as logits).
pub fn merge(out: &Output, threshold: f32) -> Option<Prediction> {
    let s = |x: f32| 1.0 / (1.0 + (-x).exp());
    let presence = s(out.presence);
    let keep: Vec<(usize, f32)> = out.logits.iter().enumerate().map(|(i, l)| (i, s(*l) * presence)).filter(|(_, p)| *p > threshold).collect();
    let best = keep.iter().map(|k| k.1).fold(0f32, f32::max);
    let mut merged: Option<Vec<f32>> = None;
    for (i, _) in &keep {
        let m = out.masks.i(*i).ok().and_then(|t| to_vec(&t).ok())?;
        merged = Some(match merged {
            None => m,
            Some(acc) => acc.iter().zip(&m).map(|(a, b)| a.max(*b)).collect(),
        });
    }
    merged.map(|logits| Prediction { logits, score: best })
}
