//! Behavioral tests for the actual SDK rows inside LightCraft's retained context popups.
use crate::headless::{Headless, HeadlessView};
use crate::state::{ViewMode, Zoom};
use crate::{LightcraftApp, Services};
use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

fn ask(h: &mut Headless, method: &str, params: Value) {
    let result = h.request(method, params, WAIT);
    assert_eq!(result["ok"], true, "{method}: {result}");
    h.settle(WAIT);
}

fn setup() -> (Headless, Vec<u64>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(4).map(|p| p.0).collect();
    assert_eq!(ids.len(), 4);
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &ids[..2], "active": ids[0]}}));
    ask(&mut h, "engine.execute", json!({"command": "view.compare"}));
    // Non-default navigation is a fixture, not part of the action being tested.
    h.app.ui.reference = Some(ids[3]);
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.app.ui.pan = (7.0, -5.0);
    h.settle(WAIT);
    assert_eq!(h.app.ui.compare, Some((ids[0], ids[1])));
    (h, ids)
}

fn open(h: &mut Headless, photo: u64, row: &str) {
    // At 100% with a nonzero pan, the painted photo's center can be off screen.
    // Aim at the part of this specific photo visible inside its real pane response.
    let name = format!("cull:{photo}");
    let image = h.app.widgets.iter().find(|(key, _)| key == &name).unwrap().1;
    let pane =
        (0..=1u8).find_map(|slot| h.view.ctx.read_response(egui::Id::unique(("cull-tile", slot, photo)))).expect("real pane for requested photo");
    let visible = pane.interact_rect.intersect(image);
    assert!(visible.is_positive(), "requested photo has no visible image area");
    let at = visible.center();
    ask(
        h,
        "ui.clickWidget",
        json!({"id": name, "button": "right",
        "fx": (at.x - image.left()) / image.width(), "fy": (at.y - image.top()) / image.height()}),
    );
    assert!(craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
    assert!(h.app.widget_hits.iter().any(|(id, _)| id == row), "real menu response registered for {row}");
}

fn selected(h: &Headless) -> Vec<u64> {
    h.app.session.selection.ids.iter().map(|p| p.0).collect()
}

// The snapshot includes identity and selection, not just the painted row labels.
fn snapshot(h: &Headless) -> Value {
    json!({
        "pair": h.app.ui.compare, "reference": h.app.ui.reference,
        "active": h.app.session.active().map(|p| p.0), "selected": selected(h),
        "zoom": format!("{:?}", h.app.ui.zoom), "pan": h.app.ui.pan,
        "view": format!("{:?}", h.app.ui.view), "undo": h.app.session.undo.len(),
    })
}

#[test]
fn compare_external155_swap_real_press_release_preserves_active_pin_and_navigation() {
    let (mut h, ids) = setup();
    let undo = h.app.session.undo.len();
    let canvas = h.app.canvas_rect;
    open(&mut h, ids[0], "compare:swap");
    let rect = h.app.widgets.iter().find(|(id, _)| id == "compare:swap").unwrap().1;
    let at = rect.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
        h.step();
    }
    h.settle(WAIT);
    assert_eq!(h.app.ui.compare, Some((ids[1], ids[0])));
    assert_eq!(h.app.session.active(), Some(PhotoId(ids[1])), "swap keeps the same photo active, not the same pane");
    assert_eq!(selected(&h), vec![ids[1], ids[0]]);
    assert_eq!(h.app.ui.reference, Some(ids[3]));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert_eq!(h.app.ui.pan, (7.0, -5.0));
    assert_eq!(h.app.canvas_rect, canvas);
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    assert_eq!(h.app.session.undo.len(), undo);
    assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
}

#[test]
fn compare_external155_make_select_actual_click_advances_exactly_once() {
    let (mut h, ids) = setup();
    let undo = h.app.session.undo.len();
    let canvas = h.app.canvas_rect;
    open(&mut h, ids[1], "compare:makeSelect");
    ask(&mut h, "ui.clickWidget", json!({"id": "compare:makeSelect"}));
    assert_eq!(h.app.ui.compare, Some((ids[1], ids[2])), "one activation, one candidate step");
    assert_eq!(h.app.session.active(), Some(PhotoId(ids[2])));
    assert_eq!(selected(&h), vec![ids[1], ids[2]]);
    assert_eq!(h.app.ui.reference, Some(ids[3]));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert_eq!(h.app.ui.pan, (7.0, -5.0));
    assert_eq!(h.app.canvas_rect, canvas);
    assert_eq!(h.app.session.undo.len(), undo);
    assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
}

#[test]
fn compare_external155_clear_reference_keyboard_activation_keeps_active_selection_and_pair() {
    let (mut h, ids) = setup();
    h.app.ui.view = ViewMode::Reference;
    h.settle(WAIT);
    let selection = selected(&h);
    let active = h.app.session.active();
    let pair = h.app.ui.compare;
    let undo = h.app.session.undo.len();
    open(&mut h, ids[3], "compare:clearReference");
    // Reach the real recorded MenuItem by natural keyboard travel (Tab into the menu, arrows between rows), then
    // deliver a real Enter through the control channel.
    let target = h.app.widget_hits.iter().find(|(id, _)| id == "compare:clearReference").unwrap().1.id;
    // At least one key first: Enter as the first key of a pointer-opened menu is withheld by its row.
    let mut pressed = false;
    for _ in 0..16 {
        let focused = craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|rec| rec.stop.id);
        if pressed && focused == Some(target) {
            break;
        }
        let in_menu = focused.is_some_and(|id| h.app.widget_hits.iter().any(|(name, hit)| name.starts_with("compare:") && hit.id == id));
        ask(&mut h, "ui.key", json!({"key": if in_menu { "ArrowDown" } else { "Tab" }}));
        pressed = true;
    }
    assert_eq!(craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|rec| rec.stop.id), Some(target), "the menu row holds the SDK focus");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.reference, None);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert_eq!(h.app.ui.compare, pair);
    assert_eq!(h.app.session.active(), active);
    assert_eq!(selected(&h), selection);
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert_eq!(h.app.ui.pan, (7.0, -5.0));
    assert_eq!(h.app.session.undo.len(), undo);
    assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
}

#[test]
fn compare_external155_escape_cancels_both_menus_without_retargeting_photos() {
    let (mut h, ids) = setup();
    for (view, photo, row) in [(ViewMode::Compare, ids[0], "compare:swap"), (ViewMode::Reference, ids[3], "compare:clearReference")] {
        h.app.ui.view = view;
        h.settle(WAIT);
        let before = snapshot(&h);
        open(&mut h, photo, row);
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
        assert_eq!(snapshot(&h), before, "opening and cancelling is not an action");
        assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
        assert!(!h.app.widgets.iter().any(|(id, _)| id == row), "dismissed rows are no longer targets");
        let stale = h.request("ui.clickWidget", json!({"id": row}), WAIT);
        assert_eq!(stale["ok"], false, "a dismissed target must not run: {stale}");
        assert_eq!(snapshot(&h), before);
    }
}

#[test]
fn compare_external155_disabled_real_row_does_not_activate() {
    let mut view = HeadlessView::new();
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.install_framework(&view.ctx);
    let mut at = egui::Pos2::ZERO;
    for frame in 0..5 {
        let events = if frame == 2 || frame == 3 {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: frame == 2, modifiers: egui::Modifiers::NONE },
            ]
        } else {
            vec![]
        };
        let raw = HeadlessView::raw_input(egui::vec2(400.0, 200.0), 1.0, frame as f64 / 60.0, events);
        view.run(raw, |ui| {
            crate::widgets::take_registries(ui.ctx());
            ui.add_enabled_ui(false, |ui| {
                assert!(!super::compare_action(ui, "compare:disabled-test", "Swap"));
            });
            let (rects, hits) = crate::widgets::take_registries(ui.ctx());
            at = rects.iter().find(|(id, _)| id == "compare:disabled-test").unwrap().1.center();
            assert!(hits.iter().any(|(id, _)| id == "compare:disabled-test"), "disabled row retains its actual no-op target");
        });
    }
}

#[test]
fn compare_external155_measurement_does_not_publish_action_targets() {
    let ctx = egui::Context::default();
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.install_framework(&ctx);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        crate::widgets::take_registries(&ctx);
        let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        assert!(!super::compare_action(&mut sizing, "compare:swap", "Swap"));
        let (rects, hits) = crate::widgets::take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty());
        craft::ui::runtime::measure::run(&ctx, || {
            assert!(!super::compare_action(ui, "compare:clearReference", "Clear Reference"));
        });
        let (rects, hits) = crate::widgets::take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty());
    });
    output.textures_delta.clear();
}

#[test]
fn compare_cancel_replace_candidate_then_promote_uses_new_pair_and_keeps_reference() {
    let (mut h, ids) = setup();
    let before = snapshot(&h);
    open(&mut h, ids[1], "compare:makeSelect");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(snapshot(&h), before);
    assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));

    // Change the candidate through the filmstrip, not by assigning the pair in the test.
    ask(&mut h, "ui.clickWidget", json!({"id": format!("film:{}", ids[2])}));
    assert_eq!(h.app.ui.compare, Some((ids[0], ids[2])));
    let changed = snapshot(&h);
    let stale = h.request("ui.clickWidget", json!({"id": "compare:makeSelect"}), WAIT);
    assert_eq!(stale["ok"], false, "a dismissed target must not promote the old candidate");
    assert_eq!(snapshot(&h), changed);

    open(&mut h, ids[2], "compare:makeSelect");
    ask(&mut h, "ui.clickWidget", json!({"id": "compare:makeSelect"}));
    assert_eq!(h.app.ui.compare, Some((ids[2], ids[3])));
    assert_eq!(selected(&h), vec![ids[2], ids[3]]);
    assert_eq!(h.app.session.active(), Some(PhotoId(ids[3])));
    assert_eq!(h.app.ui.reference, Some(ids[3]));
    assert_eq!(h.app.ui.pan, (7.0, -5.0));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert_eq!(snapshot(&h)["undo"], before["undo"]);
    assert!(!craft::ui::runtime::overlay::any_open(&h.view.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
}
