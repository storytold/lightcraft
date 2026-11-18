//! A plug-in's page: the Plug-in Manager opens it on its Settings tab and saves what the user
//! typed; a command's dialog opens with the values it last ran with; a prepare step's message is
//! shown and no dialog opens.
//!
//! Its own test binary: the Plug-in Manager's window state is the process's, and other UI tests
//! drawing frames at the same time would race it.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use lcc_ui_egui::headless::Headless;
use lcc_ui_egui::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn app() -> Headless {
    // install into memory, never into the real user's settings folder
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let services = Services { png: None, ..Default::default() };
    lcc_engine::scene_disk::use_build_dir();
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    Headless::new(app, [1200.0, 800.0], 1.0)
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// A plug-in with `manifest` that answers every call with `reply`, written to `dir/<name>.wasm`.
fn module_file(dir: &Path, name: &str, manifest: &Value, reply: &str) -> String {
    let manifest = manifest.to_string();
    let esc = |s: &str| s.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
        m = esc(&manifest),
        r = esc(reply),
        ml = manifest.len(),
        rl = reply.len()
    );
    let path = dir.join(format!("{name}.wasm"));
    std::fs::write(&path, wat::parse_str(&src).unwrap()).unwrap();
    path.display().to_string()
}

fn has_widget(h: &mut Headless, id: &str) -> bool {
    let w = h.request("ui.widgets", json!({}), T);
    w["result"].as_array().into_iter().flatten().any(|x| x["id"] == id)
}

/// Runs a UI command and draws until it has happened.
fn ui_cmd(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = exec(h, command, params);
    for _ in 0..4 {
        h.step();
    }
    r
}

/// Asks the app something over the control channel and draws until it has happened.
fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    for _ in 0..4 {
        h.step();
    }
    r
}

#[test]
fn the_plugin_page_saves_settings_and_dialogs_remember() {
    let dir = lcc_testkit::TempDir::new("plugin-settings-ui");
    std::fs::create_dir_all(&dir).unwrap();
    let settings = json!({"id": "test.uiset", "name": "Set", "version": "1.0.0",
        "settings": [{"key": "name", "label": "Name", "type": "text", "default": "first"}],
        "commands": [{"id": "go", "label": "Go", "dialog": [{"key": "who", "label": "Who", "type": "text", "default": "me"}]}]});
    let asks = json!({"id": "test.uiask", "name": "Ask", "version": "1.0.0",
        "commands": [{"id": "ask", "label": "Ask", "prepare": true, "dialog": [{"key": "v", "label": "V", "type": "choice", "options": []}]}]});
    let mut h = app();
    exec(&mut h, "plugin.install", json!({"path": module_file(&dir, "set", &settings, "{}")}));
    exec(&mut h, "plugin.install", json!({"path": module_file(&dir, "ask", &asks, r#"{"ok":{"message":"Nothing to do here."}}"#)}));

    // the page: a plug-in with settings opens on its Settings tab; one without has none
    ui_cmd(&mut h, "plugins.page", json!({"id": "test.uiset"}));
    assert!(has_widget(&mut h, "button:pluginPage-settings"), "the Settings tab");
    assert!(h.painted_text().iter().any(|t| t == "first"), "the setting's value: {:?}", h.painted_text());
    let r = h.request("engine.execute", json!({"command": "plugins.page", "params": {"id": "test.uiask", "tab": "settings"}}), T);
    assert_eq!(r["ok"], false, "no settings, no Settings tab: {r}");
    ui_cmd(&mut h, "plugins.page", json!({"id": "test.uiset"}));
    // type a new value and save it
    ask(&mut h, "ui.clickWidget", json!({"id": "pluginText:settings:test.uiset:name"}));
    ask(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
    ask(&mut h, "ui.text", json!({"text": "second"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:pluginSettingsSave"}));
    assert_eq!(exec(&mut h, "plugin.settings", json!({"id": "test.uiset"}))["values"], json!({"name": "second"}));
    assert!(has_widget(&mut h, "button:pluginSettingsSave"), "saved; the page stays");
    // the page opens again with what was saved
    ui_cmd(&mut h, "plugins.manager", json!({"open": false}));
    ui_cmd(&mut h, "plugins.page", json!({"id": "test.uiset"}));
    assert!(h.painted_text().iter().any(|t| t == "second"), "{:?}", h.painted_text());
    ui_cmd(&mut h, "plugins.manager", json!({"open": false}));

    // a dialog opens with the values it last ran with
    exec(&mut h, "plugin.run", json!({"plugin": "test.uiset", "command": "go", "args": {"who": "you"}}));
    ui_cmd(&mut h, "plugins.runItem", json!({"plugin": "test.uiset", "command": "go"}));
    assert!(has_widget(&mut h, "pluginText:test.uiset:go:who"));
    assert!(h.painted_text().iter().any(|t| t == "you"), "{:?}", h.painted_text());
    ask(&mut h, "ui.clickWidget", json!({"id": "button:pluginRunCancel"}));

    // a prepare step that explains: its message, no dialog, nothing run
    let r = ui_cmd(&mut h, "plugins.runItem", json!({"plugin": "test.uiask", "command": "ask"}));
    assert_eq!(r["message"], "Nothing to do here.", "{r}");
    assert!(!has_widget(&mut h, "button:pluginRun"), "no dialog");
    assert!(h.painted_text().iter().any(|t| t.contains("Nothing to do here.")), "shown: {:?}", h.painted_text());

    exec(&mut h, "plugin.uninstall", json!({"id": "test.uiset"}));
    exec(&mut h, "plugin.uninstall", json!({"id": "test.uiask"}));
    let _ = std::fs::remove_dir_all(&dir);
}
