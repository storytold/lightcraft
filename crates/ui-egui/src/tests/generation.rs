use serde_json::json;
use std::time::Duration;

use crate::{LightcraftApp, Services, headless::Headless};

#[test]
fn selected_mask_exposes_replacement_prompt_and_saves_connection_without_network() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let timeout = Duration::from_secs(20);
    let r = h.request("ui.set", json!({"view": "detail"}), timeout);
    assert_eq!(r["ok"], true, "{r}");
    for command in ["panel.masking", "mask.add"] {
        let params = if command == "mask.add" { json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2}) } else { json!({}) };
        let r = h.request("engine.execute", json!({"command": command, "params": params}), timeout);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "generation:section"}), timeout);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "generation": {"endpoint": "http://127.0.0.1:1", "prompt": "a blue vase"}}), timeout);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.widgets", json!({"filter": "generation:"}), timeout);
    let text = r.to_string();
    assert!(text.contains("generation:prompt"), "{text}");
    assert!(text.contains("generation:generate"), "{text}");
    assert!(h.app.session.generation.is_none(), "editing connection/prompt does not start a job");
    assert_eq!(h.app.ui.generation.prompt, "a blue vase");
    assert_eq!(h.app.ui.generation.endpoint, "http://127.0.0.1:1");
}
