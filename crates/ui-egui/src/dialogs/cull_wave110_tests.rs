//! Wave110 culling control coverage, deliberately drives the production dialog boundary.
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn draft(h: &Headless) -> (f32, bool, f32, f32) {
    match &h.app.ui.dialog {
        Some(Dialog::Cull { reject_below, pick_best, burst_gap, similarity }) => (*reject_below, *pick_best, *burst_gap, *similarity),
        dialog => panic!("unexpected dialog: {dialog:?}"),
    }
}

fn settle(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn click(h: &mut Headless, id: &str) {
    let rect = h.app.widgets.iter().find(|(name, _)| name == id).map(|(_, rect)| *rect).unwrap_or_else(|| panic!("missing {id}"));
    let r = h.request("ui.click", json!({"x": rect.center().x, "y": rect.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    settle(h);
}

fn type_value(h: &mut Headless, id: &str, text: &str) {
    let rect = h.app.widgets.iter().find(|(name, _)| name == id).map(|(_, rect)| *rect).unwrap_or_else(|| panic!("missing {id}"));
    let spacing = h.view.ctx.global_style().spacing.clone();
    let x = rect.left() + spacing.slider_width + spacing.item_spacing.x + 6.0;
    assert_eq!(h.request("ui.click", json!({"x": x, "y": rect.center().y}), T)["ok"], true);
    h.step();
    for key in ["a", "Backspace"] {
        assert_eq!(h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T)["ok"], true);
    }
    assert_eq!(h.request("ui.text", json!({"text": text}), T)["ok"], true);
    assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
    settle(h);
}

fn framework_context() -> egui::Context {
    let ctx = egui::Context::default();
    let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows));
    craft::ui::install(&ctx, spec).unwrap();
    ctx
}

#[test]
fn cull_controls_edit_the_real_draft_then_confirm_and_reopen() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.step();
    h.app.run("dialog.cull", json!({})).unwrap();
    settle(&mut h);
    assert_eq!(draft(&h), (0.0, true, 10.0, 0.93));
    click(&mut h, "check:cullPick");
    type_value(&mut h, "field:cullReject", "25");
    type_value(&mut h, "field:cullGap", "30 min");
    type_value(&mut h, "field:cullSimilar", "0.9951");
    assert_eq!(draft(&h), (25.0, false, 1800.0, 0.995));
    type_value(&mut h, "field:cullSimilar", "1.0");
    assert_eq!(draft(&h), (25.0, false, 1800.0, 1.0));
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.ui.cull_burst, Some((1800.0, 1.0)));
    h.app.run("dialog.cull", json!({})).unwrap();
    settle(&mut h);
    assert_eq!(draft(&h), (0.0, true, 1800.0, 1.0));
    click(&mut h, "check:cullPick");
    type_value(&mut h, "field:cullSimilar", "0");
    assert_eq!(draft(&h).3, 0.0);
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.ui.cull_burst, Some((1800.0, 1.0)), "Cancel must not remember draft changes");
}

#[test]
fn production_cull_body_mounts_sdk_controls_without_clamping_typed_values() {
    let ctx = framework_context();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut dialog = Dialog::Cull { reject_below: 25.0, pick_best: false, burst_gap: 1800.0, similarity: 1.0 };
    let mut roles = Vec::new();
    for _ in 0..3 {
        let mut frame_out = ctx.run_ui(egui::RawInput::default(), |root_ui| {
            let ctx = &root_ui.ctx().clone();
            egui::CentralPanel::default().show(root_ui, |ui| {
                let t = crate::theme::Tokens::get(ctx);
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("wave110-cull-probe"), |ui| {
                    crate::dialogs::cull_body(&mut app, ui, &t, &mut dialog);
                });
                roles = record
                    .iter()
                    .flat_map(|record| record.elements.iter())
                    .filter_map(|element| element.facts.as_ref().map(|facts| facts.role.clone()))
                    .collect();
            });
        });
        frame_out.textures_delta.clear();
    }
    // reject, burst gap (a framework log slider since the raw-egui port) and similarity
    assert_eq!(roles.iter().filter(|role| role.as_str() == "Slider").count(), 3, "{roles:?}");
    assert_eq!(roles.iter().filter(|role| role.as_str() == "SpinButton").count(), 2, "{roles:?}");
    assert_eq!(roles.iter().filter(|role| role.as_str() == "CheckBox").count(), 1, "{roles:?}");
    assert!(matches!(dialog, Dialog::Cull { reject_below: 25.0, pick_best: false, burst_gap: 1800.0, similarity: 1.0 }));
}

#[test]
fn cull_registration_skips_both_sizing_modes() {
    let ctx = framework_context();
    let mut frame_out = ctx.run_ui(egui::RawInput::default(), |root_ui| {
        let ctx = &root_ui.ctx().clone();
        egui::CentralPanel::default().show(root_ui, |ui| {
            crate::widgets::take_registries(ctx);
            let mut value = 1.0;
            let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
            assert!(sizing.is_sizing_pass());
            super::number(&mut sizing, "field:cullSimilar", "Similarity", &mut value, true);
            let (rects, hits) = crate::widgets::take_registries(ctx);
            assert!(rects.is_empty() && hits.is_empty(), "native sizing registered automation");
            assert_eq!(value, 1.0);
            craft::ui::runtime::measure::run(ctx, || {
                super::number(ui, "field:cullSimilar", "Similarity", &mut value, true);
            });
            let (rects, hits) = crate::widgets::take_registries(ctx);
            assert!(rects.is_empty() && hits.is_empty(), "SDK measuring registered automation");
            assert_eq!(value, 1.0);
        });
    });
    frame_out.textures_delta.clear();
}

#[test]
fn cull_historical_field_alias_owns_the_real_slider_hit() {
    let ctx = framework_context();
    for _ in 0..3 {
        let mut frame_out = ctx.run_ui(egui::RawInput::default(), |root_ui| {
            let ctx = &root_ui.ctx().clone();
            egui::CentralPanel::default().show(root_ui, |ui| {
                crate::widgets::take_registries(ctx);
                let mut value = 25.0;
                super::number(ui, "field:cullReject", "Reject below focus", &mut value, false);
                let (rects, hits) = crate::widgets::take_registries(ctx);
                if super::measuring(ui) {
                    assert!(rects.is_empty() && hits.is_empty());
                    return;
                }
                let hit = |id: &str| hits.iter().find(|(name, _)| name == id).map(|(_, hit)| *hit).unwrap();
                let alias = hit("field:cullReject");
                assert_eq!(alias, hit("slider:field:cullReject"));
                assert_ne!(alias.id, hit("value:field:cullReject").id);
                let rect = rects.iter().find(|(name, _)| name == "field:cullReject").unwrap().1;
                assert!(alias.interact_rect.contains(rect.center()), "clickWidget center must belong to its real slider");
                assert_eq!(value, 25.0);
            });
        });
        frame_out.textures_delta.clear();
    }
}
