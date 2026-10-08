//! Shared building blocks: attention, MLPs, layer norms, sine position encodings.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/modeling_sam3.py`, `models/sam3_tracker`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use crate::Result;

/// `nn.LayerNorm(d)` (PyTorch's default eps).
pub const TORCH_LN_EPS: f64 = 1e-5;

pub fn ln(d: usize, eps: f64, vb: VarBuilder) -> Result<LayerNorm> {
    Ok(candle_nn::layer_norm(d, eps, vb)?)
}

pub fn linear(i: usize, o: usize, vb: VarBuilder) -> Result<Linear> {
    Ok(candle_nn::linear(i, o, vb)?)
}

/// Largest attention-score block computed at once (elements): the 5184-token global layers
/// would otherwise need ~1.7 GB for all 16 heads.
const SCORE_BLOCK: usize = 64 << 20;

/// Scaled dot-product attention on `[B, H, L, d]` tensors, with an optional additive bias
/// broadcastable to `[B, H, Lq, Lk]`. Heads are processed in groups to bound memory.
pub fn sdpa(q: &Tensor, k: &Tensor, v: &Tensor, bias: Option<&Tensor>, scale: f64) -> Result<Tensor> {
    let (b, h, lq, _) = q.dims4()?;
    let lk = k.dim(2)?;
    let per_head = b.saturating_mul(lq).saturating_mul(lk).max(1);
    let group = (SCORE_BLOCK / per_head).clamp(1, h);
    let bias_heads = match bias {
        Some(t) => t.dim(1)?,
        None => 1,
    };
    let mut outs = Vec::with_capacity(h.div_ceil(group));
    let mut start = 0;
    while start < h {
        let n = group.min(h - start);
        let qs = q.narrow(1, start, n)?;
        let ks = k.narrow(1, start, n)?;
        let vs = v.narrow(1, start, n)?;
        let mut att = (qs.matmul(&ks.t()?)? * scale)?;
        if let Some(bias) = bias {
            let bs = if bias_heads > 1 { bias.narrow(1, start, n)? } else { bias.clone() };
            att = att.broadcast_add(&bs)?;
        }
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        outs.push(att.matmul(&vs)?);
        start += n;
    }
    Ok(Tensor::cat(&outs, 1)?)
}

/// Multi-head attention with q/k/v/o projections (`Sam3Attention`, `Sam3TrackerAttention`
/// with `internal` = hidden / downsample rate).
pub struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
    head_dim: usize,
}

impl Attention {
    pub fn new(hidden: usize, internal: usize, heads: usize, vb: VarBuilder) -> Result<Self> {
        Self::named(hidden, internal, heads, "o_proj", vb)
    }

    /// With the output projection called `out` (CLIP's `out_proj`).
    pub fn named(hidden: usize, internal: usize, heads: usize, out: &str, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            q: linear(hidden, internal, vb.pp("q_proj"))?,
            k: linear(hidden, internal, vb.pp("k_proj"))?,
            v: linear(hidden, internal, vb.pp("v_proj"))?,
            o: linear(internal, hidden, vb.pp(out))?,
            heads,
            head_dim: internal / heads,
        })
    }

    fn split(&self, x: &Tensor) -> Result<Tensor> {
        let (b, l, _) = x.dims3()?;
        Ok(x.reshape((b, l, self.heads, self.head_dim))?.transpose(1, 2)?.contiguous()?)
    }

    /// The q, k and v projections of `x` (for attention variants that transform them).
    pub fn qkv(&self, x: &Tensor) -> Result<(Tensor, Tensor, Tensor)> {
        Ok((self.q.forward(x)?, self.k.forward(x)?, self.v.forward(x)?))
    }

    /// The output projection.
    pub fn out(&self, x: &Tensor) -> Result<Tensor> {
        Ok(self.o.forward(x)?)
    }

    /// `q`, `k`, `v`: `[B, L, hidden]`; `bias` broadcastable to `[B, heads, Lq, Lk]`.
    pub fn forward(&self, q: &Tensor, k: &Tensor, v: &Tensor, bias: Option<&Tensor>) -> Result<Tensor> {
        let (b, lq, _) = q.dims3()?;
        let q = self.split(&self.q.forward(q)?)?;
        let k = self.split(&self.k.forward(k)?)?;
        let v = self.split(&self.v.forward(v)?)?;
        let o = sdpa(&q, &k, &v, bias, (self.head_dim as f64).powf(-0.5))?;
        let o = o.transpose(1, 2)?.reshape((b, lq, self.heads * self.head_dim))?;
        Ok(self.o.forward(&o)?)
    }
}

/// An additive key-padding bias `[1, 1, 1, L]` (0 for valid tokens, the f32 minimum for padding).
pub fn padding_bias(valid: &[bool], device: &Device) -> Result<Tensor> {
    let v: Vec<f32> = valid.iter().map(|&ok| if ok { 0.0 } else { f32::MIN }).collect();
    Ok(Tensor::from_vec(v, (1, 1, 1, valid.len()), device)?)
}

/// `Sam3MLP`: fc1 → activation → fc2.
pub struct Mlp {
    fc1: Linear,
    fc2: Linear,
    gelu: bool,
}

impl Mlp {
    pub fn new(hidden: usize, inter: usize, gelu: bool, vb: VarBuilder) -> Result<Self> {
        Ok(Self { fc1: linear(hidden, inter, vb.pp("fc1"))?, fc2: linear(inter, hidden, vb.pp("fc2"))?, gelu })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.fc1.forward(x)?;
        let x = if self.gelu { x.gelu_erf()? } else { x.relu()? };
        Ok(self.fc2.forward(&x)?)
    }
}

/// `Sam3DecoderMLP`: `layer1 … layerN` with ReLU between (2 or 3 layers).
pub struct DecoderMlp {
    layers: Vec<Linear>,
}

impl DecoderMlp {
    pub fn new(input: usize, hidden: usize, output: usize, n: usize, vb: VarBuilder) -> Result<Self> {
        let mut layers = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (if i == 0 { input } else { hidden }, if i + 1 == n { output } else { hidden });
            layers.push(linear(a, b, vb.pp(format!("layer{}", i + 1)))?);
        }
        Ok(Self { layers })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mut x = x.clone();
        for (i, l) in self.layers.iter().enumerate() {
            x = l.forward(&x)?;
            if i + 1 < self.layers.len() {
                x = x.relu()?;
            }
        }
        Ok(x)
    }
}

/// `Sam3TrackerFeedForward`: proj_in → act → (layers.i → act)* → proj_out (→ sigmoid).
pub struct FeedForward {
    proj_in: Linear,
    layers: Vec<Linear>,
    proj_out: Linear,
    sigmoid: bool,
}

impl FeedForward {
    pub fn new(input: usize, hidden: usize, output: usize, n: usize, sigmoid: bool, vb: VarBuilder) -> Result<Self> {
        let mut layers = Vec::new();
        for i in 0..n.saturating_sub(2) {
            layers.push(linear(hidden, hidden, vb.pp(format!("layers.{i}")))?);
        }
        Ok(Self { proj_in: linear(input, hidden, vb.pp("proj_in"))?, layers, proj_out: linear(hidden, output, vb.pp("proj_out"))?, sigmoid })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mut x = self.proj_in.forward(x)?.relu()?;
        for l in &self.layers {
            x = l.forward(&x)?.relu()?;
        }
        let x = self.proj_out.forward(&x)?;
        Ok(if self.sigmoid { candle_nn::ops::sigmoid(&x)? } else { x })
    }
}

/// Layer norm over the channel axis of an NCHW tensor (`Sam3TrackerLayerNorm`, channels_first).
pub fn ln_channels(x: &Tensor, norm: &LayerNorm) -> Result<Tensor> {
    let x = x.permute((0, 2, 3, 1))?;
    Ok(norm.forward(&x)?.permute((0, 3, 1, 2))?.contiguous()?)
}

/// The DETR sine embedding of an `h × w` grid, normalized (`Sam3SinePositionEmbedding` with
/// `normalize=True`, scale 2π): `[1, 2·feats, h, w]`, y features first.
pub fn sine_grid(h: usize, w: usize, feats: usize, device: &Device) -> Result<Tensor> {
    let scale = 2.0 * std::f64::consts::PI;
    let dim_t: Vec<f64> = (0..feats).map(|i| 10000f64.powf(2.0 * (i / 2) as f64 / feats as f64)).collect();
    let mut out = vec![0f32; 2 * feats * h * w];
    for y in 0..h {
        let ye = (y + 1) as f64 / (h as f64 + 1e-6) * scale;
        for x in 0..w {
            let xe = (x + 1) as f64 / (w as f64 + 1e-6) * scale;
            for (i, d) in dim_t.iter().enumerate() {
                let (py, px) = (ye / d, xe / d);
                let (vy, vx) = if i % 2 == 0 { (py.sin(), px.sin()) } else { (py.cos(), px.cos()) };
                if let Some(v) = out.get_mut((i * h + y) * w + x) {
                    *v = vy as f32;
                }
                if let Some(v) = out.get_mut(((feats + i) * h + y) * w + x) {
                    *v = vx as f32;
                }
            }
        }
    }
    Ok(Tensor::from_vec(out, (1, 2 * feats, h, w), device)?)
}

/// `inverse_sigmoid` with the reference's clamping (eps 1e-3).
pub fn inverse_sigmoid(x: &Tensor) -> Result<Tensor> {
    let x = x.clamp(0f32, 1f32)?;
    let x1 = x.clamp(1e-3f32, f32::MAX)?;
    let x2 = (1.0 - &x)?.clamp(1e-3f32, f32::MAX)?;
    Ok((x1 / x2)?.log()?)
}

/// `[..., 4]` boxes (cx, cy, w, h) → (x0, y0, x1, y1).
pub fn cxcywh_to_xyxy(b: &Tensor) -> Result<Tensor> {
    let cx = b.narrow(D::Minus1, 0, 1)?;
    let cy = b.narrow(D::Minus1, 1, 1)?;
    let w = (b.narrow(D::Minus1, 2, 1)? * 0.5)?;
    let h = (b.narrow(D::Minus1, 3, 1)? * 0.5)?;
    Ok(Tensor::cat(&[(&cx - &w)?, (&cy - &h)?, (&cx + &w)?, (&cy + &h)?], D::Minus1)?)
}

/// A tensor's values as `f32` on the CPU.
pub fn to_vec(t: &Tensor) -> Result<Vec<f32>> {
    Ok(t.to_dtype(DType::F32)?.flatten_all()?.to_vec1::<f32>()?)
}
