//! Production action regressions for the private rules component port.
use super::*;
use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

fn fixture(rows: Value) -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.dialog = Some(Dialog::SmartRules {
        id: None,
        name: "Port regression".into(),
        parent: None,
        rules: serde_json::from_value(json!({"match": "all", "rules": rows})).unwrap(),
    });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    assert!(h.settle(WAIT));
    h
}

fn draft(h: &Headless) -> &RuleSet {
    let Some(Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("rule dialog closed") };
    rules
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let response = h.request(method, params, WAIT);
    assert_eq!(response["ok"], true, "{method}: {response}");
    assert!(h.settle(WAIT));
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}
fn key(h: &mut Headless, key: &str) {
    ask(h, "ui.key", json!({"key": key}));
}

fn type_into(h: &mut Headless, id: &str, value: &str) {
    click(h, id);
    ask(h, "ui.key", json!({"key": "A", "cmd": true, "ctrl": true}));
    ask(h, "ui.text", json!({"text": value}));
    // Blur commits the SDK editor, not the enclosing dialog's primary action.
    key(h, "Tab");
}

fn framework_context() -> egui::Context {
    let ctx = egui::Context::default();
    let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows));
    craft::ui::install(&ctx, spec).unwrap();
    ctx
}

#[test]
fn rules_port155_actions_edit_numbers_and_cancel_without_catalog_writes() {
    let mut h = fixture(json!([{"field": "rating", "op": "gte", "value": 3}]));
    let revision = h.app.session.catalog.revision;
    let undo = h.app.session.undo.len();
    type_into(&mut h, "field:rules-0", "4");
    assert!(matches!(&draft(&h).rules[0], Rule::Field { value, .. } if value == &json!(4)));
    click(&mut h, "ruleMatch:rules");
    click(&mut h, "ruleMatch:rules:item:none");
    assert_eq!(draft(&h).mode, Match::None);
    click(&mut h, "button:ruleAdd-rules-0");
    assert_eq!(draft(&h).rules.len(), 2);
    click(&mut h, "button:ruleRemove-rules-1");
    assert_eq!(draft(&h).rules.len(), 1);
    assert_eq!(h.app.session.catalog.revision, revision);
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.session.catalog.revision, revision);
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn rules_port155_actions_field_submenu_operator_reset_and_nested_group() {
    let mut h = fixture(json!([{"field": "rating", "op": "gte", "value": 4}]));
    click(&mut h, "ruleField:rules-0");
    ask(&mut h, "ui.hoverWidget", json!({"id": "ruleFieldGroup:Date:rules-0"}));
    click(&mut h, "ruleFieldItem:captureDate:rules-0");
    assert!(matches!(&draft(&h).rules[0], Rule::Field { field, op, value }
        if field == "captureDate" && op == "is" && value == &default_value("captureDate", "is")));
    click(&mut h, "ruleOp:rules-0");
    click(&mut h, "ruleOp:rules-0:item:between");
    assert!(matches!(&draft(&h).rules[0], Rule::Field { op, value, .. }
        if op == "between" && value == &default_value("captureDate", "between")));
    click(&mut h, "button:ruleAddGroup-rules");
    assert!(matches!(&draft(&h).rules[1], Rule::Group { group } if group.mode == Match::Any && group.rules == vec![new_rule()]));
    type_into(&mut h, "field:rules-1-0", "5");
    assert!(matches!(&draft(&h).rules[1], Rule::Group { group }
        if matches!(&group.rules[0], Rule::Field { value, .. } if value == &json!(5))));
    click(&mut h, "button:ruleRemoveGroup-rules-1");
    assert_eq!(draft(&h).rules.len(), 1);
}

#[test]
fn rules_port155_camera_text_and_relative_date_units_change_real_json() {
    let mut h = fixture(json!([
        {"field": "shutterSpeed", "op": "gte", "value": "1/250"},
        {"field": "captureDate", "op": "inLast", "value": {"n": "7", "unit": "DAY"}}
    ]));
    type_into(&mut h, "field:rules-0", "1/500");
    assert!(matches!(&draft(&h).rules[0], Rule::Field { value, .. } if value == &json!("1/500")));
    click(&mut h, "ruleUnit:rules-1");
    click(&mut h, "ruleUnit:rules-1:item:weeks");
    assert!(matches!(&draft(&h).rules[1], Rule::Field { value, .. } if value == &json!({"n": 7.0, "unit": "weeks"})));
    type_into(&mut h, "field:rules-1-n", "2");
    assert!(matches!(&draft(&h).rules[1], Rule::Field { value, .. } if value == &json!({"n": 2.0, "unit": "weeks"})));
}

#[test]
fn rules_port155_choice_and_boolean_actions_replace_values_without_coercing_idle_input() {
    let mut h = fixture(json!([
        {"field": "flag", "op": "is", "value": "pick"},
        {"field": "virtualCopy", "op": "is", "value": "false"}
    ]));
    assert!(matches!(&draft(&h).rules[1], Rule::Field { value, .. } if value == &json!("false")));
    click(&mut h, "ruleChoice:rules-0");
    click(&mut h, "ruleChoice:rules-0:item:reject");
    assert!(matches!(&draft(&h).rules[0], Rule::Field { value, .. } if value == &json!("reject")));
    click(&mut h, "ruleBool:rules-1");
    click(&mut h, "ruleBool:rules-1:item:true");
    assert!(matches!(&draft(&h).rules[1], Rule::Field { value, .. } if value == &json!(true)));
}

#[test]
fn rules_port155_idle_and_both_measurement_modes_preserve_invalid_stored_rules() {
    let ctx = framework_context();
    let mut rules: RuleSet = serde_json::from_value(json!({"rules": [
        {"field": "rating", "op": "gte", "value": 9},
        {"field": "captureDate", "op": "inLast", "value": {"n": "0", "unit": "DAY"}},
        {"field": "rating", "op": "between", "value": "malformed pair"}
    ]}))
    .unwrap();
    let before = rules.clone();
    for _ in 0..3 {
        // Keep the same editor salts/scopes, but show each mode in its own actual pass.
        // measure::run changes measuring depth; it does not make a normal Ui a sizing Ui.
        for mode in ["egui-sizing", "sdk-measure", "idle"] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |root_ui| {
                let ctx = &root_ui.ctx().clone();
                egui::CentralPanel::default().show(root_ui, |ui| {
                    crate::widgets::take_registries(ctx);
                    match mode {
                        "egui-sizing" => {
                            let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                            edit(&mut sizing, &mut rules, "rules", 0, &[], &Env::default());
                        }
                        "sdk-measure" => craft::ui::runtime::measure::run(ctx, || edit(ui, &mut rules, "rules", 0, &[], &Env::default())),
                        _ => edit(ui, &mut rules, "rules", 0, &[], &Env::default()),
                    }
                    let (rects, hits) = crate::widgets::take_registries(ctx);
                    if mode != "idle" {
                        assert!(rects.is_empty() && hits.is_empty(), "{mode} must not publish automation owners");
                    } else {
                        assert!(!rects.is_empty() && !hits.is_empty(), "the ordinary editor must actually mount");
                    }
                    assert_eq!(rules, before, "{mode}: drawing must never silently repair a stored invalid rule");
                });
            });
            // This memory-only fixture has no renderer to apply texture uploads.
            output.textures_delta.clear();
        }
    }
}

#[test]
fn rules_port155_sdk_controls_keep_real_hit_owners_and_mount_number_role() {
    let ctx = framework_context();
    let mut rules = RuleSet { mode: Match::All, rules: vec![new_rule()] };
    let mut roles = Vec::new();
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |root_ui| {
            let ctx = &root_ui.ctx().clone();
            egui::CentralPanel::default().show(root_ui, |ui| {
                crate::widgets::take_registries(ctx);
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("rules-port155"), |ui| {
                    edit(ui, &mut rules, "rules", 0, &[], &Env::default());
                });
                roles = record.iter().flat_map(|r| r.elements.iter()).filter_map(|e| e.facts.as_ref().map(|f| f.role.clone())).collect();
                if !controls::measuring(ui) {
                    let (rects, hits) = crate::widgets::take_registries(ctx);
                    for id in ["field:rules-0", "ruleField:rules-0", "ruleOp:rules-0", "button:ruleAdd-rules-0", "button:ruleRemove-rules-0"] {
                        let rect = rects.iter().find(|(name, _)| name == id).unwrap().1;
                        let hit = hits.iter().find(|(name, _)| name == id).unwrap().1;
                        assert!(hit.interact_rect.contains(rect.center()), "{id} center is owned by the actual SDK control");
                        assert_ne!(hit.id, egui::Id::unique(("lc-automation-hit", id, 0_usize)), "{id} is not a hover-only proxy");
                    }
                }
            });
        });
        // No renderer is present; explicitly discard only this fixture's texture uploads.
        output.textures_delta.clear();
    }
    assert_eq!(roles.iter().filter(|role| role.as_str() == "SpinButton").count(), 1, "{roles:?}");
}
