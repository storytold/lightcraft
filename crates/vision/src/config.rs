//! A SigLIP or CLIP model's `config.json` (Hugging Face format): the tower sizes, with the defaults of
//! `SiglipTextConfig` / `SiglipVisionConfig` (or `CLIPTextConfig` / `CLIPVisionConfig`) for what the file leaves out,
//! checked against bounds before anything is allocated from them.

use serde::Deserialize;

use crate::{Error, Result};

/// Largest `config.json` we read.
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;
pub const MAX_LAYERS: usize = 64;
pub const MAX_HIDDEN: usize = 4096;
pub const MAX_INTERMEDIATE: usize = 16384;
pub const MAX_HEADS: usize = 64;
pub const MAX_VOCAB: usize = 512 * 1024;
pub const MAX_CONTEXT: usize = 256;
pub const MAX_IMAGE: usize = 1024;
pub const MAX_PATCHES: usize = 4096;

/// The MLP activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// `gelu_pytorch_tanh` (SigLIP's).
    GeluTanh,
    /// Exact (erf) GELU.
    Gelu,
    /// `x · σ(1.702 x)` (CLIP's `quick_gelu`).
    QuickGelu,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextConfig {
    pub vocab: usize,
    pub hidden: usize,
    pub intermediate: usize,
    pub layers: usize,
    pub heads: usize,
    /// Position embeddings: the context length.
    pub context: usize,
    pub projection: usize,
    pub eps: f64,
    pub act: Act,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisionConfig {
    pub hidden: usize,
    pub intermediate: usize,
    pub layers: usize,
    pub heads: usize,
    pub channels: usize,
    pub image: usize,
    pub patch: usize,
    pub eps: f64,
    pub act: Act,
}

impl VisionConfig {
    /// Patches per side.
    pub fn grid(&self) -> usize {
        self.image / self.patch.max(1)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SiglipConfig {
    pub text: TextConfig,
    pub vision: VisionConfig,
}

#[derive(Deserialize, Default)]
struct RawText {
    vocab_size: Option<u64>,
    hidden_size: Option<u64>,
    intermediate_size: Option<u64>,
    num_hidden_layers: Option<u64>,
    num_attention_heads: Option<u64>,
    max_position_embeddings: Option<u64>,
    projection_size: Option<u64>,
    layer_norm_eps: Option<f64>,
    hidden_act: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawVision {
    hidden_size: Option<u64>,
    intermediate_size: Option<u64>,
    num_hidden_layers: Option<u64>,
    num_attention_heads: Option<u64>,
    num_channels: Option<u64>,
    image_size: Option<u64>,
    patch_size: Option<u64>,
    layer_norm_eps: Option<f64>,
    hidden_act: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    model_type: Option<String>,
    /// CLIP: the width of the shared embedding space both towers project to.
    projection_dim: Option<u64>,
    #[serde(default)]
    text_config: Option<RawText>,
    #[serde(default)]
    vision_config: Option<RawVision>,
}

fn size(field: &str, v: Option<u64>, default: usize, min: usize, max: usize) -> Result<usize> {
    let v = match v {
        None => default,
        Some(v) => usize::try_from(v).unwrap_or(usize::MAX),
    };
    if v < min || v > max {
        return Err(Error::Model(format!("config.json: {field} = {v} is outside {min}..={max}")));
    }
    Ok(v)
}

fn eps(field: &str, v: Option<f64>, default: f64) -> Result<f64> {
    let v = v.unwrap_or(default);
    if !(v.is_finite() && v > 0.0 && v <= 0.1) {
        return Err(Error::Model(format!("config.json: {field} = {v} is not a usable epsilon")));
    }
    Ok(v)
}

fn act(field: &str, v: Option<&str>, default: &str) -> Result<Act> {
    match v.unwrap_or(default) {
        "gelu_pytorch_tanh" | "gelu_new" | "gelu_fast" => Ok(Act::GeluTanh),
        "gelu" => Ok(Act::Gelu),
        "quick_gelu" => Ok(Act::QuickGelu),
        other => Err(Error::Model(format!("config.json: unsupported {field} `{}`", other.chars().take(40).collect::<String>()))),
    }
}

fn heads(field: &str, v: Option<u64>, default: usize, hidden: usize) -> Result<usize> {
    let h = size(field, v, default, 1, MAX_HEADS)?;
    if !hidden.is_multiple_of(h) {
        return Err(Error::Model(format!("config.json: {field} = {h} does not divide the width {hidden}")));
    }
    Ok(h)
}

impl SiglipConfig {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(Error::Model(format!("config.json is larger than {MAX_CONFIG_BYTES} bytes")));
        }
        let raw: Raw = serde_json::from_slice(bytes).map_err(|e| Error::Model(format!("config.json: {e}")))?;
        if let Some(t) = &raw.model_type
            && t != "siglip"
        {
            return Err(Error::Model(format!("config.json: model_type `{}` is not a SigLIP model", t.chars().take(40).collect::<String>())));
        }
        let t = raw.text_config.unwrap_or_default();
        let v = raw.vision_config.unwrap_or_default();
        let hidden = size("text_config.hidden_size", t.hidden_size, 768, 8, MAX_HIDDEN)?;
        let text = TextConfig {
            vocab: size("text_config.vocab_size", t.vocab_size, 32000, 4, MAX_VOCAB)?,
            hidden,
            intermediate: size("text_config.intermediate_size", t.intermediate_size, 3072, 1, MAX_INTERMEDIATE)?,
            layers: size("text_config.num_hidden_layers", t.num_hidden_layers, 12, 1, MAX_LAYERS)?,
            heads: heads("text_config.num_attention_heads", t.num_attention_heads, 12, hidden)?,
            context: size("text_config.max_position_embeddings", t.max_position_embeddings, 64, 2, MAX_CONTEXT)?,
            projection: size("text_config.projection_size", t.projection_size, hidden, 1, MAX_HIDDEN)?,
            eps: eps("text_config.layer_norm_eps", t.layer_norm_eps, 1e-6)?,
            act: act("text_config.hidden_act", t.hidden_act.as_deref(), "gelu_pytorch_tanh")?,
        };
        let vhidden = size("vision_config.hidden_size", v.hidden_size, 768, 8, MAX_HIDDEN)?;
        let vision = VisionConfig {
            hidden: vhidden,
            intermediate: size("vision_config.intermediate_size", v.intermediate_size, 3072, 1, MAX_INTERMEDIATE)?,
            layers: size("vision_config.num_hidden_layers", v.num_hidden_layers, 12, 1, MAX_LAYERS)?,
            heads: heads("vision_config.num_attention_heads", v.num_attention_heads, 12, vhidden)?,
            channels: size("vision_config.num_channels", v.num_channels, 3, 3, 3)?,
            image: size("vision_config.image_size", v.image_size, 224, 16, MAX_IMAGE)?,
            patch: size("vision_config.patch_size", v.patch_size, 16, 1, 64)?,
            eps: eps("vision_config.layer_norm_eps", v.layer_norm_eps, 1e-6)?,
            act: act("vision_config.hidden_act", v.hidden_act.as_deref(), "gelu_pytorch_tanh")?,
        };
        if !vision.image.is_multiple_of(vision.patch) || vision.grid() * vision.grid() > MAX_PATCHES {
            return Err(Error::Model(format!(
                "config.json: image_size {} and patch_size {} give an unusable patch grid",
                vision.image, vision.patch
            )));
        }
        if text.projection != vision.hidden {
            return Err(Error::Model(format!(
                "config.json: the text projection ({}) and the image width ({}) differ",
                text.projection, vision.hidden
            )));
        }
        Ok(Self { text, vision })
    }
}

/// A CLIP model's sizes: the two towers and the shared embedding space they project to (`projection_dim`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipConfig {
    /// Its `projection` is the shared embedding size, as is [`ClipConfig::projection`].
    pub text: TextConfig,
    pub vision: VisionConfig,
    pub projection: usize,
}

impl ClipConfig {
    /// Read a `config.json` of `model_type` `clip`, with `CLIPConfig`'s defaults (ViT-B/32: 224 px, patch 32; a
    /// 12-layer, 512-wide causal text tower over 77 tokens; 512-dim embeddings).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(Error::Model(format!("config.json is larger than {MAX_CONFIG_BYTES} bytes")));
        }
        let raw: Raw = serde_json::from_slice(bytes).map_err(|e| Error::Model(format!("config.json: {e}")))?;
        match raw.model_type.as_deref() {
            Some("clip") => {}
            Some(t) => return Err(Error::Model(format!("config.json: model_type `{}` is not a CLIP model", t.chars().take(40).collect::<String>()))),
            None => return Err(Error::Model("config.json: no model_type (a CLIP model says `clip`)".into())),
        }
        let projection = size("projection_dim", raw.projection_dim, 512, 1, MAX_HIDDEN)?;
        let t = raw.text_config.unwrap_or_default();
        let v = raw.vision_config.unwrap_or_default();
        let hidden = size("text_config.hidden_size", t.hidden_size, 512, 8, MAX_HIDDEN)?;
        let text = TextConfig {
            vocab: size("text_config.vocab_size", t.vocab_size, 49408, 4, MAX_VOCAB)?,
            hidden,
            intermediate: size("text_config.intermediate_size", t.intermediate_size, 2048, 1, MAX_INTERMEDIATE)?,
            layers: size("text_config.num_hidden_layers", t.num_hidden_layers, 12, 1, MAX_LAYERS)?,
            heads: heads("text_config.num_attention_heads", t.num_attention_heads, 8, hidden)?,
            context: size("text_config.max_position_embeddings", t.max_position_embeddings, 77, 2, MAX_CONTEXT)?,
            projection,
            eps: eps("text_config.layer_norm_eps", t.layer_norm_eps, 1e-5)?,
            act: act("text_config.hidden_act", t.hidden_act.as_deref(), "quick_gelu")?,
        };
        let vhidden = size("vision_config.hidden_size", v.hidden_size, 768, 8, MAX_HIDDEN)?;
        let vision = VisionConfig {
            hidden: vhidden,
            intermediate: size("vision_config.intermediate_size", v.intermediate_size, 3072, 1, MAX_INTERMEDIATE)?,
            layers: size("vision_config.num_hidden_layers", v.num_hidden_layers, 12, 1, MAX_LAYERS)?,
            heads: heads("vision_config.num_attention_heads", v.num_attention_heads, 12, vhidden)?,
            channels: size("vision_config.num_channels", v.num_channels, 3, 3, 3)?,
            image: size("vision_config.image_size", v.image_size, 224, 16, MAX_IMAGE)?,
            patch: size("vision_config.patch_size", v.patch_size, 32, 1, 64)?,
            eps: eps("vision_config.layer_norm_eps", v.layer_norm_eps, 1e-5)?,
            act: act("vision_config.hidden_act", v.hidden_act.as_deref(), "quick_gelu")?,
        };
        // (the class token takes one more position)
        if !vision.image.is_multiple_of(vision.patch) || vision.grid() * vision.grid() + 1 > MAX_PATCHES {
            return Err(Error::Model(format!(
                "config.json: image_size {} and patch_size {} give an unusable patch grid",
                vision.image, vision.patch
            )));
        }
        Ok(Self { text, vision, projection })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_siglip2_base_config_uses_the_defaults() {
        let c = SiglipConfig::parse(
            br#"{"initializer_factor": 1.0, "model_type": "siglip",
                 "text_config": {"model_type": "siglip_text_model", "vocab_size": 256000},
                 "vision_config": {"image_size": 256, "model_type": "siglip_vision_model"}}"#,
        )
        .unwrap();
        assert_eq!((c.text.vocab, c.text.hidden, c.text.layers, c.text.heads, c.text.context), (256000, 768, 12, 12, 64));
        assert_eq!((c.vision.image, c.vision.patch, c.vision.grid(), c.vision.hidden), (256, 16, 16, 768));
        assert_eq!(c.text.act, Act::GeluTanh);
    }

    #[test]
    fn hostile_configs_are_refused() {
        for bad in [
            r#"{"text_config": {"vocab_size": 99999999999}}"#,
            r#"{"text_config": {"vocab_size": -1}}"#,
            r#"{"text_config": {"hidden_size": 1000000000}}"#,
            r#"{"text_config": {"num_hidden_layers": 100000}}"#,
            r#"{"text_config": {"num_attention_heads": 7}}"#,
            r#"{"text_config": {"num_attention_heads": 0}}"#,
            r#"{"text_config": {"max_position_embeddings": 1000000}}"#,
            r#"{"text_config": {"layer_norm_eps": -1}}"#,
            r#"{"text_config": {"hidden_act": "swish"}}"#,
            r#"{"vision_config": {"image_size": 100000}}"#,
            r#"{"vision_config": {"patch_size": 0}}"#,
            r#"{"vision_config": {"image_size": 250, "patch_size": 16}}"#,
            r#"{"vision_config": {"image_size": 1024, "patch_size": 1}}"#,
            r#"{"vision_config": {"num_channels": 4}}"#,
            r#"{"text_config": {"projection_size": 512}}"#,
            r#"{"model_type": "llama"}"#,
            r#"[1, 2]"#,
            r#"not json"#,
        ] {
            assert!(SiglipConfig::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(SiglipConfig::parse(&vec![b' '; MAX_CONFIG_BYTES + 1]).is_err());
        // a CLIP config is not a SigLIP one
        assert!(SiglipConfig::parse(br#"{"model_type": "clip"}"#).is_err());
    }

    #[test]
    fn the_clip_b32_config_uses_the_defaults() {
        let c = ClipConfig::parse(
            br#"{"model_type": "clip", "projection_dim": 512,
                 "text_config": {"hidden_act": "quick_gelu", "max_position_embeddings": 77, "vocab_size": 49408},
                 "vision_config": {"hidden_act": "quick_gelu", "image_size": 224, "patch_size": 32}}"#,
        )
        .unwrap();
        assert_eq!((c.text.vocab, c.text.hidden, c.text.layers, c.text.heads, c.text.context), (49408, 512, 12, 8, 77));
        assert_eq!((c.vision.image, c.vision.patch, c.vision.grid(), c.vision.hidden, c.vision.heads), (224, 32, 7, 768, 12));
        assert_eq!((c.projection, c.text.act, c.vision.act, c.text.eps), (512, Act::QuickGelu, Act::QuickGelu, 1e-5));
    }

    #[test]
    fn hostile_clip_configs_are_refused() {
        for bad in [
            r#"{"model_type": "siglip"}"#,
            r#"{"text_config": {}}"#,
            r#"{"model_type": "clip", "projection_dim": 0}"#,
            r#"{"model_type": "clip", "projection_dim": 1000000}"#,
            r#"{"model_type": "clip", "text_config": {"vocab_size": 99999999999}}"#,
            r#"{"model_type": "clip", "text_config": {"max_position_embeddings": 100000}}"#,
            r#"{"model_type": "clip", "text_config": {"num_attention_heads": 7}}"#,
            r#"{"model_type": "clip", "text_config": {"hidden_act": "relu"}}"#,
            r#"{"model_type": "clip", "vision_config": {"image_size": 225}}"#,
            r#"{"model_type": "clip", "vision_config": {"image_size": 1024, "patch_size": 16}}"#,
            r#"{"model_type": "clip", "vision_config": {"layer_norm_eps": 0}}"#,
            r#"{"model_type": "clip", "vision_config": {"num_hidden_layers": 65}}"#,
        ] {
            assert!(ClipConfig::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(ClipConfig::parse(&vec![b' '; MAX_CONFIG_BYTES + 1]).is_err());
    }
}
