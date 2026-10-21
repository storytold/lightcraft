//! Separable resampling with precomputed filter weights.

use crate::{Image, par_rows};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Box,
    Bilinear,
    Mitchell,
    Lanczos3,
}

impl Filter {
    fn support(self) -> f32 {
        match self {
            Filter::Box => 0.5,
            Filter::Bilinear => 1.0,
            Filter::Mitchell => 2.0,
            Filter::Lanczos3 => 3.0,
        }
    }
    /// The filter's weight at distance `x` (pixels of the source).
    pub(crate) fn eval(self, x: f32) -> f32 {
        let x = x.abs();
        match self {
            Filter::Box => {
                if x <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Filter::Bilinear => (1.0 - x).max(0.0),
            Filter::Mitchell => {
                let (b, c) = (1.0 / 3.0, 1.0 / 3.0);
                if x < 1.0 {
                    ((12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b)) / 6.0
                } else if x < 2.0 {
                    ((-b - 6.0 * c) * x * x * x + (6.0 * b + 30.0 * c) * x * x + (-12.0 * b - 48.0 * c) * x + (8.0 * b + 24.0 * c)) / 6.0
                } else {
                    0.0
                }
            }
            Filter::Lanczos3 => {
                if x < 1e-6 {
                    1.0
                } else if x < 3.0 {
                    let px = std::f32::consts::PI * x;
                    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
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

/// Exact area weights for `dst` output samples covering the source span `start .. start + len`
/// (in source pixels, fractional) of a `src`-pixel axis: output sample `i` averages the source
/// interval `[start + i·k, start + (i+1)·k)` (`k = len / dst`), each source pixel weighted by how
/// much of it lies inside. The span is clamped to the axis; weights sum to 1. A degenerate span
/// (empty, or not a number) reads the nearest whole pixel.
pub fn area_weights(src: usize, start: f64, len: f64, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let dst = dst.max(1);
    let size = src.max(1) as f64;
    let (start, len) = if start.is_finite() && len.is_finite() { (start, len) } else { (0.0, size) };
    let a = start.clamp(0.0, size);
    let b = (start + len).clamp(a, size);
    let k = (b - a) / dst as f64;
    (0..dst)
        .map(|i| {
            let (x0, x1) = (a + i as f64 * k, a + (i + 1) as f64 * k);
            let lo = (x0.floor().max(0.0) as usize).min(src.saturating_sub(1));
            if x1 - x0 < 1e-9 {
                return (lo, vec![1.0]);
            }
            let hi = (x1.ceil() as usize).clamp(lo + 1, src.max(lo + 1));
            let w: Vec<f64> = (lo..hi).map(|j| (x1.min(j as f64 + 1.0) - x0.max(j as f64)).max(0.0)).collect();
            let sum: f64 = w.iter().sum();
            let w = if sum > 1e-12 { w.iter().map(|v| (v / sum) as f32).collect() } else { vec![1.0 / w.len().max(1) as f32; w.len()] };
            (lo, w)
        })
        .collect()
}

/// Area-average the rectangle `rect` = `[x, y, w, h]` (source pixels, fractional; clamped into the
/// image) of `img` to `w × h` pixels: every output pixel is the exact area-weighted mean of the
/// source it covers ([`area_weights`]), at any ratio. `to` maps each source pixel to the space the
/// averaging is done in (e.g. 8-bit display codes to linear light), read row by row, so no
/// converted copy of the whole source is held. Meant for downscaling; an empty image gives `w × h`
/// zeros.
pub fn area<S: Copy + Default + Sync, T: Pixel>(img: &Image<S>, rect: [f64; 4], w: usize, h: usize, to: impl Fn(S) -> T + Sync) -> Image<T> {
    let (w, h) = (w.max(1), h.max(1));
    if img.width == 0 || img.height == 0 || img.data.len() < img.width.saturating_mul(img.height) {
        return Image::<T>::new(w, h);
    }
    let wx = area_weights(img.width, rect[0], rect[2], w);
    let wy = area_weights(img.height, rect[1], rect[3], h);
    // the source rows any output row reads
    let rows_lo = wy.first().map_or(0, |(lo, _)| *lo);
    let rows_hi = wy.last().map_or(0, |(lo, ws)| lo + ws.len()).clamp(rows_lo, img.height);
    let mut tmp = Image::<T>::new(w, rows_hi - rows_lo);
    par_rows(&mut tmp.data, w, |r, row| {
        let src = img.row(rows_lo + r);
        for (o, (lo, ws)) in row.iter_mut().zip(&wx) {
            let mut acc = T::zero();
            for (s, wt) in src.iter().skip(*lo).zip(ws) {
                acc = acc.madd(to(*s), *wt);
            }
            *o = acc;
        }
    });
    let mut out = Image::<T>::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        let Some((lo, ws)) = wy.get(y) else { return };
        for (k, wt) in ws.iter().enumerate() {
            let Some(r) = (lo + k).checked_sub(rows_lo).filter(|r| *r < tmp.height) else { continue };
            for (o, s) in row.iter_mut().zip(tmp.row(r)) {
                *o = o.madd(*s, *wt);
            }
        }
    });
    out
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
        for f in [Filter::Box, Filter::Bilinear, Filter::Mitchell, Filter::Lanczos3] {
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

    fn mean3(img: &Image<[f32; 3]>) -> [f64; 3] {
        let n = img.len().max(1) as f64;
        let s = img.data.iter().fold([0.0f64; 3], |a, p| [a[0] + p[0] as f64, a[1] + p[1] as f64, a[2] + p[2] as f64]);
        [s[0] / n, s[1] / n, s[2] / n]
    }

    #[test]
    fn area_keeps_a_constant_image_constant() {
        let img = Image::<[f32; 3]>::filled(37, 23, [0.25, 0.5, 0.75]);
        for (rect, w, h) in
            [([0.0, 0.0, 37.0, 23.0], 10, 7), ([0.0, 0.0, 37.0, 23.0], 3, 1), ([3.3, 1.7, 20.25, 13.9], 9, 5), ([5.0, 5.0, 0.0, 0.0], 2, 2)]
        {
            let r = area(&img, rect, w, h, |p| p);
            assert_eq!((r.width, r.height), (w, h));
            assert!(r.data.iter().all(|p| (p[0] - 0.25).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6 && (p[2] - 0.75).abs() < 1e-6), "{rect:?}");
        }
    }

    /// At 2×, each output pixel is exactly the mean of its 2 × 2 block, averaged in linear light
    /// (not in the 8-bit codes it reads).
    #[test]
    fn area_at_two_to_one_is_the_exact_linear_average() {
        let img = crate::Rgba8::from_fn(8, 6, |x, y| if (x + y) % 2 == 0 { [255, 0, 128, 255] } else { [0, 255, 10, 255] });
        let lin = |p: [u8; 4]| [p[0], p[1], p[2]].map(lightcraft_color::transfer::decode_srgb8);
        let r = area(&img, [0.0, 0.0, 8.0, 6.0], 4, 3, lin);
        for y in 0..3 {
            for x in 0..4 {
                let want = [0, 1, 2].map(|c| (0..4).map(|i| lin(img.get(2 * x + i % 2, 2 * y + i / 2))[c]).sum::<f32>() / 4.0);
                let got = r.get(x, y);
                assert!((0..3).all(|c| (got[c] - want[c]).abs() < 1e-6), "{x},{y}: {got:?} vs {want:?}");
            }
        }
        // half of full-scale light, not the code value halfway (which is about a fifth of the light)
        assert!((r.get(0, 0)[0] - 0.5).abs() < 1e-6);
    }

    /// At a ratio that is not a whole number, the light in the image is kept: the mean of the
    /// result equals the mean of the source, and a sparse pattern averages to its share of lit pixels.
    #[test]
    fn area_at_a_fractional_ratio_conserves_mean_light() {
        let img = Image::<[f32; 3]>::from_fn(101, 77, |x, y| if (x * 31 + y * 17) % 19 == 0 { [1.0, 0.5, 0.0] } else { [0.0, 0.0, 0.0] });
        let want = mean3(&img);
        for (w, h) in [(37, 29), (13, 10), (100, 76), (1, 1)] {
            let got = mean3(&area(&img, [0.0, 0.0, 101.0, 77.0], w, h, |p| p));
            assert!((0..3).all(|c| (got[c] - want[c]).abs() < 1e-5), "{w}x{h}: {got:?} vs {want:?}");
        }
        // weights: exact overlaps, summing to one
        let ws = area_weights(10, 0.0, 10.0, 4);
        assert_eq!(ws[0].0, 0);
        assert_eq!(ws[0].1, vec![0.4, 0.4, 0.2]);
        assert_eq!(ws[1].0, 2);
        assert!(ws.iter().all(|(_, w)| (w.iter().sum::<f32>() - 1.0).abs() < 1e-6));
    }

    /// A fractional source rectangle reads only what it covers: tiles of one image cut at
    /// fractional edges add up to the whole image's light, and hostile numbers never panic.
    #[test]
    fn area_of_a_fractional_rect_and_hostile_rects() {
        let img = Image::<f32>::from_fn(40, 30, |x, y| (x * 3 + y * 7) as f32 % 5.0);
        let whole = area(&img, [0.0, 0.0, 40.0, 30.0], 16, 12, |p| p);
        // the left 6 output columns are the source's first 15 columns
        let left = area(&img, [0.0, 0.0, 15.0, 30.0], 6, 12, |p| p);
        for y in 0..12 {
            for x in 0..6 {
                assert!((left.get(x, y) - whole.get(x, y)).abs() < 1e-5);
            }
        }
        for rect in [[f64::NAN, 0.0, 5.0, 5.0], [-10.0, -10.0, 1e9, 1e9], [50.0, 50.0, 10.0, 10.0], [0.0, 0.0, -5.0, f64::INFINITY]] {
            let r = area(&img, rect, 3, 2, |p| p);
            assert_eq!((r.width, r.height), (3, 2));
            assert!(r.data.iter().all(|v| v.is_finite()), "{rect:?}");
        }
        let empty = Image::<f32> { width: 4, height: 4, data: vec![] };
        assert_eq!(area(&empty, [0.0, 0.0, 4.0, 4.0], 2, 2, |p| p).data, vec![0.0; 4]);
    }

    #[test]
    fn fit_keeps_aspect() {
        let img = Image::<f32>::new(4000, 3000);
        let r = fit(&img, 400, 400, Filter::Box);
        assert_eq!((r.width, r.height), (400, 300));
        let small = fit(&Image::<f32>::new(10, 5), 400, 400, Filter::Box);
        assert_eq!((small.width, small.height), (10, 5));
    }
}
