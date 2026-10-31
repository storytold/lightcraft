//! Pan and zoom (the Playback panel's Pan And Zoom): which part of a photo shows at each moment of
//! its slide.
//!
//! A [`Motion`] runs at constant speed from one [`View`] to another over the slide's time. A view
//! is a window of the photo in normalized coordinates (0–1 across and down) with the photo's own
//! aspect, so it never reaches past the photo's edges and is the same at any resolution. The path
//! comes from a key (playback derives it from its seed, the pass and the photo): the same key
//! always gives the same path, and different keys give different ones.

/// Zoom of the closer view at amount 0 (Low) and at amount 1 (High).
pub const ZOOM_LOW: f64 = 1.01;
pub const ZOOM_HIGH: f64 = 1.12;
/// The wider view's share of the closer view's zoom above 1.
const WIDE_SHARE: f64 = 0.25;

/// A window of the photo: its zoom (1 = the whole photo) and its center (0–1 across and down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f64,
    pub x: f64,
    pub y: f64,
}

impl View {
    /// The whole photo.
    pub const FULL: View = View { zoom: 1.0, x: 0.5, y: 0.5 };

    /// The window as (x0, y0, x1, y1), inside 0–1: a zoom below 1 or not finite is 1, and the
    /// center is kept far enough from the edges for the window to fit.
    pub fn window(&self) -> (f64, f64, f64, f64) {
        let z = if self.zoom.is_finite() { self.zoom.max(1.0) } else { 1.0 };
        let h = 0.5 / z;
        let c = |v: f64| if v.is_finite() { v.clamp(h, 1.0 - h) } else { 0.5 };
        let (x, y) = (c(self.x), c(self.y));
        (x - h, y - h, x + h, y + h)
    }
}

/// A slide's pan and zoom: from `start` when the slide comes up to `end` when its time is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub start: View,
    pub end: View,
}

/// SplitMix64 step.
fn mix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A number in 0–1 from `state`.
fn unit(state: &mut u64) -> f64 {
    (mix(state) >> 11) as f64 / (1u64 << 53) as f64
}

impl Motion {
    /// The path for `key` at `amount` (0 = Low, 1 = High; clamped, not finite = 0.5). One end is
    /// the closer view (zoom [`ZOOM_LOW`] at Low to [`ZOOM_HIGH`] at High), the other a wider one;
    /// whether the slide zooms in or out and where each view sits come from the key.
    pub fn new(key: u64, amount: f64) -> Motion {
        let a = if amount.is_finite() { amount.clamp(0.0, 1.0) } else { 0.5 };
        let close = ZOOM_LOW + (ZOOM_HIGH - ZOOM_LOW) * a;
        let wide = 1.0 + (close - 1.0) * WIDE_SHARE;
        let mut st = key;
        let (z0, z1) = if unit(&mut st) < 0.5 { (wide, close) } else { (close, wide) };
        let mut place = |z: f64| {
            let room = 1.0 - 1.0 / z;
            View { zoom: z, x: 0.5 / z + room * unit(&mut st), y: 0.5 / z + room * unit(&mut st) }
        };
        let start = place(z0);
        let end = place(z1);
        Motion { start, end }
    }

    /// The view at `t` (0 = the slide's start, 1 = its end; clamped, not finite = 0). Zoom and
    /// center move linearly; the window stays inside the photo all the way.
    pub fn at(&self, t: f64) -> View {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let l = |a: f64, b: f64| a + (b - a) * t;
        View { zoom: l(self.start.zoom, self.end.zoom), x: l(self.start.x, self.end.x), y: l(self.start.y, self.end.y) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(v: View) -> bool {
        let (x0, y0, x1, y1) = v.window();
        x0 >= 0.0 && y0 >= 0.0 && x1 <= 1.0 + 1e-12 && y1 <= 1.0 + 1e-12 && x1 > x0 && y1 > y0
    }

    #[test]
    fn the_same_key_gives_the_same_path_and_other_keys_other_paths() {
        assert_eq!(Motion::new(7, 0.5), Motion::new(7, 0.5));
        let paths: Vec<Motion> = (0..32).map(|k| Motion::new(k, 0.5)).collect();
        for (i, a) in paths.iter().enumerate() {
            for b in &paths[i + 1..] {
                assert_ne!(a, b, "every key has its own path");
            }
        }
        assert!(paths.iter().any(|m| m.start.zoom < m.end.zoom) && paths.iter().any(|m| m.start.zoom > m.end.zoom), "in and out");
    }

    #[test]
    fn the_amount_sets_how_far_it_zooms() {
        for k in 0..64 {
            let low = Motion::new(k, 0.0);
            let high = Motion::new(k, 1.0);
            let close = |m: &Motion| m.start.zoom.max(m.end.zoom);
            assert!((close(&low) - ZOOM_LOW).abs() < 1e-12, "{low:?}");
            assert!((close(&high) - ZOOM_HIGH).abs() < 1e-12, "{high:?}");
            assert!(low.start.zoom > 1.0 && low.end.zoom > 1.0, "Low still moves a little");
        }
        assert_eq!(Motion::new(3, 7.0), Motion::new(3, 1.0), "clamped");
        assert_eq!(Motion::new(3, f64::NAN), Motion::new(3, 0.5));
    }

    #[test]
    fn the_window_never_leaves_the_photo() {
        for k in 0..256 {
            let m = Motion::new(k, (k % 5) as f64 / 4.0);
            for i in 0..=20 {
                let v = m.at(i as f64 / 20.0);
                assert!(inside(v), "key {k} at {i}: {v:?} {:?}", v.window());
            }
        }
        let m = Motion::new(1, 1.0);
        assert_eq!(m.at(-1.0), m.start);
        assert_eq!(m.at(2.0), m.end);
        assert_eq!(m.at(f64::NAN), m.start);
        assert_eq!(View::FULL.window(), (0.0, 0.0, 1.0, 1.0));
        assert_eq!(View { zoom: f64::NAN, x: 9.0, y: -1.0 }.window(), (0.0, 0.0, 1.0, 1.0), "bad values: the whole photo");
        assert_eq!(View { zoom: 2.0, x: 0.0, y: 1.0 }.window(), (0.0, 0.5, 0.5, 1.0), "the center is moved in");
    }
}
