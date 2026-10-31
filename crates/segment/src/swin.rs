//! Swin Transformer v1 (Liu et al., 2021), the backbone variant BiRefNet uses: 4 × 4 patch
//! embedding with layer norm, four stages of window attention with relative position bias
//! (every second block on windows shifted by half a window, with the cyclic-shift attention
//! mask), zero padding to whole windows, patch merging between stages, and a layer norm on
//! each stage's output. The size (tiny, small, base, large) is read from the weights.
//!
//! Tokens are rows of a channels-last [`Map`]; window partition, cyclic shift and padding are
//! one gather, and their inverse one scatter fused with the residual add.
//!
//! Modified work (MIT): ported by the LightCraft contributors in 2026 from
//! `models/backbones/swin_v1.py` of BiRefNet (<https://github.com/ZhengPeng7/BiRefNet>,
//! Copyright (c) 2024 ZhengPeng, MIT licence), itself from the Swin Transformer code,
//! Copyright (c) 2021 Microsoft, MIT licence (written by Ze Liu, Yutong Lin, Yixuan Wei);
//! translated to Rust and restructured. See NOTICE.

use rayon::prelude::*;

use crate::feat::{Act, Dense, Map, Params, add_assign, layer_norm};
use crate::{Error, Result};

const LN_EPS: f32 = 1e-5;
const PATCH: usize = 4;
/// Most blocks per stage accepted from a checkpoint (Swin-L has 18).
const MAX_DEPTH: usize = 64;

struct Norm {
    g: Vec<f32>,
    b: Vec<f32>,
}

impl Norm {
    fn load(p: &Params, name: &str, n: usize) -> Result<Self> {
        let (g, b) = p.norm(name, n)?;
        Ok(Self { g, b })
    }

    fn apply(&self, x: &mut [f32]) {
        layer_norm(x, &self.g, &self.b, LN_EPS);
    }
}

struct Block {
    norm1: Norm,
    qkv: Dense,
    proj: Dense,
    /// Relative position bias `[heads, N, N]`.
    bias: Vec<f32>,
    norm2: Norm,
    fc1: Dense,
    fc2: Dense,
    heads: usize,
    shift: bool,
}

struct Stage {
    dim: usize,
    blocks: Vec<Block>,
    merge: Option<(Norm, Dense)>,
}

/// The backbone.
pub struct Swin {
    embed: Dense,
    embed_norm: Norm,
    stages: Vec<Stage>,
    out_norms: Vec<Norm>,
    window: usize,
    pub name: &'static str,
}

/// Window geometry of one resolution.
struct Windows {
    h: usize,
    w: usize,
    ws: usize,
    /// Windows per row and column (of the padded map).
    nx: usize,
    ny: usize,
}

impl Windows {
    fn new(h: usize, w: usize, ws: usize) -> Self {
        Self { h, w, ws, ny: h.div_ceil(ws), nx: w.div_ceil(ws) }
    }

    fn count(&self) -> usize {
        self.nx * self.ny
    }

    fn n(&self) -> usize {
        self.ws * self.ws
    }

    /// The source pixel of window row `r` (window-major, then row-major in the window) after a
    /// cyclic shift of `shift` (`torch.roll(x, -shift)`), or `None` in the padding.
    fn source(&self, r: usize, shift: usize) -> Option<usize> {
        let (win, t) = (r / self.n(), r % self.n());
        let (hp, wp) = (self.ny * self.ws, self.nx * self.ws);
        let py = (win / self.nx) * self.ws + t / self.ws;
        let px = (win % self.nx) * self.ws + t % self.ws;
        let (sy, sx) = ((py + shift) % hp, (px + shift) % wp);
        (sy < self.h && sx < self.w).then_some(sy * self.w + sx)
    }

    /// The shifted-window region label of window row `r` (the reference's `img_mask`).
    fn region(&self, r: usize, shift: usize) -> u8 {
        let (win, t) = (r / self.n(), r % self.n());
        let (hp, wp) = (self.ny * self.ws, self.nx * self.ws);
        let py = (win / self.nx) * self.ws + t / self.ws;
        let px = (win % self.nx) * self.ws + t % self.ws;
        let band = |p: usize, size: usize| -> u8 {
            if p + self.ws < size {
                0
            } else if p + shift < size {
                1
            } else {
                2
            }
        };
        band(py, hp) * 3 + band(px, wp)
    }
}

impl Block {
    fn load(p: &Params, name: &str, dim: usize, ws: usize, shift: bool) -> Result<Self> {
        let table_name = format!("{name}.attn.relative_position_bias_table");
        let (table, shape) = p.get(&table_name)?;
        let side = 2 * ws - 1;
        let &[rows, heads] = shape.as_slice() else { return Err(Error::Model(format!("{table_name}: shape {shape:?}"))) };
        if rows != side * side || heads == 0 || !dim.is_multiple_of(heads) {
            return Err(Error::Model(format!("{table_name}: shape {shape:?} for window {ws}")));
        }
        let n = ws * ws;
        let mut bias = vec![0f32; heads * n * n];
        for i in 0..n {
            for j in 0..n {
                let (yi, xi, yj, xj) = (i / ws, i % ws, j / ws, j % ws);
                let idx = (yi + ws - 1 - yj) * side + (xi + ws - 1 - xj);
                for h in 0..heads {
                    if let (Some(d), Some(s)) = (bias.get_mut((h * n + i) * n + j), table.get(idx * heads + h)) {
                        *d = *s;
                    }
                }
            }
        }
        let b = Self {
            norm1: Norm::load(p, &format!("{name}.norm1"), dim)?,
            qkv: p.linear(&format!("{name}.attn.qkv"))?,
            proj: p.linear(&format!("{name}.attn.proj"))?,
            bias,
            norm2: Norm::load(p, &format!("{name}.norm2"), dim)?,
            fc1: p.linear(&format!("{name}.mlp.fc1"))?,
            fc2: p.linear(&format!("{name}.mlp.fc2"))?,
            heads,
            shift,
        };
        if b.qkv.inp != dim || b.qkv.out != 3 * dim || b.proj.inp != dim || b.proj.out != dim || b.fc1.inp != dim || b.fc2.out != dim {
            return Err(Error::Model(format!("{name}: layer sizes do not match {dim} channels")));
        }
        Ok(b)
    }

    /// One block on tokens `x` (`win.h × win.w` rows of `dim`), in place.
    fn forward(&self, x: &mut [f32], dim: usize, win: &Windows) -> Result<()> {
        let (n, nw, heads) = (win.n(), win.count(), self.heads);
        let d = dim / heads;
        let shift = if self.shift { win.ws / 2 } else { 0 };
        let rows = nw * n;
        let tokens = win.h * win.w;
        // LN, then q, k, v of every token; padding tokens are zero after the norm, so theirs
        // are the qkv bias
        let mut xn = x.to_vec();
        self.norm1.apply(&mut xn);
        let qkv = self.qkv.forward(&xn, Act::None)?;
        drop(xn);
        let zero = vec![0f32; 3 * dim];
        let pad_row = self.qkv.bias().unwrap_or(&zero);
        // pad → roll → window partition, as a source token per window row
        let src: Vec<Option<usize>> = (0..rows).map(|r| win.source(r, shift)).collect();
        let labels: Vec<u8> = if shift > 0 { (0..rows).map(|r| win.region(r, shift)).collect() } else { Vec::new() };
        let scale = (d as f32).powf(-0.5);
        // attention per (window, head), out as [nw, heads, n, d]
        let mut o = vec![0f32; rows * dim];
        o.par_chunks_mut(n * d).enumerate().for_each_init(
            || (vec![0f32; n * d], vec![0f32; d * n], vec![0f32; n * d], vec![0f32; n]),
            |(q, kt, v, s), (bh, out)| {
                let (wi, h) = (bh / heads, bh % heads);
                let win_src = src.get(wi * n..(wi + 1) * n).unwrap_or(&[]);
                for (t, sr) in win_src.iter().enumerate() {
                    let row = match sr {
                        Some(s) => qkv.get(s * 3 * dim..(s + 1) * 3 * dim).unwrap_or(pad_row),
                        None => pad_row,
                    };
                    let (rq, rk, rv) =
                        (row.get(h * d..(h + 1) * d), row.get(dim + h * d..dim + (h + 1) * d), row.get(2 * dim + h * d..2 * dim + (h + 1) * d));
                    if let (Some(rq), Some(rk), Some(rv), Some(dq), Some(dv)) =
                        (rq, rk, rv, q.get_mut(t * d..(t + 1) * d), v.get_mut(t * d..(t + 1) * d))
                    {
                        dq.iter_mut().zip(rq).for_each(|(a, b)| *a = b * scale);
                        dv.copy_from_slice(rv);
                        for (e, val) in rk.iter().enumerate() {
                            if let Some(slot) = kt.get_mut(e * n + t) {
                                *slot = *val;
                            }
                        }
                    }
                }
                let win_labels = labels.get(wi * n..(wi + 1) * n);
                let bias = self.bias.get(h * n * n..(h + 1) * n * n).unwrap_or(&[]);
                for (i, ((qi, orow), brow)) in q.chunks(d).zip(out.chunks_mut(d)).zip(bias.chunks(n)).enumerate() {
                    // scores: relative position bias (+ the shift mask) + q·kᵀ
                    s.copy_from_slice(brow);
                    if let Some(l) = win_labels
                        && let Some(li) = l.get(i)
                    {
                        s.iter_mut().zip(l).for_each(|(a, lj)| {
                            if lj != li {
                                *a += -100.0;
                            }
                        });
                    }
                    for (qe, kr) in qi.iter().zip(kt.chunks(n)) {
                        s.iter_mut().zip(kr).for_each(|(a, k)| *a += qe * k);
                    }
                    let m = s.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
                    let mut sum = 0.0;
                    s.iter_mut().for_each(|v| {
                        *v = (*v - m).exp();
                        sum += *v;
                    });
                    let inv = 1.0 / sum;
                    orow.fill(0.0);
                    for (p, vr) in s.iter().zip(v.chunks(d)) {
                        let p = p * inv;
                        orow.iter_mut().zip(vr).for_each(|(a, b)| *a += p * b);
                    }
                }
            },
        );
        // window reverse → roll back → crop (each token has exactly one window row)
        let mut row_of = vec![usize::MAX; tokens];
        for (r, s) in src.iter().enumerate() {
            if let Some(s) = s
                && let Some(d) = row_of.get_mut(*s)
            {
                *d = r;
            }
        }
        let mut merged = vec![0f32; tokens * dim];
        merged.par_chunks_mut(dim).zip(row_of.par_iter()).for_each(|(px, &r)| {
            let (wi, t) = (r / n, r % n);
            for (h, dst) in px.chunks_mut(d).enumerate() {
                let at = ((wi * heads + h) * n + t) * d;
                if let Some(s) = o.get(at..at + d) {
                    dst.copy_from_slice(s);
                }
            }
        });
        drop(o);
        let y = self.proj.forward(&merged, Act::None)?;
        drop(merged);
        add_assign(x, &y);
        drop(y);
        // MLP
        let mut xn = x.to_vec();
        self.norm2.apply(&mut xn);
        let hid = self.fc1.forward(&xn, Act::Gelu)?;
        drop(xn);
        let y = self.fc2.forward(&hid, Act::None)?;
        drop(hid);
        add_assign(x, &y);
        Ok(())
    }
}

impl Swin {
    /// The backbone under `prefix` (`bb`), sized from its tensors.
    pub fn load(p: &Params, prefix: &str) -> Result<Self> {
        let (w, shape) = p.get(&format!("{prefix}.patch_embed.proj.weight"))?;
        let &[embed, 3, PATCH, PATCH] = shape.as_slice() else {
            return Err(Error::Model(format!("{prefix}.patch_embed.proj.weight: shape {shape:?}")));
        };
        let b = p.vec(&format!("{prefix}.patch_embed.proj.bias"), embed)?;
        let embed_dense = Dense::from_out_in(&w, embed, 3 * PATCH * PATCH, Some(b), &p.dev)?;
        let table = p
            .shape(&format!("{prefix}.layers.0.blocks.0.attn.relative_position_bias_table"))
            .and_then(|s| s.first().copied())
            .ok_or_else(|| Error::Model(format!("{prefix}: no Swin attention tables")))?;
        // (2·ws − 1)² rows
        let window = (1..64).find(|ws| (2 * ws - 1) * (2 * ws - 1) == table).ok_or_else(|| Error::Model(format!("window table of {table} rows")))?;
        let mut stages = Vec::new();
        let mut out_norms = Vec::new();
        let mut depths = Vec::new();
        for i in 0..4 {
            let dim = embed << i;
            let mut blocks = Vec::new();
            while blocks.len() < MAX_DEPTH && p.has(&format!("{prefix}.layers.{i}.blocks.{}.norm1.weight", blocks.len())) {
                let j = blocks.len();
                blocks.push(Block::load(p, &format!("{prefix}.layers.{i}.blocks.{j}"), dim, window, j % 2 == 1)?);
            }
            if blocks.is_empty() {
                return Err(Error::Model(format!("{prefix}: stage {i} has no blocks")));
            }
            depths.push(blocks.len());
            let merge = if i < 3 {
                let reduction = p.linear(&format!("{prefix}.layers.{i}.downsample.reduction"))?;
                if reduction.inp != 4 * dim || reduction.out != 2 * dim {
                    return Err(Error::Model(format!("{prefix}.layers.{i}.downsample: wrong size")));
                }
                Some((Norm::load(p, &format!("{prefix}.layers.{i}.downsample.norm"), 4 * dim)?, reduction))
            } else {
                None
            };
            stages.push(Stage { dim, blocks, merge });
            out_norms.push(Norm::load(p, &format!("{prefix}.norm{i}"), dim)?);
        }
        let name = match (embed, depths.get(2).copied(), window) {
            (96, Some(6), 7) => "swin_v1_t",
            (96, Some(18), 7) => "swin_v1_s",
            (128, Some(18), 12) => "swin_v1_b",
            (192, Some(18), 12) => "swin_v1_l",
            _ => "swin_v1",
        };
        Ok(Self { embed: embed_dense, embed_norm: Norm::load(p, &format!("{prefix}.patch_embed.norm"), embed)?, stages, out_norms, window, name })
    }

    /// The four normalized stage outputs (strides 4, 8, 16, 32) of a 3-channel image.
    pub fn forward(&self, img: &Map) -> Result<Vec<Map>> {
        if img.c != 3 || img.h == 0 || img.w == 0 {
            return Err(Error::Model("Swin input must be a 3-channel image".into()));
        }
        // patch embedding (zero padding to whole patches), as one product
        let (mut h, mut w) = (img.h.div_ceil(PATCH), img.w.div_ceil(PATCH));
        let k = 3 * PATCH * PATCH;
        let mut patches = vec![0f32; h * w * k];
        patches.par_chunks_mut(k).enumerate().for_each(|(p, row)| {
            let (py, px) = (p / w, p % w);
            for c in 0..3 {
                for ky in 0..PATCH {
                    for kx in 0..PATCH {
                        let (y, x) = (py * PATCH + ky, px * PATCH + kx);
                        if y < img.h
                            && x < img.w
                            && let (Some(d), Some(s)) = (row.get_mut((c * PATCH + ky) * PATCH + kx), img.data.get((y * img.w + x) * 3 + c))
                        {
                            *d = *s;
                        }
                    }
                }
            }
        });
        let mut x = self.embed.forward(&patches, Act::None)?;
        drop(patches);
        self.embed_norm.apply(&mut x);
        let mut outs = Vec::with_capacity(4);
        for (stage, norm) in self.stages.iter().zip(&self.out_norms) {
            let win = Windows::new(h, w, self.window);
            let mut prof = crate::feat::Profile::new();
            for b in &stage.blocks {
                b.forward(&mut x, stage.dim, &win)?;
            }
            prof.step(&format!("swin stage {} ({h} × {w})", stage.dim));
            let mut o = x.clone();
            norm.apply(&mut o);
            outs.push(Map::new(h, w, stage.dim, o)?);
            if let Some((mnorm, red)) = &stage.merge {
                let (h2, w2) = (h.div_ceil(2), w.div_ceil(2));
                let c = stage.dim;
                let mut m = vec![0f32; h2 * w2 * 4 * c];
                m.par_chunks_mut(4 * c).enumerate().for_each(|(p, row)| {
                    let (y, xx) = (2 * (p / w2), 2 * (p % w2));
                    // x0 (even, even), x1 (odd row), x2 (odd column), x3 (odd, odd)
                    for (q, (dy, dx)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
                        let (sy, sx) = (y + dy, xx + dx);
                        if sy < h
                            && sx < w
                            && let (Some(d), Some(s)) = (row.get_mut(q * c..(q + 1) * c), x.get((sy * w + sx) * c..(sy * w + sx + 1) * c))
                        {
                            d.copy_from_slice(s);
                        }
                    }
                });
                mnorm.apply(&mut m);
                x = red.forward(&m, Act::None)?;
                (h, w) = (h2, w2);
            }
        }
        Ok(outs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cover_every_token_once() {
        for (h, w, ws, shift) in [(10, 13, 7, 3), (14, 14, 7, 0), (5, 9, 4, 2)] {
            let win = Windows::new(h, w, ws);
            let mut seen = vec![0; h * w];
            for r in 0..win.count() * win.n() {
                if let Some(s) = win.source(r, shift) {
                    seen[s] += 1;
                }
            }
            assert!(seen.iter().all(|&c| c == 1));
        }
    }

    #[test]
    fn shifted_regions_follow_the_reference_mask() {
        // Hp = Wp = 8, ws 4, shift 2: rows 0..4 → band 0, 4..6 → 1, 6..8 → 2
        let win = Windows::new(8, 8, 4);
        let band = |py: usize| {
            let r = (py / 4) * 2 * 16 + (py % 4) * 4; // first column of that row
            win.region(r, 2) / 3
        };
        assert_eq!((0..8).map(band).collect::<Vec<_>>(), vec![0, 0, 0, 0, 1, 1, 2, 2]);
    }
}
