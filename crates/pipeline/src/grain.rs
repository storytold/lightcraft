//! Experimental signed-particle shot noise, NOT the Boolean occupancy reference.
//! Jittered continuous particle positions, Gaussian footprints and analytic Gaussian preview
//! filtering. The spatial cells only index particles; no grid-value interpolation is used.
use lightcraft_develop::Grain;

const BASE_VARIANCE: f32 = 0.8 * 0.8 + 0.65 * 0.65 / 4.0;

pub(crate) struct Particles {
    scale: f64,
    aperture: f32,
    rough: f32,
    extent: f64,
    seed: u32,
    gain: f32,
}

fn mix(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

fn uniform(h: u32) -> f32 {
    (h >> 8) as f32 * (1.0 / 16777216.0)
}

impl Particles {
    pub(crate) fn new(g: &Grain, native_long: f64, px_per_long: f64) -> Option<Self> {
        if !g.amount.is_finite() || g.amount <= 0.0 {
            return None;
        }
        let size = if g.size.is_finite() { g.size.clamp(0.0, 100.0) } else { 25.0 };
        let scale = (0.25 + size * 0.016) / 0.65;
        let pitch = native_long / px_per_long.max(1e-6) / scale;
        // Smoothly discard sub-display precision texture at extreme reductions, bounding work
        // independently of native source size. At pitch >= 32 no particles are evaluated.
        let fade = ((32.0 - pitch) / 8.0).clamp(0.0, 1.0) as f32;
        if fade == 0.0 || !pitch.is_finite() {
            return None;
        }
        let rough = if g.roughness.is_finite() { g.roughness.clamp(0.0, 100.0) as f32 / 100.0 } else { 0.0 };
        // A disk's per-axis second moment is r²/4. This matches that moment, not its
        // occupancy covariance. Only additional reduction beyond native pitch adds aperture.
        let aperture = ((pitch * pitch - 1.0).max(0.0) / 12.0) as f32;
        let extent = 3.5 * (BASE_VARIANCE * (1.0 + 0.5 * rough).powi(2) + aperture).sqrt() as f64;
        Some(Self { scale, aperture, rough, extent, seed: g.seed, gain: (g.amount.clamp(0.0, 100.0) / 100.0) as f32 * 0.1 * fade })
    }

    /// Absolute oriented source coordinates, scaled from preview dimensions to original pixels.
    pub(crate) fn residual(&self, x: f64, y: f64, lum: f32) -> f32 {
        let (x, y) = (x / self.scale, y / self.scale);
        // Input coordinates are bounded by the frame; explicit limits also keep integer loop
        // arithmetic safe for malformed settings. No allocation or input-sized particle cache.
        if !x.is_finite() || !y.is_finite() || x.abs() > 1e12 || y.abs() > 1e12 {
            return 0.0;
        }
        let mut sum = 0.0;
        let (x0, x1) = (((x - self.extent) / 2.0).floor() as i64, ((x + self.extent) / 2.0).floor() as i64);
        let (y0, y1) = (((y - self.extent) / 2.0).floor() as i64, ((y + self.extent) / 2.0).floor() as i64);
        for j in y0..=y1 {
            for i in x0..=x1 {
                let h = mix((i as u32).wrapping_mul(0x8da6_b343) ^ (j as u32).wrapping_mul(0xd816_3841) ^ mix(self.seed));
                let a = mix(h ^ 0xa511_e9b3);
                let b = mix(h ^ 0x63d8_3595);
                let dx = (x - (i as f64 + uniform(h) as f64) * 2.0) as f32;
                let dy = (y - (j as f64 + uniform(a) as f64) * 2.0) as f32;
                let radius = 1.0 + self.rough * (uniform(b) - 0.5);
                let variance = BASE_VARIANCE * radius * radius + self.aperture;
                let t = (dx * dx + dy * dy) / (2.0 * variance);
                if t <= 6.125 {
                    let sign = if b & 1 == 0 { -1.0 } else { 1.0 };
                    sum += sign * (-t).exp() / (std::f32::consts::TAU * variance);
                }
            }
        }
        let u = lum.clamp(0.0, 1.0);
        self.gain * (4.0 * u * (1.0 - u)).sqrt() * sum
    }
}
