//! Vectorscope: where a picture's colours sit on the chroma plane (BT.709 Cb / Cr of the
//! display-encoded values), counted into a square grid. Hue is the angle around the centre and
//! saturation the distance from it; a neutral picture is a dot in the middle.

use crate::Rgba8;

/// Cells along each side of the grid.
pub const VECTORSCOPE_N: usize = 128;

/// The angle (degrees, counter-clockwise from +Cb) of the skin tone line ("I line"): skin of
/// every complexion falls near it.
pub const SKIN_TONE_DEGREES: f32 = 123.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Vectorscope {
    /// Cells along each side.
    pub n: usize,
    /// Counts, row-major from the top row (+Cr) down; column 0 is −Cb.
    pub bins: Vec<u32>,
    /// Pixels counted.
    pub total: u32,
}

impl Vectorscope {
    /// BT.709 chroma (Cb, Cr), each −0.5..0.5, of display-encoded RGB (0..1).
    pub fn cbcr(rgb: [f32; 3]) -> (f32, f32) {
        let [r, g, b] = rgb;
        (-0.114_572 * r - 0.385_428 * g + 0.5 * b, 0.5 * r - 0.454_153 * g - 0.045_847 * b)
    }

    /// The cell `(column, row)` of chroma `(cb, cr)` in an `n`-cell grid (clamped to the edge).
    pub fn cell(n: usize, cb: f32, cr: f32) -> (usize, usize) {
        let n1 = n.saturating_sub(1) as f32;
        let at = |v: f32| ((v.clamp(-0.5, 0.5) + 0.5) * n1).round() as usize;
        (at(cb), n.saturating_sub(1).saturating_sub(at(cr)))
    }

    /// Count `img` (8-bit display-encoded) on a [`VECTORSCOPE_N`] grid, reading at most about
    /// `max_samples` pixels evenly spread (every pixel when the picture is smaller). Transparent
    /// pixels are skipped.
    pub fn of_srgb8(img: &Rgba8, max_samples: usize) -> Vectorscope {
        let n = VECTORSCOPE_N;
        let mut bins = vec![0u32; n * n];
        let mut total = 0u32;
        let px = img.width.saturating_mul(img.height);
        // one pixel in `step` along each axis
        let step = if px > max_samples.max(1) { ((px as f64 / max_samples.max(1) as f64).sqrt().ceil() as usize).max(1) } else { 1 };
        for y in (0..img.height).step_by(step) {
            for x in (0..img.width).step_by(step) {
                let Some(p) = img.data.get(y * img.width + x) else { continue };
                if p[3] == 0 {
                    continue;
                }
                let (cb, cr) = Self::cbcr([p[0], p[1], p[2]].map(|v| v as f32 / 255.0));
                let (cx, cy) = Self::cell(n, cb, cr);
                if let Some(b) = bins.get_mut(cy * n + cx) {
                    *b = b.saturating_add(1);
                    total = total.saturating_add(1);
                }
            }
        }
        Vectorscope { n, bins, total }
    }

    /// The largest count in any cell.
    pub fn peak(&self) -> u32 {
        self.bins.iter().copied().max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small picture of one colour.
    fn flat(rgb: [u8; 3]) -> Rgba8 {
        Rgba8::from_fn(20, 10, |_, _| [rgb[0], rgb[1], rgb[2], 255])
    }

    /// Greys land in the centre cell and primaries in their own directions.
    #[test]
    fn greys_sit_in_the_middle_and_colours_around_it() {
        let mid = VECTORSCOPE_N / 2;
        for v in [0u8, 128, 255] {
            let s = Vectorscope::of_srgb8(&flat([v, v, v]), 1 << 20);
            assert_eq!(s.total, 200);
            let (x, y) = (0..s.bins.len()).find(|i| s.bins[*i] > 0).map(|i| (i % s.n, i / s.n)).unwrap();
            assert!(x.abs_diff(mid) <= 1 && y.abs_diff(mid) <= 1, "grey {v} at {x},{y}");
        }
        // red: +Cr (up), a little −Cb (left); blue: +Cb (right); green: down-left
        let at = |rgb| {
            let s = Vectorscope::of_srgb8(&flat(rgb), 1 << 20);
            let i = (0..s.bins.len()).find(|i| s.bins[*i] > 0).unwrap();
            (i % s.n, i / s.n)
        };
        let (rx, ry) = at([255, 0, 0]);
        assert!(ry < mid - 30 && rx < mid, "red {rx},{ry}");
        let (bx, by) = at([0, 0, 255]);
        assert!(bx > mid + 30 && by.abs_diff(mid) < 15, "blue {bx},{by}");
        let (gx, gy) = at([0, 255, 0]);
        assert!(gx < mid - 20 && gy > mid + 20, "green {gx},{gy}");
    }

    /// Photographed skin, light to dark, falls within 10° of the skin tone line.
    #[test]
    fn skin_sits_near_the_skin_tone_line() {
        // skin as photographed, light to dark
        for skin in [[224u8, 172, 140], [220, 170, 150], [235, 195, 175], [180, 120, 100], [120, 70, 55], [90, 56, 37]] {
            let (cb, cr) = Vectorscope::cbcr(skin.map(|v| v as f32 / 255.0));
            let deg = cr.atan2(cb).to_degrees();
            assert!((deg - SKIN_TONE_DEGREES).abs() < 10.0, "{skin:?} at {deg}°");
        }
    }

    /// BT.709 primaries sit at the angles a broadcast vectorscope shows.
    #[test]
    fn primaries_sit_where_a_broadcast_scope_puts_them() {
        let deg = |rgb: [f32; 3]| {
            let (cb, cr) = Vectorscope::cbcr(rgb);
            cr.atan2(cb).to_degrees().rem_euclid(360.0)
        };
        // BT.709 coefficients: red 103°, yellow 175°, green 230°, blue 355°, magenta 50°
        for (rgb, want) in
            [([1.0, 0.0, 0.0], 103.0), ([1.0, 1.0, 0.0], 175.0), ([0.0, 1.0, 0.0], 230.0), ([0.0, 0.0, 1.0], 355.0), ([1.0, 0.0, 1.0], 50.0)]
        {
            assert!((deg(rgb) - want).abs() < 3.0, "{rgb:?}: {}", deg(rgb));
        }
    }

    /// Large pictures are sampled to the budget; empty, transparent and tiny input is safe.
    #[test]
    fn large_pictures_are_sampled_and_odd_input_is_safe() {
        let big = Rgba8::from_fn(1000, 800, |x, y| [(x % 256) as u8, (y % 256) as u8, 100, 255]);
        let s = Vectorscope::of_srgb8(&big, 50_000);
        assert!(s.total > 20_000 && s.total <= 60_000, "{}", s.total);
        assert_eq!(s.bins.iter().map(|b| *b as u64).sum::<u64>(), s.total as u64);
        let empty = Vectorscope::of_srgb8(&Rgba8::new(0, 0), 10);
        assert_eq!((empty.total, empty.peak()), (0, 0));
        let clear = Vectorscope::of_srgb8(&Rgba8::from_fn(4, 4, |_, _| [255, 0, 0, 0]), 10);
        assert_eq!(clear.total, 0, "transparent pixels are skipped");
        assert!(Vectorscope::of_srgb8(&flat([9, 9, 9]), 0).total >= 1, "a budget of 0 still reads a pixel");
        assert_eq!(Vectorscope::cell(1, 0.3, -0.3), (0, 0));
        assert_eq!(Vectorscope::cell(0, 0.3, -0.3), (0, 0));
    }
}
