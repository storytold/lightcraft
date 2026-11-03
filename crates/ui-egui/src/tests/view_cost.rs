//! What one edit costs in a large library: a measurement, not a test (run with `--ignored --nocapture`).

use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A library of `n` photos, about a third of them keyworded from a pool of 500 keywords, showing a smart album of
/// the photos without keywords at random.
fn library(n: usize) -> lightcraft_engine::Session {
    let mut s = lightcraft_engine::Session::new();
    let mut keyworded: Vec<Vec<u64>> = vec![Vec::new(); 500];
    for i in 0..n {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i:06}.jpg");
        let day = 1 + (i / 400) % 28;
        let month = 1 + (i / 11_200) % 12;
        let mut p = Photo::new(id, Source::File { path: format!("/pics/{}/{name}", i / 1000) }, &name, "JPEG", 6000, 4000, "2026-01-01T10:00:00");
        p.captured = Some(format!("2025-{month:02}-{day:02}T10:{:02}:{:02}", (i / 60) % 60, i % 60));
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        if i % 3 == 0 {
            keyworded[(i / 3) % 500].push(id.0);
        }
    }
    for (k, ids) in keyworded.iter().enumerate() {
        s.execute("library.select", &json!({"ids": ids})).unwrap();
        s.execute("photo.setMeta", &json!({"addKeywords": [format!("keyword {k:03}")]})).unwrap();
    }
    let rules = json!({"ruleSet": {"match": "all", "rules": [{"field": "keywordCount", "op": "is", "value": 0}]}});
    let album = s.execute("album.createSmart", &json!({"name": "Unkeyworded", "rules": rules})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.execute("library.sort", &json!({"key": "random"})).unwrap();
    s
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn what_one_keyword_costs_in_a_library_of_67_000_photos() {
    let t = Instant::now();
    let mut s = library(67_000);
    eprintln!("built the library in {:.0} ms", ms(t.elapsed()));
    // the engine alone
    let t = Instant::now();
    let shown = s.visible().len();
    eprintln!("engine: first view of {shown} photos: {:.1} ms", ms(t.elapsed()));
    for round in 0..3 {
        let id: PhotoId = s.visible()[100 + round];
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        let t = Instant::now();
        s.execute("photo.setMeta", &json!({"addKeywords": ["keyword 001"]})).unwrap();
        let command = t.elapsed();
        let t = Instant::now();
        let n = s.visible().len();
        let view = t.elapsed();
        let t = Instant::now();
        let _ = s.source_total();
        let total = t.elapsed();
        eprintln!("engine: keyword {round}: command {:.1} ms, view of {n} again {:.1} ms, source total {:.1} ms", ms(command), ms(view), ms(total));
    }
    // its parts
    let ids = s.visible_cloned();
    let t = Instant::now();
    let f = s.source.to_filter(&s.filter, &s.catalog);
    let q = s.catalog.query(&f, &s.sort);
    let query = t.elapsed();
    let t = Instant::now();
    let _ = s.catalog.arrange_stacks(&q);
    let stacks = t.elapsed();
    let t = Instant::now();
    let _ = s.catalog.stack_index();
    let index = t.elapsed();
    eprintln!(
        "engine parts: query and sort {:.1} ms, arrange stacks {:.1} ms, stack index {:.1} ms ({} ids)",
        ms(query),
        ms(stacks),
        ms(index),
        ids.len()
    );
    // what the side panels tally from every photo after each change
    let time = |what: &str, f: &dyn Fn()| {
        let t = Instant::now();
        f();
        eprintln!("tally: {what} {:.1} ms", ms(t.elapsed()));
    };
    time("folder tree", &|| drop(s.catalog.folder_tree()));
    time("keyword tree", &|| drop(s.catalog.keyword_tree()));
    time("date groups", &|| drop(s.catalog.date_groups()));
    time("people", &|| drop(s.catalog.people()));
    time("count of a smart album", &|| {
        let _ = s.catalog.count(&f);
    });
    time("count of all photos", &|| {
        let _ = s.catalog.count(&lightcraft_catalog::Filter::default());
    });
    // the app: a frame after an edit against a frame with nothing changed
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.left_panel = true;
    h.request("engine.execute", json!({"command": "view.squareGrid", "params": {}}), Duration::from_secs(60));
    h.request("engine.execute", json!({"command": "panel.keywords", "params": {}}), Duration::from_secs(60));
    h.settle(Duration::from_secs(120));
    let frame = |h: &mut Headless| {
        let t = Instant::now();
        h.step();
        t.elapsed()
    };
    let idle: Vec<f64> = (0..5).map(|_| ms(frame(&mut h))).collect();
    eprintln!("app: frames with nothing changed: {idle:.1?} ms");
    for round in 0..3 {
        let id: PhotoId = h.app.session.visible()[300 + round];
        h.app.session.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        h.step();
        h.step();
        let t = Instant::now();
        h.app.run("photo.setMeta", json!({"addKeywords": ["keyword 002"]})).unwrap();
        let command = t.elapsed();
        let first = frame(&mut h);
        let second = frame(&mut h);
        eprintln!("app: keyword {round}: command {:.1} ms, the frame after it {:.1} ms, the next {:.1} ms", ms(command), ms(first), ms(second));
    }
    // which part of the window the frame after an edit pays for
    for (what, left, right) in [("grid alone", false, false), ("grid and left panel", true, false), ("grid and Keywords panel", false, true)] {
        h.app.ui.left_panel = left;
        h.request("engine.execute", json!({"command": if right { "panel.keywords" } else { "panel.close" }, "params": {}}), Duration::from_secs(60));
        if right && h.app.ui.right != crate::state::RightPanel::Keywords {
            h.request("engine.execute", json!({"command": "panel.keywords", "params": {}}), Duration::from_secs(60));
        }
        h.settle(Duration::from_secs(120));
        let id: PhotoId = h.app.session.visible()[500];
        h.app.session.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        h.step();
        h.step();
        h.app.run("photo.setMeta", json!({"addKeywords": ["keyword 004"]})).unwrap();
        let first = frame(&mut h);
        eprintln!("app: {what} (right panel {:?}): the frame after a keyword {:.1} ms", h.app.ui.right, ms(first));
    }
    h.app.ui.left_panel = true;
    let view = "view.photoGrid";
    h.request("engine.execute", json!({"command": view, "params": {}}), Duration::from_secs(60));
    h.settle(Duration::from_secs(120));
    let id: PhotoId = h.app.session.visible()[400];
    h.app.session.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    h.step();
    h.step();
    h.app.run("photo.setMeta", json!({"addKeywords": ["keyword 003"]})).unwrap();
    let first = frame(&mut h);
    eprintln!("app: {view}: the frame after a keyword {:.1} ms", ms(first));
}
