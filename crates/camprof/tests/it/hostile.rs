//! Hostile chart photos and corners: every one gives an error or `None`, never a panic.

use lcc_camprof::{BuildOptions, ChartCorners, Located, RawFrame, Shot, build, build_dual, find_chart, orient, refine};

fn frame(w: usize, h: usize, px: [f32; 3]) -> RawFrame {
    RawFrame::from_rgb(w, h, vec![px; w * h]).unwrap()
}

fn corners(v: [f64; 8]) -> ChartCorners {
    ChartCorners { top_left: [v[0], v[1]], top_right: [v[2], v[3]], bottom_right: [v[4], v[5]], bottom_left: [v[6], v[7]] }
}

#[test]
fn hostile_charts_give_errors_not_panics() {
    let frames = [
        frame(1, 1, [0.5; 3]),
        frame(64, 48, [0.0; 3]),
        frame(64, 48, [f32::NAN; 3]),
        frame(64, 48, [f32::INFINITY, -1.0, 1e30]),
        frame(300, 200, [0.2, 0.4, 0.1]),
    ];
    let bad_corners = [
        [0.0; 8],
        [f64::NAN; 8],
        [f64::INFINITY, 0.0, -1e300, 5.0, 1e300, 1e300, 0.0, 1.0],
        [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0],
        [-50.0, -50.0, 1e6, -50.0, 1e6, 1e6, -50.0, 1e6],
        [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0],
    ];
    for f in &frames {
        let _ = find_chart(f);
        for c in bad_corners {
            let c = corners(c);
            let _ = refine(f, lcc_camprof::chart(), &c, 9);
            let _ = orient(f, lcc_camprof::chart(), &c);
            for turns in [0, 3, 200] {
                let located = Located { corners: c, turns, score: None, chart: lcc_camprof::chart() };
                let opts = BuildOptions { camera_model: "Test".into(), ..Default::default() };
                let _ = build(f, &located, &opts);
                let shot = Shot { frame: f, located, illuminant: "D55" };
                let other = Shot { frame: f, located, illuminant: "A" };
                let _ = build_dual(&shot, &other, &opts);
            }
        }
    }
}

#[test]
fn bad_options_are_errors() {
    let f = frame(64, 48, [0.3; 3]);
    let located = Located { corners: corners([4.0, 4.0, 60.0, 4.0, 60.0, 44.0, 4.0, 44.0]), turns: 0, score: None, chart: lcc_camprof::chart() };
    for (model, light) in [("", "D55"), ("X", "nonsense"), ("X", ""), ("\u{0}", "D65")] {
        let opts = BuildOptions { camera_model: model.into(), illuminant: light.into(), ..Default::default() };
        assert!(build(&f, &located, &opts).is_err(), "{model:?} {light:?}");
    }
    let opts = BuildOptions { camera_model: "X".into(), ..Default::default() };
    let a = Shot { frame: &f, located, illuminant: "D55" };
    let same = Shot { frame: &f, located, illuminant: "D55" };
    assert!(build_dual(&a, &same, &opts).is_err());
    let unknown = Shot { frame: &f, located, illuminant: "Moonlight" };
    assert!(build_dual(&a, &unknown, &opts).is_err());
}
