//! Pre- and post-processing for Google's MediaPipe vision models (Apache-2.0): BlazeFace (face
//! detection), FaceMesh-V2 (478 face landmarks) and Selfie Multiclass (hair, face skin, body skin,
//! clothes), and the geometry that turns landmarks into face-part masks (eyes, irises, brows, lips,
//! teeth). The networks themselves run elsewhere (`people.rs`); everything here is plain Rust on
//! slices, independent of the inference runtime.
//!
//! Original LightCraft code. The landmark index lists ([`LIPS_OUTER`], …) are data from MediaPipe's
//! `face_landmarks_connections` (Copyright 2023 The MediaPipe Authors, Apache License 2.0; see NOTICE).

/// Side of BlazeFace's (short range) square input.
pub const BLAZEFACE_INPUT: usize = 128;
/// Side of FaceMesh-V2's square input.
pub const MESH_INPUT: usize = 256;
/// FaceMesh-V2's landmarks.
pub const MESH_POINTS: usize = 478;
/// Side of Selfie Multiclass's square input and output.
pub const SELFIE_INPUT: usize = 256;
/// Selfie Multiclass's classes, in output order.
pub const SELFIE_CLASSES: [&str; 6] = ["background", "hair", "body skin", "face skin", "clothes", "others"];

/// The outer lip line (closed).
pub const LIPS_OUTER: [usize; 20] = [61, 146, 91, 181, 84, 17, 314, 405, 321, 375, 291, 409, 270, 269, 267, 0, 37, 39, 40, 185];
/// The inner lip line (the mouth opening, closed).
pub const LIPS_INNER: [usize; 20] = [78, 95, 88, 178, 87, 14, 317, 402, 318, 324, 308, 415, 310, 311, 312, 13, 82, 81, 80, 191];
/// The eye openings (the image's left eye first: the person's right eye).
pub const EYES: [[usize; 16]; 2] = [
    [33, 7, 163, 144, 145, 153, 154, 155, 133, 173, 157, 158, 159, 160, 161, 246],
    [263, 249, 390, 373, 374, 380, 381, 382, 362, 398, 384, 385, 386, 387, 388, 466],
];
/// The brows: upper line then lower line, per brow.
pub const BROWS: [([usize; 5], [usize; 5]); 2] =
    [([70, 63, 105, 66, 107], [46, 53, 52, 65, 55]), ([300, 293, 334, 296, 336], [276, 283, 282, 295, 285])];
/// Iris centres and their four rim points, per eye (same order as [`EYES`]).
pub const IRISES: [(usize, [usize; 4]); 2] = [(468, [469, 470, 471, 472]), (473, [474, 475, 476, 477])];
/// The face oval.
pub const FACE_OVAL: [usize; 36] = [
    10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152, 148, 176, 149, 150, 136, 172, 58, 132, 93, 234,
    127, 162, 21, 54, 103, 67, 109,
];

/// A face BlazeFace found: its box and six keypoints (eyes, nose, mouth, ears), in the units of
/// whatever image it is expressed in.
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub score: f32,
    /// Right eye (the image's left), left eye, nose tip, mouth, right ear, left ear.
    pub keypoints: [[f32; 2]; 6],
}

impl Detection {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
    pub fn center(&self) -> [f32; 2] {
        [(self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0]
    }

    /// Maps `self` from a window's normalized coordinates to the image: the window starts at
    /// `(ox, oy)` and is `side` pixels.
    pub fn from_window(&self, ox: f32, oy: f32, side: f32) -> Detection {
        let m = |p: [f32; 2]| [ox + p[0] * side, oy + p[1] * side];
        Detection {
            x0: ox + self.x0 * side,
            y0: oy + self.y0 * side,
            x1: ox + self.x1 * side,
            y1: oy + self.y1 * side,
            score: self.score,
            keypoints: self.keypoints.map(m),
        }
    }

    fn iou(&self, o: &Detection) -> f32 {
        let iw = (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0.0);
        let ih = (self.y1.min(o.y1) - self.y0.max(o.y0)).max(0.0);
        let inter = iw * ih;
        let a = self.width().max(0.0) * self.height().max(0.0) + o.width().max(0.0) * o.height().max(0.0) - inter;
        if a > 0.0 { inter / a } else { 0.0 }
    }

    /// Overlap relative to the smaller box (a face found whole and again cut by a tile edge).
    fn overlap(&self, o: &Detection) -> f32 {
        let iw = (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0.0);
        let ih = (self.y1.min(o.y1) - self.y0.max(o.y0)).max(0.0);
        let small = (self.width() * self.height()).min(o.width() * o.height());
        if small > 0.0 { iw * ih / small } else { 0.0 }
    }
}

/// BlazeFace short range's 896 anchor centres (normalized): 16 × 16 cells × 2 at stride 8, then
/// 8 × 8 cells × 6 at stride 16 (MediaPipe's SSD anchor options for this model).
pub fn blazeface_anchors() -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(896);
    for (grid, per) in [(16usize, 2usize), (8, 6)] {
        for y in 0..grid {
            for x in 0..grid {
                for _ in 0..per {
                    out.push([(x as f32 + 0.5) / grid as f32, (y as f32 + 0.5) / grid as f32]);
                }
            }
        }
    }
    out
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-80.0, 80.0)).exp())
}

/// The faces in BlazeFace's raw outputs (`regressors` 896 × 16, `scores` 896 logits), scoring at
/// least `min_score`, in the input's normalized coordinates, after weighted non-maximum
/// suppression (MediaPipe's, IoU 0.3).
pub fn decode_blazeface(regressors: &[f32], scores: &[f32], min_score: f32) -> Vec<Detection> {
    let anchors = blazeface_anchors();
    let s = BLAZEFACE_INPUT as f32;
    let mut raw = Vec::new();
    for (i, a) in anchors.iter().enumerate() {
        let (Some(logit), Some(r)) = (scores.get(i), regressors.get(i * 16..i * 16 + 16)) else { break };
        let score = sigmoid(*logit);
        if score.is_nan() || score < min_score {
            continue;
        }
        let (cx, cy, w, h) = (r[0] / s + a[0], r[1] / s + a[1], r[2] / s, r[3] / s);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            continue;
        }
        let mut keypoints = [[0f32; 2]; 6];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = [r[4 + 2 * k] / s + a[0], r[5 + 2 * k] / s + a[1]];
        }
        raw.push(Detection { x0: cx - w / 2.0, y0: cy - h / 2.0, x1: cx + w / 2.0, y1: cy + h / 2.0, score, keypoints });
    }
    weighted_nms(raw, 0.3)
}

/// MediaPipe's weighted NMS: each cluster of boxes overlapping the best one by more than `iou`
/// becomes their score-weighted average.
fn weighted_nms(mut dets: Vec<Detection>, iou: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out = Vec::new();
    while let Some(best) = dets.first().cloned() {
        let (cluster, rest): (Vec<Detection>, Vec<Detection>) = dets.into_iter().partition(|d| d.iou(&best) > iou);
        dets = rest;
        let total: f32 = cluster.iter().map(|d| d.score).sum();
        if cluster.is_empty() || total <= 0.0 {
            out.push(best);
            continue;
        }
        let avg = |f: &dyn Fn(&Detection) -> f32| cluster.iter().map(|d| f(d) * d.score).sum::<f32>() / total;
        let mut keypoints = [[0f32; 2]; 6];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = [avg(&|d| d.keypoints[k][0]), avg(&|d| d.keypoints[k][1])];
        }
        out.push(Detection { x0: avg(&|d| d.x0), y0: avg(&|d| d.y0), x1: avg(&|d| d.x1), y1: avg(&|d| d.y1), score: best.score, keypoints });
    }
    out
}

/// Faces found in several overlapping windows, merged: a face seen twice keeps its best
/// detection (overlap relative to the smaller box above `thr`).
pub fn merge_windows(mut dets: Vec<Detection>, thr: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| k.overlap(&d) <= thr) {
            keep.push(d);
        }
    }
    keep
}

/// The windows (left, top, side in pixels) BlazeFace short range looks at in a `w × h` image: the
/// whole image (letterboxed to a square) and overlapping squares of 45 % of the short edge — it
/// only finds faces of at least ~20 % of its input, and photos have smaller ones.
pub fn face_windows(w: usize, h: usize) -> Vec<(usize, usize, usize)> {
    let mut out = vec![(0, 0, w.max(h))];
    let side = (w.min(h) * 45 / 100).max(1);
    let step = (side / 2).max(1);
    let starts = |len: usize| {
        let mut v: Vec<usize> = (0..).map(|i| i * step).take_while(|s| s + side < len).collect();
        v.push(len.saturating_sub(side));
        v.dedup();
        v
    };
    for y in starts(h) {
        for x in starts(w) {
            out.push((x, y, side));
        }
    }
    out
}

/// A 2 × 3 affine map (row-major): `[x', y'] = [a·x + b·y + c, d·x + e·y + f]`.
pub type Affine = [f32; 6];

/// The map from photo pixels to FaceMesh's 256 × 256 input for `det` (photo pixels): the box
/// enlarged 1.5×, centred, rotated so the eyes are level, as MediaPipe's graph crops it.
pub fn mesh_crop(det: &Detection) -> Affine {
    let [cx, cy] = det.center();
    let [rx, ry] = det.keypoints[0];
    let [lx, ly] = det.keypoints[1];
    let angle = (ly - ry).atan2(lx - rx);
    let side = 1.5 * det.width().max(det.height()).max(1.0);
    let s = MESH_INPUT as f32 / side;
    let (sin, cos) = (-angle).sin_cos();
    // rotate by −angle about the centre, scale, move the centre to the crop's centre
    let (a, b, d, e) = (s * cos, -s * sin, s * sin, s * cos);
    let half = MESH_INPUT as f32 / 2.0;
    [a, b, half - (a * cx + b * cy), d, e, half - (d * cx + e * cy)]
}

/// The inverse of an affine map (identity-like garbage for a degenerate one is avoided: `None`).
pub fn invert(m: &Affine) -> Option<Affine> {
    let [a, b, c, d, e, f] = *m;
    let det = a * e - b * d;
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
    Some([ia, ib, -(ia * c + ib * f), id, ie, -(id * c + ie * f)])
}

/// Sample an `out × out` RGB crop of `rgb` (`w × h`, 8-bit) through `m` (photo → crop), bilinear,
/// black outside, as `[out, out, 3]` floats in `lo..hi` (`0..1` or `−1..1`).
pub fn warp(rgb: &[u8], w: usize, h: usize, m: &Affine, out: usize, lo: f32, hi: f32) -> Vec<f32> {
    let mut v = vec![lo; out * out * 3];
    let Some(inv) = invert(m) else { return v };
    let px = |x: isize, y: isize, c: usize| -> f32 {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return 0.0;
        }
        f32::from(rgb.get((y as usize * w + x as usize) * 3 + c).copied().unwrap_or(0))
    };
    let scale = (hi - lo) / 255.0;
    for oy in 0..out {
        for ox in 0..out {
            let (fx, fy) = (ox as f32 + 0.5, oy as f32 + 0.5);
            let sx = inv[0] * fx + inv[1] * fy + inv[2] - 0.5;
            let sy = inv[3] * fx + inv[4] * fy + inv[5] - 0.5;
            if !(sx.is_finite() && sy.is_finite()) {
                continue;
            }
            let (x0, y0) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            for c in 0..3 {
                let top = px(x0, y0, c) * (1.0 - tx) + px(x0 + 1, y0, c) * tx;
                let bot = px(x0, y0 + 1, c) * (1.0 - tx) + px(x0 + 1, y0 + 1, c) * tx;
                if let Some(d) = v.get_mut((oy * out + ox) * 3 + c) {
                    *d = lo + (top * (1.0 - ty) + bot * ty) * scale;
                }
            }
        }
    }
    v
}

/// A square window of `rgb` (left, top, side in pixels; black outside the image) resized to `out`
/// for BlazeFace, as `[out, out, 3]` in −1..1.
pub fn window(rgb: &[u8], w: usize, h: usize, (x, y, side): (usize, usize, usize), out: usize) -> Vec<f32> {
    let s = out as f32 / side.max(1) as f32;
    let m: Affine = [s, 0.0, -(x as f32) * s, 0.0, s, -(y as f32) * s];
    warp(rgb, w, h, &m, out, -1.0, 1.0)
}

/// FaceMesh's raw landmarks (crop pixels, `[478 × 3]`) back to photo pixels through the crop map.
pub fn landmarks_to_photo(raw: &[f32], m: &Affine) -> Option<Vec<[f32; 2]>> {
    let inv = invert(m)?;
    let pts: Vec<[f32; 2]> = raw
        .as_chunks::<3>()
        .0
        .iter()
        .take(MESH_POINTS)
        .map(|p| [inv[0] * p[0] + inv[1] * p[1] + inv[2], inv[3] * p[0] + inv[4] * p[1] + inv[5]])
        .collect();
    (pts.len() == MESH_POINTS && pts.iter().all(|p| p[0].is_finite() && p[1].is_finite())).then_some(pts)
}

/// A face part made from landmarks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacePart {
    Brows,
    /// The whites of the eyes (the eye opening minus the iris).
    Sclera,
    /// Iris and pupil.
    Iris,
    Lips,
    Teeth,
}

impl FacePart {
    /// The part a Lightroom People part name means (`"Eyebrows"`, `"Eye Sclera"`, `"Iris and
    /// Pupil"`, `"Lips"`, `"Teeth"`), case-insensitively.
    pub fn parse(name: &str) -> Option<FacePart> {
        let n = name.to_ascii_lowercase();
        if n.contains("brow") {
            Some(FacePart::Brows)
        } else if n.contains("sclera") || n == "eyes" || n == "eye whites" {
            Some(FacePart::Sclera)
        } else if n.contains("iris") || n.contains("pupil") {
            Some(FacePart::Iris)
        } else if n.contains("lip") {
            Some(FacePart::Lips)
        } else if n.contains("teeth") || n.contains("tooth") {
            Some(FacePart::Teeth)
        } else {
            None
        }
    }
}

/// A grid of `side × side` cells over the normalized rectangle `rect` (`[x0, y0, x1, y1]` of a
/// `w × h` photo), as a stored segmentation covers it.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub side: usize,
    pub rect: [f64; 4],
    pub w: usize,
    pub h: usize,
}

impl Grid {
    /// Photo pixel of cell `(i, j)`'s centre.
    pub fn centre(&self, i: usize, j: usize) -> [f32; 2] {
        self.photo(i, j, 0.5, 0.5)
    }

    /// Photo pixel of cell `(i, j)`'s sub-sample `(sx, sy)` (0..1 within the cell).
    fn photo(&self, i: usize, j: usize, sx: f32, sy: f32) -> [f32; 2] {
        let u = (i as f64 + f64::from(sx)) / self.side as f64;
        let v = (j as f64 + f64::from(sy)) / self.side as f64;
        let x = (self.rect[0] + u * (self.rect[2] - self.rect[0])) * self.w as f64;
        let y = (self.rect[1] + v * (self.rect[3] - self.rect[1])) * self.h as f64;
        [x as f32, y as f32]
    }

    /// The cells whose area meets the photo-pixel box `bb` (`[x0, y0, x1, y1]`), as half-open
    /// ranges of columns and rows (empty when it lies outside the grid).
    fn cells(&self, bb: [f32; 4]) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let (rw, rh) = ((self.rect[2] - self.rect[0]) * self.w as f64, (self.rect[3] - self.rect[1]) * self.h as f64);
        if !(rw > 0.0 && rh > 0.0) || bb.iter().any(|v| !v.is_finite()) {
            return (0..0, 0..0);
        }
        let s = self.side as f64;
        let col = |x: f32| ((f64::from(x) - self.rect[0] * self.w as f64) / rw * s).clamp(0.0, s);
        let row = |y: f32| ((f64::from(y) - self.rect[1] * self.h as f64) / rh * s).clamp(0.0, s);
        (col(bb[0]).floor() as usize..col(bb[2]).ceil() as usize, row(bb[1]).floor() as usize..row(bb[3]).ceil() as usize)
    }
}

/// The bounding box of `pts` grown by `m` pixels.
fn bbox(pts: &[[f32; 2]], m: f32) -> [f32; 4] {
    pts.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0] - m), b[1].min(p[1] - m), b[2].max(p[0] + m), b[3].max(p[1] + m)])
}

/// Whether `p` lies inside the closed polygon `poly` (even–odd rule).
fn inside(poly: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

/// Coverage 0..1 of each grid cell inside the photo-pixel box `bb` by the region `f` (4 × 4
/// samples per cell, for smooth edges).
fn coverage(grid: &Grid, cov: &mut [f32], bb: [f32; 4], f: impl Fn([f32; 2]) -> bool) {
    const N: usize = 4;
    let (cols, rows) = grid.cells(bb);
    for j in rows {
        for i in cols.clone() {
            let mut hit = 0;
            for sy in 0..N {
                for sx in 0..N {
                    if f(grid.photo(i, j, (sx as f32 + 0.5) / N as f32, (sy as f32 + 0.5) / N as f32)) {
                        hit += 1;
                    }
                }
            }
            if let Some(c) = cov.get_mut(j * grid.side + i) {
                *c = c.max(hit as f32 / (N * N) as f32);
            }
        }
    }
}

fn points(lm: &[[f32; 2]], idx: &[usize]) -> Vec<[f32; 2]> {
    idx.iter().filter_map(|i| lm.get(*i).copied()).collect()
}

/// The iris disc of eye `e` (centre and radius in photo pixels).
fn iris(lm: &[[f32; 2]], e: usize) -> Option<([f32; 2], f32)> {
    let (c, rim) = IRISES.get(e)?;
    let c = *lm.get(*c)?;
    let r = rim.iter().filter_map(|i| lm.get(*i)).map(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt()).sum::<f32>() / 4.0;
    Some((c, r))
}

/// Accumulate face part `part` of one face (`lm`: its 478 landmarks in photo pixels) into the
/// coverage grid `cov`. Teeth also need the photo (`rgb`, `w × h`): the bright, unsaturated pixels
/// inside the mouth opening.
pub fn rasterize(grid: &Grid, cov: &mut [f32], lm: &[[f32; 2]], part: FacePart, rgb: &[u8]) {
    if lm.len() < MESH_POINTS {
        return;
    }
    match part {
        FacePart::Brows => {
            for (upper, lower) in BROWS {
                let mut poly = points(lm, &upper);
                poly.extend(points(lm, &lower).into_iter().rev());
                coverage(grid, cov, bbox(&poly, 1.0), |p| inside(&poly, p));
            }
        }
        FacePart::Sclera | FacePart::Iris => {
            for (e, eye) in EYES.iter().enumerate() {
                let poly = points(lm, eye);
                let Some((c, r)) = iris(lm, e) else { continue };
                let in_iris = |p: [f32; 2]| (p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) <= r * r;
                let bb = bbox(&poly, 1.0);
                match part {
                    FacePart::Iris => coverage(grid, cov, bb, |p| in_iris(p) && inside(&poly, p)),
                    _ => coverage(grid, cov, bb, |p| inside(&poly, p) && !in_iris(p)),
                }
            }
        }
        FacePart::Lips => {
            let (outer, inner) = (points(lm, &LIPS_OUTER), points(lm, &LIPS_INNER));
            coverage(grid, cov, bbox(&outer, 1.0), |p| inside(&outer, p) && !inside(&inner, p));
        }
        FacePart::Teeth => {
            let inner = points(lm, &LIPS_INNER);
            let (w, h) = (grid.w, grid.h);
            // the mouth's brightness: teeth are the bright, unsaturated part of the opening
            let sample = |p: [f32; 2]| -> Option<(f32, f32)> {
                let (x, y) = (p[0].round(), p[1].round());
                if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                    return None;
                }
                let i = (y as usize * w + x as usize) * 3;
                let px = rgb.get(i..i + 3)?;
                let (r, g, b) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                Some((0.299 * r + 0.587 * g + 0.114 * b, if max > 0.0 { (max - min) / max } else { 0.0 }))
            };
            let (mut lum, mut n) = (0f32, 0f32);
            let (x0, x1) = inner.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = inner.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            if !(x1 > x0 && y1 > y0) {
                return;
            }
            let step = ((x1 - x0).max(y1 - y0) / 24.0).max(1.0);
            let mut y = y0;
            while y <= y1 {
                let mut x = x0;
                while x <= x1 {
                    if inside(&inner, [x, y])
                        && let Some((l, _)) = sample([x, y])
                    {
                        lum += l;
                        n += 1.0;
                    }
                    x += step;
                }
                y += step;
            }
            if n < 1.0 {
                return;
            }
            let mean = lum / n;
            coverage(grid, cov, bbox(&inner, 1.0), |p| inside(&inner, p) && sample(p).is_some_and(|(l, s)| l > mean.max(70.0) && s < 0.45));
        }
    }
}

/// `poly` scaled about its centroid by `sx` horizontally and `sy` vertically.
fn grown(poly: &[[f32; 2]], sx: f32, sy: f32) -> Vec<[f32; 2]> {
    let n = poly.len().max(1) as f32;
    let c = poly.iter().fold([0f32; 2], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
    poly.iter().map(|p| [c[0] + (p[0] - c[0]) * sx, c[1] + (p[1] - c[1]) * sy]).collect()
}

/// The ellipse an eye's cut-out takes (centre, unit axis from corner to corner, semi-axes along
/// and across it, photo pixels): the eye's width plus a little for the corners and lashes, and at
/// least a third of its width tall, wider when the eye is wide open. `None` for a degenerate eye.
fn eye_ellipse(lm: &[[f32; 2]], eye: &[usize; 16]) -> Option<([f32; 2], [f32; 2], f32, f32)> {
    let (a, b) = (*lm.get(eye[0])?, *lm.get(eye[8])?);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let w = dx.hypot(dy);
    if !(w.is_finite() && w > 0.5) {
        return None;
    }
    let ax = [dx / w, dy / w];
    let pts = points(lm, eye);
    let n = pts.len().max(1) as f32;
    let c = pts.iter().fold([0f32; 2], |s, p| [s[0] + p[0] / n, s[1] + p[1] / n]);
    // the opening's height across the axis
    let across = pts.iter().map(|p| -(p[0] - c[0]) * ax[1] + (p[1] - c[1]) * ax[0]).fold((0f32, 0f32), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let open = across.1 - across.0;
    let semi_b = (0.8 * open).max(0.3 * w);
    (c.iter().all(|v| v.is_finite()) && semi_b.is_finite()).then_some((c, ax, 0.62 * w, semi_b))
}

/// A brow's cut-out: the band between its upper and lower lines, grown, and at least a fifth of
/// its length thick (FaceMesh often draws thin or plucked brows as two lines close together, and
/// the hairs beyond them are brow too).
fn brow_band(upper: &[[f32; 2]], lower: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let len = upper.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).sum::<f32>();
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for (u, l) in upper.iter().zip(lower) {
        let m = [(u[0] + l[0]) / 2.0, (u[1] + l[1]) / 2.0];
        let (nx, ny) = (u[0] - l[0], u[1] - l[1]);
        let d = nx.hypot(ny);
        let half = (0.5 * d * 1.4).max(0.1 * len);
        let n = if d > 1e-3 { [nx / d, ny / d] } else { [0.0, -1.0] };
        top.push([m[0] + n[0] * half, m[1] + n[1] * half]);
        bottom.push([m[0] - n[0] * half, m[1] - n[1] * half]);
    }
    let mut poly = grown(&top, 1.08, 1.0);
    poly.extend(grown(&bottom, 1.08, 1.0).into_iter().rev());
    poly
}

/// Forehead landmarks the face's own skin is sampled at (mid forehead, above each brow).
const FOREHEAD: [usize; 3] = [151, 108, 337];
/// The top and bottom of the face oval (forehead top, chin).
const OVAL_TOP: usize = 10;
const CHIN: usize = 152;

/// Mean and spread of the opponent colour (0..1 scale) in a disc of radius `r` around `c`.
fn colour_stats(rgb: &[u8], w: usize, h: usize, pts: &[[f32; 2]], r: f32) -> Option<([f32; 3], [f32; 3])> {
    let mut v = Vec::new();
    for c in pts {
        let step = (r / 3.0).max(1.0);
        let mut y = c[1] - r;
        while y <= c[1] + r {
            let mut x = c[0] - r;
            while x <= c[0] + r {
                if let Some(o) = opponent_at(rgb, w, h, [x, y]) {
                    v.push(o);
                }
                x += step;
            }
            y += step;
        }
    }
    if v.len() < 6 {
        return None;
    }
    let n = v.len() as f32;
    let mean = v.iter().fold([0f32; 3], |m, o| [m[0] + o[0] / n, m[1] + o[1] / n, m[2] + o[2] / n]);
    let var = v
        .iter()
        .fold([0f32; 3], |m, o| [m[0] + (o[0] - mean[0]).powi(2) / n, m[1] + (o[1] - mean[1]).powi(2) / n, m[2] + (o[2] - mean[2]).powi(2) / n]);
    Some((mean, var.map(f32::sqrt)))
}

/// The photo's colour at `p` (3 × 3 mean) as opponent channels: lightness, red–green, yellow–blue.
fn opponent_at(rgb: &[u8], w: usize, h: usize, p: [f32; 2]) -> Option<[f32; 3]> {
    let (cx, cy) = (p[0].round() as isize, p[1].round() as isize);
    if !(p[0].is_finite() && p[1].is_finite()) || cx < 1 || cy < 1 || cx + 1 >= w as isize || cy + 1 >= h as isize {
        return None;
    }
    let mut s = [0f32; 3];
    for dy in -1..=1isize {
        for dx in -1..=1isize {
            let i = ((cy + dy) as usize * w + (cx + dx) as usize) * 3;
            let px = rgb.get(i..i + 3)?;
            for (a, b) in s.iter_mut().zip(px) {
                *a += f32::from(*b) / (255.0 * 9.0);
            }
        }
    }
    Some([(s[0] + s[1] + s[2]) / 3.0, s[0] - s[1], (s[0] + s[1]) / 2.0 - s[2]])
}

/// Face skin up to the hairline (one face, `lm` in photo pixels): Selfie Multiclass's face-skin
/// class fades out on the upper forehead (its 256-cell map, a high forehead, a receding hairline),
/// so above the brows, inside the face oval stretched up to where the hair starts, cells coloured
/// like the face's own forehead (sampled at [`FOREHEAD`]) and not hair (`hair(u, v)`: the class
/// map's hair probability at a normalized point) are skin. Raises `skin` (coverage 0..1), never
/// lowers it.
pub fn forehead(grid: &Grid, skin: &mut [f32], lm: &[[f32; 2]], rgb: &[u8], hair: impl Fn(f32, f32) -> f32) {
    if lm.len() < MESH_POINTS || grid.w == 0 || grid.h == 0 {
        return;
    }
    let (Some(top), Some(chin)) = (lm.get(OVAL_TOP).copied(), lm.get(CHIN).copied()) else { return };
    let (ux, uy) = (top[0] - chin[0], top[1] - chin[1]);
    let face_h = ux.hypot(uy);
    if !(face_h.is_finite() && face_h > 4.0) {
        return;
    }
    let up = [ux / face_h, uy / face_h];
    // the brow line: the highest brow point along `up`
    let height = |p: [f32; 2]| (p[0] - chin[0]) * up[0] + (p[1] - chin[1]) * up[1];
    let brow = BROWS.iter().flat_map(|(u, _)| u.iter()).filter_map(|i| lm.get(*i)).map(|p| height(*p)).fold(f32::MIN, f32::max);
    if !brow.is_finite() {
        return;
    }
    // the oval with its upper part stretched up by up to a fifth of the face's height
    let oval: Vec<[f32; 2]> = points(lm, &FACE_OVAL)
        .into_iter()
        .map(|p| {
            let t = ((height(p) - brow) / (face_h - brow).max(1.0)).clamp(0.0, 1.0);
            let k = 0.2 * face_h * t;
            [p[0] + up[0] * k, p[1] + up[1] * k]
        })
        .collect();
    let Some((mean, spread)) = colour_stats(rgb, grid.w, grid.h, &points(lm, &FOREHEAD), (face_h * 0.03).max(1.5)) else { return };
    // how far a colour may stray from the forehead's (lightness varies with the light across it)
    let tol = [spread[0].max(0.05) * 3.0, spread[1].max(0.02) * 3.0, spread[2].max(0.02) * 3.0];
    let (gw, gh) = (grid.w as f32, grid.h as f32);
    let like = |p: [f32; 2]| -> f32 {
        if height(p) < brow || !inside(&oval, p) {
            return 0.0;
        }
        let Some(o) = opponent_at(rgb, grid.w, grid.h, p) else { return 0.0 };
        let d: f32 = (0..3).map(|i| ((o[i] - mean[i]) / tol[i]).powi(2)).sum();
        let sim = (1.0 - d).clamp(0.0, 1.0);
        sim * (1.0 - 1.5 * hair(p[0] / gw, p[1] / gh)).clamp(0.0, 1.0)
    };
    let bb = bbox(&oval, 1.0);
    let (cols, rows) = grid.cells(bb);
    for j in rows {
        for i in cols.clone() {
            let v = like(grid.centre(i, j));
            if let Some(c) = skin.get_mut(j * grid.side + i) {
                *c = c.max(v);
            }
        }
    }
}

/// Luminance (0..255) of the photo at `p`, and its spread (standard deviation) over the 5 × 5
/// pixels around it — how textured the spot is.
fn luma_texture(rgb: &[u8], w: usize, h: usize, p: [f32; 2]) -> Option<(f32, f32)> {
    let (cx, cy) = (p[0].round() as isize, p[1].round() as isize);
    if cx < 2 || cy < 2 || cx + 2 >= w as isize || cy + 2 >= h as isize {
        return None;
    }
    let (mut sum, mut sq, mut centre) = (0f32, 0f32, 0f32);
    for dy in -2..=2isize {
        for dx in -2..=2isize {
            let i = ((cy + dy) as usize * w + (cx + dx) as usize) * 3;
            let px = rgb.get(i..i + 3)?;
            let l = 0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2]);
            sum += l;
            sq += l * l;
            if dx == 0 && dy == 0 {
                centre = l;
            }
        }
    }
    let mean = sum / 25.0;
    Some((centre, (sq / 25.0 - mean * mean).max(0.0).sqrt()))
}

/// Cheek landmarks (one per side) whose surroundings are the face's reference skin.
const CHEEKS: [usize; 2] = [205, 425];
/// Below the nose (subnasale): the lower face, where beards and moustaches grow, starts here.
const SUBNASALE: usize = 2;

/// What face skin must leave out of one face (`lm`: its 478 landmarks in photo pixels), as
/// coverage 0..1 into `holes`: the eye openings grown for the lashes and lash line, the brows, the
/// mouth (the outer lip line filled: lips, teeth, tongue), and — in the lower face only — facial
/// hair: pixels clearly darker *and* more textured than the cheeks (so skin in shadow, which is
/// dark but smooth, stays). The nostrils stay in the skin (cutting them looked unnatural).
pub fn feature_holes(grid: &Grid, holes: &mut [f32], lm: &[[f32; 2]], rgb: &[u8]) {
    if lm.len() < MESH_POINTS {
        return;
    }
    for eye in &EYES {
        let poly = grown(&points(lm, eye), 1.18, 1.6);
        coverage(grid, holes, bbox(&poly, 1.0), |p| inside(&poly, p));
        // the whole eye, lids and lashes, however far it is open: a smiling or blinking eye's
        // opening is a sliver, and the lashes and lid line around it aren't skin either
        if let Some((c, ax, a, b)) = eye_ellipse(lm, eye) {
            let r = a.max(b);
            coverage(grid, holes, [c[0] - r - 1.0, c[1] - r - 1.0, c[0] + r + 1.0, c[1] + r + 1.0], |p| {
                let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
                let (x, y) = (dx * ax[0] + dy * ax[1], -dx * ax[1] + dy * ax[0]);
                (x / a).powi(2) + (y / b).powi(2) <= 1.0
            });
        }
    }
    for (upper, lower) in BROWS {
        let poly = brow_band(&points(lm, &upper), &points(lm, &lower));
        coverage(grid, holes, bbox(&poly, 1.0), |p| inside(&poly, p));
    }
    let mouth = grown(&points(lm, &LIPS_OUTER), 1.06, 1.1);
    coverage(grid, holes, bbox(&mouth, 1.0), |p| inside(&mouth, p));
    // facial hair: compared with the cheeks' skin
    let (w, h) = (grid.w, grid.h);
    let oval = points(lm, &FACE_OVAL);
    let face_w = bbox(&oval, 0.0);
    let r = ((face_w[2] - face_w[0]) * 0.06).max(2.0);
    let mut cheek = Vec::new();
    for c in CHEEKS {
        let Some(c) = lm.get(c) else { continue };
        let mut y = c[1] - r;
        while y <= c[1] + r {
            let mut x = c[0] - r;
            while x <= c[0] + r {
                if let Some(v) = luma_texture(rgb, w, h, [x, y]) {
                    cheek.push(v);
                }
                x += (r / 4.0).max(1.0);
            }
            y += (r / 4.0).max(1.0);
        }
    }
    if cheek.len() < 8 {
        return;
    }
    let n = cheek.len() as f32;
    let mean = cheek.iter().map(|v| v.0).sum::<f32>() / n;
    let spread = (cheek.iter().map(|v| (v.0 - mean).powi(2)).sum::<f32>() / n).sqrt();
    let texture = cheek.iter().map(|v| v.1).sum::<f32>() / n;
    let Some(top) = lm.get(SUBNASALE).map(|p| p[1]) else { return };
    // (from just below the nose: the nostrils are dark and textured too, and stay skin)
    let start = top + 0.3 * r;
    let mut lower = bbox(&oval, 1.0);
    lower[1] = lower[1].max(start);
    let hair = |p: [f32; 2]| {
        p[1] >= start
            && inside(&oval, p)
            && luma_texture(rgb, w, h, p).is_some_and(|(l, t)| l < mean - (2.0 * spread).max(25.0) && t > (2.5 * texture).max(6.0))
    };
    coverage(grid, holes, lower, hair);
}

/// A 3 × 3 box blur of a coverage grid (`side × side`): soft edges for the holes face features cut.
pub fn soften(grid: &[f32], side: usize) -> Vec<f32> {
    let mut out = grid.to_vec();
    if side < 3 || grid.len() < side * side {
        return out;
    }
    for j in 0..side {
        for i in 0..side {
            let (mut sum, mut n) = (0f32, 0f32);
            for y in j.saturating_sub(1)..=(j + 1).min(side - 1) {
                for x in i.saturating_sub(1)..=(i + 1).min(side - 1) {
                    if let Some(v) = grid.get(y * side + x) {
                        sum += v;
                        n += 1.0;
                    }
                }
            }
            if let Some(o) = out.get_mut(j * side + i) {
                *o = sum / n.max(1.0);
            }
        }
    }
    out
}

/// Coverage (0..1) to logits, clamped so a stored segmentation stays finite.
pub fn coverage_to_logits(cov: &[f32]) -> Vec<f32> {
    cov.iter()
        .map(|p| {
            let p = p.clamp(1e-4, 1.0 - 1e-4);
            (p / (1.0 - p)).ln()
        })
        .collect()
}

/// Softmax over the class axis of Selfie Multiclass's `[256, 256, 6]` logits.
pub fn selfie_softmax(logits: &[f32]) -> Vec<f32> {
    let mut out = logits.to_vec();
    for px in out.as_chunks_mut::<6>().0 {
        let m = px.iter().copied().fold(f32::MIN, f32::max);
        let mut sum = 0.0;
        for v in px.iter_mut() {
            *v = (*v - m).exp();
            sum += *v;
        }
        if sum > 0.0 {
            px.iter_mut().for_each(|v| *v /= sum);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_and_decoding() {
        let a = blazeface_anchors();
        assert_eq!(a.len(), 896);
        assert_eq!(a[0], [1.0 / 32.0, 1.0 / 32.0]);
        assert_eq!(a[512], [1.0 / 16.0, 1.0 / 16.0]);
        // one confident box at anchor 100, a weaker duplicate next to it, the rest below threshold
        let mut reg = vec![0f32; 896 * 16];
        let mut cls = vec![-10f32; 896];
        for (i, score) in [(100usize, 5.0f32), (101, 3.0)] {
            cls[i] = score;
            reg[i * 16 + 2] = 32.0;
            reg[i * 16 + 3] = 32.0;
        }
        let d = decode_blazeface(&reg, &cls, 0.5);
        assert_eq!(d.len(), 1, "duplicates merge");
        assert!((d[0].width() - 0.25).abs() < 1e-5 && d[0].score > 0.99);
        // hostile outputs: short buffers, NaN boxes
        assert!(decode_blazeface(&reg[..10], &cls, 0.5).is_empty());
        let mut nan = reg.clone();
        nan[100 * 16 + 2] = f32::NAN;
        nan[101 * 16 + 2] = f32::NAN;
        assert!(decode_blazeface(&nan, &cls, 0.5).is_empty());
    }

    #[test]
    fn windows_cover_the_photo() {
        let ws = face_windows(1600, 1067);
        assert_eq!(ws[0], (0, 0, 1600));
        let side = 1067 * 45 / 100;
        assert!(ws[1..].iter().all(|(x, y, s)| *s == side && x + s <= 1600 && y + s <= 1067));
        assert!(ws.iter().any(|(x, _, s)| x + s == 1600) && ws.iter().any(|(_, y, s)| y + s == 1067));
        assert!(!face_windows(1, 1).is_empty());
    }

    #[test]
    fn crop_maps_round_trip_and_level_the_eyes() {
        let det = Detection {
            x0: 100.0,
            y0: 100.0,
            x1: 200.0,
            y1: 200.0,
            score: 1.0,
            keypoints: [[120.0, 120.0], [180.0, 140.0], [150.0; 2], [150.0; 2], [0.0; 2], [0.0; 2]],
        };
        let m = mesh_crop(&det);
        let inv = invert(&m).unwrap();
        let ap = |m: &Affine, p: [f32; 2]| [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]];
        let c = ap(&m, [150.0, 150.0]);
        assert!((c[0] - 128.0).abs() < 1e-3 && (c[1] - 128.0).abs() < 1e-3, "centre to centre: {c:?}");
        let (r, l) = (ap(&m, [120.0, 120.0]), ap(&m, [180.0, 140.0]));
        assert!((r[1] - l[1]).abs() < 1e-3, "eyes level: {r:?} {l:?}");
        let back = ap(&inv, ap(&m, [133.0, 177.0]));
        assert!((back[0] - 133.0).abs() < 1e-3 && (back[1] - 177.0).abs() < 1e-3);
        assert!(invert(&[0.0; 6]).is_none());
    }

    #[test]
    fn parts_rasterize_inside_their_landmarks() {
        // a synthetic face: every landmark on a circle, the mouth opening a smaller one inside the lips
        let mut lm = vec![[500.0f32, 500.0]; MESH_POINTS];
        let ring = |idx: &[usize], cx: f32, cy: f32, r: f32, lm: &mut Vec<[f32; 2]>| {
            for (k, i) in idx.iter().enumerate() {
                let a = k as f32 / idx.len() as f32 * std::f32::consts::TAU;
                lm[*i] = [cx + r * a.cos(), cy + r * a.sin()];
            }
        };
        ring(&LIPS_OUTER, 500.0, 600.0, 60.0, &mut lm);
        ring(&LIPS_INNER, 500.0, 600.0, 30.0, &mut lm);
        let grid = Grid { side: 100, rect: [0.0, 0.0, 1.0, 1.0], w: 1000, h: 1000 };
        let mut cov = vec![0f32; 100 * 100];
        rasterize(&grid, &mut cov, &lm, FacePart::Lips, &[]);
        let at = |x: usize, y: usize| cov[y / 10 * 100 + x / 10];
        assert!(at(500, 555) > 0.9, "on the lips");
        assert!(at(500, 600) < 0.1, "the mouth opening is not lips");
        assert!(at(200, 200) == 0.0);
        // teeth: a white mouth opening on a dark photo
        let rgb: Vec<u8> = (0..1000 * 1000)
            .flat_map(|i| if ((i % 1000) as f32 - 500.0).hypot((i / 1000) as f32 - 600.0) < 30.0 { [240, 240, 235] } else { [60, 40, 30] })
            .collect();
        let mut teeth = vec![0f32; 100 * 100];
        rasterize(&grid, &mut teeth, &lm, FacePart::Teeth, &rgb);
        assert!(teeth[60 * 100 + 50] > 0.9 && teeth[55 * 100 + 50] < 0.1);
        // too few landmarks: nothing, no panic
        let mut none = vec![0f32; 100];
        rasterize(&Grid { side: 10, ..grid }, &mut none, &lm[..10], FacePart::Brows, &[]);
        assert!(none.iter().all(|v| *v == 0.0));
        assert_eq!(FacePart::parse("Iris and Pupil"), Some(FacePart::Iris));
        assert_eq!(FacePart::parse("Eye Sclera"), Some(FacePart::Sclera));
        assert_eq!(FacePart::parse("Hair"), None);
    }

    #[test]
    fn softmax_sums_to_one() {
        let p = selfie_softmax(&[1.0, 2.0, 3.0, 0.0, -1.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!((p[..6].iter().sum::<f32>() - 1.0).abs() < 1e-5 && (p[6..].iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(p[5] > p[2]);
    }
}
