//! Searching photos by their contents from the search bar: the Text / Contents toggle, typing (with its pause), the
//! ranked grid and its chip (More / Fewer, ✕), Relevance in the sort menu, Find Similar Contents, the hint that turns
//! indexing on, and a search model that loads without holding up the window. A small stand-in model (an image's
//! embedding is its colour cast) stands for the real one; `the_real_model_finds_the_beach_from_the_search_bar` runs
//! the real one when `LIGHTCRAFT_VISION_DIR` points at it.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;
use lightcraft_engine::content_query::{ContentAsk, ContentHint, ContentState};
use lightcraft_vision::{ContentModel, Manifest, Rgb8, Towers};
use serde_json::json;

use crate::headless::Headless;
use crate::state::SearchMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    lightcraft_vision::normalize(&mut v);
    v
}

/// The stand-in: a picture's embedding is its colour cast; "green", "blue" and "red" name casts.
struct Fake(Manifest);

impl ContentModel for Fake {
    fn embed_images(&self, batch: &[Rgb8]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        Ok(batch
            .iter()
            .map(|img| {
                let n = img.data.len().max(1) as f32;
                let mean = |c: usize| img.data.iter().map(|p| f32::from(p[c]) / 255.0).sum::<f32>() / n;
                let (r, g, b) = (mean(0), mean(1), mean(2));
                let m = (r + g + b) / 3.0;
                normalised(vec![r - m, g - m, b - m, 0.02])
            })
            .collect())
    }

    fn embed_texts(&self, texts: &[&str]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|t| match *t {
                "green" => Ok(normalised(vec![-0.5, 1.0, -0.5, 0.0])),
                "blue" => Ok(normalised(vec![-0.5, -0.5, 1.0, 0.0])),
                "red" => Ok(normalised(vec![1.0, -0.5, -0.5, 0.0])),
                _ => Err(lightcraft_vision::Error::Input(format!("the stand-in knows no `{t}`"))),
            })
            .collect()
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        1.0 / (1.0 + (-(10.0 * cosine - 5.0)).exp())
    }

    fn manifest(&self) -> &Manifest {
        &self.0
    }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-contentsearch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Install the stand-in as model `fake` in `content` (indexing `enabled`).
fn install_fake(content: &Path, enabled: bool) {
    let mut m = lightcraft_vision::manifest::builtin_siglip2_b16_256();
    m.id = "fake".into();
    m.embedding.dim = 4;
    m.match_threshold = 0.5;
    let body = b"weights".to_vec();
    m.files = vec![lightcraft_vision::manifest::FileSpec {
        name: "model.safetensors".into(),
        size: body.len() as u64,
        sha256: lightcraft_faces::hash::sha256_hex(&body),
    }];
    let home = content.join("fake");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("model.safetensors"), &body).unwrap();
    std::fs::write(home.join("content-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join("installed.json"), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
    std::fs::write(content.join("settings.json"), format!(r#"{{"enabled": {enabled}, "model": "fake", "pace": "fast"}}"#)).unwrap();
}

/// The demo library in the grid, with the stand-in model (text tower loading takes `text_ms`), indexed when `index`.
fn app(d: &Path, model: bool, index: bool, text_ms: u64) -> Headless {
    let content = d.join("content");
    std::fs::create_dir_all(&content).unwrap();
    if model {
        install_fake(&content, index);
    }
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    app.session.content_models_dir = Some(content);
    app.session.set_content_model_loader(Arc::new(move |_: &Path, m: &Manifest, towers: Towers| {
        if towers.text {
            std::thread::sleep(Duration::from_millis(text_ms));
        }
        Ok(Box::new(Fake(m.clone())) as Box<dyn ContentModel>)
    }));
    if index {
        app.session.execute("content.pump", &json!({"wait": true})).unwrap();
    }
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

fn contents_mode(h: &mut Headless) {
    click(h, "search:mode");
    click(h, "menu:searchContents");
    assert_eq!(h.app.ui.search_mode, SearchMode::Contents);
}

/// Type `text` in the search field (Contents mode searches after a pause in the typing).
fn type_search(h: &mut Headless, text: &str) {
    click(h, "field:search");
    let r = h.request("ui.text", json!({"text": text}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// View ▸ Sort ▸ Relevance: (enabled, checked).
fn relevance_item(app: &LightcraftApp) -> Option<(bool, Option<bool>)> {
    use crate::menubar::MenuNode;
    fn find(nodes: &[MenuNode]) -> Option<(bool, Option<bool>)> {
        nodes.iter().find_map(|n| match n {
            MenuNode::Item { id, params, enabled, checked, .. } if id == "library.sort" && params["key"] == "relevance" => Some((*enabled, *checked)),
            MenuNode::Submenu { children, .. } => find(children),
            _ => None,
        })
    }
    crate::menubar::menu_bar(app).iter().find_map(|(_, nodes)| find(nodes))
}

fn ready(h: &mut Headless) -> bool {
    h.step_until(T, |h| h.app.session.content_query.contents.is_some()) && {
        h.step_until(T, |h| {
            // (`content_view` needs the session mutably; the state the last frame left is enough here)
            h.app.widgets.iter().any(|(w, _)| w == "label:contentsStatus") && h.app.session.content_ask().is_some()
        })
    }
}

#[test]
fn typing_in_contents_mode_ranks_the_grid_and_shows_the_chip_with_more_and_fewer() {
    let d = temp("typing");
    let mut h = app(&d, true, true, 0);
    let all = h.app.session.visible().len();
    contents_mode(&mut h);
    type_search(&mut h, "green");
    // not on every key: the search waits for a pause
    assert_eq!(h.app.session.content_query.contents, None, "searched before the pause");
    assert!(ready(&mut h));
    h.settle(T);
    let v = h.app.session.content_view().expect("a search by contents");
    assert_eq!((v.ask.clone(), v.state.clone()), (ContentAsk::Text("green".into()), ContentState::Ready));
    // the grid: the matching photos, best first, with the chip, More and Fewer
    let ids = h.app.session.visible().to_vec();
    assert!(!ids.is_empty() && ids.len() < all, "{} of {all}", ids.len());
    assert_eq!(h.app.session.sort.key, lightcraft_catalog::SortKey::Relevance);
    let scores: Vec<f32> = ids.iter().map(|id| h.app.session.content_score(*id).unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "best first: {scores:?}");
    let first = h
        .app
        .widgets
        .iter()
        .filter(|(w, _)| w.starts_with("thumb:"))
        .min_by(|a, b| (a.1.top(), a.1.left()).partial_cmp(&(b.1.top(), b.1.left())).unwrap_or(std::cmp::Ordering::Equal));
    assert_eq!(first.map(|(w, _)| w.clone()), Some(format!("thumb:{}", ids[0].0)), "the best match is the first cell");
    for w in ["chip:contents", "chip:contents:x", "button:contentsMore", "button:contentsFewer", "label:contentsStatus"] {
        assert!(has(&h, w), "{w}");
    }
    // More widens, Fewer narrows back
    let n0 = ids.len();
    // (each More halves the cut-off: a few steps until the next photos come in)
    let mut steps = 0;
    while h.app.session.visible().len() == n0 && steps < 6 {
        click(&mut h, "button:contentsMore");
        h.settle(T);
        steps += 1;
    }
    let n1 = h.app.session.visible().len();
    assert!(n1 > n0, "More: {n1} after {n0} ({:?})", h.app.session.content_view());
    assert_eq!(h.app.session.content_query.step, steps);
    click(&mut h, "button:contentsFewer");
    h.settle(T);
    assert!(h.app.session.visible().len() < n1, "Fewer narrows again");
    // hovering a cell shows its match bar
    let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{}", ids[0].0)).map(|(_, r)| r.center()).unwrap();
    h.request("ui.move", json!({"x": cell.x, "y": cell.y}), T);
    h.step();
    h.step();
    assert!(has(&h, &format!("match:{}", ids[0].0)), "the match bar on hover");
    // ✕ ends the search: every photo, the sort back, the field empty
    click(&mut h, "chip:contents:x");
    h.settle(T);
    assert_eq!(h.app.session.visible().len(), all);
    assert_eq!(h.app.session.sort.key, lightcraft_catalog::SortKey::CaptureDate);
    assert!(h.app.ui.search.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn contents_words_work_in_text_mode_and_relevance_is_in_the_sort_menu_only_while_searching() {
    let d = temp("words");
    let mut h = app(&d, true, true, 0);
    assert!(!h.app.session.content_ask().is_some());
    type_search(&mut h, "contents:green");
    h.settle(T);
    assert!(h.step_until(T, |h| h.app.session.content_ask() == Some(ContentAsk::Text("green".into()))));
    h.settle(T);
    let n = h.app.session.visible().len();
    assert!(n > 0 && has(&h, "chip:contents"));
    // the View ▸ Sort menu offers Relevance now (checked), and not without a search
    assert_eq!(relevance_item(&h.app), Some((true, Some(true))));
    click(&mut h, "chip:contents:x");
    h.settle(T);
    assert_eq!(h.app.session.content_ask(), None);
    assert_eq!(relevance_item(&h.app).map(|r| r.0), Some(false), "disabled without a search");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn find_similar_contents_from_the_grid_shows_a_ranked_view_with_its_chip() {
    let d = temp("similar");
    let mut h = app(&d, true, true, 0);
    let id: PhotoId = h.app.session.visible()[2];
    let name = h.app.session.catalog.photo(id).unwrap().file_name.clone();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": format!("thumb:{}", id.0), "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert!(has(&h, "menu:findSimilarContents"), "the context menu has it");
    click(&mut h, "menu:findSimilarContents");
    h.settle(T);
    let v = h.app.session.content_view().expect("a ranked view");
    assert_eq!(v.label, format!("Similar to {name}"));
    assert_eq!(h.app.session.visible().first(), Some(&id), "the photo first");
    assert!(has(&h, "chip:contents"));
    // the other Find Similar is still there, under its own name
    let r = h.request("ui.clickWidget", json!({"id": format!("thumb:{}", id.0), "button": "right"}), T);
    assert_eq!(r["ok"], true);
    h.settle(T);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn contents_mode_without_indexing_says_how_to_turn_it_on() {
    let d = temp("hint");
    // no model at all
    let mut h = app(&d, false, false, 0);
    contents_mode(&mut h);
    h.settle(T);
    assert_eq!(h.app.session.content_hint(), Some(ContentHint::TurnOnIndexing));
    assert!(has(&h, "label:contentsHint") && has(&h, "button:contentsSettings"));
    click(&mut h, "button:contentsSettings");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "search"), "Settings ▸ Search opens");
    let _ = std::fs::remove_dir_all(&d);
    // a model, indexing off: the same hint, also on a search
    let d = temp("hint2");
    let mut h = app(&d, true, false, 0);
    contents_mode(&mut h);
    type_search(&mut h, "green");
    assert!(ready(&mut h));
    h.settle(T);
    assert!(has(&h, "label:contentsHint") && has(&h, "button:contentsSettings"));
    assert_eq!(h.app.session.visible().len(), 0, "nothing indexed, nothing found");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_search_model_loads_while_the_window_keeps_drawing_frames() {
    let d = temp("offthread");
    let mut h = app(&d, true, true, 800);
    contents_mode(&mut h);
    type_search(&mut h, "gr");
    // more typing while it loads: the last text is what is searched
    let r = h.request("ui.text", json!({"text": "een"}), T);
    assert_eq!(r["ok"], true);
    let t0 = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut preparing_frames = 0;
    while t0.elapsed() < Duration::from_secs(20) {
        let f = Instant::now();
        h.step();
        slowest = slowest.max(f.elapsed());
        match h.app.session.content_view().map(|v| v.state) {
            Some(ContentState::Preparing) => preparing_frames += 1,
            Some(ContentState::Ready) => break,
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let v = h.app.session.content_view().unwrap();
    assert_eq!((v.ask, v.state), (ContentAsk::Text("green".into()), ContentState::Ready));
    assert!(preparing_frames >= 5, "frames were drawn while the model loaded: {preparing_frames}");
    assert!(slowest < Duration::from_millis(600), "no frame waited for the model: {slowest:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// The real model (`LIGHTCRAFT_VISION_DIR` = the folder with SigLIP 2 base 256's files): the demo library is indexed,
/// and "a beach" typed in the search bar's Contents mode puts a beach first.
#[cfg(unix)]
#[test]
fn the_real_model_finds_the_beach_from_the_search_bar() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_VISION_DIR").map(PathBuf::from) else {
        eprintln!("LIGHTCRAFT_VISION_DIR is not set: skipping");
        return;
    };
    let d = temp("real");
    let content = d.join("content");
    let m = lightcraft_vision::manifest::builtin_siglip2_b16_256();
    let home = content.join(&m.id);
    std::fs::create_dir_all(&home).unwrap();
    for f in &m.files {
        std::os::unix::fs::symlink(dir.join(&f.name), home.join(&f.name)).unwrap();
    }
    std::fs::write(home.join("content-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(home.join("installed.json"), br#"{"acceptedAt": "2026-10-11T00:00:00Z"}"#).unwrap();
    std::fs::write(content.join("settings.json"), format!(r#"{{"enabled": true, "model": "{}", "pace": "fast"}}"#, m.id)).unwrap();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    app.session.content_models_dir = Some(content);
    app.session.execute("content.pump", &json!({"wait": true})).unwrap();
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    contents_mode(&mut h);
    type_search(&mut h, "a beach");
    assert!(h.step_until(Duration::from_secs(120), |h| h.app.session.content_query.contents.is_some()));
    assert!(h.step_until(Duration::from_secs(120), |h| {
        h.app.widgets.iter().any(|(w, _)| w == "label:contentsStatus") && h.app.session.content_query.contents.is_some()
    }));
    let deadline = Instant::now() + Duration::from_secs(120);
    while h.app.session.content_view().is_some_and(|v| v.state == ContentState::Preparing) && Instant::now() < deadline {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.settle(T);
    let first = h.app.session.visible()[0];
    let lightcraft_catalog::Source::Demo { scene } = h.app.session.catalog.photo(first).unwrap().source else { panic!("not a demo photo") };
    let kind = lightcraft_scenes::demo_library().into_iter().find(|s| s.id == scene).unwrap().kind;
    let view = h.app.session.content_view();
    eprintln!("first: {} ({kind:?}), {view:?}", h.app.session.catalog.photo(first).unwrap().file_name);
    assert_eq!(kind, lightcraft_scenes::Kind::Beach);
    let _ = std::fs::remove_dir_all(&d);
}

/// A search by contents isn't kept between runs: its text doesn't come back as a search of the photos' text.
#[test]
fn a_contents_search_text_is_not_restored_as_a_text_search() {
    let mut ui = crate::UiState { search: "a dog on a jetty".into(), search_mode: SearchMode::Contents, ..Default::default() };
    let back = serde_json::from_str::<crate::UiState>(&serde_json::to_string(&ui).unwrap()).unwrap().sanitized();
    assert_eq!((back.search.as_str(), back.search_mode), ("", SearchMode::Text));
    // a text search is kept
    ui.search = "rating:>=3".into();
    ui.search_mode = SearchMode::Text;
    let back = serde_json::from_str::<crate::UiState>(&serde_json::to_string(&ui).unwrap()).unwrap().sanitized();
    assert_eq!((back.search.as_str(), back.search_mode), ("rating:>=3", SearchMode::Text));
    // an older ui.json without the mode, or with junk in it, reads as Text
    let mut v = serde_json::to_value(&ui).unwrap();
    v["searchMode"] = json!(42);
    assert_eq!(serde_json::from_value::<crate::UiState>(v).unwrap().search_mode, SearchMode::Text);
}

/// A pause waited for in Contents mode is forgotten once the field searches text: it neither searches nor keeps the
/// window repainting.
#[test]
fn a_pending_contents_search_is_dropped_in_text_mode() {
    let d = temp("pendingtext");
    let mut h = app(&d, false, false, 0);
    h.app.ui.search_mode = SearchMode::Text;
    h.app.ui.search_due = Some(1e9);
    h.step();
    h.step();
    assert_eq!(h.app.ui.search_due, None);
    assert_eq!(h.app.session.content_ask(), None);
    let _ = std::fs::remove_dir_all(&d);
}
