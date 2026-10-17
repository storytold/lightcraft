//! BiRefNet (Bilateral Reference for high-resolution dichotomous image segmentation, Zheng et
//! al., CAAI AIR 2024) inference in pure Rust on candle: the Subject (and, inverted, Background)
//! AI mask.
//!
//! The network: a Swin Transformer v1 backbone run on the image and on a half-size copy (their
//! features concatenated per stage), the coarser stages resized onto the deepest one, a squeeze
//! block, and a four-stage decoder whose blocks are 3×3 convolutions around a deformable ASPP
//! (atrous spatial pyramid pooling with modulated deformable convolutions, kernels 1 / 3 / 7).
//! Each stage also sees the input image cut into patches stacked as channels ("inward
//! reference"), and its output is gated by a predicted gradient-attention map ("outward
//! reference"). The last stage predicts one logit per pixel of the square input.
//!
//! The architecture's sizes (Swin width, depths, heads, window; decoder channels) are read from
//! the checkpoint, so BiRefNet_lite (Swin-T) and the full Swin-L checkpoints both load; the
//! app downloads BiRefNet_lite (`ZhengPeng7/BiRefNet_lite`, MIT, see [`crate::fetch`]).
//!
//! Sources: the paper (arXiv 2401.03407) and the MIT-licensed reference implementation
//! (github.com/ZhengPeng7/BiRefNet, `models/birefnet.py`, `models/backbones/swin_v1.py`,
//! `models/modules/{aspp,decoder_blocks,deform_conv}.py`, Copyright (c) 2024 ZhengPeng),
//! restructured for candle; Swin Transformer: Liu et al., ICCV 2021 (paper); modulated
//! deformable convolution: Zhu et al., "Deformable ConvNets v2", CVPR 2019 (paper).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use candle_core::{Device, Module, Tensor};
use candle_nn::{LayerNorm, Linear};

use crate::weights::Weights;
use crate::{Error, Result};

/// The weights file of a BiRefNet model folder.
pub const WEIGHTS_FILE: &str = "model.safetensors";
/// Side of the square input the released checkpoints were trained at.
pub const DEFAULT_INPUT: usize = 1024;
/// ImageNet normalisation (the reference preprocessing).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// Batch-norm epsilon (PyTorch's default).
const BN_EPS: f64 = 1e-5;

/// Whether `dir` holds a BiRefNet checkpoint.
pub fn is_model_dir(dir: &Path) -> bool {
    dir.join(WEIGHTS_FILE).is_file()
}

fn err(m: impl Into<String>) -> Error {
    Error::Model(format!("BiRefNet: {}", m.into()))
}

/// Tensors of one module (a name prefix) of the checkpoint.
#[derive(Clone)]
struct P {
    w: Arc<Weights>,
    prefix: String,
    dev: Device,
}

impl P {
    fn pp(&self, s: impl std::fmt::Display) -> P {
        let prefix = if self.prefix.is_empty() { s.to_string() } else { format!("{}.{s}", self.prefix) };
        P { w: self.w.clone(), prefix, dev: self.dev.clone() }
    }

    fn name(&self, s: &str) -> String {
        if self.prefix.is_empty() { s.to_string() } else { format!("{}.{s}", self.prefix) }
    }

    fn has(&self, s: &str) -> bool {
        self.w.contains(&self.name(s))
    }

    fn shape(&self, s: &str) -> Result<Vec<usize>> {
        let n = self.name(s);
        self.w.shape(&n).map(<[usize]>::to_vec).ok_or_else(|| err(format!("missing tensor `{n}`")))
    }

    fn get(&self, s: &str) -> Result<Tensor> {
        Ok(self.w.read(&self.name(s))?.to_device(&self.dev)?)
    }

    fn linear(&self, bias: bool) -> Result<Linear> {
        let b = if bias { Some(self.get("bias")?) } else { None };
        Ok(Linear::new(self.get("weight")?, b))
    }

    fn ln(&self) -> Result<LayerNorm> {
        Ok(LayerNorm::new(self.get("weight")?, self.get("bias")?, crate::nn::TORCH_LN_EPS))
    }

    /// A 2-D convolution with "same"-style padding `pad` (bias if the checkpoint has one).
    fn conv(&self, pad: usize, stride: usize) -> Result<Conv> {
        let w = self.get("weight")?;
        let b = if self.has("bias") { Some(self.get("bias")?.reshape((1, (), 1, 1))?) } else { None };
        Ok(Conv { w, b, pad, stride })
    }

    /// A batch norm in eval mode as a per-channel scale and shift; `None` when the checkpoint
    /// has none here (an identity in the reference when trained with batch size 1).
    fn bn(&self) -> Result<Option<Affine>> {
        if !self.has("running_mean") {
            return Ok(None);
        }
        let (w, b) = (self.get("weight")?, self.get("bias")?);
        let (mean, var) = (self.get("running_mean")?, self.get("running_var")?);
        let scale = w.div(&(var + BN_EPS)?.sqrt()?)?;
        let shift = b.sub(&mean.mul(&scale)?)?;
        Ok(Some(Affine { scale: scale.reshape((1, (), 1, 1))?, shift: shift.reshape((1, (), 1, 1))? }))
    }
}

struct Affine {
    scale: Tensor,
    shift: Tensor,
}

fn bn(x: Tensor, a: &Option<Affine>) -> Result<Tensor> {
    Ok(match a {
        Some(a) => x.broadcast_mul(&a.scale)?.broadcast_add(&a.shift)?,
        None => x,
    })
}

/// `nn.Conv2d` (groups 1, dilation 1).
struct Conv {
    w: Tensor,
    b: Option<Tensor>,
    pad: usize,
    stride: usize,
}

impl Conv {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, c, h, wd) = x.dims4()?;
        let (_, _, kh, kw) = self.w.dims4()?;
        let s = self.stride.max(1);
        if h + 2 * self.pad < kh || wd + 2 * self.pad < kw {
            return Err(err("input smaller than a kernel"));
        }
        let y = if s == 1 && (kh, kw) != (1, 1) && x.device().is_cpu() {
            // candle 0.9.2's tiled CPU convolution is slow on large maps (a 64 → 24 channel 3×3 at
            // 1024² takes 12 s) and mistakes a contiguous NCHW input for NHWC when the strides
            // coincide (channels = height = width, e.g. 64 channels at 64²)
            sampled_conv(x, &self.w, self.pad, None)?
        } else if c == h && c == wd && (kh, kw) != (1, 1) {
            // (the stride coincidence above: a zero column on the right breaks the tie)
            let wo = (wd + 2 * self.pad - kw) / s + 1;
            x.pad_with_zeros(3, 0, s)?.conv2d(&self.w, self.pad, s, 1, 1)?.narrow(3, 0, wo)?
        } else {
            x.conv2d(&self.w, self.pad, s, 1, 1)?
        };
        Ok(match &self.b {
            Some(b) => y.broadcast_add(b)?,
            None => y,
        })
    }
}

/// `F.interpolate(x, size, mode='bilinear', align_corners=True)` (identity when the size matches).
fn resize(x: &Tensor, h: usize, w: usize) -> Result<Tensor> {
    let (_, _, xh, xw) = x.dims4()?;
    if (xh, xw) == (h, w) {
        return Ok(x.clone());
    }
    if xh == 1 && xw == 1 {
        let (n, c, _, _) = x.dims4()?;
        return Ok(x.broadcast_as((n, c, h, w))?.contiguous()?);
    }
    Ok(x.upsample_bilinear2d(h, w, true)?)
}

// ---------------------------------------------------------------------------------------------
// Swin Transformer v1

/// The backbone's sizes, read from the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwinConfig {
    pub embed: usize,
    pub depths: Vec<usize>,
    pub heads: Vec<usize>,
    pub window: usize,
}

impl SwinConfig {
    fn read(p: &P) -> Result<Self> {
        let embed = *p.shape("patch_embed.proj.weight")?.first().ok_or_else(|| err("patch embedding"))?;
        let mut depths = Vec::new();
        let mut heads = Vec::new();
        let mut window = 0;
        for i in 0..8 {
            let mut d = 0;
            while p.has(&format!("layers.{i}.blocks.{d}.norm1.weight")) {
                d += 1;
            }
            if d == 0 {
                break;
            }
            let t = p.shape(&format!("layers.{i}.blocks.0.attn.relative_position_bias_table"))?;
            let [rows, h] = t[..] else { return Err(err("relative position table")) };
            let side = (rows as f64).sqrt().round() as usize;
            if side * side != rows || side.is_multiple_of(2) {
                return Err(err("relative position table is not (2w−1)²"));
            }
            window = side.div_ceil(2);
            depths.push(d);
            heads.push(h);
        }
        if depths.len() != 4 || embed == 0 || window == 0 {
            return Err(err("not a four-stage Swin backbone"));
        }
        Ok(SwinConfig { embed, depths, heads, window })
    }
}

struct WindowAttention {
    qkv: Linear,
    proj: Linear,
    heads: usize,
    /// Relative position bias `[1, heads, N, N]`.
    bias: Tensor,
}

impl WindowAttention {
    fn new(p: &P, heads: usize, window: usize) -> Result<Self> {
        let table = p.get("relative_position_bias_table")?;
        let n = window * window;
        // relative position index (as in the paper): (dy + w − 1)·(2w − 1) + (dx + w − 1)
        let mut idx = Vec::with_capacity(n * n);
        for i in 0..n {
            for j in 0..n {
                let (yi, xi, yj, xj) = (i / window, i % window, j / window, j % window);
                let dy = yi + window - 1 - yj;
                let dx = xi + window - 1 - xj;
                idx.push((dy * (2 * window - 1) + dx) as u32);
            }
        }
        let idx = Tensor::from_vec(idx, n * n, table.device())?;
        let bias = table.index_select(&idx, 0)?.reshape((n, n, heads))?.permute((2, 0, 1))?.contiguous()?.unsqueeze(0)?;
        Ok(Self { qkv: p.pp("qkv").linear(true)?, proj: p.pp("proj").linear(true)?, heads, bias })
    }

    /// `x`: `[nW, N, C]`; `mask`: `[nW, 1, N, N]` (0 / −100) for shifted windows.
    fn forward(&self, x: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let (nw, n, c) = x.dims3()?;
        let hd = c / self.heads;
        let qkv = self.qkv.forward(x)?.reshape((nw, n, 3, self.heads, hd))?.permute((2, 0, 3, 1, 4))?;
        let q = (qkv.get(0)?.contiguous()? * (hd as f64).powf(-0.5))?;
        let k = qkv.get(1)?.contiguous()?;
        let v = qkv.get(2)?.contiguous()?;
        let mut att = q.matmul(&k.t()?)?.broadcast_add(&self.bias)?;
        if let Some(m) = mask {
            att = att.broadcast_add(m)?;
        }
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        let o = att.matmul(&v)?.transpose(1, 2)?.reshape((nw, n, c))?;
        Ok(self.proj.forward(&o)?)
    }
}

struct SwinBlock {
    norm1: LayerNorm,
    attn: WindowAttention,
    norm2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    shift: usize,
}

/// `[1, Hp, Wp, C]` → `[nW, w·w, C]`.
fn partition(x: &Tensor, w: usize) -> Result<Tensor> {
    let (_, hp, wp, c) = x.dims4()?;
    Ok(x.reshape((hp / w, w, wp / w, w, c))?.permute((0, 2, 1, 3, 4))?.reshape(((hp / w) * (wp / w), w * w, c))?)
}

/// [`partition`] reversed.
fn unpartition(x: &Tensor, w: usize, hp: usize, wp: usize) -> Result<Tensor> {
    let c = x.dim(2)?;
    Ok(x.reshape((hp / w, wp / w, w, w, c))?.permute((0, 2, 1, 3, 4))?.reshape((1, hp, wp, c))?)
}

/// `torch.roll(x, -s, dim)` (`forward`) or `torch.roll(x, s, dim)`.
fn roll(x: &Tensor, s: usize, dim: usize, forward: bool) -> Result<Tensor> {
    let n = x.dim(dim)?;
    let s = if n == 0 { 0 } else { s % n };
    if s == 0 {
        return Ok(x.clone());
    }
    let k = if forward { s } else { n - s };
    Ok(Tensor::cat(&[x.narrow(dim, k, n - k)?, x.narrow(dim, 0, k)?], dim)?)
}

impl SwinBlock {
    fn new(p: &P, heads: usize, window: usize, shift: usize) -> Result<Self> {
        Ok(Self {
            norm1: p.pp("norm1").ln()?,
            attn: WindowAttention::new(&p.pp("attn"), heads, window)?,
            norm2: p.pp("norm2").ln()?,
            fc1: p.pp("mlp.fc1").linear(true)?,
            fc2: p.pp("mlp.fc2").linear(true)?,
            shift,
        })
    }

    /// `x`: `[1, H, W, C]`.
    fn forward(&self, x: &Tensor, window: usize, mask: &Tensor) -> Result<Tensor> {
        let (_, h, w, _) = x.dims4()?;
        let (hp, wp) = (h.div_ceil(window) * window, w.div_ceil(window) * window);
        let y = self.norm1.forward(x)?;
        let y = y.pad_with_zeros(1, 0, hp - h)?.pad_with_zeros(2, 0, wp - w)?;
        let y = if self.shift > 0 { roll(&roll(&y, self.shift, 1, true)?, self.shift, 2, true)? } else { y };
        let win = partition(&y, window)?;
        let att = self.attn.forward(&win, (self.shift > 0).then_some(mask))?;
        let y = unpartition(&att, window, hp, wp)?;
        let y = if self.shift > 0 { roll(&roll(&y, self.shift, 1, false)?, self.shift, 2, false)? } else { y };
        let y = y.narrow(1, 0, h)?.narrow(2, 0, w)?;
        let x = (x + y)?;
        let m = self.fc2.forward(&self.fc1.forward(&self.norm2.forward(&x)?)?.gelu_erf()?)?;
        Ok((x + m)?)
    }
}

/// The shifted-window attention mask `[nW, 1, N, N]` of an `hp × wp` (padded) map.
fn shift_mask(hp: usize, wp: usize, window: usize, shift: usize, dev: &Device) -> Result<Tensor> {
    let region = |i: usize, n: usize| -> usize {
        if i < n.saturating_sub(window) {
            0
        } else if i < n.saturating_sub(shift) {
            1
        } else {
            2
        }
    };
    let (nh, nw) = (hp / window, wp / window);
    let n = window * window;
    let mut out = vec![0f32; nh * nw * n * n];
    let mut ids = vec![0usize; n];
    for wy in 0..nh {
        for wx in 0..nw {
            for (k, id) in ids.iter_mut().enumerate() {
                let (y, x) = (wy * window + k / window, wx * window + k % window);
                *id = region(y, hp) * 3 + region(x, wp);
            }
            let base = (wy * nw + wx) * n * n;
            for i in 0..n {
                for j in 0..n {
                    if ids.get(i) != ids.get(j)
                        && let Some(v) = out.get_mut(base + i * n + j)
                    {
                        *v = -100.0;
                    }
                }
            }
        }
    }
    Ok(Tensor::from_vec(out, (nh * nw, 1, n, n), dev)?)
}

struct PatchMerging {
    norm: LayerNorm,
    reduction: Linear,
}

impl PatchMerging {
    /// `[1, H, W, C]` → `[1, ⌈H/2⌉, ⌈W/2⌉, 2C]`.
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, h, w, c) = x.dims4()?;
        let x = x.pad_with_zeros(1, 0, h % 2)?.pad_with_zeros(2, 0, w % 2)?;
        let (h2, w2) = (h.div_ceil(2), w.div_ceil(2));
        let x = x.reshape((h2, 2, w2, 2, c))?;
        let pick = |dy: usize, dx: usize| -> Result<Tensor> { Ok(x.narrow(1, dy, 1)?.narrow(3, dx, 1)?.reshape((1, h2, w2, c))?) };
        // x0 = [0::2, 0::2], x1 = [1::2, 0::2], x2 = [0::2, 1::2], x3 = [1::2, 1::2]
        let x = Tensor::cat(&[pick(0, 0)?, pick(1, 0)?, pick(0, 1)?, pick(1, 1)?], 3)?;
        Ok(self.reduction.forward(&self.norm.forward(&x)?)?)
    }
}

struct Stage {
    blocks: Vec<SwinBlock>,
    down: Option<PatchMerging>,
    norm: LayerNorm,
}

struct Swin {
    cfg: SwinConfig,
    patch: Conv,
    patch_norm: LayerNorm,
    stages: Vec<Stage>,
}

impl Swin {
    fn new(p: &P) -> Result<Self> {
        let cfg = SwinConfig::read(p)?;
        let mut stages = Vec::new();
        for (i, (&depth, &heads)) in cfg.depths.iter().zip(&cfg.heads).enumerate() {
            let lp = p.pp(format!("layers.{i}"));
            let blocks = (0..depth)
                .map(|j| SwinBlock::new(&lp.pp(format!("blocks.{j}")), heads, cfg.window, if j % 2 == 0 { 0 } else { cfg.window / 2 }))
                .collect::<Result<Vec<_>>>()?;
            let down = if lp.has("downsample.reduction.weight") {
                Some(PatchMerging { norm: lp.pp("downsample.norm").ln()?, reduction: lp.pp("downsample.reduction").linear(false)? })
            } else {
                None
            };
            stages.push(Stage { blocks, down, norm: p.pp(format!("norm{i}")).ln()? });
        }
        Ok(Self { patch: p.pp("patch_embed.proj").conv(0, 4)?, patch_norm: p.pp("patch_embed.norm").ln()?, stages, cfg })
    }

    /// The four stages' features (NCHW) of `x` (`[1, 3, H, W]`).
    fn forward(&self, x: &Tensor) -> Result<Vec<Tensor>> {
        let (_, _, h, w) = x.dims4()?;
        let x = x.pad_with_zeros(2, 0, (4 - h % 4) % 4)?.pad_with_zeros(3, 0, (4 - w % 4) % 4)?;
        let x = self.patch.forward(&x)?.permute((0, 2, 3, 1))?;
        let mut x = self.patch_norm.forward(&x)?;
        let win = self.cfg.window;
        let mut outs = Vec::with_capacity(self.stages.len());
        for st in &self.stages {
            let (_, h, w, _) = x.dims4()?;
            let (hp, wp) = (h.div_ceil(win) * win, w.div_ceil(win) * win);
            let mask = shift_mask(hp, wp, win, win / 2, x.device())?;
            for b in &st.blocks {
                x = b.forward(&x, win, &mask)?;
            }
            outs.push(st.norm.forward(&x)?.permute((0, 3, 1, 2))?.contiguous()?);
            if let Some(d) = &st.down {
                x = d.forward(&x)?;
            }
        }
        Ok(outs)
    }
}

// ---------------------------------------------------------------------------------------------
// Decoder

/// Modulated deformable convolution (`torchvision.ops.deform_conv2d` with a mask, stride 1).
struct DeformConv {
    offset: Conv,
    modulator: Conv,
    /// Regular weights `[O, C, kh, kw]`.
    w: Tensor,
    pad: usize,
}

impl DeformConv {
    fn new(p: &P, pad: usize) -> Result<Self> {
        Ok(Self { offset: p.pp("offset_conv").conv(pad, 1)?, modulator: p.pp("modulator_conv").conv(pad, 1)?, w: p.get("regular_conv.weight")?, pad })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let off = self.offset.forward(x)?;
        let modu = (candle_nn::ops::sigmoid(&self.modulator.forward(x)?)? * 2.0)?;
        deform_conv(x, &off, &modu, &self.w, self.pad)
    }
}

/// `x`: `[1, C, H, W]`; `off`: `[1, 2K, H, W]` (dy, dx per kernel tap); `modu`: `[1, K, H, W]`;
/// `w`: `[O, C, kh, kw]`; stride 1, output `H × W` (`pad` = kernel / 2). Each tap samples the
/// input bilinearly at its offset position (zero outside), scales by the modulation, and the
/// taps' samples are summed through the weights.
fn deform_conv(x: &Tensor, off: &Tensor, modu: &Tensor, w: &Tensor, pad: usize) -> Result<Tensor> {
    sampled_conv(x, w, pad, Some((off, modu)))
}

/// Most im2col elements built at once (the rest runs in strips of output rows).
const COLS_BLOCK: usize = 32 << 20;

/// A stride-1 convolution as im2col + matrix product, the columns built on the CPU in parallel
/// (bounded strips). With `deform`, each tap samples at its offset (bilinear) times its
/// modulation; without, at the integer tap position (a plain convolution with zero padding).
fn sampled_conv(x: &Tensor, w: &Tensor, pad: usize, deform: Option<(&Tensor, &Tensor)>) -> Result<Tensor> {
    use rayon::prelude::*;
    let (_, c, h, wd) = x.dims4()?;
    let (o, wc, kh, kw) = w.dims4()?;
    if wc != c || h + 2 * pad < kh || wd + 2 * pad < kw {
        return Err(err("convolution: shape mismatch"));
    }
    let (ho, wo) = (h + 2 * pad - kh + 1, wd + 2 * pad - kw + 1);
    let k = kh * kw;
    let hw = h * wd;
    let dev = x.device().clone();
    let xv = crate::nn::to_vec(x)?;
    let (offv, modv) = match deform {
        Some((off, m)) => {
            if (ho, wo) != (h, wd) || off.dims4()? != (1, 2 * k, h, wd) || m.dims4()? != (1, k, h, wd) {
                return Err(err("deformable convolution: offset shape"));
            }
            (crate::nn::to_vec(off)?, crate::nn::to_vec(m)?)
        }
        None => (Vec::new(), Vec::new()),
    };
    let wf = w.reshape((o, c * k))?;
    let rows = (COLS_BLOCK / (c * k * wo).max(1)).clamp(1, ho);
    let mut outs = Vec::with_capacity(ho.div_ceil(rows));
    let mut r0 = 0;
    while r0 < ho {
        let nr = rows.min(ho - r0);
        let n = nr * wo;
        // per tap and output pixel: four (index, weight) samples
        let mut taps = vec![[(0u32, 0f32); 4]; k * n];
        taps.par_chunks_mut(n).enumerate().for_each(|(t, tap)| {
            let (ky, kx) = ((t / kw) as f32, (t % kw) as f32);
            for (i, s) in tap.iter_mut().enumerate() {
                let (oy, ox) = (r0 + i / wo, i % wo);
                let mut py = oy as f32 - pad as f32 + ky;
                let mut px = ox as f32 - pad as f32 + kx;
                let mut m = 1.0;
                if deform.is_some() {
                    let p = oy * wo + ox;
                    py += offv.get(2 * t * hw + p).copied().unwrap_or(0.0);
                    px += offv.get((2 * t + 1) * hw + p).copied().unwrap_or(0.0);
                    m = modv.get(t * hw + p).copied().unwrap_or(0.0);
                }
                let (y0, x0) = (py.floor(), px.floor());
                let (ly, lx) = (py - y0, px - x0);
                let corners = [(0.0, 0.0, (1.0 - ly) * (1.0 - lx)), (0.0, 1.0, (1.0 - ly) * lx), (1.0, 0.0, ly * (1.0 - lx)), (1.0, 1.0, ly * lx)];
                for (slot, (dy, dx, cw)) in s.iter_mut().zip(corners) {
                    let (yy, xx) = (y0 + dy, x0 + dx);
                    *slot = if yy >= 0.0 && yy < h as f32 && xx >= 0.0 && xx < wd as f32 && cw != 0.0 {
                        ((yy as usize * wd + xx as usize) as u32, cw * m)
                    } else {
                        (0, 0.0)
                    };
                }
            }
        });
        // columns: row (channel, tap), one value per output pixel
        let mut cols = vec![0f32; c * k * n];
        cols.par_chunks_mut(n).enumerate().for_each(|(row, col)| {
            let (ch, t) = (row / k, row % k);
            let plane = xv.get(ch * hw..(ch + 1) * hw).unwrap_or_default();
            let tap = taps.get(t * n..(t + 1) * n).unwrap_or_default();
            for (v, s) in col.iter_mut().zip(tap) {
                *v = s.iter().map(|&(i, wt)| wt * plane.get(i as usize).copied().unwrap_or(0.0)).sum();
            }
        });
        let cols = Tensor::from_vec(cols, (c * k, n), &dev)?;
        outs.push(wf.matmul(&cols)?.reshape((1, o, nr, wo))?);
        r0 += nr;
    }
    Ok(Tensor::cat(&outs, 2)?)
}

/// `_ASPPModuleDeformable`: deformable conv → BN → ReLU.
struct AsppBranch {
    conv: DeformConv,
    bn: Option<Affine>,
}

impl AsppBranch {
    fn new(p: &P) -> Result<Self> {
        let k = *p.shape("atrous_conv.regular_conv.weight")?.get(2).ok_or_else(|| err("ASPP kernel"))?;
        Ok(Self { conv: DeformConv::new(&p.pp("atrous_conv"), k / 2)?, bn: p.pp("bn").bn()? })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(bn(self.conv.forward(x)?, &self.bn)?.relu()?)
    }
}

/// `ASPPDeformable`: a 1×1 branch, deformable branches (1, 3, 7), global pooling; fused by a 1×1.
struct Aspp {
    aspp1: AsppBranch,
    deforms: Vec<AsppBranch>,
    pool_conv: Conv,
    pool_bn: Option<Affine>,
    conv1: Conv,
    bn1: Option<Affine>,
}

impl Aspp {
    fn new(p: &P) -> Result<Self> {
        let mut deforms = Vec::new();
        while p.has(&format!("aspp_deforms.{}.atrous_conv.regular_conv.weight", deforms.len())) {
            deforms.push(AsppBranch::new(&p.pp(format!("aspp_deforms.{}", deforms.len())))?);
        }
        Ok(Self {
            aspp1: AsppBranch::new(&p.pp("aspp1"))?,
            deforms,
            pool_conv: p.pp("global_avg_pool.1").conv(0, 1)?,
            pool_bn: p.pp("global_avg_pool.2").bn()?,
            conv1: p.pp("conv1").conv(0, 1)?,
            bn1: p.pp("bn1").bn()?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, _, h, w) = x.dims4()?;
        let mut parts = vec![self.aspp1.forward(x)?];
        for d in &self.deforms {
            parts.push(d.forward(x)?);
        }
        let pooled = x.mean_keepdim(2)?.mean_keepdim(3)?;
        let pooled = bn(self.pool_conv.forward(&pooled)?, &self.pool_bn)?.relu()?;
        parts.push(resize(&pooled, h, w)?);
        let x = Tensor::cat(&parts, 1)?;
        Ok(bn(self.conv1.forward(&x)?, &self.bn1)?.relu()?)
    }
}

/// `BasicDecBlk`: 3×3 conv → BN → ReLU → deformable ASPP → 3×3 conv → BN.
struct DecBlock {
    conv_in: Conv,
    bn_in: Option<Affine>,
    att: Option<Aspp>,
    conv_out: Conv,
    bn_out: Option<Affine>,
}

impl DecBlock {
    fn new(p: &P) -> Result<Self> {
        if p.has("conv_resi.weight") {
            return Err(err("residual decoder blocks are not supported"));
        }
        let att = if p.has("dec_att.conv1.weight") {
            if !p.has("dec_att.aspp1.atrous_conv.offset_conv.weight") {
                return Err(err("only the deformable ASPP decoder is supported"));
            }
            Some(Aspp::new(&p.pp("dec_att"))?)
        } else {
            None
        };
        Ok(Self {
            conv_in: p.pp("conv_in").conv(1, 1)?,
            bn_in: p.pp("bn_in").bn()?,
            att,
            conv_out: p.pp("conv_out").conv(1, 1)?,
            bn_out: p.pp("bn_out").bn()?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mut x = bn(self.conv_in.forward(x)?, &self.bn_in)?.relu()?;
        if let Some(a) = &self.att {
            x = a.forward(&x)?;
        }
        bn(self.conv_out.forward(&x)?, &self.bn_out)
    }
}

/// `SimpleConvs`: two 3×3 convolutions (no activation between, as in the reference).
struct SimpleConvs {
    conv1: Conv,
    conv_out: Conv,
}

impl SimpleConvs {
    fn new(p: &P) -> Result<Self> {
        Ok(Self { conv1: p.pp("conv1").conv(1, 1)?, conv_out: p.pp("conv_out").conv(1, 1)? })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.conv_out.forward(&self.conv1.forward(x)?)
    }
}

/// The gradient-attention gate of a decoder stage: `p · σ(attn(relu(bn(conv(p)))))`.
struct Gate {
    conv: Conv,
    bn: Option<Affine>,
    attn: Conv,
}

impl Gate {
    fn new(p: &P, level: usize) -> Result<Self> {
        Ok(Self {
            conv: p.pp(format!("gdt_convs_{level}.0")).conv(1, 1)?,
            bn: p.pp(format!("gdt_convs_{level}.1")).bn()?,
            attn: p.pp(format!("gdt_convs_attn_{level}.0")).conv(0, 1)?,
        })
    }

    fn forward(&self, p: &Tensor) -> Result<Tensor> {
        let g = bn(self.conv.forward(p)?, &self.bn)?.relu()?;
        let a = candle_nn::ops::sigmoid(&self.attn.forward(&g)?)?;
        Ok(p.broadcast_mul(&a)?)
    }
}

/// `'b c (hg h) (wg w) -> b (c hg wg) h w'` with the patch grid of `ref_h × ref_w`, resized to it.
fn patches(x: &Tensor, ref_h: usize, ref_w: usize) -> Result<Tensor> {
    let (_, c, h, w) = x.dims4()?;
    let (gh, gw) = (h / ref_h.max(1), w / ref_w.max(1));
    if gh == 0 || gw == 0 || h % gh != 0 || w % gw != 0 {
        return Err(err("input side must be a multiple of 32"));
    }
    let (ph, pw) = (h / gh, w / gw);
    let p = x.reshape((1, c, gh, ph, gw, pw))?.permute((0, 1, 2, 4, 3, 5))?.reshape((1, c * gh * gw, ph, pw))?;
    resize(&p, ref_h, ref_w)
}

struct Decoder {
    ipt: [SimpleConvs; 5],
    blocks: [DecBlock; 4],
    laterals: [Conv; 3],
    gates: Option<[Gate; 3]>,
    out: Conv,
}

impl Decoder {
    fn new(p: &P) -> Result<Self> {
        let ipt = |i: usize| SimpleConvs::new(&p.pp(format!("ipt_blk{i}")));
        let blk = |i: usize| DecBlock::new(&p.pp(format!("decoder_block{i}")));
        let lat = |i: usize| p.pp(format!("lateral_block{i}.conv")).conv(0, 1);
        if !p.has("ipt_blk5.conv1.weight") {
            return Err(err("decoders without the image-patch input are not supported"));
        }
        let gates = if p.has("gdt_convs_4.0.weight") { Some([Gate::new(p, 4)?, Gate::new(p, 3)?, Gate::new(p, 2)?]) } else { None };
        Ok(Self {
            ipt: [ipt(1)?, ipt(2)?, ipt(3)?, ipt(4)?, ipt(5)?],
            blocks: [blk(1)?, blk(2)?, blk(3)?, blk(4)?],
            laterals: [lat(2)?, lat(3)?, lat(4)?],
            gates,
            out: p.pp("conv_out1.0").conv(0, 1)?,
        })
    }

    fn forward(&self, x: &Tensor, f: [&Tensor; 4]) -> Result<Tensor> {
        let [x1, x2, x3, x4] = f;
        let [ipt1, ipt2, ipt3, ipt4, ipt5] = &self.ipt;
        let [b1, b2, b3, b4] = &self.blocks;
        let [lat2, lat3, lat4] = &self.laterals;
        let gate = |i: usize, p: Tensor| -> Result<Tensor> {
            match &self.gates {
                Some(g) => g.get(i).ok_or_else(|| err("gate"))?.forward(&p),
                None => Ok(p),
            }
        };
        let dims = |t: &Tensor| -> Result<(usize, usize)> {
            let (_, _, h, w) = t.dims4()?;
            Ok((h, w))
        };
        let with_patches = |p: Tensor, blk: &SimpleConvs| -> Result<Tensor> {
            let (h, w) = dims(&p)?;
            let ip = blk.forward(&patches(x, h, w)?)?;
            Ok(Tensor::cat(&[p, ip], 1)?)
        };

        let p4 = gate(0, b4.forward(&with_patches(x4.clone(), ipt5)?)?)?;
        let (h3, w3) = dims(x3)?;
        let p3 = (resize(&p4, h3, w3)? + lat4.forward(x3)?)?;
        let p3 = gate(1, b3.forward(&with_patches(p3, ipt4)?)?)?;
        let (h2, w2) = dims(x2)?;
        let p2 = (resize(&p3, h2, w2)? + lat3.forward(x2)?)?;
        let p2 = gate(2, b2.forward(&with_patches(p2, ipt3)?)?)?;
        let (h1, w1) = dims(x1)?;
        let p1 = (resize(&p2, h1, w1)? + lat2.forward(x1)?)?;
        let p1 = b1.forward(&with_patches(p1, ipt2)?)?;
        let (h, w) = dims(x)?;
        let p1 = with_patches(resize(&p1, h, w)?, ipt1)?;
        self.out.forward(&p1)
    }
}

// ---------------------------------------------------------------------------------------------

/// A loaded BiRefNet.
pub struct BiRefNet {
    device: Device,
    bb: Swin,
    /// The backbone also runs on the half-size image (features concatenated per stage).
    multi_scale: bool,
    squeeze: Vec<DecBlock>,
    decoder: Decoder,
    input: usize,
}

/// A predicted matte: `side²` probabilities (0..1) over the square input the image was stretched to.
#[derive(Clone, Debug)]
pub struct Matte {
    pub side: usize,
    pub data: Vec<f32>,
}

impl BiRefNet {
    /// Load `model.safetensors` from `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, crate::best_device())
    }

    /// Load `model.safetensors` from `dir` on the CPU (the fallback when a GPU run fails).
    pub fn load_cpu(dir: &Path) -> Result<Self> {
        Self::load_on(dir, Device::Cpu)
    }

    /// Whether the model runs on the CPU.
    pub fn on_cpu(&self) -> bool {
        self.device().is_cpu()
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(err(format!("no {WEIGHTS_FILE} in {}", dir.display())));
        }
        Self::from_file(&dir.join(WEIGHTS_FILE), device)
    }

    /// Load from a safetensors file.
    pub fn from_file(path: &Path, device: Device) -> Result<Self> {
        let w = Weights::open(path)?;
        let root = P { w, prefix: String::new(), dev: device.clone() };
        let bb = Swin::new(&root.pp("bb"))?;
        let embed = bb.cfg.embed;
        let lat_in = *root.shape("decoder.lateral_block2.conv.weight")?.get(1).ok_or_else(|| err("lateral block"))?;
        let multi_scale = match lat_in {
            v if v == 2 * embed => true,
            v if v == embed => false,
            _ => return Err(err("unsupported multi-scale input")),
        };
        // the three coarser stages are concatenated onto the deepest one (cxt_num = 3)
        let stage = |i: u32| embed * 2usize.pow(i) * if multi_scale { 2 } else { 1 };
        let want = stage(0) + stage(1) + stage(2) + stage(3);
        let mut squeeze = Vec::new();
        while root.has(&format!("squeeze_module.{}.conv_in.weight", squeeze.len())) {
            squeeze.push(DecBlock::new(&root.pp(format!("squeeze_module.{}", squeeze.len())))?);
        }
        let first_in = match squeeze.first() {
            Some(_) => root.shape("squeeze_module.0.conv_in.weight")?.get(1).copied(),
            None => root.shape("decoder.decoder_block4.conv_in.weight")?.get(1).copied().map(|c| c.saturating_sub(stage(3) / 8)),
        };
        if first_in != Some(want) {
            return Err(err("unsupported context configuration"));
        }
        let decoder = Decoder::new(&root.pp("decoder"))?;
        Ok(Self { device, bb, multi_scale, squeeze, decoder, input: DEFAULT_INPUT })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The backbone's sizes.
    pub fn swin(&self) -> &SwinConfig {
        &self.bb.cfg
    }

    /// Side of the square input (a multiple of 32, at least 64).
    pub fn input(&self) -> usize {
        self.input
    }

    /// Run at another input side (rounded down to a multiple of 32; smaller is faster and coarser).
    pub fn set_input(&mut self, side: usize) {
        self.input = (side / 32 * 32).clamp(64, 4096);
    }

    /// Predict the matte of an RGB image (8-bit, row-major, `w × h`), stretched to the square input.
    pub fn predict(&self, rgb: &[u8], w: usize, h: usize) -> Result<Matte> {
        let s = self.input;
        let px = preprocess(rgb, w, h, s)?;
        let x = Tensor::from_vec(px, (1, 3, s, s), &self.device)?;
        let logits = self.forward(&x)?;
        let data = crate::nn::to_vec(&candle_nn::ops::sigmoid(&logits)?)?;
        Ok(Matte { side: s, data })
    }

    /// The backbone's four stage outputs (NCHW) on `x` alone (tests).
    pub fn backbone(&self, x: &Tensor) -> Result<Vec<Tensor>> {
        self.bb.forward(&x.to_device(&self.device)?)
    }

    /// Logits `[1, 1, S, S]` of normalized pixels `[1, 3, S, S]` (S a multiple of 32).
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, c, h, w) = x.dims4()?;
        if c != 3 || h % 32 != 0 || w % 32 != 0 || h < 64 || w < 64 {
            return Err(err("input must be [1, 3, S, S] with S a multiple of 32"));
        }
        let x = x.to_device(&self.device)?;
        let profile = lcc_brand::env_os("PROFILE").is_some();
        let t = std::time::Instant::now();
        let mark = |what: &str| {
            if profile {
                eprintln!("birefnet {what}: {:?}", t.elapsed());
            }
        };
        let mut f = self.bb.forward(&x)?;
        mark("backbone");
        if self.multi_scale {
            let half = resize(&x, h / 2, w / 2)?;
            let g = self.bb.forward(&half)?;
            mark("backbone (half size)");
            for (a, b) in f.iter_mut().zip(&g) {
                let (_, _, fh, fw) = a.dims4()?;
                *a = Tensor::cat(&[a.clone(), resize(b, fh, fw)?], 1)?;
            }
        }
        let [x1, x2, x3, x4] = <[Tensor; 4]>::try_from(f).map_err(|_| err("backbone stages"))?;
        let (_, _, h4, w4) = x4.dims4()?;
        let mut x4 = Tensor::cat(&[resize(&x1, h4, w4)?, resize(&x2, h4, w4)?, resize(&x3, h4, w4)?, x4], 1)?;
        for s in &self.squeeze {
            x4 = s.forward(&x4)?;
        }
        mark("squeeze");
        let out = self.decoder.forward(&x, [&x1, &x2, &x3, &x4]);
        mark("decoder");
        out
    }
}

/// An RGB image resized to `side²` (a triangle filter as wide as the scale, like PIL's bilinear
/// resize) and normalized for the model: `[3, side, side]` planar.
pub fn preprocess(rgb: &[u8], w: usize, h: usize, side: usize) -> Result<Vec<f32>> {
    let n = w.checked_mul(h).and_then(|n| n.checked_mul(3)).ok_or_else(|| err("image too large"))?;
    if w == 0 || h == 0 || side == 0 || rgb.len() < n {
        return Err(err("empty or short image"));
    }
    let src: Vec<f32> = rgb.get(..n).unwrap_or_default().iter().map(|&v| v as f32 / 255.0).collect();
    let horiz = resample_rows(&src, w, h, side);
    let out = resample_cols(&horiz, side, h, side);
    let mut planar = vec![0f32; 3 * side * side];
    for (i, px) in out.as_chunks::<3>().0.iter().enumerate() {
        for (c, v) in px.iter().enumerate() {
            if let Some(d) = planar.get_mut(c * side * side + i) {
                *d = (v - MEAN[c]) / STD[c];
            }
        }
    }
    Ok(planar)
}

/// Filter taps (first index, weights) for resampling `n` samples to `m`.
fn taps(n: usize, m: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = n as f64 / m as f64;
    let support = scale.max(1.0);
    (0..m)
        .map(|i| {
            let centre = (i as f64 + 0.5) * scale;
            let lo = ((centre - support).floor().max(0.0)) as usize;
            let hi = ((centre + support).ceil() as usize).min(n);
            let mut ws: Vec<f32> = (lo..hi)
                .map(|j| {
                    let d = ((j as f64 + 0.5 - centre) / support).abs();
                    (1.0 - d).max(0.0) as f32
                })
                .collect();
            let sum: f32 = ws.iter().sum();
            if sum > 0.0 {
                ws.iter_mut().for_each(|v| *v /= sum);
            }
            (lo, ws)
        })
        .collect()
}

fn resample_rows(src: &[f32], w: usize, h: usize, nw: usize) -> Vec<f32> {
    let t = taps(w, nw);
    let mut out = vec![0f32; nw * h * 3];
    for y in 0..h {
        for (x, (lo, ws)) in t.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (j, wt) in ws.iter().enumerate() {
                let i = (y * w + lo + j) * 3;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * src.get(i + c).copied().unwrap_or(0.0);
                }
            }
            if let Some(d) = out.get_mut((y * nw + x) * 3..(y * nw + x) * 3 + 3) {
                d.copy_from_slice(&acc);
            }
        }
    }
    out
}

fn resample_cols(src: &[f32], w: usize, h: usize, nh: usize) -> Vec<f32> {
    let t = taps(h, nh);
    let mut out = vec![0f32; w * nh * 3];
    for (y, (lo, ws)) in t.iter().enumerate() {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for (j, wt) in ws.iter().enumerate() {
                let i = ((lo + j) * w + x) * 3;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * src.get(i + c).copied().unwrap_or(0.0);
                }
            }
            if let Some(d) = out.get_mut((y * w + x) * 3..(y * w + x) * 3 + 3) {
                d.copy_from_slice(&acc);
            }
        }
    }
    out
}

/// A checkpoint of the BiRefNet_lite layout with tiny widths and random weights (tests of
/// the plumbing; Swin width `e`, window 3, one head and two blocks per stage).
#[doc(hidden)]
pub fn tiny_checkpoint(e: usize) -> Result<HashMap<String, Tensor>> {
    let mut specs: Vec<(String, Vec<usize>)> = Vec::new();
    let mut put = |name: String, shape: &[usize]| specs.push((name, shape.to_vec()));
    let ln = |put: &mut dyn FnMut(String, &[usize]), p: &str, d: usize| {
        put(format!("{p}.weight"), &[d]);
        put(format!("{p}.bias"), &[d]);
    };
    let conv = |put: &mut dyn FnMut(String, &[usize]), p: &str, o: usize, i: usize, k: usize, bias: bool| {
        put(format!("{p}.weight"), &[o, i, k, k]);
        if bias {
            put(format!("{p}.bias"), &[o]);
        }
    };
    let bn = |put: &mut dyn FnMut(String, &[usize]), p: &str, d: usize| {
        for n in ["weight", "bias", "running_mean", "running_var"] {
            put(format!("{p}.{n}"), &[d]);
        }
    };
    // Swin: window 3, one head per stage, two blocks per stage
    conv(&mut put, "bb.patch_embed.proj", e, 3, 4, true);
    ln(&mut put, "bb.patch_embed.norm", e);
    for i in 0..4 {
        let d = e << i;
        for j in 0..2 {
            let p = format!("bb.layers.{i}.blocks.{j}");
            ln(&mut put, &format!("{p}.norm1"), d);
            ln(&mut put, &format!("{p}.norm2"), d);
            put(format!("{p}.attn.qkv.weight"), &[3 * d, d]);
            put(format!("{p}.attn.qkv.bias"), &[3 * d]);
            put(format!("{p}.attn.proj.weight"), &[d, d]);
            put(format!("{p}.attn.proj.bias"), &[d]);
            put(format!("{p}.attn.relative_position_bias_table"), &[25, 1]);
            put(format!("{p}.mlp.fc1.weight"), &[4 * d, d]);
            put(format!("{p}.mlp.fc1.bias"), &[4 * d]);
            put(format!("{p}.mlp.fc2.weight"), &[d, 4 * d]);
            put(format!("{p}.mlp.fc2.bias"), &[d]);
        }
        if i < 3 {
            ln(&mut put, &format!("bb.layers.{i}.downsample.norm"), 4 * d);
            put(format!("bb.layers.{i}.downsample.reduction.weight"), &[2 * d, 4 * d]);
        }
        ln(&mut put, &format!("bb.norm{i}"), d);
    }
    let dec_block = |put: &mut dyn FnMut(String, &[usize]), p: &str, i: usize, o: usize| {
        conv(put, &format!("{p}.conv_in"), 64, i, 3, true);
        bn(put, &format!("{p}.bn_in"), 64);
        for (b, k) in [("aspp1", 1), ("aspp_deforms.0", 1), ("aspp_deforms.1", 3), ("aspp_deforms.2", 7)] {
            let q = format!("{p}.dec_att.{b}");
            conv(put, &format!("{q}.atrous_conv.offset_conv"), 2 * k * k, 64, k, true);
            conv(put, &format!("{q}.atrous_conv.modulator_conv"), k * k, 64, k, true);
            conv(put, &format!("{q}.atrous_conv.regular_conv"), 256, 64, k, false);
            bn(put, &format!("{q}.bn"), 256);
        }
        conv(put, &format!("{p}.dec_att.global_avg_pool.1"), 256, 64, 1, false);
        bn(put, &format!("{p}.dec_att.global_avg_pool.2"), 256);
        conv(put, &format!("{p}.dec_att.conv1"), 64, 1280, 1, false);
        bn(put, &format!("{p}.dec_att.bn1"), 64);
        conv(put, &format!("{p}.conv_out"), o, 64, 3, true);
        bn(put, &format!("{p}.bn_out"), o);
    };
    let [c0, c1, c2, c3] = [16 * e, 8 * e, 4 * e, 2 * e];
    dec_block(&mut put, "squeeze_module.0", c0 + c1 + c2 + c3, c0);
    dec_block(&mut put, "decoder.decoder_block4", c0 + c0 / 8, c1);
    dec_block(&mut put, "decoder.decoder_block3", c1 + c0 / 8, c2);
    dec_block(&mut put, "decoder.decoder_block2", c2 + c1 / 8, c3);
    dec_block(&mut put, "decoder.decoder_block1", c3 + c2 / 8, c3 / 2);
    for (i, inp, out) in [(5, 3072, c0 / 8), (4, 768, c0 / 8), (3, 192, c1 / 8), (2, 48, c2 / 8), (1, 3, c3 / 8)] {
        conv(&mut put, &format!("decoder.ipt_blk{i}.conv1"), 64, inp, 3, true);
        conv(&mut put, &format!("decoder.ipt_blk{i}.conv_out"), out, 64, 3, true);
    }
    for (i, ch) in [(4, c1), (3, c2), (2, c3)] {
        conv(&mut put, &format!("decoder.lateral_block{i}.conv"), ch, ch, 1, true);
        conv(&mut put, &format!("decoder.gdt_convs_{i}.0"), 16, ch, 3, true);
        bn(&mut put, &format!("decoder.gdt_convs_{i}.1"), 16);
        conv(&mut put, &format!("decoder.gdt_convs_attn_{i}.0"), 1, 16, 1, true);
    }
    conv(&mut put, "decoder.conv_out1.0", 1, c3 / 2 + c3 / 8, 1, true);
    let mut t = HashMap::new();
    for (name, shape) in specs {
        let r = (Tensor::randn(0f32, 1.0, shape.as_slice(), &Device::Cpu)? * 0.05)?;
        // batch-norm variances must be positive
        let r = if name.ends_with("running_var") { (r.abs()? + 1.0)? } else { r };
        t.insert(name, r);
    }
    Ok(t)
}

/// [`tiny_checkpoint`] written to `path` (tests of the plumbing elsewhere).
#[doc(hidden)]
pub fn write_tiny_checkpoint(path: &Path, e: usize) -> Result<()> {
    Ok(candle_core::safetensors::save(&tiny_checkpoint(e)?, path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(t: &Tensor) -> Vec<f32> {
        t.flatten_all().unwrap().to_vec1::<f32>().unwrap()
    }

    fn max_err(a: &Tensor, b: &Tensor) -> f32 {
        assert_eq!(a.dims(), b.dims());
        v(a).iter().zip(v(b)).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
    }

    /// The im2col convolution against candle's (on shapes candle gets right) and against
    /// one-row strips (on the shape candle 0.9.2 gets wrong: channels = height = width).
    #[test]
    fn sampled_conv_matches_a_plain_convolution() {
        for (c, o, s, k) in [(64, 16, 64, 3), (5, 4, 9, 3), (6, 3, 12, 7), (4, 2, 10, 1)] {
            let x = Tensor::randn(0f32, 1.0, (1, c, s, s + 3), &Device::Cpu).unwrap();
            let w = Tensor::randn(0f32, 1.0, (o, c, k, k), &Device::Cpu).unwrap();
            let p = k / 2;
            let a = sampled_conv(&x, &w, p, None).unwrap();
            let xp = x.pad_with_zeros(2, p, p).unwrap().pad_with_zeros(3, p, p).unwrap();
            let rows: Vec<Tensor> = (0..s).map(|r| xp.narrow(2, r, k).unwrap().conv2d(&w, 0, 1, 1, 1).unwrap()).collect();
            let b = Tensor::cat(&rows, 2).unwrap();
            assert!(max_err(&a, &b) < 1e-3, "{c}->{o} @{s} k{k}");
        }
    }

    /// Modulated deformable convolution against a direct per-pixel evaluation of its definition.
    #[test]
    fn deform_conv_samples_at_the_offsets() {
        let (c, o, h, w, k) = (3usize, 2usize, 6usize, 7usize, 3usize);
        let dev = Device::Cpu;
        let x = Tensor::randn(0f32, 1.0, (1, c, h, w), &dev).unwrap();
        let off = (Tensor::randn(0f32, 1.0, (1, 2 * k * k, h, w), &dev).unwrap() * 1.5).unwrap();
        let m = Tensor::rand(0f32, 1.0, (1, k * k, h, w), &dev).unwrap();
        let wt = Tensor::randn(0f32, 1.0, (o, c, k, k), &dev).unwrap();
        let got = v(&deform_conv(&x, &off, &m, &wt, 1).unwrap());
        let (xv, ov, mv, wv) = (v(&x), v(&off), v(&m), v(&wt));
        let at = |ch: usize, y: f32, xx: f32| -> f32 {
            if y <= -1.0 || y >= h as f32 || xx <= -1.0 || xx >= w as f32 {
                return 0.0;
            }
            let (y0, x0) = (y.floor(), xx.floor());
            let mut s = 0.0;
            for (yy, wy) in [(y0, 1.0 - (y - y0)), (y0 + 1.0, y - y0)] {
                for (xq, wx) in [(x0, 1.0 - (xx - x0)), (x0 + 1.0, xx - x0)] {
                    if yy >= 0.0 && yy < h as f32 && xq >= 0.0 && xq < w as f32 {
                        s += wy * wx * xv[ch * h * w + yy as usize * w + xq as usize];
                    }
                }
            }
            s
        };
        for oc in 0..o {
            for y in 0..h {
                for xx in 0..w {
                    let p = y * w + xx;
                    let mut want = 0.0;
                    for t in 0..k * k {
                        let py = y as f32 - 1.0 + (t / k) as f32 + ov[2 * t * h * w + p];
                        let px = xx as f32 - 1.0 + (t % k) as f32 + ov[(2 * t + 1) * h * w + p];
                        for ch in 0..c {
                            want += wv[(oc * c + ch) * k * k + t] * mv[t * h * w + p] * at(ch, py, px);
                        }
                    }
                    let g = got[oc * h * w + p];
                    assert!((g - want).abs() < 1e-3, "{oc} {y} {xx}: {g} vs {want}");
                }
            }
        }
    }

    #[test]
    fn roll_matches_torch() {
        let x = Tensor::arange(0f32, 5.0, &Device::Cpu).unwrap();
        assert_eq!(v(&roll(&x, 2, 0, true).unwrap()), [2.0, 3.0, 4.0, 0.0, 1.0]);
        assert_eq!(v(&roll(&x, 2, 0, false).unwrap()), [3.0, 4.0, 0.0, 1.0, 2.0]);
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-birefnet-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tiny_checkpoint_loads_and_predicts_a_matte() {
        let dir = tmp("tiny");
        candle_core::safetensors::save(&tiny_checkpoint(8).unwrap(), dir.join(WEIGHTS_FILE)).unwrap();
        let mut m = BiRefNet::load_on(&dir, Device::Cpu).unwrap();
        assert_eq!(*m.swin(), SwinConfig { embed: 8, depths: vec![2; 4], heads: vec![1; 4], window: 3 });
        m.set_input(64);
        let rgb: Vec<u8> = (0..50 * 30 * 3).map(|i| (i * 7 % 256) as u8).collect();
        let matte = m.predict(&rgb, 50, 30).unwrap();
        assert_eq!((matte.side, matte.data.len()), (64, 64 * 64));
        assert!(matte.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        // bad inputs are errors, never panics
        assert!(m.predict(&rgb, 50, 31).is_err());
        assert!(m.predict(&rgb, usize::MAX, 2).is_err());
        assert!(m.forward(&Tensor::zeros((1, 3, 48, 64), candle_core::DType::F32, &Device::Cpu).unwrap()).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn broken_checkpoints_are_errors() {
        let dir = tmp("broken");
        assert!(BiRefNet::load_on(&dir, Device::Cpu).is_err());
        std::fs::write(dir.join(WEIGHTS_FILE), b"not a safetensors file").unwrap();
        assert!(BiRefNet::load_on(&dir, Device::Cpu).is_err());
        // a checkpoint missing its decoder
        let mut t = tiny_checkpoint(8).unwrap();
        t.retain(|k, _| k.starts_with("bb."));
        candle_core::safetensors::save(&t, dir.join(WEIGHTS_FILE)).unwrap();
        assert!(BiRefNet::load_on(&dir, Device::Cpu).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
