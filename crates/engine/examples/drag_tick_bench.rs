//! UI-thread work one slider-drag tick costs outside the render: `develop.set` inside an
//! interaction, the filmstrip's `visible()` after the catalog revision moved, and the settings
//! hash the loupe takes every frame. `cargo run --release -p lightcraft-engine --example
//! drag_tick_bench -- [photos]` (default 20000).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use serde_json::json;
use std::time::Instant;

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let mut s = Session::new();
    for i in 1..=n {
        let mut p = Photo::new(PhotoId(i), Source::File { path: format!("/x/{i}.ARW") }, &format!("{i}.ARW"), "ARW", 9984, 6656, "2026-01-01");
        p.captured = Some(format!("2025-{:02}-{:02}T10:{:02}:00", 1 + i % 12, 1 + i % 28, i % 60));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    s.visible_cloned();
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    let ticks = 30;
    let (mut set, mut vis, mut hash) = (0.0, 0.0, 0.0);
    for k in 0..ticks {
        let t = Instant::now();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": k as f64 / 30.0})).unwrap();
        set += ms(t);
        let t = Instant::now();
        let v = s.visible_cloned();
        vis += ms(t);
        assert_eq!(v.len() as u64, n);
        let d = s.develop_of(PhotoId(1)).unwrap();
        let t = Instant::now();
        std::hint::black_box(d.hash64());
        hash += ms(t);
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    let f = ticks as f64;
    println!("{n} photos, per tick: develop.set {:.3} ms, visible() {:.3} ms, settings hash64 {:.3} ms", set / f, vis / f, hash / f);
}
