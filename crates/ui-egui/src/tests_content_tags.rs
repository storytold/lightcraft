//! Auto-tags in the window: the Library Filter's Contents picker (tags with counts, picking one filters the grid), the
//! Keywording panel's "Suggested from contents" (click accepts, ✕ dismisses) and Settings ▸ Search's auto-tag section.
//! A small stand-in model (an image's embedding is its colour cast, a text's the colour it names) stands for the real
//! one.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightcraft_catalog::PhotoId;
use lightcraft_vision::{ContentModel, Manifest, Rgb8, Scoring, Towers};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);
const SCORING: Scoring = Scoring { scale: 10.0, bias: -5.0, sigmoid: true };

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    lightcraft_vision::normalize(&mut v);
    v
}

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
        Ok(texts
            .iter()
            .map(|t| {
                normalised(if t.contains("green") {
                    vec![-0.5, 1.0, -0.5, 0.0]
                } else if t.contains("blue") {
                    vec![-0.5, -0.5, 1.0, 0.0]
                } else if t.contains("red") {
                    vec![1.0, -0.5, -0.5, 0.0]
                } else {
                    vec![0.0, 0.0, 0.0, 1.0]
                })
            })
            .collect())
    }

    fn score_cosine(&self, cosine: f32) -> f32 {
        SCORING.score(cosine)
    }

    fn scoring(&self) -> Option<Scoring> {
        Some(SCORING)
    }

    fn manifest(&self) -> &Manifest {
        &self.0
    }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-contenttags-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The demo library in the grid, indexed by the stand-in, with a vocabulary of colours and their tags worked out.
fn app(d: &Path) -> Headless {
    let content = d.join("content");
    let mut m = lightcraft_vision::manifest::builtin_siglip2_b16_256();
    m.id = "fake".into();
    m.embedding.dim = 4;
    m.match_threshold = 0.5;
    m.tag_threshold = 0.5;
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
    std::fs::write(content.join("settings.json"), r#"{"enabled": true, "model": "fake", "pace": "fast"}"#).unwrap();
    std::fs::write(content.join("vocabulary.txt"), "colour|green\ncolour|blue\ncolour|warm|red\n").unwrap();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    app.ui.filter_bar = true;
    app.session.content_models_dir = Some(content);
    app.session.set_content_model_loader(Arc::new(|_: &Path, m: &Manifest, _: Towers| Ok(Box::new(Fake(m.clone())) as Box<dyn ContentModel>)));
    app.session.execute("content.pump", &json!({"wait": true})).unwrap();
    let r = app.session.execute("content.tags", &json!({"ids": [], "wait": true})).unwrap();
    assert_eq!(r["state"], "ready", "{r}");
    let mut h = Headless::new(app, [1500.0, 1000.0], 1.0);
    h.settle(T);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    for _ in 0..3 {
        h.step();
    }
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn the_contents_picker_lists_the_tags_with_their_counts_and_filters_the_grid() {
    let d = temp("picker");
    let mut h = app(&d);
    let counts = h.app.session.content_tag_counts();
    let (tag, n) = counts.iter().find(|(t, _)| t == "colour|green").cloned().expect("some photos are green");
    let all = h.app.session.visible_cloned().len();
    assert!(n > 0 && n < all);
    assert!(has(&h, "dropdown:filter-contentTag"), "the picker is in the filter bar");
    click(&mut h, "dropdown:filter-contentTag");
    // the parent tag is listed; a tag below it too, when some photo has it
    assert!(has(&h, "filterItem:contentTag:colour"), "the parent tag is listed");
    if counts.iter().any(|(t, _)| t == "colour|warm|red") {
        assert!(has(&h, "filterItem:contentTag:colour|warm|red"));
    }
    click(&mut h, &format!("filterItem:contentTag:{tag}"));
    assert_eq!(h.app.session.filter.content_tag.as_deref(), Some(tag.as_str()));
    assert_eq!(h.app.session.visible_cloned().len(), n, "the grid shows the tag's photos");
    // a chip says so, and its ✕ ends it
    let chips = lightcraft_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog);
    assert!(chips.iter().any(|c| c.label == "Contents: green"), "{chips:?}");
    h.request("engine.execute", json!({"command": "library.filter", "params": {"contentTag": null}}), T);
    h.settle(T);
    assert_eq!(h.app.session.visible_cloned().len(), all);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn suggestions_from_contents_are_accepted_as_keywords_or_dismissed_from_the_keywording_panel() {
    let d = temp("suggest");
    let mut h = app(&d);
    // a green photo selected, the Keywords panel open
    let shown = h.app.session.visible_cloned();
    let green = h.app.session.content_suggestions(&shown).into_iter().find(|s| s.tag == "colour|green").unwrap().photos;
    let id = green[0];
    for (method, params) in
        [("ui.set", json!({"right": "keywords"})), ("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}))]
    {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(T);
    assert!(has(&h, "label:contentSuggestions"));
    assert!(
        has(&h, "contentSuggest:colour|green"),
        "{:?}",
        h.app.widgets.iter().map(|w| &w.0).filter(|w| w.starts_with("content")).collect::<Vec<_>>()
    );
    click(&mut h, "contentSuggest:colour|green");
    let kws = h.app.session.catalog.photo(id).unwrap().meta.keywords.clone();
    assert!(kws.contains(&"Contents|green".to_string()), "{kws:?}");
    h.settle(T);
    assert!(!has(&h, "contentSuggest:colour|green"), "accepted: no longer suggested");
    // undo takes it off again, and it is suggested again; ✕ dismisses it for good
    h.request("engine.execute", json!({"command": "edit.undo", "params": {}}), T);
    h.settle(T);
    assert!(has(&h, "contentSuggestDismiss:colour|green"));
    click(&mut h, "contentSuggestDismiss:colour|green");
    h.settle(T);
    assert!(!has(&h, "contentSuggest:colour|green"));
    assert!(!h.app.session.catalog.photo(PhotoId(id.0)).unwrap().meta.keywords.iter().any(|k| k.contains("green")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn settings_search_shows_the_auto_tag_options_and_the_vocabulary() {
    let d = temp("settings");
    let mut h = app(&d);
    let r = h.request("ui.menu.invoke", json!({"command": "app.settings", "params": {"tab": "search"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert!(has(&h, "field:contentTagParent"), "{:?}", h.app.widgets.iter().map(|w| &w.0).filter(|w| w.contains("ontent")).collect::<Vec<_>>());
    assert!(has(&h, "label:contentTagsState"));
    click(&mut h, "content:vocabulary");
    h.settle(T);
    assert!(has(&h, "field:contentVocabulary") && has(&h, "button:contentVocabularySave") && has(&h, "button:contentVocabularyReset"));
    // Restore Defaults: the built-in list
    click(&mut h, "button:contentVocabularyReset");
    let v = h.app.session.execute("content.vocabulary", &json!({})).unwrap();
    assert_eq!(v["custom"], false);
    assert!(v["count"].as_u64().unwrap() > 1000);
    let _ = std::fs::remove_dir_all(&d);
}

/// The Smart Rules editor's unsaved contents rules are asked about while it is open, and forgotten when it closes.
#[test]
fn the_smart_rules_editor_forgets_its_contents_asks_when_it_closes() {
    let d = temp("rulesasks");
    let mut h = app(&d);
    let rules: lightcraft_catalog::RuleSet =
        serde_json::from_value(json!({"rules": [{"field": "contents", "op": "shows", "value": "green"}]})).unwrap();
    h.app.ui.dialog = Some(crate::state::Dialog::SmartRules { id: None, name: "Green".into(), rules, parent: None });
    h.step();
    h.step();
    assert!(!h.app.session.content_rules_asked().is_empty(), "asked while the editor is open");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.session.content_rules_asked().is_empty(), "forgotten once it is closed");
    let _ = std::fs::remove_dir_all(&d);
}
