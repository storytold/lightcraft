//! The plausibility check on synthetic Bayer frames: a flat field passes; detail, colour, a light from one side and
//! a frame without fall-off fail; a lopsided clean field passes with a note; a faint spot is Borderline.

use super::*;
use crate::Cfa;

const W: usize = 960;
const H: usize = 640;

/// EV below the brightest point at `(x, y)` for a lens's radial fall-off centred at `(cx, cy)` (fractions of the frame).
fn radial(x: f64, y: f64, cx: f64, cy: f64) -> f64 {
    let (dx, dy) = (x / W as f64 - cx, (y / H as f64 - cy) * H as f64 / W as f64);
    let d2 = (dx * dx + dy * dy) / (0.25 + 0.25 * (H as f64 / W as f64).powi(2));
    1.1 * d2 * d2 + 0.25 * d2
}

/// A frame at `level` (of white) in the middle, darker by `ev(x, y)`, each colour scaled by `tint(x, y)` (R, G, B), with
/// a little noise.
fn frame(level: f64, ev: impl Fn(f64, f64) -> f64, tint: impl Fn(f64, f64) -> [f64; 3]) -> Normalized {
    let cfa = Cfa::bayer("RGGB");
    let mut state = 12345u64;
    let data = (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = ((state >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * 0.004;
            let c = cfa.as_ref().map_or(1, |c| match c.color_at(x, y) {
                0 => 0,
                2 => 2,
                _ => 1,
            });
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            (level * (-ev(fx, fy)).exp2() * tint(fx, fy)[c] + noise) as f32
        })
        .collect();
    Normalized { width: W, height: H, cpp: 1, data, cfa }
}

fn even(_: f64, _: f64) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

fn lens(x: f64, y: f64) -> f64 {
    radial(x, y, 0.5, 0.5)
}

fn verdict(n: &Normalized) -> (Verdict, Vec<Reason>) {
    let p = check(n).expect("measured");
    (p.verdict, p.reasons)
}

#[test]
fn a_flat_field_passes_with_room_to_spare() {
    let p = check(&frame(0.4, lens, even)).expect("measured");
    assert_eq!(p.verdict, Verdict::Pass, "{p:?}");
    let m = &p.metrics;
    assert!(m.detail_max < DETAIL_MAX.borderline / 2.0 && m.colour < COLOUR.borderline / 2.0, "{m:?}");
    assert!(m.peak_offset < 0.05 && m.rising_rings == 0.0 && m.falloff > 1.0, "{m:?}");
}

#[test]
fn something_in_the_frame_fails_on_detail() {
    // a dark object (a bird against a sky) of 30 × 20 px
    let bird = |x: f64, y: f64| lens(x, y) + if (600.0..630.0).contains(&x) && (200.0..220.0).contains(&y) { 2.0 } else { 0.0 };
    assert_eq!(verdict(&frame(0.4, bird, even)), (Verdict::Fail, vec![Reason::Detail]));
    // texture all over (a wall, foliage)
    let texture = |x: f64, y: f64| lens(x, y) + 0.3 * ((x / 13.0).sin() * (y / 17.0).cos()).abs();
    assert_eq!(verdict(&frame(0.4, texture, even)).0, Verdict::Fail);
}

#[test]
fn colours_across_the_frame_fail_on_colour() {
    // as bright everywhere (green is even), but red and blue patches all over (flowers in a meadow): the colour measure
    // judges the spread over the frame, as tuned on real photos
    let patches = |x: f64, y: f64| {
        let k = ((x / 40.0).floor() + (y / 40.0).floor()) as i64;
        if k % 2 == 0 { [1.3, 1.0, 0.8] } else { [0.8, 1.0, 1.3] }
    };
    let (v, reasons) = verdict(&frame(0.4, lens, patches));
    assert_eq!(v, Verdict::Fail);
    assert!(reasons.contains(&Reason::Colour), "{reasons:?}");
    // a lens's smooth colour shading is not colour structure
    let shading = |x: f64, y: f64| {
        let r = lens(x, y);
        [1.0 - 0.05 * r, 1.0, 1.0 + 0.04 * r]
    };
    assert_eq!(verdict(&frame(0.4, lens, shading)).0, Verdict::Pass);
}

#[test]
fn light_from_one_side_or_no_falloff_fails_on_shape() {
    // brighter toward the right edge: the brightest area is far from the middle
    let side = |x: f64, _: f64| 1.5 * (1.0 - x / W as f64);
    let (v, reasons) = verdict(&frame(0.4, side, even));
    assert_eq!(v, Verdict::Fail);
    assert!(reasons.contains(&Reason::Shape), "{reasons:?}");
    // perfectly even: no lens fall-off at all
    let (v, reasons) = verdict(&frame(0.4, |_, _| 0.0, even));
    assert_eq!(v, Verdict::Fail);
    assert!(reasons.contains(&Reason::NoFalloff), "{reasons:?}");
}

#[test]
fn a_lopsided_clean_field_passes_with_a_note_and_a_faint_spot_asks_first() {
    // a fall-off steeper up and down than across (a sky, a light from above and below): clean, but not radial
    let sky = |x: f64, y: f64| radial(x, 0.5 * H as f64 + (y - 0.5 * H as f64) * 1.2, 0.5, 0.5);
    let p = check(&frame(0.4, sky, even)).expect("measured");
    assert_eq!(p.verdict, Verdict::PassWithNote, "{p:?}");
    // a faint spot (dust on a screen): above the flat fields' range, below what photos show
    let dust = |x: f64, y: f64| lens(x, y) + if (300.0..330.0).contains(&x) && (300.0..320.0).contains(&y) { 0.08 } else { 0.0 };
    assert_eq!(verdict(&frame(0.4, dust, even)), (Verdict::Borderline, vec![Reason::Detail]));
}

#[test]
fn a_dark_or_clipped_frame_fails_and_an_empty_one_is_not_measured() {
    assert!(verdict(&frame(0.005, lens, even)).1.contains(&Reason::Dark));
    assert!(verdict(&frame(1.5, lens, even)).1.contains(&Reason::Clipped));
    assert!(check(&Normalized { width: 0, height: 0, cpp: 1, data: vec![], cfa: None }).is_none());
}

/// The judge alone: each limit's two sides.
#[test]
fn the_limits_split_pass_borderline_and_fail() {
    let ok = Metrics { level: 0.3, falloff: 1.0, fit_rms: 0.01, detail_max: 0.01, detail_p99: 0.003, colour: 0.001, ..Default::default() };
    assert_eq!(Plausibility::judge(ok.clone()).verdict, Verdict::Pass);
    let at = |f: &dyn Fn(&mut Metrics)| {
        let mut m = ok.clone();
        f(&mut m);
        Plausibility::judge(m).verdict
    };
    assert_eq!(at(&|m| m.detail_max = 0.06), Verdict::Borderline);
    assert_eq!(at(&|m| m.detail_max = 0.13), Verdict::Fail);
    assert_eq!(at(&|m| m.colour = 0.005), Verdict::Borderline);
    assert_eq!(at(&|m| m.peak_offset = 0.3), Verdict::Fail);
    assert_eq!(at(&|m| m.rising_rings = 0.05), Verdict::Borderline);
    assert_eq!(at(&|m| m.fit_rms = 0.05), Verdict::PassWithNote);
    assert_eq!(at(&|m| m.fit_rms = f64::NAN), Verdict::Fail);
    assert_eq!(at(&|m| m.level = 0.01), Verdict::Fail);
}
