//! The shared procedural bracket for heap and release timing measurements. No files are read.

use lightcraft_merge::Frame;
use lightcraft_raster::Rgb32f;
use rayon::prelude::*;

const SCENE_W: usize = 1200;
const SCENE_H: usize = 800;
const MARGIN: usize = 16;

/// A fixed-size procedural scene keeps fixture memory independent of the measured image size.
/// It is built before profiling, so neither it nor its median scratch counts as merge memory.
pub fn scene() -> Rgb32f {
    let scene = lightcraft_scenes::demo_library().into_iter().find(|s| s.kind == lightcraft_scenes::Kind::OceanSunset).unwrap();
    let mut image = scene.render(SCENE_W + 2 * MARGIN, SCENE_H + 2 * MARGIN);
    let mut green: Vec<f32> = image.data.iter().map(|p| p[1]).collect();
    green.sort_by(f32::total_cmp);
    let scale = 0.18 / green[green.len() / 2].max(1e-6);
    image.map_in_place(|p| p.map(|v| v * scale));
    image
}

/// Quantised, clipped exposures with handheld translations and a moving dark disk. Each frame
/// is made directly at its final size, without a second full-size truth or temporary frame.
pub fn brackets<'a>(scene: &'a Rgb32f, w: usize, h: usize, n: usize, before_frame: impl Fn(usize) + 'a) -> impl Iterator<Item = Frame> + 'a {
    (0..n).map(move |i| {
        before_frame(i);
        let offset = i as f32 - (n / 2) as f32;
        let ev = if n == 7 { offset } else { 2.0 * offset };
        let gain = 2f32.powf(ev);
        let mut image = Rgb32f::new(w, h);
        image.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, p) in row.iter_mut().enumerate() {
                let sx = (x as f32 + 0.5) * SCENE_W as f32 / w as f32 - offset * 3.25;
                let sy = (y as f32 + 0.5) * SCENE_H as f32 / h as f32 + offset * 1.75;
                let mut value = scene.sample_bilinear(sx + MARGIN as f32, sy + MARGIN as f32);
                if (sx - (600.0 + offset * 90.0)).powi(2) + (sy - 500.0).powi(2) < 30.0f32.powi(2) {
                    value = [0.01, 0.012, 0.02];
                }
                *p = value.map(|v| ((v * gain).clamp(0.0, 1.0) * 16383.0).round() / 16383.0);
            }
        });
        let mut frame = Frame::from_linear_rec2020(image, Some(f64::from(ev)));
        frame.raw = true;
        frame.clip = 1.0;
        frame
    })
}
