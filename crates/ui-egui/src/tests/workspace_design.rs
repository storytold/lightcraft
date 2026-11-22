//! Navigation and progressive disclosure must keep commands reachable at small window sizes.
use crate::{
    LightcraftApp, Services,
    headless::Headless,
    state::{RightPanel, ViewMode},
};
use serde_json::{Value, json};
use std::time::Duration;
const T: Duration = Duration::from_secs(20);
fn request(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    r["result"].clone()
}
fn setup(width: f32) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [width, 1200.0], 1.0);
    request(&mut h, "ui.set", json!({"view":"detail","right":"edit","openSections":["effects"]}));
    h
}
fn has(w: &Value, id: &str) -> bool {
    w.as_array().unwrap().iter().any(|v| v["id"] == id)
}
#[test]
fn workspace_design_navigation_is_reachable_and_keeps_editing_controls() {
    for width in [850.0, 1440.0] {
        let mut h = setup(width);
        let widgets = request(&mut h, "ui.widgets", json!({}));
        for id in ["button:workspaceLibrary", "button:workspaceDevelop", "icon:import", "icon:share"] {
            assert!(has(&widgets, id), "{width}: {id}");
        }
        request(&mut h, "ui.clickWidget", json!({"id":"button:workspaceLibrary"}));
        assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
        assert!(h.app.ui.left_panel);
        request(&mut h, "ui.clickWidget", json!({"id":"button:workspaceDevelop"}));
        assert_eq!(h.app.ui.view, ViewMode::Detail);
        assert_eq!(h.app.ui.right, RightPanel::Edit);
        request(&mut h, "ui.clickWidget", json!({"id":"button:workspaceDevelop"}));
        assert_eq!(h.app.ui.right, RightPanel::Edit, "selecting Develop twice must not close editing");
    }
}
#[test]
fn workspace_design_advanced_effects_are_collapsible_without_losing_values() {
    let mut h = setup(1440.0);
    request(&mut h, "engine.execute", json!({"command":"develop.set","params":{"values":{"effects.glow.amount":65,"effects.glow.spread":82}}}));
    let widgets = request(&mut h, "ui.widgets", json!({}));
    assert!(has(&widgets, "slider:effects.glow.amount"));
    assert!(!has(&widgets, "slider:effects.glow.spread"));
    request(&mut h, "ui.clickWidget", json!({"id":"effectGroup:glow"}));
    let widgets = request(&mut h, "ui.widgets", json!({}));
    assert!(has(&widgets, "dropdown:glowStyle"));
    assert!(has(&widgets, "slider:effects.glow.spread"));
    request(&mut h, "ui.clickWidget", json!({"id":"effectGroup:glow"}));
    let id = h.app.session.active().unwrap();
    assert_eq!(h.app.session.develop_of(id).unwrap().effects.glow.spread, 82.0);
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let restored: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert!(!restored.flyout_open("effects.glow"));
}
