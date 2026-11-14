//! The SAM 3 vision backbone as a Core ML program, for the Apple Neural Engine.
//!
//! [`write_backbone`] reads the backbone's weights the way [`crate::vit::Vit`] does and writes the layers after the
//! patch embedding (which stays on the candle device: a convolution and a norm, a fraction of the work) as an ML
//! Program package ([`lightcraft_coreml::mil`]). The graph follows `vit.rs` layer by layer:
//!
//! - windowed layers work on `grid²/window²` windows of `window²` tokens at once (a batch), global layers on all the
//!   tokens; the windows are cut and put back with a reshape and a transpose, only where the kind of layer changes;
//! - the rotary position encoding is `x·cos + rotate(x)·sin`, where `rotate` swaps and negates each pair of numbers.
//!   `rotate` of a projection is the projection with its rows swapped and negated in pairs, so the query and the key
//!   are each projected twice (plain and rotated) and the encoding is two products and a sum: a `matmul` by a fixed
//!   matrix, or slices and a concatenation, left the Neural Engine for the processor or its compiler. The attention
//!   scale `head_dim^-½` is folded into the query's tables;
//! - attention, the GELU MLP and the residual sums are as in the reference.
//!
//! Everything computes in 16 bit (what the Neural Engine does). The residual stream of the real model stays below
//! about 300 on photos, far from the 65504 limit, so no rescaling is needed; `a_real_photo_...` in the tests measures
//! the agreement with candle where the model is installed.

use std::path::{Path, PathBuf};

use candle_nn::VarBuilder;
use lightcraft_coreml::mil::{Builder, Val};

use crate::vit::rope_values;
use crate::{Error, Result};

/// The sizes of a backbone.
#[derive(Clone, Copy, Debug)]
pub struct Dims {
    /// Tokens per side of the grid.
    pub grid: usize,
    /// Tokens per side of a window (the grid is a whole number of windows).
    pub window: usize,
    pub hidden: usize,
    pub heads: usize,
    /// The MLP's inner size.
    pub mlp: usize,
    pub layers: usize,
    /// The layers that attend over the whole grid.
    pub global: &'static [usize],
}

impl Dims {
    /// SAM 3's backbone.
    pub const SAM3: Dims = Dims { grid: 72, window: 24, hidden: 1024, heads: 16, mlp: 4736, layers: 32, global: &[7, 15, 23, 31] };

    /// Tokens in the grid.
    pub fn tokens(&self) -> usize {
        self.grid * self.grid
    }

    fn check(&self) -> Result<()> {
        let ok = self.window > 0
            && self.grid.is_multiple_of(self.window)
            && self.heads > 0
            && self.hidden.is_multiple_of(self.heads)
            && (self.hidden / self.heads).is_multiple_of(2);
        if ok { Ok(()) } else { Err(Error::Model(format!("a backbone of {self:?} cannot be written"))) }
    }
}

/// One weight tensor as numbers.
fn numbers(vb: &VarBuilder, shape: &[usize], name: &str) -> Result<Vec<f32>> {
    Ok(vb.get(shape, name)?.flatten_all()?.to_vec1::<f32>()?)
}

/// The weight `name` (of `shape`) as a constant of the program.
fn weight(b: &mut Builder, vb: &VarBuilder, shape: &[usize], name: &str) -> Result<Val> {
    Ok(b.weight(shape, &numbers(vb, shape, name)?)?)
}

/// The rows of a projection (`rows` of `cols` numbers, a head's `head_dim` rows after another) that give the rotary
/// encoding's pairwise rotation `(a, b) → (−b, a)` of what the projection gives.
fn rotated_rows(w: &[f32], cols: usize, head_dim: usize) -> Vec<f32> {
    let rows = w.len().checked_div(cols).unwrap_or(0);
    let mut out = Vec::with_capacity(w.len());
    for r in 0..rows {
        let j = r % head_dim;
        let (from, sign) = if j.is_multiple_of(2) { (r + 1, -1.0) } else { (r - 1, 1.0) };
        let row = w.get(from * cols..(from + 1) * cols).unwrap_or(&[]);
        out.extend(row.iter().map(|v| v * sign));
    }
    out
}

/// The tables of one rotary encoding as constants: cosine and sine for the keys, and both with the attention scale for
/// the queries.
struct Rope {
    cos_q: Val,
    sin_q: Val,
    cos_k: Val,
    sin_k: Val,
}

fn rope_tables(b: &mut Builder, side: usize, scale: f64, head_dim: usize, query_scale: f32) -> Result<Rope> {
    let (cos, sin) = rope_values(side, scale, head_dim);
    let shape = [side * side, head_dim];
    let scaled = |t: &[f32]| t.iter().map(|v| v * query_scale).collect::<Vec<f32>>();
    Ok(Rope {
        cos_q: b.weight(&shape, &scaled(&cos))?,
        sin_q: b.weight(&shape, &scaled(&sin))?,
        cos_k: b.weight(&shape, &cos)?,
        sin_k: b.weight(&shape, &sin)?,
    })
}

/// The projection `name` with its rows rotated (see [`rotated_rows`]).
fn rotated_linear(b: &mut Builder, vb: &VarBuilder, x: &Val, name: &str, h: usize, head_dim: usize) -> Result<Val> {
    let w = rotated_rows(&numbers(vb, &[h, h], &format!("{name}.weight"))?, h, head_dim);
    let bias = rotated_rows(&numbers(vb, &[h], &format!("{name}.bias"))?, 1, head_dim);
    let (w, bias) = (b.weight(&[h, h], &w)?, b.weight(&[h], &bias)?);
    Ok(b.linear(x, &w, &bias)?)
}

/// `x·cos + rotated·sin` for `x` and its rotated projection, both `[batch, heads, tokens, head_dim]`.
fn apply_rope(b: &mut Builder, x: &Val, rotated: &Val, cos: &Val, sin: &Val) -> Result<Val> {
    let (a, c) = (b.mul(x, cos)?, b.mul(rotated, sin)?);
    Ok(b.add(&a, &c)?)
}

/// One transformer layer over `x` of `[batch, tokens, hidden]` (see the module's notes).
fn layer(b: &mut Builder, vb: &VarBuilder, d: &Dims, x: &Val, rope: &Rope) -> Result<Val> {
    let (batch, tokens) = match x.shape() {
        [b, t, _] => (*b, *t),
        other => return Err(Error::Model(format!("a layer's input has shape {other:?}"))),
    };
    let (h, heads, head_dim) = (d.hidden, d.heads, d.hidden / d.heads);
    let norm = |b: &mut Builder, x: &Val, name: &str| -> Result<Val> {
        let (g, be) = (weight(b, vb, &[h], &format!("{name}.weight"))?, weight(b, vb, &[h], &format!("{name}.bias"))?);
        Ok(b.layer_norm(x, &g, &be, 1e-6)?)
    };
    let linear = |b: &mut Builder, x: &Val, name: &str, out: usize, inn: usize| -> Result<Val> {
        let (w, bias) = (weight(b, vb, &[out, inn], &format!("{name}.weight"))?, weight(b, vb, &[out], &format!("{name}.bias"))?);
        Ok(b.linear(x, &w, &bias)?)
    };
    let split = |b: &mut Builder, t: &Val| -> Result<Val> {
        let t = b.reshape(t, &[batch, tokens, heads, head_dim])?;
        Ok(b.transpose(&t, &[0, 2, 1, 3])?)
    };

    let y = norm(b, x, "layer_norm1")?;
    let q = linear(b, &y, "attention.q_proj", h, h)?;
    let k = linear(b, &y, "attention.k_proj", h, h)?;
    let v = linear(b, &y, "attention.v_proj", h, h)?;
    let q_rot = rotated_linear(b, vb, &y, "attention.q_proj", h, head_dim)?;
    let k_rot = rotated_linear(b, vb, &y, "attention.k_proj", h, head_dim)?;
    let (q, k, v, q_rot, k_rot) = (split(b, &q)?, split(b, &k)?, split(b, &v)?, split(b, &q_rot)?, split(b, &k_rot)?);
    let q = apply_rope(b, &q, &q_rot, &rope.cos_q, &rope.sin_q)?;
    let k = apply_rope(b, &k, &k_rot, &rope.cos_k, &rope.sin_k)?;
    let scores = b.matmul(&q, &k, true)?;
    let probs = b.softmax(&scores)?;
    let o = b.matmul(&probs, &v, false)?;
    let o = b.transpose(&o, &[0, 2, 1, 3])?;
    let o = b.reshape(&o, &[batch, tokens, h])?;
    let o = linear(b, &o, "attention.o_proj", h, h)?;
    let x = b.add(x, &o)?;
    let y = norm(b, &x, "layer_norm2")?;
    let y = linear(b, &y, "mlp.fc1", d.mlp, h)?;
    let y = b.gelu(&y)?;
    let y = linear(b, &y, "mlp.fc2", h, d.mlp)?;
    Ok(b.add(&x, &y)?)
}

/// Cut `[1, grid², hidden]` into windows `[windows, window², hidden]`.
fn windows(b: &mut Builder, d: &Dims, x: &Val) -> Result<Val> {
    let (nw, w, h) = (d.grid / d.window, d.window, d.hidden);
    let x = b.reshape(x, &[nw, w, nw, w, h])?;
    let x = b.transpose(&x, &[0, 2, 1, 3, 4])?;
    Ok(b.reshape(&x, &[nw * nw, w * w, h])?)
}

/// The reverse of [`windows`].
fn unwindow(b: &mut Builder, d: &Dims, x: &Val) -> Result<Val> {
    let (nw, w, h) = (d.grid / d.window, d.window, d.hidden);
    let x = b.reshape(x, &[nw, nw, w, w, h])?;
    let x = b.transpose(&x, &[0, 2, 1, 3, 4])?;
    Ok(b.reshape(&x, &[1, d.tokens(), h])?)
}

/// Writes the layers of the backbone whose weights `vb` holds (at `…backbone`) as a package in `dir`; its input `x` is
/// the first layer's input `[1, grid², hidden]` (32-bit; what [`crate::vit::Vit::embed`] gives, flattened) and its
/// output the last layer's `[1, grid², hidden]`. Returns the package's path and the name of its output.
pub fn write_backbone(vb: &VarBuilder, d: &Dims, dir: &Path) -> Result<(PathBuf, String)> {
    d.check()?;
    let head_dim = d.hidden / d.heads;
    let mut b = Builder::create(dir)?;
    let input = b.input("x", &[1, d.tokens(), d.hidden])?;
    let mut x = b.to_f16(&input)?;
    let query_scale = (head_dim as f32).powf(-0.5);
    let rope_window = rope_tables(&mut b, d.window, 1.0, head_dim, query_scale)?;
    let rope_global = rope_tables(&mut b, d.grid, d.window as f64 / d.grid as f64, head_dim, query_scale)?;
    let mut cut = false;
    for i in 0..d.layers {
        let l = vb.pp(format!("layers.{i}"));
        if d.global.contains(&i) {
            if cut {
                x = unwindow(&mut b, d, &x)?;
                cut = false;
            }
            x = layer(&mut b, &l, d, &x, &rope_global)?;
        } else {
            if !cut {
                x = windows(&mut b, d, &x)?;
                cut = true;
            }
            x = layer(&mut b, &l, d, &x, &rope_window)?;
        }
    }
    if cut {
        x = unwindow(&mut b, d, &x)?;
    }
    let out = b.to_f32(&x)?;
    let name = out.name().to_string();
    Ok((b.finish(&out)?, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{DType, Device, Tensor};
    use lightcraft_coreml::{ComputeUnits, Model, compile};
    use std::collections::HashMap;

    const TINY: Dims = Dims { grid: 4, window: 2, hidden: 8, heads: 2, mlp: 12, layers: 3, global: &[2] };

    struct Rng(u32);
    impl Rng {
        fn next(&mut self, scale: f32) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((self.0 >> 8) as f32 / (1 << 24) as f32 - 0.5) * scale
        }
    }

    /// Random weights named as the checkpoint names them, under `layers.{i}.`.
    fn weights(d: &Dims, rng: &mut Rng) -> HashMap<String, Vec<f32>> {
        let mut m = HashMap::new();
        for i in 0..d.layers {
            let mut put = |name: &str, n: usize, scale: f32, base: f32| {
                m.insert(format!("layers.{i}.{name}"), (0..n).map(|_| base + rng.next(scale)).collect::<Vec<f32>>());
            };
            let (h, f) = (d.hidden, d.mlp);
            for n in ["layer_norm1", "layer_norm2"] {
                put(&format!("{n}.weight"), h, 0.2, 1.0);
                put(&format!("{n}.bias"), h, 0.2, 0.0);
            }
            for n in ["q_proj", "k_proj", "v_proj", "o_proj"] {
                put(&format!("attention.{n}.weight"), h * h, 0.8, 0.0);
                put(&format!("attention.{n}.bias"), h, 0.2, 0.0);
            }
            put("mlp.fc1.weight", f * h, 0.8, 0.0);
            put("mlp.fc1.bias", f, 0.2, 0.0);
            put("mlp.fc2.weight", h * f, 0.8, 0.0);
            put("mlp.fc2.bias", h, 0.2, 0.0);
        }
        m
    }

    fn matvec(w: &[f32], b: &[f32], x: &[f32]) -> Vec<f32> {
        b.iter().enumerate().map(|(o, bias)| bias + x.iter().enumerate().map(|(i, v)| v * w[o * x.len() + i]).sum::<f32>()).collect()
    }

    fn erf(x: f32) -> f32 {
        // Abramowitz-Stegun 7.1.26, good to 1.5e-7
        let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
        let y = 1.0 - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6) * t * (-x * x).exp();
        if x < 0.0 { -y } else { y }
    }

    /// The backbone's layers in plain loops, token by token, as `vit.rs` does them.
    fn reference(d: &Dims, w: &HashMap<String, Vec<f32>>, input: &[f32]) -> Vec<f32> {
        let (h, hd, grid) = (d.hidden, d.hidden / d.heads, d.grid);
        let mut x: Vec<Vec<f32>> = input.chunks(h).map(<[f32]>::to_vec).collect();
        let (cw, sw) = rope_values(d.window, 1.0, hd);
        let (cg, sg) = rope_values(grid, d.window as f64 / grid as f64, hd);
        for i in 0..d.layers {
            let g = |n: &str| &w[&format!("layers.{i}.{n}")];
            let global = d.global.contains(&i);
            // each group of tokens attends within itself; `local` is a token's place in the table
            let mut groups: Vec<Vec<usize>> = Vec::new();
            if global {
                groups.push((0..grid * grid).collect());
            } else {
                let nw = grid / d.window;
                for wr in 0..nw {
                    for wc in 0..nw {
                        groups.push((0..d.window * d.window).map(|k| (wr * d.window + k / d.window) * grid + wc * d.window + k % d.window).collect());
                    }
                }
            }
            let norm = |t: &[f32], n: &str| -> Vec<f32> {
                let mean = t.iter().sum::<f32>() / h as f32;
                let var = t.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / h as f32;
                (0..h).map(|c| (t[c] - mean) / (var + 1e-6).sqrt() * g(&format!("{n}.weight"))[c] + g(&format!("{n}.bias"))[c]).collect()
            };
            let mut next = x.clone();
            for group in &groups {
                let y: Vec<Vec<f32>> = group.iter().map(|t| norm(&x[*t], "layer_norm1")).collect();
                let proj = |n: &str| -> Vec<Vec<f32>> {
                    y.iter().map(|v| matvec(g(&format!("attention.{n}.weight")), g(&format!("attention.{n}.bias")), v)).collect()
                };
                let (mut q, mut k, v) = (proj("q_proj"), proj("k_proj"), proj("v_proj"));
                let (cos, sin) = if global { (&cg, &sg) } else { (&cw, &sw) };
                for (local, (qt, kt)) in q.iter_mut().zip(k.iter_mut()).enumerate() {
                    for t in [qt, kt] {
                        for hh in 0..d.heads {
                            let part = t[hh * hd..(hh + 1) * hd].to_vec();
                            for c in 0..hd {
                                let rot = if c % 2 == 0 { -part[c + 1] } else { part[c - 1] };
                                t[hh * hd + c] = part[c] * cos[local * hd + c] + rot * sin[local * hd + c];
                            }
                        }
                    }
                }
                let mut o = vec![vec![0f32; h]; group.len()];
                for hh in 0..d.heads {
                    for a in 0..group.len() {
                        let mut sc: Vec<f32> = (0..group.len())
                            .map(|b| (0..hd).map(|c| q[a][hh * hd + c] * k[b][hh * hd + c]).sum::<f32>() * (hd as f32).powf(-0.5))
                            .collect();
                        let m = sc.iter().cloned().fold(f32::MIN, f32::max);
                        sc.iter_mut().for_each(|s| *s = (*s - m).exp());
                        let z: f32 = sc.iter().sum();
                        for c in 0..hd {
                            o[a][hh * hd + c] = (0..group.len()).map(|b| sc[b] / z * v[b][hh * hd + c]).sum();
                        }
                    }
                }
                for (a, t) in group.iter().enumerate() {
                    let att = matvec(g("attention.o_proj.weight"), g("attention.o_proj.bias"), &o[a]);
                    let mid: Vec<f32> = (0..h).map(|c| x[*t][c] + att[c]).collect();
                    let y2 = norm(&mid, "layer_norm2");
                    let f1: Vec<f32> = matvec(g("mlp.fc1.weight"), g("mlp.fc1.bias"), &y2)
                        .into_iter()
                        .map(|u| 0.5 * u * (1.0 + erf(u / std::f32::consts::SQRT_2)))
                        .collect();
                    let f2 = matvec(g("mlp.fc2.weight"), g("mlp.fc2.bias"), &f1);
                    next[*t] = (0..h).map(|c| mid[c] + f2[c]).collect();
                }
            }
            x = next;
        }
        x.concat()
    }

    #[test]
    fn a_tiny_backbone_in_core_ml_matches_plain_loops() {
        if !lightcraft_coreml::supported() {
            lightcraft_testkit::skip!(System, "Core ML runs on macOS only");
            return;
        }
        let d = TINY;
        let mut rng = Rng(7);
        let w = weights(&d, &mut rng);
        let tensors: HashMap<String, Tensor> = w
            .iter()
            .map(|(k, v)| {
                let shape: Vec<usize> = if k.ends_with("weight") && !k.contains("layer_norm") {
                    if k.contains("fc1") {
                        vec![d.mlp, d.hidden]
                    } else if k.contains("fc2") {
                        vec![d.hidden, d.mlp]
                    } else {
                        vec![d.hidden, d.hidden]
                    }
                } else if k.contains("fc1.bias") {
                    vec![d.mlp]
                } else {
                    vec![d.hidden]
                };
                (k.clone(), Tensor::from_vec(v.clone(), shape, &Device::Cpu).unwrap())
            })
            .collect();
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
        let dir = std::env::temp_dir().join(format!("lc-vit-tiny-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (package, output) = write_backbone(&vb, &d, &dir).unwrap();
        let compiled = compile(&package, &dir).unwrap();
        let model = Model::load(&compiled, ComputeUnits::CpuOnly, "x", &output).unwrap();
        let input: Vec<f32> = (0..d.tokens() * d.hidden).map(|_| rng.next(3.0)).collect();
        let got = model.predict(&[1, d.tokens(), d.hidden], &input).unwrap();
        let expect = reference(&d, &w, &input);
        let worst = got.data.iter().zip(&expect).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        let big = expect.iter().map(|v| v.abs()).fold(0f32, f32::max);
        assert!(worst < 0.02 * big, "worst difference {worst} of values up to {big}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dimensions_that_do_not_fit_are_errors() {
        let vb = VarBuilder::from_tensors(HashMap::new(), DType::F32, &Device::Cpu);
        let dir = std::env::temp_dir().join(format!("lc-vit-bad-{}", std::process::id()));
        for d in
            [Dims { window: 3, ..TINY }, Dims { window: 0, ..TINY }, Dims { heads: 0, ..TINY }, Dims { hidden: 9, ..TINY }, Dims { heads: 8, ..TINY }]
        {
            assert!(write_backbone(&vb, &d, &dir).is_err(), "{d:?}");
        }
        // missing weights are an error too, not a panic
        assert!(write_backbone(&vb, &TINY, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
