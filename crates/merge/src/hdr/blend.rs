use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;

use crate::{Frame, Result};

use super::input::{bad, pixels};
use super::rows::SaturationRows;
use super::{maxc, sample_plane, smoothstep};

/// A band holds three f32 saturation rows per input, not full-size planes. No nested parallel
/// work runs while those rows are live, so at most one set per active Rayon worker is needed.
const BAND_ROWS: usize = 64;
const TILE_PIXELS: usize = 256;

#[derive(Clone, Copy)]
struct Accumulator {
    rgb: [f64; 3],
    weight: f64,
}

#[derive(Clone, Copy)]
struct FrameRow<'a> {
    pixels: &'a [[f32; 3]],
    valid: Option<&'a [bool]>,
    saturation: [&'a [f32]; 3],
    ghost: Option<(&'a Plane, f32)>,
    k: f32,
    clip: f32,
    reference: bool,
    darkest: bool,
}

impl FrameRow<'_> {
    const EMPTY: Self = Self { pixels: &[], valid: None, saturation: [&[]; 3], ghost: None, k: 1.0, clip: 1.0, reference: false, darkest: false };

    fn accumulate(&self, output: &mut [Accumulator], bounds: &[f32], range: std::ops::Range<usize>, y: usize) -> Result<()> {
        let pixels = self.pixels.get(range.clone()).ok_or_else(|| bad("missing input tile"))?;
        let [a, b, c] = self.saturation;
        let a = a.get(range.clone()).ok_or_else(|| bad("missing saturation tile"))?;
        let b = b.get(range.clone()).ok_or_else(|| bad("missing saturation tile"))?;
        let c = c.get(range.clone()).ok_or_else(|| bad("missing saturation tile"))?;
        let valid = self.valid.map(|mask| mask.get(range.clone()).ok_or_else(|| bad("missing validity tile"))).transpose()?;
        // All present slices have the tile's length. The true tail serves only frames with no mask.
        let valid = valid.into_iter().flatten().copied().chain(std::iter::repeat(true));
        let values = output.iter_mut().zip(bounds).zip(pixels).zip(a.iter().zip(b).zip(c)).zip(valid).zip(range);
        for (((((output, &bound), &v), ((&a, &b), &c)), inside), x) in values {
            if !inside {
                continue;
            }
            let m = 0.0f32.max(a).max(b).max(c);
            let mut wt = self.k * (1.0 - smoothstep(0.80 * self.clip, 0.96 * self.clip, m));
            if self.darkest {
                wt += 1e-6;
            }
            if self.reference {
                wt += 1e-9;
            } else if bound > 0.0 {
                wt *= smoothstep(0.35, 0.6, m / self.k / bound);
            }
            if let Some((map, fac)) = self.ghost {
                let g = sample_plane(map, (x as f32 + 0.5) / fac, (y as f32 + 0.5) / fac);
                wt *= 1.0 - g;
            }
            if wt <= 0.0 {
                continue;
            }
            let inv = 1.0 / self.k;
            for (sum, value) in output.rgb.iter_mut().zip(v) {
                *sum += (wt * value * inv) as f64;
            }
            output.weight += wt as f64;
        }
        Ok(())
    }
}

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
        let count = self.frames.len();
        let len = w.checked_mul(h).ok_or_else(|| bad("merge image size overflow"))?;
        if w == 0
            || h == 0
            || count > super::input::MAX_FRAMES
            || self.k.len() != count
            || self.valid.len() != count
            || self.reference >= count
            || self.darkest >= count
            || self.frames.iter().any(|frame| frame.width() != w || frame.height() != h || frame.image.data.len() != len)
            || self.valid.iter().flatten().any(|mask| mask.len() != len)
        {
            return Err(bad("inconsistent merge inputs"));
        }
        if let Some((maps, fac)) = self.ghost
            && (*fac == 0
                || maps.len() != count
                || maps.iter().any(|map| map.width == 0 || map.height == 0 || map.width.checked_mul(map.height) != Some(map.data.len())))
        {
            return Err(bad("inconsistent deghost maps"));
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
        if !super::input::samples_finite(&output) {
            return Err(bad("exposure arithmetic produced non-finite radiance"));
        }
        Ok(output)
    }

    fn row(&self, y: usize, row: &mut [[f32; 3]], saturation: &SaturationRows) -> Result<()> {
        let reference = self.frames.get(self.reference).ok_or_else(|| bad("missing reference frame"))?;
        let kr = *self.k.get(self.reference).ok_or_else(|| bad("missing reference exposure"))?;
        let start = y.checked_mul(reference.width()).ok_or_else(|| bad("merge row offset overflow"))?;
        let end = start.checked_add(row.len()).ok_or_else(|| bad("merge row end overflow"))?;
        let reference_row = reference.image.data.get(start..end).ok_or_else(|| bad("missing reference row"))?;
        let mut frames = [FrameRow::EMPTY; super::input::MAX_FRAMES];
        let frames = frames.get_mut(..self.frames.len()).ok_or_else(|| bad("too many merge inputs"))?;
        for (i, (((entry, frame), mask), &k)) in frames.iter_mut().zip(self.frames).zip(self.valid).zip(self.k).enumerate() {
            let ghost = if i == self.reference {
                None
            } else {
                self.ghost.map(|(maps, fac)| maps.get(i).map(|map| (map, *fac as f32)).ok_or_else(|| bad("missing deghost map"))).transpose()?
            };
            *entry = FrameRow {
                pixels: frame.image.data.get(start..end).ok_or_else(|| bad("missing input row"))?,
                valid: mask.as_ref().map(|mask| mask.get(start..end).ok_or_else(|| bad("missing validity row"))).transpose()?,
                saturation: saturation.for_row(i, y)?,
                ghost,
                k,
                clip: frame.clip,
                reference: i == self.reference,
                darkest: i == self.darkest,
            };
        }
        for (index, (output, reference_pixels)) in row.chunks_mut(TILE_PIXELS).zip(reference_row.chunks(TILE_PIXELS)).enumerate() {
            let first = index.checked_mul(TILE_PIXELS).ok_or_else(|| bad("merge tile offset overflow"))?;
            let last = first.checked_add(output.len()).ok_or_else(|| bad("merge tile end overflow"))?;
            let mut acc = [Accumulator { rgb: [0.0; 3], weight: 0.0 }; TILE_PIXELS];
            let acc = acc.get_mut(..output.len()).ok_or_else(|| bad("invalid merge tile"))?;
            let mut bounds = [0.0; TILE_PIXELS];
            let bounds = bounds.get_mut(..output.len()).ok_or_else(|| bad("invalid reference tile"))?;
            for (bound, &pixel) in bounds.iter_mut().zip(reference_pixels) {
                let mr = maxc(pixel);
                *bound = if mr >= 0.80 * reference.clip { mr / kr } else { 0.0 };
            }
            // Frames still accumulate in brightness order for every pixel. Tile accumulators
            // stay on the stack; no additional image or row allocation is retained.
            for frame in frames.iter() {
                frame.accumulate(acc, bounds, first..last, y)?;
            }
            for ((pixel, &reference), sum) in output.iter_mut().zip(reference_pixels).zip(acc) {
                *pixel = if sum.weight > 0.0 { sum.rgb.map(|value| (value / sum.weight) as f32) } else { reference };
            }
        }
        Ok(())
    }
}
