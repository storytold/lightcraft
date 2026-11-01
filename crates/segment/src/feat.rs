//! Plain-`Vec<f32>` feature maps (height × width × channels, channels last) and the CPU
//! operations the BiRefNet port runs on them: dense layers (candle matrix products), layer
//! norm, GELU, bilinear resizing (`align_corners=True`), channel concatenation.
//!
//! Channels-last rows make every per-pixel linear map one contiguous matrix product, and keep
//! the gathers of the convolutions ([`crate::deform`]) cache-friendly.
//!
//! Modified work (MIT): the semantics follow the BiRefNet code (`models/birefnet.py`,
//! Copyright (c) 2024 ZhengPeng, MIT licence, <https://github.com/ZhengPeng7/BiRefNet>) and
//! PyTorch's `F.interpolate`; reimplemented in Rust by the LightCraft contributors in 2026.
//! See NOTICE.

use candle_core::{Device, Tensor};
use rayon::prelude::*;

use crate::{Error, Result};

/// A feature map: `h × w` pixels of `c` channels, row-major, channels last.
#[derive(Clone, Debug)]
pub struct Map {
    pub h: usize,
    pub w: usize,
    pub c: usize,
    pub data: Vec<f32>,
}

impl Map {
    pub fn new(h: usize, w: usize, c: usize, data: Vec<f32>) -> Result<Self> {
        let n = h.checked_mul(w).and_then(|p| p.checked_mul(c)).ok_or_else(|| Error::Model("feature map too large".into()))?;
        if data.len() != n {
            return Err(Error::Model(format!("feature map of {} values for {h} × {w} × {c}", data.len())));
        }
        Ok(Self { h, w, c, data })
    }

    pub fn pixels(&self) -> usize {
        self.h * self.w
    }

    /// From a channel-first (`[c, h, w]`) buffer.
    pub fn from_chw(chw: &[f32], c: usize, h: usize, w: usize) -> Result<Self> {
        let hw = h.checked_mul(w).ok_or_else(|| Error::Model("feature map too large".into()))?;
        if Some(chw.len()) != hw.checked_mul(c) {
            return Err(Error::Model(format!("feature map of {} values for {c} × {h} × {w}", chw.len())));
        }
        let mut data = vec![0f32; chw.len()];
        data.par_chunks_mut(c.max(1)).enumerate().for_each(|(p, px)| {
            for (ch, v) in px.iter_mut().enumerate() {
                *v = chw.get(ch * hw + p).copied().unwrap_or(0.0);
            }
        });
        Self::new(h, w, c, data)
    }

    /// As a channel-first (`[c, h, w]`) buffer.
    pub fn to_chw(&self) -> Vec<f32> {
        let hw = self.pixels();
        let mut out = vec![0f32; self.data.len()];
        out.par_chunks_mut(hw.max(1)).enumerate().for_each(|(ch, plane)| {
            for (p, v) in plane.iter_mut().enumerate() {
                *v = self.data.get(p * self.c + ch).copied().unwrap_or(0.0);
            }
        });
        out
    }
}

/// An activation applied after a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    Relu,
    Gelu,
}

/// Applies `act` and the per-column bias to `rows × cols` values in place, in parallel.
pub fn bias_act(x: &mut [f32], cols: usize, bias: Option<&[f32]>, act: Act) {
    if cols == 0 || (bias.is_none() && act == Act::None) {
        return;
    }
    x.par_chunks_mut(cols * 64usize.div_ceil(cols).max(1)).for_each(|rows| {
        for row in rows.chunks_mut(cols) {
            if let Some(b) = bias {
                row.iter_mut().zip(b).for_each(|(v, b)| *v += b);
            }
            match act {
                Act::None => {}
                Act::Relu => row.iter_mut().for_each(|v| *v = v.max(0.0)),
                Act::Gelu => row.iter_mut().for_each(|v| *v = gelu(*v)),
            }
        }
    });
}

/// `nn.GELU()` (the exact, erf-based form; erf to ~1e-7, Abramowitz & Stegun 7.1.26).
pub fn gelu(x: f32) -> f32 {
    let z = (x * std::f32::consts::FRAC_1_SQRT_2).abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_6 + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152_1 + t * 1.061_405_4))));
    let erf = 1.0 - poly * (-z * z).exp();
    0.5 * x * (1.0 + if x < 0.0 { -erf } else { erf })
}

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// A dense layer `x · W + b` over rows (`W` stored `[in, out]`).
pub struct Dense {
    w: Tensor,
    b: Option<Vec<f32>>,
    pub inp: usize,
    pub out: usize,
}

impl Dense {
    /// From a row-major `[out, in]` matrix (PyTorch's `nn.Linear` and 1 × 1 conv layout).
    pub fn from_out_in(w: &[f32], out: usize, inp: usize, b: Option<Vec<f32>>, dev: &Device) -> Result<Self> {
        if Some(w.len()) != out.checked_mul(inp) || b.as_ref().is_some_and(|b| b.len() != out) {
            return Err(Error::Model(format!("dense layer: {} weights for {out} × {inp}", w.len())));
        }
        let mut t = vec![0f32; w.len()];
        for o in 0..out {
            for i in 0..inp {
                if let (Some(d), Some(s)) = (t.get_mut(i * out + o), w.get(o * inp + i)) {
                    *d = *s;
                }
            }
        }
        Self::from_in_out(t, inp, out, b, dev)
    }

    /// From a row-major `[in, out]` matrix.
    pub fn from_in_out(w: Vec<f32>, inp: usize, out: usize, b: Option<Vec<f32>>, dev: &Device) -> Result<Self> {
        if Some(w.len()) != out.checked_mul(inp) || b.as_ref().is_some_and(|b| b.len() != out) {
            return Err(Error::Model(format!("dense layer: {} weights for {inp} × {out}", w.len())));
        }
        Ok(Self { w: Tensor::from_vec(w, (inp, out), dev)?, b, inp, out })
    }

    /// `x` (`rows × in`, row-major) → `rows × out`, with the bias and `act` applied.
    pub fn forward(&self, x: &[f32], act: Act) -> Result<Vec<f32>> {
        let mut y = matmul(x, self.inp, &self.w)?;
        bias_act(&mut y, self.out, self.b.as_deref(), act);
        Ok(y)
    }

    pub fn bias(&self) -> Option<&[f32]> {
        self.b.as_deref()
    }

    /// The layer over the channels of every pixel of `x`.
    pub fn map(&self, x: &Map, act: Act) -> Result<Map> {
        if x.c != self.inp {
            return Err(Error::Model(format!("dense layer of {} inputs on {} channels", self.inp, x.c)));
        }
        Map::new(x.h, x.w, self.out, self.forward(&x.data, act)?)
    }
}

/// `a` (`rows × k`, row-major) times `w` (`[k, n]`), on `w`'s device.
pub fn matmul(a: &[f32], k: usize, w: &Tensor) -> Result<Vec<f32>> {
    if k == 0 || !a.len().is_multiple_of(k) {
        return Err(Error::Model(format!("matrix of {} values with rows of {k}", a.len())));
    }
    let rows = a.len() / k;
    if rows == 0 {
        return Ok(Vec::new());
    }
    // large products go to the accelerator (the GPU) when one is installed; weights are kept there
    if w.device().is_cpu()
        && let [wk, n] = w.dims()
        && *wk == k
    {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        w.id().hash(&mut h);
        let weights = || w.flatten_all().and_then(|t| t.to_vec1::<f32>()).ok();
        if let Some(c) = crate::accel::gemm(a, rows, k, *n, h.finish(), &weights) {
            return Ok(c);
        }
    }
    let t = Tensor::from_slice(a, (rows, k), w.device())?;
    Ok(t.matmul(w)?.flatten_all()?.to_vec1::<f32>()?)
}

/// `nn.LayerNorm` over rows of `c` values, in place.
pub fn layer_norm(x: &mut [f32], gamma: &[f32], beta: &[f32], eps: f32) {
    let c = gamma.len();
    if c == 0 {
        return;
    }
    x.par_chunks_mut(c * 16).for_each(|rows| {
        for row in rows.chunks_mut(c) {
            let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / c as f64;
            let var = row.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum::<f64>() / c as f64;
            let inv = 1.0 / (var + f64::from(eps)).sqrt();
            for ((v, g), b) in row.iter_mut().zip(gamma).zip(beta) {
                *v = ((f64::from(*v) - mean) * inv) as f32 * g + b;
            }
        }
    });
}

/// Source index and weight pairs of `F.interpolate(mode='bilinear', align_corners=True)` along
/// one axis.
fn ac_taps(src: usize, dst: usize) -> Vec<(usize, usize, f32)> {
    let scale = if dst > 1 { (src as f32 - 1.0) / (dst as f32 - 1.0) } else { 0.0 };
    (0..dst)
        .map(|o| {
            let s = scale * o as f32;
            let i0 = (s as usize).min(src.saturating_sub(1));
            let i1 = if i0 + 1 < src { i0 + 1 } else { i0 };
            (i0, i1, s - i0 as f32)
        })
        .collect()
}

/// `F.interpolate(x, size=(h, w), mode='bilinear', align_corners=True)`.
pub fn resize(x: &Map, h: usize, w: usize) -> Result<Map> {
    if x.h == h && x.w == w {
        return Ok(x.clone());
    }
    if x.h == 0 || x.w == 0 {
        return Err(Error::Model("resizing an empty feature map".into()));
    }
    let c = x.c;
    let n = h.checked_mul(w).and_then(|p| p.checked_mul(c)).ok_or_else(|| Error::Model("feature map too large".into()))?;
    let (ty, tx) = (ac_taps(x.h, h), ac_taps(x.w, w));
    let mut out = vec![0f32; n];
    out.par_chunks_mut((w * c).max(1)).zip(ty.par_iter()).for_each(|(row, &(y0, y1, ly))| {
        let r0 = x.data.get(y0 * x.w * c..(y0 + 1) * x.w * c).unwrap_or(&[]);
        let r1 = x.data.get(y1 * x.w * c..(y1 + 1) * x.w * c).unwrap_or(&[]);
        for (px, &(x0, x1, lx)) in row.chunks_mut(c.max(1)).zip(&tx) {
            let (a, b) = (r0.get(x0 * c..x0 * c + c).unwrap_or(&[]), r0.get(x1 * c..x1 * c + c).unwrap_or(&[]));
            let (d, e) = (r1.get(x0 * c..x0 * c + c).unwrap_or(&[]), r1.get(x1 * c..x1 * c + c).unwrap_or(&[]));
            for (i, v) in px.iter_mut().enumerate() {
                let g = |s: &[f32]| s.get(i).copied().unwrap_or(0.0);
                let top = (1.0 - lx) * g(a) + lx * g(b);
                let bot = (1.0 - lx) * g(d) + lx * g(e);
                *v = (1.0 - ly) * top + ly * bot;
            }
        }
    });
    Map::new(h, w, c, out)
}

/// Concatenation along channels (`torch.cat(…, dim=1)`); all maps share `h × w`.
pub fn cat(maps: &[&Map]) -> Result<Map> {
    let Some(first) = maps.first() else { return Err(Error::Model("nothing to concatenate".into())) };
    let (h, w) = (first.h, first.w);
    if maps.iter().any(|m| m.h != h || m.w != w) {
        return Err(Error::Model("concatenating feature maps of different sizes".into()));
    }
    let c: usize = maps.iter().map(|m| m.c).sum();
    let mut out = vec![0f32; h * w * c];
    out.par_chunks_mut(c.max(1)).enumerate().for_each(|(p, px)| {
        let mut at = 0;
        for m in maps {
            if let (Some(d), Some(s)) = (px.get_mut(at..at + m.c), m.data.get(p * m.c..(p + 1) * m.c)) {
                d.copy_from_slice(s);
            }
            at += m.c;
        }
    });
    Map::new(h, w, c, out)
}

/// Named tensors of a checkpoint as `f32` vectors.
pub struct Params<'a> {
    pub weights: &'a crate::weights::Weights,
    pub dev: Device,
}

impl Params<'_> {
    pub fn has(&self, name: &str) -> bool {
        self.weights.contains(name)
    }

    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.weights.shape(name)
    }

    /// Tensor `name`: values and shape.
    pub fn get(&self, name: &str) -> Result<(Vec<f32>, Vec<usize>)> {
        let t = self.weights.read(name)?;
        let shape = t.dims().to_vec();
        Ok((t.flatten_all()?.to_vec1::<f32>()?, shape))
    }

    /// Tensor `name`, which must hold `n` values.
    pub fn vec(&self, name: &str, n: usize) -> Result<Vec<f32>> {
        let (v, _) = self.get(name)?;
        if v.len() != n {
            return Err(Error::Model(format!("tensor `{name}` has {} values, expected {n}", v.len())));
        }
        Ok(v)
    }

    /// `nn.Linear` `name` (`weight` `[out, in]`, optional `bias`).
    pub fn linear(&self, name: &str) -> Result<Dense> {
        let (w, shape) = self.get(&format!("{name}.weight"))?;
        let &[out, inp] = shape.as_slice() else { return Err(Error::Model(format!("{name}: weight of shape {shape:?}"))) };
        let b = if self.has(&format!("{name}.bias")) { Some(self.vec(&format!("{name}.bias"), out)?) } else { None };
        Dense::from_out_in(&w, out, inp, b, &self.dev)
    }

    /// `nn.LayerNorm` `name` of `n` channels: (gamma, beta).
    pub fn norm(&self, name: &str, n: usize) -> Result<(Vec<f32>, Vec<f32>)> {
        Ok((self.vec(&format!("{name}.weight"), n)?, self.vec(&format!("{name}.bias"), n)?))
    }

    /// An eval-mode `nn.BatchNorm2d` `name` (eps 1e-5) as a per-channel `(scale, shift)`.
    pub fn batch_norm(&self, name: &str, n: usize) -> Result<(Vec<f32>, Vec<f32>)> {
        let g = self.vec(&format!("{name}.weight"), n)?;
        let b = self.vec(&format!("{name}.bias"), n)?;
        let m = self.vec(&format!("{name}.running_mean"), n)?;
        let v = self.vec(&format!("{name}.running_var"), n)?;
        let scale: Vec<f32> = g.iter().zip(&v).map(|(g, v)| g / (v + 1e-5).sqrt()).collect();
        let shift = b.iter().zip(&m).zip(&scale).map(|((b, m), s)| b - m * s).collect();
        Ok((scale, shift))
    }

    /// `nn.Conv2d` `name` with `pad`, optionally followed by the BatchNorm `bn`.
    pub fn conv(&self, name: &str, pad: usize, bn: Option<&str>) -> Result<crate::deform::Conv> {
        let (w, shape) = self.get(&format!("{name}.weight"))?;
        let cout = shape.first().copied().unwrap_or(0);
        let b = if self.has(&format!("{name}.bias")) { Some(self.vec(&format!("{name}.bias"), cout)?) } else { None };
        let bn = match bn {
            Some(n) => Some(self.batch_norm(n, cout)?),
            None => None,
        };
        crate::deform::Conv::new(&w, &shape, b.as_deref(), bn.as_ref().map(|(s, t)| (s.as_slice(), t.as_slice())), pad, &self.dev)
    }
}

/// Per-stage wall-clock timings on stderr when `LIGHTCRAFT_PROFILE` is set.
pub struct Profile {
    on: bool,
    t: std::time::Instant,
}

impl Profile {
    pub fn new() -> Self {
        Self { on: std::env::var_os("LIGHTCRAFT_PROFILE").is_some(), t: std::time::Instant::now() }
    }

    pub fn step(&mut self, what: &str) {
        if self.on {
            eprintln!("birefnet {what}: {:.1} ms", self.t.elapsed().as_secs_f64() * 1e3);
            self.t = std::time::Instant::now();
        }
    }
}

/// `a += b` elementwise, in parallel.
pub fn add_assign(a: &mut [f32], b: &[f32]) {
    a.par_chunks_mut(4096).zip(b.par_chunks(4096)).for_each(|(x, y)| x.iter_mut().zip(y).for_each(|(x, y)| *x += y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_matches_align_corners() {
        // 1-D ramp 0, 1, 2 → 5 samples: 0, 0.5, 1, 1.5, 2
        let x = Map::new(1, 3, 1, vec![0.0, 1.0, 2.0]).unwrap();
        let y = resize(&x, 1, 5).unwrap();
        for (a, b) in y.data.iter().zip([0.0, 0.5, 1.0, 1.5, 2.0]) {
            assert!((a - b).abs() < 1e-6);
        }
        // downscale 5 → 3 picks samples 0, 2, 4
        let x = Map::new(5, 1, 2, (0..10).map(|v| v as f32).collect()).unwrap();
        let y = resize(&x, 3, 1).unwrap();
        assert_eq!(y.data, vec![0.0, 1.0, 4.0, 5.0, 8.0, 9.0]);
    }

    #[test]
    fn chw_round_trip_and_cat() {
        let chw: Vec<f32> = (0..24).map(|v| v as f32).collect();
        let m = Map::from_chw(&chw, 2, 3, 4).unwrap();
        assert_eq!(m.data[..4], [0.0, 12.0, 1.0, 13.0]);
        assert_eq!(m.to_chw(), chw);
        let c = cat(&[&m, &m]).unwrap();
        assert_eq!(c.c, 4);
        assert_eq!(c.data[..4], [0.0, 12.0, 0.0, 12.0]);
        assert!(Map::new(2, 2, 2, vec![0.0; 3]).is_err());
    }

    #[test]
    fn gelu_and_layer_norm() {
        assert!((gelu(1.0) - 0.841_344_7).abs() < 1e-6);
        assert!(gelu(-10.0).abs() < 1e-6);
        let mut x = vec![1.0, 2.0, 3.0, 4.0];
        layer_norm(&mut x, &[1.0, 1.0], &[0.0, 0.5], 1e-5);
        assert!((x[0] + 1.0).abs() < 1e-3 && (x[1] - 1.5).abs() < 1e-3);
    }
}
