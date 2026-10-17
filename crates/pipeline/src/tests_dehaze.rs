//! Dehaze from process V3 (issue #1237): haze is estimated from a patch-minimum dark channel, the
//! response eases in, and no pixel is pushed to black; V1 and V2 keep their rendering.

use lcc_develop::{DevelopSettings, Process};

use crate::{finish::dehaze_v3_pixel, local};

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
    let p = lcc_raster::Plane::from_fn(9, 5, |x, y| if (x, y) == (4, 2) { 0.0 } else { 1.0 });
    let m = local::min_hv(&p, 1);
    assert_eq!(m.get(3, 1), 0.0);
    assert_eq!(m.get(5, 3), 0.0);
    assert_eq!(m.get(6, 2), 1.0);
    assert_eq!(m.get(0, 0), 1.0);
}

#[test]
fn min_filter_matches_brute_force() {
    for (w, h, r) in [(1, 1, 3), (7, 3, 1), (13, 11, 2), (40, 9, 5), (5, 30, 9)] {
        let p = lcc_raster::Plane::from_fn(w, h, |x, y| ((x * 37 + y * 61) % 23) as f32);
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
