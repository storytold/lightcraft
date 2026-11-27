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
        let frame_len = w.checked_mul(3).ok_or_else(|| bad("saturation frame size overflow"))?;
        let slot = (y % 3).checked_mul(w).ok_or_else(|| bad("saturation slot offset overflow"))?;
        let slot_end = slot.checked_add(w).ok_or_else(|| bad("saturation slot end overflow"))?;
        for (frame, buffer) in frames.iter().zip(self.data.chunks_exact_mut(frame_len)) {
            let src = frame.image.data.get(start..end).ok_or_else(|| bad("source row is outside its image"))?;
            let dst = buffer.get_mut(slot..slot_end).ok_or_else(|| bad("missing saturation row"))?;
            for (x, value) in dst.iter_mut().enumerate() {
                let neighbours = src.get(x.saturating_sub(1)..=x.saturating_add(1).min(w - 1)).ok_or_else(|| bad("missing saturation neighbours"))?;
                *value = neighbours.iter().copied().map(maxc).fold(0.0, f32::max);
            }
        }
        Ok(())
    }

    /// Borrow the vertical neighbourhood once per row. Repeating the last row at an edge
    /// keeps the original maximum, including its zero initial value, without pixel offsets.
    pub(super) fn for_row(&self, frame: usize, y: usize) -> Result<[&[f32]; 3]> {
        if y >= self.height {
            return Err(bad("saturation row is outside its image"));
        }
        let base = frame.checked_mul(3).and_then(|v| v.checked_mul(self.width)).ok_or_else(|| bad("saturation frame offset overflow"))?;
        let first = y.saturating_sub(1);
        let last = y.saturating_add(1).min(self.height.saturating_sub(1));
        let row = |sy: usize| {
            let start = (sy % 3).checked_mul(self.width).and_then(|v| base.checked_add(v)).ok_or_else(|| bad("saturation row offset overflow"))?;
            let end = start.checked_add(self.width).ok_or_else(|| bad("saturation row end overflow"))?;
            self.data.get(start..end).ok_or_else(|| bad("missing saturation row"))
        };
        Ok([row(first)?, row(first.saturating_add(1).min(last))?, row(first.saturating_add(2).min(last))?])
    }
}
