//! Cutting a face out of a photo the way recognition models expect it.
//!
//! With the five landmarks the detector finds (eyes, nose tip, mouth corners), the face is turned and scaled
//! (a similarity transform, found by least squares) so those points land on the standard 112 × 112
//! ArcFace template. Without landmarks (a box from an XMP sidecar) a padded square around the box is used
//! instead. Both warp with supersampling, so shrinking a large face does not alias.
//!
//! Pure geometry on 8-bit RGB: no image types, no panics (every access is checked).

/// The five template points of a 112 × 112 aligned face, image-left eye first: eye, eye, nose tip, mouth
/// corner, mouth corner. (The standard ArcFace template.)
pub const TEMPLATE_112: [(f32, f32); 5] = [(38.2946, 51.6963), (73.5318, 51.5014), (56.0252, 71.7366), (41.5493, 92.3655), (70.7299, 92.2041)];

/// A borrowed 8-bit RGB picture.
#[derive(Clone, Copy)]
pub struct Rgb<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
}

impl Rgb<'_> {
    fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.width.checked_mul(self.height).and_then(|n| n.checked_mul(3)) == Some(self.data.len())
    }

    /// The pixel at integer coordinates, clamped to the picture.
    fn at(&self, x: isize, y: isize) -> [f32; 3] {
        let (x, y) = (x.clamp(0, self.width as isize - 1) as usize, y.clamp(0, self.height as isize - 1) as usize);
        let i = (y * self.width + x) * 3;
        match self.data.get(i..i + 3) {
            Some(p) => [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])],
            None => [0.0; 3],
        }
    }

    /// Bilinear sample at a fractional position (pixel centres at integers).
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x0 + 1, y0), self.at(x0, y0 + 1), self.at(x0 + 1, y0 + 1));
        let mut out = [0.0; 3];
        for k in 0..3 {
            let top = a[k] * (1.0 - fx) + b[k] * fx;
            let bottom = c[k] * (1.0 - fx) + d[k] * fx;
            out[k] = top * (1.0 - fy) + bottom * fy;
        }
        out
    }
}

/// `q = a · p + t` with complex `a` (scale and rotation) and `t`, mapping source positions to output positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    pub a: (f32, f32),
    pub t: (f32, f32),
}

impl Similarity {
    pub fn scale(&self) -> f32 {
        self.a.0.hypot(self.a.1)
    }

    /// Where an output position `q` comes from in the source.
    fn invert(&self, q: (f32, f32)) -> Option<(f32, f32)> {
        let n = self.a.0 * self.a.0 + self.a.1 * self.a.1;
        if !(n.is_finite() && n > 1e-12) {
            return None;
        }
        let (dx, dy) = (q.0 - self.t.0, q.1 - self.t.1);
        Some(((dx * self.a.0 + dy * self.a.1) / n, (dy * self.a.0 - dx * self.a.1) / n))
    }
}

/// The least-squares similarity transform taking `from[i]` to `to[i]` (no reflection). `None` when the points
/// do not determine one (all the same point, or non-finite).
pub fn fit_similarity(from: &[(f32, f32)], to: &[(f32, f32)]) -> Option<Similarity> {
    if from.len() != to.len() || from.len() < 2 {
        return None;
    }
    let n = from.len() as f32;
    let mean = |pts: &[(f32, f32)]| (pts.iter().map(|p| p.0).sum::<f32>() / n, pts.iter().map(|p| p.1).sum::<f32>() / n);
    let (mp, mq) = (mean(from), mean(to));
    // a = Σ conj(p') q' / Σ |p'|²   (complex numbers)
    let (mut re, mut im, mut norm) = (0.0f32, 0.0f32, 0.0f32);
    for (p, q) in from.iter().zip(to) {
        let (px, py, qx, qy) = (p.0 - mp.0, p.1 - mp.1, q.0 - mq.0, q.1 - mq.1);
        re += px * qx + py * qy;
        im += px * qy - py * qx;
        norm += px * px + py * py;
    }
    if !(norm.is_finite() && norm > 1e-9) {
        return None;
    }
    let a = (re / norm, im / norm);
    let t = (mq.0 - (a.0 * mp.0 - a.1 * mp.1), mq.1 - (a.0 * mp.1 + a.1 * mp.0));
    (a.0.is_finite() && a.1.is_finite() && t.0.is_finite() && t.1.is_finite()).then_some(Similarity { a, t })
}

/// Fill an `out_w` × `out_h` picture by taking, for each output pixel, the source under it, averaging
/// enough sub-samples that shrinking does not alias.
fn warp(img: &Rgb, m: &Similarity, out_w: usize, out_h: usize) -> Option<Vec<u8>> {
    if !img.valid() || out_w == 0 || out_h == 0 || out_w > 4096 || out_h > 4096 {
        return None;
    }
    if ![m.a.0, m.a.1, m.t.0, m.t.1].iter().all(|v| v.is_finite()) {
        return None;
    }
    // how many source pixels one output pixel spans, per side
    let span = 1.0 / m.scale();
    let n = if span.is_finite() { (span.ceil() as usize).clamp(1, 4) } else { 1 };
    let mut out = vec![0u8; out_w * out_h * 3];
    for (i, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        let (ox, oy) = ((i % out_w) as f32, (i / out_w) as f32);
        let mut acc = [0.0f32; 3];
        for sy in 0..n {
            for sx in 0..n {
                let q = (ox + (sx as f32 + 0.5) / n as f32 - 0.5, oy + (sy as f32 + 0.5) / n as f32 - 0.5);
                let (px_, py_) = m.invert(q)?;
                let s = img.sample(px_, py_);
                acc.iter_mut().zip(s).for_each(|(a, v)| *a += v);
            }
        }
        let k = (n * n) as f32;
        for (o, a) in px.iter_mut().zip(acc) {
            *o = (a / k).round().clamp(0.0, 255.0) as u8;
        }
    }
    Some(out)
}

/// The face aligned to the ArcFace template, `out_w` × `out_h` (the template is for 112 × 112 and is scaled for
/// other sizes). `landmarks` are in pixels of `img`. `None` when the landmarks cannot define a transform.
pub fn align_to_template(img: &Rgb, landmarks: &[(f32, f32); 5], out_w: usize, out_h: usize) -> Option<Vec<u8>> {
    let (sx, sy) = (out_w as f32 / 112.0, out_h as f32 / 112.0);
    let template: Vec<(f32, f32)> = TEMPLATE_112.iter().map(|(x, y)| (x * sx, y * sy)).collect();
    let m = fit_similarity(landmarks, &template)?;
    // a face smaller than a few pixels, or absurdly large, is not a face we can use
    let scale = m.scale();
    if !(scale.is_finite() && (0.01..=100.0).contains(&scale)) {
        return None;
    }
    warp(img, &m, out_w, out_h)
}

/// A square around a face box (pixels of `img`), `pad` times its larger side, as `out_w` × `out_h`.
pub fn crop_box(img: &Rgb, x0: f32, y0: f32, x1: f32, y1: f32, pad: f32, out_w: usize, out_h: usize) -> Option<Vec<u8>> {
    if ![x0, y0, x1, y1, pad].iter().all(|v| v.is_finite()) {
        return None;
    }
    let side = (x1 - x0).abs().max((y1 - y0).abs()) * pad;
    if !(side.is_finite() && side >= 2.0) {
        return None;
    }
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let s = out_w.min(out_h) as f32 / side;
    let m = Similarity { a: (s, 0.0), t: (out_w as f32 / 2.0 - s * cx, out_h as f32 / 2.0 - s * cy) };
    warp(img, &m, out_w, out_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32), tol: f32) -> bool {
        (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol
    }

    fn apply(m: &Similarity, p: (f32, f32)) -> (f32, f32) {
        (m.a.0 * p.0 - m.a.1 * p.1 + m.t.0, m.a.0 * p.1 + m.a.1 * p.0 + m.t.1)
    }

    #[test]
    fn a_known_similarity_is_recovered() {
        // rotate 20 degrees, scale 0.7, translate
        let (s, th) = (0.7f32, 20f32.to_radians());
        let truth = Similarity { a: (s * th.cos(), s * th.sin()), t: (13.0, -5.0) };
        let from: Vec<(f32, f32)> = TEMPLATE_112.to_vec();
        let to: Vec<(f32, f32)> = from.iter().map(|p| apply(&truth, *p)).collect();
        let m = fit_similarity(&from, &to).unwrap();
        assert!(close(m.a, truth.a, 1e-4) && close(m.t, truth.t, 1e-3), "{m:?} vs {truth:?}");
        // noisy points still give the best fit (the residual is small)
        let noisy: Vec<(f32, f32)> = to.iter().enumerate().map(|(i, p)| (p.0 + (i as f32 - 2.0) * 0.2, p.1 - (i as f32 - 2.0) * 0.1)).collect();
        let m2 = fit_similarity(&from, &noisy).unwrap();
        assert!(from.iter().zip(&noisy).all(|(p, q)| close(apply(&m2, *p), *q, 1.0)));
    }

    #[test]
    fn degenerate_points_give_nothing() {
        let same = vec![(5.0, 5.0); 5];
        assert!(fit_similarity(&same, &TEMPLATE_112).is_none());
        assert!(fit_similarity(&TEMPLATE_112, &TEMPLATE_112[..3]).is_none());
        assert!(fit_similarity(&[(0.0, 0.0)], &[(1.0, 1.0)]).is_none());
        let nan = vec![(f32::NAN, 0.0); 5];
        assert!(fit_similarity(&nan, &TEMPLATE_112).is_none());
        let inf = vec![(f32::INFINITY, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0), (4.0, 4.0)];
        assert!(fit_similarity(&inf, &TEMPLATE_112).is_none());
    }

    /// A picture with a bright dot at each of five landmarks, the landmarks being the template moved by a known
    /// similarity: aligning must put the dots back on the template.
    #[test]
    fn alignment_puts_the_landmarks_on_the_template() {
        let (w, h) = (400usize, 300usize);
        let (s, th) = (1.7f32, -15f32.to_radians());
        let place = Similarity { a: (s * th.cos(), s * th.sin()), t: (150.0, 60.0) };
        let landmarks: Vec<(f32, f32)> = TEMPLATE_112.iter().map(|p| apply(&place, *p)).collect();
        let mut data = vec![0u8; w * h * 3];
        for l in &landmarks {
            for dy in -3i32..=3 {
                for dx in -3i32..=3 {
                    let (x, y) = ((l.0 as i32 + dx) as usize, (l.1 as i32 + dy) as usize);
                    if x < w && y < h {
                        data[(y * w + x) * 3] = 255;
                    }
                }
            }
        }
        let img = Rgb { data: &data, width: w, height: h };
        let lm: [(f32, f32); 5] = [landmarks[0], landmarks[1], landmarks[2], landmarks[3], landmarks[4]];
        let out = align_to_template(&img, &lm, 112, 112).unwrap();
        assert_eq!(out.len(), 112 * 112 * 3);
        // the red channel's centre of mass near each template point is on the point
        for t in TEMPLATE_112 {
            let (mut sx, mut sy, mut sw) = (0.0f32, 0.0f32, 0.0f32);
            for y in (t.1 as usize).saturating_sub(8)..(t.1 as usize + 9).min(112) {
                for x in (t.0 as usize).saturating_sub(8)..(t.0 as usize + 9).min(112) {
                    let v = f32::from(out[(y * 112 + x) * 3]);
                    sx += v * x as f32;
                    sy += v * y as f32;
                    sw += v;
                }
            }
            assert!(sw > 100.0, "no dot near {t:?}");
            assert!(close((sx / sw, sy / sw), t, 1.5), "dot at {:?}, template {t:?}", (sx / sw, sy / sw));
        }
    }

    #[test]
    fn box_crops_are_centred_and_bad_inputs_are_refused() {
        let (w, h) = (200usize, 100usize);
        let mut data = vec![0u8; w * h * 3];
        for y in 40..60 {
            for x in 90..110 {
                data[(y * w + x) * 3 + 1] = 255;
            }
        }
        let img = Rgb { data: &data, width: w, height: h };
        let out = crop_box(&img, 90.0, 40.0, 110.0, 60.0, 2.0, 64, 64).unwrap();
        // the 20 px block fills the middle half of the 64 px output
        let at = |x: usize, y: usize| out[(y * 64 + x) * 3 + 1];
        assert!(at(32, 32) > 250 && at(20, 20) > 250 && at(44, 44) > 250 && at(5, 5) < 5 && at(60, 32) < 5);
        assert!(crop_box(&img, 5.0, 5.0, 5.0, 5.0, 2.0, 64, 64).is_none(), "an empty box");
        assert!(crop_box(&img, f32::NAN, 0.0, 10.0, 10.0, 2.0, 64, 64).is_none());
        assert!(crop_box(&img, 0.0, 0.0, 10.0, 10.0, 2.0, 0, 64).is_none());
        assert!(crop_box(&img, 0.0, 0.0, 10.0, 10.0, 2.0, 100_000, 64).is_none());
        let bad = Rgb { data: &data[..10], width: w, height: h };
        assert!(crop_box(&bad, 0.0, 0.0, 10.0, 10.0, 2.0, 64, 64).is_none(), "wrong data length");
    }

    #[test]
    fn faces_off_the_edge_and_tiny_or_huge_landmarks_do_not_panic() {
        let data = vec![80u8; 50 * 50 * 3];
        let img = Rgb { data: &data, width: 50, height: 50 };
        // a box mostly outside the picture is padded by clamping, not an error
        assert!(crop_box(&img, -30.0, -30.0, 20.0, 20.0, 1.5, 32, 32).is_some());
        let far: [(f32, f32); 5] = [(1e6, 1e6), (1e6 + 5.0, 1e6), (1e6 + 2.0, 1e6 + 3.0), (1e6, 1e6 + 6.0), (1e6 + 5.0, 1e6 + 6.0)];
        let _ = align_to_template(&img, &far, 112, 112);
        let tiny: [(f32, f32); 5] = [(1.0, 1.0), (1.01, 1.0), (1.0, 1.01), (1.01, 1.01), (1.005, 1.005)];
        assert!(align_to_template(&img, &tiny, 112, 112).is_none(), "landmarks a hundredth of a pixel apart");
    }
}
