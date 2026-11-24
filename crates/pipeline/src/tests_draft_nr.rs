//! Draft noise reduction during slider drags (task 4.5, `local::denoise_lum_draft`): a Luminance
//! drag keeps the colour-NR'd image and reruns only a cheap luminance NR; the release render (and
//! every full-quality render or export) is exactly the render it always was.

use crate::local::{denoise_lum_draft, denoise_with};
use crate::{Quality, RenderRequest, SourceInfo, StageCache, render, render_cached};
use lcc_develop::{DevelopSettings, Process};
use lcc_raster::{Rgb32f, Rgba8};
use std::sync::Arc;

/// A small noisy linear scene: gradient, edge, texture, luminance and colour noise.
fn noisy() -> Rgb32f {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    };
    let n: Vec<[f32; 3]> = (0..480 * 320).map(|_| [rnd(), rnd(), rnd()]).collect();
    Rgb32f::from_fn(480, 320, |x, y| {
        let r = n[y * 480 + x];
        let base = 0.05 + 0.4 * x as f32 / 480.0 + if y > 160 { 0.2 } else { 0.0 } + 0.05 * (x as f32 / 5.0).sin();
        let l = base * (1.0 + 0.4 * r[0]);
        [l * (1.0 + 0.3 * r[1]), l, l * (1.0 + 0.3 * r[2])]
    })
}

fn mean_diff(a: &Rgba8, b: &Rgba8) -> f64 {
    let n = (a.data.len() * 3).max(1) as f64;
    a.data.iter().zip(&b.data).flat_map(|(x, y)| (0..3).map(move |k| (x[k] as i32 - y[k] as i32).abs() as f64)).sum::<f64>() / n
}

fn edits(process: Process) -> DevelopSettings {
    let mut s = DevelopSettings { process: process.version(), ..Default::default() };
    s.detail.nr_color = 40.0;
    s.detail.nr_luminance = 20.0;
    s
}

#[test]
fn release_render_after_a_luminance_drag_is_unchanged() {
    let src = Arc::new(noisy());
    let info = SourceInfo { raw: true, ..Default::default() };
    for process in [Process::V8, Process::V5] {
        let cache = StageCache::default();
        let draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(320, 320) };
        let full = RenderRequest { quality: Quality::Full, ..draft };
        let mut s = edits(process);
        for v in [20.0, 35.0, 50.0, 65.0, 80.0] {
            s.detail.nr_luminance = v;
            s.detail.nr_detail = v / 2.0;
            let d = render_cached(&src, &info, &s, &draft, &cache).image;
            // a cached draft is the draft an uncached render makes
            assert_eq!(d, render(&src, &info, &s, &draft).image, "{process:?} nr {v}");
            // and close to the final look (the drag shows what release will give)
            let f = render(&src, &info, &s, &full).image;
            let m = mean_diff(&d, &f);
            assert!(m < 2.0, "{process:?} nr {v}: draft differs from the release render by {m:.2} levels on average");
        }
        // release: exact, cached or not, and the same as a fresh export of the settings
        let released = render_cached(&src, &info, &s, &full, &cache).image;
        assert_eq!(released, render(&src, &info, &s, &full).image, "{process:?}");
        let export = RenderRequest { quality: Quality::Full, ..RenderRequest::fit(320, 320) };
        assert_eq!(released, render(&src, &info, &s, &export).image, "{process:?}");
    }
}

#[test]
fn a_luminance_drag_reuses_the_colour_nr_image() {
    let src = Arc::new(noisy());
    let info = SourceInfo { raw: true, ..Default::default() };
    let cache = StageCache::default();
    let draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(320, 320) };
    let mut s = edits(Process::V8);
    render_cached(&src, &info, &s, &draft, &cache);
    let col = |c: &StageCache| c.entries.lock().unwrap().first().and_then(|e| e.draft_col.clone()).unwrap().1;
    let first = col(&cache);
    s.detail.nr_luminance = 60.0;
    render_cached(&src, &info, &s, &draft, &cache);
    assert!(Arc::ptr_eq(&first, &col(&cache)), "a Luminance drag redid the colour NR");
    // a Color change makes a new one
    s.detail.nr_color = 70.0;
    render_cached(&src, &info, &s, &draft, &cache);
    assert!(!Arc::ptr_eq(&first, &col(&cache)));
    // a full render keeps none (it never uses it)
    render_cached(&src, &info, &s, &RenderRequest { quality: Quality::Full, ..draft }, &cache);
    assert!(cache.entries.lock().unwrap().first().is_some_and(|e| e.draft_col.is_none()));
}

#[test]
fn draft_luminance_nr_is_the_full_filter_and_safe_on_hostile_input() {
    let img = noisy();
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 50.0;
    let (mut a, mut b) = (img.clone(), img.clone());
    denoise_with(&mut a, &s, 1.0, false, None);
    denoise_lum_draft(&mut b, &s, 1.0);
    assert_eq!(a.data, b.data);
    // tiny, empty and non-finite images, and hostile slider values: no panic, finite where finite
    for (w, h) in [(0, 0), (1, 1), (1, 7), (3, 2), (4, 4), (5, 1)] {
        for v in [f64::NAN, f64::INFINITY, -50.0, 1e9, 100.0] {
            let mut t = DevelopSettings::default();
            t.detail.nr_luminance = v;
            t.detail.nr_detail = v;
            let mut img = Rgb32f::filled(w, h, [0.2, 0.3, 0.4]);
            denoise_lum_draft(&mut img, &t, 1.0);
            assert!(img.data.iter().flatten().all(|c| c.is_finite()), "{w}x{h} {v}");
            denoise_lum_draft(&mut img, &t, f32::NAN);
        }
    }
    let mut img = Rgb32f::filled(6, 6, [f32::NAN, -1.0, f32::INFINITY]);
    denoise_lum_draft(&mut img, &s, 1.0);
}
