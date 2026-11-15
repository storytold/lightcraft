//! The filter bar's camera / lens / keyword dropdowns: a search box that takes the typing as soon
//! as the dropdown opens, narrows the rows, keeps the "Any …" row, and starts empty next time.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ok(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    h.settle(SETTLE);
}

fn rows(h: &Headless, picker: &str) -> Vec<String> {
    let prefix = format!("filterItem:{picker}:");
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(&prefix)).map(|(w, _)| w.clone()).collect()
}

/// The demo library's grid with the filter bar over it.
fn filter_bar() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.filter_bar = true;
    h.settle(SETTLE);
    h
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn the_keyword_picker_takes_several_keywords_toggles_or_and_and_narrows_as_you_type() {
    let mut h = filter_bar();
    // the keyword picker takes several keywords
    // Issue #213: the keyword picker takes several keywords. It stays open while they are clicked, shows any of them,
    // and "Any keyword" clears them all.
    {
        let keywords = h.app.caches.filter_values(&h.app.session.catalog).keywords.clone();
        assert!(keywords.len() >= 2, "the demo library has keywords: {keywords:?}");
        let all = h.app.session.visible_cloned().len();
        ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-keyword"}));
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:1"}));
        assert_eq!(h.app.session.filter.keywords, vec![keywords[0].clone()]);
        let one = h.app.session.visible_cloned().len();
        assert!(rows(&h, "keyword").len() > 1, "the list stays open for another keyword");
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:2"}));
        assert_eq!(h.app.session.filter.keywords, vec![keywords[0].clone(), keywords[1].clone()]);
        let two = h.app.session.visible_cloned().len();
        assert!(one <= two && two <= all, "any of them: {one} ≤ {two} ≤ {all}");
        // a second click takes one off again
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:1"}));
        assert_eq!(h.app.session.filter.keywords, vec![keywords[1].clone()]);
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:0"}));
        assert!(h.app.session.filter.keywords.is_empty() && h.app.session.filter.keyword.is_none());
        assert_eq!(h.app.session.visible_cloned().len(), all);
        assert!(rows(&h, "keyword").is_empty(), "Any keyword closes the list");
    }
    // the keyword picker toggles between or and and
    // Issue #683: the keyword picker (and each Metadata column of a multi-value field) carries an OR / AND toggle:
    // "Match all (AND)" switches the column's operator. The operator rows sit at the end of the picker's rows.
    {
        let keywords = h.app.caches.filter_values(&h.app.session.catalog).keywords.clone();
        assert!(keywords.len() >= 2, "the demo library has keywords: {keywords:?}");
        ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-keyword"}));
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:1"}));
        assert_eq!(h.app.session.filter.keyword_op, lcc_catalog::MatchOp::Any, "OR is the default");
        let or = h.app.session.visible_cloned().len();
        // the operator rows are the picker's last two; type a search that keeps only a few rows, so the
        // rows at the end are laid out (a long list culls the ones below the fold)
        ok(&mut h, "ui.text", json!({"text": keywords[0]}));
        let ids = |h: &Headless| -> Vec<u32> {
            let mut v: Vec<u32> = rows(h, "keyword").iter().filter_map(|w| w.rsplit(':').next().and_then(|n| n.parse().ok())).collect();
            v.sort_unstable();
            v
        };
        let all_row = ids(&h).last().copied().unwrap();
        ok(&mut h, "ui.clickWidget", json!({"id": format!("filterItem:keyword:{all_row}")}));
        assert_eq!(h.app.session.filter.keyword_op, lcc_catalog::MatchOp::All, "the toggle switched to AND");
        assert!(h.app.session.visible_cloned().len() <= or, "AND with one keyword matches no more than OR");
        // back to OR
        let any_row = all_row - 1;
        ok(&mut h, "ui.clickWidget", json!({"id": format!("filterItem:keyword:{any_row}")}));
        assert_eq!(h.app.session.filter.keyword_op, lcc_catalog::MatchOp::Any);
        // "Any keyword" clears the choice and puts the operator back to OR, so the next pick doesn't start in AND
        ok(&mut h, "ui.clickWidget", json!({"id": format!("filterItem:keyword:{all_row}")}));
        assert_eq!(h.app.session.filter.keyword_op, lcc_catalog::MatchOp::All);
        ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:keyword:0"}));
        assert!(h.app.session.filter.keywords.is_empty(), "Any keyword clears the values");
        assert_eq!(h.app.session.filter.keyword_op, lcc_catalog::MatchOp::Any, "and the operator");
    }
    // typing narrows the keyword rows and keeps any
    {
        let keywords = h.app.caches.filter_values(&h.app.session.catalog).keywords.clone();
        assert!(keywords.len() >= 2, "the demo library has keywords: {keywords:?}");
        let target = keywords[0].clone();
        ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-keyword"}));
        let all = rows(&h, "keyword");
        // every keyword, Any, and the column's two operator rows (Match any / Match all, issue #683)
        assert_eq!(all.len(), keywords.len() + 3, "every keyword, Any and the operator rows");
        // no click into the box first: it has the focus when the dropdown opens
        ok(&mut h, "ui.text", json!({"text": target.to_uppercase()}));
        let narrowed = rows(&h, "keyword");
        let matching = keywords.iter().filter(|k| k.to_lowercase().contains(&target.to_lowercase())).count();
        assert_eq!(narrowed.len(), matching + 3, "matches (any case), Any and the operator rows: {narrowed:?}");
        assert!(narrowed.contains(&"filterItem:keyword:0".to_string()), "the Any row stays");
        assert!(narrowed.len() < all.len());
        // closed and opened again: the search starts empty, and other pickers never saw it
        ok(&mut h, "ui.key", json!({"key": "Escape"}));
        ok(&mut h, "ui.key", json!({"key": "Escape"}));
        ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-camera"}));
        let cameras = h.app.caches.filter_values(&h.app.session.catalog).cameras.len();
        assert_eq!(
            rows(&h, "camera").len(),
            cameras + 1,
            "the camera picker (no operator rows: a photo has one camera) isn't filtered by the keyword search"
        );
    }
}

/// Issue #218: the filter bar's Metadata columns list each field's values with counts; a click narrows the grid,
/// the next column counts only what is left, and "All" clears the column again.
#[test]
fn metadata_columns_narrow_the_grid_and_cascade() {
    let mut h = filter_bar();
    let all = h.app.session.visible_cloned().len();
    ok(&mut h, "ui.clickWidget", json!({"id": "button:filterColumns"}));
    assert!(!h.app.ui.filter_columns.is_empty(), "the columns show");
    // column 1 lists cameras, column 2 lenses
    ok(&mut h, "engine.execute", json!({"command": "view.filterColumns", "params": {"column": 1, "field": "camera"}}));
    ok(&mut h, "engine.execute", json!({"command": "view.filterColumns", "params": {"column": 2, "field": "keyword"}}));
    let column_rows = |h: &Headless, c: usize| {
        let prefix = format!("filterValue:{c}:");
        h.app.widgets.iter().filter(|(w, _)| w.starts_with(&prefix)).count()
    };
    let keywords_before = column_rows(&h, 2);
    assert!(column_rows(&h, 1) > 2, "All and at least two cameras");
    // the first camera: the grid narrows to its photos
    ok(&mut h, "ui.clickWidget", json!({"id": "filterValue:1:1"}));
    let cameras = h.app.session.filter.cameras.clone();
    assert_eq!(cameras.len(), 1, "{:?}", h.app.session.filter);
    let narrowed = h.app.session.visible_cloned();
    assert!(!narrowed.is_empty() && narrowed.len() < all, "{} of {all}", narrowed.len());
    assert!(narrowed.iter().all(|id| h.app.session.catalog.photo(*id).unwrap().meta.camera.eq_ignore_ascii_case(&cameras[0])));
    // the keyword column counts only those photos' keywords
    assert!(column_rows(&h, 2) <= keywords_before, "the keyword column cascades");
    // the camera column still offers the other cameras
    assert!(column_rows(&h, 1) > 2);
    // All clears the camera column
    ok(&mut h, "ui.clickWidget", json!({"id": "filterValue:1:0"}));
    assert!(h.app.session.filter.cameras.is_empty());
    assert_eq!(h.app.session.visible_cloned().len(), all);
    // the field picker in a column header
    ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filterColumn0"}));
    ok(&mut h, "ui.clickWidget", json!({"id": "filterColumnField:0:flag"}));
    assert_eq!(h.app.ui.filter_columns[0], "flag");
    // bad arguments are errors
    for p in [
        json!({"column": 9, "field": "camera"}),
        json!({"column": 0, "field": "shoe"}),
        json!({"column": 0}),
        json!({"count": 0}),
        json!({"show": "yes"}),
    ] {
        let r = h.request("engine.execute", json!({"command": "view.filterColumns", "params": p}), T);
        assert_eq!(r["ok"], false, "{p}: {r}");
    }
    // hidden again
    ok(&mut h, "ui.clickWidget", json!({"id": "button:filterColumns"}));
    assert!(h.app.ui.filter_columns.is_empty());
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "filterColumns"));
}

/// Issue #218: each Metadata column has a sticky search box under its header. Typing narrows that column's rows at
/// once (any case), another column keeps its rows, the grid's photos stay put, an emptied box brings every row back,
/// and turning the columns — or the filter bar — off forgets what was typed.
#[test]
fn a_metadata_columns_search_narrows_its_rows() {
    let mut h = filter_bar();
    let column_rows = |h: &Headless, c: usize| {
        let prefix = format!("filterValue:{c}:");
        h.app.widgets.iter().filter(|(w, _)| w.starts_with(&prefix)).count()
    };
    let visible = h.app.session.visible_cloned().len();
    ok(&mut h, "ui.clickWidget", json!({"id": "button:filterColumns"}));
    // column 1 lists cameras
    ok(&mut h, "engine.execute", json!({"command": "view.filterColumns", "params": {"column": 1, "field": "camera"}}));
    let all = column_rows(&h, 1);
    assert!(all > 2, "All and at least two cameras: {all}");
    let years = column_rows(&h, 0);
    let cameras = h.app.caches.filter_values(&h.app.session.catalog).cameras.clone();
    assert!(cameras.len() >= 2, "the demo library has cameras: {cameras:?}");
    let target = cameras[0].clone();
    let matching = cameras.iter().filter(|c| c.to_lowercase().contains(&target.to_lowercase())).count();
    // a click into the box, then the typing narrows the rows at once
    ok(&mut h, "ui.clickWidget", json!({"id": "field:filterColumnSearch:1"}));
    ok(&mut h, "ui.text", json!({"text": target.to_uppercase()}));
    assert_eq!(h.app.ui.filter_column_search[1], target.to_uppercase(), "the column keeps what is typed");
    let narrowed = column_rows(&h, 1);
    // the matches (any case), All, and the Unknown row when the photos have a camera-less one
    assert!(narrowed < all, "{narrowed} of {all} rows");
    assert!((matching + 1..=matching + 2).contains(&narrowed), "{narrowed} rows for {matching} of {cameras:?}");
    // the other column and the grid are untouched
    assert_eq!(column_rows(&h, 0), years, "the next column keeps its rows");
    assert_eq!(h.app.session.visible_cloned().len(), visible, "searching a column does not filter the photos");
    // emptied, every row is back
    ok(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
    ok(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert_eq!(h.app.ui.filter_column_search[1], "", "the emptied box is kept as it is");
    assert_eq!(column_rows(&h, 1), all);
    // typing again, then the columns go away: the search goes with them
    ok(&mut h, "ui.text", json!({"text": target.to_uppercase()}));
    assert_eq!(h.app.ui.filter_column_search[1], target.to_uppercase());
    ok(&mut h, "ui.clickWidget", json!({"id": "button:filterColumns"}));
    assert!(h.app.ui.filter_columns.is_empty());
    assert!(h.app.ui.filter_column_search.is_empty(), "hiding the columns forgets their searches");
    // and once more with the filter bar itself
    ok(&mut h, "ui.clickWidget", json!({"id": "button:filterColumns"}));
    ok(&mut h, "engine.execute", json!({"command": "view.filterColumns", "params": {"column": 0, "field": "camera"}}));
    assert_eq!(column_rows(&h, 0), all);
    ok(&mut h, "ui.clickWidget", json!({"id": "field:filterColumnSearch:0"}));
    ok(&mut h, "ui.text", json!({"text": target.to_uppercase()}));
    assert!(column_rows(&h, 0) < all);
    ok(&mut h, "engine.execute", json!({"command": "view.filterBar"}));
    assert!(!h.app.ui.filter_bar);
    assert!(h.app.ui.filter_column_search.is_empty(), "hiding the filter bar forgets the column searches");
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("filterValue:")), "no column is drawn");
}

/// The People view's search box narrows the cards to the names that hold what is typed (any case) while the unnamed
/// faces below stay; an emptied box, or the People button, brings everyone back.
#[test]
fn the_peoples_search_narrows_the_cards_by_name() {
    use lcc_catalog::Op;
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(2).collect();
    for (id, name) in [(ids[0], "Jane Doe"), (ids[1], "John Roe")] {
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: Some(name.to_string()),
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }
    let cards = |h: &Headless| -> Vec<String> { h.app.widgets.iter().filter(|(w, _)| w.starts_with("person:")).map(|(w, _)| w.clone()).collect() };
    ok(&mut h, "engine.execute", json!({"command": "view.people"}));
    let both = cards(&h);
    assert!(both.contains(&"person:Jane Doe".to_string()) && both.contains(&"person:John Roe".to_string()), "{both:?}");
    // a name typed into the box narrows the cards to it
    ok(&mut h, "ui.clickWidget", json!({"id": "field:peopleSearch"}));
    ok(&mut h, "ui.text", json!({"text": "JANE"}));
    assert_eq!(h.app.ui.people_search, "JANE");
    let jane = cards(&h);
    assert!(jane.contains(&"person:Jane Doe".to_string()), "{jane:?}");
    assert!(!jane.contains(&"person:John Roe".to_string()), "the other person's card is gone: {jane:?}");
    // emptied, everyone is back
    ok(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
    ok(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert_eq!(h.app.ui.people_search, "");
    assert_eq!(cards(&h).len(), both.len(), "every card again");
    // a name no one has leaves no card, and the box to change it
    ok(&mut h, "ui.text", json!({"text": "zzz"}));
    assert!(cards(&h).is_empty(), "no card matches: {:?}", cards(&h));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "field:peopleSearch"), "the box stays");
    // the People button starts everyone afresh
    ok(&mut h, "engine.execute", json!({"command": "view.people"}));
    assert_eq!(h.app.ui.people_search, "");
    assert!(cards(&h).contains(&"person:Jane Doe".to_string()) && cards(&h).contains(&"person:John Roe".to_string()));
}

/// Issue #538: the pickers' cameras, lenses and keywords are listed again only when metadata changes. A rating
/// rebuilt them from every photo, which made scrolling a large library crawl while the filter bar showed and
/// anything else in the catalog changed. Each value is listed once, whatever other spellings sort between.
#[test]
fn picker_values_survive_changes_to_other_things() {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let mut s = lcc_engine::Session::new();
    for (i, camera) in ["NIKON Z 6_2", "Canon EOS R6", "NIKON Z 6_2", "nikon z 6_2", "NIKON Z 6_2"].into_iter().enumerate() {
        let n = i as u64 + 1;
        let mut p = Photo::new(
            PhotoId(n),
            Source::File { path: format!("/filter-values/{n}.jpg") },
            &format!("{n}.jpg"),
            "JPEG",
            60,
            40,
            "2026-01-01T00:00:00",
        );
        p.meta.camera = camera.to_string();
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut app = LccApp::new(s, Services { png: None, ..Default::default() });
    let before = app.caches.filter_values(&app.session.catalog);
    assert_eq!(before.cameras, ["Canon EOS R6", "NIKON Z 6_2", "nikon z 6_2"]);
    app.session.catalog.apply(Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
    let after = app.caches.filter_values(&app.session.catalog);
    assert!(std::sync::Arc::ptr_eq(&before, &after), "a rating changes no camera, lens or keyword");
    let mut meta = app.session.catalog.photo(PhotoId(2)).unwrap().meta.clone();
    meta.camera = "ILCE-1".into();
    app.session.catalog.apply(Op::SetMeta { id: PhotoId(2), meta: Box::new(meta) }).unwrap();
    assert_eq!(app.caches.filter_values(&app.session.catalog).cameras, ["ILCE-1", "NIKON Z 6_2", "nikon z 6_2"]);
}

/// Issue #487: the top bar's AI search toggle switches the search box to natural-language (semantic) search,
/// narrows the grid by image-text similarity, shows a removable `AI search: “…”` chip, and lists the
/// Semantic search model in Settings ▸ AI Models.
#[test]
fn semantic_search_toggle_filters_the_grid_and_shows_chip_and_ai_models_section() {
    let d = lcc_testkit::TempDir::new("lcc-ui-semantic");
    let _ = std::fs::remove_dir_all(&d);
    let models_dir = d.join("ml-models");
    let src = d.join("synthetic-src");
    lcc_engine::ml::write_synthetic_clip_dir(&src, "test-clip").unwrap();

    let mut s = lcc_engine::Session::with_demo();
    s.ml_models_dir = Some(models_dir);
    s.execute("ml.models.install", &json!({"path": src.to_string_lossy(), "acknowledged": true})).unwrap();
    s.execute("ml.index", &json!({"wait": true})).unwrap();

    let app = LccApp::new(s, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.settle(SETTLE);
    let all = h.app.session.visible_cloned().len();
    assert!(all >= 2);

    // Toggle AI search on via the top bar's `search:ai` button
    assert!(!h.app.ui.semantic_search);
    ok(&mut h, "ui.clickWidget", json!({"id": "search:ai"}));
    assert!(h.app.ui.semantic_search);

    // Type a natural-language description into the search box
    ok(&mut h, "ui.clickWidget", json!({"id": "field:search"}));
    ok(&mut h, "ui.text", json!({"text": "golden sunset"}));
    assert_eq!(h.app.session.filter.semantic.as_deref(), Some("golden sunset"));
    let narrowed = h.app.session.visible_cloned().len();
    assert!(narrowed > 0 && narrowed < all, "semantic search narrowed {all} demo photos to {narrowed}");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "chip:0"), "removable AI search filter chip shown");

    // Removing the chip clears the semantic filter and the search box
    ok(&mut h, "ui.clickWidget", json!({"id": "chip:0"}));
    assert!(h.app.session.filter.semantic.is_none());
    assert_eq!(h.app.ui.search, "");
    assert_eq!(h.app.session.visible_cloned().len(), all);

    // Settings ▸ AI Models (`dialog.aiModels`) shows the Semantic search model row
    ok(&mut h, "engine.execute", json!({"command": "dialog.aiModels"}));
    assert!(
        h.app.widgets.iter().any(|(w, _)| w.starts_with("ml:download:") || w.starts_with("ml:remove:")),
        "Semantic search model controls shown in AI Models dialog: {:?}",
        h.app.widgets.iter().map(|(w, _)| w).filter(|w| w.starts_with("ml:")).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn apply_settings_adopts_saved_ml_device_and_retries_on_error() {
    let d = lcc_testkit::TempDir::new("lcc-ui-ml-dev");
    let _ = std::fs::remove_dir_all(&d);
    let models_dir = d.join("ml-models");

    // Engine session 1 saves device="cpu" to ml-models/settings.json
    let mut s1 = lcc_engine::Session::new();
    s1.ml_models_dir = Some(models_dir.clone());
    s1.execute("ml.status", &json!({"device": "cpu"})).unwrap();

    // Session 2 starts with an empty AppSettings.ml_device: apply_settings adopts the engine-saved "cpu"
    let mut s2 = lcc_engine::Session::new();
    s2.ml_models_dir = Some(models_dir.clone());
    let mut app = LccApp::new(s2, Services { png: None, ..Default::default() });
    let ctx = egui::Context::default();
    assert_eq!(app.ui.settings.ml_device, "");
    app.apply_settings(&ctx);
    assert_eq!(app.ui.settings.ml_device, "cpu");

    // An unknown device errors in ml.status and does not mark ml_device_applied, so a subsequent valid setting applies
    app.ui.settings.ml_device = "vulkan:10de:9999:Missing#1".into();
    app.apply_settings(&ctx);
    let st = app.session.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["deviceChoice"], "cpu", "failed ml.status leaves engine choice untouched");

    // Executing ml.status with device="auto" via run_typed updates both engine and AppSettings.ml_device
    app.ui.settings.ml_device = String::new();
    app.apply_settings(&ctx);
    let st = app.session.execute("ml.status", &json!({})).unwrap();
    assert_eq!(st["deviceChoice"], "auto");

    let _ = std::fs::remove_dir_all(&d);
}

/// Semantic search switched off (Settings ▸ AI Models ▸ Semantic search): the search box loses its AI button, an AI
/// search mode left on falls back to text search, and switching it on again brings the button back.
#[test]
fn switching_semantic_search_off_takes_the_ai_button_away() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.settle(SETTLE);
    let has_ai = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "search:ai");
    ok(&mut h, "ui.clickWidget", json!({"id": "search:ai"}));
    assert!(h.app.ui.semantic_search);
    ok(&mut h, "engine.execute", json!({"command": "ml.enable", "params": {"enabled": false}}));
    h.step();
    h.step();
    assert!(!has_ai(&h), "no AI button while semantic search is off");
    assert!(!h.app.ui.semantic_search, "the search box searches the text");
    ok(&mut h, "engine.execute", json!({"command": "ml.enable", "params": {"enabled": true}}));
    h.step();
    h.step();
    assert!(has_ai(&h), "the button is back");
}
