//! What a scan, an import and Synchronize Folder cost before they read the disk, and what
//! naming one photo's sidecar costs, by the size of the library (`N`, default 100,000 photos; the
//! folder looked at holds 500 of them).
//!
//! `N=3500000 cargo run --release -p lightcraft-engine --example scan_setup_bench`
use std::time::Instant;

use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::Session;

fn main() {
    let n: u64 = std::env::var("N").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let mut s = Session::new();
    let t = Instant::now();
    for chunk in (0..n).collect::<Vec<_>>().chunks(10_000) {
        for i in chunk {
            let id = s.catalog.alloc_photo_id();
            let name = format!("IMG_{i:08}.jpg");
            let path = format!("/Volumes/Photos/Library/Archive/2019/2019-07 Summer/Trip to the coast/Day {:05}/extra/{name}", i / 500);
            let mut p = Photo::new(id, Source::File { path }, &name, "JPEG", 4032, 3024, "2026-09-30T12:00:00");
            p.file_size = 3_400_000 + i;
            p.captured = Some(format!("2019-07-{:02}T10:{:02}:{:02}", 1 + i % 28, i % 60, (i / 60) % 60));
            p.content_hash = Some(format!("{:032x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
    }
    eprintln!("{n} photos built in {:.2} s", t.elapsed().as_secs_f64());
    let folder = "/Volumes/Photos/Library/Archive/2019/2019-07 Summer/Trip to the coast/Day 00000".to_string();
    for run in 0..3 {
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let f = lightcraft_catalog::Filter { library_folder: Some(folder.clone()), ..Default::default() };
        let ids = s.catalog.query(&f, &lightcraft_catalog::Sort::default());
        let query = ms(t);
        let t = Instant::now();
        let namings = s.sidecar_namings(&ids);
        let naming = ms(t);
        let t = Instant::now();
        let one = ids.first().map(|id| s.sidecar_naming(*id));
        let naming_one = ms(t);
        let t = Instant::now();
        let (input, paths) = lightcraft_engine::import::ScanInput::new(&mut s, std::slice::from_ref(&folder));
        let scan = ms(t);
        // (the folder is not on this disk: the scan finds no file, and its cost is what it makes of the library)
        let t = Instant::now();
        let out = lightcraft_engine::import::scan_with(input, &paths, &Default::default());
        let scanned = ms(t);
        let t = Instant::now();
        let job = lightcraft_engine::import::ImportJob::new(&mut s, Default::default());
        let import = ms(t);
        let t = Instant::now();
        let sync = lightcraft_engine::sync::SyncInput::new(&mut s, &folder, false);
        let whole = ms(t);
        println!(
            "N={n} run {run}: query {query:.0} ms ({} ids) | sidecar_namings {naming:.0} ms | sidecar_naming of one {naming_one:.0} ms ({one:?}) | ScanInput::new {scan:.0} ms + scan_with {scanned:.0} ms ({} found) | ImportJob::new {import:.0} ms (ok {}) | SyncInput::new {whole:.0} ms (ok {})",
            ids.len(),
            out.candidates.len(),
            job.is_ok(),
            sync.is_ok()
        );
        let _ = namings;
    }
}
