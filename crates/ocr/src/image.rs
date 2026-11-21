//! The pixel work around the models, as PaddleOCR does it with OpenCV: bilinear resizing (`cv2.resize`), cutting a
//! text line out along its rotated box with bicubic sampling and replicated edges (`cv2.warpPerspective`), and quarter
//! and half turns.

use crate::{Error, Result};

/// Largest image accepted, in pixels per side.
pub const MAX_SIDE: usize = 1 << 15;

/// An 8-bit RGB image, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl Rgb {
    /// `data` is `width × height × 3` bytes.
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(Error::Input(format!("a {width}×{height} image can't be read")));
        }
        if Some(data.len()) != width.checked_mul(height).and_then(|n| n.checked_mul(3)) {
            return Err(Error::Input(format!("{} bytes is not a {width}×{height} RGB image", data.len())));
        }
        Ok(Self { width, height, data })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The pixel at (x, y), the nearest edge pixel outside the image.
    fn at(&self, x: isize, y: isize) -> [f32; 3] {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        let i = (y * self.width + x) * 3;
        match self.data.get(i..i + 3) {
            Some(&[r, g, b]) => [f32::from(r), f32::from(g), f32::from(b)],
            _ => [0.0; 3],
        }
    }

    /// Bilinear resize with OpenCV's pixel-centre alignment (`cv2.resize`, INTER_LINEAR, no antialiasing).
    pub fn resize(&self, width: usize, height: usize) -> Result<Rgb> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(Error::Input(format!("can't resize to {width}×{height}")));
        }
        if width == self.width && height == self.height {
            return Ok(self.clone());
        }
        let taps = |out: usize, len: usize| -> Vec<(isize, isize, f32)> {
            let scale = len as f64 / out as f64;
            (0..out)
                .map(|i| {
                    let s = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
                    let x0 = s.floor();
                    let f = (s - x0) as f32;
                    let x0 = x0 as isize;
                    let x1 = (x0 + 1).min(len as isize - 1);
                    let x0 = x0.min(len as isize - 1);
                    (x0, x1, f)
                })
                .collect()
        };
        let (xs, ys) = (taps(width, self.width), taps(height, self.height));
        let mut data = Vec::with_capacity(width * height * 3);
        for &(y0, y1, fy) in &ys {
            for &(x0, x1, fx) in &xs {
                let (a, b, c, d) = (self.at(x0, y0), self.at(x1, y0), self.at(x0, y1), self.at(x1, y1));
                for k in 0..3 {
                    let top = a[k] + (b[k] - a[k]) * fx;
                    let bottom = c[k] + (d[k] - c[k]) * fx;
                    data.push((top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
        Rgb::new(width, height, data)
    }

    /// The text line inside `quad` (top-left, top-right, bottom-right, bottom-left, in pixels), straightened: as wide as
    /// its longer top/bottom edge and as tall as its longer side, sampled bicubically with replicated edges (PaddleOCR's
    /// `get_rotate_crop_image`). A line at least 1.5 times taller than wide is turned a quarter anticlockwise (vertical
    /// text reads as a row). `None` for a box with no area.
    pub fn crop(&self, quad: &[[f32; 2]; 4]) -> Option<Rgb> {
        let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let [p0, p1, p2, p3] = *quad;
        let w = d(p0, p1).max(d(p2, p3));
        let h = d(p0, p3).max(d(p1, p2));
        if !(w.is_finite() && h.is_finite()) {
            return None;
        }
        let (w, h) = (w as usize, h as usize);
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return None;
        }
        // the destination pixel (u, v) comes from p0 + u·(p1 - p0)/w + v·(p3 - p0)/h (the box is a rectangle)
        let (ux, uy) = ((p1[0] - p0[0]) / w as f32, (p1[1] - p0[1]) / w as f32);
        let (vx, vy) = ((p3[0] - p0[0]) / h as f32, (p3[1] - p0[1]) / h as f32);
        let mut data = Vec::with_capacity(w * h * 3);
        for v in 0..h {
            for u in 0..w {
                let x = p0[0] + u as f32 * ux + v as f32 * vx;
                let y = p0[1] + u as f32 * uy + v as f32 * vy;
                data.extend(self.bicubic(x, y));
            }
        }
        let out = Rgb::new(w, h, data).ok()?;
        if h as f32 / w as f32 >= 1.5 { Some(out.rot90()) } else { Some(out) }
    }

    /// OpenCV's bicubic sample (a = -0.75) at (x, y), edges replicated.
    fn bicubic(&self, x: f32, y: f32) -> [u8; 3] {
        fn weights(t: f32) -> [f32; 4] {
            const A: f32 = -0.75;
            let w0 = ((A * (t + 1.0) - 5.0 * A) * (t + 1.0) + 8.0 * A) * (t + 1.0) - 4.0 * A;
            let w1 = ((A + 2.0) * t - (A + 3.0)) * t * t + 1.0;
            let w2 = ((A + 2.0) * (1.0 - t) - (A + 3.0)) * (1.0 - t) * (1.0 - t) + 1.0;
            [w0, w1, w2, 1.0 - w0 - w1 - w2]
        }
        let (fx, fy) = (x.floor(), y.floor());
        let (wx, wy) = (weights(x - fx), weights(y - fy));
        let (ix, iy) = (fx as isize, fy as isize);
        let mut acc = [0f32; 3];
        for (j, wyj) in wy.iter().enumerate() {
            for (i, wxi) in wx.iter().enumerate() {
                let p = self.at(ix + i as isize - 1, iy + j as isize - 1);
                for k in 0..3 {
                    acc[k] += p[k] * wxi * wyj;
                }
            }
        }
        acc.map(|v| v.round().clamp(0.0, 255.0) as u8)
    }

    /// A quarter turn anticlockwise (`np.rot90`).
    pub fn rot90(&self) -> Rgb {
        let (w, h) = (self.width, self.height);
        let mut data = Vec::with_capacity(self.data.len());
        for i in 0..w {
            for j in 0..h {
                let p = self.at((w - 1 - i) as isize, j as isize);
                data.extend(p.map(|v| v as u8));
            }
        }
        Rgb { width: h, height: w, data }
    }

    /// A half turn.
    pub fn rot180(&self) -> Rgb {
        let mut data = Vec::with_capacity(self.data.len());
        for px in self.data.as_chunks::<3>().0.iter().rev() {
            data.extend_from_slice(px);
        }
        Rgb { width: self.width, height: self.height, data }
    }
}

#[cfg(test)]
mod tests {
    use super::Rgb;

    fn gray(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Rgb {
        let data = (0..h).flat_map(|y| (0..w).flat_map(|x| [f(x, y); 3]).collect::<Vec<_>>()).collect::<Vec<_>>();
        Rgb::new(w, h, data).unwrap()
    }

    #[test]
    fn bad_sizes_are_errors() {
        assert!(Rgb::new(0, 4, vec![]).is_err());
        assert!(Rgb::new(2, 2, vec![0; 11]).is_err());
        assert!(Rgb::new(usize::MAX, 2, vec![]).is_err());
        assert!(gray(2, 2, |_, _| 0).resize(0, 3).is_err());
    }

    #[test]
    fn resize_matches_opencv_on_a_ramp() {
        // cv2.resize([[0, 100]], (4, 1)) = [[0, 25, 75, 100]]
        let r = gray(2, 1, |x, _| if x == 0 { 0 } else { 100 }).resize(4, 1).unwrap();
        assert_eq!(r.data().chunks(3).map(|p| p[0]).collect::<Vec<_>>(), vec![0, 25, 75, 100]);
    }

    #[test]
    fn crop_straightens_and_turns_tall_lines() {
        let img = gray(10, 10, |x, y| (x * 10 + y) as u8);
        let c = img.crop(&[[2.0, 3.0], [6.0, 3.0], [6.0, 5.0], [2.0, 5.0]]).unwrap();
        assert_eq!((c.width(), c.height()), (4, 2));
        assert_eq!(c.data()[0], 23, "starts at (2, 3)");
        // a tall box (2 wide, 6 high) is turned a quarter anticlockwise
        let t = img.crop(&[[1.0, 1.0], [3.0, 1.0], [3.0, 7.0], [1.0, 7.0]]).unwrap();
        assert_eq!((t.width(), t.height()), (6, 2));
        assert!(img.crop(&[[1.0, 1.0]; 4]).is_none());
        assert!(img.crop(&[[f32::NAN, 1.0]; 4]).is_none());
    }

    #[test]
    fn turns() {
        let img = gray(3, 2, |x, y| (y * 3 + x) as u8);
        let r = img.rot90();
        // np.rot90([[0,1,2],[3,4,5]]) = [[2,5],[1,4],[0,3]]
        assert_eq!(r.data().chunks(3).map(|p| p[0]).collect::<Vec<_>>(), vec![2, 5, 1, 4, 0, 3]);
        assert_eq!(img.rot180().data().chunks(3).map(|p| p[0]).collect::<Vec<_>>(), vec![5, 4, 3, 2, 1, 0]);
    }
}
