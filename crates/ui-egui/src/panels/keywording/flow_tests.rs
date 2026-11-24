//! Reusable behavioral suite: include beside keywording.rs in either source tree.
use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: Value) {
    let answer = h.request(method, params.clone(), T);
    assert_eq!(answer["ok"], true, "{method} {params}: {answer}");
    h.settle(SETTLE);
}

fn setup() -> (Headless, Vec<u64>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let photos: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    assert_eq!(photos.len(), 3);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    for id in &photos {
        ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [id], "keywords": ["existing"]}}));
    }
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &photos[..2]}}));
    (h, photos)
}

fn keywords(h: &Headless, id: u64) -> Vec<String> {
    h.app.session.catalog.photo(PhotoId(id)).unwrap().meta.keywords.clone()
}

fn type_keywords(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:keyword"}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "the actual current add field has focus");
    ask(h, "ui.text", json!({"text": text}));
}

#[test]
fn add_is_one_transaction_for_selected_photos_and_undo_restores_them() {
    let (mut h, photos) = setup();
    let before: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
    let undo = h.app.session.undo.len();
    type_keywords(&mut h, "port trip, sunset");
    assert_eq!(h.app.session.undo.len(), undo, "typing is not a document transaction");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.session.undo.len(), undo + 1);
    for id in &photos[..2] {
        let now = keywords(&h, *id);
        assert!(now.contains(&"port trip".to_owned()) && now.contains(&"sunset".to_owned()), "{now:?}");
        assert!(now.contains(&"existing".to_owned()));
    }
    assert_eq!(keywords(&h, photos[2]), before[2], "unselected photo is untouched");
    ask(&mut h, "engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
}

#[test]
fn escape_discards_new_draft_without_writing_history() {
    let (mut h, photos) = setup();
    let before: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
    let undo = h.app.session.undo.len();
    type_keywords(&mut h, "never commit");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "this Enter must reach the actual keyword field");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Enter must not escape to the grid's open-Detail shortcut");
    assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
    assert_eq!(h.app.session.undo.len(), undo);
    type_keywords(&mut h, "after escape");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.session.undo.len(), undo + 1);
    for (at, id) in photos.iter().enumerate() {
        assert_eq!(keywords(&h, *id).contains(&"after escape".to_owned()), at < 2);
    }
}

#[test]
fn changing_selection_does_not_retarget_an_unfinished_draft() {
    let (mut h, photos) = setup();
    let before: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
    let undo = h.app.session.undo.len();
    type_keywords(&mut h, "belongs to original selection");
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [photos[2]]}}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "this Enter must reach the actual keyword field");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Enter must not escape to the grid's open-Detail shortcut");
    assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
    assert_eq!(h.app.session.undo.len(), undo);
    type_keywords(&mut h, "fresh third photo keyword");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.session.undo.len(), undo + 1);
    for (at, id) in photos.iter().enumerate() {
        assert_eq!(keywords(&h, *id).contains(&"fresh third photo keyword".to_owned()), at == 2);
    }
    let after_third: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
    // Returning to the old selection must not revive its prior SDK draft either.
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &photos[..2]}}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "this Enter must reach the actual keyword field");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Enter must not escape to the grid's open-Detail shortcut");
    assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), after_third);
    type_keywords(&mut h, "fresh original selection keyword");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.session.undo.len(), undo + 2);
    for (at, id) in photos.iter().enumerate() {
        assert_eq!(keywords(&h, *id).contains(&"fresh original selection keyword".to_owned()), at < 2);
    }
}

#[test]
fn escape_restores_draft_kept_by_an_earlier_blur() {
    let (mut h, photos) = setup();
    let undo = h.app.session.undo.len();
    type_keywords(&mut h, "kept on blur");
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    assert!(
        !crate::panels::keywording::field_focused(&h.view.ctx),
        "Tab must end the first edit before Escape can restore the second edit's starting draft"
    );
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.text", json!({"text": ", discarded suffix"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    let draft = h.view.ctx.data(|d| d.get_temp::<String>(egui::Id::unique("kw-input"))).unwrap_or_default();
    assert_eq!(draft, "kept on blur", "Escape restores the UI draft before Enter can write any photo metadata");
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "this Enter must reach the actual keyword field");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Enter must not escape to the grid's open-Detail shortcut");
    assert_eq!(h.app.session.undo.len(), undo + 1);
    for id in &photos[..2] {
        let now = keywords(&h, *id);
        assert!(now.contains(&"kept on blur".to_owned()), "{now:?}");
        assert!(!now.contains(&"discarded suffix".to_owned()), "{now:?}");
    }
}

#[test]
fn native_clipboard_menu_still_adds_to_the_selection() {
    let (mut h, photos) = setup();
    h.view.clipboard = "pasted keyword".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword:paste"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(photos[..2].iter().all(|id| keywords(&h, *id).contains(&"pasted keyword".to_owned())));
    assert!(!keywords(&h, photos[2]).contains(&"pasted keyword".to_owned()));
}

fn no_keyword_offers(h: &Headless) {
    assert!(!h.app.widgets.iter().any(|(id, _)| id.starts_with("kwComplete")), "hidden or reopened field has no stale popup");
}

fn hide_keywords(h: &mut Headless, fold: bool) {
    if fold {
        ask(h, "ui.clickWidget", json!({"id": "section:keywording"}));
    } else {
        ask(h, "ui.set", json!({"right": "none"}));
    }
}

fn show_keywords(h: &mut Headless, fold: bool) {
    if fold {
        ask(h, "ui.clickWidget", json!({"id": "section:keywording"}));
    } else {
        ask(h, "ui.set", json!({"right": "keywords"}));
    }
}

#[test]
fn same_owner_reopening_keeps_text_but_requires_new_focus_before_submit() {
    for fold in [true, false] {
        for escape in [false, true] {
            let (mut h, photos) = setup();
            let undo = h.app.session.undo.len();
            type_keywords(&mut h, "retained hidden draft");
            hide_keywords(&mut h, fold);
            assert!(!crate::panels::keywording::field_focused(&h.view.ctx), "hidden field cannot keep focus");
            no_keyword_offers(&h);
            ask(&mut h, "ui.key", json!({"key": "Enter"}));
            assert_eq!(
                h.app.session.selected(),
                photos[..2].iter().map(|id| PhotoId(*id)).collect::<Vec<_>>(),
                "hidden Enter must not reassign targets"
            );
            assert_eq!(h.app.session.undo.len(), undo, "Enter while hidden cannot submit keywords");
            assert!(photos.iter().all(|id| !keywords(&h, *id).contains(&"retained hidden draft".to_owned())));
            // A hidden field observes only the target set when reopened. Changing away and back
            // while hidden deliberately preserves its same-owner draft, not the intermediate set.
            ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [photos[2]]}}));
            ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &photos[..2]}}));
            // Enter without a text field may open Detail; return to the starting view before reopening.
            ask(&mut h, "ui.set", json!({"view": "photoGrid"}));
            show_keywords(&mut h, fold);
            assert!(!crate::panels::keywording::field_focused(&h.view.ctx), "reopening must not restore stale field focus");
            no_keyword_offers(&h);
            ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
            assert!(crate::panels::keywording::field_focused(&h.view.ctx), "explicit click focuses the new SDK field");
            assert!(h.app.widgets.iter().any(|(id, _)| id == "kwCompleteCreate"), "fresh focus offers the preserved new keyword, not a stale popup");
            if escape {
                ask(&mut h, "ui.key", json!({"key": "End"}));
                ask(&mut h, "ui.text", json!({"text": ", discarded after reopen"}));
                ask(&mut h, "ui.key", json!({"key": "Escape"}));
                ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
            }
            assert!(crate::panels::keywording::field_focused(&h.view.ctx), "this Enter reaches the actual keyword field");
            ask(&mut h, "ui.key", json!({"key": "Enter"}));
            assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
            assert_eq!(h.app.session.undo.len(), undo + 1, "same-owner draft remains available after explicit focus");
            for (at, id) in photos.iter().enumerate() {
                assert_eq!(keywords(&h, *id).contains(&"retained hidden draft".to_owned()), at < 2);
                assert!(!keywords(&h, *id).contains(&"discarded after reopen".to_owned()));
            }
            type_keywords(&mut h, "after reopening");
            ask(&mut h, "ui.key", json!({"key": "Enter"}));
            assert_eq!(h.app.session.undo.len(), undo + 2);
            for (at, id) in photos.iter().enumerate() {
                assert_eq!(keywords(&h, *id).contains(&"after reopening".to_owned()), at < 2);
            }
        }
    }
}

#[test]
fn hidden_drafts_are_discarded_when_photo_targets_or_library_change() {
    for change in ["selection", "deleted", "library"] {
        let (mut h, mut photos) = setup();
        type_keywords(&mut h, "old targets only");
        hide_keywords(&mut h, true);
        match change {
            "selection" => ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [photos[2]]}})),
            "deleted" => {
                ask(&mut h, "engine.execute", json!({"command": "photo.delete", "params": {"ids": [photos[1]]}}));
                assert_eq!(h.app.session.selected(), vec![PhotoId(photos[0])], "the selected photo set shrank");
            }
            _ => {
                let library = h.app.session.library_generation();
                let stores = lightcraft_engine::library::LibraryStores {
                    dir: "keyword-draft-replacement".into(),
                    catalog: Box::new(lightcraft_catalog::MemStore::new()),
                    files: Box::new(lightcraft_catalog::MemStore::new()),
                    on_disk: false,
                };
                h.app.session.open_library_in(stores, true).unwrap();
                assert_ne!(h.app.session.library_generation(), library);
                photos = h.app.session.visible_cloned().iter().take(3).map(|id| id.0).collect();
                ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &photos[..2]}}));
            }
        }
        let target: Vec<u64> = h.app.session.selected().iter().map(|id| id.0).collect();
        assert!(!target.is_empty());
        let before: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
        let undo = h.app.session.undo.len();
        show_keywords(&mut h, true);
        assert!(!crate::panels::keywording::field_focused(&h.view.ctx));
        no_keyword_offers(&h);
        ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
        assert!(crate::panels::keywording::field_focused(&h.view.ctx), "negative Enter reaches the actual field");
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
        assert_eq!(h.app.session.undo.len(), undo, "stale draft cannot create history for {change}");
        assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
        type_keywords(&mut h, "new targets only");
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        assert_eq!(h.app.session.undo.len(), undo + 1);
        for id in &photos {
            assert_eq!(keywords(&h, *id).contains(&"new targets only".to_owned()), target.contains(id), "positive target control for {change}");
        }
    }
}

#[test]
fn tab_between_add_and_painter_preserves_distinct_drafts_and_native_caret_edits() {
    let (mut h, photos) = setup();
    let before: Vec<_> = photos.iter().map(|id| keywords(&h, *id)).collect();
    let undo = h.app.session.undo.len();
    type_keywords(&mut h, "kept add draft");
    // The four view choices are real keyboard stops between the two text fields.
    // Follow their displayed order and assert the actual focused owner at every step.
    let through_views = |h: &mut Headless, reverse: bool| {
        let names = if reverse { ["willExport", "containing", "keywords", "enter"] } else { ["enter", "keywords", "containing", "willExport"] };
        for name in names {
            let key = format!("keywordView:{name}");
            let expected = h.app.widget_hits.iter().find(|(k, _)| k == &key).expect("real view choice").1.id;
            // (the keyword chips are real stops too, between the last view choice and the painter's field)
            for _ in 0..8 {
                ask(h, "ui.key", json!({"key": "Tab", "shift": reverse}));
                {
                    let sid = craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|r| r.stop.id);
                    let name = h.app.widget_hits.iter().find(|(_, hit)| Some(hit.id) == sid).map(|(n, _)| n.clone());
                    eprintln!("DBGTAB want {key} expected={expected:?} stop={sid:?} name={name:?}");
                }
                if craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|r| r.stop.id) == Some(expected) {
                    break;
                }
            }
            assert_eq!(craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|r| r.stop.id), Some(expected), "Tab at {name}");
            assert_eq!(h.app.ui.keywording_view, crate::state::KeywordingView::Keywords, "focus alone does not select a view");
        }
    };
    through_views(&mut h, false);
    let painter = crate::text_field::id("field:keywordPainter");
    for _ in 0..8 {
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        if craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|rec| rec.stop.id) == Some(painter) {
            break;
        }
    }
    assert_eq!(craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|rec| rec.stop.id), Some(painter));
    ask(&mut h, "ui.text", json!({"text": "Weddings"}));
    through_views(&mut h, true);
    ask(&mut h, "ui.key", json!({"key": "Tab", "shift": true}));
    assert!(crate::panels::keywording::field_focused(&h.view.ctx), "Shift+Tab returns to the real add field");
    assert_eq!(h.app.ui.keyword_painter_text, "Weddings");
    let draft = h.view.ctx.data(|d| d.get_temp::<String>(egui::Id::unique("kw-input"))).unwrap_or_default();
    assert_eq!(draft, "kept add draft", "the painter's edit never becomes an add-keyword draft");
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowLeft"}));
    ask(&mut h, "ui.text", json!({"text": "x"}));
    assert_eq!(h.app.ui.keyword_painter_text, "Weddingxs", "typing inserts at the actual caret");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.ui.keyword_painter_text, "Weddings", "Escape restores the painter's own last blurred draft");
    assert!(h.app.ui.keyword_painter.is_none());
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.keyword_painter.as_deref(), Some("Weddings"));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter"}));
    ask(&mut h, "ui.text", json!({"text": "disabled edit"}));
    assert_eq!(h.app.ui.keyword_painter_text, "Weddings", "the painter keyword is fixed while painting");
    assert_eq!(h.app.session.undo.len(), undo, "drafts and enabling the painter do not edit photos");
    assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
}

#[test]
fn keyword_view_changes_are_read_only_for_mixed_photo_selection() {
    let (mut h, photos) = setup();
    ask(&mut h, "engine.execute", json!({"command":"photo.setMeta", "params":{"ids":[photos[0]], "addKeywords":["Trips|Coast"]}}));
    let before = photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>();
    let selection = h.app.session.selection.clone();
    let undo = h.app.session.undo.len();
    for (id, visible) in [("containing", "keywordContaining:Trips"), ("willExport", "keywordExport:existing"), ("keywords", "keywordChip:existing")] {
        ask(&mut h, "ui.clickWidget", json!({"id": format!("keywordView:{id}")}));
        assert!(h.app.widgets.iter().any(|(name, _)| name == visible), "{visible}");
        assert_eq!(h.app.session.selection.ids, selection.ids);
        assert_eq!(h.app.session.selection.active, selection.active);
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(photos.iter().map(|id| keywords(&h, *id)).collect::<Vec<_>>(), before);
    }
}
