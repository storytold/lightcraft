//! Times the "never write over an original" check on export targets that already exist (#3533).
//!
//! `cargo run --release -p lightcraft-engine --example original_guard_bench -- [originals] [targets]`
//!
//! Builds a library of `originals` real files (default 20000) in a temp folder and `targets`
//! existing export files (default 20), then times `OriginalGuard::check` on each target, as an
//! export with "overwrite" does for every file it places.

use std::path::Path;
use std::time::Instant;

use lightcraft_catalog::{Catalog, Op, Photo, PhotoId, Source};
use lightcraft_engine::originals::OriginalGuard;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).map(|a| a.parse::<u64>());
    let originals = args.next().transpose()?.unwrap_or(20_000);
    let targets = args.next().transpose()?.unwrap_or(20);

    let d = std::env::temp_dir().join(format!("lc-guard-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("lib"))?;
    std::fs::create_dir_all(d.join("out"))?;

    let t = Instant::now();
    let mut c = Catalog::default();
    for i in 0..originals {
        // 100 folders, like a library sorted by date
        let folder = d.join("lib").join(format!("{:03}", i % 100));
        std::fs::create_dir_all(&folder)?;
        let path = folder.join(format!("IMG_{i}.jpg"));
        std::fs::write(&path, b"original")?;
        let p = Photo::new(
            PhotoId(i + 1),
            Source::File { path: path.to_string_lossy().to_string() },
            &format!("IMG_{i}.jpg"),
            "JPEG",
            10,
            10,
            "2026-01-01",
        );
        c.apply(Op::AddPhoto { photo: Box::new(p) })?;
    }
    let out: Vec<_> = (0..targets).map(|i| d.join("out").join(format!("export_{i}.jpg"))).collect();
    for p in &out {
        std::fs::write(p, b"previous export")?;
    }
    println!("setup: {originals} originals ({} protected paths), {targets} existing targets in {:.1?}", originals.saturating_mul(3), t.elapsed());

    let g = OriginalGuard::new(&c);
    let mut first = None;
    let t = Instant::now();
    for p in &out {
        let one = Instant::now();
        check(&g, p)?;
        first.get_or_insert(one.elapsed());
    }
    let total = t.elapsed();
    let first = first.unwrap_or_default();
    println!(
        "check (overwrite) x{targets}: total {total:.2?}, first {first:.2?}, per target after the first {:.2?}",
        total.saturating_sub(first) / targets.saturating_sub(1).max(1) as u32
    );

    let _ = std::fs::remove_dir_all(&d);
    Ok(())
}

fn check(g: &OriginalGuard, p: &Path) -> Result<(), Box<dyn std::error::Error>> {
    g.check(p).map_err(|e| e.into())
}
