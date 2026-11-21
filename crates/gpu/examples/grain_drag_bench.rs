//! Grain slider drags on the GPU: the time per frame of the draft renders the loupe makes while
//! Amount or Size is dragged, on a ~2.5 MP preview (1936 × 1290, the procedural demo photo).
//! `cargo run --release -p lightcraft-gpu --example grain_drag_bench` (`FRAMES=n`, default 40).
//!
//! The same source, settings and request on every build, so it can be copied unchanged into
//! another branch to compare grain models (see `docs/grain.md` → Performance). Each drag first
//! renders four frames that aren't counted (stage cache, GPU pipelines), then moves the slider one
//! step a frame; prints the median, fastest and slowest frame.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Instant;

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{Quality, RenderRequest, SourceInfo, StageCache};

fn main() {
    let frames: usize = std::env::var("FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40).max(1);
    let (w, h) = (1936, 1290);
    let src = Arc::new(lightcraft_scenes::demo_library()[3].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(w, h) };
    let Some(adapter) = lightcraft_gpu::adapter_name() else {
        eprintln!("no GPU adapter: nothing to measure");
        return;
    };
    println!("gpu: {adapter}; {w}×{h} draft renders, {frames} frames per drag");
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.3;
    let mut drag = |label: &str, step: &dyn Fn(&mut DevelopSettings, usize)| {
        let cache = StageCache::default();
        let mut ms = Vec::with_capacity(frames);
        for i in 0..frames + 4 {
            step(&mut s, i);
            let t = Instant::now();
            assert!(lightcraft_gpu::render(&src, &info, &s, &draft, Some(&cache)).is_some(), "GPU render failed");
            if i >= 4 {
                ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
        }
        ms.sort_by(f64::total_cmp);
        println!("{label:<30} median {:5.1} ms   min {:5.1}   max {:5.1}", ms[ms.len() / 2], ms[0], ms[ms.len() - 1]);
    };
    drag("exposure drag, no grain", &|s, i| {
        s.grain.amount = 0.0;
        s.light.exposure = 0.3 + i as f64 * 0.01;
    });
    drag("Amount drag (20 → 100)", &|s, i| {
        s.grain.size = 25.0;
        s.grain.amount = 20.0 + (i * 2 % 80) as f64;
    });
    drag("Size drag (amount 50, 0 → 100)", &|s, i| {
        s.grain.amount = 50.0;
        s.grain.size = (i * 2 % 100) as f64;
    });
}
