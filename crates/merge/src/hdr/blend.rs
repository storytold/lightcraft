use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;

use crate::{Frame, Result};

use super::input::{bad, pixels};
use super::rows::SaturationRows;
use super::{maxc, sample_plane, smoothstep};

/// A band holds three f32 saturation rows per input, not full-size planes. No nested parallel
/// work runs while those rows are live, so at most one set per active Rayon worker is needed.
const BAND_ROWS: usize = 64;

pub(super) struct Blend<'a> {
    pub frames: &'a [Frame],
    pub valid: &'a [Option<Vec<bool>>],
    pub k: &'a [f32],
    pub ghost: Option<&'a (Vec<Plane>, usize)>,
    pub reference: usize,
    pub darkest: usize,
}

impl Blend<'_> {
    pub(super) fn render(&self) -> Result<Rgb32f> {
        let first = self.frames.first().ok_or_else(|| bad("missing merge input"))?;
        let (w, h) = (first.width(), first.height());
        if self.k.len() != self.frames.len() || self.valid.len() != self.frames.len() {
            return Err(bad("inconsistent merge inputs"));
        }
        let band = w.checked_mul(BAND_ROWS).ok_or_else(|| bad("merge band size overflow"))?;
        let mut output = Rgb32f { width: w, height: h, data: pixels(first.image.data.len())? };
        output.data.par_chunks_mut(band).enumerate().try_for_each(|(index, data)| -> Result<()> {
            let first_y = index.checked_mul(BAND_ROWS).ok_or_else(|| bad("merge band offset overflow"))?;
            let mut saturation = SaturationRows::new(self.frames, first_y)?;
            for (offset, row) in data.chunks_exact_mut(w).enumerate() {
                let y = first_y.checked_add(offset).ok_or_else(|| bad("merge row offset overflow"))?;
                if offset > 0 {
                    saturation.advance(self.frames, y)?;
                }
                self.row(y, row, &saturation)?;
            }
            Ok(())
        })?;
        if output.data.iter().flatten().any(|v| !v.is_finite()) {
            return Err(bad("exposure arithmetic produced non-finite radiance"));
        }
        Ok(output)
    }

    fn row(&self, y: usize, row: &mut [[f32; 3]], saturation: &SaturationRows) -> Result<()> {
        let reference = self.frames.get(self.reference).ok_or_else(|| bad("missing reference frame"))?;
        let kr = *self.k.get(self.reference).ok_or_else(|| bad("missing reference exposure"))?;
        let start = y.checked_mul(reference.width()).ok_or_else(|| bad("merge row offset overflow"))?;
        for (x, output) in row.iter_mut().enumerate() {
            let pixel = start.checked_add(x).ok_or_else(|| bad("merge pixel offset overflow"))?;
            let ref_pixel = *reference.image.data.get(pixel).ok_or_else(|| bad("missing reference pixel"))?;
            let mut acc = [0f64; 3];
            let mut ws = 0f64;
            let mr = maxc(ref_pixel);
            let bound = if mr >= 0.80 * reference.clip { mr / kr } else { 0.0 };
            for (i, ((frame, mask), &k)) in self.frames.iter().zip(self.valid).zip(self.k).enumerate() {
                if let Some(mask) = mask
                    && !mask.get(pixel).copied().ok_or_else(|| bad("missing validity pixel"))?
                {
                    continue;
                }
                let v = *frame.image.data.get(pixel).ok_or_else(|| bad("missing input pixel"))?;
                let m = saturation.at(i, x, y)?;
                let mut wt = k * (1.0 - smoothstep(0.80 * frame.clip, 0.96 * frame.clip, m));
                if i == self.darkest {
                    wt += 1e-6;
                }
                if i == self.reference {
                    wt += 1e-9;
                } else if bound > 0.0 {
                    wt *= smoothstep(0.35, 0.6, m / k / bound);
                }
                if i != self.reference
                    && let Some((maps, fac)) = self.ghost
                {
                    let map = maps.get(i).ok_or_else(|| bad("missing deghost map"))?;
                    let g = sample_plane(map, (x as f32 + 0.5) / *fac as f32, (y as f32 + 0.5) / *fac as f32);
                    wt *= 1.0 - g;
                }
                if wt <= 0.0 {
                    continue;
                }
                let inv = 1.0 / k;
                for c in 0..3 {
                    acc[c] += (wt * v[c] * inv) as f64;
                }
                ws += wt as f64;
            }
            *output = if ws > 0.0 { acc.map(|a| (a / ws) as f32) } else { ref_pixel };
        }
        Ok(())
    }
}
