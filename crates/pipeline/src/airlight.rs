//! Full-frame Dehaze statistics for windows. Each renderer keeps its own scalar so its
//! sampling, upstream stages and floating-point implementation match its full render.

use crate::{Plan, SourceInfo, local};
use lightcraft_raster::Rgb32f;
use std::sync::{Arc, Mutex, Weak};

pub struct AirlightCache {
    entries: Mutex<Vec<(Weak<Rgb32f>, u64, f32)>>,
    budget: u64,
}
impl Default for AirlightCache {
    fn default() -> Self {
        Self::with_budget(PREPARE_BUDGET)
    }
}

impl AirlightCache {
    /// Limit exact preparation for this view. Smaller budgets use the cached proxy.
    pub fn with_budget(budget: u64) -> Self {
        Self { entries: Mutex::new(Vec::new()), budget }
    }
    pub fn budget(&self) -> u64 {
        self.budget
    }
    pub fn clear(&self) {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Compute once for this source and full-frame linear-stage key. The key excludes the
    /// window rectangle and downstream adjustments, including Exposure and Dehaze amount.
    pub fn get_or_compute(&self, src: &Arc<Rgb32f>, key: u64, compute: impl FnOnce() -> f32) -> f32 {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, air)) = entries.iter().find(|(s, k, _)| *k == key && s.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src))) {
            return *air;
        }
        let air = compute();
        Self::put(&mut entries, src, key, air);
        air
    }

    pub fn get(&self, src: &Arc<Rgb32f>, key: u64) -> Option<f32> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(s, k, _)| *k == key && s.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src)))
            .map(|(_, _, air)| *air)
    }

    /// Seed the scalar from an already prepared full-frame dark channel.
    pub fn remember(&self, src: &Arc<Rgb32f>, key: u64, air: f32) {
        Self::put(&mut self.entries.lock().unwrap_or_else(|e| e.into_inner()), src, key, air);
    }

    fn put(entries: &mut Vec<(Weak<Rgb32f>, u64, f32)>, src: &Arc<Rgb32f>, key: u64, air: f32) {
        entries.retain(|(s, k, _)| s.upgrade().is_some_and(|s| *k != key || !Arc::ptr_eq(&s, src)));
        entries.push((Arc::downgrade(src), key, air));
        if entries.len() > 2 {
            entries.remove(0);
        }
    }
}

/// One full preparation, retained only below 1 GiB. Source ownership stays weak.
/// This lets pans use exactly the same upstream arithmetic as the first window.
pub struct FrameCache<T> {
    entry: Mutex<Option<(Weak<Rgb32f>, u64, Arc<T>, usize)>>,
}
impl<T> Default for FrameCache<T> {
    fn default() -> Self {
        Self { entry: Mutex::new(None) }
    }
}
impl<T> FrameCache<T> {
    pub const BUDGET: usize = 1024 * 1024 * 1024;
    pub fn clear(&self) {
        *self.entry.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    pub fn get(&self, src: &Arc<Rgb32f>) -> Option<(u64, Arc<T>)> {
        self.entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(s, _, _, _)| s.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src)))
            .map(|(_, key, data, _)| (*key, data.clone()))
    }
    pub fn value(&self) -> Option<Arc<T>> {
        self.entry.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(_, _, data, _)| data.clone())
    }
    pub fn put(&self, src: &Arc<Rgb32f>, key: u64, data: Arc<T>, bytes: usize) {
        *self.entry.lock().unwrap_or_else(|e| e.into_inner()) = (bytes <= Self::BUDGET).then(|| (Arc::downgrade(src), key, data, bytes));
    }
}

/// Maximum pixels in a window's work image or exact-statistic image. A 61 MP source
/// fits; a larger exact-statistic frame uses the proxy before sampling it.
/// Each RGB allocation is at most 768 MiB, including on the GPU's CPU fallback.
pub const MAX_PIXELS: usize = 64 * 1024 * 1024;
/// Temporary storage for preparing an exact statistic and retaining the work rectangle.
/// The source and existing stage entries belong to their callers and are not included.
pub const PREPARE_BUDGET: u64 = 4 * 1024 * 1024 * 1024;

pub fn check_pixels(w: usize, h: usize) -> Result<(), String> {
    if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(format!(
            "The {w} by {h} zoom frame exceeds the exact-render memory budget. Use a smaller source or reduce the zoom or output size."
        ));
    }
    Ok(())
}

/// Peak live CPU image storage, excluding the caller's source and existing caches.
/// Two RGB images use 24 bytes/pixel. Luminance NR adds five scalar planes (20),
/// colour NR adds two RGB buffers (24), and dark-channel blur adds two planes (8).
/// These stages run sequentially. Cropping copies the retained 28-byte or
/// 16-byte preparation. Row scratch is bounded separately.
pub fn prepare_bytes(full: &Plan<'_>, work: Option<&Plan<'_>>, gpu: bool) -> u64 {
    let n = (full.w as u64).saturating_mul(full.h as u64);
    let (lum, col) = local::nr_params(&full.settings, full.detail_scale);
    // GPU luminance NR peaks while writing RGB output with its input, log
    // luminance, coefficients and filtered luminance still alive (28 bytes).
    let scratch = if gpu && lum.is_some() {
        28
    } else if col.is_some() {
        24
    } else if lum.is_some() {
        20
    } else {
        8
    };
    let filtering = n.saturating_mul(24 + scratch);
    // CPU linear stages upload their result while the host image and sampled
    // device image still exist. GPU red-eye also keeps input and output RGB.
    let hybrid = if gpu && (!full.settings.spots.is_empty() || full.settings.profile.camera_local_tone || !full.eyes.is_empty()) {
        n.saturating_mul(36)
    } else {
        0
    };
    let defringe = if crate::optics::defringe_radius(&full.settings, full.px_per_long / crate::optics::DEFRINGE_REF_LONG) > 0 {
        n.saturating_mul(52)
    } else {
        0
    };
    let retouch =
        n.saturating_mul(24).saturating_add(crate::spots::preparation_scratch(&full.settings.spots, &full.frame, full.w, full.h, full.px_per_long));
    let retained = if n.saturating_mul(28) <= FrameCache::<Full>::BUDGET as u64 { 28 } else { 16 };
    let cropping = n.saturating_mul(retained).saturating_add(work.map_or(0, |p| (p.w as u64).saturating_mul(p.h as u64).saturating_mul(retained)));
    let sampling = full.frame.sample_plan(full.frame.src_w, full.frame.src_h, full.w, full.h);
    let oriented = if full.frame.orient == lightcraft_geom::Orientation::Normal {
        0
    } else {
        (sampling.ow as u64).saturating_mul(sampling.oh as u64).saturating_mul(12)
    };
    let prefilter =
        sampling.prefilter.map_or(0, |(w, h)| (w as u64).saturating_mul((h as u64).saturating_add(sampling.oh as u64)).saturating_mul(12));
    // Each active worker can hold row scratch. The filter allowance also covers selection
    // (one f32 every seven pixels) and GPU subsample readback at the dark stage.
    let row_scratch = (full.w.max(full.h) as u64).saturating_mul(24).saturating_mul(rayon::current_num_threads() as u64);
    filtering
        .max(hybrid)
        .max(defringe)
        .max(retouch)
        .max(cropping)
        .max(oriented.saturating_add(prefilter).saturating_add(n.saturating_mul(12)))
        .saturating_add(row_scratch)
}

/// Bound the CPU full pass before allocating it.
pub fn check_prepare(full: &Plan<'_>, work: Option<&Plan<'_>>) -> Result<(), String> {
    check_prepare_budget(full, work, PREPARE_BUDGET, false)
}

pub fn check_prepare_budget(full: &Plan<'_>, work: Option<&Plan<'_>>, budget: u64, gpu: bool) -> Result<(), String> {
    check_pixels(full.w, full.h)?;
    if prepare_bytes(full, work, gpu) > budget {
        return Err("The exact Dehaze preparation exceeds its 4 GiB memory budget. Use a smaller source or reduce the zoom or output size.".into());
    }
    Ok(())
}

/// Main's cached 384-pixel proxy. It preserves a rendered window when exact
/// preparation exceeds the budget, at the cost of approximate airlight.
pub fn proxy(src: &Rgb32f, info: &SourceInfo, full: &Plan<'_>) -> f32 {
    let k = (384.0 / full.w.max(full.h).max(1) as f64).min(1.0);
    let (w, h) = (((full.w as f64 * k).round() as usize).max(1), ((full.h as f64 * k).round() as usize).max(1));
    local::frame_airlight(src, info, &full.settings, &full.frame, w, h)
}

/// The exact full preparation. Work rectangles are copies, with no resampling.
pub(crate) struct Full {
    pub geo: u64,
    pub sampled: Option<Arc<Rgb32f>>,
    pub lin: Arc<Rgb32f>,
    pub dark: Arc<lightcraft_raster::Plane>,
    pub air: f32,
}
pub(crate) struct Work {
    pub sampled: Option<Arc<Rgb32f>>,
    pub lin: Arc<Rgb32f>,
    pub dark: Arc<lightcraft_raster::Plane>,
}
impl Full {
    pub fn window(&self, plan: &Plan<'_>) -> Work {
        let (x, y) = plan.frame.view.map_or((0, 0), |v| (v.x as usize, v.y as usize));
        Work {
            sampled: self.sampled.as_ref().map(|s| Arc::new(s.crop(x, y, plan.w, plan.h))),
            lin: Arc::new(self.lin.crop(x, y, plan.w, plan.h)),
            dark: Arc::new(self.dark.crop(x, y, plan.w, plan.h)),
        }
    }
    pub fn bytes(&self) -> usize {
        self.sampled.as_ref().map_or(0, |s| s.len() * 12) + self.lin.len() * 12 + self.dark.len() * 4
    }
}

pub(crate) fn prepare(
    src: &Rgb32f,
    info: &SourceInfo,
    full: &Plan<'_>,
    work: Option<&Plan<'_>>,
    sampled: Option<Arc<Rgb32f>>,
) -> Result<Full, String> {
    check_prepare(full, work)?;
    check_pixels(src.width, src.height)?;
    let sampled = sampled.unwrap_or_else(|| Arc::new(full.frame.sample(src, full.w, full.h)));
    let mut img = (*sampled).clone();
    crate::lin_cpu(&mut img, info, full);
    local::denoise(&mut img, &full.settings, full.detail_scale);
    let dark = img.map(local::dark_of);
    let sigma = (0.02 * full.px_per_long as f32).max(1.0);
    let dark = lightcraft_raster::blur::gaussian_owned(dark, sigma);
    let air = local::airlight(&dark);
    // Keep exact pans warm up to the pixel bound without keeping an extra RGB
    // image. Small frames also retain sampled pixels for upstream edits.
    let sampled = (img.len() * 28 <= FrameCache::<Full>::BUDGET).then_some(sampled);
    Ok(Full { geo: full.geo, sampled, lin: Arc::new(img), dark: Arc::new(dark), air })
}

/// CPU statistic with the full render's resolution, upstream stages and selection.
pub fn compute(src: &Rgb32f, info: &SourceInfo, full: &Plan<'_>) -> Result<f32, String> {
    prepare(src, info, full, None, None).map(|full| full.air)
}

#[cfg(test)]
pub(crate) mod tests {
    std::thread_local! { pub(crate) static LINEAR_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    use super::*;
    use crate::{PixelWindow, RenderRequest, StageCache};
    use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, WbMode};
    use lightcraft_geom::Point;

    #[test]
    fn statistic_matches_full_prepare_and_cache_tracks_upstream_inputs() {
        let src = Arc::new(Rgb32f::from_fn(121, 83, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let mut info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 100.0;
        let req = RenderRequest::fit(481, 327);
        let cache = AirlightCache::default();
        let mut last_key = None;
        for step in 0..10 {
            match step {
                1 => s.light.exposure = 0.5,
                2 => s.effects.dehaze = 80.0,
                3 => {
                    s.wb.mode = WbMode::Custom;
                    s.wb.temp = 9000.0;
                }
                4 => s.detail.nr_luminance = 80.0,
                5 => s.detail.nr_color = 60.0,
                6 => s.optics.defringe_purple_amount = 50.0,
                7 => s.crop.flip_h = true,
                8 => info.relative_wb = true,
                9 => info.native_long = Some(2000),
                _ => {}
            }
            let full = crate::plan(&src, &info, &s, &req);
            let air = compute(&src, &info, &full).unwrap();
            let mut img = full.frame.sample(&src, full.w, full.h);
            crate::lin_cpu(&mut img, &info, &full);
            local::denoise(&mut img, &s, full.detail_scale);
            let mut planes = local::Planes::default();
            let prep = local::prepare(
                Arc::new(img),
                &s,
                &full.frame,
                full.px_per_long,
                req.quality,
                &mut planes,
                None,
                (&mut crate::masks::AlphaCache::default(), crate::masks::AlphaKeys { geo: full.geo, image: full.lin_key }),
            );
            assert_eq!(air.to_bits(), prep.air.to_bits(), "upstream step {step}");
            let mut computed = false;
            assert_eq!(
                cache.get_or_compute(&src, full.lin_key, || {
                    computed = true;
                    air
                }),
                air
            );
            assert_eq!(computed, last_key != Some(full.lin_key));
            if matches!(step, 1 | 2) {
                assert!(!computed, "downstream edit invalidated airlight");
            } else if step < 8 {
                assert!(computed, "upstream edit reused airlight");
            }
            last_key = Some(full.lin_key);
        }
        let other = Arc::new((*src).clone());
        let full = crate::plan(&other, &info, &s, &req);
        let mut computed = false;
        cache.get_or_compute(&other, full.lin_key, || {
            computed = true;
            0.25
        });
        assert!(computed, "source identity must invalidate the statistic");
        cache.clear();
        assert!(cache.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn window_pan_and_exposure_reuse_full_statistic_and_refined_alphas() {
        let src = Arc::new(Rgb32f::from_fn(301, 203, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 100.0;
        for (id, shape) in
            [MaskShape::Linear { start: Point::new(0.2, 0.1), end: Point::new(0.8, 0.9) }, MaskShape::Subject, MaskShape::Sky].into_iter().enumerate()
        {
            s.masks.push(Mask {
                id: id as u32,
                refine: 100.0,
                components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
                ..Default::default()
            });
        }
        let cache = StageCache::default();
        let mut previous = Vec::new();
        for i in 0..3 {
            s.light.exposure = i as f64 * 0.1;
            let req = RenderRequest {
                window: Some(PixelWindow { x: 51 + if i == 2 { 3 } else { 0 }, y: 13, w: 71, h: 53 }),
                ..RenderRequest::fit(601, 403)
            };
            let warm = crate::render_cached(&src, &info, &s, &req, &cache).unwrap();
            assert_eq!(warm.image, crate::render(&src, &info, &s, &req).unwrap().image);
            let entries = cache.lock();
            let current: Vec<_> = entries.last().unwrap().masks.planes().cloned().collect();
            if i == 1 {
                assert!(Arc::ptr_eq(&previous[0], &current[0]), "geometric refined alpha");
                assert!(Arc::ptr_eq(&previous[1], &current[1]), "Subject heuristic refined alpha");
                assert!(!Arc::ptr_eq(&previous[2], &current[2]), "Sky reads Exposure");
            }
            if i == 2 {
                assert!(previous.iter().zip(&current).all(|(a, b)| !Arc::ptr_eq(a, b)), "a different work rect invalidates alphas");
            }
            previous = current;
            assert_eq!(cache.airlight.entries.lock().unwrap().len(), 1, "panning must reuse the full-frame statistic");
        }
    }
}

#[cfg(test)]
mod reuse_regression {
    use crate::*;

    #[test]
    fn trimmed_cpu_windows_reuse_the_full_statistic() {
        let src = Arc::new(Rgb32f::from_fn(1201, 803, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 40.0;
        s.effects.clarity = 30.0;
        s.detail.nr_luminance = 30.0;
        s.detail.nr_color = 25.0;
        let req = RenderRequest { window: Some(PixelWindow { x: 401, y: 277, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
        let cache = StageCache::default();
        render_cached(&src, &info, &s, &req, &cache).unwrap();
        assert!(cache.airlight_frame.value().is_some());
        for step in 0..3 {
            cache.trim_images();
            s.light.exposure = step as f64 * 0.1;
            let req = RenderRequest { window: Some(PixelWindow { x: 401 + step * 3, y: 277, w: 121, h: 83 }), ..req };
            airlight::tests::LINEAR_PASSES.with(|n| n.set(0));
            let reused = render_cached(&src, &info, &s, &req, &cache).unwrap();
            assert_eq!(airlight::tests::LINEAR_PASSES.with(|n| n.get()), 1, "only the work window needs upstream preparation");
            assert!(cache.airlight_frame.value().is_none(), "a trim must not refill the full-frame preparation");
            let fresh = render(&src, &info, &s, &req).unwrap();
            let max = reused.image.data.iter().flatten().zip(fresh.image.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(max <= 1, "step {step}: scalar reuse changed requested pixels by {max} codes");
        }
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = 7800.0;
        render_cached(&src, &info, &s, &req, &cache).unwrap();
        assert!(cache.airlight_frame.value().is_some(), "an upstream edit must recompute the full statistic");
    }

    #[test]
    fn dehaze_window_prepares_upstream_once_per_edit() {
        let src = Arc::new(Rgb32f::from_fn(1201, 803, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 15.0;
        let req = RenderRequest { window: Some(PixelWindow { x: 400, y: 300, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
        let cache = StageCache::default();
        for step in 0..2 {
            if step == 1 {
                s.wb.mode = lightcraft_develop::WbMode::Custom;
                s.wb.temp = 7800.0;
            }
            airlight::tests::LINEAR_PASSES.with(|n| n.set(0));
            let _out = render_cached(&src, &info, &s, &req, &cache).unwrap();
            assert_eq!(airlight::tests::LINEAR_PASSES.with(|n| n.get()), 1, "step {step}: full statistic and window must share upstream preparation");
        }
    }
}

#[cfg(test)]
mod frame_budget_tests {
    use super::*;

    #[test]
    fn small_budget_renders_with_proxy_and_retries_exact_after_upstream_edits() {
        use crate::{PixelWindow, RenderRequest, StageCache};
        let src = Arc::new(Rgb32f::from_fn(1201, 803, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = lightcraft_develop::DevelopSettings::default();
        s.effects.dehaze = 15.0;
        s.detail.nr_color = 25.0;
        let mut cache = StageCache { airlight: AirlightCache::with_budget(1), ..Default::default() };
        for step in 0..4 {
            s.light.exposure = step as f64 * 0.2;
            if step == 2 {
                s.detail.nr_color = 40.0;
            }
            let req = RenderRequest { window: Some(PixelWindow { x: 400 + step * 3, y: 300, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
            let full = crate::plan(&src, &info, &s, &RenderRequest { window: None, ..req });
            let plan = crate::plan(&src, &info, &s, &req);
            assert!(check_prepare_budget(&full, Some(&plan), 1, false).is_err());
            assert!(check_prepare(&full, Some(&plan)).is_ok());
            let out = crate::render_cached(&src, &info, &s, &req, &cache).unwrap();
            let fresh = StageCache { airlight: AirlightCache::with_budget(1), ..Default::default() };
            assert_eq!(out.image, crate::render_cached(&src, &info, &s, &req, &fresh).unwrap().image);
            assert!(cache.airlight_frame.value().is_none());
            assert_eq!((out.image.width, out.image.height), (121, 83));
        }
        cache.airlight = AirlightCache::default();
        let req = RenderRequest { window: Some(PixelWindow { x: 400, y: 300, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
        let exact = crate::render_cached(&src, &info, &s, &req, &cache).unwrap();
        assert!(cache.airlight_frame.value().is_some());
        assert_eq!(exact.image, crate::render(&src, &info, &s, &req).unwrap().image);
    }

    #[test]
    fn preparation_counts_sequential_nr_storage_and_crop_peak() {
        let src = Rgb32f::new(3, 2);
        let info = SourceInfo::default();
        let mut s = lightcraft_develop::DevelopSettings::default();
        let req = crate::RenderRequest::fit(6000, 4000);
        for (lum, col, cpu, gpu) in [(0., 0., 32, 32), (30., 0., 44, 52), (0., 25., 48, 48), (30., 25., 48, 52)] {
            s.detail.nr_luminance = lum;
            s.detail.nr_color = col;
            let p = crate::plan(&src, &info, &s, &req);
            let rows = 6000 * 24 * rayon::current_num_threads() as u64;
            assert_eq!(prepare_bytes(&p, None, false), 24_000_000 * cpu + rows);
            assert_eq!(prepare_bytes(&p, None, true), 24_000_000 * gpu + rows);
        }
    }

    #[test]
    fn retained_frames_obey_the_budget_and_release_source_and_data() {
        let cache = FrameCache::<u8>::default();
        let src = Arc::new(Rgb32f::new(1, 1));
        let source = Arc::downgrade(&src);
        let data = Arc::new(7);
        cache.put(&src, 10, data.clone(), FrameCache::<u8>::BUDGET);
        assert_eq!(cache.get(&src).unwrap().0, 10);
        assert_eq!(Arc::strong_count(&src), 1);
        cache.put(&src, 11, data.clone(), FrameCache::<u8>::BUDGET + 1);
        assert!(cache.get(&src).is_none());
        assert_eq!(Arc::strong_count(&data), 1);
        cache.put(&src, 12, data.clone(), 1);
        drop(src);
        assert!(source.upgrade().is_none());
        cache.clear();
        assert_eq!(Arc::strong_count(&data), 1);
        assert!(check_pixels(usize::MAX, usize::MAX).is_err());
    }
}
