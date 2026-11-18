//! A GPU runner for a CLIP tower, plugged in by a higher layer (the engine wires `lcc-gpu`'s wgpu kernels in, so
//! search uses the graphics card on every OS). The CPU encoder stays the reference: a tower is checked against it on a
//! test input ([`agree`]) before it is used, and any error sends the job back to the CPU.

use std::sync::Arc;

/// One block's weights, linear weights in `[out, in]` order.
#[derive(Clone, Debug, Default)]
pub struct BlockWeights {
    pub ln1_w: Vec<f32>,
    pub ln1_b: Vec<f32>,
    pub q_w: Vec<f32>,
    pub q_b: Vec<f32>,
    pub k_w: Vec<f32>,
    pub k_b: Vec<f32>,
    pub v_w: Vec<f32>,
    pub v_b: Vec<f32>,
    pub o_w: Vec<f32>,
    pub o_b: Vec<f32>,
    pub ln2_w: Vec<f32>,
    pub ln2_b: Vec<f32>,
    pub fc1_w: Vec<f32>,
    pub fc1_b: Vec<f32>,
    pub fc2_w: Vec<f32>,
    pub fc2_b: Vec<f32>,
}

/// Full weights and shape parameters for a CLIP vision tower.
#[derive(Clone, Debug)]
pub struct VisionTowerWeights {
    pub hidden: usize,
    pub intermediate: usize,
    pub heads: usize,
    pub eps: f32,
    pub embed_dim: usize,
    pub patch_size: usize,
    pub num_positions: usize,
    pub img_w: usize,
    pub img_h: usize,
    pub patch_weight: Vec<f32>,
    pub class_embedding: Vec<f32>,
    pub position_embedding: Vec<f32>,
    pub pre_ln_w: Vec<f32>,
    pub pre_ln_b: Vec<f32>,
    pub blocks: Vec<BlockWeights>,
    pub post_ln_w: Vec<f32>,
    pub post_ln_b: Vec<f32>,
    pub projection: Vec<f32>,
}

/// Full weights and shape parameters for a CLIP text tower.
#[derive(Clone, Debug)]
pub struct TextTowerWeights {
    pub hidden: usize,
    pub intermediate: usize,
    pub heads: usize,
    pub eps: f32,
    pub embed_dim: usize,
    pub vocab_size: usize,
    pub context_len: usize,
    pub token_embedding: Vec<f32>,
    pub position_embedding: Vec<f32>,
    pub blocks: Vec<BlockWeights>,
    pub final_ln_w: Vec<f32>,
    pub final_ln_b: Vec<f32>,
    pub projection: Vec<f32>,
}

/// Weights passed to an [`Accelerator`] factory.
#[derive(Clone, Debug)]
pub enum TowerWeights {
    Vision(VisionTowerWeights),
    Text(TextTowerWeights),
    Blocks { hidden: usize, intermediate: usize, heads: usize, causal: bool, eps: f32, blocks: Vec<BlockWeights> },
}

/// Runs a CLIP tower somewhere other than the CPU.
pub trait Accelerator: Send + Sync {
    /// The adapter ("AMD Radeon RX 7900 XTX (Vulkan)").
    fn device(&self) -> String;

    /// Runs preprocessed CHW `images` (`[batch, 3, h, w]`) through the full vision tower, returning `[batch, embed_dim]`.
    fn run_vision(&self, images: &[f32], batch: usize, h: usize, w: usize) -> Result<Vec<f32>, String> {
        let _ = (images, batch, h, w);
        Err("full vision tower is not supported on this accelerator".into())
    }

    /// Runs `token_ids` (`[batch, seq]`) and `eot_indices` (`[batch]`) through the full text tower, returning `[batch, embed_dim]`.
    fn run_text(&self, token_ids: &[u32], eot_indices: &[u32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        let _ = (token_ids, eot_indices, batch, seq);
        Err("full text tower is not supported on this accelerator".into())
    }

    /// `x` (`[batch, seq, hidden]`, row-major) through every block, same shape back.
    fn run_blocks(&self, x: &[f32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        let _ = (x, batch, seq);
        Err("block execution is not supported on this accelerator".into())
    }
}

/// Makes an [`Accelerator`] for a tower, or says why there is none.
pub type AccelFactory = Arc<dyn Fn(&TowerWeights) -> Result<Arc<dyn Accelerator>, String> + Send + Sync>;

/// Whether `got` matches the reference `want` within the tolerance the GPU path is held to.
pub fn agree(got: &[f32], want: &[f32]) -> Result<(), String> {
    if got.len() != want.len() {
        return Err(format!("{} values from the GPU, {} from the CPU", got.len(), want.len()));
    }
    let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
    let worst = got.iter().zip(want).fold(0f32, |m, (g, w)| if g.is_finite() { m.max((g - w).abs()) } else { f32::INFINITY });
    let tolerance = 2e-3 * (1.0 + scale);
    if worst <= tolerance { Ok(()) } else { Err(format!("the GPU differs from the CPU by {worst} (tolerance {tolerance})")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agreement_has_a_tolerance_and_rejects_nan() {
        assert!(agree(&[1.0, 2.0], &[1.0005, 2.0]).is_ok());
        assert!(agree(&[1.0, 2.5], &[1.0, 2.0]).is_err());
        assert!(agree(&[f32::NAN], &[0.0]).is_err());
        assert!(agree(&[1.0], &[1.0, 2.0]).is_err());
    }
}
