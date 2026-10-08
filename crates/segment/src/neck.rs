//! The feature pyramid (`Sam3VisionNeck`): the backbone's 72 × 72 map at four scales
//! (288, 144, 72 and 36 per side), each projected to 256 channels.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/modeling_sam3.py`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use candle_core::{Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, ConvTranspose2d, ConvTranspose2dConfig, VarBuilder};

use crate::Result;
use crate::vit::HIDDEN;

pub const FPN: usize = 256;

enum Scale {
    /// Two 2× transposed convolutions with GELU between (4×).
    Up4(ConvTranspose2d, ConvTranspose2d),
    Up2(ConvTranspose2d),
    Same,
    /// 2 × 2 max pooling (½×).
    Down2,
}

struct Level {
    scale: Scale,
    proj1: Conv2d,
    proj2: Conv2d,
}

pub struct Neck {
    levels: Vec<Level>,
}

impl Neck {
    /// `vb` at `…neck` (detector) or `tracker_neck`.
    pub fn new(vb: VarBuilder) -> Result<Self> {
        let up = ConvTranspose2dConfig { stride: 2, ..Default::default() };
        let mut levels = Vec::new();
        for (i, factor) in [4.0f32, 2.0, 1.0, 0.5].into_iter().enumerate() {
            let l = vb.pp(format!("fpn_layers.{i}"));
            let s = l.pp("scale_layers");
            let (scale, inter) = if factor == 4.0 {
                let a = candle_nn::conv_transpose2d(HIDDEN, HIDDEN / 2, 2, up, s.pp("0"))?;
                let b = candle_nn::conv_transpose2d(HIDDEN / 2, HIDDEN / 4, 2, up, s.pp("2"))?;
                (Scale::Up4(a, b), HIDDEN / 4)
            } else if factor == 2.0 {
                (Scale::Up2(candle_nn::conv_transpose2d(HIDDEN, HIDDEN / 2, 2, up, s.pp("0"))?), HIDDEN / 2)
            } else if factor == 1.0 {
                (Scale::Same, HIDDEN)
            } else {
                (Scale::Down2, HIDDEN)
            };
            levels.push(Level {
                scale,
                proj1: candle_nn::conv2d(inter, FPN, 1, Conv2dConfig::default(), l.pp("proj1"))?,
                proj2: candle_nn::conv2d(FPN, FPN, 3, Conv2dConfig { padding: 1, ..Default::default() }, l.pp("proj2"))?,
            });
        }
        Ok(Self { levels })
    }

    /// `x`: the backbone's `[1, 1024, 72, 72]`; `n`: how many levels (from the finest) to compute.
    pub fn forward(&self, x: &Tensor, n: usize) -> Result<Vec<Tensor>> {
        let mut out = Vec::with_capacity(n);
        for l in self.levels.iter().take(n) {
            let y = match &l.scale {
                Scale::Up4(a, b) => b.forward(&a.forward(x)?.gelu_erf()?)?,
                Scale::Up2(a) => a.forward(x)?,
                Scale::Same => x.clone(),
                Scale::Down2 => x.max_pool2d(2)?,
            };
            out.push(l.proj2.forward(&l.proj1.forward(&y)?)?);
        }
        Ok(out)
    }
}
