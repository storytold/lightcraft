//! Fast guided-filter windows use subsets of the full frame's resampling taps.

use crate::geometry::ViewWindow;
use lightcraft_raster::{
    Plane, par_rows,
    resample::{Filter, weights_range},
};
use std::ops::Range;

/// Separable taps, indexed relative to the input buffer. Shared with the GPU.
pub type ResampleTaps = [Vec<(usize, Vec<f32>)>; 2];

/// The reduced coefficient rectangle on the full frame's grid. A nominal step
/// `s` actually samples at `full / ceil(full / s)`, separately on each axis.
/// Copying the full-frame taps preserves that scale, phase and edge weights.
pub struct GuidedGrid {
    pub down: ResampleTaps,
    pub up: ResampleTaps,
}

impl GuidedGrid {
    /// Only partial windows need different taps. Full renders (including whole
    /// frame windows) and non-subsampled filters retain their original path.
    pub fn for_window(w: usize, h: usize, step: usize, view: Option<ViewWindow>) -> Option<Self> {
        let view = view?;
        if step <= 1 || w == 0 || h == 0 {
            return None;
        }
        let (fw, fh) = (view.full_w as usize, view.full_h as usize);
        let (x, y) = (view.x as usize, view.y as usize);
        let (end_x, end_y) = (x.checked_add(w)?, y.checked_add(h)?);
        if fw == 0 || fh == 0 || end_x > fw || end_y > fh || (x == 0 && y == 0 && w == fw && h == fh) {
            return None;
        }
        let (dx, ux) = axis(fw, x..end_x, step);
        let (dy, uy) = axis(fh, y..end_y, step);
        Some(Self { down: [dx, dy], up: [ux, uy] })
    }
}

type AxisTaps = Vec<(usize, Vec<f32>)>;

fn axis(full: usize, work: Range<usize>, step: usize) -> (AxisTaps, AxisTaps) {
    let low = full.div_ceil(step);
    // Snap the coefficient rectangle outwards to the full frame's reduced
    // grid. Integer multiples of step would be wrong for odd frame dimensions.
    let first = (work.start as u128 * low as u128 / full as u128) as usize;
    let end = (work.end as u128 * low as u128).div_ceil(full as u128) as usize;
    let reduced = first..end;
    let down = crop_taps(weights_range(full, low, Filter::Box, reduced.clone()), work.clone());
    let up = crop_taps(weights_range(low, full, Filter::Bilinear, work), reduced);
    (down, up)
}

fn crop_taps(taps: AxisTaps, src: Range<usize>) -> AxisTaps {
    taps.into_iter()
        .map(|(lo, ws)| {
            let first = lo.clamp(src.start, src.end - 1);
            let last = lo.saturating_add(ws.len().saturating_sub(1)).clamp(src.start, src.end - 1);
            let mut local = vec![0.0; last - first + 1];
            for (k, wt) in ws.into_iter().enumerate() {
                // Only the outer work boundary can miss taps. Its clamped samples
                // and their filter support are outside the requested rectangle.
                if let Some(v) = local.get_mut((lo + k).clamp(src.start, src.end - 1) - first) {
                    *v += wt;
                }
            }
            (first - src.start, local)
        })
        .collect()
}

/// The same accumulation order as raster::resample::resize, with the shared taps.
pub(crate) fn resize(p: &Plane, taps: &ResampleTaps) -> Plane {
    let [wx, wy] = taps;
    let mut tmp = Plane::new(wx.len(), p.height);
    par_rows(&mut tmp.data, wx.len(), |y, row| {
        let src = p.row(y);
        for (o, (lo, ws)) in row.iter_mut().zip(wx) {
            for (k, wt) in ws.iter().enumerate() {
                if let Some(v) = src.get(lo + k) {
                    *o += v * wt;
                }
            }
        }
    });
    let mut out = Plane::new(wx.len(), wy.len());
    par_rows(&mut out.data, wx.len(), |y, row| {
        if let Some((lo, ws)) = wy.get(y) {
            for (k, wt) in ws.iter().enumerate() {
                for (o, v) in row.iter_mut().zip(tmp.row(lo + k)) {
                    *o += v * wt;
                }
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_taps_match_full_resampling_bit_for_bit() {
        let p = Plane::from_fn(401, 257, |x, y| ((x * 17 + y * 23) % 103) as f32 / 102.0);
        for step in [2, 7, 11, 16] {
            let low = lightcraft_raster::resample::resize(&p, p.width.div_ceil(step), p.height.div_ceil(step), Filter::Box);
            let full = lightcraft_raster::resample::resize(&low, p.width, p.height, Filter::Bilinear);
            for (x, y) in [(0, 0), (19, 37), (200, 106), (0, 53)] {
                let (w, h) = (201, 151);
                let view = ViewWindow { x: x as f64, y: y as f64, full_w: p.width as f64, full_h: p.height as f64 };
                let grid = GuidedGrid::for_window(w, h, step, Some(view)).unwrap();
                let down = resize(&p.crop(x, y, w, h), &grid.down);
                let up = resize(&down, &grid.up);
                let mut compared = 0;
                for yy in 0..h {
                    for xx in 0..w {
                        if (x == 0 || xx >= 2 * step)
                            && (y == 0 || yy >= 2 * step)
                            && (x + w == p.width || xx + 2 * step < w)
                            && (y + h == p.height || yy + 2 * step < h)
                        {
                            assert_eq!(up.get(xx, yy).to_bits(), full.get(x + xx, y + yy).to_bits(), "step={step} ({x},{y}) pixel=({xx},{yy})");
                            compared += 1;
                        }
                    }
                }
                assert!(compared > 1000);
            }
        }
    }

    #[test]
    fn full_frames_and_step_one_keep_original_path() {
        let view = ViewWindow { x: 0.0, y: 0.0, full_w: 401.0, full_h: 257.0 };
        assert!(GuidedGrid::for_window(401, 257, 11, Some(view)).is_none());
        assert!(GuidedGrid::for_window(201, 151, 1, Some(view)).is_none());
        assert!(GuidedGrid::for_window(401, 257, 11, None).is_none());
    }
}
