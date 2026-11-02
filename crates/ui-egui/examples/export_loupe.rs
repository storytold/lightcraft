//! Opening photos in the loupe while an export runs (report on PR #3530: the loupe took 0.8–11.4 s
//! to open during a 61 MP export): the real app, headless, with the GPU on.
//!
//! `cargo run --release -p lightcraft-ui-egui --example export_loupe -- LIBRARY_DIR OUT_DIR N_OPEN FILES…`
//! The first `N_OPEN` files are opened in the loupe one after another (each never shown before):
//! the first before the export, as the baseline, the others while the rest of FILES export
//! (JPEG, long edge `LONG_EDGE`=6000, Limit file size `LIMIT_KB`=5120). Between opens the view goes
//! back to the grid for `PAUSE_MS`=3000. Env `GPU=0`: CPU pipeline; `GPU_MB`, `GPU_JOBS`: the
//! export's GPU budget (as `wedding_export_bench --gpu-mb/--gpu-jobs`).
//! Prints each open's time to the finished render, and every change of the export row's count.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use lightcraft_engine::Session;
use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::render::Slot;
use lightcraft_ui_egui::{LightcraftApp, Services};
use serde_json::json;

const T: Duration = Duration::from_secs(60);

fn export_done(h: &Headless) -> Option<u64> {
    h.app.session.activity.list().iter().find(|t| t.kind == "export").map(|t| t.done)
}

fn main() {
    lightcraft_engine::memory::set_ram_probe(lightcraft_sysmem::total_physical_memory);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (lib, out) = (&args[0], &args[1]);
    let n_open: usize = args[2].parse().unwrap();
    let files: Vec<String> = args[3..].to_vec();
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let gpu = env("GPU", "1") != "0";
    let long_edge: u32 = env("LONG_EDGE", "6000").parse().unwrap();
    let limit_kb: u32 = env("LIMIT_KB", "5120").parse().unwrap();
    let pause = Duration::from_millis(env("PAUSE_MS", "3000").parse().unwrap());
    if let (Ok(mb), Ok(jobs)) = (std::env::var("GPU_MB"), std::env::var("GPU_JOBS")) {
        lightcraft_engine::gpu::set_batch_limits(Some(mb.parse::<u64>().unwrap() << 20), Some(jobs.parse().unwrap()));
    }

    let mut s = Session::new().with_fs().with_system_clock();
    s.open_library(lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": files})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).collect();
    assert_eq!(ids.len(), files.len(), "a fresh library: {r}");
    let (opens, exports) = ids.split_at(n_open);
    let mut app =
        LightcraftApp::new(s, Services { write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)), ..Services::default() });
    app.ui.settings.gpu = gpu;
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    let cmd = |h: &mut Headless, c: &str, p: serde_json::Value| h.request("engine.execute", json!({"command": c, "params": p}), T);
    h.request("ui.set", json!({"view": "grid"}), T);
    h.settle(Duration::from_secs(120));

    let t0 = Instant::now();
    let mut last_done = None;
    let mut track = |h: &Headless| {
        let d = export_done(h);
        if d != last_done {
            eprintln!("{:8.1} s export row {:?}", t0.elapsed().as_secs_f64(), d);
            last_done = d;
        }
    };
    let mut results = Vec::new();
    for (k, &id) in opens.iter().enumerate() {
        if k == 1 {
            // start the export of the rest
            cmd(&mut h, "library.select", json!({"ids": exports}));
            cmd(&mut h, "dialog.export", json!({}));
            h.step();
            if let Some(lightcraft_ui_egui::state::Dialog::Export { full_size, resize, dir, limit_kb: lim, .. }) = &mut h.app.ui.dialog {
                *full_size = false;
                *resize = lightcraft_engine::export::Resize::long_edge(long_edge);
                *lim = limit_kb;
                *dir = out.clone();
            }
            let r = h.request("ui.dialog.confirm", json!({}), T);
            assert_eq!(r["ok"], true, "{r}");
            eprintln!("{:8.1} s export of {} started", t0.elapsed().as_secs_f64(), exports.len());
            let p = Instant::now();
            while p.elapsed() < pause {
                h.step();
                track(&h);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let exporting = h.app.export.is_some();
        let pid = lightcraft_engine::catalog::PhotoId(id);
        let start = Instant::now();
        cmd(&mut h, "library.select", json!({"ids": [id]}));
        h.request("ui.set", json!({"view": "detail"}), T);
        let mut first = None;
        let ms = loop {
            h.step();
            track(&h);
            let r = &h.app.renderer;
            let tex = r.textures.get(&Slot::Main).filter(|t| t.photo == pid);
            if first.is_none() && tex.is_some() {
                first = Some(start.elapsed().as_secs_f64() * 1e3);
            }
            if tex.is_some_and(|t| Some(t.key) == r.wanted(Slot::Main)) && !r.is_pending(Slot::Main) {
                break start.elapsed().as_secs_f64() * 1e3;
            }
            if start.elapsed() > Duration::from_secs(120) {
                break f64::NAN;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        eprintln!(
            "{:8.1} s open {k}: first picture {:.0} ms, finished render {ms:.0} ms (export running: {exporting}, row {:?}, texture {:?})",
            t0.elapsed().as_secs_f64(),
            first.unwrap_or(f64::NAN),
            export_done(&h),
            h.app.renderer.textures.get(&Slot::Main).map(|t| t.size)
        );
        results.push((k, exporting, first.unwrap_or(f64::NAN), ms));
        h.request("ui.set", json!({"view": "grid"}), T);
        let p = Instant::now();
        while p.elapsed() < pause {
            h.step();
            track(&h);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    while h.app.export.is_some() && t0.elapsed() < Duration::from_secs(900) {
        h.step();
        track(&h);
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("{:8.1} s export finished: {:?}", t0.elapsed().as_secs_f64(), h.app.last_export_result.as_ref().map(|r| r["exported"].clone()));
    println!("open,during_export,first_ms,finished_ms");
    for (k, e, f, m) in results {
        println!("{k},{e},{f:.0},{m:.0}");
    }
}
