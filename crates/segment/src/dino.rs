//! DINOv2-style ViT building blocks shared by the scene (EoMT) and depth (Depth Anything)
//! models: the pre-norm transformer layer with layer scale, the positional-embedding
//! interpolation, and the small convolutions their heads use.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch code of Hugging Face Transformers (`models/dinov2/modeling_dinov2.py`,
//! `models/eomt/modeling_eomt.py`), Copyright The HuggingFace Team, Meta Platforms, Inc. and the
//! EoMT authors; translated to Rust on candle and restructured. See NOTICE.

use std::path::Path;
use std::sync::Arc;

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use rayon::prelude::*;

use crate::nn::ln;
use crate::weights::{Backend, Weights};
use crate::{Error, Result, WEIGHTS_FILE};

/// The Hugging Face `config.json` of a model directory, when it is there, small and valid JSON.
pub fn read_config(dir: &Path) -> Option<serde_json::Value> {
    let path = dir.join("config.json");
    let len = std::fs::metadata(&path).ok()?.len();
    if len > 4 << 20 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
}

/// Whether `dir` holds `model.safetensors` and a `config.json` with this `model_type`.
pub fn is_model_dir(dir: &Path, model_type: &str) -> bool {
    dir.join(WEIGHTS_FILE).is_file() && read_config(dir).is_some_and(|c| c.get("model_type").and_then(|t| t.as_str()) == Some(model_type))
}

/// Open the checkpoint of `what` (a human-readable model name) in `dir`, checking its
/// `config.json` declares `model_type`; returns a weight builder on `device` and the config.
pub fn open_checkpoint<'a>(dir: &Path, what: &str, model_type: &str, device: &Device) -> Result<(VarBuilder<'a>, serde_json::Value)> {
    let missing = || Error::Message(format!("{what} model files not found in {} (expected model.safetensors and config.json)", dir.display()));
    if !dir.join(WEIGHTS_FILE).is_file() {
        return Err(missing());
    }
    let config = read_config(dir).ok_or_else(missing)?;
    let found = config.get("model_type").and_then(|t| t.as_str()).unwrap_or("?");
    if found != model_type {
        return Err(Error::Message(format!("{} holds a `{found}` model, not {what} (`{model_type}`)", dir.display())));
    }
    let weights: Arc<Weights> = Weights::open(&dir.join(WEIGHTS_FILE)).map_err(|e| match e {
        Error::Model(m) => Error::Message(format!("{what} model: {m}")),
        other => other,
    })?;
    Ok((VarBuilder::from_backend(Box::new(Backend(weights)), DType::F32, device.clone()), config))
}

/// A config entry as an integer, or an error naming it.
pub fn config_usize(config: &serde_json::Value, path: &[&str], what: &str) -> Result<usize> {
    let mut v = config;
    for p in path {
        v = v.get(p).ok_or_else(|| Error::Message(format!("{what} config.json: `{}` missing", path.join("."))))?;
    }
    v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| Error::Message(format!("{what} config.json: `{}` is not a count", path.join("."))))
}

/// Errors from building the model (missing or misshaped tensors) with the model's name.
pub fn named<T>(what: &str, r: Result<T>) -> Result<T> {
    r.map_err(|e| match e {
        Error::Candle(c) => Error::Message(format!("{what} model: unexpected weights ({c})")),
        Error::Model(m) => Error::Message(format!("{what} model: {m}")),
        other => other,
    })
}

/// Parameter names of a layer's attention projections (q, k, v, out).
pub struct AttnNames {
    pub q: &'static str,
    pub k: &'static str,
    pub v: &'static str,
    pub o: &'static str,
}

/// EoMT's names (`EomtAttention`).
pub const EOMT_ATTN: AttnNames = AttnNames { q: "attention.q_proj", k: "attention.k_proj", v: "attention.v_proj", o: "attention.out_proj" };
/// The Hugging Face DINOv2 checkpoint names (`Dinov2SelfAttention` + `Dinov2SelfOutput`).
pub const DINOV2_ATTN: AttnNames =
    AttnNames { q: "attention.attention.query", k: "attention.attention.key", v: "attention.attention.value", o: "attention.output.dense" };

/// A linear layer whose output rows are scaled by `scale` (layer scale folded into the weights:
/// `(x·Wᵀ + b)·λ = x·(λ·W)ᵀ + λ·b`).
fn scaled_linear(i: usize, o: usize, scale: &Tensor, vb: VarBuilder) -> Result<Linear> {
    let w = vb.get((o, i), "weight")?.broadcast_mul(&scale.reshape((o, 1))?)?;
    let b = vb.get(o, "bias")?.mul(scale)?;
    Ok(Linear::new(w, Some(b)))
}

/// `Dinov2Layer` / `EomtLayer`: x + λ₁·attn(norm1(x)); x + λ₂·mlp(norm2(x)).
pub struct Block {
    norm1: LayerNorm,
    /// q (pre-scaled by head_dim^-½), k and v in one matmul.
    qkv: Linear,
    o: Linear,
    norm2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    heads: usize,
    hidden: usize,
}

impl Block {
    pub fn new(hidden: usize, heads: usize, eps: f64, names: &AttnNames, vb: VarBuilder) -> Result<Self> {
        if heads == 0 || !hidden.is_multiple_of(heads) {
            return Err(Error::Message(format!("{hidden} channels do not split into {heads} heads")));
        }
        let ls1 = vb.get(hidden, "layer_scale1.lambda1")?;
        let ls2 = vb.get(hidden, "layer_scale2.lambda1")?;
        let scale = ((hidden / heads) as f64).powf(-0.5);
        let (mut ws, mut bs) = (Vec::with_capacity(3), Vec::with_capacity(3));
        for (i, n) in [names.q, names.k, names.v].iter().enumerate() {
            let s = if i == 0 { scale } else { 1.0 };
            ws.push((vb.get((hidden, hidden), &format!("{n}.weight"))? * s)?);
            bs.push((vb.get(hidden, &format!("{n}.bias"))? * s)?);
        }
        let qkv = Linear::new(Tensor::cat(&ws, 0)?, Some(Tensor::cat(&bs, 0)?));
        let inter = hidden.saturating_mul(4);
        let mvb = vb.pp("mlp");
        Ok(Self {
            norm1: ln(hidden, eps, vb.pp("norm1"))?,
            qkv,
            o: scaled_linear(hidden, hidden, &ls1, vb.pp(names.o))?,
            norm2: ln(hidden, eps, vb.pp("norm2"))?,
            fc1: candle_nn::linear(hidden, inter, mvb.pp("fc1"))?,
            fc2: scaled_linear(inter, hidden, &ls2, mvb.pp("fc2"))?,
            heads,
            hidden,
        })
    }

    /// `x`: `[B, N, hidden]`.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (b, n, _) = x.dims3()?;
        let head_dim = self.hidden / self.heads;
        let qkv = self.qkv.forward(&self.norm1.forward(x)?)?.reshape((b, n, 3, self.heads, head_dim))?.permute((2, 0, 3, 1, 4))?;
        let q = qkv.get(0)?.contiguous()?;
        let k = qkv.get(1)?.contiguous()?;
        let v = qkv.get(2)?.contiguous()?;
        let a = attention(&q, &k, &v)?;
        let x = (x + self.o.forward(&a)?)?;
        let y = self.fc2.forward(&gelu(&self.fc1.forward(&self.norm2.forward(&x)?)?)?)?;
        Ok((x + y)?)
    }
}

/// Query rows per attention work item on the CPU (scores of one block stay in cache).
const QUERY_BLOCK: usize = 128;

/// Softmax attention `softmax(q·kᵀ)·v` (q pre-scaled) on `[1, H, N, d]` tensors, returning the
/// heads merged as `[1, N, H·d]`. On the CPU, (head, query block) items run in parallel with
/// the softmax done in place; elsewhere plain tensor ops.
pub fn attention(q: &Tensor, k: &Tensor, v: &Tensor) -> Result<Tensor> {
    let (b, h, n, d) = q.dims4()?;
    if !q.device().is_cpu() || b != 1 {
        let att = candle_nn::ops::softmax_last_dim(&q.matmul(&k.t()?)?)?;
        return Ok(att.matmul(v)?.transpose(1, 2)?.reshape((b, n, h * d))?);
    }
    let blocks = n.div_ceil(QUERY_BLOCK);
    let tasks: Vec<(usize, usize)> = (0..h).flat_map(|hh| (0..blocks).map(move |bi| (hh, bi * QUERY_BLOCK))).collect();
    let (q, k, v) = (q.get(0)?, k.get(0)?, v.get(0)?);
    let outs: Vec<Result<Vec<f32>>> = tasks
        .par_iter()
        .map(|&(hh, start)| {
            let len = QUERY_BLOCK.min(n - start);
            let s = q.get(hh)?.narrow(0, start, len)?.matmul(&k.get(hh)?.t()?)?;
            let mut sv = s.flatten_all()?.to_vec1::<f32>()?;
            sv.chunks_exact_mut(n).for_each(softmax_row);
            let p = Tensor::from_vec(sv, (len, n), &Device::Cpu)?;
            Ok(p.matmul(&v.get(hh)?)?.flatten_all()?.to_vec1::<f32>()?)
        })
        .collect();
    let mut out = vec![0f32; n * h * d];
    for (&(hh, start), o) in tasks.iter().zip(outs) {
        for (i, row) in o?.chunks_exact(d).enumerate() {
            let at = (start + i) * h * d + hh * d;
            if let Some(dst) = out.get_mut(at..at + d) {
                dst.copy_from_slice(row);
            }
        }
    }
    Ok(Tensor::from_vec(out, (1, n, h * d), q.device())?)
}

/// In-place softmax of one row of scores.
fn softmax_row(row: &mut [f32]) {
    let m = row.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b));
    row.iter_mut().for_each(|x| *x = exp_nonpositive(*x - m));
    let mut acc = [0f32; 8];
    let (chunks, rest) = row.as_chunks::<8>();
    for c in chunks {
        for (a, x) in acc.iter_mut().zip(c) {
            *a += x;
        }
    }
    let sum: f32 = acc.iter().sum::<f32>() + rest.iter().sum::<f32>();
    let inv = if sum > 0.0 { 1.0 / sum } else { 0.0 };
    row.iter_mut().for_each(|x| *x *= inv);
}

/// `eˣ` for `x ≤ 0` (branch-free, so the compiler vectorizes it): range reduction by ln 2 and a
/// degree-6 Taylor polynomial, relative error ≈ 2·10⁻⁷; results below e⁻⁸⁷ flush to ≈ 0.
fn exp_nonpositive(x: f32) -> f32 {
    const LOG2E: f32 = std::f32::consts::LOG2_E;
    const LN2_HI: f32 = 0.693_145_75;
    const LN2_LO: f32 = 1.428_606_8e-6;
    // adding and subtracting 1.5·2²³ rounds to the nearest integer
    const ROUND: f32 = 12_582_912.0;
    let x = x.clamp(-87.0, 0.0);
    let n = (x * LOG2E + ROUND) - ROUND;
    let r = x - n * LN2_HI - n * LN2_LO;
    let p = 1.0 + r * (1.0 + r * (0.5 + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    // n ∈ [−126, 0]: 2ⁿ from its exponent bits
    p * f32::from_bits(((n as i32 + 127) as u32) << 23)
}

/// Elements per parallel work item of the CPU kernels.
const CHUNK: usize = 1 << 14;

/// Exact (erf) GELU; on the CPU in parallel (candle's element-wise ops run on one thread).
pub fn gelu(t: &Tensor) -> Result<Tensor> {
    if !t.device().is_cpu() {
        return Ok(t.gelu_erf()?);
    }
    let mut v = t.flatten_all()?.to_vec1::<f32>()?;
    v.par_chunks_mut(CHUNK).for_each(|c| c.iter_mut().for_each(|x| *x = gelu1(*x)));
    Ok(Tensor::from_vec(v, t.shape(), t.device())?)
}

fn gelu1(x: f32) -> f32 {
    (candle_core::cpu::erf::erf_f32(x * std::f32::consts::FRAC_1_SQRT_2) + 1.0) * 0.5 * x
}

/// Per-stage timings on stderr when `LIGHTCRAFT_PROFILE` is set (like the develop pipeline's).
pub struct Profile {
    on: bool,
    what: &'static str,
    last: std::time::Instant,
}

impl Profile {
    pub fn new(what: &'static str) -> Self {
        Self { on: std::env::var_os("LIGHTCRAFT_PROFILE").is_some(), what, last: std::time::Instant::now() }
    }

    /// Report the time since the previous mark (after waiting for `t` to be computed).
    pub fn mark(&mut self, stage: &str, t: &Tensor) {
        if self.on {
            let _ = t.device().synchronize();
            let now = std::time::Instant::now();
            eprintln!("[{}] {stage}: {:.1} ms", self.what, now.duration_since(self.last).as_secs_f64() * 1e3);
            self.last = now;
        }
    }
}

/// PyTorch's cubic convolution weights (A = −0.75) for fractional offset `t`.
fn cubic_weights(t: f64) -> [f64; 4] {
    let a = -0.75;
    let far = |x: f64| ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a;
    let near = |x: f64| ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0;
    [far(t + 1.0), near(t), near(1.0 - t), far(2.0 - t)]
}

/// Source taps and weights of `F.interpolate(mode="bicubic", align_corners=False)` (no
/// antialiasing) from `src` to `dst` samples.
fn bicubic_taps(src: usize, dst: usize) -> Vec<([usize; 4], [f64; 4])> {
    let scale = src as f64 / dst.max(1) as f64;
    let last = src.saturating_sub(1) as i64;
    (0..dst)
        .map(|o| {
            let x = (o as f64 + 0.5) * scale - 0.5;
            let i = x.floor();
            let w = cubic_weights(x - i);
            let i = i as i64;
            let idx = [i - 1, i, i + 1, i + 2].map(|j| j.clamp(0, last) as usize);
            (idx, w)
        })
        .collect()
}

/// A `[side × side, C]` table (row-major grid) resized to `[oh × ow, C]` like
/// `F.interpolate(…, mode="bicubic", align_corners=False)` on the `[1, C, side, side]` layout
/// (`Dinov2Embeddings.interpolate_pos_encoding`).
pub fn interpolate_table(table: &[f32], side: usize, c: usize, oh: usize, ow: usize) -> Result<Vec<f32>> {
    if side == 0 || table.len() != side.saturating_mul(side).saturating_mul(c) || oh > 4096 || ow > 4096 {
        return Err(Error::Message(format!("cannot interpolate a {side}² table to {oh} × {ow}")));
    }
    let (ty, tx) = (bicubic_taps(side, oh), bicubic_taps(side, ow));
    let mut out = vec![0f32; oh * ow * c];
    for (oy, (iy, wy)) in ty.iter().enumerate() {
        for (ox, (ix, wx)) in tx.iter().enumerate() {
            let Some(dst) = out.get_mut((oy * ow + ox) * c..(oy * ow + ox + 1) * c) else { continue };
            let mut acc = vec![0f64; c];
            for (yy, wyy) in iy.iter().zip(wy) {
                for (xx, wxx) in ix.iter().zip(wx) {
                    let wt = wyy * wxx;
                    let Some(src) = table.get((yy * side + xx) * c..(yy * side + xx + 1) * c) else { continue };
                    for (a, s) in acc.iter_mut().zip(src) {
                        *a += wt * f64::from(*s);
                    }
                }
            }
            for (d, a) in dst.iter_mut().zip(&acc) {
                *d = *a as f32;
            }
        }
    }
    Ok(out)
}

/// `ConvTranspose2d(kernel_size = stride = k)` on `[1, Cin, H, W]` as one matmul (the kernel
/// patches don't overlap), optionally followed by an exact GELU. `weight`: `[Cin, Cout, k, k]`,
/// `bias`: `[Cout]`.
pub fn conv_transpose_patches(x: &Tensor, weight: &Tensor, bias: &Tensor, then_gelu: bool) -> Result<Tensor> {
    let (b, cin, h, w) = x.dims4()?;
    let (wcin, cout, k, k2) = weight.dims4()?;
    if b != 1 || wcin != cin || k != k2 || k == 0 {
        return Err(Error::Message(format!("transposed convolution of {:?} with {:?}", x.dims(), weight.dims())));
    }
    let xt = x.reshape((cin, h * w))?.t()?.contiguous()?;
    let y = xt.matmul(&weight.reshape((cin, cout * k * k))?)?; // [HW, Cout·k·k]
    if !x.device().is_cpu() {
        let y = y.reshape((h, w, cout, k, k))?.permute((2, 0, 3, 1, 4))?.reshape((1, cout, h * k, w * k))?;
        let y = y.broadcast_add(&bias.reshape((1, cout, 1, 1))?)?;
        return if then_gelu { gelu(&y) } else { Ok(y) };
    }
    // CPU: scatter the patches into NCHW planes in parallel, adding the bias (and GELU)
    let yv = y.flatten_all()?.to_vec1::<f32>()?;
    let bv = bias.flatten_all()?.to_vec1::<f32>()?;
    let (oh, ow, kk) = (h * k, w * k, k * k);
    let mut out = vec![0f32; cout * oh * ow];
    out.par_chunks_mut(oh * ow).enumerate().for_each(|(c, plane)| {
        let bc = bv.get(c).copied().unwrap_or(0.0);
        for (oy, row) in plane.chunks_exact_mut(ow).enumerate() {
            let (i, di) = (oy / k, oy % k);
            for (ox, o) in row.iter_mut().enumerate() {
                let (j, dj) = (ox / k, ox % k);
                let v = yv.get((i * w + j) * cout * kk + c * kk + di * k + dj).copied().unwrap_or(0.0) + bc;
                *o = if then_gelu { gelu1(v) } else { v };
            }
        }
    });
    Ok(Tensor::from_vec(out, (1, cout, oh, ow), x.device())?)
}

/// Source taps of PyTorch's bilinear interpolation from `src` to `dst` samples.
fn bilinear_taps(src: usize, dst: usize, align_corners: bool) -> Vec<(usize, usize, f32)> {
    let last = src.saturating_sub(1);
    (0..dst)
        .map(|o| {
            let f = if align_corners {
                if dst > 1 { o as f32 * (last as f32 / (dst - 1) as f32) } else { 0.0 }
            } else {
                ((o as f32 + 0.5) * (src as f32 / dst.max(1) as f32) - 0.5).max(0.0)
            };
            let i0 = (f.floor() as usize).min(last);
            (i0, (i0 + 1).min(last), f - i0 as f32)
        })
        .collect()
}

/// `F.interpolate(x, size=(oh, ow), mode="bilinear", align_corners=…)` on `[1, C, H, W]`; in
/// parallel over channels on the CPU.
pub fn upsample_bilinear(x: &Tensor, oh: usize, ow: usize, align_corners: bool) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    if !x.device().is_cpu() || b != 1 || h == 0 || w == 0 {
        return Ok(x.upsample_bilinear2d(oh, ow, align_corners)?);
    }
    let (ty, tx) = (bilinear_taps(h, oh, align_corners), bilinear_taps(w, ow, align_corners));
    let xv = x.flatten_all()?.to_vec1::<f32>()?;
    let mut out = vec![0f32; c * oh * ow];
    out.par_chunks_mut(oh * ow).zip(xv.par_chunks(h * w)).for_each(|(o, src)| {
        for (row, &(y0, y1, fy)) in o.chunks_exact_mut(ow).zip(&ty) {
            let (r0, r1) = (src.get(y0 * w..(y0 + 1) * w).unwrap_or(&[]), src.get(y1 * w..(y1 + 1) * w).unwrap_or(&[]));
            for (v, &(x0, x1, fx)) in row.iter_mut().zip(&tx) {
                let at = |r: &[f32], i: usize| r.get(i).copied().unwrap_or(0.0);
                let top = at(r0, x0) * (1.0 - fx) + at(r0, x1) * fx;
                let bot = at(r1, x0) * (1.0 - fx) + at(r1, x1) * fx;
                *v = top * (1.0 - fy) + bot * fy;
            }
        }
    });
    Ok(Tensor::from_vec(out, (1, c, oh, ow), x.device())?)
}

/// A 1 × 1 convolution with bias as one matmul (`[Cout, Cin] · [Cin, H·W]`).
pub struct Pointwise {
    w: Tensor,
    b: Tensor,
}

impl Pointwise {
    pub fn new(cin: usize, cout: usize, vb: VarBuilder) -> Result<Self> {
        Ok(Self { w: vb.get((cout, cin, 1, 1), "weight")?.reshape((cout, cin))?, b: vb.get(cout, "bias")?.reshape((1, cout, 1, 1))? })
    }

    /// `x`: `[1, Cin, H, W]`.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (b, cin, h, w) = x.dims4()?;
        let cout = self.w.dim(0)?;
        let y = self.w.matmul(&x.reshape((b * cin, h * w))?)?.reshape((1, cout, h, w))?;
        Ok(y.broadcast_add(&self.b)?)
    }
}

/// A 3 × 3 convolution (stride 1, padding 1, optional bias) on `[1, Cin, H, W]`. On the CPU:
/// row bands of the output in parallel, each an im2col block times the `[Cout, Cin·9]` kernel.
pub struct Conv3x3 {
    /// `[Cout, Cin·9]`.
    w: Tensor,
    b: Option<Tensor>,
    conv: candle_nn::Conv2d,
}

/// Output pixels per band of [`Conv3x3`] (about; whole rows).
const BAND: usize = 2048;

impl Conv3x3 {
    pub fn new(cin: usize, cout: usize, bias: bool, vb: VarBuilder) -> Result<Self> {
        let cfg = candle_nn::Conv2dConfig { padding: 1, ..Default::default() };
        let conv = if bias { candle_nn::conv2d(cin, cout, 3, cfg, vb)? } else { candle_nn::conv2d_no_bias(cin, cout, 3, cfg, vb)? };
        let w = conv.weight().reshape((cout, cin * 9))?;
        let b = conv.bias().cloned();
        Ok(Self { w, b, conv })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (bn, cin, h, w) = x.dims4()?;
        let (cout, k) = self.w.dims2()?;
        if !x.device().is_cpu() || bn != 1 || k != cin * 9 || h == 0 || w == 0 {
            return Ok(self.conv.forward(x)?);
        }
        let xv = x.flatten_all()?.to_vec1::<f32>()?;
        let rows = (BAND / w).clamp(1, h);
        let bands: Vec<usize> = (0..h).step_by(rows).collect();
        let outs: Vec<Result<Vec<f32>>> = bands
            .par_iter()
            .map(|&r0| {
                let nr = rows.min(h - r0);
                let t = nr * w;
                // col[(ci·9 + dy·3 + dx), (r, x)] = x[ci, r0 + r + dy − 1, x + dx − 1] (0 outside)
                let mut col = vec![0f32; k * t];
                for (kk, dst) in col.chunks_exact_mut(t).enumerate() {
                    let (ci, dy, dx) = (kk / 9, kk % 9 / 3, kk % 3);
                    let Some(plane) = xv.get(ci * h * w..(ci + 1) * h * w) else { continue };
                    for (r, drow) in dst.chunks_exact_mut(w).enumerate() {
                        let Some(sy) = (r0 + r + dy).checked_sub(1).filter(|v| *v < h) else { continue };
                        let Some(src) = plane.get(sy * w..(sy + 1) * w) else { continue };
                        // x + dx − 1 for x in 0..w
                        match dx {
                            0 => {
                                if let (Some(d), Some(s)) = (drow.get_mut(1..), src.get(..w - 1)) {
                                    d.copy_from_slice(s);
                                }
                            }
                            1 => drow.copy_from_slice(src),
                            _ => {
                                if let (Some(d), Some(s)) = (drow.get_mut(..w - 1), src.get(1..)) {
                                    d.copy_from_slice(s);
                                }
                            }
                        }
                    }
                }
                let col = Tensor::from_vec(col, (k, t), &Device::Cpu)?;
                Ok(self.w.matmul(&col)?.flatten_all()?.to_vec1::<f32>()?)
            })
            .collect();
        let outs = outs.into_iter().collect::<Result<Vec<_>>>()?;
        let bias = match &self.b {
            Some(b) => b.to_vec1::<f32>()?,
            None => vec![0f32; cout],
        };
        let mut out = vec![0f32; cout * h * w];
        out.par_chunks_mut(h * w).enumerate().for_each(|(c, plane)| {
            let bc = bias.get(c).copied().unwrap_or(0.0);
            for (&r0, o) in bands.iter().zip(&outs) {
                let t = rows.min(h - r0) * w;
                if let (Some(dst), Some(src)) = (plane.get_mut(r0 * w..r0 * w + t), o.get(c * t..(c + 1) * t)) {
                    dst.iter_mut().zip(src).for_each(|(d, s)| *d = s + bc);
                }
            }
        });
        Ok(Tensor::from_vec(out, (1, cout, h, w), x.device())?)
    }
}

/// A depthwise 3 × 3 convolution with padding 1 and no bias on `[1, C, H, W]`
/// (`weight`: `[C, 1, 3, 3]`).
pub fn depthwise3x3(x: &Tensor, weight: &Tensor) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    if b != 1 || weight.dims() != [c, 1, 3, 3] {
        return Err(Error::Message(format!("depthwise convolution of {:?} with {:?}", x.dims(), weight.dims())));
    }
    if !x.device().is_cpu() {
        return Ok(x.conv2d(weight, 1, 1, 1, c)?);
    }
    let xv = x.flatten_all()?.to_vec1::<f32>()?;
    let wv = weight.flatten_all()?.to_vec1::<f32>()?;
    let mut out = vec![0f32; c * h * w];
    out.par_chunks_mut(h * w).zip(xv.par_chunks(h * w)).zip(wv.par_chunks(9)).for_each(|((o, src), k)| {
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0f32;
                for dy in 0..3 {
                    let Some(sy) = (y + dy).checked_sub(1).filter(|v| *v < h) else { continue };
                    for dx in 0..3 {
                        let Some(sx) = (x + dx).checked_sub(1).filter(|v| *v < w) else { continue };
                        acc += src.get(sy * w + sx).copied().unwrap_or(0.0) * k.get(dy * 3 + dx).copied().unwrap_or(0.0);
                    }
                }
                if let Some(v) = o.get_mut(y * w + x) {
                    *v = acc;
                }
            }
        }
    });
    Ok(Tensor::from_vec(out, (1, c, h, w), x.device())?)
}

/// Layer norm over the channels of `[1, C, H, W]` (`LayerNorm2d`), in parallel on the CPU.
pub fn layer_norm2d(x: &Tensor, weight: &Tensor, bias: &Tensor, eps: f64) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    if !x.device().is_cpu() || b != 1 || c == 0 {
        let norm = LayerNorm::new(weight.clone(), bias.clone(), eps);
        return crate::nn::ln_channels(x, &norm);
    }
    let n = h * w;
    let xv = x.flatten_all()?.to_vec1::<f32>()?;
    let wv = weight.flatten_all()?.to_vec1::<f32>()?;
    let bv = bias.flatten_all()?.to_vec1::<f32>()?;
    // per-pixel mean and 1/σ over pixel ranges (channel-outer loops keep the reads sequential)
    const PIX: usize = 1024;
    let mut stats = vec![(0f32, 0f32); n];
    stats.par_chunks_mut(PIX).enumerate().for_each(|(ci, st)| {
        let p0 = ci * PIX;
        let mut sum = vec![0f64; st.len()];
        for ch in 0..c {
            let Some(row) = xv.get(ch * n + p0..ch * n + p0 + st.len()) else { continue };
            sum.iter_mut().zip(row).for_each(|(s, v)| *s += f64::from(*v));
        }
        let mean: Vec<f64> = sum.iter().map(|s| s / c as f64).collect();
        let mut var = vec![0f64; st.len()];
        for ch in 0..c {
            let Some(row) = xv.get(ch * n + p0..ch * n + p0 + st.len()) else { continue };
            var.iter_mut().zip(row).zip(&mean).for_each(|((s, v), m)| *s += (f64::from(*v) - m).powi(2));
        }
        for ((s, m), v) in st.iter_mut().zip(&mean).zip(&var) {
            *s = (*m as f32, (1.0 / (v / c as f64 + eps).sqrt()) as f32);
        }
    });
    let mut out = vec![0f32; c * n];
    out.par_chunks_mut(n).zip(xv.par_chunks(n)).enumerate().for_each(|(ch, (o, src))| {
        let (g, bb) = (wv.get(ch).copied().unwrap_or(1.0), bv.get(ch).copied().unwrap_or(0.0));
        for ((o, v), (m, r)) in o.iter_mut().zip(src).zip(&stats) {
            *o = (v - m) * r * g + bb;
        }
    });
    Ok(Tensor::from_vec(out, (1, c, h, w), x.device())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_interpolation_is_exact() {
        let t: Vec<f32> = (0..3 * 3 * 2).map(|i| i as f32).collect();
        let o = interpolate_table(&t, 3, 2, 3, 3).unwrap();
        for (a, b) in o.iter().zip(&t) {
            assert!((a - b).abs() < 1e-5);
        }
        assert!(interpolate_table(&t, 4, 2, 3, 3).is_err());
    }

    #[test]
    fn cubic_weights_sum_to_one() {
        for t in [0.0, 0.25, 0.5, 0.9] {
            let s: f64 = cubic_weights(t).iter().sum();
            assert!((s - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn transposed_patches_match_candle() {
        let dev = Device::Cpu;
        let x = Tensor::rand(-1f32, 1f32, (1, 3, 4, 5), &dev).unwrap();
        let w = Tensor::rand(-1f32, 1f32, (3, 2, 2, 2), &dev).unwrap();
        let b = Tensor::new(&[0.5f32, -0.25], &dev).unwrap();
        let ours = conv_transpose_patches(&x, &w, &b, false).unwrap();
        let theirs = x.conv_transpose2d(&w, 0, 0, 2, 1).unwrap().broadcast_add(&b.reshape((1, 2, 1, 1)).unwrap()).unwrap();
        let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-5);
    }

    #[test]
    fn depthwise_matches_grouped_conv() {
        let dev = Device::Cpu;
        let x = Tensor::rand(-1f32, 1f32, (1, 4, 6, 5), &dev).unwrap();
        let w = Tensor::rand(-1f32, 1f32, (4, 1, 3, 3), &dev).unwrap();
        let ours = depthwise3x3(&x, &w).unwrap();
        let theirs = x.conv2d(&w, 1, 1, 1, 4).unwrap();
        let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-5);
    }

    #[test]
    fn layer_norm2d_matches_candle() {
        let dev = Device::Cpu;
        let x = Tensor::rand(-1f32, 3f32, (1, 5, 3, 4), &dev).unwrap();
        let g = Tensor::rand(0.5f32, 1.5f32, 5, &dev).unwrap();
        let b = Tensor::rand(-1f32, 1f32, 5, &dev).unwrap();
        let ours = layer_norm2d(&x, &g, &b, 1e-6).unwrap();
        let theirs = crate::nn::ln_channels(&x, &LayerNorm::new(g, b, 1e-6)).unwrap();
        let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-5);
    }

    #[test]
    fn attention_matches_the_definition() {
        let dev = Device::Cpu;
        let q = Tensor::rand(-2f32, 2f32, (1, 2, 300, 8), &dev).unwrap();
        let k = Tensor::rand(-2f32, 2f32, (1, 2, 300, 8), &dev).unwrap();
        let v = Tensor::rand(-1f32, 1f32, (1, 2, 300, 8), &dev).unwrap();
        let ours = attention(&q, &k, &v).unwrap();
        let att = candle_nn::ops::softmax_last_dim(&q.matmul(&k.t().unwrap()).unwrap()).unwrap();
        let theirs = att.matmul(&v).unwrap().transpose(1, 2).unwrap().reshape((1, 300, 16)).unwrap();
        let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-5, "{d}");
        for x in [0.0f32, -1e-3, -0.5, -1.0, -10.0, -80.0] {
            assert!((exp_nonpositive(x) - x.exp()).abs() <= 1e-6 * x.exp(), "{x}");
        }
        assert!(exp_nonpositive(-1000.0) < 1e-37);
    }

    #[test]
    fn bilinear_matches_candle() {
        let x = Tensor::rand(-1f32, 1f32, (1, 3, 5, 7), &Device::Cpu).unwrap();
        for (oh, ow) in [(10, 14), (9, 13), (5, 7)] {
            for ac in [false, true] {
                let ours = upsample_bilinear(&x, oh, ow, ac).unwrap();
                let theirs = x.upsample_bilinear2d(oh, ow, ac).unwrap();
                let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
                assert!(d < 1e-5, "{oh}x{ow} {ac}: {d}");
            }
        }
    }

    #[test]
    fn conv3x3_matches_candle() {
        let dev = Device::Cpu;
        let mut map = std::collections::HashMap::new();
        map.insert("weight".to_string(), Tensor::rand(-1f32, 1f32, (4, 3, 3, 3), &dev).unwrap());
        map.insert("bias".to_string(), Tensor::rand(-1f32, 1f32, 4, &dev).unwrap());
        let vb = VarBuilder::from_tensors(map, DType::F32, &dev);
        let conv = Conv3x3::new(3, 4, true, vb).unwrap();
        for (h, w) in [(1, 1), (5, 7), (700, 9)] {
            let x = Tensor::rand(-1f32, 1f32, (1, 3, h, w), &dev).unwrap();
            let d =
                (conv.forward(&x).unwrap() - conv.conv.forward(&x).unwrap()).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
            assert!(d < 1e-4, "{h}x{w}: {d}");
        }
    }

    #[test]
    fn gelu_matches_candle() {
        let x = Tensor::rand(-4f32, 4f32, 1000, &Device::Cpu).unwrap();
        let d = (gelu(&x).unwrap() - x.gelu_erf().unwrap()).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-6);
    }
}
