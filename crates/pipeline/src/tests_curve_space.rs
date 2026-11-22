//! Process V10 runs the Tone Curve in a fixed wide space (ProPhoto primaries, gamma 1.8) before the
//! output conversion (fork issue #1142). Measured on a synthetic chart: neutral ramp plus in-gamut
//! colour patches, with the issue's red/green channel curves (a scan's red-cast fix).

use lcc_develop::{DevelopSettings, Process, ProcessVersion};
use lcc_geom::Point;
use lcc_raster::Rgb32f;

use crate::finish::CurveSpace;
use crate::{DeepSamples, OutputDepth, OutputSpace, RenderRequest, SourceInfo, render};

const W: usize = 48;
const H: usize = 8;

/// Linear Rec.2020 chart: the top half a grey ramp, the bottom half a hue sweep at moderate chroma.
fn chart() -> Rgb32f {
    Rgb32f::from_fn(W, H, |x, y| {
        let t = (x as f32 + 0.5) / W as f32;
        if y < H / 2 {
            [0.01 + 0.8 * t * t; 3]
        } else {
            let h = t * std::f32::consts::TAU;
            let base = 0.18;
            [base * (1.0 + 0.5 * h.cos()), base * (1.0 + 0.5 * (h - 2.094).cos()), base * (1.0 + 0.5 * (h + 2.094).cos())]
        }
    })
}

fn pts(p: &[(f64, f64)]) -> Vec<Point> {
    p.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

/// The issue's edit: ToneCurvePV2012Red {0,0, 158,97, 255,255}, Green {0,0, 131,123, 255,255}.
fn settings(process: Process) -> DevelopSettings {
    let mut s = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
    s.curve.red = pts(&[(0.0, 0.0), (158.0 / 255.0, 97.0 / 255.0), (1.0, 1.0)]);
    s.curve.green = pts(&[(0.0, 0.0), (131.0 / 255.0, 123.0 / 255.0), (1.0, 1.0)]);
    s
}

/// The render as OkLab per pixel (via linear Rec.2020), so renders into different spaces compare.
fn oklab(s: &DevelopSettings, space: OutputSpace) -> Vec<[f32; 3]> {
    let req = RenderRequest { space, depth: OutputDepth::F32Linear, ..RenderRequest::fit(W, H) };
    let r = render(&chart(), &SourceInfo::default(), s, &req);
    let Some(DeepSamples::F32(v)) = r.deep.map(|d| d.samples) else { panic!("no float render") };
    let m = space.rgb_space().to_space(&lcc_color::REC2020).to_f32();
    v.chunks_exact(3)
        .map(|c| {
            let l = [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]);
            lcc_color::perceptual::oklab_from_2020(l)
        })
        .collect()
}

/// Median OkLab distance ×100 (≈ ΔE units) between two renders.
fn median_de(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    let mut d: Vec<f32> =
        a.iter().zip(b).map(|(p, q)| 100.0 * ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()).collect();
    d.sort_by(f32::total_cmp);
    d.get(d.len() / 2).copied().unwrap_or(f32::NAN)
}

#[test]
fn v10_curves_no_longer_depend_on_the_export_space() {
    let v9 = settings(Process::V9);
    let v10 = settings(Process::V10);
    let de = |s: &DevelopSettings, a: OutputSpace, b: OutputSpace| median_de(&oklab(s, a), &oklab(s, b));
    // two spaces wide enough to hold the result: V10 renders the same colours in both
    let (w9, w10) = (de(&v9, OutputSpace::Rec2020, OutputSpace::ProPhoto), de(&v10, OutputSpace::Rec2020, OutputSpace::ProPhoto));
    // sRGB can't hold the cyan the red curve leaves on neutrals, so its gamut map still moves them in V10
    let (s9, s10) = (de(&v9, OutputSpace::Srgb, OutputSpace::ProPhoto), de(&v10, OutputSpace::Srgb, OutputSpace::ProPhoto));
    eprintln!("median ΔE(OkLab×100) Rec.2020 vs ProPhoto export: V9 {w9:.2}, V10 {w10:.2}; sRGB vs ProPhoto: V9 {s9:.2}, V10 {s10:.2}");
    assert!(w10 < 0.3, "V10 should render the same edit alike in both wide spaces: {w10}");
    assert!(w9 > 1.0, "V9 depends on the output space: {w9}");
    assert!(s10 < s9 * 0.6, "{s10} vs {s9}");
}

#[test]
fn v10_moves_the_render_and_older_processes_keep_theirs() {
    let v9 = oklab(&settings(Process::V9), OutputSpace::Srgb);
    let v10 = oklab(&settings(Process::V10), OutputSpace::Srgb);
    let d = median_de(&v9, &v10);
    eprintln!("V9 vs V10 sRGB render, median ΔE(OkLab×100): {d:.2}");
    assert!(d > 0.5, "{d}");
    // without curves the two processes render alike
    let flat = |p: Process| DevelopSettings { process: p.version(), ..DevelopSettings::default() };
    let d0 = median_de(&oklab(&flat(Process::V9), OutputSpace::Srgb), &oklab(&flat(Process::V10), OutputSpace::Srgb));
    assert!(d0 < 0.05, "{d0}");
    assert_eq!(ProcessVersion::LATEST, Process::V10.version());
}

#[test]
fn identity_tables_round_trip_and_hostile_values_stay_finite_or_pass_through() {
    let cs = CurveSpace::new();
    let id = std::array::from_fn(|_| lcc_color::spline::Lut1::identity(1024));
    for c in [[0.18f32; 3], [0.5, 0.2, 0.05], [2.5, 1.0, 0.3], [-0.02, 0.4, 0.1], [0.0; 3]] {
        let o = cs.apply_curves(&id, c, 1.0);
        for k in 0..3 {
            assert!((o[k] - c[k]).abs() < 2e-3 * c[k].abs().max(1.0), "{c:?} -> {o:?}");
        }
    }
    for c in [[f32::INFINITY, 0.0, 0.0], [f32::MAX, f32::MIN, 0.0], [1e30, 1e30, 1e30]] {
        let _ = cs.apply_curves(&id, c, 0.5); // no panic
    }
}
