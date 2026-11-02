//! A batch of renders on the GPU cards, the way exports and preview builds take them: `lanes`
//! threads, each taking `wait_batch_turn_any` inside `as_batch` per photo. Prints the time and how
//! many photos each card rendered, to compare one card with both.
//!
//! `cargo run --release -p lightcraft-gpu --example batch_bench -- [--helper off|auto|<name>]
//! [--photos 24] [--lanes 2] [--mp 24]`
//!
//! The source is a procedural demo photo (`lightcraft-scenes`); no files are read.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::Role;
use lightcraft_pipeline::{RenderRequest, SourceInfo};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let helper = arg(&args, "--helper").unwrap_or_else(|| "off".into());
    let photos: usize = arg(&args, "--photos").and_then(|v| v.parse().ok()).unwrap_or(24);
    let lanes: usize = arg(&args, "--lanes").and_then(|v| v.parse().ok()).unwrap_or(2).max(1);
    let mp: f64 = arg(&args, "--mp").and_then(|v| v.parse().ok()).unwrap_or(24.0);
    let w = ((mp * 1e6 * 1.5).sqrt()).round() as usize;
    let h = (w * 2 / 3).max(1);

    let t = Instant::now();
    if !lightcraft_gpu::available() {
        eprintln!("no GPU: {}", lightcraft_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    eprintln!("editing card: {} ({:.1} s)", lightcraft_gpu::card_in_use().unwrap_or_default(), t.elapsed().as_secs_f64());
    if let Err(e) = lightcraft_gpu::set_helper(&helper) {
        eprintln!("helper: {e}");
        return;
    }
    let t = Instant::now();
    lightcraft_gpu::wait_helper(Duration::from_secs(240));
    let st = lightcraft_gpu::helper_status();
    eprintln!("second card: {} {:?} ({:.1} s)", st.state, st.name, t.elapsed().as_secs_f64());

    let src = Arc::new(lightcraft_scenes::demo_library()[2].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.effects.clarity = 25.0;
    s.detail.nr_luminance = 20.0;
    let req = RenderRequest::fit(w, h);
    let px = w * h;
    // one render first on each card in use (pipelines warm, buffers pooled)
    let _ = lightcraft_gpu::try_render(&src, &info, &s, &req, None);
    if lightcraft_gpu::helper_ready() {
        let _ = lightcraft_gpu::with_role(Role::Helper, || lightcraft_gpu::try_render(&src, &info, &s, &req, None));
    }

    let next = AtomicUsize::new(0);
    let on = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
    let t = Instant::now();
    std::thread::scope(|sc| {
        for _ in 0..lanes {
            sc.spawn(|| {
                while next.fetch_add(1, Ordering::Relaxed) < photos {
                    lightcraft_gpu::as_batch(|| {
                        let turn = lightcraft_gpu::wait_batch_turn_any(px, px, &|| false);
                        let slot = match turn.as_ref().map(|t| t.role()) {
                            Some(Role::Primary) => 0,
                            Some(Role::Helper) => 1,
                            None => 2,
                        };
                        let ok = lightcraft_gpu::try_render(&src, &info, &s, &req, None).is_ok();
                        drop(turn);
                        if ok {
                            on[slot].fetch_add(1, Ordering::Relaxed);
                        } else {
                            on[2].fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            });
        }
    });
    let secs = t.elapsed().as_secs_f64();
    let mb = |r: Option<f64>| r.map_or("-".to_string(), |r| format!("{:.0} MB/s", r / 1e6));
    println!(
        "helper={helper} photos={photos} lanes={lanes} {w}x{h}: {secs:.2} s ({:.2} s/photo) | editing {} ({}) | second {} ({}) | cpu/failed {}",
        secs / photos.max(1) as f64,
        on[0].load(Ordering::Relaxed),
        mb(lightcraft_gpu::batch_rate_of(Role::Primary)),
        on[1].load(Ordering::Relaxed),
        mb(lightcraft_gpu::batch_rate_of(Role::Helper)),
        on[2].load(Ordering::Relaxed),
    );
}
