//! Ratio Corrected Demosaicing (RCD, Luis Sanz Rodríguez, v2.3).
//!
//! Translated from RawTherapee's `rtengine/rcd_demosaic.cc` (GPL-3.0-or-later; the same algorithm ships as
//! darktable's `src/iop/demosaicing/rcd.c`). Structure and constants follow that source; the tiling, SSE paths and
//! 16-bit scaling are left out, borders use mirror reflection (which keeps Bayer parity) instead of a separate
//! border interpolation, and divisions are guarded so hostile samples (negative, NaN, infinite) cannot divide by
//! zero.
//!
//! 1. Vertical / horizontal discrimination from squared 1-D high-pass responses of the mosaic, summed over three
//!    pixels across the direction (`vh`), and the same along the diagonals (`pq`).
//! 2. A 3 × 3 low-pass of the mosaic at red/blue sites (it mixes all three colours: a luminance estimate).
//! 3. Green at red/blue: the four cardinal greens, each scaled by the ratio of the low-pass at the site and at the
//!    next same-colour site, weighted by inverse gradients and blended by the vertical/horizontal discrimination
//!    (taken from the neighbourhood when that is more decided than the pixel itself).
//! 4. Red/blue at blue/red sites by diagonal colour differences, then red and blue at green sites by cardinal
//!    colour differences, both weighted the same way.

use super::{Mosaic, reflect};
use crate::Rgb32f;
use lcc_raster::par_rows;

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

#[inline]
fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

/// `n / d`, or `fallback` when `d` is not a usable divisor (zero, negative from hostile data, or not finite).
#[inline]
fn div(n: f32, d: f32, fallback: f32) -> f32 {
    if d > 0.0 && d.is_finite() { n / d } else { fallback }
}

/// A full plane with mirror-reflected reads.
struct Plane<'a> {
    w: usize,
    h: usize,
    d: &'a [f32],
}

impl Plane<'_> {
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        self.d.get(reflect(y, self.h) * self.w + reflect(x, self.w)).copied().unwrap_or(0.0)
    }
}

/// Direction strength `a / (a + b)` from per-pixel squared high-pass responses along `(dx, dy)` and across it.
fn discrimination(m: &Mosaic, (ax, ay): (isize, isize), (bx, by): (isize, isize)) -> Vec<f32> {
    let (w, h) = (m.w, m.h);
    let hp = |x: isize, y: isize, dx: isize, dy: isize| {
        let c = |k: isize| m.get(x + k * dx, y + k * dy);
        let v = (c(-3) - c(-1) - c(1) + c(3)) - 3.0 * (c(-2) + c(2)) + 6.0 * c(0);
        v * v
    };
    let mut buf_a = vec![0f32; w * h];
    let mut buf_b = vec![0f32; w * h];
    par_rows(&mut buf_a, w, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = hp(x as isize, y as isize, ax, ay);
        }
    });
    par_rows(&mut buf_b, w, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = hp(x as isize, y as isize, bx, by);
        }
    });
    let (pa, pb) = (Plane { w, h, d: &buf_a }, Plane { w, h, d: &buf_b });
    let mut out = vec![0.5f32; w * h];
    par_rows(&mut out, w, |y, row| {
        let y = y as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let x = x as isize;
            // summed across the direction: the neighbours one step along it
            let sa = (pa.at(x - ax, y - ay) + pa.at(x, y) + pa.at(x + ax, y + ay)).max(EPSSQ);
            let sb = (pb.at(x - bx, y - by) + pb.at(x, y) + pb.at(x + bx, y + by)).max(EPSSQ);
            *o = div(sa, sa + sb, 0.5);
        }
    });
    out
}

/// The discrimination at a site, or the mean of its four diagonal neighbours when that is further from 0.5.
#[inline]
fn disc(p: &Plane, x: isize, y: isize) -> f32 {
    let c = p.at(x, y);
    let n = 0.25 * (p.at(x - 1, y - 1) + p.at(x + 1, y - 1) + p.at(x - 1, y + 1) + p.at(x + 1, y + 1));
    if (0.5 - c).abs() < (0.5 - n).abs() { n } else { c }
}

pub(crate) fn rcd(m: &Mosaic) -> Rgb32f {
    let (w, h) = (m.w, m.h);
    // Step 1: V/H (along y vs along x) and P/Q (diagonals) discrimination
    let vh_buf = discrimination(m, (0, 1), (1, 0));
    let pq_buf = discrimination(m, (1, 1), (1, -1));
    let vh = Plane { w, h, d: &vh_buf };
    let pq = Plane { w, h, d: &pq_buf };

    // Step 2: low-pass at red/blue sites
    let mut lpf_buf = vec![0f32; w * h];
    par_rows(&mut lpf_buf, w, |y, row| {
        let y = y as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let x = x as isize;
            if m.color(x, y) == 1 {
                continue;
            }
            let g = |dx, dy| m.get(x + dx, y + dy);
            *o = 0.25 * g(0, 0) + 0.125 * (g(0, -1) + g(0, 1) + g(-1, 0) + g(1, 0)) + 0.0625 * (g(-1, -1) + g(1, -1) + g(-1, 1) + g(1, 1));
        }
    });
    let lpf = Plane { w, h, d: &lpf_buf };

    // Step 3: green at red/blue sites
    let mut green = vec![0f32; w * h];
    par_rows(&mut green, w, |y, row| {
        let y = y as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let x = x as isize;
            let c0 = m.get(x, y);
            if m.color(x, y) == 1 {
                *o = c0;
                continue;
            }
            let c = |dx, dy| m.get(x + dx, y + dy);
            let n_grad = EPS + (c(0, -1) - c(0, 1)).abs() + (c0 - c(0, -2)).abs() + (c(0, -1) - c(0, -3)).abs() + (c(0, -2) - c(0, -4)).abs();
            let s_grad = EPS + (c(0, -1) - c(0, 1)).abs() + (c0 - c(0, 2)).abs() + (c(0, 1) - c(0, 3)).abs() + (c(0, 2) - c(0, 4)).abs();
            let w_grad = EPS + (c(-1, 0) - c(1, 0)).abs() + (c0 - c(-2, 0)).abs() + (c(-1, 0) - c(-3, 0)).abs() + (c(-2, 0) - c(-4, 0)).abs();
            let e_grad = EPS + (c(-1, 0) - c(1, 0)).abs() + (c0 - c(2, 0)).abs() + (c(1, 0) - c(3, 0)).abs() + (c(2, 0) - c(4, 0)).abs();
            let l0 = lpf.at(x, y);
            let est = |g: f32, dx: isize, dy: isize| {
                let l = lpf.at(x + dx, y + dy);
                g * (1.0 + div(l0 - l, EPS + l0 + l, 0.0))
            };
            let (n_est, s_est) = (est(c(0, -1), 0, -2), est(c(0, 1), 0, 2));
            let (w_est, e_est) = (est(c(-1, 0), -2, 0), est(c(1, 0), 2, 0));
            let v_est = div(s_grad * n_est + n_grad * s_est, n_grad + s_grad, 0.5 * (n_est + s_est));
            let h_est = div(w_grad * e_est + e_grad * w_est, e_grad + w_grad, 0.5 * (w_est + e_est));
            *o = intp(disc(&vh, x, y), h_est, v_est);
        }
    });
    let g = Plane { w, h, d: &green };

    // Step 4.1: own samples; Step 4.2: red/blue at blue/red sites from diagonal colour differences
    let mut rb = vec![[0f32; 2]; w * h];
    par_rows(&mut rb, w, |y, row| {
        let y = y as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let x = x as isize;
            let own = m.color(x, y);
            if own == 1 {
                continue;
            }
            let v = |dx, dy| m.get(x + dx, y + dy);
            let gg = |dx, dy| g.at(x + dx, y + dy);
            let g0 = gg(0, 0);
            let nw_grad = EPS + (v(-1, -1) - v(1, 1)).abs() + (v(-1, -1) - v(-3, -3)).abs() + (g0 - gg(-2, -2)).abs();
            let ne_grad = EPS + (v(1, -1) - v(-1, 1)).abs() + (v(1, -1) - v(3, -3)).abs() + (g0 - gg(2, -2)).abs();
            let sw_grad = EPS + (v(1, -1) - v(-1, 1)).abs() + (v(-1, 1) - v(-3, 3)).abs() + (g0 - gg(-2, 2)).abs();
            let se_grad = EPS + (v(-1, -1) - v(1, 1)).abs() + (v(1, 1) - v(3, 3)).abs() + (g0 - gg(2, 2)).abs();
            let d = |dx, dy| v(dx, dy) - gg(dx, dy);
            let (nw, ne, sw, se) = (d(-1, -1), d(1, -1), d(-1, 1), d(1, 1));
            let p_est = div(nw_grad * se + se_grad * nw, nw_grad + se_grad, 0.5 * (nw + se));
            let q_est = div(ne_grad * sw + sw_grad * ne, ne_grad + sw_grad, 0.5 * (ne + sw));
            let val = g0 + intp(disc(&pq, x, y), q_est, p_est);
            let own_v = v(0, 0);
            *o = if own == 0 { [own_v, val] } else { [val, own_v] };
        }
    });

    // Step 4.3: red and blue at green sites from cardinal colour differences
    let rb_at = |x: isize, y: isize, i: usize| rb.get(reflect(y, h) * w + reflect(x, w)).map_or(0.0, |p| p[i]);
    let mut out = Rgb32f::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        let y = y as isize;
        for (x, px) in row.iter_mut().enumerate() {
            let x = x as isize;
            let g0 = g.at(x, y);
            px[1] = g0;
            if m.color(x, y) != 1 {
                px[0] = rb_at(x, y, 0);
                px[2] = rb_at(x, y, 1);
                continue;
            }
            let dv = disc(&vh, x, y);
            for (i, ch) in [(0usize, 0usize), (1, 2)] {
                let c = |dx, dy| rb_at(x + dx, y + dy, i);
                let gg = |dx, dy| g.at(x + dx, y + dy);
                let n_grad = EPS + (g0 - gg(0, -2)).abs() + (c(0, -1) - c(0, 1)).abs() + (c(0, -1) - c(0, -3)).abs();
                let s_grad = EPS + (g0 - gg(0, 2)).abs() + (c(0, 1) - c(0, -1)).abs() + (c(0, 1) - c(0, 3)).abs();
                let w_grad = EPS + (g0 - gg(-2, 0)).abs() + (c(-1, 0) - c(1, 0)).abs() + (c(-1, 0) - c(-3, 0)).abs();
                let e_grad = EPS + (g0 - gg(2, 0)).abs() + (c(1, 0) - c(-1, 0)).abs() + (c(1, 0) - c(3, 0)).abs();
                let d = |dx, dy| c(dx, dy) - gg(dx, dy);
                let (n, s, wv, e) = (d(0, -1), d(0, 1), d(-1, 0), d(1, 0));
                let v_est = div(n_grad * s + s_grad * n, n_grad + s_grad, 0.5 * (n + s));
                let h_est = div(e_grad * wv + w_grad * e, e_grad + w_grad, 0.5 * (wv + e));
                px[ch] = g0 + intp(dv, h_est, v_est);
            }
        }
    });
    out
}
