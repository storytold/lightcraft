//! The CLIP text encoder (24 layers, 1024 wide, causal) that turns a prompt into the token
//! features SAM 3's detector attends to.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/modeling_sam3.py`, CLIP text model), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{Device, Module, Tensor};
use candle_nn::{Embedding, LayerNorm, VarBuilder};

use crate::Result;
use crate::nn::{Attention, Mlp, ln};
use crate::tokenizer::CONTEXT;

const HIDDEN: usize = 1024;
const LAYERS: usize = 24;
const HEADS: usize = 16;
const EPS: f64 = 1e-5;

struct Layer {
    ln1: LayerNorm,
    attn: Attention,
    ln2: LayerNorm,
    mlp: Mlp,
}

pub struct TextEncoder {
    tokens: Embedding,
    positions: Tensor,
    layers: Vec<Layer>,
    final_ln: LayerNorm,
    device: Device,
}

impl TextEncoder {
    /// `vb` at `detector_model.text_encoder.text_model`.
    pub fn new(vb: VarBuilder) -> Result<Self> {
        let e = vb.pp("embeddings");
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = vb.pp(format!("encoder.layers.{i}"));
            layers.push(Layer {
                ln1: ln(HIDDEN, EPS, l.pp("layer_norm1"))?,
                attn: Attention::named(HIDDEN, HIDDEN, HEADS, "out_proj", l.pp("self_attn"))?,
                ln2: ln(HIDDEN, EPS, l.pp("layer_norm2"))?,
                mlp: Mlp::new(HIDDEN, 4 * HIDDEN, true, l.pp("mlp"))?,
            });
        }
        Ok(Self {
            tokens: candle_nn::embedding(49408, HIDDEN, e.pp("token_embedding"))?,
            positions: e.get((CONTEXT, HIDDEN), "position_embedding.weight")?.unsqueeze(0)?,
            layers,
            final_ln: ln(HIDDEN, EPS, vb.pp("final_layer_norm"))?,
            device: vb.device().clone(),
        })
    }

    /// `[1, 32, 1024]` features for token `ids` (`valid` marks real tokens).
    pub fn forward(&self, ids: &[u32], valid: &[bool]) -> Result<Tensor> {
        let ids = Tensor::from_slice(ids, (1, ids.len()), &self.device)?;
        let mut x = self.tokens.forward(&ids)?.broadcast_add(&self.positions)?;
        // causal, and padding keys masked
        let n = valid.len();
        let mut bias = vec![0f32; n * n];
        for (q, row) in bias.chunks_mut(n).enumerate() {
            for (k, v) in row.iter_mut().enumerate() {
                if k > q || !valid.get(k).copied().unwrap_or(false) {
                    *v = f32::MIN;
                }
            }
        }
        let bias = Tensor::from_vec(bias, (1, 1, n, n), &self.device)?;
        for l in &self.layers {
            let h = l.ln1.forward(&x)?;
            x = (&x + l.attn.forward(&h, &h, &h, Some(&bias))?)?;
            let h = l.ln2.forward(&x)?;
            x = (&x + l.mlp.forward(&h)?)?;
        }
        Ok(self.final_ln.forward(&x)?)
    }
}
