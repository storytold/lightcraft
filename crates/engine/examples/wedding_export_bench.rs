//! A wedding-style export benchmark: full-size HDR gain-map JPEGs of the raws given, each edited
//! like a real wedding photo (tone, clarity, noise reduction, sharpening and a radial mask), as the
//! Export dialog runs them (several photos side by side). Prints photos per minute.
//!
//! `cargo run --release -p lightcraft-engine --example wedding_export_bench -- [--cpu] [--sdr] [--plain]
//! [--lanes N] [--runs N] [--gpu-jobs N] [--gpu-mb MB] [--long-edge PX] [--limit-kb KB]
//! [--helper on|off|only] [--helper-card SETTING] [--mix] FILES…`
//!
//! - `--cpu`: no GPU (as after a device failure); `--sdr`: plain JPEGs, no gain map; `--plain`:
//!   no mask; `--lanes N`: photos side by side (default: the app's choice);
//!   `--long-edge PX`, `--limit-kb KB`: resize and "Limit file size" as in the Export dialog
//!   (note 28's settings: `--long-edge 6000 --limit-kb 10240`).
//! - Two graphics cards: `--helper on` (default) exports on the second card and the editing card,
//!   `off` on the editing card alone, `only` on the second card alone. `--helper-card` picks the
//!   second card as Settings does (`auto`, `off`, `same` = a second device on the editing card, a
//!   key or part of a name; default `auto`); `LIGHTCRAFT_GPU_ADAPTER` picks the editing card.
//!   `--mix`: photos both cards are too busy for render on the processor.
//! - Each file's `gpuDevice` is counted per card; `LIGHTCRAFT_PROFILE=1` prints each render's stages on stderr.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{Destination, ExportCards, ExportOptions, export_batch};
use lightcraft_engine::gpu::Role;

/// Flags followed by a value.
const VALUED: [&str; 8] = ["--lanes", "--runs", "--gpu-jobs", "--gpu-mb", "--long-edge", "--limit-kb", "--helper", "--helper-card"];

fn main() {
    // as the apps do: the export gate follows the computer's memory
    lightcraft_engine::memory::set_ram_probe(lightcraft_sysmem::total_physical_memory);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let text = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let value = |f: &str| text(f).and_then(|v| v.parse::<usize>().ok());
    let files: Vec<String> = {
        let mut skip = false;
        args.iter()
            .filter(|a| {
                let keep = !skip && !a.starts_with("--");
                skip = VALUED.contains(&a.as_str());
                keep
            })
            .cloned()
            .collect()
    };
    if files.is_empty() && !flag("--test-machine") {
        eprintln!(
            "usage: wedding_export_bench [--cpu] [--sdr] [--plain] [--lanes N] [--runs N] [--gpu-jobs N] [--gpu-mb MB] [--helper on|off|only] [--helper-card SETTING] [--mix] FILES… | --test-machine [--quick] [FILES…]"
        );
        std::process::exit(2);
    }
    if let Some(n) = value("--lanes") {
        lightcraft_engine::export::set_export_lanes(Some(n));
    }
    let gpu = !flag("--cpu");
    lightcraft_engine::gpu::set_enabled(gpu);
    let cards = match text("--helper") {
        Some(h) => ExportCards::parse(&h).unwrap_or_else(|| {
            eprintln!("--helper takes on, off or only, not {h}");
            std::process::exit(2)
        }),
        None => ExportCards::Both,
    };
    lightcraft_engine::export::set_export_cards(cards);
    if gpu {
        // the editing card's device, then the second card's, as the app's start-up makes them
        // (the cards, listed once: a card asked for by name is looked up in it)
        for c in lightcraft_engine::gpu::list_adapters() {
            let screen = if c.drives_screen { ", drives the screen" } else { "" };
            eprintln!("card: {} ({}, {} MB{screen}){}", c.name, c.key, c.memory.unwrap_or(0) >> 20, if c.usable { "" } else { " not usable" });
        }
        if let Err(e) = lightcraft_engine::gpu::set_helper(&text("--helper-card").unwrap_or_else(|| "auto".into())) {
            eprintln!("--helper-card: {e}");
            std::process::exit(2);
        }
        lightcraft_engine::gpu::warm_up();
        let _ = lightcraft_engine::gpu::available();
        // (the warm-up thread starts the second card's set-up after the editing card's: until then
        // `wait_helper` has nothing to wait for, so wait for its state to settle too)
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(300)
            && (!lightcraft_engine::gpu::wait_helper(Duration::from_secs(1))
                || matches!(lightcraft_engine::gpu::helper_status().state, "notStarted" | "starting"))
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("second card set up in {:.1} s", t.elapsed().as_secs_f64());
        let h = lightcraft_engine::gpu::helper_status();
        eprintln!(
            "editing card: {:?}; second card: {} {:?} {}",
            lightcraft_engine::gpu::card_in_use(),
            h.state,
            h.name,
            h.reason.unwrap_or_default()
        );
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
                "  {} lanes, {}{}: {:.1} photos/min ({} photos in {:.1} s)",
                run.lanes,
                if run.gpu { "GPU" } else { "CPU" },
                if run.helper { " + second card" } else { "" },
                run.photos_per_min,
                run.photos,
                run.seconds
            );
        }
        println!("{}\n(test took {:.0} s)", r.summary, t.elapsed().as_secs_f64());
        return;
    }
    if value("--gpu-jobs").is_some() || value("--gpu-mb").is_some() {
        let (bytes, jobs) = (value("--gpu-mb").map(|m| (m as u64) << 20), value("--gpu-jobs"));
        lightcraft_engine::gpu::set_batch_limits(bytes, jobs);
        lightcraft_engine::gpu::set_batch_limits_for(Role::Helper, bytes, jobs);
    }
    lightcraft_engine::export::set_export_mix(flag("--mix"));
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
        // (a value past u32 is refused, not cut to another size: review of PR #3565)
        let Ok(edge) = u32::try_from(edge) else {
            eprintln!("--long-edge {edge}: too large");
            std::process::exit(2);
        };
        o.resize = Some(lightcraft_engine::export::Resize::long_edge(edge));
    }
    o.limit_kb = value("--limit-kb").map(|kb| kb as u32);
    let lanes = lightcraft_engine::export::export_parallelism(ids.len());
    println!(
        "{} photos, {lanes} lanes, {}, cards {cards:?}{}, {}, {}, memory budget {} MB",
        ids.len(),
        if gpu { lightcraft_engine::gpu::adapter_name().unwrap_or_else(|| "no GPU".into()) } else { "CPU only".into() },
        if flag("--mix") { " + processor" } else { "" },
        if hdr { "HDR gain map" } else { "SDR" },
        if flag("--plain") { "no mask" } else { "a radial mask" },
        lightcraft_engine::memory::budget() >> 20
    );
    for run in 0..value("--runs").unwrap_or(1) {
        let t = Instant::now();
        let mut fell_back = 0;
        let mut per_card: std::collections::BTreeMap<String, usize> = Default::default();
        let files = export_batch(&mut session, &ids, &o, &Destination::default(), &mut |_, _| Ok(()), &|_| false).expect("export");
        for f in &files {
            if let Some(why) = f.get("gpuFallback") {
                fell_back += 1;
                eprintln!("  gpu fallback: {why}");
            }
            if let Some(e) = f.get("error") {
                eprintln!("  error: {e}");
            }
            let card = f.get("gpuDevice").and_then(|c| c.as_str()).unwrap_or("processor").to_string();
            *per_card.entry(card).or_default() += 1;
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "run {run}: {} files in {secs:.1} s = {:.1} photos/min ({:.2} s each){}; rendered by {per_card:?}",
            files.len(),
            files.len() as f64 * 60.0 / secs,
            secs / files.len().max(1) as f64,
            if fell_back > 0 { format!(", {fell_back} fell back to the CPU") } else { String::new() }
        );
        if gpu {
            for role in Role::ALL {
                let b = lightcraft_engine::gpu::batch_usage_of(role);
                let m = lightcraft_engine::gpu::memory_of(role);
                println!(
                    "  {}: card {} MB, batch limit {} MB / {} jobs, {} waits ({} ms), peak {} MB",
                    role.id(),
                    lightcraft_engine::gpu::adapter_memory_of(role).unwrap_or(0) >> 20,
                    b.limit_bytes >> 20,
                    b.limit_jobs,
                    b.waits,
                    b.waited_ms,
                    m.peak >> 20,
                );
            }
            println!("  {} restarts", lightcraft_engine::gpu::recoveries());
        }
        for &id in &ids {
            session.media.forget(id);
        }
    }
}
