//! A wedding-style export benchmark: full-size HDR gain-map JPEGs of the raws given, each edited
//! like a real wedding photo (tone, clarity, noise reduction, sharpening and a radial mask), as the
//! Export dialog runs them (several photos side by side). Prints photos per minute.
//!
//! `cargo run --release -p lightcraft-engine --example wedding_export_bench -- [--cpu] [--sdr] [--plain]
//! [--lanes N] [--runs N] [--gpu-jobs N] [--gpu-mb MB] [--long-edge PX] [--limit-kb KB] FILES…`
//!
//! - `--cpu`: no GPU (as after a device failure); `--sdr`: plain JPEGs, no gain map; `--plain`:
//!   no mask; `--lanes N`: photos side by side (default: the app's choice);
//!   `--long-edge PX`, `--limit-kb KB`: resize and "Limit file size" as in the Export dialog
//!   (note 28's settings: `--long-edge 6000 --limit-kb 10240`).
//! - `LIGHTCRAFT_PROFILE=1` prints each render's stages on stderr.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{Destination, ExportOptions, export_batch};

fn main() {
    // as the apps do: the export gate follows the computer's memory
    lightcraft_engine::memory::set_ram_probe(lightcraft_sysmem::total_physical_memory);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<usize>().ok());
    let files: Vec<String> = {
        let mut skip = false;
        args.iter()
            .filter(|a| {
                let keep = !skip && !a.starts_with("--");
                skip = matches!(a.as_str(), "--lanes" | "--runs" | "--gpu-jobs" | "--gpu-mb" | "--long-edge" | "--limit-kb");
                keep
            })
            .cloned()
            .collect()
    };
    if files.is_empty() && !flag("--test-machine") {
        eprintln!(
            "usage: wedding_export_bench [--cpu] [--sdr] [--plain] [--lanes N] [--runs N] [--gpu-jobs N] [--gpu-mb MB] FILES… | --test-machine [--quick] [FILES…]"
        );
        std::process::exit(2);
    }
    if let Some(n) = value("--lanes") {
        lightcraft_engine::export::set_export_lanes(Some(n));
    }
    if flag("--test-machine") {
        // "Test my machine" as the app runs it, with these files (none: the demo photos)
        let t = Instant::now();
        let o = lightcraft_engine::machine_test::TestOptions { paths: files.clone(), quick: flag("--quick"), ..Default::default() };
        let r = lightcraft_engine::machine_test::run_test(&o, &mut |d, n, what| {
            eprintln!("  {d}/{n} {what} ({:.0} s)", t.elapsed().as_secs_f64());
            true
        })
        .expect("test");
        for run in &r.runs {
            println!(
                "  {} lanes, {}: {:.1} photos/min ({} photos in {:.1} s)",
                run.lanes,
                if run.gpu { "GPU" } else { "CPU" },
                run.photos_per_min,
                run.photos,
                run.seconds
            );
        }
        println!("{}\n(test took {:.0} s)", r.summary, t.elapsed().as_secs_f64());
        return;
    }
    if value("--gpu-jobs").is_some() || value("--gpu-mb").is_some() {
        lightcraft_engine::gpu::set_batch_limits(value("--gpu-mb").map(|m| (m as u64) << 20), value("--gpu-jobs"));
    }
    let gpu = !flag("--cpu");
    lightcraft_engine::gpu::set_enabled(gpu);
    let hdr = !flag("--sdr");
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &serde_json::json!({"paths": files})).expect("import");
    let ids: Vec<_> = r["imported"].as_array().expect("ids").iter().filter_map(|v| v.as_u64()).map(PhotoId).collect();
    let settings = lightcraft_engine::machine_test::bench_settings(flag("--plain"), hdr);
    for &id in &ids {
        session.set_develop(id, settings.clone(), "bench").expect("settings");
    }
    let mut o = ExportOptions { hdr, quality: 90, ..ExportOptions::default() };
    if let Some(edge) = value("--long-edge") {
        o.resize = Some(lightcraft_engine::export::Resize::long_edge(edge as u32));
    }
    o.limit_kb = value("--limit-kb").map(|kb| kb as u32);
    let lanes = lightcraft_engine::export::export_parallelism(ids.len());
    println!(
        "{} photos, {lanes} lanes, {}, {}, {}, memory budget {} MB",
        ids.len(),
        if gpu { lightcraft_engine::gpu::adapter_name().unwrap_or_else(|| "no GPU".into()) } else { "CPU only".into() },
        if hdr { "HDR gain map" } else { "SDR" },
        if flag("--plain") { "no mask" } else { "a radial mask" },
        lightcraft_engine::memory::budget() >> 20
    );
    for run in 0..value("--runs").unwrap_or(1) {
        let t = Instant::now();
        let mut fell_back = 0;
        let files = export_batch(&mut session, &ids, &o, &Destination::default(), &mut |_, _| Ok(()), &|_| false).expect("export");
        for f in &files {
            if let Some(why) = f.get("gpuFallback") {
                fell_back += 1;
                eprintln!("  gpu fallback: {why}");
            }
            if let Some(e) = f.get("error") {
                eprintln!("  error: {e}");
            }
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "run {run}: {} files in {secs:.1} s = {:.1} photos/min ({:.2} s each){}",
            files.len(),
            files.len() as f64 * 60.0 / secs,
            secs / files.len().max(1) as f64,
            if fell_back > 0 { format!(", {fell_back} fell back to the CPU") } else { String::new() }
        );
        if gpu {
            let b = lightcraft_engine::gpu::batch_usage();
            let m = lightcraft_engine::gpu::memory();
            println!(
                "  gpu: card {} MB, batch limit {} MB / {} jobs, {} waits ({} ms), peak {} MB, {} restarts",
                lightcraft_engine::gpu::adapter_memory().unwrap_or(0) >> 20,
                b.limit_bytes >> 20,
                b.limit_jobs,
                b.waits,
                b.waited_ms,
                m.peak >> 20,
                lightcraft_engine::gpu::recoveries()
            );
        }
        for &id in &ids {
            session.media.forget(id);
        }
    }
}
