//! Validation and lossless storage of HDR inputs. No sample is quantised here.

use crate::{Frame, MergeError, Progress, Result, check};
use lightcraft_geom::{Homography, Point};
use lightcraft_raster::Rgb32f;
use rayon::prelude::*;

const MAX_EDGE: usize = 65_000;
const MAX_PIXELS: usize = 512_000_000;
pub(super) const MAX_FRAMES: usize = 64;

pub(super) fn bad(why: &str) -> MergeError {
    MergeError::Mismatch(format!("HDR merge: {why}"))
}

/// Scan samples across workers without allocating image scratch. Each chunk retains the
/// original serial predicate; images up to 16,384 pixels fit in one chunk.
pub(super) fn samples_finite(image: &Rgb32f) -> bool {
    image.data.par_chunks(16_384).all(|pixels| pixels.iter().flatten().all(|value| value.is_finite()))
}

/// Check before any image indexing, orientation, resampling or dimension-derived allocation.
pub(super) fn validate(frame: &Frame) -> Result<()> {
    let image = &frame.image;
    let pixels = image.width.checked_mul(image.height).ok_or_else(|| bad("pixel count overflow"))?;
    let bytes = pixels.checked_mul(size_of::<[f32; 3]>()).ok_or_else(|| bad("image byte count overflow"))?;
    if image.width == 0
        || image.height == 0
        || image.width > MAX_EDGE
        || image.height > MAX_EDGE
        || pixels > MAX_PIXELS
        || bytes > isize::MAX as usize
    {
        return Err(bad("image dimensions must be nonzero, at most 65,000 pixels per edge and at most 512 MP"));
    }
    if image.data.len() != pixels {
        return Err(bad("pixel buffer length does not match its dimensions"));
    }
    if !frame.clip.is_finite() || frame.clip <= 0.0 || frame.clip * 0.001 == 0.0 || frame.clip * 0.80 >= frame.clip * 0.96 {
        return Err(bad("clip must be finite and positive, with representable thresholds"));
    }
    if frame.exposure.is_some_and(|v| !v.is_finite()) || !frame.baseline_exposure.is_finite() {
        return Err(bad("exposure must be finite"));
    }
    if !samples_finite(image) {
        return Err(bad("RGB samples must be finite"));
    }
    Ok(())
}

/// Move the pixels into retained storage, trimming any capacity left by a raw's default crop.
/// A full-capacity buffer moves without copying its samples. Each source is released before
/// the iterator is advanced to decode the next photo.
pub(super) fn collect(frames: impl IntoIterator<Item = Result<Frame>>, progress: &Progress) -> Result<Vec<Frame>> {
    let mut frames = frames.into_iter();
    if frames.size_hint().0 > MAX_FRAMES {
        return Err(bad("at most 64 frames can be merged in one bracket"));
    }
    let mut stored = Vec::new();
    loop {
        // Advancing a lazy iterator can decode a whole photo. Check before requesting it.
        check(progress, 0.0, "Reading photos")?;
        let Some(next) = frames.next() else { break };
        if stored.len() >= MAX_FRAMES {
            return Err(bad("at most 64 frames can be merged in one bracket"));
        }
        let mut frame = next?;
        validate(&frame)?;
        if frame.image.data.capacity() > frame.image.data.len() {
            let mut packed = Vec::new();
            packed.try_reserve_exact(frame.image.data.len()).map_err(|_| bad("not enough memory to retain the decoded frame"))?;
            packed.extend_from_slice(&frame.image.data);
            frame.image.data = packed;
        }
        stored.push(frame);
    }
    Ok(stored)
}

pub(super) fn pixels<T: Clone + Default>(len: usize) -> Result<Vec<T>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| bad("not enough memory for the merge"))?;
    data.resize(len, T::default());
    Ok(data)
}

/// Write the Lanczos result directly into the retained f32 buffer. Coordinates, taps,
/// neighbour clamp and validity edges are the same as `align::warp` / default `flow::warp`.
pub(super) fn warp(frame: &mut Frame, transform: &Homography) -> Result<Vec<bool>> {
    let source = &frame.image;
    let (width, height) = (source.width, source.height);
    if *transform == Homography::IDENTITY {
        let mut valid = pixels(source.data.len())?;
        valid.fill(true);
        return Ok(valid);
    }
    let mut output = Rgb32f { width, height, data: pixels(source.data.len())? };
    let mut valid = pixels(source.data.len())?;
    output.data.par_chunks_mut(width).zip(valid.par_chunks_mut(width)).enumerate().for_each(|(y, (row, mask))| {
        for (x, (pixel, inside)) in row.iter_mut().zip(mask).enumerate() {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let q = transform.apply(Point::new(px + 0.0, py + 0.0));
            let (sx, sy) = (q.x as f32, q.y as f32);
            if sx >= 0.0 && sy >= 0.0 && sx <= width as f32 && sy <= height as f32 {
                *pixel = source.sample_lanczos3(sx, sy);
                *inside = true;
            }
        }
    });
    if !samples_finite(&output) {
        return Err(bad("resampling arithmetic produced a non-finite value"));
    }
    frame.image = output;
    Ok(valid)
}
