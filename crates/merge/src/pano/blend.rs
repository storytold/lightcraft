//! Multi-band blending (P. J. Burt & E. H. Adelson, "A Multiresolution Spline With Application to
//! Image Mosaics", ACM ToG 1983), as used by Brown & Lowe: each image's Laplacian pyramid is
//! blended with the Gaussian pyramid of its (binary) seam mask, so low frequencies blend over wide
//! regions and high frequencies over narrow ones. Blending happens in log space, which turns
//! residual exposure differences into additive offsets the coarse bands smooth away.
//!
//! Images arrive as tiles placed on a canvas padded to a multiple of 2^levels; tile origins are
//! multiples of 2^levels so every pyramid level of a tile lines up with the canvas pyramid.

use rayon::prelude::*;

/// A planar-interleaved float buffer with `c` channels.
#[derive(Clone, Debug, PartialEq)]
pub struct Buf {
    pub w: usize,
    pub h: usize,
    pub c: usize,
    pub data: Vec<f32>,
}

impl Buf {
    pub fn new(w: usize, h: usize, c: usize) -> Buf {
        Buf { w, h, c, data: vec![0.0; w * h * c] }
    }
}

const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Binomial 5-tap blur then decimate by 2 (output `⌈w/2⌉ × ⌈h/2⌉`).
pub fn down(b: &Buf) -> Buf {
    let mut out = Buf::new(0, 0, b.c);
    down_into(b, &mut out);
    out
}

/// [`down`] into `out`, reusing its allocation.
pub fn down_into(b: &Buf, out: &mut Buf) {
    let (w2, h2, c) = (b.w.div_ceil(2), b.h.div_ceil(2), b.c);
    out.w = w2;
    out.h = h2;
    out.c = c;
    out.data.resize(w2 * h2 * c, 0.0);
    if b.w == 0 || b.h == 0 || c == 0 {
        return;
    }
    // the five (edge-clamped) source columns of each decimated column, then rows
    let taps = |n: usize, i: usize| std::array::from_fn::<usize, 5, _>(|k| (2 * i + k).saturating_sub(2).min(n - 1));
    let xs: Vec<[usize; 5]> = (0..w2).map(|x| taps(b.w, x)).collect();
    // horizontal pass at decimated x
    let mut tmp = vec![0.0f32; w2 * b.h * c];
    tmp.par_chunks_mut(w2 * c).zip(b.data.par_chunks(b.w * c)).for_each(|(row, src)| {
        for (x, t) in xs.iter().enumerate() {
            for ch in 0..c {
                let mut s = 0.0;
                for (k, wk) in K.iter().enumerate() {
                    s += wk * src[t[k] * c + ch];
                }
                row[x * c + ch] = s;
            }
        }
    });
    let row_len = w2 * c;
    out.data.par_chunks_mut(row_len).enumerate().for_each(|(y, row)| {
        let rows = taps(b.h, y).map(|r| &tmp[r * row_len..(r + 1) * row_len]);
        for (i, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (k, wk) in K.iter().enumerate() {
                s += wk * rows[k][i];
            }
            *o = s;
        }
    });
}

/// Per output coordinate of [`up`]: the two (edge-clamped) source coordinates and the weight of the second.
fn up_taps(n_src: usize, n: usize) -> Vec<(usize, usize, f32)> {
    (0..n)
        .map(|x| {
            let f = (x as f32 + 0.5) / 2.0 - 0.5;
            let x0 = f.floor();
            let i = x0 as isize;
            let clamp = |i: isize| i.clamp(0, n_src as isize - 1) as usize;
            (clamp(i), clamp(i + 1), f - x0)
        })
        .collect()
}

/// Expand to `w × h` (bilinear, pixel-centre aligned with [`down`]'s grid).
pub fn up(b: &Buf, w: usize, h: usize) -> Buf {
    let mut out = Buf::new(w, h, b.c);
    up_apply(b, &mut out, |o, v| *o = v);
    out
}

/// `g −= up(coarse, g.w, g.h)`: the band of a Laplacian pyramid, without the expanded level.
pub fn sub_up(g: &mut Buf, coarse: &Buf) {
    up_apply(coarse, g, |o, v| *o -= v);
}

/// [`up`]'s value at every pixel of `out` (`out.w × out.h`), handed to `f` with the pixel's channel.
fn up_apply(b: &Buf, out: &mut Buf, f: impl Fn(&mut f32, f32) + Sync) {
    let c = b.c;
    if b.w == 0 || b.h == 0 || c == 0 || out.c != c {
        return;
    }
    let xs = up_taps(b.w, out.w);
    let ys = up_taps(b.h, out.h);
    let src_len = b.w * c;
    out.data.par_chunks_mut(out.w * c).zip(&ys).for_each(|(row, &(y0, y1, ty))| {
        let (r0, r1) = (&b.data[y0 * src_len..(y0 + 1) * src_len], &b.data[y1 * src_len..(y1 + 1) * src_len]);
        for (x, &(x0, x1, tx)) in xs.iter().enumerate() {
            for ch in 0..c {
                let a = r0[x0 * c + ch];
                let bb = r0[x1 * c + ch];
                let cc = r1[x0 * c + ch];
                let d = r1[x1 * c + ch];
                let top = a + (bb - a) * tx;
                let bot = cc + (d - cc) * tx;
                f(&mut row[x * c + ch], top + (bot - top) * ty);
            }
        }
    });
}

/// Fill pixels with weight 0 from their neighbourhood (push–pull: weighted pyramid down, fill
/// from coarser levels up). `vals` has `c` channels; `wgt` one.
pub fn push_pull(vals: &mut Buf, wgt: &[f32]) {
    if wgt.iter().all(|&w| w > 0.0) || !wgt.iter().any(|&w| w > 0.0) {
        return;
    }
    let (w, h, c) = (vals.w, vals.h, vals.c);
    // premultiplied values + weight as the last channel
    let mut pm = Buf::new(w, h, c + 1);
    for i in 0..w * h {
        let a = wgt[i].clamp(0.0, 1.0);
        for ch in 0..c {
            pm.data[i * (c + 1) + ch] = vals.data[i * c + ch] * a;
        }
        pm.data[i * (c + 1) + c] = a;
    }
    // the finer levels, and the coarsest (1 × 1) one
    let (mut levels, mut l) = (Vec::new(), pm);
    while l.w > 1 || l.h > 1 {
        let d = down(&l);
        levels.push(std::mem::replace(&mut l, d));
    }
    // normalise the coarsest, then fill each finer level's holes from the coarser one
    let mut filled = {
        let mut f = Buf::new(l.w, l.h, c);
        for i in 0..l.w * l.h {
            let a = l.data[i * (c + 1) + c];
            for ch in 0..c {
                f.data[i * c + ch] = if a > 1e-12 { l.data[i * (c + 1) + ch] / a } else { 0.0 };
            }
        }
        f
    };
    for l in levels.iter().rev() {
        let coarse = up(&filled, l.w, l.h);
        let mut f = Buf::new(l.w, l.h, c);
        for i in 0..l.w * l.h {
            let a = l.data[i * (c + 1) + c].min(1.0);
            for ch in 0..c {
                let v = if a > 1e-12 { l.data[i * (c + 1) + ch] / l.data[i * (c + 1) + c] } else { 0.0 };
                f.data[i * c + ch] = v * a + coarse.data[i * c + ch] * (1.0 - a);
            }
        }
        filled = f;
    }
    for i in 0..w * h {
        if wgt[i] <= 0.0 {
            for ch in 0..c {
                vals.data[i * c + ch] = filled.data[i * c + ch];
            }
        }
    }
}

/// Accumulates tiles into the canvas Laplacian pyramid.
pub struct Blender {
    pub levels: usize,
    pub w: usize,
    pub h: usize,
    acc: Vec<Buf>,
    wsum: Vec<Buf>,
}

impl Blender {
    /// A blender for a `w × h` canvas (both multiples of 2^levels).
    pub fn new(w: usize, h: usize, levels: usize) -> Blender {
        let mut acc = Vec::new();
        let mut wsum = Vec::new();
        for l in 0..=levels {
            acc.push(Buf::new(w >> l, h >> l, 3));
            wsum.push(Buf::new(w >> l, h >> l, 1));
        }
        Blender { levels, w, h, acc, wsum }
    }

    /// Add a tile at canvas offset (`x0`, `y0`) (multiples of 2^levels; tile size too): `vals`
    /// (3 channels, holes already filled) and its seam mask (0..1).
    pub fn add(&mut self, x0: usize, y0: usize, vals: Buf, mask: Buf) {
        let mut g = vals;
        let mut m = mask;
        for l in 0..=self.levels {
            let (ox, oy) = (x0 >> l, y0 >> l);
            let (gn, mn) = if l < self.levels { (Some(down(&g)), Some(down(&m))) } else { (None, None) };
            let lap = match &gn {
                Some(gn) => {
                    let u = up(gn, g.w, g.h);
                    let mut lap = g.clone();
                    for (a, b) in lap.data.iter_mut().zip(&u.data) {
                        *a -= b;
                    }
                    lap
                }
                None => g.clone(),
            };
            let (acc, ws) = (&mut self.acc[l], &mut self.wsum[l]);
            let aw = acc.w;
            let tw = lap.w;
            acc.data.par_chunks_mut(aw * 3).zip(ws.data.par_chunks_mut(aw)).enumerate().for_each(|(y, (arow, wrow))| {
                if y < oy || y >= oy + lap.h {
                    return;
                }
                let ty = y - oy;
                for tx in 0..tw {
                    let x = ox + tx;
                    if x >= aw {
                        break;
                    }
                    let mk = m.data[ty * tw + tx];
                    if mk <= 0.0 {
                        continue;
                    }
                    for ch in 0..3 {
                        arow[x * 3 + ch] += lap.data[(ty * tw + tx) * 3 + ch] * mk;
                    }
                    wrow[x] += mk;
                }
            });
            if let (Some(gn), Some(mn)) = (gn, mn) {
                g = gn;
                m = mn;
            }
        }
    }

    /// Normalise and collapse the pyramid into the canvas.
    pub fn finish(self) -> Buf {
        let mut res: Option<Buf> = None;
        for l in (0..=self.levels).rev() {
            let mut b = self.acc[l].clone();
            let ws = &self.wsum[l];
            for i in 0..b.w * b.h {
                let s = ws.data[i];
                for ch in 0..3 {
                    b.data[i * 3 + ch] = if s > 1e-8 { b.data[i * 3 + ch] / s } else { 0.0 };
                }
            }
            if let Some(r) = res {
                let u = up(&r, b.w, b.h);
                for (a, v) in b.data.iter_mut().zip(&u.data) {
                    *a += v;
                }
            }
            res = Some(b);
        }
        // `0..=levels` always yields level 0
        res.unwrap_or_else(|| Buf::new(self.w, self.h, 3))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The edge-clamped formulas `down` and `up` implement, one tap at a time.
    fn at(b: &Buf, x: isize, y: isize, ch: usize) -> f32 {
        let x = x.clamp(0, b.w as isize - 1) as usize;
        let y = y.clamp(0, b.h as isize - 1) as usize;
        b.data[(y * b.w + x) * b.c + ch]
    }

    #[test]
    fn down_and_up_match_their_definitions_exactly() {
        for (w, h, c) in [(37, 23, 3), (8, 5, 1), (1, 1, 4), (2, 9, 2), (64, 48, 3)] {
            let mut b = Buf::new(w, h, c);
            for (i, x) in b.data.iter_mut().enumerate() {
                *x = ((i * 7919) % 1013) as f32 / 1013.0 - 0.3;
            }
            let d = down(&b);
            let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
            assert_eq!((d.w, d.h, d.c), (w2, h2, c));
            for y in 0..h2 {
                for x in 0..w2 {
                    for ch in 0..c {
                        let mut s = 0.0;
                        for (k, wk) in K.iter().enumerate() {
                            let mut t = 0.0;
                            for (j, wj) in K.iter().enumerate() {
                                t += wj * at(&b, 2 * x as isize + j as isize - 2, (2 * y as isize + k as isize - 2).clamp(0, h as isize - 1), ch);
                            }
                            s += wk * t;
                        }
                        assert_eq!(d.data[(y * w2 + x) * c + ch].to_bits(), f32::to_bits(s), "down {w}×{h}×{c} at {x},{y}");
                    }
                }
            }
            let u = up(&d, w, h);
            let mut g = b.clone();
            sub_up(&mut g, &d);
            for y in 0..h {
                let fy = (y as f32 + 0.5) / 2.0 - 0.5;
                let (y0, ty) = (fy.floor(), fy - fy.floor());
                for x in 0..w {
                    let fx = (x as f32 + 0.5) / 2.0 - 0.5;
                    let (x0, tx) = (fx.floor(), fx - fx.floor());
                    let (x0, y0) = (x0 as isize, y0 as isize);
                    for ch in 0..c {
                        let top = at(&d, x0, y0, ch) + (at(&d, x0 + 1, y0, ch) - at(&d, x0, y0, ch)) * tx;
                        let bot = at(&d, x0, y0 + 1, ch) + (at(&d, x0 + 1, y0 + 1, ch) - at(&d, x0, y0 + 1, ch)) * tx;
                        let v = top + (bot - top) * ty;
                        let i = (y * w + x) * c + ch;
                        assert_eq!(u.data[i].to_bits(), v.to_bits(), "up {w}×{h}×{c} at {x},{y}");
                        assert_eq!(g.data[i].to_bits(), (b.data[i] - v).to_bits(), "sub_up {w}×{h}×{c} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn single_tile_reconstructs_exactly() {
        let (w, h) = (64, 48);
        let mut v = Buf::new(w, h, 3);
        for (i, x) in v.data.iter_mut().enumerate() {
            *x = ((i * 37) % 101) as f32 / 101.0;
        }
        let mut m = Buf::new(w, h, 1);
        m.data.fill(1.0);
        let mut b = Blender::new(w, h, 3);
        b.add(0, 0, v.clone(), m);
        let out = b.finish();
        for (a, b) in v.data.iter().zip(&out.data) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn two_tiles_blend_without_a_step() {
        // left tile value 0, right tile value 1, seam in the middle: the result is a smooth ramp
        let (w, h) = (128, 32);
        let mut b = Blender::new(w, h, 4);
        for (val, side) in [(0.0f32, 0), (1.0, 1)] {
            let mut v = Buf::new(w, h, 3);
            v.data.fill(val);
            let mut m = Buf::new(w, h, 1);
            for y in 0..h {
                for x in 0..w {
                    m.data[y * w + x] = if (x >= w / 2) == (side == 1) { 1.0 } else { 0.0 };
                }
            }
            b.add(0, 0, v, m);
        }
        let out = b.finish();
        let row: Vec<f32> = (0..w).map(|x| out.data[(16 * w + x) * 3]).collect();
        let max_step = row.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.15, "max step {max_step}");
        assert!(row[2].abs() < 0.02 && (row[w - 3] - 1.0).abs() < 0.02);
    }

    #[test]
    fn push_pull_fills_holes() {
        let (w, h) = (20, 10);
        let mut v = Buf::new(w, h, 1);
        let mut wg = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w / 2 {
                v.data[y * w + x] = 0.7;
                wg[y * w + x] = 1.0;
            }
        }
        push_pull(&mut v, &wg);
        assert!(v.data.iter().all(|x| (x - 0.7).abs() < 1e-4));
    }
}
