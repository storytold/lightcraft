//! Separable resampling with precomputed filter weights.

use crate::{Image, par_rows};

/// A resampling kernel. Interpolating kernels (`Nearest`, `Bilinear`, `CatmullRom`, `Lanczos*`,
/// `Spline*`) pass through the source samples when enlarging; the others smooth a little.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    /// Nearest neighbour: one source pixel per output pixel (hard pixel edges, aliasing).
    Nearest,
    /// Box / area average.
    Box,
    /// Triangle (linear interpolation; a tent average when shrinking).
    Bilinear,
    /// Hermite cubic (smooth, no overshoot).
    Hermite,
    /// Catmull-Rom cubic (B = 0, C = ½): the usual "bicubic", sharp with mild halos.
    CatmullRom,
    /// Mitchell–Netravali cubic (B = C = ⅓): balanced sharpness and ringing.
    Mitchell,
    /// Cubic B-spline (B = 1, C = 0): very smooth, never rings.
    BSpline,
    /// Gaussian (σ = ½ pixel, support 2).
    Gaussian,
    /// Lanczos, 2 lobes.
    Lanczos2,
    /// Lanczos, 3 lobes.
    Lanczos3,
    /// Lanczos, 4 lobes (sharpest, most ringing).
    Lanczos4,
    /// Spline16 (support 2): close to Lanczos2 with less ringing.
    Spline16,
    /// Spline36 (support 3): close to Lanczos3 with less ringing.
    Spline36,
}

impl Filter {
    /// Every filter, in the order a picker lists them.
    pub const ALL: [Filter; 13] = [
        Filter::Nearest,
        Filter::Box,
        Filter::Bilinear,
        Filter::Hermite,
        Filter::CatmullRom,
        Filter::Mitchell,
        Filter::BSpline,
        Filter::Gaussian,
        Filter::Lanczos2,
        Filter::Lanczos3,
        Filter::Lanczos4,
        Filter::Spline16,
        Filter::Spline36,
    ];

    /// Stable id (`lanczos3`, `catmullRom`, …), as commands and settings files spell it.
    pub fn id(self) -> &'static str {
        match self {
            Filter::Nearest => "nearest",
            Filter::Box => "box",
            Filter::Bilinear => "bilinear",
            Filter::Hermite => "hermite",
            Filter::CatmullRom => "catmullRom",
            Filter::Mitchell => "mitchell",
            Filter::BSpline => "bSpline",
            Filter::Gaussian => "gaussian",
            Filter::Lanczos2 => "lanczos2",
            Filter::Lanczos3 => "lanczos3",
            Filter::Lanczos4 => "lanczos4",
            Filter::Spline16 => "spline16",
            Filter::Spline36 => "spline36",
        }
    }

    /// Human name for a picker.
    pub fn label(self) -> &'static str {
        match self {
            Filter::Nearest => "Nearest Neighbor",
            Filter::Box => "Box (Area Average)",
            Filter::Bilinear => "Bilinear",
            Filter::Hermite => "Hermite",
            Filter::CatmullRom => "Bicubic (Catmull-Rom)",
            Filter::Mitchell => "Mitchell-Netravali",
            Filter::BSpline => "B-Spline",
            Filter::Gaussian => "Gaussian",
            Filter::Lanczos2 => "Lanczos 2",
            Filter::Lanczos3 => "Lanczos 3",
            Filter::Lanczos4 => "Lanczos 4",
            Filter::Spline16 => "Spline16",
            Filter::Spline36 => "Spline36",
        }
    }

    /// Parse an [`Filter::id`] (any case; also a few common aliases: `bicubic`, `cubic`, `linear`,
    /// `triangle`, `area`, `point`, `lanczos`).
    pub fn parse(s: &str) -> Option<Filter> {
        let k: String = s.trim().chars().filter(|c| !matches!(c, '-' | '_' | ' ')).flat_map(char::to_lowercase).collect();
        Some(match k.as_str() {
            "nearest" | "nearestneighbor" | "nearestneighbour" | "point" => Filter::Nearest,
            "box" | "area" => Filter::Box,
            "bilinear" | "linear" | "triangle" | "tent" => Filter::Bilinear,
            "hermite" => Filter::Hermite,
            "catmullrom" | "bicubic" | "cubic" => Filter::CatmullRom,
            "mitchell" | "mitchellnetravali" => Filter::Mitchell,
            "bspline" | "cubicbspline" => Filter::BSpline,
            "gaussian" | "gauss" => Filter::Gaussian,
            "lanczos2" => Filter::Lanczos2,
            "lanczos3" | "lanczos" => Filter::Lanczos3,
            "lanczos4" => Filter::Lanczos4,
            "spline16" => Filter::Spline16,
            "spline36" => Filter::Spline36,
            _ => return None,
        })
    }

    /// Kernel radius in source pixels at scale 1.
    pub fn support(self) -> f32 {
        match self {
            Filter::Nearest | Filter::Box => 0.5,
            Filter::Bilinear | Filter::Hermite => 1.0,
            Filter::CatmullRom | Filter::Mitchell | Filter::BSpline | Filter::Gaussian | Filter::Lanczos2 | Filter::Spline16 => 2.0,
            Filter::Lanczos3 | Filter::Spline36 => 3.0,
            Filter::Lanczos4 => 4.0,
        }
    }

    /// Kernel value at distance `x` (source pixels at scale 1).
    pub fn eval(self, x: f32) -> f32 {
        let x = x.abs();
        // Keys' family of cubics, parameterised by B and C (Mitchell & Netravali, 1988)
        let bc = |b: f32, c: f32| {
            if x < 1.0 {
                ((12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b)) / 6.0
            } else if x < 2.0 {
                ((-b - 6.0 * c) * x * x * x + (6.0 * b + 30.0 * c) * x * x + (-12.0 * b - 48.0 * c) * x + (8.0 * b + 24.0 * c)) / 6.0
            } else {
                0.0
            }
        };
        let lanczos = |a: f32| {
            if x < 1e-6 {
                1.0
            } else if x < a {
                let px = std::f32::consts::PI * x;
                a * px.sin() * (px / a).sin() / (px * px)
            } else {
                0.0
            }
        };
        match self {
            Filter::Nearest | Filter::Box => {
                if x <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Filter::Bilinear => (1.0 - x).max(0.0),
            Filter::Hermite => {
                if x < 1.0 {
                    (2.0 * x - 3.0) * x * x + 1.0
                } else {
                    0.0
                }
            }
            Filter::CatmullRom => bc(0.0, 0.5),
            Filter::Mitchell => bc(1.0 / 3.0, 1.0 / 3.0),
            Filter::BSpline => bc(1.0, 0.0),
            Filter::Gaussian => {
                if x < 2.0 {
                    // σ = ½: exp(-x² / 2σ²) = exp(-2x²)
                    (-2.0 * x * x).exp()
                } else {
                    0.0
                }
            }
            Filter::Lanczos2 => lanczos(2.0),
            Filter::Lanczos3 => lanczos(3.0),
            Filter::Lanczos4 => lanczos(4.0),
            // piecewise cubic interpolating splines fitted for minimum ringing (Panorama Tools)
            Filter::Spline16 => {
                if x < 1.0 {
                    ((x - 9.0 / 5.0) * x - 1.0 / 5.0) * x + 1.0
                } else if x < 2.0 {
                    let t = x - 1.0;
                    ((-1.0 / 3.0 * t + 4.0 / 5.0) * t - 7.0 / 15.0) * t
                } else {
                    0.0
                }
            }
            Filter::Spline36 => {
                if x < 1.0 {
                    ((13.0 / 11.0 * x - 453.0 / 209.0) * x - 3.0 / 209.0) * x + 1.0
                } else if x < 2.0 {
                    let t = x - 1.0;
                    ((-6.0 / 11.0 * t + 270.0 / 209.0) * t - 156.0 / 209.0) * t
                } else if x < 3.0 {
                    let t = x - 2.0;
                    ((1.0 / 11.0 * t - 45.0 / 209.0) * t + 26.0 / 209.0) * t
                } else {
                    0.0
                }
            }
        }
    }
}

/// Contributions for each output coordinate: (first source index, weights) — the exact taps
/// [`resize`] uses (GPU kernels upload these tables).
pub fn weights(src: usize, dst: usize, filter: Filter) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f32 / dst as f32;
    let fscale = scale.max(1.0);
    let support = filter.support() * fscale;
    if filter == Filter::Nearest {
        // one tap: the source pixel under the output pixel's centre, whatever the scale
        return (0..dst).map(|i| ((((i as f32 + 0.5) * scale) as usize).min(src.saturating_sub(1)), vec![1.0])).collect();
    }
    (0..dst)
        .map(|i| {
            let center = (i as f32 + 0.5) * scale;
            let lo = ((center - support).floor() as isize).max(0) as usize;
            let hi = ((center + support).ceil() as usize).min(src);
            let mut w: Vec<f32> = (lo..hi).map(|j| filter.eval((j as f32 + 0.5 - center) / fscale)).collect();
            let sum: f32 = w.iter().sum();
            if sum.abs() > 1e-8 {
                w.iter_mut().for_each(|v| *v /= sum);
            } else if !w.is_empty() {
                let n = w.len() as f32;
                w.iter_mut().for_each(|v| *v = 1.0 / n);
            }
            (lo, w)
        })
        .collect()
}

/// Pixel types that can be resampled (linear combination).
pub trait Pixel: Copy + Default + Send + Sync {
    fn zero() -> Self;
    fn madd(self, o: Self, w: f32) -> Self;
}

impl Pixel for f32 {
    fn zero() -> Self {
        0.0
    }
    #[inline]
    fn madd(self, o: Self, w: f32) -> Self {
        self + o * w
    }
}

impl Pixel for [f32; 3] {
    fn zero() -> Self {
        [0.0; 3]
    }
    #[inline]
    fn madd(self, o: Self, w: f32) -> Self {
        [self[0] + o[0] * w, self[1] + o[1] * w, self[2] + o[2] * w]
    }
}

impl Pixel for [f32; 4] {
    fn zero() -> Self {
        [0.0; 4]
    }
    #[inline]
    fn madd(self, o: Self, w: f32) -> Self {
        [self[0] + o[0] * w, self[1] + o[1] * w, self[2] + o[2] * w, self[3] + o[3] * w]
    }
}

/// Resize to `w × h` (separable; horizontal pass then vertical pass).
pub fn resize<T: Pixel>(img: &Image<T>, w: usize, h: usize, filter: Filter) -> Image<T> {
    let (w, h) = (w.max(1), h.max(1));
    if img.width == w && img.height == h {
        return img.clone();
    }
    let wx = weights(img.width, w, filter);
    let mut tmp = Image::<T>::new(w, img.height);
    par_rows(&mut tmp.data, w, |y, row| {
        let src = img.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let (lo, ws) = &wx[x];
            let mut acc = T::zero();
            for (k, wt) in ws.iter().enumerate() {
                acc = acc.madd(src[lo + k], *wt);
            }
            *o = acc;
        }
    });
    let wy = weights(img.height, h, filter);
    let mut out = Image::<T>::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        let (lo, ws) = &wy[y];
        for (k, wt) in ws.iter().enumerate() {
            let src = tmp.row(lo + k);
            for (o, s) in row.iter_mut().zip(src) {
                *o = o.madd(*s, *wt);
            }
        }
    });
    out
}

/// Fit within `max_w × max_h` preserving aspect (never upscales).
pub fn fit<T: Pixel>(img: &Image<T>, max_w: usize, max_h: usize, filter: Filter) -> Image<T> {
    let s = (max_w as f64 / img.width as f64).min(max_h as f64 / img.height as f64).min(1.0);
    let w = ((img.width as f64 * s).round() as usize).max(1);
    let h = ((img.height as f64 * s).round() as usize).max(1);
    resize(img, w, h, filter)
}

/// 2× box downsample (pyramid level).
pub fn half<T: Pixel>(img: &Image<T>) -> Image<T> {
    let w = img.width.div_ceil(2).max(1);
    let h = img.height.div_ceil(2).max(1);
    let mut out = Image::<T>::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let (sx, sy) = (2 * x as isize, 2 * y as isize);
            *o = T::zero()
                .madd(img.get_clamped(sx, sy), 0.25)
                .madd(img.get_clamped(sx + 1, sy), 0.25)
                .madd(img.get_clamped(sx, sy + 1), 0.25)
                .madd(img.get_clamped(sx + 1, sy + 1), 0.25);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_image_stays_constant() {
        let img = Image::<f32>::filled(37, 21, 0.42);
        for f in Filter::ALL {
            for (w, h) in [(10, 7), (80, 40), (37, 21), (1, 1)] {
                let r = resize(&img, w, h, f);
                assert!(r.data.iter().all(|v| (v - 0.42).abs() < 1e-5), "{f:?} {w}x{h}");
            }
        }
    }

    #[test]
    fn downscale_preserves_mean() {
        let img = Image::<f32>::from_fn(64, 64, |x, y| ((x * 7 + y * 3) % 11) as f32 / 10.0);
        let mean = img.data.iter().sum::<f32>() / img.len() as f32;
        let r = resize(&img, 16, 16, Filter::Lanczos3);
        let m2 = r.data.iter().sum::<f32>() / r.len() as f32;
        assert!((mean - m2).abs() < 0.01);
        let hh = half(&img);
        assert_eq!((hh.width, hh.height), (32, 32));
        let m3 = hh.data.iter().sum::<f32>() / hh.len() as f32;
        assert!((mean - m3).abs() < 1e-5);
    }

    #[test]
    fn fit_keeps_aspect() {
        let img = Image::<f32>::new(4000, 3000);
        let r = fit(&img, 400, 400, Filter::Box);
        assert_eq!((r.width, r.height), (400, 300));
        let small = fit(&Image::<f32>::new(10, 5), 400, 400, Filter::Box);
        assert_eq!((small.width, small.height), (10, 5));
    }

    #[test]
    fn ids_round_trip_and_aliases_parse() {
        for f in Filter::ALL {
            assert_eq!(Filter::parse(f.id()), Some(f), "{f:?}");
            assert_eq!(Filter::parse(&f.id().to_uppercase()), Some(f), "{f:?} any case");
        }
        assert_eq!(Filter::parse("bicubic"), Some(Filter::CatmullRom));
        assert_eq!(Filter::parse("Lanczos"), Some(Filter::Lanczos3));
        assert_eq!(Filter::parse("nearest-neighbor"), Some(Filter::Nearest));
        assert_eq!(Filter::parse("b_spline"), Some(Filter::BSpline));
        assert_eq!(Filter::parse("sinc"), None);
        assert_eq!(Filter::parse(""), None);
    }

    #[test]
    fn kernels_are_one_at_zero_and_vanish_past_support() {
        for f in Filter::ALL {
            assert!(f.eval(f.support() + 0.01).abs() < 1e-6, "{f:?} past support");
            // the smoothing cubics (Mitchell 8/9, B-spline 2/3) are below 1 at the centre by design
            if !matches!(f, Filter::BSpline | Filter::Mitchell) {
                assert!((f.eval(0.0) - 1.0).abs() < 1e-5, "{f:?} at 0");
            }
        }
        // interpolating kernels are zero at every other integer
        for f in [
            Filter::Bilinear,
            Filter::Hermite,
            Filter::CatmullRom,
            Filter::Lanczos2,
            Filter::Lanczos3,
            Filter::Lanczos4,
            Filter::Spline16,
            Filter::Spline36,
        ] {
            for k in 1..=4 {
                assert!(f.eval(k as f32).abs() < 1e-5, "{f:?} at {k}: {}", f.eval(k as f32));
            }
        }
        // continuous at the knots of the piecewise splines
        for f in [Filter::Spline16, Filter::Spline36, Filter::CatmullRom, Filter::Mitchell, Filter::BSpline] {
            for k in [1.0f32, 2.0] {
                assert!((f.eval(k - 1e-4) - f.eval(k + 1e-4)).abs() < 1e-3, "{f:?} jumps at {k}");
            }
        }
    }

    #[test]
    fn interpolating_filters_keep_samples_on_integer_upscale() {
        // a 2× enlargement of a ramp: an interpolating kernel reproduces a linear ramp exactly
        // between its ends (where the clamped border doesn't reach)
        let img = Image::<f32>::from_fn(16, 1, |x, _| x as f32);
        for f in [Filter::Bilinear, Filter::CatmullRom, Filter::Lanczos3, Filter::Spline36] {
            let r = resize(&img, 32, 1, f);
            for x in 8..24 {
                let want = (x as f32 + 0.5) / 2.0 - 0.5;
                assert!((r.data[x] - want).abs() < 0.02, "{f:?} at {x}: {} vs {want}", r.data[x]);
            }
        }
    }

    #[test]
    fn nearest_picks_source_pixels() {
        let img = Image::<f32>::from_fn(4, 4, |x, y| (y * 4 + x) as f32);
        let up = resize(&img, 8, 8, Filter::Nearest);
        assert_eq!(up.data[0..8], [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        let down = resize(&img, 2, 2, Filter::Nearest);
        assert_eq!(down.data, vec![5.0, 7.0, 13.0, 15.0]);
        // every output value is one of the inputs
        let odd = resize(&img, 7, 3, Filter::Nearest);
        assert!(odd.data.iter().all(|v| img.data.contains(v)));
        assert_eq!(resize(&Image::<f32>::filled(1, 1, 2.0), 5, 5, Filter::Nearest).data, vec![2.0; 25]);
    }

    #[test]
    fn every_filter_survives_extreme_ratios() {
        let img = Image::<f32>::from_fn(9, 5, |x, y| ((x + y) % 3) as f32);
        for f in Filter::ALL {
            for (w, h) in [(1, 1), (1, 500), (500, 1), (900, 3), (2, 2)] {
                let r = resize(&img, w, h, f);
                assert_eq!((r.width, r.height), (w, h));
                assert!(r.data.iter().all(|v| v.is_finite()), "{f:?} {w}x{h}");
            }
        }
    }
}
