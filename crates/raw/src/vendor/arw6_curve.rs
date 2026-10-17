//! The ARW6 companding curve: 12-bit tile code to output value in 2 × 14-bit DN (black 1024, white 32766).
//!
//! Piecewise linear through knots (c_k, 128 k): c_k = 128 k for k <= 11, else 356.89 + 655.646 ln(k - 6); each code's
//! value is the interpolation rounded half up. Our own measurement from the CC0 lossless / ARW6 pairs of the ILCE-7RM6,
//! not stored in the file; see docs/arw6-compression.md (*Output range*).

use std::sync::OnceLock;

/// Code of knot `k`, whose output value is 128 k.
fn knot(k: u32) -> f64 {
    if k <= 11 { f64::from(128 * k) } else { 356.89 + 655.646 * f64::from(k - 6).ln() }
}

fn table() -> &'static [u16; 4096] {
    static TABLE: OnceLock<[u16; 4096]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u16; 4096];
        let mut k = 0u32;
        for (code, v) in t.iter_mut().enumerate() {
            let c = code as f64;
            while knot(k + 1) < c {
                k += 1;
            }
            let (c0, c1, u0) = (knot(k), knot(k + 1), f64::from(128 * k));
            *v = (u0 + 128.0 * (c - c0) / (c1 - c0) + 0.5).floor() as u16;
        }
        t
    })
}

/// Companding curve: 12-bit code (clamped to 0..=4095) to output value.
pub(crate) fn curve(code12: i32) -> u16 {
    table().get(code12.clamp(0, 4095) as usize).copied().unwrap_or(u16::MAX)
}
