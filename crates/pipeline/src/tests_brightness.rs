use super::*;

fn k(cf: f64) -> BrightnessK {
    BrightnessK::new(cf).unwrap()
}

/// The slider values the tests walk (Capture One's -50, -25, -10, 10, 25, 50 are these halved).
const CFS: [f64; 9] = [-100.0, -50.0, -20.0, -1.0, 1.0, 20.0, 50.0, 100.0, 37.0];

#[test]
fn no_brightness_has_no_stage() {
    assert!(BrightnessK::new(0.0).is_none());
    assert!(BrightnessK::new(-0.0).is_none());
    assert!(BrightnessK::new(f64::NAN).is_none());
    assert!(BrightnessK::new(f64::INFINITY).is_none());
    assert!(BrightnessK::new(f64::NEG_INFINITY).is_none());
    assert!(BrightnessK::new(0.001).is_some());
    // a damaged value past the slider's reach acts as its end
    assert_eq!(BrightnessK::new(1000.0), BrightnessK::new(100.0));
    assert_eq!(BrightnessK::new(-1000.0), BrightnessK::new(-100.0));
}

#[test]
fn the_slider_is_capture_ones_doubled() {
    // CF +-100 is C1 +-50: phi = 0.02495 * 50, no extrapolation beyond it
    let phi = |cf: f64| f64::from(k(cf).e).ln();
    assert!((phi(100.0) - PHI_PER_UNIT * 50.0).abs() < 1e-6, "{}", phi(100.0));
    assert!((phi(-100.0) + PHI_PER_UNIT * 50.0).abs() < 1e-6);
    assert!((phi(50.0) - PHI_PER_UNIT * 25.0).abs() < 1e-6);
    // a slider value is its own C1 value twice over
    assert!((phi(20.0) - PHI_PER_UNIT * 10.0).abs() < 1e-6);
}

#[test]
fn the_grey_curve_fixes_black_and_white_and_is_monotone() {
    for cf in CFS {
        let b = k(cf);
        for hdr in [false, true] {
            assert_eq!(b.curve(0.0, hdr), 0.0, "cf {cf}");
            assert!((b.curve(1.0, hdr) - 1.0).abs() < 1e-6, "cf {cf}");
        }
        let mut last = -1.0f32;
        for i in 0..=4000 {
            let y = b.curve(i as f32 / 4000.0, false);
            assert!(y.is_finite() && (0.0..=1.0 + 1e-6).contains(&y), "cf {cf}: {y}");
            assert!(y >= last, "cf {cf}, step {i}: {y} after {last}");
            last = y;
        }
    }
}

#[test]
fn positive_lifts_the_midtones_and_negative_darkens_them() {
    for cf in [1.0, 20.0, 100.0] {
        for v in [0.05f32, 0.3, 0.5, 0.8, 0.95] {
            assert!(k(cf).curve(v, false) > v, "cf {cf}, {v}");
            assert!(k(-cf).curve(v, false) < v, "cf -{cf}, {v}");
        }
    }
    // more strength, more change
    for v in [0.2f32, 0.5, 0.8] {
        assert!(k(100.0).curve(v, false) > k(50.0).curve(v, false) && k(50.0).curve(v, false) > k(20.0).curve(v, false));
    }
}

#[test]
fn opposite_values_are_exact_inverses_of_each_other() {
    for cf in [20.0, 50.0, 100.0] {
        for i in 0..=200 {
            let v = i as f32 / 200.0;
            let back = k(-cf).curve(k(cf).curve(v, false), false);
            assert!((back - v).abs() < 2e-6, "cf {cf}, {v}: {back}");
        }
    }
}

#[test]
fn the_curve_is_the_measured_one() {
    // grey values measured on Capture One's 16-bit sRGB exports (input 128 of 255 → output, in 8-bit levels): the
    // fit is good to 0.02 of a level, so a tenth is a safe tolerance for the f32 maths
    for (cf, want) in [(100.0, 198.45), (50.0, 166.48), (20.0, 143.82), (-20.0, 112.16), (-50.0, 89.42), (-100.0, 57.24)] {
        let got = k(cf).curve(128.0 / 255.0, false) * 255.0;
        assert!((f64::from(got) - want).abs() < 0.1, "cf {cf}: {got} against {want}");
    }
    // and a dark and a light step at the strongest values
    for (cf, x, want) in [(100.0, 10.0, 31.73), (100.0, 230.0, 247.28), (-100.0, 10.0, 2.95), (-100.0, 230.0, 184.98)] {
        let got = k(cf).curve(x / 255.0, false) * 255.0;
        assert!((f64::from(got) - want).abs() < 0.1, "cf {cf}, input {x}: {got} against {want}");
    }
}

#[test]
fn colours_follow_the_measured_hybrid() {
    // (input, output) colours measured on the exports (4 decimals)
    let cases: [(f64, [f32; 3], [f32; 3]); 6] = [
        (100.0, [0.6, 0.3, 0.3], [0.8393, 0.5540, 0.5540]),
        (100.0, [0.6045, 0.4420, 0.3250], [0.8418, 0.6913, 0.5829]),
        (100.0, [0.1625, 0.3900, 0.1625], [0.3743, 0.6900, 0.3743]),
        (100.0, [0.45, 0.65, 1.0], [0.6676, 0.7885, 1.0]),
        (-100.0, [0.9, 0.0, 0.0], [0.7210, 0.0, 0.0]),
        (-100.0, [0.6045, 0.4420, 0.3250], [0.3050, 0.2045, 0.1321]),
    ];
    for (cf, u, want) in cases {
        let got = k(cf).apply(u, false);
        for c in 0..3 {
            assert!((got[c] - want[c]).abs() < 1e-3, "cf {cf}, {u:?}: {got:?} against {want:?}");
        }
    }
}

#[test]
fn the_brightest_channel_takes_the_curve_and_hue_and_order_are_kept() {
    let mut seed = 0x9E3779B9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    for cf in CFS {
        let b = k(cf);
        for _ in 0..3000 {
            let u = [next(), next(), next()];
            let y = b.apply(u, false);
            let (mx, mn) = (u[0].max(u[1]).max(u[2]), u[0].min(u[1]).min(u[2]));
            assert!(y.iter().all(|v| v.is_finite() && (0.0..=1.0 + 1e-6).contains(v)), "{u:?} -> {y:?}");
            // the brightest channel is the curve's, and stays the brightest
            let ymx = y[0].max(y[1]).max(y[2]);
            assert!((ymx - b.curve(mx, false)).abs() < 1e-6, "{u:?} -> {y:?}");
            for i in 0..3 {
                for j in 0..3 {
                    if u[i] > u[j] {
                        assert!(y[i] >= y[j] - 1e-6, "order {u:?} -> {y:?}");
                    }
                }
            }
            // hue: each channel keeps its place between the darkest and the brightest
            let (ymn, range, yrange) = (y[0].min(y[1]).min(y[2]), mx - mn, ymx - y[0].min(y[1]).min(y[2]));
            if range > 1e-2 && yrange > 1e-3 {
                for i in 0..3 {
                    let (h, hy) = ((u[i] - mn) / range, (y[i] - ymn) / yrange);
                    assert!((h - hy).abs() < 2e-3, "hue of {u:?} -> {y:?}: channel {i} {h} against {hy}");
                }
            }
        }
    }
}

#[test]
fn greys_black_and_the_primaries() {
    for cf in CFS {
        let b = k(cf);
        // neutrals: all three channels take the grey curve
        for v in [0.0f32, 1e-6, 0.02, 0.5, 0.97, 1.0] {
            let y = b.apply([v; 3], false);
            assert!(y.iter().all(|c| *c == y[0]) && (y[0] - b.curve(v, false)).abs() < 1e-7, "{v}: {y:?}");
        }
        // pure black stays black
        assert_eq!(b.apply([0.0; 3], false), [0.0; 3]);
        // fully saturated primaries keep their zero channels and take the curve on the other
        for (u, i) in [([1.0f32, 0.0, 0.0], 0usize), ([0.0, 0.7, 0.0], 1), ([0.0, 0.0, 0.2], 2)] {
            let y = b.apply(u, false);
            assert!((y[i] - b.curve(u[i], false)).abs() < 1e-7, "{u:?} -> {y:?}");
            assert!(y.iter().enumerate().all(|(c, v)| c == i || *v == 0.0), "{u:?} -> {y:?}");
        }
        // white stays white
        for y in b.apply([1.0; 3], false) {
            assert!((y - 1.0).abs() < 1e-6);
        }
    }
}

#[test]
fn near_black_and_hostile_values_stay_finite() {
    let tiny = f32::MIN_POSITIVE;
    let denormal = tiny / 8.0;
    let cases: [[f32; 3]; 12] = [
        [1e-30, 0.0, 0.0],
        [1e-30, 1e-36, 0.0],
        [tiny, 0.0, tiny],
        [denormal, 0.0, 0.0],
        [denormal, denormal * 2.0, denormal * 3.0],
        [1e-12, 5e-13, 2e-13],
        [-1.0, 0.5, 0.2],
        [f32::NAN, 0.5, 0.2],
        [f32::INFINITY, 0.5, 0.2],
        [f32::NEG_INFINITY, f32::NAN, f32::INFINITY],
        [1.0e30, 1.0, 0.5],
        [2.0, 1.5, 0.0],
    ];
    for cf in CFS {
        let b = k(cf);
        for hdr in [false, true] {
            for u in cases {
                let y = b.apply(u, hdr);
                assert!(y.iter().all(|v| v.is_finite() && *v >= 0.0), "cf {cf}, hdr {hdr}: {u:?} -> {y:?}");
                if !hdr {
                    assert!(y.iter().all(|v| *v <= 1.0 + 1e-6), "{u:?} -> {y:?}");
                }
            }
        }
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 1e30] {
            assert!(b.curve(v, false).is_finite() && b.curve(v, true).is_finite(), "{v}");
        }
    }
}

/// An HDR render's stage for the peaks (linear, relative to SDR white) of 1, 2 and 3 stops of headroom.
const PEAKS: [f32; 4] = [2.0, 4.0, 8.0, 32.0]; // 1, 2, 3 and 5 stops (5 is the largest headroom: H = 4.42)

fn kp(cf: f64, peak: f32) -> BrightnessK {
    BrightnessK::for_peak(cf, peak).unwrap()
}

#[test]
fn the_headroom_is_the_encoded_peak_and_an_sdr_render_has_none() {
    assert_eq!(BrightnessK::for_peak(25.0, 1.0).unwrap(), BrightnessK::new(25.0).unwrap());
    assert_eq!(BrightnessK::for_peak(25.0, f32::NAN).unwrap(), BrightnessK::new(25.0).unwrap());
    assert!(BrightnessK::for_peak(0.0, 4.0).is_none());
    for peak in PEAKS {
        let b = kp(25.0, peak);
        assert!((b.h - lcc_color::transfer::linear_to_srgb(peak)).abs() < 1e-6 && b.h > 1.0, "{}", b.h);
    }
}

#[test]
fn at_or_below_white_an_hdr_render_is_exactly_the_sdr_render() {
    // (the separate SDR rendition of an HDR edit and the HDR render must agree there)
    for cf in CFS {
        for peak in PEAKS {
            let (b, sdr) = (kp(cf, peak), k(cf));
            for i in 0..=2000 {
                let v = i as f32 / 2000.0;
                assert_eq!(b.curve(v, true), sdr.curve(v, false), "cf {cf}, peak {peak}, {v}");
            }
            for u in [[0.9, 0.5, 0.1], [1.0, 1.0, 0.2], [0.3, 0.3, 0.3], [1.0, 0.6, 0.6], [0.0, 0.0, 0.8]] {
                assert_eq!(b.apply(u, true), sdr.apply(u, false), "cf {cf}, peak {peak}, {u:?}");
            }
        }
    }
}

#[test]
fn above_white_the_same_curve_is_rescaled_onto_white_to_the_headroom() {
    for cf in CFS {
        for peak in PEAKS {
            let b = kp(cf, peak);
            let h = b.h;
            // white, the headroom and everything past it: the ends are fixed, values above the headroom pass through
            assert_eq!(b.curve(1.0, true), 1.0);
            assert!((b.curve(h, true) - h).abs() < 1e-5, "cf {cf}: {}", b.curve(h, true));
            assert_eq!(b.curve(h * 1.5, true), h * 1.5);
            // the direction of the slider holds on both sides of white (positive lifts, negative darkens), monotone, continuous
            let mut last = 0.0f32;
            for i in 0..=((h * 1000.0) as u32) {
                let v = i as f32 / 1000.0;
                let y = b.curve(v, true);
                assert!(y >= last, "cf {cf}, peak {peak}: not monotone at {v}");
                last = y;
                if v > 0.0 && v < h && (v - 1.0).abs() > 1e-3 && (v - h).abs() > 1e-3 {
                    assert!(if cf > 0.0 { y > v } else { y < v }, "cf {cf}, peak {peak}: wrong direction at {v}: {y}");
                }
            }
            let d = 1e-4f32;
            assert!((b.curve(1.0 + d, true) - 1.0).abs() < 4.0 * d * (1.0 + b.e + 1.0 / b.e), "cf {cf}: value jump at white");
            assert!((b.curve(h - d, true) - h).abs() < 4.0 * d * (1.0 + b.e + 1.0 / b.e), "cf {cf}: value jump at the headroom");
            // an SDR render still holds 1
            assert_eq!(b.curve(3.0, false), 1.0);
        }
    }
}

#[test]
fn the_slope_at_white_jumps_from_1_over_e_to_e_so_by_e_squared() {
    // (the kink the visual test is about; the ratio does not depend on the headroom)
    for cf in CFS {
        for peak in PEAKS {
            let b = kp(cf, peak);
            let d = 1e-4f32;
            let below = (1.0 - b.curve(1.0 - d, true)) / d;
            let above = (b.curve(1.0 + d, true) - 1.0) / d;
            assert!((below - 1.0 / b.e).abs() < 4e-3 * (1.0 + 1.0 / b.e), "cf {cf}: slope below {below}, 1/E {}", 1.0 / b.e);
            assert!((above - b.e).abs() < 4e-3 * (1.0 + b.e), "cf {cf}, peak {peak}: slope above {above}, E {}", b.e);
            let ratio = above / below;
            assert!((ratio / (b.e * b.e) - 1.0).abs() < 1e-2, "cf {cf}: ratio {ratio}, E² {}", b.e * b.e);
        }
    }
}

#[test]
fn colours_above_white_keep_their_order_and_hue_and_go_the_slider_s_way() {
    for cf in [-100.0, -50.0, 50.0, 100.0] {
        let b = kp(cf, 4.0);
        for u in [[1.8, 1.4, 1.1], [1.5, 1.2, 0.3], [1.3, 0.9, 0.9], [1.7, 1.7, 1.2]] {
            let y = b.apply(u, true);
            assert!(y[0] >= y[1] && y[1] >= y[2], "cf {cf}: {u:?} -> {y:?}");
            assert!(y.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= b.h + 1e-4), "cf {cf}: {u:?} -> {y:?}");
            assert_eq!(y[0] > u[0], cf > 0.0, "cf {cf}: the brightest channel goes the slider's way: {u:?} -> {y:?}");
        }
        // black and the headroom are fixed
        assert_eq!(b.apply([0.0; 3], true), [0.0; 3]);
        let top = b.apply([b.h; 3], true);
        assert!(top.iter().all(|v| (v - b.h).abs() < 1e-5), "{top:?}");
    }
}

#[test]
fn saturated_highlights_cross_white_without_hue_flips_nans_or_saturation_jumps() {
    // A saturated colour brightened step by step (exposure sweep) so its brightest channel, then the others, cross SDR white.
    for cf in CFS {
        for peak in PEAKS {
            let b = kp(cf, peak);
            for base in [[1.0f32, 0.35, 0.1], [0.2, 1.0, 0.05], [0.1, 0.4, 1.0], [1.0, 0.9, 0.05], [1.0, 0.5, 0.5], [1.0, 0.0, 0.0]] {
                let mut last: Option<([f32; 3], f32)> = None;
                for i in 0..=1200 {
                    let s = 0.3 + i as f32 * 0.002; // 0.3 .. 2.7 times
                    let u = base.map(|v| v * s);
                    let y = b.apply(u, true);
                    assert!(y.iter().all(|v| v.is_finite() && *v >= 0.0), "cf {cf}, {base:?} x {s}: {y:?}");
                    // order of the channels (so no hue flip) and the place of the middle one between the darkest and the brightest
                    let (ui, yi) = (order(u), order(y));
                    let (mx, mn) = (y[yi[0]], y[yi[2]]);
                    assert!(y[yi[0]] >= y[yi[1]] && y[yi[1]] >= y[yi[2]] && ui == yi, "cf {cf}, {base:?} x {s}: order {u:?} -> {y:?}");
                    let (umx, umn) = (u[ui[0]], u[ui[2]]);
                    if umx - umn > 1e-3 && mx - mn > 1e-3 {
                        let (tu, ty) = ((u[ui[1]] - umn) / (umx - umn), (y[yi[1]] - mn) / (mx - mn));
                        assert!((tu - ty).abs() < 2e-3, "cf {cf}, {base:?} x {s}: hue position {tu} -> {ty}");
                    }
                    let sat = if mx > 0.0 { 1.0 - mn / mx } else { 0.0 };
                    if let Some((ly, ls)) = last {
                        // monotone in exposure, channel by channel, and the saturation moves smoothly across white
                        for c in 0..3 {
                            assert!(y[c] + 1e-5 >= ly[c], "cf {cf}, {base:?} x {s}: channel {c} falls {} -> {}", ly[c], y[c]);
                        }
                        assert!((sat - ls).abs() < 0.02, "cf {cf}, {base:?} x {s}: saturation jumps {ls} -> {sat}");
                    }
                    last = Some((y, sat));
                }
            }
        }
    }
}

/// The channel indices from the brightest to the darkest (ties keep channel order).
fn order(c: [f32; 3]) -> [usize; 3] {
    let mut i = [0usize, 1, 2];
    i.sort_by(|&a, &b| c[b].partial_cmp(&c[a]).unwrap().then(a.cmp(&b)));
    i
}

// ---- in a render

mod render_level {
    use crate::{OutputDepth, RenderRequest, SourceInfo, render};
    use lcc_develop::DevelopSettings;
    use lcc_raster::Rgb32f;

    /// `colours` (sRGB-encoded) as a scene-linear Rec.2020 image, one row.
    fn image(colours: &[[f32; 3]]) -> Rgb32f {
        let m = lcc_color::SRGB.to_space(&lcc_color::REC2020);
        let mut img = Rgb32f::new(colours.len(), 1);
        for (px, c) in img.data.iter_mut().zip(colours) {
            *px = m.apply_f32(c.map(lcc_color::transfer::srgb_to_linear));
        }
        img
    }

    fn colours() -> Vec<[f32; 3]> {
        let mut v: Vec<[f32; 3]> = (0..=64).map(|i| [i as f32 / 64.0; 3]).collect();
        v.extend([[0.6, 0.3, 0.3], [0.2, 0.5, 0.8], [1.0, 0.0, 0.0], [0.0, 0.7, 0.0], [0.9, 0.8, 0.1]]);
        v
    }

    fn with(b: f64) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.light.brightness = b;
        s
    }

    #[test]
    fn brightness_zero_renders_byte_for_byte_as_before_at_every_depth() {
        let cs = colours();
        let src = image(&cs);
        let info = SourceInfo::default();
        for depth in [OutputDepth::U8, OutputDepth::U16, OutputDepth::F32Linear, OutputDepth::F32Hdr] {
            let req = RenderRequest { depth, ..RenderRequest::fit(cs.len(), 1) };
            let plain = render(&src, &info, &DevelopSettings::default(), &req);
            for b in [0.0, -0.0, f64::NAN] {
                let r = render(&src, &info, &with(b), &req);
                assert_eq!(r.image.data, plain.image.data, "{depth:?}, brightness {b}");
                assert_eq!(format!("{:?}", r.deep.map(|d| d.samples)), format!("{:?}", plain.deep.as_ref().map(|d| &d.samples)), "{depth:?}");
            }
        }
        // the stage does not exist, so nothing of it reaches the finish parameters
        assert!(super::BrightnessK::new(DevelopSettings::default().light.brightness).is_none());
    }

    /// An 8-bit render pixel through the stage (as the render would have it afterwards), in 8-bit levels.
    fn through(p: [u8; 3], cf: f64) -> [f32; 3] {
        let k = super::BrightnessK::new(cf).unwrap();
        k.apply(p.map(|v| f32::from(v) / 255.0), false).map(|v| v * 255.0)
    }

    fn worst(a: &[[u8; 3]], b: &[[f32; 3]]) -> f32 {
        a.iter().zip(b).flat_map(|(x, y)| (0..3).map(move |c| (f32::from(x[c]) - y[c]).abs())).fold(0.0, f32::max)
    }

    fn rgb(img: &lcc_raster::Rgba8) -> Vec<[u8; 3]> {
        img.data.iter().map(|p| [p[0], p[1], p[2]]).collect()
    }

    /// The order of the finish stages, pinned: Capture One applies Exposure, Contrast, Saturation, then Brightness, then
    /// Levels (not in this branch; whatever sits next to Brightness there goes AFTER it, with the Tone Curve behind it).
    /// A rebase that moves the call must fail here.
    #[test]
    fn brightness_runs_after_contrast_and_saturation_and_before_the_tone_curve() {
        let cs = colours();
        let src = image(&cs);
        let info = SourceInfo::default();
        let req = RenderRequest::fit(cs.len(), 1);
        let px = |s: &DevelopSettings, img: &Rgb32f| rgb(&render(img, &info, s, &req).image);
        const CF: f64 = 80.0;

        // after Contrast and Saturation: the render with them, put through the stage, is the render with all three
        // (V9: the order is rebuilt here from 8-bit output values, which is where V1–V9 run the stage)
        let v9 = || DevelopSettings { process: lcc_develop::Process::V9.version(), ..DevelopSettings::default() };
        let mut earlier = v9();
        earlier.light.contrast = 45.0;
        earlier.color.saturation = 45.0;
        let mut all = earlier.clone();
        all.light.brightness = CF;
        let got = px(&all, &src);
        let want: Vec<[f32; 3]> = px(&earlier, &src).into_iter().map(|p| through(p, CF)).collect();
        assert!(worst(&got, &want) <= 2.0, "Brightness is not after Contrast and Saturation: {}", worst(&got, &want));
        // (the test can tell the orders apart: Brightness first would be elsewhere)
        let swapped: Vec<[f32; 3]> = px(&v9(), &src).into_iter().map(|p| through(p, CF)).collect();
        let swapped = px(&earlier, &image(&swapped.iter().map(|p| p.map(|v| v / 255.0)).collect::<Vec<_>>()));
        assert!(worst(&got, &swapped.iter().map(|p| p.map(f32::from)).collect::<Vec<_>>()) > 4.0, "the orders are too alike to tell apart");

        // before the Tone Curve: the stage's output is what the curve gets
        let mut curve = v9();
        curve.curve.master = vec![lcc_geom::Point::new(0.0, 0.0), lcc_geom::Point::new(0.5, 0.3), lcc_geom::Point::new(1.0, 1.0)];
        let mut both = curve.clone();
        both.light.brightness = CF;
        let got = px(&both, &src);
        let lifted: Vec<[f32; 3]> = cs.iter().map(|c| super::BrightnessK::new(CF).unwrap().apply(*c, false)).collect();
        let want = px(&curve, &image(&lifted));
        let want: Vec<[f32; 3]> = want.iter().map(|p| p.map(f32::from)).collect();
        assert!(worst(&got, &want) <= 2.0, "Brightness is not before the Tone Curve: {}", worst(&got, &want));
        let after: Vec<[f32; 3]> = px(&curve, &src).into_iter().map(|p| through(p, CF)).collect();
        assert!(worst(&got, &after) > 4.0, "the orders are too alike to tell apart");
    }

    /// With Levels in the same render: Capture One's order is Brightness, then Levels. Both orders are computed from the plain
    /// render and the one that matches the render is the Brightness-first one; the other is far enough away to tell.
    #[test]
    fn brightness_runs_before_levels() {
        use crate::levels::LevelsK;
        use lcc_develop::{ChannelLevels, Levels};
        let cs = colours();
        let src = image(&cs);
        let info = SourceInfo::default();
        let req = RenderRequest::fit(cs.len(), 1);
        let levels =
            Levels { master: ChannelLevels { black: 30.0, midtone: 0.0, white: 225.0, out_black: 10.0, out_white: 245.0 }, ..Default::default() };
        let lv = LevelsK::new(&levels).expect("a stage");
        let b = super::BrightnessK::new(80.0).expect("a stage");
        // (V9: rebuilt from 8-bit output values, where V1–V9 run both stages)
        let v9 = lcc_develop::Process::V9.version();
        let plain = rgb(&render(&src, &info, &DevelopSettings { process: v9, ..Default::default() }, &req).image);
        let unit = |p: &[u8; 3]| p.map(|v| f32::from(v) / 255.0);
        let levels_first: Vec<[f32; 3]> = plain.iter().map(|p| b.apply(lv.apply(unit(p), false), false).map(|v| v * 255.0)).collect();
        let brightness_first: Vec<[f32; 3]> = plain.iter().map(|p| lv.apply(b.apply(unit(p), false), false).map(|v| v * 255.0)).collect();
        let mut s = DevelopSettings { levels, process: v9, ..Default::default() };
        s.light.brightness = 80.0;
        let got = rgb(&render(&src, &info, &s, &req).image);
        assert!(worst(&got, &brightness_first) <= 2.0, "Brightness is not before Levels: {}", worst(&got, &brightness_first));
        assert!(worst(&got, &levels_first) > 4.0, "the orders are too alike to tell apart");
    }

    /// The call sites, as text, so a rebase that swaps the stage with a neighbour is caught on both renderers: up to
    /// process V9 on the output values after the encode, from V10 in the wide curve space before the gamut map; in both
    /// Brightness, then Levels, then the Tone Curve.
    #[test]
    fn the_stage_is_called_right_before_the_tone_curve_on_the_cpu_and_the_gpu() {
        let find_from = |src: &str, from: usize, pat: &str| src.get(from..).and_then(|t| t.find(pat)).map(|i| i + from);
        let cpu = include_str!("finish.rs");
        let encode = cpu.find("r.map(|v| encode_srgb(srgb, v))").expect("the encode");
        let b = find_from(cpu, encode, "b.apply(e, hdr)").expect("the CPU call");
        let c = find_from(cpu, encode, "eval_curves(l, e, hdr)").expect("the CPU curve call");
        assert!(b < c, "CPU: Brightness must run before the Tone Curve");
        {
            let l = levels_call(cpu, encode, "fp.levels.as_ref()", "finish.rs").expect("the Levels call in finish.rs");
            assert!(b < l, "CPU: Levels must come after Brightness");
        }
        // process V10: the same order inside the wide space
        let wide = cpu.find("d = cs.apply(d, |mut e| {").expect("the V10 call");
        let wb = find_from(cpu, wide, "e = b.apply(e, hdr)").expect("V10 Brightness");
        let wl = find_from(cpu, wide, "e = lv.apply(e, hdr)").expect("V10 Levels");
        let wc = find_from(cpu, wide, "e = eval_curves(l, e, true)").expect("V10 curve");
        assert!(wb < wl && wl < wc && wc < encode, "CPU V10: Brightness, Levels, then the Tone Curve, before the encode");
        // the shader, the same
        let gpu = include_str!("../../gpu/src/wgsl/finish.wgsl");
        let helper = gpu.find("fn brightness_levels(").expect("the GPU helper");
        let hb = find_from(gpu, helper, "brightness_apply(e, hdr)").expect("the GPU Brightness call");
        let hl = find_from(gpu, helper, "pu(F_LEVELS)").expect("the GPU Levels call");
        assert!(hb < hl, "GPU: Levels must come after Brightness");
        let encode = gpu.find("encode_srgb(r.x)").expect("the GPU encode");
        let b = find_from(gpu, encode, "brightness_levels(e, hdr)").expect("the GPU call");
        let c = find_from(gpu, encode, "pu(F_CURVES)").expect("the GPU curve block");
        assert!(b < c, "GPU: Brightness must run before the Tone Curve");
        let wide = gpu.find("if (curve_wide) {").expect("the GPU V10 block");
        let wb = find_from(gpu, wide, "brightness_levels(pow(").expect("GPU V10 Brightness");
        let wc = find_from(gpu, wide, "curve(0u, e0.x, true)").expect("GPU V10 curve");
        assert!(wb < wc && wc < encode, "GPU V10: Brightness and Levels, then the Tone Curve, before the encode");
    }

    fn levels_call(src: &str, from: usize, marker: &str, file: &str) -> Option<usize> {
        let src = src.split("\n#[cfg(test)]").next().unwrap_or(src); // (a file's own tests are not stages)
        let mut at = 0;
        for line in src.split_inclusive('\n') {
            let start = at;
            at += line.len();
            if start < from {
                continue;
            }
            let code = line.split("//").next().unwrap_or("");
            if code.to_lowercase().contains("level") {
                let m = code.find(marker);
                assert!(
                    m.is_some(),
                    "{file}: a Levels-like stage `{}` this test does not recognise (expected `{marker}`): update the test with its real name",
                    code.trim()
                );
                return Some(start + m.unwrap_or(0));
            }
        }
        None
    }

    #[test]
    fn a_render_lifts_the_midtones_and_keeps_black_and_white() {
        let cs = colours();
        let src = image(&cs);
        let info = SourceInfo::default();
        let req = RenderRequest::fit(cs.len(), 1);
        let plain = render(&src, &info, &DevelopSettings::default(), &req).image.data;
        for (cf, mid_want) in [(100.0, 198.0), (-100.0, 57.0)] {
            let out = render(&src, &info, &with(cf), &req).image.data;
            // grey ramp: black and white are fixed (also against the unedited render), the middle moves as measured
            assert_eq!(out[0], plain[0], "black, cf {cf}");
            // (the pipeline's own white is 254.95 of 255; the steep end slope of a negative value scales that small deficit, which the
            // 8-bit dither can show as one level)
            assert!(out[64][0].abs_diff(plain[64][0]) <= 1, "white, cf {cf}: {:?} against {:?}", out[64], plain[64]);
            assert!((f64::from(out[32][0]) - mid_want).abs() <= 2.0, "cf {cf}: mid grey {} (want about {mid_want})", out[32][0]);
            assert!(out[1..64].windows(2).all(|w| w[1][0] >= w[0][0]), "the ramp stays monotone, cf {cf}");
            // neutrals stay neutral
            assert!(out[..65].iter().all(|p| p[0] == p[1] && p[1] == p[2]), "cf {cf}");
        }
    }
}
