//! The Source plug-in filter stage at a fixed pixel scale, and the filtered source that whole-image
//! helpers read.
//!
//! A filter that reads neighbouring pixels (manifest `overlap` > 0: an unsharp mask, a grain, a
//! blur) looks different at each pixel scale: run on a 1500 px preview, its 3 × 3 kernel spans a
//! sixth of what it spans on a 9000 px export. So such a stack runs once on the whole frame at the
//! source's own resolution ([`native_size`]); every render (preview, loupe, zoomed window, export at
//! any size) is resampled from that result, and so matches the others. The result is kept in a
//! process-wide slot keyed by the source's pixels, the frame and the stack, so drags of other
//! sliders, zooming and a later export reuse it. A draft that finds no such result runs the stack on
//! a reduced copy of its own pixels (the draft proxy of [`crate::plugin_filters::apply_source`]),
//! which the full render after it replaces.
//!
//! Per-pixel filters (`overlap` 0) look the same at every scale and keep running on the render's
//! own pixels.
//!
//! [`aux_source`] is the source as the Source filters turn it, downsized, for the helpers that
//! analyse the whole photo before a render (Upright, the model-free auto masks, spot-source picking,
//! a zoomed view's dehaze airlight), so they see e.g. the converted negative, not the scan.

use std::sync::{Arc, Mutex, PoisonError};

use lcc_develop::{DevelopSettings, FilterStage};
use lcc_raster::Rgb32f;
use lcc_raster::resample::{Filter, resize};

use crate::plugin_filters::{self, Stack};
use crate::{Plan, SourceInfo, geometry::Frame, hash_of};

/// Most pixels a pixel-scale stack runs on; a larger frame runs it at this size (and its scale is
/// then that of this size, the same for every render of the photo).
pub const MAX_NATIVE_PIXELS: usize = if cfg!(target_arch = "wasm32") { 24_000_000 } else { 100_000_000 };

/// The whole frame's size at the photo's full resolution ([`SourceInfo::native_long`], else the
/// source buffer's), capped at [`MAX_NATIVE_PIXELS`].
pub fn native_size(frame: &Frame, info: &SourceInfo) -> (usize, usize) {
    let (nw, nh) = frame.native_size();
    // a reduced decode (a preview level) is scaled up to the photo's size; a decoder's buffer a few
    // pixels off the catalog's size is not
    let src_long = frame.src_w.max(frame.src_h).max(1) as f64;
    let up = info.native_long.unwrap_or(0) as f64 / src_long;
    let up = if up.is_finite() && up > 1.05 { up } else { 1.0 };
    let (nw, nh) = (nw * up, nh * up);
    let (nw, nh) = (if nw.is_finite() { nw.max(1.0) } else { 1.0 }, if nh.is_finite() { nh.max(1.0) } else { 1.0 });
    let k = (MAX_NATIVE_PIXELS as f64 / (nw * nh)).sqrt().min(1.0);
    frame.fit(((nw * k).round() as usize).max(1), ((nh * k).round() as usize).max(1))
}

struct Native {
    key: u64,
    img: Arc<Rgb32f>,
    warnings: Vec<String>,
}

/// The last native-resolution result (one photo's worth of memory at most).
static NATIVE: Mutex<Option<Native>> = Mutex::new(None);

/// Drops the cached native-resolution result (with [`crate::StageCache::clear`], to free memory).
pub fn clear() {
    *NATIVE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    AUX.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

/// The Source stack of `plan` on the render's pixels: `sampled` (the render's resampled source)
/// filtered, with the key it is cached under (`stack.key`, or `draft_key` for a draft proxy).
pub(crate) fn run(
    src: &Rgb32f,
    info: &SourceInfo,
    plan: &Plan<'_>,
    sampled: &Rgb32f,
    draft: bool,
    draft_key: u64,
    warnings: &mut Vec<String>,
) -> (u64, Arc<Rgb32f>) {
    let stack = &plan.source_filters;
    if stack.pixel_scale() {
        let (nw, nh) = native_size(&plan.whole_frame, info);
        let key = hash_of((stack.key, crate::optics::fingerprint(src), src.width, src.height, format!("{:?}", plan.whole_frame), nw, nh));
        let hit = {
            let g = NATIVE.lock().unwrap_or_else(PoisonError::into_inner);
            g.as_ref().filter(|n| n.key == key).map(|n| (n.img.clone(), n.warnings.clone()))
        };
        let native = match hit {
            Some((img, w)) => {
                warnings.extend(w);
                Some(img)
            }
            // a draft doesn't wait for the full-resolution run: its proxy stands in
            None if draft => None,
            None => {
                // (a whole-frame render at the native size has sampled exactly this already)
                let f = plan.filter_frame;
                let same = (f.x, f.y, f.full_w as usize, f.full_h as usize, sampled.width, sampled.height) == (0, 0, nw, nh, nw, nh);
                let mut img = if same { sampled.clone() } else { plan.whole_frame.sample(src, nw, nh) };
                let mut w = Vec::new();
                plugin_filters::apply_source(stack, &mut img, lcc_filters::FrameInfo::whole(u32_of(nw), u32_of(nh)), false, &mut w);
                let img = Arc::new(img);
                warnings.extend(w.iter().cloned());
                *NATIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Native { key, img: img.clone(), warnings: w });
                Some(img)
            }
        };
        if let Some(n) = native {
            let f = plan.filter_frame;
            let out = crate::timed("plug-in filters: resample native", || {
                to_request(&n, f.full_w as usize, f.full_h as usize, (f.x as usize, f.y as usize), sampled.width, sampled.height)
            });
            return (stack.key, Arc::new(out));
        }
    }
    let mut img = sampled.clone();
    plugin_filters::apply_source(stack, &mut img, plan.filter_frame, draft, warnings);
    (if draft { draft_key } else { stack.key }, Arc::new(img))
}

fn u32_of(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// The `w × h` window at `origin` of `native` resampled to a `full_w × full_h` whole frame.
pub fn to_request(native: &Rgb32f, full_w: usize, full_h: usize, origin: (usize, usize), w: usize, h: usize) -> Rgb32f {
    let (full_w, full_h) = (full_w.max(1), full_h.max(1));
    if full_w <= native.width && full_h <= native.height {
        // minification: area-filtered like the render's own sampling, then cut to the window
        let whole = resize(native, full_w, full_h, Filter::Mitchell);
        if origin == (0, 0) && (w, h) == (full_w, full_h) {
            return whole;
        }
        let mut out = whole.crop(origin.0, origin.1, w, h);
        if (out.width, out.height) != (w.max(1), h.max(1)) {
            out = resize(&out, w, h, Filter::Bilinear);
        }
        return out;
    }
    // magnification (a zoomed window): bilinear from the native pixels, the window only
    let (kx, ky) = (native.width as f32 / full_w as f32, native.height as f32 / full_h as f32);
    let (w, h) = (w.max(1), h.max(1));
    let mut out = Rgb32f::new(w, h);
    crate::for_rows(&mut out.data, w, |y, row| {
        let sy = ((y + origin.1) as f32 + 0.5) * ky;
        for (x, px) in row.iter_mut().enumerate() {
            *px = native.sample_bilinear(((x + origin.0) as f32 + 0.5) * kx, sy);
        }
    });
    out
}

/// Long edge of [`aux_source`].
pub const AUX_EDGE: usize = 2048;

/// Filtered sources by source pixels and stack.
static AUX: Mutex<Vec<(u64, Arc<Rgb32f>)>> = Mutex::new(Vec::new());

/// `src` as the Source filters of `s` turn it (whole, at most [`AUX_EDGE`] px), for the helpers
/// that analyse the photo itself; `src` unchanged when no Source filter is active. Cached by the
/// source's pixels and the stack, so the helpers' own caches (keyed by the pixels they get) follow
/// a change of the filters. (A pixel-scale filter runs at this size here, not at the native one: the
/// helpers look at tones and shapes, not at pixel detail.)
pub fn aux_source(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<Arc<Rgb32f>> {
    s.active_plugin_filters(FilterStage::Source).next()?;
    let stack = Stack::resolve(info, s, FilterStage::Source);
    if stack.is_empty() {
        return None;
    }
    let key = hash_of((stack.key, crate::optics::fingerprint(src), src.width, src.height));
    if let Some((_, a)) = AUX.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key) {
        return Some(a.clone());
    }
    let img = Arc::new(plugin_filters::filtered_source(src, info, s, AUX_EDGE).into_owned());
    let mut c = AUX.lock().unwrap_or_else(PoisonError::into_inner);
    if c.len() >= 4 {
        c.remove(0);
    }
    c.push((key, img.clone()));
    Some(img)
}
