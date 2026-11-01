//! Upright on real photos (a developer tool, not part of the app): imports each photo of a manifest into an
//! in-memory library, applies its lens settings, runs Upright the way the app does and writes one JSON line per
//! photo (the transform each mode finds and why a mode found nothing). With `--planes DIR` it also saves the
//! analysis plane of each photo at 2048 px (`<key>.lum`: u32 width, u32 height, f32 samples, little endian), so
//! the detector can be tuned on them without decoding raws again (`lightcraft-pipeline` example
//! `upright_planes`).
//!
//! `cargo run --release -p lightcraft-engine --example upright_eval -- <manifest.json> <out.jsonl> [--planes DIR]`
//!
//! The manifest is a JSON array of `{key, local, optics?, orientation?}` (`local`: a copy of the photo; nothing
//! is written next to it). Ground truth and scoring live outside the repository.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::media::SourceLevel;
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let manifest: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&args[0]).unwrap()).unwrap();
    let mut out = std::fs::File::create(&args[1]).unwrap();
    let planes = args.iter().position(|a| a == "--planes").map(|i| std::path::PathBuf::from(&args[i + 1]));
    if let Some(p) = &planes {
        std::fs::create_dir_all(p).unwrap();
    }
    for m in &manifest {
        // a fresh in-memory library per photo, so a few hundred raws fit
        let mut s = Session::new().with_fs();
        let key = m["key"].as_str().unwrap().to_string();
        let local = m["local"].as_str().unwrap();
        let r = match s.execute("library.import", &json!({"paths": [local]})) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{key}: {e}");
                continue;
            }
        };
        let Some(id) = r["imported"][0].as_u64() else {
            eprintln!("{key}: not imported");
            continue;
        };
        s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
        let mut partial = serde_json::Map::new();
        for k in ["optics", "orientation"] {
            if let Some(v) = m.get(k).filter(|v| !v.is_null()) {
                partial.insert(k.into(), v.clone());
            }
        }
        if !partial.is_empty()
            && let Err(e) = s.execute("develop.merge", &json!({"settings": partial}))
        {
            eprintln!("{key}: lens settings: {e}");
        }
        let mut line = json!({"key": key});
        for mode in ["vertical", "auto", "full", "level"] {
            match s.execute("geometry.upright", &json!({"mode": mode})) {
                Ok(r) => line[mode] = json!({"transform": r["transform"], "note": r["note"]}),
                Err(e) => line[mode] = json!({"error": e.to_string()}),
            }
        }
        if let Some(dir) = &planes {
            let d = s.develop_of(PhotoId(id)).unwrap_or_default();
            let src = s.source_now(PhotoId(id), SourceLevel::Preview).unwrap();
            let info = s.source_info(PhotoId(id));
            let lum = lightcraft_pipeline::upright::analysis_luminance(&src, &info, &d, 2048);
            let mut bytes = Vec::with_capacity(8 + lum.data.len() * 4);
            bytes.extend_from_slice(&(lum.width as u32).to_le_bytes());
            bytes.extend_from_slice(&(lum.height as u32).to_le_bytes());
            for v in &lum.data {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(dir.join(format!("{key}.lum")), bytes).unwrap();
            line["source"] = json!([src.width, src.height]);
        }
        writeln!(out, "{line}").unwrap();
        eprintln!("{key} done");
    }
}
