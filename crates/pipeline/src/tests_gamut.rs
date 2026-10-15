//! Bright saturated colours keep their saturation from process V3 (`finish::GamutMap`): an orange
//! past the output gamut or past white is not bleached towards white by lifting its lowest
//! channel, while clipped neutrals still end at white. V1 and V2 keep the desaturating map.

use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::finish::{GamutMap, gamut_map_keep_to, gamut_map_to};
use crate::{OutputSpace, RenderRequest, SourceInfo, render};

const SRGB_Y: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn lum(r: [f32; 3]) -> f32 {
    SRGB_Y[0] * r[0] + SRGB_Y[1] * r[1] + SRGB_Y[2] * r[2]
}

/// The halo of an EOS R10 sunset in linear sRGB before the output map (red past white).
const HALO: [f32; 3] = [1.125, 0.275, 0.027];

#[test]
fn an_orange_past_white_keeps_its_blue_and_its_luminance() {
    let (q, t) = gamut_map_keep_to(HALO, SRGB_Y, 1.0);
    assert!((q[0] - 1.0).abs() < 1e-6, "{q:?}");
    assert!((q[2] - HALO[2]).abs() < 1e-6, "the lowest channel keeps its value: {q:?}");
    assert!((lum(q) - lum(HALO)).abs() < 1e-5, "luminance {} -> {}", lum(HALO), lum(q));
    assert!(q[1] > HALO[1] && q[1] < 1.0, "green carries the clipped luminance: {q:?}");
    assert!(t < 0.995, "flagged for the gamut warnings: {t}");
    // the old map keeps the hue by lifting blue almost four-fold (the pale halo)
    let (old, _) = gamut_map_to(HALO, SRGB_Y, 1.0);
    assert!(old[2] > 3.5 * HALO[2], "{old:?}");
}

#[test]
fn in_gamut_colours_are_untouched_and_neutrals_end_at_white() {
    for c in [[0.5, 0.3, 0.1], [1.0, 1.0, 1.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.2, 0.9, 0.4]] {
        assert_eq!(gamut_map_keep_to(c, SRGB_Y, 1.0), (c, 1.0));
    }
    for c in [[1.5; 3], [4.0; 3], [1.4, 1.3, 1.2], [1.2, 1.0, 0.9], [3.0, 2.0, 0.5]] {
        let (q, _) = gamut_map_keep_to(c, SRGB_Y, 1.0);
        assert!(q.iter().all(|v| (v - 1.0).abs() < 1e-5), "{c:?} -> {q:?}");
    }
    // HDR: into 0..peak
    let c = [5.0, 1.2, 0.2];
    let (q, _) = gamut_map_keep_to(c, SRGB_Y, 4.0);
    assert!(q.iter().all(|v| (0.0..=4.0 + 1e-6).contains(v)) && (q[2] - 0.2).abs() < 1e-6, "{q:?}");
    assert!((lum(q) - lum(c)).abs() < 1e-4, "{q:?}");
    // below zero: the least desaturation that brings it to 0, as before
    let c = [0.6, 0.3, -0.05];
    assert_eq!(gamut_map_keep_to(c, SRGB_Y, 1.0), gamut_map_to(c, SRGB_Y, 1.0));
    let (q, _) = gamut_map_keep_to([1.3, 0.4, -0.05], SRGB_Y, 1.0);
    assert!(q.iter().all(|v| (-1e-6..=1.0 + 1e-6).contains(v)), "{q:?}");
    // never NaN
    for c in [[f32::NAN, 0.5, 0.5], [f32::INFINITY, 0.0, 0.0], [1e30, 1e30, -1e30]] {
        let (q, _) = gamut_map_keep_to(c, SRGB_Y, 1.0);
        assert!(q.iter().all(|v| !v.is_nan()) || c.iter().any(|v| v.is_nan()), "{c:?} -> {q:?}");
    }
}

#[test]
fn the_map_is_continuous_as_a_colour_brightens_to_white() {
    // an orange, then a red (middle channel near the lowest: the roll-off to white takes over)
    for base in [[0.9, 0.35, 0.03], [0.9, 0.08, 0.05], [0.9, 0.05, 0.05], [0.3, 0.9, 0.1]] {
        let mut prev = gamut_map_keep_to(base, SRGB_Y, 1.0).0;
        for i in 1..=4000 {
            let g = 1.0 + i as f32 * 0.001;
            let q = gamut_map_keep_to(base.map(|v| v * g), SRGB_Y, 1.0).0;
            let step = (0..3).map(|k| (q[k] - prev[k]).abs()).fold(0.0, f32::max);
            assert!(step < 0.01, "{base:?} × {g}: {prev:?} -> {q:?}");
            assert!(lum(q) + 1e-5 >= lum(prev), "brighter stays brighter: {base:?} × {g}");
            prev = q;
        }
        assert!(prev.iter().all(|v| *v > 0.999), "{base:?} ends at white: {prev:?}");
    }
}

const W: usize = 64;

/// Linear Rec. 2020: a saturated orange past sRGB (red above 1, blue just below 0) on the left,
/// an orange whose red passes white already in Rec. 2020 in the middle (luminance below white), a
/// clipped neutral on the right.
fn scene() -> Rgb32f {
    Rgb32f::from_fn(W, 8, |x, _| match x * 3 / W {
        0 => [0.85, 0.35, 0.05],
        1 => [1.5, 0.5, 0.05],
        _ => [4.0; 3],
    })
}

fn shot(process: Process) -> Rgba8 {
    let s = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
    render(&scene(), &SourceInfo::default(), &s, &RenderRequest { space: OutputSpace::Srgb, ..RenderRequest::fit(W, 8) }).image
}

fn at(img: &Rgba8, x: usize) -> [u8; 4] {
    img.data[4 * W + x]
}

#[test]
fn v3_renders_a_bright_orange_orange_and_v2_pale() {
    assert_eq!(GamutMap::of(&DevelopSettings::default()), GamutMap::KeepSaturation);
    let (v2, v3) = (shot(Process::V2), shot(Process::V3));
    assert!(shot(Process::V1) == v2, "V1 and V2 share the desaturating map");
    let (o2, o3) = (at(&v2, 10), at(&v3, 10));
    assert_eq!((o3[0], o2[0]), (255, 255));
    assert!(o3[2] < 20 && o2[2] > 60, "blue: V3 {o3:?}, V2 {o2:?}");
    assert!(o3[1].abs_diff(o2[1]) < 12, "green (luminance) about the same: V3 {o3:?}, V2 {o2:?}");
    // past white in Rec. 2020 too, the orange stays saturated
    let (b2, b3) = (at(&v2, 32), at(&v3, 32));
    assert!(b3[0] == 255 && b3[1] > b3[2] && b3[2] < 90 && b2[2] > b3[2] + 100, "V3 {b3:?}, V2 {b2:?}");
    // the clipped neutral is white on both
    assert_eq!((at(&v3, 60), at(&v2, 60)), ([255; 4], [255; 4]));
}
