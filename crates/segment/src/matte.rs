//! Clean-up for people mattes made on a coarse grid (Selfie Multiclass's 256 × 256, SAM 2.1's
//! silhouettes): enclosed specks filled ([`fill_small_holes`]), each person's region kept to the
//! part joined to their face ([`clean_owners`]), and edges pulled onto the photo's own edges by a
//! guided filter ([`guided_filter`], He, Sun and Tang, "Guided Image Filtering", 2010/2013).
//!
//! Original LightCraft code; plain Rust on slices.

use std::collections::VecDeque;

/// No one's cell in an owner grid.
pub const NOBODY: u16 = u16::MAX;

/// Fill the holes of a `side × side` coverage grid that the outside can't reach (cells under
/// 0.5 enclosed by cells at 0.5 or more) and that are at most `max_cells` large: the specks a
/// coarse class map leaves in lace, sequins or a bouquet held in front. Larger enclosed gaps (the
/// space between an arm and the body) stay. Filled cells take 1.0.
pub fn fill_small_holes(cov: &mut [f32], side: usize, max_cells: usize) {
    let n = side.saturating_mul(side);
    if side == 0 || cov.len() < n {
        return;
    }
    let low = |c: f32| c.is_nan() || c < 0.5;
    // 0 unvisited, 1 outside, 2 visited hole
    let mut seen = vec![0u8; n];
    let mut q = VecDeque::new();
    for i in 0..side {
        for idx in [i, (side - 1) * side + i, i * side, i * side + side - 1] {
            if low(cov[idx]) && seen[idx] == 0 {
                seen[idx] = 1;
                q.push_back(idx);
            }
        }
    }
    flood(&mut q, &mut seen, side, 1, |j| low(cov[j]));
    let mut comp = Vec::new();
    for start in 0..n {
        if seen[start] != 0 || !low(cov[start]) {
            continue;
        }
        comp.clear();
        seen[start] = 2;
        q.push_back(start);
        while let Some(i) = q.pop_front() {
            comp.push(i);
            for j in neighbours(i, side) {
                if seen[j] == 0 && low(cov[j]) {
                    seen[j] = 2;
                    q.push_back(j);
                }
            }
        }
        if comp.len() <= max_cells {
            for &i in &comp {
                cov[i] = 1.0;
            }
        }
    }
}

fn neighbours(i: usize, side: usize) -> impl Iterator<Item = usize> {
    let (x, y) = (i % side, i / side);
    [(x > 0).then(|| i - 1), (x + 1 < side).then(|| i + 1), (y > 0).then(|| i - side), (y + 1 < side).then(|| i + side)].into_iter().flatten()
}

fn flood(q: &mut VecDeque<usize>, seen: &mut [u8], side: usize, mark: u8, open: impl Fn(usize) -> bool) {
    while let Some(i) = q.pop_front() {
        for j in neighbours(i, side) {
            if seen[j] == 0 && open(j) {
                seen[j] = mark;
                q.push_back(j);
            }
        }
    }
}

/// An owner grid cleaned up: each person keeps only the cells of theirs joined to their face cell
/// (`seeds`, one per person, `None` when off the grid); islands they claimed elsewhere (a SAM
/// silhouette leaking onto a neighbour's lace dress) become [`NOBODY`], and then every unowned
/// cell between one person's cells (two or more of its four neighbours theirs, none anyone
/// else's) is given to them, a few passes over.
pub fn clean_owners(owner: &mut [u16], side: usize, seeds: &[Option<usize>]) {
    let n = side.saturating_mul(side);
    if side == 0 || owner.len() < n {
        return;
    }
    let mut keep = vec![false; n];
    let mut q = VecDeque::new();
    for (k, seed) in seeds.iter().enumerate() {
        let Some(s) = *seed else { continue };
        let Ok(k) = u16::try_from(k) else { break };
        if s >= n || owner[s] != k || keep[s] {
            continue;
        }
        keep[s] = true;
        q.push_back(s);
        while let Some(i) = q.pop_front() {
            for j in neighbours(i, side) {
                if !keep[j] && owner[j] == k {
                    keep[j] = true;
                    q.push_back(j);
                }
            }
        }
    }
    for (o, k) in owner.iter_mut().zip(&keep) {
        // a person without a seed on the grid keeps what they have
        let seeded = usize::from(*o) < seeds.len() && seeds.get(usize::from(*o)).is_some_and(Option::is_some);
        if *o != NOBODY && seeded && !*k {
            *o = NOBODY;
        }
    }
    for _ in 0..4 {
        let snapshot = owner.to_vec();
        let mut changed = false;
        for i in 0..n {
            if snapshot[i] != NOBODY {
                continue;
            }
            // one person's cells on at least two sides and no one else's
            let mut who = NOBODY;
            let mut count = 0;
            let mut mixed = false;
            for j in neighbours(i, side) {
                let o = snapshot[j];
                if o == NOBODY {
                    continue;
                }
                if who == NOBODY || who == o {
                    who = o;
                    count += 1;
                } else {
                    mixed = true;
                }
            }
            if !mixed && count >= 2 {
                owner[i] = who;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Box sums of a `w × h` grid over `(2r + 1)²` windows (clipped at the edges), divided by the
/// window's cell count: a mean filter.
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut integral = vec![0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += f64::from(v.get(y * w + x).copied().unwrap_or(0.0));
            integral[(y + 1) * (w + 1) + x + 1] = integral[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let s = integral[y1 * (w + 1) + x1] - integral[y0 * (w + 1) + x1] - integral[y1 * (w + 1) + x0] + integral[y0 * (w + 1) + x0];
            let cells = ((x1 - x0) * (y1 - y0)).max(1) as f64;
            out[y * w + x] = (s / cells) as f32;
        }
    }
    out
}

/// The guided filter of `p` (a `w × h` coverage) with the grey guide `g` (same size, 0..1),
/// radius `r` cells and regulariser `eps`: where the guide has an edge the result follows it,
/// elsewhere it is a smooth version of `p`. Clamped to 0..1. Inputs of the wrong size come back
/// unchanged.
pub fn guided_filter(p: &[f32], g: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let n = w.saturating_mul(h);
    if n == 0 || p.len() != n || g.len() != n || r == 0 {
        return p.to_vec();
    }
    let mean_g = box_mean(g, w, h, r);
    let mean_p = box_mean(p, w, h, r);
    let gp: Vec<f32> = g.iter().zip(p).map(|(a, b)| a * b).collect();
    let gg: Vec<f32> = g.iter().map(|a| a * a).collect();
    let corr_gp = box_mean(&gp, w, h, r);
    let corr_gg = box_mean(&gg, w, h, r);
    let mut a = vec![0f32; n];
    let mut b = vec![0f32; n];
    for i in 0..n {
        let var = corr_gg[i] - mean_g[i] * mean_g[i];
        let cov = corr_gp[i] - mean_g[i] * mean_p[i];
        a[i] = cov / (var + eps);
        b[i] = mean_p[i] - a[i] * mean_g[i];
    }
    let (ma, mb) = (box_mean(&a, w, h, r), box_mean(&b, w, h, r));
    (0..n)
        .map(|i| {
            let v = ma[i] * g[i] + mb[i];
            if v.is_finite() { v.clamp(0.0, 1.0) } else { p[i] }
        })
        .collect()
}

/// [`guided_filter`] with a colour guide (`g`: `w × h` RGB, 0..1, interleaved): an edge in colour
/// alone — a white dress against pale skin of the same lightness — is followed too (He, Sun and
/// Tang's colour guided filter: a 3 × 3 covariance per window). Inputs of the wrong size come back
/// unchanged.
pub fn guided_filter_rgb(p: &[f32], g: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let n = w.saturating_mul(h);
    if n == 0 || p.len() != n || g.len() != n.saturating_mul(3) || r == 0 {
        return p.to_vec();
    }
    let ch = |c: usize| -> Vec<f32> { g.iter().skip(c).step_by(3).copied().collect() };
    let gc = [ch(0), ch(1), ch(2)];
    let mean_g: Vec<Vec<f32>> = gc.iter().map(|c| box_mean(c, w, h, r)).collect();
    let mean_p = box_mean(p, w, h, r);
    let corr_gp: Vec<Vec<f32>> = gc.iter().map(|c| box_mean(&c.iter().zip(p).map(|(a, b)| a * b).collect::<Vec<_>>(), w, h, r)).collect();
    // the six distinct products of the colour channels
    let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
    let corr_gg: Vec<Vec<f32>> =
        pairs.iter().map(|(i, j)| box_mean(&gc[*i].iter().zip(&gc[*j]).map(|(a, b)| a * b).collect::<Vec<_>>(), w, h, r)).collect();
    let mut a = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    let mut b = vec![0f32; n];
    for k in 0..n {
        let m = [mean_g[0][k], mean_g[1][k], mean_g[2][k]];
        let s = |q: usize, i: usize, j: usize| corr_gg[q][k] - m[i] * m[j] + if i == j { eps } else { 0.0 };
        let (s00, s01, s02, s11, s12, s22) = (s(0, 0, 0), s(1, 0, 1), s(2, 0, 2), s(3, 1, 1), s(4, 1, 2), s(5, 2, 2));
        let cov = [corr_gp[0][k] - m[0] * mean_p[k], corr_gp[1][k] - m[1] * mean_p[k], corr_gp[2][k] - m[2] * mean_p[k]];
        // the inverse of the symmetric 3 × 3 matrix, by cofactors
        let (c00, c01, c02) = (s11 * s22 - s12 * s12, s02 * s12 - s01 * s22, s01 * s12 - s02 * s11);
        let det = s00 * c00 + s01 * c01 + s02 * c02;
        let (ak, bk) = if det.abs() > 1e-12 && det.is_finite() {
            let (c11, c12, c22) = (s00 * s22 - s02 * s02, s01 * s02 - s00 * s12, s00 * s11 - s01 * s01);
            let x = [
                (c00 * cov[0] + c01 * cov[1] + c02 * cov[2]) / det,
                (c01 * cov[0] + c11 * cov[1] + c12 * cov[2]) / det,
                (c02 * cov[0] + c12 * cov[1] + c22 * cov[2]) / det,
            ];
            (x, mean_p[k] - x[0] * m[0] - x[1] * m[1] - x[2] * m[2])
        } else {
            ([0.0; 3], mean_p[k])
        };
        for (c, v) in a.iter_mut().zip(ak) {
            c[k] = v;
        }
        b[k] = bk;
    }
    let ma: Vec<Vec<f32>> = a.iter().map(|v| box_mean(v, w, h, r)).collect();
    let mb = box_mean(&b, w, h, r);
    (0..n)
        .map(|k| {
            let v = ma[0][k] * gc[0][k] + ma[1][k] * gc[1][k] + ma[2][k] * gc[2][k] + mb[k];
            if v.is_finite() { v.clamp(0.0, 1.0) } else { p[k] }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two colours of the same lightness side by side (pale skin, a dress tinted the other way):
    /// the colour guide sees the edge and pulls the soft edge onto it.
    #[test]
    fn the_colour_guided_filter_follows_an_edge_in_colour_alone() {
        let (w, h) = (40usize, 8usize);
        let g: Vec<f32> = (0..w * h).flat_map(|i| if i % w < 20 { [0.8, 0.6, 0.5] } else { [0.5, 0.6, 0.8] }).collect();
        let p: Vec<f32> = (0..w * h).map(|i| (((i % w) as f32 - 14.0) / 12.0).clamp(0.0, 1.0)).collect();
        let q = guided_filter_rgb(&p, &g, w, h, 4, 1e-4);
        let at = |v: &[f32], x: usize| v[3 * w + x];
        assert!(at(&q, 18) < at(&p, 18) - 0.05, "{} {}", at(&q, 18), at(&p, 18));
        assert!(at(&q, 22) > at(&p, 22) + 0.05, "{} {}", at(&q, 22), at(&p, 22));
        assert!(q.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(guided_filter_rgb(&p, &g[..10], w, h, 4, 1e-3), p);
        // a flat guide: a smooth copy, no NaN
        let flat = vec![0.5f32; w * h * 3];
        assert!(guided_filter_rgb(&p, &flat, w, h, 4, 1e-4).iter().all(|v| v.is_finite()));
    }

    /// A ring of coverage with a speck missing inside a blob, and a gap open to the outside.
    #[test]
    fn small_enclosed_holes_fill_and_open_gaps_stay() {
        let side = 20;
        let mut cov = vec![0f32; side * side];
        for y in 2..18 {
            for x in 2..18 {
                cov[y * side + x] = 1.0;
            }
        }
        // a 2 × 2 speck inside, a 6 × 6 hole inside, a notch open to the outside
        for (x, y) in [(5, 5), (6, 5), (5, 6), (6, 6)] {
            cov[y * side + x] = 0.0;
        }
        for y in 9..15 {
            for x in 9..15 {
                cov[y * side + x] = 0.0;
            }
        }
        for x in 0..4 {
            cov[3 * side + x] = 0.0;
        }
        fill_small_holes(&mut cov, side, 10);
        assert_eq!(cov[5 * side + 5], 1.0, "the speck is filled");
        assert_eq!(cov[11 * side + 11], 0.0, "a large hole stays");
        assert_eq!(cov[3 * side + 3], 0.0, "a notch open to the outside stays");
        // junk sizes never panic
        fill_small_holes(&mut cov, 0, 10);
        fill_small_holes(&mut [0.0; 3], 5, 10);
    }

    /// A neighbour's claim inside a person's region becomes theirs; the neighbour keeps their own.
    #[test]
    fn islands_go_to_the_surrounding_person() {
        let side = 12;
        let mut owner = vec![NOBODY; side * side];
        for y in 0..side {
            for x in 0..side {
                owner[y * side + x] = if x < 6 { 0 } else { 1 };
            }
        }
        // person 1 claims a 2 × 2 island inside person 0's half
        for (x, y) in [(2, 8), (3, 8), (2, 9), (3, 9)] {
            owner[y * side + x] = 1;
        }
        clean_owners(&mut owner, side, &[Some(2 * side + 2), Some(2 * side + 9)]);
        assert_eq!(owner[8 * side + 2], 0, "the island is person 0's");
        assert_eq!(owner[8 * side + 9], 1, "person 1 keeps their own");
        clean_owners(&mut owner, side, &[Some(usize::MAX), None]);
        clean_owners(&mut [], 4, &[]);
    }

    /// The filter pulls a soft edge onto the guide's sharp one and leaves flat areas alone.
    #[test]
    fn guided_filter_follows_the_guides_edge() {
        let (w, h) = (40usize, 8usize);
        let g: Vec<f32> = (0..w * h).map(|i| if i % w < 20 { 0.1 } else { 0.9 }).collect();
        // coverage ramps over cells 14..26
        let p: Vec<f32> = (0..w * h).map(|i| (((i % w) as f32 - 14.0) / 12.0).clamp(0.0, 1.0)).collect();
        let q = guided_filter(&p, &g, w, h, 4, 1e-3);
        let at = |v: &[f32], x: usize| v[3 * w + x];
        assert!(at(&q, 18) < at(&p, 18), "{} {}", at(&q, 18), at(&p, 18));
        assert!(at(&q, 22) > at(&p, 22), "{} {}", at(&q, 22), at(&p, 22));
        assert!(q.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(guided_filter(&p, &g[..10], w, h, 4, 1e-3), p);
    }
}
