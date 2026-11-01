//! What a Synchronize Folder scan costs on a folder whose photos are all in the library and
//! unchanged: the walk, and the check of every photo and its sidecar. Reads the folder; writes
//! nothing there, and probes no file (the library is made from the folder's own listing).
//!
//! `cargo run --release -p lightcraft-engine --example sync_scan_bench -- <folder>`
use std::time::Instant;

use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::Session;
use lightcraft_engine::sync::{SyncInput, SyncProgress, scan_with};

fn main() {
    let Some(folder) = std::env::args().nth(1) else {
        eprintln!("usage: sync_scan_bench <folder>");
        return;
    };
    let mut s = Session::new();
    let t = Instant::now();
    let files = lightcraft_engine::import::expand(std::slice::from_ref(&folder), None);
    println!("{} files listed in {:.1} s (the library is made of them)", files.len(), t.elapsed().as_secs_f64());
    for f in &files {
        let id = s.catalog.alloc_photo_id();
        let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let p = Photo::new(id, Source::File { path: f.clone() }, &name, "JPEG", 6, 4, "2026-01-01T00:00:00");
        if s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).is_err() {
            eprintln!("could not add {name}");
        }
    }
    for run in 0..2 {
        let input = match SyncInput::new(&mut s, &folder, true) {
            Ok(input) => input,
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        };
        let t = Instant::now();
        let changes = scan_with(input, &SyncProgress::default());
        println!(
            "run {run}: scan {:.1} s ({} new, {} missing, {} with sidecar news)",
            t.elapsed().as_secs_f64(),
            changes.new.len(),
            changes.missing.len(),
            changes.metadata.len()
        );
    }
}
