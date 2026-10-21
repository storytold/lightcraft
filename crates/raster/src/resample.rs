//! Separable resampling with precomputed filter weights.

use crate::{Image, par_rows};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Box,
    Bilinear,
    Mitchell,
    /// Catmull-Rom (the cubic with B = 0, C = ½): sharper than Mitchell, with less ringing than Lanczos.
    CatmullRom,
    Lanczos3,
}

impl Filter {
    fn support(self) -> f32 {
        match self {
            Filter::Box => 0.5,
            Filter::Bilinear => 1.0,
            Filter::Mitchell | Filter::CatmullRom => 2.0,
            Filter::Lanczos3 => 3.0,
        }
    }
    fn eval(self, x: f32) -> f32 {
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
            Filter::Mitchell | Filter::CatmullRom => {
                let (b, c) = if self == Filter::Mitchell { (1.0 / 3.0, 1.0 / 3.0) } else { (0.0, 0.5) };
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

/// [`weights`] for `dst` samples spread evenly over the source span `start .. start + len` (source
/// pixels, fractional allowed): a crop resampled straight from the source in one pass. Taps past
/// the source's edges are dropped and the rest renormalized; a sample wholly outside reads the
/// nearest edge pixel.
pub fn window_weights(src: usize, start: f64, len: f64, dst: usize, filter: Filter) -> Vec<(usize, Vec<f32>)> {
    let dst = dst.max(1);
    let scale = if len.is_finite() && len > 0.0 { len / dst as f64 } else { 1.0 };
    let start = if start.is_finite() { start } else { 0.0 };
    let fscale = scale.max(1.0);
    let support = f64::from(filter.support()) * fscale;
    let last = src.max(1) - 1;
    (0..dst)
        .map(|i| {
            let center = start + (i as f64 + 0.5) * scale;
            let lo = ((center - support).floor().max(0.0) as usize).min(last);
            let hi = ((center + support).ceil().max(0.0) as usize).clamp(lo + 1, src.max(1));
            let mut w: Vec<f32> = (lo..hi).map(|j| filter.eval(((j as f64 + 0.5 - center) / fscale) as f32)).collect();
            let sum: f32 = w.iter().sum();
            if sum.abs() <= 1e-8 {
                // (wholly outside the source: the nearest edge pixel)
                return (if center < 0.0 { 0 } else { last }, vec![1.0]);
            }
            w.iter_mut().for_each(|v| *v /= sum);
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

/// The source span `x` × `y` (source pixels, fractional allowed) resampled to `w × h` in one
/// separable pass ([`window_weights`]): a crop and its scaling without an intermediate image.
/// Only the source rows the output reads are filtered.
pub fn resize_window<T: Pixel>(img: &Image<T>, x: (f64, f64), y: (f64, f64), w: usize, h: usize, filter: Filter) -> Image<T> {
    let (w, h) = (w.max(1), h.max(1));
    if img.width == 0 || img.height == 0 {
        return Image::new(w, h);
    }
    let wx = window_weights(img.width, x.0, x.1 - x.0, w, filter);
    let wy = window_weights(img.height, y.0, y.1 - y.0, h, filter);
    let top = wy.iter().map(|(lo, _)| *lo).min().unwrap_or(0);
    let bottom = wy.iter().map(|(lo, ws)| lo + ws.len()).max().unwrap_or(top + 1).max(top + 1);
    let mut tmp = Image::<T>::new(w, bottom - top);
    par_rows(&mut tmp.data, w, |r, row| {
        let src = img.row(top + r);
        for ((lo, ws), o) in wx.iter().zip(row.iter_mut()) {
            *o = ws.iter().zip(src.iter().skip(*lo)).fold(T::zero(), |acc, (wt, s)| acc.madd(*s, *wt));
        }
    });
    let mut out = Image::<T>::new(w, h);
    par_rows(&mut out.data, w, |r, row| {
        let Some((lo, ws)) = wy.get(r) else { return };
        for (k, wt) in ws.iter().enumerate() {
            let src = tmp.row(lo - top + k);
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
        for f in [Filter::Box, Filter::Bilinear, Filter::Mitchell, Filter::CatmullRom, Filter::Lanczos3] {
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
    fn a_whole_span_window_is_the_plain_resize() {
        let img = Image::<f32>::from_fn(37, 23, |x, y| ((x * 7 + y * 13) % 11) as f32);
        for f in [Filter::Mitchell, Filter::CatmullRom] {
            let a = resize(&img, 15, 9, f);
            let b = resize_window(&img, (0.0, 37.0), (0.0, 23.0), 15, 9, f);
            assert!(a.data.iter().zip(&b.data).all(|(p, q)| (p - q).abs() < 1e-4), "{f:?}");
        }
    }

    #[test]
    fn a_window_resamples_only_its_span() {
        // left half black, right half white: a window over the right half is white throughout
        let img = Image::<f32>::from_fn(40, 10, |x, _| if x >= 20 { 1.0 } else { 0.0 });
        let r = resize_window(&img, (24.0, 40.0), (0.0, 10.0), 8, 5, Filter::CatmullRom);
        assert!(r.data.iter().all(|v| (v - 1.0).abs() < 1e-4), "{:?}", r.data);
        // a span past the edge reads the edge, never out of bounds
        let r = resize_window(&img, (-50.0, -10.0), (30.0, 60.0), 4, 4, Filter::CatmullRom);
        assert!(r.data.iter().all(|v| v.abs() < 1e-4));
    }
}
