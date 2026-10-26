//! SigLIP / SigLIP 2 (fixed resolution) image and text towers on candle.
//!
//! - **Image tower**: a ViT. Non-overlapping `patch × patch` patches are projected (the patch
//!   convolution, done as one matrix product), learned position embeddings added, then pre-norm
//!   encoder blocks (layer norm → multi-head self-attention → residual; layer norm → GELU MLP →
//!   residual), a final layer norm, and a multi-head attention pooling head: one learned probe
//!   query attends over all patches, followed by a layer norm + MLP residual. Its output is the
//!   image embedding.
//! - **Text tower**: token + learned position embeddings over the padded context (no attention
//!   mask, as the reference model is used), the same encoder blocks, a final layer norm, and a
//!   linear head on the last position's state.
//!
//! Both embeddings are L2-normalised; a pair's score is
//! `sigmoid(exp(logit_scale) · cos + logit_bias)` with the checkpoint's learned scale and bias.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SigLIP code of Hugging Face Transformers (`src/transformers/models/siglip`),
//! Copyright The HuggingFace Team and Google; translated to Rust on candle and restructured.
//! See NOTICE.

use std::path::Path;
use std::sync::Arc;

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use crate::config::{Act, SiglipConfig, TextConfig, VisionConfig};
use crate::manifest::{Family, Manifest, ScoreKind, TokenizerKind};
use crate::tokenizer::{MAX_TEXT_BYTES, SentencePiece};
use crate::weights::{Backend, Weights};
use crate::{ContentModel, Error, MAX_BATCH, Result, Rgb8, Towers, normalize, preprocess};

pub const WEIGHTS_FILE: &str = "model.safetensors";
pub const CONFIG_FILE: &str = "config.json";
pub const TOKENIZER_FILE: &str = "tokenizer.json";

/// Pre-norm multi-head self-attention (`SiglipAttention`).
struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
    head_dim: usize,
}

fn linear(i: usize, o: usize, vb: VarBuilder) -> Result<Linear> {
    Ok(candle_nn::linear(i, o, vb)?)
}

/// `[B, L, H·d]` → `[B, H, L, d]`.
fn split_heads(x: &Tensor, heads: usize, head_dim: usize) -> Result<Tensor> {
    let (b, l, _) = x.dims3()?;
    Ok(x.reshape((b, l, heads, head_dim))?.transpose(1, 2)?.contiguous()?)
}

/// Scaled dot-product attention of `[B, H, Lq, d]` queries over `[B, H, Lk, d]`, back to `[B, Lq, H·d]`.
fn attend(q: &Tensor, k: &Tensor, v: &Tensor) -> Result<Tensor> {
    let (b, h, lq, d) = q.dims4()?;
    let scale = (d as f64).powf(-0.5);
    let att = (q.matmul(&k.t()?)? * scale)?;
    let att = candle_nn::ops::softmax_last_dim(&att)?;
    Ok(att.matmul(v)?.transpose(1, 2)?.reshape((b, lq, h * d))?)
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

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let q = split_heads(&self.q.forward(x)?, self.heads, self.head_dim)?;
        let k = split_heads(&self.k.forward(x)?, self.heads, self.head_dim)?;
        let v = split_heads(&self.v.forward(x)?, self.heads, self.head_dim)?;
        Ok(self.o.forward(&attend(&q, &k, &v)?)?)
    }
}

/// fc1 → GELU → fc2 (`SiglipMLP`).
struct Mlp {
    fc1: Linear,
    fc2: Linear,
    act: Act,
}

impl Mlp {
    fn new(hidden: usize, inter: usize, act: Act, vb: VarBuilder) -> Result<Self> {
        Ok(Self { fc1: linear(hidden, inter, vb.pp("fc1"))?, fc2: linear(inter, hidden, vb.pp("fc2"))?, act })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.fc1.forward(x)?;
        let x = match self.act {
            Act::GeluTanh => x.gelu()?,
            Act::Gelu => x.gelu_erf()?,
        };
        Ok(self.fc2.forward(&x)?)
    }
}

/// A pre-norm encoder block (`SiglipEncoderLayer`).
struct Block {
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

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = (x + self.attn.forward(&self.ln1.forward(x)?)?)?;
        Ok((&x + self.mlp.forward(&self.ln2.forward(&x)?)?)?)
    }
}

fn blocks(n: usize, hidden: usize, inter: usize, heads: usize, eps: f64, act: Act, vb: VarBuilder) -> Result<Vec<Block>> {
    (0..n).map(|i| Block::new(hidden, inter, heads, eps, act, vb.pp(format!("encoder.layers.{i}")))).collect()
}

/// The attention-pooling head (`SiglipMultiheadAttentionPoolingHead`).
struct PoolHead {
    probe: Tensor,
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
    ln: LayerNorm,
    mlp: Mlp,
}

impl PoolHead {
    fn new(c: &VisionConfig, vb: VarBuilder) -> Result<Self> {
        let d = c.hidden;
        let probe = vb.get((1, 1, d), "probe")?;
        // PyTorch's nn.MultiheadAttention packs q, k, v into one projection
        let att = vb.pp("attention");
        let w = att.get((3 * d, d), "in_proj_weight")?;
        let b = att.get(3 * d, "in_proj_bias")?;
        let part = |i: usize| -> Result<Linear> { Ok(Linear::new(w.narrow(0, i * d, d)?, Some(b.narrow(0, i * d, d)?))) };
        Ok(Self {
            probe,
            q: part(0)?,
            k: part(1)?,
            v: part(2)?,
            o: linear(d, d, att.pp("out_proj"))?,
            heads: c.heads,
            ln: candle_nn::layer_norm(d, c.eps, vb.pp("layernorm"))?,
            mlp: Mlp::new(d, c.intermediate, c.act, vb.pp("mlp"))?,
        })
    }

    /// `[B, L, D]` → `[B, D]`.
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (b, _, d) = x.dims3()?;
        let hd = d / self.heads.max(1);
        let probe = self.probe.broadcast_as((b, 1, d))?.contiguous()?;
        let q = split_heads(&self.q.forward(&probe)?, self.heads, hd)?;
        let k = split_heads(&self.k.forward(x)?, self.heads, hd)?;
        let v = split_heads(&self.v.forward(x)?, self.heads, hd)?;
        let h = self.o.forward(&attend(&q, &k, &v)?)?;
        let h = (&h + self.mlp.forward(&self.ln.forward(&h)?)?)?;
        Ok(h.i((.., 0))?)
    }
}

/// The image tower (`SiglipVisionTransformer`).
struct VisionTower {
    config: VisionConfig,
    /// The patch convolution as a matrix `[C·P·P, D]`.
    patch_w: Tensor,
    patch_b: Tensor,
    pos: Tensor,
    blocks: Vec<Block>,
    post_ln: LayerNorm,
    head: PoolHead,
}

impl VisionTower {
    fn new(c: &VisionConfig, vb: VarBuilder) -> Result<Self> {
        let (d, p, ch) = (c.hidden, c.patch, c.channels);
        let emb = vb.pp("embeddings");
        let patch_w = emb.get((d, ch, p, p), "patch_embedding.weight")?.reshape((d, ch * p * p))?.t()?.contiguous()?;
        let patch_b = emb.get(d, "patch_embedding.bias")?;
        let n = c.grid() * c.grid();
        let pos = emb.get((n, d), "position_embedding.weight")?;
        Ok(Self {
            config: c.clone(),
            patch_w,
            patch_b,
            pos,
            blocks: blocks(c.layers, d, c.intermediate, c.heads, c.eps, c.act, vb.clone())?,
            post_ln: candle_nn::layer_norm(d, c.eps, vb.pp("post_layernorm"))?,
            head: PoolHead::new(c, vb.pp("head"))?,
        })
    }

    /// `[B, C, S, S]` pixels → `[B, D]` (not normalised).
    fn forward(&self, pixels: &Tensor) -> Result<Tensor> {
        let c = &self.config;
        let (b, ch, h, w) = pixels.dims4()?;
        if ch != c.channels || h != c.image || w != c.image {
            return Err(Error::Input(format!("pixels {ch}×{h}×{w}, the model takes {}×{}×{}", c.channels, c.image, c.image)));
        }
        let (g, p) = (c.grid(), c.patch);
        // [B, C, G, P, G, P] → [B, G, G, C, P, P] → [B, G², C·P·P]
        let patches = pixels.reshape((b, ch, g, p, g, p))?.permute((0, 2, 4, 1, 3, 5))?.contiguous()?.reshape((b, g * g, ch * p * p))?;
        let x = patches.broadcast_matmul(&self.patch_w)?.broadcast_add(&self.patch_b)?.broadcast_add(&self.pos)?;
        let mut x = x;
        for blk in &self.blocks {
            x = blk.forward(&x)?;
        }
        let x = self.post_ln.forward(&x)?;
        self.head.forward(&x)
    }
}

/// The text tower (`SiglipTextTransformer`).
struct TextTower {
    config: TextConfig,
    pos: Tensor,
    blocks: Vec<Block>,
    final_ln: LayerNorm,
    head: Linear,
}

const TOKEN_TABLE: &str = "text_model.embeddings.token_embedding.weight";

impl TextTower {
    fn new(c: &TextConfig, weights: &Weights, vb: VarBuilder) -> Result<Self> {
        // the token table stays on disk; check it now so a damaged file fails at load
        if weights.shape(TOKEN_TABLE) != Some(&[c.vocab, c.hidden][..]) {
            return Err(Error::Model(format!("{TOKEN_TABLE}: missing or not {} × {}", c.vocab, c.hidden)));
        }
        Ok(Self {
            config: c.clone(),
            pos: vb.get((c.context, c.hidden), "embeddings.position_embedding.weight")?,
            blocks: blocks(c.layers, c.hidden, c.intermediate, c.heads, c.eps, c.act, vb.clone())?,
            final_ln: candle_nn::layer_norm(c.hidden, c.eps, vb.pp("final_layer_norm"))?,
            head: linear(c.hidden, c.projection, vb.pp("head"))?,
        })
    }

    /// Token ids `[B, L]` (row-major) → `[B, projection]` (not normalised).
    fn forward(&self, ids: &[u32], b: usize, l: usize, weights: &Weights, device: &Device) -> Result<Tensor> {
        if l == 0 || l > self.config.context || ids.len() != b * l {
            return Err(Error::Input(format!("{} tokens for {b} texts of {l} (context {})", ids.len(), self.config.context)));
        }
        let tok = weights.read_rows(TOKEN_TABLE, ids)?.to_dtype(DType::F32)?.to_device(device)?.reshape((b, l, self.config.hidden))?;
        let mut x = tok.broadcast_add(&self.pos.narrow(0, 0, l)?)?;
        for blk in &self.blocks {
            x = blk.forward(&x)?;
        }
        let x = self.final_ln.forward(&x)?;
        // the last position's state (the reference pools the final token of the padded context)
        Ok(self.head.forward(&x.i((.., l - 1))?)?)
    }
}

/// A loaded SigLIP model.
pub struct Siglip {
    manifest: Manifest,
    config: SiglipConfig,
    device: Device,
    weights: Arc<Weights>,
    vision: Option<VisionTower>,
    text: Option<TextTower>,
    tokenizer: Option<SentencePiece>,
    /// `exp(logit_scale)`.
    scale: f32,
    bias: f32,
}

fn scalar(weights: &Weights, name: &str) -> Result<f32> {
    let t = weights.read(name)?.to_dtype(DType::F32)?.flatten_all()?;
    let v = t.to_vec1::<f32>()?;
    match v[..] {
        [x] if x.is_finite() => Ok(x),
        _ => Err(Error::Model(format!("{name}: not a finite scalar"))),
    }
}

impl Siglip {
    /// Load from `dir` (`model.safetensors`, `config.json`, and for the text tower `tokenizer.json`).
    pub fn load_on(dir: &Path, manifest: Manifest, towers: Towers, device: Device) -> Result<Self> {
        if manifest.family != Family::Siglip || manifest.text.tokenizer != TokenizerKind::Sentencepiece {
            return Err(Error::Model(format!("{}: not a SigLIP manifest", manifest.id)));
        }
        if !towers.image && !towers.text {
            return Err(Error::Model("no tower to load".into()));
        }
        for f in [WEIGHTS_FILE, CONFIG_FILE].iter().chain(if towers.text { &[TOKENIZER_FILE][..] } else { &[] }) {
            if !dir.join(f).is_file() {
                return Err(Error::Missing(dir.to_path_buf(), format!("{WEIGHTS_FILE}, {CONFIG_FILE}, {TOKENIZER_FILE}")));
            }
        }
        let cfg_bytes = read_bounded(&dir.join(CONFIG_FILE), crate::config::MAX_CONFIG_BYTES as u64)?;
        let config = SiglipConfig::parse(&cfg_bytes)?;
        if manifest.image.size as usize != config.vision.image {
            return Err(Error::Model(format!("the manifest's image size {} is not the model's {}", manifest.image.size, config.vision.image)));
        }
        if manifest.embedding.dim as usize != config.vision.hidden {
            return Err(Error::Model(format!(
                "the manifest's embedding size {} is not the model's {}",
                manifest.embedding.dim, config.vision.hidden
            )));
        }
        if manifest.text.context as usize > config.text.context {
            return Err(Error::Model(format!("the manifest's context {} is longer than the model's {}", manifest.text.context, config.text.context)));
        }
        let weights = Weights::open(&dir.join(WEIGHTS_FILE))?;
        let vb = VarBuilder::from_backend(Box::new(Backend(weights.clone())), DType::F32, device.clone());
        let vision = if towers.image { Some(VisionTower::new(&config.vision, vb.pp("vision_model"))?) } else { None };
        let (text, tokenizer) = if towers.text {
            let tok = SentencePiece::load(dir)?;
            if tok.vocab_size() > config.text.vocab {
                return Err(Error::Model(format!("the tokenizer has {} tokens, the model {}", tok.vocab_size(), config.text.vocab)));
            }
            (Some(TextTower::new(&config.text, &weights, vb.pp("text_model"))?), Some(tok))
        } else {
            (None, None)
        };
        let logit_scale = scalar(&weights, "logit_scale")?;
        let bias = scalar(&weights, "logit_bias")?;
        let scale = logit_scale.clamp(-20.0, 20.0).exp();
        Ok(Self { manifest, config, device, weights, vision, text, tokenizer, scale, bias })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn config(&self) -> &SiglipConfig {
        &self.config
    }

    /// `exp(logit_scale)` and `logit_bias`.
    pub fn logit_scale_bias(&self) -> (f32, f32) {
        (self.scale, self.bias)
    }

    /// The padded token ids the text tower gets for `text` (after the manifest's lower-casing).
    pub fn tokens(&self, text: &str) -> Result<Vec<u32>> {
        let tok = self.tokenizer.as_ref().ok_or_else(|| Error::Model("the text tower is not loaded".into()))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(Error::Input("the text is empty".into()));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::Input(format!("the text is longer than {MAX_TEXT_BYTES} bytes")));
        }
        let text = if self.manifest.text.lowercase { text.to_lowercase() } else { text.to_string() };
        Ok(tok.encode_padded(&text, self.manifest.text.context as usize))
    }

    /// Image embeddings from preprocessed pixels `[B, 3, S, S]`.
    pub fn embed_pixels(&self, pixels: &Tensor) -> Result<Vec<Vec<f32>>> {
        let vision = self.vision.as_ref().ok_or_else(|| Error::Model("the image tower is not loaded".into()))?;
        rows(&vision.forward(&pixels.to_device(&self.device)?)?)
    }

    /// Text embeddings from padded token ids (`n × context`, row-major).
    pub fn embed_tokens(&self, ids: &[u32], n: usize) -> Result<Vec<Vec<f32>>> {
        let text = self.text.as_ref().ok_or_else(|| Error::Model("the text tower is not loaded".into()))?;
        let l = ids.len().checked_div(n).unwrap_or(0);
        rows(&text.forward(ids, n, l, &self.weights, &self.device)?)
    }
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?.len();
    if len > max {
        return Err(Error::Model(format!("{} is larger than {max} bytes", path.display())));
    }
    std::fs::read(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))
}

/// `[B, D]` → normalised rows.
fn rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let mut out = t.to_dtype(DType::F32)?.to_device(&Device::Cpu)?.to_vec2::<f32>()?;
    for r in &mut out {
        if r.iter().any(|v| !v.is_finite()) {
            return Err(Error::Model("the model produced a non-finite embedding".into()));
        }
        normalize(r);
    }
    Ok(out)
}

impl ContentModel for Siglip {
    fn embed_images(&self, batch: &[Rgb8]) -> Result<Vec<Vec<f32>>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        if batch.len() > MAX_BATCH {
            return Err(Error::Input(format!("{} images in one batch (at most {MAX_BATCH})", batch.len())));
        }
        if self.vision.is_none() {
            return Err(Error::Model("the image tower is not loaded".into()));
        }
        let s = self.manifest.image.size as usize;
        let mut px = Vec::with_capacity(batch.len() * 3 * s * s);
        for img in batch {
            px.extend(preprocess::pixels(img, &self.manifest.image)?);
        }
        let px = Tensor::from_vec(px, (batch.len(), 3, s, s), &self.device)?;
        self.embed_pixels(&px)
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() > MAX_BATCH {
            return Err(Error::Input(format!("{} texts in one batch (at most {MAX_BATCH})", texts.len())));
        }
        let mut ids = Vec::new();
        for t in texts {
            ids.extend(self.tokens(t)?);
        }
        self.embed_tokens(&ids, texts.len())
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        let logit = self.scale * cosine;
        match self.manifest.score.kind {
            ScoreKind::Sigmoid => 1.0 / (1.0 + (-(logit + self.bias)).exp()),
            ScoreKind::Softmax => logit,
        }
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::builtin_siglip2_b16_256;

    /// A model folder in the temp dir with a tiny config and the given tensors (all F32 zeros
    /// unless given), to check loading fails cleanly.
    struct Tiny {
        dir: std::path::PathBuf,
    }

    impl Drop for Tiny {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const CONFIG: &str = r#"{"model_type": "siglip",
        "text_config": {"vocab_size": 8, "hidden_size": 8, "intermediate_size": 16, "num_hidden_layers": 1, "num_attention_heads": 2, "max_position_embeddings": 4},
        "vision_config": {"hidden_size": 8, "intermediate_size": 16, "num_hidden_layers": 1, "num_attention_heads": 2, "image_size": 16, "patch_size": 8}}"#;

    fn tensors(vocab: usize) -> Vec<(String, Vec<usize>)> {
        let mut t: Vec<(String, Vec<usize>)> = vec![
            ("logit_scale".into(), vec![1]),
            ("logit_bias".into(), vec![1]),
            ("vision_model.embeddings.patch_embedding.weight".into(), vec![8, 3, 8, 8]),
            ("vision_model.embeddings.patch_embedding.bias".into(), vec![8]),
            ("vision_model.embeddings.position_embedding.weight".into(), vec![4, 8]),
            ("vision_model.post_layernorm.weight".into(), vec![8]),
            ("vision_model.post_layernorm.bias".into(), vec![8]),
            ("vision_model.head.probe".into(), vec![1, 1, 8]),
            ("vision_model.head.attention.in_proj_weight".into(), vec![24, 8]),
            ("vision_model.head.attention.in_proj_bias".into(), vec![24]),
            ("vision_model.head.attention.out_proj.weight".into(), vec![8, 8]),
            ("vision_model.head.attention.out_proj.bias".into(), vec![8]),
            ("vision_model.head.layernorm.weight".into(), vec![8]),
            ("vision_model.head.layernorm.bias".into(), vec![8]),
            ("vision_model.head.mlp.fc1.weight".into(), vec![16, 8]),
            ("vision_model.head.mlp.fc1.bias".into(), vec![16]),
            ("vision_model.head.mlp.fc2.weight".into(), vec![8, 16]),
            ("vision_model.head.mlp.fc2.bias".into(), vec![8]),
            ("text_model.embeddings.token_embedding.weight".into(), vec![vocab, 8]),
            ("text_model.embeddings.position_embedding.weight".into(), vec![4, 8]),
            ("text_model.final_layer_norm.weight".into(), vec![8]),
            ("text_model.final_layer_norm.bias".into(), vec![8]),
            ("text_model.head.weight".into(), vec![8, 8]),
            ("text_model.head.bias".into(), vec![8]),
        ];
        for tower in ["vision_model", "text_model"] {
            let l = format!("{tower}.encoder.layers.0");
            for n in ["layer_norm1", "layer_norm2"] {
                t.push((format!("{l}.{n}.weight"), vec![8]));
                t.push((format!("{l}.{n}.bias"), vec![8]));
            }
            for n in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                t.push((format!("{l}.self_attn.{n}.weight"), vec![8, 8]));
                t.push((format!("{l}.self_attn.{n}.bias"), vec![8]));
            }
            t.push((format!("{l}.mlp.fc1.weight"), vec![16, 8]));
            t.push((format!("{l}.mlp.fc1.bias"), vec![16]));
            t.push((format!("{l}.mlp.fc2.weight"), vec![8, 16]));
            t.push((format!("{l}.mlp.fc2.bias"), vec![8]));
        }
        t
    }

    fn tiny(name: &str, config: &str, tensors: &[(String, Vec<usize>)], truncate: usize) -> Tiny {
        let dir = std::env::temp_dir().join(format!("lc-vision-siglip-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE), config).unwrap();
        let mut header = serde_json::Map::new();
        let mut off = 0usize;
        for (n, shape) in tensors {
            let len = shape.iter().product::<usize>() * 4;
            header.insert(n.clone(), serde_json::json!({"dtype": "F32", "shape": shape, "data_offsets": [off, off + len]}));
            off += len;
        }
        let h = serde_json::Value::Object(header).to_string();
        let mut b = (h.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(h.as_bytes());
        // small non-zero weights so layer norms see variance
        b.extend((0..off / 4).flat_map(|i| (((i % 7) as f32 - 3.0) * 0.01).to_le_bytes()));
        b.truncate(b.len() - truncate);
        std::fs::write(dir.join(WEIGHTS_FILE), b).unwrap();
        let tok = r#"{"added_tokens": [{"id": 0, "content": "<pad>"}, {"id": 1, "content": "<eos>"}],
            "normalizer": {"type": "Replace", "pattern": {"String": " "}, "content": "▁"},
            "post_processor": {"type": "TemplateProcessing", "single": [{"Sequence": {"id": "A", "type_id": 0}}, {"SpecialToken": {"id": "<eos>", "type_id": 0}}],
                               "special_tokens": {"<eos>": {"id": "<eos>", "ids": [1], "tokens": ["<eos>"]}}},
            "model": {"type": "BPE", "vocab": {"<pad>": 0, "<eos>": 1, "a": 2, "b": 3, "▁": 4, "ab": 5}, "merges": [["a", "b"]], "unk_token": null}}"#;
        std::fs::write(dir.join(TOKENIZER_FILE), tok).unwrap();
        Tiny { dir }
    }

    fn manifest() -> Manifest {
        let mut m = builtin_siglip2_b16_256();
        m.image.size = 16;
        m.embedding.dim = 8;
        m.text.context = 4;
        m
    }

    fn load(t: &Tiny) -> Result<Siglip> {
        Siglip::load_on(&t.dir, manifest(), Towers::BOTH, Device::Cpu)
    }

    #[test]
    fn a_tiny_model_embeds_images_and_texts() {
        let t = tiny("ok", CONFIG, &tensors(8), 0);
        let m = load(&t).unwrap();
        let img = Rgb8::from_fn(40, 30, |x, y| [x as u8 * 6, y as u8 * 8, 100]);
        let e = m.embed_images(&[img.clone(), img]).unwrap();
        assert_eq!((e.len(), e[0].len()), (2, 8));
        assert!((crate::dot(&e[0], &e[0]) - 1.0).abs() < 1e-4);
        assert_eq!(m.tokens("AB").unwrap(), vec![5, 1, 0, 0]);
        let tx = m.embed_texts(&["ab", "a b"]).unwrap();
        assert_eq!((tx.len(), tx[1].len()), (2, 8));
        let s = m.score(&e[0], &tx[0]);
        assert!((0.0..=1.0).contains(&s));
        assert!(m.embed_images(&[]).unwrap().is_empty());
    }

    #[test]
    fn bad_inputs_are_errors() {
        let t = tiny("inputs", CONFIG, &tensors(8), 0);
        let m = load(&t).unwrap();
        assert!(m.embed_texts(&[""]).is_err());
        assert!(m.embed_texts(&["   "]).is_err());
        assert!(m.embed_texts(&[&"a".repeat(MAX_TEXT_BYTES + 1)]).is_err());
        assert!(m.embed_texts(&vec!["a"; MAX_BATCH + 1]).is_err());
        assert!(m.embed_images(&[Rgb8 { width: 0, height: 0, data: vec![] }]).is_err());
        assert!(m.embed_images(&[Rgb8 { width: 70_000, height: 1, data: vec![] }]).is_err());
        assert!(m.embed_images(&[Rgb8 { width: 4, height: 4, data: vec![[0; 3]; 3] }]).is_err());
        let one = Rgb8::from_fn(1, 1, |_, _| [9, 9, 9]);
        assert_eq!(m.embed_images(&[one]).unwrap().len(), 1);
        assert!(m.embed_images(&vec![Rgb8::new(2, 2); MAX_BATCH + 1]).is_err());
        assert!(m.embed_tokens(&[0, 1, 2], 2).is_err());
        assert!(m.embed_tokens(&[7, 7, 7, 99], 1).is_err());
    }

    #[test]
    fn broken_models_are_errors() {
        // truncated download
        let t = tiny("trunc", CONFIG, &tensors(8), 100);
        assert!(load(&t).err().unwrap().to_string().contains("outside the file"));
        // a missing tensor
        let mut missing = tensors(8);
        missing.retain(|(n, _)| !n.contains("head.probe"));
        let t = tiny("missing", CONFIG, &missing, 0);
        assert!(load(&t).is_err());
        // a wrong shape
        let mut wrong = tensors(8);
        if let Some(e) = wrong.iter_mut().find(|(n, _)| n.ends_with("layers.0.mlp.fc1.weight")) {
            e.1 = vec![16, 9];
        }
        let t = tiny("shape", CONFIG, &wrong, 0);
        assert!(load(&t).err().unwrap().to_string().contains("shape"));
        // a token table smaller than the config says
        let t = tiny("vocab", CONFIG, &tensors(6), 0);
        assert!(load(&t).is_err());
        // hostile config
        let t = tiny("config", r#"{"text_config": {"num_hidden_layers": 1000000}}"#, &tensors(8), 0);
        assert!(load(&t).is_err());
        // manifest that disagrees with the config
        let t = tiny("manifest", CONFIG, &tensors(8), 0);
        let mut m = manifest();
        m.embedding.dim = 768;
        assert!(Siglip::load_on(&t.dir, m, Towers::BOTH, Device::Cpu).is_err());
        assert!(Siglip::load_on(&t.dir, manifest(), Towers { image: false, text: false }, Device::Cpu).is_err());
        // missing folder
        assert!(Siglip::load_on(Path::new("/nonexistent/lc-vision"), manifest(), Towers::BOTH, Device::Cpu).is_err());
    }

    #[test]
    fn one_tower_alone_loads_and_the_other_reports_it() {
        let t = tiny("image-only", CONFIG, &tensors(8), 0);
        let m = Siglip::load_on(&t.dir, manifest(), Towers { image: true, text: false }, Device::Cpu).unwrap();
        assert!(m.embed_texts(&["a"]).is_err());
        assert_eq!(m.embed_images(&[Rgb8::new(3, 3)]).unwrap().len(), 1);
        let m = Siglip::load_on(&t.dir, manifest(), Towers { image: false, text: true }, Device::Cpu).unwrap();
        assert!(m.embed_images(&[Rgb8::new(3, 3)]).is_err());
        assert_eq!(m.embed_texts(&["a"]).unwrap().len(), 1);
    }
}
