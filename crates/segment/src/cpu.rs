//! Fused CPU kernels for the model's elementwise work, attention and upsampling.
//!
//! On the CPU, candle runs bias adds, activations, the rotary encoding and layout copies (window
//! partitioning, splitting and merging heads) one element at a time on one thread, batched matrix
//! products one pair at a time and transposed convolutions one output channel at a time: in the
//! 32-layer ViT that was more than half of the encode time on a 12-core CPU, and most of a text
//! prompt went to attention over 5184 tokens. The elementwise kernels do the same arithmetic in
//! the same order (bit-identical results), fused into fewer passes over memory and spread over
//! the rayon pool that candle's matrix products use. Attention in cache-sized blocks and the
//! transposed convolutions as one product change how gemm blocks its sums: those results move
//! by float rounding (as they do between two CPUs, gemm picking its blocking from the caches).

use std::f32::consts::FRAC_1_SQRT_2;

use candle_core::cpu::erf::erf_f32;
use candle_core::{CpuStorage, CustomOp1, CustomOp2, CustomOp3, D, IndexOp, Layout, Shape, Tensor, bail};
use candle_nn::{ConvTranspose2d, Linear};
use rayon::prelude::*;

use crate::{Error, Result};

/// A `b × h × w` token grid (row-major, `heads × head_dim` channels) attended in non-overlapping
/// `win × win` windows, or as a whole when `win` is 0.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub b: usize,
    pub h: usize,
    pub w: usize,
    pub win: usize,
    pub heads: usize,
    pub head_dim: usize,
}

impl Grid {
    /// Window height and width.
    fn window(&self) -> (usize, usize) {
        if self.win == 0 { (self.h, self.w) } else { (self.win, self.win) }
    }

    /// Windows per image (down, across).
    fn per_image(&self) -> (usize, usize) {
        let (wh, ww) = self.window();
        (self.h / wh, self.w / ww)
    }

    fn windows(&self) -> usize {
        let (nh, nw) = self.per_image();
        self.b * nh * nw
    }

    fn tokens(&self) -> usize {
        let (wh, ww) = self.window();
        wh * ww
    }

    fn channels(&self) -> usize {
        self.heads * self.head_dim
    }

    fn check(&self) -> candle_core::Result<()> {
        let (wh, ww) = self.window();
        if wh == 0 || ww == 0 || !self.h.is_multiple_of(wh) || !self.w.is_multiple_of(ww) || !self.head_dim.is_multiple_of(2) {
            bail!("grid {self:?}: windows must tile the grid");
        }
        Ok(())
    }

    /// The grid row (token index in row-major order) of token `t` of window `win`.
    fn row(&self, win: usize, t: usize) -> usize {
        let (wh, ww) = self.window();
        let (nh, nw) = self.per_image();
        let (b, i) = (win / (nh * nw), win % (nh * nw));
        let (y, x) = (i / nw * wh + t / ww, i % nw * ww + t % ww);
        (b * self.h + y) * self.w + x
    }

    /// The window and the token within it of grid row `row`.
    fn window_of(&self, row: usize) -> (usize, usize) {
        let (wh, ww) = self.window();
        let (nh, nw) = self.per_image();
        let (b, i) = (row / (self.h * self.w), row % (self.h * self.w));
        let (y, x) = (i / self.w, i % self.w);
        ((b * nh + y / wh) * nw + x / ww, y % wh * ww + x % ww)
    }
}

/// The contiguous `f32` values of a CPU tensor.
fn values<'a>(s: &'a CpuStorage, l: &Layout) -> candle_core::Result<&'a [f32]> {
    match (s, l.contiguous_offsets()) {
        (CpuStorage::F32(v), Some((start, end))) => match v.get(start..end) {
            Some(v) => Ok(v),
            None => bail!("layout outside its storage"),
        },
        _ => bail!("fused kernels take contiguous f32 tensors"),
    }
}

/// `x · Wᵀ` over the rows of `x` (`[…, K]`) as `[rows, N]`: `Linear::forward` without the bias.
fn product(x: &Tensor, l: &Linear) -> Result<Tensor> {
    let k = x.dim(D::Minus1)?;
    Ok(x.reshape(((), k))?.matmul(&l.weight().t()?)?)
}

fn bias(l: &Linear) -> Result<&Tensor> {
    l.bias().ok_or_else(|| Error::Model("fused linear without a bias".into()))
}

/// `gelu_erf(x · Wᵀ + b)` (`gelu`) or `relu(x · Wᵀ + b)`: `[rows, N]`.
pub fn linear_act(x: &Tensor, l: &Linear, gelu: bool) -> Result<Tensor> {
    Ok(product(x, l)?.apply_op2_no_bwd(bias(l)?, &BiasAct { gelu })?)
}

/// `residual + (x · Wᵀ + b)`, shaped like `residual`.
pub fn linear_add(x: &Tensor, l: &Linear, residual: &Tensor) -> Result<Tensor> {
    Ok(product(x, l)?.apply_op3_no_bwd(bias(l)?, &residual.contiguous()?, &BiasAdd)?)
}

/// The projection `x · Wᵀ + b` of the grid's tokens (`x`: `[b, h, w, C]`), split into heads per
/// window: `[windows, heads, tokens, head_dim]`, with the rotary encoding (`cos`, `sin`:
/// `[tokens, head_dim]`) applied when given, then multiplied by `scale`.
pub fn heads(x: &Tensor, l: &Linear, grid: Grid, rope: Option<(&[f32], &[f32])>, scale: f32) -> Result<Tensor> {
    Ok(product(x, l)?.apply_op2_no_bwd(bias(l)?, &Heads { grid, rope, scale })?)
}

/// Attention scores computed per task (elements): a block of query rows against all keys,
/// small enough (256 KB) to stay in a core's L2 cache between the two products and the softmax.
/// 4 MB blocks were 3× slower for the detector's 5184-token attention (8 heads of 32).
const SCORE_TILE: usize = 1 << 16;

/// `nn::sdpa` on `[B, H, L, d]` tensors (`bias` `[B or 1, H or 1, Lq or 1, Lk]`): the same
/// operations (scores × `scale` unless it is 1, plus the bias, softmax, × v), but candle
/// multiplies batched matrices one pair after another, each spread over all threads, which is
/// slow for 9 windows × 16 heads of 576 tokens or 8 heads of 5184; here every (batch, head,
/// block of query rows) is a task of its own.
pub fn sdpa(q: &Tensor, k: &Tensor, v: &Tensor, bias: Option<&Tensor>, scale: f64) -> Result<Tensor> {
    let (b, h, lq, _) = q.dims4()?;
    let (lk, dv) = (k.dim(2)?, v.dim(3)?);
    let bias = match bias {
        Some(t) => Some((t, t.dims4()?)),
        None => None,
    };
    let rows = (SCORE_TILE / lk.max(1)).clamp(1, lq.max(1));
    let tasks: Vec<(usize, usize, usize)> = (0..b * h).flat_map(|i| (0..lq).step_by(rows).map(move |r| (i / h, i % h, r))).collect();
    let out = tasks
        .par_iter()
        .map(|&(bi, hi, r)| -> Result<Tensor> {
            let n = rows.min(lq - r);
            let mut att = q.i((bi, hi))?.narrow(0, r, n)?.matmul(&k.i((bi, hi))?.t()?)?;
            if scale != 1.0 {
                att = (att * scale)?;
            }
            if let Some((t, (tb, th, tq, _))) = bias {
                let t = t.i((if tb == 1 { 0 } else { bi }, if th == 1 { 0 } else { hi }))?;
                att = att.broadcast_add(&if tq == 1 { t } else { t.narrow(0, r, n)? })?;
            }
            Ok(candle_nn::ops::softmax_last_dim(&att)?.matmul(&v.i((bi, hi))?)?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Tensor::cat(&out, 0)?.reshape((b, h, lq, dv))?)
}

/// Attention over `q`, `k`, `v` from [`heads`] (`q` already scaled), merged back to the grid:
/// `[b, h, w, C]`.
pub fn attention(q: &Tensor, k: &Tensor, v: &Tensor, grid: Grid) -> Result<Tensor> {
    Ok(sdpa(q, k, v, None, 1.0)?.apply_op1_no_bwd(&Merge { grid })?)
}

/// `ConvTranspose2d` with a 2 × 2 kernel and stride 2 (the neck's upsampling) on `x`
/// `[1, Cin, H, W]`, then GELU when `gelu`: `[1, Cout, 2H, 2W]`. The kernel windows don't overlap,
/// so this is one product `[H·W, Cin] × [Cin, Cout·4]` and a reordering with the bias; candle
/// computes it one output channel at a time (~50 GFLOP/s here).
pub fn upsample2(x: &Tensor, ct: &ConvTranspose2d, gelu: bool) -> Result<Tensor> {
    let (b, cin, h, w) = x.dims4()?;
    let (wi, cout, kh, kw) = ct.weight().dims4()?;
    let c = ct.config();
    if b != 1 || wi != cin || (kh, kw, c.stride, c.padding, c.output_padding, c.dilation) != (2, 2, 2, 0, 0, 1) {
        return Err(Error::Model(format!("upsample2: {:?} with a {:?} kernel", x.dims(), ct.weight().dims())));
    }
    let y = x.reshape((cin, h * w))?.t()?.matmul(&ct.weight().reshape((cin, cout * 4))?)?;
    let bias = match ct.bias() {
        Some(b) => b.clone(),
        None => Tensor::zeros(cout, candle_core::DType::F32, x.device())?,
    };
    Ok(y.apply_op2_no_bwd(&bias, &Unpatch { h, w, gelu })?)
}

/// `relu(group_norm(x))` over `[B, C, …]` with `groups` groups and per-channel `weight`, `bias`.
/// candle's `GroupNorm` makes eight single-threaded passes over the 288² pixel decoder maps.
pub fn group_norm_relu(x: &Tensor, weight: &Tensor, bias: &Tensor, groups: usize, eps: f64) -> Result<Tensor> {
    Ok(x.contiguous()?.apply_op3_no_bwd(weight, bias, &GroupNormRelu { groups, eps })?)
}

/// The detector's box relative-position bias `[1, heads, 1 + queries, side²]` from its row and
/// column terms `ey`, `ex` (`[queries, side, heads]`): `ey[q, y, h] + ex[q, x, h]` at `y·side + x`,
/// after a row of zeros (the presence token).
pub fn box_bias(ey: &Tensor, ex: &Tensor) -> Result<Tensor> {
    Ok(ey.contiguous()?.apply_op2_no_bwd(&ex.contiguous()?, &BoxBias)?)
}

/// Rows of `x` (`[rows, N]`) with a bias `[N]`.
fn rows(x: &[f32], b: &[f32]) -> candle_core::Result<usize> {
    if b.is_empty() || !x.len().is_multiple_of(b.len()) {
        bail!("bias of {} for {} values", b.len(), x.len());
    }
    Ok(b.len())
}

struct BiasAct {
    gelu: bool,
}

impl CustomOp2 for BiasAct {
    fn name(&self) -> &'static str {
        if self.gelu { "bias-gelu-erf" } else { "bias-relu" }
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (x, b) = (values(s1, l1)?, values(s2, l2)?);
        let n = rows(x, b)?;
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(n).zip(x.par_chunks(n)).for_each(|(o, x)| {
            for ((o, x), b) in o.iter_mut().zip(x).zip(b) {
                let v = x + b;
                // candle's `gelu_erf` and `relu`
                *o = if self.gelu { (erf_f32(v * FRAC_1_SQRT_2) + 1.) * 0.5 * v } else { v.max(0f32) };
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

struct BiasAdd;

impl CustomOp3 for BiasAdd {
    fn name(&self) -> &'static str {
        "bias-residual-add"
    }

    fn cpu_fwd(
        &self,
        s1: &CpuStorage,
        l1: &Layout,
        s2: &CpuStorage,
        l2: &Layout,
        s3: &CpuStorage,
        l3: &Layout,
    ) -> candle_core::Result<(CpuStorage, Shape)> {
        let (x, b, r) = (values(s1, l1)?, values(s2, l2)?, values(s3, l3)?);
        let n = rows(x, b)?;
        if r.len() != x.len() {
            bail!("residual of {} for {} values", r.len(), x.len());
        }
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(n).zip(x.par_chunks(n)).zip(r.par_chunks(n)).for_each(|((o, x), r)| {
            for (((o, x), b), r) in o.iter_mut().zip(x).zip(b).zip(r) {
                *o = r + (x + b);
            }
        });
        Ok((CpuStorage::F32(out), l3.shape().clone()))
    }
}

struct Heads<'a> {
    grid: Grid,
    rope: Option<(&'a [f32], &'a [f32])>,
    scale: f32,
}

impl CustomOp2 for Heads<'_> {
    fn name(&self) -> &'static str {
        "split-heads-rope"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let g = self.grid;
        g.check()?;
        let (x, bias) = (values(s1, l1)?, values(s2, l2)?);
        let (c, d, n) = (g.channels(), g.head_dim, g.tokens());
        if bias.len() != c || x.len() != g.b * g.h * g.w * c {
            bail!("heads: {} values with a bias of {} for {g:?}", x.len(), bias.len());
        }
        if let Some((cos, sin)) = self.rope
            && (cos.len() != n * d || sin.len() != n * d)
        {
            bail!("rotary tables of {} and {} for {n} tokens", cos.len(), sin.len());
        }
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(n * d).enumerate().for_each(|(i, o)| {
            let (win, head) = (i / g.heads, i % g.heads);
            let bias = &bias[head * d..][..d];
            for (t, o) in o.chunks_exact_mut(d).enumerate() {
                let x = &x[g.row(win, t) * c + head * d..][..d];
                for ((o, x), b) in o.iter_mut().zip(x).zip(bias) {
                    *o = x + b;
                }
                if let Some((cos, sin)) = self.rope {
                    // x·cos + rotate_pairwise(x)·sin, rotate_pairwise mapping (a, b) to (−b, a)
                    let (cos, sin) = (&cos[t * d..][..d], &sin[t * d..][..d]);
                    for ((o, cos), sin) in o.as_chunks_mut::<2>().0.iter_mut().zip(cos.as_chunks::<2>().0).zip(sin.as_chunks::<2>().0) {
                        let [a, b] = *o;
                        *o = [a * cos[0] + -b * sin[0], b * cos[1] + a * sin[1]];
                    }
                }
                for o in o.iter_mut() {
                    *o *= self.scale;
                }
            }
        });
        let shape = Shape::from((g.windows(), g.heads, n, d));
        Ok((CpuStorage::F32(out), shape))
    }
}

struct GroupNormRelu {
    groups: usize,
    eps: f64,
}

impl CustomOp3 for GroupNormRelu {
    fn name(&self) -> &'static str {
        "group-norm-relu"
    }

    fn cpu_fwd(
        &self,
        s1: &CpuStorage,
        l1: &Layout,
        s2: &CpuStorage,
        l2: &Layout,
        s3: &CpuStorage,
        l3: &Layout,
    ) -> candle_core::Result<(CpuStorage, Shape)> {
        let (x, w, b) = (values(s1, l1)?, values(s2, l2)?, values(s3, l3)?);
        let dims = l1.shape().dims();
        let (batch, c) = match dims {
            [batch, c, _, ..] => (*batch, *c),
            _ => bail!("group norm of {dims:?}"),
        };
        if c == 0 || self.groups == 0 || !c.is_multiple_of(self.groups) || w.len() != c || b.len() != c || x.is_empty() {
            bail!("group norm: {dims:?} in {} groups, {} weights", self.groups, w.len());
        }
        let hw = x.len() / (batch * c);
        let per = c / self.groups * hw;
        // mean and σ per (image, group), as candle computes them (f32 sums in order)
        let n = (1.0 / per as f64) as f32;
        let stats: Vec<(f32, f32)> = x
            .par_chunks(per)
            .map(|g| {
                let mean = g.iter().fold(0f32, |s, v| s + v) * n;
                let var = g.iter().fold(0f32, |s, v| s + (v - mean) * (v - mean)) * n;
                (mean, (var + self.eps as f32).sqrt())
            })
            .collect();
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(hw).zip(x.par_chunks(hw)).enumerate().for_each(|(row, (o, x))| {
            let ch = row % c;
            let (mean, sd) = stats[row / c * self.groups + ch / (c / self.groups)];
            for (o, x) in o.iter_mut().zip(x) {
                *o = ((x - mean) / sd * w[ch] + b[ch]).max(0f32);
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

/// `[H·W, Cout·2·2]` patches (+ bias, then GELU) to `[1, Cout, 2H, 2W]`.
struct Unpatch {
    h: usize,
    w: usize,
    gelu: bool,
}

impl CustomOp2 for Unpatch {
    fn name(&self) -> &'static str {
        "unpatch-2x2"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (y, bias) = (values(s1, l1)?, values(s2, l2)?);
        let (h, w, cout) = (self.h, self.w, bias.len());
        if y.len() != h * w * cout * 4 {
            bail!("unpatch: {} values for {h} × {w} × {cout} × 4", y.len());
        }
        let mut out = vec![0f32; y.len()];
        // one output row (channel c, row 2·r + i) at a time
        out.par_chunks_mut(2 * w).enumerate().for_each(|(row, o)| {
            let (c, r, i) = (row / (2 * h), row % (2 * h) / 2, row % 2);
            for (x, o) in o.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let p = &y[(r * w + x) * cout * 4 + c * 4 + i * 2..][..2];
                for (o, p) in o.iter_mut().zip(p) {
                    let v = p + bias[c];
                    *o = if self.gelu { (erf_f32(v * FRAC_1_SQRT_2) + 1.) * 0.5 * v } else { v };
                }
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((1, cout, 2 * h, 2 * w))))
    }
}

struct BoxBias;

impl CustomOp2 for BoxBias {
    fn name(&self) -> &'static str {
        "box-relative-position-bias"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (ey, ex) = (values(s1, l1)?, values(s2, l2)?);
        let (q, side, heads) = l1.shape().dims3()?;
        if l2.shape() != l1.shape() {
            bail!("box bias terms {:?} and {:?}", l1.shape(), l2.shape());
        }
        let n = side * side;
        let mut out = vec![0f32; heads * (q + 1) * n];
        // row 0 of each head (the presence token) stays zero
        out.par_chunks_mut(n).enumerate().filter(|(i, _)| i % (q + 1) != 0).for_each(|(i, o)| {
            let (h, q) = (i / (q + 1), i % (q + 1) - 1);
            for (y, o) in o.chunks_exact_mut(side).enumerate() {
                let ey = ey[(q * side + y) * heads + h];
                for (x, o) in o.iter_mut().enumerate() {
                    *o = ey + ex[(q * side + x) * heads + h];
                }
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((1, heads, q + 1, n))))
    }
}

struct Merge {
    grid: Grid,
}

impl CustomOp1 for Merge {
    fn name(&self) -> &'static str {
        "merge-heads"
    }

    fn cpu_fwd(&self, s: &CpuStorage, l: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let g = self.grid;
        g.check()?;
        let x = values(s, l)?;
        let (c, d, n) = (g.channels(), g.head_dim, g.tokens());
        if x.len() != g.windows() * g.heads * n * d {
            bail!("merge: {} values for {g:?}", x.len());
        }
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(c).enumerate().for_each(|(row, o)| {
            let (win, t) = g.window_of(row);
            for (head, o) in o.chunks_exact_mut(d).enumerate() {
                o.copy_from_slice(&x[((win * g.heads + head) * n + t) * d..][..d]);
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((g.b, g.h, g.w, c))))
    }
}

#[cfg(test)]
mod tests {
    use candle_core::Device;
    use candle_nn::{ConvTranspose2dConfig, Module};

    use super::*;

    fn randn(shape: &[usize]) -> Tensor {
        Tensor::randn(0f32, 1.0, shape, &Device::Cpu).unwrap()
    }

    /// The kernels against candle's tensor operations: equal up to float rounding (on x86-64 most
    /// are bit-identical; the matrix products may block differently).
    fn close(name: &str, got: &Tensor, want: &Tensor) {
        assert_eq!(got.dims(), want.dims(), "{name}: shape");
        let (got, want): (Vec<f32>, Vec<f32>) = (got.flatten_all().unwrap().to_vec1().unwrap(), want.flatten_all().unwrap().to_vec1().unwrap());
        let scale = want.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let err = got.iter().zip(&want).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err <= 1e-5 * scale, "{name}: max error {err} (scale {scale})");
    }

    #[test]
    fn sdpa_matches_the_tensor_ops() {
        // 1000 keys: blocks of 65 query rows, the last one short
        let (q, k, v) = (randn(&[2, 3, 150, 8]), randn(&[2, 3, 1000, 8]), randn(&[2, 3, 1000, 5]));
        let biases =
            [None, Some(randn(&[1, 1, 1, 1000])), Some(randn(&[1, 3, 150, 1000])), Some(randn(&[2, 1, 150, 1000])), Some(randn(&[2, 3, 1, 1000]))];
        for (i, bias) in biases.iter().enumerate() {
            for scale in [0.35, 1.0] {
                let got = sdpa(&q, &k, &v, bias.as_ref(), scale).unwrap();
                let want = crate::nn::sdpa_tensors(&q, &k, &v, bias.as_ref(), scale).unwrap();
                close(&format!("bias {i}, scale {scale}"), &got, &want);
            }
        }
    }

    #[test]
    fn fused_linears_match_the_tensor_ops() {
        let x = randn(&[3, 5, 7]);
        let l = Linear::new(randn(&[6, 7]), Some(randn(&[6])));
        let y = l.forward(&x).unwrap().reshape((15, 6)).unwrap();
        close("gelu", &linear_act(&x, &l, true).unwrap(), &y.gelu_erf().unwrap());
        close("relu", &linear_act(&x, &l, false).unwrap(), &y.relu().unwrap());
        let r = randn(&[3, 5, 6]);
        close("residual", &linear_add(&x, &l, &r).unwrap(), &(&r + l.forward(&x).unwrap()).unwrap());
        assert!(linear_act(&x, &Linear::new(randn(&[6, 7]), None), true).is_err());
    }

    #[test]
    fn box_bias_matches_the_tensor_ops() {
        let (ey, ex) = (randn(&[4, 5, 3]), randn(&[4, 5, 3]));
        let want = ey.unsqueeze(2).unwrap().broadcast_add(&ex.unsqueeze(1).unwrap()).unwrap();
        let want = want.reshape((4, 25, 3)).unwrap().permute((2, 0, 1)).unwrap().pad_with_zeros(1, 1, 0).unwrap().unsqueeze(0).unwrap();
        close("box bias", &box_bias(&ey, &ex).unwrap(), &want);
    }

    #[test]
    fn upsampling_matches_the_transposed_convolution() {
        let x = randn(&[1, 6, 3, 4]);
        let cfg = ConvTranspose2dConfig { stride: 2, ..Default::default() };
        let ct = ConvTranspose2d::new(randn(&[6, 5, 2, 2]), Some(randn(&[5])), cfg);
        let want = ct.forward(&x).unwrap();
        close("upsample", &upsample2(&x, &ct, false).unwrap(), &want);
        close("upsample + gelu", &upsample2(&x, &ct, true).unwrap(), &want.gelu_erf().unwrap());
        let other = ConvTranspose2d::new(randn(&[6, 5, 2, 2]), None, ConvTranspose2dConfig::default());
        assert!(upsample2(&x, &other, false).is_err());
    }

    #[test]
    fn group_norm_matches_candle() {
        let x = randn(&[2, 6, 4, 5]);
        let (w, b) = (randn(&[6]), randn(&[6]));
        let want = candle_nn::GroupNorm::new(w.clone(), b.clone(), 6, 3, 1e-5).unwrap().forward(&x).unwrap().relu().unwrap();
        close("group norm", &group_norm_relu(&x, &w, &b, 3, 1e-5).unwrap(), &want);
        assert!(group_norm_relu(&x, &w, &b, 4, 1e-5).is_err());
    }

    #[test]
    fn window_indexing_round_trips() {
        for win in [0, 3] {
            let g = Grid { b: 2, h: 6, w: 9, win, heads: 1, head_dim: 2 };
            g.check().unwrap();
            let mut seen = vec![false; g.b * g.h * g.w];
            for w in 0..g.windows() {
                for t in 0..g.tokens() {
                    let row = g.row(w, t);
                    assert_eq!(g.window_of(row), (w, t));
                    seen[row] = true;
                }
            }
            assert!(seen.iter().all(|s| *s));
        }
        assert!(Grid { b: 1, h: 6, w: 9, win: 4, heads: 1, head_dim: 2 }.check().is_err());
    }
}
