use super::*;
use lcc_develop::{ChannelLevels, DevelopSettings, Levels};

fn ch(black: f64, midtone: f64, white: f64, out_black: f64, out_white: f64) -> ChannelLevels {
    ChannelLevels { black, midtone, white, out_black, out_white }
}

fn master(c: ChannelLevels) -> Levels {
    Levels { master: c, ..Default::default() }
}

#[test]
fn neutral_levels_have_no_stage() {
    assert!(LevelsK::new(&Levels::default()).is_none());
    assert!(LevelsK::new(&DevelopSettings::default().levels).is_none());
    assert!(StageK::new(&ChannelLevels::default()).is_none());
    // one channel at work makes a stage for it alone
    let k = LevelsK::new(&Levels { green: ch(10.0, 0.0, 255.0, 0.0, 255.0), ..Default::default() }).unwrap();
    assert!(k.master.is_none() && k.channels[0].is_none() && k.channels[2].is_none() && k.channels[1].is_some());
}

#[test]
fn black_and_white_points_stretch_linearly_and_the_ends_are_limited() {
    let k = LevelsK::new(&master(ch(51.0, 0.0, 204.0, 0.0, 255.0))).unwrap();
    for (x, want) in [(0.0, 0.0), (0.2, 0.0), (0.5, 0.5), (0.8, 1.0), (1.0, 1.0), (0.35, 0.25)] {
        let y = k.apply([x; 3], false);
        assert!(y.iter().all(|v| (v - want).abs() < 1e-5), "{x} -> {y:?}, want {want}");
    }
}

#[test]
fn the_stretch_is_not_limited_before_the_output_levels() {
    // black 20, white 240, output 5 / 250: below the black point the value goes on down past the output black to 0,
    // and above the white point past the output white up to 1
    let k = LevelsK::new(&master(ch(20.0, 0.0, 240.0, 5.0, 250.0))).unwrap();
    let at = |x: f32| k.apply([x; 3], false)[0];
    assert!((at(20.0 / 255.0) - 5.0 / 255.0).abs() < 1e-6, "the black point comes out as the output black");
    assert!((at(240.0 / 255.0) - 250.0 / 255.0).abs() < 1e-6);
    // one level below the black point: below the output black by one stretched level (245/220 of a level)
    assert!((at(19.0 / 255.0) - (5.0 - 245.0 / 220.0) / 255.0).abs() < 1e-5, "{}", at(19.0 / 255.0) * 255.0);
    assert_eq!(at(0.0), 0.0);
    assert!(at(244.0 / 255.0) > 250.0 / 255.0, "above the white point it goes past the output white");
    assert_eq!(at(1.0), 1.0);
}

#[test]
fn the_midtone_is_the_measured_curve_between_the_stretch_and_the_output_levels() {
    use lcc_develop::levels_curve::MidtoneTable;
    for m in [-1.0, -0.5, 0.3, 1.0] {
        let k = LevelsK::new(&master(ch(40.0, m, 215.0, 30.0, 220.0))).unwrap();
        let table = MidtoneTable::new(m);
        for x in [0.2f32, 0.35, 0.5, 0.65, 0.8] {
            let t = (x - 40.0 / 255.0) * 255.0 / 175.0;
            let want = 30.0 / 255.0 + 190.0 / 255.0 * table.eval(t);
            assert!((k.apply([x; 3], false)[0] - want).abs() < 1e-5, "m {m}, x {x}");
        }
        // positive brightens the midtones, negative darkens them
        let neutral = LevelsK::new(&master(ch(40.0, 0.0, 215.0, 30.0, 220.0))).unwrap().apply([0.5; 3], false)[0];
        let y = k.apply([0.5; 3], false)[0];
        assert!(if m > 0.0 { y > neutral } else { y < neutral }, "m {m}: {y} against {neutral}");
    }
}

#[test]
fn output_levels_remap_the_range_and_may_invert() {
    let k = LevelsK::new(&master(ch(0.0, 0.0, 255.0, 51.0, 204.0))).unwrap();
    assert!((k.apply([0.0; 3], false)[0] - 0.2).abs() < 1e-6);
    assert!((k.apply([1.0; 3], false)[0] - 0.8).abs() < 1e-6);
    let inv = LevelsK::new(&master(ch(0.0, 0.0, 255.0, 255.0, 0.0))).unwrap();
    for x in [0.0, 0.3, 1.0] {
        assert!((inv.apply([x; 3], false)[0] - (1.0 - x)).abs() < 1e-6, "{x}");
    }
}

#[test]
fn a_black_point_above_the_white_point_inverts() {
    let k = LevelsK::new(&master(ch(204.0, 0.0, 51.0, 0.0, 255.0))).unwrap();
    // 0.8 is the black point, 0.2 the white point: beyond them the ends hold, between them the range runs backwards
    for (x, want) in [(0.0, 1.0), (0.2, 1.0), (0.5, 0.5), (0.65, 0.25), (0.8, 0.0), (1.0, 0.0)] {
        let y = k.apply([x; 3], false);
        assert!(y.iter().all(|v| (v - want).abs() < 1e-5), "{x} -> {y:?}, want {want}");
    }
    // inverted input and inverted output undo each other (inside the range)
    let both = LevelsK::new(&master(ch(204.0, 0.0, 51.0, 255.0, 0.0))).unwrap();
    for x in [0.25, 0.5, 0.7] {
        let y = both.apply([x; 3], false)[0];
        assert!((y - (x - 0.2) / 0.6).abs() < 1e-5, "{x} -> {y}");
    }
    // with a midtone: the curve runs along the inverted range, and a positive midtone still brightens
    let plain = LevelsK::new(&master(ch(204.0, 0.0, 51.0, 0.0, 255.0))).unwrap();
    let up = LevelsK::new(&master(ch(204.0, 0.5, 51.0, 0.0, 255.0))).unwrap();
    let down = LevelsK::new(&master(ch(204.0, -0.5, 51.0, 0.0, 255.0))).unwrap();
    let at = |k: &LevelsK| k.apply([0.5; 3], false)[0];
    assert!(at(&up) > at(&plain) && at(&down) < at(&plain));
}

#[test]
fn each_channel_runs_first_and_then_the_master() {
    let l = Levels { master: ch(30.0, 0.8, 255.0, 0.0, 255.0), red: ch(60.0, -0.4, 200.0, 0.0, 255.0), ..Default::default() };
    let k = LevelsK::new(&l).unwrap();
    let m = StageK::new(&l.master).unwrap();
    let r = StageK::new(&l.red).unwrap();
    let out = k.apply([0.5; 3], false);
    assert!((out[0] - m.eval(r.eval(0.5, false), false)).abs() < 1e-7);
    assert!((out[1] - m.eval(0.5, false)).abs() < 1e-7 && (out[2] - m.eval(0.5, false)).abs() < 1e-7, "only red has its own stage");
    // the other order gives a different number here, so the test tells them apart
    assert!((r.eval(m.eval(0.5, false), false) - out[0]).abs() > 1e-2);
}

#[test]
fn above_sdr_white_one_rule_holds_for_every_stage() {
    // an HDR render lets values above 1 through; the stage goes on with its slope (rising, or falling when inverted),
    // is limited below at 0, and an SDR render is limited at 1
    let rising = LevelsK::new(&master(ch(10.0, 0.4, 240.0, 0.0, 255.0))).unwrap();
    let inverted = LevelsK::new(&master(ch(240.0, 0.4, 10.0, 0.0, 255.0))).unwrap();
    let flat = LevelsK::new(&master(ch(10.0, 0.4, 240.0, 90.0, 90.0))).unwrap();
    let a = |k: &LevelsK, x: f32, hdr: bool| k.apply([x; 3], hdr)[0];
    for k in [&rising, &inverted, &flat] {
        // continuous at white, and never negative
        assert!((a(k, 1.0, true) - a(k, 1.0001, true)).abs() < 1e-2);
        assert!(a(k, 1.0, true) >= 0.0 && a(k, 3.0, true) >= 0.0);
        // monotone over the whole range (non-decreasing, non-increasing for the inverted one)
        let ys: Vec<f32> = (0..=400).map(|i| a(k, i as f32 / 100.0, true)).collect();
        let up = ys.windows(2).all(|w| w[1] >= w[0] - 1e-6);
        let down = ys.windows(2).all(|w| w[1] <= w[0] + 1e-6);
        assert!(up || down, "monotone: {ys:?}");
    }
    assert!(a(&rising, 1.5, true) > a(&rising, 1.0, true) + 0.2, "a rising stage goes on above white");
    assert!(a(&rising, 1.5, false) == 1.0 && a(&rising, 1.5, true) > 1.0, "SDR is limited at 1, HDR is not");
    assert!(a(&inverted, 1.5, true) <= a(&inverted, 1.0, true), "an inverted stage does not rise above white");
    assert_eq!(a(&flat, 1.5, true), a(&flat, 1.0, true), "a flat stage stays flat");
}

#[test]
fn hostile_values_never_produce_non_finite_output() {
    let bad = [f64::NAN, f64::INFINITY, -1e9, 1e9, 0.0, 255.0, 1.0, -1.0];
    for &a in &bad {
        for &b in &bad {
            let c = ChannelLevels { black: a, white: b, midtone: a, out_black: b, out_white: a };
            let k = LevelsK::new(&master(c)).or_else(|| LevelsK::new(&Levels { red: c, ..Default::default() }));
            let Some(k) = k else { continue };
            for x in [0.0, 0.5, 1.0, 3.0, -2.0, f32::NAN, f32::INFINITY] {
                for hdr in [false, true] {
                    let y = k.apply([x; 3], hdr);
                    // (a non-finite input stays the caller's problem; every finite one comes out finite)
                    if x.is_finite() {
                        assert!(y.iter().all(|v| v.is_finite() && *v >= 0.0), "{c:?} {x} -> {y:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_nan_input_does_not_spread() {
    let k = LevelsK::new(&master(ch(10.0, 0.5, 240.0, 0.0, 255.0))).unwrap();
    let y = k.apply([f32::NAN, 0.5, 0.5], false);
    assert!(y[0].is_finite(), "a NaN comes out as a value: {y:?}");
    assert!(y[1].is_finite() && y[2].is_finite());
}

/// Cost of the stage on a 24 MP frame (`cargo test -p lcc-pipeline --release levels_cost -- --ignored --nocapture`,
/// or any build with optimisation): single thread and spread over the cores, for the cheap case (no midtone) and the
/// dearest (master and three channels, all with a midtone).
#[test]
#[ignore = "timing, run by hand"]
fn levels_cost_on_a_24_megapixel_frame() {
    const N: usize = 6000 * 4000;
    let data: Vec<[f32; 3]> = (0..N)
        .map(|i| {
            let v = (i % 4093) as f32 / 4092.0;
            [v, v * 0.8, (1.0 - v) * 0.9]
        })
        .collect();
    let cases = [
        ("master black/white (no midtone)", master(ch(20.0, 0.0, 235.0, 0.0, 255.0))),
        ("master with midtone", master(ch(20.0, 0.4, 235.0, 0.0, 255.0))),
        (
            "master + 3 channels, all with midtone",
            Levels {
                master: ch(10.0, 0.3, 245.0, 5.0, 250.0),
                red: ch(5.0, -0.2, 250.0, 0.0, 255.0),
                green: ch(8.0, 0.2, 240.0, 0.0, 255.0),
                blue: ch(3.0, 0.6, 252.0, 0.0, 255.0),
            },
        ),
    ];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    for (name, l) in cases {
        let k = LevelsK::new(&l).unwrap();
        let run = |part: &[[f32; 3]]| -> f32 {
            let mut acc = 0.0f32;
            for e in part {
                let o = k.apply(*e, false);
                acc += o[0] + o[1] + o[2];
            }
            acc
        };
        let t = std::time::Instant::now();
        let a = run(&data);
        let single = t.elapsed();
        let t = std::time::Instant::now();
        let b: f32 = std::thread::scope(|s| {
            let hs: Vec<_> = data.chunks(N.div_ceil(threads)).map(|c| s.spawn(|| run(c))).collect();
            hs.into_iter().map(|h| h.join().unwrap_or(0.0)).sum()
        });
        let par = t.elapsed();
        eprintln!(
            "{name:<40} 1 thread {:>6.1} ms ({:.2} ns/px)   {threads} threads {:>6.1} ms   (checks {a:.0} {b:.0})",
            single.as_secs_f64() * 1e3,
            single.as_secs_f64() * 1e9 / N as f64,
            par.as_secs_f64() * 1e3
        );
    }
}
