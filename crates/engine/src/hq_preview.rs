//! High Quality Preview (the UI command `view.highQualityPreview`): the loupe shows the photo's
//! full-resolution render, scaled down for display, instead of a render made at display size.
//!
//! The pipeline resamples the source to the output size first and develops after that, so a view
//! below 100 % is a develop of averaged pixels. For most photos that looks like the full-size
//! render made smaller; for sparse, near-binary detail (star fields, line art, fractal renders)
//! under a strong non-monotonic curve or a big exposure it does not: averaging and then applying a
//! curve is a different picture from applying the curve and then averaging. A full-size export
//! (and the 1:1 view) can then be black where the fit view shows texture.
//!
//! [`Session::high_quality`](crate::Session::high_quality) turns a loupe job (whole frame or
//! window) into one that renders the photo's native-resolution frame — the frame a full-size
//! export writes, from the same source level, crop and geometry — and then area-averages it, in
//! linear light, to the pixels the job was asked for ([`HqDownscale`]). CPU and GPU renders both go
//! through it; how settings render is unchanged (no process version is involved).

use serde::{Deserialize, Serialize};

use lightcraft_pipeline::{OutputDepth, PixelWindow, Quality};
use lightcraft_preview::Hasher128;
use lightcraft_raster::Rgba8;

use crate::media::{RenderJob, SourceLevel};

/// Native frames above this many pixels are not rendered whole for a High Quality Preview when
/// the computer's memory is unknown (on the web, a wasm instance's memory is much smaller).
const MAX_PIXELS_UNKNOWN_RAM: usize = if cfg!(target_arch = "wasm32") { 24_000_000 } else { 160_000_000 };

/// How a High Quality Preview job turns its native-resolution render into the pixels the view
/// asked for: the rectangle `src` (`[x, y, w, h]` in the native render's pixels, fractional;
/// `None`: all of it) area-averaged to `out` (w, h) in linear light.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HqDownscale {
    pub src: Option<[f64; 4]>,
    pub out: (usize, usize),
}

impl HqDownscale {
    /// `image` (8-bit, sRGB transfer curve: a preview, or a render into a display's primaries
    /// before the display's own correction) scaled to [`Self::out`]: each pixel is the exact
    /// area-weighted mean of the light it covers, encoded back to 8 bits. Alpha is averaged as is.
    pub fn apply(&self, image: &Rgba8) -> Rgba8 {
        use lightcraft_color::transfer::{decode_srgb8, encode_srgb8};
        let rect = self.src.unwrap_or([0.0, 0.0, image.width as f64, image.height as f64]);
        let lin = |p: [u8; 4]| [decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2]), f32::from(p[3]) / 255.0];
        let avg = lightcraft_raster::resample::area(image, rect, self.out.0, self.out.1, lin);
        let data = avg
            .data
            .iter()
            .map(|p| [encode_srgb8(p[0]), encode_srgb8(p[1]), encode_srgb8(p[2]), (p[3] * 255.0).round().clamp(0.0, 255.0) as u8])
            .collect();
        Rgba8 { width: avg.width, height: avg.height, data }
    }
}

/// What [`Session::high_quality`](crate::Session::high_quality) did with a job (reported by
/// `ui.inspect` → `highQualityPreview`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum HqState {
    /// The job renders the native frame (`native`: its size) and scales it to the view.
    #[serde(rename_all = "camelCase")]
    Applied { native: (usize, usize) },
    /// The view is at 100 % or more: the normal render already is the native one.
    Native,
    /// The normal render is used, for `reason`.
    Skipped { reason: String },
    /// A window whose native-resolution render would read more than one render can hold (spots,
    /// Auto Mask strokes): the caller must not render it (the loupe keeps its whole-frame render).
    Refused { reason: String },
}

impl HqState {
    fn skipped(reason: impl Into<String>) -> HqState {
        HqState::Skipped { reason: reason.into() }
    }
}

/// The native window covering `win` of a `full_w × full_h` frame whose native size is
/// `native_w × native_h`, and the exact rectangle of it `win` corresponds to (in the native
/// window's pixels): the preimage rounded outwards to whole pixels and clamped into the frame.
pub fn native_window(full: (usize, usize), native: (usize, usize), win: PixelWindow) -> (PixelWindow, [f64; 4]) {
    let (fw, fh) = (full.0.max(1), full.1.max(1));
    let (nw, nh) = (native.0.max(1), native.1.max(1));
    let win = win.clamped(fw, fh);
    let (sx, sy) = (nw as f64 / fw as f64, nh as f64 / fh as f64);
    let axis = |at: usize, len: usize, s: f64, n: usize| {
        let (a, b) = (at as f64 * s, (at + len) as f64 * s);
        let lo = (a.floor().max(0.0) as usize).min(n - 1);
        let hi = (b.ceil() as usize).clamp(lo + 1, n);
        (lo, hi, a - lo as f64, b - a)
    };
    let (x0, x1, rx, rw) = axis(win.x, win.w, sx, nw);
    let (y0, y1, ry, rh) = axis(win.y, win.h, sy, nh);
    (PixelWindow { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, [rx, ry, rw, rh])
}

/// The size a `full`-sized render ends up at under the display fit `fit` (see
/// [`crate::RenderJob::display_fit`]): shrunk to fit it, never enlarged.
fn display_fit_size(full: (usize, usize), fit: (usize, usize)) -> (usize, usize) {
    let (w, h) = (full.0.max(1), full.1.max(1));
    let (fw, fh) = (fit.0.max(1), fit.1.max(1));
    if w <= fw && h <= fh {
        return (w, h);
    }
    let s = (fw as f64 / w as f64).min(fh as f64 / h as f64).min(1.0);
    (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1))
}

impl crate::Session {
    /// The High Quality Preview version of a loupe job made by [`Self::loupe_job`],
    /// [`Self::render_job`] or [`Self::region_job`] (and its Before forms): it renders the photo's
    /// native-resolution frame (or the native window covering the asked-for window) and scales it
    /// to the asked-for pixels ([`HqDownscale`]); for a whole-frame job with a display fit
    /// ([`RenderJob::with_display_fit`]) that is the drawn size, with no Lanczos pass after. Its
    /// result key is its own; it is never kept as the photo's view preview or thumbnail; its
    /// histogram is the native render's.
    ///
    /// Returns the job unchanged when it doesn't apply: a draft (slider drag), a thumbnail or
    /// slide job, an HDR (non-8-bit) request, a view at 100 % or more ([`HqState::Native`]), or a
    /// native frame too big to render in one go ([`HqState::Skipped`], logged).
    pub fn high_quality(&mut self, job: RenderJob) -> (RenderJob, HqState) {
        match self.hq_plan(&job) {
            Ok((native, window, hq, level)) => {
                let Some(p) = self.catalog.photo(job.photo).cloned() else { return (job, HqState::skipped("no such photo")) };
                let mut job = job;
                job.source = self.media.source_ref(&p, level);
                job.level = level;
                job.request.max_w = native.0;
                job.request.max_h = native.1;
                job.request.window = window;
                let (w, h) = (hq.out.0 as u64, hq.out.1 as u64);
                let src = hq.src.map_or([0; 4], |r| r.map(f64::to_bits));
                let nwin = window.map_or([0; 4], |w| [w.x, w.y, w.w, w.h].map(|v| v as u64));
                let mut k = Hasher128::new();
                k.u64(job.key).str("high quality").u64(native.0 as u64).u64(native.1 as u64).u64(level as u64).u64(w).u64(h);
                for v in src.into_iter().chain(nwin) {
                    k.u64(v);
                }
                job.key = k.finish().0 as u64;
                job.hq = Some(hq);
                // (folded into `hq.out` by `hq_plan`)
                job.display_fit = None;
                job.cache = None;
                job.view_cache = None;
                job.preview_cut = None;
                (job, HqState::Applied { native })
            }
            Err(state) => {
                if let HqState::Skipped { reason } = &state {
                    log::info!("High Quality Preview not applied to photo {}: {reason}", job.photo.0);
                }
                (job, state)
            }
        }
    }

    /// What [`Self::high_quality`] renders for `job`: the native frame size, the native window (a
    /// window job), the downscale and the source level; or why it doesn't.
    #[allow(clippy::type_complexity)]
    fn hq_plan(&self, job: &RenderJob) -> Result<((usize, usize), Option<PixelWindow>, HqDownscale, SourceLevel), HqState> {
        if job.hq.is_some() {
            return Err(HqState::skipped("already a High Quality Preview job"));
        }
        if job.request.quality == Quality::Draft {
            return Err(HqState::skipped("a draft render (slider drag)"));
        }
        if job.request.depth != OutputDepth::U8 {
            return Err(HqState::skipped("HDR output (not 8-bit)"));
        }
        if job.cache.is_some() || job.slide.is_some() {
            return Err(HqState::skipped("not a loupe render"));
        }
        let p = self.catalog.photo(job.photo).ok_or_else(|| HqState::skipped("no such photo"))?;
        let apply_crop = job.request.apply_crop;
        let frame = lightcraft_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &job.settings,
            apply_crop,
            p.embedded_lens.as_ref(),
        );
        let (nw, nh) = frame.native_size();
        // whole pixels, as a full-size export rounds them
        let round = |v: f64| if v.is_finite() { v.round().clamp(1.0, (u32::MAX >> 1) as f64) as usize } else { 1 };
        let native = (round(nw), round(nh));
        // the frame the job draws (a window job: the zoomed frame it is a window of)
        let full = frame.fit(job.request.max_w, job.request.max_h);
        // a whole-frame job with a display fit is drawn at that size: the native render is
        // averaged straight to it (as [`crate::media::fit_for_display`] would size it), with no
        // Lanczos pass over the averaged pixels after
        let full = match (job.request.window, job.display_fit) {
            (None, Some(fit)) => display_fit_size(full, fit),
            _ => full,
        };
        if full.0.max(full.1) >= native.0.max(native.1) {
            return Err(HqState::Native);
        }
        let native_px = native.0.saturating_mul(native.1);
        let (full_px, long) = ((p.width as usize).saturating_mul(p.height as usize), p.width.max(p.height) as usize);
        let level = SourceLevel::for_size(crate::media::source_edge_needed(p, &job.settings, apply_crop, native.0.max(native.1)));
        let (window, hq) = match job.request.window {
            None => (None, HqDownscale { src: None, out: full }),
            Some(win) => {
                let win = win.clamped(full.0, full.1);
                let (nwin, rect) = native_window(full, native, win);
                // what the native window's spots and Auto Mask strokes read must fit in one render
                let ppl = frame.px_per_long(native.0);
                if lightcraft_pipeline::spots::window_for_reads_checked(&job.settings, &frame, native.0, native.1, ppl, nwin).is_none() {
                    return Err(HqState::Refused { reason: "the window's spots or Auto Mask strokes read too far at full resolution".into() });
                }
                (Some(nwin), HqDownscale { src: Some(rect), out: (win.w, win.h) })
            }
        };
        let out_px = window.map_or(native_px, |w| w.w.saturating_mul(w.h));
        let weight = crate::export::render_weight(full_px, long, level, out_px);
        let too_big = match crate::memory::total_ram() {
            Some(ram) => weight > ram / 2,
            None => native_px > MAX_PIXELS_UNKNOWN_RAM,
        };
        if too_big {
            return Err(HqState::skipped(format!(
                "the full-resolution frame ({} × {}) is too big to render in one go (about {} MB)",
                native.0,
                native.1,
                weight >> 20
            )));
        }
        Ok((native, window, hq, level))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_window_covers_the_preimage() {
        // 37 %: a 1000 × 750 frame of a 2700 × 2025 photo (scale 2.7)
        let (nwin, rect) = native_window((1000, 750), (2700, 2025), PixelWindow { x: 101, y: 33, w: 256, h: 200 });
        assert_eq!(nwin, PixelWindow { x: 272, y: 89, w: 692, h: 541 });
        assert!((rect[0] - (101.0 * 2.7 - 272.0)).abs() < 1e-9 && (rect[2] - 256.0 * 2.7).abs() < 1e-9, "{rect:?}");
        assert!(rect[0] + rect[2] <= nwin.w as f64 + 1e-9 && rect[1] + rect[3] <= nwin.h as f64 + 1e-9);
        // out of range and degenerate windows are clamped, never panic
        for win in
            [PixelWindow { x: 5000, y: 5000, w: 10, h: 10 }, PixelWindow { x: 0, y: 0, w: 0, h: 0 }, PixelWindow { x: 0, y: 0, w: usize::MAX, h: 1 }]
        {
            let (n, r) = native_window((1000, 750), (2700, 2025), win);
            assert!(n.x + n.w <= 2700 && n.y + n.h <= 2025 && n.w >= 1 && n.h >= 1, "{win:?} → {n:?}");
            assert!(r.iter().all(|v| v.is_finite()));
        }
        let (n, _) = native_window((0, 0), (0, 0), PixelWindow { x: 3, y: 3, w: 3, h: 3 });
        assert_eq!(n, PixelWindow { x: 0, y: 0, w: 1, h: 1 });
    }

    #[test]
    fn downscale_averages_light() {
        // a checkerboard of black and white is half the light: code 188, not 128
        let img = Rgba8::from_fn(64, 48, |x, y| if (x + y) % 2 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
        let r = HqDownscale { src: None, out: (16, 12) }.apply(&img);
        assert_eq!((r.width, r.height), (16, 12));
        assert!(r.data.iter().all(|p| p[0].abs_diff(188) <= 1 && p[3] == 255), "{:?}", r.data[0]);
    }
}
