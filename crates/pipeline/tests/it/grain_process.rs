//! The grain model follows the photo's process (`docs/process-versions.md`): V1–V6 keep the
//! value-noise grain they always rendered with, whatever the new grain fields say; the particle grain
//! (OneSlips, `lcc_pipeline::grain`) comes with V7.

use lcc_develop::{DevelopSettings, Grain, Process};
use lcc_pipeline::{RenderRequest, SourceInfo, render};
use lcc_raster::Rgb32f;

fn rendered_at(s: &DevelopSettings, w: usize, h: usize) -> Vec<[u8; 4]> {
    let src = Rgb32f::filled(w, h, [0.18; 3]);
    render(&src, &SourceInfo::default(), s, &RenderRequest::fit(w, h)).image.data
}

fn rendered(s: &DevelopSettings) -> Vec<[u8; 4]> {
    rendered_at(s, 180, 120)
}

fn with(process: Process, grain: Grain) -> DevelopSettings {
    DevelopSettings { process: process.version(), grain, ..DevelopSettings::default() }
}

#[test]
fn old_processes_ignore_the_particle_grain_fields() {
    let base = Grain { amount: 60.0, ..Grain::default() };
    let tuned = Grain { color: 80.0, response: -60.0, size_variation: 90.0, clumping: 5.0, ..base };
    for p in [Process::V1, Process::V5, Process::V6] {
        assert_eq!(rendered(&with(p, base)), rendered(&with(p, tuned)), "{p:?} keeps its value-noise grain");
    }
}

#[test]
fn v7_renders_particle_grain_that_keeps_the_mean() {
    let g = Grain { amount: 60.0, ..Grain::default() };
    let v6 = rendered(&with(Process::V6, g));
    let v7 = rendered(&with(Process::V7, g));
    assert_ne!(v6, v7, "V7 draws a different grain");
    let plain = rendered(&with(Process::V7, Grain::default()));
    let mean = |d: &[[u8; 4]]| d.iter().map(|p| f64::from(p[1])).sum::<f64>() / d.len() as f64;
    assert!((mean(&v7) - mean(&plain)).abs() < 3.0, "grain keeps the tone: {} vs {}", mean(&v7), mean(&plain));
    let var = v7.iter().map(|p| (f64::from(p[1]) - mean(&v7)).powi(2)).sum::<f64>() / v7.len() as f64;
    assert!(var > 0.2, "visible grain: variance {var}");
}

#[test]
fn hostile_grain_values_render_without_panicking() {
    for v in [f64::NAN, f64::INFINITY, -1e300, 1e300] {
        let g = Grain { amount: 50.0, size: v, roughness: v, color: v, response: v, size_variation: v, clumping: v, seed: u32::MAX };
        let _ = rendered_at(&with(Process::V7, g), 48, 32);
    }
}
