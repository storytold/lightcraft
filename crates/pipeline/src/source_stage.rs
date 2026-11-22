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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

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
    /// The photo as framed, without the stack ([`native_base`]).
    base: u64,
    /// The stack's key: a later stack that starts with these filters starts from this result.
    stack: u64,
    /// The image before the stack's last filter (the filters' encoding), with its key and the
    /// warnings so far: a change to the last filter's parameters runs that filter only.
    before_last: Option<(u64, Arc<Rgb32f>, Vec<String>)>,
    /// Long edge of the source it was made from: a render takes it only when its own source is no
    /// larger (a result upscaled from the grid's 512-px decode would blur the loupe's).
    src_long: usize,
    img: Arc<Rgb32f>,
    warnings: Vec<String>,
}

/// Most pixels a native result also keeps the image before its last filter for (a second copy).
const KEEP_BEFORE_LAST_PIXELS: usize = 40_000_000;

/// The last native-resolution result (one photo's worth of memory at most).
static NATIVE: Mutex<Option<Native>> = Mutex::new(None);

/// A native-resolution run under way: a render that needs it waits for it instead of running the
/// stack again beside it (the loupe, the grid's cell and an export of one edit would each run it,
/// all at once, each slowed by the others).
struct Running {
    key: u64,
    /// Long edge of the source the run is made from (it serves renders of no larger sources).
    src_long: usize,
    /// Set when a run of the same key from a larger source starts: this one gives way to it.
    give_way: Arc<AtomicBool>,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());
static RAN: Condvar = Condvar::new();

/// The sources (by [`crate::optics::fingerprint`]) native runs were made of and the filter each
/// started at, for tests to count.
#[cfg(test)]
pub(crate) static NATIVE_RUNS: Mutex<Vec<(u64, usize)>> = Mutex::new(Vec::new());

/// Takes `key`'s run for a source of long edge `src_long`: `None` while another render runs it from
/// a source at least as large (after waiting for that to end, the caller looks in [`NATIVE`] again).
/// Smaller runs of the key are told to give way.
fn claim(key: u64, src_long: usize) -> Option<Claim> {
    let serves = |g: &Vec<Running>| g.iter().any(|r| r.key == key && r.src_long >= src_long);
    let mut g = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if serves(&g) {
        // (a render on one of rayon's own threads runs it itself: blocked there, it could hold up
        // the very run it waits for)
        if rayon::current_thread_index().is_some() {
            return Some(Claim { run: None, give_way: Arc::new(AtomicBool::new(false)) });
        }
        while serves(&g) {
            g = RAN.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
        return None;
    }
    for r in g.iter().filter(|r| r.key == key) {
        r.give_way.store(true, Ordering::Relaxed);
    }
    let give_way = Arc::new(AtomicBool::new(false));
    g.push(Running { key, src_long, give_way: give_way.clone() });
    Some(Claim { run: Some((key, src_long)), give_way })
}

/// A native run under way; ends (and wakes the renders waiting for it) when dropped, also when the
/// run fails. (`run: None`: a second run of a key another render holds, which it doesn't end.)
struct Claim {
    run: Option<(u64, usize)>,
    give_way: Arc<AtomicBool>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some((key, src_long)) = self.run {
            let mut g = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(i) = g.iter().position(|r| r.key == key && r.src_long == src_long && Arc::ptr_eq(&r.give_way, &self.give_way)) {
                g.remove(i);
            }
            drop(g);
            RAN.notify_all();
        }
    }
}

fn native_hit(key: u64, src_long: usize) -> Option<(Arc<Rgb32f>, Vec<String>)> {
    let g = NATIVE.lock().unwrap_or_else(PoisonError::into_inner);
    g.as_ref().filter(|n| n.key == key && n.src_long >= src_long).map(|n| (n.img.clone(), n.warnings.clone()))
}

/// What a native result is of, without the stack: the photo as `plan` frames it. With the source's
/// [`SourceInfo::identity`] this is the same at every size the photo is decoded at (the framing by
/// its orientation, normalized crop, flips and warp), so the grid's cell, the loupe's preview-size
/// render and an export share one run; without one, the source's pixels and frame.
fn native_base(src: &Rgb32f, info: &SourceInfo, plan: &Plan<'_>, nw: usize, nh: usize) -> u64 {
    let f = &plan.whole_frame;
    match info.identity {
        // (not the native size worked out from the decode: decodes whose sizes round differently
        // give sizes a pixel apart; any native result is resampled to the request anyway)
        Some(id) => hash_of((id.key, id.size, format!("{:?} {:?} {} {} {:?} {}", f.orient, f.crop, f.flip_h, f.flip_v, f.warp, f.masks_on_photo))),
        None => hash_of((crate::optics::fingerprint(src), src.width, src.height, format!("{f:?}"), nw, nh)),
    }
}

/// Where a native run of `stack` can start for a source of long edge `src_long`: the furthest
/// filter an earlier run of the photo (`base`) got to along it, the image there (the filters'
/// encoding) and the warnings so far. Adding a filter at the end runs the new filter only, and so
/// does a change to the last filter's parameters.
fn resume_point(stack: &Stack, base: u64, src_long: usize) -> Option<(usize, Rgb32f, Vec<String>)> {
    let n = stack.len();
    let at = |key: u64| (1..n).rev().find(|k| stack.prefix_key(*k) == Some(key));
    let (k, img, w, encoded) = {
        let g = NATIVE.lock().unwrap_or_else(PoisonError::into_inner);
        let p = g.as_ref().filter(|p| p.base == base && p.src_long >= src_long)?;
        let whole = at(p.stack).map(|k| (k, p.img.clone(), p.warnings.clone(), false));
        let before = p.before_last.as_ref().and_then(|(key, img, w)| at(*key).map(|k| (k, img.clone(), w.clone(), true)));
        match (whole, before) {
            (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
            (a, b) => a.or(b),
        }?
    };
    let mut img = Arc::unwrap_or_clone(img);
    if !encoded {
        plugin_filters::to_filter_space(&mut img);
    }
    Some((k, img, w))
}

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
        let base = native_base(src, info, plan, nw, nh);
        let key = hash_of((base, stack.key));
        let src_long = src.width.max(src.height);
        if let Some(n) = native(src, info, plan, sampled, (nw, nh), (base, key), src_long, draft, warnings) {
            let f = &plan.filter_frame;
            let out = crate::timed("plug-in filters: resample native", || {
                to_request(&n, f.full_w as usize, f.full_h as usize, (f.x as usize, f.y as usize), sampled.width, sampled.height)
            });
            return (stack.key, Arc::new(out));
        }
    }
    let mut img = sampled.clone();
    plugin_filters::apply_source(stack, &mut img, plan.filter_frame.clone(), draft, warnings);
    (if draft { draft_key } else { stack.key }, Arc::new(img))
}

/// The native-resolution result `key` for a source of long edge `src_long`: kept, made by another
/// render (waited for), or made here; `None` for a draft that finds none (its proxy stands in).
#[allow(clippy::too_many_arguments)]
fn native(
    src: &Rgb32f,
    info: &SourceInfo,
    plan: &Plan<'_>,
    sampled: &Rgb32f,
    (nw, nh): (usize, usize),
    (base, key): (u64, u64),
    src_long: usize,
    draft: bool,
    warnings: &mut Vec<String>,
) -> Option<Arc<Rgb32f>> {
    loop {
        if let Some((img, w)) = native_hit(key, src_long) {
            warnings.extend(w);
            return Some(img);
        }
        // a draft doesn't wait for the full-resolution run
        if draft {
            return None;
        }
        // another render running this very stack from a source as large: its result, once it
        // lands (and when that run failed or gave way, this one takes over)
        let Some(running) = claim(key, src_long) else { continue };
        // (it may have landed between the look and the claim)
        if let Some((img, w)) = native_hit(key, src_long) {
            warnings.extend(w);
            return Some(img);
        }
        let stack = &plan.source_filters;
        let (mut img, start, mut w) = match resume_point(stack, base, src_long) {
            Some((k, img, w)) => (img, k, w),
            None => {
                // (a whole-frame render at the native size has sampled exactly this already)
                let f = &plan.filter_frame;
                let same = (f.x, f.y, f.full_w as usize, f.full_h as usize, sampled.width, sampled.height) == (0, 0, nw, nh, nw, nh);
                let mut img = if same { sampled.clone() } else { plan.whole_frame.sample(src, nw, nh) };
                if !same {
                    // (`sampled` had its film dust filled in by the render; a fresh sample needs it too)
                    crate::film_dust::stage(&mut img, src, info, &plan.settings, &plan.whole_frame);
                }
                plugin_filters::to_filter_space(&mut img);
                (img, 0, Vec::new())
            }
        };
        #[cfg(test)]
        NATIVE_RUNS.lock().unwrap_or_else(PoisonError::into_inner).push((crate::optics::fingerprint(src), start));
        // (a resumed run's size is the earlier run's: a decode a pixel apart works one out a pixel off)
        let mut frame = lcc_filters::FrameInfo::whole(u32_of(img.width), u32_of(img.height));
        frame.source = plan.filter_frame.source.clone();
        let last = stack.len().saturating_sub(1);
        let keep_at = (stack.len() > 1 && img.width.saturating_mul(img.height) <= KEEP_BEFORE_LAST_PIXELS && start <= last).then_some(last);
        let ran = plugin_filters::apply_native(stack, &mut img, frame, &mut w, plugin_filters::Run { give_way: &running.give_way, start, keep_at });
        if ran.gave_way {
            // a render of a larger source of the photo started the same run: its result, once it
            // lands, serves this one too
            drop(running);
            continue;
        }
        plugin_filters::from_filter_space(&mut img);
        let img = Arc::new(img);
        warnings.extend(w.iter().cloned());
        // (stopped short at the render's cancel: not the stack's result, not kept)
        if !crate::was_cut() {
            let before_last =
                ran.kept.zip(stack.prefix_key(last)).map(|((data, kw), k)| (k, Arc::new(Rgb32f { width: img.width, height: img.height, data }), kw));
            *NATIVE.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Native { key, base, stack: stack.key, before_last, src_long, img: img.clone(), warnings: w });
        }
        drop(running);
        return Some(img);
    }
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
    if crate::was_cut() {
        return Some(img);
    }
    let mut c = AUX.lock().unwrap_or_else(PoisonError::into_inner);
    if c.len() >= 4 {
        c.remove(0);
    }
    c.push((key, img.clone()));
    Some(img)
}
