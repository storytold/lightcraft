//! Clean-room MIT CLIP ViT + Text transformer encoder on `candle-core`.
//!
//! Built from the CLIP paper (Radford et al., "Learning Transferable Visual Models From Natural Language Supervision",
//! ICML 2021) and the standard HuggingFace / OpenCLIP `model.safetensors` tensor layout.
//!
//! Supports both full checkpoints (`laion/CLIP-ViT-B-32-laion2B-s34B-b79K`) and compact synthetic checkpoints used by
//! automated tests ([`Clip::write_synthetic_dir`]).

use std::path::Path;
use std::sync::{Arc, Mutex};

use candle_core::{D, Device as CandleDevice, Tensor};
use thiserror::Error;

use crate::accel::{AccelFactory, Accelerator, BlockWeights, TextTowerWeights, TowerWeights, VisionTowerWeights, agree};
use crate::encoder::{Encoder, RgbImage, preprocess_image};
use crate::manifest::{Commercial, InputSpec, Licence, MANIFEST_FILE, MANIFEST_VERSION, ModelFile, ModelManifest, Role, VerifyError, sha256_file};
use crate::queue::{Ctx, JobError};
use crate::store::l2_normalize;
use crate::tokenizer::{Tokenizer, TokenizerError};
use crate::weights::{SafetensorsFile, WeightsError, encode_f32_safetensors};

const LAYER_NORM_EPS: f64 = 1e-5;

#[derive(Debug, Error)]
pub enum ClipError {
    #[error(transparent)]
    Verify(#[from] VerifyError),
    #[error(transparent)]
    Weights(#[from] WeightsError),
    #[error(transparent)]
    Tokenizer(#[from] TokenizerError),
    #[error("invalid CLIP architecture in checkpoint: {0}")]
    InvalidArchitecture(String),
}

struct Linear {
    weight_t: Tensor,
    bias: Option<Tensor>,
}

impl Linear {
    fn load(
        st: &mut SafetensorsFile,
        prefix: &str,
        out_dim: usize,
        in_dim: usize,
        has_bias: bool,
        device: &CandleDevice,
    ) -> Result<Self, WeightsError> {
        let w = st.load_tensor(&format!("{prefix}.weight"), &[out_dim, in_dim], device)?;
        let weight_t =
            w.t().and_then(|t| t.contiguous()).map_err(|e| WeightsError::Device { name: format!("{prefix}.weight"), message: e.to_string() })?;
        let bias = if has_bias { Some(st.load_tensor(&format!("{prefix}.bias"), &[out_dim], device)?) } else { None };
        Ok(Self { weight_t, bias })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor, candle_core::Error> {
        let dims = x.dims();
        let out_dim = self.weight_t.dims().get(1).copied().unwrap_or(0);
        let in_dim = self.weight_t.dims().first().copied().unwrap_or(0);
        let leading: usize = dims.get(..dims.len().saturating_sub(1)).unwrap_or(&[]).iter().product();
        let flat = x.reshape((leading, in_dim))?;
        let mut y = flat.matmul(&self.weight_t)?;
        if let Some(b) = &self.bias {
            y = y.broadcast_add(b)?;
        }
        let mut out_shape = dims.get(..dims.len().saturating_sub(1)).unwrap_or(&[]).to_vec();
        out_shape.push(out_dim);
        y.reshape(out_shape)
    }
}

struct LayerNorm {
    weight: Tensor,
    bias: Tensor,
}

impl LayerNorm {
    fn load(st: &mut SafetensorsFile, prefix: &str, dim: usize, device: &CandleDevice) -> Result<Self, WeightsError> {
        let weight = st.load_tensor(&format!("{prefix}.weight"), &[dim], device)?;
        let bias = st.load_tensor(&format!("{prefix}.bias"), &[dim], device)?;
        Ok(Self { weight, bias })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor, candle_core::Error> {
        let mean = x.mean_keepdim(D::Minus1)?;
        let centered = x.broadcast_sub(&mean)?;
        let var = centered.sqr()?.mean_keepdim(D::Minus1)?;
        let std = (var + LAYER_NORM_EPS)?.sqrt()?;
        let norm = centered.broadcast_div(&std)?;
        norm.broadcast_mul(&self.weight)?.broadcast_add(&self.bias)
    }
}

struct TransformerBlock {
    ln1: LayerNorm,
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    out_proj: Linear,
    ln2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    num_heads: usize,
    head_dim: usize,
}

impl TransformerBlock {
    fn load(
        st: &mut SafetensorsFile,
        prefix: &str,
        hidden: usize,
        intermediate: usize,
        num_heads: usize,
        device: &CandleDevice,
    ) -> Result<Self, WeightsError> {
        let head_dim = hidden / num_heads;
        Ok(Self {
            ln1: LayerNorm::load(st, &format!("{prefix}.layer_norm1"), hidden, device)?,
            q_proj: Linear::load(st, &format!("{prefix}.self_attn.q_proj"), hidden, hidden, true, device)?,
            k_proj: Linear::load(st, &format!("{prefix}.self_attn.k_proj"), hidden, hidden, true, device)?,
            v_proj: Linear::load(st, &format!("{prefix}.self_attn.v_proj"), hidden, hidden, true, device)?,
            out_proj: Linear::load(st, &format!("{prefix}.self_attn.out_proj"), hidden, hidden, true, device)?,
            ln2: LayerNorm::load(st, &format!("{prefix}.layer_norm2"), hidden, device)?,
            fc1: Linear::load(st, &format!("{prefix}.mlp.fc1"), intermediate, hidden, true, device)?,
            fc2: Linear::load(st, &format!("{prefix}.mlp.fc2"), hidden, intermediate, true, device)?,
            num_heads,
            head_dim,
        })
    }

    fn forward(&self, x: &Tensor, causal_mask: Option<&Tensor>) -> Result<Tensor, candle_core::Error> {
        let (b, seq, hidden) = x.dims3()?;
        let normed = self.ln1.forward(x)?;
        let q = self.q_proj.forward(&normed)?.reshape((b, seq, self.num_heads, self.head_dim))?.transpose(1, 2)?.contiguous()?;
        let k = self.k_proj.forward(&normed)?.reshape((b, seq, self.num_heads, self.head_dim))?.transpose(1, 2)?.contiguous()?;
        let v = self.v_proj.forward(&normed)?.reshape((b, seq, self.num_heads, self.head_dim))?.transpose(1, 2)?.contiguous()?;

        let scale = 1.0f64 / (self.head_dim as f64).sqrt();
        let kt = k.transpose(2, 3)?.contiguous()?;
        let mut attn = (q.matmul(&kt)? * scale)?;
        if let Some(mask) = causal_mask {
            attn = attn.broadcast_add(mask)?;
        }
        // Numerically stable softmax along last axis
        let max_logits = attn.max_keepdim(D::Minus1)?;
        let exp = attn.broadcast_sub(&max_logits)?.exp()?;
        let denom = exp.sum_keepdim(D::Minus1)?;
        let probs = exp.broadcast_div(&denom)?;

        let ctx = probs.matmul(&v)?.transpose(1, 2)?.contiguous()?.reshape((b, seq, hidden))?;
        let attn_out = self.out_proj.forward(&ctx)?;
        let x = (x + attn_out)?;

        let normed2 = self.ln2.forward(&x)?;
        let mlp_hidden = self.fc1.forward(&normed2)?.gelu_erf()?;
        let mlp_out = self.fc2.forward(&mlp_hidden)?;
        x + mlp_out
    }
}

struct VisionTower {
    device: CandleDevice,
    class_embedding: Tensor,
    patch_weight: Tensor,
    position_embedding: Tensor,
    pre_ln: LayerNorm,
    blocks: Vec<TransformerBlock>,
    post_ln: LayerNorm,
    projection: Linear,
    patch_size: usize,
    hidden: usize,
}

struct TextTower {
    device: CandleDevice,
    token_embedding: Tensor,
    position_embedding: Tensor,
    blocks: Vec<TransformerBlock>,
    final_ln: LayerNorm,
    projection: Linear,
    causal_mask: Tensor,
}

struct DeviceTowers {
    vision: VisionTower,
    text: TextTower,
}

#[derive(Debug, Clone, Copy)]
struct ArchSpec {
    embed_dim: usize,
    vision_hidden: usize,
    vision_intermediate: usize,
    vision_layers: usize,
    vision_heads: usize,
    patch_size: usize,
    num_positions: usize,
    vocab_size: usize,
    context_len: usize,
    text_hidden: usize,
    text_intermediate: usize,
    text_layers: usize,
    text_heads: usize,
}

fn infer_heads(hidden: usize, preferred_head_dim: usize) -> usize {
    if hidden.is_multiple_of(preferred_head_dim) && hidden >= preferred_head_dim {
        hidden / preferred_head_dim
    } else if hidden.is_multiple_of(4) && hidden >= 4 {
        4
    } else {
        1
    }
}

fn infer_arch(st: &SafetensorsFile, manifest: &ModelManifest) -> Result<ArchSpec, ClipError> {
    let vp = st.info("visual_projection.weight").ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("visual_projection.weight".into())))?;
    if vp.shape.len() != 2 {
        return Err(ClipError::InvalidArchitecture("visual_projection.weight must be rank 2".into()));
    }
    let embed_dim = vp.shape[0];
    let vision_hidden = vp.shape[1];
    if embed_dim != manifest.embed_dim as usize {
        return Err(ClipError::InvalidArchitecture(format!("visual_projection embed_dim {embed_dim} != manifest embed_dim {}", manifest.embed_dim)));
    }

    let patch = st
        .info("vision_model.embeddings.patch_embedding.weight")
        .ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("vision_model.embeddings.patch_embedding.weight".into())))?;
    if patch.shape.len() != 4 || patch.shape[0] != vision_hidden || patch.shape[1] != 3 || patch.shape[2] != patch.shape[3] {
        return Err(ClipError::InvalidArchitecture(format!("invalid patch_embedding shape {:?}", patch.shape)));
    }
    let patch_size = patch.shape[2];
    if patch_size == 0 || !(manifest.input.width as usize).is_multiple_of(patch_size) || !(manifest.input.height as usize).is_multiple_of(patch_size)
    {
        return Err(ClipError::InvalidArchitecture(format!(
            "input {}x{} is not divisible by patch_size {patch_size}",
            manifest.input.width, manifest.input.height
        )));
    }
    let grid_w = (manifest.input.width as usize) / patch_size;
    let grid_h = (manifest.input.height as usize) / patch_size;
    let num_positions = 1 + grid_w * grid_h;

    let mut vision_layers = 0usize;
    while st.contains(&format!("vision_model.encoder.layers.{vision_layers}.self_attn.q_proj.weight")) {
        vision_layers += 1;
    }
    if vision_layers == 0 {
        return Err(ClipError::InvalidArchitecture("no vision_model.encoder.layers found".into()));
    }
    let v_fc1 = st
        .info("vision_model.encoder.layers.0.mlp.fc1.weight")
        .ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("vision_model.encoder.layers.0.mlp.fc1.weight".into())))?;
    let vision_intermediate = v_fc1.shape.first().copied().unwrap_or(0);
    let vision_heads = infer_heads(vision_hidden, 64);

    let tok_emb = st
        .info("text_model.embeddings.token_embedding.weight")
        .ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("text_model.embeddings.token_embedding.weight".into())))?;
    if tok_emb.shape.len() != 2 {
        return Err(ClipError::InvalidArchitecture("text_model.embeddings.token_embedding.weight must be rank 2".into()));
    }
    let vocab_size = tok_emb.shape[0];
    let text_hidden = tok_emb.shape[1];

    let pos_emb = st
        .info("text_model.embeddings.position_embedding.weight")
        .ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("text_model.embeddings.position_embedding.weight".into())))?;
    if pos_emb.shape.len() != 2 || pos_emb.shape[1] != text_hidden {
        return Err(ClipError::InvalidArchitecture("invalid text position_embedding shape".into()));
    }
    let context_len = pos_emb.shape[0];

    let mut text_layers = 0usize;
    while st.contains(&format!("text_model.encoder.layers.{text_layers}.self_attn.q_proj.weight")) {
        text_layers += 1;
    }
    if text_layers == 0 {
        return Err(ClipError::InvalidArchitecture("no text_model.encoder.layers found".into()));
    }
    let t_fc1 = st
        .info("text_model.encoder.layers.0.mlp.fc1.weight")
        .ok_or_else(|| ClipError::Weights(WeightsError::MissingTensor("text_model.encoder.layers.0.mlp.fc1.weight".into())))?;
    let text_intermediate = t_fc1.shape.first().copied().unwrap_or(0);
    let text_heads = infer_heads(text_hidden, 64);

    Ok(ArchSpec {
        embed_dim,
        vision_hidden,
        vision_intermediate,
        vision_layers,
        vision_heads,
        patch_size,
        num_positions,
        vocab_size,
        context_len,
        text_hidden,
        text_intermediate,
        text_layers,
        text_heads,
    })
}

fn load_vision_tower(st: &mut SafetensorsFile, arch: ArchSpec, device: CandleDevice) -> Result<VisionTower, WeightsError> {
    let class_embedding = st.load_tensor("vision_model.embeddings.class_embedding", &[arch.vision_hidden], &device)?;
    let patch_weight =
        st.load_tensor("vision_model.embeddings.patch_embedding.weight", &[arch.vision_hidden, 3, arch.patch_size, arch.patch_size], &device)?;
    let position_embedding =
        st.load_tensor("vision_model.embeddings.position_embedding.weight", &[arch.num_positions, arch.vision_hidden], &device)?;
    let pre_ln = LayerNorm::load(st, "vision_model.pre_layrnorm", arch.vision_hidden, &device)?;
    let mut vision_blocks = Vec::with_capacity(arch.vision_layers);
    for i in 0..arch.vision_layers {
        vision_blocks.push(TransformerBlock::load(
            st,
            &format!("vision_model.encoder.layers.{i}"),
            arch.vision_hidden,
            arch.vision_intermediate,
            arch.vision_heads,
            &device,
        )?);
    }
    let post_ln = LayerNorm::load(st, "vision_model.post_layernorm", arch.vision_hidden, &device)?;
    let visual_projection = Linear::load(st, "visual_projection", arch.embed_dim, arch.vision_hidden, false, &device)?;

    Ok(VisionTower {
        device,
        class_embedding,
        patch_weight,
        position_embedding,
        pre_ln,
        blocks: vision_blocks,
        post_ln,
        projection: visual_projection,
        patch_size: arch.patch_size,
        hidden: arch.vision_hidden,
    })
}

fn load_text_tower(st: &mut SafetensorsFile, arch: ArchSpec, device: CandleDevice) -> Result<TextTower, WeightsError> {
    let token_embedding = st.load_tensor("text_model.embeddings.token_embedding.weight", &[arch.vocab_size, arch.text_hidden], &device)?;
    let text_position_embedding =
        st.load_tensor("text_model.embeddings.position_embedding.weight", &[arch.context_len, arch.text_hidden], &device)?;
    let mut text_blocks = Vec::with_capacity(arch.text_layers);
    for i in 0..arch.text_layers {
        text_blocks.push(TransformerBlock::load(
            st,
            &format!("text_model.encoder.layers.{i}"),
            arch.text_hidden,
            arch.text_intermediate,
            arch.text_heads,
            &device,
        )?);
    }
    let final_ln = LayerNorm::load(st, "text_model.final_layer_norm", arch.text_hidden, &device)?;
    let text_projection = Linear::load(st, "text_projection", arch.embed_dim, arch.text_hidden, false, &device)?;

    let mut mask_vals = vec![0.0f32; arch.context_len * arch.context_len];
    for q in 0..arch.context_len {
        for k in (q + 1)..arch.context_len {
            if let Some(slot) = mask_vals.get_mut(q * arch.context_len + k) {
                *slot = -1.0e9;
            }
        }
    }
    let causal_mask = Tensor::from_vec(mask_vals, (1, 1, arch.context_len, arch.context_len), &device)
        .map_err(|e| WeightsError::Device { name: "causal_mask".into(), message: e.to_string() })?;

    Ok(TextTower {
        device,
        token_embedding,
        position_embedding: text_position_embedding,
        blocks: text_blocks,
        final_ln,
        projection: text_projection,
        causal_mask,
    })
}

fn load_towers(weights_path: &Path, arch: ArchSpec, device: CandleDevice) -> Result<DeviceTowers, WeightsError> {
    let mut st = SafetensorsFile::open(weights_path)?;
    let vision = load_vision_tower(&mut st, arch, device.clone())?;
    let text = load_text_tower(&mut st, arch, device)?;
    Ok(DeviceTowers { vision, text })
}

fn flat(t: &Tensor) -> Result<Vec<f32>, String> {
    t.flatten_all().and_then(|t| t.to_vec1::<f32>()).map_err(|e| e.to_string())
}

/// A linear layer's weight back in `[out, in]` order, and its bias (zeros when it has none).
fn linear_weights(l: &Linear) -> Result<(Vec<f32>, Vec<f32>), String> {
    let w = l.weight_t.t().and_then(|t| t.contiguous()).map_err(|e| e.to_string())?;
    let out = l.weight_t.dims().get(1).copied().unwrap_or(0);
    let b = match &l.bias {
        Some(b) => flat(b)?,
        None => vec![0.0; out],
    };
    Ok((flat(&w)?, b))
}

/// A tower's blocks as plain weights for an [`Accelerator`].
fn blocks_weights(blocks: &[TransformerBlock]) -> Result<Vec<BlockWeights>, String> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        let (q_w, q_b) = linear_weights(&b.q_proj)?;
        let (k_w, k_b) = linear_weights(&b.k_proj)?;
        let (v_w, v_b) = linear_weights(&b.v_proj)?;
        let (o_w, o_b) = linear_weights(&b.out_proj)?;
        let (fc1_w, fc1_b) = linear_weights(&b.fc1)?;
        let (fc2_w, fc2_b) = linear_weights(&b.fc2)?;
        out.push(BlockWeights {
            ln1_w: flat(&b.ln1.weight)?,
            ln1_b: flat(&b.ln1.bias)?,
            q_w,
            q_b,
            k_w,
            k_b,
            v_w,
            v_b,
            o_w,
            o_b,
            ln2_w: flat(&b.ln2.weight)?,
            ln2_b: flat(&b.ln2.bias)?,
            fc1_w,
            fc1_b,
            fc2_w,
            fc2_b,
        });
    }
    Ok(out)
}

fn vision_tower_weights(v: &VisionTower, arch: ArchSpec, manifest: &ModelManifest) -> Result<VisionTowerWeights, String> {
    Ok(VisionTowerWeights {
        hidden: arch.vision_hidden,
        intermediate: arch.vision_intermediate,
        heads: arch.vision_heads,
        eps: LAYER_NORM_EPS as f32,
        embed_dim: arch.embed_dim,
        patch_size: arch.patch_size,
        num_positions: arch.num_positions,
        img_w: manifest.input.width as usize,
        img_h: manifest.input.height as usize,
        patch_weight: flat(&v.patch_weight)?,
        class_embedding: flat(&v.class_embedding)?,
        position_embedding: flat(&v.position_embedding)?,
        pre_ln_w: flat(&v.pre_ln.weight)?,
        pre_ln_b: flat(&v.pre_ln.bias)?,
        blocks: blocks_weights(&v.blocks)?,
        post_ln_w: flat(&v.post_ln.weight)?,
        post_ln_b: flat(&v.post_ln.bias)?,
        projection: linear_weights(&v.projection)?.0,
    })
}

fn text_tower_weights(t: &TextTower, arch: ArchSpec) -> Result<TextTowerWeights, String> {
    Ok(TextTowerWeights {
        hidden: arch.text_hidden,
        intermediate: arch.text_intermediate,
        heads: arch.text_heads,
        eps: LAYER_NORM_EPS as f32,
        embed_dim: arch.embed_dim,
        vocab_size: arch.vocab_size,
        context_len: arch.context_len,
        token_embedding: flat(&t.token_embedding)?,
        position_embedding: flat(&t.position_embedding)?,
        blocks: blocks_weights(&t.blocks)?,
        final_ln_w: flat(&t.final_ln.weight)?,
        final_ln_b: flat(&t.final_ln.bias)?,
        projection: linear_weights(&t.projection)?.0,
    })
}

fn run_cpu_vision(vision: &VisionTower, flat_chw: &[f32], b: usize, h: usize, w: usize, ctx: Option<&Ctx>) -> Result<Vec<f32>, JobError> {
    let map_err = |e: candle_core::Error| JobError::Failed(e.to_string());
    let x = Tensor::from_slice(flat_chw, (b, 3, h, w), &vision.device).map_err(map_err)?;
    let patches = x
        .conv2d(&vision.patch_weight, 0, vision.patch_size, 1, 1)
        .and_then(|t| t.flatten_from(2))
        .and_then(|t| t.transpose(1, 2))
        .and_then(|t| t.contiguous())
        .map_err(map_err)?;

    let cls = vision
        .class_embedding
        .reshape((1, 1, vision.hidden))
        .and_then(|t| t.broadcast_as((b, 1, vision.hidden)))
        .and_then(|t| t.contiguous())
        .map_err(map_err)?;

    let pos = vision.position_embedding.unsqueeze(0).map_err(map_err)?;
    let mut hidden =
        Tensor::cat(&[&cls, &patches], 1).and_then(|t| t.broadcast_add(&pos)).and_then(|t| vision.pre_ln.forward(&t)).map_err(map_err)?;

    for block in &vision.blocks {
        if let Some(c) = ctx {
            c.check()?;
        }
        hidden = block.forward(&hidden, None).map_err(map_err)?;
    }

    let projected = hidden
        .narrow(1, 0, 1)
        .and_then(|t| t.squeeze(1))
        .and_then(|t| vision.post_ln.forward(&t))
        .and_then(|t| vision.projection.forward(&t))
        .map_err(map_err)?;
    flat(&projected).map_err(JobError::Failed)
}

fn run_cpu_text(
    text_tower: &TextTower,
    flat_ids: &[u32],
    eot: &[u32],
    b: usize,
    seq: usize,
    hidden_dim: usize,
    ctx: Option<&Ctx>,
) -> Result<Vec<f32>, JobError> {
    let map_err = |e: candle_core::Error| JobError::Failed(e.to_string());
    let ids_tensor = Tensor::from_slice(flat_ids, (b * seq,), &text_tower.device).map_err(map_err)?;
    let tok_emb = text_tower.token_embedding.index_select(&ids_tensor, 0).and_then(|t| t.reshape((b, seq, hidden_dim))).map_err(map_err)?;
    let pos = text_tower.position_embedding.unsqueeze(0).map_err(map_err)?;
    let mut hidden = tok_emb.broadcast_add(&pos).map_err(map_err)?;

    for block in &text_tower.blocks {
        if let Some(c) = ctx {
            c.check()?;
        }
        hidden = block.forward(&hidden, Some(&text_tower.causal_mask)).map_err(map_err)?;
    }

    let normed = text_tower.final_ln.forward(&hidden).map_err(map_err)?;
    let mut rows = Vec::with_capacity(b);
    for (row, &eos_idx) in eot.iter().enumerate() {
        let r = normed.narrow(0, row, 1).and_then(|t| t.narrow(1, eos_idx as usize, 1)).and_then(|t| t.squeeze(1)).map_err(map_err)?;
        rows.push(r);
    }
    let pooled = Tensor::cat(&rows, 0).map_err(map_err)?;
    let projected = text_tower.projection.forward(&pooled).map_err(map_err)?;
    flat(&projected).map_err(JobError::Failed)
}

/// A runner from `factory` for `weights`, once it agrees with the CPU (`cpu`) on a test input.
fn check_accel(
    factory: &AccelFactory,
    weights: &TowerWeights,
    run_gpu: impl FnOnce(&dyn Accelerator) -> Result<Vec<f32>, String>,
    cpu: impl FnOnce() -> Result<Vec<f32>, String>,
) -> Result<Arc<dyn Accelerator>, String> {
    let acc =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory(weights))).unwrap_or_else(|_| Err("making the GPU runner failed".into()))?;
    let want = cpu()?;
    let got = run_gpu(&*acc)?;
    agree(&got, &want)?;
    log::info!("clip: full tower on {} (checked against the CPU)", acc.device());
    Ok(acc)
}

/// Loaded CLIP ViT + Text encoder ready for [`crate::Queue`] execution.
pub struct Clip {
    manifest: ModelManifest,
    weights_path: std::path::PathBuf,
    arch: ArchSpec,
    tokenizer: Tokenizer,
    cpu_vision: Mutex<Option<Arc<VisionTower>>>,
    cpu_text: Mutex<Option<Arc<TextTower>>>,
    /// Makes the GPU runner for a tower ([`Clip::with_accelerator`]).
    accel: Option<AccelFactory>,
    /// Each tower's GPU runner once made, or why there is none.
    gpu_vision: Mutex<Option<Result<Arc<dyn Accelerator>, String>>>,
    gpu_text: Mutex<Option<Result<Arc<dyn Accelerator>, String>>>,
}

impl Clip {
    /// Verify `dir` against its `manifest.json` (checking file sizes and SHA-256 digests) and load the CLIP vision
    /// and text towers plus BPE tokenizer.
    pub fn load(dir: &Path) -> Result<Self, ClipError> {
        let manifest = ModelManifest::verify_dir(dir)?;
        let weights_path = dir.join("model.safetensors");
        let st = SafetensorsFile::open(&weights_path)?;
        let arch = infer_arch(&st, &manifest)?;
        drop(st);

        let vocab_bytes = std::fs::read(dir.join("vocab.json"))
            .map_err(|e| TokenizerError::Io { path: dir.join("vocab.json").display().to_string(), message: e.to_string() })?;
        let merges_str = std::fs::read_to_string(dir.join("merges.txt"))
            .map_err(|e| TokenizerError::Io { path: dir.join("merges.txt").display().to_string(), message: e.to_string() })?;
        let tokenizer = Tokenizer::from_json_and_merges(&vocab_bytes, &merges_str, arch.context_len)?;
        let cpu = load_towers(&weights_path, arch, CandleDevice::Cpu)?;

        Ok(Self {
            manifest,
            weights_path,
            arch,
            tokenizer,
            cpu_vision: Mutex::new(Some(Arc::new(cpu.vision))),
            cpu_text: Mutex::new(Some(Arc::new(cpu.text))),
            accel: None,
            gpu_vision: Mutex::new(None),
            gpu_text: Mutex::new(None),
        })
    }

    /// Open an already-installed model directory (`manifest.json` + exact file sizes + `.safetensors` header + BPE
    /// tokenizer) without re-computing SHA-256 over multi-hundred-megabyte weights on the UI thread, deferring weight
    /// tensor loading to the worker thread on first `encode_images` / `encode_text`.
    pub fn load_lazy(dir: &Path) -> Result<Self, ClipError> {
        let manifest = ModelManifest::verify_dir_sizes(dir)?;
        let weights_path = dir.join("model.safetensors");
        let st = SafetensorsFile::open(&weights_path)?;
        let arch = infer_arch(&st, &manifest)?;
        drop(st);

        let vocab_bytes = std::fs::read(dir.join("vocab.json"))
            .map_err(|e| TokenizerError::Io { path: dir.join("vocab.json").display().to_string(), message: e.to_string() })?;
        let merges_str = std::fs::read_to_string(dir.join("merges.txt"))
            .map_err(|e| TokenizerError::Io { path: dir.join("merges.txt").display().to_string(), message: e.to_string() })?;
        let tokenizer = Tokenizer::from_json_and_merges(&vocab_bytes, &merges_str, arch.context_len)?;

        Ok(Self {
            manifest,
            weights_path,
            arch,
            tokenizer,
            cpu_vision: Mutex::new(None),
            cpu_text: Mutex::new(None),
            accel: None,
            gpu_vision: Mutex::new(None),
            gpu_text: Mutex::new(None),
        })
    }

    /// Run the towers' transformer blocks with `factory`'s runner when a job is on the GPU. Without one, GPU jobs fail
    /// with [`JobError::Device`] and the queue moves them to the CPU, saying why.
    pub fn with_accelerator(mut self, factory: AccelFactory) -> Self {
        self.accel = Some(factory);
        self
    }

    /// The GPU runner for one tower, made (and checked against the CPU) on first use.
    fn ensure_gpu(&self, text: bool) -> Result<Arc<dyn Accelerator>, JobError> {
        let slot = if text { &self.gpu_text } else { &self.gpu_vision };
        let mut guard = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = guard.as_ref() {
            return r.clone().map_err(JobError::Device);
        }
        let made = self.make_gpu(text);
        if let Err(why) = &made {
            log::warn!("clip: the {} tower stays on the CPU: {why}", if text { "text" } else { "vision" });
        }
        *guard = Some(made.clone());
        made.map_err(JobError::Device)
    }

    fn make_gpu(&self, text: bool) -> Result<Arc<dyn Accelerator>, String> {
        let factory = self.accel.as_ref().ok_or("no GPU runner for CLIP in this build")?;
        let to_s = |e: JobError| match e {
            JobError::Device(s) | JobError::Failed(s) => s,
            JobError::Cancelled => "cancelled".into(),
        };
        // A full-tower test input through the CPU and GPU: two items (so batched indexing is covered too), checked before the runner is trusted.
        if text {
            let t = self.ensure_cpu_text().map_err(to_s)?;
            let w = TowerWeights::Text(text_tower_weights(&t, self.arch)?);
            let seq = self.arch.context_len;
            let vocab = self.arch.vocab_size.max(1);
            let ids: Vec<u32> = (0..2 * seq).map(|i| ((i * 7 + 1) % vocab) as u32).collect();
            let eot = [((seq / 2).min(seq.saturating_sub(1))) as u32, seq.saturating_sub(1) as u32];
            check_accel(
                factory,
                &w,
                |acc| acc.run_text(&ids, &eot, 2, seq),
                || run_cpu_text(&t, &ids, &eot, 2, seq, self.arch.text_hidden, None).map_err(to_s),
            )
        } else {
            let v = self.ensure_cpu_vision().map_err(to_s)?;
            let w = TowerWeights::Vision(vision_tower_weights(&v, self.arch, &self.manifest)?);
            let (h, img_w) = (self.manifest.input.height as usize, self.manifest.input.width as usize);
            let n = 3usize.checked_mul(h).and_then(|x| x.checked_mul(img_w)).ok_or("vision input too large")?;
            let img: Vec<f32> = (0..2 * n).map(|i| ((i as f32) * 0.618).sin()).collect();
            check_accel(factory, &w, |acc| acc.run_vision(&img, 2, h, img_w), || run_cpu_vision(&v, &img, 2, h, img_w, None).map_err(to_s))
        }
    }

    fn ensure_cpu_vision(&self) -> Result<Arc<VisionTower>, JobError> {
        let mut guard = self.cpu_vision.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(v) = guard.as_ref() {
            return Ok(Arc::clone(v));
        }
        let mut st = SafetensorsFile::open(&self.weights_path).map_err(|e| JobError::Failed(e.to_string()))?;
        let tower = Arc::new(load_vision_tower(&mut st, self.arch, CandleDevice::Cpu).map_err(|e| JobError::Failed(e.to_string()))?);
        *guard = Some(Arc::clone(&tower));
        Ok(tower)
    }

    fn ensure_cpu_text(&self) -> Result<Arc<TextTower>, JobError> {
        let mut guard = self.cpu_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(t) = guard.as_ref() {
            return Ok(Arc::clone(t));
        }
        let mut st = SafetensorsFile::open(&self.weights_path).map_err(|e| JobError::Failed(e.to_string()))?;
        let tower = Arc::new(load_text_tower(&mut st, self.arch, CandleDevice::Cpu).map_err(|e| JobError::Failed(e.to_string()))?);
        *guard = Some(Arc::clone(&tower));
        Ok(tower)
    }

    fn forward_images_on(&self, batch: &[RgbImage], ctx: &Ctx, gpu: Option<&dyn Accelerator>) -> Result<Vec<Vec<f32>>, JobError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let b = batch.len();
        let h = self.manifest.input.height as usize;
        let w = self.manifest.input.width as usize;
        let plane = 3 * h * w;
        let mut flat = Vec::with_capacity(b * plane);
        for img in batch {
            ctx.check()?;
            let chw = preprocess_image(img, &self.manifest.input)?;
            flat.extend_from_slice(&chw);
        }

        let raw = if let Some(acc) = gpu {
            ctx.check()?;
            acc.run_vision(&flat, b, h, w).map_err(JobError::Device)?
        } else {
            let vision = self.ensure_cpu_vision()?;
            run_cpu_vision(&vision, &flat, b, h, w, Some(ctx))?
        };
        if raw.len() != b * self.arch.embed_dim {
            let err = format!("vision encoder returned {} values, expected {}", raw.len(), b * self.arch.embed_dim);
            return Err(if gpu.is_some() { JobError::Device(err) } else { JobError::Failed(err) });
        }

        let mut out = Vec::with_capacity(b);
        for chunk in raw.chunks_exact(self.arch.embed_dim) {
            let mut vec = chunk.to_vec();
            if !l2_normalize(&mut vec) {
                let err = "vision encoder produced non-finite or zero-norm embedding".to_string();
                // from the GPU it is the device's fault (a driver passing the check on its test input, then not on a
                // photo): the CPU takes over instead of the photo failing
                return Err(if gpu.is_some() { JobError::Device(err) } else { JobError::Failed(err) });
            }
            out.push(vec);
        }
        Ok(out)
    }

    fn forward_text_on(&self, text: &str, ctx: &Ctx, gpu: Option<&dyn Accelerator>) -> Result<Vec<f32>, JobError> {
        ctx.check()?;
        let seq = self.arch.context_len;
        let prompts = if seq >= 16 { prompt_variants(text) } else { vec![text.to_string()] };
        let b = prompts.len().max(1);
        let mut flat_ids = Vec::with_capacity(b * seq);
        let mut eos_indices = Vec::with_capacity(b);
        for p in &prompts {
            let encoded = self.tokenizer.encode(p);
            eos_indices.push(encoded.eos_index.min(seq.saturating_sub(1)) as u32);
            flat_ids.extend_from_slice(&encoded.ids);
        }

        let raw = if let Some(acc) = gpu {
            ctx.check()?;
            acc.run_text(&flat_ids, &eos_indices, b, seq).map_err(JobError::Device)?
        } else {
            let text_tower = self.ensure_cpu_text()?;
            run_cpu_text(&text_tower, &flat_ids, &eos_indices, b, seq, self.arch.text_hidden, Some(ctx))?
        };
        if raw.len() != b * self.arch.embed_dim {
            let err = format!("text encoder returned {} values, expected {}", raw.len(), b * self.arch.embed_dim);
            return Err(if gpu.is_some() { JobError::Device(err) } else { JobError::Failed(err) });
        }

        let mut sum = vec![0.0f32; self.arch.embed_dim];
        for chunk in raw.chunks_exact(self.arch.embed_dim) {
            let mut vec = chunk.to_vec();
            if !l2_normalize(&mut vec) {
                let err = "text encoder produced non-finite or zero-norm embedding".to_string();
                return Err(if gpu.is_some() { JobError::Device(err) } else { JobError::Failed(err) });
            }
            for (s, &v) in sum.iter_mut().zip(vec.iter()) {
                *s += v;
            }
        }
        if !l2_normalize(&mut sum) {
            return Err(JobError::Failed("text encoder produced non-finite or zero-norm embedding".into()));
        }
        Ok(sum)
    }

    /// Write a tiny, deterministic CLIP checkpoint directory (`manifest.json`, `model.safetensors`, `vocab.json`,
    /// `merges.txt`) to `dir` for tests.
    ///
    /// The synthetic weights are constructed so that warm/red images and queries containing `"sunset"` or `"warm"`
    /// align along embedding axis 0 (`+1`), while cool/blue images and queries containing `"ocean"` or `"blue"` align
    /// along embedding axis 1 (`+1`).
    pub fn write_synthetic_dir(dir: &Path, model_id: &str) -> Result<ModelManifest, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let d = 8usize;
        let inter = 16usize;
        let embed_dim = 4usize;
        let patch = 8usize;
        let input_px = 16usize;
        let num_pos = 1 + (input_px / patch) * (input_px / patch); // 5
        let vocab_size = 16usize;
        let ctx_len = 8usize;

        let zeros_d = vec![0.0f32; d];
        let ones_d = vec![1.0f32; d];
        let zeros_dd = vec![0.0f32; d * d];
        let zeros_inter_d = vec![0.0f32; inter * d];
        let zeros_d_inter = vec![0.0f32; d * inter];
        let zeros_inter = vec![0.0f32; inter];
        let zeros_pos = vec![0.0f32; num_pos * d];

        // Patch embedding `[d, 3, patch, patch]`:
        // Make attention V/out add patch channel averages into CLS token via uniform attention.
        // Specifically: channel 0 (R) maps to dim 0, channel 2 (B) maps to dim 1.
        let mut patch_w = vec![0.0f32; d * 3 * patch * patch];
        let scale = 1.0f32 / ((patch * patch) as f32);
        for p in 0..(patch * patch) {
            patch_w[p] = scale;
            patch_w[3 * patch * patch + 2 * patch * patch + p] = scale;
        }

        // In the transformer block, let `v_proj` and `out_proj` be identity on dims 0..2 so the uniform
        // attention over `[CLS, patch_0..patch_3]` averages the patch R/B signals into the CLS token.
        let mut v_proj_w = vec![0.0f32; d * d];
        let mut out_proj_w = vec![0.0f32; d * d];
        v_proj_w[0] = 1.0;
        v_proj_w[d + 1] = 1.0;
        out_proj_w[0] = 1.0;
        out_proj_w[d + 1] = 1.0;

        // Visual projection `[embed_dim, d]`: map dim 0 -> axis 0, dim 1 -> axis 1
        let mut vis_proj = vec![0.0f32; embed_dim * d];
        vis_proj[0] = 1.0;
        vis_proj[d + 1] = 1.0;

        let mut cls_emb = vec![0.0f32; d];
        cls_emb[2] = 0.1;
        cls_emb[3] = -0.1;

        let mut tok_emb = vec![0.0f32; vocab_size * d];
        for tok in 0..vocab_size {
            tok_emb[tok * d + 2] = 0.1;
            tok_emb[tok * d + 3] = -0.1;
        }
        // Token 1 = "sunset</w>", Token 2 = "warm</w>" -> strong +dim 0
        tok_emb[d] = 2.0;
        tok_emb[2 * d] = 2.0;
        // Token 3 = "ocean</w>", Token 4 = "blue</w>" -> strong +dim 1
        tok_emb[3 * d + 1] = 2.0;
        tok_emb[4 * d + 1] = 2.0;

        let zeros_tpos = vec![0.0f32; ctx_len * d];
        let mut text_proj = vec![0.0f32; embed_dim * d];
        text_proj[0] = 1.0;
        text_proj[d + 1] = 1.0;
        // Map dim 2 (carried by the SOT/EOT residual) to axis 2 so unknown queries still
        // normalize to a valid unit vector orthogonal to image axes 0 and 1.
        text_proj[2 * d + 2] = 0.2;

        let st_bytes = encode_f32_safetensors(&[
            ("vision_model.embeddings.class_embedding", &[d], &cls_emb),
            ("vision_model.embeddings.patch_embedding.weight", &[d, 3, patch, patch], &patch_w),
            ("vision_model.embeddings.position_embedding.weight", &[num_pos, d], &zeros_pos),
            ("vision_model.pre_layrnorm.weight", &[d], &ones_d),
            ("vision_model.pre_layrnorm.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.layer_norm1.weight", &[d], &ones_d),
            ("vision_model.encoder.layers.0.layer_norm1.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.self_attn.q_proj.weight", &[d, d], &zeros_dd),
            ("vision_model.encoder.layers.0.self_attn.q_proj.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.self_attn.k_proj.weight", &[d, d], &zeros_dd),
            ("vision_model.encoder.layers.0.self_attn.k_proj.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.self_attn.v_proj.weight", &[d, d], &v_proj_w),
            ("vision_model.encoder.layers.0.self_attn.v_proj.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.self_attn.out_proj.weight", &[d, d], &out_proj_w),
            ("vision_model.encoder.layers.0.self_attn.out_proj.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.layer_norm2.weight", &[d], &ones_d),
            ("vision_model.encoder.layers.0.layer_norm2.bias", &[d], &zeros_d),
            ("vision_model.encoder.layers.0.mlp.fc1.weight", &[inter, d], &zeros_inter_d),
            ("vision_model.encoder.layers.0.mlp.fc1.bias", &[inter], &zeros_inter),
            ("vision_model.encoder.layers.0.mlp.fc2.weight", &[d, inter], &zeros_d_inter),
            ("vision_model.encoder.layers.0.mlp.fc2.bias", &[d], &zeros_d),
            ("vision_model.post_layernorm.weight", &[d], &ones_d),
            ("vision_model.post_layernorm.bias", &[d], &zeros_d),
            ("visual_projection.weight", &[embed_dim, d], &vis_proj),
            ("text_model.embeddings.token_embedding.weight", &[vocab_size, d], &tok_emb),
            ("text_model.embeddings.position_embedding.weight", &[ctx_len, d], &zeros_tpos),
            ("text_model.encoder.layers.0.layer_norm1.weight", &[d], &ones_d),
            ("text_model.encoder.layers.0.layer_norm1.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.self_attn.q_proj.weight", &[d, d], &zeros_dd),
            ("text_model.encoder.layers.0.self_attn.q_proj.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.self_attn.k_proj.weight", &[d, d], &zeros_dd),
            ("text_model.encoder.layers.0.self_attn.k_proj.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.self_attn.v_proj.weight", &[d, d], &v_proj_w),
            ("text_model.encoder.layers.0.self_attn.v_proj.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.self_attn.out_proj.weight", &[d, d], &out_proj_w),
            ("text_model.encoder.layers.0.self_attn.out_proj.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.layer_norm2.weight", &[d], &ones_d),
            ("text_model.encoder.layers.0.layer_norm2.bias", &[d], &zeros_d),
            ("text_model.encoder.layers.0.mlp.fc1.weight", &[inter, d], &zeros_inter_d),
            ("text_model.encoder.layers.0.mlp.fc1.bias", &[inter], &zeros_inter),
            ("text_model.encoder.layers.0.mlp.fc2.weight", &[d, inter], &zeros_d_inter),
            ("text_model.encoder.layers.0.mlp.fc2.bias", &[d], &zeros_d),
            ("text_model.final_layer_norm.weight", &[d], &ones_d),
            ("text_model.final_layer_norm.bias", &[d], &zeros_d),
            ("text_projection.weight", &[embed_dim, d], &text_proj),
        ]);

        let st_path = dir.join("model.safetensors");
        std::fs::write(&st_path, &st_bytes).map_err(|e| e.to_string())?;
        let (st_size, st_sha) = sha256_file(&st_path, u64::MAX).map_err(|e| e.to_string())?;

        let vocab_json = br#"{
            "<|startoftext|>": 0,
            "sunset</w>": 1,
            "warm</w>": 2,
            "ocean</w>": 3,
            "blue</w>": 4,
            "<|endoftext|>": 5
        }"#;
        let vocab_path = dir.join("vocab.json");
        std::fs::write(&vocab_path, vocab_json).map_err(|e| e.to_string())?;
        let (vocab_size_bytes, vocab_sha) = sha256_file(&vocab_path, u64::MAX).map_err(|e| e.to_string())?;

        let merges_txt = concat!(
            "#version: 0.2\n",
            "s u\nsu n\nsun s\nsuns e\nsunse t</w>\n",
            "w a\nwa r\nwar m</w>\n",
            "o c\noc e\noce a\nocea n</w>\n",
            "b l\nbl u\nblu e</w>\n"
        );
        let merges_path = dir.join("merges.txt");
        std::fs::write(&merges_path, merges_txt.as_bytes()).map_err(|e| e.to_string())?;
        let (merges_size_bytes, merges_sha) = sha256_file(&merges_path, u64::MAX).map_err(|e| e.to_string())?;

        let manifest = ModelManifest {
            version: MANIFEST_VERSION,
            id: model_id.to_string(),
            name: "Synthetic Test CLIP".into(),
            role: Role::Embedder,
            version_tag: "test-v1".into(),
            embed_dim: embed_dim as u32,
            input: InputSpec { width: input_px as u32, height: input_px as u32, mean: [0.5, 0.5, 0.5], std: [0.5, 0.5, 0.5] },
            files: vec![
                ModelFile { name: "model.safetensors".into(), sha256: st_sha, size_bytes: st_size },
                ModelFile { name: "vocab.json".into(), sha256: vocab_sha, size_bytes: vocab_size_bytes },
                ModelFile { name: "merges.txt".into(), sha256: merges_sha, size_bytes: merges_size_bytes },
            ],
            licence: Licence {
                spdx: "MIT".into(),
                commercial: Commercial::Allowed,
                url: "https://example.invalid/synthetic-clip".into(),
                attribution: "Synthetic test weights (MIT)".into(),
            },
            notes: "Synthetic test encoder".into(),
        };

        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(MANIFEST_FILE), manifest_bytes).map_err(|e| e.to_string())?;
        Ok(manifest)
    }
}

/// Expand a user search query into zero-shot CLIP photo prompt variants (Radford et al., ICML 2021 §3.1.4).
///
/// CLIP text towers are trained on image captions (`"a photo of …"`); bare nouns (`"human"`, `"dog"`, `"car"`)
/// otherwise sit in a narrower similarity band and can align with text/signage in images. Wrapping bare queries in
/// `"a photo of …"` / `"a photo of a …"` and averaging their unit text embeddings boosts visual matches while
/// pushing unrelated photos well below the filter threshold.
pub fn prompt_variants(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }
    let lower = trimmed.to_lowercase();
    for prefix in ["a photo of ", "photo of ", "photos of ", "an image of ", "image of ", "a picture of ", "picture of "] {
        if lower.starts_with(prefix) {
            return vec![trimmed.to_string()];
        }
    }
    let has_determiner = ["a ", "an ", "the ", "some ", "two ", "three ", "four ", "many ", "several "].iter().any(|p| lower.starts_with(p));
    let last_word = lower.split_whitespace().last().unwrap_or("");
    let is_plural = matches!(last_word, "people" | "men" | "women" | "children")
        || (last_word.len() > 3 && last_word.ends_with('s') && !last_word.ends_with("ss") && !last_word.ends_with("us"));
    if has_determiner || is_plural {
        if last_word == "people" || last_word == "humans" {
            return vec![format!("a photo of {trimmed}"), "a photo of a person".to_string()];
        }
        return vec![format!("a photo of {trimmed}")];
    }
    let article = if lower.chars().next().is_some_and(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')) { "an" } else { "a" };
    vec![format!("a photo of {trimmed}"), format!("a photo of {article} {trimmed}")]
}

impl Encoder for Clip {
    fn manifest(&self) -> &ModelManifest {
        &self.manifest
    }

    fn encode_images(&self, batch: &[RgbImage], ctx: &Ctx) -> Result<Vec<Vec<f32>>, JobError> {
        let gpu = if ctx.device().is_gpu() { Some(self.ensure_gpu(false)?) } else { None };
        self.forward_images_on(batch, ctx, gpu.as_deref())
    }

    fn encode_text(&self, text: &str, ctx: &Ctx) -> Result<Vec<f32>, JobError> {
        let gpu = if ctx.device().is_gpu() { Some(self.ensure_gpu(true)?) } else { None };
        self.forward_text_on(text, ctx, gpu.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::device::Preference;
    use crate::queue::{Config, Job, Outcome, Priority, Queue};
    use crate::store::cosine;

    fn wait_result<T: Send + 'static>(q: &Queue<T>, key: &str) -> T {
        let start = Instant::now();
        loop {
            for f in q.drain() {
                if f.key == key {
                    match f.outcome {
                        Outcome::Done(v) => return v,
                        other => panic!("unexpected job outcome for {key}: {:?}", other.map_unit()),
                    }
                }
            }
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {key}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    impl<T> Outcome<T> {
        fn map_unit(&self) -> Outcome<()> {
            match self {
                Outcome::Done(_) => Outcome::Done(()),
                Outcome::Failed(e) => Outcome::Failed(e.clone()),
                Outcome::Cancelled => Outcome::Cancelled,
            }
        }
    }

    #[test]
    fn loads_synthetic_clip_and_separates_warm_vs_cool_on_queue() {
        let dir = std::env::temp_dir().join(format!("lcc-ml-clip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = Clip::write_synthetic_dir(&dir, "clip-vit-b32-laion2b").unwrap();
        let clip = Arc::new(Clip::load(&dir).unwrap());
        assert_eq!(clip.manifest(), &manifest);

        let q = Queue::new(Config { preference: Preference::Cpu, ..Config::default() }, Box::new(|| Err("cpu test".into())), None);

        let mut red_rgb = Vec::with_capacity(16 * 16 * 3);
        let mut blue_rgb = Vec::with_capacity(16 * 16 * 3);
        for _ in 0..(16 * 16) {
            red_rgb.extend_from_slice(&[250, 30, 20]);
            blue_rgb.extend_from_slice(&[20, 40, 250]);
        }
        let red_img = RgbImage::new(16, 16, red_rgb).unwrap();
        let blue_img = RgbImage::new(16, 16, blue_rgb).unwrap();

        let clip_img = Arc::clone(&clip);
        q.submit(
            Job::new("embed-images", move |ctx| clip_img.encode_images(&[red_img.clone(), blue_img.clone()], ctx)).priority(Priority::Interactive),
        )
        .unwrap();
        let img_embs = wait_result(&q, "embed-images");
        assert_eq!(img_embs.len(), 2);

        let q_text = Queue::new(Config { preference: Preference::Cpu, ..Config::default() }, Box::new(|| Err("cpu test".into())), None);
        let clip_sunset = Arc::clone(&clip);
        q_text.submit(Job::new("embed-sunset", move |ctx| clip_sunset.encode_text("sunset", ctx)).priority(Priority::Interactive)).unwrap();
        let sunset_emb = wait_result(&q_text, "embed-sunset");

        let clip_ocean = Arc::clone(&clip);
        q_text.submit(Job::new("embed-ocean", move |ctx| clip_ocean.encode_text("ocean", ctx)).priority(Priority::Interactive)).unwrap();
        let ocean_emb = wait_result(&q_text, "embed-ocean");

        let sunset_red = cosine(&sunset_emb, &img_embs[0]);
        let sunset_blue = cosine(&sunset_emb, &img_embs[1]);
        let ocean_blue = cosine(&ocean_emb, &img_embs[1]);
        let ocean_red = cosine(&ocean_emb, &img_embs[0]);

        assert!(sunset_red > sunset_blue + 0.5, "expected sunset to match red ({sunset_red}) > blue ({sunset_blue})");
        assert!(ocean_blue > ocean_red + 0.5, "expected ocean to match blue ({ocean_blue}) > red ({ocean_red})");

        // Lazy loading defers tower weight reads to the worker thread and produces identical embeddings
        let lazy_clip = Arc::new(Clip::load_lazy(&dir).unwrap());
        let lazy_sunset = Arc::clone(&lazy_clip);
        q_text.submit(Job::new("lazy-sunset", move |ctx| lazy_sunset.encode_text("sunset", ctx)).priority(Priority::Interactive)).unwrap();
        let lazy_sunset_emb = wait_result(&q_text, "lazy-sunset");
        assert!((cosine(&sunset_emb, &lazy_sunset_emb) - 1.0).abs() < 1e-5);

        // Verify tamper detection on weights file
        let weights_path = dir.join("model.safetensors");
        let mut bytes = std::fs::read(&weights_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&weights_path, bytes).unwrap();
        assert!(matches!(Clip::load(&dir), Err(ClipError::Verify(VerifyError::HashMismatch { .. }))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A GPU runner that disagrees with the CPU is never used: the job moves to the CPU, which says why.
    #[test]
    fn a_gpu_runner_that_disagrees_with_the_cpu_falls_back() {
        struct Identity;
        impl Accelerator for Identity {
            fn device(&self) -> String {
                "identity".into()
            }
            fn run_vision(&self, _: &[f32], batch: usize, _: usize, _: usize) -> Result<Vec<f32>, String> {
                Ok(vec![0.0; batch * 4])
            }
            fn run_text(&self, _: &[u32], _: &[u32], batch: usize, _: usize) -> Result<Vec<f32>, String> {
                Ok(vec![0.0; batch * 4])
            }
            fn run_blocks(&self, x: &[f32], _: usize, _: usize) -> Result<Vec<f32>, String> {
                Ok(x.to_vec())
            }
        }
        let dir = std::env::temp_dir().join(format!("lcc-ml-clip-accel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Clip::write_synthetic_dir(&dir, "clip-accel").unwrap();
        let clip =
            Arc::new(Clip::load_lazy(&dir).unwrap().with_accelerator(Arc::new(|_: &TowerWeights| Ok(Arc::new(Identity) as Arc<dyn Accelerator>))));
        let q = Queue::new(Config { preference: Preference::Auto, ..Config::default() }, Box::new(|| Ok("Test GPU".into())), None);
        let c = Arc::clone(&clip);
        q.submit(Job::new("text", move |ctx| c.encode_text("sunset", ctx)).priority(Priority::Interactive)).unwrap();
        let emb = wait_result(&q, "text");
        assert!(emb.iter().all(|v| v.is_finite()));
        let device = q.status().device.unwrap();
        assert!(!device.is_gpu());
        assert!(device.why_cpu.unwrap_or_default().contains("differs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A GPU that gives NaN for a photo (after passing its check on the test input) hands the photo to the CPU: the
    /// error is the device's, not the photo's, so the queue retries it there instead of failing it.
    #[test]
    fn a_non_finite_gpu_embedding_is_a_device_error() {
        struct Nan(usize);
        impl Accelerator for Nan {
            fn device(&self) -> String {
                "nan".into()
            }
            fn run_vision(&self, _: &[f32], batch: usize, _: usize, _: usize) -> Result<Vec<f32>, String> {
                Ok(vec![f32::NAN; batch * self.0])
            }
            fn run_text(&self, _: &[u32], _: &[u32], batch: usize, _: usize) -> Result<Vec<f32>, String> {
                Ok(vec![f32::NAN; batch * self.0])
            }
        }
        let dir = std::env::temp_dir().join(format!("lcc-ml-clip-nan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Clip::write_synthetic_dir(&dir, "clip-nan").unwrap();
        let clip = Clip::load(&dir).unwrap();
        let gpu = Nan(clip.arch.embed_dim);
        let img = RgbImage::new(16, 16, vec![128; 16 * 16 * 3]).unwrap();
        let q = Queue::new(Config { preference: Preference::Cpu, ..Config::default() }, Box::new(|| Err("cpu test".into())), None);
        let clip = Arc::new(clip);
        let c = Arc::clone(&clip);
        q.submit(
            Job::new("nan", move |ctx| {
                let image = c.forward_images_on(std::slice::from_ref(&img), ctx, Some(&gpu));
                let text = c.forward_text_on("sunset", ctx, Some(&gpu));
                Ok((matches!(image, Err(JobError::Device(_))), matches!(text, Err(JobError::Device(_)))))
            })
            .priority(Priority::Interactive),
        )
        .unwrap();
        assert_eq!(wait_result(&q, "nan"), (true, true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_variants_wrap_bare_queries_and_keep_explicit_photo_prompts() {
        assert_eq!(prompt_variants("Human"), vec!["a photo of Human", "a photo of a Human"]);
        assert_eq!(prompt_variants("ocean"), vec!["a photo of ocean", "a photo of an ocean"]);
        assert_eq!(prompt_variants("dogs"), vec!["a photo of dogs"]);
        assert_eq!(prompt_variants("people"), vec!["a photo of people", "a photo of a person"]);
        assert_eq!(prompt_variants("a photo of a cat"), vec!["a photo of a cat"]);
    }
}
