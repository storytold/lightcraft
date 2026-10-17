//! Running a filter over an interleaved RGB `f32` buffer, band by band.
//! Ported from the PhotoCraft plug-in host (`crates/plugins/src/apply.rs`), adapted from tiled
//! surfaces to a plain buffer.

use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use crate::runtime::{Deadline, Filter};
use crate::{Error, Result};

/// RGB has three channels; the buffer has no alpha.
const CH: usize = 3;
/// `pc_filter`'s `format` for 32-bit float RGB without alpha: depth 32, colour mode 3 (RGB).
const FORMAT_RGB32: u32 = 32 | (3 << 16);

/// Where the buffer sits in the full image, so position-dependent filters (gradients, noise
/// seeded by position) line up across tiles and bands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// Left edge of the buffer in the full image, in pixels.
    pub x: u32,
    /// Top edge of the buffer in the full image, in pixels.
    pub y: u32,
    /// Width of the full image, in pixels.
    pub full_w: u32,
    /// Height of the full image, in pixels.
    pub full_h: u32,
}

impl FrameInfo {
    /// The buffer is the whole image.
    pub fn whole(width: u32, height: u32) -> Self {
        Self { x: 0, y: 0, full_w: width, full_h: height }
    }
}

/// A band of output pixels: `[x0, x1) × [y0, y1)` in buffer coordinates.
#[derive(Clone, Copy, Debug)]
struct Band {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// Reads `band` grown by `ov`, repeating the buffer's edge pixels outside it.
fn read_padded(img: &[f32], w: usize, h: usize, b: Band, ov: usize) -> Vec<f32> {
    let (iw, ih) = (b.x1 - b.x0 + 2 * ov, b.y1 - b.y0 + 2 * ov);
    let mut out = vec![0.0f32; iw * ih * CH];
    for row in 0..ih {
        let sy = (b.y0 + row).saturating_sub(ov).min(h.saturating_sub(1));
        for col in 0..iw {
            let sx = (b.x0 + col).saturating_sub(ov).min(w.saturating_sub(1));
            let s = (sy * w + sx) * CH;
            let d = (row * iw + col) * CH;
            if let (Some(dst), Some(src)) = (out.get_mut(d..d + CH), img.get(s..s + CH)) {
                dst.copy_from_slice(src);
            }
        }
    }
    out
}

/// Splits `w × h` into bands whose padded input fits in `budget_px` pixels.
fn plan_bands(w: usize, h: usize, ov: usize, budget_px: usize) -> Result<Vec<Band>> {
    let too_big = || Error::Limit("memory budget (a band does not fit in the filter's memory)".into());
    let tall = 1 + 2 * ov;
    let bw = if (w + 2 * ov).saturating_mul(tall) <= budget_px { w } else { (budget_px / tall).saturating_sub(2 * ov).clamp(1, w) };
    if (bw + 2 * ov).saturating_mul(tall) > budget_px {
        return Err(too_big());
    }
    let bh = (budget_px / (bw + 2 * ov)).saturating_sub(2 * ov).clamp(1, h);
    let mut bands = Vec::new();
    for y0 in (0..h).step_by(bh) {
        for x0 in (0..w).step_by(bw) {
            bands.push(Band { x0, y0, x1: (x0 + bw).min(w), y1: (y0 + bh).min(h) });
        }
    }
    Ok(bands)
}

impl Filter {
    /// Runs the filter on `img`, interleaved linear RGB `f32` (3 channels, no alpha,
    /// `width × height` pixels), in place. `params` are validated against the manifest
    /// ([`crate::Manifest::resolve_params`]); `frame` places the buffer in the full image.
    ///
    /// The buffer is cut into bands of about [`crate::Limits::band_bytes`], each padded by the
    /// manifest's `overlap` (edge pixels repeated at the buffer's border) and run in a fresh
    /// instance: in parallel on native builds, one after another on wasm32. Non-finite output
    /// samples keep their original value. Setting `cancel` stops the run with
    /// [`Error::Cancelled`]; on any error `img` is left unchanged.
    pub fn apply_rgb(&self, img: &mut [f32], width: u32, height: u32, frame: FrameInfo, params: &Value, cancel: &AtomicBool) -> Result<()> {
        let resolved = self.host_params(params)?;
        let (w, h) = (width as usize, height as usize);
        let samples = w.checked_mul(h).and_then(|n| n.checked_mul(CH)).ok_or_else(|| Error::Params("image size overflows".into()))?;
        if img.len() != samples {
            return Err(Error::Params(format!("buffer has {} samples, expected {width}×{height}×3 = {samples}", img.len())));
        }
        if samples == 0 {
            return Ok(());
        }
        let ov = self.manifest().overlap as usize;
        // Half the filter's memory cap is for the band (the module needs room of its own).
        let room = self.limits().max_memory_bytes / 2;
        let budget_px = self.limits().band_bytes.min(room) / (CH * 4);
        let bands = plan_bands(w, h, ov, budget_px)?;
        let deadline = Deadline::new(self.limits(), Some(cancel));
        let base = Value::Object(resolved);
        let src: &[f32] = img;
        let run = |b: &Band| {
            let r = self.run_band(src, w, h, *b, ov, frame, &base, &deadline);
            if let Err(error) = &r {
                deadline.abort(error.clone());
            }
            r
        };
        #[cfg(not(target_arch = "wasm32"))]
        let results: Vec<Result<Vec<f32>>> = {
            use rayon::prelude::*;
            bands.par_iter().map(run).collect()
        };
        #[cfg(target_arch = "wasm32")]
        let results: Vec<Result<Vec<f32>>> = bands.iter().map(run).collect();
        let mut outs = Vec::with_capacity(results.len());
        for r in results {
            outs.push(r?);
        }
        deadline.check()?;
        for (b, data) in bands.iter().zip(outs) {
            let bw = b.x1 - b.x0;
            for (row, y) in (b.y0..b.y1).enumerate() {
                let d = (y * w + b.x0) * CH;
                if let (Some(dst), Some(s)) = (img.get_mut(d..d + bw * CH), data.get(row * bw * CH..(row + 1) * bw * CH)) {
                    dst.copy_from_slice(s);
                }
            }
        }
        Ok(())
    }

    /// `params` resolved against the manifest, plus the host's own keys the manifest can't
    /// declare: `encoding` (`"linear"` / `"srgb"`, how the samples are encoded) and keys starting
    /// with `_` (e.g. `_analysis`; `_image` is set per band).
    fn host_params(&self, params: &Value) -> Result<serde_json::Map<String, Value>> {
        let mut resolved = self.manifest().resolve_params(params)?;
        if let Value::Object(given) = params {
            for (k, v) in given {
                if (k == "encoding" || k.starts_with('_')) && k != "_image" {
                    resolved.insert(k.clone(), v.clone());
                }
            }
        }
        Ok(resolved)
    }

    /// Runs the module's optional `pc_analyze` export once on the whole `img` (interleaved RGB
    /// `f32`, typically a downsampled frame) with `params` as [`Self::apply_rgb`] passes them, and
    /// returns its JSON object (`None`: nothing measured). An error when the module has no
    /// `pc_analyze` ([`Self::has_analyze`]). Budgeted like a filter run.
    pub fn analyze_rgb(&self, img: &[f32], width: u32, height: u32, params: &Value, cancel: &AtomicBool) -> Result<Option<Value>> {
        let mut base = self.host_params(params)?;
        let (w, h) = (width as usize, height as usize);
        let samples = w.checked_mul(h).and_then(|n| n.checked_mul(CH)).ok_or_else(|| Error::Params("image size overflows".into()))?;
        if img.len() != samples || samples == 0 {
            return Err(Error::Params(format!("buffer has {} samples, expected {width}×{height}×3", img.len())));
        }
        base.insert(
            "_image".into(),
            json!({"x": 0, "y": 0, "width": w, "height": h, "overlap": 0, "canvasWidth": w, "canvasHeight": h,
                   "mode": "rgb", "depth": 32, "alpha": false, "channels": CH}),
        );
        let pbytes = serde_json::to_vec(&Value::Object(base)).map_err(|e| Error::Params(e.to_string()))?;
        let deadline = Deadline::new(self.limits(), Some(cancel));
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.analyze_buffer(img, width, height, CH as u32, FORMAT_RGB32, &pbytes, &deadline)
        }));
        run.map_err(|_| Error::Trap("the runtime panicked".into()))?
    }

    #[allow(clippy::too_many_arguments)]
    fn run_band(
        &self,
        img: &[f32],
        w: usize,
        h: usize,
        b: Band,
        ov: usize,
        frame: FrameInfo,
        base: &Value,
        deadline: &Deadline<'_>,
    ) -> Result<Vec<f32>> {
        // Cancellation and earlier failures are checked between bands.
        deadline.check()?;
        let mut data = read_padded(img, w, h, b, ov);
        let orig = data.clone();
        let (iw, ih) = (b.x1 - b.x0 + 2 * ov, b.y1 - b.y0 + 2 * ov);
        let mut params = base.clone();
        if let Value::Object(m) = &mut params {
            let x = i64::from(frame.x) + b.x0 as i64 - ov as i64;
            let y = i64::from(frame.y) + b.y0 as i64 - ov as i64;
            m.insert(
                "_image".into(),
                json!({
                    "x": x, "y": y, "width": iw, "height": ih, "overlap": ov,
                    "canvasWidth": frame.full_w, "canvasHeight": frame.full_h,
                    "mode": "rgb", "depth": 32, "alpha": false, "channels": CH,
                }),
            );
        }
        let pbytes = serde_json::to_vec(&params).map_err(|e| Error::Params(e.to_string()))?;
        let (iw32, ih32) = (u32::try_from(iw), u32::try_from(ih));
        let (Ok(iw32), Ok(ih32)) = (iw32, ih32) else {
            return Err(Error::Limit("band size".into()));
        };
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.filter_buffer(&mut data, iw32, ih32, CH as u32, FORMAT_RGB32, &pbytes, deadline)
        }));
        run.map_err(|_| Error::Trap("the runtime panicked".into()))??;
        // Keep the band (drop the overlap margins); non-finite samples keep the original.
        let bw = b.x1 - b.x0;
        let mut out = Vec::with_capacity(bw * (b.y1 - b.y0) * CH);
        for row in 0..(b.y1 - b.y0) {
            let s = ((row + ov) * iw + ov) * CH;
            for i in s..s + bw * CH {
                let o = orig.get(i).copied().unwrap_or(0.0);
                let v = data.get(i).copied().unwrap_or(o);
                out.push(if v.is_finite() { v } else { o });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_the_image_exactly_once() {
        for (w, h, ov, budget) in [(100, 80, 0, 1000), (7, 300, 3, 200), (1, 1, 0, 10), (5000, 3, 2, 600)] {
            let bands = plan_bands(w, h, ov, budget).unwrap();
            let mut seen = vec![0u8; w * h];
            for b in &bands {
                assert!((b.x1 - b.x0 + 2 * ov) * (b.y1 - b.y0 + 2 * ov) <= budget);
                for y in b.y0..b.y1 {
                    for x in b.x0..b.x1 {
                        seen[y * w + x] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|c| *c == 1));
        }
        assert!(plan_bands(10, 10, 50, 100).is_err());
    }
}
