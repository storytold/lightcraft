//! Highlights / Shadows keep the local contrast of the tones they move (issue #632), from process
//! V2; V1 keeps the base it always had (`docs/process-versions.md`).

use std::sync::Arc;

use lcc_develop::{DevelopSettings, Process, ProcessVersion, controls};
use lcc_raster::{Plane, Rgb32f};

use crate::{Quality, RenderRequest, SourceInfo, StageCache, local, render, render_cached};

const W: usize = 1200;
const H: usize = 600;

/// Left half: a bright sky crossed by 1 px twigs (≈ 2.5 EV darker) and a little texture; right
/// half: a dark ground with 1 px light stalks. The two halves meet at x = W/2 (a strong edge).
fn scene() -> Rgb32f {
    Rgb32f::from_fn(W, H, |x, y| {
        let t = 1.0 + 0.06 * (((x * 7 + y * 13) % 5) as f32 - 2.0) / 2.0;
        let line = on_line(x, y);
        let v = if x < W / 2 {
            if line { 0.14 } else { 0.85 }
        } else if line {
            0.06
        } else {
            0.011
        };
        [v * t; 3]
    })
}

/// Thin lines in a chequer of 48 px cells: twigs where a branch crosses the sky, open sky between.
fn on_line(x: usize, y: usize) -> bool {
    (x / 48 + y / 48).is_multiple_of(2) && (x % 4 == 1 || y % 5 == 2)
}

/// Settings on `process`.
fn on(process: Process) -> DevelopSettings {
    DevelopSettings { process: process.version(), ..DevelopSettings::default() }
}

/// V2 settings with control `id` at `v`.
fn with(id: &str, v: f64) -> DevelopSettings {
    with_on(Process::V2, id, v)
}

fn with_on(process: Process, id: &str, v: f64) -> DevelopSettings {
    let mut s = on(process);
    controls::set(&mut s, id, v);
    s
}

#[test]
fn zero_highlights_and_shadows_render_without_the_base() {
    let mut s = on(Process::V2);
    s.light.highlights = 0.0;
    s.light.shadows = 0.0;
    controls::set(&mut s, "effects.clarity", 40.0);
    // no base plane is computed or read (the per-pixel stage sees log luminance): the render is
    // the one it always was
    assert_eq!(local::plane_sigmas(&s, 1200.0, 1200.0, Quality::Full).base, None);
    assert!(local::plane_sigmas(&with("light.shadows", 1.0), 1200.0, 1200.0, Quality::Full).base.is_some());
}

#[test]
fn median_matches_a_plain_per_pixel_median() {
    let p =
        Plane::from_fn(37, 23, |x, y| (((x * 31 + y * 17) * 2_654_435_761usize) % 1000) as f32 / 250.0 - 2.0 + if x % 5 == 0 { 0.5 } else { 0.0 });
    for r in [1, 2, 4, 9] {
        let med = |v: &mut Vec<f32>| {
            v.sort_by(f32::total_cmp);
            v[r]
        };
        let rows = Plane::from_fn(37, 23, |x, y| med(&mut (0..=2 * r).map(|k| p.get((x + k).saturating_sub(r).min(36), y)).collect()));
        let want = Plane::from_fn(37, 23, |x, y| med(&mut (0..=2 * r).map(|k| rows.get(x, (y + k).saturating_sub(r).min(22))).collect()));
        assert_eq!(local::median_hv(&p, r).data, want.data, "r {r}");
    }
}

#[test]
fn v1_keeps_the_base_it_always_had() {
    // V1's base is the edge-aware filter of log luminance itself, as before process V2: no median
    // radius, and `tone_base` at 0 is that filter bit for bit
    let v1 = with_on(Process::V1, "light.highlights", -100.0);
    assert_eq!(ProcessVersion::legacy().process(), Process::V1);
    for ppl in [300.0, 1200.0, 5568.0, 1e6] {
        for q in [Quality::Full, Quality::Draft] {
            let sig = local::plane_sigmas(&v1, ppl, ppl, q);
            assert!(sig.base.is_some());
            assert_eq!(sig.base_detail, 0, "V1 at {ppl} px");
            assert_eq!(local::tone_base_detail(&v1, ppl), 0);
        }
    }
    let l =
        Plane::from_fn(301, 157, |x, y| (((x * 31 + y * 17) * 2_654_435_761usize) % 1000) as f32 / 250.0 - 2.0 + if x % 5 == 0 { 0.5 } else { 0.0 });
    for sg in [1.0f32, 4.5, 18.0] {
        assert_eq!(local::tone_base(&l, sg, 0).data, local::guided_fast(&l, sg, local::BASE_EPS).data, "sigma {sg}");
    }
    // V2 takes the median wherever the image is big enough for one
    let v2 = with("light.highlights", -100.0);
    assert_eq!(local::plane_sigmas(&v2, 5568.0, 5568.0, Quality::Full).base_detail, local::base_detail_radius(5568.0));
    assert_eq!(local::plane_sigmas(&v2, 300.0, 300.0, Quality::Full).base_detail, 0);
}

#[test]
fn the_base_plane_is_cached_per_process() {
    let s1 = with_on(Process::V1, "light.highlights", -100.0);
    let s2 = with("light.highlights", -100.0);
    // moving a photo between processes (Update to Current Process, then undo) with warm caches
    // renders what a fresh render does
    let src = Arc::new(scene());
    let (info, req) = (SourceInfo::default(), RenderRequest::fit(W, H));
    let cache = StageCache::default();
    for s in [&s1, &s2, &s1, &s2] {
        let warm = render_cached(&src, &info, s, &req, &cache).image;
        assert!(warm == render(&src, &info, s, &req).image, "process {:?}", s.process);
    }
    let sig = local::plane_sigmas(&s2, W as f64, W as f64, Quality::Full);
    let sg = sig.base.unwrap();
    assert_ne!(local::base_key(&s1, sg, 0), local::base_key(&s2, sg, 0), "the process is part of the key");
    assert_ne!(local::base_key(&s2, sg, 0), local::base_key(&s2, sg, sig.base_detail));
}
