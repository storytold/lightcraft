//! Headless tests of the left sidebar's Keywords section on a large library (issue #1011): with
//! thousands of keywords listed, a frame draws only the rows near the screen, and the rows out
//! of sight still take their room, so scrolling reaches the last keyword.

use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(120);

/// `photos` photos with six keywords each out of `keywords` top-level ones (each with a child).
fn library(photos: u64, keywords: u64) -> Session {
    let mut s = Session::new();
    for i in 0..photos {
        let path = format!("/lightcraft-kw-test/IMG_{i:05}.jpg");
        let mut p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:05}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.meta.keywords = (0..6).map(|k| format!("Keyword {:05}|Child", (i * 6 + k) % keywords)).collect();
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s
}

fn app(photos: u64, keywords: u64) -> Headless {
    let app = LightcraftApp::new(library(photos, keywords), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// The keyword rows drawn this frame (every row is listed as `source:…`; only a drawn one has its
/// `label:…`).
fn keyword_rows(h: &Headless) -> Vec<String> {
    h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("label:keyword:")).map(str::to_string).collect()
}

/// Given a library with 2,000 keywords, when the sidebar's Keywords section is open, then a frame
/// draws only the few dozen rows that fit on screen (not all 2,000), and scrolling to the end
/// draws the last keyword: the rows out of sight keep their height.
#[test]
fn keywords_section_draws_only_the_rows_near_the_screen() {
    const KEYWORDS: u64 = 2000;
    let mut h = app(KEYWORDS / 6 + 1, KEYWORDS);
    assert_eq!(h.app.caches.keyword_tree(&h.app.session.catalog).len(), KEYWORDS as usize);
    let rows = keyword_rows(&h);
    assert!(!rows.is_empty() && rows.len() < 100, "{} of {KEYWORDS} keyword rows drawn", rows.len());
    assert!(rows.contains(&"Keyword 00000".to_string()), "{rows:?}");
    // all the way down
    crate::tests_panels::scroll_sidebar(&mut h, -(KEYWORDS as f32) * 40.0);
    let rows = keyword_rows(&h);
    assert!(!rows.is_empty() && rows.len() < 100, "{} of {KEYWORDS} keyword rows drawn", rows.len());
    assert!(rows.contains(&"Keyword 01999".to_string()), "the last keyword is reached: {rows:?}");
    assert!(!rows.contains(&"Keyword 00000".to_string()), "{rows:?}");
}

/// Timings on a library the size of the report's (58,000 photos): `cargo test --release -p
/// lightcraft-ui-egui --lib probe_keyword_sidebar_cost -- --ignored --nocapture`.
#[test]
#[ignore = "timing probe"]
fn probe_keyword_sidebar_cost() {
    let mut h = app(58_000, 4000);
    let t = Instant::now();
    let tree = h.app.session.catalog.keyword_tree();
    eprintln!("keyword_tree: {:?} ({} top level)", t.elapsed(), tree.len());
    eprintln!("keyword rows drawn: {}", keyword_rows(&h).len());
    let t = Instant::now();
    for _ in 0..10 {
        h.step();
    }
    eprintln!("unchanged frame: {:?}", t.elapsed() / 10);
    let t = Instant::now();
    for i in 0..10u64 {
        let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [i + 1]}}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": (i % 5) + 1}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    eprintln!("frame after a rating: {:?}", t.elapsed() / 10);
}
