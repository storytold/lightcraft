//! CLIP (OpenAI's ViT-B/32 and other `CLIPModel` checkpoints) image and text towers on candle.
//!
//! - **Image tower**: a ViT. Non-overlapping `patch × patch` patches are projected (the patch convolution, without
//!   bias, as one matrix product), a learned class token is put in front, learned position embeddings added, a layer
//!   norm (`pre_layrnorm`), pre-norm encoder blocks (quick-GELU MLPs), then the class token's state, layer-normed
//!   (`post_layernorm`) and projected (`visual_projection`, no bias), is the image embedding.
//! - **Text tower**: CLIP's byte-level BPE ([`crate::tokenizer::ClipBpe`]) to at most 77 tokens, token + learned
//!   position embeddings, the same blocks under a causal mask, a final layer norm, and the state at the end-of-text
//!   token, projected (`text_projection`, no bias). As the mask is causal, nothing after that token changes it: each
//!   batch runs only as long as its longest text.
//!
//! Both embeddings are L2-normalised. CLIP has a learned scale (`exp(logit_scale)`, 100) and no bias: its scores are
//! `sigmoid(scale · cos + offset)` with the manifest's calibrated offset (see [`crate::manifest::ScoreKind::Softmax`]).
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from the Python/PyTorch
//! CLIP code of Hugging Face Transformers (`src/transformers/models/clip`), Copyright The HuggingFace Team and OpenAI;
//! translated to Rust on candle and restructured. See NOTICE.

use std::path::Path;
use std::sync::Arc;

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use crate::accel::{Accel, Accelerator, Input, Pool, TowerPlan};
use crate::config::{ClipConfig, TextConfig, VisionConfig};
use crate::manifest::{Family, Manifest, ScoreKind, TokenizerKind};
use crate::nn::{
    Block, Lazy, block_plans, blocks, causal_mask, flat_rows, linear_no_bias, linear_plan, make_runner, norm_plan, read_bounded, rows, scalar,
};
use crate::tokenizer::clip_bpe::{self, ClipBpe};
use crate::tokenizer::{MAX_TEXT_BYTES, clip_text};
use crate::weights::{Backend, Weights};
use crate::{ContentModel, Error, MAX_BATCH, Result, Rgb8, Scoring, Towers, preprocess};

pub const WEIGHTS_FILE: &str = "model.safetensors";
pub const CONFIG_FILE: &str = "config.json";

/// The image tower (`CLIPVisionTransformer` + `visual_projection`).
struct VisionTower {
    config: VisionConfig,
    /// The patch convolution as a matrix `[C·P·P, D]`.
    patch_w: Tensor,
    class: Tensor,
    pos: Tensor,
    pre_ln: LayerNorm,
    blocks: Vec<Block>,
    post_ln: LayerNorm,
    proj: Linear,
}

impl VisionTower {
    fn new(c: &VisionConfig, projection: usize, vb: VarBuilder) -> Result<Self> {
        let (d, p, ch) = (c.hidden, c.patch, c.channels);
        let tower = vb.pp("vision_model");
        let emb = tower.pp("embeddings");
        let patch_w = emb.get((d, ch, p, p), "patch_embedding.weight")?.reshape((d, ch * p * p))?.t()?.contiguous()?;
        let n = c.grid() * c.grid() + 1;
        Ok(Self {
            config: c.clone(),
            patch_w,
            class: emb.get(d, "class_embedding")?,
            pos: emb.get((n, d), "position_embedding.weight")?,
            // (sic: the reference names it `pre_layrnorm`)
            pre_ln: candle_nn::layer_norm(d, c.eps, tower.pp("pre_layrnorm"))?,
            blocks: blocks(c.layers, d, c.intermediate, c.heads, c.eps, c.act, tower.clone())?,
            post_ln: candle_nn::layer_norm(d, c.eps, tower.pp("post_layernorm"))?,
            proj: linear_no_bias(d, projection, vb.pp("visual_projection"))?,
        })
    }

    /// `[B, C, S, S]` pixels → `[B, projection]` (not normalised).
    fn forward(&self, pixels: &Tensor) -> Result<Tensor> {
        let c = &self.config;
        let (b, ch, h, w) = pixels.dims4()?;
        if ch != c.channels || h != c.image || w != c.image {
            return Err(Error::Input(format!("pixels {ch}×{h}×{w}, the model takes {}×{}×{}", c.channels, c.image, c.image)));
        }
        let (g, p, d) = (c.grid(), c.patch, c.hidden);
        // [B, C, G, P, G, P] → [B, G, G, C, P, P] → [B, G², C·P·P]
        let patches = pixels.reshape((b, ch, g, p, g, p))?.permute((0, 2, 4, 1, 3, 5))?.contiguous()?.reshape((b, g * g, ch * p * p))?;
        let patches = patches.broadcast_matmul(&self.patch_w)?;
        let class = self.class.reshape((1, 1, d))?.broadcast_as((b, 1, d))?.contiguous()?;
        let x = Tensor::cat(&[&class, &patches], 1)?.broadcast_add(&self.pos)?;
        let mut x = self.pre_ln.forward(&x)?;
        for blk in &self.blocks {
            x = blk.forward(&x, None)?;
        }
        let pooled = self.post_ln.forward(&x.i((.., 0))?)?;
        Ok(self.proj.forward(&pooled)?)
    }
}

/// The text tower (`CLIPTextTransformer` + `text_projection`).
struct TextTower {
    config: TextConfig,
    pos: Tensor,
    blocks: Vec<Block>,
    final_ln: LayerNorm,
    proj: Linear,
}

const TOKEN_TABLE: &str = "text_model.embeddings.token_embedding.weight";

impl TextTower {
    fn new(c: &TextConfig, weights: &Weights, vb: VarBuilder) -> Result<Self> {
        // the token table is read a row at a time; check it now so a damaged file fails at load
        if weights.shape(TOKEN_TABLE) != Some(&[c.vocab, c.hidden][..]) {
            return Err(Error::Model(format!("{TOKEN_TABLE}: missing or not {} × {}", c.vocab, c.hidden)));
        }
        let tower = vb.pp("text_model");
        Ok(Self {
            config: c.clone(),
            pos: tower.get((c.context, c.hidden), "embeddings.position_embedding.weight")?,
            blocks: blocks(c.layers, c.hidden, c.intermediate, c.heads, c.eps, c.act, tower.clone())?,
            final_ln: candle_nn::layer_norm(c.hidden, c.eps, tower.pp("final_layer_norm"))?,
            proj: linear_no_bias(c.hidden, c.projection, vb.pp("text_projection"))?,
        })
    }

    /// Token ids `[B, L]` (row-major) and each text's end-of-text position → `[B, projection]` (not normalised).
    fn forward(&self, ids: &[u32], ends: &[usize], l: usize, weights: &Weights, device: &Device) -> Result<Tensor> {
        let b = ends.len();
        if l == 0 || l > self.config.context || ids.len() != b * l || ends.iter().any(|e| *e >= l) {
            return Err(Error::Input(format!("{} tokens for {b} texts of {l} (context {})", ids.len(), self.config.context)));
        }
        let tok = weights.read_rows(TOKEN_TABLE, ids)?.to_dtype(DType::F32)?.to_device(device)?.reshape((b, l, self.config.hidden))?;
        let mut x = tok.broadcast_add(&self.pos.narrow(0, 0, l)?)?;
        let mask = causal_mask(l, device)?;
        for blk in &self.blocks {
            x = blk.forward(&x, Some(&mask))?;
        }
        let x = self.final_ln.forward(&x)?;
        // each text's state at its end-of-text token
        let picked = ends.iter().enumerate().map(|(i, e)| x.i((i, *e))).collect::<candle_core::Result<Vec<_>>>()?;
        let pooled = Tensor::stack(&picked, 0)?;
        Ok(self.proj.forward(&pooled)?)
    }
}

/// The image tower as plain data, for a [`crate::accel::TowerRunner`]. The final layer norm is `post_layernorm`
/// (on every position: the class token's is the one picked).
fn image_plan(c: &VisionConfig, projection: usize) -> TowerPlan {
    let d = c.hidden;
    let n = c.grid() * c.grid();
    TowerPlan {
        name: "image",
        hidden: d,
        heads: c.heads,
        intermediate: c.intermediate,
        eps: c.eps as f32,
        act: c.act,
        causal: false,
        input: Input::Patches { patches: n, proj: linear_plan("vision_model.embeddings.patch_embedding", c.channels * c.patch * c.patch, d, false) },
        class: Some("vision_model.embeddings.class_embedding".into()),
        positions: "vision_model.embeddings.position_embedding.weight".into(),
        max_len: n + 1,
        pre_ln: Some(norm_plan("vision_model.pre_layrnorm")),
        blocks: block_plans("vision_model", c.layers, d, c.intermediate),
        final_ln: norm_plan("vision_model.post_layernorm"),
        pool: Pool::Pick,
        proj: Some(linear_plan("visual_projection", d, projection, false)),
        out: projection,
    }
}

/// The text tower as plain data, for a [`crate::accel::TowerRunner`].
fn text_plan(c: &TextConfig) -> TowerPlan {
    TowerPlan {
        name: "text",
        hidden: c.hidden,
        heads: c.heads,
        intermediate: c.intermediate,
        eps: c.eps as f32,
        act: c.act,
        causal: true,
        input: Input::Tokens,
        class: None,
        positions: "text_model.embeddings.position_embedding.weight".into(),
        max_len: c.context,
        pre_ln: None,
        blocks: block_plans("text_model", c.layers, c.hidden, c.intermediate),
        final_ln: norm_plan("text_model.final_layer_norm"),
        pool: Pool::Pick,
        proj: Some(linear_plan("text_projection", c.hidden, c.projection, false)),
        out: c.projection,
    }
}

/// A loaded CLIP model.
pub struct Clip {
    manifest: Manifest,
    config: ClipConfig,
    device: Device,
    weights: Arc<Weights>,
    towers: Towers,
    /// The candle towers: loaded with the model, or (when a runner runs the tower) only if the CPU has to take over.
    vision: Lazy<VisionTower>,
    text: Lazy<TextTower>,
    gpu_image: Option<Accel>,
    gpu_text: Option<Accel>,
    tokenizer: Option<ClipBpe>,
    /// `exp(logit_scale)`.
    scale: f32,
}

impl Clip {
    /// Load from `dir` (`model.safetensors`, `config.json`, and for the text tower `vocab.json` + `merges.txt`).
    pub fn load_on(dir: &Path, manifest: Manifest, towers: Towers, device: Device) -> Result<Self> {
        Self::load_with(dir, manifest, towers, device, None)
    }

    /// [`Clip::load_on`], running each tower on a runner `accel` makes when it can (the CPU, on `device`, otherwise).
    pub fn load_with(dir: &Path, manifest: Manifest, towers: Towers, device: Device, accel: Option<&Accelerator>) -> Result<Self> {
        if manifest.family != Family::Clip || manifest.text.tokenizer != TokenizerKind::ClipBpe || manifest.score.kind != ScoreKind::Softmax {
            return Err(Error::Model(format!("{}: not a CLIP manifest", manifest.id)));
        }
        if !towers.image && !towers.text {
            return Err(Error::Model("no tower to load".into()));
        }
        let text_files: &[&str] = if towers.text { &[clip_bpe::VOCAB_FILE, clip_bpe::MERGES_FILE] } else { &[] };
        for f in [WEIGHTS_FILE, CONFIG_FILE].iter().chain(text_files) {
            if !dir.join(f).is_file() {
                return Err(Error::Missing(
                    dir.to_path_buf(),
                    format!("{WEIGHTS_FILE}, {CONFIG_FILE}, {}, {}", clip_bpe::VOCAB_FILE, clip_bpe::MERGES_FILE),
                ));
            }
        }
        let config = ClipConfig::parse(&read_bounded(&dir.join(CONFIG_FILE), crate::config::MAX_CONFIG_BYTES as u64)?)?;
        if manifest.image.size as usize != config.vision.image {
            return Err(Error::Model(format!("the manifest's image size {} is not the model's {}", manifest.image.size, config.vision.image)));
        }
        if manifest.embedding.dim as usize != config.projection {
            return Err(Error::Model(format!("the manifest's embedding size {} is not the model's {}", manifest.embedding.dim, config.projection)));
        }
        if manifest.text.context as usize > config.text.context {
            return Err(Error::Model(format!("the manifest's context {} is longer than the model's {}", manifest.text.context, config.text.context)));
        }
        let weights = Weights::open(&dir.join(WEIGHTS_FILE))?;
        let scale = scalar(&weights, "logit_scale")?.clamp(-20.0, 20.0).exp();
        let mut m = Self {
            manifest,
            config,
            device,
            weights,
            towers,
            vision: Lazy::empty(),
            text: Lazy::empty(),
            gpu_image: None,
            gpu_text: None,
            tokenizer: None,
            scale,
        };
        if towers.image {
            m.gpu_image = make_runner(accel, || image_plan(&m.config.vision, m.config.projection), &m.weights);
            if m.gpu_image.is_none() {
                m.vision = Lazy::loaded(m.load_vision()?);
            }
        }
        if towers.text {
            let tok = ClipBpe::load(dir)?;
            if tok.vocab_size() > m.config.text.vocab {
                return Err(Error::Model(format!("the tokenizer has {} tokens, the model {}", tok.vocab_size(), m.config.text.vocab)));
            }
            m.tokenizer = Some(tok);
            let c = &m.config.text;
            // the token table is read a row at a time; check it now so a damaged file fails at load
            if m.weights.shape(TOKEN_TABLE) != Some(&[c.vocab, c.hidden][..]) {
                return Err(Error::Model(format!("{TOKEN_TABLE}: missing or not {} × {}", c.vocab, c.hidden)));
            }
            m.gpu_text = make_runner(accel, || text_plan(&m.config.text), &m.weights);
            if m.gpu_text.is_none() {
                m.text = Lazy::loaded(m.load_text()?);
            }
        }
        Ok(m)
    }

    fn var_builder(&self) -> VarBuilder<'static> {
        VarBuilder::from_backend(Box::new(Backend(self.weights.clone())), DType::F32, self.device.clone())
    }

    fn load_vision(&self) -> Result<VisionTower> {
        VisionTower::new(&self.config.vision, self.config.projection, self.var_builder())
    }

    fn load_text(&self) -> Result<TextTower> {
        TextTower::new(&self.config.text, &self.weights, self.var_builder())
    }

    pub fn config(&self) -> &ClipConfig {
        &self.config
    }

    /// `exp(logit_scale)`.
    pub fn logit_scale(&self) -> f32 {
        self.scale
    }

    /// The token ids the text tower gets for `text` (padded to the manifest's context) and how many are real.
    pub fn tokens(&self, text: &str) -> Result<(Vec<u32>, usize)> {
        let tok = self.tokenizer.as_ref().ok_or_else(|| Error::Model("the text tower is not loaded".into()))?;
        if text.trim().is_empty() {
            return Err(Error::Input("the text is empty".into()));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::Input(format!("the text is longer than {MAX_TEXT_BYTES} bytes")));
        }
        // (CLIP's tokenizer lower-cases whatever the manifest says)
        Ok(tok.encode(clip_text(text, MAX_TEXT_BYTES), self.manifest.text.context as usize))
    }

    /// Image embeddings from preprocessed pixels `[B, 3, S, S]`.
    pub fn embed_pixels(&self, pixels: &Tensor) -> Result<Vec<Vec<f32>>> {
        let (b, ..) = pixels.dims4()?;
        self.embed_pixel_values(pixels.flatten_all()?.to_dtype(DType::F32)?.to_vec1::<f32>()?, b)
    }

    /// Image embeddings from preprocessed pixels (`n` images `[3, S, S]`, one after the other): on the runner when there
    /// is one, else (or when it fails) on candle.
    fn embed_pixel_values(&self, px: Vec<f32>, n: usize) -> Result<Vec<Vec<f32>>> {
        if !self.towers.image {
            return Err(Error::Model("the image tower is not loaded".into()));
        }
        let (c, out) = (&self.config.vision, self.config.projection);
        if n == 0 || px.len() != n * c.channels * c.image * c.image {
            return Err(Error::Input(format!("{} pixel values for {n} images of {}×{}×{}", px.len(), c.channels, c.image, c.image)));
        }
        if let Some(g) = &self.gpu_image
            && let Some(v) = g.run(|| crate::accel::patches(&px, n, c.image, c.patch), n, out, &vec![0; n])
        {
            return Ok(flat_rows(v, out));
        }
        let vision = self.vision.get(|| self.load_vision())?;
        let t = Tensor::from_vec(px, (n, c.channels, c.image, c.image), &self.device)?;
        rows(&vision.forward(&t)?)
    }

    /// Text embeddings from token ids as [`Clip::tokens`] gives them (each padded to the same length, with its count
    /// of real tokens).
    pub fn embed_tokens(&self, texts: &[(Vec<u32>, usize)]) -> Result<Vec<Vec<f32>>> {
        if !self.towers.text {
            return Err(Error::Model("the text tower is not loaded".into()));
        }
        // only as long as the longest text: the causal mask keeps what follows a text's end out of it
        let l = texts.iter().map(|(_, n)| *n).max().unwrap_or(0);
        let mut ids = Vec::with_capacity(texts.len() * l);
        let mut ends = Vec::with_capacity(texts.len());
        for (t, n) in texts {
            let row = t.get(..l).ok_or_else(|| Error::Input(format!("{} tokens, {l} wanted", t.len())))?;
            ids.extend_from_slice(row);
            ends.push(n.saturating_sub(1));
        }
        let (b, c) = (ends.len(), &self.config.text);
        if l == 0 || l > c.context || ids.len() != b * l {
            return Err(Error::Input(format!("{} tokens for {b} texts of {l} (context {})", ids.len(), c.context)));
        }
        if let Some(g) = &self.gpu_text {
            let input =
                || -> Result<Vec<f32>> { Ok(self.weights.read_rows(TOKEN_TABLE, &ids)?.to_dtype(DType::F32)?.flatten_all()?.to_vec1::<f32>()?) };
            let pick: Vec<u32> = ends.iter().map(|e| *e as u32).collect();
            if let Some(v) = g.run(input, b, c.projection, &pick) {
                return Ok(flat_rows(v, c.projection));
            }
        }
        let text = self.text.get(|| self.load_text())?;
        rows(&text.forward(&ids, &ends, l, &self.weights, &self.device)?)
    }

    fn score_map(&self) -> Scoring {
        Scoring { scale: self.scale, bias: self.manifest.score.softmax_offset(), sigmoid: true }
    }

    fn runs_on_gpu(&self) -> Option<String> {
        let towers = [(self.towers.image, &self.gpu_image), (self.towers.text, &self.gpu_text)];
        let mut names = towers.iter().filter(|(on, _)| *on).map(|(_, g)| g.as_ref().and_then(|g| g.device()));
        let first = names.next()??;
        names.all(|n| n.as_deref() == Some(first.as_str())).then_some(first)
    }
}

impl ContentModel for Clip {
    fn embed_images(&self, batch: &[Rgb8]) -> Result<Vec<Vec<f32>>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        if batch.len() > MAX_BATCH {
            return Err(Error::Input(format!("{} images in one batch (at most {MAX_BATCH})", batch.len())));
        }
        if !self.towers.image {
            return Err(Error::Model("the image tower is not loaded".into()));
        }
        self.embed_pixel_values(preprocess::batch(batch, &self.manifest.image)?, batch.len())
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() > MAX_BATCH {
            return Err(Error::Input(format!("{} texts in one batch (at most {MAX_BATCH})", texts.len())));
        }
        let tokens = texts.iter().map(|t| self.tokens(t)).collect::<Result<Vec<_>>>()?;
        self.embed_tokens(&tokens)
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        self.score_map().score(cosine)
    }

    fn scoring(&self) -> Option<Scoring> {
        Some(self.score_map())
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn runs_on(&self) -> String {
        self.runs_on_gpu().map_or_else(|| crate::device_name(&self.device), |d| format!("GPU: {d}"))
    }

    fn gpu_fallback(&self) -> Option<String> {
        self.gpu_image.as_ref().and_then(Accel::failed).or_else(|| self.gpu_text.as_ref().and_then(Accel::failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::builtin_clip_vit_b32_openai;

    /// A model folder in the temp dir with a tiny CLIP config and the given tensors, to check loading and failures.
    struct Tiny {
        dir: std::path::PathBuf,
    }

    impl Drop for Tiny {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const CONFIG: &str = r#"{"model_type": "clip", "projection_dim": 4,
        "text_config": {"vocab_size": 12, "hidden_size": 8, "intermediate_size": 16, "num_hidden_layers": 1, "num_attention_heads": 2, "max_position_embeddings": 6},
        "vision_config": {"hidden_size": 8, "intermediate_size": 16, "num_hidden_layers": 1, "num_attention_heads": 2, "image_size": 16, "patch_size": 8}}"#;

    fn tensors(vocab: usize) -> Vec<(String, Vec<usize>, &'static str)> {
        let f = |n: &str, s: Vec<usize>| (n.to_string(), s, "F32");
        let mut t = vec![
            f("logit_scale", vec![]),
            f("vision_model.embeddings.class_embedding", vec![8]),
            f("vision_model.embeddings.patch_embedding.weight", vec![8, 3, 8, 8]),
            f("vision_model.embeddings.position_embedding.weight", vec![5, 8]),
            ("vision_model.embeddings.position_ids".to_string(), vec![1, 5], "I64"),
            f("vision_model.pre_layrnorm.weight", vec![8]),
            f("vision_model.pre_layrnorm.bias", vec![8]),
            f("vision_model.post_layernorm.weight", vec![8]),
            f("vision_model.post_layernorm.bias", vec![8]),
            f("visual_projection.weight", vec![4, 8]),
            f("text_model.embeddings.token_embedding.weight", vec![vocab, 8]),
            f("text_model.embeddings.position_embedding.weight", vec![6, 8]),
            f("text_model.final_layer_norm.weight", vec![8]),
            f("text_model.final_layer_norm.bias", vec![8]),
            f("text_projection.weight", vec![4, 8]),
        ];
        for tower in ["vision_model", "text_model"] {
            let l = format!("{tower}.encoder.layers.0");
            for n in ["layer_norm1", "layer_norm2"] {
                t.push(f(&format!("{l}.{n}.weight"), vec![8]));
                t.push(f(&format!("{l}.{n}.bias"), vec![8]));
            }
            for n in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                t.push(f(&format!("{l}.self_attn.{n}.weight"), vec![8, 8]));
                t.push(f(&format!("{l}.self_attn.{n}.bias"), vec![8]));
            }
            t.push(f(&format!("{l}.mlp.fc1.weight"), vec![16, 8]));
            t.push(f(&format!("{l}.mlp.fc1.bias"), vec![16]));
            t.push(f(&format!("{l}.mlp.fc2.weight"), vec![8, 16]));
            t.push(f(&format!("{l}.mlp.fc2.bias"), vec![8]));
        }
        t
    }

    const VOCAB: &str = r#"{"a</w>": 2, "b</w>": 3, "a": 4, "b": 5, "ab</w>": 6, "<|startoftext|>": 10, "<|endoftext|>": 11}"#;

    fn tiny(name: &str, config: &str, tensors: &[(String, Vec<usize>, &str)], vocab: &str) -> Tiny {
        let dir = std::env::temp_dir().join(format!("lc-vision-clip-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE), config).unwrap();
        let mut header = serde_json::Map::new();
        let mut off = 0usize;
        for (n, shape, dtype) in tensors {
            let len = shape.iter().product::<usize>() * if *dtype == "I64" { 8 } else { 4 };
            header.insert(n.clone(), serde_json::json!({"dtype": dtype, "shape": shape, "data_offsets": [off, off + len]}));
            off += len;
        }
        let h = serde_json::Value::Object(header).to_string();
        let mut b = (h.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(h.as_bytes());
        // small non-zero weights so layer norms see variance (logit_scale first: ln 100)
        b.extend((0..off / 4).flat_map(|i| if i == 0 { 100f32.ln() } else { ((i % 7) as f32 - 3.0) * 0.01 }.to_le_bytes()));
        std::fs::write(dir.join(WEIGHTS_FILE), b).unwrap();
        std::fs::write(dir.join(clip_bpe::VOCAB_FILE), vocab).unwrap();
        std::fs::write(dir.join(clip_bpe::MERGES_FILE), "#version: 0.2\na b</w>\n").unwrap();
        Tiny { dir }
    }

    fn manifest() -> Manifest {
        let mut m = builtin_clip_vit_b32_openai();
        m.image.size = 16;
        m.embedding.dim = 4;
        m.text.context = 6;
        m
    }

    fn load(t: &Tiny, towers: Towers) -> Result<Clip> {
        Clip::load_on(&t.dir, manifest(), towers, Device::Cpu)
    }

    #[test]
    fn a_tiny_model_embeds_images_and_texts() {
        let t = tiny("ok", CONFIG, &tensors(12), VOCAB);
        let m = load(&t, Towers::BOTH).unwrap();
        let img = Rgb8::from_fn(40, 30, |x, y| [x as u8 * 6, y as u8 * 8, 100]);
        let e = m.embed_images(&[img.clone(), img]).unwrap();
        assert_eq!((e.len(), e[0].len()), (2, 4));
        assert!((crate::dot(&e[0], &e[0]) - 1.0).abs() < 1e-4);
        assert_eq!(m.tokens("A B").unwrap(), (vec![10, 2, 3, 11, 11, 11], 4));
        assert_eq!(m.tokens("ab").unwrap(), (vec![10, 6, 11, 11, 11, 11], 3));
        // a text alone and in a batch with a longer one embed the same (the causal mask)
        let alone = m.embed_texts(&["ab"]).unwrap();
        let batch = m.embed_texts(&["a b a b", "ab"]).unwrap();
        assert!((crate::dot(&alone[0], &batch[1]) - 1.0).abs() < 1e-5);
        let s = m.score(&e[0], &batch[0]);
        assert!((0.0..=1.0).contains(&s));
        assert!((m.logit_scale() - 100.0).abs() < 1e-3);
        let sc = m.scoring().unwrap();
        assert_eq!((sc.scale, sc.bias, sc.sigmoid), (m.logit_scale(), crate::manifest::CLIP_B32_OFFSET, true));
        assert!(m.embed_images(&[]).unwrap().is_empty() && m.embed_texts(&[]).unwrap().is_empty());
    }

    #[test]
    fn clip_bounds_hold() {
        let t = tiny("bounds", CONFIG, &tensors(12), VOCAB);
        let m = load(&t, Towers::BOTH).unwrap();
        // a long text is cut to the context, keeping its end
        let (ids, n) = m.tokens(&"a ".repeat(500)).unwrap();
        assert_eq!((ids.len(), n, ids[5]), (6, 6, 11));
        assert_eq!(m.embed_texts(&[&"a ".repeat(500)]).unwrap().len(), 1);
        assert!(m.embed_texts(&[""]).is_err());
        assert!(m.embed_texts(&["  \t "]).is_err());
        assert!(m.embed_texts(&[&"a".repeat(MAX_TEXT_BYTES + 1)]).is_err());
        assert!(m.embed_texts(&vec!["a"; MAX_BATCH + 1]).is_err());
        assert!(m.embed_tokens(&[(vec![10, 2], 3)]).is_err(), "fewer ids than it says");
        assert!(m.embed_tokens(&[(vec![10, 2, 11, 11, 11, 11, 11, 11], 8)]).is_err(), "longer than the context");
        assert!(m.embed_tokens(&[(vec![10, 99, 11], 3)]).is_err(), "an id outside the table");
        // any image size and shape becomes the model's square; nonsense is refused
        for (w, h) in [(1, 1), (3, 700), (1000, 2), (16, 16)] {
            assert_eq!(m.embed_images(&[Rgb8::new(w, h)]).unwrap().len(), 1, "{w}×{h}");
        }
        assert!(m.embed_images(&[Rgb8 { width: 0, height: 0, data: vec![] }]).is_err());
        assert!(m.embed_images(&[Rgb8 { width: 4, height: 4, data: vec![[0; 3]; 3] }]).is_err());
        assert!(m.embed_images(&vec![Rgb8::new(2, 2); MAX_BATCH + 1]).is_err());
        assert!(m.embed_pixels(&Tensor::zeros((1, 3, 32, 32), DType::F32, &Device::Cpu).unwrap()).is_err(), "not 16 px");
    }

    #[test]
    fn broken_or_mismatched_models_are_errors() {
        // a vocabulary larger than the token table
        let big = r#"{"a": 4, "<|endoftext|>": 40}"#;
        let t = tiny("vocab", CONFIG, &tensors(12), big);
        assert!(load(&t, Towers::BOTH).err().unwrap().to_string().contains("tokenizer"));
        assert!(load(&t, Towers { image: true, text: false }).is_ok(), "the image tower does not need it");
        // a token table smaller than the config says
        let t = tiny("table", CONFIG, &tensors(10), VOCAB);
        assert!(load(&t, Towers::BOTH).is_err());
        // a missing projection
        let mut missing = tensors(12);
        missing.retain(|(n, _, _)| n != "visual_projection.weight");
        let t = tiny("missing", CONFIG, &missing, VOCAB);
        assert!(load(&t, Towers { image: true, text: false }).is_err());
        assert!(load(&t, Towers { image: false, text: true }).is_ok());
        // hostile config, a SigLIP config, a manifest that disagrees with the config
        let t = tiny("hostile", r#"{"model_type": "clip", "text_config": {"num_hidden_layers": 1000000}}"#, &tensors(12), VOCAB);
        assert!(load(&t, Towers::BOTH).is_err());
        let t = tiny("siglip", r#"{"model_type": "siglip"}"#, &tensors(12), VOCAB);
        assert!(load(&t, Towers::BOTH).err().unwrap().to_string().contains("not a CLIP model"));
        let t = tiny("manifest", CONFIG, &tensors(12), VOCAB);
        for edit in [|m: &mut Manifest| m.embedding.dim = 512, |m: &mut Manifest| m.image.size = 224, |m: &mut Manifest| m.text.context = 77] {
            let mut m = manifest();
            edit(&mut m);
            assert!(Clip::load_on(&t.dir, m, Towers::BOTH, Device::Cpu).is_err());
        }
        // a SigLIP manifest is not loaded as CLIP, nor CLIP's as SigLIP
        let siglip = crate::manifest::builtin_siglip2_b16_256();
        assert!(Clip::load_on(&t.dir, siglip, Towers::BOTH, Device::Cpu).is_err());
        assert!(crate::siglip::Siglip::load_on(&t.dir, manifest(), Towers::BOTH, Device::Cpu).is_err());
        assert!(Clip::load_on(&t.dir, manifest(), Towers { image: false, text: false }, Device::Cpu).is_err());
        // the tokenizer files missing (a SigLIP folder has tokenizer.json instead)
        let _ = std::fs::remove_file(t.dir.join(clip_bpe::MERGES_FILE));
        assert!(matches!(load(&t, Towers::BOTH), Err(Error::Missing(..))));
        assert!(load(&t, Towers { image: true, text: false }).is_ok());
        assert!(Clip::load_on(Path::new("/nonexistent/lc-vision-clip"), manifest(), Towers::BOTH, Device::Cpu).is_err());
    }
}
