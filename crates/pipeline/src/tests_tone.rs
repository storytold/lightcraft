//! Highlights / Shadows keep the local contrast of the tones they move (issue #632), from process
//! V2; V1 keeps the base it always had (`docs/process-versions.md`).

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Process, ProcessVersion, controls};
use lightcraft_raster::{Plane, Rgb32f, Rgba8};

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

/// log2 of (mean luma off the lines / mean luma on them) over columns `x0..x1`: the twigs' (or
/// stalks') contrast against their surroundings.
fn line_contrast(img: &Rgba8, x0: usize, x1: usize) -> f32 {
    let (mut on, mut off) = ((0.0f64, 0.0f64), (0.0f64, 0.0f64));
    for y in 40..H - 40 {
        for x in x0..x1 {
            let v = luma(img.data[y * W + x]) as f64;
            let acc = if on_line(x, y) { &mut on } else { &mut off };
            acc.0 += v;
            acc.1 += 1.0;
        }
    }
    ((off.0 / off.1) / (on.0 / on.1)).log2().abs() as f32
}

fn shot(s: &DevelopSettings) -> Rgba8 {
    render(&scene(), &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
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

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// (mean, rel. local contrast: std of luma minus its 9×9 mean, over the mean) of the block
/// `x0..x1 × 40..H-40`.
fn stats(img: &Rgba8, x0: usize, x1: usize) -> (f32, f32) {
    let l = |x: usize, y: usize| luma(img.data[y * W + x]);
    let (mut sum, mut hp, mut n) = (0.0f64, Vec::new(), 0.0f64);
    for y in 40..H - 40 {
        for x in x0..x1 {
            let mut m = 0.0;
            for dy in 0..9 {
                for dx in 0..9 {
                    m += l(x + dx - 4, y + dy - 4);
                }
            }
            sum += l(x, y) as f64;
            hp.push(l(x, y) - m / 81.0);
            n += 1.0;
        }
    }
    let mean = (sum / n) as f32;
    let var = hp.iter().map(|v| v * v).sum::<f32>() / hp.len() as f32;
    (mean, var.sqrt() / mean)
}

#[test]
fn highlights_keep_the_local_contrast_of_bright_areas() {
    let (plain, hl, exp) = (shot(&on(Process::V2)), shot(&with("light.highlights", -100.0)), shot(&with("light.exposure", -1.0)));
    let (bright, flat_dark) = ((40, 540), (700, 1160));
    let ((m0, c0), (mh, ch), (_, ce)) = (stats(&plain, bright.0, bright.1), stats(&hl, bright.0, bright.1), stats(&exp, bright.0, bright.1));
    let (k0, kh, ke) = (line_contrast(&plain, bright.0, bright.1), line_contrast(&hl, bright.0, bright.1), line_contrast(&exp, bright.0, bright.1));
    // the sky comes down as much as with −1 EV or more…
    assert!(mh < m0 - 25.0, "highlights −100 mean {mh} vs {m0}");
    // …and keeps its twigs as −1 EV does (the edge-aware base alone followed them: 0.73 of
    // Exposure's twig contrast and 0.79 of its local contrast were left)
    assert!(kh > 0.9 * ke && kh > 0.9 * k0, "highlights −100 twig contrast {kh} EV vs exposure −1 {ke}, plain {k0}");
    assert!(ch > 0.9 * ce, "highlights −100 local contrast {ch} vs exposure −1 {ce}, plain {c0}");
    // the dark half is not touched
    let (d0, dh) = (stats(&plain, flat_dark.0, flat_dark.1).0, stats(&hl, flat_dark.0, flat_dark.1).0);
    assert!((dh - d0).abs() < 1.5, "dark half {dh} vs {d0}");
}

#[test]
fn shadows_keep_the_local_contrast_of_dark_areas() {
    let (plain, sh, exp) = (shot(&on(Process::V2)), shot(&with("light.shadows", 100.0)), shot(&with("light.exposure", 1.0)));
    let dark = (640, 1160);
    let ((m0, _), (ms, cs), (_, ce)) = (stats(&plain, dark.0, dark.1), stats(&sh, dark.0, dark.1), stats(&exp, dark.0, dark.1));
    let (k0, ks, ke) = (line_contrast(&plain, dark.0, dark.1), line_contrast(&sh, dark.0, dark.1), line_contrast(&exp, dark.0, dark.1));
    assert!(ms > m0 + 10.0, "shadows +100 mean {ms} vs {m0}");
    // (the edge-aware base alone: 0.75 of Exposure's stalk contrast, 0.72 of its local contrast)
    assert!(ks > 0.9 * ke, "shadows +100 stalk contrast {ks} EV vs exposure +1 {ke}, plain {k0}");
    assert!(cs > 0.9 * ce, "shadows +100 local contrast {cs} vs exposure +1 {ce}");
    // the bright half is not lifted
    let (b0, bs) = (stats(&plain, 40, 540).0, stats(&sh, 40, 540).0);
    assert!((bs - b0).abs() < 1.5, "bright half {bs} vs {b0}");
}

#[test]
fn highlights_add_no_halo_at_a_strong_edge() {
    // a flat bright field against a flat dark one: the bright pixels next to the edge come down
    // (nearly) as much as those far from it, the dark ones stay where they are (the guided
    // filter's own softness leaves up to 4 levels within a few px of the edge, as before)
    let src = Rgb32f::from_fn(W, H, |x, _| if x < W / 2 { [0.85; 3] } else { [0.011; 3] });
    let shot = |s: &DevelopSettings| render(&src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image;
    let (plain, hl) = (shot(&on(Process::V2)), shot(&with("light.highlights", -100.0)));
    let at = |img: &Rgba8, x: usize| luma(img.data[(H / 2) * W + x]);
    let far = at(&hl, 100) - at(&plain, 100);
    for x in (W / 2 - 40..W / 2).chain(W / 2..W / 2 + 40) {
        let d = at(&hl, x) - at(&plain, x);
        let want = if x < W / 2 { far } else { 0.0 };
        assert!((d - want).abs() <= 5.0, "x {x}: change {d}, far from the edge {want}");
    }
}

#[test]
fn zero_highlights_and_shadows_render_without_the_base() {
    let mut s = on(Process::V2);
    s.light.highlights = 0.0;
    s.light.shadows = 0.0;
    controls::set(&mut s, "effects.clarity", 40.0);
    // no base plane is computed or read (the per-pixel stage sees log luminance): the render is
    // the one it always was
    assert_eq!(local::plane_sigmas(&s, 1200.0, Quality::Full).base, None);
    assert!(local::plane_sigmas(&with("light.shadows", 1.0), 1200.0, Quality::Full).base.is_some());
}

#[test]
fn median_keeps_steps_and_drops_thin_lines() {
    let p = Plane::from_fn(40, 30, |x, y| if x < 20 { -2.0 } else { 2.0 } + if y == 15 || x == 8 { -3.0 } else { 0.0 });
    let m = local::median_hv(&p, 2);
    for y in 0..30 {
        for x in 0..40 {
            let want = if x < 20 { -2.0 } else { 2.0 };
            assert_eq!(m.get(x, y), want, "({x}, {y})");
        }
    }
    assert_eq!(local::median_hv(&p, 0).data, p.data);
    assert_eq!(local::base_detail_radius(5568.0), 8);
    assert_eq!(local::base_detail_radius(300.0), 0);
    assert_eq!(local::base_detail_radius(1e9), local::BASE_DETAIL_MAX);
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
            let sig = local::plane_sigmas(&v1, ppl, q);
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
    assert_eq!(local::plane_sigmas(&v2, 5568.0, Quality::Full).base_detail, local::base_detail_radius(5568.0));
    assert_eq!(local::plane_sigmas(&v2, 300.0, Quality::Full).base_detail, 0);
}

#[test]
fn only_v2_keeps_the_twigs_under_highlights() {
    let bright = (40, 540);
    let (p1, p2) = (shot(&on(Process::V1)), shot(&on(Process::V2)));
    // without Highlights / Shadows the processes render alike
    assert!(p1 == p2, "plain renders differ between V1 and V2");
    let (h1, h2, e) =
        (shot(&with_on(Process::V1, "light.highlights", -100.0)), shot(&with("light.highlights", -100.0)), shot(&with("light.exposure", -1.0)));
    let (k1, k2, ke) = (line_contrast(&h1, bright.0, bright.1), line_contrast(&h2, bright.0, bright.1), line_contrast(&e, bright.0, bright.1));
    // V1 is the base from before (≈ 0.73 of Exposure's twig contrast), V2 keeps them
    assert!(k1 < 0.85 * ke && k2 > 0.9 * ke, "twig contrast V1 {k1} EV, V2 {k2} EV, exposure −1 {ke} EV");
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
    let sig = local::plane_sigmas(&s2, W as f64, Quality::Full);
    let sg = sig.base.unwrap();
    assert_ne!(local::base_key(&s1, sg, 0), local::base_key(&s2, sg, 0), "the process is part of the key");
    assert_ne!(local::base_key(&s2, sg, 0), local::base_key(&s2, sg, sig.base_detail));
}
