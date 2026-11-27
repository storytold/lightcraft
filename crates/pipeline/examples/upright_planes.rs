//! Upright detector on saved analysis planes (a developer tool, not part of the app): reads the `.lum` planes
//! that `lightcraft-engine`'s `upright_eval` example saves, runs the line finder and the solver at the app's
//! analysis size and writes one JSON line per plane (the Vertical / Auto / Full transforms and why a mode found
//! nothing), so a change to the detector can be scored against the photographer's own corrections quickly.
//!
//! `cargo run --release -p lightcraft-pipeline --example upright_planes -- <planes dir> <out.jsonl>`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;

use lightcraft_develop::Upright;
use lightcraft_pipeline::upright;
use lightcraft_raster::Plane;

fn read_plane(path: &std::path::Path) -> Plane {
    let b = std::fs::read(path).unwrap();
    let w = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
    let h = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
    let data = b[8..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    Plane { width: w, height: h, data }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::fs::File::create(&args[1]).unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(&args[0]).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.retain(|p| p.extension().is_some_and(|e| e == "lum"));
    paths.sort();
    for p in paths {
        let key = p.file_stem().unwrap().to_string_lossy().to_string();
        let plane = read_plane(&p);
        let size = std::env::var("UPRIGHT_SIZE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0).unwrap_or(1024);
        let a = upright::analyze_plane_at(&plane, size);
        let mut line = serde_json::json!({"key": key});
        for (name, mode) in [("vertical", Upright::Vertical), ("auto", Upright::Auto), ("full", Upright::Full)] {
            line[name] = serde_json::json!({"transform": upright::solve(mode, &a), "note": upright::shortfall(mode, &a)});
        }
        writeln!(out, "{line}").unwrap();
        // UPRIGHT_DUMP=dir: the near-vertical lines and the vanishing point, to draw over the photo
        if let Ok(dir) = std::env::var("UPRIGHT_DUMP") {
            let segs: Vec<[f64; 4]> = a.vertical.iter().map(|s| [s.a.x, s.a.y, s.b.x, s.b.y]).collect();
            let h: Vec<[f64; 4]> = a.horizontal.iter().map(|s| [s.a.x, s.a.y, s.b.x, s.b.y]).collect();
            let d = serde_json::json!({"vertical": segs, "horizontal": h, "vp": a.vertical_vp.or(upright::vanishing_point(&a.vertical).map(|x| x.0)), "why": a.vertical_why.map(|w| w.message())});
            std::fs::write(std::path::Path::new(&dir).join(format!("{key}.json")), d.to_string()).unwrap();
        }
    }
}
