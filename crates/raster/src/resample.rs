//! Separable resampling with precomputed filter weights.

use crate::{AllocError, Image, Rgba8, par_rows};

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

/// Resize to `w × h` (separable; horizontal pass then vertical pass, or the other way round when
/// that intermediate would be far larger: widening a tall, narrow strip kept every source row at
/// the new width, gigabytes from a small source).
pub fn resize<T: Pixel>(img: &Image<T>, w: usize, h: usize, filter: Filter) -> Image<T> {
    let (w, h) = (w.max(1), h.max(1));
    if img.width == w && img.height == h {
        return img.clone();
    }
    if w.saturating_mul(img.height) > img.width.saturating_mul(h).saturating_mul(4) {
        return horizontal(&vertical(img, Image::new(img.width, h), filter), Image::new(w, h), filter);
    }
    vertical(&horizontal(img, Image::new(w, img.height), filter), Image::new(w, h), filter)
}

/// [`resize`] that returns an error instead of aborting when the intermediate or the result can't
/// be had (a megapixel-sized buffer past [`crate::MAX_IMAGE_BYTES`] or the allocator's reach).
pub fn try_resize<T: Pixel>(img: &Image<T>, w: usize, h: usize, filter: Filter) -> Result<Image<T>, AllocError> {
    let (w, h) = (w.max(1), h.max(1));
    if img.width == w && img.height == h {
        let mut data = Vec::new();
        data.try_reserve_exact(img.data.len()).map_err(|_| AllocError { width: w, height: h })?;
        data.extend_from_slice(&img.data);
        return Ok(Image { width: w, height: h, data });
    }
    if w.saturating_mul(img.height) > img.width.saturating_mul(h).saturating_mul(4) {
        let mid = vertical(img, Image::try_new(img.width, h)?, filter);
        return Ok(horizontal(&mid, Image::try_new(w, h)?, filter));
    }
    let mid = horizontal(img, Image::try_new(w, img.height)?, filter);
    Ok(vertical(&mid, Image::try_new(w, h)?, filter))
}

/// Each row resampled to `tmp.width` wide (`tmp` is the `w × img.height` output buffer).
fn horizontal<T: Pixel>(img: &Image<T>, mut tmp: Image<T>, filter: Filter) -> Image<T> {
    let w = tmp.width;
    let wx = weights(img.width, w, filter);
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
    tmp
}

/// Each column resampled to `out.height` tall (`out` is the `img.width × h` output buffer).
fn vertical<T: Pixel>(img: &Image<T>, mut out: Image<T>, filter: Filter) -> Image<T> {
    let (w, h) = (img.width, out.height);
    let wy = weights(img.height, h, filter);
    par_rows(&mut out.data, w, |y, row| {
        let (lo, ws) = &wy[y];
        for (k, wt) in ws.iter().enumerate() {
            let src = img.row(lo + k);
            for (o, s) in row.iter_mut().zip(src) {
                *o = o.madd(*s, *wt);
            }
        }
    });
    out
}

/// Fit within `max_w × max_h` preserving aspect (never upscales).
pub fn fit<T: Pixel>(img: &Image<T>, max_w: usize, max_h: usize, filter: Filter) -> Image<T> {
    let (w, h) = fit_size(img.width, img.height, max_w, max_h);
    resize(img, w, h, filter)
}

/// The size [`fit`] (and [`fit_rgba8`]) shrinks a `width × height` image to.
pub fn fit_size(width: usize, height: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    let s = (max_w as f64 / width as f64).min(max_h as f64 / height as f64).min(1.0);
    (((width as f64 * s).round() as usize).max(1), ((height as f64 * s).round() as usize).max(1))
}

/// [`fit`] for an 8-bit image, read and written as bytes: the same taps and float arithmetic as
/// `fit` on a float copy of it, rounded back to bytes, so the result is identical, without the two
/// full-size float copies and their single-threaded conversions (most of the time a 2560 px
/// loupe render took to shrink). Alpha is filtered only when some pixel is not opaque.
pub fn fit_rgba8(img: &Rgba8, max_w: usize, max_h: usize, filter: Filter) -> Rgba8 {
    let (w, h) = fit_size(img.width, img.height, max_w, max_h);
    if img.width == w && img.height == h {
        return img.clone();
    }
    let mut opaque = vec![true; img.height];
    par_rows(&mut opaque, 1, |y, o| o[0] = img.row(y).iter().all(|p| p[3] == 255));
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    if opaque.iter().all(|o| *o) {
        resize_bytes(
            img,
            w,
            h,
            filter,
            |p| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])],
            |v: [f32; 3]| [byte(v[0]), byte(v[1]), byte(v[2]), 255],
        )
    } else {
        resize_bytes(img, w, h, filter, |p| p.map(f32::from), |v: [f32; 4]| v.map(byte))
    }
}

/// [`resize`] of bytes through `T` samples: `load` reads a pixel, `store` writes one back.
fn resize_bytes<T: Pixel>(
    img: &Rgba8,
    w: usize,
    h: usize,
    filter: Filter,
    load: impl Fn(&[u8; 4]) -> T + Sync + Send,
    store: impl Fn(T) -> [u8; 4] + Sync + Send,
) -> Rgba8 {
    let wx = weights(img.width, w, filter);
    let mut tmp = Image::<T>::new(w, img.height);
    par_rows(&mut tmp.data, w, |y, row| {
        let src = img.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let (lo, ws) = &wx[x];
            let mut acc = T::zero();
            for (k, wt) in ws.iter().enumerate() {
                acc = acc.madd(load(&src[lo + k]), *wt);
            }
            *o = acc;
        }
    });
    let wy = weights(img.height, h, filter);
    let mut out = Rgba8::new(w, h);
    par_rows(&mut out.data, w, |y, row| {
        let (lo, ws) = &wy[y];
        // (summed in the order `resize` sums, so the bytes match its float result rounded)
        let mut acc = vec![T::zero(); w];
        for (k, wt) in ws.iter().enumerate() {
            let src = tmp.row(lo + k);
            for (o, s) in acc.iter_mut().zip(src) {
                *o = o.madd(*s, *wt);
            }
        }
        for (o, a) in row.iter_mut().zip(acc) {
            *o = store(a);
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
    fn try_resize_reports_an_unallocatable_size_and_matches_resize_otherwise() {
        let img = Image::<[f32; 3]>::filled(16, 8, [0.25, 0.5, 0.75]);
        assert!(try_resize(&img, 1 << 20, 1 << 20, Filter::Mitchell).is_err(), "12 TiB is an error, not an abort");
        assert!(try_resize(&img, usize::MAX, 3, Filter::Box).is_err());
        for (w, h) in [(16, 8), (5, 3), (40, 2), (1, 30)] {
            let ok = try_resize(&img, w, h, Filter::Lanczos3).expect("small sizes allocate");
            assert_eq!(ok, resize(&img, w, h, Filter::Lanczos3));
        }
    }

    #[test]
    fn widening_a_tall_strip_resamples_its_columns_first() {
        // 1 × 400 000 to 4000 × 8: horizontal-first kept 400 000 rows of 4000 (25 GB of RGBA f32)
        let mut strip = Image::<[f32; 4]>::new(1, 400_000);
        strip.data.iter_mut().for_each(|p| *p = [0.25, 0.5, 0.75, 1.0]);
        let out = resize(&strip, 4000, 8, Filter::Mitchell);
        assert_eq!((out.width, out.height), (4000, 8));
        assert!(out.data.iter().all(|p| (p[1] - 0.5).abs() < 1e-3), "a flat strip stays flat");
    }

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

    #[test]
    fn fit_keeps_aspect() {
        let img = Image::<f32>::new(4000, 3000);
        let r = fit(&img, 400, 400, Filter::Box);
        assert_eq!((r.width, r.height), (400, 300));
        let small = fit(&Image::<f32>::new(10, 5), 400, 400, Filter::Box);
        assert_eq!((small.width, small.height), (10, 5));
    }

    #[test]
    fn byte_fit_matches_the_float_fit_rounded() {
        // the float path the loupe's display fit used: bytes to floats, `fit`, rounded back
        let float_fit = |img: &Rgba8, w: usize, h: usize, f: Filter| {
            let fl = Image { width: img.width, height: img.height, data: img.data.iter().map(|p| p.map(f32::from)).collect() };
            let r = fit(&fl, w, h, f);
            Image { width: r.width, height: r.height, data: r.data.iter().map(|p| p.map(|v| v.round().clamp(0.0, 255.0) as u8)).collect::<Vec<_>>() }
        };
        // hard edges (Lanczos overshoots past 0 and 255) and texture
        let opaque = Rgba8::from_fn(257, 171, |x, y| {
            [if (x / 3 + y / 2) % 2 == 0 { 0 } else { 255 }, ((x * 7 + y * 13) % 256) as u8, ((x ^ y) % 256) as u8, 255]
        });
        let mut translucent = opaque.clone();
        for (i, p) in translucent.data.iter_mut().enumerate() {
            p[3] = if i % 5 == 0 { 0 } else { 200 };
        }
        for img in [&opaque, &translucent] {
            for f in [Filter::Lanczos3, Filter::Mitchell, Filter::Bilinear, Filter::Box] {
                for (w, h) in [(173, 115), (64, 64), (100, 300), (1, 1), (257, 171), (400, 400)] {
                    let got = fit_rgba8(img, w, h, f);
                    let want = float_fit(img, w, h, f);
                    assert_eq!((got.width, got.height), (want.width, want.height), "{f:?} {w}x{h}");
                    assert!(got.data == want.data, "{f:?} {w}x{h}: bytes differ");
                }
            }
        }
        assert!(fit_rgba8(&opaque, 100, 100, Filter::Lanczos3).data.iter().all(|p| p[3] == 255), "opaque stays opaque");
    }
}
