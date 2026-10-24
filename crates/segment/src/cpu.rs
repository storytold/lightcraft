//! Multi-threaded CPU kernels for the hot paths of SAM 2 (encoder and mask decoder).
//!
//! candle runs element-wise operations (bias, GELU, residual adds) on one thread, and its
//! batched matmul is slow for many small matrices (the Hiera windows). These kernels compute
//! the same arithmetic over rayon's threads: linear-layer epilogues, softmax attention with
//! packed 4 × 8 micro-kernels, Hiera's windowed attention with query pooling (read straight
//! from the qkv projection, padding tokens included), and the mask decoder's upscaling
//! epilogue. On other devices the plain candle operations run.
//!
//! Modified work (Apache License 2.0, §4(b)): the operations reproduce the Python/PyTorch SAM 2
//! code of Hugging Face Transformers (`models/sam2/modeling_sam2.py`: `Sam2MultiScaleAttention`,
//! `window_partition`, `Sam2Attention`, `Sam2MaskDecoder`), Copyright The HuggingFace Team and
//! Meta Platforms, Inc.; reimplemented by the LightCraft contributors in 2026 as fused CPU
//! kernels. See NOTICE.

use candle_core::{CpuStorage, CustomOp1, CustomOp2, CustomOp3, Layout, Shape, Tensor};
use candle_nn::Linear;
use rayon::prelude::*;

use crate::Result;

type CResult<T> = candle_core::Result<T>;

fn f32s<'a>(s: &'a CpuStorage, l: &Layout, what: &str) -> CResult<&'a [f32]> {
    let CpuStorage::F32(v) = s else { candle_core::bail!("{what}: f32 expected") };
    let Some((a, b)) = l.contiguous_offsets() else { candle_core::bail!("{what}: contiguous input expected") };
    v.get(a..b).ok_or_else(|| candle_core::Error::Msg(format!("{what}: storage too small")))
}

#[inline]
fn gelu(v: f32) -> f32 {
    (candle_core::cpu::erf::erf_f32(v * std::f32::consts::FRAC_1_SQRT_2) + 1.0) * 0.5 * v
}

/// Rows per parallel task for row-wise kernels.
const ROWS: usize = 64;

/// `y + bias`, then GELU (erf) when `gelu`.
struct BiasAct {
    gelu: bool,
}

impl CustomOp2 for BiasAct {
    fn name(&self) -> &'static str {
        "lc-bias-act"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (y, b) = (f32s(s1, l1, "bias-act")?, f32s(s2, l2, "bias-act bias")?);
        let c = b.len();
        if c == 0 || y.len() % c != 0 {
            candle_core::bail!("bias-act: {} values for {c} channels", y.len())
        }
        let mut out = vec![0f32; y.len()];
        out.par_chunks_mut(c * ROWS).zip(y.par_chunks(c * ROWS)).for_each(|(o, y)| {
            for (orow, yrow) in o.chunks_exact_mut(c).zip(y.chunks_exact(c)) {
                for ((o, y), b) in orow.iter_mut().zip(yrow).zip(b) {
                    let v = y + b;
                    *o = if self.gelu { gelu(v) } else { v };
                }
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

/// `y + bias + residual`.
struct BiasResidual;

impl CustomOp3 for BiasResidual {
    fn name(&self) -> &'static str {
        "lc-bias-residual"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout, s3: &CpuStorage, l3: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (y, b, r) = (f32s(s1, l1, "bias-residual")?, f32s(s2, l2, "bias-residual bias")?, f32s(s3, l3, "bias-residual residual")?);
        let c = b.len();
        if c == 0 || y.len() % c != 0 || r.len() != y.len() {
            candle_core::bail!("bias-residual: {} values, {} residual, {c} channels", y.len(), r.len())
        }
        let mut out = vec![0f32; y.len()];
        out.par_chunks_mut(c * ROWS).zip(y.par_chunks(c * ROWS)).zip(r.par_chunks(c * ROWS)).for_each(|((o, y), r)| {
            for ((orow, yrow), rrow) in o.chunks_exact_mut(c).zip(y.chunks_exact(c)).zip(r.chunks_exact(c)) {
                for (((o, y), b), r) in orow.iter_mut().zip(yrow).zip(b).zip(rrow) {
                    *o = r + (y + b);
                }
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

/// What follows a linear layer.
#[derive(Clone, Copy, PartialEq)]
pub enum Act {
    None,
    Gelu,
}

/// `x · Wᵀ + b` over the last axis (then `act`, then `+ residual`), as one 2-D matmul and a
/// multi-threaded epilogue on the CPU.
pub fn linear(x: &Tensor, l: &Linear, act: Act, residual: Option<&Tensor>) -> Result<Tensor> {
    let w = l.weight();
    let (out_c, in_c) = w.dims2()?;
    let dims = x.dims().to_vec();
    let rows = x.elem_count() / in_c.max(1);
    let y = x.reshape((rows, in_c))?.matmul(&w.t()?)?;
    let mut shape = dims;
    if let Some(last) = shape.last_mut() {
        *last = out_c;
    }
    let Some(b) = l.bias() else {
        let y = if act == Act::Gelu { y.gelu_erf()? } else { y };
        let y = y.reshape(shape)?;
        return Ok(match residual {
            Some(r) => (r + y)?,
            None => y,
        });
    };
    if !x.device().is_cpu() {
        let y = y.broadcast_add(b)?;
        let y = if act == Act::Gelu { y.gelu_erf()? } else { y };
        let y = y.reshape(shape)?;
        return Ok(match residual {
            Some(r) => (r + y)?,
            None => y,
        });
    }
    let y = match residual {
        Some(r) if act == Act::None => y.apply_op3_no_bwd(b, &r.contiguous()?.reshape((rows, out_c))?, &BiasResidual)?,
        Some(r) => (r.reshape((rows, out_c))? + y.apply_op2_no_bwd(b, &BiasAct { gelu: true })?)?,
        None => y.apply_op2_no_bwd(b, &BiasAct { gelu: act == Act::Gelu })?,
    };
    Ok(y.reshape(shape)?)
}

/// `a + b` (same shapes), then GELU when `gelu`.
struct AddAct {
    gelu: bool,
}

impl CustomOp2 for AddAct {
    fn name(&self) -> &'static str {
        "lc-add-act"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (a, b) = (f32s(s1, l1, "add-act")?, f32s(s2, l2, "add-act")?);
        if a.len() != b.len() {
            candle_core::bail!("add-act: {:?} + {:?}", l1.shape(), l2.shape())
        }
        let mut out = vec![0f32; a.len()];
        out.par_chunks_mut(1 << 14).zip(a.par_chunks(1 << 14)).zip(b.par_chunks(1 << 14)).for_each(|((o, a), b)| {
            for ((o, a), b) in o.iter_mut().zip(a).zip(b) {
                let v = a + b;
                *o = if self.gelu { gelu(v) } else { v };
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

/// `a + b` (same shapes; `b` may broadcast on non-CPU devices), then GELU when `gelu`.
pub fn add_act(a: &Tensor, b: &Tensor, gelu: bool) -> Result<Tensor> {
    if !a.device().is_cpu() || a.dims() != b.dims() {
        let s = a.broadcast_add(b)?;
        return Ok(if gelu { s.gelu_erf()? } else { s });
    }
    Ok(a.contiguous()?.apply_op2_no_bwd(&b.contiguous()?, &AddAct { gelu })?)
}

/// 2 × 2 max pooling (stride 2, odd edges dropped) of an NHWC tensor.
struct Pool2;

impl CustomOp1 for Pool2 {
    fn name(&self) -> &'static str {
        "lc-pool2-nhwc"
    }

    fn cpu_fwd(&self, s: &CpuStorage, l: &Layout) -> CResult<(CpuStorage, Shape)> {
        let x = f32s(s, l, "pool2")?;
        let (b, h, w, c) = l.shape().dims4()?;
        let (oh, ow) = (h / 2, w / 2);
        let mut out = vec![0f32; b * oh * ow * c];
        if out.is_empty() {
            return Ok((CpuStorage::F32(out), Shape::from((b, oh, ow, c))));
        }
        // one task per output row
        out.par_chunks_mut(ow * c).enumerate().for_each(|(row, o)| {
            let (bi, oy) = (row / oh, row % oh);
            for (ox, oc) in o.chunks_exact_mut(c).enumerate() {
                let at = |dy: usize, dx: usize| ((bi * h + 2 * oy + dy) * w + 2 * ox + dx) * c;
                let taps = [at(0, 0), at(0, 1), at(1, 0), at(1, 1)];
                for (ch, v) in oc.iter_mut().enumerate() {
                    *v = taps.iter().fold(f32::NEG_INFINITY, |m, t| m.max(x.get(t + ch).copied().unwrap_or(f32::NEG_INFINITY)));
                }
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((b, oh, ow, c))))
    }
}

/// 2 × 2 max pooling of an NHWC tensor.
pub fn pool2(x: &Tensor) -> Result<Tensor> {
    if !x.device().is_cpu() {
        return Ok(x.permute((0, 3, 1, 2))?.max_pool2d(2)?.permute((0, 2, 3, 1))?.contiguous()?);
    }
    Ok(x.contiguous()?.apply_op1_no_bwd(&Pool2)?)
}

/// `e^x` for `x ≤ 0` (softmax), branch-free so that it vectorizes: `2^n · p(r)` with a
/// degree-6 polynomial (relative error ≈ 2e-7).
#[inline]
fn exp_neg(x: f32) -> f32 {
    const LOG2E: f32 = std::f32::consts::LOG2_E;
    const LN2_HI: f32 = 0.693_145_75;
    const LN2_LO: f32 = 1.428_606_8e-6;
    const MAGIC: f32 = 12_582_912.0; // 1.5 · 2²³: adding it rounds to an integer
    let x = x.clamp(-87.0, 0.0);
    let t = x * LOG2E + MAGIC;
    let n = t - MAGIC;
    let r = x - n * LN2_HI - n * LN2_LO;
    let p = 1.0 + r * (1.0 + r * (0.5 + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    let e = (t.to_bits() as i32).wrapping_sub(0x4B40_0000).wrapping_add(127);
    p * f32::from_bits((e.clamp(1, 254) as u32) << 23)
}

/// Rounds `n` up to a multiple of 8.
fn up8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// One attention head's keys and values, packed for the micro-kernels: keys as panels of 8
/// keys `[lk8 / 8][dp][8]`, values as panels of 8 channels `[dp / 8][lk8][8]` (zero padded).
struct Packed {
    k: Vec<f32>,
    v: Vec<f32>,
    lk: usize,
    lk8: usize,
    dp: usize,
}

impl Packed {
    /// `kv(j)`: the key and value rows (at least `d` values each) of key `j < lk`.
    fn new<'a>(lk: usize, d: usize, kv: impl Fn(usize) -> (&'a [f32], &'a [f32])) -> Self {
        let (lk8, dp) = (up8(lk), up8(d));
        let mut k = vec![0f32; lk8 * dp];
        let mut v = vec![0f32; dp * lk8];
        for j in 0..lk {
            let (kr, vr) = kv(j);
            let kbase = (j / 8) * dp * 8 + j % 8;
            for (c, x) in kr.iter().take(d).enumerate() {
                if let Some(o) = k.get_mut(kbase + c * 8) {
                    *o = *x;
                }
            }
            for (c, x) in vr.iter().take(d).enumerate() {
                if let Some(o) = v.get_mut((c / 8) * lk8 * 8 + j * 8 + c % 8) {
                    *o = *x;
                }
            }
        }
        Self { k, v, lk, lk8, dp }
    }

    /// Attention of the queries `q` (`n × dp`, already scaled, `n` a multiple of 4) into `out`
    /// (`n × dp`).
    fn attend(&self, q: &[f32], out: &mut [f32]) {
        let (lk8, dp) = (self.lk8, self.dp);
        let mut s = vec![0f32; 4 * lk8];
        let mut st = vec![0f32; 4 * lk8];
        let mut qt = vec![0f32; 4 * dp];
        for (q4, o4) in q.chunks_exact(4 * dp).zip(out.chunks_exact_mut(4 * dp)) {
            transpose4(q4, &mut qt);
            // scores: 4 queries × 8 keys per step
            for (p, panel) in self.k.chunks_exact(dp * 8).enumerate() {
                let acc = micro(panel, &qt);
                for (i, acc) in acc.iter().enumerate() {
                    if let Some(dst) = s.get_mut(i * lk8 + p * 8..i * lk8 + p * 8 + 8) {
                        dst.copy_from_slice(acc);
                    }
                }
            }
            for row in s.chunks_exact_mut(lk8) {
                let (real, pad) = row.split_at_mut(self.lk.min(lk8));
                let m = real.iter().fold(f32::NEG_INFINITY, |m, x| m.max(*x));
                let mut sum = 0f32;
                for x in real.iter_mut() {
                    *x = exp_neg(*x - m);
                    sum += *x;
                }
                let inv = if sum > 0.0 { 1.0 / sum } else { 0.0 };
                real.iter_mut().for_each(|x| *x *= inv);
                pad.iter_mut().for_each(|x| *x = 0.0);
            }
            // weighted values: 4 queries × 8 channels per step
            transpose4(&s, &mut st);
            for (p, panel) in self.v.chunks_exact(lk8 * 8).enumerate() {
                let acc = micro(panel, &st);
                for (i, acc) in acc.iter().enumerate() {
                    if let Some(dst) = o4.get_mut(i * dp + p * 8..i * dp + p * 8 + 8) {
                        dst.copy_from_slice(acc);
                    }
                }
            }
        }
    }
}

/// `[4][n]` rows → `[n][4]`.
fn transpose4(x: &[f32], t: &mut [f32]) {
    let n = x.len() / 4;
    let (r0, rest) = x.split_at(n);
    let (r1, rest) = rest.split_at(n);
    let (r2, r3) = rest.split_at(n);
    for ((((t, a), b), c), d) in t.as_chunks_mut::<4>().0.iter_mut().zip(r0).zip(r1).zip(r2).zip(r3) {
        *t = [*a, *b, *c, *d];
    }
}

/// The 4 × 8 micro-kernel: `Σ_c a[c][i] · b[c][l]` for `b` `[n][8]` and `a` `[n][4]`.
#[inline(always)]
fn micro(b: &[f32], a: &[f32]) -> [[f32; 8]; 4] {
    let mut acc = [[0f32; 8]; 4];
    for (b8, a4) in b.as_chunks::<8>().0.iter().zip(a.as_chunks::<4>().0) {
        for i in 0..4 {
            for l in 0..8 {
                acc[i][l] += a4[i] * b8[l];
            }
        }
    }
    acc
}

/// Queries per parallel attention task (a multiple of 4).
const TILE: usize = 32;

/// Attention of the scaled, padded queries `q` (`lq × dp` rows, padded to a multiple of 4 by
/// the caller) over `p`, split into parallel tiles; returns `lq × dp`.
fn attend_parallel(p: &Packed, q: &[f32], out: &mut [f32]) {
    let dp = p.dp;
    out.par_chunks_mut(TILE * dp).zip(q.par_chunks(TILE * dp)).for_each(|(o, q)| p.attend(q, o));
}

/// Gathers `n` query rows (`d` values each, scaled) into a zero-padded `[up4(n), dp]` buffer.
fn pack_queries(n: usize, d: usize, dp: usize, scale: f32, row: impl Fn(usize, &mut [f32])) -> Vec<f32> {
    let n4 = n.div_ceil(4) * 4;
    let mut q = vec![0f32; n4 * dp];
    for (i, dst) in q.chunks_exact_mut(dp).take(n).enumerate() {
        if let Some(d) = dst.get_mut(..d) {
            row(i, d);
            d.iter_mut().for_each(|x| *x *= scale);
        }
    }
    q
}

/// `x[at..at + n]`, or empty when out of range.
fn slice_at(x: &[f32], at: usize, n: usize) -> &[f32] {
    x.get(at..at.saturating_add(n)).unwrap_or(&[])
}

/// Multi-head softmax attention on channel-last tensors: `q` `[B, Lq, C]` over `k`, `v`
/// `[B, Lk, C]`, heads interleaved in `C` (contiguous) → `[B, Lq, C]`.
struct Mha {
    heads: usize,
    scale: f32,
}

impl CustomOp3 for Mha {
    fn name(&self) -> &'static str {
        "lc-mha"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout, s3: &CpuStorage, l3: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (q, k, v) = (f32s(s1, l1, "attention q")?, f32s(s2, l2, "attention k")?, f32s(s3, l3, "attention v")?);
        let (b, lq, c) = l1.shape().dims3()?;
        let (bk, lk, ck) = l2.shape().dims3()?;
        let heads = self.heads.max(1);
        if bk != b || ck != c || l3.shape().dims() != l2.shape().dims() || c % heads != 0 || c == 0 || lk == 0 || lq == 0 {
            candle_core::bail!("attention: q {:?}, k {:?}, v {:?}, {} heads", l1.shape(), l2.shape(), l3.shape(), self.heads)
        }
        let hd = c / heads;
        let dp = up8(hd);
        let lq4 = lq.div_ceil(4) * 4;
        let mut per_head = vec![0f32; b * heads * lq4 * dp];
        per_head.par_chunks_mut(lq4 * dp).enumerate().for_each(|(t, o)| {
            let (bi, head) = (t / heads, t % heads);
            let at = |n: usize, i: usize| (bi * n + i) * c + head * hd;
            let p = Packed::new(lk, hd, |j| (slice_at(k, at(lk, j), hd), slice_at(v, at(lk, j), hd)));
            let qp = pack_queries(lq, hd, dp, self.scale, |i, dst| {
                let src = slice_at(q, at(lq, i), hd);
                if src.len() == dst.len() {
                    dst.copy_from_slice(src);
                }
            });
            attend_parallel(&p, &qp, o);
        });
        let mut out = vec![0f32; q.len()];
        out.par_chunks_mut(c).enumerate().for_each(|(r, o)| {
            let (bi, i) = (r / lq, r % lq);
            for (head, dst) in o.chunks_exact_mut(hd).enumerate() {
                let at = ((bi * heads + head) * lq4 + i) * dp;
                if let Some(src) = per_head.get(at..at + hd) {
                    dst.copy_from_slice(src);
                }
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

/// Multi-head scaled dot-product attention (no mask) on `[B, L, C]` tensors with `heads`
/// interleaved in `C`.
pub fn mha(q: &Tensor, k: &Tensor, v: &Tensor, heads: usize, scale: f64) -> Result<Tensor> {
    if !q.device().is_cpu() {
        let split = |x: &Tensor| -> Result<Tensor> {
            let (b, l, c) = x.dims3()?;
            Ok(x.reshape((b, l, heads, c / heads.max(1)))?.transpose(1, 2)?.contiguous()?)
        };
        let (b, lq, c) = q.dims3()?;
        let o = crate::nn::sdpa(&split(q)?, &split(k)?, &split(v)?, None, scale)?;
        return Ok(o.transpose(1, 2)?.reshape((b, lq, c))?);
    }
    Ok(q.contiguous()?.apply_op3_no_bwd(&k.contiguous()?, &v.contiguous()?, &Mha { heads, scale: scale as f32 })?)
}

/// Hiera's (windowed) multi-head attention straight from the qkv projection, with optional
/// 2 × 2 query max-pooling (`Sam2MultiScaleAttention` around `window_partition` /
/// `window_unpartition`).
struct WindowAttention {
    heads: usize,
    /// Window side; 0 = global attention.
    window: usize,
    pool: bool,
    scale: f32,
}

impl CustomOp2 for WindowAttention {
    fn name(&self) -> &'static str {
        "lc-window-attention"
    }

    /// `qkv` `[B, H, W, 3·dim]` (q, k, v; heads within each), `bias` `[3·dim]` (the projection
    /// of a zero token: the padding outside the map) → `[B, H', W', dim]`.
    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (qkv, bias) = (f32s(s1, l1, "window attention")?, f32s(s2, l2, "window attention bias")?);
        let (b, h, w, c3) = l1.shape().dims4()?;
        let dim = c3 / 3;
        let heads = self.heads.max(1);
        let hd = dim / heads;
        if dim == 0 || c3 % 3 != 0 || dim % heads != 0 || bias.len() != c3 || h == 0 || w == 0 {
            candle_core::bail!("window attention: qkv {:?}, bias {}, {} heads", l1.shape(), bias.len(), self.heads)
        }
        // window sides at full resolution, and of the (pooled) queries
        let (wh, ww) = if self.window == 0 { (h, w) } else { (self.window, self.window) };
        let (qh, qw) = if self.pool { (wh / 2, ww / 2) } else { (wh, ww) };
        let (oh, ow) = if self.pool { (h / 2, w / 2) } else { (h, w) };
        if qh == 0 || qw == 0 || oh == 0 || ow == 0 {
            candle_core::bail!("window attention: window {} on a {h} × {w} map", self.window)
        }
        let (nwh, nww) = (h.div_ceil(wh), w.div_ceil(ww));
        let (lk, lq) = (wh * ww, qh * qw);
        let dp = up8(hd);
        let lq4 = lq.div_ceil(4) * 4;
        // the token at (y, x) of image `bi`, or the padding token
        let token = |bi: usize, y: usize, x: usize| -> &[f32] {
            if y < h && x < w { qkv.get(((bi * h + y) * w + x) * c3..((bi * h + y) * w + x + 1) * c3).unwrap_or(bias) } else { bias }
        };
        // per (image, window, head): attention outputs `[lq4, dp]`
        let tasks = b * nwh * nww * heads;
        let mut win_out = vec![0f32; tasks * lq4 * dp];
        win_out.par_chunks_mut(lq4 * dp).enumerate().for_each(|(t, o)| {
            let head = t % heads;
            let wi = t / heads;
            let (bi, wy, wx) = (wi / (nwh * nww), (wi / nww) % nwh, wi % nww);
            let key = |j: usize| {
                let tok = token(bi, wy * wh + j / ww, wx * ww + j % ww);
                let k = tok.get(dim + head * hd..dim + (head + 1) * hd).unwrap_or(&[]);
                let v = tok.get(2 * dim + head * hd..2 * dim + (head + 1) * hd).unwrap_or(&[]);
                (k, v)
            };
            let p = Packed::new(lk, hd, key);
            let qp = pack_queries(lq, hd, dp, self.scale, |i, dst| {
                let (y, x) = (wy * qh + i / qw, wx * qw + i % qw);
                let part = |y: usize, x: usize| token(bi, y, x).get(head * hd..(head + 1) * hd).unwrap_or(&[]);
                if self.pool {
                    dst.iter_mut().for_each(|v| *v = f32::NEG_INFINITY);
                    for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        for (d, s) in dst.iter_mut().zip(part(2 * y + dy, 2 * x + dx)) {
                            *d = d.max(*s);
                        }
                    }
                } else {
                    for (d, s) in dst.iter_mut().zip(part(y, x)) {
                        *d = *s;
                    }
                }
            });
            attend_parallel(&p, &qp, o);
        });
        // un-window into `[B, H', W', dim]` (heads concatenated), cropping the padding
        let mut out = vec![0f32; b * oh * ow * dim];
        out.par_chunks_mut(ow * dim).enumerate().for_each(|(row, o)| {
            let (bi, y) = (row / oh, row % oh);
            for (x, px) in o.chunks_exact_mut(dim).enumerate() {
                let wi = (bi * nwh + y / qh) * nww + x / qw;
                let i = (y % qh) * qw + x % qw;
                for (head, dst) in px.chunks_exact_mut(hd).enumerate() {
                    let at = ((wi * heads + head) * lq4 + i) * dp;
                    if let Some(src) = win_out.get(at..at + hd) {
                        dst.copy_from_slice(src);
                    }
                }
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((b, oh, ow, dim))))
    }
}

/// Hiera's attention from the qkv projection `[B, H, W, 3·dim]` (`bias`: the projection's
/// bias, the value of padding tokens): windows of `window` (0: global), optional query
/// pooling → `[B, H', W', dim]`. CPU only (see [`window_attention_supported`]).
pub fn window_attention(qkv: &Tensor, bias: &Tensor, heads: usize, window: usize, pool: bool, scale: f64) -> Result<Tensor> {
    Ok(qkv.contiguous()?.apply_op2_no_bwd(bias, &WindowAttention { heads, window, pool, scale: scale as f32 })?)
}
/// The epilogue of a kernel-2, stride-2 transposed convolution computed as a per-pixel matmul
/// `[h·w, 4·c]` (blocks `(dy, dx, c)`, no bias): adds the bias, places each 2 × 2 block, adds
/// the skip features `[1, 2h, 2w, c]`, then applies the optional layer norm over `c` and GELU.
pub struct UpEpilogue {
    /// `[4·c]`.
    bias: Vec<f32>,
    /// Layer-norm weight, bias and eps.
    ln: Option<(Vec<f32>, Vec<f32>, f32)>,
}

impl UpEpilogue {
    pub fn new(bias: Vec<f32>, ln: Option<(Vec<f32>, Vec<f32>, f32)>) -> Self {
        Self { bias, ln }
    }
}

impl CustomOp2 for UpEpilogue {
    fn name(&self) -> &'static str {
        "lc-up-epilogue"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> CResult<(CpuStorage, Shape)> {
        let (y, res) = (f32s(s1, l1, "upscale")?, f32s(s2, l2, "upscale skip")?);
        let (n, c4) = l1.shape().dims2()?;
        let (one, h2, w2, c) = l2.shape().dims4()?;
        let ln_ok = self.ln.as_ref().is_none_or(|(g, b, _)| g.len() == c && b.len() == c);
        if one != 1 || c == 0 || c4 != 4 * c || n != (h2 / 2) * (w2 / 2) || h2 % 2 != 0 || w2 % 2 != 0 || self.bias.len() != c4 || !ln_ok {
            candle_core::bail!("upscale: {:?} into {:?}", l1.shape(), l2.shape())
        }
        let w = w2 / 2;
        let mut out = vec![0f32; res.len()];
        out.par_chunks_mut(w2 * c).zip(res.par_chunks(w2 * c)).enumerate().for_each(|(oy, (o, r))| {
            let (iy, dy) = (oy / 2, oy % 2);
            for (ox, (op, rp)) in o.chunks_exact_mut(c).zip(r.chunks_exact(c)).enumerate() {
                let (ix, dx) = (ox / 2, ox % 2);
                let at = (iy * w + ix) * c4 + (dy * 2 + dx) * c;
                let (Some(src), Some(b)) = (y.get(at..at + c), self.bias.get((dy * 2 + dx) * c..(dy * 2 + dx + 1) * c)) else { continue };
                for (((o, s), b), r) in op.iter_mut().zip(src).zip(b).zip(rp) {
                    *o = s + b + r;
                }
                if let Some((g, beta, eps)) = &self.ln {
                    let mean = op.iter().sum::<f32>() / c as f32;
                    let var = op.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / c as f32;
                    let inv = 1.0 / (var + eps).sqrt();
                    for ((o, g), beta) in op.iter_mut().zip(g).zip(beta) {
                        *o = (*o - mean) * inv * g + beta;
                    }
                }
                op.iter_mut().for_each(|v| *v = gelu(*v));
            }
        });
        Ok((CpuStorage::F32(out), l2.shape().clone()))
    }
}

/// Applies [`UpEpilogue`] to `y` `[h·w, 4·c]` with the skip features `res` `[1, 2h, 2w, c]`.
pub fn up_epilogue(y: &Tensor, res: &Tensor, ep: &UpEpilogue) -> Result<Tensor> {
    if !y.device().is_cpu() {
        let (_, h2, w2, c) = res.dims4()?;
        let bias = Tensor::from_slice(&ep.bias, ep.bias.len(), y.device())?;
        let blocks = y.broadcast_add(&bias)?.reshape((h2 / 2, w2 / 2, 2, 2 * c))?.transpose(1, 2)?.contiguous()?;
        let x = (blocks.reshape((1, h2, w2, c))? + res)?;
        let x = match &ep.ln {
            Some((g, b, eps)) => {
                let (g, b) = (Tensor::from_slice(g, c, y.device())?, Tensor::from_slice(b, c, y.device())?);
                candle_core::Module::forward(&candle_nn::LayerNorm::new(g, b, f64::from(*eps)), &x)?
            }
            None => x,
        };
        return Ok(x.gelu_erf()?);
    }
    Ok(y.contiguous()?.apply_op2_no_bwd(&res.contiguous()?, ep)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, Module};

    fn close(a: &Tensor, b: &Tensor) -> f32 {
        let (a, b) = (a.flatten_all().unwrap().to_vec1::<f32>().unwrap(), b.flatten_all().unwrap().to_vec1::<f32>().unwrap());
        a.iter().zip(&b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
    }

    #[test]
    fn kernels_match_candle() {
        let d = Device::Cpu;
        let x = Tensor::randn(0f32, 1., (3, 70, 24), &d).unwrap();
        let l = Linear::new(Tensor::randn(0f32, 1., (40, 24), &d).unwrap(), Some(Tensor::randn(0f32, 1., 40, &d).unwrap()));
        let r = Tensor::randn(0f32, 1., (3, 70, 40), &d).unwrap();
        let want = l.forward(&x).unwrap();
        assert!(close(&linear(&x, &l, Act::None, None).unwrap(), &want) < 1e-4);
        assert!(close(&linear(&x, &l, Act::Gelu, None).unwrap(), &want.gelu_erf().unwrap()) < 1e-4);
        assert!(close(&linear(&x, &l, Act::None, Some(&r)).unwrap(), &(&want + &r).unwrap()) < 1e-4);
        assert!(close(&linear(&x, &l, Act::Gelu, Some(&r)).unwrap(), &(want.gelu_erf().unwrap() + &r).unwrap()) < 1e-4);

        let q = Tensor::randn(0f32, 1., (2, 3, 37, 20), &d).unwrap();
        let k = Tensor::randn(0f32, 1., (2, 3, 50, 20), &d).unwrap();
        let v = Tensor::randn(0f32, 1., (2, 3, 50, 20), &d).unwrap();
        let want = crate::nn::sdpa(&q, &k, &v, None, 0.3).unwrap().transpose(1, 2).unwrap().reshape((2, 37, 60)).unwrap();
        let merge = |x: &Tensor| x.transpose(1, 2).unwrap().reshape((2, x.dim(2).unwrap(), 60)).unwrap();
        assert!(close(&mha(&merge(&q), &merge(&k), &merge(&v), 3, 0.3).unwrap(), &want) < 1e-4);

        let a = Tensor::randn(0f32, 1., (2, 5, 7, 3), &d).unwrap();
        let b = Tensor::randn(0f32, 1., (2, 5, 7, 3), &d).unwrap();
        assert!(close(&add_act(&a, &b, false).unwrap(), &(&a + &b).unwrap()) < 1e-6);
        assert!(close(&add_act(&a, &b, true).unwrap(), &(&a + &b).unwrap().gelu_erf().unwrap()) < 1e-6);
        let want = a.permute((0, 3, 1, 2)).unwrap().max_pool2d(2).unwrap().permute((0, 2, 3, 1)).unwrap();
        assert_eq!(pool2(&a).unwrap().dims(), &[2, 2, 3, 3]);
        assert!(close(&pool2(&a).unwrap(), &want) < 1e-6);
    }

    #[test]
    fn upscale_epilogue_matches_transposed_convolution() {
        let d = Device::Cpu;
        let (h, w, cin, c) = (3, 5, 6, 4);
        let x = Tensor::randn(0f32, 1., (1, cin, h, w), &d).unwrap();
        let conv = candle_nn::ConvTranspose2d::new(
            Tensor::randn(0f32, 1., (cin, c, 2, 2), &d).unwrap(),
            Some(Tensor::randn(0f32, 1., c, &d).unwrap()),
            candle_nn::ConvTranspose2dConfig { stride: 2, ..Default::default() },
        );
        let skip = Tensor::randn(0f32, 1., (1, c, 2 * h, 2 * w), &d).unwrap();
        let (g, beta) = (Tensor::randn(0f32, 1., c, &d).unwrap(), Tensor::randn(0f32, 1., c, &d).unwrap());
        let norm = candle_nn::LayerNorm::new(g.clone(), beta.clone(), 1e-6);
        let sum = (conv.forward(&x).unwrap() + &skip).unwrap().permute((0, 2, 3, 1)).unwrap().contiguous().unwrap();
        // the convolution as a per-pixel linear map to (dy, dx, c) blocks
        let wl = conv.weight().permute((2, 3, 1, 0)).unwrap().contiguous().unwrap().reshape((4 * c, cin)).unwrap();
        let b = conv.bias().unwrap().to_vec1::<f32>().unwrap().repeat(4);
        let y = x.permute((0, 2, 3, 1)).unwrap().reshape((h * w, cin)).unwrap().matmul(&wl.t().unwrap()).unwrap();
        let skip = skip.permute((0, 2, 3, 1)).unwrap().contiguous().unwrap();
        let plain = UpEpilogue::new(b.clone(), None);
        assert!(close(&up_epilogue(&y, &skip, &plain).unwrap(), &sum.gelu_erf().unwrap()) < 1e-4);
        let normed = UpEpilogue::new(b, Some((g.to_vec1().unwrap(), beta.to_vec1().unwrap(), 1e-6)));
        let want = norm.forward(&sum).unwrap().gelu_erf().unwrap();
        assert!(close(&up_epilogue(&y, &skip, &normed).unwrap(), &want) < 1e-4);
        // a mismatched skip map is an error, not a panic
        assert!(up_epilogue(&y, &skip.narrow(1, 0, 4).unwrap(), &plain).is_err());
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use candle_core::Device;

    #[test]
    #[ignore]
    fn bench_global_attention() {
        let d = Device::Cpu;
        let qkv = Tensor::randn(0f32, 1., (1, 64, 64, 1152), &d).unwrap();
        let b = Tensor::randn(0f32, 1., 1152, &d).unwrap();
        let mut best = f64::MAX;
        for _ in 0..8 {
            let t = std::time::Instant::now();
            let _ = window_attention(&qkv, &b, 4, 0, false, 0.1).unwrap();
            best = best.min(t.elapsed().as_secs_f64());
        }
        let qkv0 = Tensor::randn(0f32, 1., (1, 256, 256, 288), &d).unwrap();
        let b0 = Tensor::randn(0f32, 1., 288, &d).unwrap();
        let mut best0 = f64::MAX;
        for _ in 0..8 {
            let t = std::time::Instant::now();
            let _ = window_attention(&qkv0, &b0, 1, 8, false, 0.1).unwrap();
            best0 = best0.min(t.elapsed().as_secs_f64());
        }
        eprintln!("global 64x64x384/4 heads: {:.1} ms ({:.0} GMAC/s); stage0 windows: {:.1} ms", best * 1e3, 12.9 / best, best0 * 1e3);
    }
}
