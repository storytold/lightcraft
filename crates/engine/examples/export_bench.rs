//! Single-photo export benchmark (median and minimum of N runs):
//! `cargo run --release -p lightcraft-engine --example export_bench -- corpus/raw/arw-sony-a7m3-compressed.arw`
//!
//! Imports the files given and runs the export path the app, CLI and MCP share
//! ([`lightcraft_engine::export::export_photo`]: decode the original, render it full size, encode)
//! once per file per run, forgetting the decoded original between runs so each export reads and
//! decodes it like a batch export does. `N=9` runs (default), `OPT='{"format":"png"}'` for other
//! export options (JSON as `app.export` takes them), `LIGHTCRAFT_GPU=0` for the CPU pipeline,
//! `PHASES=1` to time render and encode separately, `OUT=dir` to write each file's export there
//! (for byte comparisons between builds).
use std::time::Instant;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{ExportOptions, encode_rendered, export_metadata, export_photo};

fn stats(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[0])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(!args.is_empty(), "usage: export_bench FILE...");
    let n: usize = std::env::var("N").ok().and_then(|v| v.parse().ok()).unwrap_or(9).max(1);
    let opt: serde_json::Value =
        std::env::var("OPT").ok().map(|s| serde_json::from_str(&s).expect("OPT is JSON")).unwrap_or_else(|| serde_json::json!({}));
    let o = ExportOptions::from_params(&opt).expect("export options");
    let mut s = Session::new().with_fs();
    let paths: Vec<String> = args.iter().map(|p| std::fs::canonicalize(p).expect("input file").to_string_lossy().to_string()).collect();
    let r = s.execute("library.import", &serde_json::json!({"paths": paths})).expect("import");
    let ids: Vec<PhotoId> = r["imported"].as_array().expect("ids").iter().filter_map(|v| v.as_u64()).map(PhotoId).collect();
    assert_eq!(ids.len(), args.len(), "every file imports");
    println!("{} file(s), {:?}, {n} runs, gpu {}", ids.len(), o.format, lightcraft_engine::gpu::available());
    let mut times = Vec::new();
    for i in 0..=n {
        let t = Instant::now();
        for (k, &id) in ids.iter().enumerate() {
            let e = export_photo(&mut s, id, &o, k + 1).expect("export");
            if i == 0
                && let Some(dir) = std::env::var_os("OUT")
            {
                std::fs::write(std::path::Path::new(&dir).join(&e.file_name), &e.bytes).expect("write");
                println!("  {} {}×{} {} bytes", e.file_name, e.width, e.height, e.bytes.len());
            }
            s.media.forget(id);
        }
        // run 0 warms up (GPU device, allocator, page cache)
        if i > 0 {
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    let (med, min) = stats(times.clone());
    println!("export: median {med:.1} ms, min {min:.1} ms  (runs: {})", times.iter().map(|t| format!("{t:.0}")).collect::<Vec<_>>().join(" "));
    if std::env::var_os("PHASES").is_none() {
        return;
    }
    let (mut render, mut encode) = (Vec::new(), Vec::new());
    for _ in 0..n {
        let (mut tr, mut te) = (0.0, 0.0);
        for &id in &ids {
            let p = s.catalog.photo(id).expect("photo").clone();
            let (w, h) = lightcraft_engine::export::output_size(&p, &o);
            let meta = export_metadata(&p, &s.catalog, &o);
            let job = s.export_job(id, w, h, o.effective_space(), o.effective_depth()).expect("job");
            let t = Instant::now();
            let r = job.run().rendered.expect("render");
            tr += t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            std::hint::black_box(encode_rendered(&r, &o, meta.as_ref()).expect("encode"));
            te += t.elapsed().as_secs_f64() * 1e3;
            s.media.forget(id);
        }
        render.push(tr);
        encode.push(te);
    }
    let (rm, rn) = stats(render);
    let (em, en) = stats(encode);
    println!("  decode+render: median {rm:.1} ms, min {rn:.1} ms");
    println!("  encode:        median {em:.1} ms, min {en:.1} ms");
}
