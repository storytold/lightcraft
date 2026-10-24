//! The SAM 3 vision backbone: a 32-layer ViT (Perception Encoder) with 2-D axial rotary
//! position encoding, windowed attention (24 × 24) in most layers and global attention in four.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/modeling_sam3.py`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{D, Device, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, VarBuilder};

use crate::nn::{Attention, Mlp, ln, sdpa_tensors};
use crate::{Error, Result, cpu};

pub const IMAGE: usize = 1008;
pub const PATCH: usize = 14;
/// Patches per side (72).
pub const GRID: usize = IMAGE / PATCH;
pub const HIDDEN: usize = 1024;
const HEADS: usize = 16;
const HEAD_DIM: usize = HIDDEN / HEADS;
const LAYERS: usize = 32;
const INTERMEDIATE: usize = 4736;
const WINDOW: usize = 24;
const GLOBAL: [usize; 4] = [7, 15, 23, 31];
const PRETRAIN_GRID: usize = 336 / PATCH;
const EPS: f64 = 1e-6;
/// The attention score scale, `HEAD_DIM^-0.5`. A power of two, so scaling the queries instead
/// of the scores (on the CPU) gives the same scores, bit for bit.
const SCALE: f32 = 0.125;
const _: () = assert!(HEAD_DIM == 64);

/// Cosine and sine tables `[N, head_dim]`, also as plain values for the CPU kernels.
struct Rope {
    cos: Tensor,
    sin: Tensor,
    values: (Vec<f32>, Vec<f32>),
}

/// Cosine and sine tables `[N, head_dim]` for a `side × side` grid whose coordinates are scaled
/// by `scale` (`Sam3ViTRotaryEmbedding`: x frequencies then y, each duplicated pairwise).
fn rope_tables(side: usize, scale: f64, device: &Device) -> Result<Rope> {
    let spatial = HEAD_DIM / 2;
    let inv: Vec<f64> = (0..spatial).step_by(2).map(|i| 1.0 / 10000f64.powf(i as f64 / spatial as f64)).collect();
    let n = side * side;
    let (mut cos, mut sin) = (Vec::with_capacity(n * HEAD_DIM), Vec::with_capacity(n * HEAD_DIM));
    for r in 0..side {
        for c in 0..side {
            let (x, y) = (c as f64 * scale, r as f64 * scale);
            for pos in [x, y] {
                for f in &inv {
                    let a = (pos * f) as f32 as f64;
                    let (s, co) = (a.sin() as f32, a.cos() as f32);
                    cos.extend([co, co]);
                    sin.extend([s, s]);
                }
            }
        }
    }
    Ok(Rope { cos: Tensor::from_slice(&cos, (n, HEAD_DIM), device)?, sin: Tensor::from_slice(&sin, (n, HEAD_DIM), device)?, values: (cos, sin) })
}

/// `x·cos + rotate_pairwise(x)·sin`, where rotate_pairwise maps (a, b) pairs to (−b, a).
fn apply_rope(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    let dims = x.dims().to_vec();
    let last = *dims.last().ok_or_else(|| Error::Model("rope on a scalar".into()))?;
    let mut paired = dims.clone();
    paired.pop();
    paired.extend([last / 2, 2]);
    let p = x.reshape(paired.as_slice())?;
    let a = p.narrow(D::Minus1, 0, 1)?;
    let b = p.narrow(D::Minus1, 1, 1)?;
    let rot = Tensor::cat(&[b.neg()?, a], D::Minus1)?.reshape(dims.as_slice())?;
    Ok((x.broadcast_mul(cos)? + rot.broadcast_mul(sin)?)?)
}

struct Layer {
    ln1: LayerNorm,
    attn: Attention,
    ln2: LayerNorm,
    mlp: Mlp,
    window: usize,
}

impl Layer {
    /// `x`: `[B, H, W, C]`.
    fn forward(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        if x.device().is_cpu() { self.forward_cpu(x, rope) } else { self.forward_tensors(x, rope) }
    }

    /// With candle's tensor operations (any device).
    fn forward_tensors(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        let (b, h, w, c) = x.dims4()?;
        let y = self.ln1.forward(x)?;
        let y = match (h.checked_div(self.window), w.checked_div(self.window)) {
            (Some(nh), Some(nw)) => {
                // non-overlapping windows (72 is a multiple of 24: no padding)
                let ws = self.window;
                let y = y.reshape((b, nh, ws, nw, ws, c))?.permute((0, 1, 3, 2, 4, 5))?.reshape((b * nh * nw, ws, ws, c))?;
                let y = self.attend(&y, rope)?;
                y.reshape((b, nh, nw, ws, ws, c))?.permute((0, 1, 3, 2, 4, 5))?.reshape((b, h, w, c))?
            }
            // global attention
            _ => self.attend(&y, rope)?,
        };
        let x = (x + y)?;
        let y = self.mlp.forward(&self.ln2.forward(&x)?)?;
        Ok((x + y)?)
    }

    /// [`Self::forward`] on the CPU: the same arithmetic, with the window partitioning, head
    /// split, rotary encoding, bias adds, GELU and residual adds in fused parallel kernels.
    fn forward_cpu(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        let (b, h, w, _) = x.dims4()?;
        let grid = cpu::Grid { b, h, w, win: self.window, heads: HEADS, head_dim: HEAD_DIM };
        let y = self.ln1.forward(x)?;
        let [q, k, v, o] = self.attn.projections();
        let table = Some((rope.values.0.as_slice(), rope.values.1.as_slice()));
        let q = cpu::heads(&y, q, grid, table, SCALE)?;
        let k = cpu::heads(&y, k, grid, table, 1.0)?;
        let v = cpu::heads(&y, v, grid, None, 1.0)?;
        let att = cpu::attention(&q, &k, &v, grid)?;
        let x = cpu::linear_add(&att, o, x)?;
        self.mlp.forward_add(&self.ln2.forward(&x)?, &x)
    }

    /// RoPE self-attention over `[B, H, W, C]`.
    fn attend(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        let (b, h, w, c) = x.dims4()?;
        let seq = x.reshape((b, h * w, c))?;
        Ok(self.attn_rope(&seq, rope)?.reshape((b, h, w, c))?)
    }

    fn attn_rope(&self, x: &Tensor, rope: &Rope) -> Result<Tensor> {
        let (b, n, _) = x.dims3()?;
        let split = |t: Tensor| -> Result<Tensor> { Ok(t.reshape((b, n, HEADS, HEAD_DIM))?.transpose(1, 2)?.contiguous()?) };
        let (q, k, v) = self.attn.qkv(x)?;
        let q = apply_rope(&split(q)?, &rope.cos, &rope.sin)?;
        let k = apply_rope(&split(k)?, &rope.cos, &rope.sin)?;
        let v = split(v)?;
        let o = sdpa_tensors(&q, &k, &v, None, (HEAD_DIM as f64).powf(-0.5))?;
        let o = o.transpose(1, 2)?.reshape((b, n, HIDDEN))?;
        self.attn.out(&o)
    }
}

pub struct Vit {
    patch: Conv2d,
    pos: Tensor,
    ln_pre: LayerNorm,
    layers: Vec<Layer>,
    rope_window: Rope,
    rope_global: Rope,
}

impl Vit {
    /// `vb` at `detector_model.vision_encoder.backbone`.
    pub fn new(vb: VarBuilder) -> Result<Self> {
        let device = vb.device().clone();
        let cfg = Conv2dConfig { stride: PATCH, ..Default::default() };
        let patch = candle_nn::conv2d_no_bias(3, HIDDEN, PATCH, cfg, vb.pp("embeddings.patch_embeddings.projection"))?;
        let pos = vb.get((1, PRETRAIN_GRID * PRETRAIN_GRID, HIDDEN), "embeddings.position_embeddings")?;
        // tile (not interpolate) the 24 × 24 pretraining table over the 72 × 72 grid
        let reps = GRID.div_ceil(PRETRAIN_GRID);
        let pos = pos
            .reshape((1, PRETRAIN_GRID, PRETRAIN_GRID, HIDDEN))?
            .repeat((1, reps, reps, 1))?
            .narrow(1, 0, GRID)?
            .narrow(2, 0, GRID)?
            .reshape((1, GRID * GRID, HIDDEN))?;
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = vb.pp(format!("layers.{i}"));
            layers.push(Layer {
                ln1: ln(HIDDEN, EPS, l.pp("layer_norm1"))?,
                attn: Attention::new(HIDDEN, HIDDEN, HEADS, l.pp("attention"))?,
                ln2: ln(HIDDEN, EPS, l.pp("layer_norm2"))?,
                mlp: Mlp::new(HIDDEN, INTERMEDIATE, true, l.pp("mlp"))?,
                window: if GLOBAL.contains(&i) { 0 } else { WINDOW },
            });
        }
        Ok(Self {
            patch,
            pos,
            ln_pre: ln(HIDDEN, EPS, vb.pp("layer_norm"))?,
            layers,
            rope_window: rope_tables(WINDOW, 1.0, &device)?,
            rope_global: rope_tables(GRID, WINDOW as f64 / GRID as f64, &device)?,
        })
    }

    /// `pixels`: `[1, 3, 1008, 1008]` normalized to −1..1. Returns `[1, 1024, 72, 72]`.
    pub fn forward(&self, pixels: &Tensor) -> Result<Tensor> {
        let x = self.patch.forward(pixels)?; // [1, C, 72, 72]
        let b = x.dim(0)?;
        let x = x.flatten_from(2)?.transpose(1, 2)?.broadcast_add(&self.pos)?;
        let mut x = self.ln_pre.forward(&x.reshape((b, GRID, GRID, HIDDEN))?)?;
        for l in &self.layers {
            let rope = if l.window > 0 { &self.rope_window } else { &self.rope_global };
            x = l.forward(&x, rope)?;
        }
        Ok(x.permute((0, 3, 1, 2))?.contiguous()?)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use candle_core::DType;

    use super::*;

    /// A layer with random weights.
    fn layer(window: usize) -> Layer {
        let d = Device::Cpu;
        let mut t = HashMap::new();
        let mut add = |name: String, shape: &[usize], mean: f32| {
            t.insert(name, Tensor::randn(mean, 0.05f32, shape, &d).unwrap());
        };
        for n in ["layer_norm1", "layer_norm2"] {
            add(format!("{n}.weight"), &[HIDDEN], 1.0);
            add(format!("{n}.bias"), &[HIDDEN], 0.0);
        }
        for n in ["q_proj", "k_proj", "v_proj", "o_proj"] {
            add(format!("attention.{n}.weight"), &[HIDDEN, HIDDEN], 0.0);
            add(format!("attention.{n}.bias"), &[HIDDEN], 0.0);
        }
        add("mlp.fc1.weight".into(), &[INTERMEDIATE, HIDDEN], 0.0);
        add("mlp.fc1.bias".into(), &[INTERMEDIATE], 0.0);
        add("mlp.fc2.weight".into(), &[HIDDEN, INTERMEDIATE], 0.0);
        add("mlp.fc2.bias".into(), &[HIDDEN], 0.0);
        let vb = VarBuilder::from_tensors(t, DType::F32, &d);
        Layer {
            ln1: ln(HIDDEN, EPS, vb.pp("layer_norm1")).unwrap(),
            attn: Attention::new(HIDDEN, HIDDEN, HEADS, vb.pp("attention")).unwrap(),
            ln2: ln(HIDDEN, EPS, vb.pp("layer_norm2")).unwrap(),
            mlp: Mlp::new(HIDDEN, INTERMEDIATE, true, vb.pp("mlp")).unwrap(),
            window,
        }
    }

    #[test]
    fn cpu_kernels_match_the_tensor_ops() {
        // two images each: 12 × 24 tokens in 6 × 6 windows, and 12 × 12 with global attention
        for (window, (h, w), side, scale) in [(6, (12, 24), 6, 1.0), (0, (12, 12), 12, 0.5)] {
            let x = Tensor::randn(0f32, 1.0, (2, h, w, HIDDEN), &Device::Cpu).unwrap();
            let (l, rope) = (layer(window), rope_tables(side, scale, &Device::Cpu).unwrap());
            let want = l.forward_tensors(&x, &rope).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap();
            let got = l.forward_cpu(&x, &rope).unwrap();
            assert_eq!(got.dims(), x.dims());
            let got = got.flatten_all().unwrap().to_vec1::<f32>().unwrap();
            let err = want.iter().zip(&got).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            // the same arithmetic (one attention block per window here): equal on x86-64
            assert!(err <= 1e-5, "window {window}: max error {err}");
        }
    }
}
