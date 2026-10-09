//! Test-only ARW6 synthetic encoder pieces: forward 5/3 transforms (exact inverse of `llvc::inverse_*`) and `quantize`.

use crate::llvc::{Bands3, Plane, band_rows};

/// Exact inverse of `dequant` on representable values: `sign(v) * (|v| >> qi)`.
pub(crate) fn quantize(v: i32, qi: u32) -> i32 {
    v.signum() * (v.abs() >> qi)
}

fn at(v: &[i32], i: isize) -> i32 {
    if v.is_empty() { 0 } else { v[i.clamp(0, v.len() as isize - 1) as usize] }
}

/// Forward 1-D 5/3 with whole-sample symmetric extension (`plan/arw6/scratch/fwd.py::fwd1d`).
fn forward_1d(x: &[i32], phase: u8) -> (Vec<i32>, Vec<i32>) {
    let n = x.len() as isize;
    let p = isize::from(phase);
    let xs = |i: isize| -> i32 {
        let r = if i < 0 {
            -i
        } else if i >= n {
            2 * (n - 1) - i
        } else {
            i
        };
        if (0..n).contains(&r) { x[r as usize] } else { 0 }
    };
    let (nl, nh) = band_rows(x.len(), phase);
    let h: Vec<i32> = (0..nh as isize)
        .map(|k| {
            let pos = 2 * k + 1 - p;
            xs(pos) - ((xs(pos - 1) + xs(pos + 1)) >> 1)
        })
        .collect();
    let l = (0..nl as isize).map(|j| xs(2 * j + p) + ((at(&h, j - 1 + p) + at(&h, j + p) + 2) >> 2)).collect();
    (l, h)
}

fn split(x: &Plane, phase: u8, vertical: bool) -> (Plane, Plane) {
    let (across, n) = if vertical { (x.width, x.height) } else { (x.height, x.width) };
    let (nl, nh) = band_rows(n, phase);
    let dims = |m| if vertical { (across, m) } else { (m, across) };
    let (mut lo, mut hi) = (Plane::zeros(dims(nl).0, dims(nl).1), Plane::zeros(dims(nh).0, dims(nh).1));
    for i in 0..across {
        let line: Vec<i32> = (0..n).map(|k| if vertical { x.data[k * x.width + i] } else { x.data[i * x.width + k] }).collect();
        let (l, h) = forward_1d(&line, phase);
        for (p, v) in [(&mut lo, l), (&mut hi, h)] {
            for (k, v) in v.into_iter().enumerate() {
                let idx = if vertical { k * p.width + i } else { i * p.width + k };
                p.data[idx] = v;
            }
        }
    }
    (lo, hi)
}

pub(crate) fn forward_53_1d_rows(x: &Plane, phase: u8) -> (Plane, Plane) {
    split(x, phase, true)
}

pub(crate) fn forward_53_1d_cols(x: &Plane, phase: u8) -> (Plane, Plane) {
    split(x, phase, false)
}

/// Horizontal first (phase 0), then vertical; returns `(ll, lh, hl, hh)`, the exact inverse of `inverse_53_2d`.
pub(crate) fn forward_53_2d(x: &Plane, vphase: u8) -> (Plane, Plane, Plane, Plane) {
    let (lo, hi) = forward_53_1d_cols(x, 0);
    let (ll, lh) = forward_53_1d_rows(&lo, vphase);
    let (hl, hh) = forward_53_1d_rows(&hi, vphase);
    (ll, lh, hl, hh)
}

/// 3-level forward, the exact inverse of `reconstruct3` with the same `phases`.
pub(crate) fn forward3(x: &Plane, phases: [u8; 3]) -> Bands3 {
    let (ll1, lh1, hl1, hh1) = forward_53_2d(x, phases[0]);
    let (ll2, lh2, hl2, hh2) = forward_53_2d(&ll1, phases[1]);
    let (ll3, lh3, hl3, hh3) = forward_53_2d(&ll2, phases[2]);
    Bands3 { ll3, hl3, lh3, hh3, hl2, lh2, hh2, hl1, lh1, hh1 }
}
