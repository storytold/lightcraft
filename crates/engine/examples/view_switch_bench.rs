//! What switching the view costs in a big library, on a synthetic catalog (no files): a click on a sidebar source,
//! a sort or filter change, and the follow of an import batch while a view is shown. Times the engine calls the UI
//! thread makes (`library.source` and friends, `Session::visible`), not a frame.
//!
//! `cargo run --release -p lightcraft-engine --example view_switch_bench -- [photos] [runs]`
use std::time::{Duration, Instant};

use lightcraft_catalog::{Flag, Op, Photo, Source};
use lightcraft_engine::Session;
use serde_json::{Value, json};

const FOLDERS: usize = 2_000;
const IMPORT_DAYS: usize = 400;

fn photo(s: &mut Session, i: usize, imported: &str) -> Photo {
    let id = s.catalog.alloc_photo_id();
    let name = format!("IMG_{:05}.jpg", (i * 7919) % 100_000);
    let mut p = Photo::new(id, Source::File { path: format!("/pics/f{}/{name}", i % FOLDERS) }, &name, "JPEG", 60, 40, imported);
    p.captured = Some(format!("{}-{:02}-{:02}T10:{:02}:{:02}", 2000 + i % 25, 1 + i % 12, 1 + i % 28, i % 60, (i / 60) % 60));
    p.rating = (i % 6) as u8;
    p.file_size = (i as u64 * 31) % 5_000_000;
    if i.is_multiple_of(40) {
        p.flag = Flag::Pick;
    }
    p
}

fn build(n: usize) -> Session {
    let mut s = Session::new();
    let mut i = 0;
    while i < n {
        let chunk = (n - i).min(10_000);
        let ops = (i..i + chunk)
            .map(|k| {
                let day = (k * IMPORT_DAYS) / n;
                let imported = format!("2025-{:02}-{:02}T{:02}:00:00", 1 + (day / 28) % 12, 1 + day % 28, (k / 997) % 24);
                Op::AddPhoto { photo: Box::new(photo(&mut s, k, &imported)) }
            })
            .collect();
        s.catalog.apply(Op::Batch { ops }).expect("add");
        i += chunk;
    }
    s
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn run(s: &mut Session, cmd: &str, p: Value) -> f64 {
    let t = Instant::now();
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    ms(t.elapsed())
}

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(500_000);
    let runs: usize = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(3);
    let t = Instant::now();
    let mut s = build(n);
    println!("{n} photos built in {:.1} s", t.elapsed().as_secs_f64());
    let album = s.execute("album.create", &json!({"name": "Plain"})).expect("album")["id"].as_u64().expect("id");
    let ids: Vec<u64> = (0..n as u64 / 20).map(|k| k * 20 + 1).collect();
    s.execute("album.addPhotos", &json!({"id": album, "ids": ids})).expect("add to album");
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "rating", "op": "gte", "value": 4}]}});
    let smart = s.execute("album.createSmart", &json!({"name": "Good", "rules": rules})).expect("smart")["id"].as_u64().expect("id");
    let tiny = s.execute("album.create", &json!({"name": "Tiny"})).expect("album")["id"].as_u64().expect("id");
    // a few stacks, as a real library has (stacks make the view arrange its photos)
    for k in 0..50u64 {
        let at = k * (n as u64 / 60) + 2;
        s.execute("library.select", &json!({"ids": [at, at + 1, at + 2]})).expect("select");
        s.execute("stack.group", &json!({})).expect("stack");
    }

    let cases: Vec<(&str, &str, Value)> = vec![
        ("All Photos", "library.source", json!({"kind": "all"})),
        ("Recently Added", "library.source", json!({"kind": "recentlyAdded"})),
        ("Picks", "library.source", json!({"kind": "picks"})),
        ("Folder (1 of 2000)", "library.source", json!({"kind": "libraryFolder", "path": "/pics/f7"})),
        ("Plain album", "library.source", json!({"kind": "album", "id": album})),
        ("Smart album (rating>=4)", "library.source", json!({"kind": "album", "id": smart})),
    ];
    println!("\nsource switch, ms (first = the first click of the session; then median of {runs} with the view left in between)");
    for (name, cmd, p) in &cases {
        let mut v = Vec::new();
        let mut first = 0.0;
        for r in 0..=runs {
            run(&mut s, "library.source", json!({"kind": "album", "id": tiny}));
            let d = run(&mut s, cmd, p.clone());
            if r == 0 {
                first = d;
                // the first ask of a session may start work in the background: give it time, as a user does
                std::thread::sleep(Duration::from_secs(4));
            } else {
                v.push(d);
            }
        }
        println!("{name:<28} first {first:>9.1}  median {:>9.1}  worst {:>9.1}", median(&mut v.clone()), v.iter().cloned().fold(0.0, f64::max));
    }

    run(&mut s, "library.source", json!({"kind": "all"}));
    println!("\nsort and filter changes on All Photos, ms");
    for key in ["fileName", "rating", "importDate", "captureDate"] {
        let mut v = Vec::new();
        for _ in 0..=runs {
            run(&mut s, "library.sort", json!({"key": "captureDate", "ascending": false}));
            run(&mut s, "library.source", json!({"kind": "all"}));
            let t = Instant::now();
            s.execute("library.sort", &json!({"key": key, "ascending": true})).expect("sort");
            s.visible();
            v.push(ms(t.elapsed()));
        }
        v.remove(0);
        println!("sort by {key:<24} median {:>9.1}  worst {:>9.1}", median(&mut v.clone()), v.iter().cloned().fold(0.0, f64::max));
    }
    run(&mut s, "library.sort", json!({"key": "captureDate", "ascending": false}));
    let mut v = Vec::new();
    for _ in 0..=runs {
        run(&mut s, "library.filter", json!({"rating": 4}));
        v.push(run(&mut s, "library.clearFilter", json!({})));
    }
    v.remove(0);
    println!("clear a filter               median {:>9.1}  worst {:>9.1}", median(&mut v.clone()), v.iter().cloned().fold(0.0, f64::max));

    println!("\nfollowing an import batch of 16 photos while a view is shown, ms per batch (commit + visible)");
    for (name, cmd, p) in [cases[0].clone(), cases[1].clone(), cases[5].clone()] {
        run(&mut s, cmd, p);
        s.visible();
        let (mut v, mut k) = (Vec::new(), 0);
        for b in 0..120 {
            let ops = (0..16)
                .map(|j| {
                    k += 1;
                    let imported = format!("2026-11-07T{:02}:{:02}:{:02}", (b / 60) % 24, b % 60, j);
                    Op::AddPhoto { photo: Box::new(photo(&mut s, n + k, &imported)) }
                })
                .collect();
            let t = Instant::now();
            s.catalog.apply(Op::Batch { ops }).expect("batch");
            s.visible();
            v.push(ms(t.elapsed()));
        }
        let total: f64 = v.iter().sum();
        println!(
            "{name:<28} median {:>9.2}  worst {:>9.1}  total for 120 batches {:>9.0}",
            median(&mut v.clone()),
            v.iter().cloned().fold(0.0, f64::max),
            total
        );
    }
}
