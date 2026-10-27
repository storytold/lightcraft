//! Process V3's Highlights, Shadows, Whites and Blacks (issue #523): their response per 10-L*
//! band, on flat grey patches through a real camera tone curve (Sony's, `assets/camera-tone`).
//!
//! The reference rows are black-box measurements of Lightroom's exports posted in #523 (the median
//! L* shift per band of three Canon scenes at −100; for Shadows +100, the Nikon Z f response
//! estimated from the share / error tables there). Print every row with
//! `cargo test -p lightcraft-pipeline light_probe -- --ignored --nocapture`.

use lightcraft_develop::{DevelopSettings, Process, controls};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const COLS: usize = 12;
const ROWS: usize = 8;
const CELL: usize = 100;

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

fn shot(s: &DevelopSettings) -> Rgba8 {
    let (w, h) = (COLS * CELL, ROWS * CELL);
    let src = Rgb32f::from_fn(w, h, |x, y| [0.18 * ev_of((y / CELL) * COLS + x / CELL).exp2(); 3]);
    let info = SourceInfo { raw: true, relative_wb: true, camera_tone: Some(curve()), ..Default::default() };
    render(&src, &info, s, &RenderRequest::fit(w, h)).image
}

/// L* (of the green channel: the patches are grey) at the centre of every patch.
fn patches(process: Process, set: &[(&str, f64)]) -> Vec<f32> {
    let img = shot(&settings(process, set));
    (0..COLS * ROWS).map(|i| lstar(img.get((i % COLS) * CELL + CELL / 2, (i / COLS) * CELL + CELL / 2)[1])).collect()
}

/// [`patches`] without edits: the same on every process (`v3_renders_like_v2_without_its_sliders`).
fn neutral() -> &'static [f32] {
    static NEUTRAL: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    NEUTRAL.get_or_init(|| patches(Process::V2, &[]))
}

/// Mean L* shift of `set` per 10-L* band of the neutral render (`None`: no patch in the band).
fn bands(process: Process, set: &[(&str, f64)]) -> [Option<f32>; 10] {
    let moved = patches(process, set);
    let band_of = |l: f32| ((l / 10.0).floor() as usize).min(9);
    std::array::from_fn(|b| {
        let (sum, n) = neutral().iter().zip(&moved).filter(|(l, _)| band_of(**l) == b).fold((0.0, 0), |(s, n), (l, m)| (s + m - l, n + 1));
        (n > 0).then(|| sum / n as f32)
    })
}

/// RMS over the bands of `got − want`.
fn rms(got: &[Option<f32>; 10], want: &[f32; 10]) -> f32 {
    let (sum, n) = got.iter().zip(want).filter_map(|(g, w)| g.map(|g| (g - w).powi(2))).fold((0.0, 0), |(s, n), d| (s + d, n + 1));
    (sum / n as f32).sqrt()
}

const HIGHLIGHTS_M100: [f32; 10] = [-0.2, -0.9, -1.5, -2.1, -3.5, -5.9, -8.9, -14.0, -19.3, -15.6];
const WHITES_M100: [f32; 10] = [0.0, -0.5, -0.8, -1.3, -2.3, -3.4, -4.6, -7.1, -9.5, -9.9];
const BLACKS_M100: [f32; 10] = [-5.5, -14.2, -22.0, -20.7, -16.3, -12.8, -9.0, -5.3, -2.4, -1.0];
const SHADOWS_P100: [f32; 10] = [5.0, 9.0, 8.0, 7.0, 5.5, 3.5, 1.8, 0.6, 0.0, 0.0];

#[test]
fn v3_tone_sliders_follow_the_measured_response() {
    for (id, v, want, v2_was, v3_within) in [
        ("light.highlights", -100.0, HIGHLIGHTS_M100, 4.0, 1.5),
        ("light.whites", -100.0, WHITES_M100, 2.0, 1.0),
        ("light.blacks", -100.0, BLACKS_M100, 12.0, 1.5),
        ("light.shadows", 100.0, SHADOWS_P100, 3.0, 1.0),
    ] {
        let (v2, v3) = (rms(&bands(Process::V2, &[(id, v)]), &want), rms(&bands(Process::V3, &[(id, v)]), &want));
        // V2 stays where it was (frozen), V3 is close to the reference
        assert!(v2 > v2_was, "{id} {v}: V2 RMS {v2} L*");
        assert!(v3 < v3_within, "{id} {v}: V3 RMS {v3} L* (V2 {v2})");
    }
}

#[test]
fn v3_positive_sliders_move_the_right_tones() {
    let b = |id: &str, v: f64| bands(Process::V3, &[(id, v)]).map(|x| x.unwrap_or(0.0));
    // Highlights +100 lifts the upper midtones and highlights, not the shadows
    let h = b("light.highlights", 100.0);
    assert!(h[0..3].iter().all(|v| v.abs() < 0.5) && h[6] > 6.0 && h[7] > 8.0, "highlights +100 {h:?}");
    // Whites +50 brightens down to the midtones, deep shadows barely
    let w = b("light.whites", 50.0);
    assert!(w[0] < 0.5 && w[4] > 2.0 && w[7] > 3.0, "whites +50 {w:?}");
    // Blacks +100 lifts the shadows most and leaves white where it is
    let k = b("light.blacks", 100.0);
    assert!(k[1] > 8.0 && k[4] > 4.0 && k[9] < 0.5, "blacks +100 {k:?}");
    // Shadows −100 deepens the shadows, not the highlights
    let s = b("light.shadows", -100.0);
    assert!(s[2] < -4.0 && s[8].abs() < 0.5, "shadows −100 {s:?}");
}

#[test]
fn v3_renders_like_v2_without_its_sliders() {
    // V3 only changes Highlights, Shadows, Whites and Blacks: an edit without them renders exactly
    // as on V2
    for set in [&[][..], &[("light.exposure", 0.7), ("light.contrast", 35.0), ("effects.clarity", 20.0)]] {
        assert!(shot(&settings(Process::V2, set)) == shot(&settings(Process::V3, set)), "{set:?}");
    }
}

#[test]
fn v3_whites_blacks_are_monotone_and_finite() {
    use crate::tone::whites_blacks_v3;
    for w in [-1.0, -0.5, -0.01, 0.0, 0.01, 0.25, 0.5, 1.0] {
        for b in [-1.0, -0.5, -0.01, 0.0, 0.01, 0.25, 0.5, 1.0] {
            let mut prev = whites_blacks_v3(0.0, w, b);
            assert!(prev.abs() < 1e-6, "{w} {b}: black {prev}");
            for i in 1..=2000 {
                let v = whites_blacks_v3(i as f32 / 2000.0, w, b);
                assert!(v.is_finite() && (0.0..=1.0).contains(&v) && v >= prev - 1e-6, "{w} {b} at {i}: {v} after {prev}");
                prev = v;
            }
        }
    }
    for o in [f32::NAN, f32::INFINITY, -1.0, 2.0] {
        assert!(whites_blacks_v3(o, 0.5, -0.5).is_finite());
    }
}

#[test]
#[ignore]
fn light_probe() {
    println!("{:<22} {}", "band L*", (0..10).map(|b| format!("{:>3}-{:<2}", b * 10, b * 10 + 10)).collect::<Vec<_>>().join(" "));
    for p in [Process::V2, Process::V3] {
        println!("-- {p:?}");
        for (id, v) in [
            ("light.highlights", -100.0),
            ("light.highlights", 100.0),
            ("light.shadows", 100.0),
            ("light.shadows", -100.0),
            ("light.whites", -100.0),
            ("light.whites", 100.0),
            ("light.whites", 50.0),
            ("light.blacks", -100.0),
            ("light.blacks", 100.0),
        ] {
            let cells: Vec<String> = bands(p, &[(id, v)]).iter().map(|v| v.map_or("   -  ".into(), |v| format!("{v:+6.1}"))).collect();
            println!("{:<22} {}", format!("{id} {v:+}"), cells.join(" "));
        }
    }
}
