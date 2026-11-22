//! Auto Sync carries the active photo's VALUE to every selected photo. Where white balance is a
//! mode rather than a number, the mode travels and each photo applies it to itself: As Shot gives
//! each photo its own camera white, Auto each photo's own white from its pixels, a light preset
//! its numbers.
//!
//! A typed or dragged Temp or Tint is set on each photo's own scale: a raw edited in Kelvin gets
//! the same Kelvin (an old relative-scale value converted first, never read as Kelvin), and only
//! the slider that moved changes (a Temp move keeps each photo's own tint). A photo on another
//! scale than the active one (a JPEG next to a raw) has no "same" number and moves by as much as
//! the active photo instead.

use std::collections::HashMap;

use lcc_catalog::PhotoId;
use lcc_develop::{Crop, DevelopSettings, WbMode};
use lcc_geom::{CropGeometry, MIN_CROP_FRACTION, Point, Rect, crop_fit_angle};
use lcc_pipeline::SourceInfo;

use crate::Session;

/// White balance on this source is edited in Kelvin (a raw with a measured camera scale), not on
/// the relative scale (6500 K / 0 = as shot).
fn kelvin(info: &SourceInfo) -> bool {
    info.raw && !info.relative_wb
}

/// The temperature and tint `mode` stands for on a source with `info` (As Shot and the light
/// presets; `None` for Auto and Custom, which need the pixels or the numbers).
fn wb_of_mode(info: &SourceInfo, mode: WbMode) -> Option<(f64, f64)> {
    match mode {
        WbMode::AsShot => Some((info.as_shot_temp, info.as_shot_tint)),
        m => m.preset().map(|(t, ti)| {
            // presets are Kelvin on raws with that scale, relative to daylight (6500 K = as shot) elsewhere
            if kelvin(info) { (t, ti) } else { (6500.0 * t / 5500.0, ti) }
        }),
    }
}

/// `synced.wb` for a photo (source `oinfo`, settings `own` before the sync) when the active
/// photo's white balance went from `old` to `new` (source `ainfo`). `auto`: the photo's own Auto
/// white balance, when `new` is Auto (`None`: it couldn't be worked out, and the photo keeps its
/// white balance).
pub(crate) fn synced_wb(
    ainfo: &SourceInfo,
    old: &DevelopSettings,
    new: &DevelopSettings,
    oinfo: &SourceInfo,
    own: &DevelopSettings,
    auto: Option<(f64, f64)>,
    synced: &mut DevelopSettings,
) {
    use lcc_pipeline::local::{effective_wb, normalize_wb};
    let mut d = own.clone();
    normalize_wb(&mut d, oinfo);
    // A light preset with numbers of its own (`develop.wb {mode, tint}` on a photo on the
    // relative scale, where the stored numbers are what renders): those numbers travel, as a
    // Custom white balance does
    let overridden = new.wb.mode.preset().is_some()
        && wb_of_mode(ainfo, new.wb.mode).is_some_and(|(t, ti)| {
            let (at, ati) = effective_wb(ainfo, new);
            (at - t).abs() > 1e-6 || (ati - ti).abs() > 1e-6
        });
    let mode = if overridden { WbMode::Custom } else { new.wb.mode };
    let resolved = match mode {
        WbMode::Auto => auto,
        WbMode::Custom => None,
        m => wb_of_mode(oinfo, m),
    };
    match (mode, resolved) {
        (WbMode::Custom, _) => {
            let (ot, oti) = effective_wb(oinfo, own);
            let (at0, ati0) = effective_wb(ainfo, old);
            let (at, ati) = effective_wb(ainfo, new);
            let same_scale = kelvin(ainfo) == kelvin(oinfo);
            // only what moved: a Temp move leaves each photo's own tint
            let pick = |own: f64, was: f64, now: f64| {
                if (now - was).abs() < 1e-9 {
                    own
                } else if same_scale {
                    now
                } else {
                    own + (now - was)
                }
            };
            d.wb.temp = pick(ot, at0, at);
            d.wb.tint = pick(oti, ati0, ati);
            d.wb.mode = WbMode::Custom;
        }
        (m, Some((t, ti))) => {
            d.wb.temp = t;
            d.wb.tint = ti;
            d.wb.mode = m;
        }
        // Auto couldn't be worked out on this photo (its pixels didn't load): it keeps its own
        (_, None) => {
            synced.wb = own.wb;
            return;
        }
    }
    let (t, ti) = (d.wb.temp, d.wb.tint);
    lcc_develop::controls::set(&mut d, "wb.temp", t);
    lcc_develop::controls::set(&mut d, "wb.tint", ti);
    synced.wb = d.wb;
}

impl Session {
    /// Each other selected photo's own Auto white balance, worked out on its own pixels, when
    /// Auto Sync is about to carry the active photo's (`id`) switch to Auto (`new`) over. Empty
    /// otherwise. A photo whose pixels don't load is left out (it keeps its white balance).
    pub(crate) fn own_auto_wb(&mut self, id: PhotoId, new: &DevelopSettings, label: &str) -> HashMap<PhotoId, (f64, f64)> {
        let mut own = HashMap::new();
        if !self.auto_sync_applies(id, label) || new.wb.mode != WbMode::Auto || self.develop_of(id).is_some_and(|old| old.wb == new.wb) {
            return own;
        }
        let others: Vec<PhotoId> = self.selection.ids.iter().copied().filter(|x| *x != id).collect();
        // decoded in parallel, a core's worth at a time: a big selection never holds more
        // decoded thumbnails at once than that, whatever the cache keeps
        let batch = rayon::current_num_threads().clamp(1, 16);
        for chunk in others.chunks(batch) {
            for (x, src) in self.sources_now(chunk, crate::media::SourceLevel::Thumb) {
                match src {
                    // with the facts its own decoder gave
                    Ok((src, info)) => {
                        own.insert(x, lcc_pipeline::auto::auto_wb(&src, &info));
                    }
                    Err(e) => log::warn!("Auto Sync: Auto white balance on photo {}: {e}", x.0),
                }
            }
        }
        own
    }
}

/// The crop a photo with frame `odims` (oriented width, height) gets when the active photo (frame
/// `adims`) has crop `new`: the same aspect lock, flips and angle; on a frame of the same shape the
/// same rectangle. On another shape (test notes 2026-10-25 note 15) the crop keeps its tightness and
/// position: its shape is turned to the photo's own orientation (a landscape never gets a
/// portrait-shaped crop, nor a vertical a landscape one), its size is the same fraction of the
/// largest crop of that shape at that angle as on the active photo, and its centre keeps its
/// normalized place in the frame, moved towards the middle only as far as it must to stay inside
/// the rotated photo. An uncropped photo gives each photo its own whole frame at that angle, and an
/// "Original" lock takes the photo's own shape. Sync Settings, Auto Sync, Paste and Paste from
/// Previous all refit a crop here.
pub(crate) fn synced_crop(new: &Crop, adims: (f64, f64), odims: (f64, f64)) -> Crop {
    let (aw, ah) = (adims.0.max(1.0), adims.1.max(1.0));
    let (ow, oh) = (odims.0.max(1.0), odims.1.max(1.0));
    let (ar, or) = (aw / ah, ow / oh);
    if (ar - or).abs() <= 0.005 * ar {
        return *new;
    }
    let wide = or > 1.0;
    // the shape turned to the photo's own orientation (a square stays a square)
    let orient = |s: f64| if s != 1.0 && (s > 1.0) != wide { 1.0 / s } else { s };
    // an "Original" lock takes this photo's own shape (and becomes its Original lock, issue #3056)
    let original = new.aspect.is_some_and(|a| a == Crop::original_aspect(aw, ah));
    let aspect =
        if original { Some(Crop::original_aspect(ow, oh)) } else { new.aspect.map(|(x, y)| if x != y && (x > y) != wide { (y, x) } else { (x, y) }) };
    let angle = new.geometry.angle;
    let full = crop_fit_angle(aw, ah, angle, None).rect;
    let r = new.geometry.rect;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
    let uncropped = new.aspect.is_none() && close(r.x0, full.x0) && close(r.y0, full.y0) && close(r.x1, full.x1) && close(r.y1, full.y1);
    let px = new.geometry.rect_px(aw, ah);
    let own = new.aspect.map(|(x, y)| f64::from(x) / f64::from(y.max(1))).or(Some(px.width() / px.height()));
    let own = own.filter(|s| s.is_finite() && *s > 0.0);
    let (Some(own), false) = (own, uncropped) else {
        return Crop { geometry: crop_fit_angle(ow, oh, angle, None), aspect, flip_h: new.flip_h, flip_v: new.flip_v };
    };
    // tightness: the crop's size against the largest crop of its own shape on the active photo
    let largest = crop_fit_angle(aw, ah, angle, Some(own)).rect_px(aw, ah);
    let frac = (px.width() / largest.width()).clamp(MIN_CROP_FRACTION, 1.0);
    let frac = if frac.is_finite() { frac } else { 1.0 };
    let fit = crop_fit_angle(ow, oh, angle, Some(if original { ow / oh } else { orient(own) }));
    let f = fit.rect_px(ow, oh);
    let (cw, ch) = (f.width() * frac, f.height() * frac);
    let at = |c: Point| CropGeometry { rect: Rect::from_center(c, cw, ch).scale(1.0 / ow, 1.0 / oh), angle };
    let mid = f.center();
    let want = px.center();
    let want = Point::new(want.x / aw * ow, want.y / ah * oh);
    let want = if want.x.is_finite() && want.y.is_finite() { want } else { mid };
    // the centres where a crop of this size fits form a convex set holding the middle: the first
    // point on the way from `want` to the middle that fits
    let geometry = if at(want).is_within_image(ow, oh) {
        at(want)
    } else {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let t = (lo + hi) / 2.0;
            if at(want.lerp(mid, t)).is_within_image(ow, oh) {
                hi = t;
            } else {
                lo = t;
            }
        }
        at(want.lerp(mid, hi))
    };
    Crop { geometry, aspect, flip_h: new.flip_h, flip_v: new.flip_v }
}

/// The last History batch id handed out (process-wide, so two sessions never share one).
static LAST_BATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A new History batch id: milliseconds since 1970, above every id handed out before (unique across
/// sessions and restarts, as long as the clock doesn't run back further than the ids went).
fn next_batch_id() -> u64 {
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0);
    use std::sync::atomic::Ordering::SeqCst;
    let mut last = LAST_BATCH.load(SeqCst);
    loop {
        let id = now.max(last.saturating_add(1));
        match LAST_BATCH.compare_exchange_weak(last, id, SeqCst, SeqCst) {
            Ok(_) => return id,
            Err(seen) => last = seen,
        }
    }
}

/// Photos whose History `op` pushes a step on that has no batch yet (batches nested at most 16 deep).
fn unbatched_steps(op: &lcc_catalog::Op, out: &mut std::collections::HashSet<PhotoId>, depth: usize) {
    match op {
        lcc_catalog::Op::PushHistory { id, step } if step.batch.is_none() => {
            out.insert(*id);
        }
        lcc_catalog::Op::Batch { ops } if depth < 16 => ops.iter().for_each(|o| unbatched_steps(o, out, depth + 1)),
        _ => {}
    }
}

fn set_batch(op: &mut lcc_catalog::Op, batch: u64, depth: usize) {
    match op {
        lcc_catalog::Op::PushHistory { step, .. } if step.batch.is_none() => step.batch = Some(batch),
        lcc_catalog::Op::Batch { ops } if depth < 16 => ops.iter_mut().for_each(|o| set_batch(o, batch, depth + 1)),
        _ => {}
    }
}

impl Session {
    /// One edit that adds History steps on several photos (Auto Sync, Sync Settings, a preset or
    /// Quick Develop on a selection, a multi-photo drag): the same batch id on each of its steps
    /// (groundwork for note 22, the whole selection back to a step).
    pub(crate) fn mark_batch(&mut self, op: &mut lcc_catalog::Op) {
        let mut photos = std::collections::HashSet::new();
        unbatched_steps(op, &mut photos, 0);
        if photos.len() >= 2 {
            set_batch(op, next_batch_id(), 0);
        }
    }
}
