//! Real pointer/key activation of the two filter-bar actions; no command dispatch substitutes for the action under test.
use super::tests::{ask, fixture, visible};
use crate::headless::{Headless, HeadlessView};
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};
use lightcraft_catalog::PhotoId;
use serde_json::json;

const SAVE: &str = "button:filterSave";
const CLEAR: &str = "button:filterClear";

/// Whether the control takes a click: the framework leaves a disabled control's egui widget registered as enabled (so
/// it can keep keyboard focus), but it senses no click.
fn clickable(h: &Headless, widget: &str) -> bool {
    let r = response(h, widget);
    r.enabled() && r.sense.senses_click()
}

fn response(h: &Headless, widget: &str) -> egui::Response {
    let hit = &h.app.widget_hits.iter().find(|(id, _)| id == widget).unwrap().1;
    h.view.ctx.read_response(hit.id).unwrap_or_else(|| panic!("actual response for {widget}"))
}

fn filtered(width: f32) -> (Headless, Vec<u64>) {
    let mut h = fixture(width);
    let all = visible(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [102]}}));
    ask(&mut h, "engine.execute", json!({"command": "library.filter", "params": {"kind": "raw", "rating": 3, "text": "fixture"}}));
    h.app.ui.search = "fixture".into();
    h.step();
    assert_eq!(visible(&mut h), vec![102]);
    assert_eq!(h.app.ui.search, "fixture");
    (h, all)
}

/// Press and release on separate frames: the SDK must not run the action on press, or run twice on release.
fn pointer(h: &mut Headless, widget: &str, before_release: impl Fn(&Headless)) {
    let at = h.app.widgets.iter().find(|(id, _)| id == widget).unwrap().1.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE,
    });
    h.step();
    before_release(h);
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
}

#[test]
fn filterbar_actions_external155_default_disabled_pointer_actions_are_noops() {
    for width in [1100.0, 1900.0] {
        let mut h = fixture(width);
        let all = visible(&mut h);
        let undo = h.app.session.undo.len();
        let albums = h.app.session.catalog.albums().count();
        // Distinguishes a disabled Clear from one that wrongly resets UI-only draft text.
        h.app.ui.search = "unsubmitted draft".into();
        h.step();
        for widget in [SAVE, CLEAR] {
            assert!(!clickable(&h, widget), "{widget} is disabled");
            ask(&mut h, "ui.clickWidget", json!({"id": widget}));
            assert!(h.app.ui.dialog.is_none());
            assert_eq!(h.app.session.filter, Default::default());
            assert_eq!(h.app.ui.search, "unsubmitted draft");
            assert_eq!(visible(&mut h), all);
            assert_eq!(h.app.session.undo.len(), undo);
            assert_eq!(h.app.session.catalog.albums().count(), albums);
        }
    }
}

#[test]
fn filterbar_actions_external155_save_real_release_opens_existing_dialog_and_cancel_preserves_filter() {
    for width in [1100.0, 1900.0] {
        let (mut h, _) = filtered(width);
        let filter = h.app.session.filter.clone();
        let selection = h.app.session.selection.clone();
        let active = h.app.session.active();
        let albums = h.app.session.catalog.albums().count();
        let undo = h.app.session.undo.len();
        pointer(&mut h, SAVE, |h| assert!(h.app.ui.dialog.is_none(), "press alone does not activate Save"));
        assert!(matches!(h.app.ui.dialog, Some(Dialog::NewSmartAlbum { .. })));
        assert_eq!(h.app.session.catalog.albums().count(), albums, "Save opens a dialog; it does not create an album yet");
        assert_eq!(h.app.session.filter, filter);
        assert_eq!(h.app.session.selection, selection);
        assert_eq!(h.app.session.active(), active);
        assert_eq!(h.app.session.undo.len(), undo);
        ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogCancel"}));
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(h.app.session.filter, filter);
        assert_eq!(h.app.ui.search, "fixture");
        assert_eq!(visible(&mut h), vec![102]);
        assert_eq!(h.app.session.catalog.albums().count(), albums);
        assert_eq!(h.app.session.undo.len(), undo);
    }
}

#[test]
fn filterbar_actions_external155_save_click_then_existing_dialog_creates_one_album_with_exact_rules() {
    let (mut h, _) = filtered(1900.0);
    let filter = h.app.session.filter.clone();
    let before = h.app.session.catalog.albums().count();
    ask(&mut h, "ui.clickWidget", json!({"id": SAVE}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::NewSmartAlbum { .. })));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.session.catalog.albums().count(), before + 1);
    let album = h.app.session.catalog.albums().find(|album| album.is_smart()).unwrap();
    assert_eq!(album.smart.as_deref(), Some(&filter));
    assert_eq!(h.app.session.catalog.album_count(album.id), 1);
    assert_eq!(h.app.session.filter, filter);
}

#[test]
fn filterbar_actions_external155_clear_real_release_resets_complete_filter_search_and_shelved_state() {
    for width in [1100.0, 1900.0] {
        let (mut h, all) = filtered(width);
        let filter = h.app.session.filter.clone();
        // Adversarial fixture: the public toggle shelves the filter; restoring only the
        // public active filter keeps both states set. Ordinary UI does not create this pair.
        ask(&mut h, "engine.execute", json!({"command": "library.toggleFilters", "params": {}}));
        assert!(h.app.session.filters_off(), "public toggle really shelved the filter");
        h.app.session.filter = filter.clone();
        // (the toggle emptied the search field with the shelved filter; the restored filter brings its text back)
        h.app.ui.search = "fixture".into();
        h.step();
        assert_eq!(h.app.session.filter, filter);
        assert!(h.app.session.filters_off(), "fixture retains the shelved filter until Clear releases");
        let selection = h.app.session.selection.clone();
        let active = h.app.session.active();
        let undo = h.app.session.undo.len();
        pointer(&mut h, CLEAR, |h| {
            assert_eq!(h.app.session.filter, filter, "press alone does not activate Clear");
            assert_eq!(h.app.ui.search, "fixture");
            assert!(h.app.session.filters_off(), "press alone does not clear the shelved filter");
        });
        assert_eq!(h.app.session.filter, Default::default());
        assert!(!h.app.session.filters_off(), "library.clearFilter command clears the shelved filter too");
        assert_eq!(h.app.ui.search, "");
        assert_eq!(visible(&mut h), all, "all original identities and ordering return");
        assert_eq!(h.app.session.selection, selection);
        assert_eq!(h.app.session.active(), active);
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
        assert!(!clickable(&h, CLEAR));
        assert!(!clickable(&h, SAVE));
        ask(&mut h, "ui.key", json!({"key": "2"}));
        assert_eq!(h.app.session.catalog.photo(PhotoId(102)).unwrap().rating, 2, "mouse Clear releases keyboard ownership to grid shortcuts");
    }
}

#[test]
fn filterbar_actions_external155_keyboard_enter_and_space_run_actual_button_actions() {
    for key in ["Enter", "Space"] {
        for widget in [SAVE, CLEAR] {
            let (mut h, all) = filtered(1900.0);
            let filter = h.app.session.filter.clone();
            let target = response(&h, widget).id;
            h.view.ctx.memory_mut(|memory| memory.request_focus(target));
            h.step();
            assert!(h.view.ctx.memory(|memory| memory.has_focus(target)));
            ask(&mut h, "ui.key", json!({"key": key}));
            if widget == SAVE {
                assert!(matches!(h.app.ui.dialog, Some(Dialog::NewSmartAlbum { .. })));
                assert_eq!(h.app.session.filter, filter);
                assert_eq!(h.app.ui.search, "fixture");
            } else {
                assert!(h.app.ui.dialog.is_none());
                assert_eq!(h.app.session.filter, Default::default());
                assert_eq!(h.app.ui.search, "");
                assert_eq!(visible(&mut h), all);
            }
        }
    }
}

#[test]
fn filterbar_actions_external155_tab_reaches_real_save_and_clear_without_forced_focus() {
    for widget in [SAVE, CLEAR] {
        let (mut h, _) = filtered(1900.0);
        let target = response(&h, widget).id;
        let mut reached = false;
        for _ in 0..120 {
            ask(&mut h, "ui.key", json!({"key": "Tab"}));
            if h.view.ctx.memory(|memory| memory.has_focus(target)) {
                reached = true;
                break;
            }
        }
        assert!(reached, "natural Tab traversal reaches {widget}");
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        if widget == SAVE {
            assert!(matches!(h.app.ui.dialog, Some(Dialog::NewSmartAlbum { .. })));
        } else {
            assert_eq!(h.app.session.filter, Default::default());
            assert_eq!(h.app.ui.search, "");
        }
    }
}

#[test]
fn filterbar_actions_external155_targets_are_unique_real_buttons_with_nonoverlapping_geometry() {
    for width in [1100.0, 1900.0] {
        let (h, _) = filtered(width);
        let save = response(&h, SAVE);
        let clear = response(&h, CLEAR);
        let presets = h.app.widgets.iter().find(|(id, _)| id == "button:filterPresets").unwrap().1;
        for widget in [SAVE, CLEAR] {
            let shown = response(&h, widget);
            assert_eq!(h.app.widgets.iter().filter(|(id, _)| id == widget).count(), 1);
            assert_eq!(h.app.widget_hits.iter().filter(|(id, _)| id == widget).count(), 1);
            assert_eq!(shown.id, egui::Id::unique(widget), "locator belongs to the SDK input owner, not a hover-only probe");
            assert!(shown.enabled());
            assert!(shown.interact_rect.is_positive());
            assert!(shown.rect.is_finite());
        }
        assert!(!save.rect.intersects(clear.rect));
        assert!(!clear.rect.intersects(presets));
    }
}

#[test]
fn filterbar_actions_external155_inherited_disabled_button_keeps_target_but_cannot_activate() {
    let mut view = HeadlessView::new();
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.install_framework(&view.ctx);
    let mut at = egui::Pos2::ZERO;
    for frame in 0..5 {
        let events = if frame == 2 || frame == 3 {
            vec![egui::Event::PointerMoved(at), egui::Event::PointerButton {
                pos: at, button: egui::PointerButton::Primary, pressed: frame == 2, modifiers: egui::Modifiers::NONE,
            }]
        } else { vec![] };
        let raw = HeadlessView::raw_input(egui::vec2(500.0, 200.0), 1.0, frame as f64 / 60.0, events);
        view.run(raw, |ui| {
            crate::widgets::take_registries(ui.ctx());
            ui.add_enabled_ui(false, |ui| {
                assert!(!super::filter_action(ui, SAVE, "Save as Smart Album…", true));
            });
            let (rects, hits) = crate::widgets::take_registries(ui.ctx());
            assert_eq!(rects.len(), 1);
            assert_eq!(hits.len(), 1);
            at = rects[0].1.center();
            assert_eq!(hits[0].1.id, egui::Id::unique(SAVE));
            assert!(!ui.ctx().read_response(hits[0].1.id).unwrap().enabled());
        });
    }
}

#[test]
fn filterbar_actions_external155_sizing_and_sdk_measurement_publish_no_action_targets() {
    let mut view = HeadlessView::new();
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.install_framework(&view.ctx);
    let raw = HeadlessView::raw_input(egui::vec2(500.0, 200.0), 1.0, 0.0, vec![]);
    view.run(raw, |ui| {
        crate::widgets::take_registries(ui.ctx());
        let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        assert!(!super::filter_action(&mut sizing, SAVE, "Save as Smart Album…", true));
        assert!(!super::filter_action(&mut sizing, CLEAR, "Clear", true));
        let (rects, hits) = crate::widgets::take_registries(ui.ctx());
        assert!(rects.is_empty() && hits.is_empty());
        let ctx = ui.ctx().clone();
        craft::ui::runtime::measure::run(&ctx, || {
            assert!(!super::filter_action(ui, SAVE, "Save as Smart Album…", true));
            assert!(!super::filter_action(ui, CLEAR, "Clear", true));
        });
        let (rects, hits) = crate::widgets::take_registries(ui.ctx());
        assert!(rects.is_empty() && hits.is_empty());
    });
}

#[test]
fn filterbar_actions_external155_no_match_filter_keeps_save_and_clear_enabled() {
    let (mut h, all) = filtered(1900.0);
    ask(&mut h, "engine.execute", json!({"command": "library.filter", "params": {"rating": 5}}));
    assert!(visible(&mut h).is_empty());
    let filter = h.app.session.filter.clone();
    assert!(clickable(&h, SAVE));
    assert!(clickable(&h, CLEAR));
    ask(&mut h, "ui.clickWidget", json!({"id": SAVE}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::NewSmartAlbum { .. })));
    assert_eq!(h.app.session.filter, filter);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogCancel"}));
    ask(&mut h, "ui.clickWidget", json!({"id": CLEAR}));
    assert_eq!(h.app.session.filter, Default::default());
    assert_eq!(h.app.ui.search, "");
    assert_eq!(visible(&mut h), all);
}
