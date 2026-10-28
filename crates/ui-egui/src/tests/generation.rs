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
    assert!(text.contains("generation:seed"), "{text}");
    // The connection lives in Settings ▸ ComfyUI, not in the panel.
    assert!(!text.contains("generation:endpoint"), "{text}");
    assert!(!text.contains("generation:qwenInstruction"), "{text}");
}

#[test]
fn comfyui_settings_page_edits_connection_and_resets_the_qwen_instruction() {
    use lightcraft_engine::comfyui::DEFAULT_QWEN_INSTRUCTION;
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
    // The panel's button opens Settings on the ComfyUI page.
    let r = h.request("ui.clickWidget", json!({"id": "generation:settings"}), timeout);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "comfyui"));
    let text = h.request("ui.widgets", json!({"filter": "generation:"}), timeout).to_string();
    for id in ["generation:endpoint", "generation:model", "generation:negative", "generation:qwenInstruction", "generation:resetInstruction"] {
        assert!(text.contains(id), "{id}: {text}");
    }
    // An edited instruction is kept and sent; Reset to default brings back the bundled one.
    h.app.ui.generation.model = "qwen-image-2.1-UC-BF16.gguf".into();
    h.app.ui.generation.prompt = "a red door".into();
    h.app.ui.generation.qwen_instruction = "Paint {prompt}".into();
    h.step();
    assert_eq!(h.app.ui.generation.resolved_prompt(), "Paint a red door");
    let r = h.request("ui.clickWidget", json!({"id": "generation:resetInstruction"}), timeout);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.generation.qwen_instruction, DEFAULT_QWEN_INSTRUCTION);
    assert!(h.app.session.generation.is_none(), "editing settings does not start a job");
}
