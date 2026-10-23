//! Dehaze from process V3 (issue #1237): haze is estimated from a patch-minimum dark channel, the
//! response eases in, and no pixel is pushed to black; V1 and V2 keep their rendering.

use lightcraft_develop::{DevelopSettings, Process, controls};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, finish::dehaze_v3_pixel, local, render};

const W: usize = 600;
const H: usize = 300;

/// Left: bright sky (0.8); right: a textured ground (a 0.03 / 0.30 chequer of 6 px cells) whose
/// patches all hold dark pixels: no haze there.
fn scene() -> Rgb32f {
    Rgb32f::from_fn(W, H, |x, y| {
        let v = if x < W / 2 {
            0.8
        } else if (x / 6 + y / 6).is_multiple_of(2) {
            0.03
        } else {
            0.30
        };
        [v; 3]
    })
}

fn shot(p: Process, dehaze: f64) -> Rgba8 {
    let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
    controls::set(&mut s, "effects.dehaze", dehaze);
    render(&scene(), &SourceInfo::default(), &s, &RenderRequest::fit(W, H)).image
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Mean luma and the share of black (< 2) pixels of the ground, columns `x0..x1`.
fn ground(img: &Rgba8, x0: usize, x1: usize) -> (f32, f32) {
    let (mut sum, mut black, mut n) = (0.0, 0.0, 0.0);
    for y in 20..H - 20 {
        for x in x0..x1 {
            let v = luma(img.data[y * W + x]);
            sum += v;
            black += f32::from(u8::from(v < 2.0));
            n += 1.0;
        }
    }
    (sum / n, black / n)
}

#[test]
fn v3_leaves_haze_free_texture_and_shadows_by_the_sky_alone() {
    let ev = |a: f32, b: f32| (b / a).log2();
    for p in [Process::V1, Process::V3] {
        let (m0, _) = ground(&shot(p, 0.0), W / 2 + 20, W);
        let (m50, _) = ground(&shot(p, 50.0), W / 2 + 20, W);
        let (_, black10) = ground(&shot(p, 10.0), W / 2, W / 2 + 12);
        let (_, black50) = ground(&shot(p, 50.0), W / 2, W / 2 + 12);
        eprintln!("{p:?}: ground {m0:.1} -> {m50:.1} at +50 ({:.2} EV); black by the sky {black10:.2} (+10) {black50:.2} (+50)", ev(m0, m50));
        if p == Process::V3 {
            assert!(ev(m0, m50) > -0.25, "V3 darkens a haze-free ground by {:.2} EV at +50", ev(m0, m50));
            assert!(black10 < 0.01 && black50 < 0.01, "V3 clips shadows by the sky: {black10} {black50}");
        } else {
            // V1 is frozen with the faults the issue measured
            assert!(ev(m0, m50) < -0.4, "V1 changed: {:.2} EV", ev(m0, m50));
        }
    }
}

#[test]
fn v3_dehaze_still_clears_a_uniform_veil() {
    // a hazy scene: a chequer lifted by a veil
    let hazy = Rgb32f::from_fn(W, H, |x, y| [if (x / 6 + y / 6).is_multiple_of(2) { 0.42 } else { 0.6 }; 3]);
    let contrast = |d: f64| {
        let mut s = DevelopSettings { process: Process::V3.version(), ..DevelopSettings::default() };
        controls::set(&mut s, "effects.dehaze", d);
        let img = render(&hazy, &SourceInfo::default(), &s, &RenderRequest::fit(W, H)).image;
        let (a, b) = (luma(img.data[100 * W + 100]), luma(img.data[100 * W + 106]));
        (a - b).abs()
    };
    let (c0, c50, c100) = (contrast(0.0), contrast(50.0), contrast(100.0));
    assert!(c50 > c0 * 1.3 && c100 > c50, "{c0} {c50} {c100}");
}

#[test]
fn v3_pixel_never_goes_black_and_eases_in() {
    // a shadow (0.05) whose blurred dark channel is the sky's: V1 clipped it to 0 at +10
    let air = 0.8;
    for dz in [0.1, 0.25, 0.5, 1.0] {
        let c = dehaze_v3_pixel([0.05, 0.06, 0.07], dz, 0.9, air);
        assert!(c.iter().all(|v| *v > 0.0), "{dz}: {c:?}");
    }
    let small = dehaze_v3_pixel([0.4; 3], 0.1, 0.5, air)[0];
    let v1_t = 1.0 - 0.95 * 0.1 * 0.5;
    let v1 = (0.4 - air * (1.0 - v1_t)) / v1_t;
    assert!((0.4 - small) < 0.6 * (0.4 - v1), "eases in: {small} vs V1 {v1}");
    assert_eq!(dehaze_v3_pixel([0.3, 0.2, 0.1], 0.0, 0.7, air), [0.3, 0.2, 0.1]);
}

#[test]
fn patch_radius_only_from_v3_and_min_filter_erodes() {
    let s = |p: Process| DevelopSettings { process: p.version(), ..DevelopSettings::default() };
    assert_eq!(local::dehaze_patch_radius(&s(Process::V1), 3000.0), 0);
    assert_eq!(local::dehaze_patch_radius(&s(Process::V2), 3000.0), 0);
    assert_eq!(local::dehaze_patch_radius(&s(Process::V3), 3000.0), 15);
    assert_eq!(local::dehaze_patch_radius(&s(Process::V3), 1e9), local::DEHAZE_PATCH_MAX);
    let p = lightcraft_raster::Plane::from_fn(9, 5, |x, y| if (x, y) == (4, 2) { 0.0 } else { 1.0 });
    let m = local::min_hv(&p, 1);
    assert_eq!(m.get(3, 1), 0.0);
    assert_eq!(m.get(5, 3), 0.0);
    assert_eq!(m.get(6, 2), 1.0);
    assert_eq!(m.get(0, 0), 1.0);
}

#[test]
fn min_filter_matches_brute_force() {
    for (w, h, r) in [(1, 1, 3), (7, 3, 1), (13, 11, 2), (40, 9, 5), (5, 30, 9)] {
        let p = lightcraft_raster::Plane::from_fn(w, h, |x, y| ((x * 37 + y * 61) % 23) as f32);
        let m = local::min_hv(&p, r);
        for y in 0..h {
            for x in 0..w {
                let mut want = f32::INFINITY;
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        want = want.min(p.get(xx, yy));
                    }
                }
                assert_eq!(m.get(x, y), want, "{w}x{h} r {r} at ({x}, {y})");
            }
        }
    }
}
