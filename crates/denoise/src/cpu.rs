//! Single-thread CPU execution of the checked denoise network.
//!
//! Convolutions lower at most 256 spatial positions at a time, then use faer's safe views over pure-Rust GEMM.
//! No inner thread pool: `run::denoise_bayer` already runs tiles on the application's Rayon pool. Tensor lifetimes
//! are planned once; buffers and im2col storage are recycled between calls, independently for concurrent callers.

use crate::net::{Net, NetError, Op};
use crate::run::{Error, TileRunner};
use faer::{Parallelism, mat};
use std::sync::{Arc, Mutex};

const BLOCK: usize = 256;
/// Cap all tensor buffers and convolution scratch for one tile together (512 MiB).
const MAX_VALUES: usize = 128 * 1024 * 1024;
const MAX_TILE: usize = 2048;
const MAX_IDLE: usize = 1;

type Result<T> = std::result::Result<T, NetError>;
fn bad(why: &str) -> NetError {
    NetError::Invalid(why.into())
}
fn count(c: usize, side: usize) -> Result<usize> {
    c.checked_mul(side)
        .and_then(|n| n.checked_mul(side))
        .filter(|&n| n > 0 && n <= MAX_VALUES)
        .ok_or_else(|| bad("a tensor has no size or is too large"))
}
fn zeros(n: usize) -> Result<Vec<f32>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| bad("not enough memory for a denoise tile"))?;
    v.resize(n, 0.0);
    Ok(v)
}
fn sources(op: &Op) -> (usize, Option<usize>) {
    match op {
        Op::Conv(c) => (c.src, c.src2),
        Op::ConvT2(c) => (c.src, None),
        Op::MaxPool2 { src } | Op::DepthToSpace2 { src } => (*src, None),
    }
}
#[derive(Clone, Copy)]
struct Tensor {
    slot: usize,
    side: usize,
    channels: usize,
}
struct Plan {
    net: Net,
    tensors: Vec<Tensor>,
    sizes: Vec<usize>,
    lower: usize,
    product: usize,
    up_weights: Vec<Option<Vec<f32>>>,
}
struct Workspace {
    buffers: Vec<Vec<f32>>,
    lower: Vec<f32>,
    product: Vec<f32>,
}

/// A checked Net on square CHW tiles. Clones share immutable weights and the idle workspace cache.
#[derive(Clone)]
pub struct NetRunner {
    plan: Arc<Plan>,
    idle: Arc<Mutex<Vec<Workspace>>>,
}

impl NetRunner {
    pub fn new(net: &Net, tile: usize) -> Result<Self> {
        if tile == 0
            || tile > MAX_TILE
            || !net.fits_tile(tile)
            || net.in_channels != 4
            || net.out_channels() != 3
            || net.shape(0).is_none_or(|s| s.channels != net.in_channels)
        {
            return Err(bad("the tile or Bayer-to-RGB contract does not fit the network"));
        }
        let mut last = vec![0; net.ops().len() + 1];
        for (i, op) in net.ops().iter().enumerate() {
            let (a, b) = sources(op);
            for id in std::iter::once(a).chain(b) {
                *last.get_mut(id).ok_or_else(|| bad("missing tensor"))? = i + 1;
            }
        }
        let mut tensors = vec![Tensor { slot: 0, side: tile, channels: net.in_channels }];
        let mut sizes = vec![count(net.in_channels, tile)?];
        let mut busy = vec![true];
        let (mut lower, mut product) = (0, 0);
        let mut up_weights = Vec::new();
        for (i, op) in net.ops().iter().enumerate() {
            let (src, src2) = sources(op);
            let a = *tensors.get(src).ok_or_else(|| bad("missing tensor"))?;
            let (channels, side) = match op {
                Op::Conv(c) => {
                    if let Some(s) = src2
                        && tensors.get(s).is_none_or(|b| b.side != a.side)
                    {
                        return Err(bad("a join has different sizes"));
                    }
                    (c.cout, a.side)
                }
                Op::ConvT2(c) => (c.cout, a.side.checked_mul(2).ok_or_else(|| bad("size overflow"))?),
                Op::MaxPool2 { .. } => {
                    if a.side < 2 || !a.side.is_multiple_of(2) {
                        return Err(bad("pooling does not divide the tile"));
                    }
                    (a.channels, a.side / 2)
                }
                Op::DepthToSpace2 { .. } => (a.channels / 4, a.side.checked_mul(2).ok_or_else(|| bad("size overflow"))?),
            };
            let n = count(channels, side)?;
            let slot = busy
                .iter()
                .zip(&sizes)
                .enumerate()
                .filter(|(_, (used, _))| !**used)
                .min_by_key(|(_, (_, size))| (n.saturating_sub(**size), **size))
                .map_or(busy.len(), |(i, _)| i);
            if slot == sizes.len() {
                sizes.push(n);
                busy.push(true);
            } else {
                let sz = sizes.get_mut(slot).ok_or_else(|| bad("missing buffer"))?;
                *sz = (*sz).max(n);
                *busy.get_mut(slot).ok_or_else(|| bad("missing buffer"))? = true;
            }
            tensors.push(Tensor { slot, side, channels });
            // Release sources only after the output has its own buffer (also handles reading the same tensor twice).
            for id in std::iter::once(src).chain(src2) {
                if last.get(id) == Some(&(i + 1)) {
                    let t = tensors.get(id).ok_or_else(|| bad("missing tensor"))?;
                    *busy.get_mut(t.slot).ok_or_else(|| bad("missing buffer"))? = false;
                }
            }
            let (k, m, positions) = match op {
                Op::Conv(c) => (c.cin * c.k * c.k, c.cout, a.side * a.side),
                Op::ConvT2(c) => (c.cin, c.cout * 4, a.side * a.side),
                _ => (0, 0, 0),
            };
            lower = lower.max(k.saturating_mul(BLOCK.min(positions)));
            product = product.max(m.saturating_mul(BLOCK.min(positions)));
            let packed = if let Op::ConvT2(c) = op {
                let mut w = zeros(c.weight.len())?;
                // [cin][cout][dy][dx] -> [cout*4][cin], so an up-convolution is one GEMM and a shuffle.
                for (ci, row) in c.weight.chunks_exact(c.cout * 4).enumerate() {
                    for (co, &v) in row.iter().enumerate() {
                        *w.get_mut(co * c.cin + ci).ok_or_else(|| bad("invalid transposed weights"))? = v;
                    }
                }
                Some(w)
            } else {
                None
            };
            up_weights.push(packed);
        }
        let total = sizes
            .iter()
            .try_fold(lower.saturating_add(product), |n, &s| n.checked_add(s))
            .filter(|&n| n <= MAX_VALUES)
            .ok_or_else(|| bad("the tile needs too much working memory"))?;
        let _ = total;
        Ok(Self { plan: Arc::new(Plan { net: net.clone(), tensors, sizes, lower, product, up_weights }), idle: Arc::new(Mutex::new(Vec::new())) })
    }
    /// Immutable data for the GPU runner, without re-reading the model.
    pub fn net(&self) -> &Net {
        &self.plan.net
    }
    pub fn bytes_per_tile(&self) -> usize {
        (self.plan.sizes.iter().sum::<usize>() + self.plan.lower + self.plan.product) * 4
    }
    fn workspace(&self) -> Result<Workspace> {
        if let Some(w) = self.idle.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop() {
            return Ok(w);
        }
        Ok(Workspace {
            buffers: self.plan.sizes.iter().map(|&n| zeros(n)).collect::<Result<_>>()?,
            lower: zeros(self.plan.lower)?,
            product: zeros(self.plan.product)?,
        })
    }
    fn execute(&self, input: &[f32], w: &mut Workspace) -> Result<Vec<f32>> {
        let first = self.plan.tensors.first().ok_or_else(|| bad("no input"))?;
        let want = count(first.channels, first.side)?;
        w.buffers.first_mut().and_then(|b| b.get_mut(..want)).ok_or_else(|| bad("missing input buffer"))?.copy_from_slice(input);
        let profile = dac_brand::env_is_set("PROFILE");
        for (i, op) in self.plan.net.ops().iter().enumerate() {
            let started = web_time::Instant::now();
            let out = *self.plan.tensors.get(i + 1).ok_or_else(|| bad("missing output tensor"))?;
            let (before, rest) = w.buffers.split_at_mut_checked(out.slot).ok_or_else(|| bad("missing output buffer"))?;
            let (dest, after) = rest.split_first_mut().ok_or_else(|| bad("missing output buffer"))?;
            let get = |id: usize| -> Result<(&[f32], Tensor)> {
                let t = *self.plan.tensors.get(id).ok_or_else(|| bad("missing source tensor"))?;
                let data = if t.slot < out.slot {
                    before.get(t.slot)
                } else {
                    after.get(t.slot.checked_sub(out.slot + 1).ok_or_else(|| bad("aliased buffers"))?)
                }
                .and_then(|b| b.get(..count(t.channels, t.side).ok()?))
                .ok_or_else(|| bad("missing source buffer"))?;
                Ok((data, t))
            };
            let (src, src2) = sources(op);
            let (a, shape) = get(src)?;
            let b = src2.map(get).transpose()?.map(|v| v.0);
            let dest = dest.get_mut(..count(out.channels, out.side)?).ok_or_else(|| bad("short output buffer"))?;
            match op {
                Op::Conv(c) => conv(c, a, b, shape.side, dest, &mut w.lower, &mut w.product)?,
                Op::ConvT2(c) => up(
                    c,
                    a,
                    shape.side,
                    dest,
                    self.plan.up_weights.get(i).and_then(Option::as_deref).ok_or_else(|| bad("missing up weights"))?,
                    &mut w.lower,
                    &mut w.product,
                )?,
                Op::MaxPool2 { .. } => pool(a, shape.side, dest)?,
                Op::DepthToSpace2 { .. } => shuffle(a, shape.side, dest)?,
            }
            if dest.iter().any(|v| !v.is_finite()) {
                return Err(bad("the model produced non-finite values"));
            }
            if profile {
                eprintln!("[denoise-cpu] layer {i}: {}x{}x{} {:.2} ms", out.channels, out.side, out.side, started.elapsed().as_secs_f64() * 1e3);
            }
        }
        let t = self.plan.tensors.last().ok_or_else(|| bad("no output"))?;
        let data = w.buffers.get(t.slot).and_then(|b| b.get(..count(t.channels, t.side).ok()?)).ok_or_else(|| bad("missing result"))?;
        let mut out = zeros(data.len())?;
        out.copy_from_slice(data);
        Ok(out)
    }
}
impl TileRunner for NetRunner {
    fn run(&self, input: &[f32]) -> std::result::Result<Vec<f32>, Error> {
        let first = self.plan.tensors.first().ok_or_else(|| Error::Runtime("missing input shape".into()))?;
        let want = count(first.channels, first.side).map_err(|e| Error::Runtime(e.to_string()))?;
        if input.len() != want || input.iter().any(|v| !v.is_finite()) {
            return Err(Error::Input("the tile has the wrong size or non-finite values".into()));
        }
        let mut w = self.workspace().map_err(|e| Error::Runtime(e.to_string()))?;
        let result = self.execute(input, &mut w).map_err(|e| Error::Runtime(e.to_string()));
        let mut idle = self.idle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if idle.len() < MAX_IDLE {
            idle.push(w);
        }
        result
    }
}
fn activate(x: f32, bias: f32, alpha: Option<f32>) -> f32 {
    let x = x + bias;
    if x < 0.0 { x * alpha.unwrap_or(1.0) } else { x }
}

/// All views are over exact, checked slices. The matrix library's assertions therefore never see file-derived sizes.
fn multiply(w: &[f32], x: &[f32], y: &mut [f32], m: usize, k: usize, n: usize) -> Result<()> {
    if m.checked_mul(k) != Some(w.len()) || k.checked_mul(n) != Some(x.len()) || m.checked_mul(n) != Some(y.len()) || m == 0 || k == 0 || n == 0 {
        return Err(bad("invalid matrix dimensions"));
    }
    faer::mul::matmul(
        mat::from_row_major_slice_mut::<f32>(y, m, n),
        mat::from_row_major_slice::<f32>(w, m, k),
        mat::from_row_major_slice::<f32>(x, k, n),
        None,
        1.0,
        Parallelism::None,
    );
    Ok(())
}
fn lower(a: &[f32], b: Option<&[f32]>, side: usize, k: usize, start: usize, n: usize, dest: &mut [f32]) -> Result<()> {
    let pad = k / 2;
    let pixels = side * side;
    // Whole row segments are copied, so bounds checks and edge branches stay out of the multiply loop.
    for (ci, plane) in a.chunks_exact(pixels).chain(b.into_iter().flat_map(|b| b.chunks_exact(pixels))).enumerate() {
        for ky in 0..k {
            for kx in 0..k {
                let row = (ci * k * k + ky * k + kx) * n;
                let out = dest.get_mut(row..row + n).ok_or_else(|| bad("short im2col buffer"))?;
                let mut at = 0;
                while at < n {
                    let p = start + at;
                    let (y, x) = (p / side, p % side);
                    let len = (side - x).min(n - at);
                    if let Some(yy) = y.checked_add(ky).and_then(|v| v.checked_sub(pad)).filter(|&v| v < side) {
                        let lo = x.max(pad.saturating_sub(kx));
                        let hi = (x + len).min((side + pad).saturating_sub(kx));
                        if lo < hi {
                            let from = yy * side + lo + kx - pad;
                            let to = at + lo - x;
                            out.get_mut(at..to).ok_or_else(|| bad("short im2col padding"))?.fill(0.0);
                            out.get_mut(to..to + hi - lo)
                                .ok_or_else(|| bad("short im2col row"))?
                                .copy_from_slice(plane.get(from..from + hi - lo).ok_or_else(|| bad("short source row"))?);
                            out.get_mut(to + hi - lo..at + len).ok_or_else(|| bad("short im2col padding"))?.fill(0.0);
                        } else {
                            out.get_mut(at..at + len).ok_or_else(|| bad("short im2col row"))?.fill(0.0);
                        }
                    } else {
                        out.get_mut(at..at + len).ok_or_else(|| bad("short im2col row"))?.fill(0.0);
                    }
                    at += len;
                }
            }
        }
    }
    Ok(())
}
fn conv(c: &crate::net::Conv, a: &[f32], b: Option<&[f32]>, side: usize, dest: &mut [f32], scratch: &mut [f32], product: &mut [f32]) -> Result<()> {
    let pixels = side * side;
    let k = c.cin * c.k * c.k;
    for start in (0..pixels).step_by(BLOCK) {
        let n = BLOCK.min(pixels - start);
        let x = scratch.get_mut(..k * n).ok_or_else(|| bad("short im2col storage"))?;
        lower(a, b, side, c.k, start, n, x)?;
        let y = product.get_mut(..c.cout * n).ok_or_else(|| bad("short product storage"))?;
        multiply(&c.weight, x, y, c.cout, k, n)?;
        for ((row, &bias), out) in y.chunks_exact(n).zip(&c.bias).zip(dest.chunks_exact_mut(pixels)) {
            let to = out.get_mut(start..start + n).ok_or_else(|| bad("short convolution output"))?;
            for (d, &v) in to.iter_mut().zip(row) {
                *d = activate(v, bias, c.leaky);
            }
        }
    }
    Ok(())
}
fn up(c: &crate::net::ConvT2, a: &[f32], side: usize, dest: &mut [f32], weights: &[f32], scratch: &mut [f32], product: &mut [f32]) -> Result<()> {
    let pixels = side * side;
    let out_side = 2 * side;
    for start in (0..pixels).step_by(BLOCK) {
        let n = BLOCK.min(pixels - start);
        let x = scratch.get_mut(..c.cin * n).ok_or_else(|| bad("short up input storage"))?;
        for (to, from) in x.chunks_exact_mut(n).zip(a.chunks_exact(pixels)) {
            to.copy_from_slice(from.get(start..start + n).ok_or_else(|| bad("short up input"))?);
        }
        let y = product.get_mut(..c.cout * 4 * n).ok_or_else(|| bad("short up product storage"))?;
        multiply(weights, x, y, c.cout * 4, c.cin, n)?;
        for (co, (&bias, out)) in c.bias.iter().zip(dest.chunks_exact_mut(4 * pixels)).enumerate() {
            for d in 0..4 {
                let row = y.get((4 * co + d) * n..(4 * co + d + 1) * n).ok_or_else(|| bad("short up product"))?;
                for (j, &v) in row.iter().enumerate() {
                    let p = start + j;
                    let at = (2 * (p / side) + d / 2) * out_side + 2 * (p % side) + d % 2;
                    *out.get_mut(at).ok_or_else(|| bad("short up output"))? = activate(v, bias, c.leaky);
                }
            }
        }
    }
    Ok(())
}
fn pool(a: &[f32], side: usize, dest: &mut [f32]) -> Result<()> {
    let s = side / 2;
    for (input, out) in a.chunks_exact(side * side).zip(dest.chunks_exact_mut(s * s)) {
        for (y, row) in out.chunks_exact_mut(s).enumerate() {
            let lo = input.get(2 * y * side..(2 * y + 1) * side).ok_or_else(|| bad("short pool input"))?;
            let hi = input.get((2 * y + 1) * side..(2 * y + 2) * side).ok_or_else(|| bad("short pool input"))?;
            for ((d, lo), hi) in row.iter_mut().zip(lo.as_chunks::<2>().0).zip(hi.as_chunks::<2>().0) {
                let [a, b] = *lo;
                let [c, e] = *hi;
                *d = a.max(b).max(c).max(e);
            }
        }
    }
    Ok(())
}
fn shuffle(a: &[f32], side: usize, dest: &mut [f32]) -> Result<()> {
    let pixels = side * side;
    for (input, out) in a.chunks_exact(4 * pixels).zip(dest.chunks_exact_mut(4 * pixels)) {
        for d in 0..4 {
            let plane = input.get(d * pixels..(d + 1) * pixels).ok_or_else(|| bad("short shuffle input"))?;
            for (y, row) in plane.chunks_exact(side).enumerate() {
                let at = (2 * y + d / 2) * 2 * side;
                let to = out.get_mut(at..at + 2 * side).ok_or_else(|| bad("short shuffle output"))?;
                for (to, &v) in to.as_chunks_mut::<2>().0.iter_mut().zip(row) {
                    if let Some(t) = to.get_mut(d % 2) {
                        *t = v;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
