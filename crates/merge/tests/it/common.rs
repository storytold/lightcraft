//! Helpers shared by the merge crate's integration tests.
#![allow(dead_code)]

use lightcraft_geom::{Homography, Point};

/// Deterministic noise: xorshift64 and Box–Muller.
pub struct Rng(pub u64);

impl Rng {
    pub fn gauss(&mut self) -> f32 {
        let mut u = || {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64).max(1e-12)
        };
        let (a, b) = (u(), u());
        ((-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()) as f32
    }
}

/// The largest distance between where `a` and `b` map the corners of a `w × h` frame.
pub fn corner_error(a: &Homography, b: &Homography, w: f64, h: f64) -> f64 {
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().map(|&(x, y)| a.apply(Point::new(x, y)).dist(b.apply(Point::new(x, y)))).fold(0.0, f64::max)
}
