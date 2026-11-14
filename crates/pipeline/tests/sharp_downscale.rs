//! A pure above-Nyquist carrier must be suppressed rather than folded into a smaller export.
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::geometry::Frame;
use lightcraft_raster::{Rgb32f, resample::Filter};

#[test]
fn mild_sharp_reductions_suppress_above_target_nyquist_aliases() {
    // 0.49 cycles/source pixel is above the 90% and 80% outputs' Nyquist limits.
    let source = Rgb32f::from_fn(1000, 100, |x, _| [0.5 + 0.25 * (std::f32::consts::TAU * 0.49 * x as f32).sin(); 3]);
    let power = |image: &Rgb32f| {
        let values: Vec<f64> = (12..image.width - 12).map(|x| f64::from(image.get(x, image.height / 2)[0])).collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64
    };
    let mut frame = Frame::new(1000, 100, &DevelopSettings::default(), true);
    frame.downscale = Filter::Lanczos3;
    for (w, h) in [(900, 90), (800, 80)] {
        let k = 1000.0 / w as f64;
        let unfiltered = Rgb32f::from_fn(w, h, |x, y| source.sample_bilinear(((x as f64 + 0.5) * k) as f32, ((y as f64 + 0.5) * k) as f32));
        let (sharp, bilinear) = (power(&frame.sample(&source, w, h)), power(&unfiltered));
        assert!(sharp < bilinear * 0.95, "{w}: residual alias power {sharp} vs unfiltered {bilinear}");
    }
}

#[test]
fn a_height_only_reduction_is_prefiltered_too() {
    // the width stays 1:1, the height shrinks 1000 → 900: the vertical carrier must not fold in
    let source = Rgb32f::from_fn(100, 1000, |_, y| [0.5 + 0.25 * (std::f32::consts::TAU * 0.49 * y as f32).sin(); 3]);
    let power = |image: &Rgb32f| {
        let values: Vec<f64> = (12..image.height - 12).map(|y| f64::from(image.get(image.width / 2, y)[0])).collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64
    };
    let mut frame = Frame::new(100, 1000, &DevelopSettings::default(), true);
    frame.downscale = Filter::Lanczos3;
    let (w, h) = (100, 900);
    assert!(frame.sample_plan(100, 1000, w, h).prefilter.is_some(), "a reduction on either axis is prefiltered");
    let k = 1000.0 / h as f64;
    let unfiltered = Rgb32f::from_fn(w, h, |x, y| source.sample_bilinear(x as f32 + 0.5, ((y as f64 + 0.5) * k) as f32));
    let (sharp, bilinear) = (power(&frame.sample(&source, w, h)), power(&unfiltered));
    assert!(sharp < bilinear * 0.95, "residual alias power {sharp} vs unfiltered {bilinear}");
}
