//! SAM 2's image encoder: the Hiera backbone (`Sam2HieraDetModel`) and the feature pyramid neck
//! (`Sam2VisionNeck`).
//!
//! Hiera is a hierarchical vision transformer: four stages at 1/4, 1/8, 1/16 and 1/32 of the
//! input, windowed attention (a few blocks attend globally), and query max-pooling at the first
//! block of each later stage. The neck projects each stage to 256 channels and adds the
//! upsampled coarser level to the levels listed in `fpn_top_down_levels`.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 2 code of Hugging Face Transformers (`models/sam2/modeling_sam2.py`),
//! Copyright The HuggingFace Team and Meta Platforms, Inc.; translated to Rust on candle and
//! restructured. See NOTICE.

use candle_core::{D, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, Linear, VarBuilder};

use crate::cpu::{Act, add_act, linear, pool2, window_attention};
use crate::nn::{ln, to_vec};
use crate::{Error, Result, nn};

/// The Hiera hyper-parameters (`Sam2HieraDetConfig`) and the neck's top-down levels.
#[derive(Clone, Debug, PartialEq)]
pub struct HieraConfig {
    /// Channels after the patch embedding (`hidden_size`).
    pub embed: usize,
    pub blocks_per_stage: Vec<usize>,
    pub dims: Vec<usize>,
    pub heads: Vec<usize>,
    pub windows: Vec<usize>,
    /// Blocks (counted over all stages) with global attention.
    pub global: Vec<usize>,
    /// Size of the interpolated background position embedding.
    pub pos_bkg: [usize; 2],
    /// How many stages (after the first) start with query pooling.
    pub q_pool: usize,
    /// Side of the square input.
    pub image: usize,
    /// Neck levels that add the upsampled coarser level.
    pub top_down: Vec<usize>,
}

impl HieraConfig {
    /// SAM 2.1 Hiera-Tiny (`facebook/sam2.1-hiera-tiny`).
    pub fn tiny() -> Self {
        Self {
            embed: 96,
            blocks_per_stage: vec![1, 2, 7, 2],
            dims: vec![96, 192, 384, 768],
            heads: vec![1, 2, 4, 8],
            windows: vec![8, 4, 14, 7],
            global: vec![5, 7, 9],
            pos_bkg: [7, 7],
            q_pool: 3,
            image: 1024,
            top_down: vec![2, 3],
        }
    }

    /// Read from a transformers `config.json` (`sam2` or `sam2_video`); fields it lacks keep the
    /// Hiera-Tiny values.
    pub fn from_json(v: &serde_json::Value) -> Result<Self> {
        let mut c = Self::tiny();
        let vision = v.get("vision_config");
        let bb = vision.and_then(|v| v.get("backbone_config"));
        let usizes = |o: Option<&serde_json::Value>, key: &str| -> Option<Vec<usize>> {
            o?.get(key)?.as_array()?.iter().map(|x| x.as_u64().and_then(|x| usize::try_from(x).ok())).collect()
        };
        if let Some(x) = bb.and_then(|b| b.get("hidden_size")).and_then(|x| x.as_u64()) {
            c.embed = usize::try_from(x).map_err(|_| Error::Model("hidden_size".into()))?;
        }
        if let Some(x) = usizes(bb, "blocks_per_stage") {
            c.blocks_per_stage = x;
        }
        if let Some(x) = usizes(bb, "embed_dim_per_stage") {
            c.dims = x;
        }
        if let Some(x) = usizes(bb, "num_attention_heads_per_stage") {
            c.heads = x;
        }
        if let Some(x) = usizes(bb, "window_size_per_stage") {
            c.windows = x;
        }
        if let Some(x) = usizes(bb, "global_attention_blocks") {
            c.global = x;
        }
        if let Some([a, b]) = usizes(bb, "window_positional_embedding_background_size").as_deref() {
            c.pos_bkg = [*a, *b];
        }
        if let Some(x) = bb.and_then(|b| b.get("num_query_pool_stages")).and_then(|x| x.as_u64()) {
            c.q_pool = usize::try_from(x).map_err(|_| Error::Model("num_query_pool_stages".into()))?;
        }
        if let Some(x) = v.get("image_size").and_then(|x| x.as_u64()) {
            c.image = usize::try_from(x).map_err(|_| Error::Model("image_size".into()))?;
        }
        if let Some(x) = usizes(vision, "fpn_top_down_levels") {
            c.top_down = x;
        }
        // the parts this port assumes fixed (true of every released SAM 2 / 2.1 checkpoint)
        let fixed = |key: &str, want: &[usize]| match usizes(bb, key) {
            Some(x) if x != want => Err(Error::Model(format!("unsupported Hiera {key} {x:?} (expected {want:?})"))),
            _ => Ok(()),
        };
        fixed("patch_kernel_size", &[7, 7])?;
        fixed("patch_stride", &[4, 4])?;
        fixed("patch_padding", &[3, 3])?;
        fixed("query_stride", &[2, 2])?;
        if let Some(n) = vision.and_then(|v| v.get("num_feature_levels")).and_then(|x| x.as_u64())
            && n != 3
        {
            return Err(Error::Model(format!("unsupported num_feature_levels {n} (expected 3)")));
        }
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<()> {
        let n = self.blocks_per_stage.len();
        let bad = |what: &str| Err(Error::Model(format!("unsupported Hiera configuration: {what}")));
        if n != 4 || self.dims.len() != n || self.heads.len() != n || self.windows.len() != n {
            return bad("expected four stages");
        }
        if self.blocks_per_stage.iter().any(|b| *b == 0 || *b > 64) || self.q_pool >= n {
            return bad("blocks per stage");
        }
        if self.dims.iter().any(|d| *d == 0 || *d > 4096) || self.embed != self.dims.first().copied().unwrap_or(0) {
            return bad("stage widths");
        }
        if self.heads.iter().zip(&self.dims).any(|(h, d)| *h == 0 || d % h != 0) {
            return bad("heads");
        }
        if self.windows.iter().any(|w| *w > 256) || self.pos_bkg.iter().any(|s| *s == 0 || *s > 256) {
            return bad("window sizes");
        }
        // 1/4 … 1/32 of the input must be whole, and the first window must tile the 1/4 grid
        let grid = self.image / 4;
        let w0 = self.windows.first().copied().unwrap_or(0);
        if self.image == 0 || !self.image.is_multiple_of(32) || self.image > 4096 || w0 == 0 || !grid.is_multiple_of(w0) {
            return bad("input size");
        }
        if self.top_down.iter().any(|l| *l >= n) {
            return bad("fpn_top_down_levels");
        }
        Ok(())
    }

    /// Side of the image-embedding grid the mask decoder reads (1/16 of the input).
    pub fn grid(&self) -> usize {
        self.image / 16
    }
}

/// `[B, H, W, C]` → windows `[B·nh·nw, w, w, C]` (zero padding to a multiple of `w`).
fn partition(x: &Tensor, w: usize) -> Result<(Tensor, usize, usize)> {
    let (b, h, wd, c) = x.dims4()?;
    let (ph, pw) = (h.div_ceil(w) * w, wd.div_ceil(w) * w);
    let x = if ph > h { x.pad_with_zeros(1, 0, ph - h)? } else { x.clone() };
    let x = if pw > wd { x.pad_with_zeros(2, 0, pw - wd)? } else { x };
    let (nh, nw) = (ph / w, pw / w);
    let x = x.reshape((b * nh, w, nw, w * c))?.transpose(1, 2)?.contiguous()?;
    Ok((x.reshape((b * nh * nw, w, w, c))?, ph, pw))
}

/// The inverse of [`partition`], cropped to `h × wd`.
fn unpartition(x: &Tensor, w: usize, (ph, pw): (usize, usize), (h, wd): (usize, usize)) -> Result<Tensor> {
    let c = x.dim(D::Minus1)?;
    let (nh, nw) = (ph / w, pw / w);
    let b = x.dim(0)? / (nh * nw).max(1);
    let x = x.reshape((b * nh, nw, w, w * c))?.transpose(1, 2)?.contiguous()?.reshape((b, ph, pw, c))?;
    let x = if ph > h { x.narrow(1, 0, h)? } else { x };
    let x = if pw > wd { x.narrow(2, 0, wd)? } else { x };
    Ok(x.contiguous()?)
}

/// `Sam2MultiScaleBlock`.
struct Block {
    ln1: LayerNorm,
    ln2: LayerNorm,
    qkv: Linear,
    attn_proj: Linear,
    /// Skip-connection projection when the width changes.
    proj: Option<Linear>,
    mlp_in: Linear,
    mlp_out: Linear,
    heads: usize,
    dim_out: usize,
    window: usize,
    pool: bool,
}

impl Block {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let xn = self.ln1.forward(x)?;
        let residual = match &self.proj {
            Some(p) if self.pool => pool2(&linear(&xn, p, Act::None, None)?)?,
            Some(p) => linear(&xn, p, Act::None, None)?,
            None => x.clone(),
        };
        let a = if xn.device().is_cpu() {
            // windows, padding and query pooling inside one multi-threaded kernel
            let qkv = linear(&xn, &self.qkv, Act::None, None)?;
            let bias = match self.qkv.bias() {
                Some(b) => b.clone(),
                None => Tensor::zeros(3 * self.dim_out, candle_core::DType::F32, xn.device())?,
            };
            let hd = self.dim_out / self.heads;
            window_attention(&qkv, &bias, self.heads, self.window, self.pool, (hd as f64).powf(-0.5))?
        } else if self.window > 0 {
            let (win, _, _) = partition(&xn, self.window)?;
            let a = self.attention(&win)?;
            // after query pooling, the windows and the map are half the size
            let ws = if self.pool { self.window / 2 } else { self.window };
            let (rh, rw) = (residual.dim(1)?, residual.dim(2)?);
            unpartition(&a, ws, (rh.div_ceil(ws) * ws, rw.div_ceil(ws) * ws), (rh, rw))?
        } else {
            self.attention(&xn)?
        };
        // the output projection is per token, so it runs after un-windowing, fused with the skip
        let x = linear(&a, &self.attn_proj, Act::None, Some(&residual))?;
        let m = linear(&self.ln2.forward(&x)?, &self.mlp_in, Act::Gelu, None)?;
        linear(&m, &self.mlp_out, Act::None, Some(&x))
    }

    /// `Sam2MultiScaleAttention` without its output projection: `[B, H, W, C]` →
    /// `[B, H', W', dim_out]`.
    fn attention(&self, x: &Tensor) -> Result<Tensor> {
        let (b, h, w, _) = x.dims4()?;
        let hd = self.dim_out / self.heads;
        let qkv = linear(x, &self.qkv, Act::None, None)?.reshape((b, h * w, 3, self.heads, hd))?;
        let (q, k, v) = (qkv.narrow(2, 0, 1)?.squeeze(2)?, qkv.narrow(2, 1, 1)?.squeeze(2)?, qkv.narrow(2, 2, 1)?.squeeze(2)?);
        let (q, hq, wq) = if self.pool {
            let q = pool2(&q.reshape((b, h, w, self.dim_out))?)?;
            let (_, hq, wq, _) = q.dims4()?;
            (q.reshape((b, hq * wq, self.heads, hd))?, hq, wq)
        } else {
            (q, h, w)
        };
        let (q, k, v) = (q.transpose(1, 2)?.contiguous()?, k.transpose(1, 2)?.contiguous()?, v.transpose(1, 2)?.contiguous()?);
        let o = nn::sdpa(&q, &k, &v, None, (hd as f64).powf(-0.5))?;
        Ok(o.transpose(1, 2)?.reshape((b, hq, wq, self.dim_out))?)
    }
}

/// Cubic convolution weights (PyTorch's bicubic, A = −0.75) for a fractional offset `t`.
fn cubic(t: f32) -> [f32; 4] {
    const A: f32 = -0.75;
    let c1 = |x: f32| ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    let c2 = |x: f32| ((A * x - 5.0 * A) * x + 8.0 * A) * x - 4.0 * A;
    [c2(t + 1.0), c1(t), c1(1.0 - t), c2(2.0 - t)]
}

/// `F.interpolate(x, size, mode="bicubic", align_corners=False)` of a `[c, h, w]` map.
fn bicubic(src: &[f32], (c, h, w): (usize, usize, usize), (oh, ow): (usize, usize)) -> Vec<f32> {
    let taps = |n: usize, on: usize| -> Vec<([usize; 4], [f32; 4])> {
        let scale = n as f32 / on as f32;
        (0..on)
            .map(|o| {
                let s = scale * (o as f32 + 0.5) - 0.5;
                let i = s.floor();
                let ws = cubic(s - i);
                let at = |k: f32| (i + k).clamp(0.0, n.saturating_sub(1) as f32) as usize;
                ([at(-1.0), at(0.0), at(1.0), at(2.0)], ws)
            })
            .collect()
    };
    let (ty, tx) = (taps(h, oh), taps(w, ow));
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        for (oy, (iy, wy)) in ty.iter().enumerate() {
            for (ox, (ix, wx)) in tx.iter().enumerate() {
                let mut acc = 0f32;
                for (yy, wyy) in iy.iter().zip(wy) {
                    let mut row = 0f32;
                    for (xx, wxx) in ix.iter().zip(wx) {
                        row += wxx * src.get((ch * h + yy) * w + xx).copied().unwrap_or(0.0);
                    }
                    acc += wyy * row;
                }
                if let Some(o) = out.get_mut((ch * oh + oy) * ow + ox) {
                    *o = acc;
                }
            }
        }
    }
    out
}

/// The Hiera backbone.
pub struct Hiera {
    patch: Conv2d,
    /// Background + tiled window position embedding, NHWC `[1, image/4, image/4, embed]`.
    pos: Tensor,
    blocks: Vec<Block>,
    /// Indices of the last block of each stage.
    stage_ends: Vec<usize>,
}

impl Hiera {
    /// `vb` at `vision_encoder.backbone`.
    pub fn new(cfg: &HieraConfig, vb: VarBuilder) -> Result<Self> {
        let e = cfg.embed;
        let patch = candle_nn::conv2d(3, e, 7, Conv2dConfig { stride: 4, padding: 3, ..Default::default() }, vb.pp("patch_embed.projection"))?;
        let grid = cfg.image / 4;
        let [bh, bw] = cfg.pos_bkg;
        let bkg = to_vec(&vb.get((1, e, bh, bw), "pos_embed")?)?;
        let w0 = cfg.windows.first().copied().unwrap_or(1).max(1);
        let win = to_vec(&vb.get((1, e, w0, w0), "pos_embed_window")?)?;
        let mut pos = bicubic(&bkg, (e, bh, bw), (grid, grid));
        for ch in 0..e {
            for y in 0..grid {
                for x in 0..grid {
                    let add = win.get((ch * w0 + y % w0) * w0 + x % w0).copied().unwrap_or(0.0);
                    if let Some(p) = pos.get_mut((ch * grid + y) * grid + x) {
                        *p += add;
                    }
                }
            }
        }
        let pos = Tensor::from_vec(pos, (1, e, grid, grid), vb.device())?.permute((0, 2, 3, 1))?.contiguous()?;
        let mut blocks = Vec::new();
        let mut stage_ends = Vec::new();
        let mut total = 0usize;
        for (stage, &n) in cfg.blocks_per_stage.iter().enumerate() {
            let at = |v: &[usize], i: usize| v.get(i).copied().ok_or_else(|| Error::Model("Hiera configuration".into()));
            for block in 0..n {
                let first_of_later = stage > 0 && block == 0;
                let dim = if first_of_later { at(&cfg.dims, stage - 1)? } else { at(&cfg.dims, stage)? };
                let dim_out = at(&cfg.dims, stage)?;
                let window = if cfg.global.contains(&total) {
                    0
                } else if first_of_later {
                    at(&cfg.windows, stage - 1)?
                } else {
                    at(&cfg.windows, stage)?
                };
                let pool = first_of_later && stage <= cfg.q_pool;
                let b = vb.pp(format!("blocks.{total}"));
                blocks.push(Block {
                    ln1: ln(dim, 1e-6, b.pp("layer_norm1"))?,
                    ln2: ln(dim_out, 1e-6, b.pp("layer_norm2"))?,
                    qkv: nn::linear(dim, 3 * dim_out, b.pp("attn.qkv"))?,
                    attn_proj: nn::linear(dim_out, dim_out, b.pp("attn.proj"))?,
                    proj: if dim != dim_out { Some(nn::linear(dim, dim_out, b.pp("proj"))?) } else { None },
                    mlp_in: nn::linear(dim_out, 4 * dim_out, b.pp("mlp.proj_in"))?,
                    mlp_out: nn::linear(4 * dim_out, dim_out, b.pp("mlp.proj_out"))?,
                    heads: at(&cfg.heads, stage)?,
                    dim_out,
                    window,
                    pool,
                });
                total += 1;
            }
            stage_ends.push(total.saturating_sub(1));
        }
        Ok(Self { patch, pos, blocks, stage_ends })
    }

    /// `pixels` `[1, 3, S, S]` → the four stage outputs (NHWC; 1/4 … 1/32 of the input).
    pub fn forward(&self, pixels: &Tensor) -> Result<Vec<Tensor>> {
        let x = self.patch.forward(pixels)?.permute((0, 2, 3, 1))?;
        if x.dims() != self.pos.dims() {
            return Err(Error::Model(format!("SAM 2 input must be {:?} after the patch embedding, got {:?}", self.pos.dims(), x.dims())));
        }
        let mut x = (x + &self.pos)?.contiguous()?;
        let mut out = Vec::with_capacity(self.stage_ends.len());
        for (i, b) in self.blocks.iter().enumerate() {
            x = b.forward(&x)?;
            if self.stage_ends.contains(&i) {
                out.push(x.clone());
            }
        }
        Ok(out)
    }
}

/// `Sam2VisionNeck`: 1 × 1 projections of the stages to 256 channels, with top-down fusion.
pub struct FpnNeck {
    /// Indexed by stage (finest first).
    lateral: Vec<Linear>,
    top_down: Vec<usize>,
}

pub const FPN: usize = 256;

impl FpnNeck {
    /// `vb` at `vision_encoder.neck`.
    pub fn new(cfg: &HieraConfig, vb: VarBuilder) -> Result<Self> {
        // `convs.j` projects stage `n − j` (the coarsest first)
        let n = cfg.dims.len();
        let mut lateral = Vec::with_capacity(n);
        for (i, &c) in cfg.dims.iter().enumerate() {
            let p = vb.pp(format!("convs.{}", n - 1 - i));
            let w = p.get((FPN, c, 1, 1), "weight")?.reshape((FPN, c))?;
            lateral.push(Linear::new(w, Some(p.get(FPN, "bias")?)));
        }
        Ok(Self { lateral, top_down: cfg.top_down.clone() })
    }

    /// The three finest levels (channel-last `[1, H, W, 256]`; 1/4, 1/8 and 1/16 of the input)
    /// from the stage outputs.
    pub fn forward(&self, stages: &[Tensor]) -> Result<Vec<Tensor>> {
        let n = self.lateral.len();
        if stages.len() != n {
            return Err(Error::Model("neck: wrong number of backbone stages".into()));
        }
        let mut levels: Vec<Tensor> = Vec::with_capacity(n);
        let mut prev: Option<Tensor> = None;
        for i in (0..n).rev() {
            let (Some(s), Some(l)) = (stages.get(i), self.lateral.get(i)) else { return Err(Error::Model("neck".into())) };
            let lat = linear(s, l, Act::None, None)?;
            let cur = match &prev {
                Some(p) if self.top_down.contains(&i) && i + 1 != n => {
                    // nearest-neighbour 2× upsampling, channel-last
                    let (b, h, w, c) = p.dims4()?;
                    let up = p.reshape((b * h, 1, w, 1, c))?.broadcast_as((b * h, 2, w, 2, c))?.contiguous()?.reshape((b, 2 * h, 2 * w, c))?;
                    add_act(&lat, &up, false)?
                }
                _ => lat,
            };
            levels.push(cur.clone());
            prev = Some(cur);
        }
        levels.reverse(); // finest first
        levels.truncate(3);
        Ok(levels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    /// The windowed attention the plain way: pad the map with the padding token, partition,
    /// pool the queries, attend, un-partition.
    fn window_attention_reference(qkv: &Tensor, bias: &Tensor, heads: usize, window: usize, pool: bool, scale: f64) -> Tensor {
        let (b, h, w, c3) = qkv.dims4().unwrap();
        let dim = c3 / 3;
        let hd = dim / heads;
        let (ph, pw) = if window > 0 { (h.div_ceil(window) * window, w.div_ceil(window) * window) } else { (h, w) };
        let src = qkv.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let bias_v = bias.to_vec1::<f32>().unwrap();
        let mut padded = Vec::with_capacity(b * ph * pw * c3);
        for bi in 0..b {
            for y in 0..ph {
                for x in 0..pw {
                    if y < h && x < w {
                        let at = ((bi * h + y) * w + x) * c3;
                        padded.extend_from_slice(&src[at..at + c3]);
                    } else {
                        padded.extend_from_slice(&bias_v);
                    }
                }
            }
        }
        let padded = Tensor::from_vec(padded, (b, ph, pw, c3), qkv.device()).unwrap();
        let win = if window > 0 { partition(&padded, window).unwrap().0 } else { padded };
        let (nw, wh, ww, _) = win.dims4().unwrap();
        let qkv5 = win.reshape((nw, wh * ww, 3, heads, hd)).unwrap();
        let part = |i: usize| qkv5.narrow(2, i, 1).unwrap().squeeze(2).unwrap();
        let (mut q, k, v) = (part(0), part(1), part(2));
        let (qh, qw) = if pool { (wh / 2, ww / 2) } else { (wh, ww) };
        if pool {
            q = pool2(&q.reshape((nw, wh, ww, dim)).unwrap()).unwrap().reshape((nw, qh * qw, heads, hd)).unwrap();
        }
        let t = |x: &Tensor| x.transpose(1, 2).unwrap().contiguous().unwrap();
        let o = nn::sdpa(&t(&q), &t(&k), &t(&v), None, scale).unwrap();
        let o = o.transpose(1, 2).unwrap().reshape((nw, qh, qw, dim)).unwrap();
        let (oh, ow) = if pool { (h / 2, w / 2) } else { (h, w) };
        if window > 0 {
            let ws = if pool { window / 2 } else { window };
            unpartition(&o, ws, (oh.div_ceil(ws) * ws, ow.div_ceil(ws) * ws), (oh, ow)).unwrap()
        } else {
            o.narrow(1, 0, oh).unwrap().narrow(2, 0, ow).unwrap()
        }
    }

    #[test]
    fn window_attention_matches_partitioned_attention() {
        let d = Device::Cpu;
        let (heads, dim) = (2, 24);
        for (h, w, window, pool) in
            [(10, 9, 4, false), (10, 8, 4, true), (12, 12, 6, true), (14, 10, 6, true), (7, 5, 0, false), (8, 6, 0, true), (16, 16, 8, false)]
        {
            let qkv = Tensor::randn(0f32, 1., (2, h, w, 3 * dim), &d).unwrap();
            let bias = Tensor::randn(0f32, 1., 3 * dim, &d).unwrap();
            let got = crate::cpu::window_attention(&qkv, &bias, heads, window, pool, 0.2).unwrap();
            let want = window_attention_reference(&qkv, &bias, heads, window, pool, 0.2);
            assert_eq!(got.dims(), want.dims(), "{h}x{w} window {window} pool {pool}");
            let err = (got - want).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
            assert!(err < 1e-5, "{h}x{w} window {window} pool {pool}: {err}");
        }
    }

    #[test]
    fn windows_round_trip_with_padding() {
        let d = Device::Cpu;
        let x = Tensor::arange(0f32, (2 * 10 * 9 * 3) as f32, &d).unwrap().reshape((2, 10, 9, 3)).unwrap();
        let (w, ph, pw) = partition(&x, 4).unwrap();
        assert_eq!((w.dims(), ph, pw), (&[2 * 3 * 3, 4, 4, 3][..], 12, 12));
        // window (0, 1) of image 0 starts at row 0, column 4
        let first = w.get(1).unwrap().get(0).unwrap().get(0).unwrap().to_vec1::<f32>().unwrap();
        assert_eq!(first, x.get(0).unwrap().get(0).unwrap().get(4).unwrap().to_vec1::<f32>().unwrap());
        let back = unpartition(&w, 4, (ph, pw), (10, 9)).unwrap();
        assert_eq!(back.flatten_all().unwrap().to_vec1::<f32>().unwrap(), x.flatten_all().unwrap().to_vec1::<f32>().unwrap());
    }

    #[test]
    fn bicubic_keeps_constants_and_matches_torch() {
        let flat = vec![2.5f32; 3 * 4 * 4];
        assert!(bicubic(&flat, (3, 4, 4), (9, 7)).iter().all(|v| (v - 2.5).abs() < 1e-5));
        // torch.nn.functional.interpolate(torch.tensor([[[[0.,1.,2.]]]]), size=(1,5), mode="bicubic")
        let r = bicubic(&[0.0, 1.0, 2.0], (1, 1, 3), (1, 5));
        let want = [-0.0960, 0.3160, 1.0000, 1.6840, 2.0960];
        for (a, b) in r.iter().zip(want) {
            assert!((a - b).abs() < 1e-3, "{r:?}");
        }
    }

    #[test]
    fn config_defaults_and_rejects_bad_values() {
        let c = HieraConfig::from_json(&serde_json::json!({})).unwrap();
        assert_eq!(c, HieraConfig::tiny());
        assert_eq!(c.grid(), 64);
        let bad = serde_json::json!({"vision_config": {"backbone_config": {"blocks_per_stage": [1, 2, 7]}}});
        assert!(HieraConfig::from_json(&bad).is_err());
        let bad = serde_json::json!({"image_size": 1000});
        assert!(HieraConfig::from_json(&bad).is_err());
        let bad = serde_json::json!({"vision_config": {"backbone_config": {"patch_stride": [8, 8]}}});
        assert!(HieraConfig::from_json(&bad).is_err());
    }
}
