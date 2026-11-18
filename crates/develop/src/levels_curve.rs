//! The midtone curve of the Levels tool.
//!
//! For a signed midtone `m` (-1 to +1, 0 neutral) the curve maps the input `t` (0 to 1, after the black and
//! white points) to the output before the output levels. It is *not* a gamma: it has finite slopes at both ends.
//! The curves were measured from Capture One with a synthetic chart, at thirteen values of `m`
//! (`levels_table`, aggregate calibration numbers); a curve for any `m` is interpolated between them, at each of the
//! 257 samples, with a monotone cubic in `m` (so a curve never overshoots the ones it lies between). Outside 0..1 the
//! curve continues with its end slopes, which is what the measured curves show below the black point.
//!
//! The marker is the input that comes out as 0.5: it runs from about 0.72 (midtone -1) through 0.5 (neutral) to
//! about 0.25 (midtone +1), and flattens toward both ends.

use crate::levels_table::{MIDTONE_CURVES, MIDTONE_KNOTS, MIDTONE_M, MIDTONE_S0, MIDTONE_S1};

/// Samples in a [`MidtoneTable`] (`t = j / 256`).
pub const MIDTONE_SAMPLES: usize = MIDTONE_KNOTS;

/// One midtone curve: its samples over 0..1 and the slopes it continues with beyond.
#[derive(Clone, Debug, PartialEq)]
pub struct MidtoneTable {
    pub y: [f32; MIDTONE_SAMPLES],
    /// Slope at 0, used below 0.
    pub s0: f32,
    /// Slope at 1, used above 1.
    pub s1: f32,
}

/// Monotone cubic (Fritsch-Carlson) through the thirteen measured values `ys` at [`MIDTONE_M`], at `x`.
fn interp_m(ys: &[f64; 13], x: f64) -> f64 {
    let xs = &MIDTONE_M;
    let mut h = [0.0f64; 12];
    let mut d = [0.0f64; 12];
    for i in 0..12 {
        h[i] = xs[i + 1] - xs[i];
        d[i] = (ys[i + 1] - ys[i]) / h[i];
    }
    let mut s = [0.0f64; 13];
    s[0] = d[0];
    s[12] = d[11];
    for i in 1..12 {
        if d[i - 1] * d[i] <= 0.0 {
            s[i] = 0.0;
        } else {
            let (w1, w2) = (2.0 * h[i] + h[i - 1], h[i] + 2.0 * h[i - 1]);
            s[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
        }
    }
    let mut j = 0;
    while j < 11 && x > xs[j + 1] {
        j += 1;
    }
    let u = (x - xs[j]) / h[j];
    let (u2, u3) = (u * u, u * u * u);
    (2.0 * u3 - 3.0 * u2 + 1.0) * ys[j] + (u3 - 2.0 * u2 + u) * h[j] * s[j] + (-2.0 * u3 + 3.0 * u2) * ys[j + 1] + (u3 - u2) * h[j] * s[j + 1]
}

impl MidtoneTable {
    /// The curve for the signed midtone `m` (limited to -1..+1; not a number: 0).
    pub fn new(m: f64) -> MidtoneTable {
        let m = if m.is_finite() { m.clamp(-1.0, 1.0) } else { 0.0 };
        let mut y = [0.0f32; MIDTONE_SAMPLES];
        let mut run = 0.0f64;
        for (j, out) in y.iter_mut().enumerate() {
            let mut col = [0.0f64; 13];
            for (c, curve) in col.iter_mut().zip(MIDTONE_CURVES.iter()) {
                *c = f64::from(curve[j]);
            }
            // (never below the sample before, never above 1: the curve stays monotone)
            run = interp_m(&col, m).clamp(run, 1.0);
            *out = run as f32;
        }
        y[0] = 0.0;
        y[MIDTONE_SAMPLES - 1] = 1.0;
        let slope = |v: &[f32; 13]| {
            let mut col = [0.0f64; 13];
            for (c, s) in col.iter_mut().zip(v.iter()) {
                *c = f64::from(*s);
            }
            interp_m(&col, m).max(0.0) as f32
        };
        MidtoneTable { y, s0: slope(&MIDTONE_S0), s1: slope(&MIDTONE_S1) }
    }

    /// The curve at `t`: between the samples straight lines, beyond 0 and 1 the end slopes.
    #[inline]
    pub fn eval(&self, t: f32) -> f32 {
        if t <= 0.0 {
            self.s0 * t
        } else if t >= 1.0 {
            1.0 + self.s1 * (t - 1.0)
        } else {
            let f = t * (MIDTONE_SAMPLES - 1) as f32;
            let i = (f as usize).min(MIDTONE_SAMPLES - 2);
            let fr = f - i as f32;
            let (a, b) = (self.y.get(i).copied().unwrap_or(0.0), self.y.get(i + 1).copied().unwrap_or(1.0));
            a + (b - a) * fr
        }
    }

    /// [`Self::eval`] in double precision on a `t` within 0..1 (for the marker).
    fn eval64(&self, t: f64) -> f64 {
        let f = t.clamp(0.0, 1.0) * (MIDTONE_SAMPLES - 1) as f64;
        let i = (f as usize).min(MIDTONE_SAMPLES - 2);
        let fr = f - i as f64;
        let (a, b) = (self.y.get(i).copied().unwrap_or(0.0), self.y.get(i + 1).copied().unwrap_or(1.0));
        f64::from(a) + (f64::from(b) - f64::from(a)) * fr
    }
}

/// Where the marker of the signed midtone `m` sits, as a fraction of the way from the black point to the white point:
/// the input at which the curve reaches 0.5. 0.5 for 0; smaller for a positive midtone (toward the shadows).
pub fn marker(m: f64) -> f64 {
    if m == 0.0 || !m.is_finite() {
        return 0.5;
    }
    let table = MidtoneTable::new(m);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if table.eval64(mid) < 0.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// The signed midtone whose marker is at `p` (a fraction of the way from black to white): the inverse of [`marker`].
/// A position the marker cannot reach gives -1 or +1.
pub fn from_marker(p: f64) -> f64 {
    if !p.is_finite() {
        return 0.0;
    }
    if p == 0.5 {
        return 0.0;
    }
    if p <= marker(1.0) {
        return 1.0;
    }
    if p >= marker(-1.0) {
        return -1.0;
    }
    // the marker only moves one way as the midtone grows: toward black
    let (mut a, mut b) = (-1.0f64, 1.0f64);
    for _ in 0..60 {
        let mid = (a + b) / 2.0;
        if marker(mid) > p {
            a = mid;
        } else {
            b = mid;
        }
    }
    (a + b) / 2.0
}
