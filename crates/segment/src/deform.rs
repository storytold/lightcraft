//! 2-D convolutions over channels-last [`Map`]s, regular and deformable
//! (`torchvision.ops.deform_conv2d` with a modulation mask, DCNv2), as a chunked gather
//! (im2col) followed by a matrix product. Stride 1, dilation 1, one group — what BiRefNet uses.
//!
//! The gather writes, for every output pixel, the `k × k × c_in` input values its kernel sees
//! (bilinearly sampled at the offset positions for the deformable form, zero outside the
//! image, times the mask), in kernel-position-major order; the weights are reordered to match
//! once at load time.
//!
//! Modified work (MIT): the semantics follow `models/modules/deform_conv.py` of BiRefNet
//! (Copyright (c) 2024 ZhengPeng, MIT licence, <https://github.com/ZhengPeng7/BiRefNet>) and
//! the documented behaviour of `torchvision.ops.deform_conv2d` (BSD-3-Clause); reimplemented in
//! Rust by the LightCraft contributors in 2026. See NOTICE.

use candle_core::{Device, Tensor};
use rayon::prelude::*;

use crate::feat::{Act, Map, bias_act, matmul};
use crate::{Error, Result};

/// Largest gathered block (values) multiplied at once (64 MB).
const CHUNK: usize = 16 << 20;
/// Largest per-tap product map of the shift-and-add form (256 MB).
const TAPS_MAX: usize = 64 << 20;

/// A square convolution, stride 1, with `pad` zero padding.
pub struct Conv {
    pub k: usize,
    pub pad: usize,
    pub cin: usize,
    pub cout: usize,
    /// `[k·k·cin, cout]`, rows ordered (ky, kx, c) (im2col form).
    w: Tensor,
    /// `[cin, k·k·cout]`, columns ordered (ky, kx, o): the shift-and-add form, used for
    /// regular convolutions with fewer outputs than inputs (one product, then `k²` shifted sums).
    taps: Option<Tensor>,
    b: Option<Vec<f32>>,
}

impl Conv {
    /// From PyTorch's `[cout, cin, k, k]` weights, an optional bias and an optional per-output
    /// affine (`scale`, `shift`, a folded eval-mode BatchNorm: `y·scale + shift`).
    pub fn new(w: &[f32], shape: &[usize], b: Option<&[f32]>, bn: Option<(&[f32], &[f32])>, pad: usize, dev: &Device) -> Result<Self> {
        let &[cout, cin, k, k2] = shape else { return Err(Error::Model(format!("conv weights of shape {shape:?}"))) };
        let kk = k.checked_mul(k).and_then(|kk| kk.checked_mul(cin)).ok_or_else(|| Error::Model("conv too large".into()))?;
        if k != k2 || k == 0 || Some(w.len()) != kk.checked_mul(cout) || b.is_some_and(|b| b.len() != cout) {
            return Err(Error::Model(format!("conv weights of shape {shape:?} ({} values)", w.len())));
        }
        if let Some((s, t)) = bn
            && (s.len() != cout || t.len() != cout)
        {
            return Err(Error::Model("batch norm size does not match its conv".into()));
        }
        let scale = |o: usize| bn.and_then(|(s, _)| s.get(o).copied()).unwrap_or(1.0);
        let mut t = vec![0f32; w.len()];
        for o in 0..cout {
            for c in 0..cin {
                for ky in 0..k {
                    for kx in 0..k {
                        let src = ((o * cin + c) * k + ky) * k + kx;
                        let row = (ky * k + kx) * cin + c;
                        if let (Some(d), Some(s)) = (t.get_mut(row * cout + o), w.get(src)) {
                            *d = s * scale(o);
                        }
                    }
                }
            }
        }
        let bias = match (b, bn) {
            (None, None) => None,
            _ => Some(
                (0..cout)
                    .map(|o| {
                        let b = b.and_then(|b| b.get(o).copied()).unwrap_or(0.0);
                        let shift = bn.and_then(|(_, t)| t.get(o).copied()).unwrap_or(0.0);
                        b * scale(o) + shift
                    })
                    .collect(),
            ),
        };
        let taps = if cout < cin && k > 1 {
            let n = k * k * cout;
            let mut m = vec![0f32; cin * n];
            for (row, wr) in t.chunks(cout).enumerate() {
                let (tap, c) = (row / cin, row % cin);
                if let Some(d) = m.get_mut(c * n + tap * cout..c * n + (tap + 1) * cout) {
                    d.copy_from_slice(wr);
                }
            }
            Some(Tensor::from_vec(m, (cin, n), dev)?)
        } else {
            None
        };
        Ok(Self { k, pad, cin, cout, w: Tensor::from_vec(t, (kk, cout), dev)?, taps, b: bias })
    }

    fn out_size(&self, x: &Map) -> Result<(usize, usize)> {
        if x.c != self.cin {
            return Err(Error::Model(format!("conv of {} input channels on {} channels", self.cin, x.c)));
        }
        let oh = (x.h + 2 * self.pad).checked_sub(self.k - 1);
        let ow = (x.w + 2 * self.pad).checked_sub(self.k - 1);
        match (oh, ow) {
            (Some(h), Some(w)) if h > 0 && w > 0 => Ok((h, w)),
            _ => Err(Error::Model("conv input smaller than its kernel".into())),
        }
    }

    /// The convolution of `x`, then the bias and `act`.
    pub fn forward(&self, x: &Map, act: Act) -> Result<Map> {
        self.run(x, None, act)
    }

    /// The deformable convolution of `x`: `om` holds, per output pixel, `2·k²` offsets (dy, dx
    /// per kernel position, row-major) followed by `k²` modulation factors (already through
    /// the reference's `2·sigmoid`).
    pub fn forward_deform(&self, x: &Map, om: &Map, act: Act) -> Result<Map> {
        self.run(x, Some(om), act)
    }

    fn run(&self, x: &Map, om: Option<&Map>, act: Act) -> Result<Map> {
        let (oh, ow) = self.out_size(x)?;
        let kk = self.k * self.k;
        if let Some(om) = om
            && (om.h != oh || om.w != ow || om.c != 3 * kk)
        {
            return Err(Error::Model("deformable conv offsets do not match its output".into()));
        }
        let p = oh * ow;
        let cols = kk * self.cin;
        let mut out = vec![0f32; p.checked_mul(self.cout).ok_or_else(|| Error::Model("conv output too large".into()))?];
        if om.is_none() && self.k == 1 && self.pad == 0 {
            // a 1 × 1 conv is one product over the pixels
            out = matmul(&x.data, cols, &self.w)?;
        } else if let Some(taps) = self.taps.as_ref().filter(|_| om.is_none() && x.pixels().saturating_mul(kk * self.cout) <= TAPS_MAX) {
            // every input pixel times every tap's weights, then each output pixel sums its
            // k² neighbours' products
            let y = matmul(&x.data, self.cin, taps)?;
            let (k, pad, cout, n) = (self.k, self.pad, self.cout, kk * self.cout);
            out.par_chunks_mut(cout.max(1)).enumerate().for_each(|(p, px)| {
                let (oy, ox) = (p / ow, p % ow);
                for ky in 0..k {
                    let Some(iy) = (oy + ky).checked_sub(pad).filter(|iy| *iy < x.h) else { continue };
                    for kx in 0..k {
                        let Some(ix) = (ox + kx).checked_sub(pad).filter(|ix| *ix < x.w) else { continue };
                        let at = (iy * x.w + ix) * n + (ky * k + kx) * cout;
                        if let Some(s) = y.get(at..at + cout) {
                            px.iter_mut().zip(s).for_each(|(a, b)| *a += b);
                        }
                    }
                }
            });
        } else {
            let rows = (CHUNK / cols.max(1)).clamp(1, p);
            let mut buf = vec![0f32; rows * cols];
            let mut start = 0;
            while start < p {
                let n = rows.min(p - start);
                let block = buf.get_mut(..n * cols).ok_or_else(|| Error::Model("conv gather".into()))?;
                match om {
                    None => self.gather(x, ow, start, block),
                    Some(om) => self.gather_deform(x, om, ow, start, block),
                }
                let y = matmul(block, cols, &self.w)?;
                if let Some(d) = out.get_mut(start * self.cout..(start + n) * self.cout) {
                    d.copy_from_slice(&y);
                }
                start += n;
            }
        }
        bias_act(&mut out, self.cout, self.b.as_deref(), act);
        Map::new(oh, ow, self.cout, out)
    }

    /// im2col rows `start..` of a regular convolution.
    fn gather(&self, x: &Map, ow: usize, start: usize, block: &mut [f32]) {
        let (c, k, pad) = (self.cin, self.k, self.pad);
        block.par_chunks_mut(k * k * c).enumerate().for_each(|(i, row)| {
            let (oy, ox) = ((start + i) / ow, (start + i) % ow);
            for ky in 0..k {
                for kx in 0..k {
                    let dst = row.get_mut((ky * k + kx) * c..(ky * k + kx + 1) * c).unwrap_or(&mut []);
                    let (iy, ix) = ((oy + ky).checked_sub(pad), (ox + kx).checked_sub(pad));
                    match (iy, ix) {
                        (Some(iy), Some(ix)) if iy < x.h && ix < x.w => {
                            let at = (iy * x.w + ix) * c;
                            match x.data.get(at..at + c) {
                                Some(s) => dst.copy_from_slice(s),
                                None => dst.fill(0.0),
                            }
                        }
                        _ => dst.fill(0.0),
                    }
                }
            }
        });
    }

    /// im2col rows `start..` of a deformable convolution (torchvision's bilinear sampling:
    /// zero beyond one pixel outside the image, missing corners count as zero).
    fn gather_deform(&self, x: &Map, om: &Map, ow: usize, start: usize, block: &mut [f32]) {
        let (c, k, pad) = (self.cin, self.k, self.pad as f32);
        let kk = k * k;
        let (h, w) = (x.h as f32, x.w as f32);
        block.par_chunks_mut(kk * c).enumerate().for_each(|(i, row)| {
            let p = start + i;
            let (oy, ox) = ((p / ow) as f32, (p % ow) as f32);
            let o = om.data.get(p * 3 * kk..(p + 1) * 3 * kk).unwrap_or(&[]);
            for ky in 0..k {
                for kx in 0..k {
                    let j = ky * k + kx;
                    let dst = row.get_mut(j * c..(j + 1) * c).unwrap_or(&mut []);
                    let dy = o.get(2 * j).copied().unwrap_or(0.0);
                    let dx = o.get(2 * j + 1).copied().unwrap_or(0.0);
                    let m = o.get(2 * kk + j).copied().unwrap_or(0.0);
                    let y = oy - pad + ky as f32 + dy;
                    let xx = ox - pad + kx as f32 + dx;
                    if !(y > -1.0 && y < h && xx > -1.0 && xx < w) {
                        dst.fill(0.0);
                        continue;
                    }
                    let (y0, x0) = (y.floor(), xx.floor());
                    let (ly, lx) = (y - y0, xx - x0);
                    let (hy, hx) = (1.0 - ly, 1.0 - lx);
                    let (y0, x0) = (y0 as i64, x0 as i64);
                    let corner = |cy: i64, cx: i64| -> &[f32] {
                        if cy < 0 || cx < 0 || cy >= x.h as i64 || cx >= x.w as i64 {
                            return &[];
                        }
                        let at = (cy as usize * x.w + cx as usize) * c;
                        x.data.get(at..at + c).unwrap_or(&[])
                    };
                    let taps =
                        [(corner(y0, x0), hy * hx), (corner(y0, x0 + 1), hy * lx), (corner(y0 + 1, x0), ly * hx), (corner(y0 + 1, x0 + 1), ly * lx)];
                    dst.fill(0.0);
                    for (s, wt) in taps {
                        if !s.is_empty() {
                            dst.iter_mut().zip(s).for_each(|(d, v)| *d += wt * v);
                        }
                    }
                    dst.iter_mut().for_each(|d| *d *= m);
                }
            }
        });
    }
}

/// `DeformableConv2d`: offset and modulator convolutions (merged into one), then the
/// modulated deformable convolution.
pub struct DeformConv {
    offsets: Conv,
    conv: Conv,
}

impl DeformConv {
    pub fn new(offsets: Conv, conv: Conv) -> Result<Self> {
        let kk = conv.k * conv.k;
        if offsets.cout != 3 * kk || offsets.k != conv.k || offsets.pad != conv.pad || offsets.cin != conv.cin {
            return Err(Error::Model("deformable conv: offset conv does not match".into()));
        }
        Ok(Self { offsets, conv })
    }

    pub fn forward(&self, x: &Map, act: Act) -> Result<Map> {
        let mut om = self.offsets.forward(x, Act::None)?;
        let (kk, c) = (self.conv.k * self.conv.k, om.c);
        om.data.par_chunks_mut(c.max(1)).for_each(|px| {
            if let Some(m) = px.get_mut(2 * kk..) {
                m.iter_mut().for_each(|v| *v = 2.0 * crate::feat::sigmoid(*v));
            }
        });
        self.conv.forward_deform(x, &om, act)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(n: usize, a: f32, b: f32, s: f32) -> Vec<f32> {
        (0..n).map(|i| (f64::from(s) * (i as f64 * f64::from(a) + f64::from(b)).sin()) as f32).collect()
    }

    /// torchvision.ops.deform_conv2d on the same inputs (see the BiRefNet port notes).
    const TORCHVISION: [f32; 60] = [
        0.409474, 0.277479, 0.495312, 0.251350, 0.269454, 0.389519, 0.349180, 0.289685, 0.541398, 0.983900, 0.343104, 0.326755, 0.009053, 0.112521,
        0.379089, 0.376254, 0.264655, 0.753719, 0.262523, 0.305926, 0.851153, 0.973716, 0.740096, 0.954596, 1.002662, 0.884778, 0.959176, 1.017619,
        0.739257, 0.279368, 0.956792, 0.897946, 1.325485, 1.101080, 0.873888, 0.905935, 1.015565, 0.510681, 0.996126, 0.965778, 0.751331, 0.639826,
        0.886133, 0.703940, 0.590341, 0.704482, 0.597012, 0.540396, 0.841714, 1.312891, 0.607823, 0.741084, 0.209377, 0.551745, 0.736548, 0.675712,
        0.569506, 1.083513, 0.610259, 0.627148,
    ];

    #[test]
    fn deform_conv_matches_torchvision() {
        let (c, h, w, o, k) = (2, 4, 5, 3, 3);
        let kk = k * k;
        let x = Map::from_chw(&seq(c * h * w, 0.37, 0.0, 1.0), c, h, w).unwrap();
        let wt = seq(o * c * kk, 0.53, 0.2, 0.5);
        let b = seq(o, 1.1, 0.3, 1.0);
        let off = seq(2 * kk * h * w, 0.71, 0.1, 1.7);
        let mask: Vec<f32> = seq(kk * h * w, 0.29, 0.4, 1.0).iter().map(|v| v * 0.5 + 0.5).collect();
        let om = Map::from_chw(&[off, mask].concat(), 3 * kk, h, w).unwrap();
        let conv = Conv::new(&wt, &[o, c, k, k], Some(&b), None, 1, &Device::Cpu).unwrap();
        let y = conv.forward_deform(&x, &om, Act::None).unwrap().to_chw();
        for (a, b) in y.iter().zip(TORCHVISION) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn zero_offsets_equal_a_regular_conv() {
        // im2col (more outputs than inputs) and shift-and-add (fewer) forms, 3 × 3 and 5 × 5
        for (c, o, k) in [(3, 4, 3), (5, 2, 3), (4, 3, 5)] {
            check_regular(c, o, k);
        }
    }

    fn check_regular(c: usize, o: usize, k: usize) {
        let (h, w) = (6, 7);
        let x = Map::from_chw(&seq(c * h * w, 0.41, 0.3, 1.0), c, h, w).unwrap();
        let wt = seq(o * c * k * k, 0.23, 0.1, 0.4);
        let conv = Conv::new(&wt, &[o, c, k, k], None, None, k / 2, &Device::Cpu).unwrap();
        assert_eq!(conv.taps.is_some(), o < c);
        let mut om = vec![0f32; h * w * 3 * k * k];
        om.chunks_mut(3 * k * k).for_each(|px| px[2 * k * k..].fill(1.0));
        let om = Map::new(h, w, 3 * k * k, om).unwrap();
        let a = conv.forward(&x, Act::None).unwrap();
        let b = conv.forward_deform(&x, &om, Act::None).unwrap();
        // a direct convolution as the reference
        let chw = x.to_chw();
        let ya = a.to_chw();
        for oc in 0..o {
            for y in 0..h {
                for xx in 0..w {
                    let mut s = 0.0;
                    for ic in 0..c {
                        for ky in 0..k {
                            for kx in 0..k {
                                let r = (k / 2) as i64;
                                let (iy, ix) = (y as i64 + ky as i64 - r, xx as i64 + kx as i64 - r);
                                if iy >= 0 && ix >= 0 && iy < h as i64 && ix < w as i64 {
                                    s += wt[((oc * c + ic) * k + ky) * k + kx] * chw[(ic * h + iy as usize) * w + ix as usize];
                                }
                            }
                        }
                    }
                    assert!((ya[(oc * h + y) * w + xx] - s).abs() < 1e-5);
                }
            }
        }
        for (p, q) in a.data.iter().zip(&b.data) {
            assert!((p - q).abs() < 1e-5);
        }
    }

    #[test]
    fn bad_shapes_are_errors() {
        assert!(Conv::new(&[0.0; 5], &[1, 1, 2, 2], None, None, 0, &Device::Cpu).is_err());
        let conv = Conv::new(&[0.0; 9], &[1, 1, 3, 3], None, None, 0, &Device::Cpu).unwrap();
        let tiny = Map::new(2, 2, 1, vec![0.0; 4]).unwrap();
        assert!(conv.forward(&tiny, Act::None).is_err());
        let wrong = Map::new(4, 4, 2, vec![0.0; 32]).unwrap();
        assert!(conv.forward(&wrong, Act::None).is_err());
    }
}
