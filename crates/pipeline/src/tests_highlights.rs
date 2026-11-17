//! Process V9's Highlights (issue #1371): its response per 10-L* band on flat grey patches through a
//! measured camera tone curve (Sony's, `assets/camera-tone`), against Lightroom Classic's.
//!
//! The reference row is the black-box measurement of Lightroom's exports posted in #523 and used in
//! #2313: the median L* shift per band of three Canon scenes at Highlights −100. Print the rows with
//! `cargo test -p lightcraft-pipeline highlights_probe -- --ignored --nocapture`.

use lightcraft_develop::{DevelopSettings, Process, controls};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const COLS: usize = 12;
const ROWS: usize = 8;
const CELL: usize = 64;

/// Lightroom Classic's L* shift per 10-L* band at Highlights −100 (#523).
pub(crate) const LIGHTROOM_M100: [f32; 10] = [-0.2, -0.9, -1.5, -2.1, -3.5, -5.9, -8.9, -14.0, -19.3, -15.6];

/// Scene EV over grey of patch `i`: −8 … +3.
fn ev_of(i: usize) -> f32 {
    -8.0 + 11.0 * i as f32 / (COLS * ROWS - 1) as f32
}

fn curve() -> crate::tone::CameraTone {
    serde_json::from_str(include_str!("../../../assets/camera-tone/sony-dro-off.json")).unwrap()
}

fn lstar(v: u8) -> f32 {
    let y = lightcraft_color::transfer::decode_srgb8(v);
    if y > 216.0 / 24389.0 { 116.0 * y.cbrt() - 16.0 } else { y * 24389.0 / 27.0 }
}

fn settings(process: Process, set: &[(&str, f64)]) -> DevelopSettings {
    let mut s = DevelopSettings { process: process.version(), ..DevelopSettings::for_raw(5000.0, 0.0) };
    for (id, v) in set {
        controls::set(&mut s, id, *v);
    }
    s
}

pub(crate) fn shot(s: &DevelopSettings) -> Rgba8 {
    let (w, h) = (COLS * CELL, ROWS * CELL);
    let src = Rgb32f::from_fn(w, h, |x, y| [0.18 * ev_of((y / CELL) * COLS + x / CELL).exp2(); 3]);
    let info = SourceInfo { raw: true, relative_wb: true, camera_tone: Some(curve()), ..Default::default() };
    render(&src, &info, s, &RenderRequest::fit(w, h)).image
}

/// L* (of green: the patches are grey) at the centre of every patch.
fn patches(process: Process, set: &[(&str, f64)]) -> Vec<f32> {
    let img = shot(&settings(process, set));
    (0..COLS * ROWS).map(|i| lstar(img.get((i % COLS) * CELL + CELL / 2, (i / COLS) * CELL + CELL / 2)[1])).collect()
}

/// Mean L* shift of `set` per 10-L* band of the process's neutral render (`None`: no patch there).
pub(crate) fn bands(process: Process, set: &[(&str, f64)]) -> [Option<f32>; 10] {
    let (neutral, moved) = (patches(process, &[]), patches(process, set));
    let band_of = |l: f32| ((l / 10.0).floor() as usize).min(9);
    std::array::from_fn(|b| {
        let (sum, n) = neutral.iter().zip(&moved).filter(|(l, _)| band_of(**l) == b).fold((0.0, 0), |(s, n), (l, m)| (s + m - l, n + 1));
        (n > 0).then(|| sum / n as f32)
    })
}

/// RMS over the bands of `got − want`.
pub(crate) fn rms(got: &[Option<f32>; 10], want: &[f32; 10]) -> f32 {
    let (sum, n) = got.iter().zip(want).filter_map(|(g, w)| g.map(|g| (g - w).powi(2))).fold((0.0, 0), |(s, n), d| (s + d, n + 1));
    (sum / n as f32).sqrt()
}

/// Given a ramp of grey patches through a camera curve, when Highlights is at −100, then V9 moves each 10-L*
/// band within 1.5 L* RMS of Lightroom's shift (V8 stays where it was, 4 L* or more off: frozen).
#[test]
fn v9_highlights_follow_lightrooms_response() {
    let v8 = rms(&bands(Process::V8, &[("light.highlights", -100.0)]), &LIGHTROOM_M100);
    let v9 = rms(&bands(Process::V9, &[("light.highlights", -100.0)]), &LIGHTROOM_M100);
    assert!(v8 > 4.0, "V8 RMS {v8} L*");
    assert!(v9 < 1.5, "V9 RMS {v9} L* (V8 {v8})");
}

/// Given the same ramp, when Highlights is ±100, then V9 moves the upper midtones and highlights (about twice as
/// far as V8 at L* 50–80), each way, and leaves the shadows where they are.
#[test]
fn v9_highlights_move_the_upper_tones_both_ways_and_not_the_shadows() {
    for v in [-100.0, 100.0] {
        let b = |p| bands(p, &[("light.highlights", v)]).map(|x| x.unwrap_or(0.0));
        let (v8, v9) = (b(Process::V8), b(Process::V9));
        let sign = v.signum() as f32;
        assert!(v9[0..3].iter().all(|x| x.abs() < 0.5), "{v}: shadows {v9:?}");
        assert!(v9[5..9].iter().all(|x| x * sign > 0.0), "{v}: upper tones move its way {v9:?}");
        let (s8, s9): (f32, f32) = (v8[5..8].iter().sum(), v9[5..8].iter().sum());
        assert!(s9 * sign > 1.7 * s8 * sign, "{v}: L* 50–80 V9 {s9} vs V8 {s8}");
    }
}

/// Given an edit without Highlights, then V9 renders byte for byte like V8: only Highlights changed.
#[test]
fn v9_renders_like_v8_without_highlights() {
    for set in
        [&[][..], &[("light.exposure", 0.7), ("light.contrast", 35.0), ("light.shadows", 40.0), ("light.whites", -20.0), ("effects.clarity", 20.0)]]
    {
        assert!(shot(&settings(Process::V8, set)) == shot(&settings(Process::V9, set)), "{set:?}");
    }
}

#[test]
#[ignore]
fn highlights_probe() {
    println!("{:<26} {}", "band L*", (0..10).map(|b| format!("{:>3}-{:<2}", b * 10, b * 10 + 10)).collect::<Vec<_>>().join(" "));
    println!("{:<26} {}", "Lightroom −100", LIGHTROOM_M100.map(|v| format!("{v:+6.1}")).join(" "));
    for p in [Process::V8, Process::V9] {
        for v in [-100.0, -50.0, 50.0, 100.0] {
            let b = bands(p, &[("light.highlights", v)]);
            let cells: Vec<String> = b.iter().map(|v| v.map_or("   -  ".into(), |v| format!("{v:+6.1}"))).collect();
            let r = if v == -100.0 { format!("  RMS {:.2}", rms(&b, &LIGHTROOM_M100)) } else { String::new() };
            println!("{:<26} {}{r}", format!("{p:?} highlights {v:+}"), cells.join(" "));
        }
    }
}
