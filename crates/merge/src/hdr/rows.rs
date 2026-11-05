//! Three horizontally filtered rows per frame. Vertical maxima use the original top-to-bottom
//! order and clipped edge intervals, including the original zero initial value.

use crate::{Frame, Result};

use super::input::{bad, pixels};
use super::maxc;

pub(super) struct SaturationRows {
    data: Vec<f32>,
    width: usize,
    height: usize,
}

impl SaturationRows {
    pub(super) fn new(frames: &[Frame], first: usize) -> Result<Self> {
        let frame = frames.first().ok_or_else(|| bad("missing saturation input"))?;
        let (width, height) = (frame.width(), frame.height());
        let len = width.checked_mul(3).and_then(|v| v.checked_mul(frames.len())).ok_or_else(|| bad("saturation row size overflow"))?;
        let mut rows = Self { data: pixels(len)?, width, height };
        for y in first.saturating_sub(1)..=first.saturating_add(1).min(height.saturating_sub(1)) {
            rows.read(frames, y)?;
        }
        Ok(rows)
    }

    pub(super) fn advance(&mut self, frames: &[Frame], y: usize) -> Result<()> {
        if let Some(next) = y.checked_add(1).filter(|next| *next < self.height) {
            self.read(frames, next)?;
        }
        Ok(())
    }

    fn read(&mut self, frames: &[Frame], y: usize) -> Result<()> {
        let w = self.width;
        let start = y.checked_mul(w).ok_or_else(|| bad("source row offset overflow"))?;
        let end = start.checked_add(w).ok_or_else(|| bad("source row end overflow"))?;
        let slot = (y % 3) * w;
        for (frame, buffer) in frames.iter().zip(self.data.chunks_exact_mut(3 * w)) {
            let src = frame.image.data.get(start..end).ok_or_else(|| bad("source row is outside its image"))?;
            let dst = buffer.get_mut(slot..slot + w).ok_or_else(|| bad("missing saturation row"))?;
            for (x, value) in dst.iter_mut().enumerate() {
                let neighbours = src.get(x.saturating_sub(1)..=x.saturating_add(1).min(w - 1)).ok_or_else(|| bad("missing saturation neighbours"))?;
                *value = neighbours.iter().copied().map(maxc).fold(0.0, f32::max);
            }
        }
        Ok(())
    }

    #[inline]
    pub(super) fn at(&self, frame: usize, x: usize, y: usize) -> Result<f32> {
        let base = frame.checked_mul(3).and_then(|v| v.checked_mul(self.width)).ok_or_else(|| bad("saturation frame offset overflow"))?;
        let mut value = 0.0f32;
        for sy in y.saturating_sub(1)..=y.saturating_add(1).min(self.height - 1) {
            let offset =
                base.checked_add((sy % 3) * self.width).and_then(|v| v.checked_add(x)).ok_or_else(|| bad("saturation sample offset overflow"))?;
            value = value.max(*self.data.get(offset).ok_or_else(|| bad("missing saturation sample"))?);
        }
        Ok(value)
    }
}
