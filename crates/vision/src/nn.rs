//! The transformer pieces SigLIP and CLIP share: pre-norm encoder blocks (layer norm → multi-head self-attention →
//! residual; layer norm → MLP → residual), with an optional additive attention mask (CLIP's text tower is causal),
//! plus the small helpers both towers use to read their files and hand back embeddings.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from the Python/PyTorch
//! SigLIP and CLIP code of Hugging Face Transformers (`src/transformers/models/siglip`, `models/clip`; the encoder
//! layers are the same in both), Copyright The HuggingFace Team, Google and OpenAI; translated to Rust on candle and
//! restructured. See NOTICE.

use std::path::Path;

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use crate::config::Act;
use crate::weights::Weights;
use crate::{Error, Result, normalize};

pub(crate) fn linear(i: usize, o: usize, vb: VarBuilder) -> Result<Linear> {
    Ok(candle_nn::linear(i, o, vb)?)
}

/// A projection without bias (CLIP's `visual_projection` / `text_projection`).
pub(crate) fn linear_no_bias(i: usize, o: usize, vb: VarBuilder) -> Result<Linear> {
    Ok(candle_nn::linear_no_bias(i, o, vb)?)
}

/// `[B, L, H·d]` → `[B, H, L, d]`.
pub(crate) fn split_heads(x: &Tensor, heads: usize, head_dim: usize) -> Result<Tensor> {
    let (b, l, _) = x.dims3()?;
    Ok(x.reshape((b, l, heads, head_dim))?.transpose(1, 2)?.contiguous()?)
}

/// Scaled dot-product attention of `[B, H, Lq, d]` queries over `[B, H, Lk, d]`, back to `[B, Lq, H·d]`. `mask` is
/// added to the scores before the softmax (broadcast from `[1, 1, Lq, Lk]`).
pub(crate) fn attend(q: &Tensor, k: &Tensor, v: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
    let (b, h, lq, d) = q.dims4()?;
    let scale = (d as f64).powf(-0.5);
    let mut att = (q.matmul(&k.t()?)? * scale)?;
    if let Some(m) = mask {
        att = att.broadcast_add(m)?;
    }
    let att = candle_nn::ops::softmax_last_dim(&att)?;
    Ok(att.matmul(v)?.transpose(1, 2)?.reshape((b, lq, h * d))?)
}

/// The causal mask for `l` positions: `[1, 1, l, l]`, 0 on and below the diagonal, a large negative number above it
/// (a position sees itself and the ones before it).
pub(crate) fn causal_mask(l: usize, device: &Device) -> Result<Tensor> {
    let mut m = vec![0f32; l * l];
    for (q, row) in m.chunks_mut(l.max(1)).enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            if k > q {
                *v = f32::MIN;
            }
        }
    }
    Ok(Tensor::from_vec(m, (1, 1, l, l), device)?)
}

/// Multi-head self-attention (`SiglipAttention`, `CLIPAttention`).
pub(crate) struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
    head_dim: usize,
}

impl Attention {
    fn new(hidden: usize, heads: usize, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            q: linear(hidden, hidden, vb.pp("q_proj"))?,
            k: linear(hidden, hidden, vb.pp("k_proj"))?,
            v: linear(hidden, hidden, vb.pp("v_proj"))?,
            o: linear(hidden, hidden, vb.pp("out_proj"))?,
            heads,
            head_dim: hidden / heads.max(1),
        })
    }

    fn forward(&self, x: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let q = split_heads(&self.q.forward(x)?, self.heads, self.head_dim)?;
        let k = split_heads(&self.k.forward(x)?, self.heads, self.head_dim)?;
        let v = split_heads(&self.v.forward(x)?, self.heads, self.head_dim)?;
        Ok(self.o.forward(&attend(&q, &k, &v, mask)?)?)
    }
}

/// fc1 → activation → fc2 (`SiglipMLP`, `CLIPMLP`).
pub(crate) struct Mlp {
    fc1: Linear,
    fc2: Linear,
    act: Act,
}

impl Mlp {
    pub(crate) fn new(hidden: usize, inter: usize, act: Act, vb: VarBuilder) -> Result<Self> {
        Ok(Self { fc1: linear(hidden, inter, vb.pp("fc1"))?, fc2: linear(inter, hidden, vb.pp("fc2"))?, act })
    }

    pub(crate) fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.fc1.forward(x)?;
        let x = match self.act {
            Act::GeluTanh => x.gelu()?,
            Act::Gelu => x.gelu_erf()?,
            // x · σ(1.702 x) (CLIP's)
            Act::QuickGelu => (&x * candle_nn::ops::sigmoid(&(&x * 1.702)?)?)?,
        };
        Ok(self.fc2.forward(&x)?)
    }
}

/// A pre-norm encoder block (`SiglipEncoderLayer`, `CLIPEncoderLayer`).
pub(crate) struct Block {
    ln1: LayerNorm,
    attn: Attention,
    ln2: LayerNorm,
    mlp: Mlp,
}

impl Block {
    fn new(hidden: usize, inter: usize, heads: usize, eps: f64, act: Act, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            ln1: candle_nn::layer_norm(hidden, eps, vb.pp("layer_norm1"))?,
            attn: Attention::new(hidden, heads, vb.pp("self_attn"))?,
            ln2: candle_nn::layer_norm(hidden, eps, vb.pp("layer_norm2"))?,
            mlp: Mlp::new(hidden, inter, act, vb.pp("mlp"))?,
        })
    }

    pub(crate) fn forward(&self, x: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let x = (x + self.attn.forward(&self.ln1.forward(x)?, mask)?)?;
        Ok((&x + self.mlp.forward(&self.ln2.forward(&x)?)?)?)
    }
}

/// The `n` blocks under `<vb>.encoder.layers.<i>`.
pub(crate) fn blocks(n: usize, hidden: usize, inter: usize, heads: usize, eps: f64, act: Act, vb: VarBuilder) -> Result<Vec<Block>> {
    (0..n).map(|i| Block::new(hidden, inter, heads, eps, act, vb.pp(format!("encoder.layers.{i}")))).collect()
}

/// A tensor that must hold one finite number (`logit_scale`, `logit_bias`).
pub(crate) fn scalar(weights: &Weights, name: &str) -> Result<f32> {
    let t = weights.read(name)?.to_dtype(DType::F32)?.flatten_all()?;
    let v = t.to_vec1::<f32>()?;
    match v[..] {
        [x] if x.is_finite() => Ok(x),
        _ => Err(Error::Model(format!("{name}: not a finite scalar"))),
    }
}

/// A small file (a config, a tokenizer), refused when larger than `max` bytes.
pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?.len();
    if len > max {
        return Err(Error::Model(format!("{} is larger than {max} bytes", path.display())));
    }
    std::fs::read(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))
}

/// `[B, D]` → normalised rows.
pub(crate) fn rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let mut out = t.to_dtype(DType::F32)?.to_device(&Device::Cpu)?.to_vec2::<f32>()?;
    for r in &mut out {
        if r.iter().any(|v| !v.is_finite()) {
            return Err(Error::Model("the model produced a non-finite embedding".into()));
        }
        normalize(r);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_causal_mask_hides_later_positions() {
        let m = causal_mask(3, &Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert_eq!(&m[..3], &[0.0, f32::MIN, f32::MIN]);
        assert_eq!(&m[3..6], &[0.0, 0.0, f32::MIN]);
        assert_eq!(&m[6..], &[0.0, 0.0, 0.0]);
        // a masked key gets no weight
        let q = Tensor::ones((1, 1, 2, 2), DType::F32, &Device::Cpu).unwrap();
        let v = Tensor::new(&[[[[1f32, 0.0], [0.0, 1.0]]]], &Device::Cpu).unwrap();
        let out = attend(&q, &q, &v, Some(&causal_mask(2, &Device::Cpu).unwrap())).unwrap();
        assert_eq!(out.flatten_all().unwrap().to_vec1::<f32>().unwrap(), vec![1.0, 0.0, 0.5, 0.5]);
    }
}
