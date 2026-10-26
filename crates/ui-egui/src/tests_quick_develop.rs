//! Headless tests for Quick Develop panel and batch adjustments.

use serde_json::json;
use std::time::Duration;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    Headless::new(app, [1400.0, 900.0], 1.0)
}

#[test]
fn quick_develop_batch_exposure_nudge() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    assert!(vis.len() >= 3);

    // Select 3 photos
    let targets = vec![vis[0], vis[1], vis[2]];
    let r = h.request(
        "engine.execute",
        json!({
            "command": "library.select",
            "params": {"ids": targets.clone(), "active": targets[0]}
        }),
        T,
    );
    assert_eq!(r["ok"], true);

    // Switch right panel to Quick Develop
    h.app.ui.right = RightPanel::QuickDevelop;
    h.settle(SETTLE);

    let exp_of = |h: &Headless, id: u64| h.app.session.develop_of(lightcraft_catalog::PhotoId(id)).map(|d| d.light.exposure).unwrap_or(0.0);

    let before0 = exp_of(&h, targets[0]);
    let before1 = exp_of(&h, targets[1]);
    let before2 = exp_of(&h, targets[2]);

    // Click the +0.33 nudge button on exposure (button index 2 is +0.33)
    let r = h.request("ui.clickWidget", json!({"id": "nudge:light.exposure:2"}), T);
    assert_eq!(r["ok"], true);
    h.settle(SETTLE);

    let after0 = exp_of(&h, targets[0]);
    let after1 = exp_of(&h, targets[1]);
    let after2 = exp_of(&h, targets[2]);

    assert!((after0 - (before0 + 0.33333)).abs() < 0.05, "target 0 wasn't nudged: {before0} -> {after0}");
    assert!((after1 - (before1 + 0.33333)).abs() < 0.05, "target 1 wasn't nudged: {before1} -> {after1}");
    assert!((after2 - (before2 + 0.33333)).abs() < 0.05, "target 2 wasn't nudged: {before2} -> {after2}");
}

#[test]
fn quick_develop_auto_tone_batch() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    assert!(vis.len() >= 2);

    let targets = vec![vis[0], vis[1]];
    let r = h.request(
        "engine.execute",
        json!({
            "command": "library.select",
            "params": {"ids": targets.clone(), "active": targets[0]}
        }),
        T,
    );
    assert_eq!(r["ok"], true);

    h.app.ui.right = RightPanel::QuickDevelop;
    h.settle(SETTLE);

    // Click Auto Tone in Quick Develop
    let r = h.request("ui.clickWidget", json!({"id": "button:quickDevelopAutoTone"}), T);
    assert_eq!(r["ok"], true);
    h.settle(SETTLE);
}

#[test]
fn survey_dismiss_badge_drops_candidate() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    assert!(vis.len() >= 3);

    let candidates = vec![vis[0], vis[1], vis[2]];
    let r = h.request(
        "engine.execute",
        json!({
            "command": "library.select",
            "params": {"ids": candidates.clone(), "active": candidates[0]}
        }),
        T,
    );
    assert_eq!(r["ok"], true);

    // Enable iPad touch UI mode so dismiss badges are always visible
    crate::theme::set_touch_ui(&h.view.ctx, true);

    let r = h.request("engine.execute", json!({"command": "view.survey"}), T);
    assert_eq!(r["ok"], true);
    h.settle(SETTLE);

    assert_eq!(h.app.session.selection.ids.len(), 3);

    // Dismiss candidate 1 (vis[1]) using the dismiss badge
    let dismiss_target = candidates[1];
    let r = h.request("ui.clickWidget", json!({"id": format!("survey-x:{dismiss_target}")}), T);
    assert_eq!(r["ok"], true);
    h.settle(SETTLE);

    // The dismissed candidate should no longer be in the selection
    assert!(
        !h.app.session.selection.ids.contains(&lightcraft_catalog::PhotoId(dismiss_target)),
        "dismissed candidate should be removed from selection"
    );
    assert_eq!(h.app.session.selection.ids.len(), 2, "selection should now have 2 photos");
}

#[test]
fn quick_develop_auto_tone_edits_every_selected_photo() {
    let mut h = demo();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    assert!(vis.len() >= 3);
    let targets = vec![vis[0], vis[1], vis[2]];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": targets.clone(), "active": targets[0]}}), T);
    assert_eq!(r["ok"], true);
    let contrast = |h: &Headless, id: u64| h.app.session.develop_of(lightcraft_catalog::PhotoId(id)).map(|d| d.light.contrast).unwrap_or(0.0);
    assert!(targets.iter().all(|&id| contrast(&h, id) != 68.0));

    let r = h.request("engine.execute", json!({"command": "develop.auto", "params": {"ids": targets.clone()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(targets.iter().all(|&id| contrast(&h, id) == 68.0), "Auto must reach every selected photo, not only the active one");

    // one undo step reverts all three
    let r = h.request("engine.execute", json!({"command": "edit.undo", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(targets.iter().all(|&id| contrast(&h, id) != 68.0));
}
