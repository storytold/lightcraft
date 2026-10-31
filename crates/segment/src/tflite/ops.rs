//! The operator kernels of the TFLite interpreter, on NHWC `f32` buffers.
//!
//! Semantics follow the TensorFlow Lite builtin operators as documented in TensorFlow
//! (`tensorflow/lite/schema/schema.fbs` option fields; `tf.nn` / `tf.image` behaviour for padding,
//! resizing and activations), Apache-2.0, Copyright The TensorFlow Authors; written from that
//! behaviour by the LightCraft contributors in 2026, no code copied.
//!
//! Every kernel checks the lengths of its inputs and output against the shapes validated at load
//! before it touches them; inside the loops, offsets come from those shapes. Kernels write every
//! element of their output buffer (which comes from a [`Pool`] and holds stale values).

use std::sync::{Mutex, PoisonError};

use rayon::prelude::*;

use super::bad;
use crate::Result;

/// A fused activation (`ActivationFunctionType`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Act {
    #[default]
    None,
    Relu,
    ReluN1To1,
    Relu6,
}

impl Act {
    pub fn from_code(code: i8) -> Result<Self> {
        Ok(match code {
            0 => Act::None,
            1 => Act::Relu,
            2 => Act::ReluN1To1,
            3 => Act::Relu6,
            c => return Err(bad(&format!("fused activation {c} is not supported (NONE, RELU, RELU_N1_TO_1, RELU6 are)"))),
        })
    }

    #[inline]
    pub fn apply(self, v: f32) -> f32 {
        match self {
            Act::None => v,
            Act::Relu => v.max(0.0),
            Act::ReluN1To1 => v.clamp(-1.0, 1.0),
            Act::Relu6 => v.clamp(0.0, 6.0),
        }
    }
}

/// What an operator applies to its result: its fused activation, then (when a following PRELU
/// was folded into it) a per-channel PRELU.
#[derive(Clone, Debug, Default)]
pub struct Post {
    pub act: Act,
    pub alpha: Option<Vec<f32>>,
}

impl Post {
    pub fn act(act: Act) -> Self {
        Self { act, alpha: None }
    }

    pub fn is_identity(&self) -> bool {
        self.act == Act::None && self.alpha.is_none()
    }

    /// Apply to one pixel's channels, starting at channel `c0`.
    #[inline]
    fn row(&self, v: &mut [f32], c0: usize) {
        if self.act != Act::None {
            v.iter_mut().for_each(|x| *x = self.act.apply(*x));
        }
        if let Some(a) = self.alpha.as_deref().and_then(|a| a.get(c0..c0 + v.len())) {
            for (x, a) in v.iter_mut().zip(a) {
                if *x < 0.0 {
                    *x *= a;
                }
            }
        }
    }

    /// Apply to whole pixels of `channels` values.
    fn pixels(&self, v: &mut [f32], channels: usize) {
        if self.is_identity() || channels == 0 {
            return;
        }
        v.par_chunks_mut(channels * (CHUNK / channels).max(1)).for_each(|v| v.chunks_mut(channels).for_each(|p| self.row(p, 0)));
    }
}

/// Elements per parallel task in element-wise kernels.
const CHUNK: usize = 1 << 14;

fn check(what: &str, v: &[f32], n: usize) -> Result<()> {
    if v.len() != n {
        return Err(bad(&format!("{what}: {} values where {n} were expected", v.len())));
    }
    Ok(())
}

/// Reusable `f32` buffers, so a run doesn't fault in fresh pages for every tensor.
#[derive(Default)]
pub struct Pool(Mutex<Vec<Vec<f32>>>);

/// Buffers a pool keeps.
const POOL: usize = 48;

impl Pool {
    /// A buffer of `n` values (contents unspecified).
    pub fn take(&self, n: usize) -> Vec<f32> {
        let mut p = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let best = p.iter().enumerate().filter(|(_, v)| v.capacity() >= n).min_by_key(|(_, v)| v.capacity()).map(|(i, _)| i);
        let mut v = best.map(|i| p.swap_remove(i)).unwrap_or_default();
        drop(p);
        if v.len() >= n {
            v.truncate(n);
        } else {
            v.resize(n, 0.0);
        }
        v
    }

    /// Return a buffer for reuse.
    pub fn give(&self, v: Vec<f32>) {
        let mut p = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if p.len() < POOL {
            p.push(v);
        } else if let Some((i, _)) = p.iter().enumerate().min_by_key(|(_, b)| b.capacity()).filter(|(_, b)| b.capacity() < v.capacity()) {
            p[i] = v;
        }
    }
}

/// Padding before the data for SAME/VALID on one axis, and the output size (TFLite's rule: half
/// of the total padding before, the odd pixel after).
pub fn pad_before(same: bool, input: usize, kernel: usize, stride: usize, dilation: usize) -> (usize, usize) {
    let eff = kernel.saturating_sub(1).saturating_mul(dilation).saturating_add(1);
    let stride = stride.max(1);
    let out = if same { input.div_ceil(stride) } else { input.saturating_sub(eff) / stride + 1 };
    let total = out.saturating_sub(1).saturating_mul(stride).saturating_add(eff).saturating_sub(input);
    (out, total / 2)
}

/// Micro-tile of the matrix product: `MR` rows × `NR` columns.
const MR: usize = 4;
const NR: usize = 8;

/// A `k × n` weight matrix in `NR`-column panels (`[panel][k][NR]`, zero-padded), the layout the
/// micro-kernel streams.
pub struct Packed {
    pub k: usize,
    pub n: usize,
    panels: Vec<f32>,
}

impl Packed {
    /// Pack the matrix whose element `(row, col)` is `w(row, col)`.
    pub fn new(k: usize, n: usize, w: impl Fn(usize, usize) -> f32) -> Self {
        let np = n.div_ceil(NR);
        let mut panels = vec![0f32; np * k * NR];
        for (p, panel) in panels.chunks_exact_mut((k * NR).max(1)).enumerate() {
            for (kk, row) in panel.as_chunks_mut::<NR>().0.iter_mut().enumerate() {
                for (j, v) in row.iter_mut().enumerate() {
                    let c = p * NR + j;
                    if c < n {
                        *v = w(kk, c);
                    }
                }
            }
        }
        Self { k, n, panels }
    }
}

/// The `MR × NR` micro-kernel: `Σ_k a[k][i] · b[k][j]` for `a` `[k][MR]` and `b` `[k][NR]`.
#[inline(always)]
fn micro(b: &[f32], a: &[f32]) -> [[f32; NR]; MR] {
    let mut acc = [[0f32; NR]; MR];
    for (b8, a4) in b.as_chunks::<NR>().0.iter().zip(a.as_chunks::<MR>().0) {
        for i in 0..MR {
            for j in 0..NR {
                acc[i][j] += a4[i] * b8[j];
            }
        }
    }
    acc
}

/// Values of `a` a parallel task packs at most (its block stays in the L2 cache).
const BLOCK: usize = 1 << 15;

/// `out (m × n) = post(a (m × k) · w + bias)`, parallel over blocks of rows.
pub fn gemm(a: &[f32], m: usize, w: &Packed, bias: Option<&[f32]>, post: &Post, out: &mut [f32]) -> Result<()> {
    let (k, n) = (w.k, w.n);
    check("matrix product input", a, m * k)?;
    check("matrix product output", out, m * n)?;
    if let Some(b) = bias {
        check("bias", b, n)?;
    }
    if m == 0 || n == 0 {
        return Ok(());
    }
    if k == 0 {
        for row in out.chunks_exact_mut(n) {
            match bias {
                Some(b) => row.copy_from_slice(b),
                None => row.fill(0.0),
            }
            post.row(row, 0);
        }
        return Ok(());
    }
    let threads = rayon::current_num_threads().max(1);
    let rb = (m / (threads * 4)).min(64).min(BLOCK / k).max(MR) / MR * MR;
    out.par_chunks_mut(rb * n).zip(a.par_chunks(rb * k)).for_each(|(o, a)| {
        let rows = a.len() / k;
        let groups = rows.div_ceil(MR);
        let mut ap = vec![0f32; groups * k * MR];
        for (g, ag) in ap.chunks_exact_mut(k * MR).enumerate() {
            for r in 0..MR {
                let Some(src) = a.get((g * MR + r) * k..(g * MR + r + 1) * k) else { continue };
                for (d, v) in ag.iter_mut().skip(r).step_by(MR).zip(src) {
                    *d = *v;
                }
            }
        }
        for (pi, panel) in w.panels.chunks_exact(k * NR).enumerate() {
            let c0 = pi * NR;
            let cols = NR.min(n - c0);
            for (g, ag) in ap.chunks_exact(k * MR).enumerate() {
                let acc = micro(panel, ag);
                for (r, acc) in acc.iter().enumerate() {
                    let row = g * MR + r;
                    if row >= rows {
                        break;
                    }
                    let Some(dst) = o.get_mut(row * n + c0..row * n + c0 + cols) else { continue };
                    match bias.and_then(|b| b.get(c0..c0 + cols)) {
                        Some(b) => {
                            for ((d, v), b) in dst.iter_mut().zip(acc).zip(b) {
                                *d = v + b;
                            }
                        }
                        None => dst.copy_from_slice(acc.get(..cols).unwrap_or(&[])),
                    }
                    post.row(dst, c0);
                }
            }
        }
    });
    Ok(())
}

/// The geometry of a 2-D window operation (convolution, pooling) on NHWC data.
#[derive(Clone, Debug)]
pub struct Window {
    pub n: usize,
    pub h: usize,
    pub w: usize,
    pub c: usize,
    pub oh: usize,
    pub ow: usize,
    pub kh: usize,
    pub kw: usize,
    pub sh: usize,
    pub sw: usize,
    pub dh: usize,
    pub dw: usize,
    pub pt: usize,
    pub pl: usize,
}

impl Window {
    pub fn new(same: bool, [n, h, w, c]: [usize; 4], (kh, kw): (usize, usize), (sh, sw): (usize, usize), (dh, dw): (usize, usize)) -> Self {
        let (oh, pt) = pad_before(same, h, kh, sh, dh);
        let (ow, pl) = pad_before(same, w, kw, sw, dw);
        Self { n, h, w, c, oh, ow, kh, kw, sh, sw, dh, dw, pt, pl }
    }

    /// Input row/column of kernel tap `k` for output `o` on an axis, if inside the image.
    #[inline]
    fn tap(o: usize, k: usize, stride: usize, dil: usize, pad: usize, size: usize) -> Option<usize> {
        (o * stride + k * dil).checked_sub(pad).filter(|&i| i < size)
    }

    fn in_len(&self) -> usize {
        self.n * self.h * self.w * self.c
    }

    fn out_len(&self, channels: usize) -> usize {
        self.n * self.oh * self.ow * channels
    }

    /// Calls `f(tap index, input offset)` for the taps of output `(b, oy, ox)` inside the image.
    #[inline]
    fn taps(&self, b: usize, oy: usize, ox: usize, mut f: impl FnMut(usize, Option<usize>)) {
        for ky in 0..self.kh {
            let iy = Self::tap(oy, ky, self.sh, self.dh, self.pt, self.h);
            for kx in 0..self.kw {
                let ix = Self::tap(ox, kx, self.sw, self.dw, self.pl, self.w);
                f(ky * self.kw + kx, iy.zip(ix).map(|(iy, ix)| ((b * self.h + iy) * self.w + ix) * self.c));
            }
        }
    }
}

/// CONV_2D: im2col + matrix product. `w` packs the `(kh·kw·c) × o` weight matrix.
pub struct Conv {
    pub g: Window,
    pub w: Packed,
    pub bias: Vec<f32>,
    pub post: Post,
}

impl Conv {
    pub fn out_shape(&self) -> [usize; 4] {
        [self.g.n, self.g.oh, self.g.ow, self.w.n]
    }

    fn pointwise(&self) -> bool {
        let g = &self.g;
        g.kh == 1 && g.kw == 1 && g.sh == 1 && g.sw == 1 && g.pt == 0 && g.pl == 0
    }

    pub fn run(&self, x: &[f32], out: &mut [f32], pool: &Pool) -> Result<()> {
        let g = &self.g;
        check("CONV_2D input", x, g.in_len())?;
        let m = g.n * g.oh * g.ow;
        if self.pointwise() {
            return gemm(x, m, &self.w, Some(&self.bias), &self.post, out);
        }
        let mut col = pool.take(m * self.w.k);
        im2col(g, x, &mut col);
        let r = gemm(&col, m, &self.w, Some(&self.bias), &self.post, out);
        pool.give(col);
        r
    }
}

fn im2col(g: &Window, x: &[f32], col: &mut [f32]) {
    let c = g.c;
    let k = g.kh * g.kw * c;
    if k == 0 || g.ow == 0 || g.oh == 0 {
        return;
    }
    col.par_chunks_mut(g.ow * k).enumerate().for_each(|(row, dst)| {
        let (b, oy) = (row / g.oh, row % g.oh);
        for (ox, dst) in dst.chunks_exact_mut(k).enumerate() {
            g.taps(b, oy, ox, |t, src| {
                let Some(d) = dst.get_mut(t * c..(t + 1) * c) else { return };
                match src.and_then(|s| x.get(s..s + c)) {
                    Some(s) => d.copy_from_slice(s),
                    None => d.fill(0.0),
                }
            });
        }
    });
}

/// DEPTHWISE_CONV_2D: direct loops. `w` is `kh × kw × (c·mult)`, as stored.
pub struct Depthwise {
    pub g: Window,
    pub mult: usize,
    pub w: Vec<f32>,
    pub bias: Vec<f32>,
    pub post: Post,
}

/// Channels a depthwise accumulator holds in registers.
const LANES: usize = 16;

impl Depthwise {
    pub fn out_shape(&self) -> [usize; 4] {
        [self.g.n, self.g.oh, self.g.ow, self.g.c * self.mult]
    }

    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        let g = &self.g;
        check("DEPTHWISE_CONV_2D input", x, g.in_len())?;
        let oc = g.c * self.mult;
        check("DEPTHWISE_CONV_2D weights", &self.w, g.kh * g.kw * oc)?;
        check("DEPTHWISE_CONV_2D bias", &self.bias, oc)?;
        check("DEPTHWISE_CONV_2D output", out, g.out_len(oc))?;
        if oc == 0 || g.ow == 0 || g.oh == 0 {
            return Ok(());
        }
        out.par_chunks_mut(g.ow * oc).enumerate().for_each(|(row, dst)| {
            let (b, oy) = (row / g.oh, row % g.oh);
            let mut taps: Vec<(usize, usize)> = Vec::with_capacity(g.kh * g.kw);
            for (ox, acc) in dst.chunks_exact_mut(oc).enumerate() {
                taps.clear();
                g.taps(b, oy, ox, |t, src| {
                    if let Some(s) = src {
                        taps.push((s, t * oc));
                    }
                });
                if self.mult == 1 {
                    // channel blocks held in registers across the taps
                    let mut c0 = 0;
                    while c0 + LANES <= oc {
                        let mut a = [0f32; LANES];
                        if let Some(b) = self.bias.get(c0..c0 + LANES) {
                            a.copy_from_slice(b);
                        }
                        for &(s, wk) in &taps {
                            let (Some(i), Some(w)) = (x.get(s + c0..s + c0 + LANES), self.w.get(wk + c0..wk + c0 + LANES)) else { continue };
                            for j in 0..LANES {
                                a[j] += i[j] * w[j];
                            }
                        }
                        if let Some(d) = acc.get_mut(c0..c0 + LANES) {
                            d.copy_from_slice(&a);
                        }
                        c0 += LANES;
                    }
                    let rest = acc.get_mut(c0..).unwrap_or(&mut []);
                    rest.copy_from_slice(self.bias.get(c0..).unwrap_or(&[]));
                    for &(s, wk) in &taps {
                        let (Some(i), Some(w)) = (x.get(s + c0..s + g.c), self.w.get(wk + c0..wk + oc)) else { continue };
                        for ((a, i), w) in rest.iter_mut().zip(i).zip(w) {
                            *a += i * w;
                        }
                    }
                } else {
                    acc.copy_from_slice(&self.bias);
                    for &(s, wk) in &taps {
                        let (Some(inp), Some(wk)) = (x.get(s..s + g.c), self.w.get(wk..wk + oc)) else { continue };
                        for ((a, w), i) in acc.chunks_exact_mut(self.mult).zip(wk.chunks_exact(self.mult)).zip(inp) {
                            for (a, w) in a.iter_mut().zip(w) {
                                *a += i * w;
                            }
                        }
                    }
                }
                self.post.row(acc, 0);
            }
        });
        Ok(())
    }
}

/// TRANSPOSE_CONV: `input · w` gives every input pixel's `kh × kw × o` contribution, gathered into
/// the output. `w` packs the `c × (kh·kw·o)` matrix.
pub struct TransposeConv {
    pub n: usize,
    pub h: usize,
    pub w: usize,
    pub oh: usize,
    pub ow: usize,
    pub o: usize,
    pub kh: usize,
    pub kw: usize,
    pub sh: usize,
    pub sw: usize,
    pub pt: usize,
    pub pl: usize,
    pub weights: Packed,
    pub bias: Vec<f32>,
    pub post: Post,
}

impl TransposeConv {
    pub fn out_shape(&self) -> [usize; 4] {
        [self.n, self.oh, self.ow, self.o]
    }

    pub fn run(&self, x: &[f32], out: &mut [f32], pool: &Pool) -> Result<()> {
        let m = self.n * self.h * self.w;
        check("TRANSPOSE_CONV bias", &self.bias, self.o)?;
        check("TRANSPOSE_CONV output", out, self.n * self.oh * self.ow * self.o)?;
        let kko = self.kh * self.kw * self.o;
        if kko != self.weights.n {
            return Err(bad("TRANSPOSE_CONV weights"));
        }
        let mut cols = pool.take(m * kko);
        gemm(x, m, &self.weights, None, &Post::default(), &mut cols)?;
        if self.o > 0 && self.ow > 0 && self.oh > 0 {
            // output pixel (oy, ox) receives input (y, x) through tap (ky, kx) when y·s + k − pad = o
            let src = |o: usize, k: usize, s: usize, pad: usize, size: usize| -> Option<usize> {
                let t = (o + pad).checked_sub(k)?;
                (t % s == 0 && t / s < size).then_some(t / s)
            };
            out.par_chunks_mut(self.ow * self.o).enumerate().for_each(|(row, dst)| {
                let (b, oy) = (row / self.oh, row % self.oh);
                for (ox, acc) in dst.chunks_exact_mut(self.o).enumerate() {
                    acc.copy_from_slice(&self.bias);
                    for ky in 0..self.kh {
                        let Some(y) = src(oy, ky, self.sh.max(1), self.pt, self.h) else { continue };
                        for kx in 0..self.kw {
                            let Some(x) = src(ox, kx, self.sw.max(1), self.pl, self.w) else { continue };
                            let at = ((b * self.h + y) * self.w + x) * kko + (ky * self.kw + kx) * self.o;
                            let Some(c) = cols.get(at..at + self.o) else { continue };
                            for (a, c) in acc.iter_mut().zip(c) {
                                *a += c;
                            }
                        }
                    }
                    self.post.row(acc, 0);
                }
            });
        }
        pool.give(cols);
        Ok(())
    }
}

/// MAX_POOL_2D (padding cells excluded).
pub struct MaxPool {
    pub g: Window,
    pub post: Post,
}

impl MaxPool {
    pub fn out_shape(&self) -> [usize; 4] {
        [self.g.n, self.g.oh, self.g.ow, self.g.c]
    }

    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        let g = &self.g;
        check("MAX_POOL_2D input", x, g.in_len())?;
        check("MAX_POOL_2D output", out, g.out_len(g.c))?;
        if g.c == 0 || g.ow == 0 || g.oh == 0 {
            return Ok(());
        }
        out.par_chunks_mut(g.ow * g.c).enumerate().for_each(|(row, dst)| {
            let (b, oy) = (row / g.oh, row % g.oh);
            for (ox, acc) in dst.chunks_exact_mut(g.c).enumerate() {
                acc.fill(f32::NEG_INFINITY);
                g.taps(b, oy, ox, |_, src| {
                    let Some(inp) = src.and_then(|s| x.get(s..s + g.c)) else { return };
                    for (a, v) in acc.iter_mut().zip(inp) {
                        *a = a.max(*v);
                    }
                });
                self.post.row(acc, 0);
            }
        });
        Ok(())
    }
}

/// Element-wise binary operations with NumPy broadcasting (over 4-D shapes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Mul,
    /// `x ≥ 0 ? x : x · alpha` (PRELU; the second operand is alpha).
    Prelu,
}

pub struct Binary {
    pub op: BinOp,
    pub a: [usize; 4],
    pub b: [usize; 4],
    pub out: [usize; 4],
    pub post: Post,
}

/// The broadcast shape of two 4-D shapes.
pub fn broadcast(a: [usize; 4], b: [usize; 4]) -> Result<[usize; 4]> {
    let mut o = [0; 4];
    for i in 0..4 {
        o[i] = match (a[i], b[i]) {
            (x, y) if x == y => x,
            (1, y) => y,
            (x, 1) => x,
            _ => return Err(bad(&format!("shapes {a:?} and {b:?} don't broadcast"))),
        };
    }
    Ok(o)
}

fn strides(d: [usize; 4]) -> [usize; 4] {
    [d[1] * d[2] * d[3], d[2] * d[3], d[3], 1]
}

/// Strides that repeat broadcast (size-1) axes.
fn bstrides(d: [usize; 4]) -> [usize; 4] {
    let s = strides(d);
    std::array::from_fn(|i| if d[i] == 1 { 0 } else { s[i] })
}

/// Whether `small` broadcast to `out` just repeats it whole (ones, then `out`'s trailing axes).
fn repeats(small: [usize; 4], out: [usize; 4]) -> bool {
    let first = small.iter().position(|&d| d != 1).unwrap_or(4);
    small.get(first..) == out.get(first..)
}

impl Binary {
    #[inline]
    fn f(op: BinOp, x: f32, y: f32) -> f32 {
        match op {
            BinOp::Add => x + y,
            BinOp::Mul => x * y,
            BinOp::Prelu => {
                if x >= 0.0 {
                    x
                } else {
                    x * y
                }
            }
        }
    }

    pub fn run(&self, a: &[f32], b: &[f32], out: &mut [f32]) -> Result<()> {
        check("binary operand", a, self.a.iter().product())?;
        check("binary operand", b, self.b.iter().product())?;
        check("binary output", out, self.out.iter().product())?;
        let op = self.op;
        let c = self.out[3];
        if c == 0 || out.is_empty() {
            return Ok(());
        }
        // rows of whole pixels per task, so the channel index of the post-op is known
        let rows = c * (CHUNK / c).max(1);
        if self.a == self.out && (self.b == self.out || repeats(self.b, self.out)) {
            let nb = b.len().max(1);
            let same = b.len() == out.len();
            // a task's elements must start at a multiple of b's period
            let rows = if same || nb == 1 {
                rows
            } else if nb <= rows {
                rows / nb * nb
            } else {
                nb
            };
            out.par_chunks_mut(rows).zip(a.par_chunks(rows)).enumerate().for_each(|(t, (o, a))| {
                let start = t * rows;
                if same {
                    if let Some(b) = b.get(start..start + o.len()) {
                        for ((o, a), b) in o.iter_mut().zip(a).zip(b) {
                            *o = Self::f(op, *a, *b);
                        }
                    }
                } else {
                    for ((o, a), b) in o.iter_mut().zip(a).zip(b.iter().cycle().skip(start % nb)) {
                        *o = Self::f(op, *a, *b);
                    }
                }
                o.chunks_exact_mut(c).for_each(|p| self.post.row(p, 0));
            });
            return Ok(());
        }
        let (sa, sb) = (bstrides(self.a), bstrides(self.b));
        let (o1, o2) = (self.out[1], self.out[2]);
        out.par_chunks_mut(c).enumerate().for_each(|(row, dst)| {
            let (i0, i1, i2) = (row / (o1 * o2), row / o2 % o1, row % o2);
            let ba = i0 * sa[0] + i1 * sa[1] + i2 * sa[2];
            let bb = i0 * sb[0] + i1 * sb[1] + i2 * sb[2];
            for (i3, o) in dst.iter_mut().enumerate() {
                let (x, y) = (a.get(ba + i3 * sa[3]).copied().unwrap_or(0.0), b.get(bb + i3 * sb[3]).copied().unwrap_or(0.0));
                *o = Self::f(op, x, y);
            }
            self.post.row(dst, 0);
        });
        Ok(())
    }
}

/// Element-wise unary operations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unary {
    Act(Act),
    Logistic,
}

impl Unary {
    pub fn run(self, x: &[f32], out: &mut [f32]) -> Result<()> {
        check("unary output", out, x.len())?;
        out.par_chunks_mut(CHUNK).zip(x.par_chunks(CHUNK)).for_each(|(o, x)| {
            for (o, x) in o.iter_mut().zip(x) {
                *o = match self {
                    Unary::Act(a) => a.apply(*x),
                    Unary::Logistic => 1.0 / (1.0 + (-*x).exp()),
                };
            }
        });
        Ok(())
    }
}

/// PAD with zeros (4-D, non-negative paddings).
pub struct Pad {
    pub d: [usize; 4],
    pub before: [usize; 4],
    pub out: [usize; 4],
}

impl Pad {
    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        check("PAD input", x, self.d.iter().product())?;
        check("PAD output", out, self.out.iter().product())?;
        out.fill(0.0);
        let so = strides(self.out);
        let d = self.d;
        if d[3] == 0 {
            return Ok(());
        }
        for (row, src) in x.chunks_exact(d[3]).enumerate() {
            let (i0, i1, i2) = (row / (d[1] * d[2]), row / d[2] % d[1], row % d[2]);
            let at = (i0 + self.before[0]) * so[0] + (i1 + self.before[1]) * so[1] + (i2 + self.before[2]) * so[2] + self.before[3];
            if let Some(dst) = out.get_mut(at..at + d[3]) {
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    }
}

/// TRANSPOSE (4-D): output axis `j` is input axis `perm[j]`.
pub struct Transpose {
    pub d: [usize; 4],
    pub perm: [usize; 4],
}

impl Transpose {
    pub fn out_shape(&self) -> [usize; 4] {
        self.perm.map(|p| self.d.get(p).copied().unwrap_or(0))
    }

    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        check("TRANSPOSE input", x, self.d.iter().product())?;
        let od = self.out_shape();
        check("TRANSPOSE output", out, od.iter().product())?;
        let si = strides(self.d);
        let s: [usize; 4] = self.perm.map(|p| si.get(p).copied().unwrap_or(0));
        if od[3] == 0 {
            return Ok(());
        }
        out.par_chunks_mut(od[3]).enumerate().for_each(|(row, dst)| {
            let (i0, i1, i2) = (row / (od[1] * od[2]), row / od[2] % od[1], row % od[2]);
            let base = i0 * s[0] + i1 * s[1] + i2 * s[2];
            for (i3, o) in dst.iter_mut().enumerate() {
                *o = x.get(base + i3 * s[3]).copied().unwrap_or(0.0);
            }
        });
        Ok(())
    }
}

/// SOFTMAX over the last axis.
pub fn softmax(x: &[f32], inner: usize, beta: f32, out: &mut [f32]) -> Result<()> {
    check("SOFTMAX output", out, x.len())?;
    out.copy_from_slice(x);
    if inner == 0 {
        return Ok(());
    }
    out.par_chunks_mut(inner).for_each(|row| {
        let m = row.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
        let mut sum = 0f32;
        for v in row.iter_mut() {
            *v = ((*v - m) * beta).exp();
            sum += *v;
        }
        let inv = 1.0 / sum;
        row.iter_mut().for_each(|v| *v *= inv);
    });
    Ok(())
}

/// SUM over the axes flagged in `reduce` (4-D); the output keeps size-1 axes in their place.
pub struct Sum {
    pub d: [usize; 4],
    pub reduce: [bool; 4],
}

impl Sum {
    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        check("SUM input", x, self.d.iter().product())?;
        let od: [usize; 4] = std::array::from_fn(|i| if self.reduce[i] { 1 } else { self.d[i] });
        check("SUM output", out, od.iter().product())?;
        out.fill(0.0);
        let so = bstrides(od);
        let d = self.d;
        if d[3] == 0 {
            return Ok(());
        }
        for (row, src) in x.chunks_exact(d[3]).enumerate() {
            let (i0, i1, i2) = (row / (d[1] * d[2]), row / d[2] % d[1], row % d[2]);
            let base = i0 * so[0] + i1 * so[1] + i2 * so[2];
            if so[3] == 1 {
                if let Some(o) = out.get_mut(base..base + d[3]) {
                    o.iter_mut().zip(src).for_each(|(o, v)| *o += v);
                }
            } else if let Some(o) = out.get_mut(base) {
                *o += src.iter().sum::<f32>();
            }
        }
        Ok(())
    }
}

/// RESIZE_BILINEAR / RESIZE_NEAREST_NEIGHBOR on NHWC.
pub struct Resize {
    pub n: usize,
    pub h: usize,
    pub w: usize,
    pub c: usize,
    pub oh: usize,
    pub ow: usize,
    pub bilinear: bool,
    pub align_corners: bool,
    pub half_pixel: bool,
}

/// Sample positions on one axis: (lower, upper, weight of upper).
fn axis(bilinear: bool, align: bool, half: bool, input: usize, output: usize) -> Vec<(usize, usize, f32)> {
    let scale = if align && output > 1 { input.saturating_sub(1) as f32 / (output - 1) as f32 } else { input as f32 / output.max(1) as f32 };
    let last = input.saturating_sub(1);
    (0..output)
        .map(|o| {
            if bilinear {
                let v = if half { (o as f32 + 0.5) * scale - 0.5 } else { o as f32 * scale };
                let lo = (v.floor().max(0.0) as usize).min(last);
                let hi = (v.ceil().max(0.0) as usize).min(last);
                (lo, hi, v - lo as f32)
            } else {
                let off = if half { 0.5 } else { 0.0 };
                let v = (o as f32 + off) * scale;
                let i = if align { v.round() } else { v.floor() };
                let i = (i.max(0.0) as usize).min(last);
                (i, i, 0.0)
            }
        })
        .collect()
}

impl Resize {
    pub fn out_shape(&self) -> [usize; 4] {
        [self.n, self.oh, self.ow, self.c]
    }

    pub fn run(&self, x: &[f32], out: &mut [f32]) -> Result<()> {
        check("RESIZE input", x, self.n * self.h * self.w * self.c)?;
        let c = self.c;
        check("RESIZE output", out, self.n * self.oh * self.ow * c)?;
        let ys = axis(self.bilinear, self.align_corners, self.half_pixel, self.h, self.oh);
        let xs = axis(self.bilinear, self.align_corners, self.half_pixel, self.w, self.ow);
        if c == 0 || self.ow == 0 || self.oh == 0 {
            return Ok(());
        }
        let px = |b: usize, y: usize, x0: usize| -> &[f32] {
            let at = ((b * self.h + y) * self.w + x0) * c;
            x.get(at..at + c).unwrap_or(&[])
        };
        out.par_chunks_mut(self.ow * c).enumerate().for_each(|(row, dst)| {
            let (b, oy) = (row / self.oh, row % self.oh);
            let (y0, y1, fy) = ys.get(oy).copied().unwrap_or((0, 0, 0.0));
            for (o, &(x0, x1, fx)) in dst.chunks_exact_mut(c).zip(&xs) {
                if self.bilinear {
                    let (p00, p01, p10, p11) = (px(b, y0, x0), px(b, y0, x1), px(b, y1, x0), px(b, y1, x1));
                    for (i, o) in o.iter_mut().enumerate() {
                        let g = |s: &[f32]| s.get(i).copied().unwrap_or(0.0);
                        *o = g(p00) * (1.0 - fy) * (1.0 - fx) + g(p01) * (1.0 - fy) * fx + g(p10) * fy * (1.0 - fx) + g(p11) * fy * fx;
                    }
                } else {
                    let s = px(b, y0, x0);
                    for (i, o) in o.iter_mut().enumerate() {
                        *o = s.get(i).copied().unwrap_or(0.0);
                    }
                }
            }
        });
        Ok(())
    }
}

/// CONCATENATION along one axis: `outer` blocks, each the inputs' `inner` runs in order.
pub fn concat(xs: &[&[f32]], outer: usize, inners: &[usize], post: &Post, out: &mut [f32]) -> Result<()> {
    for (x, inner) in xs.iter().zip(inners) {
        check("CONCATENATION input", x, outer * inner)?;
    }
    check("CONCATENATION output", out, outer * inners.iter().sum::<usize>())?;
    let mut at = 0;
    for o in 0..outer {
        for (x, &inner) in xs.iter().zip(inners) {
            if let (Some(d), Some(s)) = (out.get_mut(at..at + inner), x.get(o * inner..(o + 1) * inner)) {
                d.copy_from_slice(s);
            }
            at += inner;
        }
    }
    post.pixels(out, 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain reference convolution (NHWC, OHWI weights, explicit padding).
    fn conv_ref(x: &[f32], g: &Window, o: usize, w: &[f32], bias: &[f32]) -> Vec<f32> {
        let mut out = vec![0f32; g.n * g.oh * g.ow * o];
        for b in 0..g.n {
            for oy in 0..g.oh {
                for ox in 0..g.ow {
                    for oc in 0..o {
                        let mut acc = bias[oc];
                        for ky in 0..g.kh {
                            for kx in 0..g.kw {
                                let iy = (oy * g.sh + ky * g.dh) as isize - g.pt as isize;
                                let ix = (ox * g.sw + kx * g.dw) as isize - g.pl as isize;
                                if iy < 0 || ix < 0 || iy >= g.h as isize || ix >= g.w as isize {
                                    continue;
                                }
                                for ic in 0..g.c {
                                    acc += x[((b * g.h + iy as usize) * g.w + ix as usize) * g.c + ic] * w[((oc * g.kh + ky) * g.kw + kx) * g.c + ic];
                                }
                            }
                        }
                        out[((b * g.oh + oy) * g.ow + ox) * o + oc] = acc;
                    }
                }
            }
        }
        out
    }

    fn ramp(n: usize, k: f32) -> Vec<f32> {
        (0..n).map(|i| ((i as f32 * k).sin() * 3.0).round() / 2.0).collect()
    }

    /// Pack OHWI weights (`o` rows of `k`) as a `k × o` matrix.
    fn packed(w: &[f32], o: usize, k: usize) -> Packed {
        Packed::new(k, o, |kk, oc| w[oc * k + kk])
    }

    /// A stale output buffer: kernels must overwrite every value.
    fn stale(n: usize) -> Vec<f32> {
        vec![f32::NAN; n]
    }

    fn conv(c: &Conv, x: &[f32]) -> Vec<f32> {
        let mut out = stale(c.out_shape().iter().product());
        c.run(x, &mut out, &Pool::default()).unwrap();
        out
    }

    #[test]
    fn same_padding_stride_2() {
        // TFLite: 5 → 3 with k = 3, s = 2: total padding 2, 1 before; 4 → 2: total 1, 0 before
        assert_eq!(pad_before(true, 5, 3, 2, 1), (3, 1));
        assert_eq!(pad_before(true, 4, 3, 2, 1), (2, 0));
        assert_eq!(pad_before(false, 5, 3, 2, 1), (2, 0));
        // dilation 2: effective kernel 5
        assert_eq!(pad_before(true, 6, 3, 1, 2), (6, 2));
        assert_eq!(pad_before(false, 6, 3, 1, 2), (2, 0));
    }

    #[test]
    fn conv_matches_reference() {
        // a 1-channel 4 × 4 image, 3 × 3 kernel of ones, SAME, stride 2: hand-computed window sums
        let x: Vec<f32> = (1..=16).map(|v| v as f32).collect();
        let g = Window::new(true, [1, 4, 4, 1], (3, 3), (2, 2), (1, 1));
        assert_eq!((g.oh, g.ow, g.pt, g.pl), (2, 2, 0, 0));
        let c = Conv { g: g.clone(), w: packed(&[1.0; 9], 1, 9), bias: vec![0.5], post: Post::default() };
        // windows: rows 0-2 × cols 0-2, rows 0-2 × cols 2-3, rows 2-3 × cols 0-2, rows 2-3 × cols 2-3
        assert_eq!(conv(&c, &x), vec![54.5, 45.5, 72.5, 54.5]);

        // against the plain loops, with dilation, several channels and odd sizes (row and column
        // remainders of the micro-kernel)
        for (same, hw, k, s, d, ci, o) in
            [(true, 7, 3, 2, 1, 3, 4), (false, 9, 3, 1, 2, 3, 9), (true, 8, 2, 2, 1, 5, 17), (true, 6, 1, 1, 1, 16, 8), (true, 5, 5, 2, 1, 3, 3)]
        {
            let g = Window::new(same, [2, hw, hw, ci], (k, k), (s, s), (d, d));
            let x = ramp(2 * hw * hw * ci, 0.37);
            let w = ramp(o * k * k * ci, 0.91);
            let bias = ramp(o, 1.3);
            let c = Conv { g: g.clone(), w: packed(&w, o, k * k * ci), bias: bias.clone(), post: Post::default() };
            let got = conv(&c, &x);
            let want = conv_ref(&x, &g, o, &w, &bias);
            for (a, b) in got.iter().zip(&want) {
                assert!((a - b).abs() < 1e-4, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn fused_prelu_and_activation() {
        let g = Window::new(true, [1, 1, 2, 2], (1, 1), (1, 1), (1, 1));
        let w = [1.0, 0.0, 0.0, 1.0]; // identity
        let post = Post { act: Act::ReluN1To1, alpha: Some(vec![0.5, 0.25]) };
        let c = Conv { g, w: packed(&w, 2, 2), bias: vec![0.0, 0.0], post };
        // clamp to [-1, 1] first, then PRELU per channel
        assert_eq!(conv(&c, &[-4.0, -4.0, 0.5, 3.0]), vec![-0.5, -0.25, 0.5, 1.0]);
    }

    #[test]
    fn depthwise_with_multiplier() {
        // 2 channels, multiplier 2: output channel ic·2 + m
        let g = Window::new(false, [1, 3, 3, 2], (2, 2), (1, 1), (1, 1));
        let x: Vec<f32> = (0..18).map(|v| v as f32).collect();
        // weights 2 × 2 × 4: tap (ky, kx) weight for oc = 1 + oc (same per tap)
        let w: Vec<f32> = (0..4).flat_map(|_| [1.0, 2.0, 3.0, 4.0]).collect();
        let dw = Depthwise { g, mult: 2, w, bias: vec![0.0, 0.0, 0.0, 1.0], post: Post::default() };
        let mut out = stale(16);
        dw.run(&x, &mut out).unwrap();
        assert_eq!(dw.out_shape(), [1, 2, 2, 4]);
        // pixel (0, 0): channel 0 window sum = 0 + 2 + 6 + 8 = 16, channel 1 = 1 + 3 + 7 + 9 = 20
        assert_eq!(&out[..4], &[16.0, 32.0, 60.0, 81.0]);
        // dilation 2, SAME, 1 and 37 channels (register blocks and the remainder): against the
        // general convolution of each channel
        for c in [1, 37] {
            let g = Window::new(true, [1, 6, 6, c], (3, 3), (1, 1), (2, 2));
            let x = ramp(36 * c, 0.5);
            let w = ramp(9 * c, 0.3);
            let bias = ramp(c, 0.7);
            let dw = Depthwise { g: g.clone(), mult: 1, w: w.clone(), bias: bias.clone(), post: Post::act(Act::Relu6) };
            let mut out = stale(36 * c);
            dw.run(&x, &mut out).unwrap();
            for ch in 0..c {
                let g1 = Window { c: 1, ..g.clone() };
                let x1: Vec<f32> = x.iter().skip(ch).step_by(c).copied().collect();
                let w1: Vec<f32> = w.iter().skip(ch).step_by(c).copied().collect();
                let want = conv_ref(&x1, &g1, 1, &w1, &[bias[ch]]);
                for (i, v) in want.iter().enumerate() {
                    assert!((out[i * c + ch] - v.clamp(0.0, 6.0)).abs() < 1e-5);
                }
            }
        }
    }

    fn tconv(t: &TransposeConv, x: &[f32]) -> Vec<f32> {
        let mut out = stale(t.out_shape().iter().product());
        t.run(x, &mut out, &Pool::default()).unwrap();
        out
    }

    #[test]
    fn transpose_conv_scatters() {
        // 1 × 2 × 2 × 1 input, 2 × 2 kernel, stride 2, no padding: each input pixel paints a block
        let w = [1.0f32, 2.0, 3.0, 4.0]; // o = 1, kh = kw = 2, c = 1
        let tc = TransposeConv {
            n: 1,
            h: 2,
            w: 2,
            oh: 4,
            ow: 4,
            o: 1,
            kh: 2,
            kw: 2,
            sh: 2,
            sw: 2,
            pt: 0,
            pl: 0,
            weights: Packed::new(1, 4, |_, j| w[j]),
            bias: vec![1.0],
            post: Post::default(),
        };
        #[rustfmt::skip]
        let want = [
            2.0, 3.0, 11.0, 21.0,
            4.0, 5.0, 31.0, 41.0,
            101.0, 201.0, 1001.0, 2001.0,
            301.0, 401.0, 3001.0, 4001.0,
        ];
        assert_eq!(tconv(&tc, &[1.0, 10.0, 100.0, 1000.0]), want);
        // overlapping taps (k = 3, stride 2, padding 1 before): output 1 sums two input pixels
        let w = [1.0f32, 2.0, 3.0];
        let tc = TransposeConv {
            n: 1,
            h: 1,
            w: 2,
            oh: 1,
            ow: 4,
            o: 1,
            kh: 1,
            kw: 3,
            sh: 1,
            sw: 2,
            pt: 0,
            pl: 1,
            weights: Packed::new(1, 3, |_, j| w[j]),
            bias: vec![0.0],
            post: Post::default(),
        };
        // out[ox] = Σ x[i]·w[k] with ox = 2i + k − 1
        assert_eq!(tconv(&tc, &[1.0, 10.0]), vec![2.0, 3.0 + 10.0, 20.0, 30.0]);
    }

    #[test]
    fn resize_modes() {
        // 1 × 2 → 1 × 4 bilinear, half-pixel centers: 0.25 / 0.75 positions, clamped at the ends
        let r = |bilinear, align, half| {
            let mut out = stale(4);
            Resize { n: 1, h: 1, w: 2, c: 1, oh: 1, ow: 4, bilinear, align_corners: align, half_pixel: half }.run(&[0.0, 4.0], &mut out).unwrap();
            out
        };
        assert_eq!(r(true, false, true), vec![0.0, 1.0, 3.0, 4.0]);
        // legacy (no half-pixel): positions 0, 0.5, 1, 1.5
        assert_eq!(r(true, false, false), vec![0.0, 2.0, 4.0, 4.0]);
        // align corners: positions 0, 1/3, 2/3, 1
        let a = r(true, true, false);
        assert!((a[1] - 4.0 / 3.0).abs() < 1e-6 && a[3] == 4.0);
        assert_eq!(r(false, false, true), vec![0.0, 0.0, 4.0, 4.0]);
        assert_eq!(r(false, false, false), vec![0.0, 0.0, 4.0, 4.0]);
        // 3 → 2 nearest with half-pixel: (0.5·1.5) = 0.75 → 0, (1.5·1.5) = 2.25 → 2
        let n = Resize { n: 1, h: 1, w: 3, c: 1, oh: 1, ow: 2, bilinear: false, align_corners: false, half_pixel: true };
        let mut out = stale(2);
        n.run(&[1.0, 2.0, 3.0], &mut out).unwrap();
        assert_eq!(out, vec![1.0, 3.0]);
    }

    fn bin(b: &Binary, x: &[f32], y: &[f32]) -> Vec<f32> {
        let mut out = stale(b.out.iter().product());
        b.run(x, y, &mut out).unwrap();
        out
    }

    #[test]
    fn elementwise_ops() {
        let b = Binary { op: BinOp::Mul, a: [1, 2, 1, 3], b: [1, 1, 1, 3], out: [1, 2, 1, 3], post: Post::default() };
        assert_eq!(bin(&b, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], &[1.0, 10.0, 100.0]), vec![1.0, 20.0, 300.0, 4.0, 50.0, 600.0]);
        let b = Binary { op: BinOp::Add, a: [1, 2, 1, 1], b: [1, 1, 1, 3], out: [1, 2, 1, 3], post: Post::act(Act::Relu) };
        assert_eq!(bin(&b, &[1.0, -20.0], &[0.0, 1.0, -2.0]), vec![1.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
        let p = Binary { op: BinOp::Prelu, a: [1, 1, 2, 2], b: [1, 1, 1, 2], out: [1, 1, 2, 2], post: Post::default() };
        assert_eq!(bin(&p, &[-1.0, -1.0, 2.0, -4.0], &[0.5, 0.25]), vec![-0.5, -0.25, 2.0, -1.0]);
        // a repeated operand over many parallel tasks
        let (h, c) = (300, 24);
        let x = ramp(h * h * c, 0.01);
        let y = ramp(h * c, 0.2);
        let b = Binary { op: BinOp::Add, a: [1, h, h, c], b: [1, 1, h, c], out: [1, h, h, c], post: Post::default() };
        let got = bin(&b, &x, &y);
        assert!(got.iter().enumerate().all(|(i, v)| *v == x[i] + y[i % (h * c)]));
        assert!(broadcast([1, 2, 3, 4], [1, 3, 3, 4]).is_err());
        assert!(b.run(&[1.0], &[1.0, 2.0, 3.0], &mut stale(6)).is_err());

        let mut s = stale(4);
        softmax(&[0.0, 0.0, 1.0, 1.0], 2, 1.0, &mut s).unwrap();
        assert_eq!(s, vec![0.5, 0.5, 0.5, 0.5]);
        let mut l = stale(1);
        Unary::Logistic.run(&[0.0], &mut l).unwrap();
        assert_eq!(l, vec![0.5]);
        let mut l = stale(3);
        Unary::Act(Act::ReluN1To1).run(&[-3.0, 0.5, 3.0], &mut l).unwrap();
        assert_eq!(l, vec![-1.0, 0.5, 1.0]);

        let x8 = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut o = stale(4);
        Sum { d: [1, 2, 2, 2], reduce: [false, false, true, false] }.run(&x8, &mut o).unwrap();
        assert_eq!(o, vec![4.0, 6.0, 12.0, 14.0]);
        let mut o = stale(4);
        Sum { d: [1, 2, 2, 2], reduce: [false, false, false, true] }.run(&x8, &mut o).unwrap();
        assert_eq!(o, vec![3.0, 7.0, 11.0, 15.0]);

        let t = Transpose { d: [1, 2, 3, 1], perm: [0, 2, 1, 3] };
        assert_eq!(t.out_shape(), [1, 3, 2, 1]);
        let mut o = stale(6);
        t.run(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], &mut o).unwrap();
        assert_eq!(o, vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);

        let p = Pad { d: [1, 1, 2, 1], before: [0, 1, 0, 1], out: [1, 2, 2, 3] };
        let mut o = stale(12);
        p.run(&[7.0, 8.0], &mut o).unwrap();
        assert_eq!(o, vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.0, 0.0, 0.0, 8.0, 0.0]);

        let mut o = stale(6);
        concat(&[&[1.0, 2.0], &[3.0, 4.0, 5.0, 6.0]], 2, &[1, 2], &Post::default(), &mut o).unwrap();
        assert_eq!(o, vec![1.0, 3.0, 4.0, 2.0, 5.0, 6.0]);

        let mp = MaxPool { g: Window::new(true, [1, 3, 3, 1], (2, 2), (2, 2), (1, 1)), post: Post::default() };
        let mut o = stale(4);
        mp.run(&[1.0, -2.0, 3.0, -4.0, -5.0, -6.0, 7.0, -8.0, -9.0], &mut o).unwrap();
        assert_eq!(o, vec![1.0, 3.0, 7.0, -9.0]);
    }

    #[test]
    fn pool_reuses_buffers() {
        let p = Pool::default();
        let v = p.take(1000);
        assert_eq!(v.len(), 1000);
        let cap = v.capacity();
        p.give(v);
        let w = p.take(10);
        assert_eq!((w.len(), w.capacity()), (10, cap));
        assert_eq!(p.take(2000).len(), 2000);
    }
}
