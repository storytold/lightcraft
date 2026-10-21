//! Seam leveling: what one gain (and flare offset) per frame leaves at the seams.
//!
//! Every frame has its own slow brightness and colour changes: the sky's gradient towards the sun,
//! veiling flare that grows towards it, residual vignetting. A single gain per frame can't line those
//! up, and multi-band blending switches between frames within a couple of hundred pixels, so each seam
//! shows as a band in a smooth sky (Kopf et al. 2007 report the same "slight radiometric mismatches
//! visible in smooth regions such as sky"). Gradient-domain stitching (Levin et al., GIST, 2004;
//! Agarwala 2007) removes such low-frequency differences by matching gradients across the seams. Here
//! the same is done for the low frequencies only, on a coarse grid:
//!
//! 1. The canvas is cut into cells of [`cell_size`] pixels. Per frame and cell: the mean of the
//!    frame's (exposure-compensated) colour where it has pixels, in log space.
//! 2. At every cell where frame `i`'s region meets frame `j`'s and both frames cover the cell, frame
//!    `i` is to move by half the log difference `(Lⱼ − Lᵢ)/2` (and `j` by the other half, from its side),
//!    so both meet in the middle. Differences are held to ±[`MAX_STEP`].
//! 3. Each frame's correction is spread over the rest of its cells by a membrane (Laplace) solve with
//!    those cells fixed: smooth, and zero change where nothing asks for one. Frames with no seam get none.
//! 4. The correction is read bilinearly at every pixel and added in log space before the blend.
//!
//! The pyramid blend keeps the fine detail and the sharp seams; only the slow brightness changes are
//! leveled, a few milliseconds even for a 500 MP canvas (the grid has ≤ [`GRID`] cells along an edge).

use super::Tile;

/// Cells along the long edge of the canvas (at most).
pub(crate) const GRID: usize = 384;
/// Largest log difference evened out at a seam (a factor of e^0.7 ≈ 2): beyond it the frames don't
/// show the same thing there (a moving cloud, a misaligned edge).
pub(crate) const MAX_STEP: f32 = 0.7;

/// Side of a cell (canvas pixels) for a `pw × ph` canvas.
pub(crate) fn cell_size(pw: usize, ph: usize) -> usize {
    pw.max(ph).div_ceil(GRID).max(4)
}

/// Per-frame log corrections on the cell grid.
pub(crate) struct Leveling {
    cell: usize,
    gw: usize,
    gh: usize,
    /// Per frame, per cell, per channel; empty for a frame without a correction.
    u: Vec<Vec<[f32; 3]>>,
}

impl Leveling {
    /// The log correction of frame `i` at canvas pixel (`x`, `y`), bilinear between cell centres.
    pub(crate) fn at(&self, i: usize, x: usize, y: usize) -> [f32; 3] {
        let Some(u) = self.u.get(i).filter(|u| !u.is_empty()) else { return [0.0; 3] };
        let c = self.cell as f32;
        let fx = ((x as f32 + 0.5) / c - 0.5).clamp(0.0, (self.gw - 1) as f32);
        let fy = ((y as f32 + 0.5) / c - 0.5).clamp(0.0, (self.gh - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.gw - 1), (y0 + 1).min(self.gh - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x: usize, y: usize| u.get(y * self.gw + x).copied().unwrap_or([0.0; 3]);
        let (a, b, cc, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bot = cc[k] + (d[k] - cc[k]) * tx;
            top + (bot - top) * ty
        })
    }
}

/// Seam leveling of the frames in `tiles` (canvas `pw × ph`, seams in `label`), their pixels as the
/// blend will take them: `colour(i, rgb)` (linear, exposure-compensated).
pub(crate) fn level(tiles: &[Option<Tile>], label: &[u16], pw: usize, ph: usize, colour: impl Fn(usize, [f32; 3]) -> [f32; 3]) -> Leveling {
    let n = tiles.len();
    let cell = cell_size(pw, ph);
    let (gw, gh) = (pw.div_ceil(cell), ph.div_ceil(cell));
    let cells = gw * gh;
    let none = Leveling { cell, gw, gh, u: vec![Vec::new(); n] };
    if cells == 0 || label.len() < pw * ph {
        return none;
    }
    // 1. per frame and cell: mean log colour where the frame has pixels (None without enough of them)
    let means: Vec<Vec<Option<[f32; 3]>>> = tiles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let Some(t) = t else { return vec![None; cells] };
            let mut sum = vec![[0f64; 3]; cells];
            let mut cnt = vec![0usize; cells];
            for ty in 0..t.h {
                let cy = (t.y0 + ty) / cell;
                for tx in 0..t.w {
                    let k = ty * t.w + tx;
                    if t.wgt.get(k).copied().unwrap_or(0.0) <= 0.0 {
                        continue;
                    }
                    let Some(&p) = t.rgb.get(k) else { continue };
                    let c = cy * gw + (t.x0 + tx) / cell;
                    let v = colour(i, p);
                    if let (Some(s), Some(m)) = (sum.get_mut(c), cnt.get_mut(c)) {
                        for ch in 0..3 {
                            s[ch] += v[ch].max(1e-6) as f64;
                        }
                        *m += 1;
                    }
                }
            }
            let need = (cell * cell / 4).max(1);
            sum.iter().zip(&cnt).map(|(s, &m)| (m >= need).then(|| s.map(|v| ((v / m as f64) as f32).ln()))).collect()
        })
        .collect();
    // the frame each cell's centre belongs to
    let owner: Vec<Option<usize>> = (0..cells)
        .map(|c| {
            let (x, y) = (((c % gw) * cell + cell / 2).min(pw - 1), ((c / gw) * cell + cell / 2).min(ph - 1));
            label.get(y * pw + x).copied().filter(|&l| l != u16::MAX).map(usize::from)
        })
        .collect();
    let u = (0..n)
        .map(|i| {
            // 2. fixed cells: frame i's cells next to another frame's, both frames covering the cell
            let mut fixed: Vec<Option<[f32; 3]>> = vec![None; cells];
            let mut any = false;
            for c in 0..cells {
                if owner[c] != Some(i) {
                    continue;
                }
                let Some(li) = means[i][c] else { continue };
                let (x, y) = (c % gw, c / gw);
                let mut acc = [0f32; 3];
                let mut k = 0;
                for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= gw as i64 || ny >= gh as i64 {
                        continue;
                    }
                    let Some(j) = owner[ny as usize * gw + nx as usize].filter(|&j| j != i) else { continue };
                    let Some(lj) = means.get(j).and_then(|m| m[c]) else { continue };
                    for ch in 0..3 {
                        acc[ch] += ((lj[ch] - li[ch]) / 2.0).clamp(-MAX_STEP / 2.0, MAX_STEP / 2.0);
                    }
                    k += 1;
                }
                if k > 0 {
                    fixed[c] = Some(acc.map(|v| v / k as f32));
                    any = true;
                }
            }
            if !any {
                return Vec::new();
            }
            // 3. membrane over the frame's cells, the fixed ones held
            let inside: Vec<bool> = (0..cells).map(|c| means[i][c].is_some()).collect();
            membrane(&fixed, &inside, gw, gh)
        })
        .collect();
    Leveling { cell, gw, gh, u }
}

/// Solve Δu = 0 over the `inside` cells with `fixed` values held (Neumann at the region's edges):
/// successive over-relaxation from the mean of the fixed values.
fn membrane(fixed: &[Option<[f32; 3]>], inside: &[bool], gw: usize, gh: usize) -> Vec<[f32; 3]> {
    let cells = gw * gh;
    let (mut sum, mut k) = ([0f32; 3], 0f32);
    for v in fixed.iter().flatten() {
        for ch in 0..3 {
            sum[ch] += v[ch];
        }
        k += 1.0;
    }
    let start = sum.map(|v| if k > 0.0 { v / k } else { 0.0 });
    let mut u: Vec<[f32; 3]> = (0..cells).map(|c| fixed[c].unwrap_or(start)).collect();
    let omega = 1.9f32;
    for _ in 0..4 * (gw + gh) {
        let mut change = 0f32;
        for c in 0..cells {
            if fixed[c].is_some() || !inside[c] {
                continue;
            }
            let (x, y) = (c % gw, c / gw);
            let mut acc = [0f32; 3];
            let mut m = 0f32;
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= gw as i64 || ny >= gh as i64 {
                    continue;
                }
                let nc = ny as usize * gw + nx as usize;
                if !inside[nc] {
                    continue;
                }
                for ch in 0..3 {
                    acc[ch] += u[nc][ch];
                }
                m += 1.0;
            }
            if m == 0.0 {
                continue;
            }
            for ch in 0..3 {
                let target = acc[ch] / m;
                let d = omega * (target - u[c][ch]);
                u[c][ch] += d;
                change = change.max(d.abs());
            }
        }
        if change < 1e-4 {
            break;
        }
    }
    // cells the frame doesn't cover take nothing (they are never read for its pixels anyway)
    for c in 0..cells {
        if !inside[c] {
            u[c] = [0.0; 3];
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two frames over a smooth sky, the right one 30 % brighter and with a gradient of its own: the
    /// corrected frames agree at the seam, and far from it each is left nearly as it was.
    #[test]
    fn frames_meet_at_the_seam() {
        let (pw, ph) = (1024usize, 256usize);
        let sky = |x: usize| 0.3 + 0.2 * x as f32 / pw as f32;
        let a =
            Tile { x0: 0, y0: 0, w: 640, h: ph, rgb: (0..640 * ph).map(|k| [sky(k % 640); 3]).collect(), wgt: vec![1.0; 640 * ph], r2: Vec::new() };
        let b = Tile {
            x0: 384,
            y0: 0,
            w: 640,
            h: ph,
            rgb: (0..640 * ph).map(|k| [sky(384 + k % 640) * 1.3 * (1.0 + 0.2 * (k % 640) as f32 / 640.0); 3]).collect(),
            wgt: vec![1.0; 640 * ph],
            r2: Vec::new(),
        };
        let label: Vec<u16> = (0..pw * ph).map(|k| if k % pw < 512 { 0 } else { 1 }).collect();
        let lv = level(&[Some(a), Some(b)], &label, pw, ph, |_, p| p);
        let col = |i: usize, x: usize| {
            let raw = if i == 0 { sky(x) } else { sky(x) * 1.3 * (1.0 + 0.2 * (x - 384) as f32 / 640.0) };
            raw.ln() + lv.at(i, x, 128)[1]
        };
        let step = (col(0, 511) - col(1, 512)).abs();
        assert!(step < 0.01, "log step at the seam {step}");
        // before: ln(1.3 · 1.04) ≈ 0.30
        assert!(lv.at(0, 20, 128)[1].abs() < 0.2 && lv.at(1, 1000, 128)[1].abs() < 0.2);
    }

    #[test]
    fn nothing_to_level_without_a_seam() {
        let t = Tile { x0: 0, y0: 0, w: 64, h: 64, rgb: vec![[0.5; 3]; 64 * 64], wgt: vec![1.0; 64 * 64], r2: Vec::new() };
        let lv = level(&[Some(t), None], &vec![0u16; 64 * 64], 64, 64, |_, p| p);
        assert_eq!(lv.at(0, 10, 10), [0.0; 3]);
        assert_eq!(lv.at(1, 10, 10), [0.0; 3]);
        let empty = level(&[], &[], 0, 0, |_, p| p);
        assert_eq!(empty.at(0, 0, 0), [0.0; 3]);
    }
}
